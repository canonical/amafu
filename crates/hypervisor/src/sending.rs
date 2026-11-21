use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::{NodeName, ObjectId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::migration::{
    MIGRATION_PORT, MIGRATION_TIMEOUT_SECONDS, MigrationRecord, MigrationTransport, report_from_log,
};
use crate::runtime::VmDirectory;
use crate::sandbox::{give_to, limit_cpu_share};
use crate::vmm_api;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const API_TIMEOUT: Duration = Duration::from_secs(15);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_POLL: Duration = Duration::from_millis(100);
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

async fn read_response(stream: &mut UnixStream) -> io::Result<vmm_api::Response> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(response) = vmm_api::complete(&received) {
            return Ok(response);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return vmm_api::closed(&received);
        }
        received.extend_from_slice(&chunk[..read]);
    }
}

async fn put_api(socket: &Path, path: &str, body: &str) -> io::Result<()> {
    let exchange = async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(vmm_api::put_request(path, body).as_bytes()).await?;
        read_response(&mut stream).await
    };
    let response = tokio::time::timeout(API_TIMEOUT, exchange)
        .await
        .map_err(|_| io::Error::other("the VMM did not answer its API in time"))??;
    if response.is_success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the VMM refused to send the VM ({}): {}",
            response.status, response.body
        )))
    }
}

fn send_request(destination: &Path, downtime_milliseconds: u64) -> String {
    serde_json::json!({
        "destination_url": format!("unix:{}", destination.display()),
        "downtime_ms": downtime_milliseconds.max(1),
        "timeout_s": MIGRATION_TIMEOUT_SECONDS,
        "timeout_strategy": vmm::api::TimeoutStrategy::Cancel,
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
    put_api(
        &job.directory.api_socket(),
        vmm_api::SEND_MIGRATION,
        &send_request(&outgoing, job.downtime_milliseconds),
    )
    .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_send_request_is_one_the_vmm_accepts() {
        let request = send_request(Path::new("/run/amafu/outgoing.sock"), 0);
        let accepted: vmm::api::VmSendMigrationData = serde_json::from_str(&request).unwrap();
        assert_eq!(accepted.timeout_strategy, vmm::api::TimeoutStrategy::Cancel);
        assert_eq!(accepted.destination_url, "unix:/run/amafu/outgoing.sock");
    }

    #[tokio::test]
    async fn an_answer_is_read_without_waiting_for_the_connection_to_close() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let answer = read_response(&mut client).await.unwrap();
        assert_eq!(answer.status, "204");
        assert_eq!(answer.body, "");
        server
            .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nbroke")
            .await
            .unwrap();
        let answer = read_response(&mut client).await.unwrap();
        assert_eq!(answer.body, "broke");
        assert!(!answer.is_success());
        drop(server);
    }
}
