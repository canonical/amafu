use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::{NodeName, ObjectId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::migration::{MIGRATION_PORT, MigrationRecord, MigrationTransport, report_from_log};
use crate::runtime::VmDirectory;
use crate::sandbox::{give_to, limit_cpu_share};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_POLL: Duration = Duration::from_millis(100);
const MIGRATION_TIMEOUT_SECONDS: u64 = 900;
const THROTTLE_AFTER: Duration = Duration::from_secs(10);
const THROTTLE_STEP: Duration = Duration::from_secs(5);
const THROTTLE_SHARES_PERCENT: [u64; 4] = [50, 25, 10, 5];

pub struct SendJob {
    pub directory: VmDirectory,
    pub transport: Arc<MigrationTransport>,
    pub vm_id: ObjectId,
    pub attempt: ObjectId,
    pub node: NodeName,
    pub node_address: Ipv4Addr,
    pub downtime_milliseconds: u64,
    pub cgroup: String,
    pub cpu_count: u16,
    pub is_alive: fn(&VmDirectory) -> bool,
}

async fn put_api(socket: &Path, path: &str, body: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket).await?;
    let request = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await?;
    let status = answer.split_whitespace().nth(1).unwrap_or_default();
    if status.starts_with('2') {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the VMM refused to send the VM: {}",
            answer.lines().last().unwrap_or(status)
        )))
    }
}

fn send_request(job: &SendJob) -> String {
    serde_json::json!({
        "destination_url": format!("unix:{}", job.directory.outgoing_socket().display()),
        "downtime_ms": job.downtime_milliseconds.max(1),
        "timeout_s": MIGRATION_TIMEOUT_SECONDS,
        "timeout_strategy": "cancel",
    })
    .to_string()
}

async fn throttle_while_sending(cgroup: String, cpu_count: u16, vm_id: ObjectId) {
    tokio::time::sleep(THROTTLE_AFTER).await;
    for share in THROTTLE_SHARES_PERCENT {
        match limit_cpu_share(&cgroup, cpu_count, share) {
            Ok(()) => tracing::info!(%vm_id, share, "slowing a VM so its migration converges"),
            Err(error) => tracing::warn!(%vm_id, %error, "slowing a VM failed"),
        }
        tokio::time::sleep(THROTTLE_STEP).await;
    }
}

async fn relay_once(job: &SendJob, listener: UnixListener) -> io::Result<u64> {
    let (from, _) = tokio::time::timeout(CONNECTION_TIMEOUT, listener.accept())
        .await
        .map_err(|_| io::Error::other("the VMM never connected to send the VM"))??;
    let destination = SocketAddr::from((job.node_address, MIGRATION_PORT));
    job.transport.relay(from, destination, &job.node, &job.vm_id).await
}

async fn wait_for_exit(job: &SendJob) -> bool {
    let started = tokio::time::Instant::now();
    while (job.is_alive)(&job.directory) {
        if started.elapsed() >= EXIT_TIMEOUT {
            return false;
        }
        tokio::time::sleep(EXIT_POLL).await;
    }
    true
}

async fn send(job: &SendJob) -> io::Result<u64> {
    let outgoing = job.directory.outgoing_socket();
    let _ = std::fs::remove_file(&outgoing);
    let listener = UnixListener::bind(&outgoing)?;
    if let Some(user_id) = job.directory.user_id() {
        give_to(&outgoing, user_id)?;
    }
    put_api(&job.directory.api_socket(), "/api/v1/vm.send-migration", &send_request(job)).await?;
    let throttle = tokio::spawn(throttle_while_sending(job.cgroup.clone(), job.cpu_count, job.vm_id));
    let relayed = relay_once(job, listener).await;
    throttle.abort();
    let relayed = relayed?;
    if wait_for_exit(job).await {
        Ok(relayed)
    } else {
        let _ = limit_cpu_share(&job.cgroup, job.cpu_count, 100);
        Err(io::Error::other("the VMM kept the VM after sending it, so the migration failed"))
    }
}

pub async fn run(job: SendJob) {
    let record = match send(&job).await {
        Ok(relayed) => {
            let log = std::fs::read_to_string(job.directory.process_log()).unwrap_or_default();
            let report = report_from_log(&log, relayed);
            tracing::info!(vm_id = %job.vm_id, ?report, "sent a VM to another node");
            MigrationRecord::Sent { attempt: job.attempt, report }
        }
        Err(error) => {
            let _ = limit_cpu_share(&job.cgroup, job.cpu_count, 100);
            tracing::warn!(vm_id = %job.vm_id, %error, "sending a VM failed");
            MigrationRecord::Failed { attempt: job.attempt, reason: error.to_string() }
        }
    };
    if let Err(error) = record.write(&job.directory) {
        tracing::error!(vm_id = %job.vm_id, %error, "recording the migration outcome failed");
    }
}
