//! Structured presentation sidecar for chat messages.
//!
//! This module defines a versioned `StructuredResponseV1` contract plus a
//! bounded validation path. The sidecar is additive and optional; `content`
//! remains the canonical answer representation.

use std::collections::{HashMap, HashSet};
use std::fmt;

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
    pub fn from_content(content: &ChatMessageContent) -> Option<Self> {
        let plain_text = normalize_for_compare(plain_text_from_content(content));
        if plain_text.trim().is_empty() {
            return None;
        }

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
                let title = format!("Action completed: {tool_name}");
                (
                    Some(title.clone()),
                    Some(summary.clone()),
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
                append_content_blocks(&mut blocks, content_blocks, None);
                let provenance = provenance_refs_from_content_blocks(content_blocks);
                (
                    Some("Action result".to_string()),
                    summary,
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
                let label = non_empty(label.as_deref()).unwrap_or(filename).to_string();
                (
                    Some("Attachment".to_string()),
                    None,
                    Some(StructuredResponseTone::Neutral),
                    vec![StructuredResponseBlockV1::Artifacts {
                        title: Some("Attachment".to_string()),
                        items: vec![StructuredArtifactRefV1 {
                            label,
                            href: None,
                            artifact_id: Some(session_artifact_id(filename)),
                            mime_type: non_empty(Some(mime_type)).map(str::to_string),
                            size: Some(*size),
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
                let title = non_empty(display_label.as_deref())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Task {task_id}: {status}"));
                let tone = task_status_tone(status);
                let mut blocks = vec![StructuredResponseBlockV1::Callout {
                    tone: tone.clone(),
                    title: Some(title.clone()),
                    text: body.clone(),
                }];
                append_content_blocks(&mut blocks, output_files, Some(task_id));
                (
                    Some(title),
                    Some(body),
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
                        cost: None,
                        confidence: None,
                        created_at: None,
                    }),
                )
            },
            // Escalations carry live pause authority and remain owned by the
            // HITL components rather than a display-sidecar action contract.
            ChatMessageContent::Escalation { .. } => return None,
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
                append_content_blocks(&mut blocks, output_files, task_id.as_deref());
                let actions = task_id.as_ref().map(|task_id| {
                    vec![StructuredResponseActionV1::OpenTask {
                        label: "Open task".to_string(),
                        task_id: task_id.clone(),
                    }]
                });
                (
                    Some("Request resolved".to_string()),
                    Some(summary.clone()),
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
            meta,
        };
        match validate_structured_response(&response) {
            Ok(()) => Some(response),
            Err(reason) => {
                tracing::warn!(?reason, "dropping invalid structured chat presentation");
                None
            },
        }
    }
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
) {
    for block in content {
        if let ContentBlockRecord::Text { text } = block {
            if !text.trim().is_empty() {
                blocks.push(StructuredResponseBlockV1::Markdown { text: text.clone() });
            }
        }
    }
    let artifacts = artifact_refs_from_content_blocks(content, task_id);
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
) -> Vec<StructuredArtifactRefV1> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlockRecord::File {
                source: _,
                relative_path,
                display_name,
                mime_type,
                absolute_path: _,
                label,
                size,
            } => Some(StructuredArtifactRefV1 {
                label: non_empty(label.as_deref())
                    .unwrap_or(display_name)
                    .to_string(),
                href: None,
                artifact_id: Some(match task_id {
                    Some(task_id) => task_artifact_id(task_id, relative_path),
                    None => session_artifact_id(relative_path),
                }),
                mime_type: non_empty(Some(mime_type)).map(str::to_string),
                size: Some(*size),
            }),
            _ => None,
        })
        .collect()
}

fn session_artifact_id(relative_path: &str) -> String {
    format!("{STRUCTURED_ARTIFACT_SESSION_PREFIX}{relative_path}")
}

fn task_artifact_id(task_id: &str, relative_path: &str) -> String {
    format!("{STRUCTURED_ARTIFACT_TASK_PREFIX}{task_id}:{relative_path}")
}

fn source_refs_from_content_blocks(content: &[ContentBlockRecord]) -> Vec<StructuredSourceRefV1> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlockRecord::Url {
                url,
                display_name,
                label,
                ..
            } if !url.trim().is_empty() => Some(StructuredSourceRefV1 {
                label: non_empty(label.as_deref())
                    .unwrap_or(display_name)
                    .to_string(),
                href: url.clone(),
            }),
            _ => None,
        })
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
            } if !url.trim().is_empty() => Some(StructuredProvenanceRefV1 {
                id: url.clone(),
                label: Some(
                    non_empty(label.as_deref())
                        .unwrap_or(display_name)
                        .to_string(),
                ),
                ref_: Some(url.clone()),
            }),
            _ => None,
        })
        .collect()
}

pub fn validate_structured_response(
    response: &StructuredResponseV1,
) -> Result<(), StructuredResponseDropReason> {
    if response.schema.is_empty() {
        return Err(StructuredResponseDropReason::MissingSchema);
    }
    if response.schema != STRUCTURED_RESPONSE_SCHEMA {
        return Err(StructuredResponseDropReason::UnsupportedSchema);
    }
    if response.version != 1 {
        return Err(StructuredResponseDropReason::UnsupportedVersion);
    }

    if response.blocks.is_empty() {
        return Err(StructuredResponseDropReason::MissingBlocks);
    }
    if response.blocks.len() > MAX_BLOCKS {
        return Err(StructuredResponseDropReason::TooManyBlocks);
    }

    if let Some(actions) = response.actions.as_ref() {
        if actions.len() > MAX_ACTIONS {
            return Err(StructuredResponseDropReason::TooManyActions);
        }
        for action in actions {
            validate_action(action)?;
        }
    }

    if response.plain_text.is_empty() {
        return Err(StructuredResponseDropReason::MissingPlainText);
    }
    if response.plain_text.as_bytes().len() > MAX_AGGREGATE_TEXT_BYTES {
        return Err(StructuredResponseDropReason::TextTooLarge);
    }
    if !is_safe_text(&response.plain_text) {
        return Err(StructuredResponseDropReason::UnsafeControlCharacter);
    }

    validate_optional_text(response.title.as_deref(), MAX_TITLE_BYTES)?;
    validate_optional_text(response.summary.as_deref(), MAX_VALUE_BYTES)?;

    for block in &response.blocks {
        validate_block(block)?;
    }

    if let Some(model_context) = response.model_context.as_ref() {
        if model_context.summary.is_empty() || model_context.summary.len() > MAX_VALUE_BYTES {
            return Err(StructuredResponseDropReason::InvalidText);
        }
        if !is_safe_text(&model_context.summary) {
            return Err(StructuredResponseDropReason::UnsafeControlCharacter);
        }
        for fact in &model_context.visible_facts {
            validate_string(fact, MAX_VALUE_BYTES)?;
        }
        if let Some(item) = model_context.selected_item.as_ref() {
            validate_string(item, MAX_LABEL_BYTES)?;
        }
    }

    if let Some(meta) = response.meta.as_ref() {
        validate_optional_text(meta.response_id.as_deref(), MAX_LABEL_BYTES)?;
        validate_optional_text(meta.source_surface.as_deref(), MAX_LABEL_BYTES)?;
        validate_optional_text(meta.task_id.as_deref(), MAX_LABEL_BYTES)?;
        validate_optional_text(meta.execution_id.as_deref(), MAX_LABEL_BYTES)?;
        validate_optional_text(meta.chat_turn_id.as_deref(), MAX_LABEL_BYTES)?;
        for item in &meta.provenance {
            validate_string(&item.id, MAX_LABEL_BYTES)?;
            if let Some(label) = item.label.as_deref() {
                validate_string(label, MAX_LABEL_BYTES)?;
            }
            if let Some(reference) = item.ref_.as_deref() {
                validate_string(reference, MAX_VALUE_BYTES)?;
            }
        }
        if let Some(confidence) = meta.confidence {
            if !(0.0..=1.0).contains(&confidence) {
                return Err(StructuredResponseDropReason::InvalidConfidence);
            }
        }
        if let Some(cost) = meta.cost.as_ref() {
            if let Some(value) = cost.cost_usd {
                if !value.is_finite() {
                    return Err(StructuredResponseDropReason::InvalidNumericValue);
                }
            }
            if let Some(model) = cost.model.as_deref() {
                validate_string(model, MAX_LABEL_BYTES)?;
            }
        }
    }

    let bytes =
        serde_json::to_vec(response).map_err(|_| StructuredResponseDropReason::InvalidText)?;
    if bytes.len() > MAX_PRESENTATION_BYTES {
        return Err(StructuredResponseDropReason::PresentationTooLarge);
    }

    Ok(())
}

fn validate_block(block: &StructuredResponseBlockV1) -> Result<(), StructuredResponseDropReason> {
    match block {
        StructuredResponseBlockV1::Markdown { text } => {
            validate_string(text, MAX_AGGREGATE_TEXT_BYTES)
        },
        StructuredResponseBlockV1::Text { text } => validate_string(text, MAX_VALUE_BYTES),
        StructuredResponseBlockV1::Callout { title, text, .. } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            validate_string(text, MAX_AGGREGATE_TEXT_BYTES)
        },
        StructuredResponseBlockV1::KeyValues { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(StructuredResponseDropReason::TooManyListLikeItems);
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES)?;
                validate_string(&item.value, MAX_VALUE_BYTES)?;
                if let Some(hint) = item.hint.as_deref() {
                    validate_string(hint, MAX_VALUE_BYTES)?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Table {
            title,
            columns,
            rows,
        } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if columns.is_empty() || columns.len() > MAX_TABLE_COLUMNS {
                return Err(StructuredResponseDropReason::TooManyTableColumns);
            }
            if rows.len() > MAX_TABLE_ROWS {
                return Err(StructuredResponseDropReason::TooManyTableRows);
            }

            let mut column_keys = HashSet::with_capacity(columns.len());
            let mut keys = Vec::with_capacity(columns.len());
            for column in columns {
                validate_string(&column.key, MAX_LABEL_BYTES)?;
                validate_string(&column.label, MAX_LABEL_BYTES)?;
                if !column_keys.insert(column.key.clone()) {
                    return Err(StructuredResponseDropReason::DuplicateTableColumnKey);
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
                    return Err(StructuredResponseDropReason::TableShapeMismatch);
                }
                for (name, value) in row {
                    if !expected.contains(name.as_str()) {
                        return Err(StructuredResponseDropReason::TableShapeMismatch);
                    }
                    validate_string(value, MAX_VALUE_BYTES)?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::List {
            title,
            items,
            style: _,
        } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(StructuredResponseDropReason::TooManyListLikeItems);
            }
            for item in items {
                validate_string(&item.text, MAX_VALUE_BYTES)?;
                if let Some(detail) = item.detail.as_deref() {
                    validate_string(detail, MAX_VALUE_BYTES)?;
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Artifacts { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(StructuredResponseDropReason::TooManyListLikeItems);
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES)?;
                if let Some(href) = item.href.as_deref() {
                    validate_url(href)?;
                }
                if let Some(id) = item.artifact_id.as_deref() {
                    validate_string(id, MAX_LABEL_BYTES)?;
                }
                if let Some(mime) = item.mime_type.as_deref() {
                    validate_string(mime, MAX_LABEL_BYTES)?;
                }
                if let Some(size) = item.size {
                    if size > i32::MAX as u64 {
                        return Err(StructuredResponseDropReason::InvalidNumericValue);
                    }
                }
            }
            Ok(())
        },
        StructuredResponseBlockV1::Sources { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(StructuredResponseDropReason::TooManyListLikeItems);
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES)?;
                validate_url(&item.href)?;
            }
            Ok(())
        },
        StructuredResponseBlockV1::Metrics { title, items } => {
            validate_optional_text(title.as_deref(), MAX_TITLE_BYTES)?;
            if items.len() > MAX_LIST_LIKE_ITEMS {
                return Err(StructuredResponseDropReason::TooManyListLikeItems);
            }
            for item in items {
                validate_string(&item.label, MAX_LABEL_BYTES)?;
                validate_string(&item.value, MAX_VALUE_BYTES)?;
                if let Some(unit) = item.unit.as_deref() {
                    validate_string(unit, MAX_LABEL_BYTES)?;
                }
            }
            Ok(())
        },
    }
}

fn validate_action(
    action: &StructuredResponseActionV1,
) -> Result<(), StructuredResponseDropReason> {
    match action {
        StructuredResponseActionV1::CopyText { label, text } => {
            validate_string(label, MAX_LABEL_BYTES)?;
            validate_string(text, MAX_VALUE_BYTES)?;
        },
        StructuredResponseActionV1::OpenUrl { label, url } => {
            validate_string(label, MAX_LABEL_BYTES)?;
            validate_url(url)?;
        },
        StructuredResponseActionV1::OpenTask { label, task_id } => {
            validate_string(label, MAX_LABEL_BYTES)?;
            validate_string(task_id, MAX_LABEL_BYTES)?;
        },
        StructuredResponseActionV1::OpenArtifact { label, artifact_id } => {
            validate_string(label, MAX_LABEL_BYTES)?;
            validate_string(artifact_id, MAX_LABEL_BYTES)?;
        },
        StructuredResponseActionV1::SendFollowUp { label, prompt } => {
            validate_string(label, MAX_LABEL_BYTES)?;
            validate_string(prompt, MAX_VALUE_BYTES)?;
        },
        StructuredResponseActionV1::InvokeServerAction { label, action_ref } => {
            // Kept deserializable so existing persisted messages remain readable, but
            // never accepted for new presentations. A mutating server action needs a
            // durable, capability-scoped action record rather than client-visible data.
            let _ = (label, action_ref);
            return Err(StructuredResponseDropReason::ServerActionsUnsupported);
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

fn validate_string(text: &str, max_bytes: usize) -> Result<(), StructuredResponseDropReason> {
    if text.is_empty() {
        return Err(StructuredResponseDropReason::InvalidText);
    }
    if text.len() > max_bytes {
        return Err(StructuredResponseDropReason::InvalidFieldLength);
    }
    if !is_safe_text(text) {
        return Err(StructuredResponseDropReason::UnsafeControlCharacter);
    }
    Ok(())
}

fn validate_optional_text(
    text: Option<&str>,
    max_bytes: usize,
) -> Result<(), StructuredResponseDropReason> {
    if let Some(value) = text {
        validate_string(value, max_bytes)
    } else {
        Ok(())
    }
}

fn validate_url(url: &str) -> Result<(), StructuredResponseDropReason> {
    if url.len() > MAX_URL_BYTES {
        return Err(StructuredResponseDropReason::TextTooLarge);
    }
    let parsed = Url::parse(url).map_err(|_| StructuredResponseDropReason::UnsupportedUrl)?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(StructuredResponseDropReason::UnsupportedUrl),
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
                .unwrap_err(),
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
        );

        assert!(matches!(
            blocks.as_slice(),
            [StructuredResponseBlockV1::Markdown { text: block_text }] if block_text == &text
        ));
    }
}
