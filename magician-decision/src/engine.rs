//! Settings → bound runtime.
//!
//! Idle-until-bound is enforced here, structurally: an operation is bound
//! only when its model entry exists, the adapter is known, the API key
//! resolves, and the pack loads. Any miss logs and skips that operation —
//! the caller keeps its incumbent path. If nothing binds, [`build_runtime`]
//! returns `None`, so every consumer's fast path is "not configured".
//!
//! This is the host wiring that plan Part IV (§29) moves out of the host:
//! model construction, API-key lookup, and the locality filter live with
//! the adapters they configure.

use std::sync::Arc;

use tracing::{info, warn};

use crate::adapters::systemone::{SystemOneConfig, SystemOneDecisionModel};
use crate::config::{DecisionConfig, DecisionModelConfig, DecisionOperationConfig, DecisionTier};
use crate::model::StructuredDecisionModel;
use crate::pack::PackStore;
use crate::registry::ModelRegistry;
use crate::runtime::{BoundModel, DecisionRuntime, DecisionRuntimeBuilder};

/// Hosted Jev's endpoint and key env, used when a `typesafe` entry omits them.
pub const TYPESAFE_DEFAULT_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const TYPESAFE_DEFAULT_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// Build the runtime from config. `None` when disabled or when nothing
/// bound — in both states the plane is off, and callers must not treat
/// them differently.
///
/// Locality is enforced here, at binding time: in local mode a body-seeing
/// operation (page text, message bodies) refuses every remote model unless
/// the operator explicitly set `allow_remote_when_local` on that op. A
/// model is remote unless its endpoint is loopback. The call sites never
/// re-check locality, so no consumer can forget it.
pub fn build_runtime(config: &DecisionConfig, is_local_mode: bool) -> Option<Arc<DecisionRuntime>> {
    build_runtime_with(config, is_local_mode, &PackStore::new(None), &|name| {
        std::env::var(name).ok()
    })
}

/// [`build_runtime`] with the pack store and env lookup injected, so tests
/// bind without mutating the process environment.
pub fn build_runtime_with(
    config: &DecisionConfig,
    is_local_mode: bool,
    pack_store: &PackStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<Arc<DecisionRuntime>> {
    build_runtime_in(
        config,
        is_local_mode,
        pack_store,
        env,
        &ModelRegistry::new(),
    )
}

/// [`build_runtime_with`] loading in-process models through `models`, so
/// both locality runtimes share them and a settings reload keeps the ones
/// it still routes to (see [`ModelRegistry`]).
pub fn build_runtime_in(
    config: &DecisionConfig,
    is_local_mode: bool,
    pack_store: &PackStore,
    env: &dyn Fn(&str) -> Option<String>,
    models: &ModelRegistry,
) -> Option<Arc<DecisionRuntime>> {
    if !config.enabled {
        return None;
    }
    let mut builder = DecisionRuntimeBuilder::new();
    for (name, operation) in &config.operations {
        let route = build_route(config, name, operation, is_local_mode, env, models);
        if route.is_empty() {
            continue;
        }
        match pack_store.load(&operation.pack, &operation.pack_version) {
            Ok(pack) => {
                let models: Vec<&str> = route.iter().map(|entry| entry.name.as_str()).collect();
                info!(
                    operation = %name,
                    pack = %operation.pack,
                    version = %operation.pack_version,
                    route = ?models,
                    "decision operation bound"
                );
                builder = builder.bind_route(name.clone(), pack, route);
            },
            Err(err) => {
                warn!(
                    operation = %name,
                    error = %err,
                    "decision operation left idle: pack unavailable"
                );
            },
        }
    }
    let runtime = builder.build();
    let bound = runtime.bound_operations();
    if bound.is_empty() {
        info!("decision plane enabled but no operation bound; staying off");
        return None;
    }
    info!("decision plane bound operations: {:?}", bound);
    Some(Arc::new(runtime))
}

/// The model entry names an operation may route across, in order: the
/// pinned `model` alone, else its tier (`small` escalates into `large`).
pub fn route_names(config: &DecisionConfig, operation: &DecisionOperationConfig) -> Vec<String> {
    if !operation.model.is_empty() {
        let mut names = vec![operation.model.clone()];
        for name in &operation.fallback_models {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        return names;
    }
    let tiers: Vec<&Vec<String>> = match operation.tier {
        Some(DecisionTier::Small) => vec![&config.tiers.small, &config.tiers.large],
        Some(DecisionTier::Large) => vec![&config.tiers.large],
        None => vec![],
    };
    let mut names: Vec<String> = Vec::new();
    for name in tiers
        .into_iter()
        .flatten()
        .chain(&operation.fallback_models)
    {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// Select the operation mapping for this request's processing locality.
pub fn route_names_for(
    config: &DecisionConfig,
    operation: &DecisionOperationConfig,
    local: bool,
) -> Vec<String> {
    if let Some(routes) = &operation.routing {
        return if local { &routes.local } else { &routes.cloud }.names();
    }
    route_names(config, operation)
}

/// Build an operation's route. Each model that cannot bind (missing entry,
/// unresolved key, remote in local mode for a body-seeing op) is dropped
/// with a warning; the rest of the route still serves.
fn build_route(
    config: &DecisionConfig,
    operation_name: &str,
    operation: &DecisionOperationConfig,
    is_local_mode: bool,
    env: &dyn Fn(&str) -> Option<String>,
    models: &ModelRegistry,
) -> Vec<BoundModel> {
    let names = route_names_for(config, operation, is_local_mode);
    if names.is_empty() {
        warn!(
            operation = %operation_name,
            "decision operation left idle: no model and no populated tier"
        );
        return Vec::new();
    }
    if operation.model.is_empty() && !operation.thresholds.is_empty() {
        warn!(
            operation = %operation_name,
            "decision operation `thresholds` ignored: a routed operation keys thresholds per model (thresholds_by_model)"
        );
    }
    let mut route = Vec::new();
    for model_name in names {
        let Some(model_config) = config.models.get(&model_name) else {
            warn!(
                operation = %operation_name,
                model = %model_name,
                "decision model skipped: no such decision.models entry"
            );
            continue;
        };
        if let Err(reason) = crate::host::memory_gate(
            &model_config.adapter,
            model_config.min_memory_gb,
            config.local_min_memory_gb,
            crate::host::host_memory_gb(),
        ) {
            warn!(
                operation = %operation_name,
                model = %model_name,
                "decision model skipped: {reason}"
            );
            continue;
        }
        let model = match build_model(model_config, env, models) {
            Ok(model) => model,
            Err(reason) => {
                warn!(
                    operation = %operation_name,
                    model = %model_name,
                    "decision model skipped: {reason}"
                );
                continue;
            },
        };
        if is_local_mode
            && (operation.sees_body || operation.routing.is_some())
            && !operation.allow_remote_when_local
            && model.capabilities().remote
        {
            warn!(
                operation = %operation_name,
                model = %model_name,
                "decision model skipped: body-seeing op refuses remote models in local mode"
            );
            continue;
        }
        let thresholds = operation
            .thresholds_by_model
            .get(&model_name)
            .cloned()
            .or_else(|| (model_name == operation.model).then(|| operation.thresholds.clone()));
        route.push(BoundModel {
            admission: models.admission(model_config),
            name: model_name,
            model,
            thresholds,
        });
    }
    route
}

/// Build one `decision.models` entry, or the reason it stays idle. HTTP
/// entries (`typesafe`, `systemone`) become a System One client; an
/// in-process entry (laya, Kev) is loaded through `models` — once per
/// settings, shared by every operation and runtime that routes to it.
pub fn build_model(
    model_config: &DecisionModelConfig,
    env: &dyn Fn(&str) -> Option<String>,
    models: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    match model_config.adapter.as_str() {
        "laya-onnx" => laya_model(model_config, models),
        "laya-mlx" => laya_mlx_model(model_config, models),
        "kev-onnx" => kev_model(model_config, models),
        "kev-mlx" => kev_mlx_model(model_config, models),
        _ => model_profile(model_config, env).map(|profile| {
            Arc::new(SystemOneDecisionModel::new(profile)) as Arc<dyn StructuredDecisionModel>
        }),
    }
}

fn apply_capability_overrides(
    capabilities: &mut crate::model::ModelCapabilities,
    overrides: &crate::config::DecisionCapabilitiesConfig,
) {
    if let Some(calibrated) = overrides.calibrated {
        capabilities.calibrated = calibrated;
    }
    if let Some(max_state_tokens) = overrides.max_state_tokens {
        capabilities.max_state_tokens = Some(max_state_tokens);
    }
    if let Some(max_choice_options) = overrides.max_choice_options {
        capabilities.max_choice_options = Some(max_choice_options);
    }
    if let Some(max_questions) = overrides.max_questions {
        capabilities.max_questions = Some(max_questions);
    }
    if let Some(supports_score) = overrides.supports_score {
        capabilities.supports_score = supports_score;
    }
}

#[cfg(feature = "onnx")]
fn laya_model(
    model_config: &DecisionModelConfig,
    models: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    use crate::adapters::laya_onnx::{LayaOnnxConfig, LayaOnnxModel};

    let dir = model_config
        .model_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
        .ok_or_else(|| "laya-onnx needs a model_dir".to_string())?;
    let mut config = LayaOnnxConfig::new(dir, &model_config.model);
    config.threads = model_config.threads.unwrap_or(config.threads);
    apply_capability_overrides(&mut config.capabilities, &model_config.capabilities);
    let key = format!("laya-onnx {config:?}");
    let label = format!("laya-onnx {}", model_config.model);
    models.get_or_load(key, &label, || {
        let loaded = LayaOnnxModel::load(config).map_err(|error| error.to_string())?;
        info!(model = %model_config.model, dir, "laya model loaded");
        Ok(Arc::new(loaded) as Arc<dyn StructuredDecisionModel>)
    })
}

#[cfg(not(feature = "onnx"))]
fn laya_model(
    _: &DecisionModelConfig,
    _: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    Err("laya-onnx runs in the decision-engine process; this build has no ONNX support".to_string())
}

#[cfg(feature = "onnx")]
fn kev_model(
    model_config: &DecisionModelConfig,
    models: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    use crate::adapters::kev_onnx::{KevOnnxConfig, KevOnnxModel};

    let dir = model_config
        .model_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
        .ok_or_else(|| "kev-onnx needs a model_dir".to_string())?;
    let mut config = KevOnnxConfig::new(dir, &model_config.model);
    config.threads = model_config.threads.unwrap_or(config.threads);
    apply_capability_overrides(&mut config.capabilities, &model_config.capabilities);
    let key = format!("kev-onnx {config:?}");
    let label = format!("kev-onnx {}", model_config.model);
    models.get_or_load(key, &label, || {
        let loaded = KevOnnxModel::load(config).map_err(|error| error.to_string())?;
        info!(model = %model_config.model, dir, temperature = loaded.temperature(), "kev model loaded");
        Ok(Arc::new(loaded) as Arc<dyn StructuredDecisionModel>)
    })
}

#[cfg(not(feature = "onnx"))]
fn kev_model(
    _: &DecisionModelConfig,
    _: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    Err("kev-onnx runs in the decision-engine process; this build has no ONNX support".to_string())
}

/// laya on the GPU through MLX, loaded once per settings and shared across
/// both locality runtimes (one worker thread owns the network).
#[cfg(feature = "mlx")]
fn laya_mlx_model(
    model_config: &DecisionModelConfig,
    models: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    use crate::adapters::laya_mlx::{LayaMlxConfig, LayaMlxModel};

    let dir = model_config
        .model_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
        .ok_or_else(|| "laya-mlx needs a model_dir".to_string())?;
    let mut config = LayaMlxConfig::new(dir, &model_config.model);
    apply_capability_overrides(&mut config.capabilities, &model_config.capabilities);
    let key = format!("laya-mlx {config:?}");
    let label = format!("laya-mlx {}", model_config.model);
    models.get_or_load(key, &label, || {
        let loaded = LayaMlxModel::load(config).map_err(|error| error.to_string())?;
        info!(model = %model_config.model, dir, "laya-mlx model loaded");
        Ok(Arc::new(loaded) as Arc<dyn StructuredDecisionModel>)
    })
}

#[cfg(not(feature = "mlx"))]
fn laya_mlx_model(
    _: &DecisionModelConfig,
    _: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    Err("laya-mlx needs a decision-engine built with the `mlx` feature (Apple Silicon)".to_string())
}

/// Kev on the GPU through MLX, loaded once per settings and shared across
/// both locality runtimes (one worker thread owns the model).
#[cfg(feature = "mlx")]
fn kev_mlx_model(
    model_config: &DecisionModelConfig,
    models: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    use crate::adapters::kev_mlx::{KevMlxConfig, KevMlxModel};

    let dir = model_config
        .model_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
        .ok_or_else(|| "kev-mlx needs a model_dir".to_string())?;
    let mut config = KevMlxConfig::new(dir, &model_config.model);
    config.quantize = model_config.quantize.unwrap_or(config.quantize);
    if model_config.state_chunk.is_some() {
        config.state_chunk = model_config.state_chunk;
    }
    apply_capability_overrides(&mut config.capabilities, &model_config.capabilities);
    let key = format!("kev-mlx {config:?}");
    let label = format!("kev-mlx {}", model_config.model);
    models.get_or_load(key, &label, || {
        let loaded = KevMlxModel::load(config).map_err(|error| error.to_string())?;
        info!(model = %model_config.model, dir, "kev-mlx model loaded");
        Ok(Arc::new(loaded) as Arc<dyn StructuredDecisionModel>)
    })
}

#[cfg(not(feature = "mlx"))]
fn kev_mlx_model(
    _: &DecisionModelConfig,
    _: &ModelRegistry,
) -> Result<Arc<dyn StructuredDecisionModel>, String> {
    Err("kev-mlx needs a decision-engine built with the `mlx` feature (Apple Silicon)".to_string())
}

/// Resolve one HTTP `decision.models` entry into an adapter profile, or the
/// reason it stays idle.
pub fn model_profile(
    model_config: &DecisionModelConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<SystemOneConfig, String> {
    let resolve_key = |key_env: &str| -> Result<String, String> {
        env(key_env)
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| format!("api key env {key_env} is unset"))
    };
    let mut profile = match model_config.adapter.as_str() {
        "typesafe" => {
            let endpoint = if model_config.endpoint.trim().is_empty() {
                TYPESAFE_DEFAULT_ENDPOINT
            } else {
                model_config.endpoint.as_str()
            };
            let key_env = model_config
                .api_key_env
                .as_deref()
                .unwrap_or(TYPESAFE_DEFAULT_KEY_ENV);
            SystemOneConfig::new(endpoint, resolve_key(key_env)?, &model_config.model)
        },
        "systemone" => {
            if model_config.endpoint.trim().is_empty() {
                return Err("systemone adapter needs an endpoint".to_string());
            }
            let mut profile = SystemOneConfig::keyless(&model_config.endpoint, &model_config.model);
            if let Some(key_env) = &model_config.api_key_env {
                profile.api_key = Some(resolve_key(key_env)?);
            }
            profile
        },
        other => return Err(format!("unknown adapter '{other}'")),
    };
    profile.timeout = std::time::Duration::from_millis(model_config.timeout_ms);
    profile.max_retries = model_config.max_retries;
    apply_capability_overrides(&mut profile.capabilities, &model_config.capabilities);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<DecisionConfig, String> {
        serde_yaml::from_str(yaml).map_err(|err| err.to_string())
    }

    #[test]
    fn parses_full_block_and_defaults_idle() {
        let config = parse(
            "enabled: true\nmodels:\n  jev:\n    adapter: typesafe\n    model: jev-latest\n    endpoint: https://api.typesafe.ai/v1/systemone\n    api_key_env: TYPESAFE_API_KEY\noperations:\n  tool_action_judge:\n    model: jev\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: true\n",
        )
        .expect("parses");
        assert!(config.enabled);
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.operations.len(), 1);
        let op = &config.operations["tool_action_judge"];
        assert!(op.sees_body);
        // Rollout discipline ships off.
        assert!(!op.shadow.enabled);
        assert!(!op.gate.enabled);
        assert_eq!(op.gate.max_consecutive_steps, 3);
    }

    #[test]
    fn empty_block_is_disabled() {
        let config = parse("").expect("empty parses to default");
        assert!(!config.enabled);
        assert!(build_runtime(&config, false).is_none());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = parse("enabled: true\nbogus: 1\n").expect_err("must reject");
        assert!(err.contains("unknown field"), "error was: {err}");
        let err = parse("enabled: true\nmodels:\n  jev:\n    adapter: typesafe\n    bogus: 1\n")
            .expect_err("must reject");
        assert!(err.contains("unknown field"), "error was: {err}");
    }

    #[test]
    fn enabled_without_key_or_pack_binds_nothing() {
        let config = parse(
            "enabled: true\nmodels:\n  jev:\n    adapter: typesafe\n    model: jev-latest\n    endpoint: https://api.typesafe.ai/v1/systemone\n    api_key_env: TYPESAFE_API_KEY_TESTS_ABSENT\noperations:\n  tool_action_judge:\n    model: jev\n    pack: definitely_missing_pack\n    pack_version: '1.0.0'\n",
        )
        .expect("parses");
        // No env key and no pack: the operation stays idle and the plane
        // reports off rather than half-on.
        assert!(build_runtime(&config, false).is_none());
    }

    #[test]
    fn local_mode_refuses_body_seeing_remote_ops() {
        let config = parse(
            "enabled: true\nmodels:\n  jev:\n    adapter: typesafe\n    model: jev-latest\n    endpoint: https://api.typesafe.ai/v1/systemone\n    api_key_env: TYPESAFE_API_KEY_TESTS_ABSENT\noperations:\n  tool_action_judge:\n    model: jev\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: true\n",
        )
        .expect("parses");
        // Local mode + body-seeing + remote adapter: refused before the
        // key/pack lookups even run, exactly as the locality matrix demands.
        assert!(build_runtime(&config, true).is_none());
        // Explicit operator opt-in re-enables binding (then the absent key
        // and missing repo pack keep it idle in this test environment).
        let mut opted = config.clone();
        opted
            .operations
            .get_mut("tool_action_judge")
            .expect("op")
            .allow_remote_when_local = true;
        assert!(build_runtime(&opted, true).is_none());
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn local_judge(endpoint: &str, sees_body: bool) -> DecisionConfig {
        parse(&format!(
            "enabled: true\nmodels:\n  laya:\n    adapter: systemone\n    model: laya-typed-decisions\n    endpoint: {endpoint}\n    capabilities:\n      max_state_tokens: 512\n      max_choice_options: 77\n      supports_score: false\noperations:\n  tool_action_judge:\n    model: laya\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: {sees_body}\n"
        ))
        .expect("parses")
    }

    #[test]
    fn a_loopback_model_binds_a_body_seeing_op_in_local_mode_without_a_key() {
        let config = local_judge("http://127.0.0.1:8090/v1/systemone", true);
        let runtime = build_runtime_with(&config, true, &PackStore::new(None), &no_env)
            .expect("a local model serves a body-seeing op in local mode");
        let bound = runtime.bound_operation("tool_action_judge").expect("bound");
        let capabilities = bound.model.capabilities();
        assert!(!capabilities.remote);
        assert!(
            !capabilities.calibrated,
            "nothing claimed that was not declared"
        );
        assert_eq!(capabilities.max_state_tokens, Some(512));
        assert_eq!(capabilities.max_choice_options, Some(77));
        assert!(!capabilities.supports_score);
        assert_eq!(bound.model.identity().adapter, "systemone");
    }

    #[test]
    fn a_non_loopback_systemone_model_is_remote_and_refused_in_local_mode() {
        let config = local_judge("http://decisions.example.com/v1/systemone", true);
        assert!(build_runtime_with(&config, true, &PackStore::new(None), &no_env).is_none());
        // Cloud mode: the same entry binds.
        assert!(build_runtime_with(&config, false, &PackStore::new(None), &no_env).is_some());
    }

    #[test]
    fn a_named_key_env_must_resolve_even_for_systemone() {
        let mut config = local_judge("http://localhost:8090/v1/systemone", false);
        config.models.get_mut("laya").expect("model").api_key_env =
            Some("LAYA_KEY_TESTS_ABSENT".to_string());
        assert!(build_runtime_with(&config, false, &PackStore::new(None), &no_env).is_none());
        let with_key = |name: &str| (name == "LAYA_KEY_TESTS_ABSENT").then(|| "k".to_string());
        assert!(build_runtime_with(&config, false, &PackStore::new(None), &with_key).is_some());
    }

    #[test]
    fn a_systemone_entry_without_an_endpoint_stays_idle() {
        let model = DecisionModelConfig {
            adapter: "systemone".to_string(),
            ..DecisionModelConfig::default()
        };
        assert!(model_profile(&model, &no_env).is_err());
    }

    #[test]
    fn a_bare_typesafe_entry_resolves_to_the_hosted_jev_profile() {
        let model = DecisionModelConfig::default();
        assert!(
            model_profile(&model, &no_env).is_err(),
            "the key is required"
        );
        let key = |name: &str| (name == TYPESAFE_DEFAULT_KEY_ENV).then(|| "k".to_string());
        let profile = model_profile(&model, &key).expect("resolves");
        assert_eq!(profile.endpoint, TYPESAFE_DEFAULT_ENDPOINT);
        assert_eq!(profile.adapter, "typesafe");
        assert_eq!(profile.api_key.as_deref(), Some("k"));
        assert!(profile.capabilities.calibrated);
        assert!(profile.capabilities.remote);
        assert_eq!(profile.capabilities.max_choice_options, Some(255));
    }

    #[test]
    fn a_body_seeing_typesafe_op_is_refused_in_local_mode_even_with_a_key() {
        let config = parse(
            "enabled: true\nmodels:\n  jev:\n    adapter: typesafe\n    model: jev-1.13.0\noperations:\n  tool_action_judge:\n    model: jev\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: true\n",
        )
        .expect("parses");
        let key = |name: &str| (name == TYPESAFE_DEFAULT_KEY_ENV).then(|| "k".to_string());
        assert!(build_runtime_with(&config, true, &PackStore::new(None), &key).is_none());
        assert!(build_runtime_with(&config, false, &PackStore::new(None), &key).is_some());
        let mut opted = config.clone();
        opted
            .operations
            .get_mut("tool_action_judge")
            .expect("op")
            .allow_remote_when_local = true;
        assert!(build_runtime_with(&opted, true, &PackStore::new(None), &key).is_some());
    }
}
