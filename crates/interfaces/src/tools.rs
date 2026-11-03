use std::path::PathBuf;

const LIBRARY_PATH_VARIABLE: &str = "LD_LIBRARY_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTool {
    pub program: PathBuf,
    pub library_directory: Option<PathBuf>,
}

impl ExternalTool {
    #[must_use]
    pub fn installed(program: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), library_directory: None }
    }

    #[must_use]
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        if let Some(directory) = &self.library_directory {
            command.env(LIBRARY_PATH_VARIABLE, directory);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_with_its_own_libraries_is_run_with_them() {
        let tool = ExternalTool {
            program: PathBuf::from("/boot/tools/nft"),
            library_directory: Some(PathBuf::from("/boot/tools/lib")),
        };
        let command = tool.command();
        let variables: Vec<_> = command.get_envs().collect();
        assert_eq!(
            variables,
            vec![(
                std::ffi::OsStr::new(LIBRARY_PATH_VARIABLE),
                Some(std::ffi::OsStr::new("/boot/tools/lib"))
            )]
        );
        assert_eq!(ExternalTool::installed("/usr/sbin/nft").command().get_envs().count(), 0);
    }
}
