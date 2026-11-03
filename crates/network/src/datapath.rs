use std::collections::BTreeMap;
use std::net::IpAddr;
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};

use amafu_interfaces::network::bridge_name;
use amafu_interfaces::{
    ActionError, Datapath, ExternalTool, NodeNetworkState, PortKey, RealizedNetwork, SegmentKey,
};
use futures::TryStreamExt;
use rtnetlink::packet_route::link::{LinkAttribute, LinkMessage};
use rtnetlink::{Handle, LinkBridge, LinkUnspec, new_connection};
use tokio::io::AsyncWriteExt;

use crate::announcement::announce;
use crate::floating::converge_floating_addresses;
use crate::metadata::METADATA_ADDRESS;
use crate::nftables::compile_ruleset;
use crate::overlay::{TUNNEL_PREFIX, ensure_tunnels, wanted_tunnel_names};

const BRIDGE_PREFIX: &str = "amb";
const TAP_PREFIX: &str = "amt";
const ALREADY_EXISTS: i32 = -17;
const FORWARDING: &str = "/proc/sys/net/ipv4/ip_forward";

#[derive(Default)]
struct KnownNames {
    segments: BTreeMap<String, SegmentKey>,
    ports: BTreeMap<String, PortKey>,
}

pub struct KernelDatapath {
    underlay: String,
    nft: ExternalTool,
    known: Mutex<KnownNames>,
}

fn netlink_failure(error: impl std::fmt::Display) -> ActionError {
    ActionError::retryable(format!("netlink: {error}"))
}

fn link_name(link: &LinkMessage) -> Option<&str> {
    link.attributes.iter().find_map(|attribute| match attribute {
        LinkAttribute::IfName(name) => Some(name.as_str()),
        _ => None,
    })
}

fn ignore_existing(result: Result<(), rtnetlink::Error>) -> Result<(), ActionError> {
    match result {
        Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == ALREADY_EXISTS => Ok(()),
        other => other.map_err(netlink_failure),
    }
}

async fn links(handle: &Handle) -> Result<BTreeMap<String, u32>, ActionError> {
    let messages: Vec<LinkMessage> =
        handle.link().get().execute().try_collect().await.map_err(netlink_failure)?;
    Ok(messages
        .iter()
        .filter_map(|link| link_name(link).map(|name| (name.to_owned(), link.header.index)))
        .collect())
}

fn create_persistent_tap(name: &str) -> Result<(), ActionError> {
    let failure = |error: std::io::Error| ActionError::retryable(format!("creating tap {name}: {error}"));
    let device = tun_rs::DeviceBuilder::new()
        .name(name)
        .layer(tun_rs::Layer::L2)
        .offload(true)
        .enable(true)
        .build_sync()
        .map_err(failure)?;
    device.persist().map_err(failure)
}

impl KernelDatapath {
    #[must_use]
    pub fn new(underlay: String, nft: ExternalTool) -> Self {
        Self { underlay, nft, known: Mutex::new(KnownNames::default()) }
    }

    async fn ensure_bridges(
        &self,
        handle: &Handle,
        desired: &NodeNetworkState,
    ) -> Result<BTreeMap<String, u32>, ActionError> {
        let mut existing = links(handle).await?;
        for segment in &desired.segments {
            let name = bridge_name(&segment.key);
            if !existing.contains_key(&name) {
                ignore_existing(handle.link().add(LinkBridge::new(&name).build()).execute().await)?;
                existing = links(handle).await?;
            }
            let index = *existing
                .get(&name)
                .ok_or_else(|| ActionError::retryable(format!("bridge {name} vanished")))?;
            handle
                .link()
                .set(LinkUnspec::new_with_index(index).up().build())
                .execute()
                .await
                .map_err(netlink_failure)?;
            let gateway = IpAddr::V4(segment.gateway());
            ignore_existing(
                handle.address().add(index, gateway, segment.subnet.prefix_length()).execute().await,
            )?;
            ignore_existing(handle.address().add(index, IpAddr::V4(METADATA_ADDRESS), 32).execute().await)?;
        }
        Ok(existing)
    }

    async fn ensure_taps(&self, handle: &Handle, desired: &NodeNetworkState) -> Result<(), ActionError> {
        let mut existing = links(handle).await?;
        for port in &desired.ports {
            let name = port.key.tap_name();
            if !existing.contains_key(&name) {
                create_persistent_tap(&name)?;
                existing = links(handle).await?;
            }
            let index =
                *existing.get(&name).ok_or_else(|| ActionError::retryable(format!("tap {name} vanished")))?;
            let bridge = bridge_name(&port.segment);
            let bridge_index = *existing
                .get(&bridge)
                .ok_or_else(|| ActionError::retryable(format!("bridge {bridge} is missing")))?;
            handle
                .link()
                .set(LinkUnspec::new_with_index(index).controller(bridge_index).up().build())
                .execute()
                .await
                .map_err(netlink_failure)?;
        }
        Ok(())
    }

    async fn remove_unwanted(&self, handle: &Handle, desired: &NodeNetworkState) -> Result<(), ActionError> {
        let mut wanted: Vec<String> =
            desired.segments.iter().map(|segment| bridge_name(&segment.key)).collect();
        wanted.extend(desired.ports.iter().map(|port| port.key.tap_name()));
        wanted.extend(wanted_tunnel_names(desired));
        let existing = links(handle).await?;
        let owned = |name: &str| {
            [BRIDGE_PREFIX, TAP_PREFIX, TUNNEL_PREFIX].iter().any(|prefix| name.starts_with(prefix))
        };
        let mut stale: Vec<(&String, &u32)> =
            existing.iter().filter(|(name, _)| owned(name) && !wanted.contains(name)).collect();
        stale.sort_by_key(|(name, _)| !name.starts_with(TAP_PREFIX));
        for (name, index) in stale {
            tracing::info!(link = %name, "removing a link that is no longer wanted");
            handle.link().del(*index).execute().await.map_err(netlink_failure)?;
        }
        Ok(())
    }

    async fn apply_ruleset(&self, desired: &NodeNetworkState) -> Result<(), ActionError> {
        let ruleset = compile_ruleset(desired);
        let mut nft = tokio::process::Command::from(self.nft.command())
            .args(["--file", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| ActionError::retryable(format!("starting nft: {error}")))?;
        if let Some(mut input) = nft.stdin.take() {
            input
                .write_all(ruleset.as_bytes())
                .await
                .map_err(|error| ActionError::retryable(format!("writing to nft: {error}")))?;
        }
        let output = nft
            .wait_with_output()
            .await
            .map_err(|error| ActionError::retryable(format!("running nft: {error}")))?;
        if !output.status.success() {
            return Err(ActionError::fatal(format!(
                "nft refused the ruleset: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    fn remember(&self, desired: &NodeNetworkState) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        known.segments =
            desired.segments.iter().map(|segment| (bridge_name(&segment.key), segment.key.clone())).collect();
        known.ports = desired.ports.iter().map(|port| (port.key.tap_name(), port.key)).collect();
    }
}

#[async_trait::async_trait]
impl Datapath for KernelDatapath {
    async fn converge(&self, desired: &NodeNetworkState) -> Result<RealizedNetwork, ActionError> {
        tokio::fs::write(FORWARDING, b"1")
            .await
            .map_err(|error| ActionError::retryable(format!("enabling forwarding: {error}")))?;
        let (connection, handle, _) = new_connection().map_err(netlink_failure)?;
        let connection_task = tokio::spawn(connection);
        let outcome = async {
            let mut existing = self.ensure_bridges(&handle, desired).await?;
            ensure_tunnels(&handle, desired, &self.underlay, &mut existing, links).await?;
            self.ensure_taps(&handle, desired).await?;
            let underlay_index = *existing.get(&self.underlay).ok_or_else(|| {
                ActionError::retryable(format!("the underlay {} is missing", self.underlay))
            })?;
            let announced = converge_floating_addresses(&handle, underlay_index, desired).await?;
            self.apply_ruleset(desired).await?;
            self.remove_unwanted(&handle, desired).await?;
            Ok::<_, ActionError>(announced)
        }
        .await;
        connection_task.abort();
        let announced = outcome?;
        if !announced.is_empty() {
            let underlay = self.underlay.clone();
            let announcement = tokio::task::spawn_blocking(move || announce(&underlay, &announced)).await;
            if let Ok(Err(error)) = announcement {
                tracing::warn!(%error, "floating addresses could not be announced");
            }
        }
        self.remember(desired);
        self.current().await
    }

    async fn current(&self) -> Result<RealizedNetwork, ActionError> {
        let (connection, handle, _) = new_connection().map_err(netlink_failure)?;
        let connection_task = tokio::spawn(connection);
        let existing = links(&handle).await;
        connection_task.abort();
        let existing = existing?;
        let known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(RealizedNetwork {
            segments: known
                .segments
                .iter()
                .filter(|(name, _)| existing.contains_key(*name))
                .map(|(_, key)| key.clone())
                .collect(),
            ports: known
                .ports
                .iter()
                .filter(|(name, _)| existing.contains_key(*name))
                .map(|(_, key)| *key)
                .collect(),
        })
    }
}
