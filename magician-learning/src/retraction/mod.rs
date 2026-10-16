//! **A claim turned out to be wrong. Which rooms are serving it?**
//!
//! Doc: `docs/plans/2026-08-07-opc-deal-close.md` §4 and §9. The plan's rule is
//! one sentence and every part of it is load-bearing:
//!
//! > retracting a claim does **not** silently swap the file — it raises an
//! > explicit **replacement or revocation event** against every room carrying
//! > that revision, which the owning agent acts on.
//!
//! `evidence::outward_assertions` already answers the *sent* half — who we told,
//! through which payload — and `corrections_api` raises the debt for it. This is
//! the *served* half, which nothing answered: a deck sitting in a data room is
//! not an outward act, it is a standing offer to read one, and a correction that
//! chased only what was emailed would leave the wrong number on a page a
//! counterparty can open today.
//!
//! # Which way the dependency runs
//!
//! A **coordinator**, like `introductions` and `delivery_hygiene`.
//! [`magician::magician_v2::claim_manifest`] knows which revision carries which
//! claim; [`crate::data_room`] knows which room holds which
//! document; [`magician::magician_v2::obligations`] knows what is owed. None of the
//! three may import another — a claim register that knew what a room was would
//! stop being usable for a deck nobody shared.
//!
//! # It proposes; it does not record, and it does not withdraw
//!
//! [`retraction_sweep`] returns what is affected and what is owed, and writes
//! nothing. Deliberately, and for two different reasons:
//!
//! - **The obligation is a judgement.** Replace, revoke, or say nothing is the
//!   owner's call — §9's own list.
//! - **Withdrawing the document automatically would be the silent swap the plan
//!   forbids.** A room whose contents change under a reader, with no event, is
//!   exactly the failure this machinery exists to make impossible.
//!
//! # Revocation cannot recall a download
//!
//! The plan says so and this module models it. A document already withdrawn, or
//! a room already closed, still reports as [`Exposure::Past`] rather than
//! disappearing: access is blocked, the copy somebody took is not. An
//! implementation that filtered those out would answer *"nobody is holding it"*
//! about people who are.
//!
//! # Pinned and unpinned are different answers
//!
//! §4 requires a room to reference `artifact_ref@revision`, and since
//! 2026-08-21 `DataRoomStore::add_document` refuses anything else. Rows written
//! **before** that guard may still hold a bare reference, which means the room
//! serves whatever is current and **we cannot tell from here whether that
//! carries the claim**. Those come back as [`Certainty::Unpinned`], counted
//! apart. Folding them into the confirmed list would overstate the blast radius;
//! dropping them would understate it, and understating is the direction that
//! leaves a wrong number in front of somebody.
//!
//! The arm stays even though nothing new can reach it: a history that predates a
//! rule does not retroactively obey it, and a sweep that assumed otherwise would
//! answer confidently about rooms it cannot read.

use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::data_room::{
    split_document_ref, DataRoomScope, DataRoomStore, RoomStanding, REVISION_SEP,
};
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::claim_manifest::{ClaimManifestScope, ClaimManifestStore};
use magician::magician_v2::obligations::{ObligationDirection, RecordObligation};

/// How sure we are that a room's document carries the retracted claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// The room names an exact revision, and that revision's manifest binds the
    /// claim. This is the case §4 designed for.
    Pinned,
    /// The room names an artifact with no revision, so it serves whatever is
    /// current. It may or may not carry the claim — nothing here can tell, and
    /// saying either would be a guess about what a counterparty is reading.
    Unpinned,
}

impl Certainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
        }
    }
}

/// Whether the room is still serving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// In the room, in an open room, right now.
    Current,
    /// Withdrawn, or the room is closed or expired. **Access is blocked; a copy
    /// already taken is not recalled** — which is why this is reported rather
    /// than filtered away.
    Past,
}

impl Exposure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Past => "past",
        }
    }
}

/// One room holding a document that carries — or may carry — the claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedRoom {
    pub room_id: String,
    pub audience: AudienceRef,
    /// The reference exactly as the room holds it, pin and all.
    pub document_ref: String,
    /// The artifact half, once the pin is split off.
    pub artifact_ref: String,
    /// The revision half, when the room named one.
    pub revision_ref: Option<String>,
    pub certainty: Certainty,
    pub exposure: Exposure,
}

/// What one retraction reaches.
#[derive(Debug, Clone, Default)]
pub struct RetractionSweep {
    /// Rooms whose pinned revision carries the claim.
    pub confirmed: Vec<AffectedRoom>,
    /// Rooms holding the artifact with no revision pin. Counted apart: we
    /// cannot say whether what they serve carries the claim.
    pub unpinned: Vec<AffectedRoom>,
    /// One proposal per affected AUDIENCE, not per room. What is owed is a
    /// conversation with a counterparty, and four messages to one person about
    /// four rooms is the behaviour this exists to prevent.
    pub proposed: Vec<RecordObligation>,
    /// How many rooms were read. The denominator — "reached none" over three
    /// rooms and over three hundred are different facts.
    pub rooms_seen: usize,
    /// Revisions the claim register says carry the claim. Reported even when no
    /// room holds them: a claim carried by a revision nobody shared is a fact
    /// about the artifact, and a zero here means something different from a
    /// zero in `rooms_seen`.
    pub revisions_carrying: usize,
    /// Document references this sweep could not read unambiguously.
    ///
    /// A reference carrying more than one [`REVISION_SEP`] cannot be split
    /// reliably — `mailto:a@b.test` reads as artifact `mailto:a` pinned to
    /// revision `b.test` — and the write guard refuses those, so any that exist
    /// predate it or arrived some other way. They are still evaluated under the
    /// last-separator reading, so a match is still reported; what this list adds
    /// is that the reading was a GUESS.
    ///
    /// It matters in one direction. A misread reference matches no carried
    /// artifact, so the room is skipped — and a room silently skipped by a
    /// correction is a wrong figure left in front of somebody, which is the
    /// failure the pin exists to prevent. Reported rather than dropped, so
    /// "found none" and "could not read three of them" are different answers.
    pub ambiguous_document_refs: Vec<String>,
}

impl RetractionSweep {
    /// Every room this retraction touches, whichever certainty.
    pub fn affected(&self) -> usize {
        self.confirmed.len() + self.unpinned.len()
    }
}

/// Which rooms are serving a retracted claim, and what that owes.
///
/// # Failures are failures
///
/// An unreadable claim register or room list propagates rather than folding to
/// "nothing affected". This is the read that decides whether anybody is told
/// their information was wrong, and an empty answer from a broken store is the
/// most reassuring wrong answer it could give.
/// Whether this sweep can only GUESS at what a stored reference means.
///
/// [`split_document_ref`] splits on the LAST separator, which is the only
/// sensible rule for reading and cannot tell an unpinned reference that merely
/// contains a separator from a pinned one: `mailto:a@b.test` reads as artifact
/// `mailto:a` at revision `b.test`, matches no carried artifact, and the room
/// carrying the retracted claim is skipped.
///
/// `DataRoomStore::add_document` refuses these, so no new write can create one.
/// A history written before that guard existed is not retroactively obedient,
/// which is the same reason the unpinned arm is reachable — and a correction
/// that silently skips a room leaves a wrong figure in front of somebody.
fn reading_is_a_guess(document_ref: &str) -> bool {
    document_ref.matches(REVISION_SEP).count() > 1
}

pub fn retraction_sweep(
    manifests: &ClaimManifestStore,
    rooms: &DataRoomStore,
    manifest_scope: &ClaimManifestScope,
    room_scope: &DataRoomScope,
    claim_ref: &str,
    correction_ref: &str,
    created_by: &str,
    now: DateTime<Utc>,
) -> Result<RetractionSweep> {
    let claim_ref = claim_ref.trim();
    if claim_ref.is_empty() {
        anyhow::bail!("a retraction must name the claim being retracted");
    }
    if correction_ref.trim().is_empty() {
        anyhow::bail!(
            "a retraction must name what the correction IS: the obligation points at it, and \
             one that points nowhere cannot be acted on"
        );
    }

    let carrying = manifests.revisions_carrying(manifest_scope, claim_ref)?;
    let mut sweep = RetractionSweep {
        revisions_carrying: carrying.len(),
        ..RetractionSweep::default()
    };

    // Indexed once, not scanned per document. `revisions_carrying` walks EVERY
    // artifact carrying the claim and every revision of each, so `carrying`
    // grows with how widely the claim was cited — and the scans it replaced ran
    // twice per document of every room, making the sweep
    // `rooms × documents × carrying`. The worst case is a claim that ended up in
    // everything, which is exactly the retraction that matters most and would
    // have been the slowest to answer.
    //
    // Borrowed keys are safe here: `carrying` is bound above the loop and
    // outlives every borrow taken from it.
    let carried_artifacts: BTreeSet<&str> = carrying
        .iter()
        .map(|held| held.artifact_ref.as_str())
        .collect();
    let carried_revisions: BTreeSet<(&str, &str)> = carrying
        .iter()
        .map(|held| (held.artifact_ref.as_str(), held.revision_ref.as_str()))
        .collect();

    for room in rooms.list(room_scope)? {
        sweep.rooms_seen += 1;
        let room_open = room.standing(now) == RoomStanding::Open;
        for document in &room.documents {
            // The store's own splitter, not a second copy. Two functions that
            // must agree about "which revision is this" and do not share code
            // will disagree eventually, and here the disagreement is a
            // correction that misses a room.
            let (artifact, revision) = split_document_ref(&document.artifact_ref);
            // Counted BEFORE the match, and the match still runs. Skipping an
            // unreadable reference would hide it; refusing the whole sweep for
            // one would lose every room it could read. So it is evaluated under
            // the guessed reading and named in the report either way.
            if reading_is_a_guess(&document.artifact_ref) {
                sweep
                    .ambiguous_document_refs
                    .push(document.artifact_ref.clone());
            }
            if !carried_artifacts.contains(artifact) {
                continue;
            }
            let certainty = match revision {
                // Named a revision, and it is one that carries the claim.
                Some(revision) if carried_revisions.contains(&(artifact, revision)) => {
                    Certainty::Pinned
                },
                // Named a revision that does NOT carry it. The room is serving a
                // different version of the artifact, which is the case §4's
                // pinning exists to make visible — and it is genuinely
                // unaffected, so it is skipped rather than reported.
                Some(_) => continue,
                None => Certainty::Unpinned,
            };
            let exposure = if room_open && document.is_present() {
                Exposure::Current
            } else {
                Exposure::Past
            };
            let entry = AffectedRoom {
                room_id: room.room_id.clone(),
                audience: room.audience.clone(),
                document_ref: document.artifact_ref.clone(),
                artifact_ref: artifact.to_string(),
                revision_ref: revision.map(str::to_string),
                certainty,
                exposure,
            };
            match certainty {
                Certainty::Pinned => sweep.confirmed.push(entry),
                Certainty::Unpinned => sweep.unpinned.push(entry),
            }
        }
    }

    sweep.proposed = proposals(&sweep, claim_ref, correction_ref, created_by, now);
    Ok(sweep)
}

/// One obligation per affected audience.
///
/// Grouped by audience rather than by room because what is owed is a
/// conversation with a counterparty. Four rooms shared with one investor is one
/// message, and a register that produced four would be the noise that makes
/// people stop reading it.
fn proposals(
    sweep: &RetractionSweep,
    claim_ref: &str,
    correction_ref: &str,
    created_by: &str,
    now: DateTime<Utc>,
) -> Vec<RecordObligation> {
    use std::collections::BTreeMap;

    let mut by_audience: BTreeMap<String, (AudienceRef, usize, usize, bool)> = BTreeMap::new();
    for room in sweep.confirmed.iter().chain(sweep.unpinned.iter()) {
        let entry = by_audience
            .entry(room.audience.as_key())
            .or_insert_with(|| (room.audience.clone(), 0, 0, false));
        match room.certainty {
            Certainty::Pinned => entry.1 += 1,
            Certainty::Unpinned => entry.2 += 1,
        }
        if room.exposure == Exposure::Current {
            entry.3 = true;
        }
    }

    by_audience
        .into_values()
        .map(|(audience, confirmed, unpinned, any_current)| {
            // Composed rather than templated, because the counts are what an
            // owner acts on and "0 pinned document(s)" is the sentence a
            // template produces for the case where every affected room is
            // unpinned — which reads as nothing to do, for the case that most
            // needs looking at.
            let mut what =
                format!("Claim `{claim_ref}` was retracted (correction `{correction_ref}`). ");
            match (confirmed, unpinned) {
                (0, _) => what.push_str(&format!(
                    "No document shared with this counterparty pins a revision that carries it, \
                     but {unpinned} name no revision at all — those serve whatever is current \
                     and have to be looked at"
                )),
                (_, 0) => what.push_str(&format!(
                    "Decide replacement or revocation for {confirmed} document(s) shared with \
                     this counterparty"
                )),
                _ => what.push_str(&format!(
                    "Decide replacement or revocation for {confirmed} document(s) shared with \
                     this counterparty, and look at {unpinned} more whose reference names no \
                     revision — those may or may not carry it"
                )),
            }
            if !any_current {
                // Said outright. Every copy is already out of reach, and an
                // owner reading "revoke it" for a closed room would think the
                // problem was solved by an action that solves nothing.
                what.push_str(
                    ". Nothing here is still being served — access is already closed, and a \
                     copy already taken is not recalled by anything you do now",
                );
            }
            RecordObligation {
                audience,
                program_id: None,
                what,
                // Due now: the claim is already wrong and already read.
                due_at: now,
                direction: ObligationDirection::OwedByUs,
                created_by: created_by.to_string(),
                source_act_ref: Some(correction_ref.to_string()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
