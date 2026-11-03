use std::fmt;
use std::sync::Arc;

use amafu_api::ApiClient;
use std::net::Ipv4Addr;

use amafu_api::NodeClient;
use amafu_api::diagnostics::SandboxCheck;
use amafu_domain::access::Role;
use amafu_domain::kinds::external_network::ExternalNetworkSpecification;
use amafu_domain::{NodeName, ProjectName};
use amafu_interfaces::Clock;
use uuid::Uuid;

use crate::certificates::{CertificateAuthority, TlsIdentity};
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
    Partition { node: NodeName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaultHandle(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsExport {
    pub server: String,
    pub export: String,
}

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

    async fn sandbox_checks(&self) -> Result<Option<Vec<SandboxCheck>>, ScenarioError>;

    fn nfs_export(&self) -> Option<NfsExport>;

    fn certificate_authority(&self) -> &CertificateAuthority;

    fn external_network(&self) -> ExternalNetworkSpecification;

    async fn probe_from_outside(&self, address: Ipv4Addr, port: u16)
    -> Result<Option<String>, ScenarioError>;

    async fn node_connection(&self, identity: Option<&TlsIdentity>)
    -> Result<Arc<NodeClient>, ScenarioError>;

    fn clock(&self) -> &dyn Clock;

    fn level(&self) -> Level;
}

pub const DEFAULT_PROJECT: &str = "project-a";

pub async fn client(cluster: &dyn Cluster, role: Role) -> ApiClient {
    let project = ProjectName::new(DEFAULT_PROJECT).unwrap_or_else(|_| unreachable!());
    cluster.client_in_project(role, &project).await
}
