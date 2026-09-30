//! Coding-adapter factory.
//!
//! Stage 5 lets the handler construct a Codex spec when the journaled
//! engine is Codex and the install is Ready. Pi remains the omitted default.

use std::path::PathBuf;

use serde_json::Value;

use super::codex_contract::CodexLaunchProfile;
use super::{
    agy::AgyCliAdapter, claude::ClaudeCodeAdapter, codex::CodexAppServerAdapter,
    grok::GrokAcpAdapter, pi::PiCodingEngineAdapter, selection::engine_str, CodingEngineAdapter,
    CodingEngineKind,
};

#[derive(Debug, Clone)]
pub struct PiCodingOptions {
    pub binary: PathBuf,
}

/// Per-turn Pi options. Kept off the common request fields so a later Codex
/// adapter is not forced to ignore Pi session/model/image flags.
#[derive(Debug, Clone, Default)]
pub struct PiTurnOptions {
    pub session_name: Option<String>,
    pub session_dir: Option<PathBuf>,
    pub resume_recent: bool,
    pub resume_session_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub extension_paths: Vec<PathBuf>,
    pub append_system_prompt: Vec<PathBuf>,
    pub images: Vec<Value>,
    /// Plane runs strip Pi's native resources and use only the explicit
    /// Magician bridge extension. Coding turns keep Pi's normal posture.
    pub plane_harness: bool,
    pub system_prompt_path: Option<PathBuf>,
    /// A Plane extension writes this after its MCP tools are registered.
    /// The RPC driver refuses to prompt if the bridge did not initialize.
    pub ready_marker: Option<PathBuf>,
}

impl PiTurnOptions {
    pub fn is_resuming(&self) -> bool {
        self.resume_recent
            || self
                .resume_session_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
    }
}

impl Default for PiCodingOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("pi"),
        }
    }
}

/// Per-turn Codex options. Kept off the common Pi fields so the Pi adapter
/// never has to ignore them.
#[derive(Debug, Clone, Default)]
pub struct CodexTurnOptions {
    pub resume_thread_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: CodexTurnMode,
    /// Feature posture of the spawned app-server. Defaults to the coding
    /// contract; only the plane harness selects its own profile.
    pub launch_profile: CodexLaunchProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexTurnMode {
    Discuss,
    #[default]
    Build,
    Autopilot,
}

impl CodexTurnMode {
    /// The `thread/start` sandbox argument: a bare kebab-case string.
    ///
    /// Deliberately separate from [`Self::turn_sandbox_policy`]. The app-server
    /// spells the same concept two different ways depending on the method, and
    /// the spellings are not interchangeable — the other one is rejected here as
    /// an unknown variant. A single accessor feeding both call sites is what
    /// let that mismatch ship, so the split is the guard.
    pub fn thread_sandbox(self) -> &'static str {
        match self {
            Self::Discuss => "read-only",
            Self::Build | Self::Autopilot => "workspace-write",
        }
    }

    /// The `turn/start` sandbox policy: an internally tagged enum, not a
    /// string. A bare string is rejected on deserialization, so the tag is
    /// built here rather than left for the call site to remember.
    pub fn turn_sandbox_policy(self) -> Value {
        serde_json::json!({ "type": self.turn_sandbox_tag() })
    }

    /// The camelCase variant name carried inside [`Self::turn_sandbox_policy`].
    /// Private: it is only ever correct inside that tag.
    fn turn_sandbox_tag(self) -> &'static str {
        match self {
            Self::Discuss => "readOnly",
            Self::Build | Self::Autopilot => "workspaceWrite",
        }
    }

    pub fn is_mutation_capable(self) -> bool {
        !matches!(self, Self::Discuss)
    }
}

#[derive(Debug, Clone)]
pub struct CodexCodingOptions {
    pub binary: PathBuf,
}

impl Default for CodexCodingOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("codex"),
        }
    }
}

/// Per-turn Grok options. Kept off the common Pi/Codex fields so those
/// adapters never have to ignore them.
#[derive(Debug, Clone, Default)]
pub struct GrokTurnOptions {
    pub resume_session_id: Option<String>,
    pub model: Option<String>,
    pub mode: GrokTurnMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrokTurnMode {
    Discuss,
    #[default]
    Build,
}

impl GrokTurnMode {
    pub fn sandbox_flag(self) -> &'static str {
        match self {
            Self::Discuss => "read-only",
            Self::Build => "workspace",
        }
    }

    pub fn is_mutation_capable(self) -> bool {
        !matches!(self, Self::Discuss)
    }
}

#[derive(Debug, Clone)]
pub struct GrokCodingOptions {
    pub binary: PathBuf,
}

impl Default for GrokCodingOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("grok"),
        }
    }
}

/// Per-turn Claude Code options. Kept off Pi/Codex/Grok/Agy fields.
#[derive(Debug, Clone, Default)]
pub struct ClaudeTurnOptions {
    pub resume_session_id: Option<String>,
    pub mode: ClaudeTurnMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeTurnMode {
    Discuss,
    #[default]
    Build,
}

impl ClaudeTurnMode {
    pub fn permission_mode(self) -> &'static str {
        match self {
            Self::Discuss => "plan",
            Self::Build => "bypassPermissions",
        }
    }

    pub fn includes_skip_permissions(self) -> bool {
        matches!(self, Self::Build)
    }

    pub fn is_mutation_capable(self) -> bool {
        !matches!(self, Self::Discuss)
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCodingOptions {
    pub binary: PathBuf,
    pub use_api_key: bool,
}

impl Default for ClaudeCodingOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("claude"),
            use_api_key: false,
        }
    }
}

/// Per-turn Agy options. Kept off Pi/Codex/Grok/Claude fields.
#[derive(Debug, Clone, Default)]
pub struct AgyTurnOptions {
    pub resume_session_id: Option<String>,
    pub mode: AgyTurnMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgyTurnMode {
    Discuss,
    #[default]
    Build,
}

impl AgyTurnMode {
    pub fn includes_skip_permissions(self) -> bool {
        matches!(self, Self::Build)
    }

    pub fn is_mutation_capable(self) -> bool {
        !matches!(self, Self::Discuss)
    }
}

#[derive(Debug, Clone)]
pub struct AgyCodingOptions {
    pub binary: PathBuf,
    pub use_api_key: bool,
}

impl Default for AgyCodingOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("agy"),
            use_api_key: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CodingAdapterSpec {
    Pi(PiCodingOptions),
    Codex(CodexCodingOptions),
    Grok(GrokCodingOptions),
    Claude(ClaudeCodingOptions),
    Agy(AgyCodingOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAdapterFactoryError {
    Unconstructable { engine: CodingEngineKind },
}

impl std::fmt::Display for CodingAdapterFactoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconstructable { engine } => write!(
                f,
                "coding engine `{}` is not constructable from this factory",
                engine_str(*engine)
            ),
        }
    }
}

impl std::error::Error for CodingAdapterFactoryError {}

/// Build the adapter for a resolved engine. Engine and spec must agree.
pub fn construct_coding_adapter(
    engine: CodingEngineKind,
    spec: CodingAdapterSpec,
) -> Result<Box<dyn CodingEngineAdapter>, CodingAdapterFactoryError> {
    match (engine, spec) {
        (CodingEngineKind::Pi, CodingAdapterSpec::Pi(options)) => {
            Ok(Box::new(PiCodingEngineAdapter::from_options(options)))
        },
        (CodingEngineKind::CodexAppServer, CodingAdapterSpec::Codex(options)) => {
            Ok(Box::new(CodexAppServerAdapter::from_options(options)))
        },
        (CodingEngineKind::GrokAcp, CodingAdapterSpec::Grok(options)) => {
            Ok(Box::new(GrokAcpAdapter::from_options(options)))
        },
        (CodingEngineKind::ClaudeCode, CodingAdapterSpec::Claude(options)) => {
            Ok(Box::new(ClaudeCodeAdapter::from_options(options)))
        },
        (CodingEngineKind::AgyCli, CodingAdapterSpec::Agy(options)) => {
            Ok(Box::new(AgyCliAdapter::from_options(options)))
        },
        (engine, _) => Err(CodingAdapterFactoryError::Unconstructable { engine }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn factory_constructs_pi_and_reports_the_pi_engine() {
        let adapter = construct_coding_adapter(
            CodingEngineKind::Pi,
            CodingAdapterSpec::Pi(PiCodingOptions {
                binary: PathBuf::from("/opt/pi"),
            }),
        )
        .expect("pi");
        assert_eq!(adapter.engine(), CodingEngineKind::Pi);
    }

    #[test]
    fn factory_constructs_codex_for_the_harness() {
        let adapter = construct_coding_adapter(
            CodingEngineKind::CodexAppServer,
            CodingAdapterSpec::Codex(CodexCodingOptions {
                binary: PathBuf::from("/opt/codex"),
            }),
        )
        .expect("codex");
        assert_eq!(adapter.engine(), CodingEngineKind::CodexAppServer);
    }

    #[test]
    fn factory_constructs_grok_when_kind_and_spec_agree() {
        let adapter = construct_coding_adapter(
            CodingEngineKind::GrokAcp,
            CodingAdapterSpec::Grok(GrokCodingOptions {
                binary: PathBuf::from("/opt/grok"),
            }),
        )
        .expect("grok");
        assert_eq!(adapter.engine(), CodingEngineKind::GrokAcp);
    }

    #[test]
    fn factory_refuses_codex_when_handed_pi_options() {
        let err = construct_coding_adapter(
            CodingEngineKind::CodexAppServer,
            CodingAdapterSpec::Pi(PiCodingOptions::default()),
        )
        .expect_err("codex");
        assert_eq!(
            err,
            CodingAdapterFactoryError::Unconstructable {
                engine: CodingEngineKind::CodexAppServer
            }
        );
        assert!(
            !err.to_string().contains("/opt"),
            "refusal must not depend on a caller binary path: {err}"
        );
    }

    #[test]
    fn factory_refuses_pi_when_handed_codex_options() {
        let err = construct_coding_adapter(
            CodingEngineKind::Pi,
            CodingAdapterSpec::Codex(CodexCodingOptions::default()),
        )
        .expect_err("pi");
        assert_eq!(
            err,
            CodingAdapterFactoryError::Unconstructable {
                engine: CodingEngineKind::Pi
            }
        );
    }

    #[test]
    fn factory_refuses_grok_when_handed_pi_options() {
        let err = construct_coding_adapter(
            CodingEngineKind::GrokAcp,
            CodingAdapterSpec::Pi(PiCodingOptions {
                binary: PathBuf::from("/opt/grok"),
            }),
        )
        .expect_err("grok");
        assert_eq!(
            err,
            CodingAdapterFactoryError::Unconstructable {
                engine: CodingEngineKind::GrokAcp
            }
        );
        assert!(
            !err.to_string().contains("/opt"),
            "refusal must not depend on a caller binary path: {err}"
        );
    }

    #[test]
    fn factory_constructs_claude_when_kind_and_spec_agree() {
        let adapter = construct_coding_adapter(
            CodingEngineKind::ClaudeCode,
            CodingAdapterSpec::Claude(ClaudeCodingOptions {
                binary: PathBuf::from("/opt/claude"),
                use_api_key: false,
            }),
        )
        .expect("claude");
        assert_eq!(adapter.engine(), CodingEngineKind::ClaudeCode);
    }

    #[test]
    fn factory_refuses_claude_when_handed_pi_options() {
        let err = construct_coding_adapter(
            CodingEngineKind::ClaudeCode,
            CodingAdapterSpec::Pi(PiCodingOptions {
                binary: PathBuf::from("/opt/claude"),
            }),
        )
        .expect_err("claude");
        assert_eq!(
            err,
            CodingAdapterFactoryError::Unconstructable {
                engine: CodingEngineKind::ClaudeCode
            }
        );
        assert!(
            !err.to_string().contains("/opt"),
            "refusal must not depend on a caller binary path: {err}"
        );
    }

    #[test]
    fn factory_constructs_agy_when_kind_and_spec_agree() {
        let adapter = construct_coding_adapter(
            CodingEngineKind::AgyCli,
            CodingAdapterSpec::Agy(AgyCodingOptions {
                binary: PathBuf::from("/opt/agy"),
                use_api_key: false,
            }),
        )
        .expect("agy");
        assert_eq!(adapter.engine(), CodingEngineKind::AgyCli);
    }

    #[test]
    fn factory_refuses_agy_when_handed_pi_options() {
        let err = construct_coding_adapter(
            CodingEngineKind::AgyCli,
            CodingAdapterSpec::Pi(PiCodingOptions {
                binary: PathBuf::from("/opt/agy"),
            }),
        )
        .expect_err("agy");
        assert_eq!(
            err,
            CodingAdapterFactoryError::Unconstructable {
                engine: CodingEngineKind::AgyCli
            }
        );
        assert!(
            !err.to_string().contains("/opt"),
            "refusal must not depend on a caller binary path: {err}"
        );
    }

    #[test]
    fn production_handler_constructs_only_through_the_factory() {
        let source = include_str!("../compiled_handlers/run_coding_task.rs");
        assert!(
            source.contains("construct_coding_adapter("),
            "run_coding_task must construct adapters through the factory"
        );
        assert!(
            !source.contains("PiCodingEngineAdapter::new"),
            "run_coding_task must not construct Pi directly"
        );
        assert!(
            !source.contains("CodexAppServerAdapter"),
            "run_coding_task must not name the Codex adapter"
        );
        assert!(
            source.contains("adapter_spec_for_engine("),
            "run_coding_task must pick Pi vs Codex vs Grok vs Claude spec from the journaled engine"
        );
        assert!(
            source.contains("CodingAdapterSpec::Codex"),
            "run_coding_task must be able to construct a Codex spec"
        );
        assert!(
            source.contains("CodingAdapterSpec::Grok"),
            "run_coding_task must be able to construct a Grok spec"
        );
        assert!(
            source.contains("CodingAdapterSpec::Claude"),
            "run_coding_task must be able to construct a Claude spec"
        );
        assert!(
            source.contains("CodingAdapterSpec::Agy"),
            "run_coding_task must be able to construct an Agy spec"
        );
        assert!(
            !source.contains("GrokAcpAdapter"),
            "run_coding_task must not name the Grok adapter"
        );
        assert!(
            !source.contains("ClaudeCodeAdapter"),
            "run_coding_task must not name the Claude adapter"
        );
        assert!(
            !source.contains("AgyCliAdapter"),
            "run_coding_task must not name the Agy adapter"
        );
        assert!(
            source.contains("attach_staged_coding_proposal("),
            "run_coding_task must stage proposals after the adapter turn"
        );
        assert!(
            source.contains("result.event_count"),
            "handler must use the result event count, not a second event vector"
        );
        assert!(
            !source.contains("result.events.len()"),
            "handler must not read a result-side event vector"
        );
        assert!(
            source.contains("emit_coding_event("),
            "handler must project events through the engine-neutral sink name"
        );
        assert!(
            source.contains("\"coding.agent_started\""),
            "serialized coding.* event names must stay compatible"
        );
        assert!(
            !source.contains("fn emit_pi_event"),
            "Pi-only internal emitter name must be gone"
        );
    }

    /// Everything before the file's test module.
    ///
    /// Split on the ATTRIBUTE's opening rather than on `#[cfg(test)]` exactly:
    /// the crate split (`7a7e78ca0`) rewrote these modules to
    /// `#[cfg(any(test, feature = "test-fixtures"))]`, after which the old
    /// literal matched nothing, the whole file counted as production, and every
    /// assertion below tripped over the test module's own strings — a guard
    /// that fails for the one reason it is not looking for.
    fn production_source(source: &str) -> &str {
        source
            .find("\n#[cfg(test)]")
            .into_iter()
            .chain(source.find("\n#[cfg(any(test"))
            .min()
            .map(|at| &source[..at])
            .unwrap_or(source)
    }

    #[test]
    fn pi_adapter_does_not_stage_proposals() {
        let source = production_source(include_str!("pi.rs"));
        assert!(
            !source.contains("stage_shadow_workspace_patch"),
            "Pi must not stage proposals; the common handler owns that"
        );
        assert!(
            !source.contains("attach_staged_coding_proposal"),
            "Pi must not call the common staging helper"
        );
        assert!(
            !source.contains("request.codex"),
            "Pi must not read Codex turn options"
        );
    }

    #[test]
    fn codex_adapter_does_not_stage_proposals() {
        let source = production_source(include_str!("codex.rs"));
        assert!(
            !source.contains("stage_shadow_workspace_patch"),
            "Codex must not stage proposals; the common handler owns that"
        );
        assert!(
            !source.contains("attach_staged_coding_proposal("),
            "Codex must not call the common staging helper"
        );
        assert!(
            !source.contains("request.pi"),
            "Codex must not read Pi turn options"
        );
    }

    #[test]
    fn grok_adapter_does_not_stage_proposals() {
        let source = production_source(include_str!("grok.rs"));
        assert!(
            !source.contains("stage_shadow_workspace_patch"),
            "Grok must not stage proposals; the common handler owns that"
        );
        assert!(
            !source.contains("attach_staged_coding_proposal("),
            "Grok must not call the common staging helper"
        );
        assert!(
            !source.contains("request.pi"),
            "Grok must not read Pi turn options"
        );
    }

    #[test]
    fn claude_adapter_does_not_stage_proposals() {
        let source = production_source(include_str!("claude.rs"));
        assert!(
            !source.contains("stage_shadow_workspace_patch"),
            "Claude must not stage proposals; the common handler owns that"
        );
        assert!(
            !source.contains("attach_staged_coding_proposal("),
            "Claude must not call the common staging helper"
        );
        assert!(
            !source.contains("request.pi"),
            "Claude must not read Pi turn options"
        );
        assert!(
            !source.contains("request.codex"),
            "Claude must not read Codex turn options"
        );
        assert!(
            !source.contains("request.grok"),
            "Claude must not read Grok turn options"
        );
        assert!(
            !source.contains("request.agy"),
            "Claude must not read Agy turn options"
        );
    }

    #[test]
    fn codex_mode_sandbox_parity() {
        // `thread/start` takes the kebab spelling as a bare string.
        assert_eq!(CodexTurnMode::Discuss.thread_sandbox(), "read-only");
        assert_eq!(CodexTurnMode::Build.thread_sandbox(), "workspace-write");
        assert_eq!(CodexTurnMode::Autopilot.thread_sandbox(), "workspace-write");
        // `turn/start` takes a tagged object carrying the camel spelling. The
        // two must not converge back onto one value.
        assert_eq!(
            CodexTurnMode::Discuss.turn_sandbox_policy(),
            serde_json::json!({ "type": "readOnly" })
        );
        assert_eq!(
            CodexTurnMode::Build.turn_sandbox_policy(),
            serde_json::json!({ "type": "workspaceWrite" })
        );
        assert_eq!(
            CodexTurnMode::Autopilot.turn_sandbox_policy(),
            serde_json::json!({ "type": "workspaceWrite" })
        );
        assert!(
            CodexTurnMode::Build.turn_sandbox_policy() != serde_json::json!("workspace-write"),
            "the turn policy must stay a tagged object, never a bare string"
        );
        assert!(!CodexTurnMode::Discuss.is_mutation_capable());
        assert!(CodexTurnMode::Build.is_mutation_capable());
        assert!(CodexTurnMode::Autopilot.is_mutation_capable());
    }

    #[test]
    fn grok_mode_sandbox_parity() {
        assert_eq!(GrokTurnMode::Discuss.sandbox_flag(), "read-only");
        assert_eq!(GrokTurnMode::Build.sandbox_flag(), "workspace");
        assert!(!GrokTurnMode::Discuss.is_mutation_capable());
        assert!(GrokTurnMode::Build.is_mutation_capable());
    }

    #[test]
    fn claude_mode_permission_parity() {
        assert_eq!(ClaudeTurnMode::Build.permission_mode(), "bypassPermissions");
        assert_eq!(ClaudeTurnMode::Discuss.permission_mode(), "plan");
        assert!(ClaudeTurnMode::Build.includes_skip_permissions());
        assert!(!ClaudeTurnMode::Discuss.includes_skip_permissions());
        assert!(ClaudeTurnMode::Build.is_mutation_capable());
        assert!(!ClaudeTurnMode::Discuss.is_mutation_capable());
    }

    #[test]
    fn resume_flags_are_pi_turn_options() {
        let idle = PiTurnOptions::default();
        assert!(!idle.is_resuming());
        assert!(PiTurnOptions {
            resume_recent: true,
            ..PiTurnOptions::default()
        }
        .is_resuming());
        assert!(PiTurnOptions {
            resume_session_id: Some("sess-1".to_string()),
            ..PiTurnOptions::default()
        }
        .is_resuming());
        assert!(!PiTurnOptions {
            resume_session_id: Some(String::new()),
            ..PiTurnOptions::default()
        }
        .is_resuming());
    }
}
