use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

const WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";

fn relative_parts(path: &Path) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(relative)
}

fn contained_directory(root: &Path, relative: &Path) -> io::Result<Option<PathBuf>> {
    let candidate = root.join(relative);
    let Ok(canonical) = candidate.canonicalize() else {
        return Ok(None);
    };
    Ok(canonical.starts_with(root.canonicalize()?).then_some(canonical))
}

fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn apply_whiteout(root: &Path, relative: &Path) -> io::Result<bool> {
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    if !name.starts_with(WHITEOUT_PREFIX) {
        return Ok(false);
    }
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let Some(directory) = contained_directory(root, parent)? else {
        return Ok(true);
    };
    if name == OPAQUE_WHITEOUT {
        for entry in fs::read_dir(&directory)? {
            remove_entry(&entry?.path())?;
        }
    } else {
        let hidden = &name[WHITEOUT_PREFIX.len()..];
        if !hidden.is_empty() && hidden != "." && hidden != ".." {
            remove_entry(&directory.join(hidden))?;
        }
    }
    Ok(true)
}

pub fn unpack_layer(root: &Path, layer: impl Read) -> io::Result<()> {
    let mut archive = tar::Archive::new(layer);
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let Some(relative) = relative_parts(&entry.path()?) else {
            continue;
        };
        if apply_whiteout(root, &relative)? {
            continue;
        }
        if !entry.unpack_in(root)? {
            tracing::warn!(path = %relative.display(), "skipped a layer entry that points outside the root");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(entries: &[(&str, Option<&str>, Option<&str>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, contents, link) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_uid(u64::from(rustix::process::getuid().as_raw()));
            header.set_gid(u64::from(rustix::process::getgid().as_raw()));
            header.set_mtime(0);
            match (contents, link) {
                (_, Some(target)) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    builder.append_link(&mut header, path, target).unwrap();
                }
                (Some(contents), None) => {
                    header.set_size(contents.len() as u64);
                    builder.append_data(&mut header, path, contents.as_bytes()).unwrap();
                }
                (None, None) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    builder.append_data(&mut header, path, io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn whiteouts_remove_files_from_lower_layers() {
        let root = tempfile::tempdir().unwrap();
        unpack_layer(
            root.path(),
            layer(&[
                ("etc/", None, None),
                ("etc/motd", Some("hello"), None),
                ("etc/keep", Some("kept"), None),
            ])
            .as_slice(),
        )
        .unwrap();
        unpack_layer(root.path(), layer(&[("etc/.wh.motd", Some(""), None)]).as_slice()).unwrap();
        assert!(!root.path().join("etc/motd").exists());
        assert!(root.path().join("etc/keep").exists());
        unpack_layer(
            root.path(),
            layer(&[("etc/.wh..wh..opq", Some(""), None), ("etc/new", Some("new"), None)]).as_slice(),
        )
        .unwrap();
        assert!(!root.path().join("etc/keep").exists());
        assert!(root.path().join("etc/new").exists());
    }

    #[test]
    fn a_whiteout_through_a_symlink_cannot_reach_outside_the_root() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("victim"), "precious").unwrap();
        let root = tempfile::tempdir().unwrap();
        let escape = outside.path().to_str().unwrap().to_owned();
        unpack_layer(root.path(), layer(&[("escape", None, Some(&escape))]).as_slice()).unwrap();
        unpack_layer(root.path(), layer(&[("escape/.wh.victim", Some(""), None)]).as_slice()).unwrap();
        unpack_layer(root.path(), layer(&[("escape/.wh..wh..opq", Some(""), None)]).as_slice()).unwrap();
        assert!(outside.path().join("victim").exists());
    }

    #[test]
    fn files_cannot_be_written_through_a_symlink_or_a_parent_path() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let escape = outside.path().to_str().unwrap().to_owned();
        unpack_layer(root.path(), layer(&[("escape", None, Some(&escape))]).as_slice()).unwrap();
        let _ = unpack_layer(root.path(), layer(&[("escape/planted", Some("bad"), None)]).as_slice());
        assert!(!outside.path().join("planted").exists());
        assert_eq!(relative_parts(Path::new("../../etc/passwd")), None);
    }
}
