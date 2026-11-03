#![no_main]

mod state;

use amafu_network::dns::{Decision, Query, answer};
use libfuzzer_sys::fuzz_target;

const MAXIMUM_ANSWER_BYTES: usize = 512;

fuzz_target!(|bytes: &[u8]| {
    let Ok(query) = Query::parse(bytes) else {
        return;
    };
    if let Decision::Answer(response) = answer(&query, &state::segment(), &state::state()) {
        assert!(response.len() <= MAXIMUM_ANSWER_BYTES, "an answer fits in one UDP datagram");
        assert_eq!(response.get(..2), Some(&query.identifier.to_be_bytes()[..]), "an answer keeps the identifier");
    }
});
