use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use amafu_domain::NodeName;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_interfaces::{
    ActionError, AttachTarget, StorageProtocol, StorageProvider, VolumeAttacher, VolumeRequest,
};
use reqwest::StatusCode;
use serde_json::{Value, json};

const RBD_DEVICES: &str = "/sys/bus/rbd/devices";
const RBD_ADD: &str = "/sys/bus/rbd/add_single_major";
const RBD_REMOVE: &str = "/sys/bus/rbd/remove_single_major";
const DASHBOARD_ACCEPT: &str = "application/vnd.ceph.api.v1.0+json";
const DEVICE_WAIT: Duration = Duration::from_secs(2);
const DEVICE_POLL: Duration = Duration::from_millis(100);

pub struct CephCluster {
    monitors: Vec<SocketAddrV4>,
    pool: String,
    user: String,
    key: String,
    management: String,
    management_user: String,
    management_password: String,
    http: reqwest::Client,
}

pub struct CephSettings<'settings> {
    pub monitors: &'settings [SocketAddrV4],
    pub pool: &'settings str,
    pub user: &'settings str,
    pub key: &'settings str,
    pub management: &'settings str,
    pub management_user: &'settings str,
    pub management_password: &'settings str,
}

fn unreachable_dashboard(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("the Ceph dashboard: {error}"))
}

impl CephCluster {
    pub fn new(settings: &CephSettings<'_>) -> Result<Self, ActionError> {
        let http = crate::management::management_client(Duration::from_secs(30))?;
        Ok(Self {
            monitors: settings.monitors.to_vec(),
            pool: settings.pool.to_owned(),
            user: settings.user.to_owned(),
            key: settings.key.to_owned(),
            management: settings.management.trim_end_matches('/').to_owned(),
            management_user: settings.management_user.to_owned(),
            management_password: settings.management_password.to_owned(),
            http,
        })
    }

    async fn token(&self) -> Result<String, ActionError> {
        let response = self
            .http
            .post(format!("{}/api/auth", self.management))
            .header("Accept", DASHBOARD_ACCEPT)
            .json(&json!({"username": self.management_user, "password": self.management_password}))
            .send()
            .await
            .map_err(unreachable_dashboard)?;
        if !response.status().is_success() {
            return Err(unreachable_dashboard(format!("signing in answered {}", response.status())));
        }
        let answer: Value = response.json().await.map_err(unreachable_dashboard)?;
        answer["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| unreachable_dashboard("no token was returned"))
    }

    fn image_path(&self, image: &str) -> String {
        format!("{}/api/block/image/{}%2F{image}", self.management, self.pool)
    }

    async fn image_exists(&self, token: &str, image: &str) -> Result<bool, ActionError> {
        let response = self
            .http
            .get(self.image_path(image))
            .bearer_auth(token)
            .header("Accept", DASHBOARD_ACCEPT)
            .send()
            .await
            .map_err(unreachable_dashboard)?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(false),
            status if status.is_success() => Ok(true),
            status => Err(unreachable_dashboard(format!("looking up {image} answered {status}"))),
        }
    }
}

#[async_trait::async_trait]
impl StorageProvider for CephCluster {
    async fn check(&self) -> Result<(), ActionError> {
        let token = self.token().await?;
        let response = self
            .http
            .get(format!("{}/api/pool/{}", self.management, self.pool))
            .bearer_auth(token)
            .header("Accept", DASHBOARD_ACCEPT)
            .send()
            .await
            .map_err(unreachable_dashboard)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(unreachable_dashboard(format!("pool {} answered {}", self.pool, response.status())))
        }
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        if request.source_image.is_some() {
            return Err(ActionError::invalid("Ceph RBD volumes cannot be cloned from an image yet"));
        }
        let identifier = request.volume_id.to_string();
        let token = self.token().await?;
        if !self.image_exists(&token, &identifier).await? {
            let response = self
                .http
                .post(format!("{}/api/block/image", self.management))
                .bearer_auth(&token)
                .header("Accept", DASHBOARD_ACCEPT)
                .json(&json!({"pool_name": self.pool, "name": identifier, "size": request.size_bytes, "features": ["layering"]}))
                .send()
                .await
                .map_err(unreachable_dashboard)?;
            if !response.status().is_success() {
                return Err(unreachable_dashboard(format!(
                    "creating {identifier} answered {}",
                    response.status()
                )));
            }
            if !self.image_exists(&token, &identifier).await? {
                return Err(ActionError::retryable(format!("image {identifier} is still being created")));
            }
        }
        Ok(ProvisionedVolume { identifier, size_bytes: request.size_bytes, node: None })
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        let token = self.token().await?;
        let response = self
            .http
            .delete(self.image_path(&volume.identifier))
            .bearer_auth(token)
            .header("Accept", DASHBOARD_ACCEPT)
            .send()
            .await
            .map_err(unreachable_dashboard)?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(()),
            status if status.is_success() => Ok(()),
            status => Err(unreachable_dashboard(format!("deleting {} answered {status}", volume.identifier))),
        }
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        _node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        Ok(AttachTarget::Rbd {
            monitors: self.monitors.clone(),
            pool: self.pool.clone(),
            image: volume.identifier.clone(),
            user: self.user.clone(),
            key: self.key.clone(),
        })
    }

    async fn revoke_node(&self, _node: &NodeName) -> Result<(), ActionError> {
        Ok(())
    }
}

fn read_attribute(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|value| value.trim().to_owned())
}

fn mapped_device(devices: &Path, pool: &str, image: &str) -> Option<(String, PathBuf)> {
    std::fs::read_dir(devices).ok()?.flatten().find_map(|entry| {
        let directory = entry.path();
        let is_wanted = read_attribute(&directory.join("pool")).as_deref() == Some(pool)
            && read_attribute(&directory.join("name")).as_deref() == Some(image);
        let identifier = entry.file_name().to_string_lossy().into_owned();
        is_wanted.then(|| (identifier.clone(), PathBuf::from(format!("/dev/rbd{identifier}"))))
    })
}

fn map_request(monitors: &[SocketAddrV4], user: &str, key: &str, pool: &str, image: &str) -> String {
    let monitors: Vec<String> = monitors.iter().map(ToString::to_string).collect();
    format!("{} name={user},secret={key} {pool} {image}", monitors.join(","))
}

#[derive(Default)]
pub struct RbdAttacher;

#[async_trait::async_trait]
impl VolumeAttacher for RbdAttacher {
    fn protocol(&self) -> StorageProtocol {
        StorageProtocol::Rbd
    }

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError> {
        let AttachTarget::Rbd { monitors, pool, image, user, key } = target else {
            return Err(ActionError::invalid("not a Ceph RBD target"));
        };
        if let Some((_, device)) = mapped_device(Path::new(RBD_DEVICES), pool, image) {
            return Ok(device);
        }
        let request = map_request(monitors, user, key, pool, image);
        tokio::task::spawn_blocking(move || std::fs::write(RBD_ADD, request.as_bytes()))
            .await
            .map_err(|error| ActionError::retryable(format!("mapping: {error}")))?
            .map_err(|error| ActionError::retryable(format!("mapping {pool}/{image}: {error}")))?;
        let started = Instant::now();
        loop {
            if let Some((_, device)) = mapped_device(Path::new(RBD_DEVICES), pool, image)
                && device.exists()
            {
                return Ok(device);
            }
            if started.elapsed() >= DEVICE_WAIT {
                return Err(ActionError::retryable(format!("{pool}/{image} did not appear as a device")));
            }
            tokio::time::sleep(DEVICE_POLL).await;
        }
    }

    async fn detach(&self, target: &AttachTarget) -> Result<(), ActionError> {
        let AttachTarget::Rbd { pool, image, .. } = target else {
            return Err(ActionError::invalid("not a Ceph RBD target"));
        };
        if let Some((identifier, _)) = mapped_device(Path::new(RBD_DEVICES), pool, image) {
            std::fs::write(RBD_REMOVE, identifier.as_bytes())
                .map_err(|error| ActionError::retryable(format!("unmapping {pool}/{image}: {error}")))?;
        }
        Ok(())
    }

    async fn attached(&self) -> Result<Vec<(AttachTarget, PathBuf)>, ActionError> {
        let Ok(entries) = std::fs::read_dir(RBD_DEVICES) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .flatten()
            .filter_map(|entry| {
                let directory = entry.path();
                let pool = read_attribute(&directory.join("pool"))?;
                let image = read_attribute(&directory.join("name"))?;
                let identifier = entry.file_name().to_string_lossy().into_owned();
                let target = AttachTarget::Rbd {
                    monitors: Vec::new(),
                    pool,
                    image,
                    user: String::new(),
                    key: String::new(),
                };
                Some((target, PathBuf::from(format!("/dev/rbd{identifier}"))))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cluster_client_is_built_without_touching_the_network() {
        let monitors = ["192.0.2.42:6789".parse().unwrap()];
        let cluster = CephCluster::new(&CephSettings {
            monitors: &monitors,
            pool: "amafu",
            user: "amafu",
            key: "c2VjcmV0",
            management: "http://192.0.2.42:8080",
            management_user: "amafu",
            management_password: "secret",
        });
        assert!(cluster.is_ok());
    }

    #[test]
    fn a_map_request_names_monitors_credentials_and_image() {
        let monitors = ["10.216.0.42:6789".parse().unwrap()];
        assert_eq!(
            map_request(&monitors, "amafu", "c2VjcmV0", "volumes", "abc"),
            "10.216.0.42:6789 name=amafu,secret=c2VjcmV0 volumes abc"
        );
    }

    #[test]
    fn a_mapped_image_is_found_by_pool_and_name() {
        let sysfs = tempfile::tempdir().unwrap();
        let device = sysfs.path().join("3");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("pool"), "volumes\n").unwrap();
        std::fs::write(device.join("name"), "abc\n").unwrap();
        assert_eq!(
            mapped_device(sysfs.path(), "volumes", "abc"),
            Some((String::from("3"), PathBuf::from("/dev/rbd3")))
        );
        assert_eq!(mapped_device(sysfs.path(), "volumes", "other"), None);
    }
}
