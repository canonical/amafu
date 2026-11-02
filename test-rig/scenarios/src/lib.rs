pub mod security;
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
    ]
}

#[must_use]
pub fn named(name: &str) -> Option<ScenarioDefinition> {
    all().into_iter().find(|definition| definition.name == name)
}
