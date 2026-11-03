use std::sync::Arc;

use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::{StorageBackend, StorageProviderSpecification};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::{ProvisionedVolume, Volume, VolumePhase, VolumeStatus};
use amafu_domain::{NodeName, Object, ObjectReference};
use amafu_interfaces::objects::{get, list, update_status};
use amafu_interfaces::{ActionError, CachedImage, Consistency, ImageStore, Storage, Store, VolumeRequest};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use crate::references::{cluster_reference, project_reference};

pub struct LocalVolumeController {
    node: NodeName,
    store: Arc<dyn Store>,
    storage: Arc<dyn Storage>,
    images: Arc<dyn ImageStore>,
}

impl LocalVolumeController {
    #[must_use]
    pub fn new(
        node: NodeName,
        store: Arc<dyn Store>,
        storage: Arc<dyn Storage>,
        images: Arc<dyn ImageStore>,
    ) -> Self {
        Self { node, store, storage, images }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalVolumeObservation {
    pub node: NodeName,
    pub volume: Option<Object<Volume>>,
    pub is_local_disk: bool,
    pub is_used_here: bool,
    pub needs_source_image: bool,
    pub cached_source: Option<CachedImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalVolumeAction {
    Create(VolumeRequest),
    Delete(ProvisionedVolume),
    ReportReleased,
}

#[must_use]
pub fn plan_local_volume(observation: &LocalVolumeObservation) -> Vec<LocalVolumeAction> {
    let Some(volume) = observation.volume.as_ref().filter(|_| observation.is_local_disk) else {
        return Vec::new();
    };
    match &volume.status.provisioned {
        Some(provisioned) if provisioned.node.as_ref() == Some(&observation.node) => {
            if volume.metadata.is_deleting && !observation.is_used_here {
                vec![LocalVolumeAction::Delete(provisioned.clone()), LocalVolumeAction::ReportReleased]
            } else {
                Vec::new()
            }
        }
        Some(_) => Vec::new(),
        None if volume.metadata.is_deleting || !observation.is_used_here => Vec::new(),
        None if observation.needs_source_image && observation.cached_source.is_none() => Vec::new(),
        None => vec![LocalVolumeAction::Create(VolumeRequest {
            volume_id: volume.metadata.id,
            size_bytes: volume.specification.size_bytes(),
            source_image: observation.cached_source.as_ref().map(|cached| cached.path.clone()),
        })],
    }
}

impl LocalVolumeController {
    async fn source_image(&self, volume: &Object<Volume>) -> Result<Option<CachedImage>, ActionError> {
        let Some(name) = &volume.specification.source_image else {
            return Ok(None);
        };
        let image = get::<Image>(
            self.store.as_ref(),
            &project_reference::<Image>(volume, name),
            Consistency::Linearizable,
        )
        .await?;
        match image {
            Some(image) => Ok(self.images.cached(&image.metadata.id).await?),
            None => Ok(None),
        }
    }
}

impl Controller for LocalVolumeController {
    type Key = ObjectReference;
    type Observation = LocalVolumeObservation;
    type Action = LocalVolumeAction;

    const NAME: &'static str = "local-volume";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Volume", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Image", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let volumes = list::<Volume>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(volumes.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<LocalVolumeObservation, ActionError> {
        let store = self.store.as_ref();
        let volume = get::<Volume>(store, key, Consistency::Linearizable).await?;
        let mut observation = LocalVolumeObservation {
            node: self.node.clone(),
            volume: None,
            is_local_disk: false,
            is_used_here: false,
            needs_source_image: false,
            cached_source: None,
        };
        let Some(volume) = volume else {
            return Ok(observation);
        };
        let backend = get::<StorageBackend>(
            store,
            &cluster_reference::<StorageBackend>(&volume.specification.backend),
            Consistency::Linearizable,
        )
        .await?;
        observation.is_local_disk = backend
            .is_some_and(|backend| backend.specification.provider == StorageProviderSpecification::LocalDisk);
        if observation.is_local_disk {
            let vms = list::<Vm>(store, volume.metadata.project.as_ref(), Consistency::Linearizable).await?;
            observation.is_used_here = vms.iter().any(|vm| {
                vm.status.node.as_ref() == Some(&self.node)
                    && vm.specification.volumes.contains(&volume.metadata.name)
            });
            observation.needs_source_image = volume.specification.source_image.is_some();
            observation.cached_source = self.source_image(&volume).await?;
        }
        observation.volume = Some(volume);
        Ok(observation)
    }

    fn plan(&self, observation: &LocalVolumeObservation) -> Vec<LocalVolumeAction> {
        plan_local_volume(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: LocalVolumeAction) -> Result<(), ActionError> {
        let provider = self.storage.provider(&StorageProviderSpecification::LocalDisk)?;
        let store = self.store.as_ref();
        match action {
            LocalVolumeAction::Create(request) => {
                let provisioned = provider.create_volume(&request).await?;
                update_status::<Volume>(store, key, |volume| {
                    volume.status.provisioned.is_none().then(|| VolumeStatus {
                        phase: VolumePhase::Available,
                        provisioned: Some(provisioned.clone()),
                        message: None,
                        ..volume.status.clone()
                    })
                })
                .await?;
            }
            LocalVolumeAction::Delete(provisioned) => provider.delete_volume(&provisioned).await?,
            LocalVolumeAction::ReportReleased => {
                update_status::<Volume>(store, key, |volume| {
                    volume.status.provisioned.is_some().then(|| VolumeStatus {
                        provisioned: None,
                        phase: VolumePhase::Pending,
                        ..volume.status.clone()
                    })
                })
                .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::volume::VolumeSpecification;

    use super::*;
    use crate::testing::{metadata, name};

    fn here() -> NodeName {
        NodeName::new("node-1").unwrap()
    }

    fn observation(volume: Object<Volume>) -> LocalVolumeObservation {
        LocalVolumeObservation {
            node: here(),
            volume: Some(volume),
            is_local_disk: true,
            is_used_here: true,
            needs_source_image: false,
            cached_source: None,
        }
    }

    fn volume() -> Object<Volume> {
        Object {
            metadata: metadata("data", Some("project-a")),
            specification: VolumeSpecification {
                size_gibibytes: 1,
                backend: name("local"),
                source_image: None,
            },
            status: VolumeStatus::default(),
        }
    }

    fn provisioned_on(node: &str) -> Object<Volume> {
        let mut volume = volume();
        volume.status.provisioned = Some(ProvisionedVolume {
            identifier: String::from("data"),
            size_bytes: 1,
            node: Some(NodeName::new(node).unwrap()),
        });
        volume
    }

    #[test]
    fn a_volume_used_here_is_created_here() {
        assert!(matches!(
            plan_local_volume(&observation(volume())).as_slice(),
            [LocalVolumeAction::Create(_)]
        ));
        let unused = LocalVolumeObservation { is_used_here: false, ..observation(volume()) };
        assert_eq!(plan_local_volume(&unused), Vec::new());
        let not_local = LocalVolumeObservation { is_local_disk: false, ..observation(volume()) };
        assert_eq!(plan_local_volume(&not_local), Vec::new());
    }

    #[test]
    fn a_clone_waits_until_its_image_is_cached_here() {
        let waiting = LocalVolumeObservation { needs_source_image: true, ..observation(volume()) };
        assert_eq!(plan_local_volume(&waiting), Vec::new());
        let cached = LocalVolumeObservation {
            cached_source: Some(CachedImage { path: "/images/noble".into(), size_bytes: 1, container: None }),
            ..waiting
        };
        assert!(matches!(
            plan_local_volume(&cached).as_slice(),
            [LocalVolumeAction::Create(request)] if request.source_image.is_some()
        ));
    }

    #[test]
    fn a_deleted_volume_is_freed_only_by_its_node_once_unused() {
        let mut deleting = provisioned_on("node-1");
        deleting.metadata.is_deleting = true;
        let in_use = observation(deleting.clone());
        assert_eq!(plan_local_volume(&in_use), Vec::new());
        let unused = LocalVolumeObservation { is_used_here: false, ..in_use };
        assert!(matches!(
            plan_local_volume(&unused).as_slice(),
            [LocalVolumeAction::Delete(_), LocalVolumeAction::ReportReleased]
        ));
        let mut elsewhere = provisioned_on("node-2");
        elsewhere.metadata.is_deleting = true;
        assert_eq!(
            plan_local_volume(&LocalVolumeObservation { is_used_here: false, ..observation(elsewhere) }),
            Vec::new()
        );
    }
}
