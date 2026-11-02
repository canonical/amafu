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
    Row { role: Admin, kind: "Node", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Node", in_own_project: READ_AND_UPDATE, in_other_project: READ_AND_UPDATE },
    Row { role: Member, kind: "Node", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Node", in_own_project: NOTHING, in_other_project: NOTHING },
    Row { role: Admin, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: EVERY_VERB },
    Row { role: Operator, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Member, kind: "Vm", in_own_project: EVERY_VERB, in_other_project: NOTHING },
    Row { role: Viewer, kind: "Vm", in_own_project: READ, in_other_project: NOTHING },
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
