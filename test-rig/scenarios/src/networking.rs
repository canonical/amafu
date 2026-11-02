use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_checkers::invariants::no_address_is_used_twice_in_one_network;
use amafu_domain::access::Role;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::network::{Network, NetworkPhase, NetworkSpecification};
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::kinds::vm::{BootSource, NetworkInterfaceSpecification, Vm, VmSpecification};
use amafu_domain::{Ipv4Network, Object};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, always_during, ensure, eventually,
};
use futures::FutureExt;

use crate::support::{
    CONTAINER_START_BUDGET, START_TIMEOUT, VM_START_BUDGET, busybox, console_shows, delete_and_wait, name,
    ready_image, started_within_budget,
};

pub const ADDRESSES_ARE_DISTINCT: ScenarioDefinition = ScenarioDefinition {
    name: "VMs on a network get distinct addresses from its subnet",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| vms_get_distinct_addresses(cluster).boxed_local(),
};

pub const SECURITY_GROUPS_DECIDE: ScenarioDefinition = ScenarioDefinition {
    name: "security groups decide which guests may connect, and forged addresses are cut off",
    levels: &[Level::VirtualLab],
    node_count: 1,
    run: |cluster| security_groups_decide_who_connects(cluster).boxed_local(),
};

const VM_NAMES: [&str; 3] = ["addressed-1", "addressed-2", "addressed-3"];
const SETTLING: Duration = Duration::from_secs(3);
const SERVER_PORT: u16 = 8080;

pub async fn ready_network(
    member: &ApiClient,
    cluster: &dyn Cluster,
    network_name: &str,
    subnet: &str,
) -> ScenarioResult {
    let subnet: Ipv4Network = subnet
        .parse()
        .map_err(|error: amafu_domain::InvalidAddress| ScenarioError::Expectation(error.to_string()))?;
    let specification = NetworkSpecification { subnet, is_outbound_access_allowed: true };
    member.create::<Network>(&name(network_name)?, &specification).await?;
    eventually(cluster, START_TIMEOUT, "the network is ready", async || {
        Ok(member.get::<Network>(network_name).await?.status.phase == NetworkPhase::Ready)
    })
    .await
}

pub fn interface(network: &str, groups: &[&str]) -> Result<NetworkInterfaceSpecification, ScenarioError> {
    Ok(NetworkInterfaceSpecification {
        network: name(network)?,
        security_groups: groups.iter().map(|group| name(group)).collect::<Result<_, _>>()?,
        address: None,
    })
}

fn address_of(vm: &Object<Vm>) -> Result<Ipv4Addr, ScenarioError> {
    vm.status
        .interfaces
        .first()
        .map(|interface| interface.address)
        .ok_or_else(|| ScenarioError::Expectation(format!("{} reports no address", vm.metadata.name)))
}

async fn vms_get_distinct_addresses(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    ready_network(&member, cluster, "addressed", "10.80.0.0/24").await?;
    let subnet: Ipv4Network =
        "10.80.0.0/24".parse().map_err(|_| ScenarioError::Expectation(String::new()))?;
    let mut addresses = BTreeSet::new();
    for vm_name in VM_NAMES {
        let specification = VmSpecification {
            network_interfaces: vec![interface("addressed", &[])?],
            ..VmSpecification::small()
        };
        member.create::<Vm>(&name(vm_name)?, &specification).await?;
        let running = started_within_budget(&member, cluster, vm_name, VM_START_BUDGET).await?;
        let address = address_of(&running)?;
        ensure(subnet.contains(address) && address != subnet.gateway(), || {
            format!("{vm_name} got {address}, which is not an assignable address of {subnet}")
        })?;
        ensure(addresses.insert(address), || format!("{address} was handed out twice"))?;
        eventually(cluster, START_TIMEOUT, "DHCP offers the assigned address", async || {
            Ok(cluster
                .observe()
                .await?
                .instance(running.id())
                .is_some_and(|instance| instance.offered_addresses == vec![address]))
        })
        .await?;
        console_shows(cluster, running.id(), &format!("amafu test guest address eth0 {address}")).await?;
    }
    always_during(cluster, SETTLING, no_address_is_used_twice_in_one_network).await?;
    for vm_name in VM_NAMES {
        delete_and_wait::<Vm>(&member, cluster, vm_name).await?;
    }
    delete_and_wait::<Network>(&member, cluster, "addressed").await
}

fn ingress_from_group(group: &str) -> Result<SecurityGroupRule, ScenarioError> {
    Ok(SecurityGroupRule {
        direction: Direction::Ingress,
        protocol: Protocol::Tcp,
        ports: Some(PortRange { first: SERVER_PORT, last: SERVER_PORT }),
        remote: Remote::SecurityGroup { name: name(group)? },
    })
}

async fn container_on(
    member: &ApiClient,
    container_name: &str,
    groups: &[&str],
    script: String,
) -> Result<Object<Vm>, ScenarioError> {
    let mut specification = VmSpecification::container(name("busybox-network")?);
    specification.network_interfaces = vec![interface("guarded", groups)?];
    if let BootSource::Container { command, .. } = &mut specification.boot {
        *command = vec![String::from("sh"), String::from("-c"), script];
    }
    Ok(member.create::<Vm>(&name(container_name)?, &specification).await?)
}

fn probe_script(label: &str) -> String {
    format!(
        "sleep 2; for attempt in 1 2 3; do if wget -q -T 3 -O - http://guarded-server:{SERVER_PORT}/ | grep -q served; then echo {label}-REACHED; break; fi; sleep 1; done; echo {label}-DONE; sleep 3600"
    )
}

fn forging_script(forged: Ipv4Addr) -> String {
    format!(
        "sleep 2; ip address flush dev eth0; ip address add {forged}/24 dev eth0; if wget -q -T 3 -O - http://10.81.0.2:{SERVER_PORT}/ | grep -q served; then echo FORGED-REACHED; fi; echo FORGED-DONE; sleep 3600"
    )
}

async fn security_groups_decide_who_connects(cluster: &dyn Cluster) -> ScenarioResult {
    let member = client(cluster, Role::Member).await;
    ready_image(&member, cluster, "busybox-network", busybox()?).await?;
    ready_network(&member, cluster, "guarded", "10.81.0.0/24").await?;
    let server_rules = SecurityGroupSpecification { rules: vec![ingress_from_group("clients")?] };
    member.create::<SecurityGroup>(&name("servers")?, &server_rules).await?;
    member.create::<SecurityGroup>(&name("clients")?, &SecurityGroupSpecification::default()).await?;
    let server_script =
        format!("mkdir -p /www && echo served > /www/index.html && httpd -f -p {SERVER_PORT} -h /www");
    container_on(&member, "guarded-server", &["servers"], server_script).await?;
    started_within_budget(&member, cluster, "guarded-server", CONTAINER_START_BUDGET).await?;
    let server = member.get::<Vm>("guarded-server").await?;
    ensure(address_of(&server)? == Ipv4Addr::new(10, 81, 0, 2), || {
        format!("the server got {:?}, the forging client expects 10.81.0.2", server.status.interfaces)
    })?;
    let allowed = container_on(&member, "allowed-client", &["clients"], probe_script("ALLOWED")).await?;
    let refused = container_on(&member, "refused-client", &[], probe_script("REFUSED")).await?;
    let forger =
        container_on(&member, "forging-client", &["clients"], forging_script(Ipv4Addr::new(10, 81, 0, 200)))
            .await?;
    console_shows(cluster, allowed.id(), "ALLOWED-REACHED").await?;
    for (vm, label) in [(&refused, "REFUSED"), (&forger, "FORGED")] {
        console_shows(cluster, vm.id(), &format!("{label}-DONE")).await?;
        let console = crate::support::instance(cluster, vm.id()).await?.console;
        ensure(!console.contains(&format!("{label}-REACHED")), || {
            format!("the {label} client reached the server")
        })?;
    }
    for vm_name in ["allowed-client", "refused-client", "forging-client", "guarded-server"] {
        delete_and_wait::<Vm>(&member, cluster, vm_name).await?;
    }
    for group in ["servers", "clients"] {
        delete_and_wait::<SecurityGroup>(&member, cluster, group).await?;
    }
    delete_and_wait::<Network>(&member, cluster, "guarded").await?;
    delete_and_wait::<Image>(&member, cluster, "busybox-network").await
}
