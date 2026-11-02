use std::net::SocketAddr;
use std::path::Path;

use quorium::Quorium;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("the state store could not start: {0}")]
    Start(#[from] quorium::StartError),
}

pub async fn start_single_voter(data_directory: &Path, bind: SocketAddr) -> Result<Quorium, StateError> {
    Ok(Quorium::builder()
        .data_dir(data_directory)
        .bind(bind)
        .single_node_dev_mode()
        .insecure_plaintext()
        .start()
        .await?)
}
