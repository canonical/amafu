use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

use amafu_domain::kinds::guest_inventory::LxdAccess;
use amafu_interfaces::{ActionError, ClusterJoin, LxdEndpoint, LxdPlatform};

use crate::lock::lock;

#[derive(Default)]
struct State {
    servers: BTreeMap<Ipv4Addr, LxdAccess>,
    cluster_of: BTreeMap<Ipv4Addr, Ipv4Addr>,
    members: BTreeMap<Ipv4Addr, Vec<String>>,
    tokens: BTreeMap<String, Ipv4Addr>,
}

#[derive(Default)]
pub struct FakeLxd {
    state: Mutex<State>,
}

impl FakeLxd {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn serve(&self, address: Ipv4Addr, access: LxdAccess) {
        lock(&self.state).servers.insert(address, access);
    }

    pub fn stop(&self, address: Ipv4Addr) {
        lock(&self.state).servers.remove(&address);
    }
}

#[async_trait::async_trait]
impl LxdPlatform for FakeLxd {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError> {
        Ok(lock(&self.state).servers.get(&endpoint.address).cloned())
    }

    async fn members(&self, endpoint: &LxdEndpoint) -> Result<Vec<String>, ActionError> {
        let state = lock(&self.state);
        let cluster = state.cluster_of.get(&endpoint.address);
        Ok(cluster.and_then(|founder| state.members.get(founder)).cloned().unwrap_or_default())
    }

    async fn enable_clustering(&self, endpoint: &LxdEndpoint, server_name: &str) -> Result<(), ActionError> {
        let mut state = lock(&self.state);
        if let std::collections::btree_map::Entry::Vacant(entry) = state.cluster_of.entry(endpoint.address) {
            entry.insert(endpoint.address);
            state.members.insert(endpoint.address, vec![server_name.to_owned()]);
        }
        Ok(())
    }

    async fn join_token(&self, endpoint: &LxdEndpoint, member: &str) -> Result<String, ActionError> {
        let mut state = lock(&self.state);
        let founder = *state
            .cluster_of
            .get(&endpoint.address)
            .ok_or_else(|| ActionError::retryable("not clustered"))?;
        let token = format!("token-for-{member}");
        state.tokens.insert(token.clone(), founder);
        Ok(token)
    }

    async fn join(&self, endpoint: &LxdEndpoint, join: &ClusterJoin) -> Result<(), ActionError> {
        let mut state = lock(&self.state);
        let founder =
            *state.tokens.get(&join.token).ok_or_else(|| ActionError::retryable("unknown join token"))?;
        state.cluster_of.insert(endpoint.address, founder);
        let members = state.members.entry(founder).or_default();
        if !members.contains(&join.server_name) {
            members.push(join.server_name.clone());
        }
        Ok(())
    }
}
