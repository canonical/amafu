use std::io::{Read, Write};
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use amafu_domain::NodeName;
use amafu_domain::kinds::volume::ProvisionedVolume;
use amafu_interfaces::{
    ActionError, AttachTarget, StorageProtocol, StorageProvider, VolumeAttacher, VolumeRequest,
};
use serde_json::{Value, json};

const FABRICS_DEVICE: &str = "/dev/nvme-fabrics";
const CONTROLLERS: &str = "/sys/class/nvme";
const SUBSYSTEMS: &str = "/sys/class/nvme-subsystem";
const VOLUME_PREFIX: &str = "nqn.2026-11.io.amafu:volume:";
const HOST_PREFIX: &str = "nqn.2026-11.io.amafu:node:";
const ALREADY_EXISTS: i64 = -17;
const NOT_FOUND: i64 = -19;
const NAMESPACE_WAIT: Duration = Duration::from_secs(2);
const NAMESPACE_POLL: Duration = Duration::from_millis(100);
const MEBIBYTE: u64 = 1024 * 1024;

#[must_use]
pub fn volume_qualified_name(identifier: &str) -> String {
    format!("{VOLUME_PREFIX}{identifier}")
}

#[must_use]
pub fn host_qualified_name(node: &NodeName) -> String {
    format!("{HOST_PREFIX}{node}")
}

fn retryable(action: &'static str) -> impl Fn(std::io::Error) -> ActionError {
    move |error| ActionError::retryable(format!("{action}: {error}"))
}

pub struct SpdkTarget {
    management: String,
    credentials: Option<(String, String)>,
    addresses: Vec<SocketAddrV4>,
    pool: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the target refused {method}: {message} ({code})")]
struct RpcRefusal {
    method: String,
    code: i64,
    message: String,
}

impl SpdkTarget {
    pub fn new(
        management: &str,
        credentials: Option<(String, String)>,
        addresses: Vec<SocketAddrV4>,
        pool: &str,
    ) -> Result<Self, ActionError> {
        let http = crate::management::management_client(Duration::from_secs(30))?;
        Ok(Self { management: management.to_owned(), credentials, addresses, pool: pool.to_owned(), http })
    }

    async fn call(&self, method: &str, parameters: Value) -> Result<Result<Value, RpcRefusal>, ActionError> {
        let body = request_body(method, &parameters);
        let mut request = self.http.post(&self.management).json(&body);
        if let Some((user, password)) = &self.credentials {
            request = request.basic_auth(user, Some(password));
        }
        let response = request.send().await.map_err(|error| {
            ActionError::retryable(format!("calling {method} on the NVMe target: {error}"))
        })?;
        let answer: Value = response
            .json()
            .await
            .map_err(|error| ActionError::retryable(format!("reading the answer to {method}: {error}")))?;
        if let Some(error) = answer.get("error") {
            return Ok(Err(RpcRefusal {
                method: method.to_owned(),
                code: error["code"].as_i64().unwrap_or_default(),
                message: error["message"].as_str().unwrap_or_default().to_owned(),
            }));
        }
        Ok(Ok(answer["result"].clone()))
    }

    async fn call_tolerating(
        &self,
        method: &str,
        parameters: Value,
        tolerated: &[i64],
    ) -> Result<(), ActionError> {
        match self.call(method, parameters).await? {
            Ok(_) => Ok(()),
            Err(refusal) if tolerated.contains(&refusal.code) || refused_as_existing(&refusal, tolerated) => {
                Ok(())
            }
            Err(refusal) => Err(ActionError::retryable(refusal)),
        }
    }

    fn bdev(&self, identifier: &str) -> String {
        format!("{}/{identifier}", self.pool)
    }

    async fn subsystems(&self) -> Result<Vec<Value>, ActionError> {
        let subsystems =
            self.call("nvmf_get_subsystems", json!({})).await?.map_err(ActionError::retryable)?;
        Ok(subsystems.as_array().cloned().unwrap_or_default())
    }
}

fn request_body(method: &str, parameters: &Value) -> Value {
    let has_parameters = parameters.as_object().is_none_or(|fields| !fields.is_empty());
    if has_parameters {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": parameters})
    } else {
        json!({"jsonrpc": "2.0", "id": 1, "method": method})
    }
}

fn allows_host(subsystem: &Value, host: &str) -> bool {
    subsystem["hosts"].as_array().into_iter().flatten().any(|entry| entry["nqn"].as_str() == Some(host))
}

fn refused_as_existing(refusal: &RpcRefusal, tolerated: &[i64]) -> bool {
    let message = refusal.message.to_ascii_lowercase();
    (tolerated.contains(&ALREADY_EXISTS) && message.contains("exist"))
        || (tolerated.contains(&NOT_FOUND) && (message.contains("not found") || message.contains("no such")))
}

#[async_trait::async_trait]
impl StorageProvider for SpdkTarget {
    async fn check(&self) -> Result<(), ActionError> {
        let stores = self.call("bdev_lvol_get_lvstores", json!({})).await?.map_err(ActionError::retryable)?;
        let has_pool =
            stores.as_array().into_iter().flatten().any(|store| store["name"].as_str() == Some(&self.pool));
        if has_pool {
            Ok(())
        } else {
            Err(ActionError::retryable(format!("the target has no store named {}", self.pool)))
        }
    }

    async fn create_volume(&self, request: &VolumeRequest) -> Result<ProvisionedVolume, ActionError> {
        if request.source_image.is_some() {
            return Err(ActionError::invalid("NVMe/TCP volumes cannot be cloned from an image yet"));
        }
        let identifier = request.volume_id.to_string();
        let qualified_name = volume_qualified_name(&identifier);
        let size_in_mib = request.size_bytes.div_ceil(MEBIBYTE);
        self.call_tolerating(
            "bdev_lvol_create",
            json!({"lvs_name": self.pool, "lvol_name": identifier, "size_in_mib": size_in_mib, "thin_provision": true}),
            &[ALREADY_EXISTS],
        )
        .await?;
        self.call_tolerating(
            "nvmf_create_subsystem",
            json!({"nqn": qualified_name, "allow_any_host": false, "serial_number": "AMAFU0001", "model_number": "Amafu volume"}),
            &[ALREADY_EXISTS],
        )
        .await?;
        let subsystem = self
            .subsystems()
            .await?
            .into_iter()
            .find(|subsystem| subsystem["nqn"].as_str() == Some(&qualified_name))
            .unwrap_or_default();
        if subsystem["namespaces"].as_array().is_none_or(Vec::is_empty) {
            self.call_tolerating(
                "nvmf_subsystem_add_ns",
                json!({"nqn": qualified_name, "namespace": {"bdev_name": self.bdev(&identifier)}}),
                &[ALREADY_EXISTS],
            )
            .await?;
        }
        for address in &self.addresses {
            self.call_tolerating(
                "nvmf_subsystem_add_listener",
                json!({
                    "nqn": qualified_name,
                    "listen_address": {"trtype": "tcp", "adrfam": "ipv4", "traddr": address.ip().to_string(), "trsvcid": address.port().to_string()},
                }),
                &[ALREADY_EXISTS],
            )
            .await?;
        }
        Ok(ProvisionedVolume { identifier, size_bytes: request.size_bytes, node: None })
    }

    async fn delete_volume(&self, volume: &ProvisionedVolume) -> Result<(), ActionError> {
        let qualified_name = volume_qualified_name(&volume.identifier);
        self.call_tolerating("nvmf_delete_subsystem", json!({"nqn": qualified_name}), &[NOT_FOUND]).await?;
        self.call_tolerating("bdev_lvol_delete", json!({"name": self.bdev(&volume.identifier)}), &[NOT_FOUND])
            .await
    }

    async fn export_to_node(
        &self,
        volume: &ProvisionedVolume,
        node: &NodeName,
    ) -> Result<AttachTarget, ActionError> {
        let qualified_name = volume_qualified_name(&volume.identifier);
        let host = host_qualified_name(node);
        let subsystem = self
            .subsystems()
            .await?
            .into_iter()
            .find(|subsystem| subsystem["nqn"].as_str() == Some(&qualified_name))
            .ok_or_else(|| {
                ActionError::retryable(format!("the target has no subsystem {qualified_name} yet"))
            })?;
        if !allows_host(&subsystem, &host) {
            self.call_tolerating(
                "nvmf_subsystem_add_host",
                json!({"nqn": qualified_name, "host": host}),
                &[ALREADY_EXISTS],
            )
            .await?;
        }
        Ok(AttachTarget::NvmeTcp { qualified_name, addresses: self.addresses.clone() })
    }

    async fn revoke_node(&self, node: &NodeName) -> Result<(), ActionError> {
        let host = host_qualified_name(node);
        for subsystem in self.subsystems().await? {
            let allows = subsystem["hosts"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|entry| entry["nqn"].as_str() == Some(&host));
            if let (true, Some(qualified_name)) = (allows, subsystem["nqn"].as_str()) {
                self.call_tolerating(
                    "nvmf_subsystem_remove_host",
                    json!({"nqn": qualified_name, "host": host}),
                    &[NOT_FOUND],
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn read_attribute(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|value| value.trim().to_owned())
}

fn entries(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn namespace_for(subsystems: &Path, qualified_name: &str) -> Option<PathBuf> {
    entries(subsystems)
        .into_iter()
        .filter(|subsystem| read_attribute(&subsystem.join("subsysnqn")).as_deref() == Some(qualified_name))
        .flat_map(|subsystem| entries(&subsystem))
        .map(|entry| file_name(&entry))
        .filter(|name| name.starts_with("nvme") && name.contains('n') && name[4..].contains('n'))
        .min()
        .map(|name| PathBuf::from("/dev").join(name))
}

fn controllers_for(controllers: &Path, qualified_name: &str) -> Vec<PathBuf> {
    entries(controllers)
        .into_iter()
        .filter(|controller| read_attribute(&controller.join("subsysnqn")).as_deref() == Some(qualified_name))
        .collect()
}

fn connect_options(qualified_name: &str, address: SocketAddrV4, host: &str) -> String {
    format!(
        "transport=tcp,traddr={},trsvcid={},nqn={qualified_name},hostnqn={host}",
        address.ip(),
        address.port()
    )
}

fn connect(options: &str) -> std::io::Result<String> {
    let mut fabrics = std::fs::OpenOptions::new().read(true).write(true).open(FABRICS_DEVICE)?;
    fabrics.write_all(options.as_bytes())?;
    let mut answer = String::new();
    fabrics.read_to_string(&mut answer)?;
    Ok(answer)
}

pub struct NvmeTcpAttacher {
    host_qualified_name: String,
}

impl NvmeTcpAttacher {
    #[must_use]
    pub fn new(node: &NodeName) -> Self {
        Self { host_qualified_name: host_qualified_name(node) }
    }
}

#[async_trait::async_trait]
impl VolumeAttacher for NvmeTcpAttacher {
    fn protocol(&self) -> StorageProtocol {
        StorageProtocol::NvmeTcp
    }

    async fn attach(&self, target: &AttachTarget) -> Result<PathBuf, ActionError> {
        let AttachTarget::NvmeTcp { qualified_name, addresses } = target else {
            return Err(ActionError::invalid("not an NVMe/TCP target"));
        };
        if let Some(device) = namespace_for(Path::new(SUBSYSTEMS), qualified_name) {
            return Ok(device);
        }
        let connected = controllers_for(Path::new(CONTROLLERS), qualified_name);
        if connected.is_empty() {
            for address in addresses {
                let options = connect_options(qualified_name, *address, &self.host_qualified_name);
                let answer = tokio::task::spawn_blocking(move || connect(&options))
                    .await
                    .map_err(|error| ActionError::retryable(format!("connecting: {error}")))?
                    .map_err(retryable("connecting to the NVMe/TCP target"))?;
                tracing::info!(%qualified_name, %address, answer = answer.trim(), "connected to an NVMe/TCP target");
            }
        }
        let started = Instant::now();
        loop {
            if let Some(device) = namespace_for(Path::new(SUBSYSTEMS), qualified_name) {
                return Ok(device);
            }
            if started.elapsed() >= NAMESPACE_WAIT {
                return Err(ActionError::retryable(format!("no namespace of {qualified_name} appeared")));
            }
            tokio::time::sleep(NAMESPACE_POLL).await;
        }
    }

    async fn detach(&self, target: &AttachTarget) -> Result<(), ActionError> {
        let AttachTarget::NvmeTcp { qualified_name, .. } = target else {
            return Err(ActionError::invalid("not an NVMe/TCP target"));
        };
        for controller in controllers_for(Path::new(CONTROLLERS), qualified_name) {
            std::fs::write(controller.join("delete_controller"), b"1").map_err(retryable("disconnecting"))?;
        }
        Ok(())
    }

    async fn attached(&self) -> Result<Vec<(AttachTarget, PathBuf)>, ActionError> {
        let mut attached = Vec::new();
        for subsystem in entries(Path::new(SUBSYSTEMS)) {
            let Some(qualified_name) = read_attribute(&subsystem.join("subsysnqn")) else {
                continue;
            };
            if !qualified_name.starts_with(VOLUME_PREFIX) {
                continue;
            }
            if let Some(device) = namespace_for(Path::new(SUBSYSTEMS), &qualified_name) {
                attached.push((AttachTarget::NvmeTcp { qualified_name, addresses: Vec::new() }, device));
            }
        }
        Ok(attached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_client_is_built_without_touching_the_network() {
        let target = SpdkTarget::new(
            "http://192.0.2.41:9009/",
            None,
            vec!["192.0.2.41:4420".parse().unwrap()],
            "amafu",
        );
        assert!(target.is_ok());
    }

    #[test]
    fn connect_options_name_the_target_and_this_host() {
        let options = connect_options(
            &volume_qualified_name("abc"),
            "10.216.0.41:4420".parse().unwrap(),
            &host_qualified_name(&NodeName::new("node-3").unwrap()),
        );
        assert_eq!(
            options,
            "transport=tcp,traddr=10.216.0.41,trsvcid=4420,nqn=nqn.2026-11.io.amafu:volume:abc,hostnqn=nqn.2026-11.io.amafu:node:node-3"
        );
    }

    #[test]
    fn the_namespace_of_a_subsystem_is_found_in_sysfs() {
        let sysfs = tempfile::tempdir().unwrap();
        let subsystem = sysfs.path().join("nvme-subsys0");
        std::fs::create_dir_all(subsystem.join("nvme0n1")).unwrap();
        std::fs::create_dir_all(subsystem.join("nvme0")).unwrap();
        std::fs::write(subsystem.join("subsysnqn"), format!("{}\n", volume_qualified_name("abc"))).unwrap();
        assert_eq!(
            namespace_for(sysfs.path(), &volume_qualified_name("abc")),
            Some(PathBuf::from("/dev/nvme0n1"))
        );
        assert_eq!(namespace_for(sysfs.path(), &volume_qualified_name("other")), None);
    }

    #[test]
    fn methods_without_parameters_are_called_without_a_parameter_object() {
        assert_eq!(request_body("nvmf_get_subsystems", &json!({})).get("params"), None);
        assert_eq!(request_body("nvmf_delete_subsystem", &json!({"nqn": "x"}))["params"]["nqn"], "x");
    }

    #[test]
    fn a_host_already_allowed_is_recognized() {
        let subsystem = json!({"nqn": "x", "hosts": [{"nqn": "nqn.2026-11.io.amafu:node:node-0"}]});
        assert!(allows_host(&subsystem, "nqn.2026-11.io.amafu:node:node-0"));
        assert!(!allows_host(&subsystem, "nqn.2026-11.io.amafu:node:node-1"));
    }

    #[test]
    fn known_refusals_are_recognized_by_message() {
        let refusal = |message: &str| RpcRefusal {
            method: String::from("m"),
            code: -32602,
            message: message.to_owned(),
        };
        assert!(refused_as_existing(&refusal("Subsystem NQN already exists"), &[ALREADY_EXISTS]));
        assert!(refused_as_existing(&refusal("bdev not found"), &[NOT_FOUND]));
        assert!(!refused_as_existing(&refusal("Invalid parameters"), &[ALREADY_EXISTS, NOT_FOUND]));
    }
}
