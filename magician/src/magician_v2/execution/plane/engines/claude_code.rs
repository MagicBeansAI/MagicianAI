//! Claude Code as a plane harness. Argv matches the Task 1 verified invocation.
//!
//! `spawn: true` (the default) launches one process per session with a process
//! group, writes turns to stdin as stream-json, and settles on the terminal
//! `result` event. Unit tests that must not require a `claude` binary set
//! `spawn: false`, or point `binary` at a fake that speaks the same protocol.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::execution::coding_engine::claude::{
    apply_claude_child_process_group, force_kill_process_group, terminate_process_group,
};
use crate::magician_v2::execution::plane::engine::{
    mcp_config_document, revoke_session_grant, HarnessCapabilities, HarnessEngine, HarnessError,
    HarnessSession, HarnessSessionRequest, HarnessStopReason, HarnessStreamSink, HarnessTurnInput,
    HarnessTurnSettled,
};
use crate::magician_v2::execution::plane::grant::{plane_grant_registry, PlaneTurnStopReason};

const MAX_HARNESS_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ClaudeCodeEngine {
    /// When false, `start` records a spawn without launching a process so
    /// argv/revoke tests do not require the Claude CLI.
    pub spawn: bool,
    /// Binary invoked as argv[0]. Production is `claude`; tests inject a fake.
    pub binary: PathBuf,
}

impl Default for ClaudeCodeEngine {
    fn default() -> Self {
        Self {
            spawn: true,
            binary: PathBuf::from("claude"),
        }
    }
}

impl ClaudeCodeEngine {
    pub fn argv(&self, req: &HarnessSessionRequest) -> Vec<String> {
        self.argv_with_config_path(req, &mcp_config_path_for())
    }

    fn argv_with_config_path(
        &self,
        req: &HarnessSessionRequest,
        mcp_config_path: &Path,
    ) -> Vec<String> {
        let mut argv = vec![
            self.binary.display().to_string(),
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
            "--tools".to_string(),
            String::new(),
            "--mcp-config".to_string(),
            mcp_config_path.display().to_string(),
            "--strict-mcp-config".to_string(),
            "--setting-sources".to_string(),
            String::new(),
            "--append-system-prompt".to_string(),
            req.system_prompt.clone(),
        ];
        if let Some(model) = &req.model {
            argv.push("--model".to_string());
            argv.push(model.clone());
        }
        if let Some(session_id) = &req.resume_session_id {
            argv.push("--resume".to_string());
            argv.push(session_id.clone());
        }
        argv
    }
}

fn mcp_config_path_for() -> PathBuf {
    // Unpredictable + create-new below: no shared deterministic pathname for
    // another local process to pre-place as a symlink. The grant is inside the
    // mode-0600 file, never in this path or argv.
    std::env::temp_dir().join(format!(
        "magician-plane-{}.mcp.json",
        uuid::Uuid::new_v4().simple()
    ))
}

fn write_private_mcp_config(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub struct ClaudeCodeSession {
    grant: String,
    process_spawn_count: u32,
    cancel: Option<CancellationToken>,
    /// Separate from cancellation: approval ends this harness turn so the
    /// Magician loop can pause rather than waiting for Claude's result event.
    turn_stop: Option<CancellationToken>,
    revoked: bool,
    /// True only when a real child was launched. Distinguishes `spawn: false`
    /// tests from a live process that already died.
    actually_spawned: bool,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    mcp_config_path: PathBuf,
    native_session_id: Option<String>,
    /// Set on `NeedsApproval`. Drop must not revoke: Task 8 will start a
    /// new `--resume` process against the same live grant.
    retain_grant: bool,
    /// From `execution.harness_turn_max_seconds`. Zero = no Magician ceiling.
    turn_timeout: Duration,
}

impl ClaudeCodeSession {
    pub fn process_spawn_count(&self) -> u32 {
        self.process_spawn_count
    }
}

#[async_trait]
impl HarnessEngine for ClaudeCodeEngine {
    fn name(&self) -> &'static str {
        "claude_code"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            supports_resume: true,
            tools_list_changed: true,
            streams_text_deltas: true,
            native_tool_posture:
                crate::magician_v2::execution::plane::engine::NativeToolPosture::Stripped,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        Ok(Box::new(self.start_session(req).await?))
    }
}

impl ClaudeCodeEngine {
    pub async fn start_session(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<ClaudeCodeSession, HarnessError> {
        let turn_stop = plane_grant_registry()
            .resolve_run_scoped(&req.grant)
            .await
            .map(|grant| grant.turn_stop_signal());
        let path = mcp_config_path_for();
        let doc = mcp_config_document(&req.endpoint, &req.grant);
        let bytes = match serde_json::to_vec_pretty(&doc) {
            Ok(bytes) => bytes,
            Err(err) => {
                revoke_session_grant(&req.grant).await;
                return Err(HarnessError::Message(err.to_string()));
            },
        };
        if let Err(err) = write_private_mcp_config(&path, &bytes) {
            revoke_session_grant(&req.grant).await;
            return Err(HarnessError::Message(format!(
                "write private MCP config: {err}"
            )));
        }
        let argv = self.argv_with_config_path(req, &path);

        if !self.spawn {
            return Ok(ClaudeCodeSession {
                grant: req.grant.clone(),
                process_spawn_count: 1,
                cancel: req.cancel.clone(),
                turn_stop,
                revoked: false,
                actually_spawned: false,
                child: None,
                stdin: None,
                stdout: None,
                mcp_config_path: path,
                native_session_id: None,
                retain_grant: false,
                turn_timeout: req.turn_timeout,
            });
        }

        // The allowlist below re-sets PATH on the child; a bare binary name
        // combined with that would force std onto `fork` instead of
        // `posix_spawn`, so resolve it against the same PATH first (see
        // `runtime_core::process`).
        let mut command = tokio::process::Command::new(runtime_core::process::resolve_program_str(
            &argv[0], None,
        ));
        command.args(&argv[1..]);
        command.current_dir(&req.cwd);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        apply_env_allowlist(&mut command, &req.env_allowlist);
        apply_claude_child_process_group(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                revoke_session_grant(&req.grant).await;
                return Err(HarnessError::Message(format!(
                    "spawn {} failed: {err}",
                    argv[0]
                )));
            },
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(drain_stderr(stderr));
        }
        if stdin.is_none() || stdout.is_none() {
            terminate_process_group(&mut child).await;
            let _ = std::fs::remove_file(&path);
            revoke_session_grant(&req.grant).await;
            return Err(HarnessError::Message(
                "spawned harness did not expose the required stdin/stdout pipes".to_string(),
            ));
        }
        Ok(ClaudeCodeSession {
            grant: req.grant.clone(),
            process_spawn_count: 1,
            cancel: req.cancel.clone(),
            turn_stop,
            revoked: false,
            actually_spawned: true,
            child: Some(child),
            stdin,
            stdout,
            mcp_config_path: path,
            native_session_id: None,
            retain_grant: false,
            turn_timeout: req.turn_timeout,
        })
    }
}

fn apply_env_allowlist(command: &mut tokio::process::Command, allowlist: &[String]) {
    // Empty means no extra variables, not "inherit the service environment".
    // Otherwise a default harness receives every credential held by Magician.
    command.env_clear();
    for key in ["PATH", "HOME", "USER", "TMPDIR", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    for key in allowlist {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
}

async fn drain_stderr(stderr: tokio::process::ChildStderr) {
    let mut stderr = stderr;
    let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
}

/// Read one newline-delimited protocol event without allowing a malicious or
/// broken child to make `read_line` allocate until OOM.
async fn read_bounded_event<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    output: &mut Vec<u8>,
) -> io::Result<usize> {
    output.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output.len());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(end) > MAX_HARNESS_EVENT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("harness event exceeds {MAX_HARNESS_EVENT_BYTES} byte limit"),
            ));
        }
        output.extend_from_slice(&available[..end]);
        let ended = available[end - 1] == b'\n';
        reader.consume(end);
        if ended {
            return Ok(output.len());
        }
    }
}

fn stream_json_user_message(input: &HarnessTurnInput) -> Value {
    let mut text = input.text.clone();
    if !input.operator_steer.is_empty() {
        text.push_str("\n\nOperator steer:\n");
        for line in &input.operator_steer {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
    }
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

/// One reply text delta. With `--include-partial-messages` the CLI streams
/// the Messages-API wire events (`stream_event` → `content_block_delta` →
/// `text_delta`) as the model writes; the whole `assistant` message that
/// follows repeats that text and is not a delta. Thinking deltas are not
/// reply text.
fn parse_assistant_text_delta(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("stream_event") {
        return None;
    }
    let event = value.get("event")?;
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    delta
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn parse_result_event(value: &Value) -> Option<HarnessTurnSettled> {
    if value.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let assistant_text = value
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let native_session_id = value
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let usage = crate::magician_v2::execution::plane::usage::usage_from_event(value);
    Some(HarnessTurnSettled {
        assistant_text,
        stop_reason: if is_error {
            HarnessStopReason::Refused
        } else {
            HarnessStopReason::Settled
        },
        usage,
        native_session_id,
    })
}

async fn wait_for_signal(signal: Option<CancellationToken>) {
    match signal {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

async fn wait_until_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

impl ClaudeCodeSession {
    async fn kill_process_group(&mut self) {
        if let Some(child) = self.child.as_mut() {
            terminate_process_group(child).await;
        }
        self.stdin = None;
        self.stdout = None;
        self.child = None;
    }

    async fn stop_child_keep_grant(&mut self) {
        self.kill_process_group().await;
        let _ = std::fs::remove_file(&self.mcp_config_path);
        self.retain_grant = true;
    }

    async fn release(&mut self) {
        self.kill_process_group().await;
        let _ = std::fs::remove_file(&self.mcp_config_path);
        if !self.revoked {
            revoke_session_grant(&self.grant).await;
            self.revoked = true;
        }
    }

    fn force_kill_now(&mut self) {
        if let Some(child) = self.child.as_mut() {
            force_kill_process_group(child);
        }
        self.stdin = None;
        self.stdout = None;
        self.child = None;
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    fn turn_stopped(&self) -> bool {
        self.turn_stop
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    /// Settle a fired turn-stop signal by its reason. `NeedsApproval` pauses:
    /// the child stops but the grant is retained so a later turn can `--resume`
    /// this native session. `TurnBudgetSpent` — and any reason that cannot be
    /// read back — ends the turn and releases, so control returns to the loop
    /// with the grant revoked rather than pausing on a dead authority.
    async fn settle_turn_stop(&mut self) -> HarnessTurnSettled {
        let reason = plane_grant_registry()
            .resolve_run_scoped(&self.grant)
            .await
            .and_then(|grant| grant.turn_stop_reason());
        let stop_reason = match reason {
            Some(PlaneTurnStopReason::NeedsApproval) => {
                // A pause can be arbitrarily long. Stop the child now; a later
                // Task 8 caller can `--resume` from `native_session_id`. Do
                // **not** revoke the grant — that would 401 every later
                // `tools/call` and delete Magician HITL for the rest of the run.
                self.stop_child_keep_grant().await;
                HarnessStopReason::NeedsApproval
            },
            Some(PlaneTurnStopReason::Delegate) => {
                // The park can be arbitrarily long, like an approval; the
                // resumed turn continues this native conversation. The
                // grant is what carries the captured targets to the loop.
                self.stop_child_keep_grant().await;
                HarnessStopReason::Delegate
            },
            Some(PlaneTurnStopReason::TurnBudgetSpent) | None => {
                self.release().await;
                HarnessStopReason::TurnBudgetSpent
            },
        };
        HarnessTurnSettled {
            assistant_text: String::new(),
            stop_reason,
            usage: None,
            native_session_id: self.native_session_id.clone(),
        }
    }
}

#[async_trait]
impl HarnessSession for ClaudeCodeSession {
    async fn turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError> {
        let result = self.drive_turn(input, sink).await;
        if result.is_err() && !self.retain_grant {
            self.release().await;
        }
        result
    }

    async fn shutdown(&mut self) {
        self.release().await;
    }
}

impl ClaudeCodeSession {
    async fn drive_turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError> {
        if self.cancelled() {
            self.release().await;
            return Ok(HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Cancelled,
                usage: None,
                native_session_id: self.native_session_id.clone(),
            });
        }
        if self.turn_stopped() {
            return Ok(self.settle_turn_stop().await);
        }
        if self.child.is_none() {
            if self.actually_spawned {
                return Err(HarnessError::Message(
                    "harness process is gone; start a new session".into(),
                ));
            }
            return Ok(HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Settled,
                usage: None,
                native_session_id: self.native_session_id.clone(),
            });
        }

        let payload = stream_json_user_message(input);
        let mut line =
            serde_json::to_vec(&payload).map_err(|err| HarnessError::Message(err.to_string()))?;
        line.push(b'\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HarnessError::Message("harness stdin closed".into()))?;
        stdin
            .write_all(&line)
            .await
            .map_err(|err| HarnessError::Message(format!("write turn: {err}")))?;
        stdin
            .flush()
            .await
            .map_err(|err| HarnessError::Message(format!("flush turn: {err}")))?;

        let deadline = if self.turn_timeout.is_zero() {
            None
        } else {
            Some(tokio::time::Instant::now() + self.turn_timeout)
        };
        loop {
            if self.cancelled() {
                self.kill_process_group().await;
                self.release().await;
                return Ok(HarnessTurnSettled {
                    assistant_text: String::new(),
                    stop_reason: HarnessStopReason::Cancelled,
                    usage: None,
                    native_session_id: self.native_session_id.clone(),
                });
            }
            if let Some(deadline) = deadline {
                if deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .is_zero()
                {
                    self.kill_process_group().await;
                    self.release().await;
                    return Ok(HarnessTurnSettled {
                        assistant_text: String::new(),
                        stop_reason: HarnessStopReason::TurnBudgetSpent,
                        usage: None,
                        native_session_id: self.native_session_id.clone(),
                    });
                }
            }
            let stdout = self
                .stdout
                .as_mut()
                .ok_or_else(|| HarnessError::Message("harness stdout closed".into()))?;
            let mut buf = Vec::new();
            tokio::select! {
                biased;
                _ = wait_for_signal(self.cancel.clone()) => {
                    self.kill_process_group().await;
                    self.release().await;
                    return Ok(HarnessTurnSettled {
                        assistant_text: String::new(),
                        stop_reason: HarnessStopReason::Cancelled,
                        usage: None,
                        native_session_id: self.native_session_id.clone(),
                    });
                }
                _ = wait_for_signal(self.turn_stop.clone()) => {
                    return Ok(self.settle_turn_stop().await);
                }
                _ = wait_until_deadline(deadline) => {
                    self.kill_process_group().await;
                    self.release().await;
                    return Ok(HarnessTurnSettled {
                        assistant_text: String::new(),
                        stop_reason: HarnessStopReason::TurnBudgetSpent,
                        usage: None,
                        native_session_id: self.native_session_id.clone(),
                    });
                }
                read = read_bounded_event(stdout, &mut buf) => {
                    match read {
                        Ok(0) => {
                            self.kill_process_group().await;
                            self.release().await;
                            return Err(HarnessError::Message(
                                "harness exited before a result event".into(),
                            ));
                        }
                        Err(err) => {
                            self.kill_process_group().await;
                            self.release().await;
                            return Err(HarnessError::Message(format!("read harness: {err}")));
                        }
                        Ok(_) => {
                            let trimmed = match std::str::from_utf8(&buf) {
                                Ok(text) => text.trim(),
                                Err(_) => continue,
                            };
                            if trimmed.is_empty() {
                                continue;
                            }
                            let value: Value = match serde_json::from_str(trimmed) {
                                Ok(value) => value,
                                Err(_) => continue,
                            };
                            if let Some(id) = value
                                .get("session_id")
                                .and_then(Value::as_str)
                                .filter(|id| !id.is_empty())
                            {
                                self.native_session_id = Some(id.to_string());
                            }
                            if let Some(delta) = parse_assistant_text_delta(&value) {
                                sink.emit(&delta);
                            }
                            if let Some(mut settled) = parse_result_event(&value) {
                                if settled.native_session_id.is_none() {
                                    settled.native_session_id = self.native_session_id.clone();
                                }
                                return Ok(settled);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for ClaudeCodeSession {
    fn drop(&mut self) {
        self.force_kill_now();
        let _ = std::fs::remove_file(&self.mcp_config_path);
        if self.revoked || self.retain_grant {
            return;
        }
        let grant = self.grant.clone();
        self.revoked = true;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                revoke_session_grant(&grant).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::engine::{
        HarnessSessionRequest, HarnessStreamSink, HarnessTurnInput,
    };
    use crate::magician_v2::execution::plane::grant::{plane_grant_registry, PlaneGrant};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tokio::time::timeout;

    fn engine() -> ClaudeCodeEngine {
        ClaudeCodeEngine {
            spawn: false,
            binary: PathBuf::from("claude"),
        }
    }

    fn write_fake_claude(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("fake-claude");
        std::fs::write(
            &path,
            r#"#!/usr/bin/env python3
import json, sys, time
for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    if "sleep a long time" in raw:
        time.sleep(60)
    sys.stdout.write(json.dumps({
        "type": "result",
        "subtype": "success",
        "is_error": False,
        "result": "ok",
        "session_id": "fake-session",
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }) + "\n")
    sys.stdout.flush()
"#,
        )
        .expect("write fake claude");
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    fn spawning_engine(binary: PathBuf) -> ClaudeCodeEngine {
        ClaudeCodeEngine {
            spawn: true,
            binary,
        }
    }

    #[test]
    fn assistant_stream_json_yields_text_deltas() {
        let delta = serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Hello"}}
        });
        assert_eq!(parse_assistant_text_delta(&delta).as_deref(), Some("Hello"));
        let thinking = serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hm"}}
        });
        assert!(parse_assistant_text_delta(&thinking).is_none());
        let whole = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "Hello world"}]}
        });
        assert!(
            parse_assistant_text_delta(&whole).is_none(),
            "the whole message repeats the streamed text and is not a delta"
        );
        assert!(parse_assistant_text_delta(&serde_json::json!({"type": "result"})).is_none());
    }

    #[test]
    fn the_argv_carries_every_flag_the_verified_run_used() {
        let argv = engine().argv(&HarnessSessionRequest::test());
        for flag in [
            "-p",
            "--output-format",
            "--input-format",
            "--verbose",
            "--include-partial-messages",
            "--permission-mode",
            "--tools",
            "--mcp-config",
            "--strict-mcp-config",
            "--setting-sources",
            "--append-system-prompt",
        ] {
            assert!(argv.iter().any(|a| a == flag), "missing {flag}: {argv:?}");
        }
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--input-format" && w[1] == "stream-json"));
    }

    #[test]
    fn the_grant_is_never_placed_on_the_command_line() {
        let joined = engine().argv(&HarnessSessionRequest::test()).join(" ");
        assert!(
            !joined.contains("plt_"),
            "a grant on argv is world-readable: {joined}"
        );
    }

    #[tokio::test]
    async fn two_turns_run_in_one_process() {
        let mut session = engine()
            .start_session(&HarnessSessionRequest::test())
            .await
            .expect("start");
        let a = session
            .turn(
                &HarnessTurnInput {
                    text: "say ONE".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn 1");
        let b = session
            .turn(
                &HarnessTurnInput {
                    text: "say TWO".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn 2");
        assert_eq!(a.stop_reason, HarnessStopReason::Settled);
        assert_eq!(b.stop_reason, HarnessStopReason::Settled);
        assert_eq!(
            session.process_spawn_count(),
            1,
            "turn 2 spawned a second process"
        );
    }

    #[tokio::test]
    async fn cancelling_kills_the_process_group_and_settles() {
        let token = CancellationToken::new();
        let mut req = HarnessSessionRequest::test();
        req.cancel = Some(token.clone());
        let mut session = engine().start_session(&req).await.unwrap();
        token.cancel();
        let handle = tokio::spawn(async move {
            session
                .turn(
                    &HarnessTurnInput {
                        text: "sleep a long time".to_string(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
        });
        let settled = timeout(Duration::from_secs(10), handle)
            .await
            .expect("cancel must settle, not hang")
            .expect("join")
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::Cancelled);
    }

    #[tokio::test]
    async fn a_configured_turn_timeout_ends_the_turn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut req = HarnessSessionRequest::test();
        req.turn_timeout = Duration::from_millis(150);
        let mut session = spawning_engine(write_fake_claude(dir.path()))
            .start_session(&req)
            .await
            .expect("start");
        let settled = timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "sleep a long time".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("turn must settle on the configured ceiling, not hang")
        .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::TurnBudgetSpent);
    }

    #[tokio::test]
    async fn approval_signal_ends_a_live_claude_turn_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut grant = PlaneGrant::for_test("exec-approval")
            .with_live_harness_turn()
            .with_approval_rule_for("read_file");
        grant.catalog_profile =
            crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
        let token = plane_grant_registry().mint(grant).await;
        let live_grant = plane_grant_registry()
            .resolve_run_scoped(&token)
            .await
            .expect("live grant");
        let mut req = HarnessSessionRequest::test();
        req.grant = token.clone();
        let mut session = spawning_engine(write_fake_claude(dir.path()))
            .start_session(&req)
            .await
            .expect("start");
        let handle = tokio::spawn(async move {
            session
                .turn(
                    &HarnessTurnInput {
                        text: "sleep a long time".to_string(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        let response = crate::magician_v2::execution::plane::plane_tools_call(
            &live_grant,
            "read_file",
            &json!({"path": "Cargo.toml"}),
        )
        .await;
        assert_eq!(response["_meta"]["planeTurnStop"], json!("needs_approval"));

        let settled = timeout(Duration::from_secs(10), handle)
            .await
            .expect("approval must settle, not wait for Claude's result")
            .expect("join")
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::NeedsApproval);
        // NeedsApproval retains the grant: the next decide resumes this run
        // with a fresh grant and revokes this one, and an in-flight dispatch
        // must not have its cancellation token tripped mid-action. See
        // `dropping_a_paused_session_does_not_revoke_the_grant`.
        assert!(
            plane_grant_registry().resolve(&token).await.is_some(),
            "NeedsApproval must pause, not revoke the live grant"
        );
        plane_grant_registry().revoke(&token).await;
    }

    #[tokio::test]
    async fn a_spent_tool_call_bound_settles_the_turn_and_releases_the_grant() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-budget-engine"))
            .await;
        let mut req = HarnessSessionRequest::test();
        req.grant = token.clone();
        let mut session = engine().start_session(&req).await.expect("start");
        plane_grant_registry()
            .resolve(&token)
            .await
            .expect("grant")
            .set_turn_stop(
                crate::magician_v2::execution::plane::grant::PlaneTurnStopReason::TurnBudgetSpent,
            );
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "gated".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::TurnBudgetSpent);
        assert!(
            plane_grant_registry().resolve(&token).await.is_none(),
            "a budget-spent turn is over; its grant must not linger"
        );
    }

    #[tokio::test]
    async fn the_grant_is_revoked_on_every_exit_path() {
        for path in ["settled", "cancel"] {
            let mut ctx = crate::magician_v2::execution::agentic::AgenticContext::default();
            ctx.execution_id = Some(format!("exec-{path}"));
            let mut grant = PlaneGrant::for_test(&format!("exec-{path}"));
            grant.ctx = ctx;
            grant.catalog_profile =
                crate::magician_v2::execution::plane::grant::PlaneCatalogProfile::SpawnedBare;
            let token = plane_grant_registry().mint(grant).await;
            let mut req = HarnessSessionRequest::test();
            req.grant = token.clone();
            if path == "cancel" {
                let cancel = CancellationToken::new();
                cancel.cancel();
                req.cancel = Some(cancel);
            }
            let mut session = engine().start_session(&req).await.expect("start");
            let _ = session
                .turn(
                    &HarnessTurnInput {
                        text: path.to_string(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await;
            session.shutdown().await;
            assert!(
                plane_grant_registry().resolve(&token).await.is_none(),
                "grant survived {path}"
            );
        }
    }

    #[tokio::test]
    async fn approval_ends_the_turn_without_revoking_the_grant() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-approval"))
            .await;
        let mut req = HarnessSessionRequest::test();
        req.grant = token.clone();
        let mut session = engine().start_session(&req).await.expect("start");
        plane_grant_registry()
            .resolve(&token)
            .await
            .expect("grant")
            .set_turn_stop(
                crate::magician_v2::execution::plane::grant::PlaneTurnStopReason::NeedsApproval,
            );
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "gated".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::NeedsApproval);
        assert!(
            plane_grant_registry().resolve(&token).await.is_some(),
            "NeedsApproval must pause, not revoke the live grant"
        );
        session.shutdown().await;
        plane_grant_registry().revoke(&token).await;
    }

    #[tokio::test]
    async fn dropping_a_paused_session_does_not_revoke_the_grant() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-drop-pause"))
            .await;
        let mut req = HarnessSessionRequest::test();
        req.grant = token.clone();
        let mut session = engine().start_session(&req).await.expect("start");
        plane_grant_registry()
            .resolve(&token)
            .await
            .expect("grant")
            .set_turn_stop(
                crate::magician_v2::execution::plane::grant::PlaneTurnStopReason::NeedsApproval,
            );
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "gated".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::NeedsApproval);
        drop(session);
        assert!(
            plane_grant_registry().resolve(&token).await.is_some(),
            "Drop after NeedsApproval must not revoke; Task 8 resumes this grant"
        );
        plane_grant_registry().revoke(&token).await;
    }

    #[tokio::test]
    async fn two_spawned_turns_run_in_one_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = spawning_engine(write_fake_claude(dir.path()))
            .start_session(&HarnessSessionRequest::test())
            .await
            .expect("start");
        let a = session
            .turn(
                &HarnessTurnInput {
                    text: "say ONE".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn 1");
        let b = session
            .turn(
                &HarnessTurnInput {
                    text: "say TWO".to_string(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .expect("turn 2");
        assert_eq!(a.stop_reason, HarnessStopReason::Settled);
        assert_eq!(b.stop_reason, HarnessStopReason::Settled);
        assert_eq!(a.native_session_id.as_deref(), Some("fake-session"));
        assert_eq!(
            session.process_spawn_count(),
            1,
            "turn 2 spawned a second process"
        );
        session.shutdown().await;
    }

    #[tokio::test]
    async fn cancelling_a_live_turn_kills_the_process_group_and_settles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = CancellationToken::new();
        let mut req = HarnessSessionRequest::test();
        req.cancel = Some(token.clone());
        let mut session = spawning_engine(write_fake_claude(dir.path()))
            .start_session(&req)
            .await
            .expect("start");
        let handle = tokio::spawn(async move {
            session
                .turn(
                    &HarnessTurnInput {
                        text: "sleep a long time".to_string(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        token.cancel();
        let settled = timeout(Duration::from_secs(10), handle)
            .await
            .expect("cancel must settle, not hang")
            .expect("join")
            .expect("turn");
        assert_eq!(settled.stop_reason, HarnessStopReason::Cancelled);
    }

    #[test]
    fn the_grant_is_never_placed_on_the_command_line_even_with_a_custom_binary() {
        let mut engine = engine();
        engine.binary = PathBuf::from("/opt/claude");
        let joined = engine.argv(&HarnessSessionRequest::test()).join(" ");
        assert!(!joined.contains("plt_"), "{joined}");
        assert!(joined.starts_with("/opt/claude "));
    }
}
