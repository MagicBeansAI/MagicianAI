//! Negotiating a time with someone outside — composable work modules, Module A.
//!
//! Contract from §3: `propose(counterparty, window, duration, constraints) ->
//! slots[]; offer(slots) — sent on whatever channel the conversation is on;
//! absorb(reply) — accept | decline | counter-propose | silence; hold(slot);
//! reschedule(event, why)`.
//!
//! This module records the **negotiation**. It never touches a calendar and it
//! never sends anything: `propose -> slots[]` is the agent's judgment, and
//! `offer`/`hold` are outward acts the consumer performs. What lives here is
//! their refs — [`Negotiation::offer_act_ref`], [`Held::calendar_event_ref`] —
//! so the record links to the outward-assertions trail without owning it.
//!
//! # Identity is the caller's job
//!
//! §3: *"The invite returns on a channel. Their acceptance… must resolve to the
//! same counterparty."* Resolving a channel address to a person is channel
//! work. This module takes an
//! [`AudienceRef`](magician::magician_v2::audience::AudienceRef) and a
//! counterparty identity the caller has already resolved, and deliberately
//! never reads an inbox, a roster or a directory to resolve one itself —
//! loose coupling is the point, so any flow on any channel can use it.
//!
//! # Silence
//!
//! §3: *"Silence is a state. No answer is neither yes nor no; it is a thing to
//! chase later, which is Module D."* Silence is **derived from the clock at
//! read time**, never stored: a stored `silent` flag needs a sweep to have
//! written it, and the point of noticing silence is to catch what nobody
//! remembered. It is a property of an unanswered offer, not a transition —
//! which is why [`NegotiationState`] has no `Silent` arm and
//! [`Negotiation::silent_since`] exists instead. The chase itself is Module
//! D's job: [`Negotiation::silence_follow_up`] emits the
//! [`RecordObligation`] that hands it over, and this module chases nobody.
//! When the answer finally lands, [`Negotiation::silence_settlement`]
//! re-derives the identical tuple — the follow-up alone goes to `None` the
//! moment anything answers, which would strand a chase already recorded for
//! silence that had ripened.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::{ObligationDirection, RecordObligation};

/// One candidate meeting time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Slot {
    /// A slot must end after it starts.
    ///
    /// Zero-length is refused along with inverted: a slot nobody could sit in
    /// must not become a time somebody "accepted" — recording one would let an
    /// agreement stand on a time that cannot be met in.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> anyhow::Result<Self> {
        let slot = Self { start, end };
        if !slot.is_well_formed() {
            anyhow::bail!(
                "a slot must end after it starts; a zero or negative length slot cannot be met \
                 in, and recording one would let an agreement stand on a time nobody can attend"
            );
        }
        Ok(slot)
    }

    /// Whether the slot has positive length.
    ///
    /// Exists so the store can re-check slots that arrived through
    /// deserialisation rather than [`Slot::new`] — parsed data is not trusted
    /// to have been constructed correctly.
    pub fn is_well_formed(&self) -> bool {
        self.end > self.start
    }
}

/// What their reply said.
///
/// §3's `absorb(reply) — accept | decline | counter-propose | silence`, minus
/// silence: silence is the absence of a reply, so it cannot be a reply kind —
/// storing it as one would need somebody to notice and write it, which is the
/// exact dependency derived silence removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyKind {
    /// They took one of the standing slots.
    Accepted { slot: Slot },
    /// They said no.
    Declined { reason: Option<String> },
    /// They proposed different times. These **replace** the standing slots:
    /// once somebody counters, the original offer is no longer on the table.
    Countered { slots: Vec<Slot> },
}

impl ReplyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Declined { .. } => "declined",
            Self::Countered { .. } => "countered",
        }
    }
}

/// One reply from the counterparty, absorbed off whatever channel the
/// conversation is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// The message or transcript the reply was read from. **Required** — the
    /// owner reads the words, and a reply with no source cannot be checked
    /// against what was actually said. It is also the idempotency key: a
    /// re-read inbox re-delivers the same source, and must not double-record.
    pub source_ref: String,
    /// When they replied — the message's time, supplied by the caller, never
    /// this library's clock.
    pub at: DateTime<Utc>,
    pub kind: ReplyKind,
}

/// A slot placed on the calendar.
///
/// The calendar write itself is the consumer's outward act; this records its
/// ref so the negotiation links to the event it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub slot: Slot,
    pub calendar_event_ref: String,
    pub at: DateTime<Utc>,
}

/// How and when a negotiation ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub at: DateTime<Utc>,
    pub reason: String,
}

/// One past round that was booked and then moved — §3's `reschedule(event,
/// why)`, kept so the full history survives the reset: the dead times are
/// cleared from the current round, not from the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reschedule {
    pub at: DateTime<Utc>,
    /// Why the booked time died, in the caller's words.
    pub why: String,
    /// The hold that was released — the slot and the calendar event it had.
    pub released: Held,
}

/// Where a negotiation stands.
///
/// Every arm is a fact somebody asserted, so no arm needs the clock; the only
/// clock-derived observation is silence, which is deliberately not an arm —
/// see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegotiationState {
    /// No verdict stands. This covers both waits — theirs for a reply, and
    /// ours for a fresh offer after a reschedule. Which wait it is reads off
    /// `offered`: empty means the next move is ours, and the silence
    /// derivation guards on `offered` being non-empty so a sweep never chases
    /// a counterparty for an answer to an offer that is not standing.
    AwaitingReply,
    Accepted,
    Declined,
    Countered,
    Held,
    Closed,
}

impl NegotiationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingReply => "awaiting_reply",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Countered => "countered",
            Self::Held => "held",
            Self::Closed => "closed",
        }
    }

    /// Whether the negotiation is settled — nothing more may be absorbed.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Held | Self::Closed)
    }
}

/// One ask: negotiating a time with one counterparty for one purpose.
///
/// This is the fold of the append-only log — the **current round**. A
/// reschedule resets the round (offer and replies clear; the dead times are
/// dead) but the history survives in [`Negotiation::reschedules`] and in the
/// log itself. A close ends the round, not the identity: reopening the same
/// ask starts a fresh round under the same id, and the closed rounds stay in
/// the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Negotiation {
    pub negotiation_id: String,
    /// The relationship the ask lives in. Never open-ended: an audience is a
    /// named, enumerable set, and the kind is part of its key.
    pub audience: AudienceRef,
    /// Who we are asking, as an identity the caller has already resolved.
    pub counterparty: String,
    /// What the meeting is for, in the words the owner would recognise.
    pub purpose: String,
    /// The slots we currently have on offer. Empty after a reschedule — the
    /// old times are dead and a fresh offer is owed.
    pub offered: Vec<Slot>,
    pub offered_at: DateTime<Utc>,
    /// The outward act that carried the offer, when the consumer recorded one.
    pub offer_act_ref: Option<String>,
    /// Their replies this round, oldest first.
    pub replies: Vec<Reply>,
    pub held: Option<Held>,
    pub closed: Option<Closed>,
    /// Every round that was booked and then moved, oldest first.
    pub reschedules: Vec<Reschedule>,
    /// Every reply source ever absorbed on this ask — across round resets and
    /// reopened generations, not just the current round. A re-offer and a
    /// reschedule clear [`Negotiation::replies`], and reopening after a close
    /// replaces the whole fold entry, but a re-delivered inbox item is still
    /// the same physical message: this set is the dedupe memory that survives
    /// those resets, so absorbing a source stays idempotent for the ask's
    /// whole life. Without it, any round reset amnesied the seen sources and a
    /// re-read inbox double-recorded the same words as a fresh reply.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub absorbed_sources: BTreeSet<String>,
}

impl Negotiation {
    /// Where this stands.
    ///
    /// Takes no clock on purpose: every arm is an asserted fact — a reply, a
    /// hold, a close. The one time-derived observation, silence, is not a
    /// state arm (see the module note) and lives in
    /// [`Negotiation::silent_since`].
    pub fn state(&self) -> NegotiationState {
        if self.closed.is_some() {
            return NegotiationState::Closed;
        }
        if self.held.is_some() {
            return NegotiationState::Held;
        }
        match self.replies.last().map(|reply| &reply.kind) {
            None => NegotiationState::AwaitingReply,
            Some(ReplyKind::Accepted { .. }) => NegotiationState::Accepted,
            Some(ReplyKind::Declined { .. }) => NegotiationState::Declined,
            Some(ReplyKind::Countered { .. }) => NegotiationState::Countered,
        }
    }

    /// The slots currently on the table.
    ///
    /// A counter **replaces** the standing slots, so this is the latest
    /// counter's slots when one exists, and the offer otherwise. Accepting a
    /// slot outside this set is recording an agreement that never happened.
    pub fn standing_slots(&self) -> &[Slot] {
        self.replies
            .iter()
            .rev()
            .find_map(|reply| match &reply.kind {
                ReplyKind::Countered { slots } => Some(slots.as_slice()),
                _ => None,
            })
            .unwrap_or(&self.offered)
    }

    /// The slot they agreed to, when the latest word is an acceptance.
    pub fn accepted_slot(&self) -> Option<Slot> {
        match self.replies.last().map(|reply| &reply.kind) {
            Some(ReplyKind::Accepted { slot }) => Some(*slot),
            _ => None,
        }
    }

    /// When silence over the standing offer ripened, or `None` if it has not.
    ///
    /// The instant is `offered_at + window`, **inclusive** — at the window
    /// boundary the silence is already real, like every other expiry in this
    /// codebase. The instant is derived from the offer, never from `now`, so
    /// every sweep sees the same ripening moment.
    ///
    /// `None` — refusal, not absence of data — when:
    /// - the window is zero or negative: a caller bug, and failing open here
    ///   would declare silence the instant an offer is made and flood the
    ///   chase register;
    /// - no slots are standing: an ask with nothing offered has nothing to be
    ///   silent about, and deriving silence over an empty offer is the
    ///   vacuous-truth bug class;
    /// - any reply exists: no answer is the condition, and one answer of any
    ///   kind ends it;
    /// - the negotiation is held or closed: it is settled.
    pub fn silent_since(&self, window: Duration, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if window <= Duration::zero() {
            return None;
        }
        if self.offered.is_empty() {
            return None;
        }
        if !self.replies.is_empty() || self.held.is_some() || self.closed.is_some() {
            return None;
        }
        let ripened = self.offered_at + window;
        (now >= ripened).then_some(ripened)
    }

    /// The Module D hand-over for ripened silence.
    ///
    /// §3: *"Silence is a state. No answer is neither yes nor no; it is a
    /// thing to chase later, which is Module D."* This module does not chase —
    /// it emits the obligation and Module D owns the follow-up.
    ///
    /// Every field is **sweep-stable** — derived from the negotiation, never
    /// from `now`:
    /// - `what` names the counterparty and the purpose deterministically;
    /// - `due_at` is `offered_at + window`, the stable ripening instant. Never
    ///   `now`: a moving due date would give every sweep a fresh identity
    ///   tuple and flood the register with duplicates of one silence;
    /// - `direction` is `OwedToUs` — they owe us an answer, and a lapse is a
    ///   prompt to follow up, not our failure;
    /// - `source_act_ref` is the offer's act ref, so the chase links back to
    ///   what was actually sent.
    ///
    /// Because the obligation store derives its id from this tuple, repeated
    /// sweeps resume the same obligation instead of duplicating it.
    ///
    /// **Pairs with [`Negotiation::silence_settlement`].** This fn goes to
    /// `None` the instant any reply, hold or close lands — correct for
    /// *recording* (an answered ask is no longer silent), but an obligation a
    /// sweep already recorded while the silence stood still exists in Module
    /// D's register, and its identity tuple must remain derivable so the
    /// answered chase can be settled. `silence_settlement` re-produces the
    /// identical tuple for exactly that case.
    pub fn silence_follow_up(
        &self,
        window: Duration,
        now: DateTime<Utc>,
    ) -> Option<RecordObligation> {
        let ripened = self.silent_since(window, now)?;
        Some(self.silence_obligation(ripened))
    }

    /// The settlement half of the silence pair — the tuple
    /// [`Negotiation::silence_follow_up`] produced, re-derived after they
    /// finally answered.
    ///
    /// `silence_follow_up` returns `None` the moment a reply, hold or close
    /// arrives, so on its own an obligation already recorded for ripened
    /// silence could never be settled: the tuple that identifies it in the
    /// obligation store was unrecoverable. This fn returns the **same** tuple
    /// — same `what`, same `due_at` (`offered_at + window`, the round's
    /// original ripening instant), same direction and source act ref — built
    /// by the same [`Negotiation::silence_obligation`] the follow-up uses, so
    /// the caller can settle the answered chase against the id the store
    /// derived from it.
    ///
    /// `Some` only when the silence **had ripened before the first answer
    /// arrived**: `offered_at + window` is compared against the earliest of
    /// the first reply's `.at`, `held.at` and `closed.at`, inclusive at the
    /// boundary exactly as [`Negotiation::silent_since`] is — at the ripening
    /// instant the silence was already real, so an answer landing at that same
    /// instant still settles the chase a boundary sweep could have recorded.
    ///
    /// `None` — nothing to settle, not missing data — when:
    /// - the window is zero or negative, mirroring the follow-up's guard: no
    ///   chase could have been derived from a window the follow-up refused;
    /// - no slots are standing: an empty offer never had a silence to ripen;
    /// - nothing has answered yet: the ask is still silent, and the
    ///   *follow-up* path owns it — settling an unanswered chase would close
    ///   it while the counterparty still owes the reply;
    /// - the first answer arrived **before** the ripening instant: the
    ///   silence never ripened, no follow-up was ever derivable, and there is
    ///   no chase to settle.
    pub fn silence_settlement(&self, window: Duration) -> Option<RecordObligation> {
        if window <= Duration::zero() {
            return None;
        }
        if self.offered.is_empty() {
            return None;
        }
        let first_answer = [
            self.replies.first().map(|reply| reply.at),
            self.held.as_ref().map(|held| held.at),
            self.closed.as_ref().map(|closed| closed.at),
        ]
        .into_iter()
        .flatten()
        .min()?;
        let ripened = self.offered_at + window;
        (first_answer >= ripened).then(|| self.silence_obligation(ripened))
    }

    /// The one obligation tuple both halves of the silence pair emit.
    ///
    /// Private and shared on purpose: the obligation store derives the
    /// obligation's id from this tuple, so [`Negotiation::silence_follow_up`]
    /// (which records the chase) and [`Negotiation::silence_settlement`]
    /// (which settles it after they answer) must produce byte-identical
    /// fields — two drifting copies would give the settlement a tuple that
    /// resolves to a different id than the chase it is settling.
    fn silence_obligation(&self, ripened: DateTime<Utc>) -> RecordObligation {
        RecordObligation {
            audience: self.audience.clone(),
            program_id: None,
            what: format!(
                "a reply to the scheduling ask to {} about {}",
                self.counterparty, self.purpose
            ),
            due_at: ripened,
            direction: ObligationDirection::OwedToUs,
            created_by: "scheduling".to_string(),
            source_act_ref: self.offer_act_ref.clone(),
        }
    }
}
