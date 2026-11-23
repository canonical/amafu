use amafu_api::ApiClient;
use amafu_domain::kinds::juju_controller::{
    Deployment, JujuController, JujuControllerChange, JujuControllerChangeSpecification,
    JujuControllerSpecification, JujuRequest, JujuVersion, SignInMode, Substrate,
};
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::{Object, ObjectName};
use anyhow::bail;
use clap::{Args, Subcommand, ValueEnum};

use crate::kubernetes::shape;
use crate::output::{OutputFormat, emit_line};
use crate::resources::{CommonCommand, Tabulated, kebab_name, or_dash, print_objects, run_common};
use crate::step_plans::{Waiting, plan_columns, submit, unique_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SubstrateKind {
    Pods,
    Kubernetes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SignIn {
    Amafu,
    Jaas,
}

#[derive(Args)]
pub struct ControllerArguments {
    #[arg(long, value_parser = version, help = "A released Juju version, such as 3.6.29")]
    version: JujuVersion,
    #[arg(long = "on", value_enum, default_value = "pods")]
    substrate: SubstrateKind,
    #[arg(long, default_value_t = 1, help = "LXD pods to run the controller and units on: 1, or 3 to 16")]
    pods: u8,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3), help = "1, or 3 for high availability")]
    controllers: u8,
    #[arg(long, value_parser = shape, default_value = "medium")]
    shape: MachineShape,
    #[arg(long, value_name = "NETWORK")]
    network: Option<ObjectName>,
    #[arg(long = "storage-backend", value_name = "BACKEND")]
    storage_backend: Option<ObjectName>,
    #[arg(long = "external-network", value_name = "NETWORK")]
    external_network: Option<ObjectName>,
    #[arg(long, value_name = "CLUSTER")]
    cluster: Option<ObjectName>,
    #[arg(long = "first-model", default_value = "default")]
    first_model: ObjectName,
    #[arg(long = "sign-in", value_enum, default_value = "amafu")]
    sign_in: SignIn,
}

impl ControllerArguments {
    fn specification(self) -> anyhow::Result<JujuControllerSpecification> {
        let substrate = match (
            self.substrate,
            self.network,
            self.storage_backend,
            self.external_network,
            self.cluster,
        ) {
            (SubstrateKind::Pods, Some(network), Some(storage_backend), Some(external_network), _) => {
                Substrate::Pods {
                    pods: self.pods,
                    controllers: self.controllers,
                    shape: self.shape,
                    network,
                    storage_backend,
                    external_network,
                }
            }
            (SubstrateKind::Kubernetes, _, _, _, Some(cluster)) => Substrate::Kubernetes { cluster },
            _ => bail!(
                "a controller on pods needs --network, --storage-backend and --external-network; one on Kubernetes needs --cluster"
            ),
        };
        let sign_in = match self.sign_in {
            SignIn::Amafu => SignInMode::Amafu,
            SignIn::Jaas => SignInMode::Jaas,
        };
        Ok(JujuControllerSpecification {
            version: self.version,
            substrate,
            first_model: self.first_model,
            sign_in,
        })
    }
}

#[derive(Args)]
pub struct Application {
    #[arg(long, value_name = "MODEL")]
    model: ObjectName,
    #[arg(long, value_name = "APPLICATION")]
    application: ObjectName,
}

#[derive(Subcommand)]
pub enum JujuCommand {
    #[command(about = "Bootstrap a Juju controller on LXD pods or on an Amafu Kubernetes cluster")]
    Create {
        name: ObjectName,
        #[command(flatten)]
        controller: ControllerArguments,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Upgrade the controller, then its models")]
    Upgrade {
        name: String,
        #[arg(long, value_parser = version)]
        version: JujuVersion,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "add-model", about = "Add a model")]
    AddModel {
        name: String,
        model: ObjectName,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "destroy-model", about = "Destroy a model and its storage")]
    DestroyModel {
        name: String,
        model: ObjectName,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "enable-ha", about = "Run three controllers spread over the pods")]
    EnableHa {
        name: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "scale-pods", about = "Add pods to the LXD cluster, or retire empty ones")]
    ScalePods {
        name: String,
        #[arg(long, help = "1, or 3 to 16")]
        pods: u8,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Deploy a charm; Juju runs each unit in an LXD container on the pods")]
    Deploy {
        name: String,
        #[command(flatten)]
        target: Application,
        #[arg(long)]
        charm: String,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long, default_value_t = 1)]
        units: u16,
        #[arg(long, help = "Expose the application through Juju")]
        expose: bool,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Add or remove units until the application has this many")]
    Scale {
        name: String,
        #[command(flatten)]
        target: Application,
        #[arg(long)]
        units: u16,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "remove-units", about = "Remove these units")]
    RemoveUnits {
        name: String,
        #[command(flatten)]
        target: Application,
        #[arg(required = true, value_name = "UNIT")]
        units: Vec<String>,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "remove-application", about = "Remove an application and its units")]
    RemoveApplication {
        name: String,
        #[command(flatten)]
        target: Application,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "rotate-credential", about = "Give a controller on Kubernetes a new cluster credential")]
    RotateCredential {
        name: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Destroy every model, then delete the controller and everything it owns")]
    Delete {
        name: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "List controllers")]
    List,
    #[command(about = "Show a controller")]
    Show { name: String },
    #[command(about = "List the changes made to controllers, or to one controller")]
    Changes { name: Option<String> },
    #[command(about = "Stop a change, undoing its steps where they can be undone")]
    Cancel { change: String },
    #[command(about = "Print a short-lived token for the controller's API")]
    Token { name: String },
}

fn version(text: &str) -> Result<JujuVersion, String> {
    JujuVersion::try_from(text.to_owned())
}

impl Tabulated for JujuController {
    const HEADINGS: &'static [&'static str] = &["NAME", "VERSION", "ON", "PHASE", "MODELS", "ADDRESS"];

    fn row(controller: &Object<Self>) -> Vec<String> {
        let substrate = match &controller.specification.substrate {
            Substrate::Pods { pods, .. } => format!("{pods} pod(s)"),
            Substrate::Kubernetes { cluster } => format!("cluster {cluster}"),
        };
        vec![
            controller.metadata.name.to_string(),
            controller.specification.version.to_string(),
            substrate,
            kebab_name(controller.status.phase),
            controller.status.models.len().to_string(),
            or_dash(controller.status.addresses.first().cloned()),
        ]
    }
}

impl Tabulated for JujuControllerChange {
    const HEADINGS: &'static [&'static str] = &["NAME", "CONTROLLER", "REQUEST", "PROGRESS", "STEP", "STATE"];

    fn row(change: &Object<Self>) -> Vec<String> {
        let [progress, step, state] = plan_columns(change);
        vec![
            change.metadata.name.to_string(),
            change.specification.controller.to_string(),
            change.specification.request.name().to_owned(),
            progress,
            step,
            state,
        ]
    }
}

async fn request(
    client: &ApiClient,
    controller: &str,
    request: JujuRequest,
    waiting: Waiting,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let name = unique_name(request.name(), controller)?;
    let specification = JujuControllerChangeSpecification::new(ObjectName::new(controller)?, request);
    submit::<JujuControllerChange>(client, &name, &specification, waiting.wait, format).await
}

fn translate(command: JujuCommand) -> anyhow::Result<(String, JujuRequest, Waiting)> {
    Ok(match command {
        JujuCommand::Create { name, controller, waiting } => {
            (name.to_string(), JujuRequest::Create { controller: controller.specification()? }, waiting)
        }
        JujuCommand::Upgrade { name, version, waiting } => (name, JujuRequest::Upgrade { version }, waiting),
        JujuCommand::AddModel { name, model, waiting } => (name, JujuRequest::AddModel { model }, waiting),
        JujuCommand::DestroyModel { name, model, waiting } => {
            (name, JujuRequest::DestroyModel { model }, waiting)
        }
        JujuCommand::EnableHa { name, waiting } => (name, JujuRequest::EnableHa, waiting),
        JujuCommand::ScalePods { name, pods, waiting } => (name, JujuRequest::ScalePods { pods }, waiting),
        JujuCommand::Deploy { name, target, charm, channel, units, expose, waiting } => {
            let deployment = Deployment {
                model: target.model,
                application: target.application,
                charm,
                channel,
                units,
                expose,
            };
            (name, JujuRequest::Deploy { deployment }, waiting)
        }
        JujuCommand::Scale { name, target, units, waiting } => (
            name,
            JujuRequest::ScaleApplication { model: target.model, application: target.application, units },
            waiting,
        ),
        JujuCommand::RemoveUnits { name, target, units, waiting } => (
            name,
            JujuRequest::RemoveUnits { model: target.model, application: target.application, units },
            waiting,
        ),
        JujuCommand::RemoveApplication { name, target, waiting } => (
            name,
            JujuRequest::RemoveApplication { model: target.model, application: target.application },
            waiting,
        ),
        JujuCommand::RotateCredential { name, waiting } => (name, JujuRequest::RotateCredential, waiting),
        JujuCommand::Delete { name, waiting } => (name, JujuRequest::Delete, waiting),
        JujuCommand::List
        | JujuCommand::Show { .. }
        | JujuCommand::Changes { .. }
        | JujuCommand::Cancel { .. }
        | JujuCommand::Token { .. } => {
            bail!("reading a controller requests no change")
        }
    })
}

pub async fn run(client: &ApiClient, command: JujuCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        JujuCommand::List => run_common::<JujuController>(client, CommonCommand::List, format).await,
        JujuCommand::Show { name } => {
            run_common::<JujuController>(client, CommonCommand::Show { name }, format).await
        }
        JujuCommand::Changes { name } => {
            let changes: Vec<_> = client
                .list::<JujuControllerChange>()
                .await?
                .into_iter()
                .filter(|change| {
                    name.as_deref().is_none_or(|name| change.specification.controller.as_str() == name)
                })
                .collect();
            print_objects(&changes, format)
        }
        JujuCommand::Cancel { change } => {
            crate::step_plans::cancel::<JujuControllerChange>(client, &change, format).await
        }
        JujuCommand::Token { name } => {
            emit_line(client.platform_token::<JujuController>(&name).await?.token)?;
            Ok(())
        }
        change => {
            let (controller, juju_request, waiting) = translate(change)?;
            request(client, &controller, juju_request, waiting, format).await
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Probe {
        #[command(subcommand)]
        command: JujuCommand,
    }

    fn parsed(arguments: &[&str]) -> JujuCommand {
        Probe::try_parse_from(std::iter::once("juju").chain(arguments.iter().copied())).unwrap().command
    }

    #[test]
    fn a_controller_on_pods_needs_its_network_and_one_on_kubernetes_its_cluster() {
        let on_pods = parsed(&[
            "create",
            "observability",
            "--version",
            "4.2.1",
            "--pods",
            "3",
            "--controllers",
            "3",
            "--network",
            "platform",
            "--storage-backend",
            "shared",
            "--external-network",
            "public",
        ]);
        let (name, JujuRequest::Create { controller }, _) = translate(on_pods).unwrap() else { panic!() };
        assert_eq!(name, "observability");
        assert_eq!(controller.substrate.controller_count(), 3);
        assert!(matches!(controller.substrate, Substrate::Pods { pods: 3, .. }));
        let on_cluster =
            parsed(&["create", "apps", "--version", "4.2.1", "--on", "kubernetes", "--cluster", "platform"]);
        let (_, JujuRequest::Create { controller }, _) = translate(on_cluster).unwrap() else { panic!() };
        assert_eq!(controller.substrate.hosting_cluster().map(ObjectName::as_str), Some("platform"));
        assert!(translate(parsed(&["create", "x", "--version", "4.2.1"])).is_err());
        assert!(translate(parsed(&["create", "x", "--version", "4.2.1", "--on", "kubernetes"])).is_err());
    }
}
