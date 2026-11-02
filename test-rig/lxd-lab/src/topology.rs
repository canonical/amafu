use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

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
            image_directory: PathBuf::from("image/output/test"),
            first_proxy_port: 17_443,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabNode {
    pub name: String,
    pub cpu_count: u32,
    pub memory_gibibytes: u32,
    pub disk_gibibytes: u32,
    pub address: Ipv4Addr,
    pub api_port: u16,
    pub diagnostics_port: u16,
}

impl LabNode {
    #[must_use]
    pub fn api_endpoint(&self, settings: &LabSettings) -> String {
        format!("https://{}:{}", settings.host_address, self.api_port)
    }

    #[must_use]
    pub fn diagnostics_endpoint(&self, settings: &LabSettings) -> String {
        format!("http://{}:{}", settings.host_address, self.diagnostics_port)
    }
}

pub const NODE_API_PORT: u16 = 7443;
pub const NODE_DIAGNOSTICS_PORT: u16 = 7444;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabTopology {
    pub nodes: Vec<LabNode>,
}

impl LabTopology {
    #[must_use]
    pub fn standard(settings: &LabSettings, node_count: u8) -> Self {
        let octets = settings.gateway.octets();
        let nodes = (0..node_count)
            .map(|index| {
                let port_offset = u16::from(index) * 2;
                LabNode {
                    name: format!("node-{index}"),
                    cpu_count: 2,
                    memory_gibibytes: 4,
                    disk_gibibytes: 20,
                    address: Ipv4Addr::new(octets[0], octets[1], octets[2], 50 + index),
                    api_port: settings.first_proxy_port + port_offset,
                    diagnostics_port: settings.first_proxy_port + port_offset + 1,
                }
            })
            .collect();
        Self { nodes }
    }

    #[must_use]
    pub fn next_proxy_port(&self, settings: &LabSettings) -> u16 {
        let used = u16::try_from(self.nodes.len()).unwrap_or(u16::MAX).saturating_mul(2);
        settings.first_proxy_port.saturating_add(used)
    }
}

#[must_use]
pub fn node_listen(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], port))
}
