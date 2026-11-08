use std::collections::BTreeMap;
use std::fmt::Write;

use amafu_domain::kinds::cluster_version::ClusterVersion;
use amafu_domain::kinds::external_network::ExternalNetwork;
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::Operation;
use amafu_domain::kinds::security_group::SecurityGroup;
use amafu_domain::kinds::storage_backend::StorageBackend;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference, ProjectName, Resource};
use amafu_interfaces::Store;
use amafu_interfaces::objects::{apply, update_status};
use anyhow::Context;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const DEFAULT_PROJECT: &str = "default";
pub const RESEARCH_PROJECT: &str = "research";
const NODE_COUNT: usize = 9;
const ZONES: [&str; 3] = ["zone-a", "zone-b", "zone-c"];
const CURRENT_VERSION: &str = "26.04.1";
const PREVIOUS_VERSION: &str = "26.04.0";

#[must_use]
pub fn digest_of(seed: &str) -> String {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hexadecimal = String::new();
    while hexadecimal.len() < 64 {
        for byte in seed.bytes() {
            value = (value ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        value = value.wrapping_add(hexadecimal.len() as u64);
        let _ = write!(hexadecimal, "{value:016x}");
    }
    hexadecimal.truncate(64);
    hexadecimal
}

#[must_use]
pub fn now() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default()
}

fn reference<R: Resource>(project: Option<&str>, name: &str) -> anyhow::Result<ObjectReference> {
    Ok(ObjectReference {
        kind: R::KIND.to_owned(),
        project: project.map(ProjectName::new).transpose()?,
        name: ObjectName::new(name)?,
    })
}

pub async fn put<R: Resource>(
    store: &dyn Store,
    project: Option<&str>,
    name: &str,
    specification: Value,
) -> anyhow::Result<ObjectReference> {
    let reference = reference::<R>(project, name)?;
    let specification = serde_json::from_value(specification)
        .with_context(|| format!("the specification of {} {name}", R::KIND))?;
    apply::<R>(
        store,
        &reference,
        Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name: reference.name.clone(),
            project: reference.project.clone(),
            labels: BTreeMap::new(),
            specification,
        },
    )
    .await?;
    Ok(reference)
}

pub async fn put_with_status<R: Resource>(
    store: &dyn Store,
    project: Option<&str>,
    name: &str,
    specification: Value,
    status: Value,
) -> anyhow::Result<()> {
    let reference = put::<R>(store, project, name, specification).await?;
    let status: R::Status =
        serde_json::from_value(status).with_context(|| format!("the status of {} {name}", R::KIND))?;
    update_status::<R>(store, &reference, |_| Some(status.clone())).await?;
    Ok(())
}

async fn seed_nodes(store: &dyn Store) -> anyhow::Result<()> {
    for index in 1..=NODE_COUNT {
        let zone = ZONES[(index - 1) % ZONES.len()];
        let is_core = index <= ZONES.len();
        let roles = if is_core { json!(["core", "compute"]) } else { json!(["compute"]) };
        let identity = |version: &str| json!({"version": version, "digest": digest_of(version)});
        put_with_status::<Node>(
            store,
            None,
            &format!("node-{index}"),
            json!({"roles": roles, "is_cordoned": index == NODE_COUNT, "zone": zone}),
            json!({
                "phase": "ready",
                "capacity": {"cpu_count": 64, "memory_mebibytes": 262_144},
                "cpu_baseline": "x86-64-v3",
                "underlay_address": format!("10.0.0.{}", 10 + index),
                "software_version": CURRENT_VERSION,
                "last_heartbeat": now(),
                "boot": {
                    "booted_slot": "a",
                    "default_slot": "a",
                    "booted_image": identity(CURRENT_VERSION),
                    "inactive_image": identity(PREVIOUS_VERSION),
                }
            }),
        )
        .await?;
    }
    Ok(())
}

async fn seed_cluster(store: &dyn Store) -> anyhow::Result<()> {
    let image = |version: &str| {
        json!({
            "version": version,
            "feature_level": 3,
            "digest": digest_of(version),
            "size_bytes": 412_000_000,
            "url": format!("https://images.example/amafu-{version}.img"),
            "signature": format!("{}{}", digest_of(version), digest_of(&format!("{version}-signature"))),
        })
    };
    put::<ClusterVersion>(
        store,
        None,
        "cluster",
        json!({"image": image(CURRENT_VERSION), "previous_image": image(PREVIOUS_VERSION), "finalized_feature_level": 3}),
    )
    .await?;
    let backends = [
        (
            "ceph-prod",
            json!({"type": "ceph-rbd", "monitors": ["10.0.1.11:6789", "10.0.1.12:6789", "10.0.1.13:6789"], "pool": "amafu",
                   "user": "amafu", "key": "demo", "management": "https://ceph.example:8443", "management_user": "amafu",
                   "management_password": "demo", "command_endpoint": "https://ceph.example:8003", "command_user": "amafu",
                   "command_key": "demo"}),
            "ready",
        ),
        (
            "flash",
            json!({"type": "nvme-tcp", "management": "http://10.0.2.10:5260", "addresses": ["10.0.2.10:4420", "10.0.2.11:4420"], "pool": "flash"}),
            "ready",
        ),
        ("nfs-archive", json!({"type": "nfs", "server": "10.0.3.10", "export": "/srv/amafu"}), "ready"),
        ("local", json!({"type": "local-disk"}), "ready"),
        (
            "vendor-array",
            json!({"type": "plugin", "module": {"url": "https://plugins.example/spdk-rpc.wasm", "digest": digest_of("spdk-rpc")},
                   "configuration": {"management": "http://10.0.4.10:5260", "addresses": ["10.0.4.10:4420"], "pool": "vendor"},
                   "endpoints": ["http://10.0.4.10:5260"], "secrets": {"user": "amafu", "password": "demo"}}),
            "unreachable",
        ),
    ];
    for (name, provider, phase) in backends {
        let message = (phase == "unreachable").then_some("the management endpoint refused the connection");
        put_with_status::<StorageBackend>(
            store,
            None,
            name,
            json!({"provider": provider}),
            json!({"phase": phase, "message": message}),
        )
        .await?;
    }
    put::<ExternalNetwork>(
        store,
        None,
        "public",
        json!({"subnet": "203.0.113.0/24", "first_address": "203.0.113.10", "last_address": "203.0.113.209"}),
    )
    .await?;
    put::<ExternalNetwork>(
        store,
        None,
        "edge-bgp",
        json!({"subnet": "198.51.100.0/26", "first_address": "198.51.100.2", "last_address": "198.51.100.62"}),
    )
    .await?;
    Ok(())
}

async fn seed_networking(store: &dyn Store, project: &str) -> anyhow::Result<()> {
    let networks =
        [("app", "10.10.0.0/24", true), ("db", "10.20.0.0/24", true), ("corp", "10.30.0.0/24", false)];
    for (name, subnet, is_outbound_access_allowed) in networks {
        put::<Network>(
            store,
            Some(project),
            name,
            json!({"subnet": subnet, "is_outbound_access_allowed": is_outbound_access_allowed}),
        )
        .await?;
    }
    let rule = |direction: &str, protocol: &str, port: Option<u16>, remote: Value| json!({"direction": direction, "protocol": protocol, "ports": port.map(|port| json!({"first": port, "last": port})), "remote": remote});
    let anywhere = json!({"type": "any"});
    let groups = [
        ("default", vec![rule("ingress", "any", None, json!({"type": "security-group", "name": "default"}))]),
        (
            "web",
            vec![
                rule("ingress", "tcp", Some(443), anywhere.clone()),
                rule("ingress", "tcp", Some(80), anywhere.clone()),
                rule("ingress", "tcp", Some(22), json!({"type": "addresses", "network": "10.30.0.0/24"})),
                rule("egress", "any", None, anywhere.clone()),
            ],
        ),
        ("api", vec![rule("ingress", "tcp", Some(8080), json!({"type": "security-group", "name": "web"}))]),
        (
            "db",
            vec![
                rule("ingress", "tcp", Some(5432), json!({"type": "security-group", "name": "api"})),
                rule("ingress", "tcp", Some(5432), json!({"type": "security-group", "name": "db"})),
            ],
        ),
        (
            "monitoring",
            vec![rule("egress", "tcp", Some(9100), json!({"type": "addresses", "network": "10.0.0.0/8"}))],
        ),
    ];
    for (name, rules) in groups {
        put::<SecurityGroup>(store, Some(project), name, json!({"rules": rules})).await?;
    }
    Ok(())
}

async fn seed_images_and_volumes(store: &dyn Store, project: &str) -> anyhow::Result<()> {
    for (name, url) in [
        ("ubuntu-noble", "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"),
        ("debian-13", "https://cloud.debian.org/images/cloud/trixie/latest/debian-13-generic-amd64.qcow2"),
        ("rhel-9", "https://images.example/rhel-9.6-x86_64-kvm.qcow2"),
    ] {
        put::<Image>(
            store,
            Some(project),
            name,
            json!({"source": {"type": "disk", "url": url, "sha256": digest_of(name)}}),
        )
        .await?;
    }
    for (name, repository) in [
        ("acme-api", "ghcr.io/acme/api"),
        ("redis", "docker.io/library/redis"),
        ("nginx", "docker.io/library/nginx"),
    ] {
        let reference = format!("{repository}@sha256:{}", digest_of(name));
        put::<Image>(
            store,
            Some(project),
            name,
            json!({"source": {"type": "container", "reference": reference}}),
        )
        .await?;
    }
    let volumes = [
        ("web-1-root", 50, "ceph-prod", Some("ubuntu-noble")),
        ("web-2-root", 50, "ceph-prod", Some("ubuntu-noble")),
        ("web-3-root", 50, "ceph-prod", Some("ubuntu-noble")),
        ("db-primary-data", 2048, "flash", Some("rhel-9")),
        ("db-replica-data", 2048, "flash", Some("rhel-9")),
        ("build-cache", 200, "local", Some("debian-13")),
        ("monitoring-data", 500, "nfs-archive", Some("ubuntu-noble")),
        ("scratch", 20, "ceph-prod", None),
    ];
    for (name, size, backend, image) in volumes {
        put::<Volume>(
            store,
            Some(project),
            name,
            json!({"size_gibibytes": size, "backend": backend, "source_image": image}),
        )
        .await?;
    }
    Ok(())
}

fn interface(network: &str, groups: &[&str]) -> Value {
    json!({"network": network, "security_groups": groups})
}

async fn seed_workloads(store: &dyn Store, project: &str) -> anyhow::Result<()> {
    let disk_vm = |cpus: u16, mebibytes: u64, volume: &str, interfaces: Vec<Value>, power: &str| {
        json!({"cpu_count": cpus, "memory_mebibytes": mebibytes, "boot": {"type": "disk"}, "volumes": [volume],
               "network_interfaces": interfaces, "power_state": power})
    };
    let vms = [
        ("web-1", disk_vm(4, 8192, "web-1-root", vec![interface("app", &["web"])], "running")),
        ("web-2", disk_vm(4, 8192, "web-2-root", vec![interface("app", &["web"])], "running")),
        ("web-3", disk_vm(4, 8192, "web-3-root", vec![interface("app", &["web"])], "running")),
        ("db-primary", disk_vm(16, 131_072, "db-primary-data", vec![interface("db", &["db"])], "running")),
        ("db-replica", disk_vm(16, 131_072, "db-replica-data", vec![interface("db", &["db"])], "running")),
        ("build-runner", disk_vm(8, 16_384, "build-cache", vec![interface("corp", &["default"])], "stopped")),
        (
            "monitoring",
            disk_vm(4, 16_384, "monitoring-data", vec![interface("app", &["monitoring"])], "running"),
        ),
    ];
    for (name, specification) in vms {
        put::<Vm>(store, Some(project), name, specification).await?;
    }
    let container = |image: &str, cpus: u16, mebibytes: u64, groups: &[&str], environment: Value| {
        json!({"cpu_count": cpus, "memory_mebibytes": mebibytes,
               "boot": {"type": "container", "image": image, "command": [], "environment": environment},
               "network_interfaces": [interface("app", groups)], "power_state": "running"})
    };
    let containers = [
        ("api", container("acme-api", 2, 4096, &["api"], json!({"PORT": "8080"}))),
        ("api-2", container("acme-api", 2, 4096, &["api"], json!({"PORT": "8080"}))),
        ("worker", container("acme-api", 1, 2048, &["default"], json!({"ROLE": "worker"}))),
        ("redis", container("redis", 1, 1024, &["default"], json!({}))),
        ("nginx-edge", container("nginx", 1, 512, &["web"], json!({}))),
    ];
    for (name, specification) in containers {
        put::<Vm>(store, Some(project), name, specification).await?;
    }
    let floating_ips = [
        ("web-1-public", "public", Some("web-1")),
        ("web-2-public", "public", Some("web-2")),
        ("monitoring-public", "public", Some("monitoring")),
        ("api-public", "public", Some("api")),
        ("spare", "public", None),
        ("edge", "edge-bgp", Some("nginx-edge")),
    ];
    for (name, network, target) in floating_ips {
        let target = target.map(|vm| json!({"vm": vm, "interface": 0}));
        put::<FloatingIp>(store, Some(project), name, json!({"external_network": network, "target": target}))
            .await?;
    }
    Ok(())
}

async fn seed_research(store: &dyn Store) -> anyhow::Result<()> {
    put::<Network>(
        store,
        Some(RESEARCH_PROJECT),
        "gpu",
        json!({"subnet": "10.40.0.0/24", "is_outbound_access_allowed": true}),
    )
    .await?;
    for (name, cpus, mebibytes) in [("gpu-train-1", 32, 262_144 / 2), ("notebook", 8, 32_768)] {
        put::<Vm>(
            store,
            Some(RESEARCH_PROJECT),
            name,
            json!({"cpu_count": cpus, "memory_mebibytes": mebibytes, "boot": {"type": "kernel", "bundle": "ubuntu-noble"},
                   "network_interfaces": [interface("gpu", &[])], "power_state": "running"}),
        )
        .await?;
    }
    Ok(())
}

async fn seed_history(store: &dyn Store) -> anyhow::Result<()> {
    let vm = |name: &str| json!({"project": DEFAULT_PROJECT, "name": name});
    let migration_steps = |name: &str, destination: Option<&str>| {
        let mut prepare = json!({"step": "prepare-destination", "vm": vm(name)});
        if let Some(destination) = destination {
            prepare["destination"] = json!(destination);
        }
        json!([prepare, {"step": "copy-memory", "vm": vm(name)}, {"step": "switch-over", "vm": vm(name)},
               {"step": "announce-addresses", "vm": vm(name)}, {"step": "clean-up-source", "vm": vm(name)}])
    };
    put_with_status::<Operation>(
        store,
        None,
        "migrate-web-3-4f1a",
        json!({"kind": "migration", "target": {"kind": "vm", "vm": vm("web-3")}, "steps": migration_steps("web-3", None),
               "is_paused": false, "is_cancelled": false}),
        json!({"completed_steps": 5, "outcome": {"outcome": "succeeded"}}),
    )
    .await?;
    put_with_status::<Operation>(
        store,
        None,
        "migrate-db-replica-77c2",
        json!({"kind": "migration", "target": {"kind": "vm", "vm": vm("db-replica")}, "steps": migration_steps("db-replica", Some("node-8")),
               "is_paused": true, "is_cancelled": false}),
        json!({"completed_steps": 0}),
    )
    .await?;
    Ok(())
}

pub async fn seed(store: &dyn Store) -> anyhow::Result<()> {
    seed_nodes(store).await?;
    seed_cluster(store).await?;
    seed_networking(store, DEFAULT_PROJECT).await?;
    seed_images_and_volumes(store, DEFAULT_PROJECT).await?;
    seed_workloads(store, DEFAULT_PROJECT).await?;
    seed_research(store).await?;
    seed_history(store).await
}
