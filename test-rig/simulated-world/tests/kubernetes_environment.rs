#[cfg(test)]
mod kubernetes_environment {
    use std::collections::BTreeMap;

    use amafu_controllers::environments::kubernetes::KubernetesSteps;
    use amafu_domain::Object;
    use amafu_domain::kinds::environment::EnvironmentPhase;
    use amafu_domain::kinds::floating_ip::FloatingIp;
    use amafu_domain::kinds::kubernetes_cluster::{
        ClusterMember, KubernetesCluster, KubernetesClusterChange, KubernetesClusterChangeSpecification,
        KubernetesClusterSpecification, KubernetesRequest, KubernetesVersion, NodePool,
    };
    use amafu_domain::kinds::machine_shape::MachineShape;
    use amafu_domain::kinds::operation::OperationOutcome;
    use amafu_domain::kinds::security_group::SecurityGroup;
    use amafu_domain::kinds::vm::Vm;
    use amafu_domain::kinds::volume::Volume;
    use amafu_simulated_world::environments::{EnvironmentWorld, FLOATING_ADDRESS, name};

    pub fn cluster_specification() -> KubernetesClusterSpecification {
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

    async fn change(
        world: &EnvironmentWorld,
        steps: &KubernetesSteps,
        change: &str,
        request: KubernetesRequest,
    ) -> OperationOutcome {
        let specification = KubernetesClusterChangeSpecification::new(name("platform"), request);
        world.run::<KubernetesClusterChange>(steps, change, specification).await
    }

    async fn cluster(world: &EnvironmentWorld) -> Option<Object<KubernetesCluster>> {
        world.get::<KubernetesCluster>("platform").await
    }

    fn members(cluster: &Object<KubernetesCluster>) -> Vec<ClusterMember> {
        cluster.status.nodes.iter().map(|node| node.member.clone()).collect()
    }

    fn workers(cluster: &Object<KubernetesCluster>) -> Vec<ClusterMember> {
        members(cluster).into_iter().filter(|member| !member.is_control_plane()).collect()
    }

    #[tokio::test]
    async fn a_cluster_is_created_scaled_upgraded_and_deleted_without_running_commands_on_its_nodes() {
        let world = EnvironmentWorld::new().await;
        let steps = world.kubernetes_steps();
        let created =
            change(&world, &steps, "create", KubernetesRequest::Create { cluster: cluster_specification() })
                .await;
        assert_eq!(created, OperationOutcome::Succeeded);
        let found = cluster(&world).await.unwrap();
        assert_eq!(found.status.phase, EnvironmentPhase::Ready);
        assert_eq!(found.status.endpoint, Some(FLOATING_ADDRESS));
        assert!(found.status.uid.is_some());
        assert_eq!(found.status.nodes.len(), 3);
        assert!(found.status.nodes.iter().all(|node| node.is_ready));
        assert_eq!(world.kubernetes.nodes().len(), 3);
        assert_eq!(world.kubernetes.applied().len(), 4);
        assert!(world.kubernetes.api_tokens_seen() > 0);
        let vms = world.owned::<Vm>().await;
        assert_eq!(vms.len(), 3);
        assert!(vms.iter().all(|vm| vm.metadata.labels["amafu.io/kubernetes-cluster"] == "platform"));
        let bootstrap = vms.iter().find(|vm| vm.metadata.name.as_str() == "platform-cp-0").unwrap();
        assert!(bootstrap.specification.user_data.as_deref().unwrap().contains("set-auth-token"));

        let scale = KubernetesRequest::ScalePool { pool: name("general"), nodes: workers(&found), to: 1 };
        assert_eq!(change(&world, &steps, "scale", scale).await, OperationOutcome::Succeeded);
        let found = cluster(&world).await.unwrap();
        assert_eq!(found.specification.node_pools[&name("general")].nodes, 1);
        assert_eq!(workers(&found).len(), 1);
        assert_eq!(world.kubernetes.nodes().len(), 2);

        let upgrade =
            KubernetesRequest::Upgrade { version: KubernetesVersion::new(1, 35), nodes: members(&found) };
        assert_eq!(change(&world, &steps, "upgrade", upgrade).await, OperationOutcome::Succeeded);
        let found = cluster(&world).await.unwrap();
        assert_eq!(found.specification.version, KubernetesVersion::new(1, 35));
        assert!(found.status.nodes.iter().all(|node| node.version == KubernetesVersion::new(1, 35)));
        assert!(!world.kubernetes.nodes().contains_key("platform-cp-0"));
        let api = world.get::<FloatingIp>("platform-api").await.unwrap();
        assert_eq!(api.specification.target.unwrap().vm, name("platform-cp-1"));

        let restart = KubernetesRequest::RestartNode { member: workers(&found)[0].clone() };
        assert_eq!(change(&world, &steps, "restart", restart).await, OperationOutcome::Succeeded);
        assert!(world.kubernetes.nodes().values().all(|node| !node.is_cordoned));

        assert_eq!(
            change(&world, &steps, "delete", KubernetesRequest::Delete).await,
            OperationOutcome::Succeeded
        );
        assert!(cluster(&world).await.is_none());
        assert_eq!(world.owned::<Vm>().await.len(), 0);
        assert_eq!(world.owned::<Volume>().await.len(), 0);
        assert_eq!(world.owned::<SecurityGroup>().await.len(), 0);
    }
}
