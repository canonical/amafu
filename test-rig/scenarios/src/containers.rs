use amafu_domain::access::Role;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::vm::{BootSource, Vm, VmPhase, VmSpecification};
use amafu_interfaces::WorkloadState;
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure, ensure_equal, eventually,
};
use futures::FutureExt;

use crate::support::{
    CONTAINER_START_BUDGET, REMOVAL_TIMEOUT, START_TIMEOUT, busybox, console_shows, delete_and_wait,
    instance, name, ready_image, started_within_budget, vm_in_phase,
};

pub const CONTAINER_RUNS: ScenarioDefinition = ScenarioDefinition {
    name: "a container runs its image command in its own VM",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_container_runs_its_image_command(cluster).boxed_local(),
};

pub const CONTAINER_EXIT_IS_REPORTED: ScenarioDefinition = ScenarioDefinition {
    name: "a container whose command fails is reported failed with its exit code",
    levels: &[Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_failing_container_is_reported(cluster).boxed_local(),
};

const READY_MARKER: &str = "amafu-container-ready";

fn shell(script: &str) -> Vec<String> {
    vec![String::from("sh"), String::from("-c"), script.to_owned()]
}

fn container(image: &str, command: Vec<String>) -> Result<VmSpecification, ScenarioError> {
    let mut specification = VmSpecification::container(name(image)?);
    if let BootSource::Container { command: replaced, .. } = &mut specification.boot {
        *replaced = command;
    }
    Ok(specification)
}

async fn a_container_runs_its_image_command(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    ready_image(&member, cluster, "busybox-runs", busybox()?).await?;
    let specification = container("busybox-runs", shell(&format!("echo {READY_MARKER}; sleep 3600")))?;
    let created = member.create::<Vm>(&name("box-runs")?, &specification).await?;
    started_within_budget(&member, cluster, "box-runs", CONTAINER_START_BUDGET).await?;
    eventually(cluster, START_TIMEOUT, "the guest agent reports the workload running", async || {
        Ok(instance(cluster, created.id()).await?.workload == Some(WorkloadState::Running))
    })
    .await?;
    console_shows(cluster, created.id(), READY_MARKER).await?;
    delete_and_wait::<Vm>(&member, cluster, "box-runs").await?;
    eventually(cluster, REMOVAL_TIMEOUT, "the container VM is gone", async || {
        Ok(cluster.observe().await?.instance(created.id()).is_none())
    })
    .await?;
    delete_and_wait::<Image>(&member, cluster, "busybox-runs").await
}

async fn a_failing_container_is_reported(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    ready_image(&member, cluster, "busybox-fails", busybox()?).await?;
    let created =
        member.create::<Vm>(&name("box-fails")?, &container("busybox-fails", shell("exit 3"))?).await?;
    let failed = vm_in_phase(&member, cluster, "box-fails", VmPhase::Failed, START_TIMEOUT).await?;
    ensure_equal(&failed.status.message.as_deref(), &Some("the workload exited with code 3"), || {
        String::from("the failure carries the exit code")
    })?;
    eventually(cluster, REMOVAL_TIMEOUT, "the failed container's VM is stopped", async || {
        Ok(cluster.observe().await?.instance(created.id()).is_none())
    })
    .await?;
    let settled = member.get::<Vm>("box-fails").await?;
    ensure(settled.status.phase == VmPhase::Failed, || String::from("a failed container stays down"))?;
    delete_and_wait::<Vm>(&member, cluster, "box-fails").await?;
    delete_and_wait::<Image>(&member, cluster, "busybox-fails").await
}
