use amafu_domain::{Object, ObjectName, ObjectReference, Resource};

#[must_use]
pub fn cluster_reference<R: Resource>(name: &ObjectName) -> ObjectReference {
    ObjectReference::of::<R>(None, name.clone())
}

#[must_use]
pub fn project_reference<R: Resource>(owner: &Object<impl Resource>, name: &ObjectName) -> ObjectReference {
    ObjectReference::of::<R>(owner.metadata.project.clone(), name.clone())
}
