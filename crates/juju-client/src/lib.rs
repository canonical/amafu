mod connection;
mod facades;

use std::time::Duration;

use amafu_interfaces::{
    ActionError, Charm, JujuControllerInfo, JujuEndpoint, JujuMachine, JujuModel, JujuPlatform, ManualMachine,
};
use serde_json::{Value, json};

use crate::connection::{Connection, Scope};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_USER: &str = "user-amafu@amafu";
const BOOTSTRAP_USER: &str = "admin";
const MACHINE_BASE: (&str, &str) = ("ubuntu", "24.04");
const MACHINE_ARCHITECTURE: &str = "amd64";

#[derive(Debug, Clone, Copy)]
pub struct WebSocketJuju {
    timeout: Duration,
}

impl Default for WebSocketJuju {
    fn default() -> Self {
        Self { timeout: REQUEST_TIMEOUT }
    }
}

impl WebSocketJuju {
    async fn controller_connection(&self, endpoint: &JujuEndpoint) -> Result<Connection, ActionError> {
        Connection::open(endpoint, Scope::Controller, self.timeout).await
    }

    async fn model_connection(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
    ) -> Result<Connection, ActionError> {
        Connection::open(endpoint, Scope::Model(model), self.timeout).await
    }

    fn cloud_credential(cloud: &str) -> String {
        format!("cloudcred-{cloud}_{BOOTSTRAP_USER}_{cloud}")
    }
}

fn add_machine_request(machine: &ManualMachine) -> Value {
    let (name, channel) = MACHINE_BASE;
    json!({"params": [{
        "jobs": ["JobHostUnits"],
        "instance-id": machine.instance_id,
        "nonce": machine.nonce,
        "base": {"name": name, "channel": channel},
        "hardware-characteristics": {
            "arch": MACHINE_ARCHITECTURE,
            "cpu-cores": machine.hardware.cpu_cores,
            "mem": machine.hardware.memory_mebibytes,
            "root-disk": machine.hardware.root_disk_mebibytes,
        },
    }]})
}

#[async_trait::async_trait]
impl JujuPlatform for WebSocketJuju {
    async fn controller(&self, endpoint: &JujuEndpoint) -> Result<Option<JujuControllerInfo>, ActionError> {
        let connection = match self.controller_connection(endpoint).await {
            Ok(connection) => connection,
            Err(ActionError::Retryable(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let login = connection.login();
        Ok(Some(JujuControllerInfo {
            uuid: login.controller_uuid.clone(),
            version: login.server_version.clone(),
        }))
    }

    async fn models(&self, endpoint: &JujuEndpoint) -> Result<Vec<JujuModel>, ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let listing = connection.call("ModelManager", "ListModels", json!({"tag": SERVICE_USER})).await?;
        let details = connection.call("ModelManager", "ModelInfo", facades::model_tags(&listing)).await?;
        Ok(facades::models(&listing, &details))
    }

    async fn add_model(&self, endpoint: &JujuEndpoint, name: &str) -> Result<JujuModel, ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let created = connection
            .call("ModelManager", "CreateModel", json!({"name": name, "owner-tag": SERVICE_USER}))
            .await?;
        Ok(JujuModel {
            name: name.to_owned(),
            uuid: created["uuid"].as_str().unwrap_or_default().to_owned(),
            version: created["agent-version"].as_str().unwrap_or_default().to_owned(),
        })
    }

    async fn destroy_model(&self, endpoint: &JujuEndpoint, model: &str) -> Result<(), ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let models = json!({"models": [{"model-tag": format!("model-{model}"), "destroy-storage": true}]});
        connection.call("ModelManager", "DestroyModels", models).await?;
        Ok(())
    }

    async fn upgrade_model(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        version: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let upgrade = json!({"model-tag": format!("model-{model}"), "target-version": version});
        connection.call("ModelUpgrader", "UpgradeModel", upgrade).await?;
        Ok(())
    }

    async fn machines(&self, endpoint: &JujuEndpoint, model: &str) -> Result<Vec<JujuMachine>, ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let status = connection.call("Client", "FullStatus", json!({"patterns": []})).await?;
        Ok(facades::machines(&status))
    }

    async fn add_machine(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &ManualMachine,
    ) -> Result<String, ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = add_machine_request(machine);
        let added = connection.call("MachineManager", "AddMachines", request).await?;
        added["machines"][0]["machine"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ActionError::retryable("the controller returned no machine"))
    }

    async fn provisioning_script(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &str,
        nonce: &str,
    ) -> Result<String, ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = json!({"machine-id": machine, "nonce": nonce, "disable-package-commands": false});
        let script = connection.call("MachineManager", "ProvisioningScript", request).await?;
        let body =
            script["script"].as_str().ok_or_else(|| ActionError::retryable("no provisioning script"))?;
        Ok(format!("#!/bin/bash\n{body}"))
    }

    async fn remove_machine(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = json!({"machine-tags": [format!("machine-{machine}")], "force": true, "keep": false});
        connection.call("MachineManager", "DestroyMachineWithParams", request).await?;
        Ok(())
    }

    async fn enable_ha(&self, endpoint: &JujuEndpoint, machines: &[String]) -> Result<(), ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let count = machines.len() + 1;
        let request = json!({"specs": [{"num-controller-machines": count, "placement": machines}]});
        connection.call("HighAvailability", "EnableHA", request).await?;
        Ok(())
    }

    async fn deploy(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        charm: &Charm,
        machines: &[String],
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let mut argument = json!({
            "charm-name": charm.charm,
            "application-name": charm.application,
            "num-units": machines.len(),
            "placement": facades::placements(machines),
        });
        if let Some(channel) = &charm.channel {
            argument["channel"] = json!(channel);
        }
        connection.call("Application", "DeployFromRepository", json!({"Args": [argument]})).await?;
        Ok(())
    }

    async fn add_units(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
        machines: &[String],
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = json!({
            "application": application,
            "num-units": machines.len(),
            "placement": facades::placements(machines),
        });
        connection.call("Application", "AddUnits", request).await?;
        Ok(())
    }

    async fn remove_units(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        units: &[String],
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let tags: Vec<_> = units.iter().map(|unit| json!({"unit-tag": facades::unit_tag(unit)})).collect();
        connection.call("Application", "DestroyUnit", json!({"units": tags})).await?;
        Ok(())
    }

    async fn remove_application(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = json!({"applications": [{"application-tag": format!("application-{application}")}]});
        connection.call("Application", "DestroyApplication", request).await?;
        Ok(())
    }

    async fn expose(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        connection.call("Application", "Expose", json!({"application": application})).await?;
        Ok(())
    }

    async fn remove_authorized_key(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        comment: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        connection
            .call("KeyManager", "DeleteKeys", json!({"user": BOOTSTRAP_USER, "keys": [comment]}))
            .await?;
        Ok(())
    }

    async fn update_kubernetes_credential(
        &self,
        endpoint: &JujuEndpoint,
        cloud: &str,
        token: &str,
    ) -> Result<(), ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let request = json!({"credentials": [{
            "tag": Self::cloud_credential(cloud),
            "credential": {"auth-type": "oauth2", "attrs": {"Token": token}},
        }]});
        connection.call("Cloud", "UpdateCredentialsCheckModels", request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_machine_is_added_with_its_hardware() {
        let machine = ManualMachine {
            instance_id: String::from("manual:apps-db-0"),
            nonce: String::from("amafu:apps-db-0"),
            hardware: amafu_interfaces::MachineHardware {
                cpu_cores: 2,
                memory_mebibytes: 4096,
                root_disk_mebibytes: 20480,
            },
        };
        let hardware = &add_machine_request(&machine)["params"][0]["hardware-characteristics"];
        assert_eq!(hardware, &json!({"arch": "amd64", "cpu-cores": 2, "mem": 4096, "root-disk": 20480}));
    }
}
