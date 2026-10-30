//! Bounded output requirements and authored finding checkpoints. These carry
//! intent and continuity, never permission or proof of a claim. Full sources
//! remain in the existing scoped artifact/result stores.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const REQUEST_GUIDANCE: &str = "\n\n## Delivering the requested outcome\nLead with the answer, artifact or action result the user requested, including useful partial findings. Keep material limitations beside the affected conclusion and secondary gaps after the findings. Process details, counts and paths are appropriate when requested; otherwise they support the answer. Before expensive work, ask through the existing user-question/HITL flow only when an essential missing input prevents meaningful progress. Continue useful independent work when possible and label consequential assumptions. Preserve findings and original source references across long work; a completed step or an authored summary is not factual verification.\n";

pub const MAX_REQUIREMENTS: usize = 8;
pub const MAX_FINDING_CHARS: usize = 800;
pub const MAX_FINDING_REFS: usize = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRequirement {
    pub id: String,
    pub description: String,
}

/// Select literal owner-authored requirements, without inventing a new plan.
/// The full request is always retained as the first obligation; explicit list
/// entries give retrieval more focused queries. Work-step status is separate.
pub fn requirements(goal: &str) -> Vec<DeliveryRequirement> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Vec::new();
    }
    let mut descriptions = vec![clip(goal, 4_000)];
    for line in goal.lines() {
        let line = line.trim();
        let entry = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| {
                let n = line.bytes().take_while(u8::is_ascii_digit).count();
                if n > 0 && matches!(line.as_bytes().get(n), Some(b'.' | b')')) {
                    Some(line[n + 1..].trim_start())
                } else {
                    None
                }
            });
        if let Some(entry) = entry.filter(|text| !text.is_empty()) {
            let entry = clip(entry, 800);
            if !descriptions.contains(&entry) {
                descriptions.push(entry);
            }
        }
        if descriptions.len() == MAX_REQUIREMENTS {
            break;
        }
    }
    descriptions
        .into_iter()
        .enumerate()
        .map(|(index, description)| DeliveryRequirement {
            id: format!("request_{}", index + 1),
            description,
        })
        .collect()
}

/// Head and tail, with an explicit omission marker. Never use this for opaque
/// evidence identifiers: an overlong identifier must be omitted, not modified.
pub fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    const MARKER: &str = "\n[… omitted …]\n";
    let available = chars.saturating_sub(MARKER.chars().count());
    if available == 0 {
        return text.chars().take(chars).collect();
    }
    let head = available * 2 / 3;
    let tail = available - head;
    format!(
        "{}{}{}",
        text.chars().take(head).collect::<String>(),
        MARKER,
        text.chars()
            .rev()
            .take(tail)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    )
}

pub fn evidence_refs(goal: &Value) -> Vec<Value> {
    goal.get("result_evidence_refs")
        .or_else(|| goal.get("evidence_refs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        // References are untrusted selectors. Keep their exact representation
        // or leave them out; never truncate an opaque result capability.
        .filter(|value| serde_json::to_string(value).is_ok_and(|text| text.len() <= 256))
        .take(MAX_FINDING_REFS)
        .cloned()
        .collect()
}

/// A model-authored report saved with the micro-goal. It must still pass the
/// ordinary source/receipt grounding checks before it can be called verified.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FindingCheckpoint {
    pub requirement_id: String,
    pub description: String,
    pub summary: String,
    pub status: String,
    pub evidence_refs: Vec<Value>,
    #[serde(default)]
    pub remaining_gap: Option<String>,
}

pub fn findings(state: &Value) -> Vec<FindingCheckpoint> {
    state
        .get("micro_goals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|goal| {
            let summary = goal.get("result_summary")?.as_str()?.trim();
            let refs = evidence_refs(goal);
            if summary.is_empty() || refs.is_empty() {
                return None;
            }
            Some(FindingCheckpoint {
                requirement_id: clip(goal.get("id")?.as_str()?, 80),
                description: clip(goal.get("description")?.as_str()?, 400),
                summary: clip(summary, MAX_FINDING_CHARS),
                status: goal
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("in_progress")
                    .to_string(),
                evidence_refs: refs,
                remaining_gap: goal
                    .get("blocked_reason")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| clip(text, 300)),
            })
        })
        .take(16)
        .collect()
}

pub fn partial_findings_for_task(task_id: &str, state: &str) -> Option<String> {
    if task_id.trim().is_empty() {
        return None;
    }
    let state: Value = serde_json::from_str(state).ok()?;
    if state.get("task_id").and_then(Value::as_str) != Some(task_id) {
        return None;
    }
    partial_findings_markdown(&state)
}

pub fn partial_findings_markdown(state: &Value) -> Option<String> {
    let findings = findings(state);
    if findings.is_empty() {
        return None;
    }
    let mut text = String::from("## Findings saved during execution\n\nThese authored checkpoints retain their original evidence references; they are not independent verification.\n");
    for finding in findings {
        // JSON strings keep authored delimiters/newlines as data, including
        // attempts to close a Markdown fence or imitate runtime headings.
        text.push_str(&format!(
            "\n- Requirement: {}\n  Finding: {}\n  Evidence references: {}\n",
            serde_json::to_string(&finding.description).unwrap_or_default(),
            serde_json::to_string(&finding.summary).unwrap_or_default(),
            serde_json::to_string(&finding.evidence_refs).unwrap_or_default()
        ));
        if let Some(gap) = finding.remaining_gap {
            text.push_str(&format!(
                "  Remaining gap: {}\n",
                serde_json::to_string(&gap).unwrap_or_default()
            ));
        }
    }
    Some(text)
}

/// Kept separate from factual grounding. An unavailable or unanswered coverage
/// judgement cannot turn a useful answer into an unsupported-claims warning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeliveryCheck {
    pub reviewed: usize,
    pub unreviewed: usize,
    pub missing: Vec<DeliveryGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryGap {
    pub requirement_id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DeliveryCheck {
    /// A verdict on the authored candidate cannot certify a different body
    /// after factual review removes supporting statements. Unknown coverage
    /// cannot authorize a quality-only replacement or create a new warning.
    pub fn invalidate_after_edit(&mut self) {
        self.unreviewed = self.unreviewed.saturating_add(self.reviewed);
        self.reviewed = 0;
        self.missing.clear();
    }

    pub fn repair_feedback(&self) -> String {
        if self.missing.is_empty() {
            return String::new();
        }
        format!("DELIVERY COVERAGE: The answer omitted useful findings available for these requested outcomes: {}. Rewrite the answer to deliver the available supported result. Keep precise unresolved gaps and the original evidence strength; do not invent missing facts or substitute tool counts, file sizes, paths or process commentary. Coverage is not a factual-support verdict.",
            serde_json::to_string(&self.missing).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requirements_keep_owner_intent_and_explicit_items_without_inventing_work() {
        let goal = "Compare systems\n1) model switching\n2. persistent memory\n- fair demos";
        let items = requirements(goal);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].description, goal);
        assert_eq!(items[2].description, "persistent memory");
        assert!(requirements("").is_empty());
    }

    #[test]
    fn partial_findings_survive_blocked_status_and_preserve_exact_references() {
        let state = json!({"micro_goals":[{"id":"comparison","description":"Compare memory",
            "status":"blocked","result_summary":"System A persists named memory blocks; model switching was not tested.",
            "evidence_refs":["result_ref_v1_0123456789"],"blocked_reason":"System B docs unavailable"}]});
        let text = partial_findings_markdown(&state).unwrap();
        assert!(text.contains("persists named memory blocks"));
        assert!(text.contains("result_ref_v1_0123456789"));
        assert!(text.contains("System B docs unavailable"));
        assert!(!text.contains("COMPLETED"));
    }

    #[test]
    fn missing_or_oversized_refs_never_become_verified_finding_checkpoints() {
        let state = json!({"micro_goals":[{"id":"x","description":"x","status":"completed",
            "result_summary":"claimed finding","evidence_refs":["x".repeat(300)]}]});
        assert!(findings(&state).is_empty());
    }

    #[test]
    fn bounded_text_keeps_tail_and_does_not_break_unicode() {
        let text = format!("{}IMPORTANT CONCLUSION", "श".repeat(1000));
        let clipped = clip(&text, 100);
        assert!(clipped.ends_with("IMPORTANT CONCLUSION"));
        assert_eq!(clipped.chars().count(), 100);
    }

    #[test]
    fn absent_coverage_review_is_not_a_factual_failure() {
        let review = DeliveryCheck {
            unreviewed: 2,
            ..Default::default()
        };
        assert!(review.repair_feedback().is_empty());
    }
    #[test]
    fn checkpoint_projection_rejects_foreign_and_missing_task_identity() {
        let state = json!({"task_id":"task-a","micro_goals":[{"id":"result","description":"Result",
            "status":"blocked","result_summary":"Useful source finding","evidence_refs":["observed-source"]}]}).to_string();
        assert!(partial_findings_for_task("task-a", &state).is_some());
        assert!(partial_findings_for_task("task-b", &state).is_none());
        assert!(partial_findings_for_task("", &state).is_none());
    }
}
