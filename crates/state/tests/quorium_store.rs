use std::time::Duration;

use amafu_domain::access::Role;
use amafu_domain::{ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::contract::store::store_contract;
use amafu_interfaces::{AuditEntry, AuditLog, AuditOutcome, Leases, Store, WatchEvent};
use amafu_state::{QuoriumAuditLog, QuoriumLeases, QuoriumStore, StateError, start_single_voter};
use futures::StreamExt;
use quorium::Quorium;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

async fn single_voter(directory: &tempfile::TempDir) -> Result<Quorium, StateError> {
    let address = quorium::testing::free_addrs(1)[0];
    start_single_voter(directory.path(), address).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quorium_store_honours_the_store_contract() {
    let directory = tempfile::tempdir().unwrap();
    let node = single_voter(&directory).await.unwrap();
    assert_eq!(store_contract(&QuoriumStore::new(node.clone())).await, Ok(()));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watchers_hear_about_every_change_of_their_kind() {
    let directory = tempfile::tempdir().unwrap();
    let node = single_voter(&directory).await.unwrap();
    let store = QuoriumStore::new(node.clone());
    let mut events = store.watch("Vm");
    let reference = ObjectReference {
        kind: String::from("Vm"),
        project: Some(ProjectName::new("project-a").unwrap()),
        name: ObjectName::new("web-1").unwrap(),
    };
    let revision = store.compare_and_swap(&reference, None, Some(json!({"value": 1}))).await.unwrap();
    store.compare_and_swap(&reference, Some(revision), Some(json!({"value": 2}))).await.unwrap();
    let mut heard_change = false;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(5), events.next()).await {
        if event == WatchEvent::Changed(reference.clone()) {
            heard_change = true;
            break;
        }
    }
    assert!(heard_change, "the watcher was never told about web-1");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_entries_can_be_found_by_request() {
    let directory = tempfile::tempdir().unwrap();
    let node = single_voter(&directory).await.unwrap();
    let audit = QuoriumAuditLog::new(node.clone());
    let mut request_ids = Vec::new();
    for _ in 0..300 {
        let request_id = Uuid::new_v4();
        request_ids.push(request_id);
        audit
            .record(AuditEntry {
                request_id,
                recorded_at: OffsetDateTime::UNIX_EPOCH,
                principal: Some(String::from("alice")),
                role: Some(Role::Member),
                verb: None,
                kind: None,
                project: None,
                name: None,
                method: String::from("GET"),
                path: String::from("/v1/vms"),
                outcome: AuditOutcome::Denied,
            })
            .await
            .unwrap();
    }
    assert!(audit.find(request_ids[0]).await.unwrap().is_some());
    assert!(audit.find(request_ids[299]).await.unwrap().is_some());
    assert!(audit.find(Uuid::new_v4()).await.unwrap().is_none());
    let recent = audit.recent(3).await.unwrap();
    assert_eq!(
        recent.iter().map(|entry| entry.request_id).collect::<Vec<_>>(),
        vec![request_ids[299], request_ids[298], request_ids[297]]
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lease_is_granted_with_a_fencing_token() {
    let directory = tempfile::tempdir().unwrap();
    let node = single_voter(&directory).await.unwrap();
    let leases = QuoriumLeases::new(node.clone());
    let held =
        tokio::time::timeout(Duration::from_secs(10), leases.acquire("scheduler")).await.unwrap().unwrap();
    assert_eq!(held.fence.name, "scheduler");
    assert!(held.fence.token > 0);
    let (_, lost) = held.into_parts();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), lost).await.is_err(),
        "a held lease is not lost"
    );
    node.shutdown().await;
}
