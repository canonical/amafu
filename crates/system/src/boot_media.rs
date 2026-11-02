use std::fs;
use std::path::{Path, PathBuf};

pub const BOOT_PARTITION_NAME: &str = "amafu-boot";
pub const DATA_PARTITION_NAME: &str = "amafu-data";

fn partition_name(uevent: &str) -> Option<&str> {
    uevent.lines().find_map(|line| line.strip_prefix("PARTNAME="))
}

fn device_name(uevent: &str) -> Option<&str> {
    uevent.lines().find_map(|line| line.strip_prefix("DEVNAME="))
}

#[must_use]
pub fn find_partition(sysfs_block: &Path, name: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(sysfs_block).ok()?.filter_map(Result::ok).map(|entry| entry.path()).collect();
    entries.sort();
    entries.iter().find_map(|entry| {
        let uevent = fs::read_to_string(entry.join("uevent")).ok()?;
        if partition_name(&uevent) != Some(name) {
            return None;
        }
        device_name(&uevent).map(|device| Path::new("/dev").join(device))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_are_found_by_their_gpt_name() {
        let sysfs = tempfile::tempdir().unwrap();
        for (device, name) in [("vda", None), ("vda1", Some("amafu-boot")), ("vda2", Some("amafu-data"))] {
            let directory = sysfs.path().join(device);
            fs::create_dir_all(&directory).unwrap();
            let partition = name.map(|name| format!("PARTNAME={name}\n")).unwrap_or_default();
            let uevent = format!("MAJOR=253\nDEVNAME={device}\nDEVTYPE=partition\n{partition}");
            fs::write(directory.join("uevent"), uevent).unwrap();
        }
        assert_eq!(find_partition(sysfs.path(), BOOT_PARTITION_NAME), Some(PathBuf::from("/dev/vda1")));
        assert_eq!(find_partition(sysfs.path(), DATA_PARTITION_NAME), Some(PathBuf::from("/dev/vda2")));
        assert_eq!(find_partition(sysfs.path(), "missing"), None);
    }
}
