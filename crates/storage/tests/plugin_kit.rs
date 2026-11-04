#[cfg(test)]
mod plugin_kit {
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::SocketAddrV4;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use amafu_domain::NodeName;
    use amafu_domain::kinds::storage_backend::{PluginBackend, PluginModule};
    use amafu_interfaces::contract::storage::provider_contract;
    use amafu_interfaces::{ActionError, StorageProvider, VolumeRequest};
    use amafu_storage::local_disk::LocalDiskProvider;
    use amafu_storage::nvme_tcp::SpdkTarget;
    use amafu_storage::plugins::{PluginHost, PluginLimits, PluginModules, WebAssemblyProvider};
    use axum::Json;
    use axum::extract::State;
    use serde_json::{Value, json};

    const POOL: &str = "amafu";
    const TARGET_ADDRESS: &str = "192.0.2.5:4420";

    #[derive(Default)]
    struct Subsystem {
        namespaces: Vec<String>,
        hosts: BTreeSet<String>,
    }

    #[derive(Default)]
    struct FakeSpdk {
        volumes: BTreeSet<String>,
        subsystems: BTreeMap<String, Subsystem>,
    }

    fn refused(code: i64, message: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": message}})
    }

    fn answered(result: &Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "result": result})
    }

    impl FakeSpdk {
        fn answer(&mut self, method: &str, parameters: &Value) -> Value {
            let text = |key: &str| parameters[key].as_str().unwrap_or_default().to_owned();
            match method {
                "bdev_lvol_get_lvstores" => answered(&json!([{"name": POOL}])),
                "bdev_lvol_create" => {
                    let name = format!("{}/{}", text("lvs_name"), text("lvol_name"));
                    if self.volumes.insert(name) { answered(&json!("uuid")) } else { refused(-17, "File exists") }
                }
                "bdev_lvol_delete" => {
                    if self.volumes.remove(&text("name")) { answered(&json!(true)) } else { refused(-19, "No such device") }
                }
                "nvmf_create_subsystem" => {
                    let nqn = text("nqn");
                    if self.subsystems.contains_key(&nqn) {
                        return refused(-32602, "Subsystem already exists");
                    }
                    self.subsystems.insert(nqn, Subsystem::default());
                    answered(&json!(true))
                }
                "nvmf_delete_subsystem" => match self.subsystems.remove(&text("nqn")) {
                    Some(_) => answered(&json!(true)),
                    None => refused(-32602, "Subsystem not found"),
                },
                "nvmf_get_subsystems" => answered(&Value::Array(
                    self.subsystems
                        .iter()
                        .map(|(nqn, subsystem)| {
                            json!({
                                "nqn": nqn,
                                "namespaces": subsystem.namespaces.iter().map(|bdev| json!({"bdev_name": bdev})).collect::<Vec<_>>(),
                                "hosts": subsystem.hosts.iter().map(|host| json!({"nqn": host})).collect::<Vec<_>>(),
                            })
                        })
                        .collect(),
                )),
                "nvmf_subsystem_add_ns" => match self.subsystems.get_mut(&text("nqn")) {
                    Some(subsystem) => {
                        subsystem.namespaces.push(parameters["namespace"]["bdev_name"].as_str().unwrap_or_default().to_owned());
                        answered(&json!(1))
                    }
                    None => refused(-32602, "Subsystem not found"),
                },
                "nvmf_subsystem_add_listener" => answered(&json!(true)),
                "nvmf_subsystem_add_host" => match self.subsystems.get_mut(&text("nqn")) {
                    Some(subsystem) => {
                        subsystem.hosts.insert(text("host"));
                        answered(&json!(true))
                    }
                    None => refused(-32602, "Subsystem not found"),
                },
                "nvmf_subsystem_remove_host" => match self.subsystems.get_mut(&text("nqn")) {
                    Some(subsystem) => {
                        subsystem.hosts.remove(&text("host"));
                        answered(&json!(true))
                    }
                    None => refused(-32602, "Subsystem not found"),
                },
                _ => refused(-32601, "Method not found"),
            }
        }
    }

    struct Served {
        url: String,
        requests: Arc<AtomicUsize>,
    }

    async fn serve_fake_spdk() -> Served {
        let state = Arc::new(Mutex::new(FakeSpdk::default()));
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let application = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    |State(state): State<Arc<Mutex<FakeSpdk>>>, Json(body): Json<Value>| async move {
                        let method = body["method"].as_str().unwrap_or_default().to_owned();
                        Json(state.lock().unwrap().answer(&method, &body["params"]))
                    },
                )
                .get(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    async { "reached" }
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, application).await.unwrap() });
        Served { url, requests }
    }

    fn plugins_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
    }

    fn built_plugins() -> &'static PathBuf {
        static BUILT: OnceLock<PathBuf> = OnceLock::new();
        BUILT.get_or_init(|| {
            let directory = plugins_directory();
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "--release", "--quiet"])
                .current_dir(&directory)
                .status()
                .unwrap();
            assert!(status.success(), "the plugins did not build");
            directory.join("target/wasm32-wasip2/release")
        })
    }

    struct BuiltModules;

    #[async_trait::async_trait]
    impl PluginModules for BuiltModules {
        async fn module(&self, module: &PluginModule) -> Result<Vec<u8>, ActionError> {
            let name = module.url.rsplit('/').next().unwrap_or_default();
            std::fs::read(built_plugins().join(name)).map_err(ActionError::invalid)
        }
    }

    fn module(file: &str) -> PluginModule {
        PluginModule {
            url: format!("https://plugins.example/{file}"),
            digest: "0".repeat(64).parse().unwrap(),
        }
    }

    fn module_with_digest(file: &str, digest: char) -> PluginModule {
        PluginModule { digest: digest.to_string().repeat(64).parse().unwrap(), ..module(file) }
    }

    fn test_limits() -> PluginLimits {
        PluginLimits {
            fuel: 200_000_000,
            memory_bytes: 32 * 1024 * 1024,
            wall_clock: Duration::from_secs(5),
            response_bytes: 1024 * 1024,
        }
    }

    fn host() -> Arc<PluginHost> {
        PluginHost::new(Arc::new(BuiltModules), test_limits()).unwrap()
    }

    fn spdk_backend(url: &str) -> PluginBackend {
        PluginBackend {
            module: module_with_digest("amafu_plugin_spdk_rpc.wasm", 'a'),
            configuration: json!({"management": url, "addresses": [TARGET_ADDRESS], "pool": POOL}),
            endpoints: vec![url.to_owned()],
            secrets: BTreeMap::from([(String::from("user"), String::from("admin"))]),
        }
    }

    fn hostile_backend(behaviour: &str, extra: &Value) -> PluginBackend {
        let mut configuration = json!({"behaviour": behaviour});
        if let (Some(fields), Some(more)) = (configuration.as_object_mut(), extra.as_object()) {
            fields.extend(more.clone());
        }
        PluginBackend {
            module: module_with_digest("amafu_plugin_hostile.wasm", 'b'),
            configuration,
            endpoints: Vec::new(),
            secrets: BTreeMap::from([(
                String::from("password"),
                String::from("only-for-the-hostile-backend"),
            )]),
        }
    }

    fn node() -> NodeName {
        NodeName::new("node-1").unwrap()
    }

    fn request() -> VolumeRequest {
        VolumeRequest {
            volume_id: amafu_domain::ObjectId::from_uuid(uuid::Uuid::from_u128(9)),
            size_bytes: 1024 * 1024,
            source_image: None,
        }
    }

    #[tokio::test]
    async fn the_built_in_nvme_tcp_provider_passes_the_plugin_kit_contract() {
        let served = serve_fake_spdk().await;
        let address: SocketAddrV4 = TARGET_ADDRESS.parse().unwrap();
        let provider = SpdkTarget::new(&served.url, None, vec![address], POOL).unwrap();
        assert_eq!(provider_contract(&provider, &node()).await, Ok(()));
    }

    #[tokio::test]
    async fn the_built_in_local_disk_provider_passes_the_plugin_kit_contract() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalDiskProvider::new(node(), directory.path().to_path_buf());
        assert_eq!(provider_contract(&provider, &node()).await, Ok(()));
    }

    #[tokio::test]
    async fn the_spdk_plugin_passes_the_same_contract_as_the_built_in_provider() {
        let served = serve_fake_spdk().await;
        let provider = WebAssemblyProvider::new(host(), spdk_backend(&served.url));
        assert_eq!(provider_contract(&provider, &node()).await, Ok(()));
    }

    async fn stopped_within_limits(provider: &WebAssemblyProvider) -> ActionError {
        let started = Instant::now();
        let error = provider.create_volume(&request()).await.unwrap_err();
        assert!(
            started.elapsed() < test_limits().wall_clock + Duration::from_secs(2),
            "{error} took {:?}",
            started.elapsed()
        );
        error
    }

    #[tokio::test]
    async fn hostile_plugins_are_stopped_while_other_backends_keep_working() {
        let served = serve_fake_spdk().await;
        let honeypot = serve_fake_spdk().await;
        let host = host();
        let behaviours = [
            ("loop-forever", json!({}), true, "fuel"),
            ("allocate-without-bound", json!({}), true, "stopped"),
            (
                "call-undeclared-endpoint",
                json!({"undeclared_endpoint": honeypot.url}),
                false,
                "not an endpoint this backend declares",
            ),
            ("read-foreign-secret", json!({"foreign_secret": "user"}), false, "withheld"),
            ("return-oversized-result", json!({}), false, "length"),
            ("return-malformed-result", json!({}), false, "characters"),
            ("panic", json!({}), true, "stopped"),
        ];
        let well_behaved = WebAssemblyProvider::new(Arc::clone(&host), spdk_backend(&served.url));
        let hostile = behaviours.iter().map(|(behaviour, extra, is_retryable, reason)| {
            let provider = WebAssemblyProvider::new(Arc::clone(&host), hostile_backend(behaviour, extra));
            async move {
                let error = stopped_within_limits(&provider).await;
                assert_eq!(error.is_retryable(), *is_retryable, "{behaviour}: {error}");
                assert!(
                    error.to_string().contains(reason),
                    "{behaviour} was stopped for another reason: {error}"
                );
            }
        });
        let node = node();
        let (contract, _) =
            tokio::join!(provider_contract(&well_behaved, &node), futures::future::join_all(hostile));
        assert_eq!(contract, Ok(()));
        assert_eq!(
            honeypot.requests.load(Ordering::SeqCst),
            0,
            "a plugin reached an endpoint it did not declare"
        );
    }

    #[tokio::test]
    async fn a_malformed_attach_target_is_refused() {
        let provider =
            WebAssemblyProvider::new(host(), hostile_backend("return-malformed-result", &json!({})));
        let volume = amafu_domain::kinds::volume::ProvisionedVolume {
            identifier: String::from("volume-1"),
            size_bytes: 1,
            node: None,
        };
        assert!(!provider.export_to_node(&volume, &node()).await.unwrap_err().is_retryable());
    }
}
