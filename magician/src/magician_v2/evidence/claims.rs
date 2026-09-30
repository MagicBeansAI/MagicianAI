//! Generic claim records (WEG generic substrate) — facet-scoped derived
//! assertions over evidence.
//!
//! Per the design's "career interpretation record": claims are LLM-derived at
//! read/output time and are **not a durable tier** — generated on demand,
//! grounded by `supporting_evidence_ids`, validated deterministically, and never
//! auto-promoted to memory. Domain-agnostic: career ("repeated ownership on X"),
//! health ("consistent exercise"), relationships ("frequent contact with Y") are
//! the same mechanism, different facet. The LLM proposes; deterministic grounding
//! is the gate — ungrounded claims (no support, or any hallucinated citation) are
//! dropped, not surfaced.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

use super::{is_sensitive, EvidenceRecord, EvidenceStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimTimeWindow {
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
}

/// A derived, facet-scoped assertion over evidence. Ephemeral by design (read-time
/// / artifact-local) — never persisted as a durable tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub statement: String,
    #[serde(default)]
    pub claim_type: String,
    #[serde(default)]
    pub facet: String,
    #[serde(default)]
    pub supporting_evidence_ids: Vec<String>,
    #[serde(default)]
    pub time_window: Option<ClaimTimeWindow>,
    #[serde(default)]
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ClaimsWrapper {
    #[serde(default)]
    claims: Vec<ClaimRecord>,
}

/// Deterministic grounding verdict for one claim.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimGrounding {
    /// ≥1 supporting id AND every cited id exists in-scope (no hallucinations).
    pub grounded: bool,
    /// Cited ids resolving to an in-scope (active, non-sensitive) record.
    pub supported: usize,
    /// Cited ids NOT found in scope — hallucinated / dangling citations.
    pub missing: Vec<String>,
}

fn extract_json(content: &str) -> String {
    let mut s = content.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        s = rest.trim();
    } else if let Some(rest) = s.strip_prefix("```") {
        s = rest.trim();
    }
    if let Some(rest) = s.strip_suffix("```") {
        s = rest.trim();
    }
    s.to_string()
}

/// Parse the model's claims output — a bare JSON array or a `{ "claims": [...] }`
/// wrapper, optionally fenced. Unknown fields are ignored.
pub fn parse_claims(content: &str) -> anyhow::Result<Vec<ClaimRecord>> {
    let cleaned = extract_json(content);
    if let Ok(wrapper) = serde_json::from_str::<ClaimsWrapper>(&cleaned) {
        if !wrapper.claims.is_empty() {
            return Ok(wrapper.claims);
        }
    }
    serde_json::from_str::<Vec<ClaimRecord>>(&cleaned)
        .map_err(|e| anyhow::anyhow!("parsing claims JSON: {e}"))
}

/// Deterministic grounding gate: every `supporting_evidence_ids` entry must
/// resolve to an in-scope (active, non-sensitive) record, and there must be at
/// least one. A claim that fails this MUST be dropped, never surfaced.
pub fn validate_claim_grounding(claim: &ClaimRecord, records: &[EvidenceRecord]) -> ClaimGrounding {
    let in_scope: HashSet<&str> = records
        .iter()
        .filter(|r| r.status == EvidenceStatus::Active && !is_sensitive(&r.sensitivity))
        .map(|r| r.evidence_id.as_str())
        .collect();
    let mut supported = 0;
    let mut missing = Vec::new();
    for id in &claim.supporting_evidence_ids {
        if in_scope.contains(id.as_str()) {
            supported += 1;
        } else {
            missing.push(id.clone());
        }
    }
    ClaimGrounding {
        grounded: supported > 0 && missing.is_empty(),
        supported,
        missing,
    }
}

/// LLM proposal stage: ask the model for facet-scoped claims grounded in the
/// assembled evidence packet (`build_review_packet`). The model proposes; callers
/// MUST run [`validate_claim_grounding`] and drop ungrounded claims.
pub async fn propose_claims(
    packet: &str,
    window_days: i64,
    facet: Option<&str>,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<Vec<ClaimRecord>> {
    let mut vars = HashMap::new();
    vars.insert("window_days".to_string(), window_days.to_string());
    vars.insert("facet".to_string(), facet.unwrap_or("all").to_string());
    vars.insert("evidence_packet".to_string(), packet.to_string());
    let prompt = prompt_manager
        .get_rendered_prompt("evidence_claims", "1.0.0", vars)
        .await?;
    let operation = LLMOperation::Other("evidence_claims".to_string());
    let response = router
        .generate_for_operation_with_system(&operation, None, &prompt)
        .await?;
    parse_claims(&response.content)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn rec(id: &str, sensitivity: &str) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.into(),
            summary: "s".into(),
            evidence_kind: "activity".into(),
            observed_actions: vec![],
            entity_keys: vec![],
            people_keys: vec![],
            artifact_refs: vec![],
            source_refs: vec![],
            facets: vec![],
            importance: 0.6,
            confidence: 0.6,
            sensitivity: sensitivity.into(),
            first_seen_at: "2026-06-13T00:00:00Z".into(),
            last_seen_at: "2026-06-13T00:00:00Z".into(),
            status: EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "task_episode".into(),
            metadata: serde_json::Value::Null,
        }
    }

    fn claim(ids: &[&str]) -> ClaimRecord {
        ClaimRecord {
            statement: "did a thing".into(),
            claim_type: "ownership".into(),
            facet: "work".into(),
            supporting_evidence_ids: ids.iter().map(|s| s.to_string()).collect(),
            time_window: None,
            confidence: 0.7,
        }
    }

    #[test]
    fn parse_array_wrapper_and_fenced() {
        let arr = r#"[{"statement":"x","supporting_evidence_ids":["evd:1"]}]"#;
        assert_eq!(parse_claims(arr).unwrap().len(), 1);
        let wrapped = r#"{"claims":[{"statement":"x","supporting_evidence_ids":["evd:1"]},{"statement":"y"}]}"#;
        assert_eq!(parse_claims(wrapped).unwrap().len(), 2);
        let fenced =
            "```json\n[{\"statement\":\"x\",\"supporting_evidence_ids\":[\"evd:1\"]}]\n```";
        assert_eq!(parse_claims(fenced).unwrap().len(), 1);
    }

    #[test]
    fn grounding_requires_all_ids_in_scope() {
        let records = [rec("evd:1", "work"), rec("evd:2", "work")];
        let ok = validate_claim_grounding(&claim(&["evd:1", "evd:2"]), &records);
        assert!(ok.grounded);
        assert_eq!(ok.supported, 2);
        assert!(ok.missing.is_empty());
    }

    #[test]
    fn grounding_flags_hallucinated_ids() {
        let records = [rec("evd:1", "work")];
        let bad = validate_claim_grounding(&claim(&["evd:1", "evd:nope"]), &records);
        assert!(!bad.grounded, "a dangling citation ungrounds the claim");
        assert_eq!(bad.missing, vec!["evd:nope".to_string()]);
    }

    #[test]
    fn grounding_rejects_sensitive_and_unsupported() {
        let sensitive = [rec("evd:1", "financial")]; // sensitive → out of scope
        assert!(!validate_claim_grounding(&claim(&["evd:1"]), &sensitive).grounded);
        let any = [rec("evd:1", "work")];
        assert!(
            !validate_claim_grounding(&claim(&[]), &any).grounded,
            "no supporting ids → not grounded"
        );
    }
}
