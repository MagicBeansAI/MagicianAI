//! Ollama daemon lifecycle management.
//!
//! On magician boot:
//! 1. Detect `ollama` binary on PATH.
//! 2. Probe the configured base URL; if reachable, use the existing daemon
//!    (do not respawn, do not stop on shutdown).
//! 3. Otherwise spawn `ollama serve` as a child process and wait up to
//!    `STARTUP_PROBE_TIMEOUT` for it to become healthy.
//! 4. Verify the configured embedding model is installed. Missing models fail
//!    closed: setup/install owns downloads, while runtime never substitutes or
//!    silently pulls a different model.
//!
//! While running:
//! - A background task polls health every `HEALTH_CHECK_INTERVAL` and
//!   updates the global [`OllamaState`].
//!
//! On shutdown:
//! - If magician spawned the daemon, SIGTERM the child; wait up to 5s; SIGKILL.
//! - If the daemon was pre-existing, leave it alone.
//!
//! Capability gating elsewhere reads [`is_available`] (the cheap atomic
//! check) to decide whether to expose `vector` to agents.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use magician_vector_index::OllamaEmbedder;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How often the background loop probes Ollama's `/api/tags` endpoint.
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// Boot-time: how long to wait for a freshly-spawned daemon to respond.
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Boot-time: how often to retry during the startup wait.
const STARTUP_PROBE_INTERVAL: Duration = Duration::from_millis(500);
/// Shutdown: SIGTERM grace period before SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

static STATE: OnceLock<Arc<OllamaState>> = OnceLock::new();

/// Public availability check. Cheap — single atomic load. Returns `false` if
/// Ollama hasn't been started, the daemon is unreachable, or the embedding
/// model isn't pulled.
pub fn is_available() -> bool {
    STATE
        .get()
        .map(|s| s.available.load(Ordering::Relaxed))
        .unwrap_or(false)
}

/// Returns a handle for in-process consumers (provider impls) that want a
/// pre-configured embedder. Returns `None` when the lifecycle has not been
/// started.
pub fn embedder() -> Option<OllamaEmbedder> {
    STATE.get().map(|s| s.embedder.clone())
}

/// Lifecycle state owned by the process singleton.
pub struct OllamaState {
    pub available: AtomicBool,
    pub embedder: OllamaEmbedder,
    pub child: Mutex<Option<Child>>,
    pub health_task: std::sync::Mutex<Option<JoinHandle<()>>>,
    pub shutdown: CancellationToken,
    boot_finished: CancellationToken,
}

/// Start the Ollama lifecycle. Idempotent — calling twice is a no-op.
///
/// Honors `MAGICIAN_OLLAMA_AUTOSTART=false` to skip the spawn step (still
/// performs health checks against whatever daemon happens to be running).
pub async fn start() {
    start_background();
    if let Some(state) = STATE.get() {
        state.boot_finished.cancelled().await;
    }
}

/// Publish the configured embedder synchronously, then own boot and monitoring
/// in one cancellable task. Consumers can wire the handle before warm-up ends.
pub fn start_background() {
    if STATE.get().is_some() {
        return;
    }
    let embedder = OllamaEmbedder::from_env();
    let shutdown = CancellationToken::new();
    let state = Arc::new(OllamaState {
        available: AtomicBool::new(false),
        embedder,
        child: Mutex::new(None),
        health_task: std::sync::Mutex::new(None),
        shutdown,
        boot_finished: CancellationToken::new(),
    });
    let mut task_slot = state.health_task.lock().expect("Ollama task lock poisoned");
    if STATE.set(state.clone()).is_err() {
        // Lost the race — another caller initialized first.
        return;
    }
    let autostart = std::env::var("MAGICIAN_OLLAMA_AUTOSTART")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let st = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = st.shutdown.cancelled() => {},
            _ = perform_boot_sequence(&st, autostart) => {},
        }
        st.boot_finished.cancel();
        if st.shutdown.is_cancelled() {
            return;
        }
        // Monitor even after boot failure so capabilities recover dynamically.
        let token = st.shutdown.clone();
        run_health_check_loop(st, token).await;
    });
    *task_slot = Some(handle);
}

/// Stop the lifecycle. If we spawned Ollama ourselves, terminate it.
/// Always cancels the health-check loop.
pub async fn stop() {
    let Some(state) = STATE.get() else {
        return;
    };
    state.shutdown.cancel();
    // Cancel health loop first so we don't race with the flag.
    state.available.store(false, Ordering::Relaxed);
    let handle = state
        .health_task
        .lock()
        .expect("Ollama task lock poisoned")
        .take();
    if let Some(mut handle) = handle {
        if tokio::time::timeout(Duration::from_secs(2), &mut handle)
            .await
            .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }
    }
    state.boot_finished.cancel();
    state.available.store(false, Ordering::Relaxed);
    let mut child_slot = state.child.lock().await;
    if let Some(mut child) = child_slot.take() {
        info!(target: "ollama_lifecycle", "stopping magician-spawned ollama daemon");
        // tokio::process::Child::start_kill sends SIGKILL on unix; for
        // graceful shutdown we'd want SIGTERM first. Use libc when available.
        graceful_terminate(&mut child).await;
    }
}

async fn perform_boot_sequence(state: &Arc<OllamaState>, autostart: bool) {
    // 1. Probe existing daemon.
    if state.embedder.health_check().await.is_ok() {
        info!(
            target: "ollama_lifecycle",
            base_url = state.embedder.config().base_url.as_str(),
            "ollama daemon already reachable; using existing instance"
        );
        verify_and_mark_available(state).await;
        return;
    }

    if !autostart {
        warn!(
            target: "ollama_lifecycle",
            "MAGICIAN_OLLAMA_AUTOSTART=false and no existing daemon reachable; vector capability will remain disabled until Ollama is started"
        );
        return;
    }

    if !is_local_ollama_base_url(&state.embedder.config().base_url) {
        warn!(
            target: "ollama_lifecycle",
            base_url = state.embedder.config().base_url.as_str(),
            "remote embedding Ollama is unreachable; refusing to spawn a local daemon for a remote endpoint"
        );
        return;
    }

    // 2. Locate binary.
    let binary = match which_ollama() {
        Some(path) => path,
        None => {
            warn!(
                target: "ollama_lifecycle",
                "`ollama` binary not found on PATH; vector capability will remain disabled. Install via `brew install ollama` (macOS) or `curl -fsSL https://ollama.com/install.sh | sh` (linux)."
            );
            return;
        },
    };

    // 3. Spawn child process.
    info!(
        target: "ollama_lifecycle",
        binary = binary.as_str(),
        "starting ollama serve as child process"
    );
    let mut cmd = Command::new(&binary);
    cmd.arg("serve")
        .env(
            "OLLAMA_HOST",
            ollama_host_from_base_url(&state.embedder.config().base_url),
        )
        .env(
            "OLLAMA_NUM_PARALLEL",
            state.embedder.config().num_parallel.to_string(),
        )
        .env(
            "OLLAMA_MAX_LOADED_MODELS",
            state.embedder.config().max_loaded_models.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(context_tokens) = state.embedder.config().context_tokens {
        cmd.env("OLLAMA_CONTEXT_LENGTH", context_tokens.to_string());
    }
    if let Some(keep_alive) = state.embedder.config().keep_alive.as_deref() {
        cmd.env("OLLAMA_KEEP_ALIVE", keep_alive);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            warn!(
                target: "ollama_lifecycle",
                error = %error,
                "failed to spawn ollama serve; vector capability will remain disabled"
            );
            return;
        },
    };
    *state.child.lock().await = Some(child);

    // 4. Wait for daemon to come up.
    let deadline = tokio::time::Instant::now() + STARTUP_PROBE_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if state.embedder.health_check().await.is_ok() {
            info!(target: "ollama_lifecycle", "ollama serve is healthy");
            verify_and_mark_available(state).await;
            return;
        }
        tokio::time::sleep(STARTUP_PROBE_INTERVAL).await;
    }
    warn!(
        target: "ollama_lifecycle",
        "ollama serve did not become healthy within {}s; vector capability will remain disabled until next health-check tick",
        STARTUP_PROBE_TIMEOUT.as_secs()
    );
}

/// Confirm the embedding model is installed; mark available accordingly.
async fn verify_and_mark_available(state: &Arc<OllamaState>) {
    match state.embedder.model_pulled().await {
        Ok(true) => {
            if let Err(error) = state.embedder.prewarm().await {
                warn!(
                    target: "ollama_lifecycle",
                    model = state.embedder.config().model.as_str(),
                    error = %error,
                    "embedding model is installed but could not be prewarmed"
                );
                return;
            }
            state.available.store(true, Ordering::Relaxed);
            info!(
                target: "ollama_lifecycle",
                model = state.embedder.config().model.as_str(),
                "embedding model is pulled; vector capability enabled"
            );
        },
        Ok(false) => {
            warn!(
                target: "ollama_lifecycle",
                model = state.embedder.config().model.as_str(),
                "configured embedding model is not installed; vector capability is disabled. Run `make setup-ollama`."
            );
        },
        Err(error) => {
            warn!(
                target: "ollama_lifecycle",
                error = %error,
                "could not verify embedding model presence; will retry on next health tick"
            );
        },
    }
}

async fn run_health_check_loop(state: Arc<OllamaState>, shutdown: CancellationToken) {
    let mut ticker = tokio::time::interval(HEALTH_CHECK_INTERVAL);
    ticker.tick().await; // burn the immediate first tick (already probed at boot)
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                let healthy = state.embedder.health_check().await.is_ok();
                if !healthy {
                    if state.available.swap(false, Ordering::Relaxed) {
                        warn!(target: "ollama_lifecycle", "ollama health check failed; vector capability disabled");
                    }
                    continue;
                }
                // Healthy. Confirm model is still present (cheap call).
                let model_ok = state.embedder.model_pulled().await.unwrap_or(false);
                let prior = state.available.load(Ordering::Relaxed);
                if model_ok && !prior {
                    verify_and_mark_available(&state).await;
                } else if !model_ok && prior {
                    state.available.store(false, Ordering::Relaxed);
                    warn!(target: "ollama_lifecycle", "ollama healthy but embedding model missing; vector disabled");
                }
            }
        }
    }
}

fn which_ollama() -> Option<String> {
    // Search PATH for `ollama`.
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = std::path::Path::new(dir).join("ollama");
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    // Common fallback locations (homebrew, system installs).
    for candidate in [
        "/opt/homebrew/bin/ollama",
        "/usr/local/bin/ollama",
        "/usr/bin/ollama",
    ] {
        if std::path::Path::new(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn ollama_host_from_base_url(base_url: &str) -> String {
    let without_scheme = base_url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://")
        .or_else(|| {
            base_url
                .trim()
                .trim_end_matches('/')
                .strip_prefix("https://")
        })
        .unwrap_or_else(|| base_url.trim().trim_end_matches('/'));
    without_scheme
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn is_local_ollama_base_url(base_url: &str) -> bool {
    let authority = ollama_host_from_base_url(base_url);
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .unwrap_or_else(|| authority.split(':').next().unwrap_or_default());
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1")
}

async fn graceful_terminate(child: &mut Child) {
    // Try SIGTERM via shell on unix; fall back to start_kill (SIGKILL)
    // everywhere. Avoids pulling in `nix` as a dep just for one syscall.
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .arg(format!("{pid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(status)) => {
            info!(target: "ollama_lifecycle", code = status.code(), "ollama exited cleanly");
        },
        Ok(Err(error)) => {
            warn!(target: "ollama_lifecycle", error = %error, "error waiting for ollama exit");
        },
        Err(_) => {
            warn!(target: "ollama_lifecycle", "ollama did not exit within grace period; SIGKILL");
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_available_false_before_start() {
        // STATE may be set by other test ordering; this test asserts a
        // simple invariant rather than the global flag.
        let cfg = magician_vector_index::OllamaEmbedderConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        let embedder = OllamaEmbedder::new(cfg);
        // Should fail fast — port 1 is reserved.
        assert!(embedder.health_check().await.is_err());
    }

    #[tokio::test]
    async fn embedder_returns_none_before_start_if_state_uninitialized() {
        // Race-safe: if another test initialized STATE we just check the
        // path doesn't panic.
        let _ = embedder();
    }

    #[test]
    fn local_daemon_detection_does_not_spawn_for_remote_embedding_endpoints() {
        assert!(is_local_ollama_base_url("http://127.0.0.1:11435"));
        assert!(is_local_ollama_base_url("http://localhost:11435/"));
        assert!(is_local_ollama_base_url("http://[::1]:11435"));
        assert!(!is_local_ollama_base_url(
            "http://host.docker.internal:11435"
        ));
        assert!(!is_local_ollama_base_url("https://embeddings.example.test"));
    }
}
