//! Per-harness engines. Each file is one harness; the slot is the trait in
//! `plane::engine`.

pub mod agy;
pub mod claude_code;
pub mod codex;
pub mod codex_app_server;
pub mod grok;
pub mod oneshot;
pub mod pi;

pub use agy::AgyEngine;
pub use claude_code::ClaudeCodeEngine;
pub use codex::CodexExecEngine;
pub use codex_app_server::CodexAppServerEngine;
pub use grok::GrokEngine;
pub use pi::PiPlaneEngine;

/// The roster of harness engines this build can actually launch (plane plan
/// Task 7: "adding a harness should approach one file"). Selection flows
/// through [`crate::magician_v2::execution::plane::turn_engine::resolve_turn_engine`],
/// which must list exactly the names this factory can build — the roster and
/// the factory cannot drift, or the dropdown offers a name the seam cannot
/// run.
pub fn harness_engine_for(
    name: &str,
) -> Option<std::sync::Arc<dyn crate::magician_v2::execution::plane::engine::HarnessEngine>> {
    match name.trim() {
        "pi" => Some(std::sync::Arc::new(PiPlaneEngine::default())),
        "claude_code" => Some(std::sync::Arc::new(
            crate::magician_v2::execution::plane::engines::ClaudeCodeEngine::default(),
        )),
        "codex" => Some(std::sync::Arc::new(
            crate::magician_v2::execution::plane::engines::CodexExecEngine::default(),
        )),
        "codex_app_server" => Some(std::sync::Arc::new(
            crate::magician_v2::execution::plane::engines::CodexAppServerEngine::default(),
        )),
        "grok" => Some(std::sync::Arc::new(
            crate::magician_v2::execution::plane::engines::GrokEngine::default(),
        )),
        "agy" => Some(std::sync::Arc::new(
            crate::magician_v2::execution::plane::engines::AgyEngine::default(),
        )),
        _ => None,
    }
}

/// The roster with on-this-machine install status, for surfaces that must
/// not offer an engine the operator cannot run (the settings dropdown).
/// Detection is a PATH lookup of the binary — deliberately cheap and
/// side-effect-free; no version probes, no spawns.
pub fn roster_with_install_status() -> Vec<(&'static str, bool)> {
    let binaries: &[(&'static str, &str)] = &[
        ("pi", "pi"),
        ("claude_code", "claude"),
        ("codex", "codex"),
        ("codex_app_server", "codex"),
        ("grok", "grok"),
        ("agy", "agy"),
    ];
    binaries
        .iter()
        .map(|(name, binary)| (*name, binary_on_path(binary)))
        .collect()
}

fn binary_on_path(binary: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(binary);
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::turn_engine::{resolve_turn_engine, TurnEngine};

    /// The roster and the factory cannot drift: every name the roster
    /// resolves to a harness must build here, and every factory arm must be
    /// a roster name — otherwise the dropdown offers a name the seam cannot
    /// run, or an engine exists nothing can select.
    #[test]
    fn the_roster_and_the_factory_cannot_drift() {
        for name in [
            "pi",
            "claude_code",
            "codex",
            "codex_app_server",
            "grok",
            "agy",
        ] {
            assert!(
                matches!(resolve_turn_engine(Some(name)), TurnEngine::Harness(_)),
                "{name} must be on the roster"
            );
            assert!(
                harness_engine_for(name).is_some(),
                "{name} must be buildable by the factory"
            );
            let engine = harness_engine_for(name).unwrap();
            assert_eq!(engine.name(), name, "the engine must know its own name");
        }
        // Unknown names build nothing and resolve to the loop.
        for name in ["opencode", "clade_code", ""] {
            assert!(
                harness_engine_for(name).is_none(),
                "{name:?} must not build"
            );
            assert!(matches!(
                resolve_turn_engine(Some(name)),
                TurnEngine::MagicianDecision
            ));
        }
    }

    /// Install-status detection is a pure PATH lookup — cheap enough to
    /// call per settings load, and never spawning anything.
    #[test]
    fn install_status_covers_the_whole_roster() {
        let roster = roster_with_install_status();
        assert_eq!(
            roster.len(),
            6,
            "pi, claude_code, codex, codex_app_server, grok, agy"
        );
        for (_, installed) in roster {
            // Whatever the machine's state, the flag is a bool — the point
            // is the shape, not this machine's binaries.
            let _ = installed;
        }
    }

    #[test]
    fn native_tool_posture_is_declared_per_engine() {
        use crate::magician_v2::execution::plane::engine::NativeToolPosture;
        let cases = [
            ("pi", NativeToolPosture::Stripped),
            ("claude_code", NativeToolPosture::Stripped),
            ("grok", NativeToolPosture::Stripped),
            ("codex", NativeToolPosture::Sandboxed),
            ("codex_app_server", NativeToolPosture::Sandboxed),
            ("agy", NativeToolPosture::Sandboxed),
        ];
        for (name, posture) in cases {
            let engine = harness_engine_for(name).expect(name);
            assert_eq!(engine.capabilities().native_tool_posture, posture, "{name}");
        }
    }

    /// Every roster engine resumes its own native session: claude_code's
    /// `--resume`, and for the rest the CLI's own resume flag or subcommand
    /// against the session it persisted in the chat mouth's per-conversation
    /// `native_home` (codex_app_server `thread/resume`, `codex exec resume`,
    /// grok `--resume`, agy `--conversation`).
    #[test]
    fn every_roster_engine_resumes_its_native_session() {
        for name in [
            "pi",
            "claude_code",
            "codex",
            "codex_app_server",
            "grok",
            "agy",
        ] {
            assert!(
                harness_engine_for(name)
                    .expect(name)
                    .capabilities()
                    .supports_resume,
                "{name} must resume its native session"
            );
        }
    }

    /// Which engines stream their reply through the plane sink as it is
    /// produced: claude_code's stream-json, codex_app_server's driver tap,
    /// grok's partial messages and agy's step updates do; `codex exec
    /// --json` emits completed items only, so the chat side sends its
    /// settled reply as one chunk.
    #[test]
    fn streaming_is_declared_per_engine() {
        let cases = [
            ("pi", true),
            ("claude_code", true),
            ("codex_app_server", true),
            ("grok", true),
            ("agy", true),
            ("codex", false),
        ];
        for (name, streams) in cases {
            let engine = harness_engine_for(name).expect(name);
            assert_eq!(engine.capabilities().streams_text_deltas, streams, "{name}");
        }
    }
}
