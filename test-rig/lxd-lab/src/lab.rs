use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use amafu_api::diagnostics::{BOOT_PATH, BootFacts};
use amafu_domain::NodeName;
use amafu_domain::kinds::node::NodeRole;
use amafu_network::{StaticAddress, UnderlayConfiguration};
use amafu_node::configuration::{
    ApiSection, ComputeSection, DiagnosticsSection, Directories, NodeConfiguration, NodeSection,
    SUPPORTED_VERSION, StateSection,
};
use anyhow::{Context, bail};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use amafu_scenario::CertificateAuthority;

use crate::credentials::{ClientTokens, ServerCredentials, client_tokens};
use crate::lxd::{LxdClient, LxdCredentials};
use crate::nfs::LabNfsServer;
use crate::topology::{LabNode, LabSettings, LabTopology, NODE_API_PORT, NODE_DIAGNOSTICS_PORT, node_listen};

const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
const BOOT_POLL: Duration = Duration::from_millis(250);
const IMAGE_ALIAS: &str = "amafu-node-under-test";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabState {
    pub settings: LabSettings,
    pub topology: LabTopology,
    pub authority: CertificateAuthority,
    pub server: ServerCredentials,
    pub tokens: ClientTokens,
    pub image_fingerprint: String,
    #[serde(default)]
    pub nfs: Option<LabNfsServer>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeBoot {
    pub node: String,
    pub seconds_from_start_to_serving: f64,
    pub facts: BootFacts,
}

#[must_use]
pub fn state_file(repository: &Path) -> PathBuf {
    repository.join("target/rig/lab.json")
}

pub fn load_state(repository: &Path) -> anyhow::Result<LabState> {
    let path = state_file(repository);
    let contents =
        std::fs::read(&path).with_context(|| format!("no lab is up ({} is missing)", path.display()))?;
    Ok(serde_json::from_slice(&contents)?)
}

fn save_state(repository: &Path, state: &LabState) -> anyhow::Result<()> {
    let path = state_file(repository);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

pub fn lxd_client(settings: &LabSettings) -> anyhow::Result<LxdClient> {
    let credentials = LxdCredentials::from_snap_configuration(&settings.lxd_endpoint, &settings.lxd_remote)?;
    LxdClient::new(&credentials, &settings.project)
}

fn node_configuration(
    settings: &LabSettings,
    node: &LabNode,
    server: &ServerCredentials,
    identities: Vec<amafu_api::TokenIdentity>,
) -> anyhow::Result<NodeConfiguration> {
    Ok(NodeConfiguration {
        version: SUPPORTED_VERSION,
        node: NodeSection {
            name: NodeName::new(node.name.as_str())?,
            roles: BTreeSet::from([NodeRole::Core, NodeRole::Compute]),
            zone: None,
        },
        network: Some(UnderlayConfiguration {
            interface: None,
            address: StaticAddress { address: node.address, prefix_length: settings.prefix_length },
            gateway: Some(settings.gateway),
        }),
        api: ApiSection {
            listen: node_listen(NODE_API_PORT),
            certificate: server.certificate.clone(),
            private_key: server.private_key.clone(),
            identities,
        },
        state: StateSection::default(),
        directories: Directories::default(),
        compute: ComputeSection { resolvers: vec![settings.gateway], ..ComputeSection::default() },
        diagnostics: Some(DiagnosticsSection { listen: node_listen(NODE_DIAGNOSTICS_PORT) }),
    })
}

fn assemble_disk(
    repository: &Path,
    image: &Path,
    configuration: &Path,
    output: &Path,
) -> anyhow::Result<PathBuf> {
    let status = Command::new(repository.join("image/assemble-disk"))
        .arg(image)
        .arg(configuration)
        .arg(output)
        .status()
        .context("running image/assemble-disk")?;
    if !status.success() {
        bail!("image/assemble-disk failed with {status}");
    }
    Ok(output.join("lxd-image.tar"))
}

async fn ensure_project(lxd: &LxdClient, settings: &LabSettings) -> anyhow::Result<()> {
    let default = lxd.in_project("default");
    if default.exists(&format!("/1.0/projects/{}", settings.project)).await? {
        return Ok(());
    }
    let body = json!({
        "name": settings.project,
        "description": "Amafu virtual lab",
        "config": {
            "features.images": "true",
            "features.profiles": "true",
            "features.storage.volumes": "true",
            "limits.memory": "12GiB",
            "limits.cpu": "6",
        },
    });
    default.request(Method::POST, "/1.0/projects", Some(body)).await?;
    Ok(())
}

fn instance_definition(settings: &LabSettings, node: &LabNode, fingerprint: &str) -> serde_json::Value {
    let proxy = |listen_port: u16, node_port: u16| {
        json!({
            "type": "proxy",
            "listen": format!("tcp:{}:{listen_port}", settings.host_address),
            "connect": format!("tcp:{}:{node_port}", node.address),
            "nat": "true",
        })
    };
    json!({
        "name": node.name,
        "type": "virtual-machine",
        "source": { "type": "image", "fingerprint": fingerprint },
        "config": {
            "limits.cpu": node.cpu_count.to_string(),
            "limits.memory": format!("{}GiB", node.memory_gibibytes),
            "boot.mode": "uefi-secureboot",
        },
        "devices": {
            "root": {
                "type": "disk",
                "path": "/",
                "pool": settings.storage_pool,
                "size": format!("{}GiB", node.disk_gibibytes),
            },
            "eth0": { "type": "nic", "network": settings.network, "ipv4.address": node.address.to_string() },
            "vtpm": { "type": "tpm", "path": "/dev/tpm0" },
            "api": proxy(node.api_port, NODE_API_PORT),
            "diagnostics": proxy(node.diagnostics_port, NODE_DIAGNOSTICS_PORT),
        },
    })
}

pub async fn wait_until_serving(settings: &LabSettings, node: &LabNode) -> anyhow::Result<NodeBoot> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let url = format!("{}{BOOT_PATH}", node.diagnostics_endpoint(settings));
    let started = Instant::now();
    loop {
        if let Ok(response) = http.get(&url).send().await
            && response.status().is_success()
        {
            let facts: BootFacts = response.json().await?;
            if facts.seconds_from_kernel_start_to_serving.is_some() {
                return Ok(NodeBoot {
                    node: node.name.clone(),
                    seconds_from_start_to_serving: started.elapsed().as_secs_f64(),
                    facts,
                });
            }
        }
        if started.elapsed() >= BOOT_TIMEOUT {
            bail!("{} did not start serving within {} seconds", node.name, BOOT_TIMEOUT.as_secs());
        }
        tokio::time::sleep(BOOT_POLL).await;
    }
}

pub async fn up(
    repository: &Path,
    settings: LabSettings,
    node_count: u8,
) -> anyhow::Result<(LabState, Vec<NodeBoot>)> {
    let image = repository.join(&settings.image_directory);
    if !image.join("esp").is_dir() {
        bail!("no built image in {}; run image/build test first", image.display());
    }
    let topology = LabTopology::standard(&settings, node_count);
    let lxd = lxd_client(&settings)?;
    ensure_project(&lxd, &settings).await?;
    let nfs = LabNfsServer::standard(&settings, topology.next_proxy_port(&settings));
    crate::nfs::create(&lxd, &settings, &nfs).await?;
    let (tokens, identities) = client_tokens(OffsetDateTime::now_utc())?;
    let authority = CertificateAuthority::generate("Amafu lab certificate authority")?;
    let work = repository.join("target/rig/work");
    std::fs::create_dir_all(&work)?;
    let mut fingerprint = String::new();
    let mut server = None;
    for node in &topology.nodes {
        let addresses = [IpAddr::V4(settings.host_address), IpAddr::V4(node.address)];
        let issued = authority.issue_server(&node.name, &addresses)?;
        let credentials = ServerCredentials {
            certificate_authority: authority.certificate_pem.clone(),
            certificate: issued.certificate_pem,
            private_key: issued.private_key_pem,
        };
        let configuration = node_configuration(&settings, node, &credentials, identities.clone())?;
        let configuration_path = work.join(format!("{}-configuration.yaml", node.name));
        std::fs::write(&configuration_path, serde_yaml_ng::to_string(&configuration)?)?;
        let tarball = assemble_disk(repository, &image, &configuration_path, &work.join(&node.name))?;
        fingerprint = lxd.upload_image(&tarball).await?;
        let _ = lxd
            .request(
                Method::POST,
                "/1.0/images/aliases",
                Some(json!({ "name": IMAGE_ALIAS, "target": fingerprint })),
            )
            .await;
        lxd.request(Method::POST, "/1.0/instances", Some(instance_definition(&settings, node, &fingerprint)))
            .await?;
        server = Some(credentials);
    }
    let server = server.context("the lab has no node")?;
    let state = LabState {
        settings,
        topology,
        authority,
        server,
        tokens,
        image_fingerprint: fingerprint,
        nfs: Some(nfs),
    };
    save_state(repository, &state)?;
    let mut boots = Vec::new();
    for node in &state.topology.nodes {
        lxd.change_state(&node.name, "start", false).await?;
        boots.push(wait_until_serving(&state.settings, node).await?);
    }
    if let Some(nfs) = &state.nfs {
        let waited = crate::nfs::wait_until_serving(&state.settings, nfs).await?;
        tracing::info!(seconds = waited.as_secs(), "the NFS server is serving");
    }
    Ok((state, boots))
}

fn last_path_segment(url: &str) -> &str {
    let path = url.split_once('?').map_or(url, |(path, _)| path);
    path.rsplit('/').next().unwrap_or_default()
}

async fn stop_instance(lxd: &LxdClient, name: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        let state = lxd.request(Method::GET, &format!("/1.0/instances/{name}/state"), None).await?;
        if state["status"].as_str() == Some("Stopped") {
            return Ok(());
        }
        if let Err(error) = lxd.change_state(name, "stop", true).await {
            tracing::info!(%error, instance = name, "stopping failed; retrying");
        }
        if started.elapsed() >= BOOT_TIMEOUT {
            bail!("{name} did not stop");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn down(repository: &Path, settings: &LabSettings) -> anyhow::Result<()> {
    let lxd = lxd_client(settings)?;
    let default = lxd.in_project("default");
    if !default.exists(&format!("/1.0/projects/{}", settings.project)).await? {
        let _ = std::fs::remove_file(state_file(repository));
        return Ok(());
    }
    let instances = lxd.request(Method::GET, "/1.0/instances", None).await?;
    for url in instances.as_array().into_iter().flatten().filter_map(|value| value.as_str()) {
        let name = last_path_segment(url);
        stop_instance(&lxd, name).await?;
        lxd.request(Method::DELETE, &format!("/1.0/instances/{name}"), None).await?;
    }
    let images = lxd.request(Method::GET, "/1.0/images", None).await?;
    for url in images.as_array().into_iter().flatten().filter_map(|value| value.as_str()) {
        let fingerprint = last_path_segment(url);
        lxd.request(Method::DELETE, &format!("/1.0/images/{fingerprint}"), None).await?;
    }
    let profile = default.in_project(&settings.project);
    let _ = profile
        .request(Method::PUT, "/1.0/profiles/default", Some(json!({ "config": {}, "devices": {} })))
        .await;
    default.request(Method::DELETE, &format!("/1.0/projects/{}", settings.project), None).await?;
    let _ = std::fs::remove_file(state_file(repository));
    let _ = std::fs::remove_dir_all(repository.join("target/rig/work"));
    Ok(())
}
