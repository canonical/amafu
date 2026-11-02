use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use amafu_interfaces::{WorkloadSpecification, WorkloadState};
use anyhow::{Context, bail};
use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};

const PASSWORD_FILE: &str = "/etc/passwd";
const GROUP_FILE: &str = "/etc/group";
const SIGNAL_EXIT_OFFSET: i32 = 128;

#[derive(Debug, Default)]
pub struct Workload {
    process_id: Option<i32>,
    exit_code: Option<i32>,
}

fn lookup(file: &str, name: &str, field: usize) -> Option<u32> {
    std::fs::read_to_string(file).ok()?.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.first() == Some(&name)).then(|| fields.get(field)?.parse().ok()).flatten()
    })
}

fn resolve_user(user: Option<&str>) -> anyhow::Result<(u32, u32)> {
    let Some(user) = user else {
        return Ok((0, 0));
    };
    let (user_part, group_part) =
        user.split_once(':').map_or((user, None), |(user, group)| (user, Some(group)));
    let user_id = user_part
        .parse()
        .ok()
        .or_else(|| lookup(PASSWORD_FILE, user_part, 2))
        .context("the user is unknown")?;
    let group_id = match group_part {
        Some(group) => {
            group.parse().ok().or_else(|| lookup(GROUP_FILE, group, 2)).context("the group is unknown")?
        }
        None => lookup(PASSWORD_FILE, user_part, 3).unwrap_or(user_id),
    };
    Ok((user_id, group_id))
}

impl Workload {
    pub fn start(&mut self, specification: &WorkloadSpecification) -> anyhow::Result<()> {
        if self.process_id.is_some() {
            return Ok(());
        }
        let Some((program, arguments)) = specification.arguments.split_first() else {
            bail!("the workload has no command");
        };
        let (user_id, group_id) = resolve_user(specification.user.as_deref())?;
        rustix::system::sethostname(specification.hostname.as_bytes()).context("setting the hostname")?;
        std::fs::write("/etc/hostname", format!("{}\n", specification.hostname))
            .context("writing /etc/hostname")?;
        let child = Command::new(program)
            .args(arguments)
            .env_clear()
            .envs(&specification.environment)
            .current_dir(specification.working_directory.as_deref().unwrap_or("/"))
            .uid(user_id)
            .gid(group_id)
            .stdin(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {program}"))?;
        self.process_id = i32::try_from(child.id()).ok();
        tracing::info!(program, process_id = child.id(), "the workload started");
        Ok(())
    }

    pub fn reap(&mut self) {
        while let Ok(Some((process, status))) = waitpid(None, WaitOptions::NOHANG) {
            if Some(process.as_raw_nonzero().get()) == self.process_id {
                let code = status
                    .exit_status()
                    .or_else(|| status.terminating_signal().map(|signal| SIGNAL_EXIT_OFFSET + signal))
                    .unwrap_or(-1);
                tracing::info!(code, "the workload exited");
                self.exit_code = Some(code);
            }
        }
    }

    #[must_use]
    pub fn state(&self) -> WorkloadState {
        match (self.process_id, self.exit_code) {
            (_, Some(code)) => WorkloadState::Exited { code },
            (Some(_), None) => WorkloadState::Running,
            (None, None) => WorkloadState::NotStarted,
        }
    }

    pub fn stop(&self) {
        if let (Some(process), None) = (self.process_id.and_then(Pid::from_raw), self.exit_code) {
            let _ = kill_process(process, Signal::TERM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workload_moves_from_not_started_to_exited() {
        let mut workload = Workload::default();
        assert_eq!(workload.state(), WorkloadState::NotStarted);
        workload.process_id = Some(42);
        assert_eq!(workload.state(), WorkloadState::Running);
        workload.exit_code = Some(3);
        assert_eq!(workload.state(), WorkloadState::Exited { code: 3 });
    }

    #[test]
    fn numeric_users_need_no_password_file() {
        assert_eq!(resolve_user(None).unwrap(), (0, 0));
        assert_eq!(resolve_user(Some("1000:100")).unwrap(), (1000, 100));
        assert!(resolve_user(Some("nobody-at-all")).is_err());
    }
}
