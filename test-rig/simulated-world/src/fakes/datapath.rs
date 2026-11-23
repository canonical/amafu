use std::collections::BTreeMap;
use std::sync::Mutex;

use amafu_domain::MacAddress;
use amafu_interfaces::{
    ActionError, Datapath, GuestSighting, LocalServices, NodeNetworkState, RealizedNetwork, SegmentKey,
};

use crate::lock::lock;

#[derive(Default)]
pub struct FakeDatapath {
    state: Mutex<NodeNetworkState>,
    learned: Mutex<BTreeMap<(SegmentKey, MacAddress), String>>,
}

impl FakeDatapath {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn state(&self) -> NodeNetworkState {
        lock(&self.state).clone()
    }

    pub fn power_off(&self) {
        *lock(&self.state) = NodeNetworkState::default();
    }

    pub fn learn(&self, segment: &SegmentKey, mac: MacAddress, tap: &str) {
        lock(&self.learned).insert((segment.clone(), mac), tap.to_owned());
    }
}

fn realized(state: &NodeNetworkState) -> RealizedNetwork {
    RealizedNetwork {
        segments: state.segments.iter().map(|segment| segment.key.clone()).collect(),
        ports: state.ports.iter().map(|port| port.key).collect(),
    }
}

#[async_trait::async_trait]
impl Datapath for FakeDatapath {
    async fn converge(&self, desired: &NodeNetworkState) -> Result<RealizedNetwork, ActionError> {
        *lock(&self.state) = desired.clone();
        Ok(realized(desired))
    }

    async fn current(&self) -> Result<RealizedNetwork, ActionError> {
        Ok(realized(&lock(&self.state)))
    }

    async fn learned_port(
        &self,
        segment: &SegmentKey,
        mac: MacAddress,
    ) -> Result<Option<String>, ActionError> {
        Ok(lock(&self.learned).get(&(segment.clone(), mac)).cloned())
    }
}

#[derive(Default)]
pub struct FakeLocalServices {
    state: Mutex<NodeNetworkState>,
    sightings: Mutex<Vec<GuestSighting>>,
}

impl FakeLocalServices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn state(&self) -> NodeNetworkState {
        lock(&self.state).clone()
    }

    pub fn power_off(&self) {
        *lock(&self.state) = NodeNetworkState::default();
    }

    pub fn sight(&self, sighting: GuestSighting) {
        lock(&self.sightings).push(sighting);
    }
}

impl LocalServices for FakeLocalServices {
    fn update(&self, state: &NodeNetworkState) {
        *lock(&self.state) = state.clone();
    }

    fn take_guest_sightings(&self) -> Vec<GuestSighting> {
        std::mem::take(&mut *lock(&self.sightings))
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::{MacAddress, ObjectId};
    use amafu_interfaces::contract::datapath::datapath_contract;
    use amafu_interfaces::{Port, PortKey, Segment, SegmentKey};

    use super::*;

    #[test]
    fn the_fake_datapath_honours_the_datapath_contract() {
        let segment = SegmentKey::of("project-a", "default");
        let desired = NodeNetworkState {
            segments: vec![Segment {
                key: segment.clone(),
                subnet: "10.1.0.0/24".parse().unwrap(),
                is_outbound_access_allowed: true,
                is_platform_api_allowed: false,
                domain: String::from("default.project-a.internal"),
                segment_id: None,
            }],
            ports: vec![Port {
                guest_pool: Vec::new(),
                guests: Vec::new(),
                key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 },
                segment,
                mac: MacAddress::locally_administered(b"web"),
                address: "10.1.0.2".parse().unwrap(),
                hostname: String::from("web"),
                rules: Vec::new(),
                user_data: None,
                public_keys: Vec::new(),
            }],
            ..NodeNetworkState::default()
        };
        assert_eq!(futures::executor::block_on(datapath_contract(&FakeDatapath::new(), &desired)), Ok(()));
    }
}
