//! What a durable run is made of — composable work modules, Module C.
//!
//! Contract from §5: durability for *"a multi-session authenticated form that
//! spans days and requires an out-of-band verification step"*. The types here
//! make the grounding rules structural:
//!
//! - a stored [`Answer`] always cites evidence — the store refuses one that
//!   does not, so "answered from nothing" has no stored representation;
//! - a [`Gap`] is what an ungroundable question becomes, and resolving one
//!   takes a **named** person, because *"the owner's reply is new evidence"*;
//! - an [`Expectation`] is a wait on an event that arrives somewhere else, and
//!   holds the run in [`RunState::AwaitingExternal`] until the caller feeds
//!   the event in;
//! - [`RunState::Submitted`] is reachable only through a record carrying a
//!   named person and a reviewed act — *"the agent drafts the whole thing; a
//!   person presses send."*

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::AudienceRef;

/// One grounded answer.
///
/// §5's grounding rule: *"every emitted answer cites retrievable evidence —
/// never answer from nothing"*. An `Answer` value with empty refs can exist in
/// memory — it is plain data — but the store refuses to record one and the
/// fold refuses to apply one, so an ungrounded answer is unrepresentable as
/// stored state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub text: String,
    /// Refs to the evidence this answer was read from. For a gap resolution,
    /// the ref of the owner's reply belongs here — the reply IS the evidence.
    pub evidence_refs: Vec<String>,
}

impl Answer {
    /// Whether this answer meets the grounding bar: non-blank text, at least
    /// one evidence ref, and every ref non-blank.
    ///
    /// The `all` over the refs cannot pass vacuously — the emptiness check
    /// just before it refuses the zero-ref case outright.
    pub fn is_grounded(&self) -> bool {
        !self.text.trim().is_empty()
            && !self.evidence_refs.is_empty()
            && self
                .evidence_refs
                .iter()
                .all(|held| !held.trim().is_empty())
    }
}

/// One field of the form: what is filled, or that it is pending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldState {
    pub name: String,
    /// Whether the form demands this field before it can be reviewed.
    pub required: bool,
    /// `None` means pending — nothing has ever been recorded. That is a fact
    /// the review needs to see, not a zero to be papered over.
    pub answer: Option<Answer>,
    /// The run-wide change sequence number of this field's last change; `0`
    /// means never filled.
    ///
    /// Revisions are one monotonic sequence across the whole run rather than
    /// independent per-field counters, so a single watermark means something:
    /// with independent counters, two fields "at the same revision" would have
    /// changed at unrelated moments and [`Run::changed_since`] could not
    /// answer "what changed since I last looked". Each field's own sequence of
    /// revisions is still strictly increasing.
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

/// How a gap was closed: who answered, with what, when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapResolution {
    /// The owner's reply, grounded like any other answer — its evidence refs
    /// may cite the reply itself, because the reply IS the new evidence.
    pub answer: Answer,
    /// The named person whose reply this is. Never blank: an unattributed
    /// resolution would be synthesised text wearing an owner's authority.
    pub resolved_by: String,
    pub at: DateTime<Utc>,
}

/// A question the evidence store could not ground, owed to the owner.
///
/// §5: *"a question the evidence store cannot ground becomes an owner-facing
/// gap, never synthesised text. The owner's reply is new evidence."*
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    /// The declared field whose answer this question is blocking.
    pub field: String,
    /// The question, in the words the owner will be asked.
    pub question: String,
    pub raised_at: DateTime<Utc>,
    pub resolution: Option<GapResolution>,
}

impl Gap {
    pub fn is_open(&self) -> bool {
        self.resolution.is_none()
    }
}

/// Proof that the awaited event arrived: which event, when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectationFulfilment {
    /// Ref to the event the caller extracted — a message, a call record. The
    /// module never fetches it; the caller feeds it in.
    pub event_ref: String,
    pub at: DateTime<Utc>,
}

/// A wait on an out-of-band event.
///
/// §5's *"inbox-coupled verification — the agent must read its own inbox
/// mid-flow, extract the code, and continue. This is the primitive nobody
/// has."* The coupling runs through the CALLER: this module never reads an
/// inbox, a phone, or anything else. It records that a wait exists and which
/// event ended it. That absence is deliberate — it is what keeps the run
/// usable by any flow, whatever channel its verification arrives on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expectation {
    pub expectation_id: String,
    /// What is being waited for, in the caller's words.
    pub description: String,
    /// Where the event will arrive — an inbox, a phone. A hint for whoever
    /// watches; the module itself watches nothing.
    pub source_hint: String,
    pub raised_at: DateTime<Utc>,
    pub fulfilled: Option<ExpectationFulfilment>,
}

impl Expectation {
    pub fn is_open(&self) -> bool {
        self.fulfilled.is_none()
    }
}

/// The record that a person pressed send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// The named person. Never blank: the module never fires the gate itself,
    /// and an unnamed submission is how an agent would.
    pub by: String,
    /// The reviewed act that covered the send. §5: *"covered only by a
    /// reviewed batch, never a standing envelope."*
    pub act_ref: String,
    pub at: DateTime<Utc>,
}

/// Where a run stands.
///
/// Derived from the fold of the log, never stored: a stored state would need
/// somebody to keep it honest, and the one thing a multi-session run cannot
/// rely on is the previous session having cleaned up after its crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Being filled in.
    Drafting,
    /// At least one expectation is open — the run is waiting on an event that
    /// arrives somewhere else.
    AwaitingExternal,
    /// Everything required is grounded and nothing is being waited on. A
    /// judgement, not a lock: drafting may continue until a person submits.
    ReadyForReview,
    /// A person pressed send. Terminal; from here the log is evidence.
    Submitted,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drafting => "drafting",
            Self::AwaitingExternal => "awaiting_external",
            Self::ReadyForReview => "ready_for_review",
            Self::Submitted => "submitted",
        }
    }
}

/// One durable run: a form or portal flow that spans sessions and days.
///
/// §5's first piece, as a value: *"Resumable state — what is filled, what is
/// pending, what changed since. A form abandoned halfway is worse than one
/// never started."* Filled and pending are [`Self::fields`]; changed-since is
/// [`Self::changed_since`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    /// What this run is for, in the caller's words.
    pub purpose: String,
    /// The form or portal being run against.
    pub resource_ref: String,
    /// The relationship this run serves, when it serves one. Optional and
    /// descriptive — a run against the owner's own records binds nobody — and
    /// always a named, enumerable [`AudienceRef`]: there is no open-ended
    /// audience here any more than anywhere else in this codebase.
    pub audience: Option<AudienceRef>,
    /// Declaration order.
    pub fields: Vec<FieldState>,
    /// Raise order.
    pub gaps: Vec<Gap>,
    /// Raise order.
    pub expectations: Vec<Expectation>,
    pub submission: Option<Submission>,
    pub opened_at: DateTime<Utc>,
    pub opened_by: String,
}

impl Run {
    /// One field by name.
    pub fn field(&self, name: &str) -> Option<&FieldState> {
        self.fields.iter().find(|held| held.name == name)
    }

    /// Where this run stands, derived from its contents.
    ///
    /// `Submitted` outranks everything — the record of what went out does not
    /// regress. An open expectation outranks readiness (readiness also checks
    /// it, defensively): a run waiting on a verification code is waiting,
    /// whatever else is true of it.
    pub fn state(&self) -> RunState {
        if self.submission.is_some() {
            return RunState::Submitted;
        }
        if self.expectations.iter().any(Expectation::is_open) {
            return RunState::AwaitingExternal;
        }
        if self.is_ready_for_review() {
            return RunState::ReadyForReview;
        }
        RunState::Drafting
    }

    /// Every required field answered, no open gap on a required field, and no
    /// open expectation.
    ///
    /// A run with **no fields at all** is never ready: "every required field
    /// answered" would hold vacuously over an empty form, and a vacuously
    /// ready run is one an agent could carry to the submit gate with nothing
    /// drafted. A form of only optional fields IS reviewable once its fields
    /// exist — whether blank is acceptable is exactly the judgement the
    /// reviewing person makes.
    ///
    /// An open gap on an OPTIONAL field does not block: the gate protects
    /// required content, and the open question stays visible on the run for
    /// the review to weigh.
    fn is_ready_for_review(&self) -> bool {
        if self.fields.is_empty() {
            return false;
        }
        let every_required_answered = self
            .fields
            .iter()
            .filter(|held| held.required)
            .all(|held| held.answer.is_some());
        let open_gap_on_required = self
            .gaps
            .iter()
            .any(|gap| gap.is_open() && self.field(&gap.field).is_some_and(|held| held.required));
        let open_expectation = self.expectations.iter().any(Expectation::is_open);
        every_required_answered && !open_gap_on_required && !open_expectation
    }

    /// §5's "what changed since": every field whose revision is strictly above
    /// the watermark, in change order.
    ///
    /// The watermark to hold is [`Self::revision_high_water`] as of the last
    /// look. Strictly greater, so a field at exactly the watermark was already
    /// seen. A never-filled field sits at revision `0`, so no watermark ever
    /// reports it — a pending field is visible as `answer: None` in
    /// [`Self::fields`], not as a change. Declaring a field is likewise not a
    /// change: a revision is earned by an answer, not by existence.
    pub fn changed_since(&self, watermark: u64) -> Vec<&FieldState> {
        let mut out: Vec<&FieldState> = self
            .fields
            .iter()
            .filter(|held| held.revision > watermark)
            .collect();
        out.sort_by(|left, right| {
            left.revision
                .cmp(&right.revision)
                .then_with(|| left.name.cmp(&right.name))
        });
        out
    }

    /// The highest revision on the run — the watermark a caller snapshots so
    /// its next [`Self::changed_since`] reports exactly what moved in between.
    /// `0` when nothing has ever been filled.
    pub fn revision_high_water(&self) -> u64 {
        self.fields
            .iter()
            .map(|held| held.revision)
            .max()
            .unwrap_or(0)
    }
}
