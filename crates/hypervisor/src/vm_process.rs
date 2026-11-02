use std::os::fd::{IntoRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::mpsc;

use seccompiler::SeccompAction;
use vmm::api::{ApiAction, ApiRequest, VmBoot, VmCreate};
use vmm::config::VmParams;
use vmm::vm_config::VmConfig;
use vmm_sys_util::eventfd::{EFD_NONBLOCK, EventFd};
use vmm_sys_util::signal::block_signal;

use crate::boot::BootFiles;
use crate::control::{self, Work};
use crate::plan::VmProcessPlan;
use crate::probe::{self, Attempt, DENIED_EXIT_CODE, PERMITTED_EXIT_CODE};
use crate::runtime::VmDirectory;
use crate::sandbox::{self, SandboxError};

#[derive(Debug, thiserror::Error)]
pub enum VmProcessError {
    #[error("the VM plan cannot be read: {0}")]
    Plan(String),
    #[error("the hypervisor is unavailable: {0}")]
    Hypervisor(String),
    #[error("the VMM could not start: {0}")]
    Vmm(String),
    #[error("the VM configuration is invalid: {0}")]
    Configuration(String),
    #[error("the VM could not be created or booted: {0}")]
    Boot(String),
    #[error("the VMM stopped with an error: {0}")]
    Exit(String),
    #[error(transparent)]
    Sandbox(SandboxError),
}

fn debug_message(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

fn listed(options: &[String]) -> Option<Vec<&str>> {
    (!options.is_empty()).then(|| options.iter().map(String::as_str).collect())
}

fn configuration(plan: &VmProcessPlan, tap_descriptors: &[RawFd]) -> Result<VmConfig, VmProcessError> {
    let cpus = plan.cpus_option();
    let memory = plan.memory_option();
    let serial = plan.serial_option();
    let disks = plan.disk_options();
    let networks = plan.network_options(tap_descriptors);
    let vsock = plan.vsock_option();
    let platform = plan.platform_option();
    let (kernel, initramfs, command_line, firmware) = match &plan.boot {
        BootFiles::Kernel { kernel, initramfs, command_line } => (
            Some(kernel.to_string_lossy()),
            initramfs.as_ref().map(|path| path.to_string_lossy()),
            Some(command_line.as_str()),
            None,
        ),
        BootFiles::Firmware { firmware } => (None, None, None, Some(firmware.to_string_lossy())),
    };
    let parameters = VmParams {
        cpus: &cpus,
        memory: &memory,
        memory_zones: None,
        firmware: firmware.as_deref(),
        kernel: kernel.as_deref(),
        initramfs: initramfs.as_deref(),
        cmdline: command_line,
        rate_limit_groups: None,
        disks: listed(&disks),
        net: listed(&networks),
        rng: "src=/dev/urandom",
        balloon: None,
        fs: None,
        generic_vhost_user: None,
        pmem: None,
        serial: &serial,
        console: "off",
        debug_console: "off",
        devices: None,
        user_devices: None,
        vdpa: None,
        vsock: vsock.as_deref(),
        pvpanic: false,
        numa: None,
        watchdog: false,
        rtc: None,
        pci_segments: None,
        platform: platform.as_deref(),
        tpm: None,
        landlock_enable: false,
        landlock_rules: None,
    };
    VmConfig::parse(parameters).map_err(|error| VmProcessError::Configuration(debug_message(error)))
}

fn block_handled_signals() {
    for signal in vmm::vm::Vm::HANDLED_SIGNALS.iter().chain(vmm::Vmm::HANDLED_SIGNALS.iter()) {
        if let Err(error) = block_signal(*signal) {
            tracing::warn!(signal, error = %error, "blocking a signal failed");
        }
    }
}

fn create_and_boot(
    api_event: &EventFd,
    api_sender: mpsc::Sender<ApiRequest>,
    configuration: VmConfig,
) -> Result<(), VmProcessError> {
    let event = || api_event.try_clone().map_err(|error| VmProcessError::Vmm(error.to_string()));
    VmCreate
        .send(event()?, api_sender.clone(), Box::new(configuration))
        .map_err(|error| VmProcessError::Boot(debug_message(error)))?;
    VmBoot.send(event()?, api_sender, ()).map_err(|error| VmProcessError::Boot(debug_message(error)))?;
    Ok(())
}

fn boot(
    directory: &VmDirectory,
    plan: &VmProcessPlan,
    tap_descriptors: Vec<OwnedFd>,
) -> Result<(), VmProcessError> {
    let tap_descriptors: Vec<RawFd> = tap_descriptors.into_iter().map(IntoRawFd::into_raw_fd).collect();
    let configuration = configuration(plan, &tap_descriptors)?;
    block_handled_signals();
    let hypervisor =
        cloud_hypervisor_backend::new().map_err(|error| VmProcessError::Hypervisor(debug_message(error)))?;
    let event = || EventFd::new(EFD_NONBLOCK).map_err(|error| VmProcessError::Vmm(error.to_string()));
    let api_event = event()?;
    let exit_event = event()?;
    let (api_sender, api_receiver) = mpsc::channel();
    let clone = |event: &EventFd| event.try_clone().map_err(|error| VmProcessError::Vmm(error.to_string()));
    let vmm_thread = vmm::start_vmm_thread(
        vmm::VmmVersionInfo::new(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_VERSION")),
        &None,
        None,
        clone(&api_event)?,
        api_sender.clone(),
        api_receiver,
        clone(&exit_event)?,
        &SeccompAction::KillProcess,
        hypervisor,
        false,
        false,
    )
    .map_err(|error| VmProcessError::Vmm(debug_message(error)))?;

    let booted = create_and_boot(&api_event, api_sender, configuration);
    match booted {
        Ok(()) => {
            std::fs::write(directory.booted_file(), b"")
                .map_err(|error| VmProcessError::Boot(error.to_string()))?;
            tracing::info!(vm_id = %plan.launch.vm_id, "the VM booted");
        }
        Err(error) => {
            let _ = exit_event.write(1);
            let _ = vmm_thread.thread_handle.join();
            return Err(error);
        }
    }
    match vmm_thread.thread_handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(VmProcessError::Exit(debug_message(error))),
        Err(_) => Err(VmProcessError::Exit(String::from("the VMM thread panicked"))),
    }
}

fn probe(attempt: &Attempt) -> ! {
    match probe::attempt(attempt) {
        Ok(()) => std::process::exit(PERMITTED_EXIT_CODE),
        Err(reason) => {
            println!("{reason}");
            std::process::exit(DENIED_EXIT_CODE)
        }
    }
}

pub fn run_vm_process(path: &Path) -> Result<(), VmProcessError> {
    let directory = VmDirectory::at(path);
    let (instructions, descriptors) =
        control::receive(std::io::stdin()).map_err(|error| VmProcessError::Plan(error.to_string()))?;
    sandbox::enter(&instructions.restrictions).map_err(VmProcessError::Sandbox)?;
    match instructions.work {
        Work::Probe { attempt } => probe(&attempt),
        Work::Boot { plan } => boot(&directory, &plan, descriptors),
    }
}
