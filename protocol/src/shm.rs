//! Shared memory: created and filled by the helper, mapped read-only by the app.
//!
//! The object is unlinked right after creation, so it has no name while in use and disappears
//! with its last descriptor or mapping. The receiving side never trusts the announced size: it
//! checks the object's real size before mapping.

use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};

use memmap2::{Mmap, MmapMut, MmapOptions};
use rustix::fs::Mode;
use rustix::shm;

/// A writable shared buffer and the descriptor to send along with a reply.
pub struct SharedBuffer {
    pub fd: OwnedFd,
    pub map: MmapMut,
}

impl SharedBuffer {
    pub fn create(len: usize) -> io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        if len == 0 {
            return Err(io::Error::other("empty shared buffer"));
        }
        // macOS limits shared memory names to 31 bytes.
        let name = format!(
            "/lfr{:x}.{:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let fd = shm::open(
            name.as_str(),
            shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )?;
        shm::unlink(name.as_str())?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        // SAFETY: the object was just created by this process, has no name any more and is only
        // written through this mapping; the peer maps it read-only after receiving it.
        let map = unsafe { MmapOptions::new().len(len).map_mut(&fd)? };
        Ok(Self { fd, map })
    }
}

/// Maps a received buffer read-only, after checking that it really holds `len` bytes.
pub fn map_received(fd: &OwnedFd, len: usize) -> io::Result<Mmap> {
    let size = rustix::fs::fstat(fd)?.st_size;
    if len == 0 || (size as u64) < len as u64 {
        return Err(io::Error::other("shared buffer smaller than announced"));
    }
    // SAFETY: read-only mapping of a buffer the helper finished writing before replying; the
    // helper never writes to it again (it drops its mapping after sending the reply).
    unsafe { MmapOptions::new().len(len).map(fd) }
}
