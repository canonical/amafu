use std::io;
use std::path::{Path, PathBuf};

use amafu_domain::kinds::node::{BootSlot, RefusedImage};
use amafu_domain::node_image::ImageIdentity;
use serde::{Deserialize, Serialize};

const HISTORY_FILE: &str = "boot-history.json";
const ATTEMPTS_PER_IMAGE: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trial {
    pub slot: BootSlot,
    pub image: ImageIdentity,
    #[serde(default)]
    pub has_booted: bool,
    #[serde(default)]
    pub attempt: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BootHistory {
    #[serde(default)]
    pub trial: Option<Trial>,
    #[serde(default)]
    pub interrupted: Option<Trial>,
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
        let Some(trial) = self.trial.take() else {
            return self;
        };
        if trial.slot == booted {
            self.trial = Some(Trial { has_booted: true, ..trial });
        } else if trial.has_booted && trial.attempt < ATTEMPTS_PER_IMAGE {
            self.interrupted = Some(trial);
        } else {
            self.fallen_back_from = Some(trial.image);
        }
        self
    }

    pub fn begin_trial(&mut self, slot: BootSlot, image: ImageIdentity) {
        if self.trial.as_ref().is_some_and(|trial| trial.slot == slot && trial.image == image) {
            return;
        }
        let attempt =
            self.interrupted.take().filter(|trial| trial.image == image).map_or(1, |trial| trial.attempt + 1);
        self.trial = Some(Trial { slot, image, has_booted: false, attempt });
        self.fallen_back_from = None;
    }

    pub fn abandon_trial(&mut self) {
        if let Some(trial) = self.trial.take() {
            self.fallen_back_from = Some(trial.image);
        }
        self.interrupted = None;
    }

    pub fn confirm_trial(&mut self) {
        self.trial = None;
        self.interrupted = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ImageIdentity {
        ImageIdentity { version: String::from("0.2.0"), digest: "e".repeat(64).parse().unwrap() }
    }

    fn trying(history: &mut BootHistory) {
        history.begin_trial(BootSlot::Second, identity());
    }

    #[test]
    fn a_trial_that_never_booted_falls_back() {
        let mut history = BootHistory::default();
        trying(&mut history);
        let fell_back = history.after_booting(BootSlot::First);
        assert_eq!(fell_back.trial, None);
        assert_eq!(fell_back.fallen_back_from, Some(identity()));
    }

    #[test]
    fn a_trial_reset_after_it_booted_is_tried_again_up_to_three_times() {
        let mut history = BootHistory::default();
        for attempt in 1..=3 {
            trying(&mut history);
            history = history.after_booting(BootSlot::Second).after_booting(BootSlot::First);
            let gave_up = attempt == 3;
            assert_eq!(history.fallen_back_from.is_some(), gave_up, "attempt {attempt}");
            assert_eq!(history.interrupted.is_some(), !gave_up, "attempt {attempt}");
        }
    }

    #[test]
    fn asking_again_for_the_same_trial_keeps_its_attempt() {
        let mut history = BootHistory::default();
        trying(&mut history);
        history = history.after_booting(BootSlot::Second).after_booting(BootSlot::First);
        trying(&mut history);
        trying(&mut history);
        assert_eq!(history.trial.as_ref().map(|trial| trial.attempt), Some(2));
    }

    #[test]
    fn an_abandoned_trial_counts_as_a_fall_back_and_a_confirmed_one_forgets_interruptions() {
        let mut history = BootHistory::default();
        trying(&mut history);
        history.abandon_trial();
        assert_eq!(history.fallen_back_from, Some(identity()));
        let mut confirmed = BootHistory::default();
        trying(&mut confirmed);
        confirmed = confirmed.after_booting(BootSlot::Second).after_booting(BootSlot::First);
        trying(&mut confirmed);
        confirmed.confirm_trial();
        assert_eq!(confirmed, BootHistory::default());
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
