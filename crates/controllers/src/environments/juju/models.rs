use std::sync::Arc;

use amafu_domain::ObjectName;
use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::environment_secret::EnvironmentSecret;
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuModelStatus, JujuVersion,
};
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::kinds::security_group::SecurityGroup;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_interfaces::{ActionError, JujuModel};
use amafu_reconciler::Step;

use super::JujuEnvironment;
use super::controller::{decoded_token, removable_manifests, token_secret};
use super::scope::{CONTROLLER_MODEL, CREDENTIAL_SECRET, ChangeContext, ControllerScope};

async fn platform_models(scope: &ControllerScope<'_>) -> Result<Vec<JujuModel>, ActionError> {
    scope.environment.platform.models(&scope.endpoint().await?).await
}

fn hosted(models: &[JujuModel]) -> impl Iterator<Item = &JujuModel> {
    models.iter().filter(|model| model.name != CONTROLLER_MODEL)
}

pub struct AddModel {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for AddModel {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.controller().await?.status.models.iter().any(|model| model.name == self.model))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let platform = &self.environment.platform;
        let existing =
            platform.models(&endpoint).await?.into_iter().find(|model| model.name == self.model.as_str());
        let model = match existing {
            Some(model) => model,
            None => platform.add_model(&endpoint, self.model.as_str()).await?,
        };
        let recorded = JujuModelStatus { name: self.model.clone(), uuid: model.uuid };
        scope
            .update_status(|status| {
                if !status.models.iter().any(|known| known.name == recorded.name) {
                    status.models.push(recorded.clone());
                }
            })
            .await
    }
}

pub struct DestroyModel {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for DestroyModel {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(!scope.controller().await?.status.models.iter().any(|model| model.name == self.model))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let models = platform_models(&scope).await?;
        if let Some(model) = models.iter().find(|model| model.name == self.model.as_str()) {
            return self.environment.platform.destroy_model(&scope.endpoint().await?, &model.uuid).await;
        }
        let name = self.model.clone();
        scope.update_status(|status| status.models.retain(|model| model.name != name)).await
    }
}

pub struct DestroyModels {
    pub environment: Arc<JujuEnvironment>,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for DestroyModels {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        if scope.controller().await?.status.uuid.is_none() {
            return Ok(true);
        }
        Ok(hosted(&platform_models(&scope).await?).next().is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        scope.update_status(|status| status.phase = EnvironmentPhase::Deleting).await?;
        let endpoint = scope.endpoint().await?;
        for model in hosted(&self.environment.platform.models(&endpoint).await?) {
            self.environment.platform.destroy_model(&endpoint, &model.uuid).await?;
        }
        Ok(())
    }
}

pub struct UpgradeController {
    pub environment: Arc<JujuEnvironment>,
    pub version: JujuVersion,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for UpgradeController {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let running = self.environment.platform.controller(&scope.endpoint().await?).await?;
        Ok(running.is_some_and(|controller| controller.version == self.version.to_string()))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let controller =
            scope.model_uuid(&ObjectName::new(CONTROLLER_MODEL).map_err(ActionError::fatal)?).await?;
        self.environment.platform.upgrade_model(&endpoint, &controller, &self.version.to_string()).await
    }
}

pub struct UpgradeModels {
    pub environment: Arc<JujuEnvironment>,
    pub version: JujuVersion,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for UpgradeModels {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let target = self.version.to_string();
        Ok(hosted(&platform_models(&scope).await?).all(|model| model.version == target))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let target = self.version.to_string();
        for model in hosted(&self.environment.platform.models(&endpoint).await?) {
            if model.version != target {
                self.environment.platform.upgrade_model(&endpoint, &model.uuid, &target).await?;
            }
        }
        let version = self.version.clone();
        scope
            .objects()
            .change_specification::<JujuController>(&scope.controller_reference(), |current| {
                (current.version != version).then(|| {
                    let mut updated = current.clone();
                    updated.version = version.clone();
                    updated
                })
            })
            .await?;
        scope.update_status(|status| status.version = Some(self.version.clone())).await
    }
}

pub struct RotateCredential {
    pub environment: Arc<JujuEnvironment>,
}

impl RotateCredential {
    fn rotated_name(scope: &ControllerScope<'_>) -> String {
        format!("juju-{}-{}", scope.ownership.environment(), scope.change_tag())
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RotateCredential {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.secret().await?.value(CREDENTIAL_SECRET) == Some(Self::rotated_name(&scope).as_str()))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let cluster = scope.host_cluster().await?.ok_or_else(|| {
            ActionError::fatal("only a controller on Kubernetes has a cloud credential to rotate")
        })?;
        let kubernetes_endpoint = scope.host_endpoint(&cluster).await?;
        let controller = scope.ownership.environment().clone();
        let rotated = token_secret(&controller, &Self::rotated_name(&scope));
        let kubernetes = &self.environment.kubernetes;
        kubernetes.apply(&kubernetes_endpoint, std::slice::from_ref(&rotated)).await?;
        let Some(token) =
            kubernetes.read(&kubernetes_endpoint, &rotated).await?.as_ref().and_then(decoded_token)
        else {
            return Ok(());
        };
        self.environment.platform.update_kubernetes_credential(&scope.endpoint().await?, &token).await?;
        let previous = scope.secret().await?.value(CREDENTIAL_SECRET).map(str::to_owned);
        if let Some(previous) = previous {
            kubernetes.delete(&kubernetes_endpoint, &token_secret(&controller, &previous)).await?;
        }
        scope.remember(CREDENTIAL_SECRET, Self::rotated_name(&scope)).await
    }
}

pub struct DeleteResources {
    pub environment: Arc<JujuEnvironment>,
}

impl DeleteResources {
    async fn leave_host_cluster(&self, scope: &ControllerScope<'_>) -> Result<(), ActionError> {
        let Some(cluster) = scope.host_cluster().await? else {
            return Ok(());
        };
        let endpoint = scope.host_endpoint(&cluster).await?;
        let controller = scope.ownership.environment().clone();
        let mut manifests = removable_manifests(&controller).to_vec();
        if let Some(rotated) = scope.secret().await?.value(CREDENTIAL_SECRET) {
            manifests.push(token_secret(&controller, rotated));
        }
        for manifest in &manifests {
            self.environment.kubernetes.delete(&endpoint, manifest).await?;
        }
        scope
            .objects()
            .change_status::<KubernetesCluster>(&cluster.reference(), |cluster| {
                cluster.status.hosted_juju_controllers.contains(&controller).then(|| {
                    let mut status = cluster.status.clone();
                    status.hosted_juju_controllers.retain(|hosted| hosted != &controller);
                    status
                })
            })
            .await
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for DeleteResources {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(scope.objects().read::<JujuController>(&scope.controller_reference()).await?.is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let objects = scope.objects();
        let reference = scope.controller_reference();
        let Some(controller) = objects.read::<JujuController>(&reference).await? else {
            return Ok(());
        };
        let ownership = &scope.ownership;
        if !objects.delete_owned::<Vm>(ownership).await? {
            return Ok(());
        }
        let volumes_gone = objects.delete_owned::<Volume>(ownership).await?;
        let addresses_gone = objects.delete_owned::<FloatingIp>(ownership).await?;
        let groups_gone = objects.delete_owned::<SecurityGroup>(ownership).await?;
        if !(volumes_gone && addresses_gone && groups_gone) {
            return Ok(());
        }
        self.leave_host_cluster(&scope).await?;
        objects.erase::<EnvironmentSecret>(&ControllerScope::secret_reference(&controller)?).await?;
        objects.erase::<JujuController>(&reference).await
    }
}
