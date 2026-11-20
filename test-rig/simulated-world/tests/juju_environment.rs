#[cfg(test)]
mod juju_environment {
    use std::collections::BTreeMap;

    use amafu_controllers::environments::juju::{BOOTSTRAP_KEY_COMMENT, JujuSteps};
    use amafu_domain::Object;
    use amafu_domain::kinds::environment::EnvironmentPhase;
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
    use amafu_domain::labels::JUJU_MODEL;
    use amafu_simulated_world::environments::{EnvironmentWorld, SERVICE_ADDRESS, name};

    fn on_vms(controllers: u8) -> JujuControllerSpecification {
        JujuControllerSpecification {
            version: JujuVersion::new(4, 2, 1),
            substrate: Substrate::Vms {
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

    async fn model_vms(world: &EnvironmentWorld) -> Vec<Object<Vm>> {
        world
            .owned::<Vm>()
            .await
            .into_iter()
            .filter(|vm| vm.metadata.labels.get(JUJU_MODEL).map(String::as_str) == Some("default"))
            .collect()
    }

    #[tokio::test]
    async fn a_controller_on_vms_bootstraps_scales_out_and_runs_applications_on_amafu_machines() {
        let world = EnvironmentWorld::new().await;
        let steps = world.juju_steps();
        let created = change(&world, &steps, "create", JujuRequest::Create { controller: on_vms(3) }).await;
        assert_eq!(created, OperationOutcome::Succeeded);
        let found = controller(&world).await.unwrap();
        assert_eq!(found.status.phase, EnvironmentPhase::Ready);
        assert_eq!(found.status.uuid.as_deref(), Some("controller-uuid"));
        assert_eq!(
            found.status.models.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(),
            ["default"]
        );
        let controllers = world.juju.machines("model-controller");
        assert_eq!(controllers.iter().filter(|machine| !machine.units.is_empty()).count(), 3);
        assert_eq!(world.juju.removed_keys(), [BOOTSTRAP_KEY_COMMENT]);
        let vms = world.owned::<Vm>().await;
        assert!(!vms.iter().any(|vm| matches!(vm.specification.boot, BootSource::Container { .. })));
        assert_eq!(vms.len(), 3);

        let deployment = Deployment {
            model: name("default"),
            application: name("postgresql"),
            charm: String::from("postgresql"),
            channel: Some(String::from("16/stable")),
            units: 2,
            shape: MachineShape::Large,
            expose: true,
        };
        assert_eq!(
            change(&world, &steps, "deploy", JujuRequest::Deploy { deployment }).await,
            OperationOutcome::Succeeded
        );
        let machines = model_vms(&world).await;
        assert_eq!(machines.len(), 2);
        assert!(world.juju.machines("model-default").iter().all(|machine| machine.units.len() == 1));
        assert_eq!(world.juju.exposed(), ["postgresql"]);

        let remove = JujuRequest::RemoveUnits {
            model: name("default"),
            application: name("postgresql"),
            units: vec![String::from("postgresql/0")],
        };
        assert_eq!(change(&world, &steps, "shrink", remove).await, OperationOutcome::Succeeded);
        assert_eq!(model_vms(&world).await.len(), 1);
        assert_eq!(world.juju.machines("model-default").len(), 1);

        let survivor = model_vms(&world).await.remove(0);
        let lost_machine = survivor.metadata.labels["amafu.io/juju-machine"].clone();
        world.forget::<Vm>(survivor.metadata.name.as_str()).await;
        let replace = JujuRequest::ReplaceMachine { model: name("default"), machine: lost_machine.clone() };
        assert_eq!(change(&world, &steps, "replace", replace).await, OperationOutcome::Succeeded);
        let remaining = world.juju.machines("model-default");
        assert_eq!(remaining.len(), 1);
        assert_ne!(remaining[0].id, lost_machine);
        assert!(remaining[0].units.iter().any(|unit| unit.starts_with("postgresql/")));

        let upgrade = JujuRequest::Upgrade { version: JujuVersion::new(4, 2, 2) };
        assert_eq!(change(&world, &steps, "upgrade", upgrade).await, OperationOutcome::Succeeded);
        assert_eq!(controller(&world).await.unwrap().specification.version, JujuVersion::new(4, 2, 2));

        assert_eq!(change(&world, &steps, "delete", JujuRequest::Delete).await, OperationOutcome::Succeeded);
        assert!(controller(&world).await.is_none());
        assert_eq!(world.owned::<Vm>().await.len(), 0);
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
        let mut on_cluster = on_vms(1);
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
