use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::mpsc;

use seccompiler::SeccompAction;
use vmm::api::{ApiAction, ApiRequest, VmBoot, VmCreate};
use vmm::vm_config::VmConfig;
use vmm::{GuestResetAction, GuestShutdownAction};
use vmm_sys_util::eventfd::{EFD_NONBLOCK, EventFd};
use vmm_sys_util::signal::block_signal;

use crate::control::{self, Work};
use crate::plan::VmProcessPlan;
use crate::probe::{self, Attempt, DENIED_EXIT_CODE, PERMITTED_EXIT_CODE};
use crate::receiving;
use crate::runtime::VmDirectory;
use crate::sandbox::{self, SandboxError};
use crate::vm_configuration::vm_configuration;

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

fn receive_migration(
    directory: &VmDirectory,
    plan: &VmProcessPlan,
    taps: &[OwnedFd],
) -> Result<(), VmProcessError> {
    let receiver_url = format!("unix:{}", directory.incoming_socket().display());
    let interfaces: Vec<String> = plan.network_devices().into_iter().map(|device| device.id).collect();
    receiving::receive_migration(&directory.api_socket(), &receiver_url, &interfaces, taps)
        .map_err(|error| VmProcessError::Boot(error.to_string()))
}

fn start_event_monitor(directory: &VmDirectory, exit_event: &EventFd) -> Result<(), VmProcessError> {
    let file = std::fs::File::create(directory.events_file())
        .map_err(|error| VmProcessError::Vmm(error.to_string()))?;
    let monitor =
        event_monitor::set_monitor(Some(file)).map_err(|error| VmProcessError::Vmm(error.to_string()))?;
    let exit = exit_event.try_clone().map_err(|error| VmProcessError::Vmm(error.to_string()))?;
    vmm::start_event_monitor_thread(monitor, &SeccompAction::KillProcess, false, exit)
        .map_err(|error| VmProcessError::Vmm(debug_message(error)))?;
    Ok(())
}

enum Start {
    Boot(Box<VmConfig>),
    Receive(Vec<OwnedFd>),
}

fn run(directory: &VmDirectory, plan: &VmProcessPlan, start: Start) -> Result<(), VmProcessError> {
    block_handled_signals();
    let hypervisor =
        cloud_hypervisor_backend::new().map_err(|error| VmProcessError::Hypervisor(debug_message(error)))?;
    let event = || EventFd::new(EFD_NONBLOCK).map_err(|error| VmProcessError::Vmm(error.to_string()));
    let api_event = event()?;
    let exit_event = event()?;
    start_event_monitor(directory, &exit_event)?;
    let (api_sender, api_receiver) = mpsc::channel();
    let clone = |event: &EventFd| event.try_clone().map_err(|error| VmProcessError::Vmm(error.to_string()));
    let api_socket = Some(directory.api_socket().to_string_lossy().into_owned());
    let vmm_thread = vmm::start_vmm_thread(
        vmm::VmmVersionInfo::new(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_VERSION")),
        &api_socket,
        None,
        clone(&api_event)?,
        api_sender.clone(),
        api_receiver,
        clone(&exit_event)?,
        &SeccompAction::KillProcess,
        hypervisor,
        GuestShutdownAction::ExitVmm,
        GuestResetAction::ResetVm,
        false,
    )
    .map_err(|error| VmProcessError::Vmm(debug_message(error)))?;

    let booted = match start {
        Start::Boot(configuration) => create_and_boot(&api_event, api_sender, *configuration),
        Start::Receive(taps) => receive_migration(directory, plan, &taps),
    };
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

fn boot(directory: &VmDirectory, plan: &VmProcessPlan, taps: Vec<OwnedFd>) -> Result<(), VmProcessError> {
    let configuration =
        vm_configuration(plan, taps).map_err(|error| VmProcessError::Configuration(error.to_string()))?;
    run(directory, plan, Start::Boot(Box::new(configuration)))
}

fn receive(directory: &VmDirectory, plan: &VmProcessPlan, taps: Vec<OwnedFd>) -> Result<(), VmProcessError> {
    std::fs::write(directory.receiving_file(), b"")
        .map_err(|error| VmProcessError::Boot(error.to_string()))?;
    tracing::info!(vm_id = %plan.launch.vm_id, "waiting for the VM to arrive");
    run(directory, plan, Start::Receive(taps))
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
        Work::Receive { plan } => receive(&directory, &plan, descriptors),
    }
}
