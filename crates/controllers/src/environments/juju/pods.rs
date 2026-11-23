use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::kinds::guest_inventory::{GuestInventory, LxdAccess};
use amafu_domain::kinds::juju_controller::{JujuController, JujuControllerChange, MAXIMUM_PODS, Substrate};
use amafu_domain::kinds::lxd_identity::{LxdIdentity, LxdIdentitySpecification, lxd_identity_name};
use amafu_domain::{ObjectName, ObjectReference};
use amafu_interfaces::{ActionError, LxdEndpoint};
use amafu_reconciler::Step;
use time::Duration;

use super::JujuEnvironment;
use super::bootstrap::{PodRole, PodSetup, Target};
use super::scope::{ADMIN_PASSWORD, ChangeContext, ControllerScope, UBUNTU_IMAGE, pod_substrate};
use crate::environments::{Machine, MachinePlan};

pub const GUESTS_PER_POD: u16 = 32;
pub const POD_GROUP: &str = "pods";
pub const LXD_ADDRESS: &str = "lxd";
const POD_SETUP_DEADLINE: Duration = Duration::minutes(30);
const RUNNING: &str = "Running";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerGuest {
    pub pod: ObjectName,
    pub address: Ipv4Addr,
}

#[must_use]
pub fn controller_guest(inventories: &[(ObjectName, LxdAccess)]) -> Option<ControllerGuest> {
    inventories.iter().find_map(|(pod, access)| {
        let LxdAccess::Trusted { instances, .. } = access else {
            return None;
        };
        instances
            .iter()
            .filter(|instance| instance.is_juju_controller && instance.status == RUNNING)
            .find_map(|instance| instance.addresses.first())
            .map(|address| ControllerGuest { pod: pod.clone(), address: *address })
    })
}

fn pod_suffix(index: u8) -> String {
    format!("pod-{index}")
}

pub struct Pods<'s> {
    scope: &'s ControllerScope<'s>,
}

impl<'s> Pods<'s> {
    #[must_use]
    pub fn of(scope: &'s ControllerScope<'s>) -> Self {
        Self { scope }
    }

    pub async fn wanted(&self) -> Result<u8, ActionError> {
        Ok(pod_substrate(&self.scope.specification().await?)?.pods)
    }

    pub fn name(&self, index: u8) -> Result<ObjectName, ActionError> {
        self.scope.named(&pod_suffix(index))
    }

    fn machine(&self, index: u8) -> Result<Machine<'s>, ActionError> {
        self.scope.machine(&pod_suffix(index))
    }

    async fn exists(&self, index: u8) -> Result<bool, ActionError> {
        Ok(self.machine(index)?.vm().await?.is_some())
    }

    pub fn address_reference(&self) -> Result<ObjectReference, ActionError> {
        Ok(self.scope.ownership.reference::<FloatingIp>(self.scope.named(LXD_ADDRESS)?))
    }

    async fn identity(&self) -> Result<LxdIdentitySpecification, ActionError> {
        let reference = ObjectReference::of::<LxdIdentity>(None, lxd_identity_name());
        let identity = self.scope.objects().read::<LxdIdentity>(&reference).await?;
        identity
            .map(|identity| identity.specification)
            .ok_or_else(|| ActionError::retryable("Amafu has no LXD identity yet"))
    }

    async fn founder(&self) -> Result<Option<LxdEndpoint>, ActionError> {
        let address = self.scope.objects().read::<FloatingIp>(&self.address_reference()?).await?;
        let Some(address) =
            address.and_then(|address| address.status.binding.map(|_| address.status.address)).flatten()
        else {
            return Ok(None);
        };
        let identity = self.identity().await?;
        Ok(Some(LxdEndpoint {
            address,
            certificate_pem: identity.certificate_pem,
            private_key_pem: identity.private_key_pem,
        }))
    }

    async fn required_founder(&self) -> Result<LxdEndpoint, ActionError> {
        self.founder()
            .await?
            .ok_or_else(|| ActionError::retryable("the founding pod has no reachable address yet"))
    }

    async fn access(&self, name: &ObjectName) -> Result<Option<LxdAccess>, ActionError> {
        let reference = self.scope.ownership.reference::<GuestInventory>(name.clone());
        let inventory = self.scope.objects().read::<GuestInventory>(&reference).await?;
        Ok(inventory.map(|inventory| inventory.specification.lxd))
    }

    pub async fn controller_guest(&self) -> Result<Option<ControllerGuest>, ActionError> {
        let mut inventories = Vec::new();
        for index in 0..self.wanted().await? {
            let name = self.name(index)?;
            if let Some(access) = self.access(&name).await? {
                inventories.push((name, access));
            }
        }
        Ok(controller_guest(&inventories))
    }

    async fn members(&self) -> Result<Vec<String>, ActionError> {
        self.scope.environment.lxd.members(&self.required_founder().await?).await
    }

    async fn missing_members(&self) -> Result<Vec<u8>, ActionError> {
        let members = self.members().await?;
        let mut missing = Vec::new();
        for index in 0..self.wanted().await? {
            if !members.contains(&self.name(index)?.to_string()) {
                missing.push(index);
            }
        }
        Ok(missing)
    }

    async fn surplus(&self) -> Result<Vec<u8>, ActionError> {
        let mut surplus = Vec::new();
        for index in (self.wanted().await?..MAXIMUM_PODS).rev() {
            if self.exists(index).await? {
                surplus.push(index);
            }
        }
        Ok(surplus)
    }

    async fn create(&self, index: u8, role: PodRole<'_>) -> Result<(), ActionError> {
        let substrate = pod_substrate(&self.scope.specification().await?)?;
        let identity = self.identity().await?;
        let name = self.name(index)?;
        let setup = PodSetup {
            name: name.as_str(),
            role,
            trusted_certificate: &identity.certificate_pem,
            amafu_certificate_authority: &self.scope.environment.settings.amafu_certificate_authority,
        };
        self.machine(index)?
            .create(MachinePlan {
                shape: substrate.shape,
                image: ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?,
                network: substrate.network,
                storage_backend: substrate.storage_backend,
                security_groups: vec![self.scope.named(POD_GROUP)?],
                user_data: setup.user_data(),
                labels: BTreeMap::new(),
                guests: GUESTS_PER_POD,
            })
            .await
    }

    fn overdue(&self, what: &str) -> Option<ActionError> {
        self.scope.step_is_overdue(POD_SETUP_DEADLINE).then(|| {
            ActionError::fatal(format!(
                "{what} within {} minutes; each pod keeps its setup log in /root/amafu-pod/setup.log",
                POD_SETUP_DEADLINE.whole_minutes()
            ))
        })
    }
}

macro_rules! pod_step {
    ($name:ident) => {
        pub struct $name {
            pub environment: Arc<JujuEnvironment>,
        }
    };
}

pod_step!(CreateFounderPod);
pod_step!(FormCluster);
pod_step!(RetirePods);

#[async_trait::async_trait]
impl Step<JujuControllerChange> for CreateFounderPod {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope);
        let Some(founder) = pods.founder().await? else {
            return Ok(false);
        };
        if matches!(self.environment.lxd.inspect(&founder).await?, Some(LxdAccess::Trusted { .. })) {
            return Ok(true);
        }
        pods.overdue(&format!("pod {} did not set up LXD", pods.name(0)?)).map_or(Ok(false), Err)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope);
        if !pods.exists(0).await? {
            let bootstrap = if scope.controller().await?.status.uuid.is_none() {
                let admin_password = scope.secret_value(ADMIN_PASSWORD).await?;
                let target = Target::Pods { pods: pods.wanted().await?, admin_password };
                Some(scope.bootstrap_plan(target).await?)
            } else {
                None
            };
            pods.create(0, PodRole::Founder { bootstrap: bootstrap.as_ref() }).await?;
        }
        let target = FloatingIpTarget { vm: pods.name(0)?, interface: 0, guest_address: None };
        scope
            .objects()
            .change_specification::<FloatingIp>(&pods.address_reference()?, |current| {
                (current.target.as_ref() != Some(&target))
                    .then(|| FloatingIpSpecification { target: Some(target.clone()), ..current.clone() })
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for FormCluster {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope);
        let missing = pods.missing_members().await?;
        if missing.is_empty() {
            return Ok(true);
        }
        let names: Vec<String> = missing
            .iter()
            .map(|index| pods.name(*index).map(|name| name.to_string()))
            .collect::<Result<_, _>>()?;
        pods.overdue(&format!("{} did not join the LXD cluster", names.join(", "))).map_or(Ok(false), Err)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope);
        let lxd = &self.environment.lxd;
        let founder = pods.required_founder().await?;
        if lxd.members(&founder).await?.is_empty() {
            lxd.enable_clustering(&founder, pods.name(0)?.as_str()).await?;
        }
        for index in pods.missing_members().await? {
            if index == 0 || pods.exists(index).await? {
                continue;
            }
            let join_token = lxd.invite(&founder, pods.name(index)?.as_str()).await?;
            pods.create(index, PodRole::Member { join_token: &join_token }).await?;
        }
        Ok(())
    }
}

pub struct RecordPodCount {
    pub environment: Arc<JujuEnvironment>,
    pub pods: u8,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RecordPodCount {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(Pods::of(&scope).wanted().await? == self.pods)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let controllers = scope.specification().await?.substrate.controller_count();
        if self.pods < controllers {
            return Err(ActionError::fatal(format!(
                "a controller with {controllers} controller machines needs at least {controllers} pods"
            )));
        }
        let pods = self.pods;
        scope
            .objects()
            .change_specification::<JujuController>(&scope.controller_reference(), |current| {
                let mut updated = current.clone();
                if let Substrate::Pods { pods: recorded, .. } = &mut updated.substrate {
                    *recorded = pods;
                }
                (updated != *current).then_some(updated)
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RetirePods {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(Pods::of(&scope).surplus().await?.is_empty())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope);
        let lxd = &self.environment.lxd;
        let founder = pods.required_founder().await?;
        for index in pods.surplus().await? {
            let name = pods.name(index)?;
            if let Some(LxdAccess::Trusted { instances, .. }) = pods.access(&name).await?
                && !instances.is_empty()
            {
                let hosted: Vec<&str> = instances.iter().map(|instance| instance.name.as_str()).collect();
                return Err(ActionError::fatal(format!(
                    "pod {name} still runs {}; remove the units on it first",
                    hosted.join(", ")
                )));
            }
            if lxd.members(&founder).await?.contains(&name.to_string()) {
                lxd.remove_member(&founder, name.as_str()).await?;
            }
            pods.machine(index)?.remove().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::guest_inventory::{GuestInstance, GuestInstanceKind};

    use super::*;

    fn instance(
        name: &str,
        status: &str,
        is_juju_controller: bool,
        address: Option<Ipv4Addr>,
    ) -> GuestInstance {
        GuestInstance {
            name: String::from(name),
            kind: GuestInstanceKind::Container,
            status: String::from(status),
            location: None,
            addresses: address.into_iter().collect(),
            is_juju_controller,
        }
    }

    fn trusted(instances: Vec<GuestInstance>) -> LxdAccess {
        LxdAccess::Trusted { server: String::from("pod"), is_clustered: true, instances }
    }

    #[test]
    fn the_controller_is_the_first_running_controller_container_with_an_address() {
        let address = Ipv4Addr::new(10, 70, 0, 4);
        let inventories = [
            (ObjectName::new("ctl-pod-0").unwrap(), LxdAccess::Untrusted),
            (
                ObjectName::new("ctl-pod-1").unwrap(),
                trusted(vec![
                    instance("juju-1-0", "Running", true, None),
                    instance("juju-2-0", "Running", false, Some(Ipv4Addr::new(10, 70, 0, 5))),
                ]),
            ),
            (
                ObjectName::new("ctl-pod-2").unwrap(),
                trusted(vec![instance("juju-1-1", "Running", true, Some(address))]),
            ),
        ];
        assert_eq!(
            controller_guest(&inventories),
            Some(ControllerGuest { pod: ObjectName::new("ctl-pod-2").unwrap(), address })
        );
        assert_eq!(controller_guest(&inventories[..2]), None);
    }
}
