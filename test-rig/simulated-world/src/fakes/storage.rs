use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_domain::{NodeName, ObjectId};
use amafu_interfaces::{
    ActionError, AttachTarget, Storage, StorageProtocol, StorageProvider, VolumeAttacher, VolumeRequest,
};

use crate::lock::lock;

#[derive(Default)]
pub struct FakeBackends {
    volumes: Mutex<BTreeMap<String, BTreeSet<ObjectId>>>,
    unreachable: Mutex<BTreeSet<String>>,
}

impl FakeBackends {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_reachable(&self, backend: &str, is_reachable: bool) {
        let mut unreachable = lock(&self.unreachable);
        if is_reachable {
            unreachable.remove(backend);
        } else {
            unreachable.insert(backend.to_owned());
        }
    }

    #[must_use]
    pub fn volume_count(&self) -> usize {
        lock(&self.volumes).values().map(BTreeSet::len).sum()
    }
}

fn backend_key(specification: &StorageProviderSpecification, node: &NodeName) -> String {
    match specification {
        StorageProviderSpecification::LocalDisk => format!("local-disk@{node}"),
        StorageProviderSpecification::Nfs { server, export } => format!("nfs://{server}{export}"),
    }
}

struct FakeProvider {
    key: String,
    node: NodeName,
    specification: StorageProviderSpecification,
    backends: Arc<FakeBackends>,
}

#[async_trait::async_trait]
impl StorageProvider for FakeProvider {
    async fn check(&self) -> Result<(), ActionError> {
        if lock(&self.backends.unreachable).contains(&self.key) {
            return Err(ActionError::retryable(format!("{} does not answer", self.key)));
        }
        Ok(())
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        self.check().await?;
        lock(&self.backends.volumes).entry(self.key.clone()).or_default().insert(request.volume_id);
        let is_local = self.specification == StorageProviderSpecification::LocalDisk;
        Ok(ProvisionedVolume {
            identifier: request.volume_id.to_string(),
            size_bytes: request.size_bytes,
            node: is_local.then(|| self.node.clone()),
        })
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        self.check().await?;
        if let Ok(volume_id) = volume.identifier.parse::<ObjectId>()
            && let Some(volumes) = lock(&self.backends.volumes).get_mut(&self.key)
        {
            volumes.remove(&volume_id);
        }
        Ok(())
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        let is_elsewhere = volume.node.as_ref().is_some_and(|holder| holder != node) || *node != self.node;
        if self.specification == StorageProviderSpecification::LocalDisk && is_elsewhere {
            return Err(ActionError::invalid(format!("a local disk volume cannot reach {node}")));
        }
        Ok(match &self.specification {
            StorageProviderSpecification::LocalDisk => AttachTarget::LocalFile {
                node: node.clone(),
                path: PathBuf::from(format!("/var/lib/amafu/volumes/{}.qcow2", volume.identifier)),
            },
            StorageProviderSpecification::Nfs { server, export } => AttachTarget::NfsFile {
                server: server.clone(),
                export: export.clone(),
                file: format!("{}.qcow2", volume.identifier),
            },
        })
    }

    async fn revoke_node(&self, _node: &NodeName) -> Result<(), ActionError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeAttachments {
    attached: Mutex<BTreeMap<AttachTarget, PathBuf>>,
}

impl FakeAttachments {
    #[must_use]
    pub fn attached(&self) -> Vec<(AttachTarget, PathBuf)> {
        lock(&self.attached).iter().map(|(target, path)| (target.clone(), path.clone())).collect()
    }
}

struct FakeAttacher {
    protocol: StorageProtocol,
    attachments: Arc<FakeAttachments>,
}

#[async_trait::async_trait]
impl VolumeAttacher for FakeAttacher {
    fn protocol(&self) -> StorageProtocol {
        self.protocol
    }

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError> {
        if target.protocol() != self.protocol {
            return Err(ActionError::invalid("the target does not suit this attacher"));
        }
        let path = match target {
            AttachTarget::LocalFile { path, .. } => path.clone(),
            AttachTarget::NfsFile { server, file, .. } => {
                PathBuf::from(format!("/run/amafu/nfs/{server}/{file}"))
            }
        };
        lock(&self.attachments.attached).entry(target.clone()).or_insert(path.clone());
        Ok(path)
    }

    async fn detach(&self, target: &AttachTarget) -> Result<(), ActionError> {
        lock(&self.attachments.attached).remove(target);
        Ok(())
    }

    async fn attached(&self) -> Result<Vec<(AttachTarget, PathBuf)>, ActionError> {
        Ok(self
            .attachments
            .attached()
            .into_iter()
            .filter(|(target, _)| target.protocol() == self.protocol)
            .collect())
    }
}

pub struct FakeStorage {
    node: NodeName,
    backends: Arc<FakeBackends>,
    attachments: Arc<FakeAttachments>,
}

impl FakeStorage {
    #[must_use]
    pub fn new(node: NodeName, backends: Arc<FakeBackends>) -> Self {
        Self { node, backends, attachments: Arc::new(FakeAttachments::default()) }
    }

    #[must_use]
    pub fn attachments(&self) -> Vec<(AttachTarget, PathBuf)> {
        self.attachments.attached()
    }

    pub fn power_off(&self) {
        lock(&self.attachments.attached).clear();
    }
}

impl Storage for FakeStorage {
    fn provider(
        &self,
        backend: &StorageProviderSpecification,
    ) -> Result<Arc<dyn StorageProvider>, ActionError> {
        Ok(Arc::new(FakeProvider {
            key: backend_key(backend, &self.node),
            node: self.node.clone(),
            specification: backend.clone(),
            backends: Arc::clone(&self.backends),
        }))
    }

    fn attacher(&self, protocol: StorageProtocol) -> Result<Arc<dyn VolumeAttacher>, ActionError> {
        Ok(Arc::new(FakeAttacher { protocol, attachments: Arc::clone(&self.attachments) }))
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::contract::storage::storage_contract;

    use super::*;

    #[test]
    fn the_fake_storage_honours_the_storage_contract_for_every_protocol() {
        let node = NodeName::new("node-0").unwrap();
        let storage = FakeStorage::new(node.clone(), FakeBackends::new());
        for specification in [
            StorageProviderSpecification::LocalDisk,
            StorageProviderSpecification::Nfs { server: String::from("filer"), export: String::from("/srv") },
        ] {
            let provider = storage.provider(&specification).unwrap();
            let protocol = match specification {
                StorageProviderSpecification::LocalDisk => StorageProtocol::LocalFile,
                StorageProviderSpecification::Nfs { .. } => StorageProtocol::Nfs,
            };
            let attacher = storage.attacher(protocol).unwrap();
            assert_eq!(
                futures::executor::block_on(storage_contract(provider.as_ref(), attacher.as_ref(), &node)),
                Ok(())
            );
        }
    }
}
