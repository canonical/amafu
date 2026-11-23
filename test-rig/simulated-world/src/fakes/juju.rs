use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use amafu_interfaces::{
    ActionError, Charm, JujuApplication, JujuControllerInfo, JujuEndpoint, JujuMachine, JujuModel,
    JujuPlatform,
};

const CONTROLLER_MODEL: &str = "model-controller";
const CONTROLLER_APPLICATION: &str = "controller";

#[derive(Debug, Default)]
struct Model {
    machines: Vec<JujuMachine>,
    applications: BTreeMap<String, bool>,
    next_machine: u32,
    next_unit: BTreeMap<String, u32>,
}

impl Model {
    fn add_unit(&mut self, application: &str) {
        let number = self.next_unit.entry(application.to_owned()).or_default();
        let unit = format!("{application}/{number}");
        *number += 1;
        let id = self.next_machine.to_string();
        self.next_machine += 1;
        self.machines.push(JujuMachine { id, is_started: true, units: vec![unit] });
    }

    fn remove_units(&mut self, doomed: impl Fn(&str) -> bool) {
        for machine in &mut self.machines {
            machine.units.retain(|unit| !doomed(unit));
        }
        self.machines.retain(|machine| !machine.units.is_empty());
    }

    fn units_of(&self, application: &str) -> Vec<String> {
        let prefix = format!("{application}/");
        self.machines
            .iter()
            .flat_map(|machine| &machine.units)
            .filter(|unit| unit.starts_with(&prefix))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Default)]
struct State {
    is_bootstrapped: bool,
    version: String,
    models: Vec<JujuModel>,
    contents: BTreeMap<String, Model>,
    credential: Option<String>,
}

impl State {
    fn model(&mut self, uuid: &str) -> &mut Model {
        self.contents.entry(uuid.to_owned()).or_default()
    }
}

#[derive(Debug, Default)]
pub struct FakeJuju {
    state: Mutex<State>,
}

impl FakeJuju {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub fn bootstrap(&self, version: &str) {
        self.with(|state| {
            if state.is_bootstrapped {
                return;
            }
            state.is_bootstrapped = true;
            version.clone_into(&mut state.version);
            state.models.push(JujuModel {
                name: String::from("controller"),
                uuid: CONTROLLER_MODEL.to_owned(),
                version: version.to_owned(),
            });
            let controller = state.model(CONTROLLER_MODEL);
            controller.applications.insert(CONTROLLER_APPLICATION.to_owned(), false);
            controller.add_unit(CONTROLLER_APPLICATION);
        });
    }

    #[must_use]
    pub fn is_bootstrapped(&self) -> bool {
        self.with(|state| state.is_bootstrapped)
    }

    #[must_use]
    pub fn machines(&self, model: &str) -> Vec<JujuMachine> {
        self.with(|state| state.model(model).machines.clone())
    }

    #[must_use]
    pub fn credential(&self) -> Option<String> {
        self.with(|state| state.credential.clone())
    }

    fn require_bootstrapped(&self) -> Result<(), ActionError> {
        if self.is_bootstrapped() {
            Ok(())
        } else {
            Err(ActionError::retryable("the controller does not answer yet"))
        }
    }
}

#[async_trait::async_trait]
impl JujuPlatform for FakeJuju {
    async fn controller(&self, _endpoint: &JujuEndpoint) -> Result<Option<JujuControllerInfo>, ActionError> {
        Ok(self.with(|state| {
            state.is_bootstrapped.then(|| JujuControllerInfo {
                uuid: String::from("controller-uuid"),
                version: state.version.clone(),
            })
        }))
    }

    async fn models(&self, _endpoint: &JujuEndpoint) -> Result<Vec<JujuModel>, ActionError> {
        self.require_bootstrapped()?;
        Ok(self.with(|state| state.models.clone()))
    }

    async fn add_model(&self, _endpoint: &JujuEndpoint, name: &str) -> Result<JujuModel, ActionError> {
        self.require_bootstrapped()?;
        Ok(self.with(|state| {
            let model = JujuModel {
                name: name.to_owned(),
                uuid: format!("model-{name}"),
                version: state.version.clone(),
            };
            state.models.push(model.clone());
            model
        }))
    }

    async fn destroy_model(&self, _endpoint: &JujuEndpoint, model: &str) -> Result<(), ActionError> {
        self.with(|state| {
            state.models.retain(|known| known.uuid != model);
            state.contents.remove(model);
        });
        Ok(())
    }

    async fn upgrade_model(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        version: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| {
            for known in state.models.iter_mut().filter(|known| known.uuid == model) {
                version.clone_into(&mut known.version);
                if known.uuid == CONTROLLER_MODEL {
                    version.clone_into(&mut state.version);
                }
            }
        });
        Ok(())
    }

    async fn machines(&self, _endpoint: &JujuEndpoint, model: &str) -> Result<Vec<JujuMachine>, ActionError> {
        self.require_bootstrapped()?;
        Ok(self.machines(model))
    }

    async fn applications(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
    ) -> Result<Vec<JujuApplication>, ActionError> {
        self.require_bootstrapped()?;
        Ok(self.with(|state| {
            let model = state.model(model);
            model
                .applications
                .iter()
                .map(|(name, is_exposed)| JujuApplication {
                    name: name.clone(),
                    units: model.units_of(name),
                    is_exposed: *is_exposed,
                })
                .collect()
        }))
    }

    async fn enable_ha(&self, _endpoint: &JujuEndpoint, controllers: u8) -> Result<(), ActionError> {
        self.with(|state| {
            let model = state.model(CONTROLLER_MODEL);
            while model.units_of(CONTROLLER_APPLICATION).len() < usize::from(controllers) {
                model.add_unit(CONTROLLER_APPLICATION);
            }
        });
        Ok(())
    }

    async fn deploy(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        charm: &Charm,
        units: u16,
    ) -> Result<(), ActionError> {
        self.with(|state| {
            let model = state.model(model);
            model.applications.insert(charm.application.clone(), false);
            for _ in 0..units {
                model.add_unit(&charm.application);
            }
        });
        Ok(())
    }

    async fn add_units(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
        units: u16,
    ) -> Result<(), ActionError> {
        self.with(|state| {
            let model = state.model(model);
            for _ in 0..units {
                model.add_unit(application);
            }
        });
        Ok(())
    }

    async fn remove_units(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        units: &[String],
    ) -> Result<(), ActionError> {
        let doomed: BTreeSet<&str> = units.iter().map(String::as_str).collect();
        self.with(|state| state.model(model).remove_units(|unit| doomed.contains(unit)));
        Ok(())
    }

    async fn remove_application(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError> {
        let prefix = format!("{application}/");
        self.with(|state| {
            let model = state.model(model);
            model.remove_units(|unit| unit.starts_with(&prefix));
            model.applications.remove(application);
        });
        Ok(())
    }

    async fn expose(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| {
            if let Some(is_exposed) = state.model(model).applications.get_mut(application) {
                *is_exposed = true;
            }
        });
        Ok(())
    }

    async fn update_kubernetes_credential(
        &self,
        _endpoint: &JujuEndpoint,
        _cloud: &str,
        token: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| state.credential = Some(token.to_owned()));
        Ok(())
    }
}
