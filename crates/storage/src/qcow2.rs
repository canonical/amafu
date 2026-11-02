use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

const MAGIC: [u8; 4] = *b"QFI\xfb";
const VERSION: u32 = 3;
const CLUSTER_BITS: u32 = 16;
const CLUSTER_BYTES: u64 = 1 << CLUSTER_BITS;
const HEADER_BYTES: u32 = 104;
const REFCOUNT_ORDER: u32 = 4;
const L2_ENTRIES_PER_CLUSTER: u64 = CLUSTER_BYTES / 8;
const BACKING_FORMAT_EXTENSION: u32 = 0xe279_2aca;
const END_OF_EXTENSIONS: u32 = 0;
const REFCOUNT_TABLE_CLUSTER: u64 = 1;
const REFCOUNT_BLOCK_CLUSTER: u64 = 2;
const FIRST_L1_CLUSTER: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackingFormat {
    Qcow2,
    Raw,
}

impl BackingFormat {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Qcow2 => b"qcow2",
            Self::Raw => b"raw",
        }
    }

    pub fn detect(path: &Path) -> io::Result<Self> {
        let mut magic = [0_u8; 4];
        let read = File::open(path)?.read(&mut magic)?;
        Ok(if read == magic.len() && magic == MAGIC { Self::Qcow2 } else { Self::Raw })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backing<'path> {
    pub path: &'path Path,
    pub format: BackingFormat,
}

fn padded(bytes: &mut Vec<u8>, alignment: usize) {
    let remainder = bytes.len() % alignment;
    if remainder != 0 {
        bytes.resize(bytes.len() + alignment - remainder, 0);
    }
}

fn l1_entries(virtual_size: u64) -> u64 {
    virtual_size.div_ceil(CLUSTER_BYTES * L2_ENTRIES_PER_CLUSTER).max(1)
}

#[must_use]
pub fn encode(virtual_size: u64, backing: Option<Backing<'_>>) -> Vec<u8> {
    let l1_size = l1_entries(virtual_size);
    let l1_clusters = (l1_size * 8).div_ceil(CLUSTER_BYTES);
    let total_clusters = FIRST_L1_CLUSTER + l1_clusters;
    let mut extensions = Vec::new();
    if let Some(backing) = backing {
        let name = backing.format.name();
        extensions.extend_from_slice(&BACKING_FORMAT_EXTENSION.to_be_bytes());
        extensions.extend_from_slice(&u32::try_from(name.len()).unwrap_or(0).to_be_bytes());
        extensions.extend_from_slice(name);
        padded(&mut extensions, 8);
    }
    extensions.extend_from_slice(&END_OF_EXTENSIONS.to_be_bytes());
    extensions.extend_from_slice(&0_u32.to_be_bytes());
    let backing_name =
        backing.map(|backing| backing.path.as_os_str().as_encoded_bytes().to_vec()).unwrap_or_default();
    let backing_offset =
        if backing_name.is_empty() { 0 } else { u64::from(HEADER_BYTES) + extensions.len() as u64 };

    let mut header = Vec::with_capacity(usize::try_from(CLUSTER_BYTES).unwrap_or(0));
    header.extend_from_slice(&MAGIC);
    header.extend_from_slice(&VERSION.to_be_bytes());
    header.extend_from_slice(&backing_offset.to_be_bytes());
    header.extend_from_slice(&u32::try_from(backing_name.len()).unwrap_or(0).to_be_bytes());
    header.extend_from_slice(&CLUSTER_BITS.to_be_bytes());
    header.extend_from_slice(&virtual_size.to_be_bytes());
    header.extend_from_slice(&0_u32.to_be_bytes());
    header.extend_from_slice(&u32::try_from(l1_size).unwrap_or(u32::MAX).to_be_bytes());
    header.extend_from_slice(&(FIRST_L1_CLUSTER * CLUSTER_BYTES).to_be_bytes());
    header.extend_from_slice(&(REFCOUNT_TABLE_CLUSTER * CLUSTER_BYTES).to_be_bytes());
    header.extend_from_slice(&1_u32.to_be_bytes());
    header.extend_from_slice(&0_u32.to_be_bytes());
    header.extend_from_slice(&0_u64.to_be_bytes());
    header.extend_from_slice(&0_u64.to_be_bytes());
    header.extend_from_slice(&0_u64.to_be_bytes());
    header.extend_from_slice(&0_u64.to_be_bytes());
    header.extend_from_slice(&REFCOUNT_ORDER.to_be_bytes());
    header.extend_from_slice(&HEADER_BYTES.to_be_bytes());
    header.extend_from_slice(&extensions);
    header.extend_from_slice(&backing_name);

    let cluster = usize::try_from(CLUSTER_BYTES).unwrap_or(0);
    let mut image = vec![0_u8; usize::try_from(total_clusters * CLUSTER_BYTES).unwrap_or(0)];
    image[..header.len()].copy_from_slice(&header);
    let refcount_table = usize::try_from(REFCOUNT_TABLE_CLUSTER).unwrap_or(0) * cluster;
    image[refcount_table..refcount_table + 8]
        .copy_from_slice(&(REFCOUNT_BLOCK_CLUSTER * CLUSTER_BYTES).to_be_bytes());
    let refcount_block = usize::try_from(REFCOUNT_BLOCK_CLUSTER).unwrap_or(0) * cluster;
    for index in 0..usize::try_from(total_clusters).unwrap_or(0) {
        image[refcount_block + index * 2..refcount_block + index * 2 + 2]
            .copy_from_slice(&1_u16.to_be_bytes());
    }
    image
}

#[must_use]
pub fn fits(backing: &Path) -> bool {
    backing.as_os_str().len() + 64 < usize::try_from(CLUSTER_BYTES).unwrap_or(0) - HEADER_BYTES as usize
}

pub fn create(path: &Path, virtual_size: u64, backing: Option<Backing<'_>>) -> io::Result<()> {
    if backing.is_some_and(|backing| !fits(backing.path)) {
        return Err(io::Error::other("the backing file path is too long"));
    }
    let temporary = path.with_extension("partial");
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(&temporary)?;
    file.write_all(&encode(virtual_size, backing))?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn qemu_img(arguments: &[&str]) -> Option<String> {
        let output = Command::new("qemu-img").args(arguments).output().ok()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn the_header_starts_with_the_magic_and_version() {
        let image = encode(1 << 30, None);
        assert_eq!(&image[..4], &MAGIC);
        assert_eq!(&image[4..8], &VERSION.to_be_bytes());
        assert_eq!(image.len() as u64 % CLUSTER_BYTES, 0);
    }

    #[test]
    fn qemu_accepts_a_standalone_image_and_an_overlay() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("base.raw");
        fs::write(&base, vec![7_u8; 1 << 20]).unwrap();
        let standalone = directory.path().join("standalone.qcow2");
        create(&standalone, 16 << 30, None).unwrap();
        let overlay = directory.path().join("overlay.qcow2");
        create(
            &overlay,
            16 << 30,
            Some(Backing { path: &base, format: BackingFormat::detect(&base).unwrap() }),
        )
        .unwrap();
        let Some(check) = qemu_img(&["check", "--output=json", standalone.to_str().unwrap()]) else {
            return;
        };
        assert!(check.contains("\"check-errors\": 0"), "{check}");
        let info = qemu_img(&["info", "--output=json", overlay.to_str().unwrap()]).unwrap_or_default();
        assert!(info.contains("\"virtual-size\": 17179869184"), "{info}");
        assert!(info.contains("\"backing-filename-format\": \"raw\""), "{info}");
        assert!(qemu_img(&["check", overlay.to_str().unwrap()]).unwrap_or_default().contains("No errors"));
        let converted = directory.path().join("converted.raw");
        qemu_img(&["convert", "-O", "raw", overlay.to_str().unwrap(), converted.to_str().unwrap()]);
        let mut first = [0_u8; 4];
        File::open(&converted).unwrap().read_exact(&mut first).unwrap();
        assert_eq!(first, [7, 7, 7, 7], "reads fall through to the backing file");
    }
}
