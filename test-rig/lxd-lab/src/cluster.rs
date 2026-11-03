use std::sync::Arc;
use std::time::Duration;

use amafu_api::diagnostics::{
    ATTACHMENTS_PATH, AUDIT_PATH, INSTANCES_PATH, NETWORK_PATH, NodeAttachment, NodeInstance,
    NodeNetworkFacts, SANDBOX_PATH, SandboxCheck,
};
use amafu_api::wire::ListResponse;
use amafu_api::{ApiClient, ClientIdentity, HttpTransport, NodeClient, Transport};
use std::net::Ipv4Addr;

use amafu_domain::access::Role;
use amafu_domain::kinds::external_network::ExternalNetworkSpecification;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_domain::{NodeName, ProjectName};
use amafu_interfaces::Clock;
use amafu_scenario::{
    CertificateAuthority, Cluster, ClusterObservation, Fault, FaultHandle, Level, NfsExport,
    ObservedAttachment, ObservedFloatingAddress, ObservedInstance, ObservedPort, ObservedRemoteEndpoint,
    ScenarioError, TlsIdentity,
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
    core_transports: Vec<Arc<dyn Transport>>,
    diagnostics: reqwest::Client,
    probes: reqwest::Client,
    clock: WallClock,
}

fn lab_failure(error: impl std::fmt::Display) -> ScenarioError {
    ScenarioError::Lab(error.to_string())
}

impl VirtualLab {
    pub fn connect(state: LabState) -> anyhow::Result<Self> {
        let lxd = lxd_client(&state.settings)?;
        let diagnostics = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        let probes = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
        let core = state.topology.nodes.first().ok_or_else(|| anyhow::anyhow!("the lab has no node"))?;
        let endpoint = core.api_endpoint(&state.settings);
        let authority = state.server.certificate_authority.as_bytes();
        let transport = Arc::new(HttpTransport::new(&endpoint, Some(authority))?);
        let mut core_transports: Vec<Arc<dyn Transport>> = Vec::new();
        for node in state.topology.nodes.iter().filter(|node| node.is_core) {
            core_transports
                .push(Arc::new(HttpTransport::new(&node.api_endpoint(&state.settings), Some(authority))?));
        }
        Ok(Self { state, lxd, transport, core_transports, diagnostics, probes, clock: WallClock })
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

    async fn node_facts<Facts: serde::de::DeserializeOwned + Default>(
        &self,
        node: &LabNode,
        path: &str,
    ) -> Facts {
        let url = format!("{}{path}", node.diagnostics_endpoint(&self.state.settings));
        match self.diagnostics.get(url).send().await {
            Ok(response) if response.status().is_success() => response.json().await.unwrap_or_default(),
            _ => Facts::default(),
        }
    }

    async fn observe_node(
        &self,
        node: &LabNode,
        observation: &mut ClusterObservation,
    ) -> Result<(), ScenarioError> {
        let name = NodeName::new(node.name.as_str()).map_err(lab_failure)?;
        let instances: Vec<NodeInstance> = self.node_facts(node, INSTANCES_PATH).await;
        let network: NodeNetworkFacts = self.node_facts(node, NETWORK_PATH).await;
        let attachments: Vec<NodeAttachment> = self.node_facts(node, ATTACHMENTS_PATH).await;
        for instance in instances {
            let offered_addresses = instance
                .mac_addresses
                .iter()
                .filter_map(|mac| network.ports.iter().find(|port| port.mac == *mac).map(|port| port.address))
                .collect();
            observation.instances.push(ObservedInstance {
                node: name.clone(),
                vm_id: instance.vm_id,
                state: instance.state,
                workload: instance.workload,
                disks: instance.disks,
                offered_addresses,
                console: instance.console_tail,
            });
        }
        observation.attachments.extend(attachments.into_iter().map(|attachment| ObservedAttachment {
            node: name.clone(),
            volume: attachment.volume,
            path: attachment.path,
        }));
        observation.ports.extend(network.ports.into_iter().map(|port| ObservedPort {
            node: name.clone(),
            vm_id: port.vm_id,
            segment: port.segment,
            address: port.address,
        }));
        observation.remote_endpoints.extend(network.remote_endpoints.into_iter().map(|remote| {
            ObservedRemoteEndpoint {
                node: name.clone(),
                segment: remote.segment,
                address: remote.address,
                node_address: remote.node_address,
            }
        }));
        observation.floating_addresses.extend(network.floating_addresses.into_iter().map(|floating| {
            ObservedFloatingAddress {
                node: name.clone(),
                address: floating.address,
                private_address: floating.private_address,
            }
        }));
        Ok(())
    }

    async fn stored_objects(&self) -> Result<Vec<serde_json::Value>, ScenarioError> {
        let project = ProjectName::new(crate::credentials::PROJECTS[0]).map_err(lab_failure)?;
        let admin = self.client_in_project(Role::Admin, &project).await;
        let mut objects = Vec::new();
        for path in ["/v1/vms?all-projects=true", "/v1/volumes?all-projects=true", "/v1/nodes"] {
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

    async fn core_member_clients(&self, role: Role, project: &ProjectName) -> Vec<ApiClient> {
        let token = self.state.tokens.token(role, project);
        self.core_transports
            .iter()
            .map(|transport| ApiClient::new(Arc::clone(transport), token.clone()).in_project(project.clone()))
            .collect()
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
            Fault::Partition { node } => {
                return Err(ScenarioError::Lab(format!("{node} cannot be partitioned without a lab switch")));
            }
        }
        Ok(FaultHandle(0))
    }

    async fn heal(&self, _handle: FaultHandle) -> Result<(), ScenarioError> {
        Ok(())
    }

    async fn observe(&self) -> Result<ClusterObservation, ScenarioError> {
        let mut observation = ClusterObservation::default();
        for node in &self.state.topology.nodes {
            self.observe_node(node, &mut observation).await?;
        }
        observation.stored_objects = self.stored_objects().await.unwrap_or_default();
        Ok(observation)
    }

    async fn sandbox_checks(&self) -> Result<Option<Vec<SandboxCheck>>, ScenarioError> {
        let node = self.core_node()?;
        let url = format!("{}{SANDBOX_PATH}", node.diagnostics_endpoint(&self.state.settings));
        let response = self.probes.post(url).send().await.map_err(lab_failure)?;
        Ok(Some(response.json().await.map_err(lab_failure)?))
    }

    fn certificate_authority(&self) -> &CertificateAuthority {
        &self.state.authority
    }

    fn external_network(&self) -> ExternalNetworkSpecification {
        self.state.topology.external_network.clone()
    }

    fn block_backends(&self) -> Vec<(String, StorageProviderSpecification)> {
        self.state.block_backends.clone()
    }

    async fn probe_from_outside(
        &self,
        address: Ipv4Addr,
        port: u16,
    ) -> Result<Option<String>, ScenarioError> {
        crate::switch::probe_http(&self.lxd, address, port).await.map(Some).map_err(lab_failure)
    }

    async fn node_connection(
        &self,
        identity: Option<&TlsIdentity>,
    ) -> Result<Arc<NodeClient>, ScenarioError> {
        let endpoint = self.core_node()?.api_endpoint(&self.state.settings);
        let client_identity = identity.map(|identity| ClientIdentity {
            certificate_pem: identity.certificate_pem.clone(),
            private_key_pem: identity.private_key_pem.clone(),
        });
        let transport = HttpTransport::with_identity(
            &endpoint,
            Some(self.state.authority.certificate_pem.as_bytes()),
            client_identity.as_ref(),
        )
        .map_err(lab_failure)?;
        Ok(Arc::new(NodeClient::new(vec![Arc::new(transport)])))
    }

    fn nfs_export(&self) -> Option<NfsExport> {
        self.state
            .nfs
            .as_ref()
            .map(|nfs| NfsExport { server: nfs.address.to_string(), export: nfs.export.clone() })
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
