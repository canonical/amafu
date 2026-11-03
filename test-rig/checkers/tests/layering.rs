use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const ALLOWED: &[(&str, &[&str])] = &[
    ("amafu-domain", &[]),
    ("amafu-interfaces", &["amafu-domain"]),
    ("amafu-reconciler", &["amafu-domain", "amafu-interfaces"]),
    ("amafu-scheduler", &["amafu-domain"]),
    ("amafu-controllers", &["amafu-domain", "amafu-interfaces", "amafu-reconciler", "amafu-scheduler"]),
    ("amafu-state", &["amafu-domain", "amafu-interfaces"]),
    ("amafu-hypervisor", &["amafu-domain", "amafu-guest-agent", "amafu-interfaces"]),
    ("amafu-network", &["amafu-domain", "amafu-interfaces"]),
    ("amafu-storage", &["amafu-domain", "amafu-interfaces"]),
    ("amafu-guest-agent", &["amafu-domain", "amafu-interfaces", "amafu-network", "amafu-system"]),
    ("amafu-system", &[]),
    ("amafu-api", &["amafu-domain", "amafu-interfaces"]),
    ("amafu-command-line", &["amafu-api", "amafu-domain"]),
    (
        "amafu-node",
        &[
            "amafu-api",
            "amafu-controllers",
            "amafu-domain",
            "amafu-hypervisor",
            "amafu-interfaces",
            "amafu-network",
            "amafu-reconciler",
            "amafu-state",
            "amafu-storage",
            "amafu-system",
        ],
    ),
];

fn crates_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates")
}

fn amafu_dependencies(manifest: &toml::Table) -> BTreeSet<String> {
    manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .map(|dependencies| dependencies.keys().filter(|name| name.starts_with("amafu-")).cloned().collect())
        .unwrap_or_default()
}

fn product_crates() -> Result<BTreeMap<String, BTreeSet<String>>, Box<dyn std::error::Error>> {
    let mut crates = BTreeMap::new();
    for entry in std::fs::read_dir(crates_directory())? {
        let manifest_path = entry?.path().join("Cargo.toml");
        let Ok(contents) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let manifest: toml::Table = contents.parse()?;
        let name = manifest["package"]["name"].as_str().ok_or("a package has no name")?.to_owned();
        crates.insert(name, amafu_dependencies(&manifest));
    }
    Ok(crates)
}

#[test]
fn every_product_crate_depends_only_on_the_layers_it_may_use() {
    let allowed: BTreeMap<&str, BTreeSet<String>> = ALLOWED
        .iter()
        .map(|(name, dependencies)| {
            (*name, dependencies.iter().map(|dependency| (*dependency).to_owned()).collect())
        })
        .collect();
    for (name, dependencies) in product_crates().unwrap() {
        let permitted = allowed.get(name.as_str()).unwrap_or_else(|| panic!("{name} has no layering rule"));
        let forbidden: Vec<&String> = dependencies.difference(permitted).collect();
        assert!(forbidden.is_empty(), "{name} must not depend on {forbidden:?}");
    }
}
