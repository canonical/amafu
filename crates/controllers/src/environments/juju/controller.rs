use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::environment_secret::{EnvironmentSecret, EnvironmentSecretSpecification};
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::kinds::image::{Image, ImagePhase, ImageSource, ImageSpecification};
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuRequest, JujuVersion, Substrate,
};
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::kinds::vm::VmPhase;
use amafu_domain::{ObjectName, ObjectReference};
use amafu_interfaces::ActionError;
use amafu_interfaces::juju::JUJU_API_PORT;
use amafu_reconciler::Step;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::JujuEnvironment;
use super::bootstrap::{self, BOOTSTRAP_KEY_COMMENT, BootstrapPlan, RUNNER_LOG, Target};
use super::provisioning::ProvisionedMachine;
use super::scope::{
    CA_CERTIFICATE, CA_KEY, CONTROLLER_MODEL, CREDENTIAL_SECRET, ChangeContext, ControllerScope,
    RUNNER_IMAGE, SSH_PRIVATE_KEY, SSH_PUBLIC_KEY, UBUNTU_IMAGE, substrate_network,
};
use amafu_ssh_key_pair::SshKeyPair;

use crate::environments::{AuthorityKey, CertificateAuthority, ContainerPlan, MachinePlan, Settings};

const RUNNER: &str = "runner";
const BOOTSTRAP_DEADLINE: time::Duration = time::Duration::minutes(45);
const FIRST_CONTROLLER: &str = "controller-0";
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
juju_step!(CreateControllerMachines);
juju_step!(CreateClusterServiceAccount);
juju_step!(Bootstrap);
juju_step!(AwaitControllerService);
juju_step!(RecordController);
juju_step!(AddControllerMachines);
juju_step!(EnableHa);
juju_step!(CleanUpBootstrap);
juju_step!(MarkReady);

async fn ensure_secret(
    scope: &ControllerScope<'_>,
    with_ssh: bool,
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
    if with_ssh {
        let keys = SshKeyPair::generate(BOOTSTRAP_KEY_COMMENT).map_err(ActionError::fatal)?;
        values.insert(SSH_PRIVATE_KEY.to_owned(), keys.private_key);
        values.insert(SSH_PUBLIC_KEY.to_owned(), keys.public_key);
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
    fn groups(
        scope: &ControllerScope<'_>,
    ) -> Result<[(ObjectName, SecurityGroupSpecification); 2], ActionError> {
        let controller = scope.named("controller")?;
        let machines = scope.named("machines")?;
        let from = |group: &ObjectName| Remote::SecurityGroup { name: group.clone() };
        let egress = rule(Direction::Egress, Protocol::Any, None, Remote::Any);
        Ok([
            (
                controller.clone(),
                SecurityGroupSpecification {
                    rules: vec![
                        rule(Direction::Ingress, Protocol::Tcp, Some(JUJU_API_PORT), Remote::Any),
                        rule(Direction::Ingress, Protocol::Any, None, from(&controller)),
                        rule(Direction::Ingress, Protocol::Any, None, from(&machines)),
                        egress.clone(),
                    ],
                },
            ),
            (
                machines.clone(),
                SecurityGroupSpecification {
                    rules: vec![
                        rule(Direction::Ingress, Protocol::Any, None, from(&machines)),
                        rule(Direction::Ingress, Protocol::Any, None, from(&controller)),
                        egress,
                    ],
                },
            ),
        ])
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for PrepareNetworkAccess {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        for (group, _) in Self::groups(&scope)? {
            if objects
                .read::<SecurityGroup>(&scope.ownership.reference::<SecurityGroup>(group))
                .await?
                .is_none()
            {
                return Ok(false);
            }
        }
        Ok(objects.read::<FloatingIp>(&address).await?.is_some()
            && scope.controller().await?.status.certificate_authority.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let labels = scope.ownership.labels();
        for (group, rules) in Self::groups(&scope)? {
            objects
                .ensure::<SecurityGroup>(
                    &scope.ownership.reference::<SecurityGroup>(group),
                    labels.clone(),
                    rules,
                )
                .await?;
        }
        let Substrate::Vms { external_network, .. } = scope.specification().await?.substrate else {
            return Err(ActionError::fatal("only a controller on VMs has its own address"));
        };
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        let floating = FloatingIpSpecification { external_network, address: None, target: None };
        objects.ensure::<FloatingIp>(&address, labels, floating).await?;
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
            Substrate::Vms { .. } => UBUNTU_IMAGE,
            Substrate::Kubernetes { .. } => RUNNER_IMAGE,
        };
        Ok(scope.ownership.reference::<Image>(ObjectName::new(name).map_err(ActionError::fatal)?))
    }

    async fn source(scope: &ControllerScope<'_>) -> Result<ImageSource, ActionError> {
        let settings = Settings::read(scope.objects()).await?;
        match scope.specification().await?.substrate {
            Substrate::Vms { .. } => settings.ubuntu_image(),
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
impl Step<JujuControllerChange> for CreateControllerMachines {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machine = scope.machine(FIRST_CONTROLLER)?;
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        let target = scope.objects().require::<FloatingIp>(&address).await?.specification.target;
        Ok(machine.is_running().await? && target.is_some_and(|target| &target.vm == machine.name()))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let specification = scope.specification().await?;
        let Substrate::Vms { shape, .. } = &specification.substrate else {
            return Err(ActionError::fatal("only a controller on VMs has controller machines"));
        };
        let (network, storage_backend) = substrate_network(&specification)?;
        let user_data = bootstrap::controller_user_data(
            &scope.secret_value(SSH_PUBLIC_KEY).await?,
            &self.environment.settings.amafu_certificate_authority,
        );
        let machine = scope.machine(FIRST_CONTROLLER)?;
        machine
            .create(crate::environments::MachinePlan {
                shape: *shape,
                image: ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?,
                network: network.clone(),
                storage_backend: storage_backend.clone(),
                security_groups: vec![scope.named("controller")?],
                user_data,
                labels: BTreeMap::new(),
            })
            .await?;
        let target = FloatingIpTarget { vm: machine.name().clone(), interface: 0 };
        let address = scope.ownership.reference::<FloatingIp>(scope.named("api")?);
        scope
            .objects()
            .change_specification::<FloatingIp>(&address, |current| {
                current
                    .target
                    .is_none()
                    .then(|| FloatingIpSpecification { target: Some(target), ..current.clone() })
            })
            .await
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
    async fn target(&self, scope: &ControllerScope<'_>) -> Result<Option<(Target, ObjectName)>, ActionError> {
        if let Some(cluster) = scope.host_cluster().await? {
            return Ok(Some((
                self.cluster_target(scope, &cluster).await?,
                cluster.specification.network.clone(),
            )));
        }
        let controller = scope.machine(FIRST_CONTROLLER)?;
        let Some(vm) = controller.vm().await? else {
            return Ok(None);
        };
        let Some(address) = vm.status.interfaces.first().map(|interface| interface.address) else {
            return Ok(None);
        };
        let network = vm.specification.network_interfaces.first().map(|interface| interface.network.clone());
        let network = network.ok_or_else(|| ActionError::fatal("the controller machine has no network"))?;
        let ssh_private_key = scope.secret_value(SSH_PRIVATE_KEY).await?;
        Ok(Some((Target::Machine { address, ssh_private_key }, network)))
    }

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

impl Bootstrap {
    async fn has_bootstrapped(
        &self,
        scope: &ControllerScope<'_>,
        runner: &ObjectName,
    ) -> Result<bool, ActionError> {
        if let Ok(Some(_)) = self.environment.platform.controller(&scope.endpoint().await?).await {
            return Ok(true);
        }
        let started = scope.context.operation.status.current_step.as_ref().map(|step| step.started_at);
        let is_overdue =
            started.is_some_and(|started| scope.context.clock.now() - started > BOOTSTRAP_DEADLINE);
        if is_overdue {
            return Err(ActionError::fatal(format!(
                "no Juju controller answered within {} minutes of starting its bootstrap; VM {runner} keeps the log in {RUNNER_LOG}",
                BOOTSTRAP_DEADLINE.whole_minutes()
            )));
        }
        Ok(false)
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for Bootstrap {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let runner = scope.machine(RUNNER)?;
        if matches!(scope.specification().await?.substrate, Substrate::Vms { .. }) {
            return self.has_bootstrapped(&scope, runner.name()).await;
        }
        match runner.phase().await? {
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
        if runner.vm().await?.is_some() {
            return Ok(());
        }
        let Some((target, network)) = self.target(&scope).await? else {
            return Ok(());
        };
        let specification = scope.specification().await?;
        let secret = scope.secret().await?;
        let plan = BootstrapPlan {
            controller: scope.ownership.environment().to_string(),
            version: specification.version.clone(),
            sign_in: specification.sign_in,
            certificate: secret.value(CA_CERTIFICATE).unwrap_or_default().to_owned(),
            private_key: secret.value(CA_KEY).unwrap_or_default().to_owned(),
            token_refresh_url: self.environment.settings.token_refresh_url.clone(),
            target,
        };
        match specification.substrate {
            Substrate::Vms { storage_backend, .. } => {
                runner
                    .run_once(MachinePlan {
                        shape: MachineShape::Small,
                        image: ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?,
                        network,
                        storage_backend,
                        security_groups: vec![scope.named("controller")?],
                        user_data: plan.runner_user_data(),
                        labels: BTreeMap::new(),
                    })
                    .await
            }
            Substrate::Kubernetes { .. } => {
                runner
                    .run_container(ContainerPlan {
                        image: ObjectName::new(RUNNER_IMAGE).map_err(ActionError::fatal)?,
                        environment: plan.environment(),
                        network,
                        security_groups: Vec::new(),
                    })
                    .await
            }
        }
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

impl AddControllerMachines {
    async fn controllers(
        scope: &ControllerScope<'_>,
    ) -> Result<(u8, amafu_domain::kinds::machine_shape::MachineShape), ActionError> {
        match scope.specification().await?.substrate {
            Substrate::Vms { controllers, shape, .. } => Ok((controllers, shape)),
            Substrate::Kubernetes { .. } => {
                Err(ActionError::fatal("a controller on Kubernetes has no machines"))
            }
        }
    }

    fn suffixes(count: u8) -> Vec<String> {
        (1..count).map(|index| format!("controller-{index}")).collect()
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for AddControllerMachines {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (count, _) = Self::controllers(&scope).await?;
        let endpoint = scope.endpoint().await?;
        let model = scope.model_uuid(&ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?).await?;
        let machines = self.environment.platform.machines(&endpoint, &model).await?;
        for suffix in Self::suffixes(count) {
            if !ProvisionedMachine::new(&scope, &suffix)?.is_started(&machines).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (count, shape) = Self::controllers(&scope).await?;
        let endpoint = scope.endpoint().await?;
        let model = ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?;
        let model_uuid = scope.model_uuid(&model).await?;
        for suffix in Self::suffixes(count) {
            ProvisionedMachine::new(&scope, &suffix)?
                .provision(&endpoint, &model, &model_uuid, shape, None)
                .await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for EnableHa {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (count, _) = AddControllerMachines::controllers(&scope).await?;
        let endpoint = scope.endpoint().await?;
        let model = scope.model_uuid(&ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?).await?;
        let machines = self.environment.platform.machines(&endpoint, &model).await?;
        let controllers = machines
            .iter()
            .filter(|machine| {
                machine.is_started && machine.units.iter().any(|unit| unit.starts_with("controller/"))
            })
            .count();
        Ok(controllers >= usize::from(count))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (count, _) = AddControllerMachines::controllers(&scope).await?;
        let mut placements = Vec::new();
        for suffix in AddControllerMachines::suffixes(count) {
            let id = ProvisionedMachine::new(&scope, &suffix)?.juju_id().await?;
            placements
                .push(id.ok_or_else(|| ActionError::retryable(format!("{suffix} has no Juju machine yet")))?);
        }
        self.environment.platform.enable_ha(&scope.endpoint().await?, &placements).await
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
        if matches!(scope.specification().await?.substrate, Substrate::Vms { .. }) {
            let endpoint = scope.endpoint().await?;
            let model =
                scope.model_uuid(&ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?).await?;
            self.environment.platform.remove_authorized_key(&endpoint, &model, BOOTSTRAP_KEY_COMMENT).await?;
        }
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
