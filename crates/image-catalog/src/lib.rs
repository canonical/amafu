mod registry;
mod streams;

use std::sync::Arc;
use std::time::{Duration, Instant};

use amafu_interfaces::{ActionError, CatalogEntry, CatalogKind, ImageCatalog};
use tokio::sync::Mutex;

pub use streams::object_name;

const FRESHNESS: Duration = Duration::from_secs(3600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct CanonicalCatalog {
    client: reqwest::Client,
    architecture: String,
    cached: Mutex<Option<(Instant, Vec<CatalogEntry>)>>,
}

fn https_client() -> Result<reqwest::Client, ActionError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .https_only(true)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(ActionError::fatal)
}

#[must_use]
pub fn host_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "riscv64" => "riscv64",
        _ => "amd64",
    }
}

fn ordered(mut entries: Vec<CatalogEntry>) -> Vec<CatalogEntry> {
    entries.sort_by(|left, right| {
        (left.kind == CatalogKind::Container, left.distribution != "ubuntu", !left.is_long_term)
            .cmp(&(right.kind == CatalogKind::Container, right.distribution != "ubuntu", !right.is_long_term))
            .then_with(|| right.id.cmp(&left.id))
    });
    entries
}

impl CanonicalCatalog {
    pub fn new(architecture: &str) -> Result<Self, ActionError> {
        Ok(Self { client: https_client()?, architecture: architecture.to_owned(), cached: Mutex::new(None) })
    }

    async fn stream(&self, url: &str) -> Result<streams::Stream, String> {
        let response = self.client.get(url).send().await.map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("{url} answered {}", response.status()));
        }
        let body = response.bytes().await.map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || serde_json::from_slice::<streams::Stream>(&body))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{url}: {error}"))
    }

    async fn containers(&self) -> Vec<CatalogEntry> {
        let mut entries = Vec::new();
        for offered in registry::OFFERED {
            match registry::digest_of(&self.client, offered.repository, offered.tag).await {
                Ok(digest) => entries.extend(registry::container_entry(offered, &digest)),
                Err(error) => {
                    tracing::warn!(image = offered.id, %error, "a catalog container image could not be resolved");
                }
            }
        }
        entries
    }

    async fn fetch(&self) -> Result<Vec<CatalogEntry>, ActionError> {
        let (ubuntu, lxd) =
            tokio::join!(self.stream(streams::UBUNTU_STREAM), self.stream(streams::LXD_STREAM));
        let mut entries = Vec::new();
        let mut failures = Vec::new();
        match ubuntu {
            Ok(stream) => entries.extend(streams::ubuntu_entries(&stream, &self.architecture)),
            Err(error) => failures.push(error),
        }
        match lxd {
            Ok(stream) => entries.extend(streams::lxd_entries(&stream, &self.architecture)),
            Err(error) => failures.push(error),
        }
        entries.extend(self.containers().await);
        if entries.is_empty() {
            return Err(ActionError::retryable(format!("no image server answered: {}", failures.join("; "))));
        }
        Ok(ordered(entries))
    }
}

#[async_trait::async_trait]
impl ImageCatalog for CanonicalCatalog {
    async fn entries(&self) -> Result<Vec<CatalogEntry>, ActionError> {
        let mut cached = self.cached.lock().await;
        if let Some((fetched_at, entries)) = cached.as_ref()
            && fetched_at.elapsed() < FRESHNESS
        {
            return Ok(entries.clone());
        }
        let entries = self.fetch().await?;
        *cached = Some((Instant::now(), entries.clone()));
        Ok(entries)
    }
}
