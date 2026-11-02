use std::sync::Mutex;

use amafu_interfaces::{ActionError, Datapath, LocalServices, NodeNetworkState, RealizedNetwork};

use crate::lock::lock;

#[derive(Default)]
pub struct FakeDatapath {
    state: Mutex<NodeNetworkState>,
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
}

#[derive(Default)]
pub struct FakeLocalServices {
    state: Mutex<NodeNetworkState>,
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
}

impl LocalServices for FakeLocalServices {
    fn update(&self, state: &NodeNetworkState) {
        *lock(&self.state) = state.clone();
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
                domain: String::from("default.project-a.internal"),
            }],
            ports: vec![Port {
                key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::nil()), index: 0 },
                segment,
                mac: MacAddress::locally_administered(b"web"),
                address: "10.1.0.2".parse().unwrap(),
                hostname: String::from("web"),
                rules: Vec::new(),
                user_data: None,
            }],
        };
        assert_eq!(futures::executor::block_on(datapath_contract(&FakeDatapath::new(), &desired)), Ok(()));
    }
}
