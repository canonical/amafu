use std::time::Duration;

use amafu_interfaces::{ActionError, Clock, Store, WatchEvent};
use futures::future::{self, Either};
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use time::OffsetDateTime;

use crate::controller::{Controller, KeyMapping};
use crate::settings::ReconcilerSettings;
use crate::work_queue::WorkQueue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAttempt {
    After(Duration),
    WithBackoff,
    WhenSpecificationChanges(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("controller {controller} failed on {key}: {cause}")]
pub struct ControllerFailure {
    pub controller: &'static str,
    pub key: String,
    pub cause: String,
}

pub async fn reconcile_once<C: Controller>(
    controller: &C,
    key: &C::Key,
    settings: &ReconcilerSettings,
) -> Result<NextAttempt, ControllerFailure> {
    let fail = |error| next_attempt_after::<C>(key, error);
    let observation = match controller.observe(key).await {
        Ok(observation) => observation,
        Err(error) => return fail(error),
    };
    let revisit = controller.revisit_after(&observation);
    for action in controller.plan(&observation) {
        tracing::debug!(controller = C::NAME, ?key, ?action, "applying action");
        if let Err(error) = controller.apply(key, action).await {
            return fail(error);
        }
    }
    let periodic = settings.periodic_resynchronization;
    Ok(NextAttempt::After(revisit.map_or(periodic, |delay| delay.min(periodic))))
}

pub fn next_attempt_after<C: Controller>(
    key: &C::Key,
    error: ActionError,
) -> Result<NextAttempt, ControllerFailure> {
    match error {
        ActionError::Retryable(reason) => {
            tracing::info!(controller = C::NAME, ?key, %reason, "retrying after a transient error");
            Ok(NextAttempt::WithBackoff)
        }
        ActionError::Invalid(reason) => {
            tracing::warn!(controller = C::NAME, ?key, %reason, "waiting for the specification to change");
            Ok(NextAttempt::WhenSpecificationChanges(reason))
        }
        ActionError::Fatal(cause) => {
            tracing::error!(controller = C::NAME, ?key, %cause, "invariant broken");
            Err(ControllerFailure { controller: C::NAME, key: format!("{key:?}"), cause })
        }
    }
}

struct Loop<'run, C: Controller> {
    controller: &'run C,
    clock: &'run dyn Clock,
    settings: ReconcilerSettings,
    queue: WorkQueue<C::Key>,
    next_discovery_at: OffsetDateTime,
    consecutive_discovery_failures: u32,
}

impl<C: Controller> Loop<'_, C> {
    async fn discover(&mut self) {
        let now = self.clock.now();
        match self.controller.discover_keys().await {
            Ok(keys) => {
                self.consecutive_discovery_failures = 0;
                for key in keys {
                    self.queue.notify_changed(key, now);
                }
                self.next_discovery_at = now + self.settings.periodic_resynchronization;
            }
            Err(error) => {
                self.consecutive_discovery_failures = self.consecutive_discovery_failures.saturating_add(1);
                tracing::info!(controller = C::NAME, %error, "discovering keys failed");
                self.next_discovery_at =
                    now + self.settings.backoff_after(self.consecutive_discovery_failures);
            }
        }
    }

    fn handle_event(&mut self, event: &WatchEvent) {
        match event {
            WatchEvent::Changed(reference) => {
                let now = self.clock.now();
                for watched in C::WATCHES.iter().filter(|watched| watched.kind == reference.kind) {
                    match &watched.mapping {
                        KeyMapping::ObjectKey(key_of) => {
                            if let Some(key) = key_of(reference) {
                                self.queue.notify_changed(key, now);
                            }
                        }
                        KeyMapping::EveryKey => self.next_discovery_at = now,
                    }
                }
            }
            WatchEvent::Resynchronize => self.next_discovery_at = self.clock.now(),
        }
    }

    async fn reconcile_ready_keys(&mut self) -> Result<(), ControllerFailure> {
        while let Some(key) = self.queue.pop_ready(self.clock.now()) {
            let next_attempt = reconcile_once(self.controller, &key, &self.settings).await?;
            let now = self.clock.now();
            match next_attempt {
                NextAttempt::After(delay) => {
                    self.queue.record_success(&key);
                    self.queue.schedule(key, now + delay);
                }
                NextAttempt::WithBackoff => {
                    let failures = self.queue.record_failure(&key);
                    self.queue.schedule(key, now + self.settings.backoff_after(failures));
                }
                NextAttempt::WhenSpecificationChanges(_) => {
                    self.queue.record_success(&key);
                    self.queue.park_until_changed(key);
                }
            }
        }
        Ok(())
    }

    fn time_until_next_work(&self) -> Duration {
        let now = self.clock.now();
        let next = self
            .queue
            .next_ready_at()
            .map_or(self.next_discovery_at, |ready| ready.min(self.next_discovery_at));
        Duration::try_from(next - now).unwrap_or(Duration::ZERO)
    }
}

fn watch_all<C: Controller>(store: &dyn Store) -> BoxStream<'static, WatchEvent> {
    stream::select_all(C::WATCHES.iter().map(|watched| store.watch(watched.kind))).boxed()
}

pub async fn run_controller<C: Controller>(
    controller: &C,
    store: &dyn Store,
    clock: &dyn Clock,
    settings: ReconcilerSettings,
) -> ControllerFailure {
    let mut events = watch_all::<C>(store);
    let mut state = Loop {
        controller,
        clock,
        settings,
        queue: WorkQueue::new(),
        next_discovery_at: clock.now(),
        consecutive_discovery_failures: 0,
    };
    loop {
        if state.next_discovery_at <= clock.now() {
            state.discover().await;
        }
        if let Err(failure) = state.reconcile_ready_keys().await {
            return failure;
        }
        let sleep = clock.sleep(state.time_until_next_work());
        match future::select(events.next(), sleep).await {
            Either::Left((Some(event), _)) => {
                state.handle_event(&event);
                while let Some(Some(event)) = events.next().now_or_never() {
                    state.handle_event(&event);
                }
            }
            Either::Left((None, _)) => {
                return ControllerFailure {
                    controller: C::NAME,
                    key: String::from("*"),
                    cause: String::from("the store watch ended"),
                };
            }
            Either::Right(((), _)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::controller::WatchedKind;

    #[derive(Default)]
    struct Counter {
        observed: Mutex<Option<Result<u32, ActionError>>>,
        applied: Mutex<Vec<u32>>,
        failure_on_apply: Mutex<Option<ActionError>>,
    }

    impl Controller for Counter {
        type Key = String;
        type Observation = u32;
        type Action = u32;

        const NAME: &'static str = "counter";
        const WATCHES: &'static [WatchedKind<String>] = &[];

        async fn discover_keys(&self) -> Result<Vec<String>, ActionError> {
            Ok(vec![String::from("only")])
        }

        async fn observe(&self, _key: &String) -> Result<u32, ActionError> {
            self.observed.lock().unwrap().clone().unwrap_or(Ok(0))
        }

        fn plan(&self, observation: &u32) -> Vec<u32> {
            (0..*observation).collect()
        }

        async fn apply(&self, _key: &String, action: u32) -> Result<(), ActionError> {
            if let Some(error) = self.failure_on_apply.lock().unwrap().clone() {
                return Err(error);
            }
            self.applied.lock().unwrap().push(action);
            Ok(())
        }
    }

    fn run(counter: &Counter) -> Result<NextAttempt, ControllerFailure> {
        futures::executor::block_on(reconcile_once(
            counter,
            &String::from("only"),
            &ReconcilerSettings::default(),
        ))
    }

    #[test]
    fn every_planned_action_is_applied_in_order() {
        let counter = Counter::default();
        *counter.observed.lock().unwrap() = Some(Ok(3));
        assert_eq!(
            run(&counter),
            Ok(NextAttempt::After(ReconcilerSettings::default().periodic_resynchronization))
        );
        assert_eq!(*counter.applied.lock().unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn a_retryable_error_backs_off_and_stops_the_pass() {
        let counter = Counter::default();
        *counter.observed.lock().unwrap() = Some(Ok(3));
        *counter.failure_on_apply.lock().unwrap() = Some(ActionError::retryable("busy"));
        assert_eq!(run(&counter), Ok(NextAttempt::WithBackoff));
        assert!(counter.applied.lock().unwrap().is_empty());
    }

    #[test]
    fn an_invalid_observation_waits_for_a_change() {
        let counter = Counter::default();
        *counter.observed.lock().unwrap() = Some(Err(ActionError::invalid("no such image")));
        assert_eq!(run(&counter), Ok(NextAttempt::WhenSpecificationChanges(String::from("no such image"))));
    }

    #[test]
    fn a_fatal_error_fails_the_controller() {
        let counter = Counter::default();
        *counter.observed.lock().unwrap() = Some(Err(ActionError::fatal("two owners")));
        let failure = run(&counter).unwrap_err();
        assert_eq!(failure.controller, "counter");
        assert_eq!(failure.cause, "two owners");
    }
}
