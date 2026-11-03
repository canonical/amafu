use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use amafu_api::{ApiClient, ClientError};
use amafu_checkers::history::{HistoryEntry, RegisterOperation, RegisterOutcome, check_linearizable};
use amafu_domain::access::Role;
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::{NodeName, Object, ProjectName};
use amafu_scenario::cluster::DEFAULT_PROJECT;
use amafu_scenario::{Cluster, Fault, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure};
use futures::FutureExt;

use crate::support::{delete_and_wait, every_node_ready, name};

pub const STORE_HISTORY_IS_LINEARIZABLE: ScenarioDefinition = ScenarioDefinition {
    name: "concurrent conditional updates through every core member stay linearizable while a member is cut off",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| concurrent_updates_stay_linearizable(cluster).boxed_local(),
};

const REGISTERS: [&str; 2] = ["register-0", "register-1"];
const WORKLOAD: Duration = Duration::from_secs(40);
const PAUSE: Duration = Duration::from_millis(150);
const FAULT_START: Duration = Duration::from_secs(8);
const FAULT_LENGTH: Duration = Duration::from_secs(12);
const VALUES_PER_CLIENT: u64 = 20_000;
const INITIAL_VALUE: u64 = 65_000;
const CONFLICT: u16 = 409;
const NOT_FOUND: u16 = 404;

type History = Rc<RefCell<Vec<HistoryEntry>>>;

fn holding(value: u64) -> SecurityGroupSpecification {
    let port = u16::try_from(value).unwrap_or(u16::MAX);
    SecurityGroupSpecification {
        rules: vec![SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            ports: Some(PortRange { first: port, last: port }),
            remote: Remote::Any,
        }],
    }
}

fn held(group: &Object<SecurityGroup>) -> (u64, u64) {
    let value = group
        .specification
        .rules
        .first()
        .and_then(|rule| rule.ports)
        .map_or(0, |range| u64::from(range.first));
    (group.metadata.generation, value)
}

fn nanoseconds(cluster: &dyn Cluster) -> i128 {
    cluster.clock().now().unix_timestamp_nanos()
}

fn record(history: &History, entry: HistoryEntry) {
    history.borrow_mut().push(entry);
}

async fn read(
    client: &ApiClient,
    cluster: &dyn Cluster,
    history: &History,
    index: usize,
    key: &str,
) -> Option<u64> {
    let called_at = nanoseconds(cluster);
    let outcome = match client.get::<SecurityGroup>(key).await {
        Ok(group) => RegisterOutcome::Read(Some(held(&group))),
        Err(error) if error.status() == Some(NOT_FOUND) => RegisterOutcome::Read(None),
        Err(_) => return None,
    };
    let generation = match &outcome {
        RegisterOutcome::Read(Some((generation, _))) => Some(*generation),
        _ => None,
    };
    let returned_at = Some(nanoseconds(cluster));
    record(
        history,
        HistoryEntry {
            key: key.to_owned(),
            client: index,
            called_at,
            returned_at,
            operation: RegisterOperation::Read,
            outcome,
        },
    );
    generation
}

fn write_outcome(result: &Result<Object<SecurityGroup>, ClientError>) -> RegisterOutcome {
    match result {
        Ok(group) => RegisterOutcome::Written { generation: group.metadata.generation },
        Err(error) if error.status() == Some(CONFLICT) => RegisterOutcome::Conflict,
        Err(_) => RegisterOutcome::Unknown,
    }
}

async fn write(
    client: &ApiClient,
    cluster: &dyn Cluster,
    history: &History,
    index: usize,
    key: &str,
    expected_generation: u64,
    value: u64,
) {
    let called_at = nanoseconds(cluster);
    let result = client.update::<SecurityGroup>(key, &holding(value), Some(expected_generation)).await;
    let outcome = write_outcome(&result);
    let returned_at = (outcome != RegisterOutcome::Unknown).then(|| nanoseconds(cluster));
    let operation = RegisterOperation::Write { expected_generation, value };
    record(
        history,
        HistoryEntry { key: key.to_owned(), client: index, called_at, returned_at, operation, outcome },
    );
}

async fn worker(client: ApiClient, cluster: &dyn Cluster, history: History, index: usize) {
    let clock = cluster.clock();
    let deadline = clock.now() + WORKLOAD;
    let mut sequence: u64 = 1;
    while clock.now() < deadline {
        let key = REGISTERS[usize::try_from(sequence).unwrap_or_default() % REGISTERS.len()];
        if let Some(generation) = read(&client, cluster, &history, index, key).await {
            let value = u64::try_from(index).unwrap_or_default() * VALUES_PER_CLIENT + sequence;
            write(&client, cluster, &history, index, key, generation, value).await;
        }
        sequence = sequence % (VALUES_PER_CLIENT - 1) + 1;
        clock.sleep(PAUSE).await;
    }
}

async fn cut_off_a_core_member(cluster: &dyn Cluster, member: NodeName) -> ScenarioResult {
    cluster.clock().sleep(FAULT_START).await;
    let fault = cluster.inject(Fault::Partition { node: member }).await?;
    cluster.clock().sleep(FAULT_LENGTH).await;
    cluster.heal(fault).await
}

async fn concurrent_updates_stay_linearizable(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let project =
        ProjectName::new(DEFAULT_PROJECT).map_err(|error| ScenarioError::Expectation(error.to_string()))?;
    let clients = cluster.core_member_clients(Role::Member, &project).await;
    let creator =
        clients.first().ok_or_else(|| ScenarioError::Expectation(String::from("no core member")))?;
    let history: History = Rc::new(RefCell::new(Vec::new()));
    for key in REGISTERS {
        let called_at = nanoseconds(cluster);
        creator.create::<SecurityGroup>(&name(key)?, &holding(INITIAL_VALUE)).await?;
        record(
            &history,
            HistoryEntry {
                key: key.to_owned(),
                client: 0,
                called_at,
                returned_at: Some(nanoseconds(cluster)),
                operation: RegisterOperation::Create { value: INITIAL_VALUE },
                outcome: RegisterOutcome::Created,
            },
        );
    }
    let victim = cluster
        .nodes()
        .await
        .get(1)
        .cloned()
        .ok_or_else(|| ScenarioError::Expectation(String::from("too few nodes")))?;
    let workers = clients
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, client)| worker(client, cluster, Rc::clone(&history), index));
    let (fault, ()) = futures::future::join(
        cut_off_a_core_member(cluster, victim),
        futures::future::join_all(workers).map(|_| ()),
    )
    .await;
    fault?;
    let recorded = history.borrow().clone();
    let writes =
        recorded.iter().filter(|entry| matches!(entry.outcome, RegisterOutcome::Written { .. })).count();
    ensure(writes > 0, || String::from("no write succeeded during the run"))?;
    check_linearizable(&recorded).map_err(|failure| {
        ScenarioError::Expectation(format!(
            "the history of {} with {} operations is not linearizable",
            failure.key, failure.operations
        ))
    })?;
    for key in REGISTERS {
        delete_and_wait::<SecurityGroup>(creator, cluster, key).await?;
    }
    Ok(())
}
