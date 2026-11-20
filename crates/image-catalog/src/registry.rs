use amafu_domain::kinds::image::ImageSource;
use amafu_interfaces::{CatalogEntry, CatalogKind};

const AUTHORITY: &str = "https://auth.docker.io/token?service=registry.docker.io";
const REGISTRY: &str = "https://registry-1.docker.io/v2";
const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

pub struct OfferedContainer {
    pub repository: &'static str,
    pub tag: &'static str,
    pub id: &'static str,
    pub title: &'static str,
    pub is_long_term: bool,
}

pub const OFFERED: &[OfferedContainer] = &[
    OfferedContainer {
        repository: "library/ubuntu",
        tag: "24.04",
        id: "ubuntu/24.04",
        title: "Ubuntu 24.04 LTS",
        is_long_term: true,
    },
    OfferedContainer {
        repository: "library/ubuntu",
        tag: "22.04",
        id: "ubuntu/22.04",
        title: "Ubuntu 22.04 LTS",
        is_long_term: true,
    },
    OfferedContainer {
        repository: "ubuntu/nginx",
        tag: "latest",
        id: "ubuntu/nginx",
        title: "NGINX on Ubuntu",
        is_long_term: false,
    },
    OfferedContainer {
        repository: "ubuntu/redis",
        tag: "latest",
        id: "ubuntu/redis",
        title: "Redis on Ubuntu",
        is_long_term: false,
    },
    OfferedContainer {
        repository: "ubuntu/postgres",
        tag: "latest",
        id: "ubuntu/postgres",
        title: "PostgreSQL on Ubuntu",
        is_long_term: false,
    },
];

pub async fn digest_of(client: &reqwest::Client, repository: &str, tag: &str) -> Result<String, String> {
    let token: serde_json::Value = client
        .get(format!("{AUTHORITY}&scope=repository:{repository}:pull"))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let bearer = token["token"].as_str().ok_or("the registry gave no token")?;
    let response = client
        .head(format!("{REGISTRY}/{repository}/manifests/{tag}"))
        .bearer_auth(bearer)
        .header(reqwest::header::ACCEPT, MANIFEST_TYPES)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    response
        .headers()
        .get("docker-content-digest")
        .and_then(|digest| digest.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| format!("{repository}:{tag} answered {} without a digest", response.status()))
}

pub fn container_entry(offered: &OfferedContainer, digest: &str) -> Option<CatalogEntry> {
    let reference = format!("docker.io/{}@{digest}", offered.repository).parse().ok()?;
    Some(CatalogEntry {
        id: offered.id.to_owned(),
        name: format!("{}-container", crate::streams::object_name(offered.id)),
        title: offered.title.to_owned(),
        distribution: String::from("ubuntu"),
        kind: CatalogKind::Container,
        source: ImageSource::Container { reference },
        size_bytes: None,
        published: None,
        is_long_term: offered.is_long_term,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolved_image_is_pinned_by_its_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let entry = container_entry(&OFFERED[0], &digest).unwrap();
        assert_eq!(entry.name, "ubuntu-24-04-container");
        let ImageSource::Container { reference } = entry.source else { panic!() };
        assert_eq!(reference.to_string(), format!("docker.io/library/ubuntu@{digest}"));
        assert!(OFFERED.iter().all(|offered| container_entry(offered, &digest).is_some()));
    }
}
