use std::path::{Path, PathBuf};

use amafu_domain::ObjectName;
use serde::{Deserialize, Serialize};

const KERNEL_FILE: &str = "kernel";
const INITRAMFS_FILE: &str = "initramfs";
const COMMAND_LINE_FILE: &str = "command-line";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBundle {
    pub kernel: PathBuf,
    pub initramfs: Option<PathBuf>,
    pub command_line: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("image {0} is not available on this node")]
    NotFound(ObjectName),
    #[error("image {image} cannot be read: {cause}")]
    Unreadable { image: ObjectName, cause: std::io::Error },
}

pub fn find_image(images_directory: &Path, image: &ObjectName) -> Result<ImageBundle, ImageError> {
    let directory = images_directory.join(image.as_str());
    let kernel = directory.join(KERNEL_FILE);
    if !kernel.is_file() {
        return Err(ImageError::NotFound(image.clone()));
    }
    let initramfs = Some(directory.join(INITRAMFS_FILE)).filter(|path| path.is_file());
    let command_line_path = directory.join(COMMAND_LINE_FILE);
    let command_line = if command_line_path.is_file() {
        std::fs::read_to_string(&command_line_path)
            .map_err(|cause| ImageError::Unreadable { image: image.clone(), cause })?
            .trim()
            .to_owned()
    } else {
        String::from("console=ttyS0")
    };
    Ok(ImageBundle { kernel, initramfs, command_line })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_needs_a_kernel_and_may_have_an_initramfs_and_command_line() {
        let directory = tempfile::tempdir().unwrap();
        let image = ObjectName::new("test-guest").unwrap();
        assert!(matches!(find_image(directory.path(), &image), Err(ImageError::NotFound(_))));

        let bundle_directory = directory.path().join("test-guest");
        std::fs::create_dir_all(&bundle_directory).unwrap();
        std::fs::write(bundle_directory.join("kernel"), b"kernel").unwrap();
        let bundle = find_image(directory.path(), &image).unwrap();
        assert_eq!(bundle.initramfs, None);
        assert_eq!(bundle.command_line, "console=ttyS0");

        std::fs::write(bundle_directory.join("initramfs"), b"initramfs").unwrap();
        std::fs::write(bundle_directory.join("command-line"), b"console=ttyS0 quiet\n").unwrap();
        let bundle = find_image(directory.path(), &image).unwrap();
        assert_eq!(bundle.initramfs, Some(bundle_directory.join("initramfs")));
        assert_eq!(bundle.command_line, "console=ttyS0 quiet");
    }
}
