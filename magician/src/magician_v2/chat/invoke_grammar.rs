//! The shared invoke-grammar module for conversational product lanes (plan
//! workstream 1.2b;
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! The VibeDev rail's invoke grammar moved here from `tutor.rs` as a pure
//! move, and `tutor.rs` re-exports the public symbols so every existing
//! `crate::magician_v2::tutor::…` import path keeps resolving unedited.
//! The tokenizer moved with it because the Tutor/App Copilot voice-guided
//! flow already shares it: that flow's parser
//! (`tutor.rs`'s `parse_voice_guided_flow_invocation`, lane slice 1.2c)
//! still lives in `tutor.rs` and tokenizes through this module's
//! `voice_command_tokens`/`VoiceCommandToken` — one tokenizer, one
//! spelling.
//!
//! The spoken-phrase constants below are `pub(crate)` because the
//! server-published catalog (`chat::invoke_catalog`, plan 1.2) serializes
//! them for clients; the parsers remain their only consumers.

/// The VibeDev rail has no bare-word counterpart, and that asymmetry with
/// `@tutor`/`tutor` is deliberate. The marker is the whole of the intent, and a
/// bare `vibedev` would make "vibedev is stuck again" — an ordinary thing to say
/// about the product — start a build. Keeping the `@` also keeps one shape to
/// learn across every lane.
pub const VIBEDEV_MARKER_INVOKE_WORDS: [&str; 1] = ["@vibedev"];
/// Verbs that may open the SPOKEN VibeDev invoke. Speech cannot say `@vibedev` —
/// ASR renders it "at vibe dev" — and the bare word is ruled out above, so the
/// spoken form is a whole phrase and **the verb is the part that makes it
/// safe**: without one, "vibedev build failed on main" (an ordinary thing to say
/// into a voice note about CI) would start a build; with one, the utterance has
/// to open with somebody asking for a build.
///
/// The set deliberately overlaps [the Tutor voice-guided flow][flow]'s
/// leading verbs so one spoken grammar covers every product lane.
///
/// [flow]: crate::magician_v2::tutor::parse_voice_guided_flow_invocation
pub(crate) const VIBEDEV_SPOKEN_LEAD_VERBS: [&str; 5] = ["start", "run", "launch", "begin", "open"];
/// Optional article between the verb and the subject: "start **a** vibedev build".
pub(crate) const VIBEDEV_SPOKEN_ARTICLES: [&str; 2] = ["a", "the"];
/// The subject word — the product's own name, and nothing else. The two-token
/// "vibe dev" spelling ASR usually produces for it is accepted separately below.
pub(crate) const VIBEDEV_SPOKEN_SUBJECTS: [&str; 1] = ["vibedev"];
/// The noun that closes the phrase and picks the mode. Requiring a noun is what
/// keeps "start vibedev on the login page" — which stops at the subject —
/// ordinary dictation.
pub(crate) const VIBEDEV_SPOKEN_BUILD_NOUN: &str = "build";
/// Spoken counterpart of [`VIBEDEV_DISCUSS_FLAG`]: "start a vibedev **plan** …".
/// A hash flag cannot be spoken, and the plan run is the *safer* of the two
/// modes, so leaving it unreachable by voice would push every spoken request
/// onto the mode that writes code.
pub(crate) const VIBEDEV_SPOKEN_PLAN_NOUN: &str = "plan";
/// Plan-only counterpart to `#quick`. Autopilot deliberately has no chat flag
/// and no spoken phrase: it self-applies its own work and must not sit one word
/// away in a chat box or a microphone.
pub const VIBEDEV_DISCUSS_FLAG: &str = "#discuss";

/// One whitespace/punctuation-delimited word of a typed or spoken command,
/// normalized to ASCII lowercase. Shared with the Tutor/App Copilot
/// voice-guided flow that still lives in `tutor.rs`.
#[derive(Debug)]
pub(crate) struct VoiceCommandToken {
    pub(crate) end: usize,
    pub(crate) normalized: String,
}

/// Tokenize an utterance into [`VoiceCommandToken`]s: runs of alphanumerics
/// plus `@`, `#`, `_`, and `-`, lowercased, with each token's end byte
/// offset attached so the parser can slice the prompt verbatim.
pub(crate) fn voice_command_tokens(text: &str) -> Vec<VoiceCommandToken> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        let token_char = ch.is_alphanumeric() || matches!(ch, '@' | '#' | '_' | '-');
        match (start, token_char) {
            (None, true) => start = Some(index),
            (Some(token_start), false) => {
                if let Some(raw) = text.get(token_start..index) {
                    tokens.push(VoiceCommandToken {
                        end: index,
                        normalized: raw.to_ascii_lowercase(),
                    });
                }
                start = None;
            },
            _ => {},
        }
    }
    tokens
}

/// One recognized `@vibedev` turn: the mode flag it carried and the request left
/// over for the build. Reporting only; deciding what an empty `prompt` means
/// belongs to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VibedevRailInvocation {
    pub discuss: bool,
    pub prompt: String,
}

/// Match the SPOKEN VibeDev invoke at the head of an already-tokenized
/// utterance, returning the mode it selected and the byte offset just past the
/// phrase.
///
/// The shape is `[hey] <verb> [a|the] <vibedev|vibe dev> <build|plan>` — for
/// example "start a vibedev build fix the footer spacing" or "start a vibedev
/// plan should we split this panel".
///
/// Every one of those slots is load-bearing against a false positive, and a
/// false positive here starts a real build:
///
/// * the **verb** stops "vibedev build failed on main" and "the vibedev build
///   is red", neither of which opens with someone asking for one;
/// * the **noun** stops "start vibedev on the login page" and "start a vibedev
///   review of the diff", which reach the subject and then say something else;
/// * requiring the **subject** stops "start a build", which in this repository
///   is at least as likely to mean a container image as a VibeDev run.
///
/// Leading position is required by the caller, for the same reason the typed
/// marker requires it: "can you start a vibedev build for the footer" is a
/// deliberate non-match, because incidental mentions must never start work.
fn parse_spoken_vibedev_rail_phrase(tokens: &[VoiceCommandToken]) -> Option<(bool, usize)> {
    let word = |index: usize| tokens.get(index).map(|token| token.normalized.as_str());
    let mut cursor = 0usize;
    if word(cursor) == Some("hey") {
        cursor += 1;
    }
    if !VIBEDEV_SPOKEN_LEAD_VERBS.contains(&word(cursor)?) {
        return None;
    }
    cursor += 1;
    if VIBEDEV_SPOKEN_ARTICLES.contains(&word(cursor)?) {
        cursor += 1;
    }
    if word(cursor) == Some("vibe") && word(cursor + 1) == Some("dev") {
        cursor += 2;
    } else if VIBEDEV_SPOKEN_SUBJECTS.contains(&word(cursor)?) {
        cursor += 1;
    } else {
        return None;
    }
    let noun = tokens.get(cursor)?;
    let discuss = if noun.normalized == VIBEDEV_SPOKEN_BUILD_NOUN {
        false
    } else if noun.normalized == VIBEDEV_SPOKEN_PLAN_NOUN {
        true
    } else {
        return None;
    };
    Some((discuss, noun.end))
}

/// Recognize a leading VibeDev-rail invoke — typed `@vibedev` or the spoken
/// phrase — and split off the mode it selected.
///
/// This deliberately does not follow `is_tutor_prompt`, which finds its markers
/// with `contains`. Substring matching would accept any longer handle that
/// merely opens with the marker's letters, so `@vibedev-review` or a future
/// `@vibedevops` would start a build. Tokenizing makes the marker match only as
/// a whole word, and costs nothing to keep.
///
/// The invoke must also open the turn, for the reason
/// `parse_leading_feature_marker` requires the same: quoted text, pasted logs
/// and mid-sentence mentions must never be able to start work. "the docs say to
/// use @vibedev" is someone explaining the rail, not using it.
///
/// `#discuss` is read only in the slot directly after the marker, matching the
/// `#quick` contract the composer already serializes against. Keeping it
/// positional also lets `prompt` stay a verbatim slice of what the user typed
/// rather than a re-joined approximation of it. Its spoken counterpart is the
/// `plan` noun in [`parse_spoken_vibedev_rail_phrase`].
///
/// **Both spellings are read here, and only here.** `parse_leading_feature_marker`
/// reports what this function decided rather than tokenizing again, so adding
/// the spoken phrase could not create a second `@vibedev` judge that disagrees
/// with this one — which matters more for voice than for typing, because the
/// spoken grammar is the half a future edit is most likely to widen.
pub fn parse_vibedev_rail_invocation(text: &str) -> Option<VibedevRailInvocation> {
    let tokens = voice_command_tokens(text);
    let invoke = tokens.first()?;
    // The tokenizer skips leading punctuation, so check the gap it skipped: an
    // `@vibedev` in quotes or brackets is someone describing the rail, not using
    // it.
    let invoke_start = invoke.end.saturating_sub(invoke.normalized.len());
    if !text
        .get(..invoke_start)
        .is_some_and(|prefix| prefix.trim().is_empty())
    {
        return None;
    }

    let is_marker = VIBEDEV_MARKER_INVOKE_WORDS.contains(&invoke.normalized.as_str());
    let (discuss, consumed_end) = if is_marker {
        match tokens.get(1) {
            Some(flag) if flag.normalized == VIBEDEV_DISCUSS_FLAG => (true, flag.end),
            _ => (false, invoke.end),
        }
    } else {
        parse_spoken_vibedev_rail_phrase(&tokens)?
    };
    let prompt = text
        .get(consumed_end..)
        .unwrap_or_default()
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';' | '-'))
        .trim()
        .to_string();

    Some(VibedevRailInvocation { discuss, prompt })
}
