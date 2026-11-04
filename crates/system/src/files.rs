use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

pub fn write_durably(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let directory = path.parent().ok_or_else(|| std::io::Error::other("the path has no parent directory"))?;
    let file_name = path.file_name().ok_or_else(|| std::io::Error::other("the path has no file name"))?;
    let temporary = directory.join(format!(".{}.partial", file_name.to_string_lossy()));
    let mut file = File::create(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_replaced_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("node-id");
        write_durably(&path, b"first").unwrap();
        write_durably(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
