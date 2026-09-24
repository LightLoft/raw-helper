//! LightLoft raw decoding helper, as a library: the decoding steps and the system decoders,
//! shared by the helper process (`main.rs`) and the fuzz targets (`../fuzz`).
//!
//! Licensed LGPL-2.1-only, like rawler.

pub mod alloc;
pub mod decode;
pub mod system;
