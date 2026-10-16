//! Answering an alignment hold on a live voice call (plan §10.1, P6).
//!
//! Voice is a shared audience by default: a speaker, a car, a room. So a call
//! hears only the generic notice, and only choices that neither consent to
//! the act nor change what Magician remembers can be answered by speech:
//! cancel, try the check again, do something else, and (on a wording stage)
//! don't change it. Allowing the act, proceeding without the check and every
//! lasting change stay on the owner's private app surfaces, where the reason
//! is shown; the respond policy refuses them from `voice` regardless.
//!
//! The answer is bound to the ask that was read out, never inferred:
//!
//! 1. A `hitl.requested` for a hold reaches the call as one
//!    [`VOICE_REVIEW_REQUESTED`] message ([`VoiceReview`]) carrying the full
//!    correlation id (`storage_key~ask_id`) and the voice-answerable choices.
//! 2. The call speaks [`VoiceReview::announcement`] and remembers the review.
//! 3. A finalized owner utterance that is exactly one of those choices
//!    ([`VoiceReview::match_utterance`]) is answered by the server, through
//!    the installed [`VoiceReviewAnswerSink`], for that correlation id under
//!    channel `voice`. Anything else is ordinary conversation and answers
//!    nothing. An utterance that sounds like consent ("yes, go ahead") is
//!    told where to give it instead.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ask::GENERIC_ASK_TEXT;
use crate::magician_v2::media_seam::VoiceDownstreamMessage;

/// Downstream kind a live call receives for a new hold.
pub const VOICE_REVIEW_REQUESTED: &str = "alignment.review_requested";

/// Downstream kind that closes a call's open review: its run resumed or
/// finished, however the hold was answered.
pub const VOICE_REVIEW_CLOSED: &str = "alignment.review_closed";

/// How long after the announcement an utterance can answer the review. Past
/// it, speech is ordinary conversation again; the hold stays answerable in
/// the app.
pub const VOICE_ANSWER_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The channel a spoken answer is recorded under.
pub const VOICE_ANSWER_CHANNEL: &str = "voice";

/// Choice ids a spoken answer may carry: none consents to the act, none
/// changes a preference.
const VOICE_ANSWERABLE: &[&str] = &["cancel", "retry", "use_alternative", "keep_as_is"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceChoice {
    pub id: String,
    pub label: String,
}

/// One hold as a voice call may hear and answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceReview {
    /// `storage_key~ask_id`: the exact ask an answer is for.
    pub correlation_id: String,
    pub ask_id: String,
    pub choices: Vec<VoiceChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    /// The stage that was read out (`decide`, `confirm_wording`). An answer
    /// is applied only while the ask is still at this stage.
    #[serde(default)]
    pub stage: String,
    /// The held run, so a resolution of that run closes the review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
}

/// What a finalized utterance means while a review is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceUtterance<'a> {
    /// Exactly one of the review's spoken choices.
    Choice(&'a VoiceChoice),
    /// Sounds like approval, which only a private surface can give.
    Consent,
    /// Anything else: ordinary conversation, answers nothing.
    Other,
}

fn normalize(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' || c == ' ' { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    // Courtesy words around a command do not change it.
    while words.first().is_some_and(|word| matches!(*word, "please" | "ok" | "okay")) {
        words.remove(0);
    }
    while words.last().is_some_and(|word| matches!(*word, "please" | "thanks")) {
        words.pop();
    }
    words.join(" ")
}

/// The spoken forms that answer a choice. Each names the held action or the
/// review, so ordinary speech ("cancel", "try again") meant for something
/// else on the call never answers it. The first form is the one announced.
fn spoken_forms(id: &str) -> &'static [&'static str] {
    match id {
        "cancel" => &[
            "cancel the action",
            "cancel the held action",
            "don't run the action",
            "do not run the action",
        ],
        "retry" => &["check the action again", "try the check again"],
        "use_alternative" => &["do something else instead", "do something else for the action"],
        "keep_as_is" => &["keep my preference", "don't change my preference"],
        _ => &[],
    }
}

/// Utterances that would be consent on a private surface.
const CONSENT_PHRASES: &[&str] = &[
    "yes",
    "yeah",
    "yep",
    "sure",
    "go ahead",
    "yes go ahead",
    "do it",
    "allow it",
    "allow this once",
    "allow it once",
    "approve it",
    "approve",
    "proceed",
    "proceed anyway",
    "proceed without the check",
];

impl VoiceReview {
    /// The review for a canonical `hitl.requested`, from its public
    /// `input_schema` alone. `None` unless it is an alignment hold whose
    /// correlation id names its ask.
    pub fn from_hitl(correlation_id: &str, input_schema: &Value) -> Option<Self> {
        if input_schema.get("escalation_trigger")?.as_str()? != "preference_alignment" {
            return None;
        }
        let alignment = input_schema.get("alignment")?;
        if alignment.get("kind")?.as_str()? != "preference_alignment" {
            return None;
        }
        let ask_id = alignment.get("ask_id")?.as_str()?.trim();
        // The answer must name the ask; a bare storage key names none.
        let (_, named) = correlation_id.rsplit_once('~')?;
        if ask_id.is_empty() || named != ask_id {
            return None;
        }
        let choices: Vec<VoiceChoice> = alignment
            .get("choices")?
            .as_array()?
            .iter()
            .filter_map(|choice| {
                let id = choice.get("id")?.as_str()?;
                let label = choice.get("label")?.as_str()?;
                VOICE_ANSWERABLE.contains(&id).then(|| VoiceChoice {
                    id: id.to_string(),
                    label: label.to_string(),
                })
            })
            .collect();
        Some(Self {
            correlation_id: correlation_id.to_string(),
            ask_id: ask_id.to_string(),
            choices,
            deadline_at_ms: alignment.get("deadline_at_ms").and_then(Value::as_i64),
            stage: alignment
                .get("stage")
                .and_then(Value::as_str)
                .unwrap_or("decide")
                .to_string(),
            execution_id: None,
        })
    }

    pub fn for_execution(mut self, execution_id: &str) -> Self {
        self.execution_id = Some(execution_id.to_string());
        self
    }

    /// The message that closes any open review of `execution_id` on a call.
    pub fn closed_message(execution_id: &str) -> VoiceDownstreamMessage {
        VoiceDownstreamMessage {
            kind: VOICE_REVIEW_CLOSED.to_string(),
            payload: json!({ "execution_id": execution_id }),
        }
    }

    /// Whether a [`VOICE_REVIEW_CLOSED`] payload closes this review.
    pub fn closed_by(&self, payload: &Value) -> bool {
        match (&self.execution_id, payload.get("execution_id").and_then(Value::as_str)) {
            (Some(mine), Some(closed)) => mine == closed,
            // A review that does not know its run closes on any resolution.
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    /// The downstream message a live call receives.
    pub fn message(&self) -> VoiceDownstreamMessage {
        VoiceDownstreamMessage {
            kind: VOICE_REVIEW_REQUESTED.to_string(),
            payload: serde_json::to_value(self).unwrap_or(Value::Null),
        }
    }

    /// Read a [`VOICE_REVIEW_REQUESTED`] payload back.
    pub fn from_payload(payload: &Value) -> Option<Self> {
        let review: Self = serde_json::from_value(payload.clone()).ok()?;
        let (_, named) = review.correlation_id.rsplit_once('~')?;
        (named == review.ask_id && !review.ask_id.is_empty()).then_some(review)
    }

    /// What the call says, word for word: the generic notice and what can
    /// be said back. Names no preference, reason or act.
    pub fn announcement(&self) -> String {
        let spoken = self
            .choices
            .iter()
            .filter_map(|choice| spoken_forms(&choice.id).first())
            .map(|form| format!("\"{form}\""))
            .collect::<Vec<_>>()
            .join(" or ");
        let options = if spoken.is_empty() {
            String::new()
        } else {
            format!(" Here, you can say {spoken}.")
        };
        format!("{GENERIC_ASK_TEXT} To allow it, use Magician on your phone or the web.{options}")
    }

    /// Whether `now_ms` is past the ask's answer window.
    pub fn expired(&self, now_ms: i64) -> bool {
        self.deadline_at_ms.is_some_and(|deadline| now_ms > deadline)
    }

    /// Read a finalized utterance against this review. Only one of a
    /// choice's spoken forms, each naming the action, answers it.
    pub fn match_utterance(&self, transcript: &str) -> VoiceUtterance<'_> {
        let said = normalize(transcript);
        if said.is_empty() {
            return VoiceUtterance::Other;
        }
        if let Some(choice) = self
            .choices
            .iter()
            .find(|choice| spoken_forms(&choice.id).contains(&said.as_str()))
        {
            return VoiceUtterance::Choice(choice);
        }
        if CONSENT_PHRASES.contains(&said.as_str()) {
            return VoiceUtterance::Consent;
        }
        VoiceUtterance::Other
    }
}

/// What the call says, word for word, after a spoken answer was applied.
pub fn spoken_confirmation(choice_id: &str) -> &'static str {
    match choice_id {
        "cancel" => "Cancelled. It won't run.",
        "retry" => "OK, checking it again.",
        "use_alternative" => "OK, it won't run as proposed. What should I do instead?",
        "keep_as_is" => "Nothing was changed. The action is still on hold.",
        _ => "Your answer was recorded.",
    }
}

/// What the call says, word for word, when an utterance sounded like
/// approval: it is not taken here.
pub const SPOKEN_CONSENT_REFUSAL: &str = "To allow it, use Magician on your phone or the web, \
     where you can see why it was held. It has not been allowed.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAnswerOutcome {
    Accepted,
    /// Answered, replaced or expired already; nothing was applied.
    AlreadyResolved,
    Refused(String),
}

impl VoiceAnswerOutcome {
    /// What the call says for this outcome of answering `choice_id`.
    pub fn spoken(&self, choice_id: &str) -> String {
        match self {
            Self::Accepted => spoken_confirmation(choice_id).to_string(),
            Self::AlreadyResolved => "That review was already answered or has closed.".to_string(),
            Self::Refused(_) => "I couldn't apply that here. You can answer in Magician on your \
                 phone or the web."
                .to_string(),
        }
    }
}

/// Applies a spoken answer through the same resume path, and the same
/// answer policy, as `POST /hitl/{id}/respond`. Installed by the API layer.
#[async_trait]
pub trait VoiceReviewAnswerSink: Send + Sync {
    async fn answer(
        &self,
        principal: &str,
        workspace: &str,
        review: &VoiceReview,
        choice_id: &str,
    ) -> VoiceAnswerOutcome;
}

static VOICE_ANSWER_SINK: OnceLock<Arc<dyn VoiceReviewAnswerSink>> = OnceLock::new();

/// Install the sink. First call wins.
pub fn install_voice_answer_sink(sink: Arc<dyn VoiceReviewAnswerSink>) {
    let _ = VOICE_ANSWER_SINK.set(sink);
}

pub fn voice_answer_sink() -> Option<Arc<dyn VoiceReviewAnswerSink>> {
    VOICE_ANSWER_SINK.get().cloned()
}

/// The structured log line for a spoken answer. Content-free.
pub fn voice_answer_log(review: &VoiceReview, choice_id: &str, outcome: &VoiceAnswerOutcome) -> Value {
    json!({
        "kind": "voice_answer",
        "ask_id": review.ask_id,
        "choice": choice_id,
        "outcome": match outcome {
            VoiceAnswerOutcome::Accepted => "accepted",
            VoiceAnswerOutcome::AlreadyResolved => "already_resolved",
            VoiceAnswerOutcome::Refused(_) => "refused",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::ask::{AlignmentAsk, CoveredGate, CoveredRequirement};
    use crate::magician_v2::preference_alignment::contract::ResolutionChoice;

    fn ask() -> AlignmentAsk {
        AlignmentAsk::for_hold(
            "ask-7".into(),
            "alignment_binding.v1:x".into(),
            Some("exec-1".into()),
            vec!["pref-1".into()],
            &[ResolutionChoice::UseAlternative],
            1_000,
            60,
            false,
            None,
        )
    }

    fn review() -> VoiceReview {
        VoiceReview::from_hitl("exec-1:plan:step~ask-7", &ask().hitl_input_schema("pack"))
            .expect("a hold is a voice review")
    }

    #[test]
    fn a_call_hears_only_the_choices_that_neither_consent_nor_remember() {
        let review = review();
        let ids: Vec<_> = review.choices.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(ids, vec!["use_alternative", "cancel"]);
        let message = review.message();
        assert_eq!(message.kind, VOICE_REVIEW_REQUESTED);
        assert_eq!(VoiceReview::from_payload(&message.payload), Some(review.clone()));
        let said = review.announcement();
        assert!(said.contains(GENERIC_ASK_TEXT));
        assert!(!said.contains("pref-1"));
        assert!(said.contains("\"do something else instead\" or \"cancel the action\""));
        assert!(!said.contains("Allow this once"), "consent is never offered aloud");
    }

    #[test]
    fn a_combined_ask_is_still_only_cancelled_or_redirected_by_voice() {
        let combined = ask().with_also_covers(vec![CoveredRequirement {
            gate: CoveredGate::ApprovalRule,
            reason: "rule".into(),
        }]);
        let review =
            VoiceReview::from_hitl("k~ask-7", &combined.hitl_input_schema("pack")).expect("review");
        assert!(review.choices.iter().all(|choice| choice.id != "allow_once"));
    }

    #[test]
    fn only_an_exact_choice_answers_and_approval_is_sent_to_the_app() {
        let review = review();
        assert!(matches!(
            review.match_utterance("Cancel the action, please."),
            VoiceUtterance::Choice(choice) if choice.id == "cancel"
        ));
        // A bare "cancel" may be meant for anything on the call.
        assert_eq!(review.match_utterance("cancel"), VoiceUtterance::Other);
        assert_eq!(review.match_utterance("cancel that"), VoiceUtterance::Other);
        assert!(matches!(
            review.match_utterance("do something else instead"),
            VoiceUtterance::Choice(choice) if choice.id == "use_alternative"
        ));
        assert_eq!(review.match_utterance("Yes, go ahead"), VoiceUtterance::Consent);
        assert_eq!(review.match_utterance("allow this once"), VoiceUtterance::Consent);
        // Ordinary speech that mentions a choice answers nothing.
        assert_eq!(
            review.match_utterance("cancel my 3pm meeting and order dinner"),
            VoiceUtterance::Other
        );
        assert_eq!(
            review.match_utterance("check the action again"),
            VoiceUtterance::Other,
            "retry was not offered"
        );
    }

    #[test]
    fn a_review_must_name_its_ask() {
        let schema = ask().hitl_input_schema("pack");
        assert!(VoiceReview::from_hitl("exec-1:plan:step", &schema).is_none());
        assert!(VoiceReview::from_hitl("exec-1:plan:step~other", &schema).is_none());
        assert!(VoiceReview::from_hitl("k~ask-7", &json!({"escalation_trigger": "approval"})).is_none());
        let mut payload = review().message().payload;
        payload["correlation_id"] = json!("k~someone-else");
        assert!(VoiceReview::from_payload(&payload).is_none());
    }

    #[test]
    fn a_review_closes_when_its_run_resolves() {
        let review = review().for_execution("exec-1");
        assert!(review.closed_by(&VoiceReview::closed_message("exec-1").payload));
        assert!(!review.closed_by(&VoiceReview::closed_message("exec-2").payload));
        assert_eq!(review.stage, "decide");
    }

    #[test]
    fn the_window_closes_at_the_deadline() {
        let review = review();
        let deadline = review.deadline_at_ms.expect("deadline");
        assert!(!review.expired(deadline));
        assert!(review.expired(deadline + 1));
    }
}
