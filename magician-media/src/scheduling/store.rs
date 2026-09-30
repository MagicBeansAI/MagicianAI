//! The negotiation log — Module A.
//!
//! Append-only per audience: every negotiation in a relationship is one log,
//! so *"what are we scheduling with these people"* is one read rather than a
//! scan. Current state is the fold of the log; nothing is ever rewritten.
//!
//! # Concurrency
//!
//! There is no lock. Every mutation is read-validate-append: it folds the log
//! once, validates against that fold, appends one record, and applies the
//! record to the in-memory fold through the same [`fold_record`] the read
//! path uses — so what a mutation returns is what the next read folds. Two
//! concurrent mutations can still each validate against the same pre-state
//! and both land; the fold's defensive first-wins arms keep the surviving
//! state deterministic and make replays converge, but the loser's record can
//! be folded away after its caller already saw `Ok` — an absorb whose reply a
//! racing hold settled out. Closing that window needs a lock or a
//! single-writer path, the same debt the approval-envelope store records;
//! until then the guarantee is exactly this narrowed one: the log never folds
//! to a state the write rules refuse, and a re-read tells the truth.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;

use super::types::{
    Closed, Held, Negotiation, NegotiationState, Reply, ReplyKind, Reschedule, Slot,
};

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingScope {
    pub principal: String,
    pub workspace: String,
}

impl SchedulingScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One line in an audience's negotiation log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum NegotiationRecord {
    /// A new ask, with its opening offer — or, when the round under this id
    /// has **closed**, the opening of the next round: the fold lets a later
    /// `Opened` supersede a closed one, so a recurring ask (same audience,
    /// counterparty, purpose) can be negotiated again while every earlier
    /// round survives in the log.
    Opened(Negotiation),
    /// A fresh offer on an existing ask whose previous round was rescheduled
    /// away. Never the first offer — that is inside `Opened`.
    Offered {
        negotiation_id: String,
        slots: Vec<Slot>,
        offer_act_ref: Option<String>,
        at: DateTime<Utc>,
    },
    /// A replacement offer answering their decline or counter: the moment we
    /// send different times, the old ones are off the table, so the round
    /// resets around the new offer. Distinct from `Offered`, which only fills
    /// the empty round a reschedule leaves.
    ReOffered {
        negotiation_id: String,
        slots: Vec<Slot>,
        offer_act_ref: Option<String>,
        at: DateTime<Utc>,
    },
    /// A reply absorbed off the conversation's channel.
    Replied {
        negotiation_id: String,
        reply: Reply,
    },
    /// The accepted slot went on the calendar (the consumer's act; we hold
    /// its ref).
    Held { negotiation_id: String, held: Held },
    /// A booked time died; the round resets and a fresh offer is owed.
    Rescheduled {
        negotiation_id: String,
        why: String,
        at: DateTime<Utc>,
    },
    /// The negotiation ended.
    Closed {
        negotiation_id: String,
        closed: Closed,
    },
}

/// Time negotiations, per audience.
#[derive(Debug, Clone)]
pub struct SchedulingStore {
    workspace_layout: ArtifactV2Workspace,
}

impl SchedulingStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &SchedulingScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("scheduling")
    }

    fn log_path(&self, scope: &SchedulingScope, audience: &AudienceRef) -> PathBuf {
        self.root(scope)
            .join(format!("{}.jsonl", stable_id(&audience.as_key())))
    }

    /// Open a negotiation: record the offer the consumer is sending.
    ///
    /// Idempotent on `(audience, counterparty, purpose)` — the derived id —
    /// so a retried open **resumes** the existing negotiation rather than
    /// forking a duplicate ask. Only an **identical** replay resumes: the
    /// same slots and the same `offer_act_ref` the round already holds. An
    /// open carrying anything different is refused, naming
    /// [`SchedulingStore::re_offer`] — the first cut returned `Ok` while
    /// recording nothing, so a genuinely new offer after their decline or
    /// counter silently vanished, and their acceptance of it was then refused
    /// as an agreement that never happened.
    ///
    /// Two states write instead of resuming or refusing:
    /// - after a **reschedule** the round has no standing offer (the old
    ///   times are dead), so the supplied slots become the fresh offer §3's
    ///   reschedule requires, on the same negotiation id with its history
    ///   intact — but only while the emptied round is also **unanswered**:
    ///   once the counterparty has countered into it, their times are
    ///   standing, and new times from us are an answer to that counter, which
    ///   is `re_offer`'s transition (letting `open` fill here overwrote their
    ///   standing counter with an offer that never actually stood);
    /// - after a **close** the identity is free again — for a **genuinely
    ///   new** open only: a fresh `Opened` record starts the next round in
    ///   the same identity slot, and the closed round stays in the log. An
    ///   **identical** replay (same slots, same `offer_act_ref` as the closed
    ///   round) is a retry of the open the owner already closed, and it
    ///   resumes the closed record unchanged — before this guard a retried
    ///   open landing after the close silently reopened a round the owner had
    ///   ended. See [`derive_negotiation_id`] for why the id deliberately
    ///   stays stable across rounds.
    ///
    /// The offer itself travels on whatever channel the conversation is on —
    /// that send is the consumer's outward act, and `offer_act_ref` is its
    /// ref, so this record links to the assertion trail without owning it.
    pub fn open(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        counterparty: &str,
        purpose: &str,
        offered: &[Slot],
        offer_act_ref: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Negotiation> {
        if !audience.is_named() {
            anyhow::bail!(
                "a negotiation must name the relationship it lives in; without one the record \
                 belongs to nobody and can never be read back"
            );
        }
        if counterparty.trim().is_empty() {
            anyhow::bail!(
                "a negotiation must name its counterparty; their acceptance must resolve to the \
                 same counterparty, and a blank identity can never be resolved to"
            );
        }
        if purpose.trim().is_empty() {
            anyhow::bail!(
                "a negotiation must say what the meeting is for; a purpose the owner cannot \
                 recognise is an ask they cannot act on"
            );
        }
        if offered.is_empty() {
            anyhow::bail!(
                "an offer must name at least one slot; an offer of nothing gives the \
                 counterparty nothing to accept, and silence over it would mean nothing"
            );
        }
        for slot in offered {
            if !slot.is_well_formed() {
                anyhow::bail!(
                    "an offered slot must end after it starts; offering a time nobody can \
                     attend invites an agreement that cannot be kept"
                );
            }
        }

        let negotiation_id = derive_negotiation_id(scope, audience, counterparty, purpose);
        let path = self.log_path(scope, audience);
        // The next round inherits the dedupe memory of every earlier one: a
        // re-delivered inbox item from a past round is still the same
        // physical message, and must stay a no-op in the round that follows.
        let mut absorbed_sources = BTreeSet::new();
        match self.load(scope, audience, &negotiation_id)? {
            Some(existing) if existing.closed.is_some() => {
                if existing.offered.as_slice() == offered && existing.offer_act_ref == offer_act_ref
                {
                    // The retried open of the round the owner CLOSED, landing
                    // late: a replay resumes, it never reopens. Before this
                    // arm ran ahead of the identical-replay check, the retry
                    // appended a fresh `Opened` and silently reopened a round
                    // the owner had already ended.
                    return Ok(existing);
                }
                // A genuinely new open: the identity is free again — fall
                // through and append a fresh `Opened`, which the fold treats
                // as the next round. The closed round stays in the log.
                absorbed_sources = existing.absorbed_sources;
            },
            Some(existing) => {
                if existing.offered.is_empty()
                    && existing.held.is_none()
                    && existing.replies.is_empty()
                {
                    // Post-reschedule, still unanswered: the round owes a
                    // fresh offer, and the supplied slots are it. The
                    // `replies` guard is load-bearing — once a counter has
                    // landed in the emptied round, their times are standing
                    // and new times from us answer that counter, so the call
                    // falls through to the refusal naming `re_offer` instead
                    // of overwriting the counterparty's standing counter.
                    let record = NegotiationRecord::Offered {
                        negotiation_id: negotiation_id.clone(),
                        slots: offered.to_vec(),
                        offer_act_ref,
                        at: now,
                    };
                    self.append(&path, &record)?;
                    let mut updated = existing;
                    fold_record(&mut updated, record);
                    return Ok(updated);
                }
                if existing.offered.as_slice() == offered && existing.offer_act_ref == offer_act_ref
                {
                    // An identical replay is a resume, whatever the round's
                    // state — a retried call must not fork or reset the ask.
                    return Ok(existing);
                }
                anyhow::bail!(
                    "the ask already has a round in flight and this open does not match it; a \
                     retried open resumes only when it is identical, and a genuinely new offer \
                     must go through `re_offer` so the replacement is recorded — silently \
                     resuming here would drop the new times while reporting success"
                );
            },
            None => {},
        }

        let negotiation = Negotiation {
            negotiation_id,
            audience: audience.clone(),
            counterparty: counterparty.trim().to_string(),
            purpose: purpose.trim().to_string(),
            offered: offered.to_vec(),
            offered_at: now,
            offer_act_ref,
            replies: Vec::new(),
            held: None,
            closed: None,
            reschedules: Vec::new(),
            absorbed_sources,
        };
        self.append(&path, &NegotiationRecord::Opened(negotiation.clone()))?;
        Ok(negotiation)
    }

    /// Replace the standing slots with a new offer — our answer to their
    /// decline or counter.
    ///
    /// The round resets around the new offer: the dead slots and the replies
    /// they provoked clear from the round (the log keeps the words),
    /// `offered_at` restarts the silence clock, and `offer_act_ref` links the
    /// record to the outward act that carried the replacement. Without this
    /// transition the negotiation wedged permanently: `open` could not record
    /// a changed offer, so the new times landed nowhere and their acceptance
    /// of one was refused as an agreement that never happened.
    ///
    /// Allowed from `Declined`, `Countered`, and the empty round a reschedule
    /// leaves. Refused:
    /// - from `AwaitingReply` with an offer standing — there is no reply to
    ///   answer, and replacing an unanswered offer would reset the silence
    ///   clock a chase may already be measuring;
    /// - from `Accepted` — they took a slot, and replacing the offer now
    ///   would discard an agreement that did happen: hold the accepted slot,
    ///   or absorb the reply that changed it;
    /// - from `Held` — the time is booked, so a new time is a `reschedule`;
    /// - from `Closed` — the round ended, and `open` starts the next one.
    pub fn re_offer(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
        slots: &[Slot],
        offer_act_ref: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Negotiation> {
        if slots.is_empty() {
            anyhow::bail!(
                "a re-offer must name at least one slot; replacing the standing offer with \
                 nothing gives the counterparty nothing to accept while reading as a live offer"
            );
        }
        for slot in slots {
            if !slot.is_well_formed() {
                anyhow::bail!(
                    "a re-offered slot must end after it starts; offering a time nobody can \
                     attend invites an agreement that cannot be kept"
                );
            }
        }
        let Some(negotiation) = self.load(scope, audience, negotiation_id)? else {
            anyhow::bail!(
                "no negotiation `{negotiation_id}` on `{}`",
                audience.as_key()
            );
        };
        match negotiation.state() {
            NegotiationState::Declined | NegotiationState::Countered => {},
            NegotiationState::AwaitingReply if negotiation.offered.is_empty() => {
                // The empty round a reschedule leaves: the fresh offer §3's
                // reschedule requires may arrive through here as well as
                // through `open`.
            },
            NegotiationState::AwaitingReply => anyhow::bail!(
                "the standing offer has no reply yet; a re-offer answers a decline or a \
                 counter, and replacing an unanswered offer would reset the silence clock a \
                 chase may already be measuring — absorb their reply first"
            ),
            NegotiationState::Accepted => anyhow::bail!(
                "they accepted a standing slot; replacing the offer now would discard an \
                 agreement that did happen — hold the accepted slot, or absorb the reply \
                 that changed it"
            ),
            NegotiationState::Held => anyhow::bail!(
                "the negotiation is held on the calendar; offering new times over a booked \
                 event is a reschedule conversation — reschedule instead"
            ),
            NegotiationState::Closed => anyhow::bail!(
                "the negotiation is closed; the round ended, and a new ask on the same \
                 purpose is an `open`, which starts the next round"
            ),
        }
        let record = NegotiationRecord::ReOffered {
            negotiation_id: negotiation_id.to_string(),
            slots: slots.to_vec(),
            offer_act_ref,
            at: now,
        };
        self.append(&self.log_path(scope, audience), &record)?;
        let mut updated = negotiation;
        fold_record(&mut updated, record);
        Ok(updated)
    }

    /// Absorb a reply off the conversation's channel.
    ///
    /// Idempotent on `reply.source_ref`: a re-read inbox re-delivers the same
    /// message, and must not double-record — the first absorption of a source
    /// is the absorption, replays return the current state untouched. The
    /// dedupe consults [`Negotiation::absorbed_sources`], the memory of every
    /// source ever absorbed on the ask, **across round resets and reopened
    /// generations** — not the current round's `replies`, which a re-offer, a
    /// reschedule or a close-and-reopen clears: reading only the round
    /// amnesied the seen sources at every reset, so a re-delivered item
    /// absorbed before the reset double-recorded after it.
    ///
    /// Refused after `Held` or `Closed`: the negotiation is settled. A reply
    /// that arrives after the slot is on the calendar is the start of a
    /// reschedule conversation, and silently absorbing it here would let the
    /// record contradict a booked event; after a close it would reopen a
    /// record the owner has already acted on.
    ///
    /// An acceptance must name one of the **currently-standing** slots — the
    /// opening offer, or the latest counter or re-offer, which replaced it.
    /// Accepting a slot nobody offered is recording an agreement that never
    /// happened.
    pub fn absorb(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
        reply: &Reply,
    ) -> Result<Negotiation> {
        if reply.source_ref.trim().is_empty() {
            anyhow::bail!(
                "a reply must carry the ref of the message or transcript it was read from; the \
                 owner reads the words, and an unsourced reply can never be checked against them"
            );
        }
        let Some(negotiation) = self.load(scope, audience, negotiation_id)? else {
            anyhow::bail!(
                "no negotiation `{negotiation_id}` on `{}`",
                audience.as_key()
            );
        };
        if negotiation.absorbed_sources.contains(&reply.source_ref) {
            // A replayed source is not a new reply, whatever state the
            // negotiation has since reached — including a round reset or a
            // reopened generation that cleared `replies` since the source was
            // first absorbed.
            return Ok(negotiation);
        }
        if negotiation.closed.is_some() {
            anyhow::bail!(
                "the negotiation is closed; absorbing a reply now would reopen a record the \
                 owner has already acted on"
            );
        }
        if negotiation.held.is_some() {
            anyhow::bail!(
                "the negotiation is held on the calendar; a further reply is a reschedule \
                 conversation, and absorbing it here would let the record silently contradict a \
                 booked event — reschedule instead"
            );
        }
        match &reply.kind {
            ReplyKind::Accepted { slot } => {
                if !negotiation.standing_slots().contains(slot) {
                    anyhow::bail!(
                        "the accepted slot is not one of the standing slots; recording it would \
                         record an agreement that never happened"
                    );
                }
            },
            ReplyKind::Countered { slots } => {
                if slots.is_empty() {
                    anyhow::bail!(
                        "a counter must name at least one slot; a counter of nothing would \
                         replace the standing offer with nothing while reading as a live \
                         counter, and no acceptance could ever follow it"
                    );
                }
                for slot in slots {
                    if !slot.is_well_formed() {
                        anyhow::bail!(
                            "a countered slot must end after it starts; recording a time nobody \
                             can attend invites an agreement that cannot be kept"
                        );
                    }
                }
            },
            ReplyKind::Declined { .. } => {},
        }
        let record = NegotiationRecord::Replied {
            negotiation_id: negotiation_id.to_string(),
            reply: reply.clone(),
        };
        self.append(&self.log_path(scope, audience), &record)?;
        let mut updated = negotiation;
        fold_record(&mut updated, record);
        Ok(updated)
    }

    /// Record that the accepted slot is on the calendar.
    ///
    /// The calendar write is the consumer's outward act; `calendar_event_ref`
    /// is required so this record points at the event it claims exists.
    ///
    /// Requires the negotiation to stand **accepted**, and the held slot to be
    /// the accepted slot: holding before they accept books a time nobody
    /// agreed to, and holding a different time than the one they accepted
    /// records an agreement that never happened. A retry of the identical
    /// hold resumes; a conflicting hold is refused — the first hold is the
    /// hold.
    pub fn hold(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
        slot: Slot,
        calendar_event_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<Negotiation> {
        if calendar_event_ref.trim().is_empty() {
            anyhow::bail!(
                "a hold must carry its calendar event ref; without it the record cannot point \
                 at the outward assertion it claims exists"
            );
        }
        let Some(negotiation) = self.load(scope, audience, negotiation_id)? else {
            anyhow::bail!(
                "no negotiation `{negotiation_id}` on `{}`",
                audience.as_key()
            );
        };
        if let Some(held) = &negotiation.held {
            if held.slot == slot && held.calendar_event_ref == calendar_event_ref {
                return Ok(negotiation);
            }
            anyhow::bail!(
                "the negotiation is already held at a different slot or event; the first hold \
                 is the hold, and overwriting it would orphan a booked calendar event"
            );
        }
        if negotiation.closed.is_some() {
            anyhow::bail!("the negotiation is closed; a closed ask cannot be booked");
        }
        let Some(accepted) = negotiation.accepted_slot() else {
            anyhow::bail!(
                "only an accepted negotiation can be held (state is `{}`); holding before they \
                 accept books a time nobody agreed to",
                negotiation.state().as_str()
            );
        };
        if accepted != slot {
            anyhow::bail!(
                "the held slot must be the slot they accepted; booking a different time records \
                 an agreement that never happened"
            );
        }
        let record = NegotiationRecord::Held {
            negotiation_id: negotiation_id.to_string(),
            held: Held {
                slot,
                calendar_event_ref: calendar_event_ref.to_string(),
                at: now,
            },
        };
        self.append(&self.log_path(scope, audience), &record)?;
        let mut updated = negotiation;
        fold_record(&mut updated, record);
        Ok(updated)
    }

    /// A booked time died — §3's `reschedule(event, why)`.
    ///
    /// Only from `Held`: before a hold there is no booked event to move —
    /// absorb their reply, or answer it with `re_offer`. The hold is released
    /// into the reschedule history, and the round resets: offer and replies
    /// clear, because the old times are dead and leaving them standing would
    /// let a stale acceptance be re-held. The negotiation then needs a fresh
    /// offer, which `open` on the same ask (or `re_offer`) records.
    pub fn reschedule(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
        why: &str,
        now: DateTime<Utc>,
    ) -> Result<Negotiation> {
        if why.trim().is_empty() {
            anyhow::bail!(
                "a reschedule must say why; the history of moved times is part of the record \
                 the relationship is read from, and a wordless move erases its own reason"
            );
        }
        let Some(negotiation) = self.load(scope, audience, negotiation_id)? else {
            anyhow::bail!(
                "no negotiation `{negotiation_id}` on `{}`",
                audience.as_key()
            );
        };
        if negotiation.closed.is_some() {
            anyhow::bail!("the negotiation is closed; a closed ask has nothing left to move");
        }
        if negotiation.held.is_none() {
            anyhow::bail!(
                "only a held negotiation can be rescheduled; before a hold there is no booked \
                 event to move — absorb their reply, or answer it with `re_offer`, instead"
            );
        }
        let record = NegotiationRecord::Rescheduled {
            negotiation_id: negotiation_id.to_string(),
            why: why.trim().to_string(),
            at: now,
        };
        self.append(&self.log_path(scope, audience), &record)?;
        let mut updated = negotiation;
        fold_record(&mut updated, record);
        Ok(updated)
    }

    /// End a negotiation — the current **round**, not the identity.
    ///
    /// Idempotent: the first close is the close, and a later call returns the
    /// standing record untouched — letting a second close move the date or
    /// reword the reason would make the log unusable as evidence of what
    /// happened when. A recurring ask is not tombstoned by its close: a fresh
    /// `open` afterwards starts the next round under the same id.
    pub fn close(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Negotiation> {
        if reason.trim().is_empty() {
            anyhow::bail!(
                "a close must say why; an unexplained end leaves the owner unable to tell a \
                 dead ask from a booked one that moved on"
            );
        }
        let Some(negotiation) = self.load(scope, audience, negotiation_id)? else {
            anyhow::bail!(
                "no negotiation `{negotiation_id}` on `{}`",
                audience.as_key()
            );
        };
        if negotiation.closed.is_some() {
            return Ok(negotiation);
        }
        let record = NegotiationRecord::Closed {
            negotiation_id: negotiation_id.to_string(),
            closed: Closed {
                at: now,
                reason: reason.trim().to_string(),
            },
        };
        self.append(&self.log_path(scope, audience), &record)?;
        let mut updated = negotiation;
        fold_record(&mut updated, record);
        Ok(updated)
    }

    /// Every negotiation in one relationship, oldest first.
    ///
    /// Order is the log's own append order — deterministic, because the log is
    /// append-only. Closed negotiations are included: what was asked and how
    /// it ended is part of the record a relationship is read from. A
    /// **reopened** ask shows its current round; the earlier closed rounds of
    /// the same ask live in the log, not in this fold.
    pub fn for_audience(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
    ) -> Result<Vec<Negotiation>> {
        self.fold_log(&self.log_path(scope, audience))
    }

    /// Every negotiation this scope holds, across every relationship.
    ///
    /// The store keeps one log per audience, named for a hash of the audience
    /// key, so [`Self::for_audience`] can only answer for a relationship the
    /// caller can already name. A sweep looking for ripened silence cannot: it
    /// is asking *"which asks has nobody answered"*, and the asks it must not
    /// miss are exactly the ones nobody remembered to name. The `Opened`
    /// record carries the whole [`Negotiation`], audience included, so folding
    /// the directory recovers what the hashed file name cannot be asked
    /// backwards for.
    ///
    /// # Absent is empty; unreadable is not
    ///
    /// A scope that has never opened a negotiation has no directory, and that
    /// is the only condition that reads as *"nothing to sweep"*. Every other
    /// listing failure propagates, and so does a log that will not fold — a
    /// listing that dropped the relationship it could not read would tell a
    /// sweep the counterparty owes nothing, which is the fail-open reading of
    /// silence.
    ///
    /// A log that vanished between the listing and the read contributes
    /// nothing rather than failing the listing: it holds no negotiation now.
    ///
    /// Ordered by audience key then negotiation id, so a sweep sees one fixed
    /// order across calls whatever order the filesystem hands the logs back in.
    pub fn all_negotiations(&self, scope: &SchedulingScope) -> Result<Vec<Negotiation>> {
        let root = self.root(scope);
        let mut out = Vec::new();
        for path in magician::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &root)? {
            out.extend(self.fold_log(&path)?);
        }
        out.sort_by(|left, right| {
            left.audience
                .as_key()
                .cmp(&right.audience.as_key())
                .then_with(|| left.negotiation_id.cmp(&right.negotiation_id))
        });
        Ok(out)
    }

    /// The fold of one relationship's log, addressed by path.
    ///
    /// Shared by [`Self::for_audience`] and [`Self::all_negotiations`] rather
    /// than copied: the supersede rule for a reopened ask and the dedupe
    /// memory it carries forward are subtle enough that a second copy would be
    /// a second copy that drifts, and the two reads would then disagree about
    /// which round speaks for an ask.
    fn fold_log(&self, path: &PathBuf) -> Result<Vec<Negotiation>> {
        let Some(raw) = self.read_if_present(path)? else {
            return Ok(Vec::new());
        };

        // Position map keeps the fold linear: every record finds its
        // negotiation in O(1) instead of scanning the vec.
        let mut position: HashMap<String, usize> = HashMap::new();
        let mut out: Vec<Negotiation> = Vec::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`. Before
        // that helper, one crash mid-append poisoned the audience's whole
        // log: every negotiation in the relationship became unreadable and
        // unwritable, forever.
        for record in
            magician::magician_v2::jsonl::parse_log_lines::<NegotiationRecord>(&raw, path)?
        {
            match record {
                NegotiationRecord::Opened(negotiation) => {
                    match position.get(&negotiation.negotiation_id) {
                        None => {
                            position.insert(negotiation.negotiation_id.clone(), out.len());
                            out.push(negotiation);
                        },
                        Some(&index) => {
                            // A replayed open line must not reset a LIVE
                            // negotiation — but once the round has closed, a
                            // later open is the next round of a recurring
                            // ask, and it supersedes the closed fold entry.
                            // The closed round stays in the log — and its
                            // dedupe memory carries forward: the sources the
                            // earlier rounds absorbed must survive the
                            // supersede, or a re-delivered item from a past
                            // generation double-records into the new one.
                            if out[index].closed.is_some() {
                                let carried = std::mem::take(&mut out[index].absorbed_sources);
                                out[index] = negotiation;
                                out[index].absorbed_sources.extend(carried);
                            }
                        },
                    }
                },
                other => {
                    if let Some(&index) = record_negotiation_id(&other)
                        .and_then(|negotiation_id| position.get(negotiation_id))
                    {
                        fold_record(&mut out[index], other);
                    }
                },
            }
        }
        Ok(out)
    }

    /// One negotiation.
    pub fn load(
        &self,
        scope: &SchedulingScope,
        audience: &AudienceRef,
        negotiation_id: &str,
    ) -> Result<Option<Negotiation>> {
        Ok(self
            .for_audience(scope, audience)?
            .into_iter()
            .find(|held| held.negotiation_id == negotiation_id))
    }

    fn append(&self, path: &PathBuf, record: &NegotiationRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" fails open —
        // `open` forks a duplicate ask over a negotiation that is already
        // booked, and `for_audience` confidently answers "nothing" where the
        // truth was a disk fault. Shared semantics live in `magician_v2::jsonl`.
        magician::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// The negotiation a non-`Opened` record belongs to. `Opened` carries its
/// negotiation inline and is the fold's own job, so it has no answer here.
fn record_negotiation_id(record: &NegotiationRecord) -> Option<&str> {
    match record {
        NegotiationRecord::Opened(_) => None,
        NegotiationRecord::Offered { negotiation_id, .. }
        | NegotiationRecord::ReOffered { negotiation_id, .. }
        | NegotiationRecord::Replied { negotiation_id, .. }
        | NegotiationRecord::Held { negotiation_id, .. }
        | NegotiationRecord::Rescheduled { negotiation_id, .. }
        | NegotiationRecord::Closed { negotiation_id, .. } => Some(negotiation_id.as_str()),
    }
}

/// Apply one appended record to a folded negotiation — the single transition
/// function shared by the read fold and every mutation, so a mutation's
/// in-memory result can never disagree with what the next read folds.
///
/// Every arm is defensive as well as validated at the write: a line from an
/// older binary, or the loser of the unlocked write race (see the module
/// note), must not smuggle in a transition the write rules refuse.
fn fold_record(negotiation: &mut Negotiation, record: NegotiationRecord) {
    match record {
        // Identity records are `for_audience`'s own job — it decides whether
        // an `Opened` starts, replays or supersedes. One never lands here.
        NegotiationRecord::Opened(_) => {},
        NegotiationRecord::Offered {
            slots,
            offer_act_ref,
            at,
            ..
        } => {
            // A fresh offer only lands on a round that needs one — empty AND
            // unanswered, matching `open`'s write rule exactly. A duplicate
            // line must not clobber a standing offer that already has
            // replies, and a stray line from an older binary must not land an
            // offer over an emptied round the counterparty has already
            // countered into: their times are standing, and burying them
            // under ours records an offer that never stood.
            if negotiation.offered.is_empty()
                && negotiation.held.is_none()
                && negotiation.closed.is_none()
                && negotiation.replies.is_empty()
            {
                negotiation.offered = slots;
                negotiation.offered_at = at;
                negotiation.offer_act_ref = offer_act_ref;
            }
        },
        NegotiationRecord::ReOffered {
            slots,
            offer_act_ref,
            at,
            ..
        } => {
            // A replacement answers a decline or a counter, or fills the
            // empty round a reschedule left. Applied anywhere else, a stray
            // line could erase an acceptance or shadow a standing offer.
            let answers_a_reply = matches!(
                negotiation.replies.last().map(|reply| &reply.kind),
                Some(ReplyKind::Declined { .. }) | Some(ReplyKind::Countered { .. })
            );
            let fills_an_empty_round =
                negotiation.offered.is_empty() && negotiation.replies.is_empty();
            if negotiation.held.is_none()
                && negotiation.closed.is_none()
                && (answers_a_reply || fills_an_empty_round)
            {
                // The round resets around the new offer: the dead slots and
                // the replies they provoked clear (the log keeps the words),
                // and `offered_at` restarts the silence clock.
                negotiation.replies.clear();
                negotiation.offered = slots;
                negotiation.offered_at = at;
                negotiation.offer_act_ref = offer_act_ref;
            }
        },
        NegotiationRecord::Replied { reply, .. } => {
            // The write refuses these; the fold refuses them again so a line
            // from an older binary cannot smuggle a reply into a settled
            // negotiation, double-record a re-read source, or fold in an
            // acceptance of a slot that was never standing. The duplicate
            // check reads `absorbed_sources` — the whole-life memory the fold
            // itself maintains — never the current round's `replies`, which
            // re-offers, reschedules and reopens clear: a round-scoped check
            // is exactly the amnesia that let a re-delivered source
            // double-record after a reset.
            let duplicate = negotiation.absorbed_sources.contains(&reply.source_ref);
            let settled = negotiation.held.is_some() || negotiation.closed.is_some();
            let phantom_acceptance = match &reply.kind {
                ReplyKind::Accepted { slot } => !negotiation.standing_slots().contains(slot),
                _ => false,
            };
            if !duplicate && !settled && !phantom_acceptance {
                negotiation
                    .absorbed_sources
                    .insert(reply.source_ref.clone());
                negotiation.replies.push(reply);
            }
        },
        NegotiationRecord::Held { held, .. } => {
            // The first hold wins, defensively as well as at the write: a
            // duplicate line must not move a booking.
            if negotiation.held.is_none() && negotiation.closed.is_none() {
                negotiation.held = Some(held);
            }
        },
        NegotiationRecord::Rescheduled { why, at, .. } => {
            if negotiation.closed.is_none() {
                // Only a held round can be released; the take() both enforces
                // that and carries the released hold into the history.
                if let Some(released) = negotiation.held.take() {
                    negotiation
                        .reschedules
                        .push(Reschedule { at, why, released });
                    // The old times are dead: the round resets and a fresh
                    // offer is owed.
                    negotiation.offered.clear();
                    negotiation.replies.clear();
                }
            }
        },
        NegotiationRecord::Closed { closed, .. } => {
            // The first close wins, defensively as well as at the write: a
            // duplicate line must not move the date the ask ended.
            if negotiation.closed.is_none() {
                negotiation.closed = Some(closed);
            }
        },
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one ask.
///
/// Derived, never allocated, so a retried open resumes instead of duplicating.
/// The tuple, and why each element is in it:
/// - `scope.principal` and `scope.workspace`: negotiations are per-principal
///   and per-workspace, and two tenants asking the same person must never
///   share a record;
/// - `audience.as_key()`: the **kind** is part of the key, so the same id as
///   an engagement and as an account stay separate negotiations rather than
///   silently merging two relationships that share an id;
/// - `counterparty`: whom we are asking. Trimmed but case-preserved — the
///   identity is resolved by the caller and is exact, and case-folding an
///   identity could merge two people;
/// - `purpose`: the same counterparty can carry two concurrent asks about
///   different things. Normalised for whitespace and case, because the same
///   ask re-extracted from a conversation rarely comes back
///   character-identical.
///
/// The offered slots are deliberately **not** in the tuple: retrying an open
/// with different candidate times is the same ask resuming, not a new
/// negotiation.
///
/// Nothing round- or time-varying is in the tuple either, on purpose: the id
/// stays **stable across rounds**, and a close ends the round, not the
/// identity. Folding a generation counter in would mean a caller re-deriving
/// the id after a close could never find the ask again — the whole point of a
/// derived id is that a retry needs no allocation step to find what it is
/// retrying. Reopening after a close therefore appends a fresh `Opened` under
/// the same id; the fold shows the current round and the log keeps them all.
fn derive_negotiation_id(
    scope: &SchedulingScope,
    audience: &AudienceRef,
    counterparty: &str,
    purpose: &str,
) -> String {
    let purpose = purpose
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    format!(
        "neg-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            audience.as_key(),
            counterparty.trim(),
            purpose,
        ))
    )
}
