//! The OS sandbox around every shell command outside a coding run
//! (`execution.shell_sandbox.os_enforce`). Design:
//! `docs/plans/2026-10-13-hard-shell-sandbox-design.md`.
//!
//! The string checks in `native_executors` look at the working directory and a
//! fragment list; nothing inside a command. This box is what the kernel
//! enforces, however the command phrases itself:
//!
//! - writes only under the sandbox's working-dir roots, temp, `/dev` and tool
//!   caches;
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
const SOCKET_ENV: &[&str] = &["SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_AGENT_INFO", "DOCKER_HOST", "DOCKER_CONTEXT"];

/// Unix sockets outside the writable roots a boxed process may still reach:
/// logging and name resolution.
#[cfg(any(target_os = "macos", test))]
const ALLOWED_SYSTEM_SOCKETS: &[&str] = &["/private/var/run/syslog", "/private/var/run/mDNSResponder"];

/// Magician's API port while startup has registered none.
const DEFAULT_MAGICIAN_API_PORT: u16 = 3002;

static MAGICIAN_LISTENERS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// Record a loopback port Magician itself serves (its HTTP API): a boxed
/// process may not reach it directly, while other loopback stays open in
/// `public`. Call at startup once the port is known; until then the default
/// API port is denied.
pub fn register_magician_listener(port: u16) {
    let mut ports = MAGICIAN_LISTENERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !ports.contains(&port) {
        ports.push(port);
    }
}

fn magician_listener_ports() -> Vec<u16> {
    let ports = MAGICIAN_LISTENERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if ports.is_empty() {
        vec![DEFAULT_MAGICIAN_API_PORT]
    } else {
        ports
    }
}

/// Agent packages installed off `PATH` (global prefixes, other nvm versions,
/// `npx` copies): Linux masks them by directory, having no path regex.
#[cfg(target_os = "linux")]
fn installed_agent_packages(home: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut roots = vec![PathBuf::from("/usr/lib/node_modules"), PathBuf::from("/usr/local/lib/node_modules")];
    if let Some(home) = home {
        for (parent, suffix) in [(home.join(".nvm/versions/node"), "lib/node_modules"), (home.join(".npm/_npx"), "node_modules")] {
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
    let at = parts.iter().rposition(|part| part.as_os_str() == "node_modules")?;
    let first = parts.get(at + 1)?;
    let take = if first.as_os_str().to_string_lossy().starts_with('@') { at + 3 } else { at + 2 };
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
                .args(["-p", "(version 1)(allow default)", "/bin/sh", "-c", "exit 0"])
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
                    .args(["--ro-bind", "/", "/", "--dev", "/dev", "--die-with-parent", "/bin/sh", "-c", "exit 0"])
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
    if !sandbox.os_enforce || crate::magician_v2::execution::coding_engine::coding_context_active() {
        return None;
    }
    if !available() {
        static WARNED: OnceLock<()> = OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!("[SHELL-BOX] shell_sandbox.os_enforce is on but no OS sandbox works here; shell runs with string checks only");
        });
        return None;
    }
    let plan = Plan::current(sandbox);
    launch(&plan, program, args)
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
        allowed_working_dirs: harness.writable.iter().map(|dir| dir.display().to_string()).collect(),
        os_enforce: true,
        ..ShellSandboxConfig::default()
    };
    let mut plan = Plan::current(&sandbox);
    plan.writable.extend(build_cache_dirs());
    plan.writable.sort();
    plan.writable.dedup();
    plan.writable_regex = harness.writable_regex.clone();
    plan.write_denied = harness_write_denied(&harness.engine_binary, &harness.deny_write);
    plan.exempt_agent(&harness.engine_binary);
    if harness.keychain_readable {
        plan.secret_dirs.retain(|dir| !dir.ends_with("Library/Keychains"));
    }
    launch(&plan, program, args)
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
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most
    // that many, NUL-terminated, and returns the length it needed.
    let needed = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
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
    /// Never writable, even under a writable root: emitted after every allow.
    write_denied: Vec<PathBuf>,
    /// Magician's own loopback listeners, not reachable directly.
    magician_ports: Vec<u16>,
}

fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// `engine` as a file: a bare name is looked up on `PATH`.
fn locate(engine: &Path) -> PathBuf {
    if engine.components().count() != 1 {
        return engine.to_path_buf();
    }
    std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|dir| dir.join(engine)).find(|candidate| candidate.is_file()))
        .unwrap_or_else(|| engine.to_path_buf())
}

#[cfg(any(target_os = "macos", test))]
fn sbpl_escape(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

impl Plan {
    fn current(sandbox: &ShellSandboxConfig) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().map(|home| resolved(&home));
        let mut writable: Vec<PathBuf> = sandbox
            .allowed_working_dirs
            .iter()
            .map(|dir| {
                let path = PathBuf::from(dir);
                resolved(&if path.is_absolute() { path } else { cwd.join(path) })
            })
            .collect();
        writable.push(resolved(Path::new("/tmp")));
        writable.extend(temp_dirs());
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

        let network = crate::magician_v2::execution::network_policy::mode();
        let proxy_port = match network {
            NetworkMode::Public => {
                let port = crate::magician_v2::execution::egress_proxy::port();
                if port.is_none() {
                    tracing::warn!("[SHELL-BOX] the egress proxy is not running; shell network is denied");
                }
                port
            },
            _ => None,
        };
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut nested_agent_dirs: Vec<(&'static str, PathBuf)> = home
            .iter()
            .flat_map(|home| NESTED_AGENT_HOME_DIRS.iter().map(move |(name, dir)| (*name, home.join(dir))))
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
            write_denied: Vec::new(),
            magician_ports: magician_listener_ports(),
        }
    }

    /// Env stripped from a boxed process: the credential and host-control
    /// sockets, outside `allow` (where their sockets are closed).
    fn env_remove(&self) -> Vec<String> {
        if self.network == NetworkMode::Allow {
            return Vec::new();
        }
        SOCKET_ENV.iter().map(|name| name.to_string()).collect()
    }

    /// Let `engine` (the harness being launched) run: drop it from the
    /// nested-agent denies by file, package, name and install directory.
    fn exempt_agent(&mut self, engine: &Path) {
        let file = resolved(&locate(engine));
        let package = package_root(&file);
        self.nested_agents.retain(|(executable, unreadable)| {
            *executable != file
                && Some(unreadable) != package.as_ref()
                && !package.as_ref().is_some_and(|package| executable.starts_with(package))
        });
        // The engine inside an agent's package or install directory is that
        // agent (`~/.local/share/claude/versions/<v>` is `claude`).
        let file_text = file.to_string_lossy().into_owned();
        let mut own: Vec<&'static str> = NESTED_AGENT_PACKAGES
            .iter()
            .filter(|(_, package)| file_text.contains(&format!("/node_modules/{package}/")))
            .map(|(name, _)| *name)
            .collect();
        own.extend(self.nested_agent_dirs.iter().filter(|(_, dir)| file.starts_with(dir)).map(|(name, _)| *name));
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
        self.nested_agent_names.retain(|name| !names.iter().any(|own| own == name));
        // A name on PATH that resolves into the engine's file or package is the
        // engine under another name.
        self.nested_agent_names.retain(|name| {
            !std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|dir| {
                    let candidate = dir.join(name);
                    candidate.is_file() && {
                        let real = resolved(&candidate);
                        real == file || package.as_ref().is_some_and(|package| real.starts_with(package))
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
        self.nested_agent_packages.retain(|(name, _)| !exempt.contains(name));
        self.nested_agent_dirs.retain(|(name, _)| !exempt.contains(name));
    }

    /// Env pointing a public-mode child at the egress proxy.
    fn proxy_env(&self) -> Vec<(String, String)> {
        let Some(port) = self.proxy_port.filter(|_| self.network == NetworkMode::Public) else {
            return Vec::new();
        };
        let url = crate::magician_v2::execution::egress_proxy::proxy_url(port);
        let mut env = Vec::new();
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
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
        let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write* (subpath \"/\"))\n");
        profile.push_str("(allow file-write* (subpath \"/dev\"))\n");
        for root in &self.writable {
            profile.push_str(&format!("(allow file-write* (subpath \"{}\"))\n", escape(root)));
        }
        for regex in &self.writable_regex {
            profile.push_str(&format!("(allow file-write* (regex #\"{regex}\"))\n"));
        }
        // After every write allow: last match wins.
        for path in &self.write_denied {
            profile.push_str(&format!("(deny file-write* (subpath \"{}\"))\n", escape(path)));
        }
        for dir in &self.secret_dirs {
            profile.push_str(&format!("(deny file-read* (subpath \"{}\"))\n", escape(dir)));
        }
        for file in &self.secret_files {
            profile.push_str(&format!("(deny file-read* (literal \"{}\"))\n", escape(file)));
        }
        profile.push_str(VAULT_FILE_READ_DENIES);
        if !self.nested_agent_names.is_empty() {
            profile.push_str(&format!(
                "(deny process-exec (regex #\"/({})(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))\n",
                self.nested_agent_names.join("|")
            ));
        }
        for (executable, unreadable) in &self.nested_agents {
            profile.push_str(&format!("(deny process-exec (literal \"{}\"))\n", escape(executable)));
            if unreadable.is_dir() {
                profile.push_str(&format!("(deny file-read* (subpath \"{}\"))\n", escape(unreadable)));
            } else {
                profile.push_str(&format!("(deny file-read* (literal \"{}\"))\n", escape(unreadable)));
            }
        }
        for (_, package) in &self.nested_agent_packages {
            let rule = format!("(regex #\"/node_modules/{package}(/|$)\")");
            profile.push_str(&format!("(deny process-exec {rule})\n(deny file-read* {rule})\n"));
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

    /// The SBPL network rules for the policy. Outside `allow`, unix sockets
    /// only under the writable roots and the system's logging and resolver.
    #[cfg(any(target_os = "macos", test))]
    fn network_rules(&self) -> String {
        if self.network == NetworkMode::Allow {
            return String::new();
        }
        let mut rules = String::from("(deny network-outbound (remote unix-socket))\n");
        for root in &self.writable {
            rules.push_str(&format!(
                "(allow network-outbound (remote unix-socket (subpath \"{}\")))\n",
                sbpl_escape(root)
            ));
        }
        for socket in ALLOWED_SYSTEM_SOCKETS {
            rules.push_str(&format!("(allow network-outbound (remote unix-socket (path-literal \"{socket}\")))\n"));
        }
        rules.push_str("(deny network-outbound (remote ip \"*:*\"))\n");
        if let (NetworkMode::Public, Some(proxy)) = (self.network, self.proxy_port) {
            // Direct: loopback only, but not Magician's own API. Everything
            // else must go through the proxy, whose allow comes last.
            rules.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
            for port in self.magician_ports.iter().filter(|port| **port != proxy) {
                rules.push_str(&format!("(deny network-outbound (remote ip \"localhost:{port}\"))\n"));
            }
            rules.push_str(&format!("(allow network-outbound (remote ip \"localhost:{proxy}\"))\n"));
        }
        rules
    }

    #[cfg(any(target_os = "linux", test))]
    fn bwrap_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
            .into_iter()
            .map(OsString::from)
            .collect();
        for root in self.writable.iter().filter(|root| root.exists()) {
            args.extend([OsString::from("--bind"), root.clone().into(), root.clone().into()]);
        }
        // After the binds: a later mount wins.
        for path in self.write_denied.iter().filter(|path| path.exists()) {
            args.extend([OsString::from("--ro-bind"), path.clone().into(), path.clone().into()]);
        }
        for dir in self.secret_dirs.iter().filter(|dir| dir.is_dir()) {
            args.extend([OsString::from("--tmpfs"), dir.clone().into()]);
        }
        for file in self.secret_files.iter().filter(|file| file.is_file()) {
            args.extend([OsString::from("--ro-bind"), OsString::from("/dev/null"), file.clone().into()]);
        }
        for (executable, unreadable) in &self.nested_agents {
            if unreadable.is_dir() {
                args.extend([OsString::from("--tmpfs"), unreadable.clone().into()]);
            } else if executable.is_file() {
                args.extend([OsString::from("--ro-bind"), OsString::from("/dev/null"), executable.clone().into()]);
            }
        }
        for (_, dir) in self.nested_agent_dirs.iter().filter(|(_, dir)| dir.is_dir()) {
            args.extend([OsString::from("--tmpfs"), dir.clone().into()]);
        }
        for name in self.env_remove() {
            args.extend([OsString::from("--unsetenv"), OsString::from(name)]);
        }
        if self.network == NetworkMode::Deny || (self.network == NetworkMode::Public && self.proxy_port.is_none()) {
            args.push(OsString::from("--unshare-net"));
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
            write_denied: Vec::new(),
            magician_ports: vec![3002],
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
        assert!(profile.contains("(deny process-exec (literal \"/n/lib/node_modules/@openai/codex/bin/codex.js\"))"));
    }

    #[test]
    fn the_profile_orders_denies_after_allows() {
        let profile = plan(NetworkMode::Public, Some(4242)).macos_profile();
        let deny_all = profile.find("(deny file-write* (subpath \"/\"))").unwrap();
        let allow_work = profile.find("(allow file-write* (subpath \"/work/space\"))").unwrap();
        assert!(deny_all < allow_work, "last match wins: the allow comes after the blanket deny");
        assert!(profile.contains("(deny file-read* (subpath \"/Users/o/.ssh\"))"));
        assert!(profile.contains("(deny file-read* (literal \"/Users/o/MagicianNotes/.env\"))"));
        assert!(profile.contains(
            "(deny process-exec (regex #\"/(claude|codex|pi|grok|agy|gemini)(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))"
        ));
        let deny_net = profile.find("(deny network-outbound (remote ip \"*:*\"))").unwrap();
        let allow_local = profile.find("(allow network-outbound (remote ip \"localhost:*\"))").unwrap();
        assert!(deny_net < allow_local, "loopback (and so the proxy) is reachable, nothing else directly");
    }

    #[test]
    fn network_rules_follow_the_policy() {
        assert!(!plan(NetworkMode::Allow, None).macos_profile().contains("network-outbound"));
        let deny = plan(NetworkMode::Deny, None).macos_profile();
        assert!(
            deny.contains("(deny network-outbound (remote ip \"*:*\"))")
                && !deny.contains("(allow network-outbound (remote ip")
        );
        let no_proxy = plan(NetworkMode::Public, None).macos_profile();
        assert!(!no_proxy.contains("(allow network-outbound (remote ip"), "no proxy: no network rather than open");
        let env = plan(NetworkMode::Public, Some(9)).proxy_env();
        assert!(env.iter().any(|(k, v)| k == "HTTPS_PROXY" && v == "http://127.0.0.1:9"));
        if std::env::var_os("PIP_CERT").is_none() && Path::new(SYSTEM_CA_BUNDLE).is_file() {
            assert!(
                env.iter().any(|(k, v)| k == "PIP_CERT" && v == SYSTEM_CA_BUNDLE),
                "proxied pip verifies against the system roots"
            );
        }
        assert!(env.iter().any(|(k, v)| k == "NO_PROXY" && v.contains("127.0.0.1")), "loopback goes direct");
        assert!(plan(NetworkMode::Allow, Some(9)).proxy_env().is_empty());
    }

    /// Codex's shells reset `TMPDIR`, so `mktemp` falls back to the per-user
    /// temp dir; Magician's own `TMPDIR` may be elsewhere. Both are writable.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_os_user_temp_dir_is_writable_whatever_tmpdir_says() {
        let user_temp = resolved(&darwin_user_temp_dir().expect("macOS gives every user a temp dir"));
        let temps = temp_dirs();
        assert!(temps.contains(&user_temp) && temps.contains(&resolved(&std::env::temp_dir())));
        if user_temp.file_name().is_some_and(|name| name == "T") {
            assert!(build_cache_dirs().contains(&user_temp.parent().unwrap().join("C")));
        }
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
        assert_eq!(plan.nested_agents.len(), 1, "codex is exempt, claude is not");
        assert!(plan.nested_agents[0].0.ends_with("claude.exe"));
        assert!(!plan.nested_agent_names.contains(&"codex"));
        assert!(plan.nested_agent_names.contains(&"claude"));
        plan.writable_regex.push("^/Users/o/\\.claude\\.json".to_string());
        let profile = plan.macos_profile();
        assert!(profile.contains("(allow file-write* (regex #\"^/Users/o/\\.claude\\.json\"))"));
        assert!(profile.contains(
            "(deny process-exec (regex #\"/(claude|pi|grok|agy|gemini)(\\.exe|\\.js|\\.mjs|\\.cjs)?$\"))"
        ));
        assert!(!profile.contains("@openai/codex"), "nothing denies the engine's own package");
        assert!(profile.contains("(deny process-exec (regex #\"/node_modules/@anthropic-ai/claude-code(/|$)\"))"));
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
            std::process::Command::new("chmod").args(["+x"]).arg(&path).status().unwrap();
            resolved(&path)
        };
        let (engine, other) = (script("engine-cli"), script("other-cli"));
        let mut plan = Plan {
            writable: vec![resolved(&std::env::temp_dir())],
            secret_dirs: vec![],
            secret_files: vec![],
            network: NetworkMode::Allow,
            proxy_port: None,
            nested_agents: vec![(engine.clone(), engine.clone()), (other.clone(), other.clone())],
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs: Vec::new(),
            writable_regex: vec![format!(
                "^{}/state\\.json",
                regex::escape(&resolved(agents.path()).to_string_lossy())
            )],
            write_denied: Vec::new(),
            magician_ports: Vec::new(),
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
        assert!(!run(&other.display().to_string()), "another agent CLI does not");
        let dir = resolved(agents.path());
        assert!(run(&format!("echo s > {}/state.json", dir.display())), "its own state file is writable");
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
                &HarnessBox { keychain_readable, ..HarnessBox::default() },
            )
            .expect("boxed");
            boxed.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
        };
        let deny = "Library/Keychains\"))";
        assert!(profile_of(false).contains(deny), "the default box denies the keychain");
        assert!(!profile_of(true).contains(deny), "a keychain harness may read it");
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
                std::process::Command::new("chmod").args(["+x"]).arg(&agent).status().unwrap();
                let agent = resolved(&agent);
                vec![(agent.clone(), agent)]
            },
            nested_agent_names: NESTED_AGENT_NAMES.to_vec(),
            nested_agent_packages: NESTED_AGENT_PACKAGES.to_vec(),
            nested_agent_dirs: Vec::new(),
            writable_regex: Vec::new(),
            write_denied: Vec::new(),
            magician_ports: Vec::new(),
        };
        let run = |command: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &plan.macos_profile(), "/bin/sh", "-c", command])
                .status()
                .unwrap()
                .success()
        };
        assert!(run(&format!("echo x > {}/in.txt", work.path().display())));
        assert!(!run(&format!("echo x > {}/out.txt", outside.path().display())));
        assert!(!outside.path().join("out.txt").exists());
        assert!(!run(&format!("cat {}/key", secret.path().display())));
        assert!(!run(&format!("{}/fake-agent", resolved(secret.path()).display())), "a nested agent cannot start");
        assert!(!run("/usr/bin/curl -s -m 3 http://127.0.0.1:9/ >/dev/null 2>&1 && exit 0 || exit 1"));
    }

    #[test]
    fn build_caches_leave_executable_dirs_unwritable() {
        let caches = build_cache_dirs();
        let Some(home) = dirs::home_dir().map(|home| resolved(&home)) else {
            return;
        };
        for executable in [".cargo/bin", ".pyenv/shims", ".deno", ".rustup", ".yarn", ".gradle", ".m2", "Library/pnpm"] {
            assert!(!caches.contains(&home.join(executable)), "{executable} holds executables that run unboxed later");
        }
        assert!(caches.contains(&home.join(".rustup/downloads")) && caches.contains(&home.join(".m2/repository")));
    }

    #[test]
    fn harness_write_denials_come_after_every_allow() {
        let denied = harness_write_denied(
            Path::new("/n/lib/node_modules/@openai/codex/bin/codex.js"),
            &[PathBuf::from("/private/tmp/real-repo-under-temp")],
        );
        assert!(denied.contains(&PathBuf::from("/n/lib/node_modules/@openai/codex")), "the engine's package");
        assert!(denied.contains(&PathBuf::from("/n/lib/node_modules/@openai/codex/bin/codex.js")));
        assert!(denied.contains(&PathBuf::from("/private/tmp/real-repo-under-temp")), "the real repo, though under temp");
        if let Some(home) = dirs::home_dir().map(|home| resolved(&home)) {
            assert!(denied.contains(&home.join(".local/share/claude/versions")));
            assert!(denied.contains(&home.join(".claude/local")));
            assert!(denied.contains(&home.join(".grok/bin")));
        }
        let mut plan = plan(NetworkMode::Allow, None);
        plan.writable_regex.push("^/Users/o/\\.claude\\.json".to_string());
        plan.write_denied = denied;
        let profile = plan.macos_profile();
        let allow_tmp = profile.find("(allow file-write* (subpath \"/private/tmp\"))").unwrap();
        let allow_regex = profile.find("(allow file-write* (regex").unwrap();
        let deny_repo = profile.find("(deny file-write* (subpath \"/private/tmp/real-repo-under-temp\"))").unwrap();
        assert!(allow_tmp < deny_repo && allow_regex < deny_repo, "last match wins: the deny follows every allow");
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
        let ro = args.iter().position(|arg| arg.as_os_str() == repo.as_os_str()).unwrap();
        assert!(bind < ro && args[ro - 1] == "--ro-bind");
        assert!(args.windows(2).any(|pair| pair[0] == "--unsetenv" && pair[1] == "SSH_AUTH_SOCK"));
    }

    #[test]
    fn nested_agents_are_denied_off_path_too() {
        let profile = plan(NetworkMode::Allow, None).macos_profile();
        for package in ["@anthropic-ai/claude-code", "@openai/codex", "@earendil-works/pi-coding-agent", "@google/gemini-cli"] {
            assert!(profile.contains(&format!("(deny process-exec (regex #\"/node_modules/{package}(/|$)\"))")));
            assert!(profile.contains(&format!("(deny file-read* (regex #\"/node_modules/{package}(/|$)\"))")));
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
        assert!(!codex.nested_agent_names.contains(&"codex"), "an .mjs entry is the agent by name");
        assert!(!codex.macos_profile().contains("@openai/codex(/|$)"));
    }

    #[test]
    fn unix_sockets_close_outside_allow() {
        assert!(!plan(NetworkMode::Allow, None).macos_profile().contains("unix-socket"));
        assert!(plan(NetworkMode::Allow, None).env_remove().is_empty());
        for (network, proxy) in [(NetworkMode::Deny, None), (NetworkMode::Public, Some(9))] {
            let plan = plan(network, proxy);
            let profile = plan.macos_profile();
            let deny = profile.find("(deny network-outbound (remote unix-socket))").unwrap();
            let reallow = profile
                .find("(allow network-outbound (remote unix-socket (subpath \"/work/space\")))")
                .unwrap();
            assert!(deny < reallow, "sockets under a writable root stay reachable");
            assert!(profile.contains(
                "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))"
            ));
            assert!(plan.env_remove().iter().any(|name| name == "SSH_AUTH_SOCK"));
            assert!(plan.env_remove().iter().any(|name| name == "DOCKER_HOST"));
        }
        let prefix = env_strip_prefix(&["SSH_AUTH_SOCK".to_string()], OsStr::new("/bin/sh"));
        assert_eq!(prefix, ["/usr/bin/env", "-u", "SSH_AUTH_SOCK"].map(OsString::from).to_vec());
        assert!(env_strip_prefix(&[], OsStr::new("/bin/sh")).is_empty());
        assert!(env_strip_prefix(&["X".to_string()], OsStr::new("/odd/a=b")).is_empty());
    }

    #[test]
    fn public_loopback_skips_magicians_own_api() {
        let profile = plan(NetworkMode::Public, Some(4242)).macos_profile();
        let allow_local = profile.find("(allow network-outbound (remote ip \"localhost:*\"))").unwrap();
        let deny_api = profile.find("(deny network-outbound (remote ip \"localhost:3002\"))").unwrap();
        let allow_proxy = profile.find("(allow network-outbound (remote ip \"localhost:4242\"))").unwrap();
        assert!(allow_local < deny_api && deny_api < allow_proxy, "the API is denied after loopback, the proxy allowed last");
        let same = plan(NetworkMode::Public, Some(3002));
        assert!(
            !same.macos_profile().contains("(deny network-outbound (remote ip \"localhost:3002\"))"),
            "never deny the proxy itself"
        );
        assert!(!plan(NetworkMode::Deny, None).macos_profile().contains("localhost:"));
    }

    #[test]
    fn registered_listeners_replace_the_default_api_port() {
        register_magician_listener(43_210);
        register_magician_listener(43_210);
        let ports = magician_listener_ports();
        assert_eq!(ports.iter().filter(|port| **port == 43_210).count(), 1);
        assert!(!ports.contains(&DEFAULT_MAGICIAN_API_PORT));
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
        assert!(connect(&inside_socket), "a socket under a writable root is reachable");
        assert!(!connect(&outside_socket), "an agent or Docker socket elsewhere is not");

        let boxed = launch(
            &plan,
            OsStr::new("/bin/sh"),
            &[OsString::from("-c"), OsString::from("test -z \"$SSH_AUTH_SOCK\"")],
        )
        .unwrap();
        assert_eq!(boxed.env_remove.first().map(String::as_str), Some("SSH_AUTH_SOCK"));
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
                .args(["-p", &profile, "/usr/bin/nc", "-z", "-n", "-w", "2", "127.0.0.1", &port.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(!reach(port(&api)), "Magician's API is not reachable from the box");
        assert!(reach(port(&proxy)), "the egress proxy is");
        assert!(reach(port(&other)), "the owner's own loopback servers are");
    }
}
