use std::net::Ipv4Addr;
use std::time::Duration;

use amafu_domain::kinds::guest_inventory::{GuestInstance, GuestInstanceKind, LxdAccess};
use amafu_interfaces::{ActionError, LXD_PORT, LxdEndpoint, LxdPlatform};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const TRUSTED: &str = "trusted";
const JUJU_CONTROLLER_KEY: &str = "user.juju-is-controller";
const OPERATION_WAIT_SECONDS: u32 = 120;

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

    async fn send(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value, ActionError> {
        let response = self
            .client
            .request(method, format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(ActionError::retryable)?;
        let document: Value = response.json().await.map_err(ActionError::retryable)?;
        refusal(&document).map_or(Ok(document), |error| Err(ActionError::retryable(error)))
    }

    async fn finish(&self, started: &Value) -> Result<(), ActionError> {
        let Some(operation) = started["operation"].as_str().filter(|operation| !operation.is_empty()) else {
            return Ok(());
        };
        let finished = self.get(&format!("{operation}/wait?timeout={OPERATION_WAIT_SECONDS}")).await;
        let finished = finished.map_err(ActionError::retryable)?;
        match finished["metadata"]["err"].as_str().filter(|error| !error.is_empty()) {
            Some(error) => Err(ActionError::retryable(format!("LXD: {error}"))),
            None => Ok(()),
        }
    }
}

fn refusal(document: &Value) -> Option<String> {
    (document["type"].as_str() == Some("error"))
        .then(|| format!("LXD: {}", document["error"].as_str().unwrap_or("refused")))
}

fn join_token(metadata: &Value) -> Result<String, ActionError> {
    let field = |name: &str| metadata[name].clone();
    let token = serde_json::json!({
        "server_name": field("serverName"),
        "fingerprint": field("fingerprint"),
        "addresses": field("addresses"),
        "secret": field("secret"),
        "expires_at": field("expiresAt"),
    });
    if token.as_object().is_some_and(|fields| fields.values().any(Value::is_null)) {
        return Err(ActionError::retryable("LXD returned an incomplete join token"));
    }
    Ok(STANDARD.encode(token.to_string()))
}

fn is_absence(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
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

fn addresses(state: &Value) -> Vec<Ipv4Addr> {
    let mut found: Vec<Ipv4Addr> = state["network"]
        .as_object()
        .into_iter()
        .flat_map(|interfaces| interfaces.values())
        .flat_map(|interface| interface["addresses"].as_array().into_iter().flatten())
        .filter(|address| address["family"] == "inet" && address["scope"] == "global")
        .filter_map(|address| address["address"].as_str()?.parse().ok())
        .collect();
    found.sort();
    found
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
                addresses: addresses(&instance["state"]),
                is_juju_controller: instance["config"][JUJU_CONTROLLER_KEY].as_str() == Some("true"),
            })
        })
        .collect();
    instances.sort();
    instances
}

fn hosted_by(instances: Vec<GuestInstance>, server: &str, is_clustered: bool) -> Vec<GuestInstance> {
    instances
        .into_iter()
        .filter(|instance| !is_clustered || instance.location.as_deref() == Some(server))
        .collect()
}

#[async_trait::async_trait]
impl LxdPlatform for HttpsLxd {
    async fn inspect(&self, endpoint: &LxdEndpoint) -> Result<Option<LxdAccess>, ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let root = match connection.get("/1.0").await {
            Ok(root) => root,
            Err(error) if is_absence(&error) => return Ok(None),
            Err(error) => {
                return Err(ActionError::retryable(format!("LXD on {}: {error}", endpoint.address)));
            }
        };
        let Some((server, is_clustered)) = server(&root) else {
            return Ok(Some(LxdAccess::Untrusted));
        };
        let listing = connection
            .get("/1.0/instances?recursion=2&all-projects=true")
            .await
            .map_err(ActionError::retryable)?;
        let hosted = hosted_by(instances(&listing), &server, is_clustered);
        Ok(Some(LxdAccess::Trusted { server, is_clustered, instances: hosted }))
    }

    async fn members(&self, endpoint: &LxdEndpoint) -> Result<Vec<String>, ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let cluster = connection.get("/1.0/cluster").await.map_err(ActionError::retryable)?;
        if cluster["metadata"]["enabled"].as_bool() != Some(true) {
            return Ok(Vec::new());
        }
        let listing =
            connection.get("/1.0/cluster/members?recursion=1").await.map_err(ActionError::retryable)?;
        Ok(listing["metadata"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|member| member["server_name"].as_str().map(str::to_owned))
            .collect())
    }

    async fn enable_clustering(&self, endpoint: &LxdEndpoint, server_name: &str) -> Result<(), ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let body = serde_json::json!({"server_name": server_name, "enabled": true});
        let started = connection.send(reqwest::Method::PUT, "/1.0/cluster", &body).await?;
        connection.finish(&started).await
    }

    async fn invite(&self, endpoint: &LxdEndpoint, member: &str) -> Result<String, ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let body = serde_json::json!({"server_name": member});
        let started = connection.send(reqwest::Method::POST, "/1.0/cluster/members", &body).await?;
        join_token(&started["metadata"]["metadata"])
    }

    async fn remove_member(&self, endpoint: &LxdEndpoint, member: &str) -> Result<(), ActionError> {
        let connection = Connection::open(endpoint, self.timeout)?;
        let path = format!("/1.0/cluster/members/{member}");
        let started = connection.send(reqwest::Method::DELETE, &path, &Value::Null).await?;
        connection.finish(&started).await
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
    fn a_join_token_is_the_base64_of_what_lxd_reports() {
        let metadata = json!({
            "serverName": "pod-1",
            "fingerprint": "abc",
            "addresses": ["10.70.0.2:8443"],
            "secret": "s3cret",
            "expiresAt": "2026-11-24T00:00:00Z",
        });
        let decoded = STANDARD.decode(join_token(&metadata).unwrap()).unwrap();
        let token: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(token["server_name"], "pod-1");
        assert_eq!(token["addresses"], json!(["10.70.0.2:8443"]));
        assert!(join_token(&json!({"serverName": "pod-1"})).is_err());
    }

    #[test]
    fn a_cluster_member_keeps_only_the_instances_it_runs() {
        let instance = |name: &str, location: &str| GuestInstance {
            addresses: Vec::new(),
            is_juju_controller: false,
            name: String::from(name),
            kind: GuestInstanceKind::Container,
            status: String::from("Running"),
            location: Some(String::from(location)),
        };
        let listed = vec![instance("c0", "pod-0"), instance("c1", "pod-1")];
        assert_eq!(hosted_by(listed.clone(), "pod-0", true), [instance("c0", "pod-0")]);
        assert_eq!(hosted_by(listed, "pod-0", false).len(), 2);
    }

    #[test]
    fn instances_keep_their_kind_status_member_and_global_addresses() {
        let listing = json!({"metadata": [
            {"name": "juju-2a3b-1", "type": "virtual-machine", "status": "Running", "location": "pod-1",
             "config": {"user.juju-is-controller": "true"},
             "state": {"network": {
                "eth0": {"addresses": [
                    {"family": "inet", "address": "10.70.0.4", "scope": "global"},
                    {"family": "inet6", "address": "fe80::1", "scope": "link"},
                ]},
                "lo": {"addresses": [{"family": "inet", "address": "127.0.0.1", "scope": "local"}]},
             }}},
            {"name": "juju-2a3b-0", "type": "container", "status": "Stopped", "location": "", "state": null},
        ]});
        assert_eq!(
            instances(&listing),
            [
                GuestInstance {
                    name: String::from("juju-2a3b-0"),
                    kind: GuestInstanceKind::Container,
                    status: String::from("Stopped"),
                    location: None,
                    addresses: Vec::new(),
                    is_juju_controller: false,
                },
                GuestInstance {
                    name: String::from("juju-2a3b-1"),
                    kind: GuestInstanceKind::VirtualMachine,
                    status: String::from("Running"),
                    location: Some(String::from("pod-1")),
                    addresses: vec![Ipv4Addr::new(10, 70, 0, 4)],
                    is_juju_controller: true,
                },
            ]
        );
    }
}
