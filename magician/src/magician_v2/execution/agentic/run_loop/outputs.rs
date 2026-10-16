//! What a run has produced so far.
//!
//! The second scratch group to move out of `ActionExecutors`, after the identity.
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md` — a worker that resumes an
//! execution has to be told what the run already produced, or it reports having
//! made nothing and re-runs objectives it had already closed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An inner-loop objective this execution has closed.
///
/// Moved here from `executor.rs`, where it was a private non-serializable struct.
/// It gains `Serialize`/`Deserialize` and **keeps every field**: an earlier cut of
/// this module carried a two-field projection (`objective_id` + `summary`), which
/// would have left a resumed run holding objectives marked complete with no
/// evidence, no final URL, no capability and no completion time. Skipping
/// re-execution on the strength of a record that thin is worse than not skipping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedPrimitiveObjective {
    pub capability_name: String,
    pub objective_id: String,
    pub objective_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    pub completed_at: DateTime<Utc>,
    pub iteration: usize,
}

/// Everything this execution has accumulated on its way through.
///
/// # Why this is NOT shaped like `RunIdentity`
///
/// [`super::state::ExecutorRunIdentity`] holds an `Arc` inside its lock, so a read
/// is a refcount bump and a write is copy-on-write. That is right for the
/// identity: read on essentially every dispatch, written at a handful of entry
/// points, and small.
///
/// These are the opposite on both axes. They are **appended to** — a durable file
/// action pushes a path, a closed objective pushes a record — and they **grow**.
/// Copy-on-write per append is O(n) per push and O(n²) across a run, paid to make
/// reads cheap that almost never happen. So this is mutated in place behind a
/// plain lock and only reads clone.
///
/// The two shapes are a deliberate pair rather than an inconsistency: state moves
/// out of ambient memory into a named value either way, and each takes the form
/// its access pattern actually has.
///
/// # What is deliberately NOT here
///
/// `app_labeled_tool_results` is not here. It lives on
/// [`super::controls::RunControls`] instead, beside `shell_stream_ctx`, because
/// the two share a shape rather than a subject. The field audit places it
/// per-execution, which is true, but it is an in-flight handoff rather than an
/// accumulation: the dispatcher inserts after server labeling and the owning loop
/// removes it before any generic artifact, evidence or projection sink sees the
/// bytes — both inside one dispatch. Putting it in a boundary-carrying value and
/// then marking it `#[serde(skip)]` would say two contradictory things about it.
/// A resumed worker has no dispatcher waiting to collect the entry, so carrying it
/// would either leak labeled bytes into a record the loop's contract keeps them
/// out of, or restore a handoff nobody will take.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RunOutputs {
    /// Durable artifact relative paths written during this execution.
    ///
    /// Read at completion to build `completion_artifact_names` with real
    /// filesystem paths. A resumed run that lost this reports having produced
    /// nothing, which is why it crosses the boundary rather than living only in
    /// process memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub durable_artifacts_written: Vec<String>,

    /// Objectives this execution has closed.
    ///
    /// Objective-scoped, not root-task-scoped: the outer LLM may continue with
    /// distinct remaining work, but a duplicate invocation of an objective
    /// already finished is skipped and fed back as history. Losing this on a
    /// resume makes the run redo finished work — and, for any objective whose
    /// completion had an effect, redo the effect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_primitive_objectives: Vec<CompletedPrimitiveObjective>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objective() -> CompletedPrimitiveObjective {
        CompletedPrimitiveObjective {
            capability_name: "browser".to_string(),
            objective_id: "browser::open-and-read".to_string(),
            objective_text: "open the filing and read it".to_string(),
            evidence: Some("artifact://evidence/1".to_string()),
            summary: Some("read the filing".to_string()),
            final_url: Some("https://example.test/filing".to_string()),
            completed_at: DateTime::parse_from_rfc3339("2026-08-26T10:00:00Z")
                .expect("fixture timestamp")
                .with_timezone(&Utc),
            iteration: 4,
        }
    }

    #[test]
    fn an_untouched_run_adds_nothing_to_the_wire() {
        let encoded = serde_json::to_string(&RunOutputs::default()).expect("serialize");
        assert_eq!(
            encoded, "{}",
            "a run that produced nothing must not grow the record that carries it"
        );
    }

    #[test]
    fn a_closed_objective_keeps_every_field_across_the_boundary() {
        // REGRESSION GUARD against the shape this module first had. A projection
        // carrying only `objective_id` and `summary` round-trips perfectly and is
        // still wrong: a resumed run would skip re-executing an objective on the
        // strength of a record with no evidence, no final URL, no capability and
        // no completion time. Field-for-field equality is the assertion, not a
        // spot check, because the failure is a field quietly going missing.
        let outputs = RunOutputs {
            durable_artifacts_written: vec!["notes/summary.md".to_string()],
            completed_primitive_objectives: vec![objective()],
        };

        let encoded = serde_json::to_string(&outputs).expect("serialize");
        let restored: RunOutputs = serde_json::from_str(&encoded).expect("read back");

        assert_eq!(
            restored.durable_artifacts_written,
            vec!["notes/summary.md".to_string()]
        );
        assert_eq!(
            restored.completed_primitive_objectives,
            vec![objective()],
            "every field of a closed objective must survive, not just the id"
        );
    }

    #[test]
    fn an_objective_written_before_the_optional_fields_existed_still_loads() {
        // The record is append-only in practice and older rows must keep reading.
        // Absent means "nobody recorded one", never "there was none".
        let legacy = r#"{
            "capability_name": "browser",
            "objective_id": "browser::open",
            "objective_text": "open it",
            "completed_at": "2026-08-26T10:00:00Z",
            "iteration": 1
        }"#;
        let restored: CompletedPrimitiveObjective =
            serde_json::from_str(legacy).expect("a row without the optionals must load");
        assert!(restored.evidence.is_none());
        assert!(restored.summary.is_none());
        assert!(restored.final_url.is_none());
        assert_eq!(restored.iteration, 1);
    }
}
