use std::time::Duration;

use super::{ContractViolation, ensure, succeed};
use crate::hypervisor::{Hypervisor, VmLaunchSpecification};

const GRACE_PERIOD: Duration = Duration::from_secs(5);

pub async fn hypervisor_contract(
    hypervisor: &dyn Hypervisor,
    launch: &VmLaunchSpecification,
) -> Result<(), ContractViolation> {
    let before = succeed(hypervisor.running().await, "listing works before any start")?;
    ensure(
        before.iter().all(|instance| instance.vm_id != launch.vm_id),
        "the VM under test is not running yet",
        &before,
    )?;

    succeed(hypervisor.start(launch).await, "start succeeds")?;
    succeed(hypervisor.start(launch).await, "start is idempotent")?;
    let after_start = succeed(hypervisor.running().await, "listing works after start")?;
    let instances = after_start.iter().filter(|instance| instance.vm_id == launch.vm_id).count();
    ensure(instances == 1, "one instance after two starts", &after_start)?;

    succeed(hypervisor.stop(&launch.vm_id, GRACE_PERIOD).await, "stop succeeds")?;
    succeed(hypervisor.stop(&launch.vm_id, GRACE_PERIOD).await, "stop is idempotent")?;
    let after_stop = succeed(hypervisor.running().await, "listing works after stop")?;
    ensure(
        after_stop.iter().all(|instance| instance.vm_id != launch.vm_id),
        "no instance after stop",
        &after_stop,
    )
}
