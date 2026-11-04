use std::collections::BTreeMap;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_domain::ObjectId;
use amafu_domain::access::Role;
use amafu_domain::features::Feature;
use amafu_domain::kinds::cluster_version::{CLUSTER_VERSION_NAME, ClusterVersion};
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::{Operation, OperationOutcome, OperationSpecification};
use amafu_domain::kinds::vm::VmSpecification;
use amafu_domain::node_image::NodeImage;
use amafu_domain::upgrade::{UpgradePlan, WavePolicy, inventory};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Level, NodeImages, ScenarioDefinition, ScenarioError, ScenarioResult, ensure, eventually,
};
use futures::FutureExt;

use crate::support::{GUEST_TIMEOUT, create_running, delete_all, delete_and_wait, every_node_ready, name};

pub const CLUSTER_UPGRADES_AND_ROLLS_BACK: ScenarioDefinition = ScenarioDefinition {
    name: "the cluster upgrades and rolls back without restarting a VM",
    levels: &[Level::VirtualLab],
    node_count: 4,
    run: |cluster| the_cluster_upgrades_and_rolls_back(cluster, LAB_VM_COUNT).boxed_local(),
};

pub const LARGE_CLUSTER_UPGRADES_AND_ROLLS_BACK: ScenarioDefinition = ScenarioDefinition {
    name: "twenty nodes with 200 VMs upgrade and roll back without restarting a VM",
    levels: &[Level::Simulated],
    node_count: 20,
    run: |cluster| the_cluster_upgrades_and_rolls_back(cluster, SIMULATED_VM_COUNT).boxed_local(),
};

pub const BROKEN_IMAGE_FALLS_BACK: ScenarioDefinition = ScenarioDefinition {
    name: "a node given a broken image boots its previous one without intervention",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_broken_image_falls_back(cluster).boxed_local(),
};

pub const FINALIZED_CLUSTER_REFUSES_ROLLBACK: ScenarioDefinition = ScenarioDefinition {
    name: "finalizing enables the new features and refuses a rollback",
    levels: &[Level::Simulated],
    node_count: 4,
    run: |cluster| a_finalized_cluster_refuses_rollback(cluster).boxed_local(),
};

const LAB_VM_COUNT: usize = 8;
const SIMULATED_VM_COUNT: usize = 200;
const VM_MEMORY_MEBIBYTES: u64 = 128;
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(7200);
const POLICY: WavePolicy = WavePolicy { canary_count: 1, health_deadline_seconds: 600, soak_seconds: 20 };

fn images(cluster: &dyn Cluster) -> Result<NodeImages, ScenarioError> {
    cluster
        .node_images()
        .ok_or_else(|| ScenarioError::Lab(String::from("the cluster offers no node images to upgrade to")))
}

async fn start(admin: &ApiClient, operation_name: &str, plan: UpgradePlan) -> ScenarioResult {
    admin.create::<Operation>(&name(operation_name)?, &OperationSpecification::upgrade(plan)).await?;
    Ok(())
}

async fn outcome_of(
    admin: &ApiClient,
    cluster: &dyn Cluster,
    operation_name: &str,
) -> Result<OperationOutcome, ScenarioError> {
    eventually(cluster, UPGRADE_TIMEOUT, &format!("{operation_name} finishes"), async || {
        Ok(admin.get::<Operation>(operation_name).await?.status.is_finished())
    })
    .await?;
    admin
        .get::<Operation>(operation_name)
        .await?
        .status
        .outcome
        .ok_or_else(|| ScenarioError::Expectation(format!("{operation_name} has no outcome")))
}

async fn upgrade_to(
    admin: &ApiClient,
    cluster: &dyn Cluster,
    operation_name: &str,
    image: &NodeImage,
) -> Result<OperationOutcome, ScenarioError> {
    let nodes = admin.list::<Node>().await?;
    start(admin, operation_name, UpgradePlan::upgrade(&inventory(&nodes, image), image.clone(), &POLICY))
        .await?;
    outcome_of(admin, cluster, operation_name).await
}

async fn roll_back_to(
    admin: &ApiClient,
    cluster: &dyn Cluster,
    operation_name: &str,
    image: &NodeImage,
) -> Result<OperationOutcome, ScenarioError> {
    let nodes = admin.list::<Node>().await?;
    start(admin, operation_name, UpgradePlan::rollback(&inventory(&nodes, image), image.clone(), &POLICY))
        .await?;
    outcome_of(admin, cluster, operation_name).await
}

async fn every_node_runs(admin: &ApiClient, image: &NodeImage) -> ScenarioResult {
    let nodes = admin.list::<Node>().await?;
    let stragglers: Vec<String> = nodes
        .iter()
        .filter(|node| !node.status.runs_confirmed(image) || node.specification.is_cordoned)
        .map(|node| {
            format!(
                "{} ({:?}, cordoned: {})",
                node.metadata.name, node.status.boot, node.specification.is_cordoned
            )
        })
        .collect();
    ensure(stragglers.is_empty(), || {
        format!("these nodes do not run {} uncordoned: {stragglers:?}", image.identity())
    })
}

async fn guest_boots(
    cluster: &dyn Cluster,
    vm_ids: &[ObjectId],
) -> Result<BTreeMap<ObjectId, String>, ScenarioError> {
    let mut boots = BTreeMap::new();
    eventually(cluster, GUEST_TIMEOUT, "every guest reports its boot", async || {
        let observation = cluster.observe().await?;
        boots = observation
            .instances
            .iter()
            .filter(|instance| vm_ids.contains(&instance.vm_id) && !instance.is_receiving)
            .filter_map(|instance| Some((instance.vm_id, instance.guest_boot.clone()?)))
            .collect();
        Ok(boots.len() == vm_ids.len())
    })
    .await?;
    Ok(boots)
}

async fn no_guest_restarted(cluster: &dyn Cluster, before: &BTreeMap<ObjectId, String>) -> ScenarioResult {
    let vm_ids: Vec<ObjectId> = before.keys().copied().collect();
    let after = guest_boots(cluster, &vm_ids).await?;
    let restarted: Vec<&ObjectId> = before
        .iter()
        .filter(|(vm_id, boot)| after.get(vm_id) != Some(boot))
        .map(|(vm_id, _)| vm_id)
        .collect();
    ensure(restarted.is_empty(), || format!("these VMs restarted: {restarted:?}"))
}

async fn tenants(cluster: &dyn Cluster, count: usize) -> Result<(Vec<String>, Vec<ObjectId>), ScenarioError> {
    let member = client(cluster, Role::Member).await;
    let names: Vec<String> = (0..count).map(|index| format!("resident-{index}")).collect();
    let small = VmSpecification { memory_mebibytes: VM_MEMORY_MEBIBYTES, ..VmSpecification::small() };
    let vm_ids = create_running(&member, cluster, &names, &small).await?;
    Ok((names, vm_ids))
}

async fn clean_up(cluster: &dyn Cluster, names: &[String]) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    let admin = client(cluster, Role::Admin).await;
    delete_all(&member, cluster, names).await?;
    for operation in admin.list::<Operation>().await? {
        delete_and_wait::<Operation>(&admin, cluster, operation.metadata.name.as_str()).await?;
    }
    Ok(())
}

async fn the_cluster_upgrades_and_rolls_back(cluster: &dyn Cluster, vm_count: usize) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let images = images(cluster)?;
    let admin = client(cluster, Role::Admin).await;
    let (names, vm_ids) = tenants(cluster, vm_count).await?;
    let before = guest_boots(cluster, &vm_ids).await?;

    let outcome = upgrade_to(&admin, cluster, "upgrade-forward", &images.upgrade).await?;
    ensure(outcome == OperationOutcome::Succeeded, || format!("the upgrade ended {outcome:?}"))?;
    every_node_runs(&admin, &images.upgrade).await?;
    let version = admin.get::<ClusterVersion>(CLUSTER_VERSION_NAME).await?.specification;
    ensure(
        version.image == images.upgrade && version.previous_image.as_ref() == Some(&images.installed),
        || format!("the cluster version did not move to the upgrade: {version:?}"),
    )?;
    no_guest_restarted(cluster, &before).await?;

    let outcome = roll_back_to(&admin, cluster, "upgrade-backward", &images.installed).await?;
    ensure(outcome == OperationOutcome::Succeeded, || format!("the rollback ended {outcome:?}"))?;
    every_node_runs(&admin, &images.installed).await?;
    no_guest_restarted(cluster, &before).await?;
    clean_up(cluster, &names).await
}

async fn a_broken_image_falls_back(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let images = images(cluster)?;
    let admin = client(cluster, Role::Admin).await;
    let outcome = upgrade_to(&admin, cluster, "upgrade-broken", &images.broken).await?;
    let OperationOutcome::Failed { reason } = outcome else {
        return Err(ScenarioError::Expectation(format!("the upgrade to a broken image ended {outcome:?}")));
    };
    ensure(reason.contains("went back to its previous image"), || {
        format!("the upgrade failed for another reason: {reason}")
    })?;
    let nodes = admin.list::<Node>().await?;
    let fallen: Vec<&str> = nodes
        .iter()
        .filter(|node| node.status.boot.as_ref().is_some_and(|boot| boot.fell_back_from(&images.broken)))
        .map(|node| node.metadata.name.as_str())
        .collect();
    ensure(fallen.len() == 1, || {
        format!("expected only the canary to try the broken image, got {fallen:?}")
    })?;
    ensure(nodes.iter().all(|node| node.status.runs_confirmed(&images.installed)), || {
        String::from("a node does not run its previous image after the broken one failed")
    })?;
    let outcome = roll_back_to(&admin, cluster, "upgrade-recovered", &images.installed).await?;
    ensure(outcome == OperationOutcome::Succeeded, || format!("the rollback ended {outcome:?}"))?;
    every_node_runs(&admin, &images.installed).await?;
    clean_up(cluster, &[]).await
}

async fn a_finalized_cluster_refuses_rollback(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let images = images(cluster)?;
    let admin = client(cluster, Role::Admin).await;
    ensure(images.upgrade.feature_level > images.installed.feature_level, || {
        String::from("the upgrade image must raise the feature level")
    })?;
    let outcome = upgrade_to(&admin, cluster, "upgrade-to-finalize", &images.upgrade).await?;
    ensure(outcome == OperationOutcome::Succeeded, || format!("the upgrade ended {outcome:?}"))?;
    let upgraded = admin.get::<ClusterVersion>(CLUSTER_VERSION_NAME).await?;
    ensure(!Feature::ParallelMigrationStreams.is_enabled(&upgraded.specification), || {
        String::from("a feature of the new image is enabled before finalizing")
    })?;
    let finalized = upgraded.specification.finalized();
    admin
        .update::<ClusterVersion>(CLUSTER_VERSION_NAME, &finalized, Some(upgraded.metadata.generation))
        .await?;
    let current = admin.get::<ClusterVersion>(CLUSTER_VERSION_NAME).await?;
    ensure(Feature::ParallelMigrationStreams.is_enabled(&current.specification), || {
        String::from("finalizing did not enable the features of the new image")
    })?;
    let outcome = roll_back_to(&admin, cluster, "upgrade-refused", &images.installed).await?;
    ensure(matches!(&outcome, OperationOutcome::Failed { reason } if reason.contains("finalized")), || {
        format!("a rollback after finalizing ended {outcome:?}")
    })?;
    every_node_runs(&admin, &images.upgrade).await?;
    clean_up(cluster, &[]).await
}
