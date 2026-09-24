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
pub const PROTOCOL_VERSION: u32 = 1;

/// Upper bound on a control message: a peer can never make the other allocate more.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    /// The raw file's descriptor is attached.
    Open {
        id: u64,
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

/// 8-bit RGBA pixels, rows packed (`width * 4` bytes per row).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ImageLayout {
    pub width: u32,
    pub height: u32,
}

impl ImageLayout {
    pub fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sample {
    U16,
    F32,
}

/// Samples, rows packed: `width * height * components` values of `sample`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SensorLayout {
    pub width: u32,
    pub height: u32,
    /// 1 for a colour filter mosaic, 3 for linear RGB.
    pub components: u8,
    pub sample: Sample,
}

impl SensorLayout {
    pub fn bytes(&self) -> usize {
        let size = match self.sample {
            Sample::U16 => 2,
            Sample::F32 => 4,
        };
        self.width as usize * self.height as usize * self.components as usize * size
    }
}

/// What the information panel and the library need, read from the headers only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileInfo {
    pub make: String,
    pub model: String,
    /// EXIF orientation (1-8), 0 when absent.
    pub orientation: u16,
    pub exif: Exif,
}

/// What the engine needs to develop the sensor data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SensorInfo {
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
