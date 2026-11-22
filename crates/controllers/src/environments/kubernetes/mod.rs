mod add_ons;
mod cloud_init;
mod lifecycle;
mod nodes;
mod scope;

use std::sync::Arc;

use amafu_domain::kinds::kubernetes_cluster::{KubernetesClusterChange, KubernetesStep};
use amafu_interfaces::{ActionError, Identifiers, KubernetesPlatform};
use amafu_reconciler::{Step, StepRegistry};
use amafu_token_issuer::Keyring;

pub use cloud_init::IssuerTrust;
pub use scope::{endpoint_of, ownership_of};

pub struct KubernetesSettings {
    pub trust: IssuerTrust,
}

pub struct KubernetesEnvironment {
    pub platform: Arc<dyn KubernetesPlatform>,
    pub keyring: Keyring,
    pub identifiers: Arc<dyn Identifiers>,
    pub settings: KubernetesSettings,
}

pub struct KubernetesSteps {
    environment: Arc<KubernetesEnvironment>,
}

impl KubernetesSteps {
    #[must_use]
    pub fn new(environment: KubernetesEnvironment) -> Self {
        Self { environment: Arc::new(environment) }
    }
}

impl StepRegistry<KubernetesClusterChange> for KubernetesSteps {
    fn build(&self, step: &KubernetesStep) -> Result<Box<dyn Step<KubernetesClusterChange>>, ActionError> {
        let environment = Arc::clone(&self.environment);
        Ok(match step {
            KubernetesStep::RecordCluster => Box::new(lifecycle::RecordCluster { environment }),
            KubernetesStep::PrepareNetworkAccess => Box::new(lifecycle::PrepareNetworkAccess { environment }),
            KubernetesStep::ImportImage => Box::new(lifecycle::ImportImage { environment }),
            KubernetesStep::BootstrapControlPlane { member } => {
                Box::new(nodes::BootstrapControlPlane { environment, member: member.clone() })
            }
            KubernetesStep::AwaitApi => Box::new(lifecycle::AwaitApi { environment }),
            KubernetesStep::JoinNode { member } => {
                Box::new(nodes::JoinNode { environment, member: member.clone() })
            }
            KubernetesStep::InstallAddOns => Box::new(lifecycle::InstallAddOns { environment }),
            KubernetesStep::MarkReady => Box::new(lifecycle::MarkReady { environment }),
            KubernetesStep::RemoveNode { member } => {
                Box::new(nodes::RemoveNode { environment, member: member.clone() })
            }
            KubernetesStep::DrainNode { member } => {
                Box::new(nodes::DrainNode { environment, member: member.clone() })
            }
            KubernetesStep::StopNode { member } => {
                Box::new(nodes::StopNode { environment, member: member.clone() })
            }
            KubernetesStep::StartNode { member } => {
                Box::new(nodes::StartNode { environment, member: member.clone() })
            }
            KubernetesStep::RecordVersion { version } => {
                Box::new(lifecycle::RecordVersion { environment, version: version.clone() })
            }
            KubernetesStep::RecordShape => Box::new(lifecycle::RecordShape { environment }),
            KubernetesStep::RefuseWhileHosting => Box::new(lifecycle::RefuseWhileHosting { environment }),
            KubernetesStep::DeleteResources => Box::new(lifecycle::DeleteResources { environment }),
        })
    }
}
