use amafu_api::ApiClient;
use amafu_domain::kinds::node::{Node, NodeSpecification};
use amafu_domain::{NodeName, Object};
use clap::Subcommand;

use crate::output::{OutputFormat, Table, print_structured};

#[derive(Subcommand)]
pub enum NodeCommand {
    #[command(about = "List nodes")]
    List,
    #[command(about = "Show a node")]
    Show { name: String },
    #[command(about = "Stop placing new workloads on a node")]
    Cordon { name: String },
    #[command(about = "Allow new workloads on a node again")]
    Uncordon { name: String },
    #[command(about = "Cordon a node and move every VM off it")]
    Evacuate {
        name: NodeName,
        #[arg(long, help = "Wait until every VM has moved")]
        wait: bool,
        #[arg(long, value_name = "SECONDS", default_value = "1800")]
        timeout: u64,
    },
}

fn print_nodes(nodes: &[Object<Node>], format: OutputFormat) -> anyhow::Result<()> {
    if format != OutputFormat::Table {
        return print_structured(&nodes, format);
    }
    let mut table = Table::new(vec![
        "NAME", "PHASE", "ROLES", "ZONE", "UNDERLAY", "CORDONED", "CPUS", "MEMORY", "VERSION",
    ]);
    for node in nodes {
        let roles: Vec<String> = node
            .specification
            .roles
            .iter()
            .filter_map(|role| serde_json::to_value(role).ok()?.as_str().map(str::to_owned))
            .collect();
        let phase =
            serde_json::to_value(node.status.phase).ok().and_then(|value| value.as_str().map(str::to_owned));
        table.add(vec![
            node.metadata.name.to_string(),
            phase.unwrap_or_default(),
            roles.join(","),
            node.specification.zone.as_ref().map_or_else(|| String::from("-"), ToString::to_string),
            node.status.underlay_address.map_or_else(|| String::from("-"), |address| address.to_string()),
            if node.specification.is_cordoned { String::from("yes") } else { String::from("no") },
            node.status.capacity.cpu_count.to_string(),
            format!("{} MiB", node.status.capacity.memory_mebibytes),
            node.status.software_version.clone(),
        ]);
    }
    print!("{}", table.render());
    Ok(())
}

async fn set_cordoned(client: &ApiClient, name: &str, is_cordoned: bool) -> anyhow::Result<Object<Node>> {
    let node = client.get::<Node>(name).await?;
    let specification = NodeSpecification { is_cordoned, ..node.specification };
    Ok(client.update::<Node>(name, &specification, Some(node.metadata.generation)).await?)
}

pub async fn run(client: &ApiClient, command: NodeCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        NodeCommand::List => print_nodes(&client.list::<Node>().await?, format),
        NodeCommand::Show { name } => print_structured(&client.get::<Node>(&name).await?, format),
        NodeCommand::Cordon { name } => print_nodes(&[set_cordoned(client, &name, true).await?], format),
        NodeCommand::Uncordon { name } => print_nodes(&[set_cordoned(client, &name, false).await?], format),
        NodeCommand::Evacuate { name, wait, timeout } => {
            let wait = wait.then(|| std::time::Duration::from_secs(timeout));
            crate::operation::evacuate(client, &name, wait, format).await
        }
    }
}
