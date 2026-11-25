#[cfg(test)]
mod planned_restart {
    use std::sync::{Arc, Mutex};

    use amafu_controllers::boot_slots::{
        BootSlotsAction, BootSlotsController, BootSlotsServices, BootSlotsSettings,
    };
    use amafu_domain::NodeName;
    use amafu_domain::kinds::node::{BootReport, BootSlot, StoreHealth};
    use amafu_domain::kinds::node_lease::LeaseTerms;
    use amafu_domain::node_image::NodeImage;
    use amafu_interfaces::{
        ActionError, BootSlotError, BootSlots, ClusterAddressClaim, ConsensusMembership, NodeLeaseAuthority,
        NodeLeaseError, NodeLeaseGrant, NodeLeaseState,
    };
    use amafu_reconciler::Controller;
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::MemoryStore;
    use time::OffsetDateTime;

    type Steps = Arc<Mutex<Vec<&'static str>>>;

    fn record(steps: &Steps, step: &'static str) {
        steps.lock().unwrap().push(step);
    }

    struct RecordingNode {
        steps: Steps,
    }

    #[async_trait::async_trait]
    impl ClusterAddressClaim for RecordingNode {
        fn holds(&self) -> bool {
            true
        }

        async fn step_aside(&self) {
            record(&self.steps, "hand the cluster address on");
        }
    }

    #[async_trait::async_trait]
    impl ConsensusMembership for RecordingNode {
        fn health(&self) -> Option<StoreHealth> {
            None
        }

        async fn hand_off_leadership(&self) -> Result<(), ActionError> {
            record(&self.steps, "hand store leadership on");
            Ok(())
        }

        async fn close(&self) {
            record(&self.steps, "close the store");
        }
    }

    #[async_trait::async_trait]
    impl BootSlots for RecordingNode {
        fn report(&self) -> BootReport {
            BootReport {
                booted_slot: BootSlot::First,
                default_slot: BootSlot::First,
                booted_image: None,
                inactive_image: None,
                fallen_back_from: None,
                refused: None,
            }
        }

        async fn write_inactive_slot(&self, _image: &NodeImage) -> Result<(), BootSlotError> {
            Ok(())
        }

        async fn boot_inactive_slot_next(&self) -> Result<(), BootSlotError> {
            record(&self.steps, "boot the other slot next");
            Ok(())
        }

        async fn confirm_booted_slot(&self) -> Result<(), BootSlotError> {
            Ok(())
        }

        async fn abandon_trial(&self) -> Result<(), BootSlotError> {
            Ok(())
        }

        async fn reboot(&self) -> Result<(), BootSlotError> {
            record(&self.steps, "reboot");
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl NodeLeaseAuthority for RecordingNode {
        async fn acquire(&self, _terms: LeaseTerms) -> Result<NodeLeaseGrant, NodeLeaseError> {
            unreachable!("a planned restart never acquires a lease")
        }

        async fn renew(&self, _epoch: u64) -> Result<NodeLeaseGrant, NodeLeaseError> {
            unreachable!("a planned restart never renews a lease")
        }

        async fn surrender(&self, _epoch: u64) -> Result<(), NodeLeaseError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_node_hands_the_cluster_address_on_before_it_leaves_the_store_and_reboots() {
        let steps = Steps::default();
        let node = Arc::new(RecordingNode { steps: Arc::clone(&steps) });
        let controller = BootSlotsController::new(
            NodeName::new("node-1").unwrap(),
            BootSlotsServices {
                store: Arc::new(MemoryStore::new()),
                clock: Arc::new(VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH)),
                boot_slots: Arc::clone(&node) as Arc<dyn BootSlots>,
                leases: Arc::clone(&node) as Arc<dyn NodeLeaseAuthority>,
                lease: NodeLeaseState::new(),
                consensus: Arc::clone(&node) as Arc<dyn ConsensusMembership>,
                cluster_address: Arc::clone(&node) as Arc<dyn ClusterAddressClaim>,
            },
            BootSlotsSettings::default(),
        );
        controller.apply(&(), BootSlotsAction::BootInactiveSlot).await.unwrap();
        assert_eq!(
            *steps.lock().unwrap(),
            vec![
                "boot the other slot next",
                "hand the cluster address on",
                "hand store leadership on",
                "close the store",
                "reboot"
            ]
        );
    }
}
