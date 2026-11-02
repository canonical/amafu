use std::time::Duration;

use amafu_domain::ObjectId;
use amafu_guest_agent::protocol::{AGENT_PORT, MAXIMUM_MESSAGE_BYTES, Reply, Request, decode, encode};
use amafu_interfaces::{ActionError, GuestAgents, WorkloadSpecification, WorkloadState};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::runtime::RuntimeDirectory;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);
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

impl HybridVsockAgents {
    #[must_use]
    pub fn new(runtime: RuntimeDirectory) -> Self {
        Self { runtime }
    }

    async fn exchange_unbounded(&self, vm_id: &ObjectId, request: &Request) -> Result<Reply, ActionError> {
        let stream = UnixStream::connect(self.runtime.vm(vm_id).vsock_socket()).await.map_err(unreachable)?;
        let (reader, mut writer) = stream.into_split();
        let limit = u64::try_from(MAXIMUM_MESSAGE_BYTES * 2).unwrap_or(u64::MAX);
        let mut reader = BufReader::new(reader.take(limit));
        writer.write_all(format!("CONNECT {AGENT_PORT}\n").as_bytes()).await.map_err(unreachable)?;
        let accepted = read_line(&mut reader).await?;
        if !accepted.starts_with(ACCEPTED_PREFIX) {
            return Err(unreachable(format!("the VMM refused the connection: {}", accepted.trim())));
        }
        writer.write_all(&encode(request)).await.map_err(unreachable)?;
        let reply = read_line(&mut reader).await?;
        decode(reply.as_bytes()).map_err(unreachable)
    }

    async fn exchange(&self, vm_id: &ObjectId, request: &Request) -> Result<Reply, ActionError> {
        tokio::time::timeout(EXCHANGE_TIMEOUT, self.exchange_unbounded(vm_id, request))
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
}
