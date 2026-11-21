use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::ssh_key::{
    GrantedKey, SshKey, SshKeyGroup, SshKeyGroupSpecification, SshKeySpecification, granted_keys,
};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{Object, ObjectName, ObjectReference};
use amafu_interfaces::objects::{get, list, update_status};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

#[derive(Debug, Clone, PartialEq)]
pub struct GrantObservation {
    pub vm: Object<Vm>,
    pub keys: BTreeMap<ObjectName, SshKeySpecification>,
    pub groups: BTreeMap<ObjectName, SshKeyGroupSpecification>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantAction {
    Freeze(Vec<GrantedKey>),
    Explain(String),
}

#[must_use]
pub fn plan_grant(observation: &GrantObservation) -> Vec<GrantAction> {
    let vm = &observation.vm;
    if vm.has_granted_keys() || vm.metadata.is_deleting {
        return Vec::new();
    }
    let action = match granted_keys(
        &vm.specification.ssh_keys,
        |name| observation.keys.get(name),
        |name| observation.groups.get(name),
    ) {
        Ok(granted) => GrantAction::Freeze(granted),
        Err(problem) => GrantAction::Explain(problem.to_string()),
    };
    match &action {
        GrantAction::Explain(reason) if vm.status.message.as_ref() == Some(reason) => Vec::new(),
        _ => vec![action],
    }
}

pub struct AuthorizedKeysController {
    store: Arc<dyn Store>,
}

impl AuthorizedKeysController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

fn by_name<R: amafu_domain::Resource>(objects: Vec<Object<R>>) -> BTreeMap<ObjectName, R::Specification> {
    objects.into_iter().map(|object| (object.metadata.name, object.specification)).collect()
}

impl Controller for AuthorizedKeysController {
    type Key = ObjectReference;
    type Observation = Option<GrantObservation>;
    type Action = GrantAction;

    const NAME: &'static str = "authorized-keys";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "SshKey", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "SshKeyGroup", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let vms = list::<Vm>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(vms.iter().filter(|vm| !vm.has_granted_keys()).map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<Option<GrantObservation>, ActionError> {
        let store = self.store.as_ref();
        let Some(vm) = get::<Vm>(store, key, Consistency::Linearizable).await? else {
            return Ok(None);
        };
        let project = key.project.as_ref();
        let keys = by_name(list::<SshKey>(store, project, Consistency::Linearizable).await?);
        let groups = by_name(list::<SshKeyGroup>(store, project, Consistency::Linearizable).await?);
        Ok(Some(GrantObservation { vm, keys, groups }))
    }

    fn plan(&self, observation: &Option<GrantObservation>) -> Vec<GrantAction> {
        observation.as_ref().map(plan_grant).unwrap_or_default()
    }

    async fn apply(&self, key: &ObjectReference, action: GrantAction) -> Result<(), ActionError> {
        update_status::<Vm>(self.store.as_ref(), key, |vm| {
            let mut status = vm.status.clone();
            match &action {
                GrantAction::Freeze(granted) => {
                    status.authorized_keys = Some(granted.clone());
                    status.message = None;
                }
                GrantAction::Explain(reason) => status.message = Some(reason.clone()),
            }
            (status != vm.status).then_some(status)
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::ssh_key::{KeyOrigin, SshKeyReference};
    use amafu_domain::kinds::vm::{VmSpecification, VmStatus};

    use super::*;
    use crate::testing::{metadata, name, only};

    const ALICE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOpKbGPinFIKvvVQexMuxfmVR3auvr57kkIe6mkURtIs alice";

    fn vm(references: Vec<SshKeyReference>) -> Object<Vm> {
        Object {
            metadata: metadata("web", Some("default")),
            specification: VmSpecification { ssh_keys: references, ..VmSpecification::small() },
            status: VmStatus::default(),
        }
    }

    fn observation(vm: Object<Vm>) -> GrantObservation {
        GrantObservation {
            vm,
            keys: BTreeMap::from([(
                name("alice"),
                SshKeySpecification { public_key: ALICE.to_owned(), origin: KeyOrigin::Pasted },
            )]),
            groups: BTreeMap::from([(name("team"), SshKeyGroupSpecification { keys: vec![name("alice")] })]),
        }
    }

    #[test]
    fn a_vm_gets_its_keys_frozen_once() {
        let pending = observation(vm(vec![SshKeyReference::Group { name: name("team") }]));
        let GrantAction::Freeze(granted) = only(plan_grant(&pending)) else {
            panic!("the keys were not frozen")
        };
        assert_eq!(granted[0].public_key, ALICE);
        let mut frozen = pending.vm.clone();
        frozen.status.authorized_keys = Some(granted);
        assert_eq!(plan_grant(&observation(frozen)), []);
        assert_eq!(plan_grant(&observation(vm(Vec::new()))), []);
    }

    #[test]
    fn a_missing_key_is_explained_until_it_exists() {
        let missing = observation(vm(vec![SshKeyReference::Key { name: name("bob") }]));
        assert_eq!(plan_grant(&missing), [GrantAction::Explain(String::from("there is no SSH key bob"))]);
        let mut explained = missing.vm.clone();
        explained.status.message = Some(String::from("there is no SSH key bob"));
        assert_eq!(plan_grant(&observation(explained)), []);
    }
}
