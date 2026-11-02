use std::sync::OnceLock;

use tokio::task::JoinSet;

use crate::roles::{ComputeAdapters, Services};
use amafu_node::configuration::NodeConfiguration;

static SECONDS_FROM_KERNEL_START_TO_SERVING: OnceLock<f64> = OnceLock::new();

pub fn record_serving() {
    if let Some(uptime) = amafu_system::host::uptime_seconds(std::path::Path::new("/proc")) {
        let _ = SECONDS_FROM_KERNEL_START_TO_SERVING.set(uptime);
    }
}

#[cfg(not(feature = "diagnostics"))]
pub fn start(
    _roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    _services: &Services,
    _adapters: &ComputeAdapters,
) {
    if configuration.diagnostics.is_some() {
        tracing::warn!("diagnostics are configured but this is a release build, so they stay off");
    }
}

#[cfg(feature = "diagnostics")]
pub fn start(
    roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    services: &Services,
    adapters: &ComputeAdapters,
) {
    if let Some(diagnostics) = &configuration.diagnostics {
        let listen = diagnostics.listen;
        let application = test_channel::router(configuration, services, adapters);
        roles.spawn(async move {
            match tokio::net::TcpListener::bind(listen).await {
                Ok(listener) => {
                    tracing::warn!(%listen, "the test-only diagnostic channel is serving");
                    if let Err(error) = axum::serve(listener, application).await {
                        tracing::error!(%error, "the diagnostic channel stopped");
                    }
                }
                Err(error) => tracing::error!(%listen, %error, "the diagnostic channel cannot bind"),
            }
        });
    }
}

#[cfg(feature = "diagnostics")]
mod test_channel {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use amafu_api::diagnostics::{
        ATTACHMENTS_PATH, AUDIT_PATH, BOOT_PATH, BootFacts, ContractOutcome, HYPERVISOR_CONTRACT_PATH,
        INSTANCES_PATH, NETWORK_PATH, NodeAttachment, NodeInstance, NodeNetworkFacts, NodePort, SANDBOX_PATH,
        SandboxCheck,
    };
    use amafu_domain::{ObjectId, ObjectName};
    use amafu_hypervisor::{Attempt, CloudHypervisor, HybridVsockAgents, RuntimeDirectory, VmProcessPlan};
    use amafu_interfaces::contract::hypervisor::hypervisor_contract;
    use amafu_interfaces::{
        AuditEntry, AuditLog, Datapath, GuestAgents, Hypervisor, InstanceState, Storage, StorageProtocol,
        VmLaunchSpecification,
    };
    use amafu_network::{KernelDatapath, LocalServicesServer};
    use amafu_storage::BuiltInStorage;
    use axum::extract::{Path as UrlPath, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use http::StatusCode;

    use super::SECONDS_FROM_KERNEL_START_TO_SERVING;
    use crate::roles::{ComputeAdapters, Services};
    use amafu_node::configuration::NodeConfiguration;

    const TEST_IMAGE: &str = "test-guest";
    const SERIAL_LOG_FILE: &str = "serial.log";
    const PROCESS_LOG_FILE: &str = "vm-process.log";
    const PLAN_FILE: &str = "plan.json";
    const TAIL_BYTES: usize = 2048;
    const NODE_CONFIGURATION: &str = "/run/amafu/boot/amafu/node-configuration.yaml";
    const PROBE_MEMORY_MEBIBYTES: usize = 1024;

    fn tail(path: &Path) -> String {
        let contents = std::fs::read(path).unwrap_or_default();
        let start = contents.len().saturating_sub(TAIL_BYTES);
        String::from_utf8_lossy(&contents[start..]).into_owned()
    }

    #[derive(Clone)]
    struct ChannelState {
        hypervisor: Arc<CloudHypervisor>,
        agents: Arc<HybridVsockAgents>,
        datapath: Arc<KernelDatapath>,
        local_services: Arc<LocalServicesServer>,
        storage: Arc<BuiltInStorage>,
        audit: Arc<dyn AuditLog>,
        runtime: RuntimeDirectory,
        nft: PathBuf,
        data: PathBuf,
    }

    pub fn router(
        configuration: &NodeConfiguration,
        services: &Services,
        adapters: &ComputeAdapters,
    ) -> Router {
        Router::new()
            .route(INSTANCES_PATH, get(instances))
            .route(BOOT_PATH, get(boot))
            .route(NETWORK_PATH, get(network))
            .route(ATTACHMENTS_PATH, get(attachments))
            .route(SANDBOX_PATH, post(sandbox))
            .route(HYPERVISOR_CONTRACT_PATH, post(contract))
            .route(&format!("{AUDIT_PATH}/{{request_id}}"), get(audit_entry))
            .with_state(ChannelState {
                hypervisor: Arc::clone(&adapters.hypervisor),
                agents: Arc::clone(&adapters.agents),
                datapath: Arc::clone(&adapters.datapath),
                local_services: Arc::clone(&adapters.local_services),
                storage: Arc::clone(&adapters.storage),
                audit: Arc::clone(&services.audit),
                runtime: RuntimeDirectory::new(
                    configuration.directories.runtime.join(crate::roles::VM_DIRECTORY),
                ),
                nft: configuration.compute.nft.clone(),
                data: configuration.directories.data.clone(),
            })
    }

    async fn audit_entry(
        State(state): State<ChannelState>,
        UrlPath(request_id): UrlPath<uuid::Uuid>,
    ) -> Result<Json<AuditEntry>, StatusCode> {
        match state.audit.find(request_id).await {
            Ok(Some(entry)) => Ok(Json(entry)),
            Ok(None) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    async fn observed_instance(
        state: &ChannelState,
        vm_id: ObjectId,
        instance_state: InstanceState,
    ) -> NodeInstance {
        let directory = state.runtime.vm(&vm_id).path().to_path_buf();
        let plan: Option<VmProcessPlan> = std::fs::read(directory.join(PLAN_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let launch = plan.map(|plan| plan.launch);
        let has_guest_agent = launch.as_ref().is_some_and(|launch| launch.has_guest_agent);
        let workload = if has_guest_agent && instance_state == InstanceState::Running {
            state.agents.workload(&vm_id).await.ok()
        } else {
            None
        };
        NodeInstance {
            vm_id,
            state: instance_state,
            console_tail: tail(&directory.join(SERIAL_LOG_FILE)),
            process_log_tail: tail(&directory.join(PROCESS_LOG_FILE)),
            disks: launch
                .iter()
                .flat_map(|launch| launch.disks.iter().map(|disk| disk.path.clone()))
                .collect(),
            mac_addresses: launch
                .iter()
                .flat_map(|launch| launch.network_interfaces.iter().map(|interface| interface.mac))
                .collect(),
            workload,
        }
    }

    async fn instances(State(state): State<ChannelState>) -> Json<Vec<NodeInstance>> {
        let hypervisor = Arc::clone(&state.hypervisor) as Arc<dyn Hypervisor>;
        let mut instances = Vec::new();
        for running in hypervisor.running().await.unwrap_or_default() {
            instances.push(observed_instance(&state, running.vm_id, running.state).await);
        }
        Json(instances)
    }

    async fn network(State(state): State<ChannelState>) -> Result<Json<NodeNetworkFacts>, StatusCode> {
        let realized = state.datapath.current().await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let served = state.local_services.served();
        let ports = served
            .ports
            .iter()
            .filter(|port| realized.ports.contains(&port.key))
            .map(|port| NodePort {
                vm_id: port.key.vm_id,
                index: port.key.index,
                segment: port.segment.0.clone(),
                mac: port.mac,
                address: port.address,
                tap: port.key.tap_name(),
            })
            .collect();
        let ruleset = tokio::process::Command::new(&state.nft)
            .args(["list", "ruleset"])
            .output()
            .await
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default();
        Ok(Json(NodeNetworkFacts { ports, ruleset }))
    }

    async fn attachments(State(state): State<ChannelState>) -> Result<Json<Vec<NodeAttachment>>, StatusCode> {
        let mut attachments = Vec::new();
        for protocol in [StorageProtocol::LocalFile, StorageProtocol::Nfs] {
            let attacher = state.storage.attacher(protocol).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            let current = attacher.attached().await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            attachments.extend(
                current
                    .into_iter()
                    .map(|(target, path)| NodeAttachment { volume: format!("{target:?}"), path }),
            );
        }
        Ok(Json(attachments))
    }

    fn sandbox_checks(data: &Path) -> Vec<(&'static str, Attempt)> {
        vec![
            ("read the node configuration", Attempt::ReadFile { path: PathBuf::from(NODE_CONFIGURATION) }),
            ("read the node state", Attempt::ReadFile { path: data.join("node-id") }),
            ("write the node data directory", Attempt::WriteFile { path: data.join("escaped") }),
            ("open an internet socket", Attempt::OpenInternetSocket),
            ("open a netlink socket", Attempt::OpenNetlinkSocket),
            ("mount a file system", Attempt::Mount),
            ("load a kernel module", Attempt::LoadModule),
            ("exceed the memory limit", Attempt::AllocateMemory { mebibytes: PROBE_MEMORY_MEBIBYTES }),
        ]
    }

    async fn sandbox(State(state): State<ChannelState>) -> Json<Vec<SandboxCheck>> {
        let mut checks = Vec::new();
        for (name, attempt) in sandbox_checks(&state.data) {
            let check = match state.hypervisor.probe(attempt).await {
                Ok(outcome) => SandboxCheck {
                    name: name.to_owned(),
                    is_blocked: outcome.is_blocked(),
                    outcome: format!("{outcome:?}"),
                },
                Err(error) => SandboxCheck {
                    name: name.to_owned(),
                    is_blocked: false,
                    outcome: format!("the probe failed: {error}"),
                },
            };
            checks.push(check);
        }
        Json(checks)
    }

    async fn boot() -> Json<BootFacts> {
        Json(BootFacts {
            is_secure_boot_enabled: amafu_system::firmware::is_secure_boot_enabled(Path::new(
                "/sys/firmware/efi/efivars",
            )),
            is_tpm_present: amafu_system::firmware::is_tpm_present(Path::new("/sys")),
            is_kvm_available: Path::new("/dev/kvm").exists(),
            seconds_from_kernel_start_to_serving: SECONDS_FROM_KERNEL_START_TO_SERVING.get().copied(),
        })
    }

    async fn contract(State(state): State<ChannelState>) -> Json<ContractOutcome> {
        let Ok(image) = ObjectName::new(TEST_IMAGE) else {
            return Json(ContractOutcome { violation: Some(String::from("the test image name is invalid")) });
        };
        let launch =
            VmLaunchSpecification::of_bundle(ObjectId::from_uuid(uuid::Uuid::new_v4()), 1, 256, image);
        let hypervisor = Arc::clone(&state.hypervisor) as Arc<dyn Hypervisor>;
        let outcome = hypervisor_contract(hypervisor.as_ref(), &launch).await;
        Json(ContractOutcome { violation: outcome.err().map(|violation| violation.to_string()) })
    }
}
