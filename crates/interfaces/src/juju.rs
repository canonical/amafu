use std::net::Ipv4Addr;

use crate::error::ActionError;

pub const JUJU_API_PORT: u16 = 17070;

#[must_use]
pub fn cloud_name(controller: &str) -> String {
    format!("amafu-{controller}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuEndpoint {
    pub address: Ipv4Addr,
    pub certificate_authority: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuControllerInfo {
    pub uuid: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuModel {
    pub name: String,
    pub uuid: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuMachine {
    pub id: String,
    pub instance_id: String,
    pub is_started: bool,
    pub units: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualMachine {
    pub instance_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Charm {
    pub application: String,
    pub charm: String,
    pub channel: Option<String>,
}

#[async_trait::async_trait]
pub trait JujuPlatform: Send + Sync {
    async fn controller(&self, endpoint: &JujuEndpoint) -> Result<Option<JujuControllerInfo>, ActionError>;

    async fn models(&self, endpoint: &JujuEndpoint) -> Result<Vec<JujuModel>, ActionError>;

    async fn add_model(&self, endpoint: &JujuEndpoint, name: &str) -> Result<JujuModel, ActionError>;

    async fn destroy_model(&self, endpoint: &JujuEndpoint, model: &str) -> Result<(), ActionError>;

    async fn upgrade_model(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        version: &str,
    ) -> Result<(), ActionError>;

    async fn machines(&self, endpoint: &JujuEndpoint, model: &str) -> Result<Vec<JujuMachine>, ActionError>;

    async fn add_machine(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &ManualMachine,
    ) -> Result<String, ActionError>;

    async fn provisioning_script(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &str,
        nonce: &str,
    ) -> Result<String, ActionError>;

    async fn remove_machine(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        machine: &str,
    ) -> Result<(), ActionError>;

    async fn enable_ha(&self, endpoint: &JujuEndpoint, machines: &[String]) -> Result<(), ActionError>;

    async fn deploy(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        charm: &Charm,
        machines: &[String],
    ) -> Result<(), ActionError>;

    async fn add_units(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
        machines: &[String],
    ) -> Result<(), ActionError>;

    async fn remove_units(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        units: &[String],
    ) -> Result<(), ActionError>;

    async fn remove_application(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError>;

    async fn expose(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        application: &str,
    ) -> Result<(), ActionError>;

    async fn remove_authorized_key(
        &self,
        endpoint: &JujuEndpoint,
        model: &str,
        comment: &str,
    ) -> Result<(), ActionError>;

    async fn update_kubernetes_credential(
        &self,
        endpoint: &JujuEndpoint,
        cloud: &str,
        token: &str,
    ) -> Result<(), ActionError>;
}
