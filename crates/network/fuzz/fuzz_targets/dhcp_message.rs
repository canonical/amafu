#![no_main]

mod state;

use amafu_network::dhcp::{DhcpMessage, answer, is_domain_name};
use libfuzzer_sys::fuzz_target;

fn header(message: &DhcpMessage) -> impl PartialEq + std::fmt::Debug {
    (
        message.is_reply,
        message.transaction,
        message.flags,
        message.client_address,
        message.your_address,
        message.relay_address,
        message.hardware_address,
        message.message_type,
    )
}

fuzz_target!(|bytes: &[u8]| {
    let Ok(message) = DhcpMessage::parse(bytes) else {
        return;
    };
    if let Some(name) = &message.lease.domain_name {
        assert!(is_domain_name(name), "an accepted domain name holds only hostname characters");
    }
    let reparsed = DhcpMessage::parse(&message.encode());
    assert_eq!(reparsed.as_ref().map(header), Ok(header(&message)), "the header survives encoding");
    if let Some(reply) = answer(&message, &state::segment(), &state::state()) {
        assert!(reply.is_reply, "an answer is a reply");
        assert_eq!(reply.transaction, message.transaction, "an answer keeps the transaction");
        let parsed = DhcpMessage::parse(&reply.encode());
        assert!(parsed.is_ok(), "an answer must parse");
        if !reply.your_address.is_unspecified() {
            assert_eq!(reply.your_address, state::SERVED_ADDRESS, "only the port's address is offered");
            assert!(parsed.is_ok_and(|parsed| parsed.lease.domain_name.is_some()), "a lease carries the domain");
        }
    }
});
