#[cfg(test)]
mod juju_environment {
    use std::collections::BTreeMap;

    use amafu_domain::Object;
    use amafu_domain::kinds::environment::EnvironmentPhase;
    use amafu_domain::kinds::environment_secret::EnvironmentSecret;
    use amafu_domain::kinds::floating_ip::FloatingIp;
    use amafu_domain::kinds::juju_controller::{
        Deployment, JujuController, JujuControllerChange, JujuControllerChangeSpecification,
        JujuControllerSpecification, JujuRequest, JujuVersion, SignInMode, Substrate,
    };
    use amafu_domain::kinds::kubernetes_cluster::{
        KubernetesCluster, KubernetesClusterChange, KubernetesClusterChangeSpecification,
        KubernetesClusterSpecification, KubernetesRequest, KubernetesVersion,
    };
    use amafu_domain::kinds::machine_shape::MachineShape;
    use amafu_domain::kinds::operation::OperationOutcome;
    use amafu_domain::kinds::vm::{BootSource, Vm};
    use amafu_interfaces::objects::list;
    use amafu_interfaces::{Consistency, JujuEndpoint, JujuPlatform, LxdEndpoint, LxdPlatform};
    use amafu_simulated_world::environments::{
        EnvironmentWorld, LXD_FLOATING_ADDRESS, SERVICE_ADDRESS, name,
    };

    use amafu_controllers::environments::juju::JujuSteps;

    fn on_pods(pods: u8, controllers: u8) -> JujuControllerSpecification {
        JujuControllerSpecification {
            version: JujuVersion::new(4, 2, 1),
            substrate: Substrate::Pods {
                pods,
                controllers,
                shape: MachineShape::Medium,
                network: name("platform"),
                storage_backend: name("shared"),
                external_network: name("public"),
            },
            first_model: name("default"),
            sign_in: SignInMode::Amafu,
        }
    }

    async fn change(
        world: &EnvironmentWorld,
        steps: &JujuSteps,
        change: &str,
        request: JujuRequest,
    ) -> OperationOutcome {
        let specification = JujuControllerChangeSpecification::new(name("observability"), request);
        world.run::<JujuControllerChange>(steps, change, specification).await
    }

    async fn controller(world: &EnvironmentWorld) -> Option<Object<JujuController>> {
        world.get::<JujuController>("observability").await
    }

    async fn pod_names(world: &EnvironmentWorld) -> Vec<String> {
        world.owned::<Vm>().await.iter().map(|vm| vm.metadata.name.to_string()).collect()
    }

    async fn members(world: &EnvironmentWorld) -> Vec<String> {
        let founder = LxdEndpoint {
            address: LXD_FLOATING_ADDRESS,
            certificate_pem: String::new(),
            private_key_pem: String::new(),
        };
        world.lxd.members(&founder).await.unwrap()
    }

    async fn units(world: &EnvironmentWorld, application: &str) -> Vec<String> {
        let endpoint = JujuEndpoint {
            address: SERVICE_ADDRESS,
            certificate_authority: String::new(),
            credentials: amafu_interfaces::JujuCredentials::Token(String::new()),
        };
        let applications = world.juju.applications(&endpoint, "model-default").await.unwrap();
        applications
            .into_iter()
            .find(|found| found.name == application)
            .map(|found| found.units)
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_controller_on_pods_forms_an_lxd_cluster_and_lets_juju_run_the_units() {
        let world = EnvironmentWorld::new().await;
        let steps = world.juju_steps();
        let created =
            change(&world, &steps, "create", JujuRequest::Create { controller: on_pods(3, 3) }).await;
        assert_eq!(created, OperationOutcome::Succeeded);
        let found = controller(&world).await.unwrap();
        assert_eq!(found.status.phase, EnvironmentPhase::Ready);
        assert_eq!(found.status.uuid.as_deref(), Some("controller-uuid"));
        assert_eq!(
            found.status.models.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(),
            ["default"]
        );
        assert_eq!(world.juju.machines("model-controller").len(), 3);
        let pods = ["observability-pod-0", "observability-pod-1", "observability-pod-2"];
        assert_eq!(pod_names(&world).await, pods);
        assert_eq!(members(&world).await, pods);
        let vms = world.owned::<Vm>().await;
        assert!(vms.iter().all(|vm| vm.specification.network_interfaces[0].guests > 0));
        assert!(!vms.iter().any(|vm| matches!(vm.specification.boot, BootSource::Container { .. })));
        let api = world.get::<FloatingIp>("observability-api").await.unwrap();
        let target = api.specification.target.unwrap();
        assert_eq!(target.vm, name("observability-pod-0"));
        assert!(target.guest_address.is_some());
        let secrets =
            list::<EnvironmentSecret>(&*world.store, None, Consistency::Linearizable).await.unwrap();
        assert!(
            secrets.iter().all(|secret| !secret
                .specification
                .values
                .keys()
                .any(|key| key.starts_with("ssh-")))
        );

        let deployment = Deployment {
            model: name("default"),
            application: name("postgresql"),
            charm: String::from("postgresql"),
            channel: Some(String::from("16/stable")),
            units: 2,
            expose: true,
        };
        assert_eq!(
            change(&world, &steps, "deploy", JujuRequest::Deploy { deployment }).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(units(&world, "postgresql").await, ["postgresql/0", "postgresql/1"]);
        assert_eq!(pod_names(&world).await.len(), 3);

        let scale = JujuRequest::ScaleApplication {
            model: name("default"),
            application: name("postgresql"),
            units: 3,
        };
        assert_eq!(change(&world, &steps, "grow", scale).await, OperationOutcome::Succeeded);
        assert_eq!(units(&world, "postgresql").await.len(), 3);
        let remove = JujuRequest::RemoveUnits {
            model: name("default"),
            application: name("postgresql"),
            units: vec![String::from("postgresql/0")],
        };
        assert_eq!(change(&world, &steps, "shrink", remove).await, OperationOutcome::Succeeded);
        assert_eq!(units(&world, "postgresql").await, ["postgresql/1", "postgresql/2"]);

        assert_eq!(
            change(&world, &steps, "add-pod", JujuRequest::ScalePods { pods: 4 }).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(members(&world).await.len(), 4);
        assert_eq!(pod_names(&world).await.len(), 4);
        assert_eq!(
            change(&world, &steps, "retire-pod", JujuRequest::ScalePods { pods: 3 }).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(members(&world).await, pods);
        assert_eq!(pod_names(&world).await, pods);

        let upgrade = JujuRequest::Upgrade { version: JujuVersion::new(4, 2, 2) };
        assert_eq!(change(&world, &steps, "upgrade", upgrade).await, OperationOutcome::Succeeded);
        assert_eq!(controller(&world).await.unwrap().specification.version, JujuVersion::new(4, 2, 2));

        assert_eq!(change(&world, &steps, "delete", JujuRequest::Delete).await, OperationOutcome::Succeeded);
        assert!(controller(&world).await.is_none());
        assert_eq!(world.owned::<Vm>().await.len(), 0);
    }

    #[tokio::test]
    async fn a_highly_available_controller_needs_three_pods() {
        let world = EnvironmentWorld::new().await;
        let steps = world.juju_steps();
        let created =
            change(&world, &steps, "create", JujuRequest::Create { controller: on_pods(1, 1) }).await;
        assert_eq!(created, OperationOutcome::Succeeded);
        assert!(matches!(
            change(&world, &steps, "ha", JujuRequest::EnableHa).await,
            OperationOutcome::Failed { .. }
        ));
        assert_eq!(
            change(&world, &steps, "grow", JujuRequest::ScalePods { pods: 3 }).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(
            change(&world, &steps, "ha-again", JujuRequest::EnableHa).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(world.juju.machines("model-controller").len(), 3);
        let substrate = controller(&world).await.unwrap().specification.substrate;
        assert_eq!(substrate.controller_count(), 3);
    }

    #[tokio::test]
    async fn a_controller_on_kubernetes_lives_on_its_cluster_and_rotates_its_credential() {
        let world = EnvironmentWorld::new().await;
        let cluster = KubernetesClusterSpecification {
            version: KubernetesVersion::new(1, 34),
            control_plane_nodes: 1,
            control_plane_shape: MachineShape::Medium,
            node_pools: BTreeMap::new(),
            network: name("platform"),
            storage_backend: name("shared"),
            external_network: name("public"),
            organization_sign_in: false,
        };
        let create_cluster = KubernetesClusterChangeSpecification::new(
            name("platform"),
            KubernetesRequest::Create { cluster },
        );
        let kubernetes_steps = world.kubernetes_steps();
        assert_eq!(
            world.run::<KubernetesClusterChange>(&kubernetes_steps, "create-cluster", create_cluster).await,
            OperationOutcome::Succeeded
        );

        let steps = world.juju_steps();
        let mut on_cluster = on_pods(1, 1);
        on_cluster.substrate = Substrate::Kubernetes { cluster: name("platform") };
        assert_eq!(
            change(&world, &steps, "create", JujuRequest::Create { controller: on_cluster }).await,
            OperationOutcome::Succeeded
        );
        let found = controller(&world).await.unwrap();
        assert_eq!(found.status.addresses, [SERVICE_ADDRESS.to_string()]);
        let host = world.get::<KubernetesCluster>("platform").await.unwrap();
        assert_eq!(host.status.hosted_juju_controllers, [name("observability")]);
        assert!(world.kubernetes.applied().contains("ServiceAccount/kube-system/juju-observability"));

        assert_eq!(
            change(&world, &steps, "rotate", JujuRequest::RotateCredential).await,
            OperationOutcome::Succeeded
        );
        assert_eq!(world.juju.credential().as_deref(), Some("fake-token"));
        assert!(!world.kubernetes.applied().contains("Secret/kube-system/juju-observability"));

        assert_eq!(change(&world, &steps, "delete", JujuRequest::Delete).await, OperationOutcome::Succeeded);
        let host = world.get::<KubernetesCluster>("platform").await.unwrap();
        assert_eq!(host.status.hosted_juju_controllers.len(), 0);
        assert!(!world.kubernetes.applied().iter().any(|object| object.contains("juju-observability")));
    }
}
