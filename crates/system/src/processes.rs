use rustix::process::{WaitOptions, getpid, wait};
use rustix::system::RebootCommand;

#[must_use]
pub fn is_process_one() -> bool {
    getpid().is_init()
}

#[must_use]
pub fn reap_orphans() -> usize {
    let mut reaped = 0;
    while let Ok(Some(_)) = wait(WaitOptions::NOHANG) {
        reaped += 1;
    }
    reaped
}

pub fn reboot() -> std::io::Result<()> {
    rustix::fs::sync();
    rustix::system::reboot(RebootCommand::Restart).map_err(Into::into)
}
