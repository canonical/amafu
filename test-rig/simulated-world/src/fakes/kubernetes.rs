use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use amafu_interfaces::{ActionError, DrainState, KubernetesEndpoint, KubernetesPlatform, NodeRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeNode {
    pub role: NodeRole,
    pub is_cordoned: bool,
    pub evictable_pods: u32,
}

#[derive(Debug, Default)]
struct State {
    is_api_ready: bool,
    nodes: BTreeMap<String, FakeNode>,
    applied: BTreeSet<String>,
    tokens_seen: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct FakeKubernetes {
    state: Mutex<State>,
}

impl FakeKubernetes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub fn bootstrap(&self, node: &str) {
        self.with(|state| {
            state.is_api_ready = true;
            state.nodes.insert(
                node.to_owned(),
                FakeNode { role: NodeRole::ControlPlane, is_cordoned: false, evictable_pods: 3 },
            );
        });
    }

    #[must_use]
    pub fn nodes(&self) -> BTreeMap<String, FakeNode> {
        self.with(|state| state.nodes.clone())
    }

    #[must_use]
    pub fn applied(&self) -> BTreeSet<String> {
        self.with(|state| state.applied.clone())
    }

    #[must_use]
    pub fn api_tokens_seen(&self) -> usize {
        self.with(|state| state.tokens_seen.len())
    }

    fn observe(&self, endpoint: &KubernetesEndpoint) {
        self.with(|state| state.tokens_seen.insert(endpoint.api_token.clone()));
    }

    fn require_node<T>(&self, node: &str, change: impl FnOnce(&mut FakeNode) -> T) -> Result<T, ActionError> {
        self.with(|state| {
            state
                .nodes
                .get_mut(node)
                .map(change)
                .ok_or_else(|| ActionError::retryable(format!("no node {node}")))
        })
    }
}

#[async_trait::async_trait]
impl KubernetesPlatform for FakeKubernetes {
    async fn is_api_ready(&self, endpoint: &KubernetesEndpoint) -> Result<bool, ActionError> {
        self.observe(endpoint);
        Ok(self.with(|state| state.is_api_ready))
    }

    async fn join_token(
        &self,
        endpoint: &KubernetesEndpoint,
        node: &str,
        role: NodeRole,
    ) -> Result<String, ActionError> {
        self.observe(endpoint);
        self.with(|state| {
            state.nodes.insert(node.to_owned(), FakeNode { role, is_cordoned: false, evictable_pods: 2 });
        });
        Ok(format!("join-{node}"))
    }

    async fn is_node_ready(&self, _endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError> {
        Ok(self.with(|state| state.nodes.contains_key(node)))
    }

    async fn has_node(&self, _endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError> {
        Ok(self.with(|state| state.nodes.contains_key(node)))
    }

    async fn drain(&self, _endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        self.require_node(node, |found| {
            found.is_cordoned = true;
            found.evictable_pods = found.evictable_pods.saturating_sub(1);
        })
    }

    async fn drain_state(
        &self,
        _endpoint: &KubernetesEndpoint,
        node: &str,
    ) -> Result<DrainState, ActionError> {
        self.require_node(node, |found| match (found.is_cordoned, found.evictable_pods) {
            (false, _) => DrainState::Schedulable,
            (true, 0) => DrainState::Drained,
            (true, _) => DrainState::Draining,
        })
    }

    async fn uncordon(&self, _endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        self.require_node(node, |found| found.is_cordoned = false)
    }

    async fn remove_node(&self, _endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        self.with(|state| state.nodes.remove(node));
        Ok(())
    }

    async fn apply(
        &self,
        _endpoint: &KubernetesEndpoint,
        manifests: &[serde_json::Value],
    ) -> Result<(), ActionError> {
        self.with(|state| {
            for manifest in manifests {
                state.applied.insert(manifest["metadata"]["name"].as_str().unwrap_or_default().to_owned());
            }
        });
        Ok(())
    }

    async fn is_applied(
        &self,
        _endpoint: &KubernetesEndpoint,
        manifests: &[serde_json::Value],
    ) -> Result<bool, ActionError> {
        Ok(self.with(|state| {
            manifests.iter().all(|manifest| {
                state.applied.contains(manifest["metadata"]["name"].as_str().unwrap_or_default())
            })
        }))
    }
}
