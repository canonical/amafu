use std::sync::Arc;

use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::{StorageBackend, StorageProviderSpecification};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::{ProvisionedVolume, Volume, VolumePhase, VolumeStatus};
use amafu_domain::{Change, Object, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, CachedImage, Consistency, ImageStore, Storage, Store, VolumeRequest};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use crate::references::{cluster_reference, project_reference};

pub struct VolumeController {
    store: Arc<dyn Store>,
    storage: Arc<dyn Storage>,
    images: Arc<dyn ImageStore>,
}

impl VolumeController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, storage: Arc<dyn Storage>, images: Arc<dyn ImageStore>) -> Self {
        Self { store, storage, images }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolumeObservation {
    pub volume: Option<Object<Volume>>,
    pub backend: Option<Object<StorageBackend>>,
    pub source_image: Option<Object<Image>>,
    pub cached_source: Option<CachedImage>,
    pub users: Vec<ObjectName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeAction {
    Provision { backend: StorageProviderSpecification, request: VolumeRequest, observed_generation: u64 },
    DeleteBacking { backend: StorageProviderSpecification, provisioned: ProvisionedVolume },
    Report { observed_generation: u64, status: VolumeStatus },
    Remove,
}

fn report(volume: &Object<Volume>, status: VolumeStatus) -> Vec<VolumeAction> {
    if volume.status == status && volume.is_status_current() {
        return Vec::new();
    }
    vec![VolumeAction::Report { observed_generation: volume.metadata.generation, status }]
}

fn waiting(volume: &Object<Volume>, reason: String) -> Vec<VolumeAction> {
    report(
        volume,
        VolumeStatus { phase: VolumePhase::Pending, message: Some(reason), ..volume.status.clone() },
    )
}

fn plan_deletion(volume: &Object<Volume>, observation: &VolumeObservation) -> Vec<VolumeAction> {
    if let Some(user) = observation.users.first() {
        return report(
            volume,
            VolumeStatus { message: Some(format!("in use by VM {user}")), ..volume.status.clone() },
        );
    }
    match (&volume.status.provisioned, &observation.backend) {
        (Some(provisioned), Some(backend)) => vec![VolumeAction::DeleteBacking {
            backend: backend.specification.provider.clone(),
            provisioned: provisioned.clone(),
        }],
        (Some(_), None) => report(
            volume,
            VolumeStatus {
                message: Some(String::from("its storage backend no longer exists")),
                ..volume.status.clone()
            },
        ),
        (None, _) => vec![VolumeAction::Remove],
    }
}

fn plan_provisioning(volume: &Object<Volume>, observation: &VolumeObservation) -> Vec<VolumeAction> {
    let Some(backend) = observation.backend.as_ref().filter(|backend| !backend.metadata.is_deleting) else {
        return waiting(volume, format!("storage backend {} does not exist", volume.specification.backend));
    };
    if !backend.status.is_ready() {
        return waiting(volume, format!("storage backend {} is not ready", volume.specification.backend));
    }
    let source_image = match &volume.specification.source_image {
        None => None,
        Some(name) => match (&observation.source_image, &observation.cached_source) {
            (Some(image), _) if !image.specification.is_disk() => {
                return report(
                    volume,
                    VolumeStatus {
                        phase: VolumePhase::Failed,
                        message: Some(format!("image {name} is a container image, not a disk image")),
                        ..VolumeStatus::default()
                    },
                );
            }
            (Some(image), Some(cached)) if image.status.is_ready() => Some(cached.path.clone()),
            _ => return waiting(volume, format!("image {name} is not ready")),
        },
    };
    vec![VolumeAction::Provision {
        backend: backend.specification.provider.clone(),
        request: VolumeRequest {
            volume_id: volume.metadata.id,
            size_bytes: volume.specification.size_bytes(),
            source_image,
        },
        observed_generation: volume.metadata.generation,
    }]
}

#[must_use]
pub fn plan_volume(observation: &VolumeObservation) -> Vec<VolumeAction> {
    let Some(volume) = &observation.volume else {
        return Vec::new();
    };
    if volume.metadata.is_deleting {
        return plan_deletion(volume, observation);
    }
    let Some(provisioned) = &volume.status.provisioned else {
        return plan_provisioning(volume, observation);
    };
    let keeps_current_user =
        volume.status.attached_to.as_ref().is_some_and(|current| observation.users.contains(current));
    let attached_to = if keeps_current_user {
        volume.status.attached_to.clone()
    } else {
        observation.users.first().cloned()
    };
    let phase = if attached_to.is_some() { VolumePhase::Attached } else { VolumePhase::Available };
    let message = (observation.users.len() > 1).then(|| {
        let waiting: Vec<String> = observation
            .users
            .iter()
            .filter(|user| Some(*user) != attached_to.as_ref())
            .map(ToString::to_string)
            .collect();
        format!("also wanted by {}, which wait until it is released", waiting.join(", "))
    });
    report(volume, VolumeStatus { phase, provisioned: Some(provisioned.clone()), attached_to, message })
}

impl VolumeController {
    async fn users_of(&self, volume: &Object<Volume>) -> Result<Vec<ObjectName>, ActionError> {
        let vms =
            list::<Vm>(self.store.as_ref(), volume.metadata.project.as_ref(), Consistency::Linearizable)
                .await?;
        let mut users: Vec<ObjectName> = vms
            .into_iter()
            .filter(|vm| vm.specification.volumes.contains(&volume.metadata.name))
            .map(|vm| vm.metadata.name)
            .collect();
        users.sort();
        Ok(users)
    }
}

impl Controller for VolumeController {
    type Key = ObjectReference;
    type Observation = VolumeObservation;
    type Action = VolumeAction;

    const NAME: &'static str = "volume";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Volume", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Image", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "StorageBackend", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let volumes = list::<Volume>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(volumes.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<VolumeObservation, ActionError> {
        let store = self.store.as_ref();
        let Some(volume) = get::<Volume>(store, key, Consistency::Linearizable).await? else {
            return Ok(VolumeObservation {
                volume: None,
                backend: None,
                source_image: None,
                cached_source: None,
                users: Vec::new(),
            });
        };
        let backend = get::<StorageBackend>(
            store,
            &cluster_reference::<StorageBackend>(&volume.specification.backend),
            Consistency::Linearizable,
        )
        .await?;
        let source_image = match &volume.specification.source_image {
            Some(name) => {
                get::<Image>(store, &project_reference::<Image>(&volume, name), Consistency::Linearizable)
                    .await?
            }
            None => None,
        };
        let cached_source = match &source_image {
            Some(image) => self.images.cached(&image.metadata.id).await?,
            None => None,
        };
        let users = self.users_of(&volume).await?;
        Ok(VolumeObservation { volume: Some(volume), backend, source_image, cached_source, users })
    }

    fn plan(&self, observation: &VolumeObservation) -> Vec<VolumeAction> {
        plan_volume(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: VolumeAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            VolumeAction::Provision { backend, request, observed_generation } => {
                let provisioned = self.storage.provider(&backend)?.create_volume(&request).await?;
                let status = VolumeStatus {
                    phase: VolumePhase::Available,
                    provisioned: Some(provisioned),
                    ..VolumeStatus::default()
                };
                apply::<Volume>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
            }
            VolumeAction::DeleteBacking { backend, provisioned } => {
                self.storage.provider(&backend)?.delete_volume(&provisioned).await?;
                remove::<Volume>(store, key).await?;
            }
            VolumeAction::Report { observed_generation, status } => {
                apply::<Volume>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
            }
            VolumeAction::Remove => remove::<Volume>(store, key).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::image::{ImagePhase, ImageSource, ImageSpecification, ImageStatus};
    use amafu_domain::kinds::storage_backend::{
        StorageBackendPhase, StorageBackendSpecification, StorageBackendStatus,
    };
    use amafu_domain::kinds::volume::VolumeSpecification;

    use super::*;
    use crate::testing::{metadata, name, only};

    fn backend(phase: StorageBackendPhase) -> Object<StorageBackend> {
        Object {
            metadata: metadata("local", None),
            specification: StorageBackendSpecification { provider: StorageProviderSpecification::LocalDisk },
            status: StorageBackendStatus { phase, message: None },
        }
    }

    fn volume(source_image: Option<&str>) -> Object<Volume> {
        Object {
            metadata: metadata("data", Some("project-a")),
            specification: VolumeSpecification {
                size_gibibytes: 1,
                backend: name("local"),
                source_image: source_image.map(name),
            },
            status: VolumeStatus::default(),
        }
    }

    fn observation(volume: Object<Volume>) -> VolumeObservation {
        VolumeObservation {
            volume: Some(volume),
            backend: Some(backend(StorageBackendPhase::Ready)),
            source_image: None,
            cached_source: None,
            users: Vec::new(),
        }
    }

    fn provisioned(mut volume: Object<Volume>) -> Object<Volume> {
        volume.status = VolumeStatus {
            phase: VolumePhase::Available,
            provisioned: Some(ProvisionedVolume {
                identifier: String::from("data"),
                size_bytes: 1,
                node: None,
            }),
            attached_to: None,
            message: None,
        };
        volume
    }

    #[test]
    fn a_new_volume_is_provisioned_on_a_ready_backend() {
        let actions = plan_volume(&observation(volume(None)));
        assert!(
            matches!(actions.as_slice(), [VolumeAction::Provision { request, .. }] if request.source_image.is_none())
        );
    }

    #[test]
    fn provisioning_waits_for_the_backend_and_the_image() {
        let mut unready = observation(volume(None));
        unready.backend = Some(backend(StorageBackendPhase::Unknown));
        assert!(
            matches!(plan_volume(&unready).as_slice(), [VolumeAction::Report { status, .. }] if status.phase == VolumePhase::Pending)
        );

        let mut cloned = observation(volume(Some("ubuntu")));
        assert!(matches!(plan_volume(&cloned).as_slice(), [VolumeAction::Report { .. }]));
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".parse().unwrap();
        cloned.source_image = Some(Object {
            metadata: metadata("ubuntu", Some("project-a")),
            specification: ImageSpecification {
                source: ImageSource::Disk { url: String::from("https://example.com/u.img"), sha256 },
            },
            status: ImageStatus { phase: ImagePhase::Ready, ..ImageStatus::default() },
        });
        cloned.cached_source =
            Some(CachedImage { path: "/var/lib/amafu/images/ubuntu".into(), size_bytes: 1, container: None });
        assert!(matches!(
            plan_volume(&cloned).as_slice(),
            [VolumeAction::Provision { request, .. }] if request.source_image.is_some()
        ));
    }

    #[test]
    fn the_first_user_by_name_gets_the_volume_and_keeps_it() {
        let mut shared = observation(provisioned(volume(None)));
        shared.users = vec![name("alpha"), name("beta")];
        let VolumeAction::Report { status, .. } = only(plan_volume(&shared)) else {
            panic!("expected one report");
        };
        assert_eq!(status.attached_to, Some(name("alpha")));
        assert_eq!(status.phase, VolumePhase::Attached);

        let mut held = shared.clone();
        if let Some(volume) = &mut held.volume {
            volume.status.attached_to = Some(name("beta"));
        }
        let VolumeAction::Report { status, .. } = only(plan_volume(&held)) else {
            panic!("expected one report");
        };
        assert_eq!(status.attached_to, Some(name("beta")));
    }

    #[test]
    fn a_deleted_volume_waits_for_its_users_then_frees_its_storage() {
        let mut deleting = provisioned(volume(None));
        deleting.metadata.is_deleting = true;
        let mut in_use = observation(deleting.clone());
        in_use.users = vec![name("alpha")];
        assert!(matches!(plan_volume(&in_use).as_slice(), [VolumeAction::Report { .. }]));
        assert!(matches!(
            plan_volume(&observation(deleting)).as_slice(),
            [VolumeAction::DeleteBacking { .. }]
        ));
        let mut never_provisioned = volume(None);
        never_provisioned.metadata.is_deleting = true;
        assert_eq!(plan_volume(&observation(never_provisioned)), vec![VolumeAction::Remove]);
    }
}
