#[cfg(test)]
mod ssh_keys {
    use std::sync::Arc;

    use amafu_api::authentication::{TokenAuthenticator, TokenIdentity};
    use amafu_api::identity::Principal;
    use amafu_api::key_directory::{DirectoryError, KeyDirectory};
    use amafu_api::routes::{ApiState, router};
    use amafu_api::{ApiClient, RouterTransport, Transport};
    use amafu_controllers::authorized_keys::AuthorizedKeysController;
    use amafu_domain::access::Role;
    use amafu_domain::kinds::ssh_key::{
        KeyOrigin, KeySource, SshKey, SshKeyGroup, SshKeyGroupSpecification, SshKeyReference,
        SshKeySpecification,
    };
    use amafu_domain::kinds::vm::{Vm, VmSpecification};
    use amafu_domain::{ObjectName, ProjectName};
    use amafu_interfaces::{AuditLog, Identifiers, Store};
    use amafu_reconciler::Controller;
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore, SeededIdentifiers};
    use time::OffsetDateTime;

    const TOKEN: &str = "amafu-member-token";
    const ALICE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOpKbGPinFIKvvVQexMuxfmVR3auvr57kkIe6mkURtIs alice@laptop";
    const PRIVATE: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";

    struct Directory;

    #[async_trait::async_trait]
    impl KeyDirectory for Directory {
        async fn listing(&self, source: KeySource, account: &str) -> Result<String, DirectoryError> {
            match (source, account) {
                (KeySource::Github, "alice") => Ok(format!("{ALICE}\nssh-dss AAAA ancient\n")),
                _ => Err(DirectoryError::UnknownAccount(source.keys_url(account))),
            }
        }
    }

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn api() -> (ApiClient, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::new());
        let member = Principal {
            name: String::from("alice"),
            role: Role::Member,
            project: ProjectName::new("web").unwrap(),
        };
        let state = ApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(vec![TokenIdentity::for_token(
                member, TOKEN, None,
            )])),
            audit: Arc::new(MemoryAuditLog::new()) as Arc<dyn AuditLog>,
            clock: Arc::new(VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH)),
            identifiers: Arc::new(SeededIdentifiers::new(5)) as Arc<dyn Identifiers>,
            issuers: Arc::new(amafu_api::service_accounts::HttpIssuerDirectory::default()),
            key_directory: Arc::new(Directory),
            catalog: Arc::new(amafu_interfaces::FixedImageCatalog::default()),
        };
        let transport: Arc<dyn Transport> = Arc::new(RouterTransport::new(router(state)));
        (
            ApiClient::new(transport, Some(String::from(TOKEN))).in_project(ProjectName::new("web").unwrap()),
            store,
        )
    }

    fn vm(references: Vec<SshKeyReference>) -> VmSpecification {
        VmSpecification { ssh_keys: references, ..VmSpecification::small() }
    }

    #[tokio::test]
    async fn imported_keys_reach_a_vm_through_a_group_and_stay_frozen() {
        let (client, store) = api();
        let lookup = client.look_up_ssh_keys(KeySource::Github, "alice").await.unwrap();
        assert_eq!(lookup.keys.len(), 1);
        assert_eq!(lookup.skipped.len(), 1);
        let missing = client.look_up_ssh_keys(KeySource::Launchpad, "nobody").await;
        assert_eq!(missing.unwrap_err().status(), Some(404));

        let imported = SshKeySpecification {
            public_key: lookup.keys[0].public_key.clone(),
            origin: KeySource::Github.origin("alice"),
        };
        client.create::<SshKey>(&name("alice-laptop"), &imported).await.unwrap();
        let pasted = SshKeySpecification { public_key: PRIVATE.to_owned(), origin: KeyOrigin::Pasted };
        assert_eq!(client.create::<SshKey>(&name("oops"), &pasted).await.unwrap_err().status(), Some(422));
        let ghosts = SshKeyGroupSpecification { keys: vec![name("alice-laptop"), name("ghost")] };
        assert!(client.create::<SshKeyGroup>(&name("team"), &ghosts).await.is_err());
        let team = SshKeyGroupSpecification { keys: vec![name("alice-laptop")] };
        let created = client.create::<SshKeyGroup>(&name("team"), &team).await.unwrap();
        let updated = client.update::<SshKeyGroup>("team", &ghosts, Some(created.metadata.generation)).await;
        assert!(updated.is_err());

        let unknown = vm(vec![SshKeyReference::Group { name: name("strangers") }]);
        assert!(client.create::<Vm>(&name("web-1"), &unknown).await.is_err());
        let granted = vm(vec![SshKeyReference::Group { name: name("team") }]);
        client.create::<Vm>(&name("web-1"), &granted).await.unwrap();
        assert_eq!(client.delete::<SshKey>("alice-laptop").await.unwrap_err().status(), Some(409));
        assert_eq!(client.delete::<SshKeyGroup>("team").await.unwrap_err().status(), Some(409));

        let controller = AuthorizedKeysController::new(Arc::clone(&store) as Arc<dyn Store>);
        for key in controller.discover_keys().await.unwrap() {
            let observation = controller.observe(&key).await.unwrap();
            for action in controller.plan(&observation) {
                controller.apply(&key, action).await.unwrap();
            }
        }
        let frozen = client.get::<Vm>("web-1").await.unwrap().status.authorized_keys.unwrap();
        assert_eq!(frozen.len(), 1);
        assert_eq!(frozen[0].public_key, ALICE);
        assert_eq!(frozen[0].fingerprint, lookup.keys[0].fingerprint);

        client.delete::<SshKeyGroup>("team").await.unwrap();
        client.delete::<SshKey>("alice-laptop").await.unwrap();
        let after = client.get::<Vm>("web-1").await.unwrap().status.authorized_keys.unwrap();
        assert_eq!(after, frozen);
    }
}
