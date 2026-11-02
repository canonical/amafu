use std::path::Path;

const SECURE_BOOT_VARIABLE: &str = "SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";
const VARIABLE_ATTRIBUTE_BYTES: usize = 4;

#[must_use]
pub fn is_secure_boot_enabled(efivars: &Path) -> bool {
    std::fs::read(efivars.join(SECURE_BOOT_VARIABLE))
        .ok()
        .and_then(|contents| contents.get(VARIABLE_ATTRIBUTE_BYTES).copied())
        .is_some_and(|value| value == 1)
}

#[must_use]
pub fn is_tpm_present(sysfs: &Path) -> bool {
    sysfs.join("class/tpm/tpm0").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_boot_is_read_after_the_attribute_bytes() {
        let efivars = tempfile::tempdir().unwrap();
        assert!(!is_secure_boot_enabled(efivars.path()));
        std::fs::write(efivars.path().join(SECURE_BOOT_VARIABLE), [6, 0, 0, 0, 1]).unwrap();
        assert!(is_secure_boot_enabled(efivars.path()));
        std::fs::write(efivars.path().join(SECURE_BOOT_VARIABLE), [6, 0, 0, 0, 0]).unwrap();
        assert!(!is_secure_boot_enabled(efivars.path()));
    }
}
