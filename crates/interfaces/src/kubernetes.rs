use std::net::Ipv4Addr;

use crate::error::ActionError;

pub const K8SD_PORT: u16 = 6400;
pub const KUBERNETES_API_PORT: u16 = 6443;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesEndpoint {
    pub address: Ipv4Addr,
    pub certificate_authority: String,
    pub k8sd_token: String,
    pub api_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    ControlPlane,
    Worker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainState {
    Schedulable,
    Draining,
    Drained,
}

#[async_trait::async_trait]
pub trait KubernetesPlatform: Send + Sync {
    async fn is_api_ready(&self, endpoint: &KubernetesEndpoint) -> Result<bool, ActionError>;

    async fn join_token(
        &self,
        endpoint: &KubernetesEndpoint,
        node: &str,
        role: NodeRole,
    ) -> Result<String, ActionError>;

    async fn is_node_ready(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError>;

    async fn has_node(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError>;

    async fn drain(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError>;

    async fn drain_state(&self, endpoint: &KubernetesEndpoint, node: &str)
    -> Result<DrainState, ActionError>;

    async fn uncordon(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError>;

    async fn remove_node(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError>;

    async fn apply(
        &self,
        endpoint: &KubernetesEndpoint,
        manifests: &[serde_json::Value],
    ) -> Result<(), ActionError>;

    async fn is_applied(
        &self,
        endpoint: &KubernetesEndpoint,
        manifests: &[serde_json::Value],
    ) -> Result<bool, ActionError>;
}
