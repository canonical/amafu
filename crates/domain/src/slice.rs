use std::collections::BTreeSet;

use serde_json::Value;

use crate::kinds::node::NodeRole;
use crate::name::NodeName;

const CLUSTER_WIDE_KINDS: &[&str] =
    &["Image", "StorageBackend", "Network", "SecurityGroup", "ExternalNetwork"];

type ProjectAndName = (String, String);

fn text<'object>(object: &'object Value, pointer: &str) -> Option<&'object str> {
    object.pointer(pointer).and_then(Value::as_str)
}

fn project_and_name(object: &Value) -> ProjectAndName {
    (
        text(object, "/metadata/project").unwrap_or_default().to_owned(),
        text(object, "/metadata/name").unwrap_or_default().to_owned(),
    )
}

fn is_deleting(object: &Value) -> bool {
    object.pointer("/metadata/is_deleting").and_then(Value::as_bool).unwrap_or(false)
}

fn names_node(object: &Value, pointer: &str, node: &NodeName) -> bool {
    text(object, pointer) == Some(node.as_str())
}

fn without_status(object: &Value) -> Value {
    let mut stripped = object.clone();
    if let Some(fields) = stripped.as_object_mut() {
        fields.remove("status");
    }
    if let Some(metadata) = stripped.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("observed_generation");
    }
    stripped
}

fn changes_only_status(before: &Value, after: &Value) -> bool {
    without_status(before) == without_status(after)
}

fn node_set(object: &Value, pointer: &str) -> BTreeSet<String> {
    object
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSlice {
    node: NodeName,
    hosted_vms: BTreeSet<ProjectAndName>,
    hosted_projects: BTreeSet<String>,
    used_volumes: BTreeSet<ProjectAndName>,
}

impl NodeSlice {
    #[must_use]
    pub fn new(node: NodeName) -> Self {
        Self {
            node,
            hosted_vms: BTreeSet::new(),
            hosted_projects: BTreeSet::new(),
            used_volumes: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn node(&self) -> &NodeName {
        &self.node
    }

    pub fn learn_vms<'vms>(&mut self, vms: impl IntoIterator<Item = &'vms Value>) {
        self.hosted_vms.clear();
        self.hosted_projects.clear();
        self.used_volumes.clear();
        for vm in vms.into_iter().filter(|vm| names_node(vm, "/status/node", &self.node)) {
            let (project, name) = project_and_name(vm);
            let volumes =
                vm.pointer("/specification/volumes").and_then(Value::as_array).into_iter().flatten();
            for volume in volumes.filter_map(Value::as_str) {
                self.used_volumes.insert((project.clone(), volume.to_owned()));
            }
            self.hosted_projects.insert(project.clone());
            self.hosted_vms.insert((project, name));
        }
    }

    #[must_use]
    pub fn hosts_vm(&self, project: &str, name: &str) -> bool {
        self.hosted_vms.contains(&(project.to_owned(), name.to_owned()))
    }

    #[must_use]
    pub fn includes(&self, kind: &str, object: &Value) -> bool {
        if CLUSTER_WIDE_KINDS.contains(&kind) {
            return true;
        }
        match kind {
            "Node" => text(object, "/metadata/name") == Some(self.node.as_str()),
            "Vm" => names_node(object, "/status/node", &self.node),
            "Volume" => {
                self.used_volumes.contains(&project_and_name(object))
                    || names_node(object, "/status/provisioned/node", &self.node)
            }
            "NetworkEndpoint" => {
                self.hosted_projects.contains(&project_and_name(object).0)
                    || names_node(object, "/specification/node", &self.node)
            }
            "FloatingIp" => names_node(object, "/status/binding/node", &self.node),
            _ => false,
        }
    }

    #[must_use]
    pub fn may_write(
        &self,
        roles: &BTreeSet<NodeRole>,
        kind: &str,
        before: Option<&Value>,
        after: Option<&Value>,
    ) -> bool {
        match (before, after) {
            (None, None) => false,
            (None, Some(created)) => self.may_create(roles, kind, created),
            (Some(current), Some(updated)) => self.may_update(kind, current, updated),
            (Some(current), None) => self.may_remove(kind, current),
        }
    }

    fn may_create(&self, roles: &BTreeSet<NodeRole>, kind: &str, created: &Value) -> bool {
        match kind {
            "Node" => {
                let claimed: BTreeSet<String> = node_set(created, "/specification/roles");
                let held: BTreeSet<String> = roles
                    .iter()
                    .filter_map(|role| serde_json::to_value(role).ok())
                    .filter_map(|role| role.as_str().map(str::to_owned))
                    .collect();
                text(created, "/metadata/name") == Some(self.node.as_str()) && claimed.is_subset(&held)
            }
            "NetworkEndpoint" => self.owns_endpoint(created),
            _ => false,
        }
    }

    fn may_update(&self, kind: &str, current: &Value, updated: &Value) -> bool {
        if kind == "NetworkEndpoint" {
            let is_same = project_and_name(current) == project_and_name(updated);
            let is_withdrawal = is_deleting(updated)
                && names_node(current, "/specification/node", &self.node)
                && names_node(updated, "/specification/node", &self.node);
            return is_same
                && (is_withdrawal || (self.owns_endpoint(current) && self.owns_endpoint(updated)));
        }
        if !changes_only_status(current, updated) {
            return false;
        }
        match kind {
            "Node" => text(current, "/metadata/name") == Some(self.node.as_str()),
            "Vm" => {
                names_node(current, "/status/node", &self.node)
                    && names_node(updated, "/status/node", &self.node)
            }
            "Volume" => {
                let provisioned_here_or_nowhere = |object: &Value| {
                    object.pointer("/status/provisioned/node").is_none_or(Value::is_null)
                        || names_node(object, "/status/provisioned/node", &self.node)
                };
                self.includes(kind, current)
                    && provisioned_here_or_nowhere(current)
                    && provisioned_here_or_nowhere(updated)
            }
            "Image" => {
                let before = node_set(current, "/status/cached_on");
                let after = node_set(updated, "/status/cached_on");
                before.symmetric_difference(&after).all(|changed| changed == self.node.as_str())
            }
            _ => false,
        }
    }

    fn may_remove(&self, kind: &str, current: &Value) -> bool {
        match kind {
            "Node" => is_deleting(current) && text(current, "/metadata/name") == Some(self.node.as_str()),
            "Vm" => is_deleting(current) && names_node(current, "/status/node", &self.node),
            "Image" => {
                is_deleting(current)
                    && node_set(current, "/status/cached_on")
                        .iter()
                        .all(|cached| cached == self.node.as_str())
            }
            "NetworkEndpoint" => names_node(current, "/specification/node", &self.node),
            _ => false,
        }
    }

    fn owns_endpoint(&self, endpoint: &Value) -> bool {
        let project = text(endpoint, "/metadata/project").unwrap_or_default();
        let vm = text(endpoint, "/specification/vm").unwrap_or_default();
        names_node(endpoint, "/specification/node", &self.node) && self.hosts_vm(project, vm)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::registry::kinds;

    fn node(name: &str) -> NodeName {
        NodeName::new(name).unwrap()
    }

    fn vm(name: &str, on: Option<&str>, volumes: &[&str]) -> Value {
        json!({
            "metadata": {"name": name, "project": "project-a", "generation": 1, "observed_generation": 1},
            "specification": {"cpu_count": 1, "volumes": volumes},
            "status": {"phase": "running", "node": on},
        })
    }

    fn slice_with(vms: &[Value]) -> NodeSlice {
        let mut slice = NodeSlice::new(node("node-1"));
        slice.learn_vms(vms);
        slice
    }

    fn endpoint(vm: &str, on: &str) -> Value {
        json!({
            "metadata": {"name": format!("endpoint-{vm}"), "project": "project-a"},
            "specification": {"vm": vm, "node": on, "network": "default"},
        })
    }

    fn compute() -> BTreeSet<NodeRole> {
        BTreeSet::from([NodeRole::Compute])
    }

    #[test]
    fn a_node_receives_its_own_objects_and_the_cluster_wide_ones() {
        let vms = [vm("web-1", Some("node-1"), &["data"]), vm("web-2", Some("node-2"), &["other"])];
        let slice = slice_with(&vms);
        assert!(slice.includes("Node", &json!({"metadata": {"name": "node-1"}})));
        assert!(!slice.includes("Node", &json!({"metadata": {"name": "node-2"}})));
        assert!(slice.includes("Vm", &vms[0]));
        assert!(!slice.includes("Vm", &vms[1]));
        assert!(slice.includes("Volume", &json!({"metadata": {"name": "data", "project": "project-a"}})));
        assert!(!slice.includes("Volume", &json!({"metadata": {"name": "other", "project": "project-a"}})));
        assert!(slice.includes(
            "Volume",
            &json!({"metadata": {"name": "scratch", "project": "project-b"}, "status": {"provisioned": {"node": "node-1"}}})
        ));
        assert!(slice.includes("NetworkEndpoint", &endpoint("web-2", "node-2")));
        assert!(
            !slice.includes(
                "NetworkEndpoint",
                &json!({"metadata": {"name": "endpoint", "project": "project-b"}})
            )
        );
        assert!(slice.includes(
            "FloatingIp",
            &json!({"metadata": {"name": "ip"}, "status": {"binding": {"node": "node-1"}}})
        ));
        assert!(!slice.includes("FloatingIp", &json!({"metadata": {"name": "ip"}, "status": {}})));
        for kind in CLUSTER_WIDE_KINDS {
            assert!(slice.includes(kind, &json!({"metadata": {"name": "anything"}})));
        }
        assert!(!slice.includes("CertificateRevocation", &json!({"metadata": {"name": "revoked"}})));
    }

    #[test]
    fn a_node_writes_only_the_status_of_its_own_vms() {
        let hosted = vm("web-1", Some("node-1"), &[]);
        let slice = slice_with(std::slice::from_ref(&hosted));
        let mut reported = hosted.clone();
        reported["status"]["phase"] = json!("stopped");
        reported["metadata"]["observed_generation"] = json!(2);
        assert!(slice.may_write(&compute(), "Vm", Some(&hosted), Some(&reported)));
        let mut resized = hosted.clone();
        resized["specification"]["cpu_count"] = json!(8);
        assert!(!slice.may_write(&compute(), "Vm", Some(&hosted), Some(&resized)));
        let mut handed_over = hosted.clone();
        handed_over["status"]["node"] = json!("node-2");
        assert!(!slice.may_write(&compute(), "Vm", Some(&hosted), Some(&handed_over)));
        let foreign = vm("web-2", Some("node-2"), &[]);
        let mut hijacked = foreign.clone();
        hijacked["status"]["node"] = json!("node-1");
        assert!(!slice.may_write(&compute(), "Vm", Some(&foreign), Some(&hijacked)));
        assert!(!slice.may_write(&compute(), "Vm", None, Some(&hosted)));
        assert!(!slice.may_write(&compute(), "Vm", Some(&hosted), None));
        let mut deleting = hosted;
        deleting["metadata"]["is_deleting"] = json!(true);
        assert!(slice.may_write(&compute(), "Vm", Some(&deleting), None));
    }

    #[test]
    fn a_node_registers_itself_only_with_the_roles_its_certificate_names() {
        let slice = slice_with(&[]);
        let registration = |name: &str, roles: Value| json!({"metadata": {"name": name}, "specification": {"roles": roles}, "status": {}});
        assert!(slice.may_write(&compute(), "Node", None, Some(&registration("node-1", json!(["compute"])))));
        assert!(!slice.may_write(
            &compute(),
            "Node",
            None,
            Some(&registration("node-1", json!(["core", "compute"])))
        ));
        assert!(!slice.may_write(
            &compute(),
            "Node",
            None,
            Some(&registration("node-2", json!(["compute"])))
        ));
        let registered = registration("node-1", json!(["compute"]));
        let mut uncordoned = registered.clone();
        uncordoned["specification"]["is_cordoned"] = json!(false);
        assert!(!slice.may_write(&compute(), "Node", Some(&registered), Some(&uncordoned)));
    }

    #[test]
    fn a_node_changes_only_its_own_entry_in_an_image_cache_list() {
        let slice = slice_with(&[]);
        let image = |cached_on: Value| json!({"metadata": {"name": "busybox"}, "specification": {}, "status": {"cached_on": cached_on}});
        let before = image(json!(["node-2"]));
        assert!(slice.may_write(
            &compute(),
            "Image",
            Some(&before),
            Some(&image(json!(["node-1", "node-2"])))
        ));
        assert!(!slice.may_write(&compute(), "Image", Some(&before), Some(&image(json!([])))));
    }

    #[test]
    fn a_node_publishes_endpoints_only_for_vms_it_hosts() {
        let slice = slice_with(&[vm("web-1", Some("node-1"), &[])]);
        assert!(slice.may_write(&compute(), "NetworkEndpoint", None, Some(&endpoint("web-1", "node-1"))));
        assert!(!slice.may_write(&compute(), "NetworkEndpoint", None, Some(&endpoint("web-1", "node-2"))));
        assert!(!slice.may_write(&compute(), "NetworkEndpoint", None, Some(&endpoint("web-9", "node-1"))));
        assert!(slice.may_write(&compute(), "NetworkEndpoint", Some(&endpoint("gone", "node-1")), None));
        let mut withdrawn = endpoint("gone", "node-1");
        withdrawn["metadata"]["is_deleting"] = json!(true);
        assert!(slice.may_write(
            &compute(),
            "NetworkEndpoint",
            Some(&endpoint("gone", "node-1")),
            Some(&withdrawn)
        ));
        let mut foreign_withdrawal = endpoint("web-2", "node-2");
        foreign_withdrawal["metadata"]["is_deleting"] = json!(true);
        assert!(!slice.may_write(
            &compute(),
            "NetworkEndpoint",
            Some(&endpoint("web-2", "node-2")),
            Some(&foreign_withdrawal)
        ));
        assert!(!slice.may_write(&compute(), "NetworkEndpoint", Some(&endpoint("web-2", "node-2")), None));
    }

    #[test]
    fn a_node_keeps_seeing_its_endpoints_after_its_last_vm_of_the_project_is_gone() {
        let slice = slice_with(&[]);
        let mut endpoint_in_other_project = endpoint("gone", "node-1");
        endpoint_in_other_project["metadata"]["project"] = json!("project-c");
        assert!(slice.includes("NetworkEndpoint", &endpoint_in_other_project));
        let mut foreign_endpoint = endpoint("web-2", "node-2");
        foreign_endpoint["metadata"]["project"] = json!("project-c");
        assert!(!slice.includes("NetworkEndpoint", &foreign_endpoint));
    }

    proptest! {
        #[test]
        fn no_kind_outside_the_rules_is_ever_written(index in 0..kinds().len(), name in "[a-z]{1,8}") {
            let kind = kinds()[index].kind;
            let slice = slice_with(&[]);
            let object = json!({"metadata": {"name": name, "project": "project-a"}, "specification": {}, "status": {}});
            let writable = ["Node", "NetworkEndpoint"];
            if !writable.contains(&kind) {
                prop_assert!(!slice.may_write(&compute(), kind, None, Some(&object)));
            }
            prop_assert!(!slice.may_write(&compute(), kind, None, None));
        }

        #[test]
        fn objects_of_other_nodes_are_never_included(index in 0..kinds().len(), other in "node-[2-9]") {
            let kind = kinds()[index].kind;
            let slice = slice_with(&[]);
            let object = json!({
                "metadata": {"name": other.clone(), "project": "project-z"},
                "specification": {"node": other.clone()},
                "status": {"node": other.clone(), "binding": {"node": other.clone()}, "provisioned": {"node": other}},
            });
            prop_assert_eq!(slice.includes(kind, &object), CLUSTER_WIDE_KINDS.contains(&kind));
        }
    }
}
