use std::sync::Arc;

use amafu_domain::NodeName;
use amafu_domain::kinds::storage_backend::PluginBackend;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_interfaces::{ActionError, AttachTarget, FencedNode, StorageProvider, VolumeRequest};

use super::bindings::amafu::storage::types;
use super::host::{Answer, Call, PluginHost};

const LONGEST_IDENTIFIER: usize = 256;
const LONGEST_TARGET: usize = 64 * 1024;

pub struct WebAssemblyProvider {
    host: Arc<PluginHost>,
    backend: PluginBackend,
}

fn is_identifier(identifier: &str) -> bool {
    (1..=LONGEST_IDENTIFIER).contains(&identifier.len())
        && identifier.chars().all(|character| character.is_ascii_alphanumeric() || "-_.:".contains(character))
}

fn unexpected() -> ActionError {
    ActionError::invalid("the plugin answered something else than was asked")
}

fn fenced(node: &FencedNode) -> types::FencedNode {
    types::FencedNode {
        name: node.name.to_string(),
        address: node.address.map(|address| address.to_string()),
    }
}

fn plugin_volume(volume: &ProvisionedVolume) -> types::ProvisionedVolume {
    types::ProvisionedVolume { identifier: volume.identifier.clone(), size_in_bytes: volume.size_bytes }
}

impl WebAssemblyProvider {
    #[must_use]
    pub fn new(host: Arc<PluginHost>, backend: PluginBackend) -> Self {
        Self { host, backend }
    }

    async fn nothing(&self, call: Call) -> Result<(), ActionError> {
        match self.host.invoke(&self.backend, call).await? {
            Answer::Nothing => Ok(()),
            _ => Err(unexpected()),
        }
    }

    async fn flag(&self, call: Call) -> Result<bool, ActionError> {
        match self.host.invoke(&self.backend, call).await? {
            Answer::Flag(flag) => Ok(flag),
            _ => Err(unexpected()),
        }
    }
}

#[async_trait::async_trait]
impl StorageProvider for WebAssemblyProvider {
    async fn check(&self) -> Result<(), ActionError> {
        self.nothing(Call::Check).await
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        if request.source_image.is_some() {
            return Err(ActionError::invalid(
                "volumes on plugin backends cannot be cloned from an image yet",
            ));
        }
        let asked = types::VolumeRequest {
            volume_id: request.volume_id.to_string(),
            size_in_bytes: request.size_bytes,
        };
        let Answer::Volume(volume) = self.host.invoke(&self.backend, Call::CreateVolume(asked)).await? else {
            return Err(unexpected());
        };
        if !is_identifier(&volume.identifier) {
            return Err(ActionError::invalid(
                "the plugin named the volume with characters or a length it may not use",
            ));
        }
        if volume.size_in_bytes < request.size_bytes {
            return Err(ActionError::invalid("the plugin provisioned a smaller volume than asked"));
        }
        Ok(ProvisionedVolume { identifier: volume.identifier, size_bytes: volume.size_in_bytes, node: None })
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        self.nothing(Call::DeleteVolume(plugin_volume(volume))).await
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        let call = Call::ExportToNode(plugin_volume(volume), node.to_string());
        let Answer::Target(target) = self.host.invoke(&self.backend, call).await? else {
            return Err(unexpected());
        };
        if target.len() > LONGEST_TARGET {
            return Err(ActionError::invalid("the plugin's attach target is larger than 64 KiB"));
        }
        let target: AttachTarget = serde_json::from_str(&target).map_err(|error| {
            ActionError::invalid(format!("the plugin's attach target is malformed: {error}"))
        })?;
        if matches!(target, AttachTarget::LocalFile { .. }) {
            return Err(ActionError::invalid("a plugin cannot export a file on a node"));
        }
        Ok(target)
    }

    async fn revoke_node(&self, node: &FencedNode) -> Result<(), ActionError> {
        self.nothing(Call::RevokeNode(fenced(node))).await
    }

    async fn has_revoked_node(&self, node: &FencedNode) -> Result<bool, ActionError> {
        self.flag(Call::HasRevokedNode(fenced(node))).await
    }

    async fn readmit_node(&self, node: &FencedNode) -> Result<(), ActionError> {
        self.nothing(Call::ReadmitNode(fenced(node))).await
    }
}
