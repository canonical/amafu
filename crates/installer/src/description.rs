use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::Path;

use amafu_domain::kinds::node::NodeRole;
use amafu_domain::node_image::NodeImage;
use amafu_domain::{NodeName, Sha256Digest, ZoneName};
use amafu_network::UnderlayConfiguration;
use serde::{Deserialize, Serialize};

pub const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallDescription {
    pub version: u32,
    pub cluster: ClusterSection,
    pub maas: MaasSection,
    pub machines: Vec<MachineSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSection {
    pub name: String,
    pub image: NodeImage,
    pub boot_loader: BootLoader,
    pub image_keys: Vec<String>,
    #[serde(default)]
    pub resolvers: Vec<Ipv4Addr>,
    #[serde(default = "default_confirm_after_seconds")]
    pub confirm_after_seconds: u64,
}

fn default_confirm_after_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootLoader {
    pub url: String,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaasSection {
    pub url: String,
    pub api_key: String,
    #[serde(default)]
    pub certificate_authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSection {
    pub hostname: String,
    pub node: NodeName,
    pub roles: BTreeSet<NodeRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<ZoneName>,
    pub underlay: UnderlayConfiguration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management: Option<UnderlayConfiguration>,
}

impl MachineSection {
    #[must_use]
    pub fn is_core(&self) -> bool {
        self.roles.contains(&NodeRole::Core)
    }

    #[must_use]
    pub fn underlay_address(&self) -> Ipv4Addr {
        self.underlay.address.address
    }

    #[must_use]
    pub fn reachable_address(&self) -> Ipv4Addr {
        self.management
            .as_ref()
            .map_or_else(|| self.underlay_address(), |management| management.address.address)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DescriptionError {
    #[error("the install description cannot be read: {0}")]
    Read(#[from] std::io::Error),
    #[error("the install description is malformed: {0}")]
    Malformed(#[from] serde_yaml_ng::Error),
    #[error("install description version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("the cluster needs at least one core member")]
    NoCore,
    #[error("{0} is named more than once")]
    Duplicate(String),
    #[error("the MAAS API key must be three parts joined by colons")]
    MalformedApiKey,
}

impl InstallDescription {
    pub fn parse(contents: &str) -> Result<Self, DescriptionError> {
        let description: Self = serde_yaml_ng::from_str(contents)?;
        if description.version != SUPPORTED_VERSION {
            return Err(DescriptionError::UnsupportedVersion(description.version));
        }
        if !description.machines.iter().any(MachineSection::is_core) {
            return Err(DescriptionError::NoCore);
        }
        let mut seen = BTreeSet::new();
        for machine in &description.machines {
            for name in [machine.hostname.clone(), machine.node.to_string()] {
                if !seen.insert(name.clone()) {
                    return Err(DescriptionError::Duplicate(name));
                }
            }
        }
        if description.maas.api_key.split(':').count() != 3 {
            return Err(DescriptionError::MalformedApiKey);
        }
        Ok(description)
    }

    pub fn load(path: &Path) -> Result<Self, DescriptionError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn core_members(&self) -> impl Iterator<Item = &MachineSection> {
        self.machines.iter().filter(|machine| machine.is_core())
    }

    #[must_use]
    pub fn ordered_nodes(&self) -> Vec<NodeName> {
        let (core, others): (Vec<&MachineSection>, Vec<&MachineSection>) =
            self.machines.iter().partition(|machine| machine.is_core());
        core.into_iter().chain(others).map(|machine| machine.node.clone()).collect()
    }

    #[must_use]
    pub fn machine(&self, node: &NodeName) -> Option<&MachineSection> {
        self.machines.iter().find(|machine| machine.node == *node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = r#"
version: 1
cluster:
  name: lab
  image:
    version: 0.2.0
    feature_level: 1
    digest: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    size_bytes: 1024
    url: http://images.lab/node-image.tar
    signature: "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
  boot_loader:
    url: http://images.lab/boot-loader.tar
    digest: sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  image_keys: []
maas:
  url: http://maas.lab:5240/MAAS
  api_key: consumer:token:secret
machines:
  - hostname: amafu-1
    node: node-1
    roles: [compute]
    underlay: { address: 10.0.1.11/24 }
  - hostname: amafu-0
    node: node-0
    roles: [core, compute]
    underlay: { address: 10.0.1.10/24 }
"#;

    #[test]
    fn core_members_are_deployed_first() {
        let description = InstallDescription::parse(DESCRIPTION).unwrap();
        let order: Vec<String> = description.ordered_nodes().iter().map(ToString::to_string).collect();
        assert_eq!(order, vec!["node-0", "node-1"]);
    }

    #[test]
    fn a_cluster_without_core_or_with_duplicates_is_refused() {
        let without_core = DESCRIPTION.replace("[core, compute]", "[compute]");
        assert!(matches!(InstallDescription::parse(&without_core), Err(DescriptionError::NoCore)));
        let duplicate = DESCRIPTION.replace("hostname: amafu-0", "hostname: amafu-1");
        assert!(matches!(InstallDescription::parse(&duplicate), Err(DescriptionError::Duplicate(_))));
        let bad_key = DESCRIPTION.replace("consumer:token:secret", "secret");
        assert!(matches!(InstallDescription::parse(&bad_key), Err(DescriptionError::MalformedApiKey)));
    }
}
