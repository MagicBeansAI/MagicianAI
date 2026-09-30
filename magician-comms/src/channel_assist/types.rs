pub use magician::magician_v2::channel_types::{
    ChannelChangeFact, ChannelDetailStatus, ChannelFollowUpHint, ChannelInformationBrief,
    ChannelInformationType, ChannelRequiredActionKind, ChannelTemporalFact, ChannelTemporalKind,
    MailRecordOrigin, MessageDirection,
};
pub use magician::magician_v2::channel_types::{DistillState, MailMessageMeta};
use std::sync::LazyLock;

pub use magician::magician_v2::channel_types::ChannelLane;
use regex::Regex;
use serde::{Deserialize, Serialize};

static FULL_LINK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?://|www\.)[^\s<>\"']+"#).expect("valid full-link pattern")
});
static EMAIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").expect("valid email pattern")
});
static SECRET_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(api[_ -]?key|access[_ -]?token|refresh[_ -]?token|token|bearer|password|passwd|pwd|secret|otp|one[- ]time(?: password| code)?)\s*([:=]\s*|\s+)([A-Z0-9_./+=-]{4,})",
    )
    .expect("valid secret pattern")
});
static JWT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]{8,})?\b")
        .expect("valid JWT pattern")
});
static LABELED_IDENTIFIER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(account|acct|card|identifier|id|reference|ref|invoice|booking|order)\s*(?:no\.?|number|#|:|=)?\s*([A-Z0-9][A-Z0-9_-]{4,})\b",
    )
    .expect("valid labeled identifier pattern")
});

/// Wire schema version stamped on every channel-assist record. Bump when a
/// field is added/renamed so downstream consumers (extension, evals) can
/// branch on shape; `#[serde(default)]` keeps older payloads readable.
/// v3 (Phase 2b): persists local reply/follow-up hints on messages and the
/// exact message evidence classified by annotations. v4 preserves the complete
/// message-id batch for coalesced distillations. v5 adds structured
/// action-critical key details to follow-up hints/actions. v6 adds the
/// provider-neutral safe information brief and monotonic distill revision. v7
/// records the exact distill revision consumed by classification. v8 adds
/// normalized provider-side thread changes used by reconciliation. v9
/// materializes annotation attention/currentness for bounded Today reads.
pub const MAIL_ASSIST_SCHEMA_VERSION: u32 = 9;
pub const CHANNEL_ASSIST_SCHEMA_VERSION: u32 = MAIL_ASSIST_SCHEMA_VERSION;

/// Subject stored for rows suppressed by the sensitive-content heuristics.
/// Ids and sender domain are retained so sync reconciliation still works;
/// the original subject is never persisted for suppressed rows.
pub const REDACTED_SUBJECT_PLACEHOLDER: &str = "[subject suppressed]";

pub fn default_schema_version() -> u32 {
    MAIL_ASSIST_SCHEMA_VERSION
}

pub fn sanitize_follow_up_key_detail(raw: impl AsRef<str>, max_chars: usize) -> Option<String> {
    let compact = raw
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        return None;
    }
    let redacted = redact_model_sensitive_text(&compact);
    let trimmed: String = redacted.chars().take(max_chars).collect();
    if trimmed.trim().is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Normalize one persisted information-brief field. Information briefs share
/// the follow-up identifier policy, but additionally remove full links: source
/// adapters, rather than model output, own every URL exposed to the client.
pub fn sanitize_channel_brief_text(raw: impl AsRef<str>, max_chars: usize) -> Option<String> {
    sanitize_follow_up_key_detail(raw, max_chars)
}

fn redact_model_sensitive_text(value: &str) -> String {
    let without_links = FULL_LINK_PATTERN.replace_all(value, "[link omitted]");
    let without_emails = EMAIL_PATTERN.replace_all(&without_links, "[email omitted]");
    let without_jwts = JWT_PATTERN.replace_all(&without_emails, "[secret omitted]");
    let without_secrets =
        SECRET_PATTERN.replace_all(&without_jwts, |captures: &regex::Captures<'_>| {
            let value = captures.get(3).map_or("", |value| value.as_str());
            let separator = captures.get(2).map_or("", |value| value.as_str());
            let explicit_assignment = separator.contains(':') || separator.contains('=');
            let identifier_like = value.chars().any(|ch| ch.is_ascii_digit())
                || value.chars().any(|ch| !ch.is_ascii_alphabetic())
                || value.chars().count() >= 16
                || (value.chars().count() >= 4
                    && value
                        .chars()
                        .filter(|ch| ch.is_ascii_alphabetic())
                        .all(|ch| ch.is_ascii_uppercase()));
            if explicit_assignment || identifier_like {
                format!("{} [secret omitted]", &captures[1])
            } else {
                captures[0].to_string()
            }
        });
    let without_identifiers = LABELED_IDENTIFIER_PATTERN.replace_all(
        &without_secrets,
        |captures: &regex::Captures<'_>| {
            let value = captures
                .get(2)
                .map(|value| value.as_str())
                .unwrap_or_default();
            let identifier_like = value.chars().any(|ch| ch.is_ascii_digit())
                || value
                    .chars()
                    .filter(|ch| ch.is_ascii_alphabetic())
                    .all(|ch| ch.is_ascii_uppercase());
            if identifier_like {
                format!("{} ****{}", &captures[1], last_chars(value, 4))
            } else {
                captures[0].to_string()
            }
        },
    );
    redact_identifier_numbers(&without_identifiers)
}

fn last_chars(value: &str, count: usize) -> &str {
    let start = value
        .char_indices()
        .rev()
        .nth(count.saturating_sub(1))
        .map(|(index, _)| index)
        .unwrap_or(0);
    &value[start..]
}

fn redact_identifier_numbers(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            out.push(chars[index]);
            index += 1;
            continue;
        }

        let start = index;
        let (end, digits, saw_separator) = collect_digit_span(&chars, start);
        let original: String = chars[start..end].iter().collect();
        let context = nearby_context(&chars, start, end).to_ascii_lowercase();
        let explicit_last4 = has_last4_context(&context) && digits.len() <= 4;
        let should_redact = !explicit_last4
            && !looks_like_calendar_date(&original)
            && !looks_like_amount_context(&context)
            && ((saw_separator && digits.len() >= 12)
                || digits.len() >= 9
                || (has_identifier_context(&context) && digits.len() >= 5));

        if should_redact {
            out.push_str("****");
            out.push_str(last_digits(&digits, 4));
        } else {
            out.push_str(&original);
        }
        index = end;
    }
    out
}

fn collect_digit_span(chars: &[char], start: usize) -> (usize, String, bool) {
    let mut index = start;
    let mut digits = String::new();
    let mut saw_separator = false;
    loop {
        while index < chars.len() && chars[index].is_ascii_digit() {
            digits.push(chars[index]);
            index += 1;
        }
        if index + 1 < chars.len()
            && matches!(chars[index], ' ' | '-')
            && chars[index + 1].is_ascii_digit()
        {
            saw_separator = true;
            index += 1;
            continue;
        }
        break;
    }
    (index, digits, saw_separator)
}

fn nearby_context(chars: &[char], start: usize, end: usize) -> String {
    let window_start = start.saturating_sub(32);
    let window_end = (end + 32).min(chars.len());
    chars[window_start..window_end].iter().collect()
}

fn last_digits(value: &str, count: usize) -> &str {
    let start = value.len().saturating_sub(count);
    &value[start..]
}

fn has_last4_context(context: &str) -> bool {
    ["last4", "last 4", "ending", "ends with", "ending in"]
        .iter()
        .any(|needle| context.contains(needle))
}

fn has_identifier_context(context: &str) -> bool {
    [
        "account",
        "acct",
        "a/c",
        "card",
        "number",
        " no.",
        " no ",
        " id ",
        " id:",
        "id:",
        "identifier",
        "ssn",
        "aadhaar",
        "pan",
        "phone",
        "mobile",
        "reference",
        " ref ",
        " ref:",
        "ref:",
        "invoice",
        "booking",
        "order",
    ]
    .iter()
    .any(|needle| context.contains(needle))
        || context.starts_with("id ")
        || context.starts_with("ref ")
}

fn looks_like_amount_context(context: &str) -> bool {
    let has_identifier_marker = [
        " id ",
        " id:",
        "id:",
        "number",
        " no.",
        "reference",
        " ref ",
        " ref:",
        "ref:",
        "invoice",
        "booking",
        "order",
    ]
    .iter()
    .any(|needle| context.contains(needle));
    if has_identifier_marker {
        return false;
    }
    [
        "amount",
        "balance",
        "outstanding",
        "payable",
        "total",
        "minimum due",
        "min due",
        "due amount",
        "paid",
        "₹",
        "$",
        "rs ",
        "inr",
        "usd",
        "eur",
        "gbp",
    ]
    .iter()
    .any(|needle| context.contains(needle))
}

fn looks_like_calendar_date(span: &str) -> bool {
    if !span.contains('-') {
        return false;
    }
    let parts = span.split('-').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return false;
    }
    parts
        .iter()
        .all(|part| part.chars().all(|ch| ch.is_ascii_digit()) && matches!(part.len(), 2 | 4))
}

/// Annotation lifecycle. The happy path runs observed → classified →
/// needs_approval → approved → scheduled → draft_requested → draft_ready →
/// inserted → sent_detected → completed; dismissed/stale/superseded/errored
/// are terminal-ish side exits reachable from any active state. Phase 1
/// only populates observed/classified (seeded) and dismissed; the full enum
/// is the contract for later phases.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailAnnotationState {
    Observed,
    Classified,
    NeedsApproval,
    Approved,
    /// Positive acknowledgement with NO immediate action — the owner saw it and
    /// it's fine as-is. Clears the card like `approved`, but creates no task; a
    /// distinct state so the audit + feedback signal stays legible.
    Acknowledged,
    Scheduled,
    DraftRequested,
    DraftReady,
    Inserted,
    SentDetected,
    Completed,
    Dismissed,
    Stale,
    Superseded,
    Errored,
}

impl MailAnnotationState {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Classified => "classified",
            Self::NeedsApproval => "needs_approval",
            Self::Approved => "approved",
            Self::Acknowledged => "acknowledged",
            Self::Scheduled => "scheduled",
            Self::DraftRequested => "draft_requested",
            Self::DraftReady => "draft_ready",
            Self::Inserted => "inserted",
            Self::SentDetected => "sent_detected",
            Self::Completed => "completed",
            Self::Dismissed => "dismissed",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
            Self::Errored => "errored",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "observed" => Ok(Self::Observed),
            "classified" => Ok(Self::Classified),
            "needs_approval" => Ok(Self::NeedsApproval),
            "approved" => Ok(Self::Approved),
            "acknowledged" => Ok(Self::Acknowledged),
            "scheduled" => Ok(Self::Scheduled),
            "draft_requested" => Ok(Self::DraftRequested),
            "draft_ready" => Ok(Self::DraftReady),
            "inserted" => Ok(Self::Inserted),
            "sent_detected" => Ok(Self::SentDetected),
            "completed" => Ok(Self::Completed),
            "dismissed" => Ok(Self::Dismissed),
            "stale" => Ok(Self::Stale),
            "superseded" => Ok(Self::Superseded),
            "errored" => Ok(Self::Errored),
            other => anyhow::bail!("unknown mail annotation state: {other}"),
        }
    }
}

/// Who caused an audit event: the human owner (dismiss/feedback/approve) or
/// the background sync/classifier worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailAssistActor {
    User,
    Worker,
}

impl MailAssistActor {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Worker => "worker",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "worker" => Ok(Self::Worker),
            other => anyhow::bail!("unknown mail assist actor: {other}"),
        }
    }
}

/// Version of the provider-neutral safe information brief produced by the
/// local channel distiller. This version is intentionally independent from
/// [`MAIL_ASSIST_SCHEMA_VERSION`]: prompt/brief revisions can evolve without
/// changing every channel-assist API record.
pub const CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRequiredActionSource {
    NeedsReplyHint,
    FollowUpHint,
    Intent,
    InformationBrief,
}

impl ChannelRequiredActionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsReplyHint => "needs_reply_hint",
            Self::FollowUpHint => "follow_up_hint",
            Self::Intent => "intent",
            Self::InformationBrief => "information_brief",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelRequiredAction {
    pub kind: ChannelRequiredActionKind,
    pub source: ChannelRequiredActionSource,
}

/// Map compatibility hints and the V2 information brief into For you work.
/// For you is owner obligation only: reply, handle, schedule, a deadline you
/// owe. News, FYI, and audience invitations belong in Worth a look and must
/// not enter here from a verb in the brief.
/// Whether a brief's `stated_action` actually states an action.
///
/// The contract intends `stated_action: null` when nothing is required, but the
/// distiller frequently writes the *words* instead — "None", "None required;
/// informational newsletter", "No action needed". 332 of 4,908 stored briefs
/// (6.8%) do this, 76 of them the bare string "none". A non-empty check reads
/// every one of them as work, so the model's own statement that nothing is
/// needed becomes the evidence that something is.
///
/// This is a sentinel-normalization, not a content judgement: it decides whether
/// a field is populated, not whether a message deserves attention.
fn stated_action_is_actionable(stated_action: &str) -> bool {
    let normalized = stated_action.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    // Anchored at the start: a real action that merely mentions "no" later
    // ("Confirm no changes are needed before Friday") still counts.
    const ABSENT_SENTINELS: [&str; 7] = [
        "none",
        "no action",
        "no specific action",
        "no further action",
        "not required",
        "nothing",
        "n/a",
    ];
    !ABSENT_SENTINELS
        .iter()
        .any(|sentinel| normalized.starts_with(sentinel))
}

pub fn derive_channel_required_action(
    intent: Option<&str>,
    needs_reply_hint: bool,
    follow_up_hint: Option<&ChannelFollowUpHint>,
    brief: Option<&ChannelInformationBrief>,
) -> Option<ChannelRequiredAction> {
    if needs_reply_hint {
        return Some(ChannelRequiredAction {
            kind: ChannelRequiredActionKind::Reply,
            source: ChannelRequiredActionSource::NeedsReplyHint,
        });
    }

    if let Some(hint) = follow_up_hint {
        let kind = hint.kind.trim().to_ascii_lowercase();
        let action_kind = match kind.as_str() {
            "needs_reply" | "reply" => Some(ChannelRequiredActionKind::Reply),
            "schedule" | "meeting" => Some(ChannelRequiredActionKind::Schedule),
            "follow_up" | "owner_owes" | "waiting_on" => Some(ChannelRequiredActionKind::FollowUp),
            // Unknown / FYI / "check this out" hints are information, not work.
            _ => None,
        };
        if let Some(kind) = action_kind {
            return Some(ChannelRequiredAction {
                kind,
                source: ChannelRequiredActionSource::FollowUpHint,
            });
        }
    }

    match intent
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("needs_reply" | "reply") => {
            return Some(ChannelRequiredAction {
                kind: ChannelRequiredActionKind::Reply,
                source: ChannelRequiredActionSource::Intent,
            });
        },
        Some("action_request" | "follow_up" | "owner_owes" | "waiting_on") => {
            return Some(ChannelRequiredAction {
                kind: ChannelRequiredActionKind::FollowUp,
                source: ChannelRequiredActionSource::Intent,
            });
        },
        _ => {},
    }

    let brief = brief?;
    let has_stated_action = brief
        .stated_action
        .as_deref()
        .is_some_and(stated_action_is_actionable);
    let kind = match brief.information_type {
        ChannelInformationType::Scheduling if has_stated_action => {
            Some(ChannelRequiredActionKind::Schedule)
        },
        ChannelInformationType::Deadline => Some(ChannelRequiredActionKind::FollowUp),
        // Owner work only. General information, promotions, events, and
        // untyped briefs are Worth-a-look material even when they contain a
        // verb — "read the digest", "see details", "claim 50% off".
        ChannelInformationType::Request
        | ChannelInformationType::Transaction
        | ChannelInformationType::ChangeNotice
            if has_stated_action =>
        {
            Some(ChannelRequiredActionKind::FollowUp)
        },
        ChannelInformationType::Promotion
        | ChannelInformationType::Event
        | ChannelInformationType::GeneralInformation
        | ChannelInformationType::Other => None,
        _ => None,
    }?;
    Some(ChannelRequiredAction {
        kind,
        source: ChannelRequiredActionSource::InformationBrief,
    })
}

/// Audit event kinds appended to `mail_assist_events`. Dismissal is a
/// state transition but gets its own kind so audits read cleanly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailAssistEventType {
    AnnotationCreated,
    AnnotationUpdated,
    StateTransition,
    Dismissed,
    Feedback,
    /// Provider-side state changed without necessarily adding a message
    /// (archive, trash/delete, or labels).
    ProviderChange,
}

impl MailAssistEventType {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::AnnotationCreated => "annotation_created",
            Self::AnnotationUpdated => "annotation_updated",
            Self::StateTransition => "state_transition",
            Self::Dismissed => "dismissed",
            Self::Feedback => "feedback",
            Self::ProviderChange => "provider_change",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "annotation_created" => Ok(Self::AnnotationCreated),
            "annotation_updated" => Ok(Self::AnnotationUpdated),
            "state_transition" => Ok(Self::StateTransition),
            "dismissed" => Ok(Self::Dismissed),
            "feedback" => Ok(Self::Feedback),
            "provider_change" => Ok(Self::ProviderChange),
            other => anyhow::bail!("unknown mail assist event type: {other}"),
        }
    }
}

/// Provider-side thread changes that can reconcile an active recommendation
/// even when no new message exists. This normalized shape carries only ids
/// and label names, never message content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderThreadChangeKind {
    MessageDeleted,
    LabelsAdded,
    LabelsRemoved,
}

impl ProviderThreadChangeKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::MessageDeleted => "message_deleted",
            Self::LabelsAdded => "labels_added",
            Self::LabelsRemoved => "labels_removed",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "message_deleted" => Ok(Self::MessageDeleted),
            "labels_added" => Ok(Self::LabelsAdded),
            "labels_removed" => Ok(Self::LabelsRemoved),
            other => anyhow::bail!("unknown provider thread change kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderThreadChange {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Stable provider-derived id so replay before watermark advancement is
    /// idempotent.
    pub id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub kind: ProviderThreadChangeKind,
    /// True only when the provider no longer has a resource for this thread.
    /// A `message_deleted` history delta alone is not sufficient: deleting an
    /// older message from a live thread must not close the thread's active work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thread_removed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub label_ids: Vec<String>,
    /// Provider labels on the current thread snapshot fetched in the same sync
    /// segment. Reconciliation uses the final state, not a transient history
    /// delta that may have been reversed later in the stream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_label_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cursor: Option<String>,
    pub observed_at: i64,
}

impl ProviderThreadChange {
    /// Changes that mean the provider no longer presents the item as active
    /// inbox work. All other label changes remain audit-only.
    pub fn closes_active_work(&self) -> bool {
        match self.kind {
            ProviderThreadChangeKind::MessageDeleted => self.thread_removed,
            ProviderThreadChangeKind::LabelsAdded => self
                .current_label_ids
                .iter()
                .any(|label| matches!(label.as_str(), "TRASH" | "SPAM")),
            ProviderThreadChangeKind::LabelsRemoved => {
                self.label_ids.iter().any(|label| label == "INBOX")
                    && !self.current_label_ids.iter().any(|label| label == "INBOX")
            },
        }
    }
}

/// Current state per (provider, account_alias, thread_id) — updated
/// in place on every sync pass. Captured-fields contract (design §1/§2):
/// subject (or redacted), latest sender name/address, recipient DOMAINS
/// only, label ids, counts and timestamps. Never bodies, snippets,
/// attachments, or full recipient lists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailThreadRecord {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub provider: String,
    pub account_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    pub thread_id: String,
    /// Assistance lane inherited from the ingesting channel account.
    #[serde(default)]
    pub lane: ChannelLane,
    /// Latest subject; [`REDACTED_SUBJECT_PLACEHOLDER`] when
    /// `sensitive_suppressed` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling locally-derived summary of the thread's newest distilled
    /// message — populated ONLY by the local distiller (N3), never by
    /// sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_from_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_from_address: Option<String>,
    /// Recipient DOMAINS only — the privacy contract forbids full
    /// recipient lists.
    #[serde(default)]
    pub recipient_domains: Vec<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    pub message_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<i64>,
    /// Opaque per-provider incremental-sync cursor observed on this thread
    /// (for gmail it holds the Gmail `historyId`) — optional because a
    /// provider may not expose an incremental cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cursor: Option<String>,
    #[serde(default)]
    pub sensitive_suppressed: bool,
    pub origin: MailRecordOrigin,
    pub first_observed_at: i64,
    pub last_observed_at: i64,
}

/// Current annotation state per thread. Dismissal/approval NEVER delete —
/// they transition `state` and append a [`MailAssistEvent`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailThreadAnnotation {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    /// Assistance lane, inherited from the annotated thread at create
    /// time (the store resolves it — see `create_annotation`).
    #[serde(default)]
    pub lane: ChannelLane,
    pub state: MailAnnotationState,
    /// Classifier label (e.g. follow-up category) — free-form until the
    /// Phase 2 classifier fixes the vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// References to the evidence backing this annotation (message ids,
    /// eval case ids) — never raw content.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// Exact distilled message the classifier acted on. Enables fresh
    /// reclassification when a newer message in the same thread is distilled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_message_at: Option<i64>,
    /// Scope-local distill revision consumed by the classifier. A nullable
    /// value identifies legacy or deterministic provisional annotations that
    /// still need a revision-bound classifier pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification_input_revision: Option<i64>,
    /// Independently validated semantic extraction from the same
    /// `channel_classify` call, bound to `classification_input_revision`.
    /// Missing/invalid extraction is persisted as status, not retried or used
    /// to alter the legacy route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_features: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_action: Option<serde_json::Value>,
    /// Classifier run id — always `None` in Phase 1 (no classifier yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Contract type for Phase 2+ follow-up detection output. Defined now so
/// the record shape is stable; not populated in Phase 1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailFollowUpCandidate {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub annotation_id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub created_at: i64,
}

/// Contract type for a proposed reply draft (Phase 2+; not populated in
/// Phase 1). Carries intent only — draft content lives in the artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailDraftCandidate {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub annotation_id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_summary: Option<String>,
    pub created_at: i64,
}

/// Contract type for a materialized draft (Phase 2+; not populated in
/// Phase 1). References the produced artifact and, once inserted into the
/// mailbox, the provider-side draft id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailDraftArtifact {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub draft_candidate_id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_draft_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailFeedbackVerdict {
    Helpful,
    NotHelpful,
    WrongLabel,
    Other,
}

/// Typed user feedback on an annotation. Persisted as a `feedback` audit
/// event (the record travels in the event's `detail`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailAssistUserFeedback {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub annotation_id: String,
    pub provider: String,
    pub account_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub verdict: MailFeedbackVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub actor: MailAssistActor,
    pub created_at: i64,
}

/// Append-only audit row. Rows are NEVER updated or deleted — corrections
/// append new events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailAssistEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation_id: Option<String>,
    pub provider: String,
    pub account_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub event_type: MailAssistEventType,
    pub actor: MailAssistActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_state: Option<MailAnnotationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_state: Option<MailAnnotationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    pub created_at: i64,
}

/// Per-account sync cursor. Supports both incremental strategies: an
/// opaque provider cursor (`provider_cursor`, when the channel exposes
/// one) and the watermark re-list fallback (`after:` on
/// `last_internal_date`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncWatermark {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub provider: String,
    pub account_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_internal_date: Option<i64>,
    /// Opaque per-provider incremental cursor — the provider decides what
    /// it means (for gmail it holds the `history.list`/`getProfile`
    /// `historyId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cursor: Option<String>,
    pub last_synced_at: i64,
    /// Last sync error for this account, surfaced in `sync/status`;
    /// cleared on the next successful pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magician::magician_v2::attention::resurfacing::scoring::supported_temporal_markers_ms;

    #[test]
    fn key_detail_sanitizer_preserves_action_details_and_masks_identifiers() {
        assert_eq!(
            sanitize_follow_up_key_detail("Card 1234 5678 9012 3456 due Jul 12", 120).as_deref(),
            Some("Card ****3456 due Jul 12")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Amount: INR 125000 due July 12, 2026 at 10:30 AM", 120)
                .as_deref(),
            Some("Amount: INR 125000 due July 12, 2026 at 10:30 AM")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Paid by Rahul: 125000", 120).as_deref(),
            Some("Paid by Rahul: 125000")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Preferred amount: 125000", 120).as_deref(),
            Some("Preferred amount: 125000")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Due: 2026-07-12", 120).as_deref(),
            Some("Due: 2026-07-12")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Card ending 1234", 120).as_deref(),
            Some("Card ending 1234")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Phone 9876543210", 120).as_deref(),
            Some("Phone ****3210")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Account 12345-67890", 120).as_deref(),
            Some("Account ****7890")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Reference ABC 123456", 120).as_deref(),
            Some("Reference ABC ****3456")
        );
        assert_eq!(
            sanitize_follow_up_key_detail("Ref 123456", 120).as_deref(),
            Some("Ref ****3456")
        );
        assert_eq!(
            sanitize_follow_up_key_detail(
                "Amount INR 125000 due 2026-07-12; token=super-secret-value",
                160,
            )
            .as_deref(),
            Some("Amount INR 125000 due 2026-07-12; token [secret omitted]")
        );
        assert_eq!(
            sanitize_follow_up_key_detail(
                "Contact owner@example.com at https://example.com/pay",
                160,
            )
            .as_deref(),
            Some("Contact [email omitted] at [link omitted]")
        );
        assert_eq!(
            sanitize_follow_up_key_detail(
                "LPG booking changes and credit card policy updates are effective July 12",
                160,
            )
            .as_deref(),
            Some("LPG booking changes and credit card policy updates are effective July 12")
        );
        assert_eq!(
            sanitize_follow_up_key_detail(
                "Invoice overdue; order delayed; password policy changed",
                160,
            )
            .as_deref(),
            Some("Invoice overdue; order delayed; password policy changed")
        );
    }

    #[test]
    fn information_brief_sanitizer_removes_links_and_masks_identifiers() {
        assert_eq!(
            sanitize_channel_brief_text(
                "Review https://bank.example/policy?account=123456789 and card 4111 1111 1111 1234",
                160,
            )
            .as_deref(),
            Some("Review [link omitted] and card ****1234")
        );
        assert_eq!(
            sanitize_channel_brief_text("Effective: 2026-07-01; cap: INR 15,000", 160).as_deref(),
            Some("Effective: 2026-07-01; cap: INR 15,000")
        );
    }

    fn brief(
        information_type: ChannelInformationType,
        temporal_kind: Option<ChannelTemporalKind>,
        stated_action: Option<&str>,
    ) -> ChannelInformationBrief {
        ChannelInformationBrief {
            schema_version: CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
            information_type,
            summary: "Safe summary".to_string(),
            key_facts: Vec::new(),
            changes: Vec::new(),
            temporal_facts: temporal_kind
                .map(|kind| ChannelTemporalFact {
                    kind,
                    text: "July 15, 2026".to_string(),
                    at_ms: None,
                    timezone: None,
                })
                .into_iter()
                .collect(),
            stated_action: stated_action.map(str::to_string),
            detail_status: ChannelDetailStatus::Complete,
            missing_details: Vec::new(),
        }
    }

    #[test]
    fn required_action_mapping_covers_due_reply_payment_and_schedule_evidence() {
        let due = brief(
            ChannelInformationType::Deadline,
            Some(ChannelTemporalKind::Due),
            None,
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&due)).map(|action| action.kind),
            Some(ChannelRequiredActionKind::FollowUp)
        );

        let expiry = brief(
            ChannelInformationType::Deadline,
            Some(ChannelTemporalKind::Expiry),
            None,
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&expiry))
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::FollowUp)
        );

        let payment = brief(
            ChannelInformationType::Transaction,
            None,
            Some("Pay INR 1,250"),
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&payment))
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::FollowUp)
        );
        assert_eq!(
            derive_channel_required_action(Some("fyi"), true, None, None).map(|action| action.kind),
            Some(ChannelRequiredActionKind::Reply)
        );

        let scheduling = ChannelFollowUpHint {
            kind: "schedule".to_string(),
            ..Default::default()
        };
        assert_eq!(
            derive_channel_required_action(None, false, Some(&scheduling), None)
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::Schedule)
        );

        let fyi = ChannelFollowUpHint {
            kind: "newsletter_digest".to_string(),
            ..Default::default()
        };
        assert_eq!(
            derive_channel_required_action(None, false, Some(&fyi), None),
            None
        );
    }

    #[test]
    fn informational_effective_dates_and_source_gaps_do_not_imply_work() {
        let effective = brief(
            ChannelInformationType::ChangeNotice,
            Some(ChannelTemporalKind::Effective),
            None,
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&effective)),
            None
        );

        let mut source_gap = brief(ChannelInformationType::ChangeNotice, None, None);
        source_gap.detail_status = ChannelDetailStatus::SourceOmitsDetails;
        source_gap.missing_details = vec!["The changed amount".to_string()];
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&source_gap)),
            None
        );
    }

    /// The distiller writes "None required" into `stated_action` instead of
    /// emitting null, and a non-empty check reads that as work — 332 of 4,908
    /// stored briefs, 76 of them the bare string "none".
    #[test]
    fn a_stated_action_that_says_none_is_not_a_stated_action() {
        for absent in [
            "None",
            "none",
            "None required; informational newsletter content.",
            "None specified in content.",
            "No action needed",
            "no further action required",
            "Not required",
            "N/A",
            "  none  ",
            "",
        ] {
            let brief = brief(
                ChannelInformationType::GeneralInformation,
                None,
                Some(absent),
            );
            assert_eq!(
                derive_channel_required_action(None, false, None, Some(&brief)),
                None,
                "{absent:?} must not imply work"
            );
        }
    }

    /// The sentinel check is anchored at the start, so a real instruction that
    /// happens to contain a negation still counts.
    #[test]
    fn a_real_action_mentioning_no_still_implies_work() {
        for present in [
            "Confirm no changes are needed before Friday",
            "Pay the invoice by the 15th",
            "Nominate a reviewer",
        ] {
            let brief = brief(ChannelInformationType::Request, None, Some(present));
            assert_eq!(
                derive_channel_required_action(None, false, None, Some(&brief))
                    .map(|action| action.kind),
                Some(ChannelRequiredActionKind::FollowUp),
                "{present:?} must still imply work"
            );
        }
    }

    /// A promotion or a public event states an action for its audience, not for
    /// the owner, so the brief alone must not manufacture follow-up work from
    /// it. These are the two largest stated-action populations in a real store,
    /// and reading them as work is what fills the lane with marketing.
    #[test]
    fn audience_addressed_stated_actions_do_not_imply_owner_work() {
        let promotion = brief(
            ChannelInformationType::Promotion,
            None,
            Some("Claim 50% off the course"),
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&promotion)),
            None
        );

        let event = brief(
            ChannelInformationType::Event,
            None,
            Some("Take the online quiz before 15 August"),
        );
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&event)),
            None
        );
    }

    /// General information and untyped briefs stay out of For you even when
    /// they contain a verb. That is how newsletters and "see details" mail
    /// stop looking like work.
    #[test]
    fn informational_briefs_do_not_become_for_you_from_a_stated_action() {
        for information_type in [
            ChannelInformationType::GeneralInformation,
            ChannelInformationType::Other,
            ChannelInformationType::Promotion,
            ChannelInformationType::Event,
        ] {
            let brief = brief(
                information_type,
                None,
                Some("Read the latest product updates"),
            );
            assert_eq!(
                derive_channel_required_action(None, false, None, Some(&brief)),
                None,
                "{information_type:?} must stay out of For you"
            );
        }
    }

    /// The suppression is scoped to how the brief is shaped, not to whether a
    /// date is present: an owner-addressed deadline, request, transaction, or
    /// change notice still routes exactly as before.
    #[test]
    fn owner_addressed_briefs_still_imply_work() {
        let deadline = brief(ChannelInformationType::Deadline, None, None);
        assert_eq!(
            derive_channel_required_action(None, false, None, Some(&deadline))
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::FollowUp)
        );

        for information_type in [
            ChannelInformationType::Request,
            ChannelInformationType::Transaction,
            ChannelInformationType::ChangeNotice,
        ] {
            let owed = brief(information_type, None, Some("Pay the invoice by the 15th"));
            assert_eq!(
                derive_channel_required_action(None, false, None, Some(&owed))
                    .map(|action| action.kind),
                Some(ChannelRequiredActionKind::FollowUp),
                "{information_type:?} must still imply work"
            );
        }
    }

    /// Explicit evidence is read before the brief, so a promotion the owner
    /// actually engaged with keeps its action.
    #[test]
    fn explicit_evidence_still_wins_over_an_audience_addressed_brief() {
        let promotion = brief(
            ChannelInformationType::Promotion,
            None,
            Some("Claim 50% off the course"),
        );
        assert_eq!(
            derive_channel_required_action(None, true, None, Some(&promotion))
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::Reply)
        );
        assert_eq!(
            derive_channel_required_action(Some("action_request"), false, None, Some(&promotion))
                .map(|action| action.kind),
            Some(ChannelRequiredActionKind::FollowUp)
        );
    }

    #[test]
    fn supported_temporal_markers_ignore_model_epochs_and_preserve_date_only_semantics() {
        let markers = supported_temporal_markers_ms(
            "Due 2026-07-15; effective July 20, 2026; invalid 2026-02-31",
        );
        assert_eq!(markers.len(), 2);
        assert_eq!(
            chrono::DateTime::from_timestamp_millis(markers[0])
                .unwrap()
                .date_naive(),
            chrono::NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()
        );
        assert!(markers.iter().all(|marker| marker % 86_400_000 == 0));
    }

    /// Locks the provider-neutral wire contract: rows serialize with the
    /// neutral identity names (`thread_id`, `message_id`,
    /// `provider_cursor`) and never with provider-coupled `gmail_*` /
    /// `*history_id` names. These types serialize directly into the
    /// channel-assist API responses, so this IS the API contract.
    #[test]
    fn wire_serialization_uses_provider_neutral_identity_names() {
        let thread = MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "t-1".to_string(),
            lane: ChannelLane::Envoy,
            subject: None,
            latest_summary: None,
            latest_from_name: None,
            latest_from_address: None,
            recipient_domains: Vec::new(),
            label_ids: Vec::new(),
            message_count: 1,
            last_message_at: None,
            provider_cursor: Some("cursor-1".to_string()),
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: 1,
            last_observed_at: 1,
        };
        let message = MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "t-1".to_string(),
            message_id: "m-1".to_string(),
            provider_cursor: Some("cursor-1".to_string()),
            label_ids: Vec::new(),
            subject: None,
            from_name: None,
            from_address: None,
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: 1,
            observed_at: 1,
            direction: Some(MessageDirection::Inbound),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        };
        let annotation = MailThreadAnnotation {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "ann-1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            thread_id: "t-1".to_string(),
            lane: ChannelLane::UserAssist,
            state: MailAnnotationState::Observed,
            label: None,
            confidence: None,
            reason: None,
            evidence_refs: Vec::new(),
            evidence_message_id: None,
            evidence_message_at: None,
            classification_input_revision: None,
            semantic_features: None,
            proposed_action: None,
            provenance: None,
            created_at: 1,
            updated_at: 1,
        };
        let watermark = SyncWatermark {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            last_internal_date: None,
            provider_cursor: Some("cursor-1".to_string()),
            last_synced_at: 1,
            last_error: None,
        };

        let thread_json = serde_json::to_string(&thread).unwrap();
        let message_json = serde_json::to_string(&message).unwrap();
        let annotation_json = serde_json::to_string(&annotation).unwrap();
        let watermark_json = serde_json::to_string(&watermark).unwrap();

        for json in [&thread_json, &message_json, &annotation_json] {
            assert!(
                json.contains("\"thread_id\":\"t-1\""),
                "neutral thread_id missing: {json}"
            );
        }
        assert!(message_json.contains("\"message_id\":\"m-1\""));
        for json in [&thread_json, &message_json, &watermark_json] {
            assert!(
                json.contains("\"provider_cursor\":\"cursor-1\""),
                "neutral provider_cursor missing: {json}"
            );
        }
        // The provider-coupled names must never reappear on the wire (the
        // `provider` VALUE "gmail" is fine — only field names are checked).
        for json in [
            &thread_json,
            &message_json,
            &annotation_json,
            &watermark_json,
        ] {
            assert!(!json.contains("gmail_"), "gmail_* field leaked: {json}");
            assert!(
                !json.contains("history_id"),
                "history_id field leaked: {json}"
            );
        }

        // Phase 1b additions serialize in snake_case with their db-string
        // values (serde and as_db_str stay in agreement).
        assert!(thread_json.contains("\"lane\":\"envoy\""));
        assert!(annotation_json.contains("\"lane\":\"user_assist\""));
        assert!(message_json.contains("\"direction\":\"inbound\""));
        assert!(message_json.contains("\"distill_state\":\"pending\""));
        assert!(message_json.contains("\"distill_attempts\":0"));
    }

    /// Locks serde ⇄ db-string agreement for the Phase 1b enums (the same
    /// invariant the lifecycle enum relies on).
    #[test]
    fn phase1b_enums_roundtrip_db_strings() {
        for lane in [ChannelLane::UserAssist, ChannelLane::Envoy] {
            assert_eq!(ChannelLane::from_db_str(lane.as_db_str()).unwrap(), lane);
        }
        for direction in [MessageDirection::Inbound, MessageDirection::Outbound] {
            assert_eq!(
                MessageDirection::from_db_str(direction.as_db_str()).unwrap(),
                direction
            );
        }
        for state in [
            DistillState::Pending,
            DistillState::Done,
            DistillState::Skipped,
            DistillState::Suppressed,
            DistillState::Failed,
        ] {
            assert_eq!(DistillState::from_db_str(state.as_db_str()).unwrap(), state);
        }
        assert!(ChannelLane::from_db_str("bogus").is_err());
        assert!(MessageDirection::from_db_str("bogus").is_err());
        assert!(DistillState::from_db_str("bogus").is_err());
        assert_eq!(ChannelLane::default(), ChannelLane::UserAssist);
        assert_eq!(DistillState::default(), DistillState::Pending);
    }
}
