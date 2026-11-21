use amafu_domain::kinds::ssh_key::{SshKey, SshKeyGroup, SshKeyReference};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{Object, ObjectName, ObjectReference, ProjectName, Resource};
use amafu_interfaces::objects::{get, list};
use amafu_interfaces::{Consistency, ObjectStoreError, Store};

pub async fn missing_reference(
    store: &dyn Store,
    project: Option<&ProjectName>,
    references: &[SshKeyReference],
) -> Result<Option<String>, ObjectStoreError> {
    for reference in references {
        let (exists, noun) = match reference {
            SshKeyReference::Key { name } => (exists::<SshKey>(store, project, name).await?, "SSH key"),
            SshKeyReference::Group { name } => {
                (exists::<SshKeyGroup>(store, project, name).await?, "SSH key group")
            }
        };
        if !exists {
            return Ok(Some(format!("there is no {noun} {} in this project", reference.name())));
        }
    }
    Ok(None)
}

pub async fn missing_key(
    store: &dyn Store,
    project: Option<&ProjectName>,
    keys: &[ObjectName],
) -> Result<Option<String>, ObjectStoreError> {
    let references: Vec<SshKeyReference> =
        keys.iter().map(|name| SshKeyReference::Key { name: name.clone() }).collect();
    missing_reference(store, project, &references).await
}

async fn exists<R: Resource>(
    store: &dyn Store,
    project: Option<&ProjectName>,
    name: &ObjectName,
) -> Result<bool, ObjectStoreError> {
    let reference = ObjectReference::of::<R>(project.cloned(), name.clone());
    Ok(get::<R>(store, &reference, Consistency::Linearizable).await?.is_some())
}

fn waiting_for_keys(vms: Vec<Object<Vm>>) -> impl Iterator<Item = Object<Vm>> {
    vms.into_iter().filter(|vm| !vm.has_granted_keys())
}

pub async fn key_holders(store: &dyn Store, key: &ObjectReference) -> Result<Vec<String>, ObjectStoreError> {
    let project = key.project.as_ref();
    let groups = list::<SshKeyGroup>(store, project, Consistency::Linearizable).await?;
    let holding_groups: Vec<ObjectName> = groups
        .into_iter()
        .filter(|group| !group.metadata.is_deleting && group.specification.keys.contains(&key.name))
        .map(|group| group.metadata.name)
        .collect();
    let vms = list::<Vm>(store, project, Consistency::Linearizable).await?;
    let holding_vms = waiting_for_keys(vms).filter(|vm| {
        vm.specification.ssh_keys.iter().any(|reference| match reference {
            SshKeyReference::Key { name } => name == &key.name,
            SshKeyReference::Group { name } => holding_groups.contains(name),
        })
    });
    Ok(holding_groups
        .iter()
        .map(|group| format!("SSH key group {group}"))
        .chain(holding_vms.map(|vm| format!("VM {}, which has not started yet", vm.metadata.name)))
        .collect())
}

pub async fn group_holders(
    store: &dyn Store,
    group: &ObjectReference,
) -> Result<Vec<String>, ObjectStoreError> {
    let vms = list::<Vm>(store, group.project.as_ref(), Consistency::Linearizable).await?;
    Ok(waiting_for_keys(vms)
        .filter(|vm| vm.specification.ssh_keys.contains(&SshKeyReference::Group { name: group.name.clone() }))
        .map(|vm| format!("VM {}, which has not started yet", vm.metadata.name))
        .collect())
}
