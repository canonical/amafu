use std::collections::BTreeMap;

use amafu_domain::registry::{KindDescriptor, UntypedChange};
use amafu_domain::{ObjectId, ProjectName, ValidationError};

use crate::wire::{CreateRequest, UpdateRequest};

const MAXIMUM_LABELS: usize = 64;
const MAXIMUM_LABEL_KEY_LENGTH: usize = 63;
const MAXIMUM_LABEL_VALUE_LENGTH: usize = 253;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("the request body is malformed: {0}")]
    Malformed(String),
    #[error("{0}")]
    Invalid(ValidationError),
}

fn check_labels(labels: &BTreeMap<String, String>) -> Result<(), AdmissionError> {
    let mut error = ValidationError::new();
    error.check(labels.len() <= MAXIMUM_LABELS, "metadata.labels", "must hold at most 64 labels");
    for (key, value) in labels {
        let is_valid_key = !key.is_empty()
            && key.len() <= MAXIMUM_LABEL_KEY_LENGTH
            && key.chars().all(|character| character.is_ascii_alphanumeric() || "-_./".contains(character));
        error.check(
            is_valid_key,
            "metadata.labels",
            "keys must be 1 to 63 letters, digits, '-', '_', '.' or '/'",
        );
        error.check(
            value.len() <= MAXIMUM_LABEL_VALUE_LENGTH && !value.chars().any(char::is_control),
            "metadata.labels",
            "values must be at most 253 printable characters",
        );
    }
    error.into_result().map_err(AdmissionError::Invalid)
}

pub fn admit_create(
    descriptor: &KindDescriptor,
    project: Option<ProjectName>,
    body: &[u8],
    id: ObjectId,
) -> Result<UntypedChange, AdmissionError> {
    let request: CreateRequest =
        serde_json::from_slice(body).map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    check_labels(&request.metadata.labels)?;
    let project = if descriptor.is_project_scoped() { project } else { None };
    Ok(UntypedChange::Create {
        id,
        name: request.metadata.name,
        project,
        labels: request.metadata.labels,
        specification: request.specification,
    })
}

pub fn admit_update(body: &[u8]) -> Result<UntypedChange, AdmissionError> {
    let request: UpdateRequest =
        serde_json::from_slice(body).map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    Ok(UntypedChange::UpdateSpecification {
        expected_generation: request.expected_generation,
        specification: request.specification,
    })
}

#[cfg(test)]
mod tests {
    use amafu_domain::registry::{kind_named, kinds};
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;

    fn new_id() -> ObjectId {
        ObjectId::from_uuid(Uuid::new_v4())
    }

    #[test]
    fn a_well_formed_body_is_admitted() {
        let vm = kind_named("Vm").unwrap();
        let body = br#"{"metadata":{"name":"web-1"},"specification":{"cpu_count":1,"memory_mebibytes":256,"boot":{"type":"kernel","bundle":"test-guest"}}}"#;
        let change = admit_create(&vm, ProjectName::new("project-a").ok(), body, new_id()).unwrap();
        let created = (vm.apply_change)(None, change).unwrap();
        assert_eq!(created["metadata"]["project"], "project-a");
    }

    #[test]
    fn unknown_fields_are_refused() {
        let vm = kind_named("Vm").unwrap();
        let body = br#"{"metadata":{"name":"web-1","owner":"mallory"},"specification":{}}"#;
        assert!(matches!(admit_create(&vm, None, body, new_id()), Err(AdmissionError::Malformed(_))));
    }

    #[test]
    fn bad_labels_are_refused() {
        let vm = kind_named("Vm").unwrap();
        let body = br#"{"metadata":{"name":"web-1","labels":{"bad key":"x"}},"specification":{}}"#;
        assert!(matches!(admit_create(&vm, None, body, new_id()), Err(AdmissionError::Invalid(_))));
    }

    proptest! {
        #[test]
        fn arbitrary_bodies_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
            for descriptor in kinds() {
                if let Ok(change) = admit_create(&descriptor, ProjectName::new("project-a").ok(), &body, new_id()) {
                    let _ = (descriptor.apply_change)(None, change);
                }
                if let Ok(change) = admit_update(&body) {
                    let _ = (descriptor.apply_change)(None, change);
                }
            }
        }
    }
}
