use amafu_domain::{ObjectName, ObjectReference, ProjectName};
use serde_json::json;

use super::{ContractViolation, ensure, succeed};
use crate::store::{Consistency, Store, StoreError};

fn reference(kind: &str, project: Option<&str>, name: &str) -> Result<ObjectReference, ContractViolation> {
    let project = project.map(ProjectName::new).transpose();
    let project = succeed(project, "the test project name is valid")?;
    let name = succeed(ObjectName::new(name), "the test object name is valid")?;
    Ok(ObjectReference { kind: kind.to_owned(), project, name })
}

pub async fn store_contract(store: &dyn Store) -> Result<(), ContractViolation> {
    let first = reference("ContractKind", Some("project-a"), "first")?;
    let second = reference("ContractKind", Some("project-b"), "second")?;
    let other_kind = reference("OtherContractKind", None, "third")?;

    let missing =
        succeed(store.read(&first, Consistency::Linearizable).await, "reading a missing object works")?;
    ensure(missing.is_none(), "a missing object reads as absent", &missing)?;

    let revision = succeed(
        store.compare_and_swap(&first, None, Some(json!({"value": 1}))).await,
        "creating an object works",
    )?;
    let duplicate = store.compare_and_swap(&first, None, Some(json!({"value": 2}))).await;
    ensure(duplicate == Err(StoreError::Conflict), "creating an existing object conflicts", &duplicate)?;

    let stored = succeed(store.read(&first, Consistency::Linearizable).await, "reading an object works")?;
    ensure(
        stored
            .as_ref()
            .is_some_and(|record| record.revision == revision && record.object == json!({"value": 1})),
        "a read returns the written object and its revision",
        &stored,
    )?;

    let updated = succeed(
        store.compare_and_swap(&first, Some(revision), Some(json!({"value": 3}))).await,
        "updating at the current revision works",
    )?;
    ensure(updated > revision, "revisions increase", (revision, updated))?;
    let stale = store.compare_and_swap(&first, Some(revision), Some(json!({"value": 4}))).await;
    ensure(stale == Err(StoreError::Conflict), "updating at a stale revision conflicts", &stale)?;

    succeed(
        store.compare_and_swap(&second, None, Some(json!({"value": 5}))).await,
        "creating a second object works",
    )?;
    succeed(
        store.compare_and_swap(&other_kind, None, Some(json!({"value": 6}))).await,
        "creating another kind works",
    )?;

    let project_a = succeed(ProjectName::new("project-a"), "the project name is valid")?;
    let in_project = succeed(
        store.list("ContractKind", Some(&project_a), Consistency::Linearizable).await,
        "listing works",
    )?;
    ensure(in_project.len() == 1, "listing filters by project", &in_project)?;
    let in_every_project = succeed(
        store.list("ContractKind", None, Consistency::Linearizable).await,
        "listing all projects works",
    )?;
    ensure(in_every_project.len() == 2, "listing without a project covers every project", &in_every_project)?;

    succeed(
        store.compare_and_swap(&first, Some(updated), None).await,
        "removing at the current revision works",
    )?;
    let removed =
        succeed(store.read(&first, Consistency::Linearizable).await, "reading a removed object works")?;
    ensure(removed.is_none(), "a removed object reads as absent", &removed)?;
    let after_removal = succeed(
        store.list("ContractKind", None, Consistency::Linearizable).await,
        "listing after removal works",
    )?;
    ensure(after_removal.len() == 1, "a removed object is not listed", &after_removal)?;

    let recreated = succeed(
        store.compare_and_swap(&first, None, Some(json!({"value": 7}))).await,
        "recreating an object works",
    )?;
    ensure(recreated > updated, "revisions keep increasing after removal", (updated, recreated))
}
