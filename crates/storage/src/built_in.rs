use std::path::PathBuf;
use std::sync::Arc;

use amafu_domain::NodeName;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_interfaces::{ActionError, Storage, StorageProtocol, StorageProvider, VolumeAttacher};

use crate::local_disk::{LocalDiskProvider, LocalFileAttacher};
use crate::nfs::{NfsAttacher, NfsProvider};

pub struct BuiltInStorage {
    local_disk: Arc<LocalDiskProvider>,
    local_file: Arc<LocalFileAttacher>,
    nfs: Arc<NfsAttacher>,
}

impl BuiltInStorage {
    #[must_use]
    pub fn new(node: NodeName, volumes_directory: PathBuf) -> Self {
        Self {
            local_disk: Arc::new(LocalDiskProvider::new(node.clone(), volumes_directory)),
            local_file: Arc::new(LocalFileAttacher::new(node)),
            nfs: Arc::new(NfsAttacher::default()),
        }
    }
}

impl Storage for BuiltInStorage {
    fn provider(
        &self,
        backend: &StorageProviderSpecification,
    ) -> Result<Arc<dyn StorageProvider>, ActionError> {
        Ok(match backend {
            StorageProviderSpecification::LocalDisk => {
                Arc::clone(&self.local_disk) as Arc<dyn StorageProvider>
            }
            StorageProviderSpecification::Nfs { server, export } => {
                Arc::new(NfsProvider::new(server.clone(), export.clone()))
            }
        })
    }

    fn attacher(&self, protocol: StorageProtocol) -> Result<Arc<dyn VolumeAttacher>, ActionError> {
        Ok(match protocol {
            StorageProtocol::LocalFile => Arc::clone(&self.local_file) as Arc<dyn VolumeAttacher>,
            StorageProtocol::Nfs => Arc::clone(&self.nfs) as Arc<dyn VolumeAttacher>,
        })
    }
}
