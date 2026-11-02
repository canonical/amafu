use amafu_domain::access::Role;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::vm::{Vm, VmSpecification};
use amafu_scenario::cluster::client;
use amafu_scenario::{Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure};
use futures::FutureExt;

use crate::networking::{interface, ready_network};
use crate::support::{
    VM_START_BUDGET, console_shows, delete_and_wait, instance, name, started_within_budget,
};

pub const HOSTILE_GUEST_IS_CONFINED: ScenarioDefinition = ScenarioDefinition {
    name: "a hostile guest reaches neither the node nor anything its VM process may not touch",
    levels: &[Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_hostile_guest_is_confined(cluster).boxed_local(),
};

const NODE_API_PORT: u16 = 7443;

fn hostile_script(gateway: &str) -> String {
    format!(
        "#!/bin/sh\nif nc -w 3 {gateway} {NODE_API_PORT} </dev/null; then echo HOSTILE-REACHED-NODE; fi\nif nc -w 3 10.215.2.50 {NODE_API_PORT} </dev/null; then echo HOSTILE-REACHED-UNDERLAY; fi\necho HOSTILE-DONE\n"
    )
}

async fn a_hostile_guest_is_confined(cluster: &dyn Cluster) -> ScenarioResult {
    let checks = cluster
        .sandbox_checks()
        .await?
        .ok_or_else(|| ScenarioError::Expectation(String::from("this cluster cannot probe the sandbox")))?;
    ensure(!checks.is_empty(), || String::from("the node ran no sandbox checks"))?;
    for check in &checks {
        ensure(check.is_blocked, || format!("a VM process could {}: {}", check.name, check.outcome))?;
    }
    let member = client(cluster, Role::Member).await;
    ready_network(&member, cluster, "hostile", "10.82.0.0/24").await?;
    let specification = VmSpecification {
        network_interfaces: vec![interface("hostile", &[])?],
        user_data: Some(hostile_script("10.82.0.1")),
        ..VmSpecification::small()
    };
    let created = member.create::<Vm>(&name("hostile-guest")?, &specification).await?;
    started_within_budget(&member, cluster, "hostile-guest", VM_START_BUDGET).await?;
    console_shows(cluster, created.id(), "HOSTILE-DONE").await?;
    let console = instance(cluster, created.id()).await?.console;
    ensure(!console.contains("HOSTILE-REACHED"), || format!("the guest reached the node: {console}"))?;
    delete_and_wait::<Vm>(&member, cluster, "hostile-guest").await?;
    delete_and_wait::<Network>(&member, cluster, "hostile").await
}
