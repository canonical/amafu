use std::sync::Arc;

use amafu_domain::kinds::storage_backend::{StorageBackend, StorageBackendPhase, StorageBackendStatus};
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, Consistency, Storage, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct StorageBackendController {
    store: Arc<dyn Store>,
    storage: Arc<dyn Storage>,
}

impl StorageBackendController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, storage: Arc<dyn Storage>) -> Self {
        Self { store, storage }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StorageBackendObservation {
    pub backend: Option<Object<StorageBackend>>,
    pub reachability: Result<(), String>,
    pub is_in_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageBackendAction {
    Report { observed_generation: u64, status: StorageBackendStatus },
    Remove,
}

#[must_use]
pub fn plan_storage_backend(observation: &StorageBackendObservation) -> Vec<StorageBackendAction> {
    let Some(backend) = &observation.backend else {
        return Vec::new();
    };
    if backend.metadata.is_deleting {
        return if observation.is_in_use { Vec::new() } else { vec![StorageBackendAction::Remove] };
    }
    let status = match &observation.reachability {
        Ok(()) => StorageBackendStatus { phase: StorageBackendPhase::Ready, message: None },
        Err(reason) => {
            StorageBackendStatus { phase: StorageBackendPhase::Unreachable, message: Some(reason.clone()) }
        }
    };
    if backend.status == status && backend.is_status_current() {
        return Vec::new();
    }
    vec![StorageBackendAction::Report { observed_generation: backend.metadata.generation, status }]
}

impl Controller for StorageBackendController {
    type Key = ObjectReference;
    type Observation = StorageBackendObservation;
    type Action = StorageBackendAction;

    const NAME: &'static str = "storage-backend";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "StorageBackend",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let backends = list::<StorageBackend>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(backends.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<StorageBackendObservation, ActionError> {
        let store = self.store.as_ref();
        let backend = get::<StorageBackend>(store, key, Consistency::Linearizable).await?;
        let Some(object) = &backend else {
            return Ok(StorageBackendObservation { backend, reachability: Ok(()), is_in_use: false });
        };
        let reachability = match self.storage.provider(&object.specification.provider) {
            Ok(provider) => provider.check().await.map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let is_in_use = list::<Volume>(store, None, Consistency::Linearizable)
            .await?
            .iter()
            .any(|volume| volume.specification.backend == object.metadata.name);
        Ok(StorageBackendObservation { backend, reachability, is_in_use })
    }

    fn plan(&self, observation: &StorageBackendObservation) -> Vec<StorageBackendAction> {
        plan_storage_backend(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: StorageBackendAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            StorageBackendAction::Report { observed_generation, status } => {
                apply::<StorageBackend>(store, key, Change::UpdateStatus { observed_generation, status })
                    .await?;
            }
            StorageBackendAction::Remove => remove::<StorageBackend>(store, key).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::storage_backend::{StorageBackendSpecification, StorageProviderSpecification};

    use super::*;
    use crate::testing::metadata;

    fn backend() -> Object<StorageBackend> {
        Object {
            metadata: metadata("filer", None),
            specification: StorageBackendSpecification { provider: StorageProviderSpecification::LocalDisk },
            status: StorageBackendStatus::default(),
        }
    }

    #[test]
    fn reachability_is_reported() {
        let reachable =
            StorageBackendObservation { backend: Some(backend()), reachability: Ok(()), is_in_use: false };
        assert!(matches!(
            plan_storage_backend(&reachable).as_slice(),
            [StorageBackendAction::Report { status, .. }] if status.is_ready()
        ));
        let unreachable =
            StorageBackendObservation { reachability: Err(String::from("no route")), ..reachable };
        assert!(matches!(
            plan_storage_backend(&unreachable).as_slice(),
            [StorageBackendAction::Report { status, .. }] if status.phase == StorageBackendPhase::Unreachable
        ));
    }

    #[test]
    fn a_backend_with_volumes_is_not_removed() {
        let mut deleting = backend();
        deleting.metadata.is_deleting = true;
        let in_use =
            StorageBackendObservation { backend: Some(deleting), reachability: Ok(()), is_in_use: true };
        assert_eq!(plan_storage_backend(&in_use), Vec::new());
        assert_eq!(
            plan_storage_backend(&StorageBackendObservation { is_in_use: false, ..in_use }),
            vec![StorageBackendAction::Remove]
        );
    }
}
