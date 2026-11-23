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

#[async_trait::async_trait]
pub trait LxdPlatform: Send + Sync {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError>;

    async fn members(&self, endpoint: &LxdEndpoint) -> Result<Vec<String>, ActionError>;

    async fn enable_clustering(&self, endpoint: &LxdEndpoint, server_name: &str) -> Result<(), ActionError>;

    async fn invite(&self, endpoint: &LxdEndpoint, member: &str) -> Result<String, ActionError>;

    async fn remove_member(&self, endpoint: &LxdEndpoint, member: &str) -> Result<(), ActionError>;
}
