use std::collections::BTreeMap;

use amafu_api::ApiClient;
use amafu_domain::ObjectName;
use amafu_domain::kinds::vm::{
    BootSource, CONTAINER_CPU_COUNT, CONTAINER_MEMORY_MEBIBYTES, Vm, VmSpecification,
};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::vm::{Attachments, DEFAULT_WAIT, create_and_wait, print_vms};

#[derive(Subcommand)]
pub enum ContainerCommand {
    #[command(about = "Run a container image in its own small VM")]
    Run {
        name: ObjectName,
        #[arg(long)]
        image: ObjectName,
        #[arg(long, default_value_t = CONTAINER_CPU_COUNT)]
        cpus: u16,
        #[arg(long = "memory", value_name = "MEBIBYTES", default_value_t = CONTAINER_MEMORY_MEBIBYTES)]
        memory_mebibytes: u64,
        #[command(flatten)]
        attachments: Attachments,
        #[arg(long = "env", value_name = "NAME=VALUE", value_parser = environment_variable)]
        environment: Vec<(String, String)>,
        #[arg(long, help = "Wait until the container runs")]
        wait: bool,
        #[arg(long, value_name = "SECONDS", default_value = DEFAULT_WAIT)]
        timeout: u64,
        #[arg(last = true, value_name = "COMMAND", help = "Replace the image command")]
        command: Vec<String>,
    },
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
        ContainerCommand::Run {
            name,
            image,
            cpus,
            memory_mebibytes,
            attachments,
            environment,
            wait,
            timeout,
            command,
        } => {
            let specification = VmSpecification {
                cpu_count: cpus,
                memory_mebibytes,
                boot: BootSource::Container {
                    image,
                    command,
                    environment: environment.into_iter().collect::<BTreeMap<_, _>>(),
                },
                network_interfaces: attachments.interfaces(),
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
