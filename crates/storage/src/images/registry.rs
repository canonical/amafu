use std::collections::BTreeMap;

use amafu_domain::{OciReference, Sha256Digest};
use amafu_interfaces::{ActionError, ContainerConfiguration};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
const MAXIMUM_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const PLATFORM_OS: &str = "linux";
const PLATFORM_ARCHITECTURE: &str = "amd64";

#[derive(Debug, Deserialize)]
pub struct Descriptor {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct Platform {
    os: String,
    architecture: String,
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    manifests: Vec<Descriptor>,
    #[serde(default)]
    config: Option<Descriptor>,
    #[serde(default)]
    layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize, Default)]
struct ImageConfiguration {
    #[serde(default)]
    config: RuntimeConfiguration,
}

#[derive(Debug, Deserialize, Default)]
struct RuntimeConfiguration {
    #[serde(rename = "Entrypoint", default)]
    entrypoint: Option<Vec<String>>,
    #[serde(rename = "Cmd", default)]
    command: Option<Vec<String>>,
    #[serde(rename = "Env", default)]
    environment: Option<Vec<String>>,
    #[serde(rename = "WorkingDir", default)]
    working_directory: Option<String>,
    #[serde(rename = "User", default)]
    user: Option<String>,
}

pub struct Manifest {
    pub configuration: ContainerConfiguration,
    pub layers: Vec<Descriptor>,
}

fn registry_host(registry: &str) -> &str {
    if registry == "docker.io" { "registry-1.docker.io" } else { registry }
}

fn failure(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("the registry: {error}"))
}

fn parameter<'header>(challenge: &'header str, name: &str) -> Option<&'header str> {
    challenge.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key.trim().trim_start_matches("Bearer ").trim() == name).then(|| value.trim().trim_matches('"'))
    })
}

#[must_use]
pub fn verify_digest(bytes: &[u8], expected: &Sha256Digest) -> bool {
    hex::encode(Sha256::digest(bytes)) == expected.hexadecimal()
}

fn configuration_from(document: ImageConfiguration) -> ContainerConfiguration {
    let runtime = document.config;
    let environment: BTreeMap<String, String> = runtime
        .environment
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pair| pair.split_once('=').map(|(name, value)| (name.to_owned(), value.to_owned())))
        .collect();
    ContainerConfiguration {
        entrypoint: runtime.entrypoint.unwrap_or_default(),
        command: runtime.command.unwrap_or_default(),
        environment,
        working_directory: runtime.working_directory.filter(|directory| !directory.is_empty()),
        user: runtime.user.filter(|user| !user.is_empty()),
    }
}

pub struct RegistryClient {
    http: reqwest::Client,
    reference: OciReference,
    token: Option<String>,
}

impl RegistryClient {
    #[must_use]
    pub fn new(http: reqwest::Client, reference: OciReference) -> Self {
        Self { http, reference, token: None }
    }

    fn url(&self, kind: &str, digest: &str) -> String {
        format!(
            "https://{}/v2/{}/{kind}/{digest}",
            registry_host(&self.reference.registry),
            self.reference.repository
        )
    }

    async fn authenticate(&mut self, challenge: &str) -> Result<(), ActionError> {
        let realm = parameter(challenge, "realm")
            .ok_or_else(|| failure("the authentication challenge has no realm"))?;
        let mut request = self.http.get(realm);
        let mut query = Vec::new();
        if let Some(service) = parameter(challenge, "service") {
            query.push(("service", service.to_owned()));
        }
        query.push(("scope", format!("repository:{}:pull", self.reference.repository)));
        request = request.query(&query);
        let body: serde_json::Value = request
            .send()
            .await
            .map_err(failure)?
            .error_for_status()
            .map_err(failure)?
            .json()
            .await
            .map_err(failure)?;
        let token = body["token"]
            .as_str()
            .or_else(|| body["access_token"].as_str())
            .ok_or_else(|| failure("no token was issued"))?;
        self.token = Some(token.to_owned());
        Ok(())
    }

    pub async fn get(
        &mut self,
        kind: &str,
        digest: &str,
        accept: Option<&str>,
    ) -> Result<reqwest::Response, ActionError> {
        for _ in 0..2 {
            let mut request = self.http.get(self.url(kind, digest));
            if let Some(accept) = accept {
                request = request.header(ACCEPT, accept);
            }
            if let Some(token) = &self.token {
                request = request.header(AUTHORIZATION, format!("Bearer {token}"));
            }
            let response = request.send().await.map_err(failure)?;
            if response.status() == StatusCode::UNAUTHORIZED && self.token.is_none() {
                let challenge = response
                    .headers()
                    .get(WWW_AUTHENTICATE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                self.authenticate(&challenge).await?;
                continue;
            }
            return match response.status() {
                StatusCode::NOT_FOUND => {
                    Err(ActionError::invalid(format!("{} has no {kind} {digest}", self.reference)))
                }
                _ => response.error_for_status().map_err(failure),
            };
        }
        Err(failure("the registry refused the credentials it issued"))
    }

    async fn document(
        &mut self,
        kind: &str,
        digest: &Sha256Digest,
        accept: Option<&str>,
    ) -> Result<Vec<u8>, ActionError> {
        let response = self.get(kind, &digest.to_string(), accept).await?;
        let bytes = response.bytes().await.map_err(failure)?;
        if bytes.len() > MAXIMUM_MANIFEST_BYTES {
            return Err(ActionError::invalid("the document is too large"));
        }
        if !verify_digest(&bytes, digest) {
            return Err(ActionError::invalid(format!("the {kind} {digest} does not match its digest")));
        }
        Ok(bytes.to_vec())
    }

    pub async fn manifest(&mut self) -> Result<Manifest, ActionError> {
        let top_digest = self.reference.digest.clone();
        let parse = |bytes: &[u8]| {
            serde_json::from_slice::<Document>(bytes)
                .map_err(|error| ActionError::invalid(format!("the manifest is malformed: {error}")))
        };
        let mut document = parse(&self.document("manifests", &top_digest, Some(MANIFEST_TYPES)).await?)?;
        if !document.manifests.is_empty() {
            let chosen = document
                .manifests
                .iter()
                .find(|descriptor| {
                    descriptor.platform.as_ref().is_some_and(|platform| {
                        platform.os == PLATFORM_OS && platform.architecture == PLATFORM_ARCHITECTURE
                    })
                })
                .ok_or_else(|| ActionError::invalid("the image has no linux/amd64 variant"))?;
            let digest: Sha256Digest = chosen.digest.parse().map_err(ActionError::invalid)?;
            document = parse(&self.document("manifests", &digest, Some(MANIFEST_TYPES)).await?)?;
        }
        let descriptor =
            document.config.ok_or_else(|| ActionError::invalid("the manifest has no configuration"))?;
        let config_digest: Sha256Digest = descriptor.digest.parse().map_err(ActionError::invalid)?;
        let config_bytes = self.document("blobs", &config_digest, None).await?;
        let configuration = serde_json::from_slice::<ImageConfiguration>(&config_bytes).map_err(|error| {
            ActionError::invalid(format!("the image configuration is malformed: {error}"))
        })?;
        Ok(Manifest { configuration: configuration_from(configuration), layers: document.layers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_challenges_are_parsed() {
        let challenge = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/busybox:pull""#;
        assert_eq!(parameter(challenge, "realm"), Some("https://auth.docker.io/token"));
        assert_eq!(parameter(challenge, "service"), Some("registry.docker.io"));
        assert_eq!(parameter(challenge, "missing"), None);
    }

    #[test]
    fn the_image_configuration_is_extracted() {
        let document: ImageConfiguration = serde_json::from_str(
            r#"{"config":{"Entrypoint":null,"Cmd":["sh"],"Env":["PATH=/usr/bin:/bin","EMPTY="],"WorkingDir":"","User":"nobody"}}"#,
        )
        .unwrap();
        let configuration = configuration_from(document);
        assert_eq!(configuration.command, vec!["sh"]);
        assert_eq!(configuration.environment["PATH"], "/usr/bin:/bin");
        assert_eq!(configuration.environment["EMPTY"], "");
        assert_eq!(configuration.working_directory, None);
        assert_eq!(configuration.user.as_deref(), Some("nobody"));
    }

    #[test]
    fn digests_are_checked() {
        let digest: Sha256Digest = hex::encode(Sha256::digest(b"manifest")).parse().unwrap();
        assert!(verify_digest(b"manifest", &digest));
        assert!(!verify_digest(b"tampered", &digest));
    }
}
