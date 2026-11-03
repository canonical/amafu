#![no_main]

use amafu_guest_agent::protocol::{MAXIMUM_MESSAGE_BYTES, Reply, Request, decode, encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: &[u8]| {
    if let Ok(request) = decode::<Request>(line) {
        assert!(line.len() <= MAXIMUM_MESSAGE_BYTES, "oversized requests are refused");
        assert_eq!(decode::<Request>(&encode(&request)), Ok(request), "a request survives a round trip");
    }
    if let Ok(reply) = decode::<Reply>(line) {
        assert_eq!(decode::<Reply>(&encode(&reply)), Ok(reply), "a reply survives a round trip");
    }
});
