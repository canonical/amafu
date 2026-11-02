use rustix::process::{WaitOptions, getpid, wait};
use rustix::system::RebootCommand;

#[must_use]
pub fn is_process_one() -> bool {
    getpid().is_init()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedProcess {
    pub process_id: i32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[must_use]
pub fn reap_orphans() -> Vec<ReapedProcess> {
    let mut reaped = Vec::new();
    while let Ok(Some((process, status))) = wait(WaitOptions::NOHANG) {
        reaped.push(ReapedProcess {
            process_id: process.as_raw_nonzero().get(),
            exit_code: status.exit_status(),
            signal: status.terminating_signal(),
        });
    }
    reaped
}

pub fn reboot() -> std::io::Result<()> {
    rustix::fs::sync();
    rustix::system::reboot(RebootCommand::Restart).map_err(Into::into)
}
