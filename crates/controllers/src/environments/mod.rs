mod authority;
pub mod failover;
pub mod host_health;
pub mod juju;
pub mod kubernetes;
mod machines;
mod objects;
pub mod rotation;
mod ssh;

use amafu_domain::ObjectReference;
use amafu_domain::kinds::juju_controller::JujuControllerChange;
use amafu_domain::kinds::kubernetes_cluster::KubernetesClusterChange;
use amafu_reconciler::WatchedKind;

pub use authority::CertificateAuthority;
pub use machines::{ContainerPlan, Machine, MachinePlan};
pub use objects::{EnvironmentObjects, Ownership};
pub use ssh::SshKeyPair;

use crate::operations::{RunnerKind, watching};

impl RunnerKind for KubernetesClusterChange {
    const RUNNER: &'static str = "kubernetes-changes";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &watching("KubernetesClusterChange");
}

impl RunnerKind for JujuControllerChange {
    const RUNNER: &'static str = "juju-changes";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &watching("JujuControllerChange");
}
