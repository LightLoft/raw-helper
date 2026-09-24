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

#[cfg(target_os = "macos")]
mod sandbox_macos;
#[cfg(target_os = "macos")]
pub use sandbox_macos::enter as enter_sandbox;

#[cfg(not(target_os = "macos"))]
mod sandbox_unsupported;
#[cfg(not(target_os = "macos"))]
pub use sandbox_unsupported::enter as enter_sandbox;

#[cfg(target_os = "macos")]
pub use macos::warm_up;

/// Nothing to initialise ahead of the sandbox on this platform.
#[cfg(not(target_os = "macos"))]
pub fn warm_up() {}
