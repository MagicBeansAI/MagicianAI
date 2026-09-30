//! Realtime provider abstraction — bidirectional voice/audio sessions.
//!
//! Realtime is fundamentally different from chat-completion in three
//! ways that justify a separate trait hierarchy:
//!
//! 1. **The session is long-lived.** A chat-completion call is a
//!    request/response pair (possibly streamed). A realtime session
//!    persists across many turns, holds upstream context, and may
//!    outlive several upstream provider sessions via rotation.
//!
//! 2. **Audio topology varies per surface/provider.** Browser OpenAI
//!    Realtime can use direct WebRTC peer-to-peer between browser
//!    and provider — the cheapest possible latency path. Host-native
//!    clients and WebSocket-only providers proxy audio through the
//!    backend. The trait surfaces this via
//!    [`RealtimeAudioTopology`] so orchestration can stay uniform.
//!
//! 3. **Control + audio are different concerns.** Tool calls,
//!    transcripts, and lifecycle events flow through magician for
//!    orchestration. Raw audio bytes don't. Splitting these concerns
//!    cleanly is what lets us keep WebRTC P2P latency for OpenAI
//!    without bleeding provider knowledge into the frontend.
//!
//! Adding a new realtime provider is one new file under
//! `magicllm/src/realtime/`, plus a profile entry in
//! `magician-config.yaml`. No changes to the orchestrator, no
//! changes to the frontend.

pub mod factory;
pub mod gemini;
pub mod openai;
pub mod openai_live;
pub mod provider;
pub mod types;
pub mod voices;

pub use factory::build_realtime_provider;
pub use gemini::{
    gemini_live_model_contract, GeminiLiveModelContract, GeminiLiveProvider, GeminiThinkingLevel,
    GeminiToolResultScheduling, GEMINI_LIVE_DEFAULT_MODEL, GEMINI_LIVE_DEFAULT_VOICE,
    GEMINI_LIVE_DEFAULT_WEBSOCKET_URL, GEMINI_LIVE_PROVIDER_ID,
};
pub use openai::{
    OpenAiRealtimeProvider, OPENAI_REALTIME_DEFAULT_BASE_URL, OPENAI_REALTIME_DEFAULT_MODEL,
    OPENAI_REALTIME_DEFAULT_VOICE, OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL,
};
pub use openai_live::{
    OpenAiLiveProvider, OPENAI_LIVE_DEFAULT_INSTRUCTIONS, OPENAI_LIVE_DEFAULT_MODEL,
    OPENAI_LIVE_DEFAULT_VOICE, OPENAI_LIVE_DEFAULT_WEBSOCKET_URL, OPENAI_LIVE_PROVIDER_ID,
};
pub use provider::RealtimeProvider;
pub use types::{
    AudioStreamChannel, RealtimeAudioControl, RealtimeAudioTopology, RealtimeProviderError,
    RealtimeProviderEvent, RealtimeProviderKind, RealtimeResponseTerminalState,
    RealtimeSessionDescriptor, RealtimeSpeechSegment,
};
pub use voices::{
    canonical_realtime_voice, voice_is_valid_for_realtime_provider, voices_for_realtime_provider,
    RealtimeVoiceChoice,
};
