//! LightLoft raw decoding helper: decodes untrusted raw photo files with rawler, in a process of
//! its own, so that a malformed file can at worst crash this helper, never the application.
//!
//! Its standard input is a Unix socket to the application (see `loft-raw-protocol`). It never
//! opens a path: files arrive as descriptors, and pixels leave in shared memory. What rawler
//! cannot read (non-raw formats, some raw files, missing previews) falls back to the system's
//! decoders (`system`), in this same process.
//!
//! Licensed LGPL-2.1-only, like rawler, which it links statically: its full source is published
//! so that it can be rebuilt against a modified rawler.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use loft_raw_helper::alloc::Capped;
use loft_raw_helper::decode::{info, preview, sensor};
use loft_raw_helper::system::{self, SystemFile};
use loft_raw_protocol::channel::Channel;
use loft_raw_protocol::shm::SharedBuffer;
use loft_raw_protocol::{Reply, Request, PROTOCOL_VERSION};
use rawler::rawsource::RawSource;

/// Hostile files must not be able to exhaust the machine's memory (see `alloc`).
#[global_allocator]
static ALLOCATOR: Capped = Capped;

/// Files kept open between requests; the app closes them, this only bounds a misbehaving peer.
const MAX_OPEN_FILES: usize = 8;

struct OpenFile {
    source: RawSource,
    /// Keeps the received descriptor alive while `source` maps it.
    _fd: OwnedFd,
    /// Whether rawler recognised the file.
    rawler: bool,
    /// System decoder, created on first need (always, when rawler did not recognise the file).
    system: Option<Result<SystemFile, String>>,
    extension: Option<String>,
}

impl OpenFile {
    fn system(&mut self) -> Result<&SystemFile, String> {
        let (source, extension) = (&self.source, self.extension.as_deref());
        let system = self.system.get_or_insert_with(|| {
            guarded(|| SystemFile::open(source.buf(), extension).map(|(file, _)| file))
        });
        system.as_ref().map_err(Clone::clone)
    }
}

fn main() -> ExitCode {
    let stream = match std::io::stdin().as_fd().try_clone_to_owned() {
        Ok(fd) => UnixStream::from(fd),
        Err(_) => return ExitCode::FAILURE,
    };
    // Load what the decoders need, then close the sandbox before reading any request. On macOS
    // the helper refuses to run unconfined.
    system::warm_up();
    let sandboxed = match system::enter_sandbox() {
        Ok(()) => true,
        Err(error) if cfg!(target_os = "macos") => {
            eprintln!("loft-raw-helper: cannot enter the sandbox: {error}");
            return ExitCode::FAILURE;
        }
        Err(_) => false,
    };
    let mut channel = Channel::new(stream);
    let mut files: HashMap<u64, OpenFile> = HashMap::new();
    loop {
        let (request, fd) = match channel.recv::<Request>() {
            Ok(received) => received,
            // The application closed the channel: normal shutdown.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return ExitCode::SUCCESS
            }
            Err(_) => return ExitCode::FAILURE,
        };
        let (reply, buffer) = handle(request, fd, &mut files, sandboxed);
        if channel
            .send(&reply, buffer.as_ref().map(|b| &b.fd))
            .is_err()
        {
            return ExitCode::FAILURE;
        }
    }
}

fn handle(
    request: Request,
    fd: Option<OwnedFd>,
    files: &mut HashMap<u64, OpenFile>,
    sandboxed: bool,
) -> (Reply, Option<SharedBuffer>) {
    let start = Instant::now();
    let micros = || start.elapsed().as_micros() as u64;
    match request {
        Request::Hello { .. } => (
            Reply::Hello {
                version: PROTOCOL_VERSION,
                decoder: "rawler 0.8.0".into(),
                sandboxed,
            },
            None,
        ),
        Request::Open { id, extension } => {
            let Some(fd) = fd else {
                return failed(id, "no file descriptor attached");
            };
            if files.len() >= MAX_OPEN_FILES {
                return failed(id, "too many open files");
            }
            // The descriptor is mapped through /dev/fd: no path of the user's disk is ever used.
            let path = PathBuf::from(format!("/dev/fd/{}", fd.as_raw_fd()));
            let source = match RawSource::new(&path) {
                Ok(source) => source,
                Err(error) => return failed(id, &format!("cannot map file: {error}")),
            };
            let (info, rawler, system) = match guarded(|| info(&source)) {
                Ok(info) => (info, true, None),
                Err(raw_error) => {
                    match guarded(|| SystemFile::open(source.buf(), extension.as_deref())) {
                        Ok((file, info)) => (info, false, Some(Ok(file))),
                        Err(system_error) => {
                            return failed(id, &format!("{raw_error}; system: {system_error}"))
                        }
                    }
                }
            };
            files.insert(
                id,
                OpenFile {
                    source,
                    _fd: fd,
                    rawler,
                    system,
                    extension,
                },
            );
            (
                Reply::Opened {
                    id,
                    info: Box::new(info),
                    micros: micros(),
                },
                None,
            )
        }
        Request::Preview { id } => {
            let Some(file) = files.get_mut(&id) else {
                return failed(id, "file not open");
            };
            let from_rawler = if file.rawler {
                guarded(|| preview(&file.source))
            } else {
                Err(String::new())
            };
            let result = from_rawler.or_else(|raw_error| {
                let system = file.system()?;
                guarded(|| system.preview()).map_err(|e| join(&raw_error, &e))
            });
            match result {
                Ok((image, buffer)) => (
                    Reply::Preview {
                        id,
                        image,
                        micros: micros(),
                    },
                    Some(buffer),
                ),
                Err(error) => failed(id, &error),
            }
        }
        Request::Sensor { id } => {
            let Some(file) = files.get_mut(&id) else {
                return failed(id, "file not open");
            };
            let from_rawler = if file.rawler {
                guarded(|| sensor(&file.source))
            } else {
                Err(String::new())
            };
            let result = from_rawler.or_else(|raw_error| {
                let system = file.system()?;
                guarded(|| system.develop()).map_err(|e| join(&raw_error, &e))
            });
            match result {
                Ok((sensor, info, buffer)) => (
                    Reply::Sensor {
                        id,
                        sensor,
                        info: Box::new(info),
                        micros: micros(),
                    },
                    Some(buffer),
                ),
                Err(error) => failed(id, &error),
            }
        }
        Request::SandboxCheck => (
            Reply::SandboxCheck {
                sandboxed,
                escapes: sandbox_escapes(),
            },
            None,
        ),
        Request::Close { id } => {
            files.remove(&id);
            (Reply::Closed { id }, None)
        }
    }
}

/// Tries what the sandbox must forbid; returns the operations that nevertheless succeeded.
fn sandbox_escapes() -> Vec<String> {
    let mut escapes = Vec::new();
    if std::fs::read_dir("/Users").is_ok() {
        escapes.push("list /Users".to_owned());
    }
    if std::fs::read("/etc/passwd").is_ok() {
        escapes.push("read /etc/passwd".to_owned());
    }
    let probe = std::env::temp_dir().join("loft-raw-helper-sandbox-check");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        escapes.push("write a file".to_owned());
    }
    if std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| socket.send_to(b"x", "192.0.2.1:9"))
        .is_ok()
    {
        escapes.push("send a network packet".to_owned());
    }
    escapes
}

/// Both decoders' reasons, when both failed.
fn join(raw_error: &str, system_error: &str) -> String {
    if raw_error.is_empty() {
        system_error.to_owned()
    } else {
        format!("{raw_error}; system: {system_error}")
    }
}

fn failed(id: u64, error: &str) -> (Reply, Option<SharedBuffer>) {
    (
        Reply::Failed {
            id,
            error: error.to_owned(),
        },
        None,
    )
}

/// Runs a decoding step, turning a panic inside the decoder into an error: the helper stays up.
fn guarded<T>(step: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(step)).unwrap_or_else(|_| Err("decoder panicked".into()))
}
