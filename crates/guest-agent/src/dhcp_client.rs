use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use amafu_domain::MacAddress;
use amafu_network::dhcp::{DhcpMessage, Lease, MessageType};
use amafu_network::{StaticAddress, UnderlayConfiguration, bring_up, configure_underlay};
use anyhow::{Context, bail};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
const ATTEMPTS: u32 = 10;
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const OPTION_REQUESTED_ADDRESS: u8 = 50;
const OPTION_SERVER_IDENTIFIER: u8 = 54;
const BROADCAST_FLAG: u16 = 0x8000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configured {
    pub interface: String,
    pub address: Ipv4Addr,
    pub lease: Lease,
}

fn socket(interface: &str) -> anyhow::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.bind_device(Some(interface.as_bytes()))?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, CLIENT_PORT)).into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

fn hardware_address(interface: &str) -> anyhow::Result<MacAddress> {
    let text = std::fs::read_to_string(format!("/sys/class/net/{interface}/address"))
        .context("reading the MAC address")?;
    Ok(text.trim().parse()?)
}

fn message(
    transaction: u32,
    mac: MacAddress,
    message_type: MessageType,
    options: Vec<(u8, Vec<u8>)>,
) -> DhcpMessage {
    DhcpMessage {
        is_reply: false,
        transaction,
        flags: BROADCAST_FLAG,
        client_address: Ipv4Addr::UNSPECIFIED,
        your_address: Ipv4Addr::UNSPECIFIED,
        relay_address: Ipv4Addr::UNSPECIFIED,
        hardware_address: mac,
        message_type,
        requested_address: None,
        server_identifier: None,
        lease: Lease::default(),
        options,
    }
}

async fn exchange(
    socket: &UdpSocket,
    request: &DhcpMessage,
    wanted: MessageType,
) -> anyhow::Result<Option<DhcpMessage>> {
    let server = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, SERVER_PORT));
    socket.send_to(&request.encode(), server).await?;
    let mut buffer = vec![0_u8; 1500];
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    while let Ok(received) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await {
        let (length, _) = received?;
        let Ok(reply) = DhcpMessage::parse(&buffer[..length]) else {
            continue;
        };
        let is_ours = reply.is_reply
            && reply.transaction == request.transaction
            && reply.hardware_address == request.hardware_address;
        if is_ours && (reply.message_type == wanted || reply.message_type == MessageType::NegativeAcknowledge)
        {
            return Ok(Some(reply));
        }
    }
    Ok(None)
}

async fn lease(socket: &UdpSocket, mac: MacAddress) -> anyhow::Result<DhcpMessage> {
    for attempt in 0..ATTEMPTS {
        let transaction = 0x414d_0000 | attempt;
        let Some(offer) = exchange(
            socket,
            &message(transaction, mac, MessageType::Discover, Vec::new()),
            MessageType::Offer,
        )
        .await?
        else {
            continue;
        };
        let mut options = vec![(OPTION_REQUESTED_ADDRESS, offer.your_address.octets().to_vec())];
        if let Some(server) = offer.server_identifier {
            options.push((OPTION_SERVER_IDENTIFIER, server.octets().to_vec()));
        }
        let request = message(transaction, mac, MessageType::Request, options);
        if let Some(acknowledgement) = exchange(socket, &request, MessageType::Acknowledge).await?
            && acknowledgement.message_type == MessageType::Acknowledge
        {
            return Ok(acknowledgement);
        }
    }
    bail!("no DHCP server answered")
}

pub async fn configure(interface: Option<&str>) -> anyhow::Result<Configured> {
    let interface = bring_up(interface).await?;
    let mac = hardware_address(&interface)?;
    let socket = socket(&interface)?;
    let acknowledgement = lease(&socket, mac).await?;
    let prefix_length = acknowledgement.lease.prefix_length().context("the lease has no subnet mask")?;
    configure_underlay(&UnderlayConfiguration {
        mac: None,
        interface: Some(interface.clone()),
        address: StaticAddress { address: acknowledgement.your_address, prefix_length },
        gateway: acknowledgement.lease.router,
    })
    .await?;
    Ok(Configured { interface, address: acknowledgement.your_address, lease: acknowledgement.lease })
}
