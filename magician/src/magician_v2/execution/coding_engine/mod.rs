//! Coding-engine adapters for long-running code work.
//!
//! The first implementation embeds Pi through its RPC mode. Engines run
//! against a shadow workspace, then Magician imports the shadow-vs-real
//! patch as a `CodeChangeProposal` so existing `diff_approval` HITL owns
//! the real workspace mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::agentic::{DiffApprovalFile as AgenticDiffApprovalFile, UserInputType};
use super::file_edit::diff::compute_unified_diff;
use super::file_edit::proposal::{CodeChangeProposal, CodeChangeProposalStore, TestRunSummary};
use super::file_edit::snapshot::MAX_FILE_READ_BYTES;
use super::file_edit::transaction::TransactionScope;

pub mod agy;
pub mod agy_contract;
pub mod agy_qualification;
pub mod agy_qualify_worker;
pub mod budgets;
pub mod catalog;
pub mod citizen;
pub mod claude;
pub mod claude_contract;
pub mod claude_qualification;
pub mod claude_qualify_worker;
pub mod codex;
pub mod codex_contract;
pub mod codex_lifecycle;
pub(crate) mod codex_usage;
pub mod control;
pub mod cost_budget;
pub mod discovery;
pub mod factory;
pub mod grok;
pub mod grok_contract;
pub mod grok_qualification;
pub mod grok_qualify_worker;
pub mod jsonl;
pub mod ledger;
pub mod model_discovery;
pub mod original_edits;
pub mod pi;
pub mod qualification;
pub mod qualify_worker;
pub mod run_handoff;
pub mod selection;
pub mod shadow_lock;
pub mod task_budget;
pub mod write_denials;

pub use agy::AgyCliAdapter;
pub use budgets::{
    clear_termination_reason, coding_budget_settings, configure_coding_budgets,
    record_termination_reason, termination_reason, CodingProgressPhase, CodingTerminated,
    CodingTerminationReason, ProgressWatchdog, ResolvedCodingBudgets,
};
pub use claude::ClaudeCodeAdapter;
pub use codex::CodexAppServerAdapter;
pub use codex_lifecycle::ContinuationFreshReason;
pub use control::{
    coding_control_registry, scoped_control_key, CodingControlAction, CodingControlHandle,
    CodingControlRegistry,
};
pub use factory::{
    construct_coding_adapter, AgyCodingOptions, AgyTurnMode, AgyTurnOptions, ClaudeCodingOptions,
    ClaudeTurnMode, ClaudeTurnOptions, CodexCodingOptions, CodexTurnMode, CodexTurnOptions,
    CodingAdapterFactoryError, CodingAdapterSpec, GrokCodingOptions, GrokTurnMode, GrokTurnOptions,
    PiCodingOptions, PiTurnOptions,
};
pub use grok::GrokAcpAdapter;
pub use pi::PiCodingEngineAdapter;
pub use selection::CodingContinuationRef;
pub use shadow_lock::shadow_admission_lock;
pub use task_budget::{CodingTaskBudgetLedger, TaskBudgetStatus};

#[derive(Debug, Clone, Serialize)]
pub struct CodingRepoBinding {
    pub repo_path: String,
    #[serde(skip)]
    pub real_path: PathBuf,
}

/// Stable per-repo key for the persistent shadow workspace (R2). Hashing the
/// real repo path keeps the key short + filesystem-safe and ensures distinct
/// repos never share a cache-warm shadow.
pub fn persistent_shadow_key(real_path: &Path) -> String {
    blake3::hash(real_path.display().to_string().as_bytes())
        .to_hex()
        .as_str()[..16]
        .to_string()
}

/// Resolve the per-repo shadow workspace root (the single source of truth for
/// the four call sites that previously inlined this derivation).
///
/// Defaults to the scope-nested `<scope_root>/coding_engine/worktrees/<key>`,
/// which is writable under the OS-sandbox gate because it nests inside
/// `magician_data_v3`. Operators can relocate the (multi-GB) shadow OUT of the
/// scope data tree with `MAGICIAN_CODING_SANDBOX_ROOT=<dir>` — the relocated
/// root is `<MAGICIAN_CODING_SANDBOX_ROOT>/coding_engine/worktrees/<key>`.
/// Relocation is opt-in so the default preserves the warm shadow cache; choose a
/// target OUTSIDE the live repo (or under `magician_data_v3`) so the gate keeps
/// it writable.
///
/// Footgun guard: a relocation target INSIDE the live repo but OUTSIDE the
/// writable storage base would desync the `GIT_CEILING` + OS-sandbox allow rule
/// (both key on the storage base, not the relocated root) — under the gate the
/// shadow's own writes would be OS-denied, and with the gate off git could walk
/// up to the live `.git`. Such a target is rejected here (warn + fall back to the
/// scope-nested default) rather than silently defeating the fences.
pub fn coding_shadow_root(scope_root: &Path, real_path: &Path) -> PathBuf {
    coding_sandbox_root(scope_root)
        .join("coding_engine")
        .join("worktrees")
        .join(persistent_shadow_key(real_path))
}

/// Magician's own folders a coding turn must never write: the runtime root
/// (config, `.env`, vault, project and approval records) and the live source
/// checkout. Coding folders inside them are admitted by
/// [`admissible_write_root`].
pub fn guarded_magician_paths() -> Vec<PathBuf> {
    let mut guarded = vec![canonicalize_lenient(
        &crate::magician_v2::process_storage::runtime_root(),
    )];
    if let Some((source, _)) = live_repo_source_fence() {
        guarded.push(canonicalize_lenient(&source));
    }
    guarded
}

/// Whether a folder from a project's settings (an owner-allowed write path or
/// a build cache) may join a turn's write roots. Checked again at every turn,
/// so a list edited on disk cannot widen the sandbox: absolute, not on the
/// never-allow list, and neither inside nor holding Magician's runtime root
/// or source — except the `coding_engine` folders, where shadows, clones,
/// build caches and engine sessions live.
pub fn admissible_write_root(root: &Path, scope_root: &Path, home: Option<&Path>) -> bool {
    if !root.is_absolute() || write_denials::never_allow(root, home) {
        return false;
    }
    let root = canonicalize_lenient(root);
    let coding_dirs = [
        canonicalize_lenient(&scope_root.join("coding_engine")),
        canonicalize_lenient(&coding_sandbox_root(scope_root).join("coding_engine")),
    ];
    if coding_dirs.iter().any(|dir| root.starts_with(dir)) {
        return true;
    }
    !guarded_magician_paths()
        .iter()
        .any(|guarded| root.starts_with(guarded) || guarded.starts_with(&root))
}

/// The directory coding-run state hangs off: `MAGICIAN_CODING_SANDBOX_ROOT`
/// when set (and not a footgun — see [`coding_shadow_root`]), else the scope
/// root. Shadows and the shared build dir both live under its `coding_engine/`.
pub fn coding_sandbox_root(scope_root: &Path) -> PathBuf {
    match std::env::var_os("MAGICIAN_CODING_SANDBOX_ROOT").map(PathBuf::from) {
        Some(reloc) => {
            let inside_repo_outside_base = live_repo_source_fence()
                .map(|(repo, sandbox_base)| {
                    let resolved = canonicalize_lenient(&reloc);
                    resolved.starts_with(&repo) && !resolved.starts_with(&sandbox_base)
                })
                .unwrap_or(false);
            if inside_repo_outside_base {
                tracing::warn!(
                    "MAGICIAN_CODING_SANDBOX_ROOT={} is inside the live repo but outside the \
                     storage base — ignoring (would desync the isolation fences); using the \
                     scope-nested default shadow root",
                    reloc.display()
                );
                scope_root.to_path_buf()
            } else {
                reloc
            }
        },
        None => scope_root.to_path_buf(),
    }
}

/// Free bytes on the filesystem holding `path`, or `None` when it cannot be
/// read.
#[cfg(unix)]
pub fn free_disk_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated path and `stat` a writable
    // struct of the type `statvfs` fills.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field widths differ by platform
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
pub fn free_disk_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Empty the shared build dir when its disk is below `min_free_bytes` and no
/// cargo is using it — the equivalent of a `cargo clean`, done before a turn
/// rather than letting builds fill a single-disk machine. Returns whether it
/// pruned.
///
/// "Not in use" is cargo's own lock: every profile directory holds a
/// `.cargo-lock` that a running build keeps exclusively locked. If any is held,
/// nothing is removed — a running build is never pulled out from under.
pub fn prune_build_dir_when_low(dir: &Path, min_free_bytes: u64) -> Result<bool, String> {
    use fs2::FileExt;
    if min_free_bytes == 0 || !dir.is_dir() {
        return Ok(false);
    }
    match free_disk_bytes(dir) {
        Some(free) if free < min_free_bytes => {},
        _ => return Ok(false),
    }
    let mut held = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries.flatten() {
        let lock_path = entry.path().join(".cargo-lock");
        if !lock_path.is_file() {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| format!("{}: {error}", lock_path.display()))?;
        if file.try_lock_exclusive().is_err() {
            return Ok(false);
        }
        held.push(file);
    }
    let before = free_disk_bytes(dir);
    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            tracing::warn!(path = %path.display(), %error, "build dir prune: could not remove");
        }
    }
    drop(held);
    tracing::warn!(
        build_dir = %dir.display(),
        free_before = ?before,
        free_after = ?free_disk_bytes(dir),
        floor = min_free_bytes,
        "coding build dir emptied: free disk space was below the floor"
    );
    Ok(true)
}

/// The shared `CARGO_TARGET_DIR` for Pi coding turns, created if missing.
///
/// `configured` (`coding.build_dir`) may be any absolute path; `~` expands.
/// Unset, it is `<coding_sandbox_root>/coding_engine/build/cargo-target`. One
/// dir shared by every repo and run, because a Rust target is tens of GB and a
/// per-run copy fills a single-disk machine. Cargo's own lock serializes
/// builds that collide.
///
/// Errors — rather than falling back somewhere else — when the configured dir
/// is relative, cannot be created, or sits inside the live repo source outside
/// the storage base (the OS sandbox denies writes there, so every build in the
/// run would fail with a permission error far from its cause).
pub fn coding_build_dir(scope_root: &Path, configured: Option<&str>) -> Result<PathBuf, String> {
    let dir = match configured.map(str::trim).filter(|dir| !dir.is_empty()) {
        Some(dir) => {
            let expanded = PathBuf::from(expand_home_path(dir));
            if !expanded.is_absolute() {
                return Err(format!("coding.build_dir `{dir}` must be an absolute path"));
            }
            expanded
        },
        None => coding_sandbox_root(scope_root)
            .join("coding_engine")
            .join("build")
            .join("cargo-target"),
    };
    if let Some((repo, sandbox_base)) = live_repo_source_fence() {
        let resolved = canonicalize_lenient(&dir);
        if resolved.starts_with(&repo) && !resolved.starts_with(&sandbox_base) {
            return Err(format!(
                "coding.build_dir `{}` is inside the Magician source tree, where the coding \
                 sandbox denies writes; choose a directory outside it",
                dir.display()
            ));
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("coding build dir `{}`: {error}", dir.display()))?;
    Ok(dir)
}

/// Process-wide live repo source root (the magician git working tree), set once
/// at startup so sandboxed shell actions far from any scope context can still
/// refuse to mutate the user's real repo. See [`reject_repo_source_tree`].
static LIVE_REPO_SOURCE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Record the live repo source root (called once at startup with
/// `cli.config.parent()`). Idempotent — later calls are ignored.
///
/// Resolves to an ABSOLUTE path first: `cli.config.parent()` can be `""` (a bare
/// relative `--config` like the supervisor's default `tool-runtime-config.yaml`)
/// or relative. An empty/relative root is catastrophic for the fence — an empty
/// `repo_source_root` is a `starts_with` prefix of EVERY path, so the fence would
/// reject every explicit working_dir (including the legitimate sandbox). Absolutize
/// against the process CWD (= the real repo root for every shipped launcher), then
/// canonicalize so it compares like-for-like with canonicalized candidates.
pub fn set_live_repo_source_root(root: PathBuf) {
    let absolute = if root.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or(root)
    } else if root.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&root))
            .unwrap_or(root)
    } else {
        root
    };
    let canonical = absolute.canonicalize().unwrap_or(absolute);
    let _ = LIVE_REPO_SOURCE_ROOT.set(canonical);
}

/// Process-wide scope-storage base (`config.storage_path` resolved against the
/// repo root), set once at startup. The fence treats this subtree as WRITABLE
/// even though it nests inside the repo — it holds scope data + the shadow
/// workspace. Falls back to `<repo>/magician_data_v3` (the default) when unset,
/// so the fence + OS-sandbox gate stay correct for non-default `storage_path`
/// deployments instead of hardcoding the dir name.
static LIVE_STORAGE_BASE: OnceLock<PathBuf> = OnceLock::new();

/// Canonicalize a path whose leaf may not exist yet: resolve the nearest
/// existing ancestor, then re-append the unresolved tail. Mirrors
/// `native_executors::resolve_path_for_policy` so the stored storage base
/// compares like-for-like with the canonicalized candidates the fence checks —
/// even on a fresh deploy (storage dir absent) under a symlinked repo path,
/// where a plain `canonicalize().unwrap_or(raw)` would keep a non-canonical
/// prefix and wrongly reject legitimate in-sandbox writes.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut existing = path;
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return path.to_path_buf(),
        }
    }
    let canonical = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    match path.strip_prefix(existing) {
        Ok(tail) => canonical.join(tail),
        Err(_) => canonical,
    }
}

/// Record the resolved scope-storage base (called once at startup, idempotent).
/// Leniently canonicalized so it compares like-for-like with canonicalized
/// candidates even when the storage dir does not exist yet.
pub fn set_live_storage_base(base: PathBuf) {
    let _ = LIVE_STORAGE_BASE.set(canonicalize_lenient(&base));
}

/// The `(repo_source_root, sandbox_base)` fence pair derived from the
/// process-wide live repo root, or `None` when it was never set or isn't a real
/// git repo. `sandbox_base` is the resolved scope-storage base (default
/// `magician_data_v3`) that is ALLOWED even though it nests inside the repo. Used
/// by the shell deny-fence + OS-sandbox gate, which have no scope
/// `workspace_root` to derive from.
pub fn live_repo_source_fence() -> Option<(PathBuf, PathBuf)> {
    let root = LIVE_REPO_SOURCE_ROOT.get()?;
    if !root.join(".git").exists() {
        return None;
    }
    let default_base = root.join("magician_data_v3");
    let configured = LIVE_STORAGE_BASE.get().cloned();
    // Degenerate-config guard: `sandbox_base` MUST be a STRICT proper subpath of
    // the repo root. A custom `storage_path` that resolves to the repo root or an
    // ancestor (e.g. ".", "..") would make the SBPL `(allow … sandbox_base)`
    // re-permit the whole repo (last-match-wins) AND make the declarative
    // `!starts_with(sandbox_base)` always false — silently defeating BOTH layers.
    // Fall back to the nested default instead of arming a useless fence.
    let sandbox_base = match configured {
        Some(base) if base.starts_with(root) && base != *root => base,
        _ => default_base,
    };
    Some((root.clone(), sandbox_base))
}

/// Derive the `(repo_source_root, sandbox_base)` fence pair from a scope
/// workspace root by finding its `magician_data_v3` ancestor (the dangerous
/// nested-sandbox layout). `None` when the sandbox isn't nested inside a
/// `magician_data_v3` dir (a sandbox configured outside any repo is unaffected).
pub fn live_repo_fence_roots(workspace_root: &Path) -> Option<(PathBuf, PathBuf)> {
    let sandbox_base = workspace_root
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new("magician_data_v3")))?;
    let repo_source_root = sandbox_base.parent()?;
    Some((repo_source_root.to_path_buf(), sandbox_base.to_path_buf()))
}

/// Shared isolation fence: reject `candidate` if it resolves into the live repo
/// source tree (`repo_source_root`) while allowing the scope sandbox
/// (`sandbox_base`, which legitimately nests inside the repo) and genuine
/// external dirs. Only fires when `repo_source_root` is a real git repo. Shared
/// by `resolve_coding_repo_binding` (repo_path) and the native shell deny-fence
/// (explicit `working_dir`).
pub fn reject_repo_source_tree(
    candidate: &Path,
    repo_source_root: &Path,
    sandbox_base: &Path,
) -> std::result::Result<(), String> {
    // Defensive: never arm with a degenerate empty root — an empty path is a
    // `starts_with` prefix of EVERY path, which would reject everything. The
    // setter already absolutizes, but a caller passing `""` must fail open.
    if repo_source_root.as_os_str().is_empty() {
        return Ok(());
    }
    if repo_source_root.join(".git").exists()
        && candidate.starts_with(repo_source_root)
        && !candidate.starts_with(sandbox_base)
    {
        return Err(format!(
            "path `{}` resolves into the live magician repository source tree (`{}`), which \
             sandboxed actions are not allowed to modify. Use a project directory under the \
             scope sandbox, or a path outside this tree.",
            candidate.display(),
            repo_source_root.display()
        ));
    }
    Ok(())
}

tokio::task_local! {
    /// True while the current async task is executing a CODING run — set per
    /// agent-loop in `execute_agentically` when the agent holds a coding
    /// capability, and inherited by everything it awaits inline (its shell/file
    /// tools, `run_coding_task` → Pi, `run_project_checks` → the check runner).
    /// Read by the OS-sandbox + file-action fences so isolation arms ONLY for
    /// coding work and never touches general agents/skills. No process-global
    /// flag, no operator action.
    static CODING_RUN_CONTEXT: bool;

    /// The current coding run's REAL repo path (canonical), scoped by
    /// `run_coding_task` around the Pi turn once the binding is resolved. The
    /// OS-sandbox builder reads it to fence WRITES to the real repo read-only:
    /// Pi must only edit its shadow CWD (apply-to-real happens later, OUTSIDE
    /// this sandbox). Without this, an external/absolute repo_path stays writable
    /// (the sandbox only denies the magician repo source), so a prompt leak or a
    /// non-compliant model could edit the real repo and invert the captured diff.
    /// Unset (None) for non-coding spawns and the check/shell leaf executors.
    static CODING_RUN_REAL_REPO: PathBuf;

    /// The directories an engine turn may write: its workspace (clone or
    /// shadow), the build caches and Magician's coding sandbox root. Scoped by
    /// `run_coding_task` around the turn; when set (macOS), the OS sandbox is a
    /// write allow-list instead of allow-all-but-the-repos, and engines run
    /// with their own sandboxes off (`os_write_allowlist_will_wrap`).
    static CODING_WRITE_ROOTS: Vec<PathBuf>;
}

/// Whether the current task is inside a coding run (see `CODING_RUN_CONTEXT`).
/// Defaults to `false` outside any coding scope — so general agents/skills are
/// never sandboxed.
pub fn coding_context_active() -> bool {
    CODING_RUN_CONTEXT
        .try_with(|active| *active)
        .unwrap_or(false)
}

/// Run `fut` with the coding-run context flag set. `execute_agentically` scopes
/// every agent loop with its agent's coding-ness; the flag then propagates to
/// the shared leaf executors that agent's tools reach.
pub async fn with_coding_context<F>(active: bool, fut: F) -> F::Output
where
    F: std::future::Future,
{
    CODING_RUN_CONTEXT.scope(active, fut).await
}

/// The current coding run's real repo path, if a coding turn scoped it (see
/// `CODING_RUN_REAL_REPO`). Read by the OS-sandbox builder to fence the real repo.
fn coding_run_real_repo() -> Option<PathBuf> {
    CODING_RUN_REAL_REPO.try_with(|path| path.clone()).ok()
}

/// Run `fut` with the coding run's real repo path scoped, so the OS sandbox fences
/// writes to it. `run_coding_task` wraps the Pi turn with this once the repo
/// binding is resolved; the path propagates to the inline Pi command build.
pub async fn with_coding_real_repo<F>(real_repo: PathBuf, fut: F) -> F::Output
where
    F: std::future::Future,
{
    CODING_RUN_REAL_REPO.scope(real_repo, fut).await
}

fn coding_write_roots() -> Option<Vec<PathBuf>> {
    CODING_WRITE_ROOTS.try_with(|roots| roots.clone()).ok()
}

/// Run `fut` with the turn's write roots scoped (see `CODING_WRITE_ROOTS`).
pub async fn with_coding_write_roots<F>(roots: Vec<PathBuf>, fut: F) -> F::Output
where
    F: std::future::Future,
{
    CODING_WRITE_ROOTS.scope(roots, fut).await
}

/// Whether a spawn now gets the write allow-list profile: macOS or Linux, the
/// coding sandbox armed, the turn's write roots scoped, and
/// `coding.sandbox_write_allowlist` on. Engines that bring their own sandbox
/// (Codex, Grok, Antigravity) turn it off exactly then: macOS refuses a nested
/// `sandbox_apply` inside a restrictive profile, and this profile (or the
/// Linux `bwrap` mount set) fences them.
pub fn os_write_allowlist_will_wrap() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
        && os_sandbox_will_wrap()
        && coding_write_roots().is_some()
        && budgets::coding_budget_settings().sandbox_write_allowlist
}

/// Home-relative paths an engine writes its own state to (sessions, logs,
/// auth refresh), measured 2026-10-09. A trailing `*` is a prefix
/// (`~/.claude.json*` covers its `.backup` / `.lock` siblings).
pub fn engine_state_paths(engine: CodingEngineKind, home: &Path) -> Vec<PathBuf> {
    let rel: &[&str] = match engine {
        CodingEngineKind::Pi => &[".pi"],
        CodingEngineKind::ClaudeCode => &[".claude", ".claude.json*"],
        CodingEngineKind::CodexAppServer => &[".codex"],
        CodingEngineKind::AgyCli => &[".gemini"],
        CodingEngineKind::GrokAcp => &[".grok"],
        #[allow(unreachable_patterns)]
        _ => &[],
    };
    rel.iter().map(|rel| home.join(rel)).collect()
}

/// Package-manager caches every engine's build and test commands write.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
const TOOL_CACHE_DIRS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".rustup",
    ".npm",
    ".cache",
    "Library/Caches",
    ".gradle",
    ".m2",
    ".hex",
    ".mix",
    "go",
    ".pnpm-store",
    ".yarn",
    ".bun",
];

/// Whether the OS filesystem sandbox launcher actually works on this host.
/// Probed ONCE (cached). Lets the sandbox default **ON** for coding spawns while
/// failing OPEN at the system level if the launcher is missing or broken (e.g.
/// `bwrap` where user namespaces are disabled, or `sandbox-exec` rejecting a
/// trivial profile) — so a broken launcher never breaks coding runs. Force-OFF
/// with `MAGICIAN_CODING_OS_SANDBOX=0` (kill-switch); any other value / unset
/// leaves it probe-decided.
pub fn os_sandbox_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if matches!(
            std::env::var("MAGICIAN_CODING_OS_SANDBOX")
                .ok()
                .as_deref()
                .map(str::trim),
            Some("0") | Some("false") | Some("no") | Some("off")
        ) {
            return false;
        }
        os_sandbox_probe()
    })
}

/// Outer Magician coding fence: a live coding context, a working OS sandbox,
/// and a live-repo source fence. Codex and Grok both require this; the Pi
/// unsandboxed fallback is not allowed for those engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingFenceError {
    Required,
    /// A Pi coding turn would spawn without the OS filesystem sandbox.
    PiSandboxUnavailable,
}

impl std::fmt::Display for CodingFenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Required => write!(
                f,
                "coding dispatch requires the Magician outer coding fence; the Pi unsandboxed fallback is not allowed"
            ),
            Self::PiSandboxUnavailable => write!(
                f,
                "Pi coding turns require the OS filesystem sandbox (sandbox-exec on macOS, bwrap \
                 on Linux) and it is unavailable or switched off (MAGICIAN_CODING_OS_SANDBOX=0). \
                 Where the host itself is the sandbox, set `coding.pi_allow_unsandboxed: true`."
            ),
        }
    }
}

impl std::error::Error for CodingFenceError {}

/// Whether [`os_sandbox_command`] will actually wrap a command spawned now:
/// a coding context, a working launcher, and a supported platform. Unlike
/// [`require_outer_fence`] this needs no live-repo fence — the sandbox's
/// real-repo and credential rules apply on a packaged install too.
pub fn os_sandbox_will_wrap() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
        && coding_context_active()
        && os_sandbox_available()
}

/// The gate a Pi coding turn passes before it spawns. Pi used to be the one
/// engine that ran unsandboxed when the sandbox could not start; it now fails
/// closed like the others unless the operator opted out
/// (`coding.pi_allow_unsandboxed`) on a host that is its own sandbox.
pub fn require_pi_sandbox(allow_unsandboxed: bool) -> Result<(), CodingFenceError> {
    if allow_unsandboxed || os_sandbox_will_wrap() {
        Ok(())
    } else {
        Err(CodingFenceError::PiSandboxUnavailable)
    }
}

pub fn require_outer_fence() -> Result<(), CodingFenceError> {
    if coding_context_active() && os_sandbox_available() && live_repo_source_fence().is_some() {
        Ok(())
    } else {
        Err(CodingFenceError::Required)
    }
}

/// One-shot self-test: launch the platform sandbox against a no-op command under
/// a trivial allow-all profile. Returns true iff the launcher exists and exits 0.
fn os_sandbox_probe() -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::process::Stdio;
        const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
        if !Path::new(SANDBOX_EXEC).exists() {
            return false;
        }
        let ok = std::process::Command::new(SANDBOX_EXEC)
            .arg("-p")
            .arg("(version 1)(allow default)")
            .arg("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            tracing::warn!(
                "OS coding sandbox self-test failed (sandbox-exec) — coding runs run unsandboxed"
            );
        }
        return ok;
    }
    #[cfg(target_os = "linux")]
    {
        let Some(bwrap) = trusted_bwrap() else {
            tracing::warn!(
                "OS coding sandbox unavailable: no root-owned `bwrap` in /usr/bin, /bin or \
                 /usr/local/bin — coding engines fail closed"
            );
            return false;
        };
        let ok = bwrap_confines_writes(bwrap);
        if !ok {
            tracing::warn!(
                bwrap = %bwrap.display(),
                "OS coding sandbox self-test failed (bwrap could not confine a write; user \
                 namespaces may be disabled) — coding engines fail closed"
            );
        }
        return ok;
    }
    #[allow(unreachable_code)]
    false
}

/// The `bwrap` launcher, found once in fixed system directories (never on
/// `PATH`, which a coding turn may be able to write) and accepted only when
/// it and its directory are root-owned and not group- or world-writable.
#[cfg(target_os = "linux")]
fn trusted_bwrap() -> Option<&'static Path> {
    use std::os::unix::fs::MetadataExt;
    static BWRAP: OnceLock<Option<PathBuf>> = OnceLock::new();
    let root_owned = |path: &Path| {
        std::fs::metadata(path)
            .map(|meta| meta.uid() == 0 && meta.mode() & 0o022 == 0)
            .unwrap_or(false)
    };
    BWRAP
        .get_or_init(|| {
            ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
                .iter()
                .map(PathBuf::from)
                .find(|path| {
                    path.is_file()
                        && root_owned(path)
                        && path.parent().map(root_owned).unwrap_or(false)
                })
        })
        .as_deref()
}

/// The Linux self-test: the production argv ([`bwrap_args`]) for a scratch
/// layout must let a write into the listed root through, refuse one beside
/// it, and give the child a working `/proc`. Proves confinement, not just
/// that `bwrap` starts.
#[cfg(target_os = "linux")]
fn bwrap_confines_writes(bwrap: &Path) -> bool {
    use std::process::Stdio;
    let Ok(scratch) = tempfile::tempdir() else {
        return false;
    };
    let root = scratch
        .path()
        .canonicalize()
        .unwrap_or_else(|_| scratch.path().to_path_buf());
    let allowed = root.join("allowed");
    let denied = root.join("denied");
    if std::fs::create_dir_all(&allowed).is_err() || std::fs::create_dir_all(&denied).is_err() {
        return false;
    }
    let script = format!(
        "echo ok > '{}/ok' && {{ echo no > '{}/no'; }} 2>/dev/null; test -r /proc/self/status",
        allowed.display(),
        denied.display()
    );
    let policy = BwrapPolicy {
        write_allow: Some(vec![allowed.clone()]),
        ..BwrapPolicy::default()
    };
    let argv = bwrap_args(
        &policy,
        OsStr::new("/bin/sh"),
        &[OsString::from("-c"), OsString::from(script)],
    );
    let ran = std::process::Command::new(bwrap)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    ran && allowed.join("ok").is_file() && !denied.join("no").exists()
}

/// What one Linux coding spawn may write and see, rendered by [`bwrap_args`].
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default, Clone)]
struct BwrapPolicy {
    /// `Some`: the write allow-list — only these roots are writable (an entry
    /// ending in `*` binds just the file before the `*`; bwrap has no
    /// prefixes). `None`: every path the user can write stays writable but
    /// the fences below.
    write_allow: Option<Vec<PathBuf>>,
    /// The live repo source and its nested storage base.
    live_fence: Option<(PathBuf, PathBuf)>,
    /// The run's real repo, read-only whatever else is allowed.
    deny_real_repo: Option<PathBuf>,
    /// Directories replaced by an empty read-only tmpfs (vault folders).
    hide_dirs: Vec<PathBuf>,
    /// Files replaced by `/dev/null` (the session bus socket, a keyring
    /// password file).
    hide_files: Vec<PathBuf>,
    /// The agents' `file_sandbox.read_only_roots`: read-only whatever else is
    /// allowed, and a separate mount, so a hard link out of one fails
    /// (EXDEV) and cannot give a program a writable second name. A root
    /// missing at spawn is skipped (`--ro-bind-try`): one created during the
    /// spawn is not fenced until the next.
    read_only: Vec<PathBuf>,
    /// Without a write allow-list, the writable roots re-opened inside a
    /// read-only root: the agents' allowed roots, the shell's working dirs,
    /// the task root. With one, its roots are used instead.
    legacy_writable: Vec<PathBuf>,
    /// The goal workspace mounts for the acting agent
    /// (`goal_workspace_access::bwrap_args_with`), after every other mount.
    workspace_args: Vec<OsString>,
    /// The member workspaces those mounts bind back from the host: the
    /// layering, the real repo and the hidden credentials inside them are
    /// mounted again after them.
    workspace_reopened: Vec<PathBuf>,
    /// Those of them opened only to read, through a delegator
    /// (`WorkspaceDenies::read_only_dirs`): no layer made again inside one is
    /// writable.
    workspace_read_only: Vec<PathBuf>,
}

/// The `bwrap` argv for one spawn. Pure, so the mount order — the contract,
/// as SBPL's rule order is on macOS — is unit-tested on every platform.
///
/// bwrap applies mounts in order and a later mount over a path hides earlier
/// ones beneath it, so: the whole tree read-only (allow-list) or writable
/// (legacy); `/dev` and a fresh `/proc` (which needs its own PID namespace
/// without setuid bwrap); the write roots, parents before children; the
/// agents' read-only roots, with the writable roots inside them re-bound;
/// the live source read-only with the roots inside its storage base
/// re-opened, then the read-only layering inside that base again; the real
/// repo read-only; the hidden credentials last. The network is shared
/// on purpose. A missing write root is skipped (`--bind-try`), never fatal.
#[cfg(any(target_os = "linux", test))]
fn bwrap_args(policy: &BwrapPolicy, program: &OsStr, args: &[OsString]) -> Vec<OsString> {
    fn push(out: &mut Vec<OsString>, flag: &str, paths: &[&Path]) {
        out.push(OsString::from(flag));
        out.extend(paths.iter().map(|path| path.as_os_str().to_os_string()));
    }
    fn bind_root(out: &mut Vec<OsString>, root: &Path) {
        let text = root.to_string_lossy();
        let target = match text.strip_suffix('*') {
            Some(prefix) => PathBuf::from(prefix),
            None => root.to_path_buf(),
        };
        push(out, "--bind-try", &[&target, &target]);
    }
    fn layer(out: &mut Vec<OsString>, layers: &[(PathBuf, bool)]) {
        for (root, read_only) in layers {
            if *read_only {
                push(out, "--ro-bind-try", &[root, root]);
            } else {
                bind_root(out, root);
            }
        }
    }
    let layers = super::shell_box::read_only_layers(
        &policy.read_only,
        policy
            .write_allow
            .as_deref()
            .unwrap_or(&policy.legacy_writable),
    );
    let mut out: Vec<OsString> = vec!["--die-with-parent".into(), "--unshare-pid".into()];
    match &policy.write_allow {
        Some(roots) => {
            push(&mut out, "--ro-bind", &[Path::new("/"), Path::new("/")]);
            push(&mut out, "--dev", &[Path::new("/dev")]);
            push(&mut out, "--proc", &[Path::new("/proc")]);
            let mut roots: Vec<&PathBuf> = roots.iter().collect();
            roots.sort_by_key(|root| (root.components().count(), root.as_os_str().to_owned()));
            for root in &roots {
                bind_root(&mut out, root);
            }
            // Read-only roots after the write roots, writable roots inside
            // them re-bound after; all before the live-fence and real-repo
            // mounts, which no re-bind may undo.
            layer(&mut out, &layers);
            if let Some((source, base)) = &policy.live_fence {
                push(&mut out, "--ro-bind", &[source, source]);
                for root in roots
                    .iter()
                    .filter(|root| root.starts_with(source) && root.starts_with(base))
                {
                    bind_root(&mut out, root);
                }
            }
        },
        None => {
            push(&mut out, "--bind", &[Path::new("/"), Path::new("/")]);
            push(&mut out, "--dev", &[Path::new("/dev")]);
            push(&mut out, "--proc", &[Path::new("/proc")]);
            layer(&mut out, &layers);
            if let Some((source, base)) = &policy.live_fence {
                push(&mut out, "--ro-bind", &[source, source]);
                push(&mut out, "--bind", &[base, base]);
            }
        },
    }
    // The live fence's base mount hid the layering inside the base: it
    // applies again there.
    if let Some((_, base)) = &policy.live_fence {
        layer(
            &mut out,
            &super::shell_box::read_only_layers_within(&layers, base),
        );
    }
    if let Some(real) = &policy.deny_real_repo {
        push(&mut out, "--ro-bind", &[real, real]);
    }
    for dir in &policy.hide_dirs {
        push(&mut out, "--tmpfs", &[dir]);
        push(&mut out, "--remount-ro", &[dir]);
    }
    for file in &policy.hide_files {
        push(&mut out, "--ro-bind", &[Path::new("/dev/null"), file]);
    }
    out.extend(policy.workspace_args.iter().cloned());
    // A member workspace bound back from the host dropped every mount made
    // inside it above: they apply again there.
    for member in &policy.workspace_reopened {
        let read_only_member = policy.workspace_read_only.contains(member);
        layer(
            &mut out,
            &super::shell_box::read_only_layers_within(&layers, member)
                .into_iter()
                .filter(|(root, _)| root != member)
                .map(|(root, read_only)| (root, read_only || read_only_member))
                .collect::<Vec<_>>(),
        );
        if let Some(real) = policy
            .deny_real_repo
            .as_ref()
            .filter(|real| real.starts_with(member))
        {
            push(&mut out, "--ro-bind", &[real, real]);
        }
        for dir in policy
            .hide_dirs
            .iter()
            .filter(|dir| dir.starts_with(member))
        {
            push(&mut out, "--tmpfs", &[dir]);
            push(&mut out, "--remount-ro", &[dir]);
        }
        for file in policy
            .hide_files
            .iter()
            .filter(|file| file.starts_with(member))
        {
            push(&mut out, "--ro-bind", &[Path::new("/dev/null"), file]);
        }
    }
    out.push(OsString::from("--"));
    out.push(program.to_os_string());
    out.extend(args.iter().cloned());
    out
}

/// Vault folders (`<root>/scopes/<principal>/<workspace>/secrets`) a Linux
/// coding spawn must not read; bwrap cannot match file names as SBPL does, so
/// the whole folder is hidden.
#[cfg(target_os = "linux")]
fn vault_dirs_to_hide(runtime_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Ok(principals) = std::fs::read_dir(runtime_root.join("scopes")) else {
        return dirs;
    };
    for principal in principals.flatten() {
        let Ok(workspaces) = std::fs::read_dir(principal.path()) else {
            continue;
        };
        for workspace in workspaces.flatten() {
            let secrets = workspace.path().join("secrets");
            if secrets.is_dir() {
                dirs.push(secrets.canonicalize().unwrap_or(secrets));
            }
        }
    }
    dirs
}

/// Escape a path for embedding in an SBPL `(subpath "...")` literal.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
fn sbpl_escape_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Vault files, matched by name so the profile needs no knowledge of the scope
/// root. Partitions are ciphertext; the journal is plaintext metadata. Verified
/// precise: a sibling file in the same directory stays readable.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
const VAULT_FILE_READ_DENIES: &str = concat!(
    "(deny file-read* (regex #\"/(provisioned_secrets|captured_secrets|mcp_oauth)\\.vault$\"))\n",
    "(deny file-read* (regex #\"/secret_audit\\.jsonl$\"))\n",
);

/// The SBPL profile for a coding run. Pure — no probing, no environment — so it
/// is testable as text and live under `sandbox-exec`.
///
/// SBPL is evaluated last-match-wins, so order is the contract: allow
/// everything; deny writes under the live repo source; re-allow writes under the
/// nested scope sandbox; fence the run's REAL repo read-only; then deny the two
/// credential reads no coding task performs — the keychain database directory
/// and the vault files. What is deliberately absent is `(deny process-info*)`:
/// measured 2026-09-05, it does not hide another same-uid process's environment
/// (`KERN_PROCARGS2` stays readable under it), while it breaks process tooling.
#[cfg(any(test, feature = "test-fixtures"))]
fn macos_sandbox_profile(
    live_fence: Option<(&Path, &Path)>,
    deny_real_repo: Option<&Path>,
    home: Option<&Path>,
) -> String {
    macos_sandbox_profile_with(
        live_fence,
        deny_real_repo,
        home,
        &SandboxAllowances::default(),
        None,
    )
}

/// SBPL for the read-only layering ([`super::shell_box::read_only_layers`]):
/// a read-only root denies writes and hard links out of it
/// (`ln programs/plan.md <shadow>/plan.md` would give a program a writable
/// second name); a writable root inside one is re-opened for both (a `*`
/// prefix entry for writes only). The god-mode override's unboxed shell in a
/// coding context still runs under this profile; super god mode launches
/// with no profile at all.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
fn sbpl_read_only_layers(layers: &[(PathBuf, bool)]) -> String {
    let mut rules = String::new();
    for (root, read_only) in layers {
        if *read_only {
            let root = sbpl_escape_path(root);
            rules.push_str(&format!(
                "(deny file-write* (subpath \"{root}\"))\n(deny file-link (subpath \"{root}\"))\n"
            ));
        } else {
            rules.push_str(&sbpl_write_allow(root));
            if !root.to_string_lossy().ends_with('*') {
                rules.push_str(&format!(
                    "(allow file-link (subpath \"{}\"))\n",
                    sbpl_escape_path(root)
                ));
            }
        }
    }
    rules
}

/// Whether the coding profile lets a spawn write at `path`: under the write
/// allow-list (or, without one, anywhere), with the deepest of those roots
/// and the agents' read-only roots deciding (a tie goes to read-only).
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn coding_writes_allowed(
    path: &Path,
    write_allow: Option<&[PathBuf]>,
    read_only: &[PathBuf],
    legacy_writable: &[PathBuf],
) -> bool {
    let deepest = |roots: &[PathBuf]| {
        roots
            .iter()
            .map(|root| {
                let text = root.to_string_lossy();
                PathBuf::from(text.strip_suffix('*').unwrap_or(&text))
            })
            .filter(|root| path.starts_with(root))
            .map(|root| root.components().count())
            .max()
    };
    let read_only_depth = deepest(read_only);
    match write_allow {
        Some(list) => match (deepest(list), read_only_depth) {
            (Some(writable), Some(read_only)) => writable > read_only,
            (Some(_), None) => true,
            _ => false,
        },
        None => match read_only_depth {
            None => true,
            Some(read_only) => {
                deepest(legacy_writable).is_some_and(|writable| writable > read_only)
            },
        },
    }
}

/// Escape a path for an SBPL `#"..."` regex literal.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
fn sbpl_escape_regex(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        if "\\.+*?()[]{}^$|\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One write allow rule: a directory subtree, or a prefix for an entry that
/// ends in `*`.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
fn sbpl_write_allow(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_suffix('*') {
        Some(prefix) => format!(
            "(allow file-write* (regex #\"^{}\"))\n",
            sbpl_escape_regex(Path::new(prefix))
        ),
        None => format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape_path(path)
        ),
    }
}

/// What one engine's spawn may do beyond the default coding sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxAllowances {
    /// Read `~/Library/Keychains`. Claude Code and Antigravity keep their
    /// login there on macOS; with the read denied they start logged out
    /// ("Not logged in", a browser OAuth prompt). Reading the database gives
    /// ciphertext only: items still decrypt through `securityd` under each
    /// item's own access list. Owner decision 2026-10-09: per engine.
    pub keychain_read: bool,
    /// The engine's own state paths ([`engine_state_paths`]), writable under
    /// the write allow-list profile.
    pub state_paths: Vec<PathBuf>,
}

#[cfg(any(test, feature = "test-fixtures"))]
fn macos_sandbox_profile_with(
    live_fence: Option<(&Path, &Path)>,
    deny_real_repo: Option<&Path>,
    home: Option<&Path>,
    allowances: &SandboxAllowances,
    write_allow: Option<&[PathBuf]>,
) -> String {
    macos_sandbox_profile_layered(
        live_fence,
        deny_real_repo,
        home,
        allowances,
        write_allow,
        &[],
        &[],
    )
}

/// [`macos_sandbox_profile_with`] with the agents' `read_only_roots`. The
/// writable roots re-opened inside one are the write allow-list's, or
/// without one `legacy_writable` (the agents' allowed roots, the shell's
/// working dirs, the task root). The layering sits after the write allows
/// and before the live-fence and real-repo denies, so no re-open undoes
/// them; it is emitted again inside the live fence's storage base after the
/// base's carve-out, which would otherwise re-open read-only roots there.
#[cfg(any(target_os = "macos", test, feature = "test-fixtures"))]
fn macos_sandbox_profile_layered(
    live_fence: Option<(&Path, &Path)>,
    deny_real_repo: Option<&Path>,
    home: Option<&Path>,
    allowances: &SandboxAllowances,
    write_allow: Option<&[PathBuf]>,
    read_only: &[PathBuf],
    legacy_writable: &[PathBuf],
) -> String {
    let layers =
        super::shell_box::read_only_layers(read_only, write_allow.unwrap_or(legacy_writable));
    let mut profile = String::from("(version 1)\n(allow default)\n");
    // Write allow-list mode: no writes except the listed roots and the
    // devices every process needs. The live repo source is denied again after
    // the roots, so a listed ancestor (an owner-allowed `~/Development`)
    // cannot reopen it; only roots inside its storage base are re-allowed.
    // The real-repo deny below stays last.
    if let Some(roots) = write_allow {
        profile.push_str("(deny file-write*)\n");
        profile.push_str(
            "(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") \
             (regex #\"^/dev/(tty|ttys[0-9]+|fd/[0-9]+|ptmx|dtracehelper)\"))\n",
        );
        for root in roots {
            profile.push_str(&sbpl_write_allow(root));
        }
    }
    profile.push_str(&sbpl_read_only_layers(&layers));
    if let Some(roots) = write_allow {
        if let Some((repo_source_root, sandbox_base)) = live_fence {
            profile.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                sbpl_escape_path(repo_source_root),
            ));
            for root in roots
                .iter()
                .filter(|root| root.starts_with(repo_source_root) && root.starts_with(sandbox_base))
            {
                profile.push_str(&sbpl_write_allow(root));
            }
        }
    }
    // Only a source checkout has a live repo to protect; a packaged install
    // still gets the real-repo and credential rules below.
    if let Some((repo_source_root, sandbox_base)) = live_fence.filter(|_| write_allow.is_none()) {
        profile.push_str(&format!(
            "(deny file-write* (subpath \"{}\"))\n(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape_path(repo_source_root),
            sbpl_escape_path(sandbox_base),
        ));
    }
    // The live fence's base carve-out re-opened its whole base: the
    // layering inside the base applies again.
    if let Some((_, sandbox_base)) = live_fence {
        profile.push_str(&sbpl_read_only_layers(
            &super::shell_box::read_only_layers_within(&layers, sandbox_base),
        ));
    }
    // Trailing rule (last-match-wins) — fence the run's REAL repo read-only. This
    // overrides the broad `allow … sandbox_base` for an in-workspace repo AND the
    // initial `allow default` for an external/absolute repo, so Pi physically
    // cannot write the real repo and is forced to edit its shadow CWD.
    if let Some(real) = deny_real_repo {
        profile.push_str(&format!(
            "(deny file-write* (subpath \"{}\"))\n",
            sbpl_escape_path(real),
        ));
    }
    if let Some(home) = home.filter(|_| !allowances.keychain_read) {
        profile.push_str(&format!(
            "(deny file-read* (subpath \"{}\"))\n",
            sbpl_escape_path(&home.join("Library").join("Keychains")),
        ));
    }
    profile.push_str(VAULT_FILE_READ_DENIES);
    profile
}

/// Everything a turn under the write allow-list may write: its roots (the
/// workspace, build caches, the engine's session folder), the temp areas,
/// package-manager caches and the engine's own state. Each path goes through
/// `resolve` (SBPL and bwrap both act on resolved paths).
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn write_allow_list(
    roots: Vec<PathBuf>,
    home: Option<&Path>,
    state_paths: &[PathBuf],
    resolve: &dyn Fn(&Path) -> PathBuf,
) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = roots.iter().map(|root| resolve(root)).collect();
    list.push(temp_write_root(&resolve(&std::env::temp_dir())));
    if cfg!(target_os = "macos") {
        list.push(PathBuf::from("/private/tmp"));
    } else {
        list.push(PathBuf::from("/tmp"));
        list.push(PathBuf::from("/var/tmp"));
    }
    if let Some(home) = home {
        list.extend(TOOL_CACHE_DIRS.iter().map(|rel| home.join(rel)));
    }
    for path in state_paths {
        let text = path.to_string_lossy();
        match text.strip_suffix('*') {
            // A prefix entry keeps its `*`; resolve the directory part.
            Some(prefix) => {
                let prefix = Path::new(prefix);
                let resolved = match (prefix.parent(), prefix.file_name()) {
                    (Some(parent), Some(name)) => resolve(parent).join(name),
                    _ => prefix.to_path_buf(),
                };
                list.push(PathBuf::from(format!("{}*", resolved.display())));
            },
            None => list.push(resolve(path)),
        }
    }
    list.sort();
    list.dedup();
    list
}

/// The writable temp root for a resolved `$TMPDIR`. macOS's per-user
/// `/private/var/folders/<a>/<b>/T` opens its parent too, which holds the `C`
/// cache folder tools use. Any other temp dir is opened alone: the parent of
/// `/tmp` is `/`, and of a custom `~/tmp` the whole home.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn temp_write_root(tmp: &Path) -> PathBuf {
    let parts: Vec<_> = tmp.components().collect();
    let per_user_macos =
        parts.len() == 7 && tmp.starts_with("/private/var/folders") && parts[6].as_os_str() == "T";
    match tmp.parent().filter(|_| per_user_macos) {
        Some(parent) => parent.to_path_buf(),
        None => tmp.to_path_buf(),
    }
}

/// THE GATE — wrap a spawned command in an OS-level filesystem sandbox so the
/// live repo source tree is READ-ONLY while the scope sandbox (`magician_data_v3`,
/// nested in the repo) and the rest of the filesystem stay writable, regardless
/// of how the command phrases its writes (absolute path / `cd` / inherited-CWD
/// relative write — none of which the declarative layers can stop). macOS
/// `sandbox-exec` (SBPL), Linux `bwrap`.
///
/// Arms automatically for coding work: wraps iff the current task is in a coding
/// context (`coding_context_active()` — a coding agent's tool, or the coding
/// engine's Pi / check-runner spawn) AND the launcher self-test passed
/// (`os_sandbox_available()`, default-on, kill-switch `MAGICIAN_CODING_OS_SANDBOX=0`).
/// General agents/skills (no coding context) always pass through, so normal flows
/// are byte-equivalent to pre-isolation. Context is checked FIRST so general
/// spawns short-circuit before the one-time launcher probe.
///
/// Returns the original command unchanged (FAIL OPEN) when not in a coding
/// context, the launcher self-test failed/was killed, or the platform is
/// unsupported. Without a source checkout (a packaged install) it still wraps:
/// the live-repo rules are dropped, the real-repo and credential rules stay.
/// Fail-open is not the last word for Pi: `require_pi_sandbox` refuses a Pi
/// coding turn that would spawn unwrapped. NOTE:
/// once a launcher is wrapped, a *runtime* launcher failure the self-test didn't
/// catch (e.g. a path-specific SBPL rejection) is fail-CLOSED — the command
/// errors rather than silently running unsandboxed.
#[cfg(any(test, feature = "test-fixtures"))]
fn os_sandbox_wrap(program: &OsStr, args: &[OsString]) -> (OsString, Vec<OsString>) {
    os_sandbox_wrap_with(program, args, SandboxAllowances::default())
}

fn os_sandbox_wrap_with(
    program: &OsStr,
    args: &[OsString],
    allowances: SandboxAllowances,
) -> (OsString, Vec<OsString>) {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = allowances;
    let passthrough = || (program.to_os_string(), args.to_vec());

    if !(coding_context_active() && os_sandbox_available()) {
        return passthrough();
    }
    // `None` on a packaged install (no source checkout). The sandbox still
    // arms: the run's real repo stays read-only and credentials unreadable.
    let live_fence = live_repo_source_fence();
    // Tier-1 airtight fence: the current coding run's REAL repo (when scoped) is held
    // read-only for the duration of the run — for BOTH external and in-workspace repos
    // (the trailing deny below overrides the broad `allow sandbox_base` for the latter).
    // `None` for non-coding spawns (general agents/skills, the check/shell leaf
    // executors) and for the degenerate shadow-ancestor case the scope site skips.
    let deny_real_repo = coding_run_real_repo();
    // On targets without a supported launcher both cfg blocks below are stripped,
    // leaving these bindings unused; discard them so a cross-target
    // `clippy -D warnings` run (CI windows matrix) doesn't error on
    // `unused_variables`. No-op on macOS/Linux (where both are read).
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (&live_fence, &deny_real_repo);

    #[cfg(target_os = "macos")]
    {
        const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
        if !Path::new(SANDBOX_EXEC).exists() {
            tracing::warn!(
                "MAGICIAN_CODING_OS_SANDBOX is set but {} is missing — running unsandboxed",
                SANDBOX_EXEC
            );
            return passthrough();
        }
        // `macos_sandbox_profile` owns the rule order (last-match-wins) and is
        // unit-tested as text and live under `sandbox-exec`.
        // SBPL `subpath` matches the path the kernel resolves, so a rule written
        // against a symlinked prefix (`/var/folders` -> `/private/var/folders`, a
        // symlinked home) silently never matches. Resolve every fenced path
        // best-effort; a path that cannot be resolved keeps its given form.
        let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let live_fence = live_fence
            .as_ref()
            .map(|(repo, base)| (resolved(repo), resolved(base)));
        let deny_real_repo = deny_real_repo.as_deref().map(resolved);
        let home = dirs::home_dir().map(|home| resolved(&home));
        if home.is_none() {
            tracing::warn!(
                "coding sandbox: no home directory resolved — keychain read deny omitted"
            );
        }
        let write_allow = os_write_allowlist_will_wrap().then(|| {
            write_allow_list(
                coding_write_roots().unwrap_or_default(),
                home.as_deref(),
                &allowances.state_paths,
                &resolved,
            )
        });
        let read_only_roots =
            crate::magician_v2::execution::native_executors::agent_read_only_roots();
        let legacy_writable =
            crate::magician_v2::execution::native_executors::agent_writable_roots();
        let profile = macos_sandbox_profile_layered(
            live_fence
                .as_ref()
                .map(|(repo, base)| (repo.as_path(), base.as_path())),
            deny_real_repo.as_deref(),
            home.as_deref(),
            &allowances,
            write_allow.as_deref(),
            &read_only_roots,
            &legacy_writable,
        );
        // Last (last match wins): the notes hybrid index, and the goal
        // workspaces the acting agent may not reach (a member workspace is
        // re-opened for writes only where this profile allows them). No
        // re-open above undoes them.
        let profile = format!(
            "{profile}{}{}{}",
            crate::magician_v2::execution::shell_box::plane_private_rules_outside_sessions(),
            crate::magician_v2::execution::goal_workspace_access::HYBRID_INDEX_SBPL_RULE,
            crate::magician_v2::execution::goal_workspace_access::sbpl_rules_with(
                &crate::magician_v2::execution::goal_workspace_access::denies_for_current(),
                &|path| coding_writes_allowed(
                    path,
                    write_allow.as_deref(),
                    &read_only_roots,
                    &legacy_writable
                ),
            ),
        );
        let mut wrapped = Vec::with_capacity(args.len() + 3);
        wrapped.push(OsString::from("-p"));
        wrapped.push(OsString::from(profile));
        wrapped.push(program.to_os_string());
        wrapped.extend(args.iter().cloned());
        return (OsString::from(SANDBOX_EXEC), wrapped);
    }

    #[cfg(target_os = "linux")]
    {
        // `os_sandbox_available()` passed, so the self-test found and ran
        // this launcher; it is never looked up on PATH again.
        let Some(bwrap) = trusted_bwrap() else {
            return passthrough();
        };
        let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let home = dirs::home_dir().map(|home| resolved(&home));
        let write_allow = os_write_allowlist_will_wrap().then(|| {
            // bwrap cannot bind a folder that does not exist, and the engine
            // could not create it under a read-only parent: make the engine's
            // state dirs (`~/.grok` on a fresh box) and the turn's roots (an
            // owner-allowed folder not created yet) first.
            let roots = coding_write_roots().unwrap_or_default();
            for path in allowances.state_paths.iter().chain(&roots) {
                if !path.to_string_lossy().ends_with('*') && !path.exists() {
                    let _ = std::fs::create_dir_all(path);
                }
            }
            write_allow_list(roots, home.as_deref(), &allowances.state_paths, &resolved)
        });
        let mut hide_files: Vec<PathBuf> = Vec::new();
        // The desktop keyring answers any same-user process over the session
        // bus (Secret Service has no per-app access list), and Magician's
        // vault key lives there.
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            hide_files.push(PathBuf::from(runtime_dir).join("bus"));
        }
        if let Some(password_file) = std::env::var_os("MAGICIAN_KEYRING_PASSWORD_FILE") {
            hide_files.push(PathBuf::from(password_file));
        }
        hide_files.retain(|path| path.exists());
        // The notes hybrid index, and the goal workspaces the acting agent
        // may not reach (mounted last, `goal_workspace_access`).
        let mut hide_dirs =
            vault_dirs_to_hide(&crate::magician_v2::process_storage::runtime_root());
        hide_dirs.extend(
            crate::magician_v2::execution::goal_workspace_access::hybrid_index_dirs(
                &crate::magician_v2::process_storage::runtime_root(),
            ),
        );
        // Plane harness sessions' private folders (grant files, homes).
        let plane_private =
            crate::magician_v2::execution::shell_box::plane_private_paths_outside_sessions();
        hide_dirs.extend(plane_private.iter().filter(|path| path.is_dir()).cloned());
        hide_files.extend(plane_private.into_iter().filter(|path| path.is_file()));
        let read_only = crate::magician_v2::execution::native_executors::agent_read_only_roots();
        let legacy_writable =
            crate::magician_v2::execution::native_executors::agent_writable_roots();
        let workspace_denies =
            crate::magician_v2::execution::goal_workspace_access::denies_for_current();
        let workspace_args = crate::magician_v2::execution::goal_workspace_access::bwrap_args_with(
            &workspace_denies,
            &|path| {
                coding_writes_allowed(path, write_allow.as_deref(), &read_only, &legacy_writable)
            },
        );
        let policy = BwrapPolicy {
            write_allow,
            live_fence: live_fence
                .as_ref()
                .map(|(repo, base)| (resolved(repo), resolved(base))),
            // Tier-1 airtight fence: the run's REAL repo stays read-only even
            // inside a writable root.
            deny_real_repo: deny_real_repo.as_deref().map(resolved),
            hide_dirs,
            hide_files: hide_files.iter().map(|path| resolved(path)).collect(),
            read_only,
            legacy_writable,
            workspace_args,
            workspace_reopened: workspace_denies.reopened.clone(),
            workspace_read_only: workspace_denies.read_only_dirs.clone(),
        };
        return (
            bwrap.as_os_str().to_os_string(),
            bwrap_args(&policy, program, args),
        );
    }

    #[allow(unreachable_code)]
    passthrough()
}

/// Build a `tokio::process::Command` for `(program, args)`, transparently
/// wrapping it in the OS sandbox gate when enabled (see [`os_sandbox_wrap`]).
/// Callers add their own cwd / env / stdio afterwards — those apply to the
/// wrapper process and propagate to the sandboxed child. When the gate is off
/// (default) this is identical to `Command::new(program).args(args)`.
pub fn os_sandbox_command(program: &OsStr, args: &[OsString]) -> tokio::process::Command {
    os_sandbox_command_with(program, args, SandboxAllowances::default())
}

/// [`os_sandbox_command`] for an engine that needs a [`SandboxAllowances`]
/// exception (only ever narrower than no sandbox at all).
pub fn os_sandbox_command_with(
    program: &OsStr,
    args: &[OsString],
    allowances: SandboxAllowances,
) -> tokio::process::Command {
    let (resolved_program, resolved_args) = os_sandbox_wrap_with(program, args, allowances);
    #[cfg(target_os = "linux")]
    let wrapped = resolved_program.as_os_str() != program;
    let mut cmd = tokio::process::Command::new(resolved_program);
    cmd.args(resolved_args);
    // The bus socket is hidden in the sandbox; without its address a client
    // also cannot reach an abstract-socket bus.
    #[cfg(target_os = "linux")]
    if wrapped {
        cmd.env_remove("DBUS_SESSION_BUS_ADDRESS");
    }
    cmd
}

/// How a VibeDev coding turn's engine process is fenced. Scoped by
/// `run_coding_task` around the turn; every other launch (plane harness turns,
/// qualification) keeps today's coding profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessFence {
    /// Magician's box around the whole engine process
    /// (`shell_box::wrap_harness`): writes only to its workspace copy and its
    /// own state, secrets unreadable, other agent CLIs blocked, network by
    /// `execution.network_policy`.
    Box,
    /// The coding profile alone ([`os_sandbox_wrap`]): the live repo source
    /// and the real repo read-only. `coding.box_harness: false`.
    Profile,
    /// Super god mode (debug): no Magician fence at all, and the engine's own
    /// sandbox and tool restrictions dropped.
    Unrestricted,
}

tokio::task_local! {
    static HARNESS_FENCE: HarnessFence;
}

/// Run `fut` with `fence` as the fence of every engine process it launches.
pub async fn with_harness_fence<F>(fence: HarnessFence, fut: F) -> F::Output
where
    F: std::future::Future,
{
    HARNESS_FENCE.scope(fence, fut).await
}

fn harness_fence() -> HarnessFence {
    HARNESS_FENCE
        .try_with(|fence| *fence)
        .unwrap_or(HarnessFence::Profile)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarnessWrap {
    Plain,
    Profile,
    Box,
}

fn harness_wrap() -> HarnessWrap {
    if harness_unrestricted() || !(coding_context_active() && os_sandbox_available()) {
        return HarnessWrap::Plain;
    }
    if harness_fence() == HarnessFence::Box && super::shell_box::available() {
        return HarnessWrap::Box;
    }
    if live_repo_source_fence().is_some() {
        HarnessWrap::Profile
    } else {
        HarnessWrap::Plain
    }
}

/// This turn's engine launches with no restriction (super god mode).
/// This turn's engine runs inside Magician's box (not the coding profile
/// alone, and not unrestricted).
pub fn harness_boxed() -> bool {
    harness_wrap() == HarnessWrap::Box
}

pub fn harness_unrestricted() -> bool {
    harness_fence() == HarnessFence::Unrestricted
}

/// The engine's own sandbox is off for this turn. Either it runs inside a
/// Magician Seatbelt sandbox (box or coding profile), where its own cannot
/// apply: macOS refuses a nested `sandbox_apply` (measured 2026-10-14: Codex's
/// `workspace-write` commands exit 71 and Grok's `--sandbox workspace` refuses
/// to start), so Magician's fence stands in. Or super god mode lifts every
/// fence. Codex then runs `danger-full-access` and Grok `--sandbox off`.
/// The write allow-list ([`os_write_allowlist_will_wrap`]) is such a fence on
/// Linux too. VibeDev engines only: a plane Codex decides its own sandbox
/// from its session's approval (`codex::own_sandbox_off`), never from this.
pub fn harness_own_sandbox_off() -> bool {
    harness_unrestricted()
        || os_write_allowlist_will_wrap()
        || (cfg!(target_os = "macos") && harness_wrap() != HarnessWrap::Plain)
}

/// The engine's own state a boxed engine may write, by engine: config,
/// sessions, auth refresh, caches. Home-relative.
fn harness_state_dirs(engine: CodingEngineKind) -> &'static [&'static str] {
    match engine {
        CodingEngineKind::ClaudeCode => &[
            ".claude",
            ".config/claude",
            ".local/state/claude",
            ".local/share/claude",
        ],
        CodingEngineKind::CodexAppServer => &[".codex"],
        CodingEngineKind::Pi => &[".pi"],
        // None: the owner's `~/.grok` holds config, hooks and plugins their
        // own unboxed Grok runs. Only its sign-in is writable (`harness_box`),
        // and Magician's Grok home only in its runtime subdirectories.
        CodingEngineKind::GrokAcp => &[],
        CodingEngineKind::AgyCli => &[
            ".gemini",
            ".antigravity",
            "Library/Application Support/Antigravity",
        ],
    }
}

/// Env a boxed engine gets: no self-update or non-essential traffic (its
/// install directory is not writable in the box, and nothing it does there
/// is the owner's work).
fn harness_box_env(engine: CodingEngineKind) -> &'static [(&'static str, &'static str)] {
    match engine {
        CodingEngineKind::ClaudeCode => &[
            ("DISABLE_AUTOUPDATER", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ],
        CodingEngineKind::Pi => &[("PI_SKIP_VERSION_CHECK", "1"), ("PI_TELEMETRY", "0")],
        _ => &[],
    }
}

/// Engines whose sign-in lives in the login keychain, which their box must
/// leave readable. Antigravity's `ChainedAuth` uses the OS keyring; unreadable,
/// its token save timed out after 5 s (`Keyring SaveToken timed out`) and
/// macOS asked the owner about a keychain it could not find (2026-10-15).
fn keychain_signin(engine: CodingEngineKind) -> bool {
    matches!(
        engine,
        CodingEngineKind::ClaudeCode | CodingEngineKind::AgyCli
    )
}

/// The box for `engine`'s process on this turn. `read_only`: a discuss turn,
/// whose workspace copy stays unwritable (the engine's own read-only sandbox
/// cannot nest inside this one).
fn harness_box(
    engine: CodingEngineKind,
    binary: &Path,
    request: &CodingEngineRequest,
    read_only: bool,
) -> super::shell_box::HarnessBox {
    let home = dirs::home_dir();
    let mut writable = Vec::new();
    if !read_only {
        writable.push(request.shadow_workspace_root.clone());
        writable.extend(request.working_dir.clone());
    }
    if let Some(home) = &home {
        writable.extend(harness_state_dirs(engine).iter().map(|dir| home.join(dir)));
    }
    // The turn's write roots: the shared build caches and the folders the
    // owner allowed. A Discuss turn's roots already leave out the workspace.
    writable.extend(coding_write_roots().unwrap_or_default());
    let mut writable_regex = Vec::new();
    match engine {
        CodingEngineKind::Pi => {
            writable.push(request.scope_root.join("coding_engine").join("pi_sessions"));
        },
        CodingEngineKind::GrokAcp => {
            // The same home `grok.rs` prepares and points `GROK_HOME` at. Its
            // root, config files, hooks and plugins stay read-only, so a turn
            // cannot plant a hook the next session (or the unboxed
            // qualification probe) would run. Resolved first: the runtime
            // root may sit behind a symlink, and SBPL matches the path the
            // kernel resolves.
            let grok_home = grok_contract::magician_grok_home_dir();
            let grok_home = grok_home.canonicalize().unwrap_or(grok_home);
            writable.extend(
                grok_contract::GROK_HOME_WRITABLE_DIRS
                    .iter()
                    .map(|dir| grok_home.join(dir)),
            );
            writable_regex.push(grok_contract::grok_home_writable_regex(&grok_home));
            // Token refreshes go through the `auth.json` link to the owner's
            // sign-in, and its lock beside it.
            if let Some(owner) = grok_contract::owner_grok_home() {
                let owner = owner.canonicalize().unwrap_or(owner);
                writable_regex.push(grok_contract::owner_auth_writable_regex(&owner));
            }
        },
        CodingEngineKind::CodexAppServer => {
            if let Some(codex_home) = request
                .env
                .get("CODEX_HOME")
                .cloned()
                .or_else(|| std::env::var("CODEX_HOME").ok())
                .filter(|value| !value.trim().is_empty())
            {
                writable.push(PathBuf::from(codex_home));
            }
        },
        CodingEngineKind::ClaudeCode => {
            if let Some(home) = &home {
                let home = home.canonicalize().unwrap_or_else(|_| home.clone());
                writable_regex.push(format!(
                    "^{}/\\.claude\\.json",
                    regex::escape(&home.to_string_lossy())
                ));
            }
        },
        _ => {},
    }
    // The coding run's real repo stays unwritable even when it sits under a
    // writable root (temp, a tool cache); unset in the nested-shadow case.
    let deny_write = coding_run_real_repo()
        .map(|repo| repo.canonicalize().unwrap_or(repo))
        .into_iter()
        .collect();
    super::shell_box::HarnessBox {
        engine_binary: binary.to_path_buf(),
        writable,
        writable_regex,
        deny_write,
        // Claude Code and Antigravity keep their sign-in in the login keychain
        // and save a refreshed token back to it; every other engine keeps it
        // unreadable.
        keychain_readable: keychain_signin(engine),
        // Goal workspaces the requesting agent may not reach stay closed to
        // the engine (`goal_workspace_access`).
        acting_agent_id: request
            .acting_agent_id
            .clone()
            .or_else(crate::magician_v2::execution::native_executors::current_acting_agent),
    }
}

/// Drop `flags` (and, for those in `with_value`, the value after each) from
/// `args`: super god mode's unrestricted launch.
pub fn without_flags(args: Vec<OsString>, flags: &[&str], with_value: &[&str]) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        let text = arg.to_string_lossy();
        if with_value.iter().any(|flag| *flag == text) {
            skip_next = true;
            continue;
        }
        if flags.iter().any(|flag| *flag == text) {
            continue;
        }
        out.push(arg);
    }
    out
}

/// A VibeDev engine's process, fenced for this turn, and the env to add
/// after the adapter filters its child env (so the proxy settings survive).
pub struct HarnessCommand {
    pub command: tokio::process::Command,
    pub env: Vec<(String, String)>,
}

impl HarnessCommand {
    /// Add the fence's env: call after the adapter's own env.
    pub fn apply_env(&mut self) {
        for (key, value) in &self.env {
            self.command.env(key, value);
        }
    }
}

/// Build `engine`'s process for this turn: in Magician's box when the turn's
/// fence is [`HarnessFence::Box`], else under the coding profile, exactly as
/// [`os_sandbox_command`].
pub fn harness_command(
    engine: CodingEngineKind,
    binary: &Path,
    args: &[OsString],
    request: &CodingEngineRequest,
    read_only: bool,
) -> HarnessCommand {
    if let Some(repo) = request
        .original_repo
        .as_ref()
        .filter(|_| harness_unrestricted())
    {
        let acting = request
            .acting_agent_id
            .clone()
            .or_else(crate::magician_v2::execution::native_executors::current_acting_agent);
        let denies = crate::magician_v2::execution::goal_workspace_access::denies_for_acting(
            acting.as_deref(),
        );
        if denies.guards_workspaces && super::shell_box::linux_harness_isolation_missing() {
            return HarnessCommand {
                command: refused_launch(
                    "Original-folder runs cannot bypass the Linux goal workspace network fence",
                ),
                env: Vec::new(),
            };
        }
        return match super::shell_box::wrap_workspace_and_read_only(
            binary.as_os_str(),
            args,
            &denies,
            true,
            std::slice::from_ref(repo),
        ) {
            Some(boxed) => {
                let mut command = tokio::process::Command::new(&boxed.program);
                command.args(&boxed.args);
                HarnessCommand {
                    command,
                    env: boxed.env,
                }
            },
            None => HarnessCommand {
                command: refused_launch("Original-folder approval needs an OS write fence"),
                env: Vec::new(),
            },
        };
    }
    if harness_unrestricted() {
        // Super god mode lifts every fence but goal workspaces (owner
        // decision 2026-10-20: that fence is always on): an agent a
        // workspace is fenced from runs under the workspace rules alone.
        let acting = request
            .acting_agent_id
            .clone()
            .or_else(crate::magician_v2::execution::native_executors::current_acting_agent);
        let denies = crate::magician_v2::execution::goal_workspace_access::denies_for_acting(
            acting.as_deref(),
        );
        if denies.guards_workspaces && super::shell_box::linux_harness_isolation_missing() {
            // bwrap cannot hold a harness's network to its own grant channel.
            return HarnessCommand {
                command: refused_launch(
                    "super god mode cannot launch this engine for an agent fenced from a goal workspace on Linux: its network cannot yet be held to its own channel",
                ),
                env: Vec::new(),
            };
        }
        if denies.guards_workspaces {
            return match super::shell_box::wrap_workspace_only(binary.as_os_str(), args, &denies, true) {
                Some(boxed) => {
                    tracing::warn!(
                        target: "coding_engine",
                        engine = ?engine,
                        "[SUPER-GOD] engine launched with only the goal workspace fence"
                    );
                    let mut command = tokio::process::Command::new(&boxed.program);
                    command.args(&boxed.args);
                    HarnessCommand {
                        command,
                        env: boxed.env,
                    }
                },
                None => HarnessCommand {
                    command: refused_launch(
                        "super god mode cannot launch this engine here: goal workspaces need an OS sandbox for this agent",
                    ),
                    env: Vec::new(),
                },
            };
        }
        tracing::warn!(
            target: "coding_engine",
            engine = ?engine,
            binary = %binary.display(),
            "[SUPER-GOD] engine launched with no fence"
        );
        let mut command = tokio::process::Command::new(binary);
        command.args(args);
        return HarnessCommand {
            command,
            env: Vec::new(),
        };
    }
    if harness_wrap() == HarnessWrap::Box {
        let harness = harness_box(engine, binary, request, read_only);
        // On Linux a fenced harness would keep the host network (bwrap cannot
        // filter ports) and could reach another session's grant channel.
        if super::shell_box::linux_harness_isolation_missing()
            && crate::magician_v2::execution::goal_workspace_access::denies_for(
                harness.acting_agent_id.as_deref(),
            )
            .guards_workspaces
        {
            return HarnessCommand {
                command: refused_launch(
                    "this coding engine may not start for an agent fenced from a goal workspace on Linux: its network cannot yet be held to its own channel",
                ),
                env: Vec::new(),
            };
        }
        if let Some(boxed) = super::shell_box::wrap_harness(binary.as_os_str(), args, &harness) {
            tracing::info!(
                target: "coding_engine",
                engine = ?engine,
                read_only,
                "[HARNESS-BOX] engine process launched in Magician's box"
            );
            let mut command = tokio::process::Command::new(&boxed.program);
            command.args(&boxed.args);
            let mut env = boxed.env;
            env.extend(
                harness_box_env(engine)
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string())),
            );
            return HarnessCommand { command, env };
        }
    }
    HarnessCommand {
        command: os_sandbox_command_with(binary.as_os_str(), args, engine_allowances(engine)),
        env: Vec::new(),
    }
}

/// A command that only reports `message` and fails (exit 126): a launch the
/// goal workspace fence refuses, for callers that spawn whatever they get.
pub fn refused_launch(message: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "printf '%s\\n' \"$1\" >&2; exit 126",
        "refused",
        message,
    ]);
    command
}

/// What `engine` may do under the coding profile beyond its default: its own
/// state paths (writable under the write allow-list) and, for an engine whose
/// sign-in lives there, the login keychain (owner decision 2026-10-09).
pub fn engine_allowances(engine: CodingEngineKind) -> SandboxAllowances {
    SandboxAllowances {
        keychain_read: keychain_signin(engine),
        state_paths: engine_state_paths(engine, &dirs::home_dir().unwrap_or_default()),
    }
}

pub fn resolve_coding_repo_binding(
    workspace_root: &Path,
    value: Option<&str>,
) -> std::result::Result<CodingRepoBinding, String> {
    let workspace_root = workspace_root.canonicalize().map_err(|error| {
        format!(
            "Could not canonicalize scoped workspace root `{}`: {error}",
            workspace_root.display()
        )
    })?;
    let trimmed = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let expanded = expand_home_path(trimmed);
    let input = PathBuf::from(&expanded);
    let candidate = if input.is_absolute() {
        input
    } else {
        workspace_root.join(normalize_coding_repo_relative_path(Path::new(trimmed))?)
    };
    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "repo_path `{}` is not an existing directory: {error}",
            trimmed
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!("repo_path `{}` is not a directory", trimmed));
    }
    // Isolation deny-fence: refuse to bind the LIVE magician repo's source tree
    // as a coding work target (observed in the M3 live test — an absolute
    // repo_path would shadow-copy + stage proposals against the user's real
    // repo). Shared with the shell deny-fence via `reject_repo_source_tree`;
    // the sandbox + genuine external dirs are allowed, only the repo source is
    // rejected. Only fires when the sandbox is nested inside a real git repo.
    if let Some((repo_source_root, sandbox_base)) = live_repo_fence_roots(&workspace_root) {
        reject_repo_source_tree(&canonical, &repo_source_root, &sandbox_base)
            .map_err(|msg| format!("repo_path `{}`: {msg}", trimmed))?;
    }
    let repo_path = if canonical.starts_with(&workspace_root) {
        let relative = canonical.strip_prefix(&workspace_root).map_err(|error| {
            format!(
                "Could not normalize repo_path `{}` relative to scoped workspace `{}`: {error}",
                canonical.display(),
                workspace_root.display()
            )
        })?;
        if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            slash_path(relative)
        }
    } else {
        canonical.display().to_string()
    };
    Ok(CodingRepoBinding {
        repo_path,
        real_path: canonical,
    })
}

fn expand_home_path(value: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return value.to_string();
    };
    let home = home.display().to_string();
    if value == "~" {
        return home;
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    if value == "$HOME" || value == "${HOME}" {
        return home;
    }
    if let Some(rest) = value.strip_prefix("$HOME/") {
        return format!("{home}/{rest}");
    }
    if let Some(rest) = value.strip_prefix("${HOME}/") {
        return format!("{home}/{rest}");
    }
    value.to_string()
}

fn normalize_coding_repo_relative_path(path: &Path) -> std::result::Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {},
            Component::ParentDir => {
                return Err(format!(
                    "repo_path `{}` cannot contain `..`",
                    path.display()
                ));
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "repo_path `{}` must be workspace-relative or an absolute path",
                    path.display()
                ));
            },
        }
    }
    Ok(normalized)
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingEngineKind {
    Pi,
    /// Local `codex app-server` over stdio. The factory can construct the
    /// dormant adapter for harnesses; no profile/task/UI route selects it yet.
    CodexAppServer,
    /// Local `grok agent stdio` over ACP JSON-RPC. Phase 1 is a live picker
    /// row when `coding.grok.enabled` and a single reviewed binary is Ready.
    GrokAcp,
    /// Local `claude -p --output-format stream-json` headless NDJSON.
    /// Slice C1 is a live picker row when `coding.claude.enabled` and exactly
    /// one reviewed binary is found. Isolation/version overlays wait for C2.
    ClaudeCode,
    /// Local `agy` headless NDJSON. Dormant: the factory is Unconstructable
    /// and no VibeDev route spawns it.
    AgyCli,
}

pub type CodingEngineEventSink = Arc<dyn Fn(&CodingEngineEvent) + Send + Sync + 'static>;

/// Where the assistant's text goes as it is spoken: one call per delta the
/// engine emitted, in order, uncoalesced. [`CodingEngineEventSink`] is not
/// that channel — its queue folds a run of message deltas into one event to
/// stay under the per-turn event cap, which is right for the cockpit stream
/// and wrong for a mouth showing text live. The text is what the settled
/// `assistant_text` accumulates, before that field's byte bound. It is
/// called on the adapter's reader task, between protocol lines, so it must
/// return at once and never block (hand the delta to a channel; never await
/// or lock across I/O). Only the Codex app-server adapter feeds it today:
/// the plane's other engines read their CLI's stream themselves.
pub type CodingEngineTextDeltaSink = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// What an adapter can say about its dispatch **while the turn is still open**.
///
/// Every other channel out of a turn — the run result, `usage_capture`,
/// `backoff_capture` — is read once the turn has settled, which is precisely the
/// case a mid-turn death is not. These two facts have to leave the adapter
/// before then or they are lost with the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingDispatchNotice {
    /// The engine named the native session/thread this turn is running on.
    /// Reported as early as each engine can say it — see the adapters — and
    /// always before the turn's outcome is known, so `last_completed_turn_id`
    /// is `None` on this ref by construction.
    LiveSession(CodingContinuationRef),
    /// The provider accepted the turn and gave it an id.
    ///
    /// Only Codex reports this: it is the `turnId` from `turn/start`. Pi, Grok,
    /// Claude and Agy have no per-turn provider identifier to report, so they
    /// never send this notice and their invocations stay at
    /// `RequestMayHaveStarted` for the whole turn — which is the honest state,
    /// not a missing one.
    TurnAccepted { provider_turn_id: String },
}

/// Where a [`CodingDispatchNotice`] goes. `run_coding_task` installs one that
/// writes the coding ledger; every other caller leaves it `None`.
pub type CodingDispatchSink = Arc<dyn Fn(CodingDispatchNotice) + Send + Sync + 'static>;

/// The sink plus the three run-scoped values a [`CodingContinuationRef`] is
/// keyed by, so an adapter reports a session with one call instead of
/// reassembling the key at five sites.
///
/// Cloned onto the session structs of the adapters whose session id arrives on
/// a stream event (Claude, Agy) rather than from a call they make (Pi, Codex,
/// Grok), because those handlers have no `CodingEngineRequest` in scope.
///
/// `None` from [`Self::from_request`] is the ordinary case: a caller that
/// installed no sink, which is every caller but the coding handler.
#[derive(Clone)]
pub struct CodingLiveSessionReporter {
    sink: CodingDispatchSink,
    scope_root: PathBuf,
    workspace_root: PathBuf,
    run_task_id: Option<String>,
}

impl CodingLiveSessionReporter {
    pub fn from_request(request: &CodingEngineRequest) -> Option<Self> {
        request.dispatch_sink.as_ref().map(|sink| Self {
            sink: sink.clone(),
            scope_root: request.scope_root.clone(),
            workspace_root: request.workspace_root.clone(),
            run_task_id: request.run_task_id.clone(),
        })
    }

    /// The same three coordinates the adapters' end-of-turn constructions use
    /// (`request.scope_root`, `request.workspace_root`, `request.run_task_id`),
    /// so the mid-flight ref and the settled one agree on every binding digest
    /// and `resume_or_fresh` cannot read them as two different scopes.
    pub fn report_live_session(&self, engine: CodingEngineKind, native_session_id: &str) {
        if native_session_id.trim().is_empty() {
            return;
        }
        (self.sink)(CodingDispatchNotice::LiveSession(
            CodingContinuationRef::for_engine(
                engine,
                native_session_id,
                &self.scope_root,
                &self.workspace_root,
                self.run_task_id.as_deref(),
            ),
        ));
    }

    pub fn report_turn_accepted(&self, provider_turn_id: &str) {
        if provider_turn_id.trim().is_empty() {
            return;
        }
        (self.sink)(CodingDispatchNotice::TurnAccepted {
            provider_turn_id: provider_turn_id.to_string(),
        });
    }
}

/// How much of a repository's own CLI configuration a coding engine loads.
///
/// `Stripped` is the default and the only posture a sandboxed project gets:
/// no hooks, no MCP servers, no slash commands, no subagents — every engine's
/// isolation receipt attests exactly that. `Project` is for a **trusted**
/// project's chain (its own clone): the CLI loads what is checked into the
/// repo — its settings and hooks, its MCP servers, its skills/slash commands
/// and subagents — but nothing from the operator's own user-level config.
/// The OS sandbox still holds either way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RepoConfigPosture {
    #[default]
    Stripped,
    Project {
        repo_root: PathBuf,
        /// Server names declared in the repo's `.mcp.json` — the only MCP
        /// servers a `Project` turn may report.
        mcp_server_names: Vec<String>,
    },
}

impl RepoConfigPosture {
    /// The `Project` posture for a trusted clone, reading the repo's
    /// `.mcp.json` server names (a missing or unreadable file declares none).
    pub fn project(repo_root: &Path) -> Self {
        let mcp_server_names = std::fs::read(repo_root.join(".mcp.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("mcpServers")
                    .and_then(|servers| servers.as_object().cloned())
            })
            .map(|servers| servers.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Self::Project {
            repo_root: repo_root.to_path_buf(),
            mcp_server_names,
        }
    }

    pub fn is_project(&self) -> bool {
        matches!(self, Self::Project { .. })
    }

    /// The repo's `.mcp.json`, when this posture loads one.
    pub fn project_mcp_config(&self) -> Option<PathBuf> {
        match self {
            Self::Project { repo_root, .. } => {
                let path = repo_root.join(".mcp.json");
                path.is_file().then_some(path)
            },
            Self::Stripped => None,
        }
    }

    pub fn allows_mcp_server(&self, name: &str) -> bool {
        match self {
            Self::Project {
                mcp_server_names, ..
            } => mcp_server_names.iter().any(|allowed| allowed == name),
            Self::Stripped => false,
        }
    }
}

#[derive(Clone)]
pub struct CodingEngineRequest {
    pub prompt: String,
    pub workspace_root: PathBuf,
    pub shadow_workspace_root: PathBuf,
    pub working_dir: Option<PathBuf>,
    pub apply_root: Option<PathBuf>,
    pub scope_root: PathBuf,
    pub scope: TransactionScope,
    /// Pi-only spawn/turn options. Codex must not read these.
    pub pi: PiTurnOptions,
    /// Codex-only spawn/turn options. Pi must not read these.
    pub codex: CodexTurnOptions,
    /// Grok-only spawn/turn options. Pi and Codex must not read these.
    pub grok: GrokTurnOptions,
    /// Claude-only spawn/turn options. Other adapters must not read these.
    pub claude: ClaudeTurnOptions,
    /// Agy-only spawn/turn options. Dormant; no live adapter reads these.
    pub agy: AgyTurnOptions,
    pub env: BTreeMap<String, String>,
    /// Build settings every engine receives after its own env filtering —
    /// today the shared `CARGO_TARGET_DIR`. The CLI adapters rebuild their
    /// child env from an allowlist, so anything a build needs that is not on
    /// it must ride here or it never arrives.
    pub build_env: BTreeMap<String, String>,
    /// How much of the repo's own CLI configuration the engine may load
    /// (plan 2026-10-07 P5). `Stripped` for every sandboxed run.
    pub repo_config: RepoConfigPosture,
    /// Wall clock for this ONE turn. Resolved from [`budgets`] by the caller
    /// and narrowed to whatever the whole-task budget has left.
    ///
    /// [`budgets`]: CodingEngineRequest::budgets
    pub timeout: Duration,
    /// The resolved budget set for this run. `Some` arms the phase-aware
    /// no-progress detector; `None` leaves the turn bounded only by `timeout`,
    /// which is the pre-detector behaviour and the kill-switch path.
    pub budgets:
        Option<crate::magician_v2::execution::coding_engine::budgets::ResolvedCodingBudgets>,
    /// Execution id the typed termination cause is filed under, so the reason a
    /// cancellation token fired travels with it instead of being guessed from a
    /// message. Empty/`None` = the reason stays local to this turn.
    pub termination_key: Option<String>,
    /// Out-cell for declared provider backoff spent this turn, written on every
    /// terminal path so the task ledger can exclude a rate-limit wait from
    /// active work even when the turn ends badly.
    pub backoff_capture: Option<Arc<std::sync::Mutex<Duration>>>,
    /// The run's dollar cap, shared with every live turn of the same run.
    /// Engines that stream per-message cost (Pi) stop the turn the moment the
    /// run reaches it; `None` = the run has no cap. Not serialized.
    pub cost_ceiling: Option<Arc<cost_budget::CodingCostCeiling>>,
    pub stage_result: bool,
    pub proposal_summary: Option<String>,
    pub test_evidence: Vec<TestRunSummary>,
    pub event_sink: Option<CodingEngineEventSink>,
    /// See [`CodingEngineTextDeltaSink`]. `None` = nothing shows the text live.
    pub text_delta_sink: Option<CodingEngineTextDeltaSink>,
    /// Run identifiers (task_id / execution_id / shadow_workspace_id) under which
    /// the live Pi turn is registered in the [`control`] registry while it runs,
    /// so the cockpit's Stop / steer control plane can reach it. Empty = the run
    /// is not externally steerable (e.g. a non-interactive batch turn).
    pub control_keys: Vec<String>,
    /// Optional cancel token (B2). When set and cancelled, `run_turn` aborts the
    /// in-flight Pi turn promptly and tears the session down GRACEFULLY (drains
    /// stderr, unregisters the control handle) instead of an abrupt future-drop
    /// that would leak a stale steerable handle. `None` = no external cancel.
    pub cancel_token: Option<CancellationToken>,
    /// Run identity stamped onto any `CodeChangeProposal` staged this turn, so the
    /// coordinator side can join the proposal back to its task/child (the parent
    /// reconcile reads proposal status by `task_id`; the B14 backstop fingerprints
    /// by content). `None` = un-stamped (legacy / non-task runs).
    pub run_task_id: Option<String>,
    pub run_execution_id: Option<String>,
    /// Out-cell for the /llm analytics bridge: `run_turn` writes THIS turn's incremental LLM-usage
    /// delta here (cumulative-after − cumulative-before) on EVERY terminal path — success, RPC
    /// error, timeout, and cancellation — so the caller records one `llm_calls` row per run
    /// regardless of outcome, and chained/resumed runs never double-count the session total.
    /// `None` = no bridge. Not serialized — a runtime side channel (like `event_sink`).
    pub usage_capture: Option<Arc<std::sync::Mutex<Option<CodingTurnUsage>>>>,
    /// Out-channel for what the adapter learns about its dispatch BEFORE the
    /// turn settles — see [`CodingDispatchNotice`]. A callback rather than an
    /// out-cell because the only caller that wants these is blocked awaiting
    /// `run_turn`, so a cell nobody reads until the turn ends would carry
    /// exactly the information a mid-turn death destroys. `None` = nothing is
    /// listening. Not serialized — a runtime side channel (like `event_sink`).
    pub dispatch_sink: Option<CodingDispatchSink>,
    /// The agent this turn works for, from runtime identity (never from tool
    /// arguments): the engine's box keeps out the goal workspaces that agent
    /// may not reach. `None` = no agent, a non-member of every workspace.
    pub acting_agent_id: Option<String>,
    /// Direct original checkout: held read-only even under Super god mode.
    pub original_repo: Option<PathBuf>,
}

impl CodingEngineRequest {
    /// A discuss turn for `engine`: it may not change the workspace copy, so
    /// its box keeps the copy unwritable and god mode never lifts it.
    pub fn read_only_turn(&self, engine: CodingEngineKind) -> bool {
        match engine {
            CodingEngineKind::CodexAppServer => !self.codex.mode.is_mutation_capable(),
            CodingEngineKind::GrokAcp => self.grok.mode == factory::GrokTurnMode::Discuss,
            CodingEngineKind::ClaudeCode => self.claude.mode == factory::ClaudeTurnMode::Discuss,
            CodingEngineKind::AgyCli => !self.agy.mode.is_mutation_capable(),
            CodingEngineKind::Pi => self.pi.plan_only,
        }
    }

    pub fn new(
        prompt: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        shadow_workspace_root: impl Into<PathBuf>,
        scope_root: impl Into<PathBuf>,
        scope: TransactionScope,
    ) -> Self {
        Self {
            prompt: prompt.into(),
            workspace_root: workspace_root.into(),
            shadow_workspace_root: shadow_workspace_root.into(),
            working_dir: None,
            apply_root: None,
            scope_root: scope_root.into(),
            scope,
            pi: PiTurnOptions::default(),
            codex: CodexTurnOptions::default(),
            grok: GrokTurnOptions::default(),
            claude: ClaudeTurnOptions::default(),
            agy: AgyTurnOptions::default(),
            env: BTreeMap::new(),
            build_env: BTreeMap::new(),
            repo_config: RepoConfigPosture::Stripped,
            timeout: Duration::from_secs(budgets::DEFAULT_CODING_TURN_TIMEOUT_SECS),
            budgets: None,
            termination_key: None,
            backoff_capture: None,
            cost_ceiling: None,
            stage_result: true,
            proposal_summary: None,
            test_evidence: Vec::new(),
            event_sink: None,
            text_delta_sink: None,
            control_keys: Vec::new(),
            cancel_token: None,
            run_task_id: None,
            run_execution_id: None,
            usage_capture: None,
            dispatch_sink: None,
            acting_agent_id: None,
            original_repo: None,
        }
    }
}

/// THIS turn's incremental LLM spend — `CodingSessionStats` (cumulative per Pi session) sampled
/// after the turn minus the sample taken before it. Recording this delta (not the cumulative)
/// keeps the `llm_calls` sink correct across chained/resumed runs: each run records only its own
/// spend, so summing rows never double-counts a session prefix. `success` reflects the turn's
/// terminal outcome (false on RPC error / timeout / cancellation).
///
/// `cost` is a billed USD amount only when `cost_known` is true. Unreported
/// provider cost stays unknown (not `$0.00`). `Default` is unknown, not zero.
#[derive(Debug, Clone, Default)]
pub struct CodingTurnUsage {
    pub cost: f64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub success: bool,
    pub cost_known: bool,
}

impl CodingTurnUsage {
    /// True when the provider reported tokens or a known (possibly zero) cost.
    /// All-zero unknown cost is not spend — the /llm bridge skips that row.
    pub fn has_reported_spend(&self) -> bool {
        self.input > 0
            || self.output > 0
            || self.cache_read > 0
            || self.cache_write > 0
            || (self.cost_known && self.cost > 0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingEngineRunResult {
    pub engine: CodingEngineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_text: Option<String>,
    /// Events are published once through the request sink. The result keeps
    /// only the count so a flood cannot duplicate the stream.
    #[serde(default)]
    pub event_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<CodingContinuationRef>,
    /// Why this turn opened a fresh native session. `continuation_lost` is
    /// recorded here (and on the chain-root store) when `session/load` fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_fresh_reason: Option<ContinuationFreshReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<CodeChangeProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_payload: Option<Value>,
    /// Cumulative session telemetry (cost + tokens + context window) fetched via
    /// Pi `get_session_stats` after the turn completes (R5). `None` when the
    /// engine didn't report stats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_stats: Option<CodingSessionStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingEngineEvent {
    pub sequence: usize,
    pub kind: CodingEngineEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning-stream delta (Pi `assistantMessageEvent.type == "thinking_delta"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_delta: Option<String>,
    /// Second-level streaming discriminant (`assistantMessageEvent.type`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_event_type: Option<String>,
    /// Token usage carried on `message_end` / `turn_end` (and on the streamed
    /// `assistantMessageEvent.partial`), extracted from `message.usage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CodingUsage>,
    /// Cumulative USD for the assistant message (`message.usage.cost.total`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// `message.stopReason` (`stop|length|toolUse|error|aborted`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// `tool_execution_end.isError`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_is_error: Option<bool>,
    /// `agent_end.willRetry` / `compaction_end.willRetry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_retry: Option<bool>,
    /// Unified failure text (Pi has no top-level error event — see the contract
    /// doc): `message.errorMessage` | `assistantMessageEvent.error.errorMessage`
    /// | `compaction_end.errorMessage` | `auto_retry_end.finalError`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub raw: serde_json::Value,
}

/// Token + cost usage as Pi reports it on `message.usage` (`pi-ai` `Usage`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodingUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// Billed USD when the provider reported it. Omitted cost is unknown, not `$0.00`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
}

/// Cumulative session telemetry from Pi's `get_session_stats` RPC
/// (`SessionStats`). `context_usage` is `None` immediately after a compaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSessionStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub user_messages: u64,
    #[serde(default)]
    pub assistant_messages: u64,
    #[serde(default)]
    pub tool_calls: u64,
    #[serde(default)]
    pub tool_results: u64,
    #[serde(default)]
    pub total_messages: u64,
    #[serde(default)]
    pub tokens: CodingSessionTokens,
    /// Cumulative USD across the whole session.
    #[serde(default)]
    pub cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<CodingContextUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSessionTokens {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingContextUsage {
    /// Estimated context tokens. `None` right after a compaction / before the first LLM response
    /// of a turn — Pi sends `null` there (`ContextUsage.tokens: number | null`). MUST be `Option`:
    /// a non-`Option` field cannot deserialize Pi's explicit `null` (`#[serde(default)]` only
    /// covers an ABSENT key), and the failure propagates up and drops the WHOLE `SessionStats`
    /// payload — losing cost + tokens too, not just context %.
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub context_window: u64,
    /// Context usage as a **0–100** percentage of the window — Pi computes
    /// `(tokens / contextWindow) * 100` (`agent-session.js`). `None` when `tokens` is unknown;
    /// `Option` for the same null-tolerance reason as `tokens`.
    #[serde(default)]
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingEngineEventKind {
    Response,
    AgentStart,
    AgentEnd,
    /// Pi 0.83's session-level completion boundary. Unlike `AgentEnd`, this is
    /// emitted only after retries, compaction retries, and queued continuations
    /// can no longer re-enter the agent loop.
    AgentSettled,
    TurnStart,
    TurnEnd,
    MessageStart,
    MessageUpdate,
    MessageEnd,
    ToolExecutionStart,
    ToolExecutionUpdate,
    ToolExecutionEnd,
    QueueUpdate,
    EntryAppended,
    SessionInfoChanged,
    ThinkingLevelChanged,
    CompactionStart,
    CompactionEnd,
    AutoRetryStart,
    AutoRetryEnd,
    SummarizationRetryScheduled,
    SummarizationRetryAttemptStart,
    SummarizationRetryFinished,
    BashExecutionUpdate,
    ExtensionError,
    Unknown,
}

impl CodingEngineEventKind {
    /// Wire name, as a static string. Lets a hot path record *which* event it
    /// saw without allocating; the exhaustive match is what keeps this from
    /// drifting away from `coding_event_from_raw` when a variant is added.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::AgentSettled => "agent_settled",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update",
            Self::MessageEnd => "message_end",
            Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update",
            Self::ToolExecutionEnd => "tool_execution_end",
            Self::QueueUpdate => "queue_update",
            Self::EntryAppended => "entry_appended",
            Self::SessionInfoChanged => "session_info_changed",
            Self::ThinkingLevelChanged => "thinking_level_changed",
            Self::CompactionStart => "compaction_start",
            Self::CompactionEnd => "compaction_end",
            Self::AutoRetryStart => "auto_retry_start",
            Self::AutoRetryEnd => "auto_retry_end",
            Self::SummarizationRetryScheduled => "summarization_retry_scheduled",
            Self::SummarizationRetryAttemptStart => "summarization_retry_attempt_start",
            Self::SummarizationRetryFinished => "summarization_retry_finished",
            Self::BashExecutionUpdate => "bash_execution_update",
            Self::ExtensionError => "extension_error",
            Self::Unknown => "unknown",
        }
    }
}

#[async_trait]
pub trait CodingEngineAdapter: Send + Sync + std::fmt::Debug {
    fn engine(&self) -> CodingEngineKind;

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult>;
}

pub fn coding_event_from_raw(sequence: usize, raw: serde_json::Value) -> CodingEngineEvent {
    let raw_type = raw
        .get("type")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let kind = match raw_type.as_deref() {
        Some("response") => CodingEngineEventKind::Response,
        Some("agent_start") => CodingEngineEventKind::AgentStart,
        Some("agent_end") => CodingEngineEventKind::AgentEnd,
        Some("agent_settled") => CodingEngineEventKind::AgentSettled,
        Some("turn_start") => CodingEngineEventKind::TurnStart,
        Some("turn_end") => CodingEngineEventKind::TurnEnd,
        Some("message_start") => CodingEngineEventKind::MessageStart,
        Some("message_update") => CodingEngineEventKind::MessageUpdate,
        Some("message_end") => CodingEngineEventKind::MessageEnd,
        Some("tool_execution_start") => CodingEngineEventKind::ToolExecutionStart,
        Some("tool_execution_update") => CodingEngineEventKind::ToolExecutionUpdate,
        Some("tool_execution_end") => CodingEngineEventKind::ToolExecutionEnd,
        Some("queue_update") => CodingEngineEventKind::QueueUpdate,
        Some("entry_appended") => CodingEngineEventKind::EntryAppended,
        Some("session_info_changed") => CodingEngineEventKind::SessionInfoChanged,
        Some("thinking_level_changed") => CodingEngineEventKind::ThinkingLevelChanged,
        Some("compaction_start") => CodingEngineEventKind::CompactionStart,
        Some("compaction_end") => CodingEngineEventKind::CompactionEnd,
        Some("auto_retry_start") => CodingEngineEventKind::AutoRetryStart,
        Some("auto_retry_end") => CodingEngineEventKind::AutoRetryEnd,
        Some("summarization_retry_scheduled") => CodingEngineEventKind::SummarizationRetryScheduled,
        Some("summarization_retry_attempt_start") => {
            CodingEngineEventKind::SummarizationRetryAttemptStart
        },
        Some("summarization_retry_finished") => CodingEngineEventKind::SummarizationRetryFinished,
        Some("bash_execution_update") => CodingEngineEventKind::BashExecutionUpdate,
        Some("extension_error") => CodingEngineEventKind::ExtensionError,
        _ => CodingEngineEventKind::Unknown,
    };
    let text_delta = raw
        .get("assistantMessageEvent")
        .and_then(|event| {
            (event.get("type").and_then(|value| value.as_str()) == Some("text_delta"))
                .then(|| event.get("delta").and_then(|value| value.as_str()))
                .flatten()
        })
        .map(str::to_string);
    let assistant_event_type = raw
        .pointer("/assistantMessageEvent/type")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let thinking_delta = raw
        .get("assistantMessageEvent")
        .and_then(|event| {
            (event.get("type").and_then(|value| value.as_str()) == Some("thinking_delta"))
                .then(|| event.get("delta").and_then(|value| value.as_str()))
                .flatten()
        })
        .map(str::to_string);
    // Token usage lives at `message.usage` for turn_end/message_end/message_start.
    // For message_update, Pi 0.84+ carries the cumulative usage at the top-level
    // `usage` (it dropped `assistantMessageEvent.partial`); older builds and the
    // other engines put it at `assistantMessageEvent.{partial|message|error}.usage`.
    // Prefer the first present.
    let usage = raw
        .pointer("/message/usage")
        .or_else(|| raw.pointer("/usage"))
        .or_else(|| raw.pointer("/assistantMessageEvent/partial/usage"))
        .or_else(|| raw.pointer("/assistantMessageEvent/message/usage"))
        .or_else(|| raw.pointer("/assistantMessageEvent/error/usage"))
        .and_then(parse_coding_usage);
    let cost_total = usage.as_ref().and_then(|usage| usage.cost_total);
    let stop_reason = raw
        .pointer("/message/stopReason")
        .or_else(|| raw.pointer("/assistantMessageEvent/partial/stopReason"))
        .or_else(|| raw.pointer("/assistantMessageEvent/message/stopReason"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tool_result_is_error = raw.get("isError").and_then(|value| value.as_bool());
    let will_retry = raw.get("willRetry").and_then(|value| value.as_bool());
    // Pi has no top-level error event (see the contract doc); failure text is
    // unioned across the channels that can carry it.
    let error_message = raw
        .pointer("/message/errorMessage")
        .or_else(|| raw.pointer("/assistantMessageEvent/error/errorMessage"))
        .or_else(|| raw.get("errorMessage"))
        .or_else(|| raw.get("finalError"))
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string);
    let tool_name = raw
        .get("toolName")
        .or_else(|| raw.get("tool_name"))
        .or_else(|| raw.pointer("/tool/name"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tool_call_id = raw
        .get("toolCallId")
        .or_else(|| raw.get("tool_call_id"))
        .or_else(|| raw.pointer("/tool/id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    CodingEngineEvent {
        sequence,
        kind,
        raw_type,
        text_delta,
        tool_name,
        tool_call_id,
        thinking_delta,
        assistant_event_type,
        usage,
        cost_total,
        stop_reason,
        tool_result_is_error,
        will_retry,
        error_message,
        raw,
    }
}

/// Parse Pi's `Usage` JSON (camelCase, nested `cost.total`) into [`CodingUsage`].
fn parse_coding_usage(value: &Value) -> Option<CodingUsage> {
    if !value.is_object() {
        return None;
    }
    let count = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(CodingUsage {
        input: count("input"),
        output: count("output"),
        cache_read: count("cacheRead"),
        cache_write: count("cacheWrite"),
        total_tokens: count("totalTokens"),
        cost_total: value.pointer("/cost/total").and_then(Value::as_f64),
    })
}

pub fn stage_shadow_workspace_patch(
    request: &CodingEngineRequest,
    source_session_id: impl Into<String>,
) -> Result<Option<CodeChangeProposal>> {
    let patch = compute_shadow_workspace_patch(
        &request.workspace_root,
        &request.shadow_workspace_root,
        &ShadowPatchOptions::default(),
    )?;
    if patch.trim().is_empty() {
        return Ok(None);
    }

    let summary = request
        .proposal_summary
        .clone()
        .unwrap_or_else(|| "Coding agent session changes".to_string());
    let store = CodeChangeProposalStore::new(&request.scope_root);
    store
        .stage_patch_with_apply_root(
            request.scope.clone(),
            summary,
            patch,
            source_session_id,
            request.test_evidence.clone(),
            request.apply_root.clone(),
            request.run_task_id.clone(),
            request.run_execution_id.clone(),
        )
        .map(Some)
}

/// Stage the shadow-vs-real patch after an adapter turn. Adapters must not
/// call this; `run_coding_task` owns proposal authority.
pub fn attach_staged_coding_proposal(
    request: &CodingEngineRequest,
    result: &mut CodingEngineRunResult,
) -> Result<()> {
    if !request.stage_result || result.proposal.is_some() {
        return Ok(());
    }
    let source_session_id = result
        .session_id
        .clone()
        .unwrap_or_else(|| match result.engine {
            CodingEngineKind::Pi => format!("pi-rpc-{}", Uuid::new_v4()),
            CodingEngineKind::CodexAppServer => format!("codex-{}", Uuid::new_v4()),
            CodingEngineKind::GrokAcp => format!("grok-{}", Uuid::new_v4()),
            CodingEngineKind::ClaudeCode => format!("claude-{}", Uuid::new_v4()),
            CodingEngineKind::AgyCli => format!("agy-{}", Uuid::new_v4()),
        });
    result.proposal = stage_shadow_workspace_patch(request, source_session_id)?;
    result.approval_payload = result
        .proposal
        .as_ref()
        .map(proposal_pending_approval_response);
    Ok(())
}

pub fn proposal_pending_approval_response(proposal: &CodeChangeProposal) -> Value {
    let files: Vec<AgenticDiffApprovalFile> = proposal
        .files
        .iter()
        .map(|file| AgenticDiffApprovalFile {
            path: file.path.clone(),
            status: file.status.clone(),
            additions: file.additions,
            deletions: file.deletions,
            unified_diff: file.unified_diff.clone(),
        })
        .collect();
    let additions: usize = proposal.files.iter().map(|file| file.additions).sum();
    let deletions: usize = proposal.files.iter().map(|file| file.deletions).sum();
    let input_type = UserInputType::DiffApproval {
        transaction_id: None,
        proposal_id: Some(proposal.id.as_str().to_string()),
        approval_source: Some("proposal".to_string()),
        rationale: proposal.summary.clone(),
        files: files.clone(),
    };

    json!({
        "status": "pending_approval",
        "paused": true,
        "pause_kind": "diff_approval",
        "pause_question": "Approve coding agent changes?",
        "pause_hint": "Review the diff. Apply writes the staged proposal to the real workspace; reject leaves the real workspace untouched.",
        "pause_input_type": input_type,
        "proposal_id": proposal.id.as_str(),
        "approval_source": "proposal",
        "rationale": proposal.summary.clone(),
        "files": files,
        "stats": {
            "files": proposal.files.len(),
            "additions": additions,
            "deletions": deletions,
        },
    })
}

#[derive(Debug, Clone)]
pub struct ShadowPatchOptions {
    pub ignored_dir_names: BTreeSet<String>,
    pub max_file_bytes: u64,
}

impl Default for ShadowPatchOptions {
    fn default() -> Self {
        Self {
            // Excludes build / VCS / language-cache dirs AND home-level config /
            // credential / cache dotdirs. The latter matter when the "repo" is the
            // scope home (the default `.` project): without them `.config/gcloud/logs`
            // (300k+ files) floods the proposal diff and tanks performance, and secrets
            // under `.ssh` / `.gnupg` / `.aws` could leak into a code proposal. A coding
            // change never legitimately touches these.
            ignored_dir_names: [
                // build / VCS / language caches
                ".cache",
                ".git",
                ".hg",
                ".next",
                ".svn",
                ".svelte-kit",
                ".turbo",
                ".venv",
                "__pycache__",
                "build",
                "dist",
                "node_modules",
                "target",
                "venv",
                // home-level config / credentials / tool caches (scope-home repos)
                ".aws",
                ".azure",
                ".cargo",
                ".config",
                ".docker",
                ".gnupg",
                ".gradle",
                ".kube",
                ".local",
                ".m2",
                ".mozilla",
                ".npm",
                ".rustup",
                ".ssh",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            max_file_bytes: MAX_FILE_READ_BYTES,
        }
    }
}

pub fn compute_shadow_workspace_patch(
    workspace_root: &Path,
    shadow_workspace_root: &Path,
    options: &ShadowPatchOptions,
) -> Result<String> {
    ensure_directory(workspace_root, "workspace_root")?;
    ensure_directory(shadow_workspace_root, "shadow_workspace_root")?;

    let real = scan_tree(workspace_root, options)
        .with_context(|| format!("scan workspace {}", workspace_root.display()))?;
    let shadow = scan_tree(shadow_workspace_root, options)
        .with_context(|| format!("scan shadow workspace {}", shadow_workspace_root.display()))?;

    if real.symlinks != shadow.symlinks {
        return Err(anyhow!(
            "shadow patch cannot stage symlink changes; real symlinks={:?}, shadow symlinks={:?}",
            real.symlinks,
            shadow.symlinks
        ));
    }

    let mut paths = BTreeSet::new();
    paths.extend(real.files);
    paths.extend(shadow.files);

    let mut patch = String::new();
    let mut binary_skipped: usize = 0;
    for rel_path in paths {
        let real_path = workspace_root.join(&rel_path);
        let shadow_path = shadow_workspace_root.join(&rel_path);
        let real_exists = real_path.is_file();
        let shadow_exists = shadow_path.is_file();

        match (real_exists, shadow_exists) {
            (true, true) => {
                let old_bytes = read_bytes_bounded_local(&real_path, options.max_file_bytes)?;
                let new_bytes = read_bytes_bounded_local(&shadow_path, options.max_file_bytes)?;
                // Byte-compare FIRST: an unchanged file (incl. binary like an image/PDF)
                // contributes no diff and needs no UTF-8 decode. This is what lets a
                // workspace that already contains binary files stage its text changes
                // instead of aborting the whole patch on the first non-UTF-8 byte.
                if old_bytes == new_bytes {
                    continue;
                }
                match (
                    String::from_utf8(old_bytes).ok(),
                    String::from_utf8(new_bytes).ok(),
                ) {
                    (Some(old), Some(new)) => {
                        patch.push_str(&compute_unified_diff(
                            &old,
                            &new,
                            &diff_path_string(&rel_path)?,
                        ));
                    },
                    // A genuinely-changed binary file: skip it from the text patch (binary
                    // coding proposals aren't supported yet) rather than failing the diff.
                    _ => binary_skipped += 1,
                }
            },
            (false, true) => {
                match String::from_utf8(read_bytes_bounded_local(
                    &shadow_path,
                    options.max_file_bytes,
                )?)
                .ok()
                {
                    Some(new) => {
                        let diff = compute_unified_diff("", &new, &diff_path_string(&rel_path)?);
                        patch.push_str(&rewrite_first_header(diff, "--- ", "--- /dev/null\n")?);
                    },
                    None => binary_skipped += 1,
                }
            },
            (true, false) => {
                match String::from_utf8(read_bytes_bounded_local(
                    &real_path,
                    options.max_file_bytes,
                )?)
                .ok()
                {
                    Some(old) => {
                        let diff = compute_unified_diff(&old, "", &diff_path_string(&rel_path)?);
                        patch.push_str(&rewrite_second_header(diff, "+++ ", "+++ /dev/null\n")?);
                    },
                    None => binary_skipped += 1,
                }
            },
            (false, false) => {},
        }
    }

    if binary_skipped > 0 {
        tracing::warn!(
            "shadow patch skipped {} non-UTF-8 (binary) file(s) — binary coding proposals are not supported yet; text changes still staged",
            binary_skipped
        );
    }

    Ok(patch)
}

pub fn prepare_shadow_workspace(
    workspace_root: &Path,
    shadow_workspace_root: &Path,
    options: &ShadowPatchOptions,
) -> Result<()> {
    ensure_directory(workspace_root, "workspace_root")?;
    if shadow_workspace_root.exists() {
        if !shadow_workspace_root.is_dir() {
            return Err(anyhow!(
                "shadow_workspace_root exists but is not a directory: {}",
                shadow_workspace_root.display()
            ));
        }
        if std::fs::read_dir(shadow_workspace_root)
            .with_context(|| format!("read {}", shadow_workspace_root.display()))?
            .next()
            .is_some()
        {
            return Err(anyhow!(
                "shadow_workspace_root must be empty before preparation: {}",
                shadow_workspace_root.display()
            ));
        }
    } else {
        std::fs::create_dir_all(shadow_workspace_root)
            .with_context(|| format!("create {}", shadow_workspace_root.display()))?;
    }
    copy_workspace_tree(
        workspace_root,
        workspace_root,
        shadow_workspace_root,
        options,
    )
}

/// Sync a **persistent** shadow workspace to the real repo's current state, in
/// place (R2) — the cache-preserving alternative to [`prepare_shadow_workspace`],
/// which requires an empty dir and cold-copies the whole tree every run.
///
/// Tracked files are overwritten/added from `workspace_root`; entries left by a
/// prior run that the real repo no longer has are pruned. Crucially, the ignored
/// cache dirs (`node_modules`, `target`, `.venv`, …) are **never copied and
/// never pruned**, so an `npm install` / `cargo build` performed once inside the
/// shadow survives across runs — the cold-reinstall-every-turn tax goes away.
/// The shadow-vs-real byte diff (and thus the `CodeChangeProposal` / apply gate)
/// is unchanged; only the shadow's *lifecycle* changes.
pub fn sync_persistent_workspace(
    workspace_root: &Path,
    shadow_workspace_root: &Path,
    options: &ShadowPatchOptions,
) -> Result<()> {
    ensure_directory(workspace_root, "workspace_root")?;
    if shadow_workspace_root.exists() && !shadow_workspace_root.is_dir() {
        return Err(anyhow!(
            "shadow_workspace_root exists but is not a directory: {}",
            shadow_workspace_root.display()
        ));
    }
    std::fs::create_dir_all(shadow_workspace_root)
        .with_context(|| format!("create {}", shadow_workspace_root.display()))?;
    // 1. Overwrite/add tracked files from the real repo (skips ignored caches).
    copy_workspace_tree(
        workspace_root,
        workspace_root,
        shadow_workspace_root,
        options,
    )?;
    // 2. Prune strays from a prior run so they don't pollute the next diff,
    //    preserving ignored cache dirs untouched.
    prune_shadow_strays(
        workspace_root,
        shadow_workspace_root,
        shadow_workspace_root,
        options,
    )
}

/// Remove entries under `dir` (inside the shadow) that have no counterpart in
/// the real repo — a previous run's unapplied creations. Ignored cache dirs are
/// preserved untouched (that is the whole point of the persistent shadow).
fn prune_shadow_strays(
    real_root: &Path,
    shadow_root: &Path,
    dir: &Path,
    options: &ShadowPatchOptions,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read shadow dir {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("collect shadow dir entries {}", dir.display()))?;
    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let metadata =
            std::fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        let file_type = metadata.file_type();
        let rel = path
            .strip_prefix(shadow_root)
            .with_context(|| format!("strip shadow root from {}", path.display()))?;
        let real_path = real_root.join(rel);
        if file_type.is_dir() {
            if should_ignore_dir(file_name.as_os_str(), options) {
                continue; // preserve node_modules / target / .venv / ...
            }
            if real_path.is_dir() {
                prune_shadow_strays(real_root, shadow_root, &path, options)?;
            } else {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("prune stray shadow dir {}", path.display()))?;
            }
        } else if match std::fs::symlink_metadata(&real_path) {
            Err(_) => true,
            Ok(m) => m.file_type().is_dir(),
        } {
            // Shadow file/symlink is stray: either it has no counterpart in the
            // real repo, OR the real counterpart is now a DIRECTORY (type flip) —
            // a stale shadow file must not shadow a real dir. Prune it.
            std::fs::remove_file(&path)
                .with_context(|| format!("prune stray shadow file {}", path.display()))?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct TreeScan {
    files: BTreeSet<PathBuf>,
    symlinks: BTreeSet<PathBuf>,
}

fn ensure_directory(path: &Path, label: &str) -> Result<()> {
    if !path.is_dir() {
        return Err(anyhow!("{label} is not a directory: {}", path.display()));
    }
    Ok(())
}

fn scan_tree(root: &Path, options: &ShadowPatchOptions) -> Result<TreeScan> {
    let mut scan = TreeScan::default();
    scan_tree_inner(root, root, options, &mut scan)?;
    Ok(scan)
}

fn scan_tree_inner(
    root: &Path,
    dir: &Path,
    options: &ShadowPatchOptions,
    scan: &mut TreeScan,
) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("collect dir entries {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let metadata =
            std::fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        let file_type = metadata.file_type();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("strip root {} from {}", root.display(), path.display()))?
            .to_path_buf();
        if file_type.is_symlink() {
            scan.symlinks.insert(rel);
            continue;
        }
        if file_type.is_dir() {
            if should_ignore_dir(file_name.as_os_str(), options) {
                continue;
            }
            scan_tree_inner(root, &path, options, scan)?;
        } else if file_type.is_file() {
            scan.files.insert(rel);
        }
    }
    Ok(())
}

fn copy_workspace_tree(
    workspace_root: &Path,
    src_dir: &Path,
    shadow_root: &Path,
    options: &ShadowPatchOptions,
) -> Result<()> {
    let mut entries = std::fs::read_dir(src_dir)
        .with_context(|| format!("read dir {}", src_dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("collect dir entries {}", src_dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let src = entry.path();
        let file_name = entry.file_name();
        let metadata =
            std::fs::symlink_metadata(&src).with_context(|| format!("stat {}", src.display()))?;
        let file_type = metadata.file_type();
        let rel = src.strip_prefix(workspace_root).with_context(|| {
            format!(
                "strip root {} from {}",
                workspace_root.display(),
                src.display()
            )
        })?;
        let dst = shadow_root.join(rel);

        if file_type.is_symlink() {
            copy_symlink(&src, &dst)?;
        } else if file_type.is_dir() {
            if should_ignore_dir(file_name.as_os_str(), options) {
                continue;
            }
            std::fs::create_dir_all(&dst)
                .with_context(|| format!("create shadow dir {}", dst.display()))?;
            copy_workspace_tree(workspace_root, &src, shadow_root, options)?;
        } else if file_type.is_file() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create shadow parent {}", parent.display()))?;
            }
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        }
    }
    Ok(())
}

fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = std::fs::read_link(src).with_context(|| format!("readlink {}", src.display()))?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create symlink parent {}", parent.display()))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst)
            .with_context(|| format!("symlink {} -> {}", dst.display(), target.display()))?;
    }
    #[cfg(windows)]
    {
        let target_abs = src.parent().unwrap_or_else(|| Path::new("")).join(&target);
        if target_abs.is_dir() {
            std::os::windows::fs::symlink_dir(&target, dst).with_context(|| {
                format!("symlink_dir {} -> {}", dst.display(), target.display())
            })?;
        } else {
            std::os::windows::fs::symlink_file(&target, dst).with_context(|| {
                format!("symlink_file {} -> {}", dst.display(), target.display())
            })?;
        }
    }
    Ok(())
}

fn should_ignore_dir(name: &OsStr, options: &ShadowPatchOptions) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    options.ignored_dir_names.contains(name)
}

/// Read a file's bytes with the per-file size cap. Unlike a UTF-8 read this never
/// fails on binary content — `compute_shadow_workspace_patch` decides per file whether
/// the bytes are text (diffable) or binary (skipped from the text patch), so a single
/// pre-existing binary file (image, PDF, …) can't abort the whole diff. The size cap is
/// still enforced (huge files don't belong in a proposal diff).
fn read_bytes_bounded_local(path: &Path, max_file_bytes: u64) -> Result<Vec<u8>> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("stat {} for diff", path.display()))?;
    if metadata.len() > max_file_bytes {
        return Err(anyhow!(
            "file {} is {} bytes; refusing to include in coding proposal diff (cap {} bytes)",
            path.display(),
            metadata.len(),
            max_file_bytes
        ));
    }
    std::fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn diff_path_string(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("path {} is not workspace-relative", path.display()));
            },
        }
    }
    if parts.is_empty() {
        return Err(anyhow!("empty diff path"));
    }
    Ok(parts.join("/"))
}

fn rewrite_first_header(diff: String, expected_prefix: &str, replacement: &str) -> Result<String> {
    rewrite_header_line(diff, 0, expected_prefix, replacement)
}

fn rewrite_second_header(diff: String, expected_prefix: &str, replacement: &str) -> Result<String> {
    rewrite_header_line(diff, 1, expected_prefix, replacement)
}

fn rewrite_header_line(
    diff: String,
    line_index: usize,
    expected_prefix: &str,
    replacement: &str,
) -> Result<String> {
    let mut lines: Vec<&str> = diff.split_inclusive('\n').collect();
    let Some(line) = lines.get_mut(line_index) else {
        return Err(anyhow!(
            "cannot rewrite diff header: missing line {line_index}"
        ));
    };
    if !line.starts_with(expected_prefix) {
        return Err(anyhow!(
            "cannot rewrite diff header line {line_index}: expected prefix `{expected_prefix}`, got `{}`",
            line.trim_end()
        ));
    }
    *line = replacement;
    Ok(lines.concat())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn write_bytes(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn shadow_patch_skips_binary_files_instead_of_aborting() {
        // RCA regression: an UNCHANGED binary file present in both trees used to abort the
        // ENTIRE patch (read_text_bounded_local's `String::from_utf8` error → `?`), failing
        // the whole build run. Now it is byte-skipped and text changes still stage; a
        // genuinely-changed/added binary is skipped from the text patch (counted), not errored.
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        let bin = [0xFFu8, 0xFE, 0x00, 0x80, 0x01]; // invalid UTF-8
        write_bytes(&real.join(".wu/media/x.pdf"), &bin); // unchanged binary in BOTH trees
        write_bytes(&shadow.join(".wu/media/x.pdf"), &bin);
        write_bytes(&shadow.join("img.png"), &[0x89, 0x50, 0x4E, 0x47, 0x00]); // added binary
        write(&shadow.join("NOTE.md"), "added text\n"); // added text — MUST still stage

        let patch =
            compute_shadow_workspace_patch(&real, &shadow, &ShadowPatchOptions::default()).unwrap();

        assert!(
            patch.contains("+++ b/NOTE.md") && patch.contains("+added text"),
            "text change must still stage; got: {patch}"
        );
        assert!(!patch.contains("x.pdf"), "unchanged binary must be skipped");
        assert!(
            !patch.contains("img.png"),
            "changed/added binary must be skipped, not abort the diff"
        );
    }

    #[test]
    fn shadow_patch_ignores_home_config_and_credential_dirs() {
        // The scope-home `.` project carries gcloud logs (300k+ files), ssh keys, and
        // tool caches that are NOT project source — they must never flood (or leak
        // into) a proposal diff; only the real change should stage.
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        // Baseline file present in BOTH trees so the workspace dirs exist (and there's
        // an unchanged file the diff must skip).
        write(&real.join("keep.txt"), "base\n");
        write(&shadow.join("keep.txt"), "base\n");
        write(
            &shadow.join(".config/gcloud/logs/2026.06.02/x.log"),
            "gcloud noise\n",
        );
        write(&shadow.join(".ssh/id_rsa"), "PRIVATE KEY MATERIAL\n");
        write(&shadow.join(".cargo/registry/cache/y"), "cache\n");
        write(&shadow.join("SMOKE.md"), "real change\n");

        let patch =
            compute_shadow_workspace_patch(&real, &shadow, &ShadowPatchOptions::default()).unwrap();

        assert!(
            patch.contains("+++ b/SMOKE.md"),
            "the real change must stage; got: {patch}"
        );
        assert!(
            !patch.contains(".config"),
            "home config dir must be ignored"
        );
        assert!(
            !patch.contains("id_rsa") && !patch.contains(".ssh"),
            "credentials must be ignored, never staged into a proposal"
        );
        assert!(!patch.contains(".cargo"), "tool cache must be ignored");
    }

    #[test]
    fn classifies_pi_rpc_events() {
        let raw = serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "text_delta",
                "delta": "hello"
            }
        });
        let event = coding_event_from_raw(7, raw);
        assert_eq!(event.sequence, 7);
        assert_eq!(event.kind, CodingEngineEventKind::MessageUpdate);
        assert_eq!(event.text_delta.as_deref(), Some("hello"));
    }

    #[test]
    fn classifies_pi_083_session_lifecycle_events() {
        let cases = [
            ("agent_settled", CodingEngineEventKind::AgentSettled),
            ("entry_appended", CodingEngineEventKind::EntryAppended),
            (
                "session_info_changed",
                CodingEngineEventKind::SessionInfoChanged,
            ),
            (
                "thinking_level_changed",
                CodingEngineEventKind::ThinkingLevelChanged,
            ),
            (
                "summarization_retry_scheduled",
                CodingEngineEventKind::SummarizationRetryScheduled,
            ),
            (
                "summarization_retry_attempt_start",
                CodingEngineEventKind::SummarizationRetryAttemptStart,
            ),
            (
                "summarization_retry_finished",
                CodingEngineEventKind::SummarizationRetryFinished,
            ),
            (
                "bash_execution_update",
                CodingEngineEventKind::BashExecutionUpdate,
            ),
        ];

        for (index, (raw_type, expected)) in cases.into_iter().enumerate() {
            let event = coding_event_from_raw(index + 1, serde_json::json!({ "type": raw_type }));
            assert_eq!(event.kind, expected, "wrong kind for {raw_type}");
            assert_eq!(event.raw_type.as_deref(), Some(raw_type));
        }
    }

    #[test]
    fn shadow_patch_includes_modify_create_delete_and_ignores_build_dirs() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        write(&real.join("src/main.rs"), "hello\n");
        write(&real.join("old.txt"), "old\n");
        write(&real.join("target/debug/cache.txt"), "real cache\n");
        write(&shadow.join("src/main.rs"), "hello world\n");
        write(&shadow.join("new.txt"), "new\n");
        write(&shadow.join("target/debug/cache.txt"), "shadow cache\n");

        let patch =
            compute_shadow_workspace_patch(&real, &shadow, &ShadowPatchOptions::default()).unwrap();

        assert!(patch.contains("--- a/src/main.rs"));
        assert!(patch.contains("+++ b/src/main.rs"));
        assert!(patch.contains("-hello"));
        assert!(patch.contains("+hello world"));
        assert!(patch.contains("--- /dev/null"));
        assert!(patch.contains("+++ b/new.txt"));
        assert!(patch.contains("--- a/old.txt"));
        assert!(patch.contains("+++ /dev/null"));
        assert!(!patch.contains("target/debug/cache.txt"));
    }

    #[test]
    fn prepare_shadow_workspace_copies_files_and_ignores_generated_dirs() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        write(&real.join("src/main.rs"), "hello\n");
        write(&real.join("target/debug/cache.txt"), "cache\n");

        prepare_shadow_workspace(&real, &shadow, &ShadowPatchOptions::default()).unwrap();

        assert_eq!(
            std::fs::read_to_string(shadow.join("src/main.rs")).unwrap(),
            "hello\n"
        );
        assert!(!shadow.join("target/debug/cache.txt").exists());
    }

    fn empty_run_result() -> CodingEngineRunResult {
        CodingEngineRunResult {
            engine: CodingEngineKind::Pi,
            session_id: Some("sess-1".to_string()),
            session_file: None,
            assistant_text: None,
            event_count: 0,
            continuation: None,
            continuation_fresh_reason: None,
            proposal: None,
            approval_payload: None,
            session_stats: None,
        }
    }

    #[test]
    fn common_handler_stages_a_shadow_diff_and_skips_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        let scope = tmp.path().join("scope");
        std::fs::create_dir_all(&scope).unwrap();
        write(&real.join("keep.txt"), "base\n");
        write(&shadow.join("keep.txt"), "base\n");
        write(&shadow.join("NOTE.md"), "added\n");

        let mut request = CodingEngineRequest::new(
            "fix it",
            &real,
            &shadow,
            &scope,
            TransactionScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
        );
        let mut result = empty_run_result();
        attach_staged_coding_proposal(&request, &mut result).unwrap();
        let proposal = result.proposal.expect("text change must stage");
        assert_eq!(proposal.source_session_id, "sess-1");
        assert!(result.approval_payload.is_some());

        request.stage_result = false;
        let mut skipped = empty_run_result();
        attach_staged_coding_proposal(&request, &mut skipped).unwrap();
        assert!(skipped.proposal.is_none());
        assert!(skipped.approval_payload.is_none());
    }

    #[test]
    fn common_handler_does_not_replace_an_existing_proposal() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        let scope = tmp.path().join("scope");
        std::fs::create_dir_all(&scope).unwrap();
        write(&real.join("keep.txt"), "base\n");
        write(&shadow.join("keep.txt"), "base\n");
        write(&shadow.join("NOTE.md"), "first\n");

        let request = CodingEngineRequest::new(
            "fix it",
            &real,
            &shadow,
            &scope,
            TransactionScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
        );
        let mut result = empty_run_result();
        attach_staged_coding_proposal(&request, &mut result).unwrap();
        let first_id = result.proposal.as_ref().expect("first stage").id.clone();
        write(&shadow.join("NOTE.md"), "second\n");
        attach_staged_coding_proposal(&request, &mut result).unwrap();
        assert_eq!(
            result.proposal.as_ref().expect("kept").id,
            first_id,
            "a later attach must not restage over an existing proposal"
        );
    }

    #[test]
    fn run_result_carries_event_count_not_a_duplicate_event_vector() {
        let whole = include_str!("mod.rs");
        // Cut at the test module itself: test-only helpers further up carry
        // `#[cfg(any(test, ...))]` too, so the first such attribute is not
        // where production code ends.
        let source = whole
            .find("\nmod tests {")
            .map(|at| &whole[..at])
            .expect("production coding-engine source");
        assert!(
            source.contains("pub event_count: u64"),
            "the common result must report a terminal event count"
        );
        assert!(
            !source.contains("pub events: Vec<CodingEngineEvent>"),
            "the common result must not retain a second event vector"
        );
        let mut result = empty_run_result();
        result.event_count = 10_000;
        assert_eq!(result.event_count, 10_000);
        assert!(result.continuation.is_none());
    }

    #[test]
    fn deny_fence_rejects_live_repo_source_allows_sandbox_and_external() {
        // Simulate the dangerous default layout: the scope sandbox nested under a
        // live magician repo (`<repo>/magician_data_v3/scopes/p/w/workdirs/home`),
        // the repo being a real git repo (`<repo>/.git`).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("magician");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("magician/src")).unwrap();
        let home = repo.join("magician_data_v3/scopes/anonymous/default/workdirs/home");
        std::fs::create_dir_all(&home).unwrap();
        let external = tmp.path().join("external_project");
        std::fs::create_dir_all(&external).unwrap();

        // (1) a live-repo SOURCE subdir → REJECTED by the deny-fence.
        let err =
            resolve_coding_repo_binding(&home, Some(repo.join("magician/src").to_str().unwrap()))
                .unwrap_err();
        assert!(
            err.contains("live magician repository source tree"),
            "expected deny-fence rejection, got: {err}"
        );
        // (1b) the repo root itself → REJECTED.
        assert!(resolve_coding_repo_binding(&home, Some(repo.to_str().unwrap())).is_err());

        // (2) the scope sandbox home (".") → ALLOWED.
        assert!(resolve_coding_repo_binding(&home, Some(".")).is_ok());

        // (3) a genuine external project dir (outside the repo) → ALLOWED.
        assert!(resolve_coding_repo_binding(&home, Some(external.to_str().unwrap())).is_ok());
    }

    fn harness_request() -> CodingEngineRequest {
        CodingEngineRequest::new(
            "fix it",
            "/work/real",
            "/scope/coding_engine/worktrees/w1",
            "/scope",
            TransactionScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
        )
    }

    #[test]
    fn discuss_turns_are_read_only_for_every_engine() {
        let mut request = harness_request();
        for engine in [
            CodingEngineKind::CodexAppServer,
            CodingEngineKind::GrokAcp,
            CodingEngineKind::ClaudeCode,
            CodingEngineKind::AgyCli,
            CodingEngineKind::Pi,
        ] {
            assert!(
                !request.read_only_turn(engine),
                "{engine:?} builds by default"
            );
        }
        request.codex.mode = factory::CodexTurnMode::Discuss;
        request.grok.mode = factory::GrokTurnMode::Discuss;
        request.claude.mode = factory::ClaudeTurnMode::Discuss;
        request.agy.mode = factory::AgyTurnMode::Discuss;
        request.pi.plan_only = true;
        for engine in [
            CodingEngineKind::CodexAppServer,
            CodingEngineKind::GrokAcp,
            CodingEngineKind::ClaudeCode,
            CodingEngineKind::AgyCli,
            CodingEngineKind::Pi,
        ] {
            assert!(
                request.read_only_turn(engine),
                "{engine:?} discuss is read-only"
            );
        }
    }

    /// Only the engines that sign in through the login keychain may read it
    /// in their box; the rest keep it unreadable.
    #[test]
    fn only_keychain_signin_engines_read_the_keychain() {
        for (engine, readable) in [
            (CodingEngineKind::ClaudeCode, true),
            (CodingEngineKind::AgyCli, true),
            (CodingEngineKind::CodexAppServer, false),
            (CodingEngineKind::GrokAcp, false),
            (CodingEngineKind::Pi, false),
        ] {
            assert_eq!(keychain_signin(engine), readable, "{engine:?}");
        }
    }

    #[test]
    fn a_harness_box_writes_its_copy_and_its_own_state_only() {
        let request = harness_request();
        let build = harness_box(CodingEngineKind::Pi, Path::new("/bin/pi"), &request, false);
        assert!(build
            .writable
            .contains(&PathBuf::from("/scope/coding_engine/worktrees/w1")));
        assert!(build
            .writable
            .contains(&PathBuf::from("/scope/coding_engine/pi_sessions")));
        assert!(
            !build
                .writable
                .iter()
                .any(|dir| dir.starts_with("/work/real")),
            "the real repo stays read-only"
        );
        let discuss = harness_box(
            CodingEngineKind::ClaudeCode,
            Path::new("/bin/claude"),
            &request,
            true,
        );
        assert!(!discuss
            .writable
            .contains(&PathBuf::from("/scope/coding_engine/worktrees/w1")));
        if let Some(home) = dirs::home_dir() {
            assert!(discuss.writable.contains(&home.join(".claude")));
            assert_eq!(
                discuss.writable_regex.len(),
                1,
                "~/.claude.json and its backups"
            );
        }
    }

    #[test]
    fn a_boxed_grok_writes_its_runtime_state_but_not_its_config_or_the_owner_grok() {
        let request = harness_request();
        let harness = harness_box(
            CodingEngineKind::GrokAcp,
            Path::new("/bin/grok"),
            &request,
            false,
        );
        let grok_home = grok_contract::magician_grok_home_dir();
        let grok_home = grok_home.canonicalize().unwrap_or(grok_home);
        assert!(harness.writable.contains(&grok_home.join("sessions")));
        assert!(harness.writable.contains(&grok_home.join("logs")));
        assert!(
            !harness.writable.contains(&grok_home),
            "the home root holds config, hooks and plugins"
        );
        assert!(!harness
            .writable
            .iter()
            .any(|dir| dir.ends_with("hooks") || dir.ends_with("installed-plugins")));
        if let Some(home) = dirs::home_dir() {
            assert!(
                !harness.writable.contains(&home.join(".grok")),
                "the owner's Grok runs unboxed"
            );
        }
        assert!(harness
            .writable_regex
            .contains(&grok_contract::grok_home_writable_regex(&grok_home)));
        if let Some(owner) = grok_contract::owner_grok_home() {
            let owner = owner.canonicalize().unwrap_or(owner);
            assert!(harness
                .writable_regex
                .contains(&grok_contract::owner_auth_writable_regex(&owner)));
            assert_eq!(
                harness.writable_regex.len(),
                2,
                "{:?}",
                harness.writable_regex
            );
        }
        assert!(harness_state_dirs(CodingEngineKind::GrokAcp).is_empty());
    }

    #[test]
    fn a_harness_box_denies_writes_to_the_real_repo_even_under_a_writable_root() {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let request = harness_request();
        let harness = CODING_RUN_REAL_REPO.sync_scope(repo.clone(), || {
            harness_box(
                CodingEngineKind::ClaudeCode,
                Path::new("/bin/claude"),
                &request,
                false,
            )
        });
        assert_eq!(harness.deny_write, vec![repo.canonicalize().unwrap()]);
        let unscoped = harness_box(
            CodingEngineKind::ClaudeCode,
            Path::new("/bin/claude"),
            &request,
            false,
        );
        assert!(
            unscoped.deny_write.is_empty(),
            "no coding run, no real repo to fence"
        );
    }

    #[test]
    fn unrestricted_launches_drop_the_named_flags_and_their_values() {
        let args: Vec<OsString> = [
            "-p",
            "hi",
            "--setting-sources",
            "",
            "--safe-mode",
            "--disallowedTools",
            "WebSearch",
            "--verbose",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let kept = without_flags(
            args,
            &["--safe-mode"],
            &["--setting-sources", "--disallowedTools"],
        );
        assert_eq!(kept, ["-p", "hi", "--verbose"].map(OsString::from).to_vec());
    }

    #[test]
    fn a_harness_outside_a_coding_run_is_launched_as_is() {
        // No coding context: neither the box nor the profile applies, so the
        // engine keeps its own sandbox.
        assert!(!harness_own_sandbox_off());
        let harness = harness_command(
            CodingEngineKind::GrokAcp,
            Path::new("grok"),
            &[OsString::from("agent")],
            &harness_request(),
            false,
        );
        assert!(harness.env.is_empty());
        assert_eq!(harness.command.as_std().get_program(), "grok");
    }

    #[test]
    fn os_sandbox_wrap_passes_through_when_unarmed() {
        // The OS-sandbox gate fails OPEN: a unit test runs outside any
        // `with_coding_context` scope, so `coding_context_active()` is false and
        // the gate short-circuits (never even reaching the launcher probe) →
        // command passes through UNCHANGED. Locks the contract that a general
        // (non-coding) spawn is never sandboxed.
        let program = std::ffi::OsString::from("sh");
        let args = vec![
            std::ffi::OsString::from("-c"),
            std::ffi::OsString::from("echo hi"),
        ];
        let (prog, out) = super::os_sandbox_wrap(&program, &args);
        assert_eq!(prog, program);
        assert_eq!(out, args);
    }

    #[test]
    fn coding_shadow_root_default_nests_under_scope() {
        // Default (no MAGICIAN_CODING_SANDBOX_ROOT relocation): the shadow nests
        // under the scope root, keeping it writable under the OS-sandbox gate.
        if std::env::var_os("MAGICIAN_CODING_SANDBOX_ROOT").is_some() {
            return; // relocation override active in this env — default N/A
        }
        let scope = std::path::Path::new("/tmp/scope");
        let real = std::path::Path::new("/work/myrepo");
        let shadow = coding_shadow_root(scope, real);
        assert!(shadow.starts_with(scope.join("coding_engine").join("worktrees")));
        assert!(shadow.ends_with(persistent_shadow_key(real)));
    }

    #[test]
    fn reject_repo_source_tree_matrix() {
        // Pure-predicate coverage of the shared shell/repo deny-fence — including
        // the degenerate empty-root case that the relative-`--config` regression
        // hit (an empty root is a `starts_with` prefix of every path).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("magician");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let sandbox = repo.join("magician_data_v3");
        std::fs::create_dir_all(sandbox.join("scopes/p/w/workdirs/home")).unwrap();
        let external = tmp.path().join("external_project");
        std::fs::create_dir_all(&external).unwrap();

        // live-repo root + a source subdir → REJECTED
        assert!(reject_repo_source_tree(&repo, &repo, &sandbox).is_err());
        assert!(reject_repo_source_tree(&repo.join("src/main.rs"), &repo, &sandbox).is_err());
        // the scope sandbox base + a deep sandbox path → ALLOWED (nested in repo
        // but it IS the storage base)
        assert!(reject_repo_source_tree(&sandbox, &repo, &sandbox).is_ok());
        assert!(reject_repo_source_tree(
            &sandbox.join("scopes/p/w/workdirs/home"),
            &repo,
            &sandbox
        )
        .is_ok());
        // a genuine external dir → ALLOWED
        assert!(reject_repo_source_tree(&external, &repo, &sandbox).is_ok());
        // degenerate empty root → ALLOWED (fail open; must never reject everything)
        assert!(reject_repo_source_tree(&repo, Path::new(""), &sandbox).is_ok());
        // a non-git root → ALLOWED (the fence only fires for a real git repo)
        let nongit = tmp.path().join("not_a_repo");
        std::fs::create_dir_all(&nongit).unwrap();
        assert!(reject_repo_source_tree(
            &nongit.join("x"),
            &nongit,
            &nongit.join("magician_data_v3")
        )
        .is_ok());
    }

    #[test]
    fn sync_persistent_workspace_resyncs_prunes_strays_and_keeps_caches() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let shadow = tmp.path().join("shadow");
        write(&real.join("src/main.rs"), "fn main() {}\n");
        write(&real.join("README.md"), "real\n");

        // A shadow left over from a prior run: a STALE tracked file, a cached
        // install dir, a stray file, and a stray dir.
        write(&shadow.join("src/main.rs"), "STALE\n");
        write(&shadow.join("node_modules/lib.js"), "cached\n");
        write(&shadow.join("stray.txt"), "pi-made\n");
        write(&shadow.join("scratch/tmp.txt"), "junk\n");

        sync_persistent_workspace(&real, &shadow, &ShadowPatchOptions::default()).unwrap();

        // Tracked files resynced from the real repo.
        assert_eq!(
            std::fs::read_to_string(shadow.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(shadow.join("README.md")).unwrap(),
            "real\n"
        );
        // The ignored cache dir survives the sync (no cold reinstall next run).
        assert!(shadow.join("node_modules/lib.js").exists());
        // Strays from the prior run are pruned so they don't pollute the diff.
        assert!(!shadow.join("stray.txt").exists());
        assert!(!shadow.join("scratch").exists());
    }

    #[test]
    fn proposal_pending_approval_payload_uses_proposal_id() {
        let tmp = TempDir::new().unwrap();
        let store = CodeChangeProposalStore::new(tmp.path());
        let proposal = store
            .stage_patch(
                TransactionScope {
                    principal: "anonymous".to_string(),
                    workspace: "default".to_string(),
                },
                "Update greeting",
                "\
--- a/hello.txt
+++ b/hello.txt
@@ -1,1 +1,1 @@
-hello
+hello world
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        let payload = proposal_pending_approval_response(&proposal);

        assert_eq!(payload["status"], "pending_approval");
        assert_eq!(payload["pause_kind"], "diff_approval");
        assert_eq!(payload["proposal_id"], proposal.id.as_str());
        assert_eq!(
            payload["pause_input_type"]["proposal_id"],
            proposal.id.as_str()
        );
        assert!(payload["pause_input_type"]["transaction_id"].is_null());
    }

    // ---- §15 item 7: the coding sandbox denies credential reads --------------

    fn profile_for(tmp: &Path, home: Option<&Path>) -> String {
        super::macos_sandbox_profile(
            Some((&tmp.join("repo"), &tmp.join("shadow"))),
            Some(&tmp.join("real")),
            home,
        )
    }

    #[test]
    fn a_packaged_install_profile_still_fences_the_real_repo_and_credentials() {
        // No source checkout: no live-repo rules, but the run's real repo stays
        // read-only and credential reads stay denied. The sandbox used to skip
        // wrapping entirely here, so a packaged Pi ran with no fence at all.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let real = tmp.path().join("real");
        let profile = super::macos_sandbox_profile(None, Some(&real), Some(&home));
        assert!(profile.starts_with("(version 1)\n(allow default)\n"));
        assert!(!profile.contains("allow file-write*"), "{profile}");
        assert!(profile.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            sbpl_escape_path(&real)
        )));
        assert!(profile.contains("Library/Keychains"));
        assert!(profile.contains(r"\.vault$"));
    }

    /// The packaged-install shape, live: no source checkout (the test process
    /// never arms the live-repo fence), a coding context, and the run's real
    /// repo scoped. The spawn must be wrapped, a write into the real repo must
    /// be denied, and a write elsewhere must succeed.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn without_a_source_checkout_a_coding_spawn_is_still_fenced_from_the_real_repo() {
        if super::live_repo_source_fence().is_some() || !super::os_sandbox_available() {
            eprintln!("skipped: a live fence is armed or the sandbox launcher is unavailable");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let real = root.join("real-repo");
        let work = root.join("work");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let script = format!(
            "echo denied > '{}/escape.txt'; echo allowed > '{}/ok.txt'",
            real.display(),
            work.display()
        );
        let (program, args) = super::with_coding_context(
            true,
            super::with_coding_real_repo(real.clone(), async {
                super::os_sandbox_wrap(
                    std::ffi::OsStr::new("/bin/sh"),
                    &[
                        std::ffi::OsString::from("-c"),
                        std::ffi::OsString::from(script),
                    ],
                )
            }),
        )
        .await;
        assert_eq!(program, std::ffi::OsString::from("/usr/bin/sandbox-exec"));
        let _ = std::process::Command::new(&program)
            .args(&args)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            !real.join("escape.txt").exists(),
            "the real repo was writable"
        );
        assert!(
            work.join("ok.txt").is_file(),
            "an ordinary write was denied"
        );
    }

    #[test]
    fn pi_refuses_to_spawn_unsandboxed_unless_the_operator_opted_out() {
        // Outside a coding context nothing wraps, which is exactly the state a
        // Pi turn must not spawn in.
        assert!(!super::os_sandbox_will_wrap());
        assert_eq!(
            super::require_pi_sandbox(false),
            Err(super::CodingFenceError::PiSandboxUnavailable)
        );
        assert!(super::CodingFenceError::PiSandboxUnavailable
            .to_string()
            .contains("coding.pi_allow_unsandboxed"));
        assert_eq!(super::require_pi_sandbox(true), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn the_build_dir_is_pruned_only_below_the_floor_and_never_under_a_running_cargo() {
        use fs2::FileExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("cargo-target");
        let debug = dir.join("debug");
        std::fs::create_dir_all(debug.join("deps")).unwrap();
        std::fs::write(debug.join("deps/libbig.rlib"), b"x").unwrap();
        std::fs::write(debug.join(".cargo-lock"), b"").unwrap();

        assert!(super::free_disk_bytes(tmp.path()).unwrap() > 0);
        // Plenty of room: untouched. A floor of 0 disables the prune.
        assert!(!super::prune_build_dir_when_low(&dir, 1).unwrap());
        assert!(!super::prune_build_dir_when_low(&dir, 0).unwrap());

        // Below an impossible floor, but a cargo holds the lock: untouched.
        let running = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(debug.join(".cargo-lock"))
            .unwrap();
        running.lock_exclusive().unwrap();
        assert!(!super::prune_build_dir_when_low(&dir, u64::MAX).unwrap());
        assert!(debug.join("deps/libbig.rlib").is_file());

        // Lock released: emptied, the dir itself kept.
        running.unlock().unwrap();
        drop(running);
        assert!(super::prune_build_dir_when_low(&dir, u64::MAX).unwrap());
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn the_shared_build_dir_defaults_beside_the_shadows_and_honours_config() {
        let tmp = TempDir::new().unwrap();
        let scope = tmp.path().join("scope");
        let default = super::coding_build_dir(&scope, None).expect("default");
        assert!(default.ends_with("coding_engine/build/cargo-target"));
        assert!(default.is_dir());
        assert_eq!(
            super::coding_build_dir(&scope, Some("   ")).unwrap(),
            default,
            "a blank setting is unset"
        );

        let configured = tmp.path().join("elsewhere/target");
        let dir = super::coding_build_dir(&scope, configured.to_str()).expect("configured");
        assert_eq!(dir, configured);
        assert!(dir.is_dir());

        let err = super::coding_build_dir(&scope, Some("relative/target")).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn sandbox_profile_keeps_the_write_fence_and_adds_credential_read_denies() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let profile = profile_for(tmp.path(), Some(&home));

        // The existing shape is unchanged.
        assert!(profile.starts_with("(version 1)\n(allow default)\n"));
        for (rule, path) in [
            ("deny file-write*", tmp.path().join("repo")),
            ("allow file-write*", tmp.path().join("shadow")),
            ("deny file-write*", tmp.path().join("real")),
        ] {
            let expected = format!("({rule} (subpath \"{}\"))", sbpl_escape_path(&path));
            assert!(
                profile.contains(&expected),
                "missing {expected} in:\n{profile}"
            );
        }

        // The two credential read denies, placed after `(allow default)` because
        // SBPL is last-match-wins.
        let keychain = format!(
            "(deny file-read* (subpath \"{}\"))",
            sbpl_escape_path(&home.join("Library").join("Keychains"))
        );
        assert!(
            profile.contains(&keychain),
            "missing keychain deny in:\n{profile}"
        );
        assert!(profile.contains(
            r#"(deny file-read* (regex #"/(provisioned_secrets|captured_secrets|mcp_oauth)\.vault$"))"#
        ));
        assert!(profile.contains(r#"(deny file-read* (regex #"/secret_audit\.jsonl$"))"#));
        assert!(profile.find("(allow default)").unwrap() < profile.find(&keychain).unwrap());

        // Deliberately absent: measured ineffective against the env read and it
        // breaks process tooling.
        assert!(
            !profile.contains("process-info"),
            "process-info deny must not return"
        );
    }

    #[test]
    fn a_keychain_allowance_lifts_only_the_keychain_deny() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let real = tmp.path().join("real");
        let profile = super::macos_sandbox_profile_with(
            None,
            Some(&real),
            Some(&home),
            &super::SandboxAllowances {
                keychain_read: true,
                ..Default::default()
            },
            None,
        );
        assert!(!profile.contains("Keychains"), "{profile}");
        assert!(profile.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            sbpl_escape_path(&real)
        )));
        assert!(profile.contains(
            r#"(deny file-read* (regex #"/(provisioned_secrets|captured_secrets|mcp_oauth)\.vault$"))"#
        ));
        // The default keeps the deny.
        assert!(super::macos_sandbox_profile(None, Some(&real), Some(&home)).contains("Keychains"));
    }

    #[test]
    fn the_write_allow_list_denies_everything_but_its_roots_and_keeps_the_repo_fence_last() {
        let tmp = TempDir::new().unwrap();
        let clone = tmp.path().join("clone");
        let real = tmp.path().join("real");
        let state = tmp.path().join("home/.claude.json*");
        let profile = super::macos_sandbox_profile_with(
            Some((&tmp.path().join("src"), &tmp.path().join("src/data"))),
            Some(&real),
            None,
            &super::SandboxAllowances::default(),
            Some(&[clone.clone(), state]),
        );
        let deny_all = profile
            .find("(deny file-write*)\n")
            .expect("deny all writes");
        let allow_clone = profile
            .find(&format!(
                "(allow file-write* (subpath \"{}\"))",
                sbpl_escape_path(&clone)
            ))
            .expect("clone allowed");
        let real_fence = profile
            .find(&format!(
                "(deny file-write* (subpath \"{}\"))",
                sbpl_escape_path(&real)
            ))
            .expect("real repo fenced");
        assert!(
            deny_all < allow_clone && allow_clone < real_fence,
            "{profile}"
        );
        assert!(profile.contains(r#"home/\.claude\.json"))"#), "{profile}");
        // The broad live-fence re-allow of the sandbox base is not used: only
        // listed roots are writable.
        assert!(
            !profile.contains(&sbpl_escape_path(&tmp.path().join("src/data"))),
            "{profile}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_write_allow_list_holds_live() {
        let launcher = Path::new("/usr/bin/sandbox-exec");
        if !launcher.exists() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let clone = root.join("clone");
        let real = clone.join("nested-real");
        let outside = root.join("outside");
        for dir in [&clone, &real, &outside] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let profile = super::macos_sandbox_profile_with(
            None,
            Some(&real),
            None,
            &super::SandboxAllowances::default(),
            Some(&[clone.clone(), root.join("state.json*")]),
        );
        let try_write = |path: &Path| {
            std::process::Command::new(launcher)
                .args(["-p", &profile, "/bin/sh", "-c"])
                .arg(format!("echo x > '{}'", path.display()))
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        assert!(
            try_write(&clone.join("ok.txt")),
            "a listed root is writable"
        );
        assert!(
            try_write(&root.join("state.json.lock")),
            "a prefix entry is writable"
        );
        assert!(
            !try_write(&outside.join("no.txt")),
            "outside the list is denied"
        );
        assert!(
            !try_write(&real.join("no.txt")),
            "the real repo stays fenced inside a root"
        );
        assert!(
            try_write(Path::new("/dev/null")),
            "/dev/null stays writable"
        );
    }

    #[test]
    fn sandbox_profile_without_a_home_still_denies_vault_reads() {
        let tmp = TempDir::new().unwrap();
        let profile = profile_for(tmp.path(), None);
        assert!(!profile.contains("Library/Keychains"));
        assert!(profile.contains(r"secret_audit\.jsonl"));
        assert!(profile.contains(r"\.vault$"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generated_sandbox_profile_blocks_keychain_and_vault_reads_live() {
        // Runs the GENERATED profile under sandbox-exec against real files. Skips
        // where the launcher is missing or refuses a trivial profile, exactly as
        // the production self-test does.
        let launcher = Path::new("/usr/bin/sandbox-exec");
        if !launcher.exists() {
            return;
        }
        let trivial_ok = std::process::Command::new(launcher)
            .args([
                "-p",
                "(version 1)(allow default)",
                "/bin/sh",
                "-c",
                "exit 0",
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !trivial_ok {
            return;
        }

        // Canonical root: SBPL `subpath` matches the resolved path, and macOS
        // temp dirs live under `/var/folders`, a symlink to `/private/var/folders`.
        // The production caller resolves the same way.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let keychain = home.join("Library/Keychains/login.keychain-db");
        write(&keychain, "not-a-real-keychain");
        let vault = root.join("scope/secrets/provisioned_secrets.vault");
        write(&vault, "not-ciphertext");
        let journal = root.join("scope/secrets/secret_audit.jsonl");
        write(&journal, "{}\n");
        let plain = root.join("scope/secrets/notes.txt");
        write(&plain, "readable");

        let profile = profile_for(&root, Some(&home));
        let readable = |path: &Path| {
            std::process::Command::new(launcher)
                .args([
                    "-p",
                    &profile,
                    "/bin/sh",
                    "-c",
                    &format!("head -c1 '{}' >/dev/null 2>&1", path.display()),
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        assert!(!readable(&keychain), "keychain database must be denied");
        assert!(!readable(&vault), "vault partition must be denied");
        assert!(!readable(&journal), "audit journal must be denied");
        assert!(
            readable(&plain),
            "an unrelated file in the same directory must stay readable"
        );
    }

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BwrapAccess {
        Write,
        ReadOnly,
        Hidden,
    }

    /// What a path is under a bwrap argv: mounts apply in order, a mount
    /// hides every earlier mount at or below it, and the deepest surviving
    /// mount above a path decides.
    #[cfg(test)]
    fn bwrap_model(argv: &[OsString], path: &Path) -> BwrapAccess {
        let argv: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let end = argv
            .iter()
            .position(|arg| arg == "--")
            .expect("`--` ends the options");
        let mut mounts: Vec<(PathBuf, BwrapAccess)> = Vec::new();
        let mut mount = |dest: &str, access: BwrapAccess| {
            let dest = PathBuf::from(dest);
            mounts.retain(|(earlier, _)| !earlier.starts_with(&dest));
            mounts.push((dest, access));
        };
        let mut i = 0;
        while i < end {
            match argv[i].as_str() {
                "--bind" | "--bind-try" => {
                    mount(&argv[i + 2], BwrapAccess::Write);
                    i += 3;
                },
                "--ro-bind" | "--ro-bind-try" => {
                    let access = if argv[i + 1] == "/dev/null" && argv[i + 2] != "/dev/null" {
                        BwrapAccess::Hidden
                    } else {
                        BwrapAccess::ReadOnly
                    };
                    mount(&argv[i + 2], access);
                    i += 3;
                },
                "--tmpfs" => {
                    mount(&argv[i + 1], BwrapAccess::Hidden);
                    i += 2;
                },
                "--dev" | "--proc" | "--remount-ro" => i += 2,
                _ => i += 1,
            }
        }
        mounts
            .iter()
            .filter(|(dest, _)| path.starts_with(dest))
            .max_by_key(|(dest, _)| dest.components().count())
            .map(|(_, access)| *access)
            .unwrap_or(BwrapAccess::Hidden)
    }

    /// The goal workspace mounts bind a member workspace back from the host
    /// last, which drops the mounts made inside it before: its read-only
    /// roots and hidden folders are mounted again after them.
    #[test]
    fn bwrap_masks_inside_a_member_workspace_hold_after_the_workspace_mounts() {
        let ws = PathBuf::from("/notes/Workspaces/Magican");
        let policy = BwrapPolicy {
            read_only: vec![ws.join("Archive")],
            hide_dirs: vec![ws.join("vault")],
            workspace_args: [
                "--tmpfs",
                "/notes/Workspaces",
                "--bind-try",
                "/notes/Workspaces/Magican",
                "/notes/Workspaces/Magican",
                "--remount-ro",
                "/notes/Workspaces",
            ]
            .map(OsString::from)
            .to_vec(),
            workspace_reopened: vec![ws.clone()],
            ..BwrapPolicy::default()
        };
        let argv = bwrap_args(&policy, OsStr::new("sh"), &[]);
        let access = |path: &str| bwrap_model(&argv, Path::new(path));
        assert_eq!(access("/notes/Workspaces/Magican/a.md"), BwrapAccess::Write);
        assert_eq!(
            access("/notes/Workspaces/Magican/Archive/a.md"),
            BwrapAccess::ReadOnly
        );
        assert_eq!(
            access("/notes/Workspaces/Magican/vault/key"),
            BwrapAccess::Hidden
        );
        assert_eq!(access("/notes/Workspaces/Other/a.md"), BwrapAccess::Hidden);
    }

    #[test]
    fn bwrap_allow_list_writes_only_its_roots_and_keeps_the_fences() {
        let home = Path::new("/home/me");
        let source = PathBuf::from("/home/me/dev/magician");
        let base = source.join("magician_data_v3");
        let shadow = base.join("scopes/a/b/coding_engine/worktrees/k");
        let clone = PathBuf::from("/home/me/Notes/scopes/a/b/coding_engine/runs/r1");
        let build = PathBuf::from("/home/me/Notes/scopes/a/b/coding_engine/build/cargo-target");
        let real = PathBuf::from("/home/me/code/proj");
        let secrets = PathBuf::from("/home/me/Notes/scopes/a/b/secrets");
        let roots = vec![
            clone.clone(),
            build.clone(),
            shadow.clone(),
            // An owner-allowed ancestor of the live source and of the real
            // repo: neither may reopen.
            PathBuf::from("/home/me/dev"),
            PathBuf::from("/home/me/code"),
        ];
        let state = engine_state_paths(CodingEngineKind::ClaudeCode, home);
        let identity = |path: &Path| path.to_path_buf();
        let policy = BwrapPolicy {
            write_allow: Some(write_allow_list(roots, Some(home), &state, &identity)),
            live_fence: Some((source.clone(), base.clone())),
            deny_real_repo: Some(real.clone()),
            hide_dirs: vec![secrets.clone()],
            hide_files: vec![PathBuf::from("/run/user/1000/bus")],
            read_only: Vec::new(),
            legacy_writable: Vec::new(),
            workspace_args: Vec::new(),
            workspace_reopened: Vec::new(),
            workspace_read_only: Vec::new(),
        };
        let argv = bwrap_args(
            &policy,
            OsStr::new("claude"),
            &[OsString::from("-p"), OsString::from("hi")],
        );
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&text[..2], ["--die-with-parent", "--unshare-pid"]);
        assert_eq!(&text[text.len() - 4..], ["--", "claude", "-p", "hi"]);
        assert!(!text
            .iter()
            .any(|arg| arg.starts_with("--unshare-net") || arg == "--unshare-all"));
        assert!(!text.iter().any(|arg| arg.contains('*')), "{text:?}");
        assert!(text.windows(2).any(|w| w == ["--proc", "/proc"]));
        let at = |path: &str| bwrap_model(&argv, Path::new(path));
        for writable in [
            "/home/me/Notes/scopes/a/b/coding_engine/runs/r1/src/lib.rs",
            "/home/me/Notes/scopes/a/b/coding_engine/build/cargo-target/debug/x",
            "/home/me/dev/magician/magician_data_v3/scopes/a/b/coding_engine/worktrees/k/f",
            "/home/me/dev/other/f",
            "/home/me/code/other/f",
            if cfg!(target_os = "macos") {
                "/private/tmp/x"
            } else {
                "/tmp/x"
            },
            "/home/me/.claude/sessions/s.jsonl",
            "/home/me/.claude.json",
            "/home/me/.cargo/registry/cache/x",
            "/home/me/.npm/_cacache/x",
        ] {
            assert_eq!(at(writable), BwrapAccess::Write, "{writable}\n{text:?}");
        }
        for read_only in [
            "/home/me/.bashrc",
            "/home/me/.ssh/authorized_keys",
            "/home/me/.local/bin/bwrap",
            "/home/me/.cargo/bin/cargo",
            "/etc/passwd",
            "/home/me/dev/magician/src/main.rs",
            "/home/me/dev/magician/magician_data_v3/scopes/a/b/vibedev/projects.json",
            "/home/me/code/proj/src/lib.rs",
            "/home/me/Notes/scopes/a/b/vibedev/projects.json",
            "/home/me/Notes/magician-config.yaml",
        ] {
            assert_eq!(
                at(read_only),
                BwrapAccess::ReadOnly,
                "{read_only}\n{text:?}"
            );
        }
        for hidden in [
            "/home/me/Notes/scopes/a/b/secrets/captured_secrets.vault",
            "/run/user/1000/bus",
        ] {
            assert_eq!(at(hidden), BwrapAccess::Hidden, "{hidden}\n{text:?}");
        }
    }

    /// Read-only roots hold in a coding profile, a writable root inside one
    /// stays writable, a read-only root inside the live fence's storage base
    /// survives the base's carve-out, and the real repo stays denied last.
    #[test]
    fn coding_profile_layers_read_only_roots_and_reopens_nested_writable_ones() {
        let read_only = [
            PathBuf::from("/notes"),
            PathBuf::from("/src/data/scopes/a/b/programs"),
        ];
        let write_allow = [
            PathBuf::from("/work"),
            PathBuf::from("/notes/Workspaces"),
            PathBuf::from("/src/data/scopes"),
        ];
        let profile = macos_sandbox_profile_layered(
            Some((Path::new("/src"), Path::new("/src/data"))),
            Some(Path::new("/code/proj")),
            None,
            &SandboxAllowances::default(),
            Some(&write_allow),
            &read_only,
            &[],
        );
        let at = |rule: &str| {
            profile
                .rfind(rule)
                .unwrap_or_else(|| panic!("missing {rule} in {profile}"))
        };
        let deny_notes = at("(deny file-write* (subpath \"/notes\"))");
        assert!(profile.contains("(deny file-link (subpath \"/notes\"))"));
        assert!(deny_notes > at("(allow file-write* (subpath \"/work\"))"));
        assert!(deny_notes < at("(allow file-write* (subpath \"/notes/Workspaces\"))"));
        assert!(deny_notes < at("(allow file-link (subpath \"/notes/Workspaces\"))"));
        // The layering comes before the live fence, and is emitted again
        // inside the base after the base's re-allow.
        let deny_source = at("(deny file-write* (subpath \"/src\"))");
        assert!(deny_notes < deny_source);
        let programs = at("(deny file-write* (subpath \"/src/data/scopes/a/b/programs\"))");
        assert!(programs > at("(allow file-write* (subpath \"/src/data/scopes\"))"));
        assert!(at("(deny file-write* (subpath \"/code/proj\"))") > programs);

        // Without a write allow-list the legacy writable roots are re-opened.
        let legacy = macos_sandbox_profile_layered(
            None,
            None,
            None,
            &SandboxAllowances::default(),
            None,
            &[PathBuf::from("/notes")],
            &[
                PathBuf::from("/notes/Workspaces"),
                PathBuf::from("/elsewhere"),
            ],
        );
        let deny = legacy
            .rfind("(deny file-write* (subpath \"/notes\"))")
            .unwrap();
        let reopen = legacy
            .rfind("(allow file-write* (subpath \"/notes/Workspaces\"))")
            .unwrap();
        assert!(deny < reopen, "{legacy}");
        assert!(!legacy.contains("/elsewhere"));
        // No read-only roots: no layering at all.
        let plain = macos_sandbox_profile_with(
            None,
            Some(Path::new("/code/proj")),
            None,
            &SandboxAllowances::default(),
            Some(&write_allow),
        );
        assert!(!plain.contains("file-link"), "{plain}");

        // Regression: a write-allowed ancestor of the live source inside a
        // read-only root must not re-open the whole storage base.
        let write_allow = [
            PathBuf::from("/h/dev"),
            PathBuf::from("/h/dev/repo/data/scopes/a"),
        ];
        let profile = macos_sandbox_profile_layered(
            Some((Path::new("/h/dev/repo"), Path::new("/h/dev/repo/data"))),
            None,
            None,
            &SandboxAllowances::default(),
            Some(&write_allow),
            &[PathBuf::from("/h")],
            &[],
        );
        assert!(
            !profile.contains("(allow file-write* (subpath \"/h/dev/repo/data\"))"),
            "{profile}"
        );
        assert!(
            !profile.contains("(allow file-link (subpath \"/h/dev/repo/data\"))"),
            "{profile}"
        );
        assert!(
            profile
                .rfind("(deny file-write* (subpath \"/h/dev/repo\"))")
                .unwrap()
                > profile
                    .rfind("(allow file-write* (subpath \"/h/dev\"))")
                    .unwrap(),
            "{profile}"
        );
    }

    #[test]
    fn coding_bwrap_layers_read_only_roots_and_reopens_nested_writable_ones() {
        let policy = BwrapPolicy {
            write_allow: Some(vec![
                PathBuf::from("/work"),
                PathBuf::from("/notes/Workspaces"),
                PathBuf::from("/src/data/scopes"),
            ]),
            live_fence: Some((PathBuf::from("/src"), PathBuf::from("/src/data"))),
            deny_real_repo: Some(PathBuf::from("/code/proj")),
            read_only: vec![
                PathBuf::from("/notes"),
                PathBuf::from("/src/data/scopes/a/b/programs"),
            ],
            ..BwrapPolicy::default()
        };
        let argv = bwrap_args(&policy, OsStr::new("sh"), &[]);
        let access = |path: &str| bwrap_model(&argv, Path::new(path));
        assert_eq!(access("/notes/plan.md"), BwrapAccess::ReadOnly);
        assert_eq!(
            access("/notes/Workspaces/goal/status.md"),
            BwrapAccess::Write
        );
        assert_eq!(
            access("/src/data/scopes/a/b/programs/plan.md"),
            BwrapAccess::ReadOnly
        );
        assert_eq!(access("/src/data/scopes/a/b/other.md"), BwrapAccess::Write);
        assert_eq!(access("/src/main.rs"), BwrapAccess::ReadOnly);
        assert_eq!(access("/code/proj/x"), BwrapAccess::ReadOnly);
        assert_eq!(access("/work/x"), BwrapAccess::Write);

        let legacy = BwrapPolicy {
            read_only: vec![PathBuf::from("/notes")],
            legacy_writable: vec![PathBuf::from("/notes/Workspaces")],
            ..BwrapPolicy::default()
        };
        let argv = bwrap_args(&legacy, OsStr::new("sh"), &[]);
        let access = |path: &str| bwrap_model(&argv, Path::new(path));
        assert_eq!(access("/notes/plan.md"), BwrapAccess::ReadOnly);
        assert_eq!(access("/notes/Workspaces/a.md"), BwrapAccess::Write);
        assert_eq!(access("/elsewhere/a.md"), BwrapAccess::Write);

        // Regression: a write-allowed ancestor of the live source inside a
        // read-only root must not re-open the whole storage base.
        let nested = BwrapPolicy {
            write_allow: Some(vec![
                PathBuf::from("/h/dev"),
                PathBuf::from("/h/dev/repo/data/scopes/a"),
            ]),
            live_fence: Some((
                PathBuf::from("/h/dev/repo"),
                PathBuf::from("/h/dev/repo/data"),
            )),
            read_only: vec![PathBuf::from("/h")],
            ..BwrapPolicy::default()
        };
        let argv = bwrap_args(&nested, OsStr::new("sh"), &[]);
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !text
                .windows(3)
                .any(|w| w[0].starts_with("--bind") && w[1] == "/h/dev/repo/data"),
            "{text:?}"
        );
        let access = |path: &str| bwrap_model(&argv, Path::new(path));
        assert_eq!(access("/h/dev/repo/data/scopes/b/x"), BwrapAccess::ReadOnly);
        assert_eq!(access("/h/dev/repo/data/scopes/a/x"), BwrapAccess::Write);
        assert_eq!(access("/h/dev/notes.md"), BwrapAccess::Write);
        assert_eq!(access("/h/other.md"), BwrapAccess::ReadOnly);
    }

    /// Live under `sandbox-exec`, through the real profile builder: no hard
    /// link and no write out of a read-only root; both work inside a
    /// writable root re-opened within it.
    #[cfg(target_os = "macos")]
    #[test]
    fn coding_profile_read_only_roots_hold_live() {
        let launcher = Path::new("/usr/bin/sandbox-exec");
        if !launcher.exists() {
            return;
        }
        let trivial_ok = std::process::Command::new(launcher)
            .args([
                "-p",
                "(version 1)(allow default)",
                "/bin/sh",
                "-c",
                "exit 0",
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !trivial_ok {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let notes = root.join("notes");
        let workspaces = notes.join("Workspaces");
        write(&notes.join("plan.md"), "plan");
        write(&workspaces.join("a.md"), "a");
        let profile = macos_sandbox_profile_layered(
            None,
            None,
            None,
            &SandboxAllowances::default(),
            None,
            &[notes.clone()],
            &[workspaces.clone()],
        );
        let runs = |script: String| {
            std::process::Command::new(launcher)
                .args(["-p", &profile, "/bin/sh", "-c", &script])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let quote = |path: &Path| format!("'{}'", path.display());
        assert!(
            !runs(format!(
                "ln {} {}",
                quote(&notes.join("plan.md")),
                quote(&workspaces.join("plan.md"))
            )),
            "a hard link out of a read-only root must be refused"
        );
        assert!(!workspaces.join("plan.md").exists());
        assert!(
            runs(format!(
                "ln {} {}",
                quote(&workspaces.join("a.md")),
                quote(&workspaces.join("b.md"))
            )),
            "a hard link inside the re-opened root must work"
        );
        assert!(
            runs(format!("echo new > {}", quote(&workspaces.join("new.md")))),
            "a write in the re-opened root must work"
        );
        assert!(
            !runs(format!("echo new > {}", quote(&notes.join("new.md")))),
            "a write in the read-only root must be refused"
        );
        assert!(!notes.join("new.md").exists());
    }

    #[test]
    fn bwrap_without_an_allow_list_keeps_the_repo_fences_and_a_working_proc() {
        let policy = BwrapPolicy {
            live_fence: Some((PathBuf::from("/src"), PathBuf::from("/src/data"))),
            deny_real_repo: Some(PathBuf::from("/code/proj")),
            ..BwrapPolicy::default()
        };
        let argv = bwrap_args(&policy, OsStr::new("sh"), &[]);
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        // A fresh procfs needs its own PID namespace unless bwrap is setuid.
        let proc_at = text.iter().position(|arg| arg == "--proc").unwrap();
        let pid_at = text.iter().position(|arg| arg == "--unshare-pid").unwrap();
        assert!(pid_at < proc_at);
        let at = |path: &str| bwrap_model(&argv, Path::new(path));
        assert_eq!(at("/home/me/.bashrc"), BwrapAccess::Write);
        assert_eq!(at("/src/main.rs"), BwrapAccess::ReadOnly);
        assert_eq!(at("/src/data/scopes/x"), BwrapAccess::Write);
        assert_eq!(at("/code/proj/lib.rs"), BwrapAccess::ReadOnly);
    }

    #[test]
    fn the_temp_root_opens_a_parent_only_for_the_macos_per_user_dir() {
        assert_eq!(
            temp_write_root(Path::new("/private/var/folders/ab/cd1234/T")),
            PathBuf::from("/private/var/folders/ab/cd1234")
        );
        for tmp in [
            "/tmp",
            "/Users/me/tmp",
            "/private/var/folders/ab/T",
            "/var/tmp/T",
        ] {
            assert_eq!(temp_write_root(Path::new(tmp)), PathBuf::from(tmp));
        }
    }

    #[test]
    fn project_folders_never_reach_magician_data_or_source() {
        let runtime_root =
            canonicalize_lenient(&crate::magician_v2::process_storage::runtime_root());
        let scope = runtime_root.join("scopes/anon/default");
        let ok = |path: &Path| super::admissible_write_root(path, &scope, None);
        assert!(ok(&scope.join("coding_engine/build/gradle")));
        assert!(ok(Path::new("/opt/caches/gradle")));
        for refused in [
            runtime_root.clone(),
            runtime_root.join("scopes"),
            scope.clone(),
            scope.join("vibedev"),
            scope.join("secrets"),
            runtime_root.parent().unwrap().to_path_buf(),
            PathBuf::from("relative/path"),
            PathBuf::from("/etc"),
        ] {
            assert!(!ok(&refused), "{}", refused.display());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_allowed_ancestor_cannot_reopen_the_live_source_live() {
        let launcher = Path::new("/usr/bin/sandbox-exec");
        if !launcher.exists() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let source = root.join("src");
        let base = source.join("data");
        let shadow = base.join("shadow");
        let other = root.join("other");
        for dir in [&shadow, &other] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let profile = super::macos_sandbox_profile_with(
            Some((&source, &base)),
            None,
            None,
            &super::SandboxAllowances::default(),
            Some(&[root.clone(), shadow.clone()]),
        );
        let try_write = |path: &Path| {
            std::process::Command::new(launcher)
                .args(["-p", &profile, "/bin/sh", "-c"])
                .arg(format!("echo x > '{}'", path.display()))
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        assert!(try_write(&other.join("ok.txt")), "the allowed ancestor");
        assert!(
            try_write(&shadow.join("ok.txt")),
            "a root in the storage base"
        );
        assert!(!try_write(&source.join("no.txt")), "the live source");
        assert!(
            !try_write(&base.join("no.txt")),
            "the base outside listed roots"
        );
    }
}
