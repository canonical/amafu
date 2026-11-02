use amafu_domain::{ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Store};
use amafu_state::{QuoriumStore, start_single_voter};
use serde_json::json;

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn reference(index: usize) -> Result<ObjectReference, Box<dyn std::error::Error>> {
    Ok(ObjectReference {
        kind: String::from("Vm"),
        project: Some(ProjectName::new("project-a")?),
        name: ObjectName::new(format!("vm-{index}"))?,
    })
}

async fn write_objects(store: &QuoriumStore, count: usize) -> Outcome {
    for index in 0..count {
        let reference = reference(index)?;
        let revision = store.compare_and_swap(&reference, None, Some(json!({"index": index}))).await?;
        store
            .compare_and_swap(&reference, Some(revision), Some(json!({"index": index, "updated": true})))
            .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_store_reopens_with_its_objects_after_a_clean_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let address = quorium::testing::free_addrs(1)[0];
    let node = start_single_voter(directory.path(), address).await.unwrap();
    write_objects(&QuoriumStore::new(node.clone()), 200).await.unwrap();
    node.shutdown().await;

    let address = quorium::testing::free_addrs(1)[0];
    let node = start_single_voter(directory.path(), address).await.unwrap();
    let listed = QuoriumStore::new(node.clone()).list("Vm", None, Consistency::Linearizable).await.unwrap();
    assert_eq!(listed.len(), 200);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_store_reopens_after_power_loss_leaves_zeros_at_the_end_of_the_log() {
    let directory = tempfile::tempdir().unwrap();
    let address = quorium::testing::free_addrs(1)[0];
    let node = start_single_voter(directory.path(), address).await.unwrap();
    write_objects(&QuoriumStore::new(node.clone()), 20).await.unwrap();
    node.shutdown().await;

    let log = directory.path().join("wal");
    let mut contents = std::fs::read(&log).unwrap();
    contents.extend(std::iter::repeat_n(0_u8, 4096));
    std::fs::write(&log, contents).unwrap();

    let address = quorium::testing::free_addrs(1)[0];
    let node = start_single_voter(directory.path(), address).await.unwrap();
    let listed = QuoriumStore::new(node.clone()).list("Vm", None, Consistency::Linearizable).await.unwrap();
    assert_eq!(listed.len(), 20);
    node.shutdown().await;
}
