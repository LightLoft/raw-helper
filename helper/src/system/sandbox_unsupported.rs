//! No sandbox on this platform yet: the helper still runs in its own process.

pub fn enter() -> Result<(), String> {
    Err("no sandbox on this platform".into())
}
