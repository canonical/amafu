#[cfg(test)]
mod cluster_address {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use amafu_api::authentication::{TokenAuthenticator, TokenIdentity};
    use amafu_api::identity::Principal;
    use amafu_api::routes::{ApiState, router};
    use amafu_api::{ApiClient, RouterTransport, Transport};
    use amafu_domain::access::Role;
    use amafu_domain::features::{Feature, FeatureLevel};
    use amafu_domain::kinds::cluster_address::{
        ClusterAddress, ClusterAddressSpecification, DashboardSessionKey, DashboardSessionKeySpecification,
        cluster_address_name,
    };
    use amafu_domain::kinds::cluster_version::{
        ClusterVersion, ClusterVersionSpecification, cluster_version_name,
    };
    use amafu_domain::node_image::NodeImage;
    use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference, ProjectName};
    use amafu_interfaces::objects::apply;
    use amafu_interfaces::{AuditLog, Identifiers, Store};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore, SeededIdentifiers};
    use time::OffsetDateTime;

    const TOKEN: &str = "amafu-admin-token";
    const UNPROCESSABLE_OR_CONFLICT: [u16; 2] = [409, 422];

    fn api() -> (ApiClient, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::new());
        let admin = Principal {
            name: String::from("root"),
            role: Role::Admin,
            project: ProjectName::new("operations").unwrap(),
        };
        let state = ApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(vec![TokenIdentity::for_token(
                admin, TOKEN, None,
            )])),
            audit: Arc::new(MemoryAuditLog::new()) as Arc<dyn AuditLog>,
            clock: Arc::new(VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH)),
            identifiers: Arc::new(SeededIdentifiers::new(7)) as Arc<dyn Identifiers>,
            issuers: Arc::new(amafu_api::service_accounts::HttpIssuerDirectory::default()),
            key_directory: Arc::new(amafu_api::key_directory::HttpKeyDirectory::default()),
            catalog: Arc::new(amafu_interfaces::FixedImageCatalog::default()),
        };
        let transport: Arc<dyn Transport> = Arc::new(RouterTransport::new(router(state)));
        (ApiClient::new(transport, Some(String::from(TOKEN))), store)
    }

    async fn run_cluster_version(store: &MemoryStore, finalized: FeatureLevel) {
        let image = NodeImage {
            version: String::from("26.11.50"),
            feature_level: finalized,
            digest: "d".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: String::from("https://images.example/node-image.tar"),
            signature: "0".repeat(128),
        };
        let specification = ClusterVersionSpecification {
            finalized_feature_level: finalized,
            ..ClusterVersionSpecification::installed(image)
        };
        let reference = ObjectReference::of::<ClusterVersion>(None, cluster_version_name());
        let change = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name: cluster_version_name(),
            project: None,
            labels: BTreeMap::new(),
            specification,
        };
        apply::<ClusterVersion>(store, &reference, change).await.unwrap();
    }

    fn the_address() -> ClusterAddressSpecification {
        ClusterAddressSpecification {
            address: "10.20.0.50".parse().unwrap(),
            prefix_length: 24,
            interface: None,
        }
    }

    #[tokio::test]
    async fn the_cluster_address_waits_for_its_feature_level_to_be_finalized() {
        let (client, store) = api();
        run_cluster_version(&store, FeatureLevel(1)).await;
        let refused =
            client.create::<ClusterAddress>(&cluster_address_name(), &the_address()).await.unwrap_err();
        assert!(
            refused.status().is_some_and(|status| UNPROCESSABLE_OR_CONFLICT.contains(&status)),
            "{refused}"
        );
        assert!(refused.to_string().contains("feature level 2"), "{refused}");

        let (client, store) = api();
        run_cluster_version(&store, Feature::ClusterAddress.introduced_in()).await;
        let created = client.create::<ClusterAddress>(&cluster_address_name(), &the_address()).await.unwrap();
        assert_eq!(created.specification, the_address());
    }

    #[tokio::test]
    async fn a_node_secret_can_be_written_but_never_read_back() {
        let (client, store) = api();
        run_cluster_version(&store, Feature::ClusterAddress.introduced_in()).await;
        let name = ObjectName::new("dashboard").unwrap();
        let key = DashboardSessionKeySpecification { key_hexadecimal: "ab".repeat(32) };
        client.create::<DashboardSessionKey>(&name, &key).await.unwrap();
        let refused = client.get::<DashboardSessionKey>("dashboard").await.unwrap_err();
        assert_eq!(refused.status(), Some(403), "{refused}");
        assert_eq!(client.list::<DashboardSessionKey>().await.unwrap_err().status(), Some(403));
    }
}
