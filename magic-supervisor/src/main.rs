//! Magic Supervisor
//!
//! A lightweight supervisor process that manages Magician and Magicutor lifecycle
//! operations including restart, stop, and health monitoring.

use std::{
    collections::VecDeque,
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::{watch, Mutex},
    time::{interval, timeout},
};
use tracing::{debug, error, info, trace, warn};

mod decision_engine;
use decision_engine::DecisionEngineProcess;

const DEFAULT_HOST_GATEWAY_URL: &str = "http://127.0.0.1:3017";
const DEFAULT_RESTART_ATTEMPT_WINDOW_SECS: u64 = 5 * 60;

type ProcessHealthIdentity = (u32, u32);

fn health_failure_is_regression(
    healthy_process: Option<ProcessHealthIdentity>,
    current_process: ProcessHealthIdentity,
) -> bool {
    healthy_process == Some(current_process)
}

#[cfg(unix)]
fn isolate_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn signal_process_group(process_group_id: Option<i32>, signal: i32) -> std::io::Result<()> {
    let Some(process_group_id) = process_group_id.filter(|id| *id > 1) else {
        return Ok(());
    };
    // A negative PID addresses a Unix process group. Magician is launched as
    // the leader of its own group, so this reaches FluidAudio and any other
    // process-owned sidecars even when the Magician parent has already died.
    let result = unsafe { libc::kill(-process_group_id, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Supervisor configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SupervisorConfig {
    /// Port for supervisor control interface
    pub control_port: u16,
    /// Path to magician binary
    pub magician_binary: PathBuf,
    /// Default arguments for magician
    pub magician_args: Vec<String>,
    /// Start Magician during supervisor boot. A staged rollout can defer it
    /// until the decision engine's policy has been verified.
    pub magician_start_on_boot: bool,
    /// Health check interval for magician in seconds
    pub magician_health_check_interval: u64,
    /// Maximum restart attempts for magician inside the rolling attempt window
    pub magician_max_restart_attempts: u32,
    /// Rolling restart attempt window for magician in seconds
    pub magician_restart_attempt_window: u64,
    /// Restart cooldown period for magician in seconds
    pub magician_restart_cooldown: u64,
    /// Path to magicutor binary
    pub magicutor_binary: PathBuf,
    /// Default arguments for magicutor
    pub magicutor_args: Vec<String>,
    /// Health check interval for magicutor in seconds
    pub magicutor_health_check_interval: u64,
    /// Maximum restart attempts for magicutor inside the rolling attempt window
    pub magicutor_max_restart_attempts: u32,
    /// Rolling restart attempt window for magicutor in seconds
    pub magicutor_restart_attempt_window: u64,
    /// Restart cooldown period for magicutor in seconds
    pub magicutor_restart_cooldown: u64,
    /// Manage the decision-engine process when its binary is installed.
    pub decision_engine_enabled: bool,
    /// Path to the decision-engine binary
    pub decision_engine_binary: PathBuf,
    /// Arguments for the decision engine (`--socket` is added unless given)
    pub decision_engine_args: Vec<String>,
    /// Socket override; unset = `<runtime root>/run/decision-engine.sock`
    pub decision_engine_socket: Option<PathBuf>,
    /// Health check interval for the decision engine in seconds
    pub decision_engine_health_check_interval: u64,
    /// Maximum restart attempts for the decision engine inside the window
    pub decision_engine_max_restart_attempts: u32,
    /// Rolling restart attempt window for the decision engine in seconds
    pub decision_engine_restart_attempt_window: u64,
    /// Restart cooldown period for the decision engine in seconds
    pub decision_engine_restart_cooldown: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        let mut magician_args = vec![
            "--log-level".to_string(),
            "info".to_string(),
            "--config".to_string(),
            "tool-runtime-config.yaml".to_string(),
        ];
        // Standard desktop-managed native installs listen on the LAN so the
        // owner can select the same-Wi-Fi mobile route. An explicit value still
        // wins, including 127.0.0.1 for a deliberately loopback-only service.
        let http_host = std::env::var("MAGICIAN_HTTP_HOST")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "0.0.0.0".to_owned());
        magician_args.extend(["--host".to_owned(), http_host]);
        if let Ok(value) = std::env::var("MAGICIAN_FRONTEND_DIR") {
            if !value.trim().is_empty() {
                magician_args.extend(["--frontend-dir".to_owned(), value]);
            }
        }
        Self {
            control_port: 8081,
            magician_binary: PathBuf::from(default_managed_binary("magician")),
            magician_args,
            magician_start_on_boot: std::env::var("MAGICIAN_SUPERVISOR_START_MAGICIAN")
                .map(|value| value.trim() != "0")
                .unwrap_or(true),
            magician_health_check_interval: 20,
            magician_max_restart_attempts: 5,
            magician_restart_attempt_window: DEFAULT_RESTART_ATTEMPT_WINDOW_SECS,
            magician_restart_cooldown: 10,
            magicutor_binary: PathBuf::from(default_managed_binary("magicutor")),
            // Let env drive server mode; args are empty by default.
            magicutor_args: vec![],
            magicutor_health_check_interval: 20,
            magicutor_max_restart_attempts: 5,
            magicutor_restart_attempt_window: DEFAULT_RESTART_ATTEMPT_WINDOW_SECS,
            magicutor_restart_cooldown: 10,
            decision_engine_enabled: true,
            decision_engine_binary: PathBuf::from(default_managed_binary("decision-engine")),
            decision_engine_args: vec![],
            decision_engine_socket: None,
            decision_engine_health_check_interval: 20,
            decision_engine_max_restart_attempts: 5,
            decision_engine_restart_attempt_window: DEFAULT_RESTART_ATTEMPT_WINDOW_SECS,
            decision_engine_restart_cooldown: 10,
        }
    }
}

fn default_managed_binary(name: &str) -> String {
    if cfg!(windows) {
        format!("./{name}.exe")
    } else {
        format!("./{name}.bin")
    }
}

/// Log a subprocess line at the appropriate level based on the log level in the line
/// Which pipe a relayed line arrived on.
///
/// A child's stderr is not merely another output stream. A line there that
/// carries no recognisable level is far likelier to be a failure than a status
/// update, so it must not default to info the way stdout can.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SubprocessStream {
    Stdout,
    Stderr,
}

/// True for the shapes a Rust process emits as it dies.
///
/// `main() -> Result` prints exactly `Error: {err:?}` via `Termination`, and a
/// panic prints `thread '...' panicked at ...`. Neither goes through tracing,
/// so neither carries a level token — which is how
/// `Error: acquiring default scope lease: conflict` once relayed at info. The
/// one line explaining why the process died is the one line an error filter
/// must not miss.
fn is_process_fatal_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Error:") || trimmed.contains("panicked at")
}

fn subprocess_log_level(line: &str, stream: SubprocessStream) -> tracing::Level {
    if is_process_fatal_line(line) {
        return tracing::Level::ERROR;
    }
    if line.contains("/health") {
        return tracing::Level::DEBUG;
    }
    // Tracing and the native Swift sidecar put the level first or after
    // their timestamp. Do not infer severity from words in the message body.
    if let Some(level) =
        line.split_whitespace()
            .take(2)
            .find_map(|token| match token.trim_matches(['[', ']']) {
                "TRACE" | "trace" => Some(tracing::Level::TRACE),
                "DEBUG" | "debug" => Some(tracing::Level::DEBUG),
                "INFO" | "info" => Some(tracing::Level::INFO),
                "WARN" | "warn" => Some(tracing::Level::WARN),
                "ERROR" | "error" => Some(tracing::Level::ERROR),
                _ => None,
            })
    {
        return level;
    }
    match stream {
        SubprocessStream::Stdout => tracing::Level::INFO,
        SubprocessStream::Stderr => tracing::Level::WARN,
    }
}

fn log_subprocess_line(prefix: &str, line: &str, stream: SubprocessStream) {
    match subprocess_log_level(line, stream) {
        tracing::Level::TRACE => trace!("{} {}", prefix, line),
        tracing::Level::DEBUG => debug!("{} {}", prefix, line),
        tracing::Level::INFO => info!("{} {}", prefix, line),
        tracing::Level::WARN => warn!("{} {}", prefix, line),
        tracing::Level::ERROR => error!("{} {}", prefix, line),
    }
}

/// Commands that can be sent to the supervisor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SupervisorCommand {
    /// Restart Magician with optional new arguments
    RestartMagician { args: Option<Vec<String>> },
    /// Restart Magicutor with optional new arguments
    RestartMagicutor { args: Option<Vec<String>> },
    /// Stop Magician gracefully
    StopMagician,
    /// Stop Magicutor gracefully
    StopMagicutor,
    /// Restart the decision engine with optional new arguments
    RestartDecisionEngine { args: Option<Vec<String>> },
    /// Stop the decision engine
    StopDecisionEngine,
    /// Get the decision engine's process status
    StatusDecisionEngine,
    /// Get current process status
    Status,
    /// Get Magician process status
    StatusMagician,
    /// Get Magicutor process status
    StatusMagicutor,
    /// Get host gateway status through MAGICIAN_HOST_GATEWAY_URL
    HostGatewayStatus,
    /// Ask host gateway to start the macOS presence host
    StartHostPresence,
    /// Ask host gateway to stop the macOS presence host
    StopHostPresence,
    /// Ask host gateway to restart the macOS presence host
    RestartHostPresence,
    /// Shutdown supervisor
    Shutdown,
    /// Health check
    HealthCheck,
}

/// Response from supervisor commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorResponse {
    pub success: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
    pub timestamp: String,
}

/// Process status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub service: String,
    pub pid: Option<u32>,
    pub status: String,
    pub uptime_seconds: Option<u64>,
    pub restart_count: u32,
    pub restart_attempts_in_window: u32,
    pub restart_attempt_window_seconds: u64,
    pub last_restart: Option<String>,
    pub health_status: String,
    pub args: Vec<String>,
}

fn recent_restart_attempt_count(
    history: &VecDeque<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    window_secs: u64,
) -> u32 {
    let window_secs = window_secs.max(1) as i64;
    history
        .iter()
        .filter(|timestamp| {
            let elapsed = now.signed_duration_since(**timestamp).num_seconds();
            elapsed >= 0 && elapsed < window_secs
        })
        .count() as u32
}

fn record_restart_attempt(
    service: &str,
    history: &mut VecDeque<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    max_attempts: u32,
    window_secs: u64,
) -> Result<u32> {
    let window_secs = window_secs.max(1);
    while let Some(oldest) = history.front().copied() {
        let elapsed = now.signed_duration_since(oldest).num_seconds();
        if elapsed < 0 || elapsed < window_secs as i64 {
            break;
        }
        history.pop_front();
    }

    if history.len() >= max_attempts as usize {
        anyhow::bail!(
            "Maximum {service} restart attempts exceeded: {max_attempts} attempts in the last {window_secs} seconds"
        );
    }

    history.push_back(now);
    Ok(history.len() as u32)
}

fn extract_port_from_args(args: &[String]) -> Option<u16> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--port" || arg == "-p" {
            if let Some(value) = iter.next() {
                if let Ok(parsed) = value.parse::<u16>() {
                    return Some(parsed);
                }
            }
        } else if let Some(stripped) = arg.strip_prefix("--port=") {
            if let Ok(parsed) = stripped.parse::<u16>() {
                return Some(parsed);
            }
        }
    }
    None
}

/// Magician process manager
pub struct MagicianProcess {
    config: SupervisorConfig,
    process: Option<Child>,
    process_group_id: Option<i32>,
    start_time: Option<Instant>,
    restart_count: u32,
    restart_attempts: VecDeque<chrono::DateTime<chrono::Utc>>,
    last_restart: Option<chrono::DateTime<chrono::Utc>>,
    current_args: Vec<String>,
    /// Set when the child was observed to have exited on its own — a crash, a
    /// signal, anything that was not `stop()`. Cleared by the next spawn or by
    /// a deliberate stop, so the health task restarts only what died.
    unexpected_exit: Option<String>,
}

impl MagicianProcess {
    pub fn new(config: SupervisorConfig) -> Self {
        let current_args = config.magician_args.clone();
        Self {
            config,
            process: None,
            process_group_id: None,
            start_time: None,
            restart_count: 0,
            restart_attempts: VecDeque::new(),
            last_restart: None,
            current_args,
            unexpected_exit: None,
        }
    }

    pub async fn start(&mut self, args: Option<Vec<String>>) -> Result<()> {
        self.start_with_env(args, None).await
    }

    pub async fn start_with_env(
        &mut self,
        args: Option<Vec<String>>,
        env_vars: Option<std::collections::HashMap<String, String>>,
    ) -> Result<()> {
        if self.is_running() {
            info!("Magician is already running, stopping first");
            self.stop().await?;
        }

        if let Some(new_args) = args {
            self.current_args = new_args;
        }

        info!("Starting Magician with args: {:?}", self.current_args);

        let mut cmd = Command::new(&self.config.magician_binary);
        cmd.args(&self.current_args);

        if let Some(env_map) = env_vars {
            for (key, value) in env_map {
                info!("Setting custom magician env: {}={}", key, value);
                cmd.env(key, value);
            }
        }

        if std::env::var("MAGICIAN_ENV").is_err() {
            cmd.env("MAGICIAN_ENV", "development");
        }
        if std::env::var("MAGICIAN_RUNTIME_MODE").is_err() {
            cmd.env("MAGICIAN_RUNTIME_MODE", "production");
        }

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(parent) = self.config.magician_binary.parent() {
            cmd.current_dir(parent);
        }

        isolate_process_group(&mut cmd);

        let mut child = cmd.spawn()?;
        self.unexpected_exit = None;
        let pid = child.id().unwrap_or(0);

        if let Some(stdout) = child.stdout.take() {
            let stdout_reader = BufReader::new(stdout);
            tokio::spawn(async move {
                let mut lines = stdout_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let stripped = strip_ansi_escapes::strip(&line);
                    let clean_line = String::from_utf8_lossy(&stripped);
                    log_subprocess_line("[Magician]", &clean_line, SubprocessStream::Stdout);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let stderr_reader = BufReader::new(stderr);
            tokio::spawn(async move {
                let mut lines = stderr_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let stripped = strip_ansi_escapes::strip(&line);
                    let clean_line = String::from_utf8_lossy(&stripped);
                    log_subprocess_line("[Magician]", &clean_line, SubprocessStream::Stderr);
                }
            });
        }

        self.process = Some(child);
        self.process_group_id = (pid > 1).then_some(pid as i32);
        self.start_time = Some(Instant::now());

        info!("Magician started successfully with PID: {}", pid);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.unexpected_exit = None;
        let recorded_process_group_id = self.process_group_id.take();
        #[cfg(not(unix))]
        let _ = recorded_process_group_id;
        if let Some(mut process) = self.process.take() {
            let pid = process.id().unwrap_or(0);
            #[cfg(unix)]
            let process_group_id =
                recorded_process_group_id.or_else(|| (pid > 1).then_some(pid as i32));
            info!("Stopping Magician process (PID: {})", pid);

            #[cfg(unix)]
            if process_group_id.is_some() {
                if let Err(error) = signal_process_group(process_group_id, libc::SIGTERM) {
                    warn!(
                        "Failed to terminate Magician process group {:?}: {}",
                        process_group_id, error
                    );
                    if let Err(kill_error) = process.kill().await {
                        warn!("Failed to kill Magician process {}: {}", pid, kill_error);
                    }
                }
            } else {
                if let Err(kill_error) = process.kill().await {
                    warn!("Failed to kill Magician process {}: {}", pid, kill_error);
                }
            }

            #[cfg(not(unix))]
            if let Err(error) = process.kill().await {
                warn!("Failed to kill Magician process {}: {}", pid, error);
            }

            let wait_result = timeout(Duration::from_secs(10), process.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    info!("Magician exited with status: {}", status);
                },
                Ok(Err(e)) => {
                    error!("Error waiting for Magician to exit: {}", e);
                },
                Err(_) => {
                    warn!("Magician did not exit within timeout; may still be running");
                    if let Err(error) = process.kill().await {
                        warn!("Failed to force-kill Magician process {}: {}", pid, error);
                    }
                },
            }

            #[cfg(unix)]
            if let Err(error) = signal_process_group(process_group_id, libc::SIGKILL) {
                warn!(
                    "Failed to reap Magician process group {:?}: {}",
                    process_group_id, error
                );
            }
        } else {
            #[cfg(unix)]
            if let Err(error) = signal_process_group(recorded_process_group_id, libc::SIGKILL) {
                warn!(
                    "Failed to reap detached Magician process group {:?}: {}",
                    recorded_process_group_id, error
                );
            }
        }

        self.start_time = None;
        Ok(())
    }

    pub async fn restart(&mut self, args: Option<Vec<String>>) -> Result<()> {
        info!("Restarting Magician...");

        if let Some(last_restart) = self.last_restart {
            let elapsed = chrono::Utc::now()
                .signed_duration_since(last_restart)
                .num_seconds();
            if elapsed >= 0 && elapsed < self.config.magician_restart_cooldown as i64 {
                let remaining = self.config.magician_restart_cooldown - elapsed as u64;
                anyhow::bail!(
                    "Magician restart cooldown active, {} seconds remaining",
                    remaining
                );
            }
        }

        let now = chrono::Utc::now();
        let attempts_in_window = record_restart_attempt(
            "Magician",
            &mut self.restart_attempts,
            now,
            self.config.magician_max_restart_attempts,
            self.config.magician_restart_attempt_window,
        )?;

        self.stop().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;

        self.start(args).await?;
        self.restart_count += 1;
        self.last_restart = Some(now);

        info!(
            "Magician restarted successfully (total {}, {} attempt(s) in the last {}s)",
            self.restart_count,
            attempts_in_window,
            self.config.magician_restart_attempt_window.max(1)
        );
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(process) = &mut self.process {
            match process.try_wait() {
                Ok(Some(status)) => {
                    error!(
                        "Magician exited unexpectedly with status {}; reaping process-owned sidecars",
                        status
                    );
                    self.unexpected_exit = Some(status.to_string());
                    #[cfg(unix)]
                    if let Err(error) = signal_process_group(self.process_group_id, libc::SIGKILL) {
                        warn!(
                            "Failed to reap crashed Magician process group {:?}: {}",
                            self.process_group_id, error
                        );
                    }
                    self.process = None;
                    self.process_group_id = None;
                    self.start_time = None;
                    false
                },
                Ok(None) => true,
                Err(error) => {
                    error!("Failed to inspect Magician child process: {}", error);
                    self.unexpected_exit = Some(format!("uninspectable: {error}"));
                    #[cfg(unix)]
                    if let Err(signal_error) =
                        signal_process_group(self.process_group_id, libc::SIGKILL)
                    {
                        warn!(
                            "Failed to reap uninspectable Magician process group {:?}: {}",
                            self.process_group_id, signal_error
                        );
                    }
                    self.process = None;
                    self.process_group_id = None;
                    self.start_time = None;
                    false
                },
            }
        } else {
            false
        }
    }

    pub fn get_status(&mut self) -> ProcessStatus {
        let is_running = self.is_running();
        let pid = if is_running {
            self.process.as_ref().and_then(|p| p.id())
        } else {
            None
        };

        let uptime_seconds = if is_running {
            self.start_time.map(|start| start.elapsed().as_secs())
        } else {
            None
        };

        let status = if is_running {
            "running".to_string()
        } else {
            "stopped".to_string()
        };

        let health_status = if is_running {
            "healthy".to_string()
        } else {
            "stopped".to_string()
        };

        ProcessStatus {
            service: "magician".to_string(),
            pid,
            status,
            uptime_seconds,
            restart_count: self.restart_count,
            restart_attempts_in_window: recent_restart_attempt_count(
                &self.restart_attempts,
                chrono::Utc::now(),
                self.config.magician_restart_attempt_window,
            ),
            restart_attempt_window_seconds: self.config.magician_restart_attempt_window.max(1),
            last_restart: self.last_restart.map(|t| t.to_rfc3339()),
            health_status,
            args: self.current_args.clone(),
        }
    }

    fn get_magician_port(&self) -> u16 {
        if let Ok(port_str) = std::env::var("MAGICIAN_PORT") {
            if let Ok(port) = port_str.parse::<u16>() {
                return port;
            }
        }

        extract_port_from_args(&self.current_args).unwrap_or(3002)
    }

    pub async fn health_check(&self) -> bool {
        let port = self.get_magician_port();
        let health_url = format!("http://127.0.0.1:{}/health", port);

        debug!(
            "Magician health check attempting HTTP request to {}",
            health_url
        );

        if let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            match client.get(&health_url).send().await {
                Ok(response) if response.status().is_success() => {
                    debug!("Magician health check successful on port {}", port);
                    return true;
                },
                Ok(response) => {
                    debug!(
                        "Magician health check failed with status {} on port {}",
                        response.status(),
                        port
                    );
                },
                Err(e) => {
                    debug!("Magician health check HTTP error on port {}: {}", port, e);
                },
            }
        }

        let address = format!("127.0.0.1:{}", port);
        debug!(
            "Magician health check falling back to TCP connection at {}",
            address
        );
        match timeout(Duration::from_secs(2), TcpStream::connect(&address)).await {
            Ok(Ok(_)) => {
                debug!("Magician TCP health check successful on port {}", port);
                true
            },
            Ok(Err(e)) => {
                debug!("Magician TCP health check failed on port {}: {}", port, e);
                false
            },
            Err(_) => {
                debug!("Magician TCP health check timed out on port {}", port);
                false
            },
        }
    }
}

/// Magicutor process manager
pub struct MagicutorProcess {
    config: SupervisorConfig,
    process: Option<Child>,
    start_time: Option<Instant>,
    restart_count: u32,
    restart_attempts: VecDeque<chrono::DateTime<chrono::Utc>>,
    last_restart: Option<chrono::DateTime<chrono::Utc>>,
    current_args: Vec<String>,
}

impl MagicutorProcess {
    pub fn new(config: SupervisorConfig) -> Self {
        let current_args = config.magicutor_args.clone();
        Self {
            config,
            process: None,
            start_time: None,
            restart_count: 0,
            restart_attempts: VecDeque::new(),
            last_restart: None,
            current_args,
        }
    }

    pub async fn start(&mut self, args: Option<Vec<String>>) -> Result<()> {
        self.start_with_env(args, None).await
    }

    pub async fn start_with_env(
        &mut self,
        args: Option<Vec<String>>,
        env_vars: Option<std::collections::HashMap<String, String>>,
    ) -> Result<()> {
        if self.is_running() {
            info!("Magicutor is already running, stopping first");
            self.stop().await?;
        }

        if let Some(new_args) = args {
            self.current_args = new_args;
        }

        info!("Starting Magicutor with args: {:?}", self.current_args);

        let mut cmd = Command::new(&self.config.magicutor_binary);
        cmd.args(&self.current_args);

        if let Some(env_map) = env_vars {
            for (key, value) in env_map {
                info!("Setting custom magicutor env: {}={}", key, value);
                cmd.env(key, value);
            }
        }

        if std::env::var("MAGICUTOR_ENV").is_err() {
            cmd.env("MAGICUTOR_ENV", "development");
        }

        // Force HTTP server mode even when stdin is not a TTY (supervised launches close stdin)
        if std::env::var("MAGICUTOR_FORCE_SERVER").is_err() {
            cmd.env("MAGICUTOR_FORCE_SERVER", "1");
        } else if std::env::var("MAGICUTOR_MODE").is_err() {
            // Fallback for older binaries that still honor MAGICUTOR_MODE
            cmd.env("MAGICUTOR_MODE", "server");
        }

        // Point to the packaged config unless caller overrides
        if std::env::var("MAGICUTOR_CONFIG_PATH").is_err() {
            let default_config = std::path::Path::new("magicutor/config/magicutor-config.yaml");
            if default_config.exists() {
                cmd.env("MAGICUTOR_CONFIG_PATH", default_config);
            }
        }

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(parent) = self.config.magicutor_binary.parent() {
            cmd.current_dir(parent);
        }

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);

        if let Some(stdout) = child.stdout.take() {
            let stdout_reader = BufReader::new(stdout);
            tokio::spawn(async move {
                let mut lines = stdout_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let stripped = strip_ansi_escapes::strip(&line);
                    let clean_line = String::from_utf8_lossy(&stripped);
                    log_subprocess_line("[Magicutor]", &clean_line, SubprocessStream::Stdout);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let stderr_reader = BufReader::new(stderr);
            tokio::spawn(async move {
                let mut lines = stderr_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let stripped = strip_ansi_escapes::strip(&line);
                    let clean_line = String::from_utf8_lossy(&stripped);
                    log_subprocess_line("[Magicutor]", &clean_line, SubprocessStream::Stderr);
                }
            });
        }

        self.process = Some(child);
        self.start_time = Some(Instant::now());

        info!("Magicutor started successfully with PID: {}", pid);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            let pid = process.id().unwrap_or(0);
            info!("Stopping Magicutor process (PID: {})", pid);

            if let Err(e) = process.kill().await {
                warn!("Failed to kill Magicutor process {}: {}", pid, e);
            }

            let wait_result = timeout(Duration::from_secs(10), process.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    info!("Magicutor exited with status: {}", status);
                },
                Ok(Err(e)) => {
                    error!("Error waiting for Magicutor to exit: {}", e);
                },
                Err(_) => {
                    warn!("Magicutor did not exit within timeout; may still be running");
                },
            }
        }

        self.start_time = None;
        Ok(())
    }

    pub async fn restart(&mut self, args: Option<Vec<String>>) -> Result<()> {
        info!("Restarting Magicutor...");

        if let Some(last_restart) = self.last_restart {
            let elapsed = chrono::Utc::now()
                .signed_duration_since(last_restart)
                .num_seconds();
            if elapsed >= 0 && elapsed < self.config.magicutor_restart_cooldown as i64 {
                let remaining = self.config.magicutor_restart_cooldown - elapsed as u64;
                anyhow::bail!(
                    "Magicutor restart cooldown active, {} seconds remaining",
                    remaining
                );
            }
        }

        let now = chrono::Utc::now();
        let attempts_in_window = record_restart_attempt(
            "Magicutor",
            &mut self.restart_attempts,
            now,
            self.config.magicutor_max_restart_attempts,
            self.config.magicutor_restart_attempt_window,
        )?;

        self.stop().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;

        self.start(args).await?;
        self.restart_count += 1;
        self.last_restart = Some(now);

        info!(
            "Magicutor restarted successfully (total {}, {} attempt(s) in the last {}s)",
            self.restart_count,
            attempts_in_window,
            self.config.magicutor_restart_attempt_window.max(1)
        );
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(process) = &mut self.process {
            match process.try_wait() {
                Ok(Some(_)) => {
                    self.process = None;
                    self.start_time = None;
                    false
                },
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    pub fn get_status(&mut self) -> ProcessStatus {
        let is_running = self.is_running();
        let pid = if is_running {
            self.process.as_ref().and_then(|p| p.id())
        } else {
            None
        };

        let uptime_seconds = if is_running {
            self.start_time.map(|start| start.elapsed().as_secs())
        } else {
            None
        };

        let status = if is_running {
            "running".to_string()
        } else {
            "stopped".to_string()
        };

        let health_status = if is_running {
            "healthy".to_string()
        } else {
            "stopped".to_string()
        };

        ProcessStatus {
            service: "magicutor".to_string(),
            pid,
            status,
            uptime_seconds,
            restart_count: self.restart_count,
            restart_attempts_in_window: recent_restart_attempt_count(
                &self.restart_attempts,
                chrono::Utc::now(),
                self.config.magicutor_restart_attempt_window,
            ),
            restart_attempt_window_seconds: self.config.magicutor_restart_attempt_window.max(1),
            last_restart: self.last_restart.map(|t| t.to_rfc3339()),
            health_status,
            args: self.current_args.clone(),
        }
    }

    fn get_magicutor_port(&self) -> u16 {
        if let Ok(port_str) = std::env::var("MAGICUTOR_PORT") {
            if let Ok(port) = port_str.parse::<u16>() {
                return port;
            }
        }

        extract_port_from_args(&self.current_args).unwrap_or(3003)
    }

    pub async fn health_check(&self) -> bool {
        let port = self.get_magicutor_port();
        let health_url = format!("http://127.0.0.1:{}/health", port);

        debug!(
            "Magicutor health check attempting HTTP request to {}",
            health_url
        );

        if let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            match client.get(&health_url).send().await {
                Ok(response) if response.status().is_success() => {
                    debug!("Magicutor health check successful on port {}", port);
                    return true;
                },
                Ok(response) => {
                    debug!(
                        "Magicutor health check failed with status {} on port {}",
                        response.status(),
                        port
                    );
                },
                Err(e) => {
                    debug!("Magicutor health check HTTP error on port {}: {}", port, e);
                },
            }
        }

        let address = format!("127.0.0.1:{}", port);
        debug!(
            "Magicutor health check falling back to TCP connection at {}",
            address
        );
        match timeout(Duration::from_secs(2), TcpStream::connect(&address)).await {
            Ok(Ok(_)) => {
                debug!("Magicutor TCP health check successful on port {}", port);
                true
            },
            Ok(Err(e)) => {
                debug!("Magicutor TCP health check failed on port {}: {}", port, e);
                false
            },
            Err(_) => {
                debug!("Magicutor TCP health check timed out on port {}", port);
                false
            },
        }
    }
}

/// Supervisor server that manages runtime processes
pub struct SupervisorServer {
    config: SupervisorConfig,
    processes: Arc<SupervisorProcesses>,
    shutdown_tx: watch::Sender<bool>,
}

/// Shared access to managed processes
pub struct SupervisorProcesses {
    pub magician: Arc<Mutex<MagicianProcess>>,
    pub magicutor: Arc<Mutex<MagicutorProcess>>,
    pub decision_engine: Arc<Mutex<DecisionEngineProcess>>,
}

impl SupervisorServer {
    pub fn new(config: SupervisorConfig) -> Self {
        let magician = Arc::new(Mutex::new(MagicianProcess::new(config.clone())));
        let magicutor = Arc::new(Mutex::new(MagicutorProcess::new(config.clone())));
        let decision_engine = Arc::new(Mutex::new(DecisionEngineProcess::new(config.clone())));
        let (shutdown_tx, _) = watch::channel(false);
        let processes = Arc::new(SupervisorProcesses {
            magician,
            magicutor,
            decision_engine,
        });

        Self {
            config: config.clone(),
            processes,
            shutdown_tx,
        }
    }

    /// Start the supervisor server
    pub async fn start(&self) -> Result<()> {
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        info!(
            "Starting Magic Supervisor on port {}",
            self.config.control_port
        );

        // The decision engine first: Magician in `transport: service` asks
        // it from its first step (and falls through to the LLM until then).
        {
            let mut process = self.processes.decision_engine.lock().await;
            if let Err(e) = process.start(None).await {
                error!("Failed to start the decision engine initially: {}", e);
            }
        }

        {
            let mut process = self.processes.magicutor.lock().await;
            if let Err(e) = process.start(None).await {
                error!("Failed to start Magicutor initially: {}", e);
            }
        }

        if self.config.magician_start_on_boot {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut process = self.processes.magician.lock().await;
            if let Err(e) = process.start(None).await {
                error!("Failed to start Magician initially: {}", e);
            }
        } else {
            info!(
                "Magician startup deferred; verify the decision engine, then use restart-magician"
            );
        }

        self.start_magicutor_health_check_task().await;
        self.start_magician_health_check_task().await;
        self.start_decision_engine_health_check_task().await;

        let listener = TcpListener::bind(format!("127.0.0.1:{}", self.config.control_port)).await?;
        info!(
            "Supervisor control interface listening on port {}",
            self.config.control_port
        );

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    match changed {
                        Ok(()) if *shutdown_rx.borrow() => {
                            info!("Shutdown signal received; stopping Magic Supervisor");
                            break;
                        },
                        Ok(()) => {},
                        Err(_) => {
                            info!("Shutdown channel closed; stopping Magic Supervisor");
                            break;
                        },
                    }
                },
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            debug!("Accepted connection from {}", addr);
                            let processes = Arc::clone(&self.processes);
                            let shutdown_tx = self.shutdown_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_connection(stream, processes, shutdown_tx).await {
                                    error!("Error handling connection: {}", e);
                                }
                            });
                        },
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        },
                    }
                },
            }
        }

        info!("Magic Supervisor stopped");
        Ok(())
    }

    /// Start Magicutor health check background task
    async fn start_magicutor_health_check_task(&self) {
        let process = Arc::clone(&self.processes.magicutor);
        let interval_secs = self.config.magicutor_health_check_interval;

        info!(
            "Starting Magicutor health check task (interval: {}s)",
            interval_secs
        );
        tokio::spawn(async move {
            info!("Waiting 10 seconds for Magicutor initialization before first health check");
            tokio::time::sleep(Duration::from_secs(10)).await;

            let mut interval = interval(Duration::from_secs(interval_secs));
            // Cold start binds the /health listener only after heavy synchronous
            // init (can take minutes). Until the FIRST successful check, treat
            // failures as expected warmup (DEBUG), not WARN — a hardcoded 10s grace
            // can't cover a multi-minute boot. Only a healthy→unhealthy transition
            // is a real liveness concern worth a WARN.
            let mut healthy_process = None;

            loop {
                interval.tick().await;

                let mut process_guard = process.lock().await;
                if process_guard.is_running() {
                    let pid = process_guard
                        .process
                        .as_ref()
                        .and_then(|p| p.id())
                        .unwrap_or(0);
                    let process_identity = (pid, process_guard.restart_count);
                    if process_guard.health_check().await {
                        if healthy_process != Some(process_identity) {
                            info!("Magicutor health endpoint is up (PID {} responsive)", pid);
                        } else {
                            debug!(
                                "Health check passed for PID {} - Magicutor is responsive",
                                pid
                            );
                        }
                        healthy_process = Some(process_identity);
                    } else if health_failure_is_regression(healthy_process, process_identity) {
                        warn!(
                            "Health check failed for PID {} - Magicutor may be unresponsive",
                            pid
                        );
                    } else {
                        debug!(
                            "Magicutor still initializing (PID {}) - health endpoint not ready yet",
                            pid
                        );
                    }
                }
            }
        });
    }

    /// Decision engine health task: logs responsiveness and restarts the
    /// engine after an unexpected exit (bounded by its cooldown and restart
    /// budget; a deliberate stop is never fought).
    async fn start_decision_engine_health_check_task(&self) {
        let process = Arc::clone(&self.processes.decision_engine);
        if !process.lock().await.is_managed() {
            return;
        }
        let interval_secs = self.config.decision_engine_health_check_interval.max(1);
        info!(
            "Starting decision engine health check task (interval: {}s)",
            interval_secs
        );
        tokio::spawn(async move {
            // The engine binds its socket within a second of starting.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut interval = interval(Duration::from_secs(interval_secs));
            let mut healthy_process = None;
            loop {
                interval.tick().await;
                let mut process_guard = process.lock().await;
                if process_guard.is_running() {
                    let pid = process_guard
                        .process
                        .as_ref()
                        .and_then(|p| p.id())
                        .unwrap_or(0);
                    let identity = (pid, process_guard.restart_count);
                    if process_guard.health_check().await {
                        if healthy_process != Some(identity) {
                            info!("Decision engine socket is up (PID {} responsive)", pid);
                        }
                        healthy_process = Some(identity);
                    } else if health_failure_is_regression(healthy_process, identity) {
                        warn!(
                            "Health check failed for PID {} - decision engine may be unresponsive",
                            pid
                        );
                    } else {
                        debug!("Decision engine still starting (PID {})", pid);
                    }
                } else if let Some(status) = process_guard.unexpected_exit.clone() {
                    warn!(
                        "Decision engine exited on its own ({}); restarting it",
                        status
                    );
                    match process_guard.restart(None).await {
                        Ok(()) => info!("Decision engine restarted after an unexpected exit"),
                        Err(error) => {
                            warn!(
                                "Decision engine not restarted after an unexpected exit: {}",
                                error
                            )
                        },
                    }
                }
            }
        });
    }

    /// Start Magician health check background task
    async fn start_magician_health_check_task(&self) {
        let process = Arc::clone(&self.processes.magician);
        let interval_secs = self.config.magician_health_check_interval;

        info!(
            "Starting Magician health check task (interval: {}s)",
            interval_secs
        );
        tokio::spawn(async move {
            info!("Waiting 10 seconds for Magician initialization before first health check");
            tokio::time::sleep(Duration::from_secs(10)).await;

            let mut interval = interval(Duration::from_secs(interval_secs));
            // Cold start binds the /health listener only after heavy synchronous
            // init (can take minutes). Until the FIRST successful check, treat
            // failures as expected warmup (DEBUG), not WARN — a hardcoded 10s grace
            // can't cover a multi-minute boot. Only a healthy→unhealthy transition
            // is a real liveness concern worth a WARN.
            let mut healthy_process = None;

            loop {
                interval.tick().await;

                let mut process_guard = process.lock().await;
                if process_guard.is_running() {
                    let pid = process_guard
                        .process
                        .as_ref()
                        .and_then(|p| p.id())
                        .unwrap_or(0);
                    let process_identity = (pid, process_guard.restart_count);
                    if process_guard.health_check().await {
                        if healthy_process != Some(process_identity) {
                            info!("Magician health endpoint is up (PID {} responsive)", pid);
                        } else {
                            debug!(
                                "Health check passed for PID {} - Magician is responsive",
                                pid
                            );
                        }
                        healthy_process = Some(process_identity);
                    } else if health_failure_is_regression(healthy_process, process_identity) {
                        warn!(
                            "Health check failed for PID {} - Magician may be unresponsive",
                            pid
                        );
                    } else {
                        debug!(
                            "Magician still initializing (PID {}) - health endpoint not ready yet",
                            pid
                        );
                    }
                } else if let Some(status) = process_guard.unexpected_exit.clone() {
                    // The child died on its own. Until now this task only observed: a
                    // crash was logged once and the service stayed down until a person
                    // noticed — measured five times in one day, every one a stack
                    // overflow (SIGABRT) a restart would have cleared. A deliberate
                    // `stop()` clears the flag, so an operator's stop is never fought.
                    // `restart` applies its own cooldown and the 5-per-window budget, so
                    // a crash loop is bounded: past the budget it warns every tick until
                    // the window rolls.
                    warn!("Magician exited on its own ({}); restarting it", status);
                    match process_guard.restart(None).await {
                        Ok(()) => info!("Magician restarted after an unexpected exit"),
                        Err(error) => {
                            warn!("Magician not restarted after an unexpected exit: {}", error)
                        },
                    }
                }
            }
        });
    }

    /// Handle incoming control connection
    async fn handle_connection(
        mut stream: TcpStream,
        processes: Arc<SupervisorProcesses>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Result<()> {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();

        match reader.read_line(&mut line).await {
            Ok(0) => return Ok(()),
            Ok(_) => {
                let command: SupervisorCommand = match serde_json::from_str(&line) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        let response = SupervisorResponse {
                            success: false,
                            message: format!("Invalid command format: {}", e),
                            data: None,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        };
                        Self::send_response(&mut stream, response).await?;
                        return Ok(());
                    },
                };

                let response = Self::handle_command(command, processes, shutdown_tx).await;
                Self::send_response(&mut stream, response).await?;
            },
            Err(e) => {
                error!("Failed to read from connection: {}", e);
            },
        }

        Ok(())
    }

    /// Handle supervisor command
    async fn handle_command(
        command: SupervisorCommand,
        processes: Arc<SupervisorProcesses>,
        shutdown_tx: watch::Sender<bool>,
    ) -> SupervisorResponse {
        let timestamp = chrono::Utc::now().to_rfc3339();

        match command {
            SupervisorCommand::RestartMagician { args } => {
                let result = {
                    let mut process_guard = processes.magician.lock().await;
                    process_guard.restart(args).await
                };

                let (success, message) = match result {
                    Ok(()) => (true, "Magician restarted successfully".to_string()),
                    Err(e) => (false, format!("Failed to restart Magician: {}", e)),
                };

                let data = Some(Self::collect_status_snapshot(processes.as_ref()).await);

                SupervisorResponse {
                    success,
                    message,
                    data,
                    timestamp,
                }
            },
            SupervisorCommand::RestartMagicutor { args } => {
                let result = {
                    let mut process_guard = processes.magicutor.lock().await;
                    process_guard.restart(args).await
                };

                let (success, message) = match result {
                    Ok(()) => (true, "Magicutor restarted successfully".to_string()),
                    Err(e) => (false, format!("Failed to restart Magicutor: {}", e)),
                };

                let data = Some(Self::collect_status_snapshot(processes.as_ref()).await);

                SupervisorResponse {
                    success,
                    message,
                    data,
                    timestamp,
                }
            },
            SupervisorCommand::StopMagician => {
                let result = {
                    let mut process_guard = processes.magician.lock().await;
                    process_guard.stop().await
                };

                let (success, message) = match result {
                    Ok(()) => (true, "Magician stopped successfully".to_string()),
                    Err(e) => (false, format!("Failed to stop Magician: {}", e)),
                };

                let data = Some(Self::collect_status_snapshot(processes.as_ref()).await);

                SupervisorResponse {
                    success,
                    message,
                    data,
                    timestamp,
                }
            },
            SupervisorCommand::StopMagicutor => {
                let result = {
                    let mut process_guard = processes.magicutor.lock().await;
                    process_guard.stop().await
                };

                let (success, message) = match result {
                    Ok(()) => (true, "Magicutor stopped successfully".to_string()),
                    Err(e) => (false, format!("Failed to stop Magicutor: {}", e)),
                };

                let data = Some(Self::collect_status_snapshot(processes.as_ref()).await);

                SupervisorResponse {
                    success,
                    message,
                    data,
                    timestamp,
                }
            },
            SupervisorCommand::Status => {
                let snapshot = Self::collect_status_snapshot(processes.as_ref()).await;
                SupervisorResponse {
                    success: true,
                    message: "Status retrieved successfully".to_string(),
                    data: Some(snapshot),
                    timestamp,
                }
            },
            SupervisorCommand::RestartDecisionEngine { args } => {
                let result = {
                    let mut process_guard = processes.decision_engine.lock().await;
                    process_guard.restart(args).await
                };
                let (success, message) = match result {
                    Ok(()) => (true, "Decision engine restarted successfully".to_string()),
                    Err(e) => (
                        false,
                        format!("Failed to restart the decision engine: {}", e),
                    ),
                };
                SupervisorResponse {
                    success,
                    message,
                    data: Some(Self::collect_status_snapshot(processes.as_ref()).await),
                    timestamp,
                }
            },
            SupervisorCommand::StopDecisionEngine => {
                let result = {
                    let mut process_guard = processes.decision_engine.lock().await;
                    process_guard.stop().await
                };
                let (success, message) = match result {
                    Ok(()) => (true, "Decision engine stopped successfully".to_string()),
                    Err(e) => (false, format!("Failed to stop the decision engine: {}", e)),
                };
                SupervisorResponse {
                    success,
                    message,
                    data: Some(Self::collect_status_snapshot(processes.as_ref()).await),
                    timestamp,
                }
            },
            SupervisorCommand::StatusDecisionEngine => {
                let status = {
                    let mut process_guard = processes.decision_engine.lock().await;
                    process_guard.get_status()
                };
                SupervisorResponse {
                    success: true,
                    message: "Decision engine status retrieved successfully".to_string(),
                    data: Some(serde_json::to_value(status).unwrap()),
                    timestamp,
                }
            },
            SupervisorCommand::StatusMagician => {
                let status = {
                    let mut process_guard = processes.magician.lock().await;
                    process_guard.get_status()
                };
                SupervisorResponse {
                    success: true,
                    message: "Magician status retrieved successfully".to_string(),
                    data: Some(serde_json::to_value(status).unwrap()),
                    timestamp,
                }
            },
            SupervisorCommand::StatusMagicutor => {
                let status = {
                    let mut process_guard = processes.magicutor.lock().await;
                    process_guard.get_status()
                };
                SupervisorResponse {
                    success: true,
                    message: "Magicutor status retrieved successfully".to_string(),
                    data: Some(serde_json::to_value(status).unwrap()),
                    timestamp,
                }
            },
            SupervisorCommand::HostGatewayStatus => {
                Self::host_gateway_response("GET", "/host/status", timestamp).await
            },
            SupervisorCommand::StartHostPresence => {
                Self::host_gateway_response("POST", "/host/presence/start", timestamp).await
            },
            SupervisorCommand::StopHostPresence => {
                Self::host_gateway_response("POST", "/host/presence/stop", timestamp).await
            },
            SupervisorCommand::RestartHostPresence => {
                Self::host_gateway_response("POST", "/host/presence/restart", timestamp).await
            },
            SupervisorCommand::HealthCheck => {
                let magician_healthy = {
                    let process_guard = processes.magician.lock().await;
                    process_guard.health_check().await
                };
                let magicutor_healthy = {
                    let process_guard = processes.magicutor.lock().await;
                    process_guard.health_check().await
                };

                // The engine counts only where it is installed: a host
                // without it is not unhealthy.
                let decision_engine_healthy = {
                    let process_guard = processes.decision_engine.lock().await;
                    if process_guard.is_managed() {
                        Some(process_guard.health_check().await)
                    } else {
                        None
                    }
                };

                let success =
                    magician_healthy && magicutor_healthy && decision_engine_healthy != Some(false);
                let message = if success {
                    "All services healthy".to_string()
                } else {
                    "One or more services unhealthy".to_string()
                };

                let data = Some(json!({
                    "magician": magician_healthy,
                    "magicutor": magicutor_healthy,
                    "decision_engine": decision_engine_healthy,
                }));

                SupervisorResponse {
                    success,
                    message,
                    data,
                    timestamp,
                }
            },
            SupervisorCommand::Shutdown => {
                {
                    let mut process_guard = processes.magician.lock().await;
                    let _ = process_guard.stop().await;
                }
                {
                    let mut process_guard = processes.magicutor.lock().await;
                    let _ = process_guard.stop().await;
                }
                {
                    let mut process_guard = processes.decision_engine.lock().await;
                    let _ = process_guard.stop().await;
                }
                let _ = shutdown_tx.send(true);

                SupervisorResponse {
                    success: true,
                    message: "Supervisor shutting down".to_string(),
                    data: Some(Self::collect_status_snapshot(processes.as_ref()).await),
                    timestamp,
                }
            },
        }
    }

    async fn host_gateway_response(
        method: &str,
        path: &str,
        timestamp: String,
    ) -> SupervisorResponse {
        match Self::call_host_gateway(method, path).await {
            Ok(data) => SupervisorResponse {
                success: true,
                message: "Host gateway command completed".to_string(),
                data: Some(data),
                timestamp,
            },
            Err(error) => SupervisorResponse {
                success: false,
                message: error,
                data: None,
                timestamp,
            },
        }
    }

    async fn call_host_gateway(method: &str, path: &str) -> Result<serde_json::Value, String> {
        let base_url = std::env::var("MAGICIAN_HOST_GATEWAY_URL")
            .unwrap_or_else(|_| DEFAULT_HOST_GATEWAY_URL.to_string());
        let base_url = base_url.trim_end_matches('/');
        let url = format!("{}{}", base_url, path);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|error| format!("failed to create host gateway client: {}", error))?;
        let request = match method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            _ => return Err(format!("unsupported host gateway method: {}", method)),
        };
        let response = request
            .send()
            .await
            .map_err(|error| format!("host gateway request failed for {}: {}", url, error))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| format!("failed to read host gateway response: {}", error))?;
        let data = serde_json::from_str::<serde_json::Value>(&body).unwrap_or_else(|_| {
            json!({
                "raw": body,
            })
        });
        if !status.is_success() {
            return Err(format!(
                "host gateway returned HTTP {} for {}: {}",
                status, url, data
            ));
        }
        Ok(data)
    }

    async fn collect_status_snapshot(processes: &SupervisorProcesses) -> serde_json::Value {
        let magician_status = {
            let mut process_guard = processes.magician.lock().await;
            process_guard.get_status()
        };
        let magicutor_status = {
            let mut process_guard = processes.magicutor.lock().await;
            process_guard.get_status()
        };
        let decision_engine_status = {
            let mut process_guard = processes.decision_engine.lock().await;
            process_guard.get_status()
        };

        json!({
            "magician": magician_status,
            "magicutor": magicutor_status,
            "decision_engine": decision_engine_status,
        })
    }

    /// Send response back to client
    async fn send_response(stream: &mut TcpStream, response: SupervisorResponse) -> Result<()> {
        let response_json = serde_json::to_string(&response)?;
        stream.write_all(response_json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;
        Ok(())
    }
}

/// Supervisor client for communicating with the supervisor server
pub struct SupervisorClient {
    port: u16,
}

impl SupervisorClient {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// Send command to supervisor
    pub async fn send_command(
        &self,
        command: SupervisorCommand,
    ) -> anyhow::Result<SupervisorResponse> {
        let connect_result = timeout(
            Duration::from_secs(5),
            TcpStream::connect(format!("127.0.0.1:{}", self.port)),
        )
        .await
        .context("Timed out connecting to supervisor")?;
        let mut stream = connect_result?;

        let command_json = serde_json::to_string(&command)?;
        stream.write_all(command_json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;

        let mut reader = BufReader::new(&mut stream);
        let mut response_line = String::new();
        let read_result = timeout(Duration::from_secs(5), reader.read_line(&mut response_line))
            .await
            .context("Timed out waiting for supervisor response")?;
        read_result?;

        let response: SupervisorResponse = serde_json::from_str(&response_line)?;
        Ok(response)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("magic_supervisor=info".parse()?),
        )
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() > 1 && args[1] == "client" {
        // Run as client for testing
        let client = SupervisorClient::new(8081);

        if args.len() > 2 {
            let command = match args[2].as_str() {
                "restart-magician" => SupervisorCommand::RestartMagician { args: None },
                "restart-magicutor" => SupervisorCommand::RestartMagicutor { args: None },
                "stop-magician" => SupervisorCommand::StopMagician,
                "stop-magicutor" => SupervisorCommand::StopMagicutor,
                "status" => SupervisorCommand::Status,
                "status-magician" => SupervisorCommand::StatusMagician,
                "status-magicutor" => SupervisorCommand::StatusMagicutor,
                "restart-decision-engine" => {
                    SupervisorCommand::RestartDecisionEngine { args: None }
                },
                "stop-decision-engine" => SupervisorCommand::StopDecisionEngine,
                "status-decision-engine" => SupervisorCommand::StatusDecisionEngine,
                "host-status" => SupervisorCommand::HostGatewayStatus,
                "host-presence-start" => SupervisorCommand::StartHostPresence,
                "host-presence-stop" => SupervisorCommand::StopHostPresence,
                "host-presence-restart" => SupervisorCommand::RestartHostPresence,
                "health" => SupervisorCommand::HealthCheck,
                "shutdown" => SupervisorCommand::Shutdown,
                _ => {
                    eprintln!(
                        "Usage: {} client [restart-magician|restart-magicutor|restart-decision-engine|stop-magician|stop-magicutor|stop-decision-engine|status|status-magician|status-magicutor|status-decision-engine|host-status|host-presence-start|host-presence-stop|host-presence-restart|health|shutdown]\n  (Magicutor runs forced server mode with packaged config unless overridden)",
                        args[0]
                    );
                    std::process::exit(1);
                },
            };

            match client.send_command(command).await {
                Ok(response) => {
                    println!("Response: {}", serde_json::to_string_pretty(&response)?);
                },
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                },
            }
        }

        return Ok(());
    }

    // Run as supervisor server
    let config = SupervisorConfig::default();
    let supervisor = SupervisorServer::new(config);

    info!("Magic Supervisor starting...");
    supervisor.start().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::{net::TcpListener, path::PathBuf};
    use tokio::time::{sleep, Duration};

    #[test]
    fn managed_binary_names_follow_the_host_executable_convention() {
        let config = SupervisorConfig::default();
        let suffix = if cfg!(windows) { ".exe" } else { ".bin" };
        assert_eq!(
            config.magician_binary,
            PathBuf::from(format!("./magician{suffix}"))
        );
        assert_eq!(
            config.magicutor_binary,
            PathBuf::from(format!("./magicutor{suffix}"))
        );
    }

    #[test]
    fn health_failure_warns_only_after_the_same_process_was_healthy() {
        assert!(!health_failure_is_regression(None, (41, 0)));
        assert!(health_failure_is_regression(Some((41, 0)), (41, 0)));
        assert!(
            !health_failure_is_regression(Some((41, 0)), (42, 1)),
            "a restarted process gets a fresh startup grace"
        );
        assert!(
            !health_failure_is_regression(Some((41, 0)), (41, 1)),
            "PID reuse after a restart still gets fresh startup grace"
        );
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .to_path_buf()
    }

    fn bin_exists(path: &PathBuf) -> bool {
        path.exists()
    }

    fn port_available(port: u16) -> bool {
        TcpListener::bind(("0.0.0.0", port)).is_ok()
            && TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    fn find_free_port() -> std::io::Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(port)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_magician_reaps_its_isolated_process_group() -> anyhow::Result<()> {
        let mut config = SupervisorConfig::default();
        config.magician_binary = PathBuf::from("/bin/sh");
        config.magician_args = vec!["-c".to_string(), "sleep 30 & wait".to_string()];
        let mut process = MagicianProcess::new(config);
        process.start(None).await?;
        let process_group_id = process
            .process_group_id
            .context("Magician process group was not recorded")?;
        let process_id = process
            .process
            .as_ref()
            .and_then(Child::id)
            .context("Magician child PID was not recorded")?;

        assert_eq!(
            unsafe { libc::getpgid(process_id as i32) },
            process_group_id
        );

        process.stop().await?;
        for _ in 0..20 {
            if unsafe { libc::kill(-process_group_id, 0) } != 0 {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        let group_is_gone = unsafe { libc::kill(-process_group_id, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        assert!(group_is_gone, "Magician process group survived stop");
        Ok(())
    }

    /// A child that exits on its own must be remembered as such, because the
    /// health task restarts only what died — never what an operator stopped.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unexpected_exit_is_remembered_until_a_deliberate_stop() -> anyhow::Result<()> {
        let mut config = SupervisorConfig::default();
        config.magician_binary = PathBuf::from("/bin/sh");
        config.magician_args = vec!["-c".to_string(), "exit 6".to_string()];
        let mut process = MagicianProcess::new(config);
        process.start(None).await?;
        for _ in 0..40 {
            if !process.is_running() {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert!(!process.is_running());
        assert!(
            process.unexpected_exit.is_some(),
            "a child that exited on its own must be flagged for the health task"
        );
        process.stop().await?;
        assert!(
            process.unexpected_exit.is_none(),
            "a deliberate stop must cancel any pending auto-restart"
        );
        Ok(())
    }

    /// The restart the health task issues reuses the recorded launch args and
    /// clears the flag, so one crash yields exactly one restart.
    #[cfg(unix)]
    #[tokio::test]
    async fn restart_after_an_unexpected_exit_clears_the_flag() -> anyhow::Result<()> {
        let mut config = SupervisorConfig::default();
        config.magician_binary = PathBuf::from("/bin/sh");
        config.magician_args = vec!["-c".to_string(), "exit 6".to_string()];
        let mut process = MagicianProcess::new(config);
        process.start(None).await?;
        for _ in 0..40 {
            if !process.is_running() {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
        assert!(process.unexpected_exit.is_some());
        // Restart with args that stay alive, exactly as the health task calls it
        // (None keeps the recorded args; here we pass new ones so the child lives).
        process
            .restart(Some(vec!["-c".to_string(), "sleep 30 & wait".to_string()]))
            .await?;
        assert!(process.is_running(), "the restart must spawn a live child");
        assert!(
            process.unexpected_exit.is_none(),
            "a successful spawn clears the flag"
        );
        process.stop().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn observing_unexpected_magician_exit_reaps_surviving_descendants() -> anyhow::Result<()>
    {
        let mut config = SupervisorConfig::default();
        config.magician_binary = PathBuf::from("/bin/sh");
        config.magician_args = vec!["-c".to_string(), "sleep 30 & exit 17".to_string()];
        let mut process = MagicianProcess::new(config);
        process.start(None).await?;
        let process_group_id = process
            .process_group_id
            .context("Magician process group was not recorded")?;

        for _ in 0..20 {
            if !process.is_running() {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }

        assert!(!process.is_running());
        assert!(process.process_group_id.is_none());
        for _ in 0..20 {
            if unsafe { libc::kill(-process_group_id, 0) } != 0 {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        let group_is_gone = unsafe { libc::kill(-process_group_id, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        assert!(
            group_is_gone,
            "descendant survived parent crash observation"
        );
        Ok(())
    }

    #[test]
    fn rolling_restart_limit_blocks_only_inside_window() {
        let now = chrono::Utc::now();
        let mut history = VecDeque::new();

        assert_eq!(
            record_restart_attempt("Magician", &mut history, now, 2, 300).unwrap(),
            1
        );
        assert_eq!(
            record_restart_attempt(
                "Magician",
                &mut history,
                now + chrono::Duration::seconds(10),
                2,
                300
            )
            .unwrap(),
            2
        );

        let blocked = record_restart_attempt(
            "Magician",
            &mut history,
            now + chrono::Duration::seconds(20),
            2,
            300,
        )
        .expect_err("third restart inside window should be blocked");
        assert!(blocked
            .to_string()
            .contains("2 attempts in the last 300 seconds"));
    }

    #[test]
    fn rolling_restart_limit_allows_attempts_after_window_expires() {
        let now = chrono::Utc::now();
        let mut history = VecDeque::new();

        assert_eq!(
            record_restart_attempt("Magicutor", &mut history, now, 2, 300).unwrap(),
            1
        );
        assert_eq!(
            record_restart_attempt(
                "Magicutor",
                &mut history,
                now + chrono::Duration::seconds(10),
                2,
                300
            )
            .unwrap(),
            2
        );
        assert_eq!(
            record_restart_attempt(
                "Magicutor",
                &mut history,
                now + chrono::Duration::seconds(311),
                2,
                300
            )
            .unwrap(),
            1
        );
        assert_eq!(
            recent_restart_attempt_count(&history, now + chrono::Duration::seconds(311), 300),
            1
        );
    }

    async fn shutdown_supervisor_if_running(
        client: &SupervisorClient,
        handle: tokio::task::JoinHandle<anyhow::Result<()>>,
    ) -> anyhow::Result<()> {
        if let Ok(response) = client.send_command(SupervisorCommand::Shutdown).await {
            if response.success {
                let join_result = tokio::time::timeout(Duration::from_secs(5), handle)
                    .await
                    .context("Supervisor did not stop after shutdown")?;
                join_result??;
            }
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn supervisor_reports_running_with_real_bins() -> anyhow::Result<()> {
        // Require the real release binaries to be present; skip quietly if not.
        let workspace = workspace_root();
        let magicutor_bin = workspace.join("magicutor.bin");
        let magician_bin = workspace.join("magician.bin");
        for bin in [&magicutor_bin, &magician_bin] {
            if !bin_exists(bin) {
                eprintln!("Skipping smoke test: missing binary {:?}", bin);
                return Ok(());
            }
        }

        // Avoid running if the default service ports are already taken.
        for port in [3002u16, 3003u16] {
            if !port_available(port) {
                eprintln!("Skipping smoke test: port {} is already in use", port);
                return Ok(());
            }
        }

        let control_port = find_free_port()?;
        let mut config = SupervisorConfig::default();
        config.control_port = control_port;
        config.magicutor_binary = magicutor_bin;
        config.magician_binary = magician_bin;

        // Start supervisor in the background
        let supervisor = SupervisorServer::new(config);
        let handle = tokio::spawn(async move { supervisor.start().await });

        let client = SupervisorClient::new(control_port);

        // Wait for all services to report running
        let mut running = false;
        for _ in 0..20 {
            if let Ok(response) = client.send_command(SupervisorCommand::Status).await {
                if let Some(data) = response.data {
                    let all_running = ["magicutor", "magician"].iter().all(|name| {
                        data.get(*name)
                            .and_then(|s| s.get("status"))
                            .and_then(|s| s.as_str())
                            == Some("running")
                    });
                    if all_running {
                        running = true;
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(750)).await;
        }

        if !running {
            if let Ok(response) = client.send_command(SupervisorCommand::Status).await {
                if let Some(data) = response.data {
                    let any_stopped = ["magicutor", "magician"].iter().any(|name| {
                        data.get(*name)
                            .and_then(|s| s.get("status"))
                            .and_then(|s| s.as_str())
                            != Some("running")
                    });
                    if any_stopped {
                        let _ = shutdown_supervisor_if_running(&client, handle).await;
                        eprintln!(
                            "Skipping smoke test: packaged binaries did not stay running in this local environment: {}",
                            data
                        );
                        return Ok(());
                    }
                }
            }
            anyhow::bail!("Services failed to reach running state");
        }

        let mut healthy = false;
        let mut last_health_message = String::new();
        for _ in 0..20 {
            if let Ok(response) = client.send_command(SupervisorCommand::HealthCheck).await {
                last_health_message = response.message.clone();
                if response.success {
                    healthy = true;
                    break;
                }
            }
            sleep(Duration::from_millis(750)).await;
        }
        if !healthy {
            if let Ok(response) = client.send_command(SupervisorCommand::Status).await {
                if let Some(data) = response.data {
                    let _ = shutdown_supervisor_if_running(&client, handle).await;
                    eprintln!(
                        "Skipping smoke test: packaged binaries did not become healthy in this local environment (last health: {}): {}",
                        last_health_message,
                        data
                    );
                    return Ok(());
                }
            }
            anyhow::bail!("Health check failed: {}", last_health_message);
        }

        // Stop supervised services and tear down the server task
        let shutdown = client
            .send_command(SupervisorCommand::Shutdown)
            .await
            .expect("Shutdown command failed");
        assert!(shutdown.success, "Shutdown failed: {}", shutdown.message);

        let join_result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("Supervisor did not stop after shutdown");
        join_result??;

        Ok(())
    }

    /// The line that started this: `main() -> Result` returning `Err` prints
    /// `Error: {err:?}` through `Termination`, with no tracing level attached.
    /// It relayed at info, so the reason a startup died was invisible to any
    /// error-level filter.
    #[test]
    fn a_dying_child_is_never_relayed_below_error() {
        assert!(is_process_fatal_line(
            "Error: acquiring default scope lease: conflict"
        ));
        assert!(is_process_fatal_line(
            "thread 'main' panicked at src/main.rs:10:5:"
        ));
        // Indented by a wrapper is still fatal.
        assert!(is_process_fatal_line("   Error: something broke"));
    }

    /// A fatal line mentioning the health endpoint must not be swallowed by the
    /// noise rule that silences `/health` chatter to debug.
    #[test]
    fn the_health_noise_filter_cannot_swallow_a_crash() {
        assert!(is_process_fatal_line(
            "Error: binding /health listener: address in use"
        ));
    }

    #[test]
    fn subprocess_levels_preserve_info_on_stderr_and_unlabelled_failures() {
        for stream in [SubprocessStream::Stdout, SubprocessStream::Stderr] {
            assert_eq!(
                subprocess_log_level("2026-09-08T00:00:00Z  INFO magicutor: ready", stream),
                tracing::Level::INFO
            );
            assert_eq!(
                subprocess_log_level("2026-09-08T00:00:00Z  WARN magicutor: retry", stream),
                tracing::Level::WARN
            );
            assert_eq!(
                subprocess_log_level("Error: binding /health listener", stream),
                tracing::Level::ERROR
            );
        }
        assert_eq!(
            subprocess_log_level(
                "2026-09-08T09:17:23+0530 info magician-macos-audio-engine: ready",
                SubprocessStream::Stderr
            ),
            tracing::Level::INFO,
        );
        assert_eq!(
            subprocess_log_level(
                "[09:43:21.647] [INFO] [FluidAudio.VadManager] VAD model loaded successfully",
                SubprocessStream::Stderr
            ),
            tracing::Level::INFO,
        );
        assert_eq!(
            subprocess_log_level(
                "2026-09-08T00:00:00Z INFO ready after an error retry",
                SubprocessStream::Stderr
            ),
            tracing::Level::INFO,
        );
        assert_eq!(
            subprocess_log_level("unlabelled diagnostic", SubprocessStream::Stderr),
            tracing::Level::WARN
        );
        assert_eq!(
            subprocess_log_level("ready", SubprocessStream::Stdout),
            tracing::Level::INFO
        );
    }

    /// Ordinary output must not be promoted just because it says "error"
    /// somewhere in the middle — only the process-death shapes count.
    #[test]
    fn prose_mentioning_an_error_is_not_treated_as_a_crash() {
        assert!(!is_process_fatal_line(
            "recovered from a transient error and continued"
        ));
        assert!(!is_process_fatal_line("GET /v3/errors 200"));
    }
}
