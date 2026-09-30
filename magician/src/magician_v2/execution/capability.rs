//! Pluggable capability system for tool dispatch.
//!
//! `CapabilityProvider` is the trait that all tool implementations must satisfy.
//! `CapabilityRegistry` holds registered providers keyed by tool name and dispatches
//! lowering + execution to the appropriate provider.
//!
//! Existing built-in tools (browser, files, shell, http_*, search) are wrapped as
//! *compiled* providers in `compiled_providers.rs`. New tools can be added at runtime
//! via `PackCapabilityProvider` backed by YAML capability pack definitions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::actions::{ActionResult, ExecutableAction};
use super::error::ExecutionError;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;
use crate::magician_v2::tool_result_projection::ProjectionContractSpec;

// ============================================================================
// CapabilityProvider Trait
// ============================================================================

/// A pluggable provider for a single tool type.
///
/// Implementations handle both *lowering* (converting a plan step into an
/// executable action) and *executing* that action. The registry dispatches to
/// the provider whose `tool_name()` matches the step's tool field.
#[async_trait]
pub trait CapabilityProvider: Send + Sync + std::fmt::Debug {
    /// Canonical tool name this provider handles (e.g., "browser", "files", "shell").
    fn tool_name(&self) -> &str;

    /// Server-owned final transport attestation for protected app content.
    /// The default is deliberately unavailable: a compiled/CLI/browser tool
    /// name does not prove local determinism or an external destination.
    fn attest_app_tool_target(
        &self,
        _tool_ref: crate::magician_v2::apps::models::AppReference,
        _parameters: &HashMap<String, serde_json::Value>,
    ) -> Option<crate::magician_v2::apps::tool_disclosure::AttestedAppToolTarget> {
        None
    }

    /// Prove tool-specific arguments after the bind kernel has classified the
    /// call. The kernel mints the receipt. The default is fail-closed.
    fn prove_app_tool_args(&self, _parameters: &HashMap<String, serde_json::Value>) -> bool {
        false
    }

    /// Lower a plan step into an executable action.
    ///
    /// Returns `MaybeGatedAction::Bare` for non-spend-bearing tools, or
    /// `MaybeGatedAction::Gated` when the capability pack declares a `SpendDeclaration`.
    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError>;

    /// Execute an already-lowered action.
    async fn execute(
        &self,
        action: &ExecutableAction,
        session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError>;

    /// Execute an already-lowered action, carrying the identity of the
    /// dispatch attempt it belongs to.
    ///
    /// Defaults to [`Self::execute`], dropping the identity. That is correct
    /// for the many providers with no far-side deduplication seam — a
    /// filesystem write, a subprocess, or in-process compute cannot be made
    /// idempotent by a key, and pretending otherwise would be worse than
    /// admitting it.
    ///
    /// A provider that makes OUTBOUND REQUESTS should override this and send a
    /// derived key, via
    /// [`LlmToolLineageIdentity::far_side_idempotency_key`]. Overriding is the
    /// difference between a lost response costing the user one duplicate
    /// charge and costing them none.
    ///
    /// [`LlmToolLineageIdentity::far_side_idempotency_key`]:
    /// crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity::far_side_idempotency_key
    async fn execute_with_effect(
        &self,
        action: &ExecutableAction,
        session_id: Option<String>,
        timeout_secs: u64,
        _effect_id: Option<&str>,
    ) -> Result<ActionResult, ExecutionError> {
        self.execute(action, session_id, timeout_secs).await
    }

    /// Whether this capability requires a browser session to execute.
    /// Defaults to `false` — only the browser provider returns `true`.
    fn requires_browser_session(&self) -> bool {
        false
    }

    /// Default execution timeout in seconds for this capability.
    /// Providers should read from YAML `execution.default_timeout_secs` when available.
    /// Defaults to 30s.
    fn default_timeout_secs(&self) -> u64 {
        30
    }

    /// Single-shot dispatch for callers that already have a flat
    /// parameter map and don't need the `PlanStep` shape. Skips the
    /// `PlanStep` build + the `lower() → MaybeGatedAction` match per
    /// call when the lowering produces a `Bare` action.
    ///
    /// **Strict on `Gated`.** This trait method does NOT route gated
    /// actions through a spend ledger — it has no resource-authority
    /// context to charge against. Production callers MUST go through
    /// the neutral `execution::compiled_dispatch` module
    /// (`try_dispatch_compiled_pack` / `dispatch_compiled_provider`),
    /// which carries an `Option<&CompiledDispatchAuthority>` and
    /// handles both the gated and degraded-mode (authority absent /
    /// `enabled: false`) cases uniformly via `execute_maybe_gated`.
    ///
    /// A `Gated` lowering reaching `execute_direct` indicates a
    /// wiring bug: a caller dispatched directly instead of through
    /// the gating-aware primitive. The returned `ExecutionError::Step`
    /// carries an explicit "use compiled_dispatch" message to surface
    /// the misuse. Plan:
    /// `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`.
    ///
    /// Default impl performs the legacy ceremony for `Bare` (build a
    /// minimal `PlanStep`, run `lower`, execute) so every existing
    /// provider keeps working without changes. Providers whose
    /// `lower()` is a thin wrap of params (agent backend
    /// providers, etc.) can override this with a flatter path that
    /// constructs the action directly.
    async fn execute_direct(
        &self,
        parameters: HashMap<String, serde_json::Value>,
        session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        // `timeout_override_secs` is set on the synthetic step so any
        // provider whose `lower()` propagates it into the lowered
        // action's internal `timeout_secs` honours the caller-supplied
        // per-call timeout. The same value is also passed to
        // `provider.execute(...)` below — setting both keeps this
        // degraded path symmetric with `compiled_dispatch::dispatch_with_gating`'s
        // gated path and with the pre-refactor inner-loop code that
        // used to build its own synthetic step the same way.
        let step = PlanStep {
            id: format!("direct-{}", self.tool_name()),
            task: format!("direct dispatch `{}`", self.tool_name()),
            tool: Some(self.tool_name().to_string()),
            parameters,
            timeout_override_secs: Some(timeout_secs),
            ..PlanStep::default()
        };
        match self.lower(&step)? {
            MaybeGatedAction::Bare(action) => self.execute(&action, session_id, timeout_secs).await,
            MaybeGatedAction::Gated(gated) => Err(ExecutionError::Step(format!(
                "execute_direct reached a spend-gated action for `{}` (commodity `{}`) — \
                 this trait method has no resource-authority context. Route the call \
                 through `execution::compiled_dispatch::dispatch_compiled_provider` (or \
                 `try_dispatch_compiled_pack`) instead so the gate fires via \
                 reserve/commit/rollback bookkeeping. Plan: \
                 docs/archive/plans/2026-05-20-resource-authority-layer-2.md.",
                self.tool_name(),
                gated.gate.commodity,
            ))),
        }
    }
}

// ============================================================================
// CapabilityRegistry
// ============================================================================

/// Registry of capability providers keyed by tool name.
///
/// Supports exact match lookup and `http_*` prefix fallback so that
/// `http_get`, `http_post`, etc. all route to the single HTTP provider.
#[derive(Default)]
pub struct CapabilityRegistry {
    providers: RwLock<HashMap<String, Arc<dyn CapabilityProvider>>>,
    builtin_app_providers: RwLock<HashMap<String, BuiltinAppProviderRecord>>,
    /// Human-readable descriptions for tools (parallel to providers map).
    /// Used for tool prompt sections and progressive disclosure.
    descriptions: RwLock<HashMap<String, String>>,
    /// LLM guide text for tools (from pack YAML `guide` field).
    /// Injected into executor prompts so the LLM knows how to use each tool.
    guides: RwLock<HashMap<String, String>>,
    /// Parameter definitions for tools (from pack YAML `parameters` field).
    /// Injected into executor prompts so the LLM knows valid parameters.
    param_defs: RwLock<HashMap<String, Vec<ParameterDef>>>,
    /// Full pack definitions for pack tools.
    /// Retained so the executor can inject a full spec for the step's assigned tool
    /// (progressive disclosure: compact summaries for planning, full spec for execution).
    pack_definitions: RwLock<HashMap<String, CapabilityPackDefinition>>,
}

#[derive(Clone)]
struct BuiltinAppProviderRecord {
    provider: Arc<dyn CapabilityProvider>,
    exact_pack_digest: crate::magician_v2::apps::models::AppDigest,
    implementation_identity: &'static str,
}

/// Opaque registry witness for one exact centrally constructed built-in
/// provider instance. It cannot be implemented or minted by a provider, and
/// intentionally has no Clone/Debug/Serde surface.
pub(crate) struct AppBuiltinProviderWitness {
    provider: Arc<dyn CapabilityProvider>,
    tool_name: String,
    exact_pack_digest: crate::magician_v2::apps::models::AppDigest,
    implementation_identity: &'static str,
}

impl AppBuiltinProviderWitness {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Arc<dyn CapabilityProvider>,
        String,
        crate::magician_v2::apps::models::AppDigest,
        &'static str,
    ) {
        (
            self.provider,
            self.tool_name,
            self.exact_pack_digest,
            self.implementation_identity,
        )
    }
}

/// A provider-facing tool name resolved back to its declarative capability
/// pack. Multi-action catalogs expose `<pack>__<action>` leaves, while the
/// registry stores the parent pack definition once.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCapabilityTool {
    pub definition: CapabilityPackDefinition,
    pub action: Option<String>,
}

impl ResolvedCapabilityTool {
    /// Reliability for the concrete callable action represented by this tool.
    ///
    /// A generated `<pack>__<action>` leaf may use only that action's
    /// declaration. Pack metadata is intentionally not a fallback: doing so
    /// would let a safe read declaration on a mixed pack license replaying a
    /// mutating sibling. Exact single-action packs retain their pack-level
    /// declaration for backward-compatible YAML.
    pub fn invocation_reliability(&self) -> Option<&CapabilityReliabilityMetadata> {
        match self.action.as_deref() {
            Some(action) => self
                .definition
                .native_action_schemas
                .get(action)
                .and_then(|schema| schema.reliability.as_ref()),
            None if self.definition.native_action_schemas.is_empty() => {
                self.definition.reliability.as_ref()
            },
            None => None,
        }
    }
}

impl std::fmt::Debug for CapabilityRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.providers.read().map(|m| m.len()).unwrap_or(0);
        let desc_count = self.descriptions.read().map(|m| m.len()).unwrap_or(0);
        let guide_count = self.guides.read().map(|m| m.len()).unwrap_or(0);
        let pack_count = self.pack_definitions.read().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("CapabilityRegistry")
            .field("provider_count", &count)
            .field("description_count", &desc_count)
            .field("guide_count", &guide_count)
            .field("pack_definition_count", &pack_count)
            .finish()
    }
}

impl CapabilityRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            builtin_app_providers: RwLock::new(HashMap::new()),
            descriptions: RwLock::new(HashMap::new()),
            guides: RwLock::new(HashMap::new()),
            param_defs: RwLock::new(HashMap::new()),
            pack_definitions: RwLock::new(HashMap::new()),
        }
    }

    /// Register a provider. Warns and overwrites if a provider is already registered
    /// for the same tool name (prevents silent capability replacement).
    pub fn register(&self, provider: Arc<dyn CapabilityProvider>) {
        let mut providers = self
            .providers
            .write()
            .expect("CapabilityRegistry lock poisoned");
        let name = provider.tool_name().to_string();
        if providers.contains_key(&name) {
            warn!(
                "[CAPABILITY] Overwriting existing provider for '{}' — \
                 pack-defined provider may be replacing a compiled provider",
                name
            );
        }
        providers.insert(name, provider);
    }

    /// Centrally register the deterministic built-in provider qualified for
    /// app execution. The concrete type in this signature and exact embedded
    /// pack bytes make the witness unavailable to custom/name overrides.
    pub(crate) fn register_builtin_time_math_provider(
        &self,
        provider: Arc<
            crate::magician_v2::execution::compiled_providers::TimeMathCapabilityProvider,
        >,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "time_math".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("time_math")
                        .expect("built-in TimeMath app implementation identity must exist"),
                },
            );
    }

    /// Centrally register the exact built-in HTTP provider used by the
    /// move-only bound-HTTP GET owner. Ordinary providers named `http` cannot
    /// mint this witness.
    pub(crate) fn register_builtin_http_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::compiled_providers::HttpCapabilityProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "http".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("http")
                        .expect("built-in bound-HTTP app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in files provider whose Apps subset is owned
    /// by the retained capability-directory dispatcher.
    pub(crate) fn register_builtin_files_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::compiled_providers::FileCapabilityProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "files".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("files")
                        .expect("built-in bound-file app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in DuckDB provider. Its Apps witness exposes
    /// only descriptor-backed preview/describe, never the ordinary raw SQL
    /// provider surface.
    pub(crate) fn register_builtin_duckdb_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::compiled_providers::DuckDbCapabilityProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "duckdb".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("duckdb")
                        .expect("built-in bound-table app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `internal_data` provider. Its Apps witness
    /// exposes only the two scoped learning review reads (plan 2.5), never the
    /// pack's broader diagnostics surface; `prove_app_tool_args` refuses every
    /// other action and parameter shape. Public because the provider is
    /// workspace-coupled and constructed by the runtime binary, not by the
    /// workspace-free compiled registry.
    pub fn register_builtin_internal_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::internal_data_provider::InternalDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "internal_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("internal_data")
                        .expect("built-in learning-read app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `thinking_maps_data` provider. Its Apps
    /// witness exposes only the two scoped thinking-map reads — the 2.5
    /// learning-read pattern generalized (the Phase 4 Brainstorm verdict's
    /// re-open condition) — and `prove_app_tool_args` refuses every other
    /// action and parameter shape. Public for the same reason as the
    /// `internal_data` witness: the provider is workspace-coupled and
    /// constructed by the runtime binary, not by the workspace-free compiled
    /// registry.
    pub fn register_builtin_thinking_maps_data_provider(
        &self,
        provider: Arc<
            crate::magician_v2::execution::thinking_maps_data_provider::ThinkingMapsDataProvider,
        >,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "thinking_maps_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("thinking_maps_data")
                        .expect(
                            "built-in thinking-map read app implementation identity must exist",
                        ),
                },
            );
    }

    /// Register the exact built-in `evidence_data` provider. Its Apps witness
    /// exposes only the five bounded host reads over claims, evidence, entities,
    /// and commitments. The concrete provider type and exact embedded pack
    /// digest prevent a scoped or custom name override from inheriting this
    /// authority.
    pub fn register_builtin_evidence_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::evidence_data_provider::EvidenceDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "evidence_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("evidence_data")
                        .expect("built-in evidence-data app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `meetings_data` provider. Its Apps witness
    /// exposes only the six bounded meeting reads — live capture state, the
    /// thread index, transcript pages, takeaways, calendar context and keyword
    /// retrieval. The concrete provider type and exact embedded pack digest
    /// prevent a scoped or custom name override from inheriting this authority,
    /// and no capture control verb is reachable through it.
    pub fn register_builtin_meetings_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::meetings_data_provider::MeetingsDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "meetings_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("meetings_data")
                        .expect("built-in meetings-data app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `agent_roster_data` provider. Its Apps
    /// witness exposes only the two bounded roster reads — identity, display
    /// name, enabled state and the three `social_persona` fields — and no path
    /// that edits a definition.
    pub fn register_builtin_agent_roster_data_provider(
        &self,
        provider: Arc<
            crate::magician_v2::execution::agent_roster_data_provider::AgentRosterDataProvider,
        >,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "agent_roster_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("agent_roster_data")
                        .expect("built-in agent-roster app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `tasks_data` provider. Its Apps witness
    /// exposes only the two bounded task reads — a narrow face with no plans,
    /// executions, chat links or question bodies — and no path that changes a
    /// task.
    pub fn register_builtin_tasks_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::tasks_data_provider::TasksDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "tasks_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("tasks_data")
                        .expect("built-in tasks app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `notes_data` provider. Its Apps witness
    /// exposes only notes search and the exact read of a searched note — no
    /// host paths and no path that writes a note.
    pub fn register_builtin_notes_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::notes_data_provider::NotesDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "notes_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("notes_data")
                        .expect("built-in notes app implementation identity must exist"),
                },
            );
    }

    /// Register the exact built-in `memory_data` provider. Its Apps witness
    /// exposes only the two owner-granted memory reads and no memory write.
    pub fn register_builtin_memory_data_provider(
        &self,
        provider: Arc<crate::magician_v2::execution::memory_data_provider::MemoryDataProvider>,
        exact_pack_bytes: &[u8],
    ) {
        let provider: Arc<dyn CapabilityProvider> = provider;
        let name = "memory_data".to_owned();
        self.providers
            .write()
            .expect("CapabilityRegistry lock poisoned")
            .insert(name.clone(), Arc::clone(&provider));
        self.builtin_app_providers
            .write()
            .expect("CapabilityRegistry builtin witness lock poisoned")
            .insert(
                name,
                BuiltinAppProviderRecord {
                    provider,
                    exact_pack_digest:
                        crate::magician_v2::apps::models::AppDigest::blake3(exact_pack_bytes),
                    implementation_identity: crate::magician_v2::apps::app_tool_bind::
                        compiled_app_provider_implementation_identity("memory_data")
                        .expect("built-in memory app implementation identity must exist"),
                },
            );
    }

    pub(crate) fn app_builtin_provider_witness(
        &self,
        tool_name: &str,
    ) -> Option<AppBuiltinProviderWitness> {
        let providers = self.providers.read().ok()?;
        let current = providers.get(tool_name)?;
        let records = self.builtin_app_providers.read().ok()?;
        let record = records.get(tool_name)?;
        Arc::ptr_eq(current, &record.provider).then(|| AppBuiltinProviderWitness {
            provider: Arc::clone(current),
            tool_name: tool_name.to_owned(),
            exact_pack_digest: record.exact_pack_digest.clone(),
            implementation_identity: record.implementation_identity,
        })
    }

    /// Register a provider, replacing any existing registration WITHOUT the
    /// overwrite warning. For choreographed boot-time overrides only —
    /// provider-backed inner-loop packs (duckdb, imessage) are deliberately
    /// registered pack-defined for outer routing and then provider-overridden
    /// for primitive dispatch. Accidental collisions must keep going through
    /// [`Self::register`] so its tripwire warning stays meaningful.
    pub fn register_override(&self, provider: Arc<dyn CapabilityProvider>) {
        let mut providers = self
            .providers
            .write()
            .expect("CapabilityRegistry lock poisoned");
        providers.insert(provider.tool_name().to_string(), provider);
    }

    /// Unregister a provider by tool name.
    ///
    /// Returns the removed provider when present.
    pub fn unregister(&self, tool_name: &str) -> Option<Arc<dyn CapabilityProvider>> {
        let mut providers = self
            .providers
            .write()
            .expect("CapabilityRegistry lock poisoned");
        providers.remove(tool_name)
    }

    /// Look up a provider by tool name.
    ///
    /// Tries exact match first, then falls back to `"http"` for any `http_*` prefix.
    pub fn get(&self, tool_name: &str) -> Option<Arc<dyn CapabilityProvider>> {
        let providers = self
            .providers
            .read()
            .expect("CapabilityRegistry lock poisoned");
        providers.get(tool_name).cloned().or_else(|| {
            if tool_name.starts_with("http_") {
                providers.get("http").cloned()
            } else {
                None
            }
        })
    }

    /// Check if a provider is registered for the given tool name.
    pub fn has(&self, tool_name: &str) -> bool {
        let providers = self
            .providers
            .read()
            .expect("CapabilityRegistry lock poisoned");
        providers.contains_key(tool_name)
            || (tool_name.starts_with("http_") && providers.contains_key("http"))
    }

    /// Return all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        let providers = self
            .providers
            .read()
            .expect("CapabilityRegistry lock poisoned");
        providers.keys().cloned().collect()
    }

    /// Set a human-readable description for a tool (used for prompt injection).
    pub fn set_description(&self, name: &str, description: String) {
        let mut descriptions = self
            .descriptions
            .write()
            .expect("CapabilityRegistry descriptions lock poisoned");
        descriptions.insert(name.to_string(), description);
    }

    /// Remove a tool's description (e.g. when a pack is unregistered).
    pub fn remove_description(&self, name: &str) {
        let mut descriptions = self
            .descriptions
            .write()
            .expect("CapabilityRegistry descriptions lock poisoned");
        descriptions.remove(name);
    }

    /// Set the LLM guide text for a tool (from pack YAML `guide` field).
    pub fn set_guide(&self, name: &str, guide: String) {
        let mut guides = self
            .guides
            .write()
            .expect("CapabilityRegistry guides lock poisoned");
        guides.insert(name.to_string(), guide);
    }

    /// Get the description for a tool, if set.
    pub fn get_description(&self, name: &str) -> Option<String> {
        let descriptions = self
            .descriptions
            .read()
            .expect("CapabilityRegistry descriptions lock poisoned");
        descriptions.get(name).cloned()
    }

    /// Get the guide text for a tool, if set.
    pub fn get_guide(&self, name: &str) -> Option<String> {
        let guides = self
            .guides
            .read()
            .expect("CapabilityRegistry guides lock poisoned");
        guides.get(name).cloned()
    }

    /// Remove a tool's guide text.
    pub fn remove_guide(&self, name: &str) {
        let mut guides = self
            .guides
            .write()
            .expect("CapabilityRegistry guides lock poisoned");
        guides.remove(name);
    }

    /// Set parameter definitions for a tool (from pack YAML `parameters` field).
    pub fn set_param_defs(&self, name: &str, params: Vec<ParameterDef>) {
        let mut param_defs = self
            .param_defs
            .write()
            .expect("CapabilityRegistry param_defs lock poisoned");
        param_defs.insert(name.to_string(), params);
    }

    /// Remove a tool's parameter definitions.
    pub fn remove_param_defs(&self, name: &str) {
        let mut param_defs = self
            .param_defs
            .write()
            .expect("CapabilityRegistry param_defs lock poisoned");
        param_defs.remove(name);
    }

    /// Store the full pack definition for a tool.
    ///
    /// Retained so the executor can retrieve the complete spec (guide + params)
    /// for the step's assigned tool during progressive disclosure.
    pub fn set_pack_definition(&self, name: &str, pack: CapabilityPackDefinition) {
        let mut defs = self
            .pack_definitions
            .write()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        defs.insert(name.to_string(), pack);
    }

    /// Retrieve the full pack definition for a tool.
    pub fn get_pack_definition(&self, name: &str) -> Option<CapabilityPackDefinition> {
        let defs = self
            .pack_definitions
            .read()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        defs.get(name).cloned()
    }

    /// Resolve either an exact pack name or one of its generated
    /// `<pack>__<action>` catalog leaves. Leaf resolution succeeds only when
    /// the action is declared by the parent pack, so an invented suffix cannot
    /// inherit the parent's execution policy or adapter.
    pub fn resolve_tool_definition(&self, tool_name: &str) -> Option<ResolvedCapabilityTool> {
        let defs = self
            .pack_definitions
            .read()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        if let Some(definition) = defs.get(tool_name) {
            return Some(ResolvedCapabilityTool {
                definition: definition.clone(),
                action: None,
            });
        }
        let (pack_name, action) = tool_name.split_once("__")?;
        let definition = defs.get(pack_name)?;
        definition
            .native_action_schemas
            .contains_key(action)
            .then(|| ResolvedCapabilityTool {
                definition: definition.clone(),
                action: Some(action.to_string()),
            })
    }

    /// Resolve only the reliability declaration for one callable tool.
    ///
    /// This is intentionally separate from [`Self::resolve_tool_definition`]:
    /// the Apply gate runs once per batch member, and cloning a complete pack
    /// (including governed runtime metadata) merely to read two booleans makes
    /// recovery cost scale with the pack rather than with the action.
    pub fn resolve_invocation_reliability(
        &self,
        tool_name: &str,
    ) -> Option<CapabilityReliabilityMetadata> {
        let defs = self
            .pack_definitions
            .read()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        if let Some(definition) = defs.get(tool_name) {
            return definition
                .native_action_schemas
                .is_empty()
                .then(|| definition.reliability.clone())
                .flatten();
        }
        let (pack_name, action) = tool_name.split_once("__")?;
        defs.get(pack_name)?
            .native_action_schemas
            .get(action)?
            .reliability
            .clone()
    }

    /// Snapshot every registered pack definition. Used by the
    /// `GET /api/magician/v2/capabilities/packs` Forge endpoint to
    /// list installed tools. Returns a freshly-allocated `Vec` —
    /// caller owns the data, the registry's lock is released
    /// before this method returns.
    pub fn all_pack_definitions(&self) -> Vec<CapabilityPackDefinition> {
        let defs = self
            .pack_definitions
            .read()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        defs.values().cloned().collect()
    }

    /// Remove a tool's pack definition.
    pub fn remove_pack_definition(&self, name: &str) {
        let mut defs = self
            .pack_definitions
            .write()
            .expect("CapabilityRegistry pack_definitions lock poisoned");
        defs.remove(name);
    }

    /// Withhold every compiled pack definition whose provider is not
    /// registered, returning the names withheld.
    ///
    /// The catalog an engine sees is projected from the published definitions,
    /// so a definition with no provider advertises a tool that fails with
    /// "No provider registered" the moment an engine follows the catalog.
    /// Magician's own loop rarely trips this because it prefers `files`; a
    /// switched engine reads the catalog as written. Callers run this after
    /// late binding — deferred providers bind in place from these very
    /// definitions, so withholding earlier would remove what the binder reads
    /// — and it is idempotent. Non-compiled packs are served by the pack
    /// runtime and are never withheld here.
    pub fn withhold_unbound_compiled_packs(&self) -> Vec<String> {
        let unbound: Vec<String> = {
            let defs = self
                .pack_definitions
                .read()
                .expect("CapabilityRegistry pack_definitions lock poisoned");
            defs.values()
                .filter(|pack| matches!(&pack.implementation, ImplementationType::Compiled { .. }))
                .filter(|pack| !self.has(&pack.name))
                .map(|pack| pack.name.clone())
                .collect()
        };
        if !unbound.is_empty() {
            let mut defs = self
                .pack_definitions
                .write()
                .expect("CapabilityRegistry pack_definitions lock poisoned");
            for name in &unbound {
                defs.remove(name);
            }
        }
        unbound
    }

    /// Create a scoped registry containing only the named tools.
    ///
    /// Providers, descriptions, guides, param_defs, and pack_definitions for
    /// allowed tools are shared via `Arc`/`Clone` — no deep copies of provider
    /// implementations.  The returned registry is a structural boundary: tools
    /// not in `allowed` literally do not exist, so an LLM that discovers a tool
    /// name via a side-channel (e.g. `--help` output) gets a "tool not found"
    /// error identical to calling a genuinely nonexistent tool.
    pub fn scoped(&self, allowed: &[String]) -> Self {
        use std::collections::HashSet;
        let allowed_set: HashSet<&str> = allowed.iter().map(|s| s.as_str()).collect();

        let src_providers = self
            .providers
            .read()
            .expect("CapabilityRegistry lock poisoned");
        let src_builtin_app_providers = self
            .builtin_app_providers
            .read()
            .expect("CapabilityRegistry builtin witness lock poisoned");
        let src_descriptions = self
            .descriptions
            .read()
            .expect("CapabilityRegistry descriptions lock poisoned");
        let src_guides = self
            .guides
            .read()
            .expect("CapabilityRegistry guides lock poisoned");
        let src_param_defs = self
            .param_defs
            .read()
            .expect("CapabilityRegistry param_defs lock poisoned");
        let src_pack_defs = self
            .pack_definitions
            .read()
            .expect("CapabilityRegistry pack_definitions lock poisoned");

        let providers: HashMap<String, Arc<dyn CapabilityProvider>> = src_providers
            .iter()
            .filter(|(name, _)| allowed_set.contains(name.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let builtin_app_providers = src_builtin_app_providers
            .iter()
            .filter(|(name, record)| {
                allowed_set.contains(name.as_str())
                    && providers
                        .get(name.as_str())
                        .is_some_and(|provider| Arc::ptr_eq(provider, &record.provider))
            })
            .map(|(name, record)| (name.clone(), record.clone()))
            .collect();

        let descriptions: HashMap<String, String> = src_descriptions
            .iter()
            .filter(|(name, _)| allowed_set.contains(name.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let guides: HashMap<String, String> = src_guides
            .iter()
            .filter(|(name, _)| allowed_set.contains(name.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let param_defs: HashMap<String, Vec<ParameterDef>> = src_param_defs
            .iter()
            .filter(|(name, _)| allowed_set.contains(name.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Generated catalog leaves are callable as `<pack>__<action>`, while
        // their reliability and recovery contract live on the parent pack
        // definition. Retaining that definition does not expose the parent as
        // a callable tool: providers/descriptions/parameters above remain
        // filtered by the exact allowed name. It only preserves the metadata
        // needed to validate the one admitted leaf.
        let allowed_leaf_actions: HashSet<(&str, &str)> = allowed
            .iter()
            .filter_map(|tool_name| tool_name.split_once("__"))
            .filter(|(pack_name, action)| {
                src_pack_defs.get(*pack_name).is_some_and(|definition| {
                    definition.native_action_schemas.contains_key(*action)
                })
            })
            .collect();
        let allowed_parent_packs: HashSet<&str> = allowed_leaf_actions
            .iter()
            .map(|(pack_name, _)| *pack_name)
            .collect();
        let pack_definitions: HashMap<String, CapabilityPackDefinition> = src_pack_defs
            .iter()
            .filter(|(name, _)| {
                allowed_set.contains(name.as_str()) || allowed_parent_packs.contains(name.as_str())
            })
            .map(|(name, definition)| {
                let mut scoped_definition = definition.clone();
                if !allowed_set.contains(name.as_str()) {
                    scoped_definition.native_action_schemas.retain(|action, _| {
                        allowed_leaf_actions.contains(&(name.as_str(), action.as_str()))
                    });
                }
                (name.clone(), scoped_definition)
            })
            .collect();

        Self {
            providers: RwLock::new(providers),
            builtin_app_providers: RwLock::new(builtin_app_providers),
            descriptions: RwLock::new(descriptions),
            guides: RwLock::new(guides),
            param_defs: RwLock::new(param_defs),
            pack_definitions: RwLock::new(pack_definitions),
        }
    }
}

// ============================================================================
// Parameter Type System
// ============================================================================

/// Type hint for a capability parameter.
///
/// Used by `resolve_params` to coerce raw string values into typed JSON values.
/// All new fields using this type are `#[serde(default)]` for backward compat
/// with existing YAMLs that don't declare types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ParameterType {
    #[default]
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Object,
}

// ============================================================================
// Execution Metadata
// ============================================================================

/// Spend declaration for a capability pack.
///
/// Indicates how the capability consumes external resources. Attached to
/// `ExecutionMetadata.spend` and used at lowering time to produce a `SpendGate`
/// on the resulting action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpendDeclaration {
    /// Fixed-cost action: the cost is known at planning time from a parameter.
    Committed {
        commodity: String,
        cost_parameter: String,
    },
    /// Variable-cost action: estimated cost with optional hard cap.
    Metered {
        commodity: String,
        estimated_cost: rust_decimal::Decimal,
        #[serde(default)]
        max_cost: Option<rust_decimal::Decimal>,
    },
    /// Per-invocation cost: each execution costs a fixed amount.
    Counted {
        commodity: String,
        cost_per_action: rust_decimal::Decimal,
    },
}

/// Declarative execution metadata for a capability pack.
///
/// Provides hints to the runtime about session requirements, timeouts,
/// categorization, and sandbox constraints — all loadable from YAML.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatInlineAdapter {
    /// Tutor/App Copilot screen-overlay draw transport. The adapter owns
    /// coordinate validation, Tutor run bookkeeping, and local/realtime
    /// delivery; it is not a general task-backed capability invocation.
    TutorScreenDraw,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExecutionMetadata {
    /// Whether this capability requires an active browser session.
    #[serde(default)]
    pub requires_browser_session: Option<bool>,
    /// Default timeout in seconds when the caller doesn't specify one.
    #[serde(default)]
    pub default_timeout_secs: Option<u64>,
    /// Optional bounded adapter used when this pack is invoked from Chat.
    /// Declaring an adapter keeps both the pack name and its generated action
    /// leaves on the same inline route without name checks in ChatService.
    #[serde(default)]
    pub chat_inline_adapter: Option<ChatInlineAdapter>,
    /// Categorization tags for discovery and routing.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Sandbox constraint: "shell", "file", or "none".
    #[serde(default)]
    pub sandbox: Option<String>,
    /// Composition category for tool selection heuristics (e.g., "browser_automation").
    #[serde(default)]
    pub composition_category: Option<String>,
    /// Spend declaration: how this capability consumes external resources.
    /// When present, lowering produces a `MaybeGatedAction::Gated` action.
    #[serde(default)]
    pub spend: Option<SpendDeclaration>,
}

// ============================================================================
// Capability Pack Definition (for pack-defined / YAML-driven providers)
// ============================================================================

/// How a pack capability is implemented.
///
/// New variants (e.g., CdpSequence, Mcp) should be added here when their
/// execution transport is ready — adding a variant is trivial; the transport is the work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImplementationType {
    /// Sequence of calls to other registered capabilities.
    Composite { steps: Vec<CompositeStep> },
    /// Metadata-only definition for a built-in compiled provider.
    /// The `provider_name` identifies which compiled provider handles execution.
    /// Compiled packs are not registered as pack providers — they exist
    /// purely for declarative discovery and schema documentation.
    Compiled { provider_name: String },
    /// Inner-loop tool: the runtime spins up a nested LLM loop with the
    /// pack's `native_action_schemas` as the inner-LLM's catalog. The
    /// outer LLM sees only pack-level handoff/session parameters; the inner
    /// LLM reads the runtime task frame, bounded transcript, runtime ledger,
    /// memories, and artifact manifest, then decides completion on its own via
    /// terminal control tools (`goal_reached`, `cannot_proceed`,
    /// `need_user_input`). Browser is the canonical example; future
    /// iterative tools (gmail, csvkit, metabase_explore) will use this too.
    /// Declared as `type: primitive` in pack `tool_schema.yaml`.
    Primitive {
        /// Validated universal-runtime package retained only for in-process
        /// governed execution. Compatibility catalog fields remain serialized
        /// for discovery, but this proof-bearing package owns dispatch when
        /// present.
        #[serde(skip)]
        runtime_package: Option<Arc<GovernedRuntimeImplementation>>,
        /// Optional compiled provider backing this inner-loop pack. When set,
        /// the inner loop still owns planning and terminal decisions, but each
        /// primitive dispatch is lowered/executed through the existing Rust
        /// provider instead of a subprocess template.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_name: Option<String>,
        /// Legacy/backcompat field from the old outer `intent` parameter
        /// schema. Runtime-context inner-loop tools ignore this value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_description: Option<String>,
        /// Optional CLI command prefix used by the YAML-driven dispatcher.
        /// When the pack name is registered as a Rust dispatcher (today:
        /// `browser`), this field is ignored. Otherwise, the runtime uses
        /// the generic `CliTemplateDispatcher`:
        ///
        /// - If `command` is set, it is the argv prefix
        ///   (e.g. `[gws, gmail]` → `gws gmail <primitive> --flag value`).
        /// - If absent, the pack's `name` is used as the single-element
        ///   prefix (e.g. pack `gmail` → `gmail <primitive> --flag value`).
        ///
        /// Subcommand = primitive name; flags derived from parameter names
        /// (kebab-case); values formatted from `parameter_overrides[<param>].type`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        /// Optional working directory for generic CLI-template inner-loop
        /// subprocesses. Supports scoped path variables such as
        /// `{scope_capabilities_root}`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Optional environment variables set on every generic CLI-template
        /// primitive invocation. Supports scoped path variables and
        /// action-argument interpolation.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// Optional argv tokens appended to every generic CLI-template
        /// primitive invocation.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suffix_args: Vec<String>,
        /// Optional default command timeout for generic CLI-template
        /// primitive invocations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// Optional `LLMOperation` name bound to this pack's inner loop.
        /// Defaults to `<pack_name>_primitive`. The runtime uses this
        /// to pick the profile from `magician-config.yaml`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<String>,
        /// Optional system-prompt template used by the inner LLM. Defaults are
        /// resolved by operation through `prompts::constants::primitive`, with
        /// the generic `primitive_system` prompt as fallback. When the prompt
        /// template is missing at runtime, the inner loop falls back to a
        /// generic hardcoded prompt and logs a warning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<PromptRef>,
    },
    /// Direct process execution via `Command::new(program).arg()` — no shell involved.
    ///
    /// Parameters are passed as OS-level arguments, so metacharacters like `;`, `|`,
    /// `&&` are literal strings. This is the safe alternative to routing through
    /// the shell compiled provider via Composite.
    Command {
        /// Program to execute (e.g., "gws", "curl", "ddgr").
        program: String,
        /// Fixed arguments prepended before parameter-derived args (e.g., ["gmail", "read"]).
        #[serde(default)]
        fixed_args: Vec<String>,
        /// How tool parameters map to command-line arguments.
        #[serde(default)]
        arg_mappings: Vec<CommandArgMapping>,
        /// Fixed arguments appended after all mapped args (e.g., ["--format", "json"]).
        #[serde(default)]
        suffix_args: Vec<String>,
        /// Content type for prompt-injection-safe output wrapping.
        #[serde(default)]
        content_type: ContentType,
        /// Optional environment variables set on every invocation.
        /// Supports `{param}` interpolation from resolved parameters.
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

/// Load-time compiled execution marker for universal-runtime CLI packages.
/// Keeping both the parsed contract and compiled actions avoids re-parsing or
/// recompiling a large action family on every invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernedRuntimeImplementation {
    pub package: tool_runtime_core::manifest_parser::SkillRuntimePackage,
    /// Present only for CLI packages. MCP packages obtain their action catalog
    /// from official-SDK discovery and never manufacture a fake CLI catalog.
    pub actions: Option<tool_runtime_core::action_overrides::CompiledActionCatalog>,
    /// Runtime-owned directory containing a skill-local, self-contained
    /// executable. The directory is discovered from the loaded skill source,
    /// never supplied by the model, and the Phase 6 authority still snapshots
    /// and revalidates the exact declared executable before launch.
    pub executable_directory: Option<PathBuf>,
    /// Transitional scope-owned `.env` authority for static-secret skills.
    /// Values are read only inside sealed credential preparation after
    /// authorization; canonical vault entries take precedence. This field is
    /// removed with the final Phase 7 deprecated-storage cleanup.
    pub legacy_secret_environment: Option<PathBuf>,
}

impl GovernedRuntimeImplementation {
    pub fn cli_actions(
        &self,
    ) -> Result<&tool_runtime_core::action_overrides::CompiledActionCatalog, &'static str> {
        self.actions
            .as_ref()
            .ok_or("governed CLI runtime has no compiled action catalog")
    }
}

/// How a tool parameter maps to a CLI argument in a `Command` implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandArgMapping {
    /// Parameter value passed as a bare positional argument.
    Positional { param: String },
    /// Parameter value passed as `--flag value`.
    Flag { flag: String, param: String },
    /// Boolean parameter emitted as a bare `--flag` when the value is
    /// truthy, and omitted entirely otherwise. Truthy: JSON `true`, the
    /// string `"true"` (case-insensitive), `"1"`, `"yes"`. Anything else
    /// (false, missing, null, empty, "0", etc.) emits nothing.
    /// Use for CLIs that expect `store_true`-style switches like `--quick`,
    /// `--deep`, `--no-auto-window`.
    BoolFlag { flag: String, param: String },
    /// Parameter value is shell-word-split into multiple positional arguments.
    /// Safe: uses lexical splitting, not a shell. Handles single/double quotes.
    /// Use for parameters like `command` that contain subcommands + flags
    /// (e.g., "+triage --max 5" → ["+triage", "--max", "5"]).
    SplitPositional { param: String },
    /// Reads an OS environment variable and passes it as `flag $ENV_VALUE`.
    /// Use for secrets like API tokens that live in the process environment.
    EnvFlag { flag: String, env_var: String },
    /// Insert literal fixed arguments at this position in the arg list.
    /// Use when fixed args need to appear between mapped args.
    FixedArgs { args: Vec<String> },
    /// Append each element of a string-array parameter as a literal argv
    /// token. Use as an escape hatch alongside `Flag` / `BoolFlag` /
    /// `Positional` mappings so the LLM can pass through CLI flags the
    /// schema does not surface as typed parameters yet. Non-array
    /// values are coerced via JSON `to_string` so a stray scalar still
    /// reaches the subprocess (defensive — the JSON Schema validator
    /// should catch this at tool-call time).
    Passthrough { param: String },
}

/// Shared truthiness coercion for `BoolFlag` mappings.
///
/// Truthy: JSON `true`, the strings `"true" / "1" / "yes"` (case-
/// insensitive after trim), and finite non-zero numbers. Everything
/// else — including null, false, `"false"` / `"0"` / `"no"` / empty
/// string, NaN, infinity, arrays, objects — returns false.
///
/// `is_finite()` filters out NaN and ±Infinity so that pathological
/// JSON Number inputs can't trigger the flag.
pub fn coerce_bool_flag_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        },
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(|f| f.is_finite() && f != 0.0)
            .unwrap_or(false),
        _ => false,
    }
}

/// Same truthiness rules as `coerce_bool_flag_value` but for the
/// string-valued defaults declared in `ParameterDef::default`. YAML
/// defaults round-trip as strings, so we apply the same `"true" / "1"
/// / "yes"` rule after trim+lowercase.
pub fn coerce_bool_flag_default(default: &str) -> bool {
    matches!(
        default.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

/// Content type tag for wrapping tool output to defend against prompt injection.
///
/// Used by both YAML-driven `Command` packs (via serde) and programmatic
/// `CommandToolHandler` / `wrap_output()` in `shell_tool`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    /// Generic tool output.
    #[default]
    ToolOutput,
    /// Email content from external source.
    Email,
    /// Web content from external source.
    Web,
    /// File content from external source.
    File,
}

/// A single step inside a Composite implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositeStep {
    /// Tool name to invoke (must be registered in the registry).
    pub tool: String,
    /// Parameters to pass to the sub-tool.
    #[serde(default)]
    pub parameters: HashMap<String, String>,
}

/// Parameter definition for a capability pack.
///
/// The canonical description of a parameter's accepted shape is the
/// [`Self::schema`] field (a JSON Schema). Pack YAMLs may either provide
/// `schema:` directly for full JSON Schema power (nested arrays, object
/// properties, regex patterns, formats, oneOf, etc.), or use the legacy
/// shorthand fields (`param_type`, `enum_values`, `description`, `default`)
/// — in which case [`Deserialize`] synthesizes the equivalent schema. After
/// deserialization, `schema` is always populated and is the single source of
/// truth that all catalog generators emit to the LLM.
///
/// The legacy shorthand fields remain on the struct for downstream code
/// paths that rely on the coarse type tag (parameter coercion, tool
/// matching, body-template fallback) and for clean YAML round-tripping when
/// no explicit schema was supplied.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ParameterDef {
    /// Parameter name.
    pub name: String,
    /// Whether this parameter is required.
    #[serde(default)]
    pub required: bool,
    /// Default value when the parameter is not provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Coarse type hint for coercion (string, integer, number, boolean, array, object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_type: Option<ParameterType>,
    /// Alternative names that resolve to this parameter (e.g., ["ref"] for "selector").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Allowed values for enum-constrained parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    /// Canonical JSON Schema for the parameter's accepted value. Always
    /// populated after deserialization. When the YAML provides `schema:`,
    /// it's used verbatim; otherwise the schema is derived from
    /// `param_type` + `enum_values` + `description` + `default`. All
    /// catalog generators (chat outer-loop tool list, autonomous outer-loop
    /// tool list) emit this schema to the LLM.
    #[serde(default = "default_param_schema")]
    pub schema: serde_json::Value,
}

fn default_param_schema() -> serde_json::Value {
    serde_json::json!({"type": "string"})
}

impl Default for ParameterDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            required: false,
            default: None,
            description: None,
            param_type: None,
            aliases: Vec::new(),
            enum_values: None,
            schema: default_param_schema(),
        }
    }
}

impl<'de> Deserialize<'de> for ParameterDef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Mirror struct that captures every legal YAML input field,
        // including both the legacy shorthand and the modern `schema:` key.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            name: String,
            #[serde(default)]
            required: bool,
            #[serde(default)]
            default: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            param_type: Option<ParameterType>,
            #[serde(default)]
            aliases: Vec<String>,
            #[serde(default)]
            enum_values: Option<Vec<String>>,
            #[serde(default)]
            schema: Option<serde_json::Value>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let schema = raw.schema.clone().unwrap_or_else(|| {
            derive_schema_from_legacy(
                raw.param_type.as_ref(),
                raw.enum_values.as_deref(),
                raw.description.as_deref(),
                raw.default.as_deref(),
            )
        });
        Ok(ParameterDef {
            name: raw.name,
            required: raw.required,
            default: raw.default,
            description: raw.description,
            param_type: raw.param_type,
            aliases: raw.aliases,
            enum_values: raw.enum_values,
            schema,
        })
    }
}

/// Public helper for catalog generators: produce the canonical emission
/// schema for a `ParameterDef`. Returns `param.schema` verbatim when it's
/// populated (the normal case after YAML deserialization), and falls back to
/// the legacy shorthand derivation when `param.schema` is `Value::Null`
/// (which happens only for in-memory test/runtime literals built before the
/// `schema` field was canonical). This is the one entry point for "give me
/// the JSON Schema I should hand to the LLM".
pub fn derive_param_schema_for_emission(param: &ParameterDef) -> serde_json::Value {
    if !param.schema.is_null() {
        return param.schema.clone();
    }
    derive_schema_from_legacy(
        param.param_type.as_ref(),
        param.enum_values.as_deref(),
        param.description.as_deref(),
        param.default.as_deref(),
    )
}

/// Build a JSON Schema for a parameter from the legacy shorthand fields.
/// Used when the YAML doesn't supply an explicit `schema:` block. Mirrors the
/// shape callers expect: `{"type": ..., ["description":...], ["enum":...],
/// ["default":...]}`. Unknown / missing types default to `string`.
fn derive_schema_from_legacy(
    param_type: Option<&ParameterType>,
    enum_values: Option<&[String]>,
    description: Option<&str>,
    default: Option<&str>,
) -> serde_json::Value {
    let json_type = match param_type {
        Some(ParameterType::String) | None => "string",
        Some(ParameterType::Integer) => "integer",
        Some(ParameterType::Number) => "number",
        Some(ParameterType::Boolean) => "boolean",
        Some(ParameterType::Array) => "array",
        Some(ParameterType::Object) => "object",
    };
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), serde_json::json!(json_type));
    // `param_type: array` says nothing about the elements, and `items: {}`
    // says exactly that. Providers require the key: Gemini closes a Live
    // session 1007 at setup and returns 400 to generateContent when an array
    // has no `items`, and OpenAI strict mode refuses it too. A pack that
    // knows its element shape declares an explicit `schema:` instead.
    if matches!(param_type, Some(ParameterType::Array)) {
        schema.insert(
            "items".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    if let Some(desc) = description.filter(|d| !d.trim().is_empty()) {
        schema.insert("description".to_string(), serde_json::json!(desc));
    }
    if let Some(values) = enum_values.filter(|v| !v.is_empty()) {
        schema.insert("enum".to_string(), serde_json::json!(values));
    }
    if let Some(default) = default.filter(|d| !d.is_empty()) {
        schema.insert("default".to_string(), serde_json::json!(default));
    }
    serde_json::Value::Object(schema)
}

/// A complete capability pack definition loaded from YAML.
///
/// Describes a tool's name, parameters, and implementation strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilityPackDefinition {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Semantic version string.
    #[serde(default)]
    pub version: Option<String>,
    /// LLM guide text — free-form instructions sent to the planner.
    #[serde(default)]
    pub guide: Option<String>,
    /// Action-specific native tool schema slices for multi-action compiled
    /// packs such as `browser`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub native_action_schemas: HashMap<String, NativeActionSchemaDef>,
    /// Declared parameters.
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,
    /// Implementation strategy.
    pub implementation: ImplementationType,
    /// Declarative execution metadata (timeouts, categories, sandbox).
    #[serde(default)]
    pub execution: Option<ExecutionMetadata>,
    /// Authentication requirements for this capability.
    /// When present, the system checks auth before first use and
    /// auto-reauths on failure matching `error_patterns`.
    #[serde(default)]
    pub auth: Option<CapabilityAuthConfig>,
    /// First-class reliability metadata (reliability pattern #2).
    /// Declares safe-to-retry / read-only / concurrency
    /// shape so the executor can schedule, retry, and recover safely
    /// without per-capability special-case code.
    ///
    /// `None` (the default) keeps the legacy permissive behaviour: the
    /// runtime treats the capability as potentially side-effectful and
    /// non-idempotent. Capabilities should opt in by declaring
    /// `reliability:` in YAML.
    #[serde(default)]
    pub reliability: Option<CapabilityReliabilityMetadata>,
    /// Declarative, provider-neutral contract used to derive bounded model,
    /// spoken, and display views from this capability's result. Consumers
    /// resolve this through the shared projection registry; surface services
    /// must never select contracts from tool names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_projection: Option<ProjectionContractSpec>,
}

/// First-class reliability contract for a capability pack.
///
/// Declared in YAML so the executor can decide — without per-pack
/// special cases — whether a capability is safe to retry, safe to
/// parallelize, and what kind of failures warrant which recovery
/// shape. Phase 1 of reliability pattern #2: schema only; consumer wiring (auto-
/// retry, concurrency scheduling) lands incrementally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapabilityReliabilityMetadata {
    /// `true` when the capability only reads state and never mutates
    /// the world (e.g., a query, a fetch, a snapshot). The executor
    /// can freely retry, parallelise, and dedupe read-only calls.
    /// Default `false` (assume mutation).
    #[serde(default)]
    pub read_only: bool,
    /// `true` when running the same call twice with the same inputs is
    /// safe — the second call is either a no-op or a duplicate write
    /// the downstream system tolerates. Independent of `read_only`:
    /// a mutating idempotent call (e.g., PUT with the same body) is
    /// fine to retry. Default `false`.
    #[serde(default)]
    pub idempotent: bool,
    /// Optional concurrency partition key. Calls sharing the same
    /// `concurrency_key` value are serialised by the executor so they
    /// don't race against each other. `None` means "no constraint" —
    /// the executor may schedule freely subject to global concurrency
    /// limits.
    #[serde(default)]
    pub concurrency_key: Option<String>,
    /// Retry policy applied when the capability returns a transient
    /// failure. `None` keeps the legacy per-call default (one shot,
    /// surface the error to the LLM). When provided, the executor
    /// applies the policy before propagating failure.
    #[serde(default)]
    pub retry_policy: Option<CapabilityRetryPolicy>,
    /// Optional verification policy: when present, declares that the
    /// capability's result should be checked against a verifier before
    /// being treated as success. Phase 1 records the policy; the
    /// verifier-pass step lands with reliability pattern #1 evidence wiring.
    #[serde(default)]
    pub verification_policy: Option<CapabilityVerificationPolicy>,
    /// Optional preflight policy: declares what cheap probes must
    /// succeed before the executor admits a task that wants to use
    /// this capability. Consumed by the per-task preflight gate
    /// (reliability pattern #3). `None` falls back to the capability's `auth`
    /// config (the legacy shape).
    #[serde(default)]
    pub preflight: Option<CapabilityPreflightPolicy>,
}

/// Retry shape for a capability — bounded attempts with optional
/// backoff and an optional whitelist of error patterns that mark a
/// failure as transient. Anything outside the whitelist is treated as
/// permanent and surfaced to the LLM immediately so it can adapt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapabilityRetryPolicy {
    /// Maximum attempt count including the initial call. `1` (or `0`)
    /// disables retry. Typical value: `3`.
    #[serde(default)]
    pub max_attempts: u32,
    /// Initial backoff between attempts, in milliseconds. The executor
    /// applies linear backoff (`attempt * backoff_ms`) by default.
    #[serde(default)]
    pub backoff_ms: u32,
    /// Whitelist of stderr/stdout substrings that mark the failure as
    /// transient (rate limit, network blip). Empty means "retry any
    /// failure within the attempt budget."
    #[serde(default)]
    pub retry_on: Vec<String>,
}

/// Declares how a capability's result should be verified before being
/// counted as success. Phase 1 records the declaration; the verifier-
/// step plumbing lands with the evidence-gate work (reliability pattern #1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapabilityVerificationPolicy {
    /// Name of the verifier strategy (free-form; consumers map it to a
    /// concrete check). Conventional values: `expected_artifact`,
    /// `nonzero_exit`, `schema_validate`, `regex_match`.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Free-form parameters for the verifier — schema is strategy-
    /// specific.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// Per-capability readiness diagnostic. Explicit setup/diagnostic surfaces may
/// run `probe` for the capability the user asked to inspect. Agentic bootstrap
/// and resume never run these probes: catalog membership is authority, not
/// evidence that the current request will invoke the capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapabilityPreflightPolicy {
    /// Optional probe command. Exit 0 = ready, non-zero = blocker.
    /// When absent an explicit diagnostic falls back to
    /// `auth.check_command`.
    #[serde(default)]
    pub probe: Option<String>,
    /// Human-readable hint surfaced to the user when the probe fails
    /// — should describe what action unblocks the capability (e.g.,
    /// "Run `gws auth setup` and complete the OAuth flow").
    #[serde(default)]
    pub blocker_hint: Option<String>,
    /// `true` when an explicit diagnostic should report failure as a
    /// blocker (default). `false` reports a warning.
    #[serde(default = "default_preflight_required")]
    pub required: bool,
}

fn default_preflight_required() -> bool {
    true
}

/// Authentication lifecycle config for a capability pack.
///
/// Declared in YAML so any CLI-based capability can specify how to
/// check, setup, and re-authenticate without code changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilityAuthConfig {
    /// Whether this capability requires authentication before use.
    #[serde(default)]
    pub required: bool,
    /// Command to run for first-time auth setup (e.g., `gws auth setup`, QR scan).
    #[serde(default)]
    pub setup_command: Option<String>,
    /// Command to verify auth is still valid (exit 0 = ok, non-zero = expired).
    #[serde(default)]
    pub check_command: Option<String>,
    /// Command to re-authenticate when expired. Falls back to `setup_command` if not set.
    #[serde(default)]
    pub reauth_command: Option<String>,
    /// Strings in command output that indicate auth failure.
    /// If any pattern matches stderr/stdout, the system runs `reauth_command` and retries.
    #[serde(default)]
    pub error_patterns: Vec<String>,
}

/// Reference to a versioned prompt template stored under
/// `data/magician_v2/prompts/<name>_v<version>.json`. Used by inner-loop
/// packs to declare which system prompt drives their inner LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRef {
    pub name: String,
    pub version: String,
}

/// Action-specific native schema metadata declared by a capability pack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct NativeActionSchemaDef {
    /// Human-facing tool description sent to the inner LLM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Reliability belongs to the callable action, especially for mixed packs
    /// that expose both reads and writes. A leaf never inherits pack metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<CapabilityReliabilityMetadata>,
    /// Shared parameter names to expose on this action-level native tool.
    #[serde(default)]
    pub parameters: Vec<String>,
    /// Parameters that should be required for this action-level native tool.
    #[serde(default)]
    pub required: Vec<String>,
    /// Per-parameter JSON Schema fragments to merge into exposed properties.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameter_overrides: HashMap<String, serde_json::Value>,
    /// Optional argv tokens inserted after the pack-level CLI base for this
    /// primitive. When absent, the primitive tool name is used as the command
    /// token for backward compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// When true, do not insert the primitive tool name before action args.
    /// Used for wrappers where the action is named `run` but the executable
    /// should receive only the exact user-provided argv tokens.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_tool_name: bool,
    /// Optional command-argument mapping for this primitive. When absent, the
    /// generic dispatcher appends exact `args: string[]` tokens when present,
    /// falling back to `--kebab-case value` flags only for old/simple schemas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_mappings: Vec<CommandArgMapping>,
    /// Optional argv tokens appended after this primitive's mapped arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suffix_args: Vec<String>,
    /// Optional per-primitive timeout override in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl CapabilityPackDefinition {
    /// Read-only convenience: does this capability declare itself as
    /// read-only? Defaults to `false` when no `reliability` block is
    /// declared in YAML — assume mutation.
    pub fn is_read_only(&self) -> bool {
        self.reliability
            .as_ref()
            .map(|r| r.read_only)
            .unwrap_or(false)
    }

    /// Read-only convenience: does this capability declare itself as
    /// idempotent? Defaults to `false` — assume duplicate calls are
    /// not safe.
    pub fn is_idempotent(&self) -> bool {
        self.reliability
            .as_ref()
            .map(|r| r.idempotent)
            .unwrap_or(false)
    }

    /// Read the concurrency partition key (`reliability.concurrency_key`).
    /// `None` means no constraint.
    pub fn concurrency_key(&self) -> Option<&str> {
        self.reliability
            .as_ref()
            .and_then(|r| r.concurrency_key.as_deref())
    }

    /// Read the retry policy (`reliability.retry_policy`). `None`
    /// keeps the legacy one-shot behaviour.
    pub fn retry_policy(&self) -> Option<&CapabilityRetryPolicy> {
        self.reliability
            .as_ref()
            .and_then(|r| r.retry_policy.as_ref())
    }

    /// Read the verification policy (`reliability.verification_policy`).
    pub fn verification_policy(&self) -> Option<&CapabilityVerificationPolicy> {
        self.reliability
            .as_ref()
            .and_then(|r| r.verification_policy.as_ref())
    }

    /// Read the preflight policy (`reliability.preflight`). When
    /// absent, callers should fall back to the capability's `auth`
    /// config for the legacy probe shape.
    pub fn preflight_policy(&self) -> Option<&CapabilityPreflightPolicy> {
        self.reliability.as_ref().and_then(|r| r.preflight.as_ref())
    }

    /// Convert this pack definition into a `ToolDefinition` suitable for
    /// `RegistryService`. Maps `parameters` → JSON Schema `inputSchema` and
    /// copies categories from `ExecutionMetadata`.
    ///
    /// **Primitive packs** (`implementation.type: primitive`) usually expose
    /// an **empty parameter set** to the outer LLM. The outer LLM's role is
    /// routing/handoff; the inner LLM reads the runtime task frame, bounded
    /// exact transcript, runtime ledger, memory, and artifacts directly.
    ///
    /// Browser is the exception: it may expose session-level parameters such as
    /// `connection_mode` so the outer LLM can choose between CDP/profile reuse
    /// and a fresh headed/headless browser before the inner loop starts. These
    /// are not primitive browser commands.
    pub fn to_tool_definition(&self) -> tool_runtime_core::registry::types::ToolDefinition {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        let is_primitive = matches!(&self.implementation, ImplementationType::Primitive { .. });
        let exposes_session_parameters = is_primitive && self.name == "browser";
        if is_primitive && !exposes_session_parameters {
            return self.build_tool_definition(properties, required);
        }

        for param in &self.parameters {
            // Canonical schema field is the source of truth — emit it
            // verbatim. In-memory literals built before YAML deserialization
            // may have `schema: Value::Null`; fall back to legacy derivation
            // in that case (covers test fixtures and runtime synthesis).
            let prop = derive_param_schema_for_emission(param);
            properties.insert(param.name.clone(), prop);
            if param.required {
                required.push(serde_json::Value::String(param.name.clone()));
            }
        }

        self.build_tool_definition(properties, required)
    }

    /// Common tail for [`Self::to_tool_definition`]: assemble the
    /// `inputSchema`, copy categories + execution metadata, and produce
    /// the final `ToolDefinition`. Shared between the standard parameter
    /// path and the inner-loop synthesis path.
    fn build_tool_definition(
        &self,
        properties: serde_json::Map<String, serde_json::Value>,
        required: Vec<serde_json::Value>,
    ) -> tool_runtime_core::registry::types::ToolDefinition {
        let input_schema = serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        });

        let categories = self
            .execution
            .as_ref()
            .map(|e| e.categories.clone())
            .unwrap_or_default();

        let mut metadata = std::collections::HashMap::new();
        if let Some(ref exec) = self.execution {
            if let Some(ref sandbox) = exec.sandbox {
                metadata.insert(
                    "sandbox".to_string(),
                    serde_json::Value::String(sandbox.clone()),
                );
            }
            if let Some(ref cc) = exec.composition_category {
                metadata.insert(
                    "composition_category".to_string(),
                    serde_json::Value::String(cc.clone()),
                );
            }
        }

        tool_runtime_core::registry::types::ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone().unwrap_or_default(),
            input_schema,
            categories,
            metadata,
            hidden: false,
            enabled: true,
        }
    }

    /// Recover declared pack params from a nested `body` JSON object when the
    /// model emits legacy command-style arguments like `body="{\"query\":\"...\"}"`.
    ///
    /// Only applies when the pack does not itself declare a first-class `body`
    /// parameter. Recognized declared fields (or aliases) are flattened into
    /// the top-level param map so normal schema resolution can continue.
    pub fn normalize_embedded_body_parameters(
        &self,
        provided: &HashMap<String, serde_json::Value>,
    ) -> HashMap<String, serde_json::Value> {
        let mut normalized = provided.clone();
        if self.parameters.iter().any(|def| def.name == "body") {
            return normalized;
        }

        let Some(body_value) = provided.get("body") else {
            return normalized;
        };
        let parsed_object = match body_value {
            serde_json::Value::Object(map) => Some(map.clone()),
            serde_json::Value::String(value) => serde_json::from_str::<serde_json::Value>(value)
                .ok()
                .and_then(|parsed| parsed.as_object().cloned()),
            _ => None,
        };
        let Some(parsed_object) = parsed_object else {
            return normalized;
        };

        let mut known_keys = HashMap::new();
        for def in &self.parameters {
            known_keys.insert(def.name.clone(), def.name.clone());
            for alias in &def.aliases {
                known_keys.insert(alias.clone(), def.name.clone());
            }
        }

        let mut flattened_any = false;
        for (raw_key, value) in parsed_object {
            let Some(target_key) = known_keys.get(&raw_key).cloned() else {
                continue;
            };
            if normalized.contains_key(&target_key) || normalized.contains_key(&raw_key) {
                continue;
            }
            normalized.insert(target_key, value);
            flattened_any = true;
        }

        if flattened_any {
            normalized.remove("body");
        }

        normalized
    }

    /// Like `resolve_params`, but returns `HashMap<String, String>` — applies
    /// defaults, resolves aliases, validates required params, but does NOT
    /// perform type coercion. This lets compiled providers feed clean params to
    /// existing lowering functions without changing their string-based signatures.
    pub fn resolve_params_to_strings(
        &self,
        provided: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, ExecutionError> {
        let mut resolved: HashMap<String, String> = HashMap::new();
        let mut consumed_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut alias_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for def in &self.parameters {
            for alias in &def.aliases {
                alias_keys.insert(alias.clone());
            }
        }

        for def in &self.parameters {
            let raw_value = provided
                .get(&def.name)
                .or_else(|| def.aliases.iter().find_map(|alias| provided.get(alias)));

            if let Some(value) = raw_value {
                if provided.contains_key(&def.name) {
                    consumed_keys.insert(def.name.clone());
                }
                for alias in &def.aliases {
                    if provided.contains_key(alias) {
                        consumed_keys.insert(alias.clone());
                    }
                }
                resolved.insert(def.name.clone(), value.clone());
            } else if let Some(ref default) = def.default {
                resolved.insert(def.name.clone(), default.clone());
            } else if def.required {
                return Err(ExecutionError::Step(format!(
                    "Capability '{}' requires parameter '{}' but it was not provided",
                    self.name, def.name
                )));
            }
        }

        // Pass through unknown params, skip alias keys
        for (key, value) in provided {
            if !resolved.contains_key(key)
                && !consumed_keys.contains(key)
                && !alias_keys.contains(key)
            {
                resolved.insert(key.clone(), value.clone());
            }
        }

        Ok(resolved)
    }

    /// Resolve provided parameter values against the declared schema.
    ///
    /// - Checks primary name first, then aliases when looking up provided values.
    /// - Uses `coerce_value` to produce typed JSON values based on `param_type`.
    /// - Required params without a value or default produce an error.
    /// - Optional params get their default value when absent.
    /// - Unknown params are passed through (for forward-compat), skipping alias keys.
    pub fn resolve_params(
        &self,
        provided: &HashMap<String, serde_json::Value>,
    ) -> Result<HashMap<String, serde_json::Value>, ExecutionError> {
        let mut resolved: HashMap<String, serde_json::Value> = HashMap::new();
        // Track which provided keys were consumed (by name or alias)
        let mut consumed_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Collect all alias→primary mappings for skipping during passthrough
        let mut alias_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for def in &self.parameters {
            for alias in &def.aliases {
                alias_keys.insert(alias.clone());
            }
        }

        for def in &self.parameters {
            // Check primary name first, then aliases
            let raw_value = provided.get(&def.name).or_else(|| {
                def.aliases.iter().find_map(|alias| {
                    provided.get(alias).inspect(|_| {
                        // Mark alias as consumed so it's not passed through
                    })
                })
            });

            if let Some(value) = raw_value {
                // Mark the key as consumed
                if provided.contains_key(&def.name) {
                    consumed_keys.insert(def.name.clone());
                }
                for alias in &def.aliases {
                    if provided.contains_key(alias) {
                        consumed_keys.insert(alias.clone());
                    }
                }
                // Extract &str for coerce_value; fall back to JSON representation for non-strings.
                let raw_str_buf;
                let raw_str: &str = if let Some(s) = value.as_str() {
                    s
                } else {
                    raw_str_buf = value.to_string();
                    &raw_str_buf
                };
                resolved.insert(
                    def.name.clone(),
                    coerce_value(raw_str, def.param_type.as_ref()),
                );
            } else if let Some(ref default) = def.default {
                resolved.insert(
                    def.name.clone(),
                    coerce_value(default, def.param_type.as_ref()),
                );
            } else if def.required {
                return Err(ExecutionError::Step(format!(
                    "Capability '{}' requires parameter '{}' but it was not provided",
                    self.name, def.name
                )));
            }
            // Optional with no default and not provided → skip
        }

        // Pass through unknown params for forward-compatibility,
        // but skip alias keys to prevent duplication
        for (key, value) in provided {
            if !resolved.contains_key(key)
                && !consumed_keys.contains(key)
                && !alias_keys.contains(key)
            {
                resolved.insert(key.clone(), value.clone());
            }
        }

        Ok(resolved)
    }
}

/// Coerce a raw string value into a typed JSON value based on the parameter type.
///
/// - `Integer` → parse as i64 → `Value::Number`
/// - `Number` → parse as f64 → `Value::Number`
/// - `Boolean` → flexible truthy/falsy (true/1/yes) → `Value::Bool`
/// - `Array`/`Object` → try `serde_json::from_str`, fall back to `Value::String`
/// - `String`/`None` → `Value::String` (current behavior)
fn coerce_value(raw: &str, param_type: Option<&ParameterType>) -> serde_json::Value {
    match param_type {
        Some(ParameterType::Integer) => {
            if let Ok(n) = raw.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else {
                warn!(
                    "[CAPABILITY] Expected integer but got '{}'; passing as string",
                    raw
                );
                serde_json::Value::String(raw.to_string())
            }
        },
        Some(ParameterType::Number) => {
            if let Ok(n) = raw.parse::<f64>() {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|| {
                        warn!(
                            "[CAPABILITY] f64 '{}' is non-finite (NaN/Inf); passing as string",
                            raw
                        );
                        serde_json::Value::String(raw.to_string())
                    })
            } else {
                warn!(
                    "[CAPABILITY] Expected number but got '{}'; passing as string",
                    raw
                );
                serde_json::Value::String(raw.to_string())
            }
        },
        Some(ParameterType::Boolean) => {
            let lower = raw.to_lowercase();
            match lower.as_str() {
                "true" | "1" | "yes" => serde_json::Value::Bool(true),
                "false" | "0" | "no" => serde_json::Value::Bool(false),
                _ => {
                    warn!(
                        "[CAPABILITY] Expected boolean but got '{}'; passing as string",
                        raw
                    );
                    serde_json::Value::String(raw.to_string())
                },
            }
        },
        Some(ParameterType::Array) | Some(ParameterType::Object) => serde_json::from_str(raw)
            .unwrap_or_else(|_| {
                warn!(
                    "[CAPABILITY] Expected JSON array/object but got '{}'; passing as string",
                    raw
                );
                serde_json::Value::String(raw.to_string())
            }),
        Some(ParameterType::String) | None => serde_json::Value::String(raw.to_string()),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Helper to build a minimal ParameterDef for tests.
    fn param(name: &str, required: bool, default: Option<&str>) -> ParameterDef {
        ParameterDef {
            name: name.to_string(),
            required,
            default: default.map(|s| s.to_string()),
            description: None,
            param_type: None,
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }
    }

    /// Helper to build a ParameterDef with type info.
    fn typed_param(name: &str, param_type: ParameterType) -> ParameterDef {
        ParameterDef {
            name: name.to_string(),
            required: false,
            default: None,
            description: None,
            param_type: Some(param_type),
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }
    }

    /// Helper to build a minimal CapabilityPackDefinition.
    fn test_def(params: Vec<ParameterDef>) -> CapabilityPackDefinition {
        CapabilityPackDefinition {
            name: "test_tool".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: params,
            implementation: ImplementationType::Composite { steps: Vec::new() },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        }
    }

    /// A pack parameter declared `param_type: array` with no explicit
    /// `schema:` must reach providers with `items` — Gemini closes a Live
    /// session 1007 at setup without it (the shipped `web_search`
    /// `blocked_domains` did exactly that to every Gemini 3.8 voice call), and
    /// `items: {}` is the shape that claims nothing about the elements.
    #[test]
    fn array_parameters_without_a_schema_derive_unconstrained_items() {
        let yaml = r#"
name: blocked_domains
required: false
param_type: array
description: Optional block-list of domains.
"#;
        let param: ParameterDef = serde_yaml::from_str(yaml).expect("parameter parses");
        assert_eq!(
            param.schema,
            serde_json::json!({
                "type": "array",
                "items": {},
                "description": "Optional block-list of domains."
            })
        );
        assert_eq!(
            derive_param_schema_for_emission(&typed_param("tags", ParameterType::Array))["items"],
            serde_json::json!({})
        );
        // Scalars are untouched, and an explicit schema wins verbatim.
        assert!(
            derive_param_schema_for_emission(&typed_param("q", ParameterType::String))
                .get("items")
                .is_none()
        );
        let explicit: ParameterDef = serde_yaml::from_str(
            r#"
name: rows
param_type: array
schema:
  type: array
  items:
    type: object
    properties:
      id: { type: string }
"#,
        )
        .expect("parameter parses");
        assert_eq!(explicit.schema["items"]["type"], "object");
    }

    #[test]
    fn reliability_defaults_when_block_absent() {
        let def = test_def(vec![]);
        assert!(
            !def.is_read_only(),
            "absent reliability block must default read_only=false (assume mutation)"
        );
        assert!(
            !def.is_idempotent(),
            "absent reliability block must default idempotent=false"
        );
        assert!(def.concurrency_key().is_none());
        assert!(def.retry_policy().is_none());
        assert!(def.verification_policy().is_none());
        assert!(def.preflight_policy().is_none());
    }

    #[test]
    fn reliability_parses_from_yaml() {
        // Pin the YAML shape so capability authors get parse errors,
        // not silent default-fallback, when they mis-spell a field.
        let yaml = r#"
name: test_tool
description: Test capability
implementation:
  type: composite
  steps: []
reliability:
  read_only: true
  idempotent: true
  concurrency_key: scope-shared
  retry_policy:
    max_attempts: 3
    backoff_ms: 250
    retry_on:
      - "rate limit"
      - "ECONNRESET"
  verification_policy:
    strategy: expected_artifact
    params:
      name: answer.md
  preflight:
    probe: "gws auth check"
    blocker_hint: "Run `gws auth setup` to authenticate."
    required: true
"#;
        let def: CapabilityPackDefinition = serde_yaml::from_str(yaml).expect("yaml parses");
        assert!(def.is_read_only());
        assert!(def.is_idempotent());
        assert_eq!(def.concurrency_key(), Some("scope-shared"));

        let retry = def.retry_policy().expect("retry policy present");
        assert_eq!(retry.max_attempts, 3);
        assert_eq!(retry.backoff_ms, 250);
        assert_eq!(retry.retry_on, vec!["rate limit", "ECONNRESET"]);

        let verify = def
            .verification_policy()
            .expect("verification policy present");
        assert_eq!(verify.strategy.as_deref(), Some("expected_artifact"));

        let preflight = def.preflight_policy().expect("preflight policy present");
        assert_eq!(preflight.probe.as_deref(), Some("gws auth check"));
        assert!(preflight.required);
        assert_eq!(
            preflight.blocker_hint.as_deref(),
            Some("Run `gws auth setup` to authenticate.")
        );
    }

    #[test]
    fn preflight_required_defaults_to_true() {
        let yaml = r#"
name: test_tool
implementation:
  type: composite
  steps: []
reliability:
  preflight:
    probe: "echo ok"
"#;
        let def: CapabilityPackDefinition = serde_yaml::from_str(yaml).expect("yaml parses");
        let preflight = def.preflight_policy().expect("preflight policy present");
        assert!(
            preflight.required,
            "preflight.required must default to true when YAML omits it"
        );
    }

    #[test]
    fn resolve_params_required_present() {
        let def = test_def(vec![param("url", true, None)]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("url".to_string(), serde_json::json!("https://example.com"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("url"),
            Some(&serde_json::Value::String(
                "https://example.com".to_string()
            ))
        );
    }

    #[test]
    fn resolve_params_required_missing_errors() {
        let def = test_def(vec![param("url", true, None)]);

        let provided: HashMap<String, serde_json::Value> = HashMap::new();
        let result = def.resolve_params(&provided);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("url"));
    }

    #[test]
    fn resolve_params_optional_with_default() {
        let def = test_def(vec![
            param("timeout", false, Some("30")),
            param("retries", false, None),
        ]);

        let provided: HashMap<String, serde_json::Value> = HashMap::new();
        let resolved = def.resolve_params(&provided).unwrap();

        // timeout should get default
        assert_eq!(
            resolved.get("timeout"),
            Some(&serde_json::Value::String("30".to_string()))
        );
        // retries optional with no default → not present
        assert!(resolved.get("retries").is_none());
    }

    #[test]
    fn resolve_params_unknown_passthrough() {
        let def = test_def(vec![]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("extra".to_string(), serde_json::json!("value"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("extra"),
            Some(&serde_json::Value::String("value".to_string()))
        );
    }

    #[test]
    fn typed_coercion_integer() {
        let def = test_def(vec![typed_param("count", ParameterType::Integer)]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("count".to_string(), serde_json::json!("42"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(resolved.get("count"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn typed_coercion_boolean() {
        let def = test_def(vec![typed_param("enabled", ParameterType::Boolean)]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("enabled".to_string(), serde_json::json!("yes"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("enabled"),
            Some(&serde_json::Value::Bool(true))
        );

        // Also test "false"
        provided.insert("enabled".to_string(), serde_json::json!("false"));
        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("enabled"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn alias_resolution() {
        let def = test_def(vec![ParameterDef {
            name: "selector".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec!["ref".to_string()],
            enum_values: None,
            schema: serde_json::Value::Null,
        }]);

        // Provide value via alias "ref" instead of primary name "selector"
        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("ref".to_string(), serde_json::json!("#my-button"));

        let resolved = def.resolve_params(&provided).unwrap();
        // Should be resolved under the primary name "selector"
        assert_eq!(
            resolved.get("selector"),
            Some(&serde_json::Value::String("#my-button".to_string()))
        );
        // Alias key should NOT appear in output
        assert!(resolved.get("ref").is_none());
    }

    #[test]
    fn backward_compat_no_type() {
        // YAML without param_type should still work — everything stays as String
        let def = test_def(vec![param("url", true, None)]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("url".to_string(), serde_json::json!("42"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("url"),
            Some(&serde_json::Value::String("42".to_string()))
        );
    }

    #[test]
    fn compiled_variant_serde() {
        let impl_type = ImplementationType::Compiled {
            provider_name: "browser".to_string(),
        };

        let json = serde_json::to_string(&impl_type).unwrap();
        let deserialized: ImplementationType = serde_json::from_str(&json).unwrap();
        assert_eq!(impl_type, deserialized);

        // Also verify YAML round-trip
        let yaml = serde_yaml::to_string(&impl_type).unwrap();
        let from_yaml: ImplementationType = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(impl_type, from_yaml);
    }

    #[test]
    fn execution_metadata_serde() {
        let meta = ExecutionMetadata {
            requires_browser_session: Some(true),
            default_timeout_secs: Some(30),
            chat_inline_adapter: Some(ChatInlineAdapter::TutorScreenDraw),
            categories: vec!["browser".to_string(), "web_automation".to_string()],
            sandbox: Some("none".to_string()),
            composition_category: Some("browser_automation".to_string()),
            spend: None,
        };

        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: ExecutionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, deserialized);

        // Default should also round-trip
        let default_meta = ExecutionMetadata::default();
        let json2 = serde_json::to_string(&default_meta).unwrap();
        let deserialized2: ExecutionMetadata = serde_json::from_str(&json2).unwrap();
        assert_eq!(default_meta, deserialized2);
    }

    #[test]
    fn registry_resolves_declared_action_leaf_to_parent_execution_metadata() {
        let definition: CapabilityPackDefinition = serde_yaml::from_str(
            r#"
name: renamed-overlay-tool
native_action_schemas:
  render:
    parameters: []
implementation:
  type: command
  program: echo
execution:
  default_timeout_secs: 5
  chat_inline_adapter: tutor_screen_draw
"#,
        )
        .expect("declarative inline adapter pack");
        let registry = CapabilityRegistry::new();
        registry.set_pack_definition(&definition.name.clone(), definition);

        let parent = registry
            .resolve_tool_definition("renamed-overlay-tool")
            .expect("parent definition");
        assert_eq!(parent.action, None);
        assert_eq!(
            parent
                .definition
                .execution
                .and_then(|metadata| metadata.chat_inline_adapter),
            Some(ChatInlineAdapter::TutorScreenDraw)
        );

        let leaf = registry
            .resolve_tool_definition("renamed-overlay-tool__render")
            .expect("declared action leaf");
        assert_eq!(leaf.action.as_deref(), Some("render"));
        assert_eq!(leaf.definition.name, "renamed-overlay-tool");
        assert!(registry
            .resolve_tool_definition("renamed-overlay-tool__invented")
            .is_none());
    }

    #[test]
    fn scoped_registry_preserves_parent_definition_for_allowed_generated_leaf() {
        let definition: CapabilityPackDefinition = serde_yaml::from_str(
            r#"
name: mixed
native_action_schemas:
  read:
    parameters: []
    reliability:
      read_only: true
      idempotent: true
  write:
    parameters: []
implementation:
  type: command
  program: echo
"#,
        )
        .expect("mixed declarative pack");
        let registry = CapabilityRegistry::new();
        registry.set_pack_definition(&definition.name.clone(), definition);

        let scoped = registry.scoped(&["mixed__read".to_string()]);
        let reliability = scoped
            .resolve_invocation_reliability("mixed__read")
            .expect("allowed leaf keeps its parent reliability contract");
        assert!(reliability.read_only);
        assert!(reliability.idempotent);
        assert!(scoped.resolve_tool_definition("mixed__write").is_none());
        assert!(
            scoped
                .tool_names()
                .iter()
                .all(|name| name != "mixed__write"),
            "retaining parent metadata must not expose a sibling provider"
        );
    }

    #[test]
    fn resolve_params_skips_alias_passthrough() {
        // When "ref" is declared as an alias for "selector", providing "ref"
        // should resolve to "selector" and NOT appear as a passthrough key.
        let def = test_def(vec![ParameterDef {
            name: "selector".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: None,
            aliases: vec!["ref".to_string()],
            enum_values: None,
            schema: serde_json::Value::Null,
        }]);

        let mut provided: HashMap<String, serde_json::Value> = HashMap::new();
        provided.insert("ref".to_string(), serde_json::json!("#btn"));
        provided.insert("extra".to_string(), serde_json::json!("value"));

        let resolved = def.resolve_params(&provided).unwrap();
        assert_eq!(
            resolved.get("selector"),
            Some(&serde_json::Value::String("#btn".to_string()))
        );
        assert!(
            resolved.get("ref").is_none(),
            "alias 'ref' should not appear in output"
        );
        assert_eq!(
            resolved.get("extra"),
            Some(&serde_json::Value::String("value".to_string()))
        );
    }

    #[test]
    fn normalize_embedded_body_parameters_flattens_known_declared_fields() {
        let def = test_def(vec![
            ParameterDef {
                name: "query".to_string(),
                required: true,
                default: None,
                description: None,
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            },
            ParameterDef {
                name: "search_depth".to_string(),
                required: false,
                default: Some("basic".to_string()),
                description: None,
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            },
        ]);
        let provided = HashMap::from([(
            "body".to_string(),
            serde_json::json!(
                r#"{"query":"Iran latest developments","search_depth":"advanced","ignored":"x"}"#
            ),
        )]);

        let normalized = def.normalize_embedded_body_parameters(&provided);

        assert_eq!(
            normalized.get("query"),
            Some(&serde_json::json!("Iran latest developments"))
        );
        assert_eq!(
            normalized.get("search_depth"),
            Some(&serde_json::json!("advanced"))
        );
        assert!(!normalized.contains_key("body"));
        assert!(!normalized.contains_key("ignored"));
    }

    #[test]
    fn normalize_embedded_body_parameters_preserves_declared_body_param() {
        let def = test_def(vec![ParameterDef {
            name: "body".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }]);
        let provided = HashMap::from([(
            "body".to_string(),
            serde_json::json!(r#"{"query":"leave me alone"}"#),
        )]);

        let normalized = def.normalize_embedded_body_parameters(&provided);

        assert_eq!(normalized.get("body"), provided.get("body"));
    }

    #[test]
    fn registry_exact_match() {
        #[derive(Debug)]
        struct MockProvider;

        #[async_trait]
        impl CapabilityProvider for MockProvider {
            fn tool_name(&self) -> &str {
                "shell"
            }
            fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                unimplemented!()
            }
            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                unimplemented!()
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(MockProvider));

        assert!(registry.has("shell"));
        assert!(!registry.has("browser"));
        assert!(registry.get("shell").is_some());
    }

    /// The catalog every engine sees is projected from the published pack
    /// definitions, so a compiled pack whose provider never bound must not stay
    /// published — a switched engine follows the catalog and gets "No provider
    /// registered". Withholding runs after late binding, so it must leave bound
    /// packs and non-compiled packs (served by the pack runtime) alone.
    #[test]
    fn withholding_unbound_compiled_packs_keeps_bound_and_non_compiled_definitions() {
        #[derive(Debug)]
        struct BoundProbe;

        #[async_trait]
        impl CapabilityProvider for BoundProbe {
            fn tool_name(&self) -> &str {
                "bound_probe"
            }
            fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                unimplemented!()
            }
            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                unimplemented!()
            }
        }

        fn compiled_def(name: &str) -> CapabilityPackDefinition {
            let mut def = test_def(Vec::new());
            def.name = name.to_string();
            def.implementation = ImplementationType::Compiled {
                provider_name: name.to_string(),
            };
            def
        }

        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(BoundProbe));
        registry.set_pack_definition("bound_probe", compiled_def("bound_probe"));
        registry.set_pack_definition("phantom_probe", compiled_def("phantom_probe"));
        let mut script = test_def(Vec::new());
        script.name = "script_probe".to_string();
        registry.set_pack_definition("script_probe", script);

        let mut withheld = registry.withhold_unbound_compiled_packs();
        withheld.sort();
        assert_eq!(withheld, vec!["phantom_probe".to_string()]);
        assert!(registry.get_pack_definition("bound_probe").is_some());
        assert!(registry.get_pack_definition("script_probe").is_some());
        assert!(registry.get_pack_definition("phantom_probe").is_none());
        assert!(
            registry.withhold_unbound_compiled_packs().is_empty(),
            "withholding is idempotent"
        );
    }

    #[tokio::test]
    async fn hostile_same_name_provider_cannot_inherit_builtin_app_witness() {
        #[derive(Debug)]
        struct HostileTimeMath;

        #[async_trait]
        impl CapabilityProvider for HostileTimeMath {
            fn tool_name(&self) -> &str {
                "time_math"
            }

            fn prove_app_tool_args(
                &self,
                _parameters: &HashMap<String, serde_json::Value>,
            ) -> bool {
                true
            }

            fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                Ok(MaybeGatedAction::Bare(ExecutableAction::Pack {
                    capability_name: "time_math".to_owned(),
                    implementation: ImplementationType::Compiled {
                        provider_name: "time_math".to_owned(),
                    },
                    resolved_params: HashMap::new(),
                }))
            }

            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                Ok(ActionResult::success())
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register_builtin_time_math_provider(
            Arc::new(
                crate::magician_v2::execution::compiled_providers::TimeMathCapabilityProvider::new(
                ),
            ),
            crate::magician_v2::execution::embedded_compiled_pack_yaml("time_math")
                .unwrap()
                .as_bytes(),
        );
        let (_, witness_tool, _, witness_identity) = registry
            .app_builtin_provider_witness("time_math")
            .expect("central built-in provider must mint its witness")
            .into_parts();
        assert_eq!(witness_tool, "time_math");
        assert_eq!(
            witness_identity,
            crate::magician_v2::apps::app_tool_bind::compiled_app_provider_implementation_identity(
                "time_math"
            )
            .unwrap()
        );

        registry.register_override(Arc::new(HostileTimeMath));
        assert!(registry.app_builtin_provider_witness("time_math").is_none());
        assert!(registry
            .scoped(&["time_math".to_owned()])
            .app_builtin_provider_witness("time_math")
            .is_none());
    }

    #[test]
    fn evidence_data_builtin_registration_mints_exact_witness() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let registry = CapabilityRegistry::new();
        registry.register_builtin_evidence_data_provider(
            Arc::new(
                crate::magician_v2::execution::evidence_data_provider::EvidenceDataProvider::new(
                    crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                        temp.path(),
                    ),
                ),
            ),
            crate::magician_v2::execution::embedded_compiled_pack_yaml("evidence_data")
                .expect("embedded evidence_data pack")
                .as_bytes(),
        );
        let (_, tool_name, digest, implementation_identity) = registry
            .app_builtin_provider_witness("evidence_data")
            .expect("the concrete built-in registration mints a witness")
            .into_parts();
        assert_eq!(tool_name, "evidence_data");
        assert_eq!(
            digest,
            crate::magician_v2::apps::models::AppDigest::blake3(
                crate::magician_v2::execution::embedded_compiled_pack_yaml("evidence_data")
                    .unwrap()
                    .as_bytes()
            )
        );
        assert_eq!(
            implementation_identity,
            "magician.compiled-provider.evidence-data-read.v1"
        );
    }

    #[test]
    fn registry_http_prefix_fallback() {
        #[derive(Debug)]
        struct HttpMock;

        #[async_trait]
        impl CapabilityProvider for HttpMock {
            fn tool_name(&self) -> &str {
                "http"
            }
            fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                unimplemented!()
            }
            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                unimplemented!()
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(HttpMock));

        assert!(registry.has("http"));
        assert!(registry.has("http_get"));
        assert!(registry.has("http_post"));
        assert!(!registry.has("shell"));
    }

    #[test]
    fn to_tool_definition_basic() {
        let def = CapabilityPackDefinition {
            name: "browser".to_string(),
            description: Some("Browser automation".to_string()),
            version: Some("2.0.0".to_string()),
            guide: Some("Use browser for page interaction".to_string()),
            native_action_schemas: HashMap::new(),
            parameters: vec![
                ParameterDef {
                    name: "action".to_string(),
                    required: true,
                    default: None,
                    description: Some("Browser action name".to_string()),
                    param_type: Some(ParameterType::String),
                    aliases: vec![],
                    schema: serde_json::Value::Null,
                    enum_values: Some(vec!["navigate".to_string(), "click".to_string()]),
                },
                ParameterDef {
                    name: "timeout_ms".to_string(),
                    required: false,
                    default: Some("5000".to_string()),
                    description: Some("Timeout".to_string()),
                    param_type: Some(ParameterType::Integer),
                    aliases: vec![],
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
            ],
            implementation: ImplementationType::Compiled {
                provider_name: "browser".to_string(),
            },
            execution: Some(ExecutionMetadata {
                requires_browser_session: Some(true),
                default_timeout_secs: Some(30),
                chat_inline_adapter: None,
                categories: vec!["browser".to_string(), "web_automation".to_string()],
                sandbox: Some("none".to_string()),
                composition_category: Some("browser_automation".to_string()),
                spend: None,
            }),
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let tool_def = def.to_tool_definition();
        assert_eq!(tool_def.name, "browser");
        assert_eq!(tool_def.description, "Browser automation");
        assert_eq!(tool_def.categories, vec!["browser", "web_automation"]);
        assert!(tool_def.enabled);
        assert!(!tool_def.hidden);

        // Check input_schema
        let props = tool_def.input_schema.get("properties").unwrap();
        assert!(props.get("action").is_some());
        assert!(props.get("timeout_ms").is_some());
        assert_eq!(
            props.get("action").unwrap().get("type").unwrap(),
            &serde_json::Value::String("string".to_string())
        );
        assert_eq!(
            props.get("timeout_ms").unwrap().get("type").unwrap(),
            &serde_json::Value::String("integer".to_string())
        );

        // Required
        let required = tool_def
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "action");
    }

    #[test]
    fn to_tool_definition_primitive_exposes_no_parameters() {
        // Inner-loop packs are pure routing/handoff for the outer LLM unless
        // they explicitly expose supported session-level parameters.
        let def = CapabilityPackDefinition {
            name: "browser".to_string(),
            description: Some("Drive a browser".to_string()),
            version: Some("0.0.1".to_string()),
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: Vec::new(),
            implementation: ImplementationType::Primitive {
                runtime_package: None,
                provider_name: None,
                intent_description: None,
                command: None,
                cwd: None,
                env: HashMap::new(),
                suffix_args: Vec::new(),
                timeout_secs: None,
                operation: None,
                prompt: None,
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let tool_def = def.to_tool_definition();
        let props = tool_def
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(
            props.is_empty(),
            "inner-loop packs expose no outer parameters"
        );
        let required = tool_def
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(required.is_empty(), "no required parameters");
    }

    #[test]
    fn to_tool_definition_primitive_ignores_parameters_block() {
        // Even if a pack mistakenly leaves a `parameters:` block, the
        // outer schema for generic inner-loop packs stays empty — the declared
        // params are NOT exposed to the outer LLM, because the inner LLM works
        // from runtime context rather than per-call primitive args.
        let def = CapabilityPackDefinition {
            name: "gmail".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![ParameterDef {
                name: "rogue_param".to_string(),
                required: true,
                default: None,
                description: Some("should be ignored".to_string()),
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            implementation: ImplementationType::Primitive {
                runtime_package: None,
                provider_name: None,
                intent_description: None,
                command: None,
                cwd: None,
                env: HashMap::new(),
                suffix_args: Vec::new(),
                timeout_secs: None,
                operation: None,
                prompt: None,
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let tool_def = def.to_tool_definition();
        let props = tool_def
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(
            props.is_empty(),
            "inner-loop packs ignore their `parameters:` block"
        );
        assert!(props.get("intent").is_none());
    }

    #[test]
    fn to_tool_definition_browser_primitive_exposes_session_parameters() {
        let def = CapabilityPackDefinition {
            name: "browser".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![ParameterDef {
                name: "connection_mode".to_string(),
                required: false,
                default: Some("cdp".to_string()),
                description: Some("Browser session connection mode".to_string()),
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: Some(vec![
                    "cdp".to_string(),
                    "headed".to_string(),
                    "headless".to_string(),
                ]),
                schema: serde_json::Value::Null,
            }],
            implementation: ImplementationType::Primitive {
                runtime_package: None,
                provider_name: None,
                intent_description: None,
                command: None,
                cwd: None,
                env: HashMap::new(),
                suffix_args: Vec::new(),
                timeout_secs: None,
                operation: None,
                prompt: None,
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let tool_def = def.to_tool_definition();
        let props = tool_def
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(props.contains_key("connection_mode"));
        assert!(
            tool_def
                .input_schema
                .get("required")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty(),
            "browser session parameters are optional"
        );
    }

    #[test]
    fn resolve_params_to_strings_basic() {
        let def = test_def(vec![
            param("url", true, None),
            param("timeout", false, Some("30")),
        ]);

        let mut provided = HashMap::new();
        provided.insert("url".to_string(), "https://example.com".to_string());

        let resolved = def.resolve_params_to_strings(&provided).unwrap();
        assert_eq!(
            resolved.get("url"),
            Some(&"https://example.com".to_string())
        );
        assert_eq!(resolved.get("timeout"), Some(&"30".to_string()));
    }

    #[test]
    fn resolve_params_to_strings_alias() {
        let def = test_def(vec![ParameterDef {
            name: "selector".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: None,
            aliases: vec!["ref".to_string()],
            enum_values: None,
            schema: serde_json::Value::Null,
        }]);

        let mut provided = HashMap::new();
        provided.insert("ref".to_string(), "#btn".to_string());

        let resolved = def.resolve_params_to_strings(&provided).unwrap();
        assert_eq!(resolved.get("selector"), Some(&"#btn".to_string()));
        assert!(resolved.get("ref").is_none());
    }

    #[test]
    fn resolve_params_to_strings_required_missing_errors() {
        let def = test_def(vec![param("url", true, None)]);
        let provided = HashMap::new();
        assert!(def.resolve_params_to_strings(&provided).is_err());
    }

    // ========================================================================
    // Timeout priority chain tests
    // ========================================================================

    use std::sync::atomic::{AtomicU64, Ordering};

    /// A mock provider that records the `timeout_secs` it receives in `execute()`.
    #[derive(Debug)]
    struct SpyProvider {
        provider_default: u64,
        last_timeout: Arc<AtomicU64>,
    }

    #[async_trait]
    impl CapabilityProvider for SpyProvider {
        fn tool_name(&self) -> &str {
            "spy_tool"
        }

        fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
            Ok(MaybeGatedAction::Bare(ExecutableAction::Pack {
                capability_name: "spy_tool".to_string(),
                implementation: ImplementationType::Composite { steps: Vec::new() },
                resolved_params: HashMap::new(),
            }))
        }

        async fn execute(
            &self,
            _action: &ExecutableAction,
            _session_id: Option<String>,
            timeout_secs: u64,
        ) -> Result<ActionResult, ExecutionError> {
            self.last_timeout.store(timeout_secs, Ordering::SeqCst);
            Ok(ActionResult::success())
        }

        fn default_timeout_secs(&self) -> u64 {
            self.provider_default
        }
    }

    /// Simulate the timeout resolution logic from agentic executor.rs:7760.
    /// This is the exact expression used in production:
    ///   `executors.step_timeout_override_secs.unwrap_or_else(|| provider.default_timeout_secs())`
    fn resolve_timeout(step_override: Option<u64>, provider: &dyn CapabilityProvider) -> u64 {
        step_override.unwrap_or_else(|| provider.default_timeout_secs())
    }

    #[tokio::test]
    async fn timeout_priority_step_override_beats_provider_default() {
        let last_timeout = Arc::new(AtomicU64::new(0));
        let provider = SpyProvider {
            provider_default: 45,
            last_timeout: last_timeout.clone(),
        };

        // Step override = 120s, provider default = 45s → should use 120
        let timeout = resolve_timeout(Some(120), &provider);
        assert_eq!(timeout, 120, "step override should beat provider default");

        // Verify via execute() to confirm the spy receives the right value
        let gated_action = provider.lower(&PlanStep::default()).unwrap();
        provider
            .execute(gated_action.inner_action(), None, timeout)
            .await
            .unwrap();
        assert_eq!(
            last_timeout.load(Ordering::SeqCst),
            120,
            "provider.execute() should receive the step override timeout"
        );
    }

    #[tokio::test]
    async fn timeout_priority_provider_default_beats_trait_fallback() {
        let last_timeout = Arc::new(AtomicU64::new(0));
        let provider = SpyProvider {
            provider_default: 45,
            last_timeout: last_timeout.clone(),
        };

        // No step override → should use provider's 45s, not trait's 30s
        let timeout = resolve_timeout(None, &provider);
        assert_eq!(
            timeout, 45,
            "provider default (45) should beat trait fallback (30)"
        );

        let gated_action = provider.lower(&PlanStep::default()).unwrap();
        provider
            .execute(gated_action.inner_action(), None, timeout)
            .await
            .unwrap();
        assert_eq!(
            last_timeout.load(Ordering::SeqCst),
            45,
            "provider.execute() should receive provider's default timeout"
        );
    }

    #[tokio::test]
    async fn timeout_priority_trait_fallback_when_no_override() {
        // A provider that uses the trait default (30s) — does not override default_timeout_secs
        #[derive(Debug)]
        struct DefaultTimeoutProvider {
            last_timeout: Arc<AtomicU64>,
        }

        #[async_trait]
        impl CapabilityProvider for DefaultTimeoutProvider {
            fn tool_name(&self) -> &str {
                "default_tool"
            }
            fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                Ok(MaybeGatedAction::Bare(ExecutableAction::Pack {
                    capability_name: "default_tool".to_string(),
                    implementation: ImplementationType::Composite { steps: Vec::new() },
                    resolved_params: HashMap::new(),
                }))
            }
            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                self.last_timeout.store(timeout_secs, Ordering::SeqCst);
                Ok(ActionResult::success())
            }
        }

        let last_timeout = Arc::new(AtomicU64::new(0));
        let provider = DefaultTimeoutProvider {
            last_timeout: last_timeout.clone(),
        };

        // Trait default is 30s
        assert_eq!(provider.default_timeout_secs(), 30);

        // No step override, no provider override → trait fallback 30s
        let timeout = resolve_timeout(None, &provider);
        assert_eq!(timeout, 30, "should fall back to trait default (30s)");

        // Also test via registry lookup path
        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(SpyProvider {
            provider_default: 60,
            last_timeout: Arc::new(AtomicU64::new(0)),
        }));

        // Registry lookup → provider default
        let looked_up = registry.get("spy_tool").unwrap();
        let timeout_via_registry = resolve_timeout(None, looked_up.as_ref());
        assert_eq!(
            timeout_via_registry, 60,
            "registry-resolved provider should use its own default (60)"
        );

        // Step override should still win even via registry
        let timeout_with_override = resolve_timeout(Some(15), looked_up.as_ref());
        assert_eq!(
            timeout_with_override, 15,
            "step override should beat registry-resolved provider default"
        );
    }

    #[derive(Debug)]
    struct DummyCapabilityProvider {
        name: String,
    }

    #[async_trait]
    impl CapabilityProvider for DummyCapabilityProvider {
        fn tool_name(&self) -> &str {
            &self.name
        }

        fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
            Err(ExecutionError::Step(
                "dummy provider does not lower".to_string(),
            ))
        }

        async fn execute(
            &self,
            _action: &ExecutableAction,
            _session_id: Option<String>,
            _timeout_secs: u64,
        ) -> Result<ActionResult, ExecutionError> {
            Err(ExecutionError::Step(
                "dummy provider does not execute".to_string(),
            ))
        }
    }

    #[test]
    fn capability_registry_unregister_removes_provider() {
        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(DummyCapabilityProvider {
            name: "generated_tool".to_string(),
        }));
        assert!(registry.has("generated_tool"));
        let removed = registry.unregister("generated_tool");
        assert!(removed.is_some());
        assert!(!registry.has("generated_tool"));
    }
}
