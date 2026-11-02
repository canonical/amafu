use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

use futures::TryStreamExt;
use rtnetlink::packet_route::link::{LinkAttribute, LinkFlags, LinkMessage};
use rtnetlink::{Handle, LinkUnspec, RouteMessageBuilder, new_connection};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const LOOPBACK: &str = "lo";
const ALREADY_EXISTS: i32 = -17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticAddress {
    pub address: Ipv4Addr,
    pub prefix_length: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an IPv4 address with a prefix length, such as 10.0.0.5/24")]
pub struct InvalidStaticAddress(String);

impl FromStr for StaticAddress {
    type Err = InvalidStaticAddress;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidStaticAddress(value.to_owned());
        let (address, prefix_length) = value.split_once('/').ok_or_else(invalid)?;
        let address = address.parse().map_err(|_| invalid())?;
        let prefix_length = prefix_length.parse().ok().filter(|length| *length <= 32).ok_or_else(invalid)?;
        Ok(Self { address, prefix_length })
    }
}

impl fmt::Display for StaticAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.address, self.prefix_length)
    }
}

impl Serialize for StaticAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StaticAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnderlayConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    pub address: StaticAddress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Ipv4Addr>,
}

#[derive(Debug, thiserror::Error)]
pub enum UnderlayError {
    #[error("the netlink socket cannot be opened: {0}")]
    Connection(std::io::Error),
    #[error("no network interface named {0} exists")]
    MissingInterface(String),
    #[error("no network interface other than loopback exists")]
    NoInterface,
    #[error("netlink refused a request: {0}")]
    Netlink(#[from] rtnetlink::Error),
}

fn link_name(link: &LinkMessage) -> Option<&str> {
    link.attributes.iter().find_map(|attribute| match attribute {
        LinkAttribute::IfName(name) => Some(name.as_str()),
        _ => None,
    })
}

fn ignore_existing(result: Result<(), rtnetlink::Error>) -> Result<(), UnderlayError> {
    match result {
        Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == ALREADY_EXISTS => Ok(()),
        other => Ok(other?),
    }
}

async fn links(handle: &Handle) -> Result<Vec<LinkMessage>, UnderlayError> {
    Ok(handle.link().get().execute().try_collect().await?)
}

async fn bring_up_index(handle: &Handle, index: u32) -> Result<(), UnderlayError> {
    Ok(handle.link().set(LinkUnspec::new_with_index(index).up().build()).execute().await?)
}

fn choose_interface<'links>(
    links: &'links [LinkMessage],
    wanted: Option<&str>,
) -> Result<&'links LinkMessage, UnderlayError> {
    match wanted {
        Some(name) => links
            .iter()
            .find(|link| link_name(link) == Some(name))
            .ok_or_else(|| UnderlayError::MissingInterface(name.to_owned())),
        None => links
            .iter()
            .find(|link| !link.header.flags.contains(LinkFlags::Loopback))
            .ok_or(UnderlayError::NoInterface),
    }
}

pub async fn bring_up(interface: Option<&str>) -> Result<String, UnderlayError> {
    let (connection, handle, _) = new_connection().map_err(UnderlayError::Connection)?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        let all_links = links(&handle).await?;
        if let Some(loopback) = all_links.iter().find(|link| link_name(link) == Some(LOOPBACK)) {
            bring_up_index(&handle, loopback.header.index).await?;
        }
        let chosen = choose_interface(&all_links, interface)?;
        bring_up_index(&handle, chosen.header.index).await?;
        Ok(link_name(chosen).unwrap_or_default().to_owned())
    }
    .await;
    connection_task.abort();
    result
}

pub async fn configure_underlay(configuration: &UnderlayConfiguration) -> Result<String, UnderlayError> {
    let (connection, handle, _) = new_connection().map_err(UnderlayError::Connection)?;
    let connection_task = tokio::spawn(connection);
    let result = apply(&handle, configuration).await;
    connection_task.abort();
    result
}

async fn apply(handle: &Handle, configuration: &UnderlayConfiguration) -> Result<String, UnderlayError> {
    let all_links = links(handle).await?;
    if let Some(loopback) = all_links.iter().find(|link| link_name(link) == Some(LOOPBACK)) {
        bring_up_index(handle, loopback.header.index).await?;
    }
    let interface = choose_interface(&all_links, configuration.interface.as_deref())?;
    let index = interface.header.index;
    let name = link_name(interface).unwrap_or_default().to_owned();
    bring_up_index(handle, index).await?;
    let address = configuration.address;
    ignore_existing(
        handle.address().add(index, IpAddr::V4(address.address), address.prefix_length).execute().await,
    )?;
    if let Some(gateway) = configuration.gateway {
        let route = RouteMessageBuilder::<Ipv4Addr>::new().gateway(gateway).output_interface(index).build();
        ignore_existing(handle.route().add(route).execute().await)?;
    }
    tracing::info!(interface = %name, %address, gateway = ?configuration.gateway, "the underlay is configured");
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_addresses_parse_and_print() {
        let address: StaticAddress = "10.215.2.50/24".parse().unwrap();
        assert_eq!(address, StaticAddress { address: Ipv4Addr::new(10, 215, 2, 50), prefix_length: 24 });
        assert_eq!(address.to_string(), "10.215.2.50/24");
        for invalid in ["10.215.2.50", "10.215.2.50/33", "nonsense/24", "10.215.2/24"] {
            assert!(invalid.parse::<StaticAddress>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn the_configuration_reads_from_yaml() {
        let configuration: UnderlayConfiguration =
            serde_yaml_ng::from_str("address: 10.215.2.50/24\ngateway: 10.215.2.1\n").unwrap();
        assert_eq!(configuration.interface, None);
        assert_eq!(configuration.gateway, Some(Ipv4Addr::new(10, 215, 2, 1)));
    }
}
