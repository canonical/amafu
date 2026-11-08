use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::Operation;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::registry;
use amafu_domain::{Change, ObjectName, ObjectReference, ProjectName, Resource};
use amafu_interfaces::objects::{apply, remove_untyped, update_status};
use amafu_interfaces::{Consistency, Store};
use serde_json::{Value, json};

use crate::seed::now;

const TICK: Duration = Duration::from_millis(400);
const STEP_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const FIRST_HOST_ADDRESS: u32 = 10;
const FIRST_SEGMENT: u64 = 1000;

pub struct Animator {
    store: Arc<dyn Store>,
    last_steps: HashMap<String, Instant>,
    last_heartbeat: Option<Instant>,
}

fn text<'value>(value: &'value Value, pointer: &str) -> Option<&'value str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn name_of(object: &Value) -> &str {
    text(object, "/metadata/name").unwrap_or_default()
}

fn project_of(object: &Value) -> Option<&str> {
    text(object, "/metadata/project")
}

fn reference_of<R: Resource>(object: &Value) -> anyhow::Result<ObjectReference> {
    Ok(ObjectReference {
        kind: R::KIND.to_owned(),
        project: project_of(object).map(ProjectName::new).transpose()?,
        name: ObjectName::new(name_of(object))?,
    })
}

fn parse_subnet(subnet: &str) -> Option<(u32, u32)> {
    let (address, prefix) = subnet.split_once('/')?;
    Some((u32::from(address.parse::<Ipv4Addr>().ok()?), prefix.parse().ok()?))
}

fn host_address(subnet: &str, host: u32) -> Option<String> {
    let (base, _) = parse_subnet(subnet)?;
    Some(Ipv4Addr::from(base + host).to_string())
}

fn mac_for(project: &str, vm: &str, index: usize) -> String {
    let digest = crate::seed::digest_of(&format!("{project}/{vm}/{index}"));
    format!("52:54:00:{}:{}:{}", &digest[0..2], &digest[2..4], &digest[4..6])
}

fn free_address(range: &Value, taken: &BTreeSet<String>) -> anyhow::Result<Option<String>> {
    let bound = |pointer: &str| -> anyhow::Result<u32> {
        Ok(u32::from(text(range, pointer).unwrap_or("0.0.0.0").parse::<Ipv4Addr>()?))
    };
    let (first, last) = (bound("/specification/first_address")?, bound("/specification/last_address")?);
    Ok((first..=last).map(|value| Ipv4Addr::from(value).to_string()).find(|address| !taken.contains(address)))
}

fn is_deleting(object: &Value) -> bool {
    object.pointer("/metadata/is_deleting").and_then(Value::as_bool).unwrap_or(false)
}

impl Animator {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store, last_steps: HashMap::new(), last_heartbeat: None }
    }

    pub async fn run(mut self) {
        loop {
            if let Err(error) = self.tick().await {
                tracing::warn!(%error, "the demo cluster could not settle");
            }
            tokio::time::sleep(TICK).await;
        }
    }

    pub async fn tick(&mut self) -> anyhow::Result<()> {
        self.remove_deleted().await?;
        self.settle_networks().await?;
        self.settle_images().await?;
        self.place_vms().await?;
        self.settle_volumes().await?;
        self.settle_floating_ips().await?;
        self.advance_operations().await?;
        self.beat_hearts().await
    }

    async fn objects(&self, kind: &str) -> anyhow::Result<Vec<Value>> {
        Ok(self
            .store
            .list(kind, None, Consistency::Linearizable)
            .await?
            .into_iter()
            .map(|record| record.object)
            .collect())
    }

    async fn set_status<R: Resource>(&self, object: &Value, status: Value) -> anyhow::Result<()> {
        if object.get("status") == Some(&status) {
            return Ok(());
        }
        let status: R::Status = serde_json::from_value(status)?;
        update_status::<R>(self.store.as_ref(), &reference_of::<R>(object)?, |_| Some(status.clone()))
            .await?;
        Ok(())
    }

    async fn remove_deleted(&self) -> anyhow::Result<()> {
        for descriptor in registry::kinds() {
            for object in self.objects(descriptor.kind).await? {
                if is_deleting(&object) {
                    let reference = ObjectReference {
                        kind: descriptor.kind.to_owned(),
                        project: project_of(&object).map(ProjectName::new).transpose()?,
                        name: ObjectName::new(name_of(&object))?,
                    };
                    remove_untyped(self.store.as_ref(), &descriptor, &reference).await?;
                }
            }
        }
        Ok(())
    }

    async fn settle_networks(&self) -> anyhow::Result<()> {
        for (index, network) in self.objects(Network::KIND).await?.iter().enumerate() {
            let subnet = text(network, "/specification/subnet").unwrap_or_default();
            let status = json!({"phase": "ready", "gateway": host_address(subnet, 1), "segment_id": FIRST_SEGMENT + index as u64});
            self.set_status::<Network>(network, status).await?;
        }
        Ok(())
    }

    async fn settle_images(&self) -> anyhow::Result<()> {
        let nodes: Vec<String> =
            self.objects(Node::KIND).await?.iter().take(3).map(|node| name_of(node).to_owned()).collect();
        for image in self.objects(Image::KIND).await? {
            let size: u64 = if text(&image, "/specification/source/type") == Some("disk") {
                2_684_354_560
            } else {
                125_829_120
            };
            self.set_status::<Image>(
                &image,
                json!({"phase": "ready", "size_bytes": size, "cached_on": nodes}),
            )
            .await?;
        }
        Ok(())
    }

    async fn eligible_nodes(&self) -> anyhow::Result<Vec<Value>> {
        Ok(self
            .objects(Node::KIND)
            .await?
            .into_iter()
            .filter(|node| {
                text(node, "/status/phase") == Some("ready")
                    && !node.pointer("/specification/is_cordoned").and_then(Value::as_bool).unwrap_or(false)
                    && node
                        .pointer("/specification/roles")
                        .and_then(Value::as_array)
                        .is_some_and(|roles| roles.iter().any(|role| role == "compute"))
            })
            .collect())
    }

    fn least_used(nodes: &[Value], vms: &[Value], excluded: Option<&str>) -> Option<String> {
        let mut usage: BTreeMap<&str, u64> =
            nodes.iter().map(name_of).filter(|name| Some(*name) != excluded).map(|name| (name, 0)).collect();
        for vm in vms {
            if let Some(used) = text(vm, "/status/node").and_then(|node| usage.get_mut(node)) {
                *used += vm.pointer("/specification/cpu_count").and_then(Value::as_u64).unwrap_or_default();
            }
        }
        usage.into_iter().min_by_key(|(name, used)| (*used, name.to_owned())).map(|(name, _)| name.to_owned())
    }

    async fn place_vms(&self) -> anyhow::Result<()> {
        let nodes = self.eligible_nodes().await?;
        let networks = self.objects(Network::KIND).await?;
        let mut vms = self.objects(Vm::KIND).await?;
        let mut used: BTreeSet<(String, String)> = BTreeSet::new();
        for vm in &vms {
            for interface in vm.pointer("/status/interfaces").and_then(Value::as_array).into_iter().flatten()
            {
                used.insert((
                    text(interface, "/network").unwrap_or_default().to_owned(),
                    text(interface, "/address").unwrap_or_default().to_owned(),
                ));
            }
        }
        for index in 0..vms.len() {
            let vm = vms[index].clone();
            let project = project_of(&vm).unwrap_or_default().to_owned();
            let is_running = text(&vm, "/specification/power_state") != Some("stopped");
            let mut status = vm.get("status").cloned().unwrap_or_else(|| json!({}));
            if !is_running {
                status = json!({"phase": "stopped"});
            } else if text(&vm, "/status/node").is_none() {
                let Some(node) = Self::least_used(&nodes, &vms, None) else { continue };
                let mut interfaces = Vec::new();
                for (position, interface) in vm
                    .pointer("/specification/network_interfaces")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let network = text(interface, "/network").unwrap_or_default();
                    let Some(subnet) = networks
                        .iter()
                        .find(|candidate| {
                            name_of(candidate) == network && project_of(candidate) == Some(project.as_str())
                        })
                        .and_then(|candidate| text(candidate, "/specification/subnet"))
                    else {
                        continue;
                    };
                    let address = (FIRST_HOST_ADDRESS..250)
                        .filter_map(|host| host_address(subnet, host))
                        .find(|address| !used.contains(&(network.to_owned(), address.clone())));
                    if let Some(address) = address {
                        used.insert((network.to_owned(), address.clone()));
                        interfaces.push(json!({"network": network, "mac": mac_for(&project, name_of(&vm), position), "address": address}));
                    }
                }
                status = json!({"phase": "running", "node": node, "interfaces": interfaces});
            }
            self.set_status::<Vm>(&vm, status.clone()).await?;
            vms[index]["status"] = status;
        }
        Ok(())
    }

    async fn settle_volumes(&self) -> anyhow::Result<()> {
        let vms = self.objects(Vm::KIND).await?;
        for volume in self.objects(Volume::KIND).await? {
            let name = name_of(&volume);
            let user = vms.iter().find(|vm| {
                project_of(vm) == project_of(&volume)
                    && text(vm, "/status/node").is_some()
                    && vm
                        .pointer("/specification/volumes")
                        .and_then(Value::as_array)
                        .is_some_and(|volumes| volumes.iter().any(|candidate| candidate == name))
            });
            let size =
                volume.pointer("/specification/size_gibibytes").and_then(Value::as_u64).unwrap_or_default()
                    << 30;
            let is_local = text(&volume, "/specification/backend") == Some("local");
            let node = if is_local { user.and_then(|vm| text(vm, "/status/node")) } else { None };
            let provisioned =
                json!({"identifier": format!("amafu-{name}"), "size_bytes": size, "node": node});
            let status = match user {
                Some(vm) => {
                    json!({"phase": "attached", "provisioned": provisioned, "attached_to": name_of(vm)})
                }
                None => json!({"phase": "available", "provisioned": provisioned}),
            };
            self.set_status::<Volume>(&volume, status).await?;
        }
        Ok(())
    }

    async fn settle_floating_ips(&self) -> anyhow::Result<()> {
        let external_networks = self.objects("ExternalNetwork").await?;
        let vms = self.objects(Vm::KIND).await?;
        let floating_ips = self.objects(FloatingIp::KIND).await?;
        let mut taken: BTreeSet<String> = floating_ips
            .iter()
            .filter_map(|floating_ip| text(floating_ip, "/status/address").map(str::to_owned))
            .collect();
        for floating_ip in &floating_ips {
            let existing =
                text(floating_ip, "/status/address").or_else(|| text(floating_ip, "/specification/address"));
            let address = if let Some(address) = existing {
                address.to_owned()
            } else {
                let network = text(floating_ip, "/specification/external_network").unwrap_or_default();
                let range = external_networks.iter().find(|candidate| name_of(candidate) == network);
                let Some(free) = range.map(|range| free_address(range, &taken)).transpose()?.flatten() else {
                    continue;
                };
                free
            };
            taken.insert(address.clone());
            let target = text(floating_ip, "/specification/target/vm");
            let bound = target.and_then(|target| {
                vms.iter().find(|vm| {
                    name_of(vm) == target
                        && project_of(vm) == project_of(floating_ip)
                        && text(vm, "/status/node").is_some()
                })
            });
            let status = match bound {
                Some(vm) => json!({"phase": "bound", "address": address, "binding": {
                    "node": text(vm, "/status/node"),
                    "vm_id": text(vm, "/metadata/id"),
                    "private_address": text(vm, "/status/interfaces/0/address").unwrap_or("0.0.0.0"),
                }}),
                None => json!({"phase": "allocated", "address": address}),
            };
            self.set_status::<FloatingIp>(floating_ip, status).await?;
        }
        Ok(())
    }

    async fn find_vm(&self, reference: &Value) -> anyhow::Result<Option<Value>> {
        let project = text(reference, "/project");
        let name = text(reference, "/name");
        Ok(self
            .objects(Vm::KIND)
            .await?
            .into_iter()
            .find(|vm| project_of(vm) == project && Some(name_of(vm)) == name))
    }

    async fn move_vm(&self, reference: &Value, destination: Option<&str>, phase: &str) -> anyhow::Result<()> {
        let Some(vm) = self.find_vm(reference).await? else { return Ok(()) };
        let Some(source) = text(&vm, "/status/node").map(str::to_owned) else { return Ok(()) };
        let nodes = self.eligible_nodes().await?;
        let vms = self.objects(Vm::KIND).await?;
        let destination = match destination {
            Some(destination) => destination.to_owned(),
            None => text(&vm, "/status/migration/destination")
                .map(str::to_owned)
                .or_else(|| Self::least_used(&nodes, &vms, Some(&source)))
                .unwrap_or_else(|| source.clone()),
        };
        let mut status = vm.get("status").cloned().unwrap_or_else(|| json!({}));
        let attempt =
            status.pointer("/migration/attempt").cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4()));
        let origin = text(&status, "/migration/source").map_or(source.clone(), str::to_owned);
        let mut migration = json!({"attempt": attempt, "source": origin, "destination": destination,
            "destination_address": "10.0.0.20", "downtime_milliseconds": 38, "phase": phase});
        if phase == "cleaned" {
            migration["report"] = json!({"total_bytes": 8_796_093_022_u64, "final_round_bytes": 41_943_040, "duration_milliseconds": 2100, "downtime_milliseconds": 38});
        }
        if phase == "switched-over" || phase == "cleaned" {
            status["node"] = json!(destination);
        }
        status["migration"] = migration;
        self.set_status::<Vm>(&vm, status).await
    }

    async fn cordon(&self, node: &str) -> anyhow::Result<()> {
        let Some(object) =
            self.objects(Node::KIND).await?.into_iter().find(|candidate| name_of(candidate) == node)
        else {
            return Ok(());
        };
        let mut specification = object.get("specification").cloned().unwrap_or_default();
        specification["is_cordoned"] = json!(true);
        apply::<Node>(
            self.store.as_ref(),
            &reference_of::<Node>(&object)?,
            Change::UpdateSpecification {
                expected_generation: None,
                specification: serde_json::from_value(specification)?,
            },
        )
        .await?;
        Ok(())
    }

    async fn perform(&self, step: &Value) -> anyhow::Result<()> {
        let vm = step.get("vm").cloned().unwrap_or_default();
        match text(step, "/step").unwrap_or_default() {
            "prepare-destination" => self.move_vm(&vm, text(step, "/destination"), "destination-ready").await,
            "copy-memory" => self.move_vm(&vm, None, "copied").await,
            "switch-over" => self.move_vm(&vm, None, "switched-over").await,
            "clean-up-source" | "migrate-vm" => self.move_vm(&vm, None, "cleaned").await,
            "cordon" => self.cordon(text(step, "/node").unwrap_or_default()).await,
            _ => Ok(()),
        }
    }

    async fn advance_operations(&mut self) -> anyhow::Result<()> {
        for operation in self.objects(Operation::KIND).await? {
            if operation.pointer("/status/outcome").is_some() {
                continue;
            }
            let name = name_of(&operation).to_owned();
            if operation.pointer("/specification/is_cancelled").and_then(Value::as_bool).unwrap_or(false) {
                let mut status = operation.get("status").cloned().unwrap_or_else(|| json!({}));
                status["outcome"] = json!({"outcome": "cancelled"});
                status.as_object_mut().map(|fields| fields.remove("current_step"));
                self.set_status::<Operation>(&operation, status).await?;
                continue;
            }
            if operation.pointer("/specification/is_paused").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let is_due = self.last_steps.get(&name).is_none_or(|last| last.elapsed() >= STEP_INTERVAL);
            if !is_due {
                continue;
            }
            self.last_steps.insert(name, Instant::now());
            let steps = operation
                .pointer("/specification/steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let completed = operation
                .pointer("/status/completed_steps")
                .and_then(Value::as_u64)
                .and_then(|completed| usize::try_from(completed).ok())
                .unwrap_or(0);
            if let Some(step) = steps.get(completed) {
                self.perform(step).await?;
            }
            let completed = (completed + 1).min(steps.len());
            let status = if completed == steps.len() {
                json!({"completed_steps": completed, "outcome": {"outcome": "succeeded"}})
            } else {
                json!({"completed_steps": completed, "current_step": {"attempts": 1, "started_at": now()}})
            };
            self.set_status::<Operation>(&operation, status).await?;
        }
        Ok(())
    }

    async fn beat_hearts(&mut self) -> anyhow::Result<()> {
        if self.last_heartbeat.is_some_and(|last| last.elapsed() < HEARTBEAT_INTERVAL) {
            return Ok(());
        }
        self.last_heartbeat = Some(Instant::now());
        for node in self.objects(Node::KIND).await? {
            let mut status = node.get("status").cloned().unwrap_or_default();
            status["last_heartbeat"] = json!(now());
            self.set_status::<Node>(&node, status).await?;
        }
        Ok(())
    }
}
