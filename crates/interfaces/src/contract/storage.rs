use amafu_domain::{NodeName, ObjectId};

use super::{ContractViolation, ensure, succeed};
use crate::storage::{FencedNode, StorageProvider, VolumeAttacher, VolumeRequest};

pub async fn storage_contract(
    provider: &dyn StorageProvider,
    attacher: &dyn VolumeAttacher,
    node: &NodeName,
) -> Result<(), ContractViolation> {
    succeed(provider.check().await, "the backend is reachable")?;
    let request = VolumeRequest {
        volume_id: ObjectId::from_uuid(uuid::Uuid::from_u128(0x00a1_fa00_0000_0000_0000_0000_0000_c0de)),
        size_bytes: 64 * 1024 * 1024,
        source_image: None,
    };
    let provisioned = succeed(provider.create_volume(&request).await, "creating a volume works")?;
    let again = succeed(provider.create_volume(&request).await, "creating a volume is idempotent")?;
    ensure(
        again == provisioned,
        "creating the same volume twice gives the same volume",
        (&provisioned, &again),
    )?;
    ensure(
        provisioned.size_bytes >= request.size_bytes,
        "the volume is at least as large as asked",
        &provisioned,
    )?;

    let target = succeed(provider.export_to_node(&provisioned, node).await, "exporting to a node works")?;
    ensure(target.protocol() == attacher.protocol(), "the export suits the attacher", &target)?;
    let path = succeed(attacher.attach(&target).await, "attaching works")?;
    let path_again = succeed(attacher.attach(&target).await, "attaching is idempotent")?;
    ensure(path == path_again, "attaching twice gives the same device", (&path, &path_again))?;
    let attachments = succeed(attacher.attached().await, "listing attachments works")?;
    ensure(
        attachments.iter().filter(|(attached_target, _)| *attached_target == target).count() == 1,
        "one attachment after two attaches",
        &attachments,
    )?;

    succeed(attacher.detach(&target).await, "detaching works")?;
    succeed(attacher.detach(&target).await, "detaching is idempotent")?;
    let remaining = succeed(attacher.attached().await, "listing attachments works after detaching")?;
    ensure(
        remaining.iter().all(|(attached_target, _)| *attached_target != target),
        "no attachment after detaching",
        &remaining,
    )?;

    succeed(provider.delete_volume(&provisioned).await, "deleting works")?;
    succeed(provider.delete_volume(&provisioned).await, "deleting is idempotent")
}

fn contract_volume(volume_number: u128) -> VolumeRequest {
    VolumeRequest {
        volume_id: ObjectId::from_uuid(uuid::Uuid::from_u128(
            0x00a1_fa00_0000_0000_0000_0000_0001_0000 + volume_number,
        )),
        size_bytes: 32 * 1024 * 1024,
        source_image: None,
    }
}

pub async fn provider_contract(
    provider: &dyn StorageProvider,
    node: &NodeName,
) -> Result<(), ContractViolation> {
    succeed(provider.check().await, "the backend is reachable")?;
    let request = contract_volume(1);
    let provisioned = succeed(provider.create_volume(&request).await, "creating a volume works")?;
    let again = succeed(provider.create_volume(&request).await, "creating a volume is idempotent")?;
    ensure(
        again == provisioned,
        "creating the same volume twice gives the same volume",
        (&provisioned, &again),
    )?;
    ensure(
        provisioned.size_bytes >= request.size_bytes,
        "the volume is at least as large as asked",
        &provisioned,
    )?;
    let other = succeed(provider.create_volume(&contract_volume(2)).await, "creating a second volume works")?;
    ensure(
        other.identifier != provisioned.identifier,
        "two volumes have different identifiers",
        (&provisioned, &other),
    )?;

    let target = succeed(provider.export_to_node(&provisioned, node).await, "exporting to a node works")?;
    let target_again = succeed(provider.export_to_node(&provisioned, node).await, "exporting is idempotent")?;
    ensure(target == target_again, "exporting twice gives the same target", (&target, &target_again))?;

    let fenced = FencedNode { name: node.clone(), address: None };
    succeed(provider.revoke_node(&fenced).await, "revoking a node works")?;
    succeed(provider.revoke_node(&fenced).await, "revoking a node is idempotent")?;
    let is_revoked = succeed(provider.has_revoked_node(&fenced).await, "asking about a revoked node works")?;
    ensure(is_revoked, "a revoked node is reported revoked", is_revoked)?;
    succeed(provider.readmit_node(&fenced).await, "readmitting a node works")?;
    succeed(provider.export_to_node(&provisioned, node).await, "a readmitted node can be exported to again")?;

    for volume in [&provisioned, &other] {
        succeed(provider.delete_volume(volume).await, "deleting works")?;
        succeed(provider.delete_volume(volume).await, "deleting is idempotent")?;
    }
    Ok(())
}
