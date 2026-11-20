use std::time::Duration;

use amafu_interfaces::kubernetes::KUBERNETES_API_PORT;
use amafu_interfaces::{ActionError, KubernetesEndpoint};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::{resources, tls};

const FIELD_MANAGER: &str = "amafu";
const APPLY_PATCH: &str = "application/apply-patch+yaml";
const MERGE_PATCH: &str = "application/merge-patch+json";

fn object_path(manifest: &Value) -> Result<String, ActionError> {
    resources::object_path(manifest)
        .ok_or_else(|| ActionError::fatal("a manifest needs an apiVersion, a kind and a name"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodReference {
    namespace: String,
    name: String,
}

pub struct KubernetesApi {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl KubernetesApi {
    pub fn connect(endpoint: &KubernetesEndpoint, timeout: Duration) -> Result<Self, ActionError> {
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls::trusting(&endpoint.certificate_authority)?)
            .timeout(timeout)
            .build()
            .map_err(ActionError::fatal)?;
        Ok(Self {
            client,
            base: format!("https://{}:{KUBERNETES_API_PORT}", endpoint.address),
            token: endpoint.api_token.clone(),
        })
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token)
    }

    async fn send(request: reqwest::RequestBuilder, what: &str) -> Result<Option<Value>, ActionError> {
        let response = request.send().await.map_err(ActionError::retryable)?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => Ok(Some(response.json().await.map_err(ActionError::retryable)?)),
            status => {
                let detail = response.text().await.unwrap_or_default();
                Err(ActionError::retryable(format!(
                    "the Kubernetes API answered {what} with {status}: {detail}"
                )))
            }
        }
    }

    pub async fn is_ready(&self) -> bool {
        self.request(Method::GET, "/readyz").send().await.is_ok_and(|response| response.status().is_success())
    }

    pub async fn node(&self, node: &str) -> Result<Option<Value>, ActionError> {
        Self::send(self.request(Method::GET, &format!("/api/v1/nodes/{node}")), "a node read").await
    }

    pub async fn set_unschedulable(&self, node: &str, unschedulable: bool) -> Result<(), ActionError> {
        let patch = json!({"spec": {"unschedulable": unschedulable}});
        let request = self
            .request(Method::PATCH, &format!("/api/v1/nodes/{node}"))
            .header(reqwest::header::CONTENT_TYPE, MERGE_PATCH)
            .body(patch.to_string());
        Self::send(request, "a cordon").await?;
        Ok(())
    }

    pub async fn evictable_pods(&self, node: &str) -> Result<Vec<PodReference>, ActionError> {
        let request = self
            .request(Method::GET, "/api/v1/pods")
            .query(&[("fieldSelector", format!("spec.nodeName={node}"))]);
        let pods = Self::send(request, "a pod list").await?.unwrap_or_default();
        Ok(pods["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|pod| resources::is_evictable(pod))
                    .filter_map(|pod| {
                        Some(PodReference {
                            namespace: pod["metadata"]["namespace"].as_str()?.to_owned(),
                            name: pod["metadata"]["name"].as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn evict(&self, pod: &PodReference) -> Result<(), ActionError> {
        let eviction = json!({
            "apiVersion": "policy/v1",
            "kind": "Eviction",
            "metadata": {"name": pod.name, "namespace": pod.namespace},
        });
        let path = format!("/api/v1/namespaces/{}/pods/{}/eviction", pod.namespace, pod.name);
        let response =
            self.request(Method::POST, &path).json(&eviction).send().await.map_err(ActionError::retryable)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::NOT_FOUND => Ok(()),
            status => Err(ActionError::retryable(format!(
                "evicting {}/{} failed with {status}",
                pod.namespace, pod.name
            ))),
        }
    }

    pub async fn apply(&self, manifest: &Value) -> Result<(), ActionError> {
        let path = object_path(manifest)?;
        let request = self
            .request(Method::PATCH, &path)
            .query(&[("fieldManager", FIELD_MANAGER), ("force", "true")])
            .header(reqwest::header::CONTENT_TYPE, APPLY_PATCH)
            .body(manifest.to_string());
        Self::send(request, "an apply").await?;
        Ok(())
    }

    pub async fn read(&self, manifest: &Value) -> Result<Option<Value>, ActionError> {
        Self::send(self.request(Method::GET, &object_path(manifest)?), "a read").await
    }

    pub async fn delete(&self, manifest: &Value) -> Result<(), ActionError> {
        Self::send(self.request(Method::DELETE, &object_path(manifest)?), "a delete").await?;
        Ok(())
    }
}
