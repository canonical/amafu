mod connection;
mod facades;

use std::time::Duration;

use amafu_interfaces::{
    ActionError, Charm, JujuApplication, JujuControllerInfo, JujuEndpoint, JujuMachine, JujuModel,
    JujuPlatform,
};
use serde_json::{Value, json};

use crate::connection::{Connection, Scope};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_USER: &str = "user-amafu@amafu";
const BOOTSTRAP_USER: &str = "admin";

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

fn deploy_request(charm: &Charm, units: u16) -> Value {
    let mut argument = json!({
        "CharmName": charm.charm,
        "ApplicationName": charm.application,
        "num-units": units,
    });
    if let Some(channel) = &charm.channel {
        argument["channel"] = json!(channel);
    }
    json!({"Args": [argument]})
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

    async fn applications(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
    ) -> Result<Vec<JujuApplication>, ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let status = connection.call("Client", "FullStatus", json!({"patterns": []})).await?;
        Ok(facades::applications(&status))
    }

    async fn enable_ha(&self, endpoint: &JujuEndpoint, controllers: u8) -> Result<(), ActionError> {
        let mut connection = self.controller_connection(endpoint).await?;
        let request = json!({"specs": [{"num-controller-machines": controllers}]});
        connection.call("HighAvailability", "EnableHA", request).await?;
        Ok(())
    }

    async fn deploy(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        charm: &Charm,
        units: u16,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        connection.call("Application", "DeployFromRepository", deploy_request(charm, units)).await?;
        Ok(())
    }

    async fn add_units(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
        units: u16,
    ) -> Result<(), ActionError> {
        let mut connection = self.model_connection(endpoint, model).await?;
        let request = json!({"application": application, "num-units": units});
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
    fn a_deployment_names_its_charm_and_application_as_juju_spells_them() {
        let charm = Charm {
            application: String::from("db"),
            charm: String::from("postgresql"),
            channel: Some(String::from("14/stable")),
        };
        let argument = &deploy_request(&charm, 3)["Args"][0];
        assert_eq!(argument["CharmName"], "postgresql");
        assert_eq!(argument["ApplicationName"], "db");
        assert_eq!(argument["num-units"], 3);
        assert_eq!(argument["channel"], "14/stable");
        assert!(argument.get("Placement").is_none());
    }
}
