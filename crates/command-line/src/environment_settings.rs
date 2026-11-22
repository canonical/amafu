use amafu_api::ApiClient;
use amafu_domain::kinds::environment_settings::{
    ENVIRONMENT_SETTINGS_NAME, EnvironmentSettings, EnvironmentSettingsSpecification,
    environment_settings_name,
};
use amafu_domain::kinds::image::ImageSource;
use amafu_domain::{Object, OciReference};
use amafu_interfaces::CatalogKind;
use clap::Subcommand;

use crate::catalog;
use crate::output::OutputFormat;
use crate::resources::{Tabulated, print_objects};

#[derive(Subcommand)]
pub enum EnvironmentSettingsCommand {
    #[command(about = "Show the images Kubernetes clusters and Juju controllers are built from")]
    Show,
    #[command(about = "Change the images Kubernetes clusters and Juju controllers are built from")]
    Set {
        #[arg(
            long,
            value_name = "CATALOG_ID",
            help = "The Ubuntu disk image from the catalog, such as ubuntu/24.04"
        )]
        ubuntu_image: Option<String>,
        #[arg(
            long,
            value_name = "REFERENCE",
            help = "The Juju runner container for controllers on Kubernetes, as REGISTRY/REPOSITORY@sha256:DIGEST"
        )]
        juju_runner_image: Option<OciReference>,
    },
}

impl Tabulated for EnvironmentSettings {
    const HEADINGS: &'static [&'static str] = &["UBUNTU IMAGE", "JUJU RUNNER IMAGE"];

    fn row(settings: &Object<Self>) -> Vec<String> {
        let ubuntu = match &settings.specification.ubuntu_image {
            Some(ImageSource::Disk { url, .. }) => url.clone(),
            Some(ImageSource::Container { reference }) => reference.to_string(),
            None => String::from("-"),
        };
        let runner = settings.specification.juju_runner_image.as_ref().map(ToString::to_string);
        vec![ubuntu, runner.unwrap_or_else(|| String::from("-"))]
    }
}

async fn current(client: &ApiClient) -> anyhow::Result<Option<Object<EnvironmentSettings>>> {
    match client.get::<EnvironmentSettings>(ENVIRONMENT_SETTINGS_NAME).await {
        Ok(found) => Ok(Some(found)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn set(
    client: &ApiClient,
    ubuntu_image: Option<String>,
    juju_runner_image: Option<OciReference>,
) -> anyhow::Result<Object<EnvironmentSettings>> {
    let ubuntu_image = match ubuntu_image {
        Some(id) => Some(catalog::find(client, &id, CatalogKind::VirtualMachine).await?.source),
        None => None,
    };
    let found = current(client).await?;
    let previous = found.as_ref().map(|settings| settings.specification.clone()).unwrap_or_default();
    let specification = EnvironmentSettingsSpecification {
        ubuntu_image: ubuntu_image.or(previous.ubuntu_image),
        juju_runner_image: juju_runner_image.or(previous.juju_runner_image),
    };
    Ok(match found {
        Some(found) => {
            client
                .update::<EnvironmentSettings>(
                    ENVIRONMENT_SETTINGS_NAME,
                    &specification,
                    Some(found.metadata.generation),
                )
                .await?
        }
        None => client.create::<EnvironmentSettings>(&environment_settings_name(), &specification).await?,
    })
}

pub async fn run(
    client: &ApiClient,
    command: EnvironmentSettingsCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let settings = match command {
        EnvironmentSettingsCommand::Show => current(client).await?,
        EnvironmentSettingsCommand::Set { ubuntu_image, juju_runner_image } => {
            Some(set(client, ubuntu_image, juju_runner_image).await?)
        }
    };
    print_objects(settings.as_slice(), format)
}
