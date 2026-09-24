//! The operating system's image decoders, used where rawler cannot help: non-raw formats (JPEG,
//! PNG, TIFF, HEIC...), raw files it cannot read, missing embedded previews. They read untrusted
//! files too, hence they run here, in the helper, never in the application.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::SystemFile;

#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
pub use unsupported::SystemFile;
