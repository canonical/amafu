use std::path::PathBuf;

use amafu_domain::features::FeatureLevel;
use amafu_interfaces::ImageSigningKey;
use anyhow::{Context, bail};
use clap::Subcommand;
use sha2::{Digest, Sha256};

#[derive(Subcommand)]
pub enum ReleaseCommand {
    #[command(about = "Create a key that signs node images and print its public half")]
    CreateKey {
        #[arg(long, help = "Where to keep the private key")]
        private_key: PathBuf,
    },
    #[command(about = "Sign a node image bundle and print its manifest")]
    Sign {
        #[arg(long)]
        private_key: PathBuf,
        #[arg(long, help = "The node-image.tar the image build produced")]
        bundle: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long)]
        feature_level: u32,
        #[arg(long, help = "Where nodes will download the bundle from")]
        url: String,
    },
}

fn write_private(path: &PathBuf, contents: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(
        &mut options.open(path).with_context(|| format!("creating {}", path.display()))?,
        contents,
    )?;
    Ok(())
}

pub fn run(command: ReleaseCommand) -> anyhow::Result<()> {
    match command {
        ReleaseCommand::CreateKey { private_key } => {
            let (key, document) = ImageSigningKey::generate()?;
            write_private(&private_key, &document)?;
            println!("{}", key.public_key_hexadecimal());
            Ok(())
        }
        ReleaseCommand::Sign { private_key, bundle, version, feature_level, url } => {
            if !amafu_domain::node_image::is_version(&version) {
                bail!("{version} is not a valid image version");
            }
            let key = ImageSigningKey::from_pkcs8(
                &std::fs::read(&private_key).with_context(|| format!("reading {}", private_key.display()))?,
            )?;
            let bytes = std::fs::read(&bundle).with_context(|| format!("reading {}", bundle.display()))?;
            let digest = hex::encode(Sha256::digest(&bytes)).parse()?;
            let image = key.sign(&version, FeatureLevel(feature_level), digest, bytes.len() as u64, url);
            println!("{}", serde_json::to_string_pretty(&image)?);
            Ok(())
        }
    }
}
