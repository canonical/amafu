use amafu_domain::{NodeName, ObjectId};
use amafu_interfaces::InstanceState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedInstance {
    pub node: NodeName,
    pub vm_id: ObjectId,
    pub state: InstanceState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterObservation {
    pub instances: Vec<ObservedInstance>,
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
    pub fn node_running(&self, vm_id: ObjectId) -> Option<&NodeName> {
        self.running_instances().find(|instance| instance.vm_id == vm_id).map(|instance| &instance.node)
    }
}
