//! Antigravity's one-shot CLI with a private workspace MCP registration.
//!
//! The operator's login remains in their own home. Each plane session runs in
//! its private workspace; its grant never mutates Agy's global MCP config.
//! Proposal-only sessions load an engine-owned custom primary-agent prompt and
//! stop at the first complete JSON assistant response, before the CLI can loop
//! into native work. Native tools remain sandboxed, not claimed stripped.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::magician_v2::execution::plane::engine::{
    HarnessCapabilities, HarnessEngine, HarnessError, HarnessSession, HarnessSessionRequest,
};
use crate::magician_v2::execution::plane::engines::oneshot::{
    inherit_env, write_private, HomeInstall, OneShotEngineSpec, OneShotSession,
};

const SERVER_NAME: &str = "magician_plane";

/// The prompt sits at index 2 (the spawner's model flag goes in before the
/// last element, a flag); a warm turn adds `--conversation <id>` right
/// after it.
fn argv_for_turn(text: &str, resume: Option<&str>) -> Vec<String> {
    let mut argv = vec!["agy".to_string(), "-p".to_string(), text.to_string()];
    if let Some(id) = resume {
        argv.push("--conversation".to_string());
        argv.push(id.to_string());
    }
    argv.extend(
        [
            "--output-format",
            "stream-json",
            "--sandbox",
            "--disable-slash-commands",
            "--dangerously-skip-permissions",
            // `0` waits until the turn completes. Without it agy applied its own
            // five-minute print limit, and its response to hitting one is to exit
            // 0 with partial output and no final answer — a turn that did the
            // whole job (26 driver actions, the document written and saved) then
            // settled empty, which the loop must read as a failure. Magician's
            // own bounds are the ones that should end a turn: the wall-clock
            // ceiling and the idle watchdog, both of which terminate the process
            // group and say which bound fired.
            "--print-timeout",
            "0",
        ]
        .map(str::to_string),
    );
    argv
}

/// The conversation id from the first line `agy -p` prints.
fn native_session_id_of(value: &Value) -> Option<String> {
    (value.get("event").and_then(Value::as_str) == Some("init"))
        .then(|| {
            value
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

/// A reply text delta from a `step_update` line: the `agent_response`
/// step's `text_delta`, whatever the step's state — the DONE update can
/// carry a trailing delta that is part of the text (probe-verified
/// 2026-09-13). Other step types (the user's own input) and the `result`
/// line, whose `response` repeats the reply whole, carry none.
fn text_delta_of(value: &Value) -> Option<String> {
    if value.get("event").and_then(Value::as_str) != Some("step_update") {
        return None;
    }
    let step_type = value
        .pointer("/step_update/step_type")
        .and_then(Value::as_str);
    if step_type != Some("agent_response") {
        return None;
    }
    value
        .pointer("/step_update/text_delta")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Workspace MCP is read by headless turns even though `agy mcp list`
/// only lists global registrations (live-probed with the installed CLI).
fn install_config(install: &HomeInstall) -> std::io::Result<()> {
    let dir = install.home.join(".agents");
    std::fs::create_dir_all(&dir)?;
    let config = serde_json::json!({"mcpServers":{SERVER_NAME:{
        "serverUrl":install.endpoint_url,
        "headers":{"Authorization":format!("Bearer {}",install.grant)}
    }}});
    write_private(&dir.join("mcp_config.json"), config.to_string().as_bytes())?;
    if let Some(system) = &install.planner_system {
        let path = dir.join("agents/magician-planner/agent.md");
        std::fs::create_dir_all(path.parent().unwrap())?;
        // Quote `off`: YAML 1.1 otherwise treats it as a boolean. The CLI
        // requires the H1 system-prompt section, rather than a bare body.
        let prompt = format!(
            "---\nname: magician-planner\ndescription: Proposes JSON for the Magician decision engine\nmainAgent: true\nsubagent: false\ncommandExecutionPolicy: \"off\"\n---\n# System Prompt\n{system}\n"
        );
        write_private(&path, prompt.as_bytes())?;
    }
    Ok(())
}

fn remove_config(install: &HomeInstall) {
    // Persistent conversation homes outlive this grant. Remove its credential
    // immediately; the caller owns the rest of that workspace's lifetime.
    let _ = std::fs::remove_file(install.home.join(".agents/mcp_config.json"));
}

fn planner_response_boundary(value: &Value) -> Option<(u64, bool)> {
    let step = value.get("step_update")?;
    if value["event"] != "step_update" || step["step_type"] != "agent_response" {
        return None;
    }
    Some((step["step_index"].as_u64()?, step["state"] == "DONE"))
}

#[derive(Debug, Clone)]
pub struct AgyEngine {
    /// Tests point this at a fake speaking the same CLI shape.
    pub binary: PathBuf,
}

impl Default for AgyEngine {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("agy"),
        }
    }
}

#[async_trait]
impl HarnessEngine for AgyEngine {
    fn name(&self) -> &'static str {
        "agy"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            // `--conversation <id>`; the conversation lives in the
            // operator's home, the persistent plane home is its cwd.
            supports_resume: true,
            tools_list_changed: true,
            // `stream-json` emits a `step_update` per reply chunk.
            streams_text_deltas: true,
            native_tool_posture:
                crate::magician_v2::execution::plane::engine::NativeToolPosture::Sandboxed,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let planning_only = req.planning_only;
        let spec = OneShotEngineSpec {
            binary: self.binary.clone(),
            argv_for_turn: Box::new(move |text, resume| {
                let mut argv = argv_for_turn(text, resume);
                if planning_only {
                    argv.retain(|arg| arg != "--dangerously-skip-permissions");
                    argv.extend(["--agent".to_string(), "magician-planner".to_string()]);
                    argv.extend([
                        "--json-schema".to_string(),
                        decision_engine_contract::action::planner_reply_schema().to_string(),
                    ]);
                }
                argv
            }),
            native_session_id_of,
            text_delta_of,
            env_for: |_| {
                inherit_env(&[
                    "GOOGLE_API_KEY",
                    "GEMINI_API_KEY",
                    "GOOGLE_CLOUD_PROJECT",
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    "VERTEX_LOCATION",
                    "CLOUD_ML_PROJECT",
                ])
            },
            install_config,
            remove_config,
            use_isolated_home_as_cwd: true,
            planner_system_supplied: true,
            planner_response_boundary: Some(planner_response_boundary),
        };
        Ok(Box::new(OneShotSession::new(spec, req)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::engine::NativeToolPosture;

    #[test]
    fn decision_planner_agy_config_is_per_session_and_cleans_its_grant() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (home, token) in [(first.path(), "grant-one"), (second.path(), "grant-two")] {
            let install = HomeInstall {
                home: home.into(),
                cwd: home.into(),
                endpoint_url: "http://localhost/plane".into(),
                grant: token.into(),
                planner_system: Some("Propose only.".into()),
            };
            install_config(&install).unwrap();
            let config: Value = serde_json::from_slice(
                &std::fs::read(home.join(".agents/mcp_config.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                config["mcpServers"][SERVER_NAME]["headers"]["Authorization"],
                format!("Bearer {token}")
            );
            let agent =
                std::fs::read_to_string(home.join(".agents/agents/magician-planner/agent.md"))
                    .unwrap();
            assert!(agent.contains("# System Prompt\nPropose only."));
            remove_config(&install);
            assert!(!home.join(".agents/mcp_config.json").exists());
        }
    }

    #[test]
    fn sandbox_and_no_slash_commands_are_the_strip_compromise() {
        let argv = argv_for_turn("hello", None);
        assert!(argv.windows(2).any(|w| w[0] == "-p" && w[1] == "hello"));
        assert!(argv.iter().any(|a| a == "--sandbox"));
        assert!(argv.iter().any(|a| a == "--disable-slash-commands"));
        assert!(argv.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert_eq!(
            AgyEngine::default().capabilities().native_tool_posture,
            NativeToolPosture::Sandboxed
        );
    }

    /// agy's own print limit must never be the bound that ends a turn: it
    /// exits 0 with partial output and no final answer, so a turn that did the
    /// whole job settles empty and the loop can only read that as a failure.
    /// `--print-timeout 0` waits for the turn; Magician's ceiling and idle
    /// watchdog are the bounds that end it, and they say so when they do.
    #[test]
    fn the_print_timeout_is_ours_not_agys() {
        let argv = argv_for_turn("hello", None);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--print-timeout" && w[1] == "0"),
            "{argv:?}"
        );
    }

    /// A warm turn adds `--conversation <id>` right after the prompt; a
    /// cold turn carries no such flag. The prompt stays at index 2.
    #[test]
    fn warm_argv_resumes_the_native_session() {
        let cold = argv_for_turn("hello", None);
        assert_eq!(&cold[..3], ["agy", "-p", "hello"]);
        assert!(!cold.iter().any(|a| a == "--conversation"));

        let warm = argv_for_turn("hello", Some("conv-1"));
        assert_eq!(
            &warm[..5],
            ["agy", "-p", "hello", "--conversation", "conv-1"]
        );
        assert_eq!(&warm[5..], &cold[3..], "the rest is the cold turn's");
        assert!(AgyEngine::default().capabilities().supports_resume);
    }

    #[test]
    fn native_session_id_is_read_from_the_init_line() {
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "event": "init",
                "conversation_id": "conv-1"
            }))
            .as_deref(),
            Some("conv-1")
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "event": "result",
                "result": {"status": "SUCCESS", "response": "hi"}
            })),
            None
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({"event": "init"})),
            None
        );
    }

    /// Every `agent_response` step delta is streamed, the DONE update's
    /// trailing one included; the user's own input step and the `result`
    /// line that repeats the reply whole are not.
    #[test]
    fn agy_agent_response_deltas_are_streamed() {
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "step_update",
                "step_update": {
                    "conversation_id": "conv-1",
                    "step_index": 1,
                    "state": "ACTIVE",
                    "step_type": "agent_response",
                    "text_delta": "ok"
                }
            }))
            .as_deref(),
            Some("ok")
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "step_update",
                "step_update": {
                    "conversation_id": "conv-1",
                    "step_index": 1,
                    "state": "DONE",
                    "step_type": "agent_response",
                    "text_delta": "\n"
                }
            }))
            .as_deref(),
            Some("\n"),
            "the DONE update's trailing delta is part of the text"
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "step_update",
                "step_update": {
                    "conversation_id": "conv-1",
                    "step_index": 0,
                    "state": "DONE",
                    "step_type": "user_input",
                    "text_delta": "ping"
                }
            })),
            None,
            "the user's own input is never the reply"
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "step_update",
                "step_update": {
                    "conversation_id": "conv-1",
                    "step_index": 1,
                    "state": "ACTIVE",
                    "step_type": "agent_response"
                }
            })),
            None,
            "a step update without text is no delta"
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "result",
                "result": {"status": "SUCCESS", "response": "ok\n"}
            })),
            None
        );
        assert_eq!(
            text_delta_of(&serde_json::json!({
                "event": "init",
                "conversation_id": "conv-1"
            })),
            None
        );
        assert!(AgyEngine::default().capabilities().streams_text_deltas);
    }
}
