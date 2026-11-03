use std::time::Duration;

use amafu_domain::{ObjectName, ObjectReference};
use amafu_interfaces::{Consistency, Store};
use amafu_state::{QuoriumStore, STATE_SERVER_NAME, StateTls, VoterSettings, start_voter};
use quorium::TlsMaterial;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_voters_over_mutual_tls_replicate_every_write() {
    let authority = TlsMaterial::generate_ca("amafu-test").unwrap();
    let addresses = quorium::testing::free_addrs(3);
    let directories: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let starts = addresses.iter().zip(&directories).map(|(address, directory)| {
        let issued = authority.issue(STATE_SERVER_NAME).unwrap();
        let settings = VoterSettings {
            bind: *address,
            advertise: *address,
            voters: addresses.clone(),
            tls: Some(StateTls {
                certificate_authority_pem: authority.cert_pem.clone(),
                certificate_pem: issued.cert_pem,
                private_key_pem: issued.key_pem,
            }),
            heartbeat: amafu_state::DEFAULT_HEARTBEAT,
        };
        let path = directory.path().to_path_buf();
        async move { start_voter(&path, &settings).await }
    });
    let voters: Vec<_> = tokio::time::timeout(Duration::from_secs(60), futures::future::try_join_all(starts))
        .await
        .unwrap()
        .unwrap();
    let reference =
        ObjectReference { kind: String::from("Vm"), project: None, name: ObjectName::new("web-1").unwrap() };
    let writer = QuoriumStore::new(voters[0].clone());
    let reader = QuoriumStore::new(voters[2].clone());
    let mut written = Err(amafu_interfaces::StoreError::Unavailable(String::new()));
    for _ in 0..50 {
        written = writer.compare_and_swap(&reference, None, Some(json!({"value": 1}))).await;
        if written.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let revision = written.unwrap();
    let read = reader.read(&reference, Consistency::Linearizable).await.unwrap().unwrap();
    assert_eq!(read.revision, revision);
    for voter in voters {
        voter.shutdown().await;
    }
}
