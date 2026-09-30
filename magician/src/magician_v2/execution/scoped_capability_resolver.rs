use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use runtime_core::{FileSandboxConfig, ShellSandboxConfig};
use serde::Serialize;
use tokio::sync::RwLock;

use super::agent_resources::AgentResources;
use super::compiled_providers::{CompiledHandlerRegistry, SkillLoadFailure};
use super::harness_provider::{
    register_harness_action_providers_if_absent, register_harness_read_providers_if_absent,
};
use super::internal_data_provider::{InternalDataProvider, INTERNAL_DATA_TOOL_NAME};
use super::thinking_maps_data_provider::{ThinkingMapsDataProvider, THINKING_MAPS_DATA_TOOL_NAME};
use super::{
    build_compiled_registry, embedded_compiled_pack_defs_ref, embedded_compiled_pack_yaml,
    prune_runtime_disabled_pack_defs, prune_unexecutable_pack_defs, AgentRosterDataProvider,
    CapabilityRegistry, EvidenceDataProvider, MagicutorClient, MeetingsDataProvider,
    MemoryDataProvider, NotesDataProvider, TasksDataProvider, AGENT_ROSTER_DATA_TOOL_NAME,
    EVIDENCE_DATA_TOOL_NAME, MEETINGS_DATA_TOOL_NAME, MEMORY_DATA_TOOL_NAME, NOTES_DATA_TOOL_NAME,
    TASKS_DATA_TOOL_NAME,
};
use crate::magician_v2::apps::capability_catalog::{
    authorize_discovered_computed_capability, AppComputedCapabilityOverlayCache,
    AppComputedCapabilityScopeOverlay,
};
use crate::magician_v2::artifact_v2::capabilities::hash_skill_catalog;
use crate::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use crate::magician_v2::harness::HarnessServices;
use crate::magician_v2::resource_authority::{ledger::ResourceLedger, token_store::TokenStore};
use crate::magician_v2::secrets::{ScopedSecretBroker, SecretBroker, SecretStoreResolver};

const DEFAULT_SCOPED_CAPABILITY_CACHE_LIMIT: usize = 64;
const DEFAULT_SCOPED_CAPABILITY_IDLE_TTL_SECONDS: u64 = 1_800;
const SCOPED_CAPABILITY_BACKGROUND_REVISION_INTERVAL_SECONDS: u64 = 30;
const SCOPED_CAPABILITY_REVISION_VERSION: &str = "scoped-capability-revision.v1";

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ScopedCapabilityCacheKey {
    principal: String,
    workspace: String,
}

impl ScopedCapabilityCacheKey {
    fn new(principal: &str, workspace: &str) -> Self {
        Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        }
    }
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// One immutable, revision-bound view of a scope's executable capabilities.
///
/// The registry and tool index are built from the exact same pack definitions,
/// so Chat, realtime voice, and autonomous tasks cannot observe different
/// family membership for an otherwise identical scope revision.
#[derive(Clone)]
pub struct ScopedCapabilitySnapshot {
    pub revision: String,
    pub overlay_revision: String,
    pub registry: Arc<CapabilityRegistry>,
    pub tool_index: Arc<crate::magician_v2::execution::flat_loop::ToolIndex>,
    /// The same packs as `tool_index`, projected for a surface that dispatches
    /// whole packs (Chat, realtime voice): a multi-primitive pack is one
    /// pack-named tool here, not its leaves. See
    /// [`crate::magician_v2::execution::flat_loop::build_surface_tool_index`].
    pub surface_tool_index: Arc<crate::magician_v2::execution::flat_loop::ToolIndex>,
    pub pack_count: usize,
    pub built_at_ms: u64,
    /// Skills (by directory name) that produced a pack in this build. The
    /// background revision check refuses to replace this snapshot with one
    /// that newly fails any of them.
    pub loaded_skills: Arc<BTreeSet<String>>,
    /// Skills the loader could not turn into a capability in this build.
    pub load_failures: Arc<Vec<SkillLoadFailure>>,
    /// Tools in `registry` contributed by an installed app's computed
    /// capability overlay rather than by first-party packs, mapped to the
    /// installation that owns each one.
    ///
    /// App authority is a property of the dispatched resource, not of the
    /// caller: these tools carry an installation's grant (network policy,
    /// resource ceiling, disclosure permit), which only the app-workflow path
    /// applies. A dispatcher that reaches one outside that path must resolve
    /// that installation's grant itself or fail closed — the installation id is
    /// carried here because it is the key that makes resolving it possible.
    pub app_origin_tool_installations: Arc<
        std::collections::BTreeMap<String, crate::magician_v2::apps::models::AppInstallationId>,
    >,
}

impl ScopedCapabilitySnapshot {
    /// The installation that owns `tool_name`, when it is app-authored.
    /// `None` means first-party — dispatch it normally.
    pub fn app_origin_installation(
        &self,
        tool_name: &str,
    ) -> Option<&crate::magician_v2::apps::models::AppInstallationId> {
        self.app_origin_tool_installations.get(tool_name)
    }
}

impl std::fmt::Debug for ScopedCapabilitySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedCapabilitySnapshot")
            .field("revision", &self.revision)
            .field("overlay_revision", &self.overlay_revision)
            .field("pack_count", &self.pack_count)
            .field("tool_index_len", &self.tool_index.len())
            .field("built_at_ms", &self.built_at_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct ScopedCapabilityCacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    builds: AtomicU64,
    invalidations: AtomicU64,
    revision_checks: AtomicU64,
    revision_failures: AtomicU64,
    coalesced_waiters: AtomicU64,
    /// Snapshots the background check rebuilt, validated and swapped in place.
    swaps: AtomicU64,
    /// Distinct revisions the background check refused to swap in because
    /// they newly failed a skill the served snapshot carried.
    held_back_revisions: AtomicU64,
}

#[derive(Debug)]
struct ScopedCapabilityCacheEntry {
    snapshot: Arc<ScopedCapabilitySnapshot>,
    last_access_epoch_seconds: AtomicU64,
    last_revision_check_epoch_seconds: AtomicU64,
    revision_check_in_flight: AtomicBool,
    /// The on-disk revision the check refused to swap in, and why. Cleared
    /// when a later revision validates.
    held_back: StdMutex<Option<HeldBackRevision>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldBackRevision {
    observed_revision: String,
    failures: Vec<SkillLoadFailure>,
}

/// A scope whose on-disk skills no longer build the catalog it is serving:
/// the served snapshot stays until the skills load again. Operator-facing;
/// carries skill names and loader reasons, never file contents.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HeldBackScopeStatus {
    pub principal: String,
    pub workspace: String,
    pub served_revision: String,
    pub observed_revision: String,
    pub failures: Vec<HeldBackSkillStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HeldBackSkillStatus {
    pub skill: String,
    pub reason: String,
}

/// Operator-safe cache summary. It intentionally contains only revisions and
/// counts; provider schemas, credentials, and private runtime state remain
/// server-side.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ScopedCapabilityCacheStatus {
    pub entry_count: usize,
    pub hits: u64,
    pub misses: u64,
    pub builds: u64,
    pub invalidations: u64,
    pub revision_checks: u64,
    pub revision_failures: u64,
    pub coalesced_waiters: u64,
    pub swaps: u64,
    pub held_back_revisions: u64,
    /// Scopes currently serving a snapshot their on-disk skills no longer
    /// build, with the skills that fail and why.
    pub held_back_scopes: Vec<HeldBackScopeStatus>,
}

#[derive(Clone)]
pub struct ScopedCapabilityResolver {
    workspace_manager: Arc<CapabilityWorkspaceManager>,
    magicutor_client: Arc<MagicutorClient>,
    file_sandbox: FileSandboxConfig,
    shell_sandbox: ShellSandboxConfig,
    secret_store_resolver: Option<Arc<SecretStoreResolver>>,
    resource_ledger: Option<Arc<RwLock<ResourceLedger>>>,
    token_store: Option<Arc<RwLock<TokenStore>>>,
    /// Phase 0.8c — agent-level resource bundle for compiled-pack
    /// providers. Late-bound from `bin/magician.rs` at boot (same
    /// pattern as `agent_backend` above), wrapped in interior
    /// mutability so the resolver `Clone`s share the slot. Compiled
    /// providers that have migrated off `AgentBackend` hold an
    /// `Arc<AgentResources>` clone to access memory/definition/
    /// workspace resources directly.
    agent_resources: Arc<StdRwLock<Option<Arc<AgentResources>>>>,

    /// Phase 0.8c — handler registry for compiled tools migrated to
    /// the `GenericCompiledProvider` pattern. Populated at boot from
    /// `default_compiled_handler_registry()`; per-scope
    /// `registry_for_scope()` calls pass it through to
    /// `build_compiled_registry` which uses it to route migrated
    /// tools to `GenericCompiledProvider` instead of the legacy
    /// per-tool struct.
    compiled_handlers: Arc<StdRwLock<Option<Arc<CompiledHandlerRegistry>>>>,

    /// Harness READ services for binding harness introspection providers
    /// (`list_episodes`, `list_proposals`, `read_trace`,
    /// `read_program_state`, `system_status`, `inspect_agent`,
    /// `evaluate_harness`, `magician_work_ledger`) onto each per-scope
    /// registry. These tools are auto-granted to harness agents via
    /// `HARNESS_TOOL_NAMES`, but their providers are `HarnessCapabilityProvider`
    /// (needing `HarnessServices`) — which `build_compiled_registry` does not
    /// carry. Without this the pack-defs exist in-scope but have no provider,
    /// so dispatch returns "is not a compiled pack". Late-bound at boot from
    /// the same `HarnessServices` that binds the base registry (see
    /// `set_harness_services` / `bin/magician.rs`), wrapped in interior
    /// mutability so cloned resolvers share the slot.
    harness_services: Arc<StdRwLock<Option<HarnessServices>>>,

    /// Revision-bound immutable registries and tool indexes. Canonical
    /// skill/config mutation paths evict the affected entry; cold builds hash
    /// the exact scope/extras source bytes and validate that revision again
    /// before publication. Warm reads are intentionally O(1): they never
    /// materialize the scope or walk/hash the skill tree merely to prove that a
    /// cached immutable snapshot is still itself. A coalesced background
    /// revision check provides eventual detection for out-of-band edits without
    /// putting filesystem traversal back on bootstrap's critical path.
    cache: Arc<StdRwLock<HashMap<ScopedCapabilityCacheKey, Arc<ScopedCapabilityCacheEntry>>>>,
    cache_build_locks: Arc<StdMutex<HashMap<ScopedCapabilityCacheKey, Arc<StdMutex<()>>>>>,
    cache_metrics: Arc<ScopedCapabilityCacheMetrics>,
    cache_limit: Arc<AtomicUsize>,
    cache_idle_ttl_seconds: Arc<AtomicU64>,
    cache_singleflight: Arc<AtomicBool>,
    computed_capabilities: Arc<StdRwLock<Option<Arc<AppComputedCapabilityOverlayCache>>>>,
}

impl std::fmt::Debug for ScopedCapabilityResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedCapabilityResolver")
            .field("workspace_manager", &self.workspace_manager)
            .field("file_sandbox", &self.file_sandbox)
            .field("shell_sandbox", &self.shell_sandbox)
            .finish()
    }
}

impl ScopedCapabilityResolver {
    pub fn new(
        workspace_manager: Arc<CapabilityWorkspaceManager>,
        magicutor_client: Arc<MagicutorClient>,
        file_sandbox: FileSandboxConfig,
        shell_sandbox: ShellSandboxConfig,
        secret_store_resolver: Option<Arc<SecretStoreResolver>>,
        resource_ledger: Option<Arc<RwLock<ResourceLedger>>>,
        token_store: Option<Arc<RwLock<TokenStore>>>,
    ) -> Self {
        Self {
            workspace_manager,
            magicutor_client,
            file_sandbox,
            shell_sandbox,
            secret_store_resolver,
            resource_ledger,
            token_store,
            agent_resources: Arc::new(StdRwLock::new(None)),
            compiled_handlers: Arc::new(StdRwLock::new(None)),
            harness_services: Arc::new(StdRwLock::new(None)),
            cache: Arc::new(StdRwLock::new(HashMap::new())),
            cache_build_locks: Arc::new(StdMutex::new(HashMap::new())),
            cache_metrics: Arc::new(ScopedCapabilityCacheMetrics::default()),
            cache_limit: Arc::new(AtomicUsize::new(DEFAULT_SCOPED_CAPABILITY_CACHE_LIMIT)),
            cache_idle_ttl_seconds: Arc::new(AtomicU64::new(
                DEFAULT_SCOPED_CAPABILITY_IDLE_TTL_SECONDS,
            )),
            cache_singleflight: Arc::new(AtomicBool::new(true)),
            computed_capabilities: Arc::new(StdRwLock::new(None)),
        }
    }

    pub fn set_computed_capability_overlay(&self, overlay: Arc<AppComputedCapabilityOverlayCache>) {
        *self
            .computed_capabilities
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(overlay);
        self.clear_cache();
    }

    fn computed_capability_overlay(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Arc<AppComputedCapabilityScopeOverlay> {
        self.computed_capabilities
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|cache| cache.get(principal, workspace))
            .unwrap_or_else(|| Arc::new(AppComputedCapabilityScopeOverlay::empty()))
    }

    /// Apply the process-owned cache policy from `agent_surface_runtime`.
    /// Clones share these atomics and cache maps, so startup and live config
    /// reloads update Chat, voice, and autonomous callers together.
    pub fn configure_cache(&self, max_entries: usize, idle_ttl_seconds: u64, singleflight: bool) {
        let max_entries = max_entries.max(1);
        let idle_ttl_seconds = idle_ttl_seconds.max(1);
        let limit_changed = self.cache_limit.swap(max_entries, Ordering::AcqRel) != max_entries;
        let ttl_changed = self
            .cache_idle_ttl_seconds
            .swap(idle_ttl_seconds, Ordering::AcqRel)
            != idle_ttl_seconds;
        let singleflight_changed =
            self.cache_singleflight.swap(singleflight, Ordering::AcqRel) != singleflight;
        let changed = limit_changed || ttl_changed || singleflight_changed;
        if changed {
            self.clear_cache();
            self.cache_build_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        }
    }

    /// Phase 0.8c — install the agent-resources bundle. Called from
    /// `bin/magician.rs` at boot after all dependent services are
    /// constructed. Subsequent `registry_for_scope` calls pass the
    /// resources to compiled-pack provider constructors that have
    /// migrated off `AgentBackend`.
    pub fn set_agent_resources(&self, resources: Arc<AgentResources>) {
        *self.agent_resources.write().unwrap() = Some(resources);
        self.clear_cache();
    }

    /// Snapshot the current agent-resources bundle for handing to
    /// compiled-pack providers. Returns `None` while `set_agent_resources`
    /// hasn't fired (boot races).
    pub fn agent_resources(&self) -> Option<Arc<AgentResources>> {
        self.agent_resources.read().unwrap().clone()
    }

    /// Phase 0.8c — install the compiled-handler registry. Called from
    /// `bin/magician.rs` at boot with the result of
    /// `default_compiled_handler_registry()`. Subsequent
    /// `registry_for_scope` calls route migrated tools through
    /// `GenericCompiledProvider`.
    pub fn set_compiled_handlers(&self, handlers: Arc<CompiledHandlerRegistry>) {
        *self.compiled_handlers.write().unwrap() = Some(handlers);
        self.clear_cache();
    }

    /// Snapshot the current handler registry. Returns `None` while
    /// `set_compiled_handlers` hasn't fired.
    pub fn compiled_handlers(&self) -> Option<Arc<CompiledHandlerRegistry>> {
        self.compiled_handlers.read().unwrap().clone()
    }

    /// Install the harness READ services used to bind harness introspection
    /// providers onto every per-scope registry. Called from `bin/magician.rs`
    /// at boot with the SAME `HarnessServices` that binds the base registry via
    /// `register_harness_read_providers`. When present, `registry_for_scope`
    /// re-binds the harness read providers whose pack-defs exist in the scope
    /// (guarded on pack-def presence, so it only wires tools the scope actually
    /// embeds).
    pub fn set_harness_services(&self, services: HarnessServices) {
        *self.harness_services.write().unwrap() = Some(services);
        self.clear_cache();
    }

    /// Snapshot the current harness services. Returns `None` while
    /// `set_harness_services` hasn't fired (boot races) or when the resolver
    /// was built without harness support.
    pub fn harness_services(&self) -> Option<HarnessServices> {
        self.harness_services.read().unwrap().clone()
    }

    /// Return the complete immutable capability snapshot for a scope. Repeated
    /// warm calls return the same Arcs in O(1). Canonical mutations evict the
    /// entry; out-of-band edits are detected by coalesced background revision
    /// checks and can also be forced through explicit refresh.
    pub fn capability_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Arc<ScopedCapabilitySnapshot>> {
        let key = ScopedCapabilityCacheKey::new(principal, workspace);
        if let Some(snapshot) = self.cached_snapshot(&key) {
            self.cache_metrics.hits.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                principal,
                workspace,
                revision = %snapshot.revision,
                pack_count = snapshot.pack_count,
                tool_index_count = snapshot.tool_index.len(),
                "scoped capability cache hit"
            );
            return Ok(snapshot);
        }
        self.cache_metrics.misses.fetch_add(1, Ordering::Relaxed);

        self.workspace_manager
            .materialize_scope(principal, workspace)?;

        let build_lock = self.cache_singleflight.load(Ordering::Acquire).then(|| {
            let mut locks = self
                .cache_build_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let cache_limit = self.cache_limit.load(Ordering::Acquire).max(1);
            if locks.len() >= cache_limit && !locks.contains_key(&key) {
                let cached_keys = self
                    .cache
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .keys()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>();
                locks.retain(|lock_key, lock| {
                    cached_keys.contains(lock_key) || Arc::strong_count(lock) > 1
                });
            }
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(StdMutex::new(())))
                .clone()
        });
        let _build_guard = build_lock
            .as_ref()
            .map(|build_lock| match build_lock.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.cache_metrics
                        .coalesced_waiters
                        .fetch_add(1, Ordering::Relaxed);
                    build_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                },
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            });

        // Another waiter may have completed the build while this caller was
        // waiting. This check must remain O(1); the builder already validated
        // the revision before publishing the immutable snapshot.
        if let Some(snapshot) = self.cached_snapshot(&key) {
            self.cache_metrics.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(snapshot);
        }

        let mut current_revision = match self.scope_revision(principal, workspace) {
            Ok(revision) => revision,
            Err(error) => {
                self.cache_metrics
                    .revision_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "scoped capability revision failed; rebuilding without cache"
                );
                return self.build_snapshot_for_scope(
                    principal,
                    workspace,
                    Some(format!(
                        "uncached:{}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    )),
                );
            },
        };

        // Validate the source revision again after construction so a skill
        // mutation racing this cold YAML/provider build can never be published
        // under the wrong revision. A small bounded retry absorbs normal
        // atomic-renames; persistent churn fails closed rather than caching an
        // incoherent registry/index pair.
        let mut coherent_snapshot = None;
        for _ in 0..3 {
            let candidate = self.build_snapshot_for_scope(
                principal,
                workspace,
                Some(current_revision.clone()),
            )?;
            let observed_revision = self.scope_revision(principal, workspace)?;
            if observed_revision == current_revision {
                coherent_snapshot = Some(candidate);
                break;
            }
            self.cache_metrics
                .invalidations
                .fetch_add(1, Ordering::Relaxed);
            current_revision = observed_revision;
        }
        let snapshot = coherent_snapshot.ok_or_else(|| {
            anyhow::anyhow!(
                "scoped capability sources changed repeatedly while building {principal}/{workspace}"
            )
        })?;
        {
            let mut cache = self
                .cache
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let cache_limit = self.cache_limit.load(Ordering::Acquire).max(1);
            if cache.len() >= cache_limit && !cache.contains_key(&key) {
                cache.clear();
                self.cache_metrics
                    .invalidations
                    .fetch_add(1, Ordering::Relaxed);
            }
            cache.insert(
                key,
                Arc::new(ScopedCapabilityCacheEntry {
                    snapshot: Arc::clone(&snapshot),
                    last_access_epoch_seconds: AtomicU64::new(now_epoch_seconds()),
                    last_revision_check_epoch_seconds: AtomicU64::new(now_epoch_seconds()),
                    revision_check_in_flight: AtomicBool::new(false),
                    held_back: StdMutex::new(None),
                }),
            );
        }
        Ok(snapshot)
    }

    pub fn registry_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Arc<CapabilityRegistry>> {
        Ok(self
            .capability_snapshot_for_scope(principal, workspace)?
            .registry
            .clone())
    }

    /// Explicitly evict one scope. Canonical skill/config mutation paths call
    /// this eagerly; the content revision still protects out-of-band edits.
    pub fn invalidate_scope(&self, principal: &str, workspace: &str) -> bool {
        // The parsed catalog behind this snapshot is invalid for the same
        // reason the snapshot is.
        self.workspace_manager
            .invalidate_scope_pack_defs(principal, workspace);
        let removed = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&ScopedCapabilityCacheKey::new(principal, workspace))
            .is_some();
        if removed {
            self.cache_metrics
                .invalidations
                .fetch_add(1, Ordering::Relaxed);
        }
        removed
    }

    pub fn clear_cache(&self) -> usize {
        self.workspace_manager.clear_scope_pack_cache();
        let mut cache = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed = cache.len();
        cache.clear();
        drop(cache);
        if removed > 0 {
            self.cache_metrics
                .invalidations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn cache_status(&self) -> ScopedCapabilityCacheStatus {
        let entry_count = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        ScopedCapabilityCacheStatus {
            entry_count,
            hits: self.cache_metrics.hits.load(Ordering::Relaxed),
            misses: self.cache_metrics.misses.load(Ordering::Relaxed),
            builds: self.cache_metrics.builds.load(Ordering::Relaxed),
            invalidations: self.cache_metrics.invalidations.load(Ordering::Relaxed),
            revision_checks: self.cache_metrics.revision_checks.load(Ordering::Relaxed),
            revision_failures: self.cache_metrics.revision_failures.load(Ordering::Relaxed),
            coalesced_waiters: self.cache_metrics.coalesced_waiters.load(Ordering::Relaxed),
            swaps: self.cache_metrics.swaps.load(Ordering::Relaxed),
            held_back_revisions: self
                .cache_metrics
                .held_back_revisions
                .load(Ordering::Relaxed),
            held_back_scopes: self.held_back_scopes(),
        }
    }

    fn held_back_scopes(&self) -> Vec<HeldBackScopeStatus> {
        let cache = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut scopes: Vec<HeldBackScopeStatus> = cache
            .iter()
            .filter_map(|(key, entry)| {
                let held = entry
                    .held_back
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()?;
                Some(HeldBackScopeStatus {
                    principal: key.principal.clone(),
                    workspace: key.workspace.clone(),
                    served_revision: entry.snapshot.revision.clone(),
                    observed_revision: held.observed_revision,
                    failures: held
                        .failures
                        .iter()
                        .map(|failure| HeldBackSkillStatus {
                            skill: failure.skill.clone(),
                            reason: failure.reason.clone(),
                        })
                        .collect(),
                })
            })
            .collect();
        scopes.sort_by(|a, b| (&a.principal, &a.workspace).cmp(&(&b.principal, &b.workspace)));
        scopes
    }

    pub fn refresh_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Arc<ScopedCapabilitySnapshot>> {
        self.invalidate_scope(principal, workspace);
        self.capability_snapshot_for_scope(principal, workspace)
    }

    fn cached_snapshot(
        &self,
        key: &ScopedCapabilityCacheKey,
    ) -> Option<Arc<ScopedCapabilitySnapshot>> {
        let entry = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()?;
        let now = now_epoch_seconds();
        let last_access = entry.last_access_epoch_seconds.load(Ordering::Acquire);
        let idle_ttl = self.cache_idle_ttl_seconds.load(Ordering::Acquire).max(1);
        if now.saturating_sub(last_access) > idle_ttl {
            let removed = self
                .cache
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key)
                .is_some();
            if removed {
                self.cache_metrics
                    .invalidations
                    .fetch_add(1, Ordering::Relaxed);
            }
            return None;
        }
        entry
            .last_access_epoch_seconds
            .store(now, Ordering::Release);
        let live_overlay = self.computed_capability_overlay(&key.principal, &key.workspace);
        if live_overlay.revision != entry.snapshot.overlay_revision {
            drop(entry);
            let removed = self
                .cache
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key)
                .is_some();
            if removed {
                self.cache_metrics
                    .invalidations
                    .fetch_add(1, Ordering::Relaxed);
            }
            return None;
        }
        self.schedule_background_revision_check(key, &entry, now);
        Some(Arc::clone(&entry.snapshot))
    }

    fn schedule_background_revision_check(
        &self,
        key: &ScopedCapabilityCacheKey,
        entry: &Arc<ScopedCapabilityCacheEntry>,
        now: u64,
    ) {
        let last_check = entry
            .last_revision_check_epoch_seconds
            .load(Ordering::Acquire);
        if now.saturating_sub(last_check) < SCOPED_CAPABILITY_BACKGROUND_REVISION_INTERVAL_SECONDS
            || entry
                .revision_check_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }

        // Set the cooldown before spawning. A failed check may retry on a later
        // request, but cannot create a new thread/task on every hot lookup.
        entry
            .last_revision_check_epoch_seconds
            .store(now, Ordering::Release);
        let resolver = self.clone();
        let key = key.clone();
        let entry = Arc::clone(entry);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let _revision_check = runtime.spawn_blocking(move || {
                resolver.validate_cached_revision(&key, &entry);
            });
        } else {
            let _revision_check = std::thread::spawn(move || {
                resolver.validate_cached_revision(&key, &entry);
            });
        }
    }

    /// The background check, validate-before-swap. When the on-disk revision
    /// differs from the served one, the candidate catalog is built here, in
    /// the background, and replaces the served snapshot only if it does not
    /// newly fail a skill the served snapshot carried. A half-saved
    /// `SKILL.md`, a mid-checkout tree, or a field this binary does not know
    /// used to drop that skill from the catalog within a check of the next
    /// use, silently; now the served snapshot stays, the hold is on the
    /// cache status with the skills and reasons, and the next revision that
    /// loads swaps in. Removing a skill is a change, not a failure, and a
    /// newly installed skill that is broken never served anything, so
    /// neither holds the catalog back.
    fn validate_cached_revision(
        &self,
        key: &ScopedCapabilityCacheKey,
        expected_entry: &Arc<ScopedCapabilityCacheEntry>,
    ) {
        let observed_revision = self.scope_revision(&key.principal, &key.workspace);
        match observed_revision {
            Ok(observed_revision) if observed_revision != expected_entry.snapshot.revision => {
                let candidate = match self.build_snapshot_for_scope(
                    &key.principal,
                    &key.workspace,
                    Some(observed_revision.clone()),
                ) {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        self.cache_metrics
                            .revision_failures
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            principal = %key.principal,
                            workspace = %key.workspace,
                            observed_revision = %observed_revision,
                            error = %error,
                            "background revision check could not build the candidate catalog; \
                             the served snapshot stays"
                        );
                        expected_entry
                            .revision_check_in_flight
                            .store(false, Ordering::Release);
                        return;
                    },
                };
                let newly_failed = newly_failed_served_skills(&expected_entry.snapshot, &candidate);
                if !newly_failed.is_empty() {
                    let mut held = expected_entry
                        .held_back
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let already_held = held
                        .as_ref()
                        .is_some_and(|held| held.observed_revision == observed_revision);
                    if !already_held {
                        self.cache_metrics
                            .held_back_revisions
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            principal = %key.principal,
                            workspace = %key.workspace,
                            served_revision = %expected_entry.snapshot.revision,
                            observed_revision = %observed_revision,
                            skills = ?newly_failed.iter().map(|f| f.skill.as_str()).collect::<Vec<_>>(),
                            reasons = ?newly_failed.iter().map(|f| f.reason.as_str()).collect::<Vec<_>>(),
                            "[CAPABILITY] skills on disk no longer build the served catalog; holding the \
                             served snapshot until they load again"
                        );
                        *held = Some(HeldBackRevision {
                            observed_revision,
                            failures: newly_failed,
                        });
                    }
                    expected_entry
                        .revision_check_in_flight
                        .store(false, Ordering::Release);
                    return;
                }
                let swapped = {
                    let mut cache = self
                        .cache
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let is_same_entry = cache
                        .get(key)
                        .is_some_and(|current| Arc::ptr_eq(current, expected_entry));
                    if is_same_entry {
                        cache.insert(
                            key.clone(),
                            Arc::new(ScopedCapabilityCacheEntry {
                                snapshot: Arc::clone(&candidate),
                                last_access_epoch_seconds: AtomicU64::new(
                                    expected_entry
                                        .last_access_epoch_seconds
                                        .load(Ordering::Acquire),
                                ),
                                last_revision_check_epoch_seconds: AtomicU64::new(
                                    now_epoch_seconds(),
                                ),
                                revision_check_in_flight: AtomicBool::new(false),
                                held_back: StdMutex::new(None),
                            }),
                        );
                    }
                    is_same_entry
                };
                if swapped {
                    self.cache_metrics.swaps.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        principal = %key.principal,
                        workspace = %key.workspace,
                        previous_revision = %expected_entry.snapshot.revision,
                        observed_revision = %candidate.revision,
                        load_failures = candidate.load_failures.len(),
                        "background revision check validated and swapped the scoped capability snapshot"
                    );
                }
            },
            Ok(_) => {},
            Err(error) => {
                self.cache_metrics
                    .revision_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    principal = %key.principal,
                    workspace = %key.workspace,
                    error = %error,
                    "background scoped capability revision check failed"
                );
            },
        }
        expected_entry
            .revision_check_in_flight
            .store(false, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn validate_cached_scope_revision_for_test(
        &self,
        principal: &str,
        workspace: &str,
    ) -> bool {
        let key = ScopedCapabilityCacheKey::new(principal, workspace);
        let Some(entry) = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
        else {
            return false;
        };
        self.validate_cached_revision(&key, &entry);
        self.cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .is_none_or(|current| !Arc::ptr_eq(current, &entry))
    }

    fn scope_revision(&self, principal: &str, workspace: &str) -> Result<String> {
        self.cache_metrics
            .revision_checks
            .fetch_add(1, Ordering::Relaxed);
        let mut hasher = blake3::Hasher::new();
        hasher.update(SCOPED_CAPABILITY_REVISION_VERSION.as_bytes());
        hasher.update(principal.as_bytes());
        hasher.update(&[0]);
        hasher.update(workspace.as_bytes());

        let scope_skills = self
            .workspace_manager
            .workspace_layout()
            .scope_skills_root(principal, workspace);
        hash_skill_catalog(&mut hasher, "scope", &scope_skills)?;
        for (index, extra) in crate::magician_v2::config_extras::extra_skills_dirs()
            .iter()
            .enumerate()
        {
            hash_skill_catalog(&mut hasher, &format!("extra:{index}"), extra)?;
        }
        if let Some(resolver) = self.secret_store_resolver.as_ref() {
            let encoded = serde_json::to_vec(resolver.runtime_capabilities())
                .context("serializing secret runtime capabilities for registry revision")?;
            hasher.update(b"secret-runtime-capabilities");
            hasher.update(&encoded);
        }
        if let Some(resources) = self.agent_resources() {
            let process_enabled =
                crate::magician_v2::api_mining::switch::runtime_api_mining_config()
                    .map(|config| config.enabled)
                    .unwrap_or(false);
            let mining_base = resources
                .artifact_workspace
                .api_mining_root(principal, workspace);
            let effective =
                crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
                    process_enabled,
                    &mining_base,
                );
            hasher.update(b"api-mining-effective");
            hasher.update(&[u8::from(effective)]);
        }
        let overlay = self.computed_capability_overlay(principal, workspace);
        hasher.update(b"computed-capability-overlay");
        hasher.update(overlay.revision.as_bytes());
        Ok(hasher.finalize().to_hex().to_string())
    }

    fn build_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        revision: Option<String>,
    ) -> Result<Arc<ScopedCapabilitySnapshot>> {
        let pack_definitions = self
            .workspace_manager
            .load_pack_defs_for_scope_fresh(principal, workspace);
        let mut pack_defs = pack_definitions.definitions;
        let embedded_fallback_names = pack_definitions.embedded_fallback_names;
        let loaded_skills: BTreeSet<String> = pack_definitions.loaded_skills.into_iter().collect();
        let load_failures = pack_definitions.load_failures;
        let overlay = self.computed_capability_overlay(principal, workspace);
        // Names of app-authored computed capabilities injected into this scope.
        // Carried on the snapshot so a dispatcher can tell app-origin tools from
        // first-party ones: an app's authority is a property of the dispatched
        // RESOURCE, not of the code path that reached it, so an app-origin tool
        // reached outside the fenced app-workflow path must fail closed rather
        // than run with the ambient agent's authority (defect C4).
        let mut app_origin_tool_installations: std::collections::BTreeMap<
            String,
            crate::magician_v2::apps::models::AppInstallationId,
        > = std::collections::BTreeMap::new();
        for entry in overlay.entries.iter() {
            if authorize_discovered_computed_capability(&overlay, &entry.tool_name).is_err() {
                continue;
            }
            let Some(pack) = crate::magician_v2::execution::compiled_providers::pack_def_from_app_computed_capability(entry)
            else {
                continue;
            };
            // Skip on collision — never OVERWRITE a same-named platform pack.
            // Overwriting let an installed app silently replace a first-party
            // tool (`http`, `files`, …) for the whole scope. Paired with the
            // fail-closed app-origin gate that replacement becomes strictly
            // worse than a feature: the platform tool would be bricked
            // scope-wide (deny + replace = denial of service). Documented
            // same-name replacement returns once the gate can CONTAIN an
            // app-origin call on the generic path instead of denying it.
            if pack_defs
                .iter()
                .any(|candidate| candidate.name == pack.name)
            {
                tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    tool = %pack.name,
                    "[APP-OVERLAY] app computed capability shadows a platform pack of the same \
                     name; keeping the platform pack"
                );
                continue;
            }
            app_origin_tool_installations.insert(pack.name.clone(), entry.installation_id.clone());
            pack_defs.push(pack);
        }
        let resources_snapshot = self.agent_resources();
        if let Some(resources) = resources_snapshot.as_ref() {
            let process_enabled =
                crate::magician_v2::api_mining::switch::runtime_api_mining_config()
                    .map(|config| config.enabled)
                    .unwrap_or(false);
            let mining_base = resources
                .artifact_workspace
                .api_mining_root(principal, workspace);
            if !crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
                process_enabled,
                &mining_base,
            ) {
                pack_defs.retain(|pack| !pack.name.starts_with("recipe__"));
            }
        }
        prune_unexecutable_pack_defs(&mut pack_defs);

        let secret_broker = self.secret_store_resolver.as_ref().and_then(|resolver| {
            prune_runtime_disabled_pack_defs(&mut pack_defs, resolver.runtime_capabilities());
            if resolver.runtime_capabilities().treasurer_enabled() {
                Some(Arc::new(ScopedSecretBroker::new(Arc::clone(resolver)))
                    as Arc<dyn SecretBroker>)
            } else {
                None
            }
        });

        let scope_paths = self.workspace_manager.scope_paths(principal, workspace);
        let handlers_snapshot = self.compiled_handlers();
        let (registry, _) = build_compiled_registry(
            Arc::clone(&self.magicutor_client),
            self.file_sandbox.clone(),
            self.shell_sandbox.clone(),
            pack_defs,
            secret_broker,
            self.workspace_manager.repo_root().to_path_buf(),
            Some(scope_paths),
            self.resource_ledger.clone(),
            self.token_store.clone(),
            resources_snapshot,
            handlers_snapshot.as_deref(),
        );
        // `build_compiled_registry` initially binds provider-backed primitive
        // packs through the generic pack provider. Replace evidence_data with
        // its closed Rust owner for every scope, but mint Apps authority only
        // when this scope actually selected the embedded fallback. A skill or
        // extra-path override remains unwitnessed even when its parsed value or
        // bytes happen to equal the embedded definition.
        if let Some(pack) = registry.get_pack_definition(EVIDENCE_DATA_TOOL_NAME) {
            let provider = Arc::new(
                EvidenceDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone()),
            );
            let selected_embedded_fallback = embedded_fallback_names
                .contains(EVIDENCE_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == EVIDENCE_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_evidence_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(EVIDENCE_DATA_TOOL_NAME)
                        .expect("embedded evidence_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Retain the closed host-read owner and its exact embedded provenance
        // in scoped snapshots, just as in the process-wide registry.
        if let Some(pack) = registry.get_pack_definition(INTERNAL_DATA_TOOL_NAME) {
            let provider = Arc::new(
                InternalDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone()),
            );
            let selected_embedded_fallback = embedded_fallback_names
                .contains(INTERNAL_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == INTERNAL_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_internal_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(INTERNAL_DATA_TOOL_NAME)
                        .expect("embedded internal_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Retain the closed host-read owner and its exact embedded provenance
        // in scoped snapshots, just as in the process-wide registry.
        if let Some(pack) = registry.get_pack_definition(THINKING_MAPS_DATA_TOOL_NAME) {
            let provider = Arc::new(
                ThinkingMapsDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone()),
            );
            let selected_embedded_fallback = embedded_fallback_names
                .contains(THINKING_MAPS_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref().iter().any(|embedded| {
                    embedded.name == THINKING_MAPS_DATA_TOOL_NAME && embedded == &pack
                });
            if selected_embedded_fallback {
                registry.register_builtin_thinking_maps_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(THINKING_MAPS_DATA_TOOL_NAME)
                        .expect("embedded thinking_maps_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Same treatment for the meetings host binder: the closed Rust owner in
        // every scope, Apps authority only where this scope actually selected
        // the embedded fallback.
        if let Some(pack) = registry.get_pack_definition(MEETINGS_DATA_TOOL_NAME) {
            let provider = Arc::new(
                MeetingsDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone()),
            );
            let selected_embedded_fallback = embedded_fallback_names
                .contains(MEETINGS_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == MEETINGS_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_meetings_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(MEETINGS_DATA_TOOL_NAME)
                        .expect("embedded meetings_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Same treatment for the agent-roster binder.
        if let Some(pack) = registry.get_pack_definition(AGENT_ROSTER_DATA_TOOL_NAME) {
            let mut provider =
                AgentRosterDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone());
            // Share the runtime's definition store so a polled roster read hits
            // a warm, write-invalidated cache instead of re-materializing every
            // builtin template per call. The artifact service is what answers
            // "which agents have work in flight": without it every row reads
            // `busy: null`, and a round gated on idleness cannot tell.
            if let Some(resources) = self.agent_resources() {
                provider =
                    provider.with_definition_store(Arc::clone(&resources.agent_definition_store));
                if let Some(service) = resources.artifact_v2_service.as_ref() {
                    provider = provider.with_activity_source(Arc::clone(service));
                }
            }
            let provider = Arc::new(provider);
            let selected_embedded_fallback = embedded_fallback_names
                .contains(AGENT_ROSTER_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref().iter().any(|embedded| {
                    embedded.name == AGENT_ROSTER_DATA_TOOL_NAME && embedded == &pack
                });
            if selected_embedded_fallback {
                registry.register_builtin_agent_roster_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(AGENT_ROSTER_DATA_TOOL_NAME)
                        .expect("embedded agent_roster_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // The task-list read binder. It needs the artifact service to answer at
        // all, so the per-scope registry — the one the app path reads — is
        // where it becomes useful; without a service every call fails loudly.
        if let Some(pack) = registry.get_pack_definition(TASKS_DATA_TOOL_NAME) {
            let mut provider = TasksDataProvider::new().with_pack_def(pack.clone());
            if let Some(service) = self
                .agent_resources()
                .and_then(|resources| resources.artifact_v2_service.clone())
            {
                provider = provider.with_service(service);
            }
            let provider = Arc::new(provider);
            let selected_embedded_fallback = embedded_fallback_names.contains(TASKS_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == TASKS_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_tasks_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(TASKS_DATA_TOOL_NAME)
                        .expect("embedded tasks_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // The notes read binder: search and the exact read of a searched note.
        if let Some(pack) = registry.get_pack_definition(NOTES_DATA_TOOL_NAME) {
            let provider = Arc::new(
                NotesDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone()),
            );
            let selected_embedded_fallback = embedded_fallback_names.contains(NOTES_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == NOTES_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_notes_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(NOTES_DATA_TOOL_NAME)
                        .expect("embedded notes_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Owner-granted app memory reads. Shares the runtime's definition store
        // so granted agents' memory tiers resolve from the warm cache.
        if let Some(pack) = registry.get_pack_definition(MEMORY_DATA_TOOL_NAME) {
            let mut provider =
                MemoryDataProvider::new(self.workspace_manager.workspace_layout().clone())
                    .with_pack_def(pack.clone());
            if let Some(resources) = self.agent_resources() {
                provider =
                    provider.with_definition_store(Arc::clone(&resources.agent_definition_store));
            }
            let provider = Arc::new(provider);
            let selected_embedded_fallback = embedded_fallback_names
                .contains(MEMORY_DATA_TOOL_NAME)
                && embedded_compiled_pack_defs_ref()
                    .iter()
                    .any(|embedded| embedded.name == MEMORY_DATA_TOOL_NAME && embedded == &pack);
            if selected_embedded_fallback {
                registry.register_builtin_memory_data_provider(
                    provider,
                    embedded_compiled_pack_yaml(MEMORY_DATA_TOOL_NAME)
                        .expect("embedded memory_data pack")
                        .as_bytes(),
                );
            } else {
                registry.register_override(provider);
            }
        }
        // Bind harness READ providers onto the freshly-built per-scope registry.
        // `build_compiled_registry` builds from pack-defs only and never carries
        // `HarnessServices`, so the harness introspection tools
        // (`list_episodes`, `magician_work_ledger`, …) — auto-granted to harness
        // agents — would have their pack-def but no provider, failing dispatch
        // with "is not a compiled pack". The scope-safe
        // `register_harness_read_providers_if_absent` variant is used here (NOT
        // the base-registry `register_harness_read_providers`): it binds a
        // harness read provider only for tools the scope has no provider for,
        // guarded on `registry.get_pack_definition(tool).is_some()`. This binds
        // the genuinely-unbound harness tools without clobbering `list_agents`,
        // which `build_compiled_registry` already installs in every scope as the
        // universal handler-backed `GenericCompiledProvider` (full roster). A
        // non-guarded re-bind would silently replace it with the narrower
        // harness `execute_list_agents` (harness delegation targets only),
        // changing a universal read tool's result across all scopes. This runs
        // only while constructing a revision-bound cache entry. Skipped
        // entirely when the resolver has no harness services.
        if let Some(services) = self.harness_services() {
            register_harness_read_providers_if_absent(&registry, services.clone());
            // Also bind the FULL harness ACTION set. Without this the per-scope
            // registry carries their deferred pack-def but no provider, so an
            // autonomous agent sees the tool yet gets `ungranted_capability_error`
            // at call time — the read-provider gap fixed earlier, for the action
            // side. The consequential mutation tools dispatch here too but are
            // gated separately by the central owner-approval rules
            // (`agents::approval::harness_mutation_approval_rules`), so broadening
            // dispatch does NOT enable ungated autonomous self-mutation.
            register_harness_action_providers_if_absent(&registry, services);
        }
        // Every provider this snapshot binds in place has bound by now. What is
        // still unbound cannot be served from this snapshot, and the catalog
        // projected from it — the one every engine reads — must not offer it.
        let withheld = registry.withhold_unbound_compiled_packs();
        if !withheld.is_empty() {
            tracing::info!(
                principal,
                workspace,
                count = withheld.len(),
                "[CAPABILITY] scope snapshot withholds compiled packs with no provider: {:?}",
                withheld
            );
        }
        let packs = registry.all_pack_definitions();
        let pack_count = packs.len();
        let tool_index = Arc::new(crate::magician_v2::execution::flat_loop::build_tool_index(
            &packs,
        ));
        let surface_tool_index =
            Arc::new(crate::magician_v2::execution::flat_loop::build_surface_tool_index(&packs));
        self.cache_metrics.builds.fetch_add(1, Ordering::Relaxed);
        let revision = revision.unwrap_or_else(|| "uncached".to_string());
        Ok(Arc::new(ScopedCapabilitySnapshot {
            revision,
            overlay_revision: overlay.revision.clone(),
            registry,
            tool_index,
            surface_tool_index,
            pack_count,
            built_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            app_origin_tool_installations: Arc::new(app_origin_tool_installations),
            loaded_skills: Arc::new(loaded_skills),
            load_failures: Arc::new(load_failures),
        }))
    }
}

/// The skills the candidate newly fails among those the served snapshot
/// loaded. A failure the served snapshot already carried for the same skill
/// is not new; a skill absent from the candidate with no failure was removed.
fn newly_failed_served_skills(
    served: &ScopedCapabilitySnapshot,
    candidate: &ScopedCapabilitySnapshot,
) -> Vec<SkillLoadFailure> {
    candidate
        .load_failures
        .iter()
        .filter(|failure| served.loaded_skills.contains(&failure.skill))
        .filter(|failure| {
            !served
                .load_failures
                .iter()
                .any(|previous| previous.skill == failure.skill)
        })
        .cloned()
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::execution::ExecutionConfig;

    fn resolver_for(root: &Path) -> ScopedCapabilityResolver {
        let workspace = ArtifactV2Workspace::new(root.join("runtime"));
        let manager = Arc::new(CapabilityWorkspaceManager::new(workspace, root));
        let magicutor = Arc::new(
            MagicutorClient::new(ExecutionConfig::default())
                .expect("test Magicutor client should construct"),
        );
        ScopedCapabilityResolver::new(
            manager,
            magicutor,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            None,
            None,
            None,
        )
    }

    /// A scope snapshot is what every engine's catalog is projected from. A
    /// compiled pack whose provider did not bind in this snapshot must not be
    /// published by it: a switched engine that follows the catalog calls the
    /// tool and gets "No provider registered".
    #[test]
    fn a_scope_snapshot_publishes_only_compiled_packs_it_can_serve() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let snapshot = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot");
        let unbound: Vec<String> = snapshot
            .registry
            .all_pack_definitions()
            .into_iter()
            .filter(|pack| {
                matches!(
                    &pack.implementation,
                    crate::magician_v2::execution::capability::ImplementationType::Compiled { .. }
                )
            })
            .filter(|pack| !snapshot.registry.has(&pack.name))
            .map(|pack| pack.name)
            .collect();
        assert!(
            unbound.is_empty(),
            "the snapshot publishes compiled packs it cannot serve: {unbound:?}"
        );
        let mut registry_packs = snapshot
            .registry
            .all_pack_definitions()
            .into_iter()
            .map(|pack| pack.name)
            .collect::<Vec<_>>();
        registry_packs.sort();
        assert_eq!(
            snapshot.tool_index.pack_names(),
            registry_packs,
            "the tool index mirrors the withheld registry"
        );
    }

    #[test]
    fn unchanged_scope_reuses_one_registry_and_tool_index_snapshot() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());

        let first = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("first snapshot");
        let revision_checks_after_cold_build = resolver.cache_status().revision_checks;
        let second = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("cached snapshot");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&first.registry, &second.registry));
        assert!(Arc::ptr_eq(&first.tool_index, &second.tool_index));
        let mut registry_packs = first
            .registry
            .all_pack_definitions()
            .into_iter()
            .map(|pack| pack.name)
            .collect::<Vec<_>>();
        registry_packs.sort();
        assert_eq!(first.tool_index.pack_names(), registry_packs);
        assert_eq!(
            resolver.cache_status().revision_checks,
            revision_checks_after_cold_build,
            "a warm snapshot lookup must not walk and hash the skill catalog"
        );

        assert_eq!(
            resolver.cache_status(),
            ScopedCapabilityCacheStatus {
                entry_count: 1,
                hits: 1,
                misses: 1,
                builds: 1,
                invalidations: 0,
                revision_checks: revision_checks_after_cold_build,
                revision_failures: 0,
                coalesced_waiters: 0,
                swaps: 0,
                held_back_revisions: 0,
                held_back_scopes: Vec::new(),
            }
        );
    }

    #[test]
    fn background_revision_check_invalidates_out_of_band_skill_change() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let first = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("first snapshot");
        let skill_root = resolver
            .workspace_manager
            .workspace_layout()
            .scope_skills_root("owner", "default")
            .join("revision-fixture");
        std::fs::create_dir_all(&skill_root).expect("fixture skill root");
        std::fs::write(
            skill_root.join("SKILL.md"),
            "---\nname: revision-fixture\ndescription: first revision\n---\nFirst revision.\n",
        )
        .expect("fixture skill");

        let second = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("warm snapshot");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.revision, second.revision);
        assert_eq!(resolver.cache_status().builds, 1);

        let key = ScopedCapabilityCacheKey::new("owner", "default");
        let cached_entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("cached entry");
        resolver.validate_cached_revision(&key, &cached_entry);
        // Validated and swapped in place: the scope never has no snapshot,
        // and the next request pays no rebuild.
        assert_eq!(resolver.cache_status().entry_count, 1);
        assert_eq!(resolver.cache_status().swaps, 1);

        let refreshed = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot swapped by the background check");

        assert_ne!(first.revision, refreshed.revision);
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(resolver.cache_status().builds, 2);
    }

    fn write_widget_skill(skill_root: &Path, name: &str, schema: &str) {
        std::fs::create_dir_all(skill_root).expect("skill root");
        std::fs::write(skill_root.join("tool_schema.yaml"), schema).expect("tool_schema");
        std::fs::write(
            skill_root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A {name} for the revision tests.\n---\n# {name}\n\nGuide.\n"),
        )
        .expect("SKILL.md");
    }

    const VALID_WIDGET_SCHEMA: &str = "name: widget\nparameters:\n- name: input\n  required: true\n  param_type: string\n  description: Input value\nimplementation:\n  type: primitive\n  command: [\"widget\"]\n";

    /// The point of validate-before-swap. An edit that breaks a skill the
    /// current catalog serves must not replace that catalog: the old snapshot
    /// keeps serving, the hold is visible on the cache status with the
    /// reason, and the check does not spin. Once the file is fixed, the next
    /// check swaps the repaired catalog in.
    #[test]
    fn a_rebuild_that_newly_fails_a_served_skill_is_held_back_until_the_skill_is_fixed() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let skill_root = resolver
            .workspace_manager
            .workspace_layout()
            .scope_skills_root("owner", "default")
            .join("widget");
        write_widget_skill(&skill_root, "widget", VALID_WIDGET_SCHEMA);
        let served = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot with the widget");
        assert!(
            served.loaded_skills.contains("widget"),
            "{:?}",
            served.loaded_skills
        );
        assert!(served.load_failures.is_empty());
        let key = ScopedCapabilityCacheKey::new("owner", "default");
        let entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("entry");

        // A bad save: the schema is no longer YAML the loader accepts.
        std::fs::write(
            skill_root.join("tool_schema.yaml"),
            "name: widget\nparameters: [unclosed\n",
        )
        .expect("broken schema");
        resolver.validate_cached_revision(&key, &entry);

        let status = resolver.cache_status();
        assert_eq!(status.swaps, 0);
        assert_eq!(status.held_back_revisions, 1);
        let held = status
            .held_back_scopes
            .first()
            .expect("the hold is reported");
        assert_eq!(
            (held.principal.as_str(), held.workspace.as_str()),
            ("owner", "default")
        );
        assert_eq!(held.failures.len(), 1);
        assert_eq!(held.failures[0].skill, "widget");
        assert!(
            held.failures[0].reason.contains("tool_schema.yaml"),
            "{}",
            held.failures[0].reason
        );
        let still_served = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("the old snapshot keeps serving");
        assert!(Arc::ptr_eq(&served, &still_served));

        // Same broken file, next check: still held, counted once per revision.
        let entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("entry");
        resolver.validate_cached_revision(&key, &entry);
        assert_eq!(resolver.cache_status().held_back_revisions, 1);

        // Fixed: the next check swaps the repaired catalog in and clears the hold.
        write_widget_skill(
            &skill_root,
            "widget",
            VALID_WIDGET_SCHEMA
                .replace("Input value", "Input value, revised")
                .as_str(),
        );
        let entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("entry");
        resolver.validate_cached_revision(&key, &entry);
        let status = resolver.cache_status();
        assert_eq!(status.swaps, 1);
        assert!(
            status.held_back_scopes.is_empty(),
            "{:?}",
            status.held_back_scopes
        );
        let repaired = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("repaired snapshot");
        assert!(!Arc::ptr_eq(&served, &repaired));
        assert!(repaired.loaded_skills.contains("widget"));
    }

    /// Removing a skill is an intentional change, not a failure: the
    /// directory is gone, nothing failed to load, the catalog swaps.
    #[test]
    fn removing_a_skill_swaps_the_catalog() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let skill_root = resolver
            .workspace_manager
            .workspace_layout()
            .scope_skills_root("owner", "default")
            .join("widget");
        write_widget_skill(&skill_root, "widget", VALID_WIDGET_SCHEMA);
        let served = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot with the widget");
        let key = ScopedCapabilityCacheKey::new("owner", "default");
        let entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("entry");

        std::fs::remove_dir_all(&skill_root).expect("uninstall");
        resolver.validate_cached_revision(&key, &entry);

        assert_eq!(resolver.cache_status().swaps, 1);
        assert_eq!(resolver.cache_status().held_back_revisions, 0);
        let after = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot without the widget");
        assert!(!Arc::ptr_eq(&served, &after));
        assert!(!after.loaded_skills.contains("widget"));
    }

    /// A newly installed skill that is broken never served anything, so it
    /// does not hold other skills' changes back; it is simply absent (and
    /// logged), as today.
    #[test]
    fn a_new_broken_skill_does_not_hold_the_catalog_back() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let skills = resolver
            .workspace_manager
            .workspace_layout()
            .scope_skills_root("owner", "default");
        write_widget_skill(&skills.join("widget"), "widget", VALID_WIDGET_SCHEMA);
        resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("snapshot with the widget");
        let key = ScopedCapabilityCacheKey::new("owner", "default");
        let entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("entry");

        std::fs::create_dir_all(skills.join("gadget")).expect("new skill");
        std::fs::write(
            skills.join("gadget").join("tool_schema.yaml"),
            "name: gadget\nparameters: [unclosed\n",
        )
        .expect("broken new schema");
        std::fs::write(
            skills.join("gadget").join("SKILL.md"),
            "---\nname: gadget\ndescription: broken on arrival\n---\n",
        )
        .expect("SKILL.md");
        resolver.validate_cached_revision(&key, &entry);

        let status = resolver.cache_status();
        assert_eq!(status.swaps, 1);
        assert_eq!(status.held_back_revisions, 0);
        let after = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("swapped snapshot");
        assert!(after.loaded_skills.contains("widget"));
        assert!(!after.loaded_skills.contains("gadget"));
        assert_eq!(after.load_failures.len(), 1);
        assert_eq!(after.load_failures[0].skill, "gadget");
    }

    #[test]
    fn stale_background_revision_check_cannot_evict_newer_snapshot() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let first = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("first snapshot");
        let key = ScopedCapabilityCacheKey::new("owner", "default");
        let first_entry = resolver
            .cache
            .read()
            .expect("cache lock")
            .get(&key)
            .cloned()
            .expect("first cache entry");

        let skill_root = resolver
            .workspace_manager
            .workspace_layout()
            .scope_skills_root("owner", "default")
            .join("revision-race-fixture");
        std::fs::create_dir_all(&skill_root).expect("fixture skill root");
        std::fs::write(
            skill_root.join("SKILL.md"),
            "---\nname: revision-race-fixture\ndescription: newer revision\n---\nNewer revision.\n",
        )
        .expect("fixture skill");
        assert!(resolver.invalidate_scope("owner", "default"));
        let newer = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("newer snapshot");
        assert_ne!(first.revision, newer.revision);

        resolver.validate_cached_revision(&key, &first_entry);
        let still_current = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("newer snapshot remains cached");
        assert!(Arc::ptr_eq(&newer, &still_current));
    }

    #[test]
    fn explicit_refresh_rebuilds_same_revision_without_broadening_catalog() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let first = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("first snapshot");
        let first_pack_names = first.tool_index.pack_names();

        let refreshed = resolver
            .refresh_scope("owner", "default")
            .expect("refreshed snapshot");

        assert_eq!(first.revision, refreshed.revision);
        assert_eq!(first_pack_names, refreshed.tool_index.pack_names());
        assert!(!Arc::ptr_eq(&first.registry, &refreshed.registry));
        let status = resolver.cache_status();
        assert_eq!(status.entry_count, 1);
        assert_eq!(status.builds, 2);
        assert_eq!(status.invalidations, 1);
    }

    #[test]
    fn cache_identity_isolated_by_principal_and_workspace() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        let owner = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("owner snapshot");
        let guest = resolver
            .capability_snapshot_for_scope("guest", "default")
            .expect("guest snapshot");
        let alternate = resolver
            .capability_snapshot_for_scope("owner", "alternate")
            .expect("alternate workspace snapshot");

        assert_ne!(owner.revision, guest.revision);
        assert_ne!(owner.revision, alternate.revision);
        assert!(!Arc::ptr_eq(&owner.registry, &guest.registry));
        assert!(!Arc::ptr_eq(&owner.registry, &alternate.registry));
        let status = resolver.cache_status();
        assert_eq!(status.entry_count, 3);
        assert_eq!(status.builds, 3);
    }

    #[test]
    fn configured_cache_limit_and_singleflight_policy_are_applied() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        resolver.configure_cache(2, 1_800, false);

        for principal in ["one", "two", "three"] {
            resolver
                .capability_snapshot_for_scope(principal, "default")
                .expect("scoped snapshot");
        }

        assert_eq!(resolver.cache_status().entry_count, 1);
        assert!(resolver
            .cache_build_locks
            .lock()
            .expect("build locks")
            .is_empty());
    }

    #[test]
    fn configured_idle_ttl_expires_an_unchanged_snapshot() {
        let temp = tempdir().expect("tempdir");
        let resolver = resolver_for(temp.path());
        resolver.configure_cache(4, 1, true);
        let first = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("first snapshot");
        let key = ScopedCapabilityCacheKey::new("owner", "default");
        resolver
            .cache
            .read()
            .expect("cache")
            .get(&key)
            .expect("entry")
            .last_access_epoch_seconds
            .store(0, Ordering::Release);

        let second = resolver
            .capability_snapshot_for_scope("owner", "default")
            .expect("rebuilt snapshot");

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(resolver.cache_status().builds, 2);
        assert_eq!(resolver.cache_status().invalidations, 1);
    }

    #[test]
    fn concurrent_cold_reads_coalesce_to_one_immutable_snapshot() {
        const CALLERS: usize = 8;
        let temp = tempdir().expect("tempdir");
        let resolver = Arc::new(resolver_for(temp.path()));
        let barrier = Arc::new(Barrier::new(CALLERS));

        let snapshots = std::thread::scope(|scope| {
            let handles = (0..CALLERS)
                .map(|_| {
                    let resolver = Arc::clone(&resolver);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        resolver
                            .capability_snapshot_for_scope("owner", "default")
                            .expect("concurrent snapshot")
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("snapshot thread"))
                .collect::<Vec<_>>()
        });

        for snapshot in snapshots.iter().skip(1) {
            assert!(Arc::ptr_eq(&snapshots[0], snapshot));
        }
        let status = resolver.cache_status();
        assert_eq!(status.entry_count, 1);
        assert_eq!(status.builds, 1);
        assert_eq!(status.hits, (CALLERS - 1) as u64);
        assert!((1..=CALLERS as u64).contains(&status.misses));
    }
}
