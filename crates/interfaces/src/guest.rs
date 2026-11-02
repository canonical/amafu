use std::collections::BTreeMap;

use amafu_domain::ObjectId;
use serde::{Deserialize, Serialize};

use crate::error::ActionError;
use crate::images::ContainerConfiguration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadSpecification {
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    pub user: Option<String>,
    pub hostname: String,
}

impl WorkloadSpecification {
    #[must_use]
    pub fn from_image(
        image: &ContainerConfiguration,
        command: &[String],
        environment: &BTreeMap<String, String>,
        hostname: &str,
    ) -> Self {
        let mut arguments = image.entrypoint.clone();
        arguments.extend(if command.is_empty() { image.command.clone() } else { command.to_vec() });
        let mut merged = image.environment.clone();
        merged.extend(environment.iter().map(|(name, value)| (name.clone(), value.clone())));
        Self {
            arguments,
            environment: merged,
            working_directory: image.working_directory.clone(),
            user: image.user.clone(),
            hostname: hostname.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum WorkloadState {
    NotStarted,
    Running,
    Exited { code: i32 },
}

#[async_trait::async_trait]
pub trait GuestAgents: Send + Sync {
    async fn workload(&self, vm_id: &ObjectId) -> Result<WorkloadState, ActionError>;

    async fn start_workload(
        &self,
        vm_id: &ObjectId,
        workload: &WorkloadSpecification,
    ) -> Result<(), ActionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_replaces_the_image_command_and_the_environment_overrides() {
        let image = ContainerConfiguration {
            entrypoint: vec![String::from("/bin/server")],
            command: vec![String::from("--default")],
            environment: BTreeMap::from([
                (String::from("MODE"), String::from("image")),
                (String::from("PATH"), String::from("/bin")),
            ]),
            working_directory: Some(String::from("/srv")),
            user: None,
        };
        let workload = WorkloadSpecification::from_image(
            &image,
            &[String::from("--port"), String::from("80")],
            &BTreeMap::from([(String::from("MODE"), String::from("vm"))]),
            "web-1",
        );
        assert_eq!(workload.arguments, vec!["/bin/server", "--port", "80"]);
        assert_eq!(workload.environment["MODE"], "vm");
        assert_eq!(workload.environment["PATH"], "/bin");
        let defaults = WorkloadSpecification::from_image(&image, &[], &BTreeMap::new(), "web-1");
        assert_eq!(defaults.arguments, vec!["/bin/server", "--default"]);
    }
}
