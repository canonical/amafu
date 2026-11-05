#[cfg(test)]
mod read_replica {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use amafu_domain::kinds::security_group::SecurityGroup;
    use amafu_domain::{ObjectName, ObjectReference};
    use amafu_interfaces::{Consistency, Store, WatchEvent};
    use amafu_simulated_world::fakes::MemoryStore;
    use amafu_state::ReadReplicaStore;
    use futures::StreamExt;
    use serde_json::json;

    const KIND: &str = "SecurityGroup";

    fn reference(name: &str) -> ObjectReference {
        ObjectReference::of::<SecurityGroup>(None, ObjectName::new(name).unwrap())
    }

    fn object(name: &str) -> serde_json::Value {
        json!({"metadata": {"name": name, "id": uuid::Uuid::from_u128(1), "version": 1, "generation": 1, "observed_generation": 0}})
    }

    async fn wait_for(replica: &ReadReplicaStore, name: &str, is_present: bool) {
        for _ in 0..200 {
            let found = replica.read(&reference(name), Consistency::Stale).await.unwrap();
            if found.is_some() == is_present {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("{name} did not become {is_present} in the replica");
    }

    #[tokio::test]
    async fn the_replica_follows_changes_and_announces_them_to_every_watcher() {
        let store = Arc::new(MemoryStore::new());
        let first =
            store.compare_and_swap(&reference("existing"), None, Some(object("existing"))).await.unwrap();
        let replica = ReadReplicaStore::start(Arc::clone(&store) as Arc<dyn Store>, &BTreeSet::from([KIND]));
        let mut watchers = [replica.watch(KIND), replica.watch(KIND)];
        for watcher in &mut watchers {
            assert_eq!(watcher.next().await, Some(WatchEvent::Resynchronize));
        }
        store.compare_and_swap(&reference("arrival"), None, Some(object("arrival"))).await.unwrap();
        wait_for(&replica, "arrival", true).await;
        for watcher in &mut watchers {
            assert_eq!(watcher.next().await, Some(WatchEvent::Changed(reference("arrival"))));
        }
        store.compare_and_swap(&reference("existing"), Some(first), None).await.unwrap();
        wait_for(&replica, "existing", false).await;
        let matching = replica
            .list_matching(KIND, None, Consistency::Stale, &|object| object["metadata"]["name"] == "arrival")
            .await
            .unwrap();
        assert_eq!(matching.len(), 1);
    }

    #[tokio::test]
    async fn linearizable_reads_never_come_from_the_replica() {
        let store = Arc::new(MemoryStore::new());
        let replica = ReadReplicaStore::start(Arc::clone(&store) as Arc<dyn Store>, &BTreeSet::new());
        store.compare_and_swap(&reference("fresh"), None, Some(object("fresh"))).await.unwrap();
        assert!(replica.read(&reference("fresh"), Consistency::Linearizable).await.unwrap().is_some());
    }
}

#[cfg(test)]
mod audit_export {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use amafu_controllers::AuditExportController;
    use amafu_domain::kinds::audit_sink::{AuditSink, AuditSinkSpecification};
    use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference};
    use amafu_interfaces::objects::{apply, get};
    use amafu_interfaces::{
        AuditEntry, AuditLog, AuditOutcome, AuditSinkDelivery, AuditSinks, Consistency, Fence,
        SequencedAuditEntry,
    };
    use amafu_reconciler::Controller;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore};

    #[derive(Default)]
    struct Collector {
        received: Mutex<Vec<u64>>,
        failures_left: Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl AuditSinkDelivery for Collector {
        async fn deliver(&self, entries: &[SequencedAuditEntry]) -> Result<(), String> {
            let mut failures_left = self.failures_left.lock().unwrap();
            if *failures_left > 0 {
                *failures_left -= 1;
                return Err(String::from("the collector is away"));
            }
            self.received.lock().unwrap().extend(entries.iter().map(|entry| entry.sequence));
            Ok(())
        }
    }

    struct Sinks(Arc<Collector>);

    impl AuditSinks for Sinks {
        fn sink(
            &self,
            _specification: &AuditSinkSpecification,
        ) -> Result<Arc<dyn AuditSinkDelivery>, String> {
            Ok(Arc::clone(&self.0) as Arc<dyn AuditSinkDelivery>)
        }
    }

    fn entry() -> AuditEntry {
        AuditEntry {
            request_id: uuid::Uuid::new_v4(),
            recorded_at: time::OffsetDateTime::UNIX_EPOCH,
            principal: None,
            role: None,
            verb: None,
            kind: None,
            project: None,
            name: None,
            method: String::from("GET"),
            path: String::from("/v1/vms"),
            outcome: AuditOutcome::Unauthenticated,
        }
    }

    async fn pass(
        controller: &AuditExportController,
        key: &ObjectReference,
    ) -> Result<(), amafu_interfaces::ActionError> {
        let observation = controller.observe(key).await?;
        for action in controller.plan(&observation) {
            controller.apply(key, action).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn every_entry_reaches_the_sink_once_it_answers_and_the_position_survives_a_restart() {
        let store = Arc::new(MemoryStore::new());
        let audit = Arc::new(MemoryAuditLog::new());
        let collector = Arc::new(Collector::default());
        *collector.failures_left.lock().unwrap() = 1;
        let name = ObjectName::new("collector").unwrap();
        let key = ObjectReference::of::<AuditSink>(None, name.clone());
        let specification = AuditSinkSpecification::Syslog {
            address: String::from("collector.example:514"),
            server_name: None,
            certificate_authority: None,
        };
        let created = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name,
            project: None,
            labels: BTreeMap::new(),
            specification,
        };
        apply::<AuditSink>(store.as_ref(), &key, created).await.unwrap();
        for _ in 0..3 {
            audit.record(entry()).await.unwrap();
        }
        let controller = |collector: &Arc<Collector>| {
            AuditExportController::new(
                Arc::clone(&store) as Arc<dyn amafu_interfaces::Store>,
                Arc::clone(&audit) as Arc<dyn AuditLog>,
                Arc::new(Sinks(Arc::clone(collector))),
                Fence { name: String::from("audit-export"), token: 1 },
            )
        };
        assert!(pass(&controller(&collector), &key).await.is_err());
        let failed =
            get::<AuditSink>(store.as_ref(), &key, Consistency::Linearizable).await.unwrap().unwrap();
        assert_eq!(failed.status.next_sequence, 0);
        assert!(failed.status.last_failure.is_some());
        pass(&controller(&collector), &key).await.unwrap();
        audit.record(entry()).await.unwrap();
        pass(&controller(&collector), &key).await.unwrap();
        pass(&controller(&collector), &key).await.unwrap();
        assert_eq!(*collector.received.lock().unwrap(), vec![0, 1, 2, 3]);
        let current =
            get::<AuditSink>(store.as_ref(), &key, Consistency::Linearizable).await.unwrap().unwrap();
        assert_eq!(current.status.next_sequence, 4);
        assert_eq!(current.status.last_failure, None);
    }
}
