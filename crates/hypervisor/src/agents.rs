use std::time::Duration;

use amafu_domain::ObjectId;
use amafu_guest_agent::protocol::{AGENT_PORT, MAXIMUM_MESSAGE_BYTES, Reply, Request, decode, encode};
use amafu_interfaces::{ActionError, GuestAgents, WorkloadSpecification, WorkloadState};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::runtime::RuntimeDirectory;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECTION_TIMEOUT: Duration = Duration::from_millis(300);
const ACCEPTED_PREFIX: &str = "OK ";

pub struct HybridVsockAgents {
    runtime: RuntimeDirectory,
}

fn unreachable(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("the guest agent is unreachable: {error}"))
}

async fn read_line(
    reader: &mut BufReader<tokio::io::Take<tokio::net::unix::OwnedReadHalf>>,
) -> Result<String, ActionError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await.map_err(unreachable)?;
    if read == 0 {
        return Err(unreachable("the connection closed"));
    }
    Ok(line)
}

struct AgentConnection {
    reader: BufReader<tokio::io::Take<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl AgentConnection {
    async fn send(&mut self, request: &Request) -> Result<Reply, ActionError> {
        self.writer.write_all(&encode(request)).await.map_err(unreachable)?;
        let reply = read_line(&mut self.reader).await?;
        decode(reply.as_bytes()).map_err(unreachable)
    }
}

impl HybridVsockAgents {
    #[must_use]
    pub fn new(runtime: RuntimeDirectory) -> Self {
        Self { runtime }
    }

    async fn connect(&self, vm_id: &ObjectId) -> Result<AgentConnection, ActionError> {
        let stream = UnixStream::connect(self.runtime.vm(vm_id).vsock_socket()).await.map_err(unreachable)?;
        let (reader, mut writer) = stream.into_split();
        let limit = u64::try_from(MAXIMUM_MESSAGE_BYTES * 2).unwrap_or(u64::MAX);
        let mut reader = BufReader::new(reader.take(limit));
        writer.write_all(format!("CONNECT {AGENT_PORT}\n").as_bytes()).await.map_err(unreachable)?;
        let accepted = read_line(&mut reader).await?;
        if !accepted.starts_with(ACCEPTED_PREFIX) {
            return Err(unreachable(format!("the VMM refused the connection: {}", accepted.trim())));
        }
        Ok(AgentConnection { reader, writer })
    }

    async fn exchange(&self, vm_id: &ObjectId, request: &Request) -> Result<Reply, ActionError> {
        let mut connection = tokio::time::timeout(CONNECTION_TIMEOUT, self.connect(vm_id))
            .await
            .map_err(|_| unreachable("the guest did not accept the connection in time"))??;
        tokio::time::timeout(EXCHANGE_TIMEOUT, connection.send(request))
            .await
            .map_err(|_| unreachable("the exchange timed out"))?
    }
}

#[async_trait::async_trait]
impl GuestAgents for HybridVsockAgents {
    async fn workload(&self, vm_id: &ObjectId) -> Result<WorkloadState, ActionError> {
        match self.exchange(vm_id, &Request::Status).await? {
            Reply::Status { workload } => Ok(workload),
            other => {
                Err(ActionError::retryable(format!("the guest agent answered {other:?} to a status request")))
            }
        }
    }

    async fn start_workload(
        &self,
        vm_id: &ObjectId,
        workload: &WorkloadSpecification,
    ) -> Result<(), ActionError> {
        match self.exchange(vm_id, &Request::Start { workload: workload.clone() }).await? {
            Reply::Started => Ok(()),
            Reply::Refused { reason } => {
                Err(ActionError::invalid(format!("the workload cannot start: {reason}")))
            }
            other => {
                Err(ActionError::retryable(format!("the guest agent answered {other:?} to a start request")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::UnixListener;

    use super::*;

    async fn fake_vmm(listener: UnixListener, reply: Reply) {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next_line().await.unwrap().unwrap(), format!("CONNECT {AGENT_PORT}"));
        writer.write_all(b"OK 1073741824\n").await.unwrap();
        let request: Request = decode(lines.next_line().await.unwrap().unwrap().as_bytes()).unwrap();
        assert_eq!(request, Request::Status);
        writer.write_all(&encode(&reply)).await.unwrap();
    }

    #[tokio::test]
    async fn the_workload_state_is_asked_through_the_hybrid_vsock_socket() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let vm_id = ObjectId::from_uuid(uuid::Uuid::from_u128(9));
        std::fs::create_dir_all(runtime.vm(&vm_id).path()).unwrap();
        let listener = UnixListener::bind(runtime.vm(&vm_id).vsock_socket()).unwrap();
        let vmm =
            tokio::spawn(fake_vmm(listener, Reply::Status { workload: WorkloadState::Exited { code: 4 } }));
        let agents = HybridVsockAgents::new(runtime);
        assert_eq!(agents.workload(&vm_id).await.unwrap(), WorkloadState::Exited { code: 4 });
        vmm.await.unwrap();
    }

    #[tokio::test]
    async fn an_absent_agent_is_a_retryable_failure() {
        let root = tempfile::tempdir().unwrap();
        let agents = HybridVsockAgents::new(RuntimeDirectory::new(root.path()));
        let failure = agents.workload(&ObjectId::from_uuid(uuid::Uuid::from_u128(1))).await.unwrap_err();
        assert!(failure.is_retryable());
    }

    #[tokio::test]
    async fn a_guest_that_does_not_accept_yet_is_given_up_on_quickly() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let vm_id = ObjectId::from_uuid(uuid::Uuid::from_u128(5));
        std::fs::create_dir_all(runtime.vm(&vm_id).path()).unwrap();
        let listener = UnixListener::bind(runtime.vm(&vm_id).vsock_socket()).unwrap();
        let silent_vmm = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(EXCHANGE_TIMEOUT).await;
            drop(stream);
        });
        let agents = HybridVsockAgents::new(runtime);
        let started = std::time::Instant::now();
        let failure = agents.workload(&vm_id).await.unwrap_err();
        assert!(failure.is_retryable());
        assert!(started.elapsed() < EXCHANGE_TIMEOUT / 2);
        silent_vmm.abort();
    }
}
