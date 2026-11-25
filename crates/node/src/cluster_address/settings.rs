use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use amafu_dashboard::SessionKey;
use amafu_domain::kinds::cluster_address::{
    ClusterAddress, ClusterAddressCertificate, ClusterAddressCertificateSpecification,
    ClusterAddressSpecification, DashboardSessionKey, DashboardSessionKeySpecification, cluster_address_name,
};
use amafu_domain::kinds::node::Node;
use amafu_domain::{Change, ChangeError, NodeName, ObjectId, ObjectName, ObjectReference, Resource};
use amafu_interfaces::objects::{ObjectStoreError, apply, get};
use amafu_interfaces::{ActionError, Consistency, Store};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

const LOCAL_COPY_FILE: &str = "cluster-address.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const SESSION_KEY_NAME: &str = "dashboard";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterSettings {
    pub serves_dashboard: bool,
    pub address: Option<ClusterAddressSpecification>,
    pub certificate: Option<ClusterAddressCertificateSpecification>,
    pub session_key: Option<DashboardSessionKeySpecification>,
}

impl ClusterSettings {
    #[must_use]
    pub fn can_serve_the_dashboard(&self) -> bool {
        self.serves_dashboard && self.session_key.is_some()
    }

    #[must_use]
    pub fn can_hold_the_address(&self) -> bool {
        self.can_serve_the_dashboard() && self.address.is_some()
    }
}

pub struct LocalCopy {
    path: PathBuf,
}

impl LocalCopy {
    #[must_use]
    pub fn in_directory(data: &Path) -> Self {
        Self { path: data.join(LOCAL_COPY_FILE) }
    }

    #[must_use]
    pub fn load(&self) -> ClusterSettings {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, settings: &ClusterSettings) -> std::io::Result<()> {
        let staged = self.path.with_extension("json.new");
        let mut file =
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&staged)?;
        file.write_all(&serde_json::to_vec(settings)?)?;
        file.sync_all()?;
        std::fs::rename(staged, &self.path)
    }
}

fn reference<R: Resource>(name: ObjectName) -> ObjectReference {
    ObjectReference::of::<R>(None, name)
}

fn session_key_name() -> ObjectName {
    ObjectName::new(SESSION_KEY_NAME).unwrap_or_else(|_| unreachable!())
}

async fn read_specification<R: Resource>(
    store: &dyn Store,
    name: ObjectName,
) -> Result<Option<R::Specification>, ActionError> {
    let object = get::<R>(store, &reference::<R>(name), Consistency::Linearizable).await?;
    Ok(object.map(|object| object.specification))
}

async fn create_session_key(store: &dyn Store) -> Result<(), ActionError> {
    let key_hexadecimal = SessionKey::generate_hexadecimal().map_err(ActionError::retryable)?;
    let change = Change::Create {
        id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
        name: session_key_name(),
        project: None,
        labels: BTreeMap::new(),
        specification: DashboardSessionKeySpecification { key_hexadecimal },
    };
    match apply::<DashboardSessionKey>(store, &reference::<DashboardSessionKey>(session_key_name()), change)
        .await
    {
        Ok(_) => {
            tracing::info!("created the key that dashboard sessions are sealed with");
            Ok(())
        }
        Err(ObjectStoreError::Change(ChangeError::AlreadyExists)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn read_session_key(
    store: &dyn Store,
    serves_dashboard: bool,
) -> Result<Option<DashboardSessionKeySpecification>, ActionError> {
    let existing = read_specification::<DashboardSessionKey>(store, session_key_name()).await?;
    if existing.is_some() || !serves_dashboard {
        return Ok(existing);
    }
    create_session_key(store).await?;
    read_specification::<DashboardSessionKey>(store, session_key_name()).await
}

pub async fn read_from_store(store: &dyn Store, node: &NodeName) -> Result<ClusterSettings, ActionError> {
    let node_reference = reference::<Node>(ObjectName::from(node));
    let serves_dashboard = get::<Node>(store, &node_reference, Consistency::Linearizable)
        .await?
        .is_some_and(|node| node.specification.serves_dashboard());
    Ok(ClusterSettings {
        serves_dashboard,
        address: read_specification::<ClusterAddress>(store, cluster_address_name()).await?,
        certificate: read_specification::<ClusterAddressCertificate>(store, cluster_address_name()).await?,
        session_key: read_session_key(store, serves_dashboard).await?,
    })
}

pub async fn keep_settings_current(
    store: Arc<dyn Store>,
    node: NodeName,
    local_copy: LocalCopy,
    settings: watch::Sender<ClusterSettings>,
) -> String {
    let mut interval = tokio::time::interval(REFRESH_INTERVAL);
    loop {
        interval.tick().await;
        let current = match read_from_store(store.as_ref(), &node).await {
            Ok(current) => current,
            Err(error) => {
                tracing::debug!(%error, "the cluster address settings cannot be read; keeping the local copy");
                continue;
            }
        };
        if *settings.borrow() == current {
            continue;
        }
        if let Err(error) = local_copy.save(&current) {
            tracing::warn!(%error, "the local copy of the cluster address settings cannot be saved");
        }
        settings.send_replace(current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_copy_survives_a_restart_and_only_its_owner_can_read_it() {
        let directory = tempfile::tempdir().unwrap();
        let copy = LocalCopy::in_directory(directory.path());
        assert_eq!(copy.load(), ClusterSettings::default());
        let settings = ClusterSettings {
            serves_dashboard: true,
            address: Some(ClusterAddressSpecification {
                address: "10.20.0.50".parse().unwrap(),
                prefix_length: 24,
                interface: None,
            }),
            certificate: None,
            session_key: Some(DashboardSessionKeySpecification { key_hexadecimal: "ab".repeat(32) }),
        };
        copy.save(&settings).unwrap();
        assert_eq!(LocalCopy::in_directory(directory.path()).load(), settings);
        let mode = std::fs::metadata(directory.path().join(LOCAL_COPY_FILE)).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
    }
}
