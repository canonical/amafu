use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use amafu_interfaces::Clock;
use futures::future::BoxFuture;
use time::OffsetDateTime;

use crate::lock::lock;

type TimerKey = (OffsetDateTime, u64);

struct ClockState {
    now: OffsetDateTime,
    next_timer: u64,
    timers: BTreeMap<TimerKey, Waker>,
}

#[derive(Clone)]
pub struct VirtualClock {
    state: Arc<Mutex<ClockState>>,
}

impl VirtualClock {
    #[must_use]
    pub fn starting_at(now: OffsetDateTime) -> Self {
        Self { state: Arc::new(Mutex::new(ClockState { now, next_timer: 0, timers: BTreeMap::new() })) }
    }

    #[must_use]
    pub fn advance_to_next_timer(&self) -> bool {
        let due = {
            let mut state = lock(&self.state);
            let Some((&(deadline, _), _)) = state.timers.first_key_value() else {
                return false;
            };
            state.now = state.now.max(deadline);
            let now = state.now;
            let later = state.timers.split_off(&(now, u64::MAX));
            std::mem::replace(&mut state.timers, later)
        };
        for waker in due.into_values() {
            waker.wake();
        }
        true
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> OffsetDateTime {
        lock(&self.state).now
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        let deadline = self.now() + duration;
        Box::pin(Sleep { state: Arc::clone(&self.state), deadline, timer: None })
    }
}

struct Sleep {
    state: Arc<Mutex<ClockState>>,
    deadline: OffsetDateTime,
    timer: Option<u64>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let deadline = self.deadline;
        let timer = {
            let mut state = lock(&self.state);
            if state.now >= deadline {
                if let Some(timer) = self.timer {
                    state.timers.remove(&(deadline, timer));
                }
                return Poll::Ready(());
            }
            let timer = self.timer.unwrap_or_else(|| {
                state.next_timer += 1;
                state.next_timer
            });
            state.timers.insert((deadline, timer), context.waker().clone());
            timer
        };
        self.timer = Some(timer);
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some(timer) = self.timer {
            lock(&self.state).timers.remove(&(self.deadline, timer));
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    #[test]
    fn sleeping_completes_only_when_time_reaches_the_deadline() {
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let mut sleep = clock.sleep(Duration::from_secs(5));
        assert!((&mut sleep).now_or_never().is_none());
        assert!(clock.advance_to_next_timer());
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH + Duration::from_secs(5));
        assert!(sleep.now_or_never().is_some());
        assert!(!clock.advance_to_next_timer());
    }

    #[test]
    fn a_dropped_sleep_leaves_no_timer() {
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH);
        let mut sleep = clock.sleep(Duration::from_secs(5));
        assert!((&mut sleep).now_or_never().is_none());
        drop(sleep);
        assert!(!clock.advance_to_next_timer());
    }
}
