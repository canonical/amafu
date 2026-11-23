use std::time::Duration;

use amafu_interfaces::kubernetes::K8SD_PORT;
use amafu_interfaces::{ActionError, KubernetesEndpoint, NodeRole};
use serde::Deserialize;
use serde_json::json;

const AUTH_HEADER: &str = "capi-auth-token";

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    metadata: T,
}

#[derive(Debug, Deserialize)]
struct JoinToken {
    token: String,
}

pub struct K8sd {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl K8sd {
    pub fn connect(endpoint: &KubernetesEndpoint, timeout: Duration) -> Result<Self, ActionError> {
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(amafu_client_tls::self_signed()?)
            .timeout(timeout)
            .build()
            .map_err(ActionError::fatal)?;
        Ok(Self {
            client,
            base: format!("https://{}:{K8SD_PORT}/1.0/x/capi", endpoint.address),
            token: endpoint.k8sd_token.clone(),
        })
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<reqwest::Response, ActionError> {
        let response = self
            .client
            .post(format!("{}/{path}", self.base))
            .header(AUTH_HEADER, &self.token)
            .json(&body)
            .send()
            .await
            .map_err(ActionError::retryable)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        Err(ActionError::retryable(format!("k8sd answered {path} with {status}: {detail}")))
    }

    pub async fn join_token(&self, node: &str, role: NodeRole) -> Result<String, ActionError> {
        let worker = role == NodeRole::Worker;
        let response = self.post("generate-join-token", json!({ "name": node, "worker": worker })).await?;
        let envelope: Envelope<JoinToken> = response.json().await.map_err(ActionError::retryable)?;
        Ok(envelope.metadata.token)
    }

    pub async fn remove_node(&self, node: &str) -> Result<(), ActionError> {
        self.post("remove-node", json!({ "name": node, "force": false })).await?;
        Ok(())
    }
}
