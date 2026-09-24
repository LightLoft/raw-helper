//! Platforms without system decoders wired yet: every call reports that the format is unsupported.

use loft_raw_protocol::shm::SharedBuffer;
use loft_raw_protocol::{FileInfo, ImageLayout, SensorInfo, SensorLayout};

pub enum SystemFile {}

impl SystemFile {
    pub fn open(_bytes: &[u8], _extension: Option<&str>) -> Result<(Self, FileInfo), String> {
        Err("format not supported on this platform".into())
    }

    pub fn preview(&self) -> Result<(ImageLayout, SharedBuffer), String> {
        match *self {}
    }

    pub fn develop(&self) -> Result<(SensorLayout, SensorInfo, SharedBuffer), String> {
        match *self {}
    }
}
