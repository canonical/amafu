use std::io;
use std::path::Path;
use std::sync::Arc;

use amafu_domain::node_image::NodeImage;
use amafu_interfaces::BootSlotError;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

pub fn http_client() -> Result<reqwest::Client, BootSlotError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| BootSlotError::Fetch(error.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|error| BootSlotError::Fetch(error.to_string()))
}

fn fetch_failure(error: impl std::fmt::Display) -> BootSlotError {
    BootSlotError::Fetch(error.to_string())
}

pub async fn download(http: &reqwest::Client, image: &NodeImage, path: &Path) -> Result<(), BootSlotError> {
    let response = http.get(&image.url).send().await.map_err(fetch_failure)?;
    if !response.status().is_success() {
        return Err(BootSlotError::Fetch(format!("{} answered {}", image.url, response.status())));
    }
    let mut file = tokio::fs::File::create(path).await.map_err(fetch_failure)?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(fetch_failure)?;
        written += chunk.len() as u64;
        if written > image.size_bytes {
            return Err(BootSlotError::Mismatch(format!(
                "{} is larger than {} bytes",
                image.url, image.size_bytes
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(fetch_failure)?;
    }
    file.sync_all().await.map_err(fetch_failure)?;
    if written != image.size_bytes {
        return Err(BootSlotError::Mismatch(format!(
            "{} has {written} bytes, not {}",
            image.url, image.size_bytes
        )));
    }
    if hex::encode(hasher.finalize()) != image.digest.hexadecimal() {
        return Err(BootSlotError::Mismatch(format!("{} does not match {}", image.url, image.digest)));
    }
    Ok(())
}

pub fn extract(bundle: &Path, destination: &Path) -> io::Result<u64> {
    std::fs::create_dir_all(destination)?;
    let mut archive = tar::Archive::new(std::fs::File::open(bundle)?);
    let mut files = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(io::Error::other(format!(
                "the bundle holds {}, which is neither a file nor a directory",
                entry.path()?.display()
            )));
        }
        if !entry.unpack_in(destination)? {
            return Err(io::Error::other(format!(
                "the bundle reaches outside its slot with {}",
                entry.path()?.display()
            )));
        }
        files += u64::from(kind.is_file());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with(directory: &Path, entries: &[(&str, tar::EntryType)]) -> std::path::PathBuf {
        let path = directory.join("bundle.tar");
        let mut builder = tar::Builder::new(std::fs::File::create(&path).unwrap());
        for (name, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(if kind.is_file() { 5 } else { 0 });
            header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
            if !kind.is_file() && !kind.is_dir() {
                header.set_link_name("/etc/shadow").unwrap();
            }
            let content: &[u8] = if kind.is_file() { b"amafu" } else { b"" };
            builder.append_data(&mut header, name, content).unwrap();
        }
        builder.finish().unwrap();
        path
    }

    #[test]
    fn a_bundle_of_files_is_unpacked_into_its_slot() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = bundle_with(
            directory.path(),
            &[
                ("modules", tar::EntryType::Directory),
                ("modules/vxlan.ko", tar::EntryType::Regular),
                ("vmlinuz", tar::EntryType::Regular),
            ],
        );
        let slot = directory.path().join("amafu-b");
        assert_eq!(extract(&bundle, &slot).unwrap(), 2);
        assert_eq!(std::fs::read(slot.join("modules/vxlan.ko")).unwrap(), b"amafu");
    }

    #[test]
    fn links_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = bundle_with(directory.path(), &[("vmlinuz", tar::EntryType::Symlink)]);
        assert!(extract(&bundle, &directory.path().join("amafu-b")).is_err());
    }
}
