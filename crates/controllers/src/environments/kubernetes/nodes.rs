use std::sync::Arc;

use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::kinds::kubernetes_cluster::{
    ClusterMember, KubernetesCluster, KubernetesClusterChange, KubernetesNodeStatus,
};
use amafu_domain::kinds::vm::{PowerState, VmPhase};
use amafu_domain::{Object, ObjectName};
use amafu_interfaces::{ActionError, DrainState, NodeRole};
use amafu_reconciler::Step;

use super::KubernetesEnvironment;
use super::cloud_init::{self, ControlPlane};
use super::scope::{CA_CERTIFICATE, CA_KEY, ChangeContext, ClusterScope, K8SD_TOKEN};
use crate::environments::{CertificateAuthority, Machine};

fn role_of(member: &ClusterMember) -> NodeRole {
    if member.is_control_plane() { NodeRole::ControlPlane } else { NodeRole::Worker }
}

async fn record(
    scope: &ClusterScope<'_>,
    cluster: &Object<KubernetesCluster>,
    member: &ClusterMember,
    machine: &Machine<'_>,
) -> Result<(), ActionError> {
    let node = KubernetesNodeStatus {
        member: member.clone(),
        vm: machine.name().clone(),
        version: scope.version_for_new_nodes(cluster),
        is_ready: false,
    };
    scope.record_node(node).await
}

fn control_plane<'a>(
    scope: &'a ClusterScope<'a>,
    cluster: &'a Object<KubernetesCluster>,
    api_address: std::net::Ipv4Addr,
) -> Result<ControlPlane<'a>, ActionError> {
    let audience =
        cluster.status.uid.as_deref().ok_or_else(|| ActionError::retryable("the cluster has no UID yet"))?;
    Ok(ControlPlane { api_address, audience, trust: &scope.environment.settings.trust })
}

pub struct BootstrapControlPlane {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

impl BootstrapControlPlane {
    async fn points_api_at(scope: &ClusterScope<'_>, vm: &ObjectName) -> Result<bool, ActionError> {
        let reference = scope.ownership.reference::<FloatingIp>(scope.api_address_name()?);
        let address = scope.objects().require::<FloatingIp>(&reference).await?;
        Ok(address.specification.target.is_some_and(|target| &target.vm == vm))
    }
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for BootstrapControlPlane {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        Ok(machine.is_running().await? && Self::points_api_at(&scope, machine.name()).await?)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        let cluster = scope.cluster().await?;
        let Some(api_address) = scope.api_address().await? else {
            return Ok(());
        };
        if machine.vm().await?.is_none() {
            let secret = scope.secret(&cluster).await?;
            let missing = |key: &str| ActionError::fatal(format!("the cluster secret has no {key}"));
            let authority = CertificateAuthority {
                certificate_pem: secret
                    .value(CA_CERTIFICATE)
                    .ok_or_else(|| missing(CA_CERTIFICATE))?
                    .to_owned(),
                private_key_pem: secret.value(CA_KEY).ok_or_else(|| missing(CA_KEY))?.to_owned(),
            };
            let token = secret.value(K8SD_TOKEN).ok_or_else(|| missing(K8SD_TOKEN))?;
            let version = scope.version_for_new_nodes(&cluster);
            let user_data = cloud_init::bootstrap(
                &version,
                &control_plane(&scope, &cluster, api_address)?,
                &authority,
                token,
            );
            machine.create(scope.node_plan(&cluster, &self.member, user_data)?).await?;
            record(&scope, &cluster, &self.member, &machine).await?;
        }
        let target = FloatingIpTarget { guest_address: None, vm: machine.name().clone(), interface: 0 };
        let reference = scope.ownership.reference::<FloatingIp>(scope.api_address_name()?);
        scope
            .objects()
            .change_specification::<FloatingIp>(&reference, |current| {
                current
                    .target
                    .is_none()
                    .then(|| FloatingIpSpecification { target: Some(target), ..current.clone() })
            })
            .await
    }
}

pub struct JoinNode {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for JoinNode {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        if machine.vm().await?.is_none() {
            return Ok(false);
        }
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        self.environment.platform.is_node_ready(&endpoint, machine.name().as_str()).await
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        if machine.vm().await?.is_some() {
            return Ok(());
        }
        let cluster = scope.cluster().await?;
        let endpoint = scope.endpoint(&cluster).await?;
        let role = role_of(&self.member);
        let token = self.environment.platform.join_token(&endpoint, machine.name().as_str(), role).await?;
        let version = scope.version_for_new_nodes(&cluster);
        let control_plane = match role {
            NodeRole::ControlPlane => Some(control_plane(&scope, &cluster, endpoint.address)?),
            NodeRole::Worker => None,
        };
        let user_data = cloud_init::join(&version, &token, control_plane.as_ref());
        machine.create(scope.node_plan(&cluster, &self.member, user_data)?).await?;
        record(&scope, &cluster, &self.member, &machine).await
    }
}

pub struct RemoveNode {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

impl RemoveNode {
    async fn move_api_away(scope: &ClusterScope<'_>, machine: &Machine<'_>) -> Result<(), ActionError> {
        let cluster = scope.cluster().await?;
        let successor = cluster
            .status
            .nodes
            .iter()
            .find(|node| node.member.is_control_plane() && &node.vm != machine.name())
            .map(|node| node.vm.clone());
        let reference = scope.ownership.reference::<FloatingIp>(scope.api_address_name()?);
        scope
            .objects()
            .change_specification::<FloatingIp>(&reference, |current| {
                let points_here = current.target.as_ref().is_some_and(|target| &target.vm == machine.name());
                points_here.then(|| FloatingIpSpecification {
                    target: successor.clone().map(|vm| FloatingIpTarget {
                        guest_address: None,
                        vm,
                        interface: 0,
                    }),
                    ..current.clone()
                })
            })
            .await
    }

    async fn leave_kubernetes(
        &self,
        scope: &ClusterScope<'_>,
        machine: &Machine<'_>,
    ) -> Result<bool, ActionError> {
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        let platform = &self.environment.platform;
        let node = machine.name().as_str();
        if !platform.has_node(&endpoint, node).await? {
            return Ok(true);
        }
        if platform.drain_state(&endpoint, node).await? != DrainState::Drained {
            platform.drain(&endpoint, node).await?;
            return Ok(false);
        }
        platform.remove_node(&endpoint, node).await?;
        Ok(false)
    }
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for RemoveNode {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        let cluster = scope.cluster().await?;
        Ok(machine.vm().await?.is_none()
            && !cluster.status.nodes.iter().any(|node| node.member == self.member))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        if machine.vm().await?.is_some() {
            if !self.leave_kubernetes(&scope, &machine).await? {
                return Ok(());
            }
            Self::move_api_away(&scope, &machine).await?;
        }
        if machine.remove().await? {
            scope.forget_node(&self.member).await?;
        }
        Ok(())
    }
}

pub struct DrainNode {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for DrainNode {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        Ok(self.environment.platform.drain_state(&endpoint, machine.name().as_str()).await?
            == DrainState::Drained)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        self.environment.platform.drain(&endpoint, machine.name().as_str()).await
    }
}

pub struct StopNode {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for StopNode {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        Ok(machine.vm().await?.is_none_or(|vm| vm.status.phase == VmPhase::Stopped))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        scope.machine(&self.member)?.set_power(PowerState::Stopped).await
    }
}

pub struct StartNode {
    pub environment: Arc<KubernetesEnvironment>,
    pub member: ClusterMember,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for StartNode {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        if !machine.is_running().await? {
            return Ok(false);
        }
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        let platform = &self.environment.platform;
        let node = machine.name().as_str();
        Ok(platform.is_node_ready(&endpoint, node).await?
            && platform.drain_state(&endpoint, node).await? == DrainState::Schedulable)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let machine = scope.machine(&self.member)?;
        if !machine.is_running().await? {
            return machine.set_power(PowerState::Running).await;
        }
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        let platform = &self.environment.platform;
        if platform.is_node_ready(&endpoint, machine.name().as_str()).await? {
            platform.uncordon(&endpoint, machine.name().as_str()).await?;
        }
        Ok(())
    }
}
