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

use loft_raw_protocol::channel::Channel;
use loft_raw_protocol::shm::SharedBuffer;
use loft_raw_protocol::{
    ColorMatrix, Exif, FileInfo, ImageLayout, Reply, Request, Sample, SensorInfo, SensorLayout,
    PROTOCOL_VERSION,
};
use rawler::decoders::{Decoder, RawDecodeParams};
use rawler::formats::tiff::Rational;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};

mod system;

use system::SystemFile;

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
        let (reply, buffer) = handle(request, fd, &mut files);
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
) -> (Reply, Option<SharedBuffer>) {
    let start = Instant::now();
    let micros = || start.elapsed().as_micros() as u64;
    match request {
        Request::Hello { .. } => (
            Reply::Hello {
                version: PROTOCOL_VERSION,
                decoder: "rawler 0.8.0".into(),
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
        Request::Close { id } => {
            files.remove(&id);
            (Reply::Closed { id }, None)
        }
    }
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

fn decoder(source: &RawSource) -> Result<Box<dyn Decoder>, String> {
    rawler::get_decoder(source).map_err(|e| e.to_string())
}

/// Headers only: the decoder is identified and the EXIF read, no pixel data is touched.
fn info(source: &RawSource) -> Result<FileInfo, String> {
    let decoder = decoder(source)?;
    let metadata = decoder
        .raw_metadata(source, &RawDecodeParams::default())
        .map_err(|e| e.to_string())?;
    let e = metadata.exif;
    Ok(FileInfo {
        decoder: loft_raw_protocol::Decoder::Raw,
        raw: true,
        make: metadata.make,
        model: metadata.model,
        orientation: e.orientation.unwrap_or(0),
        exif: Exif {
            iso: e.iso_speed.or(e.iso_speed_ratings.map(u32::from)),
            exposure_time: e.exposure_time.map(|r| (r.n, r.d)),
            f_number: e.fnumber.map(ratio),
            focal_length: e.focal_length.map(ratio),
            lens_model: e.lens_model,
            date_time_original: e.date_time_original,
        },
    })
}

fn ratio(value: Rational) -> f32 {
    if value.d == 0 {
        0.0
    } else {
        value.n as f32 / value.d as f32
    }
}

fn describe(image: &RawImage) -> SensorInfo {
    let area = |rect: &Option<rawler::imgop::Rect>| {
        rect.as_ref()
            .map(|r| [r.p.x as u32, r.p.y as u32, r.d.w as u32, r.d.h as u32])
    };
    let cfa = match &image.photometric {
        RawPhotometricInterpretation::Cfa(config) => config.cfa.name.clone(),
        _ => String::new(),
    };
    SensorInfo {
        origin: loft_raw_protocol::PixelOrigin::Sensor,
        scale: 1.0,
        bits_per_sample: image.bps as u8,
        cfa,
        white_balance: image.wb_coeffs,
        black_levels: image.blacklevel.levels.iter().map(|r| ratio(*r)).collect(),
        white_levels: image.whitelevel.0.iter().map(|&w| w as f32).collect(),
        color_matrices: image
            .color_matrix
            .iter()
            .map(|(illuminant, values)| ColorMatrix {
                illuminant: *illuminant as u16,
                values: values.clone(),
            })
            .collect(),
        active_area: area(&image.active_area),
        crop_area: area(&image.crop_area),
        orientation: image.orientation.to_u16(),
    }
}

fn preview(source: &RawSource) -> Result<(ImageLayout, SharedBuffer), String> {
    let decoder = decoder(source)?;
    let params = RawDecodeParams::default();
    let image = decoder
        .preview_image(source, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(source, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(source, &params).ok().flatten())
        .ok_or("no embedded preview")?;
    let rgba = image.to_rgba8();
    let layout = ImageLayout {
        width: rgba.width(),
        height: rgba.height(),
        color_space: loft_raw_protocol::PreviewSpace::Srgb,
    };
    let mut buffer = SharedBuffer::create(layout.bytes()).map_err(|e| e.to_string())?;
    buffer.map.copy_from_slice(rgba.as_raw());
    Ok((layout, buffer))
}

fn sensor(source: &RawSource) -> Result<(SensorLayout, SensorInfo, SharedBuffer), String> {
    let decoder = decoder(source)?;
    let image = decoder
        .raw_image(source, &RawDecodeParams::default(), false)
        .map_err(|e| e.to_string())?;
    let (sample, bytes): (Sample, &[u8]) = match &image.data {
        RawImageData::Integer(data) => (Sample::U16, bytemuck::cast_slice(data)),
        RawImageData::Float(data) => (Sample::F32, bytemuck::cast_slice(data)),
    };
    let layout = SensorLayout {
        width: image.width as u32,
        height: image.height as u32,
        components: image.cpp as u8,
        sample,
    };
    if layout.bytes() != bytes.len() {
        return Err("sample count does not match the image size".into());
    }
    let mut buffer = SharedBuffer::create(bytes.len()).map_err(|e| e.to_string())?;
    buffer.map.copy_from_slice(bytes);
    Ok((layout, describe(&image), buffer))
}
