#[cfg(test)]
mod kubernetes_environment {
    use std::collections::BTreeMap;
    use std::net::Ipv4Addr;
    use std::sync::Arc;

    use amafu_controllers::environments::kubernetes::{
        IssuerTrust, KubernetesEnvironment, KubernetesSettings, KubernetesSteps,
    };
    use amafu_domain::kinds::environment::EnvironmentPhase;
    use amafu_domain::kinds::floating_ip::FloatingIp;
    use amafu_domain::kinds::image::{Image, ImagePhase, ImageSource};
    use amafu_domain::kinds::kubernetes_cluster::{
        ClusterMember, KubernetesCluster, KubernetesClusterChange, KubernetesClusterChangeSpecification,
        KubernetesClusterSpecification, KubernetesRequest, KubernetesVersion, NodePool,
    };
    use amafu_domain::kinds::machine_shape::MachineShape;
    use amafu_domain::kinds::operation::OperationOutcome;
    use amafu_domain::kinds::security_group::SecurityGroup;
    use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
    use amafu_domain::kinds::vm::{PowerState, Vm, VmPhase};
    use amafu_domain::kinds::volume::Volume;
    use amafu_domain::{Change, Object, ObjectId, ObjectName, ObjectReference, ProjectName, Resource};
    use amafu_interfaces::objects::{apply, get, list, remove, update_status};
    use amafu_interfaces::{Consistency, Fence, Store};
    use amafu_reconciler::{OperationContext, advance};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{FakeKubernetes, MemoryStore, SeededIdentifiers};
    use amafu_token_issuer::{KeyPair, Keyring};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use time::{Duration, OffsetDateTime};

    const PASSES: usize = 400;
    const API_ADDRESS: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn project() -> ProjectName {
        ProjectName::new("web-team").unwrap()
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn cluster_specification() -> KubernetesClusterSpecification {
        KubernetesClusterSpecification {
            version: KubernetesVersion::new(1, 34),
            control_plane_nodes: 1,
            control_plane_shape: MachineShape::Medium,
            node_pools: BTreeMap::from([(
                name("general"),
                NodePool { nodes: 2, shape: MachineShape::Large },
            )]),
            network: name("platform"),
            storage_backend: name("shared"),
            external_network: name("public"),
            organization_sign_in: false,
        }
    }

    struct World {
        store: Arc<MemoryStore>,
        platform: Arc<FakeKubernetes>,
        steps: KubernetesSteps,
        clock: VirtualClock,
        fence: Fence,
    }

    impl World {
        async fn new() -> Self {
            let store = Arc::new(MemoryStore::new());
            let key = KeyPair::generate().unwrap();
            let signing = SigningKeySpecification::window_starting(STANDARD.encode(key.pkcs8()), now());
            create::<SigningKey>(&*store, None, "key-1", signing).await;
            let platform = Arc::new(FakeKubernetes::new());
            let environment = KubernetesEnvironment {
                platform: Arc::clone(&platform) as _,
                keyring: Keyring::new(Arc::clone(&store) as Arc<dyn Store>),
                identifiers: Arc::new(SeededIdentifiers::new(7)),
                settings: KubernetesSettings {
                    ubuntu_image: Some(ImageSource::Disk {
                        url: String::from("https://images.example/noble.img"),
                        sha256: "0".repeat(64).parse().unwrap(),
                    }),
                    trust: IssuerTrust {
                        issuer: String::from("https://amafu.internal"),
                        discovery_url: String::from("https://amafu.example/.well-known/openid-configuration"),
                        certificate_authority: String::from("amafu-ca"),
                    },
                },
            };
            Self {
                store,
                platform,
                steps: KubernetesSteps::new(environment),
                clock: VirtualClock::starting_at(now()),
                fence: Fence { name: String::from("kubernetes-changes"), token: 1 },
            }
        }

        async fn owned<R: Resource>(&self) -> Vec<Object<R>> {
            list::<R>(&*self.store, Some(&project()), Consistency::Linearizable).await.unwrap()
        }

        async fn cluster(&self) -> Option<Object<KubernetesCluster>> {
            get::<KubernetesCluster>(
                &*self.store,
                &reference::<KubernetesCluster>("platform"),
                Consistency::Linearizable,
            )
            .await
            .unwrap()
        }

        async fn run(&self, change: &str, request: KubernetesRequest) -> OperationOutcome {
            let specification = KubernetesClusterChangeSpecification::new(name("platform"), request);
            create::<KubernetesClusterChange>(&*self.store, Some(project()), change, specification).await;
            let reference = reference::<KubernetesClusterChange>(change);
            for _ in 0..PASSES {
                let operation =
                    get::<KubernetesClusterChange>(&*self.store, &reference, Consistency::Linearizable)
                        .await
                        .unwrap()
                        .unwrap();
                if let Some(outcome) = operation.status.outcome.clone() {
                    return outcome;
                }
                let context = OperationContext {
                    operation: &operation,
                    store: self.store.as_ref(),
                    clock: &self.clock,
                    fence: &self.fence,
                };
                drop(advance(&context, &self.steps).await);
                self.settle().await;
            }
            panic!("the change {change} did not finish");
        }

        async fn settle(&self) {
            for image in self.owned::<Image>().await {
                set_status::<Image>(&*self.store, &image, |status| status.phase = ImagePhase::Ready).await;
            }
            for address in self.owned::<FloatingIp>().await {
                set_status::<FloatingIp>(&*self.store, &address, |status| status.address = Some(API_ADDRESS))
                    .await;
            }
            for vm in self.owned::<Vm>().await {
                let phase = match vm.specification.power_state {
                    PowerState::Running => VmPhase::Running,
                    PowerState::Stopped => VmPhase::Stopped,
                };
                set_status::<Vm>(&*self.store, &vm, |status| status.phase = phase).await;
                if vm.metadata.name.as_str() == "platform-cp-0" && self.platform.nodes().is_empty() {
                    self.platform.bootstrap("platform-cp-0");
                }
            }
            self.forget_deleted::<Vm>().await;
            self.forget_deleted::<Volume>().await;
            self.forget_deleted::<FloatingIp>().await;
            self.forget_deleted::<SecurityGroup>().await;
        }

        async fn forget_deleted<R: Resource>(&self) {
            for object in self.owned::<R>().await.into_iter().filter(|object| object.metadata.is_deleting) {
                remove::<R>(&*self.store, &object.reference()).await.unwrap();
            }
        }
    }

    fn reference<R: Resource>(object: &str) -> ObjectReference {
        ObjectReference::of::<R>(Some(project()), name(object))
    }

    async fn create<R: Resource>(
        store: &dyn Store,
        project: Option<ProjectName>,
        object: &str,
        specification: R::Specification,
    ) {
        let reference = ObjectReference::of::<R>(project.clone(), name(object));
        let change = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name: name(object),
            project,
            labels: BTreeMap::new(),
            specification,
        };
        apply::<R>(store, &reference, change).await.unwrap();
    }

    async fn set_status<R: Resource>(
        store: &dyn Store,
        object: &Object<R>,
        change: impl Fn(&mut R::Status) + Send + Sync,
    ) where
        R::Status: PartialEq,
    {
        update_status::<R>(store, &object.reference(), |current| {
            let mut status = current.status.clone();
            change(&mut status);
            (status != current.status).then_some(status)
        })
        .await
        .unwrap();
    }

    fn members(cluster: &Object<KubernetesCluster>) -> Vec<ClusterMember> {
        cluster.status.nodes.iter().map(|node| node.member.clone()).collect()
    }

    fn workers(cluster: &Object<KubernetesCluster>) -> Vec<ClusterMember> {
        members(cluster).into_iter().filter(|member| !member.is_control_plane()).collect()
    }

    #[tokio::test]
    async fn a_cluster_is_created_scaled_upgraded_and_deleted_without_running_commands_on_its_nodes() {
        let world = World::new().await;
        let created =
            world.run("create", KubernetesRequest::Create { cluster: cluster_specification() }).await;
        assert_eq!(created, OperationOutcome::Succeeded);
        let cluster = world.cluster().await.unwrap();
        assert_eq!(cluster.status.phase, EnvironmentPhase::Ready);
        assert_eq!(cluster.status.endpoint, Some(API_ADDRESS));
        assert!(cluster.status.uid.is_some());
        assert_eq!(cluster.status.nodes.len(), 3);
        assert!(cluster.status.nodes.iter().all(|node| node.is_ready));
        assert_eq!(world.platform.nodes().len(), 3);
        assert_eq!(world.platform.applied().len(), 4);
        assert!(world.platform.api_tokens_seen() > 0);
        let vms = world.owned::<Vm>().await;
        assert_eq!(vms.len(), 3);
        assert!(vms.iter().all(|vm| vm.metadata.labels["amafu.io/kubernetes-cluster"] == "platform"));
        let bootstrap = vms.iter().find(|vm| vm.metadata.name.as_str() == "platform-cp-0").unwrap();
        assert!(bootstrap.specification.user_data.as_deref().unwrap().contains("set-auth-token"));

        let scale = KubernetesRequest::ScalePool { pool: name("general"), nodes: workers(&cluster), to: 1 };
        assert_eq!(world.run("scale", scale).await, OperationOutcome::Succeeded);
        let cluster = world.cluster().await.unwrap();
        assert_eq!(cluster.specification.node_pools[&name("general")].nodes, 1);
        assert_eq!(workers(&cluster).len(), 1);
        assert_eq!(world.platform.nodes().len(), 2);

        let upgrade =
            KubernetesRequest::Upgrade { version: KubernetesVersion::new(1, 35), nodes: members(&cluster) };
        assert_eq!(world.run("upgrade", upgrade).await, OperationOutcome::Succeeded);
        let cluster = world.cluster().await.unwrap();
        assert_eq!(cluster.specification.version, KubernetesVersion::new(1, 35));
        assert!(cluster.status.nodes.iter().all(|node| node.version == KubernetesVersion::new(1, 35)));
        assert!(!world.platform.nodes().contains_key("platform-cp-0"));
        let api = get::<FloatingIp>(
            &*world.store,
            &reference::<FloatingIp>("platform-api"),
            Consistency::Linearizable,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(api.specification.target.unwrap().vm, name("platform-cp-1"));

        let restart = KubernetesRequest::RestartNode { member: workers(&cluster)[0].clone() };
        assert_eq!(world.run("restart", restart).await, OperationOutcome::Succeeded);
        assert!(world.platform.nodes().values().all(|node| !node.is_cordoned));

        assert_eq!(world.run("delete", KubernetesRequest::Delete).await, OperationOutcome::Succeeded);
        assert!(world.cluster().await.is_none());
        assert_eq!(world.owned::<Vm>().await.len(), 0);
        assert_eq!(world.owned::<Volume>().await.len(), 0);
        assert_eq!(world.owned::<SecurityGroup>().await.len(), 0);
    }
}
