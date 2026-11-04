use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use amafu_storage_plugin_kit::StorageProvider;
use amafu_storage_plugin_kit::http::{self, Request};
use amafu_storage_plugin_kit::logging::{Level, log};
use amafu_storage_plugin_kit::secrets;
use amafu_storage_plugin_kit::types::{FencedNode, ProviderError, ProvisionedVolume, VolumeRequest};

const VOLUME_PREFIX: &str = "nqn.2026-11.io.amafu:volume:";
const HOST_PREFIX: &str = "nqn.2026-11.io.amafu:node:";
const ALREADY_EXISTS: i64 = -17;
const NOT_FOUND: i64 = -19;
const MEBIBYTE: u64 = 1024 * 1024;
const USER_SECRET: &str = "user";
const PASSWORD_SECRET: &str = "password";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    management: String,
    addresses: Vec<String>,
    pool: String,
}

struct Refusal {
    code: i64,
    message: String,
}

struct Target {
    configuration: Configuration,
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::Invalid(message.into())
}

fn retryable(message: impl Into<String>) -> ProviderError {
    ProviderError::Retryable(message.into())
}

fn volume_qualified_name(identifier: &str) -> String {
    format!("{VOLUME_PREFIX}{identifier}")
}

fn host_qualified_name(node: &str) -> String {
    format!("{HOST_PREFIX}{node}")
}

fn authorization() -> Option<String> {
    let user = secrets::get(USER_SECRET)?;
    let password = secrets::get(PASSWORD_SECRET).unwrap_or_default();
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    Some(format!("Basic {encoded}"))
}

fn is_tolerated(refusal: &Refusal, tolerated: &[i64]) -> bool {
    let message = refusal.message.to_ascii_lowercase();
    tolerated.contains(&refusal.code)
        || (tolerated.contains(&ALREADY_EXISTS) && message.contains("exist"))
        || (tolerated.contains(&NOT_FOUND) && (message.contains("not found") || message.contains("no such")))
}

fn allows_host(subsystem: &Value, host: &str) -> bool {
    subsystem["hosts"].as_array().into_iter().flatten().any(|entry| entry["nqn"].as_str() == Some(host))
}

impl Target {
    fn load(configuration: &str) -> Result<Self, ProviderError> {
        let configuration: Configuration = serde_json::from_str(configuration)
            .map_err(|error| invalid(format!("the configuration is malformed: {error}")))?;
        if configuration.addresses.is_empty() {
            return Err(invalid("the configuration names no target address"));
        }
        Ok(Self { configuration })
    }

    fn call(&self, method: &str, parameters: &Value) -> Result<Result<Value, Refusal>, ProviderError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": parameters});
        let mut headers = vec![(String::from("content-type"), String::from("application/json"))];
        if let Some(authorization) = authorization() {
            headers.push((String::from("authorization"), authorization));
        }
        let request = Request {
            method: String::from("POST"),
            url: self.configuration.management.clone(),
            headers,
            body: body.to_string().into_bytes(),
        };
        let response =
            http::send(&request).map_err(|error| retryable(format!("calling {method}: {error:?}")))?;
        let answer: Value = serde_json::from_slice(&response.body)
            .map_err(|error| retryable(format!("the answer to {method} is not JSON: {error}")))?;
        if let Some(error) = answer.get("error") {
            return Ok(Err(Refusal {
                code: error["code"].as_i64().unwrap_or_default(),
                message: error["message"].as_str().unwrap_or_default().to_owned(),
            }));
        }
        Ok(Ok(answer["result"].clone()))
    }

    fn call_tolerating(
        &self,
        method: &str,
        parameters: &Value,
        tolerated: &[i64],
    ) -> Result<(), ProviderError> {
        match self.call(method, parameters)? {
            Ok(_) => Ok(()),
            Err(refusal) if is_tolerated(&refusal, tolerated) => Ok(()),
            Err(refusal) => {
                Err(retryable(format!("the target refused {method}: {} ({})", refusal.message, refusal.code)))
            }
        }
    }

    fn answer(&self, method: &str) -> Result<Value, ProviderError> {
        self.call(method, &json!({}))?
            .map_err(|refusal| retryable(format!("the target refused {method}: {}", refusal.message)))
    }

    fn subsystems(&self) -> Result<Vec<Value>, ProviderError> {
        Ok(self.answer("nvmf_get_subsystems")?.as_array().cloned().unwrap_or_default())
    }

    fn subsystem(&self, qualified_name: &str) -> Result<Option<Value>, ProviderError> {
        Ok(self.subsystems()?.into_iter().find(|subsystem| subsystem["nqn"].as_str() == Some(qualified_name)))
    }

    fn bdev(&self, identifier: &str) -> String {
        format!("{}/{identifier}", self.configuration.pool)
    }

    fn listen_address(address: &str) -> Result<Value, ProviderError> {
        let (host, port) =
            address.rsplit_once(':').ok_or_else(|| invalid(format!("{address} has no port")))?;
        Ok(json!({"trtype": "tcp", "adrfam": "ipv4", "traddr": host, "trsvcid": port}))
    }
}

struct SpdkPlugin;

impl StorageProvider for SpdkPlugin {
    fn check(configuration: String) -> Result<(), ProviderError> {
        let target = Target::load(&configuration)?;
        let stores = target.answer("bdev_lvol_get_lvstores")?;
        let pool = &target.configuration.pool;
        if stores.as_array().into_iter().flatten().any(|store| store["name"].as_str() == Some(pool)) {
            Ok(())
        } else {
            Err(retryable(format!("the target has no store named {pool}")))
        }
    }

    fn create_volume(
        configuration: String,
        request: VolumeRequest,
    ) -> Result<ProvisionedVolume, ProviderError> {
        let target = Target::load(&configuration)?;
        let identifier = request.volume_id;
        let qualified_name = volume_qualified_name(&identifier);
        target.call_tolerating(
            "bdev_lvol_create",
            &json!({"lvs_name": target.configuration.pool, "lvol_name": identifier, "size_in_mib": request.size_in_bytes.div_ceil(MEBIBYTE), "thin_provision": true}),
            &[ALREADY_EXISTS],
        )?;
        target.call_tolerating(
            "nvmf_create_subsystem",
            &json!({"nqn": qualified_name, "allow_any_host": false, "serial_number": "AMAFU0002", "model_number": "Amafu plugin volume"}),
            &[ALREADY_EXISTS],
        )?;
        let has_namespace = target.subsystem(&qualified_name)?.is_some_and(|subsystem| {
            subsystem["namespaces"].as_array().is_some_and(|namespaces| !namespaces.is_empty())
        });
        if !has_namespace {
            target.call_tolerating(
                "nvmf_subsystem_add_ns",
                &json!({"nqn": qualified_name, "namespace": {"bdev_name": target.bdev(&identifier)}}),
                &[ALREADY_EXISTS],
            )?;
        }
        for address in &target.configuration.addresses {
            target.call_tolerating(
                "nvmf_subsystem_add_listener",
                &json!({"nqn": qualified_name, "listen_address": Target::listen_address(address)?}),
                &[ALREADY_EXISTS],
            )?;
        }
        log(Level::Info, &format!("volume {identifier} is provisioned"));
        Ok(ProvisionedVolume { identifier, size_in_bytes: request.size_in_bytes })
    }

    fn delete_volume(configuration: String, volume: ProvisionedVolume) -> Result<(), ProviderError> {
        let target = Target::load(&configuration)?;
        target.call_tolerating(
            "nvmf_delete_subsystem",
            &json!({"nqn": volume_qualified_name(&volume.identifier)}),
            &[NOT_FOUND],
        )?;
        target.call_tolerating(
            "bdev_lvol_delete",
            &json!({"name": target.bdev(&volume.identifier)}),
            &[NOT_FOUND],
        )
    }

    fn export_to_node(
        configuration: String,
        volume: ProvisionedVolume,
        node_name: String,
    ) -> Result<String, ProviderError> {
        let target = Target::load(&configuration)?;
        let qualified_name = volume_qualified_name(&volume.identifier);
        let host = host_qualified_name(&node_name);
        let subsystem = target
            .subsystem(&qualified_name)?
            .ok_or_else(|| retryable(format!("the target has no subsystem {qualified_name} yet")))?;
        if !allows_host(&subsystem, &host) {
            target.call_tolerating(
                "nvmf_subsystem_add_host",
                &json!({"nqn": qualified_name, "host": host}),
                &[ALREADY_EXISTS],
            )?;
        }
        Ok(json!({"type": "nvme-tcp", "qualified_name": qualified_name, "addresses": target.configuration.addresses}).to_string())
    }

    fn revoke_node(configuration: String, node: FencedNode) -> Result<(), ProviderError> {
        let target = Target::load(&configuration)?;
        let host = host_qualified_name(&node.name);
        for subsystem in target.subsystems()? {
            if let (true, Some(qualified_name)) = (allows_host(&subsystem, &host), subsystem["nqn"].as_str())
            {
                target.call_tolerating(
                    "nvmf_subsystem_remove_host",
                    &json!({"nqn": qualified_name, "host": host}),
                    &[NOT_FOUND],
                )?;
            }
        }
        Ok(())
    }

    fn has_revoked_node(configuration: String, node: FencedNode) -> Result<bool, ProviderError> {
        let target = Target::load(&configuration)?;
        let host = host_qualified_name(&node.name);
        Ok(!target.subsystems()?.iter().any(|subsystem| allows_host(subsystem, &host)))
    }

    fn readmit_node(configuration: String, _node: FencedNode) -> Result<(), ProviderError> {
        Target::load(&configuration).map(|_| ())
    }
}

#[allow(unsafe_code)]
mod abi {
    use super::SpdkPlugin;
    amafu_storage_plugin_kit::export_storage_plugin!(SpdkPlugin);
}
