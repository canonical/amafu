use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};

use amafu_domain::MacAddress;
use amafu_interfaces::network::{bridge_name, tunnel_name};
use amafu_interfaces::{ActionError, NodeNetworkState};
use futures::TryStreamExt;
use rtnetlink::packet_route::AddressFamily;
use rtnetlink::packet_route::neighbour::{
    NeighbourAddress, NeighbourAttribute, NeighbourFlags, NeighbourMessage,
};
use rtnetlink::{Handle, LinkUnspec, LinkVxlan};

pub const TUNNEL_PREFIX: &str = "amx";
pub const VXLAN_PORT: u16 = 4789;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tunnel {
    name: String,
    bridge: String,
    segment_id: u32,
    remotes: BTreeMap<MacAddress, (Ipv4Addr, Ipv4Addr)>,
}

fn netlink_failure(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("netlink: {error}"))
}

fn wanted_tunnels(desired: &NodeNetworkState) -> Vec<Tunnel> {
    if desired.underlay_address.is_none() {
        return Vec::new();
    }
    desired
        .segments
        .iter()
        .filter_map(|segment| {
            let segment_id = segment.segment_id?;
            let remotes = desired
                .remote_endpoints
                .iter()
                .filter(|remote| remote.segment == segment.key)
                .map(|remote| (remote.mac, (remote.address, remote.node_address)))
                .collect();
            Some(Tunnel {
                name: tunnel_name(&segment.key, segment_id),
                bridge: bridge_name(&segment.key),
                segment_id: segment_id.value(),
                remotes,
            })
        })
        .collect()
}

#[must_use]
pub fn wanted_tunnel_names(desired: &NodeNetworkState) -> Vec<String> {
    wanted_tunnels(desired).into_iter().map(|tunnel| tunnel.name).collect()
}

fn index_of(links: &BTreeMap<String, u32>, name: &str) -> Result<u32, ActionError> {
    links.get(name).copied().ok_or_else(|| ActionError::retryable(format!("link {name} is missing")))
}

pub async fn ensure_tunnels(
    handle: &Handle,
    desired: &NodeNetworkState,
    underlay: &str,
    links: &mut BTreeMap<String, u32>,
    refresh: impl AsyncFn(&Handle) -> Result<BTreeMap<String, u32>, ActionError>,
) -> Result<(), ActionError> {
    let Some(local) = desired.underlay_address else {
        return Ok(());
    };
    let underlay_index = index_of(links, underlay)?;
    for tunnel in wanted_tunnels(desired) {
        if !links.contains_key(&tunnel.name) {
            let link = LinkVxlan::new(&tunnel.name, tunnel.segment_id)
                .dev(underlay_index)
                .local(local)
                .port(VXLAN_PORT)
                .learning(false)
                .proxy(true)
                .build();
            match handle.link().add(link).execute().await {
                Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == -17 => {}
                other => other.map_err(netlink_failure)?,
            }
            *links = refresh(handle).await?;
        }
        let index = index_of(links, &tunnel.name)?;
        let bridge = index_of(links, &tunnel.bridge)?;
        handle
            .link()
            .set(LinkUnspec::new_with_index(index).controller(bridge).up().build())
            .execute()
            .await
            .map_err(netlink_failure)?;
        converge_tunnel_entries(handle, index, &tunnel).await?;
    }
    Ok(())
}

fn link_layer_address(message: &NeighbourMessage) -> Option<MacAddress> {
    message.attributes.iter().find_map(|attribute| match attribute {
        NeighbourAttribute::LinkLayerAddress(bytes) => {
            <[u8; 6]>::try_from(bytes.as_slice()).ok().map(MacAddress::new)
        }
        _ => None,
    })
}

fn destination(message: &NeighbourMessage) -> Option<Ipv4Addr> {
    message.attributes.iter().find_map(|attribute| match attribute {
        NeighbourAttribute::Destination(NeighbourAddress::Inet(address)) => Some(*address),
        _ => None,
    })
}

async fn entries_of(
    handle: &Handle,
    index: u32,
    family: AddressFamily,
) -> Result<Vec<NeighbourMessage>, ActionError> {
    let messages: Vec<NeighbourMessage> = handle
        .neighbours()
        .get()
        .set_address_family(family)
        .execute()
        .try_collect()
        .await
        .map_err(netlink_failure)?;
    Ok(messages.into_iter().filter(|message| message.header.ifindex == index).collect())
}

async fn converge_tunnel_entries(handle: &Handle, index: u32, tunnel: &Tunnel) -> Result<(), ActionError> {
    let forwarding = entries_of(handle, index, AddressFamily::Bridge).await?;
    let mut programmed: BTreeSet<(MacAddress, Ipv4Addr)> = BTreeSet::new();
    for entry in forwarding.into_iter().filter(|entry| entry.header.flags.contains(NeighbourFlags::Own)) {
        let (Some(mac), Some(node)) = (link_layer_address(&entry), destination(&entry)) else {
            continue;
        };
        if tunnel.remotes.get(&mac).is_some_and(|(_, wanted_node)| *wanted_node == node) {
            programmed.insert((mac, node));
        } else {
            handle.neighbours().del(entry).execute().await.map_err(netlink_failure)?;
        }
    }
    for (mac, (_, node)) in &tunnel.remotes {
        if !programmed.contains(&(*mac, *node)) {
            handle
                .neighbours()
                .add_bridge(index, &mac.octets())
                .destination(IpAddr::V4(*node))
                .flags(NeighbourFlags::Own)
                .replace()
                .execute()
                .await
                .map_err(netlink_failure)?;
        }
    }
    let neighbours = entries_of(handle, index, AddressFamily::Inet).await?;
    let wanted: BTreeMap<Ipv4Addr, MacAddress> =
        tunnel.remotes.iter().map(|(mac, (address, _))| (*address, *mac)).collect();
    let mut answered: BTreeSet<Ipv4Addr> = BTreeSet::new();
    for entry in neighbours {
        let (Some(address), mac) = (destination(&entry), link_layer_address(&entry)) else {
            continue;
        };
        if mac.is_some() && wanted.get(&address) == mac.as_ref() {
            answered.insert(address);
        } else {
            handle.neighbours().del(entry).execute().await.map_err(netlink_failure)?;
        }
    }
    for (address, mac) in wanted.iter().filter(|(address, _)| !answered.contains(address)) {
        handle
            .neighbours()
            .add(index, IpAddr::V4(*address))
            .link_layer_address(&mac.octets())
            .replace()
            .execute()
            .await
            .map_err(netlink_failure)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use amafu_domain::SegmentId;
    use amafu_interfaces::{RemoteEndpoint, Segment, SegmentKey};

    use super::*;

    fn state(segment_id: Option<SegmentId>, underlay: Option<Ipv4Addr>) -> NodeNetworkState {
        let key = SegmentKey::of("project-a", "default");
        NodeNetworkState {
            segments: vec![Segment {
                key: key.clone(),
                subnet: "10.1.0.0/24".parse().unwrap(),
                is_outbound_access_allowed: true,
                domain: String::from("default.project-a.internal"),
                segment_id,
            }],
            remote_endpoints: vec![RemoteEndpoint {
                segment: key,
                mac: MacAddress::new([2, 0, 0, 0, 0, 9]),
                address: Ipv4Addr::new(10, 1, 0, 9),
                hostname: String::from("database"),
                node_address: Ipv4Addr::new(192, 0, 2, 11),
            }],
            underlay_address: underlay,
            ..NodeNetworkState::default()
        }
    }

    #[test]
    fn each_segment_with_an_identifier_gets_a_tunnel_to_its_remote_ports() {
        let tunnels = wanted_tunnels(&state(SegmentId::new(7).ok(), Some(Ipv4Addr::new(192, 0, 2, 10))));
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].segment_id, 7);
        assert!(tunnels[0].name.starts_with(TUNNEL_PREFIX));
        assert_eq!(
            tunnels[0].remotes.get(&MacAddress::new([2, 0, 0, 0, 0, 9])),
            Some(&(Ipv4Addr::new(10, 1, 0, 9), Ipv4Addr::new(192, 0, 2, 11)))
        );
    }

    #[test]
    fn no_tunnel_without_an_identifier_or_an_underlay_address() {
        assert_eq!(wanted_tunnels(&state(None, Some(Ipv4Addr::new(192, 0, 2, 10)))), Vec::new());
        assert_eq!(wanted_tunnels(&state(SegmentId::new(7).ok(), None)), Vec::new());
    }
}
