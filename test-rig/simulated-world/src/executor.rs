use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use rand::RngExt;
use rand_chacha::ChaCha8Rng;

use crate::clock::VirtualClock;
use crate::lock::lock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

pub type TaskFuture = Pin<Box<dyn Future<Output = ()>>>;

struct Task {
    label: String,
    future: Option<TaskFuture>,
}

struct TaskWaker {
    id: TaskId,
    ready: Arc<Mutex<BTreeSet<TaskId>>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        lock(&self.ready).insert(self.id);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        lock(&self.ready).insert(self.id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Polled { label: String },
    AdvancedClock,
    Idle,
}

pub struct Executor {
    tasks: RefCell<BTreeMap<TaskId, Task>>,
    ready: Arc<Mutex<BTreeSet<TaskId>>>,
    next_id: Cell<u64>,
    clock: VirtualClock,
}

impl Executor {
    #[must_use]
    pub fn new(clock: VirtualClock) -> Self {
        Self {
            tasks: RefCell::new(BTreeMap::new()),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            next_id: Cell::new(0),
            clock,
        }
    }

    pub fn spawn(&self, label: impl Into<String>, future: impl Future<Output = ()> + 'static) -> TaskId {
        let id = TaskId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        self.tasks.borrow_mut().insert(id, Task { label: label.into(), future: Some(Box::pin(future)) });
        lock(&self.ready).insert(id);
        id
    }

    pub fn kill(&self, id: TaskId) {
        let removed = self.tasks.borrow_mut().remove(&id);
        lock(&self.ready).remove(&id);
        drop(removed);
    }

    pub fn kill_all(&self) {
        let ids: Vec<TaskId> = self.tasks.borrow().keys().copied().collect();
        for id in ids {
            self.kill(id);
        }
    }

    #[must_use]
    pub fn is_alive(&self, id: TaskId) -> bool {
        self.tasks.borrow().contains_key(&id)
    }

    pub fn step(&self, random: &mut ChaCha8Rng) -> StepOutcome {
        let candidates: Vec<TaskId> = lock(&self.ready).iter().copied().collect();
        if candidates.is_empty() {
            return if self.clock.advance_to_next_timer() {
                StepOutcome::AdvancedClock
            } else {
                StepOutcome::Idle
            };
        }
        let id = candidates[random.random_range(0..candidates.len())];
        lock(&self.ready).remove(&id);
        let taken = self
            .tasks
            .borrow_mut()
            .get_mut(&id)
            .and_then(|task| task.future.take().map(|future| (task.label.clone(), future)));
        let Some((label, mut future)) = taken else {
            return StepOutcome::Polled { label: String::from("finished task") };
        };
        let waker = Waker::from(Arc::new(TaskWaker { id, ready: Arc::clone(&self.ready) }));
        let poll = future.as_mut().poll(&mut Context::from_waker(&waker));
        match poll {
            Poll::Ready(()) => {
                self.tasks.borrow_mut().remove(&id);
            }
            Poll::Pending => {
                if let Some(task) = self.tasks.borrow_mut().get_mut(&id) {
                    task.future = Some(future);
                }
            }
        }
        StepOutcome::Polled { label }
    }
}
