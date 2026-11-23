use std::collections::BTreeMap;

pub const KUBERNETES_CLUSTER: &str = "amafu.io/kubernetes-cluster";
pub const JUJU_CONTROLLER: &str = "amafu.io/juju-controller";
pub const ROOT_OF: &str = "amafu.io/root-of";

const ENVIRONMENT_LABELS: [&str; 2] = [KUBERNETES_CLUSTER, JUJU_CONTROLLER];

#[must_use]
pub fn owning_environment(labels: &BTreeMap<String, String>) -> Option<(&'static str, &str)> {
    ENVIRONMENT_LABELS.iter().find_map(|label| labels.get(*label).map(|owner| (*label, owner.as_str())))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSelector {
    required: BTreeMap<String, String>,
}

impl LabelSelector {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut required = BTreeMap::new();
        for term in text.split(',').map(str::trim).filter(|term| !term.is_empty()) {
            let (key, value) = term.split_once('=').ok_or_else(|| format!("{term:?} is not key=value"))?;
            if key.is_empty() {
                return Err(format!("{term:?} has no key"));
            }
            required.insert(key.to_owned(), value.to_owned());
        }
        Ok(Self { required })
    }

    #[must_use]
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.required.iter().all(|(key, value)| labels.get(key) == Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    #[test]
    fn a_selector_needs_every_pair() {
        let selector = LabelSelector::parse("team=data, tier=web").unwrap();
        assert!(selector.matches(&labels(&[("team", "data"), ("tier", "web"), ("x", "y")])));
        assert!(!selector.matches(&labels(&[("team", "data")])));
    }

    #[test]
    fn malformed_selectors_are_refused() {
        assert!(LabelSelector::parse("tier").is_err());
        assert!(LabelSelector::parse("=web").is_err());
        assert!(LabelSelector::parse("").unwrap().matches(&BTreeMap::new()));
    }

    #[test]
    fn the_owning_environment_is_read_from_its_label() {
        assert_eq!(
            owning_environment(&labels(&[(KUBERNETES_CLUSTER, "platform")])),
            Some((KUBERNETES_CLUSTER, "platform"))
        );
        assert_eq!(owning_environment(&labels(&[("tier", "web")])), None);
    }
}
