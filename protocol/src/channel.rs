//! Framed messages over a Unix stream socket, with file descriptors attached (`SCM_RIGHTS`).
//!
//! A frame is a little-endian `u32` length followed by a postcard payload. A descriptor, if any,
//! travels with the first byte of its frame.

use std::io::{self, IoSlice, IoSliceMut, Read};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use rustix::net::{
    recvmsg, sendmsg, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::MAX_MESSAGE_BYTES;

pub struct Channel {
    stream: UnixStream,
}

impl Channel {
    pub fn new(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Sends one message, with an optional descriptor.
    pub fn send<T: Serialize>(&mut self, message: &T, fd: Option<&OwnedFd>) -> io::Result<()> {
        let payload = postcard::to_stdvec(message).map_err(io::Error::other)?;
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::other("message too large"));
        }
        let header = (payload.len() as u32).to_le_bytes();
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&payload);

        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        let borrowed = fd.map(|fd| [std::os::fd::AsFd::as_fd(fd)]);
        if let Some(fds) = &borrowed {
            control.push(SendAncillaryMessage::ScmRights(fds));
        }
        let mut sent = sendmsg(
            &self.stream,
            &[IoSlice::new(&frame)],
            &mut control,
            SendFlags::empty(),
        )?;
        // The descriptor went with the first byte; the rest of the frame is plain data.
        while sent < frame.len() {
            sent += rustix::io::write(&self.stream, &frame[sent..])?;
        }
        Ok(())
    }

    /// Receives one message and the descriptor that came with it, if any.
    pub fn recv<T: DeserializeOwned>(&mut self) -> io::Result<(T, Option<OwnedFd>)> {
        let mut header = [0u8; 4];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = recvmsg(
            &self.stream,
            &mut [IoSliceMut::new(&mut header)],
            &mut control,
            RecvFlags::empty(),
        )?;
        if received.bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed the channel",
            ));
        }
        let mut fd = None;
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                // Keep the first descriptor; any extra one is closed when dropped.
                for received_fd in fds {
                    if fd.is_none() {
                        fd = Some(received_fd);
                    }
                }
            }
        }
        self.stream.read_exact(&mut header[received.bytes..])?;
        let length = u32::from_le_bytes(header) as usize;
        if length > MAX_MESSAGE_BYTES {
            return Err(io::Error::other("message too large"));
        }
        let mut payload = vec![0u8; length];
        self.stream.read_exact(&mut payload)?;
        let message = postcard::from_bytes(&payload).map_err(io::Error::other)?;
        Ok((message, fd))
    }
}
