use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::agents::memory_consolidator::redact_secrets_in_value;

/// Shared fail-closed boundary for content that may be displayed, persisted,
/// or projected into an LLM prompt. This is deliberately enforced again by
/// the store so a future writer cannot bypass API/worker validation.
pub fn contains_secret_shaped_content(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if [
        "-----begin private key",
        "-----begin rsa private key",
        "api_key=",
        "api-key:",
        "password=",
        "password:",
        "client_secret",
        "authorization: bearer",
        "sk-proj-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    if lower.contains("-----begin ") && lower.contains("private key-----") {
        return true;
    }

    // Reuse the system's high-confidence token detector as a rejection
    // oracle. Town Square must reject, rather than silently publish a redacted
    // mutation of, provider keys, GitHub/Slack tokens, JWTs, bearer tokens, or
    // URL-embedded credentials.
    let mut candidate = Value::String(value.to_string());
    redact_secrets_in_value(&mut candidate);
    candidate.as_str() != Some(value)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub member_id: String,
    pub kind: String, // 'agent' | 'operator'
    pub display_name: String,
    pub introversion: f64,
    pub opted_out: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfState {
    pub member_id: String,
    pub valence: f64,
    pub energy: f64,
    pub baseline_valence: f64,
    pub baseline_energy: f64,
    pub note: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub post_id: String,
    pub author_id: String,
    pub surface: String, // 'feed' | 'group'
    pub group_id: Option<String>,
    pub post_type: String, // 'thought' | 'reply' | 'question' | 'link'
    pub body: String,
    pub parent_id: Option<String>,
    pub created_at: String,
}

/// One durable, scope-local social delivery.
///
/// `explicit_mention` deliveries are actionable and may produce one atomic
/// reply. `reply_notification` deliveries tell the parent author that a reply
/// exists, but are informational so an autonomous reply cannot recursively
/// create an unbounded reply loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingMention {
    pub mentioned_member_id: String,
    pub delivery_kind: String,
    pub post: Post,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub post_id: String,
    pub member_id: String,
    pub emoji: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub group_id: String,
    pub name: String,
    pub created_by: String,
    pub created_at: String,
}

/// One `mentions` row in full.
///
/// [`PendingMention`] carries only what the worker needed to act on a delivery;
/// this is every column, because the migration has to move the row rather than
/// react to it — including the two that record how a delivery was settled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MentionRow {
    pub post_id: String,
    pub mentioned_member_id: String,
    pub delivery_kind: String,
    pub status: String,
    /// May be the empty string: the column carries `DEFAULT ''` and rows
    /// written before it existed took that default. A consumer that requires a
    /// real timestamp has to supply one rather than trust this.
    pub created_at: String,
    pub handled_at: Option<String>,
    pub response_post_id: Option<String>,
}

/// One `group_members` row. The host table is a pure join with no surrogate
/// key, so a consumer that needs one derives it from the pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMembershipRow {
    pub group_id: String,
    pub member_id: String,
}

/// The single `operator_policy` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorPolicyRow {
    pub autonomous_enabled: bool,
    pub updated_at: String,
}
