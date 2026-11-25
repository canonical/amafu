use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use amafu_api::server::{CertificateByLocalAddress, tls_configuration};
use tokio::sync::watch;

use super::settings::ClusterSettings;

fn present(identities: &CertificateByLocalAddress, settings: &ClusterSettings) -> Option<Ipv4Addr> {
    let (address, certificate) = (settings.address.as_ref()?, settings.certificate.as_ref()?);
    match tls_configuration(certificate.certificate_pem.as_bytes(), certificate.private_key_pem.as_bytes()) {
        Ok(configuration) => {
            identities.present_at(IpAddr::V4(address.address), configuration);
            Some(address.address)
        }
        Err(error) => {
            tracing::warn!(%error, "the cluster address certificate cannot be used");
            None
        }
    }
}

pub async fn present_on_the_api(
    identities: Arc<CertificateByLocalAddress>,
    mut settings: watch::Receiver<ClusterSettings>,
) -> String {
    let mut presented: Option<Ipv4Addr> = None;
    loop {
        let current = settings.borrow_and_update().clone();
        if let Some(previous) = presented.take() {
            identities.forget(IpAddr::V4(previous));
        }
        presented = present(&identities, &current);
        if settings.changed().await.is_err() {
            return String::from("the cluster address settings are no longer kept");
        }
    }
}
