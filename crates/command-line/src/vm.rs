use std::time::Duration;

use amafu_api::ApiClient;
use amafu_api::client::{ClientError, WatchUpdate};
use amafu_domain::kinds::vm::{
    BootSource, NetworkInterfaceSpecification, PowerState, Vm, VmPhase, VmSpecification,
};
use amafu_domain::{NodeName, Object, ObjectName};
use anyhow::bail;
use clap::Subcommand;
use futures::StreamExt;

use crate::catalog::{DEFAULT_DISK_GIBIBYTES, RootDisk, create_root_volume};
use crate::output::{OutputFormat, Table, print_structured};
use crate::ssh_key::KeyGrants;

pub const DEFAULT_WAIT: &str = "120";

#[derive(clap::Args)]
pub struct Attachments {
    #[arg(
        long = "network",
        value_name = "NETWORK",
        help = "Attach an interface on this network; repeatable"
    )]
    pub networks: Vec<ObjectName>,
    #[arg(long = "security-group", value_name = "GROUP", help = "Apply to every interface; repeatable")]
    pub security_groups: Vec<ObjectName>,
}

impl Attachments {
    #[must_use]
    pub fn interfaces(&self) -> Vec<NetworkInterfaceSpecification> {
        self.networks
            .iter()
            .map(|network| NetworkInterfaceSpecification {
                guests: 0,
                network: network.clone(),
                security_groups: self.security_groups.clone(),
                address: None,
            })
            .collect()
    }
}

#[derive(clap::Args)]
pub struct Placement {
    #[arg(
        long,
        default_value_t = 0,
        allow_negative_numbers = true,
        help = "Higher priorities are placed first"
    )]
    pub priority: i32,
    #[arg(long, value_name = "GROUP", help = "Never place two VMs of this group on the same node")]
    pub anti_affinity_group: Option<ObjectName>,
}

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
pub struct BootChoice {
    #[arg(
        long,
        value_name = "IMAGE",
        help = "Boot a new disk made from this image: one of the project's or a catalog ID such as ubuntu/24.04"
    )]
    image: Option<String>,
    #[arg(long, value_name = "BUNDLE", help = "Boot a kernel bundle installed on the nodes")]
    kernel: Option<ObjectName>,
    #[arg(long, help = "Boot from the first --volume through UEFI firmware")]
    from_disk: bool,
}

impl BootChoice {
    fn source(&self) -> BootSource {
        match &self.kernel {
            Some(bundle) => BootSource::Kernel { bundle: bundle.clone() },
            None => BootSource::Disk,
        }
    }
}

#[derive(clap::Args)]
pub struct DiskChoice {
    #[arg(long = "disk-size", value_name = "GIB", default_value_t = DEFAULT_DISK_GIBIBYTES, help = "Size of the disk made from --image")]
    size_gibibytes: u32,
    #[arg(
        long = "storage-backend",
        value_name = "BACKEND",
        help = "Where the disk made from --image is kept"
    )]
    backend: Option<ObjectName>,
    #[arg(long = "keep-disk", help = "Keep the disk made from --image when the VM is deleted")]
    is_kept: bool,
}

#[derive(clap::Args)]
pub struct CreateArguments {
    pub name: ObjectName,
    #[arg(long, default_value_t = 1)]
    pub cpus: u16,
    #[arg(long = "memory", value_name = "MEBIBYTES", default_value_t = 512)]
    pub memory_mebibytes: u64,
    #[command(flatten)]
    pub boot: BootChoice,
    #[command(flatten)]
    pub disk: DiskChoice,
    #[arg(
        long = "volume",
        value_name = "VOLUME",
        help = "Attach a volume; repeatable, the first is the boot disk"
    )]
    pub volumes: Vec<ObjectName>,
    #[command(flatten)]
    pub attachments: Attachments,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 0,
        help = "Let up to this many LXD guests take addresses on the first interface, at most 64"
    )]
    pub guests: u16,
    #[command(flatten)]
    pub placement: Placement,
    #[command(flatten)]
    pub ssh: KeyGrants,
    #[arg(long, value_name = "FILE", help = "Serve this file as cloud-init user data")]
    pub user_data: Option<std::path::PathBuf>,
    #[arg(long, help = "Create the VM without starting it")]
    pub stopped: bool,
    #[arg(long, help = "Wait until the VM runs")]
    pub wait: bool,
    #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
    pub timeout: u64,
}

#[derive(Subcommand)]
pub enum VmCommand {
    #[command(about = "Create a virtual machine")]
    Create(Box<CreateArguments>),
    #[command(about = "List virtual machines")]
    List,
    #[command(about = "Show a virtual machine")]
    Show { name: String },
    #[command(about = "List the LXD containers and VMs a virtual machine hosts")]
    Guests { name: String },
    #[command(about = "Start a stopped virtual machine")]
    Start { name: String },
    #[command(about = "Stop a running virtual machine")]
    Stop { name: String },
    #[command(about = "Move a running virtual machine to another node")]
    Migrate {
        name: String,
        #[arg(long, value_name = "NODE", help = "The node to move to; the scheduler chooses when omitted")]
        to: Option<NodeName>,
        #[arg(long, help = "Wait until the move finishes")]
        wait: bool,
        #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
        timeout: u64,
    },
    #[command(about = "Delete a virtual machine")]
    Delete {
        name: String,
        #[arg(long, help = "Wait until the VM is removed")]
        wait: bool,
        #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
        timeout: u64,
    },
}

fn phase_name(phase: VmPhase) -> String {
    serde_json::to_value(phase).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()
}

#[must_use]
pub fn boot_description(boot: &BootSource) -> String {
    match boot {
        BootSource::Kernel { bundle } => format!("kernel {bundle}"),
        BootSource::Disk => String::from("disk"),
        BootSource::Container { image, .. } => format!("container {image}"),
    }
}

#[must_use]
pub fn addresses(vm: &Object<Vm>) -> String {
    vm.status.interfaces.iter().map(|interface| interface.address.to_string()).collect::<Vec<_>>().join(",")
}

pub fn print_vms(vms: &[Object<Vm>], format: OutputFormat) -> anyhow::Result<()> {
    if format != OutputFormat::Table {
        return print_structured(&vms, format);
    }
    let mut table =
        Table::new(vec!["NAME", "PROJECT", "PHASE", "NODE", "CPUS", "MEMORY", "BOOT", "ADDRESSES"]);
    for vm in vms {
        table.add(vec![
            vm.metadata.name.to_string(),
            vm.metadata.project.as_ref().map(ToString::to_string).unwrap_or_default(),
            phase_name(vm.status.phase),
            vm.status.node.as_ref().map(ToString::to_string).unwrap_or_default(),
            vm.specification.cpu_count.to_string(),
            format!("{} MiB", vm.specification.memory_mebibytes),
            boot_description(&vm.specification.boot),
            addresses(vm),
        ]);
    }
    crate::output::emit(&table.render())?;
    Ok(())
}

pub async fn wait_for(
    client: &ApiClient,
    name: &str,
    timeout: u64,
    is_done: impl Fn(&WatchUpdate<Vm>) -> bool,
) -> anyhow::Result<()> {
    let mut updates = client.watch::<Vm>(name).await?;
    let waiting = async {
        while let Some(update) = updates.next().await {
            let update = update?;
            if let WatchUpdate::Changed(vm) = &update
                && vm.status.phase == VmPhase::Failed
            {
                bail!("the VM failed: {}", vm.status.message.clone().unwrap_or_default());
            }
            if is_done(&update) {
                return Ok(());
            }
        }
        bail!("the watch ended before the VM got there")
    };
    tokio::time::timeout(Duration::from_secs(timeout), waiting)
        .await
        .map_err(|_| anyhow::anyhow!("timed out after {timeout} seconds"))?
}

async fn wait_for_removal(client: &ApiClient, name: &str, timeout: u64) -> anyhow::Result<()> {
    match wait_for(client, name, timeout, |update| matches!(update, WatchUpdate::Removed)).await {
        Err(error) if is_already_removed(&error) => Ok(()),
        outcome => outcome,
    }
}

fn is_already_removed(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ClientError>().is_some_and(ClientError::is_not_found)
}

async fn set_power_state(
    client: &ApiClient,
    name: &str,
    power_state: PowerState,
) -> anyhow::Result<Object<Vm>> {
    let vm = client.get::<Vm>(name).await?;
    let specification = VmSpecification { power_state, ..vm.specification };
    Ok(client.update::<Vm>(name, &specification, Some(vm.metadata.generation)).await?)
}

pub async fn create_and_wait(
    client: &ApiClient,
    name: &ObjectName,
    specification: &VmSpecification,
    wait: bool,
    timeout: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let created = client.create::<Vm>(name, specification).await?;
    if wait {
        wait_for(client, name.as_str(), timeout, |update| {
            matches!(update, WatchUpdate::Changed(vm) if vm.status.phase == VmPhase::Running && vm.is_status_current())
        })
        .await?;
        return print_vms(&[client.get::<Vm>(name.as_str()).await?], format);
    }
    print_vms(&[created], format)
}

pub async fn run(client: &ApiClient, command: VmCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        VmCommand::Create(arguments) => {
            let CreateArguments {
                name,
                cpus,
                memory_mebibytes,
                boot,
                disk,
                mut volumes,
                attachments,
                guests,
                placement,
                ssh,
                user_data,
                stopped,
                wait,
                timeout,
            } = *arguments;
            if let Some(image) = &boot.image {
                let root = RootDisk {
                    image: image.clone(),
                    size_gibibytes: disk.size_gibibytes,
                    backend: disk.backend,
                    is_kept: disk.is_kept,
                };
                volumes.insert(0, create_root_volume(client, &name, root).await?);
            }
            let power_state = if stopped { PowerState::Stopped } else { PowerState::Running };
            let user_data = user_data.map(std::fs::read_to_string).transpose()?;
            let mut network_interfaces = attachments.interfaces();
            if let Some(first) = network_interfaces.first_mut() {
                first.guests = guests;
            }
            let specification = VmSpecification {
                cpu_count: cpus,
                memory_mebibytes,
                boot: boot.source(),
                volumes,
                network_interfaces,
                power_state,
                user_data,
                priority: placement.priority,
                anti_affinity_group: placement.anti_affinity_group,
                minimum_cpu_baseline: None,
                ssh_keys: ssh.references(),
            };
            create_and_wait(client, &name, &specification, wait && !stopped, timeout, format).await
        }
        VmCommand::List => print_vms(&client.list::<Vm>().await?, format),
        VmCommand::Show { name } => print_structured(&client.get::<Vm>(&name).await?, format),
        VmCommand::Guests { name } => crate::guests::print_guests(client, &name, format).await,
        VmCommand::Start { name } => {
            print_vms(&[set_power_state(client, &name, PowerState::Running).await?], format)
        }
        VmCommand::Stop { name } => {
            print_vms(&[set_power_state(client, &name, PowerState::Stopped).await?], format)
        }
        VmCommand::Migrate { name, to, wait, timeout } => {
            let wait = wait.then(|| std::time::Duration::from_secs(timeout));
            crate::operation::migrate(client, &name, to, wait, format).await
        }
        VmCommand::Delete { name, wait, timeout } => {
            let deleting = client.delete::<Vm>(&name).await?;
            if wait {
                wait_for_removal(client, &name, timeout).await?;
                crate::output::emit_line(format!("deleted {name}"))?;
                return Ok(());
            }
            print_vms(&[deleting], format)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(status: u16) -> anyhow::Error {
        ClientError::UnexpectedStatus { status, body: String::new() }.into()
    }

    #[test]
    fn a_vm_gone_before_the_watch_starts_counts_as_deleted() {
        assert!(is_already_removed(&refused(404)));
    }

    #[test]
    fn other_failures_are_still_failures() {
        assert!(!is_already_removed(&refused(503)));
        assert!(!is_already_removed(&anyhow::anyhow!("timed out after 120 seconds")));
    }
}
