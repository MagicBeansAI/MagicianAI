//! Voice session-control policy (plan workstream 3.5).
//!
//! Extracted verbatim from `voice_control_handler.rs`: the decisions a
//! voice session mints from its registration and start request — the
//! fail-closed surface attribution, the per-client guided-voice capability
//! shaping, the local-transcript / manual-turn descriptor policy, the
//! request-field parsing for turn detection / voice prefix / screen lock,
//! and the spoken guided-flow rejection copy. The actor, orchestrator
//! wiring, and session state stay in the handler.

use serde_json::Value;

use magician::magician_v2::agents::FeatureMode;
use magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE;
use magician_media::media_rails::SurfaceType;
use magicllm::realtime::{RealtimeAudioTopology, RealtimeProviderKind, RealtimeSessionDescriptor};

/// Why a call resolved the surface it did, alongside the surface itself. Both
/// are minted from ONE match so the reason can never drift from the decision it
/// explains — a reason code that disagreed with the surface would be worse than
/// none, because an operator would trust it.
///
/// Content-free by construction: it names the rule that fired, never anything
/// about the call, the room, or the people in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceSurfaceDerivation {
    pub source_surface: &'static str,
    pub reason: &'static str,
}

pub fn voice_surface_derivation(surface_type: SurfaceType) -> VoiceSurfaceDerivation {
    let reason = match surface_type {
        SurfaceType::TrayMacos | SurfaceType::TrayWindows | SurfaceType::TrayLinux => {
            "registered_tray_client"
        },
        SurfaceType::MascotMacos | SurfaceType::MascotWindows | SurfaceType::MascotLinux => {
            "registered_mascot_client"
        },
        SurfaceType::WebMobile | SurfaceType::WebDesktop => "registered_web_client",
        SurfaceType::EspTerminal => "registered_esp_terminal",
        SurfaceType::MeetingBot => "registered_meeting_bot",
        // The two arms an operator most needs to tell apart: a room BECAUSE it
        // is the meeting bot, versus a room because we could not identify the
        // caller at all. Both resolve the same surface; only the reason
        // distinguishes a working meeting from a misconfigured client.
        SurfaceType::Extension | SurfaceType::Unknown => "unrecognised_registration_fail_closed",
    };
    VoiceSurfaceDerivation {
        source_surface: voice_source_surface(surface_type),
        reason,
    }
}

pub fn voice_source_surface(surface_type: SurfaceType) -> &'static str {
    match surface_type {
        SurfaceType::TrayMacos | SurfaceType::TrayWindows | SurfaceType::TrayLinux => {
            "global_live_ptt"
        },
        SurfaceType::MascotMacos | SurfaceType::MascotWindows | SurfaceType::MascotLinux => {
            "mascot"
        },
        SurfaceType::WebMobile | SurfaceType::WebDesktop => "realtime_voice",
        SurfaceType::EspTerminal => "realtime_voice",
        // The one source-surface string that means "a room is listening". It is
        // produced HERE, from the registered session, never copied from a turn.
        SurfaceType::MeetingBot => MEETING_ROOM_SOURCE_SURFACE,
        // **Unrecognised registrations get the strictest voice-bearing surface.**
        // Every first-party client states what it is (`web_desktop`, `tray_*`,
        // `esp_terminal`, …), so reaching this arm means the caller either did
        // not say or is not one of them — and the fail-closed answer to "who is
        // this?" is "assume a room", not "assume the owner". `Unknown` is the
        // serde default for an omitted field, which is exactly the case that
        // must not silently land on an owner surface. `Extension` is here
        // because the extension registers no media sessions at all today
        // (verified 2026-08-18), so treating it strictly costs nothing and
        // stops a future extension voice path inheriting owner authority by
        // default rather than by decision.
        SurfaceType::Extension | SurfaceType::Unknown => MEETING_ROOM_SOURCE_SURFACE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuidedVoiceCapabilities {
    pub expanded_grammar: bool,
    pub screen_capture: bool,
    pub client_blackboard_handoff: bool,
}

/// Keep command understanding independent from capture capability. Native iOS
/// can run source-free Tutor/Tutor Quick on its own blackboard even though it
/// cannot attach a device screenshot to this server-owned voice connection.
pub fn guided_voice_capabilities(
    surface_type: SurfaceType,
    user_agent: Option<&str>,
) -> GuidedVoiceCapabilities {
    if is_magios_voice_client(user_agent) {
        return GuidedVoiceCapabilities {
            expanded_grammar: true,
            screen_capture: false,
            client_blackboard_handoff: true,
        };
    }
    let web = matches!(
        surface_type,
        SurfaceType::WebMobile | SurfaceType::WebDesktop
    );
    GuidedVoiceCapabilities {
        expanded_grammar: web,
        screen_capture: true,
        client_blackboard_handoff: false,
    }
}

pub fn is_magios_voice_client(user_agent: Option<&str>) -> bool {
    user_agent
        .map(str::trim)
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("magios-ios"))
}

pub fn descriptor_requests_local_transcript(descriptor: &RealtimeSessionDescriptor) -> bool {
    matches!(descriptor.topology, RealtimeAudioTopology::BackendProxied)
        && matches!(descriptor.provider, RealtimeProviderKind::OpenAi)
        && descriptor
            .transcription_model
            .as_deref()
            .is_some_and(|model| model.trim().eq_ignore_ascii_case("local"))
}

pub fn descriptor_uses_manual_turns(descriptor: &RealtimeSessionDescriptor) -> bool {
    descriptor
        .turn_detection_mode
        .as_deref()
        .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("none"))
}

/// Resolve the per-call input boundary for a native realtime session.
///
/// `voice_mode` selects a provider family (`hands_free` is the local cascaded
/// FluidAudio path), while `turn_boundary` selects continuous VAD vs manual PTT
/// *within* a realtime provider. Keeping these axes separate lets native clients
/// offer hands-free GPT Realtime without accidentally selecting the cascade.
pub fn requested_realtime_turn_detection(
    body: &Value,
    cascaded_hands_free: bool,
) -> Option<String> {
    if cascaded_hands_free {
        return None;
    }
    match body
        .get("turn_boundary")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("server_vad" | "vad" | "hands_free") => Some("server_vad".to_string()),
        Some("push_to_talk" | "ptt" | "manual" | "none") => Some("none".to_string()),
        _ => None,
    }
}

pub fn requested_voice_prefix_override(body: &Value) -> Option<bool> {
    body.get("require_voice_prefix").and_then(Value::as_bool)
}

pub fn requested_screen_locked(body: &Value) -> Option<bool> {
    body.get("screen_locked")
        .and_then(Value::as_bool)
        .or_else(|| body.get("locked").and_then(Value::as_bool))
}

/// Locked-screen copy (Batch 7 F12, declined to a registry fold). The copy
/// keys directly on the feature mode. The lane registry is deliberately NOT
/// consulted: `admission_surface()` is only meaningful for a lane that
/// matched through admission (the seam's own invariant), and this table is
/// a single App Copilot arm plus the historical Tutor else — a future lane
/// gets the Tutor sentence until someone adds its arm in this client
/// module. Both strings are byte-identical to the originals and pinned by
/// the tests below.
pub fn locked_guided_flow_message(feature_mode: FeatureMode) -> &'static str {
    if feature_mode == FeatureMode::AppCopilot {
        "Please unlock your screen to use App Copilot."
    } else {
        "Please unlock your screen to use Tutor."
    }
}

pub fn unsupported_guided_flow_message(error: &str) -> &'static str {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("tutor screen capture is not available on this device") {
        "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
    } else if normalized.contains("app copilot")
        && normalized.contains("not available on this device")
    {
        "App Copilot isn't available on this device yet."
    } else if normalized.contains("screen capture") || normalized.contains("host screen") {
        "I couldn't capture the screen, so I didn't start that guided flow. Please check Screen Recording permission or use the device-local capture flow."
    } else {
        "I couldn't start that guided session. Nothing was changed; please try again."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognised_registrations_fail_closed_to_the_room_surface() {
        for surface_type in [SurfaceType::Unknown, SurfaceType::Extension] {
            let derivation = voice_surface_derivation(surface_type);
            assert_eq!(derivation.source_surface, MEETING_ROOM_SOURCE_SURFACE);
            assert_eq!(derivation.reason, "unrecognised_registration_fail_closed");
        }
        // Every first-party client stays on an owner surface with its own
        // reason code.
        let tray = voice_surface_derivation(SurfaceType::TrayMacos);
        assert_eq!(tray.source_surface, "global_live_ptt");
        assert_eq!(tray.reason, "registered_tray_client");
        let web = voice_surface_derivation(SurfaceType::WebDesktop);
        assert_eq!(web.source_surface, "realtime_voice");
        assert_eq!(web.reason, "registered_web_client");
        let meeting = voice_surface_derivation(SurfaceType::MeetingBot);
        assert_eq!(meeting.source_surface, MEETING_ROOM_SOURCE_SURFACE);
        assert_eq!(meeting.reason, "registered_meeting_bot");
    }

    #[test]
    fn magios_client_gets_blackboard_handoff_without_server_capture() {
        let capabilities =
            guided_voice_capabilities(SurfaceType::WebMobile, Some("MagIOS-iOS/1.2"));
        assert!(capabilities.expanded_grammar);
        assert!(!capabilities.screen_capture);
        assert!(capabilities.client_blackboard_handoff);

        let web = guided_voice_capabilities(SurfaceType::WebDesktop, None);
        assert!(web.expanded_grammar);
        assert!(web.screen_capture);
        assert!(!web.client_blackboard_handoff);

        let tray = guided_voice_capabilities(SurfaceType::TrayMacos, None);
        assert!(!tray.expanded_grammar);
        assert!(tray.screen_capture);
        assert!(!tray.client_blackboard_handoff);
    }

    #[test]
    fn turn_boundary_aliases_map_onto_provider_modes_and_hands_free_wins() {
        let body = serde_json::json!({"turn_boundary": " VAD "});
        assert_eq!(
            requested_realtime_turn_detection(&body, false),
            Some("server_vad".to_string())
        );
        let ptt = serde_json::json!({"turn_boundary": "ptt"});
        assert_eq!(
            requested_realtime_turn_detection(&ptt, false),
            Some("none".to_string())
        );
        // Cascaded hands-free owns the input boundary; no realtime turn
        // detection is requested.
        assert_eq!(requested_realtime_turn_detection(&ptt, true), None);
        assert_eq!(
            requested_realtime_turn_detection(&serde_json::json!({}), false),
            None
        );
    }

    #[test]
    fn screen_lock_reads_both_spellings_and_rejections_have_stable_copy() {
        let locked = serde_json::json!({"screen_locked": true});
        let alias = serde_json::json!({"locked": true});
        assert_eq!(requested_screen_locked(&locked), Some(true));
        assert_eq!(requested_screen_locked(&alias), Some(true));
        assert_eq!(
            requested_voice_prefix_override(&serde_json::json!({"require_voice_prefix": true})),
            Some(true)
        );

        assert_eq!(
            locked_guided_flow_message(FeatureMode::AppCopilot),
            "Please unlock your screen to use App Copilot."
        );
        assert_eq!(
            locked_guided_flow_message(FeatureMode::Tutor),
            "Please unlock your screen to use Tutor."
        );
        assert_eq!(
            unsupported_guided_flow_message("Tutor screen capture is not available on this device"),
            "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
        );
        assert_eq!(
            unsupported_guided_flow_message("provider exploded"),
            "I couldn't start that guided session. Nothing was changed; please try again."
        );
    }

    /// Batch 7 F12: the locked-screen copy derives from the chat lane
    /// registry — App Copilot's lane surface selects the App Copilot
    /// sentence; Tutor, Brainstorm, and VibeDev (nominal `Chat` surface) all
    /// keep the Tutor sentence, as does the unregistered `None` default
    /// (the historical else-branch). Pin all five modes so the fold's
    /// byte-identical strings stay pinned.
    #[test]
    fn locked_guided_flow_copy_keys_on_the_feature_mode() {
        assert_eq!(
            locked_guided_flow_message(FeatureMode::AppCopilot),
            "Please unlock your screen to use App Copilot."
        );
        for mode in [
            FeatureMode::Tutor,
            FeatureMode::None,
            FeatureMode::Brainstorm,
            FeatureMode::Vibedev,
        ] {
            assert_eq!(
                locked_guided_flow_message(mode),
                "Please unlock your screen to use Tutor.",
                "{mode:?} keeps the default Tutor copy"
            );
        }
    }
}
