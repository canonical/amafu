use std::net::Ipv4Addr;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_checkers::invariants::a_floating_address_is_served_by_at_most_one_node;
use amafu_domain::access::Role;
use amafu_domain::kinds::external_network::ExternalNetwork;
use amafu_domain::kinds::floating_ip::{
    FloatingIp, FloatingIpPhase, FloatingIpSpecification, FloatingIpTarget,
};
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::network_endpoint::NetworkEndpoint;
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::kinds::vm::{BootSource, Vm, VmPhase, VmSpecification};
use amafu_domain::{NodeName, Object};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure, eventually,
};
use futures::FutureExt;

use crate::networking::{interface, ready_network};
use crate::support::{
    START_TIMEOUT, busybox, console_shows, delete_and_wait, every_node_ready, instance, name, ready_image,
    vm_in_phase,
};

pub const OVERLAY_CONNECTS_NODES: ScenarioDefinition = ScenarioDefinition {
    name: "VMs of one network on different nodes reach each other over the overlay",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| vms_on_different_nodes_reach_each_other(cluster).boxed_local(),
};

pub const FLOATING_IP_MOVES: ScenarioDefinition = ScenarioDefinition {
    name: "a floating IP reaches its VM from outside and follows it to another node",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_floating_ip_follows_its_target(cluster).boxed_local(),
};

const SERVER_PORT: u16 = 8080;
const SETTLING: Duration = Duration::from_secs(5);

fn open_web() -> SecurityGroupSpecification {
    SecurityGroupSpecification {
        rules: vec![SecurityGroupRule {
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            ports: Some(PortRange { first: SERVER_PORT, last: SERVER_PORT }),
            remote: Remote::Any,
        }],
    }
}

fn server_script(label: &str) -> String {
    format!("mkdir -p /www && echo served by {label} > /www/index.html && httpd -f -p {SERVER_PORT} -h /www")
}

fn probe_script(server: &str) -> String {
    format!(
        "sleep 2; for attempt in $(seq 60); do if wget -q -T 3 -O - http://{server}:{SERVER_PORT}/ | grep -q served; then echo OVERLAY-REACHED; break; fi; sleep 2; done; echo OVERLAY-DONE; sleep 3600"
    )
}

async fn container(
    member: &ApiClient,
    container_name: &str,
    image: &str,
    network: &str,
    groups: &[&str],
    script: String,
) -> Result<Object<Vm>, ScenarioError> {
    let mut specification = VmSpecification::container(name(image)?);
    specification.network_interfaces = vec![interface(network, groups)?];
    specification.anti_affinity_group = Some(name("overlay-spread")?);
    if let BootSource::Container { command, .. } = &mut specification.boot {
        *command = vec![String::from("sh"), String::from("-c"), script];
    }
    Ok(member.create::<Vm>(&name(container_name)?, &specification).await?)
}

async fn underlay_address(cluster: &dyn Cluster, node: &NodeName) -> Result<Ipv4Addr, ScenarioError> {
    let operator = client(cluster, Role::Operator).await;
    operator
        .get::<Node>(node.as_str())
        .await?
        .status
        .underlay_address
        .ok_or_else(|| ScenarioError::Expectation(format!("{node} reports no underlay address")))
}

fn address_of(vm: &Object<Vm>) -> Result<Ipv4Addr, ScenarioError> {
    vm.status
        .interfaces
        .first()
        .map(|interface| interface.address)
        .ok_or_else(|| ScenarioError::Expectation(format!("{} has no address", vm.metadata.name)))
}

async fn ensure_tunnel(
    cluster: &dyn Cluster,
    from: &NodeName,
    to: &Object<Vm>,
    to_node: &NodeName,
) -> ScenarioResult {
    let address = address_of(to)?;
    let node_address = underlay_address(cluster, to_node).await?;
    eventually(cluster, START_TIMEOUT, &format!("{from} reaches {address} through {to_node}"), async || {
        Ok(cluster.observe().await?.remote_endpoints.iter().any(|remote| {
            remote.node == *from && remote.address == address && remote.node_address == node_address
        }))
    })
    .await
}

async fn vms_on_different_nodes_reach_each_other(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let member = client(cluster, Role::Member).await;
    ready_image(&member, cluster, "busybox-overlay", busybox()?).await?;
    ready_network(&member, cluster, "overlay", "10.90.0.0/24").await?;
    member.create::<SecurityGroup>(&name("overlay-web")?, &open_web()).await?;
    container(
        &member,
        "overlay-server",
        "busybox-overlay",
        "overlay",
        &["overlay-web"],
        server_script("overlay-server"),
    )
    .await?;
    let probe = container(
        &member,
        "overlay-client",
        "busybox-overlay",
        "overlay",
        &[],
        probe_script("overlay-server"),
    )
    .await?;
    let server = vm_in_phase(&member, cluster, "overlay-server", VmPhase::Running, START_TIMEOUT).await?;
    let probe_vm = vm_in_phase(&member, cluster, "overlay-client", VmPhase::Running, START_TIMEOUT).await?;
    let server_node = instance(cluster, server.id()).await?.node;
    let client_node = instance(cluster, probe.id()).await?.node;
    ensure(server_node != client_node, || format!("both containers run on {server_node}"))?;
    ensure_tunnel(cluster, &client_node, &server, &server_node).await?;
    ensure_tunnel(cluster, &server_node, &probe_vm, &client_node).await?;
    console_shows(cluster, probe.id(), "OVERLAY-REACHED").await?;
    for vm_name in ["overlay-client", "overlay-server"] {
        delete_and_wait::<Vm>(&member, cluster, vm_name).await?;
    }
    eventually(cluster, START_TIMEOUT, "the network endpoints of deleted VMs are withdrawn", async || {
        let endpoints = member.list::<NetworkEndpoint>().await?;
        Ok(endpoints.iter().all(|endpoint| endpoint.specification.network.as_str() != "overlay"))
    })
    .await?;
    delete_and_wait::<SecurityGroup>(&member, cluster, "overlay-web").await?;
    delete_and_wait::<Network>(&member, cluster, "overlay").await?;
    delete_and_wait::<Image>(&member, cluster, "busybox-overlay").await
}

async fn ensure_served_by(
    cluster: &dyn Cluster,
    address: Ipv4Addr,
    node: &NodeName,
    label: &str,
) -> ScenarioResult {
    eventually(cluster, START_TIMEOUT, &format!("{address} is served by {node}"), async || {
        let observation = cluster.observe().await?;
        let serving: Vec<&NodeName> = observation
            .floating_addresses
            .iter()
            .filter(|floating| floating.address == address)
            .map(|floating| &floating.node)
            .collect();
        Ok(serving == vec![node])
    })
    .await?;
    let probe = async || -> Result<bool, ScenarioError> {
        Ok(cluster.probe_from_outside(address, SERVER_PORT).await?.is_none_or(|body| body.contains(label)))
    };
    eventually(cluster, START_TIMEOUT, &format!("{address} answers as {label} from outside"), probe).await
}

async fn bound_to(
    member: &ApiClient,
    cluster: &dyn Cluster,
    node: &NodeName,
) -> Result<Ipv4Addr, ScenarioError> {
    eventually(cluster, START_TIMEOUT, &format!("the floating IP is bound on {node}"), async || {
        let floating = member.get::<FloatingIp>("public").await?;
        Ok(floating.status.phase == FloatingIpPhase::Bound
            && floating.status.binding.as_ref().is_some_and(|binding| binding.node == *node))
    })
    .await?;
    member
        .get::<FloatingIp>("public")
        .await?
        .status
        .address
        .ok_or_else(|| ScenarioError::Expectation(String::from("the bound floating IP has no address")))
}

fn target(vm: &str) -> Result<Option<FloatingIpTarget>, ScenarioError> {
    Ok(Some(FloatingIpTarget { vm: name(vm)?, interface: 0 }))
}

async fn a_floating_ip_follows_its_target(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let operator = client(cluster, Role::Operator).await;
    let member = client(cluster, Role::Member).await;
    operator.create::<ExternalNetwork>(&name("lab")?, &cluster.external_network()).await?;
    ready_image(&member, cluster, "busybox-floating", busybox()?).await?;
    ready_network(&member, cluster, "floating", "10.91.0.0/24").await?;
    let open = SecurityGroupRule {
        direction: Direction::Ingress,
        protocol: Protocol::Tcp,
        ports: Some(PortRange { first: SERVER_PORT, last: SERVER_PORT }),
        remote: Remote::Any,
    };
    member
        .create::<SecurityGroup>(&name("public-web")?, &SecurityGroupSpecification { rules: vec![open] })
        .await?;
    let mut servers = Vec::new();
    for server in ["server-one", "server-two"] {
        container(&member, server, "busybox-floating", "floating", &["public-web"], server_script(server))
            .await?;
    }
    for server in ["server-one", "server-two"] {
        let running = vm_in_phase(&member, cluster, server, VmPhase::Running, START_TIMEOUT).await?;
        servers.push(instance(cluster, running.id()).await?.node);
    }
    let specification = FloatingIpSpecification {
        external_network: name("lab")?,
        address: None,
        target: target("server-one")?,
    };
    member.create::<FloatingIp>(&name("public")?, &specification).await?;
    let address = bound_to(&member, cluster, &servers[0]).await?;
    ensure(cluster.external_network().contains(address), || {
        format!("{address} is outside the external range")
    })?;
    ensure_served_by(cluster, address, &servers[0], "server-one").await?;
    let moved = FloatingIpSpecification { target: target("server-two")?, ..specification };
    member.update::<FloatingIp>("public", &moved, None).await?;
    let readdressed = bound_to(&member, cluster, &servers[1]).await?;
    ensure(readdressed == address, || format!("the floating IP changed from {address} to {readdressed}"))?;
    ensure_served_by(cluster, address, &servers[1], "server-two").await?;
    always_during(cluster, SETTLING, a_floating_address_is_served_by_at_most_one_node).await?;
    delete_and_wait::<FloatingIp>(&member, cluster, "public").await?;
    for server in ["server-one", "server-two"] {
        delete_and_wait::<Vm>(&member, cluster, server).await?;
    }
    delete_and_wait::<SecurityGroup>(&member, cluster, "public-web").await?;
    delete_and_wait::<Network>(&member, cluster, "floating").await?;
    delete_and_wait::<Image>(&member, cluster, "busybox-floating").await?;
    delete_and_wait::<ExternalNetwork>(&operator, cluster, "lab").await
}
