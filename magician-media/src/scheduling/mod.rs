//! Negotiating a time with someone outside — composable work modules, Module A.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §3.
//!
//! §3's contract: `propose(counterparty, window, duration, constraints) ->
//! slots[]; offer(slots) — sent on whatever channel the conversation is on;
//! absorb(reply) — accept | decline | counter-propose | silence; hold(slot);
//! reschedule(event, why)`. One verb is added over §3:
//! `re_offer(slots)` — our answer to their decline or counter, **replacing**
//! the standing slots. Without it a changed offer had no recorded transition:
//! a replayed `offer` silently dropped the new times, and the negotiation
//! wedged the moment the counterparty accepted one of them.
//!
//! # What this module is
//!
//! The record of the negotiation: what was offered, what they said, what got
//! booked, what moved and why. Current state is the fold of an append-only
//! per-audience log, so the negotiation is evidence, not a mutable status.
//!
//! # What this module refuses to do, and why
//!
//! - **It does not propose.** `propose -> slots[]` is the agent's judgment
//!   over a calendar and constraints this module never sees; baking that in
//!   would couple the record to one way of choosing times.
//! - **It does not send, and it does not touch a calendar.** `offer` and
//!   `hold` are outward acts the consumer performs; this module stores their
//!   refs (`offer_act_ref`, `calendar_event_ref`) so the record links to the
//!   outward-assertions trail without owning it.
//! - **It does not resolve identities.** §3: *"The invite returns on a
//!   channel. Their acceptance… must resolve to the same counterparty."*
//!   That resolution is the caller's job, done through the audience — this
//!   module takes an [`AudienceRef`](magician::magician_v2::audience::AudienceRef)
//!   and an already-resolved counterparty
//!   identity, and never reads an inbox, a roster or a directory. Loose
//!   coupling is the point: any flow on any channel can use it.
//! - **It does not chase.** §3: *"Silence is a state. No answer is neither
//!   yes nor no; it is a thing to chase later, which is Module D."* Silence
//!   is derived from the clock at read time, never stored, and
//!   [`Negotiation::silence_follow_up`] emits the `RecordObligation` that
//!   hands the chase to Module D.
//! - **It has no open-ended audience.** A negotiation lives in a named,
//!   enumerable relationship; there is no "public" to schedule with, and the
//!   audience type cannot express one.

pub mod consumer;
mod recipient_compliance;
pub mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use consumer::{
    absorb_reply, apply_silence_sweep, hold_intent, offer_message, read_reply, record_hold,
    silence_obligations, sweep_silence, AppliedSilenceSweep, HoldIntent, IdentifiedObligation,
    InboundReading, InboundReply, OfferMessage, SettledSilence, SilenceSweep,
    SILENCE_ANSWERED_NOTE, SILENCE_CLOSED_REASON,
};
#[cfg(feature = "test-fixtures")]
pub use recipient_compliance::{
    absorb_recipient_compliance_reply_for_test, close_recipient_compliance_negotiation_for_test,
    open_recipient_compliance_negotiation_for_test, read_recipient_compliance_negotiations_json,
};
pub use recipient_compliance::{
    install_recipient_compliance_scheduling_reader, read_recipient_compliance_negotiations,
};
pub use store::{SchedulingScope, SchedulingStore};
pub use types::{Closed, Held, Negotiation, NegotiationState, Reply, ReplyKind, Reschedule, Slot};
