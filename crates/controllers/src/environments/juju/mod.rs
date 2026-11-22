mod applications;
mod bootstrap;
mod controller;
mod models;
mod provisioning;
mod scope;

use std::sync::Arc;

use amafu_domain::kinds::juju_controller::{JujuControllerChange, JujuStep};
use amafu_interfaces::{ActionError, Identifiers, JujuPlatform, KubernetesPlatform};
use amafu_reconciler::{Step, StepRegistry};
use amafu_token_issuer::Keyring;

pub use bootstrap::BOOTSTRAP_KEY_COMMENT;

pub struct JujuSettings {
    pub token_refresh_url: String,
    pub amafu_certificate_authority: String,
}

pub struct JujuEnvironment {
    pub platform: Arc<dyn JujuPlatform>,
    pub kubernetes: Arc<dyn KubernetesPlatform>,
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
            JujuStep::CreateControllerMachines => {
                Box::new(controller::CreateControllerMachines { environment })
            }
            JujuStep::CheckHostCluster => Box::new(controller::CheckHostCluster { environment }),
            JujuStep::CreateClusterServiceAccount => {
                Box::new(controller::CreateClusterServiceAccount { environment })
            }
            JujuStep::Bootstrap => Box::new(controller::Bootstrap { environment }),
            JujuStep::AwaitControllerService => Box::new(controller::AwaitControllerService { environment }),
            JujuStep::RecordController => Box::new(controller::RecordController { environment }),
            JujuStep::AddControllerMachines => Box::new(controller::AddControllerMachines { environment }),
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
            JujuStep::CreateMachines { model, application, shape, .. } => {
                Box::new(applications::CreateMachines {
                    environment,
                    model: model.clone(),
                    application: application.clone(),
                    shape: *shape,
                })
            }
            JujuStep::Deploy { deployment } => {
                Box::new(applications::Deploy { environment, deployment: deployment.clone() })
            }
            JujuStep::AddUnits { model, application } => Box::new(applications::AddUnits {
                environment,
                model: model.clone(),
                application: application.clone(),
            }),
            JujuStep::RemoveUnits { model, units, .. } => Box::new(applications::RemoveUnits {
                environment,
                model: model.clone(),
                units: units.clone(),
            }),
            JujuStep::RemoveApplication { model, application } => Box::new(applications::RemoveApplication {
                environment,
                model: model.clone(),
                application: application.clone(),
            }),
            JujuStep::DeleteReleasedMachines { model } => {
                Box::new(applications::DeleteReleasedMachines { environment, model: model.clone() })
            }
            JujuStep::ReplaceMachine { model, machine } => Box::new(applications::ReplaceMachine {
                environment,
                model: model.clone(),
                machine: machine.clone(),
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
