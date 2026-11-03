use std::fmt::Debug;
use std::time::Duration;

use crate::cluster::Cluster;
use crate::error::{ScenarioError, ScenarioResult, Violation};
use crate::observation::ClusterObservation;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub async fn eventually<Condition>(
    cluster: &dyn Cluster,
    timeout: Duration,
    description: &str,
    mut condition: Condition,
) -> ScenarioResult
where
    Condition: AsyncFnMut() -> Result<bool, ScenarioError>,
{
    let clock = cluster.clock();
    let deadline = clock.now() + timeout;
    loop {
        match condition().await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) if is_transient(&error) => {}
            Err(error) => return Err(error),
        }
        if clock.now() >= deadline {
            return Err(ScenarioError::Timeout(description.to_owned()));
        }
        clock.sleep(POLL_INTERVAL).await;
    }
}

const UNAVAILABLE: u16 = 503;

fn is_transient(error: &ScenarioError) -> bool {
    match error {
        ScenarioError::Client(client) => {
            client.status() == Some(UNAVAILABLE) || matches!(client, amafu_api::ClientError::Transport(_))
        }
        _ => false,
    }
}

pub async fn always_during(
    cluster: &dyn Cluster,
    duration: Duration,
    invariant: fn(&ClusterObservation) -> Result<(), Violation>,
) -> ScenarioResult {
    let clock = cluster.clock();
    let deadline = clock.now() + duration;
    while clock.now() < deadline {
        invariant(&cluster.observe().await?)?;
        clock.sleep(POLL_INTERVAL).await;
    }
    Ok(())
}

pub fn ensure(condition: bool, message: impl FnOnce() -> String) -> ScenarioResult {
    if condition { Ok(()) } else { Err(ScenarioError::Expectation(message())) }
}

pub fn ensure_equal<T: PartialEq + Debug>(
    actual: &T,
    expected: &T,
    context: impl FnOnce() -> String,
) -> ScenarioResult {
    ensure(actual == expected, || format!("{}: expected {expected:?}, got {actual:?}", context()))
}
