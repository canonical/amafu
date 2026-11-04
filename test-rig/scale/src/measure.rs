use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use amafu_api::{ApiClient, HttpTransport};
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::vm::{Vm, VmPhase, VmSpecification};
use amafu_domain::{ObjectName, ProjectName};
use anyhow::bail;
use futures::StreamExt;
use serde::Serialize;

use crate::cores::{CoreCluster, PROJECT, available_memory_mebibytes, resident_memory_mebibytes};
use crate::fleet::SimulatedFleet;

const POLL: Duration = Duration::from_secs(1);
const MEMORY_CHECK: Duration = Duration::from_millis(500);
const LARGEST_CORE_MEMORY_MEBIBYTES: u64 = 6_144;
const SMALLEST_AVAILABLE_MEMORY_MEBIBYTES: u64 = 6_144;
const CREATION_CONCURRENCY: usize = 64;
const READ_CLIENTS: usize = 48;
const READER_THREADS: usize = 4;
const VM_MEMORY_MEBIBYTES: u64 = 128;

#[derive(Debug, Clone)]
pub struct ScaleSettings {
    pub node_binary: PathBuf,
    pub work: PathBuf,
    pub state: PathBuf,
    pub core_members: u16,
    pub nodes: usize,
    pub vms: usize,
    pub read_rate: u32,
    pub read_duration: Duration,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScaleBudgets {
    pub join_seconds: f64,
    pub placement_seconds: f64,
    pub read_p99_milliseconds: f64,
    pub read_rate: u32,
    pub core_memory_mebibytes: u64,
}

impl Default for ScaleBudgets {
    fn default() -> Self {
        Self {
            join_seconds: 120.0,
            placement_seconds: 120.0,
            read_p99_milliseconds: 10.0,
            read_rate: 2_000,
            core_memory_mebibytes: 2_048,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadReport {
    pub requests: usize,
    pub failures: usize,
    pub achieved_rate: f64,
    pub median_milliseconds: f64,
    pub p99_milliseconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScaleReport {
    pub nodes: usize,
    pub vms: usize,
    pub join_seconds: f64,
    pub placement_seconds: f64,
    pub read: ReadReport,
    pub core_memory_mebibytes: Vec<u64>,
    pub peak_core_memory_mebibytes: u64,
    pub fleet_memory_mebibytes: u64,
    pub nodes_not_ready_at_end: usize,
    pub budgets: ScaleBudgets,
    pub violations: Vec<String>,
}

fn operator_client(cluster: &CoreCluster, endpoint: &str) -> anyhow::Result<ApiClient> {
    let transport = HttpTransport::new(endpoint, Some(cluster.authority.certificate_pem.as_bytes()))?;
    Ok(ApiClient::new(Arc::new(transport), Some(cluster.operator_token.clone()))
        .in_project(ProjectName::new(PROJECT)?))
}

fn is_simulated(node: &amafu_domain::Object<Node>) -> bool {
    node.metadata.name.as_str().starts_with("simulated-")
}

async fn ready_simulated_nodes(client: &ApiClient) -> anyhow::Result<usize> {
    Ok(client
        .list::<Node>()
        .await?
        .iter()
        .filter(|node| is_simulated(node) && node.status.is_ready())
        .count())
}

async fn wait_for(
    description: &str,
    timeout: Duration,
    mut condition: impl AsyncFnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<f64> {
    let started = Instant::now();
    loop {
        if condition().await.unwrap_or(false) {
            return Ok(started.elapsed().as_secs_f64());
        }
        if started.elapsed() >= timeout {
            bail!("{description} did not happen within {} seconds", timeout.as_secs());
        }
        tokio::time::sleep(POLL).await;
    }
}

fn vm_name(index: usize) -> anyhow::Result<ObjectName> {
    Ok(ObjectName::new(format!("load-{index:05}"))?)
}

async fn create_vms(client: &ApiClient, count: usize) -> anyhow::Result<()> {
    let specification = VmSpecification { memory_mebibytes: VM_MEMORY_MEBIBYTES, ..VmSpecification::small() };
    let results: Vec<anyhow::Result<()>> = futures::stream::iter(0..count)
        .map(|index| {
            let specification = specification.clone();
            async move {
                client.create::<Vm>(&vm_name(index)?, &specification).await?;
                Ok(())
            }
        })
        .buffer_unordered(CREATION_CONCURRENCY)
        .collect()
        .await;
    results.into_iter().collect()
}

async fn running_vms(client: &ApiClient) -> anyhow::Result<usize> {
    Ok(client.list::<Vm>().await?.iter().filter(|vm| vm.status.phase == VmPhase::Running).count())
}

fn percentile(sorted: &[Duration], per_thousand: usize) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return f64::NAN;
    };
    let position = (last * per_thousand).div_ceil(1000);
    sorted[position.min(last)].as_secs_f64() * 1000.0
}

async fn read_load(
    clients: &[ApiClient],
    vms: usize,
    rate: u32,
    duration: Duration,
) -> anyhow::Result<ReadReport> {
    let interval = Duration::from_secs_f64(1.0 / f64::from(rate.max(1)));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let started = Instant::now();
    let mut requests = tokio::task::JoinSet::new();
    let mut index: usize = 0;
    while started.elapsed() < duration {
        ticker.tick().await;
        let client = clients[index % clients.len()].clone();
        let name = vm_name(index % vms.max(1))?;
        requests.spawn(async move {
            let sent = Instant::now();
            let answered = client.get_local::<Vm>(name.as_str()).await.is_ok();
            (sent.elapsed(), answered)
        });
        index += 1;
    }
    let elapsed = started.elapsed();
    let mut latencies = Vec::new();
    let mut failures = 0;
    while let Some(outcome) = requests.join_next().await {
        let (latency, answered) = outcome?;
        if answered {
            latencies.push(latency);
        } else {
            failures += 1;
        }
    }
    latencies.sort();
    Ok(ReadReport {
        requests: index,
        failures,
        achieved_rate: f64::from(u32::try_from(index).unwrap_or(u32::MAX)) / elapsed.as_secs_f64(),
        median_milliseconds: percentile(&latencies, 500),
        p99_milliseconds: percentile(&latencies, 990),
    })
}

async fn on_a_runtime_of_its_own(
    readers: Vec<ApiClient>,
    vms: usize,
    rate: u32,
    duration: Duration,
) -> anyhow::Result<ReadReport> {
    let (answer, answered) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let outcome = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(READER_THREADS)
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(read_load(&readers, vms, rate, duration)));
        let _ = answer.send(outcome);
    });
    answered.await?
}

fn violations(report: &ScaleReport) -> Vec<String> {
    let budgets = &report.budgets;
    let mut violations = Vec::new();
    if report.join_seconds > budgets.join_seconds {
        violations.push(format!(
            "{} nodes joined in {:.1} s, over {} s",
            report.nodes, report.join_seconds, budgets.join_seconds
        ));
    }
    if report.placement_seconds > budgets.placement_seconds {
        violations.push(format!(
            "{} VMs ran after {:.1} s, over {} s",
            report.vms, report.placement_seconds, budgets.placement_seconds
        ));
    }
    if report.read.p99_milliseconds > budgets.read_p99_milliseconds {
        violations.push(format!(
            "reads took {:.1} ms at the 99th percentile, over {} ms",
            report.read.p99_milliseconds, budgets.read_p99_milliseconds
        ));
    }
    if report.read.achieved_rate < f64::from(budgets.read_rate) * 0.95 || report.read.failures > 0 {
        violations.push(format!(
            "reads ran at {:.0} per second with {} failures",
            report.read.achieved_rate, report.read.failures
        ));
    }
    if report.peak_core_memory_mebibytes > budgets.core_memory_mebibytes {
        violations.push(format!(
            "a core member reached {} MiB, over {} MiB",
            report.peak_core_memory_mebibytes, budgets.core_memory_mebibytes
        ));
    }
    if report.nodes_not_ready_at_end > 0 {
        violations.push(format!("{} nodes were not ready at the end", report.nodes_not_ready_at_end));
    }
    violations
}

async fn memory_guard(process_ids: Vec<u32>, peak: Arc<AtomicU64>) -> anyhow::Error {
    loop {
        let largest = process_ids.iter().copied().map(resident_memory_mebibytes).max().unwrap_or_default();
        peak.fetch_max(largest, Ordering::Relaxed);
        if largest > LARGEST_CORE_MEMORY_MEBIBYTES {
            return anyhow::anyhow!("a core member reached {largest} MiB, so the run was stopped");
        }
        if let Some(available) = available_memory_mebibytes()
            && available < SMALLEST_AVAILABLE_MEMORY_MEBIBYTES
        {
            return anyhow::anyhow!("only {available} MiB of memory were left, so the run was stopped");
        }
        tokio::time::sleep(MEMORY_CHECK).await;
    }
}

pub async fn run(settings: &ScaleSettings, budgets: ScaleBudgets) -> anyhow::Result<ScaleReport> {
    for directory in [&settings.work, &settings.state] {
        let _ = std::fs::remove_dir_all(directory);
        std::fs::create_dir_all(directory)?;
    }
    if available_memory_mebibytes()
        .is_some_and(|available| available < SMALLEST_AVAILABLE_MEMORY_MEBIBYTES * 2)
    {
        bail!("too little memory is free to run the scale scenario safely");
    }
    let cluster =
        CoreCluster::start(&settings.node_binary, &settings.work, &settings.state, settings.core_members)
            .await?;
    let peak = Arc::new(AtomicU64::new(0));
    let process_ids = cluster.members.iter().map(|member| member.process.id()).collect();
    let outcome = tokio::select! {
        stopped = memory_guard(process_ids, Arc::clone(&peak)) => Err(stopped),
        report = measure(settings, budgets, &cluster, &peak) => report,
    };
    drop(cluster);
    let _ = std::fs::remove_dir_all(&settings.state);
    outcome
}

async fn measure(
    settings: &ScaleSettings,
    budgets: ScaleBudgets,
    cluster: &CoreCluster,
    peak: &AtomicU64,
) -> anyhow::Result<ScaleReport> {
    let endpoints = cluster.endpoints();
    let operator = operator_client(cluster, &endpoints[0])?;
    tracing::info!(nodes = settings.nodes, "starting the simulated nodes");
    let mut fleet = SimulatedFleet::start(settings.nodes, &endpoints, &cluster.authority)?;
    let nodes = settings.nodes;
    let join_seconds = wait_for("every simulated node joining", Duration::from_secs(600), async || {
        Ok(ready_simulated_nodes(&operator).await? >= nodes)
    })
    .await?;
    tracing::info!(join_seconds, "every simulated node is ready");
    let placing = Instant::now();
    create_vms(&operator, settings.vms).await?;
    let vms = settings.vms;
    wait_for("every VM running", Duration::from_secs(900), async || Ok(running_vms(&operator).await? >= vms))
        .await?;
    let placement_seconds = placing.elapsed().as_secs_f64();
    tracing::info!(placement_seconds, "every VM runs");
    let readers = (0..READ_CLIENTS)
        .map(|index| operator_client(cluster, &endpoints[index % endpoints.len()]))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for reader in &readers {
        reader.get_local::<Vm>(vm_name(0)?.as_str()).await?;
    }
    let read =
        on_a_runtime_of_its_own(readers, settings.vms, settings.read_rate, settings.read_duration).await?;
    let nodes_not_ready_at_end = nodes.saturating_sub(ready_simulated_nodes(&operator).await?);
    let mut report = ScaleReport {
        nodes,
        vms,
        join_seconds,
        placement_seconds,
        read,
        core_memory_mebibytes: cluster.resident_memory_mebibytes(),
        peak_core_memory_mebibytes: peak.load(Ordering::Relaxed),
        fleet_memory_mebibytes: resident_memory_mebibytes(std::process::id()),
        nodes_not_ready_at_end,
        budgets,
        violations: Vec::new(),
    };
    report.violations = violations(&report);
    fleet.stop();
    Ok(report)
}
