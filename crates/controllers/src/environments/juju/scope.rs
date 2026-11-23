use std::net::Ipv4Addr;

use amafu_domain::kinds::environment_secret::{EnvironmentSecret, EnvironmentSecretSpecification};
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuControllerSpecification, JujuControllerStatus, JujuModelStatus,
    JujuRequest, Substrate,
};
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::labels::JUJU_CONTROLLER;
use amafu_domain::platform_access::{ISSUER, SERVICE_SUBJECT, juju_user};
use amafu_domain::{Object, ObjectName, ObjectReference};
use amafu_interfaces::{ActionError, JujuCredentials, JujuEndpoint, KubernetesEndpoint};
use amafu_reconciler::OperationContext;
use amafu_token_issuer::Claims;
use serde_json::{Value, json};
use time::Duration;

use super::JujuEnvironment;
use super::bootstrap::{BootstrapPlan, Target};
use crate::environments::kubernetes::endpoint_of;
use crate::environments::{EnvironmentObjects, Machine, Ownership};

pub const CONTROLLER_MODEL: &str = "controller";
pub const UBUNTU_IMAGE: &str = "ubuntu-24-04";
pub const RUNNER_IMAGE: &str = "amafu-juju-runner";
pub const CA_CERTIFICATE: &str = "ca-crt";
pub const CA_KEY: &str = "ca-key";
pub const CREDENTIAL_SECRET: &str = "credential-secret";
pub const ADMIN_PASSWORD: &str = "admin-password";
pub const ADMIN_USER: &str = "admin";

const SERVICE_TOKEN_LIFETIME: Duration = Duration::minutes(5);
const UNKNOWN_AUDIENCE: &str = "juju";

fn service_access(controller: Option<&str>, models: &[JujuModelStatus]) -> Value {
    let mut access = serde_json::Map::new();
    if let Some(controller) = controller {
        access.insert(format!("controller-{controller}"), json!("superuser"));
    }
    for model in models {
        access.insert(format!("model-{}", model.uuid), json!("admin"));
    }
    Value::Object(access)
}

pub type ChangeContext<'a> = OperationContext<'a, JujuControllerChange>;

pub struct ControllerScope<'a> {
    pub context: &'a ChangeContext<'a>,
    pub environment: &'a JujuEnvironment,
    pub ownership: Ownership,
}

impl<'a> ControllerScope<'a> {
    pub fn new(
        context: &'a ChangeContext<'a>,
        environment: &'a JujuEnvironment,
    ) -> Result<Self, ActionError> {
        let project = context
            .operation
            .metadata
            .project
            .clone()
            .ok_or_else(|| ActionError::fatal("a Juju change belongs to a project"))?;
        let controller = context.operation.specification.controller.clone();
        Ok(Self { context, environment, ownership: Ownership::new(JUJU_CONTROLLER, controller, project) })
    }

    #[must_use]
    pub fn objects(&self) -> EnvironmentObjects<'_> {
        EnvironmentObjects {
            store: self.context.store,
            fence: self.context.fence,
            identifiers: self.environment.identifiers.as_ref(),
        }
    }

    #[must_use]
    pub fn request(&self) -> &JujuRequest {
        &self.context.operation.specification.request
    }

    #[must_use]
    pub fn change_tag(&self) -> String {
        self.context.operation.metadata.id.to_string().chars().take(8).collect()
    }

    #[must_use]
    pub fn controller_reference(&self) -> ObjectReference {
        self.ownership.reference::<JujuController>(self.ownership.environment().clone())
    }

    pub async fn controller(&self) -> Result<Object<JujuController>, ActionError> {
        self.objects().require::<JujuController>(&self.controller_reference()).await
    }

    pub async fn specification(&self) -> Result<JujuControllerSpecification, ActionError> {
        if let JujuRequest::Create { controller } = self.request() {
            return Ok(controller.clone());
        }
        Ok(self.controller().await?.specification)
    }

    pub fn named(&self, suffix: &str) -> Result<ObjectName, ActionError> {
        self.ownership.named(suffix)
    }

    pub fn machine(&self, suffix: &str) -> Result<Machine<'_>, ActionError> {
        Ok(Machine::new(self.objects(), &self.ownership, self.named(suffix)?))
    }

    pub fn secret_reference(controller: &Object<JujuController>) -> Result<ObjectReference, ActionError> {
        let name = ObjectName::new(format!("juju-{}", controller.metadata.id)).map_err(ActionError::fatal)?;
        Ok(ObjectReference::of::<EnvironmentSecret>(None, name))
    }

    pub async fn secret(&self) -> Result<EnvironmentSecretSpecification, ActionError> {
        let reference = Self::secret_reference(&self.controller().await?)?;
        Ok(self.objects().require::<EnvironmentSecret>(&reference).await?.specification)
    }

    pub async fn secret_value(&self, key: &str) -> Result<String, ActionError> {
        self.secret()
            .await?
            .value(key)
            .map(str::to_owned)
            .ok_or_else(|| ActionError::fatal(format!("the controller secret has no {key}")))
    }

    pub async fn remember(&self, key: &str, value: String) -> Result<(), ActionError> {
        let reference = Self::secret_reference(&self.controller().await?)?;
        let key = key.to_owned();
        self.objects()
            .change_specification::<EnvironmentSecret>(&reference, |current| {
                (current.value(&key) != Some(value.as_str())).then(|| {
                    let mut updated = current.clone();
                    updated.values.insert(key.clone(), value.clone());
                    updated
                })
            })
            .await
    }

    pub async fn api_address(&self) -> Result<Option<Ipv4Addr>, ActionError> {
        let reference = self.ownership.reference::<FloatingIp>(self.named("api")?);
        Ok(self.objects().read::<FloatingIp>(&reference).await?.and_then(|address| address.status.address))
    }

    async fn address(&self, controller: &Object<JujuController>) -> Result<Ipv4Addr, ActionError> {
        if let Some(address) = controller.status.addresses.iter().find_map(|address| address.parse().ok()) {
            return Ok(address);
        }
        self.api_address().await?.ok_or_else(|| ActionError::retryable("the controller has no address yet"))
    }

    pub async fn endpoint(&self) -> Result<JujuEndpoint, ActionError> {
        let controller = self.controller().await?;
        let address = self.address(&controller).await?;
        let secret = self.secret().await?;
        let certificate_authority = secret.value(CA_CERTIFICATE).map(str::to_owned).ok_or_else(|| {
            ActionError::fatal(format!("the controller's secret holds no {CA_CERTIFICATE}"))
        })?;
        let credentials = match (controller.status.uuid.as_deref(), secret.value(ADMIN_PASSWORD)) {
            (None, Some(password)) => {
                JujuCredentials::Password { user: String::from(ADMIN_USER), password: password.to_owned() }
            }
            (uuid, _) => JujuCredentials::Token(self.service_token(uuid, &controller.status.models).await?),
        };
        Ok(JujuEndpoint { address, certificate_authority, credentials })
    }

    async fn service_token(
        &self,
        uuid: Option<&str>,
        models: &[JujuModelStatus],
    ) -> Result<String, ActionError> {
        let now = self.context.clock.now();
        let claims = Claims::new(
            ISSUER,
            &juju_user(SERVICE_SUBJECT),
            uuid.unwrap_or(UNKNOWN_AUDIENCE),
            now,
            SERVICE_TOKEN_LIFETIME,
        )
        .with("access", service_access(uuid, models));
        Ok(self.environment.keyring.sign(&claims, now).await.map_err(ActionError::retryable)?.token)
    }

    pub async fn model_uuid(&self, model: &ObjectName) -> Result<String, ActionError> {
        let controller = self.controller().await?;
        if let Some(known) = controller.status.models.iter().find(|known| &known.name == model) {
            return Ok(known.uuid.clone());
        }
        let endpoint = self.endpoint().await?;
        self.environment
            .platform
            .models(&endpoint)
            .await?
            .into_iter()
            .find(|found| found.name == model.as_str())
            .map(|found| found.uuid)
            .ok_or_else(|| ActionError::fatal(format!("the controller has no model {model}")))
    }

    pub async fn host_cluster(&self) -> Result<Option<Object<KubernetesCluster>>, ActionError> {
        let Substrate::Kubernetes { cluster } = self.specification().await?.substrate else {
            return Ok(None);
        };
        let reference =
            ObjectReference::of::<KubernetesCluster>(Some(self.ownership.project().clone()), cluster);
        Ok(Some(self.objects().require::<KubernetesCluster>(&reference).await?))
    }

    pub async fn host_endpoint(
        &self,
        cluster: &Object<KubernetesCluster>,
    ) -> Result<KubernetesEndpoint, ActionError> {
        endpoint_of(self.objects(), &self.environment.keyring, self.context.clock.now(), cluster).await
    }

    pub fn step_is_overdue(&self, deadline: Duration) -> bool {
        let started = self.context.operation.status.current_step.as_ref().map(|step| step.started_at);
        started.is_some_and(|started| self.context.clock.now() - started > deadline)
    }

    pub async fn bootstrap_plan(&self, target: Target) -> Result<BootstrapPlan, ActionError> {
        let specification = self.specification().await?;
        let secret = self.secret().await?;
        Ok(BootstrapPlan {
            controller: self.ownership.environment().to_string(),
            version: specification.version.clone(),
            sign_in: specification.sign_in,
            certificate: secret.value(CA_CERTIFICATE).unwrap_or_default().to_owned(),
            private_key: secret.value(CA_KEY).unwrap_or_default().to_owned(),
            token_refresh_url: self.environment.settings.token_refresh_url.clone(),
            target,
        })
    }

    pub async fn update_status(
        &self,
        change: impl Fn(&mut JujuControllerStatus) + Send + Sync,
    ) -> Result<(), ActionError> {
        self.objects()
            .change_status::<JujuController>(&self.controller_reference(), |controller| {
                let mut status = controller.status.clone();
                change(&mut status);
                (status != controller.status).then_some(status)
            })
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodSubstrate {
    pub pods: u8,
    pub shape: MachineShape,
    pub network: ObjectName,
    pub storage_backend: ObjectName,
}

pub fn pod_substrate(specification: &JujuControllerSpecification) -> Result<PodSubstrate, ActionError> {
    match &specification.substrate {
        Substrate::Pods { pods, shape, network, storage_backend, .. } => Ok(PodSubstrate {
            pods: *pods,
            shape: *shape,
            network: network.clone(),
            storage_backend: storage_backend.clone(),
        }),
        Substrate::Kubernetes { .. } => Err(ActionError::fatal("a controller on Kubernetes has no pods")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_token_grants_the_controller_and_every_known_model() {
        let models =
            [JujuModelStatus { name: ObjectName::new("default").unwrap(), uuid: String::from("m1") }];
        assert_eq!(
            service_access(Some("c1"), &models),
            json!({"controller-c1": "superuser", "model-m1": "admin"})
        );
        assert_eq!(service_access(None, &[]), json!({}));
    }
}
