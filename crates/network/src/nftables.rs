use std::fmt::Write;

use amafu_domain::kinds::security_group::{Direction, Protocol};
use amafu_interfaces::network::{PortRule, RemoteAddresses, bridge_name};
use amafu_interfaces::{NodeNetworkState, Port};

use crate::metadata::METADATA_ADDRESS;

pub const TABLE: &str = "amafu";
const BRIDGE_PREFIX: &str = "amb*";

fn protocol_match(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Any => "",
        Protocol::Tcp => "meta l4proto tcp ",
        Protocol::Udp => "meta l4proto udp ",
        Protocol::Icmp => "meta l4proto icmp ",
    }
}

fn ports_match(rule: &PortRule) -> String {
    match (rule.protocol, rule.ports) {
        (Protocol::Tcp, Some(range)) => format!("tcp dport {}-{} ", range.first, range.last),
        (Protocol::Udp, Some(range)) => format!("udp dport {}-{} ", range.first, range.last),
        _ => String::new(),
    }
}

fn remote_match(remote: &RemoteAddresses, field: &str) -> Option<String> {
    match remote {
        RemoteAddresses::Any => Some(String::new()),
        RemoteAddresses::Networks { networks } if networks.is_empty() => None,
        RemoteAddresses::Networks { networks } => {
            let list: Vec<String> = networks.iter().map(ToString::to_string).collect();
            Some(format!("ip {field} {{ {} }} ", list.join(", ")))
        }
    }
}

fn rule_line(rule: &PortRule, verdict: &str) -> Option<String> {
    let remote_field = match rule.direction {
        Direction::Ingress => "saddr",
        Direction::Egress => "daddr",
    };
    let remote = remote_match(&rule.remote, remote_field)?;
    Some(format!("\t\t{}{}{}{verdict}\n", protocol_match(rule.protocol), ports_match(rule), remote))
}

fn chain_name(direction: Direction, port: &Port) -> String {
    let prefix = match direction {
        Direction::Ingress => "to",
        Direction::Egress => "from",
    };
    format!("{prefix}_{}", port.key.tap_name())
}

fn port_chains(output: &mut String, port: &Port) {
    let egress: Vec<&PortRule> =
        port.rules.iter().filter(|rule| rule.direction == Direction::Egress).collect();
    let _ = writeln!(output, "\tchain {} {{", chain_name(Direction::Egress, port));
    if !egress.is_empty() {
        for rule in egress {
            output.extend(rule_line(rule, "return"));
        }
        output.push_str("\t\tdrop\n");
    }
    output.push_str("\t}\n");
    let _ = writeln!(output, "\tchain {} {{", chain_name(Direction::Ingress, port));
    for rule in port.rules.iter().filter(|rule| rule.direction == Direction::Ingress) {
        output.extend(rule_line(rule, "accept"));
    }
    output.push_str("\t\tdrop\n\t}\n");
}

fn bridge_table(output: &mut String, state: &NodeNetworkState) {
    let _ = writeln!(output, "table bridge {TABLE} {{");
    for port in &state.ports {
        port_chains(output, port);
    }
    output.push_str("\tchain prerouting {\n\t\ttype filter hook prerouting priority -200; policy accept;\n");
    for port in &state.ports {
        let (tap, mac, address) = (port.key.tap_name(), port.mac, port.address);
        let _ = writeln!(output, "\t\tiifname \"{tap}\" ether saddr != {mac} drop");
        let _ = writeln!(output, "\t\tiifname \"{tap}\" arp saddr ip != {address} drop");
        let _ = writeln!(
            output,
            "\t\tiifname \"{tap}\" ether type ip ip saddr != {address} ip saddr != 0.0.0.0 drop"
        );
        let _ = writeln!(output, "\t\tiifname \"{tap}\" ip saddr 0.0.0.0 udp dport != 67 drop");
    }
    output.push_str("\t}\n\tchain forward {\n\t\ttype filter hook forward priority 0; policy accept;\n");
    output.push_str("\t\tct state established,related accept\n\t\tct state invalid drop\n");
    for port in &state.ports {
        let _ = writeln!(
            output,
            "\t\tiifname \"{}\" jump {}",
            port.key.tap_name(),
            chain_name(Direction::Egress, port)
        );
    }
    for port in &state.ports {
        let _ = writeln!(
            output,
            "\t\toifname \"{}\" jump {}",
            port.key.tap_name(),
            chain_name(Direction::Ingress, port)
        );
    }
    output.push_str("\t}\n}\n");
}

fn inet_table(output: &mut String, state: &NodeNetworkState, underlay: &str) {
    let _ = writeln!(output, "table inet {TABLE} {{");
    for port in &state.ports {
        port_chains(output, port);
    }
    output.push_str("\tchain input {\n\t\ttype filter hook input priority 0; policy accept;\n");
    let _ = writeln!(output, "\t\tiifname \"{BRIDGE_PREFIX}\" ct state established,related accept");
    let _ = writeln!(output, "\t\tiifname \"{BRIDGE_PREFIX}\" udp dport 67 accept");
    for segment in &state.segments {
        let (bridge, gateway) = (bridge_name(&segment.key), segment.gateway());
        let _ = writeln!(
            output,
            "\t\tiifname \"{bridge}\" ip daddr {gateway} meta l4proto {{ tcp, udp }} th dport 53 accept"
        );
        let _ = writeln!(output, "\t\tiifname \"{bridge}\" ip daddr {gateway} meta l4proto icmp accept");
        let _ = writeln!(output, "\t\tiifname \"{bridge}\" ip daddr {METADATA_ADDRESS} tcp dport 80 accept");
    }
    let _ = writeln!(output, "\t\tiifname \"{BRIDGE_PREFIX}\" drop\n\t}}");
    output.push_str("\tchain forward {\n\t\ttype filter hook forward priority 0; policy accept;\n");
    let _ = writeln!(output, "\t\toifname \"{BRIDGE_PREFIX}\" ct state established,related accept");
    let _ = writeln!(output, "\t\toifname \"{BRIDGE_PREFIX}\" drop");
    for port in &state.ports {
        let _ =
            writeln!(output, "\t\tip saddr {} jump {}", port.address, chain_name(Direction::Egress, port));
    }
    for segment in &state.segments {
        let bridge = bridge_name(&segment.key);
        let verdict = if segment.is_outbound_access_allowed { "accept" } else { "drop" };
        let _ = writeln!(output, "\t\tiifname \"{bridge}\" oifname \"{underlay}\" {verdict}");
    }
    let _ = writeln!(output, "\t\tiifname \"{BRIDGE_PREFIX}\" drop\n\t}}");
    output.push_str("\tchain postrouting {\n\t\ttype nat hook postrouting priority 100; policy accept;\n");
    for segment in state.segments.iter().filter(|segment| segment.is_outbound_access_allowed) {
        let _ = writeln!(output, "\t\tip saddr {} oifname \"{underlay}\" masquerade", segment.subnet);
    }
    output.push_str("\t}\n}\n");
}

#[must_use]
pub fn compile_ruleset(state: &NodeNetworkState, underlay: &str) -> String {
    let mut output = format!(
        "table bridge {TABLE}\ndelete table bridge {TABLE}\ntable inet {TABLE}\ndelete table inet {TABLE}\n"
    );
    bridge_table(&mut output, state);
    inet_table(&mut output, state, underlay);
    output
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::security_group::PortRange;
    use amafu_domain::{Ipv4Network, ObjectId};
    use amafu_interfaces::PortKey;

    use super::*;
    use crate::dhcp::tests::state;

    fn with_rules(rules: Vec<PortRule>) -> NodeNetworkState {
        let mut state = state();
        state.ports[0].rules = rules;
        state
    }

    #[test]
    fn the_ruleset_replaces_both_tables_atomically() {
        let ruleset = compile_ruleset(&state(), "eth0");
        assert!(ruleset.starts_with(
            "table bridge amafu\ndelete table bridge amafu\ntable inet amafu\ndelete table inet amafu\n"
        ));
        assert_eq!(ruleset.matches('{').count(), ruleset.matches('}').count());
    }

    #[test]
    fn every_port_is_bound_to_its_mac_and_address() {
        let ruleset = compile_ruleset(&state(), "eth0");
        let tap = PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 }.tap_name();
        assert!(ruleset.contains(&format!("iifname \"{tap}\" ether saddr != 02:00:00:00:00:01 drop")));
        assert!(ruleset.contains(&format!("iifname \"{tap}\" arp saddr ip != 10.1.0.2 drop")));
    }

    #[test]
    fn ingress_rules_become_accepts_followed_by_a_drop() {
        let rules = vec![PortRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            ports: Some(PortRange { first: 80, last: 80 }),
            remote: RemoteAddresses::Networks {
                networks: vec![Ipv4Network::host("10.1.0.3".parse().unwrap())],
            },
        }];
        let ruleset = compile_ruleset(&with_rules(rules), "eth0");
        assert!(
            ruleset.contains("meta l4proto tcp tcp dport 80-80 ip saddr { 10.1.0.3/32 } accept\n\t\tdrop")
        );
    }

    #[test]
    fn a_group_remote_without_members_allows_nothing() {
        let rules = vec![PortRule {
            direction: Direction::Ingress,
            protocol: Protocol::Any,
            ports: None,
            remote: RemoteAddresses::Networks { networks: Vec::new() },
        }];
        assert!(
            !compile_ruleset(&with_rules(rules), "eth0").contains(" accept\n\t\tdrop\n\t}\n\tchain from")
        );
    }

    #[test]
    fn guests_reach_only_the_node_services_they_need() {
        let ruleset = compile_ruleset(&state(), "eth0");
        assert!(ruleset.contains("udp dport 67 accept"));
        assert!(ruleset.contains("ip daddr 10.1.0.1 meta l4proto { tcp, udp } th dport 53 accept"));
        assert!(ruleset.contains("ip daddr 169.254.169.254 tcp dport 80 accept"));
        assert!(ruleset.contains("iifname \"amb*\" drop\n\t}\n\tchain forward"));
        assert!(ruleset.contains("ip saddr 10.1.0.0/24 oifname \"eth0\" masquerade"));
    }
}

#[cfg(test)]
mod nft_tests {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    use amafu_domain::Ipv4Network;
    use amafu_domain::kinds::security_group::PortRange;

    use super::*;
    use crate::dhcp::tests::state;

    #[test]
    #[ignore = "needs the nft binary and CAP_NET_ADMIN"]
    fn the_ruleset_is_accepted_by_nft() {
        let mut state = state();
        state.ports[0].rules = vec![
            PortRule {
                direction: Direction::Ingress,
                protocol: Protocol::Tcp,
                ports: Some(PortRange { first: 80, last: 81 }),
                remote: RemoteAddresses::Networks {
                    networks: vec![Ipv4Network::host("10.1.0.3".parse().unwrap())],
                },
            },
            PortRule {
                direction: Direction::Egress,
                protocol: Protocol::Udp,
                ports: Some(PortRange { first: 53, last: 53 }),
                remote: RemoteAddresses::Any,
            },
            PortRule {
                direction: Direction::Ingress,
                protocol: Protocol::Icmp,
                ports: None,
                remote: RemoteAddresses::Any,
            },
        ];
        let mut check = Command::new("nft")
            .args(["--check", "--file", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        check.stdin.take().unwrap().write_all(compile_ruleset(&state, "eth0").as_bytes()).unwrap();
        let output = check.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}
