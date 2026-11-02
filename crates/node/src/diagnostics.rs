use std::sync::OnceLock;

use tokio::task::JoinSet;

use crate::roles::Services;
use amafu_node::configuration::NodeConfiguration;

static SECONDS_FROM_KERNEL_START_TO_SERVING: OnceLock<f64> = OnceLock::new();

pub fn record_serving() {
    if let Some(uptime) = amafu_system::host::uptime_seconds(std::path::Path::new("/proc")) {
        let _ = SECONDS_FROM_KERNEL_START_TO_SERVING.set(uptime);
    }
}

#[cfg(not(feature = "diagnostics"))]
pub fn start(_roles: &mut JoinSet<()>, configuration: &NodeConfiguration, _services: &Services) {
    if configuration.diagnostics.is_some() {
        tracing::warn!("diagnostics are configured but this is a release build, so they stay off");
    }
}

#[cfg(feature = "diagnostics")]
pub fn start(roles: &mut JoinSet<()>, configuration: &NodeConfiguration, services: &Services) {
    if let Some(diagnostics) = &configuration.diagnostics {
        let listen = diagnostics.listen;
        let application = test_channel::router(services);
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
        AUDIT_PATH, BOOT_PATH, BootFacts, ContractOutcome, HYPERVISOR_CONTRACT_PATH, INSTANCES_PATH,
        NodeInstance,
    };
    use amafu_domain::{ObjectId, ObjectName};
    use amafu_interfaces::contract::hypervisor::hypervisor_contract;
    use amafu_interfaces::{AuditEntry, AuditLog, Hypervisor, InstanceState, VmLaunchSpecification};
    use axum::extract::{Path as UrlPath, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use http::StatusCode;

    use super::SECONDS_FROM_KERNEL_START_TO_SERVING;
    use crate::roles::Services;

    const TEST_IMAGE: &str = "test-guest";
    const VM_PROCESS_COMMAND: &[u8] = b"vm-process";
    const BOOTED_FILE: &str = "booted";
    const SERIAL_LOG_FILE: &str = "serial.log";
    const CONSOLE_TAIL_BYTES: usize = 2048;

    fn console_tail(directory: &Path) -> String {
        let contents = std::fs::read(directory.join(SERIAL_LOG_FILE)).unwrap_or_default();
        let start = contents.len().saturating_sub(CONSOLE_TAIL_BYTES);
        String::from_utf8_lossy(&contents[start..]).into_owned()
    }

    #[derive(Clone)]
    struct ChannelState {
        hypervisor: Arc<dyn Hypervisor>,
        audit: Arc<dyn AuditLog>,
    }

    pub fn router(services: &Services) -> Router {
        Router::new()
            .route(INSTANCES_PATH, get(instances))
            .route(BOOT_PATH, get(boot))
            .route(HYPERVISOR_CONTRACT_PATH, post(contract))
            .route(&format!("{AUDIT_PATH}/{{request_id}}"), get(audit_entry))
            .with_state(ChannelState {
                hypervisor: Arc::clone(&services.hypervisor),
                audit: Arc::clone(&services.audit),
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

    fn vm_directory_of(command_line: &[u8]) -> Option<PathBuf> {
        let arguments: Vec<&[u8]> = command_line.split(|byte| *byte == 0).collect();
        if arguments.get(1).copied() != Some(VM_PROCESS_COMMAND) {
            return None;
        }
        let directory = std::str::from_utf8(arguments.get(3)?).ok()?;
        Some(PathBuf::from(directory))
    }

    fn observed_instances() -> Vec<NodeInstance> {
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut instances: Vec<NodeInstance> = processes
            .filter_map(Result::ok)
            .filter_map(|process| {
                let stat = std::fs::read_to_string(process.path().join("stat")).ok()?;
                if stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next()) == Some("Z") {
                    return None;
                }
                let directory = vm_directory_of(&std::fs::read(process.path().join("cmdline")).ok()?)?;
                let vm_id: ObjectId = directory.file_name()?.to_str()?.parse().ok()?;
                let state = if directory.join(BOOTED_FILE).is_file() {
                    InstanceState::Running
                } else {
                    InstanceState::Booting
                };
                Some(NodeInstance { vm_id, state, console_tail: console_tail(&directory) })
            })
            .collect();
        instances.sort_by_key(|instance| instance.vm_id);
        instances
    }

    async fn instances() -> Json<Vec<NodeInstance>> {
        Json(observed_instances())
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
        let launch = VmLaunchSpecification {
            vm_id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            cpu_count: 1,
            memory_mebibytes: 256,
            image,
        };
        let outcome = hypervisor_contract(state.hypervisor.as_ref(), &launch).await;
        Json(ContractOutcome { violation: outcome.err().map(|violation| violation.to_string()) })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_vm_processes_are_recognized() {
            assert_eq!(
                vm_directory_of(b"/usr/bin/amafu-node\0vm-process\0--directory\0/run/amafu/vms/abc\0"),
                Some(PathBuf::from("/run/amafu/vms/abc"))
            );
            assert_eq!(vm_directory_of(b"/usr/bin/amafu-node\0run\0"), None);
        }
    }
}
