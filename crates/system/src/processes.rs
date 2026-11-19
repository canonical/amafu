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

const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub fn reboot() -> std::io::Result<()> {
    if !finishes_within(SYNC_TIMEOUT, rustix::fs::sync) {
        tracing::warn!(timeout = ?SYNC_TIMEOUT, "flushing the file systems hangs; restarting without it");
    }
    rustix::system::reboot(RebootCommand::Restart).map_err(Into::into)
}

fn finishes_within(timeout: std::time::Duration, work: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        work();
        let _ = done.send(());
    });
    finished.recv_timeout(timeout).is_ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn quick_work_finishes_in_time() {
        assert!(finishes_within(Duration::from_secs(5), || {}));
    }

    #[test]
    fn hung_work_does_not_hold_up_the_caller() {
        let started = std::time::Instant::now();
        assert!(!finishes_within(Duration::from_millis(50), || std::thread::sleep(Duration::from_secs(5))));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
