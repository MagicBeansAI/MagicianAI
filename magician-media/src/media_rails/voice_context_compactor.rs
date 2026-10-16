//! Voice context compactor — summarises chat-ledger turns into a
//! short "summary so far" snippet that the transport replays after
//! every upstream rotation.
//!
//! ## Why summarise at all
//!
//! Every upstream rotation (proactive at ~28 min, watermark crossing,
//! reconnect after drop) starts a *fresh* provider session with no
//! conversation context. To make the user experience seamless, the
//! frontend transport replays context via `conversation.item.create`
//! frames before resuming live audio. Replaying 200 raw turns each
//! time wastes input tokens, slows the reconnect, and (over multiple
//! rotations) compounds context bloat across rotations.
//!
//! Instead we compress the *older* portion of the conversation into a
//! short summary and only replay the *recent* turns verbatim. The
//! summary captures facts, decisions, and tasks created — losing
//! conversational nuance but keeping the substance the next turn is
//! likely to refer back to.
//!
//! ## Incremental compaction
//!
//! `VoiceSessionLifecycle.compacted_summary` + `last_compacted_turn_id`
//! let us run the LLM *only on the new turns since the last
//! compaction*. The prompt embeds the prior summary so the model
//! extends it rather than re-summarising from scratch. This keeps
//! per-rotation latency low (the LLM sees ~20 new turns + 150 tokens
//! of prior summary, not the full history).
//!
//! ## Provider-neutral
//!
//! The compactor outputs a single text summary. The transport adapts
//! it to the provider's native shape:
//!   * OpenAI: one `conversation.item.create` with `role: "system"`
//!   * Gemini: included in `setup.systemInstruction` on resume
//!
//! ## Failure semantics
//!
//! If the LLM call fails, the compactor returns the prior summary
//! unchanged plus the new turns concatenated verbatim. The transport
//! replays whatever it gets — degraded quality is preferable to
//! aborting the rotation. The error is logged for ops follow-up.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::media_rails::{
    VoiceSessionLifecycleStore, MEDIA_SYSTEM_AGENT, MEDIA_VOICE_SESSION_COMPACTION,
};
use magician::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use magician::magician_v2::chat::models::{
    ChatLlmTranscriptEntry, ChatMessage, ChatMessageContent, ChatMessageDirection,
};
use magician::magician_v2::chat::storage::ChatStore;
use magician::magician_v2::prompts::{constants, PromptManager};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// Default number of turns to keep verbatim alongside the summary.
/// Overridable per call via `RealtimeVoiceProfile.verbatim_recent_turns`.
/// 12 ≈ 60 s of conversation at typical density — enough for the
/// model to follow the immediate thread without trailing context.
pub const DEFAULT_VERBATIM_RECENT_TURNS: usize = 12;

/// Default soft cap on the compactor's input. Overridable per call
/// via `RealtimeVoiceProfile.compaction_input_turn_limit`. If the
/// chat ledger somehow holds more than this we still only summarise
/// the most recent uncompacted slice (older ones fall off — degraded
/// but bounded).
pub const DEFAULT_COMPACTION_INPUT_TURN_LIMIT: usize = 200;
const MIN_VOICE_TOOL_TRANSCRIPT_TAIL_ENTRIES: usize = 64;
const MAX_VOICE_TOOL_TRANSCRIPT_TAIL_ENTRIES: usize = 2_000;

/// One turn surfaced to the transport for replay. The transport
/// translates `role` to the provider's native shape (OpenAI's
/// `"user"`/`"assistant"` content items, Gemini's `parts` array,
/// etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayTurn {
    /// `"user"` or `"assistant"`. System role is reserved for the
    /// compacted summary.
    pub role: String,
    /// Plain-text body of the turn — voice transcripts only carry
    /// text, no images, so this is sufficient for replay.
    pub text: String,
    /// Chat-ledger message id this replay turn was derived from.
    /// Used as the high-water mark for incremental compaction.
    pub message_id: String,
    /// Original creation timestamp (ms epoch). Replayed turns are
    /// fed in chronological order to preserve the conversation flow.
    pub created_at_ms: i64,
}

/// One balanced projected tool exchange retained across a realtime provider
/// rotation. The result is the bounded model projection, never the raw blob.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayToolExchange {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub projected_result: Value,
}

/// Output of `compact_for_resume`. Fed directly into the transport's
/// reconnect path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeContext {
    /// Compacted summary of everything older than `recent_turns`.
    /// `None` when the conversation is short enough that no
    /// summarisation has happened yet.
    pub summary: Option<String>,
    /// The most recent N turns, in chronological order. Always
    /// replayed verbatim so the model has fresh recent context.
    pub recent_turns: Vec<ReplayTurn>,
    /// Recent versioned tool exchanges. A fresh provider can replay the
    /// original call id and its exactly-one matching projected result without
    /// re-executing the tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_exchanges: Vec<ReplayToolExchange>,
    /// Total turn count in the chat ledger after compaction. Useful
    /// for telemetry / debugging long calls.
    pub total_turns: usize,
}

/// Dependencies the compactor needs. Constructed once on startup and
/// reused across rotations. `Arc`-shared so the resume-context
/// handler can call the compactor without owning the orchestrator's
/// LLM router exclusively.
#[derive(Clone)]
pub struct VoiceContextCompactor {
    chat_store: Arc<dyn ChatStore>,
    lifecycle_store: Arc<VoiceSessionLifecycleStore>,
    operation_router: OperationLlmRouter,
    prompt_manager: Arc<PromptManager>,
    /// Optional broadcaster — when present, every successful
    /// compaction emits a `media.voice.session.compaction` event so
    /// observability dashboards can chart compaction frequency / size
    /// without re-deriving from logs. `None` in tests + when the
    /// caller doesn't have a broadcaster handy (no events, but
    /// compaction still works).
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    /// Per-deployment knobs sourced from the realtime voice profile.
    /// `None` falls back to the module-level defaults.
    verbatim_recent_turns: Option<usize>,
    compaction_input_turn_limit: Option<usize>,
}

impl VoiceContextCompactor {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        lifecycle_store: Arc<VoiceSessionLifecycleStore>,
        operation_router: OperationLlmRouter,
        prompt_manager: Arc<PromptManager>,
    ) -> Self {
        Self {
            chat_store,
            lifecycle_store,
            operation_router,
            prompt_manager,
            broadcaster: None,
            verbatim_recent_turns: None,
            compaction_input_turn_limit: None,
        }
    }

    pub fn with_broadcaster(mut self, broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Apply per-deployment knobs from the realtime voice profile.
    /// Called once at startup with values pulled from
    /// `magician-config.yaml > realtime_voice.profiles.<name>`.
    /// `None` for either field keeps the module-level default.
    pub fn with_profile_overrides(
        mut self,
        verbatim_recent_turns: Option<usize>,
        compaction_input_turn_limit: Option<usize>,
    ) -> Self {
        self.verbatim_recent_turns = verbatim_recent_turns;
        self.compaction_input_turn_limit = compaction_input_turn_limit;
        self
    }

    /// Build a resume context for the given voice session. Reads the
    /// chat ledger, splits into "older → summarise" + "recent →
    /// verbatim", calls the LLM if new turns to summarise exist, and
    /// updates the lifecycle store's cached summary.
    ///
    /// Returns the assembled `ResumeContext`. Errors propagate from
    /// the chat store (genuine I/O / database issues); LLM failures
    /// are swallowed inside the function with a logged warning and a
    /// best-effort fallback summary.
    pub async fn compact_for_resume(
        &self,
        voice_session_id: &str,
        chat_session_id: &str,
        principal: &str,
        workspace: &str,
    ) -> Result<ResumeContext> {
        // Fetch conversational turns in chronological order, excluding
        // cross-session display projections before applying the limit.
        let limit = self
            .compaction_input_turn_limit
            .unwrap_or(DEFAULT_COMPACTION_INPUT_TURN_LIMIT);
        let verbatim = self
            .verbatim_recent_turns
            .unwrap_or(DEFAULT_VERBATIM_RECENT_TURNS);
        let messages = self
            .chat_store
            .get_context_messages(chat_session_id, limit)
            .await?;
        let total_turns = messages.len();
        // Tool replay is explicitly recent context. Bound storage I/O to the
        // same neighborhood as the display-turn compaction window instead of
        // decoding an unbounded exact provider transcript merely to retain a
        // handful of completed exchanges.
        let tool_history_limit = limit.max(verbatim).saturating_mul(4).clamp(
            MIN_VOICE_TOOL_TRANSCRIPT_TAIL_ENTRIES,
            MAX_VOICE_TOOL_TRANSCRIPT_TAIL_ENTRIES,
        );
        let tool_exchanges = match self
            .chat_store
            .get_llm_history_tail(chat_session_id, tool_history_limit)
            .await
        {
            Ok(history) => projected_tool_exchanges(&history, verbatim),
            Err(error) => {
                warn!(
                    chat_session_id,
                    error = %error,
                    "[VOICE-COMPACTOR] projected tool transcript unavailable during resume"
                );
                Vec::new()
            },
        };

        // Convert to ReplayTurn shape, drop anything that isn't a
        // user/assistant Text message (status updates, escalations,
        // tool results — none of which belong in the model's
        // conversation replay).
        let mut ordered: Vec<ReplayTurn> = messages
            .into_iter()
            .filter_map(replay_turn_from_message)
            .collect();

        // Split: tail = recent verbatim turns; head = older turns we
        // may compress into the summary.
        let split_at = ordered.len().saturating_sub(verbatim);
        let older: Vec<ReplayTurn> = ordered.drain(..split_at).collect();
        let recent_turns: Vec<ReplayTurn> = ordered;

        // Pull cached state and decide whether we need to extend the
        // summary at all. Cheap path: no new older turns + cached
        // summary present → return it as-is.
        let cached = self.lifecycle_store.get(voice_session_id);
        let prior_summary = cached.as_ref().and_then(|c| c.compacted_summary.clone());
        let last_compacted_id = cached
            .as_ref()
            .and_then(|c| c.last_compacted_turn_id.clone());

        let new_older: Vec<&ReplayTurn> = older
            .iter()
            .filter(|t| match &last_compacted_id {
                Some(id) => t.message_id != *id && older_than_marker(&older, &t.message_id, id),
                None => true,
            })
            .collect();

        if new_older.is_empty() {
            return Ok(ResumeContext {
                summary: prior_summary,
                recent_turns,
                tool_exchanges,
                total_turns,
            });
        }

        let summary = match self
            .summarise(
                prior_summary.as_deref(),
                &older,
                principal,
                workspace,
                voice_session_id,
                chat_session_id,
            )
            .await
        {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    voice_session_id = %voice_session_id,
                    error = %err,
                    "[VOICE-COMPACTOR] LLM summarisation failed; falling back to concat",
                );
                fallback_concat_summary(prior_summary.as_deref(), &older)
            },
        };

        let last_id = older.last().map(|t| t.message_id.clone());
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.lifecycle_store.update_summary(
            voice_session_id,
            summary.clone(),
            last_id.clone(),
            now_ms,
        );

        if let Some(broadcaster) = &self.broadcaster {
            broadcaster.emit_named(
                MEDIA_VOICE_SESSION_COMPACTION,
                MEDIA_SYSTEM_AGENT,
                Some(principal),
                Some(workspace),
                serde_json::json!({
                    "voice_session_id": voice_session_id,
                    "summary_len": summary.len(),
                    "turns_summarised": new_older.len(),
                    "total_turns": total_turns,
                    "last_compacted_turn_id": last_id,
                }),
            );
        }

        Ok(ResumeContext {
            summary: Some(summary),
            recent_turns,
            tool_exchanges,
            total_turns,
        })
    }

    /// Call the LLM to produce (or extend) the summary. Both prompts
    /// are loaded through PromptManager so they're versioned + iterable
    /// from `data/magician_v2/prompts/` (canonical pattern, same as
    /// every other magician prompt). Routed via `MemoryArchiveSummary`
    /// — the closest existing LLM operation ("summarise episodes for
    /// archival") so the small/fast memory-consolidation profile is
    /// picked up without extending the operation-mapping config.
    async fn summarise(
        &self,
        prior_summary: Option<&str>,
        older_turns: &[ReplayTurn],
        principal: &str,
        workspace: &str,
        voice_session_id: &str,
        chat_session_id: &str,
    ) -> Result<String> {
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::VOICE_CONTEXT_COMPACTION_SYSTEM,
                constants::versions::VOICE_CONTEXT_COMPACTION_SYSTEM,
                HashMap::new(),
            )
            .await?;
        let mut user_vars: HashMap<String, String> = HashMap::new();
        user_vars.insert(
            "prior_summary_block".to_string(),
            render_prior_summary_block(prior_summary),
        );
        user_vars.insert(
            "transcript_block".to_string(),
            render_transcript_block(prior_summary.is_some(), older_turns),
        );
        let user_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::VOICE_CONTEXT_COMPACTION,
                constants::versions::VOICE_CONTEXT_COMPACTION,
                user_vars,
            )
            .await?;
        // Voice compaction has its own operation now (R2). Falls back
        // to MemoryArchiveSummary's routing when the config doesn't
        // map `voice_context_compaction` yet — keeps existing
        // deployments working until they add the mapping.
        let llm_started = std::time::Instant::now();
        let scoped_router = self
            .operation_router
            .with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));
        let response = scoped_router
            .generate_for_operation_with_system(
                &LLMOperation::VoiceContextCompaction,
                Some(&system_prompt),
                &user_prompt,
            )
            .await?;
        let text = response.content.trim().to_string();
        if let Some(broadcaster) = self.broadcaster.as_ref() {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                principal,
                workspace,
                "voice_context_compaction",
            );
            let attribution = OperationLlmCallAttribution {
                execution_id: Some(voice_session_id.to_string()),
                chat_session_id: Some(chat_session_id.to_string()),
                ..OperationLlmCallAttribution::default()
            };
            let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            if text.is_empty() {
                telemetry.emit_validation_failure(
                    LLMOperation::VoiceContextCompaction.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "voice_context_nonempty",
                    "compacted voice context was empty",
                );
            } else {
                telemetry.emit_validated_success(
                    LLMOperation::VoiceContextCompaction.as_str(),
                    &response,
                    latency_ms,
                    attribution,
                    "voice_context_nonempty",
                );
            }
        }
        debug!(
            "[VOICE-COMPACTOR] summary produced len={} prior_len={}",
            text.len(),
            prior_summary.map(str::len).unwrap_or(0)
        );
        Ok(text)
    }
}

fn projected_tool_exchanges(
    history: &[ChatLlmTranscriptEntry],
    limit: usize,
) -> Vec<ReplayToolExchange> {
    let (exchanges, stats) = projected_tool_exchanges_with_stats(history, limit);
    if stats.replay_count > 0 || stats.invalid_projection_count > 0 {
        tracing::info!(
            target: "magician::metrics::tool_result_projection_replay",
            surface = "realtime_resume",
            replay_count = stats.replay_count,
            invalid_projection_count = stats.invalid_projection_count,
            cumulative_raw_bytes = stats.cumulative_raw_bytes,
            cumulative_model_bytes = stats.cumulative_model_bytes,
            cumulative_estimated_model_tokens = stats.cumulative_estimated_model_tokens,
            cumulative_bytes_saved = stats.cumulative_bytes_saved,
            "tool_result_projection_provider_replay"
        );
    }
    exchanges
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ToolExchangeReplayStats {
    replay_count: u64,
    invalid_projection_count: u64,
    cumulative_raw_bytes: u64,
    cumulative_model_bytes: u64,
    cumulative_estimated_model_tokens: u64,
    cumulative_bytes_saved: u64,
}

fn projected_tool_exchanges_with_stats(
    history: &[ChatLlmTranscriptEntry],
    limit: usize,
) -> (Vec<ReplayToolExchange>, ToolExchangeReplayStats) {
    let mut pending = std::collections::HashMap::<String, (String, Value)>::new();
    let mut completed_call_ids = std::collections::HashSet::<String>::new();
    let mut candidates = Vec::new();
    let mut invalid_projection_count = 0_u64;
    for entry in history {
        match entry {
            ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } => {
                for call in tool_calls {
                    // Provider call ids are unique within a session. If a
                    // corrupt transcript repeats one, preserve the first call
                    // instead of silently rebinding a later result.
                    if !completed_call_ids.contains(&call.id) {
                        pending
                            .entry(call.id.clone())
                            .or_insert_with(|| (call.name.clone(), call.arguments.clone()));
                    }
                }
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id,
                projection,
                ..
            } => {
                let recorded_name = pending.get(tool_call_id).map(|(name, _)| name.as_str());
                if projection.validate_schema_version().is_err()
                    || projection.identity.tool_call_id.as_str() != tool_call_id
                    || recorded_name
                        .is_some_and(|name| projection.identity.tool_name.as_str() != name)
                {
                    invalid_projection_count = invalid_projection_count.saturating_add(1);
                    continue;
                }
                if let Some((recorded_name, arguments)) = pending.remove(tool_call_id) {
                    completed_call_ids.insert(tool_call_id.clone());
                    candidates.push((
                        ReplayToolExchange {
                            call_id: tool_call_id.clone(),
                            tool_name: recorded_name,
                            // Rotation may cross a backend or browser-owned
                            // transport. Re-apply the outbound guard here so
                            // the ResumeContext itself is safe; callers cannot
                            // accidentally bypass a provider-specific wrapper.
                            arguments:
                                magician::magician_v2::secrets::injection::sanitize_json_for_provider(
                                    &arguments,
                                ),
                            projected_result:
                                magician::magician_v2::tool_result_projection::provider_safe_model_value(
                                    projection,
                                ),
                        },
                        projection.metrics.raw_bytes,
                        projection.metrics.model_bytes,
                        projection.metrics.estimated_model_tokens,
                    ));
                }
            },
            _ => {},
        }
    }
    if candidates.len() > limit {
        candidates.drain(..candidates.len() - limit);
    }
    let mut stats = ToolExchangeReplayStats {
        invalid_projection_count,
        ..ToolExchangeReplayStats::default()
    };
    let exchanges = candidates
        .into_iter()
        .map(|(exchange, raw_bytes, model_bytes, estimated_tokens)| {
            let raw_bytes = u64::try_from(raw_bytes).unwrap_or(u64::MAX);
            let model_bytes = u64::try_from(model_bytes).unwrap_or(u64::MAX);
            stats.replay_count = stats.replay_count.saturating_add(1);
            stats.cumulative_raw_bytes = stats.cumulative_raw_bytes.saturating_add(raw_bytes);
            stats.cumulative_model_bytes = stats.cumulative_model_bytes.saturating_add(model_bytes);
            stats.cumulative_estimated_model_tokens = stats
                .cumulative_estimated_model_tokens
                .saturating_add(u64::try_from(estimated_tokens).unwrap_or(u64::MAX));
            stats.cumulative_bytes_saved = stats
                .cumulative_bytes_saved
                .saturating_add(raw_bytes.saturating_sub(model_bytes));
            exchange
        })
        .collect();
    (exchanges, stats)
}

/// Render the optional prior-summary section. Empty string when no
/// prior compaction exists — the user template just collapses the
/// variable out.
fn render_prior_summary_block(prior_summary: Option<&str>) -> String {
    match prior_summary {
        Some(prior) if !prior.trim().is_empty() => {
            format!("Prior summary (extend or refine):\n{}\n", prior.trim())
        },
        _ => String::new(),
    }
}

/// Render the transcript section. Heading line varies based on
/// whether a prior summary was supplied so the model knows whether
/// it's seeing fresh history or just the new tail since last
/// compaction.
fn render_transcript_block(has_prior: bool, older_turns: &[ReplayTurn]) -> String {
    let mut buf = String::new();
    if has_prior {
        buf.push_str("New turns since prior summary:\n");
    } else {
        buf.push_str("Conversation transcript to summarise:\n");
    }
    for turn in older_turns {
        buf.push_str(&turn.role);
        buf.push_str(": ");
        buf.push_str(&turn.text);
        buf.push('\n');
    }
    buf
}

/// Last-resort fallback when the LLM call fails — concatenate prior
/// summary with a verbatim list of the new turns. Degraded but
/// preserves substance for the next rotation.
fn fallback_concat_summary(prior_summary: Option<&str>, older_turns: &[ReplayTurn]) -> String {
    let mut buf = String::new();
    if let Some(prior) = prior_summary {
        buf.push_str(prior.trim());
        buf.push_str("\n\n");
    }
    for turn in older_turns {
        buf.push_str(&turn.role);
        buf.push_str(": ");
        buf.push_str(turn.text.trim());
        buf.push('\n');
    }
    buf.trim_end().to_string()
}

fn replay_turn_from_message(msg: ChatMessage) -> Option<ReplayTurn> {
    if msg.is_context_projection() {
        return None;
    }
    let ChatMessage {
        id,
        direction,
        content,
        created_at,
        ..
    } = msg;
    match content {
        // Surface terminal task lifecycle into the replay. A finished task is
        // otherwise only an activity CARD (`TaskStatusUpdate`), never a text
        // turn — so on resume the model sees the original request plus an "I'm
        // running it" ack with NO completion, concludes the work is still
        // pending, and re-fires the entire task. Emitting a completion turn
        // (in order, right after the request) closes that loop so the model
        // knows it's already done.
        ChatMessageContent::TaskStatusUpdate {
            status, summary, ..
        } => {
            let outcome = match status.as_str() {
                "completed" => "has COMPLETED",
                "failed" => "FAILED",
                "cancelled" => "was CANCELLED",
                // Non-terminal status churn (running/planning/…) carries no
                // replay value and would just add noise.
                _ => return None,
            };
            let detail = summary
                .as_deref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| format!(" Result: {value}."))
                .unwrap_or_default();
            let text = format!(
                "[A task you requested earlier {outcome} — it is no longer pending.{detail} \
Do NOT start it again unless the user explicitly asks for a fresh run.]"
            );
            Some(ReplayTurn {
                role: "assistant".to_string(),
                text,
                message_id: id,
                created_at_ms: created_at,
            })
        },
        ChatMessageContent::Text { text, .. } => {
            let role = match direction {
                ChatMessageDirection::User => "user",
                ChatMessageDirection::Assistant => "assistant",
                ChatMessageDirection::System => return None,
            };
            if text.trim().is_empty() {
                return None;
            }
            Some(ReplayTurn {
                role: role.to_string(),
                text,
                message_id: id,
                created_at_ms: created_at,
            })
        },
        _ => None,
    }
}

/// Determine whether a candidate id refers to a turn strictly older
/// than the marker id within the same `older` window. Used to slice
/// the "new since last compaction" subset incrementally. Linear scan
/// is fine — `older` is bounded by `COMPACTION_INPUT_TURN_LIMIT`.
fn older_than_marker(older: &[ReplayTurn], candidate: &str, marker: &str) -> bool {
    let candidate_idx = older.iter().position(|t| t.message_id == candidate);
    let marker_idx = older.iter().position(|t| t.message_id == marker);
    match (candidate_idx, marker_idx) {
        (Some(c), Some(m)) => c > m,
        // If the marker isn't in the window any more (older than the
        // input limit) we conservatively re-include the candidate.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_voice_display_projections_are_not_replayed() {
        let canonical = ChatMessage::new(
            "answer",
            "branch",
            ChatMessageDirection::Assistant,
            ChatMessageContent::Text {
                text: "Own topic".into(),
                plan_reply: None,
            },
            1,
        )
        .with_chat_turn_id(Some("voice-request-a".into()));
        assert!(replay_turn_from_message(canonical.clone()).is_some());
        let mut projected = canonical;
        projected.session_id = "parent".into();
        projected.context_origin = Some(
            magician::magician_v2::chat::models::ChatMessageContextOrigin {
                message_id: None,
                result_created_at: None,
                ui_thread_id: "general".into(),
                session_id: "branch".into(),
                request_id: "voice-request-a".into(),
            },
        );
        assert!(replay_turn_from_message(projected.clone()).is_none());
        projected.context_origin = None;
        projected.id = "voice-request-a-result".into();
        assert!(replay_turn_from_message(projected).is_none());
    }

    #[test]
    fn resume_compaction_reads_only_a_bounded_tool_transcript_tail() {
        let source = include_str!("voice_context_compactor.rs");
        let compact_for_resume = source
            .split_once("    pub async fn compact_for_resume(")
            .expect("compact_for_resume")
            .1
            .split_once("    async fn summarise(")
            .expect("compact_for_resume end")
            .0;
        assert!(compact_for_resume.contains(".get_llm_history_tail("));
        assert!(!compact_for_resume.contains(".get_llm_history(chat_session_id)"));
        assert!(compact_for_resume.contains("MAX_VOICE_TOOL_TRANSCRIPT_TAIL_ENTRIES"));
    }

    fn projection(
        call_id: &str,
        tool_name: &str,
        projected_result: Value,
        raw_bytes: usize,
        model_bytes: usize,
        estimated_tokens: usize,
    ) -> magician::magician_v2::tool_result_projection::ProjectedToolResultV1 {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "identity": {
                "tool_name": tool_name,
                "tool_call_id": call_id,
                "scope_digest": "scope",
                "authority_revision": "revision-1"
            },
            "outcome": {
                "status": "succeeded",
                "retryable": false
            },
            "model": {
                "value": projected_result,
                "strategy": "complete",
                "included_records": 1,
                "omitted_records": 0,
                "omitted_fields": 0
            },
            "display": {
                "kind": "referenced",
                "content_ref": { "result_ref": format!("result-{call_id}") },
                "content_hash": format!("hash-{call_id}"),
                "media_type": "application/json",
                "size_bytes": raw_bytes
            },
            "raw": {
                "content_ref": { "result_ref": format!("result-{call_id}") },
                "content_hash": format!("hash-{call_id}"),
                "media_type": "application/json",
                "size_bytes": raw_bytes,
                "retention_class": "voice_session"
            },
            "metrics": {
                "raw_bytes": raw_bytes,
                "model_bytes": model_bytes,
                "estimated_model_tokens": estimated_tokens,
                "spoken_characters": 0,
                "included_records": 1,
                "omitted_records": 0,
                "omitted_fields": 0,
                "maximum_input_depth": 2,
                "contract_fallback": false
            }
        }))
        .expect("projection fixture")
    }

    fn turn(id: &str, role: &str, text: &str, ts: i64) -> ReplayTurn {
        ReplayTurn {
            role: role.to_string(),
            text: text.to_string(),
            message_id: id.to_string(),
            created_at_ms: ts,
        }
    }

    #[test]
    fn fallback_concat_includes_prior_summary_first() {
        let older = vec![turn("m-1", "user", "hi there", 1)];
        let out = fallback_concat_summary(Some("earlier summary"), &older);
        assert!(out.starts_with("earlier summary"));
        assert!(out.contains("user: hi there"));
    }

    #[test]
    fn older_than_marker_handles_missing_marker() {
        let older = vec![
            turn("a", "user", "first", 1),
            turn("b", "assistant", "second", 2),
        ];
        // marker not in window → conservative include
        assert!(older_than_marker(&older, "a", "x-missing"));
    }

    #[test]
    fn older_than_marker_returns_true_only_for_later_candidates() {
        let older = vec![
            turn("a", "user", "first", 1),
            turn("b", "assistant", "second", 2),
            turn("c", "user", "third", 3),
        ];
        assert!(older_than_marker(&older, "c", "a"));
        assert!(!older_than_marker(&older, "a", "c"));
    }

    #[test]
    fn realtime_resume_preserves_balanced_projected_exchanges_and_selected_cost_stats() {
        let first_value = serde_json::json!({
            "data": { "records": [{ "id": 1 }] },
            "access_token": "escaped-token"
        });
        let first_provider_value = serde_json::json!({
            "data": { "records": [{ "id": 1 }] },
            "access_token": "[REDACTED]"
        });
        let second_value = serde_json::json!({ "data": { "records": [{ "id": 2 }] } });
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![
                    magician::magician_v2::chat::models::StoredToolCall {
                        id: "call-a".to_string(),
                        name: "first_lookup".to_string(),
                        arguments: serde_json::json!({
                            "id": 1,
                            "authorization": "Bearer resume-argument-secret"
                        }),
                    },
                    magician::magician_v2::chat::models::StoredToolCall {
                        id: "call-b".to_string(),
                        name: "second_lookup".to_string(),
                        arguments: serde_json::json!({ "id": 2 }),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("tampered_name_is_ignored".to_string()),
                projection: projection(
                    "call-a",
                    "first_lookup",
                    first_value.clone(),
                    1_000,
                    200,
                    50,
                ),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: Some("second_lookup".to_string()),
                projection: projection(
                    "different-call",
                    "second_lookup",
                    serde_json::json!({ "foreign": true }),
                    9_999,
                    1,
                    1,
                ),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: Some("second_lookup".to_string()),
                projection: projection(
                    "call-b",
                    "different_tool",
                    serde_json::json!({ "foreign": true }),
                    9_999,
                    1,
                    1,
                ),
            },
            // A corrupt result must not consume the pending call. A later
            // valid record can still form the exactly-one balanced exchange.
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: Some("second_lookup".to_string()),
                projection: projection(
                    "call-b",
                    "second_lookup",
                    second_value.clone(),
                    500,
                    125,
                    32,
                ),
            },
        ];

        let (exchanges, stats) = projected_tool_exchanges_with_stats(&history, 2);

        assert_eq!(exchanges.len(), 2);
        assert_eq!(exchanges[0].call_id, "call-a");
        assert_eq!(exchanges[0].tool_name, "first_lookup");
        assert_eq!(exchanges[0].arguments["authorization"], "Bearer [REDACTED]");
        assert!(!exchanges[0]
            .arguments
            .to_string()
            .contains("resume-argument-secret"));
        assert_eq!(exchanges[0].projected_result, first_provider_value);
        assert_eq!(exchanges[1].call_id, "call-b");
        assert_eq!(exchanges[1].tool_name, "second_lookup");
        assert_eq!(exchanges[1].projected_result, second_value);
        assert_eq!(stats.replay_count, 2);
        assert_eq!(stats.invalid_projection_count, 2);
        assert_eq!(stats.cumulative_raw_bytes, 1_500);
        assert_eq!(stats.cumulative_model_bytes, 325);
        assert_eq!(stats.cumulative_estimated_model_tokens, 82);
        assert_eq!(stats.cumulative_bytes_saved, 1_175);
    }

    #[test]
    fn realtime_resume_limit_applies_before_projection_replay_observability() {
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![
                    magician::magician_v2::chat::models::StoredToolCall {
                        id: "call-a".to_string(),
                        name: "lookup".to_string(),
                        arguments: serde_json::json!({}),
                    },
                    magician::magician_v2::chat::models::StoredToolCall {
                        id: "call-b".to_string(),
                        name: "lookup".to_string(),
                        arguments: serde_json::json!({}),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: None,
                projection: projection(
                    "call-a",
                    "lookup",
                    serde_json::json!({ "id": 1 }),
                    1_000,
                    100,
                    25,
                ),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: None,
                projection: projection(
                    "call-b",
                    "lookup",
                    serde_json::json!({ "id": 2 }),
                    600,
                    200,
                    50,
                ),
            },
        ];

        let (exchanges, stats) = projected_tool_exchanges_with_stats(&history, 1);

        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].call_id, "call-b");
        assert_eq!(stats.replay_count, 1);
        assert_eq!(stats.cumulative_raw_bytes, 600);
        assert_eq!(stats.cumulative_model_bytes, 200);
        assert_eq!(stats.cumulative_bytes_saved, 400);
    }
}
