use std::collections::BTreeMap;

use amafu_domain::{NodeName, ObjectId};
use amafu_scenario::{ClusterObservation, Violation};

pub type Invariant = fn(&ClusterObservation) -> Result<(), Violation>;

pub const ALL: &[(&str, Invariant)] = &[
    ("at most one running instance per VM", at_most_one_running_instance_per_vm),
    (
        "status generation never exceeds specification generation",
        status_generation_never_exceeds_specification_generation,
    ),
];

pub fn at_most_one_running_instance_per_vm(observation: &ClusterObservation) -> Result<(), Violation> {
    let mut running_on: BTreeMap<ObjectId, &NodeName> = BTreeMap::new();
    for instance in observation.running_instances() {
        if let Some(other_node) = running_on.insert(instance.vm_id, &instance.node) {
            return Err(Violation::new("VM running more than once")
                .with("vm", instance.vm_id)
                .with("nodes", [other_node, &instance.node]));
        }
    }
    Ok(())
}

pub fn status_generation_never_exceeds_specification_generation(
    observation: &ClusterObservation,
) -> Result<(), Violation> {
    for object in &observation.stored_objects {
        let metadata = &object["metadata"];
        let generation = metadata["generation"].as_u64().unwrap_or(0);
        let observed_generation = metadata["observed_generation"].as_u64().unwrap_or(0);
        if observed_generation > generation {
            return Err(Violation::new("status reports a generation that the specification never reached")
                .with("object", &metadata["name"])
                .with("generation", generation)
                .with("observed_generation", observed_generation));
        }
    }
    Ok(())
}

pub fn check_all(observation: &ClusterObservation) -> Result<(), Violation> {
    ALL.iter().try_for_each(|(_, invariant)| invariant(observation))
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::InstanceState;
    use amafu_scenario::ObservedInstance;
    use serde_json::json;

    use super::*;

    fn instance(node: &str, vm_id: ObjectId, state: InstanceState) -> ObservedInstance {
        ObservedInstance { node: NodeName::new(node).unwrap(), vm_id, state }
    }

    #[test]
    fn a_vm_on_two_nodes_is_a_violation() {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let observation = ClusterObservation {
            instances: vec![
                instance("node-a", vm_id, InstanceState::Running),
                instance("node-b", vm_id, InstanceState::Booting),
            ],
            stored_objects: Vec::new(),
        };
        assert!(at_most_one_running_instance_per_vm(&observation).is_err());
    }

    #[test]
    fn an_exited_copy_is_not_running() {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let observation = ClusterObservation {
            instances: vec![
                instance("node-a", vm_id, InstanceState::Running),
                instance("node-b", vm_id, InstanceState::Exited),
            ],
            stored_objects: Vec::new(),
        };
        assert_eq!(at_most_one_running_instance_per_vm(&observation), Ok(()));
    }

    #[test]
    fn a_status_from_the_future_is_a_violation() {
        let observation = ClusterObservation {
            instances: Vec::new(),
            stored_objects: vec![
                json!({"metadata": {"name": "web-1", "generation": 2, "observed_generation": 3}}),
            ],
        };
        assert!(status_generation_never_exceeds_specification_generation(&observation).is_err());
    }
}
