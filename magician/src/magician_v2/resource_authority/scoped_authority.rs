//! Per-(principal, workspace) authority bundle and resolver.
//!
//! ## Why this exists
//!
//! Resource authority state (ledger journal, token store, system
//! ceilings, freeze state) is persisted per `(principal, workspace)`
//! under `<workspace_root>/scopes/{principal}/{workspace}/resource_authority/`.
//! That matches the rest of the V3 storage model — chats, agents,
//! tasks, and artifacts are all scoped this way.
//!
//! Before this module existed, two paths read/wrote resource-authority
//! state independently:
//!
//! - **REST API** (`api/resource_authority_api.rs::ResourceAuthorityApi`)
//!   loaded per-scope ledger/token-store/ceilings/freeze from disk on
//!   each request, cached the bundle in its own per-scope map. The
//!   `/budget` UI saw correct per-scope data.
//! - **Tool dispatch** (`bin/magician.rs` → `CompiledDispatchAuthority`)
//!   constructed a single **empty global** ledger + token store at boot
//!   with no persistence load. Every gated tool call wrote to that
//!   global, which never reached disk, never showed up in the UI, and
//!   got wiped on restart.
//!
//! The two stores diverged forever — UI showed one truth, gate operated
//! on another. This module collapses that into a single resolver both
//! paths share.
//!
//! ## Boundary
//!
//! - `ScopedAuthorityBundle` — the per-scope handles (ledger,
//!   token_store, system_ceilings, system_freeze, resolver,
//!   storage_root) plus a clone of the process-wide config. Once
//!   resolved, the gate operates on this bundle's Arcs directly.
//! - `ScopedAuthorityResolver` — trait that maps
//!   `(principal, workspace) → ScopedAuthorityBundle`. Implementations
//!   own the cache + load-from-disk logic.
//! - `DiskBackedScopedResolver` — production impl. Lazy-loads per-scope
//!   state from `<workspace>/scopes/{p}/{w}/resource_authority/` on
//!   first access, caches the bundle so repeated dispatches against
//!   the same scope share the same in-memory Arcs (single source of
//!   truth).
//! - `SingleScopeResolver` — wraps a fixed bundle for tests / orphan
//!   dispatches that don't have a workspace context.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{OnceCell, RwLock};

use super::config::ResourceAuthorityConfig;
use super::gate::SystemFreezeState;
use super::ledger::ResourceLedger;
use super::spend_token_resolver::{ConfigSpendTokenResolver, SpendTokenResolver};
use super::token::SystemCeiling;
use super::token_store::TokenStore;
use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

fn persist_system_bytes(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<(), ArtifactV2Error> {
    if crate::magician_v2::system_owners::store_for_any_owner(workspace_layout, path).is_some() {
        crate::magician_v2::system_owners::persist_system_file_sync(workspace_layout, path, bytes)
            .map_err(|err| ArtifactV2Error::Io(std::io::Error::other(err.to_string())))?;
        return Ok(());
    }
    workspace_layout.write_atomic_path_sync(path, bytes)
}

/// Whitelist-validate a scope id (`principal` or `workspace`) before
/// it's joined into the on-disk scope path
/// `<root>/scopes/{principal}/{workspace}/...`.
///
/// Rejects:
/// - Empty strings (`""`) — would produce paths with `//` runs that
///   couldn't be cleaned up cleanly.
/// - `.` and `..` — directory-traversal segments.
/// - Anything containing `/`, `\`, NUL, or any other control character.
///   Without separators a string is a single path segment and
///   `PathBuf::join` can't be tricked into escaping the scope dir.
/// - Anything longer than 255 bytes — most filesystems cap segment
///   length there, so longer ids would surface as opaque I/O errors
///   later rather than a clear rejection here.
///
/// Other characters (letters, digits, `_`, `-`, `.`, `@`, etc.) are
/// allowed so the function doesn't break legitimate id shapes (e.g.
/// `user@example.com`-style principals or workspace ids with embedded
/// dots). Trailing-dot Windows quirks are left alone — production
/// deployments are Unix-only, and the dispatch path doesn't run on
/// Windows today.
pub fn is_safe_scope_id(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." || s.len() > 255 {
        return false;
    }
    !s.chars()
        .any(|c| matches!(c, '/' | '\\' | '\0') || c.is_control())
}

/// All handles for one `(principal, workspace)` scope. The gate
/// operates on the `Arc<RwLock<...>>` fields directly once a bundle
/// is resolved; cloning the bundle is cheap (just bumps Arc refcounts).
#[derive(Clone)]
pub struct ScopedAuthorityBundle {
    /// Process-wide config (enabled flag + budget rows + system
    /// ceilings declared in YAML). Cloned into every bundle so
    /// `ConfigSpendTokenResolver` and gate-side `enabled` checks
    /// don't need to chase a separate handle.
    pub config: ResourceAuthorityConfig,
    pub ledger: Arc<RwLock<ResourceLedger>>,
    pub token_store: Arc<RwLock<TokenStore>>,
    pub system_ceilings: Arc<RwLock<Vec<SystemCeiling>>>,
    pub system_freeze: Arc<RwLock<SystemFreezeState>>,
    pub resolver: Arc<dyn SpendTokenResolver>,
    /// On-disk root for this scope's resource-authority state. REST
    /// API handlers use this to persist back to the same files the
    /// resolver loaded from.
    pub storage_root: PathBuf,
    pub workspace_layout: ArtifactV2Workspace,
    /// Set when this scope's journal would NOT replay at load time.
    ///
    /// The old behaviour here was `Err(_) => ResourceLedger::new()`: one
    /// unparseable line silently replaced every account, balance, and
    /// reservation with an empty ledger, and the caller could not tell. To the
    /// gate that reads as "this scope has spent nothing", so the whole budget
    /// becomes available a second time — the single most expensive way to be
    /// wrong about a spend ledger.
    ///
    /// A bundle carrying this is FAIL-CLOSED, in two places:
    /// - `system_freeze` is engaged in memory at load, so `reserve_spend`
    ///   rejects every request for the scope before it looks at any balance;
    /// - [`Self::persist_state`] refuses to write the ledger, so the empty
    ///   in-memory stand-in never overwrites the damaged file that still holds
    ///   the real history.
    ///
    /// The freeze is in-memory only — it is never written to
    /// `system_freeze.json` — so a repaired journal clears on restart rather
    /// than leaving a scope permanently frozen by a transient read failure.
    pub ledger_load_failure: Option<String>,
}

impl std::fmt::Debug for ScopedAuthorityBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedAuthorityBundle")
            .field("enabled", &self.config.enabled)
            .field("budget_rows", &self.config.budgets.len())
            .field("storage_root", &self.storage_root)
            .field("ledger_load_failure", &self.ledger_load_failure)
            .finish()
    }
}

impl ScopedAuthorityBundle {
    /// Flush the in-memory ledger journal + token store to disk under
    /// `storage_root`. Called by the dispatch gate after every gated
    /// execution (success OR failure) so reserve/commit/rollback
    /// journal entries — plus any lazy-issued / lazy-expired / revoked
    /// tokens — survive a process restart.
    ///
    /// Why this exists: prior to Phase D the gate only ever wrote to
    /// the in-memory ledger Arc. The REST API persisted on its own
    /// schedule (handlers call `persist_*` after mutations), but the
    /// gate had no equivalent hook. Tool-driven spend was lost on
    /// every restart.
    ///
    /// Persistence shape matches the REST API's helpers (full-overwrite
    /// `save_journal` + full-overwrite `TokenStore::save`). Idempotent
    /// — concurrent persists from multiple gates / handlers in the
    /// same scope all observe the same in-memory state (shared Arcs)
    /// and produce the same file content; last writer wins but doesn't
    /// corrupt because the writes are content-identical at any given
    /// snapshot. Per-call I/O is bounded by the journal size; if that
    /// grows unwieldy, swap to `append_journal` with a per-bundle
    /// watermark.
    ///
    /// Errors are logged and swallowed — the in-memory state remains
    /// authoritative for the live process; an I/O failure means
    /// restart loses the journal entries since the last successful
    /// persist but does NOT corrupt the in-memory ledger or break
    /// the in-flight tool call's result.
    ///
    /// One exception to "always write": when `ledger_load_failure` is set the
    /// ledger write is SKIPPED. The in-memory ledger is then an
    /// empty stand-in for a journal that would not replay, and this is a
    /// full-file atomic overwrite — persisting it would replace the real spend
    /// history with nothing. The token store is still written: it has its own
    /// load path and is unaffected by journal damage.
    pub async fn persist_state(&self) {
        // Persist runs on the blocking-task pool: lock acquisition +
        // serialize + fs::write + fsync are all sync syscalls; running
        // them on the async runtime thread would pin it for ~10–100ms
        // per persist (fsync-dominated) and block every other async
        // task scheduled on that thread, including the next
        // `spend_session::admit` waiting to start a reserve.
        //
        // Lock acquisition order inside the blocking task is
        // ledger-then-token-store — matches `spend_session::admit`
        // so a writer and a persistor can't deadlock by acquiring
        // locks in opposite orders.
        //
        // Holding both read locks for the duration of both file writes
        // snapshots the ledger + token store at the same logical
        // moment — a concurrent gate writer that commits between two
        // writes can't slip in and leave the persisted ledger + token
        // store mutually inconsistent on restart (e.g. token marked
        // Expired in the store snapshot but its lazy-expiry journal
        // entry missing from the ledger snapshot).
        let ledger_arc = Arc::clone(&self.ledger);
        let token_store_arc = Arc::clone(&self.token_store);
        let storage_root = self.storage_root.clone();
        let workspace_layout = self.workspace_layout.clone();
        let ledger_load_failure = self.ledger_load_failure.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let ledger_guard = ledger_arc.blocking_read();
            let store_guard = token_store_arc.blocking_read();
            let ledger_path = storage_root.join("resource_ledger.jsonl");
            let store_path = storage_root.join("token_store.json");
            if let Some(reason) = ledger_load_failure.as_deref() {
                // The in-memory ledger is an empty stand-in for a journal we
                // could not replay. Writing it would destroy the real history
                // with a full-file atomic overwrite — and the scope is frozen,
                // so there is nothing new worth persisting anyway. Leave the
                // damaged file exactly as it is for repair/forensics.
                tracing::warn!(
                    reason = %reason,
                    path = %ledger_path.display(),
                    "[RESOURCE-AUTHORITY] refusing to overwrite an unreplayable spend journal \
                     with the empty in-memory stand-in; scope stays frozen"
                );
            } else {
                match super::persistence::journal_to_jsonl_bytes(&ledger_guard) {
                    Ok(bytes) => {
                        if let Err(error) =
                            persist_system_bytes(&workspace_layout, &ledger_path, &bytes)
                        {
                            tracing::warn!(
                                error = %error,
                                path = %ledger_path.display(),
                                "[RESOURCE-AUTHORITY] gate failed to persist ledger journal; \
                                 in-memory state is authoritative until next successful flush"
                            );
                        }
                    },
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            path = %ledger_path.display(),
                            "[RESOURCE-AUTHORITY] gate failed to serialize ledger journal; \
                             in-memory state is authoritative until next successful flush"
                        );
                    },
                }
            }
            match store_guard.to_json_bytes() {
                Ok(bytes) => {
                    if let Err(error) = persist_system_bytes(&workspace_layout, &store_path, &bytes)
                    {
                        tracing::warn!(
                            error = %error,
                            path = %store_path.display(),
                            "[RESOURCE-AUTHORITY] gate failed to persist token store; \
                             in-memory state is authoritative until next successful flush"
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        path = %store_path.display(),
                        "[RESOURCE-AUTHORITY] gate failed to serialize token store; \
                         in-memory state is authoritative until next successful flush"
                    );
                },
            }
        })
        .await;
        if let Err(error) = join_result {
            // Runtime shutting down or panic in the persist task.
            // In-memory state is still authoritative — the next
            // successful persist (or restart from disk) will catch up.
            tracing::warn!(
                error = %error,
                "[RESOURCE-AUTHORITY] persist task did not complete; in-memory \
                 state is authoritative until next successful flush"
            );
        }
    }
}

/// Maps `(principal, workspace)` to a `ScopedAuthorityBundle`.
/// `is_enabled` is a cheap fast-path so the gate can short-circuit
/// `MaybeGatedAction::Gated → degraded` without resolving a scope
/// when authority is process-wide off.
#[async_trait]
pub trait ScopedAuthorityResolver: Send + Sync + std::fmt::Debug {
    /// Process-wide enabled flag. Cheap — no scope lookup.
    fn is_enabled(&self) -> bool;

    /// Resolve the per-scope bundle. First call for a given
    /// `(principal, workspace)` loads from disk + caches; subsequent
    /// calls return the cached bundle (same Arcs).
    async fn resolve_scope(&self, principal: &str, workspace: &str) -> ScopedAuthorityBundle;
}

/// Production resolver: per-scope cache backed by on-disk JSONL +
/// JSON files under `<workspace_layout>/scopes/{p}/{w}/resource_authority/`.
///
/// The cache stores `Arc<OnceCell<ScopedAuthorityBundle>>` per
/// `(principal, workspace)` so concurrent first-touches collapse on
/// the cell's init future — only ONE caller runs `load_for_scope`'s
/// blocking I/O; the rest await the same result. The outer
/// `RwLock<HashMap<...>>` is only held long enough to look up or
/// install the cell, never across the load itself, so cold-start
/// resolutions for different scopes don't serialize.
pub struct DiskBackedScopedResolver {
    config: ResourceAuthorityConfig,
    workspace_layout: ArtifactV2Workspace,
    cache: RwLock<HashMap<(String, String), Arc<OnceCell<ScopedAuthorityBundle>>>>,
}

impl std::fmt::Debug for DiskBackedScopedResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskBackedScopedResolver")
            .field("enabled", &self.config.enabled)
            .field("budget_rows", &self.config.budgets.len())
            .finish()
    }
}

impl DiskBackedScopedResolver {
    pub fn new(config: ResourceAuthorityConfig, workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            config,
            workspace_layout,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Build a fresh bundle by loading from disk. Pure free-function
    /// shape (takes owned args, no `&self`) so the caller can move it
    /// onto a `spawn_blocking` worker without lifetime gymnastics —
    /// keeps `std::fs` calls off the async runtime thread.
    fn load_for_scope_blocking(
        config: ResourceAuthorityConfig,
        workspace_layout: ArtifactV2Workspace,
        principal: String,
        workspace: String,
    ) -> ScopedAuthorityBundle {
        let storage_root = workspace_layout.resource_authority_root(&principal, &workspace);

        // Read the journal as BYTES, not as a String: a torn append can split a
        // multi-byte character, and a UTF-8 decode failure would otherwise send
        // a recoverable torn tail down the unrecoverable path.
        let ledger_path = storage_root.join("resource_ledger.jsonl");
        let (ledger, ledger_load_failure) = match workspace_layout.read_path_sync(&ledger_path) {
            Ok(bytes) => match super::persistence::load_journal_from_bytes(&bytes) {
                Ok(replay) => {
                    if replay.torn_tail_bytes > 0 {
                        // Recoverable: the fragment was never newline-committed,
                        // so no acknowledged spend is in it. Still said out loud
                        // — it is direct evidence the process died mid-append.
                        tracing::warn!(
                            principal = %principal,
                            workspace = %workspace,
                            path = %ledger_path.display(),
                            torn_tail_bytes = replay.torn_tail_bytes,
                            "[RESOURCE-AUTHORITY] dropped an uncommitted trailing fragment from \
                             the spend journal (crash mid-append); every committed record replayed"
                        );
                    }
                    // Crash recovery — repopulate the in-memory
                    // active-reservations map from journal entries
                    // that have a reserve but no matching commit/
                    // rollback.
                    let mut loaded = replay.ledger;
                    super::recovery::reconstruct_active_reservations(&mut loaded);
                    (loaded, None)
                },
                Err(error) => {
                    let reason = format!(
                        "spend journal at {} will not replay: {error}",
                        ledger_path.display()
                    );
                    tracing::error!(
                        principal = %principal,
                        workspace = %workspace,
                        error = %error,
                        path = %ledger_path.display(),
                        "[RESOURCE-AUTHORITY] spend journal will not replay; freezing this scope \
                         and refusing to overwrite the damaged file. Spend is BLOCKED for the \
                         scope until the journal is repaired."
                    );
                    (ResourceLedger::new(), Some(reason))
                },
            },
            // No journal yet is the normal cold-start shape for a new scope.
            Err(crate::magician_v2::artifact_v2::ArtifactV2Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                (ResourceLedger::new(), None)
            },
            Err(error) => {
                let reason = format!(
                    "spend journal at {} could not be read: {error}",
                    ledger_path.display()
                );
                tracing::error!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    path = %ledger_path.display(),
                    "[RESOURCE-AUTHORITY] spend journal could not be read; freezing this scope \
                     rather than operating on an empty ledger"
                );
                (ResourceLedger::new(), Some(reason))
            },
        };

        let token_store = {
            let path = storage_root.join("token_store.json");
            workspace_layout
                .read_to_string_path_sync(&path)
                .ok()
                .and_then(|contents| TokenStore::from_json_str(&contents).ok())
                .unwrap_or_else(TokenStore::new)
        };

        let system_ceilings = {
            let path = storage_root.join("system_ceilings.json");
            match workspace_layout.read_to_string_path_sync(&path) {
                // Per-scope ceiling file overrides YAML defaults
                // (matches the REST API's CRUD behaviour — UI edits
                // persist here).
                Ok(contents) => serde_json::from_str(&contents)
                    .unwrap_or_else(|_| config.system_ceilings.clone()),
                Err(_) => config.system_ceilings.clone(),
            }
        };

        let system_freeze = {
            let path = storage_root.join("system_freeze.json");
            let mut state: SystemFreezeState =
                match workspace_layout.read_to_string_path_sync(&path) {
                    Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
                    Err(_) => SystemFreezeState::default(),
                };
            // Fail closed on an unreplayable journal. `reserve_spend` checks the
            // freeze before it looks at any token or balance, so this is the one
            // switch that stops the gate handing out budget against a ledger we
            // could not read. In-memory only: not persisted to
            // `system_freeze.json`, so it lifts on the next successful load.
            if let Some(reason) = ledger_load_failure.as_deref() {
                state.freeze("system:resource-authority", reason);
            }
            state
        };

        let ledger_arc = Arc::new(RwLock::new(ledger));
        let token_store_arc = Arc::new(RwLock::new(token_store));

        // Per-scope `ConfigSpendTokenResolver` — lazy issuance writes
        // into THIS scope's ledger + token store, so spend in workspace
        // A doesn't draw down workspace B's bucket.
        let resolver: Arc<dyn SpendTokenResolver> = Arc::new(ConfigSpendTokenResolver::new(
            config.clone(),
            Arc::clone(&ledger_arc),
            Arc::clone(&token_store_arc),
        ));

        ScopedAuthorityBundle {
            config,
            ledger: ledger_arc,
            token_store: token_store_arc,
            system_ceilings: Arc::new(RwLock::new(system_ceilings)),
            system_freeze: Arc::new(RwLock::new(system_freeze)),
            resolver,
            storage_root,
            workspace_layout,
            ledger_load_failure,
        }
    }
}

#[async_trait]
impl ScopedAuthorityResolver for DiskBackedScopedResolver {
    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    async fn resolve_scope(&self, principal: &str, workspace: &str) -> ScopedAuthorityBundle {
        let key = (principal.to_string(), workspace.to_string());

        // Acquire (or install) the per-key `OnceCell`. The outer cache
        // lock is released as soon as the cell Arc is cloned — disk
        // I/O happens entirely OUTSIDE the cache lock so unrelated
        // scopes can resolve concurrently.
        let cell = {
            let read = self.cache.read().await;
            if let Some(existing) = read.get(&key) {
                Arc::clone(existing)
            } else {
                drop(read);
                let mut write = self.cache.write().await;
                Arc::clone(
                    write
                        .entry(key.clone())
                        .or_insert_with(|| Arc::new(OnceCell::new())),
                )
            }
        };

        // Only one caller's closure runs (OnceCell guarantee); the rest
        // await the same initialized bundle. The blocking disk I/O is
        // dispatched to a `spawn_blocking` worker so the async runtime
        // thread isn't pinned during loads of large journals or slow
        // filesystem mounts. `JoinError` (i.e. the blocking task
        // panicked) is unrecoverable for this caller — propagate as
        // a panic on the awaiting task; the cell stays uninitialised
        // so a retry can attempt the load again.
        cell.get_or_init(|| {
            let config = self.config.clone();
            let workspace_layout = self.workspace_layout.clone();
            let (principal_owned, workspace_owned) = key;
            async move {
                tokio::task::spawn_blocking(move || {
                    Self::load_for_scope_blocking(
                        config,
                        workspace_layout,
                        principal_owned,
                        workspace_owned,
                    )
                })
                .await
                .expect("DiskBackedScopedResolver::load_for_scope_blocking panicked")
            }
        })
        .await
        .clone()
    }
}

/// Fixed-bundle resolver. Used by tests and any orphan dispatch path
/// that legitimately has no scope context. Production callers should
/// always use `DiskBackedScopedResolver`.
pub struct SingleScopeResolver {
    bundle: ScopedAuthorityBundle,
}

impl std::fmt::Debug for SingleScopeResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleScopeResolver")
            .field("bundle", &self.bundle)
            .finish()
    }
}

impl SingleScopeResolver {
    pub fn new(bundle: ScopedAuthorityBundle) -> Self {
        Self { bundle }
    }
}

#[async_trait]
impl ScopedAuthorityResolver for SingleScopeResolver {
    fn is_enabled(&self) -> bool {
        self.bundle.config.enabled
    }

    async fn resolve_scope(&self, _principal: &str, _workspace: &str) -> ScopedAuthorityBundle {
        self.bundle.clone()
    }
}

/// Phase D end-to-end integration tests for the resource-authority
/// dispatch pipeline. Exercise the chain
///
///   config → DiskBackedScopedResolver → CompiledDispatchAuthority
///   → execute_maybe_gated → ConfigSpendTokenResolver → SpendGatedAction
///   → ResourceLedger → atomic_write → on-disk JSONL
///
/// against a real `tempfile::tempdir`. Each test asserts both the
/// behavioural outcome (success / rejection) and the persisted on-disk
/// state, so a regression that breaks the dispatch-gate-persist round
/// trip surfaces here instead of in production.
///
/// These tests purposefully use a synthetic `Gated` action (Bash with
/// command `true`) and a no-op `exec_inner` closure — the goal is to
/// validate the gating / resolution / persistence machinery, NOT any
/// specific provider's execution. Per-path tests for chat / autonomous /
/// inner-loop / skill all converge here via `execute_maybe_gated`, so
/// one set of integration tests covers all dispatch entry points.
#[cfg(any(test, feature = "test-fixtures"))]
mod integration_tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::execution::actions::{ActionResult, BashAction, ExecutableAction};
    use crate::magician_v2::execution::compiled_dispatch::{
        execute_maybe_gated, CompiledDispatchAuthority, CompiledDispatchContext,
    };
    use crate::magician_v2::resource_authority::config::{BudgetRow, BudgetScope};
    use crate::magician_v2::resource_authority::gated_action::{
        MaybeGatedAction, SpendGatedAction,
    };
    use crate::magician_v2::resource_authority::ledger::JournalEntry;
    use crate::magician_v2::resource_authority::persistence;
    use crate::magician_v2::resource_authority::spend_gate::SpendGate;
    use crate::magician_v2::resource_authority::token::{CarryoverPolicy, CeilingPeriod};
    use rust_decimal::Decimal;
    use std::sync::Arc;

    fn synthetic_gated_action(cost: Decimal, commodity: &str) -> MaybeGatedAction {
        let action = ExecutableAction::Bash(BashAction::new("true"));
        let gate = SpendGate {
            commodity: commodity.to_string(),
            estimated_cost: cost,
            capability_name: "test_tool".to_string(),
            metered: false,
            max_cost: None,
        };
        MaybeGatedAction::Gated(SpendGatedAction::new(action, gate))
    }

    fn principal_budget(principal: &str, commodity: &str, ceiling: Decimal) -> BudgetRow {
        BudgetRow {
            scope: BudgetScope::Principal,
            id: principal.to_string(),
            commodity: commodity.to_string(),
            ceiling,
            period: CeilingPeriod::Monthly,
            carryover: CarryoverPolicy::None,
            system_ceiling_id: None,
        }
    }

    fn ctx_for<'a>(
        principal: &'a str,
        workspace: &'a str,
        agent: &'a str,
        session: &'a str,
    ) -> CompiledDispatchContext<'a> {
        CompiledDispatchContext {
            principal,
            workspace: Some(workspace),
            agent_id: agent,
            session_id: Some(session),
            task_id: None,
            execution_id: Some(session),
            chat_session_id: None,
            invocation_context: None,
            calling_profile_name: None,
            app_owner_execution_credential: None,
            active_owner: format!("test:{session}"),
            effect_id: None,
            invocation_source:
                crate::magician_v2::learning::LearningSkillInvocationSource::CompiledProvider,
            learning_store: None,
        }
    }

    async fn run_dispatch(
        authority: &CompiledDispatchAuthority,
        ctx: &CompiledDispatchContext<'_>,
        cost: Decimal,
    ) -> Result<ActionResult, crate::magician_v2::execution::error::ExecutionError> {
        execute_maybe_gated(
            synthetic_gated_action(cost, "USD"),
            |_action| async { Ok(ActionResult::Success) },
            Some(authority),
            ctx,
            "test_tool",
        )
        .await
    }

    /// End-to-end: configure a 3 USD principal budget, dispatch 3 × 1
    /// USD calls (all succeed), then assert the 4th dispatch rejects
    /// because the budget is exhausted. Verifies the entire chain from
    /// config to on-disk ledger.
    #[tokio::test]
    async fn gate_fires_persists_and_exhausts() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());

        let config = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_budget("alice", "USD", Decimal::from(3))],
        };
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            config,
            workspace_layout.clone(),
        ));
        let authority = CompiledDispatchAuthority::from_resolver(resolver);

        let ctx = ctx_for("alice", "default", "test-agent", "session-1");

        // Three dispatches at 1 USD each — within the 3 USD ceiling.
        for i in 0..3 {
            let result = run_dispatch(&authority, &ctx, Decimal::from(1)).await;
            assert!(
                result.is_ok(),
                "dispatch {i} should succeed within budget, got {result:?}"
            );
        }

        // Verify the ledger was actually persisted to disk (Phase D
        // auto-persistence — pre-D the gate wrote in-memory only).
        let ledger_path = workspace_layout
            .resource_authority_root("alice", "default")
            .join("resource_ledger.jsonl");
        assert!(
            ledger_path.exists(),
            "ledger file should be persisted at {}",
            ledger_path.display()
        );
        let journal_content = std::fs::read_to_string(&ledger_path).unwrap();
        assert!(
            !journal_content.is_empty(),
            "journal should have entries (token issuance + reserves + commits)"
        );
        // Each dispatch should produce reserve + commit journal lines;
        // plus one token-issuance entry on first call. Conservatively
        // expect ≥4 lines (1 issuance + 3 × at-least-one-entry).
        let line_count = journal_content.lines().count();
        assert!(
            line_count >= 4,
            "expected ≥4 journal entries for 3 dispatches + 1 issuance, saw {line_count}"
        );

        // 4th dispatch — budget exhausted, gate should reject.
        let result = run_dispatch(&authority, &ctx, Decimal::from(1)).await;
        assert!(
            result.is_err(),
            "4th dispatch should reject (budget exhausted), got {result:?}"
        );
    }

    #[tokio::test]
    async fn wildcard_agent_row_matches_any_agent_and_canonicalizes_usd() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        let config = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![BudgetRow {
                scope: BudgetScope::Agent,
                id: "*".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::from(2),
                period: CeilingPeriod::Daily,
                carryover: CarryoverPolicy::None,
                system_ceiling_id: None,
            }],
        };
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            config,
            workspace_layout.clone(),
        ));
        let authority = CompiledDispatchAuthority::from_resolver(resolver);
        let ctx = ctx_for("alice", "default", "presto", "session-w");
        let ctx_bob = ctx_for("alice", "default", "bob", "session-w2");

        let first = execute_maybe_gated(
            synthetic_gated_action(Decimal::from(1), "usd"),
            |_action| async { Ok(ActionResult::Success) },
            Some(&authority),
            &ctx,
            "web_answer",
        )
        .await;
        assert!(first.is_ok(), "wildcard USD row must match lowercase spend");

        let second = execute_maybe_gated(
            synthetic_gated_action(Decimal::from(1), "USD"),
            |_action| async { Ok(ActionResult::Success) },
            Some(&authority),
            &ctx_bob,
            "other_tool",
        )
        .await;
        assert!(
            second.is_ok(),
            "shared wildcard pool must accept a different agent"
        );

        let third = execute_maybe_gated(
            synthetic_gated_action(Decimal::from(1), "usd"),
            |_action| async { Ok(ActionResult::Success) },
            Some(&authority),
            &ctx,
            "web_answer",
        )
        .await;
        assert!(
            third.is_err(),
            "shared 2 USD wildcard pool must exhaust across agents"
        );
    }

    /// Per-scope isolation: two principals (alice + bob) each with a
    /// 3 USD ceiling spend independently — alice draining her budget
    /// does not affect bob, and vice versa. Verifies Phase D's
    /// per-(principal, workspace) resolver actually keeps ledgers and
    /// token stores separate on disk + in memory.
    #[tokio::test]
    async fn per_scope_isolation_alice_and_bob() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());

        let config = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![
                principal_budget("alice", "USD", Decimal::from(2)),
                principal_budget("bob", "USD", Decimal::from(2)),
            ],
        };
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            config,
            workspace_layout.clone(),
        ));
        let authority = CompiledDispatchAuthority::from_resolver(resolver);

        let alice = ctx_for("alice", "default", "agent-a", "alice-1");
        let bob = ctx_for("bob", "default", "agent-b", "bob-1");

        // Alice drains her budget (2 dispatches).
        for _ in 0..2 {
            assert!(run_dispatch(&authority, &alice, Decimal::from(1))
                .await
                .is_ok());
        }
        // Alice's 3rd dispatch — exhausted.
        assert!(run_dispatch(&authority, &alice, Decimal::from(1))
            .await
            .is_err());

        // Bob's budget is still intact — he should be able to dispatch
        // his full 2 USD.
        for _ in 0..2 {
            assert!(
                run_dispatch(&authority, &bob, Decimal::from(1))
                    .await
                    .is_ok(),
                "bob's budget should be untouched by alice's spend"
            );
        }
        // Bob's 3rd — now exhausted.
        assert!(run_dispatch(&authority, &bob, Decimal::from(1))
            .await
            .is_err());

        // On-disk scopes are distinct directories.
        let alice_root = workspace_layout.resource_authority_root("alice", "default");
        let bob_root = workspace_layout.resource_authority_root("bob", "default");
        assert_ne!(
            alice_root, bob_root,
            "alice and bob should resolve to different storage roots"
        );
        assert!(alice_root.join("resource_ledger.jsonl").exists());
        assert!(bob_root.join("resource_ledger.jsonl").exists());

        // Cross-contamination check: alice's ledger should NOT contain
        // bob's principal id and vice versa.
        let alice_journal =
            std::fs::read_to_string(alice_root.join("resource_ledger.jsonl")).unwrap();
        let bob_journal = std::fs::read_to_string(bob_root.join("resource_ledger.jsonl")).unwrap();
        assert!(
            !alice_journal.contains("principal:bob"),
            "alice's journal should not reference bob"
        );
        assert!(
            !bob_journal.contains("principal:alice"),
            "bob's journal should not reference alice"
        );
    }

    /// Path-traversal whitelist: gated dispatch with an unsafe scope
    /// id (`..`, `/`, control chars, etc.) must hard-error before
    /// reaching `resolve_scope`. Phase D's `is_safe_scope_id` guard.
    #[tokio::test]
    async fn rejects_path_traversal_in_scope_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());

        let config = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_budget("alice", "USD", Decimal::from(10))],
        };
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            config,
            workspace_layout.clone(),
        ));
        let authority = CompiledDispatchAuthority::from_resolver(resolver);

        // Unsafe principal (parent-dir traversal) → reject.
        let unsafe_principal = ctx_for("..", "default", "agent", "session");
        let result = run_dispatch(&authority, &unsafe_principal, Decimal::from(1)).await;
        assert!(result.is_err(), "`..` principal must reject");

        // Unsafe workspace (forward slash) → reject.
        let unsafe_workspace = ctx_for("alice", "evil/path", "agent", "session");
        let result = run_dispatch(&authority, &unsafe_workspace, Decimal::from(1)).await;
        assert!(result.is_err(), "workspace containing `/` must reject");

        // Empty workspace → reject (subset of safety check).
        let empty_workspace = ctx_for("alice", "", "agent", "session");
        let result = run_dispatch(&authority, &empty_workspace, Decimal::from(1)).await;
        assert!(result.is_err(), "empty workspace must reject");

        // Confirm no `..` or `evil` scope dir was created during the
        // rejected attempts — the validation fires before any disk I/O.
        let traversal_path = tmp.path().join("scopes").join("..");
        let evil_path = tmp.path().join("scopes").join("alice").join("evil");
        assert!(!traversal_path.exists() || !traversal_path.join("default").exists());
        assert!(!evil_path.exists());
    }

    /// Write `contents` as `alice/default`'s spend journal.
    fn seed_journal(workspace_layout: &ArtifactV2Workspace, contents: &[u8]) -> std::path::PathBuf {
        let root = workspace_layout.resource_authority_root("alice", "default");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("resource_ledger.jsonl");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn enabled_config() -> ResourceAuthorityConfig {
        ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_budget("alice", "USD", Decimal::from(100))],
        }
    }

    /// A journal with committed spend followed by a TORN trailing fragment
    /// replays every committed record. The old reader failed the whole parse
    /// and the caller swapped in an empty ledger, handing the scope its entire
    /// budget back.
    #[tokio::test]
    async fn torn_trailing_fragment_replays_committed_spend() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());

        // Two committed records, then a half-written third with no newline.
        let mut ledger = ResourceLedger::new();
        ledger
            .record(JournalEntry::bootstrap("cfo", "USD", Decimal::from(500)))
            .unwrap();
        ledger
            .record(JournalEntry::token_issuance(
                "cfo",
                "tok1",
                "USD",
                Decimal::from(100),
            ))
            .unwrap();
        let mut bytes = persistence::journal_to_jsonl_bytes(&ledger).unwrap();
        bytes.extend_from_slice(b"{\"id\":\"9f2");
        seed_journal(&workspace_layout, &bytes);

        let resolver = DiskBackedScopedResolver::new(enabled_config(), workspace_layout.clone());
        let bundle = resolver.resolve_scope("alice", "default").await;

        assert!(
            bundle.ledger_load_failure.is_none(),
            "a torn tail is recoverable, not a load failure"
        );
        assert_eq!(bundle.ledger.read().await.journal.len(), 2);
        assert!(
            !bundle.system_freeze.read().await.frozen,
            "a recovered torn tail must not freeze the scope"
        );
    }

    /// INTERIOR corruption must not silently zero the ledger. The bundle comes
    /// back marked, frozen (so `reserve_spend` denies before it reads a
    /// balance), and `persist_state` must not overwrite the damaged file with
    /// the empty in-memory stand-in.
    #[tokio::test]
    async fn interior_corruption_freezes_the_scope_and_preserves_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());

        let mut ledger = ResourceLedger::new();
        ledger
            .record(JournalEntry::bootstrap("cfo", "USD", Decimal::from(500)))
            .unwrap();
        ledger
            .record(JournalEntry::token_issuance(
                "cfo",
                "tok1",
                "USD",
                Decimal::from(100),
            ))
            .unwrap();
        let clean = persistence::journal_to_jsonl_bytes(&ledger).unwrap();
        let text = String::from_utf8(clean).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines[0] = "{ not json at all";
        let damaged = format!("{}\n", lines.join("\n"));
        let path = seed_journal(&workspace_layout, damaged.as_bytes());

        let resolver = DiskBackedScopedResolver::new(enabled_config(), workspace_layout.clone());
        let bundle = resolver.resolve_scope("alice", "default").await;

        // Visible to the caller, not silently swallowed.
        assert!(
            bundle.ledger_load_failure.is_some(),
            "an unreplayable journal must be visible on the bundle"
        );
        // Fail closed: the gate checks freeze before any balance.
        let freeze = bundle.system_freeze.read().await.clone();
        assert!(freeze.frozen, "unreplayable journal must freeze the scope");
        assert!(freeze.reason.is_some());

        // The damaged file survives a persist — the empty in-memory ledger must
        // never overwrite the real history.
        bundle.persist_state().await;
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, damaged, "damaged journal must be left untouched");
    }

    /// A dispatch into a scope whose journal will not replay is REJECTED rather
    /// than being allowed to spend against a phantom empty ledger.
    #[tokio::test]
    async fn dispatch_into_a_damaged_scope_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        seed_journal(&workspace_layout, b"{ not json at all\n");

        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            enabled_config(),
            workspace_layout.clone(),
        ));
        let authority = CompiledDispatchAuthority::from_resolver(resolver);
        let ctx = ctx_for("alice", "default", "test-agent", "session-damaged");

        let result = run_dispatch(&authority, &ctx, Decimal::from(1)).await;
        assert!(
            result.is_err(),
            "spend must not proceed against an unreadable ledger, got {result:?}"
        );
    }

    /// `is_safe_scope_id` whitelist coverage. Standalone test of the
    /// validator since it's the load-bearing guard against
    /// path-traversal and junk-named scopes.
    #[test]
    fn is_safe_scope_id_accepts_typical_ids() {
        assert!(is_safe_scope_id("alice"));
        assert!(is_safe_scope_id("user_123"));
        assert!(is_safe_scope_id("user-with-dashes"));
        assert!(is_safe_scope_id("workspace.with.dots"));
        assert!(is_safe_scope_id("user@example.com"));
    }

    #[test]
    fn is_safe_scope_id_rejects_unsafe_ids() {
        assert!(!is_safe_scope_id(""));
        assert!(!is_safe_scope_id("."));
        assert!(!is_safe_scope_id(".."));
        assert!(!is_safe_scope_id("evil/path"));
        assert!(!is_safe_scope_id("evil\\path"));
        assert!(!is_safe_scope_id("with\0null"));
        assert!(!is_safe_scope_id("with\tcontrol"));
        assert!(!is_safe_scope_id("with\nnewline"));
        // Length cap
        let long = "a".repeat(256);
        assert!(!is_safe_scope_id(&long));
        let max_ok = "a".repeat(255);
        assert!(is_safe_scope_id(&max_ok));
    }
}
