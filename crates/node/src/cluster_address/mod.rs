mod certificate;
mod election_role;
pub mod settings;

use std::net::IpAddr;
use std::sync::Arc;

use amafu_api::server::CertificateByLocalAddress;
use amafu_interfaces::Store;
use amafu_node_configuration::NodeConfiguration;
use tokio::sync::watch;
use tokio::task::JoinSet;

pub use election_role::AddressClaim;
use election_role::ElectionRole;
use settings::{ClusterSettings, LocalCopy, keep_settings_current};

use crate::roles::supervise;

fn peers(configuration: &NodeConfiguration) -> Vec<IpAddr> {
    let me = configuration.state.listen.ip();
    configuration.state.voters.iter().map(std::net::SocketAddr::ip).filter(|voter| *voter != me).collect()
}

pub fn start(
    roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    store: Arc<dyn Store>,
    api_identities: Arc<CertificateByLocalAddress>,
) -> (Arc<AddressClaim>, watch::Receiver<ClusterSettings>) {
    let local_copy = LocalCopy::in_directory(&configuration.directories.data);
    let (sender, settings) = watch::channel(local_copy.load());
    let node = configuration.node.name.clone();
    roles.spawn(async move {
        let failure = keep_settings_current(store, node, local_copy, sender).await;
        tracing::error!(%failure, "the cluster address settings are no longer kept");
    });
    let presented = settings.clone();
    roles.spawn(supervise("cluster-address-certificate", move || {
        certificate::present_on_the_api(Arc::clone(&api_identities), presented.clone())
    }));
    let claim = Arc::new(AddressClaim::default());
    let election = Arc::new(ElectionRole {
        me: configuration.node.name.clone(),
        peers: peers(configuration),
        settings: settings.clone(),
        claim: Arc::clone(&claim),
    });
    roles.spawn(supervise("cluster-address", move || {
        let election = Arc::clone(&election);
        async move { election.take_part().await }
    }));
    (claim, settings)
}
