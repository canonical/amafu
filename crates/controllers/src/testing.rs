use std::collections::BTreeMap;

use amafu_domain::{Metadata, ObjectId, ObjectName, ProjectName};

pub fn name(value: &str) -> ObjectName {
    ObjectName::new(value).unwrap()
}

pub fn metadata(object_name: &str, project: Option<&str>) -> Metadata {
    Metadata {
        name: name(object_name),
        project: project.map(|project| ProjectName::new(project).unwrap()),
        id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
        version: 1,
        generation: 1,
        observed_generation: 1,
        labels: BTreeMap::new(),
        is_deleting: false,
    }
}

pub fn only<T: std::fmt::Debug>(mut items: Vec<T>) -> T {
    assert_eq!(items.len(), 1, "expected exactly one item: {items:?}");
    items.remove(0)
}
