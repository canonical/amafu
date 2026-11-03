#![no_main]

mod state;

use std::net::Ipv4Addr;

use amafu_network::metadata::answer;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let Some((address, path)) = bytes.split_first_chunk::<4>() else {
        return;
    };
    let source = Ipv4Addr::from(*address);
    let path = String::from_utf8_lossy(path);
    let document = answer(&state::state(), source, &path);
    if document.is_some() {
        assert_eq!(source, state::SERVED_ADDRESS, "only a known port is served");
    }
});
