use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;

use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_domain::{NodeName, ObjectId};
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRequest {
    pub volume_id: ObjectId,
    pub size_bytes: u64,
    pub source_image: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageProtocol {
    LocalFile,
    Nfs,
    NvmeTcp,
    Rbd,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AttachTarget {
    LocalFile { node: NodeName, path: PathBuf },
    NfsFile { server: String, export: String, file: String },
    NvmeTcp { qualified_name: String, addresses: Vec<SocketAddrV4> },
    Rbd { monitors: Vec<SocketAddrV4>, pool: String, image: String, user: String, key: String },
}

impl AttachTarget {
    #[must_use]
    pub fn protocol(&self) -> StorageProtocol {
        match self {
            Self::LocalFile { .. } => StorageProtocol::LocalFile,
            Self::NfsFile { .. } => StorageProtocol::Nfs,
            Self::NvmeTcp { .. } => StorageProtocol::NvmeTcp,
            Self::Rbd { .. } => StorageProtocol::Rbd,
        }
    }
}

#[async_trait::async_trait]
pub trait StorageProvider: Send + Sync {
    async fn check(&self) -> Result<(), ActionError>;

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError>;

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError>;

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        node: &NodeName,
    ) -> Result<AttachTarget, ActionError>;

    async fn revoke_node(&self, node: &NodeName) -> Result<(), ActionError>;
}

#[async_trait::async_trait]
pub trait VolumeAttacher: Send + Sync {
    fn protocol(&self) -> StorageProtocol;

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError>;

    async fn detach(&self, target: &AttachTarget) -> Result<(), ActionError>;

    async fn attached(&self) -> Result<Vec<(AttachTarget, PathBuf)>, ActionError>;
}

pub trait Storage: Send + Sync {
    fn provider(
        &self,
        backend: &StorageProviderSpecification,
    ) -> Result<Arc<dyn StorageProvider>, ActionError>;

    fn attacher(&self, protocol: StorageProtocol) -> Result<Arc<dyn VolumeAttacher>, ActionError>;
}
