#[cfg(test)]
mod service_accounts {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use amafu_api::authentication::{TokenAuthenticator, TokenIdentity};
    use amafu_api::identity::Principal;
    use amafu_api::routes::{ApiState, router};
    use amafu_api::service_accounts::{IssuedTokenAuthenticator, IssuerDirectory};
    use amafu_api::{ApiClient, ChainedAuthenticator, RouterTransport, Transport};
    use amafu_domain::access::Role;
    use amafu_domain::kinds::service_account::{
        FederatedIdentity, ServiceAccount, ServiceAccountSpecification,
    };
    use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
    use amafu_domain::kinds::vm::Vm;
    use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference, ProjectName};
    use amafu_interfaces::objects::apply;
    use amafu_interfaces::{AuditLog, Identifiers, Store};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore, SeededIdentifiers};
    use amafu_token_issuer::{Claims, ExternalKeys, JwkSet, KeyPair};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use http::Method;
    use serde_json::{Value, json};
    use time::{Duration, OffsetDateTime};

    const MEMBER_TOKEN: &str = "amafu-member-token";
    const CI_ISSUER: &str = "https://ci.example";
    const CI_SUBJECT: &str = "repo:web-team/site:ref:refs/heads/main";

    struct CiIssuer {
        keys: ExternalKeys,
    }

    #[async_trait::async_trait]
    impl IssuerDirectory for CiIssuer {
        async fn keys(&self, issuer: &str) -> Result<ExternalKeys, String> {
            if issuer == CI_ISSUER { Ok(self.keys.clone()) } else { Err(format!("unknown issuer {issuer}")) }
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    struct Api {
        member: ApiClient,
        transport: Arc<dyn Transport>,
        ci_key: KeyPair,
    }

    async fn api() -> Api {
        let store = Arc::new(MemoryStore::new());
        let key = KeyPair::generate().unwrap();
        let signing = SigningKeySpecification::window_starting(
            STANDARD.encode(key.pkcs8()),
            now() - Duration::hours(1),
        );
        let reference = ObjectReference::of::<SigningKey>(None, ObjectName::new("key-1").unwrap());
        let change = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name: ObjectName::new("key-1").unwrap(),
            project: None,
            labels: BTreeMap::new(),
            specification: signing,
        };
        apply::<SigningKey>(&*store, &reference, change).await.unwrap();
        let ci_key = KeyPair::generate().unwrap();
        let published = serde_json::to_value(JwkSet::of([&ci_key.public_key()])).unwrap();
        let member = Principal {
            name: String::from("alice"),
            role: Role::Member,
            project: ProjectName::new("web-team").unwrap(),
        };
        let state = ApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(ChainedAuthenticator::new(vec![
                Arc::new(TokenAuthenticator::new(vec![TokenIdentity::for_token(member, MEMBER_TOKEN, None)])),
                Arc::new(IssuedTokenAuthenticator::new(Arc::clone(&store) as Arc<dyn Store>)),
            ])),
            audit: Arc::new(MemoryAuditLog::new()) as Arc<dyn AuditLog>,
            clock: Arc::new(VirtualClock::starting_at(now())),
            identifiers: Arc::new(SeededIdentifiers::new(3)) as Arc<dyn Identifiers>,
            issuers: Arc::new(CiIssuer { keys: ExternalKeys::parse(&published) }),
            key_directory: Arc::new(amafu_api::key_directory::HttpKeyDirectory::default()),
            catalog: Arc::new(amafu_interfaces::FixedImageCatalog::default()),
        };
        let transport: Arc<dyn Transport> = Arc::new(RouterTransport::new(router(state)));
        Api {
            member: ApiClient::new(Arc::clone(&transport), Some(String::from(MEMBER_TOKEN))),
            transport,
            ci_key,
        }
    }

    async fn exchange(api: &Api, request: &Value) -> (u16, Value) {
        let anonymous = ApiClient::new(Arc::clone(&api.transport), None);
        let response =
            anonymous.raw(Method::POST, "/v1/tokens", Some(request.to_string().into_bytes())).await.unwrap();
        (response.status, serde_json::from_slice(&response.body).unwrap_or(Value::Null))
    }

    fn as_service_account(api: &Api, token: &Value) -> ApiClient {
        ApiClient::new(Arc::clone(&api.transport), Some(token["access_token"].as_str().unwrap().to_owned()))
    }

    #[tokio::test]
    async fn a_client_secret_buys_a_short_token_with_the_accounts_role_until_it_is_deleted() {
        let api = api().await;
        let deployer = ServiceAccountSpecification { role: Role::Viewer, federated_identities: Vec::new() };
        api.member.create::<ServiceAccount>(&ObjectName::new("deployer").unwrap(), &deployer).await.unwrap();
        let created = api
            .member
            .raw(Method::POST, "/v1/service-accounts/deployer/secrets?project=web-team", None)
            .await
            .unwrap();
        assert_eq!(created.status, 201);
        let secret: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(secret["client_id"], "web-team/deployer");
        let account = api.member.get::<ServiceAccount>("deployer").await.unwrap();
        assert!(
            !serde_json::to_string(&account).unwrap().contains(secret["client_secret"].as_str().unwrap())
        );

        let request = json!({
            "grant_type": "client_credentials",
            "client_id": "web-team/deployer",
            "client_secret": secret["client_secret"],
        });
        let (status, token) = exchange(&api, &request).await;
        assert_eq!(status, 200);
        assert_eq!(token["expires_in"], 900);
        let viewer = as_service_account(&api, &token).in_project(ProjectName::new("web-team").unwrap());
        assert!(viewer.list::<Vm>().await.is_ok());
        let refused = viewer.create::<ServiceAccount>(&ObjectName::new("escalate").unwrap(), &deployer).await;
        assert_eq!(refused.unwrap_err().status(), Some(403));

        let wrong = json!({"grant_type": "client_credentials", "client_id": "web-team/deployer", "client_secret": "guess"});
        assert_eq!(exchange(&api, &wrong).await.0, 400);
        let path = format!(
            "/v1/service-accounts/deployer/secrets/{}?project=web-team",
            secret["id"].as_str().unwrap()
        );
        assert_eq!(api.member.raw(Method::DELETE, &path, None).await.unwrap().status, 200);
        assert_eq!(exchange(&api, &request).await.0, 400);
    }

    #[tokio::test]
    async fn a_trusted_workload_exchanges_its_own_token_without_any_secret() {
        let api = api().await;
        let account = ServiceAccountSpecification {
            role: Role::Member,
            federated_identities: vec![FederatedIdentity {
                issuer: String::from(CI_ISSUER),
                subject: String::from(CI_SUBJECT),
                audience: Some(String::from("amafu")),
            }],
        };
        api.member.create::<ServiceAccount>(&ObjectName::new("ci").unwrap(), &account).await.unwrap();
        let workload = |subject: &str, key: &KeyPair| {
            Claims::new(CI_ISSUER, subject, "amafu", now(), Duration::minutes(5)).sign(key).unwrap().token
        };
        let request = |token: String| json!({"grant_type": "urn:ietf:params:oauth:grant-type:token-exchange", "client_id": "web-team/ci", "subject_token": token});
        let (status, token) = exchange(&api, &request(workload(CI_SUBJECT, &api.ci_key))).await;
        assert_eq!(status, 200);
        let member = as_service_account(&api, &token).in_project(ProjectName::new("web-team").unwrap());
        assert!(member.list::<Vm>().await.is_ok());
        assert_eq!(exchange(&api, &request(workload("repo:other", &api.ci_key))).await.0, 400);
        let forger = KeyPair::generate().unwrap();
        assert_eq!(exchange(&api, &request(workload(CI_SUBJECT, &forger))).await.0, 400);
    }
}
