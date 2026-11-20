use std::collections::BTreeMap;

use amafu_domain::kinds::image::ImageSource;
use amafu_interfaces::{CatalogEntry, CatalogKind};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Stream {
    #[serde(default)]
    products: BTreeMap<String, Product>,
}

#[derive(Debug, Default, Deserialize)]
struct Product {
    arch: Option<String>,
    supported: Option<bool>,
    version: Option<String>,
    release_title: Option<String>,
    distro: Option<String>,
    os: Option<String>,
    release: Option<String>,
    variant: Option<String>,
    #[serde(default)]
    versions: BTreeMap<String, Version>,
}

#[derive(Debug, Default, Deserialize)]
struct Version {
    #[serde(default)]
    items: BTreeMap<String, Item>,
}

#[derive(Debug, Default, Deserialize)]
struct Item {
    path: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
}

pub const UBUNTU_STREAM: &str =
    "https://cloud-images.ubuntu.com/releases/streams/v1/com.ubuntu.cloud:released:download.json";
pub const UBUNTU_BASE: &str = "https://cloud-images.ubuntu.com/releases/";
pub const LXD_STREAM: &str = "https://images.lxd.canonical.com/streams/v1/images.json";
pub const LXD_BASE: &str = "https://images.lxd.canonical.com/";

const UBUNTU_DISK: &str = "disk1.img";
const LXD_DISK: &str = "disk.qcow2";
const LXD_VARIANT: &str = "cloud";

#[must_use]
pub fn object_name(id: &str) -> String {
    id.chars()
        .map(|character| if character.is_ascii_alphanumeric() { character.to_ascii_lowercase() } else { '-' })
        .collect()
}

fn latest_with<'a>(product: &'a Product, item: &str) -> Option<(&'a String, &'a Item)> {
    product
        .versions
        .iter()
        .filter_map(|(version, details)| details.items.get(item).map(|found| (version, found)))
        .max_by(|(left, _), (right, _)| left.cmp(right))
}

fn disk(base: &str, item: &Item) -> Option<ImageSource> {
    let url = format!("{base}{}", item.path.as_deref()?);
    let sha256 = format!("sha256:{}", item.sha256.as_deref()?).parse().ok()?;
    Some(ImageSource::Disk { url, sha256 })
}

fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map(|first| first.to_uppercase().chain(characters).collect()).unwrap_or_default()
}

#[must_use]
pub fn ubuntu_entries(stream: &Stream, architecture: &str) -> Vec<CatalogEntry> {
    stream
        .products
        .values()
        .filter(|product| product.arch.as_deref() == Some(architecture) && product.supported == Some(true))
        .filter_map(|product| {
            let version = product.version.as_deref()?;
            let (published, item) = latest_with(product, UBUNTU_DISK)?;
            let title = product.release_title.as_deref().unwrap_or(version);
            let id = format!("ubuntu/{version}");
            Some(CatalogEntry {
                name: object_name(&id),
                id,
                title: format!("Ubuntu {title}"),
                distribution: String::from("ubuntu"),
                kind: CatalogKind::VirtualMachine,
                source: disk(UBUNTU_BASE, item)?,
                size_bytes: item.size,
                published: Some(published.clone()),
                is_long_term: title.contains("LTS"),
            })
        })
        .collect()
}

#[must_use]
pub fn lxd_entries(stream: &Stream, architecture: &str) -> Vec<CatalogEntry> {
    stream
        .products
        .values()
        .filter(|product| {
            product.arch.as_deref() == Some(architecture) && product.variant.as_deref() == Some(LXD_VARIANT)
        })
        .filter(|product| product.distro.as_deref() != Some("ubuntu"))
        .filter_map(|product| {
            let distribution = product.distro.as_deref()?;
            let release = product.release.as_deref()?;
            let (published, item) = latest_with(product, LXD_DISK)?;
            let os = product.os.clone().unwrap_or_else(|| capitalised(distribution));
            let release_title = product.release_title.as_deref().unwrap_or(release);
            let id = format!("{distribution}/{release}");
            Some(CatalogEntry {
                name: object_name(&id),
                id,
                title: format!("{os} {release_title}"),
                distribution: distribution.to_owned(),
                kind: CatalogKind::VirtualMachine,
                source: disk(LXD_BASE, item)?,
                size_bytes: item.size,
                published: Some(published.clone()),
                is_long_term: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn stream(value: Value) -> Stream {
        serde_json::from_value(value).unwrap()
    }

    const DIGEST: &str = "04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94";

    #[test]
    fn supported_ubuntu_releases_offer_their_newest_disk() {
        let products = json!({"products": {
            "com.ubuntu.cloud:server:24.04:amd64": {
                "arch": "amd64", "supported": true, "version": "24.04", "release_title": "24.04 LTS",
                "versions": {
                    "20261001": {"items": {"disk1.img": {"path": "server/releases/noble/release-20261001/old.img", "sha256": DIGEST, "size": 1}}},
                    "20261017": {"items": {"disk1.img": {"path": "server/releases/noble/release-20261017/new.img", "sha256": DIGEST, "size": 621_284_352}}},
                    "20261020": {"items": {"root.tar.xz": {"path": "x", "sha256": DIGEST}}},
                },
            },
            "com.ubuntu.cloud:server:16.04:amd64": {"arch": "amd64", "supported": false, "version": "16.04", "versions": {}},
            "com.ubuntu.cloud:server:24.04:arm64": {"arch": "arm64", "supported": true, "version": "24.04", "versions": {}},
        }});
        let entries = ubuntu_entries(&stream(products), "amd64");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, "ubuntu/24.04");
        assert_eq!(entry.name, "ubuntu-24-04");
        assert_eq!(entry.title, "Ubuntu 24.04 LTS");
        assert!(entry.is_long_term);
        assert_eq!(entry.published.as_deref(), Some("20261017"));
        let ImageSource::Disk { url, .. } = &entry.source else { panic!() };
        assert_eq!(
            url,
            "https://cloud-images.ubuntu.com/releases/server/releases/noble/release-20261017/new.img"
        );
    }

    #[test]
    fn other_distributions_come_from_the_cloud_variant_of_the_lxd_images() {
        let products = json!({"products": {
            "debian:bookworm:amd64:cloud": {
                "arch": "amd64", "distro": "debian", "os": "Debian", "release": "bookworm", "release_title": "bookworm",
                "variant": "cloud",
                "versions": {"20261120_0336": {"items": {"disk.qcow2": {"path": "images/debian/bookworm/amd64/cloud/20261120_0336/disk.qcow2", "sha256": DIGEST, "size": 403_866_624}}}},
            },
            "debian:bookworm:amd64:default": {"arch": "amd64", "distro": "debian", "release": "bookworm", "variant": "default", "versions": {}},
            "alpine:3.24:amd64:cloud": {"arch": "amd64", "distro": "alpine", "release": "3.24", "variant": "cloud",
                "versions": {"20261120": {"items": {"rootfs.squashfs": {"path": "x", "sha256": DIGEST}}}}},
        }});
        let entries = lxd_entries(&stream(products), "amd64");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "debian/bookworm");
        assert_eq!(entries[0].name, "debian-bookworm");
        assert_eq!(entries[0].title, "Debian bookworm");
    }

    #[test]
    fn identifiers_become_object_names() {
        assert_eq!(object_name("opensuse/16.0"), "opensuse-16-0");
        assert_eq!(object_name("centos/10-Stream"), "centos-10-stream");
    }
}
