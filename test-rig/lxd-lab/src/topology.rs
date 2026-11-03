use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use amafu_domain::kinds::external_network::ExternalNetworkSpecification;
use amafu_domain::{Ipv4Network, MacAddress};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabSettings {
    pub lxd_endpoint: String,
    pub lxd_remote: String,
    pub project: String,
    pub host_address: Ipv4Addr,
    pub network: String,
    pub storage_pool: String,
    pub gateway: Ipv4Addr,
    pub prefix_length: u8,
    pub underlay: Ipv4Network,
    pub image_directory: PathBuf,
    pub first_proxy_port: u16,
}

impl Default for LabSettings {
    fn default() -> Self {
        Self {
            lxd_endpoint: String::from("https://10.0.10.13:8443"),
            lxd_remote: String::from("amafu-lab"),
            project: String::from("amafu-lab"),
            host_address: Ipv4Addr::new(10, 0, 10, 13),
            network: String::from("lxdbr0"),
            storage_pool: String::from("default"),
            gateway: Ipv4Addr::new(10, 215, 2, 1),
            prefix_length: 24,
            underlay: Ipv4Network::new(Ipv4Addr::new(10, 216, 0, 0), 24).unwrap_or_else(|_| unreachable!()),
            image_directory: PathBuf::from("image/output/test"),
            first_proxy_port: 17_443,
        }
    }
}

impl LabSettings {
    #[must_use]
    pub fn management_address(&self, host: u8) -> Ipv4Addr {
        let octets = self.gateway.octets();
        Ipv4Addr::new(octets[0], octets[1], octets[2], host)
    }

    #[must_use]
    pub fn underlay_address(&self, host: u8) -> Ipv4Addr {
        let octets = self.underlay.network_address().octets();
        Ipv4Addr::new(octets[0], octets[1], octets[2], host)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabMachine {
    pub name: String,
    pub host: u8,
    pub management_address: Ipv4Addr,
    pub underlay_address: Ipv4Addr,
    pub management_mac: MacAddress,
    pub underlay_mac: MacAddress,
    pub port_network: String,
    pub switch_port: String,
}

impl LabMachine {
    fn new(settings: &LabSettings, name: &str, host: u8, port: &str) -> Self {
        Self {
            name: name.to_owned(),
            host,
            management_address: settings.management_address(host),
            underlay_address: settings.underlay_address(host),
            management_mac: MacAddress::new([0x00, 0x16, 0x3e, 0xa0, 0x00, host]),
            underlay_mac: MacAddress::new([0x00, 0x16, 0x3e, 0xa1, 0x00, host]),
            port_network: format!("amfl-{port}"),
            switch_port: format!("port-{port}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabNode {
    pub machine: LabMachine,
    pub cpu_count: u32,
    pub memory_gibibytes: u32,
    pub disk_gibibytes: u32,
    pub api_port: u16,
    pub diagnostics_port: u16,
    pub is_core: bool,
}

impl LabNode {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.machine.name
    }

    #[must_use]
    pub fn api_endpoint(&self, settings: &LabSettings) -> String {
        format!("https://{}:{}", settings.host_address, self.api_port)
    }

    #[must_use]
    pub fn diagnostics_endpoint(&self, settings: &LabSettings) -> String {
        format!("http://{}:{}", settings.host_address, self.diagnostics_port)
    }

    #[must_use]
    pub fn underlay_api_endpoint(&self) -> String {
        format!("https://{}:{NODE_API_PORT}", self.machine.underlay_address)
    }

    #[must_use]
    pub fn state_address(&self) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(self.machine.underlay_address, NODE_STATE_PORT))
    }
}

pub const NODE_API_PORT: u16 = 7443;
pub const NODE_DIAGNOSTICS_PORT: u16 = 7444;
pub const NODE_STATE_PORT: u16 = 7400;
pub const CORE_MEMBER_COUNT: usize = 3;
const FIRST_NODE_HOST: u8 = 50;
const FIRST_NODE_UNDERLAY_HOST: u8 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabTopology {
    pub nodes: Vec<LabNode>,
    pub switch: LabMachine,
    pub nfs_server: LabMachine,
    pub nvme_target: LabMachine,
    pub ceph: LabMachine,
    pub external_network: ExternalNetworkSpecification,
}

impl LabTopology {
    #[must_use]
    pub fn standard(settings: &LabSettings, node_count: u8) -> Self {
        let nodes = (0..node_count)
            .map(|index| {
                let port_offset = u16::from(index) * 2;
                let mut machine = LabMachine::new(
                    settings,
                    &format!("node-{index}"),
                    FIRST_NODE_HOST + index,
                    &format!("n{index}"),
                );
                machine.underlay_address = settings.underlay_address(FIRST_NODE_UNDERLAY_HOST + index);
                LabNode {
                    machine,
                    cpu_count: 2,
                    memory_gibibytes: 3,
                    disk_gibibytes: 20,
                    api_port: settings.first_proxy_port + port_offset,
                    diagnostics_port: settings.first_proxy_port + port_offset + 1,
                    is_core: usize::from(index) < CORE_MEMBER_COUNT,
                }
            })
            .collect();
        let mut switch = LabMachine::new(settings, "lab-switch", 30, "uplink");
        switch.underlay_address = settings.underlay_address(1);
        let underlay_octets = settings.underlay.network_address().octets();
        let external_network = ExternalNetworkSpecification {
            subnet: settings.underlay,
            first_address: Ipv4Addr::new(underlay_octets[0], underlay_octets[1], underlay_octets[2], 100),
            last_address: Ipv4Addr::new(underlay_octets[0], underlay_octets[1], underlay_octets[2], 149),
        };
        Self {
            nodes,
            switch,
            nfs_server: LabMachine::new(settings, "nfs-server", 40, "nfs"),
            nvme_target: LabMachine::new(settings, "nvme-target", 41, "nvme"),
            ceph: LabMachine::new(settings, "ceph", 42, "ceph"),
            external_network,
        }
    }

    pub fn attached_machines(&self) -> impl Iterator<Item = &LabMachine> {
        self.nodes.iter().map(|node| &node.machine).chain([&self.nfs_server, &self.nvme_target, &self.ceph])
    }

    #[must_use]
    pub fn node(&self, name: &str) -> Option<&LabNode> {
        self.nodes.iter().find(|node| node.name() == name)
    }

    pub fn core_members(&self) -> impl Iterator<Item = &LabNode> {
        self.nodes.iter().filter(|node| node.is_core)
    }
}

#[must_use]
pub fn node_listen(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_machine_has_its_own_addresses_and_switch_port() {
        let topology = LabTopology::standard(&LabSettings::default(), 4);
        let machines: Vec<&LabMachine> = topology.attached_machines().chain([&topology.switch]).collect();
        let mut seen = std::collections::BTreeSet::new();
        for machine in &machines {
            assert!(seen.insert(machine.management_address), "{}", machine.name);
            assert!(seen.insert(machine.underlay_address), "{}", machine.name);
            assert!(machine.port_network.len() <= 15, "{}", machine.port_network);
            assert!(machine.switch_port.len() <= 15, "{}", machine.switch_port);
        }
        assert_eq!(topology.core_members().count(), 3);
        assert!(topology.external_network.contains(Ipv4Addr::new(10, 216, 0, 120)));
        assert!(
            !topology
                .attached_machines()
                .any(|machine| topology.external_network.contains(machine.underlay_address))
        );
    }
}
