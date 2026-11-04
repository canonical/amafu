use std::path::PathBuf;
use std::sync::Arc;

use amafu_domain::NodeName;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_interfaces::{ActionError, Storage, StorageProtocol, StorageProvider, VolumeAttacher};

use crate::ceph::{CephCluster, CephSettings, RbdAttacher};
use crate::local_disk::{LocalDiskProvider, LocalFileAttacher};
use crate::nfs::{NfsAttacher, NfsProvider};
use crate::nvme_tcp::{NvmeTcpAttacher, SpdkTarget};

pub struct BuiltInStorage {
    local_disk: Arc<LocalDiskProvider>,
    local_file: Arc<LocalFileAttacher>,
    nfs: Arc<NfsAttacher>,
    nvme_tcp: Arc<NvmeTcpAttacher>,
    rbd: Arc<RbdAttacher>,
}

impl BuiltInStorage {
    #[must_use]
    pub fn new(node: NodeName, volumes_directory: PathBuf) -> Self {
        Self {
            local_disk: Arc::new(LocalDiskProvider::new(node.clone(), volumes_directory)),
            nvme_tcp: Arc::new(NvmeTcpAttacher::new(&node)),
            local_file: Arc::new(LocalFileAttacher::new(node)),
            nfs: Arc::new(NfsAttacher::default()),
            rbd: Arc::new(RbdAttacher),
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
            StorageProviderSpecification::NvmeTcp {
                management,
                management_user,
                management_password,
                addresses,
                pool,
            } => {
                let credentials = management_user.clone().zip(management_password.clone());
                Arc::new(SpdkTarget::new(management, credentials, addresses.clone(), pool)?)
            }
            StorageProviderSpecification::CephRbd {
                monitors,
                pool,
                user,
                key,
                management,
                management_user,
                management_password,
                command_endpoint,
                command_user,
                command_key,
                command_certificate_authority,
            } => Arc::new(CephCluster::new(&CephSettings {
                monitors,
                pool,
                user,
                key,
                management,
                management_user,
                management_password,
                command_endpoint,
                command_user,
                command_key,
                command_certificate_authority: command_certificate_authority.as_deref(),
            })?),
        })
    }

    fn attacher(&self, protocol: StorageProtocol) -> Result<Arc<dyn VolumeAttacher>, ActionError> {
        Ok(match protocol {
            StorageProtocol::LocalFile => Arc::clone(&self.local_file) as Arc<dyn VolumeAttacher>,
            StorageProtocol::Nfs => Arc::clone(&self.nfs) as Arc<dyn VolumeAttacher>,
            StorageProtocol::NvmeTcp => Arc::clone(&self.nvme_tcp) as Arc<dyn VolumeAttacher>,
            StorageProtocol::Rbd => Arc::clone(&self.rbd) as Arc<dyn VolumeAttacher>,
        })
    }
}
