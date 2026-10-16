//! The run engine pin: what a run thinks with, fixed when it launches.
//!
//! Kept out of `turn_engine.rs` on purpose: `magician-bin`'s
//! execution-harness contract compiles that file on its own, and a type
//! defined there would be a second, incompatible `RunEnginePin` beside the
//! one `AgenticContext` holds.

use super::turn_engine::{harness_engine_snapshot, settings_pin};

/// What a run thinks with, fixed when it launches: the engine together with
/// the harness model and Pi profile. Settings writes the three as one choice,
/// so a run keeps all three for its whole life — through approval pauses,
/// restarts, and its own delegated children. A Settings switch moves only
/// runs launched after it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunEnginePin {
    pub engine: String,
    /// The model the harness CLI runs; `default` = the CLI's own choice.
    pub harness_model: String,
    #[serde(default)]
    pub pi_profile: Option<String>,
}

impl RunEnginePin {
    /// A pin for `engine`, given the Settings pin. The Settings harness model
    /// was chosen for the Settings engine; another engine runs its CLI's own
    /// default rather than a model name meant for a different CLI. The Pi
    /// profile is only read when the engine is `pi`.
    pub fn for_engine(engine: &str, settings: &RunEnginePin) -> Self {
        let engine = engine.trim();
        if engine == settings.engine {
            return settings.clone();
        }
        Self {
            engine: engine.to_string(),
            harness_model: "default".to_string(),
            pi_profile: settings.pi_profile.clone(),
        }
    }

    /// The identity of the native conversation this pin drives. A saved
    /// native session resumes only under the same fingerprint: another
    /// engine cannot load it, and another model or profile would silently
    /// continue a conversation the operator switched away from.
    pub fn session_fingerprint(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.engine,
            self.harness_model,
            self.pi_profile.as_deref().unwrap_or("")
        )
    }
}

tokio::task_local! {
    /// The pin of the run whose work is executing on this task. A run
    /// launched from inside it (a delegated child, or a task one of its
    /// actions creates) inherits this pin. Carried across hops by
    /// `CapturedRunTaskLocals`.
    static LAUNCHING_RUN_ENGINE_PIN: Option<RunEnginePin>;
}

/// Run `future` with `pin` as the launching run's pin.
///
/// A plain function, not an `async fn`: it wraps unboxed run futures on
/// every lane and action hop, and an `async fn` wrapper would hold another
/// copy of the wrapped future in its own state machine (the stack-size
/// pattern `with_parent_engine` avoids the same way).
pub fn with_launching_run_engine_pin<F: std::future::Future>(
    pin: Option<RunEnginePin>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    LAUNCHING_RUN_ENGINE_PIN.scope(pin, future)
}

/// The pin of the run executing on this task, if any.
pub fn current_launching_run_engine_pin() -> Option<RunEnginePin> {
    LAUNCHING_RUN_ENGINE_PIN
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

/// Resolve a new run's pin at launch. An engine named for this run wins (a
/// plane grant's harness); a named engine equal to the launching run's keeps
/// that run's model and profile too. Otherwise the run launching this one
/// passes its pin down, and a run launched by no run takes the Settings
/// choice at this instant.
pub fn resolve_launch_pin(named_engine: Option<&str>) -> RunEnginePin {
    resolve_launch_pin_from(
        named_engine,
        current_launching_run_engine_pin(),
        &settings_pin(&harness_engine_snapshot()),
    )
}

fn resolve_launch_pin_from(
    named_engine: Option<&str>,
    parent: Option<RunEnginePin>,
    settings: &RunEnginePin,
) -> RunEnginePin {
    match named_engine
        .map(str::trim)
        .filter(|named| !named.is_empty())
    {
        Some(named) => match parent {
            Some(parent) if parent.engine == named => parent,
            _ => RunEnginePin::for_engine(named, settings),
        },
        None => parent.unwrap_or_else(|| settings.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::types::AgenticContext;

    fn pin(engine: &str, harness_model: &str) -> RunEnginePin {
        RunEnginePin {
            engine: engine.to_string(),
            harness_model: harness_model.to_string(),
            pi_profile: None,
        }
    }

    /// An engine the snapshot does not name, whatever this process's
    /// Settings say, so a named-engine case is never the snapshot's own.
    fn engine_other_than_settings() -> &'static str {
        if harness_engine_snapshot().engine == "grok" {
            "claude_code"
        } else {
            "grok"
        }
    }

    #[test]
    fn a_run_launched_from_a_run_inherits_its_pin() {
        let settings = pin("claude_code", "opus-settings");
        let parent = pin("codex", "gpt-parent");
        assert_eq!(
            resolve_launch_pin_from(None, Some(parent.clone()), &settings),
            parent,
            "a child thinks with its parent's pin"
        );
        assert_eq!(
            resolve_launch_pin_from(Some("codex"), Some(parent.clone()), &settings),
            parent,
            "naming the parent's own engine keeps the parent's model"
        );
        let named = resolve_launch_pin_from(Some("grok"), Some(parent), &settings);
        assert_eq!(named.engine, "grok", "an engine named for the run wins");
        assert_eq!(
            named.harness_model, "default",
            "a model chosen for another CLI is never handed to this one"
        );
        assert_eq!(
            resolve_launch_pin_from(Some("claude_code"), None, &settings),
            settings,
            "naming the Settings engine keeps the Settings model"
        );
        assert_eq!(
            resolve_launch_pin_from(None, None, &settings),
            settings,
            "a run launched by no run takes the Settings choice"
        );
    }

    #[tokio::test]
    async fn the_launching_pin_rides_the_task_local() {
        let parent = pin("codex", "gpt-parent");
        let seen =
            with_launching_run_engine_pin(Some(parent.clone()), async { resolve_launch_pin(None) })
                .await;
        assert_eq!(seen, parent);
        assert_eq!(current_launching_run_engine_pin(), None);
    }

    #[test]
    fn composition_pins_every_run_and_a_saved_pin_stands() {
        let saved = pin("codex", "gpt-saved");
        let ctx = AgenticContext::default().with_run_engine_pin(Some(saved.clone()));
        assert_eq!(ctx.run_engine_pin.as_ref(), Some(&saved));
        assert_eq!(ctx.harness_engine.as_deref(), Some("codex"));

        let mut named_same = AgenticContext::default();
        named_same.harness_engine = Some("codex".to_string());
        assert_eq!(
            named_same
                .with_run_engine_pin(Some(saved.clone()))
                .run_engine_pin,
            Some(saved.clone())
        );

        let other = engine_other_than_settings();
        let mut named_other = AgenticContext::default();
        named_other.harness_engine = Some(other.to_string());
        let named_other = named_other.with_run_engine_pin(Some(saved));
        assert_eq!(named_other.harness_engine.as_deref(), Some(other));
        assert_eq!(
            named_other.run_engine_pin.map(|pin| pin.engine).as_deref(),
            Some(other),
            "the sealed attenuation's engine outranks a disagreeing saved pin"
        );

        let unpinned = AgenticContext::default().with_run_engine_pin(None);
        let pinned = unpinned
            .run_engine_pin
            .clone()
            .expect("every run is pinned");
        assert_eq!(
            unpinned.harness_engine.as_deref(),
            Some(pinned.engine.as_str())
        );
    }

    #[test]
    fn fingerprints_separate_engine_model_and_profile() {
        let base = pin("codex", "gpt-a");
        let mut profiled = base.clone();
        profiled.pi_profile = Some("fast".to_string());
        for other in [pin("claude_code", "gpt-a"), pin("codex", "gpt-b"), profiled] {
            assert_ne!(base.session_fingerprint(), other.session_fingerprint());
        }
    }
}
