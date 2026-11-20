use std::collections::BTreeSet;
use std::sync::Arc;

use amafu_domain::ObjectName;
use amafu_domain::kinds::juju_controller::{Deployment, JujuControllerChange, JujuRequest};
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::labels::{JUJU_MACHINE, JUJU_MODEL};
use amafu_interfaces::{ActionError, Charm, JujuMachine};
use amafu_reconciler::Step;

use super::JujuEnvironment;
use super::provisioning::ProvisionedMachine;
use super::scope::{ChangeContext, ControllerScope};
use crate::environments::Machine;

const REPLACEMENT_SHAPE: MachineShape = MachineShape::Medium;

fn application_of(unit: &str) -> &str {
    unit.split_once('/').map_or(unit, |(application, _)| application)
}

fn hosts(machine: &JujuMachine, application: &str) -> bool {
    machine.units.iter().any(|unit| application_of(unit) == application)
}

struct ChangeMachines<'a> {
    scope: &'a ControllerScope<'a>,
    model: ObjectName,
    model_uuid: String,
    machines: Vec<ProvisionedMachine<'a>>,
}

impl<'a> ChangeMachines<'a> {
    async fn of(
        scope: &'a ControllerScope<'a>,
        model: &ObjectName,
        application: &ObjectName,
    ) -> Result<Self, ActionError> {
        let count = match scope.request() {
            JujuRequest::Deploy { deployment } => deployment.units,
            JujuRequest::AddUnits { units, .. } => *units,
            _ => return Err(ActionError::fatal("only deployments and new units create machines")),
        };
        let tag = scope.change_tag();
        let machines = (0..count)
            .map(|index| ProvisionedMachine::new(scope, &format!("{application}-{tag}-{index}")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { scope, model: model.clone(), model_uuid: scope.model_uuid(model).await?, machines })
    }

    async fn known(&self) -> Result<Vec<JujuMachine>, ActionError> {
        let endpoint = self.scope.endpoint().await?;
        self.scope.environment.platform.machines(&endpoint, &self.model_uuid).await
    }

    async fn juju_ids(&self) -> Result<Vec<String>, ActionError> {
        let mut ids = Vec::new();
        for machine in &self.machines {
            ids.push(machine.juju_id().await?.ok_or_else(|| {
                ActionError::retryable(format!("{} has no Juju machine yet", machine.machine().name()))
            })?);
        }
        Ok(ids)
    }

    async fn all_host(&self, application: &str) -> Result<bool, ActionError> {
        let known = self.known().await?;
        Ok(self
            .machines
            .iter()
            .all(|machine| machine.in_juju(&known).is_some_and(|found| hosts(found, application))))
    }
}

pub struct CreateMachines {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
    pub shape: MachineShape,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for CreateMachines {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines = ChangeMachines::of(&scope, &self.model, &self.application).await?;
        let known = machines.known().await?;
        for machine in &machines.machines {
            if !machine.is_started(&known).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines = ChangeMachines::of(&scope, &self.model, &self.application).await?;
        let endpoint = scope.endpoint().await?;
        for machine in &machines.machines {
            machine.provision(&endpoint, &machines.model, &machines.model_uuid, self.shape).await?;
        }
        Ok(())
    }

    async fn undo(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines = ChangeMachines::of(&scope, &self.model, &self.application).await?;
        let endpoint = scope.endpoint().await?;
        let known = machines.known().await?;
        for machine in &machines.machines {
            if let Some(found) = machine.in_juju(&known) {
                self.environment.platform.remove_machine(&endpoint, &machines.model_uuid, &found.id).await?;
            }
            if !machine.machine().remove().await? {
                return Err(ActionError::retryable("machines of the change are still being removed"));
            }
        }
        Ok(())
    }
}

pub struct Deploy {
    pub environment: Arc<JujuEnvironment>,
    pub deployment: Deployment,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for Deploy {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines =
            ChangeMachines::of(&scope, &self.deployment.model, &self.deployment.application).await?;
        machines.all_host(self.deployment.application.as_str()).await
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines =
            ChangeMachines::of(&scope, &self.deployment.model, &self.deployment.application).await?;
        let endpoint = scope.endpoint().await?;
        let charm = Charm {
            application: self.deployment.application.to_string(),
            charm: self.deployment.charm.clone(),
            channel: self.deployment.channel.clone(),
        };
        let platform = &self.environment.platform;
        platform.deploy(&endpoint, &machines.model_uuid, &charm, &machines.juju_ids().await?).await?;
        if self.deployment.expose {
            platform.expose(&endpoint, &machines.model_uuid, &charm.application).await?;
        }
        Ok(())
    }
}

pub struct AddUnits {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for AddUnits {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        ChangeMachines::of(&scope, &self.model, &self.application)
            .await?
            .all_host(self.application.as_str())
            .await
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let machines = ChangeMachines::of(&scope, &self.model, &self.application).await?;
        let endpoint = scope.endpoint().await?;
        let ids = machines.juju_ids().await?;
        self.environment
            .platform
            .add_units(&endpoint, &machines.model_uuid, self.application.as_str(), &ids)
            .await
    }
}

async fn model_machines(
    scope: &ControllerScope<'_>,
    model: &ObjectName,
) -> Result<(String, Vec<JujuMachine>), ActionError> {
    let model_uuid = scope.model_uuid(model).await?;
    let machines = scope.environment.platform.machines(&scope.endpoint().await?, &model_uuid).await?;
    Ok((model_uuid, machines))
}

pub struct RemoveUnits {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub units: Vec<String>,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RemoveUnits {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (_, machines) = model_machines(&scope, &self.model).await?;
        Ok(!machines.iter().flat_map(|machine| &machine.units).any(|unit| self.units.contains(unit)))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model_uuid = scope.model_uuid(&self.model).await?;
        self.environment.platform.remove_units(&scope.endpoint().await?, &model_uuid, &self.units).await
    }
}

pub struct RemoveApplication {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RemoveApplication {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (_, machines) = model_machines(&scope, &self.model).await?;
        Ok(!machines.iter().any(|machine| hosts(machine, self.application.as_str())))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model_uuid = scope.model_uuid(&self.model).await?;
        self.environment
            .platform
            .remove_application(&scope.endpoint().await?, &model_uuid, self.application.as_str())
            .await
    }
}

pub struct DeleteReleasedMachines {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
}

impl DeleteReleasedMachines {
    async fn released<'s>(
        scope: &'s ControllerScope<'s>,
        model: &ObjectName,
    ) -> Result<(String, Vec<(Machine<'s>, Option<String>)>), ActionError> {
        let (model_uuid, machines) = model_machines(scope, model).await?;
        let vms = amafu_interfaces::objects::list::<Vm>(
            scope.context.store,
            Some(scope.ownership.project()),
            amafu_interfaces::Consistency::Linearizable,
        )
        .await?;
        let released = vms
            .into_iter()
            .filter(|vm| scope.ownership.owns(&vm.metadata.labels))
            .filter(|vm| vm.metadata.labels.get(JUJU_MODEL).map(String::as_str) == Some(model.as_str()))
            .filter_map(|vm| {
                let id = vm.metadata.labels.get(JUJU_MACHINE).cloned();
                let in_use = machines
                    .iter()
                    .any(|machine| Some(&machine.id) == id.as_ref() && !machine.units.is_empty());
                (!in_use).then(|| {
                    let still_in_juju = id.filter(|id| machines.iter().any(|machine| &machine.id == id));
                    (Machine::new(scope.objects(), &scope.ownership, vm.metadata.name), still_in_juju)
                })
            })
            .collect();
        Ok((model_uuid, released))
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for DeleteReleasedMachines {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        Ok(Self::released(&scope, &self.model).await?.1.is_empty())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let (model_uuid, released) = Self::released(&scope, &self.model).await?;
        for (machine, juju_id) in released {
            if let Some(id) = juju_id {
                self.environment.platform.remove_machine(&endpoint, &model_uuid, &id).await?;
            }
            machine.remove().await?;
        }
        Ok(())
    }
}

pub struct ReplaceMachine {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub machine: String,
}

impl ReplaceMachine {
    fn replacement<'s>(&self, scope: &'s ControllerScope<'s>) -> Result<ProvisionedMachine<'s>, ActionError> {
        ProvisionedMachine::new(
            scope,
            &format!("replace-{}-{}", self.machine.replace('/', "-"), scope.change_tag()),
        )
    }
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for ReplaceMachine {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let (_, machines) = model_machines(&scope, &self.model).await?;
        let replacement = self.replacement(&scope)?;
        Ok(!machines.iter().any(|machine| machine.id == self.machine)
            && replacement.machine().vm().await?.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let endpoint = scope.endpoint().await?;
        let (model_uuid, machines) = model_machines(&scope, &self.model).await?;
        let Some(old) = machines.iter().find(|machine| machine.id == self.machine) else {
            return Ok(());
        };
        let replacement = self.replacement(&scope)?;
        if !replacement.is_started(&machines).await? {
            return replacement.provision(&endpoint, &self.model, &model_uuid, REPLACEMENT_SHAPE).await;
        }
        let Some(new) = replacement.in_juju(&machines) else {
            return Ok(());
        };
        let applications: BTreeSet<&str> = old.units.iter().map(|unit| application_of(unit)).collect();
        let platform = &self.environment.platform;
        let mut is_moved = true;
        for application in applications.into_iter().filter(|application| !hosts(new, application)) {
            platform.add_units(&endpoint, &model_uuid, application, std::slice::from_ref(&new.id)).await?;
            is_moved = false;
        }
        if is_moved {
            platform.remove_machine(&endpoint, &model_uuid, &old.id).await?;
        }
        Ok(())
    }
}
