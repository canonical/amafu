use std::path::Path;
use std::sync::mpsc;

use seccompiler::SeccompAction;
use vmm::api::{ApiAction, ApiRequest, VmBoot, VmCreate};
use vmm::config::VmParams;
use vmm::vm_config::VmConfig;
use vmm_sys_util::eventfd::{EFD_NONBLOCK, EventFd};
use vmm_sys_util::signal::block_signal;

use crate::plan::VmProcessPlan;
use crate::runtime::VmDirectory;

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
}

fn debug_message(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

fn read_plan(directory: &VmDirectory) -> Result<VmProcessPlan, VmProcessError> {
    let bytes =
        std::fs::read(directory.plan_file()).map_err(|error| VmProcessError::Plan(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| VmProcessError::Plan(error.to_string()))
}

fn configuration(plan: &VmProcessPlan) -> Result<VmConfig, VmProcessError> {
    let cpus = plan.cpus_option();
    let memory = plan.memory_option();
    let serial = plan.serial_option();
    let kernel = plan.image.kernel.to_string_lossy();
    let initramfs = plan.image.initramfs.as_ref().map(|path| path.to_string_lossy());
    let parameters = VmParams {
        cpus: &cpus,
        memory: &memory,
        memory_zones: None,
        firmware: None,
        kernel: Some(&kernel),
        initramfs: initramfs.as_deref(),
        cmdline: Some(&plan.image.command_line),
        rate_limit_groups: None,
        disks: None,
        net: None,
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
        vsock: None,
        pvpanic: false,
        numa: None,
        watchdog: false,
        rtc: None,
        pci_segments: None,
        platform: None,
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

pub fn run_vm_process(path: &Path) -> Result<(), VmProcessError> {
    let directory = VmDirectory::at(path);
    let plan = read_plan(&directory)?;
    let configuration = configuration(&plan)?;
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
        &SeccompAction::Allow,
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
