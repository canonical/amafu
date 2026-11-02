use std::sync::Arc;
use std::time::Duration;

use amafu_api::diagnostics::{AUDIT_PATH, INSTANCES_PATH, NodeInstance};
use amafu_api::wire::ListResponse;
use amafu_api::{ApiClient, HttpTransport, Transport};
use amafu_domain::access::Role;
use amafu_domain::{NodeName, ProjectName};
use amafu_interfaces::Clock;
use amafu_scenario::{
    Cluster, ClusterObservation, Fault, FaultHandle, Level, ObservedInstance, ScenarioError,
};
use futures::future::BoxFuture;
use http::Method;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::lab::{LabState, lxd_client};
use crate::lxd::LxdClient;
use crate::topology::LabNode;

#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub struct VirtualLab {
    state: LabState,
    lxd: LxdClient,
    transport: Arc<dyn Transport>,
    diagnostics: reqwest::Client,
    clock: WallClock,
}

fn lab_failure(error: impl std::fmt::Display) -> ScenarioError {
    ScenarioError::Lab(error.to_string())
}

impl VirtualLab {
    pub fn connect(state: LabState) -> anyhow::Result<Self> {
        let lxd = lxd_client(&state.settings)?;
        let diagnostics = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        let core = state.topology.nodes.first().ok_or_else(|| anyhow::anyhow!("the lab has no node"))?;
        let endpoint = core.api_endpoint(&state.settings);
        let transport =
            Arc::new(HttpTransport::new(&endpoint, Some(state.server.certificate_authority.as_bytes()))?);
        Ok(Self { state, lxd, transport, diagnostics, clock: WallClock })
    }

    fn core_node(&self) -> Result<&LabNode, ScenarioError> {
        self.state
            .topology
            .nodes
            .first()
            .ok_or_else(|| ScenarioError::Lab(String::from("the lab has no node")))
    }

    fn client_with_token(&self, token: Option<String>) -> ApiClient {
        ApiClient::new(Arc::clone(&self.transport), token)
    }

    async fn node_instances(&self, node: &LabNode) -> Vec<NodeInstance> {
        let url = format!("{}{INSTANCES_PATH}", node.diagnostics_endpoint(&self.state.settings));
        match self.diagnostics.get(url).send().await {
            Ok(response) if response.status().is_success() => response.json().await.unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    async fn stored_objects(&self) -> Result<Vec<serde_json::Value>, ScenarioError> {
        let project = ProjectName::new(crate::credentials::PROJECTS[0]).map_err(lab_failure)?;
        let admin = self.client_in_project(Role::Admin, &project).await;
        let mut objects = Vec::new();
        for path in ["/v1/vms?all-projects=true", "/v1/nodes"] {
            let response = admin.raw(Method::GET, path, None).await?;
            if response.status == 200 {
                let list: ListResponse = serde_json::from_slice(&response.body).map_err(lab_failure)?;
                objects.extend(list.items);
            }
        }
        Ok(objects)
    }
}

#[async_trait::async_trait(?Send)]
impl Cluster for VirtualLab {
    async fn client_in_project(&self, role: Role, project: &ProjectName) -> ApiClient {
        self.client_with_token(self.state.tokens.token(role, project)).in_project(project.clone())
    }

    async fn anonymous_client(&self) -> ApiClient {
        self.client_with_token(None)
    }

    async fn expired_client(&self) -> ApiClient {
        self.client_with_token(Some(self.state.tokens.expired.clone()))
    }

    async fn nodes(&self) -> Vec<NodeName> {
        self.state.topology.nodes.iter().filter_map(|node| NodeName::new(node.name.as_str()).ok()).collect()
    }

    async fn inject(&self, fault: Fault) -> Result<FaultHandle, ScenarioError> {
        match &fault {
            Fault::PowerCycle { node } => {
                self.lxd.change_state(node.as_str(), "restart", true).await.map_err(lab_failure)?;
            }
        }
        Ok(FaultHandle(0))
    }

    async fn heal(&self, _handle: FaultHandle) -> Result<(), ScenarioError> {
        Ok(())
    }

    async fn observe(&self) -> Result<ClusterObservation, ScenarioError> {
        let mut instances = Vec::new();
        for node in &self.state.topology.nodes {
            let name = NodeName::new(node.name.as_str()).map_err(lab_failure)?;
            for instance in self.node_instances(node).await {
                instances.push(ObservedInstance {
                    node: name.clone(),
                    vm_id: instance.vm_id,
                    state: instance.state,
                    workload: None,
                    disks: Vec::new(),
                    offered_addresses: Vec::new(),
                });
            }
        }
        Ok(ClusterObservation {
            instances,
            stored_objects: self.stored_objects().await.unwrap_or_default(),
            ..ClusterObservation::default()
        })
    }

    async fn audit_contains(&self, request_id: Uuid) -> Result<bool, ScenarioError> {
        let node = self.core_node()?;
        let url = format!("{}{AUDIT_PATH}/{request_id}", node.diagnostics_endpoint(&self.state.settings));
        let response = self.diagnostics.get(url).send().await.map_err(lab_failure)?;
        match response.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            status => Err(ScenarioError::Lab(format!("the audit lookup answered {status}"))),
        }
    }

    fn clock(&self) -> &dyn Clock {
        &self.clock
    }

    fn level(&self) -> Level {
        Level::VirtualLab
    }
}
