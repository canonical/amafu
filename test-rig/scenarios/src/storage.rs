use amafu_checkers::invariants::a_volume_is_attached_on_at_most_one_node;
use amafu_domain::access::Role;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
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
    volume_in_phase(&member, cluster, "noble-root", VolumePhase::Available).await?;
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
