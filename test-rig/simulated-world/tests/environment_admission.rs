#[cfg(test)]
mod environment_admission {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use amafu_api::authentication::{TokenAuthenticator, TokenIdentity};
    use amafu_api::identity::Principal;
    use amafu_api::routes::{ApiState, router};
    use amafu_api::{ApiClient, ClientError, RouterTransport, Transport};
    use amafu_domain::access::Role;
    use amafu_domain::kinds::juju_controller::{
        JujuControllerChange, JujuControllerChangeSpecification, JujuControllerSpecification, JujuRequest,
        JujuVersion, SignInMode, Substrate,
    };
    use amafu_domain::kinds::kubernetes_cluster::{
        KubernetesClusterChange, KubernetesClusterChangeSpecification, KubernetesClusterSpecification,
        KubernetesRequest, KubernetesVersion, NodePool,
    };
    use amafu_domain::kinds::machine_shape::MachineShape;
    use amafu_domain::kinds::network::{Network, NetworkSpecification};
    use amafu_domain::{ObjectName, ProjectName};
    use amafu_interfaces::{AuditLog, Identifiers, Store};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore, SeededIdentifiers};
    use time::OffsetDateTime;

    const TOKEN: &str = "amafu-member-token";

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn api() -> ApiClient {
        let store = Arc::new(MemoryStore::new());
        let member = Principal {
            name: String::from("alice"),
            role: Role::Member,
            project: ProjectName::new("web").unwrap(),
        };
        let state = ApiState {
            store: store as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(vec![TokenIdentity::for_token(
                member, TOKEN, None,
            )])),
            audit: Arc::new(MemoryAuditLog::new()) as Arc<dyn AuditLog>,
            clock: Arc::new(VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH)),
            identifiers: Arc::new(SeededIdentifiers::new(7)) as Arc<dyn Identifiers>,
            issuers: Arc::new(amafu_api::service_accounts::HttpIssuerDirectory::default()),
            key_directory: Arc::new(amafu_api::key_directory::HttpKeyDirectory::default()),
            catalog: Arc::new(amafu_interfaces::FixedImageCatalog::default()),
        };
        let transport: Arc<dyn Transport> = Arc::new(RouterTransport::new(router(state)));
        ApiClient::new(transport, Some(String::from(TOKEN))).in_project(ProjectName::new("web").unwrap())
    }

    fn cluster() -> KubernetesClusterSpecification {
        KubernetesClusterSpecification {
            version: KubernetesVersion::new(1, 34),
            control_plane_nodes: 1,
            control_plane_shape: MachineShape::Small,
            node_pools: BTreeMap::from([(
                name("general"),
                NodePool { nodes: 1, shape: MachineShape::Small },
            )]),
            network: name("apps"),
            storage_backend: name("shared"),
            external_network: name("public"),
            organization_sign_in: false,
        }
    }

    fn controller() -> JujuControllerSpecification {
        JujuControllerSpecification {
            version: JujuVersion::new(3, 6, 29),
            substrate: Substrate::Pods {
                pods: 1,
                controllers: 1,
                shape: MachineShape::Small,
                network: name("apps"),
                storage_backend: name("shared"),
                external_network: name("public"),
            },
            first_model: name("default"),
            sign_in: SignInMode::Amafu,
        }
    }

    async fn create_environments(client: &ApiClient, attempt: &str) -> [Result<(), ClientError>; 2] {
        let kubernetes = KubernetesClusterChangeSpecification::new(
            name(&format!("cluster-{attempt}")),
            KubernetesRequest::Create { cluster: cluster() },
        );
        let juju = JujuControllerChangeSpecification::new(
            name(&format!("controller-{attempt}")),
            JujuRequest::Create { controller: controller() },
        );
        [
            client
                .create::<KubernetesClusterChange>(&name(&format!("create-cluster-{attempt}")), &kubernetes)
                .await
                .map(drop),
            client
                .create::<JujuControllerChange>(&name(&format!("create-controller-{attempt}")), &juju)
                .await
                .map(drop),
        ]
    }

    #[tokio::test]
    async fn environments_need_a_network_that_lets_their_vms_reach_the_platform_api() {
        let client = api();
        let closed = NetworkSpecification {
            subnet: "10.70.0.0/24".parse().unwrap(),
            is_outbound_access_allowed: true,
            is_platform_api_allowed: false,
        };
        let network = client.create::<Network>(&name("apps"), &closed).await.unwrap();
        for refused in create_environments(&client, "first").await {
            let error = refused.unwrap_err();
            assert!(error.to_string().contains("allow-platform-api apps"), "{error}");
        }
        let open = NetworkSpecification { is_platform_api_allowed: true, ..closed };
        client.update::<Network>("apps", &open, Some(network.metadata.generation)).await.unwrap();
        for admitted in create_environments(&client, "second").await {
            admitted.unwrap();
        }
    }
}
