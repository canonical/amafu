use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value:?} is not valid: {reason}")]
pub struct InvalidAddress {
    pub value: String,
    pub reason: &'static str,
}

fn invalid(value: &str, reason: &'static str) -> InvalidAddress {
    InvalidAddress { value: value.to_owned(), reason }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ipv4Network {
    address: Ipv4Addr,
    prefix_length: u8,
}

impl Ipv4Network {
    pub const MINIMUM_PREFIX_LENGTH: u8 = 8;
    pub const MAXIMUM_PREFIX_LENGTH: u8 = 29;

    pub fn new(address: Ipv4Addr, prefix_length: u8) -> Result<Self, InvalidAddress> {
        let text = format!("{address}/{prefix_length}");
        if !(Self::MINIMUM_PREFIX_LENGTH..=Self::MAXIMUM_PREFIX_LENGTH).contains(&prefix_length) {
            return Err(invalid(&text, "the prefix length must be between 8 and 29"));
        }
        let network = Self { address, prefix_length };
        if network.network_address() != address {
            return Err(invalid(&text, "host bits must be zero"));
        }
        Ok(network)
    }

    #[must_use]
    pub fn prefix_length(self) -> u8 {
        self.prefix_length
    }

    fn mask(self) -> u32 {
        u32::MAX << (32 - u32::from(self.prefix_length))
    }

    #[must_use]
    pub fn network_address(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.address) & self.mask())
    }

    #[must_use]
    pub fn broadcast_address(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.address) | !self.mask())
    }

    #[must_use]
    pub fn netmask(self) -> Ipv4Addr {
        Ipv4Addr::from(self.mask())
    }

    #[must_use]
    pub fn gateway(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network_address()) + 1)
    }

    #[must_use]
    pub fn contains(self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask() == u32::from(self.network_address())
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.contains(other.network_address()) || other.contains(self.network_address())
    }

    #[must_use]
    pub fn is_assignable(self, address: Ipv4Addr) -> bool {
        self.contains(address)
            && address != self.network_address()
            && address != self.broadcast_address()
            && address != self.gateway()
    }

    pub fn assignable_addresses(self) -> impl Iterator<Item = Ipv4Addr> {
        let first = u32::from(self.gateway()) + 1;
        let last = u32::from(self.broadcast_address());
        (first..last).map(Ipv4Addr::from)
    }
}

impl fmt::Display for Ipv4Network {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.address, self.prefix_length)
    }
}

impl FromStr for Ipv4Network {
    type Err = InvalidAddress;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (address, prefix_length) =
            value.split_once('/').ok_or_else(|| invalid(value, "it must look like 10.0.0.0/24"))?;
        let address = address.parse().map_err(|_| invalid(value, "the address is not IPv4"))?;
        let prefix_length =
            prefix_length.parse().map_err(|_| invalid(value, "the prefix length is not a number"))?;
        Self::new(address, prefix_length)
    }
}

impl Serialize for Ipv4Network {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Ipv4Network {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    #[must_use]
    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    #[must_use]
    pub fn locally_administered(seed: &[u8]) -> Self {
        let mut octets = [0x02, 0, 0, 0, 0, 0];
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in seed {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
        octets[1..].copy_from_slice(&hash.to_be_bytes()[..5]);
        Self(octets)
    }

    #[must_use]
    pub const fn octets(&self) -> [u8; 6] {
        self.0
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [first, second, third, fourth, fifth, sixth] = self.0;
        write!(formatter, "{first:02x}:{second:02x}:{third:02x}:{fourth:02x}:{fifth:02x}:{sixth:02x}")
    }
}

impl FromStr for MacAddress {
    type Err = InvalidAddress;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut octets = [0_u8; 6];
        let mut parts = value.split(':');
        for octet in &mut octets {
            let part = parts.next().ok_or_else(|| invalid(value, "it must have six octets"))?;
            if part.len() != 2 {
                return Err(invalid(value, "each octet must be two hexadecimal digits"));
            }
            *octet =
                u8::from_str_radix(part, 16).map_err(|_| invalid(value, "an octet is not hexadecimal"))?;
        }
        if parts.next().is_some() {
            return Err(invalid(value, "it must have six octets"));
        }
        Ok(Self(octets))
    }
}

impl Serialize for MacAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MacAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_network_knows_its_gateway_and_assignable_range() {
        let network: Ipv4Network = "10.20.0.0/24".parse().unwrap();
        assert_eq!(network.gateway(), Ipv4Addr::new(10, 20, 0, 1));
        assert_eq!(network.broadcast_address(), Ipv4Addr::new(10, 20, 0, 255));
        assert_eq!(network.netmask(), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(network.assignable_addresses().next(), Some(Ipv4Addr::new(10, 20, 0, 2)));
        assert_eq!(network.assignable_addresses().count(), 253);
        assert!(!network.is_assignable(Ipv4Addr::new(10, 20, 0, 1)));
        assert!(network.is_assignable(Ipv4Addr::new(10, 20, 0, 9)));
        assert!(!network.is_assignable(Ipv4Addr::new(10, 21, 0, 9)));
    }

    #[test]
    fn malformed_networks_are_refused() {
        for value in ["10.0.0.0", "10.0.0.1/24", "10.0.0.0/30", "10.0.0.0/4", "a.b.c.d/24"] {
            assert!(value.parse::<Ipv4Network>().is_err(), "{value}");
        }
    }

    #[test]
    fn overlapping_networks_are_detected() {
        let wide: Ipv4Network = "10.0.0.0/16".parse().unwrap();
        let narrow: Ipv4Network = "10.0.5.0/24".parse().unwrap();
        let apart: Ipv4Network = "10.1.0.0/24".parse().unwrap();
        assert!(wide.overlaps(narrow) && narrow.overlaps(wide));
        assert!(!wide.overlaps(apart));
    }

    #[test]
    fn mac_addresses_round_trip() {
        let mac: MacAddress = "02:ab:00:10:ff:01".parse().unwrap();
        assert_eq!(mac.to_string(), "02:ab:00:10:ff:01");
        for value in ["02:ab:00:10:ff", "02:ab:00:10:ff:01:02", "02:ab:00:10:ff:1", "zz:ab:00:10:ff:01"] {
            assert!(value.parse::<MacAddress>().is_err(), "{value}");
        }
    }

    proptest! {
        #[test]
        fn derived_mac_addresses_are_locally_administered_unicast(seed in proptest::collection::vec(any::<u8>(), 0..32)) {
            let mac = MacAddress::locally_administered(&seed);
            prop_assert_eq!(mac.octets()[0], 0x02);
            prop_assert_eq!(mac.to_string().parse::<MacAddress>().ok(), Some(mac));
        }

        #[test]
        fn arbitrary_network_text_never_panics(value in ".*") {
            let _ = value.parse::<Ipv4Network>();
        }
    }
}
