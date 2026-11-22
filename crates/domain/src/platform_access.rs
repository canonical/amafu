use serde_json::{Value, json};

use crate::access::Role;
use crate::name::ProjectName;

pub const ISSUER: &str = "https://amafu.internal";
pub const JWKS_PATH: &str = "/.well-known/jwks.json";
pub const SERVICE_SUBJECT: &str = "amafu@amafu";
pub const ROLES: [Role; 4] = [Role::Viewer, Role::Member, Role::Operator, Role::Admin];

#[must_use]
pub fn kubernetes_group(project: &ProjectName, role: Role) -> String {
    format!("amafu:{project}:{}", role_name(role))
}

#[must_use]
pub fn kubernetes_cluster_role(role: Role) -> &'static str {
    match role {
        Role::Admin | Role::Operator => "cluster-admin",
        Role::Member => "edit",
        Role::Viewer => "view",
    }
}

#[must_use]
pub fn juju_access(role: Role, controller_uuid: &str, model_uuids: &[String]) -> Value {
    let (controller, model) = match role {
        Role::Admin => ("superuser", "admin"),
        Role::Operator => ("login", "admin"),
        Role::Member => ("login", "write"),
        Role::Viewer => ("login", "read"),
    };
    let mut access = serde_json::Map::new();
    access.insert(format!("controller-{controller_uuid}"), json!(controller));
    for model_uuid in model_uuids {
        access.insert(format!("model-{model_uuid}"), json!(model));
    }
    Value::Object(access)
}

#[must_use]
pub fn platform_user(principal: &str) -> String {
    format!("{}@amafu", principal.split('@').next().unwrap_or(principal))
}

#[must_use]
pub fn juju_user(principal: &str) -> String {
    format!("user-{}", platform_user(principal))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Operator => "operator",
        Role::Member => "member",
        Role::Viewer => "viewer",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_role_reaches_kubernetes_through_a_group_bound_to_a_cluster_role() {
        let project = ProjectName::new("web-team").unwrap();
        assert_eq!(kubernetes_group(&project, Role::Member), "amafu:web-team:member");
        assert_eq!(kubernetes_cluster_role(Role::Member), "edit");
        assert_eq!(kubernetes_cluster_role(Role::Viewer), "view");
        assert_eq!(kubernetes_cluster_role(Role::Operator), "cluster-admin");
    }

    #[test]
    fn juju_access_names_the_controller_and_every_model() {
        let access = juju_access(Role::Member, "c-1", &[String::from("m-1"), String::from("m-2")]);
        assert_eq!(access["controller-c-1"], "login");
        assert_eq!(access["model-m-2"], "write");
        assert_eq!(juju_access(Role::Admin, "c-1", &[])["controller-c-1"], "superuser");
    }

    #[test]
    fn a_juju_user_is_the_principal_in_the_amafu_domain() {
        assert_eq!(juju_user("alice"), "user-alice@amafu");
        assert_eq!(platform_user("alice@example.com"), "alice@amafu");
    }
}
