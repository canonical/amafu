use std::sync::Arc;

use amafu_api::ApiClient;
use amafu_controllers::operations::upgrade::{VerifyImage, planned_image};
use amafu_domain::NodeName;
use amafu_domain::kinds::cluster_version::{
    CLUSTER_VERSION_NAME, ClusterVersion, ClusterVersionSpecification, cluster_version_name,
};
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::StepSpecification;
use amafu_interfaces::{
    ActionError, Installation, MachineName, MachineProvisioner, MachineState, ProvisioningError,
    TrustedImageKeys,
};
use amafu_reconciler::{OperationContext, Step, StepRegistry};

use crate::configuration::{ClusterCredentials, node_configuration};
use crate::description::InstallDescription;
use crate::user_data::installer_script;

fn provisioning_failure(error: ProvisioningError) -> ActionError {
    if error.is_retryable() { ActionError::retryable(error) } else { ActionError::invalid(error) }
}

fn client_failure(error: amafu_api::ClientError) -> ActionError {
    ActionError::retryable(error)
}

pub struct InstallSteps {
    pub description: Arc<InstallDescription>,
    pub credentials: Arc<ClusterCredentials>,
    pub provisioner: Arc<dyn MachineProvisioner>,
    pub cluster: Arc<ApiClient>,
    pub image_keys: Arc<TrustedImageKeys>,
}

impl StepRegistry for InstallSteps {
    fn build(&self, specification: &StepSpecification) -> Result<Box<dyn Step>, ActionError> {
        Ok(match specification {
            StepSpecification::VerifyImage => Box::new(VerifyImage { keys: Arc::clone(&self.image_keys) }),
            StepSpecification::DeployMachine { node } => Box::new(DeployMachine {
                node: node.clone(),
                description: Arc::clone(&self.description),
                credentials: Arc::clone(&self.credentials),
                provisioner: Arc::clone(&self.provisioner),
            }),
            StepSpecification::AwaitHealthy { node } => {
                Box::new(AwaitJoined { node: node.clone(), cluster: Arc::clone(&self.cluster) })
            }
            StepSpecification::SetClusterImage => {
                Box::new(RecordInstalledImage { cluster: Arc::clone(&self.cluster) })
            }
            other => {
                return Err(ActionError::fatal(format!("the installer does not run {}", other.name())));
            }
        })
    }
}

pub struct DeployMachine {
    pub node: NodeName,
    pub description: Arc<InstallDescription>,
    pub credentials: Arc<ClusterCredentials>,
    pub provisioner: Arc<dyn MachineProvisioner>,
}

impl DeployMachine {
    fn machine(&self) -> Result<MachineName, ActionError> {
        self.description
            .machine(&self.node)
            .map(|machine| MachineName(machine.hostname.clone()))
            .ok_or_else(|| ActionError::fatal(format!("{} is not in the install description", self.node)))
    }

    fn installation(&self, context: &OperationContext<'_>) -> Result<Installation, ActionError> {
        let machine = self
            .description
            .machine(&self.node)
            .ok_or_else(|| ActionError::fatal(format!("{} is not in the install description", self.node)))?;
        let image = planned_image(context)?;
        let configuration =
            node_configuration(&self.description, machine, &self.credentials).map_err(ActionError::fatal)?;
        let configuration = serde_yaml_ng::to_string(&configuration).map_err(ActionError::fatal)?;
        let manifest = serde_json::to_vec_pretty(image).map_err(ActionError::fatal)?;
        Ok(Installation {
            user_data: installer_script(
                self.node.as_str(),
                image,
                &self.description.cluster.boot_loader,
                &manifest,
                configuration.as_bytes(),
            ),
        })
    }
}

#[async_trait::async_trait]
impl Step for DeployMachine {
    async fn is_complete(&self, _context: &OperationContext<'_>) -> Result<bool, ActionError> {
        match self.provisioner.state(&self.machine()?).await.map_err(provisioning_failure)? {
            MachineState::Deployed => Ok(true),
            MachineState::Failed { reason } => {
                Err(ActionError::invalid(format!("MAAS could not deploy {}: {reason}", self.node)))
            }
            _ => Ok(false),
        }
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let machine = self.machine()?;
        match self.provisioner.state(&machine).await.map_err(provisioning_failure)? {
            MachineState::Ready | MachineState::Allocated => {
                tracing::info!(node = %self.node, %machine, "deploying");
                self.provisioner
                    .deploy(&machine, &self.installation(context)?)
                    .await
                    .map_err(provisioning_failure)
            }
            MachineState::Unavailable { status } => {
                Err(ActionError::retryable(format!("{machine} is {status}, not ready to deploy")))
            }
            MachineState::Deploying | MachineState::Deployed | MachineState::Failed { .. } => Ok(()),
        }
    }
}

pub struct AwaitJoined {
    pub node: NodeName,
    pub cluster: Arc<ApiClient>,
}

#[async_trait::async_trait]
impl Step for AwaitJoined {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        match self.cluster.get::<Node>(self.node.as_str()).await {
            Ok(node) => Ok(node.status.is_ready() && node.status.runs_confirmed(image)),
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => {
                tracing::info!(node = %self.node, %error, "the cluster does not answer yet");
                Ok(false)
            }
        }
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct RecordInstalledImage {
    pub cluster: Arc<ApiClient>,
}

#[async_trait::async_trait]
impl Step for RecordInstalledImage {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        match self.cluster.get::<ClusterVersion>(CLUSTER_VERSION_NAME).await {
            Ok(version) => Ok(version.specification.image == *image),
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => Err(client_failure(error)),
        }
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let specification = ClusterVersionSpecification::installed(planned_image(context)?.clone());
        match self.cluster.create::<ClusterVersion>(&cluster_version_name(), &specification).await {
            Ok(_) => Ok(()),
            Err(error) if error.is_conflict() => Ok(()),
            Err(error) => Err(client_failure(error)),
        }
    }
}
