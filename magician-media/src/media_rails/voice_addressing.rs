use std::time::{Duration, Instant};

const FOLLOW_UP_WINDOW: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub struct VoiceAddressing {
    required: bool,
    names: Vec<String>,
    armed_until: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAddressingDecision {
    Admitted(String),
    Armed,
    Rejected,
}

#[derive(Debug)]
struct Token {
    folded: String,
    start: usize,
    end: usize,
}

impl VoiceAddressing {
    pub fn new(required: bool, names: impl IntoIterator<Item = String>) -> Self {
        let mut normalized = Vec::new();
        for name in names {
            let mut words = tokens(&name);
            if words.first().is_some_and(|word| word.folded == "hey") {
                words.remove(0);
            }
            if words.is_empty() {
                continue;
            }
            let name = words
                .iter()
                .map(|word| &name[word.start..word.end])
                .collect::<Vec<_>>()
                .join(" ");
            if !normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&name))
            {
                normalized.push(name);
            }
        }
        normalized.sort_by_key(|name| std::cmp::Reverse(tokens(name).len()));
        Self {
            required: required && !normalized.is_empty(),
            names: normalized,
            armed_until: None,
        }
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub fn set_required(&mut self, required: bool) {
        self.required = required && !self.names.is_empty();
        self.armed_until = None;
    }

    pub fn activation_phrases(&self) -> Vec<String> {
        self.names
            .iter()
            .map(|name| format!("Hey {name}"))
            .collect()
    }

    pub fn follow_up_window_ms(&self) -> u64 {
        FOLLOW_UP_WINDOW.as_millis() as u64
    }

    pub fn admit(&mut self, transcript: &str) -> VoiceAddressingDecision {
        if !self.required {
            return non_empty(transcript)
                .map(VoiceAddressingDecision::Admitted)
                .unwrap_or(VoiceAddressingDecision::Rejected);
        }

        let transcript_tokens = tokens(transcript);
        if transcript_tokens
            .first()
            .is_some_and(|token| token.folded == "hey")
        {
            for name in &self.names {
                let name_tokens = tokens(name);
                if transcript_tokens.len() < name_tokens.len() + 1 {
                    continue;
                }
                if name_tokens
                    .iter()
                    .enumerate()
                    .all(|(index, expected)| transcript_tokens[index + 1].folded == expected.folded)
                {
                    let end = transcript_tokens[name_tokens.len()].end;
                    if let Some(remainder) = non_empty(trim_prefix_delimiters(&transcript[end..])) {
                        self.armed_until = None;
                        return VoiceAddressingDecision::Admitted(remainder);
                    }
                    self.armed_until = Some(Instant::now() + FOLLOW_UP_WINDOW);
                    return VoiceAddressingDecision::Armed;
                }
            }
        }

        if self
            .armed_until
            .take()
            .is_some_and(|deadline| Instant::now() <= deadline)
        {
            return non_empty(transcript)
                .map(VoiceAddressingDecision::Admitted)
                .unwrap_or(VoiceAddressingDecision::Rejected);
        }
        VoiceAddressingDecision::Rejected
    }

    pub fn provider_instruction(&self) -> Option<String> {
        if !self.required {
            return None;
        }
        let phrases = self
            .activation_phrases()
            .into_iter()
            .map(|phrase| format!("\"{phrase}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "Voice address control is enabled. Treat room speech as ambient and produce no reply, tool call, or conversation turn unless the user's utterance begins with one of these address phrases: {phrases}. For an addressed utterance, ignore the address phrase itself and respond only to the words after it. If an address phrase is spoken alone, stay silent and treat the next utterance after the brief pause as addressed."
        ))
    }
}

fn tokens(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut start = None;
    for (index, ch) in input.char_indices() {
        if ch.is_alphanumeric() {
            start.get_or_insert(index);
        } else if let Some(token_start) = start.take() {
            out.push(Token {
                folded: input[token_start..index].to_lowercase(),
                start: token_start,
                end: index,
            });
        }
    }
    if let Some(token_start) = start {
        out.push(Token {
            folded: input[token_start..].to_lowercase(),
            start: token_start,
            end: input.len(),
        });
    }
    out
}

fn trim_prefix_delimiters(input: &str) -> &str {
    input.trim_start_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                ',' | '.' | ':' | ';' | '-' | '–' | '—' | '!' | '?' | '\'' | '"' | '‘' | '’'
            )
    })
}

fn non_empty(input: &str) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_gate_preserves_non_empty_transcript() {
        let mut gate = VoiceAddressing::new(false, ["Sam".to_string()]);
        assert_eq!(
            gate.admit("  send the update  "),
            VoiceAddressingDecision::Admitted("send the update".to_string())
        );
        assert_eq!(gate.admit("   "), VoiceAddressingDecision::Rejected);
    }

    #[test]
    fn per_call_override_changes_gate_without_losing_names() {
        let mut gate = VoiceAddressing::new(true, ["Sam".to_string()]);
        gate.set_required(false);
        assert!(!gate.required());
        assert_eq!(
            gate.admit("send the update"),
            VoiceAddressingDecision::Admitted("send the update".to_string())
        );

        gate.set_required(true);
        assert!(gate.required());
        assert_eq!(gate.activation_phrases(), vec!["Hey Sam"]);
        assert_eq!(
            gate.admit("send the update"),
            VoiceAddressingDecision::Rejected
        );
    }

    #[test]
    fn addressed_turn_is_case_and_punctuation_tolerant_and_stripped() {
        let mut gate = VoiceAddressing::new(true, ["Sam".to_string(), "Nova".to_string()]);
        assert_eq!(
            gate.admit("Hey, SAM: send the update."),
            VoiceAddressingDecision::Admitted("send the update.".to_string())
        );
        assert_eq!(
            gate.admit("hey nova — what changed?"),
            VoiceAddressingDecision::Admitted("what changed?".to_string())
        );
    }

    #[test]
    fn aliases_and_multiword_names_use_token_boundaries_and_longest_match() {
        let mut gate = VoiceAddressing::new(
            true,
            [
                "Sam".to_string(),
                "Sam Wise".to_string(),
                "Hey Tulu".to_string(),
            ],
        );
        assert_eq!(
            gate.admit("Hey Sam Wise, start now"),
            VoiceAddressingDecision::Admitted("start now".to_string())
        );
        assert_eq!(
            gate.admit("Hey Tulu start now"),
            VoiceAddressingDecision::Admitted("start now".to_string())
        );
        assert_eq!(
            gate.admit("Hey Samantha start now"),
            VoiceAddressingDecision::Rejected
        );
    }

    #[test]
    fn ambient_or_mid_sentence_address_speech_is_rejected() {
        let mut gate = VoiceAddressing::new(true, ["Sam".to_string()]);
        assert_eq!(
            gate.admit("Can someone close the door?"),
            VoiceAddressingDecision::Rejected
        );
        assert_eq!(
            gate.admit("They said hey Sam, start now"),
            VoiceAddressingDecision::Rejected
        );
    }

    #[test]
    fn prefix_only_turn_arms_the_next_utterance() {
        let mut gate = VoiceAddressing::new(true, ["Sam".to_string()]);
        assert_eq!(gate.admit("Hey Sam!"), VoiceAddressingDecision::Armed);
        assert_eq!(
            gate.admit("send the update"),
            VoiceAddressingDecision::Admitted("send the update".to_string())
        );
        assert_eq!(
            gate.admit("and one more thing"),
            VoiceAddressingDecision::Rejected
        );
    }

    #[test]
    fn empty_name_set_disables_gate_instead_of_locking_out_voice() {
        let mut gate = VoiceAddressing::new(true, ["--".to_string()]);
        assert!(!gate.required());
        assert_eq!(
            gate.admit("start now"),
            VoiceAddressingDecision::Admitted("start now".to_string())
        );
    }
}

#[cfg(test)]
mod deployed_config_tests {
    use super::*;

    /// The real shipped config: primary agent `Magican`, no aliases, with
    /// `wake_spellings: [magical, magician]` — assembled exactly as
    /// `voice_control_handler` does (aliases, then the name, then the wake
    /// spellings).
    ///
    /// The spellings are the load-bearing part. `Magican` is absent from the
    /// on-device wake lexicon, so the spotter arms `Hey magical` instead; if
    /// this gate did not also admit it, every locally matched wake would be
    /// discarded here as unaddressed and the wake word would appear to do
    /// nothing.
    fn deployed_gate() -> VoiceAddressing {
        VoiceAddressing::new(
            true,
            [
                "Magican".to_string(),
                "magical".to_string(),
                "magician".to_string(),
            ],
        )
    }

    #[test]
    fn deployed_gate_admits_the_name_and_every_wake_spelling() {
        let mut gate = deployed_gate();
        assert!(
            gate.required(),
            "gate must be armed for the deployed config"
        );
        assert_eq!(
            gate.activation_phrases(),
            vec![
                "Hey Magican".to_string(),
                "Hey magical".to_string(),
                "Hey magician".to_string(),
            ]
        );
        assert_eq!(
            gate.admit("Hey Magican send the update"),
            VoiceAddressingDecision::Admitted("send the update".to_string())
        );
        // What the on-device spotter can actually arm, and therefore what it
        // will really send. Admitting this is the whole point of the spellings.
        assert_eq!(
            gate.admit("Hey magical, what's the weather?"),
            VoiceAddressingDecision::Admitted("what's the weather?".to_string())
        );
        assert_eq!(
            gate.admit("Hey magician send the update"),
            VoiceAddressingDecision::Admitted("send the update".to_string())
        );
    }

    #[test]
    fn deployed_gate_arms_on_a_bare_wake_phrase_then_admits_the_next_utterance() {
        let mut gate = deployed_gate();
        assert_eq!(gate.admit("Hey Magican"), VoiceAddressingDecision::Armed);
        assert_eq!(
            gate.admit("what's on my calendar"),
            VoiceAddressingDecision::Admitted("what's on my calendar".to_string())
        );
    }
}
