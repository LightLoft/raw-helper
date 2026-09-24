//! Messages between LightLoft and its raw decoding helper process.
//!
//! The helper decodes untrusted files in a separate, sandboxed process. It never opens a path:
//! the app opens the file and passes its descriptor. Pixels come back in shared memory created by
//! the helper, whose descriptor rides along with the reply, so large buffers are never copied
//! through the socket. Control messages are small and encoded with postcard.
//!
//! Licensed MIT OR Apache-2.0 so that closed-source applications can use it; the helper itself
//! is a separate program.

pub mod channel;
pub mod shm;

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change; both sides check it with `Hello`.
pub const PROTOCOL_VERSION: u32 = 2;

/// Upper bound on a control message: a peer can never make the other allocate more.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    /// The file's descriptor is attached. `extension` (never a path) helps the system decoders
    /// identify formats they cannot recognise from the bytes alone.
    Open {
        id: u64,
        extension: Option<String>,
    },
    /// Largest embedded preview, decoded to RGBA8.
    Preview {
        id: u64,
    },
    /// Sensor data (undemosaiced mosaic, or linear RGB for demosaiced DNGs).
    Sensor {
        id: u64,
    },
    Close {
        id: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Hello {
        version: u32,
        decoder: String,
    },
    /// Headers only: answered without decoding any pixel.
    Opened {
        id: u64,
        info: Box<FileInfo>,
        micros: u64,
    },
    /// Shared memory holding the pixels is attached.
    Preview {
        id: u64,
        image: ImageLayout,
        micros: u64,
    },
    /// Shared memory holding the samples is attached.
    Sensor {
        id: u64,
        sensor: SensorLayout,
        info: Box<SensorInfo>,
        micros: u64,
    },
    Closed {
        id: u64,
    },
    Failed {
        id: u64,
        error: String,
    },
}

/// 8-bit RGBA pixels, rows packed (`width * 4` bytes per row), in `color_space`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ImageLayout {
    pub width: u32,
    pub height: u32,
    pub color_space: PreviewSpace,
}

/// Colour space of preview pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewSpace {
    /// Embedded camera JPEG, taken as sRGB (its own profile is not applied).
    Srgb,
    /// Colour-managed by the system into Display P3.
    DisplayP3,
}

impl ImageLayout {
    pub fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sample {
    U16,
    /// IEEE half float.
    F16,
    F32,
}

/// Samples, rows packed: `width * height * components` values of `sample`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SensorLayout {
    pub width: u32,
    pub height: u32,
    /// 1 for a colour filter mosaic, 3 for linear RGB, 4 for system-developed RGBA.
    pub components: u8,
    pub sample: Sample,
}

impl SensorLayout {
    pub fn bytes(&self) -> usize {
        let size = match self.sample {
            Sample::U16 | Sample::F16 => 2,
            Sample::F32 => 4,
        };
        self.width as usize * self.height as usize * self.components as usize * size
    }
}

/// Which decoder read the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decoder {
    /// The raw decoder library: sensor samples as recorded.
    #[default]
    Raw,
    /// The operating system's image decoders (non-raw formats, or raw files the raw decoder
    /// cannot read).
    System,
}

/// What the information panel and the library need, read from the headers only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileInfo {
    pub decoder: Decoder,
    /// Whether the file holds raw sensor data (as opposed to an already developed image).
    pub raw: bool,
    pub make: String,
    pub model: String,
    /// EXIF orientation (1-8), 0 when absent.
    pub orientation: u16,
    pub exif: Exif,
}

/// Where the samples stand in the development pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelOrigin {
    /// As the sensor recorded them (mosaic, or linear raw): the engine develops them.
    #[default]
    Sensor,
    /// Already demosaiced and colour-converted by the system, without tone curve or look:
    /// scene-linear RGBA, extended range, ITU-R BT.2020 primaries, D65 white. The engine injects
    /// them after its own demosaicing stage.
    SystemLinearBt2020,
}

/// What the engine needs to develop the sensor data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SensorInfo {
    pub origin: PixelOrigin,
    /// Resolution of the samples relative to the file: 1.0 at full size, lower when a very large
    /// image was developed by the system at reduced size to bound memory.
    pub scale: f32,
    pub bits_per_sample: u8,
    /// Colour filter pattern, e.g. `RGGB` or a 6x6 X-Trans pattern; empty for linear RGB.
    pub cfa: String,
    /// As-shot white balance multipliers, RGBE order (NaN when absent).
    pub white_balance: [f32; 4],
    pub black_levels: Vec<f32>,
    pub white_levels: Vec<f32>,
    /// XYZ -> camera matrices by illuminant (EXIF light source code), row-major.
    pub color_matrices: Vec<ColorMatrix>,
    /// Usable area and recommended crop, as `[x, y, width, height]`.
    pub active_area: Option<[u32; 4]>,
    pub crop_area: Option<[u32; 4]>,
    /// EXIF orientation (1-8) as the decoder reads it.
    pub orientation: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColorMatrix {
    pub illuminant: u16,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Exif {
    pub iso: Option<u32>,
    /// Exposure time as a fraction of a second.
    pub exposure_time: Option<(u32, u32)>,
    pub f_number: Option<f32>,
    pub focal_length: Option<f32>,
    pub lens_model: Option<String>,
    pub date_time_original: Option<String>,
}
