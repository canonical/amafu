use amafu_domain::kinds::operation::{
    Operation, OperationOutcome, OperationSpecification, OperationStatus, Rollback, RollbackReason,
    StepProgress, StepSpecification,
};
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::update_status_fenced;
use amafu_interfaces::{ActionError, Clock, Fence, Store};

pub struct OperationContext<'a> {
    pub operation: &'a Object<Operation>,
    pub store: &'a dyn Store,
    pub clock: &'a dyn Clock,
    pub fence: &'a Fence,
}

#[async_trait::async_trait]
pub trait Step: Send + Sync {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError>;

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError>;

    async fn undo(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub trait StepRegistry: Send + Sync {
    fn build(&self, specification: &StepSpecification) -> Result<Box<dyn Step>, ActionError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Advanced,
    Waiting,
    Finished,
}

pub async fn advance(
    context: &OperationContext<'_>,
    registry: &dyn StepRegistry,
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
    let touched = touched_steps(status);
    if specification.is_cancelled && can_undo(specification, touched) {
        start_rollback(context, RollbackReason::Cancelled, touched).await?;
        return Ok(Progress::Advanced);
    }
    if specification.is_cancelled && can_abandon(specification, index) {
        finish(context, OperationOutcome::Cancelled).await?;
        return Ok(Progress::Finished);
    }
    if specification.is_paused {
        return Ok(Progress::Idle);
    }
    let Some(step_specification) = specification.steps.get(index) else {
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
        Err(error) if !error.is_retryable() && can_undo(specification, index + 1) => {
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

fn touched_steps(status: &OperationStatus) -> usize {
    status.completed_steps + usize::from(status.current_step.is_some())
}

fn can_abandon(specification: &OperationSpecification, index: usize) -> bool {
    specification.steps.get(index).is_some_and(StepSpecification::can_be_abandoned)
}

fn can_undo(specification: &OperationSpecification, steps: usize) -> bool {
    specification.steps.iter().take(steps).all(StepSpecification::can_be_undone)
}

async fn attempt(step: &dyn Step, context: &OperationContext<'_>) -> Result<bool, ActionError> {
    if step.is_complete(context).await? {
        return Ok(true);
    }
    step.run(context).await?;
    step.is_complete(context).await
}

async fn roll_back(
    context: &OperationContext<'_>,
    registry: &dyn StepRegistry,
    rollback: &Rollback,
) -> Result<Progress, ActionError> {
    let Some(index) = rollback.remaining_steps.checked_sub(1) else {
        finish(context, rollback.reason.outcome()).await?;
        return Ok(Progress::Finished);
    };
    let step_specification =
        context.operation.specification.steps.get(index).ok_or_else(|| {
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

fn reference(operation: &Object<Operation>) -> ObjectReference {
    ObjectReference::of::<Operation>(None, operation.metadata.name.clone())
}

async fn record(
    context: &OperationContext<'_>,
    change: impl Fn(&OperationStatus) -> Option<OperationStatus> + Send + Sync,
) -> Result<(), ActionError> {
    update_status_fenced::<Operation>(
        context.store,
        &reference(context.operation),
        context.fence,
        |current| {
            if current.metadata.id != context.operation.metadata.id {
                return None;
            }
            change(&current.status)
        },
    )
    .await?;
    Ok(())
}

async fn start_step(context: &OperationContext<'_>, index: usize) -> Result<(), ActionError> {
    let started_at = context.clock.now();
    record(context, |status| {
        (status.completed_steps == index && status.current_step.is_none() && status.rollback.is_none()).then(
            || OperationStatus { current_step: Some(StepProgress::started(started_at)), ..status.clone() },
        )
    })
    .await
}

async fn complete_step(context: &OperationContext<'_>, index: usize) -> Result<(), ActionError> {
    record(context, |status| {
        (status.completed_steps == index && status.rollback.is_none()).then(|| OperationStatus {
            completed_steps: index + 1,
            current_step: None,
            ..status.clone()
        })
    })
    .await
}

async fn record_failed_attempt(
    context: &OperationContext<'_>,
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

async fn start_rollback(
    context: &OperationContext<'_>,
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

async fn finish(context: &OperationContext<'_>, outcome: OperationOutcome) -> Result<(), ActionError> {
    record(context, |status| {
        status.outcome.is_none().then(|| OperationStatus {
            outcome: Some(outcome.clone()),
            current_step: None,
            ..status.clone()
        })
    })
    .await
}
