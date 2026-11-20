mod authority;
pub mod kubernetes;
mod machines;
mod objects;

use amafu_domain::ObjectReference;
use amafu_domain::kinds::kubernetes_cluster::KubernetesClusterChange;
use amafu_reconciler::WatchedKind;

pub use authority::CertificateAuthority;
pub use machines::{ContainerPlan, Machine, MachinePlan};
pub use objects::{EnvironmentObjects, Ownership};

use crate::operations::{RunnerKind, watching};

impl RunnerKind for KubernetesClusterChange {
    const RUNNER: &'static str = "kubernetes-changes";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &watching("KubernetesClusterChange");
}
