use amafu_domain::kinds::node::BootSlot;

pub const BLOCK_SIZE: usize = 1024;
const HEADER: &str = "# GRUB Environment Block\n";
const DEFAULT_SLOT: &str = "default_slot";
const NEXT_SLOT: &str = "next_slot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrubEnvironment {
    pub default_slot: BootSlot,
    pub next_slot: Option<BootSlot>,
}

impl Default for GrubEnvironment {
    fn default() -> Self {
        Self { default_slot: BootSlot::First, next_slot: None }
    }
}

impl GrubEnvironment {
    #[must_use]
    pub fn parse(block: &str) -> Self {
        let value = |key: &str| {
            block
                .lines()
                .filter(|line| !line.starts_with('#'))
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .and_then(BootSlot::from_letter)
        };
        Self { default_slot: value(DEFAULT_SLOT).unwrap_or(BootSlot::First), next_slot: value(NEXT_SLOT) }
    }

    #[must_use]
    pub fn render(&self) -> Vec<u8> {
        let next = self.next_slot.map_or("", BootSlot::letter);
        let mut block =
            format!("{HEADER}{DEFAULT_SLOT}={}\n{NEXT_SLOT}={next}\n", self.default_slot.letter())
                .into_bytes();
        block.resize(BLOCK_SIZE, b'#');
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_padded_to_the_size_grub_expects_and_reads_back() {
        let environment =
            GrubEnvironment { default_slot: BootSlot::Second, next_slot: Some(BootSlot::First) };
        let block = environment.render();
        assert_eq!(block.len(), BLOCK_SIZE);
        assert!(block.starts_with(HEADER.as_bytes()));
        assert_eq!(GrubEnvironment::parse(&String::from_utf8(block).unwrap()), environment);
    }

    #[test]
    fn a_missing_or_garbled_block_boots_the_first_slot() {
        assert_eq!(GrubEnvironment::parse(""), GrubEnvironment::default());
        let written_by_grub = format!("{HEADER}default_slot=b\nnext_slot=\n{}", "#".repeat(900));
        assert_eq!(
            GrubEnvironment::parse(&written_by_grub),
            GrubEnvironment { default_slot: BootSlot::Second, next_slot: None }
        );
        assert_eq!(GrubEnvironment::parse("default_slot=z\n").default_slot, BootSlot::First);
    }
}
