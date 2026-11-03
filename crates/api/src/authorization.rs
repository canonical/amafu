use amafu_domain::access::{ClusterPolicy, ProjectPolicy, Role, Verb};
use amafu_domain::registry::KindDescriptor;
use amafu_domain::{ProjectName, Scope};

use crate::identity::Principal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'request> {
    Cluster,
    Project(&'request ProjectName),
    EveryProject,
}

#[must_use]
pub fn is_allowed(principal: &Principal, verb: Verb, kind: &KindDescriptor, target: Target<'_>) -> bool {
    if principal.role == Role::Admin {
        return true;
    }
    match (kind.scope, target) {
        (Scope::Cluster, Target::Cluster) => match (kind.cluster_policy, principal.role) {
            (ClusterPolicy::ManagedByNodes, Role::Operator) => verb.is_read() || verb == Verb::Update,
            (ClusterPolicy::SharedInfrastructure, Role::Operator) => true,
            (ClusterPolicy::SharedInfrastructure, Role::Member | Role::Viewer) => verb.is_read(),
            _ => false,
        },
        (Scope::Project, Target::Project(project)) if *project == principal.project => {
            match (kind.project_policy, principal.role) {
                (ProjectPolicy::ManagedByUsers, Role::Admin | Role::Operator | Role::Member) => true,
                (ProjectPolicy::ManagedByUsers | ProjectPolicy::ManagedByNodes, _) => verb.is_read(),
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::registry::kind_named;

    use super::*;

    fn principal(role: Role) -> Principal {
        Principal { name: String::from("someone"), role, project: ProjectName::new("project-a").unwrap() }
    }

    #[test]
    fn listing_every_project_is_reserved_to_admins() {
        let vm = kind_named("Vm").unwrap();
        assert!(is_allowed(&principal(Role::Admin), Verb::List, &vm, Target::EveryProject));
        for role in [Role::Operator, Role::Member, Role::Viewer] {
            assert!(!is_allowed(&principal(role), Verb::List, &vm, Target::EveryProject), "{role}");
        }
    }

    #[test]
    fn users_only_read_what_nodes_manage_in_their_project() {
        let endpoint = kind_named("NetworkEndpoint").unwrap();
        let project = ProjectName::new("project-a").unwrap();
        for role in [Role::Operator, Role::Member, Role::Viewer] {
            assert!(is_allowed(&principal(role), Verb::Get, &endpoint, Target::Project(&project)), "{role}");
            assert!(
                !is_allowed(&principal(role), Verb::Create, &endpoint, Target::Project(&project)),
                "{role}"
            );
        }
    }

    #[test]
    fn only_admins_see_certificate_revocations() {
        let revocation = kind_named("CertificateRevocation").unwrap();
        assert!(is_allowed(&principal(Role::Admin), Verb::Create, &revocation, Target::Cluster));
        for role in [Role::Operator, Role::Member, Role::Viewer] {
            assert!(!is_allowed(&principal(role), Verb::Get, &revocation, Target::Cluster), "{role}");
        }
    }

    #[test]
    fn a_project_target_on_a_cluster_kind_is_refused() {
        let node = kind_named("Node").unwrap();
        let project = ProjectName::new("project-a").unwrap();
        assert!(!is_allowed(&principal(Role::Operator), Verb::Get, &node, Target::Project(&project)));
    }
}
