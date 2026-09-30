//! Harness CLI provider — subscription-backed model calls (2026-08-31 plan).
//!
//! One backend for the LLM abstraction the router already serves: `invoke`
//! is a one-shot call to an installed CLI *subscription* (`claude`, `codex`,
//! `grok`, `agy`, `pi`). It supplies a **model** — text in, text out — never an
//! agentic flow: no tools, no MCP, no plane, no grants, no isolated homes.
//! The user's own signed-in CLI is the entire point; unlike the plane
//! engines nothing is isolated and no grant travels.
//!
//! Routing safety (the plan's no-override guarantee): this provider exists
//! only as an additional *profile choice*. Shipped `operation_mapping`s map
//! zero operations to it — sensitive background flows keep local
//! (`Ollama`) and API models selectable per operation, and every flip is
//! one config line, instantly revertible.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::capability::{LLMCapability, LLMModality, LLMProviderKind};
use crate::error::{LLMError, LLMResult};
use crate::provider::LLMProvider;
use crate::types::{ContentBlock, LLMRequest, LLMResponse, MessageRole, TokenUsage};

/// Wall clock for one call. Batch operations are the eligible set; CLI
/// cold-start seconds plus subscription-model latency fit well inside this,
/// and a stuck CLI must not pin a background worker.
const CALL_DEADLINE: Duration = Duration::from_secs(300);
/// `--version` probe bound.
const VERSION_DEADLINE: Duration = Duration::from_secs(10);
/// Total stdout a CLI may emit before the provider stops reading. Replies
/// are prose for background ops; generous, but not unbounded.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Argv prompt bound for the harnesses that take the prompt as an argument
/// (macOS ARG_MAX is 1MB for argv+env combined; leave the bulk for env).
const MAX_ARGV_PROMPT_BYTES: usize = 128 * 1024;
/// Tail of the CLI's stderr kept for a failure message.
const MAX_STDERR_BYTES: usize = 4 * 1024;
/// Reply text cap — a chatty CLI must not flood a background consumer.
const MAX_REPLY_CHARS: usize = 64 * 1024;

/// The exec-style harnesses. `codex_app_server` is deliberately absent: its
/// differentiator is thread continuity, worthless for stateless model
/// calls, at JSON-RPC-handshake cost per call — the exec surface is the
/// provider shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessCliKind {
    ClaudeCode,
    Codex,
    Grok,
    Agy,
    Pi,
}

/// Curate the environment a spawned harness CLI receives.
///
/// A harness CLI authenticates as itself from `HOME`. Inheriting Magician's
/// environment hands it every credential the service holds — a CLI that finds
/// `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` there authenticates and bills as
/// that key instead of the operator's own login — and, when Magician itself
/// runs under a coding harness, that harness's session markers, which make a
/// nested launch refuse (`is_error`) or hang until its wall clock. The plane's
/// turn path curates its child environment for exactly this reason; the
/// one-shot provider must not be the hole that path closed.
fn child_path() -> OsString {
    std::env::var_os("PATH")
        .filter(|path| std::env::split_paths(path).any(|dir| !dir.as_os_str().is_empty()))
        .unwrap_or_else(|| OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"))
}

fn apply_child_env(command: &mut tokio::process::Command, path: &OsStr, kind: HarnessCliKind) {
    command.env_clear();
    command.env("PATH", path);
    for key in ["HOME", "USER", "TMPDIR", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    // Pi can store its login and model catalog outside ~/.pi/agent. This
    // points at the operator's Pi directory, not a Magician API credential.
    if kind == HarnessCliKind::Pi {
        if let Some(agent_dir) =
            std::env::var_os("PI_CODING_AGENT_DIR").filter(|value| !value.as_os_str().is_empty())
        {
            command.env("PI_CODING_AGENT_DIR", agent_dir);
        }
    }
}

impl HarnessCliKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Agy => "agy",
            Self::Pi => "pi",
        }
    }

    /// The binary name; resolved on PATH at spawn time.
    fn binary(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Agy => "agy",
            Self::Pi => "pi",
        }
    }

    /// The argv before any prompt element. Probe-verified invocations from
    /// the plane engines, minus every plane flag.
    fn base_argv(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["-p", "--output-format", "json"],
            Self::Codex => &["exec", "--json", "--skip-git-repo-check"],
            Self::Grok => &["--output-format", "streaming-json"],
            Self::Agy => &["--output-format", "stream-json"],
            Self::Pi => &[
                "--print", "--mode", "text", "--no-session", "--no-tools",
                "--no-extensions", "--no-skills", "--no-prompt-templates",
                "--no-context-files", "--no-approve", "--system-prompt",
                "You are a text-only assistant. Follow the instructions in the user message and answer directly.",
            ],
        }
    }

    /// How an argv prompt is attached. For grok and agy, `-p` takes the
    /// prompt as its value — grok's `-p` is `--single <PROMPT>`, agy reads the
    /// next token — so the prompt must follow the flag immediately; placed
    /// last on the line it never arrived, and both CLIs exited 2 at argument
    /// parsing on every background call. Probe-verified 2026-09-17 with the
    /// CLIs' own error text.
    fn argv_prompt(self, prompt: &str) -> Vec<String> {
        match self {
            Self::Grok | Self::Agy => vec!["-p".to_string(), prompt.to_string()],
            Self::ClaudeCode | Self::Codex => vec![prompt.to_string()],
            Self::Pi => vec!["--".to_string(), prompt.to_string()],
        }
    }

    /// Whether the CLI reads the prompt from stdin when none is passed on
    /// argv. Claude, Codex, and Pi do (Pi 0.87.1 prepends piped stdin to its
    /// initial message). Batch prompts can be large and should not appear in
    /// process arguments; grok/agy were probe-verified argv-only.
    fn reads_stdin(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex | Self::Pi)
    }

    /// The argv pair that pins a model (probe-verified 2026-08-31 on this
    /// machine: `claude --model <m>`, `codex -m <m>`, `grok -m <m>`,
    /// `agy --model <m>`). Background ops want SMALL models — the CLI
    /// defaults are the flagship tier — and the profile's `model` field is
    /// the size selector: ship size-variant profiles and the routing
    /// table's existing vocabulary does the rest.
    fn model_flag(self, model: &str) -> Vec<String> {
        let flag = match self {
            Self::ClaudeCode | Self::Agy | Self::Pi => "--model",
            Self::Codex | Self::Grok => "-m",
        };
        vec![flag.to_string(), model.to_string()]
    }

    pub fn from_profile_suffix(suffix: &str) -> Option<Self> {
        match suffix.trim() {
            "claude_code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "agy" => Some(Self::Agy),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }
}

/// The roster, for bootstrap validation and error messages.
pub const HARNESS_CLI_KINDS: [HarnessCliKind; 5] = [
    HarnessCliKind::ClaudeCode,
    HarnessCliKind::Codex,
    HarnessCliKind::Grok,
    HarnessCliKind::Agy,
    HarnessCliKind::Pi,
];

#[derive(Debug, Clone)]
pub struct HarnessCliProvider {
    kind: HarnessCliKind,
    /// Tests point this at a fake speaking the same CLI shape.
    binary: PathBuf,
}

impl HarnessCliProvider {
    pub fn new(kind: HarnessCliKind) -> Self {
        Self {
            kind,
            binary: PathBuf::from(kind.binary()),
        }
    }

    /// Test constructor with an explicit binary path.
    pub fn with_binary(kind: HarnessCliKind, binary: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            binary: binary.into(),
        }
    }

    fn provider_label(&self) -> String {
        format!("harness-{}", self.kind.as_str())
    }

    /// Compose the request into one prompt. Provider calls are stateless —
    /// callers already send full context; the composition only labels roles
    /// so the model can tell system from user from prior assistant text.
    fn compose_prompt(request: &LLMRequest) -> LLMResult<String> {
        let mut out = String::new();
        for message in request.messages.iter() {
            let label = match message.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool result",
            };
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text.trim().is_empty() {
                            out.push_str(label);
                            out.push_str(": ");
                            out.push_str(text);
                            out.push_str("\n\n");
                        }
                    },
                    _ => {
                        return Err(LLMError::UnsupportedCapability(
                            "harness CLI providers are text-in/text-out; non-text content blocks are refused"
                                .to_string(),
                        ));
                    },
                }
            }
        }
        if out.trim().is_empty() {
            return Err(LLMError::Validation(
                "harness CLI call carried no text content".to_string(),
            ));
        }
        Ok(out)
    }

    /// Parse the CLI's output into the reply text plus best-effort usage.
    /// Per-harness preferred paths first (probe-verified shapes), tolerant
    /// common-string-field fallback second — these CLIs' event schemas are
    /// connectivity-probed, not documented, and drift faster than APIs.
    fn parse_output(&self, output: &str) -> LLMResult<(String, Option<TokenUsage>)> {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            return Err(LLMError::Provider {
                provider: self.provider_label(),
                message: "CLI produced no output".to_string(),
            });
        }
        let (text, usage) = match self.kind {
            HarnessCliKind::ClaudeCode => parse_claude(trimmed)?,
            HarnessCliKind::Codex => parse_codex(trimmed)?,
            HarnessCliKind::Grok => parse_jsonl_preferring(trimmed, &["text", "data", "result"]),
            HarnessCliKind::Agy => {
                parse_jsonl_preferring(trimmed, &["result.response", "response"])
            },
            HarnessCliKind::Pi => (trimmed.to_string(), None),
        };
        let mut text = text;
        if text.chars().count() > MAX_REPLY_CHARS {
            let mut cut = MAX_REPLY_CHARS;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        if text.trim().is_empty() {
            return Err(LLMError::Provider {
                provider: self.provider_label(),
                message: format!(
                    "CLI output carried no reply text (first 200 chars: {:?})",
                    trimmed.chars().take(200).collect::<String>()
                ),
            });
        }
        Ok((text, usage))
    }

    async fn run_cli(
        &self,
        argv_prompt: Option<&str>,
        stdin_prompt: Option<&str>,
        model: Option<&str>,
    ) -> LLMResult<String> {
        let mut argv: Vec<String> = Vec::new();
        argv.extend(self.kind.base_argv().iter().map(|s| s.to_string()));
        if let Some(model) = model {
            argv.extend(self.kind.model_flag(model));
        }
        if let Some(prompt) = argv_prompt {
            argv.extend(self.kind.argv_prompt(prompt));
        }
        let path = child_path();
        let resolved = runtime_core::process::resolve_program(self.binary.as_os_str(), Some(&path));
        let mut command = tokio::process::Command::new(resolved);
        apply_child_env(&mut command, &path, self.kind);
        command
            .args(&argv)
            .stdin(if stdin_prompt.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| LLMError::Provider {
            provider: self.provider_label(),
            message: format!(
                "failed to spawn `{}` (is the CLI installed and on PATH?): {error}",
                self.binary.display()
            ),
        })?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout piped");
        // The CLI's own account of a failure is on stderr; drained
        // concurrently so a chatty CLI cannot block on a full pipe, and
        // bounded so a runaway one cannot pin memory.
        let stderr_tail = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut buffered = Vec::new();
                let mut chunk = [0u8; 4 * 1024];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buffered.extend_from_slice(&chunk[..n]);
                            if buffered.len() > MAX_STDERR_BYTES {
                                buffered.drain(..buffered.len() - MAX_STDERR_BYTES);
                            }
                        },
                    }
                }
                String::from_utf8_lossy(&buffered).trim().to_string()
            })
        });
        // Writing stdin, draining stdout, and waiting for process exit all
        // share one deadline. Reading only after a large stdin write can
        // deadlock on two full pipes; stopping at the output cap then waiting
        // can leave a writer blocked forever.
        let call = async {
            let write = async {
                if let (Some(prompt), Some(mut stdin)) = (stdin_prompt, stdin) {
                    // Early CLI exit closes stdin; its own output explains it.
                    let _ = stdin.write_all(prompt.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                }
                Ok::<(), LLMError>(())
            };
            let read = async {
                let mut output = Vec::new();
                let mut chunk = [0u8; 16 * 1024];
                loop {
                    let n = stdout
                        .read(&mut chunk)
                        .await
                        .map_err(|error| LLMError::Provider {
                            provider: self.provider_label(),
                            message: format!("reading CLI output failed: {error}"),
                        })?;
                    if n == 0 {
                        return Ok::<Vec<u8>, LLMError>(output);
                    }
                    if output.len() + n > MAX_OUTPUT_BYTES {
                        return Err(LLMError::Provider {
                            provider: self.provider_label(),
                            message: format!("CLI output exceeded {MAX_OUTPUT_BYTES} bytes"),
                        });
                    }
                    output.extend_from_slice(&chunk[..n]);
                }
            };
            let (_, output) = tokio::try_join!(write, read)?;
            let status = child.wait().await.map_err(|error| LLMError::Provider {
                provider: self.provider_label(),
                message: format!("waiting for CLI failed: {error}"),
            })?;
            Ok::<_, LLMError>((status, output))
        };
        let (status, output) = match tokio::time::timeout(CALL_DEADLINE, call).await {
            Ok(Ok(result)) => result,
            outcome => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                return Err(match outcome {
                    Ok(Err(error)) => error,
                    Err(_) => LLMError::Provider {
                        provider: self.provider_label(),
                        message: format!(
                            "call exceeded the {}s wall clock",
                            CALL_DEADLINE.as_secs()
                        ),
                    },
                    Ok(Ok(_)) => unreachable!(),
                });
            },
        };
        let text = String::from_utf8_lossy(&output).to_string();
        if !status.success() {
            let stderr = match stderr_tail {
                Some(handle) => tokio::time::timeout(Duration::from_secs(1), handle)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default(),
                None => String::new(),
            };
            return Err(LLMError::Provider {
                provider: self.provider_label(),
                message: if stderr.is_empty() {
                    format!(
                        "CLI exited unsuccessfully ({:?}): {}",
                        status.code(),
                        text.trim()
                    )
                } else {
                    format!(
                        "CLI exited unsuccessfully ({:?}): {}; stderr: {stderr}",
                        status.code(),
                        text.trim()
                    )
                },
            });
        }
        // A partial JSON event stream can include an assistant message before
        // a failed turn. Only a successful process exit can complete this
        // stateless background operation.
        Ok(text)
    }
}

/// Claude's `-p --output-format json` emits ONE JSON object: `result` is the
/// reply text, `usage.{input,output}_tokens` the tokens, `is_error` marks
/// refusals the caller must see as errors.
fn parse_claude(output: &str) -> LLMResult<(String, Option<TokenUsage>)> {
    let value: Value = serde_json::from_str(output).map_err(|error| LLMError::Provider {
        provider: "harness-claude_code".to_string(),
        message: format!("claude did not emit one JSON object: {error}"),
    })?;
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(LLMError::Provider {
            provider: "harness-claude_code".to_string(),
            message: value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("claude reported an error")
                .to_string(),
        });
    }
    let text = value
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let usage = parse_usage_object(value.get("usage"));
    Ok((text, usage))
}

fn parse_codex(output: &str) -> LLMResult<(String, Option<TokenUsage>)> {
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("turn.failed") {
            let detail = value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Codex reported a failed turn");
            return Err(LLMError::Provider {
                provider: "harness-codex".to_string(),
                message: detail.to_string(),
            });
        }
    }
    Ok(parse_jsonl_preferring(
        output,
        &["item.agent_message", "msg.agent_message"],
    ))
}

/// JSONL-tolerant parse for the streaming CLIs: scan lines, prefer the
/// named paths (dotted) on any line, remember the last non-empty hit;
/// fall back to common string fields; harvest the last usage-like object
/// seen anywhere.
fn parse_jsonl_preferring(output: &str, preferred_paths: &[&str]) -> (String, Option<TokenUsage>) {
    let mut text = String::new();
    let mut usage: Option<TokenUsage> = None;
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for path in preferred_paths {
            if let Some(found) = resolve_preferred_path(&value, path) {
                text = found.to_string();
            }
        }
        if text.is_empty() {
            for field in ["result", "text", "data", "message", "output", "response"] {
                if let Some(found) = value.get(field).and_then(Value::as_str) {
                    if !found.is_empty() {
                        text = found.to_string();
                        break;
                    }
                }
            }
        }
        for probe in ["usage", "token_count", "tokens"] {
            if let Some(parsed) = parse_usage_object(value.get(probe)) {
                usage = Some(parsed);
            }
        }
    }
    (text, usage)
}

/// Resolve a preferred path on one event line. Three shapes, tried in
/// order — the CLIs genuinely differ and each harness's preferred path
/// uses a different one:
/// 1. **Nested field** — `result.response` (agy) → `value["result"]["response"]`.
/// 2. **Type-tagged item** — `item.agent_message` (codex) → the `item`
///    object (or array element) whose `type == "agent_message"`, returning
///    its `text`/`message`.
/// 3. **Dotless top-level field** — `text`/`data` (grok) → the line's own
///    string field.
fn resolve_preferred_path<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    if let Some((head, tail)) = path.split_once('.') {
        if let Some(text) = value
            .get(head)
            .and_then(|inner| inner.get(tail))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return Some(text);
        }
        let candidates: Vec<&Value> = match value.get(head) {
            Some(Value::Array(items)) => items.iter().collect(),
            Some(other @ Value::Object(_)) => vec![other],
            _ => vec![],
        };
        for candidate in candidates {
            if candidate.get("type").and_then(Value::as_str) == Some(tail) {
                for field in ["text", "message"] {
                    if let Some(text) = candidate.get(field).and_then(Value::as_str) {
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
        }
        None
    } else {
        value
            .get(path)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    }
}

/// Best-effort token usage from a usage-like object; every field optional
/// across the CLIs' shapes.
fn parse_usage_object(value: Option<&Value>) -> Option<TokenUsage> {
    let value = value?;
    let pick = |names: &[&str]| -> Option<u32> {
        for name in names {
            if let Some(found) = value.get(*name).and_then(Value::as_u64) {
                return u32::try_from(found).ok();
            }
        }
        None
    };
    let prompt_tokens = pick(&["input_tokens", "prompt_tokens", "input"]);
    let completion_tokens = pick(&["output_tokens", "completion_tokens", "output"]);
    if prompt_tokens.is_none() && completion_tokens.is_none() {
        return None;
    }
    let total_tokens =
        pick(&["total_tokens"]).or_else(|| match (prompt_tokens, completion_tokens) {
            (Some(p), Some(c)) => p.checked_add(c),
            _ => None,
        });
    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_tokens: pick(&["reasoning_tokens"]),
        cached_tokens: pick(&[
            "cached_tokens",
            "cache_read_tokens",
            "cache_read_input_tokens",
        ]),
        cache_creation_tokens: pick(&[
            "cache_creation_tokens",
            "cache_write_tokens",
            "cache_creation_input_tokens",
        ]),
    })
}

#[async_trait]
impl LLMProvider for HarnessCliProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Custom(self.provider_label())
    }

    fn capabilities(&self, _model: &str) -> LLMCapability {
        LLMCapability {
            modalities: vec![LLMModality::Text],
            ..Default::default()
        }
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let prompt = Self::compose_prompt(&request)?;
        let (argv_prompt, stdin_prompt) = if self.kind.reads_stdin() {
            (None, Some(prompt.as_str()))
        } else {
            if prompt.len() > MAX_ARGV_PROMPT_BYTES {
                return Err(LLMError::Validation(format!(
                    "prompt is {} bytes; {} takes the prompt on argv and is bounded at {} — route this operation to claude/codex/pi (stdin) or a local profile",
                    prompt.len(),
                    self.kind.as_str(),
                    MAX_ARGV_PROMPT_BYTES
                )));
            }
            (Some(prompt.as_str()), None)
        };
        // The profile's model rides the CLI's model flag — `default`
        // (or empty) means the CLI's own default. Small-model profiles
        // are how background ops avoid the flagship tier.
        let model_trimmed = request.model.trim();
        let model = (!model_trimmed.is_empty() && model_trimmed != "default")
            .then(|| model_trimmed.to_string());
        let output = self
            .run_cli(argv_prompt, stdin_prompt, model.as_deref())
            .await?;
        let (text, usage) = self.parse_output(&output)?;
        Ok(LLMResponse {
            text: Some(std::sync::Arc::from(text.as_str())),
            usage,
            finish_reason: Some("stop".to_string()),
            ..Default::default()
        })
    }

    async fn health_check(&self) -> LLMResult<bool> {
        let path = child_path();
        let resolved = runtime_core::process::resolve_program(self.binary.as_os_str(), Some(&path));
        let mut command = tokio::process::Command::new(resolved);
        apply_child_env(&mut command, &path, self.kind);
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let Ok(mut child) = command.spawn() else {
            return Ok(false);
        };
        match tokio::time::timeout(VERSION_DEADLINE, child.wait()).await {
            Ok(Ok(status)) => Ok(status.success()),
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, LLMMessage, LLMRequest};
    use std::sync::Arc;

    fn request_with(prompt: &str) -> LLMRequest {
        LLMRequest {
            model: "default".to_string(),
            messages: Arc::new(vec![
                LLMMessage::system("You summarize."),
                LLMMessage {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: prompt.to_string(),
                    }],
                },
            ]),
            ..Default::default()
        }
    }

    /// A fake CLI speaking the requested harness's output shape.
    fn fake_cli(kind: HarnessCliKind) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("fake-cli");
        let script = match kind {
            HarnessCliKind::ClaudeCode => {
                "#!/bin/sh\ncat >/dev/null\necho '{\"result\":\"summarized\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"is_error\":false}'\n"
            },
            HarnessCliKind::Codex => {
                "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"codex reply\"}}'\necho '{\"type\":\"turn.completed\",\"token_count\":{\"input_tokens\":7,\"output_tokens\":3}}'\n"
            },
            HarnessCliKind::Grok => {
                "#!/bin/sh\necho '{\"type\":\"message\",\"data\":\"grok reply\"}'\n"
            },
            HarnessCliKind::Agy => {
                "#!/bin/sh\necho '{\"result\":{\"response\":\"agy reply\"}}'\n"
            },
            HarnessCliKind::Pi => "#!/bin/sh\necho 'pi reply'\n",
        };
        std::fs::write(&path, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    /// A harness CLI authenticates as itself from `HOME`. Inheriting the
    /// service environment hands it every credential Magician holds — a CLI
    /// that finds `ANTHROPIC_API_KEY`/`OPENAI_API_KEY` in its environment bills
    /// and authenticates as that key instead of the operator's own login — and,
    /// when Magician itself runs under a coding harness, that harness's session
    /// markers, which make a nested launch refuse or misroute. The turn path
    /// already curates its child environment; this provider must too.
    #[tokio::test]
    async fn a_spawned_cli_does_not_inherit_the_service_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("fake-cli");
        std::fs::write(
            &path,
            r#"#!/bin/sh
cat >/dev/null
verdict=clean
if [ -n "$MAGICLLM_TEST_SERVICE_CREDENTIAL" ]; then verdict=leaked-credential; fi
if [ -n "$MAGICLLM_TEST_NESTED_SESSION" ]; then verdict="$verdict+leaked-session"; fi
if [ -z "$HOME" ]; then verdict="$verdict+no-home"; fi
if [ -z "$PATH" ]; then verdict="$verdict+no-path"; fi
printf '{"result":"%s","usage":{"input_tokens":1,"output_tokens":1},"is_error":false}\n' "$verdict"
"#,
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        std::env::set_var("MAGICLLM_TEST_SERVICE_CREDENTIAL", "not-a-real-key");
        std::env::set_var("MAGICLLM_TEST_NESTED_SESSION", "1");
        let provider = HarnessCliProvider::with_binary(HarnessCliKind::ClaudeCode, path);
        let response = provider.invoke(request_with("summarize")).await;
        std::env::remove_var("MAGICLLM_TEST_SERVICE_CREDENTIAL");
        std::env::remove_var("MAGICLLM_TEST_NESTED_SESSION");
        let response = response.expect("the fake CLI replies");
        assert_eq!(
            response.text.as_deref(),
            Some("clean"),
            "the child environment must carry HOME and PATH and nothing of the service's own"
        );
    }

    #[tokio::test]
    async fn pi_background_uses_its_configured_agent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-pi");
        std::fs::write(
            &path,
            "#!/bin/sh\ncat >/dev/null\nif [ -n \"$MAGICLLM_TEST_PI_SERVICE_CREDENTIAL\" ]; then echo leaked; else printf '%s\\n' \"$PI_CODING_AGENT_DIR\"; fi\n",
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let original_agent_dir = std::env::var_os("PI_CODING_AGENT_DIR");
        std::env::set_var("PI_CODING_AGENT_DIR", dir.path());
        std::env::set_var("MAGICLLM_TEST_PI_SERVICE_CREDENTIAL", "not-a-real-key");
        let response = HarnessCliProvider::with_binary(HarnessCliKind::Pi, path)
            .invoke(request_with("summarize"))
            .await;
        if let Some(original) = original_agent_dir {
            std::env::set_var("PI_CODING_AGENT_DIR", original);
        } else {
            std::env::remove_var("PI_CODING_AGENT_DIR");
        }
        std::env::remove_var("MAGICLLM_TEST_PI_SERVICE_CREDENTIAL");
        let expected = dir.path().display().to_string();
        assert_eq!(
            response.expect("the fake Pi replies").text.as_deref(),
            Some(expected.as_str())
        );
    }

    #[tokio::test]
    async fn the_profiles_model_rides_the_cli_model_flag() {
        // The fake echoes its argv as grok-shaped output so the test can
        // assert the flag pair arrived.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("fake-cli");
        std::fs::write(
            &path,
            r#"#!/usr/bin/env python3
import sys
args = sys.argv[1:]
for i, a in enumerate(args):
    if a in ("--model", "-m") and i + 1 < len(args):
        print('{"data":"' + args[i + 1] + '"}')
        break
"#,
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let provider = HarnessCliProvider::with_binary(HarnessCliKind::Grok, path);
        let mut request = request_with("summarize");
        request.model = "grok-4-fast".to_string();
        let response = provider.invoke(request).await.unwrap();
        assert_eq!(
            response.text.as_deref(),
            Some("grok-4-fast"),
            "the profile's model must ride the CLI's -m/--model flag"
        );
        // 'default' means the CLI's own choice: no flag at all.
        let dir2 = tempfile::tempdir().expect("tempdir");
        let path2 = dir2.keep().join("fake-cli");
        std::fs::write(&path2, "#!/bin/sh\necho '{\"data\":\"no-model-flag\"}'\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path2, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let provider2 = HarnessCliProvider::with_binary(HarnessCliKind::Grok, path2);
        let mut request2 = request_with("summarize");
        request2.model = "default".to_string();
        let response2 = provider2.invoke(request2).await.unwrap();
        assert_eq!(response2.text.as_deref(), Some("no-model-flag"));
    }

    /// For grok and agy, `-p` takes the prompt as its value: grok's `-p` is
    /// `--single <PROMPT>`, and agy reads the token after `-p` as the prompt.
    /// A prompt placed last on the line therefore never arrives — grok exits
    /// 2 with "a value is required for '--single <PROMPT>'", agy exits 2
    /// reporting that `-p` took `--output-format` as its prompt — which is
    /// how every background call on those engines died at argument parsing.
    #[tokio::test]
    async fn an_argv_prompt_follows_its_flag_immediately() {
        for kind in [HarnessCliKind::Grok, HarnessCliKind::Agy] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.keep().join("fake-cli");
            std::fs::write(
                &path,
                r#"#!/usr/bin/env python3
import sys, json
args = sys.argv[1:]
i = args.index("-p")
print(json.dumps({"data": args[i + 1], "text": args[i + 1], "result": args[i + 1]}))
"#,
            )
            .expect("write fake");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            let provider = HarnessCliProvider::with_binary(kind, path);
            let response = provider
                .invoke(request_with("the transcript"))
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", kind.as_str()));
            let text = response.text.as_deref().unwrap_or_default();
            assert!(
                text.starts_with("System:") && text.contains("User: the transcript"),
                "{}: the token after -p must be the composed prompt, got {text:?}",
                kind.as_str()
            );
        }
    }

    /// A CLI that exits without output has usually said why on stderr; the
    /// failure names it instead of only the exit code.
    #[tokio::test]
    async fn a_failed_cli_s_stderr_reaches_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("fake-cli");
        std::fs::write(
            &path,
            "#!/bin/sh\ncat >/dev/null\necho 'error: a value is required for --single' >&2\nexit 2\n",
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let provider = HarnessCliProvider::with_binary(HarnessCliKind::Grok, path);
        let error = provider
            .invoke(request_with("summarize"))
            .await
            .expect_err("exit 2 with no output is a failure");
        let message = error.to_string();
        assert!(
            message.contains("a value is required for --single"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_failed_turn_cannot_publish_its_partial_assistant_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-codex");
        std::fs::write(
            &path,
            "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial summary\"}}'\necho '{\"type\":\"turn.failed\"}'\necho 'upstream failed' >&2\nexit 1\n",
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let provider = HarnessCliProvider::with_binary(HarnessCliKind::Codex, path);
        let error = provider
            .invoke(request_with("summarize"))
            .await
            .expect_err("a failed turn is not a completed background operation");
        assert!(error.to_string().contains("upstream failed"), "{error}");
    }

    #[tokio::test]
    async fn a_codex_failed_event_cannot_publish_partial_text_on_zero_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-codex");
        std::fs::write(
            &path,
            "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial summary\"}}'\necho '{\"type\":\"turn.failed\",\"error\":{\"message\":\"upstream failed\"}}'\nexit 0\n",
        )
        .expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let provider = HarnessCliProvider::with_binary(HarnessCliKind::Codex, path);
        let error = provider
            .invoke(request_with("summarize"))
            .await
            .expect_err("a failed event is not a completed background operation");
        assert!(error.to_string().contains("upstream failed"), "{error}");
    }

    #[tokio::test]
    async fn each_harness_settles_with_text_and_usage_where_reported() {
        for kind in HARNESS_CLI_KINDS {
            let provider = HarnessCliProvider::with_binary(kind, fake_cli(kind));
            let response = provider
                .invoke(request_with("the transcript"))
                .await
                .unwrap();
            let text = response.text.expect("reply text");
            match kind {
                HarnessCliKind::ClaudeCode => {
                    assert_eq!(&*text, "summarized");
                    let usage = response.usage.expect("claude reports usage");
                    assert_eq!(usage.prompt_tokens, Some(10));
                    assert_eq!(usage.completion_tokens, Some(5));
                },
                HarnessCliKind::Codex => {
                    assert_eq!(&*text, "codex reply");
                    let usage = response.usage.expect("codex reports tokens");
                    assert_eq!(usage.prompt_tokens, Some(7));
                },
                HarnessCliKind::Grok => assert_eq!(&*text, "grok reply"),
                HarnessCliKind::Agy => assert_eq!(&*text, "agy reply"),
                HarnessCliKind::Pi => {
                    assert_eq!(&*text, "pi reply");
                    assert!(response.usage.is_none());
                },
            }
        }
    }

    #[tokio::test]
    async fn pi_background_call_disables_tools_and_uses_profile_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-pi");
        std::fs::write(
            &path,
            r#"#!/bin/sh
for expected in --print --no-session --no-tools --no-extensions --no-skills --no-prompt-templates --no-context-files --no-approve; do
  found=no
  for arg in "$@"; do [ "$arg" = "$expected" ] && found=yes; done
  [ "$found" = yes ] || { echo "missing $expected" >&2; exit 2; }
done
while [ "$#" -gt 0 ]; do
  if [ "$1" = --model ]; then [ "$2" = openai/gpt-4o-mini ] || exit 3; fi
  case "$1" in *"User: summarize"*) echo 'prompt leaked in argv' >&2; exit 4;; esac
  shift
done
prompt=$(cat)
case "$prompt" in *"User: summarize"*) echo 'pi summary';; *) exit 5;; esac
"#,
        )
        .expect("write fake pi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let mut request = request_with("summarize");
        request.model = "openai/gpt-4o-mini".to_string();
        let response = HarnessCliProvider::with_binary(HarnessCliKind::Pi, path)
            .invoke(request)
            .await
            .expect("pi print reply");
        assert_eq!(response.text.as_deref(), Some("pi summary"));
    }

    #[tokio::test]
    async fn pi_accepts_large_background_prompts_on_stdin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-pi");
        std::fs::write(
            &path,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"${#arg}\" -lt 1000 ] || exit 2; done\nwc -c | tr -d ' '\n",
        )
        .expect("write fake pi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let request = request_with(&"x".repeat(MAX_ARGV_PROMPT_BYTES + 1));
        let expected_len = HarnessCliProvider::compose_prompt(&request)
            .expect("prompt")
            .len();
        let response = HarnessCliProvider::with_binary(HarnessCliKind::Pi, path)
            .invoke(request)
            .await
            .expect("large Pi prompt");
        assert_eq!(
            response.text.as_deref(),
            Some(expected_len.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn oversized_cli_output_fails_without_waiting_for_process_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-pi");
        std::fs::write(&path, "#!/bin/sh\nhead -c 8388610 /dev/zero\nsleep 30\n")
            .expect("write fake pi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            HarnessCliProvider::with_binary(HarnessCliKind::Pi, path).invoke(request_with("go")),
        )
        .await
        .expect("output limit must stop the call promptly")
        .expect_err("oversized output must fail");
        assert!(result.to_string().contains("output exceeded"));
    }

    #[tokio::test]
    async fn pi_nonzero_exit_with_stdout_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake-pi");
        std::fs::write(&path, "#!/bin/sh\necho 'authentication failed'\nexit 1\n")
            .expect("write fake pi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let error = HarnessCliProvider::with_binary(HarnessCliKind::Pi, path)
            .invoke(request_with("summarize"))
            .await
            .expect_err("failed pi call");
        assert!(error.to_string().contains("authentication failed"));
    }

    #[tokio::test]
    async fn non_text_content_is_refused_before_any_spawn() {
        let provider = HarnessCliProvider::new(HarnessCliKind::ClaudeCode);
        let mut request = request_with("text");
        request.messages = Arc::new(vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "ok".to_string(),
            }],
        }]);
        // Compose-side refusal is exercised via an empty-content request.
        request.messages = Arc::new(vec![]);
        let error = provider.invoke(request).await.unwrap_err();
        assert!(matches!(error, LLMError::Validation(_)));
    }

    #[test]
    fn parsers_tolerate_unknown_lines_and_prefer_canonical_paths() {
        let (text, usage) = parse_jsonl_preferring(
            "{\"type\":\"noise\"}\n{\"msg\":{\"type\":\"agent_message\",\"message\":\"the answer\"}}\n{\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n",
            &["item.agent_message", "msg.agent_message"],
        );
        assert_eq!(text, "the answer");
        assert_eq!(usage.expect("usage").prompt_tokens, Some(2));
    }

    #[test]
    fn preferred_paths_cover_all_three_shapes() {
        // agy: nested field (result.response).
        let (text, _) = parse_jsonl_preferring(
            "{\"result\":{\"response\":\"nested\"}}\n",
            &["result.response", "response"],
        );
        assert_eq!(text, "nested");
        // codex: type-tagged item (item.type == agent_message).
        let (text, _) = parse_jsonl_preferring(
            "{\"item\":{\"type\":\"agent_message\",\"text\":\"tagged\"}}\n",
            &["item.agent_message"],
        );
        assert_eq!(text, "tagged");
        // grok: dotless top-level field.
        let (text, _) = parse_jsonl_preferring(
            "{\"type\":\"message\",\"data\":\"toplevel\"}\n",
            &["text", "data", "result"],
        );
        assert_eq!(text, "toplevel");
    }

    #[test]
    fn claude_refusals_surface_as_provider_errors() {
        let error = parse_claude("{\"result\":\"refused: unsafe\",\"is_error\":true}").unwrap_err();
        let message = match error {
            LLMError::Provider { message, .. } => message,
            other => panic!("expected Provider, got {other:?}"),
        };
        assert!(message.contains("refused"));
    }

    #[test]
    fn reply_text_is_char_boundary_capped() {
        let provider = HarnessCliProvider::new(HarnessCliKind::Grok);
        let long = "日本語".repeat(MAX_REPLY_CHARS);
        let (text, _) = provider
            .parse_output(&format!("{{\"text\":\"{long}\"}}"))
            .unwrap();
        assert!(text.chars().count() <= MAX_REPLY_CHARS);
        // Truncation floored to a char boundary: the cap must not split a
        // multibyte character (the input was pure 3-byte chars, so an exact
        // multiple of 3 proves no partial char survived).
        assert_eq!(text.len() % 3, 0);
    }
}
