//! Phase A2/A3: background Agy `--version` tick plus a bounded isolation
//! probe.
//!
//! After version + auth would otherwise be Ready, the worker launches the
//! frozen qualify argv (`--input-format stream-json`, no `-p`, no user
//! JSONL) in a disposable cwd, drains `event: init`, then kills the
//! process group **before** dropping stdin. Writing a user line would
//! start a billed model turn. Missing tool / permission_mode lists stay
//! Unqualified. Request handlers never call this. The probe conversation
//! id is not journaled as a VibeDev continuation.
//!
//! Isolation probes back off 5 minutes per identity.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::{io::BufReader, process::Command, time::timeout};
use uuid::Uuid;

use super::{
    agy::{
        apply_agy_child_process_group, filter_agy_child_env, force_kill_process_group,
        terminate_process_group,
    },
    agy_contract::qualify_launch_args,
    agy_qualification::{
        agy_needs_isolation_attestation, cache_agy_receipt, qualify_from_agy_evidence,
        AgyQualifyEvidence,
    },
    discovery::{
        observe_agy_readiness, probe_agy_version_in_background, resolved_agy_executable,
        AgyReadinessSnapshot, AgySearchPaths,
    },
    jsonl::{read_bounded_jsonl_value, BoundedJsonlError},
};
use crate::{
    config::MagicianAgySettings, magician_v2::execution::coding_engine::coding_budget_settings,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_LINE_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_JSON_DEPTH: usize = 32;
const WORKER_INTERVAL: Duration = Duration::from_secs(30);
const ISOLATION_BACKOFF: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy)]
pub struct AgyQualifyLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
}

impl Default for AgyQualifyLimits {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgyQualifyError {
    Timeout,
    Protocol,
    Io,
}

impl std::fmt::Display for AgyQualifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Agy qualification session timed out"),
            Self::Protocol => write!(f, "Agy qualification protocol was malformed"),
            Self::Io => write!(f, "Agy qualification session I/O failed"),
        }
    }
}

impl std::error::Error for AgyQualifyError {}

/// Production tick: version + auth, then a bounded init probe if that would
/// be Ready. HTTP never calls this.
pub async fn tick_agy_version_worker(
    settings: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    let use_api_key = settings.use_api_key;
    tick_agy_qualify_worker(
        settings,
        search,
        move |identity, version, binary| async move {
            agy_qualify_child_stdio(
                identity,
                version,
                use_api_key,
                &binary,
                AgyQualifyLimits::default(),
            )
            .await
        },
    )
    .await
}

pub async fn tick_agy_qualify_worker<F, Fut>(
    settings: &MagicianAgySettings,
    search: &AgySearchPaths,
    run_session: F,
) -> AgyReadinessSnapshot
where
    F: FnOnce(String, Option<String>, PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<AgyQualifyEvidence, AgyQualifyError>>,
{
    if !settings.enabled {
        return observe_agy_readiness(settings, search);
    }
    let probe_settings = settings.clone();
    let probe_search = search.clone();
    let _ = tokio::task::spawn_blocking(move || {
        probe_agy_version_in_background(&probe_settings, &probe_search);
    })
    .await;
    let snapshot = observe_agy_readiness(settings, search);
    if agy_needs_isolation_attestation(&snapshot) && isolation_backoff_elapsed(snapshot.identity())
    {
        if let Some(binary) = resolved_agy_executable(settings, search) {
            record_isolation_attempt(snapshot.identity());
            match run_session(
                snapshot.identity().to_string(),
                snapshot.version.clone(),
                binary,
            )
            .await
            {
                Ok(evidence) => {
                    let receipt = qualify_from_agy_evidence(evidence);
                    // Without this the only trace of a failed qualification is an
                    // engine silently missing from VibeDev. Reasons are public-safe;
                    // each distinct outcome is logged once.
                    let failed =
                        !crate::magician_v2::execution::coding_engine::discovery::agy_is_selectable(
                            receipt.readiness,
                        );
                    let outcome =
                        failed.then(|| format!("{:?}: {}", receipt.readiness, receipt.reason));
                    if failed
                        && super::qualify_worker::qualification_outcome_changed("agy_cli", outcome)
                    {
                        tracing::warn!(engine = "agy_cli", readiness = ?receipt.readiness, reason = %receipt.reason, "coding engine did not qualify");
                    } else if !failed {
                        super::qualify_worker::qualification_outcome_changed("agy_cli", None);
                    }
                    cache_agy_receipt(receipt)
                },
                Err(error) => {
                    if super::qualify_worker::qualification_outcome_changed(
                        "agy_cli",
                        Some(format!("probe failed: {error:?}")),
                    ) {
                        tracing::warn!(engine = "agy_cli", error = ?error, "coding engine qualification probe failed");
                    }
                },
            }
        }
    }
    observe_agy_readiness(settings, search)
}

pub fn spawn_agy_version_worker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        let mut interval = tokio::time::interval(WORKER_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let settings = coding_budget_settings().agy;
            let search = AgySearchPaths::production();
            let _ = tick_agy_version_worker(&settings, &search).await;
        }
    });
}

/// Drain until `event: init`. Never journals the probe conversation id.
/// Never writes a user JSONL line.
pub async fn agy_qualify_over_stdio<R>(
    identity: impl Into<String>,
    version: Option<String>,
    reader: R,
    limits: AgyQualifyLimits,
) -> Result<AgyQualifyEvidence, AgyQualifyError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let deadline = tokio::time::Instant::now() + limits.request_timeout;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(AgyQualifyError::Timeout);
        }
        let remaining = deadline.saturating_duration_since(now);
        let incoming = timeout(
            remaining,
            read_bounded_jsonl_value(&mut reader, limits.max_line_bytes, limits.max_json_depth),
        )
        .await
        .map_err(|_| AgyQualifyError::Timeout)?;
        let value = match incoming {
            Ok(value) => value,
            Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => {
                return Err(AgyQualifyError::Protocol)
            },
            Err(
                BoundedJsonlError::Oversized
                | BoundedJsonlError::Malformed
                | BoundedJsonlError::TooDeep,
            ) => return Err(AgyQualifyError::Protocol),
        };
        if value.get("event").and_then(Value::as_str) == Some("init") {
            return Ok(AgyQualifyEvidence {
                identity: identity.into(),
                version,
                init: value,
                cancelled: false,
                canary_mcp_name: None,
            });
        }
    }
}

pub async fn agy_qualify_child_stdio(
    identity: String,
    version: Option<String>,
    use_api_key: bool,
    binary: &Path,
    limits: AgyQualifyLimits,
) -> Result<AgyQualifyEvidence, AgyQualifyError> {
    let (cwd, canary) = qualify_cwd()?;
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = filter_agy_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        use_api_key,
    );
    let args = qualify_launch_args();
    debug_assert!(
        args.iter().any(|arg| arg == "--input-format")
            && args.iter().any(|arg| arg == "stream-json")
            && !args.iter().any(|arg| arg == "-p")
            && !args.iter().any(|arg| arg == "--continue" || arg == "-c")
    );
    let mut command = Command::new(binary);
    command
        .args(&args)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    apply_agy_child_process_group(&mut command);
    let mut child = command.spawn().map_err(|_| AgyQualifyError::Io)?;
    let stdout = child.stdout.take().ok_or(AgyQualifyError::Io)?;
    let stdin = child.stdin.take().ok_or(AgyQualifyError::Io)?;
    let mut owned = OwnedAgyQualifyChild {
        child,
        stdin: Some(stdin),
        reaped: false,
    };
    // Do not write a user JSONL event. Agy print/stream-json with a user
    // message starts a billed model turn; init arrives without one.
    let mut result = agy_qualify_over_stdio(identity, version, stdout, limits).await;
    owned.reap().await;
    if let Ok(evidence) = &mut result {
        evidence.canary_mcp_name = Some(canary);
        evidence.cancelled = true;
    }
    let _ = std::fs::remove_dir_all(&cwd);
    result
}

/// Kill the process group before stdin is closed. Agy treats stdin EOF as
/// end-of-prompt; `kill_on_drop` does not reach grandchildren.
struct OwnedAgyQualifyChild {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    reaped: bool,
}

impl OwnedAgyQualifyChild {
    async fn reap(&mut self) {
        terminate_process_group(&mut self.child).await;
        self.reaped = true;
        drop(self.stdin.take());
    }
}

impl Drop for OwnedAgyQualifyChild {
    fn drop(&mut self) {
        if !self.reaped {
            force_kill_process_group(&mut self.child);
            self.reaped = true;
        }
        drop(self.stdin.take());
    }
}

fn qualify_cwd() -> Result<(PathBuf, String), AgyQualifyError> {
    let dir = std::env::temp_dir().join(format!("magician-agy-qualify-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|_| AgyQualifyError::Io)?;
    let canary = plant_qualify_canary(&dir)?;
    Ok((dir, canary))
}

fn plant_qualify_canary(cwd: &Path) -> Result<String, AgyQualifyError> {
    let name = format!("magician-agy-qualify-canary-{}", Uuid::new_v4());
    let mut servers = serde_json::Map::new();
    servers.insert(
        name.clone(),
        serde_json::json!({ "command": "false", "args": [] }),
    );
    let body = serde_json::json!({ "mcpServers": servers });
    let encoded = serde_json::to_vec_pretty(&body).map_err(|_| AgyQualifyError::Protocol)?;
    std::fs::write(cwd.join(".mcp.json"), encoded).map_err(|_| AgyQualifyError::Io)?;
    Ok(name)
}

fn isolation_backoff_elapsed(identity: &str) -> bool {
    match isolation_backoff_slot().get(identity) {
        None => true,
        Some(at) => at.elapsed() >= ISOLATION_BACKOFF,
    }
}

fn record_isolation_attempt(identity: &str) {
    isolation_backoff_slot().insert(identity.to_string(), Instant::now());
}

fn isolation_backoff_slot() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    static SLOT: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};
    use tokio::io::{duplex, AsyncWriteExt};

    use super::*;
    use crate::magician_v2::execution::coding_engine::agy::filter_agy_child_env;

    async fn write_lines(stream: tokio::io::DuplexStream, lines: Vec<Value>) {
        let (_reader, mut writer) = tokio::io::split(stream);
        for line in lines {
            let mut payload = serde_json::to_vec(&line).unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                return;
            }
        }
    }

    #[tokio::test]
    async fn qualify_over_stdio_stops_at_init_and_ignores_later_result() {
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(write_lines(
            server,
            vec![
                json!({
                    "event": "init",
                    "conversation_id": "sess-probe",
                    "init": {
                        "tools": ["run_command", "search_web"],
                        "permission_mode": "always-proceed"
                    }
                }),
                json!({
                    "event": "result",
                    "result": { "status": "SUCCESS", "conversation_id": "sess-probe" }
                }),
            ],
        ));
        let evidence = agy_qualify_over_stdio(
            "bin-1",
            Some("1.1.19".into()),
            client,
            AgyQualifyLimits::default(),
        )
        .await
        .expect("init");
        let _ = peer.await;
        assert_eq!(evidence.identity, "bin-1");
        assert_eq!(
            evidence.init.get("event").and_then(Value::as_str),
            Some("init")
        );
        assert!(!evidence.cancelled);
        let rendered = format!("{evidence:?}");
        assert!(rendered.contains("sess-probe"));
    }

    #[test]
    fn qualify_child_env_drops_magician_foo() {
        let filtered = filter_agy_child_env(
            [
                ("PATH", "/usr/bin"),
                ("HOME", "/Users/me"),
                ("MAGICIAN_FOO", "secret"),
                ("GEMINI_API_KEY", "gk-secret"),
            ],
            false,
        );
        assert!(filtered.contains_key("PATH"));
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("GEMINI_API_KEY"));
    }

    #[test]
    fn qualify_launch_argv_is_stream_json_input_without_print_or_user_line() {
        let args = qualify_launch_args();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--input-format", "stream-json"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(!args.iter().any(|arg| arg == "-p"));
        assert!(!args.iter().any(|arg| arg == "--continue" || arg == "-c"));
        let source = include_str!("agy_qualify_worker.rs");
        let production = source.split("\n#[cfg").next().expect("production");
        assert!(production.contains("qualify_launch_args"));
        assert!(production.contains("filter_agy_child_env"));
        assert!(
            production.contains("apply_agy_child_process_group"),
            "qualify spawn must join a process group so grandchildren cannot leak"
        );
        assert!(
            production.contains("terminate_process_group"),
            "qualify teardown must kill the process group, not only the agy parent"
        );
        assert!(
            production.contains("OwnedAgyQualifyChild"),
            "qualify must own stdin+child so Drop cannot EOF before kill"
        );
        assert!(
            production.contains("force_kill_process_group"),
            "qualify Drop must kill the process group, not only kill_on_drop the parent"
        );
        assert!(
            production.contains("impl Drop for OwnedAgyQualifyChild"),
            "cancelled qualify must kill the group before stdin is closed"
        );
        assert!(!production.contains("child.kill()"));
        assert!(production.contains("plant_qualify_canary"));
        assert!(production.contains(".mcp.json"));
        assert!(
            !production.contains("write_all"),
            "qualify must not write a user JSONL event"
        );
        assert!(!production.contains("store_coding_ledger"));
        assert!(!production.contains("load_coding_ledger"));
        assert!(!production.contains("for_agy_session"));
        assert!(!production.contains("run_turn_over_stdio"));
        assert!(!production.contains(".arg(\"--continue\")"));
    }

    #[test]
    fn qualify_canary_lives_in_disposable_cwd_not_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = plant_qualify_canary(dir.path()).expect("plant");
        let planted = dir.path().join(".mcp.json");
        assert!(planted.is_file());
        let body = std::fs::read_to_string(&planted).expect("read");
        assert!(body.contains(&name));
        assert!(name.starts_with("magician-agy-qualify-canary-"));
        if let Some(home) = std::env::var_os("HOME") {
            assert!(
                !planted.starts_with(Path::new(&home)),
                "canary must not be planted in HOME: {}",
                planted.display()
            );
        }
    }

    #[test]
    fn probe_session_id_is_not_written_to_the_coding_ledger() {
        let qualify = include_str!("agy_qualify_worker.rs");
        let production = qualify.split("\n#[cfg").next().expect("production");
        assert!(!production.contains("native_session_id"));
        assert!(!production.contains("coding_ledger"));
        let qualification = include_str!("agy_qualification.rs");
        let qual_prod = qualification.split("\n#[cfg").next().expect("production");
        assert!(!qual_prod.contains("native_session_id"));
        assert!(!qual_prod.contains("store_coding_ledger"));
        assert!(!qual_prod.contains("for_agy_session"));
        let ledger = include_str!("ledger.rs");
        assert!(!ledger.contains("agy_qualify_over_stdio"));
        assert!(!ledger.contains("tick_agy_qualify_worker"));
    }

    #[test]
    fn http_handlers_do_not_call_the_agy_probe() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(!web.contains("agy_qualify_over_stdio("));
        assert!(!web.contains("agy_qualify_child_stdio("));
        assert!(!web.contains("tick_agy_qualify_worker("));
        assert!(!web.contains("tick_agy_version_worker("));
        assert!(!web.contains("probe_agy_cli_version("));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(bin.contains("spawn_agy_version_worker()"));
        assert!(!bin.contains("agy_qualify_over_stdio("));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_injected_clean_init_becomes_ready() {
        clean_init_becomes_ready_with_token_at(".gemini/antigravity-cli/antigravity-oauth-token")
            .await;
    }

    /// Agy 1.2.x signs in to `~/.gemini/jetski-standalone-oauth-token` and
    /// never writes the 1.1.19 file. That alone must count as signed in.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_agy_1_2_token_location_counts_as_signed_in() {
        clean_init_becomes_ready_with_token_at(".gemini/jetski-standalone-oauth-token").await;
    }

    #[cfg(unix)]
    async fn clean_init_becomes_ready_with_token_at(token_relative: &str) {
        use std::ffi::OsString;

        use crate::config::MagicianAgySettings;
        use crate::magician_v2::execution::coding_engine::discovery::AgySearchPaths;
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("agy");
        std::fs::write(&binary, b"#!/bin/sh\necho 1.1.19\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let home = dir.path().join("home");
        let token = home.join(token_relative);
        std::fs::create_dir_all(token.parent().expect("token dir")).expect("home");
        std::fs::write(&token, b"not-a-secret-token\n").expect("oauth");
        let settings = MagicianAgySettings {
            enabled: true,
            binary: None,
            use_api_key: false,
        };
        let search = AgySearchPaths {
            path: Some(OsString::from("/no-such-agy-path")),
            reviewed: vec![binary],
            home: Some(home),
            env: Some(Vec::new()),
        };
        let captured = Arc::new(Mutex::new(None));
        let capture = captured.clone();
        let snapshot = tick_agy_qualify_worker(&settings, &search, move |identity, version, _| {
            let capture = capture.clone();
            async move {
                *capture.lock().unwrap() = Some(identity.clone());
                Ok(AgyQualifyEvidence {
                    identity,
                    version,
                    init: json!({
                        "event": "init",
                        "init": {
                            "tools": ["run_command", "view_file", "search_web"],
                            "permission_mode": "always-proceed"
                        }
                    }),
                    cancelled: true,
                    canary_mcp_name: None,
                })
            }
        })
        .await;
        assert_eq!(
            snapshot.readiness,
            crate::magician_v2::execution::coding_engine::discovery::AgyReadiness::Ready
        );
        assert!(snapshot.selectable);
        assert!(captured.lock().unwrap().is_some());
        let public = snapshot.public_json().to_string();
        assert!(!public.contains("oauth-token"), "{public}");
        assert!(!public.contains("/Users"), "{public}");
    }
}
