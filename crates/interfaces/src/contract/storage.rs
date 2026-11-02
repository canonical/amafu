use amafu_domain::{NodeName, ObjectId};

use super::{ContractViolation, ensure, succeed};
use crate::storage::{StorageProvider, VolumeAttacher, VolumeRequest};

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
