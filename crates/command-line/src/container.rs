use std::collections::BTreeMap;

use amafu_api::ApiClient;
use amafu_domain::ObjectName;
use amafu_domain::kinds::vm::{
    BootSource, CONTAINER_CPU_COUNT, CONTAINER_MEMORY_MEBIBYTES, Vm, VmSpecification,
};
use amafu_interfaces::CatalogKind;
use clap::{Args, Subcommand};

use crate::output::OutputFormat;
use crate::ssh_key::KeyGrants;
use crate::vm::{Attachments, DEFAULT_WAIT, create_and_wait, print_vms};

#[derive(Args)]
pub struct RunArguments {
    pub name: ObjectName,
    #[arg(long, help = "A container image of the project or a catalog ID such as ubuntu/nginx")]
    pub image: String,
    #[arg(long, default_value_t = CONTAINER_CPU_COUNT)]
    pub cpus: u16,
    #[arg(long = "memory", value_name = "MEBIBYTES", default_value_t = CONTAINER_MEMORY_MEBIBYTES)]
    pub memory_mebibytes: u64,
    #[command(flatten)]
    pub attachments: Attachments,
    #[command(flatten)]
    pub ssh: KeyGrants,
    #[arg(long = "env", value_name = "NAME=VALUE", value_parser = environment_variable)]
    pub environment: Vec<(String, String)>,
    #[arg(long, help = "Wait until the container runs")]
    pub wait: bool,
    #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
    pub timeout: u64,
    #[arg(last = true, value_name = "COMMAND", help = "Replace the image command")]
    pub command: Vec<String>,
}

#[derive(Subcommand)]
pub enum ContainerCommand {
    #[command(about = "Run a container image in its own small VM")]
    Run(Box<RunArguments>),
    #[command(about = "List containers; show, stop and delete them with the vm commands")]
    List,
}

fn environment_variable(text: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .ok_or_else(|| format!("{text:?} is not NAME=VALUE"))
}

pub async fn run(client: &ApiClient, command: ContainerCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        ContainerCommand::Run(arguments) => {
            let RunArguments {
                name,
                image,
                cpus,
                memory_mebibytes,
                attachments,
                ssh,
                environment,
                wait,
                timeout,
                command,
            } = *arguments;
            let image = crate::catalog::resolve(client, &image, CatalogKind::Container).await?;
            let specification = VmSpecification {
                cpu_count: cpus,
                memory_mebibytes,
                boot: BootSource::Container {
                    image,
                    command,
                    environment: environment.into_iter().collect::<BTreeMap<_, _>>(),
                },
                network_interfaces: attachments.interfaces(),
                ssh_keys: ssh.references(),
                ..VmSpecification::small()
            };
            create_and_wait(client, &name, &specification, wait, timeout, format).await
        }
        ContainerCommand::List => {
            let containers: Vec<_> =
                client.list::<Vm>().await?.into_iter().filter(|vm| vm.specification.is_container()).collect();
            print_vms(&containers, format)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_variables_split_at_the_first_equals_sign() {
        assert_eq!(environment_variable("A=b=c").unwrap(), (String::from("A"), String::from("b=c")));
        assert!(environment_variable("A").is_err());
    }
}
