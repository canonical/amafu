use std::path::PathBuf;

use amafu_domain::kinds::storage_backend::PluginModule;
use amafu_interfaces::ActionError;
use futures::StreamExt;
use sha2::{Digest, Sha256};

pub const LARGEST_MODULE_BYTES: u64 = 64 * 1024 * 1024;

#[async_trait::async_trait]
pub trait PluginModules: Send + Sync {
    async fn module(&self, module: &PluginModule) -> Result<Vec<u8>, ActionError>;
}

pub struct DownloadedModules {
    cache: PathBuf,
    http: reqwest::Client,
}

impl DownloadedModules {
    pub fn new(cache: PathBuf) -> Result<Self, ActionError> {
        Ok(Self { cache, http: crate::management::management_client(std::time::Duration::from_secs(60))? })
    }

    fn cached(&self, module: &PluginModule) -> PathBuf {
        self.cache.join(format!("{}.wasm", module.digest.hexadecimal()))
    }
}

fn matches(bytes: &[u8], module: &PluginModule) -> bool {
    hex::encode(Sha256::digest(bytes)) == module.digest.hexadecimal()
}

#[async_trait::async_trait]
impl PluginModules for DownloadedModules {
    async fn module(&self, module: &PluginModule) -> Result<Vec<u8>, ActionError> {
        let path = self.cached(module);
        if let Ok(bytes) = tokio::fs::read(&path).await
            && matches(&bytes, module)
        {
            return Ok(bytes);
        }
        let response = self.http.get(&module.url).send().await.map_err(|error| {
            ActionError::retryable(format!("fetching the plugin {}: {error}", module.url))
        })?;
        if !response.status().is_success() {
            return Err(ActionError::retryable(format!("{} answered {}", module.url, response.status())));
        }
        let mut bytes = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk =
                chunk.map_err(|error| ActionError::retryable(format!("fetching the plugin: {error}")))?;
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > LARGEST_MODULE_BYTES {
                return Err(ActionError::invalid(format!("the plugin {} is larger than 64 MiB", module.url)));
            }
        }
        if !matches(&bytes, module) {
            return Err(ActionError::invalid(format!(
                "the plugin {} does not match {}",
                module.url, module.digest
            )));
        }
        tokio::fs::create_dir_all(&self.cache)
            .await
            .map_err(|error| ActionError::retryable(format!("keeping the plugin: {error}")))?;
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|error| ActionError::retryable(format!("keeping the plugin: {error}")))?;
        Ok(bytes)
    }
}
