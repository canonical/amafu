use serde::Deserialize;

use amafu_storage_plugin_kit::StorageProvider;
use amafu_storage_plugin_kit::http::{self, Request};
use amafu_storage_plugin_kit::secrets;
use amafu_storage_plugin_kit::types::{FencedNode, ProviderError, ProvisionedVolume, VolumeRequest};

const OVERSIZED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Behaviour {
    LoopForever,
    AllocateWithoutBound,
    CallUndeclaredEndpoint,
    ReadForeignSecret,
    ReturnOversizedResult,
    ReturnMalformedResult,
    Panic,
}

#[derive(Deserialize)]
struct Configuration {
    behaviour: Behaviour,
    #[serde(default)]
    undeclared_endpoint: String,
    #[serde(default)]
    foreign_secret: String,
}

fn configuration(text: &str) -> Result<Configuration, ProviderError> {
    serde_json::from_str(text).map_err(|error| ProviderError::Invalid(error.to_string()))
}

fn loop_forever() -> u64 {
    let mut counter: u64 = 0;
    loop {
        counter = counter.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        if counter == u64::MAX {
            return counter;
        }
    }
}

fn allocate_without_bound() -> usize {
    let mut kept: Vec<Vec<u8>> = Vec::new();
    loop {
        kept.push(vec![1; 1024 * 1024]);
        if kept.len() == usize::MAX {
            return kept.len();
        }
    }
}

fn misbehave(configuration: &Configuration) -> Result<ProvisionedVolume, ProviderError> {
    match configuration.behaviour {
        Behaviour::LoopForever => Err(ProviderError::Retryable(loop_forever().to_string())),
        Behaviour::AllocateWithoutBound => {
            Err(ProviderError::Retryable(allocate_without_bound().to_string()))
        }
        Behaviour::CallUndeclaredEndpoint => {
            let request = Request {
                method: String::from("GET"),
                url: configuration.undeclared_endpoint.clone(),
                headers: Vec::new(),
                body: Vec::new(),
            };
            match http::send(&request) {
                Ok(response) => Ok(ProvisionedVolume {
                    identifier: format!("reached-{}", response.status),
                    size_in_bytes: 0,
                }),
                Err(error) => Err(ProviderError::Invalid(format!("{error:?}"))),
            }
        }
        Behaviour::ReadForeignSecret => match secrets::get(&configuration.foreign_secret) {
            Some(secret) => {
                Ok(ProvisionedVolume { identifier: format!("stolen-{secret}"), size_in_bytes: 0 })
            }
            None => Err(ProviderError::Invalid(String::from("the secret was withheld"))),
        },
        Behaviour::ReturnOversizedResult => {
            Ok(ProvisionedVolume { identifier: "x".repeat(OVERSIZED_BYTES), size_in_bytes: 1 })
        }
        Behaviour::ReturnMalformedResult => {
            Ok(ProvisionedVolume { identifier: String::from("bad\nname\0"), size_in_bytes: 0 })
        }
        Behaviour::Panic => panic!("the hostile plugin panics"),
    }
}

struct HostilePlugin;

impl StorageProvider for HostilePlugin {
    fn check(_configuration: String) -> Result<(), ProviderError> {
        Ok(())
    }

    fn create_volume(
        configuration: String,
        _request: VolumeRequest,
    ) -> Result<ProvisionedVolume, ProviderError> {
        misbehave(&self::configuration(&configuration)?)
    }

    fn delete_volume(_configuration: String, _volume: ProvisionedVolume) -> Result<(), ProviderError> {
        Ok(())
    }

    fn export_to_node(
        configuration: String,
        _volume: ProvisionedVolume,
        _node_name: String,
    ) -> Result<String, ProviderError> {
        let configuration = self::configuration(&configuration)?;
        match configuration.behaviour {
            Behaviour::ReturnOversizedResult => Ok("{".repeat(OVERSIZED_BYTES)),
            Behaviour::ReturnMalformedResult => Ok(String::from("{\"type\": \"teleport\"}")),
            _ => misbehave(&configuration).map(|volume| volume.identifier),
        }
    }

    fn revoke_node(_configuration: String, _node: FencedNode) -> Result<(), ProviderError> {
        Ok(())
    }

    fn has_revoked_node(_configuration: String, _node: FencedNode) -> Result<bool, ProviderError> {
        Ok(true)
    }

    fn readmit_node(_configuration: String, _node: FencedNode) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[allow(unsafe_code)]
mod abi {
    use super::HostilePlugin;
    amafu_storage_plugin_kit::export_storage_plugin!(HostilePlugin);
}
