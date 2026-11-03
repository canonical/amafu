use std::net::Ipv4Addr;

use amafu_domain::MacAddress;
use amafu_interfaces::{NodeNetworkState, SegmentKey};

const FIXED_HEADER_BYTES: usize = 236;
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const MINIMUM_REPLY_BYTES: usize = 300;
const BOOT_REQUEST: u8 = 1;
const BOOT_REPLY: u8 = 2;
const ETHERNET: u8 = 1;
const ETHERNET_ADDRESS_LENGTH: u8 = 6;
const LEASE_SECONDS: u32 = 3600;
const MTU: u16 = 1500;

const OPTION_PAD: u8 = 0;
const OPTION_SUBNET_MASK: u8 = 1;
const OPTION_ROUTER: u8 = 3;
const OPTION_DNS_SERVERS: u8 = 6;
const OPTION_HOSTNAME: u8 = 12;
const OPTION_DOMAIN_NAME: u8 = 15;
const OPTION_INTERFACE_MTU: u8 = 26;
const OPTION_REQUESTED_ADDRESS: u8 = 50;
const OPTION_LEASE_TIME: u8 = 51;
const OPTION_MESSAGE_TYPE: u8 = 53;
const OPTION_SERVER_IDENTIFIER: u8 = 54;
const OPTION_END: u8 = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Discover,
    Offer,
    Request,
    Decline,
    Acknowledge,
    NegativeAcknowledge,
    Release,
    Inform,
}

impl MessageType {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Discover,
            2 => Self::Offer,
            3 => Self::Request,
            4 => Self::Decline,
            5 => Self::Acknowledge,
            6 => Self::NegativeAcknowledge,
            7 => Self::Release,
            8 => Self::Inform,
            _ => return None,
        })
    }

    fn code(self) -> u8 {
        match self {
            Self::Discover => 1,
            Self::Offer => 2,
            Self::Request => 3,
            Self::Decline => 4,
            Self::Acknowledge => 5,
            Self::NegativeAcknowledge => 6,
            Self::Release => 7,
            Self::Inform => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the DHCP message is malformed: {0}")]
pub struct MalformedMessage(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpMessage {
    pub is_reply: bool,
    pub transaction: u32,
    pub flags: u16,
    pub client_address: Ipv4Addr,
    pub your_address: Ipv4Addr,
    pub relay_address: Ipv4Addr,
    pub hardware_address: MacAddress,
    pub message_type: MessageType,
    pub requested_address: Option<Ipv4Addr>,
    pub server_identifier: Option<Ipv4Addr>,
    pub lease: Lease,
    pub options: Vec<(u8, Vec<u8>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lease {
    pub subnet_mask: Option<Ipv4Addr>,
    pub router: Option<Ipv4Addr>,
    pub dns_servers: Vec<Ipv4Addr>,
    pub domain_name: Option<String>,
}

impl Lease {
    #[must_use]
    pub fn prefix_length(&self) -> Option<u8> {
        self.subnet_mask.map(|mask| u8::try_from(u32::from(mask).count_ones()).unwrap_or(32))
    }
}

fn address_at(bytes: &[u8], offset: usize) -> Ipv4Addr {
    Ipv4Addr::new(bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3])
}

#[must_use]
pub fn is_domain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
}

fn option_address(value: &[u8]) -> Option<Ipv4Addr> {
    let octets: [u8; 4] = value.try_into().ok()?;
    Some(Ipv4Addr::from(octets))
}

impl DhcpMessage {
    pub fn parse(bytes: &[u8]) -> Result<Self, MalformedMessage> {
        if bytes.len() < FIXED_HEADER_BYTES + MAGIC_COOKIE.len() {
            return Err(MalformedMessage("it is too short"));
        }
        if bytes[0] != BOOT_REQUEST && bytes[0] != BOOT_REPLY {
            return Err(MalformedMessage("the operation is unknown"));
        }
        if bytes[1] != ETHERNET || bytes[2] != ETHERNET_ADDRESS_LENGTH {
            return Err(MalformedMessage("only Ethernet clients are served"));
        }
        if bytes[FIXED_HEADER_BYTES..FIXED_HEADER_BYTES + 4] != MAGIC_COOKIE {
            return Err(MalformedMessage("the magic cookie is missing"));
        }
        let mut hardware = [0_u8; 6];
        hardware.copy_from_slice(&bytes[28..34]);
        let mut message_type = None;
        let mut requested_address = None;
        let mut server_identifier = None;
        let mut lease = Lease::default();
        let mut offset = FIXED_HEADER_BYTES + MAGIC_COOKIE.len();
        while offset < bytes.len() {
            let code = bytes[offset];
            if code == OPTION_END {
                break;
            }
            if code == OPTION_PAD {
                offset += 1;
                continue;
            }
            let length =
                usize::from(*bytes.get(offset + 1).ok_or(MalformedMessage("an option has no length"))?);
            let value = bytes
                .get(offset + 2..offset + 2 + length)
                .ok_or(MalformedMessage("an option runs past the end"))?;
            match code {
                OPTION_MESSAGE_TYPE => message_type = value.first().copied().and_then(MessageType::from_code),
                OPTION_REQUESTED_ADDRESS => requested_address = option_address(value),
                OPTION_SERVER_IDENTIFIER => server_identifier = option_address(value),
                OPTION_SUBNET_MASK => lease.subnet_mask = option_address(value),
                OPTION_ROUTER => lease.router = value.get(..4).and_then(option_address),
                OPTION_DNS_SERVERS => {
                    lease.dns_servers =
                        value.as_chunks::<4>().0.iter().map(|octets| Ipv4Addr::from(*octets)).collect();
                }
                OPTION_DOMAIN_NAME => {
                    lease.domain_name =
                        std::str::from_utf8(value).ok().filter(|name| is_domain_name(name)).map(str::to_owned);
                }
                _ => {}
            }
            offset += 2 + length;
        }
        Ok(Self {
            is_reply: bytes[0] == BOOT_REPLY,
            transaction: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            flags: u16::from_be_bytes([bytes[10], bytes[11]]),
            client_address: address_at(bytes, 12),
            your_address: address_at(bytes, 16),
            relay_address: address_at(bytes, 24),
            hardware_address: MacAddress::new(hardware),
            message_type: message_type.ok_or(MalformedMessage("the message type is missing"))?,
            requested_address,
            server_identifier,
            lease,
            options: Vec::new(),
        })
    }

    #[must_use]
    pub fn client_hardware_address(&self) -> MacAddress {
        self.hardware_address
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![0_u8; FIXED_HEADER_BYTES];
        bytes[0] = if self.is_reply { BOOT_REPLY } else { BOOT_REQUEST };
        bytes[1] = ETHERNET;
        bytes[2] = ETHERNET_ADDRESS_LENGTH;
        bytes[4..8].copy_from_slice(&self.transaction.to_be_bytes());
        bytes[10..12].copy_from_slice(&self.flags.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.client_address.octets());
        bytes[16..20].copy_from_slice(&self.your_address.octets());
        bytes[24..28].copy_from_slice(&self.relay_address.octets());
        bytes[28..34].copy_from_slice(&self.hardware_address.octets());
        bytes.extend_from_slice(&MAGIC_COOKIE);
        bytes.extend_from_slice(&[OPTION_MESSAGE_TYPE, 1, self.message_type.code()]);
        for (code, value) in &self.options {
            for chunk in value.chunks(usize::from(u8::MAX)) {
                bytes.push(*code);
                bytes.push(u8::try_from(chunk.len()).unwrap_or(u8::MAX));
                bytes.extend_from_slice(chunk);
            }
        }
        bytes.push(OPTION_END);
        if bytes.len() < MINIMUM_REPLY_BYTES {
            bytes.resize(MINIMUM_REPLY_BYTES, OPTION_PAD);
        }
        bytes
    }
}

fn reply(
    request: &DhcpMessage,
    message_type: MessageType,
    your_address: Ipv4Addr,
    options: Vec<(u8, Vec<u8>)>,
) -> DhcpMessage {
    DhcpMessage {
        is_reply: true,
        transaction: request.transaction,
        flags: request.flags,
        client_address: request.client_address,
        your_address,
        relay_address: request.relay_address,
        hardware_address: request.hardware_address,
        message_type,
        requested_address: None,
        server_identifier: None,
        lease: Lease::default(),
        options,
    }
}

#[must_use]
pub fn answer(request: &DhcpMessage, segment: &SegmentKey, state: &NodeNetworkState) -> Option<DhcpMessage> {
    if request.is_reply {
        return None;
    }
    let network = state.segment(segment)?;
    let port =
        state.ports.iter().find(|port| &port.segment == segment && port.mac == request.hardware_address)?;
    let gateway = network.gateway();
    if request.server_identifier.is_some_and(|server| server != gateway) {
        return None;
    }
    let mut options = vec![
        (OPTION_SERVER_IDENTIFIER, gateway.octets().to_vec()),
        (OPTION_SUBNET_MASK, network.subnet.netmask().octets().to_vec()),
        (OPTION_ROUTER, gateway.octets().to_vec()),
        (OPTION_DNS_SERVERS, gateway.octets().to_vec()),
        (OPTION_DOMAIN_NAME, network.domain.as_bytes().to_vec()),
        (OPTION_HOSTNAME, port.hostname.as_bytes().to_vec()),
        (OPTION_INTERFACE_MTU, MTU.to_be_bytes().to_vec()),
    ];
    let with_lease = |mut options: Vec<(u8, Vec<u8>)>| {
        options.push((OPTION_LEASE_TIME, LEASE_SECONDS.to_be_bytes().to_vec()));
        options
    };
    match request.message_type {
        MessageType::Discover => Some(reply(request, MessageType::Offer, port.address, with_lease(options))),
        MessageType::Request => {
            let wanted = request.requested_address.unwrap_or(request.client_address);
            if wanted == port.address {
                Some(reply(request, MessageType::Acknowledge, port.address, with_lease(options)))
            } else {
                options.truncate(1);
                Some(reply(request, MessageType::NegativeAcknowledge, Ipv4Addr::UNSPECIFIED, options))
            }
        }
        MessageType::Inform => Some(reply(request, MessageType::Acknowledge, Ipv4Addr::UNSPECIFIED, options)),
        MessageType::Offer
        | MessageType::Decline
        | MessageType::Acknowledge
        | MessageType::NegativeAcknowledge
        | MessageType::Release => None,
    }
}

#[cfg(test)]
pub mod tests {
    use amafu_domain::ObjectId;
    use amafu_interfaces::{Port, PortKey, Segment};

    use super::*;

    #[must_use]
    pub fn segment() -> SegmentKey {
        SegmentKey::of("project-a", "default")
    }

    #[must_use]
    pub fn state() -> NodeNetworkState {
        NodeNetworkState {
            segments: vec![Segment {
                key: segment(),
                subnet: "10.1.0.0/24".parse().unwrap(),
                is_outbound_access_allowed: true,
                domain: String::from("default.project-a.internal"),
            }],
            ports: vec![Port {
                key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 },
                segment: segment(),
                mac: "02:00:00:00:00:01".parse().unwrap(),
                address: Ipv4Addr::new(10, 1, 0, 2),
                hostname: String::from("web-1"),
                rules: Vec::new(),
                user_data: None,
            }],
        }
    }

    #[test]
    fn domain_names_that_could_inject_resolver_lines_are_ignored() {
        assert!(is_domain_name("default.project-a.internal"));
        for hostile in ["", "\0", "evil\nnameserver 6.6.6.6", ".", "a b"] {
            assert!(!is_domain_name(hostile), "{hostile:?} must be refused");
        }
    }

    fn request(message_type: MessageType, mac: &str, requested: Option<Ipv4Addr>) -> DhcpMessage {
        DhcpMessage {
            is_reply: false,
            transaction: 0x1234_5678,
            flags: 0x8000,
            client_address: Ipv4Addr::UNSPECIFIED,
            your_address: Ipv4Addr::UNSPECIFIED,
            relay_address: Ipv4Addr::UNSPECIFIED,
            hardware_address: mac.parse().unwrap(),
            message_type,
            requested_address: requested,
            server_identifier: None,
            lease: Lease::default(),
            options: Vec::new(),
        }
    }

    fn with_requested(mut bytes: Vec<u8>, address: Ipv4Addr) -> Vec<u8> {
        let end = bytes.iter().rposition(|byte| *byte == OPTION_END).unwrap();
        bytes.truncate(end);
        bytes.extend_from_slice(&[OPTION_REQUESTED_ADDRESS, 4]);
        bytes.extend_from_slice(&address.octets());
        bytes.push(OPTION_END);
        bytes
    }

    #[test]
    fn a_discover_from_a_known_port_is_offered_its_address() {
        let discover = request(MessageType::Discover, "02:00:00:00:00:01", None);
        let parsed = DhcpMessage::parse(&discover.encode()).unwrap();
        assert_eq!(parsed.message_type, MessageType::Discover);
        let offer = answer(&parsed, &segment(), &state()).unwrap();
        assert_eq!(offer.message_type, MessageType::Offer);
        assert_eq!(offer.your_address, Ipv4Addr::new(10, 1, 0, 2));
        let encoded = offer.encode();
        assert!(encoded.len() >= MINIMUM_REPLY_BYTES && encoded.len() <= 1500);
        let received = DhcpMessage::parse(&encoded).unwrap();
        assert_eq!(received.your_address, Ipv4Addr::new(10, 1, 0, 2));
        assert_eq!(received.lease.prefix_length(), Some(24));
        assert_eq!(received.lease.router, Some(Ipv4Addr::new(10, 1, 0, 1)));
        assert_eq!(received.lease.dns_servers, vec![Ipv4Addr::new(10, 1, 0, 1)]);
        assert_eq!(received.lease.domain_name.as_deref(), Some("default.project-a.internal"));
    }

    #[test]
    fn a_request_for_the_assigned_address_is_acknowledged_and_any_other_refused() {
        let bytes = request(MessageType::Request, "02:00:00:00:00:01", None).encode();
        let good = DhcpMessage::parse(&with_requested(bytes.clone(), Ipv4Addr::new(10, 1, 0, 2))).unwrap();
        assert_eq!(good.requested_address, Some(Ipv4Addr::new(10, 1, 0, 2)));
        assert_eq!(answer(&good, &segment(), &state()).unwrap().message_type, MessageType::Acknowledge);
        let greedy = DhcpMessage::parse(&with_requested(bytes, Ipv4Addr::new(10, 1, 0, 9))).unwrap();
        assert_eq!(
            answer(&greedy, &segment(), &state()).unwrap().message_type,
            MessageType::NegativeAcknowledge
        );
    }

    #[test]
    fn unknown_clients_and_other_segments_get_nothing() {
        let stranger = request(MessageType::Discover, "02:00:00:00:00:99", None);
        assert_eq!(answer(&stranger, &segment(), &state()), None);
        let known = request(MessageType::Discover, "02:00:00:00:00:01", None);
        assert_eq!(answer(&known, &SegmentKey::of("project-b", "default"), &state()), None);
    }

    #[test]
    fn truncated_and_foreign_messages_are_refused() {
        let bytes = request(MessageType::Discover, "02:00:00:00:00:01", None).encode();
        assert!(DhcpMessage::parse(&bytes[..100]).is_err());
        let mut wrong_cookie = bytes.clone();
        wrong_cookie[FIXED_HEADER_BYTES] = 0;
        assert!(DhcpMessage::parse(&wrong_cookie).is_err());
        let mut runaway = bytes[..FIXED_HEADER_BYTES + 4].to_vec();
        runaway.extend_from_slice(&[OPTION_MESSAGE_TYPE, 200, 1]);
        assert!(DhcpMessage::parse(&runaway).is_err());
    }
}
