use std::collections::BTreeMap;
use std::sync::Mutex;

use amafu_interfaces::{
    ActionError, Charm, JujuControllerInfo, JujuEndpoint, JujuMachine, JujuModel, JujuPlatform, ManualMachine,
};

const CONTROLLER_MODEL: &str = "controller";

#[derive(Debug, Default)]
struct State {
    is_bootstrapped: bool,
    version: String,
    models: Vec<JujuModel>,
    machines: BTreeMap<String, Vec<JujuMachine>>,
    next_machine: u32,
    next_unit: BTreeMap<String, u32>,
    removed_keys: Vec<String>,
    exposed: Vec<String>,
    credential: Option<String>,
}

impl State {
    fn machines_of(&mut self, model: &str) -> &mut Vec<JujuMachine> {
        self.machines.entry(model.to_owned()).or_default()
    }

    fn place(&mut self, model: &str, application: &str, machine: &str) {
        let next = self.next_unit.entry(application.to_owned()).or_default();
        let unit = format!("{application}/{next}");
        *next += 1;
        if let Some(found) = self.machines_of(model).iter_mut().find(|found| found.id == machine) {
            found.units.push(unit);
        }
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
                name: CONTROLLER_MODEL.to_owned(),
                uuid: String::from("model-controller"),
                version: version.to_owned(),
            });
            state.machines.insert(
                String::from("model-controller"),
                vec![JujuMachine {
                    id: String::from("0"),
                    instance_id: String::from("manual:bootstrap"),
                    is_started: true,
                    units: vec![String::from("controller/0")],
                }],
            );
            state.next_machine = 1;
            state.next_unit.insert(String::from("controller"), 1);
        });
    }

    pub fn start(&self, machine: &str) {
        self.with(|state| {
            for found in state.machines.values_mut().flatten().filter(|found| found.id == machine) {
                found.is_started = true;
            }
        });
    }

    #[must_use]
    pub fn machines(&self, model: &str) -> Vec<JujuMachine> {
        self.with(|state| state.machines_of(model).clone())
    }

    #[must_use]
    pub fn removed_keys(&self) -> Vec<String> {
        self.with(|state| state.removed_keys.clone())
    }

    #[must_use]
    pub fn credential(&self) -> Option<String> {
        self.with(|state| state.credential.clone())
    }

    #[must_use]
    pub fn exposed(&self) -> Vec<String> {
        self.with(|state| state.exposed.clone())
    }

    fn require_bootstrapped(&self) -> Result<(), ActionError> {
        if self.with(|state| state.is_bootstrapped) {
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
            state.machines.remove(model);
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
                if known.name == CONTROLLER_MODEL {
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

    async fn add_machine(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        machine: &ManualMachine,
    ) -> Result<String, ActionError> {
        Ok(self.with(|state| {
            let id = state.next_machine.to_string();
            state.next_machine += 1;
            state.machines_of(model).push(JujuMachine {
                id: id.clone(),
                instance_id: machine.instance_id.clone(),
                is_started: false,
                units: Vec::new(),
            });
            id
        }))
    }

    async fn provisioning_script(
        &self,
        _endpoint: &JujuEndpoint,
        _model: &str,
        machine: &str,
        nonce: &str,
    ) -> Result<String, ActionError> {
        Ok(format!("#!/bin/sh\nexec juju-provision {machine} {nonce}\n"))
    }

    async fn remove_machine(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        machine: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| state.machines_of(model).retain(|found| found.id != machine));
        Ok(())
    }

    async fn enable_ha(&self, _endpoint: &JujuEndpoint, machines: &[String]) -> Result<(), ActionError> {
        self.with(|state| {
            for machine in machines {
                let hosts_controller = state
                    .machines_of("model-controller")
                    .iter()
                    .any(|found| &found.id == machine && !found.units.is_empty());
                if !hosts_controller {
                    state.place("model-controller", "controller", machine);
                }
            }
        });
        Ok(())
    }

    async fn deploy(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        charm: &Charm,
        machines: &[String],
    ) -> Result<(), ActionError> {
        self.with(|state| {
            for machine in machines {
                state.place(model, &charm.application, machine);
            }
        });
        Ok(())
    }

    async fn add_units(
        &self,
        _endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
        machines: &[String],
    ) -> Result<(), ActionError> {
        self.with(|state| {
            for machine in machines {
                state.place(model, application, machine);
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
        self.with(|state| {
            for machine in state.machines_of(model) {
                machine.units.retain(|unit| !units.contains(unit));
            }
        });
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
            for machine in state.machines_of(model) {
                machine.units.retain(|unit| !unit.starts_with(&prefix));
            }
        });
        Ok(())
    }

    async fn expose(
        &self,
        _endpoint: &JujuEndpoint,
        _model: &str,
        application: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| state.exposed.push(application.to_owned()));
        Ok(())
    }

    async fn remove_authorized_key(
        &self,
        _endpoint: &JujuEndpoint,
        _model: &str,
        comment: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| state.removed_keys.push(comment.to_owned()));
        Ok(())
    }

    async fn update_kubernetes_credential(
        &self,
        _endpoint: &JujuEndpoint,
        token: &str,
    ) -> Result<(), ActionError> {
        self.with(|state| state.credential = Some(token.to_owned()));
        Ok(())
    }
}
