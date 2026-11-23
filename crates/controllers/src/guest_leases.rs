use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use amafu_domain::kinds::network_endpoint::{GuestLease, NetworkEndpoint, NetworkEndpointSpecification};
use amafu_domain::{MacAddress, NodeName, ObjectId, ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::{LeasedGuest, PortKey};
use time::{Duration, OffsetDateTime};

pub const GUEST_LEASE_LIFETIME: Duration = Duration::hours(2);
pub const GUEST_RENEWAL_INTERVAL: Duration = Duration::minutes(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostingInterface {
    pub project: Option<ProjectName>,
    pub vm: ObjectName,
    pub vm_id: ObjectId,
    pub index: u8,
    pub network: ObjectName,
    pub pool: Vec<Ipv4Addr>,
    pub security_groups: Vec<ObjectName>,
    pub node: NodeName,
    pub node_address: Ipv4Addr,
}

impl HostingInterface {
    #[must_use]
    pub fn port(&self) -> PortKey {
        PortKey { vm_id: self.vm_id, index: self.index }
    }

    fn holds(&self, lease: &NetworkEndpointSpecification) -> bool {
        lease.vm_id == self.vm_id && lease.index == self.index && self.pool.contains(&lease.address)
    }

    fn lease(
        &self,
        mac: MacAddress,
        address: Ipv4Addr,
        renewed_at: OffsetDateTime,
    ) -> NetworkEndpointSpecification {
        NetworkEndpointSpecification {
            network: self.network.clone(),
            vm: self.vm.clone(),
            vm_id: self.vm_id,
            index: self.index,
            mac,
            address,
            node: self.node.clone(),
            node_address: self.node_address,
            security_groups: self.security_groups.clone(),
            guest: Some(GuestLease { renewed_at }),
        }
    }

    fn reference(&self, mac: MacAddress) -> ObjectReference {
        ObjectReference::of::<NetworkEndpoint>(
            self.project.clone(),
            NetworkEndpoint::guest_name_for(self.vm_id, self.index, mac),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SeenGuest {
    pub port: PortKey,
    pub mac: MacAddress,
}

#[must_use]
pub fn is_current(lease: &NetworkEndpointSpecification, now: OffsetDateTime) -> bool {
    lease.guest.is_some_and(|guest| now - guest.renewed_at < GUEST_LEASE_LIFETIME)
}

#[must_use]
pub fn leased_guests(
    host: &HostingInterface,
    published: &[NetworkEndpointSpecification],
    now: OffsetDateTime,
) -> Vec<LeasedGuest> {
    let mut guests: Vec<LeasedGuest> = published
        .iter()
        .filter(|lease| host.holds(lease) && is_current(lease, now))
        .map(|lease| LeasedGuest { mac: lease.mac, address: lease.address })
        .collect();
    guests.sort();
    guests
}

#[must_use]
pub fn guest_leases(
    hosts: &[HostingInterface],
    published: &[NetworkEndpointSpecification],
    seen: &BTreeSet<SeenGuest>,
    now: OffsetDateTime,
) -> BTreeMap<ObjectReference, NetworkEndpointSpecification> {
    let mut wanted = BTreeMap::new();
    for host in hosts {
        let sightings: BTreeSet<MacAddress> =
            seen.iter().filter(|seen| seen.port == host.port()).map(|seen| seen.mac).collect();
        let current: Vec<&NetworkEndpointSpecification> =
            published.iter().filter(|lease| host.holds(lease) && is_current(lease, now)).collect();
        for lease in &current {
            let renewed_at = lease.guest.map_or(now, |guest| guest.renewed_at);
            let is_due = sightings.contains(&lease.mac) && now - renewed_at >= GUEST_RENEWAL_INTERVAL;
            let renewed_at = if is_due { now } else { renewed_at };
            wanted.insert(host.reference(lease.mac), host.lease(lease.mac, lease.address, renewed_at));
        }
        let leased: BTreeSet<MacAddress> = current.iter().map(|lease| lease.mac).collect();
        let taken: BTreeSet<Ipv4Addr> = current.iter().map(|lease| lease.address).collect();
        let mut free = host.pool.iter().copied().filter(|address| !taken.contains(address));
        for mac in sightings.iter().filter(|mac| !leased.contains(mac)) {
            let Some(address) = free.next() else {
                break;
            };
            wanted.insert(host.reference(*mac), host.lease(*mac, address, now));
        }
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn host() -> HostingInterface {
        HostingInterface {
            project: None,
            vm: name("pod-0"),
            vm_id: ObjectId::from_uuid(uuid::Uuid::nil()),
            index: 0,
            network: name("private"),
            pool: vec![Ipv4Addr::new(10, 1, 0, 3), Ipv4Addr::new(10, 1, 0, 4)],
            security_groups: vec![name("pods")],
            node: NodeName::new("node-1").unwrap(),
            node_address: Ipv4Addr::new(192, 0, 2, 1),
        }
    }

    fn guest(last: u8) -> MacAddress {
        MacAddress::new([0x00, 0x16, 0x3e, 0, 0, last])
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn seen(macs: &[MacAddress]) -> BTreeSet<SeenGuest> {
        macs.iter().map(|mac| SeenGuest { port: host().port(), mac: *mac }).collect()
    }

    #[test]
    fn new_guests_take_the_lowest_free_addresses_until_the_pool_is_full() {
        let wanted = guest_leases(&[host()], &[], &seen(&[guest(1), guest(2), guest(3)]), now());
        let addresses: BTreeSet<Ipv4Addr> = wanted.values().map(|lease| lease.address).collect();
        assert_eq!(addresses, BTreeSet::from([Ipv4Addr::new(10, 1, 0, 3), Ipv4Addr::new(10, 1, 0, 4)]));
        assert!(wanted.values().all(|lease| lease.guest == Some(GuestLease { renewed_at: now() })));
    }

    #[test]
    fn a_lease_is_kept_renewed_when_due_and_dropped_once_expired() {
        let fresh = host().lease(guest(1), Ipv4Addr::new(10, 1, 0, 3), now() - Duration::minutes(5));
        let due = host().lease(guest(2), Ipv4Addr::new(10, 1, 0, 4), now() - Duration::minutes(20));
        let kept =
            guest_leases(&[host()], &[fresh.clone(), due.clone()], &seen(&[guest(1), guest(2)]), now());
        assert_eq!(kept[&host().reference(guest(1))], fresh);
        assert_eq!(kept[&host().reference(guest(2))].guest, Some(GuestLease { renewed_at: now() }));
        let expired = host().lease(guest(1), Ipv4Addr::new(10, 1, 0, 3), now() - GUEST_LEASE_LIFETIME);
        assert!(guest_leases(&[host()], std::slice::from_ref(&expired), &BTreeSet::new(), now()).is_empty());
        assert_eq!(leased_guests(&host(), &[expired, fresh], now()).len(), 1);
    }

    #[test]
    fn guests_seen_on_another_port_are_not_leased_here() {
        let elsewhere = SeenGuest { port: PortKey { index: 1, ..host().port() }, mac: guest(1) };
        assert!(guest_leases(&[host()], &[], &BTreeSet::from([elsewhere]), now()).is_empty());
    }
}
