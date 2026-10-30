//! Review before sending.
//!
//! With `outward_actions.review_captured_sends` on (the default), an outward
//! send that its context would capture pauses its run instead, as a
//! confirmation the owner answers on Attention and the phones:
//!
//! - **Send** — this exact call, once. The approving resume registers a
//!   one-time pass ([`register_bound_waiver`]) for the run's replay; the capture
//!   branch of `execute_action_inner` takes it ([`take_bound_waiver`]) and
//!   dispatches. Every other gate runs again on the replay.
//! - **Discard** — the run continues with [`OutwardReview::discard_note`];
//!   nothing is sent.
//! - **Always allow this agent to send to this recipient** — Send, and a
//!   standing grant ([`grant`]) for that agent, channel and recipient. A grant
//!   lifts capture only; suppression, recipient compliance and preference
//!   checks still apply.
//!
//! A review left unanswered for [`REVIEW_DEADLINE_MS`] settles as discarded.
//! See `docs/components/magician/outward-actions.md` ("Review before sending").

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::approval_preview::ApprovalPreview;

/// How long a review waits for the owner before it settles as discarded.
pub const REVIEW_DEADLINE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// `input_schema.escalation_trigger` on the review's HITL request.
pub const ESCALATION_TRIGGER: &str = "outward_review";
pub const OPTION_SEND: &str = "send";
pub const OPTION_DISCARD: &str = "discard";
pub const OPTION_ALWAYS_ALLOW: &str = "always_allow_recipient";

/// Longest message excerpt shown on the card.
const EXCERPT_CHARS: usize = 600;
/// How long an approval's one-time pass waits for the replay to reach it. As
/// long as the review itself: a preference check after Send can hold the
/// replay for the owner again, and a pass that lapsed meanwhile would ask for
/// the review a second time. The pass names one execution and one exact call.
const WAIVER_TTL_MS: i64 = REVIEW_DEADLINE_MS;
/// How long a pass armed for one dispatch attempt waits for its capture check.
const ARMED_TTL_MS: i64 = 10 * 60 * 1000;

/// What a review pause holds back. Stored on the pause and covered by its
/// authorization hash, so the card the owner answers is the act replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutwardReview {
    pub capability: String,
    pub action: String,
    /// `mail`, `message`, `calendar_invite`, `form_submission`.
    pub class: String,
    pub recipients: Vec<String>,
    pub agent_id: String,
    pub deadline_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// The message refers to stored content that could not be shown here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub content_by_reference: bool,
    /// Scope-keyed binding of the resolved reference values shown at review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_fingerprint: Option<String>,
    /// The mailbox a profile-selector sender sends from (gmail/calendar
    /// `account`): the alias the act names, or `default (<alias>)` when it
    /// names none. `None` where the sender is the transport or the runtime's
    /// identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sending_account: Option<String>,
}

/// Fingerprint only referenced values: later credential preparation may change
/// other arguments. Nested JSON strings follow the same reference syntax as
/// `agentic::value_refs`; no referenced content is persisted by this helper.
pub fn reference_fingerprint(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    original: &Value,
    resolved: &Value,
) -> Result<Option<String>, &'static str> {
    fn collect(
        original: &Value,
        resolved: &Value,
        path: &mut Vec<String>,
        values: &mut BTreeMap<String, Value>,
    ) -> Result<(), &'static str> {
        match original {
            Value::Object(map) if map.contains_key("$ref") || map.contains_key("$file") => {
                values.insert(
                    serde_json::to_string(path).map_err(|_| "reference_path_invalid")?,
                    resolved.clone(),
                );
            },
            Value::Object(map) => {
                for (name, value) in map {
                    path.push(name.clone());
                    collect(
                        value,
                        resolved.get(name).unwrap_or(&Value::Null),
                        path,
                        values,
                    )?;
                    path.pop();
                }
            },
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    path.push(index.to_string());
                    collect(
                        value,
                        resolved.get(index).unwrap_or(&Value::Null),
                        path,
                        values,
                    )?;
                    path.pop();
                }
            },
            Value::String(text) if text.contains("$ref") || text.contains("$file") => {
                if let Ok(nested) = serde_json::from_str::<Value>(text) {
                    let filled = resolved
                        .as_str()
                        .and_then(|text| serde_json::from_str::<Value>(text).ok());
                    path.push("$json".into());
                    collect(
                        &nested,
                        filled.as_ref().unwrap_or(&Value::Null),
                        path,
                        values,
                    )?;
                    path.pop();
                }
            },
            _ => {},
        }
        Ok(())
    }
    let mut values = BTreeMap::new();
    collect(original, resolved, &mut Vec::new(), &mut values)?;
    if values.is_empty() {
        return Ok(None);
    }
    let mut canonical = serde_json::to_value(&values).map_err(|_| "reference_values_invalid")?;
    canonical.sort_all_objects();
    let bytes = serde_json::to_vec(&canonical).map_err(|_| "reference_values_invalid")?;
    crate::magician_v2::analytics::llm_trace_content::scoped_content_fingerprint(
        layout,
        &magicllm::LlmScope::new(principal, workspace),
        &bytes,
    )
    .map(Some)
}

impl OutwardReview {
    fn recipients_text(&self) -> String {
        match self.recipients.as_slice() {
            [] => "an unnamed recipient".to_string(),
            [one] => one.clone(),
            [first, rest @ ..] => format!("{first} and {} more", rest.len()),
        }
    }

    fn agent_name(&self) -> &str {
        if self.agent_id.is_empty() {
            "Your agent"
        } else {
            &self.agent_id
        }
    }

    fn what(&self) -> &'static str {
        match self.class.as_str() {
            "mail" => "an email",
            "message" => "a message",
            "calendar_invite" => "a calendar invitation",
            "form_submission" => "a form submission",
            _ => "an outward action",
        }
    }

    /// The confirmation reason the pause records.
    pub fn reason(&self) -> String {
        format!(
            "Outward send held for review: `{}` / `{}` to {} (this context captures sends; \
             review before sending is on)",
            self.capability,
            self.action,
            self.recipients_text()
        )
    }

    /// The card the owner sees.
    pub fn preview(&self) -> ApprovalPreview {
        let mut evidence = vec![format!("To: {}", self.recipients.join(", "))];
        if let Some(account) = &self.sending_account {
            evidence.push(format!("From account: {account}"));
        }
        if let Some(subject) = &self.subject {
            evidence.push(format!("Subject: {subject}"));
        }
        if let Some(excerpt) = &self.excerpt {
            evidence.push(excerpt.clone());
        }
        let mut notes = Vec::new();
        if self.content_by_reference {
            notes.push(
                "Part of the message refers to stored content that could not be shown here."
                    .to_string(),
            );
        }
        ApprovalPreview {
            title: format!(
                "{} wants to send {} to {}",
                self.agent_name(),
                self.what(),
                self.recipients_text()
            ),
            why_label: None,
            why: None,
            diff: None,
            evidence,
            warnings: Vec::new(),
            notes: {
                notes.push(format!(
                    "Via {} ({}). Nothing has been sent. Unanswered by the deadline, it is discarded.",
                    self.capability, self.action
                ));
                notes
            },
        }
    }

    /// The answers, in the order the cards show them. Always allow needs a
    /// named agent and recipient to bind the grant to.
    pub fn options_json(&self) -> Value {
        let mut options = vec![
            json!({ "id": OPTION_SEND, "label": "Send" }),
            json!({ "id": OPTION_DISCARD, "label": "Discard" }),
        ];
        if !self.agent_id.is_empty() && !self.recipients.is_empty() {
            options.push(json!({
                "id": OPTION_ALWAYS_ALLOW,
                "label": format!("Always allow {} to send to {}", self.agent_id, self.recipients_text()),
            }));
        }
        Value::Array(options)
    }

    /// The `input_schema` fields the review adds to its HITL request.
    pub fn hitl_fields(&self) -> Value {
        json!({
            "escalation_trigger": ESCALATION_TRIGGER,
            "options": self.options_json(),
            "outward_review": self,
        })
    }

    /// What the agent is told when the owner discards the send or the review
    /// expires. Final: the agent must not retry it.
    pub fn discard_note(&self, expired: bool) -> String {
        let why = if expired {
            "was not answered in time"
        } else {
            "was discarded by the owner"
        };
        format!(
            "NOT SENT — the owner's review of this {} (`{}` / `{}` to {}) {}. Nothing reached \
             any recipient. Do not retry this send; tell the owner it was not sent.",
            self.what(),
            self.capability,
            self.action,
            self.recipients_text(),
            why
        )
    }
}

/// The subject and an excerpt of the body, from the call's usual fields.
pub fn message_parts(params: &HashMap<String, Value>) -> (Option<String>, Option<String>) {
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| params.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let subject = text(&["subject", "title", "summary"]);
    let excerpt = text(&["body", "text", "message", "content", "description"]).map(|body| {
        if body.chars().count() <= EXCERPT_CHARS {
            body
        } else {
            let mut cut: String = body.chars().take(EXCERPT_CHARS - 1).collect();
            cut.push('…');
            cut
        }
    });
    (subject, excerpt)
}

// ---- one-time pass for an approved send -------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WaiverKey {
    principal: String,
    workspace: String,
    execution_id: String,
    action_json: String,
}

struct ApprovalPass {
    expires_at_ms: i64,
    reference_fingerprint: Option<String>,
}

static WAIVERS: Mutex<Option<HashMap<WaiverKey, ApprovalPass>>> = Mutex::new(None);

fn key(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
) -> Option<WaiverKey> {
    (!principal.is_empty()
        && !workspace.is_empty()
        && !execution_id.is_empty()
        && !action_json.is_empty())
    .then(|| WaiverKey {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        execution_id: execution_id.to_string(),
        action_json: action_json.to_string(),
    })
}

/// The owner approved this exact call: let its replay through capture once.
pub fn register_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    now_ms: i64,
) {
    register_bound_waiver(
        principal,
        workspace,
        execution_id,
        action_json,
        None,
        now_ms,
    );
}

pub fn register_bound_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    reference_fingerprint: Option<String>,
    now_ms: i64,
) {
    let Some(key) = key(principal, workspace, execution_id, action_json) else {
        return;
    };
    let mut guard = WAIVERS.lock().unwrap_or_else(|error| error.into_inner());
    let waivers = guard.get_or_insert_with(HashMap::new);
    waivers.retain(|_, pass| pass.expires_at_ms > now_ms);
    waivers.insert(
        key,
        ApprovalPass {
            expires_at_ms: now_ms + WAIVER_TTL_MS,
            reference_fingerprint,
        },
    );
}

/// Whether an approval's pass is waiting for this call (not consumed).
pub fn has_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    now_ms: i64,
) -> bool {
    let Some(key) = key(principal, workspace, execution_id, action_json) else {
        return false;
    };
    let guard = WAIVERS.lock().unwrap_or_else(|error| error.into_inner());
    guard
        .as_ref()
        .and_then(|waivers| waivers.get(&key))
        .is_some_and(|pass| pass.expires_at_ms > now_ms)
}

pub fn has_bound_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    reference_fingerprint: Option<&str>,
    now_ms: i64,
) -> bool {
    let Some(key) = key(principal, workspace, execution_id, action_json) else {
        return false;
    };
    let guard = WAIVERS.lock().unwrap_or_else(|error| error.into_inner());
    guard
        .as_ref()
        .and_then(|waivers| waivers.get(&key))
        .is_some_and(|pass| {
            pass.expires_at_ms > now_ms
                && pass.reference_fingerprint.as_deref() == reference_fingerprint
        })
}

/// Consume the pass for this call. `true` once per approval.
pub fn take_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    now_ms: i64,
) -> bool {
    take_bound_waiver(
        principal,
        workspace,
        execution_id,
        action_json,
        None,
        now_ms,
    )
}

/// A missing or changed reference binding invalidates the approval; a new
/// review must observe the new values before any send can use it.
pub fn take_bound_waiver(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    reference_fingerprint: Option<&str>,
    now_ms: i64,
) -> bool {
    let Some(key) = key(principal, workspace, execution_id, action_json) else {
        return false;
    };
    let mut guard = WAIVERS.lock().unwrap_or_else(|error| error.into_inner());
    guard
        .as_mut()
        .and_then(|waivers| waivers.remove(&key))
        .is_some_and(|pass| {
            pass.expires_at_ms > now_ms
                && pass.reference_fingerprint.as_deref() == reference_fingerprint
        })
}

/// The approved call passed its gate and is about to dispatch as attempt
/// `effect_id`: bind its pass to that attempt. Dispatch prepares the call
/// (secrets, credentials, one-time codes) before its capture check, so the
/// check cannot match the call's JSON; it takes the attempt's pass instead
/// ([`take_armed`]). Returns whether a pass was waiting.
pub fn arm_for_dispatch(
    principal: &str,
    workspace: &str,
    execution_id: &str,
    action_json: &str,
    effect_id: &str,
    now_ms: i64,
) -> bool {
    if effect_id.is_empty() || !has_waiver(principal, workspace, execution_id, action_json, now_ms)
    {
        return false;
    }
    let Some(key) = key(principal, workspace, execution_id, action_json) else {
        return false;
    };
    let mut guard = ARMED.lock().unwrap_or_else(|error| error.into_inner());
    let armed = guard.get_or_insert_with(HashMap::new);
    armed.retain(|_, (_, expires)| *expires > now_ms);
    armed.insert(effect_id.to_string(), (key, now_ms + ARMED_TTL_MS));
    true
}

/// Consume the pass armed for dispatch attempt `effect_id`. `true` once.
pub fn take_armed(
    effect_id: &str,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    reference_fingerprint: Option<&str>,
    now_ms: i64,
) -> bool {
    let entry = {
        let mut guard = ARMED.lock().unwrap_or_else(|error| error.into_inner());
        if guard
            .as_ref()
            .and_then(|armed| armed.get(effect_id))
            .is_some_and(|(key, _)| {
                key.principal != principal
                    || key.workspace != workspace
                    || key.execution_id != execution_id
            })
        {
            return false;
        }
        guard.as_mut().and_then(|armed| armed.remove(effect_id))
    };
    let Some((key, _)) = entry.filter(|(_, expires)| *expires > now_ms) else {
        return false;
    };
    take_bound_waiver(
        &key.principal,
        &key.workspace,
        &key.execution_id,
        &key.action_json,
        reference_fingerprint,
        now_ms,
    )
}

static ARMED: Mutex<Option<HashMap<String, (WaiverKey, i64)>>> = Mutex::new(None);

// ---- standing grants: agent → recipient -------------------------------

static FILE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientGrant {
    pub id: String,
    pub agent_id: String,
    pub capability: String,
    pub recipient: String,
    pub granted_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    grants: Vec<RecipientGrant>,
}

/// Where a scope keeps its standing send grants.
pub fn grants_path(workspace: &ArtifactV2Workspace, principal: &str, scope: &str) -> PathBuf {
    workspace
        .runtime_root(principal, scope)
        .join("outward")
        .join("recipient_grants.json")
}

/// An unreadable file reads as "no grants": a damaged store asks again
/// rather than letting a send through.
fn read(path: &Path) -> GrantFile {
    read_strict(path).unwrap_or_default()
}

/// For a write: a missing file is empty, a damaged one is an error, so a new
/// grant never overwrites the grants a damaged file still holds.
fn read_strict(path: &Path) -> std::io::Result<GrantFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(GrantFile::default()),
        Err(error) => Err(error),
    }
}

fn write(path: &Path, file: &GrantFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    magician_core::durable_io::write_bytes_durably_sync(path, &serde_json::to_vec_pretty(file)?)
}

pub fn list(path: &Path) -> Vec<RecipientGrant> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    read(path).grants
}

/// Whether `agent_id` may send through `capability` to every one of
/// `recipients`. An act naming no recipient is never covered.
pub fn all_granted(path: &Path, agent_id: &str, capability: &str, recipients: &[String]) -> bool {
    if recipients.is_empty() || agent_id.is_empty() {
        return false;
    }
    let grants = list(path);
    recipients.iter().all(|recipient| {
        grants.iter().any(|grant| {
            grant.agent_id == agent_id
                && grant.capability == capability
                && grant.recipient == *recipient
        })
    })
}

/// Record a grant per recipient; an existing identical grant is kept.
pub fn grant(
    path: &Path,
    agent_id: &str,
    capability: &str,
    recipients: &[String],
    now_ms: i64,
) -> std::io::Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut file = read_strict(path)?;
    for recipient in recipients {
        let exists = file.grants.iter().any(|grant| {
            grant.agent_id == agent_id
                && grant.capability == capability
                && grant.recipient == *recipient
        });
        if !exists {
            let id =
                blake3::hash(format!("{agent_id}\u{1f}{capability}\u{1f}{recipient}").as_bytes())
                    .to_hex()[..16]
                    .to_string();
            file.grants.push(RecipientGrant {
                id,
                agent_id: agent_id.to_string(),
                capability: capability.to_string(),
                recipient: recipient.clone(),
                granted_at_ms: now_ms,
            });
        }
    }
    write(path, &file)
}

/// Remove one grant by id. Returns whether it existed.
pub fn revoke(path: &Path, id: &str) -> std::io::Result<bool> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut file = read_strict(path)?;
    let before = file.grants.len();
    file.grants.retain(|grant| grant.id != id);
    let removed = file.grants.len() != before;
    if removed {
        write(path, &file)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> OutwardReview {
        OutwardReview {
            capability: "gmail".into(),
            action: "send".into(),
            class: "mail".into(),
            recipients: vec!["a@example.com".into()],
            agent_id: "ambassador".into(),
            deadline_at_ms: 10,
            subject: Some("Hello".into()),
            excerpt: Some("Body".into()),
            content_by_reference: false,
            reference_fingerprint: None,
            sending_account: None,
        }
    }

    #[test]
    fn the_card_names_the_sending_account_when_one_is_selected() {
        assert!(!review()
            .preview()
            .evidence
            .iter()
            .any(|line| line.starts_with("From account:")));
        let defaulted = OutwardReview {
            sending_account: Some("default (work)".into()),
            ..review()
        };
        assert!(defaulted
            .preview()
            .evidence
            .contains(&"From account: default (work)".to_string()));
    }

    #[test]
    fn the_card_names_agent_recipient_and_message() {
        let preview = review().preview();
        assert_eq!(
            preview.title,
            "ambassador wants to send an email to a@example.com"
        );
        assert!(preview.evidence.contains(&"To: a@example.com".to_string()));
        assert!(preview.evidence.contains(&"Subject: Hello".to_string()));
        let options = review().options_json();
        let ids: Vec<&str> = options
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![OPTION_SEND, OPTION_DISCARD, OPTION_ALWAYS_ALLOW]);
        assert_eq!(
            review().hitl_fields()["escalation_trigger"],
            ESCALATION_TRIGGER
        );
        let unnamed = OutwardReview {
            agent_id: String::new(),
            ..review()
        };
        assert_eq!(
            unnamed.options_json().as_array().unwrap().len(),
            2,
            "no grant without an agent"
        );
        assert!(unnamed.preview().title.starts_with("Your agent wants"));
    }

    #[test]
    fn a_pass_is_used_once_and_only_by_the_approved_call() {
        register_waiver("p", "w", "exec-1", "{call}", 1_000);
        assert!(has_waiver("p", "w", "exec-1", "{call}", 1_001));
        assert!(!take_waiver("p", "w", "exec-1", "{other}", 1_001));
        assert!(!take_waiver("p", "w", "exec-2", "{call}", 1_001));
        assert!(take_waiver("p", "w", "exec-1", "{call}", 1_002));
        assert!(!take_waiver("p", "w", "exec-1", "{call}", 1_003));
        register_waiver("p", "w", "exec-3", "{late}", 0);
        assert!(!take_waiver(
            "p",
            "w",
            "exec-3",
            "{late}",
            WAIVER_TTL_MS + 1
        ));
        // Bound to a dispatch attempt, the pass is taken by that attempt only.
        register_waiver("p", "w", "exec-5", "{armed}", 0);
        assert!(!arm_for_dispatch(
            "p", "w", "exec-5", "{other}", "effect-1", 1
        ));
        assert!(arm_for_dispatch(
            "p", "w", "exec-5", "{armed}", "effect-1", 1
        ));
        assert!(!take_armed("effect-2", "p", "w", "exec-5", None, 2));
        assert!(!take_armed("effect-1", "other", "w", "exec-5", None, 2));
        assert!(take_armed("effect-1", "p", "w", "exec-5", None, 2));
        assert!(!take_armed("effect-1", "p", "w", "exec-5", None, 3));
        assert!(
            !take_waiver("p", "w", "exec-5", "{armed}", 3),
            "the armed take consumed the pass"
        );
        register_waiver("", "w", "exec-4", "{call}", 0);
        assert!(!take_waiver("", "w", "exec-4", "{call}", 1));
    }

    #[test]
    fn a_reference_approval_rejects_changed_values_and_cannot_be_reused() {
        register_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values".into()),
            10,
        );
        assert!(has_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values"),
            11
        ));
        assert!(!has_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("changed-values"),
            11
        ));
        assert!(arm_for_dispatch(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            "ref-effect",
            11
        ));
        assert!(!take_armed(
            "ref-effect",
            "ref-owner",
            "other",
            "ref-run",
            Some("approved-values"),
            12
        ));
        assert!(!take_armed(
            "ref-effect",
            "ref-owner",
            "w",
            "ref-run",
            Some("changed-values"),
            12
        ));
        assert!(!take_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values"),
            13
        ));
        register_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values".into()),
            14,
        );
        assert!(take_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values"),
            15
        ));
        assert!(!take_bound_waiver(
            "ref-owner",
            "w",
            "ref-run",
            "file-call",
            Some("approved-values"),
            16
        ));
        register_bound_waiver(
            "ref-owner",
            "w",
            "missing-binding",
            "file-call",
            Some("unresolved".into()),
            20,
        );
        assert!(!take_bound_waiver(
            "ref-owner",
            "w",
            "missing-binding",
            "file-call",
            None,
            21,
        ));
        register_waiver("ref-owner", "w", "legacy-binding", "file-call", 20);
        assert!(!take_bound_waiver(
            "ref-owner",
            "w",
            "legacy-binding",
            "file-call",
            Some("resolved-values"),
            21,
        ));
    }

    #[test]
    fn reference_bindings_include_full_bodies_recipients_and_nested_json() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let original = json!({
            "to": {"$file": "recipient.json", "format": "json"},
            "body": {"$file": "message.txt"},
            "arguments_json": r#"{"data":{"$ref":"result-ref#/value"}}"#,
            "credential": "<secret>"
        });
        let resolved = json!({"to":"a@example.com", "body":"x".repeat(800),
            "arguments_json": r#"{"data":{"id":1,"text":"reference"}}"#, "credential":"<secret>"});
        let digest = |resolved: &Value| {
            reference_fingerprint(&layout, "p", "w", &original, resolved).unwrap()
        };
        let expected = digest(&resolved).unwrap();
        let mut changed = resolved.clone();
        changed["credential"] = json!("injected-after-approval");
        assert_eq!(
            digest(&changed).as_deref(),
            Some(expected.as_str()),
            "late credential preparation is independent"
        );
        changed["body"] = json!(format!("{}new-tail", "x".repeat(800)));
        assert_ne!(
            digest(&changed).as_deref(),
            Some(expected.as_str()),
            "bind beyond the displayed excerpt"
        );
        changed = resolved.clone();
        changed["to"] = json!("b@example.com");
        assert_ne!(digest(&changed).as_deref(), Some(expected.as_str()));
        changed = resolved.clone();
        changed["arguments_json"] = json!(r#"{"data":{"text":"reference","id":1}}"#);
        assert_eq!(
            digest(&changed).as_deref(),
            Some(expected.as_str()),
            "JSON key order is immaterial"
        );
        changed["arguments_json"] = json!(r#"{"data":{"id":2,"text":"reference"}}"#);
        assert_ne!(digest(&changed).as_deref(), Some(expected.as_str()));
        assert_eq!(
            reference_fingerprint(
                &layout,
                "p",
                "w",
                &json!({"body":"literal"}),
                &json!({"body":"literal"})
            )
            .unwrap(),
            None
        );
        assert_ne!(
            reference_fingerprint(&layout, "other", "w", &original, &resolved).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn grants_cover_every_named_recipient_and_can_be_revoked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grants.json");
        let recipients = vec!["a@example.com".to_string(), "b@example.com".to_string()];
        assert!(!all_granted(&path, "ambassador", "gmail", &recipients));
        grant(&path, "ambassador", "gmail", &recipients[..1], 5).unwrap();
        assert!(
            !all_granted(&path, "ambassador", "gmail", &recipients),
            "one recipient is not granted"
        );
        grant(&path, "ambassador", "gmail", &recipients, 6).unwrap();
        assert!(all_granted(&path, "ambassador", "gmail", &recipients));
        assert!(!all_granted(&path, "other-agent", "gmail", &recipients));
        assert!(!all_granted(&path, "ambassador", "whatsapp", &recipients));
        assert!(!all_granted(&path, "ambassador", "gmail", &[]));
        assert_eq!(
            list(&path).len(),
            2,
            "re-granting keeps one grant per recipient"
        );
        let id = list(&path)[0].id.clone();
        assert!(revoke(&path, &id).unwrap());
        assert!(!revoke(&path, &id).unwrap());
        assert!(!all_granted(&path, "ambassador", "gmail", &recipients));
        std::fs::write(&path, b"{damaged").unwrap();
        assert!(
            grant(&path, "ambassador", "gmail", &recipients, 7).is_err(),
            "never overwrite a damaged store"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{damaged");
    }

    #[test]
    fn message_parts_take_subject_and_a_bounded_excerpt() {
        let mut params = HashMap::new();
        params.insert("subject".to_string(), json!("Quarterly update"));
        params.insert("body".to_string(), json!("x".repeat(700)));
        let (subject, excerpt) = message_parts(&params);
        assert_eq!(subject.as_deref(), Some("Quarterly update"));
        assert_eq!(excerpt.unwrap().chars().count(), EXCERPT_CHARS);
    }
}
