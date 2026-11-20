mod api;
mod k8sd;
mod resources;
mod tls;

use std::time::Duration;

use amafu_interfaces::{ActionError, DrainState, KubernetesEndpoint, KubernetesPlatform, NodeRole};
use serde_json::Value;

use crate::api::KubernetesApi;
use crate::k8sd::K8sd;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
pub struct HttpKubernetes {
    timeout: Duration,
}

impl Default for HttpKubernetes {
    fn default() -> Self {
        Self { timeout: REQUEST_TIMEOUT }
    }
}

impl HttpKubernetes {
    fn api(&self, endpoint: &KubernetesEndpoint) -> Result<KubernetesApi, ActionError> {
        KubernetesApi::connect(endpoint, self.timeout)
    }

    fn k8sd(&self, endpoint: &KubernetesEndpoint) -> Result<K8sd, ActionError> {
        K8sd::connect(endpoint, self.timeout)
    }
}

#[async_trait::async_trait]
impl KubernetesPlatform for HttpKubernetes {
    async fn is_api_ready(&self, endpoint: &KubernetesEndpoint) -> Result<bool, ActionError> {
        Ok(self.api(endpoint)?.is_ready().await)
    }

    async fn join_token(
        &self,
        endpoint: &KubernetesEndpoint,
        node: &str,
        role: NodeRole,
    ) -> Result<String, ActionError> {
        self.k8sd(endpoint)?.join_token(node, role).await
    }

    async fn is_node_ready(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError> {
        Ok(self.api(endpoint)?.node(node).await?.is_some_and(|found| resources::is_node_ready(&found)))
    }

    async fn has_node(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<bool, ActionError> {
        Ok(self.api(endpoint)?.node(node).await?.is_some())
    }

    async fn drain(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        let api = self.api(endpoint)?;
        api.set_unschedulable(node, true).await?;
        for pod in api.evictable_pods(node).await? {
            api.evict(&pod).await?;
        }
        Ok(())
    }

    async fn drain_state(
        &self,
        endpoint: &KubernetesEndpoint,
        node: &str,
    ) -> Result<DrainState, ActionError> {
        let api = self.api(endpoint)?;
        let found = api.node(node).await?.ok_or_else(|| ActionError::retryable(format!("no node {node}")))?;
        if !resources::is_unschedulable(&found) {
            return Ok(DrainState::Schedulable);
        }
        Ok(if api.evictable_pods(node).await?.is_empty() {
            DrainState::Drained
        } else {
            DrainState::Draining
        })
    }

    async fn uncordon(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        self.api(endpoint)?.set_unschedulable(node, false).await
    }

    async fn remove_node(&self, endpoint: &KubernetesEndpoint, node: &str) -> Result<(), ActionError> {
        self.k8sd(endpoint)?.remove_node(node).await
    }

    async fn apply(&self, endpoint: &KubernetesEndpoint, manifests: &[Value]) -> Result<(), ActionError> {
        let api = self.api(endpoint)?;
        for manifest in manifests {
            api.apply(manifest).await?;
        }
        Ok(())
    }

    async fn read(
        &self,
        endpoint: &KubernetesEndpoint,
        manifest: &Value,
    ) -> Result<Option<Value>, ActionError> {
        self.api(endpoint)?.read(manifest).await
    }

    async fn delete(&self, endpoint: &KubernetesEndpoint, manifest: &Value) -> Result<(), ActionError> {
        self.api(endpoint)?.delete(manifest).await
    }

    async fn is_applied(
        &self,
        endpoint: &KubernetesEndpoint,
        manifests: &[Value],
    ) -> Result<bool, ActionError> {
        let api = self.api(endpoint)?;
        for manifest in manifests {
            if api.read(manifest).await?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
