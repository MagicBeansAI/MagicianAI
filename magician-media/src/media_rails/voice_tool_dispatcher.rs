//! Voice tool dispatch — wire types only.
//!
//! Voice-as-chat-agent (Phase A1) routes realtime function calls
//! through `ChatService::dispatch_external_tool_call`, which uses the
//! same private dispatcher the text chat agent uses. The standalone
//! `dispatch_voice_tool` function that lived here is gone (Phase A6
//! cleanup); only the wire-shape types and the default-thread
//! constant remain — they're produced by `VoiceOrchestrator` and
//! consumed by the control WS actor as the `tool.result` envelope.
//!
//! Pre-A1 history of this module:
//!   - It hosted a 3-tool dispatcher (create_task / get_task_status /
//!     stop_task) that translated realtime function calls to the
//!     `ArtifactV2Service` task API directly. Voice's surface has
//!     since unified with the chat agent's so that path is gone.

use serde::Serialize;

use magician::magician_v2::apps::boundary::AppRealtimeVoiceDeliveryFence;
use magician::magician_v2::chat::service::ExternalToolCatalogUpdate;

/// Default thread voice tasks land on when the voice session isn't
/// linked to a specific thread. Voice control handler reads this on
/// `session.start` when the frontend payload doesn't supply a
/// `ui_thread_id`.
pub const DEFAULT_VOICE_THREAD: &str = "general";

/// Response returned from `VoiceOrchestrator::dispatch_tool` and
/// forwarded by the control WS actor to the frontend as the
/// `tool.result` envelope. The frontend then writes it back to the
/// realtime peer in the provider-native function-output format.
#[derive(Debug, Serialize)]
pub struct VoiceToolDispatchResponse {
    pub tool_name: String,
    pub call_id: String,
    /// Short, speakable summary of the result. This is duplicated
    /// inside `output` so the realtime model receives a structural
    /// "say this, do not read the full payload" contract, and also
    /// surfaced on the WS envelope for UI/debug consumers.
    pub voice_summary: String,
    /// JSON-encoded output string the frontend transport sends back
    /// to the model via the provider-specific tool-output channel
    /// (OpenAI: `conversation.item.create` with
    /// `type: 'function_call_output'`; Gemini: `toolResponse`).
    pub output: String,
    /// `ok` = call dispatched successfully (including the synthetic
    /// "queued" reply when the time-box expired). `error` = arguments
    /// invalid / underlying dispatcher errored.
    pub status: VoiceToolDispatchStatus,
    /// Present only when `tool_search(select:...)` prepared a new catalog.
    /// The control rail must install and acknowledge this update before it
    /// forwards `output` to the realtime model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_update: Option<ExternalToolCatalogUpdate>,
    /// Internal durable-transcript payload. Catalog-changing calls defer
    /// appending their result until the provider acknowledges the matching
    /// catalog; this field is never part of the websocket wire contract.
    #[serde(skip)]
    pub projected_result:
        Option<magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
    /// Host-only proof checked by the control actor immediately before a
    /// governed result is written to the backend-proxied provider channel.
    #[serde(skip)]
    pub provider_delivery_fence: Option<AppRealtimeVoiceDeliveryFence>,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum VoiceToolDispatchStatus {
    Ok,
    Error,
}
