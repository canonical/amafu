use amafu_checkers::invariants::a_volume_is_attached_on_at_most_one_node;
use amafu_domain::access::Role;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::{StorageBackend, StorageProviderSpecification};
use amafu_domain::kinds::vm::{BootSource, Vm, VmSpecification};
use amafu_domain::kinds::volume::{Volume, VolumePhase, VolumeSpecification};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure, eventually,
};
use futures::FutureExt;
use std::time::Duration;

use crate::support::{
    START_TIMEOUT, delete_and_wait, disk_image, instance, name, ready_image, shared_backend,
    started_within_budget, vm_in_phase,
};

pub const VM_BOOTS_FROM_A_CLONED_VOLUME: ScenarioDefinition = ScenarioDefinition {
    name: "a VM boots from a volume cloned from a disk image and releases it when deleted",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| a_vm_boots_from_a_cloned_volume(cluster).boxed_local(),
};

pub const BLOCK_VOLUMES_FOLLOW_THEIR_VM: ScenarioDefinition = ScenarioDefinition {
    name: "NVMe/TCP and Ceph RBD volumes are attached on the node of their VM and follow it",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| block_volumes_follow_their_vm(cluster).boxed_local(),
};

pub const NFS_VOLUME_IS_ATTACHED: ScenarioDefinition = ScenarioDefinition {
    name: "a volume on an NFS backend is attached to its VM through the node",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| an_nfs_volume_is_attached(cluster).boxed_local(),
};

const DISK_VM_MEMORY_MEBIBYTES: u64 = 1024;
const SETTLING: Duration = Duration::from_secs(3);

async fn volume_in_phase(
    member: &amafu_api::ApiClient,
    cluster: &dyn Cluster,
    volume_name: &str,
    phase: VolumePhase,
) -> ScenarioResult {
    eventually(cluster, START_TIMEOUT, &format!("{volume_name} becomes {phase:?}"), async || {
        let volume = member.get::<Volume>(volume_name).await?;
        Ok(volume.status.phase == phase && volume.is_status_current())
    })
    .await
}

async fn a_vm_boots_from_a_cloned_volume(cluster: &dyn Cluster) -> ScenarioResult {
    let operator = client(cluster, Role::Operator).await;
    let member = client(cluster, Role::Member).await;
    shared_backend(&operator, cluster, "local-disk", StorageProviderSpecification::LocalDisk).await?;
    ready_image(&member, cluster, "noble-minimal", disk_image()?).await?;
    let volume = VolumeSpecification {
        size_gibibytes: 4,
        backend: name("local-disk")?,
        source_image: Some(name("noble-minimal")?),
    };
    member.create::<Volume>(&name("noble-root")?, &volume).await?;
    let specification = VmSpecification {
        memory_mebibytes: DISK_VM_MEMORY_MEBIBYTES,
        boot: BootSource::Disk,
        volumes: vec![name("noble-root")?],
        ..VmSpecification::small()
    };
    let created = member.create::<Vm>(&name("from-disk")?, &specification).await?;
    vm_in_phase(&member, cluster, "from-disk", amafu_domain::kinds::vm::VmPhase::Running, START_TIMEOUT)
        .await?;
    volume_in_phase(&member, cluster, "noble-root", VolumePhase::Attached).await?;
    let attached = member.get::<Volume>("noble-root").await?;
    ensure(
        attached.status.attached_to.as_ref().map(ToString::to_string).as_deref() == Some("from-disk"),
        || format!("the volume names its VM, got {:?}", attached.status.attached_to),
    )?;
    ensure(!instance(cluster, created.id()).await?.disks.is_empty(), || {
        String::from("the VM process was given the volume as a disk")
    })?;
    always_during(cluster, SETTLING, a_volume_is_attached_on_at_most_one_node).await?;
    let node = instance(cluster, created.id()).await?.node;
    let provisioned = member.get::<Volume>("noble-root").await?.status.provisioned;
    ensure(provisioned.as_ref().and_then(|provisioned| provisioned.node.as_ref()) == Some(&node), || {
        format!("the local volume was created on the node of its VM, {node}, got {provisioned:?}")
    })?;
    delete_and_wait::<Vm>(&member, cluster, "from-disk").await?;
    volume_in_phase(&member, cluster, "noble-root", VolumePhase::Available).await?;
    delete_and_wait::<Volume>(&member, cluster, "noble-root").await?;
    delete_and_wait::<Image>(&member, cluster, "noble-minimal").await
}

async fn an_nfs_volume_is_attached(cluster: &dyn Cluster) -> ScenarioResult {
    let export = cluster
        .nfs_export()
        .ok_or_else(|| ScenarioError::Expectation(String::from("this cluster has no NFS export")))?;
    let operator = client(cluster, Role::Operator).await;
    let member = client(cluster, Role::Member).await;
    let provider = StorageProviderSpecification::Nfs { server: export.server.clone(), export: export.export };
    shared_backend(&operator, cluster, "shared-nfs", provider).await?;
    let volume = VolumeSpecification { size_gibibytes: 1, backend: name("shared-nfs")?, source_image: None };
    member.create::<Volume>(&name("shared-data")?, &volume).await?;
    volume_in_phase(&member, cluster, "shared-data", VolumePhase::Available).await?;
    let specification = VmSpecification { volumes: vec![name("shared-data")?], ..VmSpecification::small() };
    let created = member.create::<Vm>(&name("nfs-user")?, &specification).await?;
    started_within_budget(&member, cluster, "nfs-user", crate::support::VM_START_BUDGET).await?;
    volume_in_phase(&member, cluster, "shared-data", VolumePhase::Attached).await?;
    eventually(cluster, START_TIMEOUT, "the node holds an NFS attachment", async || {
        let observation = cluster.observe().await?;
        Ok(observation.attachments.iter().any(|attachment| attachment.volume.contains(&export.server)))
    })
    .await?;
    ensure(!instance(cluster, created.id()).await?.disks.is_empty(), || {
        String::from("the VM process was given the NFS volume as a disk")
    })?;
    delete_and_wait::<Vm>(&member, cluster, "nfs-user").await?;
    volume_in_phase(&member, cluster, "shared-data", VolumePhase::Available).await?;
    delete_and_wait::<Volume>(&member, cluster, "shared-data").await
}

async fn set_cordoned(
    operator: &amafu_api::ApiClient,
    node: &amafu_domain::NodeName,
    is_cordoned: bool,
) -> ScenarioResult {
    let current = operator.get::<amafu_domain::kinds::node::Node>(node.as_str()).await?;
    let specification = amafu_domain::kinds::node::NodeSpecification { is_cordoned, ..current.specification };
    operator.update::<amafu_domain::kinds::node::Node>(node.as_str(), &specification, None).await?;
    Ok(())
}

async fn attached_only_on(
    cluster: &dyn Cluster,
    node: &amafu_domain::NodeName,
    marker: &str,
) -> ScenarioResult {
    eventually(cluster, START_TIMEOUT, &format!("the volume is attached on {node} only"), async || {
        let observation = cluster.observe().await?;
        let holders: Vec<&amafu_domain::NodeName> = observation
            .attachments
            .iter()
            .filter(|attachment| attachment.volume.contains(marker))
            .map(|attachment| &attachment.node)
            .collect();
        Ok(holders == vec![node])
    })
    .await
}

async fn block_volumes_follow_their_vm(cluster: &dyn Cluster) -> ScenarioResult {
    crate::support::every_node_ready(cluster).await?;
    let operator = client(cluster, Role::Operator).await;
    let member = client(cluster, Role::Member).await;
    for (backend, provider) in cluster.block_backends() {
        shared_backend(&operator, cluster, &backend, provider).await?;
        let volume_name = format!("{backend}-data");
        let volume = VolumeSpecification { size_gibibytes: 1, backend: name(&backend)?, source_image: None };
        let created = member.create::<Volume>(&name(&volume_name)?, &volume).await?;
        volume_in_phase(&member, cluster, &volume_name, VolumePhase::Available).await?;
        let marker = created.id().to_string();
        let specification =
            VmSpecification { volumes: vec![name(&volume_name)?], ..VmSpecification::small() };
        let first = member.create::<Vm>(&name("block-user")?, &specification).await?;
        started_within_budget(&member, cluster, "block-user", crate::support::VM_START_BUDGET).await?;
        let first_node = instance(cluster, first.id()).await?.node;
        attached_only_on(cluster, &first_node, &marker).await?;
        set_cordoned(&operator, &first_node, true).await?;
        delete_and_wait::<Vm>(&member, cluster, "block-user").await?;
        let second = member.create::<Vm>(&name("block-user")?, &specification).await?;
        started_within_budget(&member, cluster, "block-user", crate::support::VM_START_BUDGET).await?;
        let second_node = instance(cluster, second.id()).await?.node;
        ensure(second_node != first_node, || format!("the VM came back on the cordoned {first_node}"))?;
        attached_only_on(cluster, &second_node, &marker).await?;
        always_during(cluster, SETTLING, a_volume_is_attached_on_at_most_one_node).await?;
        set_cordoned(&operator, &first_node, false).await?;
        delete_and_wait::<Vm>(&member, cluster, "block-user").await?;
        delete_and_wait::<Volume>(&member, cluster, &volume_name).await?;
        delete_and_wait::<StorageBackend>(&operator, cluster, &backend).await?;
    }
    Ok(())
}
