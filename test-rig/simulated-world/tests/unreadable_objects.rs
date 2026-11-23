#[cfg(test)]
mod unreadable_objects {
    use amafu_domain::kinds::juju_controller::JujuControllerChange;
    use amafu_domain::kinds::network::{Network, NetworkSpecification};
    use amafu_domain::registry::{KindDescriptor, UntypedChange};
    use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference, ProjectName, Resource};
    use amafu_interfaces::objects::{ObjectStoreError, apply, apply_untyped, erase_unreadable};
    use amafu_interfaces::{Consistency, Store};
    use amafu_simulated_world::fakes::MemoryStore;
    use serde_json::{Value, json};

    fn reference<R: Resource>(name: &str) -> ObjectReference {
        ObjectReference::of::<R>(Some(ProjectName::new("web").unwrap()), ObjectName::new(name).unwrap())
    }

    fn stored(name: &str, specification: &Value) -> Value {
        json!({
            "metadata": {"id": "e9ff025f-6306-4509-9c40-2c5fd3aaf185", "name": name, "project": "web",
                         "generation": 1, "observed_generation": 1, "version": 1, "is_deleting": false},
            "specification": specification,
            "status": {},
        })
    }

    #[tokio::test]
    async fn an_object_this_release_cannot_read_is_erased_instead_of_deleted() {
        let store = MemoryStore::new();
        let retired = reference::<JujuControllerChange>("create-landscape");
        let shape = json!({"controller": "landscape", "request": {"kind": "create", "controller": {
            "version": "3.6.29", "first_model": "default",
            "substrate": {"on": "vms", "controllers": 1, "shape": "small", "network": "private",
                          "storage_backend": "local", "external_network": "lab"}}}});
        store.compare_and_swap(&retired, None, Some(stored("create-landscape", &shape))).await.unwrap();
        let descriptor = KindDescriptor::of::<JujuControllerChange>();
        let deleting = apply_untyped(&store, &descriptor, &retired, UntypedChange::Delete).await;
        assert!(matches!(deleting, Err(ObjectStoreError::Corrupt(_))));
        let erased = erase_unreadable(&store, &descriptor, &retired).await.unwrap();
        assert_eq!(erased["metadata"]["name"], "create-landscape");
        assert!(store.read(&retired, Consistency::Linearizable).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_readable_object_is_never_erased_outright() {
        let store = MemoryStore::new();
        let network = reference::<Network>("apps");
        let specification = NetworkSpecification {
            subnet: "10.70.0.0/24".parse().unwrap(),
            is_outbound_access_allowed: true,
            is_platform_api_allowed: true,
        };
        let create = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::nil()),
            name: network.name.clone(),
            project: network.project.clone(),
            labels: std::collections::BTreeMap::new(),
            specification,
        };
        apply::<Network>(&store, &network, create).await.unwrap();
        let descriptor = KindDescriptor::of::<Network>();
        assert!(erase_unreadable(&store, &descriptor, &network).await.is_err());
        assert!(store.read(&network, Consistency::Linearizable).await.unwrap().is_some());
    }
}
