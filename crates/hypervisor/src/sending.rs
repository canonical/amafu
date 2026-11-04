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
const API_TIMEOUT: Duration = Duration::from_secs(15);
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

async fn read_response(stream: &mut UnixStream) -> io::Result<(String, String)> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::other("the VMM closed its API connection early"));
        }
        received.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&received[..header_end]).into_owned();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().to_owned())
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while received.len() < header_end + length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&received[header_end..]).into_owned();
    Ok((headers, body))
}

async fn put_api(socket: &Path, path: &str, body: &str) -> io::Result<()> {
    let exchange = async {
        let mut stream = UnixStream::connect(socket).await?;
        let request = format!(
            "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await?;
        read_response(&mut stream).await
    };
    let (headers, answer) = tokio::time::timeout(API_TIMEOUT, exchange)
        .await
        .map_err(|_| io::Error::other("the VMM did not answer its API in time"))??;
    let status = headers.split_whitespace().nth(1).unwrap_or_default();
    if status.starts_with('2') {
        Ok(())
    } else {
        Err(io::Error::other(format!("the VMM refused to send the VM ({status}): {answer}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_is_read_without_waiting_for_the_connection_to_close() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let (headers, body) = read_response(&mut client).await.unwrap();
        assert!(headers.starts_with("HTTP/1.1 204"));
        assert_eq!(body, "");
        server.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nbroke").await.unwrap();
        let (_, body) = read_response(&mut client).await.unwrap();
        assert_eq!(body, "broke");
        drop(server);
    }
}
