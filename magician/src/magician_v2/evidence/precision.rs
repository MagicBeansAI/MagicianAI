//! LLM-graded evidence precision (WEG generic substrate — eval depth).
//!
//! The one plan metric that genuinely needs an outside signal: is a distilled
//! [`EvidenceRecord`] *faithful to its source* (the episode / signals it was
//! distilled from), or did distillation invent/overstate work, metrics, outcomes,
//! entities, or people? `eval.rs` stays deterministic; this is the judge leg.
//!
//! Source loading is the caller's job — `grade_evidence_precision` takes
//! pre-assembled `(record, source_excerpt)` pairs so it stays independent of how
//! the source is fetched (task lane = episode, ambient lane = signals). Parse
//! failures are scored conservatively as **not faithful** (a precision metric
//! should not reward an unreadable verdict).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

use super::EvidenceRecord;

/// Why an unfaithful summary was unfaithful. The distinction is what lets a
/// caller with incomplete source coverage tell an invented claim from a claim
/// its source simply never covered: the first is a rejection, the second is a
/// caveat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedKind {
    /// The source says otherwise.
    Contradicted,
    /// The source does not address it either way.
    Absent,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrecisionVerdict {
    pub evidence_id: String,
    pub faithful: bool,
    pub reason: String,
    /// Set by prompt v1.1.0 when `faithful` is false. `None` when the judge did
    /// not say, which callers must treat as the conservative case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_kind: Option<UnsupportedKind>,
    /// Sentences the summary used to declare a limitation of itself — "could
    /// not be verified", "remains open". Prompt v1.2.0 returns them instead
    /// of scoring them: they are declarations about the answer, not claims
    /// about the world, and a caller grades them for honesty separately.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_open: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PrecisionReport {
    /// Records actually graded (had a loadable source + were sampled).
    pub sampled: usize,
    /// Of the sampled, how many the judge found faithful to source.
    pub faithful: usize,
    /// `faithful / sampled` (1.0 when nothing was sampled).
    pub precision: f64,
    pub verdicts: Vec<PrecisionVerdict>,
}

/// Parse the judge's `{ "faithful": bool, "reason": "...", "unsupported_kind": ... }`
/// (optionally fenced). `unsupported_kind` is optional and only meaningful when
/// `faithful` is false. Conservative: an unparseable verdict is `(false, …, None)`
/// so it counts against precision rather than being silently treated as
/// faithful, and an unreadable kind is `None` rather than a guess.
fn parse_precision_verdict(content: &str) -> (bool, String, Option<UnsupportedKind>, Vec<String>) {
    let mut s = content.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        s = rest.trim();
    } else if let Some(rest) = s.strip_prefix("```") {
        s = rest.trim();
    }
    if let Some(rest) = s.strip_suffix("```") {
        s = rest.trim();
    }
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => {
            let faithful = v.get("faithful").and_then(|f| f.as_bool());
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let unsupported_kind = v
                .get("unsupported_kind")
                .cloned()
                .and_then(|kind| serde_json::from_value::<UnsupportedKind>(kind).ok());
            let declared_open = v
                .get("declared_open")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            match faithful {
                Some(true) => (true, reason, None, declared_open),
                Some(false) => (false, reason, unsupported_kind, declared_open),
                None => (
                    false,
                    "judge response missing boolean `faithful`".to_string(),
                    None,
                    Vec::new(),
                ),
            }
        },
        Err(e) => (
            false,
            format!("unparseable judge response: {e}"),
            None,
            Vec::new(),
        ),
    }
}

/// Grade a sample of `(record, source_excerpt)` pairs for faithfulness-to-source
/// via the `evidence_precision_judge` store prompt. One LLM call per sample.
pub async fn grade_evidence_precision(
    samples: &[(EvidenceRecord, String)],
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<PrecisionReport> {
    let mut verdicts = Vec::with_capacity(samples.len());
    let mut faithful = 0usize;

    for (record, source) in samples {
        let verdict = grade_summary_precision(
            &record.evidence_id,
            source,
            &record.evidence_kind,
            &record.observed_actions,
            &record.summary,
            router,
            prompt_manager,
        )
        .await?;
        if verdict.faithful {
            faithful += 1;
        }
        verdicts.push(verdict);
    }

    let sampled = samples.len();
    let precision = if sampled == 0 {
        1.0
    } else {
        faithful as f64 / sampled as f64
    };
    Ok(PrecisionReport {
        sampled,
        faithful,
        precision,
        verdicts,
    })
}

/// Grade one arbitrary summary against bounded source text through the same
/// governed evidence-precision operation. Eval surfaces use this adapter so
/// they do not need a parallel judge profile, prompt, or parser.
pub async fn grade_summary_precision(
    evidence_id: &str,
    source_excerpt: &str,
    evidence_kind: &str,
    observed_actions: &[String],
    summary: &str,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<PrecisionVerdict> {
    grade_summary_precision_with_telemetry(
        evidence_id,
        source_excerpt,
        evidence_kind,
        observed_actions,
        summary,
        router,
        prompt_manager,
    )
    .await
    .map(|(verdict, _)| verdict)
}

/// Runtime variant of [`grade_summary_precision`] that preserves call
/// telemetry so an execution can charge the bounded review to the same cost
/// ledger as its normal agentic decisions.
pub async fn grade_summary_precision_with_telemetry(
    evidence_id: &str,
    source_excerpt: &str,
    evidence_kind: &str,
    observed_actions: &[String],
    summary: &str,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<(
    PrecisionVerdict,
    Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
)> {
    let mut vars = HashMap::new();
    vars.insert("source_excerpt".to_string(), source_excerpt.to_string());
    vars.insert("evidence_kind".to_string(), evidence_kind.to_string());
    vars.insert("observed_actions".to_string(), observed_actions.join(", "));
    vars.insert("evidence_summary".to_string(), summary.to_string());

    let prompt = prompt_manager
        .get_rendered_prompt("evidence_precision_judge", "1.2.0", vars)
        .await?;
    let operation = LLMOperation::Other("evidence_precision_judge".to_string());
    let response = router
        .generate_for_operation_with_system(&operation, None, &prompt)
        .await?;
    let (faithful, reason, unsupported_kind, declared_open) =
        parse_precision_verdict(&response.content);
    Ok((
        PrecisionVerdict {
            evidence_id: evidence_id.to_string(),
            faithful,
            reason,
            unsupported_kind,
            declared_open,
        },
        response.telemetry,
    ))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn parses_faithful_true_false_and_fenced() {
        assert_eq!(
            parse_precision_verdict(r#"{"faithful": true, "reason": "ok"}"#),
            (true, "ok".to_string(), None, Vec::new())
        );
        assert_eq!(
            parse_precision_verdict(r#"{"faithful": false, "reason": "invented a metric"}"#).0,
            false
        );
        let (f, _, _, _) =
            parse_precision_verdict("```json\n{\"faithful\": true, \"reason\": \"x\"}\n```");
        assert!(f, "fenced JSON parses");
    }

    #[test]
    fn unparseable_or_missing_is_not_faithful() {
        assert_eq!(parse_precision_verdict("not json").0, false);
        assert_eq!(parse_precision_verdict(r#"{"reason": "no bool"}"#).0, false);
    }

    #[test]
    fn unsupported_kind_is_read_only_for_unfaithful_verdicts_and_never_guessed() {
        assert_eq!(
            parse_precision_verdict(
                r#"{"faithful": false, "reason": "source omits it", "unsupported_kind": "absent"}"#
            )
            .2,
            Some(UnsupportedKind::Absent)
        );
        assert_eq!(
            parse_precision_verdict(
                r#"{"faithful": false, "reason": "source says 1.98.0", "unsupported_kind": "contradicted"}"#
            )
            .2,
            Some(UnsupportedKind::Contradicted)
        );
        // A v1.0.0-shaped answer, or a kind the judge misspelled, stays None.
        assert_eq!(
            parse_precision_verdict(r#"{"faithful": false, "reason": "x"}"#).2,
            None
        );
        assert_eq!(
            parse_precision_verdict(
                r#"{"faithful": false, "reason": "x", "unsupported_kind": "unclear"}"#
            )
            .2,
            None
        );
        assert_eq!(
            parse_precision_verdict(
                r#"{"faithful": true, "reason": "ok", "unsupported_kind": "absent"}"#
            )
            .2,
            None
        );
    }
}
