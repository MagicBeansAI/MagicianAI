//! Per-surface visibility predicates derived from the master event
//! taxonomy in `realtime_events::GAUI_EVENT_TAXONOMY`.
//!
//! ## Why this lives here
//!
//! Each user-facing surface (chat timeline, WhatsApp / Telegram / Slack
//! bot output, push notifications, email digest, …) needs a different
//! slice of the agent-event stream. The naïve fix is to give each
//! surface its own denylist or allowlist of event_type strings — that
//! immediately drifts as soon as a new event is added: every surface
//! has to remember to update its list, and stale entries are silent
//! noise (denylist drift) or silent gaps (allowlist drift).
//!
//! The right shape is what the master taxonomy already provides:
//!
//!   * Every event_type is classified once in `GAUI_EVENT_TAXONOMY`
//!     with `(category, severity, user_relevant)`.
//!   * Each surface here declares its policy as a predicate over those
//!     three fields — no event_type strings.
//!   * Adding a new event = adding one row to the master table. Every
//!     surface automatically picks up the right behavior because they
//!     filter by category, not by event_type.
//!   * Adding a new surface = adding one predicate function in this
//!     module. The master table doesn't change.
//!
//! ## Adding a new surface
//!
//! Drop a `pub fn <name>_surface_renders_agent_event(event_type: &str)
//! -> bool` here that looks up the master taxonomy and returns true /
//! false based on its policy. Call it from the surface's delivery
//! handler.
//!
//! Future shape worth keeping in mind: WhatsApp / Telegram both
//! support editing prior bot messages (Telegram has no time limit;
//! WhatsApp Cloud API allows 15 minutes), so a surface predicate can
//! return `true` and the channel can still consolidate by editing an
//! anchor message keyed on `execution_id` rather than appending one
//! line per event. The predicate decides *visibility*; the channel
//! decides *delivery shape*.

use crate::magician_v2::realtime_events::{
    lookup_agent_event_taxonomy, ChatRenderKind, CoalesceKey, EventCategory, EventSeverity,
};

/// Categories the chat UI already renders through dedicated surfaces —
/// `TaskStatusUpdate` cards (every chat-spawned execution from
/// Phase 3.5 onwards), the agentic stream store + execution panel,
/// and the plan inspector. Re-rendering these as chat `Text` rows
/// produces duplicate noise the user has to scroll past.
///
/// HITL + Clarification belong here too: every HITL request is surfaced
/// by the global AttentionPill (top-right floating, every page), the
/// `/attention` page (full inbox sourced from both the HITL bus and the
/// feed-side projection), the TopBar Ops menu badge count, and the
/// embedded `RequestActivityCard` inside each chat turn's bubble (which
/// shows the actual pause reason inline). Re-emitting them again as
/// chat-system-messages (e.g. "1 pending input request(s).") was a
/// fourth competing notification surface on the same screen. The
/// in-chat "Waiting on you" CTA pill in the typing bubble remains for
/// the live in-flight turn — it carries the actual click-to-respond
/// affordance and isn't a passive notification.
const CHAT_RENDERED_ELSEWHERE: &[EventCategory] = &[
    EventCategory::Tool,
    EventCategory::Plan,
    EventCategory::Slot,
    EventCategory::Llm,
    EventCategory::Pipeline,
    EventCategory::Agentic,
    EventCategory::Hitl,
    EventCategory::Clarification,
];

/// Does the chat timeline want a `Text` ChatMessage for this agent
/// event?
///
/// Decision ladder:
///   1. Unknown event_type → `false`. Every emit site should appear
///      in `GAUI_EVENT_TAXONOMY`; missing rows are a registration bug,
///      not a hint to surface the event unclassified.
///   2. `user_relevant=false` → `false`. Internal / debug events stay
///      hidden from the chat timeline (still visible in the Internals
///      drawer's raw event stream).
///   3. Category in `CHAT_RENDERED_ELSEWHERE` → `false`. Chat already
///      renders those through dedicated surfaces (including HITL +
///      Clarification, which the global AttentionPill / `/attention`
///      page / TopBar badge / per-turn activity card cover).
///   4. Otherwise → `true` only at `Warn`-or-higher severity. This
///      keeps `agent.cycle.failed` / `agent.goal.failed` /
///      `agent.circuit.opened` visible (Error / Warn) while dropping
///      the Info-level lifecycle pings (`agent.cycle.started` /
///      `agent.cycle.completed`) that the chat doesn't need on top of
///      the assistant's own response.
pub fn chat_surface_renders_agent_event(event_type: &str) -> bool {
    !matches!(chat_render_kind(event_type), ChatRenderKind::Suppress)
}

/// Returns the chat-surface render kind for an event_type.
///
/// Decision ladder:
///   1. The master taxonomy carries an explicit `RenderHint.chat_kind`
///      that isn't `Suppress` → use it. This is the path for events
///      with deliberate render decisions baked into
///      `GAUI_EVENT_TAXONOMY` (pack-dispatcher synthetics, escalation
///      events, approval gates).
///   2. Otherwise fall back to category + severity + user_relevant
///      heuristics (the original `chat_surface_renders_agent_event`
///      shape). This handles the bulk of AGUI envelope events that
///      don't have explicit render hints yet.
///
/// Returns `Suppress` for unknown event_types — every emit site must
/// appear in `GAUI_EVENT_TAXONOMY`; missing rows show up as silent
/// suppression rather than as undifferentiated noise.
pub fn chat_render_kind(event_type: &str) -> ChatRenderKind {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return ChatRenderKind::Suppress;
    };
    if !matches!(taxonomy.render.chat_kind, ChatRenderKind::Suppress) {
        return taxonomy.render.chat_kind;
    }
    if !taxonomy.user_relevant {
        return ChatRenderKind::Suppress;
    }
    if CHAT_RENDERED_ELSEWHERE.contains(&taxonomy.category) {
        // HITL + Clarification land here too: covered by the global
        // AttentionPill, `/attention` page, TopBar badge, and the
        // per-turn `RequestActivityCard` pause-row. Chat doesn't
        // re-emit them as system-message text rows.
        return ChatRenderKind::Suppress;
    }
    if event_severity_rank(taxonomy.severity) >= event_severity_rank(EventSeverity::Warn) {
        return ChatRenderKind::Text;
    }
    ChatRenderKind::Suppress
}

/// Returns the chat-surface coalesce key for an event_type. Used by
/// the chat channel to group repeat events for the same logical
/// activity (pack run, task lifecycle, escalation reply chain) into
/// one rendered slot.
pub fn chat_coalesce_key(event_type: &str) -> CoalesceKey {
    lookup_agent_event_taxonomy(event_type)
        .map(|t| t.render.coalesce_by)
        .unwrap_or(CoalesceKey::None)
}

/// Should the webhook channel deliver this event to its configured
/// external endpoint?
///
/// Webhook integrations typically want lifecycle milestones, not the
/// agentic-stream / planning / LLM internal chatter. Drop everything
/// in `CHAT_RENDERED_ELSEWHERE` plus any event whose taxonomy says
/// `user_relevant=false`. Keep curated user-facing categories (Hitl,
/// Clarification, Agent, Execution, Task) at any severity, since the
/// external system has its own filtering.
///
/// Per-subscription `min_severity` still applies on top of this gate
/// (handled by the router). Subscriptions that want a different policy
/// can use a stricter min_severity or a dedicated routing key.
pub fn webhook_surface_renders_agent_event(event_type: &str) -> bool {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return false;
    };
    if !taxonomy.user_relevant {
        return false;
    }
    if matches!(
        taxonomy.category,
        EventCategory::Tool
            | EventCategory::Plan
            | EventCategory::Slot
            | EventCategory::Llm
            | EventCategory::Pipeline
            | EventCategory::Agentic
    ) {
        return false;
    }
    true
}

/// Should the agent_memory channel record this event as an episode?
///
/// Agent memory captures "what happened in this run" for later recall.
/// We want terminal lifecycle events (success / failure / cancellation)
/// + approval gates + agent.cycle.failed-style failure events; we
/// don't want raw progress lines (those balloon the episode store).
pub fn agent_memory_surface_renders_agent_event(event_type: &str) -> bool {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return false;
    };
    if !taxonomy.user_relevant {
        return false;
    }
    // Episodes are about "what changed", not "what's currently
    // happening" — drop agentic-stream + planning + slot internals.
    if matches!(
        taxonomy.category,
        EventCategory::Tool
            | EventCategory::Plan
            | EventCategory::Slot
            | EventCategory::Llm
            | EventCategory::Pipeline
            | EventCategory::Agentic
    ) {
        return false;
    }
    // Only events with Warn+ severity OR Hitl/Clarification categories
    // (which always demand human attention) deserve an episode entry.
    // Routine Info lifecycle pings (`agent.cycle.completed`,
    // `agent.created`) are noise for memory.
    if matches!(
        taxonomy.category,
        EventCategory::Hitl | EventCategory::Clarification
    ) {
        return true;
    }
    event_severity_rank(taxonomy.severity) >= event_severity_rank(EventSeverity::Warn)
}

/// Should the feed materializer create a feed item for this event?
///
/// The feed is the user's curated "what's happening" timeline. It
/// renders lifecycle milestones (task created / completed / failed)
/// + escalations + agent-level events; it skips internal pipeline
/// + LLM chatter the same way the chat does.
pub fn feed_surface_renders_agent_event(event_type: &str) -> bool {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return false;
    };
    if !taxonomy.user_relevant {
        return false;
    }
    !matches!(
        taxonomy.category,
        EventCategory::Tool
            | EventCategory::Plan
            | EventCategory::Slot
            | EventCategory::Llm
            | EventCategory::Pipeline
            | EventCategory::Agentic
    )
}

/// Should the execution-panel projector refresh on this event?
///
/// The execution panel projects the current execution state. Any
/// event that changes lifecycle (StatusChanged → status transition,
/// HandedOver → ownership change, agent.cycle.failed → terminal) is
/// a refresh trigger. Internal stream events (tool.call.*, reasoning,
/// plan.step.*) don't change the projected state.
pub fn execution_panel_surface_refreshes_on_agent_event(event_type: &str) -> bool {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return false;
    };
    matches!(
        taxonomy.category,
        EventCategory::Execution
            | EventCategory::Task
            | EventCategory::Agent
            | EventCategory::Hitl
            | EventCategory::Clarification
    )
}

/// Helper: passes-or-blocks check used by external bot channels
/// (WhatsApp, Telegram, push) that want only "the human needs to do
/// something / something important just changed" events. Lifts severity
/// to a meaningful floor and filters by curated categories.
///
/// Not wired to any channel yet — kept here as the canonical shape
/// channels should copy when they're ready to consolidate via editing
/// rather than appending one line per event.
pub fn external_bot_surface_renders_agent_event(
    event_type: &str,
    severity_floor: EventSeverity,
) -> bool {
    let Some(taxonomy) = lookup_agent_event_taxonomy(event_type) else {
        return false;
    };
    if !taxonomy.user_relevant {
        return false;
    }
    if event_severity_rank(taxonomy.severity) < event_severity_rank(severity_floor) {
        return false;
    }
    matches!(
        taxonomy.category,
        EventCategory::Hitl
            | EventCategory::Clarification
            | EventCategory::Agent
            | EventCategory::Execution
            | EventCategory::Task
    )
}

/// Numeric rank so callers can express "at least Warn", "at least
/// Attention", etc. Mirrors the user-impact ordering used by the
/// Activity / Internals filters; `Decision` is treated as on par with
/// `Attention` since both demand human input.
fn event_severity_rank(severity: EventSeverity) -> u8 {
    match severity {
        EventSeverity::Info => 0,
        EventSeverity::Attention | EventSeverity::Decision => 1,
        EventSeverity::Warn => 2,
        EventSeverity::Error => 3,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn chat_surface_drops_agentic_stream_events() {
        // Tool / plan / reasoning / pipeline are rendered through
        // dedicated UI surfaces; the chat timeline must not duplicate.
        for event_type in [
            "tool.call.started",
            "tool.call.args",
            "tool.call.finished",
            "reasoning.start",
            "reasoning.content",
            "reasoning.end",
            "plan.step.started",
            "plan.step.finished",
            "plan.created",
            "plan.revised",
            "execution.progress",
        ] {
            assert!(
                !chat_surface_renders_agent_event(event_type),
                "{event_type} should not appear in chat",
            );
        }
    }

    #[test]
    fn chat_surface_keeps_human_facing_events() {
        // Agent-failure / circuit-breaker events carry information the
        // chat user needs to see and don't have a dedicated alternate
        // surface that already covers them.
        for event_type in [
            "agent.cycle.failed",
            "agent.goal.failed",
            "agent.circuit.opened",
        ] {
            assert!(
                chat_surface_renders_agent_event(event_type),
                "{event_type} should appear in chat",
            );
        }
    }

    #[test]
    fn chat_surface_suppresses_hitl_and_clarification() {
        // HITL + Clarification events are covered by the global
        // AttentionPill, `/attention` page, TopBar badge, and the
        // per-turn RequestActivityCard. Chat must not double up as a
        // fourth notification surface emitting system-message rows
        // like "1 pending input request(s).".
        for event_type in [
            "hitl.requested",
            "hitl.resolved",
            "input.requested",
            "input.received",
            "input.resolved",
            "waiting_for_confirmation",
            "execution.waiting_for_user",
            "clarified_task.ready",
        ] {
            assert!(
                !chat_surface_renders_agent_event(event_type),
                "{event_type} should NOT appear in chat (covered by AttentionPill / \
                 /attention page / TopBar badge / per-turn activity card)",
            );
        }
    }

    #[test]
    fn chat_surface_drops_unknown_event_types() {
        assert!(!chat_surface_renders_agent_event(
            "definitely.not.a.real.event"
        ));
    }

    /// Pins the render decision for the run-outbox hole marker, which is the
    /// one event in the table whose *suppression* on the user-facing surfaces
    /// is the point rather than the leftover.
    ///
    /// `loop.outbox.gap` says "records this run had already journalled were
    /// evicted from the outbox, and the hole is here". Until 2026-08-29 the
    /// name had no taxonomy row at all, so every predicate below answered the
    /// same way it does now — but for the opposite reason: it was the
    /// fail-closed answer for an unknown name, and the marker reached no reader
    /// anywhere. It now has a row (`Observability / Warn / user_relevant =
    /// false`), and that row asks for exactly this shape:
    ///
    /// * the operator stream classifies it at warn prominence, so a hole is
    ///   findable by a `severity >= warn` filter on `/events`, in the Internals
    ///   drawer and in the runtime Activity view — the surfaces where the
    ///   records it is ABOUT (`plan.step.*`, `tool.result.projected`,
    ///   `reasoning.*`, all `user_relevant = false`) actually appear;
    /// * chat, feed, webhook and agent memory drop it, because a
    ///   transport-buffer defect reported to somebody who never saw the lost
    ///   records and cannot act on the loss is noise, not information.
    ///
    /// The assertion that matters most is the chat one. Flipping
    /// `user_relevant` to `true` would put an internal diagnostic into the chat
    /// timeline and the user's feed, and nothing else in the tree would notice.
    #[test]
    fn the_outbox_gap_marker_renders_for_operators_and_for_nobody_else() {
        let gap = "loop.outbox.gap";

        // FIRST, because every predicate below returns the value this test
        // asserts for an *unknown* name too. Without this line the whole test
        // would pass while the marker reached no reader at all — which is
        // precisely the bug it exists to keep from regressing.
        let taxonomy = lookup_agent_event_taxonomy(gap).unwrap_or_else(|| {
            panic!(
                "`{gap}` must have a row in `GAUI_EVENT_TAXONOMY`: without one \
                 `chat_render_kind` answers `Suppress` and the webhook / \
                 agent-memory predicates answer `false` on the unknown name, so \
                 the hole marker is journalled, projected and then dropped one \
                 step short of every surface"
            )
        });

        assert!(
            !chat_surface_renders_agent_event(gap),
            "the outbox gap marker must not become a chat bubble: it describes \
             records the chat never showed either"
        );
        assert!(!feed_surface_renders_agent_event(gap));
        assert!(!webhook_surface_renders_agent_event(gap));
        assert!(!agent_memory_surface_renders_agent_event(gap));
        assert!(!external_bot_surface_renders_agent_event(
            gap,
            EventSeverity::Info
        ));

        assert_eq!(
            taxonomy.category,
            EventCategory::Observability,
            "Observability is this taxonomy's home for one-off faults and \
             alerts. A category picked for its routing side-effects instead \
             (Agentic would suppress the four user-facing surfaces structurally) \
             is how the category axis this module derives every predicate from \
             stops meaning anything."
        );
        assert_eq!(
            taxonomy.severity,
            EventSeverity::Warn,
            "Warn, not Info: Info is what an *unregistered* name already fell \
             through to in the TS mirror's catch-all, so it would leave the hole \
             exactly as invisible to a severity filter as it was before."
        );
        assert!(
            !taxonomy.user_relevant,
            "`user_relevant` is the operator-vs-user axis every predicate above \
             gates on first. Setting it true is the one edit that silently puts \
             this diagnostic into chat and the user's feed."
        );
    }

    #[test]
    fn external_bot_filters_by_severity_floor() {
        // hitl.requested = Hitl / Attention → passes Warn floor.
        assert!(external_bot_surface_renders_agent_event(
            "hitl.requested",
            EventSeverity::Warn,
        ));
        // hitl.resolved = Hitl / Info → blocked by Warn floor.
        assert!(!external_bot_surface_renders_agent_event(
            "hitl.resolved",
            EventSeverity::Warn,
        ));
        // hitl.resolved DOES pass at Info floor.
        assert!(external_bot_surface_renders_agent_event(
            "hitl.resolved",
            EventSeverity::Info,
        ));
    }

    #[test]
    fn external_bot_drops_agentic_stream_categories() {
        // Tool / Plan / Llm / Slot / Pipeline / Agentic aren't in the
        // external-bot allowlist regardless of user_relevant or severity.
        for event_type in [
            "tool.call.started",
            "tool.call.finished",
            "reasoning.start",
            "plan.step.started",
        ] {
            assert!(
                !external_bot_surface_renders_agent_event(event_type, EventSeverity::Info),
                "{event_type} should not reach external bots",
            );
        }
    }
}
