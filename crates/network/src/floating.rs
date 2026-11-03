use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use amafu_interfaces::{ActionError, NodeNetworkState};
use futures::TryStreamExt;
use rtnetlink::Handle;
use rtnetlink::packet_route::address::{AddressAttribute, AddressMessage};

const HOST_PREFIX_LENGTH: u8 = 32;

fn netlink_failure(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("netlink: {error}"))
}

fn ipv4_of(message: &AddressMessage) -> Option<Ipv4Addr> {
    message.attributes.iter().find_map(|attribute| match attribute {
        AddressAttribute::Local(IpAddr::V4(address)) | AddressAttribute::Address(IpAddr::V4(address)) => {
            Some(*address)
        }
        _ => None,
    })
}

#[must_use]
pub fn wanted_floating_addresses(desired: &NodeNetworkState) -> BTreeSet<Ipv4Addr> {
    desired.floating_addresses.iter().map(|floating| floating.address).collect()
}

pub async fn converge_floating_addresses(
    handle: &Handle,
    underlay_index: u32,
    desired: &NodeNetworkState,
) -> Result<Vec<Ipv4Addr>, ActionError> {
    let wanted = wanted_floating_addresses(desired);
    let assigned: Vec<AddressMessage> = handle
        .address()
        .get()
        .set_link_index_filter(underlay_index)
        .execute()
        .try_collect()
        .await
        .map_err(netlink_failure)?;
    let mut present = BTreeSet::new();
    for message in assigned {
        let Some(address) = ipv4_of(&message) else {
            continue;
        };
        let is_host_route = message.header.prefix_len == HOST_PREFIX_LENGTH;
        if !is_host_route || Some(address) == desired.underlay_address {
            continue;
        }
        if wanted.contains(&address) {
            present.insert(address);
        } else {
            tracing::info!(%address, "withdrawing a floating address");
            handle.address().del(message).execute().await.map_err(netlink_failure)?;
        }
    }
    let mut added = Vec::new();
    for address in wanted.difference(&present) {
        handle
            .address()
            .add(underlay_index, IpAddr::V4(*address), HOST_PREFIX_LENGTH)
            .execute()
            .await
            .map_err(netlink_failure)?;
        tracing::info!(%address, "serving a floating address");
        added.push(*address);
    }
    Ok(added)
}
