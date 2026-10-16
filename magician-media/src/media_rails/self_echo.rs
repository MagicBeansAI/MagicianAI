//! Self-echo rejection for voice transcript admission.
//!
//! When a device's acoustic echo cancellation underperforms, the assistant's
//! own TTS comes back through the microphone as a user transcript. With
//! server VAD and auto-response, every such transcript becomes another spoken
//! reply — an unbounded self-hearing loop, because each cycle also refreshes
//! the address follow-up window. This module is the server-side defense: it
//! tracks what the assistant recently said, estimates until when the device
//! speaker is still playing it, and flags user transcripts that are — with
//! high confidence — that same speech leaking back in.
//!
//! The playback window is *computed, not guessed*: the backend streams the
//! assistant's PCM to the client itself (`ProviderAudioFrame`), so accumulating
//! frame durations behind a speaker-queue clock — exactly like the iOS
//! `AssistantPlaybackClock` — gives the instant the speaker runs dry. Only for
//! direct peer-to-peer topologies, where audio never crosses magician, does a
//! conservative fixed window substitute.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Control-socket binary frames are fixed 24 kHz mono PCM16 — never negotiated
/// (`session.ready` carries no sample rate; clients are written against exactly
/// this format). Duration of a frame = bytes / (24000 * 2) seconds.
const PLAYBACK_BYTES_PER_SECOND: f64 = 24_000.0 * 2.0;

/// Grace past the estimated end of playback. Covers client jitter buffers,
/// speaker latency, and the transcription lag of an echo captured in the last
/// instants of playback.
const PLAYBACK_TAIL: Duration = Duration::from_secs(2);

/// Substitute window for utterances whose audio never crossed the backend
/// (DirectPeerToPeer WebRTC: the browser talks straight to the vendor). No
/// byte count exists server-side, and estimating duration from text length is
/// explicitly not trusted, so a fixed window anchored at the moment the
/// transcript was observed stands in. Long enough to cover most replies;
/// direct P2P clients also run WebRTC's own echo cancellation, so this path is
/// a backstop rather than the primary defense.
const FALLBACK_PLAYBACK_WINDOW: Duration = Duration::from_secs(15);

/// Ring size: the assistant's last utterances kept for matching. Echoes are of
/// *recent* speech; anything older has aged out of its playback window anyway.
const MAX_TRACKED_UTTERANCES: usize = 3;

/// A user transcript whose tokens are at least this contained in a tracked
/// assistant utterance is treated as that utterance's echo.
const CONTAINMENT_THRESHOLD: f64 = 0.8;

/// Transcripts of at most this many tokens are never rejected on containment
/// alone — see the boundary comment in [`SelfEchoSuppressor::match_transcript`].
const SHORT_TRANSCRIPT_MAX_TOKENS: usize = 2;

/// Evidence returned when a user transcript matches recent assistant speech.
/// Carried into the rejection log so every dropped echo is auditable.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfEchoMatch {
    /// Fraction of the user transcript's tokens found in the matched assistant
    /// utterance (1.0 for the exact-tail rule).
    pub containment: f64,
    /// Which rule fired: `"containment"` or `"exact_tail"`.
    pub matched_rule: &'static str,
    /// How the matched utterance's window was derived: `"audio_bytes"` from the
    /// streamed-PCM speaker-queue clock, or `"fixed_fallback"` when no audio
    /// crossed the backend.
    pub window_source: &'static str,
    /// Milliseconds of estimated playback (plus tail) that remained when the
    /// transcript arrived.
    pub window_remaining_ms: u64,
}

#[derive(Debug)]
struct TrackedUtterance {
    /// Provider response id, when known. Links transcript text to the audio
    /// frames of the same response; `None` for client-reported transcripts.
    response_id: Option<String>,
    /// Caption text: accumulated deltas until the final replaces them.
    text: String,
    /// When the utterance was first observed — anchors the fallback window.
    noted_at: Instant,
    /// Estimated instant the device speaker finishes this utterance, from the
    /// speaker-queue clock over streamed audio bytes. `None` until the first
    /// frame is attributed (then the fallback window applies).
    playback_end: Option<Instant>,
    /// A final transcript replaced the accumulated deltas; late deltas for the
    /// same response no longer append.
    finalized: bool,
}

impl TrackedUtterance {
    fn window_end(&self) -> Instant {
        self.playback_end
            .unwrap_or(self.noted_at + FALLBACK_PLAYBACK_WINDOW)
    }

    fn window_source(&self) -> &'static str {
        if self.playback_end.is_some() {
            "audio_bytes"
        } else {
            "fixed_fallback"
        }
    }
}

/// Per-session tracker of recent assistant speech and its estimated playback,
/// consulted at transcript admission. Time is always passed in so the decision
/// logic stays purely assertable.
#[derive(Debug, Default)]
pub struct SelfEchoSuppressor {
    utterances: VecDeque<TrackedUtterance>,
    /// Speaker-queue clock: when the device runs dry of *all* queued assistant
    /// audio. Frames streamed faster than realtime stack behind each other
    /// (`max(idle, now) + frame duration`), so a response queued behind another
    /// gets a window that starts where the previous one ends.
    queue_idle_at: Option<Instant>,
}

impl SelfEchoSuppressor {
    /// Record a streamed caption fragment of the assistant's in-progress
    /// response. Fragments accumulate so an echo captured mid-response can be
    /// matched before the final caption exists.
    pub fn note_assistant_delta(&mut self, response_id: &str, text: &str, now: Instant) {
        if let Some(entry) = self.find_mut(Some(response_id)) {
            if !entry.finalized {
                entry.text.push_str(text);
            }
            return;
        }
        self.push(TrackedUtterance {
            response_id: Some(response_id.to_string()),
            text: text.to_string(),
            noted_at: now,
            playback_end: None,
            finalized: false,
        });
    }

    /// Record the complete caption of an assistant utterance, replacing any
    /// accumulated deltas for the same response.
    pub fn note_assistant_final(&mut self, response_id: Option<&str>, text: &str, now: Instant) {
        if let Some(entry) = response_id.and_then(|id| self.find_mut(Some(id))) {
            entry.text = text.to_string();
            entry.finalized = true;
            return;
        }
        self.push(TrackedUtterance {
            response_id: response_id.map(str::to_string),
            text: text.to_string(),
            noted_at: now,
            playback_end: None,
            finalized: true,
        });
    }

    /// Account one provider→client PCM frame against the speaker-queue clock
    /// and extend the owning utterance's playback window to the new idle time.
    pub fn note_assistant_audio(
        &mut self,
        response_id: Option<&str>,
        frame_bytes: usize,
        now: Instant,
    ) {
        if frame_bytes == 0 {
            return;
        }
        let seconds = frame_bytes as f64 / PLAYBACK_BYTES_PER_SECOND;
        let base = self.queue_idle_at.filter(|idle| *idle > now).unwrap_or(now);
        let idle = base + Duration::from_secs_f64(seconds);
        self.queue_idle_at = Some(idle);
        if let Some(entry) = self.find_mut(response_id) {
            entry.playback_end = Some(idle);
        } else if let Some(id) = response_id {
            // Audio can race ahead of the first caption delta (frames and
            // provider events travel separate channels). Open the entry now;
            // the deltas fill in its text when they arrive.
            self.push(TrackedUtterance {
                response_id: Some(id.to_string()),
                text: String::new(),
                noted_at: now,
                playback_end: Some(idle),
                finalized: false,
            });
        } else if let Some(entry) = self.utterances.back_mut() {
            entry.playback_end = Some(idle);
        }
    }

    /// Close a response's audio stream. An interrupted response stops sounding
    /// immediately, so its window collapses to the present instead of running
    /// to the end of the bytes that were queued but never played.
    pub fn note_assistant_audio_done(
        &mut self,
        response_id: &str,
        interrupted: bool,
        now: Instant,
    ) {
        if !interrupted {
            return;
        }
        if let Some(entry) = self.find_mut(Some(response_id)) {
            if entry.window_end() > now {
                entry.playback_end = Some(now);
            }
        }
        self.clamp_queue(now);
    }

    /// The client halted playback outright (barge-in / cascaded interrupt):
    /// every open window collapses to the present.
    pub fn truncate_playback(&mut self, now: Instant) {
        for entry in &mut self.utterances {
            if entry.window_end() > now {
                entry.playback_end = Some(now);
            }
        }
        self.clamp_queue(now);
    }

    /// Decide whether a user transcript is, with high confidence, the echo of
    /// a tracked assistant utterance still inside its playback window (+ tail).
    ///
    /// Matching is asymmetric by design: an echo is a subset/garbling of the
    /// TTS, so the test is token *containment* of the user transcript in the
    /// assistant utterance, never symmetric similarity — a real barge-in
    /// ("stop", "no, I meant…") shares few tokens with the reply it interrupts.
    pub fn match_transcript(&self, transcript: &str, now: Instant) -> Option<SelfEchoMatch> {
        let user_tokens = fold_tokens(transcript);
        if user_tokens.is_empty() {
            return None;
        }
        for entry in self.utterances.iter().rev() {
            let window_close = entry.window_end() + PLAYBACK_TAIL;
            if now > window_close {
                // Past the window: the same words later are a user genuinely
                // repeating the assistant, which is legal.
                continue;
            }
            let assistant_tokens = fold_tokens(&entry.text);
            if assistant_tokens.is_empty() {
                continue;
            }
            let window_remaining_ms =
                window_close.saturating_duration_since(now).as_millis() as u64;
            // Boundary: transcripts of one or two tokens are exactly the
            // barge-ins this gate must never eat ("stop", "yes", "no"), and
            // almost any short word appears somewhere in a long reply, so
            // containment carries no signal at that length. A short transcript
            // is only treated as echo when it *exactly equals the utterance's
            // tail* — the failure mode where AEC clips everything but the last
            // word or two of the TTS.
            if user_tokens.len() <= SHORT_TRANSCRIPT_MAX_TOKENS {
                if assistant_tokens.len() >= user_tokens.len()
                    && assistant_tokens[assistant_tokens.len() - user_tokens.len()..]
                        == user_tokens[..]
                {
                    return Some(SelfEchoMatch {
                        containment: 1.0,
                        matched_rule: "exact_tail",
                        window_source: entry.window_source(),
                        window_remaining_ms,
                    });
                }
                continue;
            }
            let containment = containment_ratio(&user_tokens, &assistant_tokens);
            if containment >= CONTAINMENT_THRESHOLD {
                return Some(SelfEchoMatch {
                    containment,
                    matched_rule: "containment",
                    window_source: entry.window_source(),
                    window_remaining_ms,
                });
            }
        }
        None
    }

    fn find_mut(&mut self, response_id: Option<&str>) -> Option<&mut TrackedUtterance> {
        let response_id = response_id?;
        self.utterances
            .iter_mut()
            .rev()
            .find(|entry| entry.response_id.as_deref() == Some(response_id))
    }

    fn push(&mut self, entry: TrackedUtterance) {
        self.utterances.push_back(entry);
        while self.utterances.len() > MAX_TRACKED_UTTERANCES {
            self.utterances.pop_front();
        }
    }

    fn clamp_queue(&mut self, now: Instant) {
        if self.queue_idle_at.is_some_and(|idle| idle > now) {
            self.queue_idle_at = Some(now);
        }
    }
}

/// Normalization shared by both sides of the match: lowercase alphanumeric
/// tokens, punctuation and whitespace stripped.
fn fold_tokens(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Multiset containment: the fraction of user tokens present in the assistant
/// utterance, each assistant token spendable once. Counting occurrences keeps
/// a stuttered barge-in ("no no no") from matching a reply that said "no" once.
fn containment_ratio(user_tokens: &[String], assistant_tokens: &[String]) -> f64 {
    let mut available: HashMap<&str, usize> = HashMap::new();
    for token in assistant_tokens {
        *available.entry(token.as_str()).or_default() += 1;
    }
    let mut matched = 0usize;
    for token in user_tokens {
        if let Some(count) = available.get_mut(token.as_str()) {
            if *count > 0 {
                *count -= 1;
                matched += 1;
            }
        }
    }
    matched as f64 / user_tokens.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_rails::voice_addressing::{VoiceAddressing, VoiceAddressingDecision};

    /// PCM16 mono at the fixed 24 kHz transport rate.
    fn seconds_of_audio(seconds: f64) -> usize {
        (seconds * PLAYBACK_BYTES_PER_SECOND) as usize
    }

    fn suppressor_with_reply(text: &str, audio_seconds: f64, at: Instant) -> SelfEchoSuppressor {
        let mut echo = SelfEchoSuppressor::default();
        echo.note_assistant_final(Some("resp-1"), text, at);
        echo.note_assistant_audio(Some("resp-1"), seconds_of_audio(audio_seconds), at);
        echo
    }

    #[test]
    fn verbatim_echo_within_playback_window_is_rejected() {
        let t0 = Instant::now();
        let echo = suppressor_with_reply("I moved the design review to Thursday at four.", 4.0, t0);
        let matched = echo
            .match_transcript(
                "I moved the design review to Thursday at four",
                t0 + Duration::from_secs(2),
            )
            .expect("verbatim echo during playback must match");
        assert_eq!(matched.matched_rule, "containment");
        assert!(matched.containment > 0.99);
        assert_eq!(matched.window_source, "audio_bytes");
    }

    #[test]
    fn garbled_echo_with_most_tokens_within_window_is_rejected() {
        let t0 = Instant::now();
        let echo = suppressor_with_reply("The report is ready and I sent a copy to Maya.", 5.0, t0);
        // AEC garbling: nine of ten heard tokens are the assistant's own.
        let matched = echo
            .match_transcript(
                "the report is ready and sent a copy to my",
                t0 + Duration::from_secs(3),
            )
            .expect("a mostly-contained garbling during playback must match");
        assert_eq!(matched.matched_rule, "containment");
        assert!(matched.containment >= CONTAINMENT_THRESHOLD);
    }

    #[test]
    fn same_text_after_window_and_tail_is_admitted() {
        let t0 = Instant::now();
        let echo = suppressor_with_reply("I moved the design review to Thursday at four.", 4.0, t0);
        // Playback ends at t0+4s, tail closes at t0+6s. A user genuinely
        // repeating the assistant afterwards is legal.
        assert_eq!(
            echo.match_transcript(
                "I moved the design review to Thursday at four",
                t0 + Duration::from_secs(7),
            ),
            None
        );
    }

    #[test]
    fn real_barge_in_during_playback_is_admitted() {
        let t0 = Instant::now();
        let echo = suppressor_with_reply(
            "I scheduled the team sync for nine tomorrow and invited the whole design group.",
            8.0,
            t0,
        );
        let mid_playback = t0 + Duration::from_secs(2);
        // One-word interjection: short transcripts never match on containment,
        // and "stop" is not the utterance's tail.
        assert_eq!(echo.match_transcript("stop", mid_playback), None);
        // Multi-word correction sharing almost no tokens with the reply.
        assert_eq!(echo.match_transcript("no not that one", mid_playback), None);
    }

    #[test]
    fn short_transcript_matches_only_the_exact_tail() {
        let t0 = Instant::now();
        let echo = suppressor_with_reply("Does Thursday at four work for you, okay?", 3.0, t0);
        let mid_playback = t0 + Duration::from_secs(1);
        // Documented boundary: a short transcript equal to the utterance's
        // tail during playback is the clipped-echo failure mode → rejected.
        let matched = echo
            .match_transcript("okay", mid_playback)
            .expect("exact tail during playback must match");
        assert_eq!(matched.matched_rule, "exact_tail");
        // Any other short answer survives, even during playback.
        assert_eq!(echo.match_transcript("yes", mid_playback), None);
        // And the same tail word after the window + tail is a real answer.
        assert_eq!(
            echo.match_transcript("okay", t0 + Duration::from_secs(6)),
            None
        );
    }

    #[test]
    fn echo_is_caught_before_the_address_gate_and_released_after_the_window() {
        // Mirrors `admit_user_transcript` ordering: the echo check runs before
        // `VoiceAddressing::admit`, so with the prefix gate ON an echoed wake
        // phrase inside the assistant's own sentence can neither arm nor spend
        // the follow-up window — and after the window the identical words are
        // an ordinary addressed utterance.
        let t0 = Instant::now();
        let echo = suppressor_with_reply("Hey Sam is what you say to wake me up.", 3.0, t0);
        let mut gate = VoiceAddressing::new(true, ["Sam".to_string()]);
        let leak = "Hey Sam is what you say to wake me up";

        assert!(echo
            .match_transcript(leak, t0 + Duration::from_secs(1))
            .is_some());
        // Gate untouched by the rejected echo: a later plain utterance still
        // needs its own address.
        assert_eq!(
            gate.admit("send the update"),
            VoiceAddressingDecision::Rejected
        );

        let after_window = t0 + Duration::from_secs(10);
        assert_eq!(echo.match_transcript(leak, after_window), None);
        assert!(matches!(
            gate.admit(leak),
            VoiceAddressingDecision::Admitted(_)
        ));
    }

    #[test]
    fn in_progress_deltas_match_before_the_final_caption_exists() {
        // The echo of a long reply is transcribed mid-response, before the
        // provider's final caption event. Accumulated deltas must already match.
        let t0 = Instant::now();
        let mut echo = SelfEchoSuppressor::default();
        echo.note_assistant_delta("resp-1", "I found three flights ", t0);
        echo.note_assistant_delta("resp-1", "leaving Friday evening", t0);
        echo.note_assistant_audio(Some("resp-1"), seconds_of_audio(6.0), t0);
        assert!(echo
            .match_transcript(
                "I found three flights leaving Friday",
                t0 + Duration::from_secs(2)
            )
            .is_some());
    }

    #[test]
    fn fallback_window_applies_when_audio_never_crosses_the_backend() {
        let t0 = Instant::now();
        let mut echo = SelfEchoSuppressor::default();
        // Direct P2P: caption reported by the client, zero audio bytes seen.
        echo.note_assistant_final(None, "Your next meeting starts in ten minutes.", t0);
        let matched = echo
            .match_transcript(
                "your next meeting starts in ten minutes",
                t0 + Duration::from_secs(5),
            )
            .expect("echo inside the fixed fallback window must match");
        assert_eq!(matched.window_source, "fixed_fallback");
        assert_eq!(
            echo.match_transcript(
                "your next meeting starts in ten minutes",
                t0 + FALLBACK_PLAYBACK_WINDOW + PLAYBACK_TAIL + Duration::from_secs(1),
            ),
            None
        );
    }

    #[test]
    fn interrupted_playback_collapses_the_window() {
        let t0 = Instant::now();
        let mut echo = suppressor_with_reply("Here is the long answer you asked about.", 10.0, t0);
        echo.note_assistant_audio_done("resp-1", true, t0 + Duration::from_secs(1));
        // Bytes said t0+10s, but the interrupt stopped the speaker at t0+1s:
        // by t0+4s the tail has passed and the words are admissible again.
        assert_eq!(
            echo.match_transcript(
                "here is the long answer you asked about",
                t0 + Duration::from_secs(4),
            ),
            None
        );
    }

    #[test]
    fn queued_responses_stack_behind_each_other_like_a_speaker_queue() {
        let t0 = Instant::now();
        let mut echo = SelfEchoSuppressor::default();
        echo.note_assistant_final(
            Some("resp-1"),
            "First answer that takes a while to say.",
            t0,
        );
        echo.note_assistant_audio(Some("resp-1"), seconds_of_audio(4.0), t0);
        // Second response streamed one second in: it plays only after the
        // first drains, so its window ends at t0+8s (tail closes t0+10s) —
        // not at t1+4s = t0+5s (tail t0+7s), which is what a clock rebased on
        // `now` instead of `max(idle, now)` would say.
        let t1 = t0 + Duration::from_secs(1);
        echo.note_assistant_final(Some("resp-2"), "Second answer queued right behind it.", t1);
        echo.note_assistant_audio(Some("resp-2"), seconds_of_audio(4.0), t1);
        // Probed at t0+9s: past the broken close (t0+7s), inside the stacked
        // one (t0+10s). A probe at t0+7s or earlier sits inside BOTH windows
        // (the close comparison is exclusive), so it passes with stacking
        // broken — which is how this test once pinned nothing.
        assert!(echo
            .match_transcript(
                "second answer queued right behind it",
                t0 + Duration::from_secs(9),
            )
            .is_some());
    }
}
