use std::fmt;

use amafu_api::ApiClient;
use amafu_domain::access::Role;
use amafu_domain::{NodeName, ProjectName};
use amafu_interfaces::Clock;
use uuid::Uuid;

use crate::error::ScenarioError;
use crate::observation::ClusterObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Simulated,
    VirtualLab,
    MaasLab,
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Simulated => "simulated",
            Self::VirtualLab => "virtual-lab",
            Self::MaasLab => "maas-lab",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    PowerCycle { node: NodeName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaultHandle(pub u64);

#[async_trait::async_trait(?Send)]
pub trait Cluster {
    async fn client_in_project(&self, role: Role, project: &ProjectName) -> ApiClient;

    async fn anonymous_client(&self) -> ApiClient;

    async fn expired_client(&self) -> ApiClient;

    async fn nodes(&self) -> Vec<NodeName>;

    async fn inject(&self, fault: Fault) -> Result<FaultHandle, ScenarioError>;

    async fn heal(&self, handle: FaultHandle) -> Result<(), ScenarioError>;

    async fn observe(&self) -> Result<ClusterObservation, ScenarioError>;

    async fn audit_contains(&self, request_id: Uuid) -> Result<bool, ScenarioError>;

    fn clock(&self) -> &dyn Clock;

    fn level(&self) -> Level;
}

pub const DEFAULT_PROJECT: &str = "project-a";

pub async fn client(cluster: &dyn Cluster, role: Role) -> ApiClient {
    let project = ProjectName::new(DEFAULT_PROJECT).unwrap_or_else(|_| unreachable!());
    cluster.client_in_project(role, &project).await
}
