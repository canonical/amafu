use serde_json::Value;

const MIRROR_ANNOTATION: &str = "kubernetes.io/config.mirror";

pub fn collection_path(manifest: &Value) -> Option<String> {
    let api_version = manifest["apiVersion"].as_str()?;
    let kind = manifest["kind"].as_str()?;
    let group = if api_version.contains('/') {
        format!("/apis/{api_version}")
    } else {
        format!("/api/{api_version}")
    };
    let namespace = manifest["metadata"]["namespace"]
        .as_str()
        .map(|namespace| format!("/namespaces/{namespace}"))
        .unwrap_or_default();
    Some(format!("{group}{namespace}/{}", plural(kind)))
}

pub fn object_path(manifest: &Value) -> Option<String> {
    let name = manifest["metadata"]["name"].as_str()?;
    Some(format!("{}/{name}", collection_path(manifest)?))
}

fn plural(kind: &str) -> String {
    let lower = kind.to_lowercase();
    if let Some(stem) = lower.strip_suffix('y') {
        format!("{stem}ies")
    } else if lower.ends_with('s') {
        format!("{lower}es")
    } else {
        format!("{lower}s")
    }
}

pub fn is_node_ready(node: &Value) -> bool {
    node["status"]["conditions"].as_array().is_some_and(|conditions| {
        conditions.iter().any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
    })
}

pub fn is_unschedulable(node: &Value) -> bool {
    node["spec"]["unschedulable"].as_bool().unwrap_or(false)
}

pub fn is_evictable(pod: &Value) -> bool {
    let is_daemon = pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| owners.iter().any(|owner| owner["kind"] == "DaemonSet"));
    let is_mirror = pod["metadata"]["annotations"].get(MIRROR_ANNOTATION).is_some();
    let is_done = matches!(pod["status"]["phase"].as_str(), Some("Succeeded" | "Failed"));
    !is_daemon && !is_mirror && !is_done
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn paths_follow_the_group_kind_and_namespace() {
        let binding = json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding", "metadata": {"name": "amafu"}});
        assert_eq!(
            object_path(&binding).unwrap(),
            "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/amafu"
        );
        let account = json!({"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": "juju", "namespace": "kube-system"}});
        assert_eq!(object_path(&account).unwrap(), "/api/v1/namespaces/kube-system/serviceaccounts/juju");
        let class =
            json!({"apiVersion": "storage.k8s.io/v1", "kind": "StorageClass", "metadata": {"name": "local"}});
        assert_eq!(collection_path(&class).unwrap(), "/apis/storage.k8s.io/v1/storageclasses");
        let policy =
            json!({"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy", "metadata": {"name": "x"}});
        assert_eq!(collection_path(&policy).unwrap(), "/apis/networking.k8s.io/v1/networkpolicies");
    }

    #[test]
    fn daemon_mirror_and_finished_pods_stay_during_a_drain() {
        let ordinary = json!({"metadata": {"ownerReferences": [{"kind": "ReplicaSet"}]}, "status": {"phase": "Running"}});
        let daemon =
            json!({"metadata": {"ownerReferences": [{"kind": "DaemonSet"}]}, "status": {"phase": "Running"}});
        let mirror =
            json!({"metadata": {"annotations": {MIRROR_ANNOTATION: "x"}}, "status": {"phase": "Running"}});
        let finished = json!({"metadata": {}, "status": {"phase": "Succeeded"}});
        assert!(is_evictable(&ordinary));
        assert!(!is_evictable(&daemon));
        assert!(!is_evictable(&mirror));
        assert!(!is_evictable(&finished));
    }

    #[test]
    fn a_node_is_ready_when_its_ready_condition_is_true() {
        assert!(is_node_ready(&json!({"status": {"conditions": [{"type": "Ready", "status": "True"}]}})));
        assert!(!is_node_ready(&json!({"status": {"conditions": [{"type": "Ready", "status": "False"}]}})));
        assert!(is_unschedulable(&json!({"spec": {"unschedulable": true}})));
    }
}
