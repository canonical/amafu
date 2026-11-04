use std::io;
use std::path::{Path, PathBuf};

use amafu_domain::kinds::node::{BootSlot, RefusedImage};
use amafu_domain::node_image::ImageIdentity;
use serde::{Deserialize, Serialize};

const HISTORY_FILE: &str = "boot-history.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trial {
    pub slot: BootSlot,
    pub image: ImageIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BootHistory {
    #[serde(default)]
    pub trial: Option<Trial>,
    #[serde(default)]
    pub fallen_back_from: Option<ImageIdentity>,
    #[serde(default)]
    pub refused: Option<RefusedImage>,
}

impl BootHistory {
    #[must_use]
    pub fn path(data_directory: &Path) -> PathBuf {
        data_directory.join(HISTORY_FILE)
    }

    #[must_use]
    pub fn load(data_directory: &Path) -> Self {
        std::fs::read(Self::path(data_directory))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_directory: &Path) -> io::Result<()> {
        let path = Self::path(data_directory);
        let temporary = path.with_extension("partial");
        std::fs::write(&temporary, serde_json::to_vec(self).map_err(io::Error::other)?)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(temporary, path)
    }

    #[must_use]
    pub fn after_booting(mut self, booted: BootSlot) -> Self {
        if let Some(trial) = self.trial.take_if(|trial| trial.slot != booted) {
            self.fallen_back_from = Some(trial.image);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ImageIdentity {
        ImageIdentity { version: String::from("0.2.0"), digest: "e".repeat(64).parse().unwrap() }
    }

    #[test]
    fn booting_another_slot_than_the_trial_means_it_fell_back() {
        let trying = BootHistory {
            trial: Some(Trial { slot: BootSlot::Second, image: identity() }),
            ..BootHistory::default()
        };
        let fell_back = trying.clone().after_booting(BootSlot::First);
        assert_eq!(fell_back.trial, None);
        assert_eq!(fell_back.fallen_back_from, Some(identity()));
        let on_trial = trying.clone().after_booting(BootSlot::Second);
        assert_eq!(on_trial, trying);
    }

    #[test]
    fn the_history_survives_a_restart() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(BootHistory::load(directory.path()), BootHistory::default());
        let history = BootHistory { fallen_back_from: Some(identity()), ..BootHistory::default() };
        history.save(directory.path()).unwrap();
        assert_eq!(BootHistory::load(directory.path()), history);
    }
}
