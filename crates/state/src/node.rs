use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use quorium::{Discovery, Quorium, TlsConfig};

use crate::log_repair::discard_zeroed_tail;

pub const STATE_SERVER_NAME: &str = "amafu-state";

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("the state store could not start: {0}")]
    Start(#[from] quorium::StartError),
    #[error("the state log could not be repaired: {0}")]
    Repair(#[from] std::io::Error),
    #[error(
        "electing a new store leader can take {estimate:?}, longer than the {budget:?} a node can go without renewing its lease"
    )]
    SlowFailover { estimate: Duration, budget: Duration },
}

pub fn ensure_failover_within(node: &Quorium, budget: Duration) -> Result<(), StateError> {
    let estimate = node.failover_estimate().total;
    if estimate > budget {
        return Err(StateError::SlowFailover { estimate, budget });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateTls {
    pub certificate_authority_pem: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreTiming {
    pub heartbeat: Duration,
    pub leader_lease: Duration,
}

impl Default for StoreTiming {
    fn default() -> Self {
        Self { heartbeat: Duration::from_millis(500), leader_lease: Duration::from_secs(2) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoterSettings {
    pub bind: SocketAddr,
    pub advertise: SocketAddr,
    pub voters: Vec<SocketAddr>,
    pub tls: Option<StateTls>,
    pub timing: StoreTiming,
}

const LOG_RETENTION_ENTRIES: u64 = 50_000;

pub async fn start_single_voter(data_directory: &Path, bind: SocketAddr) -> Result<Quorium, StateError> {
    let settings = VoterSettings {
        bind,
        advertise: bind,
        voters: Vec::new(),
        tls: None,
        timing: StoreTiming::default(),
    };
    start_voter(data_directory, &settings).await
}

pub async fn start_voter(data_directory: &Path, settings: &VoterSettings) -> Result<Quorium, StateError> {
    discard_zeroed_tail(data_directory)?;
    let mut builder =
        Quorium::builder().data_dir(data_directory).bind(settings.bind).advertise(settings.advertise);
    builder = builder
        .heartbeat(settings.timing.heartbeat)
        .leader_lease(settings.timing.leader_lease)
        .rpc_timeout(settings.timing.leader_lease)
        .log_retention(LOG_RETENTION_ENTRIES);
    builder = if settings.voters.len() > 1 {
        builder.discovery(Discovery::Static(settings.voters.clone())).replicas(settings.voters.len())
    } else {
        builder.single_node_dev_mode()
    };
    builder = match &settings.tls {
        Some(tls) => builder.tls(TlsConfig {
            ca_pem: tls.certificate_authority_pem.clone(),
            cert_pem: tls.certificate_pem.clone(),
            key_pem: tls.private_key_pem.clone(),
            server_name: STATE_SERVER_NAME.to_owned(),
        }),
        None => builder.insecure_plaintext(),
    };
    Ok(builder.start().await?)
}
