use std::net::Ipv4Addr;

use amafu_domain::MacAddress;

const BROADCAST: [u8; 6] = [0xff; 6];
const ETHER_TYPE_ARP: [u8; 2] = [0x08, 0x06];
const ARP_HARDWARE_ETHERNET: [u8; 2] = [0x00, 0x01];
const ARP_PROTOCOL_IP: [u8; 2] = [0x08, 0x00];
const ARP_OPERATION_REQUEST: [u8; 2] = [0x00, 0x01];
const ANNOUNCEMENT_REPEATS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum AnnouncementError {
    #[error("no interface named {0} exists")]
    MissingInterface(String),
    #[error("{0} has no hardware address")]
    NoHardwareAddress(String),
    #[error("the interface cannot send raw frames: {0}")]
    Channel(std::io::Error),
    #[error("sending the announcement failed: {0}")]
    Send(std::io::Error),
}

#[must_use]
pub fn gratuitous_arp(mac: MacAddress, address: Ipv4Addr) -> Vec<u8> {
    let sender = mac.octets();
    let mut frame = Vec::with_capacity(42);
    frame.extend_from_slice(&BROADCAST);
    frame.extend_from_slice(&sender);
    frame.extend_from_slice(&ETHER_TYPE_ARP);
    frame.extend_from_slice(&ARP_HARDWARE_ETHERNET);
    frame.extend_from_slice(&ARP_PROTOCOL_IP);
    frame.push(6);
    frame.push(4);
    frame.extend_from_slice(&ARP_OPERATION_REQUEST);
    frame.extend_from_slice(&sender);
    frame.extend_from_slice(&address.octets());
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&address.octets());
    frame
}

pub fn announce(interface_name: &str, addresses: &[Ipv4Addr]) -> Result<(), AnnouncementError> {
    if addresses.is_empty() {
        return Ok(());
    }
    let interface = pnet_datalink::interfaces()
        .into_iter()
        .find(|interface| interface.name == interface_name)
        .ok_or_else(|| AnnouncementError::MissingInterface(interface_name.to_owned()))?;
    let mac = interface
        .mac
        .map(|mac| MacAddress::new(mac.octets()))
        .ok_or_else(|| AnnouncementError::NoHardwareAddress(interface_name.to_owned()))?;
    let pnet_datalink::Channel::Ethernet(mut sender, _) =
        pnet_datalink::channel(&interface, pnet_datalink::Config::default())
            .map_err(AnnouncementError::Channel)?
    else {
        return Err(AnnouncementError::Channel(std::io::Error::other("not an Ethernet interface")));
    };
    for _ in 0..ANNOUNCEMENT_REPEATS {
        for address in addresses {
            if let Some(result) = sender.send_to(&gratuitous_arp(mac, *address), None) {
                result.map_err(AnnouncementError::Send)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gratuitous_arp_names_the_address_as_both_sender_and_target() {
        let mac = MacAddress::new([2, 0, 0, 0, 0, 9]);
        let frame = gratuitous_arp(mac, Ipv4Addr::new(198, 51, 100, 7));
        assert_eq!(frame.len(), 42);
        assert_eq!(&frame[0..6], &BROADCAST);
        assert_eq!(&frame[12..14], &ETHER_TYPE_ARP);
        assert_eq!(&frame[28..32], &[198, 51, 100, 7]);
        assert_eq!(&frame[38..42], &[198, 51, 100, 7]);
        assert_eq!(&frame[22..28], &mac.octets());
    }
}
