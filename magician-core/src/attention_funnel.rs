//! Canonical attention-routing vocabulary.
//!
//! This module is the shared contract for provider-neutral attention candidates,
//! route decisions, source families, and route/drop plus per-stage trace events.
//! Current producers still own their durable stores, but comms classification,
//! resurfacing curation, Today projection, and lane APIs use this vocabulary for
//! consistent Follow-ups, Needs you, Worth a look, Active work, Delivered, and
//! Changed semantics.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! impl_string_enum {
    ($ty:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s.trim() {
                    $($value => Ok(Self::$variant),)+
                    other => Err(format!("unknown {}: {other}", stringify!($ty))),
                }
            }
        }
    };
}

/// Scope key shared by all attention funnel records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionScope {
    pub principal: String,
    pub workspace: String,
}

/// Durable substrate an attention item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSourceKind {
    Comm,
    Memory,
    Task,
    Episode,
    Calendar,
    Note,
    Web,
    Meeting,
    ScreenObservation,
    TabObservation,
    Work,
    System,
    Other,
}

impl_string_enum!(AttentionSourceKind {
    Comm => "comm",
    Memory => "memory",
    Task => "task",
    Episode => "episode",
    Calendar => "calendar",
    Note => "note",
    Web => "web",
    Meeting => "meeting",
    ScreenObservation => "screen_observation",
    TabObservation => "tab_observation",
    Work => "work",
    System => "system",
    Other => "other",
});

/// Opaque source pointer plus optional provider/account context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSource {
    pub kind: AttentionSourceKind,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_alias: Option<String>,
}

/// Why a candidate exists. This is diagnostic provenance, not the surface lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSourceFamily {
    Promise,
    CommsIngest,
    Resurfacing,
    Memory,
    Task,
    Episode,
    Calendar,
    Meeting,
    ScreenObservation,
    TabObservation,
    Work,
    Other,
}

impl_string_enum!(AttentionSourceFamily {
    Promise => "promise",
    CommsIngest => "comms_ingest",
    Resurfacing => "resurfacing",
    Memory => "memory",
    Task => "task",
    Episode => "episode",
    Calendar => "calendar",
    Meeting => "meeting",
    ScreenObservation => "screen_observation",
    TabObservation => "tab_observation",
    Work => "work",
    Other => "other",
});

impl AttentionSourceFamily {
    /// Resolve source-family for current Follow-up rows. Router metadata is
    /// authoritative for new rows; label/action inference is retained only for
    /// legacy rows created before the attention funnel stamped metadata.
    pub fn from_follow_up_route_metadata_or_signals(
        label: Option<&str>,
        proposed_action: Option<&serde_json::Value>,
    ) -> Self {
        proposed_action_string(proposed_action, "attention_source_family")
            .and_then(|family| Self::from_str(&family).ok())
            .unwrap_or_else(|| Self::from_follow_up_signals(label, proposed_action))
    }

    /// Convert current channel-assist follow-up/proposed-action signals into the
    /// canonical family split used by Follow-ups observability. This is legacy
    /// compatibility for rows that do not carry `attention_source_family`.
    pub fn from_follow_up_signals(
        label: Option<&str>,
        proposed_action: Option<&serde_json::Value>,
    ) -> Self {
        if is_promise_like_label(label) || is_promise_like_action(proposed_action) {
            Self::Promise
        } else {
            Self::CommsIngest
        }
    }
}

/// Target user-facing lane after routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionLane {
    NeedsYou,
    FollowUp,
    WorthALook,
    ActiveWork,
    Delivered,
    Changed,
    Failed,
}

impl_string_enum!(AttentionLane {
    NeedsYou => "needs_you",
    FollowUp => "follow_up",
    WorthALook => "worth_a_look",
    ActiveWork => "active_work",
    Delivered => "delivered",
    Changed => "changed",
    Failed => "failed",
});

/// Funnel stage that produced a route event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionFunnelStage {
    Ingested,
    Distilled,
    Extracted,
    Filtered,
    Routed,
    Surfaced,
    Acted,
    Dropped,
}

impl_string_enum!(AttentionFunnelStage {
    Ingested => "ingested",
    Distilled => "distilled",
    Extracted => "extracted",
    Filtered => "filtered",
    Routed => "routed",
    Surfaced => "surfaced",
    Acted => "acted",
    Dropped => "dropped",
});

/// Human-attention urgency independent of lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionUrgency {
    Low,
    Normal,
    High,
    Critical,
}

impl_string_enum!(AttentionUrgency {
    Low => "low",
    Normal => "normal",
    High => "high",
    Critical => "critical",
});

/// Relative ordering hint inside a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutePriority {
    Low,
    Normal,
    High,
    Critical,
}

impl_string_enum!(RoutePriority {
    Low => "low",
    Normal => "normal",
    High => "high",
    Critical => "critical",
});

/// Status for non-terminal funnel stage traces.
///
/// Final lane decisions still use `Routed`/`Dropped`; this status lets producers
/// report ingest/distill/extract/filter/surface/action progress without faking a
/// route or drop decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionTraceStatus {
    Started,
    Succeeded,
    Skipped,
    Failed,
}

impl_string_enum!(AttentionTraceStatus {
    Started => "started",
    Succeeded => "succeeded",
    Skipped => "skipped",
    Failed => "failed",
});

/// Positive route reason attached to a lane assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReason {
    OwnerApprovalRequired,
    OwnerInterventionRequired,
    /// Recurring Monitors Phase 3: a monitored source has failed on
    /// consecutive runs (auth/permission/CAPTCHA/unavailable) and the owner
    /// must restore access. Routes to Needs You (plan §5.5/§9.2).
    AccessProblemDetected,
    ActionableCommunication,
    PromiseOrObligation,
    WaitingOnCounterparty,
    NonActionableUsefulContext,
    ActiveWorkState,
    DeliveredWork,
    ObservedChange,
    FailedWork,
}

impl_string_enum!(RouteReason {
    OwnerApprovalRequired => "owner_approval_required",
    OwnerInterventionRequired => "owner_intervention_required",
    AccessProblemDetected => "access_problem_detected",
    ActionableCommunication => "actionable_communication",
    PromiseOrObligation => "promise_or_obligation",
    WaitingOnCounterparty => "waiting_on_counterparty",
    NonActionableUsefulContext => "non_actionable_useful_context",
    ActiveWorkState => "active_work_state",
    DeliveredWork => "delivered_work",
    ObservedChange => "observed_change",
    FailedWork => "failed_work",
});

/// Machine-readable reason a candidate did not reach a user-facing lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    ActiveFollowUpExists,
    ActionAlreadyHandled,
    ActionMaterializationFailed,
    DuplicateOfHigherPriorityLane,
    OwnerDismissed,
    SensitiveOrSuppressed,
    StaleContentRevision,
    RecencyOnly,
    WeakSignal,
    MissingSafeSummary,
    CooldownActive,
    CuratorDeferred,
    UnsupportedSource,
}

impl_string_enum!(DropReason {
    ActiveFollowUpExists => "active_follow_up_exists",
    ActionAlreadyHandled => "action_already_handled",
    ActionMaterializationFailed => "action_materialization_failed",
    DuplicateOfHigherPriorityLane => "duplicate_of_higher_priority_lane",
    OwnerDismissed => "owner_dismissed",
    SensitiveOrSuppressed => "sensitive_or_suppressed",
    StaleContentRevision => "stale_content_revision",
    RecencyOnly => "recency_only",
    WeakSignal => "weak_signal",
    MissingSafeSummary => "missing_safe_summary",
    CooldownActive => "cooldown_active",
    CuratorDeferred => "curator_deferred",
    UnsupportedSource => "unsupported_source",
});

/// Participant role when a source carries people/channel metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionParticipantRole {
    Owner,
    Counterparty,
    Sender,
    Recipient,
    Observer,
    Unknown,
}

impl_string_enum!(AttentionParticipantRole {
    Owner => "owner",
    Counterparty => "counterparty",
    Sender => "sender",
    Recipient => "recipient",
    Observer => "observer",
    Unknown => "unknown",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionParticipant {
    pub role: AttentionParticipantRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Optional action intent carried by a candidate before lane routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionActionKind {
    Reply,
    FollowUp,
    Review,
    Approve,
    Open,
    Acknowledge,
    Dismiss,
    Draft,
    Schedule,
    Other,
}

impl_string_enum!(AttentionActionKind {
    Reply => "reply",
    FollowUp => "follow_up",
    Review => "review",
    Approve => "approve",
    Open => "open",
    Acknowledge => "acknowledge",
    Dismiss => "dismiss",
    Draft => "draft",
    Schedule => "schedule",
    Other => "other",
});

impl AttentionActionKind {
    fn implies_follow_up_lane(self) -> bool {
        matches!(
            self,
            Self::Reply | Self::FollowUp | Self::Draft | Self::Schedule
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionAction {
    pub kind: AttentionActionKind,
    pub label: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Normalized source item before extraction. Some sources may enter the funnel
/// here already structured and skip an LLM distillation stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedAttentionItem {
    pub item_id: String,
    pub scope: AttentionScope,
    pub source: AttentionSource,
    pub occurred_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<AttentionParticipant>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Candidate emitted by an extractor before final lane assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionCandidate {
    pub candidate_key: String,
    pub source: AttentionSource,
    pub source_family: AttentionSourceFamily,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AttentionAction>,
    pub urgency: AttentionUrgency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RouteOutcome {
    Routed {
        lane: AttentionLane,
        reason: RouteReason,
        priority: RoutePriority,
    },
    Dropped {
        reason: DropReason,
    },
    Traced {
        status: AttentionTraceStatus,
    },
}

/// Side-effect-free routing context. Production callers derive these flags from
/// their own durable stores before calling the shared router; the router itself
/// remains pure and owns no persistence.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttentionRouteContext {
    #[serde(default)]
    pub active_follow_up_exists: bool,
    #[serde(default)]
    pub action_already_handled: bool,
    #[serde(default)]
    pub action_materialization_failed: bool,
    #[serde(default)]
    pub duplicate_of_higher_priority_lane: bool,
    #[serde(default)]
    pub owner_dismissed: bool,
    #[serde(default)]
    pub sensitive_or_suppressed: bool,
    #[serde(default)]
    pub stale_content_revision: bool,
    #[serde(default)]
    pub recency_only: bool,
    #[serde(default)]
    pub weak_signal: bool,
    #[serde(default)]
    pub missing_safe_summary: bool,
    #[serde(default)]
    pub cooldown_active: bool,
    #[serde(default)]
    pub owner_approval_required: bool,
    #[serde(default)]
    pub owner_intervention_required: bool,
    /// Recurring Monitors Phase 3: a monitored source has failed access on
    /// consecutive runs and needs the owner (login/permission). Routes to
    /// Needs You with [`RouteReason::AccessProblemDetected`].
    #[serde(default)]
    pub access_problem_detected: bool,
    #[serde(default)]
    pub active_work_state: bool,
    #[serde(default)]
    pub delivered_work: bool,
    #[serde(default)]
    pub observed_change: bool,
    #[serde(default)]
    pub failed_work_report: bool,
    #[serde(default)]
    pub non_actionable_useful_context: bool,
}

/// Deterministic canonical router. It intentionally owns no persistence and
/// calls no LLMs; callers provide candidate facts plus already-known store
/// context and receive one lane decision or one drop reason.
pub fn route_attention_candidate(
    candidate: &AttentionCandidate,
    context: &AttentionRouteContext,
) -> RouteOutcome {
    if context.sensitive_or_suppressed {
        return dropped(DropReason::SensitiveOrSuppressed);
    }
    if context.stale_content_revision {
        return dropped(DropReason::StaleContentRevision);
    }
    if context.action_materialization_failed {
        return dropped(DropReason::ActionMaterializationFailed);
    }
    if context.owner_dismissed {
        return dropped(DropReason::OwnerDismissed);
    }
    if context.action_already_handled {
        return dropped(DropReason::ActionAlreadyHandled);
    }
    if context.missing_safe_summary || candidate.summary.trim().is_empty() {
        return dropped(DropReason::MissingSafeSummary);
    }
    if context.duplicate_of_higher_priority_lane {
        return dropped(DropReason::DuplicateOfHigherPriorityLane);
    }
    if context.cooldown_active {
        return dropped(DropReason::CooldownActive);
    }
    if context.recency_only {
        return dropped(DropReason::RecencyOnly);
    }
    if context.weak_signal {
        return dropped(DropReason::WeakSignal);
    }
    if context.owner_approval_required {
        return routed(
            AttentionLane::NeedsYou,
            RouteReason::OwnerApprovalRequired,
            priority_for(candidate.urgency),
        );
    }
    if context.owner_intervention_required {
        return routed(
            AttentionLane::NeedsYou,
            RouteReason::OwnerInterventionRequired,
            priority_for(candidate.urgency),
        );
    }
    if context.access_problem_detected {
        return routed(
            AttentionLane::NeedsYou,
            RouteReason::AccessProblemDetected,
            priority_for(candidate.urgency),
        );
    }
    if context.active_follow_up_exists {
        return dropped(DropReason::ActiveFollowUpExists);
    }
    if candidate.source_family == AttentionSourceFamily::Promise {
        return routed(
            AttentionLane::FollowUp,
            RouteReason::PromiseOrObligation,
            priority_for(candidate.urgency),
        );
    }
    if let Some(action) = &candidate.action {
        if action.kind.implies_follow_up_lane() {
            return routed(
                AttentionLane::FollowUp,
                RouteReason::ActionableCommunication,
                priority_for(candidate.urgency),
            );
        }
    }
    if context.active_work_state {
        return routed(
            AttentionLane::ActiveWork,
            RouteReason::ActiveWorkState,
            priority_for(candidate.urgency),
        );
    }
    if context.delivered_work {
        return routed(
            AttentionLane::Delivered,
            RouteReason::DeliveredWork,
            priority_for(candidate.urgency),
        );
    }
    if context.observed_change {
        return routed(
            AttentionLane::Changed,
            RouteReason::ObservedChange,
            priority_for(candidate.urgency),
        );
    }
    if context.failed_work_report {
        return routed(
            AttentionLane::Failed,
            RouteReason::FailedWork,
            priority_for(candidate.urgency),
        );
    }
    if context.non_actionable_useful_context {
        return routed(
            AttentionLane::WorthALook,
            RouteReason::NonActionableUsefulContext,
            priority_for(candidate.urgency),
        );
    }
    dropped(DropReason::UnsupportedSource)
}

fn routed(lane: AttentionLane, reason: RouteReason, priority: RoutePriority) -> RouteOutcome {
    RouteOutcome::Routed {
        lane,
        reason,
        priority,
    }
}

fn dropped(reason: DropReason) -> RouteOutcome {
    RouteOutcome::Dropped { reason }
}

fn priority_for(urgency: AttentionUrgency) -> RoutePriority {
    match urgency {
        AttentionUrgency::Low => RoutePriority::Low,
        AttentionUrgency::Normal => RoutePriority::Normal,
        AttentionUrgency::High => RoutePriority::High,
        AttentionUrgency::Critical => RoutePriority::Critical,
    }
}

/// Append-only audit event shape for route/drop decisions and stage traces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionRouteEvent {
    pub event_id: String,
    pub scope: AttentionScope,
    pub source: AttentionSource,
    pub source_family: AttentionSourceFamily,
    pub candidate_key: String,
    pub stage: AttentionFunnelStage,
    pub outcome: RouteOutcome,
    pub occurred_at: i64,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn proposed_action_string(action: Option<&serde_json::Value>, key: &str) -> Option<String> {
    action
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_promise_like_label(label: Option<&str>) -> bool {
    let normalized = label.map(|value| value.trim().to_ascii_lowercase());
    matches!(
        normalized.as_deref(),
        Some(
            "follow_up"
                | "promise"
                | "owner_owes"
                | "other_owes"
                | "waiting_on"
                | "check_back"
                | "schedule"
        )
    )
}

fn is_promise_like_action(action: Option<&serde_json::Value>) -> bool {
    matches!(
        proposed_action_string(action, "follow_up_kind").as_deref(),
        Some("owner_owes" | "other_owes" | "waiting_on" | "check_back" | "schedule")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        source_family: AttentionSourceFamily,
        action: Option<AttentionActionKind>,
    ) -> AttentionCandidate {
        AttentionCandidate {
            candidate_key: "candidate-1".to_string(),
            source: AttentionSource {
                kind: AttentionSourceKind::Comm,
                source_ref: "source-1".to_string(),
                provider: Some("gmail".to_string()),
                account_alias: Some("business".to_string()),
            },
            source_family,
            evidence_refs: vec!["message-1".to_string()],
            title: "Useful thing".to_string(),
            summary: "Safe local summary".to_string(),
            action: action.map(|kind| AttentionAction {
                kind,
                label: kind.as_str().to_string(),
                payload: serde_json::Value::Null,
            }),
            urgency: AttentionUrgency::Normal,
            confidence: Some(0.82),
            metadata: serde_json::Value::Null,
        }
    }

    fn assert_enum_string<T>(value: T, wire: &str)
    where
        T: Copy
            + fmt::Debug
            + PartialEq
            + Serialize
            + for<'de> Deserialize<'de>
            + FromStr<Err = String>
            + fmt::Display,
    {
        assert_eq!(value.to_string(), wire);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            format!("\"{wire}\"")
        );
        assert_eq!(
            serde_json::from_str::<T>(&format!("\"{wire}\"")).unwrap(),
            value
        );
        assert_eq!(wire.parse::<T>().unwrap(), value);
    }

    #[test]
    fn stable_wire_strings_for_core_enums() {
        assert_enum_string(AttentionSourceKind::Comm, "comm");
        assert_enum_string(AttentionSourceKind::Note, "note");
        assert_enum_string(AttentionSourceKind::ScreenObservation, "screen_observation");
        assert_enum_string(AttentionSourceFamily::CommsIngest, "comms_ingest");
        assert_enum_string(AttentionSourceFamily::Promise, "promise");
        assert_enum_string(AttentionLane::NeedsYou, "needs_you");
        assert_enum_string(AttentionLane::WorthALook, "worth_a_look");
        assert_enum_string(AttentionLane::Failed, "failed");
        assert_enum_string(AttentionFunnelStage::Distilled, "distilled");
        assert_enum_string(AttentionTraceStatus::Succeeded, "succeeded");
        assert_enum_string(RouteReason::PromiseOrObligation, "promise_or_obligation");
        assert_enum_string(RouteReason::FailedWork, "failed_work");
        assert_enum_string(
            RouteReason::AccessProblemDetected,
            "access_problem_detected",
        );
        assert_enum_string(
            DropReason::DuplicateOfHigherPriorityLane,
            "duplicate_of_higher_priority_lane",
        );
        assert_enum_string(AttentionActionKind::FollowUp, "follow_up");
    }

    #[test]
    fn follow_up_signals_map_to_source_family() {
        assert_eq!(
            AttentionSourceFamily::from_follow_up_signals(Some("needs_reply"), None),
            AttentionSourceFamily::CommsIngest
        );
        assert_eq!(
            AttentionSourceFamily::from_follow_up_signals(Some("follow_up"), None),
            AttentionSourceFamily::Promise
        );
        assert_eq!(
            AttentionSourceFamily::from_follow_up_signals(
                Some("needs_reply"),
                Some(&serde_json::json!({ "follow_up_kind": "owner_owes" })),
            ),
            AttentionSourceFamily::Promise
        );
        assert_eq!(
            AttentionSourceFamily::from_follow_up_signals(
                Some("needs_reply"),
                Some(&serde_json::json!({ "follow_up_kind": "none" })),
            ),
            AttentionSourceFamily::CommsIngest
        );
    }

    #[test]
    fn trace_outcome_serializes_as_tagged_shape() {
        let outcome = RouteOutcome::Traced {
            status: AttentionTraceStatus::Succeeded,
        };

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({
                "outcome": "traced",
                "status": "succeeded"
            })
        );
    }

    #[test]
    fn follow_up_route_metadata_is_authoritative_for_source_family() {
        assert_eq!(
            AttentionSourceFamily::from_follow_up_route_metadata_or_signals(
                Some("follow_up"),
                Some(&serde_json::json!({
                    "attention_source_family": "comms_ingest",
                    "follow_up_kind": "owner_owes",
                })),
            ),
            AttentionSourceFamily::CommsIngest
        );
        assert_eq!(
            AttentionSourceFamily::from_follow_up_route_metadata_or_signals(
                Some("needs_reply"),
                Some(&serde_json::json!({
                    "attention_source_family": "promise",
                })),
            ),
            AttentionSourceFamily::Promise
        );
    }

    #[test]
    fn route_outcome_serializes_as_tagged_shape() {
        let outcome = RouteOutcome::Routed {
            lane: AttentionLane::FollowUp,
            reason: RouteReason::PromiseOrObligation,
            priority: RoutePriority::High,
        };

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({
                "outcome": "routed",
                "lane": "follow_up",
                "reason": "promise_or_obligation",
                "priority": "high"
            })
        );
    }

    #[test]
    fn route_event_roundtrips_with_optional_context() {
        let event = AttentionRouteEvent {
            event_id: "evt-1".to_string(),
            scope: AttentionScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            source: AttentionSource {
                kind: AttentionSourceKind::Comm,
                source_ref: "gmail/business/thread/msg".to_string(),
                provider: Some("gmail".to_string()),
                account_alias: Some("business".to_string()),
            },
            source_family: AttentionSourceFamily::Promise,
            candidate_key: "candidate-1".to_string(),
            stage: AttentionFunnelStage::Routed,
            outcome: RouteOutcome::Dropped {
                reason: DropReason::ActiveFollowUpExists,
            },
            occurred_at: 1_783_209_600,
            created_at: 1_783_209_601,
            confidence: Some(0.9),
            metadata: serde_json::json!({ "lane_probe": true }),
        };

        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: AttentionRouteEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn router_drops_safety_and_duplicate_gates_before_lane_assignment() {
        let promise = candidate(AttentionSourceFamily::Promise, None);

        assert_eq!(
            route_attention_candidate(
                &promise,
                &AttentionRouteContext {
                    sensitive_or_suppressed: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Dropped {
                reason: DropReason::SensitiveOrSuppressed,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &promise,
                &AttentionRouteContext {
                    active_follow_up_exists: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Dropped {
                reason: DropReason::ActiveFollowUpExists,
            }
        );
    }

    #[test]
    fn router_sends_approval_and_intervention_to_needs_you() {
        let ordinary = candidate(AttentionSourceFamily::CommsIngest, None);

        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    owner_approval_required: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::NeedsYou,
                reason: RouteReason::OwnerApprovalRequired,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    owner_intervention_required: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::NeedsYou,
                reason: RouteReason::OwnerInterventionRequired,
                priority: RoutePriority::Normal,
            }
        );
    }

    #[test]
    fn router_sends_monitor_access_problems_to_needs_you() {
        // Recurring Monitors Phase 3 (§5.5): repeated source-access failure
        // is an owner problem, not a change — Needs You, never Changed.
        let ordinary = candidate(AttentionSourceFamily::Task, None);
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    access_problem_detected: true,
                    // even when the same run also observed a change, the
                    // access problem candidate itself routes to Needs You
                    observed_change: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::NeedsYou,
                reason: RouteReason::AccessProblemDetected,
                priority: RoutePriority::Normal,
            }
        );
        // Owner dismissal still wins (funnel-owned suppression).
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    access_problem_detected: true,
                    owner_dismissed: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Dropped {
                reason: DropReason::OwnerDismissed,
            }
        );
    }

    #[test]
    fn router_sends_promises_and_actionable_comms_to_followups() {
        assert_eq!(
            route_attention_candidate(
                &candidate(AttentionSourceFamily::Promise, None),
                &AttentionRouteContext::default(),
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                reason: RouteReason::PromiseOrObligation,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &candidate(
                    AttentionSourceFamily::CommsIngest,
                    Some(AttentionActionKind::Reply)
                ),
                &AttentionRouteContext::default(),
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                reason: RouteReason::ActionableCommunication,
                priority: RoutePriority::Normal,
            }
        );
    }

    #[test]
    fn router_sends_operational_states_to_their_lanes() {
        let ordinary = candidate(AttentionSourceFamily::Work, None);

        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    active_work_state: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::ActiveWork,
                reason: RouteReason::ActiveWorkState,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    delivered_work: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::Delivered,
                reason: RouteReason::DeliveredWork,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    observed_change: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::Changed,
                reason: RouteReason::ObservedChange,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &ordinary,
                &AttentionRouteContext {
                    failed_work_report: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::Failed,
                reason: RouteReason::FailedWork,
                priority: RoutePriority::Normal,
            }
        );
    }

    #[test]
    fn router_requires_explicit_worth_a_look_eligibility_and_drops_weak_context() {
        let resurfacing = candidate(AttentionSourceFamily::Resurfacing, None);

        assert_eq!(
            route_attention_candidate(
                &resurfacing,
                &AttentionRouteContext {
                    non_actionable_useful_context: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Routed {
                lane: AttentionLane::WorthALook,
                reason: RouteReason::NonActionableUsefulContext,
                priority: RoutePriority::Normal,
            }
        );
        assert_eq!(
            route_attention_candidate(&resurfacing, &AttentionRouteContext::default()),
            RouteOutcome::Dropped {
                reason: DropReason::UnsupportedSource,
            }
        );
        assert_eq!(
            route_attention_candidate(
                &resurfacing,
                &AttentionRouteContext {
                    recency_only: true,
                    non_actionable_useful_context: true,
                    ..Default::default()
                },
            ),
            RouteOutcome::Dropped {
                reason: DropReason::RecencyOnly,
            }
        );
    }
}
