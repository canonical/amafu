use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::labels::ROOT_OF;
use amafu_domain::{Change, Object, ObjectReference};
use amafu_interfaces::objects::{apply, list};
use amafu_interfaces::{ActionError, Clock, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootVolume {
    pub volume: ObjectReference,
    pub has_owner: bool,
    pub is_used: bool,
    pub is_deleting: bool,
    pub orphaned_since: Option<OffsetDateTime>,
}

impl RootVolume {
    fn is_orphaned(&self) -> bool {
        !self.has_owner && !self.is_used && !self.is_deleting
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootVolumesObservation {
    pub volumes: Vec<RootVolume>,
    pub now: OffsetDateTime,
}

pub const ORPHAN_GRACE: Duration = Duration::minutes(2);

#[must_use]
pub fn orphaned_root_volumes(observation: &RootVolumesObservation) -> Vec<ObjectReference> {
    observation
        .volumes
        .iter()
        .filter(|volume| volume.is_orphaned())
        .filter(|volume| volume.orphaned_since.is_some_and(|since| observation.now - since >= ORPHAN_GRACE))
        .map(|volume| volume.volume.clone())
        .collect()
}

pub struct RootVolumeController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    first_orphaned: Mutex<BTreeMap<ObjectReference, OffsetDateTime>>,
}

impl RootVolumeController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock, first_orphaned: Mutex::new(BTreeMap::new()) }
    }
}

fn remember_orphans(
    first_orphaned: &mut BTreeMap<ObjectReference, OffsetDateTime>,
    volumes: &mut [RootVolume],
    now: OffsetDateTime,
) {
    first_orphaned.retain(|reference, _| {
        volumes.iter().any(|volume| &volume.volume == reference && volume.is_orphaned())
    });
    for volume in volumes.iter_mut().filter(|volume| volume.is_orphaned()) {
        volume.orphaned_since = Some(*first_orphaned.entry(volume.volume.clone()).or_insert(now));
    }
}

fn observe_root_volumes(volumes: &[Object<Volume>], vms: &[Object<Vm>]) -> Vec<RootVolume> {
    volumes
        .iter()
        .filter_map(|volume| {
            let owner = volume.metadata.labels.get(ROOT_OF)?;
            let in_project = |vm: &&Object<Vm>| vm.metadata.project == volume.metadata.project;
            Some(RootVolume {
                volume: volume.reference(),
                has_owner: vms.iter().filter(in_project).any(|vm| vm.metadata.name.as_str() == owner),
                is_used: vms
                    .iter()
                    .filter(in_project)
                    .any(|vm| vm.specification.volumes.contains(&volume.metadata.name)),
                is_deleting: volume.metadata.is_deleting,
                orphaned_since: None,
            })
        })
        .collect()
}

impl Controller for RootVolumeController {
    type Key = ();
    type Observation = RootVolumesObservation;
    type Action = ObjectReference;

    const NAME: &'static str = "root-volumes";
    const WATCHES: &'static [WatchedKind<()>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<()>, ActionError> {
        Ok(vec![()])
    }

    async fn observe(&self, _key: &()) -> Result<RootVolumesObservation, ActionError> {
        let store = self.store.as_ref();
        let volumes = list::<Volume>(store, None, Consistency::Linearizable).await?;
        let vms = list::<Vm>(store, None, Consistency::Linearizable).await?;
        let now = self.clock.now();
        let mut volumes = observe_root_volumes(&volumes, &vms);
        let mut first_orphaned = self.first_orphaned.lock().unwrap_or_else(PoisonError::into_inner);
        remember_orphans(&mut first_orphaned, &mut volumes, now);
        Ok(RootVolumesObservation { volumes, now })
    }

    fn plan(&self, observation: &RootVolumesObservation) -> Vec<ObjectReference> {
        orphaned_root_volumes(observation)
    }

    async fn apply(&self, _key: &(), volume: ObjectReference) -> Result<(), ActionError> {
        tracing::info!(volume = %volume, "deleting the root volume of a deleted VM");
        apply::<Volume>(self.store.as_ref(), &volume, Change::Delete).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::{ObjectName, ProjectName};

    use super::*;

    fn start() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn root(name: &str, has_owner: bool, is_used: bool, is_deleting: bool) -> RootVolume {
        RootVolume {
            volume: ObjectReference::of::<Volume>(
                Some(ProjectName::new("default").unwrap()),
                ObjectName::new(name).unwrap(),
            ),
            has_owner,
            is_used,
            is_deleting,
            orphaned_since: Some(start()),
        }
    }

    #[test]
    fn a_root_volume_goes_once_its_vm_has_been_gone_a_while_and_nothing_uses_it() {
        let volumes = vec![
            root("web-root", true, true, false),
            root("gone-root", false, false, false),
            root("lent-root", false, true, false),
            root("going-root", false, false, true),
        ];
        let early = RootVolumesObservation { volumes: volumes.clone(), now: start() + Duration::seconds(30) };
        assert_eq!(orphaned_root_volumes(&early), []);
        let later = RootVolumesObservation { volumes: volumes.clone(), now: start() + ORPHAN_GRACE };
        assert_eq!(orphaned_root_volumes(&later), [volumes[1].volume.clone()]);
    }

    #[test]
    fn the_grace_starts_when_a_volume_is_first_seen_without_its_vm() {
        let mut first_orphaned = BTreeMap::new();
        let fresh = || vec![RootVolume { orphaned_since: None, ..root("new-root", false, false, false) }];
        let mut waiting = fresh();
        remember_orphans(&mut first_orphaned, &mut waiting, start());
        assert_eq!(waiting[0].orphaned_since, Some(start()));
        let mut owned = vec![RootVolume { orphaned_since: None, ..root("new-root", true, true, false) }];
        remember_orphans(&mut first_orphaned, &mut owned, start() + Duration::minutes(5));
        assert_eq!(owned[0].orphaned_since, None);
        let mut orphaned_again = fresh();
        remember_orphans(&mut first_orphaned, &mut orphaned_again, start() + Duration::minutes(6));
        assert_eq!(orphaned_again[0].orphaned_since, Some(start() + Duration::minutes(6)));
    }
}
