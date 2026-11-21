use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use amafu_api::ApiClient;
use amafu_domain::authorized_key::AuthorizedKey;
use amafu_domain::kinds::ssh_key::{
    ImportPlan, KeyOrigin, KeySource, SshKey, SshKeyGroup, SshKeyGroupSpecification, SshKeyReference,
    SshKeySpecification,
};
use amafu_domain::{Object, ObjectName};
use amafu_ssh_key_pair::SshKeyPair;
use anyhow::{Context, bail};
use clap::{Args, Subcommand};

use crate::output::{OutputFormat, emit_line};
use crate::resources::{CommonCommand, Tabulated, create, print_objects, run_common};

const PRIVATE_KEY_MODE: u32 = 0o600;
const PUBLIC_KEY_MODE: u32 = 0o644;

#[derive(Subcommand)]
pub enum SshKeyCommand {
    #[command(about = "Add a public key you already have")]
    Add {
        name: ObjectName,
        #[arg(help = "The public key, or a path to a .pub file")]
        public_key: String,
    },
    #[command(
        about = "Make a new key pair here and add its public key; the private key stays on this machine"
    )]
    Generate {
        name: ObjectName,
        #[arg(long, help = "Where to write the private key; the public key goes next to it with .pub")]
        output: Option<PathBuf>,
        #[arg(long, help = "The comment in the public key")]
        comment: Option<String>,
    },
    #[command(about = "Add the public keys a GitHub or Launchpad account publishes")]
    Import {
        #[arg(value_parser = account, value_name = "github:USER|launchpad:USER")]
        account: (KeySource, String),
        #[arg(long, help = "Also put the keys in this group, creating it if needed")]
        group: Option<ObjectName>,
        #[arg(long, help = "Delete keys imported earlier that the account no longer publishes")]
        prune: bool,
        #[arg(long, help = "Show what would change and change nothing")]
        dry_run: bool,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

#[derive(Subcommand)]
pub enum SshKeyGroupCommand {
    #[command(about = "Gather SSH keys to grant together")]
    Create {
        name: ObjectName,
        #[arg(long = "key", value_name = "KEY")]
        keys: Vec<ObjectName>,
    },
    #[command(about = "Put keys in a group")]
    Add { name: String, keys: Vec<ObjectName> },
    #[command(about = "Take keys out of a group")]
    Remove { name: String, keys: Vec<ObjectName> },
    #[command(flatten)]
    Common(CommonCommand),
}

#[derive(Debug, Clone, Default, Args)]
pub struct KeyGrants {
    #[arg(long = "ssh-key", value_name = "KEY", help = "Let this SSH key log in; repeat for more")]
    keys: Vec<ObjectName>,
    #[arg(long = "ssh-key-group", value_name = "GROUP", help = "Let every key in this group log in")]
    groups: Vec<ObjectName>,
}

impl KeyGrants {
    #[must_use]
    pub fn references(self) -> Vec<SshKeyReference> {
        let groups = self.groups.into_iter().map(|name| SshKeyReference::Group { name });
        groups.chain(self.keys.into_iter().map(|name| SshKeyReference::Key { name })).collect()
    }
}

fn account(text: &str) -> Result<(KeySource, String), String> {
    let (source, user) = text.split_once(':').ok_or_else(|| format!("{text:?} is not SOURCE:USER"))?;
    Ok((source.parse()?, user.to_owned()))
}

fn origin_text(origin: &KeyOrigin) -> String {
    match origin.source() {
        Some(source) => format!("{}:{}", source.as_str(), origin.account().unwrap_or_default()),
        None if *origin == KeyOrigin::Generated => String::from("generated"),
        None => String::from("pasted"),
    }
}

impl Tabulated for SshKey {
    const HEADINGS: &'static [&'static str] = &["NAME", "TYPE", "FINGERPRINT", "COMMENT", "ORIGIN"];

    fn row(key: &Object<Self>) -> Vec<String> {
        let parsed = key.specification.authorized_key().ok();
        vec![
            key.metadata.name.to_string(),
            parsed.as_ref().map_or_else(|| String::from("-"), |key| key.algorithm().to_owned()),
            parsed.as_ref().map_or_else(|| String::from("-"), AuthorizedKey::fingerprint),
            parsed
                .as_ref()
                .and_then(|key| key.comment().map(str::to_owned))
                .unwrap_or_else(|| String::from("-")),
            origin_text(&key.specification.origin),
        ]
    }
}

impl Tabulated for SshKeyGroup {
    const HEADINGS: &'static [&'static str] = &["NAME", "KEYS"];

    fn row(group: &Object<Self>) -> Vec<String> {
        let keys: Vec<String> = group.specification.keys.iter().map(ToString::to_string).collect();
        vec![
            group.metadata.name.to_string(),
            if keys.is_empty() { String::from("-") } else { keys.join(", ") },
        ]
    }
}

fn public_key_text(argument: &str) -> anyhow::Result<String> {
    let path = Path::new(argument);
    if argument.contains(' ') || !path.is_file() {
        return Ok(argument.to_owned());
    }
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn write_new(path: &Path, contents: &str, mode: u32) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("writing {}; it must not exist yet", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn public_path(private: &Path) -> PathBuf {
    let mut name = private.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

async fn generate(
    client: &ApiClient,
    name: &ObjectName,
    output: Option<PathBuf>,
    comment: Option<String>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let private_path = output.unwrap_or_else(|| PathBuf::from(name.as_str()));
    let public_path = public_path(&private_path);
    if private_path.exists() || public_path.exists() {
        bail!("{} or {} already exists", private_path.display(), public_path.display());
    }
    let pair = SshKeyPair::generate(&comment.unwrap_or_else(|| name.to_string()))?;
    let specification =
        SshKeySpecification { public_key: pair.public_key.clone(), origin: KeyOrigin::Generated };
    write_new(&private_path, &pair.private_key, PRIVATE_KEY_MODE)?;
    if let Err(error) = create::<SshKey>(client, name, &specification, format).await {
        std::fs::remove_file(&private_path).ok();
        return Err(error);
    }
    write_new(&public_path, &format!("{}\n", pair.public_key), PUBLIC_KEY_MODE)?;
    emit_line(format!("The private key is in {}; Amafu keeps only the public key.", private_path.display()))?;
    Ok(())
}

async fn existing_keys(client: &ApiClient) -> anyhow::Result<Vec<(ObjectName, SshKeySpecification)>> {
    Ok(client.list::<SshKey>().await?.into_iter().map(|key| (key.metadata.name, key.specification)).collect())
}

fn describe(plan: &ImportPlan, prune: bool) -> anyhow::Result<()> {
    for (name, specification) in &plan.additions {
        emit_line(format!("+ {name}  {}", specification.public_key))?;
    }
    for name in &plan.withdrawn {
        let action = if prune { "deleting" } else { "no longer published; --prune deletes it" };
        emit_line(format!("- {name}  {action}"))?;
    }
    if plan.additions.is_empty() && plan.withdrawn.is_empty() {
        emit_line("Nothing has changed.")?;
    }
    Ok(())
}

async fn add_to_group(client: &ApiClient, group: &ObjectName, keys: &[ObjectName]) -> anyhow::Result<()> {
    match client.get::<SshKeyGroup>(group.as_str()).await {
        Ok(existing) => change_group(client, group.as_str(), existing, |members| {
            members.extend(keys.iter().filter(|key| !members.contains(key)).cloned().collect::<Vec<_>>());
        })
        .await
        .map(|_| ()),
        Err(error) if error.status() == Some(404) => {
            let specification = SshKeyGroupSpecification { keys: keys.to_vec() };
            client.create::<SshKeyGroup>(group, &specification).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn import(
    client: &ApiClient,
    (source, user): (KeySource, String),
    group: Option<ObjectName>,
    prune: bool,
    dry_run: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let lookup = client.look_up_ssh_keys(source, &user).await?;
    for skipped in &lookup.skipped {
        emit_line(format!("! skipped a key: {skipped}"))?;
    }
    let plan = lookup.import_plan(&existing_keys(client).await?);
    describe(&plan, prune)?;
    if dry_run {
        return Ok(());
    }
    let mut created = Vec::new();
    for (name, specification) in &plan.additions {
        created.push(client.create::<SshKey>(name, specification).await?);
    }
    if let Some(group) = group {
        let members: Vec<ObjectName> =
            plan.unchanged.iter().chain(plan.additions.iter().map(|(name, _)| name)).cloned().collect();
        add_to_group(client, &group, &members).await?;
    }
    if prune {
        for name in &plan.withdrawn {
            client.delete::<SshKey>(name.as_str()).await?;
        }
    }
    if created.is_empty() { Ok(()) } else { print_objects(&created, format) }
}

async fn change_group(
    client: &ApiClient,
    name: &str,
    group: Object<SshKeyGroup>,
    change: impl FnOnce(&mut Vec<ObjectName>),
) -> anyhow::Result<Object<SshKeyGroup>> {
    let mut specification = group.specification;
    change(&mut specification.keys);
    Ok(client.update::<SshKeyGroup>(name, &specification, Some(group.metadata.generation)).await?)
}

pub async fn run(client: &ApiClient, command: SshKeyCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        SshKeyCommand::Add { name, public_key } => {
            let public_key = public_key_text(&public_key)?.trim().to_owned();
            let specification = SshKeySpecification { public_key, origin: KeyOrigin::Pasted };
            create::<SshKey>(client, &name, &specification, format).await
        }
        SshKeyCommand::Generate { name, output, comment } => {
            generate(client, &name, output, comment, format).await
        }
        SshKeyCommand::Import { account, group, prune, dry_run } => {
            import(client, account, group, prune, dry_run, format).await
        }
        SshKeyCommand::Common(command) => run_common::<SshKey>(client, command, format).await,
    }
}

pub async fn run_group(
    client: &ApiClient,
    command: SshKeyGroupCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        SshKeyGroupCommand::Create { name, keys } => {
            create::<SshKeyGroup>(client, &name, &SshKeyGroupSpecification { keys }, format).await
        }
        SshKeyGroupCommand::Add { name, keys } => {
            let group = client.get::<SshKeyGroup>(&name).await?;
            let changed = change_group(client, &name, group, |members| {
                members.extend(keys.into_iter().filter(|key| !members.contains(key)).collect::<Vec<_>>());
            })
            .await?;
            print_objects(&[changed], format)
        }
        SshKeyGroupCommand::Remove { name, keys } => {
            let group = client.get::<SshKeyGroup>(&name).await?;
            let changed =
                change_group(client, &name, group, |members| members.retain(|key| !keys.contains(key)))
                    .await?;
            print_objects(&[changed], format)
        }
        SshKeyGroupCommand::Common(command) => run_common::<SshKeyGroup>(client, command, format).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_names_its_source_and_user() {
        assert_eq!(account("github:alice").unwrap(), (KeySource::Github, String::from("alice")));
        assert_eq!(account("lp:bob").unwrap(), (KeySource::Launchpad, String::from("bob")));
        assert!(account("alice").is_err());
        assert!(account("gitlab:alice").is_err());
    }

    #[test]
    fn groups_are_granted_before_single_keys() {
        let grants = KeyGrants {
            keys: vec![ObjectName::new("alice").unwrap()],
            groups: vec![ObjectName::new("team").unwrap()],
        };
        assert_eq!(
            grants.references(),
            [
                SshKeyReference::Group { name: ObjectName::new("team").unwrap() },
                SshKeyReference::Key { name: ObjectName::new("alice").unwrap() }
            ]
        );
    }

    #[test]
    fn the_public_key_sits_next_to_the_private_key() {
        assert_eq!(public_path(Path::new("keys/web")), PathBuf::from("keys/web.pub"));
    }
}
