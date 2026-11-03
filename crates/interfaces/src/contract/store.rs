use amafu_domain::{ObjectName, ObjectReference, ProjectName};
use serde_json::json;

use super::{ContractViolation, ensure, succeed};
use crate::store::{Consistency, Fence, Store, StoreError};

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
    ensure(recreated > updated, "revisions keep increasing after removal", (updated, recreated))?;
    fenced_contract(store).await
}

async fn fenced_contract(store: &dyn Store) -> Result<(), ContractViolation> {
    let assignment = reference("FencedContractKind", Some("project-a"), "assignment")?;
    let fence = |token| Fence { name: String::from("scheduler"), token };
    let first = succeed(
        store.compare_and_swap_fenced(&assignment, None, Some(json!({"node": "a"})), &fence(5)).await,
        "a fenced creation works",
    )?;
    let stale =
        store.compare_and_swap_fenced(&assignment, Some(first), Some(json!({"node": "b"})), &fence(4)).await;
    ensure(
        matches!(stale, Err(StoreError::Fenced { token: 4, .. })),
        "a write with an older token than one accepted is refused",
        &stale,
    )?;
    let same_holder = succeed(
        store.compare_and_swap_fenced(&assignment, Some(first), Some(json!({"node": "c"})), &fence(5)).await,
        "the current holder writes again",
    )?;
    let newer = succeed(
        store
            .compare_and_swap_fenced(&assignment, Some(same_holder), Some(json!({"node": "d"})), &fence(6))
            .await,
        "a newer holder writes",
    )?;
    let deposed =
        store.compare_and_swap_fenced(&assignment, Some(newer), Some(json!({"node": "e"})), &fence(5)).await;
    ensure(matches!(deposed, Err(StoreError::Fenced { .. })), "a deposed holder is refused", &deposed)?;
    let conflicting = store.compare_and_swap_fenced(&assignment, Some(first), None, &fence(7)).await;
    ensure(
        conflicting == Err(StoreError::Conflict),
        "a fenced write still checks the revision",
        &conflicting,
    )?;
    let unfenced = succeed(
        store
            .compare_and_swap(&assignment, Some(newer), Some(json!({"node": "d", "phase": "running"})))
            .await,
        "unfenced writes are unaffected by fences",
    )?;
    let other_fence = Fence { name: String::from("other"), token: 1 };
    succeed(
        store.compare_and_swap_fenced(&assignment, Some(unfenced), None, &other_fence).await,
        "each fence name keeps its own tokens",
    )?;
    let after_removal = store.compare_and_swap_fenced(&assignment, None, Some(json!({})), &fence(5)).await;
    ensure(
        matches!(after_removal, Err(StoreError::Fenced { .. })),
        "tokens are remembered after the object is removed",
        &after_removal,
    )
}
