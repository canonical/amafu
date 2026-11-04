use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use amafu_domain::Sha256Digest;
use amafu_domain::kinds::storage_backend::{PluginBackend, PluginModule};
use amafu_interfaces::ActionError;
use futures::StreamExt;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::bindings::amafu::storage::http::{self, HttpError};
use super::bindings::amafu::storage::types::{FencedNode, ProviderError, ProvisionedVolume, VolumeRequest};
use super::bindings::amafu::storage::{logging, secrets, types};
use super::bindings::{StoragePlugin, StoragePluginPre};
use super::modules::PluginModules;

const EPOCH_TICK: Duration = Duration::from_millis(10);
const WALL_CLOCK_GRACE: Duration = Duration::from_secs(1);
const MOST_INSTANCES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginLimits {
    pub fuel: u64,
    pub memory_bytes: usize,
    pub wall_clock: Duration,
    pub response_bytes: usize,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            fuel: 2_000_000_000,
            memory_bytes: 64 * 1024 * 1024,
            wall_clock: Duration::from_secs(30),
            response_bytes: 4 * 1024 * 1024,
        }
    }
}

pub struct PluginState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    backend: PluginBackend,
    http: reqwest::Client,
    response_bytes: usize,
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl types::Host for PluginState {}

impl secrets::Host for PluginState {
    async fn get(&mut self, name: String) -> Option<String> {
        self.backend.secrets.get(&name).cloned()
    }
}

impl logging::Host for PluginState {
    async fn log(&mut self, level: logging::Level, message: String) {
        let module = self.backend.module.digest.to_string();
        match level {
            logging::Level::Debug => tracing::debug!(%module, "{message}"),
            logging::Level::Info => tracing::info!(%module, "{message}"),
            logging::Level::Warning => tracing::warn!(%module, "{message}"),
            logging::Level::Error => tracing::error!(%module, "{message}"),
        }
    }
}

async fn forward(
    client: reqwest::Client,
    response_bytes: usize,
    request: http::Request,
) -> Result<http::Response, HttpError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| HttpError::Failed(error.to_string()))?;
    let mut outgoing = client.request(method, &request.url).body(request.body);
    for (name, value) in request.headers {
        outgoing = outgoing.header(name, value);
    }
    let response = outgoing.send().await.map_err(|error| HttpError::Failed(error.to_string()))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| HttpError::Failed(error.to_string()))?;
        if body.len() + chunk.len() > response_bytes {
            return Err(HttpError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(http::Response { status, headers, body })
}

impl http::Host for PluginState {
    async fn send(&mut self, request: http::Request) -> Result<http::Response, HttpError> {
        if !self.backend.allows(&request.url) {
            return Err(HttpError::Denied(format!(
                "{} is not an endpoint this backend declares",
                request.url
            )));
        }
        forward(self.http.clone(), self.response_bytes, request).await
    }
}

pub enum Call {
    Check,
    CreateVolume(VolumeRequest),
    DeleteVolume(ProvisionedVolume),
    ExportToNode(ProvisionedVolume, String),
    RevokeNode(FencedNode),
    HasRevokedNode(FencedNode),
    ReadmitNode(FencedNode),
}

pub enum Answer {
    Nothing,
    Volume(ProvisionedVolume),
    Target(String),
    Flag(bool),
}

pub struct PluginHost {
    engine: Engine,
    linker: Linker<PluginState>,
    prepared: Mutex<BTreeMap<Sha256Digest, StoragePluginPre<PluginState>>>,
    modules: Arc<dyn PluginModules>,
    http: reqwest::Client,
    limits: PluginLimits,
}

fn host_failure(error: impl std::fmt::Display) -> ActionError {
    ActionError::fatal(format!("the plugin host cannot start: {error}"))
}

fn stopped(error: &wasmtime::Error) -> ActionError {
    ActionError::retryable(format!("the plugin was stopped: {error:#}"))
}

fn tick_epochs(engine: &Engine) {
    let weak = engine.weak();
    std::thread::spawn(move || {
        while let Some(engine) = weak.upgrade() {
            engine.increment_epoch();
            drop(engine);
            std::thread::sleep(EPOCH_TICK);
        }
    });
}

fn provider_failure(error: ProviderError) -> ActionError {
    match error {
        ProviderError::Retryable(message) => ActionError::retryable(message),
        ProviderError::Invalid(message) => ActionError::invalid(message),
        ProviderError::NotFound => ActionError::invalid("the backend has no such volume"),
        ProviderError::Unsupported => ActionError::invalid("the backend does not support this"),
    }
}

impl PluginHost {
    pub fn new(modules: Arc<dyn PluginModules>, limits: PluginLimits) -> Result<Arc<Self>, ActionError> {
        let mut engine_settings = Config::new();
        engine_settings.consume_fuel(true).epoch_interruption(true);
        let engine = Engine::new(&engine_settings).map_err(host_failure)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(host_failure)?;
        StoragePlugin::add_to_linker::<_, HasSelf<PluginState>>(&mut linker, |state| state)
            .map_err(host_failure)?;
        let http = crate::management::management_client(limits.wall_clock)?;
        tick_epochs(&engine);
        Ok(Arc::new(Self { engine, linker, prepared: Mutex::new(BTreeMap::new()), modules, http, limits }))
    }

    async fn prepared(&self, module: &PluginModule) -> Result<StoragePluginPre<PluginState>, ActionError> {
        if let Some(prepared) =
            self.prepared.lock().unwrap_or_else(PoisonError::into_inner).get(&module.digest)
        {
            return Ok(prepared.clone());
        }
        let bytes = self.modules.module(module).await?;
        let engine = self.engine.clone();
        let component = tokio::task::spawn_blocking(move || Component::new(&engine, bytes))
            .await
            .map_err(host_failure)?
            .map_err(|error| {
                ActionError::invalid(format!("the plugin is not a storage component: {error:#}"))
            })?;
        let instance = self.linker.instantiate_pre(&component).map_err(|error| {
            ActionError::invalid(format!("the plugin needs what the host does not offer: {error:#}"))
        })?;
        let prepared = StoragePluginPre::new(instance).map_err(|error| {
            ActionError::invalid(format!("the plugin does not export a storage provider: {error:#}"))
        })?;
        self.prepared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(module.digest.clone(), prepared.clone());
        Ok(prepared)
    }

    fn store(&self, backend: &PluginBackend) -> Result<Store<PluginState>, ActionError> {
        let state = PluginState {
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(self.limits.memory_bytes)
                .instances(MOST_INSTANCES)
                .trap_on_grow_failure(true)
                .build(),
            backend: backend.clone(),
            http: self.http.clone(),
            response_bytes: self.limits.response_bytes,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(self.limits.fuel).map_err(host_failure)?;
        let ticks =
            u64::try_from(self.limits.wall_clock.as_millis() / EPOCH_TICK.as_millis()).unwrap_or(u64::MAX);
        store.set_epoch_deadline(ticks);
        store.epoch_deadline_trap();
        Ok(store)
    }

    pub async fn invoke(&self, backend: &PluginBackend, call: Call) -> Result<Answer, ActionError> {
        let prepared = self.prepared(&backend.module).await?;
        let mut store = self.store(backend)?;
        let configuration = backend.configuration.to_string();
        let running = async {
            let plugin = prepared.instantiate_async(&mut store).await.map_err(|error| stopped(&error))?;
            let provider = plugin.amafu_storage_provider();
            let outcome = match call {
                Call::Check => provider
                    .call_check(&mut store, &configuration)
                    .await
                    .map(|answer| answer.map(|()| Answer::Nothing)),
                Call::CreateVolume(request) => provider
                    .call_create_volume(&mut store, &configuration, &request)
                    .await
                    .map(|answer| answer.map(Answer::Volume)),
                Call::DeleteVolume(volume) => provider
                    .call_delete_volume(&mut store, &configuration, &volume)
                    .await
                    .map(|answer| answer.map(|()| Answer::Nothing)),
                Call::ExportToNode(volume, node) => provider
                    .call_export_to_node(&mut store, &configuration, &volume, &node)
                    .await
                    .map(|answer| answer.map(Answer::Target)),
                Call::RevokeNode(node) => provider
                    .call_revoke_node(&mut store, &configuration, &node)
                    .await
                    .map(|answer| answer.map(|()| Answer::Nothing)),
                Call::HasRevokedNode(node) => provider
                    .call_has_revoked_node(&mut store, &configuration, &node)
                    .await
                    .map(|answer| answer.map(Answer::Flag)),
                Call::ReadmitNode(node) => provider
                    .call_readmit_node(&mut store, &configuration, &node)
                    .await
                    .map(|answer| answer.map(|()| Answer::Nothing)),
            };
            outcome.map_err(|error| stopped(&error))?.map_err(provider_failure)
        };
        tokio::time::timeout(self.limits.wall_clock + WALL_CLOCK_GRACE, running)
            .await
            .map_err(|_| ActionError::retryable("the plugin was stopped after running out of time"))?
    }
}
