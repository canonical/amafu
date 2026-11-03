use std::time::Duration;

use amafu_api::ApiClient;
use amafu_api::client::WatchUpdate;
use amafu_domain::kinds::vm::{
    BootSource, NetworkInterfaceSpecification, PowerState, Vm, VmPhase, VmSpecification,
};
use amafu_domain::{Object, ObjectName};
use anyhow::bail;
use clap::Subcommand;
use futures::StreamExt;

use crate::output::{OutputFormat, Table, print_structured};

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
    #[arg(long, value_name = "BUNDLE", help = "Boot a kernel bundle installed on the nodes")]
    kernel: Option<ObjectName>,
    #[arg(long, help = "Boot from the first volume through UEFI firmware")]
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

#[derive(Subcommand)]
pub enum VmCommand {
    #[command(about = "Create a virtual machine")]
    Create {
        name: ObjectName,
        #[arg(long, default_value_t = 1)]
        cpus: u16,
        #[arg(long = "memory", value_name = "MEBIBYTES", default_value_t = 512)]
        memory_mebibytes: u64,
        #[command(flatten)]
        boot: BootChoice,
        #[arg(
            long = "volume",
            value_name = "VOLUME",
            help = "Attach a volume; repeatable, the first is the boot disk"
        )]
        volumes: Vec<ObjectName>,
        #[command(flatten)]
        attachments: Attachments,
        #[command(flatten)]
        placement: Placement,
        #[arg(long, value_name = "FILE", help = "Serve this file as cloud-init user data")]
        user_data: Option<std::path::PathBuf>,
        #[arg(long, help = "Create the VM without starting it")]
        stopped: bool,
        #[arg(long, help = "Wait until the VM runs")]
        wait: bool,
        #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
        timeout: u64,
    },
    #[command(about = "List virtual machines")]
    List,
    #[command(about = "Show a virtual machine")]
    Show { name: String },
    #[command(about = "Start a stopped virtual machine")]
    Start { name: String },
    #[command(about = "Stop a running virtual machine")]
    Stop { name: String },
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
    print!("{}", table.render());
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
        VmCommand::Create {
            name,
            cpus,
            memory_mebibytes,
            boot,
            volumes,
            attachments,
            placement,
            user_data,
            stopped,
            wait,
            timeout,
        } => {
            let power_state = if stopped { PowerState::Stopped } else { PowerState::Running };
            let user_data = user_data.map(std::fs::read_to_string).transpose()?;
            let specification = VmSpecification {
                cpu_count: cpus,
                memory_mebibytes,
                boot: boot.source(),
                volumes,
                network_interfaces: attachments.interfaces(),
                power_state,
                user_data,
                priority: placement.priority,
                anti_affinity_group: placement.anti_affinity_group,
                minimum_cpu_baseline: None,
            };
            create_and_wait(client, &name, &specification, wait && !stopped, timeout, format).await
        }
        VmCommand::List => print_vms(&client.list::<Vm>().await?, format),
        VmCommand::Show { name } => print_structured(&client.get::<Vm>(&name).await?, format),
        VmCommand::Start { name } => {
            print_vms(&[set_power_state(client, &name, PowerState::Running).await?], format)
        }
        VmCommand::Stop { name } => {
            print_vms(&[set_power_state(client, &name, PowerState::Stopped).await?], format)
        }
        VmCommand::Delete { name, wait, timeout } => {
            let deleting = client.delete::<Vm>(&name).await?;
            if wait {
                wait_for(client, &name, timeout, |update| matches!(update, WatchUpdate::Removed)).await?;
                println!("deleted {name}");
                return Ok(());
            }
            print_vms(&[deleting], format)
        }
    }
}
