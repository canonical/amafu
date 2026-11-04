use std::sync::{Arc, Mutex};
use std::time::Duration;

use amafu_interfaces::{Clock, Watchdog};
use time::OffsetDateTime;

use crate::lock::lock;

pub struct FakeWatchdog {
    clock: Arc<dyn Clock>,
    interval: Duration,
    fed_at: Mutex<Option<OffsetDateTime>>,
}

impl FakeWatchdog {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, interval: Duration) -> Self {
        Self { clock, interval, fed_at: Mutex::new(None) }
    }

    #[must_use]
    pub fn has_fired(&self) -> bool {
        let now = self.clock.now();
        lock(&self.fed_at).is_some_and(|fed_at| now - fed_at > self.interval)
    }

    pub fn disarm(&self) {
        *lock(&self.fed_at) = None;
    }
}

impl Watchdog for FakeWatchdog {
    fn feed(&self) -> std::io::Result<()> {
        *lock(&self.fed_at) = Some(self.clock.now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualClock;

    #[test]
    fn the_watchdog_fires_only_once_armed_and_left_hungry() {
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let watchdog = FakeWatchdog::new(Arc::new(clock.clone()), Duration::from_secs(8));
        assert!(!watchdog.has_fired());
        watchdog.feed().unwrap();
        let mut sleeping = clock.sleep(Duration::from_secs(9));
        assert!(futures::FutureExt::now_or_never(&mut sleeping).is_none());
        assert!(clock.advance_to_next_timer());
        assert!(watchdog.has_fired());
        watchdog.disarm();
        assert!(!watchdog.has_fired());
    }
}
