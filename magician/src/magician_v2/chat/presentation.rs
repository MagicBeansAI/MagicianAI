//! Structured presentation sidecar for chat messages.
//!
//! This module defines a versioned `StructuredResponseV1` contract plus a
//! bounded validation path. The sidecar is additive and optional; `content`
//! remains the canonical answer representation.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::magician_v2::chat::models::{ChatMessageContent, ContentBlockRecord};

pub const STRUCTURED_RESPONSE_SCHEMA: &str = "magician.structured_response";

const MAX_PRESENTATION_BYTES: usize = 64 * 1024;
const MAX_BLOCKS: usize = 32;
const MAX_ACTIONS: usize = 8;
const MAX_TABLE_COLUMNS: usize = 12;
const MAX_TABLE_ROWS: usize = 100;
const MAX_LIST_LIKE_ITEMS: usize = 100;
const MAX_AGGREGATE_TEXT_BYTES: usize = 32 * 1024;
const MAX_URL_BYTES: usize = 2 * 1024;
const MAX_TITLE_BYTES: usize = 160;
const MAX_LABEL_BYTES: usize = 160;
const MAX_VALUE_BYTES: usize = 2048;
const STRUCTURED_ARTIFACT_SESSION_PREFIX: &str = "magician-artifact:session:";
const STRUCTURED_ARTIFACT_TASK_PREFIX: &str = "magician-artifact:task:";
/// Marker appended to display text cut to its field budget.
const TRUNCATION_MARKER: &str = "…";
/// Hex digits of the whole-id digest that replace an over-long id's tail.
const IDENTIFIER_DIGEST_HEX_CHARS: usize = 16;
/// Message ids whose invalid projection was already reported at warn level.
const MAX_REPORTED_INVALID_PROJECTIONS: usize = 1024;
/// Total `relative_path` bytes of artifact locators one presentation may carry.
const MAX_LOCATOR_PATH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredResponseV1 {
    pub schema: String,
    pub version: u8,
    pub plain_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<StructuredResponseTone>,
    pub blocks: Vec<StructuredResponseBlockV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<StructuredResponseActionV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context: Option<StructuredModelContextV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<StructuredResponseMetaV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuredResponseBlockV1 {
    Markdown {
        text: String,
    },
    Text {
        text: String,
    },
    Callout {
        tone: StructuredResponseTone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        text: String,
    },
    KeyValues {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        items: Vec<StructuredKeyValueV1>,
    },
    Table {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        columns: Vec<StructuredTableColumnV1>,
        rows: Vec<HashMap<String, String>>,
    },
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default)]
        style: Option<StructuredListStyleV1>,
        items: Vec<StructuredListItemV1>,
    },
    Artifacts {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        items: Vec<StructuredArtifactRefV1>,
    },
    Sources {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        items: Vec<StructuredSourceRefV1>,
    },
    Metrics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        items: Vec<StructuredMetricV1>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredResponseTone {
    Neutral,
    Success,
    Warning,
    Danger,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredKeyValueV1 {
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredTableColumnV1 {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<StructuredTableAlignmentV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredTableAlignmentV1 {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredListItemV1 {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredListStyleV1 {
    Bullets,
    Steps,
    Checks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredArtifactRefV1 {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredSourceRefV1 {
    pub label: String,
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredMetricV1 {
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trend: Option<StructuredMetricTrendV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredMetricTrendV1 {
    Up,
    Down,
    Flat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuredResponseActionV1 {
    CopyText { label: String, text: String },
    OpenUrl { label: String, url: String },
    OpenTask { label: String, task_id: String },
    OpenArtifact { label: String, artifact_id: String },
    SendFollowUp { label: String, prompt: String },
    InvokeServerAction { label: String, action_ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredResponseMetaV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<StructuredProvenanceRefV1>,
    /// Open targets for artifacts whose `artifact_id` was shortened. Kept in
    /// `meta` because older clients ignore unknown meta keys, while some reject
    /// unknown keys on artifact items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_locators: Vec<StructuredArtifactLocatorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<StructuredResponseCostV1>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredProvenanceRefV1 {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
}

/// Resolvable target for an artifact whose `artifact_id` no longer embeds its
/// full path. Clients open `task_id` + `relative_path` (a task output) or
/// `relative_path` alone (a session output) instead of parsing the id.
/// `relative_path` is output-relative, never a filesystem path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredArtifactLocatorV1 {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredResponseCostV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredModelContextV1 {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_item: Option<String>,
    pub privacy: StructuredModelContextPrivacyV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredModelContextPrivacyV1 {
    ModelVisible,
    LocalOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredResponseDropReason {
    UnsupportedSchema,
    UnsupportedVersion,
    MissingSchema,
    MissingBlocks,
    TooManyBlocks,
    TooManyActions,
    TooManyTableColumns,
    TooManyTableRows,
    TooManyListLikeItems,
    TextTooLarge,
    PresentationTooLarge,
    UnsupportedUrl,
    InvalidText,
    InvalidNumericValue,
    InvalidConfidence,
    MissingPlainText,
    PlainTextMismatch,
    TableShapeMismatch,
    DuplicateTableColumnKey,
    InvalidFieldLength,
    UnsafeControlCharacter,
    ServerActionsUnsupported,
}

impl fmt::Display for StructuredResponseDropReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema => f.write_str("unsupported schema"),
            Self::UnsupportedVersion => f.write_str("unsupported version"),
            Self::MissingSchema => f.write_str("missing schema"),
            Self::MissingBlocks => f.write_str("at least one response block is required"),
            Self::TooManyBlocks => f.write_str("too many blocks"),
            Self::TooManyActions => f.write_str("too many actions"),
            Self::TooManyTableColumns => f.write_str("too many table columns"),
            Self::TooManyTableRows => f.write_str("too many table rows"),
            Self::TooManyListLikeItems => f.write_str("too many list-like items"),
            Self::TextTooLarge => f.write_str("text exceeds maximum budget"),
            Self::PresentationTooLarge => f.write_str("serialized presentation exceeds budget"),
            Self::UnsupportedUrl => f.write_str("unsupported or unsafe URL"),
            Self::InvalidText => f.write_str("invalid text payload"),
            Self::InvalidNumericValue => f.write_str("invalid numeric value"),
            Self::InvalidConfidence => f.write_str("invalid confidence value"),
            Self::MissingPlainText => f.write_str("plain_text is required"),
            Self::PlainTextMismatch => f.write_str("canonical content and presentation differ"),
            Self::TableShapeMismatch => f.write_str("table row shape mismatch"),
            Self::DuplicateTableColumnKey => f.write_str("duplicate table column key"),
            Self::InvalidFieldLength => f.write_str("text field length exceeds limit"),
            Self::UnsafeControlCharacter => f.write_str("control characters are not allowed"),
            Self::ServerActionsUnsupported => {
                f.write_str("server-side structured actions are not supported")
            },
        }
    }
}

/// A validation failure plus the field path that caused it, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredResponseInvalid {
    pub reason: StructuredResponseDropReason,
    pub field: &'static str,
}

impl From<StructuredResponseInvalid> for StructuredResponseDropReason {
    fn from(error: StructuredResponseInvalid) -> Self {
        error.reason
    }
}

fn invalid(reason: StructuredResponseDropReason, field: &'static str) -> StructuredResponseInvalid {
    StructuredResponseInvalid { reason, field }
}

impl StructuredResponseV1 {
    /// Validates and normalizes presentation for a specific canonical `content`.
    ///
    /// The canonical message text is authoritative: the block tree is validated,
    /// `plain_text` is derived from blocks, and the canonical plain text is
    /// used when persistence is accepted.
    pub fn attach_to_content(
        canonical_content: &ChatMessageContent,
        presentation: Self,
    ) -> Result<Self, StructuredResponseDropReason> {
        validate_structured_response(&presentation)?;

        // A presentation is an envelope derived by this server, never an
        // alternative source of message meaning. Matching only plain_text
        // would still permit a producer to add arbitrary visible blocks or
        // actions. Rebuild the deterministic projection and require an exact
        // match before persisting or transporting a supplied sidecar.
        let expected = Self::from_content(canonical_content)
            .ok_or(StructuredResponseDropReason::PlainTextMismatch)?;
        if presentation != expected {
            return Err(StructuredResponseDropReason::PlainTextMismatch);
        }
        Ok(expected)
    }

    /// Build the only presentation representation emitted for a canonical
    /// display message. `plain_text` remains the compatibility projection;
    /// rich blocks add rendering semantics and must never redefine it.
    ///
    /// A projection that still fails validation is dropped and reported at
    /// debug level; message-aware callers use [`Self::from_content_for_message`]
    /// so the drop is reported once per message at warn level.
    pub fn from_content(content: &ChatMessageContent) -> Option<Self> {
        match Self::project(content) {
            Ok(presentation) => presentation,
            Err(error) => {
                tracing::debug!(
                    field = error.field,
                    reason = ?error.reason,
                    "dropping invalid structured chat presentation"
                );
                None
            },
        }
    }

    /// [`Self::from_content`] for a known message. Messages are re-projected
    /// on every segment read, so a projection that fails validation is
    /// reported at warn level once per message id per process, then at debug.
    pub fn from_content_for_message(
        message_id: &str,
        content: &ChatMessageContent,
    ) -> Option<Self> {
        match Self::project(content) {
            Ok(presentation) => presentation,
            Err(error) => {
                if first_invalid_projection_report(message_id) {
                    tracing::warn!(
                        message_id,
                        field = error.field,
                        reason = ?error.reason,
                        "dropping invalid structured chat presentation"
                    );
                } else {
                    tracing::debug!(
                        message_id,
                        field = error.field,
                        reason = ?error.reason,
                        "dropping invalid structured chat presentation"
                    );
                }
                None
            },
        }
    }

    /// Deterministic projection. Every generated display field is bounded to
    /// its validator budget here (text is cut on a char boundary, identifiers
    /// keep a digest of their full value), so validation stays the final
    /// guard rather than the normal path for long labels, paths or URLs.
    /// Message bodies (`plain_text`, markdown and callout text) are never cut:
    /// an over-budget body drops the sidecar and clients render `content`.
    fn project(content: &ChatMessageContent) -> Result<Option<Self>, StructuredResponseInvalid> {
        let plain_text = normalize_for_compare(plain_text_from_content(content));
        if plain_text.trim().is_empty() {
            return Ok(None);
        }

        let mut locators = Vec::new();
        let (title, summary, tone, blocks, actions, meta) = match content {
            ChatMessageContent::Text { text, .. } => (
                None,
                None,
                Some(StructuredResponseTone::Neutral),
                vec![StructuredResponseBlockV1::Markdown { text: text.clone() }],
                None,
                None,
            ),
            ChatMessageContent::ToolCallExecuted {
                tool_name, summary, ..
            } => {
                let title = bound_text(&format!("Action completed: {tool_name}"), MAX_TITLE_BYTES);
                (
                    Some(title.clone()),
                    Some(bound_text(summary, MAX_VALUE_BYTES)),
                    Some(StructuredResponseTone::Info),
                    vec![StructuredResponseBlockV1::Callout {
                        tone: StructuredResponseTone::Info,
                        title: Some(title),
                        text: summary.clone(),
                    }],
                    None,
                    None,
                )
            },
            ChatMessageContent::RichToolResult {
                summary,
                content_blocks,
                ..
            } => {
                let summary = non_empty(Some(summary.as_str())).map(str::to_string);
                let mut blocks = Vec::new();
                if let Some(summary) = summary.as_ref() {
                    blocks.push(StructuredResponseBlockV1::Markdown {
                        text: summary.clone(),
                    });
                }
                append_content_blocks(&mut blocks, content_blocks, None, &mut locators);
                let provenance = provenance_refs_from_content_blocks(content_blocks);
                (
                    Some("Action result".to_string()),
                    summary.map(|summary| bound_text(&summary, MAX_VALUE_BYTES)),
                    Some(StructuredResponseTone::Info),
                    blocks,
                    None,
                    (!provenance.is_empty()).then_some(StructuredResponseMetaV1 {
                        response_id: None,
                        source_surface: None,
                        task_id: None,
                        execution_id: None,
                        chat_turn_id: None,
                        provenance,
                        artifact_locators: Vec::new(),
                        cost: None,
                        confidence: None,
                        created_at: None,
                    }),
                )
            },
            ChatMessageContent::Attachment {
                filename,
                mime_type,
                size,
                absolute_path: _,
                label,
            } => {
                let label = bound_text(
                    non_empty(label.as_deref()).unwrap_or(filename),
                    MAX_LABEL_BYTES,
                );
                (
                    Some("Attachment".to_string()),
                    None,
                    Some(StructuredResponseTone::Neutral),
                    vec![StructuredResponseBlockV1::Artifacts {
                        title: Some("Attachment".to_string()),
                        items: vec![StructuredArtifactRefV1 {
                            label,
                            href: None,
                            artifact_id: Some(artifact_target(None, filename, &mut locators)),
                            mime_type: bounded_mime_type(mime_type),
                            size: bounded_artifact_size(*size),
                        }],
                    }],
                    None,
                    None,
                )
            },
            ChatMessageContent::TaskStatusUpdate {
                task_id,
                status,
                display_label,
                summary,
                execution_id,
                output_files,
                ..
            } => {
                let body = summary.clone().unwrap_or_else(|| status.clone());
                let title = bound_text(
                    &non_empty(display_label.as_deref())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("Task {task_id}: {status}")),
                    MAX_TITLE_BYTES,
                );
                let tone = task_status_tone(status);
                let mut blocks = vec![StructuredResponseBlockV1::Callout {
                    tone: tone.clone(),
                    title: Some(title.clone()),
                    text: body.clone(),
                }];
                append_content_blocks(&mut blocks, output_files, Some(task_id), &mut locators);
                (
                    Some(title),
                    Some(bound_text(&body, MAX_VALUE_BYTES)),
                    Some(tone),
                    blocks,
                    Some(vec![StructuredResponseActionV1::OpenTask {
                        label: "Open task".to_string(),
                        task_id: task_id.clone(),
                    }]),
                    Some(StructuredResponseMetaV1 {
                        response_id: None,
                        source_surface: None,
                        task_id: Some(task_id.clone()),
                        execution_id: execution_id.clone(),
                        chat_turn_id: None,
                        provenance: provenance_refs_from_content_blocks(output_files),
                        artifact_locators: Vec::new(),
                        cost: None,
                        confidence: None,
                        created_at: None,
                    }),
                )
            },
            // Escalations carry live pause authority and remain owned by the
            // HITL components rather than a display-sidecar action contract.
            ChatMessageContent::Escalation { .. } => return Ok(None),
            ChatMessageContent::EscalationResolved {
                summary,
                task_id,
                execution_id,
                output_files,
                ..
            } => {
                let mut blocks = vec![StructuredResponseBlockV1::Callout {
                    tone: StructuredResponseTone::Success,
                    title: Some("Request resolved".to_string()),
                    text: summary.clone(),
                }];
                append_content_blocks(&mut blocks, output_files, task_id.as_deref(), &mut locators);
                let actions = task_id.as_ref().map(|task_id| {
                    vec![StructuredResponseActionV1::OpenTask {
                        label: "Open task".to_string(),
                        task_id: task_id.clone(),
                    }]
                });
                (
                    Some("Request resolved".to_string()),
                    Some(bound_text(summary, MAX_VALUE_BYTES)),
                    Some(StructuredResponseTone::Success),
                    blocks,
                    actions,
                    Some(StructuredResponseMetaV1 {
                        response_id: None,
                        source_surface: None,
                        task_id: task_id.clone(),
                        execution_id: Some(execution_id.clone()),
                        chat_turn_id: None,
                        provenance: provenance_refs_from_content_blocks(output_files),
                        artifact_locators: Vec::new(),
                        cost: None,
                        confidence: None,
                        created_at: None,
                    }),
                )
            },
        };

        let response = Self {
            schema: STRUCTURED_RESPONSE_SCHEMA.to_string(),
            version: 1,
            plain_text,
            title,
            summary,
            tone,
            blocks,
            actions,
            // Explicit follow-up context is not part of V1: it has no
            // user-visible, consented submission path.
            model_context: None,
            meta: with_artifact_locators(meta, locators),
        };
        validate_structured_response_fields(&response)?;
        Ok(Some(response))
    }
}

/// True the first time `message_id` reports an invalid projection in this
/// process. The set is bounded: once full it is cleared, so a long-lived
/// process may re-report a message after many other distinct failures.
fn first_invalid_projection_report(message_id: &str) -> bool {
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut reported = REPORTED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if reported.contains(message_id) {
        return false;
    }
    if reported.len() >= MAX_REPORTED_INVALID_PROJECTIONS {
        reported.clear();
    }
    reported.insert(message_id.to_string());
    true
}

/// Largest char boundary in `text` at or below `max_bytes`.
fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    if max_bytes >= text.len() {
        return text.len();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Bound display text to `max_bytes` UTF-8 bytes. Text that fits is returned
/// unchanged; longer text is cut on a char boundary and ends with `…`.
fn bound_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let end = floor_char_boundary(text, max_bytes.saturating_sub(TRUNCATION_MARKER.len()));
    let mut bounded = text[..end].trim_end().to_string();
    bounded.push_str(TRUNCATION_MARKER);
    bounded
}

/// Bound an identifier to `max_bytes` without losing uniqueness. Ids that fit
/// are returned unchanged (existing ids stay stable); a longer id keeps a
/// char-boundary prefix and replaces its tail with `~` plus a blake3 digest of
/// the whole id, so distinct long ids never share a bounded form.
fn bound_identifier(id: &str, max_bytes: usize) -> String {
    if id.len() <= max_bytes {
        return id.to_string();
    }
    let digest = blake3::hash(id.as_bytes()).to_hex();
    let suffix = &digest.as_str()[..IDENTIFIER_DIGEST_HEX_CHARS];
    let end = floor_char_boundary(
        id,
        max_bytes.saturating_sub(IDENTIFIER_DIGEST_HEX_CHARS + 1),
    );
    format!("{}~{suffix}", &id[..end])
}

/// A MIME type is a token, not prose: an over-long one is omitted, not cut.
fn bounded_mime_type(mime_type: &str) -> Option<String> {
    non_empty(Some(mime_type))
        .filter(|mime_type| mime_type.len() <= MAX_LABEL_BYTES)
        .map(str::to_string)
}

/// Sizes beyond the validated range are omitted rather than failing the card.
fn bounded_artifact_size(size: u64) -> Option<u64> {
    (size <= i32::MAX as u64).then_some(size)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn task_status_tone(status: &str) -> StructuredResponseTone {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "succeeded" | "success" => StructuredResponseTone::Success,
        "failed" | "error" => StructuredResponseTone::Danger,
        "cancelled" | "canceled" | "blocked" => StructuredResponseTone::Warning,
        _ => StructuredResponseTone::Info,
    }
}

fn append_content_blocks(
    blocks: &mut Vec<StructuredResponseBlockV1>,
    content: &[ContentBlockRecord],
    task_id: Option<&str>,
    locators: &mut Vec<StructuredArtifactLocatorV1>,
) {
    for block in content {
        if let ContentBlockRecord::Text { text } = block {
            if !text.trim().is_empty() {
                blocks.push(StructuredResponseBlockV1::Markdown { text: text.clone() });
            }
        }
    }
    let artifacts = artifact_refs_from_content_blocks(content, task_id, locators);
    if !artifacts.is_empty() {
        blocks.push(StructuredResponseBlockV1::Artifacts {
            title: Some("Outputs".to_string()),
            items: artifacts,
        });
    }
    let sources = source_refs_from_content_blocks(content);
    if !sources.is_empty() {
        blocks.push(StructuredResponseBlockV1::Sources {
            title: Some("Sources".to_string()),
            items: sources,
        });
    }
}

fn artifact_refs_from_content_blocks(
    content: &[ContentBlockRecord],
    task_id: Option<&str>,
    locators: &mut Vec<StructuredArtifactLocatorV1>,
) -> Vec<StructuredArtifactRefV1> {
    let mut artifacts = Vec::new();
    for block in content {
        if artifacts.len() >= MAX_LIST_LIKE_ITEMS {
            break;
        }
        if let ContentBlockRecord::File {
            source: _,
            relative_path,
            display_name,
            mime_type,
            absolute_path: _,
            label,
            size,
        } = block
        {
            artifacts.push(StructuredArtifactRefV1 {
                label: bound_text(
                    non_empty(label.as_deref()).unwrap_or(display_name),
                    MAX_LABEL_BYTES,
                ),
                href: None,
                artifact_id: Some(artifact_target(task_id, relative_path, locators)),
                mime_type: bounded_mime_type(mime_type),
                size: bounded_artifact_size(*size),
            });
        }
    }
    artifacts
}

/// The `artifact_id` for a task output (`task_id` set) or session output.
/// An id over its 160-byte budget is shortened by [`bound_identifier`], which
/// clients can no longer parse into a path, so a locator carrying the full
/// target is recorded for it (omitted when the path itself exceeds the value
/// budget or the message's locator budget).
fn artifact_target(
    task_id: Option<&str>,
    relative_path: &str,
    locators: &mut Vec<StructuredArtifactLocatorV1>,
) -> String {
    let full_id = match task_id {
        Some(task_id) => format!("{STRUCTURED_ARTIFACT_TASK_PREFIX}{task_id}:{relative_path}"),
        None => format!("{STRUCTURED_ARTIFACT_SESSION_PREFIX}{relative_path}"),
    };
    let artifact_id = bound_identifier(&full_id, MAX_LABEL_BYTES);
    let used_path_bytes: usize = locators
        .iter()
        .map(|locator| locator.relative_path.len())
        .sum();
    let locatable = artifact_id != full_id
        && !relative_path.trim().is_empty()
        && relative_path.len() <= MAX_VALUE_BYTES
        && is_safe_text(relative_path)
        && task_id.map_or(true, |task_id| {
            !task_id.trim().is_empty() && task_id.len() <= MAX_LABEL_BYTES && is_safe_text(task_id)
        })
        && locators.len() < MAX_LIST_LIKE_ITEMS
        && used_path_bytes + relative_path.len() <= MAX_LOCATOR_PATH_BYTES;
    if locatable {
        locators.push(StructuredArtifactLocatorV1 {
            artifact_id: artifact_id.clone(),
            task_id: task_id.map(str::to_string),
            relative_path: relative_path.to_string(),
        });
    }
    artifact_id
}

/// Attach artifact locators to `meta`, creating an otherwise empty meta when
/// the projection had none.
fn with_artifact_locators(
    meta: Option<StructuredResponseMetaV1>,
    locators: Vec<StructuredArtifactLocatorV1>,
) -> Option<StructuredResponseMetaV1> {
    if locators.is_empty() {
        return meta;
    }
    let mut meta = meta.unwrap_or(StructuredResponseMetaV1 {
        response_id: None,
        source_surface: None,
        task_id: None,
        execution_id: None,
        chat_turn_id: None,
        provenance: Vec::new(),
        artifact_locators: Vec::new(),
        cost: None,
        confidence: None,
        created_at: None,
    });
    meta.artifact_locators = locators;
    Some(meta)
}

fn source_refs_from_content_blocks(content: &[ContentBlockRecord]) -> Vec<StructuredSourceRefV1> {
    content
        .iter()
        .filter_map(|block| match block {
            // A link a client cannot safely open is left out of the card
            // instead of invalidating the whole presentation.
            ContentBlockRecord::Url {
                url,
                display_name,
                label,
                ..
            } if !url.trim().is_empty() && validate_url(url, "sources.href").is_ok() => {
                Some(StructuredSourceRefV1 {
                    label: bound_text(
                        non_empty(label.as_deref()).unwrap_or(display_name),
                        MAX_LABEL_BYTES,
                    ),
                    href: url.clone(),
                })
            },
            _ => None,
        })
        .take(MAX_LIST_LIKE_ITEMS)
        .collect()
}

fn provenance_refs_from_content_blocks(
    content: &[ContentBlockRecord],
) -> Vec<StructuredProvenanceRefV1> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlockRecord::Url {
                url,
                display_name,
                label,
                ..
            } if !url.trim().is_empty() && url.len() <= MAX_VALUE_BYTES => {
                Some(StructuredProvenanceRefV1 {
                    id: bound_identifier(url, MAX_LABEL_BYTES),
                    label: Some(bound_text(
                        non_empty(label.as_deref()).unwrap_or(display_name),
                        MAX_LABEL_BYTES,
                    )),
                    ref_: Some(url.clone()),
                })
            },
            _ => None,
        })
        .collect()
}

pub fn validate_structured_response(
    response: &StructuredResponseV1,
) -> Result<(), StructuredResponseDropReason> {
    validate_structured_response_fields(response).map_err(|error| error.reason)
}

/// [`validate_structured_response`] that also names the failing field.
pub fn validate_structured_response_fields(
    response: &StructuredResponseV1,
) -> Result<(), StructuredResponseInvalid> {
    use StructuredResponseDropReason as Reason;

    if response.schema.is_empty() {
        return Err(invalid(Reason::MissingSchema, "schema"));
    }
    if response.schema != STRUCTURED_RESPONSE_SCHEMA {
        return Err(invalid(Reason::UnsupportedSchema, "schema"));
    }
    if response.version != 1 {
        return Err(invalid(Reason::UnsupportedVersion, "version"));
    }

    if response.blocks.is_empty() {
        return Err(invalid(Reason::MissingBlocks, "blocks"));
    }
    if response.blocks.len() > MAX_BLOCKS {
        return Err(invalid(Reason::TooManyBlocks, "blocks"));
    }

    if let Some(actions) = response.actions.as_ref() {
        if actions.len() > MAX_ACTIONS {
            return Err(invalid(Reason::TooManyActions, "actions"));
        }
        for action in actions {
            validate_action(action)?;
        }
    }

    if response.plain_text.is_empty() {
        return Err(invalid(Reason::MissingPlainText, "plain_text"));
    }
    if response.plain_text.as_bytes().len() > MAX_AGGREGATE_TEXT_BYTES {
        return Err(invalid(Reason::TextTooLarge, "plain_text"));
    }
    if !is_safe_text(&response.plain_text) {
        return Err(invalid(Reason::UnsafeControlCharacter, "plain_text"));
    }

    validate_optional_text(response.title.as_deref(), MAX_TITLE_BYTES, "title")?;
    validate_optional_text(response.summary.as_deref(), MAX_VALUE_BYTES, "summary")?;

    for block in &response.blocks {
        validate_block(block)?;
    }

    if let Some(model_context) = response.model_context.as_ref() {
        if model_context.summary.is_empty() || model_context.summary.len() > MAX_VALUE_BYTES {
            return Err(invalid(Reason::InvalidText, "model_context.summary"));
        }
        if !is_safe_text(&model_context.summary) {
            return Err(invalid(
                Reason::UnsafeControlCharacter,
                "model_context.summary",
            ));
        }
        for fact in &model_context.visible_facts {
            validate_string(fact, MAX_VALUE_BYTES, "model_context.visible_facts")?;
        }
        if let Some(item) = model_context.selected_item.as_ref() {
            validate_string(item, MAX_LABEL_BYTES, "model_context.selected_item")?;
        }
    }

    if let Some(meta) = response.meta.as_ref() {
        validate_optional_text(
            meta.response_id.as_deref(),
            MAX_LABEL_BYTES,
            "meta.response_id",
        )?;
        validate_optional_text(
            meta.source_surface.as_deref(),
            MAX_LABEL_BYTES,
            "meta.source_surface",
        )?;
        validate_optional_text(meta.task_id.as_deref(), MAX_LABEL_BYTES, "meta.task_id")?;
        validate_optional_text(
            meta.execution_id.as_deref(),
            MAX_LABEL_BYTES,
            "meta.execution_id",
        )?;
        validate_optional_text(
            meta.chat_turn_id.as_deref(),
            MAX_LABEL_BYTES,
            "meta.chat_turn_id",
        )?;
        for item in &meta.provenance {
            validate_string(&item.id, MAX_LABEL_BYTES, "meta.provenance.id")?;
            if let Some(label) = item.label.as_deref() {
                validate_string(label, MAX_LABEL_BYTES, "meta.provenance.label")?;
            }
            if let Some(reference) = item.ref_.as_deref() {
                validate_string(reference, MAX_VALUE_BYTES, "meta.provenance.ref")?;
            }
        }
        if meta.artifact_locators.len() > MAX_LIST_LIKE_ITEMS {
            return Err(invalid(
                Reason::TooManyListLikeItems,
                "meta.artifact_locators",
            ));
        }
        for locator in &meta.artifact_locators {
            validate_string(
                &locator.artifact_id,
                MAX_LABEL_BYTES,
                "meta.artifact_locators.artifact_id",
            )?;
            validate_optional_text(
                locator.task_id.as_deref(),
                MAX_LABEL_BYTES,
                "meta.artifact_locators.task_id",
            )?;
            validate_string(
                &locator.relative_path,
                MAX_VALUE_BYTES,
                "meta.artifact_locators.relative_path",
            )?;
        }
        if let Some(confidence) = meta.confidence {
            if !(0.0..=1.0).contains(&confidence) {
                return Err(invalid(Reason::InvalidConfidence, "meta.confidence"));
            }
        }
        if let Some(cost) = meta.cost.as_ref() {
            if let Some(value) = cost.cost_usd {
                if !value.is_finite() {
                    return Err(invalid(Reason::InvalidNumericValue, "meta.cost.cost_usd"));
                }
            }
            if let Some(model) = cost.model.as_deref() {
                validate_string(model, MAX_LABEL_BYTES, "meta.cost.model")?;
            }
        }
    }

    let bytes =
        serde_json::to_vec(response).map_err(|_| invalid(Reason::InvalidText, "presentation"))?;
    if bytes.len() > MAX_PRESENTATION_BYTES {
        return Err(invalid(Reason::PresentationTooLarge, "presentation"));
    }

    Ok(())
}

fn validate_block(block: &StructuredResponseBlockV1) -> Result<(), StructuredResponseInvalid> {
    use StructuredResponseDropReason as Reason;

    match block {
        StructuredResponseBlockV1::Markdown { text } => {
            validate_string(text, MAX_AGGREGATE_TEXT_BYTES, "blocks.markdown.text")
        },
        StructuredResponseBlockV1::Text { text } => {
            validate_string(text, MAX_VALUE_BYTES, "blocks.text.text")
        },
        StructuredResponseBlockV1::Callout { title, text, .. } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.callout.title")?;
            validate_string(text, MAX_AGGREGATE_TEXT_BYTES, "blocks.callout.text")
        },
        StructuredResponseBlockV1::KeyValues { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.key_values.title")?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(invalid(
                    Reason::TooManyListLikeItems,
                    "blocks.key_values.items",
                ));
            }
            for item in items {
                validate_string(
                    &item.label,
                    MAX_LABEL_BYTES,
                    "blocks.key_values.items.label",
                )?;
                validate_string(
                    &item.value,
                    MAX_VALUE_BYTES,
                    "blocks.key_values.items.value",
                )?;
                if let Some(hint) = item.hint.as_deref() {
                    validate_string(hint, MAX_VALUE_BYTES, "blocks.key_values.items.hint")?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Table {
            title,
            columns,
            rows,
        } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.table.title")?;
            if columns.is_empty() || columns.len() > MAX_TABLE_COLUMNS {
                return Err(invalid(Reason::TooManyTableColumns, "blocks.table.columns"));
            }
            if rows.len() > MAX_TABLE_ROWS {
                return Err(invalid(Reason::TooManyTableRows, "blocks.table.rows"));
            }

            let mut column_keys = HashSet::with_capacity(columns.len());
            let mut keys = Vec::with_capacity(columns.len());
            for column in columns {
                validate_string(&column.key, MAX_LABEL_BYTES, "blocks.table.columns.key")?;
                validate_string(&column.label, MAX_LABEL_BYTES, "blocks.table.columns.label")?;
                if !column_keys.insert(column.key.clone()) {
                    return Err(invalid(
                        Reason::DuplicateTableColumnKey,
                        "blocks.table.columns.key",
                    ));
                }
                keys.push(column.key.clone());
                if let Some(alignment) = column.alignment.as_ref() {
                    match alignment {
                        StructuredTableAlignmentV1::Start
                        | StructuredTableAlignmentV1::Center
                        | StructuredTableAlignmentV1::End => {},
                    }
                }
            }

            let expected_count = keys.len();
            let expected: HashSet<&str> = keys.iter().map(String::as_str).collect();
            for row in rows {
                if row.len() != expected_count {
                    return Err(invalid(Reason::TableShapeMismatch, "blocks.table.rows"));
                }
                for (name, value) in row {
                    if !expected.contains(name.as_str()) {
                        return Err(invalid(Reason::TableShapeMismatch, "blocks.table.rows"));
                    }
                    validate_string(value, MAX_VALUE_BYTES, "blocks.table.rows.value")?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::List {
            title,
            items,
            style: _,
        } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.list.title")?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(invalid(Reason::TooManyListLikeItems, "blocks.list.items"));
            }
            for item in items {
                validate_string(&item.text, MAX_VALUE_BYTES, "blocks.list.items.text")?;
                if let Some(detail) = item.detail.as_deref() {
                    validate_string(detail, MAX_VALUE_BYTES, "blocks.list.items.detail")?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Artifacts { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.artifacts.title")?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(invalid(
                    Reason::TooManyListLikeItems,
                    "blocks.artifacts.items",
                ));
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES, "blocks.artifacts.items.label")?;
                if let Some(href) = item.href.as_deref() {
                    validate_url(href, "blocks.artifacts.items.href")?;
                }
                if let Some(id) = item.artifact_id.as_deref() {
                    validate_string(id, MAX_LABEL_BYTES, "blocks.artifacts.items.artifact_id")?;
                }
                if let Some(mime) = item.mime_type.as_deref() {
                    validate_string(mime, MAX_LABEL_BYTES, "blocks.artifacts.items.mime_type")?;
                }
                if let Some(size) = item.size {
                    if size > i32::MAX as u64 {
                        return Err(invalid(
                            Reason::InvalidNumericValue,
                            "blocks.artifacts.items.size",
                        ));
                    }
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Sources { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.sources.title")?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(invalid(
                    Reason::TooManyListLikeItems,
                    "blocks.sources.items",
                ));
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES, "blocks.sources.items.label")?;
                validate_url(&item.href, "blocks.sources.items.href")?;
            }
            Ok(())
        },
        StructuredResponseBlockV1::Metrics { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES, "blocks.metrics.title")?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(invalid(
                    Reason::TooManyListLikeItems,
                    "blocks.metrics.items",
                ));
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES, "blocks.metrics.items.label")?;
                validate_string(&item.value, MAX_VALUE_BYTES, "blocks.metrics.items.value")?;
                if let Some(unit) = item.unit.as_deref() {
                    validate_string(unit, MAX_LABEL_BYTES, "blocks.metrics.items.unit")?;
                }
            }
            Ok(())
        },
    }
}

fn validate_action(action: &StructuredResponseActionV1) -> Result<(), StructuredResponseInvalid> {
    match action {
        StructuredResponseActionV1::CopyText { label, text } => {
            validate_string(label, MAX_LABEL_BYTES, "actions.label")?;
            validate_string(text, MAX_VALUE_BYTES, "actions.copy_text.text")?;
        },
        StructuredResponseActionV1::OpenUrl { label, url } => {
            validate_string(label, MAX_LABEL_BYTES, "actions.label")?;
            validate_url(url, "actions.open_url.url")?;
        },
        StructuredResponseActionV1::OpenTask { label, task_id } => {
            validate_string(label, MAX_LABEL_BYTES, "actions.label")?;
            validate_string(task_id, MAX_LABEL_BYTES, "actions.open_task.task_id")?;
        },
        StructuredResponseActionV1::OpenArtifact { label, artifact_id } => {
            validate_string(label, MAX_LABEL_BYTES, "actions.label")?;
            validate_string(
                artifact_id,
                MAX_LABEL_BYTES,
                "actions.open_artifact.artifact_id",
            )?;
        },
        StructuredResponseActionV1::SendFollowUp { label, prompt } => {
            validate_string(label, MAX_LABEL_BYTES, "actions.label")?;
            validate_string(prompt, MAX_VALUE_BYTES, "actions.send_follow_up.prompt")?;
        },
        StructuredResponseActionV1::InvokeServerAction { label, action_ref } => {
            // Kept deserializable so existing persisted messages remain readable, but
            // never accepted for new presentations. A mutating server action needs a
            // durable, capability-scoped action record rather than client-visible data.
            let _ = (label, action_ref);
            return Err(invalid(
                StructuredResponseDropReason::ServerActionsUnsupported,
                "actions.invoke_server_action",
            ));
        },
    }

    Ok(())
}

fn plain_text_from_content(content: &ChatMessageContent) -> String {
    match content {
        ChatMessageContent::Text { text, .. } => text.clone(),
        ChatMessageContent::ToolCallExecuted { summary, .. } => summary.clone(),
        ChatMessageContent::RichToolResult {
            summary,
            content_blocks,
            ..
        } => non_empty(Some(summary.as_str()))
            .map(str::to_string)
            .or_else(|| {
                content_blocks.iter().find_map(|block| match block {
                    ContentBlockRecord::Text { text } if !text.trim().is_empty() => {
                        Some(text.clone())
                    },
                    _ => None,
                })
            })
            .unwrap_or_default(),
        ChatMessageContent::Attachment { filename, .. } => filename.clone(),
        ChatMessageContent::TaskStatusUpdate {
            summary, status, ..
        } => summary.clone().unwrap_or_else(|| status.clone()),
        ChatMessageContent::Escalation { question, .. } => question.clone(),
        ChatMessageContent::EscalationResolved { summary, .. } => summary.clone(),
    }
}

fn normalize_for_compare(value: String) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_safe_text(text: &str) -> bool {
    !text
        .chars()
        .any(|ch| matches!(ch, '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{007F}'))
}

fn validate_string(
    text: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), StructuredResponseInvalid> {
    if text.is_empty() {
        return Err(invalid(StructuredResponseDropReason::InvalidText, field));
    }
    if text.len() > max_bytes {
        return Err(invalid(
            StructuredResponseDropReason::InvalidFieldLength,
            field,
        ));
    }
    if !is_safe_text(text) {
        return Err(invalid(
            StructuredResponseDropReason::UnsafeControlCharacter,
            field,
        ));
    }
    Ok(())
}

fn validate_optional_text(
    text: Option<&str>,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), StructuredResponseInvalid> {
    if let Some(value) = text {
        validate_string(value, max_bytes, field)
    } else {
        Ok(())
    }
}

fn validate_url(url: &str, field: &'static str) -> Result<(), StructuredResponseInvalid> {
    if url.len() > MAX_URL_BYTES {
        return Err(invalid(StructuredResponseDropReason::TextTooLarge, field));
    }
    let parsed = Url::parse(url)
        .map_err(|_| invalid(StructuredResponseDropReason::UnsupportedUrl, field))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(invalid(StructuredResponseDropReason::UnsupportedUrl, field)),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        StructuredResponseActionV1, StructuredResponseBlockV1, StructuredResponseDropReason,
        StructuredResponseTone, StructuredResponseV1, StructuredTableAlignmentV1,
    };
    use crate::magician_v2::chat::models::{ChatMessageContent, ContentBlockRecord};

    #[test]
    fn attachment_rejects_mismatched_plain_text() {
        let content = ChatMessageContent::Text {
            text: "Plan completed".to_string(),
            plan_reply: None,
        };
        let response = StructuredResponseV1 {
            schema: super::STRUCTURED_RESPONSE_SCHEMA.to_string(),
            version: 1,
            plain_text: "Different text".to_string(),
            title: None,
            summary: None,
            tone: Some(StructuredResponseTone::Neutral),
            blocks: vec![StructuredResponseBlockV1::Text {
                text: "Different text".to_string(),
            }],
            actions: None,
            model_context: None,
            meta: None,
        };
        assert_eq!(
            StructuredResponseV1::attach_to_content(&content, response).unwrap_err(),
            StructuredResponseDropReason::PlainTextMismatch
        );
    }

    #[test]
    fn validates_minimal_response() {
        let content = ChatMessageContent::Text {
            text: "Plan completed".to_string(),
            plan_reply: None,
        };
        let response = StructuredResponseV1::from_content(&content)
            .expect("canonical text should produce a presentation");
        assert!(StructuredResponseV1::attach_to_content(&content, response).is_ok());
    }

    #[test]
    fn preserves_long_markdown_content_in_the_sidecar() {
        let text = "x".repeat(3_000);
        let content = ChatMessageContent::Text {
            text: text.clone(),
            plan_reply: None,
        };

        let response = StructuredResponseV1::from_content(&content)
            .expect("long markdown should remain presentable");
        assert_eq!(response.plain_text, text);
        assert!(matches!(
            response.blocks.as_slice(),
            [StructuredResponseBlockV1::Markdown { text: block_text }] if block_text.len() == 3_000
        ));
    }

    #[test]
    fn enforces_field_limits_in_utf8_bytes() {
        let at_limit = "é".repeat(super::MAX_VALUE_BYTES / "é".len());
        let over_limit = format!("{at_limit}é");

        assert!(super::validate_block(&StructuredResponseBlockV1::Text { text: at_limit }).is_ok());
        assert_eq!(
            super::validate_block(&StructuredResponseBlockV1::Text { text: over_limit })
                .unwrap_err()
                .reason,
            StructuredResponseDropReason::InvalidFieldLength
        );
    }

    #[test]
    fn rejects_empty_blocks_and_deprecated_server_actions() {
        let empty = StructuredResponseV1 {
            schema: super::STRUCTURED_RESPONSE_SCHEMA.to_string(),
            version: 1,
            plain_text: "Summary".to_string(),
            title: None,
            summary: None,
            tone: None,
            blocks: Vec::new(),
            actions: None,
            model_context: None,
            meta: None,
        };
        assert_eq!(
            super::validate_structured_response(&empty).unwrap_err(),
            StructuredResponseDropReason::MissingBlocks
        );

        let action = StructuredResponseV1 {
            blocks: vec![StructuredResponseBlockV1::Text {
                text: "Summary".to_string(),
            }],
            actions: Some(vec![StructuredResponseActionV1::InvokeServerAction {
                label: "Run".to_string(),
                action_ref: "legacy-action".to_string(),
            }]),
            ..empty
        };
        assert_eq!(
            super::validate_structured_response(&action).unwrap_err(),
            StructuredResponseDropReason::ServerActionsUnsupported
        );
    }

    #[test]
    fn rejects_invalid_table_structure() {
        use std::collections::HashMap;

        let mut row = HashMap::new();
        row.insert("name".to_string(), "value".to_string());
        let response = StructuredResponseV1 {
            schema: super::STRUCTURED_RESPONSE_SCHEMA.to_string(),
            version: 1,
            plain_text: "wrong".to_string(),
            title: None,
            summary: None,
            tone: Some(StructuredResponseTone::Info),
            blocks: vec![StructuredResponseBlockV1::Table {
                title: None,
                columns: vec![super::StructuredTableColumnV1 {
                    key: "wrong".to_string(),
                    label: "Name".to_string(),
                    alignment: Some(StructuredTableAlignmentV1::Center),
                }],
                rows: vec![row],
            }],
            actions: None,
            model_context: None,
            meta: None,
        };
        assert_eq!(
            super::validate_structured_response(&response).unwrap_err(),
            StructuredResponseDropReason::TableShapeMismatch
        );
    }

    #[test]
    fn rejects_sidecars_with_unbound_visible_content() {
        let content = ChatMessageContent::Text {
            text: "Plan completed".to_string(),
            plan_reply: None,
        };
        let mut response = StructuredResponseV1::from_content(&content)
            .expect("canonical text should produce a presentation");
        response.blocks.push(StructuredResponseBlockV1::Callout {
            tone: StructuredResponseTone::Warning,
            title: Some("Injected".to_string()),
            text: "This is not canonical message content".to_string(),
        });

        assert_eq!(
            StructuredResponseV1::attach_to_content(&content, response).unwrap_err(),
            StructuredResponseDropReason::PlainTextMismatch
        );
    }

    #[test]
    fn preserves_full_derived_rich_text_without_truncation() {
        let text = "x".repeat(3_000);
        let mut blocks = Vec::new();
        super::append_content_blocks(
            &mut blocks,
            &[ContentBlockRecord::Text { text: text.clone() }],
            None,
            &mut Vec::new(),
        );

        assert!(matches!(
            blocks.as_slice(),
            [StructuredResponseBlockV1::Markdown { text: block_text }] if block_text == &text
        ));
    }

    #[cfg(test)]
    fn task_output(relative_path: &str, display_name: &str) -> ContentBlockRecord {
        ContentBlockRecord::File {
            source: crate::magician_v2::chat::models::ContentFileSource::TaskOutput {
                task_id: "task-1".to_string(),
            },
            relative_path: relative_path.to_string(),
            display_name: display_name.to_string(),
            mime_type: "text/markdown".to_string(),
            absolute_path: None,
            label: None,
            size: 42,
        }
    }

    #[cfg(test)]
    fn url_block(url: &str, display_name: &str) -> ContentBlockRecord {
        ContentBlockRecord::Url {
            url: url.to_string(),
            display_name: display_name.to_string(),
            mime_type: "text/html".to_string(),
            label: None,
        }
    }

    #[cfg(test)]
    fn task_update(
        display_label: Option<String>,
        summary: Option<String>,
        output_files: Vec<ContentBlockRecord>,
    ) -> ChatMessageContent {
        ChatMessageContent::TaskStatusUpdate {
            task_id: "task-1".to_string(),
            status: "completed".to_string(),
            completion_kind: None,
            display_label,
            summary,
            execution_id: Some("exec-1".to_string()),
            ui_thread_id: None,
            output_files,
            synthesis_pending: false,
            speech_tts: None,
        }
    }

    #[cfg(test)]
    fn artifact_ids(response: &StructuredResponseV1) -> Vec<String> {
        response
            .blocks
            .iter()
            .flat_map(|block| match block {
                StructuredResponseBlockV1::Artifacts { items, .. } => items
                    .iter()
                    .filter_map(|item| item.artifact_id.clone())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn bounds_generated_fields_of_oversized_task_updates() {
        let summary = "s".repeat(5_000);
        // 400 bytes of two-byte characters: the cut must land on a boundary.
        let display_label = "é".repeat(200);
        let long_path = format!("outputs/{}.md", "a".repeat(200));
        let long_url = format!("https://example.com/{}", "p".repeat(300));
        let content = task_update(
            Some(display_label),
            Some(summary.clone()),
            vec![
                task_output(&long_path, &"n".repeat(300)),
                url_block(&long_url, &"d".repeat(300)),
            ],
        );

        let response = StructuredResponseV1::from_content(&content)
            .expect("oversized task update should still project");
        assert!(super::validate_structured_response(&response).is_ok());
        assert!(StructuredResponseV1::attach_to_content(&content, response.clone()).is_ok());

        let title = response.title.as_deref().expect("title");
        assert!(title.len() <= super::MAX_TITLE_BYTES && title.ends_with('…'));
        let bounded_summary = response.summary.as_deref().expect("summary");
        assert!(bounded_summary.len() <= super::MAX_VALUE_BYTES);
        // The callout body keeps the full summary within its own budget.
        assert!(matches!(
            response.blocks.first(),
            Some(StructuredResponseBlockV1::Callout { text, title: Some(callout_title), .. })
                if text == &summary && callout_title == title
        ));
        for block in &response.blocks {
            match block {
                StructuredResponseBlockV1::Artifacts { items, .. } => {
                    assert!(items
                        .iter()
                        .all(|item| item.label.len() <= super::MAX_LABEL_BYTES));
                },
                StructuredResponseBlockV1::Sources { items, .. } => {
                    assert_eq!(items.len(), 1);
                    assert!(items[0].label.len() <= super::MAX_LABEL_BYTES);
                    assert_eq!(items[0].href, long_url);
                },
                _ => {},
            }
        }
        let provenance = &response.meta.as_ref().expect("meta").provenance;
        assert_eq!(provenance.len(), 1);
        assert!(provenance[0].id.len() <= super::MAX_LABEL_BYTES);
        assert_eq!(provenance[0].ref_.as_deref(), Some(long_url.as_str()));
    }

    #[test]
    fn long_artifact_ids_are_stable_unique_and_bounded() {
        let path_a = format!("outputs/{}/a.md", "x".repeat(150));
        let path_b = format!("outputs/{}/b.md", "x".repeat(150));
        let content = task_update(
            None,
            Some("done".to_string()),
            vec![
                task_output("outputs/report.md", "report.md"),
                task_output(&path_a, "a.md"),
                task_output(&path_b, "b.md"),
            ],
        );

        let first = StructuredResponseV1::from_content(&content).expect("first projection");
        let second = StructuredResponseV1::from_content(&content).expect("second projection");
        let ids = artifact_ids(&first);
        assert_eq!(ids, artifact_ids(&second));
        assert_eq!(ids.len(), 3);
        // Ids that already fit keep their historical form.
        assert_eq!(ids[0], "magician-artifact:task:task-1:outputs/report.md");
        assert!(ids.iter().all(|id| id.len() <= super::MAX_LABEL_BYTES));
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn long_tool_names_and_attachment_names_still_project() {
        let tool = ChatMessageContent::ToolCallExecuted {
            tool_name: "t".repeat(400),
            summary: "w".repeat(3_000),
            tool_call_id: None,
        };
        let response = StructuredResponseV1::from_content(&tool).expect("tool projection");
        assert!(response.title.as_deref().expect("title").len() <= super::MAX_TITLE_BYTES);
        assert!(response.summary.as_deref().expect("summary").len() <= super::MAX_VALUE_BYTES);

        let attachment = ChatMessageContent::Attachment {
            filename: format!("{}.pdf", "f".repeat(300)),
            mime_type: "application/pdf".to_string(),
            size: u64::from(u32::MAX),
            absolute_path: None,
            label: None,
        };
        let response =
            StructuredResponseV1::from_content(&attachment).expect("attachment projection");
        let ids = artifact_ids(&response);
        assert_eq!(ids.len(), 1);
        assert!(ids[0].len() <= super::MAX_LABEL_BYTES);
    }

    #[test]
    fn unopenable_source_links_are_left_out_instead_of_dropping_the_card() {
        let content = task_update(
            None,
            Some("done".to_string()),
            vec![
                url_block("file:///etc/hosts", "hosts"),
                url_block("https://example.com", "ok"),
            ],
        );
        let response = StructuredResponseV1::from_content(&content).expect("projection");
        let sources: Vec<_> = response
            .blocks
            .iter()
            .filter_map(|block| match block {
                StructuredResponseBlockV1::Sources { items, .. } => Some(items.len()),
                _ => None,
            })
            .collect();
        assert_eq!(sources, vec![1]);
    }

    #[test]
    fn bounding_respects_utf8_char_boundaries() {
        for pad in 0..4 {
            let text = format!("{}{}", "a".repeat(pad), "日本語🙂".repeat(40));
            let bounded = super::bound_text(&text, super::MAX_TITLE_BYTES);
            assert!(bounded.len() <= super::MAX_TITLE_BYTES);
            assert!(bounded.ends_with('…'));
            let id = super::bound_identifier(&text, super::MAX_LABEL_BYTES);
            assert!(id.len() <= super::MAX_LABEL_BYTES);
            assert_eq!(id, super::bound_identifier(&text, super::MAX_LABEL_BYTES));
        }
        assert_eq!(super::bound_text("short", super::MAX_TITLE_BYTES), "short");
        assert_eq!(
            super::bound_identifier("short", super::MAX_LABEL_BYTES),
            "short"
        );
    }

    #[test]
    fn validation_errors_name_the_failing_field() {
        let response = StructuredResponseV1 {
            schema: super::STRUCTURED_RESPONSE_SCHEMA.to_string(),
            version: 1,
            plain_text: "Summary".to_string(),
            title: Some("t".repeat(super::MAX_TITLE_BYTES + 1)),
            summary: None,
            tone: None,
            blocks: vec![StructuredResponseBlockV1::Text {
                text: "Summary".to_string(),
            }],
            actions: None,
            model_context: None,
            meta: None,
        };
        let error = super::validate_structured_response_fields(&response).unwrap_err();
        assert_eq!(
            error.reason,
            StructuredResponseDropReason::InvalidFieldLength
        );
        assert_eq!(error.field, "title");
    }

    #[test]
    fn shortened_artifact_ids_carry_a_round_tripping_locator() {
        let long_path = format!("outputs/{}/a.md", "x".repeat(150));
        let content = task_update(
            None,
            Some("done".to_string()),
            vec![
                task_output("outputs/report.md", "report.md"),
                task_output(&long_path, "a.md"),
            ],
        );
        let response = StructuredResponseV1::from_content(&content).expect("projection");
        let ids = artifact_ids(&response);
        let locators = &response.meta.as_ref().expect("meta").artifact_locators;
        // Only the shortened id needs a locator; the short one stays parseable.
        assert_eq!(locators.len(), 1);
        assert_eq!(locators[0].artifact_id, ids[1]);
        assert_eq!(locators[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(locators[0].relative_path, long_path);

        let json = serde_json::to_string(&response).expect("serialize");
        assert!(json.contains("\"artifact_locators\""));
        let decoded: StructuredResponseV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, response);
        assert!(StructuredResponseV1::attach_to_content(&content, decoded).is_ok());

        let attachment = ChatMessageContent::Attachment {
            filename: format!("{}.pdf", "f".repeat(300)),
            mime_type: "application/pdf".to_string(),
            size: 10,
            absolute_path: None,
            label: None,
        };
        let response =
            StructuredResponseV1::from_content(&attachment).expect("attachment projection");
        let locators = &response
            .meta
            .as_ref()
            .expect("attachment meta")
            .artifact_locators;
        assert_eq!(locators.len(), 1);
        assert_eq!(locators[0].task_id, None);
        assert_eq!(
            locators[0].relative_path,
            format!("{}.pdf", "f".repeat(300))
        );

        // A path beyond the 2 KiB value budget gets a bounded id but no locator.
        let huge_path = format!("outputs/{}", "y".repeat(3_000));
        let content = task_update(
            None,
            Some("done".to_string()),
            vec![task_output(&huge_path, "y")],
        );
        let response = StructuredResponseV1::from_content(&content).expect("huge path projection");
        assert_eq!(artifact_ids(&response).len(), 1);
        assert!(response
            .meta
            .as_ref()
            .expect("meta")
            .artifact_locators
            .is_empty());
    }

    #[test]
    fn invalid_projection_is_reported_once_per_message() {
        let message_id = "presentation-tests-report-once";
        assert!(super::first_invalid_projection_report(message_id));
        assert!(!super::first_invalid_projection_report(message_id));
    }
}
