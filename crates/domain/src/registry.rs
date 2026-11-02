use std::collections::BTreeMap;

use serde_json::Value;

use crate::change::{Change, ChangeError, apply_change, confirm_removal};
use crate::kinds::node::Node;
use crate::kinds::vm::Vm;
use crate::name::{ObjectName, ProjectName};
use crate::object::Object;
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};

#[derive(Debug, Clone, PartialEq)]
pub enum UntypedChange {
    Create {
        id: ObjectId,
        name: ObjectName,
        project: Option<ProjectName>,
        labels: BTreeMap<String, String>,
        specification: Value,
    },
    UpdateSpecification {
        expected_generation: Option<u64>,
        specification: Value,
    },
    UpdateStatus {
        observed_generation: u64,
        status: Value,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UntypedChangeError {
    #[error("the request body does not match the kind: {0}")]
    Malformed(String),
    #[error("the stored object cannot be decoded: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Change(#[from] ChangeError),
}

type ApplyUntyped = fn(Option<&Value>, UntypedChange) -> Result<Value, UntypedChangeError>;
type ConfirmUntypedRemoval = fn(Option<&Value>) -> Result<(), UntypedChangeError>;

#[derive(Debug, Clone, Copy)]
pub struct KindDescriptor {
    pub kind: &'static str,
    pub plural_name: &'static str,
    pub version: u32,
    pub scope: Scope,
    pub apply_change: ApplyUntyped,
    pub confirm_removal: ConfirmUntypedRemoval,
}

impl KindDescriptor {
    #[must_use]
    pub fn of<R: Resource>() -> Self {
        Self {
            kind: R::KIND,
            plural_name: R::PLURAL_NAME,
            version: R::VERSION,
            scope: R::SCOPE,
            apply_change: apply_untyped::<R>,
            confirm_removal: confirm_untyped_removal::<R>,
        }
    }

    #[must_use]
    pub fn is_project_scoped(&self) -> bool {
        self.scope == Scope::Project
    }
}

#[must_use]
pub fn kinds() -> [KindDescriptor; 2] {
    [KindDescriptor::of::<Node>(), KindDescriptor::of::<Vm>()]
}

#[must_use]
pub fn kind_named(kind: &str) -> Option<KindDescriptor> {
    kinds().into_iter().find(|descriptor| descriptor.kind == kind)
}

#[must_use]
pub fn kind_with_plural_name(plural_name: &str) -> Option<KindDescriptor> {
    kinds().into_iter().find(|descriptor| descriptor.plural_name == plural_name)
}

fn decode_stored<R: Resource>(stored: Option<&Value>) -> Result<Option<Object<R>>, UntypedChangeError> {
    stored
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| UntypedChangeError::Corrupt(error.to_string()))
}

fn decode_body<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, UntypedChangeError> {
    serde_json::from_value(value).map_err(|error| UntypedChangeError::Malformed(error.to_string()))
}

fn apply_untyped<R: Resource>(
    stored: Option<&Value>,
    change: UntypedChange,
) -> Result<Value, UntypedChangeError> {
    let current = decode_stored::<R>(stored)?;
    let change = match change {
        UntypedChange::Create { id, name, project, labels, specification } => {
            Change::<R>::Create { id, name, project, labels, specification: decode_body(specification)? }
        }
        UntypedChange::UpdateSpecification { expected_generation, specification } => {
            Change::UpdateSpecification { expected_generation, specification: decode_body(specification)? }
        }
        UntypedChange::UpdateStatus { observed_generation, status } => {
            Change::UpdateStatus { observed_generation, status: decode_body(status)? }
        }
        UntypedChange::Delete => Change::Delete,
    };
    let updated = apply_change(current, change)?;
    serde_json::to_value(updated).map_err(|error| UntypedChangeError::Corrupt(error.to_string()))
}

fn confirm_untyped_removal<R: Resource>(stored: Option<&Value>) -> Result<(), UntypedChangeError> {
    let current = decode_stored::<R>(stored)?;
    Ok(confirm_removal(current.as_ref())?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn names_are_unique() {
        let all = kinds();
        for (index, descriptor) in all.iter().enumerate() {
            for other in &all[index + 1..] {
                assert_ne!(descriptor.kind, other.kind);
                assert_ne!(descriptor.plural_name, other.plural_name);
            }
        }
    }

    #[test]
    fn lookups_find_each_kind() {
        assert_eq!(kind_named("Vm").map(|descriptor| descriptor.plural_name), Some("vms"));
        assert_eq!(kind_with_plural_name("nodes").map(|descriptor| descriptor.kind), Some("Node"));
        assert!(kind_named("Volume").is_none());
    }

    #[test]
    fn untyped_changes_apply_the_typed_rules() {
        let descriptor = kind_named("Vm").unwrap();
        let created = (descriptor.apply_change)(
            None,
            UntypedChange::Create {
                id: ObjectId::from_uuid(Uuid::new_v4()),
                name: ObjectName::new("web-1").unwrap(),
                project: Some(ProjectName::new("project-a").unwrap()),
                labels: BTreeMap::new(),
                specification: json!({"cpu_count": 2, "memory_mebibytes": 512, "image": "test-guest"}),
            },
        )
        .unwrap();
        assert_eq!(created["metadata"]["generation"], json!(1));
        assert_eq!(created["status"]["phase"], json!("pending"));

        let malformed = (descriptor.apply_change)(
            Some(&created),
            UntypedChange::UpdateSpecification {
                expected_generation: None,
                specification: json!({"cpu_count": "two"}),
            },
        );
        assert!(matches!(malformed, Err(UntypedChangeError::Malformed(_))));
    }
}
