use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use amafu_api::authentication::generate_token;
use amafu_api::{Principal, TokenIdentity};
use amafu_domain::access::Role;
use amafu_domain::kinds::node::NodeRole;
use amafu_domain::{NodeName, ProjectName};
use amafu_installer::certificates::CertificateAuthority;
use amafu_node::configuration::{
    ApiSection, ComputeSection, Directories, FencingSection, IdentitySection, NodeConfiguration, NodeSection,
    SUPPORTED_VERSION, SchedulingSection, StateSection, UpgradeSection,
};
use anyhow::{Context, bail};

pub const PROJECT: &str = "scale";
const FIRST_API_PORT: u16 = 27_443;
const FIRST_STATE_PORT: u16 = 27_400;
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
const READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct CoreMember {
    pub name: NodeName,
    pub api_endpoint: String,
    pub process: Child,
}

pub struct CoreCluster {
    pub authority: CertificateAuthority,
    pub operator_token: String,
    pub members: Vec<CoreMember>,
}

impl Drop for CoreCluster {
    fn drop(&mut self) {
        for member in &mut self.members {
            let _ = member.process.kill();
            let _ = member.process.wait();
        }
    }
}

fn state_address(index: u16) -> SocketAddr {
    SocketAddr::from((LOOPBACK, FIRST_STATE_PORT + index))
}

fn configuration(
    index: u16,
    count: u16,
    authority: &CertificateAuthority,
    identities: &[TokenIdentity],
    work: &Path,
    state: &Path,
) -> anyhow::Result<NodeConfiguration> {
    let name = NodeName::new(format!("core-{index}"))?;
    let identity = authority.issue_node(&name, &[NodeRole::Core])?;
    let server = authority.issue_server(name.as_str(), &[IpAddr::V4(LOOPBACK)])?;
    let directory = work.join(name.as_str());
    let data = state.join(name.as_str());
    Ok(NodeConfiguration {
        version: SUPPORTED_VERSION,
        node: NodeSection { name, roles: BTreeSet::from([NodeRole::Core]), zone: None },
        network: None,
        management: None,
        identity: Some(IdentitySection {
            certificate: identity.certificate_pem,
            private_key: identity.private_key_pem,
            certificate_authority: authority.certificate_pem.clone(),
        }),
        api: ApiSection {
            listen: SocketAddr::from((LOOPBACK, FIRST_API_PORT + index)),
            certificate: server.certificate_pem,
            private_key: server.private_key_pem,
            identities: identities.to_vec(),
        },
        state: StateSection {
            listen: state_address(index),
            voters: (0..count).map(state_address).collect(),
            core_members: Vec::new(),
            ..StateSection::default()
        },
        directories: Directories { data, runtime: directory.join("run"), images: directory.join("images") },
        compute: ComputeSection::default(),
        fencing: FencingSection::default(),
        scheduling: SchedulingSection { cpu_overcommit_ratio: 16, ..SchedulingSection::default() },
        upgrade: UpgradeSection::default(),
        diagnostics: None,
    })
}

async fn wait_until_answering(endpoint: &str, authority: &CertificateAuthority) -> anyhow::Result<()> {
    let transport = amafu_api::HttpTransport::new(endpoint, Some(authority.certificate_pem.as_bytes()))?;
    let client = amafu_api::ApiClient::new(std::sync::Arc::new(transport), None);
    let started = Instant::now();
    loop {
        match client.list::<amafu_domain::kinds::node::Node>().await {
            Err(amafu_api::ClientError::Transport(_)) if started.elapsed() < READY_TIMEOUT => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(amafu_api::ClientError::Transport(error)) => bail!("{endpoint} never answered: {error}"),
            _ => return Ok(()),
        }
    }
}

impl CoreCluster {
    pub async fn start(binary: &Path, work: &Path, state: &Path, count: u16) -> anyhow::Result<Self> {
        let authority = CertificateAuthority::generate("Amafu scale authority")?;
        let operator_token = generate_token();
        let principal = |role| Principal {
            name: format!("{role}-of-scale"),
            role,
            project: ProjectName::new(PROJECT).unwrap_or_else(|_| unreachable!()),
        };
        let identities = vec![TokenIdentity::for_token(principal(Role::Operator), &operator_token, None)];
        let mut members = Vec::new();
        for index in 0..count {
            let configuration = configuration(index, count, &authority, &identities, work, state)?;
            let directory = work.join(configuration.node.name.as_str());
            std::fs::create_dir_all(&directory)?;
            let path = directory.join("configuration.yaml");
            std::fs::write(&path, serde_yaml_ng::to_string(&configuration)?)?;
            let log = std::fs::File::create(directory.join("node.log"))?;
            let process = Command::new(binary)
                .arg("run")
                .arg("--configuration")
                .arg(&path)
                .env("RUST_LOG", "warn")
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()
                .with_context(|| format!("starting {}", binary.display()))?;
            members.push(CoreMember {
                name: configuration.node.name.clone(),
                api_endpoint: format!("https://{}", configuration.api.listen),
                process,
            });
        }
        let cluster = Self { authority, operator_token, members };
        for member in &cluster.members {
            wait_until_answering(&member.api_endpoint, &cluster.authority).await?;
        }
        Ok(cluster)
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        self.members.iter().map(|member| member.api_endpoint.clone()).collect()
    }

    #[must_use]
    pub fn resident_memory_mebibytes(&self) -> Vec<u64> {
        self.members.iter().map(|member| resident_memory_mebibytes(member.process.id())).collect()
    }
}

#[must_use]
pub fn resident_memory_mebibytes(process_id: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{process_id}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()
            })
        })
        .map_or(0, |kibibytes| kibibytes / 1024)
}

#[must_use]
pub fn available_memory_mebibytes() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo").ok()?.lines().find_map(|line| {
        Some(
            line.strip_prefix("MemAvailable:")?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()? / 1024,
        )
    })
}

#[must_use]
pub fn work_directory(root: &Path) -> PathBuf {
    root.join("target/rig/scale")
}
