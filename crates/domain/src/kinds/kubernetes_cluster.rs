use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::access::ProjectPolicy;
use crate::kinds::environment::{EnvironmentChange, EnvironmentPhase, EnvironmentStatus};
use crate::kinds::machine_shape::MachineShape;
use crate::kinds::operation::OperationStatus;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::step_plan::{PlannedStep, StepPlan};
use crate::validation::{Validate, ValidationError};

pub const CONTROL_PLANE_SIZES: [u8; 3] = [1, 3, 5];
pub const MAXIMUM_POOLS: usize = 16;
pub const MAXIMUM_POOL_NODES: u16 = 100;
pub const LONGEST_CLUSTER_NAME: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesCluster;

impl Resource for KubernetesCluster {
    const KIND: &'static str = "KubernetesCluster";
    const PLURAL_NAME: &'static str = "kubernetes-clusters";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    const PROJECT_POLICY: ProjectPolicy = ProjectPolicy::ManagedByNodes;
    type Specification = KubernetesClusterSpecification;
    type Status = KubernetesClusterStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KubernetesVersion {
    major: u16,
    minor: u16,
}

impl KubernetesVersion {
    #[must_use]
    pub fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    #[must_use]
    pub fn snap_channel(&self) -> String {
        format!("{self}-classic/stable")
    }
}

impl fmt::Display for KubernetesVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

impl TryFrom<String> for KubernetesVersion {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let invalid = || format!("{text:?} is not a Kubernetes version such as 1.34");
        let (major, minor) = text.split_once('.').ok_or_else(invalid)?;
        Ok(Self {
            major: major.parse().map_err(|_| invalid())?,
            minor: minor.parse().map_err(|_| invalid())?,
        })
    }
}

impl From<KubernetesVersion> for String {
    fn from(version: KubernetesVersion) -> Self {
        version.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePool {
    pub nodes: u16,
    pub shape: MachineShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubernetesClusterSpecification {
    pub version: KubernetesVersion,
    pub control_plane_nodes: u8,
    pub control_plane_shape: MachineShape,
    #[serde(default)]
    pub node_pools: BTreeMap<ObjectName, NodePool>,
    pub network: ObjectName,
    pub storage_backend: ObjectName,
    pub external_network: ObjectName,
    #[serde(default)]
    pub organization_sign_in: bool,
}

impl KubernetesClusterSpecification {
    fn check(&self, field: &str, error: &mut ValidationError) {
        error.check(
            CONTROL_PLANE_SIZES.contains(&self.control_plane_nodes),
            &format!("{field}.control_plane_nodes"),
            "must be 1, 3 or 5",
        );
        error.check(
            self.node_pools.len() <= MAXIMUM_POOLS,
            &format!("{field}.node_pools"),
            "must be at most 16",
        );
        for (name, pool) in &self.node_pools {
            error.check(
                pool.nodes <= MAXIMUM_POOL_NODES,
                &format!("{field}.node_pools.{name}.nodes"),
                "must be at most 100",
            );
        }
    }

    #[must_use]
    pub fn members(&self) -> Vec<ClusterMember> {
        let control_plane = (0..u16::from(self.control_plane_nodes)).map(ClusterMember::control_plane);
        let workers = self.node_pools.iter().flat_map(|(pool, node_pool)| {
            (0..node_pool.nodes).map(|index| ClusterMember::worker(pool.clone(), index))
        });
        control_plane.chain(workers).collect()
    }

    #[must_use]
    pub fn shape_of(&self, member: &ClusterMember) -> MachineShape {
        match &member.pool {
            None => self.control_plane_shape,
            Some(pool) => self.node_pools.get(pool).map_or(self.control_plane_shape, |pool| pool.shape),
        }
    }
}

impl Validate for KubernetesClusterSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        self.check("specification", &mut error);
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterMember {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<ObjectName>,
    pub index: u16,
}

impl ClusterMember {
    #[must_use]
    pub fn control_plane(index: u16) -> Self {
        Self { pool: None, index }
    }

    #[must_use]
    pub fn worker(pool: ObjectName, index: u16) -> Self {
        Self { pool: Some(pool), index }
    }

    #[must_use]
    pub fn is_control_plane(&self) -> bool {
        self.pool.is_none()
    }

    #[must_use]
    pub fn vm_name(&self, cluster: &ObjectName) -> String {
        match &self.pool {
            None => format!("{cluster}-cp-{}", self.index),
            Some(pool) => format!("{cluster}-{pool}-{}", self.index),
        }
    }

    #[must_use]
    pub fn replacement(&self, taken: &[ClusterMember]) -> Self {
        let next = taken
            .iter()
            .filter(|other| other.pool == self.pool)
            .map(|other| other.index + 1)
            .max()
            .unwrap_or_default();
        Self { pool: self.pool.clone(), index: next.max(self.index + 1) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubernetesNodeStatus {
    pub member: ClusterMember,
    pub vm: ObjectName,
    pub version: KubernetesVersion,
    #[serde(default)]
    pub is_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KubernetesClusterStatus {
    #[serde(default)]
    pub phase: EnvironmentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_authority: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<KubernetesNodeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosted_juju_controllers: Vec<ObjectName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl EnvironmentStatus for KubernetesClusterStatus {
    fn phase(&self) -> EnvironmentPhase {
        self.phase
    }

    fn failed(&self, reason: &str) -> Self {
        Self { phase: EnvironmentPhase::Failed, message: Some(reason.to_owned()), ..self.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesClusterChange;

impl Resource for KubernetesClusterChange {
    const KIND: &'static str = "KubernetesClusterChange";
    const PLURAL_NAME: &'static str = "kubernetes-cluster-changes";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = KubernetesClusterChangeSpecification;
    type Status = OperationStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum KubernetesRequest {
    Create { cluster: KubernetesClusterSpecification },
    Delete,
    Upgrade { version: KubernetesVersion, nodes: Vec<ClusterMember> },
    ScalePool { pool: ObjectName, nodes: Vec<ClusterMember>, to: u16 },
    AddPool { pool: ObjectName, node_pool: NodePool },
    RemovePool { pool: ObjectName, nodes: Vec<ClusterMember> },
    DrainNode { member: ClusterMember },
    RestartNode { member: ClusterMember },
}

impl KubernetesRequest {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Delete => "delete",
            Self::Upgrade { .. } => "upgrade",
            Self::ScalePool { .. } => "scale-pool",
            Self::AddPool { .. } => "add-pool",
            Self::RemovePool { .. } => "remove-pool",
            Self::DrainNode { .. } => "drain-node",
            Self::RestartNode { .. } => "restart-node",
        }
    }

    #[must_use]
    pub fn steps(&self) -> Vec<KubernetesStep> {
        match self {
            Self::Create { cluster } => creation_steps(cluster),
            Self::Delete => vec![KubernetesStep::RefuseWhileHosting, KubernetesStep::DeleteResources],
            Self::Upgrade { version, nodes } => upgrade_steps(version, nodes),
            Self::ScalePool { pool, nodes, to } => scale_steps(pool, nodes, *to),
            Self::AddPool { pool, node_pool } => (0..node_pool.nodes)
                .map(|index| KubernetesStep::JoinNode { member: ClusterMember::worker(pool.clone(), index) })
                .chain([KubernetesStep::RecordShape])
                .collect(),
            Self::RemovePool { nodes, .. } => {
                removal_steps(nodes).chain([KubernetesStep::RecordShape]).collect()
            }
            Self::DrainNode { member } => vec![KubernetesStep::DrainNode { member: member.clone() }],
            Self::RestartNode { member } => vec![
                KubernetesStep::DrainNode { member: member.clone() },
                KubernetesStep::StopNode { member: member.clone() },
                KubernetesStep::StartNode { member: member.clone() },
            ],
        }
    }
}

fn creation_steps(cluster: &KubernetesClusterSpecification) -> Vec<KubernetesStep> {
    let members = cluster.members();
    let (bootstrap, joining) =
        members.split_first().map_or((None, &[][..]), |(first, rest)| (Some(first), rest));
    [KubernetesStep::RecordCluster, KubernetesStep::PrepareNetworkAccess, KubernetesStep::ImportImage]
        .into_iter()
        .chain(bootstrap.map(|member| KubernetesStep::BootstrapControlPlane { member: member.clone() }))
        .chain([KubernetesStep::AwaitApi])
        .chain(joining.iter().map(|member| KubernetesStep::JoinNode { member: member.clone() }))
        .chain([KubernetesStep::InstallAddOns, KubernetesStep::MarkReady])
        .collect()
}

fn upgrade_steps(version: &KubernetesVersion, nodes: &[ClusterMember]) -> Vec<KubernetesStep> {
    let mut ordered = nodes.to_vec();
    ordered.sort_by_key(|member| !member.is_control_plane());
    let mut taken = nodes.to_vec();
    let mut steps = vec![KubernetesStep::ImportImage];
    for member in ordered {
        let replacement = member.replacement(&taken);
        taken.push(replacement.clone());
        steps.push(KubernetesStep::JoinNode { member: replacement });
        steps.push(KubernetesStep::RemoveNode { member });
    }
    steps.push(KubernetesStep::RecordVersion { version: version.clone() });
    steps
}

fn scale_steps(pool: &ObjectName, nodes: &[ClusterMember], to: u16) -> Vec<KubernetesStep> {
    let current = u16::try_from(nodes.len()).unwrap_or(u16::MAX);
    let mut steps: Vec<KubernetesStep> = if to >= current {
        let mut taken = nodes.to_vec();
        (current..to)
            .map(|_| {
                let member = ClusterMember::worker(pool.clone(), 0).replacement(&taken);
                taken.push(member.clone());
                KubernetesStep::JoinNode { member }
            })
            .collect()
    } else {
        let mut newest_first = nodes.to_vec();
        newest_first.sort_by_key(|member| std::cmp::Reverse(member.index));
        removal_steps(&newest_first[..usize::from(current - to)]).collect()
    };
    steps.push(KubernetesStep::RecordShape);
    steps
}

fn removal_steps(nodes: &[ClusterMember]) -> impl Iterator<Item = KubernetesStep> + '_ {
    nodes.iter().map(|member| KubernetesStep::RemoveNode { member: member.clone() })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case", deny_unknown_fields)]
pub enum KubernetesStep {
    RecordCluster,
    PrepareNetworkAccess,
    ImportImage,
    BootstrapControlPlane { member: ClusterMember },
    AwaitApi,
    JoinNode { member: ClusterMember },
    InstallAddOns,
    MarkReady,
    RemoveNode { member: ClusterMember },
    DrainNode { member: ClusterMember },
    StopNode { member: ClusterMember },
    StartNode { member: ClusterMember },
    RecordVersion { version: KubernetesVersion },
    RecordShape,
    RefuseWhileHosting,
    DeleteResources,
}

impl PlannedStep for KubernetesStep {
    fn name(&self) -> &'static str {
        match self {
            Self::RecordCluster => "record-cluster",
            Self::PrepareNetworkAccess => "prepare-network-access",
            Self::ImportImage => "import-image",
            Self::BootstrapControlPlane { .. } => "bootstrap-control-plane",
            Self::AwaitApi => "await-api",
            Self::JoinNode { .. } => "join-node",
            Self::InstallAddOns => "install-add-ons",
            Self::MarkReady => "mark-ready",
            Self::RemoveNode { .. } => "remove-node",
            Self::DrainNode { .. } => "drain-node",
            Self::StopNode { .. } => "stop-node",
            Self::StartNode { .. } => "start-node",
            Self::RecordVersion { .. } => "record-version",
            Self::RecordShape => "record-shape",
            Self::RefuseWhileHosting => "refuse-while-hosting",
            Self::DeleteResources => "delete-resources",
        }
    }

    fn can_be_undone(&self) -> bool {
        matches!(self, Self::RefuseWhileHosting | Self::ImportImage)
    }

    fn can_be_abandoned(&self) -> bool {
        !matches!(self, Self::RemoveNode { .. } | Self::DeleteResources)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestedKubernetesClusterChangeSpecification {
    cluster: ObjectName,
    request: KubernetesRequest,
    #[serde(default)]
    steps: Option<Vec<KubernetesStep>>,
    #[serde(default)]
    is_paused: bool,
    #[serde(default)]
    is_cancelled: bool,
}

impl From<RequestedKubernetesClusterChangeSpecification> for KubernetesClusterChangeSpecification {
    fn from(requested: RequestedKubernetesClusterChangeSpecification) -> Self {
        let steps = requested.steps.unwrap_or_else(|| requested.request.steps());
        Self {
            cluster: requested.cluster,
            request: requested.request,
            steps,
            is_paused: requested.is_paused,
            is_cancelled: requested.is_cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RequestedKubernetesClusterChangeSpecification")]
pub struct KubernetesClusterChangeSpecification {
    pub cluster: ObjectName,
    pub request: KubernetesRequest,
    pub steps: Vec<KubernetesStep>,
    #[serde(default)]
    pub is_paused: bool,
    #[serde(default)]
    pub is_cancelled: bool,
}

impl KubernetesClusterChangeSpecification {
    #[must_use]
    pub fn new(cluster: ObjectName, request: KubernetesRequest) -> Self {
        let steps = request.steps();
        Self { cluster, request, steps, is_paused: false, is_cancelled: false }
    }
}

impl EnvironmentChange for KubernetesClusterChangeSpecification {
    fn environment(&self) -> &ObjectName {
        &self.cluster
    }

    fn creates_environment(&self) -> bool {
        matches!(self.request, KubernetesRequest::Create { .. })
    }
}

impl StepPlan for KubernetesClusterChangeSpecification {
    type Step = KubernetesStep;

    fn steps(&self) -> &[KubernetesStep] {
        &self.steps
    }

    fn is_paused(&self) -> bool {
        self.is_paused
    }

    fn is_cancelled(&self) -> bool {
        self.is_cancelled
    }
}

impl Validate for KubernetesClusterChangeSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.cluster.as_str().len() <= LONGEST_CLUSTER_NAME,
            "specification.cluster",
            "must be at most 40 characters",
        );
        error.check(
            self.steps == self.request.steps(),
            "specification.steps",
            "must be the steps of the request",
        );
        if let KubernetesRequest::Create { cluster } = &self.request {
            cluster.check("specification.cluster", &mut error);
        }
        if let KubernetesRequest::ScalePool { to, .. } = &self.request {
            error.check(*to <= MAXIMUM_POOL_NODES, "specification.to", "must be at most 100");
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.cluster == previous.cluster, "specification.cluster", "cannot change");
        error.check(self.request == previous.request, "specification.request", "cannot change");
        error.check(self.steps == previous.steps, "specification.steps", "cannot change");
        error.check(
            self.is_cancelled || !previous.is_cancelled,
            "specification.is_cancelled",
            "cannot be withdrawn",
        );
        error.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn cluster(control_plane_nodes: u8, workers: u16) -> KubernetesClusterSpecification {
        KubernetesClusterSpecification {
            version: KubernetesVersion::new(1, 34),
            control_plane_nodes,
            control_plane_shape: MachineShape::Medium,
            node_pools: BTreeMap::from([(
                name("general"),
                NodePool { nodes: workers, shape: MachineShape::Large },
            )]),
            network: name("platform"),
            storage_backend: name("shared"),
            external_network: name("public"),
            organization_sign_in: false,
        }
    }

    fn step_names(steps: &[KubernetesStep]) -> Vec<&'static str> {
        steps.iter().map(PlannedStep::name).collect()
    }

    #[test]
    fn a_change_written_without_steps_takes_the_steps_of_its_request() {
        let change: KubernetesClusterChangeSpecification =
            serde_json::from_value(serde_json::json!({"cluster": "platform", "request": {"kind": "delete"}}))
                .unwrap();
        assert_eq!(change.steps, KubernetesRequest::Delete.steps());
        let round_trip: KubernetesClusterChangeSpecification =
            serde_json::from_value(serde_json::to_value(&change).unwrap()).unwrap();
        assert_eq!(round_trip, change);
    }

    #[test]
    fn creation_bootstraps_one_control_plane_node_then_joins_the_rest() {
        let steps = KubernetesRequest::Create { cluster: cluster(3, 2) }.steps();
        assert_eq!(
            step_names(&steps),
            [
                "record-cluster",
                "prepare-network-access",
                "import-image",
                "bootstrap-control-plane",
                "await-api",
                "join-node",
                "join-node",
                "join-node",
                "join-node",
                "install-add-ons",
                "mark-ready"
            ]
        );
        assert_eq!(
            steps[3],
            KubernetesStep::BootstrapControlPlane { member: ClusterMember::control_plane(0) }
        );
    }

    #[test]
    fn an_upgrade_replaces_control_plane_nodes_first_and_one_at_a_time() {
        let nodes = vec![
            ClusterMember::worker(name("general"), 0),
            ClusterMember::control_plane(0),
            ClusterMember::control_plane(1),
        ];
        let steps = KubernetesRequest::Upgrade { version: KubernetesVersion::new(1, 35), nodes }.steps();
        assert_eq!(steps[1], KubernetesStep::JoinNode { member: ClusterMember::control_plane(2) });
        assert_eq!(steps[2], KubernetesStep::RemoveNode { member: ClusterMember::control_plane(0) });
        assert_eq!(steps[3], KubernetesStep::JoinNode { member: ClusterMember::control_plane(3) });
        assert_eq!(steps[5], KubernetesStep::JoinNode { member: ClusterMember::worker(name("general"), 1) });
        assert_eq!(
            steps.last(),
            Some(&KubernetesStep::RecordVersion { version: KubernetesVersion::new(1, 35) })
        );
    }

    #[test]
    fn scaling_down_removes_the_newest_nodes() {
        let nodes: Vec<ClusterMember> =
            (0..4).map(|index| ClusterMember::worker(name("general"), index)).collect();
        let steps = KubernetesRequest::ScalePool { pool: name("general"), nodes, to: 2 }.steps();
        assert_eq!(
            steps[0],
            KubernetesStep::RemoveNode { member: ClusterMember::worker(name("general"), 3) }
        );
        assert_eq!(
            steps[1],
            KubernetesStep::RemoveNode { member: ClusterMember::worker(name("general"), 2) }
        );
        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn scaling_up_joins_new_indexes_after_the_existing_ones() {
        let nodes =
            vec![ClusterMember::worker(name("general"), 0), ClusterMember::worker(name("general"), 4)];
        let steps = KubernetesRequest::ScalePool { pool: name("general"), nodes, to: 4 }.steps();
        assert_eq!(steps[0], KubernetesStep::JoinNode { member: ClusterMember::worker(name("general"), 5) });
        assert_eq!(steps[1], KubernetesStep::JoinNode { member: ClusterMember::worker(name("general"), 6) });
    }

    #[test]
    fn a_change_must_carry_the_steps_of_its_request_and_a_valid_cluster() {
        let mut change = KubernetesClusterChangeSpecification::new(
            name("platform"),
            KubernetesRequest::Create { cluster: cluster(2, 1) },
        );
        assert!(change.validate().is_err());
        change.request = KubernetesRequest::Create { cluster: cluster(3, 1) };
        assert!(change.validate().is_err());
        change.steps = change.request.steps();
        assert!(change.validate().is_ok());
    }

    #[test]
    fn a_change_survives_a_trip_through_json() {
        let change = KubernetesClusterChangeSpecification::new(
            name("platform"),
            KubernetesRequest::ScalePool { pool: name("general"), nodes: vec![], to: 2 },
        );
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["request"]["kind"], "scale-pool");
        assert_eq!(serde_json::from_value::<KubernetesClusterChangeSpecification>(json).unwrap(), change);
    }

    #[test]
    fn versions_read_and_print_as_major_dot_minor() {
        let version = KubernetesVersion::try_from(String::from("1.34")).unwrap();
        assert_eq!(version.to_string(), "1.34");
        assert_eq!(version.snap_channel(), "1.34-classic/stable");
        assert!(KubernetesVersion::try_from(String::from("v1")).is_err());
    }

    #[test]
    fn member_vm_names_follow_the_cluster_and_pool() {
        assert_eq!(ClusterMember::control_plane(2).vm_name(&name("platform")), "platform-cp-2");
        assert_eq!(ClusterMember::worker(name("gpu"), 0).vm_name(&name("platform")), "platform-gpu-0");
    }
}
