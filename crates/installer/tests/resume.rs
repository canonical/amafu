#[cfg(test)]
mod resuming {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use amafu_api::{ApiClient, ApiState, RouterTransport, TokenAuthenticator, router};
    use amafu_domain::features::FeatureLevel;
    use amafu_domain::kinds::cluster_version::{CLUSTER_VERSION_NAME, ClusterVersion};
    use amafu_domain::kinds::node::{BootReport, BootSlot, Node, NodePhase, NodeSpecification, NodeStatus};
    use amafu_domain::node_image::NodeImage;
    use amafu_domain::{Change, NodeName, ObjectId, ObjectName, ObjectReference};
    use amafu_installer::configuration::ClusterCredentials;
    use amafu_installer::journal::JournalStore;
    use amafu_installer::{InstallDescription, InstallDirectory, InstallProgress, InstallSession};
    use amafu_interfaces::objects::apply;
    use amafu_interfaces::{
        AuditEntry, AuditLog, Clock, ImageSigningKey, Installation, MachineName, MachineProvisioner,
        MachineState, ProvisioningError, RandomIdentifiers, Store, StoreError,
    };
    use futures::future::BoxFuture;
    use time::OffsetDateTime;

    struct StillClock;

    impl Clock for StillClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
        }

        fn sleep(&self, _duration: Duration) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }

    struct ForgetfulAudit;

    #[async_trait::async_trait]
    impl AuditLog for ForgetfulAudit {
        async fn record(&self, _entry: AuditEntry) -> Result<(), StoreError> {
            Ok(())
        }

        async fn find(&self, _request_id: uuid::Uuid) -> Result<Option<AuditEntry>, StoreError> {
            Ok(None)
        }

        async fn entries_from(
            &self,
            _first: u64,
            _limit: usize,
        ) -> Result<Vec<amafu_interfaces::SequencedAuditEntry>, StoreError> {
            Ok(Vec::new())
        }

        async fn recent(&self, _limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
            Ok(Vec::new())
        }
    }

    struct FakeMaas {
        cluster: Arc<dyn Store>,
        image: NodeImage,
        nodes: BTreeMap<String, NodeName>,
        states: Mutex<BTreeMap<String, MachineState>>,
        deployments: Mutex<BTreeMap<String, usize>>,
    }

    impl FakeMaas {
        async fn join(&self, node: &NodeName) {
            let reference = ObjectReference::of::<Node>(None, ObjectName::from(node));
            let created = apply::<Node>(
                self.cluster.as_ref(),
                &reference,
                Change::Create {
                    id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
                    name: reference.name.clone(),
                    project: None,
                    labels: BTreeMap::new(),
                    specification: NodeSpecification::default(),
                },
            )
            .await
            .unwrap();
            let status = NodeStatus {
                phase: NodePhase::Ready,
                boot: Some(BootReport {
                    booted_slot: BootSlot::First,
                    default_slot: BootSlot::First,
                    booted_image: Some(self.image.identity()),
                    inactive_image: None,
                    fallen_back_from: None,
                    refused: None,
                }),
                ..NodeStatus::default()
            };
            let observed_generation = created.metadata.generation;
            apply::<Node>(
                self.cluster.as_ref(),
                &reference,
                Change::UpdateStatus { observed_generation, status },
            )
            .await
            .unwrap();
        }
    }

    #[async_trait::async_trait]
    impl MachineProvisioner for FakeMaas {
        async fn state(&self, machine: &MachineName) -> Result<MachineState, ProvisioningError> {
            let current = self.states.lock().unwrap().get(&machine.0).cloned().unwrap_or(MachineState::Ready);
            if current == MachineState::Deploying {
                self.states.lock().unwrap().insert(machine.0.clone(), MachineState::Deployed);
                self.join(&self.nodes[&machine.0]).await;
            }
            Ok(current)
        }

        async fn deploy(
            &self,
            machine: &MachineName,
            installation: &Installation,
        ) -> Result<(), ProvisioningError> {
            assert!(installation.user_data.starts_with("#!/bin/bash"));
            *self.deployments.lock().unwrap().entry(machine.0.clone()).or_default() += 1;
            self.states.lock().unwrap().insert(machine.0.clone(), MachineState::Deploying);
            Ok(())
        }
    }

    fn description(image: &NodeImage, key: &str) -> InstallDescription {
        let image = serde_json::to_string(image).unwrap();
        InstallDescription::parse(&format!(
            r#"
    version: 1
    cluster:
      name: test
      image: {image}
      boot_loader:
        url: http://images.test/boot-loader.tar
        digest: sha256:{}
      image_keys: ["{key}"]
    maas:
      url: http://maas.test:5240/MAAS
      api_key: consumer:token:secret
    machines:
      - hostname: amafu-0
        node: node-0
        roles: [core, compute]
        underlay: {{ address: 10.0.1.10/24 }}
      - hostname: amafu-1
        node: node-1
        roles: [compute]
        underlay: {{ address: 10.0.1.11/24 }}
    "#,
            "b".repeat(64)
        ))
        .unwrap()
    }

    async fn install_crashing_after(
        advances_before_crash: usize,
    ) -> (BTreeMap<String, usize>, Option<NodeImage>) {
        let directory = tempfile::tempdir().unwrap();
        let (key, _) = ImageSigningKey::generate().unwrap();
        let image = key.sign(
            "0.1.0",
            FeatureLevel(1),
            "c".repeat(64).parse().unwrap(),
            1024,
            String::from("http://images.test/node-image.tar"),
        );
        let description = description(&image, &key.public_key_hexadecimal());
        let install = InstallDirectory::new(directory.path());
        let credentials = ClusterCredentials::load_or_create(&install.credentials(), "test").unwrap();
        let cluster_store: Arc<dyn Store> =
            Arc::new(JournalStore::open(&directory.path().join("cluster.json")).unwrap());
        let api = router(ApiState {
            store: Arc::clone(&cluster_store),
            authenticator: Arc::new(TokenAuthenticator::new(vec![credentials.admin_identity().unwrap()])),
            audit: Arc::new(ForgetfulAudit),
            clock: Arc::new(StillClock),
            identifiers: Arc::new(RandomIdentifiers),
        });
        let maas = Arc::new(FakeMaas {
            cluster: Arc::clone(&cluster_store),
            image,
            nodes: [("amafu-0", "node-0"), ("amafu-1", "node-1")]
                .into_iter()
                .map(|(machine, node)| (machine.to_owned(), NodeName::new(node).unwrap()))
                .collect(),
            states: Mutex::new(BTreeMap::new()),
            deployments: Mutex::new(BTreeMap::new()),
        });
        let open = || async {
            let cluster = ApiClient::new(
                Arc::new(RouterTransport::new(api.clone())),
                Some(credentials.admin_token.clone()),
            );
            InstallSession::open(
                description.clone(),
                &install,
                Arc::clone(&maas) as Arc<dyn MachineProvisioner>,
                Some(cluster),
                Arc::new(StillClock),
            )
            .await
            .unwrap()
        };
        let mut session = open().await;
        let mut advances = 0;
        for _ in 0..200 {
            match session.advance().await.unwrap() {
                InstallProgress::Succeeded => break,
                InstallProgress::Advanced => {
                    advances += 1;
                    if advances == advances_before_crash {
                        drop(session);
                        session = open().await;
                    }
                }
                InstallProgress::Waiting | InstallProgress::Retrying => {}
            }
        }
        let admin = ApiClient::new(
            Arc::new(RouterTransport::new(api.clone())),
            Some(credentials.admin_token.clone()),
        );
        let installed = admin
            .get::<ClusterVersion>(CLUSTER_VERSION_NAME)
            .await
            .ok()
            .map(|object| object.specification.image);
        let deployments = maas.deployments.lock().unwrap().clone();
        (deployments, installed)
    }

    #[tokio::test]
    async fn the_install_finishes_once_whichever_step_it_crashes_after() {
        for advances_before_crash in 1..=7 {
            let (deployments, installed) = install_crashing_after(advances_before_crash).await;
            assert_eq!(
                deployments,
                BTreeMap::from([(String::from("amafu-0"), 1), (String::from("amafu-1"), 1)]),
                "crashing after {advances_before_crash} steps"
            );
            assert!(
                installed.is_some(),
                "the installed image was not recorded after crashing at {advances_before_crash}"
            );
        }
    }
}
