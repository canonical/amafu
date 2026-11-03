use std::collections::BTreeSet;
use std::fmt;
use std::net::Ipv4Addr;

use amafu_domain::kinds::security_group::{Direction, PortRange, Protocol};
use amafu_domain::{Ipv4Network, MacAddress, ObjectId, SegmentId};
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SegmentKey(pub String);

impl SegmentKey {
    #[must_use]
    pub fn of(project: &str, network: &str) -> Self {
        Self(format!("{project}/{network}"))
    }
}

impl fmt::Display for SegmentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortKey {
    pub vm_id: ObjectId,
    pub index: u8,
}

fn short_hash(text: &str) -> u64 {
    text.bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

impl PortKey {
    #[must_use]
    pub fn tap_name(&self) -> String {
        format!("amt{:012x}", short_hash(&format!("{}/{}", self.vm_id, self.index)) & 0xffff_ffff_ffff)
    }
}

#[must_use]
pub fn bridge_name(segment: &SegmentKey) -> String {
    format!("amb{:012x}", short_hash(&segment.0) & 0xffff_ffff_ffff)
}

#[must_use]
pub fn tunnel_name(segment: &SegmentKey, segment_id: SegmentId) -> String {
    format!("amx{:012x}", short_hash(&format!("{}#{segment_id}", segment.0)) & 0xffff_ffff_ffff)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub key: SegmentKey,
    pub subnet: Ipv4Network,
    pub is_outbound_access_allowed: bool,
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<SegmentId>,
}

impl Segment {
    #[must_use]
    pub fn gateway(&self) -> Ipv4Addr {
        self.subnet.gateway()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RemoteAddresses {
    Any,
    Networks { networks: Vec<Ipv4Network> },
}

impl RemoteAddresses {
    #[must_use]
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        match self {
            Self::Any => true,
            Self::Networks { networks } => networks.iter().any(|network| network.contains(address)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortRule {
    pub direction: Direction,
    pub protocol: Protocol,
    pub ports: Option<PortRange>,
    pub remote: RemoteAddresses,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub key: PortKey,
    pub segment: SegmentKey,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
    pub hostname: String,
    pub rules: Vec<PortRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RemoteEndpoint {
    pub segment: SegmentKey,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
    pub hostname: String,
    pub node_address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FloatingAddress {
    pub address: Ipv4Addr,
    pub private_address: Ipv4Addr,
    pub segment: SegmentKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeNetworkState {
    pub segments: Vec<Segment>,
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_endpoints: Vec<RemoteEndpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floating_addresses: Vec<FloatingAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay_address: Option<Ipv4Addr>,
}

impl NodeNetworkState {
    #[must_use]
    pub fn segment(&self, key: &SegmentKey) -> Option<&Segment> {
        self.segments.iter().find(|segment| &segment.key == key)
    }

    #[must_use]
    pub fn port_with_mac(&self, mac: MacAddress) -> Option<&Port> {
        self.ports.iter().find(|port| port.mac == mac)
    }

    #[must_use]
    pub fn port_with_address(&self, segment: &SegmentKey, address: Ipv4Addr) -> Option<&Port> {
        self.ports.iter().find(|port| &port.segment == segment && port.address == address)
    }

    #[must_use]
    pub fn remote_with_hostname(&self, segment: &SegmentKey, hostname: &str) -> Option<&RemoteEndpoint> {
        self.remote_endpoints.iter().find(|remote| &remote.segment == segment && remote.hostname == hostname)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RealizedNetwork {
    pub segments: BTreeSet<SegmentKey>,
    pub ports: BTreeSet<PortKey>,
}

#[async_trait::async_trait]
pub trait Datapath: Send + Sync {
    async fn converge(&self, desired: &NodeNetworkState) -> Result<RealizedNetwork, ActionError>;

    async fn current(&self) -> Result<RealizedNetwork, ActionError>;
}

pub trait LocalServices: Send + Sync {
    fn update(&self, state: &NodeNetworkState);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_names_fit_the_kernel_limit_and_are_stable() {
        let key = PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 };
        assert!(key.tap_name().len() <= 15);
        assert_eq!(key.tap_name(), key.tap_name());
        assert_ne!(key.tap_name(), PortKey { index: 1, ..key }.tap_name());
        assert!(bridge_name(&SegmentKey::of("project-a", "default")).len() <= 15);
        let segment = SegmentKey::of("project-a", "default");
        let first = SegmentId::new(1).unwrap();
        assert!(tunnel_name(&segment, first).len() <= 15);
        assert_ne!(tunnel_name(&segment, first), tunnel_name(&segment, SegmentId::new(2).unwrap()));
    }
}
