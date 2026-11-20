use std::time::{Duration, SystemTime, UNIX_EPOCH};

use amafu_api::ApiClient;
use amafu_domain::kinds::operation::OperationOutcome;
use amafu_domain::step_plan::{PlannedStep, Runnable, StepPlan};
use amafu_domain::{Object, ObjectName};
use anyhow::bail;
use serde::Serialize;

use crate::output::OutputFormat;
use crate::resources::{Tabulated, print_objects};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const LONGEST_SUBJECT: usize = 40;

pub fn seconds(text: &str) -> Result<Duration, String> {
    text.parse::<u64>().map(Duration::from_secs).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, clap::Args)]
pub struct Waiting {
    #[arg(long, value_parser = seconds, value_name = "SECONDS", help = "Wait this many seconds for the change to finish")]
    pub wait: Option<Duration>,
}

#[must_use]
pub fn outcome_text<K: Runnable>(plan: &Object<K>) -> String {
    match (&plan.status.outcome, &plan.status.rollback) {
        (Some(OperationOutcome::Succeeded), _) => String::from("succeeded"),
        (Some(OperationOutcome::Cancelled), _) => String::from("cancelled"),
        (Some(OperationOutcome::Failed { reason }), _) => format!("failed: {reason}"),
        (None, Some(_)) => String::from("rolling back"),
        (None, None) if plan.specification.is_paused() => String::from("paused"),
        (None, None) => String::from("running"),
    }
}

#[must_use]
pub fn current_step<K: Runnable>(plan: &Object<K>) -> String {
    let Some(step) = plan.specification.steps().get(plan.status.completed_steps) else {
        return String::from("-");
    };
    let error = plan
        .status
        .current_step
        .as_ref()
        .and_then(|progress| progress.last_error.as_ref())
        .map(|error| format!(" ({error})"))
        .unwrap_or_default();
    format!("{}{error}", step.name())
}

#[must_use]
pub fn plan_columns<K: Runnable>(plan: &Object<K>) -> [String; 3] {
    [
        format!("{}/{}", plan.status.completed_steps, plan.specification.steps().len()),
        current_step(plan),
        outcome_text(plan),
    ]
}

pub fn unique_name(prefix: &str, subject: &str) -> anyhow::Result<ObjectName> {
    let stamp =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis()).unwrap_or_default();
    let subject: String = amafu_domain::label_fragment(subject).chars().take(LONGEST_SUBJECT).collect();
    let stamp = format!("{:x}", stamp & 0xff_ffff_ffff);
    let parts = [prefix, subject.trim_end_matches('-'), &stamp];
    Ok(ObjectName::new(parts.iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join("-"))?)
}

pub async fn wait_until_finished<K: Runnable>(
    client: &ApiClient,
    name: &str,
    timeout: Duration,
) -> anyhow::Result<Object<K>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let progress = match client.get::<K>(name).await {
            Ok(plan) if plan.status.is_finished() => return Ok(plan),
            Ok(plan) => current_step(&plan),
            Err(error) if error.is_transient() => format!("the cluster could not answer: {error}"),
            Err(error) => return Err(error.into()),
        };
        if tokio::time::Instant::now() >= deadline {
            bail!("{name} has not finished after {timeout:?}: {progress}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub async fn submit<K: Runnable + Tabulated>(
    client: &ApiClient,
    name: &ObjectName,
    specification: &K::Specification,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()>
where
    Object<K>: Serialize,
{
    let created = client.create::<K>(name, specification).await?;
    let Some(timeout) = wait else {
        return print_objects(&[created], format);
    };
    let finished = wait_until_finished::<K>(client, name.as_str(), timeout).await?;
    print_objects(std::slice::from_ref(&finished), format)?;
    match finished.status.outcome {
        Some(OperationOutcome::Succeeded) => Ok(()),
        _ => bail!("{name} did not succeed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_with_dots_name_a_plan() {
        for version in ["0.1.1", "26.04.1+git3d3a433", "lab-2", "..."] {
            assert!(unique_name("rollback", version).is_ok(), "{version}");
        }
    }

    #[test]
    fn a_long_subject_still_names_a_plan() {
        assert!(unique_name("migrate", &"a.".repeat(60)).is_ok());
    }
}
