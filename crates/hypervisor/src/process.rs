use std::path::Path;

use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};

pub fn reap(process_id: i32) {
    if let Some(process) = Pid::from_raw(process_id) {
        let _ = waitpid(Some(process), WaitOptions::NOHANG);
    }
}

fn is_zombie(stat: &str) -> bool {
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

#[must_use]
pub fn is_running_vm_process(process_id: i32, directory: &Path) -> bool {
    reap(process_id);
    let process = Path::new("/proc").join(process_id.to_string());
    let Ok(command_line) = std::fs::read(process.join("cmdline")) else {
        return false;
    };
    let Ok(stat) = std::fs::read_to_string(process.join("stat")) else {
        return false;
    };
    let marker = directory.as_os_str().as_encoded_bytes();
    let is_ours = command_line.split(|byte| *byte == 0).any(|argument| argument == marker);
    is_ours && !is_zombie(&stat)
}

pub fn send_signal(process_id: i32, signal: Signal) -> std::io::Result<()> {
    let process =
        Pid::from_raw(process_id).ok_or_else(|| std::io::Error::other("invalid process identifier"))?;
    match kill_process(process, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zombies_are_recognized_from_their_stat_line() {
        assert!(is_zombie("4242 (amafu node) Z 1 4242 4242 0 -1"));
        assert!(!is_zombie("4242 (amafu node) S 1 4242 4242 0 -1"));
        assert!(!is_zombie("garbage"));
    }

    #[test]
    fn a_process_that_does_not_carry_the_directory_is_not_a_vm_process() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!is_running_vm_process(i32::try_from(std::process::id()).unwrap(), directory.path()));
    }
}
