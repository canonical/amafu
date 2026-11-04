use amafu_domain::access::Role::{self, Admin, Member, Operator, Viewer};
use amafu_domain::access::Verb::{self, Create, Delete, Get, List, Update, Watch};

struct Row {
    role: Role,
    kind: &'static str,
    in_own_project: &'static [Verb],
    in_other_project: &'static [Verb],
}

const EVERY_VERB: &[Verb] = &[Get, List, Watch, Create, Update, Delete];
const READ: &[Verb] = &[Get, List, Watch];
const READ_AND_UPDATE: &[Verb] = &[Get, List, Watch, Update];
const NOTHING: &[Verb] = &[];

const TABLE: &[Row] = &[
    Row { role: Admin, kind: "ExternalNetwork", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "ExternalNetwork", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Member, kind: "ExternalNetwork", in_own_project: READ, in_other_project: READ },
    Row { role: Viewer, kind: "ExternalNetwork", in_own_project: READ, in_other_project: READ },
    Row {
        role: Admin,
        kind: "CertificateRevocation",
        in_own_project: EVERY_VERB,
        in_other_project: EVERY_VERB,
    },
    Row { role: Operator, kind: "CertificateRevocation", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Member, kind: "CertificateRevocation", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Viewer, kind: "CertificateRevocation", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Admin, kind: "NodeLease", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "NodeLease", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Member, kind: "NodeLease", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Viewer, kind: "NodeLease", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Admin, kind: "Operation", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Operation", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Member, kind: "Operation", in_own_project: READ, in_other_project: READ },
    Row { role: Viewer, kind: "Operation", in_own_project: READ, in_other_project: READ },
    Row { role: Admin, kind: "NetworkEndpoint", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "NetworkEndpoint", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Member, kind: "NetworkEndpoint", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Viewer, kind: "NetworkEndpoint", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "FloatingIp", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "FloatingIp", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "FloatingIp", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "FloatingIp", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "StorageBackend", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "StorageBackend", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Member, kind: "StorageBackend", in_own_project: READ, in_other_project: READ },
    Row { role: Viewer, kind: "StorageBackend", in_own_project: READ, in_other_project: READ },
    Row { role: Admin, kind: "Node", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Node", in_own_project: READ_AND_UPDATE, in_other_project: READ_AND_UPDATE },
    Row { role: Member, kind: "Node", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Node", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Admin, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Vm", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "Image", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Image", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "Image", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Image", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "Volume", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Volume", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "Volume", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Volume", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "Network", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Network", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "Network", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Network", in_own_project: READ, in_other_project: NOTHING },
    Row { role: Admin, kind: "SecurityGroup", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "SecurityGroup", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "SecurityGroup", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "SecurityGroup", in_own_project: READ, in_other_project: NOTHING },
];

#[must_use]
pub fn expected(role: Role, verb: Verb, kind: &str, is_own_project: bool) -> Option<bool> {
    let row = TABLE.iter().find(|row| row.role == role && row.kind == kind)?;
    let allowed = if is_own_project { row.in_own_project } else { row.in_other_project };
    Some(allowed.contains(&verb))
}

#[must_use]
pub fn covered_kinds() -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = TABLE.iter().map(|row| row.kind).collect();
    kinds.dedup();
    kinds
}

#[cfg(test)]
mod tests {
    use amafu_domain::registry::kinds;

    use super::*;

    #[test]
    fn every_registered_kind_and_role_has_an_expectation() {
        for kind in kinds() {
            for role in Role::ALL {
                assert!(expected(role, Get, kind.kind, true).is_some(), "{role} {}", kind.kind);
            }
        }
    }
}
