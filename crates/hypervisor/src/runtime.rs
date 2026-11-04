use std::path::{Path, PathBuf};

use amafu_domain::ObjectId;

const PLAN_FILE: &str = "plan.json";
const PROCESS_ID_FILE: &str = "process-id";
const BOOTED_FILE: &str = "booted";
const SERIAL_LOG_FILE: &str = "serial.log";
const PROCESS_LOG_FILE: &str = "vm-process.log";
const USER_ID_FILE: &str = "user-id";
const VSOCK_SOCKET_FILE: &str = "vsock.socket";
const API_SOCKET_FILE: &str = "api.socket";
const EVENTS_FILE: &str = "events.json";
const INCOMING_SOCKET_FILE: &str = "migration-incoming.socket";
const OUTGOING_SOCKET_FILE: &str = "migration-outgoing.socket";
const RECEIVING_FILE: &str = "receiving";
const MIGRATION_FILE: &str = "migration.json";
const DISKS_DIRECTORY: &str = "disks";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDirectory {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmDirectory {
    path: PathBuf,
}

impl RuntimeDirectory {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn vm(&self, vm_id: &ObjectId) -> VmDirectory {
        VmDirectory { path: self.root.join(vm_id.to_string()) }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vms(&self) -> std::io::Result<Vec<(ObjectId, VmDirectory)>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut vms = Vec::new();
        for entry in entries {
            let entry = entry?;
            if let Some(vm_id) = entry.file_name().to_str().and_then(|name| name.parse::<ObjectId>().ok()) {
                vms.push((vm_id, VmDirectory { path: entry.path() }));
            }
        }
        vms.sort_by_key(|(vm_id, _)| *vm_id);
        Ok(vms)
    }
}

impl VmDirectory {
    #[must_use]
    pub fn at(path: &Path) -> Self {
        Self { path: path.to_path_buf() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn plan_file(&self) -> PathBuf {
        self.path.join(PLAN_FILE)
    }

    #[must_use]
    pub fn process_id_file(&self) -> PathBuf {
        self.path.join(PROCESS_ID_FILE)
    }

    #[must_use]
    pub fn booted_file(&self) -> PathBuf {
        self.path.join(BOOTED_FILE)
    }

    #[must_use]
    pub fn serial_log(&self) -> PathBuf {
        self.path.join(SERIAL_LOG_FILE)
    }

    #[must_use]
    pub fn process_log(&self) -> PathBuf {
        self.path.join(PROCESS_LOG_FILE)
    }

    #[must_use]
    pub fn vsock_socket(&self) -> PathBuf {
        self.path.join(VSOCK_SOCKET_FILE)
    }

    #[must_use]
    pub fn api_socket(&self) -> PathBuf {
        self.path.join(API_SOCKET_FILE)
    }

    #[must_use]
    pub fn events_file(&self) -> PathBuf {
        self.path.join(EVENTS_FILE)
    }

    #[must_use]
    pub fn incoming_socket(&self) -> PathBuf {
        self.path.join(INCOMING_SOCKET_FILE)
    }

    #[must_use]
    pub fn outgoing_socket(&self) -> PathBuf {
        self.path.join(OUTGOING_SOCKET_FILE)
    }

    #[must_use]
    pub fn receiving_file(&self) -> PathBuf {
        self.path.join(RECEIVING_FILE)
    }

    #[must_use]
    pub fn migration_file(&self) -> PathBuf {
        self.path.join(MIGRATION_FILE)
    }

    #[must_use]
    pub fn disk_link(&self, index: usize) -> PathBuf {
        self.path.join(DISKS_DIRECTORY).join(index.to_string())
    }

    #[must_use]
    pub fn is_receiving(&self) -> bool {
        self.receiving_file().is_file() && !self.has_booted()
    }

    #[must_use]
    pub fn user_id(&self) -> Option<u32> {
        std::fs::read_to_string(self.path.join(USER_ID_FILE)).ok()?.trim().parse().ok()
    }

    pub fn record_user_id(&self, user_id: u32) -> std::io::Result<()> {
        std::fs::write(self.path.join(USER_ID_FILE), user_id.to_string())
    }

    #[must_use]
    pub fn process_id(&self) -> Option<i32> {
        std::fs::read_to_string(self.process_id_file()).ok()?.trim().parse().ok()
    }

    #[must_use]
    pub fn has_booted(&self) -> bool {
        self.booted_file().is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_directories_are_found_by_identifier() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path().join("vms"));
        assert_eq!(runtime.vms().unwrap(), Vec::new());
        let vm_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let directory = runtime.vm(&vm_id);
        std::fs::create_dir_all(directory.path()).unwrap();
        std::fs::create_dir_all(root.path().join("vms").join("not-a-vm")).unwrap();
        std::fs::write(directory.process_id_file(), "4242\n").unwrap();
        let vms = runtime.vms().unwrap();
        assert_eq!(vms.len(), 1);
        assert_eq!(vms[0].0, vm_id);
        assert_eq!(vms[0].1.process_id(), Some(4242));
        assert!(!vms[0].1.has_booted());
    }
}
