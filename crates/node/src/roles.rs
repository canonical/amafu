use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use amafu_api::server::{serve, tls_configuration};
use amafu_api::{ApiState, TokenAuthenticator, router};
use amafu_controllers::{ComputeController, NodeController, NodeIdentity, PlacementController};
use amafu_domain::ObjectId;
use amafu_domain::kinds::node::{NodeCapacity, NodeRole};
use amafu_hypervisor::{CloudHypervisor, RuntimeDirectory};
use amafu_interfaces::{AuditLog, Clock, Hypervisor, RandomIdentifiers, Store};
use amafu_reconciler::{Controller, ReconcilerSettings, run_controller};
use amafu_state::{QuoriumAuditLog, QuoriumStore, start_single_voter};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::clock::SystemClock;
use amafu_node::configuration::NodeConfiguration;

const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RESTART_DELAY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const NODE_IDENTIFIER_FILE: &str = "node-id";
const STATE_DIRECTORY: &str = "state";
const VM_DIRECTORY: &str = "vms";

pub struct Services {
    pub store: Arc<dyn Store>,
    pub audit: Arc<dyn AuditLog>,
    pub hypervisor: Arc<dyn Hypervisor>,
    pub clock: Arc<dyn Clock>,
}

async fn supervise<Role, Running>(name: &'static str, role: Role)
where
    Role: Fn() -> Running + Send + 'static,
    Running: Future<Output = String> + Send,
{
    let mut delay = INITIAL_RESTART_DELAY;
    loop {
        let failure = role().await;
        tracing::error!(role = name, %failure, ?delay, "role stopped; restarting");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAXIMUM_RESTART_DELAY);
    }
}

fn controller_role<C: Controller>(
    roles: &mut JoinSet<()>,
    name: &'static str,
    controller: Arc<C>,
    services: &Services,
    settings: ReconcilerSettings,
) {
    let store = Arc::clone(&services.store);
    let clock = Arc::clone(&services.clock);
    roles.spawn(supervise(name, move || {
        let controller = Arc::clone(&controller);
        let store = Arc::clone(&store);
        let clock = Arc::clone(&clock);
        async move {
            run_controller(controller.as_ref(), store.as_ref(), clock.as_ref(), settings).await.to_string()
        }
    }));
}

fn node_identifier(data_directory: &Path) -> anyhow::Result<ObjectId> {
    let path = data_directory.join(NODE_IDENTIFIER_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        return existing.trim().parse().map_err(|error| {
            anyhow::anyhow!("the node identifier in {} is unreadable: {error}", path.display())
        });
    }
    let identifier = ObjectId::from_uuid(Uuid::new_v4());
    amafu_system::files::write_durably(&path, identifier.to_string().as_bytes())?;
    Ok(identifier)
}

fn node_identity(configuration: &NodeConfiguration) -> anyhow::Result<NodeIdentity> {
    let capacity = amafu_system::host::capacity(Path::new("/proc"));
    Ok(NodeIdentity {
        name: configuration.node.name.clone(),
        id: node_identifier(&configuration.directories.data)?,
        roles: configuration.node.roles.iter().copied().collect::<BTreeSet<NodeRole>>(),
        capacity: NodeCapacity { cpu_count: capacity.cpu_count, memory_mebibytes: capacity.memory_mebibytes },
        software_version: String::from(env!("CARGO_PKG_VERSION")),
        heartbeat_interval: HEARTBEAT_INTERVAL,
    })
}

fn start_api(
    roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    services: &Services,
) -> anyhow::Result<()> {
    let state = ApiState {
        store: Arc::clone(&services.store),
        authenticator: Arc::new(TokenAuthenticator::new(configuration.api.identities.clone())),
        audit: Arc::clone(&services.audit),
        clock: Arc::clone(&services.clock),
        identifiers: Arc::new(RandomIdentifiers),
    };
    let tls = tls_configuration(
        configuration.api.certificate.as_bytes(),
        configuration.api.private_key.as_bytes(),
    )?;
    let listen = configuration.api.listen;
    let application = router(state);
    roles.spawn(supervise("api", move || {
        let application = application.clone();
        let tls = Arc::clone(&tls);
        async move {
            match TcpListener::bind(listen).await {
                Ok(listener) => {
                    tracing::info!(%listen, "the API is serving");
                    crate::diagnostics::record_serving();
                    serve(listener, Some(tls), application).await.to_string()
                }
                Err(error) => format!("binding {listen} failed: {error}"),
            }
        }
    }));
    Ok(())
}

pub async fn run(configuration: NodeConfiguration) -> anyhow::Result<()> {
    let directories = &configuration.directories;
    std::fs::create_dir_all(&directories.data)?;
    std::fs::create_dir_all(&directories.runtime)?;
    let quorium =
        start_single_voter(&directories.data.join(STATE_DIRECTORY), configuration.state.listen).await?;
    let executable: PathBuf = std::env::current_exe()?;
    let services = Services {
        store: Arc::new(QuoriumStore::new(quorium.clone())),
        audit: Arc::new(QuoriumAuditLog::new(quorium.clone())),
        hypervisor: Arc::new(CloudHypervisor::new(
            RuntimeDirectory::new(directories.runtime.join(VM_DIRECTORY)),
            directories.images.clone(),
            executable,
        )),
        clock: Arc::new(SystemClock),
    };
    let mut roles = JoinSet::new();
    let responsive = ReconcilerSettings {
        periodic_resynchronization: Duration::from_secs(2),
        ..ReconcilerSettings::default()
    };
    if configuration.has_role(NodeRole::Core) {
        start_api(&mut roles, &configuration, &services)?;
        let placement = Arc::new(PlacementController::new(Arc::clone(&services.store)));
        controller_role(&mut roles, "placement", placement, &services, ReconcilerSettings::default());
    }
    let identity = node_identity(&configuration)?;
    let node =
        Arc::new(NodeController::new(identity, Arc::clone(&services.store), Arc::clone(&services.clock)));
    let heartbeat = ReconcilerSettings {
        periodic_resynchronization: HEARTBEAT_INTERVAL / 2,
        ..ReconcilerSettings::default()
    };
    controller_role(&mut roles, "node", node, &services, heartbeat);
    if configuration.has_role(NodeRole::Compute) {
        let compute = Arc::new(ComputeController::new(
            configuration.node.name.clone(),
            Arc::clone(&services.store),
            Arc::clone(&services.hypervisor),
        ));
        controller_role(&mut roles, "compute", compute, &services, responsive);
    }
    crate::diagnostics::start(&mut roles, &configuration, &services);
    while let Some(ended) = roles.join_next().await {
        tracing::error!(?ended, "a role supervisor ended");
    }
    anyhow::bail!("every role ended")
}
