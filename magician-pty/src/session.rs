//! PTY-backed long-lived subprocess primitive for interactive CLIs.
//!
//! ## Why this exists
//!
//! The default `bash` lane runs `sh -c <cmd>` to completion and only
//! returns when the process exits. That works for batch tools but
//! breaks any CLI that needs:
//!
//! - **A real tty** — Claude Code, Codex, Gemini, OpenCode and most
//!   modern AI CLIs detect non-tty stdio and either refuse to render
//!   their interactive UI or drop into a degraded plain-text mode.
//! - **Mid-run prompts** — `gh repo create`, `npm init`, `gcloud auth
//!   login`, etc. issue a sequence of prompts where each answer
//!   determines what's asked next.
//! - **Cross-iteration state** — the inner-loop agent reads a chunk,
//!   decides what to write, sends a response; the same child process
//!   must still be alive on the next iteration.
//!
//! This module wraps `portable-pty` to spawn a child inside a real
//! pseudo-tty, keeps it alive in a per-execution session registry, and
//! exposes four ops the agent can call from the inner loop:
//!
//! | op     | what it does |
//! |--------|--------------|
//! | start  | spawn (`program`, `args`, `working_dir`, `env`), return `session_id` and the first burst of output |
//! | read   | drain currently-available output (capped) — non-blocking |
//! | write  | append bytes to the child's stdin (pass `\n` yourself when you want enter) |
//! | close  | send EOF, wait briefly, kill if it didn't exit; remove from registry |
//!
//! The agent treats `session_id` as opaque. Sessions are automatically
//! reaped when their `ActionExecutors` is dropped (end of execution),
//! so leaked processes can't survive an execution.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::{PtyChunkEvent, PtyEventSink};

/// Side-channel for streaming PTY bytes to the UI via the host's
/// `PtyEventSink` implementation. When supplied to `start_session`,
/// every chunk the reader thread receives is also emitted as a
/// `PtyChunkEvent` so subscribers (e.g. the Developer-Mode xterm.js
/// pane) see PTY output in real time without waiting for the agent's
/// next `op=read`.
///
/// Used by Developer Mode (see
/// `docs/plans/2026-05-13-developer-mode-workbench.md` Phase 1). When
/// `None`, the reader thread only writes to the in-memory buffer (the
/// existing agent-readable surface), so chat-mode users pay zero
/// broadcast overhead.
#[derive(Clone)]
pub struct PtyBroadcastConfig {
    pub sink: Arc<dyn PtyEventSink>,
    pub principal: String,
    pub workspace: String,
    pub ui_thread_id: Option<String>,
}

/// Global per-program concurrency limits.
///
/// Caps how many sessions for a given `program` can be live across
/// the entire process (summed over all scopes). A runaway launcher
/// (a stuck UI retrying Start, a script) must not fill the machine with
/// coding CLIs, so the coding-agent CLIs — `claude`, `codex`, `grok`,
/// `agy`, `opencode` — always carry a cap. Running several at once is
/// the normal shape of a day's work (one per worktree), so the default
/// is a handful, not one.
///
/// Programs that aren't in the map have no limit (bash, git, etc.).
/// Operators can override via `interactive_process.max_concurrent_per_program`
/// in `magician-config.yaml`; the HTTP surface reads that map per
/// request, so a config reload takes effect without a restart.
static PROGRAM_CONCURRENCY_LIMITS: RwLock<Option<HashMap<String, usize>>> = RwLock::new(None);

/// Default per-CLI cap when no operator map was installed.
pub const DEFAULT_CODING_CLI_CONCURRENCY: usize = 6;

pub fn default_program_concurrency_limits() -> HashMap<String, usize> {
    ["claude", "codex", "grok", "agy", "opencode"]
        .into_iter()
        .map(|program| (program.to_string(), DEFAULT_CODING_CLI_CONCURRENCY))
        .collect()
}

/// Install an operator-provided concurrency-limit map. Called at boot
/// from the orchestrator wiring after the magician config has been
/// parsed; a later call replaces the map.
pub fn set_program_concurrency_limits(limits: HashMap<String, usize>) {
    let mut guard = PROGRAM_CONCURRENCY_LIMITS
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(limits);
}

/// Resolve the concurrency limit for `program`. Returns `None` when
/// the program has no cap. Falls back to the defaults so callers that
/// boot without invoking `set_program_concurrency_limits` still get the
/// safe baseline.
pub fn program_concurrency_limit(program: &str) -> Option<usize> {
    let guard = PROGRAM_CONCURRENCY_LIMITS
        .read()
        .unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(map) => map.get(program).copied(),
        None => default_program_concurrency_limits().get(program).copied(),
    }
}

// ---------------------------------------------------------------------------
// Operator terminal environment
// ---------------------------------------------------------------------------

/// Keys the host process loaded from Magician's own runtime `.env` files.
/// They are the service's credentials (LLM keys, `GITHUB_TOKEN`, tunnel
/// secrets), not the operator's shell environment, so an operator terminal
/// must not inherit them: `claude` would bill `ANTHROPIC_API_KEY` instead of
/// the operator's subscription and `gh` would act as Magician's token.
static RUNTIME_INJECTED_ENV_KEYS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Record which environment keys the runtime `.env` loader injected.
/// Called once at boot, before any terminal starts.
pub fn set_runtime_injected_env_keys(keys: Vec<String>) {
    let mut guard = RUNTIME_INJECTED_ENV_KEYS
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *guard = keys;
}

/// Removed from every operator terminal even when the operator's own
/// environment carried them. A provider key in the environment takes
/// precedence over the CLI's own sign-in: `ANTHROPIC_API_KEY` silently moves
/// Claude Code from the subscription to API billing, and `XAI_API_KEY` /
/// `OPENAI_API_KEY` do the same to `grok` and `codex`. They are listed here,
/// not left to the `.env` scrub, because that scrub misses a key the service
/// inherited from the shell that launched it. The rest are build knobs the
/// `Makefile` exports to whatever it launches (the supervisor included), so a
/// terminal in a worktree would otherwise inherit the main checkout's
/// `CARGO_TARGET_DIR` and toolchain, and `MAKEFLAGS` would make every `make`
/// behave as a sub-make of a jobserver that no longer exists.
pub const ALWAYS_SCRUBBED_TERMINAL_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "XAI_API_KEY",
    "CARGO_BUILD_JOBS",
    "CARGO_TARGET_DIR",
    "MAGICIAN_FAST_APPLIED",
    "MAKEFLAGS",
    "MAKELEVEL",
    "MAKE_TERMERR",
    "MAKE_TERMOUT",
    "MFLAGS",
    "RUSTC_JOB_GATE_NEXT",
    "RUSTC_JOB_GATE_SLOTS",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "RUSTUP_TOOLCHAIN",
    "SCCACHE_IDLE_TIMEOUT",
];

/// Never removed, whatever a `.env` file happened to mention: a terminal
/// without these cannot find programs, its home, or its locale.
const NEVER_SCRUBBED_TERMINAL_ENV_KEYS: &[&str] = &[
    "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME", "PATH", "SHELL", "TMPDIR", "USER",
];

/// The keys an operator terminal must not inherit from the host process.
pub fn operator_terminal_env_scrub_list() -> Vec<String> {
    let injected = RUNTIME_INJECTED_ENV_KEYS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut keys: Vec<String> = ALWAYS_SCRUBBED_TERMINAL_ENV_KEYS
        .iter()
        .map(|key| key.to_string())
        .chain(injected)
        .filter(|key| !NEVER_SCRUBBED_TERMINAL_ENV_KEYS.contains(&key.as_str()))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

// ---------------------------------------------------------------------------
// Session host
// ---------------------------------------------------------------------------

/// What owns the child process.
///
/// `Direct` puts the program straight on the PTY: the process dies with
/// Magician. `Tmux` runs it inside a detached session on a private tmux
/// server and puts a tmux *client* on the PTY. When Magician exits only the
/// client dies; the program keeps running, and the next Magician process
/// re-attaches to it under the same session id (`adopt_tmux_sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionHost {
    Direct,
    Tmux(TmuxHost),
}

/// A private tmux server: its binary, socket name (`-L`), and the config
/// file it is started with. The socket is never the operator's own tmux
/// server, so their key bindings and sessions are untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxHost {
    pub binary: PathBuf,
    pub socket: String,
    pub config_path: PathBuf,
}

/// Server config for the private tmux socket. Written on every start and
/// sourced, so an already-running server picks up a changed file.
///
/// - No prefix and no status line: the terminal must behave like the bare
///   CLI. Claude Code binds Ctrl-B, which tmux would otherwise swallow.
/// - `escape-time 0`: Esc reaches the CLI immediately (it cancels a turn).
/// - The `smcup@:rmcup@` override keeps tmux out of the outer alternate
///   screen, so output scrolled off the top lands in the browser terminal's
///   own scrollback and the wheel keeps working.
/// - `window-size latest`: the most recently active viewer sets the size, so
///   a phone and a laptop can share one session.
pub const TMUX_SERVER_CONFIG: &str = "\
set-option -g prefix None
set-option -g prefix2 None
set-option -g status off
set-option -g history-limit 50000
set-option -g window-size latest
set-option -g allow-passthrough on
set-option -g set-titles off
set-option -s escape-time 0
set-option -s focus-events on
set-option -s set-clipboard on
set-option -s default-terminal tmux-256color
set-option -s terminal-overrides[90] \"xterm-256color:smcup@:rmcup@\"
set-option -s terminal-features[90] \"xterm-256color:RGB\"
";

const TMUX_SESSION_PREFIX: &str = "mgc-";
const TMUX_FIELD_SEPARATOR: char = '\t';

/// Where a tmux binary usually lives when the service's `PATH` is the
/// launchd minimum and does not include Homebrew.
const TMUX_FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

impl TmuxHost {
    /// Find `tmux` on `PATH` or in the usual install locations.
    pub fn locate(socket: impl Into<String>, config_path: PathBuf) -> Option<Self> {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
        dirs.extend(TMUX_FALLBACK_DIRS.iter().map(PathBuf::from));
        dirs.into_iter()
            .map(|dir| dir.join("tmux"))
            .find(|candidate| is_executable_file(candidate))
            .map(|binary| Self {
                binary,
                socket: socket.into(),
                config_path,
            })
    }

    /// A private socket name per data root, so two Magician instances on one
    /// machine never adopt each other's terminals.
    pub fn socket_for_root(root: &Path) -> String {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        // FNV-1a: stable across builds and platforms, unlike `DefaultHasher`.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in canonical.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("magician-wb-{:08x}", hash as u32)
    }

    fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.binary);
        // -u: UTF-8 regardless of the service's locale; without it a
        // launchd-started service renders every non-ASCII glyph as `_`.
        cmd.arg("-u").arg("-L").arg(&self.socket);
        cmd
    }

    fn command_builder(&self) -> CommandBuilder {
        let mut cmd = CommandBuilder::new(&self.binary);
        cmd.arg("-u");
        cmd.arg("-L");
        cmd.arg(&self.socket);
        cmd
    }

    fn write_config(&self) -> Result<(), String> {
        if std::fs::read_to_string(&self.config_path).ok().as_deref() == Some(TMUX_SERVER_CONFIG) {
            return Ok(());
        }
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("tmux config dir `{}`: {e}", parent.display()))?;
        }
        let tmp = self
            .config_path
            .with_extension(format!("tmp-{}", Uuid::new_v4()));
        std::fs::write(&tmp, TMUX_SERVER_CONFIG)
            .map_err(|e| format!("tmux config `{}`: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.config_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("tmux config `{}`: {e}", self.config_path.display())
        })
    }

    fn kill_session(&self, name: &str) {
        let _ = self
            .command()
            .arg("kill-session")
            .arg("-t")
            .arg(exact_target(name))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Where the operator CLIs install themselves, relative to `$HOME`. A
/// service started by launchd or the supervisor has a minimal `PATH` that
/// includes none of these, while the operator's login shell does — so a CLI
/// that runs in their terminal would be "not found" in the Workbench.
/// (`grok` installs to `~/.grok/bin`; the coding engine's discovery list
/// names the same places.)
const HOME_CLI_DIRS: &[&str] = &[".local/bin", ".grok/bin", ".claude/local", ".bun/bin"];

/// System install locations searched after `PATH`, for the same reason.
const SYSTEM_CLI_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Resolve `program` against the `PATH` the child will receive, then the
/// usual CLI install locations. A name with a slash is taken as given.
pub fn resolve_program_path(program: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_program_path_in(program, path_var, home.as_deref())
}

fn resolve_program_path_in(
    program: &str,
    path_var: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable_file(&path).then_some(path);
    }
    let path_var = path_var
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    std::env::split_paths(&path_var)
        .chain(
            home.into_iter()
                .flat_map(|home| HOME_CLI_DIRS.iter().map(move |dir| home.join(dir))),
        )
        .chain(SYSTEM_CLI_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable_file(candidate))
}

/// `=name` targets a session by exact name; a bare name would prefix-match.
fn exact_target(name: &str) -> String {
    format!("={name}")
}

/// The same exact match where tmux expects a pane target (`set-option -t`):
/// the trailing colon makes it session-plus-current-window.
fn exact_pane_target(name: &str) -> String {
    format!("={name}:")
}

/// Metadata stored on the tmux session as user options. Values are one
/// line: the list format separates fields with tabs and rows with newlines.
fn tmux_option_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// The tmux session a `Tmux`-hosted session lives in, kept so `close` can
/// end the program and not just this process's client.
#[derive(Debug, Clone)]
struct TmuxSessionRef {
    host: TmuxHost,
    name: String,
}

/// Count live sessions across every scope whose `program` matches.
/// Walks the global `SCOPED_SESSION_REGISTRY` because the limit is a
/// system-wide ceiling, not a per-scope one — two principals running
/// `claude` simultaneously should also be blocked even though their
/// registries are separate.
fn count_program_sessions_globally(program: &str) -> usize {
    let map_guard = registry_map().read().unwrap_or_else(|e| e.into_inner());
    let mut count = 0usize;
    for registry in map_guard.values() {
        let sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for session_arc in sessions.values() {
            let guard = session_arc.lock().unwrap_or_else(|e| e.into_inner());
            if guard.program == program && guard.alive.load(std::sync::atomic::Ordering::Acquire) {
                count += 1;
            }
        }
    }
    count
}

/// Maximum bytes returned per `read` op. Caps the chunk the agent
/// pulls into history so a chatty CLI can't blow up the prompt budget
/// in one turn. Truncation is reported in the response.
const MAX_READ_BYTES: usize = 64 * 1024;

/// Hard cap on the buffer kept per session. If the agent ignores a
/// session for too long, drop the oldest bytes rather than leak
/// memory. The buffer is FIFO from the agent's perspective; `read`
/// drains from the front.
const MAX_BUFFER_BYTES: usize = 1024 * 1024;

/// Bounded byte history kept only for UI replay. Unlike
/// `output_buffer`, this is never drained by agent `read` calls, so a
/// refreshed Developer-Mode workbench can hydrate the xterm screen
/// without stealing output from the agent.
const MAX_REPLAY_BYTES: usize = 4 * 1024 * 1024;

/// How long `start` waits before reporting "no output yet". Most CLIs
/// emit a banner immediately; if nothing has been received by this
/// deadline, the response returns an empty `output` and `still_starting: true`.
const INITIAL_READ_TIMEOUT: Duration = Duration::from_millis(750);

/// How long `close` waits for the child to exit after sending EOF
/// before SIGKILL.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Global per-(scope) session registry. Keyed by `(principal,
/// workspace)` so different workspaces can't see each other's
/// processes. Lazy-initialized.
static SCOPED_SESSION_REGISTRY: OnceLock<
    std::sync::RwLock<HashMap<(String, String), Arc<InteractiveSessionRegistry>>>,
> = OnceLock::new();

fn registry_map(
) -> &'static std::sync::RwLock<HashMap<(String, String), Arc<InteractiveSessionRegistry>>> {
    SCOPED_SESSION_REGISTRY.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// Resolve (and lazily create) the session registry for a scope.
pub fn registry_for_scope(principal: &str, workspace: &str) -> Arc<InteractiveSessionRegistry> {
    let key = (principal.to_string(), workspace.to_string());
    {
        let guard = registry_map().read().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&key) {
            return Arc::clone(existing);
        }
    }
    let mut guard = registry_map().write().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(InteractiveSessionRegistry::default())),
    )
}

/// Per-scope registry of live PTY sessions. The Arc keeps it shared
/// between the HTTP API and the inner-loop dispatch, the inner Mutex
/// guards concurrent op handling.
#[derive(Default)]
pub struct InteractiveSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<Mutex<InteractiveSession>>>>,
}

impl InteractiveSessionRegistry {
    pub fn get(&self, session_id: &str) -> Option<Arc<Mutex<InteractiveSession>>> {
        let guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(session_id).map(Arc::clone)
    }

    pub fn insert(&self, session_id: String, session: InteractiveSession) {
        let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(session_id, Arc::new(Mutex::new(session)));
    }

    pub fn remove(&self, session_id: &str) -> Option<Arc<Mutex<InteractiveSession>>> {
        let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(session_id)
    }

    pub fn live_ids(&self) -> Vec<String> {
        let guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.keys().cloned().collect()
    }
}

/// One live PTY-backed child process. The reader thread continuously
/// drains the master side into `output_buffer`; the agent's `read`
/// op slices off the front of the buffer.
pub struct InteractiveSession {
    pub id: String,
    /// PTY master kept alive so the tty stays open for the writer
    /// and reader threads. Dropped on close — that's the trigger
    /// that lets the child see EOF on its controlling terminal.
    /// Also the handle `resize_session` sets the window size on.
    master: Box<dyn MasterPty + Send>,
    /// Set when the program runs inside a private tmux session and the
    /// PTY child is only a tmux client (`SessionHost::Tmux`).
    tmux: Option<TmuxSessionRef>,
    /// Writer half (separate from the master so reads + writes don't
    /// fight over the master object).
    writer: Box<dyn std::io::Write + Send>,
    /// Buffered output accumulated by the reader thread. The reader
    /// pushes; `read` op drains.
    output_buffer: Arc<Mutex<Vec<u8>>>,
    /// Bounded, non-draining PTY byte history for UI reconnect.
    replay_buffer: Arc<Mutex<ReplayBuffer>>,
    /// Whether the child is still alive (set by the reader thread on EOF).
    alive: Arc<std::sync::atomic::AtomicBool>,
    /// Exit code observed once the child has exited; None until then.
    exit_code: Arc<Mutex<Option<i32>>>,
    /// Child handle kept so we can `kill` on close. Wrapped in Mutex
    /// because some platforms need &mut to query/kill.
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    /// When the session was last touched (start, read, or write). Used
    /// by future GC of stale sessions.
    pub last_active: Instant,
    /// Wall-clock creation time for UI diagnostics.
    pub created_at_ms: i64,
    /// Wall-clock time of the latest PTY output chunk.
    pub last_output_at_ms: Arc<std::sync::atomic::AtomicI64>,
    /// Wall-clock time of the latest stdin write.
    pub last_input_at_ms: Arc<std::sync::atomic::AtomicI64>,
    /// Program name for log/diagnostic purposes.
    pub program: String,
    /// UI thread that owns this PTY session when the session was
    /// launched from Developer Mode. None for older/autonomous callers
    /// that do not provide a thread context.
    pub ui_thread_id: Option<String>,
    /// Working directory the child was launched in. Captured so the
    /// Developer Mode diff endpoint can resolve `git diff HEAD --
    /// <file>` for files the agent has edited during the session.
    /// `None` when the caller didn't specify (defaults to magician's
    /// CWD, which isn't a useful diff target).
    pub working_dir: Option<std::path::PathBuf>,
}

impl InteractiveSession {
    /// Public snapshot of the bounded UI replay buffer. This buffer is
    /// separate from `output_buffer` and is never drained by agent reads.
    pub fn snapshot_replay_buffer(&self) -> InteractiveReplaySnapshot {
        self.replay_buffer
            .lock()
            .map(|g| g.snapshot())
            .unwrap_or_default()
    }

    /// Lightweight replay metadata for list endpoints. This avoids
    /// cloning the potentially multi-megabyte replay buffer when callers
    /// only need offset counters.
    pub fn replay_metadata(&self) -> InteractiveReplayMetadata {
        self.replay_buffer
            .lock()
            .map(|g| g.metadata())
            .unwrap_or_default()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// True when the program outlives this process (tmux-hosted).
    pub fn is_persistent(&self) -> bool {
        self.tmux.is_some()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.lock().map(|g| *g).unwrap_or(None)
    }

    pub fn last_output_at_ms(&self) -> Option<i64> {
        let value = self
            .last_output_at_ms
            .load(std::sync::atomic::Ordering::Acquire);
        (value > 0).then_some(value)
    }

    pub fn last_input_at_ms(&self) -> Option<i64> {
        let value = self
            .last_input_at_ms
            .load(std::sync::atomic::Ordering::Acquire);
        (value > 0).then_some(value)
    }
}

#[derive(Default)]
struct ReplayBuffer {
    bytes: Vec<u8>,
    start_offset: u64,
    end_offset: u64,
}

impl ReplayBuffer {
    fn append(&mut self, bytes: &[u8]) -> (u64, u64) {
        let offset_start = self.end_offset;
        self.bytes.extend_from_slice(bytes);
        self.end_offset = self.end_offset.saturating_add(bytes.len() as u64);
        if self.bytes.len() > MAX_REPLAY_BYTES {
            let drop_n = self.bytes.len() - MAX_REPLAY_BYTES;
            self.bytes.drain(0..drop_n);
            self.start_offset = self.start_offset.saturating_add(drop_n as u64);
        }
        (offset_start, self.end_offset)
    }

    fn snapshot(&self) -> InteractiveReplaySnapshot {
        InteractiveReplaySnapshot {
            bytes: self.bytes.clone(),
            start_offset: self.start_offset,
            end_offset: self.end_offset,
        }
    }

    fn metadata(&self) -> InteractiveReplayMetadata {
        InteractiveReplayMetadata {
            start_offset: self.start_offset,
            end_offset: self.end_offset,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InteractiveReplaySnapshot {
    pub bytes: Vec<u8>,
    pub start_offset: u64,
    pub end_offset: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InteractiveReplayMetadata {
    pub start_offset: u64,
    pub end_offset: u64,
}

#[derive(Debug, Clone)]
pub struct ClosedInteractiveSession {
    pub output: InteractiveReadOutput,
    pub program: String,
    pub replay_snapshot: InteractiveReplaySnapshot,
}

impl Drop for InteractiveSession {
    fn drop(&mut self) {
        // Best-effort cleanup. If the agent didn't call `close`, kill
        // the child rather than letting it leak past execution end.
        // For a tmux-hosted session the child is only the client: the
        // program survives in its tmux session on purpose, and the next
        // `adopt_tmux_sessions` re-attaches to it. `close` is what ends it.
        let mut child_guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child_guard.kill();
        // Master drops itself; the reader thread terminates on EOF
        // (which happens when the child dies and the master closes).
    }
}

/// Output envelope returned by `read`, `start`, and `write` ops.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InteractiveReadOutput {
    /// New output drained from the child since the last call. May be
    /// empty if no output is currently buffered (the CLI is waiting
    /// on input or just slow).
    pub output: String,
    /// Number of bytes truncated because the buffer exceeded `MAX_READ_BYTES`.
    pub truncated_bytes: usize,
    /// Whether the child process is still alive.
    pub alive: bool,
    /// Exit code if the child has already exited; None otherwise.
    pub exit_code: Option<i32>,
    /// Session ID (echoed back so the agent doesn't lose track).
    pub session_id: String,
}

/// Everything `start_session_with` needs to launch one session.
pub struct StartSpec<'a> {
    /// Executable to run. Invoked directly, not through a shell.
    pub program: &'a str,
    pub args: &'a [String],
    pub working_dir: Option<&'a Path>,
    /// Set on top of the inherited environment.
    pub env: &'a HashMap<String, String>,
    /// Removed from the inherited environment before `env` is applied.
    pub env_remove: &'a [String],
    pub rows: u16,
    pub cols: u16,
    pub host: &'a SessionHost,
    /// Live-session cap for `program`; `None` means no cap.
    pub concurrency_limit: Option<usize>,
    /// Owning `(principal, workspace)`. Recorded on a tmux session so only
    /// that scope re-attaches to it after a restart.
    pub scope: Option<(&'a str, &'a str)>,
}

/// Spawn a new PTY-backed process and return the freshly-created
/// session. The `program` is invoked directly (no shell), so the
/// caller is responsible for any quoting concerns. `args` are passed
/// verbatim. `env` overrides override the inherited environment.
///
/// After spawn we wait up to `INITIAL_READ_TIMEOUT` for the first
/// output burst — most CLIs print a banner / prompt immediately, so
/// returning empty here usually means the CLI is still booting (e.g.
/// loading a model). The agent should `read` again on the next
/// iteration in that case.
pub fn start_session(
    registry: &InteractiveSessionRegistry,
    program: &str,
    args: &[String],
    working_dir: Option<&std::path::Path>,
    env: &HashMap<String, String>,
    rows: u16,
    cols: u16,
    broadcast: Option<PtyBroadcastConfig>,
) -> Result<(String, InteractiveReadOutput), String> {
    start_session_with(
        registry,
        StartSpec {
            program,
            args,
            working_dir,
            env,
            env_remove: &[],
            rows,
            cols,
            host: &SessionHost::Direct,
            concurrency_limit: program_concurrency_limit(program),
            scope: None,
        },
        broadcast,
    )
}

/// `start_session` with the host, environment scrub, and cap chosen by
/// the caller. The Developer-Mode HTTP surface uses this.
pub fn start_session_with(
    registry: &InteractiveSessionRegistry,
    spec: StartSpec<'_>,
    broadcast: Option<PtyBroadcastConfig>,
) -> Result<(String, InteractiveReadOutput), String> {
    // Enforce the per-program global concurrency cap BEFORE we spawn
    // the child — once the PTY is open we'd have to tear it down to
    // back out, so it's cheaper to bounce early. Programs without a
    // configured limit pass through unrestricted (bash, git, etc.).
    if let Some(limit) = spec.concurrency_limit {
        let current = count_program_sessions_globally(spec.program);
        if current >= limit {
            return Err(format!(
                "concurrency limit reached for `{}` ({current}/{limit} live). \
                 Close an existing session before starting another, or raise \
                 `interactive_process.max_concurrent_per_program`.",
                spec.program
            ));
        }
    }

    let session_id = Uuid::new_v4().to_string();
    let created_at_ms = chrono::Utc::now().timestamp_millis();
    let (cmd, tmux) = match spec.host {
        SessionHost::Direct => {
            // Resolved here, not by the PTY library's `PATH` walk, so a CLI in
            // `~/.grok/bin` or `~/.local/bin` starts under a launchd `PATH`.
            // An unresolvable name is passed through and fails at spawn.
            let program =
                resolve_program_path(spec.program, spec.env.get("PATH").map(String::as_str))
                    .map(|path| path.into_os_string())
                    .unwrap_or_else(|| spec.program.into());
            let mut cmd = CommandBuilder::new(program);
            for arg in spec.args {
                cmd.arg(arg);
            }
            if let Some(dir) = spec.working_dir {
                cmd.cwd(dir);
            }
            for key in spec.env_remove {
                cmd.env_remove(key);
            }
            for (k, v) in spec.env {
                cmd.env(k, v);
            }
            (cmd, None)
        },
        SessionHost::Tmux(host) => {
            let name = format!("{TMUX_SESSION_PREFIX}{session_id}");
            create_tmux_session(host, &name, &session_id, created_at_ms, &spec, &broadcast)?;
            let term = spec
                .env
                .get("TERM")
                .map(String::as_str)
                .unwrap_or("xterm-256color");
            (
                tmux_attach_command(host, &name, term),
                Some(TmuxSessionRef {
                    host: host.clone(),
                    name,
                }),
            )
        },
    };

    let labels = SessionLabels {
        program: spec.program.to_string(),
        ui_thread_id: broadcast.as_ref().and_then(|cfg| cfg.ui_thread_id.clone()),
        working_dir: spec.working_dir.map(Path::to_path_buf),
        created_at_ms,
        replay_seed: Vec::new(),
    };
    let handles = match register_pty_session(
        registry,
        &session_id,
        cmd,
        spec.rows,
        spec.cols,
        labels,
        tmux.clone(),
        broadcast,
    ) {
        Ok(handles) => handles,
        Err(error) => {
            if let Some(tmux) = &tmux {
                tmux.host.kill_session(&tmux.name);
            }
            return Err(error);
        },
    };

    // Wait briefly for the initial output burst. Most CLIs emit a
    // banner / first prompt within a few hundred ms; an empty read
    // here is informational, not an error.
    let deadline = Instant::now() + INITIAL_READ_TIMEOUT;
    loop {
        if !handles
            .output_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        if !handles.alive.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let output = drain_buffer(&handles.output_buffer);
    let (drained, truncated) = trim_to_max(output);
    let exit_code_value = handles
        .exit_code
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok((
        session_id.clone(),
        InteractiveReadOutput {
            output: drained,
            truncated_bytes: truncated,
            alive: handles.alive.load(std::sync::atomic::Ordering::Acquire),
            exit_code: exit_code_value,
            session_id,
        },
    ))
}

/// Create the detached tmux session that will own the program, with its
/// metadata recorded as user options for `adopt_tmux_sessions`.
fn create_tmux_session(
    host: &TmuxHost,
    name: &str,
    session_id: &str,
    created_at_ms: i64,
    spec: &StartSpec<'_>,
    broadcast: &Option<PtyBroadcastConfig>,
) -> Result<(), String> {
    host.write_config()?;
    let program_path = resolve_program_path(spec.program, spec.env.get("PATH").map(String::as_str))
        .ok_or_else(|| format!("`{}` was not found on PATH", spec.program))?;

    let mut cmd = host.command();
    // The tmux server is forked from the first client, so its global
    // environment — what every program it runs inherits — is this
    // command's environment.
    for key in spec.env_remove {
        cmd.env_remove(key);
    }
    remove_tmux_nesting_env(&mut cmd);
    cmd.arg("-f").arg(&host.config_path);
    cmd.arg("source-file").arg(&host.config_path).arg(";");
    cmd.arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(name)
        .arg("-x")
        .arg(spec.cols.max(1).to_string())
        .arg("-y")
        .arg(spec.rows.max(1).to_string());
    if let Some(dir) = spec.working_dir {
        cmd.arg("-c").arg(dir);
    }
    let mut env: Vec<(&String, &String)> = spec
        .env
        .iter()
        // tmux sets TERM itself from `default-terminal`.
        .filter(|(key, _)| key.as_str() != "TERM")
        .collect();
    env.sort();
    for (key, value) in &env {
        cmd.arg("-e").arg(format!("{key}={value}"));
    }
    if !spec.env.contains_key("COLORTERM") {
        cmd.arg("-e").arg("COLORTERM=truecolor");
    }
    // Two or more words make tmux exec directly; a single word would go
    // through `default-shell -c`, which re-splits a path with spaces.
    cmd.arg("--").arg("/usr/bin/env").arg(&program_path);
    for arg in spec.args {
        cmd.arg(arg);
    }

    let (principal, workspace) = spec
        .scope
        .or_else(|| {
            broadcast
                .as_ref()
                .map(|cfg| (cfg.principal.as_str(), cfg.workspace.as_str()))
        })
        .unwrap_or(("", ""));
    let thread = broadcast
        .as_ref()
        .and_then(|cfg| cfg.ui_thread_id.as_deref())
        .unwrap_or("");
    let cwd = spec
        .working_dir
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let created = created_at_ms.to_string();
    for (option, value) in [
        ("@magician_session_id", session_id),
        ("@magician_principal", principal),
        ("@magician_workspace", workspace),
        ("@magician_program", spec.program),
        ("@magician_thread", thread),
        ("@magician_cwd", cwd.as_str()),
        ("@magician_created_ms", created.as_str()),
    ] {
        cmd.arg(";")
            .arg("set-option")
            .arg("-t")
            .arg(exact_pane_target(name))
            .arg(option)
            .arg(tmux_option_value(value));
    }

    let output = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("tmux new-session failed to run: {e}"))?;
    if !output.status.success() {
        host.kill_session(name);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "tmux new-session failed (exit {:?}): {} — `{}` may have exited immediately",
            output.status.code(),
            stderr.trim(),
            spec.program
        ));
    }
    Ok(())
}

/// Most history lines captured into a re-attached session's replay buffer.
const TMUX_HISTORY_SEED_LINES: u32 = 5_000;

/// A re-attached session's scrollback: the pane's history (colours kept,
/// wrapped lines joined), then a screen's worth of blank lines so the new
/// client's full-screen redraw lands below it and the history stays in the
/// browser terminal's scrollback instead of being painted over.
fn tmux_history_seed(host: &TmuxHost, name: &str) -> Vec<u8> {
    let mut capture = host.command();
    remove_tmux_nesting_env(&mut capture);
    let Ok(output) = capture
        .arg("capture-pane")
        .arg("-p")
        .arg("-e")
        .arg("-J")
        .arg("-S")
        .arg(format!("-{TMUX_HISTORY_SEED_LINES}"))
        .arg("-t")
        .arg(exact_pane_target(name))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() || output.stdout.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let history = text.trim_end_matches('\n');
    let mut seed = Vec::with_capacity(history.len() + 256);
    for line in history.split('\n') {
        seed.extend_from_slice(line.as_bytes());
        // A terminal needs CR+LF; reset attributes so a colour cannot bleed.
        seed.extend_from_slice(b"\x1b[0m\r\n");
    }
    seed.extend(std::iter::repeat(b"\r\n").take(200).flatten());
    // Keep the seed well inside the replay cap so live output is never the
    // part that gets evicted.
    let cap = MAX_REPLAY_BYTES / 2;
    if seed.len() > cap {
        seed.drain(..seed.len() - cap);
    }
    seed
}

/// The PTY child for a tmux-hosted session: a client attached to it.
fn tmux_attach_command(host: &TmuxHost, name: &str, term: &str) -> CommandBuilder {
    let mut cmd = host.command_builder();
    cmd.arg("attach-session");
    cmd.arg("-t");
    cmd.arg(exact_target(name));
    // tmux refuses to attach from inside another tmux session.
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
    cmd.env("TERM", term);
    cmd
}

fn remove_tmux_nesting_env(cmd: &mut std::process::Command) {
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
}

/// Re-attach to tmux-hosted sessions that belong to `(principal,
/// workspace)` and are not in `registry` — the sessions a previous
/// Magician process started. Each keeps its original session id, program,
/// thread, cwd and creation time, so the Workbench finds it where it was.
/// Returns the adopted ids. A tmux server that is not running yields none.
pub fn adopt_tmux_sessions(
    registry: &InteractiveSessionRegistry,
    host: &TmuxHost,
    principal: &str,
    workspace: &str,
    broadcast_for_thread: impl Fn(Option<String>) -> Option<PtyBroadcastConfig>,
) -> Vec<String> {
    static ADOPTION_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ADOPTION_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let format = [
        "#{session_name}",
        "#{@magician_session_id}",
        "#{@magician_principal}",
        "#{@magician_workspace}",
        "#{@magician_program}",
        "#{@magician_thread}",
        "#{@magician_cwd}",
        "#{@magician_created_ms}",
    ]
    .join(&TMUX_FIELD_SEPARATOR.to_string());
    let mut list = host.command();
    remove_tmux_nesting_env(&mut list);
    let Ok(output) = list
        .arg("list-sessions")
        .arg("-F")
        .arg(format)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut adopted = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields: Vec<&str> = line.split(TMUX_FIELD_SEPARATOR).collect();
        let [name, session_id, owner_principal, owner_workspace, program, thread, cwd, created] =
            fields.as_slice()
        else {
            continue;
        };
        if !name.starts_with(TMUX_SESSION_PREFIX)
            || session_id.is_empty()
            || *owner_principal != principal
            || *owner_workspace != workspace
            || registry.get(session_id).is_some()
        {
            continue;
        }
        let ui_thread_id = (!thread.is_empty()).then(|| thread.to_string());
        let labels = SessionLabels {
            program: program.to_string(),
            ui_thread_id: ui_thread_id.clone(),
            working_dir: (!cwd.is_empty()).then(|| PathBuf::from(cwd)),
            created_at_ms: created
                .parse()
                .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis()),
            replay_seed: tmux_history_seed(host, name),
        };
        let tmux = TmuxSessionRef {
            host: host.clone(),
            name: name.to_string(),
        };
        // The viewer re-sizes on mount; this is only the size until then.
        match register_pty_session(
            registry,
            session_id,
            tmux_attach_command(host, name, "xterm-256color"),
            40,
            140,
            labels,
            Some(tmux),
            broadcast_for_thread(ui_thread_id),
        ) {
            Ok(_) => adopted.push(session_id.to_string()),
            Err(error) => tracing::warn!(
                target: "magician::interactive_process",
                session_id = %session_id,
                error = %error,
                "tmux session adoption failed"
            ),
        }
    }
    adopted
}

/// Display metadata recorded on a session at spawn (or adoption).
struct SessionLabels {
    program: String,
    ui_thread_id: Option<String>,
    working_dir: Option<PathBuf>,
    created_at_ms: i64,
    /// Bytes placed in the replay buffer before the child's first output —
    /// a re-attached session's earlier history.
    replay_seed: Vec<u8>,
}

/// The shared state `start_session_with` waits on for the first burst.
struct SessionHandles {
    output_buffer: Arc<Mutex<Vec<u8>>>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
}

/// Open a PTY, spawn `cmd` on it, start the reader thread, and insert the
/// session into `registry` under `session_id`.
#[allow(clippy::too_many_arguments)]
fn register_pty_session(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
    cmd: CommandBuilder,
    rows: u16,
    cols: u16,
    labels: SessionLabels,
    tmux: Option<TmuxSessionRef>,
    broadcast: Option<PtyBroadcastConfig>,
) -> Result<SessionHandles, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn failed: {e}"))?;
    // Slave is closed by dropping `pair.slave`; the master holds the
    // controlling tty side. We hold the master for the lifetime of
    // the session.
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take writer failed: {e}"))?;

    let output_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let replay_buffer: Arc<Mutex<ReplayBuffer>> = Arc::new(Mutex::new(ReplayBuffer::default()));
    if !labels.replay_seed.is_empty() {
        replay_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .append(&labels.replay_seed);
    }
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let exit_code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let last_output_at_ms = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let last_input_at_ms = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let child_handle: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> =
        Arc::new(Mutex::new(child));

    // Background reader thread. Reads bytes until EOF (child closes
    // its tty) and pushes into the shared buffer. On EOF, flips
    // `alive=false` and tries to read the exit code.
    //
    // If `broadcast` is supplied, every chunk is also emitted via the
    // host's `PtyEventSink` so the Developer-Mode xterm.js pane (and
    // any other subscriber) sees PTY output in real time instead of
    // waiting for the agent's next `op=read`.
    {
        let buffer_clone = Arc::clone(&output_buffer);
        let replay_clone = Arc::clone(&replay_buffer);
        let alive_clone = Arc::clone(&alive);
        let exit_clone = Arc::clone(&exit_code);
        let last_output_clone = Arc::clone(&last_output_at_ms);
        let child_clone = Arc::clone(&child_handle);
        let broadcast_for_thread = broadcast.clone();
        let session_id_for_thread = session_id.to_string();
        let program_for_thread = labels.program.clone();
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break, // EOF — child closed the tty.
                    Ok(n) => {
                        let timestamp_ms = chrono::Utc::now().timestamp_millis();
                        last_output_clone.store(timestamp_ms, std::sync::atomic::Ordering::Release);
                        let (offset_start, offset_end) = {
                            let mut replay = replay_clone.lock().unwrap_or_else(|e| e.into_inner());
                            replay.append(&chunk[..n])
                        };
                        {
                            let mut buf = buffer_clone.lock().unwrap_or_else(|e| e.into_inner());
                            buf.extend_from_slice(&chunk[..n]);
                            // Bound the buffer: drop oldest if we exceed
                            // MAX_BUFFER_BYTES so a long-idle session
                            // can't OOM the process.
                            if buf.len() > MAX_BUFFER_BYTES {
                                let drop_n = buf.len() - MAX_BUFFER_BYTES;
                                buf.drain(0..drop_n);
                            }
                        }
                        // Fan out to the realtime event bus when wired.
                        // Bytes are base64-encoded so SSE/JSON callers
                        // can carry arbitrary binary (ANSI cursor
                        // escapes, alt-screen sequences, OSC queries).
                        if let Some(cfg) = broadcast_for_thread.as_ref() {
                            use base64::Engine;
                            let encoded =
                                base64::engine::general_purpose::STANDARD.encode(&chunk[..n]);
                            cfg.sink.emit_pty_chunk(PtyChunkEvent {
                                session_id: session_id_for_thread.clone(),
                                principal: cfg.principal.clone(),
                                workspace: cfg.workspace.clone(),
                                ui_thread_id: cfg.ui_thread_id.clone(),
                                program: Some(program_for_thread.clone()),
                                offset_start,
                                offset_end,
                                bytes_b64: encoded,
                                timestamp_ms,
                            });
                        }
                    },
                    Err(_) => break,
                }
            }
            alive_clone.store(false, std::sync::atomic::Ordering::Release);
            // Try to harvest the exit code. On Unix, `wait` returns
            // the status; we record it for the agent.
            let mut child_guard = child_clone.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(status) = child_guard.wait() {
                let code = status.exit_code() as i32;
                let mut slot = exit_clone.lock().unwrap_or_else(|e| e.into_inner());
                *slot = Some(code);
            }
        });
    }

    let session = InteractiveSession {
        id: session_id.to_string(),
        master: pair.master,
        tmux,
        writer,
        output_buffer: Arc::clone(&output_buffer),
        replay_buffer: Arc::clone(&replay_buffer),
        alive: Arc::clone(&alive),
        exit_code: Arc::clone(&exit_code),
        child: child_handle,
        last_active: Instant::now(),
        created_at_ms: labels.created_at_ms,
        last_output_at_ms: Arc::clone(&last_output_at_ms),
        last_input_at_ms: Arc::clone(&last_input_at_ms),
        program: labels.program,
        ui_thread_id: labels.ui_thread_id,
        working_dir: labels.working_dir,
    };

    registry.insert(session_id.to_string(), session);
    Ok(SessionHandles {
        output_buffer,
        alive,
        exit_code,
    })
}

/// Set the terminal size the child sees. The kernel delivers `SIGWINCH`,
/// so a full-screen CLI redraws at the new width; for a tmux-hosted
/// session the client relays it to the tmux window.
pub fn resize_session(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    if rows == 0 || cols == 0 {
        return Err(format!("invalid terminal size {cols}x{rows}"));
    }
    let session = registry
        .get(session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;
    let guard = session.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("resize failed: {e}"))
}

/// Drain currently-available output. Non-blocking — returns whatever
/// the reader thread has accumulated since the last drain. The
/// `wait_ms` parameter lets the caller park briefly if the buffer is
/// empty, to catch a prompt that's about to land.
pub fn read_session(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
    wait_ms: u64,
) -> Result<InteractiveReadOutput, String> {
    let session = registry
        .get(session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;

    // Hold the mutex while we sample the buffer; if empty, release
    // and sleep before retrying so other ops can fire. Bounded by
    // `wait_ms`, capped at 5 seconds so a stuck agent can't park
    // forever.
    let wait = Duration::from_millis(wait_ms.min(5_000));
    let deadline = Instant::now() + wait;

    let (output, alive_now, exit_code_value) = loop {
        let guard = session.lock().unwrap_or_else(|e| e.into_inner());
        let has_output = {
            let buf = guard
                .output_buffer
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            !buf.is_empty()
        };
        let alive_now = guard.alive.load(std::sync::atomic::Ordering::Acquire);
        let exit_code_value = guard
            .exit_code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if has_output || !alive_now || Instant::now() >= deadline {
            let drained = drain_buffer(&guard.output_buffer);
            break (drained, alive_now, exit_code_value);
        }
        drop(guard);
        thread::sleep(Duration::from_millis(25));
    };

    let (output, truncated) = trim_to_max(output);
    // Mark last_active.
    if let Some(s) = registry.get(session_id) {
        let mut guard = s.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_active = Instant::now();
    }
    Ok(InteractiveReadOutput {
        output,
        truncated_bytes: truncated,
        alive: alive_now,
        exit_code: exit_code_value,
        session_id: session_id.to_string(),
    })
}

/// Press a named key (or burst of the same key) and optionally read
/// the screen update afterwards. Maps the friendly key name to the
/// canonical ANSI/VT escape sequence, then writes it to stdin.
///
/// Used to drive single-select and multi-select TUI menus without the
/// agent having to remember byte sequences. For multi-select:
///   1. `press: down/up` to navigate the cursor.
///   2. `press: space` to toggle the highlighted option (TUI convention).
///   3. Repeat 1+2 for each option.
///   4. `press: enter` to confirm.
///
/// Returns a fresh read of the screen after the press if `read_after_ms > 0`,
/// so the agent sees the post-press highlight without a follow-up `read` op.
pub fn press_key(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
    key: &str,
    count: usize,
    read_after_ms: u64,
) -> Result<InteractiveReadOutput, String> {
    let sequence = key_to_sequence(key)?;
    let count = count.max(1).min(64); // sanity cap; nobody needs 65 ups
    let payload: String = sequence.repeat(count);
    write_session(registry, session_id, &payload)?;
    // Read whatever the CLI redrew. If `read_after_ms` is 0 we still
    // drain the buffer (zero-wait) so the next call doesn't return
    // stale output.
    read_session(registry, session_id, read_after_ms)
}

/// Map a friendly key name to its byte sequence. Covers the keys a
/// TUI agent realistically needs; the `bytes:<hex>` escape hatch lets
/// the agent send anything else (e.g. F-keys, custom shortcuts).
fn key_to_sequence(key: &str) -> Result<String, String> {
    let lower = key.trim().to_ascii_lowercase();
    Ok(match lower.as_str() {
        // Arrow keys — CSI sequences (ESC + `[`).
        "up" | "arrow_up" => "\x1b[A".to_string(),
        "down" | "arrow_down" => "\x1b[B".to_string(),
        "right" | "arrow_right" => "\x1b[C".to_string(),
        "left" | "arrow_left" => "\x1b[D".to_string(),
        // Confirmation / cancellation.
        "enter" | "return" => "\r".to_string(),
        "space" => " ".to_string(),
        "tab" => "\t".to_string(),
        "escape" | "esc" => "\x1b".to_string(),
        "backspace" => "\x7f".to_string(),
        // Common control combinations.
        "ctrl_c" => "\x03".to_string(),
        "ctrl_d" => "\x04".to_string(),
        "ctrl_l" => "\x0c".to_string(),
        // Page navigation (some TUIs use these for long lists).
        "page_up" | "pgup" => "\x1b[5~".to_string(),
        "page_down" | "pgdn" => "\x1b[6~".to_string(),
        "home" => "\x1b[H".to_string(),
        "end" => "\x1b[F".to_string(),
        // Escape hatch: `bytes:<hex>` lets the agent send arbitrary
        // raw bytes (e.g. `bytes:1b5b313b3275` for F-keys) without us
        // needing to enumerate every key.
        other if other.starts_with("bytes:") => {
            let hex = &other["bytes:".len()..];
            if hex.len() % 2 != 0 {
                return Err(format!(
                    "key `bytes:<hex>`: hex must be even-length, got {} chars",
                    hex.len()
                ));
            }
            let mut buf = Vec::with_capacity(hex.len() / 2);
            let mut iter = hex.chars();
            while let (Some(h1), Some(h2)) = (iter.next(), iter.next()) {
                let pair: String = [h1, h2].iter().collect();
                let byte = u8::from_str_radix(&pair, 16)
                    .map_err(|e| format!("key `bytes:<hex>`: invalid hex `{pair}`: {e}"))?;
                buf.push(byte);
            }
            // Hex payloads may not be valid UTF-8 (e.g. random binary
            // input); use from_utf8_lossy so we still pass the bytes
            // through. Most CLIs interpret raw bytes regardless.
            String::from_utf8_lossy(&buf).to_string()
        },
        other => {
            return Err(format!(
                "unknown key `{other}`. Supported: up/down/left/right/enter/space/tab/escape/\
                 backspace/ctrl_c/ctrl_d/ctrl_l/page_up/page_down/home/end, or `bytes:<hex>`."
            ));
        },
    })
}

/// Write a payload to the child's stdin. The caller is responsible
/// for line endings — pass `"y\n"` when answering a y/n prompt, pass
/// `"\u{1b}[B"` for arrow-down keystrokes, etc. After write returns,
/// the caller typically does a `read` to capture the response.
pub fn write_session(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
    input: &str,
) -> Result<(), String> {
    let session = registry
        .get(session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;
    let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.alive.load(std::sync::atomic::Ordering::Acquire) {
        return Err(format!("session {session_id} is no longer alive"));
    }
    guard
        .writer
        .write_all(input.as_bytes())
        .map_err(|e| format!("stdin write failed: {e}"))?;
    guard
        .writer
        .flush()
        .map_err(|e| format!("stdin flush failed: {e}"))?;
    guard.last_active = Instant::now();
    guard.last_input_at_ms.store(
        chrono::Utc::now().timestamp_millis(),
        std::sync::atomic::Ordering::Release,
    );
    Ok(())
}

/// Persist a session's replay output buffer to disk on close.
///
/// Phase 6.2 of Developer Mode — gives the thread a replayable
/// transcript of the CLI session that survives execution teardown.
/// The replay buffer is bounded; the file header records byte offsets
/// and `replay_truncated=true` when older output was already evicted.
/// Lives under `<base>/scopes/<principal>/<workspace>/interactive_sessions/<id>.log`
/// (the same scope layout the artifact graph uses) so a future
/// artifact-graph integration can promote it without moving the file.
///
/// Best-effort: failures are logged but never propagated since this
/// runs from the `close_session` path which mustn't fail just because
/// the disk wrote slowly.
pub fn persist_session_transcript(
    base_root: &std::path::Path,
    principal: &str,
    workspace: &str,
    session_id: &str,
    program: &str,
    replay_start_offset: u64,
    replay_end_offset: u64,
    output: &[u8],
) {
    if output.is_empty() {
        return;
    }
    let dir = base_root
        .join("scopes")
        .join(principal)
        .join(workspace)
        .join("interactive_sessions");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            target: "magician::interactive_process",
            principal = %principal,
            workspace = %workspace,
            session_id = %session_id,
            error = %error,
            "transcript persist skipped — mkdir failed"
        );
        return;
    }
    let path = dir.join(format!("{session_id}.log"));
    let replay_truncated = replay_start_offset > 0;
    let header = format!(
        "# session_id={session_id} program={program} closed_at={}\n# replay_start_offset={replay_start_offset} replay_end_offset={replay_end_offset} persisted_bytes={} replay_truncated={replay_truncated}\n",
        chrono::Utc::now().to_rfc3339(),
        output.len()
    );
    if let Err(error) = std::fs::write(&path, [header.as_bytes(), output].concat()) {
        tracing::warn!(
            target: "magician::interactive_process",
            path = %path.display(),
            error = %error,
            "transcript persist skipped — write failed"
        );
    }
}

/// Close the session: signal the child to terminate (writing nothing
/// — closing the master in the Drop impl handles tty teardown), wait
/// briefly for it to exit cleanly, then kill if still alive. Always
/// removes from the registry.
pub fn close_session_with_transcript(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
) -> Result<ClosedInteractiveSession, String> {
    let session = registry
        .remove(session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))?;

    // Try graceful exit: send terminal EOF before waiting. The old
    // implementation only waited and then relied on Drop to kill the
    // process, which made every close pay the full grace period for
    // normally-interactive programs.
    let deadline = Instant::now() + CLOSE_GRACE;
    let mut alive;
    let exit_code_value;
    let final_output;
    let program;
    let replay_snapshot;
    {
        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
        program = guard.program.clone();
        if let Some(tmux) = guard.tmux.clone() {
            // The PTY child is only a client: end the session that owns
            // the program, and the client exits with it. EOF would go to
            // the CLI as a keystroke instead.
            tmux.host.kill_session(&tmux.name);
        } else {
            let _ = guard.writer.write_all(b"\x04");
            let _ = guard.writer.flush();
        }
        while Instant::now() < deadline && guard.alive.load(std::sync::atomic::Ordering::Acquire) {
            thread::sleep(Duration::from_millis(25));
        }
        alive = guard.alive.load(std::sync::atomic::Ordering::Acquire);
        if alive {
            if let Ok(mut child) = guard.child.lock() {
                let _ = child.kill();
            }
            guard
                .alive
                .store(false, std::sync::atomic::Ordering::Release);
            alive = false;
        }
        exit_code_value = guard
            .exit_code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        final_output = drain_buffer(&guard.output_buffer);
        replay_snapshot = guard.snapshot_replay_buffer();
    }
    // Dropping the Arc<Mutex<InteractiveSession>> here triggers the
    // child kill via the Drop impl if it's still alive.
    drop(session);

    let (output, truncated) = trim_to_max(final_output);
    Ok(ClosedInteractiveSession {
        output: InteractiveReadOutput {
            output,
            truncated_bytes: truncated,
            alive,
            exit_code: exit_code_value,
            session_id: session_id.to_string(),
        },
        program,
        replay_snapshot,
    })
}

pub fn close_session(
    registry: &InteractiveSessionRegistry,
    session_id: &str,
) -> Result<InteractiveReadOutput, String> {
    close_session_with_transcript(registry, session_id).map(|closed| closed.output)
}

fn drain_buffer(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = std::mem::take(&mut *buf);
    // The PTY emits raw bytes; CLIs use UTF-8 but may include ANSI
    // control sequences. We surface them as-is so the agent can
    // see (and ignore) them; downstream prompt-parsing layers can
    // strip them if needed.
    String::from_utf8_lossy(&bytes).to_string()
}

fn trim_to_max(s: String) -> (String, usize) {
    if s.len() <= MAX_READ_BYTES {
        (s, 0)
    } else {
        // Keep the TAIL — the most recent output is what the agent
        // needs to react to a prompt; dropped bytes are reported.
        let extra = s.len() - MAX_READ_BYTES;
        let trimmed = s[extra..].to_string();
        (trimmed, extra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_read_write_close_roundtrip_echo() {
        let reg = InteractiveSessionRegistry::default();
        let (sid, first) = start_session(
            &reg,
            "/bin/sh",
            &["-c".to_string(), "cat".to_string()],
            None,
            &HashMap::new(),
            24,
            80,
            None,
        )
        .expect("spawn cat");

        assert!(first.alive);
        // Write some bytes; cat echoes them back.
        write_session(&reg, &sid, "hello\n").expect("write hello");
        // Give the child a moment to echo.
        let echo = read_session(&reg, &sid, 200).expect("read echo");
        assert!(
            echo.output.contains("hello"),
            "expected echo, got: {:?}",
            echo.output
        );

        let closed = close_session(&reg, &sid).expect("close");
        // After close, the session should not be in the registry.
        assert!(reg.get(&sid).is_none());
        // alive flag after drop is intentionally false.
        assert!(!closed.alive);
    }

    #[test]
    fn read_unknown_session_returns_error() {
        let reg = InteractiveSessionRegistry::default();
        let err = read_session(&reg, "no-such-id", 50).err().unwrap();
        assert!(err.contains("unknown session"), "{err}");
    }

    #[test]
    fn write_after_close_fails() {
        let reg = InteractiveSessionRegistry::default();
        let (sid, _) = start_session(
            &reg,
            "/bin/sh",
            &["-c".to_string(), "echo done".to_string()],
            None,
            &HashMap::new(),
            24,
            80,
            None,
        )
        .expect("spawn echo");
        let _ = close_session(&reg, &sid);
        // After close, the session is gone — write returns unknown.
        let err = write_session(&reg, &sid, "anything").err().unwrap();
        assert!(err.contains("unknown session"), "{err}");
    }

    #[test]
    fn key_to_sequence_known_keys() {
        assert_eq!(key_to_sequence("up").unwrap(), "\x1b[A");
        assert_eq!(key_to_sequence("DOWN").unwrap(), "\x1b[B");
        assert_eq!(key_to_sequence("arrow_right").unwrap(), "\x1b[C");
        assert_eq!(key_to_sequence("enter").unwrap(), "\r");
        assert_eq!(key_to_sequence("space").unwrap(), " ");
        assert_eq!(key_to_sequence("escape").unwrap(), "\x1b");
        assert_eq!(key_to_sequence("ctrl_c").unwrap(), "\x03");
        assert_eq!(key_to_sequence("page_down").unwrap(), "\x1b[6~");
    }

    #[test]
    fn key_to_sequence_bytes_hex_escape_hatch() {
        // ESC [ 1 ; 2 H = ANSI cursor position (1,2) — verify the
        // hex escape passes through correctly.
        assert_eq!(key_to_sequence("bytes:1b5b313b3248").unwrap(), "\x1b[1;2H");
        // Odd-length hex rejected with a clear error.
        assert!(key_to_sequence("bytes:1b5").is_err());
    }

    #[test]
    fn key_to_sequence_unknown_rejected() {
        let err = key_to_sequence("kaboom").err().unwrap();
        assert!(err.contains("unknown key"));
    }

    #[test]
    fn press_key_sends_arrow_burst_to_cat() {
        // Use `cat -v` (POSIX: visible escapes) to verify the byte
        // sequence reaches the child verbatim. `cat -v` prints
        // ESC as `^[` and other control chars in caret-notation, so
        // we can read them out of stdout.
        let reg = InteractiveSessionRegistry::default();
        let (sid, _first) = start_session(
            &reg,
            "/bin/sh",
            &["-c".to_string(), "cat -v".to_string()],
            None,
            &HashMap::new(),
            24,
            80,
            None,
        )
        .expect("spawn cat -v");

        // Press down 3 times + enter; cat -v should echo it back.
        let out = press_key(&reg, &sid, "down", 3, 300).expect("press down");
        // Each arrow-down is `^[[B` in caret notation; three of them
        // arrive together. Tolerant of how the tty echoes them.
        let echoed = &out.output;
        let down_caret_hits = echoed.matches("^[[B").count();
        assert!(
            down_caret_hits >= 1,
            "expected at least one `^[[B` in echoed output; got: {echoed:?}"
        );

        let _ = press_key(&reg, &sid, "enter", 1, 100);
        let _ = close_session(&reg, &sid);
    }

    #[test]
    fn program_concurrency_limit_blocks_second_session() {
        // Pick a program name that isn't claude/codex/agy/opencode
        // so we don't clobber the default policy for other tests
        // running in the same process.
        //
        // We can't reset `PROGRAM_CONCURRENCY_LIMITS` (OnceLock); we
        // verify the scoped-registry count helper directly instead.
        let workspace = format!("interactive-process-test-{}", Uuid::new_v4());
        let reg = registry_for_scope("test", &workspace);
        let (sid, _) = start_session(
            &reg,
            "/bin/sh",
            &["-c".to_string(), "sleep 30".to_string()],
            None,
            &HashMap::new(),
            24,
            80,
            None,
        )
        .expect("first spawn");

        // First session is alive — the global counter should see it.
        let count = count_program_sessions_globally("/bin/sh");
        assert!(count >= 1, "expected ≥1 live /bin/sh session, got {count}");

        // Cleanup.
        let _ = close_session(&reg, &sid);
    }

    #[test]
    fn default_concurrency_limits_cap_coding_clis_above_one() {
        // One session per CLI made parallel worktrees impossible; the cap is
        // a runaway guard, not a license meter.
        let defaults = default_program_concurrency_limits();
        for program in ["claude", "codex", "grok", "agy", "opencode"] {
            assert_eq!(
                defaults.get(program),
                Some(&DEFAULT_CODING_CLI_CONCURRENCY),
                "{program}"
            );
        }
        assert!(DEFAULT_CODING_CLI_CONCURRENCY > 1);
        assert_eq!(defaults.get("bash"), None);
    }

    #[test]
    fn spec_concurrency_limit_refuses_the_session_over_the_cap() {
        let workspace = format!("interactive-process-test-{}", Uuid::new_v4());
        let reg = registry_for_scope("test", &workspace);
        // A program name nothing else in this test binary runs, so the
        // process-wide count is exactly this test's sessions.
        let program = "/bin/sleep";
        let args = ["30".to_string()];
        let env = HashMap::new();
        let spec = |limit| StartSpec {
            program,
            args: &args,
            working_dir: None,
            env: &env,
            env_remove: &[],
            rows: 24,
            cols: 80,
            host: &SessionHost::Direct,
            concurrency_limit: Some(limit),
            scope: None,
        };
        let (first, _) = start_session_with(&reg, spec(1), None).expect("first under the cap");
        let err = start_session_with(&reg, spec(1), None)
            .err()
            .expect("second refused at cap 1");
        assert!(err.contains("concurrency limit reached"), "{err}");
        let (second, _) = start_session_with(&reg, spec(2), None).expect("cap 2 admits it");
        let _ = close_session(&reg, &first);
        let _ = close_session(&reg, &second);
    }

    #[test]
    fn operator_env_scrub_list_covers_injected_keys_but_never_path() {
        set_runtime_injected_env_keys(vec![
            "MAGICIAN_PTY_TEST_INJECTED".to_string(),
            "PATH".to_string(),
            "HOME".to_string(),
        ]);
        let keys = operator_terminal_env_scrub_list();
        assert!(keys.contains(&"MAGICIAN_PTY_TEST_INJECTED".to_string()));
        assert!(keys.contains(&"ANTHROPIC_API_KEY".to_string()));
        assert!(keys.contains(&"CARGO_TARGET_DIR".to_string()));
        assert!(keys.contains(&"MAKEFLAGS".to_string()));
        assert!(!keys.contains(&"PATH".to_string()));
        assert!(!keys.contains(&"HOME".to_string()));
    }

    #[test]
    fn resize_session_reaches_the_child_tty() {
        let reg = InteractiveSessionRegistry::default();
        let (sid, _) = start_session(
            &reg,
            "/bin/sh",
            &[
                "-c".to_string(),
                "sleep 1.5; stty size; sleep 5".to_string(),
            ],
            None,
            &HashMap::new(),
            24,
            80,
            None,
        )
        .expect("spawn sh");
        resize_session(&reg, &sid, 33, 101).expect("resize");
        let mut seen = String::new();
        for _ in 0..20 {
            seen.push_str(&read_session(&reg, &sid, 200).expect("read").output);
            if seen.contains("33 101") {
                break;
            }
        }
        assert!(
            seen.contains("33 101"),
            "child never saw the new size: {seen:?}"
        );
        assert!(resize_session(&reg, &sid, 0, 80).is_err());
        assert!(resize_session(&reg, "no-such-id", 24, 80).is_err());
        let _ = close_session(&reg, &sid);
    }

    #[test]
    fn provider_keys_are_scrubbed_even_from_the_launching_shell() {
        let keys = operator_terminal_env_scrub_list();
        for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "XAI_API_KEY"] {
            assert!(keys.contains(&key.to_string()), "{key}");
        }
    }

    #[test]
    fn a_cli_in_a_home_install_dir_resolves_without_it_on_path() {
        let home = std::env::temp_dir().join(format!("magician-pty-home-{}", Uuid::new_v4()));
        let bin = home.join(".grok/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let grok = bin.join("grok-test-cli");
        std::fs::write(&grok, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&grok, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            resolve_program_path_in("grok-test-cli", Some("/usr/bin:/bin"), Some(&home)),
            Some(grok.clone())
        );
        assert_eq!(
            resolve_program_path_in("grok-test-cli", Some("/usr/bin:/bin"), None),
            None
        );
        // PATH still wins over the fallback locations.
        assert_eq!(
            resolve_program_path_in("sh", Some("/bin"), Some(&home)),
            Some(PathBuf::from("/bin/sh"))
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tmux_option_values_are_single_line() {
        assert_eq!(tmux_option_value("a\tb\nc\rd"), "a b c d");
    }

    #[test]
    fn socket_for_root_is_stable_and_root_specific() {
        let a = TmuxHost::socket_for_root(Path::new("/tmp/magician-root-a"));
        assert_eq!(
            a,
            TmuxHost::socket_for_root(Path::new("/tmp/magician-root-a"))
        );
        assert_ne!(
            a,
            TmuxHost::socket_for_root(Path::new("/tmp/magician-root-b"))
        );
        assert!(a.starts_with("magician-wb-"));
    }

    mod tmux_host {
        use super::*;

        /// A private tmux server per test, torn down on drop.
        struct TestTmux {
            host: TmuxHost,
            _dir: PathBuf,
        }

        impl TestTmux {
            fn new() -> Option<Self> {
                let dir =
                    std::env::temp_dir().join(format!("magician-pty-tmux-{}", Uuid::new_v4()));
                let socket = format!(
                    "magician-wb-test-{}",
                    &Uuid::new_v4().simple().to_string()[..12]
                );
                let host = TmuxHost::locate(socket, dir.join("tmux.conf"))?;
                Some(Self { host, _dir: dir })
            }

            fn has_session(&self, session_id: &str) -> bool {
                self.host
                    .command()
                    .arg("has-session")
                    .arg("-t")
                    .arg(exact_target(&format!("{TMUX_SESSION_PREFIX}{session_id}")))
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            }
        }

        impl Drop for TestTmux {
            fn drop(&mut self) {
                let _ = self
                    .host
                    .command()
                    .arg("kill-server")
                    .stderr(std::process::Stdio::null())
                    .status();
                let _ = std::fs::remove_dir_all(&self._dir);
            }
        }

        fn read_until(reg: &InteractiveSessionRegistry, sid: &str, needle: &str) -> String {
            let mut seen = String::new();
            for _ in 0..40 {
                seen.push_str(&read_session(reg, sid, 100).expect("read").output);
                if seen.contains(needle) {
                    break;
                }
            }
            seen
        }

        fn start(
            reg: &InteractiveSessionRegistry,
            tmux: &TestTmux,
            program: &str,
            args: &[String],
            env_remove: &[String],
        ) -> Result<(String, String), String> {
            let host = SessionHost::Tmux(tmux.host.clone());
            start_session_with(
                reg,
                StartSpec {
                    program,
                    args,
                    working_dir: None,
                    env: &HashMap::new(),
                    env_remove,
                    rows: 24,
                    cols: 80,
                    host: &host,
                    concurrency_limit: None,
                    scope: Some(("p", "w")),
                },
                None,
            )
            .map(|(sid, first)| (sid, first.output))
        }

        #[test]
        fn a_tmux_session_outlives_its_client_and_is_adopted_by_its_scope_only() {
            let Some(tmux) = TestTmux::new() else {
                eprintln!("tmux not installed; skipping");
                return;
            };
            let reg = InteractiveSessionRegistry::default();
            let (sid, _) = start(&reg, &tmux, "cat", &[], &[]).expect("start under tmux");
            write_session(&reg, &sid, "hello-before\r").expect("write");
            let before = read_until(&reg, &sid, "hello-before");
            assert!(before.contains("hello-before"), "{before:?}");
            // Push the first line off the visible screen (24 rows), so only
            // the history seed can bring it back after the restart.
            for i in 0..40 {
                write_session(&reg, &sid, &format!("filler-{i}\r")).expect("write");
            }
            read_until(&reg, &sid, "filler-39");
            assert!(reg.get(&sid).unwrap().lock().unwrap().is_persistent());

            // What a Magician restart does to the session: this process's
            // client goes away, nothing calls close.
            drop(reg.remove(&sid));
            thread::sleep(Duration::from_millis(200));
            assert!(
                tmux.has_session(&sid),
                "the program must outlive its client"
            );

            let fresh = InteractiveSessionRegistry::default();
            let other_scope =
                adopt_tmux_sessions(&fresh, &tmux.host, "someone-else", "w", |_| None);
            assert!(other_scope.is_empty(), "another scope must not adopt it");

            let adopted = adopt_tmux_sessions(&fresh, &tmux.host, "p", "w", |_| None);
            assert_eq!(adopted, vec![sid.clone()]);
            {
                let session = fresh.get(&sid).expect("adopted under its old id");
                let guard = session.lock().unwrap();
                assert_eq!(guard.program, "cat");
                assert!(guard.is_alive());
                // Scrollback from before the restart is in the replay buffer.
                let replay =
                    String::from_utf8_lossy(&guard.snapshot_replay_buffer().bytes).into_owned();
                assert!(replay.contains("hello-before"), "history seed missing");
                assert!(replay.contains("filler-0"));
            }
            // Adopting twice is a no-op.
            assert!(adopt_tmux_sessions(&fresh, &tmux.host, "p", "w", |_| None).is_empty());

            write_session(&fresh, &sid, "hello-after\r").expect("write after adoption");
            let after = read_until(&fresh, &sid, "hello-after");
            assert!(after.contains("hello-after"), "{after:?}");

            let closed = close_session(&fresh, &sid).expect("close");
            assert!(!closed.alive);
            assert!(
                !tmux.has_session(&sid),
                "close must end the program, not just the client"
            );
        }

        #[test]
        fn a_tmux_session_does_not_inherit_scrubbed_keys() {
            let Some(tmux) = TestTmux::new() else {
                eprintln!("tmux not installed; skipping");
                return;
            };
            // Set on this process the way the `.env` loader sets it.
            std::env::set_var("MAGICIAN_PTY_TEST_SECRET", "leaked");
            let reg = InteractiveSessionRegistry::default();
            let (sid, first) = start(
                &reg,
                &tmux,
                "/bin/sh",
                &[
                    "-c".to_string(),
                    "echo secret=${MAGICIAN_PTY_TEST_SECRET:-scrubbed}; sleep 5".to_string(),
                ],
                &["MAGICIAN_PTY_TEST_SECRET".to_string()],
            )
            .expect("start");
            // The program prints before the client attaches; the attach
            // redraw carries it, usually inside the first burst.
            let seen = first + &read_until(&reg, &sid, "secret=");
            assert!(seen.contains("secret=scrubbed"), "{seen:?}");
            assert!(!seen.contains("leaked"), "{seen:?}");
            let _ = close_session(&reg, &sid);
        }

        #[test]
        fn a_missing_program_is_refused_before_any_session_exists() {
            let Some(tmux) = TestTmux::new() else {
                eprintln!("tmux not installed; skipping");
                return;
            };
            let reg = InteractiveSessionRegistry::default();
            let err = start(&reg, &tmux, "definitely-not-a-real-cli-xyz", &[], &[])
                .err()
                .expect("refused");
            assert!(err.contains("not found on PATH"), "{err}");
            assert!(reg.live_ids().is_empty());
        }
    }

    #[test]
    fn trim_to_max_keeps_tail() {
        let s = "a".repeat(MAX_READ_BYTES + 100);
        let (trimmed, dropped) = trim_to_max(s);
        assert_eq!(trimmed.len(), MAX_READ_BYTES);
        assert_eq!(dropped, 100);
    }

    /// Opt-in live smoke tests against the four real operator CLIs supported
    /// by Developer Mode. They require `claude`, `codex`, `agy`, and
    /// `opencode` on PATH, so the canonical provider-free suite discovers but
    /// does not execute them. Run `make test-pty-live` on a provisioned host.
    ///
    /// Each test allows for slow boots and treats "child alive after
    /// the wait window" as success too — so a fast banner OR a slow
    /// CLI still passes, while a missing-binary spawn fails fast on
    /// the `start_session` error.
    mod live {
        use super::*;

        /// Outcome of a smoke run: captured output plus whether the
        /// session was alive at the time we read. A non-empty banner
        /// is the strong signal; `alive_after_wait` is the weaker
        /// fallback that still proves the PTY handshake succeeded
        /// (some CLIs take 5+ seconds to print anything because they
        /// load models / fetch update manifests / negotiate auth).
        struct SmokeOutcome {
            output: String,
            alive_after_wait: bool,
        }

        /// Drive `program` through start → wait → read → close. Polls
        /// up to `total_wait_ms` for output to appear so slow-booting
        /// CLIs (agy, anything that fetches creds) don't false-fail.
        fn smoke(program: &str, args: &[&str], total_wait_ms: u64) -> Result<SmokeOutcome, String> {
            let reg = InteractiveSessionRegistry::default();
            let owned_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let (sid, first) = start_session(
                &reg,
                program,
                &owned_args,
                None,
                &HashMap::new(),
                40,
                120,
                None,
            )?;
            let mut combined = first.output.clone();
            // Poll in 500ms chunks until we either see output or run
            // out of budget. This keeps fast CLIs fast (test exits in
            // <2s) while letting slow ones have their full budget.
            let chunk_ms = 500u64;
            let mut elapsed_ms = 0u64;
            let mut alive = first.alive;
            while combined.trim().is_empty() && elapsed_ms < total_wait_ms {
                let chunk = read_session(&reg, &sid, chunk_ms)?;
                combined.push_str(&chunk.output);
                alive = chunk.alive;
                elapsed_ms += chunk_ms;
                if !alive {
                    break;
                }
            }
            let _ = close_session(&reg, &sid);
            Ok(SmokeOutcome {
                output: combined,
                alive_after_wait: alive,
            })
        }

        /// Assert the live PTY round-trip worked: either banner output
        /// arrived OR the child was still alive after the wait window
        /// (proves the PTY connected to a running process even if the
        /// CLI hasn't volunteered bytes yet).
        fn assert_pty_alive_or_banner(label: &str, outcome: &SmokeOutcome) {
            eprintln!("--- {label} banner ---\n{}\n---", outcome.output);
            assert!(
                !outcome.output.is_empty() || outcome.alive_after_wait,
                "expected {label} to emit a banner or stay alive over PTY; got nothing and exited"
            );
        }

        #[test]
        #[ignore = "requires the live claude CLI; run make test-pty-live"]
        fn claude_code_banner_arrives_over_pty() {
            let outcome = smoke("claude", &[], 5_000).expect("claude start");
            assert_pty_alive_or_banner("claude", &outcome);
        }

        #[test]
        #[ignore = "requires the live codex CLI; run make test-pty-live"]
        fn codex_banner_arrives_over_pty() {
            let outcome = smoke("codex", &[], 5_000).expect("codex start");
            assert_pty_alive_or_banner("codex", &outcome);
        }

        #[test]
        #[ignore = "requires the live agy CLI; run make test-pty-live"]
        fn agy_banner_arrives_over_pty() {
            // AntiGravity loads creds + checks for updates on launch; allow
            // up to 10s for the splash. The fallback alive-after-wait
            // assertion catches even slower boots.
            let outcome = smoke("agy", &[], 10_000).expect("agy start");
            assert_pty_alive_or_banner("agy", &outcome);
        }

        #[test]
        #[ignore = "requires the live opencode CLI; run make test-pty-live"]
        fn opencode_banner_arrives_over_pty() {
            let outcome = smoke("opencode", &[], 5_000).expect("opencode start");
            assert_pty_alive_or_banner("opencode", &outcome);
        }
    }
}
