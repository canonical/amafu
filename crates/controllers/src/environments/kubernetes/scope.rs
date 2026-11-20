use std::net::Ipv4Addr;

use amafu_domain::kinds::environment_secret::{EnvironmentSecret, EnvironmentSecretSpecification};
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::kubernetes_cluster::{
    ClusterMember, KubernetesCluster, KubernetesClusterChange, KubernetesClusterSpecification,
    KubernetesNodeStatus, KubernetesRequest, KubernetesVersion,
};
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::labels::KUBERNETES_CLUSTER;
use amafu_domain::platform_access::{ISSUER, SERVICE_SUBJECT};
use amafu_domain::{Object, ObjectName, ObjectReference};
use amafu_interfaces::{ActionError, KubernetesEndpoint};
use amafu_reconciler::OperationContext;
use amafu_token_issuer::Claims;
use time::Duration;

use super::KubernetesEnvironment;
use crate::environments::{EnvironmentObjects, Machine, MachinePlan, Ownership};

pub const UBUNTU_IMAGE: &str = "ubuntu-24-04";
pub const K8SD_TOKEN: &str = "k8sd-token";
pub const CA_CERTIFICATE: &str = "ca-crt";
pub const CA_KEY: &str = "ca-key";

const CONTROLLER_TOKEN_LIFETIME: Duration = Duration::minutes(5);

pub type ChangeContext<'a> = OperationContext<'a, KubernetesClusterChange>;

pub struct ClusterScope<'a> {
    pub context: &'a ChangeContext<'a>,
    pub environment: &'a KubernetesEnvironment,
    pub ownership: Ownership,
}

impl<'a> ClusterScope<'a> {
    pub fn new(
        context: &'a ChangeContext<'a>,
        environment: &'a KubernetesEnvironment,
    ) -> Result<Self, ActionError> {
        let project = context
            .operation
            .metadata
            .project
            .clone()
            .ok_or_else(|| ActionError::fatal("a Kubernetes change belongs to a project"))?;
        let cluster = context.operation.specification.cluster.clone();
        Ok(Self { context, environment, ownership: Ownership::new(KUBERNETES_CLUSTER, cluster, project) })
    }

    #[must_use]
    pub fn objects(&self) -> EnvironmentObjects<'_> {
        EnvironmentObjects {
            store: self.context.store,
            fence: self.context.fence,
            identifiers: self.environment.identifiers.as_ref(),
        }
    }

    pub fn machine(&self, member: &ClusterMember) -> Result<Machine<'_>, ActionError> {
        let name =
            ObjectName::new(member.vm_name(self.ownership.environment())).map_err(ActionError::fatal)?;
        Ok(Machine::new(self.objects(), &self.ownership, name))
    }

    pub fn node_plan(
        &self,
        cluster: &Object<KubernetesCluster>,
        member: &ClusterMember,
        user_data: String,
    ) -> Result<MachinePlan, ActionError> {
        Ok(MachinePlan {
            shape: self.shape_of(cluster, member),
            image: ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?,
            network: cluster.specification.network.clone(),
            storage_backend: cluster.specification.storage_backend.clone(),
            security_groups: vec![self.security_group()?],
            user_data,
            labels: std::collections::BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn request(&self) -> &KubernetesRequest {
        &self.context.operation.specification.request
    }

    #[must_use]
    pub fn cluster_reference(&self) -> ObjectReference {
        self.ownership.reference::<KubernetesCluster>(self.ownership.environment().clone())
    }

    pub async fn cluster(&self) -> Result<Object<KubernetesCluster>, ActionError> {
        self.objects().require::<KubernetesCluster>(&self.cluster_reference()).await
    }

    pub fn security_group(&self) -> Result<ObjectName, ActionError> {
        self.ownership.named("nodes")
    }

    pub fn api_address_name(&self) -> Result<ObjectName, ActionError> {
        self.ownership.named("api")
    }

    pub fn secret_reference(cluster: &Object<KubernetesCluster>) -> Result<ObjectReference, ActionError> {
        let name =
            ObjectName::new(format!("kubernetes-{}", cluster.metadata.id)).map_err(ActionError::fatal)?;
        Ok(ObjectReference::of::<EnvironmentSecret>(None, name))
    }

    pub async fn secret(
        &self,
        cluster: &Object<KubernetesCluster>,
    ) -> Result<EnvironmentSecretSpecification, ActionError> {
        let reference = Self::secret_reference(cluster)?;
        Ok(self.objects().require::<EnvironmentSecret>(&reference).await?.specification)
    }

    pub async fn api_address(&self) -> Result<Option<Ipv4Addr>, ActionError> {
        let reference = self.ownership.reference::<FloatingIp>(self.api_address_name()?);
        Ok(self.objects().read::<FloatingIp>(&reference).await?.and_then(|address| address.status.address))
    }

    pub async fn endpoint(
        &self,
        cluster: &Object<KubernetesCluster>,
    ) -> Result<KubernetesEndpoint, ActionError> {
        let waiting = |what: &str| ActionError::retryable(format!("the cluster has no {what} yet"));
        let address = self.api_address().await?.ok_or_else(|| waiting("API address"))?;
        let uid = cluster.status.uid.as_deref().ok_or_else(|| waiting("UID"))?;
        let certificate_authority =
            cluster.status.certificate_authority.clone().ok_or_else(|| waiting("certificate authority"))?;
        let secret = self.secret(cluster).await?;
        let k8sd_token = secret.value(K8SD_TOKEN).ok_or_else(|| waiting("k8sd token"))?.to_owned();
        let now = self.context.clock.now();
        let claims = Claims::new(ISSUER, SERVICE_SUBJECT, uid, now, CONTROLLER_TOKEN_LIFETIME);
        let issued = self.environment.keyring.sign(&claims, now).await.map_err(ActionError::retryable)?;
        Ok(KubernetesEndpoint { address, certificate_authority, k8sd_token, api_token: issued.token })
    }

    #[must_use]
    pub fn version_for_new_nodes(&self, cluster: &Object<KubernetesCluster>) -> KubernetesVersion {
        match self.request() {
            KubernetesRequest::Create { cluster } => cluster.version.clone(),
            KubernetesRequest::Upgrade { version, .. } => version.clone(),
            _ => cluster.specification.version.clone(),
        }
    }

    #[must_use]
    pub fn shape_of(&self, cluster: &Object<KubernetesCluster>, member: &ClusterMember) -> MachineShape {
        match self.request() {
            KubernetesRequest::AddPool { pool, node_pool } if member.pool.as_ref() == Some(pool) => {
                node_pool.shape
            }
            _ => cluster.specification.shape_of(member),
        }
    }

    pub async fn record_node(&self, node: KubernetesNodeStatus) -> Result<(), ActionError> {
        self.objects()
            .change_status::<KubernetesCluster>(&self.cluster_reference(), |cluster| {
                let mut status = cluster.status.clone();
                status.nodes.retain(|existing| existing.member != node.member);
                status.nodes.push(node.clone());
                status.nodes.sort_by(|left, right| left.member.cmp(&right.member));
                Some(status)
            })
            .await
    }

    pub async fn forget_node(&self, member: &ClusterMember) -> Result<(), ActionError> {
        self.objects()
            .change_status::<KubernetesCluster>(&self.cluster_reference(), |cluster| {
                cluster.status.nodes.iter().any(|node| &node.member == member).then(|| {
                    let mut status = cluster.status.clone();
                    status.nodes.retain(|node| &node.member != member);
                    status
                })
            })
            .await
    }
}

#[must_use]
pub fn reshaped(
    current: &KubernetesClusterSpecification,
    request: &KubernetesRequest,
) -> KubernetesClusterSpecification {
    let mut specification = current.clone();
    match request {
        KubernetesRequest::ScalePool { pool, to, .. } => {
            if let Some(node_pool) = specification.node_pools.get_mut(pool) {
                node_pool.nodes = *to;
            }
        }
        KubernetesRequest::AddPool { pool, node_pool } => {
            specification.node_pools.insert(pool.clone(), *node_pool);
        }
        KubernetesRequest::RemovePool { pool, .. } => {
            specification.node_pools.remove(pool);
        }
        KubernetesRequest::Upgrade { version, .. } => specification.version = version.clone(),
        KubernetesRequest::Create { .. }
        | KubernetesRequest::Delete
        | KubernetesRequest::DrainNode { .. }
        | KubernetesRequest::RestartNode { .. } => {}
    }
    specification
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amafu_domain::kinds::kubernetes_cluster::NodePool;

    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn cluster() -> KubernetesClusterSpecification {
        KubernetesClusterSpecification {
            version: KubernetesVersion::new(1, 34),
            control_plane_nodes: 1,
            control_plane_shape: MachineShape::Medium,
            node_pools: BTreeMap::from([(
                name("general"),
                NodePool { nodes: 2, shape: MachineShape::Large },
            )]),
            network: name("platform"),
            storage_backend: name("shared"),
            external_network: name("public"),
            organization_sign_in: false,
        }
    }

    #[test]
    fn the_recorded_shape_follows_the_request() {
        let scaled = reshaped(
            &cluster(),
            &KubernetesRequest::ScalePool { pool: name("general"), nodes: vec![], to: 5 },
        );
        assert_eq!(scaled.node_pools[&name("general")].nodes, 5);
        let added = reshaped(
            &cluster(),
            &KubernetesRequest::AddPool {
                pool: name("gpu"),
                node_pool: NodePool { nodes: 1, shape: MachineShape::Xlarge },
            },
        );
        assert_eq!(added.node_pools.len(), 2);
        let removed =
            reshaped(&cluster(), &KubernetesRequest::RemovePool { pool: name("general"), nodes: vec![] });
        assert!(removed.node_pools.is_empty());
        let upgraded = reshaped(
            &cluster(),
            &KubernetesRequest::Upgrade { version: KubernetesVersion::new(1, 35), nodes: vec![] },
        );
        assert_eq!(upgraded.version, KubernetesVersion::new(1, 35));
    }
}
