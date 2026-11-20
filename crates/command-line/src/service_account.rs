use amafu_api::ApiClient;
use amafu_domain::access::Role;
use amafu_domain::kinds::service_account::{FederatedIdentity, ServiceAccount, ServiceAccountSpecification};
use amafu_domain::{Object, ObjectName};
use anyhow::bail;
use clap::{Subcommand, ValueEnum};
use http::Method;
use serde_json::Value;

use crate::output::{OutputFormat, print_structured};
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, run_common};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AccountRole {
    Viewer,
    Member,
    Operator,
}

impl From<AccountRole> for Role {
    fn from(role: AccountRole) -> Self {
        match role {
            AccountRole::Viewer => Self::Viewer,
            AccountRole::Member => Self::Member,
            AccountRole::Operator => Self::Operator,
        }
    }
}

#[derive(Subcommand)]
pub enum ServiceAccountCommand {
    #[command(about = "Create a service account for automation")]
    Create {
        name: ObjectName,
        #[arg(long, value_enum)]
        role: AccountRole,
        #[arg(
            long = "trust",
            value_parser = federated_identity,
            value_name = "ISSUER=SUBJECT[@AUDIENCE]",
            help = "Accept tokens from this issuer and subject, such as a CI system's"
        )]
        trusted: Vec<FederatedIdentity>,
    },
    #[command(name = "add-secret", about = "Create a client secret; it is shown once")]
    AddSecret { name: String },
    #[command(name = "remove-secret", about = "Delete a client secret")]
    RemoveSecret { name: String, id: String },
    #[command(flatten)]
    Common(CommonCommand),
}

fn federated_identity(text: &str) -> Result<FederatedIdentity, String> {
    let (issuer, rest) =
        text.split_once('=').ok_or_else(|| format!("{text:?} is not ISSUER=SUBJECT[@AUDIENCE]"))?;
    let (subject, audience) = match rest.rsplit_once('@') {
        Some((subject, audience)) => (subject, Some(audience.to_owned())),
        None => (rest, None),
    };
    Ok(FederatedIdentity { issuer: issuer.to_owned(), subject: subject.to_owned(), audience })
}

impl Tabulated for ServiceAccount {
    const HEADINGS: &'static [&'static str] = &["NAME", "ROLE", "SECRETS", "TRUSTED ISSUERS"];

    fn row(account: &Object<Self>) -> Vec<String> {
        let issuers: Vec<&str> = account
            .specification
            .federated_identities
            .iter()
            .map(|identity| identity.issuer.as_str())
            .collect();
        vec![
            account.metadata.name.to_string(),
            kebab_name(account.specification.role),
            account.status.secrets.len().to_string(),
            if issuers.is_empty() { String::from("-") } else { issuers.join(", ") },
        ]
    }
}

fn secrets_path(client: &ApiClient, name: &str, suffix: &str) -> String {
    let query = client.project().map(|project| format!("?project={project}")).unwrap_or_default();
    format!("/v1/service-accounts/{name}/secrets{suffix}{query}")
}

async fn send(client: &ApiClient, method: Method, path: &str) -> anyhow::Result<Value> {
    let response = client.raw(method, path, None).await?;
    let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    if !(200..300).contains(&response.status) {
        bail!("{}", body["message"].as_str().unwrap_or("the request was refused"));
    }
    Ok(body)
}

pub async fn run(
    client: &ApiClient,
    command: ServiceAccountCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ServiceAccountCommand::Create { name, role, trusted } => {
            let specification =
                ServiceAccountSpecification { role: role.into(), federated_identities: trusted };
            create::<ServiceAccount>(client, &name, &specification, format).await
        }
        ServiceAccountCommand::AddSecret { name } => {
            print_structured(&send(client, Method::POST, &secrets_path(client, &name, "")).await?, format)
        }
        ServiceAccountCommand::RemoveSecret { name, id } => print_structured(
            &send(client, Method::DELETE, &secrets_path(client, &name, &format!("/{id}"))).await?,
            format,
        ),
        ServiceAccountCommand::Common(command) => run_common::<ServiceAccount>(client, command, format).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trusted_identity_names_its_issuer_subject_and_optional_audience() {
        let identity = federated_identity("https://token.actions.example=repo:web:ref:main@amafu").unwrap();
        assert_eq!(identity.issuer, "https://token.actions.example");
        assert_eq!(identity.subject, "repo:web:ref:main");
        assert_eq!(identity.audience.as_deref(), Some("amafu"));
        assert_eq!(federated_identity("https://ci=job").unwrap().audience, None);
        assert!(federated_identity("job").is_err());
    }
}
