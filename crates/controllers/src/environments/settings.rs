use amafu_domain::kinds::environment_settings::{
    EnvironmentSettings, EnvironmentSettingsSpecification, environment_settings_name,
};
use amafu_domain::kinds::image::ImageSource;
use amafu_domain::{ObjectReference, OciReference};
use amafu_interfaces::ActionError;

use super::EnvironmentObjects;

pub struct Settings(EnvironmentSettingsSpecification);

impl Settings {
    pub async fn read(objects: EnvironmentObjects<'_>) -> Result<Self, ActionError> {
        let reference = ObjectReference::of::<EnvironmentSettings>(None, environment_settings_name());
        let found = objects.read::<EnvironmentSettings>(&reference).await?;
        Ok(Self(found.map(|settings| settings.specification).unwrap_or_default()))
    }

    pub fn ubuntu_image(&self) -> Result<ImageSource, ActionError> {
        self.0.ubuntu_image.clone().ok_or_else(|| {
            ActionError::fatal(
                "no Ubuntu image is set for environments; run amafu environment-settings set --ubuntu-image ubuntu/24.04",
            )
        })
    }

    pub fn juju_runner_image(&self) -> Result<OciReference, ActionError> {
        self.0.juju_runner_image.clone().ok_or_else(|| {
            ActionError::fatal(
                "no Juju runner image is set for environments; run amafu environment-settings set --juju-runner-image REGISTRY/REPOSITORY@sha256:DIGEST",
            )
        })
    }
}
