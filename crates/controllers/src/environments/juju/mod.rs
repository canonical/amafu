mod applications;
mod bootstrap;
mod controller;
mod models;
mod pods;
mod scope;

use std::sync::Arc;

use amafu_domain::kinds::juju_controller::{JujuControllerChange, JujuStep};
use amafu_interfaces::{ActionError, Identifiers, JujuPlatform, KubernetesPlatform, LxdPlatform};
use amafu_reconciler::{Step, StepRegistry};
use amafu_token_issuer::Keyring;

pub struct JujuSettings {
    pub token_refresh_url: String,
    pub amafu_certificate_authority: String,
}

pub struct JujuEnvironment {
    pub platform: Arc<dyn JujuPlatform>,
    pub kubernetes: Arc<dyn KubernetesPlatform>,
    pub lxd: Arc<dyn LxdPlatform>,
    pub keyring: Keyring,
    pub identifiers: Arc<dyn Identifiers>,
    pub settings: JujuSettings,
}

pub struct JujuSteps {
    environment: Arc<JujuEnvironment>,
}

impl JujuSteps {
    #[must_use]
    pub fn new(environment: JujuEnvironment) -> Self {
        Self { environment: Arc::new(environment) }
    }

    fn controller_step(&self, step: &JujuStep) -> Option<Box<dyn Step<JujuControllerChange>>> {
        let environment = Arc::clone(&self.environment);
        Some(match step {
            JujuStep::RecordSpecification => Box::new(controller::RecordSpecification { environment }),
            JujuStep::PrepareNetworkAccess => Box::new(controller::PrepareNetworkAccess { environment }),
            JujuStep::ImportImage => Box::new(controller::ImportImages { environment }),
            JujuStep::CreateFounderPod => Box::new(pods::CreateFounderPod { environment }),
            JujuStep::FormCluster => Box::new(pods::FormCluster { environment }),
            JujuStep::RecordPodCount { pods } => Box::new(pods::RecordPodCount { environment, pods: *pods }),
            JujuStep::RetirePods => Box::new(pods::RetirePods { environment }),
            JujuStep::CheckHostCluster => Box::new(controller::CheckHostCluster { environment }),
            JujuStep::CreateClusterServiceAccount => {
                Box::new(controller::CreateClusterServiceAccount { environment })
            }
            JujuStep::Bootstrap => Box::new(controller::Bootstrap { environment }),
            JujuStep::AwaitControllerService => Box::new(controller::AwaitControllerService { environment }),
            JujuStep::ExposeController => Box::new(controller::ExposeController { environment }),
            JujuStep::RecordController => Box::new(controller::RecordController { environment }),
            JujuStep::EnableHa => Box::new(controller::EnableHa { environment }),
            JujuStep::CleanUpBootstrap => Box::new(controller::CleanUpBootstrap { environment }),
            JujuStep::MarkReady => Box::new(controller::MarkReady { environment }),
            _ => return None,
        })
    }

    fn model_step(&self, step: &JujuStep) -> Option<Box<dyn Step<JujuControllerChange>>> {
        let environment = Arc::clone(&self.environment);
        Some(match step {
            JujuStep::AddModel { model } => Box::new(models::AddModel { environment, model: model.clone() }),
            JujuStep::DestroyModel { model } => {
                Box::new(models::DestroyModel { environment, model: model.clone() })
            }
            JujuStep::DestroyModels => Box::new(models::DestroyModels { environment }),
            JujuStep::DeleteResources => Box::new(models::DeleteResources { environment }),
            JujuStep::UpgradeController { version } => {
                Box::new(models::UpgradeController { environment, version: version.clone() })
            }
            JujuStep::UpgradeModels { version } => {
                Box::new(models::UpgradeModels { environment, version: version.clone() })
            }
            JujuStep::RotateCredential => Box::new(models::RotateCredential { environment }),
            _ => return None,
        })
    }

    fn application_step(&self, step: &JujuStep) -> Option<Box<dyn Step<JujuControllerChange>>> {
        let environment = Arc::clone(&self.environment);
        Some(match step {
            JujuStep::Deploy { deployment } => {
                Box::new(applications::Deploy { environment, deployment: deployment.clone() })
            }
            JujuStep::Expose { model, application } => Box::new(applications::Expose {
                environment,
                model: model.clone(),
                application: application.clone(),
            }),
            JujuStep::ScaleApplication { model, application, units } => {
                Box::new(applications::ScaleApplication {
                    environment,
                    model: model.clone(),
                    application: application.clone(),
                    units: *units,
                })
            }
            JujuStep::RemoveUnits { model, application, units } => Box::new(applications::RemoveUnits {
                environment,
                model: model.clone(),
                application: application.clone(),
                units: units.clone(),
            }),
            JujuStep::RemoveApplication { model, application } => Box::new(applications::RemoveApplication {
                environment,
                model: model.clone(),
                application: application.clone(),
            }),
            _ => return None,
        })
    }
}

impl StepRegistry<JujuControllerChange> for JujuSteps {
    fn build(&self, step: &JujuStep) -> Result<Box<dyn Step<JujuControllerChange>>, ActionError> {
        self.controller_step(step)
            .or_else(|| self.model_step(step))
            .or_else(|| self.application_step(step))
            .ok_or_else(|| ActionError::fatal(format!("no runner knows the Juju step {step:?}")))
    }
}
