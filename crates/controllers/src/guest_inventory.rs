use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::guest_inventory::{GuestInventory, GuestInventorySpecification, LxdAccess};
use amafu_domain::kinds::lxd_identity::{LxdIdentity, lxd_identity_name};
use amafu_domain::kinds::vm::{BootSource, Vm, VmPhase};
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, Clock, Consistency, Identifiers, LxdEndpoint, LxdPlatform, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

const TRUSTED_REVISIT: Duration = Duration::from_secs(30);
const UNTRUSTED_REVISIT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedVm {
    pub vm_id: ObjectId,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInventoryObservation {
    pub node: NodeName,
    pub hosted: Option<HostedVm>,
    pub recorded: Option<GuestInventorySpecification>,
    pub access: Option<LxdAccess>,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestInventoryAction {
    Record(GuestInventorySpecification),
    Forget,
}

#[must_use]
pub fn plan_guest_inventory(observation: &GuestInventoryObservation) -> Vec<GuestInventoryAction> {
    let recorded = observation.recorded.as_ref();
    let is_recorded_here = recorded.is_some_and(|inventory| inventory.node == observation.node);
    let (Some(hosted), Some(access)) = (&observation.hosted, &observation.access) else {
        let is_stale = observation.hosted.is_some() || is_recorded_here;
        return if recorded.is_some() && is_stale { vec![GuestInventoryAction::Forget] } else { Vec::new() };
    };
    let is_current = recorded.is_some_and(|inventory| {
        inventory.vm_id == hosted.vm_id && inventory.node == observation.node && &inventory.lxd == access
    });
    if is_current {
        return Vec::new();
    }
    vec![GuestInventoryAction::Record(GuestInventorySpecification {
        vm_id: hosted.vm_id,
        node: observation.node.clone(),
        lxd: access.clone(),
        changed_at: observation.now,
    })]
}

fn hosted(vm: &Object<Vm>, node: &NodeName) -> Option<HostedVm> {
    let is_here = vm.status.node.as_ref() == Some(node) && vm.status.phase == VmPhase::Running;
    let is_disk = matches!(vm.specification.boot, BootSource::Disk);
    let address = vm.status.interfaces.first()?.address;
    (is_here && is_disk).then_some(HostedVm { vm_id: vm.metadata.id, address })
}

fn inventory_of(vm: &ObjectReference) -> ObjectReference {
    ObjectReference::of::<GuestInventory>(vm.project.clone(), vm.name.clone())
}

pub struct GuestInventoryController {
    node: NodeName,
    store: Arc<dyn Store>,
    lxd: Arc<dyn LxdPlatform>,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl GuestInventoryController {
    #[must_use]
    pub fn new(
        node: NodeName,
        store: Arc<dyn Store>,
        lxd: Arc<dyn LxdPlatform>,
        clock: Arc<dyn Clock>,
        identifiers: Arc<dyn Identifiers>,
    ) -> Self {
        Self { node, store, lxd, clock, identifiers }
    }

    async fn access(&self, hosted: &HostedVm) -> Result<Option<LxdAccess>, ActionError> {
        let reference = ObjectReference::of::<LxdIdentity>(None, lxd_identity_name());
        let Some(identity) = get::<LxdIdentity>(self.store.as_ref(), &reference, Consistency::Stale).await?
        else {
            return Ok(None);
        };
        let endpoint = LxdEndpoint {
            address: hosted.address,
            certificate_pem: identity.specification.certificate_pem,
            private_key_pem: identity.specification.private_key_pem,
        };
        self.lxd.inspect(&endpoint).await
    }
}

impl Controller for GuestInventoryController {
    type Key = ObjectReference;
    type Observation = GuestInventoryObservation;
    type Action = GuestInventoryAction;

    const NAME: &'static str = "guest-inventory";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::ObjectKey(|vm| Some(inventory_of(vm))) },
        WatchedKind {
            kind: "GuestInventory",
            mapping: KeyMapping::ObjectKey(|inventory| Some(inventory.clone())),
        },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let store = self.store.as_ref();
        let vms = list::<Vm>(store, None, Consistency::Stale).await?;
        let inventories = list::<GuestInventory>(store, None, Consistency::Stale).await?;
        let hosted =
            vms.iter().filter(|vm| hosted(vm, &self.node).is_some()).map(|vm| inventory_of(&vm.reference()));
        let recorded = inventories
            .iter()
            .filter(|inventory| inventory.specification.node == self.node)
            .map(Object::reference);
        let mut keys: Vec<ObjectReference> = hosted.chain(recorded).collect();
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    async fn observe(&self, key: &ObjectReference) -> Result<GuestInventoryObservation, ActionError> {
        let store = self.store.as_ref();
        let vm_reference = ObjectReference::of::<Vm>(key.project.clone(), key.name.clone());
        let vm = get::<Vm>(store, &vm_reference, Consistency::Linearizable).await?;
        let hosted = vm.as_ref().and_then(|vm| hosted(vm, &self.node));
        let access = match &hosted {
            Some(hosted) => self.access(hosted).await?,
            None => None,
        };
        let recorded = get::<GuestInventory>(store, key, Consistency::Linearizable).await?;
        Ok(GuestInventoryObservation {
            node: self.node.clone(),
            hosted,
            recorded: recorded.map(|inventory| inventory.specification),
            access,
            now: self.clock.now(),
        })
    }

    fn plan(&self, observation: &GuestInventoryObservation) -> Vec<GuestInventoryAction> {
        plan_guest_inventory(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: GuestInventoryAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            GuestInventoryAction::Record(specification) => {
                let change = if get::<GuestInventory>(store, key, Consistency::Linearizable).await?.is_some()
                {
                    Change::UpdateSpecification { expected_generation: None, specification }
                } else {
                    Change::Create {
                        id: ObjectId::from_uuid(self.identifiers.generate()),
                        name: key.name.clone(),
                        project: key.project.clone(),
                        labels: BTreeMap::new(),
                        specification,
                    }
                };
                apply::<GuestInventory>(store, key, change).await?;
            }
            GuestInventoryAction::Forget => {
                apply::<GuestInventory>(store, key, Change::Delete).await?;
                remove::<GuestInventory>(store, key).await?;
            }
        }
        Ok(())
    }

    fn revisit_after(&self, observation: &GuestInventoryObservation) -> Option<Duration> {
        match (&observation.hosted, &observation.access) {
            (Some(_), Some(LxdAccess::Trusted { .. })) => Some(TRUSTED_REVISIT),
            (Some(_), _) => Some(UNTRUSTED_REVISIT),
            (None, _) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::guest_inventory::{GuestInstance, GuestInstanceKind};

    use super::*;

    fn node(name: &str) -> NodeName {
        NodeName::new(name).unwrap()
    }

    fn trusted(status: &str) -> LxdAccess {
        LxdAccess::Trusted {
            server: String::from("pod-0"),
            is_clustered: true,
            instances: vec![GuestInstance {
                name: String::from("juju-1a2b-0"),
                kind: GuestInstanceKind::Container,
                status: String::from(status),
                location: Some(String::from("pod-0")),
            }],
        }
    }

    fn observation(access: Option<LxdAccess>, recorded: Option<LxdAccess>) -> GuestInventoryObservation {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::nil());
        let now = OffsetDateTime::UNIX_EPOCH;
        GuestInventoryObservation {
            node: node("node-1"),
            hosted: Some(HostedVm { vm_id, address: Ipv4Addr::new(10, 1, 0, 2) }),
            recorded: recorded.map(|lxd| GuestInventorySpecification {
                vm_id,
                node: node("node-1"),
                lxd,
                changed_at: now,
            }),
            access,
            now,
        }
    }

    #[test]
    fn an_inventory_is_written_only_when_what_lxd_reports_changes() {
        assert!(matches!(
            plan_guest_inventory(&observation(Some(trusted("Running")), None))[..],
            [GuestInventoryAction::Record(_)]
        ));
        assert_eq!(
            plan_guest_inventory(&observation(Some(trusted("Running")), Some(trusted("Running")))),
            []
        );
        assert!(matches!(
            plan_guest_inventory(&observation(Some(trusted("Stopped")), Some(trusted("Running"))))[..],
            [GuestInventoryAction::Record(_)]
        ));
    }

    #[test]
    fn an_inventory_is_forgotten_when_lxd_goes_or_the_vm_leaves_this_node() {
        assert_eq!(
            plan_guest_inventory(&observation(None, Some(LxdAccess::Untrusted))),
            [GuestInventoryAction::Forget]
        );
        let mut left = observation(None, Some(trusted("Running")));
        left.hosted = None;
        assert_eq!(plan_guest_inventory(&left), [GuestInventoryAction::Forget]);
        left.recorded.as_mut().unwrap().node = node("node-2");
        assert_eq!(plan_guest_inventory(&left), []);
    }
}
