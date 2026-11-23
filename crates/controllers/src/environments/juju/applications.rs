use std::sync::Arc;

use amafu_domain::ObjectName;
use amafu_domain::kinds::juju_controller::{Deployment, JujuControllerChange};
use amafu_interfaces::{ActionError, Charm, JujuApplication};
use amafu_reconciler::Step;

use super::JujuEnvironment;
use super::scope::{ChangeContext, ControllerScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitChange {
    Add(u16),
    Remove(Vec<String>),
}

#[must_use]
pub fn units_to_reach(units: &[String], wanted: u16) -> Option<UnitChange> {
    let current = units.len();
    let wanted = usize::from(wanted);
    if current < wanted {
        return u16::try_from(wanted - current).ok().map(UnitChange::Add);
    }
    (current > wanted).then(|| UnitChange::Remove(units[wanted..].to_vec()))
}

struct Model<'s> {
    scope: &'s ControllerScope<'s>,
    uuid: String,
}

impl<'s> Model<'s> {
    async fn of(scope: &'s ControllerScope<'s>, model: &ObjectName) -> Result<Self, ActionError> {
        Ok(Self { scope, uuid: scope.model_uuid(model).await? })
    }

    async fn application(&self, name: &ObjectName) -> Result<Option<JujuApplication>, ActionError> {
        let endpoint = self.scope.endpoint().await?;
        let applications = self.scope.environment.platform.applications(&endpoint, &self.uuid).await?;
        Ok(applications.into_iter().find(|application| application.name == name.as_str()))
    }

    async fn required_application(&self, name: &ObjectName) -> Result<JujuApplication, ActionError> {
        self.application(name)
            .await?
            .ok_or_else(|| ActionError::fatal(format!("the model has no application {name}")))
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
        let model = Model::of(&scope, &self.deployment.model).await?;
        Ok(model.application(&self.deployment.application).await?.is_some())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.deployment.model).await?;
        let charm = Charm {
            application: self.deployment.application.to_string(),
            charm: self.deployment.charm.clone(),
            channel: self.deployment.channel.clone(),
        };
        let endpoint = scope.endpoint().await?;
        self.environment.platform.deploy(&endpoint, &model.uuid, &charm, self.deployment.units).await
    }
}

pub struct Expose {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for Expose {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        Ok(model.required_application(&self.application).await?.is_exposed)
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        let endpoint = scope.endpoint().await?;
        self.environment.platform.expose(&endpoint, &model.uuid, self.application.as_str()).await
    }
}

pub struct ScaleApplication {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
    pub units: u16,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for ScaleApplication {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        let application = model.required_application(&self.application).await?;
        Ok(units_to_reach(&application.units, self.units).is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        let application = model.required_application(&self.application).await?;
        let endpoint = scope.endpoint().await?;
        let platform = &self.environment.platform;
        match units_to_reach(&application.units, self.units) {
            Some(UnitChange::Add(count)) => {
                platform.add_units(&endpoint, &model.uuid, &application.name, count).await
            }
            Some(UnitChange::Remove(units)) => platform.remove_units(&endpoint, &model.uuid, &units).await,
            None => Ok(()),
        }
    }
}

pub struct RemoveUnits {
    pub environment: Arc<JujuEnvironment>,
    pub model: ObjectName,
    pub application: ObjectName,
    pub units: Vec<String>,
}

#[async_trait::async_trait]
impl Step<JujuControllerChange> for RemoveUnits {
    async fn is_complete(&self, context: &ChangeContext<'_>) -> Result<bool, ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        let remaining = model.application(&self.application).await?.map(|application| application.units);
        Ok(!remaining.unwrap_or_default().iter().any(|unit| self.units.contains(unit)))
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        self.environment.platform.remove_units(&scope.endpoint().await?, &model.uuid, &self.units).await
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
        let model = Model::of(&scope, &self.model).await?;
        Ok(model.application(&self.application).await?.is_none())
    }

    async fn run(&self, context: &ChangeContext<'_>) -> Result<(), ActionError> {
        let scope = ControllerScope::new(context, &self.environment)?;
        let model = Model::of(&scope, &self.model).await?;
        let endpoint = scope.endpoint().await?;
        self.environment.platform.remove_application(&endpoint, &model.uuid, self.application.as_str()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn scaling_adds_the_missing_units_or_removes_the_newest_ones() {
        let current = units(&["web/0", "web/2", "web/5"]);
        assert_eq!(units_to_reach(&current, 5), Some(UnitChange::Add(2)));
        assert_eq!(units_to_reach(&current, 3), None);
        assert_eq!(units_to_reach(&current, 1), Some(UnitChange::Remove(units(&["web/2", "web/5"]))));
    }
}
