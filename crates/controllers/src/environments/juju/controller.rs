use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::environment_secret::{EnvironmentSecret, EnvironmentSecretSpecification};
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::kinds::image::{Image, ImagePhase, ImageSource, ImageSpecification};
use amafu_domain::kinds::juju_controller::{
    HIGHLY_AVAILABLE_CONTROLLERS, JujuController, JujuControllerChange, JujuRequest, JujuVersion, Substrate,
};
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::kinds::vm::VmPhase;
use amafu_domain::{ObjectName, ObjectReference};
use amafu_interfaces::juju::JUJU_API_PORT;
use amafu_interfaces::{ActionError, LXD_PORT};
use amafu_reconciler::Step;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::JujuEnvironment;
use super::bootstrap::{self, BOOTSTRAP_LOG, Target};
use super::pods::{LXD_ADDRESS, POD_GROUP, Pods};
use super::scope::{
    ADMIN_PASSWORD, CA_CERTIFICATE, CA_KEY, CONTROLLER_MODEL, CREDENTIAL_SECRET, ChangeContext,
    ControllerScope, RUNNER_IMAGE, UBUNTU_IMAGE,
};

use crate::environments::{AuthorityKey, CertificateAuthority, ContainerPlan, Settings};

const RUNNER: &str = "runner";
const BOOTSTRAP_DEADLINE: time::Duration = time::Duration::minutes(60);
const JUJU_NAMESPACE: &str = "kube-system";

fn rule(direction: Direction, protocol: Protocol, ports: Option<u16>, remote: Remote) -> SecurityGroupRule {
    SecurityGroupRule {
        direction,
        protocol,
        ports: ports.map(|port| PortRange { first: port, last: port }),
        remote,
    }
}

fn service_account_manifests(controller: &ObjectName) -> [Value; 3] {
    let account = format!("juju-{controller}");
    [
        json!({"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": account, "namespace": JUJU_NAMESPACE}}),
        json!({
            "apiVersion": "rbac.authorization.k8s.io/v1",
            "kind": "ClusterRoleBinding",
            "metadata": {"name": account},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "cluster-admin"},
            "subjects": [{"kind": "ServiceAccount", "name": account, "namespace": JUJU_NAMESPACE}],
        }),
        token_secret(controller, &account),
    ]
}

pub fn token_secret(controller: &ObjectName, name: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "kubernetes.io/service-account-token",
        "metadata": {
            "name": name,
            "namespace": JUJU_NAMESPACE,
            "annotations": {"kubernetes.io/service-account.name": format!("juju-{controller}")},
        },
    })
}

pub fn decoded_token(secret: &Value) -> Option<String> {
    let encoded = secret["data"]["token"].as_str()?;
    String::from_utf8(STANDARD.decode(encoded).ok()?).ok()
}

pub fn removable_manifests(controller: &ObjectName) -> [Value; 3] {
    service_account_manifests(controller)
}

macro_rules! juju_step {
    ($name:ident) => {
        pub struct $name {
            pub environment: Arc<JujuEnvironment>,
        }
    };
}

juju_step!(RecordSpecification);
juju_step!(PrepareNetworkAccess);
juju_step!(CheckHostCluster);
juju_step!(ImportImages);
juju_step!(CreateClusterServiceAccount);
juju_step!(Bootstrap);
juju_step!(AwaitControllerService);
juju_step!(ExposeController);
juju_step!(RecordController);
juju_step!(EnableHa);
juju_step!(CleanUpBootstrap);
juju_step!(MarkReady);

fn random_password() -> Result<String, ActionError> {
    let mut bytes = [0_u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| ActionError::retryable("the system could not provide random bytes"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

async fn ensure_secret(
    scope: &ControllerScope<'_>,
    with_admin_password: bool,
) -> Result<EnvironmentSecretSpecification, ActionError> {
    let controller = scope.controller().await?;
    let reference = ControllerScope::secret_reference(&controller)?;
    if let Some(existing) = scope.objects().read::<EnvironmentSecret>(&reference).await? {
        return Ok(existing.specification);
    }
    let authority = CertificateAuthority::generate(
        &format!("{} Juju", scope.ownership.environment()),
        AuthorityKey::Ecdsa,
    )?;
    let mut values = BTreeMap::from([
        (CA_CERTIFICATE.to_owned(), authority.certificate_pem),
        (CA_KEY.to_owned(), authority.private_key_pem),
    ]);
    if with_admin_password {
        values.insert(ADMIN_PASSWORD.to_owned(), random_password()?);
    }
    let secret = EnvironmentSecretSpecification {
        project: scope.ownership.project().clone(),
        environment: scope.ownership.environment().clone(),
        values,
    };
    Ok(scope.objects().ensure::<EnvironmentSecret>(&reference, BTreeMap::new(), secret).await?.specification)
}

async fn record_authority(
    scope: &ControllerScope<'_>,
    secret: &EnvironmentSecretSpecification,
) -> Result<(), ActionError> {
    let authority = secret.value(CA_CERTIFICATE).map(str::to_owned);
    scope.update_status(|status| status.certificate_authority.clone_from(&authority)).await
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RecordSpecification {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.objects().read::<JujuController>(&scope.controller_reference()).await?.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let JujuRequest::Create { controller } = scope.request() else {
            return Err(ActionError::fatal("only a create change records a new controller"));
        };
        scope
            .objects()
            .ensure::<JujuController>(&scope.controller_reference(), BTreeMap::new(), controller.clone())
            .await?;
        Ok(())
    }
}

impl PrepareNetworkAccess {
    fn pod_group(
        scope: &ControllerScope<'_>,
    ) -> Result<(ObjectReference, SecurityGroupSpecification), ActionError> {
        let pods = scope.named(POD_GROUP)?;
        let rules = vec![
            rule(Direction::Ingress, Protocol::Tcp, Some(JUJU_API_PORT), Remote::Any),
            rule(Direction::Ingress, Protocol::Tcp, Some(LXD_PORT), Remote::Any),
            rule(Direction::Ingress, Protocol::Any, None, Remote::SecurityGroup { name: pods.clone() }),
            rule(Direction::Egress, Protocol::Any, None, Remote::Any),
        ];
        Ok((scope.ownership.reference::<SecurityGroup>(pods), SecurityGroupSpecification { rules }))
    }

    fn addresses(scope: &ControllerScope<'_>) -> Result<[ObjectReference; 2], ActionError> {
        let address = |suffix: &str| -> Result<ObjectReference, ActionError> {
            Ok(scope.ownership.reference::<FloatingIp>(scope.named(suffix)?))
        };
        Ok([address("api")?, address(LXD_ADDRESS)?])
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for PrepareNetworkAccess {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let (group, _) = Self::pod_group(&scope)?;
        if objects.read::<SecurityGroup>(&group).await?.is_none() {
            return Ok(false);
        }
        for address in Self::addresses(&scope)? {
            if objects.read::<FloatingIp>(&address).await?.is_none() {
                return Ok(false);
            }
        }
        Ok(scope.controller().await?.status.certificate_authority.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let labels = scope.ownership.labels();
        let (group, rules) = Self::pod_group(&scope)?;
        objects.ensure::<SecurityGroup>(&group, labels.clone(), rules).await?;
        let Substrate::Pods { external_network, .. } = scope.specification().await?.substrate else {
            return Err(ActionError::fatal("only a controller on pods has its own address"));
        };
        for address in Self::addresses(&scope)? {
            let floating = FloatingIpSpecification {
                external_network: external_network.clone(),
                address: None,
                target: None,
            };
            objects.ensure::<FloatingIp>(&address, labels.clone(), floating).await?;
        }
        let secret = ensure_secret(&scope, true).await?;
        record_authority(&scope, &secret).await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for CheckHostCluster {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let cluster = scope
            .host_cluster()
            .await?
            .ok_or_else(|| ActionError::fatal("the controller is not on Kubernetes"))?;
        if cluster.status.phase != EnvironmentPhase::Ready {
            return Err(ActionError::fatal(format!("cluster {} is not ready", cluster.metadata.name)));
        }
        Ok(scope.controller().await?.status.certificate_authority.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let secret = ensure_secret(&scope, false).await?;
        record_authority(&scope, &secret).await
    }
}

impl ImportImages {
    async fn image(scope: &ControllerScope<'_>) -> Result<ObjectReference, ActionError> {
        let name = match scope.specification().await?.substrate {
            Substrate::Pods { .. } => UBUNTU_IMAGE,
            Substrate::Kubernetes { .. } => RUNNER_IMAGE,
        };
        Ok(scope.ownership.reference::<Image>(ObjectName::new(name).map_err(ActionError::fatal)?))
    }

    async fn source(scope: &ControllerScope<'_>) -> Result<ImageSource, ActionError> {
        let settings = Settings::read(scope.objects()).await?;
        match scope.specification().await?.substrate {
            Substrate::Pods { .. } => settings.ubuntu_image(),
            Substrate::Kubernetes { .. } => {
                Ok(ImageSource::Container { reference: settings.juju_runner_image()? })
            }
        }
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for ImportImages {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let reference = Self::image(&scope).await?;
        match scope.objects().read::<Image>(&reference).await? {
            Some(image) if image.status.phase == ImagePhase::Failed => {
                Err(ActionError::fatal(format!("image {} failed to import", reference.name)))
            }
            Some(image) => Ok(image.status.phase == ImagePhase::Ready),
            None => Ok(false),
        }
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let reference = Self::image(&scope).await?;
        let specification = ImageSpecification { source: Self::source(&scope).await? };
        scope.objects().ensure::<Image>(&reference, BTreeMap::new(), specification).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for CreateClusterServiceAccount {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.secret().await?.value(CREDENTIAL_SECRET).is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let cluster = scope
            .host_cluster()
            .await?
            .ok_or_else(|| ActionError::fatal("the controller is not on Kubernetes"))?;
        let endpoint = scope.host_endpoint(&cluster).await?;
        let manifests = service_account_manifests(scope.ownership.environment());
        let kubernetes = &self.environment.kubernetes;
        kubernetes.apply(&endpoint, &manifests).await?;
        let Some(secret) = kubernetes.read(&endpoint, &manifests[2]).await? else {
            return Ok(());
        };
        if decoded_token(&secret).is_some() {
            let name = manifests[2]["metadata"]["name"].as_str().unwrap_or_default().to_owned();
            scope.remember(CREDENTIAL_SECRET, name).await?;
            let now = context.clock.now();
            scope.update_status(|status| status.credential_rotated_at = Some(now)).await?;
        }
        Ok(())
    }
}

impl Bootstrap {
    async fn cluster_target(
        &self,
        scope: &ControllerScope<'_>,
        cluster: &amafu_domain::Object<KubernetesCluster>,
    ) -> Result<Target, ActionError> {
        let endpoint = scope.host_endpoint(cluster).await?;
        let secret_name = scope.secret_value(CREDENTIAL_SECRET).await?;
        let manifest = token_secret(scope.ownership.environment(), &secret_name);
        let secret = self.environment.kubernetes.read(&endpoint, &manifest).await?;
        let token = secret
            .as_ref()
            .and_then(decoded_token)
            .ok_or_else(|| ActionError::retryable("the Juju service account has no token yet"))?;
        let user = format!("juju-{}", scope.ownership.environment());
        Ok(Target::Cluster {
            kubeconfig: bootstrap::kubeconfig(
                endpoint.address,
                &endpoint.certificate_authority,
                &user,
                &token,
            ),
        })
    }
}

fn bootstrap_overdue(scope: &ControllerScope<'_>, waiting_for: &str) -> Result<ActionError, ActionError> {
    Ok(ActionError::fatal(format!(
        "{waiting_for} within {} minutes of starting the bootstrap; pod {} keeps the log in {BOOTSTRAP_LOG}",
        BOOTSTRAP_DEADLINE.whole_minutes(),
        Pods::of(scope).name(0)?
    )))
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for Bootstrap {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        if matches!(scope.specification().await?.substrate, Substrate::Pods { .. }) {
            if Pods::of(&scope).controller_guest().await?.is_some() {
                return Ok(true);
            }
            if scope.step_is_overdue(BOOTSTRAP_DEADLINE) {
                return Err(bootstrap_overdue(&scope, "no Juju controller container appeared")?);
            }
            return Ok(false);
        }
        match scope.machine(RUNNER)?.phase().await? {
            Some(VmPhase::Stopped) => Ok(true),
            Some(VmPhase::Failed) => {
                Err(ActionError::fatal("the Juju bootstrap runner failed; see its console"))
            }
            _ => Ok(false),
        }
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let runner = scope.machine(RUNNER)?;
        let Some(cluster) = scope.host_cluster().await? else {
            return Ok(());
        };
        if runner.vm().await?.is_some() {
            return Ok(());
        }
        let target = self.cluster_target(&scope, &cluster).await?;
        let plan = scope.bootstrap_plan(target).await?;
        runner
            .run_container(ContainerPlan {
                image: ObjectName::new(RUNNER_IMAGE).map_err(ActionError::fatal)?,
                environment: plan.environment(),
                network: cluster.specification.network.clone(),
                security_groups: Vec::new(),
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for AwaitControllerService {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(!scope.controller().await?.status.addresses.is_empty())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let cluster = scope
            .host_cluster()
            .await?
            .ok_or_else(|| ActionError::fatal("the controller is not on Kubernetes"))?;
        let endpoint = scope.host_endpoint(&cluster).await?;
        let service = json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "controller-service", "namespace": format!("controller-{}", scope.ownership.environment())},
        });
        let Some(found) = self.environment.kubernetes.read(&endpoint, &service).await? else {
            return Ok(());
        };
        let Some(address) = found["status"]["loadBalancer"]["ingress"][0]["ip"].as_str().map(str::to_owned)
        else {
            return Ok(());
        };
        scope.update_status(|status| status.addresses = vec![address.clone()]).await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for ExposeController {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        let target = scope.objects().require::<FloatingIp>(&address).await?.specification.target;
        if target.is_none_or(|target| target.guest_address.is_none()) {
            return Ok(false);
        }
        if let Ok(Some(_)) = self.environment.platform.controller(&scope.endpoint().await?).await {
            return Ok(true);
        }
        if scope.step_is_overdue(BOOTSTRAP_DEADLINE) {
            return Err(bootstrap_overdue(
                &scope,
                "the Juju controller did not answer through its floating IP",
            )?);
        }
        Ok(false)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let guest = Pods::of(&scope)
            .controller_guest()
            .await?
            .ok_or_else(|| ActionError::retryable("the Juju controller container has no address yet"))?;
        let target = FloatingIpTarget { vm: guest.pod, interface: 0, guest_address: Some(guest.address) };
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        scope
            .objects()
            .change_specification::<FloatingIp>(&address, |current| {
                (current.target.as_ref() != Some(&target))
                    .then(|| FloatingIpSpecification { target: Some(target.clone()), ..current.clone() })
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RecordController {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let controller = scope.controller().await?;
        if controller.status.uuid.is_none() {
            return Ok(false);
        }
        let Some(cluster) = scope.host_cluster().await? else {
            return Ok(true);
        };
        Ok(cluster.status.hosted_juju_controllers.contains(scope.ownership.environment()))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let Some(found) = self.environment.platform.controller(&endpoint).await? else {
            return Ok(());
        };
        let version = JujuVersion::try_from(found.version.clone()).ok();
        let address = endpoint.address.to_string();
        scope
            .update_status(|status| {
                status.uuid = Some(found.uuid.clone());
                status.version.clone_from(&version);
                if status.addresses.is_empty() {
                    status.addresses = vec![address.clone()];
                }
            })
            .await?;
        if let Some(cluster) = scope.host_cluster().await? {
            let name = scope.ownership.environment().clone();
            scope
                .objects()
                .change_status::<KubernetesCluster>(&cluster.reference(), |cluster| {
                    (!cluster.status.hosted_juju_controllers.contains(&name)).then(|| {
                        let mut status = cluster.status.clone();
                        status.hosted_juju_controllers.push(name.clone());
                        status
                    })
                })
                .await?;
        }
        Ok(())
    }
}

async fn started_controllers(scope: &ControllerScope<'_>) -> Result<usize, ActionError> {
    let endpoint = scope.endpoint().await?;
    let model = scope.model_uuid(&ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?).await?;
    let machines = scope.environment.platform.machines(&endpoint, &model).await?;
    Ok(machines
        .iter()
        .filter(|machine| {
            machine.is_started && machine.units.iter().any(|unit| unit.starts_with("controller/"))
        })
        .count())
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for EnableHa {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(started_controllers(&scope).await? >= usize::from(HIGHLY_AVAILABLE_CONTROLLERS))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let pods = Pods::of(&scope).wanted().await?;
        if pods < HIGHLY_AVAILABLE_CONTROLLERS {
            return Err(ActionError::fatal(format!(
                "a highly available controller needs {HIGHLY_AVAILABLE_CONTROLLERS} pods; scale the pods first"
            )));
        }
        self.environment.platform.enable_ha(&scope.endpoint().await?, HIGHLY_AVAILABLE_CONTROLLERS).await?;
        scope
            .objects()
            .change_specification::<JujuController>(&scope.controller_reference(), |current| {
                let mut updated = current.clone();
                if let Substrate::Pods { controllers, .. } = &mut updated.substrate {
                    *controllers = HIGHLY_AVAILABLE_CONTROLLERS;
                }
                (updated != *current).then_some(updated)
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for CleanUpBootstrap {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.machine(RUNNER)?.vm().await?.is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        scope.machine(RUNNER)?.remove().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for MarkReady {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.controller().await?.status.phase == EnvironmentPhase::Ready)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        scope
            .update_status(|status| {
                status.phase = EnvironmentPhase::Ready;
                status.message = None;
            })
            .await
    }
}
