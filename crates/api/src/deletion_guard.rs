use amafu_domain::ObjectReference;
use amafu_domain::kinds::vm::Vm;
use amafu_interfaces::objects::list;
use amafu_interfaces::{Consistency, ObjectStoreError, Store};

pub async fn holders(
    store: &dyn Store,
    reference: &ObjectReference,
) -> Result<Vec<String>, ObjectStoreError> {
    match reference.kind.as_str() {
        "Volume" => volume_holders(store, reference).await,
        _ => Ok(Vec::new()),
    }
}

async fn volume_holders(
    store: &dyn Store,
    volume: &ObjectReference,
) -> Result<Vec<String>, ObjectStoreError> {
    let vms = list::<Vm>(store, volume.project.as_ref(), Consistency::Linearizable).await?;
    Ok(vms
        .iter()
        .filter(|vm| vm.specification.volumes.contains(&volume.name))
        .map(|vm| format!("VM {}", vm.metadata.name))
        .collect())
}
