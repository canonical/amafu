#[cfg(test)]
mod settling {
    use amafu_demo_cluster::DemoCluster;
    use amafu_domain::kinds::floating_ip::FloatingIp;
    use amafu_domain::kinds::vm::{PowerState, Vm, VmPhase};
    use amafu_interfaces::Consistency;
    use amafu_interfaces::objects::list;

    #[tokio::test]
    async fn the_seeded_cluster_settles_like_a_real_one() {
        let cluster = DemoCluster::start().await.unwrap();
        let vms = list::<Vm>(cluster.store.as_ref(), None, Consistency::Linearizable).await.unwrap();
        assert!(vms.len() > 10);
        for vm in &vms {
            match vm.specification.power_state {
                PowerState::Running => {
                    assert_eq!(vm.status.phase, VmPhase::Running, "{}", vm.metadata.name);
                    assert!(vm.status.node.is_some(), "{}", vm.metadata.name);
                }
                PowerState::Stopped => assert_eq!(vm.status.phase, VmPhase::Stopped),
            }
        }
        let floating_ips =
            list::<FloatingIp>(cluster.store.as_ref(), None, Consistency::Linearizable).await.unwrap();
        assert!(floating_ips.iter().all(|floating_ip| floating_ip.status.address.is_some()));
    }
}
