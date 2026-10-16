//! **A reply arrived. Which ask is it about?**
//!
//! Doc: `docs/plans/2026-08-07-opc-composable-work-modules.md` §3, one of the two
//! things it says *"must not be skipped"*:
//!
//! > The invite returns on a channel. Their acceptance, decline or reschedule
//! > arrives as email — it must resolve to the same counterparty, which is what
//! > engagements' identity set is for. **Scheduling without that produces orphan
//! > replies.**
//!
//! `scheduling` holds up its half and says so: it *"does not resolve
//! identities"*, and takes an already-resolved audience and negotiation id. That
//! refusal is what lets any flow on any channel use it. What was missing is the
//! other half — nothing turned *"an email from this address"* into *"this ask"*,
//! so a caller had to already know the negotiation id, which is precisely what
//! whoever receives the reply does not have.
//!
//! # Which way the dependency runs
//!
//! A **coordinator**, like `introductions`, `retraction` and `run_inbox`. It
//! takes a relationship the caller has already resolved and answers which open
//! ask lives there. It does not read an inbox, does not resolve an address, and
//! does not decide what the reply MEANS — the reading stays supplied, because
//! turning prose into a verdict is language work and a parser reading *"no
//! problem, but let me check with my co-founder"* as an acceptance puts words in
//! a counterparty's mouth and then books a room.
//!
//! # Two open asks in one relationship close NEITHER
//!
//! The same rule the run inbox applies to a contested source, for the same
//! reason: one reply answers one ask, and picking is a coin flip whose loser is
//! settled by words that were never about it. A counterparty with two live asks
//! is legitimate — a demo and a contract review — and the reply names which only
//! in prose this module deliberately does not read.
//!
//! So it refuses, by name, and the caller absorbs against the id it chooses.
//! Refusing is recoverable; guessing writes a decision into an append-only log.

use anyhow::Result;

use crate::scheduling::{Negotiation, SchedulingScope, SchedulingStore};
use magician::magician_v2::audience::AudienceRef;

/// Which ask a reply belongs to.
#[derive(Debug, Clone)]
pub enum ReplyTarget {
    /// Exactly one ask in that relationship is still waiting. The only arm a
    /// caller may absorb against without choosing.
    One(Box<Negotiation>),
    /// The relationship has no ask waiting for an answer.
    ///
    /// **Not an error.** A counterparty writes about many things, and treating
    /// every message that is not a scheduling reply as a failure would make the
    /// loop unusable — the same reason `run_inbox` counts unmatched mail rather
    /// than raising on it.
    NoOpenAsk,
    /// More than one ask is waiting. Nothing may be absorbed.
    Ambiguous { negotiation_ids: Vec<String> },
}

impl ReplyTarget {
    /// The ask, when there is exactly one.
    pub fn one(&self) -> Option<&Negotiation> {
        match self {
            Self::One(negotiation) => Some(negotiation),
            Self::NoOpenAsk | Self::Ambiguous { .. } => None,
        }
    }

    /// A stable label for logs and review.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::One(_) => "one_open_ask",
            Self::NoOpenAsk => "no_open_ask",
            Self::Ambiguous { .. } => "ambiguous_open_asks",
        }
    }
}

/// The open ask in a relationship, if there is exactly one.
///
/// "Open" is `!state().is_settled()` — held and closed are settled and absorb
/// nothing, which is the store's own rule read here rather than re-derived. An
/// ask that has been declined is **not** settled: a decline is an answer, and
/// the counterparty may still counter after it.
///
/// # Failures are failures
///
/// An unreadable log propagates rather than folding to `NoOpenAsk`. *"They have
/// no ask waiting"* and *"we could not read whether they do"* are opposite
/// facts, and only the first is one a caller may act on — the second, folded,
/// turns a reply into an orphan for a reason nothing records.
pub fn open_ask_for(
    store: &SchedulingStore,
    scope: &SchedulingScope,
    audience: &AudienceRef,
) -> Result<ReplyTarget> {
    let open: Vec<Negotiation> = store
        .for_audience(scope, audience)?
        .into_iter()
        .filter(|negotiation| !negotiation.state().is_settled())
        .collect();

    match open.len() {
        0 => Ok(ReplyTarget::NoOpenAsk),
        1 => Ok(ReplyTarget::One(Box::new(
            open.into_iter().next().expect("length checked"),
        ))),
        // Sorted so two reads of an unchanged relationship name them in the
        // same order — an owner comparing two reports should not have to
        // reconcile the ordering before reconciling the content.
        _ => {
            let mut negotiation_ids: Vec<String> = open
                .into_iter()
                .map(|negotiation| negotiation.negotiation_id)
                .collect();
            negotiation_ids.sort();
            Ok(ReplyTarget::Ambiguous { negotiation_ids })
        },
    }
}

#[cfg(test)]
mod tests;
