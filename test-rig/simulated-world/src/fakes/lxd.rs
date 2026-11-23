use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

use amafu_domain::kinds::guest_inventory::LxdAccess;
use amafu_interfaces::{ActionError, LxdEndpoint, LxdPlatform};

use crate::lock::lock;

#[derive(Default)]
pub struct FakeLxd {
    servers: Mutex<BTreeMap<Ipv4Addr, LxdAccess>>,
}

impl FakeLxd {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn serve(&self, address: Ipv4Addr, access: LxdAccess) {
        lock(&self.servers).insert(address, access);
    }

    pub fn stop(&self, address: Ipv4Addr) {
        lock(&self.servers).remove(&address);
    }
}

#[async_trait::async_trait]
impl LxdPlatform for FakeLxd {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError> {
        Ok(lock(&self.servers).get(&endpoint.address).cloned())
    }
}
