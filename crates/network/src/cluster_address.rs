use std::net::{IpAddr, Ipv4Addr};

use amafu_domain::kinds::cluster_address::ClusterAddressSpecification;
use futures::TryStreamExt;
use rtnetlink::packet_route::address::{AddressAttribute, AddressMessage};
use rtnetlink::packet_route::link::{LinkAttribute, LinkMessage};
use rtnetlink::{Handle, new_connection};

use crate::announcement::announce;

#[derive(Debug, thiserror::Error)]
pub enum ClusterAddressError {
    #[error("netlink: {0}")]
    Netlink(String),
    #[error("no interface has an address in {0}, so the cluster address has nowhere to go")]
    NoInterfaceOnItsNetwork(String),
    #[error("there is no interface named {0}")]
    MissingInterface(String),
    #[error("announcing the cluster address failed: {0}")]
    Announcement(String),
}

fn netlink(error: impl std::fmt::Display) -> ClusterAddressError {
    ClusterAddressError::Netlink(error.to_string())
}

struct Interface {
    index: u32,
    name: String,
}

fn ipv4_of(message: &AddressMessage) -> Option<Ipv4Addr> {
    message.attributes.iter().find_map(|attribute| match attribute {
        AddressAttribute::Local(IpAddr::V4(address)) | AddressAttribute::Address(IpAddr::V4(address)) => {
            Some(*address)
        }
        _ => None,
    })
}

fn name_of(link: &LinkMessage) -> Option<String> {
    link.attributes.iter().find_map(|attribute| match attribute {
        LinkAttribute::IfName(name) => Some(name.clone()),
        _ => None,
    })
}

async fn addresses(handle: &Handle) -> Result<Vec<AddressMessage>, ClusterAddressError> {
    handle.address().get().execute().try_collect().await.map_err(netlink)
}

async fn links(handle: &Handle) -> Result<Vec<LinkMessage>, ClusterAddressError> {
    handle.link().get().execute().try_collect().await.map_err(netlink)
}

async fn interface_for(
    handle: &Handle,
    cluster: &ClusterAddressSpecification,
) -> Result<Interface, ClusterAddressError> {
    let links = links(handle).await?;
    let named = |index: u32| {
        links
            .iter()
            .find(|link| link.header.index == index)
            .and_then(name_of)
            .map(|name| Interface { index, name })
    };
    if let Some(wanted) = &cluster.interface {
        return links
            .iter()
            .find(|link| name_of(link).as_ref() == Some(wanted))
            .map(|link| Interface { index: link.header.index, name: wanted.clone() })
            .ok_or_else(|| ClusterAddressError::MissingInterface(wanted.clone()));
    }
    let network =
        cluster.network().ok_or_else(|| ClusterAddressError::NoInterfaceOnItsNetwork(cluster.to_string()))?;
    addresses(handle)
        .await?
        .iter()
        .filter(|message| {
            ipv4_of(message).is_some_and(|address| address != cluster.address && network.contains(address))
        })
        .find_map(|message| named(message.header.index))
        .ok_or_else(|| ClusterAddressError::NoInterfaceOnItsNetwork(network.to_string()))
}

async fn assigned(handle: &Handle, address: Ipv4Addr) -> Result<Vec<AddressMessage>, ClusterAddressError> {
    Ok(addresses(handle).await?.into_iter().filter(|message| ipv4_of(message) == Some(address)).collect())
}

async fn with_netlink<T>(
    work: impl AsyncFnOnce(&Handle) -> Result<T, ClusterAddressError>,
) -> Result<T, ClusterAddressError> {
    let (connection, handle, _) = new_connection().map_err(netlink)?;
    let connection_task = tokio::spawn(connection);
    let result = work(&handle).await;
    connection_task.abort();
    result
}

pub async fn hold_cluster_address(cluster: &ClusterAddressSpecification) -> Result<(), ClusterAddressError> {
    let interface = with_netlink(async |handle| {
        let interface = interface_for(handle, cluster).await?;
        if assigned(handle, cluster.address).await?.is_empty() {
            handle
                .address()
                .add(interface.index, IpAddr::V4(cluster.address), cluster.prefix_length)
                .execute()
                .await
                .map_err(netlink)?;
            tracing::info!(address = %cluster, interface = %interface.name, "holding the cluster address");
        }
        Ok(interface)
    })
    .await?;
    announce(&interface.name, &[cluster.address])
        .map_err(|error| ClusterAddressError::Announcement(error.to_string()))
}

pub async fn release_cluster_address(address: Ipv4Addr) -> Result<(), ClusterAddressError> {
    with_netlink(async |handle| {
        for message in assigned(handle, address).await? {
            handle.address().del(message).execute().await.map_err(netlink)?;
            tracing::info!(%address, "released the cluster address");
        }
        Ok(())
    })
    .await
}
