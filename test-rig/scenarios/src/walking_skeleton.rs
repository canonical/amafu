use std::time::Duration;

use amafu_api::ApiClient;
use amafu_checkers::invariants::at_most_one_running_instance_per_vm;
use amafu_domain::access::Role;
use amafu_domain::kinds::vm::{Vm, VmPhase, VmSpecification};
use amafu_domain::{ObjectId, ObjectName};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Fault, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure,
    ensure_equal, eventually,
};
use futures::FutureExt;

const START_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);

pub const DEFINITION: ScenarioDefinition = ScenarioDefinition {
    name: "a VM created from the API runs and is removed after deletion",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_vm_created_from_the_api_runs_and_is_removed_after_deletion(cluster).boxed_local(),
};

pub const RESTART_AFTER_POWER_CYCLE: ScenarioDefinition = ScenarioDefinition {
    name: "a VM runs again after its node is power cycled",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_vm_runs_again_after_its_node_is_power_cycled(cluster).boxed_local(),
};

fn vm_name(name: &str) -> Result<ObjectName, ScenarioError> {
    ObjectName::new(name).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

async fn wait_until_running(cluster: &dyn Cluster, vm_id: ObjectId) -> ScenarioResult {
    eventually(cluster, START_TIMEOUT, "the VM runs on a node", async || {
        Ok(cluster.observe().await?.is_running(vm_id))
    })
    .await
}

async fn wait_until_reported(
    operator: &ApiClient,
    cluster: &dyn Cluster,
    name: &str,
    phase: VmPhase,
) -> ScenarioResult {
    eventually(cluster, START_TIMEOUT, "the API reports the VM phase", async || {
        let vm = operator.get::<Vm>(name).await?;
        Ok(vm.status.phase == phase && vm.is_status_current())
    })
    .await
}

async fn a_vm_created_from_the_api_runs_and_is_removed_after_deletion(
    cluster: &dyn Cluster,
) -> ScenarioResult {
    let operator = client(cluster, Role::Operator).await;
    let name = vm_name("web-1")?;
    let created = operator.create::<Vm>(&name, &VmSpecification::small()).await?;
    ensure_equal(&created.metadata.generation, &1, || String::from("a new VM starts at generation 1"))?;

    wait_until_running(cluster, created.id()).await?;
    wait_until_reported(&operator, cluster, name.as_str(), VmPhase::Running).await?;
    let running = operator.get::<Vm>(name.as_str()).await?;
    let nodes = cluster.nodes().await;
    ensure(running.status.node.as_ref().is_some_and(|node| nodes.contains(node)), || {
        format!("the VM reports a known node, got {:?}", running.status.node)
    })?;
    always_during(cluster, Duration::from_secs(5), at_most_one_running_instance_per_vm).await?;

    operator.delete::<Vm>(name.as_str()).await?;
    eventually(cluster, STOP_TIMEOUT, "the VM stops and its object is removed", async || {
        let is_stopped =
            !cluster.observe().await?.running_instances().any(|instance| instance.vm_id == created.id());
        let is_removed = match operator.get::<Vm>(name.as_str()).await {
            Err(error) if error.is_not_found() => true,
            Err(error) => return Err(error.into()),
            Ok(_) => false,
        };
        Ok(is_stopped && is_removed)
    })
    .await
}

async fn a_vm_runs_again_after_its_node_is_power_cycled(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    let name = vm_name("database-1")?;
    let created = member.create::<Vm>(&name, &VmSpecification::small()).await?;
    wait_until_running(cluster, created.id()).await?;

    let node = cluster
        .observe()
        .await?
        .node_running(created.id())
        .cloned()
        .ok_or_else(|| ScenarioError::Expectation(String::from("the running VM has no node")))?;
    let fault = cluster.inject(Fault::PowerCycle { node }).await?;
    cluster.heal(fault).await?;

    wait_until_running(cluster, created.id()).await?;
    always_during(cluster, Duration::from_secs(5), at_most_one_running_instance_per_vm).await
}

pub const MISSING_IMAGE_IS_REPORTED: ScenarioDefinition = ScenarioDefinition {
    name: "a VM whose image is missing is reported failed with the reason",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_vm_whose_image_is_missing_is_reported_failed(cluster).boxed_local(),
};

async fn a_vm_whose_image_is_missing_is_reported_failed(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    let name = vm_name("orphan-1")?;
    let specification = VmSpecification { image: vm_name("no-such-image")?, ..VmSpecification::small() };
    let created = member.create::<Vm>(&name, &specification).await?;
    eventually(cluster, START_TIMEOUT, "the VM is reported failed", async || {
        let vm = member.get::<Vm>(name.as_str()).await?;
        Ok(vm.status.phase == VmPhase::Failed && vm.is_status_current())
    })
    .await?;
    let failed = member.get::<Vm>(name.as_str()).await?;
    ensure(
        failed.status.message.as_deref().is_some_and(|message| message.contains("no-such-image")),
        || format!("the failure names the missing image, got {:?}", failed.status.message),
    )?;
    ensure(
        !cluster.observe().await?.running_instances().any(|instance| instance.vm_id == created.id()),
        || String::from("a failed VM has no running instance"),
    )
}
