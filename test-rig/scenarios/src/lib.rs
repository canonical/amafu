pub mod containers;
pub mod security;
pub mod storage;
pub mod support;
pub mod walking_skeleton;

use amafu_scenario::ScenarioDefinition;

#[must_use]
pub fn all() -> Vec<ScenarioDefinition> {
    vec![
        walking_skeleton::DEFINITION,
        walking_skeleton::RESTART_AFTER_POWER_CYCLE,
        walking_skeleton::MISSING_IMAGE_IS_REPORTED,
        security::AUTHORIZATION_MATRIX,
        security::CREDENTIALS_ARE_CHECKED,
        containers::CONTAINER_RUNS,
        containers::CONTAINER_EXIT_IS_REPORTED,
        storage::VM_BOOTS_FROM_A_CLONED_VOLUME,
        storage::NFS_VOLUME_IS_ATTACHED,
    ]
}

#[must_use]
pub fn named(name: &str) -> Option<ScenarioDefinition> {
    all().into_iter().find(|definition| definition.name == name)
}
