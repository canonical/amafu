use std::time::Duration;

use amafu_api::ApiClient;
use amafu_api::client::WatchUpdate;
use amafu_domain::kinds::vm::{PowerState, Vm, VmPhase, VmSpecification};
use amafu_domain::{Object, ObjectName};
use anyhow::bail;
use clap::Subcommand;
use futures::StreamExt;

use crate::output::{OutputFormat, Table, print_structured};

const DEFAULT_WAIT: &str = "120";

#[derive(Subcommand)]
pub enum VmCommand {
    #[command(about = "Create a virtual machine")]
    Create {
        name: ObjectName,
        #[arg(long, default_value_t = 1)]
        cpus: u16,
        #[arg(long = "memory", value_name = "MEBIBYTES", default_value_t = 512)]
        memory_mebibytes: u64,
        #[arg(long)]
        image: ObjectName,
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

fn print_vms(vms: &[Object<Vm>], format: OutputFormat) -> anyhow::Result<()> {
    if format != OutputFormat::Table {
        return print_structured(&vms, format);
    }
    let mut table = Table::new(vec!["NAME", "PROJECT", "PHASE", "NODE", "CPUS", "MEMORY", "IMAGE"]);
    for vm in vms {
        table.add(vec![
            vm.metadata.name.to_string(),
            vm.metadata.project.as_ref().map(ToString::to_string).unwrap_or_default(),
            phase_name(vm.status.phase),
            vm.status.node.as_ref().map(ToString::to_string).unwrap_or_default(),
            vm.specification.cpu_count.to_string(),
            format!("{} MiB", vm.specification.memory_mebibytes),
            vm.specification.image.to_string(),
        ]);
    }
    print!("{}", table.render());
    Ok(())
}

async fn wait_for(
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

pub async fn run(client: &ApiClient, command: VmCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        VmCommand::Create { name, cpus, memory_mebibytes, image, stopped, wait, timeout } => {
            let power_state = if stopped { PowerState::Stopped } else { PowerState::Running };
            let specification = VmSpecification { cpu_count: cpus, memory_mebibytes, image, power_state };
            let created = client.create::<Vm>(&name, &specification).await?;
            if wait && !stopped {
                wait_for(client, name.as_str(), timeout, |update| {
                    matches!(update, WatchUpdate::Changed(vm) if vm.status.phase == VmPhase::Running && vm.is_status_current())
                })
                .await?;
                return print_vms(&[client.get::<Vm>(name.as_str()).await?], format);
            }
            print_vms(&[created], format)
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
