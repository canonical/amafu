use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use amafu_domain::{NodeName, ObjectId};
use amafu_scenario::{ClusterObservation, Violation};

pub type Invariant = fn(&ClusterObservation) -> Result<(), Violation>;

pub const ALL: &[(&str, Invariant)] = &[
    ("at most one running instance per VM", at_most_one_running_instance_per_vm),
    (
        "status generation never exceeds specification generation",
        status_generation_never_exceeds_specification_generation,
    ),
    ("a volume is attached on at most one node", a_volume_is_attached_on_at_most_one_node),
    ("no address is used twice in one network", no_address_is_used_twice_in_one_network),
    ("no write is accepted with a stale fencing token", no_write_is_accepted_with_a_stale_fencing_token),
];

pub fn a_floating_address_is_served_by_at_most_one_node(
    observation: &ClusterObservation,
) -> Result<(), Violation> {
    let mut serving: BTreeMap<Ipv4Addr, &NodeName> = BTreeMap::new();
    for floating in &observation.floating_addresses {
        if let Some(other) = serving.insert(floating.address, &floating.node)
            && other != &floating.node
        {
            return Err(Violation::new("a floating address is served by two nodes")
                .with("address", floating.address)
                .with("nodes", [other, &floating.node]));
        }
    }
    Ok(())
}

pub fn no_write_is_accepted_with_a_stale_fencing_token(
    observation: &ClusterObservation,
) -> Result<(), Violation> {
    let mut highest: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for write in observation.fenced_writes.iter().filter(|write| write.is_accepted) {
        let accepted = highest.entry((write.object.as_str(), write.fence.as_str())).or_default();
        if write.token < *accepted {
            return Err(Violation::new("a write with a stale fencing token was accepted")
                .with("object", &write.object)
                .with("fence", &write.fence)
                .with("token", write.token)
                .with("newer token", *accepted));
        }
        *accepted = write.token;
    }
    Ok(())
}

pub fn a_volume_is_attached_on_at_most_one_node(observation: &ClusterObservation) -> Result<(), Violation> {
    let mut nodes_by_volume: BTreeMap<&str, BTreeSet<&NodeName>> = BTreeMap::new();
    for attachment in &observation.attachments {
        let nodes = nodes_by_volume.entry(attachment.volume.as_str()).or_default();
        nodes.insert(&attachment.node);
        if nodes.len() > 1 {
            return Err(Violation::new("a volume is attached on two nodes")
                .with("volume", &attachment.volume)
                .with("nodes", nodes));
        }
    }
    Ok(())
}

fn stored_addresses(object: &serde_json::Value) -> Vec<(String, Ipv4Addr)> {
    let project = object["metadata"]["project"].as_str().unwrap_or_default();
    object["status"]["interfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|interface| {
            let network = interface["network"].as_str()?;
            let address = interface["address"].as_str()?.parse().ok()?;
            Some((format!("{project}/{network}"), address))
        })
        .collect()
}

pub fn no_address_is_used_twice_in_one_network(observation: &ClusterObservation) -> Result<(), Violation> {
    let mut seen: BTreeSet<(String, Ipv4Addr)> = BTreeSet::new();
    for (network, address) in observation.stored_objects.iter().flat_map(stored_addresses) {
        if !seen.insert((network.clone(), address)) {
            return Err(Violation::new("two VMs were given the same address")
                .with("network", network)
                .with("address", address));
        }
    }
    let mut realized: BTreeMap<(String, Ipv4Addr), ObjectId> = BTreeMap::new();
    for port in &observation.ports {
        if let Some(other) = realized.insert((port.segment.clone(), port.address), port.vm_id)
            && other != port.vm_id
        {
            return Err(Violation::new("two ports carry the same address")
                .with("segment", &port.segment)
                .with("address", port.address)
                .with("vms", [other, port.vm_id]));
        }
    }
    Ok(())
}

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
    use amafu_scenario::{ObservedFencedWrite, ObservedFloatingAddress, ObservedInstance};
    use serde_json::json;

    use super::*;

    fn instance(node: &str, vm_id: ObjectId, state: InstanceState) -> ObservedInstance {
        ObservedInstance {
            node: NodeName::new(node).unwrap(),
            vm_id,
            state,
            workload: None,
            disks: Vec::new(),
            offered_addresses: Vec::new(),
            console: String::new(),
        }
    }

    #[test]
    fn a_vm_on_two_nodes_is_a_violation() {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let observation = ClusterObservation {
            instances: vec![
                instance("node-a", vm_id, InstanceState::Running),
                instance("node-b", vm_id, InstanceState::Booting),
            ],
            ..ClusterObservation::default()
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
            ..ClusterObservation::default()
        };
        assert_eq!(at_most_one_running_instance_per_vm(&observation), Ok(()));
    }

    #[test]
    fn a_status_from_the_future_is_a_violation() {
        let observation = ClusterObservation {
            stored_objects: vec![
                json!({"metadata": {"name": "web-1", "generation": 2, "observed_generation": 3}}),
            ],
            ..ClusterObservation::default()
        };
        assert!(status_generation_never_exceeds_specification_generation(&observation).is_err());
    }

    #[test]
    fn a_volume_on_two_nodes_is_a_violation() {
        let attachment = |node: &str| amafu_scenario::ObservedAttachment {
            node: NodeName::new(node).unwrap(),
            volume: String::from("data"),
            path: "/dev/vdb".into(),
        };
        let observation = ClusterObservation {
            attachments: vec![attachment("node-a"), attachment("node-a")],
            ..ClusterObservation::default()
        };
        assert_eq!(a_volume_is_attached_on_at_most_one_node(&observation), Ok(()));
        let observation = ClusterObservation {
            attachments: vec![attachment("node-a"), attachment("node-b")],
            ..ClusterObservation::default()
        };
        assert!(a_volume_is_attached_on_at_most_one_node(&observation).is_err());
    }

    #[test]
    fn a_reused_address_is_a_violation() {
        let vm = |address: &str| json!({"metadata": {"project": "project-a"}, "status": {"interfaces": [{"network": "default", "address": address}]}});
        let distinct = ClusterObservation {
            stored_objects: vec![vm("10.1.0.2"), vm("10.1.0.3")],
            ..ClusterObservation::default()
        };
        assert_eq!(no_address_is_used_twice_in_one_network(&distinct), Ok(()));
        let reused = ClusterObservation {
            stored_objects: vec![vm("10.1.0.2"), vm("10.1.0.2")],
            ..ClusterObservation::default()
        };
        assert!(no_address_is_used_twice_in_one_network(&reused).is_err());
    }

    #[test]
    fn a_floating_address_on_two_nodes_is_a_violation() {
        let served = |node: &str| ObservedFloatingAddress {
            node: NodeName::new(node).unwrap(),
            address: Ipv4Addr::new(198, 51, 100, 7),
            private_address: Ipv4Addr::new(10, 1, 0, 2),
        };
        let mut observation = ClusterObservation {
            floating_addresses: vec![served("node-0")],
            ..ClusterObservation::default()
        };
        assert!(a_floating_address_is_served_by_at_most_one_node(&observation).is_ok());
        observation.floating_addresses.push(served("node-1"));
        assert!(a_floating_address_is_served_by_at_most_one_node(&observation).is_err());
    }

    #[test]
    fn an_accepted_write_with_an_older_token_is_a_violation() {
        let write = |token, is_accepted| ObservedFencedWrite {
            object: String::from("Vm/project-a/web-1"),
            fence: String::from("scheduler"),
            token,
            is_accepted,
        };
        let mut observation = ClusterObservation {
            fenced_writes: vec![write(5, true), write(4, false), write(6, true)],
            ..ClusterObservation::default()
        };
        assert!(no_write_is_accepted_with_a_stale_fencing_token(&observation).is_ok());
        observation.fenced_writes.push(write(5, true));
        assert!(no_write_is_accepted_with_a_stale_fencing_token(&observation).is_err());
    }
}
