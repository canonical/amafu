use std::sync::{Arc, Mutex, PoisonError};

use amafu_guest_agent::protocol::{AGENT_PORT, MAXIMUM_MESSAGE_BYTES, Reply, Request, decode, encode};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

use crate::workload::Workload;

pub type SharedWorkload = Arc<Mutex<Workload>>;

fn handle(workload: &SharedWorkload, request: Request) -> Reply {
    let mut workload = workload.lock().unwrap_or_else(PoisonError::into_inner);
    workload.reap();
    match request {
        Request::Status => Reply::Status { workload: workload.state() },
        Request::Start { workload: specification } => match workload.start(&specification) {
            Ok(()) => Reply::Started,
            Err(error) => Reply::Refused { reason: format!("{error:#}") },
        },
        Request::Stop => {
            workload.stop();
            Reply::Stopping
        }
    }
}

async fn serve_connection(stream: VsockStream, workload: SharedWorkload) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines =
        BufReader::new(reader.take(u64::try_from(MAXIMUM_MESSAGE_BYTES * 64).unwrap_or(u64::MAX))).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match decode::<Request>(line.as_bytes()) {
            Ok(request) => handle(&workload, request),
            Err(error) => Reply::Refused { reason: error.to_string() },
        };
        if writer.write_all(&encode(&reply)).await.is_err() {
            return;
        }
    }
}

pub async fn serve(workload: SharedWorkload) -> anyhow::Result<()> {
    let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, AGENT_PORT))?;
    tracing::info!(port = AGENT_PORT, "the guest agent is listening");
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(serve_connection(stream, Arc::clone(&workload)));
    }
}
