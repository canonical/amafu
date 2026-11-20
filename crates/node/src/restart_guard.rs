use std::sync::atomic::{AtomicBool, Ordering};

use amafu_interfaces::Watchdog;

pub struct RestartGuard<W: Watchdog> {
    watchdog: W,
    is_restarting: AtomicBool,
}

impl<W: Watchdog> RestartGuard<W> {
    pub fn new(watchdog: W) -> Self {
        Self { watchdog, is_restarting: AtomicBool::new(false) }
    }

    pub fn feed(&self) -> std::io::Result<()> {
        if self.is_restarting.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.watchdog.feed()
    }

    pub fn begin_restart(&self) {
        self.is_restarting.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[derive(Default)]
    struct CountingWatchdog {
        feeds: AtomicUsize,
    }

    impl Watchdog for &CountingWatchdog {
        fn feed(&self) -> std::io::Result<()> {
            self.feeds.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn a_node_that_restarts_stops_feeding_its_watchdog_so_a_hung_restart_is_reset() {
        let watchdog = CountingWatchdog::default();
        let guard = RestartGuard::new(&watchdog);
        guard.feed().unwrap();
        guard.begin_restart();
        guard.feed().unwrap();
        guard.feed().unwrap();
        assert_eq!(watchdog.feeds.load(Ordering::SeqCst), 1);
    }
}
