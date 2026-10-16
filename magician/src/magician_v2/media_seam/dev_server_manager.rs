use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::media_seam::{parse_ready_line, DevCommand};

const IDLE_EVICT: Duration = Duration::from_secs(30 * 60);
const ENDED_RETAIN: Duration = Duration::from_secs(300);
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const LOG_TAIL_MAX_LINES: usize = 400;
const LOG_TAIL_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DevServerStatus {
    Starting,
    Ready,
    Crashed,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
pub struct DevServerStatusView {
    pub project_id: String,
    pub status: DevServerStatus,
    pub local_url: Option<String>,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub command: String,
    pub working_dir: String,
    pub started_at_ms: i64,
    pub last_output_at_ms: i64,
    pub recent_log_tail: String,
}

struct RingBuffer {
    lines: VecDeque<String>,
    bytes: usize,
    max_lines: usize,
    max_bytes: usize,
}

impl RingBuffer {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            max_lines,
            max_bytes,
        }
    }

    fn push(&mut self, line: &str) {
        let line = line.to_string();
        self.bytes = self.bytes.saturating_add(line.len()).saturating_add(1);
        self.lines.push_back(line);
        while self.lines.len() > self.max_lines || self.bytes > self.max_bytes {
            let Some(removed) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.len().saturating_add(1));
        }
    }

    fn render(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

struct DevServerSession {
    project_id: String,
    principal: String,
    workspace: String,
    command_display: String,
    working_dir: PathBuf,
    child: Arc<Mutex<Child>>,
    cancel: CancellationToken,
    status: Arc<Mutex<DevServerStatus>>,
    local_url: Arc<Mutex<Option<String>>>,
    port: Arc<Mutex<Option<u16>>>,
    log_tail: Arc<Mutex<RingBuffer>>,
    last_output_at_ms: Arc<AtomicI64>,
    ready: Arc<Notify>,
    started_at_ms: i64,
    pid: Option<u32>,
}

struct Managed {
    session: Arc<DevServerSession>,
    ended_at: Option<Instant>,
    done: Arc<Notify>,
}

#[derive(Default)]
pub struct DevServerSessionManager {
    sessions: Mutex<HashMap<String, Managed>>,
    /// Per-project secret env (`project_id` → `{ env_var → value }`) injected into
    /// the dev-server process at (re)start. Populated by the M6 Citizen API
    /// (`magician_secret`): Magician resolves a scoped secret and binds it here so
    /// the app runs with it, while Pi never sees the value. Applied on next start.
    secret_env: Mutex<HashMap<String, HashMap<String, String>>>,
}

static MANAGER: Lazy<Arc<DevServerSessionManager>> =
    Lazy::new(|| Arc::new(DevServerSessionManager::new()));

pub fn dev_server_manager() -> Arc<DevServerSessionManager> {
    MANAGER.clone()
}

impl DevServerSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            secret_env: Mutex::new(HashMap::new()),
        }
    }

    /// Bind a secret value to an env var for a project's dev server (M6 Citizen
    /// API). Magician resolves the value from the scoped broker and stores it here
    /// so the dev server runs with it; Pi never sees it. Applied on next (re)start.
    pub async fn set_secret_env(&self, project_id: &str, env_var: String, value: String) {
        self.secret_env
            .lock()
            .await
            .entry(project_id.to_string())
            .or_default()
            .insert(env_var, value);
    }

    /// The env-var names currently bound for a project (values never exposed).
    pub async fn secret_env_vars(&self, project_id: &str) -> Vec<String> {
        self.secret_env
            .lock()
            .await
            .get(project_id)
            .map(|map| {
                let mut vars: Vec<String> = map.keys().cloned().collect();
                vars.sort();
                vars
            })
            .unwrap_or_default()
    }

    pub async fn start(
        self: &Arc<Self>,
        project_id: String,
        principal: String,
        workspace: String,
        working_dir: PathBuf,
        command: DevCommand,
        env: HashMap<String, String>,
    ) -> Result<DevServerStatusView, String> {
        self.reap().await;
        {
            let map = self.sessions.lock().await;
            if let Some(managed) = map.get(&project_id) {
                if managed.ended_at.is_none() {
                    return Ok(managed.session.status_view().await);
                }
            }
        }

        // Merge any M6 Citizen-bound secret env for this project (resolved by the
        // Citizen API; Pi never sees the values) on top of the caller's env.
        let mut env = env;
        {
            let secrets = self.secret_env.lock().await;
            if let Some(bound) = secrets.get(&project_id) {
                for (key, value) in bound {
                    env.insert(key.clone(), value.clone());
                }
            }
        }
        let session = DevServerSession::spawn(
            project_id.clone(),
            principal,
            workspace,
            working_dir,
            command,
            env,
        )
        .await?;
        let done = Arc::new(Notify::new());
        {
            let mut map = self.sessions.lock().await;
            map.insert(
                project_id.clone(),
                Managed {
                    session: session.clone(),
                    ended_at: None,
                    done: done.clone(),
                },
            );
        }

        let mgr = Arc::clone(self);
        let id = project_id;
        let supervisor_session = session.clone();
        let supervisor_done = done;
        tokio::spawn(async move {
            supervisor_session.wait_until_exit().await;
            {
                let mut map = mgr.sessions.lock().await;
                if let Some(managed) = map.get_mut(&id) {
                    managed.ended_at = Some(Instant::now());
                }
            }
            supervisor_done.notify_one();
        });

        let _ = tokio::time::timeout(READY_TIMEOUT, session.ready.notified()).await;
        Ok(session.status_view().await)
    }

    pub async fn status(&self, project_id: &str) -> Option<DevServerStatusView> {
        self.reap().await;
        let session = {
            let map = self.sessions.lock().await;
            map.get(project_id)?.session.clone()
        };
        Some(session.status_view().await)
    }

    pub async fn port_for(&self, project_id: &str) -> Option<u16> {
        let session = {
            let map = self.sessions.lock().await;
            let managed = map.get(project_id)?;
            if managed.ended_at.is_some() {
                return None;
            }
            managed.session.clone()
        };
        let port = *session.port.lock().await;
        port
    }

    pub async fn list(&self) -> Vec<DevServerStatusView> {
        self.reap().await;
        let sessions = {
            let map = self.sessions.lock().await;
            map.values()
                .map(|managed| managed.session.clone())
                .collect::<Vec<_>>()
        };
        let mut rows = Vec::with_capacity(sessions.len());
        for session in sessions {
            rows.push(session.status_view().await);
        }
        rows
    }

    pub async fn stop(&self, project_id: &str) -> Result<(), String> {
        let (session, done, ended) = {
            let map = self.sessions.lock().await;
            match map.get(project_id) {
                Some(managed) => (
                    managed.session.clone(),
                    managed.done.clone(),
                    managed.ended_at.is_some(),
                ),
                None => return Err(format!("unknown dev server: {project_id}")),
            }
        };
        session.cancel.cancel();
        session.kill().await;
        if !ended {
            let _ = tokio::time::timeout(Duration::from_secs(5), done.notified()).await;
        }
        Ok(())
    }

    async fn reap(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut idle_sessions = Vec::new();
        {
            let mut map = self.sessions.lock().await;
            map.retain(|_, managed| {
                let idle = now - managed.session.last_output_at_ms.load(Ordering::Acquire)
                    > IDLE_EVICT.as_millis() as i64;
                let expired =
                    matches!(managed.ended_at, Some(ended_at) if ended_at.elapsed() > ENDED_RETAIN);
                if idle && managed.ended_at.is_none() {
                    idle_sessions.push(managed.session.clone());
                }
                !expired
            });
        }
        for session in idle_sessions {
            session.cancel.cancel();
            session.kill().await;
        }
    }
}

impl DevServerSession {
    async fn spawn(
        project_id: String,
        principal: String,
        workspace: String,
        working_dir: PathBuf,
        cmd: DevCommand,
        env: HashMap<String, String>,
    ) -> Result<Arc<Self>, String> {
        if !working_dir.is_dir() {
            return Err(format!(
                "dev server working directory does not exist: {}",
                working_dir.display()
            ));
        }
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
            .env("FORCE_COLOR", "0")
            .env("NO_COLOR", "1")
            .env("BROWSER", "none")
            .env("CI", "true");
        for (key, value) in &env {
            command.env(key, value);
        }
        // Run the dev server as its OWN process-group leader so `stop()` can take down the
        // WHOLE tree. `npm run dev` / `vite` fork worker processes (node, esbuild); signalling
        // only the launcher child orphans those workers — they keep running and holding the
        // port. With `process_group(0)` the child's pgid == its pid, so a kill on `-pid`
        // reaches every descendant.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "spawn `{}` in {}: {error}",
                cmd.display,
                working_dir.display()
            )
        })?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "dev server child stdout was not piped".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "dev server child stderr was not piped".to_string())?;
        let now = chrono::Utc::now().timestamp_millis();
        let session = Arc::new(Self {
            project_id,
            principal,
            workspace,
            command_display: cmd.display,
            working_dir,
            child: Arc::new(Mutex::new(child)),
            cancel: CancellationToken::new(),
            status: Arc::new(Mutex::new(DevServerStatus::Starting)),
            local_url: Arc::new(Mutex::new(None)),
            port: Arc::new(Mutex::new(None)),
            log_tail: Arc::new(Mutex::new(RingBuffer::new(
                LOG_TAIL_MAX_LINES,
                LOG_TAIL_MAX_BYTES,
            ))),
            last_output_at_ms: Arc::new(AtomicI64::new(now)),
            ready: Arc::new(Notify::new()),
            started_at_ms: now,
            pid,
        });
        session.clone().spawn_reader(stdout);
        session.clone().spawn_reader(stderr);
        Ok(session)
    }

    fn spawn_reader(self: Arc<Self>, stream: impl tokio::io::AsyncRead + Unpin + Send + 'static) {
        use tokio::io::AsyncBufReadExt;
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                self.last_output_at_ms
                    .store(chrono::Utc::now().timestamp_millis(), Ordering::Release);
                self.log_tail.lock().await.push(&line);
                if matches!(*self.status.lock().await, DevServerStatus::Starting) {
                    if let Some((url, port)) = parse_ready_line(&line) {
                        *self.local_url.lock().await = Some(url);
                        *self.port.lock().await = Some(port);
                        *self.status.lock().await = DevServerStatus::Ready;
                        self.ready.notify_waiters();
                    }
                }
            }
        });
    }

    async fn wait_until_exit(&self) {
        let exit_status = loop {
            let try_wait = {
                let mut child = self.child.lock().await;
                if self.cancel.is_cancelled() {
                    let _ = child.start_kill();
                }
                child.try_wait()
            };
            match try_wait {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    tokio::select! {
                        _ = self.cancel.cancelled() => {},
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                    }
                },
                Err(error) => break Err(error),
            }
        };
        let mut status = self.status.lock().await;
        *status = match exit_status {
            Ok(exit) if exit.success() => DevServerStatus::Stopped,
            _ if self.cancel.is_cancelled() => DevServerStatus::Stopped,
            _ => DevServerStatus::Crashed,
        };
        self.ready.notify_waiters();
    }

    async fn kill(&self) {
        // The child we hold is only the launcher (`npm`/`pnpm`/`vite`); it forks the real
        // worker processes (node, esbuild). Killing just the launcher orphans those workers,
        // which keep running and holding the port — the "stop didn't kill the process" bug.
        // We spawned the launcher as its own process-group leader (`process_group(0)`), so
        // signal the WHOLE group (`-pid`): SIGTERM for a graceful shutdown, then SIGKILL as a
        // backstop. `start_kill()` on the child then reaps the launcher.
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            let group = -(pid as i32);
            // SAFETY: `kill(2)` with a negative pid signals a process group; it touches no
            // Rust memory. A stale pid at worst signals nothing (ESRCH) — never another group,
            // since the launcher was a group leader whose pgid is reserved until reaped here.
            unsafe {
                libc::kill(group, libc::SIGTERM);
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            unsafe {
                libc::kill(group, libc::SIGKILL);
            }
        }
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
    }

    async fn status_view(&self) -> DevServerStatusView {
        let _scope = (&self.principal, &self.workspace);
        DevServerStatusView {
            project_id: self.project_id.clone(),
            status: *self.status.lock().await,
            local_url: self.local_url.lock().await.clone(),
            port: *self.port.lock().await,
            pid: self.pid,
            command: self.command_display.clone(),
            working_dir: self.working_dir.display().to_string(),
            started_at_ms: self.started_at_ms,
            last_output_at_ms: self.last_output_at_ms.load(Ordering::Acquire),
            recent_log_tail: self.log_tail.lock().await.render(),
        }
    }
}
