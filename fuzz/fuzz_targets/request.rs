//! Untrusted bytes -> request decoding in the helper.
#![no_main]

use libfuzzer_sys::fuzz_target;
use loft_raw_protocol::Request;

fuzz_target!(|data: &[u8]| {
    let _ = postcard::from_bytes::<Request>(data);
});
