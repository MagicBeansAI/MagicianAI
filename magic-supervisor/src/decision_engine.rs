//! The decision-engine process (structured-decision plan Part IV, E5).
//!
//! Magician asks the engine over a Unix socket when `decision.transport:
//! service`; this keeps that process running. It is optional: with no
//! `decision-engine` binary installed the supervisor logs that once and
//! manages nothing, and Magician's step judges fall through to the LLM as
//! they do whenever the engine is unreachable.
//!
//! The socket comes from the supervisor config when set, else from the
//! engine itself (`decision-engine --print-socket`), which resolves it as
//! Magician resolves its default. The supervisor passes it back explicitly
//! on start and health-checks that same path, so the three can never
//! disagree about where the engine listens.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tracing::{error, info, warn};

use crate::{
    log_subprocess_line, recent_restart_attempt_count, record_restart_attempt, ProcessStatus,
    SubprocessStream, SupervisorConfig,
};

/// Where the engine listens: the configured override, else what the engine
/// binary reports (`--print-socket`). The engine is the one component that
/// resolves the runtime root for its socket — the supervisor never does
/// (the typed-storage boundary keeps that resolution in reviewed places).
pub async fn resolve_socket(config: &SupervisorConfig) -> Option<PathBuf> {
    if let Some(socket) = &config.decision_engine_socket {
        return Some(socket.clone());
    }
    let output = timeout(
        Duration::from_secs(5),
        Command::new(&config.decision_engine_binary)
            .arg("--print-socket")
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!printed.is_empty()).then(|| PathBuf::from(printed))
}

/// `GET /health` over the engine's Unix socket: true on a 200.
pub async fn socket_health(socket: &Path) -> bool {
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(socket).await?;
        stream
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: decision-engine\r\nConnection: close\r\n\r\n",
            )
            .await?;
        let mut head = [0u8; 16];
        let read = stream.read(&mut head).await?;
        Ok::<bool, std::io::Error>(head[..read].starts_with(b"HTTP/1.1 200"))
    };
    matches!(
        timeout(Duration::from_secs(2), exchange).await,
        Ok(Ok(true))
    )
}

pub struct DecisionEngineProcess {
    config: SupervisorConfig,
    pub(crate) process: Option<Child>,
    start_time: Option<Instant>,
    pub(crate) restart_count: u32,
    restart_attempts: VecDeque<chrono::DateTime<chrono::Utc>>,
    last_restart: Option<chrono::DateTime<chrono::Utc>>,
    current_args: Vec<String>,
    /// The socket in use, resolved on start.
    socket: Option<PathBuf>,
    /// Set when the child died on its own; cleared by a deliberate stop so
    /// the health task never fights an operator.
    pub(crate) unexpected_exit: Option<String>,
}

impl DecisionEngineProcess {
    pub fn new(config: SupervisorConfig) -> Self {
        let current_args = config.decision_engine_args.clone();
        Self {
            config,
            process: None,
            start_time: None,
            restart_count: 0,
            restart_attempts: VecDeque::new(),
            last_restart: None,
            current_args,
            socket: None,
            unexpected_exit: None,
        }
    }

    /// Whether the supervisor manages the engine at all: enabled, and the
    /// binary is installed.
    pub fn is_managed(&self) -> bool {
        self.config.decision_engine_enabled && self.config.decision_engine_binary.exists()
    }

    pub async fn start(&mut self, args: Option<Vec<String>>) -> Result<()> {
        if !self.config.decision_engine_enabled {
            info!("Decision engine disabled in supervisor config; not starting it");
            return Ok(());
        }
        if !self.config.decision_engine_binary.exists() {
            info!(
                "Decision engine binary {} not installed; not managing it (step judges fall through to the LLM in service mode)",
                self.config.decision_engine_binary.display()
            );
            return Ok(());
        }
        if self.is_running() {
            info!("Decision engine is already running, stopping first");
            self.stop().await?;
        }
        if let Some(new_args) = args {
            self.current_args = new_args;
        }
        let Some(socket) = resolve_socket(&self.config).await else {
            anyhow::bail!(
                "decision engine at {} did not report its socket (--print-socket)",
                self.config.decision_engine_binary.display()
            );
        };
        self.socket = Some(socket.clone());
        let mut full_args = self.current_args.clone();
        if !full_args
            .iter()
            .any(|arg| arg == "--socket" || arg.starts_with("--socket="))
        {
            full_args.extend(["--socket".to_string(), socket.display().to_string()]);
        }
        info!("Starting decision engine with args: {:?}", full_args);

        let mut cmd = Command::new(&self.config.decision_engine_binary);
        cmd.args(&full_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(parent) = self.config.decision_engine_binary.parent() {
            cmd.current_dir(parent);
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        for (stream, kind) in [
            (
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                SubprocessStream::Stdout,
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                SubprocessStream::Stderr,
            ),
        ] {
            if let Some(stream) = stream {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let stripped = strip_ansi_escapes::strip(&line);
                        let clean = String::from_utf8_lossy(&stripped);
                        log_subprocess_line("[DecisionEngine]", &clean, kind);
                    }
                });
            }
        }
        self.process = Some(child);
        self.start_time = Some(Instant::now());
        self.unexpected_exit = None;
        info!("Decision engine started with PID: {}", pid);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            let pid = process.id().unwrap_or(0);
            info!("Stopping decision engine process (PID: {})", pid);
            if let Err(e) = process.kill().await {
                warn!("Failed to kill decision engine process {}: {}", pid, e);
            }
            match timeout(Duration::from_secs(10), process.wait()).await {
                Ok(Ok(status)) => info!("Decision engine exited with status: {}", status),
                Ok(Err(e)) => error!("Error waiting for decision engine to exit: {}", e),
                Err(_) => warn!("Decision engine did not exit within timeout"),
            }
        }
        self.start_time = None;
        self.unexpected_exit = None;
        Ok(())
    }

    pub async fn restart(&mut self, args: Option<Vec<String>>) -> Result<()> {
        info!("Restarting decision engine...");
        if let Some(last_restart) = self.last_restart {
            let elapsed = chrono::Utc::now()
                .signed_duration_since(last_restart)
                .num_seconds();
            if elapsed >= 0 && elapsed < self.config.decision_engine_restart_cooldown as i64 {
                let remaining = self.config.decision_engine_restart_cooldown - elapsed as u64;
                anyhow::bail!(
                    "Decision engine restart cooldown active, {remaining} seconds remaining"
                );
            }
        }
        let now = chrono::Utc::now();
        let attempts_in_window = record_restart_attempt(
            "decision engine",
            &mut self.restart_attempts,
            now,
            self.config.decision_engine_max_restart_attempts,
            self.config.decision_engine_restart_attempt_window,
        )?;
        self.stop().await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.start(args).await?;
        self.restart_count += 1;
        self.last_restart = Some(now);
        info!(
            "Decision engine restarted (total {}, {} attempt(s) in the last {}s)",
            self.restart_count,
            attempts_in_window,
            self.config.decision_engine_restart_attempt_window.max(1)
        );
        Ok(())
    }

    pub fn is_running(&mut self) -> bool {
        let Some(process) = &mut self.process else {
            return false;
        };
        match process.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                error!("Decision engine exited unexpectedly with status {}", status);
                self.unexpected_exit = Some(status.to_string());
                self.process = None;
                self.start_time = None;
                false
            },
            Err(error) => {
                error!("Failed to inspect decision engine child process: {}", error);
                self.unexpected_exit = Some(format!("uninspectable: {error}"));
                self.process = None;
                self.start_time = None;
                false
            },
        }
    }

    pub async fn health_check(&self) -> bool {
        match &self.socket {
            Some(socket) => socket_health(socket).await,
            None => false,
        }
    }

    pub fn get_status(&mut self) -> ProcessStatus {
        let managed = self.is_managed();
        let running = self.is_running();
        let status = match (managed, running) {
            (false, _) => "not_installed",
            (true, true) => "running",
            (true, false) => "stopped",
        };
        ProcessStatus {
            service: "decision-engine".to_string(),
            pid: if running {
                self.process.as_ref().and_then(|p| p.id())
            } else {
                None
            },
            status: status.to_string(),
            uptime_seconds: if running {
                self.start_time.map(|start| start.elapsed().as_secs())
            } else {
                None
            },
            restart_count: self.restart_count,
            restart_attempts_in_window: recent_restart_attempt_count(
                &self.restart_attempts,
                chrono::Utc::now(),
                self.config.decision_engine_restart_attempt_window,
            ),
            restart_attempt_window_seconds: self
                .config
                .decision_engine_restart_attempt_window
                .max(1),
            last_restart: self.last_restart.map(|t| t.to_rfc3339()),
            health_status: if running { "running" } else { status }.to_string(),
            args: self.current_args.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(binary: PathBuf, socket: PathBuf) -> SupervisorConfig {
        SupervisorConfig {
            decision_engine_binary: binary,
            decision_engine_socket: Some(socket),
            ..SupervisorConfig::default()
        }
    }

    #[tokio::test]
    async fn a_missing_binary_is_not_managed_and_starts_nothing() -> Result<()> {
        let mut process = DecisionEngineProcess::new(config_with(
            PathBuf::from("/nonexistent/decision-engine.bin"),
            PathBuf::from("/tmp/de-none.sock"),
        ));
        assert!(!process.is_managed());
        process.start(None).await?;
        assert!(!process.is_running());
        assert_eq!(process.get_status().status, "not_installed");
        Ok(())
    }

    #[tokio::test]
    async fn health_reads_a_200_over_the_socket_and_nothing_else() -> Result<()> {
        let socket = std::env::temp_dir().join(format!("sup-de-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        assert!(!socket_health(&socket).await, "no listener is unhealthy");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let serve = tokio::spawn(async move {
            for reply in [
                "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n",
                "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n",
            ] {
                let (mut stream, _) = listener.accept().await?;
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request).await?;
                stream.write_all(reply.as_bytes()).await?;
            }
            Ok::<(), std::io::Error>(())
        });
        assert!(socket_health(&socket).await, "a 200 is healthy");
        assert!(!socket_health(&socket).await, "a 503 is not");
        serve.await??;
        let _ = std::fs::remove_file(&socket);
        Ok(())
    }

    #[tokio::test]
    async fn an_unexpected_exit_is_remembered_until_a_deliberate_stop() -> Result<()> {
        // A stand-in binary that exits at once.
        let mut process = DecisionEngineProcess::new(config_with(
            PathBuf::from("/usr/bin/true"),
            PathBuf::from("/tmp/de-true.sock"),
        ));
        process.start(None).await?;
        for _ in 0..50 {
            if !process.is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            process.unexpected_exit.is_some(),
            "an exit on its own is recorded"
        );
        process.stop().await?;
        assert!(
            process.unexpected_exit.is_none(),
            "a deliberate stop clears it"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_socket_is_the_override_or_what_the_engine_reports() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("sup-de-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        // A stand-in engine that answers --print-socket.
        let binary = dir.join("decision-engine.bin");
        std::fs::write(
            &binary,
            "#!/bin/sh\n[ \"$1\" = --print-socket ] && echo /tmp/from-engine.sock\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
        }
        let mut config = SupervisorConfig {
            decision_engine_binary: binary,
            ..SupervisorConfig::default()
        };
        assert_eq!(
            resolve_socket(&config).await,
            Some(PathBuf::from("/tmp/from-engine.sock"))
        );
        config.decision_engine_socket = Some(PathBuf::from("/tmp/override.sock"));
        assert_eq!(
            resolve_socket(&config).await,
            Some(PathBuf::from("/tmp/override.sock"))
        );
        config.decision_engine_socket = None;
        config.decision_engine_binary = PathBuf::from("/usr/bin/false");
        assert_eq!(
            resolve_socket(&config).await,
            None,
            "a failing engine reports nothing"
        );
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }
}
