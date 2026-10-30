//! The macOS desktop tray app ("Magican") as a supervised service.
//!
//! On by default (`supervisor.manage_desktop` in the runtime
//! `magician-config.yaml`, read once at start; `--manage-desktop=0|1` wins),
//! macOS-only, and active only when a bundle is known (`--desktop-app`).
//! Unlike the other services the
//! app is never a child of the supervisor: it is launched through
//! LaunchServices (`/usr/bin/open -n -g ... <bundle>`), exactly like
//! `make run-desktop-tray-debug`, because macOS attributes TCC grants
//! (Accessibility, Screen Recording, Microphone) to the responsible process.
//! A supervisor child would silently lose them. The cost is that the
//! supervisor gets no exit status, so it watches instead:
//!
//! - **Discovery.** Every tick lists processes (libproc) whose executable is
//!   the bundle's `Contents/MacOS/<CFBundleExecutable>`, owned by this user.
//!   A new pid for the same path (the tray's in-app "Restart Magican") is
//!   still running, never a crash.
//! - **Crash vs quit.** The tray's Quit writes the clean-quit marker before
//!   exiting; `stop-desktop` and `make stop-desktop-tray` write it too. When
//!   the watched process is gone, a marker newer than that process's start
//!   (or the supervisor's launch) means a deliberate quit and nothing is
//!   relaunched. No such marker means a crash.
//! - **Relaunch.** After a crash the app is relaunched with a backoff
//!   (2 s, 5 s, 15 s, 60 s), at most `desktop_max_relaunches` times per
//!   `desktop_relaunch_window`; past that the supervisor gives up and reports
//!   `unhealthy` until an operator `start-desktop` / `restart-desktop` or a
//!   manual launch of the app. The process list is read again when the
//!   backoff expires, so an app that came back on its own is adopted, not
//!   doubled.
//!
//! The marker is a fixed per-user file independent of the runtime root,
//! `~/Library/Application Support/dev.magician.desktop/desktop-app.quit` (the
//! desktop app's own support dir), so an instance launched any way (Finder,
//! login item, bare `open`) writes where the supervisor watches.
//! `--desktop-quit-marker` overrides it, and every supervisor launch passes
//! the watched path to the app as `MAGICIAN_DESKTOP_QUIT_MARKER`.
//!
//! The desktop mutex is never held while the preflight script, `open` or the
//! stop grace runs: the decision is taken under the lock, the slow part runs
//! without it (an in-flight flag refuses a second launch meanwhile), and the
//! result is recorded under the lock again. Timers use `Instant`; wall-clock
//! time only compares the marker's mtime with the process start time.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::{ProcessStatus, SupervisorConfig};

/// File name of the clean-quit marker. The desktop app writes the same file
/// (`desktop/src-tauri/src/config.rs`, `desktop_quit_marker_path`).
pub const QUIT_MARKER_FILE: &str = "desktop-app.quit";
/// The desktop app's per-user support dir, relative to `$HOME` (its
/// `manifest_dir()` on macOS).
const DESKTOP_SUPPORT_DIR: &str = "Library/Application Support/dev.magician.desktop";
/// Env the app reads for a non-default marker path.
pub const QUIT_MARKER_ENV: &str = "MAGICIAN_DESKTOP_QUIT_MARKER";
/// The bundle executable when `Info.plist` cannot be read.
const DEFAULT_EXECUTABLE: &str = "magician-desktop.bin";
/// Relaunch delays after the 1st, 2nd, 3rd and later crashes in the window.
pub const RELAUNCH_BACKOFF_SECS: [u64; 4] = [2, 5, 15, 60];
/// A launch whose process never appears within this is counted as a crash.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);
/// Preflight and `open` limits; together they bound an in-flight launch.
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(120);
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// An in-flight launch older than this is treated as abandoned, so a lost
/// result can never block launches for good.
const IN_FLIGHT_LIMIT: Duration = Duration::from_secs(180);
/// At supervisor boot, how long a not-yet-running app is waited for before
/// the supervisor launches it, so the app's own login item starts it and no
/// duplicate is opened.
pub const BOOT_GRACE: Duration = Duration::from_secs(30);
/// The app's login item: tauri-plugin-autostart (`MacosLauncher::LaunchAgent`,
/// via auto-launch) writes `~/Library/LaunchAgents/<productName>.plist` when
/// "launch at login" is on and deletes it when off. productName is "Magican".
const LOGIN_ITEM_PLIST: &str = "Library/LaunchAgents/Magican.plist";
/// SIGTERM grace before SIGKILL on `stop-desktop`.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How long to wait for the processes to vanish after SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
const OPEN_PROGRAM: &str = "/usr/bin/open";
const BASH_PROGRAM: &str = "/bin/bash";

// ---------------------------------------------------------------------------
// Command-line flags
// ---------------------------------------------------------------------------

/// Apply the server-mode desktop flags to `config` and return any argument
/// that is not one of them, so the caller can warn about it.
///
/// `--manage-desktop[=1|0]` (explicit; otherwise the runtime config
/// decides), `--magician-config <path>` (the runtime `magician-config.yaml`
/// holding `supervisor.manage_desktop`), `--desktop-app <bundle>`,
/// `--desktop-log <path>`, `--desktop-env KEY=VALUE` (repeatable),
/// `--desktop-verify-script <path>`, `--desktop-quit-marker <path>`.
/// `--desktop-launch-at-boot=always|login-item` (default `login-item`).
/// `--flag=value` works too; an empty path value means unset, and an empty
/// `--manage-desktop=` leaves the decision to the config.
#[cfg_attr(not(test), allow(dead_code))]
pub fn apply_cli_args(config: &mut SupervisorConfig, args: &[String]) -> Result<Vec<String>> {
    let mut ignored = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !apply_flag(config, arg, &mut rest)? {
            ignored.push(arg.clone());
        }
    }
    Ok(ignored)
}

/// Server mode: like [`apply_cli_args`], but a bad value or a missing value
/// for a known flag is a warning and that setting keeps its default, so a
/// future or mistyped value never crash-loops the supervisor under launchd
/// KeepAlive. Returns (unknown arguments, warnings).
pub fn apply_cli_args_lenient(
    config: &mut SupervisorConfig,
    args: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut ignored = Vec::new();
    let mut warnings = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match apply_flag(config, arg, &mut rest) {
            Ok(true) => {},
            Ok(false) => ignored.push(arg.clone()),
            Err(error) => warnings.push(format!("{arg}: {error:#}; keeping the default")),
        }
    }
    (ignored, warnings)
}

/// Apply one argument; `Ok(false)` when it is not a desktop flag. A setting
/// is assigned only after its value parsed, so an error leaves it as it was.
fn apply_flag(
    config: &mut SupervisorConfig,
    arg: &str,
    rest: &mut std::slice::Iter<'_, String>,
) -> Result<bool> {
    let (flag, inline) = match arg.split_once('=') {
        Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
        _ => (arg, None),
    };
    match flag {
        "--manage-desktop" => {
            config.desktop_manage = match inline {
                // `--manage-desktop 0|1|true|false` (space-separated) takes
                // the next token only when it is a boolean; bare = on.
                None => match rest.as_slice().first().and_then(|next| parse_bool(next)) {
                    Some(value) => {
                        rest.next();
                        Some(value)
                    },
                    None => Some(true),
                },
                Some(value) if value.trim().is_empty() => None,
                Some(value) => Some(
                    parse_bool(value)
                        .with_context(|| format!("--manage-desktop={value} is not a boolean"))?,
                ),
            };
        },
        "--magician-config" => {
            config.magician_config = optional_path(take_value(flag, inline, rest)?)
        },
        "--desktop-launch-at-boot" => {
            config.desktop_launch_at_boot =
                parse_boot_launch_policy(&take_value(flag, inline, rest)?)?
        },
        "--desktop-app" => config.desktop_app = optional_path(take_value(flag, inline, rest)?),
        "--desktop-log" => config.desktop_log = optional_path(take_value(flag, inline, rest)?),
        "--desktop-verify-script" => {
            config.desktop_verify_script = optional_path(take_value(flag, inline, rest)?)
        },
        "--desktop-quit-marker" => {
            config.desktop_quit_marker = optional_path(take_value(flag, inline, rest)?)
        },
        "--desktop-env" => {
            let pair = take_value(flag, inline, rest)?;
            config.desktop_env.push(parse_env_pair(&pair)?);
        },
        _ => return Ok(false),
    }
    Ok(true)
}

fn take_value(
    flag: &str,
    inline: Option<&str>,
    rest: &mut std::slice::Iter<'_, String>,
) -> Result<String> {
    match inline {
        Some(value) => Ok(value.to_string()),
        None => rest
            .next()
            .cloned()
            .with_context(|| format!("{flag} needs a value")),
    }
}

fn optional_path(value: String) -> Option<PathBuf> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `KEY=VALUE` with a shell-style key; the value may be empty (the Makefile
/// passes `MAGICIAN_PREFIX=` when it is unset, as `open --env` does).
pub fn parse_env_pair(pair: &str) -> Result<(String, String)> {
    let Some((key, value)) = pair.split_once('=') else {
        anyhow::bail!("--desktop-env expects KEY=VALUE, got {pair:?}");
    };
    let mut chars = key.chars();
    let valid = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    anyhow::ensure!(valid, "--desktop-env key {key:?} is not a valid name");
    Ok((key.to_string(), value.to_string()))
}

// ---------------------------------------------------------------------------
// `supervisor.manage_desktop` from the runtime config, and the decision
// ---------------------------------------------------------------------------

/// What the runtime config said about `supervisor.manage_desktop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSetting {
    /// No `--magician-config` was given.
    NotGiven,
    /// The file could not be read.
    Unreadable(String),
    /// The section could not be understood.
    Unparseable(String),
    /// The file has no `supervisor.manage_desktop`.
    Absent,
    Value(bool),
}

fn parse_yaml_bool(value: &str) -> Result<bool, String> {
    let unquoted = value.trim().trim_matches(|c: char| c == '"' || c == '\'');
    match unquoted.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Ok(true),
        "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "supervisor.manage_desktop: {value:?} is not a boolean"
        )),
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }
    match line.find(" #") {
        Some(index) => &line[..index],
        None => line,
    }
}

/// Lenient read of `supervisor.manage_desktop` only: every other key and
/// section is ignored, so the supervisor never breaks on a config the
/// backend accepts. Block (`supervisor:` then an indented key) and flow
/// (`supervisor: {manage_desktop: false}`) styles are understood.
pub fn parse_manage_desktop(yaml: &str) -> Result<Option<bool>, String> {
    let mut in_section = false;
    for raw in yaml.lines() {
        let line = strip_yaml_comment(raw).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            in_section = false;
            let Some(rest) = line.strip_prefix("supervisor:") else {
                continue;
            };
            let rest = rest.trim();
            if rest.is_empty() {
                in_section = true;
                continue;
            }
            let inner = rest
                .strip_prefix('{')
                .and_then(|inner| inner.strip_suffix('}'))
                .ok_or_else(|| format!("unexpected `supervisor:` value {rest:?}"))?;
            for item in inner.split(',') {
                if let Some((key, value)) = item.split_once(':') {
                    if key.trim() == "manage_desktop" {
                        return parse_yaml_bool(value).map(Some);
                    }
                }
            }
            return Ok(None);
        }
        if in_section {
            if let Some((key, value)) = line.trim().split_once(':') {
                if key.trim() == "manage_desktop" {
                    return parse_yaml_bool(value).map(Some);
                }
            }
        }
    }
    Ok(None)
}

pub fn read_manage_desktop(path: Option<&Path>) -> ConfigSetting {
    let Some(path) = path else {
        return ConfigSetting::NotGiven;
    };
    match std::fs::read_to_string(path) {
        Err(error) => ConfigSetting::Unreadable(format!("{}: {error}", path.display())),
        Ok(text) => match parse_manage_desktop(&text) {
            Ok(Some(value)) => ConfigSetting::Value(value),
            Ok(None) => ConfigSetting::Absent,
            Err(error) => ConfigSetting::Unparseable(format!("{}: {error}", path.display())),
        },
    }
}

/// Whether the desktop is watched, and why (shown in status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Management {
    pub enabled: bool,
    pub reason: String,
}

/// Precedence: explicit `--manage-desktop` > `supervisor.manage_desktop` >
/// on. Active only on macOS and with a known bundle.
pub fn decide_management(
    supported: bool,
    cli: Option<bool>,
    setting: &ConfigSetting,
    bundle_known: bool,
) -> Management {
    let off = |reason: String| Management {
        enabled: false,
        reason,
    };
    if !supported {
        return off("unsupported on this OS (macOS only)".to_string());
    }
    let (wanted, source) = match (cli, setting) {
        (Some(value), _) => (value, "--manage-desktop".to_string()),
        (None, ConfigSetting::Value(value)) => (
            *value,
            "supervisor.manage_desktop in the runtime config".to_string(),
        ),
        (None, ConfigSetting::NotGiven | ConfigSetting::Absent) => (true, "default".to_string()),
        (None, ConfigSetting::Unreadable(why) | ConfigSetting::Unparseable(why)) => {
            (true, format!("default (config not usable: {why})"))
        },
    };
    if !wanted {
        return off(format!("disabled by {source}"));
    }
    if !bundle_known {
        return off("no desktop app bundle (--desktop-app)".to_string());
    }
    Management {
        enabled: true,
        reason: format!("enabled ({source})"),
    }
}

/// What the supervisor does when the boot grace ends with the app still not
/// running (`--desktop-launch-at-boot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootLaunchPolicy {
    /// Launch only when the app's own login item is enabled, so "launch at
    /// login" off is respected (production default).
    #[default]
    LoginItem,
    /// Always launch (dev: `make run-supervisor` has no login item).
    Always,
}

pub fn parse_boot_launch_policy(value: &str) -> Result<BootLaunchPolicy> {
    match value.trim() {
        "login-item" => Ok(BootLaunchPolicy::LoginItem),
        "always" => Ok(BootLaunchPolicy::Always),
        other => {
            anyhow::bail!("--desktop-launch-at-boot must be always or login-item, got {other:?}")
        },
    }
}

/// The login item counts as enabled when its plist exists and is not
/// marked `Disabled`.
pub fn login_item_enabled_from(plist: Option<&str>) -> bool {
    let Some(plist) = plist else {
        return false;
    };
    let compact: String = plist.split_whitespace().collect();
    !compact.contains("<key>Disabled</key><true/>")
}

fn login_item_enabled(home: Option<&Path>) -> bool {
    let contents = home.and_then(|home| std::fs::read_to_string(home.join(LOGIN_ITEM_PLIST)).ok());
    login_item_enabled_from(contents.as_deref())
}

/// Whether a launch due at the end of the boot grace proceeds: an operator
/// start that another instance overtook always does; a boot launch follows
/// the policy.
pub fn boot_launch_proceeds(
    policy: BootLaunchPolicy,
    login_item_enabled: bool,
    operator_start_pending: bool,
) -> bool {
    operator_start_pending || boot_launch_allowed(policy, login_item_enabled)
}

/// Whether the boot grace ends in a launch.
pub fn boot_launch_allowed(policy: BootLaunchPolicy, login_item_enabled: bool) -> bool {
    match policy {
        BootLaunchPolicy::Always => true,
        BootLaunchPolicy::LoginItem => login_item_enabled,
    }
}

/// Executable names of the desktop app in any build (release bundle, debug
/// bundle), so another copy is recognised wherever it runs from.
const DESKTOP_BINARY_NAMES: [&str; 2] = ["magician-desktop", "magician-desktop.bin"];
const DEFAULT_GATEWAY_PORT: u16 = 3017;

/// The host gateway port the app binds: from `MAGICIAN_HOST_GATEWAY_URL` in
/// the app's env, else the supervisor's own, else 3017.
pub fn gateway_port(desktop_env: &[(String, String)], process_env: Option<&str>) -> u16 {
    let url = desktop_env
        .iter()
        .find(|(key, _)| key == "MAGICIAN_HOST_GATEWAY_URL")
        .map(|(_, value)| value.as_str())
        .or(process_env);
    url.and_then(|url| {
        let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
        let host_port = authority.split('/').next()?;
        host_port.rsplit_once(':')?.1.parse::<u16>().ok()
    })
    .unwrap_or(DEFAULT_GATEWAY_PORT)
}

fn port_in_use(port: u16) -> bool {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// Another running instance of the desktop app that is not `executable`
/// (another bundle or worktree build of this user), or anything holding the
/// host gateway port. Call only when `executable` itself is not running.
pub fn other_instance(executable: &Path, port: u16) -> Option<String> {
    let mut names: Vec<std::ffi::OsString> = DESKTOP_BINARY_NAMES
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    if let Some(own) = executable.file_name() {
        names.push(own.to_os_string());
    }
    if let Some((app, path)) = platform::find_named(&names)
        .into_iter()
        .find(|(_, path)| !same_file_path(path, executable))
    {
        return Some(format!("PID {} ({})", app.pid, path.display()));
    }
    port_in_use(port).then(|| format!("the host gateway port {port} is in use"))
}

// ---------------------------------------------------------------------------
// Pure helpers: launch arguments, marker path, bundle executable
// ---------------------------------------------------------------------------

/// Arguments for `/usr/bin/open`, mirroring `make run-desktop-tray-debug`:
/// a new instance (`-n`), in the background (`-g`), output to the tray log,
/// env passed through `--env`, then the bundle.
pub fn open_args(bundle: &Path, log: Option<&Path>, env: &[(String, String)]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-n".into(), "-g".into()];
    if let Some(log) = log {
        args.extend([
            "--stdout".into(),
            log.as_os_str().to_owned(),
            "--stderr".into(),
            log.as_os_str().to_owned(),
        ]);
    }
    for (key, value) in env {
        args.push("--env".into());
        args.push(format!("{key}={value}").into());
    }
    args.push(bundle.as_os_str().to_owned());
    args
}

/// The configured env plus `MAGICIAN_DESKTOP_QUIT_MARKER=<marker>`, so the
/// app writes the file the supervisor watches; the watched path wins over a
/// conflicting `--desktop-env` value.
pub fn launch_env(env: &[(String, String)], marker: &Path) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = env
        .iter()
        .filter(|(key, _)| key != QUIT_MARKER_ENV)
        .cloned()
        .collect();
    merged.push((QUIT_MARKER_ENV.to_string(), marker.display().to_string()));
    merged
}

/// Where the clean-quit marker lives: the explicit path, else the fixed
/// per-user file in the desktop app's support dir under `home`.
pub fn quit_marker_path(explicit: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    home.map(|home| home.join(DESKTOP_SUPPORT_DIR).join(QUIT_MARKER_FILE))
}

/// The `<string>` after `<key>{key}</key>` in an XML plist.
fn plist_string_value(plist: &str, key: &str) -> Option<String> {
    let after_key = plist.split_once(&format!("<key>{key}</key>"))?.1;
    let after_open = after_key.trim_start().strip_prefix("<string>")?;
    let value = after_open.split_once("</string>")?.0.trim();
    (!value.is_empty() && !value.contains('/')).then(|| value.to_string())
}

/// `<bundle>/Contents/MacOS/<CFBundleExecutable>`, defaulting the name when
/// the plist is unreadable (binary or mid-rebuild).
pub fn bundle_executable(bundle: &Path) -> PathBuf {
    let name = std::fs::read_to_string(bundle.join("Contents").join("Info.plist"))
        .ok()
        .and_then(|plist| plist_string_value(&plist, "CFBundleExecutable"))
        .unwrap_or_else(|| DEFAULT_EXECUTABLE.to_string());
    bundle.join("Contents").join("MacOS").join(name)
}

fn backoff_for(attempts_in_window: u32) -> Duration {
    let index = (attempts_in_window as usize).min(RELAUNCH_BACKOFF_SECS.len() - 1);
    Duration::from_secs(RELAUNCH_BACKOFF_SECS[index])
}

/// A marker counts as a deliberate quit of a run only when it was written
/// after that run began (process start, or the supervisor's launch). An
/// older marker is left over from an earlier quit or stop.
pub fn marker_is_fresh(marker: Option<SystemTime>, run_began: SystemTime) -> bool {
    marker.is_some_and(|written| written >= run_began)
}

fn same_file_path(found: &Path, wanted: &Path) -> bool {
    if found == wanted {
        return true;
    }
    match (std::fs::canonicalize(found), std::fs::canonicalize(wanted)) {
        (Ok(found), Ok(wanted)) => found == wanted,
        _ => false,
    }
}

/// The newest `<executable>-*.ips` crash report written at or after `since`.
fn find_crash_report(dir: &Path, executable_name: &str, since: SystemTime) -> Option<PathBuf> {
    let prefix = format!("{executable_name}-");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !(name.starts_with(&prefix) && name.ends_with(".ips")) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            (modified >= since).then(|| (modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn write_marker(path: &Path, note: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{note}\n"))
}

fn remove_marker(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => info!("Removed desktop clean-quit marker {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => warn!(
            "Could not remove desktop clean-quit marker {}: {}",
            path.display(),
            error
        ),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|dir| dir.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn unix_seconds(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0).map(|t| t.to_rfc3339())
}

// ---------------------------------------------------------------------------
// The supervision state machine (pure: no processes, no files)
// ---------------------------------------------------------------------------

/// One reading of both clocks: `mono` for timers, `wall` only for comparing
/// with file mtimes and process start times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    pub mono: Instant,
    pub wall: SystemTime,
}

impl Now {
    pub fn current() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// One running instance of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedApp {
    pub pid: i32,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleReason {
    /// Nothing launched or adopted yet (or an operator start failed).
    NotStarted,
    /// `stop-desktop`.
    OperatorStop,
    /// A fresh clean-quit marker: the tray's Quit, or a deliberate stop
    /// from outside the supervisor (`make stop-desktop-tray`).
    QuitMarker,
    /// The boot grace ended without the app and its login item is off: the
    /// owner chose not to launch it at login. Crash relaunch still applies
    /// once they start it.
    LoginItemOff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Not watched for crashes.
    Idle(IdleReason),
    /// Supervisor boot with the app not running: wait until `until` for the
    /// app's own login item to start it (adopted as soon as it appears),
    /// then launch it. `since_wall` dates a marker written meanwhile.
    BootGrace {
        until: Instant,
        since_wall: SystemTime,
    },
    Running(ObservedApp),
    /// A launch was decided at `since` (`since_wall` for marker freshness);
    /// waiting for the process to appear.
    Launching {
        since: Instant,
        since_wall: SystemTime,
    },
    /// The app died without a fresh marker; relaunch once `due` passes.
    RelaunchPending {
        crashed_wall: SystemTime,
        due: Instant,
    },
    /// The relaunch budget is spent. An operator start, or the app showing
    /// up again (a manual launch), leaves this state.
    GaveUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickAction {
    None,
    /// First sighting of a process, or a new pid (in-app restart).
    Adopted {
        app: ObservedApp,
        previous_pid: Option<i32>,
    },
    /// The app is gone and a fresh marker says it was meant to go.
    Quit,
    /// The app is gone without a fresh marker.
    CrashScheduled {
        relaunch_in: Duration,
        attempts_in_window: u32,
    },
    /// The backoff expired with the app still gone: launch it now.
    Launch,
    /// The boot grace expired with the app still not running: launch it as
    /// an operator start would.
    BootLaunch,
    /// A launch is due but another instance runs: wait, spend nothing.
    OtherInstance,
    GaveUp {
        attempts_in_window: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    Adopt(ObservedApp),
    /// Not running and the marker is newer than `quit_reference` (a quit
    /// that still counts). An older marker does not.
    RespectQuit,
    Launch,
}

/// Since when a clean-quit marker keeps the app closed at supervisor boot.
/// `login-item` (production): since the system booted, so a tray Quit in
/// this session stays a quit across supervisor restarts (launchd KeepAlive,
/// `kickstart -k`, re-registration). `always` (dev): since this supervisor
/// started, so starting it is the operator asking for the app. Without a
/// boot time, the supervisor start.
pub fn quit_reference(
    policy: BootLaunchPolicy,
    supervisor_started: SystemTime,
    system_booted: Option<SystemTime>,
) -> SystemTime {
    match (policy, system_booted) {
        (BootLaunchPolicy::LoginItem, Some(booted)) => booted,
        _ => supervisor_started,
    }
}

/// What to do at supervisor start.
pub fn boot_action(app: Option<ObservedApp>, quit_still_counts: bool) -> BootAction {
    match (app, quit_still_counts) {
        (Some(app), _) => BootAction::Adopt(app),
        (None, true) => BootAction::RespectQuit,
        (None, false) => BootAction::Launch,
    }
}

pub struct Supervision {
    pub phase: Phase,
    relaunches: VecDeque<Instant>,
    max_relaunches: u32,
    window: Duration,
    /// Another instance of the app (another bundle, or whatever holds the
    /// host gateway port) runs: no launch is issued and no budget is spent;
    /// a pending launch waits until it is gone and then re-checks its own
    /// conditions.
    pub other_running: bool,
}

impl Supervision {
    pub fn new(max_relaunches: u32, window: Duration) -> Self {
        Self {
            phase: Phase::Idle(IdleReason::NotStarted),
            relaunches: VecDeque::new(),
            max_relaunches,
            window: window.max(Duration::from_secs(1)),
            other_running: false,
        }
    }

    fn in_window(&self, at: Instant, now: Instant) -> bool {
        now.saturating_duration_since(at) < self.window
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&oldest) = self.relaunches.front() {
            if self.in_window(oldest, now) {
                break;
            }
            self.relaunches.pop_front();
        }
    }

    pub fn relaunches_in_window(&self, now: Instant) -> u32 {
        self.relaunches
            .iter()
            .filter(|at| self.in_window(**at, now))
            .count() as u32
    }

    /// One tick: what is running now and when the marker was last written.
    pub fn observe(
        &mut self,
        now: Now,
        app: Option<ObservedApp>,
        marker: Option<SystemTime>,
    ) -> TickAction {
        if let Some(app) = app {
            let previous_pid = match &self.phase {
                Phase::Running(running) => Some(running.pid),
                _ => None,
            };
            if previous_pid == Some(app.pid) {
                return TickAction::None;
            }
            self.phase = Phase::Running(app);
            return TickAction::Adopted { app, previous_pid };
        }
        match self.phase.clone() {
            Phase::Running(app) => {
                if marker_is_fresh(marker, app.started_at) {
                    self.phase = Phase::Idle(IdleReason::QuitMarker);
                    TickAction::Quit
                } else {
                    self.schedule_relaunch(now)
                }
            },
            Phase::Launching { since, since_wall } => {
                if marker_is_fresh(marker, since_wall) {
                    self.phase = Phase::Idle(IdleReason::QuitMarker);
                    TickAction::Quit
                } else if now.mono.saturating_duration_since(since) >= LAUNCH_TIMEOUT {
                    self.schedule_relaunch(now)
                } else {
                    TickAction::None
                }
            },
            Phase::RelaunchPending { crashed_wall, due } => {
                if marker_is_fresh(marker, crashed_wall) {
                    // A deliberate stop arrived while the app was down.
                    self.phase = Phase::Idle(IdleReason::QuitMarker);
                    TickAction::Quit
                } else if now.mono >= due && self.other_running {
                    TickAction::OtherInstance
                } else if now.mono >= due {
                    self.prune(now.mono);
                    self.relaunches.push_back(now.mono);
                    self.phase = Phase::Launching {
                        since: now.mono,
                        since_wall: now.wall,
                    };
                    TickAction::Launch
                } else {
                    TickAction::None
                }
            },
            Phase::BootGrace { until, since_wall } => {
                if marker_is_fresh(marker, since_wall) {
                    self.phase = Phase::Idle(IdleReason::QuitMarker);
                    TickAction::Quit
                } else if now.mono >= until && self.other_running {
                    TickAction::OtherInstance
                } else if now.mono >= until {
                    TickAction::BootLaunch
                } else {
                    TickAction::None
                }
            },
            Phase::Idle(_) | Phase::GaveUp => TickAction::None,
        }
    }

    fn schedule_relaunch(&mut self, now: Now) -> TickAction {
        self.prune(now.mono);
        let attempts_in_window = self.relaunches.len() as u32;
        if attempts_in_window >= self.max_relaunches {
            self.phase = Phase::GaveUp;
            return TickAction::GaveUp { attempts_in_window };
        }
        let relaunch_in = backoff_for(attempts_in_window);
        self.phase = Phase::RelaunchPending {
            crashed_wall: now.wall,
            due: now.mono + relaunch_in,
        };
        TickAction::CrashScheduled {
            relaunch_in,
            attempts_in_window,
        }
    }

    /// An automatic relaunch could not be issued (`open` or the preflight
    /// failed): treated as another crash, inside the same budget.
    pub fn launch_failed(&mut self, now: Now) -> TickAction {
        self.schedule_relaunch(now)
    }

    /// An operator start: a fresh relaunch budget.
    pub fn operator_launched(&mut self, now: Now) {
        self.relaunches.clear();
        self.phase = Phase::Launching {
            since: now.mono,
            since_wall: now.wall,
        };
    }

    /// `open` returned: the appearance timeout starts now, not when the
    /// launch was decided (the preflight may have taken a while). The
    /// marker reference stays at the decision time.
    pub fn launch_issued(&mut self, now: Instant) {
        if let Phase::Launching { since_wall, .. } = self.phase {
            self.phase = Phase::Launching {
                since: now,
                since_wall,
            };
        }
    }

    /// Boot found the app not running: give its login item `BOOT_GRACE`.
    pub fn begin_boot_grace(&mut self, now: Now) {
        self.phase = Phase::BootGrace {
            until: now.mono + BOOT_GRACE,
            since_wall: now.wall,
        };
    }

    /// The re-scan right before `open` found another instance: undo the
    /// launch decision (no budget spent) and wait as before.
    pub fn launch_superseded(&mut self, now: Now, boot: bool) {
        let since_wall = match self.phase {
            Phase::Launching { since_wall, .. } => since_wall,
            _ => now.wall,
        };
        if boot {
            self.phase = Phase::BootGrace {
                until: now.mono,
                since_wall,
            };
        } else {
            self.relaunches.pop_back();
            self.phase = Phase::RelaunchPending {
                crashed_wall: since_wall,
                due: now.mono,
            };
        }
    }

    /// The boot grace ended and the policy says not to launch.
    pub fn boot_launch_declined(&mut self) {
        self.phase = Phase::Idle(IdleReason::LoginItemOff);
    }

    pub fn operator_launch_failed(&mut self) {
        self.phase = Phase::Idle(IdleReason::NotStarted);
    }

    pub fn operator_stopped(&mut self) {
        self.phase = Phase::Idle(IdleReason::OperatorStop);
    }

    pub fn launch_in_progress(&self, now: Instant) -> bool {
        match self.phase {
            Phase::Launching { since, .. } => now.saturating_duration_since(since) < LAUNCH_TIMEOUT,
            _ => false,
        }
    }
}

/// A `stop-desktop` that arrived while a launch was still in flight or its
/// process had not appeared yet. The tick applies it as soon as the process
/// shows up, so the stop is never lost and the app is never adopted against
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingStop {
    /// When the stop arrived, or when the pending launch's `open` returned.
    since: Instant,
    signalled: Option<(ObservedApp, Instant)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStopAction {
    Wait,
    /// The launched process appeared: write the marker and SIGTERM it.
    Signal(ObservedApp),
    /// SIGTERM's grace passed and it is still alive.
    Kill(ObservedApp),
    /// The signalled process is gone.
    Done,
    /// No process appeared within the launch timeout: nothing to stop.
    Expired,
}

impl PendingStop {
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            signalled: None,
        }
    }

    /// The pending launch's `open` returned: give its process the full
    /// appearance timeout from now.
    pub fn launch_returned(&mut self, now: Instant) {
        if self.signalled.is_none() {
            self.since = now;
        }
    }

    pub fn step(
        &mut self,
        now: Instant,
        app: Option<ObservedApp>,
        launch_in_flight: bool,
    ) -> PendingStopAction {
        match (app, self.signalled) {
            (Some(app), Some((signalled, at))) if signalled == app => {
                if now.saturating_duration_since(at) >= STOP_GRACE {
                    PendingStopAction::Kill(app)
                } else {
                    PendingStopAction::Wait
                }
            },
            (Some(app), _) => {
                self.signalled = Some((app, now));
                PendingStopAction::Signal(app)
            },
            (None, Some(_)) => PendingStopAction::Done,
            (None, None) => {
                if !launch_in_flight && now.saturating_duration_since(self.since) >= LAUNCH_TIMEOUT
                {
                    PendingStopAction::Expired
                } else {
                    PendingStopAction::Wait
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Process discovery and signals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    use super::{same_file_path, ObservedApp, Signal};

    fn all_pids() -> Vec<libc::pid_t> {
        // SAFETY: a null buffer asks libproc for the count only.
        let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Vec::new();
        }
        let capacity = needed as usize + 64;
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let bytes = (capacity * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: the buffer holds `capacity` pids and its size is in bytes.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Vec::new();
        }
        pids.truncate((count as usize).min(capacity));
        pids.retain(|pid| *pid > 0);
        pids
    }

    fn executable_path(pid: libc::pid_t) -> Option<PathBuf> {
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes as declared.
        let len =
            unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        if len <= 0 {
            return None;
        }
        buffer.truncate(len as usize);
        Some(PathBuf::from(OsStr::from_bytes(&buffer)))
    }

    fn bsd_info(pid: libc::pid_t) -> Option<libc::proc_bsdinfo> {
        // SAFETY: proc_bsdinfo is plain old data; zeroed is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: the buffer is exactly one proc_bsdinfo, as declared.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (written == size).then_some(info)
    }

    /// This user's live processes running `executable`, oldest first.
    pub fn find(executable: &Path) -> Vec<ObservedApp> {
        let Some(wanted_name) = executable.file_name() else {
            return Vec::new();
        };
        find_named(&[wanted_name.to_os_string()])
            .into_iter()
            .filter(|(_, path)| same_file_path(path, executable))
            .map(|(app, _)| app)
            .collect()
    }

    /// This user's live processes whose executable file name is one of
    /// `names`, at any path, oldest first.
    pub fn find_named(names: &[std::ffi::OsString]) -> Vec<(ObservedApp, PathBuf)> {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let mut found = Vec::new();
        for pid in all_pids() {
            let Some(path) = executable_path(pid) else {
                continue;
            };
            if !path
                .file_name()
                .is_some_and(|name| names.iter().any(|wanted| wanted == name))
            {
                continue;
            }
            let Some(info) = bsd_info(pid) else {
                continue;
            };
            if info.pbi_uid != uid || info.pbi_status == libc::SZOMB {
                continue;
            }
            let started_at = UNIX_EPOCH
                + Duration::from_secs(info.pbi_start_tvsec)
                + Duration::from_micros(info.pbi_start_tvusec);
            found.push((ObservedApp { pid, started_at }, path));
        }
        found.sort_by_key(|(app, _)| (app.started_at, app.pid));
        found
    }

    /// When the system booted (`kern.boottime`).
    pub fn boot_time() -> Option<std::time::SystemTime> {
        // SAFETY: timeval is plain old data; zeroed is a valid value.
        let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::timeval>();
        // SAFETY: the name is NUL-terminated and the buffer is one timeval.
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut boot as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && size == std::mem::size_of::<libc::timeval>() && boot.tv_sec > 0).then(|| {
            UNIX_EPOCH
                + Duration::from_secs(boot.tv_sec as u64)
                + Duration::from_micros(boot.tv_usec.max(0) as u64)
        })
    }

    pub fn signal(pid: i32, signal: Signal) -> std::io::Result<()> {
        let signo = match signal {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: kill has no memory preconditions; pid > 0 comes from find.
        if unsafe { libc::kill(pid, signo) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::Path;

    use super::{ObservedApp, Signal};

    pub fn find(_executable: &Path) -> Vec<ObservedApp> {
        Vec::new()
    }

    pub fn find_named(_names: &[std::ffi::OsString]) -> Vec<(ObservedApp, std::path::PathBuf)> {
        Vec::new()
    }

    pub fn boot_time() -> Option<std::time::SystemTime> {
        None
    }

    pub fn signal(_pid: i32, _signal: Signal) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Slow work that runs without the desktop lock
// ---------------------------------------------------------------------------

/// Everything a launch needs, captured under the lock.
pub struct LaunchPlan {
    bundle: PathBuf,
    executable: PathBuf,
    gateway_port: u16,
    shutting_down: Arc<AtomicBool>,
    verify_script: Option<PathBuf>,
    args: Vec<OsString>,
    display: Vec<String>,
}

/// How a launch ended when it did not fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// `open` returned successfully.
    Issued,
    /// An instance appeared during the preflight (a hand or `make` launch);
    /// `open` was skipped and that instance is adopted.
    AlreadyRunning(ObservedApp),
    /// Another instance (another bundle, or the gateway port taken) appeared
    /// during the preflight; `open` was skipped.
    OtherInstance(String),
}

impl LaunchPlan {
    /// The preflight (as the Makefile runs it) and the LaunchServices launch.
    /// Timed-out children are killed (`kill_on_drop`), never orphaned.
    async fn run(&self) -> Result<LaunchOutcome> {
        if let Some(script) = &self.verify_script {
            let output = timeout(
                PREFLIGHT_TIMEOUT,
                Command::new(BASH_PROGRAM)
                    .arg(script)
                    .arg(&self.bundle)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .context("desktop app preflight timed out")?
            .with_context(|| format!("running desktop app preflight {}", script.display()))?;
            if !output.status.success() {
                anyhow::bail!(
                    "desktop app preflight {} failed ({}): {}",
                    script.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
        // The supervisor began shutting down during the preflight (logout):
        // never open the app now.
        if self.shutting_down.load(Ordering::SeqCst) {
            anyhow::bail!("the supervisor is shutting down; launch cancelled before open");
        }
        // The preflight takes seconds: an instance started meanwhile must
        // not get a twin from `open -n`.
        if let Some(running) = platform::find(&self.executable).into_iter().next() {
            info!(
                "Desktop app appeared during the preflight (PID {}); not launching another",
                running.pid
            );
            return Ok(LaunchOutcome::AlreadyRunning(running));
        }
        if let Some(other) = other_instance(&self.executable, self.gateway_port) {
            info!(
                "Another desktop app instance appeared during the preflight ({}); not launching",
                other
            );
            return Ok(LaunchOutcome::OtherInstance(other));
        }
        info!(
            "Launching desktop app through LaunchServices: {} {:?}",
            OPEN_PROGRAM, self.display
        );
        let output = timeout(
            OPEN_TIMEOUT,
            Command::new(OPEN_PROGRAM)
                .args(&self.args)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("`open` did not return in time")?
        .context("running /usr/bin/open")?;
        if !output.status.success() {
            anyhow::bail!(
                "`open` failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(LaunchOutcome::Issued)
    }
}

/// The processes `stop-desktop` signalled, waited on without the lock.
pub struct StopPlan {
    executable: PathBuf,
    targets: Vec<ObservedApp>,
}

impl StopPlan {
    fn survivors(&self) -> Vec<ObservedApp> {
        platform::find(&self.executable)
            .into_iter()
            .filter(|alive| self.targets.contains(alive))
            .collect()
    }

    fn pids(apps: &[ObservedApp]) -> String {
        apps.iter()
            .map(|app| app.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Wait out the SIGTERM grace, SIGKILL what is left, wait again.
    async fn finish(self) -> Result<String> {
        let pids = Self::pids(&self.targets);
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            let alive = self.survivors();
            if alive.is_empty() {
                return Ok(format!("Desktop app stopped (PID {pids})"));
            }
            if Instant::now() >= deadline {
                warn!(
                    "Desktop app did not exit within {}s of SIGTERM; sending SIGKILL",
                    STOP_GRACE.as_secs()
                );
                for app in &alive {
                    if let Err(error) = platform::signal(app.pid, Signal::Kill) {
                        warn!("SIGKILL to desktop app PID {} failed: {}", app.pid, error);
                    }
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let deadline = Instant::now() + KILL_GRACE;
        loop {
            let alive = self.survivors();
            if alive.is_empty() {
                return Ok(format!("Desktop app killed (PID {pids})"));
            }
            if Instant::now() >= deadline {
                anyhow::bail!("desktop app PID {} survived SIGKILL", Self::pids(&alive));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// What `start` decided under the lock.
pub enum StartStep {
    Done(String),
    Launch(LaunchPlan),
}

/// What `stop` decided under the lock.
pub enum StopStep {
    Done(String),
    Wait(StopPlan),
}

// ---------------------------------------------------------------------------
// Entry points: lock, decide, unlock, run, lock, record
// ---------------------------------------------------------------------------

/// At supervisor start: adopt a running app, keep a quit made since the
/// supervisor started, or start the boot grace after which the tick
/// launches it.
pub async fn boot(process: &Arc<Mutex<DesktopAppProcess>>) {
    process.lock().await.boot_decide();
}

/// Which bookkeeping a tick-issued launch gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    /// After a crash: counts as a restart, inside the relaunch budget.
    Relaunch,
    /// After the boot grace: a fresh start, like `start-desktop`.
    Boot,
    /// A `start-desktop` that waited for another instance to exit.
    Operator,
}

/// One watch tick (every `desktop_poll_interval` seconds).
pub async fn tick(process: &Arc<Mutex<DesktopAppProcess>>) {
    let decided = process.lock().await.tick_decide();
    if let Some((plan, kind)) = decided {
        let result = plan.run().await;
        let mut guard = process.lock().await;
        match kind {
            LaunchKind::Relaunch => guard.finish_relaunch(result),
            LaunchKind::Boot | LaunchKind::Operator => {
                match guard.finish_operator_launch(result, kind == LaunchKind::Operator) {
                    Ok(message) => info!("Desktop app launch: {}", message),
                    Err(error) => error!("Failed to launch the desktop app: {:#}", error),
                }
            },
        }
    }
}

/// `start-desktop`: launch unless already running or launching. Clears the
/// clean-quit marker and any given-up state. Replies once the launch is
/// decided: the preflight and `open` continue in the background (they can
/// outlast a control client's wait); `status-desktop` shows the outcome.
pub async fn start(process: &Arc<Mutex<DesktopAppProcess>>) -> Result<String> {
    let step = process.lock().await.start_decide()?;
    match step {
        StartStep::Done(message) => Ok(message),
        StartStep::Launch(plan) => {
            let process = Arc::clone(process);
            tokio::spawn(async move {
                let result = plan.run().await;
                match process.lock().await.finish_operator_launch(result, true) {
                    Ok(message) => info!("Desktop app start: {}", message),
                    Err(error) => error!("Desktop app start failed: {:#}", error),
                }
            });
            Ok(
                "Desktop app launch started (preflight and LaunchServices open run in the background; see status-desktop)"
                    .to_string(),
            )
        },
    }
}

/// `stop-desktop`: marker first (so nothing reads the exit as a crash),
/// then SIGTERM, then SIGKILL after the grace period.
pub async fn stop(process: &Arc<Mutex<DesktopAppProcess>>) -> Result<String> {
    let step = process.lock().await.stop_begin()?;
    match step {
        StopStep::Done(message) => Ok(message),
        StopStep::Wait(plan) => plan.finish().await,
    }
}

/// `restart-desktop`: stop, then start (which clears the marker).
pub async fn restart(process: &Arc<Mutex<DesktopAppProcess>>) -> Result<String> {
    let stopped = stop(process).await?;
    let started = start(process).await?;
    process.lock().await.record_restart();
    Ok(format!("{stopped}; {started}"))
}

// ---------------------------------------------------------------------------
// The managed service (state; every method runs under the lock and is quick)
// ---------------------------------------------------------------------------

/// `status-desktop`: the common status shape plus what the watcher knows.
#[derive(Debug, Clone, Serialize)]
pub struct DesktopStatus {
    #[serde(flatten)]
    pub process: ProcessStatus,
    pub supported: bool,
    pub managed: bool,
    /// Why management is on or off (flag, config, default, OS, bundle).
    pub management: String,
    pub phase: String,
    pub launch_in_flight: bool,
    pub stop_pending: bool,
    pub other_instance: Option<String>,
    pub bundle: Option<String>,
    pub executable: Option<String>,
    pub quit_marker: Option<String>,
    pub quit_marker_written_at: Option<String>,
    pub next_relaunch_in_seconds: Option<u64>,
    pub last_exit: Option<String>,
    pub last_crash_report: Option<String>,
    pub last_error: Option<String>,
    pub relaunch_backoff_seconds: Vec<u64>,
    pub max_relaunches: u32,
}

pub struct DesktopAppProcess {
    config: SupervisorConfig,
    supervision: Supervision,
    /// Set (to when it began) while a preflight/`open` runs without the lock.
    launch_in_flight: Option<Instant>,
    /// A stop that arrived before the launched process appeared.
    stop_pending: Option<PendingStop>,
    /// When this supervisor started: only a marker written after this keeps
    /// boot from launching the app.
    started_wall: SystemTime,
    /// Decided once at start from the flag, the runtime config and the host.
    management: Management,
    /// The other instance seen on the last tick, if any.
    other_instance: Option<String>,
    /// Set when the supervisor begins shutting down: no launch after it,
    /// including an in-flight launch that has not reached `open` yet.
    shutting_down: Arc<AtomicBool>,
    /// An operator `start-desktop` was overtaken by another instance: launch
    /// when it exits, whatever the boot policy says.
    operator_start_pending: bool,
    /// Boot could not decide yet (the bundle was missing, e.g. mid-rebuild):
    /// re-checked every tick until it appears.
    boot_pending: bool,
    restart_count: u32,
    last_restart: Option<chrono::DateTime<chrono::Utc>>,
    /// The run that ended last, for crash-report lookup.
    crashed_run: Option<ObservedApp>,
    last_exit: Option<String>,
    last_crash_report: Option<PathBuf>,
    last_error: Option<String>,
}

impl DesktopAppProcess {
    pub fn new(config: SupervisorConfig) -> Self {
        let supervision = Supervision::new(
            config.desktop_max_relaunches,
            Duration::from_secs(config.desktop_relaunch_window),
        );
        // Read once: the config is restart-bound for the supervisor.
        let setting = if !Self::is_supported() || config.desktop_manage.is_some() {
            ConfigSetting::NotGiven
        } else {
            read_manage_desktop(config.magician_config.as_deref())
        };
        if let ConfigSetting::Unreadable(why) | ConfigSetting::Unparseable(why) = &setting {
            warn!(
                "supervisor.manage_desktop not read ({}); using the default (on)",
                why
            );
        }
        let management = decide_management(
            Self::is_supported(),
            config.desktop_manage,
            &setting,
            config.desktop_app.is_some(),
        );
        info!("Desktop app management: {}", management.reason);
        Self {
            management,
            other_instance: None,
            shutting_down: Arc::new(AtomicBool::new(false)),
            operator_start_pending: false,
            boot_pending: false,
            config,
            supervision,
            launch_in_flight: None,
            stop_pending: None,
            started_wall: SystemTime::now(),
            restart_count: 0,
            last_restart: None,
            crashed_run: None,
            last_exit: None,
            last_crash_report: None,
            last_error: None,
        }
    }

    pub fn is_supported() -> bool {
        cfg!(target_os = "macos")
    }

    /// Enabled (flag > config > on), on macOS, with a known bundle.
    pub fn is_managed(&self) -> bool {
        self.management.enabled
    }

    fn gateway_port(&self) -> u16 {
        gateway_port(
            &self.config.desktop_env,
            std::env::var("MAGICIAN_HOST_GATEWAY_URL").ok().as_deref(),
        )
    }

    /// Called at the start of the supervisor's stop phase (shutdown, logout):
    /// no launch or relaunch is issued after it.
    pub fn begin_shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    pub fn poll_interval_secs(&self) -> u64 {
        self.config.desktop_poll_interval.max(1)
    }

    fn bundle(&self) -> Option<PathBuf> {
        self.config.desktop_app.as_deref().map(absolute)
    }

    fn executable(&self) -> Option<PathBuf> {
        self.bundle().map(|bundle| bundle_executable(&bundle))
    }

    fn quit_marker(&self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        quit_marker_path(self.config.desktop_quit_marker.as_deref(), home.as_deref())
            .map(|path| absolute(&path))
    }

    fn launch_in_flight(&self, now: Instant) -> bool {
        self.launch_in_flight
            .is_some_and(|since| now.saturating_duration_since(since) < IN_FLIGHT_LIMIT)
    }

    /// Why a managed desktop cannot be supervised, if it cannot.
    fn config_problem(&self) -> Option<String> {
        let Some(bundle) = self.bundle() else {
            return Some("--manage-desktop needs --desktop-app <bundle>".to_string());
        };
        if !bundle.is_dir() {
            return Some(format!(
                "desktop app bundle {} is missing",
                bundle.display()
            ));
        }
        if self.quit_marker().is_none() {
            return Some(
                "the clean-quit marker has no home: HOME is unset; pass --desktop-quit-marker"
                    .to_string(),
            );
        }
        None
    }

    /// Fails with the reason when commands cannot act on the app here.
    fn ensure_manageable(&self) -> Result<(PathBuf, PathBuf, PathBuf)> {
        anyhow::ensure!(
            Self::is_supported(),
            "desktop app management is macOS-only (unsupported on this host)"
        );
        anyhow::ensure!(
            self.management.enabled,
            "desktop app management is off: {}",
            self.management.reason
        );
        if let Some(problem) = self.config_problem() {
            anyhow::bail!(problem);
        }
        match (self.bundle(), self.executable(), self.quit_marker()) {
            (Some(bundle), Some(executable), Some(marker)) => Ok((bundle, executable, marker)),
            _ => anyhow::bail!("desktop app configuration is incomplete"),
        }
    }

    /// The oldest live instance (stable across an in-app restart overlap).
    fn scan(executable: &Path) -> Option<ObservedApp> {
        platform::find(executable).into_iter().next()
    }

    /// Capture a launch and mark it in flight. Refuses while one is in
    /// flight or the app is already running.
    fn begin_launch(&mut self, now: Instant) -> Result<LaunchPlan> {
        let (bundle, executable, marker) = self.ensure_manageable()?;
        anyhow::ensure!(
            !self.launch_in_flight(now),
            "a desktop app launch is already in flight"
        );
        anyhow::ensure!(!self.is_shutting_down(), "the supervisor is shutting down");
        if let Some(running) = Self::scan(&executable) {
            anyhow::bail!("desktop app is already running (PID {})", running.pid);
        }
        let gateway_port = self.gateway_port();
        if let Some(other) = other_instance(&executable, gateway_port) {
            anyhow::bail!(
                "another desktop app instance is running ({other}); not opening a second"
            );
        }
        let log = self.config.desktop_log.as_deref().map(absolute);
        let env = launch_env(&self.config.desktop_env, &marker);
        let mut display = vec!["-n".to_string(), "-g".to_string()];
        // Env keys only: values stay out of logs and status output.
        for (key, _) in &env {
            display.extend(["--env".to_string(), key.clone()]);
        }
        display.push(bundle.display().to_string());
        let plan = LaunchPlan {
            args: open_args(&bundle, log.as_deref(), &env),
            verify_script: self.config.desktop_verify_script.clone(),
            display,
            bundle,
            executable,
            gateway_port,
            shutting_down: Arc::clone(&self.shutting_down),
        };
        self.launch_in_flight = Some(now);
        Ok(plan)
    }

    fn boot_decide(&mut self) {
        if !self.is_managed() {
            return;
        }
        let (_, executable, marker) = match self.ensure_manageable() {
            Ok(paths) => {
                self.boot_pending = false;
                paths
            },
            Err(error) => {
                warn!(
                    "Desktop app boot decision deferred until the bundle is usable: {:#}",
                    error
                );
                self.last_error = Some(format!("{error:#}"));
                self.boot_pending = true;
                return;
            },
        };
        let reference = quit_reference(
            self.config.desktop_launch_at_boot,
            self.started_wall,
            platform::boot_time(),
        );
        let quit_still_counts = marker_is_fresh(file_mtime(&marker), reference);
        match boot_action(Self::scan(&executable), quit_still_counts) {
            BootAction::Adopt(app) => {
                info!(
                    "Desktop app already running (PID {}); watching it for crashes",
                    app.pid
                );
                self.supervision.observe(Now::current(), Some(app), None);
            },
            BootAction::RespectQuit => {
                info!(
                    "Desktop app is not running and {} records a quit that still counts; not launching it (use start-desktop)",
                    marker.display()
                );
                self.supervision.phase = Phase::Idle(IdleReason::QuitMarker);
            },
            BootAction::Launch => {
                info!(
                    "Desktop app is not running; waiting {}s for its login item before launching it",
                    BOOT_GRACE.as_secs()
                );
                self.supervision.begin_boot_grace(Now::current());
            },
        }
    }

    fn tick_decide(&mut self) -> Option<(LaunchPlan, LaunchKind)> {
        let now = Now::current();
        if !self.is_managed() || self.config_problem().is_some() || self.is_shutting_down() {
            return None;
        }
        if self.boot_pending {
            // The bundle is back: take the boot decision now.
            self.boot_decide();
            return None;
        }
        let (Some(executable), Some(marker)) = (self.executable(), self.quit_marker()) else {
            return None;
        };
        let app = Self::scan(&executable);
        if self.stop_pending.is_some() {
            self.apply_pending_stop(now.mono, app, &marker);
            return None;
        }
        if self.launch_in_flight(now.mono) {
            return None;
        }
        // Only when ours is not running: another bundle or a taken gateway
        // port blocks any launch (and spends no budget) until it is gone.
        self.other_instance = match app {
            Some(_) => None,
            None => other_instance(&executable, self.gateway_port()),
        };
        self.supervision.other_running = self.other_instance.is_some();
        let previous = match &self.supervision.phase {
            Phase::Running(app) => Some(*app),
            _ => None,
        };
        match self.supervision.observe(now, app, file_mtime(&marker)) {
            TickAction::None => None,
            TickAction::Adopted { app, previous_pid } => {
                self.operator_start_pending = false;
                match previous_pid {
                    Some(old) => info!(
                        "Desktop app replaced itself (PID {} -> {}); still running",
                        old, app.pid
                    ),
                    None => info!("Desktop app is running (PID {})", app.pid),
                }
                None
            },
            TickAction::Quit => {
                self.operator_start_pending = false;
                info!(
                    "Desktop app exited after a clean-quit marker ({}); not relaunching",
                    marker.display()
                );
                self.last_exit = Some("quit".to_string());
                None
            },
            TickAction::CrashScheduled {
                relaunch_in,
                attempts_in_window,
            } => {
                self.crashed_run = previous;
                self.last_exit = Some("crashed".to_string());
                let report = self.note_crash_report(&executable);
                warn!(
                    "Desktop app exited without a clean-quit marker (crash{}); relaunching in {}s ({} relaunch(es) in the window)",
                    report
                        .map(|path| format!(", report {}", path.display()))
                        .unwrap_or_default(),
                    relaunch_in.as_secs(),
                    attempts_in_window
                );
                None
            },
            TickAction::GaveUp { attempts_in_window } => {
                self.crashed_run = previous.or(self.crashed_run);
                self.last_exit = Some("crashed".to_string());
                self.note_crash_report(&executable);
                error!(
                    "Desktop app keeps crashing: {} relaunch(es) in the last {}s; giving up until start-desktop",
                    attempts_in_window,
                    self.config.desktop_relaunch_window.max(1)
                );
                None
            },
            TickAction::OtherInstance => {
                debug!(
                    "Desktop app launch deferred: another instance runs ({})",
                    self.other_instance.as_deref().unwrap_or("unknown")
                );
                None
            },
            TickAction::BootLaunch => {
                let home = std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(PathBuf::from);
                let policy = self.config.desktop_launch_at_boot;
                let operator = std::mem::take(&mut self.operator_start_pending);
                if !boot_launch_proceeds(policy, login_item_enabled(home.as_deref()), operator) {
                    info!(
                        "Desktop app did not start within the {}s boot grace and its login item is off; not launching it (crash relaunch applies once it is started)",
                        BOOT_GRACE.as_secs()
                    );
                    self.supervision.boot_launch_declined();
                    return None;
                }
                info!(
                    "Desktop app did not start within the {}s boot grace; launching it",
                    BOOT_GRACE.as_secs()
                );
                remove_marker(&marker);
                match self.begin_launch(now.mono) {
                    Ok(plan) => {
                        self.supervision.operator_launched(now);
                        Some((
                            plan,
                            if operator {
                                LaunchKind::Operator
                            } else {
                                LaunchKind::Boot
                            },
                        ))
                    },
                    Err(error) => {
                        // Retried with the relaunch backoff, within its budget.
                        error!("Desktop app boot launch not issued: {:#}", error);
                        self.relaunch_failed(error);
                        None
                    },
                }
            },
            TickAction::Launch => {
                self.note_crash_report(&executable);
                match self.begin_launch(now.mono) {
                    Ok(plan) => Some((plan, LaunchKind::Relaunch)),
                    Err(error) => {
                        self.relaunch_failed(error);
                        None
                    },
                }
            },
        }
    }

    fn finish_relaunch(&mut self, result: Result<LaunchOutcome>) {
        self.launch_in_flight = None;
        let now = Now::current();
        if let Some(pending) = self.stop_pending.as_mut() {
            // A stop arrived meanwhile: no relaunch accounting, and nothing
            // to stop if `open` failed.
            match result {
                Ok(_) => pending.launch_returned(now.mono),
                Err(_) => self.stop_pending = None,
            }
            return;
        }
        match result {
            Ok(LaunchOutcome::Issued) => {
                self.supervision.launch_issued(now.mono);
                self.restart_count += 1;
                self.last_restart = Some(chrono::Utc::now());
                self.last_error = None;
                info!("Desktop app relaunched after a crash");
            },
            Ok(LaunchOutcome::AlreadyRunning(app)) => {
                self.supervision.observe(now, Some(app), None);
                self.last_error = None;
            },
            Ok(LaunchOutcome::OtherInstance(other)) => {
                self.other_instance = Some(other);
                self.supervision.launch_superseded(now, false);
            },
            Err(error) => self.relaunch_failed(error),
        }
    }

    /// Carry out a stop that arrived before the launched process appeared.
    fn apply_pending_stop(&mut self, now: Instant, app: Option<ObservedApp>, marker: &Path) {
        let in_flight = self.launch_in_flight(now);
        let Some(pending) = self.stop_pending.as_mut() else {
            return;
        };
        match pending.step(now, app, in_flight) {
            PendingStopAction::Wait => {},
            PendingStopAction::Signal(app) => {
                // Rewritten now, after the process started, so its exit
                // reads as a deliberate stop everywhere.
                if let Err(error) = write_marker(marker, "stopped-by=magic-supervisor") {
                    warn!(
                        "Could not write desktop clean-quit marker {}: {}",
                        marker.display(),
                        error
                    );
                }
                info!(
                    "Applying the pending stop to the launched desktop app (PID {})",
                    app.pid
                );
                if let Err(error) = platform::signal(app.pid, Signal::Term) {
                    warn!("SIGTERM to desktop app PID {} failed: {}", app.pid, error);
                }
            },
            PendingStopAction::Kill(app) => {
                warn!(
                    "Desktop app PID {} ignored SIGTERM for {}s; sending SIGKILL",
                    app.pid,
                    STOP_GRACE.as_secs()
                );
                if let Err(error) = platform::signal(app.pid, Signal::Kill) {
                    warn!("SIGKILL to desktop app PID {} failed: {}", app.pid, error);
                }
            },
            PendingStopAction::Done | PendingStopAction::Expired => {
                info!("Pending desktop app stop finished");
                self.stop_pending = None;
                self.supervision.operator_stopped();
            },
        }
    }

    fn relaunch_failed(&mut self, error: anyhow::Error) {
        self.last_error = Some(format!("{error:#}"));
        match self.supervision.launch_failed(Now::current()) {
            TickAction::CrashScheduled { relaunch_in, .. } => warn!(
                "Desktop app relaunch failed ({:#}); retrying in {}s",
                error,
                relaunch_in.as_secs()
            ),
            _ => error!(
                "Desktop app relaunch failed ({:#}); giving up until start-desktop",
                error
            ),
        }
    }

    fn start_decide(&mut self) -> Result<StartStep> {
        let (_, executable, marker) = self.ensure_manageable()?;
        // An operator decision replaces a deferred boot decision.
        self.boot_pending = false;
        let now = Now::current();
        anyhow::ensure!(!self.is_shutting_down(), "the supervisor is shutting down");
        self.operator_start_pending = false;
        anyhow::ensure!(
            self.stop_pending.is_none(),
            "a stop is pending for a desktop app launch that has not appeared yet; retry in a few seconds"
        );
        if self.launch_in_flight(now.mono) || self.supervision.launch_in_progress(now.mono) {
            return Ok(StartStep::Done(
                "Desktop app launch already in progress".to_string(),
            ));
        }
        if let Some(app) = Self::scan(&executable) {
            self.supervision.observe(now, Some(app), None);
            return Ok(StartStep::Done(format!(
                "Desktop app already running (PID {})",
                app.pid
            )));
        }
        remove_marker(&marker);
        let plan = self.begin_launch(now.mono)?;
        self.supervision.operator_launched(now);
        Ok(StartStep::Launch(plan))
    }

    /// `operator`: a `start-desktop` (not the boot-grace launch).
    fn finish_operator_launch(
        &mut self,
        result: Result<LaunchOutcome>,
        operator: bool,
    ) -> Result<String> {
        self.launch_in_flight = None;
        let now = Now::current();
        if let Some(pending) = self.stop_pending.as_mut() {
            return match result {
                Ok(_) => {
                    pending.launch_returned(now.mono);
                    Ok("Desktop app launched; the stop that arrived meanwhile applies once it appears".to_string())
                },
                Err(error) => {
                    self.stop_pending = None;
                    self.last_error = Some(format!("{error:#}"));
                    Err(error)
                },
            };
        }
        match result {
            Ok(LaunchOutcome::Issued) => {
                self.supervision.launch_issued(now.mono);
                self.last_error = None;
                Ok("Desktop app launched through LaunchServices".to_string())
            },
            Ok(LaunchOutcome::AlreadyRunning(app)) => {
                self.supervision.observe(now, Some(app), None);
                self.last_error = None;
                Ok(format!(
                    "Desktop app appeared during the preflight (PID {}); adopted it",
                    app.pid
                ))
            },
            Ok(LaunchOutcome::OtherInstance(other)) => {
                // Wait like an unfinished boot grace: launch once it is gone,
                // if the boot conditions still hold.
                self.supervision.launch_superseded(now, true);
                // An explicit start stays explicit: when that instance exits
                // the app is launched even if the login-item policy would
                // decline a boot launch.
                self.operator_start_pending = operator;
                self.other_instance = Some(other.clone());
                Ok(format!(
                    "Another desktop app instance appeared during the preflight ({other}); not launching"
                ))
            },
            Err(error) => {
                if operator {
                    self.supervision.operator_launch_failed();
                    self.last_error = Some(format!("{error:#}"));
                } else {
                    // A failed boot launch is retried with the relaunch
                    // backoff, within its budget, instead of giving up.
                    self.last_error = Some(format!("{error:#}"));
                    self.supervision.launch_failed(Now::current());
                }
                Err(error)
            },
        }
    }

    fn stop_begin(&mut self) -> Result<StopStep> {
        let (_, executable, marker) = self.ensure_manageable()?;
        // An operator decision replaces a deferred boot decision.
        self.boot_pending = false;
        let now = Instant::now();
        self.operator_start_pending = false;
        if self.stop_pending.is_some() {
            return Ok(StopStep::Done(
                "A desktop app stop is already pending".to_string(),
            ));
        }
        if let Err(error) = write_marker(&marker, "stopped-by=magic-supervisor") {
            warn!(
                "Could not write desktop clean-quit marker {}: {}",
                marker.display(),
                error
            );
        }
        let launching = self.launch_in_flight(now) || self.supervision.launch_in_progress(now);
        self.supervision.operator_stopped();
        let targets = platform::find(&executable);
        if targets.is_empty() {
            if launching {
                // `open` is running or returned but the process is not
                // visible yet: stop it the moment it appears.
                self.stop_pending = Some(PendingStop::new(now));
                return Ok(StopStep::Done(
                    "Desktop app is still launching; it will be stopped as soon as it appears"
                        .to_string(),
                ));
            }
            return Ok(StopStep::Done("Desktop app was not running".to_string()));
        }
        info!("Stopping desktop app (PID {})", StopPlan::pids(&targets));
        for app in &targets {
            if let Err(error) = platform::signal(app.pid, Signal::Term) {
                warn!("SIGTERM to desktop app PID {} failed: {}", app.pid, error);
            }
        }
        Ok(StopStep::Wait(StopPlan {
            executable,
            targets,
        }))
    }

    fn record_restart(&mut self) {
        self.restart_count += 1;
        self.last_restart = Some(chrono::Utc::now());
    }

    /// Positive crash evidence for the log: a fresh `.ips` report.
    fn note_crash_report(&mut self, executable: &Path) -> Option<PathBuf> {
        let since = self.crashed_run.map(|run| run.started_at)?;
        let name = executable.file_name()?.to_str()?.to_string();
        let home = std::env::var_os("HOME")?;
        let dir = PathBuf::from(home).join("Library/Logs/DiagnosticReports");
        let report = find_crash_report(&dir, &name, since)?;
        if self.last_crash_report.as_ref() != Some(&report) {
            info!("Desktop app crash report: {}", report.display());
            self.last_crash_report = Some(report.clone());
        }
        Some(report)
    }

    /// For the aggregate `health` command: `None` when the desktop is not
    /// managed or was deliberately stopped or quit.
    pub fn health(&self) -> Option<bool> {
        if !self.is_managed() {
            return None;
        }
        // A moved or deleted bundle is reported in status, not as an
        // unhealthy stack.
        if self.config_problem().is_some() {
            return None;
        }
        if self.other_instance.is_some()
            && matches!(
                self.supervision.phase,
                Phase::RelaunchPending { .. } | Phase::BootGrace { .. }
            )
        {
            return None;
        }
        match self.supervision.phase {
            Phase::Running(_) => Some(true),
            Phase::RelaunchPending { .. } | Phase::GaveUp => Some(false),
            Phase::Launching { .. } | Phase::BootGrace { .. } | Phase::Idle(_) => None,
        }
    }

    fn display_args(&self) -> Vec<String> {
        let mut args = vec!["-n".to_string(), "-g".to_string()];
        // Env keys only: values stay out of status output.
        for (key, _) in &self.config.desktop_env {
            args.push("--env".to_string());
            args.push(key.clone());
        }
        args.extend(["--env".to_string(), QUIT_MARKER_ENV.to_string()]);
        if let Some(bundle) = self.bundle() {
            args.push(bundle.display().to_string());
        }
        args
    }

    fn status_words(&self) -> (&'static str, &'static str) {
        if !Self::is_supported() {
            return ("unsupported", "unsupported");
        }
        if !self.management.enabled {
            return ("not_managed", "not_managed");
        }
        if self.config_problem().is_some() {
            return ("misconfigured", "degraded");
        }
        if self.other_instance.is_some()
            && matches!(
                self.supervision.phase,
                Phase::RelaunchPending { .. } | Phase::BootGrace { .. }
            )
        {
            return ("other_instance", "other_instance");
        }
        match self.supervision.phase {
            Phase::Running(_) => ("running", "healthy"),
            Phase::Launching { .. } => ("starting", "starting"),
            Phase::BootGrace { .. } => ("starting", "boot_grace"),
            Phase::RelaunchPending { .. } => ("crashed", "relaunch_pending"),
            Phase::GaveUp => ("crashed", "unhealthy"),
            Phase::Idle(IdleReason::NotStarted) => ("stopped", "not_started"),
            Phase::Idle(IdleReason::OperatorStop) => ("stopped", "stopped"),
            Phase::Idle(IdleReason::QuitMarker) => ("stopped", "quit"),
            Phase::Idle(IdleReason::LoginItemOff) => ("stopped", "login_item_off"),
        }
    }

    pub fn get_status(&self) -> ProcessStatus {
        let (status, health_status) = self.status_words();
        let running = match self.supervision.phase {
            Phase::Running(app) => Some(app),
            _ => None,
        };
        ProcessStatus {
            service: "desktop".to_string(),
            pid: running.map(|app| app.pid as u32),
            status: status.to_string(),
            uptime_seconds: running.and_then(|app| {
                SystemTime::now()
                    .duration_since(app.started_at)
                    .ok()
                    .map(|uptime| uptime.as_secs())
            }),
            restart_count: self.restart_count,
            restart_attempts_in_window: self.supervision.relaunches_in_window(Instant::now()),
            restart_attempt_window_seconds: self.config.desktop_relaunch_window.max(1),
            last_restart: self.last_restart.map(|t| t.to_rfc3339()),
            health_status: health_status.to_string(),
            args: self.display_args(),
        }
    }

    pub fn detailed_status(&self) -> DesktopStatus {
        let now = Instant::now();
        let marker = self.quit_marker();
        let phase = match &self.supervision.phase {
            Phase::Idle(IdleReason::NotStarted) => "idle:not_started",
            Phase::Idle(IdleReason::OperatorStop) => "idle:operator_stop",
            Phase::Idle(IdleReason::QuitMarker) => "idle:quit_marker",
            Phase::Idle(IdleReason::LoginItemOff) => "idle:login_item_off",
            Phase::Running(_) => "running",
            Phase::Launching { .. } => "launching",
            Phase::BootGrace { .. } => "boot_grace",
            Phase::RelaunchPending { .. } => "relaunch_pending",
            Phase::GaveUp => "gave_up",
        };
        let next_relaunch_in_seconds = match self.supervision.phase {
            Phase::RelaunchPending { due, .. } => {
                Some(due.saturating_duration_since(now).as_secs())
            },
            _ => None,
        };
        DesktopStatus {
            process: self.get_status(),
            supported: Self::is_supported(),
            managed: self.is_managed(),
            management: self.management.reason.clone(),
            phase: phase.to_string(),
            launch_in_flight: self.launch_in_flight(now),
            stop_pending: self.stop_pending.is_some(),
            other_instance: self.other_instance.clone(),
            bundle: self.bundle().map(|path| path.display().to_string()),
            executable: self.executable().map(|path| path.display().to_string()),
            quit_marker_written_at: marker
                .as_deref()
                .and_then(file_mtime)
                .and_then(unix_seconds),
            quit_marker: marker.map(|path| path.display().to_string()),
            next_relaunch_in_seconds,
            last_exit: self.last_exit.clone(),
            last_crash_report: self
                .last_crash_report
                .as_ref()
                .map(|path| path.display().to_string()),
            last_error: self.last_error.clone().or_else(|| self.config_problem()),
            relaunch_backoff_seconds: RELAUNCH_BACKOFF_SECS.to_vec(),
            max_relaunches: self.config.desktop_max_relaunches,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    /// A fixed base for both clocks, so `at(n)` is n seconds into the test.
    fn base() -> Instant {
        static BASE: OnceLock<Instant> = OnceLock::new();
        *BASE.get_or_init(Instant::now)
    }

    fn t(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + seconds)
    }

    fn at(seconds: u64) -> Now {
        Now {
            mono: base() + Duration::from_secs(seconds),
            wall: t(seconds),
        }
    }

    fn app(pid: i32, started: u64) -> ObservedApp {
        ObservedApp {
            pid,
            started_at: t(started),
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn running(max: u32) -> Supervision {
        let mut supervision = Supervision::new(max, Duration::from_secs(300));
        supervision.observe(at(0), Some(app(100, 0)), None);
        supervision
    }

    #[test]
    fn a_tray_quit_is_not_relaunched() {
        let mut supervision = running(4);
        // The tray wrote the marker while pid 100 (started at t0) ran.
        assert_eq!(
            supervision.observe(at(50), None, Some(t(49))),
            TickAction::Quit
        );
        assert_eq!(supervision.phase, Phase::Idle(IdleReason::QuitMarker));
        // And stays quit on later ticks.
        assert_eq!(
            supervision.observe(at(500), None, Some(t(49))),
            TickAction::None
        );
    }

    #[test]
    fn a_disappearance_without_a_marker_is_a_crash_and_relaunches_after_backoff() {
        let mut supervision = running(4);
        assert_eq!(
            supervision.observe(at(10), None, None),
            TickAction::CrashScheduled {
                relaunch_in: Duration::from_secs(2),
                attempts_in_window: 0
            }
        );
        assert_eq!(supervision.observe(at(11), None, None), TickAction::None);
        assert_eq!(supervision.observe(at(12), None, None), TickAction::Launch);
        assert!(matches!(supervision.phase, Phase::Launching { .. }));
        assert_eq!(supervision.relaunches_in_window(at(12).mono), 1);
    }

    #[test]
    fn a_marker_older_than_the_run_is_stale_and_the_exit_is_a_crash() {
        let mut supervision = Supervision::new(4, Duration::from_secs(300));
        // Adopted a process started at t100; the marker is from an earlier quit.
        supervision.observe(at(100), Some(app(7, 100)), None);
        assert!(matches!(
            supervision.observe(at(200), None, Some(t(90))),
            TickAction::CrashScheduled { .. }
        ));
    }

    #[test]
    fn an_in_app_restart_is_a_new_pid_not_a_crash() {
        let mut supervision = running(4);
        assert_eq!(
            supervision.observe(at(30), Some(app(200, 29)), None),
            TickAction::Adopted {
                app: app(200, 29),
                previous_pid: Some(100)
            }
        );
        assert_eq!(
            supervision.observe(at(33), Some(app(200, 29)), None),
            TickAction::None
        );
        assert_eq!(supervision.relaunches_in_window(at(33).mono), 0);
    }

    #[test]
    fn an_app_that_returns_during_the_backoff_is_adopted_not_doubled() {
        let mut supervision = running(4);
        supervision.observe(at(10), None, None);
        assert!(matches!(
            supervision.observe(at(11), Some(app(300, 11)), None),
            TickAction::Adopted { .. }
        ));
        assert_eq!(
            supervision.observe(at(20), Some(app(300, 11)), None),
            TickAction::None
        );
        assert_eq!(supervision.relaunches_in_window(at(20).mono), 0);
    }

    #[test]
    fn backoff_grows_then_the_budget_gives_up() {
        let mut supervision = running(3);
        let mut now = 0;
        let mut delays = Vec::new();
        for pid in 0..3 {
            now += 10;
            match supervision.observe(at(now), None, None) {
                TickAction::CrashScheduled { relaunch_in, .. } => {
                    delays.push(relaunch_in.as_secs());
                    now += relaunch_in.as_secs();
                },
                other => panic!("expected a scheduled relaunch, got {other:?}"),
            }
            assert_eq!(supervision.observe(at(now), None, None), TickAction::Launch);
            supervision.observe(at(now + 1), Some(app(1000 + pid, now + 1)), None);
        }
        assert_eq!(delays, vec![2, 5, 15]);
        assert_eq!(
            supervision.observe(at(now + 5), None, None),
            TickAction::GaveUp {
                attempts_in_window: 3
            }
        );
        assert!(matches!(supervision.phase, Phase::GaveUp));
        assert_eq!(
            supervision.observe(at(now + 100), None, None),
            TickAction::None
        );
    }

    #[test]
    fn the_budget_is_rolling() {
        let mut supervision = running(1);
        supervision.observe(at(10), None, None);
        assert_eq!(supervision.observe(at(12), None, None), TickAction::Launch);
        supervision.observe(at(13), Some(app(101, 13)), None);
        // Inside the window: give up.
        assert!(matches!(
            supervision.observe(at(100), None, None),
            TickAction::GaveUp { .. }
        ));
        // A manual launch is adopted; a crash after the window rolls is retried.
        supervision.observe(at(400), Some(app(102, 400)), None);
        assert!(matches!(
            supervision.observe(at(500), None, None),
            TickAction::CrashScheduled { .. }
        ));
    }

    #[test]
    fn a_launch_that_never_appears_is_a_crash_unless_quit() {
        let mut supervision = Supervision::new(4, Duration::from_secs(300));
        supervision.operator_launched(at(0));
        assert_eq!(supervision.observe(at(10), None, None), TickAction::None);
        assert!(matches!(
            supervision.observe(at(60), None, None),
            TickAction::CrashScheduled { .. }
        ));

        let mut quick_quit = Supervision::new(4, Duration::from_secs(300));
        quick_quit.operator_launched(at(0));
        assert_eq!(
            quick_quit.observe(at(5), None, Some(t(3))),
            TickAction::Quit
        );
    }

    #[test]
    fn the_appearance_timeout_starts_when_open_returns() {
        let mut supervision = Supervision::new(4, Duration::from_secs(300));
        supervision.operator_launched(at(0));
        // A 40 s preflight, then `open` returns.
        supervision.launch_issued(at(40).mono);
        assert_eq!(supervision.observe(at(60), None, None), TickAction::None);
        assert!(supervision.launch_in_progress(at(60).mono));
        assert!(matches!(
            supervision.observe(at(86), None, None),
            TickAction::CrashScheduled { .. }
        ));
        // The marker reference stays at the decision time.
        let mut quit = Supervision::new(4, Duration::from_secs(300));
        quit.operator_launched(at(0));
        quit.launch_issued(at(40).mono);
        assert_eq!(quit.observe(at(50), None, Some(t(20))), TickAction::Quit);
    }

    #[test]
    fn timers_follow_the_monotonic_clock_not_the_wall_clock() {
        let mut supervision = running(4);
        supervision.observe(at(10), None, None);
        // The wall clock steps back an hour; the relaunch is still due.
        let stepped_back = Now {
            mono: at(12).mono,
            wall: t(12) - Duration::from_secs(3600),
        };
        assert_eq!(
            supervision.observe(stepped_back, None, None),
            TickAction::Launch
        );
        // And a launch still times out after a wall-clock step forward.
        let mut launching = Supervision::new(4, Duration::from_secs(300));
        launching.operator_launched(at(0));
        let stepped_forward = Now {
            mono: at(10).mono,
            wall: t(10) + Duration::from_secs(3600),
        };
        assert_eq!(
            launching.observe(stepped_forward, None, None),
            TickAction::None
        );
        assert!(matches!(
            launching.observe(at(50), None, None),
            TickAction::CrashScheduled { .. }
        ));
    }

    #[test]
    fn a_stop_during_the_backoff_cancels_the_relaunch() {
        let mut supervision = running(4);
        supervision.observe(at(10), None, None);
        // `make stop-desktop-tray` wrote the marker after the crash.
        assert_eq!(
            supervision.observe(at(11), None, Some(t(11))),
            TickAction::Quit
        );
        assert_eq!(
            supervision.observe(at(30), None, Some(t(11))),
            TickAction::None
        );
    }

    #[test]
    fn operator_stop_and_failed_relaunch_follow_the_budget() {
        let mut supervision = running(4);
        supervision.operator_stopped();
        assert_eq!(supervision.observe(at(10), None, None), TickAction::None);

        let mut failing = running(2);
        failing.observe(at(10), None, None);
        assert_eq!(failing.observe(at(12), None, None), TickAction::Launch);
        assert!(matches!(
            failing.launch_failed(at(12)),
            TickAction::CrashScheduled {
                attempts_in_window: 1,
                ..
            }
        ));
        assert_eq!(failing.observe(at(17), None, None), TickAction::Launch);
        assert!(matches!(
            failing.launch_failed(at(17)),
            TickAction::GaveUp { .. }
        ));
        // An operator start resets the budget.
        failing.operator_launched(at(20));
        assert_eq!(failing.relaunches_in_window(at(20).mono), 0);
        assert!(failing.launch_in_progress(at(21).mono));
        assert!(!failing.launch_in_progress(at(80).mono));
    }

    #[test]
    fn boot_adopts_respects_a_quit_or_launches() {
        assert_eq!(
            boot_action(Some(app(5, 0)), true),
            BootAction::Adopt(app(5, 0))
        );
        assert_eq!(boot_action(None, true), BootAction::RespectQuit);
        assert_eq!(boot_action(None, false), BootAction::Launch);
        // Only a quit during this supervisor's life counts at boot: a marker
        // left by `make stop-all` or an earlier Quit is older than the start.
        let supervisor_started = t(1000);
        assert!(!marker_is_fresh(Some(t(400)), supervisor_started));
        assert!(marker_is_fresh(Some(t(1001)), supervisor_started));
    }

    #[test]
    fn a_quit_in_this_session_survives_a_supervisor_restart_under_login_item() {
        let booted = t(100);
        let supervisor_restarted = t(5000);
        let quit = Some(t(3000)); // tray Quit, then launchd restarted the supervisor
        let production = quit_reference(
            BootLaunchPolicy::LoginItem,
            supervisor_restarted,
            Some(booted),
        );
        assert!(marker_is_fresh(quit, production), "still a quit");
        assert_eq!(
            boot_action(None, marker_is_fresh(quit, production)),
            BootAction::RespectQuit
        );
        assert!(
            !marker_is_fresh(Some(t(50)), production),
            "a quit before this boot does not count"
        );
        let dev = quit_reference(BootLaunchPolicy::Always, supervisor_restarted, Some(booted));
        assert!(
            !marker_is_fresh(quit, dev),
            "dev: starting the supervisor asks for the app"
        );
        assert_eq!(
            quit_reference(BootLaunchPolicy::LoginItem, supervisor_restarted, None),
            supervisor_restarted,
            "no boot time: the supervisor start"
        );
        if DesktopAppProcess::is_supported() {
            let now = SystemTime::now();
            assert!(platform::boot_time().is_some_and(|booted| booted < now));
        }
    }

    #[test]
    fn a_stop_before_the_launched_process_appears_is_applied_when_it_does() {
        let mut pending = PendingStop::new(at(0).mono);
        // `open` still in flight, nothing visible yet.
        assert_eq!(
            pending.step(at(1).mono, None, true),
            PendingStopAction::Wait
        );
        pending.launch_returned(at(3).mono);
        assert_eq!(
            pending.step(at(4).mono, None, false),
            PendingStopAction::Wait
        );
        // The launched process appears: signal it once, then wait out the grace.
        assert_eq!(
            pending.step(at(5).mono, Some(app(900, 5)), false),
            PendingStopAction::Signal(app(900, 5))
        );
        assert_eq!(
            pending.step(at(7).mono, Some(app(900, 5)), false),
            PendingStopAction::Wait
        );
        assert_eq!(
            pending.step(at(11).mono, Some(app(900, 5)), false),
            PendingStopAction::Kill(app(900, 5))
        );
        assert_eq!(
            pending.step(at(12).mono, None, false),
            PendingStopAction::Done
        );
    }

    #[test]
    fn a_pending_stop_expires_when_no_process_ever_appears() {
        let mut pending = PendingStop::new(at(0).mono);
        assert_eq!(
            pending.step(at(100).mono, None, true),
            PendingStopAction::Wait,
            "never expires while `open` is still running"
        );
        pending.launch_returned(at(100).mono);
        assert_eq!(
            pending.step(at(120).mono, None, false),
            PendingStopAction::Wait
        );
        assert_eq!(
            pending.step(at(145).mono, None, false),
            PendingStopAction::Expired
        );
    }

    #[test]
    fn launching_and_a_pending_stop_show_in_status() {
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        let mut process = DesktopAppProcess::new(config);
        process.supervision.operator_launched(Now::current());
        assert!(process.supervision.launch_in_progress(Instant::now()));
        process.stop_pending = Some(PendingStop::new(Instant::now()));
        if DesktopAppProcess::is_supported() {
            assert!(process.detailed_status().stop_pending);
        }
    }

    #[test]
    fn open_args_mirror_the_makefile_launch() {
        let args = open_args(
            Path::new("/repo/.local/Magican-Debug.app"),
            Some(Path::new("/repo/magician-host-tray.log")),
            &[
                ("MAGICIAN_DESKTOP_MANAGE_RUNTIME".into(), "0".into()),
                ("MAGICIAN_PREFIX".into(), String::new()),
            ],
        );
        let args: Vec<String> = args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            strings(&[
                "-n",
                "-g",
                "--stdout",
                "/repo/magician-host-tray.log",
                "--stderr",
                "/repo/magician-host-tray.log",
                "--env",
                "MAGICIAN_DESKTOP_MANAGE_RUNTIME=0",
                "--env",
                "MAGICIAN_PREFIX=",
                "/repo/.local/Magican-Debug.app",
            ])
        );
        let bare = open_args(Path::new("/A.app"), None, &[]);
        assert_eq!(bare.len(), 3, "no log, no env: -n -g <bundle>");
    }

    #[test]
    fn every_launch_tells_the_app_which_marker_is_watched() {
        let env = vec![
            ("MAGICIAN_PREFIX".to_string(), String::new()),
            (QUIT_MARKER_ENV.to_string(), "/elsewhere".to_string()),
        ];
        let merged = launch_env(&env, Path::new("/watched/desktop-app.quit"));
        assert_eq!(
            merged,
            vec![
                ("MAGICIAN_PREFIX".to_string(), String::new()),
                (
                    QUIT_MARKER_ENV.to_string(),
                    "/watched/desktop-app.quit".to_string()
                ),
            ],
            "the watched path replaces a conflicting value"
        );
    }

    #[test]
    fn cli_flags_configure_the_desktop_and_pass_others_through() -> Result<()> {
        let mut config = SupervisorConfig::default();
        assert_eq!(config.desktop_manage, None, "the config decides by default");
        let ignored = apply_cli_args(
            &mut config,
            &strings(&[
                "--manage-desktop",
                "--desktop-app",
                "/repo/.local/Magican-Debug.app",
                "--desktop-log=/repo/tray.log",
                "--desktop-env",
                "MAGICIAN_PREFIX=",
                "--desktop-env=MAGICIAN_HOST_GATEWAY_URL=http://127.0.0.1:3017",
                "--desktop-verify-script",
                "",
                "--desktop-quit-marker",
                "/tmp/quit marker",
                "--something-else",
            ]),
        )?;
        assert_eq!(config.desktop_manage, Some(true));
        assert_eq!(
            config.desktop_app,
            Some(PathBuf::from("/repo/.local/Magican-Debug.app"))
        );
        assert_eq!(config.desktop_log, Some(PathBuf::from("/repo/tray.log")));
        assert_eq!(config.desktop_verify_script, None, "empty means unset");
        assert_eq!(
            config.desktop_quit_marker,
            Some(PathBuf::from("/tmp/quit marker"))
        );
        assert_eq!(config.desktop_env.len(), 2);
        assert_eq!(
            config.desktop_env[1].1, "http://127.0.0.1:3017",
            "only the first = splits the flag"
        );
        assert_eq!(ignored, strings(&["--something-else"]));

        apply_cli_args(&mut config, &strings(&["--manage-desktop=0"]))?;
        assert_eq!(config.desktop_manage, Some(false));
        apply_cli_args(&mut config, &strings(&["--manage-desktop="]))?;
        assert_eq!(config.desktop_manage, None, "empty leaves it to the config");
        assert_eq!(config.desktop_launch_at_boot, BootLaunchPolicy::LoginItem);
        apply_cli_args(&mut config, &strings(&["--desktop-launch-at-boot=always"]))?;
        assert_eq!(config.desktop_launch_at_boot, BootLaunchPolicy::Always);
        assert!(apply_cli_args(
            &mut config,
            &strings(&["--desktop-launch-at-boot", "never"])
        )
        .is_err());
        apply_cli_args(
            &mut config,
            &strings(&["--magician-config", "/data/magician-config.yaml"]),
        )?;
        assert_eq!(
            config.magician_config,
            Some(PathBuf::from("/data/magician-config.yaml"))
        );
        assert!(apply_cli_args(&mut config, &strings(&["--desktop-app"])).is_err());
        assert!(apply_cli_args(&mut config, &strings(&["--desktop-env", "NOEQUALS"])).is_err());
        assert!(apply_cli_args(&mut config, &strings(&["--desktop-env", "1BAD=x"])).is_err());
        Ok(())
    }

    #[test]
    fn server_mode_keeps_defaults_for_bad_flag_values_instead_of_exiting() {
        let mut config = SupervisorConfig::default();
        let (ignored, warnings) = apply_cli_args_lenient(
            &mut config,
            &strings(&[
                "--desktop-launch-at-boot=sometime-later",
                "--manage-desktop=perhaps",
                "--desktop-env",
                "1BAD=x",
                "--desktop-app",
                "/Applications/Magican.app",
                "--unknown",
                "--desktop-log",
            ]),
        );
        assert_eq!(config.desktop_launch_at_boot, BootLaunchPolicy::LoginItem);
        assert_eq!(config.desktop_manage, None);
        assert!(config.desktop_env.is_empty());
        assert_eq!(
            config.desktop_app,
            Some(PathBuf::from("/Applications/Magican.app")),
            "good flags still apply"
        );
        assert_eq!(
            config.desktop_log, None,
            "a missing value keeps the default"
        );
        assert_eq!(ignored, strings(&["--unknown"]));
        assert_eq!(warnings.len(), 4);
    }

    #[test]
    fn the_marker_is_a_fixed_per_user_file_unless_overridden() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            quit_marker_path(None, Some(home)),
            Some(PathBuf::from(
                "/Users/someone/Library/Application Support/dev.magician.desktop/desktop-app.quit"
            )),
            "independent of any runtime root"
        );
        assert_eq!(
            quit_marker_path(Some(Path::new("/x/quit")), Some(home)),
            Some(PathBuf::from("/x/quit"))
        );
        assert_eq!(quit_marker_path(None, None), None);
    }

    #[test]
    fn marker_freshness_is_relative_to_the_run() {
        assert!(marker_is_fresh(Some(t(10)), t(5)));
        assert!(marker_is_fresh(Some(t(5)), t(5)));
        assert!(!marker_is_fresh(Some(t(4)), t(5)));
        assert!(!marker_is_fresh(None, t(5)));
    }

    #[test]
    fn the_bundle_executable_comes_from_the_plist() -> Result<()> {
        let bundle =
            std::env::temp_dir().join(format!("sup-desktop-bundle-{}.app", std::process::id()));
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents)?;
        assert_eq!(
            bundle_executable(&bundle),
            contents.join("MacOS").join(DEFAULT_EXECUTABLE),
            "no plist: the default name"
        );
        std::fs::write(
            contents.join("Info.plist"),
            "<plist><dict>\n\t<key>CFBundleExecutable</key>\n\t<string>tray.bin</string>\n</dict></plist>",
        )?;
        assert_eq!(
            bundle_executable(&bundle),
            contents.join("MacOS").join("tray.bin")
        );
        let _ = std::fs::remove_dir_all(&bundle);
        Ok(())
    }

    #[test]
    fn a_crash_report_counts_only_when_written_after_the_run_began() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("sup-desktop-ips-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let before = SystemTime::now() - Duration::from_secs(5);
        std::fs::write(dir.join("magician-desktop.bin-2026-10-25-221500.ips"), "{}")?;
        std::fs::write(dir.join("other-2026-10-25-221500.ips"), "{}")?;
        assert_eq!(
            find_crash_report(&dir, "magician-desktop.bin", before),
            Some(dir.join("magician-desktop.bin-2026-10-25-221500.ips"))
        );
        assert_eq!(
            find_crash_report(
                &dir,
                "magician-desktop.bin",
                SystemTime::now() + Duration::from_secs(60)
            ),
            None,
            "a report older than the run is not this crash"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn an_in_flight_launch_refuses_another_and_expires() {
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        let mut process = DesktopAppProcess::new(config);
        let now = Instant::now();
        process.launch_in_flight = Some(now);
        assert!(process.launch_in_flight(now + Duration::from_secs(10)));
        assert!(
            !process.launch_in_flight(now + IN_FLIGHT_LIMIT),
            "an abandoned launch cannot block launches for good"
        );
        // Status reads the phase without waiting on the launch.
        process.supervision.operator_launched(Now::current());
        if DesktopAppProcess::is_supported() {
            assert_eq!(process.detailed_status().phase, "launching");
        }
    }

    #[test]
    fn manage_desktop_is_read_leniently_from_the_runtime_config() {
        let block = "enabled: true\nplane:\n  terminal:\n    hot: []\n# comment\nsupervisor:\n  # why\n  manage_desktop: false # off for now\n  future_key: 1\nunknown_root: {a: 1}\n";
        assert_eq!(parse_manage_desktop(block), Ok(Some(false)));
        assert_eq!(
            parse_manage_desktop("supervisor: {manage_desktop: yes, other: 2}\n"),
            Ok(Some(true))
        );
        assert_eq!(parse_manage_desktop("supervisor:\n  other: 1\n"), Ok(None));
        assert_eq!(
            parse_manage_desktop("plane:\n  manage_desktop: false\n"),
            Ok(None),
            "the key only counts inside `supervisor:`"
        );
        assert_eq!(parse_manage_desktop(""), Ok(None));
        assert!(parse_manage_desktop("supervisor:\n  manage_desktop: maybe\n").is_err());
        assert!(parse_manage_desktop("supervisor: 7\n").is_err());
        assert_eq!(read_manage_desktop(None), ConfigSetting::NotGiven);
        assert!(matches!(
            read_manage_desktop(Some(Path::new("/nonexistent/magician-config.yaml"))),
            ConfigSetting::Unreadable(_)
        ));
    }

    #[test]
    fn the_flag_beats_the_config_which_beats_the_default() {
        let on = ConfigSetting::Value(true);
        let off = ConfigSetting::Value(false);
        assert!(
            decide_management(true, None, &ConfigSetting::NotGiven, true).enabled,
            "default on"
        );
        assert!(decide_management(true, None, &ConfigSetting::Absent, true).enabled);
        assert!(
            decide_management(true, None, &ConfigSetting::Unparseable("x".into()), true).enabled,
            "an unusable config falls back to on"
        );
        let disabled = decide_management(true, None, &off, true);
        assert!(!disabled.enabled);
        assert!(disabled.reason.contains("supervisor.manage_desktop"));
        assert!(
            decide_management(true, Some(true), &off, true).enabled,
            "the flag wins"
        );
        assert!(
            !decide_management(true, Some(false), &on, true).enabled,
            "the flag wins"
        );
        let no_bundle = decide_management(true, None, &on, false);
        assert!(!no_bundle.enabled);
        assert!(no_bundle.reason.contains("bundle"));
        let elsewhere = decide_management(false, Some(true), &on, true);
        assert!(!elsewhere.enabled);
        assert!(elsewhere.reason.contains("macOS"));
    }

    #[test]
    fn boot_waits_for_the_login_item_before_launching() {
        let mut supervision = Supervision::new(4, Duration::from_secs(300));
        supervision.begin_boot_grace(at(0));
        assert_eq!(supervision.observe(at(3), None, None), TickAction::None);
        assert_eq!(supervision.observe(at(27), None, None), TickAction::None);
        assert_eq!(
            supervision.observe(at(30), None, None),
            TickAction::BootLaunch
        );
        assert!(!supervision.launch_in_progress(at(30).mono));

        // The login item started it during the grace: adopted, never launched.
        let mut autostarted = Supervision::new(4, Duration::from_secs(300));
        autostarted.begin_boot_grace(at(0));
        assert_eq!(
            autostarted.observe(at(9), Some(app(42, 8)), None),
            TickAction::Adopted {
                app: app(42, 8),
                previous_pid: None
            }
        );
        assert_eq!(
            autostarted.observe(at(40), Some(app(42, 8)), None),
            TickAction::None
        );

        // A stop or quit marker written during the grace keeps it closed.
        let mut stopped = Supervision::new(4, Duration::from_secs(300));
        stopped.begin_boot_grace(at(0));
        assert_eq!(stopped.observe(at(5), None, Some(t(4))), TickAction::Quit);
        assert_eq!(stopped.observe(at(40), None, Some(t(4))), TickAction::None);
    }

    #[test]
    fn the_boot_policy_respects_launch_at_login_being_off() {
        assert_eq!(
            parse_boot_launch_policy("always").ok(),
            Some(BootLaunchPolicy::Always)
        );
        assert_eq!(
            parse_boot_launch_policy("login-item").ok(),
            Some(BootLaunchPolicy::LoginItem)
        );
        assert!(parse_boot_launch_policy("sometimes").is_err());
        assert_eq!(BootLaunchPolicy::default(), BootLaunchPolicy::LoginItem);

        let plist = "<plist><dict>\n  <key>Label</key>\n  <string>Magican</string>\n  <key>RunAtLoad</key>\n  <true/>\n</dict></plist>";
        let disabled = "<plist><dict><key>Disabled</key>\n  <true/></dict></plist>";
        assert!(login_item_enabled_from(Some(plist)));
        assert!(!login_item_enabled_from(Some(disabled)));
        assert!(
            !login_item_enabled_from(None),
            "no plist = launch at login is off"
        );

        assert!(boot_launch_allowed(BootLaunchPolicy::Always, false));
        assert!(boot_launch_allowed(BootLaunchPolicy::LoginItem, true));
        assert!(!boot_launch_allowed(BootLaunchPolicy::LoginItem, false));
    }

    #[test]
    fn a_declined_boot_launch_stays_idle_but_still_relaunches_after_a_crash() {
        let mut supervision = Supervision::new(4, Duration::from_secs(300));
        supervision.begin_boot_grace(at(0));
        assert_eq!(
            supervision.observe(at(30), None, None),
            TickAction::BootLaunch
        );
        // login-item policy with the login item off.
        assert!(!boot_launch_allowed(BootLaunchPolicy::LoginItem, false));
        supervision.boot_launch_declined();
        assert_eq!(supervision.phase, Phase::Idle(IdleReason::LoginItemOff));
        assert_eq!(supervision.observe(at(60), None, None), TickAction::None);
        // The owner starts the app later: adopted, and a crash is relaunched.
        assert!(matches!(
            supervision.observe(at(500), Some(app(77, 499)), None),
            TickAction::Adopted { .. }
        ));
        assert!(matches!(
            supervision.observe(at(600), None, None),
            TickAction::CrashScheduled { .. }
        ));

        // Either policy: an app appearing during the grace is adopted, and a
        // marker written during it keeps it closed, before any policy check.
        for _policy in [BootLaunchPolicy::Always, BootLaunchPolicy::LoginItem] {
            let mut appears = Supervision::new(4, Duration::from_secs(300));
            appears.begin_boot_grace(at(0));
            assert!(matches!(
                appears.observe(at(10), Some(app(5, 9)), None),
                TickAction::Adopted { .. }
            ));
            let mut quit = Supervision::new(4, Duration::from_secs(300));
            quit.begin_boot_grace(at(0));
            assert_eq!(quit.observe(at(10), None, Some(t(9))), TickAction::Quit);
        }
    }

    #[test]
    fn another_instance_defers_launches_without_spending_the_budget() {
        // A crash relaunch is due while another bundle runs.
        let mut supervision = running(4);
        supervision.observe(at(10), None, None);
        supervision.other_running = true;
        assert_eq!(
            supervision.observe(at(12), None, None),
            TickAction::OtherInstance
        );
        assert_eq!(
            supervision.observe(at(40), None, None),
            TickAction::OtherInstance
        );
        assert_eq!(
            supervision.relaunches_in_window(at(40).mono),
            0,
            "no budget spent"
        );
        // It exits with the shared marker untouched: our relaunch conditions
        // still hold, so ours is launched.
        supervision.other_running = false;
        assert_eq!(supervision.observe(at(43), None, None), TickAction::Launch);
        // Its Quit wrote the shared marker: no launch.
        let mut quit = running(4);
        quit.observe(at(10), None, None);
        quit.other_running = true;
        assert_eq!(quit.observe(at(12), None, None), TickAction::OtherInstance);
        quit.other_running = false;
        assert_eq!(quit.observe(at(50), None, Some(t(45))), TickAction::Quit);

        // The boot grace ends while another instance runs.
        let mut boot = Supervision::new(4, Duration::from_secs(300));
        boot.begin_boot_grace(at(0));
        boot.other_running = true;
        assert_eq!(boot.observe(at(31), None, None), TickAction::OtherInstance);
        boot.other_running = false;
        assert_eq!(boot.observe(at(60), None, None), TickAction::BootLaunch);
    }

    #[test]
    fn a_launch_superseded_by_another_instance_gives_back_its_budget() {
        let mut supervision = running(4);
        supervision.observe(at(10), None, None);
        assert_eq!(supervision.observe(at(12), None, None), TickAction::Launch);
        assert_eq!(supervision.relaunches_in_window(at(12).mono), 1);
        supervision.launch_superseded(at(20), false);
        assert_eq!(supervision.relaunches_in_window(at(20).mono), 0);
        assert!(matches!(supervision.phase, Phase::RelaunchPending { .. }));
        let mut boot = Supervision::new(4, Duration::from_secs(300));
        boot.operator_launched(at(30));
        boot.launch_superseded(at(35), true);
        assert!(matches!(boot.phase, Phase::BootGrace { .. }));
    }

    #[test]
    fn the_gateway_port_comes_from_the_app_env_then_the_process_env() {
        let env = vec![(
            "MAGICIAN_HOST_GATEWAY_URL".to_string(),
            "http://127.0.0.1:4017/".to_string(),
        )];
        assert_eq!(gateway_port(&env, Some("http://127.0.0.1:5017")), 4017);
        assert_eq!(gateway_port(&[], Some("http://127.0.0.1:5017")), 5017);
        assert_eq!(gateway_port(&[], Some("not a url")), 3017);
        assert_eq!(gateway_port(&[], None), 3017);
    }

    #[test]
    fn a_missing_bundle_is_degraded_in_status_not_unhealthy_overall() {
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        config.desktop_app = Some(PathBuf::from("/nonexistent/Moved Away.app"));
        let mut process = DesktopAppProcess::new(config);
        assert_eq!(process.health(), None, "aggregate health is not failed");
        if DesktopAppProcess::is_supported() {
            let status = process.get_status();
            assert_eq!(status.status, "misconfigured");
            assert_eq!(status.health_status, "degraded");
        }
        process.begin_shutdown();
        assert!(
            process.tick_decide().is_none(),
            "no launch decisions while shutting down"
        );
    }

    #[test]
    fn an_overtaken_operator_start_ignores_the_login_item_policy() {
        assert!(!boot_launch_proceeds(
            BootLaunchPolicy::LoginItem,
            false,
            false
        ));
        assert!(
            boot_launch_proceeds(BootLaunchPolicy::LoginItem, false, true),
            "an explicit start-desktop is never declined by the login-item policy"
        );
        assert!(boot_launch_proceeds(BootLaunchPolicy::Always, false, false));
        assert_eq!(LaunchKind::Operator, LaunchKind::Operator);
    }

    #[test]
    fn shutdown_cancels_a_launch_that_has_not_reached_open() {
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        let mut process = DesktopAppProcess::new(config);
        let plan = LaunchPlan {
            bundle: PathBuf::from("/nonexistent/Magican.app"),
            executable: PathBuf::from("/nonexistent/Magican.app/Contents/MacOS/magician-desktop"),
            gateway_port: 1,
            shutting_down: Arc::clone(&process.shutting_down),
            verify_script: None,
            args: vec![],
            display: vec![],
        };
        process.begin_shutdown();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = runtime.block_on(plan.run()).expect_err("cancelled");
        assert!(error.to_string().contains("shutting down"));
    }

    #[test]
    fn manage_desktop_accepts_a_space_separated_boolean() {
        let mut config = SupervisorConfig::default();
        apply_cli_args(&mut config, &strings(&["--manage-desktop", "0"])).expect("parsed");
        assert_eq!(config.desktop_manage, Some(false));
        let ignored = apply_cli_args(
            &mut config,
            &strings(&["--manage-desktop", "--desktop-app", "/A.app"]),
        )
        .expect("parsed");
        assert_eq!(config.desktop_manage, Some(true), "bare flag = on");
        assert_eq!(config.desktop_app, Some(PathBuf::from("/A.app")));
        assert!(ignored.is_empty());
        apply_cli_args(&mut config, &strings(&["--manage-desktop", "false"])).expect("parsed");
        assert_eq!(config.desktop_manage, Some(false));
    }

    #[test]
    fn a_failed_boot_launch_is_retried_with_the_backoff() {
        let mut supervision = Supervision::new(2, Duration::from_secs(300));
        supervision.begin_boot_grace(at(0));
        assert_eq!(
            supervision.observe(at(30), None, None),
            TickAction::BootLaunch
        );
        assert!(matches!(
            supervision.launch_failed(at(30)),
            TickAction::CrashScheduled { .. }
        ));
        assert_eq!(supervision.observe(at(32), None, None), TickAction::Launch);
        assert!(matches!(
            supervision.launch_failed(at(32)),
            TickAction::CrashScheduled { .. }
        ));
        assert_eq!(supervision.observe(at(37), None, None), TickAction::Launch);
        assert!(
            matches!(supervision.launch_failed(at(37)), TickAction::GaveUp { .. }),
            "bounded by the relaunch budget"
        );
    }

    #[test]
    fn a_missing_bundle_at_boot_defers_the_boot_decision() {
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        config.desktop_app = Some(PathBuf::from("/nonexistent/Rebuilding.app"));
        let mut process = DesktopAppProcess::new(config);
        process.boot_decide();
        if DesktopAppProcess::is_supported() {
            assert!(
                process.boot_pending,
                "re-checked every tick until the bundle appears"
            );
            assert!(process.tick_decide().is_none());
            assert!(process.boot_pending);
        }
    }

    #[test]
    fn an_operator_start_or_stop_replaces_a_deferred_boot_decision() {
        let bundle =
            std::env::temp_dir().join(format!("sup-boot-pending-{}.app", std::process::id()));
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).expect("bundle");
        let mut config = SupervisorConfig::default();
        config.desktop_manage = Some(true);
        config.desktop_app = Some(bundle.clone());
        config.desktop_quit_marker = Some(bundle.join("quit-marker"));
        let mut process = DesktopAppProcess::new(config);
        if DesktopAppProcess::is_supported() {
            process.boot_pending = true;
            let _ = process.stop_begin();
            assert!(!process.boot_pending, "stop-desktop clears it");
            process.boot_pending = true;
            process.supervision.operator_launched(Now::current());
            let _ = process.start_decide();
            assert!(!process.boot_pending, "start-desktop clears it");
        }
        let _ = std::fs::remove_dir_all(&bundle);
    }

    #[test]
    fn unmanaged_or_unsupported_hosts_report_and_refuse() {
        let process = DesktopAppProcess::new(SupervisorConfig::default());
        let status = process.get_status();
        assert_eq!(status.service, "desktop");
        assert_eq!(status.pid, None);
        let expected = if DesktopAppProcess::is_supported() {
            "not_managed"
        } else {
            "unsupported"
        };
        assert_eq!(status.status, expected);
        assert_eq!(process.health(), None);
        assert!(process.ensure_manageable().is_err());
    }
}
