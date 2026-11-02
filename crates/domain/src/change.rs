use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::name::{ObjectName, ProjectName};
use crate::object::{Metadata, Object};
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq)]
pub enum Change<R: Resource> {
    Create {
        id: ObjectId,
        name: ObjectName,
        project: Option<ProjectName>,
        labels: BTreeMap<String, String>,
        specification: R::Specification,
    },
    UpdateSpecification {
        expected_generation: Option<u64>,
        specification: R::Specification,
    },
    UpdateStatus {
        observed_generation: u64,
        status: R::Status,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum ChangeError {
    #[error("expected generation {expected} but the object is at generation {actual}")]
    GenerationConflict { expected: u64, actual: u64 },
    #[error("status observed generation {observed} but the object is at generation {current}")]
    StaleStatus { observed: u64, current: u64 },
    #[error("status claims generation {observed} which the object has not reached ({current})")]
    ObservedGenerationAhead { observed: u64, current: u64 },
    #[error("the object already exists")]
    AlreadyExists,
    #[error("the object does not exist")]
    NotFound,
    #[error("the object is being deleted")]
    IsDeleting,
    #[error("the object must be deleted before it is removed")]
    NotDeleting,
    #[error("{0}")]
    Invalid(ValidationError),
}

impl From<ValidationError> for ChangeError {
    fn from(error: ValidationError) -> Self {
        Self::Invalid(error)
    }
}

pub fn apply_change<R: Resource>(
    current: Option<Object<R>>,
    change: Change<R>,
) -> Result<Object<R>, ChangeError> {
    match (current, change) {
        (None, Change::Create { id, name, project, labels, specification }) => {
            check_scope::<R>(project.as_ref())?;
            specification.validate()?;
            Ok(Object {
                metadata: Metadata {
                    name,
                    project,
                    id,
                    version: R::VERSION,
                    generation: 1,
                    observed_generation: 0,
                    labels,
                    is_deleting: false,
                },
                specification,
                status: R::Status::default(),
            })
        }
        (Some(mut object), Change::UpdateSpecification { expected_generation, specification }) => {
            if let Some(expected) = expected_generation
                && expected != object.metadata.generation
            {
                return Err(ChangeError::GenerationConflict { expected, actual: object.metadata.generation });
            }
            if object.metadata.is_deleting {
                return Err(ChangeError::IsDeleting);
            }
            specification.validate()?;
            if object.specification != specification {
                object.specification = specification;
                object.metadata.generation += 1;
            }
            Ok(object)
        }
        (Some(mut object), Change::UpdateStatus { observed_generation, status }) => {
            let current = object.metadata.generation;
            if observed_generation < current {
                return Err(ChangeError::StaleStatus { observed: observed_generation, current });
            }
            if observed_generation > current {
                return Err(ChangeError::ObservedGenerationAhead { observed: observed_generation, current });
            }
            object.status = status;
            object.metadata.observed_generation = observed_generation;
            Ok(object)
        }
        (Some(mut object), Change::Delete) => {
            object.metadata.is_deleting = true;
            Ok(object)
        }
        (Some(_), Change::Create { .. }) => Err(ChangeError::AlreadyExists),
        (None, _) => Err(ChangeError::NotFound),
    }
}

pub fn confirm_removal<R: Resource>(current: Option<&Object<R>>) -> Result<(), ChangeError> {
    match current {
        None => Err(ChangeError::NotFound),
        Some(object) if object.metadata.is_deleting => Ok(()),
        Some(_) => Err(ChangeError::NotDeleting),
    }
}

fn check_scope<R: Resource>(project: Option<&ProjectName>) -> Result<(), ValidationError> {
    let mut error = ValidationError::new();
    match (R::SCOPE, project) {
        (Scope::Project, None) => error.add("metadata.project", "is required for this kind"),
        (Scope::Cluster, Some(_)) => error.add("metadata.project", "must be absent for a cluster-wide kind"),
        _ => {}
    }
    error.into_result()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;
    use crate::kinds::node::{Node, NodeSpecification};
    use crate::kinds::vm::{Vm, VmPhase, VmSpecification, VmStatus};

    fn project() -> ProjectName {
        ProjectName::new("project-a").unwrap()
    }

    fn create_vm(specification: VmSpecification) -> Change<Vm> {
        Change::Create {
            id: ObjectId::from_uuid(Uuid::new_v4()),
            name: ObjectName::new("web-1").unwrap(),
            project: Some(project()),
            labels: BTreeMap::new(),
            specification,
        }
    }

    fn created_vm() -> Object<Vm> {
        apply_change(None, create_vm(VmSpecification::small())).unwrap()
    }

    #[test]
    fn creation_starts_at_generation_one_with_nothing_observed() {
        let vm = created_vm();
        assert_eq!(vm.metadata.generation, 1);
        assert_eq!(vm.metadata.observed_generation, 0);
        assert_eq!(vm.status, VmStatus::default());
        assert!(!vm.metadata.is_deleting);
    }

    #[test]
    fn creation_validates_the_specification() {
        let mut specification = VmSpecification::small();
        specification.cpu_count = 0;
        assert!(matches!(apply_change(None, create_vm(specification)), Err(ChangeError::Invalid(_))));
    }

    #[test]
    fn creation_requires_a_project_for_project_kinds() {
        let mut change = create_vm(VmSpecification::small());
        if let Change::Create { project, .. } = &mut change {
            *project = None;
        }
        assert!(matches!(apply_change(None, change), Err(ChangeError::Invalid(_))));
    }

    #[test]
    fn creation_refuses_a_project_for_cluster_kinds() {
        let change = Change::<Node>::Create {
            id: ObjectId::from_uuid(Uuid::new_v4()),
            name: ObjectName::new("node-0").unwrap(),
            project: Some(project()),
            labels: BTreeMap::new(),
            specification: NodeSpecification::default(),
        };
        assert!(matches!(apply_change(None, change), Err(ChangeError::Invalid(_))));
    }

    #[test]
    fn creating_twice_conflicts() {
        let vm = created_vm();
        assert_eq!(
            apply_change(Some(vm), create_vm(VmSpecification::small())),
            Err(ChangeError::AlreadyExists)
        );
    }

    #[test]
    fn changing_the_specification_increments_the_generation() {
        let mut specification = VmSpecification::small();
        specification.cpu_count = 4;
        let updated = apply_change(
            Some(created_vm()),
            Change::UpdateSpecification { expected_generation: Some(1), specification },
        )
        .unwrap();
        assert_eq!(updated.metadata.generation, 2);
        assert_eq!(updated.specification.cpu_count, 4);
    }

    #[test]
    fn an_identical_specification_keeps_the_generation() {
        let updated = apply_change(
            Some(created_vm()),
            Change::UpdateSpecification {
                expected_generation: None,
                specification: VmSpecification::small(),
            },
        )
        .unwrap();
        assert_eq!(updated.metadata.generation, 1);
    }

    #[test]
    fn a_stale_expected_generation_conflicts() {
        let result = apply_change(
            Some(created_vm()),
            Change::UpdateSpecification {
                expected_generation: Some(7),
                specification: VmSpecification::small(),
            },
        );
        assert_eq!(result, Err(ChangeError::GenerationConflict { expected: 7, actual: 1 }));
    }

    #[test]
    fn the_specification_cannot_change_while_deleting() {
        let deleting = apply_change(Some(created_vm()), Change::Delete).unwrap();
        let mut specification = VmSpecification::small();
        specification.cpu_count = 2;
        let result = apply_change(
            Some(deleting),
            Change::UpdateSpecification { expected_generation: None, specification },
        );
        assert_eq!(result, Err(ChangeError::IsDeleting));
    }

    #[test]
    fn status_for_an_older_generation_is_rejected() {
        let mut specification = VmSpecification::small();
        specification.cpu_count = 2;
        let updated = apply_change(
            Some(created_vm()),
            Change::UpdateSpecification { expected_generation: None, specification },
        )
        .unwrap();
        let status = VmStatus { phase: VmPhase::Running, ..VmStatus::default() };
        let result = apply_change(Some(updated), Change::UpdateStatus { observed_generation: 1, status });
        assert_eq!(result, Err(ChangeError::StaleStatus { observed: 1, current: 2 }));
    }

    #[test]
    fn status_cannot_claim_a_future_generation() {
        let status = VmStatus { phase: VmPhase::Running, ..VmStatus::default() };
        let result =
            apply_change(Some(created_vm()), Change::UpdateStatus { observed_generation: 2, status });
        assert_eq!(result, Err(ChangeError::ObservedGenerationAhead { observed: 2, current: 1 }));
    }

    #[test]
    fn current_status_is_recorded_with_its_generation() {
        let status = VmStatus { phase: VmPhase::Running, ..VmStatus::default() };
        let updated = apply_change(
            Some(created_vm()),
            Change::UpdateStatus { observed_generation: 1, status: status.clone() },
        )
        .unwrap();
        assert_eq!(updated.status, status);
        assert!(updated.is_status_current());
    }

    #[test]
    fn changes_to_a_missing_object_are_not_found() {
        assert_eq!(apply_change::<Vm>(None, Change::Delete), Err(ChangeError::NotFound));
    }

    #[test]
    fn removal_requires_deletion_first() {
        let vm = created_vm();
        assert_eq!(confirm_removal(Some(&vm)), Err(ChangeError::NotDeleting));
        let deleting = apply_change(Some(vm), Change::Delete).unwrap();
        assert_eq!(confirm_removal(Some(&deleting)), Ok(()));
        assert_eq!(confirm_removal::<Vm>(None), Err(ChangeError::NotFound));
    }

    #[derive(Debug, Clone)]
    enum ArbitraryChange {
        Specification { expected_generation: Option<u64>, cpu_count: u16 },
        Status { observed_offset: i8 },
        Delete,
    }

    fn arbitrary_change() -> impl Strategy<Value = ArbitraryChange> {
        prop_oneof![
            (proptest::option::of(0_u64..6), 1_u16..8).prop_map(|(expected_generation, cpu_count)| {
                ArbitraryChange::Specification { expected_generation, cpu_count }
            }),
            (-2_i8..3).prop_map(|observed_offset| ArbitraryChange::Status { observed_offset }),
            Just(ArbitraryChange::Delete),
        ]
    }

    proptest! {
        #[test]
        fn generation_rules_hold_for_any_sequence(changes in proptest::collection::vec(arbitrary_change(), 0..40)) {
            let mut object = created_vm();
            for change in changes {
                let before = object.clone();
                let change = match change {
                    ArbitraryChange::Specification { expected_generation, cpu_count } => {
                        let mut specification = VmSpecification::small();
                        specification.cpu_count = cpu_count;
                        Change::UpdateSpecification { expected_generation, specification }
                    }
                    ArbitraryChange::Status { observed_offset } => Change::UpdateStatus {
                        observed_generation: before.metadata.generation.saturating_add_signed(i64::from(observed_offset)),
                        status: VmStatus { phase: VmPhase::Running, ..VmStatus::default() },
                    },
                    ArbitraryChange::Delete => Change::Delete,
                };
                match apply_change(Some(before.clone()), change) {
                    Ok(after) => {
                        prop_assert!(after.metadata.generation >= before.metadata.generation);
                        prop_assert!(after.metadata.generation <= before.metadata.generation + 1);
                        prop_assert_eq!(after.metadata.generation != before.metadata.generation,
                            after.specification != before.specification);
                        prop_assert!(after.metadata.observed_generation <= after.metadata.generation);
                        prop_assert!(after.metadata.is_deleting || !before.metadata.is_deleting);
                        prop_assert_eq!(after.metadata.id, before.metadata.id);
                        object = after;
                    }
                    Err(_) => object = before,
                }
            }
        }
    }
}
