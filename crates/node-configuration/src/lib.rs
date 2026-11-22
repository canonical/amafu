use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::Duration;

use amafu_api::TokenIdentity;
use amafu_controllers::environments::juju::JujuSettings;
use amafu_controllers::environments::kubernetes::{IssuerTrust, KubernetesSettings};
use amafu_controllers::{BootSlotsSettings, SchedulingSettings};
use amafu_domain::kinds::node::NodeRole;
use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::platform_access::{ISSUER, JWKS_PATH};
use amafu_domain::{NodeName, ZoneName};
use amafu_interfaces::{ExternalTool, TrustedImageKeys};
use amafu_network::UnderlayConfiguration;
use serde::{Deserialize, Serialize};

pub const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfiguration {
    pub version: u32,
    pub node: NodeSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<UnderlayConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management: Option<UnderlayConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentitySection>,
    pub api: ApiSection,
    #[serde(default)]
    pub state: StateSection,
    #[serde(default)]
    pub directories: Directories,
    #[serde(default)]
    pub compute: ComputeSection,
    #[serde(default)]
    pub fencing: FencingSection,
    #[serde(default)]
    pub scheduling: SchedulingSection,
    #[serde(default)]
    pub upgrade: UpgradeSection,
    #[serde(default)]
    pub environments: EnvironmentsSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticsSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EnvironmentsSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_address: Option<String>,
}

impl EnvironmentsSection {
    fn address(&self, api: &ApiSection, underlay: Option<Ipv4Addr>) -> String {
        if let Some(address) = &self.api_address {
            return address.clone();
        }
        match (api.listen.ip(), underlay) {
            (IpAddr::V4(listen), Some(underlay)) if listen.is_unspecified() => {
                SocketAddr::from((underlay, api.listen.port())).to_string()
            }
            _ => api.listen.to_string(),
        }
    }

    #[must_use]
    pub fn juju(
        &self,
        api: &ApiSection,
        underlay: Option<Ipv4Addr>,
        certificate_authority: &str,
    ) -> JujuSettings {
        JujuSettings {
            token_refresh_url: format!("https://{}{JWKS_PATH}", self.address(api, underlay)),
            amafu_certificate_authority: certificate_authority.to_owned(),
        }
    }

    #[must_use]
    pub fn kubernetes(
        &self,
        api: &ApiSection,
        underlay: Option<Ipv4Addr>,
        certificate_authority: &str,
    ) -> KubernetesSettings {
        let address = self.address(api, underlay);
        KubernetesSettings {
            trust: IssuerTrust {
                issuer: ISSUER.to_owned(),
                discovery_url: format!("https://{address}/.well-known/openid-configuration"),
                certificate_authority: certificate_authority.to_owned(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpgradeSection {
    pub image_keys: Vec<String>,
    pub confirm_after_seconds: u64,
    pub give_up_after_seconds: u64,
}

impl Default for UpgradeSection {
    fn default() -> Self {
        let defaults = BootSlotsSettings::default();
        Self {
            image_keys: Vec::new(),
            confirm_after_seconds: defaults.confirm_after.as_secs(),
            give_up_after_seconds: defaults.give_up_after.as_secs(),
        }
    }
}

impl UpgradeSection {
    #[must_use]
    pub fn boot_slots(&self) -> BootSlotsSettings {
        BootSlotsSettings {
            confirm_after: Duration::from_secs(self.confirm_after_seconds),
            give_up_after: Duration::from_secs(self.give_up_after_seconds),
        }
    }

    pub fn trusted_keys(&self) -> Result<TrustedImageKeys, ConfigurationError> {
        TrustedImageKeys::from_hexadecimal(self.image_keys.iter().map(String::as_str))
            .map_err(|error| ConfigurationError::ImageKeys(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ComputeSection {
    pub firmware: PathBuf,
    pub container_kernel: PathBuf,
    pub container_initramfs: PathBuf,
    pub nft: PathBuf,
    pub mkfs_erofs: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_libraries: Option<PathBuf>,
    pub resolvers: Vec<Ipv4Addr>,
}

impl ComputeSection {
    fn tool(&self, program: &Path) -> ExternalTool {
        ExternalTool { program: program.to_path_buf(), library_directory: self.tool_libraries.clone() }
    }

    #[must_use]
    pub fn nft_tool(&self) -> ExternalTool {
        self.tool(&self.nft)
    }

    #[must_use]
    pub fn mkfs_erofs_tool(&self) -> ExternalTool {
        self.tool(&self.mkfs_erofs)
    }
}

impl Default for ComputeSection {
    fn default() -> Self {
        Self {
            firmware: PathBuf::from("/run/amafu/boot/amafu/CLOUDHV.fd"),
            container_kernel: PathBuf::from("/run/amafu/boot/amafu/vmlinux"),
            container_initramfs: PathBuf::from("/run/amafu/boot/amafu/container-initramfs"),
            nft: PathBuf::from("/run/amafu/boot/amafu/tools/nft"),
            mkfs_erofs: PathBuf::from("/run/amafu/boot/amafu/tools/mkfs.erofs"),
            tool_libraries: Some(PathBuf::from("/run/amafu/boot/amafu/tools/lib")),
            resolvers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSection {
    pub name: NodeName,
    pub roles: BTreeSet<NodeRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<ZoneName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSection {
    pub listen: SocketAddr,
    pub certificate: String,
    pub private_key: String,
    pub identities: Vec<TokenIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySection {
    pub certificate: String,
    pub private_key: String,
    pub certificate_authority: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSection {
    pub listen: SocketAddr,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voters: Vec<SocketAddr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub core_members: Vec<String>,
    #[serde(default = "default_heartbeat_milliseconds")]
    pub heartbeat_milliseconds: u64,
}

const DEFAULT_HEARTBEAT_MILLISECONDS: u64 = 500;

fn default_heartbeat_milliseconds() -> u64 {
    DEFAULT_HEARTBEAT_MILLISECONDS
}

impl StateSection {
    #[must_use]
    pub fn heartbeat(&self) -> Duration {
        Duration::from_millis(self.heartbeat_milliseconds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FencingSection {
    pub lease_seconds: u64,
    pub watchdog_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulingSection {
    pub cpu_overcommit_ratio: u32,
    pub reserved_memory_mebibytes: u64,
}

impl Default for SchedulingSection {
    fn default() -> Self {
        let defaults = SchedulingSettings::default();
        Self {
            cpu_overcommit_ratio: defaults.cpu_overcommit_ratio,
            reserved_memory_mebibytes: defaults.reserved_memory_mebibytes,
        }
    }
}

impl SchedulingSection {
    #[must_use]
    pub fn settings(&self) -> SchedulingSettings {
        SchedulingSettings {
            cpu_overcommit_ratio: self.cpu_overcommit_ratio,
            reserved_memory_mebibytes: self.reserved_memory_mebibytes,
        }
    }
}

impl Default for FencingSection {
    fn default() -> Self {
        Self { lease_seconds: 8, watchdog_seconds: 8 }
    }
}

impl FencingSection {
    #[must_use]
    pub fn terms(&self) -> LeaseTerms {
        LeaseTerms::new(Duration::from_secs(self.lease_seconds), Duration::from_secs(self.watchdog_seconds))
    }
}

impl Default for StateSection {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 7400)),
            voters: Vec::new(),
            core_members: Vec::new(),
            heartbeat_milliseconds: DEFAULT_HEARTBEAT_MILLISECONDS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsSection {
    pub listen: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Directories {
    pub data: PathBuf,
    pub runtime: PathBuf,
    pub images: PathBuf,
}

const IMAGE_CACHE_DIRECTORY: &str = "image-cache";
const VOLUMES_DIRECTORY: &str = "volumes";
const PLUGIN_CACHE_DIRECTORY: &str = "plugin-cache";

impl Directories {
    #[must_use]
    pub fn image_cache(&self) -> PathBuf {
        self.data.join(IMAGE_CACHE_DIRECTORY)
    }

    #[must_use]
    pub fn plugin_cache(&self) -> PathBuf {
        self.data.join(PLUGIN_CACHE_DIRECTORY)
    }

    #[must_use]
    pub fn volumes(&self) -> PathBuf {
        self.data.join(VOLUMES_DIRECTORY)
    }
}

impl Default for Directories {
    fn default() -> Self {
        Self {
            data: PathBuf::from("/var/lib/amafu"),
            runtime: PathBuf::from("/run/amafu"),
            images: PathBuf::from("/usr/lib/amafu/images"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("the node configuration {path} cannot be read: {cause}")]
    Read { path: String, cause: std::io::Error },
    #[error("the node configuration is malformed: {0}")]
    Malformed(#[from] serde_yaml_ng::Error),
    #[error("node configuration version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("the node must have at least one role")]
    NoRole,
    #[error("a node outside the core needs an identity and at least one core member to reach")]
    NoWayToTheCore,
    #[error("several voters need an identity to authenticate each other")]
    VotersWithoutIdentity,
    #[error("the lease and the watchdog interval must each be between 2 and 60 seconds")]
    FencingOutOfBounds,
    #[error("an image key is malformed: {0}")]
    ImageKeys(String),
    #[error("a new boot slot must be given up later than it is confirmed")]
    ConfirmationAfterGivingUp,
}

impl NodeConfiguration {
    #[must_use]
    pub fn platform_api(&self) -> Vec<SocketAddrV4> {
        let port = self.api.listen.port();
        let voters = self.state.voters.iter().filter_map(|voter| match voter.ip() {
            IpAddr::V4(address) => Some(SocketAddrV4::new(address, port)),
            IpAddr::V6(_) => None,
        });
        let members = self.state.core_members.iter().filter_map(|member| {
            member.strip_prefix("https://")?.trim_end_matches('/').parse::<SocketAddrV4>().ok()
        });
        let mut addresses: Vec<SocketAddrV4> = voters.chain(members).collect();
        addresses.sort();
        addresses.dedup();
        addresses
    }

    pub fn parse(contents: &str) -> Result<Self, ConfigurationError> {
        let configuration: Self = serde_yaml_ng::from_str(contents)?;
        if configuration.version != SUPPORTED_VERSION {
            return Err(ConfigurationError::UnsupportedVersion(configuration.version));
        }
        if configuration.node.roles.is_empty() {
            return Err(ConfigurationError::NoRole);
        }
        let is_core = configuration.has_role(NodeRole::Core);
        if !is_core && (configuration.identity.is_none() || configuration.state.core_members.is_empty()) {
            return Err(ConfigurationError::NoWayToTheCore);
        }
        if is_core && configuration.state.voters.len() > 1 && configuration.identity.is_none() {
            return Err(ConfigurationError::VotersWithoutIdentity);
        }
        if !configuration.fencing.terms().is_within_bounds() {
            return Err(ConfigurationError::FencingOutOfBounds);
        }
        configuration.upgrade.trusted_keys()?;
        if configuration.upgrade.give_up_after_seconds <= configuration.upgrade.confirm_after_seconds {
            return Err(ConfigurationError::ConfirmationAfterGivingUp);
        }
        Ok(configuration)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigurationError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|cause| ConfigurationError::Read { path: path.display().to_string(), cause })?;
        Self::parse(&contents)
    }

    #[must_use]
    pub fn has_role(&self, role: NodeRole) -> bool {
        self.node.roles.contains(&role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_api_is_every_core_members_api_address() {
        let mut configuration = NodeConfiguration::parse(MINIMAL).unwrap();
        configuration.api.listen = "0.0.0.0:7443".parse().unwrap();
        configuration.state.voters = vec!["10.0.0.2:7445".parse().unwrap(), "10.0.0.1:7445".parse().unwrap()];
        configuration.state.core_members =
            vec![String::from("https://10.0.0.3:7443/"), String::from("https://core.lab:7443")];
        let expected: Vec<SocketAddrV4> = ["10.0.0.1:7443", "10.0.0.2:7443", "10.0.0.3:7443"]
            .iter()
            .map(|address| address.parse().unwrap())
            .collect();
        assert_eq!(configuration.platform_api(), expected);
    }

    #[test]
    fn guests_are_told_a_reachable_api_address() {
        let api = |listen: &str| ApiSection {
            listen: listen.parse().unwrap(),
            certificate: String::new(),
            private_key: String::new(),
            identities: Vec::new(),
        };
        let underlay = Some(Ipv4Addr::new(10, 20, 0, 11));
        let unset = EnvironmentsSection::default();
        assert_eq!(unset.address(&api("0.0.0.0:7443"), underlay), "10.20.0.11:7443");
        assert_eq!(unset.address(&api("10.20.0.12:7443"), underlay), "10.20.0.12:7443");
        let configured = EnvironmentsSection { api_address: Some(String::from("api.lab:7443")) };
        assert_eq!(configured.address(&api("0.0.0.0:7443"), underlay), "api.lab:7443");
    }

    const MINIMAL: &str = "
version: 1
node:
  name: node-0
  roles: [core, compute]
api:
  listen: 0.0.0.0:7443
  certificate: certificate
  private_key: key
  identities: []
";

    #[test]
    fn a_minimal_configuration_uses_default_paths() {
        let configuration = NodeConfiguration::parse(MINIMAL).unwrap();
        assert_eq!(configuration.directories, Directories::default());
        assert_eq!(configuration.state, StateSection::default());
        assert!(configuration.has_role(NodeRole::Compute));
        assert_eq!(configuration.network, None);
        assert_eq!(configuration.compute, ComputeSection::default());
        assert_eq!(configuration.fencing.terms().duration(), Duration::from_secs(8));
    }

    #[test]
    fn image_keys_must_be_ed25519_and_confirmation_must_come_before_giving_up() {
        let malformed = format!("{MINIMAL}upgrade:\n  image_keys: [abcd]\n");
        assert!(matches!(NodeConfiguration::parse(&malformed), Err(ConfigurationError::ImageKeys(_))));
        let key = "ab".repeat(32);
        let trusted = format!(
            "{MINIMAL}upgrade:\n  image_keys: [{key}]\n  confirm_after_seconds: 20\n  give_up_after_seconds: 300\n"
        );
        let configuration = NodeConfiguration::parse(&trusted).unwrap();
        assert_eq!(configuration.upgrade.boot_slots().confirm_after, Duration::from_secs(20));
        let impatient =
            format!("{MINIMAL}upgrade:\n  confirm_after_seconds: 300\n  give_up_after_seconds: 20\n");
        assert!(matches!(
            NodeConfiguration::parse(&impatient),
            Err(ConfigurationError::ConfirmationAfterGivingUp)
        ));
    }

    #[test]
    fn fencing_outside_the_bounds_is_refused() {
        let hasty = format!("{MINIMAL}fencing:\n  lease_seconds: 1\n  watchdog_seconds: 8\n");
        assert!(matches!(NodeConfiguration::parse(&hasty), Err(ConfigurationError::FencingOutOfBounds)));
    }

    #[test]
    fn unknown_versions_and_fields_are_refused() {
        assert!(matches!(
            NodeConfiguration::parse(&MINIMAL.replace("version: 1", "version: 2")),
            Err(ConfigurationError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            NodeConfiguration::parse(&format!("{MINIMAL}surprise: true\n")),
            Err(ConfigurationError::Malformed(_))
        ));
        assert!(matches!(
            NodeConfiguration::parse(&MINIMAL.replace("[core, compute]", "[]")),
            Err(ConfigurationError::NoRole)
        ));
    }

    #[test]
    fn a_node_outside_the_core_must_know_how_to_reach_it() {
        let compute_only = MINIMAL.replace("[core, compute]", "[compute]");
        assert!(matches!(NodeConfiguration::parse(&compute_only), Err(ConfigurationError::NoWayToTheCore)));
        let reachable = format!(
            "{compute_only}identity:\n  certificate: c\n  private_key: k\n  certificate_authority: a\nstate:\n  listen: 127.0.0.1:7400\n  core_members: [\"https://10.0.0.1:7443\"]\n"
        );
        assert!(NodeConfiguration::parse(&reachable).is_ok());
        let voters = format!(
            "{MINIMAL}state:\n  listen: 127.0.0.1:7400\n  voters: [10.0.0.1:7400, 10.0.0.2:7400, 10.0.0.3:7400]\n"
        );
        assert!(matches!(NodeConfiguration::parse(&voters), Err(ConfigurationError::VotersWithoutIdentity)));
    }
}
