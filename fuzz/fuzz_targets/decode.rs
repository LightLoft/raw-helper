//! Untrusted file -> rawler, through the helper's own decoding steps, without the panic guard
//! of the process loop: any panic, abort, hang or excessive allocation is a finding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use loft_raw_helper::decode::{info, preview, sensor};
use rawler::rawsource::RawSource;

fuzz_target!(|data: &[u8]| {
    let source = RawSource::new_from_slice(data);
    if info(&source).is_ok() {
        let _ = preview(&source);
        let _ = sensor(&source);
    }
});
