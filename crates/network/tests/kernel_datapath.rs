use amafu_domain::{MacAddress, ObjectId};
use amafu_interfaces::contract::datapath::datapath_contract;
use amafu_interfaces::{NodeNetworkState, Port, PortKey, Segment, SegmentKey};
use amafu_network::KernelDatapath;

fn desired() -> NodeNetworkState {
    let segment = SegmentKey::of("contract", "default");
    NodeNetworkState {
        segments: vec![Segment {
            key: segment.clone(),
            subnet: "10.250.0.0/24".parse().unwrap(),
            is_outbound_access_allowed: true,
            domain: String::from("default.contract.internal"),
        }],
        ports: vec![Port {
            key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::from_u128(7)), index: 0 },
            segment,
            mac: MacAddress::locally_administered(b"contract"),
            address: "10.250.0.2".parse().unwrap(),
            hostname: String::from("contract"),
            rules: Vec::new(),
            user_data: None,
        }],
    }
}

#[tokio::test]
#[ignore = "needs root, /dev/net/tun and the nft binary"]
async fn the_kernel_datapath_honours_the_datapath_contract() {
    let underlay = std::env::var("AMAFU_TEST_UNDERLAY").unwrap_or_else(|_| String::from("eth0"));
    let datapath = KernelDatapath::new(underlay, "/usr/sbin/nft".into());
    assert_eq!(datapath_contract(&datapath, &desired()).await, Ok(()));
}
