use std::collections::BTreeMap;
use std::path::PathBuf;

use amafu_domain::ObjectId;
use amafu_domain::kinds::image::ImageSource;
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRequest {
    pub image_id: ObjectId,
    pub source: ImageSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContainerConfiguration {
    #[serde(default)]
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedImage {
    pub path: PathBuf,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerConfiguration>,
}

#[async_trait::async_trait]
pub trait ImageStore: Send + Sync {
    async fn ensure(&self, request: &ImageRequest) -> Result<CachedImage, ActionError>;

    async fn cached(&self, image_id: &ObjectId) -> Result<Option<CachedImage>, ActionError>;

    async fn remove(&self, image_id: &ObjectId) -> Result<(), ActionError>;
}
