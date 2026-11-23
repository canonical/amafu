use amafu_domain::ObjectName;
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::labels::JUJU_MACHINE;
use amafu_interfaces::{ActionError, JujuEndpoint, JujuMachine, MachineHardware, ManualMachine};

use super::scope::{ControllerScope, UBUNTU_IMAGE, substrate_network};
use crate::environments::{Machine, MachinePlan};

pub struct ProvisionedMachine<'a> {
    scope: &'a ControllerScope<'a>,
    machine: Machine<'a>,
    instance_id: String,
}

impl<'a> ProvisionedMachine<'a> {
    pub fn new(scope: &'a ControllerScope<'a>, suffix: &str) -> Result<Self, ActionError> {
        let machine = scope.machine(suffix)?;
        let instance_id = format!("manual:{}", machine.name());
        Ok(Self { scope, machine, instance_id })
    }

    #[must_use]
    pub fn machine(&self) -> &Machine<'a> {
        &self.machine
    }

    #[must_use]
    pub fn in_juju<'m>(&self, machines: &'m [JujuMachine]) -> Option<&'m JujuMachine> {
        machines.iter().find(|machine| machine.instance_id == self.instance_id)
    }

    pub async fn juju_id(&self) -> Result<Option<String>, ActionError> {
        Ok(self.machine.vm().await?.and_then(|vm| vm.metadata.labels.get(JUJU_MACHINE).cloned()))
    }

    pub async fn provision(
        &self,
        endpoint: &JujuEndpoint,
        model: &ObjectName,
        model_uuid: &str,
        shape: MachineShape,
        application: Option<&ObjectName>,
    ) -> Result<(), ActionError> {
        if self.machine.vm().await?.is_some() {
            return Ok(());
        }
        let platform = &self.scope.environment.platform;
        let nonce = format!("amafu:{}", self.machine.name());
        let known = platform.machines(endpoint, model_uuid).await?;
        let id = if let Some(found) = self.in_juju(&known) {
            found.id.clone()
        } else {
            let manual = ManualMachine {
                instance_id: self.instance_id.clone(),
                nonce: nonce.clone(),
                hardware: MachineHardware::from(shape),
            };
            platform.add_machine(endpoint, model_uuid, &manual).await?
        };
        let user_data = platform.provisioning_script(endpoint, model_uuid, &id, &nonce).await?;
        let specification = self.scope.specification().await?;
        let (network, storage_backend) = substrate_network(&specification)?;
        self.machine
            .create(MachinePlan {
                shape,
                image: ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?,
                network: network.clone(),
                storage_backend: storage_backend.clone(),
                security_groups: vec![self.scope.named("machines")?],
                user_data,
                labels: ControllerScope::machine_labels(model, &id, application),
            })
            .await
    }

    pub async fn is_started(&self, machines: &[JujuMachine]) -> Result<bool, ActionError> {
        Ok(self.machine.vm().await?.is_some()
            && self.in_juju(machines).is_some_and(|machine| machine.is_started))
    }
}
