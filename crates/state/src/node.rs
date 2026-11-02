use std::net::SocketAddr;
use std::path::Path;

use quorium::Quorium;

use crate::log_repair::discard_zeroed_tail;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("the state store could not start: {0}")]
    Start(#[from] quorium::StartError),
    #[error("the state log could not be repaired: {0}")]
    Repair(#[from] std::io::Error),
}

pub async fn start_single_voter(data_directory: &Path, bind: SocketAddr) -> Result<Quorium, StateError> {
    discard_zeroed_tail(data_directory)?;
    Ok(Quorium::builder()
        .data_dir(data_directory)
        .bind(bind)
        .single_node_dev_mode()
        .insecure_plaintext()
        .start()
        .await?)
}
