use std::fs::File;
use std::path::Path;

use rustix::io::Errno;

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("the module list {path} cannot be read: {cause}")]
    List { path: String, cause: std::io::Error },
    #[error("module {module} cannot be loaded: {cause}")]
    Load { module: String, cause: std::io::Error },
}

fn is_unsupported_by_hardware(error: Errno) -> bool {
    matches!(error, Errno::OPNOTSUPP | Errno::NODEV | Errno::NXIO)
}

pub fn load_listed_modules(directory: &Path, list: &Path) -> Result<usize, ModuleError> {
    let contents = std::fs::read_to_string(list)
        .map_err(|cause| ModuleError::List { path: list.display().to_string(), cause })?;
    let mut loaded = 0;
    for module in contents.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let failure = |cause: std::io::Error| ModuleError::Load { module: module.to_owned(), cause };
        let file = File::open(directory.join(module)).map_err(failure)?;
        match rustix::system::finit_module(&file, c"", 0) {
            Ok(()) => loaded += 1,
            Err(Errno::EXIST) => {}
            Err(error) if is_unsupported_by_hardware(error) => {
                tracing::info!(module, %error, "this hardware does not support the module");
            }
            Err(error) => return Err(failure(error.into())),
        }
    }
    Ok(loaded)
}
