//! Prompt rendering for active reusable procedures.
//!
//! Procedures are stored separately from semantic memory because they are
//! operational guidance, not facts/preferences. This module retrieves the
//! relevant active procedures for the current goal and renders a bounded prompt
//! block with retrieval rationale for prompt projections and audits.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use magician_vector_index::OllamaEmbedder;
use serde::Serialize;
use serde_json::json;
use tracing::warn;

use crate::magician_v2::analytics::memory_index_maintainer::{
    note_retrieval_fallback_episode, note_retrieval_recovery_episode,
};
use crate::magician_v2::prompt_identity::neutralize_boundary_tags;

use super::{
    procedure_index::{
        procedure_vector_table, queue_procedure_index_refresh,
        queue_procedure_index_refresh_if_stale,
    },
    CreateLearningEventRequest, LearningProcedure, LearningProcedureFilters,
    LearningProcedureStatus, LearningScope, LearningStore,
};

const DEFAULT_MAX_PROCEDURES: usize = 3;
const DEFAULT_MAX_CHARS: usize = 3_500;
const MIN_RELEVANCE_SCORE: i64 = 8;
const MAX_QUERY_EXCERPT_CHARS: usize = 500;
const MAX_FIELD_CHARS: usize = 700;
const PROCEDURE_HYBRID_SCORE_SCALE: f32 = 10_000.0;
const MAX_PROCEDURE_HYBRID_SEARCH_RESULTS: usize = 256;

#[derive(Debug, Clone)]
pub struct LearningProcedureRenderRequest<'a> {
    pub agent_id: Option<&'a str>,
    pub relevance_query: &'a str,
    pub max_entries: usize,
    pub max_chars: usize,
    pub include_rationale: bool,
    pub emit_audit: bool,
    pub task_id: Option<&'a str>,
    pub execution_id: Option<&'a str>,
    pub chat_session_id: Option<&'a str>,
}

impl<'a> LearningProcedureRenderRequest<'a> {
    pub fn for_goal(relevance_query: &'a str) -> Self {
        Self {
            agent_id: None,
            relevance_query,
            max_entries: DEFAULT_MAX_PROCEDURES,
            max_chars: DEFAULT_MAX_CHARS,
            include_rationale: true,
            emit_audit: true,
            task_id: None,
            execution_id: None,
            chat_session_id: None,
        }
    }

    pub fn with_agent(mut self, agent_id: Option<&'a str>) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn with_execution(
        mut self,
        task_id: Option<&'a str>,
        execution_id: Option<&'a str>,
    ) -> Self {
        self.task_id = task_id;
        self.execution_id = execution_id;
        self
    }

    pub fn with_chat_session(mut self, chat_session_id: Option<&'a str>) -> Self {
        self.chat_session_id = chat_session_id;
        self
    }

    pub fn with_emit_audit(mut self, emit_audit: bool) -> Self {
        self.emit_audit = emit_audit;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningProcedurePromptSelection {
    pub procedure_id: String,
    pub title: String,
    pub score: i64,
    pub reason: String,
    pub owner_agent: Option<String>,
    pub success_count: u64,
    pub failure_count: u64,
}

#[derive(Debug, Clone)]
pub struct LearningProcedurePromptRenderResult {
    pub section: Option<String>,
    pub selected: Vec<LearningProcedurePromptSelection>,
    pub candidate_count: usize,
    pub dropped_count: usize,
    pub retrieval_backend: LearningProcedurePromptRetrievalBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningProcedurePromptRetrievalBackend {
    Direct,
    LancedbHybrid,
    DirectFallback,
}

impl LearningProcedurePromptRetrievalBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::LancedbHybrid => "lancedb_hybrid",
            Self::DirectFallback => "direct_fallback",
        }
    }
}

#[derive(Debug, Clone)]
struct ScoredProcedure {
    procedure: LearningProcedure,
    score: i64,
    reason: String,
    selected_match_count: usize,
    avoid_match_count: usize,
    harmful_feedback: bool,
}

/// Load active reusable procedures and render the top relevant procedures for
/// prompt injection. Retrieval is read-only; canonical procedure YAML remains
/// the source of truth and audit failure does not block execution.
pub fn render_active_procedures_for_prompt(
    store: &LearningStore,
    scope: &LearningScope,
    request: &LearningProcedureRenderRequest<'_>,
) -> Result<LearningProcedurePromptRenderResult> {
    let max_entries = request.max_entries.max(1);
    let max_chars = request.max_chars.max(500);
    let procedures = store.list_procedures(
        scope,
        LearningProcedureFilters {
            status: Some(LearningProcedureStatus::Active.as_str().to_string()),
            owner_agent: None,
            limit: None,
        },
    )?;
    let candidate_count = procedures.len();
    let mut scored = procedures
        .into_iter()
        .filter(|procedure| procedure_visible_to_agent(procedure, request.agent_id))
        .filter_map(|procedure| score_procedure(procedure, request.relevance_query))
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                right
                    .procedure
                    .success_count
                    .cmp(&left.procedure.success_count)
            })
            .then_with(|| {
                left.procedure
                    .failure_count
                    .cmp(&right.procedure.failure_count)
            })
            .then_with(|| right.procedure.updated_at.cmp(&left.procedure.updated_at))
            .then_with(|| left.procedure.id.cmp(&right.procedure.id))
    });

    let selected_scored = scored
        .into_iter()
        .filter(|scored| scored.score >= MIN_RELEVANCE_SCORE)
        .take(max_entries)
        .collect::<Vec<_>>();

    let selections = selected_scored
        .iter()
        .map(|scored| LearningProcedurePromptSelection {
            procedure_id: scored.procedure.id.clone(),
            title: scored.procedure.title.clone(),
            score: scored.score,
            reason: scored.reason.clone(),
            owner_agent: scored.procedure.owner_agent.clone(),
            success_count: scored.procedure.success_count,
            failure_count: scored.procedure.failure_count,
        })
        .collect::<Vec<_>>();
    let section = render_procedure_section(&selected_scored, request, max_chars);
    let selected_count = selections.len();
    let dropped_count = candidate_count.saturating_sub(selected_count);

    if request.emit_audit && (!selections.is_empty() || candidate_count > 0) {
        emit_retrieval_event(
            store,
            scope,
            request,
            candidate_count,
            dropped_count,
            &selections,
            LearningProcedurePromptRetrievalBackend::Direct,
            None,
        );
    }

    Ok(LearningProcedurePromptRenderResult {
        section,
        selected: selections,
        candidate_count,
        dropped_count,
        retrieval_backend: LearningProcedurePromptRetrievalBackend::Direct,
    })
}

pub async fn render_active_procedures_for_prompt_with_hybrid(
    store: &LearningStore,
    scope: &LearningScope,
    request: &LearningProcedureRenderRequest<'_>,
) -> Result<LearningProcedurePromptRenderResult> {
    let max_entries = request.max_entries.max(1);
    let max_chars = request.max_chars.max(500);
    let procedures = store.list_procedures(
        scope,
        LearningProcedureFilters {
            status: Some(LearningProcedureStatus::Active.as_str().to_string()),
            owner_agent: None,
            limit: None,
        },
    )?;
    let candidate_count = procedures.len();
    let procedure_index_stale = queue_procedure_index_refresh_if_stale(store, scope, &procedures);
    let visible = procedures
        .into_iter()
        .filter(|procedure| procedure_visible_to_agent(procedure, request.agent_id))
        .collect::<Vec<_>>();

    let (mut scored, retrieval_backend, fallback_reason) = if procedure_index_stale {
        (
            score_procedures_direct(visible, request.relevance_query),
            LearningProcedurePromptRetrievalBackend::DirectFallback,
            Some("procedure_index_stale".to_string()),
        )
    } else {
        match score_procedures_with_lancedb_hybrid(store, scope, &visible, request).await {
            Ok(scored) => (
                scored,
                LearningProcedurePromptRetrievalBackend::LancedbHybrid,
                None,
            ),
            Err(error) => (
                score_procedures_direct(visible, request.relevance_query),
                LearningProcedurePromptRetrievalBackend::DirectFallback,
                Some(format!("{error:#}")),
            ),
        }
    };
    let actor = request.agent_id.unwrap_or("*");
    match fallback_reason.as_deref() {
        Some(reason) => note_retrieval_fallback_episode(
            "procedure_prompt_hybrid",
            &scope.principal,
            &scope.workspace,
            actor,
            reason,
        ),
        None => note_retrieval_recovery_episode(
            "procedure_prompt_hybrid",
            &scope.principal,
            &scope.workspace,
            actor,
        ),
    }

    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                right
                    .procedure
                    .success_count
                    .cmp(&left.procedure.success_count)
            })
            .then_with(|| {
                left.procedure
                    .failure_count
                    .cmp(&right.procedure.failure_count)
            })
            .then_with(|| right.procedure.updated_at.cmp(&left.procedure.updated_at))
            .then_with(|| left.procedure.id.cmp(&right.procedure.id))
    });

    let selected_scored = scored.into_iter().take(max_entries).collect::<Vec<_>>();
    let selections = selected_scored
        .iter()
        .map(|scored| LearningProcedurePromptSelection {
            procedure_id: scored.procedure.id.clone(),
            title: scored.procedure.title.clone(),
            score: scored.score,
            reason: scored.reason.clone(),
            owner_agent: scored.procedure.owner_agent.clone(),
            success_count: scored.procedure.success_count,
            failure_count: scored.procedure.failure_count,
        })
        .collect::<Vec<_>>();
    let section = render_procedure_section(&selected_scored, request, max_chars);
    let selected_count = selections.len();
    let dropped_count = candidate_count.saturating_sub(selected_count);

    if request.emit_audit && (!selections.is_empty() || candidate_count > 0) {
        emit_retrieval_event(
            store,
            scope,
            request,
            candidate_count,
            dropped_count,
            &selections,
            retrieval_backend,
            fallback_reason.as_deref(),
        );
    }

    Ok(LearningProcedurePromptRenderResult {
        section,
        selected: selections,
        candidate_count,
        dropped_count,
        retrieval_backend,
    })
}

fn procedure_visible_to_agent(procedure: &LearningProcedure, agent_id: Option<&str>) -> bool {
    let Some(owner_agent) = procedure.owner_agent.as_deref() else {
        return true;
    };
    agent_id
        .map(|agent_id| owner_agent == agent_id)
        .unwrap_or(false)
}

fn score_procedures_direct(
    procedures: Vec<LearningProcedure>,
    relevance_query: &str,
) -> Vec<ScoredProcedure> {
    procedures
        .into_iter()
        .filter_map(|procedure| score_procedure(procedure, relevance_query))
        .collect()
}

async fn score_procedures_with_lancedb_hybrid(
    store: &LearningStore,
    scope: &LearningScope,
    procedures: &[LearningProcedure],
    request: &LearningProcedureRenderRequest<'_>,
) -> Result<Vec<ScoredProcedure>> {
    if procedures.is_empty() || request.relevance_query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query_tokens = tokenize(request.relevance_query);
    if query_tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored_by_id = score_procedures_direct(procedures.to_vec(), request.relevance_query)
        .into_iter()
        .map(|scored| (scored.procedure.id.clone(), scored))
        .collect::<BTreeMap<_, _>>();
    let table = procedure_vector_table(store, scope);
    if !table.exists().await? {
        queue_procedure_index_refresh(store, scope);
        return Ok(scored_by_id.into_values().collect());
    }

    let embedder = OllamaEmbedder::from_env();
    let limit = procedures
        .len()
        .max(request.max_entries.max(1).saturating_mul(8).max(24))
        .min(MAX_PROCEDURE_HYBRID_SEARCH_RESULTS);
    let visible_ids = procedures
        .iter()
        .map(|procedure| procedure.id.clone())
        .collect::<Vec<_>>();
    let hits = table
        .search_hybrid_ids(&embedder, request.relevance_query, limit, &visible_ids)
        .await?;
    let procedures_by_id = procedures
        .iter()
        .map(|procedure| (procedure.id.as_str(), procedure))
        .collect::<BTreeMap<_, _>>();
    for hit in hits {
        let Some(procedure) = procedures_by_id.get(hit.id.as_str()) else {
            continue;
        };
        if let Some(direct_score) = scored_by_id.get_mut(&hit.id) {
            let hybrid_boost = (hit.score.max(0.0) * PROCEDURE_HYBRID_SCORE_SCALE).round() as i64;
            direct_score.score = direct_score.score.saturating_add(hybrid_boost);
            direct_score.reason = format!(
                "{}; LanceDB hybrid_score={:.4}",
                direct_score.reason, hit.score
            );
        } else if let Some(hybrid_score) = score_procedure_from_hybrid_hit(
            (**procedure).clone(),
            request.relevance_query,
            hit.score,
        ) {
            scored_by_id.insert(hit.id, hybrid_score);
        };
    }
    Ok(scored_by_id.into_values().collect())
}

pub(super) fn procedure_hybrid_text(procedure: &LearningProcedure) -> String {
    let mut text = String::new();
    text.push_str("Title: ");
    text.push_str(&procedure.title);
    text.push('\n');
    text.push_str("Summary: ");
    text.push_str(&procedure.summary);
    text.push('\n');
    push_indexed_list_text(&mut text, "Use when", &procedure.activation.use_when);
    push_indexed_list_text(
        &mut text,
        "Example goals",
        &procedure.activation.example_goals,
    );
    push_indexed_list_text(&mut text, "Avoid when", &procedure.activation.avoid_when);
    push_indexed_list_text(&mut text, "Workflow", &procedure.workflow);
    push_indexed_list_text(&mut text, "Decision points", &procedure.decision_points);
    push_indexed_list_text(&mut text, "Verification", &procedure.verification);
    push_indexed_list_text(&mut text, "Failure modes", &procedure.failure_modes);
    text
}

fn push_indexed_list_text(text: &mut String, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    text.push_str(label);
    text.push_str(": ");
    text.push_str(
        &values
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("; "),
    );
    text.push('\n');
}

fn score_procedure(procedure: LearningProcedure, relevance_query: &str) -> Option<ScoredProcedure> {
    let query_tokens = tokenize(relevance_query);
    if query_tokens.is_empty() {
        return None;
    }

    let mut matched_tokens = BTreeSet::new();
    let mut selected_match_count = 0usize;
    let mut phrase_match_count = 0usize;
    let mut high_signal_match_count = 0usize;
    let mut score = 0i64;
    let mut reasons = Vec::new();

    let mut add_field_score = |label: &str, weight: i64, high_signal: bool, values: &[String]| {
        let (matches, field_score, phrase_bonus) = score_text_values(&query_tokens, values, weight);
        if !matches.is_empty() {
            selected_match_count += matches.len();
            if high_signal {
                high_signal_match_count += matches.len();
            }
            matched_tokens.extend(matches.iter().cloned());
            score += field_score + phrase_bonus;
            if phrase_bonus > 0 {
                phrase_match_count += 1;
            }
            let listed = matches.into_iter().take(5).collect::<Vec<_>>().join(", ");
            reasons.push(format!("{label} matched {listed}"));
        }
    };

    add_field_score("title", 3, true, std::slice::from_ref(&procedure.title));
    add_field_score("summary", 2, true, std::slice::from_ref(&procedure.summary));
    add_field_score("use_when", 5, true, &procedure.activation.use_when);
    add_field_score(
        "example_goals",
        5,
        true,
        &procedure.activation.example_goals,
    );
    add_field_score("workflow", 1, false, &procedure.workflow);
    add_field_score("decision_points", 1, false, &procedure.decision_points);
    add_field_score("verification", 1, false, &procedure.verification);

    let avoid_matches = score_text_values(&query_tokens, &procedure.activation.avoid_when, 4);
    let avoid_match_count = avoid_matches.0.len();
    if avoid_match_count > 0 {
        if avoid_match_count >= 2 || avoid_matches.2 > 0 {
            return None;
        }
        score -= avoid_matches.1 + avoid_matches.2 + 6;
        reasons.push(format!(
            "avoid_when matched {}",
            avoid_matches
                .0
                .into_iter()
                .take(5)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let harmful_feedback = payload_indicates_harmful_feedback(&procedure.payload)
        || (procedure.failure_count > procedure.success_count && procedure.failure_count > 0);
    if harmful_feedback {
        score -= 10;
        reasons.push("downranked due to negative procedure feedback/failures".to_string());
    }

    let relevant_enough =
        matched_tokens.len() >= 2 && (high_signal_match_count > 0 || phrase_match_count > 0);
    if !relevant_enough || score < MIN_RELEVANCE_SCORE {
        return None;
    }

    Some(ScoredProcedure {
        procedure,
        score,
        reason: if reasons.is_empty() {
            "selected by reusable procedure relevance".to_string()
        } else {
            reasons.join("; ")
        },
        selected_match_count,
        avoid_match_count,
        harmful_feedback,
    })
}

fn score_procedure_from_hybrid_hit(
    procedure: LearningProcedure,
    relevance_query: &str,
    hybrid_score: f32,
) -> Option<ScoredProcedure> {
    let query_tokens = tokenize(relevance_query);
    if query_tokens.is_empty() || hybrid_score <= 0.0 {
        return None;
    }

    let avoid_matches = score_text_values(&query_tokens, &procedure.activation.avoid_when, 4);
    let avoid_match_count = avoid_matches.0.len();
    if avoid_match_count >= 2 || avoid_matches.2 > 0 {
        return None;
    }

    let harmful_feedback = payload_indicates_harmful_feedback(&procedure.payload)
        || (procedure.failure_count > procedure.success_count && procedure.failure_count > 0);
    let mut score = (hybrid_score.max(0.0) * PROCEDURE_HYBRID_SCORE_SCALE).round() as i64;
    score = score.saturating_add(procedure.success_count.min(10) as i64);
    score = score.saturating_sub(procedure.failure_count.min(10) as i64);

    let mut reasons = vec![format!(
        "selected by LanceDB semantic procedure retrieval; hybrid_score={hybrid_score:.4}"
    )];
    if avoid_match_count > 0 {
        score = score.saturating_sub(avoid_matches.1 + avoid_matches.2 + 6);
        reasons.push(format!(
            "avoid_when matched {}",
            avoid_matches
                .0
                .into_iter()
                .take(5)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if harmful_feedback {
        score = score.saturating_sub(10);
        reasons.push("downranked due to negative procedure feedback/failures".to_string());
    }

    if score <= 0 {
        return None;
    }

    Some(ScoredProcedure {
        procedure,
        score,
        reason: reasons.join("; "),
        selected_match_count: 0,
        avoid_match_count,
        harmful_feedback,
    })
}

fn score_text_values(
    query_tokens: &BTreeSet<String>,
    values: &[String],
    weight: i64,
) -> (BTreeSet<String>, i64, i64) {
    let mut matches = BTreeSet::new();
    let mut phrase_bonus = 0i64;
    for value in values {
        let value_tokens = tokenize(value);
        for token in query_tokens.intersection(&value_tokens) {
            matches.insert(token.clone());
        }
        if normalized_phrase_matches(value, query_tokens) {
            phrase_bonus += weight * 2;
        }
    }
    let score = matches.len() as i64 * weight;
    (matches, score, phrase_bonus)
}

fn normalized_phrase_matches(value: &str, query_tokens: &BTreeSet<String>) -> bool {
    let tokens = tokenize(value);
    tokens.len() >= 2 && tokens.intersection(query_tokens).count() >= tokens.len().min(4)
}

fn render_procedure_section(
    selected: &[ScoredProcedure],
    request: &LearningProcedureRenderRequest<'_>,
    max_chars: usize,
) -> Option<String> {
    if selected.is_empty() {
        return None;
    }
    let mut section = String::from(
        "## Relevant Reusable Procedures\n\
         <relevant_procedures>\n\
         These active procedures were retrieved for the current goal. Treat them as compact operating guidance, not as proof that the goal is already complete.\n",
    );
    for (idx, scored) in selected.iter().enumerate() {
        let procedure = &scored.procedure;
        section.push_str(&format!(
            "\n### {}. {} (`{}`)\n",
            idx + 1,
            sanitize_inline(&procedure.title),
            sanitize_inline(&procedure.id)
        ));
        if request.include_rationale {
            section.push_str(&format!(
                "Retrieval rationale: score={}, matches={}, avoid_matches={}, harmful_downrank={}, reason={}\n",
                scored.score,
                scored.selected_match_count,
                scored.avoid_match_count,
                scored.harmful_feedback,
                sanitize_inline(&scored.reason)
            ));
        }
        if let Some(owner) = procedure.owner_agent.as_deref() {
            section.push_str(&format!("Owner agent: `{}`\n", sanitize_inline(owner)));
        }
        push_labeled_text(&mut section, "Summary", &procedure.summary);
        push_labeled_list(&mut section, "Use when", &procedure.activation.use_when);
        push_labeled_list(&mut section, "Avoid when", &procedure.activation.avoid_when);
        push_labeled_list(&mut section, "Workflow", &procedure.workflow);
        push_labeled_list(&mut section, "Decision points", &procedure.decision_points);
        push_labeled_list(&mut section, "Verify", &procedure.verification);
        push_labeled_list(
            &mut section,
            "Known failure modes",
            &procedure.failure_modes,
        );
        if procedure.success_count > 0 || procedure.failure_count > 0 {
            section.push_str(&format!(
                "Observed outcomes: success_count={}, failure_count={}\n",
                procedure.success_count, procedure.failure_count
            ));
        }
        if section.chars().count() > max_chars {
            section = truncate_chars(&section, max_chars);
            section.push_str("\n...[truncated]");
            break;
        }
    }
    section.push_str("\n</relevant_procedures>");
    Some(section)
}

fn push_labeled_text(section: &mut String, label: &str, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    section.push_str(label);
    section.push_str(": ");
    section.push_str(&sanitize_inline(&truncate_chars(trimmed, MAX_FIELD_CHARS)));
    section.push('\n');
}

fn push_labeled_list(section: &mut String, label: &str, values: &[String]) {
    let rendered = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .take(6)
        .map(|value| sanitize_inline(&truncate_chars(value, MAX_FIELD_CHARS)))
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        return;
    }
    section.push_str(label);
    section.push_str(":\n");
    for value in rendered {
        section.push_str("- ");
        section.push_str(&value);
        section.push('\n');
    }
}

fn emit_retrieval_event(
    store: &LearningStore,
    scope: &LearningScope,
    request: &LearningProcedureRenderRequest<'_>,
    candidate_count: usize,
    dropped_count: usize,
    selections: &[LearningProcedurePromptSelection],
    retrieval_backend: LearningProcedurePromptRetrievalBackend,
    fallback_reason: Option<&str>,
) {
    let summary = if selections.is_empty() {
        "No active reusable procedures selected for prompt injection".to_string()
    } else {
        format!(
            "Selected {} active reusable procedure(s) for prompt injection",
            selections.len()
        )
    };
    if let Err(error) = store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_procedure_retrieval_rendered".to_string(),
            agent_id: request.agent_id.map(str::to_string),
            task_id: request.task_id.map(str::to_string),
            execution_id: request.execution_id.map(str::to_string),
            chat_session_id: request.chat_session_id.map(str::to_string),
            summary,
            evidence_refs: Vec::new(),
            payload: json!({
                "query_excerpt": one_line_excerpt(request.relevance_query, MAX_QUERY_EXCERPT_CHARS),
                "candidate_count": candidate_count,
                "selected_count": selections.len(),
                "dropped_count": dropped_count,
                "max_entries": request.max_entries,
                "max_chars": request.max_chars,
                "retrieval_backend": retrieval_backend.as_str(),
                "fallback_reason": fallback_reason,
                "selected": selections,
            }),
        },
    ) {
        warn!(
            principal = %scope.principal,
            workspace = %scope.workspace,
            error = %error,
            "Failed to append learning procedure retrieval audit event"
        );
    }
}

fn payload_indicates_harmful_feedback(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(v) => *v,
        serde_json::Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("never_do_this")
                || lower.contains("this_was_wrong")
                || lower.contains("harmful")
                || lower.contains("do_not_use")
                || lower.contains("do not use")
        },
        serde_json::Value::Array(items) => items.iter().any(payload_indicates_harmful_feedback),
        serde_json::Value::Object(map) => {
            if map
                .get("harmful_feedback")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                return true;
            }
            map.iter().any(|(key, value)| {
                let key = key.to_ascii_lowercase();
                let key_is_feedback = key.contains("negative")
                    || key.contains("harmful")
                    || key.contains("wrong")
                    || key.contains("feedback")
                    || key.contains("teaching")
                    || key.contains("action");
                if key_is_feedback && payload_indicates_harmful_feedback(value) {
                    return true;
                }
                matches!(
                    value,
                    serde_json::Value::Array(_) | serde_json::Value::Object(_)
                ) && payload_indicates_harmful_feedback(value)
            })
        },
        _ => false,
    }
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 3)
        .filter(|token| !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

fn sanitize_inline(value: &str) -> String {
    neutralize_boundary_tags(value)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn one_line_excerpt(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&compact, max_chars)
}

const STOP_WORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "into",
    "onto",
    "over",
    "under",
    "when",
    "then",
    "than",
    "your",
    "you",
    "are",
    "was",
    "were",
    "have",
    "has",
    "had",
    "not",
    "but",
    "all",
    "any",
    "each",
    "need",
    "needs",
    "using",
    "use",
    "used",
    "task",
    "goal",
    "please",
    "current",
    "agent",
    "user",
    "work",
    "done",
    "make",
    "find",
    "create",
    "update",
    "check",
    "run",
    "execute",
    "complete",
    "completion",
    "should",
    "would",
    "could",
    "about",
    "after",
    "before",
    "while",
    "where",
    "what",
    "which",
    "there",
    "their",
    "them",
    "they",
    "will",
    "just",
    "only",
    "same",
    "page",
    "file",
    "data",
];

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tempfile::TempDir;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    use super::super::CreateLearningProcedureRequest;
    use super::*;

    fn store() -> (TempDir, LearningStore, LearningScope) {
        let dir = TempDir::new().unwrap();
        let workspace = ArtifactV2Workspace::new(dir.path());
        let store = LearningStore::new(workspace);
        let scope = LearningScope::new("anonymous", "default");
        (dir, store, scope)
    }

    fn create_active_procedure(
        store: &LearningStore,
        scope: &LearningScope,
        id: &str,
        title: &str,
        use_when: &[&str],
    ) -> LearningProcedure {
        let procedure = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some(id.to_string()),
                    actor: "test".to_string(),
                    reason: Some("test fixture".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: title.to_string(),
                    summary: format!("Reusable procedure for {title}"),
                    owner_agent: None,
                    activation: super::super::LearningProcedureActivation {
                        use_when: use_when.iter().map(|value| value.to_string()).collect(),
                        avoid_when: Vec::new(),
                        example_goals: Vec::new(),
                    },
                    workflow: vec!["Inspect the source evidence.".to_string()],
                    decision_points: Vec::new(),
                    verification: vec!["Verify the final output against the source.".to_string()],
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 1,
                    failure_count: 0,
                    payload: json!({}),
                },
            )
            .unwrap();
        store
            .transition_procedure_status(
                scope,
                &procedure.id,
                LearningProcedureStatus::Active,
                "test",
                "activated",
                "test fixture",
                Vec::new(),
            )
            .unwrap()
    }

    #[test]
    fn renders_relevant_active_procedure_with_rationale() {
        let (_dir, store, scope) = store();
        create_active_procedure(
            &store,
            &scope,
            "extract-handwritten-pdf",
            "Extract handwritten PDF questions",
            &["extract handwritten text from scanned PDF question papers"],
        );
        create_active_procedure(
            &store,
            &scope,
            "send-whatsapp",
            "Send WhatsApp reaction",
            &["react to whatsapp messages"],
        );

        let result = render_active_procedures_for_prompt(
            &store,
            &scope,
            &LearningProcedureRenderRequest::for_goal(
                "Extract handwritten text from a scanned PDF and recreate the question paper",
            )
            .with_agent(Some("personal-assistant")),
        )
        .unwrap();

        let section = result.section.expect("procedure section");
        assert!(section.contains("extract-handwritten-pdf"));
        assert!(!section.contains("send-whatsapp"));
        assert!(section.contains("Retrieval rationale"));
        assert_eq!(result.selected.len(), 1);
    }

    #[tokio::test]
    async fn chat_context_retrieval_stale_index_uses_no_provider() {
        let (_dir, store, scope) = store();
        create_active_procedure(
            &store,
            &scope,
            "extract-handwritten-pdf",
            "Extract handwritten PDF questions",
            &["extract handwritten text from scanned PDF question papers"],
        );

        let result = render_active_procedures_for_prompt_with_hybrid(
            &store,
            &scope,
            &LearningProcedureRenderRequest::for_goal(
                "Extract handwritten text from a scanned PDF and recreate the question paper",
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.selected.len(), 1);
        assert_eq!(result.selected[0].procedure_id, "extract-handwritten-pdf");
        assert_eq!(
            result.retrieval_backend,
            LearningProcedurePromptRetrievalBackend::DirectFallback
        );
        assert!(result.section.unwrap().contains("Retrieval rationale"));
    }

    #[test]
    fn does_not_select_procedure_from_one_repeated_keyword() {
        let (_dir, store, scope) = store();
        let procedure = store
            .create_procedure(
                scope.clone(),
                CreateLearningProcedureRequest {
                    principal: None,
                    workspace: None,
                    id: Some("weekly-report".to_string()),
                    actor: "test".to_string(),
                    reason: Some("test fixture".to_string()),
                    status: LearningProcedureStatus::Draft,
                    title: "Weekly weekly".to_string(),
                    summary: "Weekly weekly".to_string(),
                    owner_agent: None,
                    activation: super::super::LearningProcedureActivation {
                        use_when: vec!["weekly weekly".to_string()],
                        avoid_when: Vec::new(),
                        example_goals: vec!["weekly weekly".to_string()],
                    },
                    workflow: vec!["Weekly weekly".to_string()],
                    decision_points: Vec::new(),
                    verification: Vec::new(),
                    failure_modes: Vec::new(),
                    evidence_refs: Vec::new(),
                    source_candidate_id: None,
                    source_task_ids: Vec::new(),
                    source_chat_session_ids: Vec::new(),
                    success_count: 3,
                    failure_count: 0,
                    payload: json!({}),
                },
            )
            .unwrap();
        store
            .transition_procedure_status(
                &scope,
                &procedure.id,
                LearningProcedureStatus::Active,
                "test",
                "activated",
                "test fixture",
                Vec::new(),
            )
            .unwrap();

        let result = render_active_procedures_for_prompt(
            &store,
            &scope,
            &LearningProcedureRenderRequest::for_goal("weekly"),
        )
        .unwrap();

        assert!(
            result.section.is_none(),
            "a single repeated token must not activate reusable procedure injection"
        );
        assert!(result.selected.is_empty());
        let events = store.list_events(&scope, 20).unwrap();
        let retrieval_event = events
            .iter()
            .find(|event| event.event_type == "learning_procedure_retrieval_rendered")
            .expect("withheld retrieval audit event");
        assert_eq!(
            retrieval_event
                .payload
                .get("selected_count")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
    }

    #[test]
    fn semantic_hybrid_hit_can_select_without_lexical_activation_match() {
        let (_dir, store, scope) = store();
        let procedure = create_active_procedure(
            &store,
            &scope,
            "scan-rebuild-document",
            "OCR reconstruction",
            &["extract handwritten answers from scanned documents"],
        );

        let scored = score_procedure_from_hybrid_hit(procedure, "prepare invoice packet", 0.42)
            .expect("semantic hybrid hit should be enough when avoid_when does not block it");

        assert_eq!(scored.selected_match_count, 0);
        assert!(scored.reason.contains("LanceDB semantic"));
    }

    #[test]
    fn semantic_hybrid_hit_respects_strong_avoid_match() {
        let (_dir, store, scope) = store();
        let mut procedure = create_active_procedure(
            &store,
            &scope,
            "scan-rebuild-document",
            "OCR reconstruction",
            &["extract handwritten answers from scanned documents"],
        );
        procedure.activation.avoid_when = vec!["prepare invoice".to_string()];

        assert!(
            score_procedure_from_hybrid_hit(procedure, "prepare invoice packet", 0.42).is_none()
        );
    }

    #[test]
    fn detects_harmful_feedback_payload_under_teaching_action() {
        assert!(payload_indicates_harmful_feedback(&json!({
            "teaching_action": "this_was_wrong"
        })));
        assert!(payload_indicates_harmful_feedback(&json!({
            "feedback": { "action": "never_do_this" }
        })));
    }

    #[test]
    fn avoids_owner_mismatched_procedure() {
        let procedure = LearningProcedure {
            id: "owned".to_string(),
            scope: LearningScope::new("anonymous", "default"),
            status: LearningProcedureStatus::Active,
            title: "Owned workflow".to_string(),
            summary: String::new(),
            owner_agent: Some("other-agent".to_string()),
            activation: Default::default(),
            workflow: Vec::new(),
            decision_points: Vec::new(),
            verification: Vec::new(),
            failure_modes: Vec::new(),
            evidence_refs: Vec::new(),
            source_candidate_id: None,
            source_task_ids: Vec::new(),
            source_chat_session_ids: Vec::new(),
            success_count: 0,
            failure_count: 0,
            payload: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_used_at: None,
            version: 1,
        };
        assert!(!procedure_visible_to_agent(
            &procedure,
            Some("personal-assistant")
        ));
    }
}
