use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::server::{serve, tls_configuration};
use amafu_api::{ApiState, TokenAuthenticator, router};
use amafu_controllers::{
    ComputeController, ComputeServices, ImageController, LocalVolumeController, NetworkController,
    NodeController, NodeIdentity, NodeNetworkController, SchedulerController, SchedulingSettings,
    SecurityGroupController, StorageBackendController, VolumeController,
};
use amafu_domain::kinds::node::{CpuBaseline, NodeCapacity, NodeRole};
use amafu_domain::{NodeName, ObjectId};
use amafu_hypervisor::{
    BootSources, CloudHypervisor, CloudHypervisorSettings, HybridVsockAgents, RuntimeDirectory,
    prepare_kvm_access,
};
use amafu_interfaces::{
    AuditLog, Clock, Datapath, Fence, GuestAgents, Hypervisor, ImageStore, Leases, LocalServices,
    RandomIdentifiers, Storage, Store,
};
use amafu_network::{KernelDatapath, LocalServicesServer};
use amafu_reconciler::{Controller, ReconcilerSettings, run_controller, run_singleton};
use amafu_state::{QuoriumAuditLog, QuoriumLeases, QuoriumStore, start_single_voter};
use amafu_storage::{BuiltInStorage, CachedImages};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::boot::ModuleLoading;
use crate::clock::SystemClock;
use amafu_node::configuration::NodeConfiguration;

const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RESTART_DELAY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const NODE_IDENTIFIER_FILE: &str = "node-id";
const STATE_DIRECTORY: &str = "state";
pub const VM_DIRECTORY: &str = "vms";

pub struct Services {
    pub store: Arc<dyn Store>,
    pub audit: Arc<dyn AuditLog>,
    pub clock: Arc<dyn Clock>,
    pub leases: Option<Arc<dyn Leases>>,
}

pub struct ComputeAdapters {
    pub hypervisor: Arc<CloudHypervisor>,
    pub storage: Arc<BuiltInStorage>,
    pub images: Arc<CachedImages>,
    pub datapath: Arc<KernelDatapath>,
    pub agents: Arc<HybridVsockAgents>,
    pub local_services: Arc<LocalServicesServer>,
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

fn singleton_role<C, Make>(
    roles: &mut JoinSet<()>,
    name: &'static str,
    make: Make,
    services: &Services,
    settings: ReconcilerSettings,
) where
    C: Controller,
    Make: Fn(Fence) -> C + Clone + Send + Sync + 'static,
{
    let Some(leases) = services.leases.clone() else {
        tracing::error!(role = name, "a singleton role needs leases, which only core members hold");
        return;
    };
    let store = Arc::clone(&services.store);
    let clock = Arc::clone(&services.clock);
    roles.spawn(supervise(name, move || {
        let (leases, store, clock, make) =
            (Arc::clone(&leases), Arc::clone(&store), Arc::clone(&clock), make.clone());
        async move {
            run_singleton(leases.as_ref(), make, store.as_ref(), clock.as_ref(), settings).await.to_string()
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
        cpu_baseline: CpuBaseline::from_flags(
            amafu_system::host::host_cpu_flags(Path::new("/proc")).iter().map(String::as_str),
        ),
        zone: configuration.node.zone.clone(),
        underlay_address: configuration.network.as_ref().map(|network| network.address.address),
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

fn compute_adapters(
    configuration: &NodeConfiguration,
    hypervisor: &Arc<CloudHypervisor>,
    underlay: String,
) -> anyhow::Result<ComputeAdapters> {
    let directories = &configuration.directories;
    let compute = &configuration.compute;
    let runtime = RuntimeDirectory::new(directories.runtime.join(VM_DIRECTORY));
    Ok(ComputeAdapters {
        hypervisor: Arc::clone(hypervisor),
        storage: Arc::new(BuiltInStorage::new(configuration.node.name.clone(), directories.volumes())),
        images: Arc::new(CachedImages::new(directories.image_cache(), compute.mkfs_erofs_tool())?),
        datapath: Arc::new(KernelDatapath::new(underlay, compute.nft_tool())),
        agents: Arc::new(HybridVsockAgents::new(runtime)),
        local_services: Arc::new(LocalServicesServer::start(
            compute.resolvers.clone(),
            tokio::runtime::Handle::current(),
        )),
    })
}

fn cloud_hypervisor(configuration: &NodeConfiguration) -> anyhow::Result<CloudHypervisor> {
    let directories = &configuration.directories;
    let compute = &configuration.compute;
    Ok(CloudHypervisor::new(CloudHypervisorSettings {
        runtime: RuntimeDirectory::new(directories.runtime.join(VM_DIRECTORY)),
        boot: BootSources {
            bundles_directory: directories.images.clone(),
            firmware: compute.firmware.clone(),
            container_kernel: compute.container_kernel.clone(),
            container_initramfs: compute.container_initramfs.clone(),
        },
        executable: std::env::current_exe()?,
        shared_readable: vec![directories.image_cache()],
    }))
}

fn core_roles(roles: &mut JoinSet<()>, services: &Services, adapters: &ComputeAdapters) {
    let store = || Arc::clone(&services.store);
    let settings = ReconcilerSettings::default();
    let scheduler_store = store();
    singleton_role(
        roles,
        "scheduler",
        move |fence| {
            SchedulerController::new(Arc::clone(&scheduler_store), fence, SchedulingSettings::default())
        },
        services,
        settings,
    );
    let network_store = store();
    singleton_role(
        roles,
        "network",
        move |_| NetworkController::new(Arc::clone(&network_store)),
        services,
        settings,
    );
    let group_store = store();
    singleton_role(
        roles,
        "security-group",
        move |_| SecurityGroupController::new(Arc::clone(&group_store)),
        services,
        settings,
    );
    let (volume_store, volume_storage, volume_images) = (
        store(),
        Arc::clone(&adapters.storage) as Arc<dyn Storage>,
        Arc::clone(&adapters.images) as Arc<dyn ImageStore>,
    );
    singleton_role(
        roles,
        "volume",
        move |_| {
            VolumeController::new(
                Arc::clone(&volume_store),
                Arc::clone(&volume_storage),
                Arc::clone(&volume_images),
            )
        },
        services,
        settings,
    );
    let (backend_store, backend_storage) = (store(), Arc::clone(&adapters.storage) as Arc<dyn Storage>);
    singleton_role(
        roles,
        "storage-backend",
        move |_| StorageBackendController::new(Arc::clone(&backend_store), Arc::clone(&backend_storage)),
        services,
        settings,
    );
}

fn compute_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    adapters: &ComputeAdapters,
    node: &NodeName,
    settings: ReconcilerSettings,
) {
    let compute = ComputeController::new(
        node.clone(),
        ComputeServices {
            store: Arc::clone(&services.store),
            hypervisor: Arc::clone(&adapters.hypervisor) as Arc<dyn Hypervisor>,
            storage: Arc::clone(&adapters.storage) as Arc<dyn Storage>,
            images: Arc::clone(&adapters.images) as Arc<dyn ImageStore>,
            datapath: Arc::clone(&adapters.datapath) as Arc<dyn Datapath>,
            agents: Arc::clone(&adapters.agents) as Arc<dyn GuestAgents>,
        },
    );
    controller_role(roles, "compute", Arc::new(compute), services, settings);
    let image = ImageController::new(
        node.clone(),
        Arc::clone(&services.store),
        Arc::clone(&adapters.images) as Arc<dyn ImageStore>,
        services.leases.is_some(),
    );
    controller_role(roles, "image", Arc::new(image), services, settings);
    let local_volume = LocalVolumeController::new(
        node.clone(),
        Arc::clone(&services.store),
        Arc::clone(&adapters.storage) as Arc<dyn Storage>,
        Arc::clone(&adapters.images) as Arc<dyn ImageStore>,
    );
    controller_role(roles, "local-volume", Arc::new(local_volume), services, settings);
    let node_network = NodeNetworkController::new(
        node.clone(),
        Arc::clone(&services.store),
        Arc::clone(&adapters.datapath) as Arc<dyn Datapath>,
        Arc::clone(&adapters.local_services) as Arc<dyn LocalServices>,
    );
    controller_role(roles, "node-network", Arc::new(node_network), services, settings);
}

pub async fn run(configuration: NodeConfiguration, modules: Option<ModuleLoading>) -> anyhow::Result<()> {
    let directories = &configuration.directories;
    for directory in
        [&directories.data, &directories.runtime, &directories.image_cache(), &directories.volumes()]
    {
        std::fs::create_dir_all(directory)?;
    }
    let quorium =
        start_single_voter(&directories.data.join(STATE_DIRECTORY), configuration.state.listen).await?;
    let hypervisor = Arc::new(cloud_hypervisor(&configuration)?);
    let services = Services {
        store: Arc::new(QuoriumStore::new(quorium.clone())),
        audit: Arc::new(QuoriumAuditLog::new(quorium.clone())),
        clock: Arc::new(SystemClock),
        leases: Some(Arc::new(QuoriumLeases::new(quorium.clone()))),
    };
    let underlay = amafu_network::bring_up(
        configuration.network.as_ref().and_then(|network| network.interface.as_deref()),
    )
    .await?;
    let adapters = compute_adapters(&configuration, &hypervisor, underlay)?;
    let mut roles = JoinSet::new();
    let responsive = ReconcilerSettings {
        periodic_resynchronization: Duration::from_secs(2),
        ..ReconcilerSettings::default()
    };
    let node_name = configuration.node.name.clone();
    if configuration.has_role(NodeRole::Core) {
        start_api(&mut roles, &configuration, &services)?;
        core_roles(&mut roles, &services, &adapters);
    }
    let identity = node_identity(&configuration)?;
    let node =
        Arc::new(NodeController::new(identity, Arc::clone(&services.store), Arc::clone(&services.clock)));
    let heartbeat = ReconcilerSettings {
        periodic_resynchronization: HEARTBEAT_INTERVAL / 2,
        ..ReconcilerSettings::default()
    };
    controller_role(&mut roles, "node", node, &services, heartbeat);
    if let Some(modules) = modules {
        modules.await?;
    }
    if configuration.has_role(NodeRole::Compute) {
        if let Err(error) = prepare_kvm_access() {
            tracing::warn!(%error, "VM processes cannot be given access to /dev/kvm");
        }
        compute_roles(&mut roles, &services, &adapters, &node_name, responsive);
    }
    crate::diagnostics::start(&mut roles, &configuration, &services, &adapters);
    while let Some(ended) = roles.join_next().await {
        tracing::error!(?ended, "a role supervisor ended");
    }
    anyhow::bail!("every role ended")
}
