use std::net::Ipv4Addr;
use std::path::PathBuf;

use amafu_domain::{NodeName, ObjectId};
use amafu_interfaces::{InstanceState, WorkloadState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedInstance {
    pub node: NodeName,
    pub vm_id: ObjectId,
    pub state: InstanceState,
    #[serde(default)]
    pub workload: Option<WorkloadState>,
    #[serde(default)]
    pub disks: Vec<PathBuf>,
    #[serde(default)]
    pub offered_addresses: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedAttachment {
    pub node: NodeName,
    pub volume: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedPort {
    pub node: NodeName,
    pub vm_id: ObjectId,
    pub segment: String,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterObservation {
    pub instances: Vec<ObservedInstance>,
    #[serde(default)]
    pub attachments: Vec<ObservedAttachment>,
    #[serde(default)]
    pub ports: Vec<ObservedPort>,
    pub stored_objects: Vec<Value>,
}

impl ClusterObservation {
    pub fn running_instances(&self) -> impl Iterator<Item = &ObservedInstance> {
        self.instances.iter().filter(|instance| instance.state != InstanceState::Exited)
    }

    #[must_use]
    pub fn is_running(&self, vm_id: ObjectId) -> bool {
        self.running_instances()
            .any(|instance| instance.vm_id == vm_id && instance.state == InstanceState::Running)
    }

    #[must_use]
    pub fn instance(&self, vm_id: ObjectId) -> Option<&ObservedInstance> {
        self.running_instances().find(|instance| instance.vm_id == vm_id)
    }

    #[must_use]
    pub fn node_running(&self, vm_id: ObjectId) -> Option<&NodeName> {
        self.running_instances().find(|instance| instance.vm_id == vm_id).map(|instance| &instance.node)
    }
}
