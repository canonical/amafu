use amafu_domain::ProjectName;
use amafu_domain::platform_access::{ROLES, kubernetes_cluster_role, kubernetes_group};
use serde_json::{Value, json};

#[must_use]
pub fn manifests(project: &ProjectName) -> Vec<Value> {
    ROLES
        .iter()
        .map(|role| {
            let group = kubernetes_group(project, *role);
            json!({
                "apiVersion": "rbac.authorization.k8s.io/v1",
                "kind": "ClusterRoleBinding",
                "metadata": {"name": group.replace(':', "-")},
                "roleRef": {
                    "apiGroup": "rbac.authorization.k8s.io",
                    "kind": "ClusterRole",
                    "name": kubernetes_cluster_role(*role),
                },
                "subjects": [{"apiGroup": "rbac.authorization.k8s.io", "kind": "Group", "name": group}],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_amafu_role_of_the_project_is_bound_to_its_cluster_role() {
        let manifests = manifests(&ProjectName::new("web-team").unwrap());
        assert_eq!(manifests.len(), 4);
        let member =
            manifests.iter().find(|binding| binding["metadata"]["name"] == "amafu-web-team-member").unwrap();
        assert_eq!(member["roleRef"]["name"], "edit");
        assert_eq!(member["subjects"][0]["name"], "amafu:web-team:member");
    }
}
