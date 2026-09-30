//! Grok as a plane harness (plane plan Task 1c, retest 2026-08-24).
//!
//! Inject is verified (HTTP MCP table in isolated `config.toml`), the
//! doctor handshake and `tools/list` work, and `grok -p` with
//! `--output-format streaming-messages-json --include-partial-messages` is
//! the headless launch (probe-verified 2026-09-13): JSONL, an init line
//! first (`system`/`init`, naming the session), then one `stream_event`
//! line per token — a `content_block_delta` whose `text_delta` is the reply
//! streamed through the plane sink, while a `thinking_delta` is never the
//! reply — then the whole message as an `assistant` line (its `message` is
//! an object the collector skips), and last a `result` line whose `result`
//! string repeats the full reply (the collector's text when nothing
//! streamed; the streamed text wins otherwise). Isolated `GROK_HOME` gets
//! a copy of the operator `auth.json` plus `XAI_API_KEY` — without that
//! the child is unsigned-in.
//! stdin is `/dev/null`, so `--permission-mode bypassPermissions` is
//! required or MCP calls hang. Native-tool strip is an empty `--tools`
//! allowlist. MCP meta-tools remain. A documented built-in denylist plus
//! `--no-subagents` and `--disable-web-search` is the leak-net. `--sandbox`
//! is not used: this host refuses `read-only` when docker.sock is a
//! symlink. Grok persists its sessions in `GROK_HOME` and names the one it
//! runs on the init line: the session reports that id when its home
//! outlives it, and the next turn resumes it with `--resume <id>`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::magician_v2::execution::plane::engine::{
    HarnessCapabilities, HarnessEngine, HarnessError, HarnessSession, HarnessSessionRequest,
};
use crate::magician_v2::execution::plane::engines::oneshot::{
    inherit_env, operator_cli_home, require_cli_auth, seed_named_file, write_private, HomeInstall,
    OneShotEngineSpec, OneShotSession,
};

const SERVER_NAME: &str = "magician_plane";

/// Built-in IDs from Grok's headless docs plus aliases in the binary
/// (`run_terminal_cmd` and `run_terminal_command`). MCP meta-tools are
/// not on this list.
const GROK_NATIVE_DENYLIST: &str = "run_terminal_cmd,run_terminal_command,grep,read_file,search_replace,list_dir,web_search,web_fetch,todo_write,task,Agent,spawn_subagent,memory_search,image_gen,image_edit,image_to_video,edit_file,write_file,glob,bash,get_command_or_subagent_output,wait_commands_or_subagents,kill_command_or_subagent";

/// The prompt sits at index 2 (the spawner's model flag goes in before the
/// last element, a flag); a warm turn adds `--resume <id>` right after it.
fn argv_for_turn(text: &str, resume: Option<&str>) -> Vec<String> {
    let mut argv = vec!["grok".to_string(), "-p".to_string(), text.to_string()];
    if let Some(id) = resume {
        argv.push("--resume".to_string());
        argv.push(id.to_string());
    }
    argv.extend(
        [
            "--output-format",
            "streaming-messages-json",
            "--include-partial-messages",
            "--permission-mode",
            "bypassPermissions",
            "--no-leader",
            "--tools",
            "",
            "--disallowed-tools",
            GROK_NATIVE_DENYLIST,
            "--no-subagents",
            "--disable-web-search",
        ]
        .map(str::to_string),
    );
    argv
}

/// The session id from the init line the streaming formats print first
/// (`type` `system`, `subtype` `init` → `session_id`), else a top-level
/// `sessionId` on any other value — how the `json` format names it (one
/// pretty-printed document carrying `sessionId` beside `text`, `thought`
/// and `stopReason`, probe-verified 2026-09-13, read from the whole stdout
/// at exit since no single line of it parses). The launch streams now, so
/// the init line is the source; the document hedge is kept, and harmless,
/// since no streaming line carries that camel-case key.
fn native_session_id_of(value: &Value) -> Option<String> {
    let init_line = value.get("type").and_then(Value::as_str) == Some("system")
        && value.get("subtype").and_then(Value::as_str) == Some("init");
    let field = if init_line { "session_id" } else { "sessionId" };
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// A reply text delta from a `stream_event` line: a `content_block_delta`
/// whose delta is a `text_delta`. A `thinking_delta` rides the same event
/// shape and is never the reply; the `assistant` and `result` lines that
/// follow repeat the streamed text whole and carry no delta.
fn text_delta_of(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("stream_event") {
        return None;
    }
    if value.pointer("/event/type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    if value.pointer("/event/delta/type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    value
        .pointer("/event/delta/text")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn install_config(install: &HomeInstall) -> std::io::Result<()> {
    require_cli_auth("GROK_HOME", ".grok", "XAI_API_KEY", "grok")?;
    std::fs::create_dir_all(&install.home)?;
    seed_named_file(
        &operator_cli_home("GROK_HOME", ".grok"),
        &install.home,
        "auth.json",
    )?;
    // File + 0600, never argv. Grant rides MAGICIAN_PLANE_GRANT like Codex.
    let url = install
        .endpoint_url
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let config = format!(
        "[compat.claude]\n\
         mcps = false\n\
         hooks = false\n\
         skills = false\n\
         rules = false\n\
         agents = false\n\
         [compat.cursor]\n\
         mcps = false\n\
         hooks = false\n\
         skills = false\n\
         rules = false\n\
         agents = false\n\
         [mcp_servers.{SERVER_NAME}]\n\
         url = \"{url}\"\n\
         bearer_token_env_var = \"MAGICIAN_PLANE_GRANT\"\n"
    );
    write_private(&install.home.join("config.toml"), config.as_bytes())
}

fn remove_config(_install: &HomeInstall) {
    // Nothing global to undo: the config lives in the isolated GROK_HOME
    // (the session's own temp dir, or the conversation's persistent home,
    // re-installed per session).
}

#[derive(Debug, Clone)]
pub struct GrokEngine {
    /// Tests point this at a fake speaking the same CLI shape.
    pub binary: PathBuf,
}

impl Default for GrokEngine {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("grok"),
        }
    }
}

#[async_trait]
impl HarnessEngine for GrokEngine {
    fn name(&self) -> &'static str {
        "grok"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            // `--resume <id>` in a persistent GROK_HOME.
            supports_resume: true,
            tools_list_changed: false,
            // `streaming-messages-json --include-partial-messages` emits a
            // `stream_event` per token.
            streams_text_deltas: true,
            native_tool_posture:
                crate::magician_v2::execution::plane::engine::NativeToolPosture::Stripped,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let planning_only = req.planning_only;
        let planner_system = req.system_prompt.clone();
        let spec = OneShotEngineSpec {
            binary: self.binary.clone(),
            argv_for_turn: Box::new(move |text, resume| {
                let mut argv = argv_for_turn(text, resume);
                if planning_only {
                    // Constrain the terminal reply while keeping progress
                    // streaming for the idle watchdog during long model calls.
                    argv.extend([
                        "--verbatim".into(),
                        "--system-prompt-override".into(),
                        planner_system.clone(),
                        "--json-schema".into(),
                        decision_engine_contract::action::planner_reply_schema().to_string(),
                    ]);
                }
                argv
            }),
            native_session_id_of,
            text_delta_of,
            env_for: |install| {
                let home = install.home.display().to_string();
                let mut env = vec![
                    ("GROK_HOME".to_string(), home.clone()),
                    // Hide ~/.claude.json and Claude plugin MCP. Compat
                    // flags disable config.toml sources; plugins still
                    // load from $HOME/.claude/plugins otherwise.
                    ("HOME".to_string(), home),
                    ("MAGICIAN_PLANE_GRANT".to_string(), install.grant.clone()),
                    ("GROK_CLAUDE_MCPS_ENABLED".to_string(), "false".to_string()),
                    ("GROK_CURSOR_MCPS_ENABLED".to_string(), "false".to_string()),
                    ("GROK_CLAUDE_HOOKS_ENABLED".to_string(), "false".to_string()),
                    ("GROK_CURSOR_HOOKS_ENABLED".to_string(), "false".to_string()),
                ];
                env.extend(inherit_env(&["XAI_API_KEY"]));
                env
            },
            install_config,
            remove_config,
            use_isolated_home_as_cwd: true,
            planner_system_supplied: true,
            planner_response_boundary: None,
        };
        Ok(Box::new(OneShotSession::new(spec, req)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::engine::NativeToolPosture;

    #[test]
    fn empty_tools_allowlist_and_denylist_are_on_argv() {
        let argv = argv_for_turn("hello", None);
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--tools" && w[1].is_empty()));
        assert!(argv
            .windows(2)
            .any(|w| { w[0] == "--disallowed-tools" && w[1] == GROK_NATIVE_DENYLIST }));
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "streaming-messages-json"));
        assert!(
            argv.iter().any(|a| a == "--include-partial-messages"),
            "per-token deltas need the partial-message flag"
        );
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--permission-mode" && w[1] == "bypassPermissions"));
        assert!(argv.iter().any(|a| a == "--no-leader"));
        assert!(argv.iter().any(|a| a == "--no-subagents"));
        assert!(argv.iter().any(|a| a == "--disable-web-search"));
        assert!(
            !argv.iter().any(|a| a == "--sandbox"),
            "sandbox fail-closes this host; denylist is the strip"
        );
        assert!(GROK_NATIVE_DENYLIST.contains("run_terminal_cmd"));
        assert!(GROK_NATIVE_DENYLIST.contains("run_terminal_command"));
        assert!(!GROK_NATIVE_DENYLIST.contains("search_tool"));
        assert!(!GROK_NATIVE_DENYLIST.contains("use_tool"));
    }

    #[test]
    fn grok_chat_mouth_is_stripped() {
        assert_eq!(
            GrokEngine::default().capabilities().native_tool_posture,
            NativeToolPosture::Stripped
        );
    }

    /// A warm turn adds `--resume <id>` right after the prompt, ahead of
    /// the output format; a cold turn carries no resume flag. The prompt
    /// stays at index 2 either way.
    #[test]
    fn warm_argv_resumes_the_native_session() {
        let cold = argv_for_turn("hello", None);
        assert_eq!(&cold[..3], ["grok", "-p", "hello"]);
        assert!(!cold.iter().any(|a| a == "--resume"));

        let warm = argv_for_turn("hello", Some("sess-1"));
        assert_eq!(&warm[..5], ["grok", "-p", "hello", "--resume", "sess-1"]);
        assert_eq!(&warm[5..], &cold[3..], "the rest is the cold turn's");
        assert!(GrokEngine::default().capabilities().supports_resume);
    }

    /// The id is on the init line a `-p` run prints first, or — when the
    /// reply comes as one `json` document — on that document.
    #[test]
    fn native_session_id_is_read_from_the_init_line() {
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "sess-1"
            }))
            .as_deref(),
            Some("sess-1")
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": ""
            })),
            None
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "text": "PONG",
                "stopReason": "end_turn",
                "sessionId": "sess-2"
            }))
            .as_deref(),
            Some("sess-2")
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({"type": "text", "data": "PONG"})),
            None
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "type": "system",
                "subtype": "hook",
                "session_id": "sess-3"
            })),
            None,
            "only the init line names the session under `session_id`"
        );
    }

    /// Only a `text_delta` content-block delta is streamed: a thinking
    /// delta rides the same event shape and stays private, and the init,
    /// assistant and result lines carry no delta at all.
    #[test]
    fn grok_text_deltas_are_streamed_and_thinking_is_not() {
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "stream_event",
                "event": {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "PO"}
                },
                "session_id": "sess-1"
            }))
            .as_deref(),
            Some("PO")
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "stream_event",
                "event": {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "thinking_delta", "thinking": "the user wants a ping"}
                },
                "session_id": "sess-1"
            })),
            None,
            "thinking is never the reply"
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "stream_event",
                "event": {"type": "content_block_start", "index": 0},
                "session_id": "sess-1"
            })),
            None
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "sess-1"
            })),
            None
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "PONG"}]}
            })),
            None
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "PONG"
            })),
            None
        );
        assert!(GrokEngine::default().capabilities().streams_text_deltas);
    }
}
