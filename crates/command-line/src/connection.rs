use std::path::PathBuf;
use std::sync::Arc;

use amafu_api::{ApiClient, HttpTransport};
use amafu_domain::ProjectName;
use anyhow::Context;
use clap::Args;

#[derive(Args)]
pub struct ConnectionArguments {
    #[arg(long, global = true, env = "AMAFU_ENDPOINT", default_value = "https://127.0.0.1:7443")]
    endpoint: String,
    #[arg(long, global = true, env = "AMAFU_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[arg(long, global = true, env = "AMAFU_CERTIFICATE_AUTHORITY")]
    certificate_authority: Option<PathBuf>,
    #[arg(long, global = true, env = "AMAFU_PROJECT")]
    project: Option<ProjectName>,
}

impl ConnectionArguments {
    #[must_use]
    pub fn certificate_authority(&self) -> Option<&PathBuf> {
        self.certificate_authority.as_ref()
    }

    pub fn client(&self) -> anyhow::Result<ApiClient> {
        let certificate_authority = self
            .certificate_authority
            .as_ref()
            .map(|path| std::fs::read(path).with_context(|| format!("reading {}", path.display())))
            .transpose()?;
        let transport = HttpTransport::new(&self.endpoint, certificate_authority.as_deref())?;
        let client = ApiClient::new(Arc::new(transport), self.token.clone());
        Ok(match &self.project {
            Some(project) => client.in_project(project.clone()),
            None => client,
        })
    }
}
