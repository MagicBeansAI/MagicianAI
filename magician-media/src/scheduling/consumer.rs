//! The negotiation record's first caller — Module A's adapters.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §3.
//!
//! [`super::store`] is complete and nothing constructs it. That is not a
//! wiring gap, it is the whole failure: a record nobody drives records
//! nothing, and a negotiation log that is never written is indistinguishable
//! from a relationship where nobody ever asked for a meeting. This module is
//! the missing half — the four things a caller has to do around the store,
//! written once so every flow does them the same way:
//!
//! - [`offer_message`] — what to send, as text and slots. **Pure.**
//! - [`absorb_reply`] — what came back, resolved against what is actually
//!   standing, then handed to the store.
//! - [`hold_intent`] / [`record_hold`] — what a calendar needs, and the write
//!   that files the event ref it hands back.
//! - [`silence_obligations`] / [`sweep_silence`] — the chase and its
//!   settlement, each carrying the register id it will live under. **Pure.**
//!
//! # Intent out, never acts
//!
//! Nothing here sends, and nothing here touches a calendar — the same refusal
//! the store makes, for the same reason. [`offer_message`] returns words and
//! times; which channel carries them is the caller's, and no name in this
//! module's public API says "email", "thread" or "invite". [`HoldIntent`] is
//! what a calendar capability needs, not a calendar call. A module that sent
//! would be a module that could only be used by whoever it knew how to send
//! through.
//!
//! # Generic first: a fundraising flow is one consumer, not the subject
//!
//! The primitive is *"we asked somebody for a time, they answered or they did
//! not, and the silence is somebody's to chase"*. That is the same shape for a
//! customer interview, a supplier audit, a candidate loop or an investor
//! meeting. No vocabulary from any one of them appears below.
//!
//! # Fail closed, everywhere
//!
//! Every refusal here exists because its permissive twin was a bug:
//!
//! - An acceptance is resolved against the **standing** slots, and a start
//!   instant that matches none of them — or matches two — is refused rather
//!   than guessed. The store refuses a phantom acceptance at the write; this
//!   refuses it a layer earlier, where the caller can still say *which* times
//!   were on the table.
//! - A non-positive silence window is **refused**, not folded to an empty
//!   sweep. [`Negotiation::silent_since`] returns `None` for one — correct
//!   there, because a derivation has no way to complain — but a sweep that
//!   turned a caller's zero window into "nothing to chase" would report an
//!   empty register as a healthy one, which is the vacuous-truth bug this
//!   codebase refuses everywhere else.
//! - A `current` view that has **dropped** a negotiation the `previous` view
//!   was chasing is refused. See [`sweep_silence`].
//!
//! # Ids, and the separator that keeps them apart
//!
//! [`silence_obligations`] pairs every obligation with
//! [`obligation_id_for`] over the same scope and request, so a caller never
//! derives an id itself and the chase and its settlement cannot drift onto
//! different rows. Every caller string that reaches that derivation — the
//! scope's principal and workspace, the audience id, the counterparty and the
//! purpose, both of which reach the obligation text — is refused if it carries
//! `U+001F`: the separator is what keeps a derived id's components apart, and
//! a component carrying it could fuse two asks into one row, settling one by
//! acting on the other.
//!
//! [`offer_message`] deliberately refuses none of them: no id is derived at
//! that layer, and a refusal placed where it is not load-bearing is a refusal
//! callers route around.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};

use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::store::obligation_id_for;
use magician::magician_v2::obligations::{
    Obligation, ObligationScope, ObligationStore, RecordObligation, Settlement,
};

use super::store::{SchedulingScope, SchedulingStore};
use super::types::{Negotiation, NegotiationState, Reply, ReplyKind, Slot};

/// The separator that keeps a derived id's components apart.
///
/// Restated rather than imported, exactly as `data_room::sweep` restates it:
/// every module that lets caller text reach a derivation has to refuse it
/// itself, and borrowing somebody else's constant would hide this module's
/// refusal behind another module's invariant.
const FIELD_SEP: char = '\u{1f}';

/// The note on a silence chase they answered.
///
/// [`Settlement::Met`], not [`Settlement::Released`]: the obligation was *"a
/// reply to the scheduling ask"*, and a reply is exactly what arrived. Calling
/// it released would report the register's hit rate as lower than it was,
/// which is the same conflation — in the opposite direction — that the two
/// settlement kinds exist to keep apart.
///
/// A constant rather than a generated sentence because it is read by people
/// comparing rows across sweeps, and a text that varied per run would make
/// identical settlements look like different events. It is not part of the
/// register's identity tuple, so it can be read as prose with no risk of
/// moving an id.
pub const SILENCE_ANSWERED_NOTE: &str = "they answered the scheduling ask this chase was raised on";

/// Why a silence chase on an ask that ended unanswered is released.
///
/// [`Settlement::Released`], not [`Settlement::Met`]: nobody replied. The ask
/// was closed — withdrawn, overtaken, no longer wanted — so the chase stopped
/// applying without ever being satisfied. Recording that as met would report a
/// hit rate that was partly wishful.
pub const SILENCE_CLOSED_REASON: &str =
    "the ask was closed before they answered, so the chase no longer applies";

// ── 1. What to send ─────────────────────────────────────────────────────────

/// An offer, ready for any send capability.
///
/// Words **and** times: a caller that only had the prose would have to
/// re-parse its own message to build a structured invite, and a caller that
/// only had the slots would have to invent the sentence. Both travel, and
/// neither names a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferMessage {
    pub negotiation_id: String,
    /// The relationship the ask lives in, so the send can be attributed to it.
    pub audience: AudienceRef,
    /// Whom to address, as the identity the caller already resolved. Not
    /// interpolated into [`Self::body`]: a resolved identity is an address, not
    /// necessarily a name somebody would want read back at them.
    pub counterparty: String,
    pub purpose: String,
    /// The times on offer, in the order the record holds them.
    pub slots: Vec<Slot>,
    /// The outward act the record already links to, when the negotiation was
    /// opened with one — so the send can be tied to the assertion trail the
    /// store points at rather than minting a second ref for the same act.
    pub offer_act_ref: Option<String>,
    pub offered_at: DateTime<Utc>,
    /// The message, composed deterministically from the purpose and the slots.
    pub body: String,
}

/// The offer standing on a negotiation, as something sendable.
///
/// Pure — no store, no clock, no channel.
///
/// `Some` only from [`NegotiationState::AwaitingReply`] **with slots
/// standing**, which is the one state where an offer is genuinely on the
/// table and unanswered. Every other state is refused rather than rendered,
/// because sending the text anyway would say something untrue:
///
/// - `Accepted` — they picked a time; re-asking the question they answered
///   invites a second, contradictory acceptance;
/// - `Declined` / `Countered` — the standing times are dead or theirs.
///   Answering with new times of ours is
///   [`re_offer`](SchedulingStore::re_offer), and composing from here would
///   send their own counter back to them as our offer;
/// - `Held` — the time is booked; offering over it is a reschedule
///   conversation;
/// - `Closed` — the round ended.
/// - the empty round a reschedule leaves — the old times are dead and no
///   fresh offer has been recorded yet, so there is nothing to send. Composing
///   an offer of no times would read as a live ask nobody could answer.
///
/// A slot that arrived through deserialisation rather than [`Slot::new`] is
/// re-checked: parsed data is not trusted to have been constructed correctly,
/// and offering a time nobody can sit in invites an agreement that cannot be
/// kept.
pub fn offer_message(negotiation: &Negotiation) -> Result<OfferMessage> {
    match negotiation.state() {
        NegotiationState::AwaitingReply => {},
        NegotiationState::Accepted => anyhow::bail!(
            "negotiation `{}` already has an acceptance; sending the offer again re-asks a \
             question they answered, and a second acceptance would contradict the first",
            negotiation.negotiation_id
        ),
        NegotiationState::Declined | NegotiationState::Countered => anyhow::bail!(
            "negotiation `{}` stands at `{}`; the standing times are theirs or dead, and \
             answering with times of ours is a `re_offer` — composing from here would send \
             their own counter back to them as our offer",
            negotiation.negotiation_id,
            negotiation.state().as_str()
        ),
        NegotiationState::Held => anyhow::bail!(
            "negotiation `{}` is held on the calendar; offering times over a booked event is a \
             reschedule conversation, not an offer",
            negotiation.negotiation_id
        ),
        NegotiationState::Closed => anyhow::bail!(
            "negotiation `{}` is closed; the round ended, and a new ask on the same purpose \
             opens the next one",
            negotiation.negotiation_id
        ),
    }
    if negotiation.offered.is_empty() {
        anyhow::bail!(
            "negotiation `{}` has no standing offer — a reschedule emptied the round and no \
             fresh offer has been recorded. An offer of no times reads as a live ask nobody \
             can answer",
            negotiation.negotiation_id
        );
    }
    for slot in &negotiation.offered {
        if !slot.is_well_formed() {
            anyhow::bail!(
                "negotiation `{}` holds a slot that does not end after it starts; offering a \
                 time nobody can attend invites an agreement that cannot be kept",
                negotiation.negotiation_id
            );
        }
    }

    Ok(OfferMessage {
        negotiation_id: negotiation.negotiation_id.clone(),
        audience: negotiation.audience.clone(),
        counterparty: negotiation.counterparty.clone(),
        purpose: negotiation.purpose.clone(),
        slots: negotiation.offered.clone(),
        offer_act_ref: negotiation.offer_act_ref.clone(),
        offered_at: negotiation.offered_at,
        body: compose_body(&negotiation.purpose, &negotiation.offered),
    })
}

// ── 2. What came back ───────────────────────────────────────────────────────

/// What the caller read out of an inbound message.
///
/// The reading is **supplied**, never inferred here: turning prose into a
/// verdict is language work that belongs to whoever holds the conversation,
/// and a parser guessing "no problem" for a decline would put words in a
/// counterparty's mouth. What this module owns is the half a parser must not
/// do alone — resolving the time they named against the times that are
/// actually standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundReading {
    /// They took the time **starting** at this instant.
    ///
    /// A start rather than a whole slot on purpose: a reply names a time, not
    /// a range, and the range it belongs to is the one that was offered. The
    /// resolution against the standing slots is
    /// [`read_reply`](self::read_reply)'s job, and it refuses rather than
    /// guesses.
    AcceptedStartingAt(DateTime<Utc>),
    /// They said no.
    Declined { reason: Option<String> },
    /// They proposed different times, which replace the standing ones.
    Countered { slots: Vec<Slot> },
}

/// One inbound message, as the caller read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundReply {
    /// The message or transcript this was read from. Required, and the store's
    /// idempotency key: a re-read inbox re-delivers the same source, and the
    /// second delivery must change nothing.
    pub source_ref: String,
    /// When they replied — the message's own time, never a sweep's clock.
    pub at: DateTime<Utc>,
    pub reading: InboundReading,
}

/// Resolve an inbound reading into the [`Reply`] the store expects.
///
/// Pure. The one judgement it makes is which standing slot an accepted start
/// instant names, and it refuses both ways it could be wrong:
///
/// - **no standing slot starts then** — the acceptance is of a time nobody
///   offered, or of a time a re-offer has since replaced. The store refuses
///   this at the write as *"an agreement that never happened"*; refusing it
///   here as well is what lets the caller see which times were actually on the
///   table;
/// - **two standing slots start then** — a counter can put two ranges at one
///   start instant, and picking either would book a time on a coin flip.
///
/// The standing slots are [`Negotiation::standing_slots`], never
/// [`Negotiation::offered`]: a counter replaces the offer, and resolving
/// against the dead offer would accept a time that is no longer on the table.
pub fn read_reply(negotiation: &Negotiation, inbound: &InboundReply) -> Result<Reply> {
    if inbound.source_ref.trim().is_empty() {
        anyhow::bail!(
            "an inbound reply must carry the ref of the message it was read from; the owner \
             reads the words, an unsourced reply can never be checked against them, and the \
             ref is also what makes a re-delivered message a no-op"
        );
    }
    let kind = match &inbound.reading {
        InboundReading::AcceptedStartingAt(start) => {
            let standing = negotiation.standing_slots();
            let matched: Vec<Slot> = standing
                .iter()
                .filter(|slot| slot.start == *start)
                .copied()
                .collect();
            match matched.len() {
                1 => ReplyKind::Accepted { slot: matched[0] },
                0 => anyhow::bail!(
                    "no standing slot on negotiation `{}` starts at {}; recording it would \
                     record an agreement that never happened — {} time(s) are standing",
                    negotiation.negotiation_id,
                    start.to_rfc3339_opts(SecondsFormat::Secs, true),
                    standing.len()
                ),
                _ => anyhow::bail!(
                    "{} standing slots on negotiation `{}` start at {}; which one they took is \
                     ambiguous, and picking either would book a time on a coin flip",
                    matched.len(),
                    negotiation.negotiation_id,
                    start.to_rfc3339_opts(SecondsFormat::Secs, true)
                ),
            }
        },
        InboundReading::Declined { reason } => {
            // A whitespace-only reason is no reason: storing one would show the
            // owner an empty quotation and read as though they explained.
            let reason = reason
                .as_deref()
                .map(str::trim)
                .filter(|held| !held.is_empty())
                .map(str::to_string);
            ReplyKind::Declined { reason }
        },
        InboundReading::Countered { slots } => {
            if slots.is_empty() {
                anyhow::bail!(
                    "a counter must name at least one time; a counter of nothing would replace \
                     the standing offer with nothing while reading as a live counter, and no \
                     acceptance could ever follow it"
                );
            }
            for slot in slots {
                if !slot.is_well_formed() {
                    anyhow::bail!(
                        "a countered slot must end after it starts; recording a time nobody can \
                         attend invites an agreement that cannot be kept"
                    );
                }
            }
            ReplyKind::Countered {
                slots: slots.clone(),
            }
        },
    };
    Ok(Reply {
        source_ref: inbound.source_ref.trim().to_string(),
        at: inbound.at,
        kind,
    })
}

/// Read an inbound message and absorb it into the negotiation.
///
/// Idempotent on `source_ref`, and the dedupe is **the store's**: a source the
/// negotiation has already absorbed returns the record untouched, because
/// [`Negotiation::absorbed_sources`] is the memory of every source ever
/// absorbed on the ask — across round resets and reopened generations — and
/// this module has no business keeping a second copy of it.
///
/// The already-absorbed check runs **before** the reading is resolved, and
/// that order is load-bearing rather than an optimisation. A re-delivered
/// acceptance is resolved against the slots standing *now*, and a re-offer or
/// a reschedule since the first delivery has moved them: re-resolving would
/// raise *"no standing slot starts then"* for a message the store would
/// correctly treat as already absorbed, turning a harmless replay into an
/// error. Consulting the store's own record is not reimplementing its dedupe —
/// it is declining to ask a question the store has already answered.
///
/// The audience is a parameter because the store keeps one log per
/// relationship and a negotiation id alone cannot say which log to open.
pub fn absorb_reply(
    store: &SchedulingStore,
    scope: &SchedulingScope,
    audience: &AudienceRef,
    negotiation_id: &str,
    inbound: &InboundReply,
) -> Result<Negotiation> {
    let Some(negotiation) = store
        .load(scope, audience, negotiation_id)
        .with_context(|| format!("loading negotiation `{negotiation_id}` to absorb a reply"))?
    else {
        anyhow::bail!(
            "no negotiation `{negotiation_id}` on `{}`; absorbing a reply into an ask that does \
             not exist would file the counterparty's words against nothing",
            audience.as_key()
        );
    };
    if negotiation
        .absorbed_sources
        .contains(inbound.source_ref.trim())
    {
        return Ok(negotiation);
    }
    let reply = read_reply(&negotiation, inbound)?;
    store
        .absorb(scope, audience, negotiation_id, &reply)
        .with_context(|| {
            format!(
                "absorbing `{}` into negotiation `{negotiation_id}`",
                reply.source_ref
            )
        })
}

// ── 3. What the calendar needs ──────────────────────────────────────────────

/// Everything a calendar capability needs to place the agreed time.
///
/// Not a calendar call: this module books nothing, exactly as the store
/// writes nothing outward. The capability inserts the event and hands its ref
/// to [`record_hold`], which files it against the negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldIntent {
    pub negotiation_id: String,
    pub audience: AudienceRef,
    /// Whom to invite, as the identity the caller already resolved.
    pub counterparty: String,
    /// What the meeting is for, in the words the owner would recognise — the
    /// title a calendar entry wants.
    pub purpose: String,
    /// The slot **they accepted**, never a slot the caller chose.
    pub slot: Slot,
}

/// What to book, from a negotiation that stands accepted.
///
/// Pure. `Some` only from [`NegotiationState::Accepted`], because that is the
/// only state in which a time exists that somebody agreed to.
///
/// An **already-held** negotiation is refused rather than handed back, and the
/// refusal names the event that exists. Returning an intent for a booked
/// negotiation would invite a second calendar insert for one agreement, and a
/// duplicate event on somebody else's calendar is not something a later sweep
/// can quietly retire — the store's own hold is idempotent, but the outward
/// act this intent triggers is not.
pub fn hold_intent(negotiation: &Negotiation) -> Result<HoldIntent> {
    if let Some(held) = &negotiation.held {
        anyhow::bail!(
            "negotiation `{}` is already held as `{}`; handing back an intent would invite a \
             second calendar insert for one agreement, and a duplicate event on somebody \
             else's calendar is not something a later sweep can retire",
            negotiation.negotiation_id,
            held.calendar_event_ref
        );
    }
    if negotiation.closed.is_some() {
        anyhow::bail!(
            "negotiation `{}` is closed; a closed ask has no agreement to book",
            negotiation.negotiation_id
        );
    }
    let Some(slot) = negotiation.accepted_slot() else {
        anyhow::bail!(
            "negotiation `{}` stands at `{}`, not `accepted`; booking before they accept puts a \
             time on a calendar that nobody agreed to",
            negotiation.negotiation_id,
            negotiation.state().as_str()
        );
    };
    Ok(HoldIntent {
        negotiation_id: negotiation.negotiation_id.clone(),
        audience: negotiation.audience.clone(),
        counterparty: negotiation.counterparty.clone(),
        purpose: negotiation.purpose.clone(),
        slot,
    })
}

/// File the calendar event the intent produced.
///
/// Deliberately thin: every judgement — which slot, whose agreement, whether
/// one exists at all — was made by [`hold_intent`] and is re-made by the
/// store's own write rules, so the only thing this can get wrong is losing the
/// ref. It delegates, and it adds context naming the negotiation so a failure
/// says which ask lost its booking.
pub fn record_hold(
    store: &SchedulingStore,
    scope: &SchedulingScope,
    intent: &HoldIntent,
    calendar_event_ref: &str,
    now: DateTime<Utc>,
) -> Result<Negotiation> {
    store
        .hold(
            scope,
            &intent.audience,
            &intent.negotiation_id,
            intent.slot,
            calendar_event_ref,
            now,
        )
        .with_context(|| {
            format!(
                "recording calendar event `{calendar_event_ref}` against negotiation `{}`",
                intent.negotiation_id
            )
        })
}

// ── 4. Silence, and the chase it owes ───────────────────────────────────────

/// One obligation the sweep intends, carrying the id the register files it
/// under.
///
/// The id is [`obligation_id_for`] over the same scope and request, so it is
/// the id `ObligationStore::record` derives for this request and the handle
/// `ObligationStore::settle` needs later: both delegate to the same private
/// derivation, so they cannot drift.
///
/// Restated here rather than shared with `data_room::sweep`'s namesake because
/// that type lives inside another consumer. Importing it would make every
/// scheduling caller depend on the data room, which is exactly the coupling
/// both modules exist to avoid.
#[derive(Debug, Clone)]
pub struct IdentifiedObligation {
    pub obligation_id: String,
    pub request: RecordObligation,
}

/// A chase to settle, and how it ended.
///
/// The settlement travels with the row because *"they replied"* and *"the ask
/// was closed unanswered"* are different facts, and a caller left to pick
/// would have to re-derive from the negotiation what this module already knows.
#[derive(Debug, Clone)]
pub struct SettledSilence {
    pub obligation: IdentifiedObligation,
    pub settlement: Settlement,
}

/// What one silence sweep intends — nothing written yet.
///
/// The two lists name **different rows**, never the same one twice: a
/// negotiation with a ripened silence has no answer, and a negotiation with a
/// settlement has one, so the two derivations are mutually exclusive per ask.
///
/// An empty sweep means **there is nothing to do on these facts**. It is not a
/// claim that every ask is answered, that the relationship is healthy, or that
/// the negotiations were read successfully.
#[derive(Debug, Clone, Default)]
pub struct SilenceSweep {
    /// Ripened chases, soonest-due first then by text — the order the register
    /// itself surfaces.
    pub to_record: Vec<IdentifiedObligation>,
    /// Chases whose silence has since been answered or abandoned, same order.
    pub to_settle: Vec<SettledSilence>,
}

impl SilenceSweep {
    /// Whether this sweep would write anything.
    ///
    /// *"Nothing to do"*, never *"nothing is wrong"* — see the type note.
    pub fn is_empty(&self) -> bool {
        self.to_record.is_empty() && self.to_settle.is_empty()
    }
}

/// One ask's silence pair: the chase it owes, or the chase it settles.
///
/// Pure — a negotiation in, intent out, no store. `now` is the sweep's
/// instant; nothing derived from it is stored, because a chase's `due_at` is
/// the instant the silence **ripened** (`offered_at + window`) and an
/// obligation's lapsed state is derived from the clock on every read.
///
/// The two halves come straight from [`Negotiation::silence_follow_up`] and
/// [`Negotiation::silence_settlement`], which build the identical obligation
/// tuple by construction. Pairing each with [`obligation_id_for`] here is what
/// makes the round trip usable: the chase a sweep records under an id is the
/// row a later sweep settles under that same id, and no caller ever rebuilds
/// the tuple by hand.
///
/// Refusals, all fail-closed:
///
/// - a **non-positive window**. [`Negotiation::silent_since`] answers `None`
///   for one, which is right for a derivation and wrong for a sweep: folding a
///   caller's zero window into an empty sweep would report *"nothing to
///   chase"* over an ask nobody has answered;
/// - an **unnamed audience**. The register refuses one at the write, so
///   pairing it here would hand back a settle handle for a row that can never
///   exist;
/// - any string reaching the id derivation that carries `U+001F` — the
///   scope's principal or workspace, the audience id, and the counterparty and
///   purpose, both of which reach the obligation text.
pub fn silence_obligations(
    scope: &ObligationScope,
    negotiation: &Negotiation,
    window: Duration,
    now: DateTime<Utc>,
) -> Result<SilenceSweep> {
    refuse_bad_window(window)?;
    validate_scope(scope)?;
    validate_negotiation(negotiation)?;

    let mut sweep = SilenceSweep::default();
    if let Some(request) = negotiation.silence_follow_up(window, now) {
        sweep.to_record.push(pair(scope, request));
    }
    if let Some(request) = negotiation.silence_settlement(window) {
        let settlement = settlement_for(negotiation).with_context(|| {
            format!(
                "negotiation `{}` derived a silence settlement with no answer to settle it \
                 with; the two derivations must agree on what answered",
                negotiation.negotiation_id
            )
        })?;
        sweep.to_settle.push(SettledSilence {
            obligation: pair(scope, request),
            settlement,
        });
    }
    Ok(sweep)
}

/// A whole relationship's silence sweep, across the view the last sweep saw
/// and the view now.
///
/// Pure, and the multi-ask form of [`silence_obligations`]. `previous` is what
/// the last sweep read from `SchedulingStore::for_audience` and `current` is
/// what this one read. Passing the same slice for both is the honest way to
/// say *"first sweep"* or *"nothing has changed"*: recording is idempotent at
/// the register, and the settling half correctly finds only asks that have
/// since been answered.
///
/// # Unknown is never permission to settle
///
/// A `current` view that has **dropped** a negotiation the `previous` view had
/// a **ripened** chase for is refused. A negotiation missing from this run's
/// read is a negotiation we know nothing about this run — a partial read, an
/// unreadable log, a caller passing the wrong audience — and *"we could not
/// see it"* is not evidence that the counterparty stopped owing an answer. The
/// empty-current case is the same bug at its purest: every previously derived
/// chase is trivially absent, so a vacuous membership test would release the
/// whole register in one call. Retiring an audience's chases is a separate,
/// explicit act with its own reason.
///
/// The check fires only on a negotiation that **contributed a ripened chase**,
/// established by running the real derivation over that negotiation rather
/// than by re-implementing the ripening rule — a second copy of that rule is
/// the thing that would drift. A negotiation that never ripened costs nothing
/// when it disappears, and a rule that fired on it is a rule callers route
/// around.
///
/// Duplicates in either view collapse to their first occurrence, so a caller
/// that concatenated two reads of one log cannot double a chase.
pub fn sweep_silence(
    scope: &ObligationScope,
    previous: &[Negotiation],
    current: &[Negotiation],
    window: Duration,
    now: DateTime<Utc>,
) -> Result<SilenceSweep> {
    refuse_bad_window(window)?;
    validate_scope(scope)?;
    for negotiation in previous.iter().chain(current) {
        validate_negotiation(negotiation)?;
    }
    refuse_dropped_ripened(previous, current, window, now)?;

    let mut sweep = SilenceSweep::default();
    let mut seen: HashSet<&str> = HashSet::new();
    for negotiation in current {
        // First occurrence wins, exactly as the dropped-ripened check dedupes,
        // so the two halves cannot disagree about which fold speaks for an ask.
        if !seen.insert(negotiation.negotiation_id.as_str()) {
            continue;
        }
        let one = silence_obligations(scope, negotiation, window, now)?;
        sweep.to_record.extend(one.to_record);
        sweep.to_settle.extend(one.to_settle);
    }
    sweep
        .to_record
        .sort_by(|left, right| order_key(&left.request).cmp(&order_key(&right.request)));
    sweep.to_settle.sort_by(|left, right| {
        order_key(&left.obligation.request).cmp(&order_key(&right.obligation.request))
    });
    Ok(sweep)
}

/// What a silence sweep actually did to the register.
///
/// Counts of rows, never rates: *"two chases recorded, one settled"* is a fact
/// an owner can act on, while *"67% of asks answered"* hides which rows moved
/// and reads as progress whichever way it goes.
#[derive(Debug, Clone)]
pub struct AppliedSilenceSweep {
    /// The register rows after recording. A row already held is **resumed**,
    /// not duplicated, and a row already settled comes back settled —
    /// recording never resurrects a terminal state.
    pub recorded: Vec<Obligation>,
    /// The register rows after settling, each carrying the settlement the
    /// sweep derived: [`Settlement::Met`] when they answered,
    /// [`Settlement::Released`] when the ask was closed unanswered.
    pub settled: Vec<Obligation>,
    /// Ids the sweep asked to settle that the register does not hold.
    ///
    /// Reported rather than raised. The recording half for that silence never
    /// ran — a first sweep of an ask that ripened and was answered between two
    /// runs, a run that crashed before its records landed — so there is
    /// genuinely nothing to settle. Failing here would wedge every future
    /// sweep of the relationship behind a row that will never exist, and the
    /// records in the same call would already have landed.
    pub absent: Vec<String>,
}

/// Write a silence sweep to the register: **record everything, then settle
/// everything**.
///
/// The thin half, and the twin of `data_room::sweep::apply_follow_up_sweep`.
/// It makes no decisions — which chases, under which ids, with which due dates
/// and with which settlement kind was all settled by [`silence_obligations`]
/// and [`sweep_silence`] — so the only thing this can get wrong is the order,
/// and the order is not a preference: a crash between the halves must leave a
/// live chase, never a settled-but-unrecorded one. A chase that vanished
/// silently is unrecoverable in the way a duplicate is not.
///
/// Both halves are idempotent at the register, so re-applying a sweep after a
/// crash converges rather than doubling: `record` resumes a row it already
/// holds, and `settle` keeps the first settlement because *"they answered on
/// Tuesday"* is a fact a second call must not move.
///
/// The settlement travels on the row rather than being chosen here, which is
/// the whole reason [`SettledSilence`] carries one: *"they replied"* and *"the
/// ask was closed unanswered"* are different facts, and a writer left to pick
/// would have to re-derive from the negotiation what the sweep already knew.
///
/// A recorded row whose id does not match the paired one is fatal. It cannot
/// happen while [`obligation_id_for`] and the store's own derivation agree,
/// and if they ever stop agreeing then every chase recorded here becomes
/// permanently unsettleable — a register that grows forever, which is the
/// failure the pairing exists to prevent. Better to fail loudly on the first
/// row than to leak quietly.
pub fn apply_silence_sweep(
    store: &ObligationStore,
    scope: &ObligationScope,
    sweep: &SilenceSweep,
    now: DateTime<Utc>,
) -> Result<AppliedSilenceSweep> {
    // ── Record first. An interruption here leaves the chase live, which the
    // next sweep settles once the answer is visible.
    let mut recorded = Vec::with_capacity(sweep.to_record.len());
    for item in &sweep.to_record {
        let obligation = store
            .record(scope, &item.request, now)
            .with_context(|| format!("recording silence chase `{}`", item.obligation_id))?;
        if obligation.obligation_id != item.obligation_id {
            anyhow::bail!(
                "the register filed this chase as `{}` but the sweep paired it with `{}`: the \
                 paired id is the only handle a later sweep has to settle this row, so a \
                 pairing that does not round-trip would leave every chase raised here \
                 unsettleable forever",
                obligation.obligation_id,
                item.obligation_id
            );
        }
        recorded.push(obligation);
    }

    // ── Then settle. Never before.
    let mut settled = Vec::with_capacity(sweep.to_settle.len());
    let mut absent = Vec::new();
    for item in &sweep.to_settle {
        // Checked before settling rather than mapping the store's "no such
        // obligation" error, because that error is also what a genuine fault
        // would look like and the two must not be confused.
        let held = store
            .load(
                scope,
                &item.obligation.request.audience,
                &item.obligation.obligation_id,
            )
            .with_context(|| {
                format!(
                    "loading silence chase `{}` to settle",
                    item.obligation.obligation_id
                )
            })?;
        if held.is_none() {
            absent.push(item.obligation.obligation_id.clone());
            continue;
        }
        let obligation = store
            .settle(
                scope,
                &item.obligation.request.audience,
                &item.obligation.obligation_id,
                item.settlement.clone(),
                now,
            )
            .with_context(|| {
                format!("settling silence chase `{}`", item.obligation.obligation_id)
            })?;
        settled.push(obligation);
    }

    Ok(AppliedSilenceSweep {
        recorded,
        settled,
        absent,
    })
}

// ── Internals ───────────────────────────────────────────────────────────────

fn compose_body(purpose: &str, slots: &[Slot]) -> String {
    let mut lines = Vec::with_capacity(slots.len() + 2);
    lines.push(format!("About {purpose} — do any of these times work?"));
    for slot in slots {
        lines.push(format!(
            "- {} to {}",
            slot.start.to_rfc3339_opts(SecondsFormat::Secs, true),
            slot.end.to_rfc3339_opts(SecondsFormat::Secs, true)
        ));
    }
    lines.push("Reply with the one that suits you, or suggest another time.".to_string());
    lines.join("\n")
}

fn pair(scope: &ObligationScope, request: RecordObligation) -> IdentifiedObligation {
    IdentifiedObligation {
        obligation_id: obligation_id_for(scope, &request),
        request,
    }
}

fn order_key(request: &RecordObligation) -> (DateTime<Utc>, String) {
    (request.due_at, request.what.clone())
}

/// How the silence ended: they answered, or the ask was closed unanswered.
///
/// The earliest of the three possible answers speaks, and a **reply or a hold
/// wins a tie with a close**: an answer landing at the same instant as the
/// close is still an answer, and calling that released would under-report a
/// chase the counterparty actually satisfied.
///
/// `None` only when nothing has answered at all, which
/// [`Negotiation::silence_settlement`] has already excluded — kept as an
/// `Option` so the impossible case surfaces as a refusal rather than as a
/// silently wrong settlement kind.
fn settlement_for(negotiation: &Negotiation) -> Option<Settlement> {
    let mut answers: Vec<(DateTime<Utc>, bool)> = Vec::with_capacity(3);
    if let Some(reply) = negotiation.replies.first() {
        answers.push((reply.at, true));
    }
    if let Some(held) = &negotiation.held {
        answers.push((held.at, true));
    }
    if let Some(closed) = &negotiation.closed {
        answers.push((closed.at, false));
    }
    // `min_by_key` keeps the FIRST minimum, and the pushes above are ordered
    // reply, hold, close — which is what makes an answer win a tie with a close.
    let (_, answered) = answers.into_iter().min_by_key(|(at, _)| *at)?;
    Some(if answered {
        Settlement::Met {
            note: Some(SILENCE_ANSWERED_NOTE.to_string()),
        }
    } else {
        Settlement::Released {
            reason: SILENCE_CLOSED_REASON.to_string(),
        }
    })
}

fn refuse_bad_window(window: Duration) -> Result<()> {
    if window <= Duration::zero() {
        anyhow::bail!(
            "a silence window must be positive; at zero or below every offer is silent the \
             instant it is made, and folding the refusal into an empty sweep would report \
             `nothing to chase` over an ask nobody has answered"
        );
    }
    Ok(())
}

fn validate_scope(scope: &ObligationScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator \
             that keeps a derived obligation id's components from bleeding into each other, \
             and a scope carrying it could address one tenant's register row from another's \
             sweep"
        );
    }
    Ok(())
}

fn validate_negotiation(negotiation: &Negotiation) -> Result<()> {
    if !negotiation.audience.is_named() {
        anyhow::bail!(
            "negotiation `{}` names no relationship; the register refuses an unnamed audience \
             at the write, so pairing one here would hand back a settle handle for a row that \
             can never exist",
            negotiation.negotiation_id
        );
    }
    if negotiation.audience.id.contains(FIELD_SEP) {
        anyhow::bail!(
            "an audience id must not contain U+001F: it is the separator that keeps a derived \
             obligation id's components from bleeding into each other, and an audience \
             carrying it could fuse two relationships' registers into one id"
        );
    }
    if negotiation.counterparty.contains(FIELD_SEP) || negotiation.purpose.contains(FIELD_SEP) {
        anyhow::bail!(
            "a counterparty and a purpose must not contain U+001F: both reach the obligation \
             text, which is part of the register's identity tuple, and either carrying it \
             could fuse two asks' chases into one row — settling one by acting on the other"
        );
    }
    Ok(())
}

fn refuse_dropped_ripened(
    previous: &[Negotiation],
    current: &[Negotiation],
    window: Duration,
    now: DateTime<Utc>,
) -> Result<()> {
    let present: HashSet<&str> = current
        .iter()
        .map(|negotiation| negotiation.negotiation_id.as_str())
        .collect();

    let mut seen: HashSet<&str> = HashSet::new();
    for negotiation in previous {
        let id = negotiation.negotiation_id.as_str();
        // First occurrence wins, exactly as the sweep itself dedupes.
        if !seen.insert(id) {
            continue;
        }
        if present.contains(id) {
            continue;
        }
        // The real derivation, never a second copy of the ripening rule.
        if negotiation.silence_follow_up(window, now).is_some() {
            anyhow::bail!(
                "negotiation `{id}` had a ripened silence chase in the previous view of `{}` \
                 and is missing from the current one: settling is a claim that a chase stopped \
                 applying, and an ask we could not see this run is unknown, not answered. \
                 Retiring a relationship's chases is an explicit act with its own reason, \
                 never something inferred from a view that came back short",
                negotiation.audience.as_key()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The adapters, as behaviour: what gets sent, what a reply resolves to,
    //! what a calendar is told, and the chase that round-trips to one row.

    use chrono::{Duration, TimeZone, Utc};

    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::AudienceRef;
    use magician::magician_v2::obligations::{
        ObligationDirection, ObligationScope, ObligationStore, Settlement,
    };

    use super::super::store::{SchedulingScope, SchedulingStore};
    use super::super::types::{Negotiation, Slot};
    use super::{
        absorb_reply, apply_silence_sweep, hold_intent, offer_message, record_hold,
        silence_obligations, sweep_silence, InboundReading, InboundReply, SILENCE_ANSWERED_NOTE,
        SILENCE_CLOSED_REASON,
    };

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, SchedulingStore, SchedulingScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let scheduling = SchedulingStore::new(ArtifactV2Workspace::new(tmp.path()));
        (
            tmp,
            scheduling,
            SchedulingScope::new("anonymous", "default"),
        )
    }

    fn register_scope() -> ObligationScope {
        ObligationScope::new("anonymous", "default")
    }

    fn eng() -> AudienceRef {
        AudienceRef::engagement("eng-1")
    }

    /// A one-hour slot on 2026-08-24 at `hour`, `days` later.
    fn slot(days: i64, hour: u32) -> Slot {
        let start = Utc.with_ymd_and_hms(2026, 8, 24, hour, 0, 0).unwrap() + Duration::days(days);
        Slot::new(start, start + Duration::hours(1)).expect("well-formed")
    }

    fn open_ask(scheduling: &SchedulingStore, scope: &SchedulingScope) -> Negotiation {
        scheduling
            .open(
                scope,
                &eng(),
                "dana",
                "quarterly review",
                &[slot(1, 9), slot(2, 14)],
                Some("act-offer-1".to_string()),
                now(),
            )
            .expect("open")
    }

    fn inbound(source_ref: &str, reading: InboundReading) -> InboundReply {
        InboundReply {
            source_ref: source_ref.to_string(),
            at: now() + Duration::hours(4),
            reading,
        }
    }

    /// The offer a caller sends is composed from the record, not from the
    /// caller's own memory of it: the body names the purpose and every standing
    /// slot in order, and the slots travel structured beside it so a send
    /// capability never has to re-parse the prose it was handed.
    #[test]
    fn an_offer_message_carries_the_exact_words_and_the_exact_slots() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);

        let message = offer_message(&negotiation).expect("sendable offer");

        assert_eq!(
            message.body,
            "About quarterly review — do any of these times work?\n\
             - 2026-08-25T09:00:00Z to 2026-08-25T10:00:00Z\n\
             - 2026-08-26T14:00:00Z to 2026-08-26T15:00:00Z\n\
             Reply with the one that suits you, or suggest another time."
        );
        assert_eq!(message.slots, vec![slot(1, 9), slot(2, 14)]);
        assert_eq!(message.counterparty, "dana");
        assert_eq!(message.purpose, "quarterly review");
        assert_eq!(message.offer_act_ref, Some("act-offer-1".to_string()));
        assert_eq!(message.offered_at, now());
        assert_eq!(message.audience, eng());
    }

    /// A held negotiation has nothing to offer. Composing one anyway would put
    /// candidate times in front of somebody whose meeting is already on a
    /// calendar, and their answer to it would contradict a booked event.
    #[test]
    fn a_held_negotiation_refuses_to_compose_an_offer() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound(
                "msg-1",
                InboundReading::AcceptedStartingAt(slot(1, 9).start),
            ),
        )
        .expect("acceptance");
        let accepted = scheduling
            .load(&scope, &eng(), &negotiation.negotiation_id)
            .expect("load")
            .expect("present");
        let intent = hold_intent(&accepted).expect("intent");
        record_hold(
            &scheduling,
            &scope,
            &intent,
            "cal-evt-9",
            now() + Duration::hours(5),
        )
        .expect("hold");

        let held = scheduling
            .load(&scope, &eng(), &negotiation.negotiation_id)
            .expect("load")
            .expect("present");
        let error = offer_message(&held).expect_err("a booked ask is not an offer");
        assert!(
            error.to_string().contains("held on the calendar"),
            "{error}"
        );
    }

    /// The idempotency the whole inbox loop rests on: a re-read inbox
    /// re-delivers the same message, and the second delivery must record
    /// nothing. Before the source check the same words landed twice as two
    /// separate replies.
    #[test]
    fn a_redelivered_reply_absorbs_exactly_once() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let message = inbound(
            "msg-1",
            InboundReading::Countered {
                slots: vec![slot(3, 11)],
            },
        );

        let first = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &message,
        )
        .expect("first delivery");
        let again = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &message,
        )
        .expect("re-delivery is not an error");

        assert_eq!(first.replies.len(), 1);
        assert_eq!(again.replies.len(), 1);
        assert_eq!(again.replies[0].source_ref, "msg-1");
        assert_eq!(again.standing_slots(), &[slot(3, 11)]);
    }

    /// An acceptance is resolved against the slots that are STANDING, not the
    /// slots that were once offered. After their counter replaced the offer, an
    /// acceptance naming one of the dead times is an agreement that never
    /// happened, and the store would refuse it one layer later with less to say.
    #[test]
    fn an_acceptance_of_a_superseded_time_is_refused_with_the_standing_count() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound(
                "msg-1",
                InboundReading::Countered {
                    slots: vec![slot(3, 11)],
                },
            ),
        )
        .expect("counter");

        let error = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound(
                "msg-2",
                InboundReading::AcceptedStartingAt(slot(1, 9).start),
            ),
        )
        .expect_err("the offered time is no longer standing");

        assert!(
            error
                .to_string()
                .contains("an agreement that never happened"),
            "{error}"
        );
        assert!(
            error.to_string().contains("1 time(s) are standing"),
            "{error}"
        );
        let unchanged = scheduling
            .load(&scope, &eng(), &negotiation.negotiation_id)
            .expect("load")
            .expect("present");
        assert_eq!(unchanged.replies.len(), 1);
    }

    /// Two standing times sharing a start instant make an accepted start
    /// ambiguous, and picking either books a meeting on a coin flip. The
    /// counter is the only way to put two ranges at one start, and it is the
    /// case a naive `find` would silently resolve to whichever came first.
    #[test]
    fn an_ambiguous_accepted_start_is_refused_rather_than_guessed() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let start = slot(3, 11).start;
        absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound(
                "msg-1",
                InboundReading::Countered {
                    slots: vec![
                        Slot::new(start, start + Duration::minutes(30)).expect("half hour"),
                        Slot::new(start, start + Duration::hours(2)).expect("two hours"),
                    ],
                },
            ),
        )
        .expect("counter with two ranges at one start");

        let error = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound("msg-2", InboundReading::AcceptedStartingAt(start)),
        )
        .expect_err("ambiguous");
        assert!(error.to_string().contains("2 standing slots"), "{error}");
        assert!(error.to_string().contains("coin flip"), "{error}");
    }

    /// A calendar is told the slot THEY accepted, and only once they have.
    /// Before acceptance the intent is refused, naming the state, because
    /// booking first puts a time on a calendar nobody agreed to.
    #[test]
    fn a_hold_intent_needs_an_acceptance_and_carries_their_slot() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);

        let too_early = hold_intent(&negotiation).expect_err("nothing accepted yet");
        assert!(
            too_early.to_string().contains("stands at `awaiting_reply`"),
            "{too_early}"
        );

        let accepted = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &inbound(
                "msg-1",
                InboundReading::AcceptedStartingAt(slot(2, 14).start),
            ),
        )
        .expect("acceptance");

        let intent = hold_intent(&accepted).expect("intent");
        assert_eq!(intent.slot, slot(2, 14));
        assert_eq!(intent.counterparty, "dana");
        assert_eq!(intent.purpose, "quarterly review");
        assert_eq!(intent.audience, eng());

        let held = record_hold(
            &scheduling,
            &scope,
            &intent,
            "cal-evt-3",
            now() + Duration::hours(5),
        )
        .expect("hold");
        assert_eq!(
            held.held.as_ref().expect("held").calendar_event_ref,
            "cal-evt-3"
        );
        assert_eq!(held.held.as_ref().expect("held").slot, slot(2, 14));

        let twice = hold_intent(&held).expect_err("already booked");
        assert!(twice.to_string().contains("cal-evt-3"), "{twice}");
    }

    /// The round trip the whole pairing exists for: the id a sweep records a
    /// ripened chase under is the id a later sweep settles it under. A tuple
    /// rebuilt by hand that drifted by one character would settle nothing at
    /// all while reporting success.
    #[test]
    fn the_silence_pair_round_trips_to_the_same_obligation_id() {
        let (_tmp, scheduling, scope) = fixture();
        let register = register_scope();
        let negotiation = open_ask(&scheduling, &scope);
        let window = Duration::days(3);
        let ripe = now() + window;

        let chase = silence_obligations(&register, &negotiation, window, ripe).expect("ripened");
        assert_eq!(chase.to_record.len(), 1);
        assert!(chase.to_settle.is_empty());
        let recorded = &chase.to_record[0];
        assert_eq!(
            recorded.request.what,
            "a reply to the scheduling ask to dana about quarterly review"
        );
        assert_eq!(recorded.request.due_at, ripe);
        assert_eq!(recorded.request.direction, ObligationDirection::OwedToUs);
        assert_eq!(recorded.request.created_by, "scheduling");
        assert_eq!(
            recorded.request.source_act_ref,
            Some("act-offer-1".to_string())
        );

        let answered = absorb_reply(
            &scheduling,
            &scope,
            &eng(),
            &negotiation.negotiation_id,
            &InboundReply {
                source_ref: "msg-1".to_string(),
                at: ripe + Duration::hours(2),
                reading: InboundReading::Declined {
                    reason: Some("  ".to_string()),
                },
            },
        )
        .expect("late answer");

        let settle = silence_obligations(&register, &answered, window, ripe + Duration::days(1))
            .expect("settlement");
        assert!(settle.to_record.is_empty());
        assert_eq!(settle.to_settle.len(), 1);
        assert_eq!(
            settle.to_settle[0].obligation.obligation_id, recorded.obligation_id,
            "the chase and its settlement must address one register row"
        );
        assert_eq!(
            settle.to_settle[0].settlement,
            Settlement::Met {
                note: Some(SILENCE_ANSWERED_NOTE.to_string())
            }
        );
        // A whitespace-only reason is no reason, not an empty quotation.
        assert_eq!(answered.replies[0].kind.as_str(), "declined");
    }

    /// A zero window is refused, never folded into an empty sweep. Silence
    /// would otherwise ripen the instant an offer is made, and the derivation's
    /// own `None` would read to a sweep as "nothing to chase" over an ask
    /// nobody has answered — vacuous truth wearing a healthy register.
    #[test]
    fn a_non_positive_silence_window_is_refused_not_emptied() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let error = silence_obligations(&register_scope(), &negotiation, Duration::zero(), now())
            .expect_err("zero window");
        assert!(error.to_string().contains("must be positive"), "{error}");
    }

    /// Unknown is never permission to settle. A view that came back short — a
    /// partial read, the wrong audience — must not be read as "those chases
    /// stopped applying", and the empty-current case is that bug at its purest:
    /// every previously derived chase is trivially absent.
    #[test]
    fn a_current_view_that_dropped_a_ripened_ask_is_refused() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let window = Duration::days(3);
        let ripe = now() + window;

        let previous = vec![negotiation.clone()];
        let error = sweep_silence(&register_scope(), &previous, &[], window, ripe)
            .expect_err("an empty current view is not permission");
        assert!(
            error
                .to_string()
                .contains("is missing from the current one"),
            "{error}"
        );
        assert!(
            error.to_string().contains(&negotiation.negotiation_id),
            "{error}"
        );

        // The same view, still present, sweeps normally.
        let sweep = sweep_silence(&register_scope(), &previous, &previous, window, ripe)
            .expect("unchanged view");
        assert_eq!(sweep.to_record.len(), 1);
        assert!(sweep.to_settle.is_empty());
    }

    /// An ask closed before anybody answered released its chase, and never
    /// meets it: nobody replied, so recording it as met would report a hit rate
    /// that was partly wishful.
    #[test]
    fn a_chase_on_an_ask_closed_unanswered_is_released_not_met() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let window = Duration::days(3);
        let ripe = now() + window;
        let closed = scheduling
            .close(
                &scope,
                &eng(),
                &negotiation.negotiation_id,
                "the counterparty went quiet and the quarter ended",
                ripe + Duration::hours(1),
            )
            .expect("close");

        let sweep =
            silence_obligations(&register_scope(), &closed, window, ripe + Duration::days(1))
                .expect("settlement");
        assert_eq!(sweep.to_settle.len(), 1);
        assert_eq!(
            sweep.to_settle[0].settlement,
            Settlement::Released {
                reason: SILENCE_CLOSED_REASON.to_string()
            }
        );
    }

    /// A ripened chase round-trips: recorded under the paired id, then settled
    /// under that same id when they answer.
    ///
    /// Pins the leak the pairing exists to prevent — a chase recorded under an
    /// id no later sweep can rebuild is a register row that grows forever, and
    /// before `apply_silence_sweep` existed nothing wrote either half at all.
    #[test]
    fn a_ripened_chase_records_and_then_settles_under_one_id() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let window = Duration::days(3);
        let ripe = now() + window;
        let register = ObligationStore::new(ArtifactV2Workspace::new(_tmp.path()));

        let sweep =
            silence_obligations(&register_scope(), &negotiation, window, ripe).expect("derive");
        assert_eq!(sweep.to_record.len(), 1, "the silence must have ripened");
        let paired_id = sweep.to_record[0].obligation_id.clone();

        let applied =
            apply_silence_sweep(&register, &register_scope(), &sweep, ripe).expect("record");
        assert_eq!(applied.recorded.len(), 1);
        assert!(applied.settled.is_empty());
        assert_eq!(applied.recorded[0].obligation_id, paired_id);
        assert_eq!(
            register
                .for_audience(&register_scope(), &eng())
                .expect("read")
                .len(),
            1
        );

        let answered = scheduling
            .absorb(
                &scope,
                &eng(),
                &negotiation.negotiation_id,
                &crate::scheduling::types::Reply {
                    source_ref: "msg-1".to_string(),
                    at: ripe + Duration::hours(1),
                    kind: crate::scheduling::types::ReplyKind::Declined { reason: None },
                },
            )
            .expect("absorb");
        let settle_sweep = silence_obligations(
            &register_scope(),
            &answered,
            window,
            ripe + Duration::days(1),
        )
        .expect("derive the settlement");
        assert_eq!(settle_sweep.to_settle.len(), 1);
        assert_eq!(
            settle_sweep.to_settle[0].obligation.obligation_id, paired_id,
            "the settle handle must be the id the chase was recorded under"
        );

        let applied = apply_silence_sweep(
            &register,
            &register_scope(),
            &settle_sweep,
            ripe + Duration::days(1),
        )
        .expect("settle");
        assert_eq!(applied.settled.len(), 1);
        assert!(applied.absent.is_empty());
        assert_eq!(
            applied.settled[0].settlement,
            Some(Settlement::Met {
                note: Some(SILENCE_ANSWERED_NOTE.to_string())
            })
        );
        assert!(
            !applied.settled[0].is_outstanding(ripe + Duration::days(30)),
            "a settled chase never resurrects"
        );
    }

    /// A settle for a row the register does not hold is reported, never raised.
    ///
    /// Pins the wedge: raising here would stop every future sweep of the
    /// relationship behind a row that will never exist, while the records in
    /// the same call would already have landed.
    #[test]
    fn a_settle_for_a_row_the_register_never_held_is_reported_not_raised() {
        let (_tmp, scheduling, scope) = fixture();
        let negotiation = open_ask(&scheduling, &scope);
        let window = Duration::days(3);
        let ripe = now() + window;
        let register = ObligationStore::new(ArtifactV2Workspace::new(_tmp.path()));

        let answered = scheduling
            .absorb(
                &scope,
                &eng(),
                &negotiation.negotiation_id,
                &crate::scheduling::types::Reply {
                    source_ref: "msg-1".to_string(),
                    at: ripe + Duration::hours(1),
                    kind: crate::scheduling::types::ReplyKind::Declined { reason: None },
                },
            )
            .expect("absorb");
        // The recording half never ran for this basis.
        let sweep = silence_obligations(
            &register_scope(),
            &answered,
            window,
            ripe + Duration::days(1),
        )
        .expect("derive");
        assert_eq!(sweep.to_settle.len(), 1, "there is a settlement to attempt");

        let applied = apply_silence_sweep(
            &register,
            &register_scope(),
            &sweep,
            ripe + Duration::days(1),
        )
        .expect("a missing row must not fail the call");
        assert!(applied.settled.is_empty());
        assert_eq!(applied.absent.len(), 1);
        assert_eq!(
            applied.absent[0],
            sweep.to_settle[0].obligation.obligation_id
        );
    }
}
