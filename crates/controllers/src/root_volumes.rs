use std::sync::Arc;

use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::labels::ROOT_OF;
use amafu_domain::{Change, Object, ObjectReference};
use amafu_interfaces::objects::{apply, list};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootVolume {
    pub volume: ObjectReference,
    pub has_owner: bool,
    pub is_used: bool,
    pub is_deleting: bool,
}

#[must_use]
pub fn orphaned_root_volumes(volumes: &[RootVolume]) -> Vec<ObjectReference> {
    volumes
        .iter()
        .filter(|volume| !volume.has_owner && !volume.is_used && !volume.is_deleting)
        .map(|volume| volume.volume.clone())
        .collect()
}

pub struct RootVolumeController {
    store: Arc<dyn Store>,
}

impl RootVolumeController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
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
            })
        })
        .collect()
}

impl Controller for RootVolumeController {
    type Key = ();
    type Observation = Vec<RootVolume>;
    type Action = ObjectReference;

    const NAME: &'static str = "root-volumes";
    const WATCHES: &'static [WatchedKind<()>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<()>, ActionError> {
        Ok(vec![()])
    }

    async fn observe(&self, _key: &()) -> Result<Vec<RootVolume>, ActionError> {
        let store = self.store.as_ref();
        let volumes = list::<Volume>(store, None, Consistency::Linearizable).await?;
        let vms = list::<Vm>(store, None, Consistency::Linearizable).await?;
        Ok(observe_root_volumes(&volumes, &vms))
    }

    fn plan(&self, observation: &Vec<RootVolume>) -> Vec<ObjectReference> {
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

    fn root(name: &str, has_owner: bool, is_used: bool, is_deleting: bool) -> RootVolume {
        RootVolume {
            volume: ObjectReference::of::<Volume>(
                Some(ProjectName::new("default").unwrap()),
                ObjectName::new(name).unwrap(),
            ),
            has_owner,
            is_used,
            is_deleting,
        }
    }

    #[test]
    fn a_root_volume_goes_once_its_vm_is_gone_and_nothing_uses_it() {
        let volumes = [
            root("web-root", true, true, false),
            root("gone-root", false, false, false),
            root("lent-root", false, true, false),
            root("going-root", false, false, true),
        ];
        assert_eq!(orphaned_root_volumes(&volumes), [volumes[1].volume.clone()]);
    }
}
