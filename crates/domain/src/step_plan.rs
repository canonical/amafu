use crate::kinds::operation::OperationStatus;
use crate::resource::Resource;

pub trait PlannedStep: Clone + PartialEq + Send + Sync {
    fn name(&self) -> &'static str;

    fn can_be_undone(&self) -> bool;

    fn can_be_abandoned(&self) -> bool;
}

pub trait StepPlan {
    type Step: PlannedStep;

    fn steps(&self) -> &[Self::Step];

    fn is_paused(&self) -> bool;

    fn is_cancelled(&self) -> bool;

    fn cancel(&mut self);

    fn can_undo_first(&self, steps: usize) -> bool {
        self.steps().iter().take(steps).all(PlannedStep::can_be_undone)
    }

    fn can_abandon_at(&self, index: usize) -> bool {
        self.steps().get(index).is_some_and(PlannedStep::can_be_abandoned)
    }

    fn can_stop_now(&self, status: &OperationStatus) -> bool {
        self.can_undo_first(status.touched_steps()) || self.can_abandon_at(status.completed_steps)
    }
}

pub trait Runnable: Resource<Status = OperationStatus, Specification: StepPlan> {}

impl<R> Runnable for R where R: Resource<Status = OperationStatus, Specification: StepPlan> {}
