use std::net::Ipv4Addr;

use amafu_domain::kinds::security_group::{Direction, Protocol};
use amafu_interfaces::network::PortRule;
use amafu_interfaces::{NodeNetworkState, Port, SegmentKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub segment: SegmentKey,
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub protocol: Protocol,
    pub destination_port: Option<u16>,
}

fn matches(rule: &PortRule, flow: &Flow, remote: Ipv4Addr) -> bool {
    let protocol_matches = rule.protocol == Protocol::Any || rule.protocol == flow.protocol;
    let ports_match = match (rule.ports, flow.destination_port) {
        (None, _) => true,
        (Some(range), Some(port)) => range.contains(port),
        (Some(_), None) => false,
    };
    protocol_matches && ports_match && rule.remote.contains(remote)
}

fn rules_in(port: &Port, direction: Direction) -> impl Iterator<Item = &PortRule> {
    port.rules.iter().filter(move |rule| rule.direction == direction)
}

#[must_use]
pub fn permits_egress(port: &Port, flow: &Flow) -> bool {
    let mut egress = rules_in(port, Direction::Egress).peekable();
    egress.peek().is_none() || egress.any(|rule| matches(rule, flow, flow.destination))
}

#[must_use]
pub fn permits_ingress(port: &Port, flow: &Flow) -> bool {
    rules_in(port, Direction::Ingress).any(|rule| matches(rule, flow, flow.source))
}

#[must_use]
pub fn permits(state: &NodeNetworkState, flow: &Flow) -> bool {
    let sender = state.port_with_address(&flow.segment, flow.source);
    let receiver = state.port_with_address(&flow.segment, flow.destination);
    sender.is_none_or(|port| permits_egress(port, flow))
        && receiver.is_none_or(|port| permits_ingress(port, flow))
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::security_group::PortRange;
    use amafu_domain::{Ipv4Network, MacAddress, ObjectId};
    use amafu_interfaces::network::RemoteAddresses;
    use amafu_interfaces::{PortKey, Segment};

    use super::*;

    fn segment() -> SegmentKey {
        SegmentKey::of("project-a", "default")
    }

    fn port(octet: u8, rules: Vec<PortRule>) -> Port {
        Port {
            key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::from_u128(u128::from(octet))), index: 0 },
            segment: segment(),
            mac: MacAddress::locally_administered(&[octet]),
            address: Ipv4Addr::new(10, 1, 0, octet),
            hostname: format!("vm-{octet}"),
            rules,
            user_data: None,
        }
    }

    fn rule(
        direction: Direction,
        protocol: Protocol,
        ports: Option<(u16, u16)>,
        remote: RemoteAddresses,
    ) -> PortRule {
        PortRule { direction, protocol, ports: ports.map(|(first, last)| PortRange { first, last }), remote }
    }

    fn state(ports: Vec<Port>) -> NodeNetworkState {
        NodeNetworkState {
            segments: vec![Segment {
                key: segment(),
                subnet: "10.1.0.0/24".parse().unwrap(),
                is_outbound_access_allowed: true,
                domain: String::from("default.project-a.internal"),
            }],
            ports,
        }
    }

    fn tcp(from: u8, to: u8, port: u16) -> Flow {
        Flow {
            segment: segment(),
            source: Ipv4Addr::new(10, 1, 0, from),
            destination: Ipv4Addr::new(10, 1, 0, to),
            protocol: Protocol::Tcp,
            destination_port: Some(port),
        }
    }

    #[test]
    fn ingress_is_denied_unless_a_rule_allows_it() {
        let web =
            port(2, vec![rule(Direction::Ingress, Protocol::Tcp, Some((80, 80)), RemoteAddresses::Any)]);
        let closed = port(3, Vec::new());
        let client = port(4, Vec::new());
        let network = state(vec![web, closed, client]);
        assert!(permits(&network, &tcp(4, 2, 80)));
        assert!(!permits(&network, &tcp(4, 2, 22)));
        assert!(!permits(&network, &tcp(4, 3, 80)));
    }

    #[test]
    fn remotes_restrict_who_may_connect() {
        let only_from_four =
            RemoteAddresses::Networks { networks: vec![Ipv4Network::host(Ipv4Addr::new(10, 1, 0, 4))] };
        let database =
            port(2, vec![rule(Direction::Ingress, Protocol::Tcp, Some((5432, 5432)), only_from_four)]);
        let network = state(vec![database, port(4, Vec::new()), port(5, Vec::new())]);
        assert!(permits(&network, &tcp(4, 2, 5432)));
        assert!(!permits(&network, &tcp(5, 2, 5432)));
    }

    #[test]
    fn egress_is_open_until_a_group_restricts_it() {
        let open = port(2, Vec::new());
        let restricted =
            port(3, vec![rule(Direction::Egress, Protocol::Udp, Some((53, 53)), RemoteAddresses::Any)]);
        let server = port(9, vec![rule(Direction::Ingress, Protocol::Any, None, RemoteAddresses::Any)]);
        let network = state(vec![open, restricted, server]);
        assert!(permits(&network, &tcp(2, 9, 443)));
        assert!(!permits(&network, &tcp(3, 9, 443)));
    }
}
