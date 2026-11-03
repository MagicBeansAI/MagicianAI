//! The OS sandbox around every shell command outside a coding run
//! (`execution.shell_sandbox.os_enforce`). Design:
//! `docs/archive/plans/2026-10-13-hard-shell-sandbox-design.md`.
//!
//! The string checks in `native_executors` look at the working directory and a
//! fragment list; nothing inside a command. This box is what the kernel
//! enforces, however the command phrases itself:
//!
//! - writes only under the sandbox's working-dir roots, temp, `/dev` and tool
//!   caches;
//! - the agents' `file_sandbox.read_only_roots` neither writable nor a
//!   source of hard links (a link would give a program a writable second
//!   name), with a writable root inside one re-opened (deepest wins);
//! - secret stores unreadable (`~/.ssh`, `~/.aws`, keychains, the runtime
//!   root's `.env` files, the vault files, …);
//! - nested agent CLIs (`claude`, `codex`, `pi`, `grok`, `agy`, `gemini`) not
//!   executable: by name (also `.exe`/`.js`/`.mjs`/`.cjs`), by npm package
//!   wherever it is installed, and by their own install directories. A
//!   renamed agent binary downloaded in `public` mode is still a gap;
//! - network by `execution.network_policy`: `public` allows loopback directly
//!   (the owner's own test servers and dev databases; owner decision
//!   2026-10-14) except Magician's own API ([`register_magician_listener`]),
//!   and everything else only through the egress proxy, which refuses
//!   private, link-local and metadata addresses; `deny` allows none. Outside
//!   `allow`, unix-domain sockets are closed too (only those under the
//!   writable roots, syslog and mDNSResponder), so `ssh-agent`, `gpg-agent`
//!   and Docker sockets cannot stand in for the `~/.ssh` deny or the write
//!   fence; their env (`SSH_AUTH_SOCK`, `DOCKER_HOST`, …) is stripped.
//!
//! macOS `sandbox-exec` (SBPL; last match wins; paths canonicalized because
//! rules match resolved paths). Linux `bwrap`, best effort: `public` cannot
//! reach a host-side proxy from a new network namespace, so it is not enforced
//! there (nor the Magician API deny), unix sockets are not filtered (only
//! their env is stripped), and agent packages are masked only where a scan of
//! the global, nvm and `npx` installs finds them. God mode runs an approved
//! command outside the box.
//!
//! VibeDev coding engines (Claude Code, Codex, Pi, Grok, Antigravity) run
//! their whole process in the same box ([`wrap_harness`]): their shadow
//! workspace and own state directories writable, the engine itself exempt
//! from the nested-agent denies. Agent install directories, hook-bearing
//! settings, the engine's own file and package, and the real repo
//! ([`HarnessBox::deny_write`]) stay unwritable even under a writable root,
//! since they run unboxed later. A god-mode turn the owner approves runs
//! under the coding profile alone (`coding_engine::os_sandbox_wrap`).
//!
//! A coding run's own `shell` calls (the coding agent's, its harness turn's)
//! take the coding profile instead of this box, except under god mode: then
//! they run in this box like god-mode shell in any run, with the live repo
//! source and the run's real repo still unwritable ([`wrap_coding_call`]).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use runtime_core::ShellSandboxConfig;

use crate::config::NetworkMode;

/// Secret stores under the home directory a sandboxed shell may not read.
const HOME_SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gh",
    ".kube",
    ".docker",
    "Library/Keychains",
];
const HOME_SECRET_FILES: &[&str] = &[".netrc", ".git-credentials", ".pgpass"];

/// Tool caches a sandboxed shell may write, under the home directory.
const HOME_CACHE_DIRS: &[&str] = &[
    "Library/Caches",
    ".cache",
    ".npm",
    ".cargo/registry",
    ".cargo/git",
];

/// Agent CLIs a sandboxed shell may not start: a nested agent would carry its
/// own credentials and native tools past every Magician gate.
const NESTED_AGENT_NAMES: &[&str] = &["claude", "codex", "pi", "grok", "agy", "gemini"];

/// Agent CLIs' npm packages, denied exec and read wherever they are installed
/// (another nvm version, an `npx` copy under `~/.npm`), not only on `PATH`:
/// (agent name, package).
const NESTED_AGENT_PACKAGES: &[(&str, &str)] = &[
    ("claude", "@anthropic-ai/claude-code"),
    ("codex", "@openai/codex"),
    ("pi", "@earendil-works/pi-coding-agent"),
    ("gemini", "@google/gemini-cli"),
];

/// Agent CLIs' own install directories, denied exec and read: (agent name,
/// home-relative directory).
const NESTED_AGENT_HOME_DIRS: &[(&str, &str)] = &[
    ("claude", ".local/share/claude"),
    ("claude", ".claude/local"),
    ("grok", ".grok/bin"),
    ("grok", ".grok/downloads"),
];

/// What a boxed harness may never write, even under a writable state dir:
/// agent install directories and the settings files that name hooks. Each
/// runs unboxed the next time the owner (or an unboxed turn) starts that
/// agent. Home-relative.
const HARNESS_WRITE_DENIED_HOME: &[&str] = &[
    ".local/share/claude/versions",
    ".claude/local",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".grok/bin",
    ".grok/downloads",
];

/// Env naming a socket that holds a credential or drives the host
/// (`ssh-agent`, `gpg-agent`, Docker): stripped from a boxed process outside
/// `allow`. The socket deny is the fence (a path can be found by globbing);
/// stripping keeps tools from trying.
const SOCKET_ENV: &[&str] = &[
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
];

/// Unix sockets outside the writable roots a boxed process may still reach:
/// logging and name resolution.
#[cfg(any(target_os = "macos", test))]
const ALLOWED_SYSTEM_SOCKETS: &[&str] =
    &["/private/var/run/syslog", "/private/var/run/mDNSResponder"];

/// Magician's API port while startup has registered none.
const DEFAULT_MAGICIAN_API_PORT: u16 = 3002;

/// A loopback service a boxed process must never reach directly: each one
/// is a way out of the box (an unboxed Chrome to drive, a notes server that
/// serves every file, a supervisor that takes unauthenticated restart and
/// stop commands, a dev server proxying Magician's API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackService {
    /// Magician's own HTTP API (it would bypass the plane's gates).
    MagicianApi,
    /// magic-supervisor's control port.
    Supervisor,
    /// A Chrome DevTools Protocol endpoint (an unboxed browser).
    Cdp,
    /// A local notes server (the retired SilverBullet sidecar served the notes
    /// folder with an unauthenticated file and shell API). Nothing registers
    /// one now that Notes are served by Magician; kept so a stray listener a
    /// deployment still runs can be fenced explicitly.
    NotesServer,
    /// A web UI dev or preview server (proxies Magician's API).
    DevServer,
    /// Magicutor (its CDP proxy drives the owner's browser).
    Magicutor,
}

/// Denied before anything registers it: Chrome's usual CDP port, which the
/// owner's browser (driven through Magicutor) listens on. Every other
/// service is denied only once its port is registered, where Magician
/// learns it (startup, config, notes settings), so a project's own dev
/// server on a common port stays reachable.
const DEFAULT_LOOPBACK_SERVICES: &[(u16, LoopbackService)] = &[(9222, LoopbackService::Cdp)];

static MAGICIAN_LISTENERS: Mutex<Vec<(u16, LoopbackService)>> = Mutex::new(Vec::new());

/// The loopback port that serves only the plane's MCP route
/// (`magician-bin` startup): boxed harnesses reach Magician's tools there
/// with their grant, and nothing else of the API.
static PLANE_MCP_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Record the plane-only MCP port: every box allows it.
pub fn register_plane_mcp_port(port: u16) {
    PLANE_MCP_PORT.store(port, std::sync::atomic::Ordering::Release);
    // Only grant channels reach it (`plane::grant_channel`): every box
    // denies it, and a harness box allows its own channel port instead.
    register_loopback_service(port, LoopbackService::MagicianApi);
}

/// The plane-only MCP port, once startup registered one.
pub fn plane_mcp_port() -> Option<u16> {
    match PLANE_MCP_PORT.load(std::sync::atomic::Ordering::Acquire) {
        0 => None,
        port => Some(port),
    }
}

/// Record a loopback port Magician itself serves (its HTTP API): a boxed
/// process may not reach it directly, while other loopback stays open in
/// `public`. Call at startup once the port is known; until then the default
/// API port is denied.
pub fn register_magician_listener(port: u16) {
    register_loopback_service(port, LoopbackService::MagicianApi);
}

/// Record a loopback service no boxed process may reach (every network
/// mode, every agent), where its port is known: the supervisor's control
/// port, a configured CDP endpoint, the notes server.
pub fn register_loopback_service(port: u16, kind: LoopbackService) {
    if port == 0 {
        return;
    }
    let mut ports = MAGICIAN_LISTENERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !ports.contains(&(port, kind)) {
        ports.push((port, kind));
    }
}

/// Keep an ephemeral test listener from marking a pooled mock-server port as
/// protected after its test ends.
#[cfg(test)]
pub(crate) fn register_loopback_service_for_test(port: u16, kind: LoopbackService) -> impl Drop {
    struct Registration(u16, Vec<(u16, LoopbackService)>);
    impl Drop for Registration {
        fn drop(&mut self) {
            let mut ports = MAGICIAN_LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
            ports.retain(|(port, _)| *port != self.0);
            ports.extend_from_slice(&self.1);
        }
    }
    let mut ports = MAGICIAN_LISTENERS.lock().unwrap_or_else(|p| p.into_inner());
    let previous = ports
        .iter()
        .copied()
        .filter(|(candidate, _)| *candidate == port)
        .collect();
    ports.retain(|(candidate, _)| *candidate != port);
    ports.push((port, kind));
    Registration(port, previous)
}

/// [`register_loopback_service`] for a `http://host:port/…` URL, when its
/// host is loopback.
pub fn register_loopback_service_url(url: &str, kind: LoopbackService) {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return;
    };
    let loopback = match parsed.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => false,
    };
    if let (true, Some(port)) = (loopback, parsed.port_or_known_default()) {
        register_loopback_service(port, kind);
    }
}

/// The registered loopback services and the defaults, with their kinds.
pub(crate) fn loopback_services() -> Vec<(u16, LoopbackService)> {
    let mut services = MAGICIAN_LISTENERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if !services
        .iter()
        .any(|(_, kind)| *kind == LoopbackService::MagicianApi)
    {
        services.push((DEFAULT_MAGICIAN_API_PORT, LoopbackService::MagicianApi));
    }
    services.extend_from_slice(DEFAULT_LOOPBACK_SERVICES);
    services.sort_by_key(|(port, _)| *port);
    services.dedup_by_key(|(port, _)| *port);
    services
}

fn magician_listener_ports() -> Vec<u16> {
    loopback_services()
        .into_iter()
        .map(|(port, _)| port)
        .collect()
}

/// Agent packages installed off `PATH` (global prefixes, other nvm versions,
/// `npx` copies): Linux masks them by directory, having no path regex.
#[cfg(target_os = "linux")]
fn installed_agent_packages(home: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut roots = vec![
        PathBuf::from("/usr/lib/node_modules"),
        PathBuf::from("/usr/local/lib/node_modules"),
    ];
    if let Some(home) = home {
        for (parent, suffix) in [
            (home.join(".nvm/versions/node"), "lib/node_modules"),
            (home.join(".npm/_npx"), "node_modules"),
        ] {
            if let Ok(entries) = std::fs::read_dir(&parent) {
                roots.extend(entries.flatten().map(|entry| entry.path().join(suffix)));
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        for (name, package) in NESTED_AGENT_PACKAGES {
            let dir = root.join(package);
            if dir.is_dir() {
                found.push((*name, resolved(&dir)));
            }
        }
    }
    found
}

/// Each nested agent CLI on `PATH`, resolved: what to deny executing and
/// reading. A name rule is not enough: `claude` resolves to
/// `…/claude-code/bin/claude.exe`, and `codex`/`pi` are Node scripts whose
/// process is `node` (measured 2026-10-13). Denying reads of the package
/// directory stops the script from loading.
fn nested_agent_files() -> Vec<(PathBuf, PathBuf)> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for dir in std::env::split_paths(&path) {
        for name in NESTED_AGENT_NAMES {
            let candidate = dir.join(name);
            if !candidate.is_file() {
                continue;
            }
            let real = resolved(&candidate);
            found.push((real.clone(), package_root(&real).unwrap_or(real)));
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The npm package a file belongs to (`…/node_modules/<pkg>` or
/// `…/node_modules/@scope/<pkg>`), if it is in one.
fn package_root(file: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = file.components().collect();
    let at = parts
        .iter()
        .rposition(|part| part.as_os_str() == "node_modules")?;
    let first = parts.get(at + 1)?;
    let take = if first.as_os_str().to_string_lossy().starts_with('@') {
        at + 3
    } else {
        at + 2
    };
    (take <= parts.len()).then(|| parts[..take].iter().collect())
}

#[cfg(any(target_os = "macos", test))]
const VAULT_FILE_READ_DENIES: &str = concat!(
    "(deny file-read* (regex #\"/(provisioned_secrets|captured_secrets|mcp_oauth)\\.vault$\"))\n",
    "(deny file-read* (regex #\"/secret_audit\\.jsonl$\"))\n",
);

/// Whether the platform launcher works here. Probed once.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            return std::process::Command::new("/usr/bin/sandbox-exec")
                .args([
                    "-p",
                    "(version 1)(allow default)",
                    "/bin/sh",
                    "-c",
                    "exit 0",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
        }
        #[cfg(target_os = "linux")]
        {
            return which_bwrap().is_some_and(|bwrap| {
                std::process::Command::new(bwrap)
                    .args([
                        "--ro-bind",
                        "/",
                        "/",
                        "--dev",
                        "/dev",
                        "--die-with-parent",
                        "/bin/sh",
                        "-c",
                        "exit 0",
                    ])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            });
        }
        #[allow(unreachable_code)]
        false
    })
}

#[cfg(target_os = "linux")]
fn which_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
}

/// A command wrapped in the box: the program and arguments to spawn, env to
/// add, and env to remove. The launcher already strips `env_remove` inside
/// the box (`env -u` / `bwrap --unsetenv`), whatever env the caller sets;
/// a caller may remove it too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boxed {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
}

/// Wrap `program args` in the box for `sandbox`, or `None` when the box does
/// not apply: `os_enforce` off, a coding run (its own profile), or no working
/// launcher (logged once; the string checks still apply).
pub fn wrap(program: &OsStr, args: &[OsString], sandbox: &ShellSandboxConfig) -> Option<Boxed> {
    if crate::magician_v2::execution::coding_engine::coding_context_active() {
        return None;
    }
    launch(&boxed_plan(sandbox)?, program, args)
}

/// [`wrap`] for a coding run's shell call that god mode is on for
/// (`native_executors::authorize_bash_action`): this box, like god-mode shell
/// in any run, not the coding profile. What the coding profile kept read-only
/// stays unwritable even under a writable root (`allowed_working_dirs` lists
/// `.`, the live repo root, in the seed): the live Magician repo source
/// outside its scope sandbox base, and the coding run's real repo
/// ([`Plan::deny_repo_writes`]). The coding engines' own processes use
/// [`wrap_harness`]. `None` when `os_enforce` is off or no launcher works here.
pub fn wrap_coding_call(
    program: &OsStr,
    args: &[OsString],
    sandbox: &ShellSandboxConfig,
) -> Option<Boxed> {
    let mut plan = boxed_plan(sandbox)?;
    plan.deny_repo_writes(
        crate::magician_v2::execution::coding_engine::live_repo_source_fence(),
        crate::magician_v2::execution::coding_engine::coding_run_real_repo(),
    );
    launch(&plan, program, args)
}

/// The box's plan for `sandbox`, or `None` when `os_enforce` is off or no
/// launcher works here (logged once; the string checks still apply).
fn boxed_plan(sandbox: &ShellSandboxConfig) -> Option<Plan> {
    if !sandbox.os_enforce {
        return None;
    }
    if !available() {
        static WARNED: OnceLock<()> = OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!("[SHELL-BOX] shell_sandbox.os_enforce is on but no OS sandbox works here; shell runs with string checks only");
        });
        return None;
    }
    Some(Plan::current(sandbox))
}

/// A governed child always receives the ordinary process box, including when
/// its caller is a coding harness. Missing enforcement is a refusal at the
/// caller; this entry never inherits the harness exception or an unboxed fallback.
pub(crate) fn wrap_required(
    program: &OsStr,
    args: &[OsString],
    sandbox: &ShellSandboxConfig,
) -> Option<Boxed> {
    if !available() {
        return None;
    }
    launch(&Plan::current(sandbox), program, args)
}

/// `program args` under `plan` on this platform's launcher.
fn launch(plan: &Plan, program: &OsStr, args: &[OsString]) -> Option<Boxed> {
    #[cfg(target_os = "macos")]
    {
        let env_remove = plan.env_remove();
        let mut wrapped = vec![OsString::from("-p"), OsString::from(plan.macos_profile())];
        wrapped.extend(env_strip_prefix(&env_remove, program));
        wrapped.push(program.to_os_string());
        wrapped.extend(args.iter().cloned());
        return Some(Boxed {
            program: OsString::from("/usr/bin/sandbox-exec"),
            args: wrapped,
            env: plan.proxy_env(),
            env_remove,
        });
    }
    #[cfg(target_os = "linux")]
    {
        let bwrap = which_bwrap()?;
        let mut wrapped = plan.bwrap_args();
        wrapped.push(program.to_os_string());
        wrapped.extend(args.iter().cloned());
        return Some(Boxed {
            program: bwrap.into_os_string(),
            args: wrapped,
            env: plan.proxy_env(),
            env_remove: plan.env_remove(),
        });
    }
    #[allow(unreachable_code)]
    {
        let _ = (program, args, plan);
        None
    }
}

/// `/usr/bin/env -u NAME …` before the program: strips `names` inside the
/// box, whatever env the caller sets on the launcher. Empty when there is
/// nothing to strip, or for a program `env` would read as an assignment.
#[cfg(any(target_os = "macos", test))]
fn env_strip_prefix(names: &[String], program: &OsStr) -> Vec<OsString> {
    if names.is_empty() || program.to_string_lossy().contains('=') {
        return Vec::new();
    }
    let mut prefix = vec![OsString::from("/usr/bin/env")];
    for name in names {
        prefix.push(OsString::from("-u"));
        prefix.push(OsString::from(name.as_str()));
    }
    prefix
}

/// What a coding harness's box adds to the shell box
/// (`coding_engine::harness_command`).
#[derive(Debug, Clone, Default)]
pub struct HarnessBox {
    /// The engine being launched: exempt from the nested-agent denies (its
    /// file, its package, its name), or it could not start itself. Other
    /// agent CLIs stay denied. Its file and package are never writable.
    pub engine_binary: PathBuf,
    /// Writable roots: the shadow workspace, the engine's session dir, and the
    /// engine's own state directories (`~/.claude`, `~/.codex`, …).
    pub writable: Vec<PathBuf>,
    /// Writable files by SBPL regex (`~/.claude.json` and its backups).
    pub writable_regex: Vec<String>,
    /// Paths no write may touch even under a writable root (the real repo,
    /// which may sit under temp or a cache dir): emitted after every allow
    /// (SBPL last match wins; `--ro-bind` after the binds on Linux).
    pub deny_write: Vec<PathBuf>,
    /// The engine signs in through the login keychain (Claude Code's
    /// subscription token, Antigravity's keyring sign-in):
    /// `~/Library/Keychains` stays readable for it. The
    /// keychain file is encrypted and each item is guarded by securityd's
    /// own access control, so this exposes no other secret. Measured
    /// 2026-10-14: with the folder unreadable Claude answers "401 OAuth
    /// access token has expired" once its token needs a refresh.
    pub keychain_readable: bool,
    /// The agent the engine works for, from runtime identity: the goal
    /// workspaces it may not reach stay closed to the whole engine process
    /// (`goal_workspace_access`). `None` is no agent, a non-member of every
    /// workspace.
    pub acting_agent_id: Option<String>,
}

/// Wrap a VibeDev coding engine's whole process in the box: writes only under
/// `harness.writable`, temp and tool caches (so the real repo, outside the
/// shadow, stays read-only), never to `harness.deny_write`, agent install
/// dirs or the engine's own file; secrets unreadable; other agent CLIs
/// blocked; network by the policy. Everything the engine runs (its shell, its
/// tests) inherits the box. `None` when no launcher works here.
pub fn wrap_harness(program: &OsStr, args: &[OsString], harness: &HarnessBox) -> Option<Boxed> {
    if !available() {
        return None;
    }
    let sandbox = ShellSandboxConfig {
        allowed_working_dirs: harness
            .writable
            .iter()
            .map(|dir| dir.display().to_string())
            .collect(),
        os_enforce: true,
        ..ShellSandboxConfig::default()
    };
    let mut plan = Plan::for_agent(&sandbox, harness.acting_agent_id.as_deref());
    plan.harness = true;
    plan.writable.extend(build_cache_dirs());
    plan.writable.sort();
    plan.writable.dedup();
    plan.writable_regex = harness.writable_regex.clone();
    plan.write_denied = harness_write_denied(&harness.engine_binary, &harness.deny_write);
    plan.exempt_agent(&harness.engine_binary);
    if harness.keychain_readable {
        plan.secret_dirs
            .retain(|dir| !dir.ends_with("Library/Keychains"));
    }
    launch(&plan, program, args)
}

/// How a harness process that runs outside the coding box is fenced from
/// the goal workspaces its agent may not reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceFence {
    /// The agent is fenced from nothing (the admin, or no workspace exists).
    Unfenced,
    /// Launch this instead.
    Boxed(Boxed),
    /// No launcher works here: the harness may not start (fail closed).
    Refused(String),
}

/// Only the goal workspace, notes-index and loopback-service rules, over
/// `(allow default)` (bwrap: the whole tree as is): for a harness that
/// otherwise runs unboxed (super god mode, a plane harness turn) and must
/// still not reach a workspace its agent may not. `None` when `denies`
/// guard nothing or no launcher works.
pub fn wrap_workspace_only(
    program: &OsStr,
    args: &[OsString],
    denies: &crate::magician_v2::execution::goal_workspace_access::WorkspaceDenies,
    harness: bool,
) -> Option<Boxed> {
    wrap_workspace_and_read_only(program, args, denies, harness, &[])
}

/// Super god mode retains broad access while original-repository writes
/// still go through the exact-diff broker.
pub fn wrap_workspace_and_read_only(
    program: &OsStr,
    args: &[OsString],
    denies: &crate::magician_v2::execution::goal_workspace_access::WorkspaceDenies,
    harness: bool,
    read_only: &[PathBuf],
) -> Option<Boxed> {
    if (!denies.guards_workspaces && read_only.is_empty()) || !available() {
        return None;
    }
    let plan = workspace_only_plan(denies, harness);
    #[cfg(target_os = "macos")]
    {
        let mut wrapped = vec![
            OsString::from("-p"),
            OsString::from(format!(
                "{}{}",
                workspace_only_profile(&plan),
                read_only
                    .iter()
                    .map(|path| format!("(deny file-write* (subpath \"{}\"))\n", sbpl_escape(path)))
                    .collect::<String>()
            )),
        ];
        wrapped.push(program.to_os_string());
        wrapped.extend(args.iter().cloned());
        return Some(Boxed {
            program: OsString::from("/usr/bin/sandbox-exec"),
            args: wrapped,
            env: plan.proxy_env(),
            env_remove: plan.env_remove(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        let bwrap = which_bwrap()?;
        let mut wrapped: Vec<OsString> = ["--bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
            .into_iter()
            .map(OsString::from)
            .collect();
        for dir in plan.hybrid_index_dirs.iter().filter(|dir| dir.is_dir()) {
            wrapped.extend([OsString::from("--tmpfs"), dir.clone().into_os_string()]);
        }
        wrapped.extend(crate::magician_v2::execution::goal_workspace_access::bwrap_args(denies));
        for path in read_only {
            wrapped.extend([
                OsString::from("--ro-bind"),
                path.clone().into_os_string(),
                path.clone().into_os_string(),
            ]);
        }
        // Other harness sessions' private files and homes, and their
        // processes (`/proc/<pid>/environ` holds a grant): a new PID
        // namespace mounts a `/proc` of this box alone.
        wrapped.extend(plan.plane_private_mounts());
        wrapped.extend(workspace_only_pid_namespace());
        for name in plan.env_remove() {
            wrapped.extend([OsString::from("--unsetenv"), OsString::from(name)]);
        }
        // bwrap cannot filter by port or reach the host's proxy from a new
        // network namespace: a fenced process that is not a harness gets no
        // network; a harness keeps its own (it needs its model's API).
        if !harness {
            wrapped.push(OsString::from("--unshare-net"));
        }
        wrapped.push(OsString::from("--die-with-parent"));
        wrapped.push(program.to_os_string());
        wrapped.extend(args.iter().cloned());
        return Some(Boxed {
            program: bwrap.into_os_string(),
            args: wrapped,
            env: plan.proxy_env(),
            env_remove: plan.env_remove(),
        });
    }
    #[allow(unreachable_code)]
    {
        let _ = (program, args, plan);
        None
    }
}

/// `--unshare-pid` for a workspace-only box: its `/proc` (mounted with
/// `--proc`) then shows only its own processes, never another session's
/// environment.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn workspace_only_pid_namespace() -> [OsString; 1] {
    [OsString::from("--unshare-pid")]
}

/// The plan whose network rules (and proxy env) a workspace-only box uses:
/// the same as the shell box's for a fenced agent, so the LAN address,
/// unix sockets and every local service are closed there too.
fn workspace_only_plan(
    denies: &crate::magician_v2::execution::goal_workspace_access::WorkspaceDenies,
    harness: bool,
) -> Plan {
    let mut plan = Plan::with_denies(&ShellSandboxConfig::default(), denies.clone());
    plan.harness = harness;
    plan
}

/// The SBPL profile of [`wrap_workspace_only`]: everything allowed but the
/// goal workspace, notes-index and network rules.
#[cfg(any(target_os = "macos", test))]
fn workspace_only_profile(plan: &Plan) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n");
    profile.push_str(&plan.plane_private_rules());
    profile.push_str(crate::magician_v2::execution::goal_workspace_access::HYBRID_INDEX_SBPL_RULE);
    profile.push_str(
        &crate::magician_v2::execution::goal_workspace_access::sbpl_rules(&plan.workspace),
    );
    profile.push_str(&plan.network_rules());
    profile
}

/// Whether a fenced harness would run without network isolation from other
/// sessions' grant channels (Linux; overridable in tests).
pub(crate) fn linux_harness_isolation_missing() -> bool {
    #[cfg(test)]
    if let Some(forced) = FORCE_LINUX_HARNESS_REFUSAL.with(std::cell::Cell::get) {
        return forced;
    }
    cfg!(target_os = "linux")
}

#[cfg(test)]
thread_local! {
    static FORCE_LINUX_HARNESS_REFUSAL: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Fence a plane harness turn's process for the current acting agent. An
/// engine that applies its own OS sandbox (`self_sandboxing`: Codex, Grok,
/// Antigravity) cannot nest one inside Magician's on macOS, so for an agent
/// that is fenced it runs in Magician's whole box instead (writes only under
/// `writable`, temp and caches) and its caller drops its own sandbox flag;
/// any other engine gets [`wrap_workspace_only`].
pub fn fence_plane_harness(
    program: &OsStr,
    args: &[OsString],
    self_sandboxing: bool,
    writable: Vec<PathBuf>,
) -> WorkspaceFence {
    let agent = crate::magician_v2::execution::native_executors::current_acting_agent();
    let denies =
        crate::magician_v2::execution::goal_workspace_access::denies_for_acting(agent.as_deref());
    if !denies.guards_workspaces {
        return WorkspaceFence::Unfenced;
    }
    let refused = || {
        WorkspaceFence::Refused(
            "This harness may not start here for this agent: goal workspaces need an OS sandbox \
             (sandbox-exec or bwrap) to keep them from agents that are not members."
                .to_string(),
        )
    };
    // On Linux a harness keeps the host network (bwrap cannot filter
    // ports), so it could reach another session's grant channel: fail
    // closed until a private network namespace (pasta/slirp4netns forwarding
    // only its own channel and the model API) lifts this.
    if linux_harness_isolation_missing() {
        return WorkspaceFence::Refused(
            "This harness may not start for an agent fenced from a goal workspace on Linux: its \
             network cannot yet be held to its own plane channel. Run it as Magican, or on macOS."
                .to_string(),
        );
    }
    let boxed = if self_sandboxing {
        wrap_harness(
            program,
            args,
            &HarnessBox {
                engine_binary: PathBuf::from(program),
                writable,
                acting_agent_id: agent,
                ..HarnessBox::default()
            },
        )
    } else {
        wrap_workspace_only(program, args, &denies, true)
    };
    boxed.map_or_else(refused, WorkspaceFence::Boxed)
}

/// What a harness may not write under any writable root: `deny_write`, agent
/// install dirs and hook-bearing settings ([`HARNESS_WRITE_DENIED_HOME`]),
/// and the engine's own file and package. Resolved.
fn harness_write_denied(engine: &Path, deny_write: &[PathBuf]) -> Vec<PathBuf> {
    let mut denied: Vec<PathBuf> = deny_write.iter().map(|path| resolved(path)).collect();
    if let Some(home) = dirs::home_dir().map(|home| resolved(&home)) {
        denied.extend(HARNESS_WRITE_DENIED_HOME.iter().map(|path| home.join(path)));
    }
    let file = resolved(&locate(engine));
    if file.is_absolute() {
        denied.extend(package_root(&file));
        denied.push(file);
    }
    denied.sort();
    denied.dedup();
    denied
}

/// The temp dirs a child may write: this process's (`TMPDIR`) and the OS
/// per-user one (`/var/folders/…/T`). A child falls back to the second when
/// its engine resets `TMPDIR` (Codex's shells do), and Magician's own may sit
/// elsewhere (a `TMPDIR` set by the owner's setup), so opening only one broke
/// `mktemp` in the box. Resolved.
/// Plane harness sessions' private files and homes, denied to a box that
/// is no harness session (a VibeDev coding engine).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn plane_private_rules_outside_sessions() -> String {
    // As a harness box of a session that owns nothing: every session's
    // files and homes denied, the Pi store (which a coding Pi may use) left
    // alone.
    #[cfg(any(target_os = "macos", test))]
    let rules = {
        let mut plan = workspace_only_plan(&Default::default(), true);
        plan.plane_session = Some(
            crate::magician_v2::execution::plane::grant_channel::PlaneSession {
                id: format!("none-{}", uuid::Uuid::new_v4().simple()),
                port: 0,
                own_paths: Vec::new(),
            },
        );
        plan.plane_private_rules()
    };
    #[cfg(not(any(target_os = "macos", test)))]
    let rules = String::new();
    rules
}

/// The folders [`plane_private_rules_outside_sessions`] covers, for bwrap
/// (hidden whole).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn plane_private_paths_outside_sessions() -> Vec<PathBuf> {
    let (homes, _) = plane_private_roots();
    [
        resolved(&crate::magician_v2::execution::plane::grant_channel::private_root()),
        homes,
    ]
    .into_iter()
    .filter(|path| path.is_dir())
    .collect()
}

/// The plane's native homes root and its shared Pi session store.
fn plane_private_roots() -> (PathBuf, PathBuf) {
    (
        resolved(&crate::magician_v2::execution::plane::chat_turn::native_homes_root_path()),
        resolved(&crate::magician_v2::execution::plane::engines::pi::agentic_sessions_root_path()),
    )
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![resolved(&std::env::temp_dir())];
    if let Some(user_temp) = darwin_user_temp_dir().map(|dir| resolved(&dir)) {
        if !dirs.contains(&user_temp) {
            dirs.push(user_temp);
        }
    }
    dirs
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: the per-user temp dir macOS gives a
/// process whose `TMPDIR` is unset.
#[cfg(target_os = "macos")]
fn darwin_user_temp_dir() -> Option<PathBuf> {
    darwin_user_dir(libc::_CS_DARWIN_USER_TEMP_DIR)
}

/// `confstr(_CS_DARWIN_USER_CACHE_DIR)`: the per-user cache dir
/// (`/var/folders/…/C`), whatever `TMPDIR` says.
#[cfg(target_os = "macos")]
fn darwin_user_cache_dir() -> Option<PathBuf> {
    darwin_user_dir(libc::_CS_DARWIN_USER_CACHE_DIR)
}

/// A per-user dir `confstr` names, when it is a directory.
#[cfg(target_os = "macos")]
fn darwin_user_dir(name: libc::c_int) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most
    // that many, NUL-terminated, and returns the length it needed.
    let needed = unsafe { libc::confstr(name, buf.as_mut_ptr().cast(), buf.len()) };
    if needed == 0 || needed > buf.len() {
        return None;
    }
    buf.truncate(needed - 1);
    let path = PathBuf::from(std::ffi::OsString::from_vec(buf));
    path.is_dir().then_some(path)
}

#[cfg(not(target_os = "macos"))]
fn darwin_user_temp_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "macos"))]
fn darwin_user_cache_dir() -> Option<PathBuf> {
    None
}

/// The system's root certificate bundle (macOS and most Linux distributions).
const SYSTEM_CA_BUNDLE: &str = "/etc/ssl/cert.pem";

/// Build-tool caches a coding engine's tests and builds write, beyond
/// [`HOME_CACHE_DIRS`]: download and package stores only, never a directory
/// whose executables run later (`~/.cargo/bin`, rustup toolchains, shims,
/// pnpm or yarn bins), and the per-user cache directory next to the temp dir
/// (`/var/folders/…/C`).
fn build_cache_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir().map(|home| resolved(&home)) {
        for dir in [
            ".rustup/downloads",
            ".rustup/tmp",
            ".rustup/update-hashes",
            "go/pkg/mod",
            ".gradle/caches",
            ".gradle/wrapper/dists",
            ".m2/repository",
            ".bun/install/cache",
            ".yarn/berry/cache",
            ".pnpm-store",
            "Library/pnpm/store",
            ".local/share/pnpm/store",
            ".pytest_cache",
        ] {
            dirs.push(home.join(dir));
        }
    }
    for temp in temp_dirs() {
        if temp.file_name().is_some_and(|name| name == "T") {
            if let Some(parent) = temp.parent() {
                dirs.push(parent.join("C"));
            }
        }
    }
    dirs
}

/// clang's (and so Swift's) module cache under the per-user cache dir
/// (`/var/folders/…/C/clang/ModuleCache`, resolved to `/private/var` for
/// SBPL): the one part of that dir a task shell may write, so `swift` and
/// `clang -fmodules` work there. The rest of `…/C` holds other apps' caches
/// and stays read-only. `None` off macOS.
fn compiler_module_cache_dir() -> Option<PathBuf> {
    darwin_user_cache_dir().map(|cache| resolved(&cache).join("clang/ModuleCache"))
}

/// What the box allows, resolved for this host.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    writable: Vec<PathBuf>,
    secret_dirs: Vec<PathBuf>,
    secret_files: Vec<PathBuf>,
    network: NetworkMode,
    proxy_port: Option<u16>,
    /// Nested agent CLIs: (executable, file or package directory to make
    /// unreadable).
    nested_agents: Vec<(PathBuf, PathBuf)>,
    /// Agent CLI names whose exec is denied by name.
    nested_agent_names: Vec<&'static str>,
    /// Agent npm packages denied exec and read by path, anywhere: (agent
    /// name, package).
    nested_agent_packages: Vec<(&'static str, &'static str)>,
    /// Agent install directories denied exec and read: (agent name,
    /// directory).
    nested_agent_dirs: Vec<(&'static str, PathBuf)>,
    /// Writable files by SBPL regex (a harness's own state files).
    writable_regex: Vec<String>,
    /// Directories the box may create, the entry itself and not its
    /// contents: the parent of a writable root that may not exist yet
    /// (`…/C/clang` above the compiler module cache). macOS only; bwrap binds
    /// only writable roots that exist.
    creatable_dirs: Vec<PathBuf>,
    /// Never writable, even under a writable root: emitted after every allow.
    write_denied: Vec<PathBuf>,
    /// Carve-outs inside a `write_denied` path that keep their verdict: the
    /// scope sandbox base inside the live repo, where a writable root reaches
    /// it ([`Plan::deny_repo_writes`]). Re-allowed after the denials, with
    /// the read-only layering and the denials inside them applied again.
    write_reopened: Vec<PathBuf>,
    /// Folders agents may only read (`file_sandbox.read_only_roots`): no
    /// writes and no hard links out of them, so `ln programs/plan.md
    /// Workspaces/plan.md` cannot give a program a writable second name. As
    /// in the file sandbox, the deepest root decides: a writable root inside
    /// one stays writable.
    read_only: Vec<PathBuf>,
    /// Loopback services (Magician's API, the supervisor, CDP, the notes
    /// server, dev servers): never reachable directly, in any mode.
    magician_ports: Vec<u16>,
    /// The acting agent is fenced from a goal workspace: loopback is closed
    /// to it but for `loopback_allowed` (`network_policy.allow_private`).
    fenced: bool,
    /// A coding or plane harness process (it needs its model's API): when
    /// fenced, direct internet stays open in `allow`; a shell, a CLI skill or
    /// grep goes through the strict proxy instead.
    harness: bool,
    /// The egress proxy's strict listener, for a fenced agent.
    strict_proxy_port: Option<u16>,
    /// The current harness session's own grant channel
    /// (`plane::grant_channel`): the one plane port this box may reach.
    plane_port: Option<u16>,
    /// The harness session this box is built for: its own private temp
    /// files and native home stay readable; every other session's are not.
    plane_session: Option<crate::magician_v2::execution::plane::grant_channel::PlaneSession>,
    /// Loopback ports a fenced agent may still reach directly; `None` in the
    /// list stands for every port (`allow_private: [localhost]`).
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    loopback_allowed: Vec<Option<u16>>,
    /// Goal workspaces the acting agent may not reach, and the entries no
    /// rename may move (`goal_workspace_access`). After every allow.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", test)),
        allow(dead_code)
    )]
    workspace: crate::magician_v2::execution::goal_workspace_access::WorkspaceDenies,
    /// The scopes' notes hybrid index folders, hidden on Linux (macOS denies
    /// them by name).
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    hybrid_index_dirs: Vec<PathBuf>,
}

/// The loopback entries of `network_policy.allow_private`: a port, or
/// `None` for a bare loopback host (every port).
fn loopback_allow_list() -> Vec<Option<u16>> {
    let mut allowed = Vec::new();
    for entry in crate::magician_v2::execution::network_policy::settings().allow_private {
        let entry = entry.trim().to_ascii_lowercase();
        let (host, port) = match entry.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() => {
                (host.to_string(), port.parse().ok())
            },
            _ => (entry.clone(), None),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if matches!(host, "localhost" | "127.0.0.1" | "::1") {
            allowed.push(port);
        }
    }
    allowed.sort();
    allowed.dedup();
    allowed
}

fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The layering that makes `read_only` roots hold in an OS profile, shared
/// by the shell box and the coding sandbox: every read-only root, and every
/// `writable` root inside one (re-opened), shallowest first and read-only
/// after writable on a tie, so the deepest root wins (SBPL last match, a
/// later bwrap mount). `true` marks a read-only root. A writable entry may
/// end in `*` (a prefix in the coding allow-list); the path before the `*`
/// decides where it sits.
#[cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "linux",
        test,
        feature = "test-fixtures"
    )),
    allow(dead_code)
)]
pub(crate) fn read_only_layers(
    read_only: &[PathBuf],
    writable: &[PathBuf],
) -> Vec<(PathBuf, bool)> {
    fn folder(path: &Path) -> PathBuf {
        let text = path.to_string_lossy();
        match text.strip_suffix('*') {
            Some(prefix) => PathBuf::from(prefix),
            None => path.to_path_buf(),
        }
    }
    let mut layers: Vec<(PathBuf, bool)> =
        read_only.iter().map(|root| (root.clone(), true)).collect();
    layers.extend(
        writable
            .iter()
            .filter(|root| read_only.iter().any(|ro| folder(root).starts_with(ro)))
            .map(|root| (root.clone(), false)),
    );
    layers.sort_by_key(|(root, read_only)| (folder(root).components().count(), *read_only));
    layers.dedup();
    layers
}

/// The part of `layers` that applies inside `region`, to emit again after a
/// rule that re-opened all of `region` (the coding profile's live-fence
/// carve-out of its storage base): first `region` itself as read-only when
/// the deepest layer containing it is read-only, then every layer strictly
/// inside. A writable verdict for `region` is never emitted: it adds nothing
/// without a write allow-list, and with one it would open the whole base
/// (every scope, the vault files) when a writable root above it sits inside
/// a read-only root. Empty when no layer touches `region`.
#[cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "linux",
        test,
        feature = "test-fixtures"
    )),
    allow(dead_code)
)]
pub(crate) fn read_only_layers_within(
    layers: &[(PathBuf, bool)],
    region: &Path,
) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    if let Some((_, true)) = layers
        .iter()
        .rev()
        .find(|(root, _)| region.starts_with(root))
    {
        out.push((region.to_path_buf(), true));
    }
    out.extend(
        layers
            .iter()
            .filter(|(root, _)| root.starts_with(region) && root.as_path() != region)
            .cloned(),
    );
    out
}

/// `engine` as a file: a bare name is looked up on `PATH`.
fn locate(engine: &Path) -> PathBuf {
    if engine.components().count() != 1 {
        return engine.to_path_buf();
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(engine))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| engine.to_path_buf())
}

#[cfg(any(target_os = "macos", test))]
fn sbpl_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

impl Plan {
    fn current(sandbox: &ShellSandboxConfig) -> Self {
        let agent = crate::magician_v2::execution::native_executors::current_acting_agent();
        Self::for_agent(sandbox, agent.as_deref())
    }

    /// [`Plan::current`] for `agent` (`None`: no agent).
    fn for_agent(sandbox: &ShellSandboxConfig, agent: Option<&str>) -> Self {
        let mut plan = Self::with_denies(
            sandbox,
            crate::magician_v2::execution::goal_workspace_access::denies_for_acting(agent),
        );
        // The admin (and the owner's own VibeDev session, which acts as it)
        // reaches a project's own dev server; every other agent does not.
        let admin = crate::magician_v2::execution::goal_workspace_access::admin_agent();
        if agent == Some(admin.as_str()) {
            let dev_servers: Vec<u16> = loopback_services()
                .into_iter()
                .filter(|(_, kind)| *kind == LoopbackService::DevServer)
                .map(|(port, _)| port)
                .collect();
            plan.magician_ports
                .retain(|port| !dev_servers.contains(port));
        }
        plan
    }

    /// The plan with goal workspace denies the caller already computed.
    fn with_denies(
        sandbox: &ShellSandboxConfig,
        workspace: crate::magician_v2::execution::goal_workspace_access::WorkspaceDenies,
    ) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().map(|home| resolved(&home));
        // Resolved like the read-only roots (`native_executors`), so a
        // working dir that does not exist yet under a symlinked root still
        // sits where `starts_with` and the rules expect.
        let mut writable: Vec<PathBuf> = sandbox
            .allowed_working_dirs
            .iter()
            .map(|dir| {
                let path = PathBuf::from(dir);
                crate::magician_v2::execution::native_executors::resolve_for_policy(&if path
                    .is_absolute()
                {
                    path
                } else {
                    cwd.join(path)
                })
            })
            .collect();
        writable.push(resolved(Path::new("/tmp")));
        writable.extend(temp_dirs());
        let module_cache = compiler_module_cache_dir();
        writable.extend(module_cache.clone());
        if let Some(home) = &home {
            writable.extend(HOME_CACHE_DIRS.iter().map(|dir| home.join(dir)));
        }
        writable.sort();
        writable.dedup();

        let mut secret_dirs = Vec::new();
        let mut secret_files = Vec::new();
        if let Some(home) = &home {
            secret_dirs.extend(HOME_SECRET_DIRS.iter().map(|dir| home.join(dir)));
            secret_files.extend(HOME_SECRET_FILES.iter().map(|file| home.join(file)));
        }
        let runtime_root = resolved(&crate::magician_v2::process_storage::runtime_root());
        secret_files.push(runtime_root.join(".env"));
        secret_files.push(runtime_root.join(".env.development"));
        secret_files.push(runtime_root.join("goal_workspace_reads.json"));

        let network = crate::magician_v2::execution::network_policy::mode();
        let proxy_port = match network {
            NetworkMode::Public => {
                let port = crate::magician_v2::execution::egress_proxy::port();
                if port.is_none() {
                    tracing::warn!(
                        "[SHELL-BOX] the egress proxy is not running; shell network is denied"
                    );
                }
                port
            },
            _ => None,
        };
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut nested_agent_dirs: Vec<(&'static str, PathBuf)> = home
            .iter()
            .flat_map(|home| {
                NESTED_AGENT_HOME_DIRS
                    .iter()
                    .map(move |(name, dir)| (*name, home.join(dir)))
            })
            .collect();
        #[cfg(target_os = "linux")]
        nested_agent_dirs.extend(installed_agent_packages(home.as_deref()));
        Self {
            writable,
            secret_dirs,
            secret_files,
            network,
            proxy_port,
            nested_agents: nested_agent_files(),
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs,
            writable_regex: Vec::new(),
            creatable_dirs: module_cache
                .iter()
                .filter_map(|dir| dir.parent().map(Path::to_path_buf))
                .collect(),
            write_denied: Vec::new(),
            write_reopened: Vec::new(),
            read_only: crate::magician_v2::execution::native_executors::agent_read_only_roots(),
            magician_ports: magician_listener_ports(),
            harness: false,
            strict_proxy_port: if workspace.guards_workspaces && network != NetworkMode::Deny {
                crate::magician_v2::execution::egress_proxy::strict_port()
            } else {
                None
            },
            plane_port: crate::magician_v2::execution::plane::grant_channel::current_plane_session(
            )
            .map(|session| session.port),
            plane_session:
                crate::magician_v2::execution::plane::grant_channel::current_plane_session(),
            fenced: workspace.guards_workspaces,
            loopback_allowed: loopback_allow_list(),
            workspace,
            hybrid_index_dirs: if cfg!(target_os = "linux") {
                crate::magician_v2::execution::goal_workspace_access::hybrid_index_dirs(
                    &runtime_root,
                )
            } else {
                Vec::new()
            },
        }
    }

    /// [`read_only_layers`] for this box's read-only and writable roots. Only
    /// `writable` is re-opened inside a read-only root, never
    /// `writable_regex` (a harness's state files), on purpose.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    fn read_only_layers(&self) -> Vec<(PathBuf, bool)> {
        read_only_layers(&self.read_only, &self.writable)
    }

    /// Env stripped from a boxed process: the credential and host-control
    /// sockets, outside `allow` (where their sockets are closed).
    fn env_remove(&self) -> Vec<String> {
        if self.network == NetworkMode::Allow && !self.fenced {
            return Vec::new();
        }
        SOCKET_ENV.iter().map(|name| name.to_string()).collect()
    }

    /// Keep what the coding profile holds read-only unwritable in this box,
    /// even under a writable root: the live repo source (`live`: the repo
    /// and its scope sandbox base) and the coding run's real repo (`real`).
    /// Where a writable root reaches the sandbox base inside the live repo,
    /// that part stays writable ([`Plan::write_reopened`]).
    fn deny_repo_writes(&mut self, live: Option<(PathBuf, PathBuf)>, real: Option<PathBuf>) {
        if let Some((repo, base)) = live {
            let (repo, base) = (resolved(&repo), resolved(&base));
            for root in &self.writable {
                if root.starts_with(&base) {
                    self.write_reopened.push(root.clone());
                } else if base.starts_with(root) {
                    self.write_reopened.push(base.clone());
                }
            }
            self.write_denied.push(repo);
        }
        self.write_denied.extend(real.map(|real| resolved(&real)));
        for paths in [&mut self.write_denied, &mut self.write_reopened] {
            paths.sort();
            paths.dedup();
        }
    }

    /// The write denials inside a carve-out, applied again after it.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    fn write_denied_within_reopened(&self) -> Vec<&PathBuf> {
        self.write_denied
            .iter()
            .filter(|denied| {
                self.write_reopened
                    .iter()
                    .any(|reopened| denied.starts_with(reopened))
            })
            .collect()
    }

    /// Let `engine` (the harness being launched) run: drop it from the
    /// nested-agent denies by file, package, name and install directory.
    fn exempt_agent(&mut self, engine: &Path) {
        let file = resolved(&locate(engine));
        let package = package_root(&file);
        self.nested_agents.retain(|(executable, unreadable)| {
            *executable != file
                && Some(unreadable) != package.as_ref()
                && !package
                    .as_ref()
                    .is_some_and(|package| executable.starts_with(package))
        });
        // The engine inside an agent's package or install directory is that
        // agent (`~/.local/share/claude/versions/<v>` is `claude`).
        let file_text = file.to_string_lossy().into_owned();
        let mut own: Vec<&'static str> = NESTED_AGENT_PACKAGES
            .iter()
            .filter(|(_, package)| file_text.contains(&format!("/node_modules/{package}/")))
            .map(|(name, _)| *name)
            .collect();
        own.extend(
            self.nested_agent_dirs
                .iter()
                .filter(|(_, dir)| file.starts_with(dir))
                .map(|(name, _)| *name),
        );
        self.nested_agent_names.retain(|name| !own.contains(name));
        let names: Vec<String> = [engine.file_name(), file.file_name()]
            .into_iter()
            .flatten()
            .map(|name| {
                name.to_string_lossy()
                    .trim_end_matches(".exe")
                    .trim_end_matches(".mjs")
                    .trim_end_matches(".cjs")
                    .trim_end_matches(".js")
                    .to_string()
            })
            .collect();
        self.nested_agent_names
            .retain(|name| !names.iter().any(|own| own == name));
        // A name on PATH that resolves into the engine's file or package is the
        // engine under another name.
        self.nested_agent_names.retain(|name| {
            !std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|dir| {
                    let candidate = dir.join(name);
                    candidate.is_file() && {
                        let real = resolved(&candidate);
                        real == file
                            || package
                                .as_ref()
                                .is_some_and(|package| real.starts_with(package))
                    }
                })
            })
        });
        // Only the exempt agent's own packages and install directories open.
        let exempt: Vec<&'static str> = NESTED_AGENT_NAMES
            .iter()
            .copied()
            .filter(|name| !self.nested_agent_names.contains(name))
            .collect();
        self.nested_agent_packages
            .retain(|(name, _)| !exempt.contains(name));
        self.nested_agent_dirs
            .retain(|(name, _)| !exempt.contains(name));
    }

    /// Env pointing a public-mode child at the egress proxy.
    fn proxy_env(&self) -> Vec<(String, String)> {
        let port = if self.fenced {
            // A fenced agent's traffic goes through the strict listener,
            // but a harness's in `allow` (direct, loopback closed).
            if self.network == NetworkMode::Deny
                || (self.harness && self.network == NetworkMode::Allow)
            {
                None
            } else {
                self.strict_proxy_port
            }
        } else {
            self.proxy_port
                .filter(|_| self.network == NetworkMode::Public)
        };
        let Some(port) = port else {
            return Vec::new();
        };
        let url = crate::magician_v2::execution::egress_proxy::proxy_url(port);
        let mut env = Vec::new();
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            env.push((name.to_string(), url.clone()));
        }
        // Loopback goes direct (the profile allows it: the owner's own test
        // servers and dev databases); everything else goes through the proxy,
        // which refuses private, link-local and metadata addresses.
        let local = "localhost,127.0.0.1,::1";
        env.push(("NO_PROXY".to_string(), local.to_string()));
        env.push(("no_proxy".to_string(), local.to_string()));
        // pip verifies with the system trust store, except through a proxy,
        // where it falls back to Python's own CA file: empty on a python.org
        // install, so a proxied `pip install` failed with
        // CERTIFICATE_VERIFY_FAILED where an unboxed one worked. Give it the
        // owner's choice, else the system root bundle.
        let pip_cert = std::env::var("PIP_CERT")
            .ok()
            .filter(|cert| !cert.trim().is_empty())
            .unwrap_or_else(|| SYSTEM_CA_BUNDLE.to_string());
        if Path::new(&pip_cert).is_file() {
            env.push(("PIP_CERT".to_string(), pip_cert));
        }
        env
    }

    #[cfg(any(target_os = "macos", test))]
    fn macos_profile(&self) -> String {
        let escape = sbpl_escape;
        let mut profile =
            String::from("(version 1)\n(allow default)\n(deny file-write* (subpath \"/\"))\n");
        profile.push_str("(allow file-write* (subpath \"/dev\"))\n");
        for root in &self.writable {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                escape(root)
            ));
        }
        for regex in &self.writable_regex {
            profile.push_str(&format!("(allow file-write* (regex #\"{regex}\"))\n"));
        }
        // Creating the directory only: no writes inside it (beyond the
        // writable roots), no chmod, no rename.
        for dir in &self.creatable_dirs {
            profile.push_str(&format!(
                "(allow file-write-create (literal \"{}\"))\n",
                escape(dir)
            ));
        }
        for (root, read_only) in self.read_only_layers() {
            let verdict = if read_only { "deny" } else { "allow" };
            profile.push_str(&format!(
                "({verdict} file-write* (subpath \"{root}\"))\n({verdict} file-link (subpath \"{root}\"))\n",
                root = escape(&root)
            ));
        }
        // After every write allow: last match wins.
        for path in &self.write_denied {
            profile.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                escape(path)
            ));
        }
        // A carve-out keeps its verdict: writable again, then the read-only
        // layering and the denials inside it.
        if !self.write_reopened.is_empty() {
            let layers = self.read_only_layers();
            for path in &self.write_reopened {
                profile.push_str(&format!(
                    "(allow file-write* (subpath \"{}\"))\n",
                    escape(path)
                ));
                for (root, read_only) in read_only_layers_within(&layers, path) {
                    let verdict = if read_only { "deny" } else { "allow" };
                    profile.push_str(&format!(
                        "({verdict} file-write* (subpath \"{root}\"))\n({verdict} file-link (subpath \"{root}\"))\n",
                        root = escape(&root)
                    ));
                }
            }
            for path in self.write_denied_within_reopened() {
                profile.push_str(&format!(
                    "(deny file-write* (subpath \"{}\"))\n",
                    escape(path)
                ));
            }
        }
        for dir in &self.secret_dirs {
            profile.push_str(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                escape(dir)
            ));
        }
        for file in &self.secret_files {
            profile.push_str(&format!(
                "(deny file-read* (literal \"{}\"))\n",
                escape(file)
            ));
        }
        profile.push_str(VAULT_FILE_READ_DENIES);
        profile.push_str(&self.plane_private_rules());
        profile
            .push_str(crate::magician_v2::execution::goal_workspace_access::HYBRID_INDEX_SBPL_RULE);
        profile.push_str(
            &crate::magician_v2::execution::goal_workspace_access::sbpl_rules_with(
                &self.workspace,
                &|path| self.writes_allowed(path),
            ),
        );
        if !self.nested_agent_names.is_empty() {
            profile.push_str(&format!(
                "(deny process-exec (regex #\"/({})(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))\n",
                self.nested_agent_names.join("|")
            ));
        }
        for (executable, unreadable) in &self.nested_agents {
            profile.push_str(&format!(
                "(deny process-exec (literal \"{}\"))\n",
                escape(executable)
            ));
            if unreadable.is_dir() {
                profile.push_str(&format!(
                    "(deny file-read* (subpath \"{}\"))\n",
                    escape(unreadable)
                ));
            } else {
                profile.push_str(&format!(
                    "(deny file-read* (literal \"{}\"))\n",
                    escape(unreadable)
                ));
            }
        }
        for (_, package) in &self.nested_agent_packages {
            let rule = format!("(regex #\"/node_modules/{package}(/|$)\")");
            profile.push_str(&format!(
                "(deny process-exec {rule})\n(deny file-read* {rule})\n"
            ));
        }
        for (_, dir) in &self.nested_agent_dirs {
            let dir = escape(dir);
            profile.push_str(&format!(
                "(deny process-exec (subpath \"{dir}\"))\n(deny file-read* (subpath \"{dir}\"))\n"
            ));
        }
        profile.push_str(&self.network_rules());
        profile
    }

    /// Plane harness private storage that is not this box's own: the
    /// private folder (`plane::grant_channel::private_root`: every session's
    /// mcp config with its grant, and temporary homes) and the native homes
    /// root, but for this box's own session subfolder and native home, plus
    /// (outside a harness box) the shared Pi session store. Neither folder
    /// may be listed. A grant read there would be refused off its own
    /// channel anyway (`plane::grant_channel`); the homes also hold
    /// transcripts and auth copies.
    #[cfg(any(target_os = "macos", test))]
    fn plane_private_rules(&self) -> String {
        let mut rules = String::new();
        let (homes, pi_sessions) = plane_private_roots();
        let (private, own) = self.plane_private_own();
        for (root, own) in [(private, own.clone()), (homes, own)] {
            let escaped = sbpl_escape(&root);
            let mut filter = format!(
                "(require-all (subpath \"{escaped}\") (require-not (literal \"{escaped}\"))"
            );
            for path in &own {
                filter.push_str(&format!(
                    " (require-not (subpath \"{}\"))",
                    sbpl_escape(path)
                ));
            }
            filter.push(')');
            rules.push_str(&format!(
                "(deny file-read* file-write* {filter})\n(deny file-read-data file-write* (literal \"{escaped}\"))\n"
            ));
        }
        if !self.harness {
            rules.push_str(&format!(
                "(deny file-read* file-write* (subpath \"{}\"))\n",
                sbpl_escape(&pi_sessions)
            ));
        }
        rules
    }

    /// The private folder and what of it (and of the native homes) this box
    /// keeps: its own session subfolder and native home; for a harness box
    /// with no session (its channel could not open), the shared `unbound`
    /// subfolder; nothing for any other box.
    fn plane_private_own(&self) -> (PathBuf, Vec<PathBuf>) {
        use crate::magician_v2::execution::plane::grant_channel;
        let private = resolved(&grant_channel::private_root());
        let own = match &self.plane_session {
            Some(session) => std::iter::once(private.join(&session.id))
                .chain(session.own_paths.iter().map(|path| resolved(path)))
                .collect(),
            None if self.harness => vec![private.join("unbound")],
            None => Vec::new(),
        };
        (private, own)
    }

    /// [`Plan::plane_private_rules`] as bwrap mounts: each folder an empty
    /// tmpfs with this box's own subfolders bound back.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    fn plane_private_mounts(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        let (homes, pi_sessions) = plane_private_roots();
        let (private, own) = self.plane_private_own();
        for root in [private, homes] {
            if !root.is_dir() {
                continue;
            }
            args.extend([OsString::from("--tmpfs"), root.clone().into_os_string()]);
            for path in own
                .iter()
                .filter(|path| path.starts_with(&root) && path.is_dir())
            {
                args.extend([
                    OsString::from("--bind-try"),
                    path.clone().into_os_string(),
                    path.clone().into_os_string(),
                ]);
            }
        }
        if !self.harness && pi_sessions.is_dir() {
            args.extend([OsString::from("--tmpfs"), pi_sessions.into_os_string()]);
        }
        args
    }

    /// The SBPL network rules for the policy. Outside `allow`, unix sockets
    /// only under the writable roots and the system's logging and resolver.
    #[cfg(any(target_os = "macos", test))]
    fn network_rules(&self) -> String {
        // Never reachable directly, but the proxies and the plane's own
        // listener (which needs a grant).
        let services: Vec<u16> = self
            .magician_ports
            .iter()
            .copied()
            .filter(|port| {
                Some(*port) != self.proxy_port
                    && Some(*port) != self.strict_proxy_port
                    && Some(*port) != self.plane_port
            })
            .collect();
        let socket_rules = || {
            let mut rules = String::from("(deny network-outbound (remote unix-socket))\n");
            for root in &self.writable {
                rules.push_str(&format!(
                    "(allow network-outbound (remote unix-socket (subpath \"{}\")))\n",
                    sbpl_escape(root)
                ));
            }
            for socket in ALLOWED_SYSTEM_SOCKETS {
                rules.push_str(&format!(
                    "(allow network-outbound (remote unix-socket (path-literal \"{socket}\")))\n"
                ));
            }
            rules
        };
        let deny_services = |rules: &mut String, any_host: bool| {
            for port in &services {
                rules.push_str(&format!(
                    "(deny network-outbound (remote ip \"localhost:{port}\"))\n"
                ));
                // 3002 and a dev server also listen on the LAN address.
                if any_host {
                    rules.push_str(&format!(
                        "(deny network-outbound (remote ip \"*:{port}\"))\n"
                    ));
                }
            }
        };
        let allow_plane = |rules: &mut String| {
            if let Some(plane) = self.plane_port {
                rules.push_str(&format!(
                    "(allow network-outbound (remote ip \"localhost:{plane}\"))\n"
                ));
            }
        };
        if self.fenced {
            // An agent fenced from a goal workspace: no unix socket but its
            // own and the system's (no Docker or agent socket to stand in for
            // the box), no direct network but what follows.
            let mut rules = socket_rules();
            let direct = self.harness && self.network == NetworkMode::Allow;
            if direct {
                // A harness in `allow`: the internet directly, loopback only
                // where `allow_private` says, no service on any address.
                rules.push_str("(deny network-outbound (remote ip \"localhost:*\"))\n");
                rules.push_str(&self.loopback_allow_rules());
                deny_services(&mut rules, true);
            } else {
                rules.push_str("(deny network-outbound (remote ip \"*:*\"))\n");
                if self.network != NetworkMode::Deny {
                    rules.push_str(&self.loopback_allow_rules());
                }
                deny_services(&mut rules, false);
                if self.network != NetworkMode::Deny {
                    if let Some(strict) = self.strict_proxy_port {
                        rules.push_str(&format!(
                            "(allow network-outbound (remote ip \"localhost:{strict}\"))\n"
                        ));
                    }
                }
            }
            allow_plane(&mut rules);
            return rules;
        }
        if self.network == NetworkMode::Allow {
            // No network gate, but the loopback services stay closed.
            let mut rules = String::new();
            deny_services(&mut rules, false);
            allow_plane(&mut rules);
            return rules;
        }
        let mut rules = socket_rules();
        rules.push_str("(deny network-outbound (remote ip \"*:*\"))\n");
        if let (NetworkMode::Public, Some(proxy)) = (self.network, self.proxy_port) {
            // Direct: loopback only, but never a loopback service. Everything
            // else must go through the proxy, whose allow comes last.
            rules.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
            for port in services.iter().filter(|port| **port != proxy) {
                rules.push_str(&format!(
                    "(deny network-outbound (remote ip \"localhost:{port}\"))\n"
                ));
            }
            rules.push_str(&format!(
                "(allow network-outbound (remote ip \"localhost:{proxy}\"))\n"
            ));
            allow_plane(&mut rules);
        }
        rules
    }

    /// Whether the box lets a process write at `path`: under a writable root,
    /// with the deepest root deciding against a read-only one (a tie goes to
    /// read-only), as in the file sandbox.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    fn writes_allowed(&self, path: &Path) -> bool {
        let deepest = |roots: &[PathBuf]| {
            roots
                .iter()
                .filter(|root| path.starts_with(root))
                .map(|root| root.components().count())
                .max()
        };
        match (deepest(&self.writable), deepest(&self.read_only)) {
            (Some(writable), Some(read_only)) => writable > read_only,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// The SBPL allows for a fenced agent's `allow_private` loopback.
    #[cfg(any(target_os = "macos", test))]
    fn loopback_allow_rules(&self) -> String {
        let mut rules = String::new();
        for port in &self.loopback_allowed {
            match port {
                Some(port) => rules.push_str(&format!(
                    "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
                )),
                // A bare loopback entry would open every harness session's
                // grant channel (ephemeral ports): a fenced box keeps only
                // the entries that name a port.
                None if self.fenced => {},
                None => rules.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n"),
            }
        }
        // Every other session's grant channel open now, whatever the allows
        // above say; the box's own is allowed after this.
        // On any address (a direct harness could name `0.0.0.0` or the LAN
        // address).
        for port in crate::magician_v2::execution::plane::grant_channel::channel_ports() {
            if Some(port) != self.plane_port {
                rules.push_str(&format!(
                    "(deny network-outbound (remote ip \"localhost:{port}\"))\n(deny network-outbound (remote ip \"*:{port}\"))\n"
                ));
            }
        }
        rules
    }

    /// The box's masks over the plain binds (read-only roots, write
    /// denials, secrets, nested agents, the notes index), for the paths
    /// `keep` admits.
    #[cfg(any(target_os = "linux", test))]
    fn bwrap_masks(&self, keep: &dyn Fn(&Path) -> bool) -> Vec<OsString> {
        let mut args = Vec::new();
        // Read-only roots, and the writable roots inside them, deepest last.
        // A hard link out of a read-only mount fails (EXDEV). `-try`: a root
        // missing when the command starts is skipped, so a read-only root
        // created during the command is not fenced on Linux until the next
        // one (macOS rules match paths, present or not).
        for (root, read_only) in self
            .read_only_layers()
            .into_iter()
            .filter(|(root, _)| keep(root))
        {
            // Never writable inside a workspace opened only to read through a
            // delegator: a later mount wins, and this one comes after it.
            let read_only = read_only
                || self
                    .workspace
                    .read_only_dirs
                    .iter()
                    .any(|dir| root.starts_with(dir));
            args.extend([
                OsString::from(if read_only {
                    "--ro-bind-try"
                } else {
                    "--bind-try"
                }),
                root.clone().into_os_string(),
                root.into_os_string(),
            ]);
        }
        // After the binds: a later mount wins.
        for path in self
            .write_denied
            .iter()
            .filter(|path| path.exists() && keep(path))
        {
            args.extend([
                OsString::from("--ro-bind"),
                path.clone().into(),
                path.clone().into(),
            ]);
        }
        // A carve-out keeps its verdict: bound writable again, then the
        // read-only layering and the denials inside it.
        if !self.write_reopened.is_empty() {
            let layers = self.read_only_layers();
            for path in self
                .write_reopened
                .iter()
                .filter(|path| path.exists() && keep(path))
            {
                args.extend([
                    OsString::from("--bind"),
                    path.clone().into(),
                    path.clone().into(),
                ]);
                for (root, read_only) in read_only_layers_within(&layers, path)
                    .into_iter()
                    .filter(|(root, _)| keep(root))
                {
                    let read_only = read_only
                        || self
                            .workspace
                            .read_only_dirs
                            .iter()
                            .any(|dir| root.starts_with(dir));
                    args.extend([
                        OsString::from(if read_only {
                            "--ro-bind-try"
                        } else {
                            "--bind-try"
                        }),
                        root.clone().into_os_string(),
                        root.into_os_string(),
                    ]);
                }
            }
            for path in self
                .write_denied_within_reopened()
                .into_iter()
                .filter(|path| path.exists() && keep(path))
            {
                args.extend([
                    OsString::from("--ro-bind"),
                    path.clone().into(),
                    path.clone().into(),
                ]);
            }
        }
        for dir in self
            .secret_dirs
            .iter()
            .filter(|dir| dir.is_dir() && keep(dir))
        {
            args.extend([OsString::from("--tmpfs"), dir.clone().into()]);
        }
        for file in self
            .secret_files
            .iter()
            .filter(|file| file.is_file() && keep(file))
        {
            args.extend([
                OsString::from("--ro-bind"),
                OsString::from("/dev/null"),
                file.clone().into(),
            ]);
        }
        for (executable, unreadable) in &self.nested_agents {
            if !keep(unreadable) && !keep(executable) {
                continue;
            }
            if unreadable.is_dir() {
                args.extend([OsString::from("--tmpfs"), unreadable.clone().into()]);
            } else if executable.is_file() {
                args.extend([
                    OsString::from("--ro-bind"),
                    OsString::from("/dev/null"),
                    executable.clone().into(),
                ]);
            }
        }
        for (_, dir) in self
            .nested_agent_dirs
            .iter()
            .filter(|(_, dir)| dir.is_dir() && keep(dir))
        {
            args.extend([OsString::from("--tmpfs"), dir.clone().into()]);
        }
        for dir in self
            .hybrid_index_dirs
            .iter()
            .filter(|dir| dir.is_dir() && keep(dir))
        {
            args.extend([OsString::from("--tmpfs"), dir.clone().into()]);
        }
        args
    }

    #[cfg(any(target_os = "linux", test))]
    fn bwrap_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
            .into_iter()
            .map(OsString::from)
            .collect();
        for root in self.writable.iter().filter(|root| root.exists()) {
            args.extend([
                OsString::from("--bind"),
                root.clone().into(),
                root.clone().into(),
            ]);
        }
        args.extend(self.bwrap_masks(&|_| true));
        args.extend(self.plane_private_mounts());
        args.extend(
            crate::magician_v2::execution::goal_workspace_access::bwrap_args_with(
                &self.workspace,
                &|path| self.writes_allowed(path),
            ),
        );
        // A member workspace bound back from the host drops every mount the
        // box made inside it, so the box's own masks inside member
        // workspaces (read-only roots, write denials, secrets, nested agents,
        // the notes index) are made again on top.
        let reopened = &self.workspace.reopened;
        if !reopened.is_empty() {
            args.extend(self.bwrap_masks(&|path| {
                reopened
                    .iter()
                    .any(|dir| path.starts_with(dir) && path != dir.as_path())
            }));
        }
        for name in self.env_remove() {
            args.extend([OsString::from("--unsetenv"), OsString::from(name)]);
        }
        // bwrap cannot filter loopback by port nor reach the host's proxy from
        // a new network namespace, so a fenced shell gets no network at all,
        // in every mode (owner decision 2026-10-20, extended to `allow` after
        // review: Docker, CDP, the notes server and the supervisor would
        // otherwise reach past the box). A harness keeps its network.
        if self.network == NetworkMode::Deny
            || (self.network == NetworkMode::Public && self.proxy_port.is_none())
            || (self.fenced && !self.harness)
        {
            args.push(OsString::from("--unshare-net"));
        }
        // A fenced box's `/proc` shows only its own processes: another
        // session's environment can carry a plane grant.
        if self.fenced {
            args.push(OsString::from("--unshare-pid"));
        }
        args.push(OsString::from("--die-with-parent"));
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(network: NetworkMode, proxy_port: Option<u16>) -> Plan {
        Plan {
            writable: vec![PathBuf::from("/work/space"), PathBuf::from("/private/tmp")],
            secret_dirs: vec![PathBuf::from("/Users/o/.ssh")],
            secret_files: vec![PathBuf::from("/Users/o/MagicianNotes/.env")],
            network,
            proxy_port,
            nested_agents: vec![(
                PathBuf::from("/n/lib/node_modules/@openai/codex/bin/codex.js"),
                PathBuf::from("/n/lib/node_modules/@openai/codex"),
            )],
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs: vec![
                ("claude", PathBuf::from("/Users/o/.local/share/claude")),
                ("grok", PathBuf::from("/Users/o/.grok/bin")),
            ],
            writable_regex: Vec::new(),
            creatable_dirs: Vec::new(),
            write_denied: Vec::new(),
            write_reopened: Vec::new(),
            read_only: Vec::new(),
            magician_ports: vec![3002],
            fenced: false,
            harness: false,
            strict_proxy_port: None,
            plane_port: None,
            plane_session: None,
            loopback_allowed: Vec::new(),
            workspace: Default::default(),
            hybrid_index_dirs: Vec::new(),
        }
    }

    #[test]
    fn nested_agents_resolve_to_their_package() {
        assert_eq!(
            package_root(Path::new("/n/lib/node_modules/@openai/codex/bin/codex.js")),
            Some(PathBuf::from("/n/lib/node_modules/@openai/codex"))
        );
        assert_eq!(
            package_root(Path::new("/n/lib/node_modules/left-pad/index.js")),
            Some(PathBuf::from("/n/lib/node_modules/left-pad"))
        );
        assert_eq!(package_root(Path::new("/usr/local/bin/grok-1.0")), None);
        let profile = plan(NetworkMode::Allow, None).macos_profile();
        assert!(profile.contains(
            "(deny process-exec (literal \"/n/lib/node_modules/@openai/codex/bin/codex.js\"))"
        ));
    }

    /// Read-only roots: no writes and no hard links out of them, after the
    /// writable allows; a writable root inside one is re-opened after it.
    #[test]
    fn read_only_roots_deny_writes_and_hard_links_with_the_deepest_root_winning() {
        let mut boxed = plan(NetworkMode::Allow, None);
        boxed.writable.push(PathBuf::from("/notes/Workspaces"));
        boxed.read_only = vec![
            PathBuf::from("/notes"),
            PathBuf::from("/scopes/o/d/programs"),
        ];
        let profile = boxed.macos_profile();
        let at = |rule: &str| {
            profile
                .rfind(rule)
                .unwrap_or_else(|| panic!("missing {rule} in {profile}"))
        };
        let deny_notes = at("(deny file-write* (subpath \"/notes\"))");
        assert!(profile.contains("(deny file-link (subpath \"/notes\"))"));
        assert!(profile.contains("(deny file-write* (subpath \"/scopes/o/d/programs\"))"));
        assert!(profile.contains("(deny file-link (subpath \"/scopes/o/d/programs\"))"));
        assert!(at("(allow file-write* (subpath \"/work/space\"))") < deny_notes);
        assert!(deny_notes < at("(allow file-write* (subpath \"/notes/Workspaces\"))"));
        assert!(deny_notes < at("(allow file-link (subpath \"/notes/Workspaces\"))"));
        assert!(!profile.contains("(allow file-link (subpath \"/work/space\"))"));
    }

    #[test]
    fn bwrap_mounts_read_only_roots_after_the_binds_and_reopens_writable_ones_inside() {
        let base = tempfile::TempDir::new().unwrap();
        let notes = resolved(base.path()).join("notes");
        let workspaces = notes.join("Workspaces");
        std::fs::create_dir_all(&workspaces).unwrap();
        let mut boxed = plan(NetworkMode::Allow, None);
        boxed.writable = vec![workspaces.clone()];
        boxed.read_only = vec![notes.clone()];
        let args: Vec<String> = boxed
            .bwrap_args()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let notes = notes.display().to_string();
        let workspaces = workspaces.display().to_string();
        let ro = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind-try" && w[1] == notes && w[2] == notes)
            .expect("read-only root mounted");
        let rebind = args
            .windows(3)
            .rposition(|w| w[0] == "--bind-try" && w[1] == workspaces && w[2] == workspaces)
            .expect("writable root re-bound");
        assert!(ro < rebind, "{args:?}");
    }

    /// A write denial inside a re-opened writable root stays effective: it
    /// is emitted after the read-only layering.
    #[test]
    fn a_write_denial_inside_a_reopened_root_stays_last() {
        let mut boxed = plan(NetworkMode::Allow, None);
        boxed.writable.push(PathBuf::from("/notes/Workspaces"));
        boxed.read_only = vec![PathBuf::from("/notes")];
        boxed.write_denied = vec![PathBuf::from("/notes/Workspaces/.hooks")];
        let profile = boxed.macos_profile();
        let reopen = profile
            .rfind("(allow file-write* (subpath \"/notes/Workspaces\"))")
            .unwrap();
        let denial = profile
            .rfind("(deny file-write* (subpath \"/notes/Workspaces/.hooks\"))")
            .unwrap();
        assert!(reopen < denial, "{profile}");
    }

    #[test]
    fn read_only_layers_order_by_depth_and_emit_the_region_verdict_first() {
        let layers = read_only_layers(
            &[
                PathBuf::from("/notes"),
                PathBuf::from("/notes/Workspaces/locked"),
            ],
            &[
                PathBuf::from("/work"),
                PathBuf::from("/notes/Workspaces"),
                PathBuf::from("/notes/state.json*"),
            ],
        );
        assert_eq!(
            layers,
            vec![
                (PathBuf::from("/notes"), true),
                (PathBuf::from("/notes/Workspaces"), false),
                (PathBuf::from("/notes/state.json*"), false),
                (PathBuf::from("/notes/Workspaces/locked"), true),
            ]
        );
        // A tie goes to read-only: it comes last.
        let tie = read_only_layers(&[PathBuf::from("/a")], &[PathBuf::from("/a")]);
        assert_eq!(
            tie,
            vec![(PathBuf::from("/a"), false), (PathBuf::from("/a"), true)]
        );
        assert_eq!(
            read_only_layers_within(&layers, Path::new("/notes/Workspaces")),
            vec![(PathBuf::from("/notes/Workspaces/locked"), true)]
        );
        assert_eq!(
            read_only_layers_within(&layers, Path::new("/notes/Workspaces/locked/x")),
            vec![(PathBuf::from("/notes/Workspaces/locked/x"), true)]
        );
        assert!(read_only_layers_within(&layers, Path::new("/elsewhere")).is_empty());
    }

    #[test]
    fn the_profile_orders_denies_after_allows() {
        let profile = plan(NetworkMode::Public, Some(4242)).macos_profile();
        let deny_all = profile.find("(deny file-write* (subpath \"/\"))").unwrap();
        let allow_work = profile
            .find("(allow file-write* (subpath \"/work/space\"))")
            .unwrap();
        assert!(
            deny_all < allow_work,
            "last match wins: the allow comes after the blanket deny"
        );
        assert!(profile.contains("(deny file-read* (subpath \"/Users/o/.ssh\"))"));
        assert!(profile.contains("(deny file-read* (literal \"/Users/o/MagicianNotes/.env\"))"));
        assert!(profile.contains(
            "(deny process-exec (regex #\"/(claude|codex|pi|grok|agy|gemini)(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))"
        ));
        let deny_net = profile
            .find("(deny network-outbound (remote ip \"*:*\"))")
            .unwrap();
        let allow_local = profile
            .find("(allow network-outbound (remote ip \"localhost:*\"))")
            .unwrap();
        assert!(
            deny_net < allow_local,
            "loopback (and so the proxy) is reachable, nothing else directly"
        );
    }

    #[test]
    fn network_rules_follow_the_policy() {
        // `allow` has no network gate; only the loopback services stay closed.
        let open = plan(NetworkMode::Allow, None).macos_profile();
        assert!(!open.contains("(remote ip \"*:*\")") && !open.contains("unix-socket"));
        assert!(open.contains("(deny network-outbound (remote ip \"localhost:3002\"))"));
        let deny = plan(NetworkMode::Deny, None).macos_profile();
        assert!(
            deny.contains("(deny network-outbound (remote ip \"*:*\"))")
                && !deny.contains("(allow network-outbound (remote ip")
        );
        let no_proxy = plan(NetworkMode::Public, None).macos_profile();
        assert!(
            !no_proxy.contains("(allow network-outbound (remote ip"),
            "no proxy: no network rather than open"
        );
        let env = plan(NetworkMode::Public, Some(9)).proxy_env();
        assert!(env
            .iter()
            .any(|(k, v)| k == "HTTPS_PROXY" && v == "http://127.0.0.1:9"));
        if std::env::var_os("PIP_CERT").is_none() && Path::new(SYSTEM_CA_BUNDLE).is_file() {
            assert!(
                env.iter()
                    .any(|(k, v)| k == "PIP_CERT" && v == SYSTEM_CA_BUNDLE),
                "proxied pip verifies against the system roots"
            );
        }
        assert!(
            env.iter()
                .any(|(k, v)| k == "NO_PROXY" && v.contains("127.0.0.1")),
            "loopback goes direct"
        );
        assert!(plan(NetworkMode::Allow, Some(9)).proxy_env().is_empty());
    }

    /// Codex's shells reset `TMPDIR`, so `mktemp` falls back to the per-user
    /// temp dir; Magician's own `TMPDIR` may be elsewhere. Both are writable.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_os_user_temp_dir_is_writable_whatever_tmpdir_says() {
        let user_temp =
            resolved(&darwin_user_temp_dir().expect("macOS gives every user a temp dir"));
        let temps = temp_dirs();
        assert!(temps.contains(&user_temp) && temps.contains(&resolved(&std::env::temp_dir())));
        if user_temp.file_name().is_some_and(|name| name == "T") {
            assert!(build_cache_dirs().contains(&user_temp.parent().unwrap().join("C")));
        }
    }

    /// A task shell compiles Swift and clang modules into the module cache
    /// under the per-user cache dir, and writes nowhere else in that dir:
    /// the rest of `…/C` is other apps' caches. `…/C/clang` itself may be
    /// created (a user who never ran clang has none), nothing else in it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_task_shell_writes_the_compiler_module_cache_and_not_the_rest_of_the_user_cache_dir() {
        let cache =
            resolved(&darwin_user_cache_dir().expect("macOS gives every user a cache dir"));
        let clang = cache.join("clang");
        let module_cache = clang.join("ModuleCache");
        assert!(
            module_cache.starts_with("/private/var"),
            "SBPL matches the real path, not the /var symlink: {module_cache:?}"
        );
        let plan = Plan::with_denies(&ShellSandboxConfig::default(), Default::default());
        assert!(plan.writable.contains(&module_cache));
        assert!(!plan.writable.contains(&cache) && !plan.writable.contains(&clang));
        assert_eq!(plan.creatable_dirs, vec![clang.clone()]);
        assert!(plan.writes_allowed(&module_cache.join("2D8LMZVD367GV/SwiftShims.pcm")));
        assert!(!plan.writes_allowed(&cache.join("com.apple.other/cache.db")));
        let profile = plan.macos_profile();
        assert!(profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            sbpl_escape(&module_cache)
        )));
        assert!(profile.contains(&format!(
            "(allow file-write-create (literal \"{}\"))",
            sbpl_escape(&clang)
        )));
        assert!(
            !profile.contains(&format!("(subpath \"{}\")", sbpl_escape(&clang))),
            "only the clang dir entry, never its contents"
        );
        if available() {
            let parses = std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile, "/usr/bin/true"])
                .status()
                .unwrap()
                .success();
            assert!(parses, "sandbox-exec takes the profile");
        }
    }

    /// The module cache comes only from the OS's per-user cache dir (none
    /// off macOS, whatever `TMPDIR` says), and a writable root that does not
    /// exist yet is never bound by bwrap.
    #[test]
    fn module_cache_dirs_follow_the_user_cache_dir_and_missing_roots_are_not_bound() {
        if cfg!(not(target_os = "macos")) {
            assert_eq!(compiler_module_cache_dir(), None);
        }
        let mut boxed = plan(NetworkMode::Allow, None);
        let missing = PathBuf::from("/nonexistent-user-cache/C/clang/ModuleCache");
        boxed.writable.push(missing.clone());
        assert!(!boxed
            .bwrap_args()
            .iter()
            .any(|arg| arg == missing.as_os_str()));
    }

    #[test]
    fn bwrap_isolates_network_only_when_it_can() {
        let deny = plan(NetworkMode::Deny, None).bwrap_args();
        assert!(deny.contains(&OsString::from("--unshare-net")));
        let public = plan(NetworkMode::Public, Some(9)).bwrap_args();
        assert!(!public.contains(&OsString::from("--unshare-net")));
    }

    #[test]
    fn a_harness_is_exempt_only_from_its_own_denies() {
        let mut plan = plan(NetworkMode::Public, Some(9));
        plan.nested_agents.push((
            PathBuf::from("/n/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe"),
            PathBuf::from("/n/lib/node_modules/@anthropic-ai/claude-code"),
        ));
        plan.exempt_agent(Path::new("/n/lib/node_modules/@openai/codex/bin/codex.js"));
        assert_eq!(
            plan.nested_agents.len(),
            1,
            "codex is exempt, claude is not"
        );
        assert!(plan.nested_agents[0].0.ends_with("claude.exe"));
        assert!(!plan.nested_agent_names.contains(&"codex"));
        assert!(plan.nested_agent_names.contains(&"claude"));
        plan.writable_regex
            .push("^/Users/o/\\.claude\\.json".to_string());
        let profile = plan.macos_profile();
        assert!(profile.contains("(allow file-write* (regex #\"^/Users/o/\\.claude\\.json\"))"));
        assert!(profile.contains(
            "(deny process-exec (regex #\"/(claude|pi|grok|agy|gemini)(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))"
        ));
        assert!(
            !profile.contains("@openai/codex"),
            "nothing denies the engine's own package"
        );
        assert!(profile.contains(
            "(deny process-exec (regex #\"/node_modules/@anthropic-ai/claude-code(/|$)\"))"
        ));
    }

    /// The real launcher, on macOS: a harness box lets the engine start and
    /// write its own state file, while another agent CLI and other writes are
    /// refused.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_lets_the_harness_run_itself_only() {
        if !available() {
            return;
        }
        let agents = tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap();
        let script = |name: &str| {
            let path = agents.path().join(name);
            std::fs::write(&path, "#!/bin/sh\necho ran\n").unwrap();
            std::process::Command::new("chmod")
                .args(["+x"])
                .arg(&path)
                .status()
                .unwrap();
            resolved(&path)
        };
        let (engine, other) = (script("engine-cli"), script("other-cli"));
        let mut plan = Plan {
            writable: vec![resolved(&std::env::temp_dir())],
            secret_dirs: vec![],
            secret_files: vec![],
            network: NetworkMode::Allow,
            proxy_port: None,
            nested_agents: vec![
                (engine.clone(), engine.clone()),
                (other.clone(), other.clone()),
            ],
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs: Vec::new(),
            writable_regex: vec![format!(
                "^{}/state\\.json",
                regex::escape(&resolved(agents.path()).to_string_lossy())
            )],
            creatable_dirs: Vec::new(),
            write_denied: Vec::new(),
            write_reopened: Vec::new(),
            read_only: Vec::new(),
            magician_ports: Vec::new(),
            fenced: false,
            harness: false,
            strict_proxy_port: None,
            plane_port: None,
            plane_session: None,
            loopback_allowed: Vec::new(),
            workspace: Default::default(),
            hybrid_index_dirs: Vec::new(),
        };
        plan.exempt_agent(&engine);
        let run = |command: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &plan.macos_profile(), "/bin/sh", "-c", command])
                .status()
                .unwrap()
                .success()
        };
        assert!(run(&engine.display().to_string()), "the engine starts");
        assert!(
            !run(&other.display().to_string()),
            "another agent CLI does not"
        );
        let dir = resolved(agents.path());
        assert!(
            run(&format!("echo s > {}/state.json", dir.display())),
            "its own state file is writable"
        );
        assert!(!run(&format!("echo s > {}/elsewhere.json", dir.display())));
    }

    /// Claude Code signs in through the login keychain: its box keeps the
    /// keychain readable; every other harness box keeps it unreadable.
    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_keychain_harness_may_read_the_keychain() {
        if !available() {
            return;
        }
        let profile_of = |keychain_readable: bool| {
            let boxed = wrap_harness(
                OsStr::new("/bin/echo"),
                &[],
                &HarnessBox {
                    keychain_readable,
                    ..HarnessBox::default()
                },
            )
            .expect("boxed");
            boxed
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let deny = "Library/Keychains\"))";
        assert!(
            profile_of(false).contains(deny),
            "the default box denies the keychain"
        );
        assert!(
            !profile_of(true).contains(deny),
            "a keychain harness may read it"
        );
    }

    /// The real launcher, on macOS: inside writes work, outside writes and a
    /// secret read fail, loopback is unreachable under `deny`.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_enforces_the_profile() {
        if !available() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap();
        let secret = tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap();
        std::fs::write(secret.path().join("key"), "k").unwrap();
        let plan = Plan {
            writable: vec![resolved(work.path())],
            secret_dirs: vec![resolved(secret.path())],
            secret_files: vec![],
            network: NetworkMode::Deny,
            proxy_port: None,
            nested_agents: {
                let agent = secret.path().join("fake-agent");
                std::fs::write(&agent, "#!/bin/sh\necho ran\n").unwrap();
                std::process::Command::new("chmod")
                    .args(["+x"])
                    .arg(&agent)
                    .status()
                    .unwrap();
                let agent = resolved(&agent);
                vec![(agent.clone(), agent)]
            },
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs: Vec::new(),
            writable_regex: Vec::new(),
            creatable_dirs: Vec::new(),
            write_denied: Vec::new(),
            write_reopened: Vec::new(),
            read_only: Vec::new(),
            magician_ports: Vec::new(),
            fenced: false,
            harness: false,
            strict_proxy_port: None,
            plane_port: None,
            plane_session: None,
            loopback_allowed: Vec::new(),
            workspace: Default::default(),
            hybrid_index_dirs: Vec::new(),
        };
        let run = |command: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &plan.macos_profile(), "/bin/sh", "-c", command])
                .status()
                .unwrap()
                .success()
        };
        assert!(run(&format!("echo x > {}/in.txt", work.path().display())));
        assert!(!run(&format!(
            "echo x > {}/out.txt",
            outside.path().display()
        )));
        assert!(!outside.path().join("out.txt").exists());
        assert!(!run(&format!("cat {}/key", secret.path().display())));
        assert!(
            !run(&format!("{}/fake-agent", resolved(secret.path()).display())),
            "a nested agent cannot start"
        );
        assert!(!run(
            "/usr/bin/curl -s -m 3 http://127.0.0.1:9/ >/dev/null 2>&1 && exit 0 || exit 1"
        ));
    }

    #[test]
    fn build_caches_leave_executable_dirs_unwritable() {
        let caches = build_cache_dirs();
        let Some(home) = dirs::home_dir().map(|home| resolved(&home)) else {
            return;
        };
        for executable in [
            ".cargo/bin",
            ".pyenv/shims",
            ".deno",
            ".rustup",
            ".yarn",
            ".gradle",
            ".m2",
            "Library/pnpm",
        ] {
            assert!(
                !caches.contains(&home.join(executable)),
                "{executable} holds executables that run unboxed later"
            );
        }
        assert!(
            caches.contains(&home.join(".rustup/downloads"))
                && caches.contains(&home.join(".m2/repository"))
        );
    }

    #[test]
    fn harness_write_denials_come_after_every_allow() {
        let denied = harness_write_denied(
            Path::new("/n/lib/node_modules/@openai/codex/bin/codex.js"),
            &[PathBuf::from("/private/tmp/real-repo-under-temp")],
        );
        assert!(
            denied.contains(&PathBuf::from("/n/lib/node_modules/@openai/codex")),
            "the engine's package"
        );
        assert!(denied.contains(&PathBuf::from(
            "/n/lib/node_modules/@openai/codex/bin/codex.js"
        )));
        assert!(
            denied.contains(&PathBuf::from("/private/tmp/real-repo-under-temp")),
            "the real repo, though under temp"
        );
        if let Some(home) = dirs::home_dir().map(|home| resolved(&home)) {
            assert!(denied.contains(&home.join(".local/share/claude/versions")));
            assert!(denied.contains(&home.join(".claude/local")));
            assert!(denied.contains(&home.join(".grok/bin")));
        }
        let mut plan = plan(NetworkMode::Allow, None);
        plan.writable_regex
            .push("^/Users/o/\\.claude\\.json".to_string());
        plan.write_denied = denied;
        let profile = plan.macos_profile();
        let allow_tmp = profile
            .find("(allow file-write* (subpath \"/private/tmp\"))")
            .unwrap();
        let allow_regex = profile.find("(allow file-write* (regex").unwrap();
        let deny_repo = profile
            .find("(deny file-write* (subpath \"/private/tmp/real-repo-under-temp\"))")
            .unwrap();
        assert!(
            allow_tmp < deny_repo && allow_regex < deny_repo,
            "last match wins: the deny follows every allow"
        );
    }

    /// A coding run's god-mode box (`wrap_coding_call`) keeps the live repo
    /// source unwritable even under a writable root (`.`, the repo root, in
    /// the seed), but its scope sandbox base keeps its verdict, with the
    /// read-only roots and the run's real repo inside it denied again after.
    #[test]
    fn a_coding_god_box_denies_the_live_repo_but_keeps_its_sandbox_base() {
        let mut coding = plan(NetworkMode::Deny, None);
        coding.writable = vec![PathBuf::from("/repo"), PathBuf::from("/private/tmp")];
        coding.read_only = vec![PathBuf::from("/repo/magician_data_v3/notes")];
        coding.deny_repo_writes(
            Some((
                PathBuf::from("/repo"),
                PathBuf::from("/repo/magician_data_v3"),
            )),
            Some(PathBuf::from("/repo/magician_data_v3/scopes/real")),
        );
        assert_eq!(
            coding.write_reopened,
            vec![PathBuf::from("/repo/magician_data_v3")]
        );
        let profile = coding.macos_profile();
        let last = |rule: &str| {
            profile
                .rfind(rule)
                .unwrap_or_else(|| panic!("{rule} missing:\n{profile}"))
        };
        let allow_repo = profile
            .find("(allow file-write* (subpath \"/repo\"))")
            .unwrap();
        let deny_repo = last("(deny file-write* (subpath \"/repo\"))");
        let reopen_base = last("(allow file-write* (subpath \"/repo/magician_data_v3\"))");
        let notes_again = last("(deny file-write* (subpath \"/repo/magician_data_v3/notes\"))");
        let real_again = last("(deny file-write* (subpath \"/repo/magician_data_v3/scopes/real\"))");
        assert!(allow_repo < deny_repo, "the repo deny follows the writable root");
        assert!(deny_repo < reopen_base, "the base keeps its verdict");
        assert!(reopen_base < notes_again && reopen_base < real_again);

        // No writable root reaches the base: nothing is re-opened.
        let mut elsewhere = plan(NetworkMode::Deny, None);
        elsewhere.deny_repo_writes(
            Some((
                PathBuf::from("/repo"),
                PathBuf::from("/repo/magician_data_v3"),
            )),
            None,
        );
        assert!(elsewhere.write_reopened.is_empty());
        assert_eq!(elsewhere.write_denied, vec![PathBuf::from("/repo")]);
    }

    #[test]
    fn bwrap_rebinds_the_live_repo_sandbox_base_after_its_denial() {
        let work = tempfile::tempdir().unwrap();
        let repo = resolved(work.path()).join("repo");
        let base = repo.join("magician_data_v3");
        std::fs::create_dir_all(&base).unwrap();
        let mut plan = plan(NetworkMode::Deny, None);
        plan.writable = vec![repo.clone()];
        plan.deny_repo_writes(Some((repo.clone(), base.clone())), None);
        let args = plan.bwrap_args();
        let at = |flag: &str, path: &Path| {
            args.windows(2)
                .position(|pair| pair[0] == flag && pair[1].as_os_str() == path.as_os_str())
                .unwrap_or_else(|| panic!("{flag} {} missing: {args:?}", path.display()))
        };
        assert!(at("--ro-bind", &repo) < at("--bind", &base));
    }

    /// The real launcher, on macOS: in a coding run's god-mode box a write
    /// into the live repo fails, one into its sandbox base works.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_holds_the_live_repo_in_a_coding_god_box() {
        if !available() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let repo = resolved(work.path()).join("repo");
        let base = repo.join("magician_data_v3");
        std::fs::create_dir_all(&base).unwrap();
        let mut plan = plan(NetworkMode::Deny, None);
        plan.writable = vec![repo.clone()];
        plan.secret_dirs.clear();
        plan.secret_files.clear();
        plan.nested_agents.clear();
        plan.nested_agent_dirs.clear();
        plan.deny_repo_writes(Some((repo.clone(), base.clone())), None);
        let boxed = launch(
            &plan,
            OsStr::new("/bin/sh"),
            &[
                OsString::from("-c"),
                OsString::from(format!(
                    "touch '{}'; touch '{}'",
                    repo.join("in-repo").display(),
                    base.join("in-base").display()
                )),
            ],
        )
        .unwrap();
        let _ = std::process::Command::new(&boxed.program)
            .args(&boxed.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!repo.join("in-repo").exists(), "the live repo stays unwritable");
        assert!(base.join("in-base").exists(), "its sandbox base stays writable");
    }

    #[test]
    fn bwrap_remounts_write_denials_read_only_after_the_binds() {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let mut plan = plan(NetworkMode::Deny, None);
        plan.writable = vec![work.path().to_path_buf()];
        plan.write_denied = vec![repo.clone()];
        let args = plan.bwrap_args();
        let bind = args.iter().position(|arg| arg == "--bind").unwrap();
        let ro = args
            .iter()
            .position(|arg| arg.as_os_str() == repo.as_os_str())
            .unwrap();
        assert!(bind < ro && args[ro - 1] == "--ro-bind");
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--unsetenv" && pair[1] == "SSH_AUTH_SOCK"));
    }

    #[test]
    fn nested_agents_are_denied_off_path_too() {
        let profile = plan(NetworkMode::Allow, None).macos_profile();
        for package in [
            "@anthropic-ai/claude-code",
            "@openai/codex",
            "@earendil-works/pi-coding-agent",
            "@google/gemini-cli",
        ] {
            assert!(profile.contains(&format!(
                "(deny process-exec (regex #\"/node_modules/{package}(/|$)\"))"
            )));
            assert!(profile.contains(&format!(
                "(deny file-read* (regex #\"/node_modules/{package}(/|$)\"))"
            )));
        }
        assert!(profile.contains("(deny process-exec (subpath \"/Users/o/.local/share/claude\"))"));
        assert!(profile.contains("(deny file-read* (subpath \"/Users/o/.grok/bin\"))"));

        // Claude from its native install is the claude engine: only its own
        // denies open.
        let mut claude = plan(NetworkMode::Allow, None);
        claude.exempt_agent(Path::new("/Users/o/.local/share/claude/versions/2.1.0"));
        assert!(!claude.nested_agent_names.contains(&"claude"));
        assert!(claude.nested_agent_names.contains(&"codex"));
        let profile = claude.macos_profile();
        assert!(!profile.contains("claude-code") && !profile.contains(".local/share/claude"));
        assert!(profile.contains("@openai/codex(/|$)") && profile.contains("/Users/o/.grok/bin"));

        let mut codex = plan(NetworkMode::Allow, None);
        codex.exempt_agent(Path::new("/x/bin/codex.mjs"));
        assert!(
            !codex.nested_agent_names.contains(&"codex"),
            "an .mjs entry is the agent by name"
        );
        assert!(!codex.macos_profile().contains("@openai/codex(/|$)"));
    }

    #[test]
    fn unix_sockets_close_outside_allow() {
        assert!(!plan(NetworkMode::Allow, None)
            .macos_profile()
            .contains("unix-socket"));
        assert!(plan(NetworkMode::Allow, None).env_remove().is_empty());
        for (network, proxy) in [(NetworkMode::Deny, None), (NetworkMode::Public, Some(9))] {
            let plan = plan(network, proxy);
            let profile = plan.macos_profile();
            let deny = profile
                .find("(deny network-outbound (remote unix-socket))")
                .unwrap();
            let reallow = profile
                .find("(allow network-outbound (remote unix-socket (subpath \"/work/space\")))")
                .unwrap();
            assert!(
                deny < reallow,
                "sockets under a writable root stay reachable"
            );
            assert!(profile.contains(
                "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))"
            ));
            assert!(plan.env_remove().iter().any(|name| name == "SSH_AUTH_SOCK"));
            assert!(plan.env_remove().iter().any(|name| name == "DOCKER_HOST"));
        }
        let prefix = env_strip_prefix(&["SSH_AUTH_SOCK".to_string()], OsStr::new("/bin/sh"));
        assert_eq!(
            prefix,
            ["/usr/bin/env", "-u", "SSH_AUTH_SOCK"]
                .map(OsString::from)
                .to_vec()
        );
        assert!(env_strip_prefix(&[], OsStr::new("/bin/sh")).is_empty());
        assert!(env_strip_prefix(&["X".to_string()], OsStr::new("/odd/a=b")).is_empty());
    }

    #[test]
    fn public_loopback_skips_magicians_own_api() {
        let profile = plan(NetworkMode::Public, Some(4242)).macos_profile();
        let allow_local = profile
            .find("(allow network-outbound (remote ip \"localhost:*\"))")
            .unwrap();
        let deny_api = profile
            .find("(deny network-outbound (remote ip \"localhost:3002\"))")
            .unwrap();
        let allow_proxy = profile
            .find("(allow network-outbound (remote ip \"localhost:4242\"))")
            .unwrap();
        assert!(
            allow_local < deny_api && deny_api < allow_proxy,
            "the API is denied after loopback, the proxy allowed last"
        );
        let same = plan(NetworkMode::Public, Some(3002));
        assert!(
            !same
                .macos_profile()
                .contains("(deny network-outbound (remote ip \"localhost:3002\"))"),
            "never deny the proxy itself"
        );
        assert!(!plan(NetworkMode::Deny, None)
            .macos_profile()
            .contains("localhost:"));
    }

    #[test]
    fn registered_listeners_replace_the_default_api_port() {
        register_magician_listener(43_210);
        register_magician_listener(43_210);
        let ports = magician_listener_ports();
        assert_eq!(ports.iter().filter(|port| **port == 43_210).count(), 1);
        // The registry is process-wide and other tests register Magician's
        // API on 3002 itself, so the default is checked on its own rule: it
        // is added only while no Magician API port is registered.
        let registered_api_ports: Vec<u16> = MAGICIAN_LISTENERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, kind)| *kind == LoopbackService::MagicianApi)
            .map(|(port, _)| *port)
            .collect();
        assert!(!registered_api_ports.is_empty());
        if !registered_api_ports.contains(&DEFAULT_MAGICIAN_API_PORT) {
            assert!(!ports.contains(&DEFAULT_MAGICIAN_API_PORT));
        }
    }

    /// The real launcher, on macOS: a unix socket under a writable root is
    /// reachable, one elsewhere (an agent or Docker socket) is not, and the
    /// socket env is gone inside the box.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_closes_unix_sockets_outside_the_writable_roots() {
        if !available() || !Path::new("/usr/bin/nc").exists() {
            return;
        }
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap();
        let listen = |dir: &Path| {
            let path = resolved(dir).join("s.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    drop(stream);
                }
            });
            path
        };
        let (inside_socket, outside_socket) = (listen(inside.path()), listen(outside.path()));
        let mut plan = plan(NetworkMode::Deny, None);
        plan.writable = vec![resolved(inside.path())];
        plan.secret_dirs.clear();
        plan.secret_files.clear();
        plan.nested_agents.clear();
        plan.nested_agent_dirs.clear();
        let profile = plan.macos_profile();
        let connect = |socket: &Path| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile, "/usr/bin/nc", "-U", "-w", "2"])
                .arg(socket)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(
            connect(&inside_socket),
            "a socket under a writable root is reachable"
        );
        assert!(
            !connect(&outside_socket),
            "an agent or Docker socket elsewhere is not"
        );

        let boxed = launch(
            &plan,
            OsStr::new("/bin/sh"),
            &[
                OsString::from("-c"),
                OsString::from("test -z \"$SSH_AUTH_SOCK\""),
            ],
        )
        .unwrap();
        assert_eq!(
            boxed.env_remove.first().map(String::as_str),
            Some("SSH_AUTH_SOCK")
        );
        let stripped = std::process::Command::new(&boxed.program)
            .args(&boxed.args)
            .env("SSH_AUTH_SOCK", "/tmp/agent.sock")
            .status()
            .unwrap()
            .success();
        assert!(stripped, "the socket env does not reach the boxed process");
    }

    /// The real launcher, on macOS: under `public`, loopback is direct except
    /// Magician's own API; the proxy port stays reachable.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_keeps_magicians_api_off_loopback() {
        if !available() || !Path::new("/usr/bin/nc").exists() {
            return;
        }
        let bind = || std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (api, proxy, other) = (bind(), bind(), bind());
        let port = |listener: &std::net::TcpListener| listener.local_addr().unwrap().port();
        let mut plan = plan(NetworkMode::Public, Some(port(&proxy)));
        plan.magician_ports = vec![port(&api)];
        plan.secret_dirs.clear();
        plan.secret_files.clear();
        plan.nested_agents.clear();
        plan.nested_agent_dirs.clear();
        let profile = plan.macos_profile();
        let reach = |port: u16| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args([
                    "-p",
                    &profile,
                    "/usr/bin/nc",
                    "-z",
                    "-n",
                    "-w",
                    "2",
                    "127.0.0.1",
                    &port.to_string(),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(
            !reach(port(&api)),
            "Magician's API is not reachable from the box"
        );
        assert!(reach(port(&proxy)), "the egress proxy is");
        assert!(reach(port(&other)), "the owner's own loopback servers are");
    }

    /// Goal workspaces in the box (unit level, every platform): a
    /// non-member's profile denies the whole workspace, a member's denies the
    /// private goals it may not touch and pins the README; the bwrap argv
    /// hides the same with a tmpfs and `/dev/null`.
    /// Binding a member workspace back from the host drops every mount the
    /// box made inside it, so its read-only roots and hidden secrets there
    /// are mounted again after the bind-back.
    #[test]
    fn bwrap_masks_inside_a_member_workspace_are_made_again_after_its_bind_back() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let vault = ws.join("vault");
            std::fs::create_dir_all(&vault).unwrap();
            let archive = ws.join("Archive");
            let mut member = plan(NetworkMode::Allow, None);
            member.workspace = access::denies_for(Some("cmo"));
            member.secret_dirs = vec![vault.clone()];
            member.read_only = vec![archive.clone()];
            let argv: Vec<String> = member
                .bwrap_args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let ws_text = ws.display().to_string();
            let bind_back = argv
                .windows(3)
                .position(|w| w[0].ends_with("bind-try") && w[1] == ws_text && w[2] == ws_text)
                .expect("the member workspace is bound back");
            let vault_text = vault.display().to_string();
            let last_vault = argv.iter().rposition(|arg| *arg == vault_text).unwrap();
            assert!(last_vault > bind_back, "{argv:?}");
            assert_eq!(argv[last_vault - 1], "--tmpfs");
            let archive_text = archive.display().to_string();
            let last_archive = argv.iter().rposition(|arg| *arg == archive_text).unwrap();
            assert!(last_archive > bind_back, "{argv:?}");
            assert_eq!(argv[last_archive - 2], "--ro-bind-try");
        });
    }

    #[test]
    fn goal_workspaces_are_denied_in_the_profile_and_the_bwrap_argv() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let mut outsider = plan(NetworkMode::Allow, None);
            outsider.workspace = access::denies_for(Some("ambassador"));
            let profile = outsider.macos_profile();
            let deny = format!(
                "(deny file-read* file-write* file-link (subpath \"{}\"))",
                ws.display()
            );
            assert!(profile.contains(&deny), "{profile}");
            // After every write allow: last match wins.
            let last_allow = profile.rfind("(allow file-write*").unwrap();
            assert!(profile.find(&deny).unwrap() > last_allow);
            assert!(profile.contains("notes/hybrid-index(/|$)"));
            let argv: Vec<String> = outsider
                .bwrap_args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let at = argv
                .iter()
                .position(|arg| *arg == fx.root.display().to_string())
                .expect("the workspaces folder is masked");
            assert_eq!(argv[at - 1], "--tmpfs");
            assert!(!argv.contains(&ws.display().to_string()));
            // Deny by default: the whole folder, so a workspace made after
            // the box was built is closed too.
            assert!(profile.contains(&format!(
                "(deny file-read* file-write* file-link (require-all (subpath \"{0}\") (require-not (literal \"{0}\"))",
                fx.root.display()
            )));

            let mut member = plan(NetworkMode::Allow, None);
            member.workspace = access::denies_for(Some("cmo"));
            let profile = member.macos_profile();
            assert!(!profile.contains(&deny));
            for private in ["Goals/X/secret.md", "Goals/X/ceo plan.md"] {
                assert!(profile.contains(&format!(
                    "(deny file-read* file-write* file-link (literal \"{}\"))",
                    ws.join(private).display()
                )));
            }
            assert!(profile.contains(&format!(
                "(deny file-write* file-link (literal \"{}\"))",
                ws.join("README.md").display()
            )));
            assert!(profile.contains(&format!(
                "(deny file-write* file-link (literal \"{}\"))",
                ws.join("Goals/X").display()
            )));
            let argv: Vec<String> = member
                .bwrap_args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let secret = ws.join("Goals/X/secret.md").display().to_string();
            let at = argv.iter().position(|arg| *arg == secret).expect("masked");
            assert_eq!(argv[at - 1], "/dev/null");
            assert_eq!(argv[at - 2], "--ro-bind");
            assert!(!argv.contains(&ws.join("Goals/X/x.md").display().to_string()));
        });
    }

    /// `Plan::current` takes the acting agent from runtime identity; a
    /// harness box takes it from the request.
    #[tokio::test]
    async fn the_box_follows_the_acting_agent() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        let _roots = access::install_test_roots(vec![fx.root.clone()]);
        let sandbox = ShellSandboxConfig::default();
        let outsider = crate::magician_v2::execution::native_executors::ACTING_AGENT
            .scope(Some("ambassador".into()), async { Plan::current(&sandbox) })
            .await;
        assert_eq!(
            outsider.workspace.hidden_dirs,
            vec![fx.root.join("Magican")]
        );
        let admin = crate::magician_v2::execution::native_executors::ACTING_AGENT
            .scope(Some(access::DEFAULT_ADMIN_AGENT.into()), async {
                Plan::current(&sandbox)
            })
            .await;
        assert!(admin.workspace.is_empty());
        // No identity at all is a non-member.
        assert_eq!(
            Plan::current(&sandbox).workspace.hidden_dirs,
            vec![fx.root.join("Magican")]
        );
        let harness = Plan::for_agent(&sandbox, Some("cmo"));
        assert!(harness.workspace.hidden_dirs.is_empty());
        assert!(!harness.workspace.hidden_files.is_empty());
    }

    /// On Linux a fenced plane harness is refused (its network cannot be held
    /// to its own channel yet); a fenced bwrap box gets its own PID
    /// namespace, so no other session's `/proc/<pid>/environ` is visible.
    #[test]
    fn a_fenced_plane_harness_fails_closed_on_linux_and_fenced_boxes_unshare_pids() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        use crate::magician_v2::execution::native_executors::ACTING_AGENT;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            FORCE_LINUX_HARNESS_REFUSAL.with(|forced| forced.set(Some(true)));
            let fence = |agent: &str| {
                ACTING_AGENT.sync_scope(Some(agent.to_string()), || {
                    fence_plane_harness(OsStr::new("/bin/echo"), &[], false, Vec::new())
                })
            };
            let fenced = fence("cmo");
            let admin = fence(access::DEFAULT_ADMIN_AGENT);
            FORCE_LINUX_HARNESS_REFUSAL.with(|forced| forced.set(None));
            assert!(matches!(fenced, WorkspaceFence::Refused(ref why) if why.contains("Linux")));
            assert!(matches!(admin, WorkspaceFence::Unfenced));
        });
        let mut fenced = plan(NetworkMode::Allow, None);
        fenced.fenced = true;
        assert!(fenced.bwrap_args().iter().any(|arg| arg == "--unshare-pid"));
        assert!(!plan(NetworkMode::Allow, None)
            .bwrap_args()
            .iter()
            .any(|arg| arg == "--unshare-pid"));
    }

    /// A fenced coding run's harness box reaches its own citizen channel
    /// (the run's session, from the task-local the coding turn runs in),
    /// never Magician's API port nor another session's channel.
    #[tokio::test]
    async fn a_fenced_coding_harness_reaches_its_citizen_channel_and_nothing_else() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        use crate::magician_v2::execution::plane::grant_channel::{
            forget, open_to, with_plane_session,
        };
        register_magician_listener(3002);
        let fx = access::test_fixture();
        let _roots = access::install_test_roots(vec![fx.root.clone()]);
        let (citizen, _citizen_guard) = open_to("cit_box_test", 1, Vec::new()).await.unwrap();
        let (other, _other_guard) = open_to("cit_box_other_test", 1, Vec::new()).await.unwrap();
        let profile = with_plane_session(Some(citizen.clone()), async {
            let mut plan = Plan::for_agent(&ShellSandboxConfig::default(), Some("cmo"));
            plan.harness = true;
            assert_eq!(plan.plane_port, Some(citizen.port));
            plan.macos_profile()
        })
        .await;
        assert!(profile.contains(&format!(
            "(allow network-outbound (remote ip \"localhost:{}\"))",
            citizen.port
        )));
        assert!(profile.contains(&format!(
            "(deny network-outbound (remote ip \"localhost:{}\"))",
            other.port
        )));
        assert!(
            profile.contains("(deny network-outbound (remote ip \"*:3002\"))")
                || profile.contains("(deny network-outbound (remote ip \"*:*\"))")
        );
        // Outside the run's session a box gets no channel at all.
        assert_eq!(
            Plan::for_agent(&ShellSandboxConfig::default(), Some("cmo")).plane_port,
            None
        );
        forget("cit_box_test");
        forget("cit_box_other_test");
    }

    /// The Linux harness refusal predicate: on by default on Linux only, and
    /// forced either way in tests.
    #[test]
    fn the_linux_harness_isolation_predicate_follows_the_platform_unless_forced() {
        assert_eq!(linux_harness_isolation_missing(), cfg!(target_os = "linux"));
        for forced in [true, false] {
            FORCE_LINUX_HARNESS_REFUSAL.with(|cell| cell.set(Some(forced)));
            assert_eq!(linux_harness_isolation_missing(), forced);
        }
        FORCE_LINUX_HARNESS_REFUSAL.with(|cell| cell.set(None));
    }

    /// A fenced box never reaches another session's grant channel: a bare
    /// loopback `allow_private` entry is dropped for it, and every live
    /// channel port but its own is denied after the allows.
    #[tokio::test]
    async fn a_fenced_box_cannot_reach_another_sessions_grant_channel() {
        use crate::magician_v2::execution::plane::grant_channel::{forget, open_to};
        let (other, _other_guard) = open_to("plt_box_other_test", 1, Vec::new()).await.unwrap();
        let (own, _own_guard) = open_to("plt_box_own_test", 1, Vec::new()).await.unwrap();
        let mut fenced = plan(NetworkMode::Public, Some(4242));
        fenced.fenced = true;
        fenced.loopback_allowed = vec![None, Some(11_434)];
        fenced.plane_port = Some(own.port);
        let profile = fenced.macos_profile();
        assert!(
            !profile.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
            "{profile}"
        );
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:11434\"))"));
        let deny_other = format!(
            "(deny network-outbound (remote ip \"localhost:{}\"))",
            other.port
        );
        let allow_own = format!(
            "(allow network-outbound (remote ip \"localhost:{}\"))",
            own.port
        );
        assert!(profile.contains(&deny_other), "{profile}");
        assert!(!profile.contains(&format!(
            "(deny network-outbound (remote ip \"localhost:{}\"))",
            own.port
        )));
        assert!(profile.contains(&allow_own));
        // A direct harness (fenced, `allow`) is denied the other channel on
        // every address, and keeps its own.
        let mut direct = plan(NetworkMode::Allow, None);
        direct.fenced = true;
        direct.harness = true;
        direct.plane_port = Some(own.port);
        let profile = direct.macos_profile();
        assert!(
            profile.contains(&format!(
                "(deny network-outbound (remote ip \"*:{}\"))",
                other.port
            )),
            "{profile}"
        );
        assert!(!profile.contains(&format!(
            "(deny network-outbound (remote ip \"*:{}\"))",
            own.port
        )));
        assert!(profile.contains(&allow_own));
        forget("plt_box_other_test");
        forget("plt_box_own_test");
    }

    /// Plane harness sessions' private storage: a box reads and lists none
    /// of it but its own session's subfolder, and reaches only its own grant
    /// channel; a shell box gets no session at all.
    #[test]
    fn a_box_keeps_other_plane_sessions_private_storage_closed() {
        use crate::magician_v2::execution::plane::grant_channel::{private_root, PlaneSession};
        let private = resolved(&private_root());
        let mut harness = plan(NetworkMode::Allow, None);
        harness.harness = true;
        harness.fenced = true;
        harness.plane_port = Some(4250);
        harness.plane_session = Some(PlaneSession {
            id: "sess-a".into(),
            port: 4250,
            own_paths: Vec::new(),
        });
        let profile = harness.macos_profile();
        let root = sbpl_escape(&private);
        assert!(
            profile.contains(&format!(
                "(deny file-read* file-write* (require-all (subpath \"{root}\") (require-not (literal \"{root}\")) (require-not (subpath \"{root}/sess-a\"))))"
            )),
            "{profile}"
        );
        assert!(profile.contains(&format!(
            "(deny file-read-data file-write* (literal \"{root}\"))"
        )));
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:4250\"))"));
        let mut shell = plan(NetworkMode::Allow, None);
        shell.fenced = true;
        let shell_profile = shell.macos_profile();
        assert!(shell_profile.contains(&format!(
            "(deny file-read* file-write* (require-all (subpath \"{root}\") (require-not (literal \"{root}\"))))"
        )));
        assert!(!shell_profile.contains("localhost:4250"));
        // bwrap: the folder an empty tmpfs, the session's own subfolder
        // bound back; a shell box binds nothing back.
        std::fs::create_dir_all(private.join("sess-a")).unwrap();
        let text = |args: Vec<OsString>| -> Vec<String> {
            args.iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let mounts = text(harness.plane_private_mounts());
        let at = mounts
            .iter()
            .position(|mount| *mount == private.display().to_string())
            .expect("the private folder is masked");
        assert_eq!(mounts[at - 1], "--tmpfs");
        let own = private.join("sess-a").display().to_string();
        assert!(mounts
            .windows(3)
            .any(|w| w[0] == "--bind-try" && w[1] == own && w[2] == own));
        assert!(!text(shell.plane_private_mounts())
            .iter()
            .any(|mount| mount == "--bind-try"));
        let _ = std::fs::remove_dir_all(private.join("sess-a"));
    }

    /// Live on macOS: neither a fenced shell box nor another session's
    /// harness box can read or list a session's grant file; the session's
    /// own box reads its own.
    #[test]
    fn sandbox_exec_keeps_another_sessions_grant_file_unreadable_and_unlisted() {
        if !available() || !cfg!(target_os = "macos") {
            return;
        }
        use crate::magician_v2::execution::plane::grant_channel::{private_root, PlaneSession};
        let other = private_root().join("sess-live-other");
        let own = private_root().join("sess-live-own");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(other.join("x.mcp.json"), "plt_secret_grant").unwrap();
        std::fs::write(own.join("own.mcp.json"), "mine").unwrap();
        let run = |plan: &Plan, command: &str| -> (bool, String) {
            let output = std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &plan.macos_profile(), "/bin/sh", "-c", command])
                .output()
                .unwrap();
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        };
        let mut harness = plan(NetworkMode::Allow, None);
        harness.harness = true;
        harness.plane_session = Some(PlaneSession {
            id: "sess-live-own".into(),
            port: 1,
            own_paths: Vec::new(),
        });
        let mut shell = plan(NetworkMode::Allow, None);
        shell.fenced = true;
        for boxed in [&harness, &shell] {
            let (read, out) = run(
                boxed,
                &format!("cat '{}'", other.join("x.mcp.json").display()),
            );
            assert!(!read && !out.contains("plt_secret_grant"), "{out}");
            let (_, listing) = run(boxed, &format!("ls '{}'", private_root().display()));
            assert!(!listing.contains("sess-live-other"), "{listing}");
        }
        let (read, out) = run(
            &harness,
            &format!("cat '{}'", own.join("own.mcp.json").display()),
        );
        assert!(read && out.contains("mine"), "{out}");
        let (read, _) = run(
            &shell,
            &format!("cat '{}'", own.join("own.mcp.json").display()),
        );
        assert!(!read);
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&own);
    }

    /// Live on macOS: under the box, `cat` of a goal fails for a non-member
    /// and for a member reading another owner's private goal, a member reads
    /// its own internal goal, and a member can neither rewrite the README nor
    /// rename the folder holding a private goal.
    #[test]
    fn sandbox_exec_keeps_goal_workspaces_from_non_members() {
        if !available() || !cfg!(target_os = "macos") {
            return;
        }
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            let ws = fx.root.join("Magican");
            let run = |agent: &str, command: &str| {
                let mut plan = plan(NetworkMode::Allow, None);
                plan.writable = vec![fx.root.parent().unwrap().to_path_buf()];
                plan.workspace = access::denies_for(Some(agent));
                std::process::Command::new("/usr/bin/sandbox-exec")
                    .args(["-p", &plan.macos_profile(), "/bin/sh", "-c", command])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            };
            let cat = |path: &str| format!("cat '{}'", ws.join(path).display());
            assert!(
                !run("ambassador", &cat("Goals/X/x.md")),
                "a non-member reads a goal"
            );
            assert!(!run("ambassador", &format!("ls '{}'", ws.display())));
            assert!(
                run("cmo", &cat("Goals/X/x.md")),
                "a member reads an internal goal"
            );
            assert!(
                !run("cmo", &cat("Goals/X/secret.md")),
                "a member reads the user's private goal"
            );
            assert!(
                !run("cmo", &cat("Goals/X/ceo plan.md")),
                "a member reads ceo's private goal"
            );
            assert!(!run(
                "cmo",
                &format!(
                    "echo 'members: [cmo, ambassador]' > '{}'",
                    ws.join("README.md").display()
                )
            ));
            assert!(std::fs::read_to_string(ws.join("README.md"))
                .unwrap()
                .contains("members: [cmo]"));
            assert!(!run(
                "cmo",
                &format!(
                    "mv '{}' '{}'",
                    ws.join("Goals/X").display(),
                    ws.join("Goals/Y").display()
                )
            ));
            assert!(ws.join("Goals/X/secret.md").is_file());
            assert!(!run(
                "cmo",
                &format!(
                    "ln '{}' '{}'",
                    ws.join("Goals/X/secret.md").display(),
                    fx.root.parent().unwrap().join("linked.md").display()
                )
            ));
            assert!(
                run(
                    "cmo",
                    &format!("echo update > '{}'", ws.join("Goals/X/new.md").display())
                ),
                "a member still writes its workspace"
            );
            assert!(run(access::DEFAULT_ADMIN_AGENT, &cat("Goals/X/secret.md")));
        });
    }

    /// Every box, every mode: the supervisor, Magicutor, CDP, the notes
    /// server and the UI dev servers are denied on loopback, not only
    /// Magician's API.
    /// A registered dev server is denied to every agent's box but the
    /// admin's (the owner's VibeDev session acts as the admin).
    #[test]
    fn only_the_admin_box_reaches_a_registered_dev_server() {
        register_loopback_service(47_174, LoopbackService::DevServer);
        register_loopback_service(8081, LoopbackService::Supervisor);
        let sandbox = ShellSandboxConfig::default();
        let admin = crate::magician_v2::execution::goal_workspace_access::DEFAULT_ADMIN_AGENT;
        let admin_plan = Plan::for_agent(&sandbox, Some(admin));
        assert!(!admin_plan.magician_ports.contains(&47_174));
        assert!(admin_plan.magician_ports.contains(&8081));
        for agent in [Some("cmo"), None] {
            assert!(
                Plan::for_agent(&sandbox, agent)
                    .magician_ports
                    .contains(&47_174),
                "{agent:?}"
            );
        }
    }

    #[test]
    fn every_box_denies_supervisor_cdp_notes_and_dev_ports() {
        // Registered where Magician learns them (startup, config, notes
        // settings); only CDP's 9222 is denied before that.
        register_loopback_service(8081, LoopbackService::Supervisor);
        register_loopback_service(3003, LoopbackService::Magicutor);
        register_loopback_service(3021, LoopbackService::NotesServer);
        register_loopback_service(5173, LoopbackService::DevServer);
        register_loopback_service(4173, LoopbackService::DevServer);
        for (network, proxy) in [
            (NetworkMode::Allow, None),
            (NetworkMode::Public, Some(4242)),
        ] {
            let mut plan = plan(network, proxy);
            plan.magician_ports = magician_listener_ports();
            let profile = plan.macos_profile();
            for port in [8081, 3003, 9222, 3021, 5173, 4173] {
                assert!(
                    profile.contains(&format!(
                        "(deny network-outbound (remote ip \"localhost:{port}\"))"
                    )),
                    "{network:?}: {port}"
                );
            }
        }
        register_loopback_service_url("http://127.0.0.1:43211/", LoopbackService::NotesServer);
        register_loopback_service_url("https://notes.example.com/", LoopbackService::NotesServer);
        let ports = magician_listener_ports();
        assert!(ports.contains(&43_211));
        assert!(!ports.contains(&443));
    }

    /// A fenced agent reaches loopback only where `allow_private` says, never
    /// a service, and the rest of the network only through the strict proxy
    /// (in `allow` too); unix sockets are closed but its own and the
    /// system's; the plane-only MCP port stays open.
    #[test]
    fn a_fenced_agent_reaches_only_allow_private_loopback() {
        for network in [NetworkMode::Public, NetworkMode::Allow] {
            let mut fenced = plan(network, Some(4242));
            fenced.fenced = true;
            fenced.strict_proxy_port = Some(4243);
            fenced.plane_port = Some(4244);
            fenced.loopback_allowed = vec![Some(11_434)];
            let profile = fenced.macos_profile();
            assert!(
                !profile.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
                "{network:?}"
            );
            assert!(
                profile.contains("(deny network-outbound (remote ip \"*:*\"))"),
                "{network:?}"
            );
            assert!(profile.contains("(allow network-outbound (remote ip \"localhost:11434\"))"));
            assert!(profile.contains("(allow network-outbound (remote ip \"localhost:4243\"))"));
            assert!(profile.contains("(allow network-outbound (remote ip \"localhost:4244\"))"));
            assert!(
                profile.contains("(deny network-outbound (remote unix-socket))"),
                "{network:?}"
            );
            assert!(fenced
                .proxy_env()
                .iter()
                .any(|(name, value)| name == "HTTPS_PROXY" && value.ends_with(":4243")));
        }
        // A fenced harness in `allow`: direct internet, but no loopback but
        // the allowed ports and no service on any address.
        let mut harness = plan(NetworkMode::Allow, None);
        harness.fenced = true;
        harness.harness = true;
        harness.plane_port = Some(4244);
        let profile = harness.macos_profile();
        assert!(profile.contains("(deny network-outbound (remote ip \"localhost:*\"))"));
        assert!(profile.contains("(deny network-outbound (remote ip \"*:3002\"))"));
        assert!(!profile.contains("(deny network-outbound (remote ip \"*:*\"))"));
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:4244\"))"));
        assert!(harness.proxy_env().is_empty());
    }

    /// Linux cannot filter loopback by port nor reach the host's proxy from a
    /// new network namespace: a fenced shell gets no network in any mode; a
    /// harness keeps its network (it needs its model's API).
    #[test]
    fn fenced_agent_bwrap_unshares_the_network() {
        let unshared = |network: NetworkMode, proxy: Option<u16>, fenced: bool, harness: bool| {
            let mut plan = plan(network, proxy);
            plan.fenced = fenced;
            plan.harness = harness;
            plan.bwrap_args().iter().any(|arg| arg == "--unshare-net")
        };
        assert!(unshared(NetworkMode::Public, Some(4242), true, false));
        assert!(unshared(NetworkMode::Allow, None, true, false));
        assert!(!unshared(NetworkMode::Public, Some(4242), false, false));
        assert!(!unshared(NetworkMode::Allow, None, true, true));
    }

    /// The real launcher, on macOS: a registered loopback service is not
    /// reachable from the box, in `allow` too; another loopback port is.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_cannot_reach_registered_loopback_services() {
        if !available() || !Path::new("/usr/bin/nc").exists() {
            return;
        }
        let bind = || std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (service, other) = (bind(), bind());
        let port = |listener: &std::net::TcpListener| listener.local_addr().unwrap().port();
        let mut plan = plan(NetworkMode::Allow, None);
        plan.magician_ports = vec![port(&service)];
        plan.secret_dirs.clear();
        plan.secret_files.clear();
        plan.nested_agents.clear();
        plan.nested_agent_dirs.clear();
        let profile = plan.macos_profile();
        let reach = |port: u16| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args([
                    "-p",
                    &profile,
                    "/usr/bin/nc",
                    "-z",
                    "-n",
                    "-w",
                    "2",
                    "127.0.0.1",
                ])
                .arg(port.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(
            !reach(port(&service)),
            "a registered service is unreachable"
        );
        assert!(reach(port(&other)), "other loopback stays open in allow");
    }

    /// Super god mode and unboxed plane harnesses keep the goal workspace
    /// fence: the workspace-only profile allows everything else.
    #[test]
    fn super_god_launch_still_denies_workspaces() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            register_loopback_service(8081, LoopbackService::Supervisor);
            let denies = access::denies_for(Some("ambassador"));
            let profile = workspace_only_profile(&workspace_only_plan(&denies, true));
            assert!(profile.starts_with("(version 1)\n(allow default)\n"));
            // The same network rules as the shell box: no unix socket but
            // its own, no service on any address.
            assert!(profile.contains("(deny network-outbound (remote unix-socket))"));
            assert!(
                profile.contains("(deny network-outbound (remote ip \"*:8081\"))")
                    || profile.contains("(deny network-outbound (remote ip \"*:*\"))")
            );
            assert!(profile.contains(&format!(
                "(deny file-read* file-write* file-link (subpath \"{}\"))",
                fx.root.join("Magican").display()
            )));
            assert!(profile.contains("hybrid-index"));
            assert!(profile.contains("(deny network-outbound (remote ip \"localhost:8081\"))"));
            // Nothing to fence for the admin: it launches unwrapped.
            let admin = access::denies_for(Some(access::DEFAULT_ADMIN_AGENT));
            assert!(wrap_workspace_only(OsStr::new("/bin/echo"), &[], &admin, true).is_none());
        });
    }

    /// Live on macOS: a workspace created after the box was built is closed
    /// to a running non-member box (deny by default), while its member
    /// workspace stays open.
    #[test]
    fn a_workspace_created_after_launch_is_denied_to_a_running_box() {
        if !available() || !cfg!(target_os = "macos") {
            return;
        }
        use crate::magician_v2::execution::goal_workspace_access as access;
        let fx = access::test_fixture();
        access::with_test_roots(vec![fx.root.clone()], || {
            let mut plan = plan(NetworkMode::Allow, None);
            plan.writable = vec![fx.root.parent().unwrap().to_path_buf()];
            plan.workspace = access::denies_for(Some("cmo"));
            let profile = plan.macos_profile();
            // Created after the profile was built.
            let late = fx.root.join("Late");
            std::fs::create_dir_all(&late).unwrap();
            std::fs::write(late.join("note.md"), "late").unwrap();
            let run = |command: String| {
                std::process::Command::new("/usr/bin/sandbox-exec")
                    .args(["-p", &profile, "/bin/sh", "-c", &command])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            };
            assert!(!run(format!("cat '{}'", late.join("note.md").display())));
            assert!(run(format!(
                "cat '{}'",
                fx.root.join("Magican/Goals/X/x.md").display()
            )));
            assert!(!run(format!(
                "mkdir '{}'",
                fx.root.join("Another").display()
            )));
        });
    }
}
