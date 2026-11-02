use std::collections::BTreeMap;
use std::ffi::CString;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use amafu_domain::NodeName;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_interfaces::{
    ActionError, AttachTarget, StorageProtocol, StorageProvider, VolumeAttacher, VolumeRequest,
};
use rustix::mount::{MountFlags, mount};
use sha2::{Digest, Sha256};

use crate::local_disk::{VOLUME_EXTENSION, create_volume_file, delete_volume_file};

const MOUNTS: &str = "/run/amafu/nfs";
const NFS_PORT: u16 = 2049;

fn mount_point(root: &Path, server: &str, export: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{server}:{export}").as_bytes());
    root.join(hex::encode(&digest[..8]))
}

fn is_mounted(path: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mounts").is_ok_and(|mounts| {
        mounts.lines().any(|line| line.split_whitespace().nth(1) == Some(&path.to_string_lossy()))
    })
}

async fn resolve(server: &str) -> Result<IpAddr, ActionError> {
    if let Ok(address) = server.parse::<IpAddr>() {
        return Ok(address);
    }
    let mut addresses = tokio::net::lookup_host((server, NFS_PORT))
        .await
        .map_err(|error| ActionError::retryable(format!("resolving {server}: {error}")))?;
    addresses
        .next()
        .map(|address: SocketAddr| address.ip())
        .ok_or_else(|| ActionError::retryable(format!("{server} has no address")))
}

pub async fn ensure_mounted(root: &Path, server: &str, export: &str) -> Result<PathBuf, ActionError> {
    let target = mount_point(root, server, export);
    if is_mounted(&target) {
        return Ok(target);
    }
    std::fs::create_dir_all(&target)
        .map_err(|error| ActionError::retryable(format!("creating {}: {error}", target.display())))?;
    let address = resolve(server).await?;
    let source = format!("{server}:{export}");
    let options = CString::new(format!("vers=4.2,addr={address},hard,timeo=100"))
        .map_err(|error| ActionError::invalid(format!("the NFS options are invalid: {error}")))?;
    let target_for_mount = target.clone();
    tokio::task::spawn_blocking(move || {
        mount(
            source.as_str(),
            &target_for_mount,
            "nfs4",
            MountFlags::NOSUID | MountFlags::NODEV,
            options.as_c_str(),
        )
    })
    .await
    .map_err(|error| ActionError::retryable(format!("mounting {server}:{export}: {error}")))?
    .map_err(|error| ActionError::retryable(format!("mounting {server}:{export}: {error}")))?;
    tracing::info!(%server, %export, target = %target.display(), "mounted an NFS export");
    Ok(target)
}

pub struct NfsProvider {
    server: String,
    export: String,
    root: PathBuf,
}

impl NfsProvider {
    #[must_use]
    pub fn new(server: String, export: String) -> Self {
        Self { server, export, root: PathBuf::from(MOUNTS) }
    }

    async fn directory(&self) -> Result<PathBuf, ActionError> {
        ensure_mounted(&self.root, &self.server, &self.export).await
    }
}

#[async_trait::async_trait]
impl StorageProvider for NfsProvider {
    async fn check(&self) -> Result<(), ActionError> {
        let directory = self.directory().await?;
        tokio::fs::metadata(&directory)
            .await
            .map(drop)
            .map_err(|error| ActionError::retryable(format!("reading {}: {error}", directory.display())))
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        create_volume_file(&self.directory().await?, request)
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        delete_volume_file(&self.directory().await?, volume)
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        _node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        Ok(AttachTarget::NfsFile {
            server: self.server.clone(),
            export: self.export.clone(),
            file: format!("{}.{VOLUME_EXTENSION}", volume.identifier),
        })
    }

    async fn revoke_node(&self, _node: &NodeName) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct NfsAttacher {
    root: PathBuf,
    attached: Mutex<BTreeMap<AttachTarget, PathBuf>>,
}

impl Default for NfsAttacher {
    fn default() -> Self {
        Self { root: PathBuf::from(MOUNTS), attached: Mutex::new(BTreeMap::new()) }
    }
}

#[async_trait::async_trait]
impl VolumeAttacher for NfsAttacher {
    fn protocol(&self) -> StorageProtocol {
        StorageProtocol::Nfs
    }

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError> {
        let AttachTarget::NfsFile { server, export, file } = target else {
            return Err(ActionError::invalid("not an NFS target"));
        };
        if file.contains('/') || file.starts_with('.') {
            return Err(ActionError::fatal(format!("{file:?} is not a volume file name")));
        }
        let path = ensure_mounted(&self.root, server, export).await?.join(file);
        if !path.is_file() {
            return Err(ActionError::retryable(format!("{} does not exist yet", path.display())));
        }
        self.attached.lock().unwrap_or_else(PoisonError::into_inner).insert(target.clone(), path.clone());
        Ok(path)
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
    use super::*;

    #[test]
    fn every_export_gets_its_own_mount_point() {
        let root = Path::new("/run/amafu/nfs");
        assert_ne!(mount_point(root, "filer", "/srv/a"), mount_point(root, "filer", "/srv/b"));
        assert_eq!(mount_point(root, "filer", "/srv/a"), mount_point(root, "filer", "/srv/a"));
    }

    #[tokio::test]
    async fn file_names_cannot_escape_the_export() {
        let attacher = NfsAttacher::default();
        let target = AttachTarget::NfsFile {
            server: String::from("filer"),
            export: String::from("/srv"),
            file: String::from("../etc"),
        };
        assert!(matches!(attacher.attach(&target).await, Err(ActionError::Fatal(_))));
    }
}
