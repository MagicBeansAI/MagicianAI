// Configuration types for Magician service

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use magicllm::capability::{LLMModality, LLMProviderKind};
use magicllm::config::{LLMProfile, LLMRouterConfig, OperationProfileSelector, ResolvedProfile};
use runtime_core::{FileSandboxConfig, ShellSandboxConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::runtime_plan::{
    reject_mixed_legacy_dispatch, resolve_boot_runtime_plan, resolve_runtime_plan,
    EffectiveRuntimePlan, LeftoverDispatchScalars, RuntimePlanError, RuntimeScaleOverrides,
    RuntimeScaleSettings, ScaleProfile,
};

const LEGACY_TASKPLAN_OPERATIONS: &[&str] = &[
    "taskplan_generate",
    "taskplan_update_ledger",
    "taskplan_update_structure",
];
const LEGACY_TASKPLAN_PROFILE_PREFIX: &str = "llm-taskplan-";

/// Frontend delivery mode for Magician UI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MagicianFrontendMode {
    /// Serve pre-built UI from a directory (one port for API + UI)
    Filesystem,
    /// API only — no frontend serving (UI runs separately or not at all)
    #[serde(alias = "disabled", alias = "embedded")]
    #[default]
    ApiOnly,
}

/// Frontend settings for Magician
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MagicianFrontendSettings {
    /// Frontend delivery mode
    #[serde(default)]
    pub mode: MagicianFrontendMode,
    /// Frontend directory path (when mode is Filesystem)
    pub directory: Option<String>,
}

/// Optional router-based LLM configuration (new schema).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MagicianLlmSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<LLMRouterConfig>,
    /// Dispatch queue configuration. The `DispatchConfig` itself nests
    /// `local_prep` so the YAML reads as `llm.dispatch.local_prep`.
    #[serde(default)]
    pub dispatch: magicllm::dispatch::DispatchConfig,
}

/// Operator-facing processing-privacy policy: the single switch that moves
/// local-eligible LLM operations between the on-device model and their
/// configured remote counterparts (`when_cloud` selector arms). One global
/// control, not per-domain — all local generation profiles share one model,
/// so the residency goal (the local model goes unused in cloud mode) is only
/// expressible with one switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrivacySettings {
    #[serde(default)]
    pub processing: PrivacyProcessingSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrivacyProcessingSettings {
    /// `local`: on-device qwen serves every local-eligible operation —
    /// exactly today's behavior; an install that has never seen this field
    /// resolves identically. `cloud`: the `when_cloud` arm of each mapping
    /// serves it instead; the local generation model goes unused.
    #[serde(default)]
    pub mode: magicllm::ProcessingLocality,
}

/// App-platform runtime policy. Absence is deliberately restrictive: ordinary
/// remote-capable content may use an externally classified profile, but no
/// profile is eligible for `local_only` content until the operator declares
/// its exact physical profile here.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AppPlatformSettings {
    #[serde(default)]
    pub processing: AppProcessingTrustSettings,
    /// Process-owned app execution scheduler and durable resource-policy
    /// limits. Packages/grants may narrow these ceilings but never select or
    /// widen the server policy.
    #[serde(default)]
    pub resources: AppResourceRuntimeSettings,
    /// Manifest-declared LLM operation admission (plan 1.4). Keys are the
    /// exact names an app manifest may declare under `app.llm_operations`
    /// behind the `llm_operations_v1` feature; a name absent here is
    /// rejected by every execution admission — fail-closed, no blanket LLM
    /// access. Declarations remain inert review material. Each
    /// admitted name must also resolve through
    /// `llm.router.operation_mapping` under the `app:` namespace, and every
    /// selector arm must name a profile declared in
    /// `app_platform.processing.profiles`, so an app operation can only
    /// ride physically reviewed app-processing profiles. Absent or empty
    /// means no app may declare operations, which is exactly today's
    /// behavior.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub llm_operations: BTreeMap<String, AppProcessingLlmOperationTrust>,
    /// Scripted custom interactive surfaces (plan 1.6,
    /// `custom_surfaces_v1`). Absent or `enabled: false` keeps every
    /// host on the pre-1.6 no-script baseline — the process-wide kill
    /// switch behind the per-package manifest feature, the owner review
    /// grant, and the per-client host contract.
    #[serde(default)]
    pub custom_surfaces_v1: AppCustomSurfacesV1Settings,
    /// Boot-bound master switch and process ceilings for host-executed schedule
    /// and event behaviors. Disabled is the legacy/default posture for
    /// unattended execution. The process worker still drains durable one-way
    /// owner notifications accepted by foreground app workflows.
    #[serde(default)]
    pub background_behaviors: AppBackgroundBehaviorSettings,
    /// The deployment's own `distribution: system` packages — the ones under
    /// the read-only seed root, which ship with the binary.
    #[serde(default)]
    pub system_packages: AppSystemPackageSettings,
}

/// Boot handling for the deployment's own system packages.
///
/// `admit_at_boot` defaults ON: publishing an inert `ready_for_review`
/// installation grants nothing, and without it the deployment's own apps are
/// invisible.
///
/// `enable_at_boot` defaults OFF in the schema so a deployment that omits the
/// policy remains fail-closed. The shipped development/package configuration
/// opts in. When enabled, the boot report mints a non-transport,
/// installation-bound host grantor only for bytes in the current digest-pinned
/// system inventory. Ordinary background workers still cannot approve an
/// installation, and the owner can still revoke, disable or quarantine one.
///
/// Note also that enabling an installation does not start unattended
/// execution: `background_behaviors.enabled` is an independent master switch.
/// Those switches being separate is the design.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSystemPackageSettings {
    /// Resolve the seed root and publish its packages as inert
    /// `ready_for_review` installations. Off means the deployment's own apps
    /// are invisible.
    #[serde(default = "default_true")]
    pub admit_at_boot: bool,
    /// Approve boot-admitted packages with the authority their manifest
    /// requests, making their surfaces and routes reachable. Off leaves them
    /// admitted and awaiting an explicit owner approval.
    #[serde(default)]
    pub enable_at_boot: bool,
}

impl Default for AppSystemPackageSettings {
    fn default() -> Self {
        Self {
            admit_at_boot: true,
            enable_at_boot: false,
        }
    }
}

/// Operator switch for the `custom_surfaces_v1` capability. Disabled is
/// the fail-closed default: no scripted surface host may open, and
/// executable `surfaces/` members stay refused, until the operator flips
/// this deliberately.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfacesV1Settings {
    #[serde(default)]
    pub enabled: bool,
}

/// The boot-bound master switch and process ceilings for host-executed
/// schedule and event behaviors.
///
/// `enabled` is the single arming switch, and arming is *all* it does: grant
/// revocation, per-scope pause, per-operation admission and
/// `app_platform.resources` are untouched by it, and each on its own is still
/// sufficient to stop a behavior. Rollback is therefore this one field back to
/// `false` plus a restart — nothing else has to be unwound.
///
/// Off is the default because arming lets a package's reviewed recipe run with
/// no person present. Whether a given deployment wants that is an owner
/// decision, not a code default. What the code owes the owner instead is that
/// arming means what it says: `enforce_app_background_behavior_invariant`
/// refuses an armed configuration whose resource policy could never admit a
/// background run, so a boot that survives with `enabled: true` has proved the
/// unattended lane is reachable rather than merely switched on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBackgroundBehaviorSettings {
    /// Arm host-executed schedule and event behaviors for this process. Off
    /// leaves the scheduler and the event router unbuilt; the same process
    /// worker still drains the durable one-way owner notifications that
    /// foreground app workflows already accepted.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_app_behavior_tick_interval_seconds")]
    pub tick_interval_seconds: u64,
    #[serde(default = "default_app_behavior_max_installations_per_scope")]
    pub max_installations_per_scope: usize,
    #[serde(default = "default_app_behavior_max_claims_per_scope_tick")]
    pub max_claims_per_scope_tick: usize,
    #[serde(default = "default_app_behavior_lease_seconds")]
    pub lease_seconds: u64,
    #[serde(default = "default_app_behavior_retry_seconds")]
    pub retry_seconds: u64,
}

impl Default for AppBackgroundBehaviorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_seconds: default_app_behavior_tick_interval_seconds(),
            max_installations_per_scope: default_app_behavior_max_installations_per_scope(),
            max_claims_per_scope_tick: default_app_behavior_max_claims_per_scope_tick(),
            lease_seconds: default_app_behavior_lease_seconds(),
            retry_seconds: default_app_behavior_retry_seconds(),
        }
    }
}

const fn default_app_behavior_tick_interval_seconds() -> u64 {
    30
}

const fn default_app_behavior_max_installations_per_scope() -> usize {
    256
}

const fn default_app_behavior_max_claims_per_scope_tick() -> usize {
    8
}

const fn default_app_behavior_lease_seconds() -> u64 {
    120
}

const fn default_app_behavior_retry_seconds() -> u64 {
    30
}
/// Operator admission of one app-declared LLM operation name. Mirrors the
/// per-entry declaration discipline of `AppProcessingProfileTrust`: the
/// reviewed purpose acknowledges what the admitted name is for, so the
/// admission is an explicit operator decision rather than a bare allowlist
/// tick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppProcessingLlmOperationTrust {
    /// Bounded acknowledgment of the reviewed operation purpose.
    pub reviewed_purpose: String,
    /// Explicit operator-owned output ceiling for every physical request in
    /// this app operation lane. `None` preserves old configuration parsing,
    /// but is deliberately non-executable: the app dispatcher fails closed
    /// until an operator reviews and sets a positive ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppResourceRuntimeSettings {
    #[serde(default = "default_app_resource_max_tree_nodes")]
    pub max_tree_nodes: u32,
    #[serde(default = "default_app_resource_max_tree_depth")]
    pub max_tree_depth: u16,
    #[serde(default = "default_app_resource_max_journal_events")]
    pub max_journal_events: u32,
    #[serde(default = "default_app_resource_max_reservations")]
    pub max_reservations: u32,
    #[serde(default = "default_app_resource_max_active_intervals")]
    pub max_active_intervals: u32,
    #[serde(default = "default_app_resource_max_capability_families")]
    pub max_capability_families: u16,
    #[serde(default = "default_app_resource_max_no_progress_seconds")]
    pub max_no_progress_seconds: u64,
    #[serde(default = "default_app_resource_max_package_bytes")]
    pub max_package_bytes: u64,
    #[serde(default = "default_app_resource_max_background_starts_per_period")]
    pub max_background_starts_per_period: u64,
    #[serde(default = "default_app_resource_scheduler_capacity")]
    pub scheduler_capacity: u16,
    #[serde(default = "default_app_resource_foreground_reserved_slots")]
    pub foreground_reserved_slots: u16,
}

impl AppResourceRuntimeSettings {
    pub fn enforcement_policy(
        self,
    ) -> crate::magician_v2::apps::resource_contract::AppResourceEnforcementPolicy {
        crate::magician_v2::apps::resource_contract::AppResourceEnforcementPolicy {
            max_tree_nodes: self.max_tree_nodes,
            max_tree_depth: self.max_tree_depth,
            max_journal_events: self.max_journal_events,
            max_reservations: self.max_reservations,
            max_active_intervals: self.max_active_intervals,
            max_capability_families: self.max_capability_families,
            max_no_progress_seconds: self.max_no_progress_seconds,
            max_package_bytes: self.max_package_bytes,
            max_background_starts_per_period: self.max_background_starts_per_period,
            scheduler_capacity: self.scheduler_capacity,
            foreground_reserved_slots: self.foreground_reserved_slots,
        }
    }
}

impl Default for AppResourceRuntimeSettings {
    fn default() -> Self {
        Self {
            max_tree_nodes: default_app_resource_max_tree_nodes(),
            max_tree_depth: default_app_resource_max_tree_depth(),
            max_journal_events: default_app_resource_max_journal_events(),
            max_reservations: default_app_resource_max_reservations(),
            max_active_intervals: default_app_resource_max_active_intervals(),
            max_capability_families: default_app_resource_max_capability_families(),
            max_no_progress_seconds: default_app_resource_max_no_progress_seconds(),
            max_package_bytes: default_app_resource_max_package_bytes(),
            max_background_starts_per_period: default_app_resource_max_background_starts_per_period(
            ),
            scheduler_capacity: default_app_resource_scheduler_capacity(),
            foreground_reserved_slots: default_app_resource_foreground_reserved_slots(),
        }
    }
}

const fn default_app_resource_max_tree_nodes() -> u32 {
    256
}
const fn default_app_resource_max_tree_depth() -> u16 {
    16
}
const fn default_app_resource_max_journal_events() -> u32 {
    2_048
}
const fn default_app_resource_max_reservations() -> u32 {
    512
}
const fn default_app_resource_max_active_intervals() -> u32 {
    2_048
}
const fn default_app_resource_max_capability_families() -> u16 {
    64
}
const fn default_app_resource_max_no_progress_seconds() -> u64 {
    30
}
const fn default_app_resource_max_package_bytes() -> u64 {
    256 * 1_024 * 1_024
}
const fn default_app_resource_max_background_starts_per_period() -> u64 {
    1_000
}
const fn default_app_resource_scheduler_capacity() -> u16 {
    8
}
const fn default_app_resource_foreground_reserved_slots() -> u16 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppProcessingTrustSettings {
    #[serde(default = "default_app_endpoint_trust_revision")]
    pub endpoint_trust_revision: u64,
    /// Concrete profile used for `local_only` app content and as the safe
    /// default for `remote_allowed` content when remote processing is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_profile: Option<String>,
    /// Remote app processing enablement. DERIVED from
    /// `privacy.processing.mode` at load (cloud ⇒ remote app processing on,
    /// local ⇒ off) — no longer an independent operator switch, so the
    /// app boundary and the LLM locality can never disagree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote_processing_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, AppProcessingProfileTrust>,
}

impl Default for AppProcessingTrustSettings {
    fn default() -> Self {
        Self {
            endpoint_trust_revision: default_app_endpoint_trust_revision(),
            local_profile: None,
            remote_processing_enabled: false,
            remote_profile: None,
            profiles: BTreeMap::new(),
        }
    }
}

const fn default_app_endpoint_trust_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppProcessingEndpointClass {
    LoopbackManaged,
    TrustedSelfHosted,
    External,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppProviderRetentionPosture {
    /// The trusted profile and physical adapter must disable provider-side
    /// request/response storage for every protected app call.
    NoProviderStorage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppProcessingProfileTrust {
    pub class: AppProcessingEndpointClass,
    #[serde(default)]
    pub local_processing_eligible: bool,
    pub provider_retention: AppProviderRetentionPosture,
}

/// Analytics settings owned by the Magician runtime. MagicLLM emits typed
/// in-process observations; this policy decides whether any content may be
/// sanitized and persisted for a particular scope/operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MagicianAnalyticsSettings {
    #[serde(default)]
    pub llm_trace: LlmTraceSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmContentMode {
    Off,
    #[default]
    Metadata,
    Sanitized,
    FullLocalEncrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTraceRetentionSettings {
    #[serde(default = "default_llm_trace_facts_days")]
    pub facts_days: u32,
    #[serde(default = "default_llm_trace_context_metadata_days")]
    pub context_metadata_days: u32,
    #[serde(default = "default_llm_trace_sanitized_io_days")]
    pub sanitized_io_days: u32,
    #[serde(default = "default_llm_trace_encrypted_raw_days")]
    pub encrypted_raw_days: u32,
}

impl Default for LlmTraceRetentionSettings {
    fn default() -> Self {
        Self {
            facts_days: default_llm_trace_facts_days(),
            context_metadata_days: default_llm_trace_context_metadata_days(),
            sanitized_io_days: default_llm_trace_sanitized_io_days(),
            encrypted_raw_days: default_llm_trace_encrypted_raw_days(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTraceRedactionSettings {
    #[serde(default = "default_llm_trace_redaction_policy_version")]
    pub policy_version: String,
    #[serde(default = "default_true")]
    pub fail_to_metadata_only: bool,
    #[serde(default = "default_llm_trace_max_payload_bytes")]
    pub max_payload_bytes: usize,
    #[serde(default = "default_llm_trace_max_block_chars")]
    pub max_block_chars: usize,
}

impl Default for LlmTraceRedactionSettings {
    fn default() -> Self {
        Self {
            policy_version: default_llm_trace_redaction_policy_version(),
            fail_to_metadata_only: true,
            max_payload_bytes: default_llm_trace_max_payload_bytes(),
            max_block_chars: default_llm_trace_max_block_chars(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTraceCaptureOverride {
    pub content_mode: LlmContentMode,
    #[serde(default = "default_capture_rate")]
    pub sanitized_content_rate: f64,
    #[serde(default)]
    pub training_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTraceSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub content_mode: LlmContentMode,
    #[serde(default = "default_capture_rate")]
    pub metadata_capture_rate: f64,
    #[serde(default = "default_capture_rate")]
    pub sanitized_content_rate: f64,
    #[serde(default = "default_llm_trace_payload_records")]
    pub payload_records: usize,
    #[serde(default)]
    pub retention: LlmTraceRetentionSettings,
    #[serde(default)]
    pub redaction: LlmTraceRedactionSettings,
    #[serde(default)]
    pub training_default_eligible: bool,
    #[serde(default = "default_true")]
    pub exclude_public_guest_content: bool,
    /// Exact operation-name overrides. Unknown operations remain governed by
    /// the safe global default and do not silently inherit sanitized capture.
    #[serde(default)]
    pub operation_overrides: BTreeMap<String, LlmTraceCaptureOverride>,
    /// Exact `principal/workspace` overrides. Scope overrides are evaluated
    /// before operation overrides so an owner can fail a scope closed.
    #[serde(default)]
    pub scope_overrides: BTreeMap<String, LlmTraceCaptureOverride>,
}

impl Default for LlmTraceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            content_mode: LlmContentMode::Metadata,
            metadata_capture_rate: 1.0,
            sanitized_content_rate: 1.0,
            payload_records: default_llm_trace_payload_records(),
            retention: LlmTraceRetentionSettings::default(),
            redaction: LlmTraceRedactionSettings::default(),
            training_default_eligible: false,
            exclude_public_guest_content: true,
            operation_overrides: BTreeMap::new(),
            scope_overrides: BTreeMap::new(),
        }
    }
}

fn default_capture_rate() -> f64 {
    1.0
}

fn default_llm_trace_payload_records() -> usize {
    512
}

fn default_llm_trace_facts_days() -> u32 {
    90
}

fn default_llm_trace_context_metadata_days() -> u32 {
    90
}

fn default_llm_trace_sanitized_io_days() -> u32 {
    30
}

fn default_llm_trace_encrypted_raw_days() -> u32 {
    7
}

fn default_llm_trace_redaction_policy_version() -> String {
    "llm-content-redaction-v1".to_string()
}

fn default_llm_trace_max_payload_bytes() -> usize {
    512 * 1024
}

fn default_llm_trace_max_block_chars() -> usize {
    64 * 1024
}

pub const DEFAULT_RUNTIME_OLLAMA_KEEP_ALIVE: &str = "10m";
pub const DEFAULT_RUNTIME_OLLAMA_MAX_LOADED_MODELS: u32 = 2;
pub const DEFAULT_RUNTIME_OLLAMA_KV_CACHE_TYPE: &str = "q8_0";
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_BASE_URL: &str = "http://127.0.0.1:11435";
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_KEEP_ALIVE: &str = "-1";
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_NUM_PARALLEL: u32 = 1;
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_MAX_LOADED_MODELS: u32 = 1;
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_QUERY_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_RUNTIME_OLLAMA_EMBEDDING_WRITE_TIMEOUT_MS: u64 = 180_000;
pub const DEFAULT_RUNTIME_RESULT_CACHE_MAX_ENTRIES: usize = 512;
pub const DEFAULT_RUNTIME_RESULT_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_RUNTIME_LANCE_TABLE_POOL_MAX_IDLE: usize = 4;
pub const DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_ENTRIES: usize = 1024;
pub const DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_RUNTIME_VECTOR_SEARCH_MIN_ROWS: usize = 256;
pub const DEFAULT_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER: usize = 4;
pub const MAX_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER: usize = 32;
pub const DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_WINDOW_MS: u64 = 3;
pub const DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_ITEMS: usize = 8;
pub const DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_CHARS: usize = 6_000;

/// Runtime service policy for local processes and daemon-backed providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianRuntimeSettings {
    /// Pre-bootstrap Tokio / dispatch / live-agent sizing. `null` overrides
    /// inherit the selected profile. Default profile is `current`.
    #[serde(default)]
    pub scale: RuntimeScaleSettings,
    /// Retrieval fast-path policy. Result and query-vector caches default on;
    /// env kill switches remain restart-bound.
    #[serde(default)]
    pub retrieval: MagicianRetrievalRuntimeSettings,
    /// Ollama model residency and daemon policy.
    #[serde(default)]
    pub ollama: MagicianOllamaRuntimeSettings,
}

impl Default for MagicianRuntimeSettings {
    fn default() -> Self {
        Self {
            scale: RuntimeScaleSettings::default(),
            retrieval: MagicianRetrievalRuntimeSettings::default(),
            ollama: MagicianOllamaRuntimeSettings::default(),
        }
    }
}

/// Request-path retrieval accelerators. Hybrid score-map cache, Lance table
/// pool, and query-vector LRU default on. Vector search stays exhaustive
/// `flat` unless an owner opts into ANN shadow or activation. Restart-bound
/// env kill switches remain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianRetrievalRuntimeSettings {
    #[serde(default)]
    pub result_cache: MagicianResultCacheSettings,
    #[serde(default)]
    pub lance_table_pool: MagicianLanceTablePoolSettings,
    #[serde(default)]
    pub query_vector_cache: MagicianQueryVectorCacheSettings,
    #[serde(default)]
    pub vector_search: MagicianVectorSearchMode,
    #[serde(default)]
    pub ann: MagicianAnnSettings,
}

impl Default for MagicianRetrievalRuntimeSettings {
    fn default() -> Self {
        Self {
            result_cache: MagicianResultCacheSettings::default(),
            lance_table_pool: MagicianLanceTablePoolSettings::default(),
            query_vector_cache: MagicianQueryVectorCacheSettings::default(),
            vector_search: MagicianVectorSearchMode::default(),
            ann: MagicianAnnSettings::default(),
        }
    }
}

/// Hybrid vector-leg mode. Magician default is `ann` (IVF_PQ shortlist plus
/// exact L2 rerank, flat fallback). Crate `Default` in magician-vector-index
/// stays `flat` so unit tests that never install config keep exhaustive KNN.
/// `MAGICIAN_VECTOR_SEARCH` overrides at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagicianVectorSearchMode {
    Flat,
    AnnShadow,
    Ann,
}

impl Default for MagicianVectorSearchMode {
    fn default() -> Self {
        Self::Ann
    }
}

impl MagicianVectorSearchMode {
    fn as_vector_index(self) -> magician_vector_index::VectorSearchMode {
        match self {
            Self::Flat => magician_vector_index::VectorSearchMode::Flat,
            Self::AnnShadow => magician_vector_index::VectorSearchMode::AnnShadow,
            Self::Ann => magician_vector_index::VectorSearchMode::Ann,
        }
    }
}

/// IVF_PQ maintenance and ANN shortlist knobs. Used only when
/// `vector_search` is `ann_shadow` or `ann`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianAnnSettings {
    #[serde(default = "default_runtime_vector_search_min_rows")]
    pub min_rows: usize,
    #[serde(default = "default_runtime_vector_search_candidate_multiplier")]
    pub candidate_multiplier: usize,
}

impl Default for MagicianAnnSettings {
    fn default() -> Self {
        Self {
            min_rows: default_runtime_vector_search_min_rows(),
            candidate_multiplier: default_runtime_vector_search_candidate_multiplier(),
        }
    }
}

fn default_runtime_vector_search_min_rows() -> usize {
    DEFAULT_RUNTIME_VECTOR_SEARCH_MIN_ROWS
}

fn default_runtime_vector_search_candidate_multiplier() -> usize {
    DEFAULT_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER
}

/// Exact-query embedding vector LRU. A hit skips the embedding daemon but
/// still requires Lance. Magician default on; crate `Default` stays off until
/// Magician installs YAML. `MAGICIAN_QUERY_VECTOR_CACHE=off` is the kill
/// switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianQueryVectorCacheSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_runtime_query_vector_cache_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_runtime_query_vector_cache_max_bytes")]
    pub max_bytes: usize,
}

impl Default for MagicianQueryVectorCacheSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: default_runtime_query_vector_cache_max_entries(),
            max_bytes: default_runtime_query_vector_cache_max_bytes(),
        }
    }
}

fn default_runtime_query_vector_cache_max_entries() -> usize {
    DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_ENTRIES
}

fn default_runtime_query_vector_cache_max_bytes() -> usize {
    DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_BYTES
}

/// Idle Lance search-table pool keyed by index directory and generation.
/// Hybrid legs use independent handles. This flag controls reuse across
/// requests. `MAGICIAN_LANCE_TABLE_POOL=off` is the kill switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianLanceTablePoolSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_runtime_lance_table_pool_max_idle")]
    pub max_idle: usize,
}

impl Default for MagicianLanceTablePoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_idle: default_runtime_lance_table_pool_max_idle(),
        }
    }
}

fn default_runtime_lance_table_pool_max_idle() -> usize {
    DEFAULT_RUNTIME_LANCE_TABLE_POOL_MAX_IDLE
}

/// Revision-bound hybrid score-map cache. A hit skips journal I/O, inspect,
/// embedding, and both Lance search legs. Restart-bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianResultCacheSettings {
    /// Magician default on. `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE=off` is the
    /// kill switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_runtime_result_cache_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_runtime_result_cache_max_bytes")]
    pub max_bytes: usize,
}

impl Default for MagicianResultCacheSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: default_runtime_result_cache_max_entries(),
            max_bytes: default_runtime_result_cache_max_bytes(),
        }
    }
}

fn default_runtime_result_cache_max_entries() -> usize {
    DEFAULT_RUNTIME_RESULT_CACHE_MAX_ENTRIES
}

fn default_runtime_result_cache_max_bytes() -> usize {
    DEFAULT_RUNTIME_RESULT_CACHE_MAX_BYTES
}

/// Ollama request/runtime defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianOllamaRuntimeSettings {
    /// Request-level Ollama `keep_alive` duration. `null` leaves residency to
    /// Ollama's daemon defaults; env overrides still win for local debugging.
    #[serde(default = "default_runtime_ollama_keep_alive")]
    pub keep_alive: Option<String>,
    /// Maximum number of model runners the generation daemon may keep resident.
    /// The dedicated embedding daemon has an independent one-model limit.
    #[serde(default = "default_runtime_ollama_max_loaded_models")]
    pub max_loaded_models: u32,
    /// Installer-owned host gate and model tiers for the local generation
    /// model, written by `scripts/setup-ollama-host.sh` and mirrored by
    /// `magician-components/src/graph.yaml`.
    ///
    /// The runtime reads nothing out of it — the value the profiles need is
    /// pulled through the `&local_generation_model` YAML anchor, not through
    /// this struct. It is declared here only so the section parses: this struct
    /// is `deny_unknown_fields`, so an installer-owned block with no field to
    /// land in fails the whole config load and magician will not boot.
    ///
    /// Deliberately untyped. Making the runtime the schema authority for
    /// installer metadata is what coupled the two in the first place; the
    /// installer can add a tier without a Rust change, and no runtime behaviour
    /// depends on the shape.
    #[serde(default)]
    pub local_generation: Option<serde_yaml::Value>,
    /// Dedicated Ollama endpoint used only for embeddings.
    #[serde(default = "default_runtime_ollama_embedding_base_url")]
    pub embedding_base_url: String,
    /// Embedding model residency. `-1` pins the model for the daemon lifetime.
    #[serde(default = "default_runtime_ollama_embedding_keep_alive")]
    pub embedding_keep_alive: String,
    /// Verified physical dedicated-daemon capacity used by in-process
    /// admission. The current runner is single-sequence, so Magician requires
    /// exactly one and enforces foreground/background priority between calls.
    #[serde(default = "default_runtime_ollama_embedding_num_parallel")]
    pub embedding_num_parallel: u32,
    /// The dedicated daemon must host only the configured embedding model.
    #[serde(default = "default_runtime_ollama_embedding_max_loaded_models")]
    pub embedding_max_loaded_models: u32,
    /// End-to-end HTTP budget for priority retrieval embeddings.
    #[serde(default = "default_runtime_ollama_embedding_query_timeout_ms")]
    pub embedding_query_timeout_ms: u64,
    /// Extra gathering window for distinct query embeddings already queued
    /// behind a same-contract waiter. Magician default `3` ms. `0` never
    /// waits. A lone chat miss does not use this window.
    /// `MAGICIAN_EMBEDDING_QUERY_BATCH=off` forces pass-through together
    /// with `embedding_query_batch_max_items: 1`.
    #[serde(default = "default_runtime_embedding_query_batch_window_ms")]
    pub embedding_query_batch_window_ms: u64,
    /// Maximum distinct queries in one physical `/api/embed` call. `1` is
    /// pass-through. Magician default is `8`.
    #[serde(default = "default_runtime_embedding_query_batch_max_items")]
    pub embedding_query_batch_max_items: usize,
    /// Character budget for one query embedding HTTP batch.
    #[serde(default = "default_runtime_embedding_query_batch_max_chars")]
    pub embedding_query_batch_max_chars: usize,
    /// Per-batch HTTP budget for yieldable background embedding writes.
    #[serde(default = "default_runtime_ollama_embedding_write_timeout_ms")]
    pub embedding_write_timeout_ms: u64,
    /// Context allocated for local embedding requests.
    #[serde(default)]
    pub embedding_context_tokens: u32,
    /// Maximum token batch evaluated together by the embedding runner. This is
    /// independent of the number of texts grouped into one HTTP request.
    #[serde(default)]
    pub embedding_batch_tokens: u32,
    /// Maximum number of texts grouped into one embedding HTTP request.
    #[serde(default)]
    pub embedding_batch_size: usize,
    /// Embedding model warmed before Magician workers begin issuing calls.
    #[serde(default)]
    pub embedding_model: String,
    /// Expected vector width returned by the configured embedding model.
    #[serde(default)]
    pub embedding_dimensions: usize,
    /// Quantization used for Ollama's context KV cache. `q8_0` materially
    /// reduces dual-model context memory with negligible quality loss.
    #[serde(default = "default_runtime_ollama_kv_cache_type")]
    pub kv_cache_type: String,
    /// Enable Ollama flash attention, required for quantized KV caches.
    #[serde(default = "default_true")]
    pub flash_attention: bool,
    /// Preload configured generation models during startup. The embedding model
    /// is always preloaded and pinned by its dedicated daemon.
    #[serde(default = "default_true")]
    pub prewarm: bool,
    /// Replace a conflicting local Ollama daemon so launch settings are
    /// deterministic. Remote endpoints are never stopped or replaced.
    #[serde(default = "default_true")]
    pub replace_existing_local_daemon: bool,
}

/// A unique local generation model selected by at least one operation mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaGenerationModelConfig {
    pub model: String,
    pub context_tokens: u32,
}

impl Default for MagicianOllamaRuntimeSettings {
    fn default() -> Self {
        Self {
            keep_alive: default_runtime_ollama_keep_alive(),
            max_loaded_models: default_runtime_ollama_max_loaded_models(),
            local_generation: None,
            embedding_base_url: default_runtime_ollama_embedding_base_url(),
            embedding_keep_alive: default_runtime_ollama_embedding_keep_alive(),
            embedding_num_parallel: default_runtime_ollama_embedding_num_parallel(),
            embedding_max_loaded_models: default_runtime_ollama_embedding_max_loaded_models(),
            embedding_query_timeout_ms: default_runtime_ollama_embedding_query_timeout_ms(),
            embedding_query_batch_window_ms: default_runtime_embedding_query_batch_window_ms(),
            embedding_query_batch_max_items: default_runtime_embedding_query_batch_max_items(),
            embedding_query_batch_max_chars: default_runtime_embedding_query_batch_max_chars(),
            embedding_write_timeout_ms: default_runtime_ollama_embedding_write_timeout_ms(),
            embedding_context_tokens: 0,
            embedding_batch_tokens: 0,
            embedding_batch_size: 0,
            embedding_model: String::new(),
            embedding_dimensions: 0,
            kv_cache_type: default_runtime_ollama_kv_cache_type(),
            flash_attention: true,
            prewarm: true,
            replace_existing_local_daemon: true,
        }
    }
}

fn default_runtime_ollama_keep_alive() -> Option<String> {
    Some(DEFAULT_RUNTIME_OLLAMA_KEEP_ALIVE.to_string())
}

fn default_runtime_ollama_max_loaded_models() -> u32 {
    DEFAULT_RUNTIME_OLLAMA_MAX_LOADED_MODELS
}

fn default_runtime_ollama_embedding_base_url() -> String {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_BASE_URL.to_string()
}

fn default_runtime_ollama_embedding_keep_alive() -> String {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_KEEP_ALIVE.to_string()
}

fn default_runtime_ollama_embedding_num_parallel() -> u32 {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_NUM_PARALLEL
}

fn default_runtime_ollama_embedding_max_loaded_models() -> u32 {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_MAX_LOADED_MODELS
}

fn default_runtime_ollama_embedding_query_timeout_ms() -> u64 {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_QUERY_TIMEOUT_MS
}

fn default_runtime_embedding_query_batch_window_ms() -> u64 {
    DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_WINDOW_MS
}

fn default_runtime_embedding_query_batch_max_items() -> usize {
    DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_ITEMS
}

fn default_runtime_embedding_query_batch_max_chars() -> usize {
    DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_CHARS
}

fn default_runtime_ollama_embedding_write_timeout_ms() -> u64 {
    DEFAULT_RUNTIME_OLLAMA_EMBEDDING_WRITE_TIMEOUT_MS
}

fn default_runtime_ollama_kv_cache_type() -> String {
    DEFAULT_RUNTIME_OLLAMA_KV_CACHE_TYPE.to_string()
}

/// Coding model/profile configuration for Magician-owned coding flows.
///
/// This intentionally exposes Magician coding profiles, not Pi-specific
/// provider/model knobs. `run_coding_task` resolves the selected coding profile
/// through `llm.router.profiles` and translates that to the Pi process
/// internally.
/// Bounds for the phase-aware no-progress detector.
///
/// Each phase carries its own deadline because they fail differently: a
/// sixteen-minute test is silent by nature, a compaction emits nothing at all,
/// and an auto-retry announces its own backoff. A single idle timer across all
/// of them either kills healthy work or never fires. Semantics and the
/// reasoning behind each default live with the detector in
/// `magician_v2::execution::coding_engine::budgets`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingNoProgressSettings {
    /// Kill switch. A detector that mis-fires is worse than none.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Silence from the model with no text or thinking delta.
    #[serde(default = "default_coding_model_idle_secs")]
    pub model_idle_secs: u64,
    /// Silence from a running tool. Deliberately longer than the model's.
    #[serde(default = "default_coding_tool_idle_secs")]
    pub tool_idle_secs: u64,
    /// A tool that runs forever even while staying chatty — the case an
    /// inactivity bound alone can never catch.
    #[serde(default = "default_coding_tool_max_secs")]
    pub tool_max_secs: u64,
    #[serde(default = "default_coding_compaction_max_secs")]
    pub compaction_max_secs: u64,
    #[serde(default = "default_coding_summarization_max_secs")]
    pub summarization_max_secs: u64,
    /// Added to a retry's own declared backoff, never used in place of it.
    #[serde(default = "default_coding_retry_grace_secs")]
    pub retry_grace_secs: u64,
}

impl Default for CodingNoProgressSettings {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            model_idle_secs: default_coding_model_idle_secs(),
            tool_idle_secs: default_coding_tool_idle_secs(),
            tool_max_secs: default_coding_tool_max_secs(),
            compaction_max_secs: default_coding_compaction_max_secs(),
            summarization_max_secs: default_coding_summarization_max_secs(),
            retry_grace_secs: default_coding_retry_grace_secs(),
        }
    }
}

/// Deserialization shape for [`MagicianCodingSettings`].
///
/// It exists for one reason: `turn_timeout_secs` replaced `timeout_secs`, and a
/// live configuration must keep loading across that rename. Both keys are
/// accepted here so the [`TryFrom`] below can reject a conflict instead of
/// silently preferring one — which is how a config change becomes a mystery.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MagicianCodingSettingsWire {
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    turn_timeout_secs: Option<u64>,
    /// Legacy alias for `turn_timeout_secs`.
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    task_budget_secs: Option<u64>,
    #[serde(default)]
    verification_reserve_secs: Option<u64>,
    #[serde(default)]
    no_progress: CodingNoProgressSettings,
    #[serde(default)]
    persist_session: bool,
    #[serde(default)]
    profiles: Vec<CodingProfileConfig>,
    #[serde(default)]
    lead_agent_id: Option<String>,
    #[serde(default = "default_true")]
    contribute_direct: bool,
    #[serde(default)]
    codex: MagicianCodexSettings,
    #[serde(default)]
    grok: MagicianGrokSettings,
    #[serde(default)]
    claude: MagicianClaudeSettings,
    #[serde(default)]
    agy: MagicianAgySettings,
}

/// Coding budgets and profile catalog.
///
/// Three separate budgets, not one: a single number cannot bound a turn, bound
/// a task, and detect a hang at the same time — and when it tried, a
/// build → test → fix → re-test cycle on a large repository ran out of clock
/// mid-thought and failed indistinguishably from a genuine failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "MagicianCodingSettingsWire")]
/// Deserialization runs through [`MagicianCodingSettingsWire`], so the defaults
/// and the `deny_unknown_fields` gate live there. The attributes here are
/// serialization-only on purpose — a `#[serde(default)]` on these fields would
/// read as though it governed parsing when it does not.
pub struct MagicianCodingSettings {
    /// Profile id used when the caller does not choose one explicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// Outer wall-clock backstop for ONE Pi turn; `0` = none. Accepts the
    /// legacy key `timeout_secs`.
    ///
    /// Not the bound that decides when a healthy run stops — duration measures
    /// the wrong thing. `no_progress` and the cost ceiling do that.
    pub turn_timeout_secs: u64,
    /// Optional active-time ceiling for the WHOLE coding task. **`0` = none,
    /// and that is the default**: active time is measured and reported, but
    /// never used to stop work.
    pub task_budget_secs: u64,
    /// Held back from `task_budget_secs` so verification and repair cannot be
    /// starved. An explicit duration rather than a percentage: a project with a
    /// sixteen-minute suite and one with a twenty-second lint need different
    /// reserves, and a flat share is wrong for both. Inert when there is no
    /// task ceiling.
    pub verification_reserve_secs: u64,
    /// Bounds for the phase-aware hang detector.
    pub no_progress: CodingNoProgressSettings,
    /// Whether Pi sessions should be persisted by default. Callers can still
    /// override with the tool's `persist_session` argument.
    pub persist_session: bool,
    /// Operator-facing profile catalog for coding UIs. Each item points at an
    /// existing `llm.router.profiles` or adaptive profile id.
    pub profiles: Vec<CodingProfileConfig>,
    /// Agent id that drives VibeDev coding runs (the "lead engineer"). When a coding task
    /// carries the `vibedev` tag, `create_task` stamps THIS agent as the task owner instead
    /// of the client-sent value, so swapping the coding lead (e.g. EM → CTO) is a one-line
    /// config edit, not a UI/code change. `None` = fall back to the client-sent agent id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_agent_id: Option<String>,
    /// Whether `contribute_to_project` writes contributions DIRECTLY into the project repo
    /// (default) or stages them behind diff-approval HITL. Direct fits an autonomous team
    /// (the contributing agent already curated the artifact); set false to require operator
    /// approval before a contribution lands in the repo.
    pub contribute_direct: bool,
    /// Local `codex app-server` discovery. Disabled by default; the browser
    /// and model cannot set the binary path.
    #[serde(default)]
    pub codex: MagicianCodexSettings,
    /// Local `grok agent stdio` discovery. Disabled by default; the browser
    /// and model cannot set the binary path.
    #[serde(default)]
    pub grok: MagicianGrokSettings,
    /// Local `claude` headless discovery. Disabled by default; the browser
    /// and model cannot set the binary path.
    #[serde(default)]
    pub claude: MagicianClaudeSettings,
    /// Local `agy` headless discovery. Disabled by default; the browser
    /// and model cannot set the binary path.
    #[serde(default)]
    pub agy: MagicianAgySettings,
}

impl TryFrom<MagicianCodingSettingsWire> for MagicianCodingSettings {
    type Error = String;

    fn try_from(wire: MagicianCodingSettingsWire) -> Result<Self, Self::Error> {
        let turn_timeout_secs =
            resolve_turn_timeout_secs("coding", wire.turn_timeout_secs, wire.timeout_secs)?
                .unwrap_or_else(default_coding_turn_timeout_secs);
        Ok(Self {
            default_profile: wire.default_profile,
            turn_timeout_secs,
            task_budget_secs: wire
                .task_budget_secs
                .unwrap_or_else(default_coding_task_budget_secs),
            verification_reserve_secs: wire
                .verification_reserve_secs
                .unwrap_or_else(default_coding_verification_reserve_secs),
            no_progress: wire.no_progress,
            persist_session: wire.persist_session,
            profiles: wire.profiles,
            lead_agent_id: wire.lead_agent_id,
            contribute_direct: wire.contribute_direct,
            codex: wire.codex,
            grok: wire.grok,
            claude: wire.claude,
            agy: wire.agy,
        })
    }
}

/// Operator-only Codex discovery settings. Default off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MagicianCodexSettings {
    /// Kill switch. False keeps Codex undiscoverable and non-selectable.
    pub enabled: bool,
    /// Optional operator-selected Codex executable. Not accepted from
    /// browser, model, or tool input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

/// Operator-only Grok Build CLI discovery settings. Default off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MagicianGrokSettings {
    /// Kill switch. False keeps Grok undiscoverable and non-selectable.
    pub enabled: bool,
    /// Optional operator-selected Grok executable. Not accepted from
    /// browser, model, or tool input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

/// Operator-only Claude Code discovery settings. Default off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MagicianClaudeSettings {
    /// Kill switch. False keeps Claude undiscoverable and non-selectable.
    pub enabled: bool,
    /// Optional operator-selected Claude executable. Not accepted from
    /// browser, model, or tool input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// When true, copy Anthropic API keys from the parent allowlist into the
    /// child. Default false: Max subscription / OAuth must not inherit
    /// Magician's `ANTHROPIC_API_KEY`.
    #[serde(default)]
    pub use_api_key: bool,
}

/// Operator-only Antigravity (`agy`) discovery settings. Default off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MagicianAgySettings {
    /// Kill switch. False keeps Agy undiscoverable and non-selectable.
    pub enabled: bool,
    /// Optional operator-selected Agy executable. Not accepted from
    /// browser, model, or tool input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// When true, copy Gemini/Google keys from the parent allowlist into the
    /// child and treat those keys as auth. Default false: Antigravity OAuth
    /// must not inherit Magician's `GEMINI_API_KEY`.
    #[serde(default)]
    pub use_api_key: bool,
}

/// Reconcile the canonical `turn_timeout_secs` with the legacy `timeout_secs`.
///
/// Both present with **different** values is an error, not a preference. One of
/// the two is what the operator believes is in effect, and picking silently
/// guarantees that half the time the running system disagrees with the person
/// who configured it.
fn resolve_turn_timeout_secs(
    context: &str,
    canonical: Option<u64>,
    legacy: Option<u64>,
) -> Result<Option<u64>, String> {
    if let (Some(canonical), Some(legacy)) = (canonical, legacy) {
        if canonical != legacy {
            return Err(format!(
                "{context}: `turn_timeout_secs` ({canonical}) and the legacy `timeout_secs` \
                 ({legacy}) disagree. Remove `timeout_secs` — silently preferring one of them \
                 is how a config change becomes a mystery."
            ));
        }
    }
    if legacy.is_some() {
        tracing::warn!(
            target: "config",
            context,
            "`timeout_secs` is deprecated; rename it to `turn_timeout_secs`"
        );
    }
    Ok(canonical.or(legacy))
}

/// Deserialization shape for [`CodingProfileConfig`], carrying the same
/// `turn_timeout_secs` / `timeout_secs` reconciliation as the parent block.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodingProfileConfigWire {
    id: String,
    #[serde(default)]
    label: Option<String>,
    llm_profile: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    turn_timeout_secs: Option<u64>,
    /// Legacy alias for `turn_timeout_secs`.
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "CodingProfileConfigWire")]
pub struct CodingProfileConfig {
    /// Stable selector surfaced to tools/UI, e.g. `coding-balanced`.
    pub id: String,
    /// Operator-facing label shown in model/profile pickers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Existing `llm.router.profiles` or `llm.router.adaptive_profiles` id.
    pub llm_profile: String,
    /// Optional helper text for UI model pickers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional per-profile turn-timeout override. Accepts the legacy key
    /// `timeout_secs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_timeout_secs: Option<u64>,
    /// Keep a profile configured but hidden/unselectable.
    pub enabled: bool,
}

impl TryFrom<CodingProfileConfigWire> for CodingProfileConfig {
    type Error = String;

    fn try_from(wire: CodingProfileConfigWire) -> Result<Self, Self::Error> {
        let context = format!("coding.profiles[{}]", wire.id.trim());
        let turn_timeout_secs =
            resolve_turn_timeout_secs(&context, wire.turn_timeout_secs, wire.timeout_secs)?;
        Ok(Self {
            id: wire.id,
            label: wire.label,
            llm_profile: wire.llm_profile,
            description: wire.description,
            turn_timeout_secs,
            enabled: wire.enabled,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodingProfileInfo {
    pub id: String,
    pub label: String,
    pub llm_profile: String,
    pub provider: String,
    pub model: String,
    pub supports_user_image_inputs: bool,
    pub is_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCodingProfile {
    pub id: String,
    pub label: String,
    pub llm_profile: String,
    pub provider: String,
    pub model: String,
    pub supports_user_image_inputs: bool,
    /// Pi thinking/reasoning level derived from the LLM profile's reasoning
    /// effort (`off|minimal|low|medium|high|xhigh`), applied to Pi at session
    /// start. `None` = the profile is not reasoning-configured → Pi's default.
    pub thinking_level: Option<String>,
    pub api_key_env: Option<String>,
    /// Wall clock for ONE Pi turn under this profile.
    pub turn_timeout_secs: u64,
}

impl Default for MagicianCodingSettings {
    fn default() -> Self {
        Self {
            default_profile: None,
            turn_timeout_secs: default_coding_turn_timeout_secs(),
            task_budget_secs: default_coding_task_budget_secs(),
            verification_reserve_secs: default_coding_verification_reserve_secs(),
            no_progress: CodingNoProgressSettings::default(),
            persist_session: false,
            profiles: Vec::new(),
            lead_agent_id: None,
            contribute_direct: default_true(),
            codex: MagicianCodexSettings::default(),
            grok: MagicianGrokSettings::default(),
            claude: MagicianClaudeSettings::default(),
            agy: MagicianAgySettings::default(),
        }
    }
}

/// Verification-controller deployment configuration.
///
/// The controller itself defaults everything (`VerificationActivation` is
/// `Disabled`, `GateBudgets` carry the module's own limits), so this block is
/// pure override: absent, the runtime behaves exactly as it did when the env
/// var was the only switch. `MAGICIAN_VERIFICATION_CONTROLLER`, when set,
/// still overrides `mode` — it is the process-local emergency kill switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationConfig {
    /// `disabled` | `observe` | `enforce`. Parsed by
    /// `VerificationActivation::from_config`, so an unrecognised value
    /// degrades to `disabled` rather than failing the load — a typo must
    /// never be the thing that starts gating production completions, nor the
    /// thing that stops them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Repair rounds before a gate settles `exhausted`. Zero is legitimate:
    /// "verify once, never repair".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_repair_rounds: Option<u32>,
    /// USD ceiling across a gate's repair rounds. Unset = no spend ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spend_usd: Option<f64>,
    /// Wall-clock ceiling for a gate's whole lifecycle, idle time included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_elapsed_secs: Option<u64>,
    /// Reconciler tick. `0` disables periodic driving, leaving startup
    /// recovery as the only scheduler.
    #[serde(default = "default_verification_reconcile_interval_secs")]
    pub reconcile_interval_secs: u64,
    /// Owner baseline policy. Highest authority: the agent being gated
    /// cannot edit it, and a repository policy may only add to it — the
    /// anti-weakening resolver refuses removals and swaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<VerificationBaselineConfig>,
}

fn default_verification_reconcile_interval_secs() -> u64 {
    60
}

/// Hand-written rather than derived, and the difference is load-bearing.
///
/// `#[serde(default = "...")]` governs a *missing key inside a present block*.
/// A missing `verification:` block entirely takes the field default on
/// `MagicianConfig`, which is this impl — and a derived one would produce
/// `reconcile_interval_secs: 0`, i.e. **no periodic driver**, from the same
/// configuration that a written-out block turns into 60.
///
/// That divergence is not theoretical. `MAGICIAN_VERIFICATION_CONTROLLER`
/// overrides the mode without touching YAML, so an operator enabling observe
/// through the env var on a deployment with no `verification:` block would get
/// gates that are opened and then never driven — the exact failure the driver
/// exists to remove, reintroduced by a derive.
impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            mode: None,
            max_repair_rounds: None,
            max_spend_usd: None,
            max_elapsed_secs: None,
            reconcile_interval_secs: default_verification_reconcile_interval_secs(),
            baseline: None,
        }
    }
}

/// The owner baseline's shape in YAML.
///
/// A thin projection of `verification::VerificationPolicy` that omits
/// `source` — config *is* the owner surface, so letting it claim another
/// source would only weaken the policy's authority.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationBaselineConfig {
    #[serde(default)]
    pub required: Vec<crate::magician_v2::execution::verification::CheckSpec>,
    #[serde(default)]
    pub advisory: Vec<crate::magician_v2::execution::verification::CheckSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<crate::magician_v2::execution::verification::SandboxPolicy>,
}

impl VerificationConfig {
    /// The configured activation, before the env-var override is applied.
    pub fn activation(
        &self,
    ) -> crate::magician_v2::execution::verification::VerificationActivation {
        crate::magician_v2::execution::verification::VerificationActivation::from_config(
            self.mode.as_deref(),
        )
    }

    /// Runtime settings for `ArtifactV2Service::set_verification_settings`.
    pub fn runtime_settings(
        &self,
    ) -> crate::magician_v2::execution::verification::VerificationRuntimeSettings {
        use crate::magician_v2::execution::verification;
        verification::VerificationRuntimeSettings {
            budgets: verification::BudgetConfig {
                max_repair_rounds: self.max_repair_rounds,
                max_spend_usd: self.max_spend_usd,
                max_elapsed_secs: self.max_elapsed_secs,
            },
            baseline: self.baseline_policy(),
            reconcile_interval_secs: self.reconcile_interval_secs,
        }
    }

    /// The owner baseline as a policy, `None` when no checks are configured —
    /// an empty baseline block must not change the "repository defines its
    /// own verification" semantics.
    fn baseline_policy(
        &self,
    ) -> Option<crate::magician_v2::execution::verification::VerificationPolicy> {
        use crate::magician_v2::execution::verification;
        let baseline = self.baseline.as_ref()?;
        if baseline.required.is_empty() && baseline.advisory.is_empty() {
            return None;
        }
        Some(verification::VerificationPolicy {
            source: verification::PolicySource::Owner,
            required: baseline.required.clone(),
            advisory: baseline.advisory.clone(),
            total_timeout_secs: baseline.total_timeout_secs,
            sandbox: baseline.sandbox.clone().unwrap_or_default(),
            baseline_ref: Some("magician-config:verification.baseline".to_string()),
        })
    }
}

// The numbers themselves — and the reasoning behind each one — live with the
// detector that enforces them, so a bound and its justification cannot drift
// apart. See `magician_v2::execution::coding_engine::budgets`.
use crate::magician_v2::execution::coding_engine::budgets as coding_budgets;

fn default_coding_turn_timeout_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_TURN_TIMEOUT_SECS
}

fn default_coding_task_budget_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_TASK_BUDGET_SECS
}

fn default_coding_verification_reserve_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_VERIFICATION_RESERVE_SECS
}

fn default_coding_model_idle_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_MODEL_IDLE_SECS
}

fn default_coding_tool_idle_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_TOOL_IDLE_SECS
}

fn default_coding_tool_max_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_TOOL_MAX_SECS
}

fn default_coding_compaction_max_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_COMPACTION_MAX_SECS
}

fn default_coding_summarization_max_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_SUMMARIZATION_MAX_SECS
}

fn default_coding_retry_grace_secs() -> u64 {
    coding_budgets::DEFAULT_CODING_RETRY_GRACE_SECS
}

/// Static-site publishing policy for VibeDev projects.
///
/// Deployment is adapter-backed at the config boundary. `targets[]` is the
/// required multi-provider surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VibeDevDeployConfig {
    /// Master switch. When false, the publish endpoint returns a clear 503.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum wall-clock time for the static build step.
    #[serde(default = "default_vibedev_deploy_build_timeout_secs")]
    pub build_timeout_secs: u64,
    /// Maximum wall-clock time for the upload/publish command.
    #[serde(default = "default_vibedev_deploy_publish_timeout_secs")]
    pub publish_timeout_secs: u64,
    /// Hard byte cap for the produced static artifact.
    #[serde(default = "default_vibedev_deploy_max_artifact_bytes")]
    pub max_artifact_bytes: u64,
    /// Hard file cap for the produced static artifact.
    #[serde(default = "default_vibedev_deploy_max_artifact_files")]
    pub max_artifact_files: usize,
    /// Candidate output folders checked after a build when the request does not
    /// specify `output_dir`.
    #[serde(default = "default_vibedev_deploy_output_dirs")]
    pub output_dirs: Vec<String>,
    /// Multi-target deployment adapters. Each target declares provider,
    /// compatibility kind, output defaults, and its command adapter.
    #[serde(default)]
    pub targets: Vec<VibeDevDeployTargetConfig>,
}

impl Default for VibeDevDeployConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            build_timeout_secs: default_vibedev_deploy_build_timeout_secs(),
            publish_timeout_secs: default_vibedev_deploy_publish_timeout_secs(),
            max_artifact_bytes: default_vibedev_deploy_max_artifact_bytes(),
            max_artifact_files: default_vibedev_deploy_max_artifact_files(),
            output_dirs: default_vibedev_deploy_output_dirs(),
            targets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VibeDevDeployTargetConfig {
    /// Stable UI/API id, e.g. `cloudflare-pages`, `vercel-static`.
    pub id: String,
    /// Human-readable label shown in the VibeDev Studio target selector.
    #[serde(default)]
    pub label: String,
    /// Provider label persisted with deployment records.
    #[serde(default = "default_vibedev_deploy_provider")]
    pub provider: String,
    /// Compatibility kind. The current deploy handler supports `static`.
    #[serde(default = "default_vibedev_deploy_target_kind")]
    pub kind: String,
    /// Whether the target appears in the UI and can be selected.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Preferred target when the deploy request omits `target_id`.
    #[serde(default)]
    pub default: bool,
    /// Target-specific output folders. Falls back to top-level `output_dirs`.
    #[serde(default)]
    pub output_dirs: Vec<String>,
    /// Command adapter configuration for this target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<VibeDevDeployCommandConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VibeDevDeployCommandConfig {
    /// Program to execute, e.g. `wrangler`, `netlify`, `vercel`, or a local
    /// wrapper script.
    pub program: String,
    /// argv template. Supported placeholders:
    /// `{output_dir}`, `{project_id}`, `{project_name}`, `{site_slug}`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Regex used to extract the final public URL from stdout+stderr. The first
    /// capture group wins; otherwise the whole match is used. If omitted, the
    /// first HTTPS URL in the command output is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url_regex: Option<String>,
    /// Extra environment variable names copied from the Magician process into
    /// the trusted provider command. Build commands never receive this list.
    #[serde(default)]
    pub env_allowlist: Vec<String>,
}

fn default_vibedev_deploy_provider() -> String {
    "command".to_string()
}

fn default_vibedev_deploy_target_kind() -> String {
    "static".to_string()
}

fn default_vibedev_deploy_build_timeout_secs() -> u64 {
    300
}

fn default_vibedev_deploy_publish_timeout_secs() -> u64 {
    300
}

fn default_vibedev_deploy_max_artifact_bytes() -> u64 {
    100 * 1024 * 1024
}

fn default_vibedev_deploy_max_artifact_files() -> usize {
    10_000
}

fn default_vibedev_deploy_output_dirs() -> Vec<String> {
    vec![
        "dist".to_string(),
        "build".to_string(),
        "out".to_string(),
        ".svelte-kit/output/prerendered/pages".to_string(),
    ]
}

// The taste-profile settings type lives with the loader that interprets it,
// so the defaults and the behavior they bound cannot drift apart. See
// `magician_v2::taste_profile`.
use crate::magician_v2::taste_profile::TasteProfileSettings;

/// Memory retrieval and prompt-injection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MagicianMemorySettings {
    /// Bounded process-local cache of canonical prompt candidates and exact
    /// lexical features. Canonical files and the mutation journal remain the
    /// authority; this cache is disposable derived state.
    #[serde(default)]
    pub prompt_snapshot: MagicianMemoryPromptSnapshotSettings,
    /// Optional per-scope caps for memory prompt packing.
    ///
    /// These are the total section caps for each memory scope. Lane budgets
    /// below still decide how that total budget is split across semantic lanes.
    #[serde(default)]
    pub prompt_scope_budgets: MagicianMemoryPromptScopeBudgetSettings,
    /// Optional per-scope/per-semantic-lane overrides for memory prompt packing.
    ///
    /// Keys are scope ids (`user`, `agent`, `agent_goal`) containing semantic
    /// lane ids (`user_preference`, `procedure`, `project_context`, etc.).
    /// Missing lanes keep the default lane-share budgets.
    #[serde(default)]
    pub prompt_lane_budgets: MagicianMemoryPromptLaneBudgetSettings,
    /// Owner-edited taste profile note, loaded through the Notes provider and
    /// snapshotted for prompt injection.
    ///
    /// Optional in YAML, and deliberately absent from the shipped config
    /// files: this struct denies unknown fields, so an active key would fail
    /// to parse under an older binary sharing the same file. Defaults live on
    /// the settings struct; the config files document the key as a comment.
    #[serde(default)]
    pub taste_profile: TasteProfileSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MagicianMemoryPromptSnapshotSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_memory_prompt_snapshot_max_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_memory_prompt_snapshot_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_memory_prompt_snapshot_idle_ttl_secs")]
    pub idle_ttl_secs: u64,
    #[serde(default = "default_memory_prompt_snapshot_refresh_debounce_ms")]
    pub refresh_debounce_ms: u64,
}

impl Default for MagicianMemoryPromptSnapshotSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: default_memory_prompt_snapshot_max_bytes(),
            max_entries: default_memory_prompt_snapshot_max_entries(),
            idle_ttl_secs: default_memory_prompt_snapshot_idle_ttl_secs(),
            refresh_debounce_ms: default_memory_prompt_snapshot_refresh_debounce_ms(),
        }
    }
}

fn default_memory_prompt_snapshot_max_bytes() -> usize {
    64 * 1024 * 1024
}

fn default_memory_prompt_snapshot_max_entries() -> usize {
    8
}

fn default_memory_prompt_snapshot_idle_ttl_secs() -> u64 {
    10 * 60
}

fn default_memory_prompt_snapshot_refresh_debounce_ms() -> u64 {
    100
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMemoryScopeBudget {
    pub max_entries: usize,
    pub max_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MagicianMemoryPromptScopeBudgetSettings {
    #[serde(default = "default_memory_prompt_user_scope_budget")]
    pub user: ConfiguredMemoryScopeBudget,
    #[serde(default = "default_memory_prompt_agent_scope_budget")]
    pub agent: ConfiguredMemoryScopeBudget,
    #[serde(default = "default_memory_prompt_agent_goal_scope_budget")]
    pub agent_goal: ConfiguredMemoryScopeBudget,
}

impl Default for MagicianMemoryPromptScopeBudgetSettings {
    fn default() -> Self {
        Self {
            user: default_memory_prompt_user_scope_budget(),
            agent: default_memory_prompt_agent_scope_budget(),
            agent_goal: default_memory_prompt_agent_goal_scope_budget(),
        }
    }
}

impl MagicianMemoryPromptScopeBudgetSettings {
    pub fn user_effective(&self) -> ResolvedMemoryScopeBudget {
        self.user
            .effective(default_memory_prompt_user_scope_budget())
    }

    pub fn agent_effective(&self) -> ResolvedMemoryScopeBudget {
        self.agent
            .effective(default_memory_prompt_agent_scope_budget())
    }

    pub fn agent_goal_effective(&self) -> ResolvedMemoryScopeBudget {
        self.agent_goal
            .effective(default_memory_prompt_agent_goal_scope_budget())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredMemoryScopeBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

impl ConfiguredMemoryScopeBudget {
    fn effective(self, default: ConfiguredMemoryScopeBudget) -> ResolvedMemoryScopeBudget {
        ResolvedMemoryScopeBudget {
            max_entries: self.max_entries.or(default.max_entries).unwrap_or(0),
            max_chars: self.max_chars.or(default.max_chars).unwrap_or(0),
        }
    }
}

fn default_memory_prompt_user_scope_budget() -> ConfiguredMemoryScopeBudget {
    ConfiguredMemoryScopeBudget {
        max_entries: Some(24),
        max_chars: Some(12_000),
    }
}

fn default_memory_prompt_agent_scope_budget() -> ConfiguredMemoryScopeBudget {
    ConfiguredMemoryScopeBudget {
        max_entries: Some(24),
        max_chars: Some(12_000),
    }
}

fn default_memory_prompt_agent_goal_scope_budget() -> ConfiguredMemoryScopeBudget {
    ConfiguredMemoryScopeBudget {
        max_entries: Some(24),
        max_chars: Some(16_000),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MagicianMemoryPromptLaneBudgetSettings {
    #[serde(default)]
    pub user: BTreeMap<String, ConfiguredMemoryLaneBudget>,
    #[serde(default)]
    pub agent: BTreeMap<String, ConfiguredMemoryLaneBudget>,
    #[serde(default)]
    pub agent_goal: BTreeMap<String, ConfiguredMemoryLaneBudget>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredMemoryLaneBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

/// Media model/provider configuration for non-LLM rails.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MagicianMediaSettings {
    /// Ordered text-to-speech providers used by reply playback, auto-speak, and
    /// task-completion read-outs. Host-local macOS TTS is registered
    /// automatically only after the desktop host speech verifier reports it
    /// available; this list is for remote/configured providers.
    #[serde(default)]
    pub tts: TtsConfig,
    /// One-shot recorded speech-to-text providers used by voice notes and
    /// dictation. Meeting/listening rails use their own streaming STT selector.
    #[serde(default)]
    pub recording_stt: RecordingSttConfig,
    /// Named audio engine families used by stage-provider bindings. Secrets and
    /// machine-local process paths do not belong here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engines: BTreeMap<String, crate::magician_v2::media_seam::AudioEngineConfig>,
    /// Reusable VAD stage bindings. Empty preserves current capture-local gates.
    #[serde(
        default,
        skip_serializing_if = "crate::magician_v2::media_seam::AudioProviderCatalogConfig::is_empty"
    )]
    pub vad: crate::magician_v2::media_seam::AudioProviderCatalogConfig,
    /// Streaming STT bindings advertised to Meeting and Listening profiles.
    #[serde(
        default,
        skip_serializing_if = "crate::magician_v2::media_seam::AudioProviderCatalogConfig::is_empty"
    )]
    pub streaming_stt: crate::magician_v2::media_seam::AudioProviderCatalogConfig,
    /// Independently selectable diarization bindings. Embedded STT
    /// diarization remains metadata on its streaming STT provider.
    #[serde(
        default,
        skip_serializing_if = "crate::magician_v2::media_seam::AudioProviderCatalogConfig::is_empty"
    )]
    pub diarization: crate::magician_v2::media_seam::AudioProviderCatalogConfig,
    /// Named per-surface pipelines and their global defaults. Older configs
    /// with missing defaults are migrated once and persisted before startup.
    #[serde(
        default,
        skip_serializing_if = "crate::magician_v2::media_seam::AudioSurfaceProfilesConfig::is_empty"
    )]
    pub surface_profiles: crate::magician_v2::media_seam::AudioSurfaceProfilesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// Ordered local/online provider list. First configured provider is the
    /// fallback-chain head when the macOS system provider is unavailable; UI
    /// can request any configured `id` per synthesis.
    #[serde(default)]
    pub providers: Vec<TtsProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsProviderConfig {
    /// Stable request/provider id surfaced to the UI, e.g. `openai`,
    /// `openai-fast`, `kokoro-local`, or `minimax-hd`.
    pub id: String,
    /// Optional human label rendered by the frontend selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Runtime adapter implementation. `openai_speech` posts JSON to an
    /// OpenAI-compatible `/v1/audio/speech` endpoint, `minimax_t2a_v2` uses the
    /// MiniMax native endpoint, `gemini_tts` uses Gemini generateContent
    /// audio output, `grok_tts` posts JSON to xAI `/v1/tts`, and
    /// `fluid_audio_kokoro_tts` uses the supervised local FluidAudio sidecar.
    #[serde(default = "default_tts_adapter")]
    pub adapter: String,
    /// Provider model id. This belongs in config so model upgrades do not
    /// require code changes.
    #[serde(default)]
    pub model: String,
    /// Provider voice id. When omitted, the adapter default is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Provider response format. When omitted, the adapter default is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Configured voices advertised for this model. Empty means only the
    /// provider's default voice is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<String>,
    /// Configured output formats advertised for this model. Empty means only
    /// the provider's default response format is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    /// Language codes supported by this configured model binding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
    /// Config-owned feature flags such as `pronunciation_control`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<String>,
    /// Optional model variant consumed by local engines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Optional pinned model repository revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Optional artifact checksum where an adapter supports one artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Per-model idle residency override for managed local engines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// Environment variable that contains the provider API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Optional endpoint override. Useful for OpenAI-compatible proxies or local
    /// servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// MiniMax group id. Prefer `group_id_env` for secrets/config that differs
    /// by machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Environment variable containing the MiniMax group id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id_env: Option<String>,
    /// Config switch for keeping a profile around without advertising it.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_tts_adapter() -> String {
    "openai_speech".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RecordingSttConfig {
    /// Ordered provider list. First provider is the default/fallback-chain head;
    /// UI can request any configured `id` per transcription.
    #[serde(default)]
    pub providers: Vec<RecordingSttProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingSttProviderConfig {
    /// Stable request/provider id surfaced to the UI, e.g. `openai`,
    /// `openai-mini`, `gemini-fast`.
    pub id: String,
    /// Optional human label rendered by the frontend selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Runtime adapter implementation. `openai_transcriptions` posts multipart
    /// audio to an OpenAI-compatible `/v1/audio/transcriptions` endpoint.
    #[serde(default = "default_recording_stt_adapter")]
    pub adapter: String,
    /// Optional local engine owner. FluidAudio recording adapters use
    /// `fluid_audio`; online adapters may omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_id: Option<String>,
    /// Provider model id. This belongs in config so model upgrades do not require
    /// code changes.
    pub model: String,
    /// Optional model artifact variant, revision, checksum, and idle residency
    /// policy for local engines. Remote providers ignore these fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// Environment variable that contains the provider API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Optional endpoint override. Useful for OpenAI-compatible proxies or local
    /// servers. Google adapters treat this as the API root, not the final method
    /// URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Environment variable containing an OAuth bearer token. Used by Google
    /// Cloud APIs that require IAM-scoped auth instead of API-key auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token_env: Option<String>,
    /// Google Cloud project id for adapters that construct resource names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Environment variable containing the Google Cloud project id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id_env: Option<String>,
    /// Google Cloud location/region for adapters that construct resource names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Google Speech recognizer id or full
    /// `projects/{project}/locations/{location}/recognizers/{recognizer}` name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recognizer: Option<String>,
    /// Preferred recognition languages for STT adapters that accept BCP-47
    /// language-code lists. Empty means provider default/request hint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
    /// Adapter-level prompt for generative transcription providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Maximum raw audio bytes sent inline by generative adapters before they
    /// reject the request and ask for a file-backed path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_max_bytes: Option<usize>,
    /// Optional Gemini `generation_config` object. Kept as raw JSON/YAML so new
    /// model-side generation knobs do not require code changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<serde_json::Value>,
    /// Optional Cloud Speech `RecognitionConfig.features` object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_features: Option<serde_json::Value>,
    /// Optional shallow overrides merged into Cloud Speech `RecognitionConfig`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_config: Option<serde_json::Value>,
    /// Config switch for keeping a profile around without advertising it.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_recording_stt_adapter() -> String {
    "openai_transcriptions".to_string()
}

/// Controls behavior when the agent hits CannotProceed or LoopDetected.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OnFailureMode {
    /// Pause execution and ask user for help (default)
    #[default]
    AskUser,
    /// Terminal failure (legacy behavior)
    Fail,
}

/// Settings for the execution engine / Magicutor integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicianExecutionSettings {
    /// Base URL for the Magicutor service.
    #[serde(default = "default_magicutor_base_url")]
    pub magicutor_base_url: String,
    /// Environment variable that stores the Magicutor API key (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magicutor_api_key_env: Option<String>,
    /// Timeout for execution requests (seconds).
    #[serde(default = "default_execution_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Shell sandbox policy for native shell actions.
    #[serde(default)]
    pub shell_sandbox: ShellSandboxConfig,
    /// File sandbox policy for native file actions.
    #[serde(default)]
    pub file_sandbox: FileSandboxConfig,
    /// What to do when agent cannot proceed or detects a loop.
    /// "ask_user" (default) pauses and asks for help. "fail" keeps current terminal behavior.
    #[serde(default)]
    pub on_failure: OnFailureMode,
    /// Who thinks inside each loop iteration. `magician` is the built-in
    /// decision path. A launchable harness such as `pi` replaces
    /// decide+execute with one Plane turn. Unknown names fail closed.
    #[serde(default = "default_harness_engine")]
    pub harness_engine: String,
    /// The model the driving harness runs; `default` = the CLI's choice.
    #[serde(default = "default_harness_model")]
    pub harness_model: String,
    /// Optional Magician LLM profile for Pi-driven agentic turns. When absent,
    /// Pi uses its own credentials and default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_profile: Option<String>,
    /// How many plane `tools/call`s a harness turn may make before the
    /// plane stops serving and control returns to the loop. Matches
    /// `AgentConstraints.max_iterations` (4000), not the plan sketch's
    /// interleave value of 40.
    #[serde(default = "default_harness_turn_max_tool_calls")]
    pub harness_turn_max_tool_calls: u32,
    /// Wall-clock bound for one harness turn, seconds. Matches
    /// [`DEFAULT_AGENTIC_MAX_DURATION_SECS`] (40 minutes).
    #[serde(default = "default_harness_turn_max_seconds")]
    pub harness_turn_max_seconds: u64,
}

/// Who thinks a **chat** turn. Orthogonal to `execution.harness_engine`
/// (the loop decide seam). Default `magician` is today's LLM mouth.
/// Launchable roster names (`claude_code`, `codex`, `codex_app_server`,
/// `grok`, `agy`) replace the mouth; native-tool strip is per engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianChatTurnSettings {
    #[serde(default = "default_harness_engine")]
    pub harness_engine: String,
    /// The model the chat harness runs; `default` = the CLI's choice.
    #[serde(default = "default_harness_model")]
    pub harness_model: String,
    #[serde(default = "default_harness_turn_max_tool_calls")]
    pub harness_turn_max_tool_calls: u32,
    #[serde(default = "default_harness_turn_max_seconds")]
    pub harness_turn_max_seconds: u64,
}

impl Default for MagicianChatTurnSettings {
    fn default() -> Self {
        Self {
            harness_engine: default_harness_engine(),
            harness_model: default_harness_model(),
            harness_turn_max_tool_calls: default_harness_turn_max_tool_calls(),
            harness_turn_max_seconds: default_harness_turn_max_seconds(),
        }
    }
}

impl Default for MagicianExecutionSettings {
    fn default() -> Self {
        Self {
            magicutor_base_url: default_magicutor_base_url(),
            magicutor_api_key_env: None,
            request_timeout_secs: default_execution_timeout_secs(),
            shell_sandbox: ShellSandboxConfig::default(),
            file_sandbox: FileSandboxConfig::default(),
            on_failure: OnFailureMode::default(),
            harness_engine: default_harness_engine(),
            harness_model: default_harness_model(),
            pi_profile: None,
            harness_turn_max_tool_calls: default_harness_turn_max_tool_calls(),
            harness_turn_max_seconds: default_harness_turn_max_seconds(),
        }
    }
}

/// Channel-assist understanding and bounded migration policy. LLM profile
/// selection remains under `llm.router.operation_mapping`; this section owns
/// the data contract rather than provider credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelAssistConfig {
    #[serde(default)]
    pub distillation: ChannelAssistDistillationConfig,
}

impl Default for ChannelAssistConfig {
    fn default() -> Self {
        Self {
            distillation: ChannelAssistDistillationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelAssistDistillationConfig {
    /// `1` keeps the legacy summary contract; `2` enables the information-
    /// complete safe brief in the existing local distillation call.
    #[serde(default = "default_channel_brief_contract_version")]
    pub brief_contract_version: u32,
    #[serde(default = "default_channel_brief_summary_max_chars")]
    pub summary_max_chars: usize,
    #[serde(default)]
    pub backfill: ChannelAssistBackfillConfig,
}

impl Default for ChannelAssistDistillationConfig {
    fn default() -> Self {
        Self {
            brief_contract_version: default_channel_brief_contract_version(),
            summary_max_chars: default_channel_brief_summary_max_chars(),
            backfill: ChannelAssistBackfillConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelAssistBackfillConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_channel_brief_backfill_lookback_days")]
    pub lookback_days: u32,
    #[serde(default = "default_channel_brief_backfill_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_true")]
    pub surfaced_first: bool,
}

impl Default for ChannelAssistBackfillConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lookback_days: default_channel_brief_backfill_lookback_days(),
            batch_size: default_channel_brief_backfill_batch_size(),
            surfaced_first: true,
        }
    }
}

fn default_channel_brief_contract_version() -> u32 {
    2
}

fn default_channel_brief_summary_max_chars() -> usize {
    900
}

fn default_channel_brief_backfill_lookback_days() -> u32 {
    30
}

fn default_channel_brief_backfill_batch_size() -> usize {
    2
}

/// Progressive attention-learning controls shared by Follow-ups and Worth a
/// look. Outcome capture and rank enforcement are deliberately separate so a
/// deployment can accumulate/replay typed evidence before changing ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionLearningConfig {
    /// Record canonical outcomes and attempt bounded semantic propagation.
    #[serde(default)]
    pub enabled: bool,
    /// Apply learned order to owner-facing lists. When false, scores and rank
    /// deltas are still returned as observe-only metadata.
    #[serde(default)]
    pub semantic_ranking_enabled: bool,
    /// Maximum active candidates embedded/re-scored synchronously after one
    /// explicit owner outcome.
    #[serde(default = "default_attention_learning_rescore_limit")]
    pub rescore_limit: usize,
    /// Maximum labelled semantic neighbours used by the Bayesian estimator.
    /// This is a versioned model parameter selected by grouped replay evals.
    #[serde(default = "default_attention_learning_neighbor_count")]
    pub neighbor_count: usize,
    /// Beta prior for each binary usefulness/actionability posterior.
    #[serde(default = "default_attention_learning_prior_alpha")]
    pub prior_alpha: f64,
    #[serde(default = "default_attention_learning_prior_beta")]
    pub prior_beta: f64,
    /// RBF kernel bandwidth over cosine distance. This is a model parameter,
    /// not a hand-authored spam rule.
    #[serde(default = "default_attention_learning_kernel_bandwidth")]
    pub kernel_bandwidth: f64,
    /// Minimum total neighbour weight before a learned probability is allowed
    /// to influence rank. Lower evidence remains observable and baseline-tied.
    #[serde(default = "default_attention_learning_min_evidence_weight")]
    pub min_evidence_weight: f64,
    /// One absolute timeout for the feedback-triggered embedding batch. This is
    /// a user-visible latency budget: the owner is waiting on the response.
    #[serde(default = "default_attention_learning_embedding_timeout_ms")]
    pub embedding_timeout_ms: u64,
    /// Budget for one background embedding pass (bind repair, active-score
    /// refresh). Nothing is waiting on these, and the local embedding model is
    /// routinely evicted by a resident chat model, so a cold load can cost far
    /// more than any request path would tolerate. Spending the request budget
    /// here means such work can never complete at all.
    #[serde(default = "default_attention_learning_background_embedding_timeout_ms")]
    pub background_embedding_timeout_ms: u64,
    /// Revision-bound semantic extraction and coverage repair. This remains
    /// independently disabled so enabling outcome learning cannot create model
    /// traffic or mutate candidate semantics.
    #[serde(default)]
    pub semantic_backfill: AttentionSemanticBackfillConfig,
    /// Durable, no-LLM post-feedback current-universe rank diagnostics.
    /// Outcomes enqueue work independently; this flag controls only leasing.
    #[serde(default)]
    pub rank_recompute: AttentionRankRecomputeConfig,
    /// Cutoff-bound migration of durable pre-Slice-1 owner labels plus bounded
    /// steady-state score refresh for newly active candidates. The worker is
    /// idempotent and never tails events created after its first cutoff.
    #[serde(default)]
    pub historical_bootstrap: AttentionHistoricalBootstrapConfig,
    /// Versioned supervised actionability model. Slice 2 defaults to disabled
    /// so semantic extraction can accumulate before any score is served.
    #[serde(default)]
    pub actionability: AttentionActionabilityConfig,
    /// Slice-3 learned duplicate/underlying-obligation grouping. Defaults to
    /// disabled and requires one explicitly pinned immutable pair snapshot.
    #[serde(default)]
    pub grouping: AttentionGroupingConfig,
    /// Slice-4 calibrated lane routing and verified web impressions. Baseline
    /// mode still records replayable decisions but never changes a lane.
    #[serde(default)]
    pub routing: AttentionRoutingConfig,
    /// Slice-5 personal Bayesian contextual ranking. Disabled unless an exact
    /// immutable policy is explicitly pinned; it never changes lanes.
    #[serde(default)]
    pub bandit: AttentionBanditConfig,
}

impl Default for AttentionLearningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            semantic_ranking_enabled: false,
            rescore_limit: default_attention_learning_rescore_limit(),
            neighbor_count: default_attention_learning_neighbor_count(),
            prior_alpha: default_attention_learning_prior_alpha(),
            prior_beta: default_attention_learning_prior_beta(),
            kernel_bandwidth: default_attention_learning_kernel_bandwidth(),
            min_evidence_weight: default_attention_learning_min_evidence_weight(),
            embedding_timeout_ms: default_attention_learning_embedding_timeout_ms(),
            background_embedding_timeout_ms:
                default_attention_learning_background_embedding_timeout_ms(),
            semantic_backfill: AttentionSemanticBackfillConfig::default(),
            rank_recompute: AttentionRankRecomputeConfig::default(),
            historical_bootstrap: AttentionHistoricalBootstrapConfig::default(),
            actionability: AttentionActionabilityConfig::default(),
            grouping: AttentionGroupingConfig::default(),
            routing: AttentionRoutingConfig::default(),
            bandit: AttentionBanditConfig::default(),
        }
    }
}

/// Operational limits for historical migration, durable live repair tails,
/// pending embedding binds, and current-cohort score refresh. These values
/// bound IO/model work; label meaning remains defined by typed source actions
/// and the canonical vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionHistoricalBootstrapConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_attention_historical_bootstrap_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_attention_historical_bootstrap_interval_secs")]
    pub interval_secs: u64,
}

impl Default for AttentionHistoricalBootstrapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            batch_size: default_attention_historical_bootstrap_batch_size(),
            interval_secs: default_attention_historical_bootstrap_interval_secs(),
        }
    }
}

/// Operational bounds for durable post-feedback rank recomputation. These are
/// queue safety limits, never learned ranking or filtering heuristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionRankRecomputeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_attention_rank_recompute_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_attention_rank_recompute_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_attention_rank_recompute_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_attention_rank_recompute_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_attention_rank_recompute_retry_base_secs")]
    pub retry_base_secs: u64,
    #[serde(default = "default_attention_rank_recompute_retry_max_secs")]
    pub retry_max_secs: u64,
    #[serde(default = "default_attention_rank_recompute_lease_secs")]
    pub lease_secs: u64,
    #[serde(default = "default_attention_rank_recompute_retention_days")]
    pub retention_days: u64,
}

impl Default for AttentionRankRecomputeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            batch_size: default_attention_rank_recompute_batch_size(),
            concurrency: default_attention_rank_recompute_concurrency(),
            interval_secs: default_attention_rank_recompute_interval_secs(),
            max_retries: default_attention_rank_recompute_max_retries(),
            retry_base_secs: default_attention_rank_recompute_retry_base_secs(),
            retry_max_secs: default_attention_rank_recompute_retry_max_secs(),
            lease_secs: default_attention_rank_recompute_lease_secs(),
            retention_days: default_attention_rank_recompute_retention_days(),
        }
    }
}

/// Operational bounds for the asynchronous semantic coverage worker. These
/// are circuit breakers and scheduling limits, never content-quality rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionSemanticBackfillConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_attention_semantic_backfill_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_attention_semantic_backfill_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_attention_semantic_backfill_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_attention_semantic_backfill_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_attention_semantic_backfill_retry_base_secs")]
    pub retry_base_secs: u64,
    #[serde(default = "default_attention_semantic_backfill_retry_max_secs")]
    pub retry_max_secs: u64,
    #[serde(default = "default_attention_semantic_backfill_lease_secs")]
    pub lease_secs: u64,
    #[serde(default = "default_attention_semantic_backfill_calls_per_minute")]
    pub calls_per_minute: u32,
}

impl Default for AttentionSemanticBackfillConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            batch_size: default_attention_semantic_backfill_batch_size(),
            concurrency: default_attention_semantic_backfill_concurrency(),
            interval_secs: default_attention_semantic_backfill_interval_secs(),
            max_retries: default_attention_semantic_backfill_max_retries(),
            retry_base_secs: default_attention_semantic_backfill_retry_base_secs(),
            retry_max_secs: default_attention_semantic_backfill_retry_max_secs(),
            lease_secs: default_attention_semantic_backfill_lease_secs(),
            calls_per_minute: default_attention_semantic_backfill_calls_per_minute(),
        }
    }
}

/// Deployment mode for the calibrated Slice-2 actionability model. `shadow`
/// computes and exposes a preview but preserves the existing order;
/// `enforced` may rank only candidates with a compatible revision-bound score
/// and safely falls back to the Slice-1 score for all others.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionActionabilityMode {
    #[default]
    Disabled,
    Shadow,
    Enforced,
}

impl AttentionActionabilityMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Shadow => "shadow",
            Self::Enforced => "enforced",
        }
    }
}

impl std::str::FromStr for AttentionActionabilityMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "shadow" => Ok(Self::Shadow),
            "enforced" => Ok(Self::Enforced),
            other => anyhow::bail!("unknown attention actionability mode: {other}"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionActionabilityConfig {
    #[serde(default)]
    pub mode: AttentionActionabilityMode,
    /// Immutable snapshot id stored in `attention_learning.db`. Required for
    /// both shadow and enforced modes; no implicit "latest" model is allowed.
    #[serde(default)]
    pub snapshot_id: Option<String>,
    /// Offline trainer Magician can run on its own. Disabled by default so a
    /// config without this block stays inert.
    #[serde(default)]
    pub training: AttentionActionabilityTrainingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionActionabilityTrainingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_attention_actionability_training_interval_secs")]
    pub interval_secs: u64,
    /// When a run passes the trainer gates, install the artifact. The first
    /// scope install is forced to shadow; a later passing install may become
    /// enforced. Installation still does not rewrite YAML.
    #[serde(default = "default_true")]
    pub auto_install: bool,
}

impl Default for AttentionActionabilityTrainingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_attention_actionability_training_interval_secs(),
            auto_install: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionGroupingMode {
    #[default]
    Disabled,
    Shadow,
    Enforced,
}

impl AttentionGroupingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Shadow => "shadow",
            Self::Enforced => "enforced",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionGroupingConfig {
    #[serde(default)]
    pub mode: AttentionGroupingMode,
    /// Immutable pair-model snapshot id. Required for shadow/enforced; no
    /// implicit latest snapshot or hidden merge threshold is allowed.
    #[serde(default)]
    pub snapshot_id: Option<String>,
    /// Integrity/latency circuit breaker for complete-universe pair scoring.
    /// Exceeding it disables grouping for that projection and returns every
    /// item as a singleton; candidates and pairs are never truncated.
    #[serde(default = "default_attention_grouping_max_pair_evaluations")]
    pub max_pair_evaluations: u64,
}

impl Default for AttentionGroupingConfig {
    fn default() -> Self {
        Self {
            mode: AttentionGroupingMode::Disabled,
            snapshot_id: None,
            max_pair_evaluations: default_attention_grouping_max_pair_evaluations(),
        }
    }
}

/// Slice-4 routing deliberately has no `enforced` mode. The only serving
/// mutation is a deterministic, reversible canary; later promotion requires a
/// separately reviewed policy generation rather than silently broadening this
/// first routing slice.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionRoutingMode {
    #[default]
    Baseline,
    Shadow,
    Canary,
}

impl AttentionRoutingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Shadow => "shadow",
            Self::Canary => "canary",
        }
    }
}

impl std::str::FromStr for AttentionRoutingMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "shadow" => Ok(Self::Shadow),
            "canary" => Ok(Self::Canary),
            other => anyhow::bail!("unknown attention routing mode: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionRoutingConfig {
    #[serde(default)]
    pub mode: AttentionRoutingMode,
    /// Immutable policy snapshot id. Shadow/canary fail closed to baseline
    /// when it is absent, unreadable, invalid, or contract-incompatible.
    #[serde(default)]
    pub snapshot_id: Option<String>,
    /// Fraction of stable scope/surface identities admitted to the canary.
    /// This is experiment allocation, not a utility or quality threshold.
    #[serde(default)]
    pub canary_fraction: f64,
    /// Versioned seed identity included in deterministic cohort hashing and
    /// persisted with every decision for exact replay.
    #[serde(default = "default_attention_routing_seed_identity")]
    pub seed_identity: String,
    /// Visibility is verified only after this dwell (1..=60,000 ms). API
    /// return is never an impression and the threshold is persisted beside
    /// every receipt.
    #[serde(default = "default_attention_routing_min_visible_ms")]
    pub min_visible_ms: u64,
    #[serde(default = "default_attention_routing_visibility_rule_version")]
    pub visibility_rule_version: String,
    #[serde(default)]
    pub training: AttentionActionabilityTrainingConfig,
}

impl Default for AttentionRoutingConfig {
    fn default() -> Self {
        Self {
            mode: AttentionRoutingMode::Baseline,
            snapshot_id: None,
            canary_fraction: 0.0,
            seed_identity: default_attention_routing_seed_identity(),
            min_visible_ms: default_attention_routing_min_visible_ms(),
            visibility_rule_version: default_attention_routing_visibility_rule_version(),
            training: AttentionActionabilityTrainingConfig::default(),
        }
    }
}

/// Slice-5 ranking can mutate only the bounded first page of an already safe
/// lane. There is deliberately no broad enforced mode in this first personal
/// policy generation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionBanditMode {
    #[default]
    Disabled,
    Shadow,
    Canary,
}

impl AttentionBanditMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Shadow => "shadow",
            Self::Canary => "canary",
        }
    }
}

impl std::str::FromStr for AttentionBanditMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "shadow" => Ok(Self::Shadow),
            "canary" => Ok(Self::Canary),
            other => anyhow::bail!("unknown attention bandit mode: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionBanditConfig {
    #[serde(default)]
    pub mode: AttentionBanditMode,
    /// Optional YAML pin. Leave null so Magician's store install can shadow
    /// first and later promote to canary. A pin wins over that auto path.
    #[serde(default)]
    pub snapshot_id: Option<String>,
    /// Stable scope/surface canary allocation. Exploration mass, draw count,
    /// slate size, and seed identity are snapshot-owned and cannot be tuned in
    /// mutable runtime config.
    #[serde(default)]
    pub canary_fraction: f64,
    /// Default number of cards materialized in one frozen delivery page.
    #[serde(default = "default_attention_delivery_page_size")]
    pub delivery_default_page_size: usize,
    /// Hard request bound for a frozen delivery page. This is an operational
    /// bound only; the root decision always records the complete lane.
    #[serde(default = "default_attention_delivery_max_page_size")]
    pub delivery_max_page_size: usize,
    /// Lifetime of an opaque cursor and its frozen decision binding.
    #[serde(default = "default_attention_delivery_ttl_secs")]
    pub delivery_ttl_secs: u64,
    /// Analytical retention after expiry for delivery/page audit records.
    #[serde(default = "default_attention_delivery_retention_days")]
    pub delivery_retention_days: u64,
    /// Magician can train and install a personal ranking snapshot on its own.
    /// YAML `mode: disabled` is an operator pin, not a kill switch for that
    /// background loop.
    #[serde(default)]
    pub training: AttentionActionabilityTrainingConfig,
}

impl Default for AttentionBanditConfig {
    fn default() -> Self {
        Self {
            mode: AttentionBanditMode::Disabled,
            snapshot_id: None,
            canary_fraction: 0.0,
            delivery_default_page_size: default_attention_delivery_page_size(),
            delivery_max_page_size: default_attention_delivery_max_page_size(),
            delivery_ttl_secs: default_attention_delivery_ttl_secs(),
            delivery_retention_days: default_attention_delivery_retention_days(),
            training: AttentionActionabilityTrainingConfig::default(),
        }
    }
}

fn default_attention_delivery_page_size() -> usize {
    50
}

fn default_attention_delivery_max_page_size() -> usize {
    200
}

fn default_attention_delivery_ttl_secs() -> u64 {
    900
}

fn default_attention_delivery_retention_days() -> u64 {
    30
}

fn default_attention_routing_seed_identity() -> String {
    "attention-routing-slice4-v1".to_string()
}

fn default_attention_routing_min_visible_ms() -> u64 {
    1_000
}

fn default_attention_routing_visibility_rule_version() -> String {
    "attention-visible-dwell-v1".to_string()
}

fn default_attention_grouping_max_pair_evaluations() -> u64 {
    // Covers the observed ~2,014-item universe (~2.03M pairs) with bounded
    // headroom. This is an operational circuit breaker, not a quality cap.
    2_500_000
}

#[cfg(any(test, feature = "test-fixtures"))]
mod attention_grouping_config_tests {
    use super::*;

    #[test]
    fn grouping_pair_budget_has_finite_default_and_yaml_override() {
        assert_eq!(
            AttentionGroupingConfig::default().max_pair_evaluations,
            2_500_000
        );
        let parsed: AttentionGroupingConfig =
            serde_yaml::from_str("mode: disabled\nsnapshot_id: null\nmax_pair_evaluations: 17\n")
                .unwrap();
        assert_eq!(parsed.max_pair_evaluations, 17);
        assert_eq!(parsed.mode, AttentionGroupingMode::Disabled);
    }

    #[test]
    fn routing_defaults_are_baseline_and_nonactivating() {
        let config = AttentionRoutingConfig::default();
        assert_eq!(config.mode, AttentionRoutingMode::Baseline);
        assert!(config.snapshot_id.is_none());
        assert_eq!(config.canary_fraction, 0.0);
        assert!(config.min_visible_ms > 0);

        let parsed: AttentionRoutingConfig = serde_yaml::from_str(
            "mode: shadow\nsnapshot_id: route-v1\ncanary_fraction: 0.0\nseed_identity: eval-v1\nmin_visible_ms: 750\nvisibility_rule_version: visible-v2\n",
        )
        .unwrap();
        assert_eq!(parsed.mode, AttentionRoutingMode::Shadow);
        assert_eq!(parsed.snapshot_id.as_deref(), Some("route-v1"));
        assert_eq!(parsed.min_visible_ms, 750);
    }

    #[test]
    fn bandit_defaults_are_disabled_and_nonactivating() {
        let config = AttentionBanditConfig::default();
        assert_eq!(config.mode, AttentionBanditMode::Disabled);
        assert!(config.snapshot_id.is_none());
        assert_eq!(config.canary_fraction, 0.0);
        assert_eq!(config.delivery_default_page_size, 50);
        assert_eq!(config.delivery_max_page_size, 200);
        assert_eq!(config.delivery_ttl_secs, 900);
        assert_eq!(config.delivery_retention_days, 30);
        assert!(!config.training.enabled);
        let parsed: AttentionBanditConfig = serde_yaml::from_str(
            "mode: shadow\nsnapshot_id: bandit-v1\ncanary_fraction: 0.0\ndelivery_default_page_size: 25\ndelivery_max_page_size: 100\ndelivery_ttl_secs: 600\ndelivery_retention_days: 14\n",
        )
        .unwrap();
        assert_eq!(parsed.mode, AttentionBanditMode::Shadow);
        assert_eq!(parsed.snapshot_id.as_deref(), Some("bandit-v1"));
        assert_eq!(parsed.delivery_default_page_size, 25);
        assert_eq!(parsed.delivery_max_page_size, 100);
        assert_eq!(parsed.delivery_ttl_secs, 600);
        assert_eq!(parsed.delivery_retention_days, 14);
    }
}

fn default_attention_learning_rescore_limit() -> usize {
    100
}

fn default_attention_learning_neighbor_count() -> usize {
    32
}

fn default_attention_learning_prior_alpha() -> f64 {
    1.0
}

fn default_attention_learning_prior_beta() -> f64 {
    1.0
}

fn default_attention_learning_kernel_bandwidth() -> f64 {
    0.20
}

fn default_attention_learning_min_evidence_weight() -> f64 {
    0.25
}

fn default_attention_learning_embedding_timeout_ms() -> u64 {
    5_000
}

fn default_attention_learning_background_embedding_timeout_ms() -> u64 {
    // Enough to absorb one cold model load plus the rest of a claimed batch.
    // The pass persists each vector as it lands, so exhausting this budget
    // costs the remainder of one batch, never the work already done.
    120_000
}

fn default_attention_semantic_backfill_batch_size() -> usize {
    60
}

fn default_attention_semantic_backfill_concurrency() -> usize {
    2
}

fn default_attention_semantic_backfill_interval_secs() -> u64 {
    60
}

fn default_attention_semantic_backfill_max_retries() -> u32 {
    5
}

fn default_attention_semantic_backfill_retry_base_secs() -> u64 {
    30
}

fn default_attention_semantic_backfill_retry_max_secs() -> u64 {
    3_600
}

fn default_attention_semantic_backfill_lease_secs() -> u64 {
    120
}

fn default_attention_semantic_backfill_calls_per_minute() -> u32 {
    60
}

fn default_attention_rank_recompute_batch_size() -> usize {
    20
}
fn default_attention_rank_recompute_concurrency() -> usize {
    2
}
fn default_attention_rank_recompute_interval_secs() -> u64 {
    60
}
fn default_attention_rank_recompute_max_retries() -> u32 {
    5
}
fn default_attention_rank_recompute_retry_base_secs() -> u64 {
    30
}
fn default_attention_rank_recompute_retry_max_secs() -> u64 {
    3_600
}
fn default_attention_rank_recompute_lease_secs() -> u64 {
    120
}
fn default_attention_rank_recompute_retention_days() -> u64 {
    30
}
fn default_attention_historical_bootstrap_batch_size() -> usize {
    20
}
fn default_attention_actionability_training_interval_secs() -> u64 {
    3_600
}

fn default_attention_historical_bootstrap_interval_secs() -> u64 {
    60
}

/// Feature gates for propagating rich briefs into resurfacing and actions.
/// V2 distillation may run in shadow while every user-facing flag remains off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResurfacingConfig {
    #[serde(default)]
    pub rich_briefs_enabled: bool,
    #[serde(default)]
    pub source_details_enabled: bool,
    #[serde(default)]
    pub contextual_actions_enabled: bool,
    #[serde(default)]
    pub recommendations_enabled: bool,
    /// Recheck already-surfaced communication cards against current routing.
    #[serde(default)]
    pub active_repair_enabled: bool,
    #[serde(default = "default_resurfacing_active_repair_batch_size")]
    pub active_repair_batch_size: usize,
    /// Candidates the curator promotes to `surfaced` per sweep. Already-surfaced
    /// items are not evicted, so the visible pool grows across sweeps; this only
    /// bounds how many are added each pass. Overridable at deploy time via
    /// `RESURFACING_SURFACE_CAP`.
    #[serde(default = "default_resurfacing_surface_cap")]
    pub surface_cap: usize,
    #[serde(default = "default_resurfacing_recommendation_min_confidence")]
    pub recommendation_min_confidence: f64,
    #[serde(default = "default_resurfacing_action_result_cooldown_days")]
    pub action_result_cooldown_days: u32,
    /// Total budget for building the scope reference embedding set, including
    /// cold local-model load and all bounded batches.
    #[serde(default = "default_resurfacing_centrality_reference_timeout_secs")]
    pub centrality_reference_timeout_secs: u64,
    /// Per-candidate embedding timeout after the reference set is available.
    #[serde(default = "default_resurfacing_centrality_query_timeout_secs")]
    pub centrality_query_timeout_secs: u64,
    /// Deprecated compatibility input. Reference construction now persists one
    /// logical reference at a time so successful prefix progress survives a
    /// shared-deadline expiry; this value is parsed but intentionally ignored.
    #[serde(default = "default_resurfacing_centrality_reference_batch_size")]
    pub centrality_reference_batch_size: usize,
    /// Memory tiers whose entries may resurface to the owner.
    ///
    /// An allowlist rather than a denylist, because the memory source reads
    /// whatever tiers the scope's knowledge store happens to contain: with a
    /// denylist, every tier added later silently reaches the owner's lane. That
    /// is exactly how the agent's own operating memory came to hold 90% of the
    /// eligible corpus and the entire top of the ranking.
    ///
    /// The default is the owner-facing half of `USER_MEMORY_TIERS`, so tiers
    /// are classified once where they are defined rather than tracked in a
    /// second list here. `workflows` and `organization` are excluded because
    /// they are marked as the agent's, not by being left out of a literal.
    #[serde(default = "default_resurfacing_memory_tiers")]
    pub memory_tiers: Vec<String>,
}

impl Default for ResurfacingConfig {
    fn default() -> Self {
        Self {
            rich_briefs_enabled: false,
            source_details_enabled: false,
            contextual_actions_enabled: false,
            recommendations_enabled: false,
            active_repair_enabled: false,
            active_repair_batch_size: default_resurfacing_active_repair_batch_size(),
            surface_cap: default_resurfacing_surface_cap(),
            recommendation_min_confidence: default_resurfacing_recommendation_min_confidence(),
            action_result_cooldown_days: default_resurfacing_action_result_cooldown_days(),
            centrality_reference_timeout_secs:
                default_resurfacing_centrality_reference_timeout_secs(),
            centrality_query_timeout_secs: default_resurfacing_centrality_query_timeout_secs(),
            centrality_reference_batch_size: default_resurfacing_centrality_reference_batch_size(),
            memory_tiers: default_resurfacing_memory_tiers(),
        }
    }
}

/// Internal fleet social-network policy.
///
/// This is deliberately owned by `magician-config.yaml`: autonomous social
/// activity must not be enabled, paused, or retuned by process environment
/// drift. The worker remains read-only with respect to task/execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocialConfig {
    /// Master switch for autonomous agent social activity.
    #[serde(default)]
    pub enabled: bool,
    /// Durable/configured operator pause. When true, the worker performs no
    /// roster, budget, LLM, or write work.
    #[serde(default)]
    pub paused: bool,
    /// Exact scopes whose agent rosters may be invited autonomously. Explicit
    /// ownership avoids adopting stale, system, or ephemeral-eval directories
    /// merely because they exist below the storage root.
    #[serde(default = "default_social_scopes")]
    pub scopes: Vec<SocialScopeConfig>,
    /// Low-frequency admission cadence.
    #[serde(default = "default_social_tick_interval_secs")]
    pub tick_interval_secs: u64,
    /// Minimum time between autonomous posts by one agent.
    #[serde(default = "default_social_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Maximum agents admitted to the LLM gate during one worker tick. The
    /// worker rotates the starting point each tick so this bounds bursts
    /// without permanently starving agents later in the roster.
    #[serde(default = "default_social_max_agents_per_tick")]
    pub max_agents_per_tick: usize,
    /// Daily token ceiling used when an agent has no explicit
    /// `social_persona.daily_tokens` override.
    #[serde(default = "default_social_daily_tokens")]
    pub default_daily_tokens: u64,
    /// Fail-closed reservation charged before the small engagement gate.
    #[serde(default = "default_social_gate_reserve_tokens")]
    pub gate_reserve_tokens: u64,
    /// Fail-closed reservation charged before composing a post.
    #[serde(default = "default_social_compose_reserve_tokens")]
    pub compose_reserve_tokens: u64,
    /// Defensive output boundary before a post can enter the scoped store.
    #[serde(default = "default_social_max_post_chars")]
    pub max_post_chars: usize,
    /// Age bound for posts, deliveries, reactions, and spend diagnostics.
    #[serde(default = "default_social_retention_days")]
    pub retention_days: u32,
    /// Hard corpus bound per scoped social store, enforced incrementally.
    #[serde(default = "default_social_max_posts_per_scope")]
    pub max_posts_per_scope: usize,
    /// Hard diagnostic-log bound per scoped social store.
    #[serde(default = "default_social_max_spend_log_rows")]
    pub max_spend_log_rows: usize,
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paused: false,
            scopes: default_social_scopes(),
            tick_interval_secs: default_social_tick_interval_secs(),
            cooldown_secs: default_social_cooldown_secs(),
            max_agents_per_tick: default_social_max_agents_per_tick(),
            default_daily_tokens: default_social_daily_tokens(),
            gate_reserve_tokens: default_social_gate_reserve_tokens(),
            compose_reserve_tokens: default_social_compose_reserve_tokens(),
            max_post_chars: default_social_max_post_chars(),
            retention_days: default_social_retention_days(),
            max_posts_per_scope: default_social_max_posts_per_scope(),
            max_spend_log_rows: default_social_max_spend_log_rows(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SocialScopeConfig {
    pub principal: String,
    pub workspace: String,
}

/// Cadence for the delivery-hygiene sweep — the writer that turns a provider's
/// hard bounces and complaints into suppression-register entries.
///
/// Owned by `magician-config.yaml` for the same reason the social policy is:
/// who may be contacted must not be retuned by process environment drift.
///
/// # On by default, unlike the maturity sweep
///
/// `outcome_learning`'s sweep ships **off** because it records judgements
/// against a waiting window somebody has to choose, and observations it writes
/// cannot be un-written. This sweep chooses nothing: the mapping from a
/// provider's cause to a suppression reason is total and already made, and
/// every entry it writes cites the ledger row that established it. The failure
/// mode of not running it is mailing people who bounced or complained — which
/// is exactly the state a fail-closed screen over an empty register produces.
/// So the conservative default here is *on*, at a gentle interval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryHygieneConfig {
    /// Master switch for the sweep.
    #[serde(default = "default_delivery_hygiene_enabled")]
    pub enabled: bool,
    /// Durable operator pause. When true, no ledger is read and no register is
    /// written.
    #[serde(default)]
    pub paused: bool,
    /// Exact scopes whose delivery ledgers are swept. Explicit ownership, in
    /// the shape `social.scopes` already uses: enumerating whatever directories
    /// happen to exist below the storage root would adopt stale, system and
    /// ephemeral-eval scopes and write suppression entries into them.
    ///
    /// An enabled sweep naming no scope refuses to start rather than ticking
    /// forever over nothing.
    #[serde(default = "default_delivery_hygiene_scopes")]
    pub scopes: Vec<DeliveryHygieneScopeConfig>,
    /// How often to sweep, in seconds. Floored at 60 by the worker.
    ///
    /// Fifteen minutes: bounces and complaints arrive over minutes, not
    /// seconds, and the register is idempotent so a missed tick costs only
    /// latency. A tighter interval buys re-reads of a ledger that has not
    /// changed.
    #[serde(default = "default_delivery_hygiene_tick_interval_secs")]
    pub tick_interval_secs: u64,
    /// Whether the same tick also asks what the ledger has never heard back
    /// about (`delivery_hygiene::silence`).
    ///
    /// On by default and read-only: the watch writes nothing at all, so the
    /// cost of running it is two reads and the cost of not running it is that a
    /// silently broken provider integration produces no signal whatsoever.
    #[serde(default = "default_silence_watch_enabled")]
    pub silence_watch_enabled: bool,
    /// How long an act may go unacknowledged before it counts as overdue.
    ///
    /// Twenty-four hours: providers acknowledge in seconds and deliver in
    /// minutes, so a day of total silence is already far past any honest
    /// in-flight window, and a shorter grace would report every send that left
    /// in the last hour. Refused at or below zero by the worker — a zero grace
    /// makes every live send overdue the instant it leaves, and a signal that
    /// is always on says nothing.
    #[serde(default = "default_silence_grace_hours")]
    pub silence_grace_hours: i64,
    /// How many overdue acts one scope may hold before the watch reports
    /// `degraded`.
    ///
    /// One, deliberately. An act that no provider acknowledged a full day after
    /// it left is already the failure this watch exists to surface, and a
    /// higher floor is a decision to stay quiet about the first few. Raise it
    /// where a rail is known to be unreconcilable and the noise is not
    /// actionable — but raise it on purpose, in this file, where somebody can
    /// read that the choice was made.
    ///
    /// Refused at zero by the worker: a floor of zero reports every tick as
    /// degraded whatever the ledger says, including one where every send was
    /// acknowledged, and a signal that is always on is one nobody reads.
    #[serde(default = "default_silence_alert_overdue")]
    pub silence_alert_overdue: usize,
    /// How many overdue acts the tick names in its health snapshot.
    ///
    /// The counts are the signal; the named acts are what makes the signal
    /// investigable. Refused at zero by the worker: a report that names nothing
    /// reads exactly like a scope with nothing to report.
    #[serde(default = "default_silence_named_acts")]
    pub silence_named_acts: usize,
    /// Where delivery status notifications come back to, if anywhere.
    ///
    /// `None` — the default — is what this build shipped with, and the health
    /// snapshot says so on every tick: `receipts_state: "no_source"`, never
    /// `idle`. That distinction is the whole point. Without a mailbox, every
    /// live send lands in `dispatch_unknown` and stays there, a hard bounce
    /// never reaches the suppression register, and the next send to a dead
    /// address goes out exactly as if the first had worked.
    #[serde(default)]
    pub bounce_mailbox: Option<BounceMailboxConfig>,
}

impl Default for DeliveryHygieneConfig {
    fn default() -> Self {
        Self {
            enabled: default_delivery_hygiene_enabled(),
            paused: false,
            scopes: default_delivery_hygiene_scopes(),
            tick_interval_secs: default_delivery_hygiene_tick_interval_secs(),
            silence_watch_enabled: default_silence_watch_enabled(),
            silence_grace_hours: default_silence_grace_hours(),
            silence_alert_overdue: default_silence_alert_overdue(),
            silence_named_acts: default_silence_named_acts(),
            bounce_mailbox: None,
        }
    }
}

/// The mailbox a durable run's out-of-band verification arrives in.
///
/// Absent — the default — means the run-inbox sweep has no source, and a run
/// that raises a wait waits until a person notices and closes it by hand. That
/// is the state §5 calls *"the primitive nobody has"*: the run store held up its
/// half from the day it was built, and nothing ever read a mailbox and told it.
///
/// A directory of `.eml` files, for the same reason `delivery_hygiene`'s bounce
/// mailbox is one: every way a message reaches a host ends in something that can
/// write a file, so it commits to no provider's API.
///
/// # Nothing is consumed
///
/// Unlike the bounce mailbox, this one is never settled — a run inbox is an
/// ORDINARY mailbox carrying the verification code and every other message that
/// person receives, and consuming what the sweep walked past would take mail
/// that was never ours. Safe because the run store is idempotent on the event
/// ref, so re-reading writes nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunInboxConfig {
    /// A directory of `.eml` files.
    pub maildir_path: String,
    /// The `source_hint` messages from this directory are reported under.
    ///
    /// It has to match the hint a run raised its wait with, and that is a
    /// person's word for *"my inbox"* rather than a filesystem location — so it
    /// is stated here rather than derived from the path. Deriving it would make
    /// whether a verification matches depend on where the mail is spooled.
    pub source_hint: String,
}

/// The mailbox the bounce bridge reads.
///
/// # Why `provider` is here and not derived
///
/// The send index is keyed on the provider name, so a lookup under the wrong
/// one finds nothing and refuses a bounce that was perfectly correlatable — and
/// the refusal counts as `uncorrelated`, which reads as a broken send-side
/// index rather than as a misconfigured string. It must therefore be stated by
/// whoever arranged for the mail to arrive here, who is the only party that
/// knows which sender's bounces this directory holds. A default would be a
/// guess that fails as a different bug.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BounceMailboxConfig {
    /// The provider whose sends these bounces belong to — the same name the
    /// dispatch path records on its receipts.
    pub provider: String,
    /// A directory of `.eml` files. Every way a bounce reaches a host ends in
    /// something that can write a file, so this commits to no provider API:
    /// a postmaster alias piped to a script, `fetchmail` into a maildir, a
    /// webhook receiver dropping messages, a shared volume.
    ///
    /// Settled reports are RENAMED beside themselves, never deleted — a hard
    /// bounce is not operationally liftable, so the evidence has to outlive the
    /// suppression that used it.
    pub maildir_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct DeliveryHygieneScopeConfig {
    pub principal: String,
    pub workspace: String,
}

fn default_delivery_hygiene_enabled() -> bool {
    true
}

fn default_delivery_hygiene_scopes() -> Vec<DeliveryHygieneScopeConfig> {
    vec![DeliveryHygieneScopeConfig {
        principal: "anonymous".to_string(),
        workspace: "default".to_string(),
    }]
}

fn default_delivery_hygiene_tick_interval_secs() -> u64 {
    900
}

/// Cadence and cohorts for the **maturity sweep** — the only producer of
/// `silent` outcome observations.
///
/// Owned by `magician-config.yaml` for the same reason the delivery-hygiene
/// block is: what gets recorded about a counterparty must not be retuned by
/// process environment drift.
///
/// # Off by default, unlike the delivery-hygiene sweep
///
/// That sweep chooses nothing — the mapping from a provider's cause to a
/// suppression reason is total and already made. This one records a
/// **judgement**: that a counterparty who has not answered within a window
/// somebody chose has decided. The window is the judgement, an append-only
/// store cannot un-write the observations it produces, and a process that
/// failed to wire its configuration must record nothing rather than record
/// silences against a waiting period nobody chose. So it ships off, and an
/// operator turns it on after naming the scopes and the cohorts.
///
/// # Why the cohorts are declared here at all
///
/// An outward act records what went out, to whom, on which exact payload, and
/// when. It does **not** record which variant version was live when it did,
/// because that is the deciding subsystem's fact — and plan §2 is explicit that
/// where no deterministic fact is available the answer is always a person's.
/// Declaring *"this exact payload is version 3 of this variant"* is how that
/// person states it. An act whose payload nobody declared is counted and left
/// unswept, never bound to a default: an observation filed under a cohort key
/// nobody chose is comparable to nothing and cannot be withdrawn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutcomeMaturityConfig {
    /// Master switch. **Off by default** — see the type note.
    #[serde(default)]
    pub enabled: bool,
    /// Durable operator pause. When true, no index is read and no observation
    /// is written.
    #[serde(default)]
    pub paused: bool,
    /// How often to sweep, in seconds. Floored at 60 by the worker.
    ///
    /// Hourly: a maturity window is measured in days, so a tighter interval
    /// buys re-reads of an index that has not changed.
    #[serde(default = "default_outcome_maturity_tick_interval_secs")]
    pub tick_interval_secs: u64,
    /// The default waiting window, in days.
    ///
    /// Refused at or below zero by the worker rather than substituted: a zero
    /// window matures silence the instant an act is sent, recording a decision
    /// nobody had the chance to make. Fourteen days is the plan's own example
    /// — *"an accelerator that has not replied in two weeks has decided"* — and
    /// it is a starting point an operator is expected to set.
    #[serde(default = "default_outcome_maturity_window_days")]
    pub default_window_days: i64,
    /// Per-variant window overrides. An accelerator's two weeks and a support
    /// ticket's two hours are not the same judgement.
    #[serde(default)]
    pub variant_window_days: Vec<VariantWindowConfig>,
    /// Exact scopes whose outward acts are swept, and the cohorts declared in
    /// each. Explicit ownership, in the shape `delivery_hygiene.scopes` already
    /// uses.
    ///
    /// An enabled sweep naming no scope refuses to start rather than ticking
    /// forever over nothing.
    #[serde(default)]
    pub scopes: Vec<OutcomeMaturityScopeConfig>,
}

impl Default for OutcomeMaturityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paused: false,
            tick_interval_secs: default_outcome_maturity_tick_interval_secs(),
            default_window_days: default_outcome_maturity_window_days(),
            variant_window_days: Vec::new(),
            scopes: Vec::new(),
        }
    }
}

impl OutcomeMaturityConfig {
    /// The worker's own view of this configuration.
    ///
    /// The scopes and cohort declarations are not part of it: they are the
    /// **book's** input, and the worker deliberately knows nothing about where
    /// its work comes from.
    pub fn sweep_config(&self) -> OutcomeMaturitySweepConfig {
        OutcomeMaturitySweepConfig {
            enabled: self.enabled,
            paused: self.paused,
            tick_interval_secs: self.tick_interval_secs,
            default_window_days: self.default_window_days,
            variant_window_days: self
                .variant_window_days
                .iter()
                .map(|held| (held.variant_ref.clone(), held.days))
                .collect(),
        }
    }
}

/// Crate-neutral maturity worker settings. The learning satellite converts
/// this DTO into its policy-bearing worker type; core configuration therefore
/// never depends back on the extracted learning engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeMaturitySweepConfig {
    pub enabled: bool,
    pub paused: bool,
    pub tick_interval_secs: u64,
    pub default_window_days: i64,
    pub variant_window_days: Vec<(String, i64)>,
}

/// One variant's waiting window, in days.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VariantWindowConfig {
    pub variant_ref: String,
    pub days: i64,
}

/// One tenant swept, and the cohorts declared inside it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutcomeMaturityScopeConfig {
    pub principal: String,
    pub workspace: String,
    /// Which exact payloads belong to which variant version.
    ///
    /// Empty is legitimate and is **not** silently fine: the sweep will find
    /// this scope's acts, bind none of them, and report `degraded` with the
    /// count — which is the honest reading of "we can see the work and nobody
    /// has said what any of it is".
    #[serde(default)]
    pub variants: Vec<CohortVariantConfig>,
}

/// *"These exact payloads are version X of variant Y."*
///
/// The one human judgement the record cannot supply. Nothing here names a
/// domain: a support macro, a supplier chaser and a pitch declare the same
/// three fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CohortVariantConfig {
    /// Which answer, framing or template this is.
    pub variant_ref: String,
    /// The cohort key: which version of that variant was live.
    pub variant_version: String,
    /// The exact payload artifact references performed under it.
    pub payloads: Vec<String>,
    /// What was true about the situation that is not the thing being tested —
    /// warm versus cold, an introducer, a stage. §3 of the plan: without these
    /// a small sample confidently attributes an introducer's effect to a
    /// subject line.
    #[serde(default)]
    pub confounders: Vec<OutcomeConfounderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutcomeConfounderConfig {
    pub kind: String,
    pub value: String,
}

/// Configuration-only form of the outcome proposal worker. The learning
/// satellite owns evidence-floor validation and runtime health.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OutcomeProposalSettings {
    pub enabled: bool,
    pub paused: bool,
    pub tick_interval_secs: u64,
    pub minimum_usable: usize,
    pub minimum_counterparties: usize,
    pub candidate_type: crate::magician_v2::learning::LearningCandidateType,
}

impl Default for OutcomeProposalSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
            tick_interval_secs: 3_600,
            minimum_usable: 5,
            minimum_counterparties: 2,
            candidate_type: crate::magician_v2::learning::LearningCandidateType::WorkflowTemplate,
        }
    }
}

fn default_outcome_maturity_tick_interval_secs() -> u64 {
    3600
}

fn default_outcome_maturity_window_days() -> i64 {
    14
}

fn default_silence_watch_enabled() -> bool {
    true
}

fn default_silence_grace_hours() -> i64 {
    24
}

fn default_silence_alert_overdue() -> usize {
    1
}

fn default_silence_named_acts() -> usize {
    20
}

fn default_social_scopes() -> Vec<SocialScopeConfig> {
    vec![SocialScopeConfig {
        principal: "anonymous".to_string(),
        workspace: "default".to_string(),
    }]
}

fn default_social_tick_interval_secs() -> u64 {
    300
}

fn default_social_cooldown_secs() -> u64 {
    3_600
}

fn default_social_max_agents_per_tick() -> usize {
    4
}

fn default_social_daily_tokens() -> u64 {
    2_000
}

fn default_social_gate_reserve_tokens() -> u64 {
    512
}

fn default_social_compose_reserve_tokens() -> u64 {
    1_024
}

fn default_social_max_post_chars() -> usize {
    2_000
}

fn default_social_retention_days() -> u32 {
    90
}

fn default_social_max_posts_per_scope() -> usize {
    10_000
}

fn default_social_max_spend_log_rows() -> usize {
    50_000
}

/// Derived from the one table of user-memory tiers rather than restated, so a
/// tier added there cannot go missing here. A hand-written copy of this list
/// already drifted once: it omitted `routines`.
fn default_resurfacing_memory_tiers() -> Vec<String> {
    crate::magician_v2::chat::service::owner_facing_user_memory_tiers()
}

fn default_resurfacing_recommendation_min_confidence() -> f64 {
    0.65
}

fn default_resurfacing_action_result_cooldown_days() -> u32 {
    60
}

fn default_resurfacing_active_repair_batch_size() -> usize {
    10
}

fn default_resurfacing_surface_cap() -> usize {
    5
}

fn default_resurfacing_centrality_reference_timeout_secs() -> u64 {
    90
}

fn default_resurfacing_centrality_query_timeout_secs() -> u64 {
    15
}

fn default_resurfacing_centrality_reference_batch_size() -> usize {
    8
}

/// Enrollment configuration for consumer channel identity resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentConfig {
    /// Auto-approve enrollments into the configured default principal for the
    /// requested workspace when a separate approval step is not required.
    #[serde(default = "default_true")]
    pub auto_approve: bool,
    /// Principal assigned on auto-approve. Since 2026-08-30 this is the
    /// **fallback** for unauthenticated callers only (open-mode bootstrap,
    /// before the first identity exists): an authenticated session's own
    /// principal is assigned instead, so a family member's channel maps
    /// into the member's scope rather than the owner's.
    #[serde(default = "default_principal_str")]
    pub default_principal: String,
    /// How long pending enrollments last before expiry (hours).
    #[serde(default = "default_pending_ttl_hours")]
    pub pending_ttl_hours: u64,
}

impl Default for EnrollmentConfig {
    fn default() -> Self {
        Self {
            auto_approve: default_true(),
            default_principal: default_principal_str(),
            pending_ttl_hours: default_pending_ttl_hours(),
        }
    }
}

fn default_principal_str() -> String {
    "default".to_string()
}

fn default_pending_ttl_hours() -> u64 {
    24
}

/// Authentication posture — `magician_v2::auth`. See
/// `docs/components/magician/auth.md` and the workspace design §10.
///
/// `open` permits requests without credentials and assigns them the local
/// anonymous/default scope. `credentials` requires a valid workspace-bound
/// bearer on every authenticated route and 401s without one. Caller-supplied
/// scope selectors are never authority in either mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[serde(rename = "open")]
    #[default]
    Open,
    #[serde(rename = "credentials")]
    Credentials,
}

impl AuthMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMode::Open => "open",
            AuthMode::Credentials => "credentials",
        }
    }
}

use crate::magician_v2::auth::credentials::Provider;

/// One social provider's client registration. Secrets come from the config
/// file or — preferred in live surfaces — from that provider's own
/// `MAGICIAN_AUTH_{GOOGLE|GITHUB}_CLIENT_SECRET` env var; empty means the
/// provider is not configured and its routes answer 503.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AuthProviderConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}

impl AuthProviderConfig {
    pub fn is_configured(&self, provider: Provider) -> bool {
        !self.client_id.trim().is_empty() && !self.effective_secret(provider).is_empty()
    }

    /// Config value with env-var fallback (live surfaces keep secrets out
    /// of the config file). The fallback is provider-scoped: a provider
    /// reads only its own `MAGICIAN_AUTH_{PROVIDER}_CLIENT_SECRET` var —
    /// a cross-provider fallback would POST one provider's secret to the
    /// other's token endpoint.
    pub fn effective_secret(&self, provider: Provider) -> String {
        if !self.client_secret.trim().is_empty() {
            return self.client_secret.trim().to_string();
        }
        let key = match provider {
            Provider::Google => "MAGICIAN_AUTH_GOOGLE_CLIENT_SECRET",
            Provider::Github => "MAGICIAN_AUTH_GITHUB_CLIENT_SECRET",
        };
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return value.trim().to_string();
            }
        }
        String::new()
    }
}

/// Auth configuration section (`auth:` in magician-config.yaml).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// `open` (default) = today's behaviour bit-identical. `credentials` =
    /// every authenticated route requires a valid bearer.
    #[serde(default)]
    pub mode: AuthMode,
    /// Whether new identities may be created (first login / social signup).
    #[serde(default = "default_true")]
    pub allow_signup: bool,
    /// Login session lifetime in days (workspace design §2.4).
    #[serde(default = "default_session_ttl_days")]
    pub session_ttl_days: u64,
    /// Social provider client registrations.
    #[serde(default)]
    pub providers: AuthProvidersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AuthProvidersConfig {
    #[serde(default)]
    pub google: AuthProviderConfig,
    #[serde(default)]
    pub github: AuthProviderConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::Open,
            allow_signup: default_true(),
            session_ttl_days: default_session_ttl_days(),
            providers: AuthProvidersConfig::default(),
        }
    }
}

fn default_session_ttl_days() -> u64 {
    30
}

/// Outward-action policy — readiness review §9 steps 2 and 4.
///
/// Governs dispatches that send something OUT of the owner's control: email,
/// chat messages, calendar invitations. Classification of WHICH dispatches those
/// are lives in `agents::outward_actions` and is central rather than per-agent,
/// because a per-agent list is true only for the agents someone remembered to
/// edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutwardActionsConfig {
    /// When true (the default), an outward dispatch is RECORDED and not
    /// performed.
    ///
    /// The default is deliberate and is the point of §9 step 4: the rest of the
    /// system can be rehearsed end to end while nothing actually reaches a third
    /// party. An operator turns sending on once, knowingly. Nobody turns it on
    /// by forgetting to set a flag — which is the failure mode a default of
    /// `false` would have.
    #[serde(default = "default_outward_capture_only")]
    pub capture_only: bool,
}

impl Default for OutwardActionsConfig {
    fn default() -> Self {
        Self {
            capture_only: default_outward_capture_only(),
        }
    }
}

fn default_outward_capture_only() -> bool {
    true
}

/// Approval-envelope posture — `docs/plans/2026-08-07-opc-approval-envelopes.md`.
///
/// Governs whether pre-authorisation is resolved at dispatch at all. Sibling of
/// [`OutwardActionsConfig`] and deliberately shaped like it: the classification
/// of WHICH acts are gated lives in `magician_v2::approval_envelopes` and is
/// central, because a per-agent list is true only for the agents someone
/// remembered to edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalEnvelopesConfig {
    /// `off` (the default), `shadow`, or `enforcing`.
    ///
    /// The default is deliberate, and is the same argument as
    /// `outward_actions.capture_only`, inverted: an envelope system that
    /// switched itself on would be the opposite of consent. `shadow` resolves
    /// and logs what WOULD have been covered while still asking for everything;
    /// only `enforcing` lets a covered act proceed without asking. An operator
    /// moves off `off` once, knowingly.
    ///
    /// A string rather than an enum because the posture is
    /// `approval_envelopes::EnvelopeMode`, which carries no serde derives — and
    /// keeping the parse explicit is what lets an unrecognised value fail closed
    /// to `off` instead of failing the whole config load.
    #[serde(default = "default_approval_envelope_mode")]
    pub mode: String,
}

impl Default for ApprovalEnvelopesConfig {
    fn default() -> Self {
        Self {
            mode: default_approval_envelope_mode(),
        }
    }
}

fn default_approval_envelope_mode() -> String {
    "off".to_string()
}

impl ApprovalEnvelopesConfig {
    /// The posture this configuration names.
    ///
    /// Fail-closed: a value naming no posture — a typo, an old spelling, a mode
    /// from a newer binary — reads as `Off`, never as the nearest match. An
    /// unreadable posture is not permission.
    pub fn mode(&self) -> crate::magician_v2::approval_envelopes::EnvelopeMode {
        crate::magician_v2::approval_envelopes::parse_envelope_mode(&self.mode)
            .unwrap_or(crate::magician_v2::approval_envelopes::EnvelopeMode::Off)
    }
}

/// Recipient-compliance posture —
/// `docs/plans/2026-08-07-opc-readiness-review.md` §9B.
///
/// Governs whether the four below-the-agent recipient checks —
/// `magician_v2::recipient_compliance` — decide anything on the outward
/// dispatch path. Sibling of [`ApprovalEnvelopesConfig`] and deliberately
/// shaped like it, down to the string-and-parse: an operator reading one key
/// should not have to learn a second convention to read the other.
///
/// The gate is reachable **whatever this key says** through the owner-facing
/// route in `magician-api/src/recipient_compliance_api.rs`. This key governs
/// only whether a refusal stops a send.
///
/// `deny_unknown_fields` because the fallback direction is dangerous here: a
/// misspelled key inside a section an operator deliberately wrote would be
/// ignored in silence and `mode` would stay `off`, so somebody who meant to
/// turn the gate ON would ship with it off and no signal that they had. Refusing
/// to boot is the loud answer, and it is safe on this key specifically because
/// the key is new — nothing already deployed carries a field it would reject.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientComplianceConfig {
    /// `off` (the default) or `blocking`.
    ///
    /// The default is deliberate, and is the same argument as
    /// `approval_envelopes.mode`, pointed the other way. That gate *permits*;
    /// this one **refuses**, and its four rules fail closed on every register
    /// they cannot read — which is exactly the shape that stops every outward
    /// send in a fleet if it arrives switched on. An operator moves off `off`
    /// once, knowingly.
    ///
    /// There is deliberately no `shadow` here. Envelopes carry one because a
    /// resolver can be observed without deciding; this gate has no such
    /// halfway house today, and a value an operator could write that the
    /// dispatch path silently reads as `off` would be a key that lies.
    ///
    /// A string rather than an enum for the reason the sibling gives: the
    /// posture is `recipient_compliance::RecipientComplianceMode`, which
    /// carries no serde derives, and keeping the parse explicit is what lets an
    /// unrecognised value degrade to `off` instead of failing the config load.
    #[serde(default = "default_recipient_compliance_mode")]
    pub mode: String,
}

impl Default for RecipientComplianceConfig {
    fn default() -> Self {
        Self {
            mode: default_recipient_compliance_mode(),
        }
    }
}

fn default_recipient_compliance_mode() -> String {
    "off".to_string()
}

impl RecipientComplianceConfig {
    /// The posture this configuration names.
    ///
    /// Fail-closed in this gate's own direction: a value naming no posture — a
    /// typo, an old spelling, a mode from a newer binary — reads as `Off`,
    /// never as the nearest match. Guessing `blocking` from `block` would start
    /// refusing outward sends on the strength of a misspelling.
    pub fn mode(&self) -> crate::magician_v2::recipient_compliance::RecipientComplianceMode {
        crate::magician_v2::recipient_compliance::parse_recipient_compliance_mode(&self.mode)
            .unwrap_or(crate::magician_v2::recipient_compliance::RecipientComplianceMode::Off)
    }
}

/// Envoy routing configuration — "Presto in envoy mode".
///
/// Identifies which (channel, address) pairs are the OWNER (full-trust,
/// Presto treatment) versus guests, who get bound to a least-privilege
/// envoy agent. See `docs/components/magician/envoy-agent.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvoyConfig {
    /// channel_type -> owner addresses that get full-trust (Presto) treatment.
    #[serde(default)]
    pub owner_identities: HashMap<String, Vec<String>>,
    /// channel_type -> env var containing owner addresses.
    ///
    /// Values are parsed as comma, semicolon, or newline separated lists and are
    /// merged with `owner_identities` at runtime. Use this for real phone numbers
    /// and email addresses so tracked config can hold only env var names.
    #[serde(default)]
    pub owner_identity_envs: HashMap<String, String>,
    /// Agent bound to guest threads. Default "envoy".
    #[serde(default = "default_envoy_agent_id")]
    pub envoy_agent_id: String,
    /// Whether a resolved engagement lane is allowed to change where an inbound
    /// message actually lands.
    ///
    /// **Default off, and off is a no-op.** Phase 2 of
    /// `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §6 makes
    /// the lanes *nameable* — an inbound from a proved counterparty identity
    /// resolves to
    /// [`crate::magician_v2::chat::envoy::InboundLane::Engagement`], which can
    /// be logged and reviewed — while every message still routes exactly where
    /// it routed before the enum existed. With this false, an engagement lane
    /// projects to the guest pair verbatim.
    ///
    /// Turning it on is Phase 3 and a separate, deliberate act: it is the point
    /// at which an inbound message reaches an agent holding an engagement's
    /// authority, so it must never become true as a side effect of a default.
    #[serde(default)]
    pub engagement_forwarding_enabled: bool,
}

impl EnvoyConfig {
    pub fn owner_identities_for(&self, channel_type: &str) -> Vec<String> {
        let mut identities = Vec::new();
        let mut seen = HashSet::new();

        if let Some(configured) = self.owner_identities.get(channel_type) {
            for identity in configured {
                push_owner_identity(&mut identities, &mut seen, identity);
            }
        }

        if let Some(env_name) = self.owner_identity_envs.get(channel_type) {
            if let Ok(raw) = std::env::var(env_name) {
                for identity in split_owner_identity_env_value(&raw) {
                    push_owner_identity(&mut identities, &mut seen, &identity);
                }
            }
        }

        identities
    }

    pub fn has_owner_identity(&self, channel_type: &str, address: &str) -> bool {
        self.owner_identities_for(channel_type)
            .iter()
            .any(|identity| identity == address)
    }
}

fn split_owner_identity_env_value(raw: &str) -> Vec<String> {
    raw.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|identity| !identity.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn push_owner_identity(identities: &mut Vec<String>, seen: &mut HashSet<String>, identity: &str) {
    let trimmed = identity.trim();
    if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
        identities.push(trimmed.to_string());
    }
}

impl Default for EnvoyConfig {
    fn default() -> Self {
        Self {
            owner_identities: HashMap::new(),
            owner_identity_envs: HashMap::new(),
            envoy_agent_id: default_envoy_agent_id(),
            engagement_forwarding_enabled: false,
        }
    }
}

fn default_envoy_agent_id() -> String {
    "envoy".to_string()
}

/// Autonomous harness ("company loop") configuration — the CTO detect-and-fix
/// loop that turns detected cycle anomalies into review-gated fix proposals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    /// Autofix: when the harness detects a cycle anomaly, auto-draft a fix as a
    /// review-gated coding proposal. Default true (opt-out). Env
    /// MAGICIAN_HARNESS_AUTOFIX (0/1/true/false/on/off) overrides this.
    #[serde(default = "default_autofix_enabled")]
    pub autofix_enabled: bool,
    /// Agent the autofix task is assigned to. MUST directly grant
    /// `run_coding_task` (i.e. a coding engineer, not a router like `cto` whose
    /// persona forbids writing code — that only works via a delegation hop).
    /// Env MAGICIAN_HARNESS_AUTOFIX_AGENT overrides this.
    #[serde(default = "default_autofix_agent")]
    pub autofix_agent: String,
    /// Global company-loop kill-switch. When true, scheduled and manual harness
    /// starts plus steward/autofix dispatches are blocked. The control API also
    /// clears queued harness work and requests active-cycle cancellation. Default
    /// false. Env `MAGICIAN_HARNESS_PAUSED` (`1/true/on` -> pause,
    /// `0/false/off` -> resume) overrides this as an emergency control. Reloaded
    /// per admission/tick, so a config-file edit toggles it live without restart.
    #[serde(default)]
    pub paused: bool,
}

/// Magician plane MCP catalog — two hot lists, because a spawned-bare harness
/// and an operator terminal want opposite things. Empty `hot` on a profile
/// means "use the built-in list in `execution::plane::catalog`"; a non-empty
/// list replaces it so adding a control tool does not require a Rust edit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PlaneConfig {
    #[serde(default)]
    pub spawned_bare: PlaneProfileConfig,
    #[serde(default)]
    pub terminal: PlaneProfileConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PlaneProfileConfig {
    /// Tool names advertised on `tools/list`. Empty = the crate's built-in
    /// profile list.
    #[serde(default)]
    pub hot: Vec<String>,
}

fn default_autofix_enabled() -> bool {
    true
}

fn default_autofix_agent() -> String {
    "senior-software-developer".to_string()
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            autofix_enabled: default_autofix_enabled(),
            autofix_agent: default_autofix_agent(),
            paused: false,
        }
    }
}

/// Runtime configuration for a managed consumer-channel bot process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotProcessConfig {
    /// Whether the bot should be started automatically with magician.
    #[serde(default)]
    pub enabled: bool,
    /// Executable to launch for the bot process.
    pub command: String,
    /// Command-line arguments for the bot process.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables passed to the bot process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional working directory for the bot process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether the bot should be restarted automatically after unexpected exit.
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    /// Maximum restart backoff after repeated failures.
    #[serde(default = "default_bot_restart_max_backoff_secs")]
    pub restart_max_backoff_secs: u64,
}

impl Default for BotProcessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            auto_restart: default_true(),
            restart_max_backoff_secs: default_bot_restart_max_backoff_secs(),
        }
    }
}

/// Tool authorization policy for unlisted tools.
///
/// Controls whether the executor should allow, deny, or ask the user
/// before dispatching tools that are not in an explicit allowlist or blocklist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAuthorizationConfig {
    /// Tools explicitly allowed (always execute, no prompt).
    /// When empty, all tools are allowed (minus blocklist).
    /// When non-empty, tools not in this list are "unlisted" and subject to `unlisted_policy`.
    #[serde(default)]
    pub allowlist: Vec<String>,

    /// Tools explicitly blocked (always rejected, no prompt).
    #[serde(default)]
    pub blocklist: Vec<String>,

    /// What to do when a tool is not in the allowlist or blocklist:
    /// "ask" (default when allowlist has entries), or "deny".
    /// Ignored when allowlist is empty (everything allowed minus blocklist).
    #[serde(default = "default_tool_authorization_policy")]
    pub unlisted_policy: String,
}

impl Default for ToolAuthorizationConfig {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            blocklist: Vec::new(),
            unlisted_policy: default_tool_authorization_policy(),
        }
    }
}

/// Durable task-state policy for runtime-context execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateConfig {
    /// Lazy optional state is the only runtime-context policy.
    #[serde(default = "default_task_state_policy")]
    pub policy: String,
    /// Boundaries where the outer model may decide to create/patch/close state.
    #[serde(default = "default_task_state_decision_points")]
    pub decision_points: Vec<String>,
    /// Durable structured revision policy.
    #[serde(default)]
    pub revisions: TaskStateRevisionConfig,
    /// Helper operation names for structured durable-state work.
    #[serde(default)]
    pub helpers: TaskStateHelperOperationsConfig,
}

impl Default for TaskStateConfig {
    fn default() -> Self {
        Self {
            policy: default_task_state_policy(),
            decision_points: default_task_state_decision_points(),
            revisions: TaskStateRevisionConfig::default(),
            helpers: TaskStateHelperOperationsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateRevisionConfig {
    #[serde(default = "default_task_state_revision_format")]
    pub format: String,
    #[serde(default)]
    pub bump_on_runtime_ledger_change: bool,
    #[serde(default)]
    pub bump_on_execution_history_change: bool,
    #[serde(default)]
    pub bump_on_artifact_change: bool,
    #[serde(default)]
    pub bump_on_prompt_projection_change: bool,
    #[serde(default = "default_task_state_allowed_actions")]
    pub allowed_actions: Vec<String>,
}

impl Default for TaskStateRevisionConfig {
    fn default() -> Self {
        Self {
            format: default_task_state_revision_format(),
            bump_on_runtime_ledger_change: false,
            bump_on_execution_history_change: false,
            bump_on_artifact_change: false,
            bump_on_prompt_projection_change: false,
            allowed_actions: default_task_state_allowed_actions(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateHelperOperationsConfig {
    #[serde(default = "default_durable_task_state_generate_operation")]
    pub generate_operation: String,
    #[serde(default = "default_durable_task_state_patch_operation")]
    pub patch_operation: String,
    #[serde(default = "default_durable_task_state_close_summary_operation")]
    pub close_summary_operation: String,
}

impl Default for TaskStateHelperOperationsConfig {
    fn default() -> Self {
        Self {
            generate_operation: default_durable_task_state_generate_operation(),
            patch_operation: default_durable_task_state_patch_operation(),
            close_summary_operation: default_durable_task_state_close_summary_operation(),
        }
    }
}

/// Admission, budget, onboarding, and enrichment policy for public chat
/// surfaces that can receive untrusted/high-volume traffic.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PublicChatConfig {
    /// Public envoy chat policy for bot surfaces such as Kapso and Telegram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kapso_envoy_chat: Option<PublicChatPolicyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatPolicyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_public_chat_kapso_source_surface")]
    pub source_surface: String,
    #[serde(default = "default_public_chat_source_surfaces")]
    pub source_surfaces: Vec<String>,
    #[serde(default = "default_public_chat_paid_operation")]
    pub paid_operation: String,
    #[serde(default = "default_public_chat_fallback_operation")]
    pub fallback_operation: String,
    #[serde(default = "default_public_chat_max_paid_concurrent")]
    pub max_paid_concurrent: usize,
    #[serde(default = "default_public_chat_max_fallback_concurrent")]
    pub max_fallback_concurrent: usize,
    #[serde(default = "default_public_chat_max_global_queue_depth")]
    pub max_global_queue_depth: usize,
    #[serde(default = "default_public_chat_max_per_sender_queue_depth")]
    pub max_per_sender_queue_depth: usize,
    #[serde(default = "default_public_chat_coalesce_window_ms")]
    pub coalesce_window_ms: u64,
    #[serde(default = "default_public_chat_max_queue_wait_ms")]
    pub max_queue_wait_ms: u64,
    #[serde(default)]
    pub daily_paid_limit: PublicChatDailyPaidLimitConfig,
    #[serde(default = "default_public_chat_queued_reply")]
    pub queued_reply: String,
    #[serde(default = "default_public_chat_overload_reply")]
    pub overload_reply: String,
    #[serde(default = "default_public_chat_daily_fallback_reply")]
    pub daily_fallback_reply: String,
    #[serde(default)]
    pub outside_window_template_policy: PublicChatOutsideWindowTemplatePolicyConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_contact: Option<PublicChatFirstContactPolicyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_research: Option<PublicChatIdentityResearchPolicyConfig>,
}

impl Default for PublicChatPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source_surface: default_public_chat_kapso_source_surface(),
            source_surfaces: default_public_chat_source_surfaces(),
            paid_operation: default_public_chat_paid_operation(),
            fallback_operation: default_public_chat_fallback_operation(),
            max_paid_concurrent: default_public_chat_max_paid_concurrent(),
            max_fallback_concurrent: default_public_chat_max_fallback_concurrent(),
            max_global_queue_depth: default_public_chat_max_global_queue_depth(),
            max_per_sender_queue_depth: default_public_chat_max_per_sender_queue_depth(),
            coalesce_window_ms: default_public_chat_coalesce_window_ms(),
            max_queue_wait_ms: default_public_chat_max_queue_wait_ms(),
            daily_paid_limit: PublicChatDailyPaidLimitConfig::default(),
            queued_reply: default_public_chat_queued_reply(),
            overload_reply: default_public_chat_overload_reply(),
            daily_fallback_reply: default_public_chat_daily_fallback_reply(),
            outside_window_template_policy: PublicChatOutsideWindowTemplatePolicyConfig::default(),
            first_contact: None,
            identity_research: None,
        }
    }
}

impl PublicChatPolicyConfig {
    pub fn source_surface_matches(&self, source_surface: &str) -> bool {
        let needle = source_surface.trim();
        if needle.is_empty() {
            return false;
        }
        self.source_surfaces
            .iter()
            .map(|surface| surface.trim())
            .any(|surface| !surface.is_empty() && surface == needle)
            || self.source_surface.trim() == needle
    }

    pub fn effective_source_surfaces(&self) -> Vec<&str> {
        let mut surfaces = Vec::new();
        let legacy = self.source_surface.trim();
        if !legacy.is_empty() {
            surfaces.push(legacy);
        }
        for surface in &self.source_surfaces {
            let surface = surface.trim();
            if !surface.is_empty() && !surfaces.contains(&surface) {
                surfaces.push(surface);
            }
        }
        surfaces
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatDailyPaidLimitConfig {
    #[serde(default = "default_public_chat_daily_max_calls")]
    pub max_calls: u64,
    #[serde(default = "default_public_chat_daily_max_cost_usd")]
    pub max_cost_usd: f64,
    #[serde(default = "default_public_chat_daily_max_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_public_chat_reset_timezone")]
    pub reset_timezone: String,
}

impl Default for PublicChatDailyPaidLimitConfig {
    fn default() -> Self {
        Self {
            max_calls: default_public_chat_daily_max_calls(),
            max_cost_usd: default_public_chat_daily_max_cost_usd(),
            max_tokens: default_public_chat_daily_max_tokens(),
            reset_timezone: default_public_chat_reset_timezone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatOutsideWindowTemplatePolicyConfig {
    #[serde(default = "default_public_chat_template_policy_allow")]
    pub queued_and_overload: PublicChatTemplatePolicy,
    #[serde(default)]
    pub normal_llm_reply: PublicChatTemplatePolicy,
}

impl Default for PublicChatOutsideWindowTemplatePolicyConfig {
    fn default() -> Self {
        Self {
            queued_and_overload: PublicChatTemplatePolicy::Allow,
            normal_llm_reply: PublicChatTemplatePolicy::Suppress,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PublicChatTemplatePolicy {
    Allow,
    #[default]
    Suppress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatFirstContactPolicyConfig {
    #[serde(default = "default_true")]
    pub llm_enabled: bool,
    #[serde(default = "default_public_chat_paid_operation")]
    pub operation: String,
    #[serde(default = "default_true")]
    pub ask_for_identity: bool,
    #[serde(default = "default_true")]
    pub ask_for_purpose: bool,
    #[serde(default = "default_public_chat_first_contact_debounce_window_ms")]
    pub debounce_window_ms: u64,
    #[serde(default = "default_public_chat_first_contact_max_debounce_wait_ms")]
    pub max_debounce_wait_ms: u64,
    #[serde(default = "default_public_chat_first_contact_min_response_delay_ms")]
    pub min_response_delay_ms: u64,
    #[serde(default = "default_public_chat_first_contact_max_paid_concurrent")]
    pub max_paid_concurrent: usize,
    #[serde(default = "default_public_chat_first_contact_max_per_sender_per_day")]
    pub max_per_sender_per_day: usize,
    #[serde(default = "default_public_chat_first_contact_prompt")]
    pub prompt: String,
    #[serde(default = "default_public_chat_first_contact_conversation_style")]
    pub conversation_style: String,
}

impl Default for PublicChatFirstContactPolicyConfig {
    fn default() -> Self {
        Self {
            llm_enabled: true,
            operation: default_public_chat_paid_operation(),
            ask_for_identity: true,
            ask_for_purpose: true,
            debounce_window_ms: default_public_chat_first_contact_debounce_window_ms(),
            max_debounce_wait_ms: default_public_chat_first_contact_max_debounce_wait_ms(),
            min_response_delay_ms: default_public_chat_first_contact_min_response_delay_ms(),
            max_paid_concurrent: default_public_chat_first_contact_max_paid_concurrent(),
            max_per_sender_per_day: default_public_chat_first_contact_max_per_sender_per_day(),
            prompt: default_public_chat_first_contact_prompt(),
            conversation_style: default_public_chat_first_contact_conversation_style(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatIdentityResearchPolicyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_public_chat_identity_research_worker_agent_id")]
    pub worker_agent_id: String,
    #[serde(default)]
    pub trigger: PublicChatIdentityResearchTrigger,
    #[serde(default)]
    pub min_details: PublicChatIdentityResearchMinDetailsConfig,
    #[serde(default = "default_public_chat_identity_research_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "default_public_chat_identity_research_max_global_queue_depth")]
    pub max_global_queue_depth: usize,
    #[serde(default = "default_public_chat_identity_research_max_per_sender_queue_depth")]
    pub max_per_sender_queue_depth: usize,
    #[serde(default = "default_public_chat_identity_research_max_per_sender_per_day")]
    pub max_per_sender_per_day: usize,
    #[serde(default)]
    pub daily_limit: PublicChatIdentityResearchDailyLimitConfig,
    #[serde(default = "default_public_chat_identity_research_coalesce_window_ms")]
    pub coalesce_window_ms: u64,
    #[serde(default = "default_true")]
    pub owner_review_priority: bool,
    #[serde(default)]
    pub expose_research_to_user: bool,
}

impl Default for PublicChatIdentityResearchPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            worker_agent_id: default_public_chat_identity_research_worker_agent_id(),
            trigger: PublicChatIdentityResearchTrigger::default(),
            min_details: PublicChatIdentityResearchMinDetailsConfig::default(),
            max_concurrent: default_public_chat_identity_research_max_concurrent(),
            max_global_queue_depth: default_public_chat_identity_research_max_global_queue_depth(),
            max_per_sender_queue_depth:
                default_public_chat_identity_research_max_per_sender_queue_depth(),
            max_per_sender_per_day: default_public_chat_identity_research_max_per_sender_per_day(),
            daily_limit: PublicChatIdentityResearchDailyLimitConfig::default(),
            coalesce_window_ms: default_public_chat_identity_research_coalesce_window_ms(),
            owner_review_priority: true,
            expose_research_to_user: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PublicChatIdentityResearchTrigger {
    #[default]
    AfterIdentityDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatIdentityResearchMinDetailsConfig {
    #[serde(default = "default_true")]
    pub claimed_name_or_org: bool,
    #[serde(default = "default_true")]
    pub purpose: bool,
}

impl Default for PublicChatIdentityResearchMinDetailsConfig {
    fn default() -> Self {
        Self {
            claimed_name_or_org: true,
            purpose: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicChatIdentityResearchDailyLimitConfig {
    #[serde(default = "default_public_chat_identity_research_daily_max_jobs")]
    pub max_jobs: u64,
    #[serde(default = "default_public_chat_identity_research_daily_max_cost_usd")]
    pub max_cost_usd: f64,
    #[serde(default = "default_public_chat_reset_timezone")]
    pub reset_timezone: String,
}

impl Default for PublicChatIdentityResearchDailyLimitConfig {
    fn default() -> Self {
        Self {
            max_jobs: default_public_chat_identity_research_daily_max_jobs(),
            max_cost_usd: default_public_chat_identity_research_daily_max_cost_usd(),
            reset_timezone: default_public_chat_reset_timezone(),
        }
    }
}

fn default_public_chat_kapso_source_surface() -> String {
    "kapso-envoy-chat".to_string()
}

fn default_public_chat_source_surfaces() -> Vec<String> {
    vec!["telegram-envoy-chat".to_string()]
}

fn default_public_chat_paid_operation() -> String {
    "kapso_envoy_chat".to_string()
}

fn default_public_chat_fallback_operation() -> String {
    "kapso_envoy_chat_fallback".to_string()
}

fn default_public_chat_max_paid_concurrent() -> usize {
    2
}

fn default_public_chat_max_fallback_concurrent() -> usize {
    4
}

fn default_public_chat_max_global_queue_depth() -> usize {
    200
}

fn default_public_chat_max_per_sender_queue_depth() -> usize {
    3
}

fn default_public_chat_coalesce_window_ms() -> u64 {
    8_000
}

fn default_public_chat_max_queue_wait_ms() -> u64 {
    45_000
}

fn default_public_chat_daily_max_calls() -> u64 {
    1_000
}

fn default_public_chat_daily_max_cost_usd() -> f64 {
    5.0
}

fn default_public_chat_daily_max_tokens() -> u64 {
    1_000_000
}

fn default_public_chat_reset_timezone() -> String {
    "Asia/Kolkata".to_string()
}

fn default_public_chat_queued_reply() -> String {
    "I got your message. I am a bit busy and will reply shortly.".to_string()
}

fn default_public_chat_overload_reply() -> String {
    "I am receiving a lot of messages right now. Please try again shortly.".to_string()
}

fn default_public_chat_daily_fallback_reply() -> String {
    "I am switching to a lower-cost mode for today, but I can still chat.".to_string()
}

fn default_public_chat_template_policy_allow() -> PublicChatTemplatePolicy {
    PublicChatTemplatePolicy::Allow
}

fn default_public_chat_first_contact_debounce_window_ms() -> u64 {
    7_000
}

fn default_public_chat_first_contact_max_debounce_wait_ms() -> u64 {
    18_000
}

fn default_public_chat_first_contact_min_response_delay_ms() -> u64 {
    1_200
}

fn default_public_chat_first_contact_max_paid_concurrent() -> usize {
    1
}

fn default_public_chat_first_contact_max_per_sender_per_day() -> usize {
    2
}

fn default_public_chat_first_contact_prompt() -> String {
    "Introduce yourself as the owner's AI partner when identity is relevant. Ask who they are only if identity is not already clear, and ask what they would like help with only if purpose is not already clear. Do not start work unless they use /magic. Legacy @magic also works."
        .to_string()
}

fn default_public_chat_first_contact_conversation_style() -> String {
    "Sound warm, concise, and human. Progress naturally from what they said. Ask at most one lightweight follow-up question; do not use checklist-style identity questions. If identity or purpose is already clear, respond to that instead of asking again."
        .to_string()
}

fn default_public_chat_identity_research_worker_agent_id() -> String {
    "web-researcher".to_string()
}

fn default_public_chat_identity_research_max_concurrent() -> usize {
    1
}

fn default_public_chat_identity_research_max_global_queue_depth() -> usize {
    100
}

fn default_public_chat_identity_research_max_per_sender_queue_depth() -> usize {
    1
}

fn default_public_chat_identity_research_max_per_sender_per_day() -> usize {
    1
}

fn default_public_chat_identity_research_daily_max_jobs() -> u64 {
    50
}

fn default_public_chat_identity_research_daily_max_cost_usd() -> f64 {
    2.0
}

fn default_public_chat_identity_research_coalesce_window_ms() -> u64 {
    60_000
}

fn default_bot_restart_max_backoff_secs() -> u64 {
    30
}

fn default_tool_authorization_policy() -> String {
    "ask".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceRuntimeCacheConfig {
    #[serde(default = "default_surface_schema_index_limit")]
    pub max_schema_indexes: usize,
    #[serde(default = "default_surface_plan_limit")]
    pub max_surface_plans: usize,
    #[serde(default = "default_surface_cache_idle_ttl_seconds")]
    pub idle_ttl_seconds: u64,
    #[serde(default = "default_true")]
    pub singleflight: bool,
}

impl Default for AgentSurfaceRuntimeCacheConfig {
    fn default() -> Self {
        Self {
            max_schema_indexes: default_surface_schema_index_limit(),
            max_surface_plans: default_surface_plan_limit(),
            idle_ttl_seconds: default_surface_cache_idle_ttl_seconds(),
            singleflight: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceWorkingSetConfig {
    #[serde(default = "default_surface_loaded_family_limit")]
    pub max_loaded_families: usize,
    #[serde(default = "default_surface_loaded_tool_limit")]
    pub max_loaded_tools: usize,
    #[serde(default = "default_surface_schema_byte_limit")]
    pub max_schema_bytes: usize,
}

impl Default for AgentSurfaceWorkingSetConfig {
    fn default() -> Self {
        Self {
            max_loaded_families: default_surface_loaded_family_limit(),
            max_loaded_tools: default_surface_loaded_tool_limit(),
            max_schema_bytes: default_surface_schema_byte_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceResultProjectionBudgetConfig {
    pub max_model_tokens: usize,
    pub max_serialized_bytes: usize,
    pub max_records: usize,
    #[serde(default = "default_result_projection_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_result_projection_max_scalar_bytes")]
    pub max_scalar_bytes: usize,
}

impl AgentSurfaceResultProjectionBudgetConfig {
    fn chat_default() -> Self {
        Self {
            max_model_tokens: 4_096,
            max_serialized_bytes: 16 * 1_024,
            max_records: 20,
            max_depth: default_result_projection_max_depth(),
            max_scalar_bytes: default_result_projection_max_scalar_bytes(),
        }
    }

    fn realtime_voice_default() -> Self {
        Self {
            max_model_tokens: 2_048,
            max_serialized_bytes: 8 * 1_024,
            max_records: 10,
            max_depth: default_result_projection_max_depth(),
            max_scalar_bytes: default_result_projection_max_scalar_bytes(),
        }
    }

    fn autonomous_task_default() -> Self {
        Self {
            max_model_tokens: 6_144,
            max_serialized_bytes: 24 * 1_024,
            // Keep the first task turn bounded. More complete records remain
            // available through the authenticated lossless continuation.
            max_records: 20,
            max_depth: default_result_projection_max_depth(),
            max_scalar_bytes: default_result_projection_max_scalar_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceResultProjectionConfig {
    #[serde(default = "AgentSurfaceResultProjectionBudgetConfig::chat_default")]
    pub chat: AgentSurfaceResultProjectionBudgetConfig,
    #[serde(default = "AgentSurfaceResultProjectionBudgetConfig::realtime_voice_default")]
    pub realtime_voice: AgentSurfaceResultProjectionBudgetConfig,
    #[serde(default = "AgentSurfaceResultProjectionBudgetConfig::autonomous_task_default")]
    pub autonomous_task: AgentSurfaceResultProjectionBudgetConfig,
}

impl Default for AgentSurfaceResultProjectionConfig {
    fn default() -> Self {
        Self {
            chat: AgentSurfaceResultProjectionBudgetConfig::chat_default(),
            realtime_voice: AgentSurfaceResultProjectionBudgetConfig::realtime_voice_default(),
            autonomous_task: AgentSurfaceResultProjectionBudgetConfig::autonomous_task_default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceContextRetrievalBudgetConfig {
    pub deadline_ms: u64,
}

impl AgentSurfaceContextRetrievalBudgetConfig {
    fn chat_default() -> Self {
        Self { deadline_ms: 3_000 }
    }

    fn realtime_voice_default() -> Self {
        Self {
            deadline_ms: default_realtime_turn_context_budget_ms(),
        }
    }

    fn autonomous_task_default() -> Self {
        // Task bootstrap is answer-critical. The staged coordinator retains
        // whichever fast-memory/hybrid/procedure branches complete inside the
        // budget; an unhealthy local index must never add multi-second latency
        // before the first agent decision or after HITL resume.
        Self { deadline_ms: 500 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceContextRetrievalConfig {
    #[serde(default = "AgentSurfaceContextRetrievalBudgetConfig::chat_default")]
    pub chat: AgentSurfaceContextRetrievalBudgetConfig,
    #[serde(default = "AgentSurfaceContextRetrievalBudgetConfig::realtime_voice_default")]
    pub realtime_voice: AgentSurfaceContextRetrievalBudgetConfig,
    #[serde(default = "AgentSurfaceContextRetrievalBudgetConfig::autonomous_task_default")]
    pub autonomous_task: AgentSurfaceContextRetrievalBudgetConfig,
}

impl Default for AgentSurfaceContextRetrievalConfig {
    fn default() -> Self {
        Self {
            chat: AgentSurfaceContextRetrievalBudgetConfig::chat_default(),
            realtime_voice: AgentSurfaceContextRetrievalBudgetConfig::realtime_voice_default(),
            autonomous_task: AgentSurfaceContextRetrievalBudgetConfig::autonomous_task_default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSurfaceRuntimeConfig {
    #[serde(default)]
    pub cache: AgentSurfaceRuntimeCacheConfig,
    #[serde(default)]
    pub working_set: AgentSurfaceWorkingSetConfig,
    #[serde(default)]
    pub result_projection: AgentSurfaceResultProjectionConfig,
    #[serde(default)]
    pub context_retrieval: AgentSurfaceContextRetrievalConfig,
}

impl Default for AgentSurfaceRuntimeConfig {
    fn default() -> Self {
        Self {
            cache: AgentSurfaceRuntimeCacheConfig::default(),
            working_set: AgentSurfaceWorkingSetConfig::default(),
            result_projection: AgentSurfaceResultProjectionConfig::default(),
            context_retrieval: AgentSurfaceContextRetrievalConfig::default(),
        }
    }
}

fn default_surface_schema_index_limit() -> usize {
    64
}

fn default_surface_plan_limit() -> usize {
    512
}

fn default_surface_cache_idle_ttl_seconds() -> u64 {
    1_800
}

fn default_surface_loaded_family_limit() -> usize {
    // Two, not one: a browser task loads a date pack for "next Friday" and
    // must not lose the browser to get it (2026-09-20).
    2
}

fn default_surface_loaded_tool_limit() -> usize {
    128
}

fn default_surface_schema_byte_limit() -> usize {
    256 * 1024
}

fn default_realtime_turn_context_budget_ms() -> u64 {
    300
}

fn default_result_projection_max_depth() -> usize {
    8
}

fn default_result_projection_max_scalar_bytes() -> usize {
    8 * 1_024
}

/// Human-in-the-loop settings (secure HITL plan §6.1, §6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitlConfig {
    #[serde(default)]
    pub critical_delivery: HitlCriticalDeliverySettings,
    #[serde(default)]
    pub verification_codes: HitlVerificationCodesSettings,
}

/// Inbound verification-code retrieval (plan §6.2). Which sources may be
/// read is not here — that is a purpose the owner grants per source (an
/// Observe channel entry's `purposes`, a paired device on the device
/// policy); this only shapes how the resolver runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitlVerificationCodesSettings {
    /// The master switch. Off: no source is watched, every code is typed.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long a critical `otp` ask with an active watcher waits before the
    /// channel alerts of §6.1 go out (push is never delayed). A short
    /// deadline skips the grace.
    #[serde(default = "default_retrieval_grace_secs")]
    pub retrieval_grace_secs: u64,
    /// How far before the challenge started a message may have arrived and
    /// still count — a "Send code" pressed a moment before the ask was raised.
    #[serde(default = "default_retrieval_lookback_secs")]
    pub lookback_secs: u64,
}

impl Default for HitlVerificationCodesSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            retrieval_grace_secs: default_retrieval_grace_secs(),
            lookback_secs: default_retrieval_lookback_secs(),
        }
    }
}

fn default_retrieval_grace_secs() -> u64 {
    20
}

fn default_retrieval_lookback_secs() -> u64 {
    90
}

/// How a critical request is projected onto the owner's registered
/// destinations. Owner addresses are never here — they stay in
/// `envoy.owner_identities` / `owner_identity_envs`, the one owner authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitlCriticalDeliverySettings {
    /// Channel types that receive a critical-request alert, in preference
    /// order (`kapso`, `telegram`, …). A type without an owner identity or a
    /// connected bot is skipped with an honest delivery state. Empty means no
    /// channel fan-out — today's behaviour.
    #[serde(default)]
    pub enabled_channels: Vec<String>,
    /// `simultaneous` sends every destination at once; `staged` sends the
    /// first and moves to the next only when the provider has not accepted
    /// within `staged_fallback_secs`.
    #[serde(default)]
    pub policy: CriticalDeliveryPolicy,
    #[serde(default = "default_staged_fallback_secs")]
    pub staged_fallback_secs: u64,
    /// Registered mobile devices receive the attention push (the existing
    /// behaviour). Off keeps push registrations but sends them no alert.
    #[serde(default = "default_true")]
    pub push_enabled: bool,
    /// A daily window during which alerts are held unless the request is
    /// time-bound and `interrupt_for_time_bound` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHoursSettings>,
    /// The origin the alert's "Open secure request" link is built on, when
    /// the owner-facing UI does not live where the mobile clients connect.
    ///
    /// Unset means `mobile_access.public_origin`, which is right whenever one
    /// origin serves both. It stops being right the moment they are split: a
    /// deployment that routes its device origin to the API and serves the UI
    /// from another hostname built a link to a path the API does not serve,
    /// and a browser offered the bodyless 404 as a download. One setting was
    /// doing two jobs — the mobile enrollment origin and the owner's link —
    /// and only a single-origin deployment let it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_ui_origin: Option<String>,
}

impl HitlCriticalDeliverySettings {
    /// The origin the owner's "open secure request" link is built on.
    ///
    /// `owner_ui_origin` first, the mobile access origin as the single-origin
    /// default. Every surface that decides whether a link can be built — the
    /// delivery policy and the settings envelope's warning — must ask this,
    /// or one of them says "no origin configured" while the other is happily
    /// sending links.
    pub fn owner_link_origin(&self, mobile_access: &MobileAccessConfig) -> Option<String> {
        self.owner_ui_origin
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| mobile_access.resolved_public_origin())
    }
}

impl Default for HitlCriticalDeliverySettings {
    fn default() -> Self {
        Self {
            enabled_channels: Vec::new(),
            policy: CriticalDeliveryPolicy::default(),
            staged_fallback_secs: default_staged_fallback_secs(),
            push_enabled: true,
            quiet_hours: None,
            owner_ui_origin: None,
        }
    }
}

fn default_staged_fallback_secs() -> u64 {
    45
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticalDeliveryPolicy {
    #[default]
    Simultaneous,
    Staged,
}

/// A daily quiet window in the owner's timezone. `start` and `end` are
/// `HH:MM`; a window may cross midnight (`22:00`–`07:00`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHoursSettings {
    pub start: String,
    pub end: String,
    /// An IANA timezone name (`Asia/Kolkata`); UTC when it does not parse.
    #[serde(default = "default_quiet_hours_timezone")]
    pub timezone: String,
    /// Whether a request with a collection deadline may interrupt the window.
    #[serde(default = "default_true")]
    pub interrupt_for_time_bound: bool,
}

fn default_quiet_hours_timezone() -> String {
    "UTC".to_string()
}

/// Runtime-owned connection information advertised to generic mobile builds.
///
/// `public_origin` is normally written by the Cloudflare/tunnel provisioning
/// path. `MAGICIAN_MOBILE_PUBLIC_ORIGIN` and the former
/// `MAGICIAN_DEVICE_PUBLIC_ORIGIN` remain machine-local override fallbacks so
/// existing installations migrate without rebuilding either mobile app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MobileAccessConfig {
    #[serde(default)]
    pub public_origin: Option<String>,
    /// SHA-256 fingerprints of reviewed Magdroid APK signing certificates.
    /// Empty is deliberately non-activatable for Android Apps automation.
    #[serde(default)]
    pub android_apps_signing_sha256: Vec<String>,
    /// SHA-256 fingerprints of trusted Android Key Attestation root
    /// certificates. Empty fails Apps enrollment closed.
    #[serde(default)]
    pub android_attestation_root_sha256: Vec<String>,
    /// Diagnostic SHA-256 checksums of reviewed whole Magdroid APK artifacts.
    /// These are never an authority proof: the app can report its own local
    /// PackageManager result. Production authority comes from a server-decoded
    /// Play Integrity PLAY_RECOGNIZED verdict below.
    #[serde(default)]
    pub android_apps_apk_sha256: Vec<String>,
    /// Exact reviewed Magdroid version codes admitted for Apps automation,
    /// regardless of whether the build came from Google Play or a private
    /// distribution channel. Empty keeps Apps automation unavailable.
    #[serde(default)]
    pub android_apps_version_codes: Vec<u64>,
    /// Google Cloud project number used by the handset's Play Integrity
    /// standard-token provider. Missing/zero keeps Apps enrollment unavailable.
    #[serde(default)]
    pub android_play_integrity_cloud_project_number: Option<u64>,
    /// Exact reviewed Google Play version codes admitted for Magdroid Apps.
    #[serde(default)]
    pub android_play_integrity_version_codes: Vec<u64>,
    /// Device-integrity labels that every decoded verdict must contain (for
    /// example `MEETS_DEVICE_INTEGRITY`). Empty fails closed.
    #[serde(default)]
    pub android_play_integrity_required_device_verdicts: Vec<String>,
}

impl MobileAccessConfig {
    pub fn resolved_public_origin(&self) -> Option<String> {
        self.public_origin
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                [
                    "MAGICIAN_MOBILE_PUBLIC_ORIGIN",
                    "MAGICIAN_DEVICE_PUBLIC_ORIGIN",
                ]
                .into_iter()
                .find_map(|name| {
                    std::env::var(name)
                        .ok()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                })
            })
    }

    pub fn android_apps_attestation_pins(
        &self,
    ) -> (
        &[String],
        &[String],
        &[String],
        &[u64],
        Option<u64>,
        &[u64],
        &[String],
    ) {
        (
            &self.android_apps_signing_sha256,
            &self.android_attestation_root_sha256,
            &self.android_apps_apk_sha256,
            &self.android_apps_version_codes,
            self.android_play_integrity_cloud_project_number,
            &self.android_play_integrity_version_codes,
            &self.android_play_integrity_required_device_verdicts,
        )
    }
}

/// Which managed chat and agentic engines use the Decision Engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    #[default]
    AllEngines,
    MagicianOnly,
    Off,
}

impl DecisionMode {
    pub fn allows(self, engine: &str) -> bool {
        match self {
            Self::AllEngines => true,
            Self::MagicianOnly => engine.trim() == "magician",
            Self::Off => false,
        }
    }
}

/// Magician's side of the structured-decision plane: which engines use it,
/// where the process listens and how long a step waits for it.
/// Everything else — models, tiers, operations, thresholds, shadow/gate
/// rollout — is the engine's, in `<runtime root>/decision-engine.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DecisionHostConfigWire")]
pub struct DecisionHostConfig {
    pub mode: DecisionMode,
    /// Unix socket path. Unset = `<runtime root>/run/decision-engine.sock`,
    /// the engine binary's own default.
    pub socket: Option<String>,
    /// Whole-call budget. A configured but unavailable engine stops the step.
    pub timeout_ms: u64,
}

impl Default for DecisionHostConfig {
    fn default() -> Self {
        Self {
            mode: DecisionMode::AllEngines,
            socket: None,
            timeout_ms: 20_000,
        }
    }
}

/// Read the former boolean on upgrade, but write only the single mode setting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct DecisionHostConfigWire {
    mode: Option<DecisionMode>,
    enabled: Option<bool>,
    socket: Option<String>,
    timeout_ms: u64,
}

impl Default for DecisionHostConfigWire {
    fn default() -> Self {
        Self {
            mode: None,
            enabled: None,
            socket: None,
            timeout_ms: DecisionHostConfig::default().timeout_ms,
        }
    }
}

impl TryFrom<DecisionHostConfigWire> for DecisionHostConfig {
    type Error = &'static str;

    fn try_from(wire: DecisionHostConfigWire) -> Result<Self, Self::Error> {
        if wire.mode.is_some() && wire.enabled.is_some() {
            return Err("decision.mode replaces decision.enabled; use only mode");
        }
        Ok(Self {
            mode: wire.mode.unwrap_or(if wire.enabled == Some(false) {
                DecisionMode::Off
            } else {
                DecisionMode::AllEngines
            }),
            socket: wire.socket,
            timeout_ms: wire.timeout_ms,
        })
    }
}

/// Magician V2 configuration
///
/// `deny_unknown_fields`: an unknown top-level key is a hard load error rather
/// than being silently ignored, so config drift surfaces loudly instead of
/// rotting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicianConfig {
    /// Enable the Magician conversation orchestrator
    #[serde(default = "default_magician_enabled")]
    pub enabled: bool,
    /// Enable real-time event streaming for workflow visualization
    #[serde(default = "default_magician_realtime_events")]
    pub realtime_events: bool,
    /// Storage path for conversation data
    #[serde(default = "default_magician_storage_path")]
    pub storage_path: String,
    /// Workspace storage backend selector (local_file | silverbullet_space).
    /// Folded in from the former `workspace_storage/settings.json` so the runtime
    /// root carries a single config file; the settings API reads + mutates this.
    #[serde(default)]
    pub workspace_storage: crate::magician_v2::workspace_storage_settings::WorkspaceStorageSettings,
    #[serde(default)]
    pub database_maintenance:
        crate::magician_v2::storage_governance::database_maintenance::DatabaseMaintenanceConfig,
    /// Runtime daemon/provider policy. Env vars remain override fallbacks for
    /// machine-local differences; config owns the deployment default.
    #[serde(default)]
    pub runtime: MagicianRuntimeSettings,
    /// Maximum number of conversations to keep in memory
    #[serde(default = "default_magician_max_conversations")]
    pub max_conversations: usize,
    /// Conversation timeout in seconds
    #[serde(default = "default_magician_conversation_timeout")]
    pub conversation_timeout: u64,
    /// Router-based LLM configuration (preferred schema).
    #[serde(default)]
    pub llm: MagicianLlmSettings,
    /// Operator processing-locality policy (`privacy.processing.mode`).
    /// Derived into `llm.router.locality` at load; the YAML section is the
    /// single source of truth.
    #[serde(default)]
    pub privacy: PrivacySettings,
    /// App provider-locality declarations and related runtime policy.
    #[serde(default)]
    pub app_platform: AppPlatformSettings,
    /// Content-free LLM facts and separately governed sanitized capture.
    #[serde(default)]
    pub analytics: MagicianAnalyticsSettings,
    /// Coding profile catalog for Pi-backed engineering workers.
    #[serde(default)]
    pub coding: MagicianCodingSettings,
    /// Verification-controller activation, gate budgets, owner baseline and
    /// reconciler cadence. Absent = disabled, module defaults.
    #[serde(default)]
    pub verification: VerificationConfig,
    /// VibeDev static-site publish/deploy policy.
    #[serde(default)]
    pub vibedev_deploy: VibeDevDeployConfig,
    /// Memory retrieval, temperature, and prompt-injection configuration.
    #[serde(default)]
    pub memory: MagicianMemorySettings,
    /// Media/audio provider configuration.
    #[serde(default)]
    pub media: MagicianMediaSettings,
    /// Communication understanding and safe-brief migration policy.
    #[serde(default)]
    pub channel_assist: ChannelAssistConfig,
    /// Structured-decision plane (typed Choice/Score/Noul judgments): how
    /// to reach the `decision-engine` process. The engine owns the rest.
    #[serde(default)]
    pub decision: DecisionHostConfig,
    /// Shared, progressive learning policy for owner attention surfaces.
    #[serde(default)]
    pub attention_learning: AttentionLearningConfig,
    /// Proactive resurfacing feature gates and action policy.
    #[serde(default)]
    pub resurfacing: ResurfacingConfig,
    /// Scoped internal social network and its autonomous admission policy.
    #[serde(default)]
    pub social: SocialConfig,
    /// Cadence for the sweep that turns provider hard bounces and complaints
    /// into suppression-register entries.
    #[serde(default)]
    pub delivery_hygiene: DeliveryHygieneConfig,
    /// Cadence and cohorts for the sweep that turns an elapsed waiting window
    /// into a recorded `silent` outcome — the only producer of the observations
    /// a cohort comparison would otherwise never see. Ships off.
    #[serde(default)]
    pub outcome_maturity: OutcomeMaturityConfig,
    /// Floors, cadence and destination for the pass that turns a filled cohort
    /// into a proposal an owner decides on.
    ///
    /// Separate from `outcome_maturity` because the two answer different
    /// questions and ship at opposite postures. The sweep above RECORDS a
    /// judgement into an append-only store, so it is off until an operator names
    /// what it may judge. This pass decides nothing and applies nothing — it
    /// files a candidate — so it is on, and inert until the sweep it reads has
    /// recorded something.
    ///
    /// It had no key at all until 2026-08-21: the binary passed
    /// `OutcomeProposalConfig::default()`, so the sample floors that decide
    /// whether a comparison is evidence or a coincidence were unreachable to
    /// the person who has to defend the conclusion.
    #[serde(default)]
    pub outcome_proposal: OutcomeProposalSettings,
    /// Where a durable run's out-of-band verification arrives, if anywhere.
    /// `None` means the run-inbox sweep has no source and every wait must be
    /// closed by hand.
    #[serde(default)]
    pub run_inbox: Option<RunInboxConfig>,
    /// Provider-neutral discovery/read ladders shared by research, feeds,
    /// monitors, and future observation surfaces.
    #[serde(default)]
    pub content_acquisition: crate::magician_v2::content_sources::ContentAcquisitionSettings,
    /// Optional HTTP endpoint for remote Magician service (enables proxy mode)
    #[serde(default)]
    pub service_url: Option<String>,
    /// Self-hosted mobile-client bootstrap. The public origin belongs to the
    /// deployment, never to an iOS or Android build; enrollment places the
    /// resolved value in a short-lived QR connection profile.
    #[serde(default)]
    pub mobile_access: MobileAccessConfig,
    /// Frontend delivery configuration
    #[serde(default)]
    pub frontend: MagicianFrontendSettings,
    /// Execution/Magicutor configuration
    #[serde(default)]
    pub execution: MagicianExecutionSettings,
    /// Generic agentic outer-loop settings. History keep-tail lives in
    /// `decision.rs`; this section is reserved for future knobs.
    #[serde(default)]
    pub agentic: AgenticSettings,
    /// Uniform cache, tool-result projection, and staged-context lifecycle for
    /// Chat, realtime voice, and autonomous tasks. The runtime is active by
    /// default; this configuration contains tuning only.
    #[serde(default)]
    pub agent_surface_runtime: AgentSurfaceRuntimeConfig,
    /// API Mining configuration (feature-flagged, all defaults to off)
    #[serde(default)]
    pub api_mining: ApiMiningConfig,
    /// Capability evolution configuration (self-generating/action-promotion track).
    /// Defaults to disabled and must be explicitly enabled.
    #[serde(default)]
    pub capability_evolution: CapabilityEvolutionConfig,
    /// Developer Mode `interactive_process` policy — operator CLI catalog,
    /// concurrency caps per CLI program, etc. See
    /// `docs/plans/2026-05-13-developer-mode-workbench.md`.
    #[serde(default)]
    pub interactive_process: InteractiveProcessConfig,
    /// Consumer mode: forces AtomicComposition strategy, blocks GuidedSearch.
    #[serde(default = "default_consumer_mode")]
    pub consumer_mode: bool,
    /// Optional durable task-state policy for runtime-context execution.
    #[serde(default)]
    pub task_state: TaskStateConfig,
    /// Enrollment configuration for consumer channel identity.
    #[serde(default)]
    pub enrollment: EnrollmentConfig,
    /// Authentication posture: `open` (default, today's behaviour
    /// bit-identical) or `credentials` (bearer required). See
    /// `docs/components/magician/auth.md`.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Envoy routing: owner vs guest identity resolution + envoy agent id.
    #[serde(default)]
    pub envoy: EnvoyConfig,
    /// Human-in-the-loop delivery: how a critical request (a credential or
    /// code ask, a time-bound decision) reaches the owner's verified private
    /// destinations. See `docs/components/magician/critical-request-delivery.md`.
    #[serde(default)]
    pub hitl: HitlConfig,
    /// Outward-action policy: what may actually be sent, versus captured.
    #[serde(default)]
    pub outward_actions: OutwardActionsConfig,
    /// Approval-envelope posture: whether consent-per-outcome is resolved at
    /// dispatch, observed only, or enforced. Off by default.
    #[serde(default)]
    pub approval_envelopes: ApprovalEnvelopesConfig,
    /// Recipient-compliance posture: whether the four below-the-agent recipient
    /// checks refuse a send at dispatch. Off by default.
    #[serde(default)]
    pub recipient_compliance: RecipientComplianceConfig,
    /// Public chat admission/budget/onboarding policy for high-volume channels.
    #[serde(default)]
    pub public_chat: PublicChatConfig,
    /// Chat-turn mouth (`chat.harness_engine`). Default `magician` is today's
    /// LLM path. Roster names replace the mouth; hands stay on the plane.
    #[serde(default)]
    pub chat: MagicianChatTurnSettings,
    /// Tool authorization policy for unlisted tools.
    #[serde(default)]
    pub tool_authorization: ToolAuthorizationConfig,
    /// Resource Authority Layer 2: declarative budgets / spend-token
    /// resolution. Off by default — see
    /// `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`.
    #[serde(default)]
    pub resource_authority: crate::magician_v2::resource_authority::config::ResourceAuthorityConfig,
    /// Autonomous harness ("company loop") — the CTO autofix detect-and-fix loop.
    /// Autofix defaults ON (opt-out via `harness.autofix_enabled: false` or the
    /// `MAGICIAN_HARNESS_AUTOFIX` env override).
    #[serde(default)]
    pub harness: HarnessConfig,
    /// Magician plane MCP catalog profiles (spawned-bare vs terminal).
    #[serde(default)]
    pub plane: PlaneConfig,
}

impl MagicianConfig {
    pub fn resolved_ollama_keep_alive(&self) -> Option<String> {
        env_ollama_keep_alive_override().or_else(|| {
            self.runtime
                .ollama
                .keep_alive
                .as_deref()
                .and_then(normalize_runtime_ollama_keep_alive)
        })
    }

    /// Returns the configured router definition when available and non-empty.
    pub fn router_config(&self) -> Option<&LLMRouterConfig> {
        self.llm
            .router
            .as_ref()
            .filter(|router| !router.profiles.is_empty())
    }

    pub fn coding_profile_catalog(&self) -> Vec<CodingProfileInfo> {
        let Some(router) = self.router_config() else {
            return Vec::new();
        };
        let configured_profiles = &self.coding.profiles;
        let default_profile =
            default_coding_profile_id(&self.coding, configured_profiles).map(str::to_string);
        let mut profiles: Vec<CodingProfileInfo> = configured_profiles
            .iter()
            .filter(|profile| profile.enabled)
            .filter_map(|profile| {
                let resolved = router.resolve_profile(profile.llm_profile.trim())?;
                let llm_profile = resolved.standard_or_fast_profile();
                Some(CodingProfileInfo {
                    id: profile.id.trim().to_string(),
                    label: profile.display_label(),
                    llm_profile: profile.llm_profile.trim().to_string(),
                    provider: llm_profile.provider.to_string(),
                    model: llm_profile.model.clone(),
                    supports_user_image_inputs: llm_profile_supports_user_image_inputs(llm_profile),
                    is_default: default_profile
                        .as_deref()
                        .map(|default_id| default_id == profile.id.trim())
                        .unwrap_or(false),
                    description: profile.description_trimmed(),
                })
            })
            .collect();
        profiles.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.id.cmp(&b.id))
        });
        profiles
    }

    pub fn resolve_coding_profile(
        &self,
        requested_profile: Option<&str>,
    ) -> Result<Option<ResolvedCodingProfile>, String> {
        let configured_profiles = &self.coding.profiles;
        let Some(profile) =
            select_coding_profile(&self.coding, configured_profiles, requested_profile)?
        else {
            return Ok(None);
        };
        let Some(router) = self.router_config() else {
            return Err(
                "coding profiles are configured, but magician-config.yaml has no llm.router profiles"
                    .to_string(),
            );
        };
        let Some(resolved) = router.resolve_profile(profile.llm_profile.trim()) else {
            return Err(format!(
                "coding profile `{}` references missing llm profile `{}`",
                profile.id.trim(),
                profile.llm_profile.trim()
            ));
        };
        let llm_profile = resolved.standard_or_fast_profile();
        Ok(Some(ResolvedCodingProfile {
            id: profile.id.trim().to_string(),
            label: profile.display_label(),
            llm_profile: profile.llm_profile.trim().to_string(),
            provider: llm_profile.provider.to_string(),
            model: llm_profile.model.clone(),
            supports_user_image_inputs: llm_profile_supports_user_image_inputs(llm_profile),
            thinking_level: llm_profile_thinking_level(llm_profile),
            api_key_env: profile_api_key_env(llm_profile),
            // NOT `clamp(1, ..)`: that would turn a configured `0` — "no wall
            // clock" — into a one-second turn.
            turn_timeout_secs: coding_budgets::clamp_turn_timeout_secs(
                profile
                    .turn_timeout_secs
                    .unwrap_or(self.coding.turn_timeout_secs),
            ),
        }))
    }
}

/// Map an LLM profile's reasoning effort to a Pi thinking level (`set_thinking_level`).
/// Only reasoning-configured profiles return a level; everything else returns
/// `None` so Pi keeps its own per-model default. Unknown effort strings also map
/// to `None` (don't send an invalid level Pi would reject).
fn llm_profile_thinking_level(config: &LLMProfile) -> Option<String> {
    if config.supports_reasoning != Some(true) {
        return None;
    }
    let effort = config
        .reasoning
        .as_ref()?
        .effort
        .trim()
        .to_ascii_lowercase();
    let level = match effort.as_str() {
        "off" | "none" => "off",
        "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" | "max" => "xhigh",
        _ => return None,
    };
    Some(level.to_string())
}

fn llm_profile_supports_user_image_inputs(config: &LLMProfile) -> bool {
    let openai_api_mode = config
        .metadata
        .as_ref()
        .and_then(|params| params.get("openai_api_mode"))
        .and_then(|value| value.as_str())
        .map(|mode| mode.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "responses".to_string());
    let supports_vision = config
        .supports_vision
        .unwrap_or_else(|| match &config.provider {
            LLMProviderKind::Anthropic => true,
            LLMProviderKind::Minimax => false,
            LLMProviderKind::DeepSeek => false,
            LLMProviderKind::OpenAI => matches!(openai_api_mode.as_str(), "responses" | "auto"),
            LLMProviderKind::Gemini => true,
            LLMProviderKind::OpenRouter | LLMProviderKind::Yutori => {
                matches!(config.default_modality, Some(LLMModality::Vision))
            },
            LLMProviderKind::Xai => magicllm::XaiProvider::model_supports_vision(&config.model),
            _ => false,
        });

    match &config.provider {
        LLMProviderKind::OpenAI => {
            supports_vision && matches!(openai_api_mode.as_str(), "responses" | "auto")
        },
        LLMProviderKind::Anthropic
        | LLMProviderKind::Minimax
        | LLMProviderKind::DeepSeek
        | LLMProviderKind::OpenRouter
        | LLMProviderKind::Gemini
        | LLMProviderKind::Yutori
        | LLMProviderKind::Xai => supports_vision,
        _ => false,
    }
}

fn default_coding_profile_id<'a>(
    coding: &'a MagicianCodingSettings,
    profiles: &'a [CodingProfileConfig],
) -> Option<&'a str> {
    coding
        .default_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            profiles
                .iter()
                .find(|profile| profile.enabled && !profile.id.trim().is_empty())
                .map(|profile| profile.id.trim())
        })
}

fn select_coding_profile<'a>(
    coding: &'a MagicianCodingSettings,
    profiles: &'a [CodingProfileConfig],
    requested_profile: Option<&str>,
) -> Result<Option<&'a CodingProfileConfig>, String> {
    let selected_id = requested_profile
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| default_coding_profile_id(coding, profiles));
    let Some(selected_id) = selected_id else {
        return Ok(None);
    };
    profiles
        .iter()
        .find(|profile| profile.enabled && profile.id.trim() == selected_id)
        .map(Some)
        .ok_or_else(|| {
            let available: Vec<String> = profiles
                .iter()
                .filter(|profile| profile.enabled)
                .map(|profile| profile.id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect();
            format!(
                "unknown coding profile `{selected_id}`; available profiles: {}",
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )
        })
}

impl CodingProfileConfig {
    fn display_label(&self) -> String {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.id.trim())
            .to_string()
    }

    fn description_trimmed(&self) -> Option<String> {
        self.description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}

trait ResolvedProfileExt<'a> {
    fn standard_or_fast_profile(self) -> &'a magicllm::config::LLMProfile;
}

impl<'a> ResolvedProfileExt<'a> for ResolvedProfile<'a> {
    fn standard_or_fast_profile(self) -> &'a magicllm::config::LLMProfile {
        match self {
            ResolvedProfile::Standard(profile) => profile,
            ResolvedProfile::Adaptive { fast, .. } => fast,
        }
    }
}

fn profile_api_key_env(profile: &magicllm::config::LLMProfile) -> Option<String> {
    profile
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn magician_config_path() -> PathBuf {
    const CONFIG_FILE: &str = "magician-config.yaml";
    // 0. Explicit evaluator/operator override. Launch helpers pass this through
    // the compiled `ollama-launch-config` command, so daemon settings and the
    // runtime/eval loading the config cannot silently diverge.
    if let Some(explicit) = explicit_magician_config_path(std::env::var_os("MAGICIAN_CONFIG_PATH"))
    {
        return explicit;
    }
    // 1. Runtime root: `MAGICIAN_ROOT_DIR` env, else the `$HOME/MagicianNotes`
    //    default (temp under the cargo test harness). The seed scripts copy the
    //    config there so a mounted runtime root is self-contained.
    let runtime_candidate =
        crate::magician_v2::artifact_v2::workspace::default_storage_base_path().join(CONFIG_FILE);
    if runtime_candidate.exists() {
        return runtime_candidate;
    }
    // 2. Repo-root git-backed seed/default. Dev and package flows copy this
    //    into runtime roots when the live config is missing.
    let repo_root_config = PathBuf::from(CONFIG_FILE);
    if repo_root_config.exists() {
        return repo_root_config;
    }
    // 3. Legacy in-repo location (pre-relocation dev checkouts).
    PathBuf::from("magician").join(CONFIG_FILE)
}

fn explicit_magician_config_path(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// The router's two large, UI-edited tables — `profiles` and `operation_mapping`
/// — live beside the config rather than inside it. They were about 4,000 of the
/// 6,200 lines the config used to carry, and they are the only parts edited at
/// anything like that cadence.
///
/// Keeping them in one sibling file also means a settings UI can rewrite that
/// file textually. The two existing config writers save by re-serializing the
/// whole document, which drops every comment in it; routing edits would have
/// been a third such writer.
pub const ROUTER_TABLES_FILE: &str = "llm-router.yaml";

/// Splice `llm-router.yaml` into the config text at `llm.router`.
///
/// A TEXT splice rather than a merge of two parsed documents, deliberately: the
/// profiles block aliases `*local_generation_model`, whose anchor is defined in
/// `runtime:` of the main config, and YAML anchors do not cross files. Keeping
/// it one document at parse time lets the alias resolve with no change to
/// either file.
///
/// The file is resolved as a sibling of the config actually being loaded, never
/// through an independent search order, so a live runtime config can never
/// silently pair with the repository's profiles.
///
/// Missing or unreadable is fatal. `llm_pricing.json` is fail-open because it
/// overlays a built-in rate table; there is no built-in profile table, so a
/// tolerated absence would boot a router with nothing to route to and surface
/// as confusing per-operation failures much later.
/// Splice router tables into config text. Pure, so tests and the desktop
/// packager can build the same complete document `load_magician_config_from_path`
/// would, without a filesystem.
pub fn splice_router_tables_into(config_str: &str, tables: &str) -> Result<String> {
    let mut out = String::with_capacity(config_str.len() + tables.len() + 1);
    let mut in_llm = false;
    let mut spliced = false;
    for line in config_str.lines() {
        out.push_str(line);
        out.push('\n');
        if line.starts_with("llm:") {
            in_llm = true;
        } else if in_llm && !line.starts_with(' ') && !line.trim().is_empty() {
            in_llm = false;
        }
        if in_llm && !spliced && line.starts_with("  router:") {
            out.push_str(tables);
            if !tables.ends_with('\n') {
                out.push('\n');
            }
            spliced = true;
        }
    }
    if !spliced {
        anyhow::bail!("config text has no `llm.router:` key to splice router tables into");
    }
    Ok(out)
}

/// The repository's `magician-config.yaml`, complete — its router tables
/// spliced in exactly as a load would.
///
/// Tests and the desktop packager must never read the config file alone: since
/// the tables moved to `llm-router.yaml`, the file on its own has no profiles
/// and no operation mapping, and `#[serde(default)]` means that parses cleanly
/// into an empty router rather than failing.
pub fn shipped_repo_config_yaml() -> String {
    splice_router_tables_into(
        include_str!("../../magician-config.yaml"),
        include_str!("../../llm-router.yaml"),
    )
    .expect("the repository config carries an llm.router key")
}

fn splice_router_tables(config_str: String, path: &Path) -> Result<String> {
    // A config that already carries its tables inline is complete — take it as
    // it is. That covers three real shapes: a document produced by
    // `splice_router_tables_into` and written to disk (which is what test
    // fixtures seed into temp directories), a config predating the split, and a
    // deliberately self-contained one. Splicing a second copy in would give the
    // document two `profiles` keys; demanding a sibling anyway would make every
    // seeded fixture unloadable for no benefit.
    if config_has_inline_router_tables(&config_str) {
        return Ok(config_str);
    }

    let tables_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(ROUTER_TABLES_FILE);
    let tables = std::fs::read_to_string(&tables_path).with_context(|| {
        format!(
            "Failed to read {} (LLM router tables for {}, which does not carry them inline)",
            tables_path.display(),
            path.display()
        )
    })?;
    splice_router_tables_into(&config_str, &tables).with_context(|| {
        format!(
            "{} has no `llm.router:` key to splice {} into",
            path.display(),
            tables_path.display()
        )
    })
}

/// Whether the config text already carries a router table itself.
///
/// Either `profiles` or `operation_mapping` counts: the two always move
/// together in a real config, and accepting either keeps this in step with the
/// Python reader in `scripts/magician_config_text.py`, which fixtures exercise
/// with only one table present.
///
/// Checked on the text rather than after parsing, because parsing is exactly
/// what this decides the input for — and because `profiles` carries
/// `#[serde(default)]`, so a parsed-but-absent table is indistinguishable from
/// an empty one.
fn config_has_inline_router_tables(config_str: &str) -> bool {
    let mut in_llm = false;
    let mut in_router = false;
    for line in config_str.lines() {
        // Comments carry no indentation contract — `llm-router.yaml` opens with
        // a column-0 header block, and treating those as a dedent would end the
        // `llm:` section before its own tables were seen.
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with("llm:") {
            in_llm = true;
            continue;
        }
        if in_llm && !line.starts_with(' ') && !line.trim().is_empty() {
            in_llm = false;
            in_router = false;
            continue;
        }
        if !in_llm {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ") {
            if !rest.starts_with(' ')
                && !rest.trim_start().starts_with('#')
                && !rest.trim().is_empty()
            {
                in_router = rest.starts_with("router:");
                continue;
            }
        }
        if in_router
            && (line.starts_with("    profiles:") || line.starts_with("    operation_mapping:"))
        {
            return true;
        }
    }
    false
}

pub fn load_magician_config_from_path(path: &Path) -> Result<MagicianConfig> {
    let config_str = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    // Before validation, so the validator and the parser see the same document.
    let config_str = splice_router_tables(config_str, path)?;
    validate_magician_config_yaml(&config_str)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("Failed to validate {}", path.display()))?;
    let mut config: MagicianConfig = serde_yaml::from_str(&config_str)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    apply_runtime_service_endpoint_overrides(&mut config)?;
    let config = validate_and_apply_magician_config(config, apply_runtime_ollama_policy)?;
    // Envelope posture, published as a process-global the moment a config is
    // accepted — the same reason `outward_actions.capture_only` is published at
    // boot: the dispatch gate holds no config snapshot, and an uninstalled
    // posture is `Off`, so the resolver never runs and the shadow can never
    // fire. Installed here rather than per-entry-point because an entry point
    // nobody remembered to wire is exactly how this stayed unreachable.
    //
    // First install wins (see `install_envelope_mode`), so a later reload cannot
    // quietly promote shadow to enforcing mid-process.
    crate::magician_v2::approval_envelopes::install_configured_envelope_mode(
        &config.approval_envelopes.mode,
    );
    // Recipient-compliance posture, published the same way and here rather than
    // per-entry-point for the same reason. Reading it off a config snapshot at
    // dispatch is not an option: the outward dispatch path holds none, and the
    // uninstalled answer is `Off`, so a binary that never loads a config
    // behaves exactly as it did before this gate existed.
    //
    // First install wins (see `install_recipient_compliance_mode`), so a later
    // reload can neither switch the gate on nor switch it off mid-process — a
    // send refused at 10:00 and permitted at 10:05 with no act in between is an
    // audit trail nobody can read.
    crate::magician_v2::recipient_compliance::install_configured_recipient_compliance_mode(
        &config.recipient_compliance.mode,
    );
    Ok(config)
}

/// Complete every pure normalization and invariant check before committing
/// process-global provider defaults. A rejected runtime reload must never
/// partially change embedding admission, keep-alive, or daemon environment for
/// requests still using the last accepted configuration.
/// `privacy.processing.mode` is the operator-facing switch;
/// `llm.router.locality` is what profile resolution actually consults (the
/// router config flows to magicllm, which never sees the top-level privacy
/// section). Copy it early in validation so every invariant below — and every
/// consumer of the derived router config — observes one consistent locality.
fn apply_privacy_processing_locality(config: &mut MagicianConfig) {
    let locality = config.privacy.processing.mode;
    if let Some(router) = config.llm.router.as_mut() {
        router.locality = locality;
    }
    // One switch, not two that can disagree: app remote processing follows
    // the locality mode rather than an independently set flag.
    // `app_platform.processing.remote_profile` still names the counterpart
    // profile and owns its trust declaration.
    config.app_platform.processing.remote_processing_enabled =
        locality == magicllm::ProcessingLocality::Cloud;
}

/// Dry-run the full validation pipeline on a candidate config with NO
/// process-global side effects (the runtime-policy commit is a no-op).
/// Used by the settings API to reject a locality switch that would leave
/// the on-disk config durably unloadable — e.g. `mode: cloud` deriving
/// `remote_processing_enabled = true` without an
/// `app_platform.processing.remote_profile` to name the counterpart.
pub fn validate_magician_config_dry_run(config: MagicianConfig) -> Result<MagicianConfig> {
    validate_and_apply_magician_config(config, |_| Ok(()))
}

fn validate_and_apply_magician_config<F>(
    mut config: MagicianConfig,
    apply_runtime_policy: F,
) -> Result<MagicianConfig>
where
    F: FnOnce(&mut MagicianConfig) -> Result<()>,
{
    config.database_maintenance.validate()?;
    apply_privacy_processing_locality(&mut config);
    warn_on_embedding_profile_drift(&config);
    enforce_runtime_ollama_config_invariant(&config)?;
    enforce_runtime_retrieval_config_invariant(&config)?;
    enforce_dispatch_config_invariant(&config)?;
    enforce_operation_catalog_invariant(&config)?;
    let keep_alive = config.resolved_ollama_keep_alive();
    normalize_runtime_ollama_policy(&mut config, keep_alive)?;
    enforce_runtime_context_config_invariant(&config)?;
    enforce_agent_surface_runtime_config_invariant(&config.agent_surface_runtime)?;
    enforce_tool_calling_invariant(&config)?;
    enforce_app_processing_trust_invariant(&config)?;
    enforce_app_resource_runtime_invariant(&config)?;
    enforce_app_background_behavior_invariant(&config)?;

    enforce_llm_trace_config_invariant(&config)?;
    enforce_coding_config_invariant(&config)?;
    enforce_memory_config_invariant(&config)?;
    enforce_media_config_invariant(&config)?;
    enforce_public_chat_config_invariant(&config)?;
    enforce_channel_assist_config_invariant(&config)?;
    enforce_social_config_invariant(&config)?;
    config.content_acquisition.validate_bounds()?;
    apply_runtime_policy(&mut config)?;
    Ok(config)
}

fn enforce_operation_catalog_invariant(config: &MagicianConfig) -> Result<()> {
    let Some(router) = config.llm.router.as_ref() else {
        return Ok(());
    };
    router.validate_operation_metadata().map_err(|errors| {
        anyhow::anyhow!(
            "llm.router.operation_mapping metadata is invalid: {}",
            errors.join("; ")
        )
    })
}

fn enforce_llm_trace_config_invariant(config: &MagicianConfig) -> Result<()> {
    let trace = &config.analytics.llm_trace;
    for (path, rate) in [
        (
            "analytics.llm_trace.metadata_capture_rate",
            trace.metadata_capture_rate,
        ),
        (
            "analytics.llm_trace.sanitized_content_rate",
            trace.sanitized_content_rate,
        ),
    ] {
        if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
            return Err(anyhow::anyhow!("{path} must be a finite value in [0, 1]"));
        }
    }
    if trace.payload_records == 0 || trace.payload_records > 8_192 {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.payload_records must be in 1..=8192 so restricted content can never outrank the critical-fact lane"
        ));
    }
    if trace.redaction.policy_version.trim().is_empty()
        || trace.redaction.policy_version.len() > 128
    {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.redaction.policy_version must contain 1..=128 bytes"
        ));
    }
    if trace.redaction.max_payload_bytes == 0
        || trace.redaction.max_payload_bytes > 4 * 1024 * 1024
        || trace.redaction.max_block_chars == 0
        || trace.redaction.max_block_chars > trace.redaction.max_payload_bytes
    {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace redaction bounds require 0 < max_block_chars <= max_payload_bytes <= 4 MiB"
        ));
    }
    if !trace.redaction.fail_to_metadata_only {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.redaction.fail_to_metadata_only must remain true; sanitizer failures may never fail open"
        ));
    }
    // Not gated on `trace.enabled`. The retention sweeper is spawned
    // unconditionally at boot and re-reads this policy every tick, so
    // `{enabled: false, retention: {facts_days: 0}}` — a natural way to express
    // "turn LLM tracing off" — does not disable the sweep, it tells the sweep to
    // delete everything older than today. That includes `llm_content_tombstones`
    // (the proof a user asked for content to be deleted) and
    // `llm_content_access_audit`, which have no other copy.
    if trace.retention.facts_days == 0 || trace.retention.context_metadata_days == 0 {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.retention fact/context days must be non-zero even when tracing is disabled; the retention sweeper runs regardless, and zero means delete every partition before today"
        ));
    }
    if trace.retention.facts_days < trace.retention.sanitized_io_days {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.retention.facts_days must be at least sanitized_io_days so deletion tombstones cannot expire before protected content"
        ));
    }
    if matches!(trace.content_mode, LlmContentMode::Sanitized)
        && trace.retention.sanitized_io_days == 0
    {
        return Err(anyhow::anyhow!(
            "sanitized LLM capture requires non-zero sanitized_io_days retention"
        ));
    }
    if trace.training_default_eligible && !matches!(trace.content_mode, LlmContentMode::Sanitized) {
        return Err(anyhow::anyhow!(
            "analytics.llm_trace.training_default_eligible requires global sanitized capture"
        ));
    }
    if matches!(trace.content_mode, LlmContentMode::FullLocalEncrypted) {
        return Err(anyhow::anyhow!(
            "full_local_encrypted LLM capture is disabled until the Phase 10 key-lifecycle gate"
        ));
    }

    for (kind, overrides) in [
        ("operation", &trace.operation_overrides),
        ("scope", &trace.scope_overrides),
    ] {
        for (key, policy) in overrides {
            if key.trim().is_empty() || key.len() > 256 {
                return Err(anyhow::anyhow!(
                    "analytics.llm_trace.{kind}_overrides contains an invalid key"
                ));
            }
            if !policy.sanitized_content_rate.is_finite()
                || !(0.0..=1.0).contains(&policy.sanitized_content_rate)
            {
                return Err(anyhow::anyhow!(
                    "analytics.llm_trace.{kind}_overrides.{key}.sanitized_content_rate must be in [0, 1]"
                ));
            }
            if matches!(policy.content_mode, LlmContentMode::FullLocalEncrypted) {
                return Err(anyhow::anyhow!(
                    "analytics.llm_trace.{kind}_overrides.{key} cannot enable full_local_encrypted before the Phase 10 security gate"
                ));
            }
            if matches!(policy.content_mode, LlmContentMode::Sanitized)
                && trace.retention.sanitized_io_days == 0
            {
                return Err(anyhow::anyhow!(
                    "analytics.llm_trace.{kind}_overrides.{key} requires non-zero sanitized_io_days retention"
                ));
            }
            if policy.training_eligible && !matches!(policy.content_mode, LlmContentMode::Sanitized)
            {
                return Err(anyhow::anyhow!(
                    "analytics.llm_trace.{kind}_overrides.{key} can be training eligible only with sanitized capture"
                ));
            }
        }
    }
    Ok(())
}

fn enforce_app_processing_trust_invariant(config: &MagicianConfig) -> Result<()> {
    let trust = &config.app_platform.processing;
    if trust.endpoint_trust_revision == 0 {
        return Err(anyhow::anyhow!(
            "app_platform.processing.endpoint_trust_revision must be greater than zero"
        ));
    }
    if trust.profiles.len() > 256 {
        return Err(anyhow::anyhow!(
            "app_platform.processing.profiles must contain at most 256 entries"
        ));
    }
    let router = config.router_config();
    for (field, selected) in [
        ("local_profile", trust.local_profile.as_deref()),
        ("remote_profile", trust.remote_profile.as_deref()),
    ] {
        if let Some(selected) = selected {
            if !trust.profiles.contains_key(selected) {
                return Err(anyhow::anyhow!(
                    "app_platform.processing.{field} must name a profile declared in app_platform.processing.profiles"
                ));
            }
        }
    }
    if trust.remote_processing_enabled && trust.remote_profile.is_none() {
        return Err(anyhow::anyhow!(
            "app_platform.processing.remote_profile is required when remote_processing_enabled is true"
        ));
    }
    for (profile_name, declaration) in &trust.profiles {
        if profile_name.is_empty()
            || profile_name.len() > 256
            || profile_name.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(anyhow::anyhow!(
                "app_platform.processing profile names must contain 1..=256 non-control bytes"
            ));
        }
        if declaration.class == AppProcessingEndpointClass::External
            && declaration.local_processing_eligible
        {
            return Err(anyhow::anyhow!(
                "app_platform.processing.profiles.{profile_name} cannot mark an external endpoint local-processing eligible"
            ));
        }
        let profile = router
            .and_then(|router| router.profiles.get(profile_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "app_platform.processing.profiles.{profile_name} must name one concrete llm.router profile"
                )
            })?;
        if matches!(
            &profile.provider,
            LLMProviderKind::DeepSeek
                | LLMProviderKind::Xai
                | LLMProviderKind::Sarvam
                | LLMProviderKind::Custom(_)
        ) {
            return Err(anyhow::anyhow!(
                "app_platform.processing.profiles.{profile_name} selects a provider without reviewed no_provider_storage enforcement"
            ));
        }
        if profile.provider == LLMProviderKind::OpenRouter {
            return Err(anyhow::anyhow!(
                "app_platform.processing.profiles.{profile_name} selects a provider aggregator without an attested downstream physical route"
            ));
        }
        if trust.local_profile.as_deref() == Some(profile_name)
            && !declaration.local_processing_eligible
        {
            return Err(anyhow::anyhow!(
                "app_platform.processing.local_profile must be local-processing eligible"
            ));
        }
        if trust.remote_profile.as_deref() == Some(profile_name)
            && declaration.local_processing_eligible
        {
            return Err(anyhow::anyhow!(
                "app_platform.processing.remote_profile must not be local-processing eligible"
            ));
        }
        if declaration.local_processing_eligible
            && declaration.class != AppProcessingEndpointClass::External
            && profile.api_base_url.is_none()
        {
            return Err(anyhow::anyhow!(
                "app_platform.processing.profiles.{profile_name} declares a local-eligible endpoint but the profile has no explicit api_base_url"
            ));
        }
        if declaration.class == AppProcessingEndpointClass::LoopbackManaged {
            let endpoint = profile.api_base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "app_platform.processing.profiles.{profile_name} declares loopback_managed but the profile has no explicit api_base_url"
                )
            })?;
            let parsed = url::Url::parse(endpoint).map_err(|error| {
                anyhow::anyhow!(
                    "app_platform.processing.profiles.{profile_name} has invalid api_base_url: {error}"
                )
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(anyhow::anyhow!(
                    "app_platform.processing.profiles.{profile_name} declares loopback_managed but api_base_url is not HTTP(S)"
                ));
            }
            let is_loopback = parsed
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|host| host.is_loopback())
                || parsed.host_str() == Some("localhost");
            if !is_loopback {
                return Err(anyhow::anyhow!(
                    "app_platform.processing.profiles.{profile_name} declares loopback_managed but api_base_url is not loopback"
                ));
            }
        }
    }
    enforce_app_llm_operation_admission_invariant(config)?;
    Ok(())
}

/// Load-time invariant for the additive app LLM operation admission policy
/// (plan 1.4). Every admitted name must be a well-formed operation name
/// (`app:` namespacing therefore cannot be spoofed), must have one static
/// `app:<name>` entry in `llm.router.operation_mapping`, and every selector
/// arm of that entry must name a profile already declared in
/// `app_platform.processing.profiles` — so an app operation can only ride
/// physically reviewed app-processing profiles. Absent or empty admission
/// is valid and changes nothing. These checks duplicate the ones the
/// resolve path performs per call
/// (`resolve_admitted_app_llm_operation_profile` re-checks the admission
/// list, the mapping and the trust catalog against live policy): load time
/// gives the operator a precise diagnosis, the re-check keeps an admission
/// minted before a policy change from routing.
fn enforce_app_llm_operation_admission_invariant(config: &MagicianConfig) -> Result<()> {
    use crate::magician_v2::apps::llm_operations as operation_lane;
    use magician_app_contract::llm_operations::APP_LLM_OPERATION_MAX_PURPOSE_BYTES;

    let trust = &config.app_platform.processing;
    let admissions = &config.app_platform.llm_operations;
    if admissions.is_empty() {
        return Ok(());
    }
    if admissions.len() > operation_lane::APP_LLM_OPERATION_MAX_ADMITTED {
        return Err(anyhow::anyhow!(
            "app_platform.llm_operations must contain at most {} entries",
            operation_lane::APP_LLM_OPERATION_MAX_ADMITTED
        ));
    }
    let router = config.router_config().ok_or_else(|| {
        anyhow::anyhow!("app_platform.llm_operations requires llm.router to be configured")
    })?;
    for (operation, admission) in admissions {
        operation_lane::validate_app_llm_operation_name(operation).map_err(|error| {
            anyhow::anyhow!("app_platform.llm_operations.{operation} is invalid: {error}")
        })?;
        if admission.reviewed_purpose.trim().is_empty()
            || admission.reviewed_purpose.len() > APP_LLM_OPERATION_MAX_PURPOSE_BYTES
            || admission
                .reviewed_purpose
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(anyhow::anyhow!(
                "app_platform.llm_operations.{operation}.reviewed_purpose must contain \
                 1..={APP_LLM_OPERATION_MAX_PURPOSE_BYTES} non-control bytes"
            ));
        }
        if admission.max_output_tokens == Some(0) {
            return Err(anyhow::anyhow!(
                "app_platform.llm_operations.{operation}.max_output_tokens must be greater than zero when present"
            ));
        }
        let namespaced = operation_lane::namespaced_app_llm_operation(operation)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let selector = router.operation_mapping.get(&namespaced).ok_or_else(|| {
            anyhow::anyhow!(
                "app_platform.llm_operations.{operation} references missing \
                 llm.router.operation_mapping `{namespaced}`"
            )
        })?;
        let mut arms: Vec<&str> = Vec::with_capacity(3);
        match selector {
            OperationProfileSelector::Simple(default) => arms.push(default.as_str()),
            OperationProfileSelector::Conditional {
                default,
                when_has_images,
                when_cloud,
                ..
            } => {
                arms.push(default.as_str());
                if let Some(arm) = when_has_images.as_deref() {
                    arms.push(arm);
                }
                if let Some(arm) = when_cloud.as_deref() {
                    arms.push(arm);
                }
            },
        }
        for arm in arms {
            if !trust.profiles.contains_key(arm) {
                return Err(anyhow::anyhow!(
                    "app_platform.llm_operations.{operation} maps `{namespaced}` to profile \
                     `{arm}` that is not declared in app_platform.processing.profiles"
                ));
            }
            // Diagnose a missing router profile at load instead of failing
            // per-call inside `resolve_admitted_app_llm_operation_profile`.
            if !router.profiles.contains_key(arm) {
                return Err(anyhow::anyhow!(
                    "app_platform.llm_operations.{operation} maps `{namespaced}` to profile \
                     `{arm}` that is not declared in llm.router.profiles"
                ));
            }
        }
    }
    Ok(())
}

fn enforce_app_resource_runtime_invariant(config: &MagicianConfig) -> Result<()> {
    config
        .app_platform
        .resources
        .enforcement_policy()
        .validate_server_configuration()
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("invalid app_platform.resources: {error}"))
}

fn enforce_app_background_behavior_invariant(config: &MagicianConfig) -> Result<()> {
    let policy = config.app_platform.background_behaviors;
    if !(5..=300).contains(&policy.tick_interval_seconds)
        || policy.max_installations_per_scope == 0
        || policy.max_installations_per_scope
            > crate::magician_v2::apps::background_behaviors::APP_BEHAVIOR_MAX_INSTALLATIONS_PER_SCOPE
        || policy.max_claims_per_scope_tick == 0
        || policy.max_claims_per_scope_tick
            > crate::magician_v2::apps::background_behaviors::APP_BEHAVIOR_MAX_CLAIMS_PER_SCOPE_TICK
        || !(30..=600).contains(&policy.lease_seconds)
        || !(5..=3_600).contains(&policy.retry_seconds)
        || policy.lease_seconds <= policy.tick_interval_seconds
    {
        return Err(anyhow::anyhow!(
            "app_platform.background_behaviors has invalid scheduler ceilings"
        ));
    }
    if policy.enabled {
        // Arming asserts that unattended schedule/event runs should now
        // happen. Whether one *can* happen is decided by the resource policy,
        // not by this switch: a schedule/event trigger takes the background
        // lane, which is admitted only out of the slots left over after the
        // foreground reserve and only under the per-period start ceiling. Both
        // shapes below deny every background admission, so arming over them
        // would report a master switch that is on while nothing can ever fire
        // — the failure an operator is least likely to notice. Refuse instead.
        // This narrows nothing a disarmed deployment relies on; the shipped
        // ceilings (8 slots, 2 reserved, 1000 starts) arm cleanly.
        let resources = config.app_platform.resources;
        if resources.scheduler_capacity <= resources.foreground_reserved_slots {
            return Err(anyhow::anyhow!(
                "app_platform.background_behaviors.enabled needs a background slot: \
                 app_platform.resources.scheduler_capacity must exceed \
                 foreground_reserved_slots"
            ));
        }
        if resources.max_background_starts_per_period == 0 {
            return Err(anyhow::anyhow!(
                "app_platform.background_behaviors.enabled needs a positive \
                 app_platform.resources.max_background_starts_per_period"
            ));
        }
    }
    Ok(())
}

fn apply_runtime_service_endpoint_overrides(config: &mut MagicianConfig) -> Result<()> {
    let container_host = std::env::var("MAGICIAN_CONTAINER_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let generation_override = std::env::var("MAGICIAN_OLLAMA_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let embedding_override = std::env::var("MAGICIAN_MEMORY_OLLAMA_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    apply_runtime_service_endpoint_values(
        config,
        container_host.as_deref(),
        generation_override.as_deref(),
        embedding_override.as_deref(),
    )
}

fn apply_runtime_service_endpoint_values(
    config: &mut MagicianConfig,
    container_host: Option<&str>,
    generation_override: Option<&str>,
    embedding_override: Option<&str>,
) -> Result<()> {
    if let Some(host) = container_host {
        validate_container_host(host)?;
        if let Some(router) = config.llm.router.as_mut() {
            for (name, profile) in &mut router.profiles {
                if profile.provider != LLMProviderKind::Ollama {
                    continue;
                }
                if let Some(endpoint) = profile.api_base_url.as_deref() {
                    let rewritten = rewrite_loopback_endpoint(endpoint, host)?;
                    if rewritten == endpoint {
                        continue;
                    }
                    profile.api_base_url = Some(rewritten);
                    // A loopback-managed app route moved across the explicit
                    // desktop/container boundary is still owner-controlled,
                    // but it is no longer loopback. Reuse the existing
                    // trusted-self-hosted class so policy and the effective URL
                    // remain consistent. Invalid/non-loopback declarations are
                    // not repaired by this derivation and still fail below.
                    if let Some(declaration) = config.app_platform.processing.profiles.get_mut(name)
                    {
                        if declaration.class == AppProcessingEndpointClass::LoopbackManaged {
                            declaration.class = AppProcessingEndpointClass::TrustedSelfHosted;
                        }
                    }
                }
            }
        }
        config.runtime.ollama.embedding_base_url =
            rewrite_loopback_endpoint(&config.runtime.ollama.embedding_base_url, host)?;
    }

    if let Some(base_url) = generation_override {
        let endpoint = normalize_ollama_generation_endpoint(base_url)?;
        let router = config
            .llm
            .router
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("MAGICIAN_OLLAMA_BASE_URL requires llm.router"))?;
        for (name, profile) in &mut router.profiles {
            if profile.provider != LLMProviderKind::Ollama
                || config.app_platform.processing.profiles.contains_key(name)
            {
                continue;
            }
            // The dedicated embedding daemon is not the generation daemon:
            // this override retargets generation endpoints only, and an
            // embedding profile caught by it would route vector writes at the
            // wrong daemon and a `/api/generate` path.
            if profile_is_embedding_profile(profile) {
                continue;
            }
            profile.api_base_url = Some(endpoint.clone());
        }
    }

    if let Some(base_url) = embedding_override {
        let normalized = normalize_http_base_url("MAGICIAN_MEMORY_OLLAMA_URL", base_url)?;
        config.runtime.ollama.embedding_base_url = normalized.clone();
        // Keep ordinary routed embedding profiles on the direct daemon. An
        // app-reviewed physical endpoint remains independently pinned.
        if let Some(router) = config.llm.router.as_mut() {
            for (name, profile) in &mut router.profiles {
                if profile.provider == LLMProviderKind::Ollama
                    && profile_is_embedding_profile(profile)
                    && !config.app_platform.processing.profiles.contains_key(name)
                {
                    profile.api_base_url = Some(normalized.clone());
                }
            }
        }
    }

    Ok(())
}

/// Warn when the routed embedding profile and the runtime embedding contract
/// drift apart. The routed path takes provider identity from the PROFILE
/// (`embed_for_operation` stamps its model), while every operational knob —
/// and the no-router fallback — still live in `runtime.ollama.embedding_*`.
/// A silent divergence (editing the runtime model without the profile, or
/// vice versa) fails embedding calls with terminal provider 4xx (model not
/// found) instead of surfacing at load time.
fn warn_on_embedding_profile_drift(config: &MagicianConfig) {
    let Some(router) = config.llm.router.as_ref() else {
        return;
    };
    let Some(profile) = router
        .operation_mapping
        .get(magician_vector_index::embedding_router::EMBED_DOCUMENTS_OPERATION)
        .map(|selector| {
            selector.profile_for_locality(&magicllm::config::RequestShape::NONE, router.locality)
        })
        .and_then(|name| router.profiles.get(name))
    else {
        return;
    };
    if !profile_is_embedding_profile(profile) {
        return;
    }
    let runtime_model = config.runtime.ollama.embedding_model.trim();
    let runtime_base = config.runtime.ollama.embedding_base_url.trim();
    let profile_base = profile
        .api_base_url
        .as_deref()
        .map(|url| url.trim().trim_end_matches('/'))
        .unwrap_or_default();
    let model_drift = profile.model.trim() != runtime_model && !runtime_model.is_empty();
    let base_drift = !profile_base.is_empty() && profile_base != runtime_base.trim_end_matches('/');
    if model_drift || base_drift {
        tracing::warn!(
            profile_model = %profile.model,
            runtime_model = %config.runtime.ollama.embedding_model,
            profile_base = %profile_base,
            runtime_base = %config.runtime.ollama.embedding_base_url,
            "routed embedding profile `op-embedding-local` and runtime.ollama.embedding_*              disagree; the ROUTED path uses the profile (edit the profile, or keep both in              sync) or embedding calls will fail with provider-not-found errors"
        );
    }
}

/// Profiles marked `metadata.embedding_profile: true` bind the routed
/// embedding operations (`embed_documents` / `embed_query`) to the dedicated
/// embedding daemon. They mirror `runtime.ollama.embedding_*` and are excluded
/// from generation-endpoint overrides.
fn profile_is_embedding_profile(profile: &LLMProfile) -> bool {
    profile
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("embedding_profile"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn validate_container_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.contains('/')
        || host.contains(':')
        || host.contains(char::is_whitespace)
    {
        return Err(anyhow::anyhow!(
            "MAGICIAN_CONTAINER_HOST must be a hostname without a scheme, port, path, or whitespace"
        ));
    }
    Ok(())
}

fn rewrite_loopback_endpoint(endpoint: &str, host: &str) -> Result<String> {
    let mut parsed = url::Url::parse(endpoint)
        .with_context(|| format!("invalid runtime service endpoint `{endpoint}`"))?;
    let is_loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if is_loopback {
        parsed
            .set_host(Some(host))
            .map_err(|_| anyhow::anyhow!("invalid container host `{host}`"))?;
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn normalize_ollama_generation_endpoint(base_url: &str) -> Result<String> {
    let normalized = normalize_http_base_url("MAGICIAN_OLLAMA_BASE_URL", base_url)?;
    if normalized.ends_with("/api/generate") {
        Ok(normalized)
    } else {
        Ok(format!("{normalized}/api/generate"))
    }
}

fn normalize_http_base_url(name: &str, value: &str) -> Result<String> {
    let parsed = url::Url::parse(value).with_context(|| format!("{name} must be a valid URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(anyhow::anyhow!("{name} must be an http(s) URL with a host"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(anyhow::anyhow!(
            "{name} must not contain a query string or fragment"
        ));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn enforce_media_config_invariant(config: &MagicianConfig) -> Result<()> {
    crate::magician_v2::media_seam::validate_media_settings(&config.media)
        .map_err(|error| anyhow::anyhow!(error))
}

fn enforce_channel_assist_config_invariant(config: &MagicianConfig) -> Result<()> {
    let distillation = &config.channel_assist.distillation;
    if !matches!(distillation.brief_contract_version, 1 | 2) {
        return Err(anyhow::anyhow!(
            "channel_assist.distillation.brief_contract_version must be 1 or 2"
        ));
    }
    let minimum_summary_chars = if distillation.brief_contract_version == 2 {
        160
    } else {
        1
    };
    if !(minimum_summary_chars..=900).contains(&distillation.summary_max_chars) {
        return Err(anyhow::anyhow!(
            "channel_assist.distillation.summary_max_chars must be between {minimum_summary_chars} and 900 for contract v{}",
            distillation.brief_contract_version
        ));
    }
    if !(1..=3_650).contains(&distillation.backfill.lookback_days) {
        return Err(anyhow::anyhow!(
            "channel_assist.distillation.backfill.lookback_days must be between 1 and 3650"
        ));
    }
    if !(1..=64).contains(&distillation.backfill.batch_size) {
        return Err(anyhow::anyhow!(
            "channel_assist.distillation.backfill.batch_size must be between 1 and 64"
        ));
    }
    if !config.resurfacing.recommendation_min_confidence.is_finite()
        || !(0.0..=1.0).contains(&config.resurfacing.recommendation_min_confidence)
    {
        return Err(anyhow::anyhow!(
            "resurfacing.recommendation_min_confidence must be between 0 and 1"
        ));
    }
    if config.resurfacing.action_result_cooldown_days == 0 {
        return Err(anyhow::anyhow!(
            "resurfacing.action_result_cooldown_days must be positive"
        ));
    }
    Ok(())
}

fn enforce_social_config_invariant(config: &MagicianConfig) -> Result<()> {
    let social = &config.social;
    if social.enabled && social.scopes.is_empty() {
        anyhow::bail!("social.scopes must contain at least one explicit scope");
    }
    let mut seen_scopes = std::collections::HashSet::new();
    let mut seen_storage_scopes = std::collections::HashSet::new();
    for scope in &social.scopes {
        for (field, value) in [
            ("principal", scope.principal.as_str()),
            ("workspace", scope.workspace.as_str()),
        ] {
            if value.trim() != value
                || value.is_empty()
                || value.len() > 128
                || matches!(value, "." | "..")
                || value
                    .chars()
                    .any(|character| matches!(character, '/' | '\\' | '\0'))
            {
                anyhow::bail!("social.scopes {field} contains an invalid scope segment");
            }
        }
        if !seen_scopes.insert((&scope.principal, &scope.workspace)) {
            anyhow::bail!("social.scopes contains a duplicate principal/workspace pair");
        }
        let storage_scope =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::scope_dir_segments(
                &scope.principal,
                &scope.workspace,
            );
        if !seen_storage_scopes.insert(storage_scope) {
            anyhow::bail!(
                "social.scopes contains principal/workspace aliases that resolve to one storage scope"
            );
        }
    }
    if social.tick_interval_secs < 30 {
        anyhow::bail!("social.tick_interval_secs must be at least 30");
    }
    if social.cooldown_secs == 0 {
        anyhow::bail!("social.cooldown_secs must be greater than zero");
    }
    if !(1..=100).contains(&social.max_agents_per_tick) {
        anyhow::bail!("social.max_agents_per_tick must be between 1 and 100");
    }
    if social.gate_reserve_tokens == 0 || social.compose_reserve_tokens == 0 {
        anyhow::bail!("social gate/compose token reservations must be greater than zero");
    }
    if social.default_daily_tokens
        < social
            .gate_reserve_tokens
            .saturating_add(social.compose_reserve_tokens)
    {
        anyhow::bail!(
            "social.default_daily_tokens must cover at least one gate plus compose reservation"
        );
    }
    if social.default_daily_tokens > 10_000_000 {
        anyhow::bail!("social.default_daily_tokens must not exceed 10000000");
    }
    if !(1..=10_000).contains(&social.max_post_chars) {
        anyhow::bail!("social.max_post_chars must be between 1 and 10000");
    }
    if !(1..=3_650).contains(&social.retention_days) {
        anyhow::bail!("social.retention_days must be between 1 and 3650");
    }
    if !(100..=1_000_000).contains(&social.max_posts_per_scope) {
        anyhow::bail!("social.max_posts_per_scope must be between 100 and 1000000");
    }
    if !(1_000..=5_000_000).contains(&social.max_spend_log_rows) {
        anyhow::bail!("social.max_spend_log_rows must be between 1000 and 5000000");
    }
    // `social.enabled` used to require `llm.router.operation_mapping` entries
    // for `social_gate` and `social_compose`. Both retired with the first-party
    // engine (queue item 6, slice 4): Town Square's gate and compose are the
    // package's own `app:` operations now, admitted by the operator's
    // `app_platform.llm_operations` policy and routed by its own mapping. Boot
    // must not refuse a config for missing mappings that no core code can
    // route, and must not require an operator to keep them to start the server.
    Ok(())
}

fn enforce_agent_surface_runtime_config_invariant(
    config: &AgentSurfaceRuntimeConfig,
) -> Result<()> {
    for (surface, budget) in [
        ("chat", &config.result_projection.chat),
        ("realtime_voice", &config.result_projection.realtime_voice),
        ("autonomous_task", &config.result_projection.autonomous_task),
    ] {
        anyhow::ensure!(
            (128..=65_536).contains(&budget.max_model_tokens),
            "agent_surface_runtime.result_projection.{surface}.max_model_tokens must be in 128..=65536"
        );
        anyhow::ensure!(
            (1_024..=262_144).contains(&budget.max_serialized_bytes),
            "agent_surface_runtime.result_projection.{surface}.max_serialized_bytes must be in 1024..=262144"
        );
        anyhow::ensure!(
            (1..=1_024).contains(&budget.max_records),
            "agent_surface_runtime.result_projection.{surface}.max_records must be in 1..=1024"
        );
        anyhow::ensure!(
            (1..=64).contains(&budget.max_depth),
            "agent_surface_runtime.result_projection.{surface}.max_depth must be in 1..=64"
        );
        anyhow::ensure!(
            (128..=budget.max_serialized_bytes).contains(&budget.max_scalar_bytes),
            "agent_surface_runtime.result_projection.{surface}.max_scalar_bytes must be in 128..=max_serialized_bytes"
        );
    }

    for (surface, budget) in [
        ("chat", &config.context_retrieval.chat),
        ("realtime_voice", &config.context_retrieval.realtime_voice),
        ("autonomous_task", &config.context_retrieval.autonomous_task),
    ] {
        anyhow::ensure!(
            (25..=60_000).contains(&budget.deadline_ms),
            "agent_surface_runtime.context_retrieval.{surface}.deadline_ms must be in 25..=60000"
        );
    }
    Ok(())
}

/// Runtime-context cutover invariant.
///
/// Runtime context is the only inner-loop harness. Reject legacy taskplan
/// helper operation/profile names that would imply the old strategy mode.
fn enforce_runtime_context_config_invariant(config: &MagicianConfig) -> Result<()> {
    let task_state_policy = config.task_state.policy.trim();
    if task_state_policy != "llm_lazy_optional" {
        return Err(anyhow::anyhow!(
            "unsupported task_state.policy `{task_state_policy}`; expected `llm_lazy_optional`"
        ));
    }
    let revisions = &config.task_state.revisions;
    if revisions.bump_on_runtime_ledger_change
        || revisions.bump_on_execution_history_change
        || revisions.bump_on_artifact_change
        || revisions.bump_on_prompt_projection_change
    {
        return Err(anyhow::anyhow!(
            "task_state.revisions may not bump on runtime ledger, execution history, artifact, or prompt projection churn"
        ));
    }
    if revisions.format.trim() != "structured_json" {
        return Err(anyhow::anyhow!(
            "unsupported task_state.revisions.format `{}`; expected `structured_json`",
            revisions.format
        ));
    }
    let Some(router) = config.llm.router.as_ref() else {
        return Ok(());
    };

    let legacy_operations: Vec<&str> = LEGACY_TASKPLAN_OPERATIONS
        .iter()
        .copied()
        .filter(|operation| router.operation_mapping.contains_key(*operation))
        .collect();
    if !legacy_operations.is_empty() {
        return Err(anyhow::anyhow!(
            "legacy taskplan operation mappings are not supported in runtime-context mode: {}",
            legacy_operations.join(", ")
        ));
    }

    let legacy_profiles: Vec<String> = router
        .profiles
        .keys()
        .filter(|profile| profile.starts_with(LEGACY_TASKPLAN_PROFILE_PREFIX))
        .cloned()
        .collect();
    if !legacy_profiles.is_empty() {
        return Err(anyhow::anyhow!(
            "legacy taskplan LLM profile ids are not supported; rename to `llm-durable-state-*`: {}",
            legacy_profiles.join(", ")
        ));
    }

    let legacy_profile_refs: Vec<String> = router
        .operation_mapping
        .iter()
        .filter_map(|(operation, selector)| {
            // Selector resolves to one, two, or three profile names; flag
            // the operation if any branch references a legacy taskplan
            // profile.
            let mut profiles: Vec<&str> = vec![selector.default_profile()];
            if let magicllm::config::OperationProfileSelector::Conditional {
                when_has_images,
                when_cloud,
                ..
            } = selector
            {
                if let Some(alt) = when_has_images.as_deref() {
                    profiles.push(alt);
                }
                if let Some(cloud) = when_cloud.as_deref() {
                    profiles.push(cloud);
                }
            }
            profiles
                .iter()
                .find(|profile| profile.starts_with(LEGACY_TASKPLAN_PROFILE_PREFIX))
                .map(|profile| format!("{operation}->{profile}"))
        })
        .collect();
    if !legacy_profile_refs.is_empty() {
        return Err(anyhow::anyhow!(
            "operation mappings may not reference legacy taskplan profiles: {}",
            legacy_profile_refs.join(", ")
        ));
    }

    Ok(())
}

/// Hard invariant: every native-execution LLM profile must declare
/// `supports_tool_calling: true`.
///
/// The execution path is unconditionally native tool calling. Profiles that do
/// not advertise tool-calling support cannot participate. Subscription-backed
/// Harness-CLI provider profiles are one-shot operation calls, not execution
/// profiles; they remain explicitly text-only. The exemption is bound to the
/// physical provider, not a profile-name prefix, so an ordinary profile cannot
/// disable native tool calling merely by naming itself `op-harness-*`.
fn is_text_only_harness_profile(profile: &LLMProfile) -> bool {
    matches!(
        &profile.provider,
        LLMProviderKind::Custom(provider) if provider.starts_with("harness-")
    )
}

fn enforce_tool_calling_invariant(config: &MagicianConfig) -> Result<()> {
    let Some(router) = config.llm.router.as_ref() else {
        return Ok(());
    };
    let offenders: Vec<String> = router
        .profiles
        .iter()
        .filter(|(_, profile)| {
            !is_text_only_harness_profile(profile) && profile.supports_tool_calling != Some(true)
        })
        .map(|(name, _)| name.clone())
        .collect();
    if !offenders.is_empty() {
        return Err(anyhow::anyhow!(
            "All native-execution LLM profiles must declare `supports_tool_calling: true` (native tool calling is the only execution path). Offending profiles: {}",
            offenders.join(", ")
        ));
    }
    Ok(())
}

fn enforce_coding_config_invariant(config: &MagicianConfig) -> Result<()> {
    let coding = &config.coding;
    let mut ids = BTreeSet::new();
    for profile in &coding.profiles {
        let id = profile.id.trim();
        if id.is_empty() {
            return Err(anyhow::anyhow!(
                "coding.profiles entries must declare a non-empty id"
            ));
        }
        if !ids.insert(id.to_string()) {
            return Err(anyhow::anyhow!("duplicate coding profile id `{id}`"));
        }
        let llm_profile = profile.llm_profile.trim();
        if llm_profile.is_empty() {
            return Err(anyhow::anyhow!(
                "coding profile `{id}` must declare a non-empty llm_profile"
            ));
        }
        let Some(router) = config.router_config() else {
            return Err(anyhow::anyhow!(
                "coding profile `{id}` references `{llm_profile}`, but llm.router.profiles is empty"
            ));
        };
        if router.resolve_profile(llm_profile).is_none() {
            return Err(anyhow::anyhow!(
                "coding profile `{id}` references missing llm profile `{llm_profile}`"
            ));
        }
    }
    if let Some(default_profile) = coding
        .default_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let exists = coding
            .profiles
            .iter()
            .any(|profile| profile.enabled && profile.id.trim() == default_profile);
        if !exists {
            return Err(anyhow::anyhow!(
                "coding.default_profile `{default_profile}` must reference an enabled coding profile id"
            ));
        }
    }
    Ok(())
}

fn enforce_memory_config_invariant(config: &MagicianConfig) -> Result<()> {
    let snapshot = &config.memory.prompt_snapshot;
    if snapshot.enabled {
        if !(1024 * 1024..=1024 * 1024 * 1024).contains(&snapshot.max_bytes) {
            return Err(anyhow::anyhow!(
                "memory.prompt_snapshot.max_bytes must be between 1048576 and 1073741824"
            ));
        }
        if !(1..=64).contains(&snapshot.max_entries) {
            return Err(anyhow::anyhow!(
                "memory.prompt_snapshot.max_entries must be between 1 and 64"
            ));
        }
        if !(1..=86_400).contains(&snapshot.idle_ttl_secs) {
            return Err(anyhow::anyhow!(
                "memory.prompt_snapshot.idle_ttl_secs must be between 1 and 86400"
            ));
        }
        if !(1..=60_000).contains(&snapshot.refresh_debounce_ms) {
            return Err(anyhow::anyhow!(
                "memory.prompt_snapshot.refresh_debounce_ms must be between 1 and 60000"
            ));
        }
    }

    for (scope, budget) in [
        ("user", &config.memory.prompt_scope_budgets.user),
        ("agent", &config.memory.prompt_scope_budgets.agent),
        ("agent_goal", &config.memory.prompt_scope_budgets.agent_goal),
    ] {
        if budget.max_entries.is_none() && budget.max_chars.is_none() {
            return Err(anyhow::anyhow!(
                "memory.prompt_scope_budgets.{scope} must set max_entries or max_chars"
            ));
        }
        if matches!(budget.max_chars, Some(1..=255)) {
            return Err(anyhow::anyhow!(
                "memory.prompt_scope_budgets.{scope}.max_chars must be 0 or at least 256"
            ));
        }
    }

    for (scope, lanes) in [
        ("user", &config.memory.prompt_lane_budgets.user),
        ("agent", &config.memory.prompt_lane_budgets.agent),
        ("agent_goal", &config.memory.prompt_lane_budgets.agent_goal),
    ] {
        for (lane, budget) in lanes {
            let lane = lane.trim();
            if !memory_lane_name_is_valid(lane) {
                return Err(anyhow::anyhow!(
                    "memory.prompt_lane_budgets.{scope} contains unknown lane `{lane}`"
                ));
            }
            if budget.max_entries.is_none() && budget.max_chars.is_none() {
                return Err(anyhow::anyhow!(
                    "memory.prompt_lane_budgets.{scope}.{lane} must set max_entries or max_chars"
                ));
            }
            if matches!(budget.max_chars, Some(1..=255)) {
                return Err(anyhow::anyhow!(
                    "memory.prompt_lane_budgets.{scope}.{lane}.max_chars must be 0 or at least 256"
                ));
            }
        }
    }

    // The rule lives with the loader that interprets these settings; config
    // load is where it is enforced, because a note path the provider can
    // never resolve would otherwise surface only as a permanently missing
    // profile.
    config
        .memory
        .taste_profile
        .validate()
        .map_err(|message| anyhow::anyhow!(message))?;

    Ok(())
}

fn enforce_public_chat_config_invariant(config: &MagicianConfig) -> Result<()> {
    let Some(policy) = config.public_chat.kapso_envoy_chat.as_ref() else {
        return Ok(());
    };
    if !policy.enabled {
        return Ok(());
    }

    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.source_surface",
        &policy.source_surface,
    )?;
    let mut source_surfaces = std::collections::HashSet::new();
    let mut configured_source_surfaces = vec![policy.source_surface.as_str()];
    configured_source_surfaces.extend(policy.source_surfaces.iter().map(String::as_str));
    for source_surface in configured_source_surfaces {
        let source_surface = source_surface.trim();
        validate_non_empty_config_string(
            "public_chat.kapso_envoy_chat.source_surfaces[]",
            source_surface,
        )?;
        if !source_surfaces.insert(source_surface.to_string()) {
            return Err(anyhow::anyhow!(
                "public_chat.kapso_envoy_chat.source_surfaces contains duplicate source surface: {source_surface}"
            ));
        }
    }
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.paid_operation",
        &policy.paid_operation,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.fallback_operation",
        &policy.fallback_operation,
    )?;
    require_operation_mapping(
        config,
        "public_chat.kapso_envoy_chat.paid_operation",
        &policy.paid_operation,
    )?;
    require_operation_mapping(
        config,
        "public_chat.kapso_envoy_chat.fallback_operation",
        &policy.fallback_operation,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.max_paid_concurrent",
        policy.max_paid_concurrent,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.max_fallback_concurrent",
        policy.max_fallback_concurrent,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.max_global_queue_depth",
        policy.max_global_queue_depth,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.max_per_sender_queue_depth",
        policy.max_per_sender_queue_depth,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.coalesce_window_ms",
        policy.coalesce_window_ms,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.max_queue_wait_ms",
        policy.max_queue_wait_ms,
    )?;
    validate_public_chat_daily_paid_limit(&policy.daily_paid_limit)?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.queued_reply",
        &policy.queued_reply,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.overload_reply",
        &policy.overload_reply,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.daily_fallback_reply",
        &policy.daily_fallback_reply,
    )?;

    if let Some(first_contact) = policy.first_contact.as_ref() {
        validate_public_chat_first_contact_policy(config, first_contact)?;
    }
    if let Some(identity_research) = policy.identity_research.as_ref() {
        validate_public_chat_identity_research_policy(identity_research)?;
    }

    Ok(())
}

fn validate_public_chat_daily_paid_limit(limit: &PublicChatDailyPaidLimitConfig) -> Result<()> {
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.daily_paid_limit.max_calls",
        limit.max_calls,
    )?;
    validate_positive_f64(
        "public_chat.kapso_envoy_chat.daily_paid_limit.max_cost_usd",
        limit.max_cost_usd,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.daily_paid_limit.max_tokens",
        limit.max_tokens,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.daily_paid_limit.reset_timezone",
        &limit.reset_timezone,
    )?;
    validate_timezone(
        "public_chat.kapso_envoy_chat.daily_paid_limit.reset_timezone",
        &limit.reset_timezone,
    )?;
    Ok(())
}

fn validate_public_chat_first_contact_policy(
    config: &MagicianConfig,
    policy: &PublicChatFirstContactPolicyConfig,
) -> Result<()> {
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.first_contact.max_paid_concurrent",
        policy.max_paid_concurrent,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.first_contact.max_per_sender_per_day",
        policy.max_per_sender_per_day,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.first_contact.debounce_window_ms",
        policy.debounce_window_ms,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.first_contact.max_debounce_wait_ms",
        policy.max_debounce_wait_ms,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.first_contact.min_response_delay_ms",
        policy.min_response_delay_ms,
    )?;
    if policy.min_response_delay_ms > policy.debounce_window_ms {
        return Err(anyhow::anyhow!(
            "public_chat.kapso_envoy_chat.first_contact.min_response_delay_ms must be <= debounce_window_ms"
        ));
    }
    if policy.debounce_window_ms > policy.max_debounce_wait_ms {
        return Err(anyhow::anyhow!(
            "public_chat.kapso_envoy_chat.first_contact.debounce_window_ms must be <= max_debounce_wait_ms"
        ));
    }
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.first_contact.prompt",
        &policy.prompt,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.first_contact.conversation_style",
        &policy.conversation_style,
    )?;
    if policy.llm_enabled {
        validate_non_empty_config_string(
            "public_chat.kapso_envoy_chat.first_contact.operation",
            &policy.operation,
        )?;
        require_operation_mapping(
            config,
            "public_chat.kapso_envoy_chat.first_contact.operation",
            &policy.operation,
        )?;
    }
    Ok(())
}

fn validate_public_chat_identity_research_policy(
    policy: &PublicChatIdentityResearchPolicyConfig,
) -> Result<()> {
    if !policy.enabled {
        return Ok(());
    }
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.identity_research.worker_agent_id",
        &policy.worker_agent_id,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.identity_research.max_concurrent",
        policy.max_concurrent,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.identity_research.max_global_queue_depth",
        policy.max_global_queue_depth,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.identity_research.max_per_sender_queue_depth",
        policy.max_per_sender_queue_depth,
    )?;
    validate_positive_usize(
        "public_chat.kapso_envoy_chat.identity_research.max_per_sender_per_day",
        policy.max_per_sender_per_day,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.identity_research.coalesce_window_ms",
        policy.coalesce_window_ms,
    )?;
    validate_positive_u64(
        "public_chat.kapso_envoy_chat.identity_research.daily_limit.max_jobs",
        policy.daily_limit.max_jobs,
    )?;
    validate_positive_f64(
        "public_chat.kapso_envoy_chat.identity_research.daily_limit.max_cost_usd",
        policy.daily_limit.max_cost_usd,
    )?;
    validate_non_empty_config_string(
        "public_chat.kapso_envoy_chat.identity_research.daily_limit.reset_timezone",
        &policy.daily_limit.reset_timezone,
    )?;
    validate_timezone(
        "public_chat.kapso_envoy_chat.identity_research.daily_limit.reset_timezone",
        &policy.daily_limit.reset_timezone,
    )?;
    if !policy.min_details.claimed_name_or_org && !policy.min_details.purpose {
        return Err(anyhow::anyhow!(
            "public_chat.kapso_envoy_chat.identity_research.min_details must require at least one detail"
        ));
    }
    Ok(())
}

fn require_operation_mapping(config: &MagicianConfig, path: &str, operation: &str) -> Result<()> {
    let Some(router) = config.llm.router.as_ref() else {
        return Err(anyhow::anyhow!(
            "{path} references `{operation}`, but llm.router is not configured"
        ));
    };
    if !router.operation_mapping.contains_key(operation) {
        return Err(anyhow::anyhow!(
            "{path} references missing llm.router.operation_mapping `{operation}`"
        ));
    }
    Ok(())
}

fn validate_non_empty_config_string(path: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow::anyhow!("{path} must not be empty"));
    }
    Ok(())
}

fn validate_positive_usize(path: &str, value: usize) -> Result<()> {
    if value == 0 {
        return Err(anyhow::anyhow!("{path} must be greater than 0"));
    }
    Ok(())
}

fn validate_positive_u64(path: &str, value: u64) -> Result<()> {
    if value == 0 {
        return Err(anyhow::anyhow!("{path} must be greater than 0"));
    }
    Ok(())
}

fn validate_positive_f64(path: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        return Err(anyhow::anyhow!(
            "{path} must be a finite value greater than 0"
        ));
    }
    Ok(())
}

fn validate_timezone(path: &str, value: &str) -> Result<()> {
    value
        .trim()
        .parse::<chrono_tz::Tz>()
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("{path} must be a valid IANA timezone"))
}

fn memory_lane_name_is_valid(lane: &str) -> bool {
    matches!(
        lane,
        "user_preference"
            | "agent_context"
            | "procedure"
            | "entity"
            | "episode"
            | "environment"
            | "project_context"
            | "source_evidence"
    )
}

pub fn load_default_magician_config() -> Result<MagicianConfig> {
    load_magician_config_from_path(&magician_config_path())
}

fn apply_runtime_ollama_policy(config: &mut MagicianConfig) -> Result<()> {
    let keep_alive = config.resolved_ollama_keep_alive();
    let ollama = config.runtime.ollama.clone();
    let generation_models = resolved_ollama_generation_models(config)?;
    // Empty is a valid steady state under `privacy.processing.mode: cloud`:
    // every mapped operation resolves to its remote arm, no Ollama
    // generation profile remains, and the local model simply goes unused.
    // Rather than deriving a daemon context length from nothing, leave
    // OLLAMA_CONTEXT_LENGTH untouched in that case.
    let daemon_context_tokens = generation_models
        .iter()
        .map(|entry| entry.context_tokens)
        .max();
    // Keep the commit phase failure-atomic too: finish the only fallible config
    // normalization before changing environment variables or process globals.
    normalize_runtime_ollama_policy(config, keep_alive.clone())?;
    if std::env::var_os("OLLAMA_MAX_LOADED_MODELS").is_none() {
        std::env::set_var(
            "OLLAMA_MAX_LOADED_MODELS",
            ollama.max_loaded_models.to_string(),
        );
    }
    if let Some(daemon_context_tokens) = daemon_context_tokens {
        if std::env::var_os("OLLAMA_CONTEXT_LENGTH").is_none() {
            std::env::set_var("OLLAMA_CONTEXT_LENGTH", daemon_context_tokens.to_string());
        }
    }
    if std::env::var_os("OLLAMA_KV_CACHE_TYPE").is_none() {
        std::env::set_var("OLLAMA_KV_CACHE_TYPE", &ollama.kv_cache_type);
    }
    if std::env::var_os("OLLAMA_FLASH_ATTENTION").is_none() {
        std::env::set_var(
            "OLLAMA_FLASH_ATTENTION",
            if ollama.flash_attention { "1" } else { "0" },
        );
    }
    magicllm::set_default_ollama_keep_alive(keep_alive.clone());
    magician_vector_index::set_default_ollama_keep_alive(keep_alive.clone());
    magician_vector_index::set_default_ollama_embedding_policy(
        Some(ollama.embedding_base_url.clone()),
        Some(ollama.embedding_keep_alive.clone()),
        Some(ollama.embedding_num_parallel),
        Some(ollama.embedding_max_loaded_models),
        Some(ollama.embedding_query_timeout_ms),
        Some(ollama.embedding_write_timeout_ms),
        Some(ollama.embedding_model.clone()),
        Some(ollama.embedding_context_tokens),
        Some(ollama.embedding_batch_tokens),
        Some(ollama.embedding_batch_size),
        Some(ollama.embedding_dimensions),
    );
    crate::magician_v2::media_seam::meeting::summarizer::set_default_ollama_keep_alive(
        keep_alive.clone(),
    );
    magician_vector_index::install_hybrid_result_cache(
        magician_vector_index::HybridResultCacheSettings {
            enabled: config.runtime.retrieval.result_cache.enabled,
            max_entries: config.runtime.retrieval.result_cache.max_entries,
            max_bytes: config.runtime.retrieval.result_cache.max_bytes,
        },
    );
    magician_vector_index::install_lance_table_pool(
        magician_vector_index::LanceTablePoolSettings {
            enabled: config.runtime.retrieval.lance_table_pool.enabled,
            max_idle: config.runtime.retrieval.lance_table_pool.max_idle,
        },
    );
    magician_vector_index::install_query_vector_cache(
        magician_vector_index::QueryVectorCacheSettings {
            enabled: config.runtime.retrieval.query_vector_cache.enabled,
            max_entries: config.runtime.retrieval.query_vector_cache.max_entries,
            max_bytes: config.runtime.retrieval.query_vector_cache.max_bytes,
        },
    );
    magician_vector_index::install_vector_search(magician_vector_index::VectorSearchSettings {
        mode: config.runtime.retrieval.vector_search.as_vector_index(),
        min_rows: config.runtime.retrieval.ann.min_rows,
        candidate_multiplier: config.runtime.retrieval.ann.candidate_multiplier,
        nprobes: magician_vector_index::DEFAULT_VECTOR_SEARCH_NPROBES,
    });
    magician_vector_index::install_query_embed_batch(
        magician_vector_index::QueryEmbedBatchSettings {
            window_ms: config.runtime.ollama.embedding_query_batch_window_ms,
            max_items: config.runtime.ollama.embedding_query_batch_max_items,
            max_chars: config.runtime.ollama.embedding_query_batch_max_chars,
        },
    );
    Ok(())
}

fn normalize_runtime_ollama_policy(
    config: &mut MagicianConfig,
    keep_alive: Option<String>,
) -> Result<()> {
    if config.llm.dispatch.local_prep.keep_alive.is_none() {
        config.llm.dispatch.local_prep.keep_alive = keep_alive.clone();
    }
    let Some(router) = config.llm.router.as_mut() else {
        if config.llm.dispatch.local_prep.enabled {
            return Err(anyhow::anyhow!(
                "llm.dispatch.local_prep requires llm.router configuration"
            ));
        }
        return Ok(());
    };

    // Cloud locality: local pre-summarisation exists to shrink a payload
    // *before* it reaches an expensive remote model, using the local model —
    // with the local model deliberately unused there is nothing to
    // pre-summarise with. Disable local prep and skip its Ollama validation;
    // this is the one part of the locality switch that is a load-time rule
    // rather than a runtime selector (switching back re-enables from YAML).
    let locality_skips_local_prep = router.locality == magicllm::ProcessingLocality::Cloud;
    if locality_skips_local_prep {
        config.llm.dispatch.local_prep.enabled = false;
        // Unmap: local pre-summarisation has no local model to run with in
        // cloud mode, and leaving the arm mapped would keep the on-device
        // generation model resident, defeating the residency goal. The YAML
        // entry is untouched — the next local-mode load restores it.
        router.operation_mapping.remove("local_prep");
    } else if let Some(operation) = config.llm.dispatch.local_prep.operation.as_deref() {
        let profile_name = router
            .operation_mapping
            .get(operation)
            .map(OperationProfileSelector::default_profile)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "llm.dispatch.local_prep.operation references missing operation mapping `{operation}`"
                )
            })?;
        let profile = router.get_profile_for_operation(operation).ok_or_else(|| {
            anyhow::anyhow!(
                "llm.dispatch.local_prep.operation `{operation}` resolves missing profile `{profile_name}`"
            )
        })?;
        if profile.provider != LLMProviderKind::Ollama {
            return Err(anyhow::anyhow!(
                "llm.dispatch.local_prep.operation `{operation}` must resolve to the ollama provider"
            ));
        }
        let context_tokens = ollama_profile_context_tokens(profile_name, profile)?;
        let base_url = profile
            .api_base_url
            .as_deref()
            .map(ollama_host_from_endpoint)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "llm.dispatch.local_prep.operation `{operation}` resolves profile `{profile_name}` without api_base_url"
                )
            })?;
        config.llm.dispatch.local_prep.model = profile.model.clone();
        config.llm.dispatch.local_prep.base_url = base_url;
        config.llm.dispatch.local_prep.context_tokens = context_tokens;
    } else if !locality_skips_local_prep && config.llm.dispatch.local_prep.enabled {
        return Err(anyhow::anyhow!(
            "llm.dispatch.local_prep.operation is required when local prep is enabled"
        ));
    }

    for profile in router.profiles.values_mut() {
        if profile.provider != LLMProviderKind::Ollama {
            continue;
        }
        let metadata = profile.metadata.get_or_insert_with(Default::default);
        if let Some(keep_alive) = keep_alive.as_ref() {
            metadata
                .entry("keep_alive".to_string())
                .or_insert_with(|| Value::String(keep_alive.clone()));
        }
    }
    Ok(())
}

fn ollama_host_from_endpoint(endpoint: &str) -> String {
    endpoint
        .trim()
        .trim_end_matches('/')
        .strip_suffix("/api/generate")
        .unwrap_or_else(|| endpoint.trim().trim_end_matches('/'))
        .to_string()
}

fn ollama_endpoints_share_listener(left: &str, right: &str) -> bool {
    let Ok(left) = url::Url::parse(left) else {
        return false;
    };
    let Ok(right) = url::Url::parse(right) else {
        return false;
    };
    let left_host = left.host_str().unwrap_or_default();
    let right_host = right.host_str().unwrap_or_default();
    let same_host = left_host.eq_ignore_ascii_case(right_host)
        || (matches!(left_host, "localhost" | "127.0.0.1" | "::1")
            && matches!(right_host, "localhost" | "127.0.0.1" | "::1"));
    same_host && left.port_or_known_default() == right.port_or_known_default()
}

fn ollama_profile_context_tokens(profile_name: &str, profile: &LLMProfile) -> Result<u32> {
    let context = profile
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("options"))
        .and_then(Value::as_object)
        .and_then(|options| options.get("num_ctx"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Ollama profile `{profile_name}` must configure metadata.options.num_ctx"
            )
        })?;
    if !(2_048..=1_048_576).contains(&context) {
        return Err(anyhow::anyhow!(
            "Ollama profile `{profile_name}` metadata.options.num_ctx must be between 2048 and 1048576"
        ));
    }
    Ok(context)
}

pub fn resolved_ollama_generation_models(
    config: &MagicianConfig,
) -> Result<Vec<OllamaGenerationModelConfig>> {
    let router = config
        .llm
        .router
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("llm.router is required to resolve Ollama models"))?;
    let mut mapped_profile_names = BTreeSet::new();
    for selector in router.operation_mapping.values() {
        // Locality-effective arms only: under `privacy.processing.mode:
        // cloud` the `when_cloud` arm serves each mapping, so the local arm
        // of a fully-remote mapping never keeps the model resident.
        mapped_profile_names.insert(
            selector.profile_for_locality(&magicllm::config::RequestShape::NONE, router.locality),
        );
        if let OperationProfileSelector::Conditional {
            when_has_images: Some(name),
            ..
        } = selector
        {
            mapped_profile_names.insert(name.as_str());
        }
    }

    let mut concrete_profile_names = BTreeSet::new();
    for name in mapped_profile_names {
        if router.profiles.contains_key(name) {
            concrete_profile_names.insert(name);
        } else if let Some(adaptive) = router.adaptive_profiles.get(name) {
            for concrete_name in [
                adaptive.fast_profile.as_str(),
                adaptive.thinking_profile.as_str(),
            ] {
                if !router.profiles.contains_key(concrete_name) {
                    return Err(anyhow::anyhow!(
                        "mapped adaptive profile `{name}` references missing concrete profile `{concrete_name}`"
                    ));
                }
                concrete_profile_names.insert(concrete_name);
            }
        } else {
            return Err(anyhow::anyhow!(
                "operation mapping references missing profile `{name}`"
            ));
        }
    }

    let mut contexts_by_model = BTreeMap::<String, u32>::new();
    for profile_name in concrete_profile_names {
        let Some(profile) = router.profiles.get(profile_name) else {
            continue;
        };
        if profile.provider != LLMProviderKind::Ollama {
            continue;
        }
        // The routed embedding profile serves vector generation, not text
        // generation: it lives on the dedicated embedding daemon and must not
        // join the prewarm/context-length set (and cannot satisfy the
        // generation num_ctx contract).
        if profile_is_embedding_profile(profile) {
            continue;
        }
        let model = profile.model.trim();
        if model.is_empty() {
            return Err(anyhow::anyhow!(
                "mapped Ollama profile `{profile_name}` has an empty model"
            ));
        }
        let context_tokens = ollama_profile_context_tokens(profile_name, profile)?;
        contexts_by_model
            .entry(model.to_string())
            .and_modify(|current| *current = (*current).max(context_tokens))
            .or_insert(context_tokens);
    }

    Ok(contexts_by_model
        .into_iter()
        .map(|(model, context_tokens)| OllamaGenerationModelConfig {
            model,
            context_tokens,
        })
        .collect())
}

fn enforce_runtime_retrieval_config_invariant(config: &MagicianConfig) -> Result<()> {
    let cache = &config.runtime.retrieval.result_cache;
    if cache.max_entries == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.result_cache.max_entries must be greater than 0"
        ));
    }
    if cache.max_bytes == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.result_cache.max_bytes must be greater than 0"
        ));
    }
    if config.runtime.retrieval.lance_table_pool.max_idle == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.lance_table_pool.max_idle must be greater than 0"
        ));
    }
    let vectors = &config.runtime.retrieval.query_vector_cache;
    if vectors.max_entries == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.query_vector_cache.max_entries must be greater than 0"
        ));
    }
    if vectors.max_bytes == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.query_vector_cache.max_bytes must be greater than 0"
        ));
    }
    let ann = &config.runtime.retrieval.ann;
    if ann.min_rows == 0 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.ann.min_rows must be greater than 0"
        ));
    }
    if ann.candidate_multiplier < 1 {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.ann.candidate_multiplier must be at least 1"
        ));
    }
    if ann.candidate_multiplier > MAX_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER {
        return Err(anyhow::anyhow!(
            "runtime.retrieval.ann.candidate_multiplier must be at most {MAX_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER}"
        ));
    }
    Ok(())
}

fn enforce_dispatch_config_invariant(config: &MagicianConfig) -> Result<()> {
    let dispatch = &config.llm.dispatch;
    match dispatch.engine {
        magicllm::dispatch::DispatchEngine::LegacyWorkerPool
        | magicllm::dispatch::DispatchEngine::ProviderIsolated => {},
    }
    magicllm::dispatch::DispatchCapacityPlan::from_config(dispatch)
        .validate()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

fn enforce_runtime_ollama_config_invariant(config: &MagicianConfig) -> Result<()> {
    let ollama = &config.runtime.ollama;
    if !(1..=16).contains(&ollama.max_loaded_models) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.max_loaded_models must be between 1 and 16"
        ));
    }
    if ollama.embedding_num_parallel != 1 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_num_parallel must be 1 until the dedicated runner verifies multi-sequence embedding execution"
        ));
    }
    if ollama.embedding_max_loaded_models != 1 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_max_loaded_models must be 1 for the dedicated embedding daemon"
        ));
    }
    if !(100..=60_000).contains(&ollama.embedding_query_timeout_ms) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_query_timeout_ms must be between 100 and 60000"
        ));
    }
    if ollama.embedding_query_batch_window_ms > 50 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_query_batch_window_ms must be between 0 and 50"
        ));
    }
    if ollama.embedding_query_batch_max_items == 0 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_query_batch_max_items must be greater than 0"
        ));
    }
    if ollama.embedding_query_batch_max_chars == 0 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_query_batch_max_chars must be greater than 0"
        ));
    }
    if !(1_000..=900_000).contains(&ollama.embedding_write_timeout_ms) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_write_timeout_ms must be between 1000 and 900000"
        ));
    }
    let embedding_base_url = ollama.embedding_base_url.trim();
    if !(embedding_base_url.starts_with("http://") || embedding_base_url.starts_with("https://")) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_base_url must be an http(s) URL"
        ));
    }
    if let Some(router) = config.llm.router.as_ref() {
        for (profile_name, profile) in &router.profiles {
            if profile.provider != LLMProviderKind::Ollama {
                continue;
            }
            // The routed embedding profile lives ON the embedding daemon by
            // design (`op-embedding-local` mirrors runtime.ollama.embedding_*).
            // The separation this invariant enforces is between the embedding
            // daemon and GENERATION profiles.
            if profile_is_embedding_profile(profile) {
                continue;
            }
            if let Some(generation_endpoint) = profile.api_base_url.as_deref() {
                if ollama_endpoints_share_listener(embedding_base_url, generation_endpoint) {
                    return Err(anyhow::anyhow!(
                        "runtime.ollama.embedding_base_url must use a different listener from Ollama generation profile `{profile_name}`"
                    ));
                }
            }
        }
    }
    if ollama.embedding_keep_alive.trim() != "-1" {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_keep_alive must be -1 so retrieval never pays model-load latency"
        ));
    }
    for (path, value) in [(
        "runtime.ollama.embedding_context_tokens",
        ollama.embedding_context_tokens,
    )] {
        if !(2_048..=1_048_576).contains(&value) {
            return Err(anyhow::anyhow!("{path} must be between 2048 and 1048576"));
        }
    }
    if !(32..=8_192).contains(&ollama.embedding_batch_tokens) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_batch_tokens must be between 32 and 8192"
        ));
    }
    if !(1..=256).contains(&ollama.embedding_batch_size) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_batch_size must be between 1 and 256"
        ));
    }
    for (path, value) in [(
        "runtime.ollama.embedding_model",
        ollama.embedding_model.as_str(),
    )] {
        if value.trim().is_empty() {
            return Err(anyhow::anyhow!("{path} must not be empty"));
        }
    }
    if !(1..=65_536).contains(&ollama.embedding_dimensions) {
        return Err(anyhow::anyhow!(
            "runtime.ollama.embedding_dimensions must be between 1 and 65536"
        ));
    }
    let generation_models = resolved_ollama_generation_models(config)?;
    let resident_models = generation_models
        .iter()
        .map(|entry| entry.model.as_str())
        .collect::<BTreeSet<_>>();
    let required_resident_models = resident_models.len();
    if ollama.prewarm && ollama.max_loaded_models < required_resident_models as u32 {
        return Err(anyhow::anyhow!(
            "runtime.ollama.max_loaded_models ({}) is smaller than the {} unique mapped generation models configured for prewarm",
            ollama.max_loaded_models,
            required_resident_models
        ));
    }
    if !matches!(ollama.kv_cache_type.as_str(), "f16" | "q8_0" | "q4_0") {
        return Err(anyhow::anyhow!(
            "runtime.ollama.kv_cache_type must be one of f16, q8_0, or q4_0"
        ));
    }
    Ok(())
}

fn normalize_runtime_ollama_keep_alive(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn env_ollama_keep_alive_override() -> Option<String> {
    ["MAGICIAN_OLLAMA_KEEP_ALIVE", "MAGICLLM_OLLAMA_KEEP_ALIVE"]
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|value| normalize_runtime_ollama_keep_alive(&value))
        })
}

pub fn validate_magician_config_yaml(yaml: &str) -> Result<(), String> {
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|err| format!("invalid YAML: {err}"))?;
    let root = parsed.as_mapping().ok_or_else(|| {
        "magician-config.yaml must contain a YAML mapping at the top level".to_string()
    })?;

    if root.contains_key(serde_yaml::Value::String("bots".to_string())) {
        return Err(
            "top-level `bots:` is no longer supported in magician-config.yaml; move bot runtime config to $MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/bot_configs.yaml (or edit the canonical source at skillshub/bots/bot_configs.yaml and run `make -C skillshub seed-scope SCOPE=<principal>/<workspace>` to redeploy)".to_string(),
        );
    }
    reject_yaml_key(
        root,
        "top-level",
        "execution_strategy",
        "runtime context is mandatory and is not configurable",
    )?;
    reject_yaml_key(
        root,
        "top-level",
        "storage",
        "canonical workspace storage provider is managed by the /workspace-storage/settings API and Web Settings",
    )?;

    reject_retired_taskplan_yaml_keys(root)?;

    Ok(())
}

fn reject_retired_taskplan_yaml_keys(root: &serde_yaml::Mapping) -> Result<(), String> {
    if let Some(task_state) = yaml_mapping_child(root, "task_state") {
        reject_yaml_key(
            task_state,
            "task_state",
            "default_action",
            "task-state actions are model-emitted per turn, not configured as a default switch",
        )?;
    }
    Ok(())
}

fn yaml_mapping_child<'a>(
    mapping: &'a serde_yaml::Mapping,
    key: &str,
) -> Option<&'a serde_yaml::Mapping> {
    mapping
        .get(serde_yaml::Value::String(key.to_string()))?
        .as_mapping()
}

fn reject_yaml_key(
    mapping: &serde_yaml::Mapping,
    path: &str,
    key: &str,
    reason: &str,
) -> Result<(), String> {
    if mapping.contains_key(serde_yaml::Value::String(key.to_string())) {
        return Err(format!(
            "`{path}.{key}` is no longer supported in magician-config.yaml: {reason}"
        ));
    }
    Ok(())
}

impl Default for MagicianConfig {
    fn default() -> Self {
        Self {
            enabled: default_magician_enabled(),
            realtime_events: default_magician_realtime_events(),
            storage_path: default_magician_storage_path(),
            workspace_storage: Default::default(),
            database_maintenance: Default::default(),
            runtime: MagicianRuntimeSettings::default(),
            max_conversations: default_magician_max_conversations(),
            conversation_timeout: default_magician_conversation_timeout(),
            llm: MagicianLlmSettings::default(),
            privacy: PrivacySettings::default(),
            app_platform: AppPlatformSettings::default(),
            analytics: MagicianAnalyticsSettings::default(),
            coding: MagicianCodingSettings::default(),
            verification: VerificationConfig::default(),
            vibedev_deploy: VibeDevDeployConfig::default(),
            memory: MagicianMemorySettings::default(),
            media: MagicianMediaSettings::default(),
            channel_assist: ChannelAssistConfig::default(),
            decision: DecisionHostConfig::default(),
            attention_learning: AttentionLearningConfig::default(),
            resurfacing: ResurfacingConfig::default(),
            social: SocialConfig::default(),
            delivery_hygiene: DeliveryHygieneConfig::default(),
            outcome_maturity: OutcomeMaturityConfig::default(),
            outcome_proposal: OutcomeProposalSettings::default(),
            run_inbox: None,
            content_acquisition:
                crate::magician_v2::content_sources::ContentAcquisitionSettings::default(),
            service_url: None,
            mobile_access: MobileAccessConfig::default(),
            frontend: MagicianFrontendSettings::default(),
            execution: MagicianExecutionSettings::default(),
            agentic: AgenticSettings::default(),
            agent_surface_runtime: AgentSurfaceRuntimeConfig::default(),
            api_mining: ApiMiningConfig::default(),
            capability_evolution: CapabilityEvolutionConfig::default(),
            interactive_process: InteractiveProcessConfig::default(),
            consumer_mode: default_consumer_mode(),
            task_state: TaskStateConfig::default(),
            enrollment: EnrollmentConfig::default(),
            auth: AuthConfig::default(),
            envoy: EnvoyConfig::default(),
            hitl: HitlConfig::default(),
            outward_actions: OutwardActionsConfig::default(),
            approval_envelopes: ApprovalEnvelopesConfig::default(),
            recipient_compliance: RecipientComplianceConfig::default(),
            public_chat: PublicChatConfig::default(),
            chat: MagicianChatTurnSettings::default(),
            tool_authorization: ToolAuthorizationConfig::default(),
            resource_authority: Default::default(),
            harness: HarnessConfig::default(),
            plane: PlaneConfig::default(),
        }
    }
}

/// Auth recovery strategy for the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AuthRecoveryMode {
    /// Wait for next natural browser request to refresh auth passively.
    Passive,
    /// Navigate browser to origin to trigger fresh auth flow.
    Active,
    /// Passive first, active after `auth_max_failures` consecutive failures.
    #[default]
    Hybrid,
}

/// API Mining feature flag and tuning configuration.
///
/// The master ceiling defaults to `true` for backwards-compatible capture;
/// individual legacy capture/mining/replay flags keep their conservative
/// defaults and can still be enabled separately for staged deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMiningConfig {
    /// Master process ceiling for all capture, learning and replay activity.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Capture network traces during browser automation sessions.
    /// When true, observe responses include `networkTraces` field.
    #[serde(default)]
    pub enable_trace_capture: bool,

    /// Run clustering and parameterization on captured traces.
    /// Requires `enable_trace_capture` to have effect.
    #[serde(default)]
    pub enable_mining: bool,

    /// Use learned capabilities for API-first routing.
    /// When true, the executor checks the capability registry before browser actions
    /// and dispatches ApiReplay for Validated/Trusted read-only capabilities.
    #[serde(default)]
    pub enable_replay: bool,

    /// Enable passive XHR/Fetch validation against learned capabilities.
    /// Observed XHR/Fetch traces are compared with matching capabilities
    /// in the background. Validates capabilities for promotion without
    /// replacing browser execution.
    #[serde(default)]
    pub enable_xhr_validation: bool,

    /// Cross-origin, task-shaped browserless replay.
    #[serde(default)]
    pub recipes: RecipesConfig,

    /// Optional periodic verification of learned recipes.
    #[serde(default)]
    pub recipe_verification: RecipeVerificationConfig,

    /// Maximum network traces stored per origin before rotation.
    #[serde(default = "default_max_traces_per_origin")]
    pub max_traces_per_origin: usize,

    /// Maximum capabilities stored per origin.
    #[serde(default = "default_max_capabilities_per_origin")]
    pub max_capabilities_per_origin: usize,

    /// Days to retain raw trace data before cleanup.
    #[serde(default = "default_trace_retention_days")]
    pub trace_retention_days: u32,

    /// URL substring patterns considered safe for POST/PUT/PATCH validation replay.
    /// GET/HEAD are always safe. For other methods, the request URL must contain
    /// at least one of these substrings to be eligible for background validation.
    /// Examples: "/search", "/query", "/sync/", "/graphql", "/lookup"
    #[serde(default)]
    pub xhr_validation_idempotent_patterns: Vec<String>,

    /// Hard cap on passive XHR/Fetch validations per mining pipeline run.
    /// Protects against a single trace drain emitting too many background
    /// validation requests. The pass still observes and counts skipped traces
    /// after the budget is exhausted.
    #[serde(default = "default_xhr_validation_max_per_pipeline")]
    pub xhr_validation_max_per_pipeline: usize,

    /// Domain+path patterns to whitelist from noise filtering.
    /// Each entry is matched as a substring against "host/path" of the URL.
    /// If a URL's host+path contains any of these substrings, it will NOT be
    /// filtered as noise even if it matches a noise host or path pattern.
    /// Use domain+path combos to scope overrides precisely.
    /// Examples: "myapp.com/collect", "internal.corp/tracking/orders"
    #[serde(default)]
    pub noise_filter_whitelist: Vec<String>,

    /// Additional domain+path patterns to blacklist as noise.
    /// Each entry is matched as a substring against "host/path" of the URL.
    /// Use this to block site-specific telemetry that the built-in list doesn't cover.
    /// Checked AFTER the whitelist — a whitelisted URL won't be blocked even if
    /// it also matches a blacklist entry.
    /// Examples: "internal-analytics.corp.com", "myapp.com/telemetry"
    #[serde(default)]
    pub noise_filter_blacklist: Vec<String>,

    /// Auth recovery mode: passive, active, or hybrid.
    #[serde(default)]
    pub auth_recovery_mode: AuthRecoveryMode,

    /// Fallback TTL for opaque tokens in seconds (JWTs use decoded exp claim).
    #[serde(default = "default_auth_ttl_secs")]
    pub auth_ttl_secs: i64,

    /// Consecutive auth failures before falling back to active recovery (hybrid mode).
    #[serde(default = "default_auth_max_failures")]
    pub auth_max_failures: usize,

    /// Maximum origins stored in captured auth state before LRU eviction.
    #[serde(default = "default_max_vault_origins")]
    pub max_vault_origins: usize,

    /// Per-HTTP-method sample-count threshold for promoting a
    /// capability from `Observed` to `Candidate` (the "takeover
    /// ready" state where the router engages on the next visit).
    /// Lower thresholds for read-only methods (GET / HEAD) so the
    /// system can replay on the SECOND visit instead of the third;
    /// side-effecting methods stay conservative (explicit opt-in
    /// required per origin via a future config knob, defaults to
    /// "never auto-takeover").
    ///
    /// Methods not listed fall back to `takeover_default_min_samples`.
    /// Setting an entry to 0 or 1 means the router engages
    /// immediately on first observation — useful only for known-safe
    /// idempotent endpoints.
    #[serde(default = "default_takeover_min_samples_per_method")]
    pub takeover_min_samples_per_method: std::collections::HashMap<String, usize>,

    /// Fallback `takeover_min_samples_per_method` value used for HTTP
    /// methods not explicitly listed.
    #[serde(default = "default_takeover_default_min_samples")]
    pub takeover_default_min_samples: usize,

    /// Inline auto-replay validation during mining (cold-start fix).
    ///
    /// Candidate capabilities accumulate `replay_success_count` /
    /// `replay_failure_count` only when the router actually replays
    /// them — but the router only replays at Validated+. This config
    /// block enables a small number of safety-gated replays inline
    /// during mining so Candidates can cross the Validated threshold
    /// without operator intervention. See
    /// `api_mining::auto_replay::SafetyFilter` for the gates.
    #[serde(default)]
    pub auto_replay_validation: AutoReplayConfig,
}

impl Default for ApiMiningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_trace_capture: false,
            enable_mining: false,
            enable_replay: false,
            enable_xhr_validation: false,
            recipes: RecipesConfig::default(),
            recipe_verification: RecipeVerificationConfig::default(),
            max_traces_per_origin: default_max_traces_per_origin(),
            max_capabilities_per_origin: default_max_capabilities_per_origin(),
            trace_retention_days: default_trace_retention_days(),
            xhr_validation_idempotent_patterns: Vec::new(),
            xhr_validation_max_per_pipeline: default_xhr_validation_max_per_pipeline(),
            noise_filter_whitelist: Vec::new(),
            noise_filter_blacklist: Vec::new(),
            auth_recovery_mode: AuthRecoveryMode::default(),
            auth_ttl_secs: default_auth_ttl_secs(),
            auth_max_failures: default_auth_max_failures(),
            max_vault_origins: default_max_vault_origins(),
            takeover_min_samples_per_method: default_takeover_min_samples_per_method(),
            takeover_default_min_samples: default_takeover_default_min_samples(),
            auto_replay_validation: AutoReplayConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub compile_after_first_run: bool,
    #[serde(default = "default_recipe_match_confirm_threshold")]
    pub match_confirm_threshold: f32,
    #[serde(default = "default_recipe_unknown_origin_default")]
    pub unknown_origin_default: String,
    #[serde(default = "default_recipe_transport_ladder")]
    pub transport_ladder: Vec<String>,
}

impl Default for RecipesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compile_after_first_run: true,
            match_confirm_threshold: default_recipe_match_confirm_threshold(),
            unknown_origin_default: default_recipe_unknown_origin_default(),
            transport_ladder: default_recipe_transport_ladder(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeVerificationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_recipe_verification_interval_secs")]
    pub interval_secs: u64,
}

impl Default for RecipeVerificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_recipe_verification_interval_secs(),
        }
    }
}

fn default_recipe_match_confirm_threshold() -> f32 {
    0.85
}

fn default_recipe_unknown_origin_default() -> String {
    "reads_auto_writes_by_grant".into()
}

fn default_recipe_transport_ladder() -> Vec<String> {
    vec!["reqwest".into(), "in_page_fetch".into(), "browser".into()]
}

fn default_recipe_verification_interval_secs() -> u64 {
    21_600
}

/// Developer-Mode `interactive_process` policy.
///
/// Currently exposes the operator-facing Workbench CLI catalog plus
/// per-program concurrency caps for those CLIs. Future fields will land
/// here (auto-close-on-idle, default rows/cols, etc.).
///
/// See `docs/plans/2026-05-13-developer-mode-workbench.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveProcessConfig {
    /// Programs exposed in the Developer Mode / VibeDev Workbench CLI
    /// launcher. This is intentionally config-owned so neither the UI nor
    /// the backend code owns a baked-in runtime catalog. Pi should not be
    /// listed here: it is an internal coding-engine adapter, not an
    /// operator CLI.
    #[serde(default)]
    pub operator_cli_programs: Vec<String>,
    /// Maximum simultaneous live sessions per `program` name across
    /// the entire process. A missing entry means no limit. Defaults
    /// hold the four operator CLIs at 1 each — these consume
    /// scarce license / quota slots that don't multiplex well.
    /// Operators can raise / lower in `magician-config.yaml`.
    #[serde(default = "default_interactive_process_concurrency_limits")]
    pub max_concurrent_per_program: std::collections::HashMap<String, usize>,
}

impl Default for InteractiveProcessConfig {
    fn default() -> Self {
        Self {
            operator_cli_programs: Vec::new(),
            max_concurrent_per_program: default_interactive_process_concurrency_limits(),
        }
    }
}

fn default_interactive_process_concurrency_limits() -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    map.insert("claude".to_string(), 1);
    map.insert("codex".to_string(), 1);
    map.insert("agy".to_string(), 1);
    map.insert("opencode".to_string(), 1);
    map
}

/// Inline auto-replay validation. See `ApiMiningConfig::auto_replay_validation`.
///
/// Defaults are deliberately conservative: enabled but in dry-run mode,
/// only GET/HEAD allowed, small per-pipeline budget. Operators flip
/// `dry_run` to `false` after at least one release of clean dry-run
/// dashboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoReplayConfig {
    /// Master kill-switch. When `false`, the orchestrator skips the
    /// entire auto-replay pass even if individual origins have
    /// `allow_replay = true`.
    #[serde(default = "default_auto_replay_enabled")]
    pub enabled: bool,

    /// When `true`, the safety filter runs and would-fire counters
    /// increment, but **no HTTP request is emitted**. Lets us validate
    /// filter behavior against real captured traces before going live.
    /// Defaults to `true` for the first release.
    #[serde(default = "default_auto_replay_dry_run")]
    pub dry_run: bool,

    /// Number of replay attempts per Candidate capability per pipeline
    /// run. Three matches `MIN_SAMPLES_FOR_CANDIDATE` so a single
    /// pipeline pass can promote a clean capability Candidate→Validated.
    #[serde(default = "default_auto_replay_runs_per_capability")]
    pub runs_per_capability: usize,

    /// Hard cap on replays per pipeline run, across all capabilities.
    /// Protects against a pathological cluster explosion firing
    /// thousands of replays in a single run.
    #[serde(default = "default_auto_replay_max_per_pipeline")]
    pub max_per_pipeline: usize,

    /// HTTP methods permitted for auto-replay. POST/PUT/PATCH/DELETE
    /// require explicit opt-in via `opt_in_post_origins`.
    #[serde(default = "default_auto_replay_methods")]
    pub allowed_methods: Vec<String>,

    /// URL substrings that disqualify a capability. Matched
    /// case-insensitively against the URL template. Defaults to the
    /// floor from `auto_replay::DEFAULT_URL_DENYLIST`; operators add
    /// more.
    #[serde(default = "default_auto_replay_url_denylist")]
    pub url_denylist_substrings: Vec<String>,

    /// Origins where the operator has explicitly opted in to replaying
    /// non-GET/HEAD methods. Empty by default.
    #[serde(default)]
    pub opt_in_post_origins: Vec<String>,
}

impl Default for AutoReplayConfig {
    fn default() -> Self {
        Self {
            enabled: default_auto_replay_enabled(),
            dry_run: default_auto_replay_dry_run(),
            runs_per_capability: default_auto_replay_runs_per_capability(),
            max_per_pipeline: default_auto_replay_max_per_pipeline(),
            allowed_methods: default_auto_replay_methods(),
            url_denylist_substrings: default_auto_replay_url_denylist(),
            opt_in_post_origins: Vec::new(),
        }
    }
}

fn default_auto_replay_enabled() -> bool {
    true
}

fn default_auto_replay_dry_run() -> bool {
    true
}

fn default_auto_replay_runs_per_capability() -> usize {
    3
}

fn default_auto_replay_max_per_pipeline() -> usize {
    15
}

fn default_auto_replay_methods() -> Vec<String> {
    // Aggressive default (v0.6.514): allow auto-replay validation for
    // every HTTP method so mining-captured POST/PUT/PATCH/DELETE
    // capabilities can validate without a per-origin opt-in. Safety
    // floors that REMAIN:
    //   - `auto_replay_validation.dry_run: true` (default) — gates
    //     whether real HTTP fires at all.
    //   - The URL denylist (`/auth`, `/login`, `/payment`, `/admin`, …)
    //     blocks the obviously-destructive paths even when methods are
    //     allowlisted.
    //   - `OriginPolicyStore::allow_replay_for_origin` (per-origin opt-in)
    //     still gates orchestrator-driven auto-replay.
    // Operators flip `dry_run` to `false` once they've audited their
    // origin policies and confirmed the denylist covers their case.
    vec![
        "GET".to_string(),
        "HEAD".to_string(),
        "POST".to_string(),
        "PUT".to_string(),
        "PATCH".to_string(),
        "DELETE".to_string(),
    ]
}

fn default_auto_replay_url_denylist() -> Vec<String> {
    crate::magician_v2::api_mining::auto_replay::DEFAULT_URL_DENYLIST
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_takeover_min_samples_per_method() -> std::collections::HashMap<String, usize> {
    // Every method engages on the SECOND visit (v0.6.514, was: GET/HEAD
    // at 1, everything else at 3 from the historical conservative
    // policy). Sample 1 captures the trace; mining promotes to
    // Candidate; visit 2's router check sees Candidate and replays.
    //
    // Side-effecting methods (POST/PUT/PATCH/DELETE) get the same
    // 1-sample threshold here, but their REPLAY tier is set in
    // `api_mining/capability.rs::min_replay_confidence_for`. As of
    // v0.6.514 that policy lowered Write methods to Candidate too —
    // so visit 2 *can* replay them. Safety floors (URL denylist, body
    // fingerprint match, auth posture) keep the obviously-dangerous
    // paths gated.
    let mut map = std::collections::HashMap::new();
    map.insert("GET".to_string(), 1);
    map.insert("HEAD".to_string(), 1);
    map.insert("POST".to_string(), 1);
    map.insert("PUT".to_string(), 1);
    map.insert("PATCH".to_string(), 1);
    map.insert("DELETE".to_string(), 1);
    map.insert("OPTIONS".to_string(), 1);
    map
}

fn default_takeover_default_min_samples() -> usize {
    // Fallback for methods not in the per-method map. Lowered in
    // v0.6.514 from 3 → 1 to match the per-method defaults above.
    // Custom verbs (anything outside the seven enumerated above) thus
    // also engage on visit 2 by default. Operators can raise this in
    // `magician-config.yaml` if they want to keep the historical
    // 3-visit warmup for a specific deployment.
    1
}

fn default_xhr_validation_max_per_pipeline() -> usize {
    25
}

impl ApiMiningConfig {
    /// Resolve the per-method takeover threshold for a given HTTP
    /// verb. Per-method override wins; otherwise falls back to
    /// `takeover_default_min_samples`. Method match is
    /// ASCII-case-insensitive (`GET` == `get`).
    pub fn takeover_min_samples(&self, method: &str) -> usize {
        let upper = method.to_ascii_uppercase();
        self.takeover_min_samples_per_method
            .get(&upper)
            .copied()
            .unwrap_or(self.takeover_default_min_samples)
    }
}

impl ApiMiningConfig {
    /// Whether any API mining feature is active.
    pub fn any_enabled(&self) -> bool {
        self.enabled
            && (self.enable_trace_capture
                || self.enable_mining
                || self.enable_replay
                || self.enable_xhr_validation)
    }

    /// Validate configuration values and clamp invalid ones to safe defaults.
    /// Called during startup to prevent division-by-zero or nonsensical limits.
    pub fn validated(mut self) -> Self {
        if self.max_traces_per_origin == 0 {
            tracing::warn!("[API_MINING] max_traces_per_origin was 0, clamping to 1");
            self.max_traces_per_origin = 1;
        }
        if self.max_capabilities_per_origin == 0 {
            tracing::warn!("[API_MINING] max_capabilities_per_origin was 0, clamping to 1");
            self.max_capabilities_per_origin = 1;
        }
        if self.trace_retention_days == 0 {
            tracing::warn!("[API_MINING] trace_retention_days was 0, clamping to 1");
            self.trace_retention_days = 1;
        }
        if self.xhr_validation_max_per_pipeline == 0 {
            tracing::warn!("[API_MINING] xhr_validation_max_per_pipeline was 0, clamping to 1");
            self.xhr_validation_max_per_pipeline = 1;
        }
        if !self.recipes.match_confirm_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.recipes.match_confirm_threshold)
        {
            tracing::warn!(
                "[API_MINING] recipes.match_confirm_threshold must be between 0 and 1; using the default"
            );
            self.recipes.match_confirm_threshold = default_recipe_match_confirm_threshold();
        }
        let mut transports = Vec::with_capacity(self.recipes.transport_ladder.len());
        for transport in &self.recipes.transport_ladder {
            let normalized = transport.trim().to_ascii_lowercase();
            if !matches!(normalized.as_str(), "reqwest" | "in_page_fetch" | "browser") {
                tracing::warn!(
                    transport = %transport,
                    "[API_MINING] ignoring unknown recipe transport"
                );
                continue;
            }
            if !transports.contains(&normalized) {
                transports.push(normalized);
            }
        }
        if transports.is_empty() {
            // An empty/invalid ladder is a deliberate browser-only fail-safe,
            // never an implicit restoration of direct HTTP.
            transports.push("browser".to_string());
        }
        self.recipes.transport_ladder = transports;
        if self.recipes.unknown_origin_default != default_recipe_unknown_origin_default() {
            tracing::warn!(
                value = %self.recipes.unknown_origin_default,
                "[API_MINING] unsupported recipes.unknown_origin_default; using the governed default"
            );
            self.recipes.unknown_origin_default = default_recipe_unknown_origin_default();
        }
        if self.recipe_verification.interval_secs == 0 {
            tracing::warn!(
                "[API_MINING] recipe_verification.interval_secs was 0, using the default"
            );
            self.recipe_verification.interval_secs = default_recipe_verification_interval_secs();
        }
        self
    }
}

/// Capability evolution feature flag and thresholds.
///
/// This track is default-off until hardening completes. When enabled, the
/// orchestrator can promote mined capabilities into generated capability packs
/// using explicit evaluation thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityEvolutionConfig {
    /// Master switch for capability evolution runtime wiring.
    #[serde(default)]
    pub enabled: bool,
    /// Enable bridge from API-mined capabilities into generated capability packs.
    /// Ignored when `enabled=false`.
    #[serde(default = "default_true")]
    pub enable_api_mined_bridge: bool,
    /// Minimum replay attempts required before Trial -> Validated.
    #[serde(default = "default_trial_to_validated_min_attempts")]
    pub trial_to_validated_min_attempts: u32,
    /// Minimum replay success rate required before Trial -> Validated.
    #[serde(default = "default_trial_to_validated_min_success_rate")]
    pub trial_to_validated_min_success_rate: f64,
    /// Minimum replay attempts required before Validated -> Trusted.
    #[serde(default = "default_validated_to_trusted_min_attempts")]
    pub validated_to_trusted_min_attempts: u32,
    /// Minimum replay success rate required before Validated -> Trusted.
    #[serde(default = "default_validated_to_trusted_min_success_rate")]
    pub validated_to_trusted_min_success_rate: f64,
    /// Failure-rate guardrail used for demotion paths.
    #[serde(default = "default_max_failure_rate_before_demotion")]
    pub max_failure_rate_before_demotion: f64,
    /// Cap the number of generated packs loaded into runtime.
    #[serde(default = "default_max_runtime_generated_packs")]
    pub max_runtime_generated_packs: usize,
    /// Whether Trial packs are loaded into runtime dispatch.
    /// Defaults to false for safer rollout.
    #[serde(default)]
    pub include_trial_packs_in_runtime: bool,
}

impl Default for CapabilityEvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            enable_api_mined_bridge: default_true(),
            trial_to_validated_min_attempts: default_trial_to_validated_min_attempts(),
            trial_to_validated_min_success_rate: default_trial_to_validated_min_success_rate(),
            validated_to_trusted_min_attempts: default_validated_to_trusted_min_attempts(),
            validated_to_trusted_min_success_rate: default_validated_to_trusted_min_success_rate(),
            max_failure_rate_before_demotion: default_max_failure_rate_before_demotion(),
            max_runtime_generated_packs: default_max_runtime_generated_packs(),
            include_trial_packs_in_runtime: false,
        }
    }
}

impl CapabilityEvolutionConfig {
    /// Validate and clamp configuration values.
    pub fn validated(mut self) -> Self {
        if self.trial_to_validated_min_attempts == 0 {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] trial_to_validated_min_attempts was 0, clamping to 1"
            );
            self.trial_to_validated_min_attempts = 1;
        }
        if self.validated_to_trusted_min_attempts == 0 {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] validated_to_trusted_min_attempts was 0, clamping to 1"
            );
            self.validated_to_trusted_min_attempts = 1;
        }
        if !(0.0..=1.0).contains(&self.trial_to_validated_min_success_rate) {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] trial_to_validated_min_success_rate out of range, resetting to default"
            );
            self.trial_to_validated_min_success_rate =
                default_trial_to_validated_min_success_rate();
        }
        if !(0.0..=1.0).contains(&self.validated_to_trusted_min_success_rate) {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] validated_to_trusted_min_success_rate out of range, resetting to default"
            );
            self.validated_to_trusted_min_success_rate =
                default_validated_to_trusted_min_success_rate();
        }
        if !(0.0..=1.0).contains(&self.max_failure_rate_before_demotion) {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] max_failure_rate_before_demotion out of range, resetting to default"
            );
            self.max_failure_rate_before_demotion = default_max_failure_rate_before_demotion();
        }
        if self.max_runtime_generated_packs == 0 {
            tracing::warn!(
                "[CAPABILITY_EVOLUTION] max_runtime_generated_packs was 0, clamping to 1"
            );
            self.max_runtime_generated_packs = 1;
        }
        self
    }
}

fn default_max_traces_per_origin() -> usize {
    500
}

fn default_max_capabilities_per_origin() -> usize {
    100
}

fn default_trace_retention_days() -> u32 {
    30
}

fn default_auth_ttl_secs() -> i64 {
    3600
}

fn default_auth_max_failures() -> usize {
    3
}

fn default_max_vault_origins() -> usize {
    500
}

fn default_true() -> bool {
    true
}

fn default_trial_to_validated_min_attempts() -> u32 {
    5
}

fn default_trial_to_validated_min_success_rate() -> f64 {
    0.8
}

fn default_validated_to_trusted_min_attempts() -> u32 {
    20
}

fn default_validated_to_trusted_min_success_rate() -> f64 {
    0.95
}

fn default_max_failure_rate_before_demotion() -> f64 {
    0.5
}

fn default_max_runtime_generated_packs() -> usize {
    256
}

// Default functions for MagicianConfig
fn default_magician_enabled() -> bool {
    false
}

fn default_magician_realtime_events() -> bool {
    false
}

fn default_magician_storage_path() -> String {
    "magician_data_v3".to_string()
}

fn default_magician_max_conversations() -> usize {
    100
}

fn default_magician_conversation_timeout() -> u64 {
    3600
}

fn default_consumer_mode() -> bool {
    true
}

fn default_task_state_policy() -> String {
    "llm_lazy_optional".to_string()
}

fn default_task_state_decision_points() -> Vec<String> {
    [
        "task_start",
        "each_outer_iteration_start",
        "after_capability_return",
        "resume_or_handoff",
        "user_goal_change",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_task_state_revision_format() -> String {
    "structured_json".to_string()
}

fn default_task_state_allowed_actions() -> Vec<String> {
    ["create", "patch", "close"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn default_durable_task_state_generate_operation() -> String {
    "durable_task_state_generate".to_string()
}

fn default_durable_task_state_patch_operation() -> String {
    "durable_task_state_patch".to_string()
}

fn default_durable_task_state_close_summary_operation() -> String {
    "durable_task_state_close_summary".to_string()
}

fn default_magicutor_base_url() -> String {
    "http://127.0.0.1:3003/".to_string()
}

fn default_execution_timeout_secs() -> u64 {
    600 // 10 minutes for complex pages like WhatsApp Web
}

fn default_harness_model() -> String {
    "default".to_string()
}

fn default_harness_engine() -> String {
    "magician".to_string()
}

pub(crate) fn default_harness_turn_max_tool_calls() -> u32 {
    4000
}

pub(crate) fn default_harness_turn_max_seconds() -> u64 {
    DEFAULT_AGENTIC_MAX_DURATION_SECS
}

/// Optional strict browser-interaction gate — OFF by default.
///
/// When enabled (env `MAGICIAN_STRICT_BROWSER_GATE` set to `1`/`true`/`yes`/`on`),
/// browser-seeded runs are held to "genuine primitive" completion: a
/// `goal_reached` claim that never used a real interaction primitive is
/// rejected, and the heuristic "no observable page effect" signal counts
/// toward stuck detection. This is useful ONLY for primitive-capability
/// benchmarking (e.g. the SoTA scroll/click tests, where the whole point is
/// to exercise a specific browser primitive).
///
/// Off by default on purpose: for real work the model is free to accomplish a
/// task by whatever method works — including `eval` that genuinely fires the
/// target's own handlers. We verify the *effect* (did the page reach the goal
/// state), not the *method*. Gating method would reject legitimate, often more
/// reliable, eval-driven completion.
pub fn strict_browser_interaction_gate_enabled() -> bool {
    std::env::var("MAGICIAN_STRICT_BROWSER_GATE")
        .map(|v| {
            let v = v.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

/// Pure parse for the agentic wall-clock ceiling: positive integer seconds only.
/// `None`/0/whitespace/unparseable → `None` (no ceiling).
fn parse_max_duration_secs(raw: Option<&str>) -> Option<std::time::Duration> {
    raw.and_then(|r| r.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(std::time::Duration::from_secs)
}

/// Default agentic wall-clock ceiling (40 min) applied when
/// `MAGICIAN_AGENTIC_MAX_DURATION_SECS` is unset. Gives every agentic run
/// — including `ChatInline`, which has no outer goal-pipeline 3h wrap — a
/// bounded wall-clock circuit-breaker after the 4000-iter/2M-token bump.
pub const DEFAULT_AGENTIC_MAX_DURATION_SECS: u64 = 2400;

/// Wall-clock ceiling for one agentic execution, from
/// `MAGICIAN_AGENTIC_MAX_DURATION_SECS`. When the env var is **unset**, a sane
/// default of `DEFAULT_AGENTIC_MAX_DURATION_SECS` applies so every run is bounded.
/// An explicit `0` disables the ceiling (unbounded, `max_iterations`-only);
/// a positive value overrides the default. A watchdog cancels the execution's
/// cancellation token at the deadline, aborting a hung Pi turn via the existing
/// execute_action select + Pi `kill_on_drop`.
pub fn agentic_max_duration() -> Option<std::time::Duration> {
    match std::env::var("MAGICIAN_AGENTIC_MAX_DURATION_SECS") {
        // Explicitly set: honor the pure-parse semantics (0/unparseable → no ceiling).
        Ok(raw) => parse_max_duration_secs(Some(raw.as_str())),
        // Unset: apply the sane default so the inner loop is never unbounded.
        Err(_) => Some(std::time::Duration::from_secs(
            DEFAULT_AGENTIC_MAX_DURATION_SECS,
        )),
    }
}

/// Wall-clock ceiling for one **coding** execution.
///
/// Changing the clip inside `run_coding_task` alone was never enough: the
/// executor independently builds its watchdog from [`agentic_max_duration`] and
/// fuses that deadline into the cancellation token that reaches every cancel
/// seam, including the select that drops the Pi future. Two independent forty
/// minute ceilings — lifting one leaves the other, and the run still dies at
/// forty minutes for reasons nobody can see.
///
/// The generic default is *not* an operator decision, so a coding execution
/// uses the coding policy instead. An explicitly-set env var **is** a decision
/// and is honoured in both directions:
///
/// - env unset → the coding execution ceiling, which is `None` (no wall clock)
///   unless a whole-task ceiling was configured;
/// - env set to `0` → unbounded, exactly as for any other execution;
/// - env set to `N` → `N`, because someone chose it deliberately.
pub fn coding_execution_max_duration() -> Option<std::time::Duration> {
    if std::env::var_os("MAGICIAN_AGENTIC_MAX_DURATION_SECS").is_some() {
        return agentic_max_duration();
    }
    let settings = coding_budgets::coding_budget_settings();
    coding_budgets::ResolvedCodingBudgets::resolve(&settings, None).execution_ceiling()
}

/// Default per-run USD cost ceiling ($5) applied when
/// `MAGICIAN_AGENTIC_MAX_COST_USD` is unset. Paired with the cumulative token
/// meter as a second runaway-cost circuit-breaker on the agentic loop.
/// 15 since 0.7.62 (5 before): a GPT-5.6 Terra browser run costs ~$1–2 and
/// never met the old ceiling; a Fable 5.1 decider — 157–181K prompt tokens a
/// turn on the browser tool set, no delta continuation — exhausted $5 in ten
/// decides on 2026-09-21, before the run reached extraction.
pub const DEFAULT_AGENTIC_MAX_COST_USD: f64 = 15.0;

/// Per-run USD cost ceiling for one agentic execution, from
/// `MAGICIAN_AGENTIC_MAX_COST_USD`. When the env var is **unset**, a sane
/// default of `DEFAULT_AGENTIC_MAX_COST_USD` applies. An explicit `0` (or any
/// non-positive/unparseable value) disables the ceiling (unbounded, token-only);
/// a positive value overrides the default. Read alongside the cumulative token
/// meter so a run that spends beyond the cap settles like a budget stop.
/// How long a harness turn may make no progress — no stdout line, and so no
/// tool call — before the turn is ended as stalled. The wall-clock ceiling
/// (`harness_turn_max_seconds`, 2400s) is the outer bound; this is the one that
/// catches a stuck child, which otherwise holds a run for the whole ceiling
/// with nothing logged.
pub const DEFAULT_HARNESS_TURN_IDLE_SECONDS: u64 = 300;

pub fn default_harness_turn_idle_seconds() -> u64 {
    DEFAULT_HARNESS_TURN_IDLE_SECONDS
}

pub fn agentic_max_cost_usd() -> Option<f64> {
    match std::env::var("MAGICIAN_AGENTIC_MAX_COST_USD") {
        Ok(raw) => raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0),
        Err(_) => Some(DEFAULT_AGENTIC_MAX_COST_USD),
    }
}

/// **OFF by default.** When enabled (`MAGICIAN_STRICT_TOKEN_USAGE` truthy), an
/// agentic decision whose LLM response is missing complete token-usage metadata
/// is treated strictly (fail-closed on the token meter). With the default OFF,
/// missing usage does NOT force the cumulative meter to its limit, so a provider
/// that omits usage metadata cannot spuriously trip a hard budget FAIL. Read by
/// the executor's token-budget accounting. **Opt IN** with
/// `MAGICIAN_STRICT_TOKEN_USAGE` = `1`/`true`/`yes`/`on`.
pub fn strict_token_usage_enabled() -> bool {
    matches!(
        std::env::var("MAGICIAN_STRICT_TOKEN_USAGE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// **ON by default.** Gate for out-of-workspace writes: whether a `write_file`/
/// `edit_file` to a path OUTSIDE the scoped workspace may escalate to the
/// sandbox-override HITL and, once the operator approves a root, stage + apply
/// there (`ScopedWorkspacePath.apply_root`). With the default ON, an
/// out-of-workspace write escalates to an operator approval; with the opt-out
/// kill-switch set, it instead returns a soft actionable error and `apply_root`
/// stays `None`.
///
/// SECURITY: this feature makes a transaction's persisted `apply_root` a TRUSTED
/// write-redirect. The `shell` tool (default-granted to every trusted agent) can
/// write the scope's `transactions/` store, so historically a malicious agent
/// could forge `apply_root` and redirect an operator-approved write — which is
/// why this shipped default-OFF. **That forge vector is now closed**, so the
/// default flips ON:
/// - The **trusted-store integrity** layer (`execution::trusted_store`) binds every
///   apply decision to an in-process, per-boot `TrustAuthority`: at the decide site
///   `apply_root`/targets come from process memory (the shell cannot reach it) and
///   the on-disk record is re-hashed and compared, so a forged/tampered `apply_root`
///   fails closed. See `docs/components/magician/trusted-store-integrity.md`.
/// - The **always-on runtime-store deny-fence** (`6b27707c9`) hard-denies native
///   file-tool writes to `{transactions, code_change_proposals, runtime/pause_states}`.
///
/// **Opt OUT** (force-disable) by setting `MAGICIAN_ALLOW_EXTERNAL_WRITES` to a
/// falsy value (`0`/`false`/`no`/`off`).
pub fn external_writes_enabled() -> bool {
    match std::env::var("MAGICIAN_ALLOW_EXTERNAL_WRITES") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// **ON by default.** When ON, an agent's `create_task` for a same-scope leaf task
/// (no schedule, disposition `now`) is dispatched immediately via `start_execution`
/// instead of orphaning in `ready` — the RC #3 fix (agent-created tasks never
/// auto-execute because both dispatch loops are cron-gated). Bounded by the existing
/// per-cycle spawn cap (`__max_spawned_tasks`) and the auto-dispatch depth cap. When
/// OFF, every `create_task` stays `ready` (the pre-fix behavior). **Opt OUT** with
/// `MAGICIAN_AUTO_DISPATCH_CREATED_TASKS` = `0`/`false`/`no`/`off`. See
/// `docs/archive/plans/2026-07-12-agent-task-dispatch-trust-tiered-design.md`.
pub fn auto_dispatch_enabled() -> bool {
    match std::env::var("MAGICIAN_AUTO_DISPATCH_CREATED_TASKS") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// STEP 3 — gate for the single-execution-context `orchestrate_pipeline` path.
///
/// **ON by default (Increment 3 flip).** A chat `orchestrate_pipeline` task is
/// decomposed up-front into an ordered stage roster and run within ONE
/// `ExecutionRun` (per-stage owner swaps, no spawned child per stage). When
/// decomposition yields nothing usable (< 2 valid stages), the task transparently
/// falls back to the dynamic PA-root delegate loop. **Opt OUT** by setting
/// `MAGICIAN_ORCHESTRATE_PIPELINE_SINGLE_CONTEXT` to a falsy value (`0`/`false`/
/// `no`/`off`) to force the legacy dynamic path. See
/// `docs/plans/2026-06-04-step3-stage-source-fork.md`.
pub fn pipeline_single_context_enabled() -> bool {
    match std::env::var("MAGICIAN_ORCHESTRATE_PIPELINE_SINGLE_CONTEXT") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// **OFF by default.** Gates the legacy **Option A** up-front *decompose* path for
/// chat `orchestrate_pipeline` tasks (the deterministic `decompose_orchestration_goal`
/// → `execute_pipeline_single_context` engine). It is retired in favor of **Option B**
/// (in-context single-delegate swap, gated by `pipeline_single_context_enabled`): the
/// fixed up-front plan locked the adaptive PA-root loop into a roster it couldn't
/// recover from. With this OFF, `orchestrate_pipeline` runs the dynamic PA-root delegate
/// loop, and each SINGLE delegate runs IN-CONTEXT on the one `ExecutionRun`. The Option-A
/// engine is kept dormant-but-live (callable under this flag for the recorded conditional
/// revisit). **Opt IN** with `MAGICIAN_ORCHESTRATE_DECOMPOSE` = `1`/`true`/`yes`/`on`.
/// See `docs/plans/2026-06-04-step3-stage-source-fork.md`.
pub fn orchestrate_decompose_enabled() -> bool {
    matches!(
        std::env::var("MAGICIAN_ORCHESTRATE_DECOMPOSE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// RCA fix #1 — VibeDev Build success-gate kill-switch. **ON by default.** Gates
/// the success verdict of a VibeDev *Build* coordinator run (the canonical signal
/// — `ui_thread_id`/`vibedev` tag, `plan`/Discuss excluded; see
/// `is_vibedev_coding_build_run`). When such a run reports success while engaging
/// NO coding pipeline (zero durable `coding.*` events AND no `CodeChangeProposal`
/// AND no delegations), the verdict is refused and the run settles as a terminal
/// failure instead of silently "completing" an unstaged live-repo write. **Opt
/// OUT** by setting `MAGICIAN_VIBEDEV_SUCCESS_GATE` to a falsy value
/// (`0`/`false`/`no`/`off`). See
/// `docs/plans/2026-06-17-vibedev-coordinator-self-serve-false-success.md`.
pub fn vibedev_success_gate_enabled() -> bool {
    match std::env::var("MAGICIAN_VIBEDEV_SUCCESS_GATE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Root-gate the task-level (stage 1.2/1.3) output synthesis so it runs ONLY on the
/// execution that owns the task projection slot (the root or a standalone task), not
/// on every terminal execution. A delegated child writes its task-level synthesis to
/// the ROOT's projection key, which the root then overwrites when it finalizes — so a
/// child's task-level synthesis is always discarded (pure redundant LLM work). The
/// child's per-execution stage 1.1 output still feeds the parent via `child_output_refs`,
/// so the synthesis chain is preserved. Applies to ALL delegating flows, not just
/// vibedev. **Opt OUT** by setting `MAGICIAN_TASK_SYNTHESIS_ROOT_GATE` to a falsy value
/// (`0`/`false`/`no`/`off`), which restores per-execution task synthesis. See
/// `docs/plans/2026-06-17-vibedev-run-startup-latency.md`.
pub fn task_synthesis_root_gate_enabled() -> bool {
    match std::env::var("MAGICIAN_TASK_SYNTHESIS_ROOT_GATE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Generic agentic outer-loop settings. Keep-tail windowing lives in
/// `execution/agentic/decision.rs`. This section is reserved for future knobs
/// and currently accepts only an empty mapping.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgenticSettings {}

#[cfg(any(test, feature = "test-fixtures"))]
mod approval_envelope_posture_tests {
    use super::*;
    use crate::magician_v2::approval_envelopes::EnvelopeMode;

    /// The shipped default is `off`, in both spellings that can reach a running
    /// process: the struct default and an absent `approval_envelopes:` section.
    ///
    /// Pins the consent property. An envelope system that arrived switched on
    /// would pre-authorise outward acts nobody was asked about, which is the
    /// exact inverse of what envelopes are for.
    #[test]
    fn the_default_posture_is_off_and_authorises_nothing() {
        let from_struct = ApprovalEnvelopesConfig::default();
        assert_eq!(from_struct.mode, "off");
        assert_eq!(from_struct.mode(), EnvelopeMode::Off);
        assert!(!from_struct.mode().may_authorise());

        let absent: ApprovalEnvelopesConfig =
            serde_yaml::from_str("{}").expect("an absent section parses");
        assert_eq!(absent.mode(), EnvelopeMode::Off);
        assert_eq!(absent.mode, "off");

        // And the whole config carries the same default, so a magician-config
        // that never mentions envelopes runs with them off.
        assert_eq!(
            MagicianConfig::default().approval_envelopes.mode(),
            EnvelopeMode::Off
        );
    }

    /// The key actually works: each posture the plan names parses to itself, so
    /// an operator who writes `shadow` gets shadow rather than silence.
    #[test]
    fn each_named_posture_is_readable_from_yaml() {
        for (written, expected) in [
            ("off", EnvelopeMode::Off),
            ("shadow", EnvelopeMode::Shadow),
            ("enforcing", EnvelopeMode::Enforcing),
        ] {
            let parsed: ApprovalEnvelopesConfig =
                serde_yaml::from_str(&format!("mode: {written}\n")).expect("section parses");
            assert_eq!(parsed.mode(), expected, "`{written}` must name its posture");
        }
    }

    /// An unrecognised posture reads as `off`, never as the nearest match.
    ///
    /// Fail-closed: `enforce`, `on` and `true` are all plausible typos for
    /// "let acts through", and guessing at any of them would authorise outward
    /// acts on the strength of a misspelling.
    #[test]
    fn an_unreadable_posture_is_not_permission() {
        for written in [
            "enforce",
            "on",
            "true",
            "yes",
            "",
            "shadow-mode",
            "enforcing!",
        ] {
            let config = ApprovalEnvelopesConfig {
                mode: written.to_string(),
            };
            assert_eq!(
                config.mode(),
                EnvelopeMode::Off,
                "`{written}` names no posture, so it must authorise nothing"
            );
        }
    }

    /// Every config surface this repo ships must actually parse.
    ///
    /// `MagicianConfig` and most of its sections are `deny_unknown_fields`, so a
    /// key added to the YAML without a matching Rust field does not degrade —
    /// it fails the whole load and magician will not boot. That is the right
    /// behaviour, and it is exactly why the shipped files need a test: a
    /// `runtime.ollama.local_generation:` block once reached all three surfaces
    /// with no field behind it, and nothing here noticed, because no test had
    /// ever parsed the files the product actually ships.
    ///
    /// The live operator config under `$MAGICIAN_ROOT_DIR` is deliberately not
    /// checked: it is machine state, not a repo artifact.
    #[test]
    fn every_shipped_config_surface_parses() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        for relative in ["magician-config.yaml"] {
            let path = repo_root.join(relative);
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if let Err(error) = serde_yaml::from_str::<MagicianConfig>(&raw) {
                panic!(
                    "{relative} does not parse into MagicianConfig, so magician \
                     would refuse to boot on it: {error}"
                );
            }
        }
    }

    /// The top-level key is spelled `approval_envelopes`, and `deny_unknown_fields`
    /// means a misspelling is a loud load error rather than a section that
    /// silently does nothing.
    #[test]
    fn the_top_level_key_is_named_and_a_typo_is_rejected() {
        let parsed: MagicianConfig =
            serde_yaml::from_str("approval_envelopes:\n  mode: shadow\n").expect("key is known");
        assert_eq!(parsed.approval_envelopes.mode(), EnvelopeMode::Shadow);

        assert!(
            serde_yaml::from_str::<MagicianConfig>("approval_envelope:\n  mode: shadow\n").is_err(),
            "a misspelled section must fail the load, not quietly leave envelopes off"
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod recipient_compliance_posture_tests {
    use super::*;
    use crate::magician_v2::recipient_compliance::RecipientComplianceMode;

    /// The shipped default is `off`, in both spellings that can reach a running
    /// process: the struct default and an absent `recipient_compliance:`
    /// section.
    ///
    /// Pins the blast-radius property. This gate REFUSES sends and its four
    /// rules fail closed on every register they cannot read, so a posture that
    /// arrived switched on would stop outward sends across a fleet before
    /// anybody had chosen to turn it on.
    #[test]
    fn the_default_posture_is_off_and_refuses_nothing() {
        let from_struct = RecipientComplianceConfig::default();
        assert_eq!(from_struct.mode, "off");
        assert_eq!(from_struct.mode(), RecipientComplianceMode::Off);
        assert!(!from_struct.mode().blocks());

        let absent: RecipientComplianceConfig =
            serde_yaml::from_str("{}").expect("an absent section parses");
        assert_eq!(absent.mode(), RecipientComplianceMode::Off);
        assert_eq!(absent.mode, "off");

        // And the whole config carries the same default, so a magician-config
        // that never mentions the gate runs with it off.
        assert_eq!(
            MagicianConfig::default().recipient_compliance.mode(),
            RecipientComplianceMode::Off
        );
    }

    /// The key actually works: an operator who writes `blocking` gets a gate
    /// that blocks, rather than a section that parses and does nothing.
    #[test]
    fn the_blocking_posture_is_readable_from_yaml() {
        let parsed: RecipientComplianceConfig =
            serde_yaml::from_str("mode: blocking\n").expect("section parses");
        assert_eq!(parsed.mode(), RecipientComplianceMode::Blocking);
        assert!(
            parsed.mode().blocks(),
            "a posture the dispatch path does not act on is a key that lies"
        );
    }

    /// An unrecognised posture reads as `off`, never as the nearest match.
    ///
    /// `shadow` and `enforcing` are in the list on purpose: they are this
    /// gate's sibling's spellings, and an operator who copies them across must
    /// get `off` rather than a gate that guessed which of them meant "block".
    #[test]
    fn an_unreadable_posture_never_starts_refusing_sends() {
        for written in [
            "block",
            "on",
            "true",
            "yes",
            "",
            "shadow",
            "enforcing",
            "blocking!",
        ] {
            let config = RecipientComplianceConfig {
                mode: written.to_string(),
            };
            assert_eq!(
                config.mode(),
                RecipientComplianceMode::Off,
                "`{written}` names no posture, so it must refuse nothing"
            );
        }
    }

    /// The top-level key is spelled `recipient_compliance`, and
    /// `deny_unknown_fields` means a misspelling is a loud load error rather
    /// than a section that silently leaves the gate off.
    #[test]
    fn the_top_level_key_is_named_and_a_typo_is_rejected() {
        let parsed: MagicianConfig =
            serde_yaml::from_str("recipient_compliance:\n  mode: blocking\n")
                .expect("key is known");
        assert_eq!(
            parsed.recipient_compliance.mode(),
            RecipientComplianceMode::Blocking
        );

        assert!(
            serde_yaml::from_str::<MagicianConfig>("recipient_compliances:\n  mode: blocking\n")
                .is_err(),
            "a misspelled section must fail the load, not quietly leave the gate off while an \
             operator believes they turned it on"
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// The profiles block moved out of `magician-config.yaml` into a sibling
    /// `llm-profiles.yaml` — about 3,800 of the file's 6,200 lines. This is the
    /// round trip. It matters because `profiles` carries `#[serde(default)]`, so
    /// a botched splice would parse happily with an empty table rather than
    /// failing loudly.
    #[test]
    fn the_repository_config_still_loads_its_router_profiles() {
        let config = load_magician_config_from_path(Path::new("../magician-config.yaml"))
            .expect("repository config loads with its sibling profiles");
        let router = config.llm.router.as_ref().expect("router configured");
        assert!(
            router.profiles.len() > 200,
            "expected the full profile table, got {}",
            router.profiles.len()
        );
        assert!(
            router.operation_mapping.len() > 50,
            "operation_mapping moved to the same sibling file, got {}",
            router.operation_mapping.len()
        );
    }

    /// Magician's `decision:` block only says how to reach the engine; the
    /// engine's settings are their own file, and the seed of each parses as
    /// its owner reads it. Engine keys under `decision:` are refused.
    #[test]
    fn the_decision_settings_live_with_the_engine() {
        let config = load_magician_config_from_path(Path::new("../magician-config.yaml"))
            .expect("repository config loads");
        assert_eq!(config.decision.mode, DecisionMode::AllEngines);
        for seed in ["../decision-engine.yaml"] {
            let text = std::fs::read_to_string(seed).expect("engine seed");
            let engine: magician_decision::config::DecisionConfig =
                serde_yaml::from_str(&text).expect("the engine's schema");
            assert!(!engine.models.is_empty(), "{seed}");
        }
        let host: DecisionHostConfig =
            serde_yaml::from_str("enabled: false\nsocket: /tmp/x.sock\n").expect("host block");
        assert_eq!(host.mode, DecisionMode::Off);
        assert_eq!(host.timeout_ms, 20_000);
        for engine_key in ["models: {}\n", "operations: {}\n", "tiers: {}\n"] {
            assert!(
                serde_yaml::from_str::<DecisionHostConfig>(engine_key).is_err(),
                "{engine_key}"
            );
        }
    }

    /// Why the splice is textual rather than a merge of two parsed documents:
    /// profiles alias `*local_generation_model`, whose anchor is defined in
    /// `runtime:` of the main config. YAML anchors do not cross files, so a
    /// document-level merge could not resolve it.
    #[test]
    fn an_anchor_defined_in_the_config_resolves_inside_the_spliced_profiles() {
        let config = load_magician_config_from_path(Path::new("../magician-config.yaml"))
            .expect("repository config loads");
        let router = config.llm.router.as_ref().expect("router configured");
        let profile = router
            .profiles
            .get("op-memory-episode-quality-local-chunked")
            .expect("a profile that aliases the cross-file anchor");
        let yaml = std::fs::read_to_string("../magician-config.yaml").expect("read seed config");
        let selected = yaml
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("selected: &local_generation_model ")
                    .map(str::to_string)
            })
            .expect("the seed config still defines the local-generation anchor");
        assert_eq!(
            profile.model, selected,
            "the alias resolved to the anchor's value, not literal alias text"
        );
    }

    /// A config that already carries its tables inline needs no sibling.
    ///
    /// This is the shape test fixtures seed into temp directories — they write
    /// the output of `splice_router_tables_into` and then load it back — and it
    /// is also what a pre-split or deliberately self-contained config looks
    /// like. Requiring the sibling anyway made every such fixture unloadable.
    #[test]
    fn a_config_carrying_its_tables_inline_loads_without_a_sibling_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("magician-config.yaml");
        std::fs::write(&config, shipped_repo_config_yaml()).expect("seed a complete config");
        assert!(
            !dir.path().join(ROUTER_TABLES_FILE).exists(),
            "the fixture deliberately has no sibling tables file"
        );
        let loaded = load_magician_config_from_path(&config)
            .expect("a self-contained config loads with no sibling");
        let router = loaded.llm.router.as_ref().expect("router configured");
        assert!(
            router.profiles.len() > 200,
            "the inline tables were used, not silently defaulted away: got {}",
            router.profiles.len()
        );
    }

    /// Absence must be fatal. `llm_pricing.json` is fail-open because it
    /// overlays built-in rates; there is no built-in profile table, so
    /// tolerating a missing file would boot a router with nothing to route to
    /// and surface much later as confusing per-operation failures.
    #[test]
    fn a_missing_profiles_file_fails_the_load_rather_than_booting_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("magician-config.yaml");
        std::fs::copy("../magician-config.yaml", &config).expect("copy config, not profiles");
        let error = load_magician_config_from_path(&config)
            .expect_err("a config with no sibling profiles file must not load");
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains(ROUTER_TABLES_FILE)),
            "the error should name the missing file, got: {error:#}"
        );
    }

    #[test]
    fn an_absent_verification_block_reproduces_the_module_defaults() {
        use crate::magician_v2::execution::verification::{GateBudgets, VerificationActivation};

        let config = VerificationConfig::default();
        assert_eq!(config.activation(), VerificationActivation::Disabled);

        let settings = config.runtime_settings();
        assert_eq!(settings.budgets.resolve(), GateBudgets::default());
        assert!(settings.baseline.is_none());
        assert_eq!(settings.reconcile_interval_secs, 60);
    }

    #[test]
    fn a_verification_block_parses_mode_budgets_and_an_owner_baseline() {
        use crate::magician_v2::execution::verification::{PolicySource, VerificationActivation};

        let config: VerificationConfig = serde_yaml::from_str(
            r#"
mode: observe
max_repair_rounds: 1
max_spend_usd: 2.5
max_elapsed_secs: 600
reconcile_interval_secs: 30
baseline:
  required:
    - id: check
      program: make
      args: ["check-all"]
      display: make check-all
"#,
        )
        .unwrap();

        assert_eq!(config.activation(), VerificationActivation::Observe);
        let settings = config.runtime_settings();
        let budgets = settings.budgets.resolve();
        assert_eq!(budgets.max_repair_rounds, 1);
        assert_eq!(budgets.max_spend_usd, Some(2.5));
        assert_eq!(budgets.max_elapsed_secs, Some(600));
        assert_eq!(settings.reconcile_interval_secs, 30);

        // The baseline is owner-sourced by construction — YAML cannot claim a
        // weaker source to dodge the anti-weakening resolver.
        let baseline = settings.baseline.expect("a configured baseline must parse");
        assert_eq!(baseline.source, PolicySource::Owner);
        assert_eq!(baseline.required.len(), 1);
        assert_eq!(baseline.required[0].program, "make");
        assert!(baseline.baseline_ref.is_some());
    }

    #[test]
    fn an_empty_baseline_block_keeps_repository_defined_semantics() {
        let config: VerificationConfig = serde_yaml::from_str("baseline: {}\n").unwrap();
        assert!(
            config.runtime_settings().baseline.is_none(),
            "an empty baseline must not become an empty owner policy"
        );
    }

    #[test]
    fn a_typo_in_verification_mode_degrades_to_disabled_not_enforce() {
        use crate::magician_v2::execution::verification::VerificationActivation;

        let config: VerificationConfig = serde_yaml::from_str("mode: enforced\n").unwrap();
        assert_eq!(config.activation(), VerificationActivation::Disabled);
    }

    #[test]
    fn an_unknown_verification_key_is_a_hard_parse_error() {
        // Consistent with the rest of the config file: a misspelt knob must
        // fail the load, not silently do nothing.
        assert!(serde_yaml::from_str::<VerificationConfig>("reconcile_interval: 5\n").is_err());
    }

    fn test_llm_router_with_operations(operations: &[&str]) -> LLMRouterConfig {
        let mut router = LLMRouterConfig::default();
        router.default_profile = "test-profile".into();
        router.profiles.insert(
            "test-profile".into(),
            magicllm::config::LLMProfile {
                provider: magicllm::capability::LLMProviderKind::OpenAI,
                model: "test-model".into(),
                api_key_env: Some("OPENAI_API_KEY".into()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: Some(1_024),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        for operation in operations {
            router
                .operation_mapping
                .insert((*operation).to_string(), "test-profile".into());
        }
        router
    }

    fn test_public_chat_config() -> MagicianConfig {
        let mut config = MagicianConfig::default();
        config.llm.router = Some(test_llm_router_with_operations(&[
            "kapso_envoy_chat",
            "kapso_envoy_chat_fallback",
        ]));
        config.public_chat.kapso_envoy_chat = Some(PublicChatPolicyConfig {
            enabled: true,
            first_contact: Some(PublicChatFirstContactPolicyConfig::default()),
            identity_research: Some(PublicChatIdentityResearchPolicyConfig {
                enabled: true,
                ..PublicChatIdentityResearchPolicyConfig::default()
            }),
            ..PublicChatPolicyConfig::default()
        });
        config
    }

    #[test]
    fn social_policy_is_yaml_owned_and_unknown_keys_fail_closed() {
        let parsed: SocialConfig = serde_yaml::from_str(
            r#"
enabled: true
paused: false
scopes:
  - principal: anonymous
    workspace: default
tick_interval_secs: 600
cooldown_secs: 7200
max_agents_per_tick: 3
default_daily_tokens: 3000
gate_reserve_tokens: 300
compose_reserve_tokens: 1200
max_post_chars: 1500
"#,
        )
        .expect("social config parses");
        assert!(parsed.enabled);
        assert_eq!(parsed.scopes, default_social_scopes());
        assert_eq!(parsed.tick_interval_secs, 600);
        assert_eq!(parsed.max_agents_per_tick, 3);
        assert_eq!(parsed.compose_reserve_tokens, 1_200);
        assert!(
            serde_yaml::from_str::<SocialConfig>("enabled: true\nSOCIAL_ENABLED: true\n").is_err()
        );
    }

    #[test]
    fn social_defaults_are_bounded_and_disabled_for_implicit_configs() {
        let config = SocialConfig::default();
        assert!(!config.enabled);
        assert!(!config.paused);
        assert_eq!(config.scopes, default_social_scopes());
        assert!(config.gate_reserve_tokens > 0);
        assert!(config.compose_reserve_tokens > config.gate_reserve_tokens);
        assert!(config.default_daily_tokens >= config.compose_reserve_tokens);
    }

    #[test]
    fn social_scope_allowlist_rejects_ambiguous_or_duplicate_storage_targets() {
        let mut config = MagicianConfig::default();
        config.social.scopes = vec![
            SocialScopeConfig {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            SocialScopeConfig {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
        ];
        assert!(enforce_social_config_invariant(&config).is_err());

        config.social.scopes = vec![SocialScopeConfig {
            principal: "../escape".to_string(),
            workspace: "default".to_string(),
        }];
        assert!(enforce_social_config_invariant(&config).is_err());

        config.social.scopes = vec![
            SocialScopeConfig {
                principal: "alice".to_string(),
                workspace: "personal".to_string(),
            },
            SocialScopeConfig {
                principal: "team".to_string(),
                workspace: "fundraising".to_string(),
            },
        ];
        enforce_social_config_invariant(&config)
            .expect("distinct social scopes are independent valid targets");

        config.social.scopes = vec![
            SocialScopeConfig {
                principal: "user:1".to_string(),
                workspace: "default".to_string(),
            },
            SocialScopeConfig {
                principal: "user_1".to_string(),
                workspace: "default".to_string(),
            },
        ];
        assert!(enforce_social_config_invariant(&config).is_err());

        config.social.enabled = false;
        config.social.scopes.clear();
        enforce_social_config_invariant(&config)
            .expect("a disabled worker may intentionally configure no autonomous scopes");

        config.social.enabled = true;
        assert!(enforce_social_config_invariant(&config).is_err());
    }

    #[test]
    fn social_no_longer_requires_the_retired_router_operation_mappings() {
        // `social_gate` and `social_compose` retired with the first-party
        // engine: Town Square's gate and compose are the package's own `app:`
        // operations now. Boot must not refuse a config for missing mappings no
        // core code can route, and an operator must not have to keep dead
        // entries in `llm.router.operation_mapping` to start the server.
        let mut config = MagicianConfig::default();
        config.social.enabled = true;
        config.social.scopes = vec![SocialScopeConfig {
            principal: "user:1".to_string(),
            workspace: "default".to_string(),
        }];
        config.llm.router = Some(test_llm_router_with_operations(&["chat"]));
        enforce_social_config_invariant(&config)
            .expect("the retired operations are not a boot requirement");
    }

    #[test]
    fn parse_max_duration_secs_handles_edges() {
        use std::time::Duration;
        assert_eq!(parse_max_duration_secs(Some("0")), None);
        assert_eq!(
            parse_max_duration_secs(Some("30")),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_max_duration_secs(Some("  60 ")),
            Some(Duration::from_secs(60))
        );
        assert_eq!(parse_max_duration_secs(Some("abc")), None);
        assert_eq!(parse_max_duration_secs(None), None);
    }

    #[test]
    fn llm_trace_defaults_are_metadata_only_and_fail_closed() {
        let config = MagicianConfig::default();
        let trace = &config.analytics.llm_trace;
        assert!(trace.enabled);
        assert_eq!(trace.content_mode, LlmContentMode::Metadata);
        assert!(trace.redaction.fail_to_metadata_only);
        assert!(!trace.training_default_eligible);
        enforce_llm_trace_config_invariant(&config).expect("safe defaults validate");
    }

    #[test]
    fn llm_trace_rejects_any_fail_open_or_unimplemented_raw_capture() {
        let mut fail_open = MagicianConfig::default();
        fail_open
            .analytics
            .llm_trace
            .redaction
            .fail_to_metadata_only = false;
        assert!(enforce_llm_trace_config_invariant(&fail_open)
            .expect_err("fail-open sanitizer policy must be rejected")
            .to_string()
            .contains("must remain true"));

        let mut raw = MagicianConfig::default();
        raw.analytics.llm_trace.content_mode = LlmContentMode::FullLocalEncrypted;
        assert!(enforce_llm_trace_config_invariant(&raw)
            .expect_err("encrypted raw capture is not implemented")
            .to_string()
            .contains("Phase 10"));

        let mut raw_override = MagicianConfig::default();
        raw_override.analytics.llm_trace.operation_overrides.insert(
            "chat".to_string(),
            LlmTraceCaptureOverride {
                content_mode: LlmContentMode::FullLocalEncrypted,
                sanitized_content_rate: 1.0,
                training_eligible: false,
            },
        );
        assert!(enforce_llm_trace_config_invariant(&raw_override)
            .expect_err("override cannot bypass the raw-capture gate")
            .to_string()
            .contains("cannot enable full_local_encrypted"));
    }

    #[test]
    fn llm_trace_training_eligibility_requires_sanitized_retained_content() {
        let mut global_metadata_training = MagicianConfig::default();
        global_metadata_training
            .analytics
            .llm_trace
            .training_default_eligible = true;
        assert!(
            enforce_llm_trace_config_invariant(&global_metadata_training)
                .expect_err("global metadata-only content cannot be training eligible")
                .to_string()
                .contains("requires global sanitized capture")
        );

        let mut metadata_training = MagicianConfig::default();
        metadata_training
            .analytics
            .llm_trace
            .operation_overrides
            .insert(
                "chat".to_string(),
                LlmTraceCaptureOverride {
                    content_mode: LlmContentMode::Metadata,
                    sanitized_content_rate: 1.0,
                    training_eligible: true,
                },
            );
        assert!(enforce_llm_trace_config_invariant(&metadata_training)
            .expect_err("metadata-only content cannot be training eligible")
            .to_string()
            .contains("only with sanitized capture"));

        let mut no_retention = MagicianConfig::default();
        no_retention.analytics.llm_trace.retention.sanitized_io_days = 0;
        no_retention.analytics.llm_trace.scope_overrides.insert(
            "owner/default".to_string(),
            LlmTraceCaptureOverride {
                content_mode: LlmContentMode::Sanitized,
                sanitized_content_rate: 1.0,
                training_eligible: true,
            },
        );
        assert!(enforce_llm_trace_config_invariant(&no_retention)
            .expect_err("sanitized capture requires bounded non-zero retention")
            .to_string()
            .contains("requires non-zero sanitized_io_days"));
    }

    #[test]
    fn llm_trace_tombstones_cannot_expire_before_sanitized_content() {
        let mut raw = MagicianConfig::default();
        raw.analytics.llm_trace.retention.facts_days = 29;
        raw.analytics.llm_trace.retention.sanitized_io_days = 30;
        let error = enforce_llm_trace_config_invariant(&raw).expect_err("retention ordering");
        assert!(error
            .to_string()
            .contains("tombstones cannot expire before protected content"));
    }

    #[test]
    fn agent_surface_runtime_has_tuning_only_and_rejects_retired_rollout_switches() {
        let defaults = AgentSurfaceRuntimeConfig::default();
        assert_eq!(defaults.context_retrieval.realtime_voice.deadline_ms, 300);
        assert_eq!(defaults.context_retrieval.autonomous_task.deadline_ms, 500);
        assert_eq!(
            defaults
                .result_projection
                .realtime_voice
                .max_serialized_bytes,
            8_192
        );

        let omitted: AgentSurfaceRuntimeConfig =
            serde_yaml::from_str("{}").expect("switch-free config");
        assert_eq!(omitted.context_retrieval.realtime_voice.deadline_ms, 300);
        assert_eq!(omitted.result_projection.chat.max_model_tokens, 4_096);

        let parsed: AgentSurfaceRuntimeConfig = serde_yaml::from_str(
            r#"
cache:
  max_schema_indexes: 64
  max_surface_plans: 512
  idle_ttl_seconds: 1800
  singleflight: true
working_set:
  max_loaded_families: 1
  max_loaded_tools: 128
  max_schema_bytes: 262144
result_projection:
  chat:
    max_model_tokens: 4096
    max_serialized_bytes: 16384
    max_records: 20
  realtime_voice:
    max_model_tokens: 2048
    max_serialized_bytes: 8192
    max_records: 10
  autonomous_task:
    max_model_tokens: 6144
    max_serialized_bytes: 24576
    max_records: 20
context_retrieval:
  chat:
    deadline_ms: 3000
  realtime_voice:
    deadline_ms: 300
  autonomous_task:
    deadline_ms: 500
"#,
        )
        .expect("tuning-only config");
        assert_eq!(parsed.working_set.max_loaded_families, 1);
        assert_eq!(parsed.working_set.max_schema_bytes, 262_144);

        for retired in [
            "mode: shadow\n",
            "chat:\n  deferred_tools: false\n",
            "realtime_voice:\n  context_budget_ms: 300\n",
            "autonomous_task:\n  shared_cache: false\n",
        ] {
            let error = serde_yaml::from_str::<AgentSurfaceRuntimeConfig>(retired)
                .expect_err("retired rollout fields must fail closed");
            assert!(error.to_string().contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn agent_surface_runtime_rejects_unsafe_projection_and_context_budgets() {
        let mut config = AgentSurfaceRuntimeConfig::default();
        config.result_projection.realtime_voice.max_records = 0;
        assert!(enforce_agent_surface_runtime_config_invariant(&config)
            .expect_err("zero record budget must fail closed")
            .to_string()
            .contains("max_records"));

        let mut config = AgentSurfaceRuntimeConfig::default();
        config.result_projection.chat.max_scalar_bytes =
            config.result_projection.chat.max_serialized_bytes + 1;
        assert!(enforce_agent_surface_runtime_config_invariant(&config)
            .expect_err("scalar budget may not exceed the serialized envelope")
            .to_string()
            .contains("max_scalar_bytes"));

        let mut config = AgentSurfaceRuntimeConfig::default();
        config.context_retrieval.autonomous_task.deadline_ms = 0;
        assert!(enforce_agent_surface_runtime_config_invariant(&config)
            .expect_err("zero retrieval deadline must fail closed")
            .to_string()
            .contains("deadline_ms"));
    }

    #[test]
    fn media_audio_profile_config_round_trips_and_validates_bindings() {
        let yaml = r#"
engines:
  local:
    label: Local audio
    enabled: true
streaming_stt:
  providers:
    - id: local-stream
      engine_id: local
      adapter: test_streaming_stt
      model: configured-model
      capabilities: [end_of_utterance]
surface_profiles:
  default_mapping:
    meeting: meeting-local
  profiles:
    meeting-local:
      surface: meeting
      turn_boundary: stt_eou
      streaming_stt:
        enabled: true
        providers: [local-stream]
"#;
        let settings: MagicianMediaSettings =
            serde_yaml::from_str(yaml).expect("media settings parse");
        enforce_media_config_invariant(&MagicianConfig {
            media: settings.clone(),
            ..MagicianConfig::default()
        })
        .expect("media settings validate");

        let rendered = serde_yaml::to_string(&settings).expect("media settings serialize");
        let reparsed: MagicianMediaSettings =
            serde_yaml::from_str(&rendered).expect("media settings reparse");
        assert_eq!(
            reparsed.streaming_stt.providers[0].model,
            "configured-model"
        );
        assert_eq!(
            reparsed
                .surface_profiles
                .default_mapping
                .get(&crate::magician_v2::media_seam::AudioSurface::Meeting)
                .map(String::as_str),
            Some("meeting-local")
        );
    }

    #[test]
    fn media_audio_profile_config_rejects_unknown_engine_and_unknown_fields() {
        let unknown_engine = r#"
engines:
  local: { enabled: true }
streaming_stt:
  providers:
    - id: remote-stream
      engine_id: remote
      adapter: test_streaming_stt
      model: configured-model
"#;
        let settings: MagicianMediaSettings =
            serde_yaml::from_str(unknown_engine).expect("schema parses before invariant");
        let error = enforce_media_config_invariant(&MagicianConfig {
            media: settings,
            ..MagicianConfig::default()
        })
        .expect_err("unknown engine must fail");
        assert!(error.to_string().contains("unknown engine remote"));

        let unknown_field = r#"
streaming_stt:
  providers:
    - id: local-stream
      engine_id: local
      adapter: test_streaming_stt
      model: configured-model
      arbitrary_model_path: /tmp/model
"#;
        let error = serde_yaml::from_str::<MagicianMediaSettings>(unknown_field)
            .expect_err("unmodeled provider fields must fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn shipped_media_catalog_keeps_streaming_models_in_config() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("template config parses");
        assert_eq!(
            config
                .media
                .streaming_stt
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "macos-speech",
                "openai-diarize",
                "openai-streaming",
                "gemini-live-transcribe",
                "fluid-parakeet-eou-en",
            ]
        );
        assert!(config
            .media
            .streaming_stt
            .providers
            .iter()
            .all(|provider| !provider.model.trim().is_empty()));
        enforce_media_config_invariant(&config).expect("template media catalog validates");
    }

    #[test]
    fn shipped_backend_realtime_profiles_request_local_user_transcription() {
        for (source, yaml) in [(
            "repository config",
            &crate::config::shipped_repo_config_yaml(),
        )] {
            let config: MagicianConfig =
                serde_yaml::from_str(yaml).unwrap_or_else(|error| panic!("{source}: {error}"));
            let realtime = &config.llm.router.as_ref().expect("router").realtime_voice;

            for profile_id in [
                "voice_realtime_openai_backend",
                "voice_realtime_openai_backend_mini",
            ] {
                assert_eq!(
                    realtime.profiles[profile_id].transcription_model.as_deref(),
                    Some("local"),
                    "{source} {profile_id} must activate the call-scoped local transcript"
                );
                assert_eq!(
                    realtime.profiles[profile_id]
                        .transcription_fallback_model
                        .as_deref(),
                    Some("whisper-1"),
                    "{source} {profile_id} must declare its vendor recovery model"
                );
            }
            assert_eq!(
                realtime.profiles["voice_realtime_default"]
                    .transcription_model
                    .as_deref(),
                Some("whisper-1"),
                "{source} direct browser WebRTC must retain provider transcription"
            );
        }
    }

    #[test]
    fn shipped_configs_retire_gpt54_from_active_routing() {
        for (source, yaml) in [(
            "repository config",
            &crate::config::shipped_repo_config_yaml(),
        )] {
            let lower = yaml.to_ascii_lowercase();
            assert!(
                !lower.contains("gpt-5.4") && !lower.contains("gpt54"),
                "{source} must not retain a GPT-5.4 model or profile identifier"
            );

            let config: MagicianConfig =
                serde_yaml::from_str(yaml).unwrap_or_else(|error| panic!("{source}: {error}"));
            let router = config.llm.router.as_ref().expect("router");
            assert!(router
                .profiles
                .values()
                .all(|profile| !profile.model.to_ascii_lowercase().starts_with("gpt-5.4")));

            assert!(router
                .profiles
                .values()
                .all(|profile| profile.model != "gpt-6-sol"));
            for (name, profile) in &router.profiles {
                if profile.model != "gpt-6.1-sol" {
                    continue;
                }
                assert_eq!(profile.supports_reasoning, Some(true), "{name}");
                assert!(
                    matches!(
                        profile.reasoning.as_ref().map(|r| r.effort.as_str()),
                        Some("low" | "medium" | "high" | "xhigh" | "max")
                    ),
                    "{name}"
                );
                assert_eq!(
                    profile
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("openai_api_mode"))
                        .and_then(serde_json::Value::as_str),
                    Some("responses"),
                    "{name}"
                );
            }
            assert_eq!(
                router.profiles["gpt6sol-responses-toolsany"].model,
                "gpt-6.1-sol"
            );
            assert_eq!(
                router.profiles["gpt6sol-responses-vision-toolsany-rnone-out16k"]
                    .reasoning
                    .as_ref()
                    .unwrap()
                    .effort,
                "low"
            );
            let normal = &router.adaptive_profiles["chat-openai-adaptive-normal"];
            assert_eq!(router.profiles[&normal.fast_profile].model, "gpt-6.1-sol");
            assert_eq!(
                router.operation_mapping["chat_completion"].default_profile(),
                "chat-openai-adaptive-instant"
            );
            let advanced = &router.adaptive_profiles["chat-openai-adaptive-advanced"];
            assert_eq!(router.profiles[&advanced.fast_profile].model, "gpt-6.1-sol");
            assert!(router.profiles[&advanced.fast_profile]
                .supports_reasoning
                .unwrap_or(true));
            assert_eq!(
                router.profiles[&advanced.thinking_profile].model,
                "gpt-6.1-sol"
            );
            assert_eq!(
                router.profiles[&advanced.thinking_profile]
                    .reasoning
                    .as_ref()
                    .map(|reasoning| reasoning.effort.as_str()),
                Some("high")
            );
            let frontier = &router.adaptive_profiles["chat-openai-adaptive-frontier"];
            assert_eq!(router.profiles[&frontier.fast_profile].model, "gpt-6-astra");
            assert_eq!(
                router.profiles[&frontier.fast_profile]
                    .reasoning
                    .as_ref()
                    .map(|reasoning| reasoning.effort.as_str()),
                Some("low")
            );
            assert_eq!(
                router.profiles[&frontier.thinking_profile].model,
                "gpt-6-astra"
            );
            assert_eq!(
                router.profiles[&frontier.thinking_profile]
                    .reasoning
                    .as_ref()
                    .map(|reasoning| reasoning.effort.as_str()),
                Some("high")
            );
            let anthropic_advanced = &router.adaptive_profiles["chat-anthropic-adaptive-advanced"];
            assert_eq!(
                router.profiles[&anthropic_advanced.fast_profile].model,
                "claude-opus-5-5"
            );
            let retained_opus5 = &router.adaptive_profiles["chat-anthropic-adaptive-opus5"];
            assert_eq!(
                router.profiles[&retained_opus5.fast_profile].model,
                "claude-opus-5"
            );
            let anthropic_frontier = &router.adaptive_profiles["chat-anthropic-adaptive-frontier"];
            assert_eq!(
                router.profiles[&anthropic_frontier.fast_profile].model,
                "claude-fable-5-1"
            );
            assert_eq!(
                router.profiles[&anthropic_frontier.fast_profile]
                    .reasoning
                    .as_ref()
                    .map(|reasoning| reasoning.effort.as_str()),
                Some("low")
            );
            let instant = &router.adaptive_profiles["chat-openai-adaptive-instant"];
            assert_eq!(router.profiles[&instant.fast_profile].model, "gpt-6-luna");
            assert_eq!(
                router.profiles["gptluna-responses-toolsany"].model,
                "gpt-5.6-luna"
            );
            assert_eq!(
                router.profiles["gptsol-responses-toolsnone-rhigh"].model,
                "gpt-5.6-sol"
            );
            let deepseek_instant = &router.adaptive_profiles["chat-deepseek-adaptive-instant"];
            assert_eq!(
                router.profiles[&deepseek_instant.fast_profile].model,
                "deepseek-flash"
            );
            assert_eq!(
                router.profiles[&deepseek_instant.fast_profile].supports_vision,
                Some(true)
            );
            assert_eq!(
                router.profiles[&deepseek_instant.thinking_profile].model,
                "deepseek-flash"
            );
            assert_eq!(
                router.operation_mapping["brainstorm_facilitation"].default_profile(),
                "chat-gpt6luna-responses-vision-toolsauto-fast"
            );
            assert_eq!(
                router.operation_mapping["agentic_input_interpretation"].default_profile(),
                "gpt6luna-chat-toolsany-rnone"
            );
            assert_eq!(router.default_profile, "gpt61sol-responses-toolsany");
        }
    }

    /// The agentic ceiling lives in one process-wide env var, and more than one
    /// test drives it. Without this they run concurrently under the default test
    /// harness and flake against each other.
    fn agentic_duration_env_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn agentic_max_duration_defaults_when_unset_and_honors_overrides() {
        use std::time::Duration;
        let _guard = agentic_duration_env_guard();
        let env_name = "MAGICIAN_AGENTIC_MAX_DURATION_SECS";
        let saved = std::env::var(env_name).ok();

        // Unset → sane default ceiling, not None (so no run is unbounded).
        std::env::remove_var(env_name);
        assert_eq!(
            agentic_max_duration(),
            Some(Duration::from_secs(DEFAULT_AGENTIC_MAX_DURATION_SECS))
        );

        // Explicit positive value overrides the default.
        std::env::set_var(env_name, "120");
        assert_eq!(agentic_max_duration(), Some(Duration::from_secs(120)));

        // Explicit 0 opts out of the ceiling entirely.
        std::env::set_var(env_name, "0");
        assert_eq!(agentic_max_duration(), None);

        match saved {
            Some(value) => std::env::set_var(env_name, value),
            None => std::env::remove_var(env_name),
        }
    }

    fn coding_settings(yaml: &str) -> Result<MagicianCodingSettings, String> {
        serde_yaml::from_str::<MagicianCodingSettings>(yaml).map_err(|error| error.to_string())
    }

    #[test]
    fn a_legacy_timeout_secs_config_still_loads() {
        // The rename must not break a live configuration. This is the exact
        // block that was shipping before `turn_timeout_secs` existed.
        let settings = coding_settings("timeout_secs: 1200\npersist_session: false\n")
            .expect("a legacy config keeps loading");
        assert_eq!(settings.turn_timeout_secs, 1200);
    }

    #[test]
    fn the_canonical_turn_timeout_key_is_read() {
        let settings = coding_settings("turn_timeout_secs: 3600\n").expect("loads");
        assert_eq!(settings.turn_timeout_secs, 3600);
    }

    #[test]
    fn conflicting_turn_timeout_keys_are_rejected() {
        // Silently preferring one is how a config change becomes a mystery: half
        // the time the running system disagrees with whoever configured it.
        let error = coding_settings("turn_timeout_secs: 3600\ntimeout_secs: 1200\n")
            .expect_err("a conflict must not resolve silently");
        assert!(error.contains("disagree"), "{error}");
        assert!(error.contains("timeout_secs"), "{error}");
    }

    #[test]
    fn matching_turn_timeout_keys_are_accepted() {
        // Agreement is not a conflict. A config mid-migration that sets both to
        // the same value has said one unambiguous thing.
        let settings =
            coding_settings("turn_timeout_secs: 1800\ntimeout_secs: 1800\n").expect("loads");
        assert_eq!(settings.turn_timeout_secs, 1800);
    }

    #[test]
    fn a_config_without_a_codex_block_stays_disabled() {
        let settings = coding_settings("persist_session: false\n").expect("loads");
        assert!(!settings.codex.enabled);
        assert_eq!(settings.codex.binary, None);
    }

    #[test]
    fn a_config_without_a_grok_block_stays_disabled() {
        let settings = coding_settings("persist_session: false\n").expect("loads");
        assert!(!settings.grok.enabled);
        assert_eq!(settings.grok.binary, None);
    }

    #[test]
    fn a_config_without_a_claude_block_stays_disabled() {
        let settings = coding_settings("persist_session: false\n").expect("loads");
        assert!(!settings.claude.enabled);
        assert!(!settings.claude.use_api_key);
        assert_eq!(settings.claude.binary, None);
        assert!(!settings.agy.enabled);
        assert_eq!(settings.agy.binary, None);
    }

    #[test]
    fn an_unknown_claude_key_is_rejected() {
        let error = coding_settings("claude:\n  home: /tmp/claude\n").expect_err("unknown");
        assert!(
            error.contains("unknown field") || error.contains("home"),
            "{error}"
        );
    }

    #[test]
    fn a_claude_kill_switch_and_operator_path_load() {
        let settings = coding_settings(
            "claude:\n  enabled: true\n  binary: /opt/claude\n  use_api_key: true\n",
        )
        .expect("loads");
        assert!(settings.claude.enabled);
        assert_eq!(settings.claude.binary.as_deref(), Some("/opt/claude"));
        assert!(settings.claude.use_api_key);
    }

    #[test]
    fn an_unknown_grok_key_is_rejected() {
        let error = coding_settings("grok:\n  home: /tmp/grok\n").expect_err("unknown");
        assert!(
            error.contains("unknown field") || error.contains("home"),
            "{error}"
        );
    }

    #[test]
    fn a_grok_kill_switch_and_operator_path_load() {
        let settings =
            coding_settings("grok:\n  enabled: true\n  binary: /opt/grok\n").expect("loads");
        assert!(settings.grok.enabled);
        assert_eq!(settings.grok.binary.as_deref(), Some("/opt/grok"));
    }

    #[test]
    fn an_unknown_codex_key_is_rejected() {
        let error = coding_settings("codex:\n  home: /tmp/codex\n").expect_err("unknown");
        assert!(
            error.contains("unknown field") || error.contains("home"),
            "{error}"
        );
    }

    #[test]
    fn a_codex_kill_switch_and_operator_path_load() {
        let settings =
            coding_settings("codex:\n  enabled: true\n  binary: /opt/codex\n").expect("loads");
        assert!(settings.codex.enabled);
        assert_eq!(settings.codex.binary.as_deref(), Some("/opt/codex"));
    }

    #[test]
    fn coding_budget_defaults_apply_to_an_empty_block() {
        let settings = coding_settings("{}\n").expect("an empty coding block loads");
        assert_eq!(
            settings.turn_timeout_secs,
            coding_budgets::DEFAULT_CODING_TURN_TIMEOUT_SECS
        );
        // No whole-task ceiling by default: duration is not what decides
        // whether a coding run should stop.
        assert_eq!(settings.task_budget_secs, 0);
        assert!(settings.no_progress.enabled);
        assert_eq!(settings.no_progress.model_idle_secs, 15 * 60);
        assert_eq!(
            settings.no_progress.tool_idle_secs,
            coding_budgets::DEFAULT_CODING_TOOL_IDLE_SECS
        );
    }

    #[test]
    fn the_no_progress_detector_can_be_switched_off_in_config() {
        let settings =
            coding_settings("no_progress:\n  enabled: false\n").expect("kill switch loads");
        assert!(!settings.no_progress.enabled);
    }

    #[test]
    fn no_progress_bounds_are_individually_overridable() {
        let settings = coding_settings("no_progress:\n  tool_idle_secs: 2400\n").expect("loads");
        assert_eq!(settings.no_progress.tool_idle_secs, 2400);
        // Untouched siblings keep their defaults rather than collapsing to zero.
        assert_eq!(
            settings.no_progress.model_idle_secs,
            coding_budgets::DEFAULT_CODING_MODEL_IDLE_SECS
        );
    }

    #[test]
    fn an_unknown_coding_key_is_still_rejected() {
        // The wire type must keep `deny_unknown_fields`, or a typo in a budget
        // name would silently take no effect.
        let error = coding_settings("turn_timeout_secs: 60\nturn_timout_secs: 60\n")
            .expect_err("a typo must not be ignored");
        assert!(error.contains("turn_timout_secs"), "{error}");
    }

    #[test]
    fn a_profile_accepts_the_legacy_timeout_key() {
        let profile: CodingProfileConfig = serde_yaml::from_str(
            "id: coding-balanced\nllm_profile: some-profile\ntimeout_secs: 900\n",
        )
        .expect("a legacy profile override keeps loading");
        assert_eq!(profile.turn_timeout_secs, Some(900));
    }

    #[test]
    fn a_profile_with_conflicting_timeout_keys_is_rejected() {
        let error = serde_yaml::from_str::<CodingProfileConfig>(
            "id: coding-balanced\nllm_profile: p\nturn_timeout_secs: 900\ntimeout_secs: 600\n",
        )
        .expect_err("a per-profile conflict is rejected too");
        assert!(error.to_string().contains("disagree"), "{error}");
    }

    #[test]
    fn coding_settings_serialize_under_the_canonical_key() {
        // Round-tripping a legacy config writes the new name, so the deprecated
        // key does not survive a rewrite.
        let settings = coding_settings("timeout_secs: 1200\n").expect("loads");
        let encoded = serde_yaml::to_string(&settings).expect("serializes");
        assert!(encoded.contains("turn_timeout_secs: 1200"), "{encoded}");
        assert!(!encoded.contains("\ntimeout_secs:"), "{encoded}");
    }

    #[test]
    fn coding_execution_ceiling_uses_the_task_budget_when_the_env_is_unset() {
        use std::time::Duration;
        let _guard = agentic_duration_env_guard();
        let env_name = "MAGICIAN_AGENTIC_MAX_DURATION_SECS";
        let saved = std::env::var(env_name).ok();

        let mut settings = MagicianCodingSettings::default();
        settings.turn_timeout_secs = 8 * 3600;
        settings.task_budget_secs = 0;
        coding_budgets::configure_coding_budgets(&settings);

        // Unset: the generic 40-minute default is not an operator decision, and
        // with no task ceiling a coding execution gets no wall clock at all —
        // the per-turn bounds and liveness govern. Leaving the generic default
        // here is what made raising the turn budget do nothing at all.
        std::env::remove_var(env_name);
        assert_eq!(coding_execution_max_duration(), None);

        // A configured task ceiling does bound the execution, never below one turn.
        settings.task_budget_secs = 12 * 3600;
        coding_budgets::configure_coding_budgets(&settings);
        assert_eq!(
            coding_execution_max_duration(),
            Some(Duration::from_secs(12 * 3600))
        );

        // Explicitly set: that IS a decision, in both directions.
        std::env::set_var(env_name, "300");
        assert_eq!(
            coding_execution_max_duration(),
            Some(Duration::from_secs(300))
        );
        std::env::set_var(env_name, "0");
        assert_eq!(coding_execution_max_duration(), None);

        match saved {
            Some(value) => std::env::set_var(env_name, value),
            None => std::env::remove_var(env_name),
        }
        coding_budgets::configure_coding_budgets(&MagicianCodingSettings::default());
    }

    #[test]
    fn public_chat_config_invariant_accepts_absent_policy() {
        let config = MagicianConfig::default();

        enforce_public_chat_config_invariant(&config)
            .expect("absent public-chat policy should preserve legacy behavior");
    }

    #[test]
    fn public_chat_config_invariant_accepts_valid_enabled_policy() {
        let config = test_public_chat_config();

        enforce_public_chat_config_invariant(&config)
            .expect("valid public-chat policy should be accepted");
    }

    #[test]
    fn public_chat_config_invariant_rejects_missing_operation_mapping() {
        let mut config = test_public_chat_config();
        config.llm.router = Some(test_llm_router_with_operations(&["kapso_envoy_chat"]));

        let error = enforce_public_chat_config_invariant(&config)
            .expect_err("fallback operation mapping should be required");

        assert!(error
            .to_string()
            .contains("public_chat.kapso_envoy_chat.fallback_operation"));
    }

    #[test]
    fn public_chat_config_invariant_rejects_zero_capacity_limits() {
        let mut config = test_public_chat_config();
        config
            .public_chat
            .kapso_envoy_chat
            .as_mut()
            .expect("policy")
            .max_paid_concurrent = 0;

        let error = enforce_public_chat_config_invariant(&config)
            .expect_err("zero paid concurrency would deadlock the paid lane");

        assert!(error
            .to_string()
            .contains("public_chat.kapso_envoy_chat.max_paid_concurrent"));
    }

    #[test]
    fn public_chat_config_invariant_rejects_invalid_first_contact_debounce_ordering() {
        let mut config = test_public_chat_config();
        let first_contact = config
            .public_chat
            .kapso_envoy_chat
            .as_mut()
            .and_then(|policy| policy.first_contact.as_mut())
            .expect("first-contact policy");
        first_contact.debounce_window_ms = 2_000;
        first_contact.min_response_delay_ms = 3_000;

        let error = enforce_public_chat_config_invariant(&config)
            .expect_err("min response delay must not exceed debounce window");

        assert!(error
            .to_string()
            .contains("first_contact.min_response_delay_ms must be <= debounce_window_ms"));
    }

    #[test]
    fn public_chat_config_invariant_rejects_enabled_identity_research_without_min_details() {
        let mut config = test_public_chat_config();
        let identity_research = config
            .public_chat
            .kapso_envoy_chat
            .as_mut()
            .and_then(|policy| policy.identity_research.as_mut())
            .expect("identity-research policy");
        identity_research.min_details.claimed_name_or_org = false;
        identity_research.min_details.purpose = false;

        let error = enforce_public_chat_config_invariant(&config)
            .expect_err("identity research must require at least one user-provided detail");

        assert!(error
            .to_string()
            .contains("identity_research.min_details must require at least one detail"));
    }

    #[test]
    fn public_chat_config_invariant_ignores_disabled_identity_research_policy() {
        let mut config = test_public_chat_config();
        let identity_research = config
            .public_chat
            .kapso_envoy_chat
            .as_mut()
            .and_then(|policy| policy.identity_research.as_mut())
            .expect("identity-research policy");
        identity_research.enabled = false;
        identity_research.max_concurrent = 0;
        identity_research.worker_agent_id.clear();
        identity_research.min_details.claimed_name_or_org = false;
        identity_research.min_details.purpose = false;

        enforce_public_chat_config_invariant(&config)
            .expect("disabled identity-research policy should not block startup");
    }

    #[test]
    fn router_config_available_when_profiles_exist() {
        let mut config = MagicianConfig::default();

        let mut router = LLMRouterConfig::default();
        router.default_profile = "fast".into();
        router.profiles.insert(
            "fast".into(),
            magicllm::config::LLMProfile {
                provider: magicllm::capability::LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".into(),
                api_key_env: Some("OPENAI_API_KEY".into()),
                api_base_url: None,
                temperature: Some(0.5),
                max_output_tokens: Some(1024),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(true),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        router
            .operation_mapping
            .insert("planning".into(), "fast".into());

        config.llm = MagicianLlmSettings {
            router: Some(router),
            dispatch: Default::default(),
        };

        let resolved = config.router_config().expect("router config should exist");
        assert_eq!(resolved.default_profile, "fast");
        assert!(resolved.profiles.contains_key("fast"));
        assert_eq!(
            resolved
                .operation_mapping
                .get("planning")
                .map(|selector| selector.default_profile()),
            Some("fast")
        );
    }

    #[test]
    fn default_runtime_context_config_uses_durable_state_helpers() {
        assert_eq!(
            MagicianConfig::default().task_state.policy,
            "llm_lazy_optional"
        );
    }

    #[test]
    fn runtime_ollama_policy_resolves_local_prep_without_overriding_profiles() {
        let mut config = MagicianConfig::default();
        config.runtime.ollama.keep_alive = Some("17m".to_string());
        let resolved_keep_alive = config
            .resolved_ollama_keep_alive()
            .expect("runtime Ollama keep_alive should resolve");

        let mut router = LLMRouterConfig::default();
        router.default_profile = "local".into();
        router.profiles.insert(
            "local".into(),
            magicllm::config::LLMProfile {
                provider: magicllm::capability::LLMProviderKind::Ollama,
                model: "test-local".into(),
                api_key_env: None,
                api_base_url: Some("http://localhost:11434/api/generate".into()),
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: Some(std::collections::HashMap::from([(
                    "options".to_string(),
                    serde_json::json!({"num_ctx": 4096}),
                )])),
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        router.profiles.insert(
            "explicit".into(),
            magicllm::config::LLMProfile {
                provider: magicllm::capability::LLMProviderKind::Ollama,
                model: "test-explicit".into(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: Some(std::collections::HashMap::from([
                    (
                        "keep_alive".to_string(),
                        serde_json::Value::String("1m".to_string()),
                    ),
                    ("options".to_string(), serde_json::json!({"num_ctx": 8192})),
                ])),
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.llm.router = Some(router);
        config.llm.dispatch.local_prep.operation = Some("local_prep".to_string());
        config
            .llm
            .router
            .as_mut()
            .expect("router should exist")
            .operation_mapping
            .insert("local_prep".to_string(), "local".into());
        config
            .llm
            .router
            .as_mut()
            .expect("router should exist")
            .operation_mapping
            .insert("meeting_summary".to_string(), "local".into());

        let generation_models =
            resolved_ollama_generation_models(&config).expect("mapped Ollama model should resolve");
        assert_eq!(
            generation_models,
            vec![OllamaGenerationModelConfig {
                model: "test-local".to_string(),
                context_tokens: 4_096,
            }]
        );

        normalize_runtime_ollama_policy(&mut config, Some(resolved_keep_alive.clone()))
            .expect("policy should resolve");

        assert_eq!(
            config.llm.dispatch.local_prep.keep_alive.as_deref(),
            Some(resolved_keep_alive.as_str())
        );
        assert_eq!(config.llm.dispatch.local_prep.model, "test-local");
        assert_eq!(config.llm.dispatch.local_prep.context_tokens, 4_096);
        assert_eq!(
            config.llm.dispatch.local_prep.base_url,
            "http://localhost:11434"
        );
        let router = config.llm.router.as_ref().expect("router should remain");
        assert_eq!(
            router
                .profiles
                .get("local")
                .and_then(|profile| profile.metadata.as_ref())
                .and_then(|metadata| metadata.get("keep_alive"))
                .and_then(|value| value.as_str()),
            Some(resolved_keep_alive.as_str())
        );
        assert_eq!(
            router
                .profiles
                .get("explicit")
                .and_then(|profile| profile.metadata.as_ref())
                .and_then(|metadata| metadata.get("keep_alive"))
                .and_then(|value| value.as_str()),
            Some("1m")
        );
        assert_eq!(
            router
                .profiles
                .get("local")
                .and_then(|profile| profile.metadata.as_ref())
                .and_then(|metadata| metadata.get("options"))
                .and_then(|options| options.get("num_ctx"))
                .and_then(|value| value.as_u64()),
            Some(4_096)
        );
    }

    #[test]
    fn runtime_context_invariant_rejects_legacy_taskplan_operation_mappings() {
        let mut config = MagicianConfig::default();
        let mut router = LLMRouterConfig::default();
        router.operation_mapping.insert(
            "taskplan_generate".to_string(),
            "gptterra-responses-toolsnone-out8k".into(),
        );
        config.llm.router = Some(router);

        let error = enforce_runtime_context_config_invariant(&config)
            .expect_err("legacy taskplan operation should be rejected");
        assert!(error
            .to_string()
            .contains("legacy taskplan operation mappings"));
    }

    #[test]
    fn runtime_ollama_invariant_rejects_invalid_embedding_batch_tokens() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.runtime.ollama.embedding_batch_tokens = 0;

        let error = enforce_runtime_ollama_config_invariant(&config)
            .expect_err("zero embedding batch tokens should be rejected");
        assert!(error
            .to_string()
            .contains("runtime.ollama.embedding_batch_tokens must be between 32 and 8192"));
    }

    #[test]
    fn runtime_ollama_invariant_rejects_invalid_embedding_batch_size() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.runtime.ollama.embedding_batch_size = 0;

        let error = enforce_runtime_ollama_config_invariant(&config)
            .expect_err("zero embedding request batch size should be rejected");
        assert!(error
            .to_string()
            .contains("runtime.ollama.embedding_batch_size must be between 1 and 256"));
    }

    #[test]
    fn runtime_retrieval_result_cache_defaults_on_and_rejects_zero_bounds() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        assert!(config.runtime.retrieval.result_cache.enabled);
        assert_eq!(
            config.runtime.retrieval.result_cache.max_entries,
            DEFAULT_RUNTIME_RESULT_CACHE_MAX_ENTRIES
        );
        assert_eq!(
            config.runtime.retrieval.result_cache.max_bytes,
            DEFAULT_RUNTIME_RESULT_CACHE_MAX_BYTES
        );
        assert!(config.runtime.retrieval.lance_table_pool.enabled);
        assert_eq!(
            config.runtime.retrieval.lance_table_pool.max_idle,
            DEFAULT_RUNTIME_LANCE_TABLE_POOL_MAX_IDLE
        );
        assert!(config.runtime.retrieval.query_vector_cache.enabled);
        assert_eq!(
            config.runtime.retrieval.query_vector_cache.max_entries,
            DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_ENTRIES
        );
        assert_eq!(
            config.runtime.retrieval.vector_search,
            MagicianVectorSearchMode::Ann
        );
        assert_eq!(
            config.runtime.retrieval.ann.min_rows,
            DEFAULT_RUNTIME_VECTOR_SEARCH_MIN_ROWS
        );
        assert_eq!(
            config.runtime.retrieval.ann.candidate_multiplier,
            DEFAULT_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER
        );
        assert_eq!(
            config.runtime.ollama.embedding_query_batch_window_ms,
            DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_WINDOW_MS
        );
        assert_eq!(
            config.runtime.ollama.embedding_query_batch_max_items,
            DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_ITEMS
        );
        assert_eq!(
            config.runtime.ollama.embedding_query_batch_max_chars,
            DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_CHARS
        );
        assert_eq!(DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_WINDOW_MS, 3);
        assert_eq!(DEFAULT_RUNTIME_EMBEDDING_QUERY_BATCH_MAX_ITEMS, 8);
        enforce_runtime_retrieval_config_invariant(&config)
            .expect("seed result-cache bounds must be valid");

        let mut invalid = config.clone();
        invalid.runtime.retrieval.result_cache.max_entries = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero entry cap must fail closed");
        assert!(error.to_string().contains("max_entries"));
        invalid.runtime.retrieval.result_cache.max_entries =
            DEFAULT_RUNTIME_RESULT_CACHE_MAX_ENTRIES;
        invalid.runtime.retrieval.result_cache.max_bytes = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero byte cap must fail closed");
        assert!(error.to_string().contains("max_bytes"));
        invalid.runtime.retrieval.result_cache.max_bytes = DEFAULT_RUNTIME_RESULT_CACHE_MAX_BYTES;
        invalid.runtime.retrieval.lance_table_pool.max_idle = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero idle cap must fail closed");
        assert!(error.to_string().contains("max_idle"));
        invalid.runtime.retrieval.lance_table_pool.max_idle =
            DEFAULT_RUNTIME_LANCE_TABLE_POOL_MAX_IDLE;
        invalid.runtime.retrieval.query_vector_cache.max_entries = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero query-vector entries must fail closed");
        assert!(error.to_string().contains("query_vector_cache.max_entries"));
        invalid.runtime.retrieval.query_vector_cache.max_entries =
            DEFAULT_RUNTIME_QUERY_VECTOR_CACHE_MAX_ENTRIES;
        invalid.runtime.retrieval.ann.min_rows = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero IVF min_rows must fail closed");
        assert!(error.to_string().contains("ann.min_rows"));
        invalid.runtime.retrieval.ann.min_rows = DEFAULT_RUNTIME_VECTOR_SEARCH_MIN_ROWS;
        invalid.runtime.retrieval.ann.candidate_multiplier = 0;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("zero ANN multiplier must fail closed");
        assert!(error.to_string().contains("candidate_multiplier"));
        invalid.runtime.retrieval.ann.candidate_multiplier =
            MAX_RUNTIME_VECTOR_SEARCH_CANDIDATE_MULTIPLIER + 1;
        let error = enforce_runtime_retrieval_config_invariant(&invalid)
            .expect_err("ANN multiplier above cap must fail closed");
        assert!(error.to_string().contains("candidate_multiplier"));
    }

    #[test]
    fn runtime_retrieval_rejects_unknown_vector_search_mode() {
        let mut yaml = crate::config::shipped_repo_config_yaml();
        yaml = yaml.replace("vector_search: ann", "vector_search: not_a_mode");
        let error = serde_yaml::from_str::<MagicianConfig>(&yaml)
            .expect_err("unknown vector_search mode must fail parse");
        let message = error.to_string();
        assert!(
            message.contains("vector_search") || message.contains("not_a_mode"),
            "unexpected parse error: {message}"
        );
    }

    #[test]
    fn seed_dispatch_capacity_matches_current_plan_and_uses_isolated_engine() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        let plan = magicllm::dispatch::DispatchCapacityPlan::from_config(&config.llm.dispatch);
        assert_eq!(plan, magicllm::dispatch::DispatchCapacityPlan::CURRENT);
        assert_eq!(
            config.llm.dispatch.engine,
            magicllm::dispatch::DispatchEngine::ProviderIsolated
        );
        enforce_dispatch_config_invariant(&config).expect("seed dispatch capacity must be valid");

        let mut legacy = config.clone();
        legacy.llm.dispatch.engine = magicllm::dispatch::DispatchEngine::LegacyWorkerPool;
        enforce_dispatch_config_invariant(&legacy)
            .expect("legacy_worker_pool remains a valid rollback engine");

        let mut zero_workers = config;
        zero_workers.llm.dispatch.workers = 0;
        let error = enforce_dispatch_config_invariant(&zero_workers)
            .expect_err("zero workers must fail closed");
        assert!(error.to_string().contains("workers"));
    }

    #[test]
    fn seed_runtime_scale_is_current_and_resolves_current_dispatch() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        assert_eq!(config.runtime.scale.profile, ScaleProfile::Current);
        assert!(config.runtime.scale.overrides.workers.is_none());
        assert!(config
            .runtime
            .scale
            .overrides
            .execution_worker_threads
            .is_none());
        let leftover = LeftoverDispatchScalars::from(&config.llm.dispatch);
        assert_eq!(
            leftover.workers,
            magicllm::dispatch::DispatchCapacityPlan::CURRENT.workers
        );
        let plan = resolve_boot_runtime_plan(
            config.runtime.scale.profile,
            &config.runtime.scale.overrides,
            12,
            None,
            leftover,
        )
        .expect("current seed must resolve");
        assert_eq!(plan.selected_profile, ScaleProfile::Current);
        assert_eq!(plan.workers, 12);
        assert_eq!(plan.execution_worker_threads, 4);
        assert_eq!(plan.live_agent_limit, 50);
        assert_eq!(plan.main_worker_threads, 12);
        assert_eq!(plan.http_worker_threads, 4);
    }

    #[test]
    fn seed_m2_max_with_leftover_current_workers_fails_closed() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.runtime.scale.profile = ScaleProfile::M2Max;
        let error = resolve_boot_runtime_plan(
            config.runtime.scale.profile,
            &config.runtime.scale.overrides,
            12,
            None,
            LeftoverDispatchScalars::from(&config.llm.dispatch),
        )
        .expect_err("m2_max plus workers 12 must fail closed");
        assert!(error.to_string().contains("m2_max"));
        assert!(error.to_string().contains("12"));
    }

    #[test]
    fn explicit_override_workers_wins_on_current_seed() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.runtime.scale.overrides.workers = Some(8);
        let plan = resolve_boot_runtime_plan(
            config.runtime.scale.profile,
            &config.runtime.scale.overrides,
            12,
            None,
            LeftoverDispatchScalars::from(&config.llm.dispatch),
        )
        .expect("override");
        assert_eq!(plan.workers, 8);
        assert_eq!(
            plan.queue_capacity_normal,
            magicllm::dispatch::DispatchCapacityPlan::CURRENT.queue_capacity_normal
        );
    }

    #[test]
    fn runtime_ollama_invariant_supports_single_sequence_pinned_embedding_daemon() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.runtime.ollama.embedding_num_parallel = 1;
        enforce_runtime_ollama_config_invariant(&config)
            .expect("foreground priority must support a single-sequence embedding runner");

        config.runtime.ollama.embedding_num_parallel = 2;
        let error = enforce_runtime_ollama_config_invariant(&config)
            .expect_err("unverified multi-sequence embedding must fail closed");
        assert!(error.to_string().contains("must be 1"));
        config.runtime.ollama.embedding_num_parallel = 1;

        config.runtime.ollama.embedding_keep_alive = "10m".to_string();
        let error = enforce_runtime_ollama_config_invariant(&config)
            .expect_err("finite embedding residency should be rejected");
        assert!(error.to_string().contains("must be -1"));

        config.runtime.ollama.embedding_keep_alive = "-1".to_string();
        config.runtime.ollama.embedding_base_url = "http://127.0.0.1:11434".to_string();
        let error = enforce_runtime_ollama_config_invariant(&config)
            .expect_err("embedding and generation must not share a listener");
        assert!(error.to_string().contains("different listener"));
    }

    #[test]
    fn rejected_config_never_reaches_process_global_runtime_policy_commit() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config.analytics.llm_trace.metadata_capture_rate = 2.0;
        let applied = std::cell::Cell::new(false);

        let error = validate_and_apply_magician_config(config, |_| {
            applied.set(true);
            Ok(())
        })
        .expect_err("invalid late-stage config must be rejected");

        assert!(error
            .to_string()
            .contains("analytics.llm_trace.metadata_capture_rate"));
        assert!(
            !applied.get(),
            "a rejected reload must leave the last accepted process-global provider policy untouched"
        );
    }

    #[test]
    fn explicit_config_path_is_nonempty_and_preserved_verbatim() {
        assert_eq!(explicit_magician_config_path(None), None);
        assert_eq!(
            explicit_magician_config_path(Some(std::ffi::OsString::new())),
            None
        );
        assert_eq!(
            explicit_magician_config_path(Some(std::ffi::OsString::from("/tmp/eval config.yaml",))),
            Some(PathBuf::from("/tmp/eval config.yaml"))
        );
    }

    #[test]
    fn ollama_generation_resolver_rejects_unknown_mapped_profile() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production config should parse");
        config
            .llm
            .router
            .as_mut()
            .expect("router should exist")
            .operation_mapping
            .insert("broken_operation".to_string(), "missing-profile".into());

        let error = resolved_ollama_generation_models(&config)
            .expect_err("unknown mapped profiles must fail closed");
        assert!(error
            .to_string()
            .contains("operation mapping references missing profile `missing-profile`"));
    }

    #[test]
    fn runtime_context_invariant_rejects_legacy_taskplan_profile_ids() {
        let mut config = MagicianConfig::default();
        let mut router = LLMRouterConfig::default();
        router.profiles.insert(
            "llm-taskplan-openai-reasoning-medium-5.4-mini".into(),
            magicllm::config::LLMProfile {
                provider: magicllm::capability::LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".into(),
                api_key_env: Some("OPENAI_API_KEY".into()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: Some(8192),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(true),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.llm.router = Some(router);

        let error = enforce_runtime_context_config_invariant(&config)
            .expect_err("legacy taskplan profile should be rejected");
        assert!(error
            .to_string()
            .contains("legacy taskplan LLM profile ids"));
    }

    #[test]
    fn config_validation_rejects_execution_strategy_switches() {
        let error = validate_magician_config_yaml("execution_strategy: runtime_context\n")
            .expect_err("top-level execution strategy should be rejected");
        assert!(error.contains("top-level.execution_strategy"));

        let error = validate_magician_config_yaml("task_state:\n  default_action: none\n")
            .expect_err("task-state default action should be rejected");
        assert!(error.contains("task_state.default_action"));
    }

    #[test]
    fn validation_rejects_legacy_global_bots_block() {
        let error = validate_magician_config_yaml("bots:\n  telegram:\n    enabled: true\n")
            .expect_err("legacy top-level bot config should be rejected");
        assert!(error.contains("top-level `bots:` is no longer supported"));
    }

    #[test]
    fn validation_rejects_legacy_storage_config_block() {
        let error = validate_magician_config_yaml("storage:\n  provider: silverbullet_space\n")
            .expect_err("storage provider config should be API managed");
        assert!(error.contains("top-level.storage"));
    }

    #[test]
    fn memory_config_rejects_unknown_lane_budget_keys() {
        let mut config = MagicianConfig::default();
        config.memory.prompt_lane_budgets.user.insert(
            "unknown_lane".to_string(),
            ConfiguredMemoryLaneBudget {
                max_entries: Some(1),
                max_chars: None,
            },
        );

        let error = enforce_memory_config_invariant(&config)
            .expect_err("unknown memory lane should be rejected");
        assert!(error
            .to_string()
            .contains("memory.prompt_lane_budgets.user contains unknown lane"));
    }

    #[test]
    fn memory_config_rejects_unbounded_prompt_snapshot() {
        let mut config = MagicianConfig::default();
        config.memory.prompt_snapshot.max_entries = 65;

        let error = enforce_memory_config_invariant(&config)
            .expect_err("prompt snapshot entry bound should be enforced");
        assert!(error
            .to_string()
            .contains("memory.prompt_snapshot.max_entries"));

        config.memory.prompt_snapshot.enabled = false;
        enforce_memory_config_invariant(&config)
            .expect("disabled prompt snapshot should accept inert bounds");
    }

    /// A note path the notes read path can never resolve would otherwise
    /// present as a profile that is simply always missing.
    #[test]
    fn memory_config_rejects_an_unreadable_taste_profile_path() {
        let mut config = MagicianConfig::default();
        enforce_memory_config_invariant(&config)
            .expect("the taste profile defaults must load unchallenged");

        config.memory.taste_profile.note_path = "../profile.md".to_string();
        let error = enforce_memory_config_invariant(&config)
            .expect_err("a path escaping the notes root should be rejected");
        assert!(error.to_string().contains("memory.taste_profile.note_path"));

        config.memory.taste_profile.enabled = false;
        enforce_memory_config_invariant(&config)
            .expect("a disabled taste profile should accept inert settings");
    }

    #[test]
    fn memory_config_rejects_empty_scope_budget_override() {
        let mut config = MagicianConfig::default();
        config.memory.prompt_scope_budgets.user = ConfiguredMemoryScopeBudget::default();

        let error = enforce_memory_config_invariant(&config)
            .expect_err("empty explicit scope budget should be rejected");
        assert!(error
            .to_string()
            .contains("memory.prompt_scope_budgets.user must set max_entries or max_chars"));
    }

    #[test]
    fn memory_config_accepts_partial_scope_budget_override() {
        let mut config = MagicianConfig::default();
        config.memory.prompt_scope_budgets.agent_goal = ConfiguredMemoryScopeBudget {
            max_entries: Some(32),
            max_chars: None,
        };

        enforce_memory_config_invariant(&config)
            .expect("valid partial scope budget override should be accepted");
    }

    #[test]
    fn memory_config_accepts_partial_valid_lane_budget_override() {
        let mut config = MagicianConfig::default();
        config.memory.prompt_lane_budgets.agent_goal.insert(
            "project_context".to_string(),
            ConfiguredMemoryLaneBudget {
                max_entries: Some(6),
                max_chars: None,
            },
        );

        enforce_memory_config_invariant(&config)
            .expect("valid partial lane budget override should be accepted");
    }

    #[test]
    fn production_config_uses_runtime_context_and_durable_state_helpers() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production magician-config.yaml should parse");
        enforce_runtime_context_config_invariant(&config)
            .expect("production config should satisfy runtime-context invariant");
        enforce_runtime_ollama_config_invariant(&config)
            .expect("production config should satisfy Ollama invariant");
        enforce_memory_config_invariant(&config)
            .expect("production config should satisfy memory config invariant");
        enforce_channel_assist_config_invariant(&config)
            .expect("production config should satisfy channel-assist config invariant");
        assert_eq!(
            config.runtime.ollama.keep_alive.as_deref(),
            Some(DEFAULT_RUNTIME_OLLAMA_KEEP_ALIVE)
        );
        assert_eq!(
            config.runtime.ollama.embedding_base_url,
            DEFAULT_RUNTIME_OLLAMA_EMBEDDING_BASE_URL
        );
        assert_eq!(
            config.runtime.ollama.embedding_keep_alive,
            DEFAULT_RUNTIME_OLLAMA_EMBEDDING_KEEP_ALIVE
        );
        assert_eq!(config.runtime.ollama.embedding_num_parallel, 1);
        assert_eq!(config.runtime.ollama.embedding_max_loaded_models, 1);
        assert_eq!(config.runtime.ollama.embedding_query_timeout_ms, 5_000);
        assert_eq!(config.runtime.ollama.embedding_write_timeout_ms, 180_000);
        assert_eq!(config.task_state.policy, "llm_lazy_optional");
        assert_eq!(config.channel_assist.distillation.brief_contract_version, 2);
        assert_eq!(config.channel_assist.distillation.summary_max_chars, 900);
        assert!(config.channel_assist.distillation.backfill.enabled);
        assert_eq!(config.channel_assist.distillation.backfill.batch_size, 1);
        assert!(config.resurfacing.rich_briefs_enabled);
        assert!(config.resurfacing.source_details_enabled);
        assert!(config.resurfacing.contextual_actions_enabled);
        assert!(config.resurfacing.recommendations_enabled);
        assert!(config.resurfacing.active_repair_enabled);
        assert_eq!(config.resurfacing.active_repair_batch_size, 10);
        assert_eq!(config.resurfacing.surface_cap, 15);

        let repo_seed: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("repo-root magician-config.yaml should parse");
        enforce_channel_assist_config_invariant(&repo_seed)
            .expect("repo-root config should satisfy channel-assist config invariant");
        assert_eq!(
            repo_seed.channel_assist.distillation.brief_contract_version,
            config.channel_assist.distillation.brief_contract_version
        );

        let router = config
            .router_config()
            .expect("production config should expose router config");
        for legacy_operation in LEGACY_TASKPLAN_OPERATIONS {
            assert!(
                !router.operation_mapping.contains_key(*legacy_operation),
                "legacy operation mapping should be absent: {legacy_operation}"
            );
        }
        for helper_operation in [
            "durable_task_state_generate",
            "durable_task_state_patch",
            "durable_task_state_close_summary",
            "durable_task_state_generate_retry",
            "durable_task_state_patch_retry",
        ] {
            assert!(
                router.operation_mapping.contains_key(helper_operation),
                "durable-state helper mapping should exist: {helper_operation}"
            );
        }
        assert!(
            router
                .profiles
                .keys()
                .all(|profile| !profile.starts_with(LEGACY_TASKPLAN_PROFILE_PREFIX)),
            "production config should not define legacy taskplan profile ids"
        );
    }

    #[test]
    fn envoy_config_resolves_owner_identities_from_envs() {
        let env_name = "MAGICIAN_TEST_ENVOY_OWNER_IDENTITIES";
        std::env::set_var(
            env_name,
            "owner@example.com; second@example.com\nowner@example.com, third@example.com",
        );

        let mut owner_identity_envs = HashMap::new();
        owner_identity_envs.insert("agentmail".to_string(), env_name.to_string());
        let config = EnvoyConfig {
            owner_identity_envs,
            ..EnvoyConfig::default()
        };

        assert_eq!(
            config.owner_identities_for("agentmail"),
            vec![
                "owner@example.com".to_string(),
                "second@example.com".to_string(),
                "third@example.com".to_string(),
            ]
        );
        assert!(config.has_owner_identity("agentmail", "second@example.com"));

        std::env::remove_var(env_name);
    }

    #[test]
    fn envoy_config_merges_configured_and_env_owner_identities() {
        let env_name = "MAGICIAN_TEST_ENVOY_OWNER_IDENTITY_MERGE";
        std::env::set_var(env_name, "env@example.com, config@example.com");

        let mut owner_identities = HashMap::new();
        owner_identities.insert(
            "agentmail".to_string(),
            vec![
                "config@example.com".to_string(),
                "another@example.com".to_string(),
            ],
        );
        let mut owner_identity_envs = HashMap::new();
        owner_identity_envs.insert("agentmail".to_string(), env_name.to_string());
        let config = EnvoyConfig {
            owner_identities,
            owner_identity_envs,
            ..EnvoyConfig::default()
        };

        assert_eq!(
            config.owner_identities_for("agentmail"),
            vec![
                "config@example.com".to_string(),
                "another@example.com".to_string(),
                "env@example.com".to_string(),
            ]
        );

        std::env::remove_var(env_name);
    }

    #[test]
    fn production_execution_profiles_explicitly_enable_tool_calling() {
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production magician-config.yaml should parse");
        let router = config
            .router_config()
            .expect("production config should expose router config");

        let missing_or_disabled: Vec<_> = router
            .profiles
            .iter()
            .filter_map(|(name, profile)| {
                (!is_text_only_harness_profile(profile)
                    && profile.supports_tool_calling != Some(true))
                .then_some(name.as_str())
            })
            .collect();

        assert!(
            missing_or_disabled.is_empty(),
            "all production native-execution LLM profiles should explicitly enable tool calling, missing/disabled: {:?}",
            missing_or_disabled
        );
    }

    #[test]
    fn tool_calling_exemption_is_bound_to_harness_provider_not_profile_name() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production magician config template parses");
        let router = config
            .llm
            .router
            .as_mut()
            .expect("production config exposes router config");

        let genuine = router
            .profiles
            .remove("op-harness-claude")
            .expect("production harness profile exists");
        router
            .profiles
            .insert("renamed-subscription-cli".to_owned(), genuine.clone());
        enforce_tool_calling_invariant(&config)
            .expect("a genuine harness provider does not depend on a naming convention");

        let router = config.llm.router.as_mut().expect("router remains present");
        let mut spoof = genuine;
        spoof.provider = LLMProviderKind::OpenAI;
        spoof.supports_tool_calling = Some(false);
        router
            .profiles
            .insert("op-harness-prefix-spoof".to_owned(), spoof);

        let error = enforce_tool_calling_invariant(&config)
            .expect_err("an ordinary provider cannot claim the harness exemption by name");
        assert!(error.to_string().contains("op-harness-prefix-spoof"));
    }

    #[test]
    fn workspace_storage_folds_into_config_and_round_trips() {
        // Provider is read from the config (folded in from the former
        // workspace_storage/settings.json).
        let config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("production magician-config.yaml should parse");
        assert_eq!(
            config.workspace_storage.provider, "silverbullet_space",
            "template should select the silverbullet_space provider"
        );

        // The settings API mutates by load-edit-save: prove a deserialize ->
        // serialize -> deserialize round-trip is lossless and stays valid under
        // `deny_unknown_fields` (every serialized key must be a modeled field).
        let serialized = serde_yaml::to_string(&config).expect("config should serialize");
        let reparsed: MagicianConfig =
            serde_yaml::from_str(&serialized).expect("round-tripped config must re-parse");
        assert_eq!(reparsed.workspace_storage.provider, "silverbullet_space");
        enforce_runtime_context_config_invariant(&reparsed)
            .expect("round-tripped config must still satisfy the runtime-context invariant");
    }

    #[test]
    fn container_host_rewrites_only_loopback_ollama_endpoints() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("template config parses");
        let router = config.llm.router.as_mut().expect("router");
        let remote_profile = router
            .profiles
            .iter_mut()
            .find(|(_, profile)| profile.provider == LLMProviderKind::Ollama)
            .expect("Ollama profile");
        remote_profile.1.api_base_url =
            Some("https://ollama.example.test/api/generate".to_string());
        let remote_profile_name = remote_profile.0.clone();

        apply_runtime_service_endpoint_values(
            &mut config,
            Some("host.container.internal"),
            None,
            None,
        )
        .expect("container endpoint rewrite");

        let router = config.llm.router.as_ref().expect("router");
        assert_eq!(
            router.profiles[&remote_profile_name]
                .api_base_url
                .as_deref(),
            Some("https://ollama.example.test/api/generate")
        );
        assert!(router
            .profiles
            .values()
            .filter(|profile| profile.provider == LLMProviderKind::Ollama)
            .filter_map(|profile| profile.api_base_url.as_deref())
            .any(|endpoint| endpoint == "http://host.container.internal:11434/api/generate"));
        assert_eq!(
            config.runtime.ollama.embedding_base_url,
            "http://host.container.internal:11435"
        );
    }

    #[test]
    fn explicit_ollama_deployment_overrides_normalize_endpoints() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("template config parses");

        apply_runtime_service_endpoint_values(
            &mut config,
            None,
            Some("http://host.docker.internal:11434/"),
            Some("http://host.docker.internal:11435/"),
        )
        .expect("explicit endpoint overrides");

        assert!(config
            .llm
            .router
            .as_ref()
            .expect("router")
            .profiles
            .iter()
            .filter(|(name, _)| !config.app_platform.processing.profiles.contains_key(*name))
            .map(|(_, profile)| profile)
            .filter(|profile| profile.provider == LLMProviderKind::Ollama)
            .filter(|profile| !profile_is_embedding_profile(profile))
            .all(|profile| profile.api_base_url.as_deref()
                == Some("http://host.docker.internal:11434/api/generate")));
        // The routed embedding profile is exempt from the generation override
        // (wrong daemon, wrong path) and instead follows the embedding
        // override so routed and direct-fallback paths cannot disagree.
        let embedding_profile = config
            .llm
            .router
            .as_ref()
            .expect("router")
            .profiles
            .get("op-embedding-local")
            .expect("template defines the routed embedding profile");
        assert_eq!(
            embedding_profile.api_base_url.as_deref(),
            Some("http://host.docker.internal:11435")
        );
        assert_eq!(
            config.runtime.ollama.embedding_base_url,
            "http://host.docker.internal:11435"
        );
    }

    #[test]
    fn generation_override_alone_leaves_embedding_profile_on_its_daemon() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
                .expect("template config parses");

        apply_runtime_service_endpoint_values(
            &mut config,
            None,
            Some("http://host.docker.internal:11434"),
            None,
        )
        .expect("generation-only override");

        let embedding_profile = config
            .llm
            .router
            .as_ref()
            .expect("router")
            .profiles
            .get("op-embedding-local")
            .expect("template defines the routed embedding profile");
        assert_eq!(
            embedding_profile.api_base_url.as_deref(),
            Some("http://127.0.0.1:11435")
        );
        assert_eq!(
            config.runtime.ollama.embedding_base_url,
            "http://127.0.0.1:11435"
        );
    }

    #[test]
    fn runtime_service_endpoint_overrides_derive_reviewed_container_host_routes() {
        for (host, generation, embedding) in [
            (Some("host.container.internal"), None, None),
            (Some("host.docker.internal"), None, None),
            (
                None,
                Some("https://generation.example.test"),
                Some("https://embeddings.example.test"),
            ),
            (
                Some("host.container.internal"),
                Some("https://generation.example.test"),
                Some("https://embeddings.example.test"),
            ),
        ] {
            let mut config: MagicianConfig =
                serde_yaml::from_str(&crate::config::shipped_repo_config_yaml()).unwrap();
            enforce_app_processing_trust_invariant(&config).unwrap();
            let protected_before: BTreeMap<_, _> = config
                .app_platform
                .processing
                .profiles
                .iter()
                .map(|(name, declaration)| {
                    let profile = &config.llm.router.as_ref().unwrap().profiles[name];
                    (
                        name.clone(),
                        (
                            profile.api_base_url.clone(),
                            declaration.class,
                            profile.provider == LLMProviderKind::Ollama,
                        ),
                    )
                })
                .collect();
            let revision = config.app_platform.processing.endpoint_trust_revision;
            apply_runtime_service_endpoint_values(&mut config, host, generation, embedding)
                .unwrap();
            // Regression: host rewriting used to move the reviewed loopback
            // URL without moving its locality declaration, so boot failed.
            enforce_app_processing_trust_invariant(&config).unwrap();
            for (name, (endpoint, class, is_ollama)) in protected_before {
                let effective = &config.llm.router.as_ref().unwrap().profiles[&name];
                let declaration = &config.app_platform.processing.profiles[&name];
                let expected = match (host, is_ollama, endpoint.as_deref()) {
                    (Some(host), true, Some(endpoint)) => {
                        Some(rewrite_loopback_endpoint(endpoint, host).unwrap())
                    },
                    _ => endpoint.clone(),
                };
                if expected.as_deref() != endpoint.as_deref() {
                    assert_eq!(effective.api_base_url.as_deref(), expected.as_deref());
                    assert_eq!(
                        declaration.class,
                        AppProcessingEndpointClass::TrustedSelfHosted
                    );
                } else {
                    assert_eq!(effective.api_base_url, endpoint);
                    assert_eq!(declaration.class, class);
                }
            }
            assert_eq!(
                config.app_platform.processing.endpoint_trust_revision,
                revision
            );
            let expected_host = generation
                .map(|_| "generation.example.test")
                .or(host)
                .unwrap();
            assert!(config
                .llm
                .router
                .as_ref()
                .unwrap()
                .profiles
                .iter()
                .filter(
                    |(name, profile)| profile.provider == LLMProviderKind::Ollama
                        && !profile_is_embedding_profile(profile)
                        && !config.app_platform.processing.profiles.contains_key(*name)
                )
                .all(
                    |(_, profile)| url::Url::parse(profile.api_base_url.as_deref().unwrap())
                        .unwrap()
                        .host_str()
                        == Some(expected_host)
                ));
        }
    }

    #[test]
    fn runtime_service_endpoint_overrides_cannot_repair_invalid_locality_by_retargeting() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml()).unwrap();
        config
            .llm
            .router
            .as_mut()
            .unwrap()
            .profiles
            .get_mut("op-app-workflow-local")
            .unwrap()
            .api_base_url = Some("https://unreviewed.example.test/api/chat".into());
        apply_runtime_service_endpoint_values(
            &mut config,
            Some("host.container.internal"),
            Some("http://localhost:11434"),
            None,
        )
        .unwrap();
        assert!(enforce_app_processing_trust_invariant(&config)
            .unwrap_err()
            .to_string()
            .contains("not loopback"));
    }

    #[test]
    fn runtime_service_endpoint_overrides_preserve_reviewed_embedding_routes() {
        let mut config: MagicianConfig =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml()).unwrap();
        let declaration = config.app_platform.processing.profiles["op-app-workflow-local"].clone();
        config
            .app_platform
            .processing
            .profiles
            .insert("op-embedding-local".into(), declaration);
        let before = config.llm.router.as_ref().unwrap().profiles["op-embedding-local"]
            .api_base_url
            .clone();
        apply_runtime_service_endpoint_values(
            &mut config,
            None,
            Some("https://generation.example.test"),
            Some("https://embeddings.example.test"),
        )
        .unwrap();
        assert_eq!(
            config.llm.router.as_ref().unwrap().profiles["op-embedding-local"].api_base_url,
            before
        );
        assert_eq!(
            config.runtime.ollama.embedding_base_url,
            "https://embeddings.example.test"
        );
        enforce_app_processing_trust_invariant(&config).unwrap();
    }

    #[test]
    fn container_host_rejects_urls_and_ports() {
        let mut config = MagicianConfig::default();
        let error = apply_runtime_service_endpoint_values(
            &mut config,
            Some("http://host.container.internal:3017"),
            None,
            None,
        )
        .expect_err("host alias must be a bare hostname");

        assert!(error.to_string().contains("must be a hostname"));
    }

    #[test]
    fn app_processing_locality_requires_explicit_concrete_profile_evidence() {
        let mut config = MagicianConfig::default();
        let mut router = LLMRouterConfig::default();
        router.profiles.insert(
            "app-local".to_owned(),
            LLMProfile {
                provider: LLMProviderKind::Ollama,
                model: "local-model".to_owned(),
                api_key_env: None,
                api_base_url: Some("http://127.0.0.1:11434/api/generate".to_owned()),
                temperature: None,
                max_output_tokens: Some(1024),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: Some(32_768),
                chunking: None,
            },
        );
        config.llm.router = Some(router);
        config.app_platform.processing.local_profile = Some("app-local".to_owned());
        config.app_platform.processing.profiles.insert(
            "app-local".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::LoopbackManaged,
                local_processing_eligible: true,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );
        enforce_app_processing_trust_invariant(&config).expect("explicit loopback trust");

        let mut unsupported_retention = config.clone();
        unsupported_retention
            .llm
            .router
            .as_mut()
            .unwrap()
            .profiles
            .get_mut("app-local")
            .unwrap()
            .provider = LLMProviderKind::DeepSeek;
        assert!(
            enforce_app_processing_trust_invariant(&unsupported_retention)
                .expect_err("automatic provider retention has no protected off switch")
                .to_string()
                .contains("no_provider_storage")
        );

        config
            .llm
            .router
            .as_mut()
            .unwrap()
            .profiles
            .get_mut("app-local")
            .unwrap()
            .api_base_url = Some("https://remote.example/api/generate".to_owned());
        assert!(enforce_app_processing_trust_invariant(&config)
            .expect_err("provider name does not prove locality")
            .to_string()
            .contains("not loopback"));

        config
            .llm
            .router
            .as_mut()
            .unwrap()
            .profiles
            .get_mut("app-local")
            .unwrap()
            .api_base_url = Some("http://host.container.internal:11434/api/generate".to_owned());
        assert!(enforce_app_processing_trust_invariant(&config)
            .expect_err("an internal DNS suffix is not loopback proof")
            .to_string()
            .contains("not loopback"));
    }

    #[test]
    fn app_processing_remote_route_is_independently_opted_in() {
        let mut config = MagicianConfig::default();
        config.app_platform.processing.remote_processing_enabled = true;
        let error = enforce_app_processing_trust_invariant(&config)
            .expect_err("remote switch without exact profile must fail");
        assert!(error.to_string().contains("remote_profile is required"));
    }

    #[test]
    fn app_processing_local_eligible_self_hosted_profile_requires_explicit_url() {
        let mut config = MagicianConfig::default();
        let mut router = LLMRouterConfig::default();
        router.profiles.insert(
            "app-self-hosted".to_owned(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "self-hosted-model".to_owned(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: Some(1024),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: Some(32_768),
                chunking: None,
            },
        );
        config.llm.router = Some(router);
        config.app_platform.processing.local_profile = Some("app-self-hosted".to_owned());
        config.app_platform.processing.profiles.insert(
            "app-self-hosted".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::TrustedSelfHosted,
                local_processing_eligible: true,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );

        let error = enforce_app_processing_trust_invariant(&config)
            .expect_err("a local-eligible profile cannot fall back to a public provider URL");
        assert!(error.to_string().contains("no explicit api_base_url"));
    }

    #[test]
    fn app_llm_operation_admission_requires_namespaced_mapping_over_trusted_profiles() {
        fn lane_profile(provider: LLMProviderKind, api_base_url: Option<String>) -> LLMProfile {
            LLMProfile {
                provider,
                model: "lane-model".to_owned(),
                api_key_env: None,
                api_base_url,
                temperature: None,
                max_output_tokens: Some(1_024),
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: None,
                context_window_tokens: Some(32_768),
                chunking: None,
            }
        }

        let mut config = MagicianConfig::default();
        let mut router = LLMRouterConfig::default();
        router.profiles.insert(
            "app-op-local".to_owned(),
            lane_profile(
                LLMProviderKind::Ollama,
                Some("http://127.0.0.1:11434/api/generate".to_owned()),
            ),
        );
        router.profiles.insert(
            "app-op-remote".to_owned(),
            lane_profile(LLMProviderKind::OpenAI, None),
        );
        router.operation_mapping.insert(
            "app:summarize_record".to_owned(),
            OperationProfileSelector::Conditional {
                default: "app-op-local".to_owned(),
                when_has_images: None,
                when_cloud: Some("app-op-remote".to_owned()),
                description: None,
                group: None,
                engine: None,
            },
        );
        config.llm.router = Some(router);
        config.app_platform.processing.profiles.insert(
            "app-op-local".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::LoopbackManaged,
                local_processing_eligible: true,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );
        config.app_platform.processing.profiles.insert(
            "app-op-remote".to_owned(),
            AppProcessingProfileTrust {
                class: AppProcessingEndpointClass::External,
                local_processing_eligible: false,
                provider_retention: AppProviderRetentionPosture::NoProviderStorage,
            },
        );
        config.app_platform.llm_operations.insert(
            "summarize_record".to_owned(),
            AppProcessingLlmOperationTrust {
                reviewed_purpose: "Summarize one admitted record.".to_owned(),
                max_output_tokens: Some(4_096),
            },
        );
        enforce_app_processing_trust_invariant(&config)
            .expect("admitted operation over trusted arms is valid");

        let mut unconfigured = config.clone();
        unconfigured.llm.router = None;
        // Without trusted profiles either, the first failure must be the
        // admission's own router requirement — the lane adds no silent
        // fallback to the router default profile.
        unconfigured.app_platform.processing.profiles.clear();
        let error = enforce_app_processing_trust_invariant(&unconfigured)
            .expect_err("admission without a router has nothing to route through");
        assert!(error
            .to_string()
            .contains("requires llm.router to be configured"));

        let mut unmapped = config.clone();
        unmapped
            .llm
            .router
            .as_mut()
            .unwrap()
            .operation_mapping
            .remove("app:summarize_record")
            .unwrap();
        let error = enforce_app_processing_trust_invariant(&unmapped)
            .expect_err("admission must name its namespaced mapping");
        assert!(error
            .to_string()
            .contains("references missing llm.router.operation_mapping `app:summarize_record`"));

        let mut untrusted_arm = config.clone();
        untrusted_arm
            .llm
            .router
            .as_mut()
            .unwrap()
            .operation_mapping
            .insert(
                "app:summarize_record".to_owned(),
                OperationProfileSelector::Simple("unreviewed-profile".to_owned()),
            );
        let error = enforce_app_processing_trust_invariant(&untrusted_arm)
            .expect_err("an arm outside the trust catalog must fail the load");
        assert!(error
            .to_string()
            .contains("not declared in app_platform.processing.profiles"));

        let mut malformed_name = config.clone();
        let admission = malformed_name
            .app_platform
            .llm_operations
            .remove("summarize_record")
            .unwrap();
        malformed_name
            .app_platform
            .llm_operations
            .insert("summarize:record".to_owned(), admission);
        let error = enforce_app_processing_trust_invariant(&malformed_name)
            .expect_err("a colon cannot smuggle a second namespace segment");
        assert!(error.to_string().contains("is invalid"));

        let mut unreviewed = config.clone();
        unreviewed
            .app_platform
            .llm_operations
            .get_mut("summarize_record")
            .unwrap()
            .reviewed_purpose = "   ".to_owned();
        let error = enforce_app_processing_trust_invariant(&unreviewed)
            .expect_err("admission must carry a non-blank reviewed purpose");
        assert!(error.to_string().contains("reviewed_purpose"));
    }

    #[test]
    fn absent_app_llm_operation_admission_keeps_current_behavior() {
        // Absent admission is the pre-1.4 configuration: no app may declare
        // operations, and the trust invariant is unchanged for it.
        let mut config = MagicianConfig::default();
        config.llm.router = Some(test_llm_router_with_operations(&["chat_completion"]));
        assert!(config.app_platform.llm_operations.is_empty());
        enforce_app_processing_trust_invariant(&config).expect("absent admission is a no-op");
        let parsed: AppPlatformSettings = serde_yaml::from_str("processing: {}\n").unwrap();
        assert!(parsed.llm_operations.is_empty());
        let parsed: AppPlatformSettings =
            serde_yaml::from_str("llm_operations:\n  summarize_record:\n    reviewed_purpose: p\n")
                .unwrap();
        assert_eq!(parsed.llm_operations.len(), 1);
        // Unknown admission fields fail closed like the rest of the policy.
        assert!(serde_yaml::from_str::<AppPlatformSettings>(
            "llm_operations:\n  summarize_record:\n    profile: op-app-workflow-local\n"
        )
        .is_err());
    }

    #[test]
    fn app_resource_runtime_uses_one_validated_server_policy() {
        let mut config = MagicianConfig::default();
        let policy = config.app_platform.resources.enforcement_policy();
        assert_eq!(policy.scheduler_capacity, 8);
        assert_eq!(policy.foreground_reserved_slots, 2);
        enforce_app_resource_runtime_invariant(&config).expect("default resource policy");

        config.app_platform.resources.foreground_reserved_slots = 9;
        let error = enforce_app_resource_runtime_invariant(&config)
            .expect_err("foreground reserve cannot exceed process capacity");
        assert!(error.to_string().contains("app_platform.resources"));
    }

    /// The closed door. Arming unattended host execution is an owner decision
    /// about a deployment, so neither the struct default nor an
    /// `app_platform` section that merely mentions the block may arm it.
    #[test]
    fn app_background_behaviors_default_is_disarmed() {
        assert!(!AppBackgroundBehaviorSettings::default().enabled);
        assert!(
            !MagicianConfig::default()
                .app_platform
                .background_behaviors
                .enabled
        );
        let parsed: AppPlatformSettings =
            serde_yaml::from_str("background_behaviors:\n  tick_interval_seconds: 30\n").unwrap();
        assert!(!parsed.background_behaviors.enabled);
    }

    /// Arming must mean the unattended lane is reachable, not merely switched
    /// on: a schedule/event trigger runs in the background lane, and both of
    /// these resource shapes deny every background admission.
    #[test]
    fn app_background_behaviors_arming_requires_an_admissible_background_lane() {
        let mut config = MagicianConfig::default();
        config.app_platform.background_behaviors.enabled = true;
        enforce_app_background_behavior_invariant(&config)
            .expect("shipped resource ceilings leave the background lane admissible");

        let mut starved = config.clone();
        starved.app_platform.resources.foreground_reserved_slots =
            starved.app_platform.resources.scheduler_capacity;
        let error = enforce_app_background_behavior_invariant(&starved)
            .expect_err("a fully reserved scheduler admits no background run");
        assert!(error.to_string().contains("scheduler_capacity"));

        let mut frozen = config.clone();
        frozen
            .app_platform
            .resources
            .max_background_starts_per_period = 0;
        let error = enforce_app_background_behavior_invariant(&frozen)
            .expect_err("a zero start ceiling denies every background start");
        assert!(error
            .to_string()
            .contains("max_background_starts_per_period"));
    }

    /// The arming preconditions are additional, not a retroactive tightening:
    /// a deployment that never arms keeps loading exactly the configurations
    /// it loaded before.
    #[test]
    fn app_background_behaviors_disarmed_ignores_the_arming_preconditions() {
        let mut config = MagicianConfig::default();
        assert!(!config.app_platform.background_behaviors.enabled);
        config.app_platform.resources.foreground_reserved_slots =
            config.app_platform.resources.scheduler_capacity;
        config
            .app_platform
            .resources
            .max_background_starts_per_period = 0;
        enforce_app_background_behavior_invariant(&config)
            .expect("a disarmed switch imposes no background-lane precondition");
    }

    /// Arming narrows nothing that was already fenced: every scheduler ceiling
    /// stays exactly as binding with the master switch on.
    #[test]
    fn app_background_behavior_ceilings_still_bind_when_armed() {
        let mut config = MagicianConfig::default();
        config.app_platform.background_behaviors.enabled = true;
        config.app_platform.background_behaviors.lease_seconds = config
            .app_platform
            .background_behaviors
            .tick_interval_seconds;
        let error = enforce_app_background_behavior_invariant(&config)
            .expect_err("a lease that cannot outlive a tick is refused armed or not");
        assert!(error
            .to_string()
            .contains("app_platform.background_behaviors has invalid scheduler ceilings"));

        let mut over_cap = MagicianConfig::default();
        over_cap.app_platform.background_behaviors.enabled = true;
        over_cap
            .app_platform
            .background_behaviors
            .max_claims_per_scope_tick =
            crate::magician_v2::apps::background_behaviors::APP_BEHAVIOR_MAX_CLAIMS_PER_SCOPE_TICK
                + 1;
        enforce_app_background_behavior_invariant(&over_cap)
            .expect_err("the per-tick claim ceiling is a hard cap when armed");
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod auth_config_tests {
    use super::*;

    #[test]
    fn auth_recovery_mode_deserializes_from_yaml() {
        let yaml = r#"
auth_recovery_mode: hybrid
auth_ttl_secs: 7200
auth_max_failures: 5
max_vault_origins: 200
"#;
        let config: ApiMiningConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config.auth_recovery_mode,
            AuthRecoveryMode::Hybrid
        ));
        assert_eq!(config.auth_ttl_secs, 7200);
        assert_eq!(config.auth_max_failures, 5);
        assert_eq!(config.max_vault_origins, 200);
    }

    #[test]
    fn auth_defaults_are_sensible() {
        let config = ApiMiningConfig::default();
        assert!(matches!(
            config.auth_recovery_mode,
            AuthRecoveryMode::Hybrid
        ));
        assert_eq!(config.auth_ttl_secs, 3600);
        assert_eq!(config.auth_max_failures, 3);
        assert_eq!(config.max_vault_origins, 500);
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod recipe_config_tests {
    use super::*;

    #[test]
    fn recipe_defaults_match_the_runtime_contract() {
        let config = ApiMiningConfig::default();
        assert!(config.enabled);
        assert!(config.recipes.enabled);
        assert!(config.recipes.compile_after_first_run);
        assert_eq!(config.recipes.match_confirm_threshold, 0.85);
        assert_eq!(
            config.recipes.transport_ladder,
            vec!["reqwest", "in_page_fetch", "browser"]
        );
        assert_eq!(
            config.recipes.unknown_origin_default,
            "reads_auto_writes_by_grant"
        );
        assert!(!config.recipe_verification.enabled);
        assert_eq!(config.recipe_verification.interval_secs, 21_600);
    }

    #[test]
    fn recipe_config_validation_normalizes_and_fails_to_browser_only() {
        let mut config = ApiMiningConfig::default();
        config.recipes.match_confirm_threshold = f32::NAN;
        config.recipes.transport_ladder = vec![" REQWEST ".into(), "reqwest".into()];
        config.recipes.unknown_origin_default = "unrestricted".into();
        config.recipe_verification.interval_secs = 0;
        let validated = config.validated();
        assert_eq!(validated.recipes.match_confirm_threshold, 0.85);
        assert_eq!(validated.recipes.transport_ladder, vec!["reqwest"]);
        assert_eq!(
            validated.recipes.unknown_origin_default,
            "reads_auto_writes_by_grant"
        );
        assert_eq!(validated.recipe_verification.interval_secs, 21_600);

        let mut invalid = ApiMiningConfig::default();
        invalid.recipes.transport_ladder = vec!["unknown".into()];
        assert_eq!(
            invalid.validated().recipes.transport_ladder,
            vec!["browser"]
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod bot_process_config_tests {
    use super::*;

    #[test]
    fn bot_process_config_defaults_are_safe() {
        let cfg = BotProcessConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.command.is_empty());
        assert!(cfg.args.is_empty());
        assert!(cfg.env.is_empty());
        assert!(cfg.cwd.is_none());
        assert!(cfg.auto_restart);
        assert_eq!(cfg.restart_max_backoff_secs, 30);
    }
}

/// Locality-policy config tests: mode derivation, model residency, the
/// local_prep load-time rule, and remote-profile hygiene over the shipped
/// configs.
#[cfg(any(test, feature = "test-fixtures"))]
mod locality_policy_tests {
    use super::*;

    fn template_config() -> MagicianConfig {
        serde_yaml::from_str(&crate::config::shipped_repo_config_yaml())
            .expect("template config parses")
    }

    /// The repository config with its router tables spliced in.
    ///
    /// Cached rather than rebuilt per call: the splice is a ~6,200-line string
    /// join and several tests below reach for this. `LazyLock` also keeps the
    /// `&'static str` shape the call sites already expect.
    fn repo_config_text() -> &'static str {
        static TEXT: std::sync::LazyLock<String> =
            std::sync::LazyLock::new(crate::config::shipped_repo_config_yaml);
        TEXT.as_str()
    }

    #[test]
    fn absent_privacy_section_means_local_everywhere() {
        let mut config = template_config();
        // The shipped config may select either operator mode. Reset the
        // section to its serde/default value to exercise an actually absent
        // `privacy:` section without rebuilding the large router fixture.
        config.privacy = PrivacySettings::default();
        assert_eq!(
            config.privacy.processing.mode,
            magicllm::ProcessingLocality::Local
        );
        apply_privacy_processing_locality(&mut config);
        assert_eq!(
            config.llm.router.as_ref().expect("router").locality,
            magicllm::ProcessingLocality::Local
        );
        assert!(!config.app_platform.processing.remote_processing_enabled);
    }

    #[test]
    fn privacy_yaml_parses_and_derives_into_router_and_app_boundary() {
        let privacy_only: MagicianConfig =
            serde_yaml::from_str("privacy:\n  processing:\n    mode: cloud\n")
                .expect("privacy section parses standalone");
        assert_eq!(
            privacy_only.privacy.processing.mode,
            magicllm::ProcessingLocality::Cloud
        );
        let mut config = template_config();
        config.privacy = privacy_only.privacy;
        apply_privacy_processing_locality(&mut config);
        assert_eq!(
            config.llm.router.as_ref().expect("router").locality,
            magicllm::ProcessingLocality::Cloud
        );
        // One switch, not two: app remote processing follows the mode.
        assert!(config.app_platform.processing.remote_processing_enabled);
    }

    #[test]
    fn cloud_mode_empties_the_generation_model_set_and_skips_ollama_rules() {
        let mut config = template_config();
        config.privacy.processing.mode = magicllm::ProcessingLocality::Cloud;
        apply_privacy_processing_locality(&mut config);

        // Production order: normalize first (it unmaps local_prep under
        // cloud), then the generation walker — exactly how
        // validate_and_apply + apply_runtime_ollama_policy sequence them.
        config.llm.dispatch.local_prep.enabled = true;
        let keep_alive = config.resolved_ollama_keep_alive();
        normalize_runtime_ollama_policy(&mut config, keep_alive)
            .expect("cloud mode must skip the local_prep ollama validation");
        assert!(!config.llm.dispatch.local_prep.enabled);
        assert!(!config
            .llm
            .router
            .as_ref()
            .expect("router")
            .operation_mapping
            .contains_key("local_prep"));

        // Every mapped qwen arm switched to its when_cloud counterpart and
        // local_prep unmapped, so no Ollama generation profile remains — the
        // on-device model goes unused. Empty is a legal steady state.
        let models = resolved_ollama_generation_models(&config)
            .expect("cloud-mode generation resolution must not error");
        assert!(
            models.is_empty(),
            "expected no resident generation models under cloud, got {models:?}"
        );
    }

    #[test]
    fn local_mode_still_requires_local_prep_to_resolve_to_ollama() {
        let mut config = template_config();
        config.privacy.processing.mode = magicllm::ProcessingLocality::Local;
        apply_privacy_processing_locality(&mut config);
        // The template binds local_prep to the ambient ollama profile: the
        // validation passes and normalizes the model/base_url.
        let keep_alive = config.resolved_ollama_keep_alive();
        normalize_runtime_ollama_policy(&mut config, keep_alive)
            .expect("template local_prep must satisfy the local-mode rule");
        assert_eq!(config.llm.dispatch.local_prep.model, "gemma4:12b");
    }

    #[test]
    fn repo_remote_profiles_carry_no_ollama_only_metadata() {
        // Metadata is merged into request.extra and unknown keys are
        // forwarded verbatim by remote providers (OpenAI Responses 400s on
        // them), so a remote arm carrying `format`/`options`/`keep_alive`/
        // `draft_num_predict` is a runtime failure, not a cosmetic one.
        let config: MagicianConfig =
            serde_yaml::from_str(repo_config_text()).expect("repo config parses");
        let router = config.llm.router.as_ref().expect("router");
        let mut checked = 0;
        for (name, profile) in &router.profiles {
            if profile.provider == LLMProviderKind::Ollama {
                continue;
            }
            if !name.ends_with("-remote") {
                continue;
            }
            checked += 1;
            if let Some(metadata) = &profile.metadata {
                for forbidden in ["format", "options", "keep_alive", "draft_num_predict"] {
                    assert!(
                        !metadata.contains_key(forbidden),
                        "remote profile `{name}` carries Ollama-only metadata `{forbidden}`"
                    );
                }
            }
        }
        assert!(
            checked >= 13,
            "expected the remote counterpart family, got {checked}"
        );
    }

    #[test]
    fn locality_switch_preserves_reviewed_execution_posture() {
        // Local Ollama arms keep reasoning disabled. Cloud mirrors do too,
        // except the two migrated Sol operations: GPT-6.1 Sol requires at
        // least low effort, with an explicit 4K shared output budget.
        for (surface, text) in [("repository", repo_config_text())] {
            let config: MagicianConfig = serde_yaml::from_str(text).expect("config parses");
            let router = config.llm.router.as_ref().expect("router");
            let mut operation_count = 0;
            let mut cloud_profiles = std::collections::BTreeSet::new();

            for (operation, selector) in &router.operation_mapping {
                if !selector.selects_cloud_arm(magicllm::ProcessingLocality::Cloud) {
                    continue;
                }
                operation_count += 1;

                let local_name = selector.default_profile();
                let local = router.profiles.get(local_name).unwrap_or_else(|| {
                    panic!("{surface} operation `{operation}` has no local profile `{local_name}`")
                });
                assert_eq!(
                    local.provider,
                    LLMProviderKind::Ollama,
                    "{surface} operation `{operation}` local arm must remain Ollama"
                );
                assert!(
                    local.reasoning.is_none(),
                    "{surface} operation `{operation}` local arm `{local_name}` enables reasoning"
                );
                assert_ne!(
                    local
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("think"))
                        .and_then(serde_json::Value::as_bool),
                    Some(true),
                    "{surface} operation `{operation}` local arm `{local_name}` sets think:true"
                );
                if let Some(metadata) = local.metadata.as_ref() {
                    for forbidden in [
                        "fallback_profile",
                        "router_profile_override",
                        "router_provider_override",
                    ] {
                        assert!(
                            !metadata.contains_key(forbidden),
                            "{surface} local arm `{local_name}` can escape its reviewed non-reasoning profile through `{forbidden}`"
                        );
                    }
                }

                let cloud_name = selector.profile_for_locality(
                    &magicllm::config::RequestShape::NONE,
                    magicllm::ProcessingLocality::Cloud,
                );
                cloud_profiles.insert(cloud_name.to_string());
                let cloud = router.profiles.get(cloud_name).unwrap_or_else(|| {
                    panic!("{surface} operation `{operation}` has no cloud profile `{cloud_name}`")
                });
                assert_eq!(cloud.provider, LLMProviderKind::OpenAI);
                if cloud.model == "gpt-6.1-sol" {
                    assert!(matches!(
                        operation.as_str(),
                        "memory_archive_summary" | "channel_reply_draft"
                    ));
                    assert_eq!(cloud.supports_reasoning, Some(true));
                    assert_eq!(
                        cloud.reasoning.as_ref().map(|r| r.effort.as_str()),
                        Some("low")
                    );
                    assert_eq!(cloud.max_output_tokens, Some(4096));
                } else {
                    assert_eq!(cloud.supports_reasoning, Some(false), "{cloud_name}");
                    assert!(cloud.reasoning.is_none(), "{cloud_name}");
                    const REVIEWED_NO_REASONING_MODELS: [&str; 3] =
                        ["gpt-5.6-terra", "gpt-5.6-luna", "gpt-6-luna"];
                    assert!(
                        REVIEWED_NO_REASONING_MODELS.contains(&cloud.model.as_str()),
                        "{cloud_name} uses an unreviewed cloud reasoning contract: {}",
                        cloud.model
                    );
                }
                let metadata = cloud.metadata.as_ref().unwrap_or_else(|| {
                    panic!("{surface} cloud arm `{cloud_name}` has no provider metadata")
                });
                assert_eq!(
                    metadata.get("openai_api_mode").and_then(serde_json::Value::as_str),
                    Some("responses"),
                    "{surface} cloud arm `{cloud_name}` changed OpenAI adapter; review its no-reasoning wire contract"
                );
                for forbidden in [
                    "reasoning",
                    "reasoning_effort",
                    "reasoning_summary",
                    "reasoning_strategy",
                    "reasoning_max_tokens",
                    "think",
                    "fallback_profile",
                    "router_profile_override",
                    "router_provider_override",
                ] {
                    assert!(
                        !metadata.contains_key(forbidden),
                        "{surface} cloud arm `{cloud_name}` bypasses typed reasoning policy through metadata key `{forbidden}`"
                    );
                }
            }

            assert_eq!(
                operation_count, 20,
                "{surface} locality-aware operation inventory changed; review every new pair"
            );
            assert_eq!(
                cloud_profiles.len(),
                14,
                "{surface} cloud replacement inventory changed; review every new profile"
            );
        }
    }

    #[test]
    fn no_embed_operation_ever_carries_a_when_cloud_arm() {
        // Forward rule: a locality flip is reversible and near-instant; an
        // embedding provider change rotates embedding_contract_id and forces
        // a full re-embed. Bindings must not put that migration behind this
        // switch — enforced, not remembered.
        for text in [repo_config_text()] {
            let config: MagicianConfig = serde_yaml::from_str(text).expect("config parses");
            let router = config.llm.router.as_ref().expect("router");
            for (operation, selector) in &router.operation_mapping {
                if operation.starts_with("embed_") {
                    assert!(
                        !selector.selects_cloud_arm(magicllm::ProcessingLocality::Cloud),
                        "embed operation `{operation}` must not carry a when_cloud arm"
                    );
                }
            }
        }
    }

    #[test]
    fn embeddings_section_is_identical_in_both_modes() {
        // The switch moves generation only. The embedding daemon, model,
        // dimensions, and keep-alive are byte-identical under cloud.
        let mut local = template_config();
        local.privacy.processing.mode = magicllm::ProcessingLocality::Local;
        apply_privacy_processing_locality(&mut local);
        let mut cloud = template_config();
        cloud.privacy.processing.mode = magicllm::ProcessingLocality::Cloud;
        apply_privacy_processing_locality(&mut cloud);
        assert_eq!(
            local.runtime.ollama.embedding_base_url,
            cloud.runtime.ollama.embedding_base_url
        );
        assert_eq!(
            local.runtime.ollama.embedding_model,
            cloud.runtime.ollama.embedding_model
        );
        assert_eq!(
            local.runtime.ollama.embedding_dimensions,
            cloud.runtime.ollama.embedding_dimensions
        );
        assert_eq!(
            local.runtime.ollama.embedding_keep_alive,
            cloud.runtime.ollama.embedding_keep_alive
        );
        // And the routed embedding profile stays mapped + ollama in both.
        for config in [&local, &cloud] {
            let router = config.llm.router.as_ref().expect("router");
            let profile = router
                .profiles
                .get("op-embedding-local")
                .expect("routed embedding profile");
            assert_eq!(profile.provider, LLMProviderKind::Ollama);
            assert!(router.operation_mapping.contains_key("embed_documents"));
            assert!(router.operation_mapping.contains_key("embed_query"));
        }
    }

    /// The harness-CLI provider's no-override guarantee (2026-08-31 plan):
    /// the SHIPPED seed maps zero operations to harness profiles. A default
    /// install never sends background data — which the owner has flagged as
    /// sensitive for several flows — to a subscription CLI unless someone
    /// typed a per-operation opt-in line; local (Ollama) and API profiles
    /// remain the shipped choices for every operation. Walks the raw YAML so
    /// every operation_mapping, present or future, is covered.
    #[test]
    fn the_shipped_seed_maps_no_operation_to_a_harness_profile() {
        // The complete document, not the config file alone: the router tables
        // live in `llm-router.yaml`, and this test asserts on where
        // `op-harness-claude` is declared inside `llm.router`.
        let raw = crate::config::shipped_repo_config_yaml();
        let value: serde_yaml::Value = serde_yaml::from_str(&raw).expect("repo seed parses");
        let mut violations = Vec::new();
        fn walk_mappings(node: &serde_yaml::Value, path: &str, violations: &mut Vec<String>) {
            let Some(map) = node.as_mapping() else { return };
            for (key, val) in map {
                let key_str = key.as_str().unwrap_or_default();
                if key_str == "operation_mapping" {
                    if let Some(mapping) = val.as_mapping() {
                        for (op, selector) in mapping {
                            collect_harness_refs(
                                selector,
                                &format!(
                                    "{path}.operation_mapping.{}",
                                    op.as_str().unwrap_or_default()
                                ),
                                violations,
                            );
                        }
                    }
                } else {
                    walk_mappings(val, &format!("{path}.{key_str}"), violations);
                }
            }
        }
        fn collect_harness_refs(
            selector: &serde_yaml::Value,
            path: &str,
            violations: &mut Vec<String>,
        ) {
            if let Some(name) = selector.as_str() {
                if name.starts_with("op-harness") {
                    violations.push(format!("{path} = {name}"));
                }
                return;
            }
            if let Some(map) = selector.as_mapping() {
                for (arm, val) in map {
                    collect_harness_refs(
                        val,
                        &format!("{path}.{}", arm.as_str().unwrap_or_default()),
                        violations,
                    );
                }
            }
        }
        walk_mappings(&value, "", &mut violations);
        assert!(
            violations.is_empty(),
            "shipped defaults must map zero operations to harness profiles (the \
             no-override guarantee); found: {violations:?}"
        );
        // Round-4 lesson, made structural: the harness profiles must live
        // in `llm.router.profiles` — an anchored insertion once landed them
        // in `adaptive_profiles` (syntax-valid YAML, boot-breaking serde).
        // Parsed placement, not string presence.
        let router = value
            .get("llm")
            .and_then(|llm| llm.get("router"))
            .cloned()
            .unwrap_or_else(|| serde_yaml::Value::Mapping(Default::default()));
        let in_profiles = router
            .get("profiles")
            .and_then(|profiles| profiles.get("op-harness-claude"))
            .is_some();
        let contaminating_adaptive = router
            .get("adaptive_profiles")
            .and_then(|profiles| profiles.get("op-harness-claude"))
            .is_some();
        assert!(
            in_profiles && !contaminating_adaptive,
            "the op-harness profiles must be declared in llm.router.profiles, never adaptive_profiles (boot-breaking misplacement)"
        );
    }
}
