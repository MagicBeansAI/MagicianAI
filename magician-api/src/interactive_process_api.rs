//! HTTP surface for the Developer Mode `interactive_process` primitive.
//!
//! Exposes the endpoints the UI's xterm.js pane needs:
//!
//! - `POST /api/magician/v2/interactive-sessions` — start a scoped
//!   PTY session from Developer Mode.
//! - `POST /api/magician/v2/interactive-sessions/{id}/stdin` — write
//!   user keystrokes to the PTY's stdin alongside whatever the agent
//!   is sending. The body is base64-encoded raw bytes; the same
//!   `interactive_process::write_session` the agent uses backs this.
//! - `GET /api/magician/v2/interactive-sessions` — list live sessions
//!   for the current scope (used by Phase 3's multi-session tabs).
//! - `GET /api/magician/v2/interactive-sessions/cli-runtimes` — list
//!   the server-configured operator CLI runtimes for the Workbench launcher.
//! - `GET /api/magician/v2/interactive-sessions/{id}/buffer` — read a
//!   non-draining replay buffer for reconnecting Developer Mode panes.
//! - `POST /api/magician/v2/interactive-sessions/{id}/resize` — set the
//!   PTY window size to the viewer's terminal grid.
//! - `DELETE /api/magician/v2/interactive-sessions/{id}` — gracefully
//!   close a session via `close_session` (used by the "× cancel"
//!   button on each tab).
//!
//! All endpoints are scope-checked against the workspace-bound bearer
//! headers (or query params for the auth-less local dev path) so a
//! caller in scope A cannot peek into scope B's processes.
//!
//! A terminal runs arbitrary programs as the host user, so every endpoint
//! here is also **owner-only** (`require_terminal_operator`): a member's
//! session, a bot token, or a terminal grant is refused even inside its own
//! scope. `start` additionally refuses any program that is not in
//! `interactive_process.operator_cli_programs`.
//!
//! See `docs/plans/2026-05-13-developer-mode-workbench.md` Phases 2-3.

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use magician::config::{InteractiveProcessConfig, InteractiveSessionHostMode};
use magician::magician_v2::auth::middleware::{authenticated, AuthRuntime};
use magician::magician_v2::auth::BearerKind;
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::execution::interactive_process as ip;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// Storage root used when the server did not register one (tests).
const DEFAULT_STORAGE_ROOT: &str = "magician_data_v3";

/// Viewer size bounds. A real terminal grid fits comfortably; anything
/// outside is a client bug, not a window.
const MAX_TERMINAL_ROWS: u16 = 500;
const MAX_TERMINAL_COLS: u16 = 1000;

/// How often `list` asks tmux for sessions a previous process left behind.
/// Adoption only matters after a restart, so the list poll need not fork
/// `tmux` every time.
const ADOPTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Refuse anyone but the install's owner. Shared with other owner-only
/// surfaces that run or trust code as the host user (VibeDev trusted repos).
///
/// The owner is the first login identity (the same rule enrollment
/// uses). With no stamped identity, access is allowed only while the
/// install has no identity at all — the middleware's single-user bootstrap
/// — or when the server runs without the auth layer (tests).
pub(crate) fn require_terminal_operator(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
) -> Result<(), HttpResponse> {
    let forbidden = |message: &str| {
        HttpResponse::Forbidden().json(serde_json::json!({
            "error": "terminal_owner_only",
            "message": message,
        }))
    };
    let Some(stamped) = authenticated(req) else {
        let Some(auth) = auth else {
            return Ok(());
        };
        return match auth.store.identities_empty() {
            Ok(true) => Ok(()),
            Ok(false) => Err(forbidden("Sign in as the owner to use terminals.")),
            Err(error) => Err(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "auth_store_failure",
                "message": error.to_string(),
            }))),
        };
    };
    match stamped.bearer {
        BearerKind::Session(_) | BearerKind::ApiToken => {},
        BearerKind::Grant { .. } | BearerKind::Bot { .. } => {
            return Err(forbidden(
                "Terminals run programs as the host user; only the owner can use them.",
            ));
        },
    }
    let owner = auth
        .and_then(|auth| auth.store.list_identities().ok())
        .and_then(|identities| identities.first().map(|identity| identity.name.clone()));
    match (owner, stamped.identity_name) {
        (Some(owner), Some(caller)) if owner == caller => Ok(()),
        _ => Err(forbidden(
            "Terminals run programs as the host user; only the owner can use them.",
        )),
    }
}

fn storage_root_of(storage_root: &Option<web::Data<PathBuf>>) -> PathBuf {
    storage_root
        .as_ref()
        .map(|root| root.get_ref().clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STORAGE_ROOT))
}

/// The host `interactive_process.session_host` asks for, on this machine.
fn session_host_for(
    config: &InteractiveProcessConfig,
    storage_root: &Path,
) -> Result<ip::SessionHost, String> {
    if config.session_host == InteractiveSessionHostMode::Direct {
        return Ok(ip::SessionHost::Direct);
    }
    let tmux = ip::TmuxHost::locate(
        ip::TmuxHost::socket_for_root(storage_root),
        storage_root.join("interactive_sessions").join("tmux.conf"),
    );
    match (tmux, config.session_host) {
        (Some(host), _) => Ok(ip::SessionHost::Tmux(host)),
        (None, InteractiveSessionHostMode::Tmux) => Err(
            "interactive_process.session_host is `tmux` but no tmux binary was found".to_string(),
        ),
        (None, _) => Ok(ip::SessionHost::Direct),
    }
}

fn broadcast_config(
    event_broadcaster: &Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
    principal: &str,
    workspace: &str,
    ui_thread_id: Option<String>,
) -> Option<ip::PtyBroadcastConfig> {
    event_broadcaster
        .as_ref()
        .map(|data| ip::PtyBroadcastConfig {
            sink: Arc::clone(data.get_ref()) as Arc<dyn ip::PtyEventSink>,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            ui_thread_id,
        })
}

/// Re-attach to this scope's tmux-hosted terminals that a previous
/// Magician process started, at most once per `ADOPTION_INTERVAL` per scope.
async fn adopt_orphaned_sessions(
    principal: &str,
    workspace: &str,
    config: &InteractiveProcessConfig,
    storage_root: &Path,
    event_broadcaster: &Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) {
    static LAST_ADOPTION: std::sync::OnceLock<
        std::sync::Mutex<HashMap<(String, String), std::time::Instant>>,
    > = std::sync::OnceLock::new();
    let Ok(ip::SessionHost::Tmux(host)) = session_host_for(config, storage_root) else {
        return;
    };
    let key = (principal.to_string(), workspace.to_string());
    {
        let mut last = LAST_ADOPTION
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        if last
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < ADOPTION_INTERVAL)
        {
            return;
        }
        last.insert(key, now);
    }
    let registry = ip::registry_for_scope(principal, workspace);
    let principal = principal.to_string();
    let workspace = workspace.to_string();
    let broadcaster = event_broadcaster.clone();
    let adopted = tokio::task::spawn_blocking(move || {
        ip::adopt_tmux_sessions(&registry, &host, &principal, &workspace, |ui_thread_id| {
            broadcast_config(&broadcaster, &principal, &workspace, ui_thread_id)
        })
    })
    .await
    .unwrap_or_default();
    if !adopted.is_empty() {
        tracing::info!(
            target: "magician::interactive_process",
            count = adopted.len(),
            "re-attached tmux-hosted terminals from a previous process"
        );
    }
}

#[derive(Debug, Deserialize)]
pub struct StdinRequest {
    /// Base64-encoded raw bytes to write to the child's stdin. The
    /// frontend xterm pane base64-encodes keystrokes before posting so
    /// arbitrary key sequences (arrows, escape, control codes) can
    /// round-trip safely through JSON.
    pub bytes_b64: String,
}

#[derive(Debug, Deserialize)]
pub struct StartSessionRequest {
    /// Executable to spawn. Invoked directly, not via a shell.
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub rows: Option<u16>,
    #[serde(default)]
    pub cols: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct ResizeRequest {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Serialize)]
pub struct StartSessionResponse {
    pub session_id: String,
    pub program: String,
    pub initial_output: String,
    pub truncated_bytes: usize,
    pub alive: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct StdinResponse {
    pub session_id: String,
    pub bytes_written: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionBufferResponse {
    pub session_id: String,
    pub program: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    pub bytes_b64: String,
    pub start_offset: u64,
    pub end_offset: u64,
    pub alive: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Serialize)]
pub struct ListCliRuntimesResponse {
    pub runtimes: Vec<CliRuntimeSummary>,
}

#[derive(Debug, Serialize)]
pub struct CliRuntimeSummary {
    pub program: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub program: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    pub created_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_output_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_input_at_ms: Option<i64>,
    pub replay_start_offset: u64,
    pub replay_end_offset: u64,
    pub alive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The program survives a Magician restart (tmux-hosted).
    pub persistent: bool,
}

#[derive(Debug, Deserialize)]
pub struct DirectoryListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DirectoryListResponse {
    pub path: String,
    pub parent: Option<String>,
    pub home: Option<String>,
    pub root: String,
    pub current_dir: String,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
}

/// `GET /api/magician/v2/interactive-sessions/cli-runtimes`
///
/// Returns the server-owned Workbench CLI catalog for the current
/// scope. The scope check keeps this endpoint aligned with the
/// interactive-session lifecycle even though the catalog itself is
/// process configuration.
pub async fn list_cli_runtimes_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    resources: web::Data<Arc<AgentResources>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    if let Err(response) = resolve_required_scope(req.headers(), workspace) {
        return response;
    }
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }

    let config = resources
        .get_ref()
        .as_ref()
        .magician_config_snapshot()
        .interactive_process;
    let mut seen = HashSet::new();
    let mut runtimes: Vec<CliRuntimeSummary> = config
        .operator_cli_programs
        .iter()
        .map(|program| program.trim())
        .filter(|program| !program.is_empty())
        .filter(|program| seen.insert((*program).to_string()))
        .map(|program| CliRuntimeSummary {
            program: program.to_string(),
            label: program.to_string(),
            detail: "Configured CLI runtime".to_string(),
        })
        .collect();

    runtimes.sort_by(|a, b| {
        a.label
            .to_ascii_lowercase()
            .cmp(&b.label.to_ascii_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });

    HttpResponse::Ok().json(ListCliRuntimesResponse { runtimes })
}

/// `GET /api/magician/v2/filesystem/directories`
///
/// Lists server-side directories for the Developer Mode CWD picker.
/// This intentionally avoids browser-native directory picker APIs:
/// Magician often runs inside a Linux container behind a tunnel while
/// the browser is on macOS/iOS. The returned paths are server paths,
/// which is exactly what `interactive_process` needs for `working_dir`.
pub async fn list_directories_handler(
    req: HttpRequest,
    query: web::Query<DirectoryListQuery>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let DirectoryListQuery { workspace, path } = query.into_inner();
    if let Err(response) = resolve_required_scope(req.headers(), workspace) {
        return response;
    }
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }

    let requested = path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    let result = tokio::task::spawn_blocking(move || list_directories(requested.as_deref()))
        .await
        .unwrap_or_else(|error| Err(format!("directory listing task panicked: {error}")));

    match result {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error,
        })),
    }
}

fn list_directories(requested: Option<&Path>) -> Result<DirectoryListResponse, String> {
    const MAX_ENTRIES: usize = 240;

    let current_dir = env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let root = PathBuf::from("/");
    let base = requested
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(current_dir.as_path());
    let canonical = fs::canonicalize(base)
        .map_err(|error| format!("failed to open directory `{}`: {error}", base.display()))?;
    if !canonical.is_dir() {
        return Err(format!("`{}` is not a directory", canonical.display()));
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&canonical).map_err(|error| {
        format!(
            "failed to read directory `{}`: {error}",
            canonical.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("failed to read directory entry: {error}"))?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(DirectoryEntry {
            name,
            path: entry.path().display().to_string(),
        });
    }

    entries.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);

    Ok(DirectoryListResponse {
        parent: canonical.parent().map(|path| path.display().to_string()),
        path: canonical.display().to_string(),
        home: home.map(|path| path.display().to_string()),
        root: root.display().to_string(),
        current_dir: current_dir.display().to_string(),
        entries,
        truncated,
    })
}

/// `POST /api/magician/v2/interactive-sessions`
///
/// Starts a live PTY session for Developer Mode. This is the
/// operator-facing counterpart to the autonomous `interactive_process`
/// control tool: same scoped registry, same concurrency caps, but
/// triggered directly by the Workbench launcher instead of by the LLM.
pub async fn start_session_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<StartSessionRequest>,
    event_broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
    resources: web::Data<Arc<AgentResources>>,
    auth: Option<web::Data<AuthRuntime>>,
    storage_root: Option<web::Data<PathBuf>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let payload = body.into_inner();
    let program = payload.program.trim().to_string();
    if program.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "`program` is required",
        }));
    }
    let config = resources
        .get_ref()
        .as_ref()
        .magician_config_snapshot()
        .interactive_process;
    // The launcher only offers the catalog; the server is what enforces it.
    if !config
        .operator_cli_programs
        .iter()
        .any(|allowed| allowed.trim() == program)
    {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": format!(
                "`{program}` is not in interactive_process.operator_cli_programs"
            ),
            "program": program,
        }));
    }
    let storage_root = storage_root_of(&storage_root);
    let host = match session_host_for(&config, &storage_root) {
        Ok(host) => host,
        Err(error) => {
            return HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": error,
                "program": program,
            }));
        },
    };
    // A restart may have left this scope's terminals running; count them
    // against the cap before admitting another.
    adopt_orphaned_sessions(
        &principal,
        &workspace,
        &config,
        &storage_root,
        &event_broadcaster,
    )
    .await;

    let rows = payload
        .rows
        .filter(|value| *value > 0)
        .unwrap_or(40)
        .min(MAX_TERMINAL_ROWS);
    let cols = payload
        .cols
        .filter(|value| *value > 0)
        .unwrap_or(140)
        .min(MAX_TERMINAL_COLS);
    let ui_thread_id = payload
        .ui_thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let working_dir = payload
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let registry = ip::registry_for_scope(&principal, &workspace);
    let broadcast = broadcast_config(
        &event_broadcaster,
        &principal,
        &workspace,
        ui_thread_id.clone(),
    );

    // The operator's terminal, not the service's environment: Magician's
    // `.env` credentials and the Makefile's build knobs are removed, and the
    // terminal type matches what xterm.js actually renders.
    let env_remove = ip::operator_terminal_env_scrub_list();
    let mut env = payload.env;
    env.entry("TERM".to_string())
        .or_insert_with(|| "xterm-256color".to_string());
    env.entry("COLORTERM".to_string())
        .or_insert_with(|| "truecolor".to_string());
    let concurrency_limit = config.max_concurrent_per_program.get(&program).copied();
    let program_for_spawn = program.clone();
    // A shell from the launcher is a login shell, so the operator's own
    // `.zprofile`/`.bash_profile` — their PATH, toolchains, aliases — load
    // exactly as in their terminal app.
    let args = if payload.args.is_empty() && is_login_shell_program(&program) {
        vec!["-l".to_string()]
    } else {
        payload.args
    };
    let scope_principal = principal.clone();
    let scope_workspace = workspace.clone();
    let started = tokio::task::spawn_blocking(move || {
        ip::start_session_with(
            &registry,
            ip::StartSpec {
                program: &program_for_spawn,
                args: &args,
                working_dir: working_dir.as_deref(),
                env: &env,
                env_remove: &env_remove,
                rows,
                cols,
                host: &host,
                concurrency_limit,
                scope: Some((&scope_principal, &scope_workspace)),
            },
            broadcast,
        )
    })
    .await;

    match started {
        Ok(Ok((session_id, first))) => HttpResponse::Ok().json(StartSessionResponse {
            session_id,
            program,
            initial_output: first.output,
            truncated_bytes: first.truncated_bytes,
            alive: first.alive,
            exit_code: first.exit_code,
        }),
        Ok(Err(error)) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error,
            "program": program,
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("interactive session start task failed: {error}"),
            "program": program,
        })),
    }
}

/// Shells the launcher starts as login shells.
fn is_login_shell_program(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    matches!(name, "zsh" | "bash" | "fish" | "sh")
}

#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    /// Any directory inside the repository; its top level is used.
    pub repo_path: String,
    /// The new branch (or an existing one to check out).
    pub branch: String,
    /// Start point for a new branch; default: the repo's current `HEAD`.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateWorktreeResponse {
    pub path: String,
    pub branch: String,
    pub repo_root: String,
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(runtime_core::process::git_program())
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `git worktree add` beside the repo: `<repo>-<branch slug>` next to the
/// repository root. A new branch starts at `base` (default `HEAD`); an existing
/// branch is checked out.
fn create_worktree(request: &CreateWorktreeRequest) -> Result<CreateWorktreeResponse, String> {
    let branch = request.branch.trim();
    run_git(Path::new("."), &["check-ref-format", "--branch", branch])
        .map_err(|_| format!("`{branch}` is not a valid branch name"))?;
    let start = PathBuf::from(request.repo_path.trim());
    let top = run_git(&start, &["rev-parse", "--show-toplevel"])
        .map_err(|_| format!("`{}` is not inside a git repository", start.display()))?;
    let top = PathBuf::from(top);
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = top
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let parent = top
        .parent()
        .ok_or_else(|| format!("`{}` has no parent directory", top.display()))?;
    let path = parent.join(format!("{name}-{}", slug.trim_matches('-')));
    if path.exists() {
        return Err(format!("`{}` already exists", path.display()));
    }
    let path_str = path.to_string_lossy().into_owned();
    let branch_exists = run_git(
        &top,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if branch_exists {
        run_git(&top, &["worktree", "add", &path_str, branch])?;
    } else {
        let base = request
            .base
            .as_deref()
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .unwrap_or("HEAD");
        run_git(&top, &["worktree", "add", "-b", branch, &path_str, base])?;
    }
    Ok(CreateWorktreeResponse {
        path: path_str,
        branch: branch.to_string(),
        repo_root: top.display().to_string(),
    })
}

/// `POST /api/magician/v2/filesystem/worktrees` — create a git worktree for a
/// Workbench session (owner-only, like every terminal endpoint).
pub async fn create_worktree_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<CreateWorktreeRequest>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    if let Err(response) = resolve_required_scope(req.headers(), workspace) {
        return response;
    }
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let request = body.into_inner();
    match tokio::task::spawn_blocking(move || create_worktree(&request)).await {
        Ok(Ok(created)) => HttpResponse::Ok().json(created),
        Ok(Err(error)) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "worktree_create_failed",
            "message": error,
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("worktree task failed: {error}"),
        })),
    }
}

/// `POST /api/magician/v2/interactive-sessions/{id}/resize`
///
/// Sets the PTY window to the viewer's grid so a full-screen CLI draws at
/// the width the browser shows. With several viewers the last resize wins,
/// as with several tmux clients.
pub async fn resize_session_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<ResizeRequest>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let session_id = path.into_inner();
    let ResizeRequest { rows, cols } = body.into_inner();
    if rows == 0 || cols == 0 || rows > MAX_TERMINAL_ROWS || cols > MAX_TERMINAL_COLS {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!("terminal size {cols}x{rows} is out of range"),
            "session_id": session_id,
        }));
    }
    let registry = ip::registry_for_scope(&principal, &workspace);
    match ip::resize_session(&registry, &session_id, rows, cols) {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "session_id": session_id,
            "rows": rows,
            "cols": cols,
        })),
        Err(error) if error.starts_with("unknown session") => {
            HttpResponse::NotFound().json(serde_json::json!({
                "error": error,
                "session_id": session_id,
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error,
            "session_id": session_id,
        })),
    }
}

/// `POST /api/magician/v2/interactive-sessions/{id}/stdin`
pub async fn write_stdin_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<StdinRequest>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let session_id = path.into_inner();
    let payload = body.into_inner();

    // Decode the base64 payload. Invalid base64 → 400 with a clear
    // error so the frontend's "failed to send" indicator surfaces
    // why.
    let bytes = match base64::engine::general_purpose::STANDARD.decode(&payload.bytes_b64) {
        Ok(bytes) => bytes,
        Err(error) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("invalid base64 payload: {error}"),
                "session_id": session_id,
            }));
        },
    };

    let registry = ip::registry_for_scope(&principal, &workspace);
    // write_session takes a &str. Most legitimate xterm payloads are
    // valid UTF-8 (or close enough — modifier escapes are ASCII). For
    // arbitrary bytes we use lossy decode so the call never fails
    // mid-flight just because of an exotic key sequence.
    let as_str = String::from_utf8(bytes.clone())
        .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());

    match ip::write_session(&registry, &session_id, &as_str) {
        Ok(()) => HttpResponse::Ok().json(StdinResponse {
            session_id,
            bytes_written: bytes.len(),
        }),
        Err(error) => HttpResponse::NotFound().json(serde_json::json!({
            "error": error,
            "session_id": session_id,
        })),
    }
}

/// `GET /api/magician/v2/interactive-sessions`
///
/// Returns every live session in the current scope. Used by the
/// Developer Mode tab strip to discover sessions the agent has spawned.
/// (Phase 3.)
pub async fn list_sessions_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    event_broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
    resources: web::Data<Arc<AgentResources>>,
    auth: Option<web::Data<AuthRuntime>>,
    storage_root: Option<web::Data<PathBuf>>,
) -> impl Responder {
    let ScopeQuery {
        workspace,
        ui_thread_id,
    } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    // The first list after a restart is how the Workbench gets its
    // tmux-hosted terminals back.
    let config = resources
        .get_ref()
        .as_ref()
        .magician_config_snapshot()
        .interactive_process;
    adopt_orphaned_sessions(
        &principal,
        &workspace,
        &config,
        &storage_root_of(&storage_root),
        &event_broadcaster,
    )
    .await;
    let registry = ip::registry_for_scope(&principal, &workspace);
    let summaries: Vec<SessionSummary> = registry
        .live_ids()
        .into_iter()
        .filter_map(|id| {
            registry.get(&id).map(|session_arc| {
                let guard = session_arc.lock().unwrap_or_else(|e| e.into_inner());
                let replay = guard.replay_metadata();
                SessionSummary {
                    session_id: id,
                    program: guard.program.clone(),
                    ui_thread_id: guard.ui_thread_id.clone(),
                    working_dir: guard
                        .working_dir
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    created_at_ms: guard.created_at_ms,
                    last_output_at_ms: guard.last_output_at_ms(),
                    last_input_at_ms: guard.last_input_at_ms(),
                    replay_start_offset: replay.start_offset,
                    replay_end_offset: replay.end_offset,
                    alive: guard.is_alive(),
                    exit_code: guard.exit_code(),
                    persistent: guard.is_persistent(),
                }
            })
        })
        .filter(|summary| {
            match ui_thread_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(thread_id) => summary.ui_thread_id.as_deref() == Some(thread_id),
                None => true,
            }
        })
        .collect();
    HttpResponse::Ok().json(ListSessionsResponse {
        sessions: summaries,
    })
}

/// `GET /api/magician/v2/interactive-sessions/{id}/buffer`
///
/// Returns a bounded, non-draining replay snapshot for UI reconnect.
/// This deliberately does not call `interactive_process::read_session`
/// because that operation drains the agent-readable buffer; terminal
/// hydration must never steal output from the agent automation loop.
pub async fn session_buffer_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let session_id = path.into_inner();
    let registry = ip::registry_for_scope(&principal, &workspace);

    let Some(session_arc) = registry.get(&session_id) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown session",
            "session_id": session_id,
        }));
    };

    let guard = session_arc.lock().unwrap_or_else(|e| e.into_inner());
    let snapshot = guard.snapshot_replay_buffer();
    let bytes_b64 = base64::engine::general_purpose::STANDARD.encode(&snapshot.bytes);
    HttpResponse::Ok().json(SessionBufferResponse {
        session_id,
        program: guard.program.clone(),
        ui_thread_id: guard.ui_thread_id.clone(),
        bytes_b64,
        start_offset: snapshot.start_offset,
        end_offset: snapshot.end_offset,
        alive: guard.is_alive(),
        exit_code: guard.exit_code(),
    })
}

/// `GET /api/magician/v2/interactive-sessions/{id}/diff`
///
/// Returns a list of files the agent has touched in the session's
/// `working_dir` since spawn, sourced from `git diff --name-status HEAD`.
/// Each entry includes the unified diff against HEAD so the
/// Developer-Mode DiffStrip can render inline.
///
/// No-op when the session has no `working_dir` or the path isn't inside
/// a git repo — returns `{ files: [] }` with `note` explaining.
pub async fn session_diff_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let session_id = path.into_inner();
    let registry = ip::registry_for_scope(&principal, &workspace);

    let working_dir = match registry.get(&session_id) {
        Some(arc) => {
            let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
            guard.working_dir.clone()
        },
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown session",
                "session_id": session_id,
            }));
        },
    };

    let Some(dir) = working_dir else {
        let empty: Vec<serde_json::Value> = Vec::new();
        return HttpResponse::Ok().json(serde_json::json!({
            "files": empty,
            "note": "session has no working_dir; nothing to diff",
        }));
    };

    let dir_clone = dir.clone();
    let files = tokio::task::spawn_blocking(move || compute_git_diff(&dir_clone))
        .await
        .unwrap_or_else(|e| Err(format!("diff task panicked: {e}")));

    match files {
        Ok(files) => HttpResponse::Ok().json(serde_json::json!({
            "files": files,
            "working_dir": dir.display().to_string(),
        })),
        Err(error) => {
            let empty: Vec<serde_json::Value> = Vec::new();
            HttpResponse::Ok().json(serde_json::json!({
                "files": empty,
                "note": error,
            }))
        },
    }
}

#[derive(Debug, Serialize)]
struct DiffFile {
    path: String,
    status: String,
    additions: usize,
    deletions: usize,
    unified_diff: String,
}

/// Run `git diff` in `dir` and parse the output into per-file entries.
/// Errors (not a git repo, git missing, etc.) are returned as Err
/// strings so the caller can pass the explanation through to the UI.
///
/// Three `git` spawns regardless of how many files changed. This used to spawn
/// one `git diff -- <path>` per changed file on top of the listing and the
/// untracked scan (`F + 2`), and the Workbench polls it every 3s, so a
/// 40-file branch was 42 forked processes every three seconds on the blocking
/// pool. The whole-tree patch is the concatenation of exactly those per-file
/// patches, so splitting it on `diff --git` headers reconstructs them.
fn compute_git_diff(dir: &std::path::Path) -> Result<Vec<DiffFile>, String> {
    use std::process::Command;

    let status = Command::new(runtime_core::process::git_program())
        .args(["diff", "--name-status", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "git diff failed (exit {:?}): {}",
            status.status.code(),
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }

    let patch_out = Command::new(runtime_core::process::git_program())
        .args(["diff", "--unified=3", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !patch_out.status.success() {
        return Err(format!(
            "git diff failed (exit {:?}): {}",
            patch_out.status.code(),
            String::from_utf8_lossy(&patch_out.stderr).trim()
        ));
    }
    let patch = String::from_utf8_lossy(&patch_out.stdout).to_string();
    // Both commands run the same diff machinery over the same worktree with the
    // same rename detection, so listing entry `i` is patch chunk `i`. Consume the
    // chunks strictly in step with the listing — including for a malformed
    // listing line — so one skipped entry can never shift every later file's diff
    // onto the wrong path.
    let mut chunks = split_unified_diff_by_file(&patch).into_iter();

    let listing = String::from_utf8_lossy(&status.stdout).to_string();
    let mut files = Vec::new();
    let mut seen_paths = HashSet::new();
    for line in listing.lines() {
        let unified = chunks.next().unwrap_or_default();

        let mut parts = line.split('\t');
        let status_code = parts.next().unwrap_or("").trim().to_string();
        let path = if status_code.starts_with('R') || status_code.starts_with('C') {
            // Rename/copy status has old and new path. Show the current path
            // because that is what users can open/edit from the Workbench.
            let _old_path = parts.next();
            parts.next().unwrap_or("").trim().to_string()
        } else {
            parts.next().unwrap_or("").trim().to_string()
        };
        if path.is_empty() {
            continue;
        }
        seen_paths.insert(path.clone());

        // Cheap counter — count lines starting with `+` (not `+++`) and
        // `-` (not `---`). Avoids spawning `--numstat` for each file.
        let mut adds = 0usize;
        let mut dels = 0usize;
        for diff_line in unified.lines() {
            if diff_line.starts_with("+++") || diff_line.starts_with("---") {
                continue;
            }
            if diff_line.starts_with('+') {
                adds += 1;
            } else if diff_line.starts_with('-') {
                dels += 1;
            }
        }

        files.push(DiffFile {
            path,
            status: status_code,
            additions: adds,
            deletions: dels,
            unified_diff: unified,
        });
    }

    let untracked = Command::new(runtime_core::process::git_program())
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git ls-files failed: {e}"))?;
    if !untracked.status.success() {
        return Err(format!(
            "git ls-files failed (exit {:?}): {}",
            untracked.status.code(),
            String::from_utf8_lossy(&untracked.stderr).trim()
        ));
    }
    for raw_path in untracked.stdout.split(|byte| *byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        let path = String::from_utf8_lossy(raw_path).to_string();
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        files.push(synthetic_untracked_diff(dir, &path));
    }

    Ok(files)
}

/// Split a whole-tree unified diff into one string per file, in git's order.
///
/// A header can only ever appear at column 0: every body line is prefixed with
/// a space, `+`, `-`, or `\`, so a file whose *content* contains a diff cannot
/// forge one. Each chunk is byte-identical to what
/// `git diff --unified=3 HEAD -- <path>` printed for that file.
///
/// `diff --cc` is a boundary too — an unmerged path in a conflicted tree gets a
/// combined diff rather than a `diff --git` one, and dropping it here would
/// misalign every entry after it.
fn split_unified_diff_by_file(patch: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") {
            chunks.push(String::new());
        }
        // Nothing precedes the first header in `git diff` output; if anything ever
        // did, dropping it beats misattributing it to a file.
        if let Some(body) = chunks.last_mut() {
            body.push_str(line);
        }
    }
    chunks
}

fn synthetic_untracked_diff(dir: &Path, path: &str) -> DiffFile {
    const MAX_INLINE_UNTRACKED_BYTES: usize = 256 * 1024;

    let full_path = dir.join(path);
    let bytes = match fs::read(&full_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return DiffFile {
                path: path.to_string(),
                status: "??".to_string(),
                additions: 0,
                deletions: 0,
                unified_diff: format!("untracked file could not be read: {error}\n"),
            };
        },
    };

    if bytes.len() > MAX_INLINE_UNTRACKED_BYTES || bytes.contains(&0) {
        return DiffFile {
            path: path.to_string(),
            status: "??".to_string(),
            additions: 0,
            deletions: 0,
            unified_diff: format!(
                "untracked file omitted from inline diff ({} bytes; binary or too large)\n",
                bytes.len()
            ),
        };
    }

    let text = String::from_utf8_lossy(&bytes);
    let additions = text.lines().count();
    let mut unified = String::new();
    unified.push_str(&format!("diff --git a/{path} b/{path}\n"));
    unified.push_str("new file mode 100644\n");
    unified.push_str("--- /dev/null\n");
    unified.push_str(&format!("+++ b/{path}\n"));
    if additions == 0 {
        unified.push_str("@@ -0,0 +0,0 @@\n");
    } else {
        unified.push_str(&format!("@@ -0,0 +1,{additions} @@\n"));
    }
    for raw_line in text.split_inclusive('\n') {
        unified.push('+');
        unified.push_str(raw_line);
        if !raw_line.ends_with('\n') {
            unified.push('\n');
        }
    }

    DiffFile {
        path: path.to_string(),
        status: "??".to_string(),
        additions,
        deletions: 0,
        unified_diff: unified,
    }
}

/// `DELETE /api/magician/v2/interactive-sessions/{id}`
pub async fn close_session_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    storage_root: Option<web::Data<PathBuf>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> impl Responder {
    let ScopeQuery { workspace, .. } = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = require_terminal_operator(&req, auth.as_ref()) {
        return response;
    }
    let session_id = path.into_inner();
    let registry = ip::registry_for_scope(&principal, &workspace);

    let base_root = storage_root_of(&storage_root);
    let principal_for_close = principal.clone();
    let workspace_for_close = workspace.clone();
    let session_id_for_close = session_id.clone();
    let closed = tokio::task::spawn_blocking(move || {
        let closed = ip::close_session_with_transcript(&registry, &session_id_for_close)?;
        ip::persist_session_transcript(
            &base_root,
            &principal_for_close,
            &workspace_for_close,
            &session_id_for_close,
            &closed.program,
            closed.replay_snapshot.start_offset,
            closed.replay_snapshot.end_offset,
            &closed.replay_snapshot.bytes,
        );
        Ok::<_, String>(closed.output)
    })
    .await;

    match closed {
        Ok(Ok(envelope)) => HttpResponse::Ok().json(envelope),
        Ok(Err(error)) => HttpResponse::NotFound().json(serde_json::json!({
            "error": error,
            "session_id": session_id,
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("interactive session close task failed: {error}"),
            "session_id": session_id,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test::TestRequest, HttpMessage};
    use magician::magician_v2::auth::{middleware::AuthenticatedRequest, AuthMethod, ScopeRef};

    fn stamped(bearer: BearerKind, identity_name: Option<&str>) -> HttpRequest {
        let req = TestRequest::post().to_http_request();
        req.extensions_mut().insert(AuthenticatedRequest {
            scope: ScopeRef::system_internal_unauthenticated("owner", "default"),
            identity_name: identity_name.map(str::to_string),
            bearer,
        });
        req
    }

    #[test]
    fn a_server_without_the_auth_layer_lets_the_terminal_through() {
        let req = TestRequest::post().to_http_request();
        assert!(require_terminal_operator(&req, None).is_ok());
    }

    #[test]
    fn bots_and_terminal_grants_never_get_a_terminal() {
        for bearer in [
            BearerKind::Bot {
                bot_name: "telegram".to_string(),
            },
            BearerKind::Grant {
                workspace: "default".to_string(),
            },
        ] {
            let response = require_terminal_operator(&stamped(bearer, Some("owner")), None)
                .expect_err("refused");
            assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn a_session_whose_owner_cannot_be_established_is_refused() {
        // Without the auth store there is no owner to compare against, so a
        // stamped caller is not assumed to be it.
        let response = require_terminal_operator(
            &stamped(BearerKind::Session(AuthMethod::Password), Some("owner")),
            None,
        )
        .expect_err("refused");
        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn shells_start_as_login_shells_and_clis_do_not() {
        assert!(is_login_shell_program("zsh"));
        assert!(is_login_shell_program("/bin/bash"));
        assert!(!is_login_shell_program("claude"));
    }

    #[test]
    fn a_worktree_is_created_beside_the_repo_on_a_new_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("proj");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        for args in [
            vec!["init", "--quiet", "--initial-branch", "main"],
            vec!["config", "user.email", "o@example.com"],
            vec!["config", "user.name", "O"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            run_git(&repo, &args).unwrap();
        }
        std::fs::write(repo.join("README.md"), "x\n").unwrap();
        run_git(&repo, &["add", "README.md"]).unwrap();
        run_git(&repo, &["commit", "--quiet", "-m", "init"]).unwrap();

        let created = create_worktree(&CreateWorktreeRequest {
            repo_path: repo.join("sub").display().to_string(),
            branch: "feature/login".to_string(),
            base: None,
        })
        .unwrap();
        let path = PathBuf::from(&created.path);
        assert_eq!(path.file_name().unwrap(), "proj-feature-login");
        assert!(path.join("README.md").is_file());
        assert_eq!(
            run_git(&path, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
            "feature/login"
        );
        // Twice is refused rather than reusing someone's directory.
        assert!(create_worktree(&CreateWorktreeRequest {
            repo_path: repo.display().to_string(),
            branch: "feature/login".to_string(),
            base: None,
        })
        .unwrap_err()
        .contains("already exists"));
        assert!(create_worktree(&CreateWorktreeRequest {
            repo_path: repo.display().to_string(),
            branch: "bad..name".to_string(),
            base: None,
        })
        .unwrap_err()
        .contains("not a valid branch name"));
    }

    #[test]
    fn direct_session_host_never_looks_for_tmux() {
        let config = InteractiveProcessConfig {
            session_host: InteractiveSessionHostMode::Direct,
            ..InteractiveProcessConfig::default()
        };
        assert_eq!(
            session_host_for(&config, Path::new("/tmp/magician-root")).unwrap(),
            ip::SessionHost::Direct
        );
    }

    #[test]
    fn auto_session_host_uses_tmux_only_when_it_is_installed() {
        let root = Path::new("/tmp/magician-root");
        let host = session_host_for(&InteractiveProcessConfig::default(), root).unwrap();
        match ip::TmuxHost::locate("probe", root.join("tmux.conf")) {
            Some(_) => match host {
                ip::SessionHost::Tmux(tmux) => {
                    assert_eq!(tmux.socket, ip::TmuxHost::socket_for_root(root));
                    assert_eq!(
                        tmux.config_path,
                        root.join("interactive_sessions/tmux.conf")
                    );
                },
                ip::SessionHost::Direct => panic!("tmux is installed but auto chose direct"),
            },
            None => assert_eq!(host, ip::SessionHost::Direct),
        }
    }

    /// A real `git diff --unified=3 HEAD` over a five-entry change: a binary
    /// modify, a text modify, a 100%-similarity rename, a path containing a
    /// space, and a delete. Captured verbatim from git rather than hand-written
    /// so the split is tested against the format it actually has to parse.
    const MULTI_FILE_PATCH: &str = concat!(
        "diff --git a/bin.dat b/bin.dat\n",
        "index 0f49c4a..4c2de3c 100644\n",
        "Binary files a/bin.dat and b/bin.dat differ\n",
        "diff --git a/modified.txt b/modified.txt\n",
        "index 2fa992c..fe5841d 100644\n",
        "--- a/modified.txt\n",
        "+++ b/modified.txt\n",
        "@@ -1 +1,2 @@\n",
        " keep\n",
        "+more\n",
        "diff --git a/old_name.txt b/new_name.txt\n",
        "similarity index 100%\n",
        "rename from old_name.txt\n",
        "rename to new_name.txt\n",
        "diff --git a/spaced name.txt b/spaced name.txt\n",
        "index 74b863c..4aaeed8 100644\n",
        "--- a/spaced name.txt\t\n",
        "+++ b/spaced name.txt\t\n",
        "@@ -1 +1 @@\n",
        "-x y\n",
        "+x y z\n",
        "diff --git a/to_delete.txt b/to_delete.txt\n",
        "deleted file mode 100644\n",
        "index abaddc0..0000000\n",
        "--- a/to_delete.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-del\n",
    );

    #[test]
    fn split_reproduces_each_per_file_patch_byte_for_byte() {
        let chunks = split_unified_diff_by_file(MULTI_FILE_PATCH);

        // One chunk per `git diff --name-status HEAD` entry, in the same order —
        // that 1:1 alignment is what lets the caller consume them positionally.
        assert_eq!(chunks.len(), 5);

        // Each chunk is exactly what `git diff --unified=3 HEAD -- <path>` printed
        // for that file back when the handler spawned one process per file.
        assert_eq!(
            chunks[0],
            concat!(
                "diff --git a/bin.dat b/bin.dat\n",
                "index 0f49c4a..4c2de3c 100644\n",
                "Binary files a/bin.dat and b/bin.dat differ\n",
            )
        );
        assert_eq!(
            chunks[1],
            concat!(
                "diff --git a/modified.txt b/modified.txt\n",
                "index 2fa992c..fe5841d 100644\n",
                "--- a/modified.txt\n",
                "+++ b/modified.txt\n",
                "@@ -1 +1,2 @@\n",
                " keep\n",
                "+more\n",
            )
        );
        assert_eq!(
            chunks[3],
            concat!(
                "diff --git a/spaced name.txt b/spaced name.txt\n",
                "index 74b863c..4aaeed8 100644\n",
                "--- a/spaced name.txt\t\n",
                "+++ b/spaced name.txt\t\n",
                "@@ -1 +1 @@\n",
                "-x y\n",
                "+x y z\n",
            ),
            "a path containing a space must not be split on whitespace"
        );
        assert_eq!(
            chunks[4],
            concat!(
                "diff --git a/to_delete.txt b/to_delete.txt\n",
                "deleted file mode 100644\n",
                "index abaddc0..0000000\n",
                "--- a/to_delete.txt\n",
                "+++ /dev/null\n",
                "@@ -1 +0,0 @@\n",
                "-del\n",
            )
        );

        // Nothing is dropped or duplicated: the chunks concatenate back to the
        // whole-tree patch.
        assert_eq!(chunks.concat(), MULTI_FILE_PATCH);
    }

    #[test]
    fn split_keeps_the_addition_and_deletion_counts_the_per_file_spawns_produced() {
        let chunks = split_unified_diff_by_file(MULTI_FILE_PATCH);
        let count = |unified: &str| {
            let mut adds = 0usize;
            let mut dels = 0usize;
            for line in unified.lines() {
                if line.starts_with("+++") || line.starts_with("---") {
                    continue;
                }
                if line.starts_with('+') {
                    adds += 1;
                } else if line.starts_with('-') {
                    dels += 1;
                }
            }
            (adds, dels)
        };

        assert_eq!(count(&chunks[1]), (1, 0), "one added line in modified.txt");
        assert_eq!(count(&chunks[3]), (1, 1), "one line rewritten");
        assert_eq!(count(&chunks[4]), (0, 1), "the deleted file's only line");
    }

    #[test]
    fn split_treats_a_combined_conflict_diff_as_its_own_file() {
        // An unmerged path gets `diff --cc`, not `diff --git`. Missing it would
        // glue the conflicted file's body onto the previous entry and shift every
        // later chunk onto the wrong path.
        let patch = concat!(
            "diff --git a/clean.txt b/clean.txt\n",
            "index 1111111..2222222 100644\n",
            "--- a/clean.txt\n",
            "+++ b/clean.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "diff --cc conflicted.txt\n",
            "index 3333333,4444444..0000000\n",
            "--- a/conflicted.txt\n",
            "+++ b/conflicted.txt\n",
            "@@@ -1,1 -1,1 +1,5 @@@\n",
            "++<<<<<<< HEAD\n",
            "+ ours\n",
            "++=======\n",
            "+ theirs\n",
            "++>>>>>>>\n",
        );

        let chunks = split_unified_diff_by_file(patch);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].starts_with("diff --git a/clean.txt"));
        assert!(chunks[1].starts_with("diff --cc conflicted.txt"));
        assert_eq!(chunks.concat(), patch);
    }

    #[test]
    fn split_of_an_empty_patch_yields_no_chunks() {
        assert!(split_unified_diff_by_file("").is_empty());
    }

    #[test]
    fn a_diff_inside_a_files_content_is_not_a_chunk_boundary() {
        // Body lines always carry a ` `, `+`, `-`, or `\` prefix, so a file that
        // itself contains a patch cannot forge a header.
        let patch = concat!(
            "diff --git a/notes.md b/notes.md\n",
            "index 1111111..2222222 100644\n",
            "--- a/notes.md\n",
            "+++ b/notes.md\n",
            "@@ -1 +1,2 @@\n",
            " intro\n",
            "+diff --git a/fake b/fake\n",
        );

        let chunks = split_unified_diff_by_file(patch);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], patch);
    }
}
