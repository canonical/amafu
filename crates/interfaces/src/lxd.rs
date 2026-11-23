use std::net::Ipv4Addr;

use amafu_domain::kinds::guest_inventory::LxdAccess;

use crate::error::ActionError;

pub const LXD_PORT: u16 = 8443;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LxdEndpoint {
    pub address: Ipv4Addr,
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterInvitation {
    pub token: String,
    pub cluster_address: Ipv4Addr,
    pub cluster_certificate: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterJoin {
    pub server_name: String,
    pub server_address: Ipv4Addr,
    pub invitation: ClusterInvitation,
}

#[async_trait::async_trait]
pub trait LxdPlatform: Send + Sync {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError>;

    async fn members(&self, endpoint: &LxdEndpoint) -> Result<Vec<String>, ActionError>;

    async fn enable_clustering(&self, endpoint: &LxdEndpoint, server_name: &str) -> Result<(), ActionError>;

    async fn invite(&self, endpoint: &LxdEndpoint, member: &str) -> Result<ClusterInvitation, ActionError>;

    async fn join(&self, endpoint: &LxdEndpoint, join: &ClusterJoin) -> Result<(), ActionError>;
}
