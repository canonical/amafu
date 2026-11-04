use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use amafu_domain::kinds::node::NodeRole;
use amafu_domain::{MacAddress, NodeName};
use amafu_installer::InstallDescription;
use amafu_installer::description::{BootLoader, ClusterSection, MaasSection, MachineSection};
use amafu_interfaces::ImageSigningKey;
use amafu_network::{StaticAddress, UnderlayConfiguration};
use anyhow::{Context, bail};
use rcgen::{CertificateParams, KeyPair};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::lab::lxd_client;
use crate::lxd::LxdClient;
use crate::machines::{MachineSize, wait_for_outcome};
use crate::node_images::{IMAGE_DIRECTORY, IMAGE_SERVER_PORT, prepare};
use crate::topology::LabSettings;

pub const MAAS_PROJECT: &str = "amafu-maas";
const MAAS_NETWORK: &str = "amafu-maas0";
const MAAS_INSTANCE: &str = "maas";
const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 217, 0, 1);
const MAAS_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 217, 0, 2);
const PREFIX_LENGTH: u8 = 24;
const FIRST_NODE_HOST: u8 = 10;
const DYNAMIC_RANGE: (&str, &str) = ("10.217.0.100", "10.217.0.199");
const READY_MARKER: &str = "/var/lib/amafu-maas-ready";
const FAILED_MARKER: &str = "/var/lib/amafu-maas-failed";
const SETUP_LOG: &str = "/var/log/amafu-maas-setup.log";
const SETUP_TIMEOUT: Duration = Duration::from_secs(3600);
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(1800);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(3600);
const POLL: Duration = Duration::from_secs(10);
const INSTALL_DIRECTORY: &str = "/root/amafu-install";
const INSTALL_DESCRIPTION: &str = "/root/install.yaml";
const INSTALL_LOG: &str = "/root/amafu-install.log";
const INSTALL_DONE: &str = "/root/amafu-install.done";
const CLI_PATH: &str = "/usr/local/bin/amafu";
const MACHINE_MEMORY_MEBIBYTES: u32 = 3072;
const MACHINE_CPU_COUNT: u32 = 2;
const MACHINE_DISK_GIBIBYTES: u32 = 16;
const CORE_MEMBER_COUNT: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaasMachine {
    pub hostname: String,
    pub node: NodeName,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
    pub is_core: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaasLabState {
    pub machines: Vec<MaasMachine>,
    pub trust_fingerprint: String,
    pub image_signing_key: String,
}

fn state_file(repository: &Path) -> PathBuf {
    repository.join("target/rig/maas-lab.json")
}

fn machines(count: u8) -> Vec<MaasMachine> {
    (0..count)
        .map(|index| MaasMachine {
            hostname: format!("amafu-{index}"),
            node: NodeName::new(format!("node-{index}")).unwrap_or_else(|_| unreachable!()),
            mac: MacAddress::new([0x00, 0x16, 0x3e, 0xb0, 0x00, index]),
            address: Ipv4Addr::new(10, 217, 0, FIRST_NODE_HOST + index),
            is_core: usize::from(index) < CORE_MEMBER_COUNT,
        })
        .collect()
}

fn maas_user_data(password: &str) -> String {
    format!(
        r"#cloud-config
write_files:
  - path: /usr/local/sbin/amafu-maas-setup
    permissions: '0755'
    content: |
      #!/bin/bash
      set -euxo pipefail
      snap install maas-test-db --channel=3.6/stable
      snap install maas --channel=3.6/stable
      maas init region+rack --database-uri maas-test-db:/// --maas-url http://{MAAS_ADDRESS}:5240/MAAS --force
      maas createadmin --username admin --password '{password}' --email admin@amafu.lab
      maas apikey --username admin > /root/maas-api-key
      maas login admin http://localhost:5240/MAAS/api/2.0/ \$(cat /root/maas-api-key)
      maas admin boot-resources import
      until [ \$(maas admin boot-resources is-importing) = false ]; do sleep 20; done
      mkdir -p {IMAGE_DIRECTORY}
      touch {READY_MARKER}
  - path: /etc/systemd/system/amafu-images.service
    content: |
      [Service]
      ExecStart=/usr/bin/python3 -m http.server {IMAGE_SERVER_PORT} --directory {IMAGE_DIRECTORY}
      Restart=always
      [Install]
      WantedBy=multi-user.target
runcmd:
  - [mkdir, -p, {IMAGE_DIRECTORY}]
  - [systemctl, enable, --now, amafu-images.service]
  - [sh, -c, '/usr/local/sbin/amafu-maas-setup > {SETUP_LOG} 2>&1 || touch {FAILED_MARKER}']
"
    )
}

fn maas_network_configuration() -> String {
    format!(
        "version: 2\nethernets:\n  maas:\n    match:\n      name: enp5s0\n    addresses: [{MAAS_ADDRESS}/{PREFIX_LENGTH}]\n    routes:\n      - to: default\n        via: {GATEWAY}\n    nameservers:\n      addresses: [{GATEWAY}]\n"
    )
}

async fn ensure_project_and_network(lxd: &LxdClient) -> anyhow::Result<()> {
    let default = lxd.in_project("default");
    if !default.exists(&format!("/1.0/projects/{MAAS_PROJECT}")).await? {
        let project = json!({
            "name": MAAS_PROJECT,
            "description": "Amafu MAAS lab",
            "config": { "features.images": "false", "features.profiles": "false", "features.storage.volumes": "true" },
        });
        default.request(Method::POST, "/1.0/projects", Some(project)).await?;
    }
    if !default.exists(&format!("/1.0/networks/{MAAS_NETWORK}")).await? {
        let network = json!({
            "name": MAAS_NETWORK,
            "type": "bridge",
            "config": {
                "ipv4.address": format!("{GATEWAY}/{PREFIX_LENGTH}"),
                "ipv4.dhcp": "false",
                "ipv4.nat": "true",
                "ipv6.address": "none",
            },
        });
        default.request(Method::POST, "/1.0/networks", Some(network)).await?;
    }
    Ok(())
}

fn maas_instance(settings: &LabSettings, password: &str) -> serde_json::Value {
    let size = MachineSize { cpu_count: 2, memory_mebibytes: 4096, disk_gibibytes: 40 };
    json!({
        "name": MAAS_INSTANCE,
        "type": "virtual-machine",
        "source": {"type": "image", "mode": "pull", "server": "https://cloud-images.ubuntu.com/releases", "protocol": "simplestreams", "alias": "24.04"},
        "config": {
            "limits.cpu": size.cpu_count.to_string(),
            "limits.memory": format!("{}MiB", size.memory_mebibytes),
            "cloud-init.user-data": maas_user_data(password),
            "cloud-init.network-config": maas_network_configuration(),
        },
        "devices": {
            "root": { "type": "disk", "path": "/", "pool": settings.storage_pool, "size": format!("{}GiB", size.disk_gibibytes) },
            "eth0": { "type": "nic", "network": MAAS_NETWORK },
        },
    })
}

fn machine_instance(settings: &LabSettings, machine: &MaasMachine) -> serde_json::Value {
    json!({
        "name": machine.hostname,
        "type": "virtual-machine",
        "source": { "type": "none" },
        "config": {
            "limits.cpu": MACHINE_CPU_COUNT.to_string(),
            "limits.memory": format!("{MACHINE_MEMORY_MEBIBYTES}MiB"),
            "boot.mode": "uefi-secureboot",
        },
        "devices": {
            "root": {
                "type": "disk",
                "path": "/",
                "pool": settings.storage_pool,
                "size": format!("{MACHINE_DISK_GIBIBYTES}GiB"),
                "boot.priority": "10",
            },
            "eth0": { "type": "nic", "network": MAAS_NETWORK, "hwaddr": machine.mac.to_string(), "boot.priority": "5" },
            "vtpm": { "type": "tpm", "path": "/dev/tpm0" },
        },
    })
}

struct TrustedClient {
    certificate_pem: String,
    key_pem: String,
}

fn maas_client_identity() -> anyhow::Result<TrustedClient> {
    let key = KeyPair::generate()?;
    let certificate = CertificateParams::new(vec![String::from("maas")])?.self_signed(&key)?;
    Ok(TrustedClient { certificate_pem: certificate.pem(), key_pem: key.serialize_pem() })
}

fn pem_body(pem: &str) -> String {
    pem.lines().filter(|line| !line.starts_with("-----")).collect()
}

async fn trust_maas(lxd: &LxdClient, identity: &TrustedClient) -> anyhow::Result<String> {
    let body = json!({
        "type": "client",
        "name": "amafu-maas",
        "certificate": pem_body(&identity.certificate_pem),
        "restricted": true,
        "projects": [MAAS_PROJECT],
    });
    lxd.in_project("default").request(Method::POST, "/1.0/certificates", Some(body)).await?;
    let der = rustls_pemfile::certs(&mut identity.certificate_pem.as_bytes())
        .next()
        .context("the MAAS certificate is empty")??;
    Ok(hex::encode(Sha256::digest(der.as_ref())))
}

async fn maas_command(lxd: &LxdClient, command: &str) -> anyhow::Result<String> {
    let output = lxd.execute(MAAS_INSTANCE, &format!("maas admin {command}")).await?;
    if !output.succeeded() {
        bail!("maas admin {command} failed: {}{}", output.standard_output, output.standard_error);
    }
    Ok(output.standard_output)
}

async fn configure_dhcp(lxd: &LxdClient) -> anyhow::Result<()> {
    let (start, end) = DYNAMIC_RANGE;
    let rack = maas_command(lxd, "rack-controllers read").await?;
    let rack: serde_json::Value = serde_json::from_str(&rack)?;
    let rack_id = rack[0]["system_id"].as_str().context("MAAS has no rack controller")?;
    maas_command(lxd, &format!("ipranges create type=dynamic start_ip={start} end_ip={end}")).await?;
    maas_command(lxd, &format!("vlan update fabric-0 untagged dhcp_on=True primary_rack={rack_id}")).await?;
    Ok(())
}

async fn enlist(
    lxd: &LxdClient,
    settings: &LabSettings,
    machine: &MaasMachine,
    identity: &TrustedClient,
) -> anyhow::Result<()> {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let command = format!(
        "machines create architecture=amd64/generic hostname={} mac_addresses={} power_type=lxd power_parameters_power_address={} power_parameters_instance_name={} power_parameters_project={MAAS_PROJECT} power_parameters_certificate={} power_parameters_key={}",
        machine.hostname,
        machine.mac,
        quote(&settings.lxd_endpoint),
        machine.hostname,
        quote(&identity.certificate_pem),
        quote(&identity.key_pem),
    );
    maas_command(lxd, &command).await?;
    Ok(())
}

async fn wait_until_ready(lxd: &LxdClient, machines: &[MaasMachine]) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        let listed: serde_json::Value = serde_json::from_str(&maas_command(lxd, "machines read").await?)?;
        let statuses: Vec<(String, String)> = listed
            .as_array()
            .into_iter()
            .flatten()
            .map(|machine| {
                (
                    machine["hostname"].as_str().unwrap_or_default().to_owned(),
                    machine["status_name"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let ready = machines.iter().all(|machine| {
            statuses.iter().any(|(hostname, status)| *hostname == machine.hostname && status == "Ready")
        });
        if ready {
            return Ok(());
        }
        if let Some((hostname, status)) = statuses.iter().find(|(_, status)| status.starts_with("Failed")) {
            bail!("MAAS could not commission {hostname}: {status}");
        }
        if started.elapsed() >= COMMISSION_TIMEOUT {
            bail!("the machines were not commissioned in time: {statuses:?}");
        }
        tokio::time::sleep(POLL).await;
    }
}

fn install_description(
    machines: &[MaasMachine],
    image: amafu_domain::node_image::NodeImage,
    boot_loader: BootLoader,
    image_key: &str,
    api_key: &str,
) -> InstallDescription {
    let sections = machines
        .iter()
        .map(|machine| {
            let mut roles = std::collections::BTreeSet::from([NodeRole::Compute]);
            if machine.is_core {
                roles.insert(NodeRole::Core);
            }
            MachineSection {
                hostname: machine.hostname.clone(),
                node: machine.node.clone(),
                roles,
                zone: None,
                underlay: UnderlayConfiguration {
                    interface: None,
                    mac: Some(machine.mac),
                    address: StaticAddress { address: machine.address, prefix_length: PREFIX_LENGTH },
                    gateway: Some(GATEWAY),
                    mtu: None,
                },
                management: None,
            }
        })
        .collect();
    InstallDescription {
        version: amafu_installer::description::SUPPORTED_VERSION,
        cluster: ClusterSection {
            name: String::from("maas-lab"),
            image,
            boot_loader,
            image_keys: vec![image_key.to_owned()],
            resolvers: vec![GATEWAY],
            confirm_after_seconds: 30,
        },
        maas: MaasSection {
            url: format!("http://{MAAS_ADDRESS}:5240/MAAS"),
            api_key: api_key.to_owned(),
            certificate_authority: None,
        },
        machines: sections,
    }
}

fn command_line_binary(repository: &Path) -> anyhow::Result<PathBuf> {
    ["target/release/amafu", "target/debug/amafu"]
        .into_iter()
        .map(|candidate| repository.join(candidate))
        .find(|candidate| candidate.is_file())
        .context("build the amafu command first: cargo build --release -p amafu-command-line")
}

pub async fn up(repository: &Path, settings: &LabSettings, node_count: u8) -> anyhow::Result<MaasLabState> {
    let image = repository.join(&settings.image_directory);
    if !image.join("esp").is_dir() {
        bail!("no built image in {}; run image/build test first", image.display());
    }
    let lxd = lxd_client(settings)?.in_project(MAAS_PROJECT);
    ensure_project_and_network(&lxd).await?;
    let password = amafu_api::authentication::generate_token();
    lxd.request(Method::POST, "/1.0/instances", Some(maas_instance(settings, &password))).await?;
    lxd.change_state(MAAS_INSTANCE, "start", false).await?;
    let waited =
        wait_for_outcome(&lxd, MAAS_INSTANCE, READY_MARKER, FAILED_MARKER, SETUP_LOG, SETUP_TIMEOUT).await?;
    tracing::info!(seconds = waited.as_secs(), "MAAS is set up");
    configure_dhcp(&lxd).await?;
    let identity = maas_client_identity()?;
    let trust_fingerprint = trust_maas(&lxd, &identity).await?;
    let machines = machines(node_count);
    for machine in &machines {
        lxd.request(Method::POST, "/1.0/instances", Some(machine_instance(settings, machine))).await?;
        enlist(&lxd, settings, machine, &identity).await?;
    }
    wait_until_ready(&lxd, &machines).await?;
    tracing::info!("every machine is commissioned");

    let (signing_key, signing_document) = ImageSigningKey::generate()?;
    let prepared = prepare(&image, &repository.join("target/rig/maas-images"), MAAS_ADDRESS, &signing_key)?;
    crate::node_images::publish(&lxd, MAAS_INSTANCE, &prepared).await?;
    let boot_loader_bytes = std::fs::read(image.join("boot-loader.tar"))?;
    let boot_loader = BootLoader {
        url: crate::node_images::image_url(MAAS_ADDRESS, "boot-loader.tar"),
        digest: hex::encode(Sha256::digest(&boot_loader_bytes)).parse()?,
    };
    let api_key = crate::machines::read_file(&lxd, MAAS_INSTANCE, "/root/maas-api-key").await?;
    let description = install_description(
        &machines,
        prepared.images.installed.clone(),
        boot_loader,
        &signing_key.public_key_hexadecimal(),
        api_key.trim(),
    );
    lxd.push_file(MAAS_INSTANCE, INSTALL_DESCRIPTION, serde_yaml_ng::to_string(&description)?.into_bytes())
        .await?;
    let binary = command_line_binary(repository)?;
    lxd.push_file(MAAS_INSTANCE, CLI_PATH, std::fs::read(&binary)?).await?;
    lxd.execute(MAAS_INSTANCE, &format!("chmod 0755 {CLI_PATH}")).await?;
    let state =
        MaasLabState { machines, trust_fingerprint, image_signing_key: hex::encode(signing_document) };
    std::fs::write(state_file(repository), serde_json::to_vec_pretty(&state)?)?;
    Ok(state)
}

pub async fn install(settings: &LabSettings) -> anyhow::Result<Duration> {
    let lxd = lxd_client(settings)?.in_project(MAAS_PROJECT);
    let started = Instant::now();
    let command = format!(
        "rm -f {INSTALL_DONE}; nohup sh -c '{CLI_PATH} install --description {INSTALL_DESCRIPTION} --state-directory {INSTALL_DIRECTORY} > {INSTALL_LOG} 2>&1; echo $? > {INSTALL_DONE}' > /dev/null 2>&1 &"
    );
    lxd.execute(MAAS_INSTANCE, &command).await?;
    loop {
        let output = lxd.execute(MAAS_INSTANCE, &format!("cat {INSTALL_DONE} 2>/dev/null")).await?;
        match output.standard_output.trim() {
            "0" => return Ok(started.elapsed()),
            "" => {}
            code => {
                let log =
                    crate::machines::read_file(&lxd, MAAS_INSTANCE, INSTALL_LOG).await.unwrap_or_default();
                bail!("amafu install ended with {code}:\n{log}");
            }
        }
        if started.elapsed() >= INSTALL_TIMEOUT {
            bail!("the install did not finish within {} seconds", INSTALL_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL).await;
    }
}

pub async fn down(repository: &Path, settings: &LabSettings) -> anyhow::Result<()> {
    let lxd = lxd_client(settings)?.in_project(MAAS_PROJECT);
    let default = lxd.in_project("default");
    if default.exists(&format!("/1.0/projects/{MAAS_PROJECT}")).await? {
        let instances = lxd.request(Method::GET, "/1.0/instances", None).await?;
        for url in instances.as_array().into_iter().flatten().filter_map(|value| value.as_str()) {
            let name =
                url.rsplit('/').next().unwrap_or_default().split('?').next().unwrap_or_default().to_owned();
            let _ = lxd.change_state(&name, "stop", true).await;
            lxd.request(Method::DELETE, &format!("/1.0/instances/{name}"), None).await?;
        }
        default.request(Method::DELETE, &format!("/1.0/projects/{MAAS_PROJECT}"), None).await?;
    }
    if let Ok(contents) = std::fs::read(state_file(repository))
        && let Ok(state) = serde_json::from_slice::<MaasLabState>(&contents)
    {
        let _ = default
            .request(Method::DELETE, &format!("/1.0/certificates/{}", state.trust_fingerprint), None)
            .await;
    }
    if default.exists(&format!("/1.0/networks/{MAAS_NETWORK}")).await? {
        default.request(Method::DELETE, &format!("/1.0/networks/{MAAS_NETWORK}"), None).await?;
    }
    let _ = std::fs::remove_file(state_file(repository));
    Ok(())
}
