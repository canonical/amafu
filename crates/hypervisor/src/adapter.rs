use std::fs::{self, File};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use amafu_domain::ObjectId;
use amafu_interfaces::{ActionError, Hypervisor, InstanceState, RunningInstance, VmLaunchSpecification};
use rustix::fs::{Mode, chmod};
use rustix::process::Signal;

use crate::boot::{BootError, BootSources};
use crate::control::{self, ControlChannel, Instructions, Work};
use crate::plan::{DiskFormat, VmProcessPlan};
use crate::probe::{Attempt, ProbeOutcome, outcome_of};
use crate::process::{is_running_vm_process, send_signal};
use crate::runtime::{RuntimeDirectory, VmDirectory};
use crate::sandbox::{
    FIRST_VM_USER_ID, Restrictions, VM_GROUP_ID, allocate_user_id, create_cgroup, distinct_paths, give_to,
    place_in_cgroup, remove_cgroup,
};

const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_USER_ID: u32 = FIRST_VM_USER_ID - 1;
const PROBE_MEMORY_MEBIBYTES: u64 = 64;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const SYSTEM_READABLE: &[&str] = &["/dev/urandom", "/dev/null", "/proc", "/sys"];
const SYSTEM_WRITABLE: &[&str] = &["/dev/kvm"];
pub const VM_PROCESS_COMMAND: &str = "vm-process";
pub const DIRECTORY_ARGUMENT: &str = "--directory";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudHypervisorSettings {
    pub runtime: RuntimeDirectory,
    pub boot: BootSources,
    pub executable: PathBuf,
    pub shared_readable: Vec<PathBuf>,
}

pub struct CloudHypervisor {
    settings: CloudHypervisorSettings,
    probes: AtomicU64,
}

struct SandboxedLaunch<'launch> {
    directory: &'launch VmDirectory,
    user_id: u32,
    cgroup: &'launch Path,
    instructions: Instructions,
    descriptors: Vec<OwnedFd>,
    output: Stdio,
    error_output: Stdio,
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

fn boot_failure(error: BootError) -> ActionError {
    match error {
        BootError::Image(crate::images::ImageError::Unreadable { .. }) => ActionError::retryable(error),
        BootError::Image(_) | BootError::Missing(_) => ActionError::invalid(error),
    }
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

fn vm_cgroup(vm_id: &ObjectId) -> String {
    format!("vm-{vm_id}")
}

fn open_tap(name: &str) -> std::io::Result<OwnedFd> {
    let device =
        tun_rs::DeviceBuilder::new().name(name).layer(tun_rs::Layer::L2).offload(true).build_sync()?;
    device.as_fd().try_clone_to_owned()
}

fn system_paths(paths: &[&str]) -> impl Iterator<Item = PathBuf> {
    paths.iter().map(PathBuf::from)
}

fn prepare_directory(directory: &VmDirectory, user_id: u32) -> std::io::Result<()> {
    if directory.path().exists() {
        fs::remove_dir_all(directory.path())?;
    }
    fs::create_dir_all(directory.path())?;
    directory.record_user_id(user_id)?;
    give_to(directory.path(), user_id)?;
    chmod(directory.path(), Mode::from_raw_mode(PRIVATE_DIRECTORY_MODE)).map_err(Into::into)
}

impl CloudHypervisor {
    #[must_use]
    pub fn new(settings: CloudHypervisorSettings) -> Self {
        Self { settings, probes: AtomicU64::new(0) }
    }

    fn launch_sandboxed(&self, launch: SandboxedLaunch<'_>) -> Result<Child, ActionError> {
        let channel = ControlChannel::new().map_err(io_failure("creating the VM process channel"))?;
        let mut child = Command::new(&self.settings.executable)
            .arg(VM_PROCESS_COMMAND)
            .arg(DIRECTORY_ARGUMENT)
            .arg(launch.directory.path())
            .current_dir(launch.directory.path())
            .uid(launch.user_id)
            .gid(VM_GROUP_ID)
            .env_clear()
            .stdin(Stdio::from(channel.child))
            .stdout(launch.output)
            .stderr(launch.error_output)
            .spawn()
            .map_err(io_failure("starting the VM process"))?;
        let handed_over = place_in_cgroup(launch.cgroup, child.id())
            .and_then(|()| {
                let descriptors: Vec<_> = launch.descriptors.iter().map(AsFd::as_fd).collect();
                control::send(&channel.parent, &launch.instructions, &descriptors)
            })
            .map_err(io_failure("handing the VM to its process"));
        if let Err(error) = handed_over {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(child)
    }

    fn restrictions(&self, plan: &VmProcessPlan, directory: &VmDirectory) -> Restrictions {
        let (read_only, writable): (Vec<_>, Vec<_>) =
            plan.launch.disks.iter().partition(|disk| disk.is_read_only);
        Restrictions {
            readable: distinct_paths(
                plan.boot
                    .paths()
                    .into_iter()
                    .chain(self.settings.shared_readable.iter().cloned())
                    .chain(read_only.into_iter().map(|disk| disk.path.clone()))
                    .chain(system_paths(SYSTEM_READABLE)),
            ),
            writable: distinct_paths(
                std::iter::once(directory.path().to_path_buf())
                    .chain(writable.into_iter().map(|disk| disk.path.clone()))
                    .chain(system_paths(SYSTEM_WRITABLE)),
            ),
        }
    }

    fn spawn(&self, directory: &VmDirectory, plan: VmProcessPlan) -> Result<(), ActionError> {
        let launch = &plan.launch;
        let user_id = allocate_user_id(&self.settings.runtime, &launch.vm_id)
            .map_err(io_failure("allocating a VM identity"))?;
        prepare_directory(directory, user_id).map_err(io_failure("preparing the VM directory"))?;
        let plan_json = serde_json::to_vec_pretty(&plan).map_err(ActionError::fatal)?;
        fs::write(directory.plan_file(), plan_json).map_err(io_failure("writing the VM plan"))?;
        for disk in launch.disks.iter().filter(|disk| !disk.is_read_only) {
            give_to(&disk.path, user_id).map_err(io_failure("handing a disk to the VM"))?;
            chmod(&disk.path, Mode::from_raw_mode(PRIVATE_FILE_MODE))
                .map_err(|error| ActionError::retryable(format!("protecting a disk: {error}")))?;
        }
        let taps = launch
            .network_interfaces
            .iter()
            .map(|interface| open_tap(&interface.tap))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(io_failure("opening the VM taps"))?;
        let cgroup = create_cgroup(&vm_cgroup(&launch.vm_id), launch.cpu_count, launch.memory_mebibytes)
            .map_err(io_failure("creating the VM cgroup"))?;
        let log = File::create(directory.process_log()).map_err(io_failure("creating the VM process log"))?;
        let error_log = log.try_clone().map_err(io_failure("sharing the VM process log"))?;
        let restrictions = self.restrictions(&plan, directory);
        let child = self.launch_sandboxed(SandboxedLaunch {
            directory,
            user_id,
            cgroup: &cgroup,
            instructions: Instructions { restrictions, work: Work::Boot { plan: Box::new(plan) } },
            descriptors: taps,
            output: Stdio::from(log),
            error_output: Stdio::from(error_log),
        })?;
        fs::write(directory.process_id_file(), child.id().to_string())
            .map_err(io_failure("recording the VM process"))
    }

    pub async fn probe(&self, attempt: Attempt) -> Result<ProbeOutcome, ActionError> {
        let name = format!("probe-{}", self.probes.fetch_add(1, Ordering::Relaxed));
        let directory = VmDirectory::at(&self.settings.runtime.root().join(&name));
        prepare_directory(&directory, PROBE_USER_ID).map_err(io_failure("preparing the probe directory"))?;
        let cgroup = create_cgroup(&name, 1, PROBE_MEMORY_MEBIBYTES)
            .map_err(io_failure("creating the probe cgroup"))?;
        let restrictions = Restrictions {
            readable: distinct_paths(
                self.settings.shared_readable.iter().cloned().chain(system_paths(SYSTEM_READABLE)),
            ),
            writable: vec![directory.path().to_path_buf()],
        };
        let launched = self.launch_sandboxed(SandboxedLaunch {
            directory: &directory,
            user_id: PROBE_USER_ID,
            cgroup: &cgroup,
            instructions: Instructions { restrictions, work: Work::Probe { attempt } },
            descriptors: Vec::new(),
            output: Stdio::piped(),
            error_output: Stdio::null(),
        });
        let finished = match launched {
            Ok(child) => tokio::task::spawn_blocking(move || child.wait_with_output())
                .await
                .map_err(ActionError::retryable)
                .and_then(|output| output.map_err(io_failure("waiting for the probe"))),
            Err(error) => Err(error),
        };
        let _ = fs::remove_dir_all(directory.path());
        let _ = remove_cgroup(&name);
        let output = finished?;
        outcome_of(output.status, &String::from_utf8_lossy(&output.stdout)).map_err(ActionError::retryable)
    }
}

#[async_trait::async_trait]
impl Hypervisor for CloudHypervisor {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError> {
        let boot = self.settings.boot.resolve(&launch.payload).map_err(boot_failure)?;
        let directory = self.settings.runtime.vm(&launch.vm_id);
        if is_alive(&directory) {
            return Ok(());
        }
        let disk_formats = launch
            .disks
            .iter()
            .map(|disk| DiskFormat::of(&disk.path))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(io_failure("reading a disk"))?;
        let plan = VmProcessPlan {
            launch: launch.clone(),
            boot,
            disk_formats,
            serial_log: directory.serial_log(),
            vsock_socket: directory.vsock_socket(),
        };
        self.spawn(&directory, plan)?;
        tracing::info!(vm_id = %launch.vm_id, "started a VM process");
        Ok(())
    }

    async fn stop(&self, vm_id: &ObjectId, grace_period: Duration) -> Result<(), ActionError> {
        let directory = self.settings.runtime.vm(vm_id);
        if !directory.path().exists() {
            return remove_cgroup(&vm_cgroup(vm_id)).map_err(io_failure("removing the VM cgroup"));
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
        remove_cgroup(&vm_cgroup(vm_id)).map_err(io_failure("removing the VM cgroup"))?;
        tracing::info!(%vm_id, "stopped a VM process");
        Ok(())
    }

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError> {
        let vms = self.settings.runtime.vms().map_err(io_failure("listing VM directories"))?;
        Ok(vms
            .iter()
            .map(|(vm_id, directory)| RunningInstance { vm_id: *vm_id, state: instance_state(directory) })
            .collect())
    }
}
