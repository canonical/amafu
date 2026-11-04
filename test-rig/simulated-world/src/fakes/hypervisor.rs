use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amafu_domain::kinds::vm::MigrationReport;
use amafu_domain::{ObjectId, ObjectName};
use amafu_interfaces::{
    ActionError, Clock, GuestAgents, Hypervisor, InstanceMigration, InstanceState, MigrationTarget, Payload,
    RunningInstance, VmLaunchSpecification, WorkloadSpecification, WorkloadState,
};
use time::OffsetDateTime;

use crate::fakes::partition::NodeLink;
use crate::lock::lock;

const MEBIBYTE: u64 = 1024 * 1024;
const TRANSFER_MEBIBYTES_PER_SECOND: u64 = 1024;
const FINAL_ROUND_BYTES: u64 = 2 * MEBIBYTE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeMigration {
    Receiving,
    Sending { attempt: ObjectId, destination: Ipv4Addr, completes_at: OffsetDateTime },
    Sent { attempt: ObjectId, report: MigrationReport },
    Failed { attempt: ObjectId, reason: String },
}

#[derive(Debug, Clone)]
pub struct FakeInstance {
    pub launch: VmLaunchSpecification,
    pub started_at: OffsetDateTime,
    pub workload: Option<WorkloadSpecification>,
    pub exit_code: Option<i32>,
    pub migration: Option<FakeMigration>,
    pub guest_boot: u64,
}

type Instances = Arc<Mutex<BTreeMap<ObjectId, FakeInstance>>>;

#[derive(Default)]
pub struct MigrationFabric {
    hosts: Mutex<BTreeMap<Ipv4Addr, (Instances, Arc<NodeLink>)>>,
    guest_boots: AtomicU64,
}

impl MigrationFabric {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn join(&self, address: Ipv4Addr, instances: Instances, link: Arc<NodeLink>) {
        lock(&self.hosts).insert(address, (instances, link));
    }

    fn host(&self, address: Ipv4Addr) -> Option<(Instances, Arc<NodeLink>)> {
        lock(&self.hosts).get(&address).cloned()
    }

    fn next_guest_boot(&self) -> u64 {
        self.guest_boots.fetch_add(1, Ordering::Relaxed) + 1
    }
}

pub struct FakeHypervisor {
    clock: Arc<dyn Clock>,
    boot_duration: Duration,
    bundles: BTreeSet<ObjectName>,
    instances: Instances,
    fabric: Arc<MigrationFabric>,
    link: Arc<NodeLink>,
}

impl FakeHypervisor {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, boot_duration: Duration, bundles: BTreeSet<ObjectName>) -> Self {
        Self::on_fabric(
            clock,
            boot_duration,
            bundles,
            MigrationFabric::new(),
            Ipv4Addr::UNSPECIFIED,
            NodeLink::new(),
        )
    }

    #[must_use]
    pub fn on_fabric(
        clock: Arc<dyn Clock>,
        boot_duration: Duration,
        bundles: BTreeSet<ObjectName>,
        fabric: Arc<MigrationFabric>,
        address: Ipv4Addr,
        link: Arc<NodeLink>,
    ) -> Self {
        let instances: Instances = Arc::new(Mutex::new(BTreeMap::new()));
        fabric.join(address, Arc::clone(&instances), Arc::clone(&link));
        Self { clock, boot_duration, bundles, instances, fabric, link }
    }

    pub fn power_off(&self) {
        lock(&self.instances).clear();
    }

    fn state_of(&self, instance: &FakeInstance) -> InstanceState {
        match instance.migration {
            Some(FakeMigration::Receiving) => InstanceState::Booting,
            Some(FakeMigration::Sent { .. }) => InstanceState::Exited,
            _ if self.clock.now() - instance.started_at >= self.boot_duration => InstanceState::Running,
            _ => InstanceState::Booting,
        }
    }

    fn finish_due_migrations(&self) {
        let now = self.clock.now();
        let due: Vec<(ObjectId, ObjectId, Ipv4Addr)> = lock(&self.instances)
            .iter()
            .filter_map(|(vm_id, instance)| match instance.migration {
                Some(FakeMigration::Sending { attempt, destination, completes_at })
                    if now >= completes_at =>
                {
                    Some((*vm_id, attempt, destination))
                }
                _ => None,
            })
            .collect();
        for (vm_id, attempt, destination) in due {
            let outcome = self.hand_over(&vm_id, attempt, destination, now);
            if let Some(instance) = lock(&self.instances).get_mut(&vm_id) {
                instance.migration = Some(outcome);
            }
        }
    }

    fn hand_over(
        &self,
        vm_id: &ObjectId,
        attempt: ObjectId,
        destination: Ipv4Addr,
        now: OffsetDateTime,
    ) -> FakeMigration {
        let failed = |reason: &str| FakeMigration::Failed { attempt, reason: reason.to_owned() };
        let Some((remote, remote_link)) = self.fabric.host(destination) else {
            return failed("no node answers at the destination address");
        };
        if self.link.is_cut() || remote_link.is_cut() {
            return failed("the migration connection was cut");
        }
        let Some(source) = lock(&self.instances).get(vm_id).cloned() else {
            return failed("the VM stopped while it was being sent");
        };
        let mut remote_instances = lock(&remote);
        let Some(incoming) = remote_instances.get_mut(vm_id) else {
            return failed("the destination is not receiving this VM");
        };
        if incoming.migration != Some(FakeMigration::Receiving) {
            return failed("the destination is not receiving this VM");
        }
        incoming.migration = None;
        incoming.started_at = now - self.boot_duration;
        incoming.workload = source.workload;
        incoming.exit_code = source.exit_code;
        incoming.guest_boot = source.guest_boot;
        let total_bytes = source.launch.memory_mebibytes * MEBIBYTE;
        FakeMigration::Sent {
            attempt,
            report: MigrationReport {
                total_bytes,
                final_round_bytes: FINAL_ROUND_BYTES,
                duration_milliseconds: transfer_time(source.launch.memory_mebibytes)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                downtime_milliseconds: 2,
            },
        }
    }

    fn reported_migration(instance: &FakeInstance) -> Option<InstanceMigration> {
        instance.migration.as_ref().map(|migration| match migration {
            FakeMigration::Receiving => InstanceMigration::Receiving,
            FakeMigration::Sending { attempt, .. } => InstanceMigration::Sending { attempt: *attempt },
            FakeMigration::Sent { attempt, report } => {
                InstanceMigration::Sent { attempt: *attempt, report: *report }
            }
            FakeMigration::Failed { attempt, reason } => {
                InstanceMigration::Failed { attempt: *attempt, reason: reason.clone() }
            }
        })
    }

    #[must_use]
    pub fn instances(&self) -> Vec<(RunningInstance, FakeInstance)> {
        self.finish_due_migrations();
        lock(&self.instances)
            .iter()
            .map(|(vm_id, instance)| {
                let running = RunningInstance::new(*vm_id, self.state_of(instance))
                    .migrating(Self::reported_migration(instance));
                (running, instance.clone())
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

    fn check_bundle(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        if let Payload::Bundle { name } = &launch.payload
            && !self.bundles.contains(name)
        {
            return Err(ActionError::invalid(format!("bundle {name} is not available on this node")));
        }
        Ok(())
    }

    fn insert(&self, launch: &VmLaunchSpecification, migration: Option<FakeMigration>) {
        let started_at = self.clock.now();
        let guest_boot = if migration.is_some() { 0 } else { self.fabric.next_guest_boot() };
        lock(&self.instances).entry(launch.vm_id).or_insert(FakeInstance {
            launch: launch.clone(),
            started_at,
            workload: None,
            exit_code: None,
            migration,
            guest_boot,
        });
    }
}

fn transfer_time(memory_mebibytes: u64) -> Duration {
    Duration::from_millis(memory_mebibytes * 1000 / TRANSFER_MEBIBYTES_PER_SECOND + 100)
}

#[async_trait::async_trait]
impl Hypervisor for FakeHypervisor {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        self.check_bundle(launch)?;
        self.insert(launch, None);
        Ok(())
    }

    async fn stop(&self, vm_id: &ObjectId, _grace_period: Duration) -> Result<(), ActionError> {
        lock(&self.instances).remove(vm_id);
        Ok(())
    }

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError> {
        Ok(self.instances().into_iter().map(|(running, _)| running).collect())
    }

    async fn receive(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        self.check_bundle(launch)?;
        self.insert(launch, Some(FakeMigration::Receiving));
        Ok(())
    }

    async fn send(&self, vm_id: &ObjectId, target: &MigrationTarget) -> Result<(), ActionError> {
        let now = self.clock.now();
        let mut instances = lock(&self.instances);
        let instance =
            instances.get_mut(vm_id).ok_or_else(|| ActionError::retryable("the VM is not running"))?;
        let is_this_attempt = match &instance.migration {
            Some(FakeMigration::Sending { attempt, .. } | FakeMigration::Sent { attempt, .. }) => {
                *attempt == target.attempt
            }
            _ => false,
        };
        if is_this_attempt {
            return Ok(());
        }
        instance.migration = Some(FakeMigration::Sending {
            attempt: target.attempt,
            destination: target.node_address,
            completes_at: now + transfer_time(instance.launch.memory_mebibytes),
        });
        Ok(())
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

    #[test]
    fn a_sent_vm_runs_on_the_destination_and_exits_on_the_source() {
        let bundle = ObjectName::new("test-guest").unwrap();
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let fabric = MigrationFabric::new();
        let host = |octet: u8| {
            FakeHypervisor::on_fabric(
                Arc::new(clock.clone()),
                Duration::ZERO,
                BTreeSet::from([bundle.clone()]),
                Arc::clone(&fabric),
                Ipv4Addr::new(192, 0, 2, octet),
                NodeLink::new(),
            )
        };
        let (source, destination) = (host(10), host(11));
        let launch = VmLaunchSpecification::of_bundle(
            ObjectId::from_uuid(uuid::Uuid::new_v4()),
            1,
            256,
            bundle.clone(),
        );
        futures::executor::block_on(source.start(&launch)).unwrap();
        futures::executor::block_on(destination.receive(&launch)).unwrap();
        let target = MigrationTarget {
            attempt: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            node: amafu_domain::NodeName::new("node-11").unwrap(),
            node_address: Ipv4Addr::new(192, 0, 2, 11),
            downtime_milliseconds: 100,
        };
        futures::executor::block_on(source.send(&launch.vm_id, &target)).unwrap();
        let mut sleeping = clock.sleep(Duration::from_secs(1));
        assert!(futures::FutureExt::now_or_never(&mut sleeping).is_none());
        assert!(clock.advance_to_next_timer());
        let sent = futures::executor::block_on(source.running()).unwrap();
        assert_eq!(sent[0].state, InstanceState::Exited);
        assert!(matches!(sent[0].migration, Some(InstanceMigration::Sent { .. })));
        let received = futures::executor::block_on(destination.running()).unwrap();
        assert_eq!(received[0].state, InstanceState::Running);
        assert_eq!(received[0].migration, None);
    }
}
