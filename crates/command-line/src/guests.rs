use amafu_api::ApiClient;
use amafu_domain::kinds::guest_inventory::{GuestInstance, GuestInstanceKind, GuestInventory, LxdAccess};
use clap::Subcommand;

use crate::output::{OutputFormat, Table, emit, emit_line, print_structured};

const TRUST_COMMAND: &str = "amafu lxd certificate > amafu.crt && lxc config trust add-certificate amafu.crt";

#[derive(Subcommand)]
pub enum LxdCommand {
    #[command(about = "Print the certificate an LXD server trusts to let Amafu list its instances")]
    Certificate,
}

pub async fn run(client: &ApiClient, command: LxdCommand) -> anyhow::Result<()> {
    match command {
        LxdCommand::Certificate => emit(&client.lxd_certificate().await?.certificate_pem)?,
    }
    Ok(())
}

fn kind_label(kind: GuestInstanceKind) -> &'static str {
    match kind {
        GuestInstanceKind::Container => "lxc",
        GuestInstanceKind::VirtualMachine => "vm",
    }
}

fn guest_table(instances: &[GuestInstance]) -> String {
    let mut table = Table::new(vec!["NAME", "TYPE", "STATUS", "MEMBER"]);
    for instance in instances {
        table.add(vec![
            instance.name.clone(),
            String::from(kind_label(instance.kind)),
            instance.status.clone(),
            instance.location.clone().unwrap_or_else(|| String::from("-")),
        ]);
    }
    table.render()
}

pub async fn print_guests(client: &ApiClient, name: &str, format: OutputFormat) -> anyhow::Result<()> {
    let inventory = match client.get::<GuestInventory>(name).await {
        Ok(inventory) => inventory,
        Err(error) if error.is_not_found() => {
            emit_line(format!("Amafu has not found LXD on {name}"))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if format != OutputFormat::Table {
        return print_structured(&inventory, format);
    }
    match &inventory.specification.lxd {
        LxdAccess::Untrusted => {
            emit_line(format!("LXD on {name} does not trust Amafu yet; on {name}, run: {TRUST_COMMAND}"))?;
        }
        LxdAccess::Trusted { instances, .. } => emit(&guest_table(instances))?,
    }
    Ok(())
}
