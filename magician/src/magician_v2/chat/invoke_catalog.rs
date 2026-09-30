//! The server-published lane invoke-grammar catalog (plan workstream 1.2;
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Served verbatim by `GET /api/magician/v2/chat/invoke-grammar`
//! (`magician-api/src/invoke_grammar_api.rs`), this is the one wire-side
//! answer to "which invoke words start which lane". It is **generated from
//! the same constants the backend parsers match against** — every marker,
//! spoken word, and quick flag below is read from
//! [`crate::magician_v2::tutor`] or
//! [`crate::magician_v2::chat::invoke_grammar`] — so the catalog cannot
//! drift from the parsers without the agreement test
//! (`magician/tests/invoke_grammar_agreement.rs`, plan 0.4) failing.
//!
//! The few grammar words that exist only as inline literals in the parsers
//! (the spoken wake word, the App Copilot spoken prefix, the Brainstorm
//! marker arm, and VibeDev's two-token ASR subject spelling) become named
//! constants here exactly once; the agreement test pins each of them back
//! to the parser site and to the parser functions' behavior, so they are
//! as single-sourced as the exported arrays.
//!
//! That arrangement is the steady state, not a stopgap: the frozen
//! agreement test asserts the parser sites keep their literal shapes
//! (marker match arms, spoken-command call sites) verbatim, so the parsers
//! cannot consume these constants without failing it. For these few words
//! the constant here plus the oracle's literal pins are the single-sourcing
//! join — one spelling on each side of a test-enforced agreement (batch 7
//! fold unit F10; the disposition is documented at the parser sites in
//! `execution/agentic/policy_snapshot.rs`).
//!
//! The wire is **additive-only**: lanes and fields are never removed or
//! retyped. Vocabulary changes surface as new/changed entries plus a new
//! `etag`; a schema change bumps `version`. Clients keep local parsers
//! until the later client-consumption step (not on this plan's critical
//! path) — publication is what that step will consume.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::magician_v2::chat::invoke_grammar::{
    VIBEDEV_DISCUSS_FLAG, VIBEDEV_MARKER_INVOKE_WORDS, VIBEDEV_SPOKEN_ARTICLES,
    VIBEDEV_SPOKEN_BUILD_NOUN, VIBEDEV_SPOKEN_LEAD_VERBS, VIBEDEV_SPOKEN_PLAN_NOUN,
    VIBEDEV_SPOKEN_SUBJECTS,
};
use crate::magician_v2::tutor::{
    APP_COPILOT_MARKER_INVOKE_WORDS, APP_COPILOT_SPOKEN_INVOKE_WORDS, TUTOR_MARKER_INVOKE_WORDS,
    TUTOR_QUICK_FLAG, TUTOR_SPOKEN_INVOKE_WORDS,
};

/// Wire-format version of the catalog itself. Bump only when the catalog's
/// schema changes, additively; vocabulary-only changes keep the version and
/// move the `etag` instead.
pub const INVOKE_GRAMMAR_CATALOG_VERSION: u32 = 1;

/// The spoken wake word every lane's voice invoke opens with. The backend
/// parsers hardcode it in three places — `tutor.rs`'s spoken windows,
/// `policy_snapshot.rs`'s `matches_leading_spoken_command` arms, and the
/// VibeDev spoken phrase parser — and the agreement test pins this constant
/// to all three.
pub const SPOKEN_WAKE_WORD: &str = "hey";

/// The word between the wake word and `copilot` in the spoken App Copilot
/// invoke (`hey app copilot`), from `is_app_copilot_prompt`'s three-word
/// window in `tutor.rs`. Pinned to that site by the agreement test.
pub const APP_COPILOT_SPOKEN_PREFIX_WORD: &str = "app";

/// The Brainstorm lane's marker words. The parser site is the
/// `"@brainstorm"` arm of `parse_leading_feature_marker`
/// (`execution/agentic/policy_snapshot.rs`), which has no exported constant
/// of its own; the agreement test pins this array to that arm functionally.
pub const BRAINSTORM_MARKER_INVOKE_WORDS: [&str; 1] = ["@brainstorm"];

/// The two-token ASR spelling of the VibeDev subject (`vibe dev`), read by
/// the two-token branch of `parse_spoken_vibedev_rail_phrase` in
/// `chat/invoke_grammar.rs`. Pinned to that branch by the agreement test.
pub const VIBEDEV_TWO_TOKEN_SUBJECT: [&str; 2] = ["vibe", "dev"];

/// The rendered shape of the VibeDev spoken invoke, for display: the slots
/// themselves are published as data in [`VibedevSpokenGrammar`].
pub const VIBEDEV_SPOKEN_SHAPE: &str = "[hey] <verb> [a|the] <subject> <build|plan>";

/// The versioned, content-hashed invoke-grammar catalog served to clients.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InvokeGrammarCatalog {
    /// The wire-format version ([`INVOKE_GRAMMAR_CATALOG_VERSION`]).
    pub version: u32,
    /// sha256 (lowercase hex) over the canonical serialization of `lanes` —
    /// the drift signal: it changes whenever the published vocabulary
    /// changes, because the vocabulary is generated from the parser
    /// constants rather than hand-maintained.
    pub etag: String,
    /// Every lane's invoke must open the turn. Incidental mid-text
    /// mentions, quoted handles, and pasted logs never invoke a lane; this
    /// is the pinned behavior of every parser the catalog projects.
    pub leading_invoke_required: bool,
    /// Per-lane grammar, keyed by lane name on the wire.
    pub lanes: LaneInvokeGrammars,
}

/// The per-lane grammars, one field per conversational product lane. Wire
/// key order is this struct's field order — fixed, not map-ordered.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LaneInvokeGrammars {
    /// The Personal Tutor lane (`@tutor` / `hey tutor`).
    pub tutor: LaneInvokeGrammar,
    /// The App Copilot lane (`@copilot` / `hey copilot`).
    pub app_copilot: LaneInvokeGrammar,
    /// The Brainstorm / Live Thinking Map lane (`@brainstorm`; no spoken
    /// form is accepted server-side).
    pub brainstorm: LaneInvokeGrammar,
    /// The VibeDev rail (`@vibedev` / `start a vibedev build`).
    pub vibedev: LaneInvokeGrammar,
}

/// One lane's published invoke grammar: the marker words, the spoken
/// phrases or phrase shape, and the quick flags — the canonical
/// vocabulary the backend parsers accept for that lane.
///
/// Canonical means space-separated here, not byte-exhaustive: the parsers
/// additionally accept a comma as a separator between the words of a
/// spoken phrase (`matches_leading_spoken_command` in
/// `execution/agentic/policy_snapshot.rs` treats `,` like whitespace, so
/// the backend accepts `hey,tutor explain` while the catalog only lists
/// `hey tutor`). A client treating this catalog as a strict input
/// allowlist therefore (safely) rejects some accepted inputs; the
/// invariant it must never break is the other direction — accepting an
/// input the parsers reject.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LaneInvokeGrammar {
    /// The lane's wire name.
    pub lane: &'static str,
    /// Typed marker invokes, serialized from the parser constants.
    pub markers: Vec<&'static str>,
    /// Enumerated spoken invokes accepted at the head of a turn, in their
    /// canonical space-separated spelling (the parsers also accept comma
    /// separators — see the type doc). Empty for lanes whose spoken
    /// grammar is structural ([`Self::spoken_shape`]) or absent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spoken_phrases: Vec<String>,
    /// The structural spoken grammar, published only by the lane whose
    /// spoken invoke is a shaped phrase rather than an enumerable word
    /// list (VibeDev).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spoken_shape: Option<VibedevSpokenGrammar>,
    /// Chat-box flags the lane reads (e.g. `#quick`, `#discuss`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quick_flags: Vec<&'static str>,
}

/// The VibeDev spoken invoke as slots, mirroring
/// `parse_spoken_vibedev_rail_phrase` token-for-token: an optional wake
/// word, a leading verb, an optional article, the subject (one word or the
/// two-token ASR spelling), and a closing noun that picks the mode.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VibedevSpokenGrammar {
    /// The rendered template ([`VIBEDEV_SPOKEN_SHAPE`]).
    pub shape: &'static str,
    /// Words that may open the phrase (`[hey]`).
    pub optional_wake_words: Vec<&'static str>,
    /// Verbs that must open (or follow the wake word in) the phrase —
    /// the slot that keeps ordinary dictation from starting a build.
    pub lead_verbs: Vec<&'static str>,
    /// Articles that may sit between the verb and the subject.
    pub optional_articles: Vec<&'static str>,
    /// The product's name as speech renders it, including the two-token
    /// ASR spelling.
    pub subjects: Vec<String>,
    /// The closing noun, with the mode it selects.
    pub mode_nouns: Vec<VibedevSpokenModeNoun>,
}

/// One closing noun of the VibeDev spoken phrase and the mode it selects.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VibedevSpokenModeNoun {
    /// The noun itself (`build` / `plan`).
    pub noun: &'static str,
    /// Whether this noun selects the discuss (`#discuss`) mode.
    pub discuss: bool,
}

/// Build the catalog served by `GET /api/magician/v2/chat/invoke-grammar`.
///
/// Pure and allocation-bounded: no I/O, no clock, no service state — the
/// same call serves the endpoint and the agreement test, which is what
/// makes the test's assertions about "the published content" assertions
/// about the wire itself.
pub fn invoke_grammar_catalog() -> InvokeGrammarCatalog {
    let lanes = LaneInvokeGrammars {
        tutor: LaneInvokeGrammar {
            lane: "tutor",
            markers: TUTOR_MARKER_INVOKE_WORDS.to_vec(),
            spoken_phrases: TUTOR_SPOKEN_INVOKE_WORDS
                .iter()
                .map(|word| format!("{SPOKEN_WAKE_WORD} {word}"))
                .collect(),
            spoken_shape: None,
            quick_flags: vec![TUTOR_QUICK_FLAG],
        },
        app_copilot: LaneInvokeGrammar {
            lane: "app_copilot",
            markers: APP_COPILOT_MARKER_INVOKE_WORDS.to_vec(),
            spoken_phrases: APP_COPILOT_SPOKEN_INVOKE_WORDS
                .iter()
                .flat_map(|word| {
                    [
                        format!("{SPOKEN_WAKE_WORD} {word}"),
                        format!("{SPOKEN_WAKE_WORD} {APP_COPILOT_SPOKEN_PREFIX_WORD} {word}"),
                    ]
                })
                .collect(),
            spoken_shape: None,
            quick_flags: vec![TUTOR_QUICK_FLAG],
        },
        brainstorm: LaneInvokeGrammar {
            lane: "brainstorm",
            markers: BRAINSTORM_MARKER_INVOKE_WORDS.to_vec(),
            spoken_phrases: Vec::new(),
            spoken_shape: None,
            quick_flags: Vec::new(),
        },
        vibedev: LaneInvokeGrammar {
            lane: "vibedev",
            markers: VIBEDEV_MARKER_INVOKE_WORDS.to_vec(),
            spoken_phrases: Vec::new(),
            spoken_shape: Some(VibedevSpokenGrammar {
                shape: VIBEDEV_SPOKEN_SHAPE,
                optional_wake_words: vec![SPOKEN_WAKE_WORD],
                lead_verbs: VIBEDEV_SPOKEN_LEAD_VERBS.to_vec(),
                optional_articles: VIBEDEV_SPOKEN_ARTICLES.to_vec(),
                subjects: VIBEDEV_SPOKEN_SUBJECTS
                    .iter()
                    .map(|subject| subject.to_string())
                    .chain(std::iter::once(VIBEDEV_TWO_TOKEN_SUBJECT.join(" ")))
                    .collect(),
                mode_nouns: vec![
                    VibedevSpokenModeNoun {
                        noun: VIBEDEV_SPOKEN_BUILD_NOUN,
                        discuss: false,
                    },
                    VibedevSpokenModeNoun {
                        noun: VIBEDEV_SPOKEN_PLAN_NOUN,
                        discuss: true,
                    },
                ],
            }),
            quick_flags: vec![VIBEDEV_DISCUSS_FLAG],
        },
    };
    InvokeGrammarCatalog {
        version: INVOKE_GRAMMAR_CATALOG_VERSION,
        etag: etag_for(&lanes),
        leading_invoke_required: true,
        lanes,
    }
}

/// A stable content hash over the serialized lane set (sha256 hex), used
/// as the catalog's drift `etag`.
///
/// The payload is structs and vectors only — no maps — so serde_json
/// serialization order is the struct declaration order and is fully
/// deterministic regardless of the `preserve_order` feature. If a map ever
/// enters the payload, canonicalize keys here first.
fn etag_for(lanes: &LaneInvokeGrammars) -> String {
    let serialized = serde_json::to_vec(lanes).unwrap_or_default();
    let digest = Sha256::digest(&serialized);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is a pure function of the parser constants: two builds
    /// agree bit-for-bit, and the etag is a 64-char lowercase hex digest.
    #[test]
    fn catalog_is_deterministic_with_a_stable_etag() {
        let first = invoke_grammar_catalog();
        let second = invoke_grammar_catalog();
        assert_eq!(first, second);
        assert_eq!(first.version, INVOKE_GRAMMAR_CATALOG_VERSION);
        assert_eq!(first.etag.len(), 64);
        assert!(first
            .etag
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert!(first.leading_invoke_required);
    }

    /// No lane publishes an empty marker set: a lane with no typed invoke
    /// is not a conversational lane.
    #[test]
    fn every_lane_publishes_markers() {
        let catalog = invoke_grammar_catalog();
        for (markers, lane) in [
            (catalog.lanes.tutor.markers, "tutor"),
            (catalog.lanes.app_copilot.markers, "app_copilot"),
            (catalog.lanes.brainstorm.markers, "brainstorm"),
            (catalog.lanes.vibedev.markers, "vibedev"),
        ] {
            assert!(
                !markers.is_empty(),
                "lane {lane} published an empty marker set"
            );
        }
    }
}
