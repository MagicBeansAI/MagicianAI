//! **Research worked out who this stranger is. Write it down as a guess.**
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` Phase 3B.
//! The identity-research subsystem
//! ([`crate::magician_v2::chat::public_contact_profile`]) dispatches a real task
//! and comes back with an organisation, a role, a confidence and evidence leads.
//! The counterparty register exists to accumulate exactly that. **Neither ever
//! mentioned the other**: the research settled into a chat-scoped profile and
//! the register stayed empty of everything research had learned.
//!
//! # What this records, and what it can never record
//!
//! [`MintSource::ResearchInferred`], whose own doc is the specification:
//!
//! > We worked it out — a pattern, a directory, a page. **A guess.** It is never
//! > verified, and it may never be promoted on the strength of the research that
//! > produced it.
//!
//! So this mints an organisation and an address under it, both unverified, and
//! **cannot** promote. Promotion stays an owner act carrying a
//! [`TrustedSignal`](crate::magician_v2::counterparties::TrustedSignal) and
//! evidence that DIFFERS from what minted the row — the register enforces that
//! second part itself, which is why a research note cannot become its own proof.
//!
//! An unverified identity confers nothing: `authority()` answers `None` for it,
//! inbound routing treats the sender as a guest, and no engagement lane opens.
//! Writing a guess down is safe precisely because the guess grants nothing.
//!
//! # Which way the dependency runs
//!
//! A **coordinator**, like `introductions`, `retraction`, `run_inbox` and
//! `reply_routing`. `public_contact_profile` keeps knowing nothing about
//! counterparties, and the register keeps knowing nothing about chat.
//!
//! # It refuses more than it records
//!
//! A guess written into the register is a row an owner has to review, so the
//! bar is *"research actually concluded something"* rather than *"research
//! ran"*. Everything below is refused, by name, and none of them is an error —
//! a research run that concluded nothing is the ordinary case.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::magician_v2::chat::inbound_sender::kind_fixed_by_channel;
use crate::magician_v2::chat::public_contact_profile::{
    PublicContactProfileConfidence, PublicContactResearchResult, PublicContactSenderKey,
};
use crate::magician_v2::counterparties::{
    AddIdentity, CounterpartyRef, CounterpartyScope, CounterpartyStore, CreateCounterparty,
    MintSource,
};

/// Why a research result recorded nothing.
///
/// Every arm is an ordinary outcome, not a fault. They are kept apart because
/// they say different things to whoever is wondering why the register is not
/// filling: two of them are about the research, one is about the adapter, and
/// only the last is one an owner can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRecorded {
    /// Research named no organisation. The one thing the register keys on.
    NoOrganisation,
    /// Research named one and is not confident enough to write a row somebody
    /// will have to review.
    BelowConfidence { confidence: String },
    /// The channel does not fix an address kind and nothing named one, so there
    /// is no identity to file. A fact about the adapter — one that names the
    /// kind turns the same result into a recorded guess.
    AddressKindNotFixed { channel: String },
    /// The sender key carries no address at all.
    NoAddress,
    /// The confidence is not a research confidence at all — the owner said it,
    /// or the user claimed it.
    ///
    /// Refused, but **not** because it is weak: an owner-reviewed identity is
    /// stronger than anything research produces. It is refused because minting
    /// it as `ResearchInferred` would file an owner's statement under research
    /// provenance, and provenance is what an owner reads before trusting an
    /// address. Those reach the register through `MintSource::OwnerStated`,
    /// which is a different door with different standing.
    NotResearch { confidence: String },
    /// The address is already on file under a DIFFERENT organisation.
    ///
    /// Research says one thing and the register says another, which is a real
    /// disagreement somebody should look at — not a crash, and not a silent
    /// overwrite. The register would refuse the write itself; catching it here
    /// turns it into a named outcome an owner can act on.
    AddressBelongsToAnother { counterparty_id: String },
}

impl NotRecorded {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoOrganisation => "no_organisation",
            Self::BelowConfidence { .. } => "below_confidence",
            Self::AddressKindNotFixed { .. } => "address_kind_not_fixed",
            Self::NoAddress => "no_address",
            Self::NotResearch { .. } => "not_research",
            Self::AddressBelongsToAnother { .. } => "address_belongs_to_another",
        }
    }
}

/// What a research result put in the register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The organisation and the address now on file, both **unverified**.
    Minted {
        counterparty: CounterpartyRef,
        /// The address in its comparison form, as the register stored it.
        identity: String,
    },
    /// The address was already on file under this organisation. **Nothing was
    /// written**, and nothing needed to be.
    ///
    /// Distinguished from a fresh mint because they are different facts, and
    /// from a skip because the register does hold what research concluded. It
    /// is the ordinary outcome of research running twice over one sender.
    AlreadyOnFile {
        counterparty: CounterpartyRef,
        identity: String,
    },
    /// Nothing was written, and why.
    Skipped(NotRecorded),
}

/// Whether a confidence is strong enough to write a row an owner must review.
///
/// `ResearchedMedium` and above. The two below it are excluded for different
/// reasons and both are deliberate: `ResearchedLow` is research that did not
/// convince itself, and `Unknown` is research that reported no confidence at
/// all — recording either fills the register with rows whose review is a waste
/// of the one attention this whole design is trying to protect.
///
/// `UserClaimed` and `OwnerReviewed` are **not** research and do not arrive
/// here; they are the owner's own statements and reach the register through
/// `MintSource::OwnerStated`, which is a different provenance with different
/// standing.
fn strong_enough(confidence: &PublicContactProfileConfidence) -> bool {
    matches!(
        confidence,
        PublicContactProfileConfidence::ResearchedMedium
            | PublicContactProfileConfidence::ResearchedHigh
    )
}

/// A stable label for the confidence, for the refusal that names it.
fn confidence_label(confidence: &PublicContactProfileConfidence) -> &'static str {
    match confidence {
        PublicContactProfileConfidence::Unknown => "unknown",
        PublicContactProfileConfidence::UserClaimed => "user_claimed",
        PublicContactProfileConfidence::OwnerReviewed => "owner_reviewed",
        PublicContactProfileConfidence::ResearchedLow => "researched_low",
        PublicContactProfileConfidence::ResearchedMedium => "researched_medium",
        PublicContactProfileConfidence::ResearchedHigh => "researched_high",
    }
}

/// Record what research concluded, as an unverified guess.
///
/// Idempotent through the register's own doors: `record_counterparty` is
/// idempotent on the organisation's derived id and `add_identity` on the
/// address's, so re-running research over the same sender resumes both rows
/// rather than duplicating them.
///
/// # Failures are failures
///
/// An unreadable or unwritable register propagates. Folding it into `Skipped`
/// would report a broken register as research that concluded nothing, and the
/// two look identical from the outside — which is how a register stays empty
/// while everything says it is working.
pub fn record_research(
    counterparties: &CounterpartyStore,
    scope: &CounterpartyScope,
    sender: &PublicContactSenderKey,
    research: &PublicContactResearchResult,
    now: DateTime<Utc>,
) -> Result<Recorded> {
    let Some(org) = research
        .org
        .as_deref()
        .map(str::trim)
        .filter(|org| !org.is_empty())
    else {
        return Ok(Recorded::Skipped(NotRecorded::NoOrganisation));
    };
    match &research.confidence {
        PublicContactProfileConfidence::UserClaimed
        | PublicContactProfileConfidence::OwnerReviewed => {
            return Ok(Recorded::Skipped(NotRecorded::NotResearch {
                confidence: confidence_label(&research.confidence).to_string(),
            }))
        },
        confidence if !strong_enough(confidence) => {
            return Ok(Recorded::Skipped(NotRecorded::BelowConfidence {
                confidence: confidence_label(confidence).to_string(),
            }))
        },
        _ => {},
    }

    let address = sender.channel_address.trim();
    if address.is_empty() {
        return Ok(Recorded::Skipped(NotRecorded::NoAddress));
    }
    // The channel's own mapping, not a second one. Two functions that must
    // agree about "what kind of address does this channel carry" and do not
    // share code disagree eventually, and here the disagreement files a phone
    // number as an email — an identity nothing will ever resolve to.
    let Some(kind) = kind_fixed_by_channel(&sender.channel_type) else {
        return Ok(Recorded::Skipped(NotRecorded::AddressKindNotFixed {
            channel: sender.channel_type.clone(),
        }));
    };

    // Two reads before any write, and the order matters.
    //
    // `evidence_ref` carries the research task id, so a SECOND research run over
    // the same sender arrives with different provenance — and the register
    // refuses a changed payload rather than silently keeping the first, because
    // provenance is what an owner reads before trusting an address. That
    // refusal is right and general; what would be wrong is letting the ordinary
    // case of research running twice surface as an error.
    //
    // So: resolve the address first. Already ours means nothing to do; somebody
    // else's means research and the register disagree, which is a fact to
    // report rather than a write to force. Only then is an organisation
    // recorded, so a conflicting address does not leave an organisation row
    // behind that nothing points at.
    let existing_owner = counterparties.resolve(scope, kind, address)?;
    if let Some(owner) = existing_owner {
        // Only asked when the address IS on file. On the common path — the
        // first research over a new sender — resolving the organisation label
        // would be a register read whose answer nothing consults.
        let already_ours = counterparties
            .resolve_labels(scope, &[org.to_string()])?
            .get(org)
            .and_then(Option::as_ref)
            .cloned();
        return Ok(match already_ours {
            Some(ours) if ours == owner => Recorded::AlreadyOnFile {
                identity: crate::magician_v2::counterparties::normalise_identity(kind, address)?,
                counterparty: owner,
            },
            _ => Recorded::Skipped(NotRecorded::AddressBelongsToAnother {
                counterparty_id: owner.as_str().to_string(),
            }),
        });
    }

    let counterparty = counterparties.record_counterparty(
        scope,
        &CreateCounterparty {
            display_name: org.to_string(),
            // Not derived from the address. An email domain is a plausible
            // organisation domain and a wrong one often enough — a personal
            // address, a shared inbox, a forwarder — and `domain` is a hint the
            // candidate review reads, so a guess here becomes somebody else's
            // near miss.
            domain: None,
            stage: None,
            created_by: "identity-research".to_string(),
        },
        now,
    )?;

    let identity = counterparties.add_identity(
        scope,
        &AddIdentity {
            counterparty_id: counterparty.counterparty_id.clone(),
            kind,
            value: address.to_string(),
            // The whole point. Never verified, and the register refuses to
            // promote it on the evidence that minted it.
            source: MintSource::ResearchInferred,
            // The research run itself, so an owner reviewing the row can read
            // what produced it. `task_id` is always present; the execution and
            // output ids are not.
            evidence_ref: format!("identity-research:{}", research.task_id),
            recorded_by: "identity-research".to_string(),
            // Refused for any source but `Introduced` — an introduction is a
            // provenance, and research is not a vouch.
            introduced_by: None,
        },
        now,
    )?;

    Ok(Recorded::Minted {
        counterparty: CounterpartyRef::new(counterparty.counterparty_id),
        identity: identity.normalised,
    })
}

#[cfg(test)]
mod tests;
