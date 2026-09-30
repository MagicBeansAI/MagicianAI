//! Chat Mode Data Models
//! Lightweight session and message types for conversational chat mode.
//! Separate from ExecutionRun — chat is an open-ended conversation layer.

use serde::{Deserialize, Serialize};

use crate::magician_v2::chat::presentation::StructuredResponseV1;
use crate::magician_v2::history::{infer_legacy_session_history_lane, HistoryLane};
use crate::magician_v2::tool_result_projection::ProjectedToolResultV1;

/// A durable chat session. User-facing threads may retain multiple active
/// sessions until explicit archive/delete; rotating feature threads are the
/// narrowly allowlisted exception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    /// Server-owned background provenance. Branches appear in Automated history;
    /// coordinators remain hidden. Parent lineage is independent of the lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_voice: Option<super::voice_requests::InternalVoiceSession>,
    /// Unique session identifier (uuid)
    pub id: String,
    /// The user principal who owns this session
    pub principal: String,
    /// Workspace context
    pub workspace: String,
    /// Assigned personal agent
    pub agent_id: String,
    /// Stable UI thread this session belongs to (for MAGICAN thread navigation)
    #[serde(default = "crate::magician_v2::storage::task_models::default_ui_thread_id")]
    pub ui_thread_id: String,
    /// User-set or auto-generated title from first message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Where this session was started (metadata only)
    pub origin_channel: ChatChannel,
    /// Whether session is active or archived
    pub status: ChatSessionStatus,
    /// User-created/default history versus product-generated activity.
    #[serde(default)]
    pub history_lane: HistoryLane,
    /// True only for the original session in the default `#general` thread.
    /// The file store persists this marker and derives it from creation order
    /// when migrating legacy records that predate the field.
    #[serde(default)]
    pub is_default_session: bool,
    /// Creation timestamp (millis since epoch)
    pub created_at: i64,
    /// Last update timestamp (millis since epoch)
    pub updated_at: i64,
}

impl ChatSession {
    pub fn effective_history_lane(&self) -> HistoryLane {
        if self.internal_voice.is_some() { return HistoryLane::Automated; }
        match self.history_lane {
            HistoryLane::Legacy => {
                infer_legacy_session_history_lane(&self.ui_thread_id, self.title.as_deref())
            },
            lane => lane,
        }
    }

    pub fn normalize_history_lane(&mut self) {
        self.history_lane = self.effective_history_lane();
    }
}

/// Session lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatSessionStatus {
    Active,
    Archived,
}

/// Message handling mode for chat sends.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageMode {
    #[default]
    Ask,
    Plan,
    /// Regular chat send, but in-tree file edits skip permission HITL.
    AcceptInScope,
}

/// Channel origin for a chat session (metadata — does not scope messages).
///
/// Open-ended: any channel type string is accepted. No code change needed
/// to add new channels — consumer channel bots just pass their channel type
/// and address via the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChannel {
    /// Channel type identifier (e.g., "web", "telegram", "discord", "slack", "whatsapp", "imessage")
    pub channel_type: String,
    /// Channel-specific address (e.g., Telegram chat_id, Discord channel_id, phone number).
    /// None for channels that don't need an address (e.g., "web").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl ChatChannel {
    pub fn web() -> Self {
        Self {
            channel_type: "web".to_string(),
            address: None,
        }
    }

    pub fn new(channel_type: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            channel_type: channel_type.into(),
            address: Some(address.into()),
        }
    }
}

/// A display copy can point at another conversation without becoming its history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageContextOrigin {
    pub ui_thread_id: String,
    pub session_id: String,
    pub request_id: String,
    /// Exact saved source row. Optional for projections written before links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Monotonic source version for refreshing a display copy in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_created_at: Option<i64>,
}

/// A single chat message within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Unique message identifier (uuid)
    pub id: String,
    /// The session this message belongs to
    pub session_id: String,
    /// Server-stamped canonical owner of a cross-session display projection.
    /// The enclosing `session_id` remains its display/delivery destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_origin: Option<ChatMessageContextOrigin>,
    /// Who sent this message
    pub direction: ChatMessageDirection,
    /// Message payload
    pub content: ChatMessageContent,
    /// Creation timestamp (millis since epoch)
    pub created_at: i64,
    /// Per-request correlation id stamped onto the outbound user
    /// message and mirrored onto the assistant reply for the same
    /// turn. The UI's `RequestActivityCard` subscribes to
    /// `/events?chat_turn_id=<id>` to render exactly this request's
    /// activity (chat-side LLM/tool emits + delegated/handover sub-
    /// agent emits via the chat-fanout re-stamp). Persisted on disk
    /// so the card survives page refreshes and session reloads —
    /// without this field the side-map fallback only worked within
    /// the current browser session. `#[serde(default)]` so older
    /// messages on disk that pre-date this field deserialize cleanly
    /// with `None`; `skip_serializing_if = Option::is_none` keeps the
    /// JSON tight for sessions whose history was written before the
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    /// Source surface that originated this chat turn (`web`, `mobile`,
    /// `mascot`, `voice`, `screen`, ...). Presence uses the normal chat
    /// ledger and stamps metadata instead of inventing a second message
    /// type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_surface: Option<String>,
    /// Media/control session id associated with the originating presence
    /// surface, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_session_id: Option<String>,
    /// `Some(true)` when this message belongs to a chat turn whose
    /// user message arrived via voice (mic → STT → composer →
    /// auto-send). Both the user message and the assistant reply for
    /// that turn carry the flag, so any surface can decide whether to
    /// auto-read the assistant text aloud without reconstructing the
    /// turn relationship from message ordering.
    ///
    /// `None` (skipped on the wire) for typed turns and for messages
    /// persisted before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_origin: Option<bool>,
    /// Pre-parsed `<speech>` segments for assistant replies that
    /// arrived on a voice-originated turn. The chat service parses
    /// the LLM's tagged output once on persist; every UI surface
    /// reads typed segments off the envelope instead of
    /// re-implementing the parser. `None` for messages with no
    /// audible content, no `<speech>` tags, or persisted before the
    /// field existed.
    ///
    /// Each segment carries the optional delivery hints the LLM
    /// emitted (emotion, style, pace, voice_mode, emphasis) so the
    /// TTS provider chain can honour the model's intent without
    /// the frontend re-parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_segments: Option<Vec<crate::magician_v2::media_seam::SpeechSegment>>,
    /// Optional rendered-only presentation payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<StructuredResponseV1>,
}

/// Persisted structured tool call in the canonical LLM transcript.
impl ChatMessage {
    /// Construct a `ChatMessage` with all required semantic fields and
    /// canonical defaults for optional metadata.
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        direction: ChatMessageDirection,
        content: ChatMessageContent,
        created_at: i64,
    ) -> Self {
        let presentation = if matches!(&direction, ChatMessageDirection::User) {
            None
        } else {
            StructuredResponseV1::from_content(&content)
        };

        Self {
            id: id.into(),
            session_id: session_id.into(),
            context_origin: None,
            direction,
            content,
            created_at,
            chat_turn_id: None,
            source_surface: None,
            presence_session_id: None,
            voice_origin: None,
            speech_segments: None,
            presentation,
        }
    }

    pub fn with_chat_turn_id(mut self, chat_turn_id: Option<String>) -> Self {
        self.chat_turn_id = chat_turn_id;
        self
    }

    pub fn with_context_origin(mut self, origin: ChatMessageContextOrigin) -> Self {
        self.context_origin = Some(origin);
        self
    }

    /// Visible notifications never become conversational turns in their inbox.
    /// Recognize the exact legacy voice projection IDs too, without filtering
    /// canonical branch messages just because they share a voice turn ID.
    pub fn is_context_projection(&self) -> bool {
        if let Some(origin) = &self.context_origin {
            return origin.session_id != self.session_id;
        }
        self.chat_turn_id.as_deref().is_some_and(|turn| {
            (turn.starts_with("voice-request-") || turn.starts_with("voice-task-result-"))
                && (self.id == format!("{turn}-user")
                    || self.id == format!("{turn}-result")
                    // Task notices keep the original request turn correlation.
                    || (self.id.starts_with("voice-task-result-") && self.id.ends_with("-result")))
        })
    }

    pub fn with_source_surface(mut self, source_surface: Option<String>) -> Self {
        self.source_surface = source_surface;
        self
    }

    pub fn with_presence_session_id(mut self, presence_session_id: Option<String>) -> Self {
        self.presence_session_id = presence_session_id;
        self
    }

    pub fn with_voice_origin(mut self, voice_origin: Option<bool>) -> Self {
        self.voice_origin = voice_origin;
        self
    }

    pub fn with_speech_segments(
        mut self,
        speech_segments: Option<Vec<crate::magician_v2::media_seam::SpeechSegment>>,
    ) -> Self {
        self.speech_segments = speech_segments;
        self
    }

    pub fn with_presentation(mut self, presentation: Option<StructuredResponseV1>) -> Self {
        self.presentation = presentation;
        self
    }

    /// Normalize every non-user message at the durability boundary. This also
    /// upgrades historical records on read without changing their canonical
    /// content or transcript semantics.
    pub fn ensure_presentation(&mut self) {
        self.presentation = if matches!(&self.direction, ChatMessageDirection::User) {
            None
        } else if let Some(presentation) = self.presentation.take() {
            StructuredResponseV1::attach_to_content(&self.content, presentation)
                .ok()
                .or_else(|| StructuredResponseV1::from_content(&self.content))
        } else {
            StructuredResponseV1::from_content(&self.content)
        };
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Prompt-visible image reference stored in the chat transcript/session index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptImageRef {
    pub stored_name: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Structured block stored in the canonical chat transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptBlock {
    Text { text: String },
    ImageFile { image: PromptImageRef },
}

/// Provider-native assistant-turn state needed for exact continuation replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum AssistantProviderState {
    OpenaiResponses {
        response_id: String,
        /// True only on a final no-tool response produced from a locally
        /// repaired transcript. This server-owned checkpoint proves that the
        /// response id was created from provider-valid history, so later turns
        /// can resume native continuation without trusting older poisoned ids.
        #[serde(default, skip_serializing_if = "is_false")]
        tool_protocol_repair_checkpoint: bool,
    },
    Gemini {
        parts: Vec<serde_json::Value>,
    },
    AnthropicMessages {
        content: Vec<serde_json::Value>,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Canonical LLM transcript entry persisted alongside display messages.
///
/// This is the server-owned source of truth for later LLM history rebuilds.
/// `ChatMessage` remains the UI/channel-facing timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatLlmTranscriptEntry {
    UserText {
        text: String,
    },
    UserTurn {
        content: Vec<TranscriptBlock>,
    },
    AssistantTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<StoredToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_state: Option<AssistantProviderState>,
    },
    ToolResult {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        content: String,
    },
    ToolResultRich {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        content: Vec<TranscriptBlock>,
    },
    /// Versioned bounded tool result used for provider replay. The complete
    /// post-redaction result remains in the scope-bound raw descriptor; UI
    /// timeline messages continue using their established rich display form.
    ToolResultProjected {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        projection: ProjectedToolResultV1,
    },
}

/// Direction of a chat message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageDirection {
    User,
    Assistant,
    System,
}

/// Content variants for chat messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatMessageContent {
    /// Plain text message
    Text {
        text: String,
        /// Durable association for a user message that answered a specific
        /// planning question. The denormalized labels preserve useful history
        /// even after the task plan advances and removes the pending question.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_reply: Option<PlanReplyMessageContext>,
    },
    /// A tool call that was executed (Phase 2+)
    ToolCallExecuted {
        tool_name: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
    /// A rich tool execution result with structured content blocks.
    RichToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        tool_name: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content_blocks: Vec<ContentBlockRecord>,
    },
    /// A user-attached file staged or persisted in the chat timeline.
    Attachment {
        filename: String,
        mime_type: String,
        size: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        absolute_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// A status update for a task spawned from chat (Phase 3+)
    TaskStatusUpdate {
        task_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        output_files: Vec<ContentBlockRecord>,
        /// `true` while the task has flipped to a terminal status but the final
        /// result is still being synthesized (the status guard holds the
        /// visible status at "running" meanwhile). Lets the card render
        /// "Preparing final result…" instead of a bare running/completed state.
        #[serde(default)]
        synthesis_pending: bool,
        /// Synthesizer-authored spoken summary for off-call TTS read-out
        /// (`VoiceSpeechSummary.tts`). Present on the terminal card once
        /// synthesis lands. The frontend reads it aloud when the task finishes
        /// and no live voice call is running and auto-speak is unmuted (a live
        /// call instead speaks `speech_live` over the voice channel).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speech_tts: Option<String>,
    },

    /// An escalation from the execution pipeline requiring user action.
    ///
    /// Injected into chat by `EscalationListener` when the executor pauses for
    /// user input, confirmation, or max-iterations. Options declare whether
    /// they post canonical HITL or invoke the dedicated continuation endpoint.
    Escalation {
        /// The execution that is paused.
        execution_id: String,
        /// Opaque ID from `FullPauseStore` — passed back on resume.
        pause_state_id: String,
        /// Central `UserRequestService` request ID when this escalation is
        /// service-backed rather than native pause-store backed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        /// Categorisation: "user_input", "confirmation", "max_iterations",
        /// "cannot_proceed", "loop_detected", "tool_authorization", "sandbox_override".
        escalation_type: String,
        /// The pause's expected `input_type` — `external_action`,
        /// `confirmation`, `guidance`, `text`, etc. The resume API
        /// validates the payload against this exact value (see
        /// `web_api.rs::resume_agentic_execution_with_scope`), so the UI
        /// must dispatch off `input_type`, not `escalation_type` (which
        /// is a categorical label that doesn't always agree with the
        /// expected payload shape). Optional for back-compat with older
        /// chat history persisted before this field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_type: Option<String>,
        /// Complete canonical input contract used to render the response UI.
        /// Keeping this beside `input_type` prevents chat clients from losing
        /// constraints such as file multiplicity, labels, instructions, and
        /// whether an arbitrary choice is allowed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_schema: Option<serde_json::Value>,
        /// Human-readable question / summary for the user.
        question: String,
        /// Optional guidance or validation context shown beside the question.
        /// Re-asks update this independently from the question itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// Action buttons the user can click.
        options: Vec<EscalationOption>,
        /// Set to `true` once the escalation has been answered (locally or
        /// via another channel). Buttons are disabled when resolved.
        #[serde(default)]
        resolved: bool,
    },
    /// Lightweight message indicating an earlier escalation was resolved.
    ///
    /// Injected by `EscalationListener` on `AgenticResumed` /
    /// `AgenticExecutionCompleted` so the chat timeline shows resolution.
    EscalationResolved {
        /// Execution that resumed / completed.
        execution_id: String,
        /// Exact pause key that was resolved, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pause_state_id: Option<String>,
        /// Central user-request id that was resolved, when applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        /// Short summary, e.g. "Agent resumed" or "Execution completed".
        summary: String,
        /// Task whose execution just completed (when known). Lets the UI
        /// link from the resolution card to the task detail surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// Output deliverables produced by the resolved execution. Populated
        /// from the task's `outputs/` dir at resolution time, filtered to
        /// agent-emitted artefacts (auto `out_task_*` summary/envelope files
        /// are skipped). Empty when the resolution is a pause/resume rather
        /// than a terminal completion or when the task produced no
        /// deliverables. Frontend renders these via the same
        /// `ChatContentBlocks` path used for `pack_progress` / `attachment`,
        /// so markdown / image / pdf / etc. all preview inline.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        output_files: Vec<ContentBlockRecord>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanReplyMessageContext {
    pub task_id: String,
    pub task_title: String,
    pub question_id: String,
    pub question_text: String,
}

/// Serializable content block record used by rich chat messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockRecord {
    Text {
        text: String,
    },
    File {
        source: ContentFileSource,
        relative_path: String,
        display_name: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        absolute_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        size: u64,
    },
    Url {
        url: String,
        display_name: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// Source discriminator for a file-backed content block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentFileSource {
    SessionOutput,
    TaskOutput { task_id: String },
}

/// Origin of a file stored in a chat session workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatSessionFileOrigin {
    Attachment,
    ToolOutput {
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
    },
}

/// File metadata indexed for chat-session-local uploads and rich tool outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSessionFileRecord {
    pub id: String,
    pub stored_name: String,
    pub original_name: String,
    pub mime_type: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_capture: Option<ScreenCaptureAttachmentContext>,
    #[serde(default)]
    pub prompt_image: bool,
    pub origin: ChatSessionFileOrigin,
    /// Phase 2 — incremental task-output projection: the upstream
    /// `TaskOutput.output_id` this record was projected from, if any.
    /// Set by `project_task_outputs_for_chat_session` when copying a
    /// task's output file into the chat session. Lets repeated
    /// non-terminal projections (running → running → completed) skip
    /// outputs already mirrored, preventing duplicate file copies and
    /// duplicate `file_index` rows for the same logical file. `None`
    /// for user attachments, pack artifacts, or records that pre-date
    /// the field. Pair this with `source_task_id` to identify
    /// (task, output) records uniquely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_output_id: Option<String>,
    /// Phase 2 — the originating task_id for projected task outputs.
    /// Pair with `source_task_output_id` for idempotent re-projection.
    /// `None` for non-task-projected records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    pub created_at: i64,
}

/// Prompt/runtime metadata for files produced by explicit screen capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenCaptureAttachmentContext {
    /// True only when Magician itself captured or decoded the image through a
    /// product-owned screen/context endpoint. Generic multipart metadata is
    /// always overwritten to false before persistence, so it cannot authorize
    /// App Copilot merely by imitating the JSON shape.
    #[serde(default)]
    pub server_registered: bool,
    /// Capture mode that produced the image (`screenshot`, `region`, `clip`).
    pub mode: String,
    /// Coordinate frame represented by the attached image. `capture` means
    /// image-local pixels can be mapped to `screen_rect`; `crop_local` means
    /// the crop origin is unknown and live overlay drawing must not use the
    /// image-local coordinates.
    pub coordinate_space: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<ScreenCaptureImageSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_rect: Option<ScreenCaptureScreenRect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenCaptureImageSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenCaptureScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Per-session file index used to resolve staged attachments and prompt-visible media.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ChatSessionFileIndex {
    #[serde(default)]
    pub files: Vec<ChatSessionFileRecord>,
}

/// A single option button rendered inside an `Escalation` chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationOption {
    /// Machine-readable identifier, e.g. "approve", "deny", "continue",
    /// "stop", "guidance", "done".
    pub id: String,
    /// Human-readable button label.
    pub label: String,
    /// When `true` the frontend should prompt for free-text input before sending.
    #[serde(default)]
    pub requires_input: bool,
    /// Authoritative operation for this option. Clients must not infer a
    /// confirmation value or continuation route from button IDs or labels.
    #[serde(default, skip_serializing_if = "EscalationOptionAction::is_legacy")]
    pub action: EscalationOptionAction,
}

/// Server-authored operation behind an escalation option.
///
/// `Legacy` is deserialize-only compatibility for cards persisted before this
/// contract existed. New cards always carry an explicit action; clients can
/// fail closed on legacy options instead of guessing from human-facing text.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EscalationOptionAction {
    #[default]
    Legacy,
    RespondChoice,
    RespondConfirmation {
        confirmed: bool,
    },
    RespondExternalAction,
    ContinueExecution,
}

impl EscalationOptionAction {
    fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }
}

impl EscalationOption {
    pub fn choice(id: impl Into<String>, label: impl Into<String>, requires_input: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            requires_input,
            action: EscalationOptionAction::RespondChoice,
        }
    }

    pub fn confirmation(id: impl Into<String>, label: impl Into<String>, confirmed: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            requires_input: false,
            action: EscalationOptionAction::RespondConfirmation { confirmed },
        }
    }

    pub fn external_action(
        id: impl Into<String>,
        label: impl Into<String>,
        requires_input: bool,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            requires_input,
            action: EscalationOptionAction::RespondExternalAction,
        }
    }

    pub fn continue_execution(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            requires_input: false,
            action: EscalationOptionAction::ContinueExecution,
        }
    }
}

/// Response returned from `ChatService::process_message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The persisted user message with server-assigned ID.
    /// Allows the frontend to reconcile its optimistic message.
    #[serde(default)]
    pub user_message: Option<ChatMessage>,
    /// The LLM's text response, already persisted
    #[serde(default)]
    pub assistant_message: Option<ChatMessage>,
    /// Tool calls that were auto-executed (Phase 2+)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_executed: Vec<ChatMessage>,
    /// Ordered display messages created by this request after the user message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatMessage>,
    /// Auto-generated or user-set session title.
    /// Present when a title was generated/set during this request (typically
    /// the first message in a session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// When the chat session is mid-turn and this message was held in the
    /// pending-message queue instead of dispatched immediately, this
    /// describes the queue placement. Absent for normally-dispatched
    /// messages and for cancellation responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<QueuedMessageReceipt>,
    /// When the turn was cancelled by `/stop` or `cancel_chat_run`,
    /// this is set instead of `assistant_message`. Partial output is dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    /// Pending-message queue depth for this session after the turn
    /// settled. `None` when the response was queued (no turn ran) or
    /// when the session has never had a queue entry; `Some(0)` after a
    /// normal turn with an empty queue; `Some(N>0)` when there's work
    /// left to drain. Lets the SDK skip the drain GET when the queue
    /// is definitely empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_queue_depth: Option<usize>,
    /// Provider-reported usage/cost for successful LLM calls made while
    /// serving this chat turn. Absent for queued/cancelled turns and when the
    /// provider did not return usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatTurnUsage>,
    /// Public-chat admission notice for deterministic queue/overload/fallback
    /// responses. Normal chat turns leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_chat_notice: Option<PublicChatNotice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicChatNotice {
    pub kind: PublicChatNoticeKind,
    pub template_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicChatNoticeKind {
    Queued,
    Overload,
    FallbackNotice,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnUsage {
    pub calls: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_availability: Option<magicllm::types::UsageAvailability>,
}

impl ChatTurnUsage {
    pub fn record_decision_calls(
        &mut self,
        calls: &[decision_engine_contract::telemetry::DecisionModelCall],
    ) {
        use crate::magician_v2::analytics::decision_model_telemetry::{call_usage, pricing};
        for call in calls {
            let usage = call_usage(call);
            let cost = pricing(&call.provider, &call.model, call.started_at_ms, &usage).cost_usd;
            let prior_cost_known = self.calls == 0 || self.cost_usd.is_some();
            let previous = self
                .usage_availability
                .unwrap_or(magicllm::types::UsageAvailability {
                    tokens: true,
                    cache_read: true,
                    cache_write: true,
                    cost: prior_cost_known,
                });
            self.record_call(
                call.provider.clone(),
                call.model.clone(),
                None,
                usage.prompt_tokens.unwrap_or(0),
                usage.completion_tokens.unwrap_or(0),
                0,
                usage.cached_tokens.unwrap_or(0),
                usage.cache_creation_tokens.unwrap_or(0),
                cost.unwrap_or(0.0),
            );
            let availability = magicllm::types::UsageAvailability {
                tokens: previous.tokens
                    && usage.prompt_tokens.is_some()
                    && usage.completion_tokens.is_some(),
                cache_read: previous.cache_read && usage.cached_tokens.is_some(),
                cache_write: previous.cache_write && usage.cache_creation_tokens.is_some(),
                cost: prior_cost_known && cost.is_some(),
            };
            if !availability.cost {
                self.cost_usd = None;
            }
            self.usage_availability = Some(availability);
        }
    }

    pub fn record_call(
        &mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
        profile: Option<String>,
        input_tokens: u32,
        output_tokens: u32,
        reasoning_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
        cost_usd: f64,
    ) {
        self.calls = self.calls.saturating_add(1);
        merge_usage_label(&mut self.provider, provider.into());
        merge_usage_label(&mut self.model, model.into());
        merge_usage_optional_label(&mut self.profile, profile);
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(reasoning_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(cache_creation_tokens);
        self.total_tokens = self.total_tokens.saturating_add(
            input_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_tokens),
        );
        *self.cost_usd.get_or_insert(0.0) += cost_usd;
    }

    pub fn into_option(self) -> Option<Self> {
        (self.calls > 0).then_some(self)
    }
}

fn merge_usage_optional_label(slot: &mut Option<String>, value: Option<String>) {
    if let Some(value) = value {
        merge_usage_label(slot, value);
    }
}

fn merge_usage_label(slot: &mut Option<String>, value: String) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    match slot.as_deref() {
        None => *slot = Some(value.to_string()),
        Some(existing) if existing == value || existing == "mixed" => {},
        Some(_) => *slot = Some("mixed".to_string()),
    }
}

/// A message held in the chat session's pending-message queue. The queue
/// fills when an inbound message arrives while the session already has an
/// in-flight agent turn. Drain happens FIFO after the current turn settles
/// (successful completion or `/stop`).
///
/// Bounded at [`MAX_QUEUED_PER_SESSION`] per session; oldest drops on
/// overflow with a `chat_queue_dropped_oldest` event surfaced through the
/// realtime broadcaster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    #[serde(default)]
    pub mode: ChatMessageMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(default)]
    pub voice_origin: bool,
    /// Stable id, used for `DELETE /chat/sessions/{id}/queue/{message_id}`.
    pub id: String,
    /// Session this message is queued for.
    pub session_id: String,
    /// User-typed text (None if the inbound was attachments-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Attachment IDs to replay with the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    /// Optional per-call profile override (carried through to drain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_override: Option<String>,
    /// Source surface that originated this queued turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_surface: Option<String>,
    /// Media/control session id associated with the queued turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_session_id: Option<String>,
    /// Channel-provided sender display name, used for public-contact
    /// enrichment when the queued turn is replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_display_name: Option<String>,
    /// Origin channel hint (`telegram`, `chat-web`, `whatsapp`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Origin channel address (chat_id, room_id, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_address: Option<String>,
    /// Explicit VibeDev coding choice for a queued `@vibedev` turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_choice: Option<QueuedCodingChoice>,
    /// When the message was enqueued (millis since epoch).
    pub queued_at: i64,
}

/// Queue-local copy of the VibeDev coding-choice wire form. Kept here so
/// `QueuedMessage` does not depend on the run-service module that already
/// imports `ChatSession`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuedCodingChoice {
    Auto,
    Profile { profile_id: String },
}

/// Receipt returned to the caller when a message was enqueued instead of
/// dispatched. `position` is 1-indexed (first queued = position 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessageReceipt {
    pub id: String,
    pub position: usize,
    /// Set when the enqueue pushed the queue past [`MAX_QUEUED_PER_SESSION`]
    /// and the oldest message was discarded. Carries the discarded message
    /// so the caller can surface the loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_oldest: Option<QueuedMessage>,
    /// Why the message was queued instead of dispatched. The UI uses
    /// this to render a specific "Queued — waiting on task X" string
    /// instead of the generic "queued behind in-flight turn." `None`
    /// for receipts that pre-date the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<QueueReason>,
}

/// Why a message was enqueued instead of dispatched immediately.
/// Surfaced on [`QueuedMessageReceipt::reason`] so the UI can render
/// a contextual "queued behind …" string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum QueueReason {
    /// Another turn is currently in-flight for this session.
    InFlightTurn,
    /// The chat session is tailing a non-terminal task; new messages
    /// queue until the task settles or the user clicks "Stop watching."
    TailingTask { task_id: String },
}

/// Per-session cap on the pending-message queue. Beyond this, the oldest
/// queued message is dropped on enqueue. Tuned small (5) to keep memory
/// bounded and to nudge users toward shorter turns during agent work.
pub const MAX_QUEUED_PER_SESSION: usize = 5;

/// Persistent document for file-based chat session storage.
/// Version 2 is metadata-only. The history fields remain deserializable solely
/// for lazy migration of pre-segmentation session files and are omitted from
/// every new write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionDocument {
    #[serde(default = "legacy_chat_session_document_version")]
    pub format_version: u32,
    pub session: ChatSession,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_history: Vec<ChatLlmTranscriptEntry>,
}

fn legacy_chat_session_document_version() -> u32 {
    1
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        ChatLlmTranscriptEntry, ChatMessageContent, ChatSession, EscalationOption,
        EscalationOptionAction, PlanReplyMessageContext,
    };
    use crate::magician_v2::history::HistoryLane;

    #[test]
    fn legacy_session_history_lane_is_inferred_from_its_product_thread() {
        let session: ChatSession = serde_json::from_value(serde_json::json!({
            "id": "legacy-tabs",
            "principal": "anonymous",
            "workspace": "default",
            "agent_id": "personal-assistant",
            "ui_thread_id": "tabs",
            "origin_channel": { "channel_type": "web" },
            "status": "active",
            "created_at": 1,
            "updated_at": 2
        }))
        .expect("legacy session");

        assert_eq!(session.history_lane, HistoryLane::Legacy);
        assert_eq!(session.effective_history_lane(), HistoryLane::Automated);
    }

    #[test]
    fn legacy_general_session_lane_uses_the_seed_title_allowlist() {
        let session = |title: &str| {
            serde_json::from_value::<ChatSession>(serde_json::json!({
                "id": format!("legacy-{title}"),
                "principal": "anonymous",
                "workspace": "default",
                "agent_id": "personal-assistant",
                "ui_thread_id": "general",
                "title": title,
                "origin_channel": { "channel_type": "web" },
                "status": "active",
                "created_at": 1,
                "updated_at": 2
            }))
            .expect("legacy general session")
        };

        assert_eq!(
            session("Wtf").effective_history_lane(),
            HistoryLane::Personal
        );
        assert_eq!(
            session("Automated run").effective_history_lane(),
            HistoryLane::Automated
        );
    }

    #[test]
    fn legacy_text_message_deserializes_without_plan_reply_context() {
        let content: ChatMessageContent = serde_json::from_value(serde_json::json!({
            "type": "text",
            "text": "hello"
        }))
        .expect("legacy text content should deserialize");

        match content {
            ChatMessageContent::Text { text, plan_reply } => {
                assert_eq!(text, "hello");
                assert!(plan_reply.is_none());
            },
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn legacy_string_tool_result_transcript_still_hydrates_exactly() {
        let entry: ChatLlmTranscriptEntry = serde_json::from_value(serde_json::json!({
            "type": "tool_result",
            "tool_call_id": "legacy-call",
            "tool_name": "lookup",
            "content": "{\"status\":\"ok\",\"value\":\"exact\"}"
        }))
        .expect("legacy transcript entry should deserialize");

        assert!(matches!(
            entry,
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id,
                tool_name: Some(tool_name),
                content,
            } if tool_call_id == "legacy-call"
                && tool_name == "lookup"
                && content == "{\"status\":\"ok\",\"value\":\"exact\"}"
        ));
    }

    #[test]
    fn plan_reply_context_round_trips_with_text_message() {
        let expected = PlanReplyMessageContext {
            task_id: "task-1".to_string(),
            task_title: "Ship the site".to_string(),
            question_id: "question-1".to_string(),
            question_text: "Which domain?".to_string(),
        };
        let content = ChatMessageContent::Text {
            text: "Use example.com".to_string(),
            plan_reply: Some(expected.clone()),
        };

        let encoded = serde_json::to_value(&content).expect("text content should serialize");
        let decoded: ChatMessageContent =
            serde_json::from_value(encoded).expect("text content should deserialize");

        match decoded {
            ChatMessageContent::Text { text, plan_reply } => {
                assert_eq!(text, "Use example.com");
                assert_eq!(plan_reply, Some(expected));
            },
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn escalation_action_is_explicit_while_legacy_cards_still_deserialize() {
        let legacy: ChatMessageContent = serde_json::from_value(serde_json::json!({
            "type": "escalation",
            "execution_id": "exec-1",
            "pause_state_id": "pause-1",
            "escalation_type": "confirmation",
            "input_type": "confirmation",
            "question": "Proceed?",
            "options": [{"id": "yes", "label": "Yes", "requires_input": false}],
            "resolved": false
        }))
        .expect("legacy escalation should deserialize");
        match legacy {
            ChatMessageContent::Escalation { options, hint, .. } => {
                assert_eq!(options[0].action, EscalationOptionAction::Legacy);
                assert_eq!(hint, None);
            },
            other => panic!("expected escalation, got {other:?}"),
        }

        let option = EscalationOption::confirmation("custom", "Launch", true);
        let encoded = serde_json::to_value(option).expect("explicit action should serialize");
        assert_eq!(encoded["action"]["type"], "respond_confirmation");
        assert_eq!(encoded["action"]["confirmed"], true);

        let with_hint: ChatMessageContent = serde_json::from_value(serde_json::json!({
            "type": "escalation",
            "execution_id": "exec-2",
            "pause_state_id": "pause-2",
            "escalation_type": "user_input",
            "question": "Which account?",
            "hint": "For example, billing",
            "options": [],
            "resolved": false
        }))
        .expect("hinted escalation should deserialize");
        assert!(matches!(
            with_hint,
            ChatMessageContent::Escalation { hint: Some(hint), .. }
                if hint == "For example, billing"
        ));
    }
}
