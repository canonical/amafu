use std::time::Duration;

use amafu_domain::kinds::guest_inventory::{GuestInstance, GuestInstanceKind, LxdAccess};
use amafu_interfaces::{ActionError, LXD_PORT, LxdEndpoint, LxdPlatform};
use serde_json::Value;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const TRUSTED: &str = "trusted";

pub struct HttpsLxd {
    timeout: Duration,
}

impl Default for HttpsLxd {
    fn default() -> Self {
        Self { timeout: REQUEST_TIMEOUT }
    }
}

struct Connection {
    client: reqwest::Client,
    base: String,
}

impl Connection {
    fn open(endpoint: &LxdEndpoint, timeout: Duration) -> Result<Self, ActionError> {
        let tls = amafu_client_tls::self_signed_as(&endpoint.certificate_pem, &endpoint.private_key_pem)?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .timeout(timeout)
            .build()
            .map_err(ActionError::fatal)?;
        Ok(Self { client, base: format!("https://{}:{LXD_PORT}", endpoint.address) })
    }

    async fn get(&self, path: &str) -> Result<Value, reqwest::Error> {
        self.client.get(format!("{}{path}", self.base)).send().await?.error_for_status()?.json().await
    }
}

fn server(document: &Value) -> Option<(String, bool)> {
    let metadata = &document["metadata"];
    if metadata["auth"].as_str() != Some(TRUSTED) {
        return None;
    }
    let environment = &metadata["environment"];
    let name = environment["server_name"].as_str().unwrap_or_default().to_owned();
    Some((name, environment["server_clustered"].as_bool().unwrap_or(false)))
}

fn instance_kind(text: &str) -> GuestInstanceKind {
    if text == "virtual-machine" { GuestInstanceKind::VirtualMachine } else { GuestInstanceKind::Container }
}

fn instances(document: &Value) -> Vec<GuestInstance> {
    let mut instances: Vec<GuestInstance> = document["metadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|instance| {
            Some(GuestInstance {
                name: instance["name"].as_str()?.to_owned(),
                kind: instance_kind(instance["type"].as_str().unwrap_or_default()),
                status: instance["status"].as_str().unwrap_or_default().to_owned(),
                location: instance["location"]
                    .as_str()
                    .filter(|location| !location.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect();
    instances.sort();
    instances
}

#[async_trait::async_trait]
impl LxdPlatform for HttpsLxd {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let Ok(root) = connection.get("/1.0").await else {
            return Ok(None);
        };
        let Some((server, is_clustered)) = server(&root) else {
            return Ok(Some(LxdAccess::Untrusted));
        };
        let listing = connection
            .get("/1.0/instances?recursion=1&all-projects=true")
            .await
            .map_err(ActionError::retryable)?;
        Ok(Some(LxdAccess::Trusted { server, is_clustered, instances: instances(&listing) }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_a_trusted_server_reveals_its_name_and_cluster() {
        let untrusted = json!({"metadata": {"auth": "untrusted", "api_version": "1.0"}});
        assert_eq!(server(&untrusted), None);
        let trusted = json!({"metadata": {"auth": "trusted", "environment": {"server_name": "pod-0", "server_clustered": true}}});
        assert_eq!(server(&trusted), Some((String::from("pod-0"), true)));
    }

    #[test]
    fn instances_keep_their_kind_status_and_member() {
        let listing = json!({"metadata": [
            {"name": "juju-2a3b-1", "type": "virtual-machine", "status": "Running", "location": "pod-1"},
            {"name": "juju-2a3b-0", "type": "container", "status": "Stopped", "location": ""},
        ]});
        assert_eq!(
            instances(&listing),
            [
                GuestInstance {
                    name: String::from("juju-2a3b-0"),
                    kind: GuestInstanceKind::Container,
                    status: String::from("Stopped"),
                    location: None,
                },
                GuestInstance {
                    name: String::from("juju-2a3b-1"),
                    kind: GuestInstanceKind::VirtualMachine,
                    status: String::from("Running"),
                    location: Some(String::from("pod-1")),
                },
            ]
        );
    }
}
