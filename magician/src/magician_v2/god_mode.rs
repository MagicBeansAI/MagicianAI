//! God mode (`execution.god_mode`).
//!
//! Where it is on: `execution.god_mode.enabled` is the default, for every
//! chat and for runs no chat started (scheduled, triggers, background). A chat
//! can override it from the composer (the Magician orb): on or off for that
//! chat, its harness turns and every run it launches. VibeDev is unaffected.
//!
//! With god mode on, a trusted agent's `shell`, file and `http` tools may go
//! beyond `execution.shell_sandbox`, `execution.file_sandbox` and
//! `execution.network_policy`, and harness engines get Magician's `shell`
//! through the plane. Where the shell runs in the OS box
//! (`shell_sandbox.os_enforce`), every shell command a grant does not cover
//! asks before it runs and then runs outside the box (a refusal inside the box
//! happens mid-command, too late to ask). Otherwise, and for files and
//! network, only what the policy refuses asks. Asking offers:
//!
//! - **Allow once**: this call only.
//! - **Allow for this session** / **Allow always**, at a scope the owner picks
//!   on the card. Shell: exactly this command, `<program> <first argument>`
//!   with any further arguments, or any use of the program. Files: this file,
//!   its folder, or its top directory. "This session" lasts for the chat
//!   session or agent run; "always" for the principal and workspace until the
//!   god-mode settings change. Changing them (turning god mode off included)
//!   forgets every session and always grant, and every chat's override.
//!
//! Shell lines are read with a real Bash parser ([`shell_scope`]): a grant
//! covers a line only if it covers every command in it. A line whose
//! commands cannot all be named (substitutions are read; `eval`, `bash -c`,
//! `xargs`, a dynamic command name, a parse error are not), that names a
//! command by a path (`./git`), sets a variable (`PATH=. git`, `export`,
//! `unset`, a step's own env, which the caller prefixes to the line), writes
//! output to a file through a redirect, or that matches
//! `shell_sandbox.blocked_command_fragments`, asks every time and offers only
//! "allow once".
//!
//! Who may use it: the call must run under a [`GodSession`], which the run
//! seam, the chat turn and the plane dispatcher set only for trusted agents,
//! and never for a VibeDev coding run ([`current_session`] is `None` inside
//! one). A call with no session is refused with the normal sandbox's error.
//! An approval answered after what it rests on changed (the settings, the
//! chat's choice, super god mode) is refused and not recorded.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{GodModeSettings, SuperGodModeSettings};
use crate::magician_v2::user_requests::service::{RequestOption, UserRequest, UserRequestService};

pub mod shell_scope;
use shell_scope::ShellCommand;

/// Whether a run is a VibeDev coding run, which god mode never applies to.
pub use crate::magician_v2::execution::agentic::types::is_vibedev_coding_context;

/// How long an approval waits for the owner before it counts as "deny".
const APPROVAL_TIMEOUT_SECS: u64 = 300;
/// The request type: a sandbox override.
const REQUEST_TYPE: &str = "sandbox_override";
/// How the prompt renders: a choice, one option per button. The web UI only
/// renders an `authorization`-kind (`sandbox_override`) prompt for the
/// `agentic` and `escalation` sources, and the user-request service publishes
/// as `user_request`, so a `sandbox_override` input type would never show.
const PROMPT_INPUT_TYPE: &str = "choice";
const GRANTS_FILE: &str = "god_mode_grants.json";

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn settings_cell() -> &'static RwLock<GodModeSettings> {
    static CELL: OnceLock<RwLock<GodModeSettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(GodModeSettings::default()))
}

/// Publish `execution.god_mode`. Called at boot, on config reload and by the
/// settings endpoint. A change to the settings forgets every grant.
pub fn configure(settings: &GodModeSettings) {
    // Under the always-store lock: an approval is checked and recorded under
    // it too (`authorize_in`), so it sees either the old settings or the new
    // ones, never a grant recorded after the change forgot the others.
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = {
        let mut guard = settings_cell()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::replace(&mut *guard, settings.clone())
    };
    let fingerprint = fingerprint(settings);
    if previous != *settings {
        session_grants()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        chat_overrides()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
    let loaded = store.get_or_insert_with(|| AlwaysGrants::load(&grants_path()));
    if loaded.fingerprint != fingerprint {
        let forgotten: usize = loaded.grants.values().map(BTreeSet::len).sum();
        *loaded = AlwaysGrants {
            fingerprint,
            grants: HashMap::new(),
        };
        loaded.save(&grants_path());
        if forgotten > 0 {
            tracing::info!(forgotten, "[GOD-MODE] settings changed; always grants forgotten");
        }
    }
}

/// The published settings.
pub fn settings() -> GodModeSettings {
    settings_cell()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The default: on for every chat without an override, and for runs no chat
/// started.
pub fn enabled() -> bool {
    settings().enabled
}

// ---------------------------------------------------------------------------
// Super god mode (debug)
// ---------------------------------------------------------------------------

fn super_cell() -> &'static RwLock<SuperGodModeSettings> {
    static CELL: OnceLock<RwLock<SuperGodModeSettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(SuperGodModeSettings::default()))
}

/// Publish `execution.super_god_mode`. Grants and chat overrides are kept:
/// super god mode sits on top of them and they apply again once it ends.
pub fn configure_super(settings: &SuperGodModeSettings) {
    // Under the always-store lock, as `configure`: an approval answered while
    // this changes is refused, not recorded.
    let _store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::mem::replace(
        &mut *super_cell()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        settings.clone(),
    );
    if previous != *settings {
        tracing::warn!(
            enabled = settings.enabled,
            until_ms = ?settings.until_ms,
            "[SUPER-GOD] super god mode changed"
        );
        // Wake every unrestricted harness's watch: if super god mode is no
        // longer active, each one stops ([`unrestricted_cancel`]).
        super_changes().send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

fn super_changes() -> &'static tokio::sync::watch::Sender<u64> {
    static CHANGES: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// How often a watch re-checks super god mode's end time: an expiry changes no
/// setting, so nothing else wakes it.
const SUPER_EXPIRY_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolves once super god mode is not active: at once when it is turned
/// off, within [`SUPER_EXPIRY_POLL`] when it passes its end.
pub async fn until_super_revoked() {
    let mut changes = super_changes().subscribe();
    while super_active() {
        tokio::select! {
            _ = changes.changed() => {},
            _ = tokio::time::sleep(SUPER_EXPIRY_POLL) => {},
        }
    }
}

/// Keeps an unrestricted harness's watch alive; dropping it stops the watch.
pub struct SuperWatch {
    task: tokio::task::JoinHandle<()>,
    revoked: Arc<std::sync::atomic::AtomicBool>,
}

impl SuperWatch {
    /// Super god mode ended while the harness ran, and its turn was stopped.
    pub fn revoked(&self) -> bool {
        self.revoked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for SuperWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The cancellation for a harness that starts unrestricted: a child of
/// `parent` (the run's or chat's own stop) that is also cancelled when super
/// god mode ends, so turning it off, or its end passing, stops an engine
/// already running with no restrictions. Only this turn stops: the run or
/// chat goes on, and its next harness asks again and starts restricted.
pub fn unrestricted_cancel(
    parent: Option<&tokio_util::sync::CancellationToken>,
    engine: &str,
) -> (tokio_util::sync::CancellationToken, SuperWatch) {
    let token = parent.map(|parent| parent.child_token()).unwrap_or_default();
    let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = {
        let (token, revoked, engine) = (token.clone(), Arc::clone(&revoked), engine.to_string());
        tokio::spawn(async move {
            until_super_revoked().await;
            if !token.is_cancelled() {
                revoked.store(true, std::sync::atomic::Ordering::SeqCst);
                tracing::warn!(%engine, "[SUPER-GOD] super god mode ended; stopping the unrestricted harness");
                token.cancel();
            }
        })
    };
    (token, SuperWatch { task, revoked })
}

pub fn super_settings() -> SuperGodModeSettings {
    super_cell()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Whether `settings` is on at `now_ms`: enabled and not past its end.
pub fn super_on_at(settings: &SuperGodModeSettings, now_ms: i64) -> bool {
    settings.enabled && settings.until_ms.is_none_or(|until| now_ms < until)
}

/// Super god mode is on now: every harness starts with no restrictions —
/// chat and agent-run engines with their native tools, the owner's settings
/// and MCP servers (`plane::engines`), VibeDev engines with no fence
/// (`coding_engine::HarnessFence::Unrestricted`). Magician's own tools are
/// unchanged: god mode still decides whether they ask.
pub fn super_active() -> bool {
    super_on_at(&super_settings(), chrono::Utc::now().timestamp_millis())
}

/// Super god mode: ask (once / this session / always) before `engine` starts
/// with no restrictions, in the chat or run `ctx` serves, whether or not god
/// mode is on there. `false` — super god mode off, an untrusted agent, no one
/// to ask, or the owner said no — means it starts restricted, as normal.
pub async fn approve_unrestricted_launch(
    ctx: &crate::magician_v2::execution::agentic::AgenticContext,
    engine: &str,
    repo: Option<&Path>,
) -> bool {
    if !super_active() || !trust_allows(ctx.trust_level.as_deref()) {
        return false;
    }
    let session = (|| {
        launch_session(
            ctx.principal.as_deref()?,
            ctx.workspace.as_deref()?,
            ctx.execution_id
                .as_deref()
                .or(ctx.legacy_execution_id.as_deref()),
            session_run(ctx).as_deref(),
            ctx.chat_session_id.as_deref(),
            ctx.task_id.as_deref(),
        )
    })();
    approve_unrestricted_in(session, engine, repo).await
}

/// Where an unrestricted launch asks: the run when there is one, so an
/// agent run's "this session" approval is its own and not the chat that
/// launched it; the chat only for a chat turn, which has no run. The card
/// goes to `execution_id`; the session is `session_run`'s (the root run),
/// shared by every run in its delegation tree.
fn launch_session(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    session_run: Option<&str>,
    chat_session_id: Option<&str>,
    task_id: Option<&str>,
) -> Option<GodSession> {
    let execution_id = execution_id.map(str::trim).filter(|id| !id.is_empty());
    let session_run = session_run.map(str::trim).filter(|id| !id.is_empty()).or(execution_id);
    let key = match session_run {
        Some(run) => format!("run:{run}"),
        None => format!(
            "chat:{}",
            chat_session_id.map(str::trim).filter(|id| !id.is_empty())?
        ),
    };
    GodSession::new(
        principal,
        workspace,
        key,
        execution_id.map(str::to_string),
        task_id.map(str::to_string),
    )
}

/// [`approve_unrestricted_launch`] for a caller with only a scope and a run:
/// a VibeDev coding turn. The agent's trust comes from the run seam
/// ([`with_run_trust`]); a call with none scoped is refused.
pub async fn approve_unrestricted_launch_for_run(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    task_id: Option<&str>,
    engine: &str,
    repo: Option<&Path>,
) -> bool {
    let run_trust = current_run_trust();
    let trusted = run_trust
        .as_ref()
        .is_some_and(|trust| trust_allows(trust.level.as_deref()));
    if !super_active() || !trusted {
        return false;
    }
    let session_run = run_trust.and_then(|trust| trust.session_run);
    let session = launch_session(principal, workspace, execution_id, session_run.as_deref(), None, task_id)
        .or_else(current_session);
    approve_unrestricted_in(session, engine, repo).await
}

async fn approve_unrestricted_in(session: Option<GodSession>, engine: &str, repo: Option<&Path>) -> bool {
    let Some(session) = session else {
        tracing::warn!(engine, "[SUPER-GOD] no chat or run to ask in; the harness starts restricted");
        return false;
    };
    match authorize_in(&session, GodAction::UnrestrictedHarness { engine, repo }).await {
        // A grant can cover the launch without asking; super god mode may
        // have ended (`until_ms`) since the caller checked.
        Ok(()) if !super_active() => {
            tracing::info!(engine, "[SUPER-GOD] super god mode ended; the harness starts restricted");
            false
        },
        Ok(()) => {
            tracing::warn!(engine, session = %session.session_key, "[SUPER-GOD] owner approved: harness starts with no restrictions");
            true
        },
        Err(reason) => {
            tracing::info!(engine, %reason, "[SUPER-GOD] harness starts restricted");
            false
        },
    }
}

fn chat_overrides() -> &'static Mutex<HashMap<String, bool>> {
    static OVERRIDES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    OVERRIDES.get_or_init(Mutex::default)
}

/// A chat's own choice, if it made one.
pub fn chat_override(chat_session_id: &str) -> Option<bool> {
    chat_overrides()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(chat_session_id)
        .copied()
}

/// Whether god mode is on for this chat: its override, else the default.
pub fn chat_effective(chat_session_id: &str) -> bool {
    chat_override(chat_session_id).unwrap_or_else(enabled)
}

/// Set (or with `None`, clear) a chat's override. Turning a chat off forgets
/// its "this session" approvals, except super god mode's harness approvals:
/// super god mode does not depend on god mode.
pub fn set_chat_override(chat_session_id: &str, enabled: Option<bool>) {
    let chat_session_id = chat_session_id.trim();
    if chat_session_id.is_empty() {
        return;
    }
    // Under the always-store lock, as `configure`: an approval answered while
    // the chat turns god mode off is refused, not recorded.
    let _store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    {
        let mut overrides = chat_overrides()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match enabled {
            Some(value) => overrides.insert(chat_session_id.to_string(), value),
            None => overrides.remove(chat_session_id),
        };
    }
    if !chat_effective(chat_session_id) {
        let suffix = format!("/chat:{chat_session_id}");
        session_grants()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, patterns| {
                if key.ends_with(&suffix) {
                    patterns.retain(|pattern| pattern.starts_with("harness"));
                }
                !patterns.is_empty()
            });
    }
}

/// What a grant is stamped with: any change to the settings invalidates it.
pub fn fingerprint(settings: &GodModeSettings) -> String {
    serde_json::to_string(settings).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Who is acting and in which session. Set only for trusted agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GodSession {
    pub principal: String,
    pub workspace: String,
    /// `chat:<chat session id>` or `run:<execution id>`.
    pub session_key: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    /// The chat whose choice keeps god mode on for this session, if any; else
    /// the default (`enabled`). Read again when an approval is answered.
    chat: Option<String>,
}

impl GodSession {
    /// A chat's session, when god mode is on for that chat.
    pub fn for_chat(principal: &str, workspace: &str, chat_session_id: &str) -> Option<Self> {
        if !chat_effective(chat_session_id) {
            return None;
        }
        Self::new(principal, workspace, format!("chat:{chat_session_id}"), None, None)
            .map(|session| session.following_chat(Some(chat_session_id)))
    }

    /// A run's session, when god mode is on for it: the launching chat's
    /// choice (`chat_session_id`), else the default.
    pub fn for_run(
        principal: &str,
        workspace: &str,
        execution_id: &str,
        task_id: Option<String>,
        chat_session_id: Option<&str>,
    ) -> Option<Self> {
        let chat = chat_session_id.map(str::trim).filter(|id| !id.is_empty());
        let on = match chat {
            Some(chat) => chat_effective(chat),
            None => enabled(),
        };
        if !on {
            return None;
        }
        Self::new(
            principal,
            workspace,
            format!("run:{execution_id}"),
            Some(execution_id.to_string()),
            task_id,
        )
        .map(|session| session.following_chat(chat))
    }

    fn following_chat(mut self, chat: Option<&str>) -> Self {
        self.chat = chat
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self
    }

    /// Whether god mode is still on for this session: its chat's choice, else
    /// the default.
    fn still_on(&self) -> bool {
        match self.chat.as_deref() {
            Some(chat) => chat_effective(chat),
            None => enabled(),
        }
    }

    fn new(
        principal: &str,
        workspace: &str,
        session_key: String,
        execution_id: Option<String>,
        task_id: Option<String>,
    ) -> Option<Self> {
        let (principal, workspace) = (principal.trim(), workspace.trim());
        if principal.is_empty() || workspace.is_empty() || session_key.ends_with(':') {
            return None;
        }
        Some(Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            session_key,
            execution_id: execution_id.filter(|id| !id.trim().is_empty()),
            task_id: task_id.filter(|id| !id.trim().is_empty()),
            chat: None,
        })
    }

    fn scope_key(&self) -> String {
        format!("{}/{}", self.principal, self.workspace)
    }

    fn session_grant_key(&self) -> String {
        format!("{}/{}", self.scope_key(), self.session_key)
    }
}

/// Whether an agent's trust level may use god mode: built-in and local agents.
/// Untrusted and reviewed agents never get the unrestricted tools.
pub fn trust_allows(trust_level: Option<&str>) -> bool {
    !matches!(
        trust_level.map(|level| level.trim().to_ascii_lowercase()).as_deref(),
        Some("untrusted") | Some("reviewed")
    )
}

/// The trust level of the agent a run serves, as the run seam scoped it
/// ([`with_run_trust`]). `RunTrust::level(None)` is a trusted agent with no level
/// set (built-in); no `RunTrust` at all means nothing scoped one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTrust {
    /// The agent's trust level; `None` is a trusted built-in agent.
    pub level: Option<String>,
    /// The run whose approval session this run shares: the root of its
    /// delegation tree ([`session_run`]), so "this session" covers a whole
    /// build or agent run, the runs it delegates to included.
    pub session_run: Option<String>,
}

impl RunTrust {
    /// The trust of `ctx`'s agent, sharing its root run's approval session.
    pub fn of(ctx: &crate::magician_v2::execution::agentic::AgenticContext) -> Self {
        Self { level: ctx.trust_level.clone(), session_run: session_run(ctx) }
    }

    #[cfg(test)]
    pub fn level(level: Option<&str>) -> Self {
        Self { level: level.map(str::to_string), session_run: None }
    }
}

/// The run a run's approvals are kept under: the root of its delegation tree
/// (`root_execution_id`), else the run itself. A delegated engineer shares
/// its coordinator's "this session" answers instead of asking again.
pub fn session_run(ctx: &crate::magician_v2::execution::agentic::AgenticContext) -> Option<String> {
    [ctx.root_execution_id.as_deref(), ctx.execution_id.as_deref(), ctx.legacy_execution_id.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|id| !id.is_empty())
        .map(str::to_string)
}

/// The god-mode session and the run's trust, in ONE task-local. Each field is
/// `None` when no scope set it (so an outer value shows through) and
/// `Some(value)` when one did, `Some(None)` clearing it. One layer, not two:
/// every task-local layer adds its poll frames to the run's stack, and a
/// second layer here overflowed native runs on a 2 MiB worker (2026-10-15).
#[derive(Debug, Clone, Default)]
struct GodScope {
    session: Option<Option<GodSession>>,
    trust: Option<Option<RunTrust>>,
}

tokio::task_local! {
    static GOD_SCOPE: GodScope;
}

fn current_scope() -> GodScope {
    GOD_SCOPE.try_with(Clone::clone).unwrap_or_default()
}

/// Run `future` with `trust` as the run's trust level. `None` clears an
/// outer one: a caller that reads it then refuses ([`current_run_trust`]).
pub async fn with_run_trust<F>(trust: Option<RunTrust>, future: F) -> F::Output
where
    F: std::future::Future,
{
    let scope = GodScope { trust: Some(trust), ..current_scope() };
    GOD_SCOPE.scope(scope, future).await
}

/// The run's trust level, if the run seam scoped one. A caller that needs
/// trust and finds none must fail closed.
pub fn current_run_trust() -> Option<RunTrust> {
    current_scope().trust.flatten()
}

/// Run `future` with `session` as the god-mode session of every shell and
/// file call inside it. `None` clears an outer session.
pub async fn with_session<F>(session: Option<GodSession>, future: F) -> F::Output
where
    F: std::future::Future,
{
    let scope = GodScope { session: Some(session), ..current_scope() };
    GOD_SCOPE.scope(scope, future).await
}

/// [`with_session`] and [`with_run_trust`] in one layer: what a run seam
/// sets.
pub async fn with_session_and_trust<F>(
    session: Option<GodSession>,
    trust: Option<RunTrust>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    GOD_SCOPE.scope(GodScope { session: Some(session), trust: Some(trust) }, future).await
}

/// God mode is on for the current call: it runs in a trusted chat or run with
/// god mode on.
pub fn active() -> bool {
    current_session().is_some()
}

/// The session set by [`with_session`]; an explicit `None` (an untrusted run)
/// stays `None`. With no session set at all, a chat compiled-dispatch call
/// supplies one from its invocation context: chat runs compiled tools in
/// process only for built-in and local agents (lower trust takes the
/// task-backed executor path), so that call is trusted.
///
/// Never inside a VibeDev coding run (`coding_engine::coding_context_active`):
/// god mode does not apply to VibeDev, whatever an outer scope set.
pub fn current_session() -> Option<GodSession> {
    if crate::magician_v2::execution::coding_engine::coding_context_active() {
        return None;
    }
    match current_scope().session {
        Some(explicit) => explicit,
        None => chat_compiled_session(),
    }
}

fn chat_compiled_session() -> Option<GodSession> {
    let invocation =
        crate::magician_v2::execution::compiled_dispatch::current_compiled_invocation_context()?;
    GodSession::for_chat(
        &invocation.principal,
        &invocation.workspace,
        invocation.chat_session_id.as_deref()?,
    )
}

// ---------------------------------------------------------------------------
// What a call needs, and the scopes a grant may cover
// ---------------------------------------------------------------------------

/// One thing a call does that a grant must cover.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Need {
    /// A command the shell line runs (read by [`shell_scope::read`]).
    Command(ShellCommand),
    /// A path a file action touches, absolute and normalized.
    Path(PathBuf),
    /// A network destination the `http` tool may not reach under the policy.
    Host(String),
    /// A harness started with no restrictions (super god mode): the engine,
    /// and for a VibeDev coding turn the repo.
    Harness { engine: String, repo: Option<PathBuf> },
}

/// How far a lasting grant reaches. The owner picks it on the approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Shell: exactly this command, every argument as written.
    Exact,
    /// Shell: `<program> <first argument>` with any further arguments.
    Prefix,
    /// Shell: any use of the program.
    Program,
    /// Files: this file.
    File,
    /// Files: anything under the file's folder.
    Folder,
    /// Files: anything under its top directory (`~/projects`, `/opt`).
    Top,
    /// Network: this host.
    Host,
    /// Network: any host.
    AnyHost,
    /// Super god mode: this engine on this repo (VibeDev).
    EngineRepo,
    /// Super god mode: this engine.
    Engine,
    /// Super god mode: any harness.
    AnyHarness,
}

impl Scope {
    const SHELL: [Scope; 3] = [Scope::Exact, Scope::Prefix, Scope::Program];
    const FILES: [Scope; 3] = [Scope::File, Scope::Folder, Scope::Top];
    const NETWORK: [Scope; 2] = [Scope::Host, Scope::AnyHost];
    const HARNESS: [Scope; 3] = [Scope::EngineRepo, Scope::Engine, Scope::AnyHarness];

    fn id(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Program => "program",
            Self::File => "file",
            Self::Folder => "folder",
            Self::Top => "top",
            Self::Host => "host",
            Self::AnyHost => "any_host",
            Self::EngineRepo => "engine_repo",
            Self::Engine => "engine",
            Self::AnyHarness => "any_harness",
        }
    }
}

/// The grant pattern that covers `need` at `scope`, if that scope applies:
/// `shell=<command>`, `shell^<program> <first argument>`, `shell*<program>`,
/// `file=<path>`, `dir=<directory>`, `net=<host>`, `net*`,
/// `harness=<engine>@<repo>`, `harness=<engine>`, `harness*`.
fn pattern(need: &Need, scope: Scope) -> Option<String> {
    match (need, scope) {
        (Need::Command(command), Scope::Exact) => command.exact().map(|text| format!("shell={text}")),
        (Need::Command(command), Scope::Prefix) => command.prefix().map(|text| format!("shell^{text}")),
        (Need::Command(command), Scope::Program) => Some(program_grant(&command.program)),
        (Need::Path(path), Scope::File) => Some(format!("file={}", path.display())),
        (Need::Path(path), Scope::Folder) => path.parent().map(folder_grant),
        (Need::Path(path), Scope::Top) => Some(folder_grant(&top_directory(path))),
        (Need::Host(host), Scope::Host) => Some(format!("net={host}")),
        (Need::Host(_), Scope::AnyHost) => Some("net*".to_string()),
        (Need::Harness { engine, repo: Some(repo) }, Scope::EngineRepo) => {
            Some(format!("harness={engine}@{}", repo.display()))
        },
        (Need::Harness { engine, .. }, Scope::Engine) => Some(format!("harness={engine}")),
        (Need::Harness { .. }, Scope::AnyHarness) => Some("harness*".to_string()),
        _ => None,
    }
}

/// The pattern that grants any use of `program`.
pub fn program_grant(program: &str) -> String {
    format!("shell*{program}")
}

/// The pattern that grants anything under `dir`.
pub fn folder_grant(dir: &Path) -> String {
    format!("dir={}", normalize(&absolute(dir)).display())
}

fn covered(grants: &HashSet<String>, need: &Need) -> bool {
    match need {
        Need::Command(command) => {
            grants.contains(&program_grant(&command.program))
                || command
                    .prefix()
                    .is_some_and(|text| grants.contains(&format!("shell^{text}")))
                || command
                    .exact()
                    .is_some_and(|text| grants.contains(&format!("shell={text}")))
        },
        Need::Path(path) => {
            grants.contains(&format!("file={}", path.display()))
                || grants
                    .iter()
                    .filter_map(|grant| grant.strip_prefix("dir="))
                    .any(|dir| path.starts_with(dir))
        },
        Need::Host(host) => grants.contains("net*") || grants.contains(&format!("net={host}")),
        Need::Harness { engine, repo } => {
            grants.contains("harness*")
                || grants.contains(&format!("harness={engine}"))
                || repo
                    .as_ref()
                    .is_some_and(|repo| grants.contains(&format!("harness={engine}@{}", repo.display())))
        },
    }
}

/// How a pattern reads on the approval card.
fn describe(pattern: &str) -> String {
    if let Some(text) = pattern.strip_prefix("shell=") {
        format!("`{text}`")
    } else if let Some(text) = pattern.strip_prefix("shell^") {
        format!("`{text} …`")
    } else if let Some(program) = pattern.strip_prefix("shell*") {
        format!("any `{program}`")
    } else if let Some(path) = pattern.strip_prefix("file=") {
        format!("`{path}`")
    } else if let Some(dir) = pattern.strip_prefix("dir=") {
        format!("anything under `{dir}`")
    } else if let Some(host) = pattern.strip_prefix("net=") {
        format!("`{host}`")
    } else if pattern == "net*" {
        "any host".to_string()
    } else if let Some(rest) = pattern.strip_prefix("harness=") {
        match rest.split_once('@') {
            Some((engine, repo)) => format!("{engine} on `{repo}`"),
            None => format!("{rest}, anywhere"),
        }
    } else if pattern == "harness*" {
        "any harness".to_string()
    } else {
        format!("`{pattern}`")
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

/// The first directory under the home directory (`~/projects`), else the
/// first directory under `/` (`/opt`).
fn top_directory(path: &Path) -> PathBuf {
    let path = normalize(&absolute(path));
    if let Some(home) = dirs::home_dir().map(|home| normalize(&home)) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return match first_component(rest) {
                Some(top) => home.join(top),
                None => home,
            };
        }
    }
    match path.strip_prefix("/").ok().and_then(first_component) {
        Some(top) => PathBuf::from("/").join(top),
        None => PathBuf::from("/"),
    }
}

fn first_component(path: &Path) -> Option<String> {
    path.components().find_map(|component| match component {
        Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        _ => None,
    })
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            },
            Component::CurDir => {},
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

fn session_grants() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static GRANTS: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
    GRANTS.get_or_init(Mutex::default)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AlwaysGrants {
    fingerprint: String,
    /// `principal/workspace` → grant patterns.
    grants: HashMap<String, BTreeSet<String>>,
}

impl AlwaysGrants {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Err(error) =
            crate::magician_v2::runtime_settings::write_text_file_atomic(path, &text, Some(0o600))
        {
            tracing::warn!(%error, path = %path.display(), "[GOD-MODE] could not save always grants");
        }
    }
}

fn always_store() -> &'static Mutex<Option<AlwaysGrants>> {
    static STORE: OnceLock<Mutex<Option<AlwaysGrants>>> = OnceLock::new();
    STORE.get_or_init(Mutex::default)
}

fn grants_path() -> PathBuf {
    // Tests never touch the runtime root's grants.
    #[cfg(test)]
    return test_grants_path().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("magician-god-mode-test-{}.json", std::process::id()))
    });
    #[cfg(not(test))]
    crate::magician_v2::runtime_settings::runtime_settings_paths()
        .runtime_root
        .join(GRANTS_FILE)
}

#[cfg(test)]
fn test_grants_path() -> Option<PathBuf> {
    TEST_GRANTS_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
static TEST_GRANTS_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Every pattern granted to `session`: its own and its scope's always grants.
fn grants_for(session: &GodSession) -> HashSet<String> {
    let mut grants = session_grants()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.session_grant_key())
        .cloned()
        .unwrap_or_default();
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(always) = store
        .get_or_insert_with(|| AlwaysGrants::load(&grants_path()))
        .grants
        .get(&session.scope_key())
    {
        grants.extend(always.iter().cloned());
    }
    grants
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lasting {
    Session,
    Always,
}

#[cfg(test)]
fn record(session: &GodSession, patterns: &[String], lasting: Lasting) {
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    record_locked(&mut store, session, patterns, lasting);
}

/// Record a grant while holding the always-store lock (`store`).
fn record_locked(
    store: &mut Option<AlwaysGrants>,
    session: &GodSession,
    patterns: &[String],
    lasting: Lasting,
) {
    match lasting {
        Lasting::Session => {
            session_grants()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(session.session_grant_key())
                .or_default()
                .extend(patterns.iter().cloned());
        },
        Lasting::Always => {
            let path = grants_path();
            let grants = store.get_or_insert_with(|| AlwaysGrants::load(&path));
            grants.fingerprint = fingerprint(&settings());
            grants
                .grants
                .entry(session.scope_key())
                .or_default()
                .extend(patterns.iter().cloned());
            grants.save(&path);
        },
    }
}

/// Forget every grant: session and always. Used by the settings endpoint's
/// explicit reset.
pub fn forget_all_grants() {
    session_grants()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grants = AlwaysGrants {
        fingerprint: fingerprint(&settings()),
        grants: HashMap::new(),
    };
    grants.save(&grants_path());
    *store = Some(grants);
}

/// Number of always grants, for the settings page.
pub fn always_grant_count() -> usize {
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store
        .get_or_insert_with(|| AlwaysGrants::load(&grants_path()))
        .grants
        .values()
        .map(BTreeSet::len)
        .sum()
}

// ---------------------------------------------------------------------------
// Approval
// ---------------------------------------------------------------------------

fn approver_cell() -> &'static OnceLock<Arc<UserRequestService>> {
    static APPROVER: OnceLock<Arc<UserRequestService>> = OnceLock::new();
    &APPROVER
}

/// The process's user-request service, which owns the approval prompts.
/// Installed once at boot.
pub fn install_approver(service: Arc<UserRequestService>) {
    let _ = approver_cell().set(service);
}

/// What is being authorized.
#[derive(Debug, Clone)]
pub enum GodAction<'a> {
    Shell {
        command: &'a str,
        /// The normal sandbox's refusal.
        violation: &'a str,
        /// Matched a blocked command fragment: asks every time, "once" only.
        catastrophic: bool,
    },
    Path {
        path: &'a Path,
        write: bool,
        violation: &'a str,
    },
    /// The `http` tool, to a destination the network policy refuses.
    Network {
        host: &'a str,
        port: u16,
        violation: &'a str,
    },
    /// Super god mode: start a harness with no restrictions (its native
    /// tools, the owner's settings and MCP servers, no sandbox). `repo` for a
    /// VibeDev coding turn.
    UnrestrictedHarness {
        engine: &'a str,
        repo: Option<&'a Path>,
    },
}

/// What a call needs, and why it can only be allowed once, if it can.
struct Assessment {
    needs: Vec<Need>,
    once_only: Option<String>,
    scopes: &'static [Scope],
}

fn assess(action: &GodAction<'_>) -> Assessment {
    match action {
        GodAction::Shell {
            command,
            catastrophic,
            ..
        } => {
            let reading = shell_scope::read(command);
            Assessment {
                needs: reading.commands.into_iter().map(Need::Command).collect(),
                once_only: if *catastrophic {
                    Some("it matches the blocked-command list".to_string())
                } else {
                    reading.opaque
                },
                scopes: &Scope::SHELL,
            }
        },
        GodAction::Path { path, .. } => Assessment {
            needs: vec![Need::Path(normalize(&absolute(path)))],
            once_only: None,
            scopes: &Scope::FILES,
        },
        GodAction::Network { host, .. } => Assessment {
            needs: vec![Need::Host(host.trim().to_ascii_lowercase())],
            once_only: None,
            scopes: &Scope::NETWORK,
        },
        GodAction::UnrestrictedHarness { engine, repo } => Assessment {
            needs: vec![Need::Harness {
                engine: engine.trim().to_ascii_lowercase(),
                repo: repo.map(|repo| normalize(&absolute(repo))),
            }],
            once_only: None,
            scopes: &Scope::HARNESS,
        },
    }
}

/// The lasting choices for `uncovered`: per scope that applies to every one
/// of them, its patterns. A scope whose patterns repeat a narrower one's is
/// left out.
fn lasting_choices(uncovered: &[&Need], scopes: &[Scope]) -> Vec<(Scope, Vec<String>)> {
    let mut choices: Vec<(Scope, Vec<String>)> = Vec::new();
    for &scope in scopes {
        let Some(patterns) = uncovered
            .iter()
            .map(|need| pattern(need, scope))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let covers_same = choices.iter().any(|(_, earlier)| {
            earlier.iter().map(|p| describe(p)).collect::<Vec<_>>()
                == patterns.iter().map(|p| describe(p)).collect::<Vec<_>>()
        });
        if !patterns.is_empty() && !covers_same {
            choices.push((scope, patterns));
        }
    }
    choices
}

/// Decide whether god mode lets `action` go beyond the normal sandbox.
/// `Ok(())` means run it unrestricted; `Err` is the reason to refuse.
pub async fn authorize(action: GodAction<'_>) -> Result<(), String> {
    let Some(session) = current_session() else {
        return Err("god mode is off for this chat or run".to_string());
    };
    authorize_in(&session, action).await
}

async fn authorize_in(session: &GodSession, action: GodAction<'_>) -> Result<(), String> {
    let session = session.clone();
    let assessment = assess(&action);
    let grants = grants_for(&session);
    let uncovered: Vec<&Need> = assessment
        .needs
        .iter()
        .filter(|need| !covered(&grants, need))
        .collect();
    if assessment.once_only.is_none() && !assessment.needs.is_empty() && uncovered.is_empty() {
        return Ok(());
    }
    let Some(service) = approver_cell().get().cloned() else {
        return Err("god mode is on, but no approval channel is available".to_string());
    };
    let choices = if assessment.once_only.is_some() {
        Vec::new()
    } else {
        lasting_choices(&uncovered, assessment.scopes)
    };
    let mut options = vec![option("allow_once", "Allow once".to_string())];
    for (scope, patterns) in &choices {
        let what = patterns.iter().map(|p| describe(p)).collect::<Vec<_>>().join(", ");
        options.push(option(
            &format!("session:{}", scope.id()),
            format!("This session: {what}"),
        ));
        options.push(option(&format!("always:{}", scope.id()), format!("Always: {what}")));
    }
    options.push(option("deny", "Deny".to_string()));
    // What the approval rests on, as the owner is asked; checked again when
    // they answer (`approval_still_holds`).
    let god_before = fingerprint(&settings());
    let super_before = super_settings();
    let (question, context) = prompt(&action, assessment.once_only.as_deref(), &session);
    let response = service
        .ask(UserRequest {
            id: String::new(),
            request_type: REQUEST_TYPE.to_string(),
            question,
            options,
            principal: session.principal.clone(),
            workspace: session.workspace.clone(),
            context,
            source: "god_mode".to_string(),
            execution_id: session.execution_id.clone(),
            task_id: session.task_id.clone(),
            timeout_secs: APPROVAL_TIMEOUT_SECS,
            default_on_timeout: "deny".to_string(),
            created_at: 0,
            sensitive: None,
        })
        .await;
    tracing::info!(
        session = %session.session_key,
        decision = %response.decision,
        "[GOD-MODE] owner answered"
    );
    let decision = response.decision.as_str();
    let chosen = if decision == "allow_once" {
        None
    } else {
        let chosen = decision.split_once(':').and_then(|(lasting, scope)| {
            let lasting = match lasting {
                "session" => Lasting::Session,
                "always" => Lasting::Always,
                _ => return None,
            };
            choices
                .iter()
                .find(|(offered, _)| offered.id() == scope)
                .map(|(_, patterns)| (lasting, patterns))
        });
        match chosen {
            Some(chosen) => Some(chosen),
            // Deny, timeout, or anything not offered.
            None => return Err("the owner denied it".to_string()),
        }
    };
    // Check and record under the always-store lock: `configure`,
    // `configure_super` and `set_chat_override` change what an approval
    // rests on under the same lock, so none lands between the two.
    let mut store = always_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    approval_still_holds(&action, &session, &god_before, &super_before)?;
    if let Some((lasting, patterns)) = chosen {
        record_locked(&mut store, &session, patterns, lasting);
    }
    Ok(())
}

/// Whether an approval the owner just gave still holds: nothing it rests on
/// changed while the card was open. A settings change, super god mode turned
/// off, changed or past its end (for a harness launch), or god mode turned
/// off for the chat or run (for a shell, file or network call) refuses it.
fn approval_still_holds(
    action: &GodAction<'_>,
    session: &GodSession,
    god_before: &str,
    super_before: &SuperGodModeSettings,
) -> Result<(), String> {
    if fingerprint(&settings()) != god_before {
        return Err("the god-mode settings changed while the owner was asked".to_string());
    }
    match action {
        GodAction::UnrestrictedHarness { .. } => {
            if super_settings() != *super_before || !super_active() {
                return Err("super god mode changed or ended while the owner was asked".to_string());
            }
        },
        GodAction::Shell { .. } | GodAction::Path { .. } | GodAction::Network { .. } => {
            if !session.still_on() {
                return Err(
                    "god mode was turned off for this chat or run while the owner was asked".to_string()
                );
            }
        },
    }
    Ok(())
}

fn option(id: &str, label: String) -> RequestOption {
    RequestOption {
        id: id.to_string(),
        label,
        requires_input: false,
    }
}

fn prompt(
    action: &GodAction<'_>,
    once_only: Option<&str>,
    session: &GodSession,
) -> (String, serde_json::Value) {
    let once = once_only
        .map(|reason| format!(" It can only be allowed once: {reason}."))
        .unwrap_or_default();
    match action {
        GodAction::Shell {
            command, violation, ..
        } => (
            format!("God mode: run `{command}` outside the sandbox? ({violation}){once}"),
            json!({
                "input_type": PROMPT_INPUT_TYPE,
                "god_mode": true,
                "hint": "God mode is on. The agent wants to run a command outside the sandbox.",
                "command": command,
                "violation": violation,
                "once_only": once_only,
                "session": session.session_key,
            }),
        ),
        GodAction::Path {
            path,
            write,
            violation,
        } => (
            format!(
                "God mode: {} `{}`? The normal sandbox refuses it ({violation}).",
                if *write { "write to" } else { "read" },
                path.display()
            ),
            json!({
                "input_type": PROMPT_INPUT_TYPE,
                "god_mode": true,
                "hint": "God mode is on. The agent wants to use a path outside the normal sandbox.",
                "command": path.display().to_string(),
                "violation": violation,
                "session": session.session_key,
            }),
        ),
        GodAction::Network {
            host,
            port,
            violation,
        } => (
            format!("God mode: let the http tool reach `{host}:{port}`? ({violation})"),
            json!({
                "input_type": PROMPT_INPUT_TYPE,
                "god_mode": true,
                "hint": "God mode is on. The agent wants to reach a host the network policy blocks.",
                "command": format!("{host}:{port}"),
                "violation": violation,
                "session": session.session_key,
            }),
        ),
        GodAction::UnrestrictedHarness { engine, repo } => {
            let target = repo
                .map(|repo| format!(" on `{}`", repo.display()))
                .unwrap_or_default();
            (
                format!(
                    "Super god mode: start {engine}{target} with no restrictions? It gets its native \
                     tools, your settings and MCP servers, and no sandbox. Deny to start it restricted, \
                     as normal."
                ),
                json!({
                    "input_type": PROMPT_INPUT_TYPE,
                    "god_mode": true,
                    "super_god_mode": true,
                    "hint": "Super god mode is on. A harness is about to start with no restrictions.",
                    "command": format!("{engine}{target}"),
                    "violation": "harness restrictions",
                    "session": session.session_key,
                }),
            )
        },
    }
}

/// Serializes tests that publish the process-wide settings or grants.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Tests elsewhere: god mode published as `enabled`, grants in a temp file,
/// everything reset when dropped.
#[cfg(test)]
pub(crate) struct TestScope {
    _lock: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
}

#[cfg(test)]
pub(crate) fn test_scope(enabled: bool) -> TestScope {
    let lock = test_lock();
    let dir = tempfile::tempdir().expect("temp dir");
    *TEST_GRANTS_PATH.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir.path().join(GRANTS_FILE));
    *always_store().lock().unwrap_or_else(|p| p.into_inner()) = None;
    configure(&GodModeSettings { enabled });
    TestScope { _lock: lock, _dir: dir }
}

#[cfg(test)]
impl Drop for TestScope {
    fn drop(&mut self) {
        configure(&GodModeSettings::default());
        configure_super(&SuperGodModeSettings::default());
        *always_store().lock().unwrap_or_else(|p| p.into_inner()) = None;
        *TEST_GRANTS_PATH.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Tests elsewhere: grant `patterns` for `session` as "this session".
#[cfg(test)]
pub(crate) fn grant_for_session_in_test(session: &GodSession, patterns: &[String]) {
    record(session, patterns, Lasting::Session);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(key: &str) -> GodSession {
        GodSession::for_chat("owner", "default", key).unwrap()
    }

    fn with_temp_store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        *TEST_GRANTS_PATH.lock().unwrap() = Some(dir.path().join(GRANTS_FILE));
        *always_store().lock().unwrap() = None;
        dir
    }

    fn shell_needs(command: &str) -> Vec<Need> {
        shell_scope::read(command).commands.into_iter().map(Need::Command).collect()
    }

    fn grants(patterns: &[&str]) -> HashSet<String> {
        patterns.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn the_card_offers_exact_prefix_and_program_scopes() {
        let needs = shell_needs("cd repo && npm install --save foo");
        let uncovered: Vec<&Need> = needs.iter().collect();
        let choices = lasting_choices(&uncovered, &Scope::SHELL);
        let offered: Vec<_> = choices
            .iter()
            .map(|(scope, patterns)| (scope.id(), patterns.clone()))
            .collect();
        assert_eq!(
            offered,
            vec![
                ("exact", vec!["shell=cd repo".to_string(), "shell=npm install --save foo".to_string()]),
                ("prefix", vec!["shell^cd repo".to_string(), "shell^npm install".to_string()]),
                ("program", vec!["shell*cd".to_string(), "shell*npm".to_string()]),
            ]
        );
        // `ls` alone: no first argument, so no prefix scope; "exactly `ls`"
        // and "any `ls`" differ (the second covers `ls -la`).
        let ls = shell_needs("ls");
        let only: Vec<&Need> = ls.iter().collect();
        let scopes: Vec<_> = lasting_choices(&only, &Scope::SHELL)
            .into_iter()
            .map(|(scope, _)| scope.id())
            .collect();
        assert_eq!(scopes, vec!["exact", "program"]);
    }

    #[test]
    fn a_grant_covers_exactly_its_scope() {
        let install = &shell_needs("npm install bar")[0];
        let run = &shell_needs("npm run build")[0];
        assert!(covered(&grants(&["shell^npm install"]), install));
        assert!(!covered(&grants(&["shell^npm install"]), run));
        assert!(covered(&grants(&["shell*npm"]), run));
        assert!(!covered(&grants(&["shell=npm install foo"]), install));
        assert!(covered(&grants(&["shell=npm install bar"]), install));

        let file = Need::Path(PathBuf::from("/opt/tools/bin/x"));
        assert!(covered(&grants(&["dir=/opt"]), &file));
        assert!(covered(&grants(&["dir=/opt/tools"]), &file));
        assert!(!covered(&grants(&["dir=/opt/tool"]), &file), "path components, not string prefixes");
        assert!(covered(&grants(&["file=/opt/tools/bin/x"]), &file));
    }

    #[test]
    fn file_scopes_are_the_file_its_folder_and_its_top_directory() {
        let home = dirs::home_dir().unwrap();
        let file = Need::Path(home.join("projects/app/src/main.rs"));
        let uncovered = vec![&file];
        let offered: Vec<_> = lasting_choices(&uncovered, &Scope::FILES)
            .into_iter()
            .map(|(scope, patterns)| (scope.id(), patterns))
            .collect();
        assert_eq!(offered[0].0, "file");
        assert_eq!(offered[1], ("folder", vec![format!("dir={}", home.join("projects/app/src").display())]));
        assert_eq!(offered[2], ("top", vec![format!("dir={}", home.join("projects").display())]));
    }

    #[test]
    fn a_network_grant_covers_its_host_or_any_host() {
        let host = Need::Host("localhost".to_string());
        let uncovered = vec![&host];
        let offered: Vec<_> = lasting_choices(&uncovered, &Scope::NETWORK)
            .into_iter()
            .map(|(scope, patterns)| (scope.id(), patterns))
            .collect();
        assert_eq!(
            offered,
            vec![("host", vec!["net=localhost".to_string()]), ("any_host", vec!["net*".to_string()])]
        );
        assert!(covered(&grants(&["net=localhost"]), &host));
        assert!(!covered(&grants(&["net=example.com"]), &host));
        assert!(covered(&grants(&["net*"]), &host));
    }

    #[test]
    fn super_god_mode_ends_on_its_own_or_lasts_until_turned_off() {
        let hour = SuperGodModeSettings { enabled: true, until_ms: Some(1_000 + 3_600_000) };
        assert!(super_on_at(&hour, 1_000));
        assert!(!super_on_at(&hour, 1_000 + 3_600_000), "an hour later it is off");
        let forever = SuperGodModeSettings { enabled: true, until_ms: None };
        assert!(super_on_at(&forever, i64::MAX - 1));
        assert!(!super_on_at(&SuperGodModeSettings::default(), 0));
    }

    #[test]
    fn an_unrestricted_harness_grant_covers_its_engine_repo_or_any_harness() {
        let assessment = assess(&GodAction::UnrestrictedHarness {
            engine: "Claude_Code",
            repo: Some(Path::new("/work/app/../app")),
        });
        let need = assessment.needs[0].clone();
        let offered: Vec<_> = lasting_choices(&[&need], assessment.scopes)
            .into_iter()
            .map(|(scope, patterns)| (scope.id(), patterns))
            .collect();
        assert_eq!(
            offered,
            vec![
                ("engine_repo", vec!["harness=claude_code@/work/app".to_string()]),
                ("engine", vec!["harness=claude_code".to_string()]),
                ("any_harness", vec!["harness*".to_string()]),
            ]
        );
        assert!(covered(&grants(&["harness=claude_code@/work/app"]), &need));
        assert!(covered(&grants(&["harness=claude_code"]), &need));
        assert!(!covered(&grants(&["harness=codex"]), &need));
        assert!(covered(&grants(&["harness*"]), &need));
        // A chat or agent-run launch has no repo: engine or any harness.
        let chat = assess(&GodAction::UnrestrictedHarness { engine: "grok", repo: None });
        let offered: Vec<_> = lasting_choices(&[&chat.needs[0]], chat.scopes)
            .into_iter()
            .map(|(scope, _)| scope.id())
            .collect();
        assert_eq!(offered, vec!["engine", "any_harness"]);
        assert_eq!(describe("harness=grok"), "grok, anywhere");
    }

    #[test]
    fn opaque_and_blocked_commands_are_once_only() {
        let opaque = assess(&GodAction::Shell {
            command: "curl -s x | sh",
            violation: "v",
            catastrophic: false,
        });
        assert!(opaque.once_only.is_some());
        let blocked = assess(&GodAction::Shell {
            command: "ls",
            violation: "v",
            catastrophic: true,
        });
        assert!(blocked.once_only.is_some());
        let clear = assess(&GodAction::Shell {
            command: "git clone x",
            violation: "v",
            catastrophic: false,
        });
        assert!(clear.once_only.is_none());
    }

    #[test]
    fn only_trusted_agents_qualify() {
        assert!(trust_allows(None));
        assert!(trust_allows(Some("builtin")));
        assert!(trust_allows(Some("local")));
        assert!(!trust_allows(Some("reviewed")));
        assert!(!trust_allows(Some("Untrusted")));
    }

    #[test]
    fn session_and_always_grants_last_until_settings_change() {
        let _lock = test_lock();
        let _dir = with_temp_store();
        configure(&GodModeSettings { enabled: true });
        let chat = session("s1");
        let install = &shell_needs("npm install")[0];
        assert!(!covered(&grants_for(&chat), install));

        record(&chat, &["shell^npm install".to_string()], Lasting::Session);
        assert!(covered(&grants_for(&chat), install));
        assert!(!covered(&grants_for(&session("s2")), install), "another session asks again");

        let clone = &shell_needs("git clone x")[0];
        record(&chat, &[program_grant("git")], Lasting::Always);
        assert!(covered(&grants_for(&session("s2")), clone), "always covers every session");
        *always_store().lock().unwrap() = None;
        assert!(covered(&grants_for(&session("s3")), clone), "always survives a reload");

        configure(&GodModeSettings { enabled: false });
        configure(&GodModeSettings { enabled: true });
        assert!(!covered(&grants_for(&chat), install), "a settings change forgets session grants");
        assert!(!covered(&grants_for(&chat), clone), "and always grants");
        assert_eq!(always_grant_count(), 0);
    }

    #[tokio::test]
    async fn without_a_session_god_mode_refuses() {
        let _lock = test_lock();
        let _dir = with_temp_store();
        configure(&GodModeSettings { enabled: true });
        let refused = authorize(GodAction::Shell {
            command: "ls",
            violation: "outside",
            catastrophic: false,
        })
        .await
        .unwrap_err();
        assert!(refused.contains("off for this chat or run"), "{refused}");
        configure(&GodModeSettings { enabled: false });
    }

    /// The default applies to chats without an override and to runs no chat
    /// started; a chat's override wins for the chat and the runs it launched;
    /// a settings change forgets every override.
    #[test]
    fn a_chat_override_beats_the_default_for_the_chat_and_its_runs() {
        let _lock = test_lock();
        let _dir = with_temp_store();
        configure(&GodModeSettings { enabled: false });
        assert!(GodSession::for_chat("o", "w", "c1").is_none(), "default off");
        assert!(GodSession::for_run("o", "w", "e1", None, None).is_none());

        set_chat_override("c1", Some(true));
        assert!(GodSession::for_chat("o", "w", "c1").is_some(), "the chat turned it on");
        assert!(GodSession::for_run("o", "w", "e1", None, Some("c1")).is_some(), "its runs too");
        assert!(GodSession::for_chat("o", "w", "c2").is_none(), "other chats follow the default");
        assert!(GodSession::for_run("o", "w", "e2", None, None).is_none());

        configure(&GodModeSettings { enabled: true });
        assert_eq!(chat_override("c1"), None, "a settings change forgets overrides");
        assert!(GodSession::for_run("o", "w", "e3", None, None).is_some(), "default on");
        set_chat_override("c2", Some(false));
        assert!(GodSession::for_chat("o", "w", "c2").is_none(), "a chat can turn it off");
        assert!(GodSession::for_chat("o", "w", "c3").is_some());
        configure(&GodModeSettings { enabled: false });
    }

    #[tokio::test]
    async fn a_covered_command_runs_without_asking_and_a_blocked_one_always_asks() {
        let _lock = test_lock();
        let _dir = with_temp_store();
        configure(&GodModeSettings { enabled: true });
        let chat = session("s9");
        record(&chat, &["shell^git clone".to_string()], Lasting::Session);
        // No approver is installed in this test: a prompt would fail.
        with_session(Some(chat.clone()), async {
            authorize(GodAction::Shell {
                command: "git clone b",
                violation: "outside",
                catastrophic: false,
            })
            .await
            .expect("granted for the session");
            assert!(
                authorize(GodAction::Shell {
                    command: "git push",
                    violation: "outside",
                    catastrophic: false,
                })
                .await
                .is_err(),
                "a different subcommand is not covered"
            );
            assert!(
                authorize(GodAction::Shell {
                    command: "git clone b",
                    violation: "blocked",
                    catastrophic: true,
                })
                .await
                .is_err(),
                "a blocked-list command always asks"
            );
        })
        .await;
        configure(&GodModeSettings { enabled: false });
    }

    /// God mode does not apply to VibeDev: inside a coding run there is no
    /// session, whatever an outer scope set.
    #[tokio::test]
    async fn god_mode_never_applies_inside_a_vibedev_coding_run() {
        let _scope = test_scope(true);
        let chat = session("vd1");
        let outside = with_session(Some(chat.clone()), async { current_session() }).await;
        assert_eq!(outside, Some(chat.clone()));
        let inside = crate::magician_v2::execution::coding_engine::with_coding_context(
            true,
            with_session(Some(chat), async { (current_session(), active()) }),
        )
        .await;
        assert_eq!(inside, (None, false));
    }

    /// An approval answered after what it rests on changed is refused: the
    /// chat's or its run's choice, the settings, super god mode turned off or
    /// past its end.
    #[test]
    fn an_approval_answered_after_what_it_rests_on_changed_is_refused() {
        let _scope = test_scope(true);
        let shell = GodAction::Shell {
            command: "ls",
            violation: "v",
            catastrophic: false,
        };
        let chat = session("late1");
        let god_before = fingerprint(&settings());
        let super_before = super_settings();
        assert!(approval_still_holds(&shell, &chat, &god_before, &super_before).is_ok());

        set_chat_override("late1", Some(false));
        assert!(
            approval_still_holds(&shell, &chat, &god_before, &super_before).is_err(),
            "the chat turned god mode off while the card was open"
        );
        set_chat_override("late1", None);
        assert!(approval_still_holds(&shell, &chat, &god_before, &super_before).is_ok());

        let run = GodSession::for_run("owner", "default", "e-late", None, Some("late2")).unwrap();
        set_chat_override("late2", Some(false));
        assert!(
            approval_still_holds(&shell, &run, &god_before, &super_before).is_err(),
            "a run follows its launching chat"
        );

        configure(&GodModeSettings { enabled: false });
        assert!(
            approval_still_holds(&shell, &chat, &god_before, &super_before).is_err(),
            "the settings changed"
        );
        configure(&GodModeSettings { enabled: true });

        let harness = GodAction::UnrestrictedHarness {
            engine: "codex",
            repo: None,
        };
        configure_super(&SuperGodModeSettings { enabled: true, until_ms: None });
        let god_before = fingerprint(&settings());
        let super_before = super_settings();
        assert!(approval_still_holds(&harness, &chat, &god_before, &super_before).is_ok());
        configure_super(&SuperGodModeSettings::default());
        assert!(
            approval_still_holds(&harness, &chat, &god_before, &super_before).is_err(),
            "super god mode turned off"
        );
        // Its end passing changes no settings, but it is over.
        let ended = SuperGodModeSettings { enabled: true, until_ms: Some(1) };
        configure_super(&ended);
        assert!(
            approval_still_holds(&harness, &chat, &god_before, &ended).is_err(),
            "super god mode past its end"
        );
    }

    /// A VibeDev launch needs the run's trust scoped by the run seam: none
    /// scoped, or a reviewed agent, starts restricted even with a grant.
    #[tokio::test]
    async fn a_vibedev_launch_needs_a_trusted_run() {
        let _scope = test_scope(false);
        configure_super(&SuperGodModeSettings { enabled: true, until_ms: None });
        let run = launch_session("owner", "default", Some("e-trust"), None, None, None).unwrap();
        record(&run, &["harness*".to_string()], Lasting::Session);
        let launch = || {
            approve_unrestricted_launch_for_run("owner", "default", Some("e-trust"), None, "codex", None)
        };
        assert!(!launch().await, "no run trust scoped");
        assert!(!with_run_trust(None, launch()).await, "run trust cleared");
        assert!(
            !with_run_trust(Some(RunTrust::level(Some("reviewed"))), launch()).await,
            "a reviewed agent"
        );
        assert!(with_run_trust(Some(RunTrust::level(None)), launch()).await, "a built-in agent");
        assert!(with_run_trust(Some(RunTrust::level(Some("local"))), launch()).await);
    }

    /// A delegated run shares its root run's approval session: "this session"
    /// answered for the coordinator covers the engineer's launch.
    #[tokio::test]
    async fn a_delegated_run_shares_its_roots_session() {
        let _scope = test_scope(false);
        configure_super(&SuperGodModeSettings { enabled: true, until_ms: None });
        let root = launch_session("owner", "default", Some("e-root"), None, None, None).unwrap();
        assert_eq!(root.session_key, "run:e-root");
        record(&root, &["harness*".to_string()], Lasting::Session);
        let child = launch_session("owner", "default", Some("e-child"), Some("e-root"), None, None).unwrap();
        assert_eq!(child.session_key, "run:e-root", "the engineer's session is the root's");
        assert_eq!(child.execution_id.as_deref(), Some("e-child"), "its card still goes to the engineer's run");
        let trust = Some(RunTrust { level: None, session_run: Some("e-root".to_string()) });
        let launch = approve_unrestricted_launch_for_run("owner", "default", Some("e-child"), None, "codex", None);
        assert!(with_run_trust(trust, launch).await, "covered by the root's session grant");
    }

    /// Turning super god mode off stops a harness that started unrestricted:
    /// its own cancellation fires, the parent's (the run's) does not.
    #[tokio::test]
    async fn turning_super_off_stops_an_unrestricted_harness() {
        let _scope = test_scope(false);
        configure_super(&SuperGodModeSettings { enabled: true, until_ms: None });
        let parent = tokio_util::sync::CancellationToken::new();
        let (token, watch) = unrestricted_cancel(Some(&parent), "codex");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!token.is_cancelled() && !watch.revoked(), "runs while super god mode is on");
        configure_super(&SuperGodModeSettings::default());
        tokio::time::timeout(std::time::Duration::from_secs(2), token.cancelled())
            .await
            .expect("stopped at once when turned off");
        assert!(watch.revoked());
        assert!(!parent.is_cancelled(), "the run itself goes on");
    }

    /// Session and trust share one task-local; setting either keeps the
    /// other, and the combined seam sets both in one layer.
    #[tokio::test]
    async fn session_and_trust_scopes_keep_each_other() {
        let session = Some(GodSession {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            session_key: "run:e-nest".to_string(),
            execution_id: Some("e-nest".to_string()),
            task_id: None,
            chat: None,
        });
        let trust = Some(RunTrust::level(Some("local")));
        let read = || async { (current_session().map(|s| s.session_key), current_run_trust()) };
        let key = session.as_ref().map(|s| s.session_key.clone());
        let (seen_session, seen_trust) =
            with_run_trust(trust.clone(), with_session(session.clone(), read())).await;
        assert_eq!((seen_session, seen_trust), (key.clone(), trust.clone()), "session inside trust");
        let (seen_session, seen_trust) =
            with_session(session.clone(), with_run_trust(trust.clone(), read())).await;
        assert_eq!((seen_session, seen_trust), (key.clone(), trust.clone()), "trust inside session");
        let (seen_session, seen_trust) =
            with_session_and_trust(session.clone(), trust.clone(), with_run_trust(None, read())).await;
        assert_eq!((seen_session, seen_trust), (key, None), "an inner clear wins");
        assert_eq!(with_session(None, read()).await.0, None, "an explicit None session stays None");
    }

    /// A harness launch in an agent run asks in the run, not in the chat that
    /// launched it; a chat turn has no run and asks in the chat.
    #[test]
    fn a_harness_launch_asks_in_the_run_not_its_chat() {
        let run = launch_session("o", "w", Some("e1"), None, Some("c1"), Some("t1")).unwrap();
        assert_eq!(run.session_key, "run:e1");
        assert_eq!(run.execution_id.as_deref(), Some("e1"));
        let chat = launch_session("o", "w", None, None, Some("c1"), None).unwrap();
        assert_eq!(chat.session_key, "chat:c1");
        assert!(launch_session("o", "w", Some("  "), None, None, None).is_none());
    }

    /// Turning a chat's god mode off forgets its god-mode approvals, not its
    /// super god mode harness approvals.
    #[test]
    fn turning_a_chat_off_keeps_its_harness_approvals() {
        let _scope = test_scope(true);
        let chat = session("keep1");
        record(&chat, &["harness*".to_string(), program_grant("git")], Lasting::Session);
        set_chat_override("keep1", Some(false));
        let left = grants_for(&chat);
        assert!(left.contains("harness*"));
        assert!(!left.contains(&program_grant("git")));
    }
}
