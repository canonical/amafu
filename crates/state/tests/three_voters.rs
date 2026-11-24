use std::time::Duration;

use amafu_domain::ObjectReference;
use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_interfaces::{ConsensusMembership, Consistency, Store, StoreError};
use amafu_state::{
    QuoriumMembership, QuoriumStore, STATE_SERVER_NAME, StateTls, StoreTiming, VoterSettings,
    ensure_failover_within, start_voter,
};
use quorium::{Quorium, TlsMaterial};
use serde_json::json;

type Started = Result<(Vec<Quorium>, Vec<tempfile::TempDir>), Box<dyn std::error::Error>>;

async fn start_three_voters() -> Started {
    let authority = TlsMaterial::generate_ca("amafu-test")?;
    let addresses = quorium::testing::free_addrs(3);
    let directories = (0..3).map(|_| tempfile::tempdir()).collect::<Result<Vec<_>, _>>()?;
    let mut starts = Vec::new();
    for (address, directory) in addresses.iter().zip(&directories) {
        let issued = authority.issue(STATE_SERVER_NAME)?;
        let settings = VoterSettings {
            bind: *address,
            advertise: *address,
            voters: addresses.clone(),
            tls: Some(StateTls {
                certificate_authority_pem: authority.cert_pem.clone(),
                certificate_pem: issued.cert_pem,
                private_key_pem: issued.key_pem,
            }),
            timing: StoreTiming::default(),
        };
        let path = directory.path().to_path_buf();
        starts.push(async move { start_voter(&path, &settings).await });
    }
    let voters =
        tokio::time::timeout(Duration::from_secs(60), futures::future::try_join_all(starts)).await??;
    Ok((voters, directories))
}

fn vm(name: &str) -> ObjectReference {
    ObjectReference {
        kind: String::from("Vm"),
        project: None,
        name: name.parse().unwrap_or_else(|_| unreachable!()),
    }
}

async fn write_until_accepted(
    store: &QuoriumStore,
    reference: &ObjectReference,
    within: Duration,
) -> Result<u64, StoreError> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match store.compare_and_swap(reference, None, Some(json!({"value": 1}))).await {
            Err(StoreError::Unavailable(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            written => return written,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_voters_over_mutual_tls_replicate_every_write() {
    let (voters, _directories) = start_three_voters().await.unwrap();
    let writer = QuoriumStore::new(voters[0].clone());
    let reader = QuoriumStore::new(voters[2].clone());
    let revision = write_until_accepted(&writer, &vm("web-1"), Duration::from_secs(10)).await.unwrap();
    let read = reader.read(&vm("web-1"), Consistency::Linearizable).await.unwrap().unwrap();
    assert_eq!(read.revision, revision);
    let terms = LeaseTerms::new(Duration::from_secs(8), Duration::from_secs(8));
    ensure_failover_within(&voters[0], terms.failover_budget()).unwrap();
    assert!(ensure_failover_within(&voters[0], Duration::from_millis(100)).is_err());
    for voter in voters {
        voter.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_leaving_for_a_restart_hands_off_so_the_others_keep_writing() {
    let (voters, _directories) = start_three_voters().await.unwrap();
    write_until_accepted(&QuoriumStore::new(voters[0].clone()), &vm("before"), Duration::from_secs(10))
        .await
        .unwrap();
    let (leaders, survivors): (Vec<Quorium>, Vec<Quorium>) =
        voters.into_iter().partition(|voter| voter.status().is_leader);
    let leaving = QuoriumMembership::new(leaders[0].clone());
    leaving.hand_off_leadership().await.unwrap();
    leaving.close().await;
    let survivor = QuoriumStore::new(survivors[0].clone());
    write_until_accepted(&survivor, &vm("after"), Duration::from_secs(1)).await.unwrap();
    assert!(survivors.iter().any(|voter| voter.status().is_leader));
    for voter in survivors {
        voter.shutdown().await;
    }
}
