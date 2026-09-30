//! Invoke-grammar agreement test (plan workstream 0.4; extended by 1.2,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! The contract: `chat::invoke_grammar` is the single source of the
//! lane-trigger grammar, and six consumers must agree with it — the five
//! deliberate copies (the backend rail, the web composer, the web voice
//! flow, iOS, and Android) plus the published server catalog. This test
//! fails the standard gates the moment any of them stops agreeing,
//! instead of surfacing as a client that silently stops invoking its
//! lane.
//!
//! Both directions are pinned. Copies ↔ backend: exact-substring
//! assertions against each copy's working-tree source hold every copy to
//! the backend's token set. Catalog ⊆ accepted: the backend PUBLISHES its
//! grammar — `GET /api/magician/v2/chat/invoke-grammar` serves
//! `chat::invoke_catalog::invoke_grammar_catalog()`, generated from the
//! parser constants, so the sixth copy that could drift no longer exists
//! (the wire is the backend's own constants serialized). The catalog
//! tests below call that same builder (no live server) and assert its
//! published content equals the parser vocabularies, both by value
//! (against the exported constants) and by behavior (every published
//! marker/phrase is run through the actual parsers).
//!
//! Assertions are exact-substring presences verified against the working
//! tree on 2026-08-26. `include_str!` also fails the *build* if a grammar
//! file moves, which is exactly the tripwire wanted here. Deliberate
//! per-client divergences are encoded per file — e.g. the iOS *composer*
//! regex intentionally does not parse `@copilot` (its voice flow does, in
//! order to reject App Copilot client-side), so copilot tokens are
//! asserted only where each platform actually recognizes them.

use magician::magician_v2::agents::FeatureMode;
use magician::magician_v2::chat::invoke_catalog::{
    invoke_grammar_catalog, APP_COPILOT_SPOKEN_PREFIX_WORD, BRAINSTORM_MARKER_INVOKE_WORDS,
    INVOKE_GRAMMAR_CATALOG_VERSION, SPOKEN_WAKE_WORD, VIBEDEV_TWO_TOKEN_SUBJECT,
};
use magician::magician_v2::chat::invoke_grammar::{
    parse_vibedev_rail_invocation, VIBEDEV_DISCUSS_FLAG, VIBEDEV_MARKER_INVOKE_WORDS,
};
use magician::magician_v2::execution::agentic::policy_snapshot::parse_leading_feature_invocation;
use magician::magician_v2::tutor::{
    APP_COPILOT_MARKER_INVOKE_WORDS, TUTOR_MARKER_INVOKE_WORDS, TUTOR_QUICK_FLAG,
};

const BACKEND_TUTOR_RAIL: &str = include_str!("../src/magician_v2/tutor.rs");
const BACKEND_INVOKE_GRAMMAR: &str = include_str!("../src/magician_v2/chat/invoke_grammar.rs");
const BACKEND_POLICY_SNAPSHOT: &str =
    include_str!("../src/magician_v2/execution/agentic/policy_snapshot.rs");
const WEB_CHAT_PANEL: &str =
    include_str!("../../ui/unified-ui/src/lib/magician/chat/ChatPanel.svelte");
const WEB_VOICE_FLOW: &str = include_str!("../../ui/unified-ui/src/lib/media/voice/guidedFlow.ts");
const IOS_TUTOR_INVOKE: &str = include_str!("../../magios/Shared/TutorInvoke.swift");
const IOS_MENTION_CATALOG: &str = include_str!("../../magios/Shared/MentionCatalog.swift");
const ANDROID_TUTOR_INVOKE: &str = include_str!(
    "../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/chat/TutorInvoke.kt"
);
const ANDROID_BRAINSTORM_INVOKE: &str = include_str!(
    "../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/chat/BrainstormInvoke.kt"
);

/// Backend tutor/copilot marker vocabulary — the authoritative spelling
/// set every other copy agrees with.
#[test]
fn backend_rail_grammar_is_pinned() {
    // `@tutor` / `@tutur` marker words (typo-tolerant spelling is
    // deliberate and every client port carries the same tolerance).
    for token in [
        "TUTOR_MARKER_INVOKE_WORDS",
        "\"@tutor\", \"@tutur\"",
        // Spoken-command arms inside the voice-turn parser.
        "\"tutor\" | \"tutur\" | \"@tutor\" | \"@tutur\"",
    ] {
        assert!(
            BACKEND_TUTOR_RAIL.contains(token),
            "tutor.rs no longer pins `{token}` — the authoritative tutor \
             invoke grammar changed; update every client copy and this test \
             together (plan 0.4)"
        );
    }
    for token in [
        "APP_COPILOT_MARKER_INVOKE_WORDS",
        "\"@copilot\", \"@appcopilot\", \"@app-copilot\", \"@app_copilot\"",
        "\"copilot\" | \"app-copilot\" | \"@copilot\" | \"@appcopilot\" | \"@app-copilot\"",
        "\"@app_copilot\"",
    ] {
        assert!(
            BACKEND_TUTOR_RAIL.contains(token),
            "tutor.rs no longer pins `{token}` — the authoritative copilot \
             invoke grammar changed; update every client copy and this test \
             together (plan 0.4)"
        );
    }
}

/// The authorization-side parser (`parse_leading_feature_marker` /
/// `parse_leading_feature_invocation`) must recognize exactly the same
/// spellings the rail advertises, plus `@brainstorm`.
#[test]
fn backend_policy_snapshot_grammar_agrees_with_rail() {
    for token in [
        "\"@tutor\" | \"@tutur\" => FeatureMode::Tutor",
        "\"@copilot\" | \"@appcopilot\" | \"@app-copilot\" | \"@app_copilot\" => FeatureMode::AppCopilot",
        "\"@brainstorm\" => FeatureMode::Brainstorm",
        "matches_leading_spoken_command(text, &[\"hey\", \"tutor\"])",
        "matches_leading_spoken_command(text, &[\"hey\", \"app\", \"copilot\"])",
    ] {
        assert!(
            BACKEND_POLICY_SNAPSHOT.contains(token),
            "policy_snapshot.rs drifted from the rail grammar: `{token}` \
             missing. The authorization parser and the advertised invoke \
             words must agree (plan 0.4)"
        );
    }
}

/// Web composer ports of the tutor, copilot, and brainstorm grammars.
#[test]
fn web_composer_grammar_agrees_with_backend() {
    for token in [
        // Tutor: both spellings, anchored, with word-boundary lookahead.
        "@tut(?:or|ur)(?=$|[\\s:,])",
        "hey[\\s,]+tut(?:or|ur)",
        // Copilot: bare and app-prefixed marker forms plus spoken form.
        "@app[-_]?copilot",
        "@copilot(?=$|[\\s:,])",
        "hey[\\s,]+(?:app[\\s,]+)?copilot",
        // Brainstorm: marker and spoken forms.
        "@brainstorm(?=$|[\\s:,])",
        "hey[\\s,]+brainstorm",
    ] {
        assert!(
            WEB_CHAT_PANEL.contains(token),
            "ChatPanel.svelte drifted from the backend invoke grammar: \
             `{token}` missing (plan 0.4)"
        );
    }
}

/// The web voice flow speaks the `Tutor [Quick] ...` / `App Copilot ...`
/// commands; the backend voice parser must keep accepting them.
#[test]
fn web_voice_flow_uses_the_spoken_grammar() {
    for token in ["App Copilot", "Tutor [Quick]"] {
        assert!(
            WEB_VOICE_FLOW.contains(token),
            "guidedFlow.ts no longer uses the spoken command `{token}` — \
             if the spoken grammar changed intentionally, update the backend \
             voice parser arms in tutor.rs in the same change (plan 0.4)"
        );
    }
}

/// iOS: the composer tutor port must stay a faithful port of the web regex
/// (both spellings, spoken form); the VOICE flow parses the full copilot
/// marker arm (mirroring the backend voice parser) so it can reject App
/// Copilot client-side; and the brainstorm mention must keep serializing
/// the backend marker.
#[test]
fn ios_grammar_agrees_with_backend() {
    // The Swift pattern literal escapes backslashes, so assert against the
    // pattern line form (`hey[\\s,]+...`) rather than the doc comment's
    // single-backslash rendering.
    for token in ["@tut(?:or|ur)", "hey[\\\\s,]+tut(?:or|ur)"] {
        assert!(
            IOS_TUTOR_INVOKE.contains(token),
            "TutorInvoke.swift drifted from the shared tutor regex: \
             `{token}` missing (plan 0.4)"
        );
    }
    assert!(
        IOS_TUTOR_INVOKE.contains(
            "\"copilot\", \"app-copilot\", \"@copilot\", \"@appcopilot\", \"@app-copilot\", \"@app_copilot\""
        ),
        "TutorInvoke.swift voice flow no longer parses the full copilot \
         marker arm the backend voice parser accepts (plan 0.4)"
    );
    assert!(
        IOS_MENTION_CATALOG.contains("\"@brainstorm\""),
        "MentionCatalog.swift no longer serializes the `@brainstorm` \
         marker the backend parses (plan 0.4)"
    );
}

/// Android: tutor invoke port plus the brainstorm invoke.
#[test]
fn android_grammar_agrees_with_backend() {
    for token in ["@tut(?:or|ur)", "hey[\\s,]+tut(?:or|ur)"] {
        assert!(
            ANDROID_TUTOR_INVOKE.contains(token),
            "TutorInvoke.kt drifted from the shared tutor regex: \
             `{token}` missing (plan 0.4)"
        );
    }
    assert!(
        ANDROID_BRAINSTORM_INVOKE.contains("@brainstorm"),
        "BrainstormInvoke.kt no longer recognizes the `@brainstorm` \
         marker the backend parses (plan 0.4)"
    );
}

// --- Server-published catalog (plan 1.2) -----------------------------------
//
// `GET /api/magician/v2/chat/invoke-grammar` serves exactly what
// `invoke_grammar_catalog()` returns; these tests call that builder, so a
// red assertion here is a red assertion about the wire.

/// The published catalog's vocabulary IS the parser vocabulary: every
/// marker set and quick flag equals the backend constants (or, where the
/// parser site is an inline arm with no constant, the exact pinned
/// spelling), and the enumerated spoken phrases are the parsers' spoken
/// words crossed with the shared wake word.
#[test]
fn catalog_pins_the_backend_parser_vocabularies() {
    let catalog = invoke_grammar_catalog();

    assert_eq!(catalog.version, INVOKE_GRAMMAR_CATALOG_VERSION);
    assert_eq!(catalog.version, 1, "wire-format version starts at 1");
    assert!(
        catalog.leading_invoke_required,
        "every lane invoke must open the turn — pinned parser behavior"
    );

    // Markers: generated from the parser constants, verified against them.
    assert_eq!(catalog.lanes.tutor.lane, "tutor");
    assert_eq!(
        catalog.lanes.tutor.markers,
        TUTOR_MARKER_INVOKE_WORDS.to_vec()
    );
    assert_eq!(
        catalog.lanes.app_copilot.markers,
        APP_COPILOT_MARKER_INVOKE_WORDS.to_vec()
    );
    assert_eq!(
        catalog.lanes.brainstorm.markers,
        BRAINSTORM_MARKER_INVOKE_WORDS.to_vec()
    );
    assert_eq!(
        catalog.lanes.vibedev.markers,
        VIBEDEV_MARKER_INVOKE_WORDS.to_vec()
    );

    // Spoken phrases: the parsers' spoken words behind the shared wake
    // word, plus the App Copilot three-word window.
    assert_eq!(
        catalog.lanes.tutor.spoken_phrases,
        vec!["hey tutor", "hey tutur"]
    );
    assert_eq!(
        catalog.lanes.app_copilot.spoken_phrases,
        vec!["hey copilot", "hey app copilot"]
    );
    // The backend accepts NO spoken brainstorm invoke (the web composer's
    // `hey brainstorm` is a client-side pre-processor, not a server arm).
    assert!(catalog.lanes.brainstorm.spoken_phrases.is_empty());
    assert!(catalog.lanes.brainstorm.spoken_shape.is_none());

    // Quick flags: the constants the turn parsers read.
    assert_eq!(catalog.lanes.tutor.quick_flags, vec![TUTOR_QUICK_FLAG]);
    assert_eq!(
        catalog.lanes.app_copilot.quick_flags,
        vec![TUTOR_QUICK_FLAG],
        "`#quick` applies to copilot turns too (is_tutor_quick_prompt)"
    );
    assert!(catalog.lanes.brainstorm.quick_flags.is_empty());
    assert_eq!(
        catalog.lanes.vibedev.quick_flags,
        vec![VIBEDEV_DISCUSS_FLAG]
    );

    // The VibeDev spoken shape publishes the rail parser's slots, with the
    // two-token ASR subject spelling included.
    let shape = catalog
        .lanes
        .vibedev
        .spoken_shape
        .clone()
        .expect("vibedev publishes its spoken shape");
    assert_eq!(shape.optional_wake_words, vec!["hey"]);
    assert_eq!(
        shape.lead_verbs,
        ["start", "run", "launch", "begin", "open"]
    );
    assert_eq!(shape.optional_articles, ["a", "the"]);
    assert_eq!(shape.subjects, ["vibedev", "vibe dev"]);
    assert_eq!(shape.mode_nouns.len(), 2);
}

/// Behavior, not just spelling: every published marker and spoken phrase
/// must be accepted by the authorization-side leading parser as an invoke
/// for exactly its lane (plan 1.2 — the catalog is generated from these
/// very constants, so this closes the loop against the parsers
/// themselves).
#[test]
fn catalog_markers_and_phrases_parse_to_their_lanes() {
    let catalog = invoke_grammar_catalog();
    let cases = [
        (
            FeatureMode::Tutor,
            catalog.lanes.tutor.markers.clone(),
            catalog.lanes.tutor.spoken_phrases.clone(),
        ),
        (
            FeatureMode::AppCopilot,
            catalog.lanes.app_copilot.markers.clone(),
            catalog.lanes.app_copilot.spoken_phrases.clone(),
        ),
        (
            FeatureMode::Brainstorm,
            catalog.lanes.brainstorm.markers.clone(),
            catalog.lanes.brainstorm.spoken_phrases.clone(),
        ),
        (
            FeatureMode::Vibedev,
            catalog.lanes.vibedev.markers.clone(),
            catalog.lanes.vibedev.spoken_phrases.clone(),
        ),
    ];
    for (expected_mode, markers, spoken_phrases) in cases {
        for marker in markers {
            let text = format!("{marker} explain this");
            assert_eq!(
                parse_leading_feature_invocation(&text),
                expected_mode,
                "published marker `{marker}` must invoke {expected_mode:?}"
            );
        }
        for phrase in spoken_phrases {
            let text = format!("{phrase} explain this");
            assert_eq!(
                parse_leading_feature_invocation(&text),
                expected_mode,
                "published spoken phrase `{phrase}` must invoke {expected_mode:?}"
            );
        }
    }
}

/// The published spoken phrases are the canonical space-separated
/// spellings, but the leading parser also accepts a comma as the
/// separator between a phrase's words (`matches_leading_spoken_command`
/// treats `,` like whitespace). Pin a comma-separated variant of every
/// published phrase so that separator behavior cannot silently regress —
/// it is the fact that makes the catalog canonical rather than a strict
/// input allowlist (see the `LaneInvokeGrammar` doc).
#[test]
fn catalog_spoken_phrases_also_parse_with_comma_separators() {
    let catalog = invoke_grammar_catalog();
    for (expected_mode, spoken_phrases) in [
        (
            FeatureMode::Tutor,
            catalog.lanes.tutor.spoken_phrases.clone(),
        ),
        (
            FeatureMode::AppCopilot,
            catalog.lanes.app_copilot.spoken_phrases.clone(),
        ),
    ] {
        for phrase in spoken_phrases {
            let comma_variant = phrase.replace(' ', ",");
            let text = format!("{comma_variant} explain this");
            assert_eq!(
                parse_leading_feature_invocation(&text),
                expected_mode,
                "comma-separated variant `{comma_variant}` of the canonical \
                 phrase `{phrase}` must still parse as {expected_mode:?}"
            );
        }
    }
}

/// The published VibeDev spoken shape must generate phrases the rail
/// parser accepts, with the mode noun picking the mode — and the shape's
/// safety slots must stay load-bearing (no verb, no invoke).
#[test]
fn catalog_vibedev_spoken_shape_matches_the_rail_parser() {
    let catalog = invoke_grammar_catalog();
    let shape = catalog
        .lanes
        .vibedev
        .spoken_shape
        .clone()
        .expect("vibedev publishes its spoken shape");

    let empty = String::new();
    let wake_prefixes: Vec<String> = std::iter::once(empty.clone())
        .chain(
            shape
                .optional_wake_words
                .iter()
                .map(|word| format!("{word} ")),
        )
        .collect();
    let articles: Vec<String> = std::iter::once(empty)
        .chain(
            shape
                .optional_articles
                .iter()
                .map(|word| format!("{word} ")),
        )
        .collect();
    let mut parsed_count = 0usize;
    for wake in &wake_prefixes {
        for verb in &shape.lead_verbs {
            for article in &articles {
                for subject in &shape.subjects {
                    for noun in &shape.mode_nouns {
                        let text = format!(
                            "{wake}{verb} {article}{subject} {} fix the footer",
                            noun.noun
                        );
                        let parsed = parse_vibedev_rail_invocation(&text)
                            .unwrap_or_else(|| panic!("spoken shape must parse: {text}"));
                        assert_eq!(
                            parsed.discuss, noun.discuss,
                            "noun `{}` must select discuss={}",
                            noun.noun, noun.discuss
                        );
                        parsed_count += 1;
                    }
                }
            }
        }
    }
    // 2 wake prefixes x 5 verbs x 3 articles x 2 subjects x 2 mode nouns:
    // the exact published 120-member cross-product parses. Keep this count
    // pinned so an accidental catalog expansion or contraction is visible.
    assert_eq!(parsed_count, 120);

    // One canonical phrase also classifies through the leading parser.
    assert_eq!(
        parse_leading_feature_invocation("start a vibedev build fix the footer"),
        FeatureMode::Vibedev
    );

    // The verb slot is load-bearing: subject + noun without a verb is
    // ordinary dictation and must not invoke anything.
    assert!(parse_vibedev_rail_invocation("vibedev build fix the footer").is_none());
    assert_eq!(
        parse_leading_feature_invocation("vibedev build fix the footer"),
        FeatureMode::None
    );
}

/// Version + etag: the drift signals. The catalog is a pure function of
/// the parser constants, so two builds agree; the etag is the sha256 hex
/// over the serialized lane set.
#[test]
fn catalog_is_versioned_and_content_hashed() {
    let first = invoke_grammar_catalog();
    let second = invoke_grammar_catalog();
    assert_eq!(first, second, "catalog must be deterministic");
    assert_eq!(first.etag.len(), 64, "etag is a 64-char sha256 hex string");
    assert!(first
        .etag
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
}

/// The few grammar words that live only as inline parser literals — the
/// shared wake word, the App Copilot spoken prefix, the Brainstorm marker
/// arm, and VibeDev's two-token subject — are named constants in the
/// catalog module; this pins each constant back to its parser site so
/// none of them can drift either (plan 1.2 single-source rule).
#[test]
fn catalog_inline_grammar_words_stay_pinned_to_their_parser_sites() {
    assert!(
        BACKEND_TUTOR_RAIL.contains(&format!("window[0] == \"{SPOKEN_WAKE_WORD}\"")),
        "tutor.rs spoken windows no longer use the shared wake word"
    );
    assert!(
        BACKEND_POLICY_SNAPSHOT.contains(&format!("&[\"{SPOKEN_WAKE_WORD}\", \"tutor\"]")),
        "policy_snapshot spoken arms no longer use the shared wake word"
    );
    assert!(
        BACKEND_INVOKE_GRAMMAR.contains(&format!("Some(\"{SPOKEN_WAKE_WORD}\")")),
        "the spoken vibedev parser no longer uses the shared wake word"
    );
    assert!(
        BACKEND_TUTOR_RAIL.contains(&format!(
            "window[1] == \"{APP_COPILOT_SPOKEN_PREFIX_WORD}\""
        )),
        "tutor.rs three-word copilot window no longer uses the spoken prefix"
    );
    assert!(
        BACKEND_POLICY_SNAPSHOT.contains(&format!(
            "\"{}\" => FeatureMode::Brainstorm",
            BRAINSTORM_MARKER_INVOKE_WORDS[0]
        )),
        "policy_snapshot brainstorm marker arm drifted from the catalog constant"
    );
    assert!(
        BACKEND_INVOKE_GRAMMAR.contains(&format!(
            "word(cursor) == Some(\"{}\") && word(cursor + 1) == Some(\"{}\")",
            VIBEDEV_TWO_TOKEN_SUBJECT[0], VIBEDEV_TWO_TOKEN_SUBJECT[1]
        )),
        "the two-token ASR subject branch drifted from the catalog constant"
    );
}
