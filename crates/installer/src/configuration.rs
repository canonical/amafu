use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::path::Path;

use amafu_api::authentication::generate_token;
use amafu_api::{Principal, TokenIdentity};
use amafu_domain::ProjectName;
use amafu_domain::access::Role;
use amafu_node::configuration::{
    ApiSection, ComputeSection, Directories, FencingSection, IdentitySection, NodeConfiguration, NodeSection,
    SUPPORTED_VERSION, SchedulingSection, StateSection, UpgradeSection,
};
use serde::{Deserialize, Serialize};

use crate::certificates::{CertificateAuthority, IssuanceError};
use crate::description::{InstallDescription, MachineSection};

pub const API_PORT: u16 = 7443;
pub const STATE_PORT: u16 = 7400;
pub const ADMIN_PROJECT: &str = "default";
const GIVE_UP_AFTER_CONFIRMATION_FACTOR: u64 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterCredentials {
    pub authority: CertificateAuthority,
    pub admin_token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error(transparent)]
    Issuance(#[from] IssuanceError),
    #[error("the cluster credentials cannot be kept: {0}")]
    Storage(#[from] std::io::Error),
    #[error("the cluster credentials are corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
    #[error("a name is invalid: {0}")]
    Name(#[from] amafu_domain::InvalidName),
}

impl ClusterCredentials {
    pub fn load_or_create(path: &Path, cluster_name: &str) -> Result<Self, CredentialsError> {
        match std::fs::read(path) {
            Ok(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            Err(_) => {}
        }
        let credentials = Self {
            authority: CertificateAuthority::generate(&format!("{cluster_name} cluster authority"))?,
            admin_token: generate_token(),
        };
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(path)?, &serde_json::to_vec_pretty(&credentials)?)?;
        Ok(credentials)
    }

    pub fn admin_identity(&self) -> Result<TokenIdentity, CredentialsError> {
        let principal = Principal {
            name: String::from("installer-admin"),
            role: Role::Admin,
            project: ProjectName::new(ADMIN_PROJECT)?,
        };
        Ok(TokenIdentity::for_token(principal, &self.admin_token, None))
    }
}

fn listen_everywhere(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], port))
}

fn state_address(machine: &MachineSection) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(machine.underlay_address(), STATE_PORT))
}

#[must_use]
pub fn api_endpoint(machine: &MachineSection) -> String {
    format!("https://{}:{API_PORT}", machine.reachable_address())
}

fn core_endpoint(machine: &MachineSection) -> String {
    format!("https://{}:{API_PORT}", machine.underlay_address())
}

pub fn node_configuration(
    description: &InstallDescription,
    machine: &MachineSection,
    credentials: &ClusterCredentials,
) -> Result<NodeConfiguration, CredentialsError> {
    let roles: Vec<_> = machine.roles.iter().copied().collect();
    let identity = credentials.authority.issue_node(&machine.node, &roles)?;
    let mut addresses = BTreeSet::from([IpAddr::V4(machine.underlay_address())]);
    addresses.insert(IpAddr::V4(machine.reachable_address()));
    let addresses: Vec<IpAddr> = addresses.into_iter().collect();
    let server = credentials.authority.issue_server(machine.node.as_str(), &addresses)?;
    let (voters, core_members) = if machine.is_core() {
        (description.core_members().map(state_address).collect(), Vec::new())
    } else {
        (Vec::new(), description.core_members().map(core_endpoint).collect())
    };
    let cluster = &description.cluster;
    Ok(NodeConfiguration {
        version: SUPPORTED_VERSION,
        node: NodeSection {
            name: machine.node.clone(),
            roles: machine.roles.clone(),
            zone: machine.zone.clone(),
        },
        network: Some(machine.underlay.clone()),
        management: machine.management.clone(),
        identity: Some(IdentitySection {
            certificate: identity.certificate_pem,
            private_key: identity.private_key_pem,
            certificate_authority: credentials.authority.certificate_pem.clone(),
        }),
        api: ApiSection {
            listen: listen_everywhere(API_PORT),
            certificate: server.certificate_pem,
            private_key: server.private_key_pem,
            identities: vec![credentials.admin_identity()?],
        },
        state: StateSection {
            listen: state_address(machine),
            voters,
            core_members,
            ..StateSection::default()
        },
        directories: Directories::default(),
        compute: ComputeSection { resolvers: cluster.resolvers.clone(), ..ComputeSection::default() },
        fencing: FencingSection::default(),
        scheduling: SchedulingSection::default(),
        upgrade: UpgradeSection {
            image_keys: cluster.image_keys.clone(),
            confirm_after_seconds: cluster.confirm_after_seconds,
            give_up_after_seconds: cluster.confirm_after_seconds * GIVE_UP_AFTER_CONFIRMATION_FACTOR,
        },
        diagnostics: None,
    })
}
