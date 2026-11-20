#[cfg(test)]
mod platform_tokens {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use amafu_api::authentication::{TokenAuthenticator, TokenIdentity};
    use amafu_api::identity::Principal;
    use amafu_api::routes::{ApiState, router};
    use amafu_api::{ApiClient, RouterTransport, Transport};
    use amafu_domain::access::Role;
    use amafu_domain::kinds::kubernetes_cluster::{KubernetesCluster, KubernetesClusterStatus};
    use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
    use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference, ProjectName, Resource};
    use amafu_interfaces::objects::apply;
    use amafu_interfaces::{AuditLog, Identifiers, Store};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore, SeededIdentifiers};
    use amafu_token_issuer::{Claims, JwkSet, KeyPair, PublicKey};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use http::Method;
    use time::{Duration, OffsetDateTime};

    const MEMBER_TOKEN: &str = "amafu-member-token";

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    async fn create<R: Resource>(
        store: &MemoryStore,
        project: Option<ProjectName>,
        name: &str,
        specification: R::Specification,
    ) {
        let name = ObjectName::new(name).unwrap();
        let reference = ObjectReference::of::<R>(project.clone(), name.clone());
        let change = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name,
            project,
            labels: BTreeMap::new(),
            specification,
        };
        apply::<R>(store, &reference, change).await.unwrap();
    }

    async fn api_with_a_cluster(key: &KeyPair) -> (ApiClient, ApiClient) {
        let store = Arc::new(MemoryStore::new());
        let project = ProjectName::new("web-team").unwrap();
        let signing = SigningKeySpecification::window_starting(
            STANDARD.encode(key.pkcs8()),
            now() - Duration::hours(1),
        );
        create::<SigningKey>(&store, None, "key-1", signing).await;
        let cluster = serde_json::from_value(serde_json::json!({
            "version": "1.34", "control_plane_nodes": 1, "control_plane_shape": "medium",
            "network": "platform", "storage_backend": "shared", "external_network": "public",
        }))
        .unwrap();
        create::<KubernetesCluster>(&store, Some(project.clone()), "platform", cluster).await;
        let reference = ObjectReference::of::<KubernetesCluster>(
            Some(project.clone()),
            ObjectName::new("platform").unwrap(),
        );
        let status = KubernetesClusterStatus {
            uid: Some(String::from("cluster-uid")),
            ..KubernetesClusterStatus::default()
        };
        apply::<KubernetesCluster>(
            &*store,
            &reference,
            Change::UpdateStatus { observed_generation: 1, status },
        )
        .await
        .unwrap();
        let member = Principal { name: String::from("alice"), role: Role::Member, project };
        let state = ApiState {
            store: store as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(vec![TokenIdentity::for_token(
                member,
                MEMBER_TOKEN,
                None,
            )])),
            audit: Arc::new(MemoryAuditLog::new()) as Arc<dyn AuditLog>,
            clock: Arc::new(VirtualClock::starting_at(now())),
            identifiers: Arc::new(SeededIdentifiers::new(1)) as Arc<dyn Identifiers>,
        };
        let transport: Arc<dyn Transport> = Arc::new(RouterTransport::new(router(state)));
        (
            ApiClient::new(Arc::clone(&transport), Some(String::from(MEMBER_TOKEN))),
            ApiClient::new(transport, None),
        )
    }

    fn published_key(set: &JwkSet, key: &KeyPair) -> PublicKey {
        assert_eq!(set.keys.len(), 1);
        assert_eq!(set.keys[0].kid, key.public_key().key_id());
        key.public_key()
    }

    #[tokio::test]
    async fn a_member_gets_a_short_kubernetes_token_that_the_published_keys_verify() {
        let key = KeyPair::generate().unwrap();
        let (client, anonymous) = api_with_a_cluster(&key).await;
        let minted = client.raw(Method::POST, "/v1/kubernetes-clusters/platform/tokens", None).await.unwrap();
        assert_eq!(minted.status, 201);
        let issued: amafu_token_issuer::Issued = serde_json::from_slice(&minted.body).unwrap();
        let keys = anonymous.raw(Method::GET, "/v1/jwks", None).await.unwrap();
        assert_eq!(keys.status, 200);
        let set: JwkSet = serde_json::from_slice(&keys.body).unwrap();
        let claims = Claims::verified(&issued.token, &published_key(&set, &key)).unwrap();
        assert_eq!(claims.aud, "cluster-uid");
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.exp - claims.iat, 300);
        assert_eq!(claims.extra["groups"], serde_json::json!(["amafu:web-team:member"]));
        let discovery = anonymous.raw(Method::GET, "/.well-known/openid-configuration", None).await.unwrap();
        assert_eq!(discovery.status, 200);
    }

    #[tokio::test]
    async fn tokens_need_credentials_and_an_existing_cluster() {
        let key = KeyPair::generate().unwrap();
        let (client, anonymous) = api_with_a_cluster(&key).await;
        let refused =
            anonymous.raw(Method::POST, "/v1/kubernetes-clusters/platform/tokens", None).await.unwrap();
        assert_eq!(refused.status, 401);
        let missing =
            client.raw(Method::POST, "/v1/kubernetes-clusters/elsewhere/tokens", None).await.unwrap();
        assert_eq!(missing.status, 404);
    }
}
