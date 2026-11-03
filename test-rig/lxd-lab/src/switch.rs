use std::collections::BTreeSet;
use std::fmt::Write;
use std::net::Ipv4Addr;
use std::time::Duration;

use amafu_domain::MacAddress;
use anyhow::bail;
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::lxd::LxdClient;
use crate::machines::wait_for_marker;
use crate::topology::{LabMachine, LabSettings, LabTopology};

pub const SWITCH_INSTANCE: &str = "lab-switch";
const PORT_NETWORK_PREFIX: &str = "amfl-";
const READY_MARKER: &str = "/run/amafu-switch-ready";
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const FAULT_TABLE: &str = "amafu_lab_faults";

fn port_mac(machine: &LabMachine) -> MacAddress {
    MacAddress::new([0x00, 0x16, 0x3e, 0xa2, 0x00, machine.host])
}

pub async fn create_port_networks(lxd: &LxdClient, topology: &LabTopology) -> anyhow::Result<()> {
    let default = lxd.in_project("default");
    for machine in topology.attached_machines() {
        if default.exists(&format!("/1.0/networks/{}", machine.port_network)).await? {
            continue;
        }
        let body = json!({
            "name": machine.port_network,
            "type": "bridge",
            "description": "Amafu virtual lab switch port",
            "config": {"ipv4.address": "none", "ipv6.address": "none"},
        });
        default.request(Method::POST, "/1.0/networks", Some(body)).await?;
    }
    Ok(())
}

pub async fn delete_port_networks(lxd: &LxdClient) -> anyhow::Result<()> {
    let default = lxd.in_project("default");
    let networks = default.request(Method::GET, "/1.0/networks", None).await?;
    for url in networks.as_array().into_iter().flatten().filter_map(Value::as_str) {
        let name = url.rsplit('/').next().unwrap_or_default();
        if name.starts_with(PORT_NETWORK_PREFIX) {
            default.request(Method::DELETE, &format!("/1.0/networks/{name}"), None).await?;
        }
    }
    Ok(())
}

fn network_configuration(settings: &LabSettings, topology: &LabTopology) -> String {
    let switch = &topology.switch;
    let mut configuration = format!(
        "version: 2\nethernets:\n  management:\n    match:\n      macaddress: \"{}\"\n    set-name: management\n    addresses: [{}/{}]\n    routes:\n      - to: default\n        via: {}\n    nameservers:\n      addresses: [{}]\n",
        switch.management_mac,
        switch.management_address,
        settings.prefix_length,
        settings.gateway,
        settings.gateway
    );
    for machine in topology.attached_machines() {
        let _ = write!(
            configuration,
            "  {}:\n    match:\n      macaddress: \"{}\"\n    set-name: {}\n",
            machine.switch_port,
            port_mac(machine),
            machine.switch_port
        );
    }
    let ports: Vec<&str> = topology.attached_machines().map(|machine| machine.switch_port.as_str()).collect();
    let _ = write!(
        configuration,
        "bridges:\n  underlay:\n    interfaces: [{}]\n    addresses: [{}/{}]\n    parameters:\n      stp: false\n      forward-delay: 0\n",
        ports.join(", "),
        switch.underlay_address,
        settings.underlay.prefix_length()
    );
    configuration
}

fn user_data() -> String {
    format!("#cloud-config\npackages:\n  - nftables\nruncmd:\n  - [touch, {READY_MARKER}]\n")
}

pub async fn create(lxd: &LxdClient, settings: &LabSettings, topology: &LabTopology) -> anyhow::Result<()> {
    let switch = &topology.switch;
    let mut devices = Map::new();
    devices.insert(
        String::from("root"),
        json!({"type": "disk", "path": "/", "pool": settings.storage_pool, "size": "8GiB"}),
    );
    devices.insert(
        String::from("eth0"),
        json!({
            "type": "nic",
            "network": settings.network,
            "hwaddr": switch.management_mac.to_string(),
            "ipv4.address": switch.management_address.to_string(),
        }),
    );
    for machine in topology.attached_machines() {
        devices.insert(
            machine.switch_port.clone(),
            json!({"type": "nic", "network": machine.port_network, "hwaddr": port_mac(machine).to_string()}),
        );
    }
    let definition = json!({
        "name": SWITCH_INSTANCE,
        "type": "virtual-machine",
        "source": {"type": "image", "mode": "pull", "server": "https://cloud-images.ubuntu.com/releases", "protocol": "simplestreams", "alias": "24.04"},
        "config": {
            "limits.cpu": "1",
            "limits.memory": "768MiB",
            "cloud-init.user-data": user_data(),
            "cloud-init.network-config": network_configuration(settings, topology),
        },
        "devices": devices,
    });
    lxd.request(Method::POST, "/1.0/instances", Some(definition)).await?;
    lxd.change_state(SWITCH_INSTANCE, "start", false).await
}

pub async fn wait_until_ready(lxd: &LxdClient) -> anyhow::Result<Duration> {
    wait_for_marker(lxd, SWITCH_INSTANCE, READY_MARKER, READY_TIMEOUT).await
}

#[must_use]
pub fn fault_ruleset(isolated_ports: &BTreeSet<String>) -> String {
    let mut ruleset = format!(
        "table bridge {FAULT_TABLE}\ndelete table bridge {FAULT_TABLE}\ntable bridge {FAULT_TABLE} {{\n\tchain forward {{\n\t\ttype filter hook forward priority 0; policy accept;\n"
    );
    for port in isolated_ports {
        let _ = writeln!(ruleset, "\t\tiifname \"{port}\" drop\n\t\toifname \"{port}\" drop");
    }
    ruleset.push_str("\t}\n}\n");
    ruleset
}

pub async fn apply_partitions(lxd: &LxdClient, isolated_ports: &BTreeSet<String>) -> anyhow::Result<()> {
    let ruleset = fault_ruleset(isolated_ports);
    let output = lxd.execute(SWITCH_INSTANCE, &format!("nft -f - <<'RULES'\n{ruleset}RULES\n")).await?;
    if !output.succeeded() {
        bail!("the lab switch refused the fault rules: {}", output.standard_error);
    }
    Ok(())
}

pub async fn probe_http(lxd: &LxdClient, address: Ipv4Addr, port: u16) -> anyhow::Result<String> {
    let output =
        lxd.execute(SWITCH_INSTANCE, &format!("wget -q -T 3 -O - http://{address}:{port}/ || true")).await?;
    if output.exit_code != 0 {
        bail!("probing {address}:{port} from the lab switch failed: {}", output.standard_error);
    }
    Ok(output.standard_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_machine_gets_a_port_on_the_underlay_bridge() {
        let settings = LabSettings::default();
        let topology = LabTopology::standard(&settings, 4);
        let configuration = network_configuration(&settings, &topology);
        for machine in topology.attached_machines() {
            assert!(
                configuration.contains(&format!("set-name: {}", machine.switch_port)),
                "{}",
                machine.name
            );
        }
        assert!(configuration.contains("addresses: [10.216.0.1/24]"));
    }

    #[test]
    fn an_isolated_port_drops_traffic_both_ways() {
        let ruleset = fault_ruleset(&BTreeSet::from([String::from("port-n1")]));
        assert!(ruleset.contains("iifname \"port-n1\" drop"));
        assert!(ruleset.contains("oifname \"port-n1\" drop"));
        assert!(fault_ruleset(&BTreeSet::new()).contains("policy accept"));
    }
}
