use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use amafu_interfaces::network::bridge_name;
use amafu_interfaces::{LocalServices, NodeNetworkState, SegmentKey};
use axum::extract::{ConnectInfo, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::metadata::METADATA_ADDRESS;
use crate::{dhcp, dns, metadata};

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DNS_PORT: u16 = 53;
const METADATA_PORT: u16 = 80;
const MAXIMUM_DATAGRAM_BYTES: usize = 1500;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_secs(1);

type SharedState = Arc<RwLock<NodeNetworkState>>;

pub struct LocalServicesServer {
    state: SharedState,
    upstream_resolvers: Vec<SocketAddr>,
    listeners: std::sync::Mutex<BTreeMap<SegmentKey, Vec<JoinHandle<()>>>>,
    runtime: tokio::runtime::Handle,
}

fn read(state: &SharedState) -> NodeNetworkState {
    state.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn udp_socket(address: SocketAddrV4, device: Option<&str>, is_broadcast: bool) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_freebind_v4(true)?;
    if let Some(device) = device {
        socket.bind_device(Some(device.as_bytes()))?;
    }
    socket.set_broadcast(is_broadcast)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(address).into())?;
    UdpSocket::from_std(socket.into())
}

async fn serve_dhcp(segment: SegmentKey, state: SharedState) {
    let bridge = bridge_name(&segment);
    let address = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DHCP_SERVER_PORT);
    let socket = loop {
        match udp_socket(address, Some(&bridge), true) {
            Ok(socket) => break socket,
            Err(error) => {
                tracing::debug!(%bridge, %error, "the DHCP server cannot bind yet");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    };
    let broadcast = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, DHCP_CLIENT_PORT));
    let mut buffer = vec![0_u8; MAXIMUM_DATAGRAM_BYTES];
    loop {
        let Ok((length, _)) = socket.recv_from(&mut buffer).await else {
            continue;
        };
        let Ok(request) = dhcp::DhcpMessage::parse(&buffer[..length]) else {
            continue;
        };
        if let Some(reply) = dhcp::answer(&request, &segment, &read(&state)) {
            let _ = socket.send_to(&reply.encode(), broadcast).await;
        }
    }
}

async fn forward(query: &[u8], resolvers: &[SocketAddr]) -> Option<Vec<u8>> {
    let upstream = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))).await.ok()?;
    let mut buffer = vec![0_u8; MAXIMUM_DATAGRAM_BYTES];
    for resolver in resolvers {
        if upstream.send_to(query, resolver).await.is_err() {
            continue;
        }
        if let Ok(Ok((length, from))) =
            tokio::time::timeout(UPSTREAM_TIMEOUT, upstream.recv_from(&mut buffer)).await
            && from == *resolver
        {
            return Some(buffer[..length].to_vec());
        }
    }
    None
}

async fn serve_dns(
    segment: SegmentKey,
    gateway: Ipv4Addr,
    state: SharedState,
    resolvers: Arc<Vec<SocketAddr>>,
) {
    let address = SocketAddrV4::new(gateway, DNS_PORT);
    let socket = loop {
        match udp_socket(address, None, false) {
            Ok(socket) => break Arc::new(socket),
            Err(error) => {
                tracing::debug!(%address, %error, "the DNS server cannot bind yet");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    };
    let mut buffer = vec![0_u8; MAXIMUM_DATAGRAM_BYTES];
    loop {
        let Ok((length, client)) = socket.recv_from(&mut buffer).await else {
            continue;
        };
        let bytes = buffer[..length].to_vec();
        let Ok(query) = dns::Query::parse(&bytes) else {
            continue;
        };
        match dns::answer(&query, &segment, &read(&state)) {
            dns::Decision::Answer(response) => {
                let _ = socket.send_to(&response, client).await;
            }
            dns::Decision::Forward if query.wants_recursion => {
                let (socket, resolvers) = (Arc::clone(&socket), Arc::clone(&resolvers));
                tokio::spawn(async move {
                    if let Some(response) = forward(&bytes, &resolvers).await {
                        let _ = socket.send_to(&response, client).await;
                    }
                });
            }
            dns::Decision::Forward => {}
        }
    }
}

async fn metadata_document(
    State(state): State<SharedState>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
    uri: Uri,
) -> Response {
    let SocketAddr::V4(client) = client else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match metadata::answer(&read(&state), *client.ip(), uri.path()) {
        Some(document) => ([(header::CONTENT_TYPE, document.content_type)], document.body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_metadata(state: SharedState) {
    let address = SocketAddr::V4(SocketAddrV4::new(METADATA_ADDRESS, METADATA_PORT));
    let listener = loop {
        let bound = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).and_then(|socket| {
            socket.set_reuse_address(true)?;
            socket.set_freebind_v4(true)?;
            socket.set_nonblocking(true)?;
            socket.bind(&address.into())?;
            socket.listen(128)?;
            tokio::net::TcpListener::from_std(socket.into())
        });
        match bound {
            Ok(listener) => break listener,
            Err(error) => {
                tracing::warn!(%address, %error, "the metadata server cannot bind yet");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    };
    let application = axum::Router::new().fallback(metadata_document).with_state(state);
    let service = application.into_make_service_with_connect_info::<SocketAddr>();
    if let Err(error) = axum::serve(listener, service).await {
        tracing::error!(%error, "the metadata server stopped");
    }
}

impl LocalServicesServer {
    #[must_use]
    pub fn start(upstream_resolvers: Vec<Ipv4Addr>, runtime: tokio::runtime::Handle) -> Self {
        let state: SharedState = Arc::new(RwLock::new(NodeNetworkState::default()));
        runtime.spawn(serve_metadata(Arc::clone(&state)));
        Self {
            state,
            upstream_resolvers: upstream_resolvers
                .into_iter()
                .map(|address| SocketAddr::V4(SocketAddrV4::new(address, DNS_PORT)))
                .collect(),
            listeners: std::sync::Mutex::new(BTreeMap::new()),
            runtime,
        }
    }
}

impl LocalServices for LocalServicesServer {
    fn update(&self, state: &NodeNetworkState) {
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = state.clone();
        let mut listeners = self.listeners.lock().unwrap_or_else(PoisonError::into_inner);
        listeners.retain(|key, tasks| {
            let is_wanted = state.segment(key).is_some();
            if !is_wanted {
                tasks.iter().for_each(JoinHandle::abort);
            }
            is_wanted
        });
        let resolvers = Arc::new(self.upstream_resolvers.clone());
        for segment in &state.segments {
            listeners.entry(segment.key.clone()).or_insert_with(|| {
                vec![
                    self.runtime.spawn(serve_dhcp(segment.key.clone(), Arc::clone(&self.state))),
                    self.runtime.spawn(serve_dns(
                        segment.key.clone(),
                        segment.gateway(),
                        Arc::clone(&self.state),
                        Arc::clone(&resolvers),
                    )),
                ]
            });
        }
    }
}
