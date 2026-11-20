use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::environment_secret::{EnvironmentSecret, EnvironmentSecretSpecification};
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification};
use amafu_domain::kinds::image::{Image, ImagePhase, ImageSpecification};
use amafu_domain::kinds::kubernetes_cluster::{
    KubernetesCluster, KubernetesClusterChange, KubernetesClusterStatus, KubernetesRequest, KubernetesVersion,
};
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Object, ObjectName};
use amafu_interfaces::kubernetes::{K8SD_PORT, KUBERNETES_API_PORT};
use amafu_interfaces::{ActionError, Identifiers};
use amafu_reconciler::Step;

use super::KubernetesEnvironment;
use super::add_ons;
use super::scope::{CA_CERTIFICATE, CA_KEY, ChangeContext, ClusterScope, K8SD_TOKEN, UBUNTU_IMAGE, reshaped};
use crate::environments::CertificateAuthority;

fn with_status(
    cluster: &Object<KubernetesCluster>,
    change: impl FnOnce(&mut KubernetesClusterStatus),
) -> Option<KubernetesClusterStatus> {
    let mut status = cluster.status.clone();
    change(&mut status);
    (status != cluster.status).then_some(status)
}

fn ready_nodes(status: &mut KubernetesClusterStatus) {
    status.phase = EnvironmentPhase::Ready;
    status.message = None;
    for node in &mut status.nodes {
        node.is_ready = true;
    }
}

pub struct RecordCluster {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for RecordCluster {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        Ok(scope.objects().read::<KubernetesCluster>(&scope.cluster_reference()).await?.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let KubernetesRequest::Create { cluster } = scope.request() else {
            return Err(ActionError::fatal("only a create change records a new cluster"));
        };
        scope
            .objects()
            .ensure::<KubernetesCluster>(&scope.cluster_reference(), BTreeMap::new(), cluster.clone())
            .await?;
        Ok(())
    }
}

pub struct PrepareNetworkAccess {
    pub environment: Arc<KubernetesEnvironment>,
}

impl PrepareNetworkAccess {
    fn node_rules(security_group: &ObjectName) -> SecurityGroupSpecification {
        let from_anywhere = |port: u16| SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            ports: Some(PortRange { first: port, last: port }),
            remote: Remote::Any,
        };
        SecurityGroupSpecification {
            rules: vec![
                from_anywhere(KUBERNETES_API_PORT),
                from_anywhere(K8SD_PORT),
                SecurityGroupRule {
                    direction: Direction::Ingress,
                    protocol: Protocol::Any,
                    ports: None,
                    remote: Remote::SecurityGroup { name: security_group.clone() },
                },
                SecurityGroupRule {
                    direction: Direction::Egress,
                    protocol: Protocol::Any,
                    ports: None,
                    remote: Remote::Any,
                },
            ],
        }
    }

    fn secret(
        scope: &ClusterScope<'_>,
        identifiers: &dyn Identifiers,
    ) -> Result<EnvironmentSecretSpecification, ActionError> {
        let authority =
            CertificateAuthority::generate(&format!("{} Kubernetes", scope.ownership.environment()))?;
        let token = format!("{}{}", identifiers.generate().simple(), identifiers.generate().simple());
        Ok(EnvironmentSecretSpecification {
            project: scope.ownership.project().clone(),
            environment: scope.ownership.environment().clone(),
            values: BTreeMap::from([
                (K8SD_TOKEN.to_owned(), token),
                (CA_CERTIFICATE.to_owned(), authority.certificate_pem),
                (CA_KEY.to_owned(), authority.private_key_pem),
            ]),
        })
    }
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for PrepareNetworkAccess {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let cluster = scope.cluster().await?;
        let group = scope.ownership.reference::<SecurityGroup>(scope.security_group()?);
        let address = scope.ownership.reference::<FloatingIp>(scope.api_address_name()?);
        Ok(objects.read::<SecurityGroup>(&group).await?.is_some()
            && objects.read::<FloatingIp>(&address).await?.is_some()
            && cluster.status.uid.is_some()
            && cluster.status.certificate_authority.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let labels = scope.ownership.labels();
        let cluster = scope.cluster().await?;
        let group_name = scope.security_group()?;
        let group = scope.ownership.reference::<SecurityGroup>(group_name.clone());
        objects.ensure::<SecurityGroup>(&group, labels.clone(), Self::node_rules(&group_name)).await?;
        let address = scope.ownership.reference::<FloatingIp>(scope.api_address_name()?);
        let floating = FloatingIpSpecification {
            external_network: cluster.specification.external_network.clone(),
            address: None,
            target: None,
        };
        objects.ensure::<FloatingIp>(&address, labels, floating).await?;
        let secret_reference = ClusterScope::secret_reference(&cluster)?;
        let secret = if let Some(secret) = objects.read::<EnvironmentSecret>(&secret_reference).await? {
            secret.specification
        } else {
            let secret = Self::secret(&scope, self.environment.identifiers.as_ref())?;
            objects
                .ensure::<EnvironmentSecret>(&secret_reference, BTreeMap::new(), secret)
                .await?
                .specification
        };
        let uid = self.environment.identifiers.generate().to_string();
        let certificate_authority = secret.value(CA_CERTIFICATE).map(str::to_owned);
        objects
            .change_status::<KubernetesCluster>(&scope.cluster_reference(), |cluster| {
                with_status(cluster, |status| {
                    status.uid.get_or_insert_with(|| uid.clone());
                    status.certificate_authority.clone_from(&certificate_authority);
                })
            })
            .await
    }
}

pub struct ImportImage {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for ImportImage {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let name = ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?;
        match scope.objects().read::<Image>(&scope.ownership.reference::<Image>(name)).await? {
            Some(image) if image.status.phase == ImagePhase::Failed => Err(ActionError::fatal(format!(
                "the Ubuntu image failed to import: {}",
                image.status.message.unwrap_or_default()
            ))),
            Some(image) => Ok(image.status.phase == ImagePhase::Ready),
            None => Ok(false),
        }
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let source = self
            .environment
            .settings
            .ubuntu_image
            .clone()
            .ok_or_else(|| ActionError::fatal("no Ubuntu image source is configured for environments"))?;
        let name = ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?;
        let reference = scope.ownership.reference::<Image>(name);
        scope.objects().ensure::<Image>(&reference, BTreeMap::new(), ImageSpecification { source }).await?;
        Ok(())
    }
}

pub struct AwaitApi {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for AwaitApi {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        self.environment.platform.is_api_ready(&endpoint).await
    }

    async fn run(&self, _context: &ChangeContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct InstallAddOns {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for InstallAddOns {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        self.environment.platform.is_applied(&endpoint, &add_ons::manifests(scope.ownership.project())).await
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint(&scope.cluster().await?).await?;
        self.environment.platform.apply(&endpoint, &add_ons::manifests(scope.ownership.project())).await
    }
}

pub struct MarkReady {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for MarkReady {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        Ok(scope.cluster().await?.status.phase == EnvironmentPhase::Ready)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let endpoint = scope.api_address().await?;
        scope
            .objects()
            .change_status::<KubernetesCluster>(&scope.cluster_reference(), |cluster| {
                with_status(cluster, |status| {
                    status.endpoint = endpoint;
                    ready_nodes(status);
                })
            })
            .await
    }
}

pub struct RecordVersion {
    pub environment: Arc<KubernetesEnvironment>,
    pub version: KubernetesVersion,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for RecordVersion {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        Ok(scope.cluster().await?.specification.version == self.version)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        RecordShape::record(&ClusterScope::new(context, &self.environment)?).await
    }
}

pub struct RecordShape {
    pub environment: Arc<KubernetesEnvironment>,
}

impl RecordShape {
    async fn record(scope: &ClusterScope<'_>) -> Result<(), ActionError> {
        let objects = scope.objects();
        let reference = scope.cluster_reference();
        objects
            .change_specification::<KubernetesCluster>(&reference, |current| {
                let target = reshaped(current, scope.request());
                (&target != current).then_some(target)
            })
            .await?;
        objects
            .change_status::<KubernetesCluster>(&reference, |cluster| with_status(cluster, ready_nodes))
            .await
    }
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for RecordShape {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let cluster = scope.cluster().await?;
        Ok(reshaped(&cluster.specification, scope.request()) == cluster.specification)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        Self::record(&ClusterScope::new(context, &self.environment)?).await
    }
}

pub struct RefuseWhileHosting {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for RefuseWhileHosting {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let hosted = scope.cluster().await?.status.hosted_juju_controllers;
        if hosted.is_empty() {
            return Ok(true);
        }
        let names: Vec<String> = hosted.iter().map(ToString::to_string).collect();
        Err(ActionError::fatal(format!("the cluster hosts the Juju controllers {}", names.join(", "))))
    }

    async fn run(&self, _context: &ChangeContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct DeleteResources {
    pub environment: Arc<KubernetesEnvironment>,
}

#[async_trait::async_trait]
impl Step<KubernetesClusterChange> for DeleteResources {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        Ok(scope.objects().read::<KubernetesCluster>(&scope.cluster_reference()).await?.is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ClusterScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let reference = scope.cluster_reference();
        let Some(cluster) = objects.read::<KubernetesCluster>(&reference).await? else {
            return Ok(());
        };
        objects
            .change_status::<KubernetesCluster>(&reference, |cluster| {
                with_status(cluster, |status| status.phase = EnvironmentPhase::Deleting)
            })
            .await?;
        let ownership = &scope.ownership;
        if !objects.delete_owned::<Vm>(ownership).await? {
            return Ok(());
        }
        let volumes_gone = objects.delete_owned::<Volume>(ownership).await?;
        let addresses_gone = objects.delete_owned::<FloatingIp>(ownership).await?;
        let groups_gone = objects.delete_owned::<SecurityGroup>(ownership).await?;
        if volumes_gone && addresses_gone && groups_gone {
            objects.erase::<EnvironmentSecret>(&ClusterScope::secret_reference(&cluster)?).await?;
            objects.erase::<KubernetesCluster>(&reference).await?;
        }
        Ok(())
    }
}
