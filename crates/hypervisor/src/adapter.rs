use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use amafu_domain::ObjectId;
use amafu_interfaces::{ActionError, Hypervisor, InstanceState, RunningInstance, VmLaunchSpecification};
use rustix::process::Signal;

use crate::images::{ImageError, find_image};
use crate::plan::VmProcessPlan;
use crate::process::{is_running_vm_process, send_signal};
use crate::runtime::{RuntimeDirectory, VmDirectory};

const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
pub const VM_PROCESS_COMMAND: &str = "vm-process";
pub const DIRECTORY_ARGUMENT: &str = "--directory";

pub struct CloudHypervisor {
    runtime: RuntimeDirectory,
    images_directory: PathBuf,
    executable: PathBuf,
}

fn is_alive(directory: &VmDirectory) -> bool {
    directory.process_id().is_some_and(|process_id| is_running_vm_process(process_id, directory.path()))
}

fn instance_state(directory: &VmDirectory) -> InstanceState {
    match (is_alive(directory), directory.has_booted()) {
        (false, _) => InstanceState::Exited,
        (true, false) => InstanceState::Booting,
        (true, true) => InstanceState::Running,
    }
}

fn io_failure(action: &str) -> impl Fn(std::io::Error) -> ActionError + '_ {
    move |error| ActionError::retryable(format!("{action}: {error}"))
}

async fn wait_for_exit(directory: &VmDirectory, timeout: Duration) -> bool {
    let started = Instant::now();
    while is_alive(directory) {
        if started.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
    }
    true
}

impl CloudHypervisor {
    #[must_use]
    pub fn new(runtime: RuntimeDirectory, images_directory: PathBuf, executable: PathBuf) -> Self {
        Self { runtime, images_directory, executable }
    }

    fn spawn(&self, directory: &VmDirectory, plan: &VmProcessPlan) -> Result<(), ActionError> {
        if directory.path().exists() {
            fs::remove_dir_all(directory.path()).map_err(io_failure("removing a stale VM directory"))?;
        }
        fs::create_dir_all(directory.path()).map_err(io_failure("creating the VM directory"))?;
        let plan_json = serde_json::to_vec_pretty(plan).map_err(ActionError::fatal)?;
        fs::write(directory.plan_file(), plan_json).map_err(io_failure("writing the VM plan"))?;
        let log = File::create(directory.process_log()).map_err(io_failure("creating the VM process log"))?;
        let error_log = log.try_clone().map_err(io_failure("sharing the VM process log"))?;
        let child = Command::new(&self.executable)
            .arg(VM_PROCESS_COMMAND)
            .arg(DIRECTORY_ARGUMENT)
            .arg(directory.path())
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(error_log)
            .spawn()
            .map_err(io_failure("starting the VM process"))?;
        fs::write(directory.process_id_file(), child.id().to_string())
            .map_err(io_failure("recording the VM process"))
    }
}

#[async_trait::async_trait]
impl Hypervisor for CloudHypervisor {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        let image = find_image(&self.images_directory, &launch.image).map_err(|error| match error {
            ImageError::NotFound(_) => ActionError::invalid(error),
            ImageError::Unreadable { .. } => ActionError::retryable(error),
        })?;
        let directory = self.runtime.vm(&launch.vm_id);
        if is_alive(&directory) {
            return Ok(());
        }
        let plan = VmProcessPlan { launch: launch.clone(), image, serial_log: directory.serial_log() };
        self.spawn(&directory, &plan)?;
        tracing::info!(vm_id = %launch.vm_id, "started a VM process");
        Ok(())
    }

    async fn stop(&self, vm_id: &ObjectId, grace_period: Duration) -> Result<(), ActionError> {
        let directory = self.runtime.vm(vm_id);
        if !directory.path().exists() {
            return Ok(());
        }
        if let Some(process_id) = directory.process_id().filter(|_| is_alive(&directory)) {
            send_signal(process_id, Signal::TERM).map_err(io_failure("asking the VM process to stop"))?;
            if !wait_for_exit(&directory, grace_period).await {
                send_signal(process_id, Signal::KILL).map_err(io_failure("killing the VM process"))?;
                if !wait_for_exit(&directory, KILL_TIMEOUT).await {
                    return Err(ActionError::retryable(format!("VM process {process_id} did not exit")));
                }
            }
        }
        fs::remove_dir_all(directory.path()).map_err(io_failure("removing the VM directory"))?;
        tracing::info!(%vm_id, "stopped a VM process");
        Ok(())
    }

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError> {
        let vms = self.runtime.vms().map_err(io_failure("listing VM directories"))?;
        Ok(vms
            .iter()
            .map(|(vm_id, directory)| RunningInstance { vm_id: *vm_id, state: instance_state(directory) })
            .collect())
    }
}
