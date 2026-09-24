//! A compromised helper writes arbitrary bytes on the socket: the application's side of the
//! channel must reject them without crashing or allocating without bound.
#![no_main]

use std::io::Write;
use std::os::unix::net::UnixStream;

use libfuzzer_sys::fuzz_target;
use loft_raw_protocol::channel::Channel;
use loft_raw_protocol::Reply;

fuzz_target!(|data: &[u8]| {
    let Ok((mut helper, app)) = UnixStream::pair() else {
        return;
    };
    if helper.write_all(data).is_err() {
        return;
    }
    drop(helper);
    let mut channel = Channel::new(app);
    // Read replies until the stream is exhausted or rejected.
    for _ in 0..16 {
        if channel.recv::<Reply>().is_err() {
            break;
        }
    }
});
