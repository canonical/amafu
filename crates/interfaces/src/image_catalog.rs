use amafu_domain::kinds::image::ImageSource;
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogKind {
    VirtualMachine,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub title: String,
    pub distribution: String,
    pub kind: CatalogKind,
    pub source: ImageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default)]
    pub is_long_term: bool,
}

#[async_trait::async_trait]
pub trait ImageCatalog: Send + Sync {
    async fn entries(&self) -> Result<Vec<CatalogEntry>, ActionError>;
}

#[derive(Debug, Clone, Default)]
pub struct FixedImageCatalog {
    entries: Vec<CatalogEntry>,
}

impl FixedImageCatalog {
    #[must_use]
    pub fn new(entries: Vec<CatalogEntry>) -> Self {
        Self { entries }
    }
}

#[async_trait::async_trait]
impl ImageCatalog for FixedImageCatalog {
    async fn entries(&self) -> Result<Vec<CatalogEntry>, ActionError> {
        Ok(self.entries.clone())
    }
}
