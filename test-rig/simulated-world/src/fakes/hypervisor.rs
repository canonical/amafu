use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amafu_domain::{ObjectId, ObjectName};
use amafu_interfaces::{
    ActionError, Clock, Hypervisor, InstanceState, RunningInstance, VmLaunchSpecification,
};
use time::OffsetDateTime;

use crate::lock::lock;

struct FakeInstance {
    started_at: OffsetDateTime,
}

pub struct FakeHypervisor {
    clock: Arc<dyn Clock>,
    boot_duration: Duration,
    images: BTreeSet<ObjectName>,
    instances: Mutex<BTreeMap<ObjectId, FakeInstance>>,
}

impl FakeHypervisor {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, boot_duration: Duration, images: BTreeSet<ObjectName>) -> Self {
        Self { clock, boot_duration, images, instances: Mutex::new(BTreeMap::new()) }
    }

    pub fn power_off(&self) {
        lock(&self.instances).clear();
    }

    #[must_use]
    pub fn instances(&self) -> Vec<RunningInstance> {
        let now = self.clock.now();
        lock(&self.instances)
            .iter()
            .map(|(vm_id, instance)| RunningInstance {
                vm_id: *vm_id,
                state: if now - instance.started_at >= self.boot_duration {
                    InstanceState::Running
                } else {
                    InstanceState::Booting
                },
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Hypervisor for FakeHypervisor {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        if !self.images.contains(&launch.image) {
            return Err(ActionError::invalid(format!(
                "image {} is not available on this node",
                launch.image
            )));
        }
        let started_at = self.clock.now();
        lock(&self.instances).entry(launch.vm_id).or_insert(FakeInstance { started_at });
        Ok(())
    }

    async fn stop(&self, vm_id: &ObjectId, _grace_period: Duration) -> Result<(), ActionError> {
        lock(&self.instances).remove(vm_id);
        Ok(())
    }

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError> {
        Ok(self.instances())
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::contract::hypervisor::hypervisor_contract;

    use super::*;
    use crate::clock::VirtualClock;

    #[test]
    fn the_fake_hypervisor_honours_the_hypervisor_contract() {
        let image = ObjectName::new("test-guest").unwrap();
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let hypervisor =
            FakeHypervisor::new(Arc::new(clock), Duration::from_secs(1), BTreeSet::from([image.clone()]));
        let launch = VmLaunchSpecification {
            vm_id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            cpu_count: 1,
            memory_mebibytes: 256,
            image,
        };
        assert_eq!(futures::executor::block_on(hypervisor_contract(&hypervisor, &launch)), Ok(()));
    }
}
