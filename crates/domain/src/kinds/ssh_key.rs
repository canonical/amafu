use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authorized_key::{AuthorizedKey, KeyError};
use crate::name::{ObjectName, label_fragment};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_GROUP_KEYS: usize = 64;
pub const LONGEST_ACCOUNT_NAME: usize = 64;
const MAXIMUM_STEM: usize = 56;
const LAST_IMPORT_INDEX: usize = 999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKey;

impl Resource for SshKey {
    const KIND: &'static str = "SshKey";
    const PLURAL_NAME: &'static str = "ssh-keys";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = SshKeySpecification;
    type Status = SshKeyStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum KeyOrigin {
    #[default]
    Pasted,
    Generated,
    Github {
        account: String,
    },
    Launchpad {
        account: String,
    },
}

impl KeyOrigin {
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        match self {
            Self::Github { account } | Self::Launchpad { account } => Some(account),
            Self::Pasted | Self::Generated => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeySource {
    Github,
    Launchpad,
}

impl KeySource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Launchpad => "launchpad",
        }
    }

    #[must_use]
    pub fn origin(self, account: &str) -> KeyOrigin {
        let account = account.to_owned();
        match self {
            Self::Github => KeyOrigin::Github { account },
            Self::Launchpad => KeyOrigin::Launchpad { account },
        }
    }

    #[must_use]
    pub fn keys_url(self, account: &str) -> String {
        match self {
            Self::Github => format!("https://github.com/{account}.keys"),
            Self::Launchpad => format!("https://launchpad.net/~{account}/+sshkeys"),
        }
    }
}

impl std::str::FromStr for KeySource {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "github" | "gh" => Ok(Self::Github),
            "launchpad" | "lp" => Ok(Self::Launchpad),
            _ => Err(format!("{text:?} is not a key source; use github or launchpad")),
        }
    }
}

impl KeyOrigin {
    #[must_use]
    pub fn source(&self) -> Option<KeySource> {
        match self {
            Self::Github { .. } => Some(KeySource::Github),
            Self::Launchpad { .. } => Some(KeySource::Launchpad),
            Self::Pasted | Self::Generated => None,
        }
    }
}

#[must_use]
pub fn is_account_name(account: &str) -> bool {
    account.len() <= LONGEST_ACCOUNT_NAME
        && account.chars().next().is_some_and(|first| first.is_ascii_alphanumeric())
        && account
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '+'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredKey {
    pub public_key: String,
    pub fingerprint: String,
    pub algorithm: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLookup {
    pub source: KeySource,
    pub account: String,
    pub keys: Vec<DiscoveredKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPlan {
    pub additions: Vec<(ObjectName, SshKeySpecification)>,
    pub unchanged: Vec<ObjectName>,
    pub withdrawn: Vec<ObjectName>,
}

impl KeyLookup {
    #[must_use]
    pub fn import_plan(&self, existing: &[(ObjectName, SshKeySpecification)]) -> ImportPlan {
        let origin = self.source.origin(&self.account);
        let fingerprint = |specification: &SshKeySpecification| {
            specification.authorized_key().map(|key| key.fingerprint()).unwrap_or_default()
        };
        let mut plan = ImportPlan::default();
        let mut taken: BTreeSet<ObjectName> = existing.iter().map(|(name, _)| name.clone()).collect();
        let stem = format!("{}-{}", self.source.as_str(), label_fragment(&self.account));
        let stem: String =
            stem.chars().take(MAXIMUM_STEM).collect::<String>().trim_end_matches('-').to_owned();
        for discovered in &self.keys {
            if let Some((name, _)) =
                existing.iter().find(|(_, known)| fingerprint(known) == discovered.fingerprint)
            {
                plan.unchanged.push(name.clone());
                continue;
            }
            let name = (1..=LAST_IMPORT_INDEX)
                .filter_map(|index| ObjectName::new(format!("{stem}-{index}")).ok())
                .find(|candidate| !taken.contains(candidate));
            if let Some(name) = name {
                taken.insert(name.clone());
                let specification =
                    SshKeySpecification { public_key: discovered.public_key.clone(), origin: origin.clone() };
                plan.additions.push((name, specification));
            }
        }
        plan.withdrawn = existing
            .iter()
            .filter(|(_, known)| known.origin == origin)
            .filter(|(_, known)| {
                !self.keys.iter().any(|discovered| discovered.fingerprint == fingerprint(known))
            })
            .map(|(name, _)| name.clone())
            .collect();
        plan
    }

    #[must_use]
    pub fn from_listing(source: KeySource, account: &str, listing: &str) -> Self {
        let mut lookup = Self { source, account: account.to_owned(), keys: Vec::new(), skipped: Vec::new() };
        for line in listing.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            match AuthorizedKey::parse(line) {
                Ok(key) if lookup.keys.iter().any(|known| known.fingerprint == key.fingerprint()) => {}
                Ok(key) => lookup.keys.push(DiscoveredKey {
                    fingerprint: key.fingerprint(),
                    algorithm: key.algorithm().to_owned(),
                    comment: key.comment().map(str::to_owned),
                    public_key: key.line(),
                }),
                Err(problem) => lookup.skipped.push(problem.to_string()),
            }
        }
        lookup
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshKeySpecification {
    pub public_key: String,
    #[serde(default)]
    pub origin: KeyOrigin,
}

impl SshKeySpecification {
    pub fn authorized_key(&self) -> Result<AuthorizedKey, KeyError> {
        AuthorizedKey::parse(&self.public_key)
    }
}

impl Validate for SshKeySpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        if let Err(problem) = self.authorized_key() {
            error.add("specification.public_key", problem.to_string());
        }
        if let Some(account) = self.origin.account() {
            error.check(is_account_name(account), "specification.origin.account", "must be an account name");
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.public_key == previous.public_key,
            "specification.public_key",
            "cannot change; add a new key",
        );
        error.check(self.origin == previous.origin, "specification.origin", "cannot change");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SshKeyStatus {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKeyGroup;

impl Resource for SshKeyGroup {
    const KIND: &'static str = "SshKeyGroup";
    const PLURAL_NAME: &'static str = "ssh-key-groups";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = SshKeyGroupSpecification;
    type Status = SshKeyGroupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshKeyGroupSpecification {
    #[serde(default)]
    pub keys: Vec<ObjectName>,
}

impl Validate for SshKeyGroupSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.keys.len() <= MAXIMUM_GROUP_KEYS, "specification.keys", "must hold at most 64 keys");
        let unique: BTreeSet<&ObjectName> = self.keys.iter().collect();
        error.check(unique.len() == self.keys.len(), "specification.keys", "must not repeat a key");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SshKeyGroupStatus {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SshKeyReference {
    Key { name: ObjectName },
    Group { name: ObjectName },
}

impl SshKeyReference {
    #[must_use]
    pub fn name(&self) -> &ObjectName {
        match self {
            Self::Key { name } | Self::Group { name } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantedKey {
    pub fingerprint: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("there is no SSH key {0}")]
    MissingKey(ObjectName),
    #[error("there is no SSH key group {0}")]
    MissingGroup(ObjectName),
    #[error("SSH key {name} is invalid: {problem}")]
    InvalidKey { name: ObjectName, problem: KeyError },
}

pub fn granted_keys<'a>(
    references: &[SshKeyReference],
    key: impl Fn(&ObjectName) -> Option<&'a SshKeySpecification>,
    group: impl Fn(&ObjectName) -> Option<&'a SshKeyGroupSpecification>,
) -> Result<Vec<GrantedKey>, GrantError> {
    let mut names = Vec::new();
    for reference in references {
        match reference {
            SshKeyReference::Key { name } => names.push(name.clone()),
            SshKeyReference::Group { name } => {
                names.extend(
                    group(name).ok_or_else(|| GrantError::MissingGroup(name.clone()))?.keys.iter().cloned(),
                );
            }
        }
    }
    let mut granted: Vec<GrantedKey> = Vec::new();
    for name in names {
        let specification = key(&name).ok_or_else(|| GrantError::MissingKey(name.clone()))?;
        let parsed = specification
            .authorized_key()
            .map_err(|problem| GrantError::InvalidKey { name: name.clone(), problem })?;
        let fingerprint = parsed.fingerprint();
        if !granted.iter().any(|known| known.fingerprint == fingerprint) {
            granted.push(GrantedKey { fingerprint, public_key: parsed.line() });
        }
    }
    Ok(granted)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const ALICE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOpKbGPinFIKvvVQexMuxfmVR3auvr57kkIe6mkURtIs alice";

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn key(public_key: &str) -> SshKeySpecification {
        SshKeySpecification { public_key: public_key.to_owned(), origin: KeyOrigin::Pasted }
    }

    #[test]
    fn a_key_must_be_a_public_key_and_cannot_be_swapped_afterwards() {
        assert!(key(ALICE).validate().is_ok());
        assert!(key("-----BEGIN OPENSSH PRIVATE KEY-----").validate().is_err());
        let other = key(&ALICE.replace("alice", "bob"));
        assert!(other.validate_update(&key(ALICE)).is_err());
        let imported = SshKeySpecification {
            public_key: ALICE.to_owned(),
            origin: KeyOrigin::Github { account: String::from("a/b") },
        };
        assert!(imported.validate().is_err());
    }

    #[test]
    fn a_listing_keeps_good_keys_once_and_says_why_others_were_skipped() {
        let listing = format!("{ALICE}\n\n{ALICE}\nssh-dss AAAA old\n");
        let lookup = KeyLookup::from_listing(KeySource::Github, "alice", &listing);
        assert_eq!(lookup.keys.len(), 1);
        assert_eq!(lookup.keys[0].comment.as_deref(), Some("alice"));
        assert_eq!(lookup.skipped.len(), 1);
        assert!(!is_account_name("../x") && !is_account_name("") && !is_account_name("-a"));
        assert!(is_account_name("alice.b+c"));
    }

    #[test]
    fn importing_again_adds_new_keys_and_names_withdrawn_ones() {
        let laptop = ALICE.replace(" alice", " laptop");
        let lookup = KeyLookup::from_listing(KeySource::Github, "Alice_B", &format!("{ALICE}\n"));
        let first = lookup.import_plan(&[]);
        assert_eq!(first.additions.len(), 1);
        assert_eq!(first.additions[0].0, name("github-alice-b-1"));
        assert_eq!(first.additions[0].1.origin, KeySource::Github.origin("Alice_B"));
        let rotated_listing = format!("{}\n", laptop.replace("AAAAIOpK", "AAAAIOpL"));
        let rotated = KeyLookup::from_listing(KeySource::Github, "Alice_B", &rotated_listing);
        let second = rotated.import_plan(&first.additions);
        assert_eq!(second.additions[0].0, name("github-alice-b-2"));
        assert_eq!(second.withdrawn, [name("github-alice-b-1")]);
        assert_eq!(lookup.import_plan(&first.additions).unchanged, [name("github-alice-b-1")]);
    }

    #[test]
    fn a_group_never_repeats_a_key() {
        let group = SshKeyGroupSpecification { keys: vec![name("alice"), name("alice")] };
        assert!(group.validate().is_err());
    }

    #[test]
    fn granted_keys_follow_groups_and_appear_once() {
        let keys = BTreeMap::from([
            (name("alice"), key(ALICE)),
            (name("alice-laptop"), key(&ALICE.replace(" alice", " laptop"))),
        ]);
        let groups = BTreeMap::from([(
            name("team"),
            SshKeyGroupSpecification { keys: vec![name("alice"), name("alice-laptop")] },
        )]);
        let references =
            [SshKeyReference::Group { name: name("team") }, SshKeyReference::Key { name: name("alice") }];
        let granted = granted_keys(&references, |name| keys.get(name), |name| groups.get(name)).unwrap();
        assert_eq!(granted.len(), 1);
        assert_eq!(granted[0].public_key, ALICE);
        let missing = granted_keys(
            &[SshKeyReference::Group { name: name("nobody") }],
            |name| keys.get(name),
            |name| groups.get(name),
        );
        assert_eq!(missing, Err(GrantError::MissingGroup(name("nobody"))));
    }
}
