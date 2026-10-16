//! What fills the obligation register — the composition layer for the sweeps.
//!
//! Doc: `docs/components/magician/obligations.md`. Plan:
//! `docs/plans/2026-08-07-opc-composable-work-modules.md` §6.
//!
//! Module D's register is where *"send me your metrics by Friday"* is supposed
//! to survive Friday. Two derivations produce rows for it —
//! [`data_room::sweep::sweep_follow_ups`](magician_learning::data_room::sweep_follow_ups)
//! for a room nobody opened, and
//! [`scheduling::consumer::sweep_silence`](crate::scheduling::sweep_silence)
//! for an offer nobody answered — and both were complete, tested, and called
//! by nothing. A register nothing writes to is a register that reads empty
//! forever, and an empty register is indistinguishable from a relationship
//! where nobody owes anybody anything. This module is the composition that
//! makes the difference visible.
//!
//! # Why it lives here and not inside `obligations`
//!
//! The register is the most primitive of the four modules: it knows about
//! promises, deadlines and direction, and deliberately nothing else. If it
//! imported the data room and the scheduling record in order to fill itself,
//! every future emitter would have to be added to it — and a promise extracted
//! from a transcript, a support SLA, a procurement deadline would each mean
//! editing Module D. The dependency runs the other way here: this module
//! imports all three, and none of them imports it. A new emitter is a new
//! branch in [`sweep_scope`] or its own caller of
//! [`ObligationStore::record`](magician::magician_v2::obligations::ObligationStore::record),
//! never a change to the register.
//!
//! # Nothing here names a kind of relationship
//!
//! Rooms and negotiations both carry an
//! [`AudienceRef`](magician::magician_v2::audience::AudienceRef), and this module
//! passes whichever one it finds straight through. A support account's room, a
//! recruiting panel's scheduling ask and a supplier's diligence pack sweep
//! through the identical code path, and no arm of [`AudienceKind`] is named
//! anywhere in this file. Adding a second flow requires no edit here.
//!
//! [`AudienceKind`]: magician::magician_v2::audience::AudienceKind
//!
//! # Fail closed
//!
//! - **An unreadable store is an error, never an empty sweep.** Every read
//!   below propagates, so a scope whose logs cannot be read fails the tick
//!   rather than reporting *"nothing is owed"* — which is the single most
//!   reassuring wrong answer a register can give.
//! - **A first sweep passes the current view as its own previous.** That is
//!   the form both derivations document as *"first sweep"* or *"nothing has
//!   changed"*, and it settles nothing. Passing an EMPTY previous view instead
//!   would be a claim that the last sweep saw no relationships, which for the
//!   data room's settle half is a vacuous membership test over an empty
//!   collection — the bug class that would retire a whole register in one call.
//! - **`replied` is never guessed.** A room has no view of mail, phone calls
//!   or meetings, so every token enters the snapshot unanswered. That is the
//!   direction that keeps a signal: at worst it raises a follow-up on somebody
//!   who answered elsewhere, which an owner settles in one act, where the other
//!   default would silently suppress every follow-up in every room.
//!
//! # What the memory is, and what is lost without it
//!
//! [`SweepMemory`] is the previous tick's view. The data room's settle half
//! derives from **absence** — an obligation the previous snapshot implied and
//! the current one does not is superseded — so without a memory a follow-up
//! whose basis moved (a never-opened token that has since opened, changing the
//! delivery question into a nudge) is never retired. The memory is held by the
//! caller, so a worker keeps it across ticks; it is **not persisted**, so a
//! restart loses it and the superseded row stays live in the register until an
//! owner settles it. That is the fail-closed direction — a stale chase stays
//! visible rather than a live one vanishing — and it is a real cost, named
//! here rather than hidden.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use crate::scheduling::{
    apply_silence_sweep, sweep_silence, Negotiation, SchedulingScope, SchedulingStore,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::obligations::{ObligationScope, ObligationStore};
use magician::magician_v2::share_links::{ShareLinkScope, ShareLinkStore};
use magician_learning::data_room::access_store::{AccessScope, AccessStore};
use magician_learning::data_room::{
    apply_follow_up_sweep, snapshot_from_events, sweep_follow_ups, DataRoom, DataRoomScope,
    DataRoomStore, FollowUpPolicy, SharedToken, TokenContext,
};

// The reading that explains a register row. It composes the SAME per-room
// snapshot this module sweeps from, so the note and the row it points at cannot
// have been derived from two different views of the room.
pub mod attention;
pub mod worker;

#[cfg(test)]
mod tests;

/// The two windows a sweep needs, and neither of them defaulted.
///
/// The wait IS the meaning of both signals, so a policy without one is refused
/// rather than substituted: at a zero window every offer is silent the instant
/// it is made and every share is a delivery question before the mail could
/// have arrived, and the register floods with rows nobody can act on. The
/// data room's own [`FollowUpPolicy`] already refuses its two; this refuses
/// the third for the same reason.
#[derive(Debug, Clone)]
pub struct SweepPolicy {
    silence_window: Duration,
    follow_up: FollowUpPolicy,
}

impl SweepPolicy {
    /// Build a policy, refusing a non-positive silence window.
    pub fn new(silence_window: Duration, follow_up: FollowUpPolicy) -> Result<Self> {
        if silence_window <= Duration::zero() {
            anyhow::bail!(
                "the scheduling silence window must be positive: at zero or below every offer \
                 is silent the instant it is made, and the register fills with a chase for \
                 every ask before the counterparty could have read it"
            );
        }
        Ok(Self {
            silence_window,
            follow_up,
        })
    }

    pub fn silence_window(&self) -> Duration {
        self.silence_window
    }

    pub fn follow_up(&self) -> &FollowUpPolicy {
        &self.follow_up
    }
}

/// The previous tick's view of one scope.
///
/// Held by the caller rather than by this module, because a sweep is a pure
/// function of two views and the thing that remembers between them is whatever
/// is doing the sweeping — a worker across ticks, a route that has no previous
/// tick at all.
#[derive(Debug, Clone, Default)]
pub struct SweepMemory {
    /// Whether this is a real previous view.
    ///
    /// A default-constructed memory is **not**. A caller's first sweep has
    /// never seen the scope, and handing an empty previous view to the
    /// derivations would assert that the last sweep observed no relationships
    /// — which is exactly the vacuous emptiness both of them refuse. When this
    /// is false the current view is passed for both, which the derivations
    /// document as the honest form of *"first sweep"*.
    seeded: bool,
    negotiations: Vec<Negotiation>,
    /// Keyed by room id, so a room this caller has not seen before falls back
    /// to its own current snapshot rather than to another room's.
    room_snapshots: BTreeMap<String, Vec<TokenContext>>,
}

impl SweepMemory {
    /// A memory that has seen nothing — the first sweep.
    pub fn unseeded() -> Self {
        Self::default()
    }

    /// Whether this memory is a real previous view rather than a first sweep.
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }
}

/// What one scope's sweep did to the register.
///
/// Counts of rows, never rates. *"Four chases recorded, one settled, two rooms
/// seen"* is a fact an owner can reconcile against the register in front of
/// them; *"80% follow-up coverage"* is a number that hides which rows moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSweepReport {
    pub negotiations_seen: usize,
    pub rooms_seen: usize,
    /// Register rows the sweep recorded or resumed.
    pub recorded: usize,
    /// Register rows the sweep settled — met or released.
    pub settled: usize,
    /// Settle handles the register does not hold. Reported, never raised: the
    /// recording half for that basis never ran, so there is nothing to settle.
    pub absent: usize,
}

/// Sweep one scope: derive every obligation its activity implies, and write
/// them.
///
/// Two derivations run, in this order, and both against the SAME instant so a
/// row cannot ripen between them:
///
/// 1. **Scheduling silence** — every negotiation in the scope, whatever
///    relationship it lives in. Offers nobody answered ripen into chases; asks
///    that have since been answered or closed settle the chase a previous
///    sweep raised.
/// 2. **Data room follow-ups** — every room in the scope. A shared link nobody
///    opened becomes a delivery question; a room read and then gone quiet
///    becomes a follow-up.
///
/// Returns the report and the memory the NEXT sweep should be handed. A caller
/// that discards the memory still sweeps correctly — it simply retires
/// superseded rows more slowly; see the module note.
///
/// # Errors
///
/// Any store that cannot be read, any derivation that refuses. A scope that
/// fails contributes nothing rather than contributing a partial answer: a
/// half-swept scope reported as swept is a register the owner would trust more
/// than it deserves.
pub fn sweep_scope(
    workspace_layout: &ArtifactV2Workspace,
    scope: &ObligationScope,
    previous: &SweepMemory,
    policy: &SweepPolicy,
    now: DateTime<Utc>,
) -> Result<(ScopeSweepReport, SweepMemory)> {
    let register = ObligationStore::new(workspace_layout.clone());
    let mut report = ScopeSweepReport::default();
    let mut next = SweepMemory {
        seeded: true,
        ..SweepMemory::default()
    };

    // ── 1. Scheduling silence ───────────────────────────────────────────────
    let negotiations = SchedulingStore::new(workspace_layout.clone())
        .all_negotiations(&SchedulingScope::new(
            scope.principal.clone(),
            scope.workspace.clone(),
        ))
        .with_context(|| {
            format!(
                "reading the negotiations of `{}`/`{}` to sweep for silence",
                scope.principal, scope.workspace
            )
        })?;
    report.negotiations_seen = negotiations.len();

    // An unseeded memory passes the CURRENT view as its own previous — the
    // documented form of "first sweep". Never an empty slice: that would claim
    // the last sweep saw no asks at all.
    let previous_negotiations: &[Negotiation] = if previous.seeded {
        &previous.negotiations
    } else {
        &negotiations
    };
    let silence = sweep_silence(
        scope,
        previous_negotiations,
        &negotiations,
        policy.silence_window,
        now,
    )
    .context("deriving the silence sweep")?;
    let applied = apply_silence_sweep(&register, scope, &silence, now)
        .context("writing the silence sweep to the register")?;
    report.recorded += applied.recorded.len();
    report.settled += applied.settled.len();
    report.absent += applied.absent.len();
    next.negotiations = negotiations;

    // ── 2. Data room follow-ups ─────────────────────────────────────────────
    let rooms = DataRoomStore::new(workspace_layout.clone())
        .list(&DataRoomScope::new(
            scope.principal.clone(),
            scope.workspace.clone(),
        ))
        .with_context(|| {
            format!(
                "listing the data rooms of `{}`/`{}` to sweep for follow-ups",
                scope.principal, scope.workspace
            )
        })?;
    report.rooms_seen = rooms.len();

    let links = ShareLinkStore::new(workspace_layout.clone());
    let link_scope = ShareLinkScope::new(scope.principal.clone(), scope.workspace.clone());
    let access = AccessStore::new(workspace_layout.clone());
    let access_scope = AccessScope::new(scope.principal.clone(), scope.workspace.clone());

    for room in rooms {
        // One assembly, shared with `attention::attention_notes_for_scope`. A
        // second copy here would let the sweep and the note that explains its
        // row see different rosters, and the note would then carry a handle for
        // an obligation derived from a view it never saw.
        let current = snapshot_room(&links, &link_scope, &access, &access_scope, &room)?;

        // A room this caller has not seen before is its own previous, for the
        // same reason an unseeded memory is: absence is not evidence that a
        // follow-up stopped applying.
        let previous_snapshot: &[TokenContext] = previous
            .room_snapshots
            .get(&room.room_id)
            .map(Vec::as_slice)
            .unwrap_or(&current);

        let sweep = sweep_follow_ups(
            scope,
            &room.audience,
            &room.room_id,
            previous_snapshot,
            &current,
            &policy.follow_up,
            now,
        )
        .with_context(|| format!("deriving the follow-up sweep of room `{}`", room.room_id))?;
        let applied = apply_follow_up_sweep(&register, scope, &sweep, now)
            .with_context(|| format!("writing the follow-up sweep of room `{}`", room.room_id))?;
        report.recorded += applied.recorded.len();
        report.settled += applied.settled.len();
        report.absent += applied.absent.len();

        next.room_snapshots.insert(room.room_id.clone(), current);
    }

    Ok((report, next))
}

// ── Internals ───────────────────────────────────────────────────────────────

/// One room's current view, assembled the one way.
///
/// `pub(crate)` and shared with [`attention`] deliberately. The sweep derives
/// the register rows from this view, and the note that explains a row derives
/// its handle from the same one; two copies of this assembly would drift, and a
/// note built from a roster the sweep never saw would carry an `obligation_id`
/// for a row that does not exist — which reads exactly like a room nobody
/// looked at.
///
/// Every read propagates. An access lane that cannot be read must not fold to
/// "no visits": that is the never-opened case, the one signal in the whole set
/// that suggests a message did not arrive.
pub(crate) fn snapshot_room(
    links: &ShareLinkStore,
    link_scope: &ShareLinkScope,
    access: &AccessStore,
    access_scope: &AccessScope,
    room: &DataRoom,
) -> Result<Vec<TokenContext>> {
    let shared_with = shared_tokens(links, link_scope, &room.room_id)?;
    let documents: Vec<String> = room
        .present_documents()
        .into_iter()
        .map(|entry| entry.artifact_ref.clone())
        .collect();
    let events = access
        .events_for(access_scope, &room.room_id)
        .with_context(|| format!("reading the access lane of room `{}`", room.room_id))?;
    snapshot_from_events(&room.room_id, &shared_with, &documents, &events)
        .with_context(|| format!("building the snapshot of room `{}`", room.room_id))
}

/// One room's roster, as the grant layer knows it.
///
/// **One token per identity, at its EARLIEST issue.** A rotated credential is
/// the same person holding the same grant slot — the grant layer says so
/// itself, keeping the presentation count across a rotation — so running the
/// never-opened window from the rotation instant would reset a delivery
/// question every time a link was re-issued, and the token nobody ever opened
/// would never ripen.
///
/// Revoked and expired grants are **kept**. The question a follow-up answers is
/// *"did this ever reach them"*, and a link that lapsed unopened is the purest
/// form of "no". Filtering to live grants would delete exactly the rows worth
/// worrying about.
///
/// `replied` is left at [`SharedToken::new`]'s `false`: replies arrive by mail,
/// by phone, in meetings, and the room has a view of none of them. See the
/// module note on why false is the direction that keeps a signal.
fn shared_tokens(
    links: &ShareLinkStore,
    scope: &ShareLinkScope,
    room_id: &str,
) -> Result<Vec<SharedToken>> {
    let grants = links
        .for_resource(scope, room_id)
        .with_context(|| format!("reading the grants on room `{room_id}`"))?;
    let mut earliest: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for grant in grants {
        let issued_at = grant.issued_at;
        earliest
            .entry(grant.issued_to)
            .and_modify(|held| {
                if issued_at < *held {
                    *held = issued_at;
                }
            })
            .or_insert(issued_at);
    }
    Ok(earliest
        .into_iter()
        .map(|(issued_to, shared_at)| SharedToken::new(issued_to, shared_at))
        .collect())
}
