use std::time::{Duration, Instant};

use anyhow::bail;
use reqwest::Method;
use serde_json::{Value, json};

use crate::lxd::LxdClient;
use crate::topology::MANAGEMENT_MTU;
use crate::topology::{LabMachine, LabSettings};

const IMAGE_SERVER: &str = "https://cloud-images.ubuntu.com/releases";
const UBUNTU: &str = "24.04";
const READY_POLL: Duration = Duration::from_secs(5);
const UNREACHABLE_AGENT: i64 = -1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineSize {
    pub cpu_count: u32,
    pub memory_mebibytes: u32,
    pub disk_gibibytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineSource {
    Ubuntu,
    Image(String),
}

#[must_use]
pub fn network_configuration(settings: &LabSettings, machine: &LabMachine) -> String {
    format!(
        "version: 2\nethernets:\n  management:\n    match:\n      macaddress: \"{}\"\n    set-name: management\n    mtu: {MANAGEMENT_MTU}\n    addresses: [{}/{}]\n    routes:\n      - to: default\n        via: {}\n    nameservers:\n      addresses: [{}]\n  underlay:\n    match:\n      macaddress: \"{}\"\n    set-name: underlay\n    addresses: [{}/{}]\n",
        machine.management_mac,
        machine.management_address,
        settings.prefix_length,
        settings.gateway,
        settings.gateway,
        machine.underlay_mac,
        machine.underlay_address,
        settings.underlay.prefix_length(),
    )
}

fn source(source: &MachineSource) -> Value {
    match source {
        MachineSource::Ubuntu => {
            json!({"type": "image", "mode": "pull", "server": IMAGE_SERVER, "protocol": "simplestreams", "alias": UBUNTU})
        }
        MachineSource::Image(alias) => json!({"type": "image", "alias": alias}),
    }
}

#[must_use]
pub fn definition(
    settings: &LabSettings,
    machine: &LabMachine,
    size: &MachineSize,
    machine_source: &MachineSource,
    user_data: &str,
) -> Value {
    json!({
        "name": machine.name,
        "type": "virtual-machine",
        "source": source(machine_source),
        "config": {
            "limits.cpu": size.cpu_count.to_string(),
            "limits.memory": format!("{}MiB", size.memory_mebibytes),
            "cloud-init.user-data": user_data,
            "cloud-init.network-config": network_configuration(settings, machine),
        },
        "devices": {
            "root": { "type": "disk", "path": "/", "pool": settings.storage_pool, "size": format!("{}GiB", size.disk_gibibytes) },
            "eth0": {
                "type": "nic",
                "network": settings.network,
                "hwaddr": machine.management_mac.to_string(),
                "ipv4.address": machine.management_address.to_string(),
            },
            "eth1": { "type": "nic", "network": machine.port_network, "hwaddr": machine.underlay_mac.to_string() },
        },
    })
}

pub async fn create(lxd: &LxdClient, definition: Value) -> anyhow::Result<()> {
    let name = definition["name"].as_str().unwrap_or_default().to_owned();
    lxd.request(Method::POST, "/1.0/instances", Some(definition)).await?;
    lxd.change_state(&name, "start", false).await
}

pub async fn wait_for_marker(
    lxd: &LxdClient,
    instance: &str,
    marker: &str,
    timeout: Duration,
) -> anyhow::Result<Duration> {
    let started = Instant::now();
    loop {
        let check = lxd.execute(instance, &format!("test -e {marker}")).await;
        match check {
            Ok(output) if output.succeeded() => return Ok(started.elapsed()),
            Ok(output) if output.exit_code != UNREACHABLE_AGENT => {}
            Ok(_) | Err(_) => {}
        }
        if started.elapsed() >= timeout {
            bail!("{instance} did not create {marker} within {} seconds", timeout.as_secs());
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

pub async fn read_file(lxd: &LxdClient, instance: &str, path: &str) -> anyhow::Result<String> {
    let output = lxd.execute(instance, &format!("cat {path}")).await?;
    if !output.succeeded() {
        bail!("reading {path} on {instance} failed: {}", output.standard_error);
    }
    Ok(output.standard_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::LabTopology;

    #[test]
    fn both_interfaces_are_matched_by_their_address() {
        let settings = LabSettings::default();
        let topology = LabTopology::standard(&settings, 1);
        let configuration = network_configuration(&settings, &topology.nfs_server);
        assert!(configuration.contains("macaddress: \"00:16:3e:a0:00:28\""));
        assert!(configuration.contains("addresses: [10.216.0.40/24]"));
        assert!(configuration.contains("via: 10.215.2.1"));
    }
}
