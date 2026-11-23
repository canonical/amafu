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
}
