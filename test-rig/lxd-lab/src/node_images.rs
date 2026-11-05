use std::io::Read;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use amafu_domain::features::SUPPORTED_FEATURE_LEVEL;
use amafu_domain::kinds::storage_backend::PluginModule;
use amafu_domain::node_image::NodeImage;
use amafu_interfaces::ImageSigningKey;
use amafu_scenario::NodeImages;
use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::lxd::LxdClient;

pub const IMAGE_SERVER_PORT: u16 = 8080;
pub const IMAGE_DIRECTORY: &str = "/srv/images";
const BUNDLE_FILE: &str = "node-image.tar";
const BOOT_LOADER_FILE: &str = "boot-loader.tar";
const RELEASE_FILE: &str = "release";
const INITRAMFS_FILE: &str = "initramfs";
const BROKEN_INITRAMFS_BYTES: usize = 4096;

pub struct PreparedImages {
    pub images: NodeImages,
    pub files: Vec<(String, PathBuf)>,
}

enum Change {
    Release(String),
    BreakInitramfs,
}

fn entry_name(path: &Path) -> String {
    path.to_string_lossy().trim_start_matches("./").to_owned()
}

fn rewrite(source: &Path, destination: &Path, change: &Change) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(std::fs::File::open(source)?);
    let mut builder = tar::Builder::new(std::fs::File::create(destination)?);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let mut header = entry.header().clone();
        let name = entry_name(&entry.path()?);
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        match change {
            Change::Release(release) if name == RELEASE_FILE => {
                contents = format!("{release}\n").into_bytes();
            }
            Change::BreakInitramfs if name == INITRAMFS_FILE => contents = vec![0; BROKEN_INITRAMFS_BYTES],
            _ => {}
        }
        header.set_size(contents.len() as u64);
        header.set_cksum();
        builder.append(&header, contents.as_slice())?;
    }
    builder.finish()?;
    Ok(())
}

fn sign(key: &ImageSigningKey, version: &str, file: &Path, url: String) -> anyhow::Result<NodeImage> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let digest = hex::encode(Sha256::digest(&bytes)).parse()?;
    Ok(key.sign(version, SUPPORTED_FEATURE_LEVEL, digest, bytes.len() as u64, url))
}

pub const SPDK_PLUGIN_FILE: &str = "amafu_plugin_spdk_rpc.wasm";

pub fn plugin_module(built_plugin: &Path, server: Ipv4Addr) -> anyhow::Result<PluginModule> {
    let bytes = std::fs::read(built_plugin)
        .with_context(|| format!("reading {}; run plugins/build", built_plugin.display()))?;
    Ok(PluginModule {
        url: image_url(server, SPDK_PLUGIN_FILE),
        digest: hex::encode(Sha256::digest(&bytes)).parse()?,
    })
}

#[must_use]
pub fn image_url(server: Ipv4Addr, file: &str) -> String {
    format!("http://{server}:{IMAGE_SERVER_PORT}/{file}")
}

pub fn prepare(
    built: &Path,
    work: &Path,
    server: Ipv4Addr,
    key: &ImageSigningKey,
) -> anyhow::Result<PreparedImages> {
    std::fs::create_dir_all(work)?;
    let source = built.join(BUNDLE_FILE);
    let variants = [
        ("lab-1", "installed.tar", Change::Release(String::from("lab-1"))),
        ("lab-2", "upgrade.tar", Change::Release(String::from("lab-2"))),
        ("lab-2-broken", "broken.tar", Change::BreakInitramfs),
    ];
    let mut files = Vec::new();
    let mut signed = Vec::new();
    for (version, file, change) in &variants {
        let path = work.join(file);
        rewrite(&source, &path, change)?;
        signed.push(sign(key, version, &path, image_url(server, file))?);
        files.push(((*file).to_owned(), path));
    }
    files.push((BOOT_LOADER_FILE.to_owned(), built.join(BOOT_LOADER_FILE)));
    let mut signed = signed.into_iter();
    let (Some(installed), Some(upgrade), Some(broken)) = (signed.next(), signed.next(), signed.next()) else {
        anyhow::bail!("not every lab image was signed");
    };
    Ok(PreparedImages { images: NodeImages { installed, upgrade, broken }, files })
}

pub async fn publish(lxd: &LxdClient, server: &str, prepared: &PreparedImages) -> anyhow::Result<()> {
    for (name, path) in &prepared.files {
        let contents = tokio::fs::read(path).await.with_context(|| format!("reading {}", path.display()))?;
        lxd.push_file(server, &format!("{IMAGE_DIRECTORY}/{name}"), contents).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(directory: &Path) -> PathBuf {
        let path = directory.join(BUNDLE_FILE);
        let mut builder = tar::Builder::new(std::fs::File::create(&path).unwrap());
        for (name, contents) in
            [("./release", b"abc\n".as_slice()), ("./initramfs", b"cpio-archive".as_slice())]
        {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, name, contents).unwrap();
        }
        builder.finish().unwrap();
        path
    }

    fn contents_of(bundle: &Path, wanted: &str) -> Vec<u8> {
        let mut archive = tar::Archive::new(std::fs::File::open(bundle).unwrap());
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry_name(&entry.path().unwrap()) == wanted {
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents).unwrap();
                return contents;
            }
        }
        Vec::new()
    }

    #[test]
    fn each_variant_differs_only_where_it_should() {
        let directory = tempfile::tempdir().unwrap();
        let source = bundle(directory.path());
        let upgrade = directory.path().join("upgrade.tar");
        rewrite(&source, &upgrade, &Change::Release(String::from("lab-2"))).unwrap();
        assert_eq!(contents_of(&upgrade, RELEASE_FILE), b"lab-2\n");
        assert_eq!(contents_of(&upgrade, INITRAMFS_FILE), b"cpio-archive");
        let broken = directory.path().join("broken.tar");
        rewrite(&source, &broken, &Change::BreakInitramfs).unwrap();
        assert_eq!(contents_of(&broken, INITRAMFS_FILE), vec![0; BROKEN_INITRAMFS_BYTES]);
    }
}
