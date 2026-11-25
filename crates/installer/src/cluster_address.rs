use std::net::IpAddr;

use amafu_api::{ApiClient, ClientError};
use amafu_domain::Resource;
use amafu_domain::kinds::cluster_address::{
    ClusterAddress, ClusterAddressCertificate, ClusterAddressCertificateSpecification,
    ClusterAddressSpecification, cluster_address_name,
};

use crate::certificates::{CertificateAuthority, IssuanceError};

const CERTIFICATE_NAME: &str = "amafu-cluster";

pub fn issue_certificate(
    authority: &CertificateAuthority,
    address: &ClusterAddressSpecification,
) -> Result<ClusterAddressCertificateSpecification, IssuanceError> {
    let issued = authority.issue_server(CERTIFICATE_NAME, &[IpAddr::V4(address.address)])?;
    Ok(ClusterAddressCertificateSpecification {
        certificate_pem: issued.certificate_pem,
        private_key_pem: issued.private_key_pem,
    })
}

async fn create_or_replace<R: Resource>(
    cluster: &ApiClient,
    specification: &R::Specification,
) -> Result<(), ClientError> {
    let name = cluster_address_name();
    match cluster.create::<R>(&name, specification).await {
        Err(error) if error.is_conflict() => {
            cluster.update::<R>(name.as_str(), specification, None).await.map(drop)
        }
        created => created.map(drop),
    }
}

pub async fn configure_cluster_address(
    cluster: &ApiClient,
    address: &ClusterAddressSpecification,
    certificate: &ClusterAddressCertificateSpecification,
) -> Result<(), ClientError> {
    create_or_replace::<ClusterAddressCertificate>(cluster, certificate).await?;
    create_or_replace::<ClusterAddress>(cluster, address).await
}
