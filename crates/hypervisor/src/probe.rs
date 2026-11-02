use std::io::Write;
use std::path::PathBuf;

use rustix::mount::{MountFlags, mount};
use rustix::net::{AddressFamily, SocketType, socket};
use serde::{Deserialize, Serialize};

pub const PERMITTED_EXIT_CODE: i32 = 0;
pub const DENIED_EXIT_CODE: i32 = 3;
const MEBIBYTE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Attempt {
    ReadFile { path: PathBuf },
    WriteFile { path: PathBuf },
    OpenInternetSocket,
    OpenNetlinkSocket,
    Mount,
    LoadModule,
    AllocateMemory { mebibytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum ProbeOutcome {
    Permitted,
    Denied { reason: String },
    Killed { signal: i32 },
}

impl ProbeOutcome {
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !matches!(self, Self::Permitted)
    }
}

fn denial(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn allocate(mebibytes: usize) {
    let mut held: Vec<Vec<u8>> = Vec::new();
    for _ in 0..mebibytes {
        held.push(vec![1_u8; MEBIBYTE]);
    }
    std::hint::black_box(&held);
}

pub fn attempt(attempt: &Attempt) -> Result<(), String> {
    match attempt {
        Attempt::ReadFile { path } => std::fs::read(path).map(drop).map_err(denial),
        Attempt::WriteFile { path } => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(b"probe"))
            .map_err(denial),
        Attempt::OpenInternetSocket => {
            socket(AddressFamily::INET, SocketType::STREAM, None).map(drop).map_err(denial)
        }
        Attempt::OpenNetlinkSocket => {
            socket(AddressFamily::NETLINK, SocketType::RAW, None).map(drop).map_err(denial)
        }
        Attempt::Mount => mount("none", "/tmp", "tmpfs", MountFlags::empty(), None).map_err(denial),
        Attempt::LoadModule => rustix::system::init_module(&[], c"").map_err(denial),
        Attempt::AllocateMemory { mebibytes } => {
            allocate(*mebibytes);
            Ok(())
        }
    }
}

pub fn outcome_of(status: std::process::ExitStatus, output: &str) -> Result<ProbeOutcome, String> {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(PERMITTED_EXIT_CODE), _) => Ok(ProbeOutcome::Permitted),
        (Some(DENIED_EXIT_CODE), _) => Ok(ProbeOutcome::Denied { reason: output.trim().to_owned() }),
        (_, Some(signal)) => Ok(ProbeOutcome::Killed { signal }),
        _ => Err(format!("the probe failed before its attempt: {}", output.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsandboxed_process_may_read_and_write_its_own_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("probe");
        assert!(attempt(&Attempt::WriteFile { path: path.clone() }).is_ok());
        assert!(attempt(&Attempt::ReadFile { path }).is_ok());
        assert!(attempt(&Attempt::ReadFile { path: directory.path().join("missing") }).is_err());
    }
}
