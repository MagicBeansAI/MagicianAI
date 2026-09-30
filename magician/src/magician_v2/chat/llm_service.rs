//! Chat LLM Service — context assembly and direct LLM call for chat mode.
//!
//! Phase 1: no tools, pure conversation. The LLM has a system prompt and
//! chat history only. Returns the text response.
//!
//! Phase 2: supports tool calling. Accepts tool specs, passes them to the LLM,
//! and returns both text content and tool calls from the response.
//!
//! Uses proper multi-message API with distinct roles (system, user, assistant)
//! so the LLM can distinguish who said what.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use magicllm::capability::LLMProviderKind;
use magicllm::prelude::{
    anthropic_raw_content_block, ContentBlock as RouterContentBlock, LLMMessage as RouterMessage,
    StreamDelta,
};
use magicllm::types::{LLMResponse, LLMToolCall, LLMToolSpec, MessageRole};
use serde_json::{json, Value};
use tracing::debug;

use crate::magician_v2::analytics::runtime_activity_layer::current_activity_id;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::query_analysis::multi_llm_service::{
    ChatCompletionRequestOverrides, ChatCompletionResponse, ChatCompletionTelemetryHint,
    ChatProfileInfo, ChatProfileWarning, LLMOperation, MultiLLMService,
};
use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;

use super::models::{
    AssistantProviderState, ChatLlmTranscriptEntry, ChatSessionFileIndex, ChatSessionFileOrigin,
    ChatSessionFileRecord, StoredToolCall, TranscriptBlock,
};

const MAX_RECENT_TOOL_OUTPUT_REFERENCES: usize = 5;
const MAX_RECENT_TOOL_OUTPUT_IMAGES: usize = 3;

/// Response from the chat LLM service that includes both text and tool calls.
#[derive(Debug, Clone)]
pub struct ChatLlmResponse {
    /// Text content from the LLM (may be None when only tool calls are returned).
    pub content: Option<String>,
    /// Tool calls requested by the LLM.
    pub tool_calls: Vec<LLMToolCall>,
    /// Provider-native assistant turn state required for exact continuation replay.
    pub provider_state: Option<AssistantProviderState>,
    /// Provider-emitted reasoning / chain-of-thought text (Anthropic
    /// extended-thinking, OpenAI Responses reasoning items, DeepSeek-R1
    /// `message.reasoning_content`). Forwarded from the underlying
    /// `ChatCompletionResponse.reasoning_text` so the chat outer-loop
    /// can emit `reasoning.start/content/end` events without going
    /// back to the raw response.
    pub reasoning_text: Option<String>,
    /// Token usage as reported by the provider, including cache-read,
    /// cache-write, and reasoning breakdowns when the provider surfaces
    /// them. Forwarded so the chat outer-loop can emit
    /// `LLMResponseReceived` (and downstream `analytics/llm_calls/*.parquet`,
    /// `chat_session_cache_summary` DuckDB view) with the same fidelity as
    /// the autonomous loop.
    pub usage: Option<crate::magician_v2::query_analysis::multi_llm_service::LLMUsage>,
    /// Resolved provider/model/profile/cost telemetry for the provider call.
    /// Chat service forwards this into the canonical `/llm` sink.
    pub telemetry: Option<LlmCallTelemetry>,
}

#[derive(Debug, Clone)]
pub struct ChatTranscriptContext {
    pub session_id: String,
    pub principal: String,
    pub workspace: String,
    /// Stable user-turn identity. Calls in the same tool loop receive fresh
    /// logical call ids under this shared root trace.
    pub chat_turn_id: Option<String>,
    pub user_message_id: Option<String>,
    /// Optional caller-minted identity for the next physical model call. Most
    /// callers leave this empty and let the adapter mint one; cancellable chat
    /// streaming supplies it so dropping the future cannot lose the receipt.
    pub trace_context: Option<magicllm::LlmTraceContext>,
    pub provider_attempt_counter: Option<Arc<std::sync::atomic::AtomicU32>>,
}

#[derive(Debug, Clone, Default)]
struct ChatProfileCapabilities {
    provider_kind: Option<LLMProviderKind>,
    openai_api_mode: Option<&'static str>,
    supports_user_image_inputs: bool,
    supports_multimodal_tool_result_replay: bool,
}

#[derive(Debug, Clone, Default)]
struct BuiltChatRequest {
    messages: Vec<RouterMessage>,
    request_overrides: Option<ChatCompletionRequestOverrides>,
    /// True when the rendered request had to repair corruption newer than the
    /// last durable clean-response checkpoint. A successful final OpenAI
    /// Responses turn promotes its response id to the next checkpoint.
    tool_protocol_repair_checkpoint_required: bool,
}

/// Content-free view of the exact provider request produced after transcript
/// repair. The provider-replay live lane consumes this crate-internal seam so
/// it exercises the production request builder without exposing prompts,
/// results, or the private `BuiltChatRequest` representation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderReplayRequestAudit {
    pub provider: String,
    pub replay_protocol: String,
    pub native_tool_replay: bool,
    pub repaired: bool,
    pub checkpoint_required: bool,
    pub removed_tool_calls: u64,
    pub removed_tool_results: u64,
    pub duplicate_tool_call_ids: u64,
    pub retained_provider_state_count: usize,
    pub retained_tool_call_ids: Vec<String>,
    pub retained_tool_result_ids: Vec<String>,
    pub rendered_native_tool_call_ids: Vec<String>,
    pub rendered_native_tool_result_ids: Vec<String>,
    pub previous_response_id: Option<String>,
    pub rendered_message_count: usize,
}

/// Content-free accounting for bounded projections replayed while constructing
/// provider history. Materialization emits per-result metrics once; this
/// aggregate makes repeated-history cost avoidance observable on every turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ToolResultProjectionReplayStats {
    replay_count: u64,
    invalid_projection_count: u64,
    cumulative_raw_bytes: u64,
    cumulative_model_bytes: u64,
    cumulative_estimated_model_tokens: u64,
    cumulative_bytes_saved: u64,
}

/// Content-free accounting for provider-protocol repair applied to a persisted
/// transcript before selecting a continuation anchor or rendering messages.
///
/// A process interruption can occur after an assistant tool-call turn is
/// durably appended but before every result is appended. Native tool providers
/// reject that history. Repair is deliberately in-memory: durable history and
/// its debug evidence remain unchanged, while provider input keeps every
/// complete call/result pair and removes only entries that cannot be paired.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ToolProtocolRepairStats {
    first_corruption_index: Option<usize>,
    latest_corruption_index: Option<usize>,
    trusted_checkpoint_index: Option<usize>,
    first_uncheckpointed_corruption_index: Option<usize>,
    removed_tool_calls: u64,
    removed_tool_results: u64,
    duplicate_tool_call_ids: u64,
}

/// Borrowed provider-input projection of one persisted transcript entry.
///
/// Request construction must own the final `RouterMessage` payload, but it
/// does not need to own two intermediate copies of the complete transcript.
/// This view keeps persisted semantic values borrowed through protocol repair
/// and stale-task analysis. Only the small interruption note for a genuinely
/// damaged assistant turn is allocated before the final provider message.
#[derive(Debug)]
enum ProviderHistoryEntryView<'a> {
    UserText {
        original_index: usize,
        text: &'a str,
    },
    UserTurn {
        original_index: usize,
        content: &'a [TranscriptBlock],
    },
    AssistantTurn {
        original_index: usize,
        text: Option<Cow<'a, str>>,
        tool_calls: ProviderToolCallsView<'a>,
        provider_state: Option<&'a AssistantProviderState>,
    },
    ToolResult {
        original_index: usize,
        tool_call_id: &'a str,
        tool_name: Option<&'a str>,
        content: &'a str,
    },
    ToolResultRich {
        original_index: usize,
        tool_call_id: &'a str,
        tool_name: Option<&'a str>,
        content: &'a [TranscriptBlock],
    },
    ToolResultProjected {
        original_index: usize,
        tool_call_id: &'a str,
        tool_name: Option<&'a str>,
        projection: &'a crate::magician_v2::tool_result_projection::ProjectedToolResultV1,
    },
}

/// Either the original contiguous tool-call slice or a pointer-only filtered
/// set used for a damaged assistant turn. The common path allocates nothing.
#[derive(Debug)]
struct ProviderToolCallsView<'a> {
    borrowed: Option<&'a [StoredToolCall]>,
    filtered: Vec<&'a StoredToolCall>,
}

impl<'a> ProviderToolCallsView<'a> {
    fn borrowed(calls: &'a [StoredToolCall]) -> Self {
        Self {
            borrowed: Some(calls),
            filtered: Vec::new(),
        }
    }

    fn filtered(calls: Vec<&'a StoredToolCall>) -> Self {
        Self {
            borrowed: None,
            filtered: calls,
        }
    }

    fn iter(&self) -> impl Iterator<Item = &'a StoredToolCall> + '_ {
        self.borrowed
            .into_iter()
            .flat_map(|calls| calls.iter())
            .chain(self.filtered.iter().copied())
    }
}

impl<'a> ProviderHistoryEntryView<'a> {
    fn original_index(&self) -> usize {
        match self {
            Self::UserText { original_index, .. }
            | Self::UserTurn { original_index, .. }
            | Self::AssistantTurn { original_index, .. }
            | Self::ToolResult { original_index, .. }
            | Self::ToolResultRich { original_index, .. }
            | Self::ToolResultProjected { original_index, .. } => *original_index,
        }
    }

    fn is_user(&self) -> bool {
        matches!(self, Self::UserText { .. } | Self::UserTurn { .. })
    }

    fn tool_result_call_id(&self) -> Option<&'a str> {
        match self {
            Self::ToolResult { tool_call_id, .. }
            | Self::ToolResultRich { tool_call_id, .. }
            | Self::ToolResultProjected { tool_call_id, .. } => Some(*tool_call_id),
            _ => None,
        }
    }
}

impl ToolProtocolRepairStats {
    fn mark_corruption(&mut self, index: usize) {
        self.first_corruption_index = Some(
            self.first_corruption_index
                .map_or(index, |current| current.min(index)),
        );
        self.latest_corruption_index = Some(
            self.latest_corruption_index
                .map_or(index, |current| current.max(index)),
        );
        if self
            .trusted_checkpoint_index
            .is_none_or(|checkpoint| index >= checkpoint)
        {
            self.first_uncheckpointed_corruption_index = Some(
                self.first_uncheckpointed_corruption_index
                    .map_or(index, |current| current.min(index)),
            );
        }
    }

    fn repaired(self) -> bool {
        self.first_corruption_index.is_some()
    }

    fn checkpoint_required(self) -> bool {
        self.first_uncheckpointed_corruption_index.is_some()
    }
}

impl ToolResultProjectionReplayStats {
    #[cfg(any(test, feature = "test-fixtures"))]
    fn from_history(history: &[ChatLlmTranscriptEntry]) -> Self {
        let mut stats = Self::default();
        for entry in history {
            let ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id,
                projection,
                ..
            } = entry
            else {
                continue;
            };
            if projection.validate_schema_version().is_err()
                || projection.identity.tool_call_id.as_str() != tool_call_id
            {
                stats.invalid_projection_count = stats.invalid_projection_count.saturating_add(1);
                continue;
            }
            let raw_bytes = u64::try_from(projection.metrics.raw_bytes).unwrap_or(u64::MAX);
            let model_bytes = u64::try_from(projection.metrics.model_bytes).unwrap_or(u64::MAX);
            stats.replay_count = stats.replay_count.saturating_add(1);
            stats.cumulative_raw_bytes = stats.cumulative_raw_bytes.saturating_add(raw_bytes);
            stats.cumulative_model_bytes = stats.cumulative_model_bytes.saturating_add(model_bytes);
            stats.cumulative_estimated_model_tokens =
                stats.cumulative_estimated_model_tokens.saturating_add(
                    u64::try_from(projection.metrics.estimated_model_tokens).unwrap_or(u64::MAX),
                );
            stats.cumulative_bytes_saved = stats
                .cumulative_bytes_saved
                .saturating_add(raw_bytes.saturating_sub(model_bytes));
        }
        stats
    }

    fn from_provider_view(
        history: &[ProviderHistoryEntryView<'_>],
        neutralized_call_ids: &HashSet<&str>,
    ) -> Self {
        let mut stats = Self::default();
        for entry in history {
            let ProviderHistoryEntryView::ToolResultProjected {
                tool_call_id,
                projection,
                ..
            } = entry
            else {
                continue;
            };
            if neutralized_call_ids.contains(*tool_call_id) {
                continue;
            }
            if projection.validate_schema_version().is_err()
                || projection.identity.tool_call_id.as_str() != *tool_call_id
            {
                stats.invalid_projection_count = stats.invalid_projection_count.saturating_add(1);
                continue;
            }
            let raw_bytes = u64::try_from(projection.metrics.raw_bytes).unwrap_or(u64::MAX);
            let model_bytes = u64::try_from(projection.metrics.model_bytes).unwrap_or(u64::MAX);
            stats.replay_count = stats.replay_count.saturating_add(1);
            stats.cumulative_raw_bytes = stats.cumulative_raw_bytes.saturating_add(raw_bytes);
            stats.cumulative_model_bytes = stats.cumulative_model_bytes.saturating_add(model_bytes);
            stats.cumulative_estimated_model_tokens =
                stats.cumulative_estimated_model_tokens.saturating_add(
                    u64::try_from(projection.metrics.estimated_model_tokens).unwrap_or(u64::MAX),
                );
            stats.cumulative_bytes_saved = stats
                .cumulative_bytes_saved
                .saturating_add(raw_bytes.saturating_sub(model_bytes));
        }
        stats
    }
}

impl ChatProfileCapabilities {
    fn is_anthropic(&self) -> bool {
        matches!(
            self.provider_kind,
            Some(LLMProviderKind::Anthropic | LLMProviderKind::Minimax | LLMProviderKind::DeepSeek)
        )
    }

    fn is_gemini(&self) -> bool {
        matches!(self.provider_kind, Some(LLMProviderKind::Gemini))
    }

    fn is_openai_chat(&self) -> bool {
        matches!(
            (&self.provider_kind, self.openai_api_mode),
            (Some(LLMProviderKind::OpenAI), Some("chat"))
        )
    }

    /// OpenAI in Responses mode, and xAI — which speaks the same Responses
    /// protocol (input items, `previous_response_id` anchoring) and nothing
    /// else.
    fn is_openai_responses_family(&self) -> bool {
        matches!(
            (&self.provider_kind, self.openai_api_mode),
            (Some(LLMProviderKind::OpenAI), Some("responses" | "auto"))
                | (Some(LLMProviderKind::Xai), _)
        )
    }

    fn is_openrouter(&self) -> bool {
        matches!(self.provider_kind, Some(LLMProviderKind::OpenRouter))
    }

    fn supports_native_assistant_turn_replay(&self) -> bool {
        self.is_anthropic()
            || self.is_openai_chat()
            || self.is_openai_responses_family()
            || self.is_gemini()
            || self.is_openrouter()
    }

    fn supports_native_tool_result_replay(&self) -> bool {
        self.supports_native_assistant_turn_replay()
    }

    fn replay_protocol(&self) -> &'static str {
        if self.is_openai_responses_family() {
            "openai_responses"
        } else if self.is_openai_chat() {
            "openai_chat"
        } else if self.is_anthropic() {
            "anthropic_messages"
        } else if self.is_gemini() {
            "gemini"
        } else if self.is_openrouter() {
            "openrouter"
        } else {
            "flattened"
        }
    }
}

/// Assembles context and calls the LLM for chat completions.
#[derive(Clone)]
pub struct ChatLlmService {
    llm_service: Arc<MultiLLMService>,
    workspace_layout: Option<ArtifactV2Workspace>,
}

#[async_trait]
pub trait ChatLlmClient: Send + Sync {
    /// Exact provider configuration for an alternate chat harness such as Pi.
    fn config_for_profile(&self, _profile_name: &str) -> Result<magicllm::config::LlmConfig> {
        anyhow::bail!("chat profile configuration is unavailable")
    }
    fn list_chat_profiles(&self) -> Vec<ChatProfileInfo>;
    fn list_chat_profile_warnings(&self) -> Vec<ChatProfileWarning>;

    /// Look up the (fast, thinking) standard-profile pair behind an
    /// adaptive composite, or `None` for standard / unknown names.
    /// Trait method so the chat-inline runtime can call this through
    /// the `dyn ChatLlmClient` it owns.
    fn adaptive_pair_for_profile(&self, profile_name: &str) -> Option<(String, String)>;

    /// Operation-mapped default profile name for the given operation
    /// key. Used by the chat runtime to detect adaptive composites
    /// when no explicit `profile_override` was supplied.
    fn default_profile_for_operation(&self, operation_key: &str) -> Option<String>;

    /// The LOCAL arm regardless of locality mode — for contracts pinned to
    /// the on-device profile (LocalOnly app-memory credential).
    fn local_default_profile_for_operation(&self, operation_key: &str) -> Option<String>;

    /// Best-effort provider/model/profile hint for a chat completion before
    /// the provider returns usage. Used to make failed chat-inline rows in
    /// `/llm` filterable by the profile that was attempted.
    fn telemetry_hint_for_profile(
        &self,
        profile_override: Option<&str>,
    ) -> Option<ChatCompletionTelemetryHint>;

    async fn generate_response(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<ChatLlmResponse>;

    async fn generate_response_with_tools(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<ChatLlmResponse>;

    /// Streaming variant of `generate_response_with_tools`. Returns the
    /// same `ChatLlmResponse` shape but drains the LLM stream through a
    /// caller-supplied `on_delta` callback so each `StreamDelta` lands
    /// in real time. Used by `process_chat_inline_turn` to surface
    /// block-level reasoning to the broadcaster as the LLM call
    /// progresses, instead of arriving as one batched payload at the
    /// end (the non-streaming path).
    ///
    /// The callback runs on the internal consumer task, *not* on the
    /// caller's task; pass a `move` closure that captures only
    /// `Send + Sync` state.
    async fn generate_response_with_tools_streaming(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
        disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
        on_delta: Box<dyn Fn(StreamDelta) + Send + Sync>,
    ) -> Result<ChatLlmResponse>;

    async fn generate_response_streaming(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<()>;
}

impl ChatLlmService {
    fn transcript_blocks_to_text(blocks: &[TranscriptBlock]) -> String {
        let mut rendered = Vec::new();
        for block in blocks {
            match block {
                TranscriptBlock::Text { text } if !text.trim().is_empty() => {
                    rendered.push(text.clone())
                },
                TranscriptBlock::Text { .. } => {},
                TranscriptBlock::ImageFile { image } => {
                    let label = image.label.as_deref().unwrap_or(&image.stored_name);
                    rendered.push(format!("[image: {} ({})]", label, image.mime_type));
                },
            }
        }
        rendered.join("\n\n")
    }

    /// Create a new `ChatLlmService` backed by the given LLM service.
    pub fn new(llm_service: Arc<MultiLLMService>) -> Self {
        Self {
            llm_service,
            workspace_layout: None,
        }
    }

    pub fn with_workspace_layout(mut self, workspace_layout: ArtifactV2Workspace) -> Self {
        self.workspace_layout = Some(workspace_layout);
        self
    }

    /// List all chat-eligible LLM profiles.
    pub fn list_chat_profiles(&self) -> Vec<ChatProfileInfo> {
        self.llm_service.list_chat_profiles()
    }

    /// Look up the (fast, thinking) pair for an adaptive composite
    /// profile. Returns `None` for standard profiles or unknown names.
    /// Used by `process_chat_inline_turn` to detect that the effective
    /// profile is adaptive and to swap profile names on escalation.
    pub fn adaptive_pair_for_profile(&self, profile_name: &str) -> Option<(String, String)> {
        self.llm_service.adaptive_pair(profile_name)
    }

    /// Operation-mapped default profile name for the given operation
    /// key. `process_chat_inline_turn` calls this with `"chat_completion"`
    /// when the caller passed no `profile_override`, so adaptive
    /// composites configured as the default still trip the
    /// `adaptive_pair_for_profile` check.
    pub fn default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        self.llm_service
            .default_profile_for_operation(operation_key)
    }

    /// The LOCAL arm regardless of locality mode — for contracts pinned to
    /// the on-device profile (LocalOnly app-memory credential).
    pub fn local_default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        self.llm_service
            .local_default_profile_for_operation(operation_key)
    }

    fn profile_capabilities(
        &self,
        _history: &[ChatLlmTranscriptEntry],
        profile_override: Option<&str>,
    ) -> ChatProfileCapabilities {
        let config = profile_override
            .and_then(|profile| self.llm_service.get_config_by_profile_name(profile).ok())
            .or_else(|| {
                self.llm_service
                    .get_config_for_operation(&LLMOperation::ChatCompletion)
                    .ok()
            });
        let Some(config) = config else {
            return ChatProfileCapabilities::default();
        };

        let supports_vision = config
            .supports_vision
            .unwrap_or_else(|| match &config.provider {
                LLMProviderKind::Anthropic => true,
                LLMProviderKind::Minimax => false,
                LLMProviderKind::DeepSeek => false,
                LLMProviderKind::OpenAI => {
                    let mode = config
                        .additional_params
                        .as_ref()
                        .and_then(|params| params.get("openai_api_mode"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("responses");
                    matches!(mode, "responses" | "auto")
                },
                LLMProviderKind::Xai => magicllm::XaiProvider::model_supports_vision(&config.model),
                _ => false,
            });

        match &config.provider {
            LLMProviderKind::Anthropic => ChatProfileCapabilities {
                provider_kind: Some(LLMProviderKind::Anthropic),
                openai_api_mode: None,
                supports_user_image_inputs: supports_vision,
                supports_multimodal_tool_result_replay: true,
            },
            LLMProviderKind::Minimax => ChatProfileCapabilities {
                provider_kind: Some(LLMProviderKind::Minimax),
                openai_api_mode: None,
                supports_user_image_inputs: supports_vision,
                supports_multimodal_tool_result_replay: false,
            },
            LLMProviderKind::DeepSeek => ChatProfileCapabilities {
                provider_kind: Some(LLMProviderKind::DeepSeek),
                openai_api_mode: None,
                supports_user_image_inputs: supports_vision,
                supports_multimodal_tool_result_replay: false,
            },
            LLMProviderKind::OpenAI => {
                let mode = config
                    .additional_params
                    .as_ref()
                    .and_then(|params| params.get("openai_api_mode"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("responses");
                ChatProfileCapabilities {
                    provider_kind: Some(LLMProviderKind::OpenAI),
                    openai_api_mode: Some(match mode {
                        "chat" => "chat",
                        "auto" => "auto",
                        _ => "responses",
                    }),
                    supports_user_image_inputs: supports_vision
                        && matches!(mode, "responses" | "auto"),
                    supports_multimodal_tool_result_replay: false,
                }
            },
            LLMProviderKind::Gemini => ChatProfileCapabilities {
                provider_kind: Some(LLMProviderKind::Gemini),
                openai_api_mode: None,
                supports_user_image_inputs: supports_vision,
                supports_multimodal_tool_result_replay: true,
            },
            LLMProviderKind::Xai => ChatProfileCapabilities {
                provider_kind: Some(LLMProviderKind::Xai),
                openai_api_mode: Some("responses"),
                supports_user_image_inputs: supports_vision,
                supports_multimodal_tool_result_replay: false,
            },
            provider_kind => ChatProfileCapabilities {
                provider_kind: Some(provider_kind.clone()),
                ..Default::default()
            },
        }
    }

    async fn load_image_block(
        &self,
        transcript_context: Option<&ChatTranscriptContext>,
        block: &TranscriptBlock,
    ) -> Option<RouterContentBlock> {
        let TranscriptBlock::ImageFile { image } = block else {
            return None;
        };
        let ctx = transcript_context?;
        let workspace = self.workspace_layout.as_ref()?;
        let path = workspace
            .chat_session_outputs_dir(&ctx.principal, &ctx.workspace, &ctx.session_id)
            .join(&image.stored_name);
        let bytes = workspace.read_path(&path).await.ok()?;
        Some(RouterContentBlock::Image {
            data: bytes,
            media_type: image.mime_type.clone(),
            caption: image.label.clone(),
        })
    }

    async fn render_user_content_blocks(
        &self,
        blocks: &[TranscriptBlock],
        transcript_context: Option<&ChatTranscriptContext>,
        capabilities: &ChatProfileCapabilities,
    ) -> Vec<RouterContentBlock> {
        if !capabilities.supports_user_image_inputs {
            let rendered = Self::transcript_blocks_to_text(blocks);
            return if rendered.is_empty() {
                Vec::new()
            } else {
                vec![RouterContentBlock::Text { text: rendered }]
            };
        }

        let mut content = Vec::new();
        for block in blocks {
            match block {
                TranscriptBlock::Text { text } if !text.trim().is_empty() => {
                    content.push(RouterContentBlock::Text { text: text.clone() });
                },
                TranscriptBlock::Text { .. } => {},
                TranscriptBlock::ImageFile { .. } => {
                    if let Some(image_block) =
                        self.load_image_block(transcript_context, block).await
                    {
                        content.push(image_block);
                    } else {
                        let fallback = Self::transcript_blocks_to_text(std::slice::from_ref(block));
                        if !fallback.is_empty() {
                            content.push(RouterContentBlock::Text { text: fallback });
                        }
                    }
                },
            }
        }
        content
    }

    fn recent_session_file_reference_text(
        file: &ChatSessionFileRecord,
        absolute_path: &std::path::Path,
    ) -> String {
        let label = file
            .label
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&file.original_name);
        let origin = match &file.origin {
            ChatSessionFileOrigin::Attachment => "attachment".to_string(),
            ChatSessionFileOrigin::ToolOutput { tool_name, .. } => {
                format!("tool_output:{tool_name}")
            },
        };
        format!(
            "- {label} [{origin}; {}; {} bytes] path: {}",
            file.mime_type,
            file.size,
            absolute_path.display()
        )
    }

    async fn load_recent_session_file_content(
        &self,
        transcript_context: Option<&ChatTranscriptContext>,
        capabilities: ChatProfileCapabilities,
        excluded_tool_call_ids: &HashSet<&str>,
    ) -> Option<Vec<RouterContentBlock>> {
        let ctx = transcript_context?;
        let workspace = self.workspace_layout.as_ref()?;
        let index_path =
            workspace.chat_session_file_index_path(&ctx.principal, &ctx.workspace, &ctx.session_id);
        let mut index: ChatSessionFileIndex = workspace.read_json_path(&index_path).await.ok()?;
        index.files.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.stored_name.cmp(&right.stored_name))
        });
        let recent_files = index
            .files
            .into_iter()
            .filter(|file| match &file.origin {
                ChatSessionFileOrigin::ToolOutput {
                    tool_call_id: Some(tool_call_id),
                    ..
                } => !excluded_tool_call_ids.contains(tool_call_id.as_str()),
                ChatSessionFileOrigin::ToolOutput { .. } => true,
                ChatSessionFileOrigin::Attachment => false,
            })
            .take(MAX_RECENT_TOOL_OUTPUT_REFERENCES)
            .collect::<Vec<_>>();
        if recent_files.is_empty() {
            return None;
        }

        let outputs_dir =
            workspace.chat_session_outputs_dir(&ctx.principal, &ctx.workspace, &ctx.session_id);
        let references = recent_files
            .iter()
            .map(|file| {
                let absolute_path = outputs_dir.join(&file.stored_name);
                Self::recent_session_file_reference_text(file, &absolute_path)
            })
            .collect::<Vec<_>>();

        let mut message_content = vec![RouterContentBlock::Text {
            text: format!(
                "Recent session files from earlier tool results. Use these `path:` values when a tool needs an existing file from this chat:\n{}",
                references.join("\n")
            ),
        }];

        if capabilities.supports_user_image_inputs {
            for file in recent_files
                .iter()
                .filter(|file| file.prompt_image && file.mime_type.starts_with("image/"))
                .take(MAX_RECENT_TOOL_OUTPUT_IMAGES)
            {
                let absolute_path = outputs_dir.join(&file.stored_name);
                if let Ok(bytes) = workspace.read_path(&absolute_path).await {
                    let label = file
                        .label
                        .clone()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| file.original_name.clone());
                    message_content.push(RouterContentBlock::Image {
                        data: bytes,
                        media_type: file.mime_type.clone(),
                        caption: Some(format!("{label} | path: {}", absolute_path.display())),
                    });
                }
            }
        }

        Some(message_content)
    }

    async fn load_immediate_tool_output_content(
        &self,
        history: &[ProviderHistoryEntryView<'_>],
        start_after_index: Option<usize>,
        neutralized_call_ids: &HashSet<&str>,
        transcript_context: Option<&ChatTranscriptContext>,
        capabilities: ChatProfileCapabilities,
    ) -> Option<Vec<RouterContentBlock>> {
        if !capabilities.supports_user_image_inputs
            || capabilities.supports_multimodal_tool_result_replay
        {
            return None;
        }

        let ctx = transcript_context?;
        let workspace = self.workspace_layout.as_ref()?;
        let outputs_dir =
            workspace.chat_session_outputs_dir(&ctx.principal, &ctx.workspace, &ctx.session_id);

        let mut seen_stored_names = HashSet::new();
        let mut references = Vec::new();
        let mut image_blocks = Vec::new();
        let history_slice = start_after_index
            .map(|index| &history[(index + 1)..])
            .unwrap_or(history);

        for entry in history_slice {
            let ProviderHistoryEntryView::ToolResultRich {
                tool_call_id,
                tool_name,
                content,
                ..
            } = entry
            else {
                continue;
            };
            if neutralized_call_ids.contains(*tool_call_id) {
                continue;
            }

            for block in (*content).iter() {
                let TranscriptBlock::ImageFile { image } = block else {
                    continue;
                };
                if !image.mime_type.starts_with("image/") {
                    continue;
                }
                if !seen_stored_names.insert(image.stored_name.clone()) {
                    continue;
                }

                let absolute_path = outputs_dir.join(&image.stored_name);
                let label = image
                    .label
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| image.stored_name.clone());
                let origin = tool_name
                    .as_ref()
                    .copied()
                    .filter(|value| !value.trim().is_empty())
                    .map(|name| format!("tool_output:{name}"))
                    .unwrap_or_else(|| "tool_output".to_string());

                references.push(format!(
                    "- {label} [{origin}; {}] path: {}",
                    image.mime_type,
                    absolute_path.display()
                ));

                if image_blocks.len() < MAX_RECENT_TOOL_OUTPUT_IMAGES {
                    if let Ok(bytes) = workspace.read_path(&absolute_path).await {
                        image_blocks.push(RouterContentBlock::Image {
                            data: bytes,
                            media_type: image.mime_type.clone(),
                            caption: Some(format!("{label} | path: {}", absolute_path.display())),
                        });
                    }
                }
            }
        }

        if references.is_empty() {
            return None;
        }

        let mut message_content = vec![RouterContentBlock::Text {
            text: format!(
                "Immediate tool output images from the preceding tool results. Use these `path:` values if the next tool call needs a file generated in this same turn:\n{}",
                references.join("\n")
            ),
        }];
        message_content.extend(image_blocks);
        Some(message_content)
    }

    async fn build_tool_result_content_value(
        &self,
        content: &[TranscriptBlock],
        transcript_context: Option<&ChatTranscriptContext>,
        capabilities: ChatProfileCapabilities,
    ) -> serde_json::Value {
        if !capabilities.supports_multimodal_tool_result_replay {
            return serde_json::Value::String(Self::transcript_blocks_to_text(content));
        }

        let mut blocks = Vec::new();
        for block in content {
            match block {
                TranscriptBlock::Text { text } if !text.trim().is_empty() => {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text,
                    }));
                },
                TranscriptBlock::Text { .. } => {},
                TranscriptBlock::ImageFile { .. } => {
                    match self.load_image_block(transcript_context, block).await {
                        Some(RouterContentBlock::Image {
                            data,
                            media_type,
                            caption,
                        }) => {
                            blocks.push(serde_json::json!({
                                "type": "image",
                                "media_type": media_type,
                                "data_base64": BASE64.encode(data),
                                "caption": caption,
                            }));
                        },
                        _ => {
                            let fallback =
                                Self::transcript_blocks_to_text(std::slice::from_ref(block));
                            if !fallback.is_empty() {
                                blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": fallback,
                                }));
                            }
                        },
                    }
                },
            }
        }

        if blocks.is_empty() {
            serde_json::Value::String(String::new())
        } else {
            serde_json::json!({
                "_magicllm_rich_tool_result": true,
                "blocks": blocks,
            })
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn assistant_provider_state_from_raw_response_shape(
        raw_response: Option<&Value>,
    ) -> Option<AssistantProviderState> {
        let raw_response = raw_response?;
        if (raw_response.get("object").and_then(Value::as_str) == Some("response")
            || raw_response
                .get("output")
                .and_then(Value::as_array)
                .is_some())
            && raw_response.get("choices").is_none()
        {
            if let Some(response_id) = raw_response.get("id").and_then(Value::as_str) {
                return Some(AssistantProviderState::OpenaiResponses {
                    response_id: response_id.to_string(),
                    tool_protocol_repair_checkpoint: false,
                });
            }
        }

        if raw_response
            .get("candidates")
            .and_then(Value::as_array)
            .is_some()
        {
            let parts = raw_response
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|candidates| candidates.first())
                .and_then(|candidate| candidate.get("content"))
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
                .cloned()?;
            return Some(AssistantProviderState::Gemini { parts });
        }

        if let Some(content) = raw_response
            .get("content")
            .and_then(Value::as_array)
            .filter(|content| !content.is_empty())
        {
            return Some(AssistantProviderState::AnthropicMessages {
                content: content.clone(),
            });
        }

        None
    }

    /// Consuming counterpart used by live chat. The normalized response is no
    /// longer exposed after this boundary, so provider replay state can move
    /// its native array out instead of cloning the complete Gemini/Anthropic
    /// block list beside the raw response.
    fn assistant_provider_state_from_owned_raw_response_shape(
        raw_response: Option<Value>,
    ) -> Option<AssistantProviderState> {
        let mut raw_response = raw_response?;
        let is_openai_response = (raw_response.get("object").and_then(Value::as_str)
            == Some("response")
            || raw_response
                .get("output")
                .and_then(Value::as_array)
                .is_some())
            && raw_response.get("choices").is_none();
        if is_openai_response {
            let response_id = raw_response
                .as_object_mut()
                .and_then(|object| object.remove("id"));
            let response_id = match response_id {
                Some(Value::String(response_id)) => Some(response_id),
                Some(other) => {
                    crate::magician_v2::json_traversal::discard_json_iteratively(other);
                    None
                },
                None => None,
            };
            if let Some(response_id) = response_id {
                crate::magician_v2::json_traversal::discard_json_iteratively(raw_response);
                return Some(AssistantProviderState::OpenaiResponses {
                    response_id,
                    tool_protocol_repair_checkpoint: false,
                });
            }
        }

        let parts = raw_response
            .get_mut("candidates")
            .and_then(Value::as_array_mut)
            .and_then(|candidates| candidates.first_mut())
            .and_then(|candidate| candidate.get_mut("content"))
            .and_then(Value::as_object_mut)
            .and_then(|content| content.remove("parts"));
        let parts = match parts {
            Some(Value::Array(parts)) => Some(parts),
            Some(other) => {
                crate::magician_v2::json_traversal::discard_json_iteratively(other);
                None
            },
            None => None,
        };
        if let Some(parts) = parts {
            crate::magician_v2::json_traversal::discard_json_iteratively(raw_response);
            return Some(AssistantProviderState::Gemini { parts });
        }

        let content = raw_response
            .as_object_mut()
            .and_then(|object| object.remove("content"));
        let content = match content {
            Some(Value::Array(content)) if !content.is_empty() => Some(content),
            Some(Value::Array(_)) | None => None,
            Some(other) => {
                crate::magician_v2::json_traversal::discard_json_iteratively(other);
                None
            },
        };
        crate::magician_v2::json_traversal::discard_json_iteratively(raw_response);
        content.map(|content| AssistantProviderState::AnthropicMessages { content })
    }

    fn assistant_provider_state_from_owned_raw_response(
        raw_response: Option<Value>,
        capabilities: &ChatProfileCapabilities,
    ) -> Option<AssistantProviderState> {
        let state = Self::assistant_provider_state_from_owned_raw_response_shape(raw_response)?;
        let accepted = match (&state, &capabilities.provider_kind) {
            (
                AssistantProviderState::OpenaiResponses { .. },
                Some(LLMProviderKind::OpenAI | LLMProviderKind::Xai),
            ) => true,
            (AssistantProviderState::Gemini { .. }, Some(LLMProviderKind::Gemini)) => true,
            (
                AssistantProviderState::AnthropicMessages { .. },
                Some(
                    LLMProviderKind::Anthropic
                    | LLMProviderKind::Minimax
                    | LLMProviderKind::DeepSeek,
                ),
            ) => true,
            (AssistantProviderState::OpenaiResponses { .. }, None)
            | (AssistantProviderState::Gemini { .. }, None)
            | (AssistantProviderState::AnthropicMessages { .. }, None) => true,
            _ => false,
        };
        if accepted {
            Some(state)
        } else {
            match state {
                AssistantProviderState::Gemini { parts } => {
                    for value in parts {
                        crate::magician_v2::json_traversal::discard_json_iteratively(value);
                    }
                },
                AssistantProviderState::AnthropicMessages { content } => {
                    for value in content {
                        crate::magician_v2::json_traversal::discard_json_iteratively(value);
                    }
                },
                AssistantProviderState::OpenaiResponses { .. } => {},
            }
            None
        }
    }

    fn into_owned_shared_provider_response(raw_response: Arc<Value>) -> Value {
        Arc::try_unwrap(raw_response).unwrap_or_else(|shared| {
            crate::magician_v2::json_traversal::clone_json_iteratively(shared.as_ref())
        })
    }

    fn into_owned_shared_tool_calls(tool_calls: Arc<Vec<LLMToolCall>>) -> Vec<LLMToolCall> {
        Arc::try_unwrap(tool_calls).unwrap_or_else(|shared| {
            shared
                .iter()
                .map(|call| LLMToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: crate::magician_v2::json_traversal::clone_json_iteratively(
                        &call.arguments,
                    ),
                })
                .collect()
        })
    }

    fn anthropic_raw_content_message(content: &[Value]) -> Option<RouterMessage> {
        let mut blocks = Vec::new();

        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        blocks.push(RouterContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                },
                Some("thinking") | Some("redacted_thinking") => {
                    blocks.push(anthropic_raw_content_block(
                        crate::magician_v2::json_traversal::clone_json_iteratively(block),
                    ));
                },
                Some("tool_use") => {
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = block
                        .get("input")
                        .map(crate::magician_v2::json_traversal::clone_json_iteratively)
                        .unwrap_or(Value::Null);
                    blocks.push(RouterContentBlock::ToolCall {
                        id: id.to_string(),
                        name,
                        arguments,
                    });
                },
                _ => {},
            }
        }

        if blocks.is_empty() {
            None
        } else {
            Some(RouterMessage {
                role: MessageRole::Assistant,
                content: blocks,
            })
        }
    }

    fn openai_responses_anchor<'entry>(
        history: &[ProviderHistoryEntryView<'entry>],
    ) -> Option<(usize, &'entry str)> {
        for (index, entry) in history.iter().enumerate().rev() {
            let ProviderHistoryEntryView::AssistantTurn { provider_state, .. } = entry else {
                continue;
            };
            return match provider_state {
                Some(AssistantProviderState::OpenaiResponses { response_id, .. }) => {
                    Some((index, response_id.as_str()))
                },
                _ => None,
            };
        }
        None
    }

    fn mark_tool_protocol_repair_checkpoint(
        provider_state: &mut Option<AssistantProviderState>,
        checkpoint_required: bool,
        content: Option<&str>,
        tool_calls: &[LLMToolCall],
    ) {
        if !checkpoint_required
            || !tool_calls.is_empty()
            || !content.is_some_and(|value| !value.trim().is_empty())
        {
            return;
        }
        if let Some(AssistantProviderState::OpenaiResponses {
            tool_protocol_repair_checkpoint,
            ..
        }) = provider_state
        {
            *tool_protocol_repair_checkpoint = true;
        }
    }

    fn build_openai_responses_request_overrides(
        previous_response_id: Option<String>,
        include_tools: bool,
    ) -> Option<ChatCompletionRequestOverrides> {
        let mut extra = serde_json::Map::new();
        if previous_response_id.is_some() || include_tools {
            extra.insert(
                "openai_api_mode".to_string(),
                Value::String("responses".to_string()),
            );
        }
        if let Some(previous_response_id) = previous_response_id {
            extra.insert(
                "openai_previous_response_id".to_string(),
                Value::String(previous_response_id),
            );
        }
        if extra.is_empty() {
            None
        } else {
            Some(ChatCompletionRequestOverrides {
                extra: Some(Value::Object(extra)),
                trace_context: None,
                provider_attempt_counter: None,
                disclosure_guard: None,
            })
        }
    }

    fn attach_trace_context(
        built_request: &mut BuiltChatRequest,
        transcript_context: Option<&ChatTranscriptContext>,
    ) {
        let Some(context) = transcript_context else {
            return;
        };
        let mut trace = context.trace_context.clone().unwrap_or_else(|| {
            magicllm::LlmTraceContext::new(
                magicllm::LlmScope::new(&context.principal, &context.workspace),
                magicllm::LlmWorkloadClass::ForegroundChat,
            )
        });
        if context.trace_context.is_none() {
            if let Some(turn_id) = context
                .chat_turn_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                trace.trace_id = turn_id.to_string();
                trace.chat_turn_id = Some(turn_id.to_string());
            }
            trace.chat_session_id = Some(context.session_id.clone());
            trace.user_message_id = context.user_message_id.clone();
        }
        if trace.activity_id.is_none() {
            trace.set_activity_id(current_activity_id().map(|id| id.to_string()));
        }
        let overrides = built_request
            .request_overrides
            .get_or_insert_with(ChatCompletionRequestOverrides::default);
        overrides.trace_context = Some(trace);
        overrides.provider_attempt_counter = context.provider_attempt_counter.clone();
    }

    fn gemini_raw_parts_message(parts: &[Value]) -> RouterMessage {
        let parts = parts
            .iter()
            .map(crate::magician_v2::json_traversal::clone_json_iteratively)
            .collect();
        let mut value = serde_json::Map::new();
        value.insert(
            "_magicllm_gemini_raw_parts".to_string(),
            Value::Array(parts),
        );
        RouterMessage {
            role: MessageRole::Assistant,
            content: vec![RouterContentBlock::Json {
                value: Value::Object(value),
            }],
        }
    }

    fn flatten_assistant_turn<'text, 'call>(
        text: Option<Cow<'text, str>>,
        tool_calls: impl IntoIterator<Item = &'call StoredToolCall>,
    ) -> String {
        let mut rendered = match text {
            Some(Cow::Owned(mut text)) => {
                let trimmed_start = text.len().saturating_sub(text.trim_start().len());
                let trimmed_len = text.trim().len();
                text.truncate(trimmed_start.saturating_add(trimmed_len));
                if trimmed_start > 0 {
                    text.drain(..trimmed_start);
                }
                text.into_bytes()
            },
            Some(Cow::Borrowed(text)) => text.trim().as_bytes().to_vec(),
            None => Vec::new(),
        };
        for call in tool_calls {
            if !rendered.is_empty() {
                rendered.extend_from_slice(b"\n\n");
            }
            rendered.extend_from_slice(b"Called tool `");
            rendered.extend_from_slice(call.name.as_bytes());
            rendered.extend_from_slice(b"` with arguments:\n");
            // `serde_json::Value` serialization is infallible for its own
            // representation. Write directly into the final UTF-8 buffer so
            // large arguments are not first materialized as another String.
            serde_json::to_writer_pretty(&mut rendered, &call.arguments)
                .expect("serde_json::Value must serialize");
        }
        String::from_utf8(rendered).expect("provider narration is valid UTF-8")
    }

    fn flatten_tool_result(tool_name: Option<&str>, mut content: String) -> String {
        let trimmed_start = content.len().saturating_sub(content.trim_start().len());
        let trimmed_len = content.trim().len();
        content.truncate(trimmed_start.saturating_add(trimmed_len));
        if trimmed_start > 0 {
            content.drain(..trimmed_start);
        }
        match tool_name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) if content.is_empty() => format!("Tool `{name}` returned no text output."),
            Some(name) => {
                content.insert_str(0, &format!("Tool `{name}` result:\n"));
                content
            },
            None => content,
        }
    }

    fn push_assistant_text_message(messages: &mut Vec<RouterMessage>, text: String) {
        if text.trim().is_empty() {
            return;
        }

        if let Some(last_message) = messages.last_mut() {
            let last_is_text_only = last_message
                .content
                .iter()
                .all(|block| matches!(block, RouterContentBlock::Text { .. }));
            if last_message.role == MessageRole::Assistant && last_is_text_only {
                last_message.content.push(RouterContentBlock::Text { text });
                return;
            }
        }

        messages.push(RouterMessage {
            role: MessageRole::Assistant,
            content: vec![RouterContentBlock::Text { text }],
        });
    }

    fn push_user_content_message(
        messages: &mut Vec<RouterMessage>,
        content: Vec<RouterContentBlock>,
    ) {
        if content.is_empty() {
            return;
        }

        if let Some(last_message) = messages.last_mut() {
            if last_message.role == MessageRole::User {
                last_message.content.extend(content);
                return;
            }
        }

        messages.push(RouterMessage {
            role: MessageRole::User,
            content,
        });
    }

    fn history_entry_tool_result_call_id(entry: &ChatLlmTranscriptEntry) -> Option<&str> {
        match entry {
            ChatLlmTranscriptEntry::ToolResult { tool_call_id, .. }
            | ChatLlmTranscriptEntry::ToolResultRich { tool_call_id, .. }
            | ChatLlmTranscriptEntry::ToolResultProjected { tool_call_id, .. } => {
                Some(tool_call_id)
            },
            _ => None,
        }
    }

    fn interrupted_tool_note(text: Option<&str>, removed_tool_calls: usize) -> Option<String> {
        if removed_tool_calls == 0 {
            return text.map(ToOwned::to_owned);
        }
        let note = if removed_tool_calls == 1 {
            "[chat-inline] A prior tool request was interrupted before its result was recorded; no result is available for that request."
        } else {
            "[chat-inline] Prior tool requests were interrupted before their results were recorded; no results are available for those requests."
        };
        match text.map(str::trim).filter(|value| !value.is_empty()) {
            Some(text) => Some(format!("{text}\n\n{note}")),
            None => Some(note.to_string()),
        }
    }

    /// Normalize persisted tool-call history into provider-valid atomic
    /// call/result groups before continuation-anchor selection.
    ///
    /// Complete pairs retain their original order and typed payloads. Missing
    /// calls, duplicate call ids, and standalone/duplicate results are omitted
    /// from provider input only. Without a later trusted checkpoint, every
    /// provider-native assistant state from that turn onward is cleared: a
    /// later OpenAI `response_id` may have chained through the interrupted call
    /// server-side even when its local row looks healthy. Once a final response
    /// produced from repaired history is checkpointed, anchor selection skips
    /// the older poisoned range and its id and descendants are safe to reuse.
    fn borrowed_provider_history_entry(
        original_index: usize,
        entry: &ChatLlmTranscriptEntry,
    ) -> ProviderHistoryEntryView<'_> {
        match entry {
            ChatLlmTranscriptEntry::UserText { text } => ProviderHistoryEntryView::UserText {
                original_index,
                text,
            },
            ChatLlmTranscriptEntry::UserTurn { content } => ProviderHistoryEntryView::UserTurn {
                original_index,
                content,
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text,
                tool_calls,
                provider_state,
            } => ProviderHistoryEntryView::AssistantTurn {
                original_index,
                text: text.as_deref().map(Cow::Borrowed),
                tool_calls: ProviderToolCallsView::borrowed(tool_calls),
                provider_state: provider_state.as_ref(),
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id,
                tool_name,
                content,
            } => ProviderHistoryEntryView::ToolResult {
                original_index,
                tool_call_id,
                tool_name: tool_name.as_deref(),
                content,
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id,
                tool_name,
                content,
            } => ProviderHistoryEntryView::ToolResultRich {
                original_index,
                tool_call_id,
                tool_name: tool_name.as_deref(),
                content,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id,
                tool_name,
                projection,
            } => ProviderHistoryEntryView::ToolResultProjected {
                original_index,
                tool_call_id,
                tool_name: tool_name.as_deref(),
                projection,
            },
        }
    }

    fn repair_provider_tool_protocol_view(
        history: &[ChatLlmTranscriptEntry],
    ) -> (Vec<ProviderHistoryEntryView<'_>>, ToolProtocolRepairStats) {
        let mut repaired = Vec::with_capacity(history.len());
        let mut stats = ToolProtocolRepairStats {
            trusted_checkpoint_index: history.iter().rposition(|entry| match entry {
                ChatLlmTranscriptEntry::AssistantTurn {
                    text: Some(text),
                    tool_calls,
                    provider_state:
                        Some(AssistantProviderState::OpenaiResponses {
                            response_id,
                            tool_protocol_repair_checkpoint: true,
                        }),
                } => {
                    !text.trim().is_empty()
                        && tool_calls.is_empty()
                        && !response_id.trim().is_empty()
                },
                _ => false,
            }),
            ..Default::default()
        };
        let mut index = 0usize;

        while index < history.len() {
            match &history[index] {
                ChatLlmTranscriptEntry::AssistantTurn {
                    text,
                    tool_calls,
                    provider_state,
                } if !tool_calls.is_empty() => {
                    let mut unique_call_ids = HashSet::with_capacity(tool_calls.len());
                    let mut unique_calls = Vec::with_capacity(tool_calls.len());
                    for call in tool_calls {
                        if call.id.trim().is_empty() || !unique_call_ids.insert(call.id.as_str()) {
                            stats.duplicate_tool_call_ids =
                                stats.duplicate_tool_call_ids.saturating_add(1);
                            stats.removed_tool_calls = stats.removed_tool_calls.saturating_add(1);
                            stats.mark_corruption(index);
                            continue;
                        }
                        unique_calls.push(call);
                    }

                    let mut result_index = index + 1;
                    let mut matched_result_ids = HashSet::with_capacity(unique_calls.len());
                    let mut matched_results = Vec::with_capacity(unique_calls.len());
                    while result_index < history.len() {
                        let Some(tool_call_id) =
                            Self::history_entry_tool_result_call_id(&history[result_index])
                        else {
                            break;
                        };
                        if unique_call_ids.contains(tool_call_id)
                            && matched_result_ids.insert(tool_call_id)
                        {
                            matched_results.push((result_index, &history[result_index]));
                        } else {
                            stats.removed_tool_results =
                                stats.removed_tool_results.saturating_add(1);
                            stats.mark_corruption(result_index);
                        }
                        result_index += 1;
                    }

                    let kept_calls = unique_calls
                        .into_iter()
                        .filter(|call| matched_result_ids.contains(call.id.as_str()))
                        .collect::<Vec<_>>();
                    let removed_here = tool_calls.len().saturating_sub(kept_calls.len());
                    if removed_here > 0 {
                        // Duplicate/blank calls were already counted above.
                        let already_counted =
                            tool_calls.len().saturating_sub(unique_call_ids.len());
                        stats.removed_tool_calls = stats
                            .removed_tool_calls
                            .saturating_add(removed_here.saturating_sub(already_counted) as u64);
                        stats.mark_corruption(index);
                    }

                    repaired.push(ProviderHistoryEntryView::AssistantTurn {
                        original_index: index,
                        text: if removed_here > 0 {
                            Self::interrupted_tool_note(text.as_deref(), removed_here)
                                .map(Cow::Owned)
                        } else {
                            text.as_deref().map(Cow::Borrowed)
                        },
                        tool_calls: if removed_here > 0 {
                            ProviderToolCallsView::filtered(kept_calls)
                        } else {
                            ProviderToolCallsView::borrowed(tool_calls)
                        },
                        provider_state: if removed_here > 0 {
                            None
                        } else {
                            provider_state.as_ref()
                        },
                    });
                    repaired.extend(matched_results.into_iter().map(|(original_index, entry)| {
                        Self::borrowed_provider_history_entry(original_index, entry)
                    }));
                    index = result_index;
                },
                entry if Self::history_entry_tool_result_call_id(entry).is_some() => {
                    // Every valid result is consumed with the immediately
                    // preceding assistant call group above. Anything reaching
                    // this branch is standalone and cannot be sent natively.
                    stats.removed_tool_results = stats.removed_tool_results.saturating_add(1);
                    stats.mark_corruption(index);
                    index += 1;
                },
                entry => {
                    repaired.push(Self::borrowed_provider_history_entry(index, entry));
                    index += 1;
                },
            }
        }

        if let Some(first_corruption_index) = stats.first_uncheckpointed_corruption_index {
            for entry in &mut repaired {
                if entry.original_index() < first_corruption_index {
                    continue;
                }
                if let ProviderHistoryEntryView::AssistantTurn { provider_state, .. } = entry {
                    *provider_state = None;
                }
            }
        }

        (repaired, stats)
    }

    /// A transcript entry and its persisted projection both carry the call id
    /// so storage/debug views remain self-describing. Never replay projected
    /// evidence under a different outer call id: that could balance the
    /// provider protocol while attaching another call's evidence. Legacy or
    /// corrupt entries instead receive one small, typed failure result paired
    /// to the outer call id.
    fn projected_tool_result_for_replay(
        tool_call_id: &str,
        projection: &crate::magician_v2::tool_result_projection::ProjectedToolResultV1,
    ) -> Value {
        if projection.validate_schema_version().is_ok()
            && projection.identity.tool_call_id == tool_call_id
        {
            return crate::magician_v2::tool_result_projection::provider_safe_model_value(
                projection,
            );
        }
        json!({
            "schema_version": crate::magician_v2::tool_result_projection::TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
            "outcome": {
                "status": "failed",
                "code": "invalid_projected_tool_result_transcript",
                "retryable": false,
            },
            "data": null,
            "projection": {
                "complete": false,
                "complete_units_only": true,
                "included_records": 0,
                "omitted_records": 0,
                "omitted_fields": 1,
            },
        })
    }

    /// Task-spawning tools whose PRIOR-turn calls are replayed as a plain note
    /// rather than a live tool-call block. Kept in lockstep with the
    /// dispatch-layer duplicate-task guard's covered set
    /// (`duplicate_guard_goal`) — plus `create_monitor`, which spawns a
    /// persistent scheduled task (recurring spend) and must never be
    /// re-fired from a stale transcript template; its own preview-fingerprint
    /// gate covers dispatch, this covers the replay temptation. The chat
    /// harness's cold replay applies the same set.
    pub(crate) fn is_stale_replay_neutralized_tool(name: &str) -> bool {
        matches!(
            name,
            "orchestrate_pipeline" | "create_task" | "create_monitor"
        )
    }

    /// Build the replacement note for a neutralized prior task spawn — the
    /// continuity fact, minus the re-issuable arguments.
    fn stale_task_spawn_note(call: &StoredToolCall) -> String {
        let what = match call.name.as_str() {
            "orchestrate_pipeline" => call.arguments.get("goal").and_then(Value::as_str),
            "create_task" => call
                .arguments
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| call.arguments.get("title").and_then(Value::as_str)),
            "create_monitor" => call
                .arguments
                .get("objective")
                .and_then(Value::as_str)
                .or_else(|| call.arguments.get("title").and_then(Value::as_str)),
            _ => None,
        }
        .unwrap_or("a task")
        .trim();
        let mut summary: String = what.chars().take(180).collect();
        if what.chars().count() > 180 {
            summary.push('…');
        }
        format!(
            "[Already handled earlier in this conversation: `{name}` for \"{summary}\". \
This is done or in progress — do NOT call `{name}` again for it. Only re-run if the user \
explicitly asks for a fresh one.]",
            name = call.name,
            summary = summary,
        )
    }

    /// Defense-in-depth companion to the dispatch-layer duplicate-task guard.
    /// Rewrites the replayed transcript so task-spawning tool calls from PRIOR
    /// turns (strictly before the current user message) are not presented to
    /// the model as re-issuable `tool_call` blocks — the verbatim template the
    /// model was observed copying to re-fire an old `orchestrate_pipeline` on an
    /// unrelated turn. Each such call collapses into a short factual note on its
    /// assistant turn and its paired tool result is dropped, preserving the
    /// continuity fact without the template. The in-flight turn's calls (after
    /// the last user message) are left intact so the model can still react to
    /// their results; the call/result pair is always dropped TOGETHER so no
    /// orphaned tool result reaches the provider. `provider_state` is cleared on
    /// a rewritten turn so the renderer emits the rewritten text/tool-calls
    /// rather than the original raw tool_use blocks.
    ///
    /// Only affects providers that replay rendered messages; under native
    /// server-side continuation (OpenAI `previous_response_id`) prior turns live
    /// on the provider and are untouched — the dispatch-layer guard is the
    /// backstop there.
    fn neutralized_stale_task_call_ids<'entry>(
        history: &[ProviderHistoryEntryView<'entry>],
    ) -> HashSet<&'entry str> {
        let boundary = history
            .iter()
            .rposition(ProviderHistoryEntryView::is_user)
            .unwrap_or(0);
        let mut neutralized_call_ids = HashSet::new();
        for (index, entry) in history.iter().enumerate() {
            if index >= boundary {
                continue;
            }
            if let ProviderHistoryEntryView::AssistantTurn { tool_calls, .. } = entry {
                for call in tool_calls.iter() {
                    if Self::is_stale_replay_neutralized_tool(&call.name) {
                        neutralized_call_ids.insert(call.id.as_str());
                    }
                }
            }
        }
        neutralized_call_ids
    }

    fn retained_tool_calls<'view, 'entry: 'view>(
        tool_calls: &'view ProviderToolCallsView<'entry>,
        neutralized_call_ids: &'view HashSet<&'entry str>,
    ) -> impl Iterator<Item = &'entry StoredToolCall> + 'view {
        tool_calls
            .iter()
            .filter(|call| !neutralized_call_ids.contains(call.id.as_str()))
    }

    /// Generate a chat response given a system prompt, conversation history,
    /// and the current user message.
    ///
    /// Phase 1 path: no tools. Returns text content only.
    async fn build_request(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> BuiltChatRequest {
        let capabilities = self.profile_capabilities(history, profile_override);
        let (protocol_safe_history, protocol_repair) =
            Self::repair_provider_tool_protocol_view(history);
        if protocol_repair.repaired() {
            tracing::warn!(
                target: "magician::metrics::chat_tool_protocol_repair",
                provider = ?capabilities.provider_kind,
                first_corruption_index = protocol_repair.first_corruption_index,
                latest_corruption_index = protocol_repair.latest_corruption_index,
                trusted_checkpoint_index = protocol_repair.trusted_checkpoint_index,
                checkpoint_required = protocol_repair.checkpoint_required(),
                removed_tool_calls = protocol_repair.removed_tool_calls,
                removed_tool_results = protocol_repair.removed_tool_results,
                duplicate_tool_call_ids = protocol_repair.duplicate_tool_call_ids,
                "repaired interrupted chat tool history before provider replay"
            );
        }
        let history = protocol_safe_history.as_slice();
        let (history_to_render, prepend_system, request_overrides) =
            if capabilities.is_openai_responses_family() {
                if let Some((anchor_index, response_id)) = Self::openai_responses_anchor(history) {
                    (
                        &history[(anchor_index + 1)..],
                        true,
                        Self::build_openai_responses_request_overrides(
                            Some(response_id.to_string()),
                            false,
                        ),
                    )
                } else {
                    (history, true, None)
                }
            } else {
                (history, true, None)
            };

        // Defense-in-depth (paired with the dispatch-layer duplicate-task
        // guard): stale task calls are filtered while constructing the final
        // provider messages. The borrowed projection avoids materializing a
        // second complete transcript solely to apply that rewrite.
        let neutralized_call_ids = Self::neutralized_stale_task_call_ids(history_to_render);
        let projection_replay = ToolResultProjectionReplayStats::from_provider_view(
            history_to_render,
            &neutralized_call_ids,
        );
        if projection_replay.replay_count > 0 || projection_replay.invalid_projection_count > 0 {
            tracing::info!(
                target: "magician::metrics::tool_result_projection_replay",
                provider = ?capabilities.provider_kind,
                native_replay = capabilities.supports_native_tool_result_replay(),
                replay_count = projection_replay.replay_count,
                invalid_projection_count = projection_replay.invalid_projection_count,
                cumulative_raw_bytes = projection_replay.cumulative_raw_bytes,
                cumulative_model_bytes = projection_replay.cumulative_model_bytes,
                cumulative_estimated_model_tokens = projection_replay.cumulative_estimated_model_tokens,
                cumulative_bytes_saved = projection_replay.cumulative_bytes_saved,
                "tool_result_projection_provider_replay"
            );
        }

        let mut messages =
            Vec::with_capacity(history_to_render.len() + if prepend_system { 1 } else { 0 });
        if prepend_system {
            messages.push(RouterMessage::system(system_prompt));
        }

        let native_openai_continuation_active = request_overrides
            .as_ref()
            .and_then(|overrides| overrides.extra.as_ref())
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .and_then(Value::as_str)
            .is_some();

        let last_user_history_index = history_to_render
            .iter()
            .rposition(ProviderHistoryEntryView::is_user);
        let excluded_recent_tool_call_ids = last_user_history_index
            .map(|index| {
                history_to_render
                    .iter()
                    .skip(index + 1)
                    .filter_map(ProviderHistoryEntryView::tool_result_call_id)
                    .filter(|call_id| !neutralized_call_ids.contains(*call_id))
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let mut recent_session_file_content = match last_user_history_index {
            Some(_) => {
                self.load_recent_session_file_content(
                    transcript_context,
                    capabilities.clone(),
                    &excluded_recent_tool_call_ids,
                )
                .await
            },
            None => None,
        };
        let immediate_tool_output_content = self
            .load_immediate_tool_output_content(
                history_to_render,
                if native_openai_continuation_active {
                    None
                } else {
                    last_user_history_index
                },
                &neutralized_call_ids,
                transcript_context,
                capabilities.clone(),
            )
            .await;
        let mut non_native_replay_call_ids: HashSet<&str> = HashSet::new();

        for (index, entry) in history_to_render.iter().enumerate() {
            match entry {
                ProviderHistoryEntryView::UserText { text, .. } => {
                    let mut content = vec![RouterContentBlock::Text {
                        text: (*text).to_string(),
                    }];
                    if Some(index) == last_user_history_index {
                        if let Some(extra_content) = recent_session_file_content.take() {
                            content.extend(extra_content);
                        }
                    }
                    Self::push_user_content_message(&mut messages, content);
                },
                ProviderHistoryEntryView::UserTurn { content, .. } => {
                    let mut rendered = self
                        .render_user_content_blocks(content, transcript_context, &capabilities)
                        .await;
                    if Some(index) == last_user_history_index {
                        if let Some(extra_content) = recent_session_file_content.take() {
                            rendered.extend(extra_content);
                        }
                    }
                    if !rendered.is_empty() {
                        Self::push_user_content_message(&mut messages, rendered);
                    }
                },
                ProviderHistoryEntryView::AssistantTurn {
                    text,
                    tool_calls,
                    provider_state,
                    ..
                } => {
                    let neutralized_in_turn = tool_calls
                        .iter()
                        .any(|call| neutralized_call_ids.contains(call.id.as_str()));
                    let mut rendered_text = if neutralized_in_turn {
                        let mut rendered = text.as_deref().unwrap_or_default().to_string();
                        for call in tool_calls
                            .iter()
                            .filter(|call| neutralized_call_ids.contains(call.id.as_str()))
                        {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(&Self::stale_task_spawn_note(call));
                        }
                        Some(Cow::Owned(rendered))
                    } else {
                        text.as_deref().map(Cow::Borrowed)
                    };
                    let provider_state = if neutralized_in_turn {
                        None
                    } else {
                        *provider_state
                    };
                    if capabilities.is_gemini() {
                        if let Some(AssistantProviderState::Gemini { parts }) = provider_state {
                            messages.push(Self::gemini_raw_parts_message(parts));
                        } else {
                            non_native_replay_call_ids.extend(
                                Self::retained_tool_calls(tool_calls, &neutralized_call_ids)
                                    .map(|call| call.id.as_str()),
                            );
                            let flattened = Self::flatten_assistant_turn(
                                rendered_text.take(),
                                Self::retained_tool_calls(tool_calls, &neutralized_call_ids),
                            );
                            Self::push_assistant_text_message(&mut messages, flattened);
                        }
                    } else if capabilities.is_anthropic() {
                        if let Some(AssistantProviderState::AnthropicMessages { content }) =
                            provider_state
                        {
                            if let Some(message) = Self::anthropic_raw_content_message(content) {
                                messages.push(message);
                            }
                        } else {
                            let mut content = Vec::new();
                            if let Some(text) = rendered_text
                                .take()
                                .filter(|text| !text.is_empty())
                                .map(Cow::into_owned)
                            {
                                content.push(RouterContentBlock::Text { text });
                            }
                            content.extend(
                                Self::retained_tool_calls(tool_calls, &neutralized_call_ids).map(
                                    |call| RouterContentBlock::ToolCall {
                                        id: call.id.clone(),
                                        name: call.name.clone(),
                                        arguments: crate::magician_v2::json_traversal::clone_json_iteratively(
                                            &call.arguments,
                                        ),
                                    },
                                ),
                            );
                            if !content.is_empty() {
                                messages.push(RouterMessage {
                                    role: MessageRole::Assistant,
                                    content,
                                });
                            }
                        }
                    } else if capabilities.supports_native_assistant_turn_replay() {
                        let mut content = Vec::new();
                        if let Some(text) = rendered_text
                            .take()
                            .filter(|text| !text.is_empty())
                            .map(Cow::into_owned)
                        {
                            content.push(RouterContentBlock::Text { text });
                        }
                        content.extend(
                            Self::retained_tool_calls(tool_calls, &neutralized_call_ids).map(
                                |call| RouterContentBlock::ToolCall {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    arguments:
                                        crate::magician_v2::json_traversal::clone_json_iteratively(
                                            &call.arguments,
                                        ),
                                },
                            ),
                        );
                        if !content.is_empty() {
                            messages.push(RouterMessage {
                                role: MessageRole::Assistant,
                                content,
                            });
                        }
                    } else {
                        let flattened = Self::flatten_assistant_turn(
                            rendered_text.take(),
                            Self::retained_tool_calls(tool_calls, &neutralized_call_ids),
                        );
                        Self::push_assistant_text_message(&mut messages, flattened);
                    }
                },
                ProviderHistoryEntryView::ToolResult {
                    tool_call_id,
                    tool_name,
                    content,
                    ..
                } => {
                    if neutralized_call_ids.contains(*tool_call_id) {
                        continue;
                    }
                    let safe_content =
                        crate::magician_v2::secrets::sanitize_text_for_provider(content);
                    if (capabilities.supports_native_tool_result_replay()
                        && !non_native_replay_call_ids.contains(*tool_call_id))
                        || native_openai_continuation_active
                    {
                        messages.push(RouterMessage {
                            role: MessageRole::Tool,
                            content: vec![RouterContentBlock::ToolResult {
                                tool_call_id: (*tool_call_id).to_string(),
                                content: serde_json::Value::String(safe_content),
                            }],
                        });
                    } else {
                        let flattened =
                            Self::flatten_tool_result(tool_name.as_ref().copied(), safe_content);
                        Self::push_assistant_text_message(&mut messages, flattened);
                    }
                },
                ProviderHistoryEntryView::ToolResultRich {
                    tool_call_id,
                    tool_name,
                    content,
                    ..
                } => {
                    if neutralized_call_ids.contains(*tool_call_id) {
                        continue;
                    }
                    if (capabilities.supports_native_tool_result_replay()
                        && !non_native_replay_call_ids.contains(*tool_call_id))
                        || native_openai_continuation_active
                    {
                        let tool_result_content = self
                            .build_tool_result_content_value(
                                content,
                                transcript_context,
                                capabilities.clone(),
                            )
                            .await;
                        let sanitized_tool_result_content =
                            crate::magician_v2::secrets::sanitize_json_for_provider_owned(
                                tool_result_content,
                            );
                        messages.push(RouterMessage {
                            role: MessageRole::Tool,
                            content: vec![RouterContentBlock::ToolResult {
                                tool_call_id: (*tool_call_id).to_string(),
                                content: sanitized_tool_result_content,
                            }],
                        });
                    } else {
                        let rendered = crate::magician_v2::secrets::sanitize_text_for_provider(
                            &Self::transcript_blocks_to_text(content),
                        );
                        let flattened =
                            Self::flatten_tool_result(tool_name.as_ref().copied(), rendered);
                        Self::push_assistant_text_message(&mut messages, flattened);
                    }
                },
                ProviderHistoryEntryView::ToolResultProjected {
                    tool_call_id,
                    tool_name,
                    projection,
                    ..
                } => {
                    if neutralized_call_ids.contains(*tool_call_id) {
                        continue;
                    }
                    let model_value =
                        Self::projected_tool_result_for_replay(tool_call_id, projection);
                    if (capabilities.supports_native_tool_result_replay()
                        && !non_native_replay_call_ids.contains(*tool_call_id))
                        || native_openai_continuation_active
                    {
                        messages.push(RouterMessage {
                            role: MessageRole::Tool,
                            content: vec![RouterContentBlock::ToolResult {
                                tool_call_id: (*tool_call_id).to_string(),
                                content: model_value,
                            }],
                        });
                    } else {
                        let rendered = serde_json::to_string(&model_value)
                            .unwrap_or_else(|_| "tool result projection unavailable".to_string());
                        let flattened =
                            Self::flatten_tool_result(tool_name.as_ref().copied(), rendered);
                        Self::push_assistant_text_message(&mut messages, flattened);
                    }
                },
            }
        }

        if let Some(immediate_content) = immediate_tool_output_content {
            Self::push_user_content_message(&mut messages, immediate_content);
        }

        BuiltChatRequest {
            messages,
            request_overrides,
            tool_protocol_repair_checkpoint_required: protocol_repair.checkpoint_required(),
        }
    }

    /// Inspect the real post-repair provider request without copying provider
    /// payload construction into an eval. Only identities and counts from the
    /// synthetic eval transcript are returned; prompt and result content stay
    /// out of reports.
    pub async fn audit_provider_replay_request(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        profile_override: Option<&str>,
    ) -> ProviderReplayRequestAudit {
        let capabilities = self.profile_capabilities(history, profile_override);
        let (repaired_history, repair) = Self::repair_provider_tool_protocol_view(history);
        let mut retained_tool_call_ids = BTreeSet::new();
        let mut retained_tool_result_ids = BTreeSet::new();
        let mut retained_provider_state_count = 0usize;
        for entry in &repaired_history {
            match entry {
                ProviderHistoryEntryView::AssistantTurn {
                    tool_calls,
                    provider_state,
                    ..
                } => {
                    retained_tool_call_ids.extend(tool_calls.iter().map(|call| call.id.clone()));
                    retained_provider_state_count = retained_provider_state_count
                        .saturating_add(usize::from(provider_state.is_some()));
                },
                entry => {
                    if let Some(tool_call_id) = entry.tool_result_call_id() {
                        retained_tool_result_ids.insert(tool_call_id.to_string());
                    }
                },
            }
        }

        let built = self
            .build_request(system_prompt, history, None, profile_override)
            .await;
        let mut rendered_native_tool_call_ids = BTreeSet::new();
        let mut rendered_native_tool_result_ids = BTreeSet::new();
        for message in &built.messages {
            for block in &message.content {
                match block {
                    RouterContentBlock::ToolCall { id, .. } => {
                        rendered_native_tool_call_ids.insert(id.clone());
                    },
                    RouterContentBlock::ToolResult { tool_call_id, .. } => {
                        rendered_native_tool_result_ids.insert(tool_call_id.clone());
                    },
                    _ => {},
                }
            }
        }
        let previous_response_id = built
            .request_overrides
            .as_ref()
            .and_then(|overrides| overrides.extra.as_ref())
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        ProviderReplayRequestAudit {
            provider: capabilities
                .provider_kind
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown".to_string()),
            replay_protocol: capabilities.replay_protocol().to_string(),
            native_tool_replay: capabilities.supports_native_tool_result_replay(),
            repaired: repair.repaired(),
            checkpoint_required: repair.checkpoint_required(),
            removed_tool_calls: repair.removed_tool_calls,
            removed_tool_results: repair.removed_tool_results,
            duplicate_tool_call_ids: repair.duplicate_tool_call_ids,
            retained_provider_state_count,
            retained_tool_call_ids: retained_tool_call_ids.into_iter().collect(),
            retained_tool_result_ids: retained_tool_result_ids.into_iter().collect(),
            rendered_native_tool_call_ids: rendered_native_tool_call_ids.into_iter().collect(),
            rendered_native_tool_result_ids: rendered_native_tool_result_ids.into_iter().collect(),
            previous_response_id,
            rendered_message_count: built.messages.len(),
        }
    }

    async fn generate_response_streaming_with_repair_state(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
        disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
    ) -> Result<bool> {
        let capabilities = self.profile_capabilities(history, profile_override);
        let mut built_request = self
            .build_request(system_prompt, history, transcript_context, profile_override)
            .await;
        if capabilities.is_openai_responses_family() {
            built_request.request_overrides = Self::build_openai_responses_request_overrides(
                built_request
                    .request_overrides
                    .as_ref()
                    .and_then(|overrides| overrides.extra.as_ref())
                    .and_then(Value::as_object)
                    .and_then(|extra| extra.get("openai_previous_response_id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                !tools.is_empty(),
            );
        }
        Self::attach_trace_context(&mut built_request, transcript_context);
        if let Some(guard) = disclosure_guard {
            built_request
                .request_overrides
                .get_or_insert_with(ChatCompletionRequestOverrides::default)
                .disclosure_guard = Some(guard);
        }
        let repair_checkpoint_required = built_request.tool_protocol_repair_checkpoint_required;
        let messages = built_request.messages;

        debug!(
            "[CHAT-LLM] Generating streaming response for ChatCompletion, messages_count={}, \
             history_len={}, tools_count={}",
            messages.len(),
            history.len(),
            tools.len()
        );

        if let Some(profile) = profile_override {
            self.llm_service
                .generate_chat_completion_streaming_with_profile(
                    profile,
                    messages,
                    tools,
                    tx,
                    built_request.request_overrides,
                )
                .await
                .context("Streaming ChatCompletion with profile override failed")?;
        } else {
            self.llm_service
                .generate_chat_completion_streaming(
                    &LLMOperation::ChatCompletion,
                    messages,
                    tools,
                    tx,
                    built_request.request_overrides,
                )
                .await
                .context("Streaming ChatCompletion LLM call failed")?;
        }

        Ok(repair_checkpoint_required)
    }
}

#[async_trait]
impl ChatLlmClient for ChatLlmService {
    fn config_for_profile(&self, profile_name: &str) -> Result<magicllm::config::LlmConfig> {
        self.llm_service.get_config_by_profile_name(profile_name)
    }
    fn list_chat_profiles(&self) -> Vec<ChatProfileInfo> {
        self.list_chat_profiles()
    }

    fn list_chat_profile_warnings(&self) -> Vec<ChatProfileWarning> {
        self.llm_service.list_chat_profile_warnings()
    }

    fn adaptive_pair_for_profile(&self, profile_name: &str) -> Option<(String, String)> {
        // Call the inner `MultiLLMService` directly to avoid relying on
        // inherent-vs-trait method resolution. Both have the same name
        // here; `self.adaptive_pair_for_profile(...)` resolves to the
        // inherent today, but a future refactor that drops the
        // inherent (or adds a different default) would silently
        // recurse forever.
        self.llm_service.adaptive_pair(profile_name)
    }

    fn default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        self.llm_service
            .default_profile_for_operation(operation_key)
    }

    fn local_default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        self.llm_service
            .local_default_profile_for_operation(operation_key)
    }

    fn telemetry_hint_for_profile(
        &self,
        profile_override: Option<&str>,
    ) -> Option<ChatCompletionTelemetryHint> {
        self.llm_service
            .chat_completion_telemetry_hint(profile_override)
    }

    async fn generate_response(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<ChatLlmResponse> {
        let capabilities = self.profile_capabilities(history, profile_override);
        let mut built_request = self
            .build_request(system_prompt, history, transcript_context, profile_override)
            .await;
        Self::attach_trace_context(&mut built_request, transcript_context);
        let repair_checkpoint_required = built_request.tool_protocol_repair_checkpoint_required;
        let messages = built_request.messages;

        debug!(
            "[CHAT-LLM] Generating response for ChatCompletion, messages_count={}, history_len={}",
            messages.len(),
            history.len(),
        );

        let response = if let Some(profile) = profile_override {
            let resp = self
                .llm_service
                .generate_chat_completion_with_profile(
                    profile,
                    messages,
                    Vec::new(),
                    built_request.request_overrides,
                )
                .await
                .context("ChatCompletion with profile override failed")?;
            // ChatCompletionResponse -> LLMResponse-like: take content
            resp
        } else {
            let resp = self
                .llm_service
                .generate_chat_completion_with_tools(
                    &LLMOperation::ChatCompletion,
                    messages,
                    Vec::new(),
                    built_request.request_overrides,
                )
                .await
                .context("ChatCompletion LLM call failed")?;
            resp
        };

        debug!(
            "[CHAT-LLM] ChatCompletion response length={}",
            response
                .content
                .as_ref()
                .map(|value| value.len())
                .unwrap_or(0)
        );

        let mut provider_state = Self::assistant_provider_state_from_owned_raw_response(
            response.raw_response,
            &capabilities,
        );
        Self::mark_tool_protocol_repair_checkpoint(
            &mut provider_state,
            repair_checkpoint_required,
            response.content.as_deref(),
            &response.tool_calls,
        );

        Ok(ChatLlmResponse {
            content: response.content,
            tool_calls: response.tool_calls,
            provider_state,
            reasoning_text: response.reasoning_text,
            usage: response.usage,
            telemetry: response.telemetry,
        })
    }

    /// Generate a chat response with tool support.
    ///
    /// Phase 2 path: accepts tool specs, passes them to the LLM, and returns
    /// a `ChatLlmResponse` containing both text content and tool calls.
    async fn generate_response_with_tools(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<ChatLlmResponse> {
        let capabilities = self.profile_capabilities(history, profile_override);
        let mut built_request = self
            .build_request(system_prompt, history, transcript_context, profile_override)
            .await;
        if capabilities.is_openai_responses_family() {
            built_request.request_overrides = Self::build_openai_responses_request_overrides(
                built_request
                    .request_overrides
                    .as_ref()
                    .and_then(|overrides| overrides.extra.as_ref())
                    .and_then(Value::as_object)
                    .and_then(|extra| extra.get("openai_previous_response_id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                !tools.is_empty(),
            );
        }
        Self::attach_trace_context(&mut built_request, transcript_context);
        let repair_checkpoint_required = built_request.tool_protocol_repair_checkpoint_required;
        let messages = built_request.messages;

        debug!(
            "[CHAT-LLM] Generating response with tools for ChatCompletion, messages_count={}, \
             history_len={}, tools_count={}",
            messages.len(),
            history.len(),
            tools.len()
        );

        let response: ChatCompletionResponse = if let Some(profile) = profile_override {
            self.llm_service
                .generate_chat_completion_with_profile(
                    profile,
                    messages,
                    tools,
                    built_request.request_overrides,
                )
                .await
                .context("ChatCompletion with profile override failed")?
        } else {
            self.llm_service
                .generate_chat_completion_with_tools(
                    &LLMOperation::ChatCompletion,
                    messages,
                    tools,
                    built_request.request_overrides,
                )
                .await
                .context("ChatCompletion with tools LLM call failed")?
        };

        debug!(
            "[CHAT-LLM] ChatCompletion response content_len={}, tool_calls={}",
            response.content.as_ref().map(|c| c.len()).unwrap_or(0),
            response.tool_calls.len()
        );

        let mut provider_state = Self::assistant_provider_state_from_owned_raw_response(
            response.raw_response,
            &capabilities,
        );
        Self::mark_tool_protocol_repair_checkpoint(
            &mut provider_state,
            repair_checkpoint_required,
            response.content.as_deref(),
            &response.tool_calls,
        );

        Ok(ChatLlmResponse {
            content: response.content,
            tool_calls: response.tool_calls,
            provider_state,
            reasoning_text: response.reasoning_text,
            usage: response.usage,
            telemetry: response.telemetry,
        })
    }

    /// Stream the LLM call through `on_delta` while accumulating the
    /// final response. Wraps `generate_response_streaming` so callers
    /// that already expect a `ChatLlmResponse` (e.g.
    /// `process_chat_inline_turn`) don't have to manage the channel
    /// + Done-delta plumbing themselves. The consumer task forwards
    /// every delta to `on_delta` *before* checking for `Done`, so
    /// reasoning blocks and tool-call deltas surface in real time
    /// while we still capture the assembled response for the caller.
    async fn generate_response_with_tools_streaming(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
        disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
        on_delta: Box<dyn Fn(StreamDelta) + Send + Sync>,
    ) -> Result<ChatLlmResponse> {
        let capabilities = self.profile_capabilities(history, profile_override);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamDelta>(64);
        // Sidecar consumer: forwards every delta to the caller's
        // callback and captures the final `LLMResponse` from the
        // `Done` variant. Runs concurrently with the streaming call
        // below; the channel closes when the streaming call drops
        // `tx` on return, and the consumer's `rx.recv()` returns
        // `None`, ending the loop.
        let consumer = tokio::spawn(async move {
            let mut last_response: Option<LLMResponse> = None;
            while let Some(delta) = rx.recv().await {
                if let StreamDelta::Done(response) = &delta {
                    last_response = Some(response.clone());
                }
                on_delta(delta);
            }
            last_response
        });

        // Drive the streaming call. Errors propagate; on success the
        // channel closes naturally and the consumer task completes.
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let streaming_outcome = self
            .generate_response_streaming_with_repair_state(
                system_prompt,
                history,
                tools,
                tx,
                transcript_context,
                profile_override,
                disclosure_guard,
            )
            .await;

        let captured = consumer.await.context("Streaming-consumer task panicked")?;

        let repair_checkpoint_required = streaming_outcome?;

        let final_response =
            captured.ok_or_else(|| anyhow::anyhow!("Streaming completed without a Done delta"))?;
        let telemetry = self.llm_service.build_chat_completion_telemetry(
            profile_override,
            final_response.usage.as_ref(),
            started_at_ms,
            final_response
                .reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            final_response.trace_receipt.clone(),
            final_response.route_identity.as_ref(),
        );

        let mut provider_state = Self::assistant_provider_state_from_owned_raw_response(
            final_response
                .raw_response
                .map(Self::into_owned_shared_provider_response),
            &capabilities,
        );
        Self::mark_tool_protocol_repair_checkpoint(
            &mut provider_state,
            repair_checkpoint_required,
            final_response.text.as_deref(),
            &final_response.tool_calls,
        );

        Ok(ChatLlmResponse {
            content: final_response.text.as_deref().map(str::to_owned),
            tool_calls: Self::into_owned_shared_tool_calls(final_response.tool_calls),
            provider_state,
            reasoning_text: final_response.reasoning_text.as_deref().map(str::to_owned),
            // Forward the full `magicllm::TokenUsage` breakdown — including
            // cache-read / cache-write / reasoning — so the chat outer-loop's
            // `LLMResponseReceived` emit carries cache efficiency data into
            // the analytics sink.
            usage: final_response.usage.as_ref().map(|u| {
                crate::magician_v2::query_analysis::multi_llm_service::LLMUsage {
                    prompt_tokens: u.prompt_tokens.unwrap_or(0),
                    completion_tokens: u.completion_tokens.unwrap_or(0),
                    total_tokens: u.total_tokens.unwrap_or(0),
                    reasoning_tokens: u.reasoning_tokens.unwrap_or(0),
                    cache_read_tokens: u.cached_tokens.unwrap_or(0),
                    cache_creation_tokens: u.cache_creation_tokens.unwrap_or(0),
                }
            }),
            telemetry,
        })
    }

    /// Generate a streaming chat response with tool support.
    ///
    /// Streams `StreamDelta` values through the provided channel. The `Done`
    /// variant carries the final aggregated `LLMResponse` including any tool
    /// calls. The caller is responsible for reading deltas from the channel
    /// and handling tool calls from the `Done` response.
    async fn generate_response_streaming(
        &self,
        system_prompt: &str,
        history: &[ChatLlmTranscriptEntry],
        tools: Vec<LLMToolSpec>,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        transcript_context: Option<&ChatTranscriptContext>,
        profile_override: Option<&str>,
    ) -> Result<()> {
        self.generate_response_streaming_with_repair_state(
            system_prompt,
            history,
            tools,
            tx,
            transcript_context,
            profile_override,
            None,
        )
        .await
        .map(|_| ())
    }
}

impl StoredToolCall {
    pub fn from_llm_tool_call(call: &LLMToolCall) -> Self {
        Self {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: crate::magician_v2::json_traversal::clone_json_iteratively(&call.arguments),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use magicllm::capability::LLMProviderKind;
    use magicllm::prelude::LlmConfig;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::fs;

    use super::*;
    use crate::magician_v2::chat::models::{
        ChatSessionFileIndex, ChatSessionFileOrigin, ChatSessionFileRecord, PromptImageRef,
    };

    use crate::magician_v2::query_analysis::multi_llm_service::MultiLLMService;
    use crate::magician_v2::tool_result_projection::{
        DisplayResultProjection, ModelResultProjection, ProjectedToolResultV1, ProjectionMetrics,
        ProjectionStrategy, RawResultDescriptor, ResultRetentionClass, ScopedResultRef,
        ToolOutcome, ToolResultIdentity, TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
    };

    #[test]
    fn stored_tool_call_conversion_is_heap_framed_for_deep_arguments() {
        std::thread::Builder::new()
            .name("stored-tool-call-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut arguments = Value::Null;
                for _ in 0..10_000 {
                    arguments = Value::Array(vec![arguments]);
                }
                let mut call = LLMToolCall {
                    id: "call-deep".to_string(),
                    name: "inspect".to_string(),
                    arguments,
                };
                let mut stored = StoredToolCall::from_llm_tool_call(&call);
                crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::take(
                    &mut stored.arguments,
                ));
                crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::take(
                    &mut call.arguments,
                ));
            })
            .expect("spawn stored-tool-call regression")
            .join()
            .expect("stored-tool-call regression completes");
    }

    fn test_service(config: LlmConfig, profile_name: &str) -> ChatLlmService {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(profile_name.to_string(), config);
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            LLMOperation::ChatCompletion.as_str().to_string(),
            profile_name.to_string(),
        );
        ChatLlmService::new(Arc::new(MultiLLMService::new(
            llm_configs,
            operation_mapping,
        )))
    }

    fn projected_result(call_id: &str, value: Value) -> ProjectedToolResultV1 {
        let raw = RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: format!("result-{call_id}"),
                cursor: None,
            },
            content_hash: format!("sha256-{call_id}"),
            media_type: "application/json".to_string(),
            size_bytes: serde_json::to_vec(&value).expect("serialize fixture").len() as u64,
            retention_class: ResultRetentionClass::ChatSession,
        };
        ProjectedToolResultV1 {
            schema_version: TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
            identity: ToolResultIdentity {
                tool_name: "memory_search".to_string(),
                tool_call_id: call_id.to_string(),
                execution_id: Some("execution-1".to_string()),
                task_id: None,
                scope_digest: "scope-digest".to_string(),
                authority_revision: "authority-1".to_string(),
            },
            outcome: ToolOutcome::succeeded(),
            model: ModelResultProjection {
                value,
                strategy: ProjectionStrategy::Complete,
                included_records: 1,
                omitted_records: 0,
                omitted_fields: 0,
                continuation: None,
            },
            app_result_checkpoint: None,
            spoken: None,
            display: DisplayResultProjection::referenced(&raw),
            raw,
            metrics: ProjectionMetrics {
                raw_bytes: 1,
                model_bytes: 1,
                estimated_model_tokens: 1,
                spoken_characters: 0,
                included_records: 1,
                omitted_records: 0,
                omitted_fields: 0,
                maximum_input_depth: 2,
                contract_fallback: false,
            },
        }
    }

    #[test]
    fn projection_replay_stats_are_content_free_cumulative_and_saturating() {
        let mut first = projected_result("call-a", json!({ "secret": "never logged" }));
        first.metrics.raw_bytes = 1_000;
        first.metrics.model_bytes = 200;
        first.metrics.estimated_model_tokens = 50;
        let mut second = projected_result("call-b", json!({ "secret": "also never logged" }));
        second.metrics.raw_bytes = 500;
        second.metrics.model_bytes = 125;
        second.metrics.estimated_model_tokens = 32;
        let invalid = projected_result("different-call", json!({ "secret": "foreign" }));
        let history = vec![
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("lookup".to_string()),
                projection: first,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: Some("lookup".to_string()),
                projection: second,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-c".to_string(),
                tool_name: Some("lookup".to_string()),
                projection: invalid,
            },
        ];

        let stats = ToolResultProjectionReplayStats::from_history(&history);

        assert_eq!(stats.replay_count, 2);
        assert_eq!(stats.invalid_projection_count, 1);
        assert_eq!(stats.cumulative_raw_bytes, 1_500);
        assert_eq!(stats.cumulative_model_bytes, 325);
        assert_eq!(stats.cumulative_estimated_model_tokens, 82);
        assert_eq!(stats.cumulative_bytes_saved, 1_175);
    }

    #[test]
    fn long_provider_projection_borrows_semantic_payloads_and_preserves_cardinality() {
        let large_text = "provider-history-payload".repeat(1_024);
        let large_result = "provider-tool-result".repeat(1_024);
        let mut history = Vec::new();
        for turn in 0..128 {
            history.push(ChatLlmTranscriptEntry::UserText {
                text: format!("{turn}:{large_text}"),
            });
            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                text: Some(format!("assistant-{turn}:{large_text}")),
                tool_calls: vec![StoredToolCall {
                    id: format!("call-{turn}"),
                    name: "lookup".to_string(),
                    arguments: json!({ "turn": turn, "payload": large_text.as_str() }),
                }],
                provider_state: None,
            });
            history.push(ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: format!("call-{turn}"),
                tool_name: Some("lookup".to_string()),
                content: format!("{turn}:{large_result}"),
            });
        }

        let (view, repair) = ChatLlmService::repair_provider_tool_protocol_view(&history);
        assert!(!repair.repaired());
        assert_eq!(view.len(), history.len());

        let mut owned_projection_texts = 0usize;
        for (source, projected) in history.iter().zip(&view) {
            match (source, projected) {
                (
                    ChatLlmTranscriptEntry::UserText { text: source },
                    ProviderHistoryEntryView::UserText {
                        text: projected, ..
                    },
                ) => assert!(std::ptr::eq(source.as_str(), *projected)),
                (
                    ChatLlmTranscriptEntry::AssistantTurn {
                        text: Some(source),
                        tool_calls: source_calls,
                        ..
                    },
                    ProviderHistoryEntryView::AssistantTurn {
                        text: Some(projected),
                        tool_calls,
                        ..
                    },
                ) => {
                    owned_projection_texts = owned_projection_texts
                        .saturating_add(usize::from(matches!(projected, Cow::Owned(_))));
                    assert!(std::ptr::eq(source.as_str(), projected.as_ref()));
                    assert_eq!(tool_calls.iter().count(), source_calls.len());
                    assert!(std::ptr::eq(
                        source_calls.first().expect("source tool call"),
                        tool_calls.iter().next().expect("projected tool call"),
                    ));
                },
                (
                    ChatLlmTranscriptEntry::ToolResult {
                        content: source, ..
                    },
                    ProviderHistoryEntryView::ToolResult {
                        content: projected, ..
                    },
                ) => assert!(std::ptr::eq(source.as_str(), *projected)),
                pair => panic!("provider projection drifted entry shape: {pair:?}"),
            }
        }
        assert_eq!(owned_projection_texts, 0);
    }

    #[test]
    fn interrupted_projection_allocates_only_the_repair_note() {
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "run both".repeat(8_192),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("working".repeat(8_192)),
                tool_calls: vec![
                    StoredToolCall {
                        id: "complete".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "one" }),
                    },
                    StoredToolCall {
                        id: "interrupted".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "two" }),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "complete".to_string(),
                tool_name: Some("lookup".to_string()),
                content: "done".repeat(8_192),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "continue".repeat(8_192),
            },
        ];

        let (view, repair) = ChatLlmService::repair_provider_tool_protocol_view(&history);
        assert!(repair.repaired());
        assert_eq!(view.len(), history.len());
        let owned_texts = view
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    ProviderHistoryEntryView::AssistantTurn {
                        text: Some(Cow::Owned(_)),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(owned_texts, 1);
        assert!(matches!(
            &view[0],
            ProviderHistoryEntryView::UserText { text, .. }
                if std::ptr::eq(*text, match &history[0] {
                    ChatLlmTranscriptEntry::UserText { text } => text.as_str(),
                    _ => unreachable!(),
                })
        ));
        assert!(matches!(
            &view[2],
            ProviderHistoryEntryView::ToolResult { content, .. }
                if std::ptr::eq(*content, match &history[2] {
                    ChatLlmTranscriptEntry::ToolResult { content, .. } => content.as_str(),
                    _ => unreachable!(),
                })
        ));
    }

    #[test]
    fn consuming_flatteners_preserve_exact_provider_narration() {
        let call = StoredToolCall {
            id: "call-1".to_string(),
            name: "lookup".to_string(),
            arguments: json!([1, true]),
        };
        assert_eq!(
            ChatLlmService::flatten_assistant_turn(
                Some(Cow::Owned("  Working.  ".to_string())),
                [&call],
            ),
            "Working.\n\nCalled tool `lookup` with arguments:\n[\n  1,\n  true\n]",
        );
        assert_eq!(
            ChatLlmService::flatten_tool_result(
                Some(" lookup "),
                "  {\"status\":\"ok\"}\n".to_string(),
            ),
            "Tool `lookup` result:\n{\"status\":\"ok\"}",
        );
        assert_eq!(
            ChatLlmService::flatten_tool_result(Some("lookup"), " \n ".to_string()),
            "Tool `lookup` returned no text output.",
        );
    }

    #[test]
    fn owned_provider_state_extraction_preserves_shape_and_moves_native_blocks() {
        let fixtures = [
            json!({
                "object": "response",
                "id": "resp-1",
                "output": [{"type": "message"}],
            }),
            json!({
                "candidates": [{"content": {"parts": [{"text": "gemini"}]}}],
            }),
            json!({
                "content": [{"type": "text", "text": "anthropic"}],
            }),
        ];
        for fixture in fixtures {
            let expected =
                ChatLlmService::assistant_provider_state_from_raw_response_shape(Some(&fixture));
            let actual = ChatLlmService::assistant_provider_state_from_owned_raw_response_shape(
                Some(fixture),
            );
            assert_eq!(actual, expected);
        }

        let large = "native-block".repeat(8_192);
        let large_ptr = large.as_ptr();
        let mut native_block = serde_json::Map::new();
        native_block.insert("type".to_string(), Value::String("text".to_string()));
        native_block.insert("text".to_string(), Value::String(large));
        let mut raw_response = serde_json::Map::new();
        raw_response.insert(
            "content".to_string(),
            Value::Array(vec![Value::Object(native_block)]),
        );
        let state = ChatLlmService::assistant_provider_state_from_owned_raw_response_shape(Some(
            Value::Object(raw_response),
        ))
        .expect("Anthropic native state");
        let AssistantProviderState::AnthropicMessages { content } = state else {
            panic!("expected Anthropic state");
        };
        assert_eq!(
            content[0]["text"].as_str().expect("native text").as_ptr(),
            large_ptr,
            "owned extraction must move the provider block instead of cloning it"
        );
    }

    #[test]
    fn owned_provider_state_cleanup_does_not_recursively_drop_deep_raw_json() {
        std::thread::Builder::new()
            .name("chat-provider-state-cleanup-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let deep_value = || {
                    let mut value = Value::Null;
                    for _ in 0..20_000 {
                        value = Value::Array(vec![value]);
                    }
                    value
                };

                let mut openai_raw = serde_json::Map::new();
                openai_raw.insert("object".to_string(), Value::String("response".to_string()));
                openai_raw.insert(
                    "id".to_string(),
                    Value::String("resp-deep-extra".to_string()),
                );
                openai_raw.insert("output".to_string(), Value::Array(Vec::new()));
                openai_raw.insert("ignored".to_string(), deep_value());
                let state = ChatLlmService::assistant_provider_state_from_owned_raw_response_shape(
                    Some(Value::Object(openai_raw)),
                );
                assert!(matches!(
                    state,
                    Some(AssistantProviderState::OpenaiResponses { .. })
                ));

                let mut anthropic_raw = serde_json::Map::new();
                anthropic_raw.insert("content".to_string(), Value::Array(vec![deep_value()]));
                let rejected = ChatLlmService::assistant_provider_state_from_owned_raw_response(
                    Some(Value::Object(anthropic_raw)),
                    &ChatProfileCapabilities {
                        provider_kind: Some(LLMProviderKind::Gemini),
                        ..ChatProfileCapabilities::default()
                    },
                );
                assert!(rejected.is_none());
            })
            .unwrap()
            .join()
            .expect("provider-state cleanup must fit a 512 KiB stack");
    }

    /// xAI speaks the Responses protocol and nothing else, so a Grok chat
    /// profile replays assistant turns the Responses way — anchored on
    /// `previous_response_id` — and keeps an OpenAI-Responses provider state
    /// its own response produced.
    #[test]
    fn an_xai_chat_profile_is_a_responses_family_replay() {
        let xai = ChatProfileCapabilities {
            provider_kind: Some(LLMProviderKind::Xai),
            openai_api_mode: Some("responses"),
            supports_user_image_inputs: true,
            supports_multimodal_tool_result_replay: false,
        };
        assert!(xai.is_openai_responses_family());
        assert!(xai.supports_native_assistant_turn_replay());
        assert_eq!(xai.replay_protocol(), "openai_responses");

        let mut raw = serde_json::Map::new();
        raw.insert("id".to_string(), Value::String("resp-grok".to_string()));
        raw.insert("output".to_string(), Value::Array(Vec::new()));
        let kept = ChatLlmService::assistant_provider_state_from_owned_raw_response(
            Some(Value::Object(raw)),
            &xai,
        );
        assert!(matches!(
            kept,
            Some(AssistantProviderState::OpenaiResponses { .. })
        ));
    }

    #[test]
    fn streaming_chat_boundary_moves_uniquely_owned_tool_arguments() {
        let arguments = Value::String("argument".repeat(8_192));
        let argument_ptr = arguments.as_str().expect("argument string").as_ptr();
        let calls = ChatLlmService::into_owned_shared_tool_calls(Arc::new(vec![LLMToolCall {
            id: "call-1".to_string(),
            name: "lookup".to_string(),
            arguments,
        }]));
        assert_eq!(
            calls[0]
                .arguments
                .as_str()
                .expect("retained argument")
                .as_ptr(),
            argument_ptr
        );
    }

    #[test]
    fn phase1_chat_tool_loop_calls_share_turn_trace_and_keep_distinct_call_ids() {
        let transcript = ChatTranscriptContext {
            session_id: "session-1".to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            chat_turn_id: Some("turn-1".to_string()),
            user_message_id: Some("message-1".to_string()),
            trace_context: None,
            provider_attempt_counter: None,
        };
        let mut first = BuiltChatRequest {
            messages: Vec::new(),
            request_overrides: None,
            tool_protocol_repair_checkpoint_required: false,
        };
        let mut synthesis = first.clone();

        ChatLlmService::attach_trace_context(&mut first, Some(&transcript));
        ChatLlmService::attach_trace_context(&mut synthesis, Some(&transcript));

        let first = first
            .request_overrides
            .and_then(|overrides| overrides.trace_context)
            .expect("first call trace");
        let synthesis = synthesis
            .request_overrides
            .and_then(|overrides| overrides.trace_context)
            .expect("synthesis call trace");
        assert_eq!(first.trace_id, "turn-1");
        assert_eq!(first.trace_id, synthesis.trace_id);
        assert_ne!(first.llm_call_id, synthesis.llm_call_id);
        assert_eq!(first.chat_session_id.as_deref(), Some("session-1"));
        assert_eq!(first.chat_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(first.user_message_id.as_deref(), Some("message-1"));
        assert_eq!(first.scope, magicllm::LlmScope::new("owner", "default"));
    }

    #[test]
    fn caller_owned_trace_and_attempt_counter_survive_request_assembly() {
        let trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "default"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let transcript = ChatTranscriptContext {
            session_id: "session-1".to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            chat_turn_id: Some("turn-1".to_string()),
            user_message_id: Some("message-1".to_string()),
            trace_context: Some(trace.clone()),
            provider_attempt_counter: Some(Arc::clone(&counter)),
        };
        let mut request = BuiltChatRequest::default();

        ChatLlmService::attach_trace_context(&mut request, Some(&transcript));

        let overrides = request.request_overrides.expect("trace overrides");
        assert_eq!(overrides.trace_context, Some(trace));
        assert!(Arc::ptr_eq(
            overrides
                .provider_attempt_counter
                .as_ref()
                .expect("attempt counter"),
            &counter,
        ));
    }

    #[tokio::test]
    async fn anthropic_provider_state_replays_thinking_before_tool_use() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                supports_tool_calling: Some(true),
                ..Default::default()
            },
            "anthropic-chat",
        );

        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "toolu_1".to_string(),
                    name: "lookup_account".to_string(),
                    arguments: json!({"account_id": "acct_1"}),
                }],
                provider_state: Some(AssistantProviderState::AnthropicMessages {
                    content: vec![
                        json!({
                            "type": "thinking",
                            "thinking": "Need account state before answering.",
                            "signature": "sig-123"
                        }),
                        json!({
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "lookup_account",
                            "input": {"account_id": "acct_1"}
                        }),
                    ],
                }),
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "toolu_1".to_string(),
                tool_name: Some("lookup_account".to_string()),
                content: r#"{"status":"ok"}"#.to_string(),
            },
        ];

        let built = service.build_request("system", &history, None, None).await;
        assert_eq!(built.messages.len(), 3);
        assert_eq!(built.messages[1].role, MessageRole::Assistant);
        assert_eq!(built.messages[1].content.len(), 2);

        let RouterContentBlock::Json { value } = &built.messages[1].content[0] else {
            panic!("thinking block should be preserved as raw Anthropic content");
        };
        let raw = magicllm::as_anthropic_raw_content_block(value).expect("raw block");
        assert_eq!(raw["type"], "thinking");
        assert_eq!(raw["signature"], "sig-123");

        match &built.messages[1].content[1] {
            RouterContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "lookup_account");
                assert_eq!(arguments, &json!({"account_id": "acct_1"}));
            },
            block => panic!("unexpected block: {:?}", block),
        }
    }

    #[tokio::test]
    async fn openai_responses_keeps_user_images_after_tool_history() {
        let temp_dir = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_vision: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        )
        .with_workspace_layout(workspace.clone());

        let session_id = "session-1".to_string();
        let principal = "user-1".to_string();
        let scope = "workspace-a".to_string();
        workspace
            .ensure_chat_session_workspace(&principal, &scope, &session_id)
            .await
            .expect("chat session workspace");
        let image_path = workspace
            .chat_session_outputs_dir(&principal, &scope, &session_id)
            .join("draft.png");
        fs::write(&image_path, [137_u8, 80, 78, 71])
            .await
            .expect("image bytes");

        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Using a tool.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "call-1".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster" }),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp_1".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("generate_image".to_string()),
                content: vec![TranscriptBlock::Text {
                    text: "Created a draft image.".to_string(),
                }],
            },
            ChatLlmTranscriptEntry::UserTurn {
                content: vec![
                    TranscriptBlock::Text {
                        text: "Make the title bigger.".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "draft.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: Some("Draft".to_string()),
                        },
                    },
                ],
            },
        ];

        let request = service
            .build_request(
                "system",
                &history,
                Some(&ChatTranscriptContext {
                    session_id,
                    principal,
                    workspace: scope,
                    chat_turn_id: None,
                    user_message_id: None,
                    trace_context: None,
                    provider_attempt_counter: None,
                }),
                Some("openai-responses"),
            )
            .await;
        let messages = request.messages;

        assert_eq!(
            request
                .request_overrides
                .as_ref()
                .and_then(|overrides| overrides.extra.as_ref())
                .and_then(Value::as_object)
                .and_then(|extra| extra.get("openai_previous_response_id"))
                .and_then(Value::as_str),
            Some("resp_1")
        );
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            &messages[0].content[..],
            [RouterContentBlock::Text { text }] if text == "system"
        ));
        assert!(matches!(
            &messages[1].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, .. }]
                if tool_call_id == "call-1"
        ));
        assert!(matches!(
            &messages[2].content[..],
            [RouterContentBlock::Text { text }, RouterContentBlock::Image { .. }]
                if text == "Make the title bigger."
        ));
    }

    #[tokio::test]
    async fn recent_tool_outputs_are_rehydrated_as_follow_up_references() {
        let temp_dir = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_vision: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        )
        .with_workspace_layout(workspace.clone());

        let session_id = "session-2".to_string();
        let principal = "user-1".to_string();
        let scope = "workspace-a".to_string();
        workspace
            .ensure_chat_session_workspace(&principal, &scope, &session_id)
            .await
            .expect("chat session workspace");

        let image_path = workspace
            .chat_session_outputs_dir(&principal, &scope, &session_id)
            .join("poster_v2.png");
        fs::write(&image_path, [137_u8, 80, 78, 71])
            .await
            .expect("image bytes");
        let file_index = ChatSessionFileIndex {
            files: vec![ChatSessionFileRecord {
                id: "file_123".to_string(),
                stored_name: "poster_v2.png".to_string(),
                original_name: "poster_v2.png".to_string(),
                mime_type: "image/png".to_string(),
                size: 4,
                label: Some("Poster V2".to_string()),
                screen_capture: None,
                prompt_image: true,
                origin: ChatSessionFileOrigin::ToolOutput {
                    tool_name: "generate_image".to_string(),
                    tool_call_id: Some("call-2".to_string()),
                },
                source_task_output_id: None,
                source_task_id: None,
                created_at: 42,
            }],
        };
        fs::write(
            workspace.chat_session_file_index_path(&principal, &scope, &session_id),
            serde_json::to_vec_pretty(&file_index).expect("serialize file index"),
        )
        .await
        .expect("file index");

        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "Create a poster with a bold headline.".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Generating a first draft.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "call-2".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster draft" }),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp_2".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "call-2".to_string(),
                tool_name: Some("generate_image".to_string()),
                content: vec![
                    TranscriptBlock::Text {
                        text: "Generated poster_v2.png".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "poster_v2.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: Some("Poster V2".to_string()),
                        },
                    },
                ],
            },
            ChatLlmTranscriptEntry::UserText {
                text: "Make the headline larger and keep the same base image.".to_string(),
            },
        ];

        let request = service
            .build_request(
                "system",
                &history,
                Some(&ChatTranscriptContext {
                    session_id,
                    principal,
                    workspace: scope,
                    chat_turn_id: None,
                    user_message_id: None,
                    trace_context: None,
                    provider_attempt_counter: None,
                }),
                Some("openai-responses"),
            )
            .await;
        let messages = request.messages;

        assert_eq!(
            request
                .request_overrides
                .as_ref()
                .and_then(|overrides| overrides.extra.as_ref())
                .and_then(Value::as_object)
                .and_then(|extra| extra.get("openai_previous_response_id"))
                .and_then(Value::as_str),
            Some("resp_2")
        );
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[1].role, MessageRole::Tool);
        assert_eq!(messages[2].role, MessageRole::User);
        assert!(messages[2].content.iter().any(|block| {
            matches!(
                block,
                RouterContentBlock::Text { text }
                    if text.contains("Recent session files from earlier tool results")
                        && text.contains("poster_v2.png")
                        && text.contains("path:")
            )
        }));
        assert!(messages[2]
            .content
            .iter()
            .any(|block| matches!(block, RouterContentBlock::Image { .. })));
    }

    #[tokio::test]
    async fn same_turn_tool_outputs_are_rehydrated_as_immediate_follow_up_references() {
        let temp_dir = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_vision: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        )
        .with_workspace_layout(workspace.clone());

        let session_id = "session-3".to_string();
        let principal = "user-1".to_string();
        let scope = "workspace-a".to_string();
        workspace
            .ensure_chat_session_workspace(&principal, &scope, &session_id)
            .await
            .expect("chat session workspace");

        let image_path = workspace
            .chat_session_outputs_dir(&principal, &scope, &session_id)
            .join("poster_v3.png");
        fs::write(&image_path, [137_u8, 80, 78, 71])
            .await
            .expect("image bytes");
        let file_index = ChatSessionFileIndex {
            files: vec![ChatSessionFileRecord {
                id: "file_456".to_string(),
                stored_name: "poster_v3.png".to_string(),
                original_name: "poster_v3.png".to_string(),
                mime_type: "image/png".to_string(),
                size: 4,
                label: Some("Poster V3".to_string()),
                screen_capture: None,
                prompt_image: true,
                origin: ChatSessionFileOrigin::ToolOutput {
                    tool_name: "generate_image".to_string(),
                    tool_call_id: Some("call-3".to_string()),
                },
                source_task_output_id: None,
                source_task_id: None,
                created_at: 42,
            }],
        };
        fs::write(
            workspace.chat_session_file_index_path(&principal, &scope, &session_id),
            serde_json::to_vec_pretty(&file_index).expect("serialize file index"),
        )
        .await
        .expect("file index");

        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "Create a revised poster.".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Generating a revised draft.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "call-3".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster revision" }),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp_3".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "call-3".to_string(),
                tool_name: Some("generate_image".to_string()),
                content: vec![
                    TranscriptBlock::Text {
                        text: "Generated poster_v3.png".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "poster_v3.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: Some("Poster V3".to_string()),
                        },
                    },
                ],
            },
        ];

        let request = service
            .build_request(
                "system",
                &history,
                Some(&ChatTranscriptContext {
                    session_id,
                    principal,
                    workspace: scope,
                    chat_turn_id: None,
                    user_message_id: None,
                    trace_context: None,
                    provider_attempt_counter: None,
                }),
                Some("openai-responses"),
            )
            .await;
        let messages = request.messages;

        assert_eq!(
            request
                .request_overrides
                .as_ref()
                .and_then(|overrides| overrides.extra.as_ref())
                .and_then(Value::as_object)
                .and_then(|extra| extra.get("openai_previous_response_id"))
                .and_then(Value::as_str),
            Some("resp_3")
        );
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(matches!(
            &messages[1].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, .. }] if tool_call_id == "call-3"
        ));
        assert!(messages[2].content.iter().any(|block| {
            matches!(
                block,
                RouterContentBlock::Text { text }
                    if text.contains("Immediate tool output images from the preceding tool results")
                        && text.contains("poster_v3.png")
                        && text.contains("path:")
            )
        }));
        assert!(messages[2]
            .content
            .iter()
            .any(|block| matches!(block, RouterContentBlock::Image { .. })));
    }

    #[tokio::test]
    async fn openai_responses_does_not_anchor_past_newer_foreign_provider_turns() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_vision: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );

        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("OpenAI turn".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp_old".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "Now switch providers.".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Gemini turn".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::Gemini {
                    parts: vec![json!({ "text": "Gemini turn" })],
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "Continue on OpenAI now.".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        assert!(request.request_overrides.is_none());
        assert_eq!(request.messages[0].role, MessageRole::System);
        assert!(request.messages.iter().any(|message| {
            matches!(
                &message.content[..],
                [RouterContentBlock::Text { text }] if text == "OpenAI turn"
            )
        }));
        assert!(request.messages.iter().any(|message| {
            matches!(
                &message.content[..],
                [RouterContentBlock::Text { text }] if text == "Continue on OpenAI now."
            )
        }));
    }

    #[tokio::test]
    async fn projected_results_replay_natively_with_ordered_call_pairing_across_providers() {
        let profiles = [
            (
                "openai-chat",
                LlmConfig {
                    provider: LLMProviderKind::OpenAI,
                    model: "gpt-5.6-terra".to_string(),
                    supports_tool_calling: Some(true),
                    additional_params: Some(HashMap::from([(
                        "openai_api_mode".to_string(),
                        json!("chat"),
                    )])),
                    ..Default::default()
                },
            ),
            (
                "openai-responses",
                LlmConfig {
                    provider: LLMProviderKind::OpenAI,
                    model: "gpt-5.6-terra".to_string(),
                    supports_tool_calling: Some(true),
                    additional_params: Some(HashMap::from([(
                        "openai_api_mode".to_string(),
                        json!("responses"),
                    )])),
                    ..Default::default()
                },
            ),
            (
                "anthropic",
                LlmConfig {
                    provider: LLMProviderKind::Anthropic,
                    model: "claude-sonnet-4-6".to_string(),
                    supports_tool_calling: Some(true),
                    ..Default::default()
                },
            ),
            (
                "gemini",
                LlmConfig {
                    provider: LLMProviderKind::Gemini,
                    model: "gemini-2.5-flash".to_string(),
                    supports_tool_calling: Some(true),
                    ..Default::default()
                },
            ),
            (
                "openrouter",
                LlmConfig {
                    provider: LLMProviderKind::OpenRouter,
                    model: "openai/gpt-5.6-terra".to_string(),
                    supports_tool_calling: Some(true),
                    ..Default::default()
                },
            ),
        ];
        let first = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "records": [{ "name": "Ada" }] }
        });
        let second = json!({
            "schema_version": 1,
            "outcome": { "status": "partial" },
            "data": { "records": [{ "name": "Grace" }] }
        });
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("I will check both records.".to_string()),
                tool_calls: vec![
                    StoredToolCall {
                        id: "call-a".to_string(),
                        name: "memory_search".to_string(),
                        arguments: json!({ "query": "Ada" }),
                    },
                    StoredToolCall {
                        id: "call-b".to_string(),
                        name: "memory_search".to_string(),
                        arguments: json!({ "query": "Grace" }),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: projected_result("call-a", first.clone()),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-b".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: projected_result("call-b", second.clone()),
            },
        ];

        for (profile_name, config) in profiles {
            let service = test_service(config, profile_name);
            let mut provider_history = history.clone();
            if profile_name == "gemini" {
                let ChatLlmTranscriptEntry::AssistantTurn { provider_state, .. } =
                    &mut provider_history[0]
                else {
                    unreachable!("fixture starts with assistant turn")
                };
                *provider_state = Some(AssistantProviderState::Gemini {
                    parts: vec![
                        json!({
                            "functionCall": {
                                "id": "call-a",
                                "name": "memory_search",
                                "args": { "query": "Ada" }
                            },
                            "thoughtSignature": "signature-a"
                        }),
                        json!({
                            "functionCall": {
                                "id": "call-b",
                                "name": "memory_search",
                                "args": { "query": "Grace" }
                            },
                            "thoughtSignature": "signature-b"
                        }),
                    ],
                });
            }
            let request = service
                .build_request("system", &provider_history, None, Some(profile_name))
                .await;

            assert_eq!(request.messages.len(), 4, "profile={profile_name}");
            assert_eq!(request.messages[1].role, MessageRole::Assistant);
            if profile_name == "gemini" {
                assert!(matches!(
                    &request.messages[1].content[..],
                    [RouterContentBlock::Json { .. }]
                ));
            } else {
                assert!(
                    matches!(
                        &request.messages[1].content[..],
                        [RouterContentBlock::Text { .. }, RouterContentBlock::ToolCall { id: first, .. }, RouterContentBlock::ToolCall { id: second, .. }]
                            if first == "call-a" && second == "call-b"
                    ),
                    "profile={profile_name}"
                );
            }
            assert!(
                matches!(
                    &request.messages[2].content[..],
                    [RouterContentBlock::ToolResult { tool_call_id, content }]
                        if tool_call_id == "call-a" && content == &first
                ),
                "profile={profile_name}"
            );
            assert!(
                matches!(
                    &request.messages[3].content[..],
                    [RouterContentBlock::ToolResult { tool_call_id, content }]
                        if tool_call_id == "call-b" && content == &second
                ),
                "profile={profile_name}"
            );
        }
    }

    #[tokio::test]
    async fn provider_replay_interrupted_tool_repair_is_consistent_across_every_family() {
        let profiles = [
            (
                "openai-chat",
                LLMProviderKind::OpenAI,
                Some("chat"),
                true,
                "openai_chat",
            ),
            (
                "openai-responses",
                LLMProviderKind::OpenAI,
                Some("responses"),
                true,
                "openai_responses",
            ),
            (
                "anthropic",
                LLMProviderKind::Anthropic,
                None,
                true,
                "anthropic_messages",
            ),
            ("gemini", LLMProviderKind::Gemini, None, true, "gemini"),
            (
                "minimax",
                LLMProviderKind::Minimax,
                None,
                true,
                "anthropic_messages",
            ),
            (
                "deepseek",
                LLMProviderKind::DeepSeek,
                None,
                true,
                "anthropic_messages",
            ),
            (
                "openrouter",
                LLMProviderKind::OpenRouter,
                None,
                true,
                "openrouter",
            ),
            ("ollama", LLMProviderKind::Ollama, None, false, "flattened"),
            ("yutori", LLMProviderKind::Yutori, None, false, "flattened"),
        ];
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "look up both".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![
                    StoredToolCall {
                        id: "complete".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({"id": 1}),
                    },
                    StoredToolCall {
                        id: "orphan".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({"id": 2}),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "complete".to_string(),
                tool_name: Some("lookup".to_string()),
                content: "done".to_string(),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "continue".to_string(),
            },
        ];

        for (profile_name, provider, openai_mode, native, protocol) in profiles {
            let service = test_service(
                LlmConfig {
                    provider,
                    model: "eval-model".to_string(),
                    api_key_env: None,
                    supports_tool_calling: Some(true),
                    additional_params: openai_mode
                        .map(|mode| HashMap::from([("openai_api_mode".to_string(), json!(mode))])),
                    ..Default::default()
                },
                profile_name,
            );
            let audit = service
                .audit_provider_replay_request("system", &history, Some(profile_name))
                .await;

            assert!(audit.repaired, "profile={profile_name}");
            assert!(audit.checkpoint_required, "profile={profile_name}");
            assert_eq!(audit.removed_tool_calls, 1, "profile={profile_name}");
            assert_eq!(audit.removed_tool_results, 0, "profile={profile_name}");
            assert_eq!(
                audit.retained_provider_state_count, 0,
                "profile={profile_name}"
            );
            assert_eq!(
                audit.retained_tool_call_ids,
                ["complete"],
                "profile={profile_name}"
            );
            assert_eq!(
                audit.retained_tool_result_ids,
                ["complete"],
                "profile={profile_name}"
            );
            assert_eq!(audit.native_tool_replay, native, "profile={profile_name}");
            assert_eq!(audit.replay_protocol, protocol, "profile={profile_name}");
            assert!(
                audit.previous_response_id.is_none(),
                "profile={profile_name}"
            );
        }
    }

    #[tokio::test]
    async fn openai_responses_anchor_replays_only_the_new_projected_result_with_original_call_id() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let model_value = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "value": "May 8" }
        });
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "call-a".to_string(),
                    name: "memory_search".to_string(),
                    arguments: json!({ "query": "birthday" }),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "response-anchor".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: projected_result("call-a", model_value.clone()),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        assert_eq!(
            request
                .request_overrides
                .as_ref()
                .and_then(|overrides| overrides.extra.as_ref())
                .and_then(|extra| extra.get("openai_previous_response_id"))
                .and_then(Value::as_str),
            Some("response-anchor")
        );
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, MessageRole::System);
        assert!(matches!(
            &request.messages[1].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, content }]
                if tool_call_id == "call-a" && content == &model_value
        ));
    }

    #[tokio::test]
    async fn openai_responses_repairs_interrupted_tool_calls_and_discards_poisoned_anchors() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "first request".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![
                    StoredToolCall {
                        id: "orphan-a".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "one" }),
                    },
                    StoredToolCall {
                        id: "orphan-b".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "two" }),
                    },
                ],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "interrupted-response".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "retry".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Recovered locally.".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "server-state-chained-through-interruption".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "new request".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        assert!(request
            .request_overrides
            .as_ref()
            .and_then(|overrides| overrides.extra.as_ref())
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .is_none());
        assert!(request.messages.iter().all(|message| {
            message.content.iter().all(|block| {
                !matches!(
                    block,
                    RouterContentBlock::ToolCall { id, .. }
                        if id == "orphan-a" || id == "orphan-b"
                ) && !matches!(
                    block,
                    RouterContentBlock::ToolResult { tool_call_id, .. }
                        if tool_call_id == "orphan-a" || tool_call_id == "orphan-b"
                )
            })
        }));
        assert!(request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    RouterContentBlock::Text { text }
                        if text.contains("interrupted before their results were recorded")
                )
            })
        }));
    }

    #[tokio::test]
    async fn openai_responses_reuses_clean_checkpoint_after_older_interruption() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "old request".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "old-orphan".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({}),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "poisoned".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "recover".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Recovered from repaired history.".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "clean-checkpoint".to_string(),
                    tool_protocol_repair_checkpoint: true,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "new request".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        assert_eq!(
            request
                .request_overrides
                .as_ref()
                .and_then(|overrides| overrides.extra.as_ref())
                .and_then(|extra| extra.get("openai_previous_response_id"))
                .and_then(Value::as_str),
            Some("clean-checkpoint")
        );
        assert!(!request.tool_protocol_repair_checkpoint_required);
        assert_eq!(request.messages.len(), 2);
        assert!(matches!(
            &request.messages[1].content[..],
            [RouterContentBlock::Text { text }] if text == "new request"
        ));
    }

    #[tokio::test]
    async fn openai_responses_invalidates_checkpoint_after_a_newer_interruption() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Earlier clean response.".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "old-clean-checkpoint".to_string(),
                    tool_protocol_repair_checkpoint: true,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "run a tool".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "new-orphan".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({}),
                }],
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "new-poisoned-response".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "continue".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        assert!(request
            .request_overrides
            .as_ref()
            .and_then(|overrides| overrides.extra.as_ref())
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .is_none());
        assert!(request.tool_protocol_repair_checkpoint_required);
        assert!(request.messages.iter().all(|message| {
            message.content.iter().all(|block| {
                !matches!(
                    block,
                    RouterContentBlock::ToolCall { id, .. } if id == "new-orphan"
                )
            })
        }));
    }

    #[test]
    fn tool_protocol_checkpoint_is_final_response_only_and_backward_compatible() {
        let mut state = Some(AssistantProviderState::OpenaiResponses {
            response_id: "response".to_string(),
            tool_protocol_repair_checkpoint: false,
        });
        ChatLlmService::mark_tool_protocol_repair_checkpoint(
            &mut state,
            true,
            Some("Recovered."),
            &[],
        );
        assert!(matches!(
            state,
            Some(AssistantProviderState::OpenaiResponses {
                tool_protocol_repair_checkpoint: true,
                ..
            })
        ));

        let mut tool_state = Some(AssistantProviderState::OpenaiResponses {
            response_id: "tool-response".to_string(),
            tool_protocol_repair_checkpoint: false,
        });
        ChatLlmService::mark_tool_protocol_repair_checkpoint(
            &mut tool_state,
            true,
            None,
            &[LLMToolCall {
                id: "call".to_string(),
                name: "lookup".to_string(),
                arguments: json!({}),
            }],
        );
        assert!(matches!(
            tool_state,
            Some(AssistantProviderState::OpenaiResponses {
                tool_protocol_repair_checkpoint: false,
                ..
            })
        ));

        let legacy: AssistantProviderState = serde_json::from_value(json!({
            "provider": "openai_responses",
            "response_id": "legacy-response"
        }))
        .expect("legacy provider state");
        assert!(matches!(
            legacy,
            AssistantProviderState::OpenaiResponses {
                tool_protocol_repair_checkpoint: false,
                ..
            }
        ));

        let encoded = serde_json::to_value(&state).expect("checkpoint serializes");
        assert_eq!(
            encoded
                .get("tool_protocol_repair_checkpoint")
                .and_then(Value::as_bool),
            Some(true)
        );
        let restored: Option<AssistantProviderState> =
            serde_json::from_value(encoded).expect("checkpoint deserializes");
        assert!(matches!(
            restored,
            Some(AssistantProviderState::OpenaiResponses {
                tool_protocol_repair_checkpoint: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn provider_history_repair_keeps_complete_calls_from_a_partial_batch() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                ..Default::default()
            },
            "openai-chat",
        );
        let kept_value = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "value": "kept" }
        });
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Checking both.".to_string()),
                tool_calls: vec![
                    StoredToolCall {
                        id: "missing-result".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "missing" }),
                    },
                    StoredToolCall {
                        id: "complete-call".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "complete" }),
                    },
                ],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "complete-call".to_string(),
                tool_name: Some("lookup".to_string()),
                projection: projected_result("complete-call", kept_value.clone()),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "continue".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-chat"))
            .await;

        let tool_call_ids = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                RouterContentBlock::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let tool_result_ids = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                RouterContentBlock::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_call_ids, vec!["complete-call"]);
        assert_eq!(tool_result_ids, vec!["complete-call"]);
        assert!(request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    RouterContentBlock::Text { text }
                        if text.contains("A prior tool request was interrupted")
                )
            })
        }));
    }

    #[tokio::test]
    async fn provider_history_repair_drops_standalone_tool_results() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                ..Default::default()
            },
            "openai-chat",
        );
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "hello".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Hello.".to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "unknown-call".to_string(),
                tool_name: Some("lookup".to_string()),
                content: "must not be replayed".to_string(),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "next".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-chat"))
            .await;

        assert!(request.messages.iter().all(|message| {
            message.content.iter().all(|block| {
                !matches!(
                    block,
                    RouterContentBlock::ToolResult { tool_call_id, .. }
                        if tool_call_id == "unknown-call"
                )
            })
        }));
    }

    #[tokio::test]
    async fn stale_task_neutralization_keeps_non_task_pair_and_current_turn_order() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("chat"),
                )])),
                ..Default::default()
            },
            "openai-chat",
        );
        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "do the work".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Starting both operations.".to_string()),
                tool_calls: vec![
                    StoredToolCall {
                        id: "task-call".to_string(),
                        name: "create_task".to_string(),
                        arguments: json!({ "description": "prepare the report" }),
                    },
                    StoredToolCall {
                        id: "lookup-call".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({ "query": "status" }),
                    },
                ],
                provider_state: Some(AssistantProviderState::AnthropicMessages {
                    content: vec![json!({ "type": "text", "text": "raw state" })],
                }),
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "task-call".to_string(),
                tool_name: Some("create_task".to_string()),
                content: "created".to_string(),
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "lookup-call".to_string(),
                tool_name: Some("lookup".to_string()),
                content: "ready".to_string(),
            },
            ChatLlmTranscriptEntry::UserText {
                text: "what happened?".to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-chat"))
            .await;
        let blocks = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .collect::<Vec<_>>();
        assert!(blocks.iter().all(|block| {
            !matches!(block, RouterContentBlock::ToolCall { id, .. } if id == "task-call")
                && !matches!(block, RouterContentBlock::ToolResult { tool_call_id, .. } if tool_call_id == "task-call")
        }));
        assert!(blocks.iter().any(|block| {
            matches!(block, RouterContentBlock::Text { text } if text.contains("Already handled earlier") && text.contains("create_task"))
        }));
        assert!(blocks.iter().any(|block| {
            matches!(block, RouterContentBlock::ToolCall { id, .. } if id == "lookup-call")
        }));
        assert!(blocks.iter().any(|block| {
            matches!(block, RouterContentBlock::ToolResult { tool_call_id, .. } if tool_call_id == "lookup-call")
        }));
        assert!(matches!(
            request.messages.last().map(|message| &message.content[..]),
            Some([RouterContentBlock::Text { text }]) if text == "what happened?"
        ));
    }

    #[tokio::test]
    async fn gemini_missing_native_turn_state_flattens_both_sides_instead_of_orphaning_result() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::Gemini,
                model: "gemini-2.5-flash".to_string(),
                supports_tool_calling: Some(true),
                ..Default::default()
            },
            "gemini",
        );
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("I checked memory.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "call-a".to_string(),
                    name: "memory_search".to_string(),
                    arguments: json!({ "query": "birthday" }),
                }],
                // A compacted/legacy turn can lack Gemini's required thought
                // signature. Replaying only its result as functionResponse
                // would create an orphan and can be rejected by Gemini.
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: projected_result("call-a", json!({ "data": { "value": "May 8" } })),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("gemini"))
            .await;

        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[1].role, MessageRole::Assistant);
        assert!(!request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    RouterContentBlock::ToolCall { .. } | RouterContentBlock::ToolResult { .. }
                )
            })
        }));
        let rendered = request.messages[1]
            .content
            .iter()
            .filter_map(|block| match block {
                RouterContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("memory_search"));
        assert!(rendered.contains("May 8"));
    }

    #[tokio::test]
    async fn invalid_projected_transcript_fails_closed_without_replaying_foreign_evidence() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let secret = "evidence-for-another-call";
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "call-a".to_string(),
                    name: "memory_search".to_string(),
                    arguments: json!({}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("memory_search".to_string()),
                projection: projected_result("call-other", json!({ "data": { "secret": secret } })),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;

        let RouterContentBlock::ToolResult {
            tool_call_id,
            content,
        } = &request.messages[2].content[0]
        else {
            panic!("expected typed tool result failure")
        };
        assert_eq!(tool_call_id, "call-a");
        assert_eq!(
            content["outcome"]["code"],
            "invalid_projected_tool_result_transcript"
        );
        assert!(!content.to_string().contains(secret));
    }

    #[tokio::test]
    async fn projected_provider_history_runs_the_second_outbound_secret_guard() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "call-a".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: "call-a".to_string(),
                tool_name: Some("lookup".to_string()),
                projection: projected_result(
                    "call-a",
                    json!({
                        "authorization": "Bearer escaped-secret",
                        "spouse_birthday": "May 8"
                    }),
                ),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;
        let RouterContentBlock::ToolResult { content, .. } = &request.messages[2].content[0] else {
            panic!("expected projected tool result")
        };

        assert_eq!(content["authorization"], "Bearer [REDACTED]");
        assert_eq!(content["spouse_birthday"], "May 8");
        assert!(!content.to_string().contains("escaped-secret"));
    }

    #[tokio::test]
    async fn legacy_provider_history_also_runs_the_outbound_secret_guard() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("responses"),
                )])),
                ..Default::default()
            },
            "openai-responses",
        );
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "legacy-call".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "legacy-call".to_string(),
                tool_name: Some("lookup".to_string()),
                content: r#"{"authorization":"Bearer escaped-legacy-secret","value":"May 8"}"#
                    .to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-responses"))
            .await;
        let RouterContentBlock::ToolResult { content, .. } = &request.messages[2].content[0] else {
            panic!("expected legacy tool result")
        };

        assert!(!content.to_string().contains("escaped-legacy-secret"));
        assert!(content.to_string().contains("May 8"));
    }

    #[tokio::test]
    async fn legacy_string_tool_result_hydrates_without_double_stringification() {
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                supports_tool_calling: Some(true),
                additional_params: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    json!("chat"),
                )])),
                ..Default::default()
            },
            "openai-chat",
        );
        let legacy_json_text = r#"{"status":"ok","value":"exact"}"#;
        let history = vec![
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "legacy-call".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "legacy-call".to_string(),
                tool_name: Some("lookup".to_string()),
                content: legacy_json_text.to_string(),
            },
        ];

        let request = service
            .build_request("system", &history, None, Some("openai-chat"))
            .await;

        assert!(matches!(
            &request.messages[2].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, content: Value::String(content) }]
                if tool_call_id == "legacy-call" && content == legacy_json_text
        ));
    }

    #[tokio::test]
    async fn anthropic_replays_rich_tool_results_natively() {
        let temp_dir = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet".to_string(),
                supports_vision: Some(true),
                ..Default::default()
            },
            "anthropic-chat",
        )
        .with_workspace_layout(workspace.clone());

        let session_id = "session-4".to_string();
        let principal = "user-1".to_string();
        let scope = "workspace-a".to_string();
        workspace
            .ensure_chat_session_workspace(&principal, &scope, &session_id)
            .await
            .expect("chat session workspace");

        let image_path = workspace
            .chat_session_outputs_dir(&principal, &scope, &session_id)
            .join("anthropic-draft.png");
        fs::write(&image_path, [137_u8, 80, 78, 71])
            .await
            .expect("image bytes");

        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "Create a poster.".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("I'll generate a draft.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "toolu_1".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster" }),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "toolu_1".to_string(),
                tool_name: Some("generate_image".to_string()),
                content: vec![
                    TranscriptBlock::Text {
                        text: "Generated a draft poster.".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "anthropic-draft.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: Some("Draft poster".to_string()),
                        },
                    },
                ],
            },
        ];

        let messages = service
            .build_request(
                "system",
                &history,
                Some(&ChatTranscriptContext {
                    session_id,
                    principal,
                    workspace: scope,
                    chat_turn_id: None,
                    user_message_id: None,
                    trace_context: None,
                    provider_attempt_counter: None,
                }),
                Some("anthropic-chat"),
            )
            .await
            .messages;

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[2].role, MessageRole::Assistant);
        assert_eq!(messages[3].role, MessageRole::Tool);

        assert!(matches!(
            &messages[2].content[..],
            [RouterContentBlock::Text { text }, RouterContentBlock::ToolCall { id, name, arguments }]
                if text == "I'll generate a draft."
                    && id == "toolu_1"
                    && name == "generate_image"
                    && arguments == &json!({ "prompt": "poster" })
        ));
        assert!(matches!(
            &messages[3].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, content }]
                if tool_call_id == "toolu_1"
                    && content.get("_magicllm_rich_tool_result").and_then(serde_json::Value::as_bool) == Some(true)
                    && content.get("blocks").and_then(serde_json::Value::as_array).map(|blocks| blocks.len()) == Some(2)
        ));
    }

    #[tokio::test]
    async fn gemini_replays_native_assistant_parts_and_multimodal_tool_results() {
        let temp_dir = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let service = test_service(
            LlmConfig {
                provider: LLMProviderKind::Gemini,
                model: "gemini-3-pro".to_string(),
                supports_vision: Some(true),
                ..Default::default()
            },
            "gemini-chat",
        )
        .with_workspace_layout(workspace.clone());

        let session_id = "session-5".to_string();
        let principal = "user-1".to_string();
        let scope = "workspace-a".to_string();
        workspace
            .ensure_chat_session_workspace(&principal, &scope, &session_id)
            .await
            .expect("chat session workspace");

        let image_path = workspace
            .chat_session_outputs_dir(&principal, &scope, &session_id)
            .join("gemini-draft.png");
        fs::write(&image_path, [137_u8, 80, 78, 71])
            .await
            .expect("image bytes");

        let history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "Create a poster.".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("I'll generate a draft.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "gemini-call-generate_image-0".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster" }),
                }],
                provider_state: Some(AssistantProviderState::Gemini {
                    parts: vec![json!({
                        "functionCall": {
                            "name": "generate_image",
                            "args": { "prompt": "poster" }
                        },
                        "thoughtSignature": "sig-123"
                    })],
                }),
            },
            ChatLlmTranscriptEntry::ToolResultRich {
                tool_call_id: "gemini-call-generate_image-0".to_string(),
                tool_name: Some("generate_image".to_string()),
                content: vec![
                    TranscriptBlock::Text {
                        text: "Generated a draft poster.".to_string(),
                    },
                    TranscriptBlock::ImageFile {
                        image: PromptImageRef {
                            stored_name: "gemini-draft.png".to_string(),
                            mime_type: "image/png".to_string(),
                            label: Some("Gemini draft".to_string()),
                        },
                    },
                ],
            },
        ];

        let messages = service
            .build_request(
                "system",
                &history,
                Some(&ChatTranscriptContext {
                    session_id,
                    principal,
                    workspace: scope,
                    chat_turn_id: None,
                    user_message_id: None,
                    trace_context: None,
                    provider_attempt_counter: None,
                }),
                Some("gemini-chat"),
            )
            .await
            .messages;

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[2].role, MessageRole::Assistant);
        assert!(matches!(
            &messages[2].content[..],
            [RouterContentBlock::Json { value }]
                if value.get("_magicllm_gemini_raw_parts").and_then(Value::as_array).map(|parts| parts.len()) == Some(1)
        ));
        assert!(matches!(
            &messages[3].content[..],
            [RouterContentBlock::ToolResult { tool_call_id, content }]
                if tool_call_id == "gemini-call-generate_image-0"
                    && content.get("_magicllm_rich_tool_result").and_then(Value::as_bool) == Some(true)
        ));
    }
}
