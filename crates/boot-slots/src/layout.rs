use std::path::{Path, PathBuf};

use amafu_domain::kinds::node::BootSlot;

const SLOT_PREFIX: &str = "amafu-";
const SHARED_DIRECTORY: &str = "amafu";
const ENVIRONMENT_FILE: &str = "grubenv";
const MANIFEST_FILE: &str = "node-image.json";
const INCOMING_SUFFIX: &str = ".incoming";
const SLOT_ARGUMENT: &str = "amafu.slot=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspLayout {
    root: PathBuf,
}

impl EspLayout {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn slot_directory(&self, slot: BootSlot) -> PathBuf {
        self.root.join(format!("{SLOT_PREFIX}{}", slot.letter()))
    }

    #[must_use]
    pub fn incoming_directory(&self, slot: BootSlot) -> PathBuf {
        self.root.join(format!("{SLOT_PREFIX}{}{INCOMING_SUFFIX}", slot.letter()))
    }

    #[must_use]
    pub fn manifest(&self, slot: BootSlot) -> PathBuf {
        self.slot_directory(slot).join(MANIFEST_FILE)
    }

    #[must_use]
    pub fn environment(&self) -> PathBuf {
        self.root.join(SHARED_DIRECTORY).join(ENVIRONMENT_FILE)
    }

    #[must_use]
    pub fn shared_directory(&self) -> PathBuf {
        self.root.join(SHARED_DIRECTORY)
    }
}

#[must_use]
pub fn booted_slot(command_line: &str) -> BootSlot {
    command_line
        .split_whitespace()
        .find_map(|argument| argument.strip_prefix(SLOT_ARGUMENT))
        .and_then(BootSlot::from_letter)
        .unwrap_or(BootSlot::First)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_booted_slot_comes_from_the_kernel_command_line() {
        assert_eq!(booted_slot("console=ttyS0 amafu.slot=b quiet"), BootSlot::Second);
        assert_eq!(booted_slot("console=ttyS0 amafu.slot=a"), BootSlot::First);
        assert_eq!(booted_slot("console=ttyS0"), BootSlot::First);
    }

    #[test]
    fn each_slot_has_its_own_directory_beside_the_shared_one() {
        let layout = EspLayout::new("/run/amafu/esp");
        assert_eq!(layout.slot_directory(BootSlot::Second), PathBuf::from("/run/amafu/esp/amafu-b"));
        assert_eq!(layout.manifest(BootSlot::First), PathBuf::from("/run/amafu/esp/amafu-a/node-image.json"));
        assert_eq!(layout.environment(), PathBuf::from("/run/amafu/esp/amafu/grubenv"));
        assert_eq!(
            layout.incoming_directory(BootSlot::First),
            PathBuf::from("/run/amafu/esp/amafu-a.incoming")
        );
    }
}
