//! Server-side parser for the `<speech>` tag protocol the chat LLM
//! emits on voice-originated turns.
//!
//! The chat outer-loop system prompt (loaded from
//! `data/magician_v2/prompts/voice_origin_speech_instruction_v1.0.0.json`)
//! teaches the LLM to wrap audible content in
//! `<speech emotion="…" pace="…">…</speech>` tags. This module
//! extracts those tags into typed `SpeechSegment`s so:
//!
//! 1. The chat message envelope can carry `speech_segments` as a
//!    structured field — every UI surface (web, mobile, voice-only
//!    client) reads typed segments instead of re-implementing the
//!    regex.
//! 2. The new streamed-segment synth endpoint
//!    (`POST /tts/synthesize_message`) can walk the parsed segments
//!    and route each one through the TTS provider chain without the
//!    caller re-sending text+attrs N times.
//!
//! The parser is intentionally lenient: double-quoted, single-quoted,
//! and bare-word attribute values are all accepted. Unknown enum
//! values are dropped silently rather than rejecting the whole tag —
//! the LLM occasionally hallucinates new emotions and a dropped hint
//! is a better failure mode than a missing audible segment.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::magician_v2::media_seam::tts::{TtsEmotion, TtsPace, TtsStyle, TtsVoiceMode};

/// One audible chunk extracted from an assistant message. Mirrors the
/// optional delivery hints that `TtsRequest` carries — the message
/// envelope can either ship segments as-is and let the client call
/// per-segment synth, or the streamed-segment endpoint can pipe them
/// straight into the TTS provider chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechSegment {
    /// Cleaned, single-spaced text to synthesize. Inner whitespace is
    /// collapsed and the body is trimmed so the spoken version reads
    /// naturally even when the LLM left markdown-friendly line breaks.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion: Option<TtsEmotion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<TtsStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<TtsPace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_mode: Option<TtsVoiceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<String>,
}

// Match either `<speech>` (no attrs) or `<speech attr=… …>`. Attrs land
// in group 1, body in group 2. Case-insensitive on tag name.
static SPEECH_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)<speech\b([^>]*)>(.*?)</speech>").expect("speech tag regex compiles")
});

// Three accepted attribute shapes — double-quoted, single-quoted, and
// unquoted bare-word. Unquoted values stop at whitespace so
// `emotion=happy style=casual` parses cleanly. Mirrors the
// frontend-only fallback parser that used to live in `speechTags.ts`
// before this moved server-side.
static ATTR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(\w+)\s*=\s*"([^"]*)"|(\w+)\s*=\s*'([^']*)'|(\w+)\s*=\s*([^\s"'>]+)"#)
        .expect("attr regex compiles")
});

static WHITESPACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s+").expect("whitespace regex compiles"));

/// Parse the raw assistant message body into ordered `SpeechSegment`s.
///
/// Returns an empty vector when the body has no `<speech>` tags —
/// this is the signal "nothing audible to read aloud". Callers that
/// want a fallback ("read the whole body when no tags are present")
/// implement that policy themselves; the parser stays mechanical.
///
/// Empty / whitespace-only bodies are dropped so the synth queue
/// doesn't fire silent requests.
pub fn parse_speech_segments(raw: &str) -> Vec<SpeechSegment> {
    let mut out = Vec::new();
    for caps in SPEECH_TAG_RE.captures_iter(raw) {
        let attrs_raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let body_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let body = WHITESPACE_RE.replace_all(body_raw, " ").trim().to_string();
        if body.is_empty() {
            continue;
        }
        let mut segment = SpeechSegment {
            text: body,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        };
        apply_attrs(attrs_raw, &mut segment);
        out.push(segment);
    }
    out
}

/// Whether the body contains at least one `<speech>` block. Cheap
/// pre-check for the chat service to avoid building an empty segment
/// vector for typed-turn replies.
pub fn has_speech_tags(raw: &str) -> bool {
    SPEECH_TAG_RE.is_match(raw)
}

/// Remove `<speech>` / `</speech>` markers (and attributes) so the
/// visible chat body and Live commentary never show the protocol tags.
pub fn strip_speech_tag_markers(raw: &str) -> String {
    static OPEN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)<speech\b[^>]*>").expect("speech open regex"));
    static CLOSE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)</speech>").expect("speech close regex"));
    let opened = OPEN.replace_all(raw, "");
    CLOSE.replace_all(&opened, "").into_owned()
}

fn apply_attrs(raw_attrs: &str, segment: &mut SpeechSegment) {
    if raw_attrs.is_empty() {
        return;
    }
    for caps in ATTR_RE.captures_iter(raw_attrs) {
        // ATTR_RE has three alternation branches with two capture
        // groups each — exactly one branch matches per iteration.
        let key = caps
            .get(1)
            .or_else(|| caps.get(3))
            .or_else(|| caps.get(5))
            .map(|m| m.as_str().to_ascii_lowercase());
        let value = caps
            .get(2)
            .or_else(|| caps.get(4))
            .or_else(|| caps.get(6))
            .map(|m| m.as_str().trim().to_string());
        let (Some(key), Some(value)) = (key, value) else {
            continue;
        };
        if key.is_empty() || value.is_empty() {
            continue;
        }
        match key.as_str() {
            "emotion" => {
                if let Some(parsed) = parse_emotion(&value) {
                    segment.emotion = Some(parsed);
                }
            },
            "style" => {
                if let Some(parsed) = parse_style(&value) {
                    segment.style = Some(parsed);
                }
            },
            "pace" => {
                if let Some(parsed) = parse_pace(&value) {
                    segment.pace = Some(parsed);
                }
            },
            "voice" | "voice_mode" => {
                if let Some(parsed) = parse_voice_mode(&value) {
                    segment.voice_mode = Some(parsed);
                }
            },
            "emphasis" => {
                segment.emphasis = Some(value);
            },
            _ => {},
        }
    }
}

// Per-enum parsers. We do these by hand rather than going through
// serde because serde's enum string parsing is case-sensitive and the
// LLM mixes cases freely (`emotion="Happy"`, `EMOTION="HAPPY"`).

fn parse_emotion(value: &str) -> Option<TtsEmotion> {
    match value.to_ascii_lowercase().as_str() {
        "neutral" => Some(TtsEmotion::Neutral),
        "happy" => Some(TtsEmotion::Happy),
        "excited" => Some(TtsEmotion::Excited),
        "concerned" => Some(TtsEmotion::Concerned),
        "apologetic" => Some(TtsEmotion::Apologetic),
        "confident" => Some(TtsEmotion::Confident),
        "playful" => Some(TtsEmotion::Playful),
        "urgent" => Some(TtsEmotion::Urgent),
        "sad" => Some(TtsEmotion::Sad),
        "confused" => Some(TtsEmotion::Confused),
        _ => None,
    }
}

fn parse_style(value: &str) -> Option<TtsStyle> {
    match value.to_ascii_lowercase().as_str() {
        "casual" => Some(TtsStyle::Casual),
        "formal" => Some(TtsStyle::Formal),
        "dramatic" => Some(TtsStyle::Dramatic),
        "deadpan" => Some(TtsStyle::Deadpan),
        "warm" => Some(TtsStyle::Warm),
        "clinical" => Some(TtsStyle::Clinical),
        _ => None,
    }
}

fn parse_pace(value: &str) -> Option<TtsPace> {
    match value.to_ascii_lowercase().as_str() {
        "slow" => Some(TtsPace::Slow),
        "normal" => Some(TtsPace::Normal),
        "fast" => Some(TtsPace::Fast),
        _ => None,
    }
}

fn parse_voice_mode(value: &str) -> Option<TtsVoiceMode> {
    match value.to_ascii_lowercase().as_str() {
        "default" => Some(TtsVoiceMode::Default),
        "whisper" => Some(TtsVoiceMode::Whisper),
        "announcement" => Some(TtsVoiceMode::Announcement),
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;

    #[test]
    fn empty_input_returns_no_segments() {
        assert!(parse_speech_segments("").is_empty());
        assert!(parse_speech_segments("just plain text").is_empty());
    }

    #[test]
    fn bare_tag_extracts_body() {
        let segments = parse_speech_segments("intro <speech>Done.</speech> trailing");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Done.");
        assert!(segments[0].emotion.is_none());
    }

    #[test]
    fn strip_markers_leaves_inner_words() {
        assert_eq!(
            strip_speech_tag_markers("intro <speech emotion=\"calm\">Done.</speech> trailing"),
            "intro Done. trailing"
        );
    }

    #[test]
    fn multiple_tags_preserve_order() {
        let segments = parse_speech_segments("<speech>first</speech> mid <speech>second</speech>");
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn empty_bodies_are_dropped() {
        let segments = parse_speech_segments("<speech>   </speech><speech>real</speech>");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "real");
    }

    #[test]
    fn collapses_internal_whitespace() {
        let segments = parse_speech_segments("<speech>hello\n\n  world\t  again</speech>");
        assert_eq!(segments[0].text, "hello world again");
    }

    #[test]
    fn parses_double_quoted_attrs() {
        let segments =
            parse_speech_segments(r#"<speech emotion="apologetic" pace="slow">Sorry.</speech>"#);
        assert_eq!(segments[0].emotion, Some(TtsEmotion::Apologetic));
        assert_eq!(segments[0].pace, Some(TtsPace::Slow));
    }

    #[test]
    fn parses_single_quoted_attrs() {
        let segments =
            parse_speech_segments("<speech emotion='excited' style='casual'>Done!</speech>");
        assert_eq!(segments[0].emotion, Some(TtsEmotion::Excited));
        assert_eq!(segments[0].style, Some(TtsStyle::Casual));
    }

    #[test]
    fn parses_unquoted_attrs() {
        let segments = parse_speech_segments("<speech emotion=happy pace=fast>Quick.</speech>");
        assert_eq!(segments[0].emotion, Some(TtsEmotion::Happy));
        assert_eq!(segments[0].pace, Some(TtsPace::Fast));
    }

    #[test]
    fn voice_attribute_maps_to_voice_mode() {
        let segments = parse_speech_segments(r#"<speech voice="whisper">aside</speech>"#);
        assert_eq!(segments[0].voice_mode, Some(TtsVoiceMode::Whisper));
        let segments = parse_speech_segments(r#"<speech voice_mode="whisper">aside</speech>"#);
        assert_eq!(segments[0].voice_mode, Some(TtsVoiceMode::Whisper));
    }

    #[test]
    fn unknown_enum_values_dropped_silently() {
        let segments =
            parse_speech_segments(r#"<speech emotion="overjoyed" style="casual">x</speech>"#);
        assert!(segments[0].emotion.is_none());
        assert_eq!(segments[0].style, Some(TtsStyle::Casual));
    }

    #[test]
    fn case_insensitive_tag_and_keys() {
        let segments = parse_speech_segments(r#"<SPEECH EMOTION="HAPPY">hi</SPEECH>"#);
        assert_eq!(segments[0].emotion, Some(TtsEmotion::Happy));
    }

    #[test]
    fn emphasis_is_free_form() {
        let segments =
            parse_speech_segments(r#"<speech emphasis="the deadline is today">Ship it.</speech>"#);
        assert_eq!(
            segments[0].emphasis.as_deref(),
            Some("the deadline is today")
        );
    }

    #[test]
    fn has_speech_tags_detects_attrs_and_bare() {
        assert!(has_speech_tags("<speech>hi</speech>"));
        assert!(has_speech_tags(r#"<speech emotion="happy">hi</speech>"#));
        assert!(!has_speech_tags("plain text"));
    }
}
