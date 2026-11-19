use amafu_domain::kinds::operation::{
    FinishedStep, Operation, OperationOutcome, OperationStatus, Rollback, RollbackReason, StepProgress,
};
use amafu_domain::step_plan::{PlannedStep, Runnable, StepPlan};
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::update_status_fenced;
use amafu_interfaces::{ActionError, Clock, Fence, Store};

pub struct OperationContext<'a, K: Runnable = Operation> {
    pub operation: &'a Object<K>,
    pub store: &'a dyn Store,
    pub clock: &'a dyn Clock,
    pub fence: &'a Fence,
}

type StepOf<K> = <<K as amafu_domain::Resource>::Specification as StepPlan>::Step;

#[async_trait::async_trait]
pub trait Step<K: Runnable = Operation>: Send + Sync {
    async fn is_complete(&self, context: &OperationContext<'_, K>) -> Result<bool, ActionError>;

    async fn run(&self, context: &OperationContext<'_, K>) -> Result<(), ActionError>;

    async fn undo(&self, _context: &OperationContext<'_, K>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub trait StepRegistry<K: Runnable = Operation>: Send + Sync {
    fn build(&self, specification: &StepOf<K>) -> Result<Box<dyn Step<K>>, ActionError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Advanced,
    Waiting,
    Finished,
}

pub async fn advance<K: Runnable>(
    context: &OperationContext<'_, K>,
    registry: &dyn StepRegistry<K>,
) -> Result<Progress, ActionError> {
    let status = &context.operation.status;
    let specification = &context.operation.specification;
    if status.is_finished() {
        return Ok(Progress::Finished);
    }
    if let Some(rollback) = &status.rollback {
        return roll_back(context, registry, rollback).await;
    }
    let index = status.completed_steps;
    let touched = status.touched_steps();
    if specification.is_cancelled() && specification.can_undo_first(touched) {
        start_rollback(context, RollbackReason::Cancelled, touched).await?;
        return Ok(Progress::Advanced);
    }
    if specification.is_cancelled() && specification.can_abandon_at(index) {
        finish(context, OperationOutcome::Cancelled).await?;
        return Ok(Progress::Finished);
    }
    if specification.is_paused() {
        return Ok(Progress::Idle);
    }
    let Some(step_specification) = specification.steps().get(index) else {
        finish(context, OperationOutcome::Succeeded).await?;
        return Ok(Progress::Finished);
    };
    if status.current_step.is_none() {
        start_step(context, index).await?;
    }
    let step = registry.build(step_specification)?;
    match attempt(step.as_ref(), context).await {
        Ok(true) => {
            complete_step(context, index).await?;
            Ok(Progress::Advanced)
        }
        Ok(false) => Ok(Progress::Waiting),
        Err(error) if !error.is_retryable() && specification.can_undo_first(index + 1) => {
            start_rollback(context, RollbackReason::Failed { error: error.to_string() }, index + 1).await?;
            Ok(Progress::Advanced)
        }
        Err(error) if !error.is_retryable() && step_specification.can_be_abandoned() => {
            let reason = format!("step {} of {}: {error}", index + 1, step_specification.name());
            finish(context, OperationOutcome::Failed { reason }).await?;
            Ok(Progress::Finished)
        }
        Err(error) => {
            record_failed_attempt(context, index, &error).await?;
            Err(ActionError::retryable(format!(
                "step {} of {}: {error}",
                index + 1,
                step_specification.name()
            )))
        }
    }
}

async fn attempt<K: Runnable>(
    step: &dyn Step<K>,
    context: &OperationContext<'_, K>,
) -> Result<bool, ActionError> {
    if step.is_complete(context).await? {
        return Ok(true);
    }
    step.run(context).await?;
    step.is_complete(context).await
}

async fn roll_back<K: Runnable>(
    context: &OperationContext<'_, K>,
    registry: &dyn StepRegistry<K>,
    rollback: &Rollback,
) -> Result<Progress, ActionError> {
    let Some(index) = rollback.remaining_steps.checked_sub(1) else {
        finish(context, rollback.reason.outcome()).await?;
        return Ok(Progress::Finished);
    };
    let step_specification =
        context.operation.specification.steps().get(index).ok_or_else(|| {
            ActionError::fatal(format!("rolling back step {} that does not exist", index + 1))
        })?;
    registry.build(step_specification)?.undo(context).await?;
    let remaining = rollback.remaining_steps;
    record(context, |status| {
        let current = status.rollback.as_ref()?;
        (current.remaining_steps == remaining).then(|| OperationStatus {
            rollback: Some(Rollback { remaining_steps: index, ..current.clone() }),
            ..status.clone()
        })
    })
    .await?;
    Ok(Progress::Advanced)
}

fn reference<K: Runnable>(operation: &Object<K>) -> ObjectReference {
    ObjectReference::of::<K>(operation.metadata.project.clone(), operation.metadata.name.clone())
}

async fn record<K: Runnable>(
    context: &OperationContext<'_, K>,
    change: impl Fn(&OperationStatus) -> Option<OperationStatus> + Send + Sync,
) -> Result<(), ActionError> {
    update_status_fenced::<K>(context.store, &reference(context.operation), context.fence, |current| {
        if current.metadata.id != context.operation.metadata.id {
            return None;
        }
        change(&current.status)
    })
    .await?;
    Ok(())
}

async fn start_step<K: Runnable>(context: &OperationContext<'_, K>, index: usize) -> Result<(), ActionError> {
    let started_at = context.clock.now();
    record(context, |status| {
        (status.completed_steps == index && status.current_step.is_none() && status.rollback.is_none()).then(
            || OperationStatus { current_step: Some(StepProgress::started(started_at)), ..status.clone() },
        )
    })
    .await
}

async fn complete_step<K: Runnable>(
    context: &OperationContext<'_, K>,
    index: usize,
) -> Result<(), ActionError> {
    let finished_at = context.clock.now();
    record(context, |status| {
        (status.completed_steps == index && status.rollback.is_none()).then(|| {
            let mut finished_steps = status.finished_steps.clone();
            if let Some(progress) = &status.current_step {
                finished_steps.push(FinishedStep {
                    started_at: progress.started_at,
                    finished_at,
                    attempts: progress.attempts,
                });
            }
            OperationStatus {
                completed_steps: index + 1,
                current_step: None,
                finished_steps,
                ..status.clone()
            }
        })
    })
    .await
}

async fn record_failed_attempt<K: Runnable>(
    context: &OperationContext<'_, K>,
    index: usize,
    error: &ActionError,
) -> Result<(), ActionError> {
    let message = error.to_string();
    record(context, |status| {
        let progress = status.current_step.as_ref()?;
        (status.completed_steps == index && status.rollback.is_none()).then(|| OperationStatus {
            current_step: Some(progress.failed_attempt(&message)),
            ..status.clone()
        })
    })
    .await
}

async fn start_rollback<K: Runnable>(
    context: &OperationContext<'_, K>,
    reason: RollbackReason,
    remaining_steps: usize,
) -> Result<(), ActionError> {
    record(context, |status| {
        (status.rollback.is_none() && status.outcome.is_none()).then(|| OperationStatus {
            rollback: Some(Rollback { reason: reason.clone(), remaining_steps }),
            ..status.clone()
        })
    })
    .await
}

async fn finish<K: Runnable>(
    context: &OperationContext<'_, K>,
    outcome: OperationOutcome,
) -> Result<(), ActionError> {
    record(context, |status| {
        status.outcome.is_none().then(|| OperationStatus {
            outcome: Some(outcome.clone()),
            current_step: None,
            ..status.clone()
        })
    })
    .await
}
