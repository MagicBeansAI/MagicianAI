//! The dictation/audio-notes/live-call UX seam (plan workstream 3.5,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Dictation, audio notes, and the live-call voice UX are client-side
//! product features riding the media rails (Layer 1: provider chains,
//! preferences, the audio runtime config, the notes provider). This tree is
//! where their BACKEND-owned product decisions live as a seam-registered
//! module — not an app package, and the rails themselves are untouched: no
//! provider, preference, runtime-config, or notes-store path changed.
//!
//! What moved here in 3.5, behavior-identical:
//! - [`dictation`] — the dictation-mode policy behind `/stt/transcribe`,
//!   `/tts/synthesize` and the dictation surface endpoints: the explicit
//!   provider override sentinels (`auto`/`default` mean unset), the
//!   unknown-explicit rejection, the availability gate for explicit picks,
//!   and the `AudioSurface::Dictation` profile resolution
//!   (`resolve_dictation_stt_provider` / `resolve_dictation_tts_provider`,
//!   extracted verbatim from `media_api` with the rails pieces passed in
//!   instead of `&MediaApi`; `media_api` keeps same-signature delegations).
//! - [`session_controls`] — the voice session-control policy extracted
//!   verbatim from `voice_control_handler.rs`: the fail-closed surface
//!   attribution (`voice_surface_derivation`), the per-client guided-voice
//!   capability shaping, the local-transcript / manual-turn descriptor
//!   policy, the request-field parsing for turn detection / voice prefix /
//!   screen lock, and the spoken guided-flow rejection copy.
//!
//! Compat shims: `voice_control_handler` and `media_api` re-import every
//! moved name, so all call sites and pre-existing test suites compile
//! unedited. The audio-notes ARCHIVE policy (dated-page layout, Markdown
//! contract, listing order, receipt projection) moved lib-side to
//! `magician_v2::audio_notes_seam` beside the media seam, with the notes
//! provider re-importing under the same names (the plan-2.3
//! `notes_projection` pattern).
//!
//! What did not move (Layer 1 stays Layer 1):
//! - The provider chains, preferences store, audio runtime config, and
//!   realtime session registry stay `magician_media::media_rails` /
//!   `media_seam`; nothing here registers a provider or writes settings.
//! - The voice control actor, orchestrator wiring, and WebSocket pump stay
//!   in `voice_control_handler.rs`; this seam owns only the decisions a
//!   session mints from a registration or request, not the session state.

pub mod dictation;
pub mod session_controls;

pub use dictation::{
    ensure_explicit_audio_provider_available, explicit_audio_provider_override,
    resolve_dictation_stt_provider, resolve_dictation_tts_provider,
};
pub use session_controls::{
    descriptor_requests_local_transcript, descriptor_uses_manual_turns, guided_voice_capabilities,
    is_magios_voice_client, locked_guided_flow_message, requested_realtime_turn_detection,
    requested_screen_locked, requested_voice_prefix_override, unsupported_guided_flow_message,
    voice_source_surface, voice_surface_derivation, GuidedVoiceCapabilities,
    VoiceSurfaceDerivation,
};
