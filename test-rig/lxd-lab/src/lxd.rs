use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::server::{crypto_provider, parse_certificates};
use anyhow::{Context, bail};
use reqwest::Method;
use rustls::pki_types::PrivateKeyDer;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pinning::PinnedCertificate;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct LxdCredentials {
    pub endpoint: String,
    pub client_certificate: Vec<u8>,
    pub client_key: Vec<u8>,
    pub server_certificate: Vec<u8>,
}

impl LxdCredentials {
    pub fn from_snap_configuration(endpoint: &str, remote: &str) -> anyhow::Result<Self> {
        let home = std::env::var("HOME").context("HOME is not set")?;
        let configuration = Path::new(&home).join("snap/lxd/common/config");
        let read = |path: &Path| std::fs::read(path).with_context(|| format!("reading {}", path.display()));
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            client_certificate: read(&configuration.join("client.crt"))?,
            client_key: read(&configuration.join("client.key"))?,
            server_certificate: read(&configuration.join("servercerts").join(format!("{remote}.crt")))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i64,
    pub standard_output: String,
    pub standard_error: String,
}

impl CommandOutput {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Debug, Deserialize)]
struct LxdResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    metadata: Value,
    #[serde(default)]
    operation: String,
}

#[derive(Clone)]
pub struct LxdClient {
    http: reqwest::Client,
    endpoint: String,
    project: String,
}

impl LxdClient {
    pub fn new(credentials: &LxdCredentials, project: &str) -> anyhow::Result<Self> {
        let provider = crypto_provider();
        let pinned = parse_certificates(&credentials.server_certificate)?
            .into_iter()
            .next()
            .context("the LXD server certificate is empty")?;
        let client_certificates = parse_certificates(&credentials.client_certificate)?;
        let client_key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut credentials.client_key.as_slice())?
                .context("the LXD client key file holds no key")?;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedCertificate::new(pinned, provider)))
            .with_client_auth_cert(client_certificates, client_key)?;
        let http = reqwest::Client::builder().use_preconfigured_tls(tls).build()?;
        Ok(Self { http, endpoint: credentials.endpoint.clone(), project: project.to_owned() })
    }

    #[must_use]
    pub fn in_project(&self, project: &str) -> Self {
        Self { project: project.to_owned(), ..self.clone() }
    }

    fn url(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{}{path}{separator}project={}", self.endpoint, self.project)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> anyhow::Result<LxdResponse> {
        let response: LxdResponse = request.send().await?.json().await?;
        if response.kind == "error" {
            bail!("LXD refused the request: {}", response.error);
        }
        if response.kind == "async" {
            return self.wait(&response.operation).await;
        }
        Ok(response)
    }

    async fn wait(&self, operation: &str) -> anyhow::Result<LxdResponse> {
        let timeout = OPERATION_TIMEOUT.as_secs();
        let url = format!("{}{operation}/wait?timeout={timeout}", self.endpoint);
        let response: LxdResponse = self.http.get(url).send().await?.json().await?;
        let status = response.metadata["status"].as_str().unwrap_or_default();
        if response.kind == "error" || status != "Success" {
            let error = response.metadata["err"].as_str().unwrap_or(&response.error);
            bail!("the LXD operation failed: {error}");
        }
        Ok(response)
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let mut request = self.http.request(method, self.url(path));
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(self.send(request).await?.metadata)
    }

    pub async fn exists(&self, path: &str) -> anyhow::Result<bool> {
        let response = self.http.get(self.url(path)).send().await?;
        Ok(response.status().is_success())
    }

    pub async fn upload_image(&self, unified_tarball: &Path) -> anyhow::Result<String> {
        let body = tokio::fs::read(unified_tarball).await?;
        let request = self
            .http
            .post(self.url("/1.0/images"))
            .header("Content-Type", "application/octet-stream")
            .header("X-LXD-filename", "amafu-node.tar")
            .body(body);
        let response = self.send(request).await?;
        response.metadata["metadata"]["fingerprint"]
            .as_str()
            .map(str::to_owned)
            .context("LXD did not return the image fingerprint")
    }

    pub async fn change_state(&self, instance: &str, action: &str, is_forced: bool) -> anyhow::Result<()> {
        let body = json!({ "action": action, "force": is_forced, "timeout": 30 });
        self.request(Method::PUT, &format!("/1.0/instances/{instance}/state"), Some(body)).await?;
        Ok(())
    }

    pub async fn execute(&self, instance: &str, script: &str) -> anyhow::Result<CommandOutput> {
        let body = json!({
            "command": ["sh", "-c", script],
            "record-output": true,
            "wait-for-websocket": false,
            "interactive": false,
        });
        let request = self.http.post(self.url(&format!("/1.0/instances/{instance}/exec"))).json(&body);
        let response = self.send(request).await?;
        let exit_code = response.metadata["metadata"]["return"].as_i64().unwrap_or(-1);
        let read_log = async |stream: &str| -> anyhow::Result<String> {
            match response.metadata["metadata"]["output"][stream].as_str() {
                Some(path) => {
                    Ok(self.http.get(format!("{}{path}", self.endpoint)).send().await?.text().await?)
                }
                None => Ok(String::new()),
            }
        };
        Ok(CommandOutput {
            exit_code,
            standard_output: read_log("1").await?,
            standard_error: read_log("2").await?,
        })
    }

    pub async fn console_log(&self, instance: &str) -> anyhow::Result<String> {
        let response = self.http.get(self.url(&format!("/1.0/instances/{instance}/console"))).send().await?;
        Ok(response.text().await?)
    }
}
