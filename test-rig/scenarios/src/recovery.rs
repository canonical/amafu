use std::collections::BTreeMap;
use std::time::Duration;

use amafu_checkers::invariants::at_most_one_running_instance_per_vm;
use amafu_domain::access::Role;
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::{Operation, OperationKind, OperationOutcome, OperationTarget};
use amafu_domain::kinds::vm::{Vm, VmPhase, VmSpecification};
use amafu_domain::{NodeName, ObjectId};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Fault, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure,
    eventually,
};
use futures::FutureExt;

use crate::support::{create_running, delete_all, every_node_ready, running_nodes};

pub const POWERED_OFF_NODE_IS_RECOVERED: ScenarioDefinition = ScenarioDefinition {
    name: "the VMs of a powered-off node run elsewhere within 30 s and never on two nodes",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| the_vms_of_a_powered_off_node_run_elsewhere(cluster).boxed_local(),
};

pub const CUT_OFF_NODE_IS_FENCED: ScenarioDefinition = ScenarioDefinition {
    name: "a node cut off past its lease is fenced before its VMs run elsewhere",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_node_cut_off_past_its_lease_is_fenced(cluster).boxed_local(),
};

const RECOVERY_BUDGET: Duration = Duration::from_secs(30);
const CUT_OFF: Duration = Duration::from_secs(45);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(120);
const RETURN_TIMEOUT: Duration = Duration::from_secs(180);
const QUIET_PERIOD: Duration = Duration::from_secs(5);

fn lost_vms(placed: &BTreeMap<ObjectId, NodeName>, lost: &NodeName) -> Vec<ObjectId> {
    placed.iter().filter(|(_, node)| *node == lost).map(|(vm_id, _)| *vm_id).collect()
}

fn victim(placed: &BTreeMap<ObjectId, NodeName>, nodes: &[NodeName]) -> Result<NodeName, ScenarioError> {
    nodes
        .iter()
        .skip(1)
        .rev()
        .find(|node| placed.values().any(|placed_on| placed_on == *node))
        .cloned()
        .ok_or_else(|| ScenarioError::Expectation(String::from("no VM landed beside the first node")))
}

async fn run_elsewhere(
    cluster: &dyn Cluster,
    vm_ids: &[ObjectId],
    lost: &NodeName,
) -> Result<bool, ScenarioError> {
    let observation = cluster.observe().await?;
    at_most_one_running_instance_per_vm(&observation).map_err(|violation| {
        ScenarioError::Expectation(format!("a VM ran on two nodes during recovery: {violation}"))
    })?;
    Ok(vm_ids.iter().all(|vm_id| {
        observation.running_instances().any(|instance| instance.vm_id == *vm_id && instance.node != *lost)
    }))
}

async fn the_recovery_succeeded(cluster: &dyn Cluster, lost: &NodeName) -> ScenarioResult {
    let operator = client(cluster, Role::Operator).await;
    let operations = operator.list::<Operation>().await?;
    let recovered = operations.iter().any(|operation| {
        operation.specification.kind == OperationKind::Recovery
            && matches!(&operation.specification.target, OperationTarget::Node { node } if node == lost)
            && operation.status.outcome == Some(OperationOutcome::Succeeded)
    });
    ensure(recovered, || format!("no recovery of {lost} succeeded: {operations:?}"))
}

async fn the_vms_of_a_powered_off_node_run_elsewhere(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let nodes = cluster.nodes().await;
    let names: Vec<String> = (0..nodes.len() * 2).map(|index| format!("survivor-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &VmSpecification::small()).await?;
    let placed: BTreeMap<ObjectId, NodeName> =
        vm_ids.iter().copied().zip(running_nodes(cluster, &vm_ids).await?).collect();
    let lost = victim(&placed, &nodes)?;
    let stranded = lost_vms(&placed, &lost);

    let clock = cluster.clock();
    let powered_off_at = clock.now();
    let fault = cluster.inject(Fault::PowerOff { node: lost.clone() }).await?;
    eventually(cluster, RECOVERY_TIMEOUT, "the lost node's VMs run on other nodes", async || {
        run_elsewhere(cluster, &stranded, &lost).await
    })
    .await?;
    let recovered_after = clock.now() - powered_off_at;
    ensure(recovered_after <= RECOVERY_BUDGET, || {
        format!("the VMs of {lost} ran elsewhere after {recovered_after}, over the 30 s budget")
    })?;
    the_recovery_succeeded(cluster, &lost).await?;
    for vm_name in &names {
        let vm = member.get::<Vm>(vm_name).await?;
        ensure(vm.status.node.as_ref() != Some(&lost), || format!("{vm_name} is still assigned to {lost}"))?;
    }

    cluster.heal(fault).await?;
    eventually(cluster, RETURN_TIMEOUT, "the powered-on node is ready again", async || {
        is_ready(cluster, &lost).await
    })
    .await?;
    always_during(cluster, QUIET_PERIOD, at_most_one_running_instance_per_vm).await?;
    for vm_name in &names {
        let vm = member.get::<Vm>(vm_name).await?;
        ensure(vm.status.phase == VmPhase::Running, || {
            format!("{vm_name} is {:?} after the node returned", vm.status.phase)
        })?;
    }
    delete_all(&member, cluster, &names).await
}

async fn is_ready(cluster: &dyn Cluster, node: &NodeName) -> Result<bool, ScenarioError> {
    let operator = client(cluster, Role::Operator).await;
    Ok(operator.get::<Node>(node.as_str()).await?.status.is_ready())
}

async fn a_node_cut_off_past_its_lease_is_fenced(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let nodes = cluster.nodes().await;
    let names: Vec<String> = (0..nodes.len() * 2).map(|index| format!("isolated-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &VmSpecification::small()).await?;
    let placed: BTreeMap<ObjectId, NodeName> =
        vm_ids.iter().copied().zip(running_nodes(cluster, &vm_ids).await?).collect();
    let isolated = victim(&placed, &nodes)?;
    let stranded = lost_vms(&placed, &isolated);

    let fault = cluster.inject(Fault::Partition { node: isolated.clone() }).await?;
    eventually(cluster, RECOVERY_TIMEOUT, "the isolated node's VMs run on other nodes", async || {
        run_elsewhere(cluster, &stranded, &isolated).await
    })
    .await?;
    the_recovery_succeeded(cluster, &isolated).await?;
    always_during(cluster, CUT_OFF, at_most_one_running_instance_per_vm).await?;
    cluster.heal(fault).await?;
    eventually(cluster, RETURN_TIMEOUT, "the isolated node is ready again", async || {
        is_ready(cluster, &isolated).await
    })
    .await?;
    always_during(cluster, QUIET_PERIOD, at_most_one_running_instance_per_vm).await?;
    delete_all(&member, cluster, &names).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedStop {
    pub round: u32,
    pub node: NodeName,
    pub recovered_after: Duration,
}

const SOAK_VM_COUNT: usize = 6;

async fn force_stop_once(
    cluster: &dyn Cluster,
    vm_ids: &[ObjectId],
    round: u32,
) -> Result<ForcedStop, ScenarioError> {
    let nodes = cluster.nodes().await;
    let placed: BTreeMap<ObjectId, NodeName> =
        vm_ids.iter().copied().zip(running_nodes(cluster, vm_ids).await?).collect();
    let candidates: Vec<&NodeName> =
        nodes.iter().skip(1).filter(|node| placed.values().any(|placed_on| placed_on == *node)).collect();
    let stopped = candidates
        .get(usize::try_from(round).unwrap_or_default() % candidates.len().max(1))
        .map(|node| (*node).clone())
        .ok_or_else(|| ScenarioError::Expectation(String::from("no VM runs beside the first node")))?;
    let stranded = lost_vms(&placed, &stopped);
    let clock = cluster.clock();
    let stopped_at = clock.now();
    let fault = cluster.inject(Fault::PowerOff { node: stopped.clone() }).await?;
    eventually(cluster, RECOVERY_TIMEOUT, "the stopped node's VMs run on other nodes", async || {
        run_elsewhere(cluster, &stranded, &stopped).await
    })
    .await?;
    let recovered_after = Duration::try_from(clock.now() - stopped_at).unwrap_or_default();
    cluster.heal(fault).await?;
    eventually(cluster, RETURN_TIMEOUT, "the stopped node is ready again", async || {
        is_ready(cluster, &stopped).await
    })
    .await?;
    ensure(recovered_after <= RECOVERY_BUDGET, || {
        format!("round {round}: the VMs of {stopped} ran elsewhere after {recovered_after:?}, over 30 s")
    })?;
    Ok(ForcedStop { round, node: stopped, recovered_after })
}

pub async fn soak_forced_stops(
    cluster: &dyn Cluster,
    rounds: u32,
    report: &dyn Fn(&Result<ForcedStop, ScenarioError>),
) -> Result<u32, ScenarioError> {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let names: Vec<String> = (0..SOAK_VM_COUNT).map(|index| format!("soak-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &VmSpecification::small()).await?;
    let mut failures = 0;
    for round in 1..=rounds {
        let outcome = force_stop_once(cluster, &vm_ids, round).await;
        if outcome.is_err() {
            failures += 1;
        }
        report(&outcome);
        every_node_ready(cluster).await?;
    }
    delete_all(&member, cluster, &names).await?;
    Ok(failures)
}
