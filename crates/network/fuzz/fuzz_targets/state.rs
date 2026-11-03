use std::net::Ipv4Addr;

use amafu_domain::ObjectId;
use amafu_interfaces::{NodeNetworkState, Port, PortKey, Segment, SegmentKey};

pub const SERVED_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 1, 0, 2);

pub fn segment() -> SegmentKey {
    SegmentKey::of("project-a", "default")
}

pub fn state() -> NodeNetworkState {
    let subnet = "10.1.0.0/24".parse().unwrap_or_else(|_| unreachable!());
    let mac = "02:00:00:00:00:01".parse().unwrap_or_else(|_| unreachable!());
    NodeNetworkState {
        segments: vec![Segment {
            key: segment(),
            subnet,
            is_outbound_access_allowed: true,
            domain: String::from("default.project-a.internal"),
        }],
        ports: vec![Port {
            key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 },
            segment: segment(),
            mac,
            address: SERVED_ADDRESS,
            hostname: String::from("web-1"),
            rules: Vec::new(),
            user_data: Some(String::from("#cloud-config\n{}\n")),
        }],
    }
}
