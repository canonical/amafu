use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::server::{serve, tls_configuration};
use amafu_api::service_accounts::IssuedTokenAuthenticator;
use amafu_api::{
    ApiState, ChainedAuthenticator, ClientIdentity, HttpTransport, NodeApiState, NodeAuthenticator,
    NodeClient, TokenAuthenticator, Transport, node_router, router,
};
use amafu_audit_export::BuiltInAuditSinks;
use amafu_controllers::RunnerKind;
use amafu_controllers::authorized_keys::AuthorizedKeysController;
use amafu_controllers::environments::failover::ApiFailoverController;
use amafu_controllers::environments::host_health::HostHealthController;
use amafu_controllers::environments::juju::{JujuEnvironment, JujuSteps};
use amafu_controllers::environments::kubernetes::{KubernetesEnvironment, KubernetesSteps};
use amafu_controllers::environments::outcomes::{
    CreationOutcomeController, EnvironmentKinds, JujuKinds, KubernetesKinds,
};
use amafu_controllers::environments::rotation::CredentialRotationController;
use amafu_controllers::node_lease::{LeaseKeeper, LeaseKeeperSettings, feed_while_leased};
use amafu_controllers::retirement::SshKeyRetirementController;
use amafu_controllers::root_volumes::RootVolumeController;
use amafu_controllers::{
    AuditExportController, BootSlotsController, BootSlotsServices, BootSlotsSettings,
    CertificateRevocationController, ComputeController, ComputeServices, ExternalNetworkController,
    FloatingIpController, ImageController, LocalVolumeController, NetworkController, NodeController,
    NodeHealthController, NodeIdentity, NodeNetworkController, OperationSteps, OperationsController,
    SchedulerController, SecurityGroupController, StorageBackendController, VolumeController,
    cluster_image::ClusterImageController, signing_keys::SigningKeysController,
};
use amafu_domain::NodeName;
use amafu_domain::ObjectId;
use amafu_domain::kinds::juju_controller::JujuControllerChange;
use amafu_domain::kinds::kubernetes_cluster::KubernetesClusterChange;
use amafu_domain::kinds::node::{CpuBaseline, NodeCapacity, NodeRole};
use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_hypervisor::{
    BootSources, CloudHypervisor, CloudHypervisorSettings, HybridVsockAgents, MIGRATION_PORT,
    MigrationTransport, RuntimeDirectory, prepare_kvm_access,
};
use amafu_image_catalog::{CanonicalCatalog, host_architecture};
use amafu_interfaces::{
    AuditEntry, AuditLog, BootSlots, Clock, CoreConnection, Datapath, Fence, GuestAgents, Hypervisor,
    ImageStore, Leases, LocalServices, NodeLeaseAuthority, NodeLeaseState, RandomIdentifiers,
    SequencedAuditEntry, Storage, Store, StoreError, Watchdog,
};
use amafu_interfaces::{FixedImageCatalog, ImageCatalog, KubernetesPlatform};
use amafu_juju_client::WebSocketJuju;
use amafu_kubernetes_client::HttpKubernetes;
use amafu_network::{KernelDatapath, LocalServicesServer};
use amafu_reconciler::{Controller, ReconcilerSettings, StepRegistry, run_controller, run_singleton};
use amafu_state::{
    BatchedAuditLog, LeaseFencedStore, LocalNodeLeases, LocalStateCache, QuoriumAuditLog, QuoriumLeases,
    QuoriumStore, ReadReplicaStore, StateTls, SubscribedStore, VoterSettings, start_voter,
};
use amafu_storage::plugins::{DownloadedModules, PluginHost, PluginLimits};
use amafu_storage::{BuiltInStorage, CachedImages};
use amafu_token_issuer::Keyring;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::clock::SystemClock;
use amafu_node_configuration::NodeConfiguration;

const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RESTART_DELAY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const NODE_IDENTIFIER_FILE: &str = "node-id";
const STATE_DIRECTORY: &str = "state";
const STATE_CACHE_FILE: &str = "state-cache.json";
const SUBSCRIPTION_REFRESH: Duration = Duration::from_secs(5);
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
const API_WORKER_THREADS: usize = 4;
const SYNCHRONIZATION_POLL: Duration = Duration::from_millis(100);
pub const VM_DIRECTORY: &str = "vms";

pub struct Services {
    pub store: Arc<dyn Store>,
    pub audit: Arc<dyn AuditLog>,
    pub clock: Arc<dyn Clock>,
    pub leases: Option<Arc<dyn Leases>>,
    pub node_leases: Arc<dyn NodeLeaseAuthority>,
}

pub struct BootServices {
    pub modules: Pin<Box<dyn Future<Output = ()> + Send>>,
    pub watchdog: Arc<dyn Watchdog>,
    pub boot_slots: Arc<dyn BootSlots>,
}

struct AuditKeptByTheCore;

#[async_trait::async_trait]
impl AuditLog for AuditKeptByTheCore {
    async fn record(&self, _entry: AuditEntry) -> Result<(), StoreError> {
        Err(StoreError::Unavailable(String::from("only core members keep the audit log")))
    }

    async fn find(&self, _request_id: uuid::Uuid) -> Result<Option<AuditEntry>, StoreError> {
        Ok(None)
    }

    async fn entries_from(&self, _first: u64, _limit: usize) -> Result<Vec<SequencedAuditEntry>, StoreError> {
        Ok(Vec::new())
    }

    async fn recent(&self, _limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        Ok(Vec::new())
    }
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
    let replicated_kinds = amafu_domain::registry::kinds()
        .iter()
        .map(|descriptor| descriptor.kind)
        .filter(|kind| *kind != "NodeLease")
        .collect();
    let replica: Arc<dyn Store> =
        Arc::new(ReadReplicaStore::start(Arc::clone(&services.store), &replicated_kinds));
    let state = ApiState {
        store: Arc::clone(&replica),
        authenticator: Arc::new(ChainedAuthenticator::new(vec![
            Arc::new(TokenAuthenticator::new(configuration.api.identities.clone())),
            Arc::new(IssuedTokenAuthenticator::new(Arc::clone(&replica))),
        ])),
        audit: Arc::clone(&services.audit),
        clock: Arc::clone(&services.clock),
        identifiers: Arc::new(RandomIdentifiers),
        issuers: Arc::new(amafu_api::service_accounts::HttpIssuerDirectory::default()),
        key_directory: Arc::new(amafu_api::key_directory::HttpKeyDirectory::default()),
        catalog: catalog_for_node(),
    };
    let tls = tls_configuration(
        configuration.api.certificate.as_bytes(),
        configuration.api.private_key.as_bytes(),
    )?;
    let listen = configuration.api.listen;
    let mut application = router(state);
    if let Some(identity) = &configuration.identity {
        let authenticator =
            NodeAuthenticator::new(identity.certificate_authority.as_bytes(), Arc::clone(&replica))?;
        application = application.merge(node_router(NodeApiState {
            store: Arc::clone(&replica),
            authenticator: Arc::new(authenticator),
            audit: Arc::clone(&services.audit),
            clock: Arc::clone(&services.clock),
            identifiers: Arc::new(RandomIdentifiers),
            refresh_interval: SUBSCRIPTION_REFRESH,
        }));
    }
    let serving = api_runtime()?.spawn(supervise("api", move || {
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
    roles.spawn(async move {
        if let Err(error) = serving.await {
            tracing::error!(%error, "the API runtime stopped");
        }
    });
    Ok(())
}

fn api_runtime() -> std::io::Result<&'static tokio::runtime::Runtime> {
    static API_RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    if let Some(runtime) = API_RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(API_WORKER_THREADS)
        .thread_name("amafu-api")
        .enable_all()
        .build()?;
    Ok(API_RUNTIME.get_or_init(|| runtime))
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
        storage: Arc::new(
            BuiltInStorage::new(configuration.node.name.clone(), directories.volumes()).with_plugins(
                PluginHost::new(
                    Arc::new(DownloadedModules::new(directories.plugin_cache())?),
                    PluginLimits::default(),
                )?,
            ),
        ),
        images: Arc::new(CachedImages::new(directories.image_cache(), compute.mkfs_erofs_tool())?),
        datapath: Arc::new(KernelDatapath::new(underlay, compute.nft_tool())),
        agents: Arc::new(HybridVsockAgents::new(runtime)),
        local_services: Arc::new(LocalServicesServer::start(
            compute.resolvers.clone(),
            tokio::runtime::Handle::current(),
        )),
    })
}

fn migration_transport(configuration: &NodeConfiguration) -> anyhow::Result<Option<Arc<MigrationTransport>>> {
    let Some(identity) = &configuration.identity else {
        return Ok(None);
    };
    Ok(Some(Arc::new(MigrationTransport::new(
        &identity.certificate,
        &identity.private_key,
        &identity.certificate_authority,
    )?)))
}

fn receive_migrations(
    roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    transport: Option<Arc<MigrationTransport>>,
) {
    let Some(transport) = transport else {
        return;
    };
    let address = configuration
        .network
        .as_ref()
        .map_or(std::net::Ipv4Addr::UNSPECIFIED, |network| network.address.address);
    let listen = std::net::SocketAddr::from((address, MIGRATION_PORT));
    let runtime = RuntimeDirectory::new(configuration.directories.runtime.join(VM_DIRECTORY));
    roles.spawn(supervise("migration", move || {
        let (transport, runtime) = (Arc::clone(&transport), runtime.clone());
        async move {
            match TcpListener::bind(listen).await {
                Ok(listener) => {
                    tracing::info!(%listen, "the node receives migrations");
                    transport.serve(listener, runtime).await.to_string()
                }
                Err(error) => format!("binding {listen} failed: {error}"),
            }
        }
    }));
}

fn cloud_hypervisor(
    configuration: &NodeConfiguration,
    transport: Option<Arc<MigrationTransport>>,
) -> anyhow::Result<CloudHypervisor> {
    let directories = &configuration.directories;
    let compute = &configuration.compute;
    let hypervisor = CloudHypervisor::new(CloudHypervisorSettings {
        runtime: RuntimeDirectory::new(directories.runtime.join(VM_DIRECTORY)),
        boot: BootSources {
            bundles_directory: directories.images.clone(),
            firmware: compute.firmware.clone(),
            container_kernel: compute.container_kernel.clone(),
            container_initramfs: compute.container_initramfs.clone(),
        },
        executable: std::env::current_exe()?,
        shared_readable: vec![directories.image_cache()],
    });
    Ok(match transport {
        Some(transport) => hypervisor.with_migration(transport),
        None => hypervisor,
    })
}

fn cluster_record_roles(roles: &mut JoinSet<()>, services: &Services, settings: ReconcilerSettings) {
    let (keys_store, keys_clock) = (Arc::clone(&services.store), Arc::clone(&services.clock));
    singleton_role(
        roles,
        "signing-keys",
        move |_| {
            SigningKeysController::new(
                Arc::clone(&keys_store),
                Arc::clone(&keys_clock),
                Arc::new(RandomIdentifiers),
            )
        },
        services,
        settings,
    );
    let image_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "cluster-image",
        move |_| ClusterImageController::new(Arc::clone(&image_store)),
        services,
        settings,
    );
}

fn catalog_for_node() -> Arc<dyn ImageCatalog> {
    match CanonicalCatalog::new(host_architecture()) {
        Ok(catalog) => Arc::new(catalog),
        Err(error) => {
            tracing::warn!(%error, "the image catalog is unavailable");
            Arc::new(FixedImageCatalog::default())
        }
    }
}

fn environment_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    settings: ReconcilerSettings,
    configuration: &NodeConfiguration,
) {
    let certificate_authority = configuration
        .identity
        .as_ref()
        .map(|identity| identity.certificate_authority.clone())
        .unwrap_or_default();
    let kubernetes: Arc<dyn KubernetesPlatform> = Arc::new(HttpKubernetes::default());
    let keyring = Keyring::new(Arc::clone(&services.store));
    let kubernetes_steps = KubernetesSteps::new(KubernetesEnvironment {
        platform: Arc::clone(&kubernetes),
        keyring: keyring.clone(),
        identifiers: Arc::new(RandomIdentifiers),
        settings: configuration.environments.kubernetes(&configuration.api, &certificate_authority),
    });
    let juju_steps = JujuSteps::new(JujuEnvironment {
        platform: Arc::new(WebSocketJuju::default()),
        kubernetes,
        keyring,
        identifiers: Arc::new(RandomIdentifiers),
        settings: configuration.environments.juju(&configuration.api, &certificate_authority),
    });
    step_runner_role::<KubernetesClusterChange>(roles, services, settings, Arc::new(kubernetes_steps));
    step_runner_role::<JujuControllerChange>(roles, services, settings, Arc::new(juju_steps));
    environment_loops(roles, services, settings);
}

fn environment_loops(roles: &mut JoinSet<()>, services: &Services, settings: ReconcilerSettings) {
    outcome_role::<KubernetesKinds>(roles, services, settings);
    outcome_role::<JujuKinds>(roles, services, settings);
    let failover_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "kubernetes-api-failover",
        move |_| ApiFailoverController::new(Arc::clone(&failover_store)),
        services,
        settings,
    );
    let health_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "juju-host-health",
        move |_| HostHealthController::new(Arc::clone(&health_store)),
        services,
        settings,
    );
    let (rotation_store, rotation_clock) = (Arc::clone(&services.store), Arc::clone(&services.clock));
    singleton_role(
        roles,
        "juju-credential-rotation",
        move |_| {
            CredentialRotationController::new(
                Arc::clone(&rotation_store),
                Arc::clone(&rotation_clock),
                Arc::new(RandomIdentifiers),
            )
        },
        services,
        settings,
    );
}

fn outcome_role<K: EnvironmentKinds>(
    roles: &mut JoinSet<()>,
    services: &Services,
    settings: ReconcilerSettings,
) {
    let store = Arc::clone(&services.store);
    singleton_role(
        roles,
        K::NAME,
        move |_| CreationOutcomeController::<K>::new(Arc::clone(&store)),
        services,
        settings,
    );
}

fn step_runner_role<K: RunnerKind>(
    roles: &mut JoinSet<()>,
    services: &Services,
    settings: ReconcilerSettings,
    steps: Arc<dyn StepRegistry<K>>,
) {
    let (store, clock) = (Arc::clone(&services.store), Arc::clone(&services.clock));
    singleton_role(
        roles,
        K::RUNNER,
        move |fence| {
            OperationsController::<K>::new(Arc::clone(&store), Arc::clone(&clock), Arc::clone(&steps), fence)
        },
        services,
        settings,
    );
}

fn recovery_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    settings: ReconcilerSettings,
    steps: OperationSteps,
) {
    let (health_store, health_clock) = (Arc::clone(&services.store), Arc::clone(&services.clock));
    singleton_role(
        roles,
        "node-health",
        move |_| {
            NodeHealthController::new(
                Arc::clone(&health_store),
                Arc::clone(&health_clock),
                Arc::new(RandomIdentifiers),
            )
        },
        services,
        settings,
    );
    let (operations_store, operations_clock, operations_steps) =
        (Arc::clone(&services.store), Arc::clone(&services.clock), Arc::new(steps) as Arc<dyn StepRegistry>);
    let (export_store, export_audit) = (Arc::clone(&services.store), Arc::clone(&services.audit));
    singleton_role(
        roles,
        "audit-export",
        move |fence| {
            AuditExportController::new(
                Arc::clone(&export_store),
                Arc::clone(&export_audit),
                Arc::new(BuiltInAuditSinks),
                fence,
            )
        },
        services,
        settings,
    );
    singleton_role(
        roles,
        "operations",
        move |fence| {
            OperationsController::new(
                Arc::clone(&operations_store),
                Arc::clone(&operations_clock),
                Arc::clone(&operations_steps),
                fence,
            )
        },
        services,
        settings,
    );
}

fn vm_preparation_roles(roles: &mut JoinSet<()>, services: &Services, settings: ReconcilerSettings) {
    let root_volume_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "root-volumes",
        move |_| RootVolumeController::new(Arc::clone(&root_volume_store)),
        services,
        settings,
    );
    let keys_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "authorized-keys",
        move |_| AuthorizedKeysController::new(Arc::clone(&keys_store)),
        services,
        settings,
    );
    let retired_keys_store = Arc::clone(&services.store);
    singleton_role(
        roles,
        "ssh-key-retirement",
        move |_| SshKeyRetirementController::new(Arc::clone(&retired_keys_store)),
        services,
        settings,
    );
}

fn core_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    adapters: &ComputeAdapters,
    configuration: &NodeConfiguration,
) -> anyhow::Result<()> {
    let scheduling = configuration.scheduling.settings();
    let steps =
        OperationSteps::new(Arc::clone(&adapters.storage) as Arc<dyn Storage>, Arc::new(RandomIdentifiers))
            .scheduling(scheduling)
            .image_keys(configuration.upgrade.trusted_keys()?);
    let store = || Arc::clone(&services.store);
    let settings = ReconcilerSettings::default();
    let scheduler_store = store();
    singleton_role(
        roles,
        "scheduler",
        move |fence| SchedulerController::new(Arc::clone(&scheduler_store), fence, scheduling),
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
    recovery_roles(roles, services, settings, steps);
    let external_store = store();
    singleton_role(
        roles,
        "external-network",
        move |_| ExternalNetworkController::new(Arc::clone(&external_store)),
        services,
        settings,
    );
    let revocation_store = store();
    singleton_role(
        roles,
        "certificate-revocation",
        move |_| CertificateRevocationController::new(Arc::clone(&revocation_store)),
        services,
        settings,
    );
    cluster_record_roles(roles, services, settings);
    vm_preparation_roles(roles, services, settings);
    environment_roles(roles, services, settings, configuration);
    let floating_store = store();
    singleton_role(
        roles,
        "floating-ip",
        move |_| FloatingIpController::new(Arc::clone(&floating_store)),
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
    Ok(())
}

fn compute_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    adapters: &ComputeAdapters,
    identity: &NodeIdentity,
    lease: &NodeLeaseState,
    settings: ReconcilerSettings,
) {
    let (node, underlay_address) = (&identity.name, identity.underlay_address);
    let compute = ComputeController::new(
        node.clone(),
        ComputeServices {
            lease: lease.clone(),
            clock: Arc::clone(&services.clock),
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
        underlay_address,
        Arc::clone(&services.store),
        Arc::clone(&adapters.datapath) as Arc<dyn Datapath>,
        Arc::clone(&adapters.local_services) as Arc<dyn LocalServices>,
    );
    controller_role(roles, "node-network", Arc::new(node_network), services, settings);
}

async fn core_member_services(configuration: &NodeConfiguration) -> anyhow::Result<Services> {
    let settings = VoterSettings {
        bind: configuration.state.listen,
        advertise: configuration.state.listen,
        voters: configuration.state.voters.clone(),
        tls: configuration.identity.as_ref().map(|identity| StateTls {
            certificate_authority_pem: identity.certificate_authority.clone(),
            certificate_pem: identity.certificate.clone(),
            private_key_pem: identity.private_key.clone(),
        }),
        heartbeat: configuration.state.heartbeat(),
    };
    let quorium = start_voter(&configuration.directories.data.join(STATE_DIRECTORY), &settings).await?;
    tracing::info!(voters = settings.voters.len(), "the state store is running");
    let store: Arc<dyn Store> = Arc::new(QuoriumStore::new(quorium.clone()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let node_leases = Arc::new(LocalNodeLeases::new(
        Arc::clone(&store),
        configuration.node.name.clone(),
        Arc::clone(&clock),
        Arc::new(RandomIdentifiers),
    ));
    Ok(Services {
        store,
        audit: Arc::new(BatchedAuditLog::start(QuoriumAuditLog::new(quorium.clone()))),
        clock,
        leases: Some(Arc::new(QuoriumLeases::new(quorium))),
        node_leases,
    })
}

fn subscribed_services(configuration: &NodeConfiguration) -> anyhow::Result<(Services, SubscribedStore)> {
    let identity = configuration
        .identity
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("a node outside the core needs an identity"))?;
    let client_identity = ClientIdentity {
        certificate_pem: identity.certificate.clone(),
        private_key_pem: identity.private_key.clone(),
    };
    let mut core_members: Vec<Arc<dyn Transport>> = Vec::new();
    for endpoint in &configuration.state.core_members {
        core_members.push(Arc::new(HttpTransport::with_identity(
            endpoint,
            Some(identity.certificate_authority.as_bytes()),
            Some(&client_identity),
        )?));
    }
    let cache = LocalStateCache::on_disk(&configuration.directories.data.join(STATE_CACHE_FILE));
    let client = Arc::new(NodeClient::spreading(core_members, &configuration.node.name));
    let store = SubscribedStore::new(
        configuration.node.name.clone(),
        Arc::clone(&client) as Arc<dyn CoreConnection>,
        cache,
    );
    let services = Services {
        store: Arc::new(store.clone()),
        audit: Arc::new(AuditKeptByTheCore),
        clock: Arc::new(SystemClock),
        leases: None,
        node_leases: client,
    };
    Ok((services, store))
}

fn follow_the_core(roles: &mut JoinSet<()>, store: SubscribedStore) {
    let follower = store.clone();
    roles.spawn(async move {
        loop {
            let ended = follower.follow().await;
            tracing::warn!(%ended, "the subscription to the core ended; subscribing again");
            tokio::time::sleep(RESUBSCRIBE_DELAY).await;
        }
    });
    roles.spawn(async move {
        while !store.has_synchronized() {
            tokio::time::sleep(SYNCHRONIZATION_POLL).await;
        }
        tracing::info!(objects = store.cached_object_count(), "the node received its share of the state");
        crate::diagnostics::record_serving();
        futures::future::pending::<()>().await;
    });
}

fn lease_roles(
    roles: &mut JoinSet<()>,
    services: &Services,
    lease: &NodeLeaseState,
    terms: LeaseTerms,
    watchdog: Option<Arc<dyn Watchdog>>,
) {
    let (authority, clock, state) =
        (Arc::clone(&services.node_leases), Arc::clone(&services.clock), lease.clone());
    roles.spawn(async move {
        let keeper = LeaseKeeper {
            authority: authority.as_ref(),
            clock: clock.as_ref(),
            state: &state,
            settings: LeaseKeeperSettings::for_terms(terms),
        };
        keeper.keep().await;
    });
    if let Some(watchdog) = watchdog {
        let (clock, state) = (Arc::clone(&services.clock), lease.clone());
        roles.spawn(async move {
            feed_while_leased(watchdog.as_ref(), &state, clock.as_ref(), terms.watchdog_interval()).await;
        });
    }
}

fn boot_slot_role(
    roles: &mut JoinSet<()>,
    node: &NodeName,
    services: &Services,
    lease: &NodeLeaseState,
    boot_slots: Arc<dyn BootSlots>,
    settings: BootSlotsSettings,
) {
    let controller = Arc::new(BootSlotsController::new(
        node.clone(),
        BootSlotsServices {
            store: Arc::clone(&services.store),
            clock: Arc::clone(&services.clock),
            boot_slots,
            leases: Arc::clone(&services.node_leases),
            lease: lease.clone(),
        },
        settings,
    ));
    controller_role(roles, "boot-slots", controller, services, ReconcilerSettings::default());
}

fn fenced_by_lease(services: &Services, node: &NodeName, lease: &NodeLeaseState) -> Services {
    Services {
        store: Arc::new(LeaseFencedStore::new(
            Arc::clone(&services.store),
            node.clone(),
            lease.clone(),
            Arc::clone(&services.clock),
        )),
        audit: Arc::clone(&services.audit),
        clock: Arc::clone(&services.clock),
        leases: services.leases.clone(),
        node_leases: Arc::clone(&services.node_leases),
    }
}

pub async fn run(configuration: NodeConfiguration, boot: Option<BootServices>) -> anyhow::Result<()> {
    let directories = &configuration.directories;
    for directory in
        [&directories.data, &directories.runtime, &directories.image_cache(), &directories.volumes()]
    {
        std::fs::create_dir_all(directory)?;
    }
    let mut roles = JoinSet::new();
    let is_core = configuration.has_role(NodeRole::Core);
    let services = if is_core {
        core_member_services(&configuration).await?
    } else {
        let (services, subscribed) = subscribed_services(&configuration)?;
        follow_the_core(&mut roles, subscribed);
        services
    };
    let transport = migration_transport(&configuration)?;
    let hypervisor = Arc::new(cloud_hypervisor(&configuration, transport.clone())?);
    let underlay = if configuration.has_role(NodeRole::Compute) {
        amafu_network::bring_up_underlay(configuration.network.as_ref()).await?
    } else {
        String::new()
    };
    let adapters = compute_adapters(&configuration, &hypervisor, underlay)?;
    let spread = configuration.node.name.spread();
    let responsive = ReconcilerSettings {
        periodic_resynchronization: Duration::from_secs(2),
        ..ReconcilerSettings::default()
    }
    .spread_by(spread);
    if is_core {
        start_api(&mut roles, &configuration, &services)?;
        core_roles(&mut roles, &services, &adapters, &configuration)?;
    }
    let identity = node_identity(&configuration)?;
    let lease = NodeLeaseState::new();
    let (modules, watchdog, boot_slots) = match boot {
        Some(boot) => (Some(boot.modules), Some(boot.watchdog), Some(boot.boot_slots)),
        None => (None, None, None),
    };
    lease_roles(&mut roles, &services, &lease, configuration.fencing.terms(), watchdog);
    let node_services = fenced_by_lease(&services, &identity.name, &lease);
    let mut node = NodeController::new(
        identity.clone(),
        Arc::clone(&node_services.store),
        Arc::clone(&node_services.clock),
    );
    if let Some(boot_slots) = boot_slots {
        node = node.with_boot_slots(Arc::clone(&boot_slots));
        boot_slot_role(
            &mut roles,
            &identity.name,
            &node_services,
            &lease,
            boot_slots,
            configuration.upgrade.boot_slots(),
        );
    }
    let node = Arc::new(node);
    let heartbeat = ReconcilerSettings {
        periodic_resynchronization: HEARTBEAT_INTERVAL / 2,
        ..ReconcilerSettings::default()
    }
    .spread_by(spread);
    controller_role(&mut roles, "node", node, &node_services, heartbeat);
    if let Some(modules) = modules {
        modules.await;
    }
    if configuration.has_role(NodeRole::Compute) {
        if let Err(error) = prepare_kvm_access() {
            tracing::warn!(%error, "VM processes cannot be given access to /dev/kvm");
        }
        compute_roles(&mut roles, &node_services, &adapters, &identity, &lease, responsive);
        receive_migrations(&mut roles, &configuration, transport);
    }
    crate::diagnostics::start(&mut roles, &configuration, &services, &adapters);
    while let Some(ended) = roles.join_next().await {
        tracing::error!(?ended, "a role supervisor ended");
    }
    anyhow::bail!("every role ended")
}
