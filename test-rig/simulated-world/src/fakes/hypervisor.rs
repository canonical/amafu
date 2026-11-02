use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amafu_domain::{ObjectId, ObjectName};
use amafu_interfaces::{
    ActionError, Clock, GuestAgents, Hypervisor, InstanceState, Payload, RunningInstance,
    VmLaunchSpecification, WorkloadSpecification, WorkloadState,
};
use time::OffsetDateTime;

use crate::lock::lock;

#[derive(Debug, Clone)]
pub struct FakeInstance {
    pub launch: VmLaunchSpecification,
    pub started_at: OffsetDateTime,
    pub workload: Option<WorkloadSpecification>,
    pub exit_code: Option<i32>,
}

pub struct FakeHypervisor {
    clock: Arc<dyn Clock>,
    boot_duration: Duration,
    bundles: BTreeSet<ObjectName>,
    instances: Mutex<BTreeMap<ObjectId, FakeInstance>>,
}

impl FakeHypervisor {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, boot_duration: Duration, bundles: BTreeSet<ObjectName>) -> Self {
        Self { clock, boot_duration, bundles, instances: Mutex::new(BTreeMap::new()) }
    }

    pub fn power_off(&self) {
        lock(&self.instances).clear();
    }

    fn state_of(&self, instance: &FakeInstance) -> InstanceState {
        if self.clock.now() - instance.started_at >= self.boot_duration {
            InstanceState::Running
        } else {
            InstanceState::Booting
        }
    }

    #[must_use]
    pub fn instances(&self) -> Vec<(RunningInstance, FakeInstance)> {
        lock(&self.instances)
            .iter()
            .map(|(vm_id, instance)| {
                (RunningInstance { vm_id: *vm_id, state: self.state_of(instance) }, instance.clone())
            })
            .collect()
    }

    pub fn exit_workload(&self, vm_id: &ObjectId, code: i32) {
        if let Some(instance) = lock(&self.instances).get_mut(vm_id)
            && instance.workload.is_some()
        {
            instance.exit_code = Some(code);
        }
    }
}

#[async_trait::async_trait]
impl Hypervisor for FakeHypervisor {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        if let Payload::Bundle { name } = &launch.payload
            && !self.bundles.contains(name)
        {
            return Err(ActionError::invalid(format!("bundle {name} is not available on this node")));
        }
        let started_at = self.clock.now();
        lock(&self.instances).entry(launch.vm_id).or_insert(FakeInstance {
            launch: launch.clone(),
            started_at,
            workload: None,
            exit_code: None,
        });
        Ok(())
    }

    async fn stop(&self, vm_id: &ObjectId, _grace_period: Duration) -> Result<(), ActionError> {
        lock(&self.instances).remove(vm_id);
        Ok(())
    }

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError> {
        Ok(self.instances().into_iter().map(|(running, _)| running).collect())
    }
}

#[async_trait::async_trait]
impl GuestAgents for FakeHypervisor {
    async fn workload(&self, vm_id: &ObjectId) -> Result<WorkloadState, ActionError> {
        let instances = lock(&self.instances);
        let instance = instances.get(vm_id).ok_or_else(|| ActionError::retryable("the VM is not running"))?;
        if !instance.launch.has_guest_agent {
            return Err(ActionError::invalid("this VM has no guest agent"));
        }
        if self.state_of(instance) != InstanceState::Running {
            return Err(ActionError::retryable("the guest agent is not up yet"));
        }
        Ok(match (&instance.workload, instance.exit_code) {
            (_, Some(code)) => WorkloadState::Exited { code },
            (Some(_), None) => WorkloadState::Running,
            (None, None) => WorkloadState::NotStarted,
        })
    }

    async fn start_workload(
        &self,
        vm_id: &ObjectId,
        workload: &WorkloadSpecification,
    ) -> Result<(), ActionError> {
        let mut instances = lock(&self.instances);
        let instance =
            instances.get_mut(vm_id).ok_or_else(|| ActionError::retryable("the VM is not running"))?;
        instance.workload.get_or_insert_with(|| workload.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::contract::hypervisor::hypervisor_contract;

    use super::*;
    use crate::clock::VirtualClock;

    #[test]
    fn the_fake_hypervisor_honours_the_hypervisor_contract() {
        let bundle = ObjectName::new("test-guest").unwrap();
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let hypervisor =
            FakeHypervisor::new(Arc::new(clock), Duration::from_secs(1), BTreeSet::from([bundle.clone()]));
        let launch =
            VmLaunchSpecification::of_bundle(ObjectId::from_uuid(uuid::Uuid::new_v4()), 1, 256, bundle);
        assert_eq!(futures::executor::block_on(hypervisor_contract(&hypervisor, &launch)), Ok(()));
    }
}
