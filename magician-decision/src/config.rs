//! The `decision:` settings block — models, operation bindings, and
//! rollout knobs.
//!
//! Owned here rather than by the host so the decision stack's settings move
//! with the decision stack (plan Part IV, §29). Hosts deserialize this type
//! from their own config and hand it to [`crate::engine::build_runtime`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Structured-decision plane configuration (`decision:` block).
///
/// A different model class from `llm.router`: typed Choice/Score/Noul
/// judgments asked as versioned question packs, never chat profiles and
/// never in `llm-router.yaml` `operation_mapping` — mixing the classes is
/// how a later model swap becomes a routing bug. Disabled, unbound,
/// key-missing, or pack-missing all mean OFF; an operation never inherits
/// an LLM router default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionConfig {
    /// Master switch. `false` (the default) keeps the whole plane off
    /// regardless of what is declared below.
    pub enabled: bool,
    /// Model entries by name; operations reference these by `model`.
    pub models: BTreeMap<String, DecisionModelConfig>,
    /// Operation bindings by name, e.g. `tool_action_judge`.
    pub operations: BTreeMap<String, DecisionOperationConfig>,
    /// Model entry names per tier, in preference order. Operations that
    /// name a `tier` instead of a `model` route across these by fit.
    pub tiers: DecisionTiersConfig,
    /// Memory a machine needs, in GB, before any model that runs inside the
    /// engine (`laya-onnx`, `laya-mlx`, `kev-onnx`, `kev-mlx`) binds; below
    /// it they stay idle and routes fall through (see [`crate::host`]).
    /// Default 16, like local generation's `min_memory_gb`. A model entry's
    /// `min_memory_gb` replaces it for that model.
    pub local_min_memory_gb: u32,
    /// Where local model folders live: absolute, `~/…`, or relative to the
    /// runtime root. Default `models/decision` (where `make
    /// setup-decision-models` installs). A model entry's relative
    /// `model_dir` is a folder under it; an absolute one is used as is.
    pub models_dir: Option<String>,
    /// ONNX Runtime for local ONNX models: the library file or its folder;
    /// absolute, `~/…`, or relative to the runtime root. Default
    /// `lib/onnxruntime`. `ORT_DYLIB_PATH` in the environment wins.
    pub onnxruntime_path: Option<String>,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            models: BTreeMap::new(),
            operations: BTreeMap::new(),
            tiers: DecisionTiersConfig::default(),
            local_min_memory_gb: 16,
            models_dir: None,
            onnxruntime_path: None,
        }
    }
}

/// The two tiers. `small` is for short, few-option judgments a compact
/// local model answers in milliseconds (laya); `large` for long states and
/// wide option sets (Kev-4B locally, Jev in the cloud).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionTiersConfig {
    pub small: Vec<String>,
    pub large: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionTier {
    /// Try `tiers.small`, then escalate to `tiers.large` on misfit or
    /// failure.
    Small,
    /// `tiers.large` only.
    Large,
}

pub use decision_engine_contract::classification::BatchStrategy;

/// Bump when shared item rendering or synthetic head mapping changes.
pub const SHARED_CHUNK_TRANSFORM_VERSION: u32 = 2;
pub const SHARED_CHUNK_MAX_QUESTIONS: usize = 128;

/// One decision model (adapter + endpoint + credentials pointer).
///
/// Both adapters speak `POST /v1/systemone`:
///
/// - `typesafe` — hosted Jev. `endpoint` defaults to TypeSafe's URL and
///   `api_key_env` to `TYPESAFE_API_KEY`; the key is required.
/// - `systemone` — a self-hosted Jev-style model (laya, Kev). `endpoint` is
///   required; `api_key_env` is optional, but when named it must resolve.
///   Nothing is claimed about calibration or limits unless `capabilities`
///   declares it. Locality is derived from the endpoint host (loopback =
///   local), never declared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionModelConfig {
    /// Shared across all operations and localities.
    pub max_in_flight: usize,
    /// Waiting work is bounded independently of active model calls.
    pub queue_capacity: usize,
    pub queue_max_bytes: usize,
    /// Adapter family: `typesafe`, `systemone`, or a local model run inside
    /// the decision-engine process — `laya-onnx`, `laya-mlx`, `kev-onnx`, or
    /// `kev-mlx`.
    pub adapter: String,
    /// Model id sent on the wire. Pin (e.g. `jev-1.13.0`) once thresholds
    /// are tuned; `jev-latest` while exploring.
    pub model: String,
    /// Full endpoint URL. Empty = the adapter's default (`typesafe` only).
    pub endpoint: String,
    /// Env var holding the API key. Keys never live in YAML.
    pub api_key_env: Option<String>,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Extra attempts beyond the first, for 429/529/5xx/timeout only.
    pub max_retries: u32,
    /// Overrides for what the model declares it can do; unset fields keep
    /// the adapter's profile.
    pub capabilities: DecisionCapabilitiesConfig,
    /// `laya-onnx` / `laya-mlx` / `kev-onnx` / `kev-mlx`: the model folder (as
    /// `make setup-decision-models` lays it out). A relative path is a folder
    /// under `models_dir`; an absolute or `~/…` one is used as is (resolved
    /// by the decision-engine binary).
    pub model_dir: Option<String>,
    /// `laya-onnx` / `kev-onnx`: ONNX Runtime intra-op threads (default:
    /// the machine's parallelism, capped at 8).
    pub threads: Option<usize>,
    /// `kev-mlx`: 8-bit weights (MLX affine, group size 32; default true).
    /// `false` runs the bf16 reference weights — Kev-4B 6.65 GiB instead
    /// of 4.41.
    pub quantize: Option<bool>,
    /// `kev-mlx`: tokens per pass when a new state runs through the model
    /// (default 512), bounding activation memory on long states.
    pub state_chunk: Option<usize>,
    /// In-process models: memory this entry needs, in GB, replacing
    /// `local_min_memory_gb` (e.g. more for Kev-4B).
    pub min_memory_gb: Option<u32>,
}

impl DecisionModelConfig {
    /// Same locality classification used by adapters; unknown adapters fail closed.
    pub fn is_remote(&self) -> bool {
        match self.adapter.as_str() {
            "laya-onnx" | "laya-mlx" | "kev-onnx" | "kev-mlx" => false,
            "systemone" => !crate::adapters::systemone::is_loopback_endpoint(&self.endpoint),
            _ => true,
        }
    }

    /// Scheduling changes resize the same queue and never reload model weights.
    pub(crate) fn identity_config(&self) -> Self {
        let mut identity = self.clone();
        identity.max_in_flight = 6;
        identity.queue_capacity = crate::dispatch::DEFAULT_CAPACITY;
        identity.queue_max_bytes = crate::dispatch::DEFAULT_BYTES;
        identity
    }
}

impl Default for DecisionModelConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 6,
            queue_capacity: crate::dispatch::DEFAULT_CAPACITY,
            queue_max_bytes: crate::dispatch::DEFAULT_BYTES,
            adapter: "typesafe".to_string(),
            model: "jev-latest".to_string(),
            endpoint: String::new(),
            api_key_env: None,
            timeout_ms: 15_000,
            max_retries: 2,
            capabilities: DecisionCapabilitiesConfig::default(),
            model_dir: None,
            threads: None,
            quantize: None,
            state_chunk: None,
            min_memory_gb: None,
        }
    }
}

/// Per-model capability overrides (see `ModelCapabilities`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionCapabilitiesConfig {
    pub calibrated: Option<bool>,
    pub max_state_tokens: Option<u64>,
    pub max_choice_options: Option<usize>,
    pub max_questions: Option<usize>,
    pub supports_score: Option<bool>,
}

/// One operation binding: which pack, which model, and the rollout knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionOperationConfig {
    /// Explicit per-locality primary and backup. Takes precedence over legacy model/tier.
    pub routing: Option<decision_engine_contract::settings::LocalityRoutes>,
    /// Explicit operator rollout before empirical qualification; never implies reviewed evidence.
    pub allow_unqualified_gate: bool,
    pub classification: decision_engine_contract::classification::ClassificationLimits,
    pub qualifications: Vec<decision_engine_contract::classification::ClassificationQualification>,
    /// Pack question ID to outputs that may mutate protected memory state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restricted_outputs: BTreeMap<String, Vec<String>>,
    /// `shared_chunk` is enabled per operation only after paired evaluation.
    pub batch_strategy: BatchStrategy,
    /// Comparison sampling has no application authority.
    pub observation: decision_engine_contract::classification::ClassificationObservationPolicy,
    /// Key into `decision.models`: pins the operation to that one model.
    /// Wins over `tier` when both are set.
    pub model: String,
    /// Route across `decision.tiers` instead of pinning one model.
    pub tier: Option<DecisionTier>,
    /// Ordered alternatives after the primary model/tier. Locality policy applies. Each
    /// requires its own thresholds_by_model entry before its answers may gate.
    pub fallback_models: Vec<String>,
    pub pack: String,
    pub pack_version: String,
    /// Whether the operation's state includes user content (page text,
    /// message bodies). Body-seeing ops follow the strict locality rule.
    pub sees_body: bool,
    /// Explicit operator opt-in to run a remote model for a body-seeing op
    /// in local mode. Default false; the matrix refuses otherwise.
    pub allow_remote_when_local: bool,
    pub shadow: DecisionShadowConfig,
    pub gate: DecisionGateConfig,
    /// Total structured selection/review deadline for the tool-action rail.
    /// Expiry returns to the selected generative planner; zero means immediate.
    pub action_timeout_ms: u64,
    /// Thresholds keyed by name, owned by the pinned `model`. Copying a
    /// tuned threshold to another model is a review item, not a default.
    pub thresholds: BTreeMap<String, f64>,
    /// Thresholds per `decision.models` entry, for routed operations. A
    /// model with no set here (and not the pinned `model`) may answer and
    /// be logged, but its answers never gate.
    pub thresholds_by_model: BTreeMap<String, BTreeMap<String, f64>>,
}

impl Default for DecisionOperationConfig {
    fn default() -> Self {
        Self {
            routing: None,
            allow_unqualified_gate: false,
            classification: Default::default(),
            qualifications: Vec::new(),
            restricted_outputs: BTreeMap::new(),
            batch_strategy: BatchStrategy::PerItem,
            observation: Default::default(),
            model: String::new(),
            tier: None,
            fallback_models: Vec::new(),
            pack: String::new(),
            pack_version: "1.0.0".to_string(),
            sees_body: false,
            allow_remote_when_local: false,
            shadow: DecisionShadowConfig::default(),
            gate: DecisionGateConfig::default(),
            action_timeout_ms: 8_000,
            thresholds: BTreeMap::new(),
            thresholds_by_model: BTreeMap::new(),
        }
    }
}

/// Shadow dual-run: the decision model runs alongside the incumbent path
/// and only content-free comparisons are logged. Zero behavior change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionShadowConfig {
    pub enabled: bool,
    /// Which incumbent the comparison is against, for the log line only.
    pub compare_against: Option<String>,
}

/// Gate mode: the decision model may answer steps directly instead of the
/// LLM. Ships off; shadow-before-flip is the discipline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DecisionGateConfig {
    pub enabled: bool,
    /// Safety cap: after this many consecutive model-decided steps, force
    /// one incumbent (LLM) step to re-ground before more gating.
    pub max_consecutive_steps: u32,
}

impl Default for DecisionGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_consecutive_steps: 3,
        }
    }
}

impl DecisionConfig {
    /// Refuse invalid settings before either initial binding or atomic reload.
    pub fn validate(&self) -> Result<(), String> {
        for (name, model) in &self.models {
            if model.queue_capacity == 0
                || model.queue_capacity > 1024
                || model.queue_max_bytes == 0
                || model.queue_max_bytes > 64 * 1024 * 1024
            {
                return Err(format!("model {name}: invalid dispatch queue bounds"));
            }
            if model.max_in_flight == 0 || model.max_in_flight > 64 {
                return Err(format!("model {name}: max_in_flight must be in 1..=64"));
            }
            if model.capabilities.max_questions == Some(0) {
                return Err(format!("model {name}: max_questions must be positive"));
            }
        }
        for (name, operation) in &self.operations {
            if let Some(routes) = &operation.routing {
                for route in [&routes.local, &routes.cloud] {
                    if route.backup.as_ref() == Some(&route.primary) {
                        return Err(format!("operation {name}: primary and backup must differ"));
                    }
                    for model in route.names() {
                        if !self.models.contains_key(&model) {
                            return Err(format!("operation {name}: unknown routing model {model}"));
                        }
                    }
                }
                if !operation.allow_remote_when_local
                    && routes
                        .local
                        .names()
                        .iter()
                        .any(|m| self.models[m].is_remote())
                {
                    return Err(format!("operation {name}: remote model in local route requires allow_remote_when_local"));
                }
            }
            for fallback in &operation.fallback_models {
                if !self.models.contains_key(fallback) {
                    return Err(format!(
                        "operation {name}: unknown fallback model {fallback}"
                    ));
                }
            }
            operation
                .classification
                .validate()
                .map_err(|e| format!("operation {name}: {e}"))?;
            operation
                .observation
                .validate()
                .map_err(|e| format!("operation {name}: {e}"))?;
            if !operation.restricted_outputs.is_empty() {
                let pack = crate::PackStore::new(None)
                    .load(&operation.pack, &operation.pack_version)
                    .map_err(|e| {
                        format!("operation {name}: cannot validate restricted outputs: {e}")
                    })?;
                for (question_id, outputs) in &operation.restricted_outputs {
                    let question = pack
                        .questions
                        .iter()
                        .find(|q| q.id().as_str() == question_id)
                        .ok_or_else(|| {
                            format!("operation {name}: unknown restricted question {question_id}")
                        })?;
                    if outputs.is_empty() || outputs.iter().any(|v| v.trim().is_empty()) {
                        return Err(format!(
                            "operation {name}: empty restricted output for {question_id}"
                        ));
                    }
                    let mut seen = std::collections::BTreeSet::new();
                    for output in outputs {
                        if !seen.insert(output) {
                            return Err(format!("operation {name}: duplicate restricted output {question_id}:{output}"));
                        }
                        let valid = match question {
                            crate::primitives::Question::Choice(q) => {
                                q.criteria.keys().any(|k| k.as_str() == output)
                            },
                            crate::primitives::Question::Noul(_) => {
                                matches!(output.as_str(), "true" | "false")
                            },
                            crate::primitives::Question::Score(_) => output == "score",
                        };
                        if !valid {
                            return Err(format!("operation {name}: invalid restricted output {question_id}:{output}"));
                        }
                    }
                }
            }
            for qualification in &operation.qualifications {
                qualification
                    .validate()
                    .map_err(|e| format!("operation {name}: {e}"))?;
            }
            for thresholds in
                std::iter::once(&operation.thresholds).chain(operation.thresholds_by_model.values())
            {
                if thresholds
                    .values()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                {
                    return Err(format!(
                        "operation {name}: thresholds must be finite probabilities"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Stable content identity; no credentials are serialized into settings.
pub fn fingerprint(value: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("config serializes"))
    )
}
impl DecisionConfig {
    pub fn behavior_fingerprint(&self, operation: &DecisionOperationConfig) -> String {
        let mut behavior = operation.clone();
        behavior.allow_unqualified_gate = false;
        behavior.shadow = Default::default();
        behavior.gate = Default::default();
        behavior.qualifications.clear();
        behavior.observation = Default::default();
        behavior.classification = Default::default();
        // Model identity, endpoint, capabilities and actual route remain part of qualification.
        // Admission capacity does not change a model's answer semantics. A
        // queue resize must not revoke reviewed labels or reload its weights.
        let models: BTreeMap<_, _> = self
            .models
            .iter()
            .map(|(name, model)| (name, model.identity_config()))
            .collect();
        fingerprint(&(
            behavior,
            models,
            &self.tiers,
            (operation.batch_strategy == BatchStrategy::SharedChunk).then_some((
                SHARED_CHUNK_TRANSFORM_VERSION,
                SHARED_CHUNK_MAX_QUESTIONS,
                operation.classification.chunk_size,
            )),
        ))
    }
}
