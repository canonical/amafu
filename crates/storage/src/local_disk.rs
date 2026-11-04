use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use amafu_domain::NodeName;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_interfaces::{
    ActionError, AttachTarget, FencedNode, StorageProtocol, StorageProvider, VolumeAttacher, VolumeRequest,
};

use crate::qcow2::{self, Backing, BackingFormat};

pub const VOLUME_EXTENSION: &str = "qcow2";

fn io_failure(action: &'static str) -> impl Fn(std::io::Error) -> ActionError {
    move |error| ActionError::retryable(format!("{action}: {error}"))
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_hexdigit() || character == '-')
}

pub fn create_volume_file(
    directory: &Path,
    request: &VolumeRequest,
) -> Result<ProvisionedVolume, ActionError> {
    std::fs::create_dir_all(directory).map_err(io_failure("creating the volume directory"))?;
    let identifier = request.volume_id.to_string();
    let path = directory.join(format!("{identifier}.{VOLUME_EXTENSION}"));
    if !path.exists() {
        let backing = match &request.source_image {
            Some(source) => {
                let format = BackingFormat::detect(source).map_err(io_failure("reading the source image"))?;
                Some((source.clone(), format))
            }
            None => None,
        };
        let backing = backing.as_ref().map(|(path, format)| Backing { path, format: *format });
        qcow2::create(&path, request.size_bytes, backing).map_err(io_failure("writing the volume"))?;
    }
    Ok(ProvisionedVolume { identifier, size_bytes: request.size_bytes, node: None })
}

pub fn delete_volume_file(directory: &Path, volume: &ProvisionedVolume) -> Result<(), ActionError> {
    if !is_identifier(&volume.identifier) {
        return Err(ActionError::fatal(format!("{:?} is not a volume identifier", volume.identifier)));
    }
    match std::fs::remove_file(directory.join(format!("{}.{VOLUME_EXTENSION}", volume.identifier))) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(io_failure("deleting the volume")(error))
        }
        _ => Ok(()),
    }
}

pub struct LocalDiskProvider {
    node: NodeName,
    directory: PathBuf,
}

impl LocalDiskProvider {
    #[must_use]
    pub fn new(node: NodeName, directory: PathBuf) -> Self {
        Self { node, directory }
    }
}

#[async_trait::async_trait]
impl StorageProvider for LocalDiskProvider {
    async fn check(&self) -> Result<(), ActionError> {
        std::fs::create_dir_all(&self.directory).map_err(io_failure("creating the volume directory"))
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        let provisioned = create_volume_file(&self.directory, request)?;
        Ok(ProvisionedVolume { node: Some(self.node.clone()), ..provisioned })
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        delete_volume_file(&self.directory, volume)
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        if *node != self.node || volume.node.as_ref().is_some_and(|holder| *holder != self.node) {
            return Err(ActionError::invalid(format!(
                "a local disk volume lives on {} and cannot reach {node}",
                volume.node.as_ref().unwrap_or(&self.node)
            )));
        }
        Ok(AttachTarget::LocalFile {
            node: node.clone(),
            path: self.directory.join(format!("{}.{VOLUME_EXTENSION}", volume.identifier)),
        })
    }

    async fn revoke_node(&self, _node: &FencedNode) -> Result<(), ActionError> {
        Ok(())
    }

    async fn has_revoked_node(&self, _node: &FencedNode) -> Result<bool, ActionError> {
        Ok(true)
    }

    async fn readmit_node(&self, _node: &FencedNode) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct LocalFileAttacher {
    node: NodeName,
    attached: Mutex<BTreeMap<AttachTarget, PathBuf>>,
}

impl LocalFileAttacher {
    #[must_use]
    pub fn new(node: NodeName) -> Self {
        Self { node, attached: Mutex::new(BTreeMap::new()) }
    }
}

#[async_trait::async_trait]
impl VolumeAttacher for LocalFileAttacher {
    fn protocol(&self) -> StorageProtocol {
        StorageProtocol::LocalFile
    }

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError> {
        let AttachTarget::LocalFile { node, path } = target else {
            return Err(ActionError::invalid("not a local file target"));
        };
        if *node != self.node {
            return Err(ActionError::invalid(format!("the volume is local to {node}, not {}", self.node)));
        }
        if !path.is_file() {
            return Err(ActionError::retryable(format!("{} does not exist yet", path.display())));
        }
        self.attached.lock().unwrap_or_else(PoisonError::into_inner).insert(target.clone(), path.clone());
        Ok(path.clone())
    }

    async fn detach(&self, target: &AttachTarget) -> Result<(), ActionError> {
        self.attached.lock().unwrap_or_else(PoisonError::into_inner).remove(target);
        Ok(())
    }

    async fn attached(&self) -> Result<Vec<(AttachTarget, PathBuf)>, ActionError> {
        let attached = self.attached.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(attached.iter().map(|(target, path)| (target.clone(), path.clone())).collect())
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::contract::storage::storage_contract;

    use super::*;

    #[tokio::test]
    async fn local_disks_honour_the_storage_contract() {
        let directory = tempfile::tempdir().unwrap();
        let node = NodeName::new("node-0").unwrap();
        let provider = LocalDiskProvider::new(node.clone(), directory.path().to_path_buf());
        let attacher = LocalFileAttacher::new(node.clone());
        assert_eq!(storage_contract(&provider, &attacher, &node).await, Ok(()));
    }

    #[tokio::test]
    async fn a_created_volume_records_its_node() {
        let directory = tempfile::tempdir().unwrap();
        let node = NodeName::new("node-0").unwrap();
        let provider = LocalDiskProvider::new(node.clone(), directory.path().to_path_buf());
        let request = VolumeRequest {
            volume_id: amafu_domain::ObjectId::from_uuid(uuid::Uuid::new_v4()),
            size_bytes: 1 << 20,
            source_image: None,
        };
        let created = provider.create_volume(&request).await.unwrap();
        assert_eq!(created.node, Some(node));
        let elsewhere = ProvisionedVolume { node: Some(NodeName::new("node-1").unwrap()), ..created };
        assert!(provider.export_to_node(&elsewhere, &NodeName::new("node-0").unwrap()).await.is_err());
    }

    #[tokio::test]
    async fn a_volume_cannot_be_exported_to_another_node() {
        let directory = tempfile::tempdir().unwrap();
        let provider =
            LocalDiskProvider::new(NodeName::new("node-0").unwrap(), directory.path().to_path_buf());
        let volume = ProvisionedVolume { identifier: String::from("abc"), size_bytes: 1, node: None };
        assert!(provider.export_to_node(&volume, &NodeName::new("node-1").unwrap()).await.is_err());
    }

    #[test]
    fn deleting_refuses_anything_but_an_identifier() {
        let directory = tempfile::tempdir().unwrap();
        let sneaky =
            ProvisionedVolume { identifier: String::from("../../etc/passwd"), size_bytes: 1, node: None };
        assert!(matches!(delete_volume_file(directory.path(), &sneaky), Err(ActionError::Fatal(_))));
    }
}
