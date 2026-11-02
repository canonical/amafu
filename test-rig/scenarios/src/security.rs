use amafu_api::{ApiClient, RawResponse};
use amafu_checkers::authorization_policy;
use amafu_domain::ProjectName;
use amafu_domain::access::{Role, Verb};
use amafu_domain::registry::{KindDescriptor, kinds};
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure, ensure_equal,
};
use futures::FutureExt;
use http::Method;

const OWN_PROJECT: &str = "project-a";
const OTHER_PROJECT: &str = "project-b";
const PROBE: &str = "probe";
const FORBIDDEN: u16 = 403;
const UNAUTHORIZED: u16 = 401;

pub const AUTHORIZATION_MATRIX: ScenarioDefinition = ScenarioDefinition {
    name: "every role, verb and kind is allowed or denied as specified",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| every_role_verb_and_kind_is_allowed_or_denied_as_specified(cluster).boxed_local(),
};

pub const CREDENTIALS_ARE_CHECKED: ScenarioDefinition = ScenarioDefinition {
    name: "missing, unknown and expired credentials are refused and audited",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| missing_unknown_and_expired_credentials_are_refused_and_audited(cluster).boxed_local(),
};

fn project(name: &str) -> Result<ProjectName, ScenarioError> {
    ProjectName::new(name).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

pub async fn attempt(
    client: &ApiClient,
    verb: Verb,
    kind: &KindDescriptor,
    target_project: &str,
) -> Result<RawResponse, ScenarioError> {
    let collection = format!("/v1/{}", kind.plural_name);
    let query = format!("?project={target_project}");
    let invalid_create = br#"{"metadata":{"name":"probe"},"specification":{}}"#.to_vec();
    let invalid_update = br#"{"specification":{}}"#.to_vec();
    let (method, path, body) = match verb {
        Verb::Get => (Method::GET, format!("{collection}/{PROBE}{query}"), None),
        Verb::List => (Method::GET, format!("{collection}{query}"), None),
        Verb::Watch => (Method::GET, format!("{collection}/{PROBE}/watch{query}"), None),
        Verb::Create => (Method::POST, format!("{collection}{query}"), Some(invalid_create)),
        Verb::Update => (Method::PUT, format!("{collection}/{PROBE}{query}"), Some(invalid_update)),
        Verb::Delete => (Method::DELETE, format!("{collection}/{PROBE}{query}"), None),
    };
    Ok(client.raw(method, &path, body).await?)
}

async fn ensure_audited(cluster: &dyn Cluster, response: &RawResponse, context: &str) -> ScenarioResult {
    let request_id = response.request_id.ok_or_else(|| {
        ScenarioError::Expectation(format!("{context}: the response carries no request ID"))
    })?;
    ensure(cluster.audit_contains(request_id).await?, || format!("{context}: the request is not audited"))
}

async fn every_role_verb_and_kind_is_allowed_or_denied_as_specified(cluster: &dyn Cluster) -> ScenarioResult {
    let own_project = project(OWN_PROJECT)?;
    for kind in kinds() {
        for verb in Verb::ALL {
            for role in Role::ALL {
                let client = cluster.client_in_project(role, &own_project).await;
                for target_project in [OWN_PROJECT, OTHER_PROJECT] {
                    let context = format!("{role} {verb} {} in {target_project}", kind.kind);
                    let expected =
                        authorization_policy::expected(role, verb, kind.kind, target_project == OWN_PROJECT)
                            .ok_or_else(|| {
                                ScenarioError::Expectation(format!("{context}: no expectation written"))
                            })?;
                    let response = attempt(&client, verb, &kind, target_project).await?;
                    ensure(response.status != UNAUTHORIZED, || {
                        format!("{context}: the client was not authenticated")
                    })?;
                    ensure_equal(&(response.status != FORBIDDEN), &expected, || {
                        format!("{context} is allowed")
                    })?;
                    ensure_audited(cluster, &response, &context).await?;
                }
            }
        }
    }
    Ok(())
}

async fn missing_unknown_and_expired_credentials_are_refused_and_audited(
    cluster: &dyn Cluster,
) -> ScenarioResult {
    let anonymous = cluster.anonymous_client().await;
    let expired = cluster.expired_client().await;
    for kind in kinds() {
        for (description, client) in [("anonymous", &anonymous), ("expired", &expired)] {
            for verb in Verb::ALL {
                let context = format!("{description} {verb} {}", kind.kind);
                let response = attempt(client, verb, &kind, OWN_PROJECT).await?;
                ensure_equal(&response.status, &UNAUTHORIZED, || context.clone())?;
                ensure_audited(cluster, &response, &context).await?;
            }
        }
    }
    let forged = anonymous.raw(Method::GET, "/v1/vms", None).await.map_err(ScenarioError::from)?;
    ensure_equal(&forged.status, &UNAUTHORIZED, || String::from("a request without a token"))
}
