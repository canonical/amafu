use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use amafu_api::TokenIdentity;
use amafu_domain::NodeName;
use amafu_domain::kinds::node::NodeRole;
use amafu_interfaces::ExternalTool;
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
    pub api: ApiSection,
    #[serde(default)]
    pub state: StateSection,
    #[serde(default)]
    pub directories: Directories,
    #[serde(default)]
    pub compute: ComputeSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticsSection>,
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
pub struct StateSection {
    pub listen: SocketAddr,
}

impl Default for StateSection {
    fn default() -> Self {
        Self { listen: SocketAddr::from(([127, 0, 0, 1], 7400)) }
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

impl Directories {
    #[must_use]
    pub fn image_cache(&self) -> PathBuf {
        self.data.join(IMAGE_CACHE_DIRECTORY)
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
}

impl NodeConfiguration {
    pub fn parse(contents: &str) -> Result<Self, ConfigurationError> {
        let configuration: Self = serde_yaml_ng::from_str(contents)?;
        if configuration.version != SUPPORTED_VERSION {
            return Err(ConfigurationError::UnsupportedVersion(configuration.version));
        }
        if configuration.node.roles.is_empty() {
            return Err(ConfigurationError::NoRole);
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
}
