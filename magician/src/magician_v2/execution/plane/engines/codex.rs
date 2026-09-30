//! Codex as a plane harness — the `codex exec` surface (plane plan Task 1b).
//!
//! Inject is verified: an isolated `CODEX_HOME` whose `config.toml` points
//! at the plane, with the grant supplied through an environment variable so
//! it never lands in a file the CLI rewrites or in argv. `codex exec` is
//! one-shot — one process per turn — and the CLI persists each thread in
//! its `CODEX_HOME`: the first stdout line names it (`thread.started`), the
//! session reports that id when its home outlives it, and the next turn
//! resumes it with `codex exec resume <thread id>`. Native-tool strip is a
//! **sandbox + feature disable**, not an empty registry:
//! `sandbox_mode="read-only"` (a config override, since the `resume`
//! subcommand has no `--sandbox` flag and the two argv shapes must not
//! drift), `--disable shell_tool`, `--disable unified_exec`,
//! `web_search=disabled`, `approval_policy=never`. Native apply_patch/read
//! may still be offered and then blocked by the sandbox. Plane MCP remains
//! the governed hands.
//!
//! The coding contract's default-on features are off too, with one
//! exception the plane needs: current Codex reaches MCP tools only through
//! its code-mode host, so the `code_mode`/`code_mode_host` pair stays on
//! (`CodexLaunchProfile::PlaneHarness`). The plane server's tools are
//! pre-approved in the isolated config because the plane door is the
//! approval authority — a Codex-side prompt under `approval_policy=never`
//! would only auto-deny a call the door was about to govern. Both Codex
//! plane engines render the one `config.toml` in this module.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::magician_v2::execution::coding_engine::codex_contract::{
    features_off_for, CodexLaunchProfile,
};
use crate::magician_v2::execution::plane::engine::{
    HarnessCapabilities, HarnessEngine, HarnessError, HarnessSession, HarnessSessionRequest,
};
use crate::magician_v2::execution::plane::engines::oneshot::{
    inherit_env, operator_cli_home, require_cli_auth, seed_named_file, write_private, HomeInstall,
    OneShotEngineSpec, OneShotSession,
};

pub(super) const GRANT_ENV_VAR: &str = "MAGICIAN_PLANE_GRANT";

/// Cold: `codex exec [OPTIONS] <prompt>`. Warm: `codex exec resume
/// [OPTIONS] <thread id> <prompt>` — the same options, then the id right
/// before the prompt. The prompt is the last element either way (the
/// spawner's model flag goes in before it, between the two positionals on
/// a warm turn — the probe-verified shape).
fn argv_for_turn(text: &str, resume: Option<&str>) -> Vec<String> {
    let mut argv = vec!["codex".to_string(), "exec".to_string()];
    if resume.is_some() {
        argv.push("resume".to_string());
    }
    argv.extend(
        [
            "--json",
            "--skip-git-repo-check",
            "-c",
            "sandbox_mode=\"read-only\"",
            "--disable",
            "shell_tool",
            "--disable",
            "unified_exec",
        ]
        .map(str::to_string),
    );
    for feature in features_off_for(CodexLaunchProfile::PlaneHarness) {
        argv.push("--disable".to_string());
        argv.push(feature.to_string());
    }
    argv.extend(["-c", "web_search=disabled", "-c", "approval_policy=never"].map(str::to_string));
    if let Some(id) = resume {
        argv.push(id.to_string());
    }
    argv.push(text.to_string());
    argv
}

/// The thread id from the first line `codex exec --json` prints.
fn native_session_id_of(value: &Value) -> Option<String> {
    (value.get("type").and_then(Value::as_str) == Some("thread.started"))
        .then(|| {
            value
                .get("thread_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

fn install_config(install: &HomeInstall) -> std::io::Result<()> {
    // Probe-verified shape (Task 1b): streamable-HTTP MCP with the token
    // read from the environment, inside an isolated CODEX_HOME.
    // Native-tool posture is sandboxed: no shell tool, read-only FS,
    // web search off, never-prompt so the CLI cannot escalate.
    // Auth lives in the operator CODEX_HOME; copy only auth.json so the
    // isolated home can sign in without inheriting the operator config.
    require_cli_auth("CODEX_HOME", ".codex", "OPENAI_API_KEY", "codex")?;
    seed_named_file(
        &operator_cli_home("CODEX_HOME", ".codex"),
        &install.home,
        "auth.json",
    )?;
    let config = plane_config_toml(&install.endpoint_url);
    write_private(&install.home.join("config.toml"), config.as_bytes())
}

/// The isolated-home `config.toml` both Codex plane engines write — one
/// renderer so the exec and app-server postures cannot drift. Read-only
/// sandbox, shell/unified_exec off, every coding-contract feature off
/// except the code-mode pair, and the plane server's tools pre-approved:
/// the plane door decides every governed call, so Codex must not ask a
/// second time (it could only auto-deny under `approval_policy = "never"`).
pub(super) fn plane_config_toml(endpoint_url: &str) -> String {
    let mut features = String::from("shell_tool = false\nunified_exec = false\n");
    for feature in features_off_for(CodexLaunchProfile::PlaneHarness) {
        features.push_str(feature);
        features.push_str(" = false\n");
    }
    format!(
        "sandbox_mode = \"read-only\"\n\
         web_search = \"disabled\"\n\
         approval_policy = \"never\"\n\
         [features]\n\
         {features}\
         [mcp_servers.magician_plane]\n\
         url = \"{url}\"\n\
         bearer_token_env_var = \"{GRANT_ENV_VAR}\"\n\
         default_tools_approval_mode = \"approve\"\n",
        url = endpoint_url
    )
}

fn remove_config(_install: &HomeInstall) {
    // Nothing global to undo: the config lives in the isolated CODEX_HOME
    // (the session's own temp dir, or the conversation's persistent home,
    // re-installed per session).
}

#[derive(Debug, Clone)]
pub struct CodexExecEngine {
    /// Tests point this at a fake speaking the same CLI shape.
    pub binary: PathBuf,
}

impl Default for CodexExecEngine {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("codex"),
        }
    }
}

#[async_trait]
impl HarnessEngine for CodexExecEngine {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            // `codex exec resume` in a persistent CODEX_HOME.
            supports_resume: true,
            tools_list_changed: false,
            // `codex exec --json` emits completed items only
            // (probe-verified 2026-09-13): the reply arrives as one
            // `item.completed` line, so this engine cannot stream.
            streams_text_deltas: false,
            native_tool_posture:
                crate::magician_v2::execution::plane::engine::NativeToolPosture::Sandboxed,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let spec = OneShotEngineSpec {
            binary: self.binary.clone(),
            argv_for_turn: Box::new(argv_for_turn),
            native_session_id_of,
            // `codex exec --json` emits completed items only (probe-verified
            // 2026-09-13): there is no delta line to read.
            text_delta_of: |_| None,
            env_for: |install| {
                let mut env = vec![
                    // An isolated CODEX_HOME: the config never touches the
                    // operator's, and the grant rides the environment into
                    // the config's bearer_token_env_var — never argv.
                    ("CODEX_HOME".to_string(), install.home.display().to_string()),
                    (GRANT_ENV_VAR.to_string(), install.grant.clone()),
                ];
                env.extend(inherit_env(&["OPENAI_API_KEY"]));
                env
            },
            install_config,
            remove_config,
            use_isolated_home_as_cwd: true,
            planner_system_supplied: false,
            planner_response_boundary: None,
        };
        Ok(Box::new(OneShotSession::new(spec, req)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::coding_engine::codex_contract::{
        CODEX_CODE_MODE_FEATURES, CODEX_FEATURES_MUST_BE_OFF,
    };
    use crate::magician_v2::execution::plane::engine::NativeToolPosture;

    #[test]
    fn sandbox_and_feature_disables_are_on_argv() {
        let argv = argv_for_turn("hello", None);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-c" && w[1] == "sandbox_mode=\"read-only\""),
            "the sandbox rides a config override: `resume` has no --sandbox flag"
        );
        assert!(!argv.iter().any(|a| a == "--sandbox"));
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--disable" && w[1] == "shell_tool"));
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--disable" && w[1] == "unified_exec"));
        // The code-mode pair stays on: it is Codex's only path to MCP tools.
        for feature in CODEX_CODE_MODE_FEATURES {
            assert!(
                !argv
                    .windows(2)
                    .any(|w| w[0] == "--disable" && w[1] == *feature),
                "{feature} must not be disabled on argv"
            );
        }
        for feature in CODEX_FEATURES_MUST_BE_OFF
            .iter()
            .filter(|feature| !CODEX_CODE_MODE_FEATURES.contains(feature))
        {
            assert!(
                argv.windows(2)
                    .any(|w| w[0] == "--disable" && w[1] == *feature),
                "{feature} must be disabled on argv"
            );
        }
        assert!(argv.iter().any(|a| a == "web_search=disabled"));
        assert!(argv.iter().any(|a| a == "approval_policy=never"));
        assert_eq!(
            CodexExecEngine::default()
                .capabilities()
                .native_tool_posture,
            NativeToolPosture::Sandboxed
        );
    }

    /// A warm turn is `codex exec resume [OPTIONS] <thread id> <prompt>`:
    /// the same options as a cold turn, then the id right before the prompt
    /// (so the spawner's model flag, inserted before the last element,
    /// lands between the two positionals). A cold turn carries neither.
    #[test]
    fn warm_argv_resumes_the_native_session() {
        let cold = argv_for_turn("hello", None);
        assert_eq!(&cold[..2], ["codex", "exec"]);
        assert_eq!(cold[2], "--json");
        assert!(!cold.iter().any(|a| a == "resume"));
        assert_eq!(cold.last().map(String::as_str), Some("hello"));

        let warm = argv_for_turn("hello", Some("thread-1"));
        assert_eq!(&warm[..3], ["codex", "exec", "resume"]);
        assert_eq!(warm[3], "--json");
        let len = warm.len();
        assert_eq!(&warm[len - 2..], ["thread-1", "hello"]);
        assert_eq!(
            &warm[3..len - 2],
            &cold[2..cold.len() - 1],
            "the options are the cold turn's, so the two shapes cannot drift"
        );
        assert!(CodexExecEngine::default().capabilities().supports_resume);
    }

    #[test]
    fn native_session_id_is_read_from_the_init_line() {
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "type": "thread.started",
                "thread_id": "thread-1"
            }))
            .as_deref(),
            Some("thread-1")
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({
                "type": "item.completed",
                "item": {"id": "i", "type": "agent_message", "text": "hi"}
            })),
            None
        );
        assert_eq!(
            native_session_id_of(&serde_json::json!({"type": "thread.started"})),
            None
        );
    }

    #[test]
    fn isolated_config_keeps_code_mode_on_and_pre_approves_the_plane_server() {
        let config = plane_config_toml("http://127.0.0.1:1/plane/mcp");
        let lines: Vec<&str> = config.lines().collect();
        let features_at = lines
            .iter()
            .position(|line| *line == "[features]")
            .expect("[features] table");
        let plane_at = lines
            .iter()
            .position(|line| *line == "[mcp_servers.magician_plane]")
            .expect("plane server table");
        assert!(features_at < plane_at);
        let features = &lines[features_at + 1..plane_at];
        let plane = &lines[plane_at + 1..];
        for feature in CODEX_CODE_MODE_FEATURES {
            assert!(
                !features.contains(&format!("{feature} = false").as_str()),
                "{feature} must stay on in the isolated config"
            );
        }
        for feature in CODEX_FEATURES_MUST_BE_OFF
            .iter()
            .filter(|feature| !CODEX_CODE_MODE_FEATURES.contains(feature))
        {
            assert!(
                features.contains(&format!("{feature} = false").as_str()),
                "{feature} must be off in the isolated config"
            );
        }
        assert!(features.contains(&"shell_tool = false"));
        assert!(features.contains(&"unified_exec = false"));
        assert!(plane.contains(&"url = \"http://127.0.0.1:1/plane/mcp\""));
        assert!(plane.contains(&"bearer_token_env_var = \"MAGICIAN_PLANE_GRANT\""));
        assert!(plane.contains(&"default_tools_approval_mode = \"approve\""));
        assert!(!plane.iter().any(|line| line.starts_with('[')));
        assert!(lines.contains(&"sandbox_mode = \"read-only\""));
        assert!(lines.contains(&"approval_policy = \"never\""));
        assert!(lines.contains(&"web_search = \"disabled\""));
    }
}
