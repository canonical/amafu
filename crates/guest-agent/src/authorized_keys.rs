use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use rustix::fs::{Gid, Uid, chown};

const SSH_DIRECTORY_MODE: u32 = 0o700;
const AUTHORIZED_KEYS_MODE: u32 = 0o600;
const ROOT_HOME: &str = "/root";
const HOME_FIELD: usize = 5;
const USER_ID_FIELD: usize = 2;

#[must_use]
pub fn home_of(password_file: &str, user_id: u32) -> PathBuf {
    let listed = password_file.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let matches = fields.get(USER_ID_FIELD)?.parse::<u32>().ok()? == user_id;
        matches.then(|| fields.get(HOME_FIELD).map(PathBuf::from)).flatten()
    });
    listed.unwrap_or_else(|| PathBuf::from(if user_id == 0 { ROOT_HOME } else { "/" }))
}

#[must_use]
pub fn authorized_keys_file(keys: &[String]) -> String {
    let mut file = keys.iter().map(|key| key.trim()).collect::<Vec<_>>().join("\n");
    file.push('\n');
    file
}

pub fn install(home: &Path, keys: &[String], user_id: u32, group_id: u32) -> anyhow::Result<()> {
    let directory = home.join(".ssh");
    DirBuilder::new()
        .recursive(true)
        .mode(SSH_DIRECTORY_MODE)
        .create(&directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    fs::set_permissions(&directory, Permissions::from_mode(SSH_DIRECTORY_MODE))?;
    let path = directory.join("authorized_keys");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(AUTHORIZED_KEYS_MODE)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(authorized_keys_file(keys).as_bytes())?;
    let (owner, group) = (Some(Uid::from_raw(user_id)), Some(Gid::from_raw(group_id)));
    chown(&directory, owner, group).context("handing .ssh to the workload user")?;
    chown(&path, owner, group).context("handing authorized_keys to the workload user")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD_FILE: &str = "root:x:0:0:root:/root:/bin/sh\nubuntu:x:1000:1000::/home/ubuntu:/bin/bash\n";

    #[test]
    fn keys_go_to_the_workload_users_home() {
        assert_eq!(home_of(PASSWORD_FILE, 1000), PathBuf::from("/home/ubuntu"));
        assert_eq!(home_of("", 0), PathBuf::from("/root"));
        assert_eq!(home_of(PASSWORD_FILE, 4242), PathBuf::from("/"));
    }

    #[test]
    fn each_key_takes_one_line() {
        let keys = [String::from("ssh-ed25519 AAAA a "), String::from("ssh-ed25519 BBBB b")];
        assert_eq!(authorized_keys_file(&keys), "ssh-ed25519 AAAA a\nssh-ed25519 BBBB b\n");
    }
}
