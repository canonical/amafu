use amafu_domain::kinds::node::{Node, NodeSpecification};
use amafu_domain::{Change, NodeName, Object, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get};
use amafu_interfaces::{ActionError, Consistency, Store};

#[must_use]
pub fn node_reference(node: &NodeName) -> ObjectReference {
    ObjectReference::of::<Node>(None, ObjectName::from(node))
}

pub async fn node_object(store: &dyn Store, node: &NodeName) -> Result<Object<Node>, ActionError> {
    get::<Node>(store, &node_reference(node), Consistency::Linearizable)
        .await?
        .ok_or_else(|| ActionError::invalid(format!("there is no node named {node}")))
}

pub async fn change_node_specification(
    store: &dyn Store,
    node: &NodeName,
    edit: impl FnOnce(&NodeSpecification) -> NodeSpecification,
) -> Result<(), ActionError> {
    let current = node_object(store, node).await?;
    let specification = edit(&current.specification);
    if specification == current.specification {
        return Ok(());
    }
    apply::<Node>(
        store,
        &node_reference(node),
        Change::UpdateSpecification { expected_generation: Some(current.metadata.generation), specification },
    )
    .await?;
    Ok(())
}
