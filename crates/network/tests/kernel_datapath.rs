use amafu_domain::{MacAddress, ObjectId};
use amafu_interfaces::contract::datapath::datapath_contract;
use amafu_interfaces::{Datapath, ExternalTool, NodeNetworkState, Port, PortKey, Segment, SegmentKey};
use amafu_network::KernelDatapath;

fn desired() -> Result<NodeNetworkState, Box<dyn std::error::Error>> {
    let segment = SegmentKey::of("contract", "default");
    Ok(NodeNetworkState {
        segments: vec![Segment {
            key: segment.clone(),
            subnet: "10.250.0.0/24".parse()?,
            is_outbound_access_allowed: true,
            domain: String::from("default.contract.internal"),
            segment_id: None,
        }],
        ports: vec![Port {
            key: PortKey { vm_id: ObjectId::from_uuid(uuid::Uuid::from_u128(7)), index: 0 },
            segment,
            mac: MacAddress::locally_administered(b"contract"),
            address: "10.250.0.2".parse()?,
            hostname: String::from("contract"),
            rules: Vec::new(),
            user_data: None,
        }],
        ..NodeNetworkState::default()
    })
}

#[tokio::test]
#[ignore = "needs root, /dev/net/tun and the nft binary"]
async fn the_kernel_datapath_honours_the_datapath_contract() {
    let underlay = std::env::var("AMAFU_TEST_UNDERLAY").unwrap_or_else(|_| String::from("eth0"));
    let datapath = KernelDatapath::new(underlay, ExternalTool::installed("/usr/sbin/nft"));
    assert_eq!(datapath_contract(&datapath, &desired().unwrap()).await, Ok(()));
}

fn run(program: &str, arguments: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(
            format!("{program} {arguments:?} failed: {}", String::from_utf8_lossy(&output.stderr)).into()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn overlay_state() -> Result<NodeNetworkState, Box<dyn std::error::Error>> {
    let mut state = desired()?;
    let segment = state.segments[0].key.clone();
    state.segments[0].segment_id = Some(amafu_domain::SegmentId::new(4242)?);
    state.underlay_address = Some("192.0.2.10".parse()?);
    state.remote_endpoints.push(amafu_interfaces::RemoteEndpoint {
        segment: segment.clone(),
        mac: "02:00:00:00:42:09".parse()?,
        address: "10.250.0.9".parse()?,
        hostname: String::from("remote"),
        node_address: "192.0.2.11".parse()?,
    });
    state.floating_addresses.push(amafu_interfaces::FloatingAddress {
        address: "198.51.100.7".parse()?,
        private_address: "10.250.0.2".parse()?,
        segment,
    });
    Ok(state)
}

#[tokio::test]
#[ignore = "needs root, /dev/net/tun and the nft binary"]
async fn tunnels_and_floating_addresses_are_programmed_and_removed() {
    let underlay = "testunder0";
    let _ = run("ip", &["link", "del", underlay]);
    run("ip", &["link", "add", underlay, "type", "dummy"]).unwrap();
    run("ip", &["address", "add", "192.0.2.10/24", "dev", underlay]).unwrap();
    run("ip", &["link", "set", underlay, "up"]).unwrap();
    let datapath = KernelDatapath::new(underlay.to_owned(), ExternalTool::installed("/usr/sbin/nft"));
    let state = overlay_state().unwrap();
    let outcome = async {
        datapath.converge(&state).await?;
        let tunnel = amafu_interfaces::network::tunnel_name(
            &state.segments[0].key,
            state.segments[0].segment_id.unwrap(),
        );
        let details = run("ip", &["-details", "link", "show", &tunnel])?;
        assert!(details.contains("vxlan id 4242"), "{details}");
        assert!(details.contains("proxy"), "{details}");
        let forwarding = run("bridge", &["fdb", "show", "dev", &tunnel])?;
        assert!(forwarding.contains("02:00:00:00:42:09 dst 192.0.2.11"), "{forwarding}");
        let neighbours = run("ip", &["neigh", "show", "dev", &tunnel])?;
        assert!(neighbours.contains("10.250.0.9 lladdr 02:00:00:00:42:09"), "{neighbours}");
        let addresses = run("ip", &["-4", "address", "show", "dev", underlay])?;
        assert!(addresses.contains("198.51.100.7/32"), "{addresses}");
        let ruleset = run("/usr/sbin/nft", &["list", "table", "inet", "amafu"])?;
        assert!(ruleset.contains("dnat ip to 10.250.0.2"), "{ruleset}");
        let inspection = datapath.inspect().await?;
        assert_eq!(inspection.floating_addresses, vec!["198.51.100.7".parse::<std::net::Ipv4Addr>()?]);
        assert_eq!(inspection.remotes.len(), 1);
        assert_eq!(inspection.remotes[0].node_address, "192.0.2.11".parse::<std::net::Ipv4Addr>()?);
        assert_eq!(inspection.remotes[0].segment, state.segments[0].key);
        let mut moved = state.clone();
        moved.remote_endpoints[0].node_address = "192.0.2.12".parse()?;
        moved.floating_addresses.clear();
        datapath.converge(&moved).await?;
        let forwarding = run("bridge", &["fdb", "show", "dev", &tunnel])?;
        assert!(
            forwarding.contains("dst 192.0.2.12") && !forwarding.contains("dst 192.0.2.11"),
            "{forwarding}"
        );
        let addresses = run("ip", &["-4", "address", "show", "dev", underlay])?;
        assert!(!addresses.contains("198.51.100.7"), "{addresses}");
        datapath.converge(&NodeNetworkState::default()).await?;
        assert!(run("ip", &["link", "show", &tunnel]).is_err(), "the tunnel was removed");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = run("ip", &["link", "del", underlay]);
    outcome.unwrap();
}
