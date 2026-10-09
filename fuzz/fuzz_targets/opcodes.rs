//! Untrusted DNG opcode list -> the helper's readers (linearisation, lens corrections): any
//! panic, hang or excessive allocation is a finding. The decode target rarely reaches them,
//! deep inside a valid DNG.
#![no_main]

use libfuzzer_sys::fuzz_target;
use loft_raw_helper::{corrections, opcodes};

fuzz_target!(|data: &[u8]| {
    let _ = opcodes::linearization(data, 6000, 4000);
    let _ = corrections::gain_maps(data);
    let _ = corrections::lens(data);
});
