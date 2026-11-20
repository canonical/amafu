use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use std::sync::Mutex;

use amafu_interfaces::{ActionError, DrainState, KubernetesEndpoint, KubernetesPlatform, NodeRole};

fn key(manifest: &Value) -> String {
    format!(
        "{}/{}/{}",
        manifest["kind"].as_str().unwrap_or_default(),
        manifest["metadata"]["namespace"].as_str().unwrap_or_default(),
        manifest["metadata"]["name"].as_str().unwrap_or_default()
    )
}

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
    objects: BTreeMap<String, Value>,
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
        self.with(|state| state.objects.keys().cloned().collect())
    }

    pub fn put(&self, manifest: &Value) {
        let mut stored = manifest.clone();
        if manifest["type"] == "kubernetes.io/service-account-token" {
            stored["data"] = json!({"token": "ZmFrZS10b2tlbg=="});
        }
        self.with(|state| state.objects.insert(key(manifest), stored));
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

    async fn apply(&self, _endpoint: &KubernetesEndpoint, manifests: &[Value]) -> Result<(), ActionError> {
        for manifest in manifests {
            if self.with(|state| !state.objects.contains_key(&key(manifest))) {
                self.put(manifest);
            }
        }
        Ok(())
    }

    async fn read(
        &self,
        _endpoint: &KubernetesEndpoint,
        manifest: &Value,
    ) -> Result<Option<Value>, ActionError> {
        Ok(self.with(|state| state.objects.get(&key(manifest)).cloned()))
    }

    async fn delete(&self, _endpoint: &KubernetesEndpoint, manifest: &Value) -> Result<(), ActionError> {
        self.with(|state| state.objects.remove(&key(manifest)));
        Ok(())
    }

    async fn is_applied(
        &self,
        _endpoint: &KubernetesEndpoint,
        manifests: &[Value],
    ) -> Result<bool, ActionError> {
        Ok(self.with(|state| manifests.iter().all(|manifest| state.objects.contains_key(&key(manifest)))))
    }
}
