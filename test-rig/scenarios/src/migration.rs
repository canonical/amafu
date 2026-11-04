use std::collections::BTreeMap;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_checkers::invariants::at_most_one_running_instance_per_vm;
use amafu_domain::access::Role;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::network_endpoint::NetworkEndpoint;
use amafu_domain::kinds::node::{Node, NodeSpecification};
use amafu_domain::kinds::operation::{Operation, OperationOutcome, OperationSpecification, VmReference};
use amafu_domain::kinds::vm::{BootSource, MigrationPhase, Vm, VmPhase, VmSpecification};
use amafu_domain::{NodeName, ObjectId, ObjectName, ProjectName};
use amafu_interfaces::WorkloadState;
use amafu_scenario::cluster::{DEFAULT_PROJECT, client};
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure, eventually,
};
use futures::FutureExt;

use crate::networking::{interface, ready_network};
use crate::support::{
    START_TIMEOUT, busybox, create_running, delete_all, delete_and_wait, every_node_ready, instance, name,
    ready_image, running_nodes, vm_in_phase,
};

pub const RUNNING_VM_MOVES: ScenarioDefinition = ScenarioDefinition {
    name: "a running VM moves to another node without ever running twice",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_running_vm_moves_to_another_node(cluster).boxed_local(),
};

pub const NODE_IS_EVACUATED: ScenarioDefinition = ScenarioDefinition {
    name: "an evacuated node is cordoned and left by every VM",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| an_evacuated_node_is_left_by_every_vm(cluster).boxed_local(),
};

pub const LOADED_VM_MOVES_WITHIN_BUDGET: ScenarioDefinition = ScenarioDefinition {
    name: "a 4 GiB VM under memory load moves within its downtime budget",
    levels: &[Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_loaded_vm_moves_within_budget(cluster).boxed_local(),
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(1200);
const SIMULATED_EVACUATED_VM_COUNT: usize = 12;
const LAB_EVACUATED_VM_COUNT: usize = 40;
const EVACUATED_VM_MEMORY_MEBIBYTES: u64 = 128;
const LOADED_VM_MEMORY_MEBIBYTES: u64 = 4096;
const FINAL_ROUND_BUDGET_BYTES: u64 = 8 * 1024 * 1024;
const DOWNTIME_BUDGET_MILLISECONDS: u64 = 300;
const MEMORY_LOAD: &str = "mount -t tmpfs -o size=1200m tmpfs /mnt && while true; do yes amafu-migration-load | head -c 1073741824 > /mnt/load; done";

fn project() -> Result<ProjectName, ScenarioError> {
    ProjectName::new(DEFAULT_PROJECT).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

fn vm_reference(vm_name: &str) -> Result<VmReference, ScenarioError> {
    Ok(VmReference { project: project()?, name: name(vm_name)? })
}

async fn finished_operation(
    operator: &ApiClient,
    cluster: &dyn Cluster,
    operation_name: &str,
) -> Result<OperationOutcome, ScenarioError> {
    eventually(cluster, OPERATION_TIMEOUT, &format!("operation {operation_name} finishes"), async || {
        Ok(operator.get::<Operation>(operation_name).await?.status.is_finished())
    })
    .await?;
    let operation = operator.get::<Operation>(operation_name).await?;
    operation
        .status
        .outcome
        .ok_or_else(|| ScenarioError::Expectation(format!("operation {operation_name} has no outcome")))
}

async fn succeeded(operator: &ApiClient, cluster: &dyn Cluster, operation_name: &str) -> ScenarioResult {
    let outcome = finished_operation(operator, cluster, operation_name).await?;
    ensure(outcome == OperationOutcome::Succeeded, || format!("operation {operation_name} ended {outcome:?}"))
}

async fn a_running_vm_moves_to_another_node(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let operator = client(cluster, Role::Operator).await;
    ready_image(&member, cluster, "busybox-moving", busybox()?).await?;
    ready_network(&member, cluster, "moving", "10.92.0.0/24").await?;
    let mut specification = VmSpecification::container(name("busybox-moving")?);
    specification.network_interfaces = vec![interface("moving", &[])?];
    if let BootSource::Container { command, .. } = &mut specification.boot {
        *command = vec![String::from("sleep"), String::from("3600")];
    }
    let created = member.create::<Vm>(&name("traveller")?, &specification).await?;
    vm_in_phase(&member, cluster, "traveller", VmPhase::Running, START_TIMEOUT).await?;
    let source = instance(cluster, created.id()).await?.node;

    operator
        .create::<Operation>(
            &name("move-traveller")?,
            &OperationSpecification::migration(vm_reference("traveller")?, None),
        )
        .await?;
    succeeded(&operator, cluster, "move-traveller").await?;

    let moved = member.get::<Vm>("traveller").await?;
    let destination = moved
        .status
        .node
        .clone()
        .ok_or_else(|| ScenarioError::Expectation(String::from("the VM lost its node")))?;
    ensure(destination != source, || format!("the VM is still on {source}"))?;
    let migration = moved
        .status
        .migration
        .clone()
        .ok_or_else(|| ScenarioError::Expectation(String::from("the migration left no record")))?;
    ensure(migration.phase == MigrationPhase::Cleaned, || {
        format!("the migration ended at {:?}", migration.phase)
    })?;
    ensure(migration.report.is_some(), || String::from("the migration left no report"))?;
    let running = instance(cluster, created.id()).await?;
    ensure(running.node == destination, || format!("the VM runs on {}, not {destination}", running.node))?;
    ensure(running.workload == Some(WorkloadState::Running), || {
        format!("the workload is {:?} after the move", running.workload)
    })?;
    let endpoints = member.list::<NetworkEndpoint>().await?;
    ensure(
        endpoints
            .iter()
            .filter(|endpoint| endpoint.specification.vm_id == created.id())
            .all(|endpoint| endpoint.specification.node == destination),
        || format!("an endpoint of the VM still names another node: {endpoints:?}"),
    )?;
    always_during(cluster, Duration::from_secs(5), at_most_one_running_instance_per_vm).await?;

    delete_and_wait::<Vm>(&member, cluster, "traveller").await?;
    delete_and_wait::<Operation>(&operator, cluster, "move-traveller").await?;
    delete_and_wait::<Network>(&member, cluster, "moving").await?;
    delete_and_wait::<amafu_domain::kinds::image::Image>(&member, cluster, "busybox-moving").await
}

async fn set_cordoned(operator: &ApiClient, node: &NodeName, is_cordoned: bool) -> ScenarioResult {
    let current = operator.get::<Node>(node.as_str()).await?;
    let specification = NodeSpecification { is_cordoned, ..current.specification };
    operator.update::<Node>(node.as_str(), &specification, None).await?;
    Ok(())
}

async fn roomiest_node(operator: &ApiClient) -> Result<NodeName, ScenarioError> {
    let nodes = operator.list::<Node>().await?;
    let roomiest = nodes
        .iter()
        .max_by_key(|node| (node.status.capacity.memory_mebibytes, node.metadata.name.clone()))
        .ok_or_else(|| ScenarioError::Expectation(String::from("no nodes")))?;
    NodeName::new(roomiest.metadata.name.as_str())
        .map_err(|error| ScenarioError::Expectation(error.to_string()))
}

async fn an_evacuated_node_is_left_by_every_vm(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let operator = client(cluster, Role::Operator).await;
    let nodes = cluster.nodes().await;
    let evacuated = roomiest_node(&operator).await?;
    for other in nodes.iter().filter(|node| **node != evacuated) {
        set_cordoned(&operator, other, true).await?;
    }
    let count = match cluster.level() {
        Level::Simulated => SIMULATED_EVACUATED_VM_COUNT,
        _ => LAB_EVACUATED_VM_COUNT,
    };
    let names: Vec<String> = (0..count).map(|index| format!("tenant-{index}")).collect();
    let small =
        VmSpecification { memory_mebibytes: EVACUATED_VM_MEMORY_MEBIBYTES, ..VmSpecification::small() };
    let created = create_running(&member, cluster, &names, &small).await;
    for other in nodes.iter().filter(|node| **node != evacuated) {
        set_cordoned(&operator, other, false).await?;
    }
    let vm_ids = created?;
    let placed: BTreeMap<ObjectId, NodeName> =
        vm_ids.iter().copied().zip(running_nodes(cluster, &vm_ids).await?).collect();
    ensure(placed.values().all(|node| *node == evacuated), || {
        format!("the VMs did not all start on {evacuated}: {placed:?}")
    })?;

    let vms = names.iter().map(|vm_name| vm_reference(vm_name)).collect::<Result<Vec<_>, _>>()?;
    let operation_name: ObjectName = name("evacuate-last-node")?;
    operator
        .create::<Operation>(&operation_name, &OperationSpecification::evacuation(evacuated.clone(), vms))
        .await?;
    succeeded(&operator, cluster, operation_name.as_str()).await?;

    let node = operator.get::<Node>(evacuated.as_str()).await?;
    ensure(node.specification.is_cordoned, || format!("{evacuated} is not cordoned after its evacuation"))?;
    let moved = running_nodes(cluster, &vm_ids).await?;
    ensure(moved.iter().all(|node| *node != evacuated), || {
        format!("a VM still runs on {evacuated}: {moved:?}")
    })?;
    always_during(cluster, Duration::from_secs(5), at_most_one_running_instance_per_vm).await?;

    set_cordoned(&operator, &evacuated, false).await?;
    delete_all(&member, cluster, &names).await?;
    for operation in operator.list::<Operation>().await? {
        delete_and_wait::<Operation>(&operator, cluster, operation.metadata.name.as_str()).await?;
    }
    Ok(())
}

async fn a_loaded_vm_moves_within_budget(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let operator = client(cluster, Role::Operator).await;
    ready_image(&member, cluster, "busybox-heavy", busybox()?).await?;
    let mut specification = VmSpecification::container(name("busybox-heavy")?);
    specification.memory_mebibytes = LOADED_VM_MEMORY_MEBIBYTES;
    if let BootSource::Container { command, .. } = &mut specification.boot {
        *command = vec![String::from("sh"), String::from("-c"), MEMORY_LOAD.to_owned()];
    }
    let created = member.create::<Vm>(&name("heavy")?, &specification).await?;
    vm_in_phase(&member, cluster, "heavy", VmPhase::Running, START_TIMEOUT).await?;
    cluster.clock().sleep(Duration::from_secs(20)).await;
    let source = instance(cluster, created.id()).await?.node;

    operator
        .create::<Operation>(
            &name("move-heavy")?,
            &OperationSpecification::migration(vm_reference("heavy")?, None),
        )
        .await?;
    succeeded(&operator, cluster, "move-heavy").await?;

    let moved = member.get::<Vm>("heavy").await?;
    let report = moved
        .status
        .migration
        .as_ref()
        .and_then(|migration| migration.report)
        .ok_or_else(|| ScenarioError::Expectation(String::from("the migration left no report")))?;
    ensure(moved.status.node.as_ref() != Some(&source), || format!("the VM is still on {source}"))?;
    ensure(report.final_round_bytes <= FINAL_ROUND_BUDGET_BYTES, || {
        format!("{} bytes were left for the final pause, over 8 MiB: {report:?}", report.final_round_bytes)
    })?;
    ensure(report.downtime_milliseconds <= DOWNTIME_BUDGET_MILLISECONDS, || {
        format!("the VM paused for {} ms, over 300 ms: {report:?}", report.downtime_milliseconds)
    })?;
    let running = instance(cluster, created.id()).await?;
    ensure(running.workload == Some(WorkloadState::Running), || {
        format!("the workload is {:?} after the move", running.workload)
    })?;
    always_during(cluster, Duration::from_secs(5), at_most_one_running_instance_per_vm).await?;

    delete_and_wait::<Vm>(&member, cluster, "heavy").await?;
    delete_and_wait::<Operation>(&operator, cluster, "move-heavy").await?;
    delete_and_wait::<amafu_domain::kinds::image::Image>(&member, cluster, "busybox-heavy").await
}
