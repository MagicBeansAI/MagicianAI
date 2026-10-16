//! Lane-admission oracles (plan workstream 0.3, corpus 1 —
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Pins the publicly-callable admission surface of the four chat lanes
//! (Tutor, App Copilot, Brainstorm, VibeDev) exactly as it behaves today,
//! so the 1.2 lane-seam refactor can be proven behavior-identical without
//! editing these assertions. Enum results are checked with `matches!`
//! deliberately: these oracles must not depend on derive details.
//!
//! The admission logic that is NOT publicly reachable — tool allowlists
//! (`TUTOR_CHAT_RUNTIME_TOOL_NAMES`, `APP_COPILOT_QUICK_CHAT_TOOLS`,
//! `tutor_quick_allowed_*`, `retain_tutor_runtime_tools_for_current_turn`),
//! prompt-injection decisions (`should_inject_personal_tutor_instructions`,
//! `tutor_or_copilot_runtime_instruction`), and capture freshness
//! (`app_copilot_capture_timestamp_is_fresh`) — is private inside
//! `chat/service.rs` and already covered by its in-module `#[cfg(test)]`
//! suites; the seam refactor keeps those green unedited (plan principle 2).
//! The brainstorm session-auth block moved with the 1.2 seam refactor into
//! `chat/lane_seam.rs` (`authenticated_product_lane`), where its in-module
//! tests cover it; this file pins the parser grammar, wire constants, and
//! lane classifiers.

use magician::magician_v2::agents::{FeatureMode, InvocationSurface};
use magician::magician_v2::chat::invoke_grammar::parse_vibedev_rail_invocation;
use magician::magician_v2::chat::lane_seam::{
    THINKING_MAP_PRODUCT_SOURCE_KEY, THINKING_MAP_THREAD_ID,
};
use magician::magician_v2::execution::agentic::policy_snapshot::{
    feature_agent_id, feature_surface_is_authorized, parse_leading_feature_invocation,
    parse_leading_feature_marker,
};
use magician::magician_v2::tutor::{
    classify_app_copilot_turn_mode, classify_tutor_or_app_copilot_canvas_mode_for_lane,
    classify_tutor_or_app_copilot_turn_mode_for_lane, classify_tutor_turn_mode,
    is_app_copilot_prompt, TutorCanvasMode, TutorRunMode, APP_COPILOT_MARKER_INVOKE_WORDS,
    TUTOR_MARKER_INVOKE_WORDS,
};

// --- Typed marker parsing (the authorization-side grammar) -----------------

#[test]
fn leading_feature_marker_accepts_exactly_the_pinned_spellings() {
    assert!(matches!(
        parse_leading_feature_marker("@tutor explain this"),
        FeatureMode::Tutor
    ));
    assert!(matches!(
        parse_leading_feature_marker("@tutur explain this"),
        FeatureMode::Tutor
    ));
    // Separators after the marker are fine — the parser splits on
    // whitespace, ':', and ','.
    assert!(matches!(
        parse_leading_feature_marker("@tutor, explain this"),
        FeatureMode::Tutor
    ));
    // Longer words are NOT the marker.
    assert!(matches!(
        parse_leading_feature_marker("@tutorial about arrays"),
        FeatureMode::None
    ));
    // Every copilot spelling parses, including the underscore form the
    // marker-word table carries alongside the parser.
    for text in [
        "@copilot click save",
        "@appcopilot click save",
        "@app-copilot click save",
        "@app_copilot click save",
    ] {
        assert!(
            matches!(parse_leading_feature_marker(text), FeatureMode::AppCopilot),
            "`{text}` must parse as AppCopilot"
        );
    }
    assert!(matches!(
        parse_leading_feature_marker("@brainstorm idea"),
        FeatureMode::Brainstorm
    ));
    // Markers must LEAD the turn; later mentions are not invocation.
    assert!(matches!(
        parse_leading_feature_marker("can you use @tutor for this"),
        FeatureMode::None
    ));
}

#[test]
fn leading_feature_invocation_adds_the_spoken_forms() {
    assert!(matches!(
        parse_leading_feature_invocation("hey tutor explain recursion"),
        FeatureMode::Tutor
    ));
    assert!(matches!(
        parse_leading_feature_invocation("Hey, app copilot click save"),
        FeatureMode::AppCopilot
    ));
    assert!(matches!(
        parse_leading_feature_invocation("hey copilot click save"),
        FeatureMode::AppCopilot
    ));
    // The spoken form must be followed by a boundary: "hey tutoring" is a
    // different word and must not invoke anything.
    assert!(matches!(
        parse_leading_feature_invocation("hey tutoring is hard"),
        FeatureMode::None
    ));
}

// --- Feature/surface authorization matrix ----------------------------------

#[test]
fn feature_surface_authorization_matrix_is_pinned() {
    let cases: &[(FeatureMode, InvocationSurface, bool)] = &[
        // No feature claim: ordinary surfaces allowed, product surfaces not.
        (FeatureMode::None, InvocationSurface::Chat, true),
        (FeatureMode::None, InvocationSurface::ContextualAssist, true),
        (FeatureMode::None, InvocationSurface::ThinkingMap, false),
        (FeatureMode::None, InvocationSurface::Tutor, false),
        (FeatureMode::None, InvocationSurface::AppCopilot, false),
        // Each conversational product lane is confined to its own surface.
        (FeatureMode::Tutor, InvocationSurface::Tutor, true),
        (FeatureMode::Tutor, InvocationSurface::Chat, false),
        (FeatureMode::AppCopilot, InvocationSurface::AppCopilot, true),
        (FeatureMode::AppCopilot, InvocationSurface::Chat, false),
        // Brainstorm travels on the thinking-map surface only.
        (
            FeatureMode::Brainstorm,
            InvocationSurface::ThinkingMap,
            true,
        ),
        (FeatureMode::Brainstorm, InvocationSurface::Chat, false),
        // VibeDev is a chat/voice rail and must not borrow visual surfaces.
        (FeatureMode::Vibedev, InvocationSurface::Chat, true),
        (FeatureMode::Vibedev, InvocationSurface::RealtimeVoice, true),
        (FeatureMode::Vibedev, InvocationSurface::Tutor, false),
    ];
    for (mode, surface, expected) in cases {
        assert_eq!(
            feature_surface_is_authorized(*mode, *surface),
            *expected,
            "authorization changed for ({mode:?}, {surface:?})"
        );
    }
}

#[test]
fn feature_agent_binding_is_brainstorm_only() {
    assert_eq!(
        feature_agent_id(FeatureMode::Brainstorm),
        Some("brainstorm-facilitator")
    );
    assert_eq!(feature_agent_id(FeatureMode::Tutor), None);
    assert_eq!(feature_agent_id(FeatureMode::AppCopilot), None);
    assert_eq!(feature_agent_id(FeatureMode::Vibedev), None);
    assert_eq!(feature_agent_id(FeatureMode::None), None);
}

// --- Brainstorm wire constants ----------------------------------------------

#[test]
fn brainstorm_thread_and_source_constants_are_pinned() {
    assert_eq!(THINKING_MAP_THREAD_ID, "brainstorming");
    assert_eq!(THINKING_MAP_PRODUCT_SOURCE_KEY, "app:ios:thinking-map");
}

// --- Tutor / App Copilot lane classification --------------------------------

#[test]
fn app_copilot_prompt_detection_pins_the_legacy_substring_semantics() {
    // Markers anywhere in the text match (legacy substring behavior — the
    // boundary-safe parser above is the authorization grammar; this is the
    // compat adapter the seam refactor must preserve bit-for-bit).
    assert!(is_app_copilot_prompt("@copilot click save"));
    assert!(is_app_copilot_prompt("please use @copilot here"));
    assert!(is_app_copilot_prompt("@app-copilot click save"));
    assert!(is_app_copilot_prompt("@appcopilot click save"));
    // The underscore form now rides the marker-word table too, matching the
    // authorization parser and the web composer regex.
    assert!(is_app_copilot_prompt("@app_copilot click save"));
    // Spoken windows.
    assert!(is_app_copilot_prompt("hey copilot click save"));
    assert!(is_app_copilot_prompt("hey app copilot click save"));
    // Tutor invokes and plain text do not.
    assert!(!is_app_copilot_prompt("@tutor explain this"));
    assert!(!is_app_copilot_prompt("hey tutor explain this"));
    assert!(!is_app_copilot_prompt("how do I save this file"));
}

#[test]
fn marker_word_tables_are_pinned() {
    assert_eq!(TUTOR_MARKER_INVOKE_WORDS, ["@tutor", "@tutur"]);
    assert_eq!(
        APP_COPILOT_MARKER_INVOKE_WORDS,
        ["@copilot", "@appcopilot", "@app-copilot", "@app_copilot"]
    );
}

#[test]
fn lane_classification_routes_on_the_authenticated_lane_not_the_text() {
    // The lane boolean is policy-bearing: with app_copilot_lane=true the
    // text is classified by the copilot rules even if it reads like a
    // tutor concept turn, and vice versa.
    assert!(matches!(
        classify_tutor_or_app_copilot_turn_mode_for_lane("explain quantum tunneling", true),
        TutorRunMode::GuidedAction
    ));
    assert!(matches!(
        classify_tutor_or_app_copilot_turn_mode_for_lane("click the save button", false),
        TutorRunMode::ExplainOnly
    ));
    // Copilot without demo-and-cleanup phrasing is GuidedAction.
    assert!(matches!(
        classify_app_copilot_turn_mode("click the save button"),
        TutorRunMode::GuidedAction
    ));
    // Tutor with no concept content is ExplainOnly.
    assert!(matches!(
        classify_tutor_turn_mode(""),
        TutorRunMode::ExplainOnly
    ));
}

#[test]
fn canvas_mode_is_screen_bound_for_copilot_and_source_bound_for_tutor() {
    // App Copilot is always screen-bound, with or without a visual source.
    assert!(matches!(
        classify_tutor_or_app_copilot_canvas_mode_for_lane("anything", false, true),
        TutorCanvasMode::ScreenOverlay
    ));
    assert!(matches!(
        classify_tutor_or_app_copilot_canvas_mode_for_lane("anything", true, true),
        TutorCanvasMode::ScreenOverlay
    ));
    // Tutor follows the visual source: blackboard when source-free.
    assert!(matches!(
        classify_tutor_or_app_copilot_canvas_mode_for_lane("anything", false, false),
        TutorCanvasMode::Blackboard
    ));
    assert!(matches!(
        classify_tutor_or_app_copilot_canvas_mode_for_lane("anything", true, false),
        TutorCanvasMode::ScreenOverlay
    ));
}

// --- VibeDev rail admission --------------------------------------------------

#[test]
fn vibedev_rail_invocation_pins_marker_and_discuss_forms() {
    // Typed marker, plain prompt.
    let invocation =
        parse_vibedev_rail_invocation("@vibedev fix the footer").expect("marker invoke parses");
    assert!(!invocation.discuss);
    assert_eq!(invocation.prompt, "fix the footer");

    // #discuss flag in the slot directly after the marker.
    let invocation = parse_vibedev_rail_invocation("@vibedev #discuss should we split this panel")
        .expect("discuss invoke parses");
    assert!(invocation.discuss);
    assert_eq!(invocation.prompt, "should we split this panel");

    // Spoken phrase: "start a vibedev build <prompt>" is a leading invoke.
    let invocation = parse_vibedev_rail_invocation("start a vibedev build fix the footer spacing")
        .expect("spoken build invoke parses");
    assert!(!invocation.discuss);
    assert_eq!(invocation.prompt, "fix the footer spacing");

    // A quoted marker is someone describing the rail, not using it.
    assert!(parse_vibedev_rail_invocation("\"@vibedev\" fix the footer").is_none());
    // Status reports and requests that do not LEAD with the phrase are not
    // invokes — a false positive here starts a real build.
    assert!(parse_vibedev_rail_invocation("the vibedev build is red").is_none());
    assert!(
        parse_vibedev_rail_invocation("can you start a vibedev build for the footer").is_none()
    );
}
