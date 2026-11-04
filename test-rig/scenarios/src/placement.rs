use std::collections::BTreeSet;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_domain::NodeName;
use amafu_domain::access::Role;
use amafu_domain::kinds::node::{Node, NodeSpecification};
use amafu_domain::kinds::vm::{Vm, VmPhase, VmSpecification};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Fault, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure, eventually,
};
use futures::FutureExt;

use crate::support::{
    START_TIMEOUT, create_running, delete_all, every_node_ready, instance, name, running_nodes,
};

pub const ANTI_AFFINITY_SPREADS_VMS: ScenarioDefinition = ScenarioDefinition {
    name: "members of an anti-affinity group run on different nodes",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| members_of_an_anti_affinity_group_run_on_different_nodes(cluster).boxed_local(),
};

pub const CORDONED_NODE_IS_AVOIDED: ScenarioDefinition = ScenarioDefinition {
    name: "a cordoned node receives no new VMs",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_cordoned_node_receives_no_new_vms(cluster).boxed_local(),
};

pub const NODE_KEEPS_SERVING_WITHOUT_THE_CORE: ScenarioDefinition = ScenarioDefinition {
    name: "a node outside the core keeps its VMs and their addresses while cut off",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_node_keeps_its_vms_while_cut_off(cluster).boxed_local(),
};

const GROUP: &str = "spread";
const CUT_OFF: Duration = Duration::from_secs(5);

fn grouped() -> Result<VmSpecification, ScenarioError> {
    Ok(VmSpecification { anti_affinity_group: Some(name(GROUP)?), ..VmSpecification::small() })
}

async fn members_of_an_anti_affinity_group_run_on_different_nodes(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let node_count = cluster.nodes().await.len();
    let names: Vec<String> = (0..node_count).map(|index| format!("spread-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &grouped()?).await?;
    let nodes: BTreeSet<NodeName> = running_nodes(cluster, &vm_ids).await?.into_iter().collect();
    ensure(nodes.len() == node_count, || {
        format!("the group runs on {} distinct nodes, not {node_count}", nodes.len())
    })?;
    member.create::<Vm>(&name("spread-extra")?, &grouped()?).await?;
    eventually(cluster, START_TIMEOUT, "the extra member waits with the reason", async || {
        let extra = member.get::<Vm>("spread-extra").await?;
        Ok(extra.status.phase == VmPhase::Pending
            && extra.status.message.as_deref().is_some_and(|message| message.contains("anti-affinity group")))
    })
    .await?;
    let mut all = names;
    all.push(String::from("spread-extra"));
    delete_all(&member, cluster, &all).await
}

async fn set_cordoned(operator: &ApiClient, node: &NodeName, is_cordoned: bool) -> ScenarioResult {
    let current = operator.get::<Node>(node.as_str()).await?;
    let specification = NodeSpecification { is_cordoned, ..current.specification };
    operator.update::<Node>(node.as_str(), &specification, None).await?;
    Ok(())
}

async fn a_cordoned_node_receives_no_new_vms(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let operator = client(cluster, Role::Operator).await;
    let member = client(cluster, Role::Member).await;
    let nodes = cluster.nodes().await;
    let cordoned =
        nodes.get(1).cloned().ok_or_else(|| ScenarioError::Expectation(String::from("too few nodes")))?;
    set_cordoned(&operator, &cordoned, true).await?;
    let names: Vec<String> = (0..nodes.len() * 2).map(|index| format!("busy-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &VmSpecification::small()).await?;
    let placed = running_nodes(cluster, &vm_ids).await?;
    ensure(!placed.contains(&cordoned), || format!("{cordoned} is cordoned but runs a new VM: {placed:?}"))?;
    set_cordoned(&operator, &cordoned, false).await?;
    delete_all(&member, cluster, &names).await
}

async fn a_node_keeps_its_vms_while_cut_off(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    let nodes = cluster.nodes().await;
    let outside =
        nodes.last().cloned().ok_or_else(|| ScenarioError::Expectation(String::from("no nodes")))?;
    let names: Vec<String> = (0..nodes.len()).map(|index| format!("steady-{index}")).collect();
    let vm_ids = create_running(&member, cluster, &names, &grouped()?).await?;
    let placed = running_nodes(cluster, &vm_ids).await?;
    let (vm_id, _) = vm_ids
        .iter()
        .zip(&placed)
        .find(|(_, node)| **node == outside)
        .ok_or_else(|| ScenarioError::Expectation(format!("no VM landed on {outside}")))?;
    let fault = cluster.inject(Fault::Partition { node: outside.clone() }).await?;
    let clock = cluster.clock();
    let deadline = clock.now() + CUT_OFF;
    while clock.now() < deadline {
        let observed = instance(cluster, *vm_id).await?;
        ensure(observed.node == outside, || format!("the VM moved off {outside} while it was cut off"))?;
        clock.sleep(Duration::from_secs(1)).await;
    }
    cluster.heal(fault).await?;
    delete_all(&member, cluster, &names).await
}
