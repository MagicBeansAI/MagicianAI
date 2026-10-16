//! The decision engine process (plan Part IV, E4).
//!
//! Owns the settings, the model routes, thresholds, and the step judges,
//! and serves them to hosts over a Unix socket in the shape
//! `decision_engine_contract` defines. Two runtimes are bound from the
//! settings — one for local mode, one for cloud — because locality filters
//! each operation's route at bind time; a request names which it runs
//! under.
//!
//! The settings are live: [`watch_settings`] re-reads `decision-engine.yaml`
//! when it changes and [`Engine::reload`] binds the new routes beside the
//! current ones — loading any newly routed model while the current routes
//! keep serving — then swaps them in. Models still routed stay loaded;
//! models no longer routed are unloaded once in-flight requests finish.
//! Settings that do not parse leave the current ones serving.

mod batch;
mod qualification;
mod settings;
mod shared_chunk;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use actix_web::{web, HttpResponse};
use decision_engine_contract::wire::{
    DecideRequest, DecideResponse, DecideStatus, HealthResponse, Locality, OperationPolicy,
    OperationsResponse, CONTRACT_VERSION, DECIDE_PATH, HEALTH_PATH, OPERATIONS_PATH,
};
use magician_decision::config::DecisionConfig;
use magician_decision::primitives::{OptionId, QuestionId};
use magician_decision::registry::ModelRegistry;
use magician_decision::request::set_choice_candidates;
use magician_decision::{DecisionError, DecisionRuntime};

use decision_engine_contract::action::{
    ActionRequest, ActionResponse, ACTION_OPERATION, ACTION_PATH,
};

pub struct Engine {
    settings: Option<settings::SettingsStore>,
    state: RwLock<Arc<EngineState>>,
}

/// One set of settings, bound: what a request is served from, start to
/// finish, even when a reload swaps in the next set meanwhile.
struct EngineState {
    instance: String,
    revision: String,
    item_slots: BTreeMap<String, Arc<magician_decision::admission::Limiter>>,
    config: DecisionConfig,
    local: Option<Arc<DecisionRuntime>>,
    cloud: Option<Arc<DecisionRuntime>>,
    models: ModelRegistry,
}

impl EngineState {
    fn bind(config: DecisionConfig, models: ModelRegistry) -> Self {
        let env = |name: &str| std::env::var(name).ok();
        let packs = magician_decision::PackStore::new(None);
        let bind = |is_local_mode| {
            magician_decision::engine::build_runtime_in(
                &config,
                is_local_mode,
                &packs,
                &env,
                &models,
            )
        };
        let (local, cloud) = (bind(true), bind(false));
        Self {
            instance: magician_decision::telemetry::group_id(),
            revision: magician_decision::config::fingerprint(&config),
            item_slots: batch::item_slots(&config),
            config,
            local,
            cloud,
            models,
        }
    }
}

/// Read the engine's settings: `decision-engine.yaml`, the decision
/// schema at top level. The host keeps none of it — models, tiers,
/// operations, thresholds, and rollout knobs live only here.
pub fn load_config(path: &Path) -> Result<DecisionConfig, String> {
    let text =
        std::fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    parse_config(&text, path)
}

/// [`load_config`] on text already read from `path` (named in errors).
pub fn parse_config(text: &str, path: &Path) -> Result<DecisionConfig, String> {
    let config: DecisionConfig = serde_yaml::from_str(text)
        .map_err(|err| format!("decision settings in {}: {err}", path.display()))?;
    config.validate()?;
    Ok(config)
}

/// A configured path: `~/…` under `home`, absolute as is, anything else
/// under `base`.
fn resolve_path(path: &str, base: &Path, home: Option<&Path>) -> PathBuf {
    let path = path.trim();
    if let (Some(rest), Some(home)) = (path.strip_prefix("~/"), home) {
        return home.join(rest);
    }
    if path == "~" {
        if let Some(home) = home {
            return home.to_path_buf();
        }
    }
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The folder local model folders live in: `models_dir` (absolute, `~/…`,
/// or under the runtime root), default `<root>/models/decision`.
pub fn models_dir(config: &DecisionConfig, root: &Path, home: Option<&Path>) -> PathBuf {
    resolve_path(
        config.models_dir.as_deref().unwrap_or("models/decision"),
        root,
        home,
    )
}

/// Make every local model entry's `model_dir` absolute: relative ones are
/// folders under [`models_dir`].
pub fn resolve_model_dirs(config: &mut DecisionConfig, root: &Path, home: Option<&Path>) {
    let base = models_dir(config, root, home);
    for model in config.models.values_mut() {
        if let Some(dir) = model.model_dir.as_mut() {
            *dir = resolve_path(dir, &base, home).display().to_string();
        }
    }
}

/// The ONNX Runtime library `onnxruntime_path` names (a file, or a folder
/// holding the platform's library), default `<root>/lib/onnxruntime`.
pub fn onnxruntime_library(config: &DecisionConfig, root: &Path, home: Option<&Path>) -> PathBuf {
    let path = resolve_path(
        config
            .onnxruntime_path
            .as_deref()
            .unwrap_or("lib/onnxruntime"),
        root,
        home,
    );
    if path.is_dir() || path.extension().is_none() {
        let name = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        path.join(name)
    } else {
        path
    }
}

fn error_status(error: &DecisionError) -> DecideStatus {
    match error {
        DecisionError::OperationUnbound(_) => DecideStatus::Unbound,
        DecisionError::NoFittingModel { .. } => DecideStatus::NoFittingModel,
        _ => DecideStatus::Failed,
    }
}

impl Engine {
    /// Bind both runtimes from settings (idle-until-bound: an operation
    /// that cannot bind is simply absent).
    pub fn from_config(config: DecisionConfig) -> Self {
        config
            .validate()
            .expect("invalid decision engine configuration");
        Self::serving(EngineState::bind(config, ModelRegistry::new()))
    }

    /// Injection point for tests (scripted models).
    pub fn with_runtimes(
        config: DecisionConfig,
        local: Option<Arc<DecisionRuntime>>,
        cloud: Option<Arc<DecisionRuntime>>,
    ) -> Self {
        config
            .validate()
            .expect("invalid decision engine configuration");
        Self::serving(EngineState {
            instance: magician_decision::telemetry::group_id(),
            revision: magician_decision::config::fingerprint(&config),
            item_slots: batch::item_slots(&config),
            config,
            local,
            cloud,
            models: ModelRegistry::new(),
        })
    }

    /// Enables owner settings endpoints for the same file watched by this engine.
    pub fn with_settings_path(mut self, path: PathBuf, root: PathBuf) -> Self {
        self.settings = Some(settings::SettingsStore::new(path, root));
        self
    }

    fn serving(state: EngineState) -> Self {
        Self {
            settings: None,
            state: RwLock::new(Arc::new(state)),
        }
    }

    fn current(&self) -> Arc<EngineState> {
        match self.state.read() {
            Ok(state) => Arc::clone(&state),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Serve `config` from now on. Blocking: newly routed models load here
    /// while the current settings keep serving; models both settings route
    /// to carry over without a reload, and the current settings' other
    /// models are unloaded once requests still using them finish.
    pub fn reload(&self, config: DecisionConfig) {
        if let Err(error) = config.validate() {
            tracing::warn!(%error, "decision engine rejected invalid reload");
            return;
        }
        let previous = self.current();
        let mut next = EngineState::bind(config, ModelRegistry::reusing(&previous.models));
        next.instance = previous.instance.clone();
        for (name, slots) in &previous.item_slots {
            if let Some(operation) = next.config.operations.get(name) {
                slots.set_limit(operation.classification.item_concurrency);
                next.item_slots.insert(name.clone(), slots.clone());
            }
        }
        let unloaded = next.models.release_unused();
        let bound = |runtime: &Option<Arc<DecisionRuntime>>| -> Vec<String> {
            runtime
                .as_ref()
                .map(|runtime| {
                    runtime
                        .bound_operations()
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        tracing::info!(
            local = ?bound(&next.local),
            cloud = ?bound(&next.cloud),
            loaded = ?next.models.loaded(),
            ?unloaded,
            "decision engine settings reloaded"
        );
        let next = Arc::new(next);
        match self.state.write() {
            Ok(mut state) => *state = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Labels of the in-process models the current settings hold loaded.
    pub fn loaded_models(&self) -> Vec<String> {
        self.current().models.loaded()
    }

    pub fn health(&self) -> HealthResponse {
        HealthResponse {
            contract_version: CONTRACT_VERSION,
            status: "ok".to_string(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn operations(&self) -> OperationsResponse {
        self.current().operations()
    }

    pub async fn decide(&self, request: DecideRequest) -> DecideResponse {
        let current = self.current();
        let identity = (
            request.operation.clone(),
            request.batch.projection_version.clone(),
            request.batch.reference_version.clone(),
        );
        let (mut response, calls) = magician_decision::telemetry::capture(async {
            if request.batch.items.is_empty() {
                current.decide(request).await
            } else {
                current.decide_batch(request).await
            }
        })
        .await;
        response.model_calls = calls;
        for item in &mut response.batch.items {
            if item.status == decision_engine_contract::batch::ItemStatus::Cancelled
                && !response
                    .model_calls
                    .iter()
                    .any(|call| call.item_ids.contains(&item.item_id))
            {
                item.status = decision_engine_contract::batch::ItemStatus::NotStarted;
                item.error = Some("execution budget exhausted before provider admission".into());
            }
        }

        current.qualify(&identity.0, &identity.1, &identity.2, &mut response);
        response
    }

    pub async fn action(&self, request: ActionRequest) -> ActionResponse {
        let current = self.current();
        let locality = request.locality;
        let (mut response, calls) =
            magician_decision::telemetry::capture(magician_decision::action::driver::decide(
                request,
                current.config.operations.get(ACTION_OPERATION),
                current.runtime(locality).map(Arc::as_ref),
                current.config.enabled,
            ))
            .await;
        response.model_calls = calls;
        response
    }
}

impl EngineState {
    fn runtime(&self, locality: Locality) -> Option<&Arc<DecisionRuntime>> {
        match locality {
            Locality::Local => self.local.as_ref(),
            Locality::Cloud => self.cloud.as_ref(),
        }
    }

    fn operations(&self) -> OperationsResponse {
        let route = |runtime: Option<&Arc<DecisionRuntime>>, name: &str| -> Vec<String> {
            runtime
                .and_then(|runtime| runtime.bound_operation(name))
                .map(|bound| bound.route.iter().map(|entry| entry.name.clone()).collect())
                .unwrap_or_default()
        };
        let operations = self
            .config
            .operations
            .iter()
            .filter(|_| self.config.enabled)
            .map(|(name, operation)| OperationPolicy {
                classification: decision_engine_contract::classification::ClassificationPolicy {
                    allow_unqualified_gate: operation.allow_unqualified_gate,
                    limits: operation.classification.clone(),
                    qualifications: operation.qualifications.clone(),
                    restricted_outputs: operation.restricted_outputs.clone(),
                    observation: operation.observation.clone(),
                    observation_revision: magician_decision::config::fingerprint(
                        &operation.observation,
                    ),
                    batch_strategy: operation.batch_strategy,
                    shared_chunk_transform_version: (operation.batch_strategy
                        == magician_decision::config::BatchStrategy::SharedChunk)
                        .then_some(magician_decision::config::SHARED_CHUNK_TRANSFORM_VERSION),
                    behavior_fingerprint: self.config.behavior_fingerprint(operation),
                    pack: operation.pack.clone(),
                    pack_version: operation.pack_version.clone(),
                },
                name: name.clone(),
                shadow: operation.shadow.enabled,
                gate: operation.gate.enabled,
                max_consecutive_steps: operation.gate.max_consecutive_steps,
                sees_body: operation.sees_body,
                route_local: route(self.local.as_ref(), name),
                route_cloud: route(self.cloud.as_ref(), name),
            })
            .collect();
        OperationsResponse {
            engine_instance: self.instance.clone(),
            policy_revision: self.revision.clone(),
            contract_version: CONTRACT_VERSION,
            action_contract_version: self.config.enabled.then_some(CONTRACT_VERSION),
            operations,
        }
    }

    async fn decide(&self, request: DecideRequest) -> DecideResponse {
        self.decide_before(request, None).await
    }

    async fn decide_before(
        &self,
        request: DecideRequest,
        deadline: Option<tokio::time::Instant>,
    ) -> DecideResponse {
        let started = Instant::now();
        let reply = |status, response, thresholds, error: Option<String>| DecideResponse {
            batch: Default::default(),
            model_calls: Vec::new(),
            contract_version: CONTRACT_VERSION,
            status,
            response,
            thresholds,
            error,
            latency_ms: started.elapsed().as_millis() as u64,
        };
        if request.contract_version != CONTRACT_VERSION {
            return reply(
                DecideStatus::Unbound,
                None,
                None,
                Some(format!(
                    "host contract {} != engine {CONTRACT_VERSION}",
                    request.contract_version
                )),
            );
        }
        let Some(runtime) = self.runtime(request.locality) else {
            return reply(DecideStatus::Unbound, None, None, None);
        };
        let mut built = match runtime.build_request(&request.operation, request.state) {
            Ok(built) => built,
            Err(error) => return reply(error_status(&error), None, None, Some(error.to_string())),
        };
        for (question, options) in &request.choice_candidates {
            let options: Vec<(OptionId, String)> = options
                .iter()
                .map(|(id, label)| (OptionId::new(id), label.clone()))
                .collect();
            if let Err(error) =
                set_choice_candidates(&mut built, &QuestionId::new(question), &options)
            {
                return reply(DecideStatus::Failed, None, None, Some(error.to_string()));
            }
        }
        match runtime.evaluate_request_before(built, deadline).await {
            Ok(response) => {
                let thresholds = runtime.thresholds_for(&request.operation, &response.model);
                reply(DecideStatus::Answered, Some(response), thresholds, None)
            },
            Err(error) => reply(error_status(&error), None, None, Some(error.to_string())),
        }
    }
}

async fn health(engine: web::Data<Engine>) -> HttpResponse {
    HttpResponse::Ok().json(engine.health())
}

async fn operations(engine: web::Data<Engine>) -> HttpResponse {
    HttpResponse::Ok().json(engine.operations())
}

async fn decide(engine: web::Data<Engine>, body: web::Json<DecideRequest>) -> HttpResponse {
    HttpResponse::Ok().json(engine.decide(body.into_inner()).await)
}

async fn action(engine: web::Data<Engine>, body: web::Json<ActionRequest>) -> HttpResponse {
    HttpResponse::Ok().json(engine.action(body.into_inner()).await)
}

/// The engine's routes, for the server and for in-process tests.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .app_data(web::JsonConfig::default().limit(8 * 1024 * 1024))
        .route(
            decision_engine_contract::settings::SETTINGS_PATH,
            web::get().to(settings::get),
        )
        .route(
            decision_engine_contract::settings::SETTINGS_PATH,
            web::put().to(settings::put),
        )
        .route(HEALTH_PATH, web::get().to(health))
        .route(OPERATIONS_PATH, web::get().to(operations))
        .route(DECIDE_PATH, web::post().to(decide))
        .route(ACTION_PATH, web::post().to(action));
}

/// How often [`watch_settings`] looks at the settings file.
pub const SETTINGS_POLL: Duration = Duration::from_secs(2);

/// Reload `engine` whenever the settings file at `path` changes, checking
/// every `every`; `current` is the text the engine was started from (None:
/// unreadable). `prepare` resolves paths as at start. A file that is
/// missing or does not parse is logged and the current settings keep
/// serving; the next change is tried again. Runs on its own thread, since
/// a reload loads models.
pub fn watch_settings(
    engine: Arc<Engine>,
    path: PathBuf,
    current: Option<String>,
    every: Duration,
    prepare: impl Fn(&mut DecisionConfig) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("decision-settings".to_string())
        .spawn(move || {
            let mut seen = current;
            loop {
                std::thread::sleep(every);
                let text = std::fs::read_to_string(&path).ok();
                if text == seen {
                    continue;
                }
                seen = text.clone();
                let Some(text) = text else {
                    tracing::warn!(path = %path.display(), "decision settings unreadable; keeping the current ones");
                    continue;
                };
                match parse_config(&text, &path) {
                    Ok(mut config) => {
                        prepare(&mut config);
                        tracing::info!(path = %path.display(), "decision settings changed; reloading");
                        engine.reload(config);
                    },
                    Err(error) => {
                        tracing::error!(%error, "decision settings rejected; keeping the current ones");
                    },
                }
            }
        })
}

/// Serve on a Unix socket until the process is stopped. A stale socket
/// file from a previous run is removed first; the socket is created
/// owner-only (the engine sees page and message text in local mode).
pub async fn serve(engine: Arc<Engine>, socket: &Path) -> std::io::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let data = web::Data::from(engine);
    let server = actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(data.clone())
            .configure(routes)
    })
    .workers(2)
    .bind_uds(socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    server.run().await
}
