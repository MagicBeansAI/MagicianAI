//! Closing the follow-up loop — what ripened gets recorded, what stopped
//! applying gets settled, and both halves carry the register's own id.
//!
//! Plan: `docs/plans/2026-08-07-opc-deal-close.md` §6.
//!
//! [`super::follow_ups`] is a pair of pure derivations with no caller. That is
//! not a wiring gap, it is the whole failure: nothing ripens, so nothing is ever
//! recorded, and nothing is ever settled either — and an obligation register
//! that only ever grows is the one people learn to ignore. This module is the
//! caller, and it is deliberately two things rather than one:
//!
//! - [`sweep_follow_ups`] — **pure**. Snapshots in, intent out. It runs the
//!   derivation and the paired diff, and pairs every result with the id the
//!   register will file it under, so the caller never derives an id itself.
//! - [`apply_follow_up_sweep`] — the **only** store-touching function here, thin
//!   enough to read in one sitting and testable on its own against a register.
//!
//! Splitting them is what lets the hard part — which obligations, under which
//! ids — be tested without a filesystem, and what stops the ordering rule below
//! from being buried inside a derivation.
//!
//! # Generic first: a room is one consumer, not the subject
//!
//! Nothing here knows what the documents are for. The primitive is *"a periodic
//! observation of a bounded set of people implies obligations; a later
//! observation retires some of them"* — the same shape for a cohort's materials
//! going unread, a client's deliverable pack, an audit request nobody opened.
//! The deal-close flow is one consumer, and no name in this module's public API
//! says otherwise.
//!
//! # Why the pairing exists
//!
//! Register ids are **derived, never assigned**
//! ([`obligation_id_for`](magician::magician_v2::obligations::store::obligation_id_for)),
//! so an emitter that recorded an obligation holds nothing to settle it with
//! later unless it rebuilds the exact tuple it recorded. Every caller doing that
//! rebuild by hand is a caller that can get it subtly wrong — a decorated text,
//! a nudged due date — and a tuple that does not round-trip to the same id
//! settles nothing at all while reporting success. So the id is computed once,
//! here, next to the tuple it came from, and handed to the caller already
//! attached.
//!
//! # The scope is a parameter, and it has to be
//!
//! A register id is derived over `(principal, workspace, audience, what, due_at,
//! direction)`. The scope is in it so two tenants recording against the same
//! audience do not resume each other's rows — which means an id derived without
//! one is an id no register row will ever match. `sweep_follow_ups` therefore
//! takes the [`ObligationScope`] even though it touches no store: pairing an
//! obligation with an id that cannot address it would be worse than not pairing
//! it at all.
//!
//! # Record, then settle — deliberately, and never the other way
//!
//! [`apply_follow_up_sweep`] records the whole `to_record` list before settling
//! anything. The two lists name **different rows**: an advanced basis mints a
//! new tuple (a later `last_seen` is a later `due_at`) while the superseded row
//! is what gets released. A crash between the halves therefore has two possible
//! shapes, and only one is survivable:
//!
//! - **Record first (this order).** The interruption leaves the superseded row
//!   still live alongside the new one. The owner sees one stale item too many;
//!   the next sweep settles it, because the diff is computed from snapshots and
//!   not from what the last run managed to write.
//! - **Settle first (refused).** The interruption releases the old row with the
//!   new one never written. The follow-up is *gone* — no row, no surface, and no
//!   later sweep re-derives it unless the basis happens to be re-observed. A
//!   silent hole in a register is unrecoverable in the way a duplicate is not.
//!
//! Over-surfacing beats silence, so the order is fixed and not a tuning knob.
//!
//! # Unknown is never permission to settle
//!
//! Settling is a claim that an obligation **stopped applying**. `sweep_follow_ups`
//! refuses a current snapshot that has dropped a token which the previous
//! snapshot had a ripened obligation for: a token absent from the roster is a
//! token we know nothing about this run — a partial read, a roster fetch that
//! failed, a caller passing the wrong list — and "we could not see them" is not
//! evidence that they no longer need chasing. The empty-current case is the same
//! bug at its purest: every previously derived obligation is trivially "not in
//! the current set", so a vacuous membership test would release the entire
//! register in one call. Genuinely retiring a room's obligations is a separate,
//! explicit act with its own reason, never something inferred from a snapshot
//! that came back short.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::store::obligation_id_for;
use magician::magician_v2::obligations::{
    Obligation, ObligationScope, ObligationStore, RecordObligation, Settlement,
};

use super::access_log::{attention_across, AccessEvent};
use super::follow_ups::{derive_follow_ups, follow_ups_to_settle, FollowUpPolicy, TokenContext};

/// The separator that keeps a derived id's components apart.
///
/// Mirrors the access lane's and the register's own constant on purpose:
/// every module that lets caller text reach a derivation has to refuse it, and a
/// shared import would hide the refusal behind somebody else's invariant.
const FIELD_SEP: char = '\u{1f}';

/// Why a superseded follow-up is released.
///
/// [`Settlement::Released`], never [`Settlement::Met`]: a follow-up whose basis
/// moved was **not done** — nobody acted on it, it stopped applying. Recording
/// it as met would report a hit rate that was partly wishful, which is exactly
/// the distinction the two settlement kinds exist to keep.
///
/// A constant rather than a generated sentence because the reason is read by
/// people comparing rows across sweeps; a text that varied per run would make
/// identical retirements look like different events. It is not part of the
/// register's identity tuple, so it can be read as prose without any risk of
/// moving an id.
pub const FOLLOW_UP_RELEASE_REASON: &str =
    "the basis no longer holds: they replied, or a later visit superseded the silence this \
     follow-up was raised on";

/// One token the room was shared with, as the caller knows it.
///
/// The roster is **supplied**, never derived from the access log, for the same
/// reason [`attention_across`] insists on it: a token that never appears in the
/// log is precisely the never-opened case, and reconstructing the roster from
/// events would make the one signal that earns the whole feature invisible by
/// construction.
///
/// Whether they `replied` is knowledge the room does not have — replies arrive by
/// mail, by phone, in meetings — so it arrives here from whoever holds the
/// correspondence. See [`TokenContext::replied`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedToken {
    /// The identity the link was **issued to** — never a claim about who used
    /// it. Links get forwarded; see [`super::access_log`].
    pub token_issued_to: String,
    /// When the room was shared with this token. The never-opened window runs
    /// from here, because a token that never appeared has no event to run from.
    pub shared_at: DateTime<Utc>,
    pub replied: bool,
}

impl SharedToken {
    /// A shared token nobody has answered yet.
    ///
    /// `replied` defaults to `false` because that is the direction that keeps a
    /// signal rather than losing one: at worst it raises a follow-up on somebody
    /// who answered elsewhere, which is noise the caller can correct. Defaulting
    /// the other way would silently suppress every follow-up in the room until a
    /// caller remembered to say otherwise.
    pub fn new(token_issued_to: impl Into<String>, shared_at: DateTime<Utc>) -> Self {
        Self {
            token_issued_to: token_issued_to.into(),
            shared_at,
            replied: false,
        }
    }

    /// Record that they answered — on any channel.
    pub fn with_reply(mut self, replied: bool) -> Self {
        self.replied = replied;
        self
    }
}

/// One obligation the sweep intends, carrying the id the register files it
/// under.
///
/// The id is [`obligation_id_for`] over the same scope and request, so it is the
/// id [`ObligationStore::record`] derives for this request and the handle
/// [`ObligationStore::settle`] needs later — the two cannot drift, because both
/// delegate to the same private derivation.
#[derive(Debug, Clone)]
pub struct IdentifiedObligation {
    pub obligation_id: String,
    pub request: RecordObligation,
}

/// What one sweep intends — nothing written yet.
///
/// The two lists name **different rows**, never the same one twice: a tuple
/// still derivable from the current snapshot is never in `to_settle` (settling
/// it would fight the recording half), and a superseded tuple is never in
/// `to_record`. See the module note on why they are applied in this order.
///
/// An empty sweep means **there is nothing to do on these facts**. It is not a
/// claim that the room is healthy, that everyone is engaged, or that the roster
/// was read successfully — the same vacuous reading the access log refuses.
#[derive(Debug, Clone, Default)]
pub struct FollowUpSweep {
    /// Ripened obligations, soonest-due first then by text — the order the
    /// register itself surfaces.
    pub to_record: Vec<IdentifiedObligation>,
    /// Obligations the previous snapshot implied that the current one does not,
    /// same order.
    pub to_settle: Vec<IdentifiedObligation>,
}

impl FollowUpSweep {
    /// Whether this sweep would write anything.
    ///
    /// "Nothing to do", never "nothing is wrong" — see the type note.
    pub fn is_empty(&self) -> bool {
        self.to_record.is_empty() && self.to_settle.is_empty()
    }
}

/// What a sweep actually did to the register.
///
/// Counts of rows, never rates: *"three recorded, one settled"* is a fact an
/// owner can act on, while *"75% follow-up coverage"* is a number that hides
/// which rows moved and reads as progress whichever way it goes.
#[derive(Debug, Clone)]
pub struct AppliedFollowUpSweep {
    /// The register rows after recording. A row already held is **resumed**, not
    /// duplicated, and a row already settled comes back settled — recording
    /// never resurrects a terminal state.
    pub recorded: Vec<Obligation>,
    /// The register rows after settling, each carrying its
    /// [`Settlement::Released`].
    pub settled: Vec<Obligation>,
    /// Ids the sweep asked to settle that the register does not hold.
    ///
    /// Reported rather than raised. The recording half for that basis never ran
    /// — a first sweep against a snapshot nobody had swept before, a run that
    /// crashed before its records landed — so there is genuinely nothing to
    /// release. Failing here would wedge every future sweep of the room behind a
    /// row that will never exist, and the records in the same call would already
    /// have landed.
    pub absent: Vec<String>,
}

/// Turn a room's stored presentations into the snapshot [`sweep_follow_ups`]
/// takes.
///
/// The bridge between the persistence lane (`AccessStore::events_for`, which
/// hands back one room's events in observed order) and the derivation, which
/// wants one [`TokenContext`] per token the room was shared with. Kept separate
/// from both so a caller holding events from anywhere — a replay, a test, a
/// different lane — can build the same snapshot.
///
/// What it preserves, and why each one is load-bearing:
///
/// - **Visits come from `sequence`, not from the event count.** One sitting that
///   views the index and then opens a document is two events and *one* visit.
///   Counting events would report "came back" — interest — for somebody who came
///   once and clicked twice, and the follow-up window would then run off a
///   fabricated return.
/// - **Ghost tokens survive.** A token in `shared_with` that appears in no event
///   is kept, with zero visits, which is the never-opened case and the delivery
///   question that earns the feature. Building the roster from the events
///   instead would delete exactly the people worth worrying about.
/// - **Unopened documents are relative to what is in the room now.** Passed
///   through to [`attention_across`]: a document withdrawn last week is not
///   something anybody failed to read.
///
/// A duplicated token in `shared_with` contributes once, **first occurrence
/// wins**, matching the derivation's own rule so the snapshot and the sweep
/// cannot disagree about which share instant speaks for a token.
///
/// Events belonging to another room are **refused**, not filtered. `AccessStore`
/// keeps one log per room so a mismatch is a caller error — the wrong room's
/// events fused into this snapshot would answer "what have they seen" with
/// somebody else's history, and quietly dropping them would report a thoroughly
/// read room as never opened.
///
/// Ordering follows [`attention_across`]: never-opened first, then by token.
///
/// An empty `shared_with` yields an empty snapshot — *"nobody holds a link"*,
/// never *"everybody is fine"*.
///
/// Nothing here refuses a separator-bearing token: no id is derived at this
/// layer, and the refusal belongs where the derivation is
/// ([`sweep_follow_ups`]), so there is exactly one place to read it.
pub fn snapshot_from_events(
    room_id: &str,
    shared_with: &[SharedToken],
    room_documents: &[String],
    events: &[AccessEvent],
) -> Result<Vec<TokenContext>> {
    let room_id = room_id.trim();
    if room_id.is_empty() {
        anyhow::bail!(
            "a snapshot must name the room it is of; without one there is no way to check the \
             events belong to it"
        );
    }
    for event in events {
        if event.room_id.trim() != room_id {
            anyhow::bail!(
                "this event happened in room `{}` but is being folded into a snapshot of \
                 `{room_id}`: another room's presentations would answer 'what have they seen' \
                 with somebody else's history",
                event.room_id
            );
        }
    }

    // First occurrence wins, exactly as the derivation dedupes, so the snapshot
    // and the sweep never disagree about which share instant speaks.
    let mut by_token: HashMap<&str, &SharedToken> = HashMap::new();
    let mut roster: Vec<String> = Vec::new();
    for shared in shared_with {
        // Checked rather than inserted-and-restored, because `HashMap::insert`
        // overwrites: a later duplicate would silently become the share instant
        // that speaks for the token, and the delivery-question window would run
        // from the wrong moment.
        if by_token.contains_key(shared.token_issued_to.as_str()) {
            continue;
        }
        by_token.insert(shared.token_issued_to.as_str(), shared);
        roster.push(shared.token_issued_to.clone());
    }

    let mut out = Vec::with_capacity(roster.len());
    for attention in attention_across(&roster, room_documents, events) {
        let Some(shared) = by_token.get(attention.token_issued_to.as_str()) else {
            // Unreachable: the roster is built from `by_token`'s own keys.
            continue;
        };
        let shared_at = shared.shared_at;
        let replied = shared.replied;
        out.push(TokenContext {
            attention,
            shared_at,
            replied,
        });
    }
    Ok(out)
}

/// What one sweep of a room implies: what to record, what to settle, each with
/// its register id.
///
/// Pure — snapshots in, intent out, no clock of its own and no store. `now` is
/// the sweep's instant; every ripening decision is made against it, and nothing
/// derived from it is stored (a `due_at` is the instant an item **ripened**, and
/// an obligation's lapsed/open state is derived from the clock on every read,
/// never written down).
///
/// `previous` is the snapshot the last sweep saw and `current` is the snapshot
/// now. Passing the same slice for both is the honest way to express *"first
/// sweep"* or *"nothing has changed"*: the recording half is idempotent — the
/// same tuple derives the same id and the register resumes rather than
/// duplicates — and the settling half correctly finds nothing.
///
/// Refusals, all of them fail-closed:
///
/// - Any string that feeds a derived id and contains `U+001F` — the scope's
///   principal or workspace, the audience id, the room id, or any token in
///   either snapshot. The separator is what keeps a derived id's components
///   apart, and a component carrying it could fuse two rooms', tenants' or
///   readers' obligations into one id, silently settling one by acting on
///   another.
/// - An unnamed audience or a blank room id. The register refuses an unnamed
///   audience at the write, so pairing one here would hand back settle handles
///   for rows that can never exist.
/// - A current snapshot that has **dropped** a token the previous snapshot had a
///   ripened obligation for. See the module note: unknown is never permission to
///   settle, and an empty current snapshot is that bug at its purest.
///
/// The room's **stable id** goes in, never its display label: the id reaches the
/// obligation text, which is part of the register's identity tuple, so a rename
/// would otherwise mint a duplicate of a row that could never be collapsed
/// again.
pub fn sweep_follow_ups(
    scope: &ObligationScope,
    audience: &AudienceRef,
    room_id: &str,
    previous: &[TokenContext],
    current: &[TokenContext],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Result<FollowUpSweep> {
    let room_id = room_id.trim();
    validate(scope, audience, room_id, previous, current)?;
    refuse_dropped_ripened_tokens(audience, room_id, previous, current, policy, now)?;

    let to_record = derive_follow_ups(audience, room_id, current, policy, now)
        .into_iter()
        .map(|request| pair(scope, request))
        .collect();
    let to_settle = follow_ups_to_settle(audience, room_id, previous, current, policy, now)
        .into_iter()
        .map(|request| pair(scope, request))
        .collect();

    Ok(FollowUpSweep {
        to_record,
        to_settle,
    })
}

/// Write a sweep to the register: **record everything, then settle everything**.
///
/// The thin half. It makes no decisions — which obligations, under which ids,
/// and with which due dates was all settled by [`sweep_follow_ups`] — so the
/// only thing this function can get wrong is the order, and the order is the
/// module note's: a crash between the halves must leave a live obligation, never
/// a settled-but-unrecorded one.
///
/// Both halves are idempotent at the register, so re-applying a sweep after a
/// crash converges rather than doubling:
/// [`record`](ObligationStore::record) resumes a row it already holds and
/// [`settle`](ObligationStore::settle) keeps the first settlement, because
/// *"we released it on Tuesday"* is a fact a second call must not move.
///
/// A settle for a row the register does not hold is reported in
/// [`AppliedFollowUpSweep::absent`], not raised — see that field.
///
/// A recorded row whose id does not match the paired one is fatal. It cannot
/// happen while both derivations agree, and if they ever stop agreeing then
/// every obligation this sweep records becomes permanently unsettleable — a
/// register that grows forever, which is the failure the whole pairing exists to
/// prevent. Better to fail loudly on the first row than to leak quietly.
pub fn apply_follow_up_sweep(
    store: &ObligationStore,
    scope: &ObligationScope,
    sweep: &FollowUpSweep,
    now: DateTime<Utc>,
) -> Result<AppliedFollowUpSweep> {
    // ── Record first. See the module note: an interruption here leaves the
    // superseded row live next to the new one, which the next sweep retires.
    let mut recorded = Vec::with_capacity(sweep.to_record.len());
    for item in &sweep.to_record {
        let obligation = store
            .record(scope, &item.request, now)
            .with_context(|| format!("recording follow-up `{}`", item.obligation_id))?;
        if obligation.obligation_id != item.obligation_id {
            anyhow::bail!(
                "the register filed this follow-up as `{}` but the sweep paired it with `{}`: \
                 the paired id is the only handle a later sweep has to retire this row, so a \
                 pairing that does not round-trip would leave every obligation raised here \
                 unsettleable forever",
                obligation.obligation_id,
                item.obligation_id
            );
        }
        recorded.push(obligation);
    }

    // ── Then settle. Never before: a crash between the halves would release the
    // superseded row with its replacement never written, and a follow-up that
    // vanished silently is unrecoverable in the way a duplicate is not.
    let mut settled = Vec::with_capacity(sweep.to_settle.len());
    let mut absent = Vec::new();
    for item in &sweep.to_settle {
        // Checked before settling rather than mapping the store's "no such
        // obligation" error, because that error is also what a genuine fault
        // would look like and the two must not be confused.
        let held = store
            .load(scope, &item.request.audience, &item.obligation_id)
            .with_context(|| format!("loading follow-up `{}` to settle", item.obligation_id))?;
        if held.is_none() {
            absent.push(item.obligation_id.clone());
            continue;
        }
        let obligation = store
            .settle(
                scope,
                &item.request.audience,
                &item.obligation_id,
                Settlement::Released {
                    reason: FOLLOW_UP_RELEASE_REASON.to_string(),
                },
                now,
            )
            .with_context(|| format!("settling follow-up `{}`", item.obligation_id))?;
        settled.push(obligation);
    }

    Ok(AppliedFollowUpSweep {
        recorded,
        settled,
        absent,
    })
}

// ── Internals ───────────────────────────────────────────────────────────────

fn pair(scope: &ObligationScope, request: RecordObligation) -> IdentifiedObligation {
    IdentifiedObligation {
        obligation_id: obligation_id_for(scope, &request),
        request,
    }
}

/// Everything that would put a wrong or unaddressable id into the register.
fn validate(
    scope: &ObligationScope,
    audience: &AudienceRef,
    room_id: &str,
    previous: &[TokenContext],
    current: &[TokenContext],
) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator that \
             keeps a derived obligation id's components from bleeding into each other, and a \
             scope carrying it could address one tenant's register row from another's sweep"
        );
    }
    if !audience.is_named() {
        anyhow::bail!(
            "a follow-up must name the relationship it belongs to; the register refuses an \
             unnamed audience at the write, so pairing one here would hand back settle handles \
             for rows that can never exist"
        );
    }
    if audience.id.contains(FIELD_SEP) {
        anyhow::bail!(
            "an audience id must not contain U+001F: it is the separator that keeps a derived \
             obligation id's components from bleeding into each other, and an audience carrying \
             it could fuse two relationships' registers into one id"
        );
    }
    if room_id.is_empty() {
        anyhow::bail!(
            "a sweep must name the room it is of; the room's stable id is what makes one room's \
             follow-ups distinguishable from another's in the register"
        );
    }
    if room_id.contains(FIELD_SEP) {
        anyhow::bail!(
            "a room id must not contain U+001F: it reaches the obligation text, which is part of \
             the register's identity tuple, and a room id carrying it could fuse one room's \
             follow-up with another's"
        );
    }
    for token in previous.iter().chain(current) {
        if token.attention.token_issued_to.contains(FIELD_SEP) {
            anyhow::bail!(
                "a token must not contain U+001F: it reaches the obligation text, which is part \
                 of the register's identity tuple, and a token carrying it could fuse two \
                 readers' follow-ups into one row — settling one by acting on the other"
            );
        }
    }
    Ok(())
}

/// Refuse a current snapshot that has lost a token the previous snapshot was
/// raising an obligation for.
///
/// The precise form of the check matters. A blanket *"every previous token must
/// reappear"* would refuse the harmless case — a token that never ripened
/// dropping off a roster costs nothing — and a rule that fires on harmless input
/// is a rule callers route around. So only a token that **contributed a ripened
/// obligation** makes its own disappearance fatal, and that contribution is
/// established by running the real derivation over that token alone rather than
/// by re-implementing the ripening rule here: a second copy of that rule is the
/// thing that would drift.
fn refuse_dropped_ripened_tokens(
    audience: &AudienceRef,
    room_id: &str,
    previous: &[TokenContext],
    current: &[TokenContext],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Result<()> {
    let present: HashSet<&str> = current
        .iter()
        .map(|token| token.attention.token_issued_to.as_str())
        .collect();

    let mut seen: HashSet<&str> = HashSet::new();
    for token in previous {
        let name = token.attention.token_issued_to.as_str();
        // First occurrence wins, exactly as the derivation dedupes.
        if !seen.insert(name) {
            continue;
        }
        if present.contains(name) {
            continue;
        }
        let ripened =
            !derive_follow_ups(audience, room_id, std::slice::from_ref(token), policy, now)
                .is_empty();
        if ripened {
            anyhow::bail!(
                "token `{name}` had a ripened follow-up in the previous snapshot of room \
                 `{room_id}` and is missing from the current one: settling is a claim that an \
                 obligation stopped applying, and a token we could not see this run is unknown, \
                 not resolved. Retiring a room's follow-ups is an explicit act with its own \
                 reason, never something inferred from a snapshot that came back short"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The loop, as behaviour: derive, pair, record, settle — and the refusals
    //! that keep a register from either flooding or emptying itself.

    use chrono::{DateTime, Duration, TimeZone, Utc};

    use crate::data_room::access_log::{
        AccessEvent, AttentionSignal, TokenAttention, UserAgentClass,
    };
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::{AudienceKind, AudienceRef};
    use magician::magician_v2::obligations::{
        ObligationDirection, ObligationScope, ObligationState, ObligationStore, Settlement,
    };

    use super::{
        apply_follow_up_sweep, snapshot_from_events, sweep_follow_ups, FollowUpPolicy, SharedToken,
        TokenContext, FOLLOW_UP_RELEASE_REASON,
    };

    const ROOM: &str = "room-7c2f";

    fn shared_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap()
    }

    fn policy() -> FollowUpPolicy {
        FollowUpPolicy::new(Duration::days(3), Duration::days(5)).expect("positive windows")
    }

    fn audience() -> AudienceRef {
        AudienceRef::engagement("eng-1")
    }

    fn register() -> (tempfile::TempDir, ObligationStore, ObligationScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = ObligationStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, ObligationScope::new("anonymous", "default"))
    }

    fn documents() -> Vec<String> {
        vec!["deck".to_string(), "financials".to_string()]
    }

    fn event(
        token: &str,
        sequence: u32,
        document: Option<&str>,
        occurred_at: DateTime<Utc>,
    ) -> AccessEvent {
        AccessEvent {
            room_id: ROOM.to_string(),
            audience: audience(),
            token_issued_to: token.to_string(),
            document_ref: document.map(str::to_string),
            occurred_at,
            dwell_ms: None,
            sequence,
            user_agent_class: UserAgentClass::Desktop,
        }
    }

    /// A context built directly, for the cases a snapshot cannot express — a
    /// token carrying the id separator, for instance, which the builder is
    /// deliberately permissive about.
    fn context(
        token: &str,
        visits: u32,
        shared: DateTime<Utc>,
        last_seen: Option<DateTime<Utc>>,
        replied: bool,
    ) -> TokenContext {
        TokenContext {
            attention: TokenAttention {
                token_issued_to: token.to_string(),
                visits,
                presentations: visits as usize,
                first_seen: last_seen,
                last_seen,
                documents_opened: Vec::new(),
                documents_unopened: Vec::new(),
                index_views: 0,
            },
            shared_at: shared,
            replied,
        }
    }

    /// The whole loop, end to end, on values — this is the failure the module
    /// exists for: the deriver had no caller, so nothing ripened into the
    /// register and nothing was ever retired from it.
    ///
    /// Shared, silent, ripened, recorded; then they come back, and the SAME
    /// register id is released while a fresh row takes its place. The id
    /// equality is the assertion that matters: a paired tuple that did not
    /// round-trip to the id it was recorded under would settle nothing at all
    /// while reporting success, which is precisely the bug the pairing prevents.
    #[test]
    fn the_full_cycle_records_a_ripened_follow_up_and_settles_that_same_id_on_a_visit() {
        let (_tmp, store, scope) = register();
        let roster = vec![SharedToken::new("alice", shared_at())];

        // ── Shared, nothing observed, inside the window: nothing to do.
        let quiet = snapshot_from_events(ROOM, &roster, &documents(), &[]).expect("snapshot");
        let early = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &quiet,
            &quiet,
            &policy(),
            shared_at() + Duration::days(1),
        )
        .expect("sweep");
        assert_eq!(early.to_record.len(), 0);
        assert_eq!(early.to_settle.len(), 0);

        // ── They open it once, then go silent.
        let first_visit = shared_at() + Duration::days(1);
        let events_v1 = vec![event("alice", 1, None, first_visit)];
        let snapshot_v1 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v1).expect("snapshot");
        assert_eq!(snapshot_v1.len(), 1);
        assert_eq!(snapshot_v1[0].attention.visits, 1);
        assert_eq!(
            snapshot_v1[0].attention.signal(),
            AttentionSignal::OpenedOnce
        );
        assert_eq!(snapshot_v1[0].attention.last_seen, Some(first_visit));

        // ── The silence ripens, exactly at the window (inclusive).
        let ripened_at = first_visit + Duration::days(5);
        let sweep_one = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot_v1,
            &snapshot_v1,
            &policy(),
            ripened_at,
        )
        .expect("sweep");
        assert_eq!(sweep_one.to_settle.len(), 0);
        assert_eq!(sweep_one.to_record.len(), 1);
        assert_eq!(
            sweep_one.to_record[0].request.what,
            "follow up with alice on data room 'room-7c2f': opened, no reply"
        );
        assert_eq!(sweep_one.to_record[0].request.due_at, ripened_at);
        assert_eq!(
            sweep_one.to_record[0].request.direction,
            ObligationDirection::OwedByUs
        );
        let follow_up_id = sweep_one.to_record[0].obligation_id.clone();

        let applied_one =
            apply_follow_up_sweep(&store, &scope, &sweep_one, ripened_at).expect("apply");
        assert_eq!(applied_one.recorded.len(), 1);
        assert_eq!(applied_one.settled.len(), 0);
        assert_eq!(applied_one.absent.len(), 0);
        assert_eq!(
            applied_one.recorded[0].obligation_id, follow_up_id,
            "the register's own id must be the id the sweep paired"
        );
        // Derived from the clock, and the deadline is inclusive: due at `now` is
        // already lapsed, never stored anywhere.
        assert_eq!(
            applied_one.recorded[0].state(ripened_at),
            ObligationState::Lapsed
        );
        assert_eq!(applied_one.recorded[0].settlement, None);

        // ── They come back. The basis advances, so the old row must go.
        let second_visit = shared_at() + Duration::days(8);
        let mut events_v2 = events_v1.clone();
        events_v2.push(event("alice", 2, Some("deck"), second_visit));
        let snapshot_v2 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v2).expect("snapshot");
        assert_eq!(snapshot_v2[0].attention.visits, 2);
        assert_eq!(
            snapshot_v2[0].attention.signal(),
            AttentionSignal::OpenedRepeatedly
        );

        let swept_at = shared_at() + Duration::days(14);
        let sweep_two = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot_v1,
            &snapshot_v2,
            &policy(),
            swept_at,
        )
        .expect("sweep");

        assert_eq!(sweep_two.to_settle.len(), 1);
        assert_eq!(
            sweep_two.to_settle[0].obligation_id, follow_up_id,
            "the paired id must round-trip: the tuple recorded is the tuple settled"
        );
        assert_eq!(sweep_two.to_settle[0].request.due_at, ripened_at);
        assert_eq!(sweep_two.to_record.len(), 1);
        assert_eq!(
            sweep_two.to_record[0].request.due_at,
            second_visit + Duration::days(5),
            "the NEW basis is what gets recorded"
        );
        let superseding_id = sweep_two.to_record[0].obligation_id.clone();
        assert_ne!(
            superseding_id, follow_up_id,
            "a later due date is a different register row"
        );

        let applied_two =
            apply_follow_up_sweep(&store, &scope, &sweep_two, swept_at).expect("apply");
        assert_eq!(applied_two.recorded.len(), 1);
        assert_eq!(applied_two.recorded[0].obligation_id, superseding_id);
        assert_eq!(applied_two.settled.len(), 1);
        assert_eq!(applied_two.absent.len(), 0);
        assert_eq!(applied_two.settled[0].obligation_id, follow_up_id);
        assert_eq!(
            applied_two.settled[0].settlement,
            Some(Settlement::Released {
                reason: FOLLOW_UP_RELEASE_REASON.to_string()
            }),
            "superseded, not done — Released never Met"
        );
        assert_eq!(applied_two.settled[0].settled_at, Some(swept_at));
        assert_eq!(
            applied_two.settled[0].state(swept_at),
            ObligationState::Released
        );

        // ── The register swapped rows rather than accumulating them.
        let outstanding = store
            .outstanding(&scope, &audience(), swept_at)
            .expect("outstanding");
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].obligation_id, superseding_id);
        assert_eq!(
            store
                .for_audience(&scope, &audience())
                .expect("register")
                .len(),
            2,
            "two rows ever written: the superseded one and its replacement"
        );
    }

    /// Nothing ripened is an EMPTY sweep in both directions — not a delivery
    /// question raised early, and not a settlement invented for a row the
    /// recording half never produced. A sweep that wrote on every run is the
    /// register-flooding failure that makes people ignore a to-do list.
    #[test]
    fn nothing_ripened_yields_an_empty_sweep_in_both_directions() {
        let (_tmp, store, scope) = register();
        let roster = vec![SharedToken::new("alice", shared_at())];
        let snapshot = snapshot_from_events(ROOM, &roster, &documents(), &[]).expect("snapshot");

        let inside_window = shared_at() + Duration::days(2);
        let sweep = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            inside_window,
        )
        .expect("sweep");
        assert_eq!(sweep.to_record.len(), 0);
        assert_eq!(sweep.to_settle.len(), 0);
        assert!(sweep.is_empty());

        let applied = apply_follow_up_sweep(&store, &scope, &sweep, inside_window).expect("apply");
        assert_eq!(applied.recorded.len(), 0);
        assert_eq!(applied.settled.len(), 0);
        assert_eq!(applied.absent.len(), 0);
        assert_eq!(
            store
                .for_audience(&scope, &audience())
                .expect("register")
                .len(),
            0
        );
    }

    /// An unchanged snapshot settles NOTHING and re-records the same id, so
    /// repeated sweeps of a room that has not moved leave exactly one row. A
    /// settle here would fight the recording half (the tuple is still
    /// derivable); a second row would be the per-sweep duplicate the stable
    /// `due_at` exists to prevent.
    #[test]
    fn an_unchanged_ripened_snapshot_settles_nothing_and_records_one_row_however_often_swept() {
        let (_tmp, store, scope) = register();
        let roster = vec![SharedToken::new("alice", shared_at())];
        let visit = shared_at() + Duration::days(1);
        let snapshot = snapshot_from_events(
            ROOM,
            &roster,
            &documents(),
            &[event("alice", 1, None, visit)],
        )
        .expect("snapshot");

        let first_at = visit + Duration::days(5);
        let first = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            first_at,
        )
        .expect("sweep");
        let applied_first = apply_follow_up_sweep(&store, &scope, &first, first_at).expect("apply");

        // A second sweep, hours later, over an unchanged world.
        let second_at = first_at + Duration::hours(6);
        let second = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            second_at,
        )
        .expect("sweep");
        assert_eq!(
            second.to_settle.len(),
            0,
            "still derivable, so never settled"
        );
        assert_eq!(second.to_record.len(), 1);
        assert_eq!(
            second.to_record[0].obligation_id, first.to_record[0].obligation_id,
            "the same basis is the same id, whatever the sweep clock says"
        );

        let applied_second =
            apply_follow_up_sweep(&store, &scope, &second, second_at).expect("apply");
        assert_eq!(
            applied_second.recorded[0].created_at, applied_first.recorded[0].created_at,
            "the second record resumed the first row rather than replacing it"
        );
        assert_eq!(
            store
                .for_audience(&scope, &audience())
                .expect("register")
                .len(),
            1,
            "two sweeps, one obligation"
        );
    }

    /// Pins the ordering the module fixes: record BEFORE settle. Applying only
    /// the recording half — the shape a crash between the two leaves — keeps the
    /// superseded obligation live next to its replacement, so the owner sees one
    /// stale item and the next sweep retires it. Applying only the settling half
    /// is the state the ordering refuses to produce: the old row released with
    /// its replacement never written, and the follow-up gone with nothing left
    /// to re-derive it from.
    #[test]
    fn a_crash_between_the_halves_leaves_a_live_obligation_never_a_silent_gap() {
        let roster = vec![SharedToken::new("alice", shared_at())];
        let first_visit = shared_at() + Duration::days(1);
        let second_visit = shared_at() + Duration::days(8);
        let events_v1 = vec![event("alice", 1, None, first_visit)];
        let mut events_v2 = events_v1.clone();
        events_v2.push(event("alice", 2, None, second_visit));
        let snapshot_v1 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v1).expect("snapshot");
        let snapshot_v2 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v2).expect("snapshot");
        let swept_at = shared_at() + Duration::days(14);

        // ── Arm one: interrupted after recording. Both rows live.
        let (_tmp_a, store_a, scope_a) = register();
        let seed = sweep_follow_ups(
            &scope_a,
            &audience(),
            ROOM,
            &snapshot_v1,
            &snapshot_v1,
            &policy(),
            first_visit + Duration::days(5),
        )
        .expect("sweep");
        apply_follow_up_sweep(&store_a, &scope_a, &seed, first_visit + Duration::days(5))
            .expect("apply");
        let old_id = seed.to_record[0].obligation_id.clone();

        let full = sweep_follow_ups(
            &scope_a,
            &audience(),
            ROOM,
            &snapshot_v1,
            &snapshot_v2,
            &policy(),
            swept_at,
        )
        .expect("sweep");
        let new_id = full.to_record[0].obligation_id.clone();
        let mut record_half = full.clone();
        record_half.to_settle.clear();
        apply_follow_up_sweep(&store_a, &scope_a, &record_half, swept_at).expect("apply");

        let live: Vec<String> = store_a
            .outstanding(&scope_a, &audience(), swept_at)
            .expect("outstanding")
            .into_iter()
            .map(|held| held.obligation_id)
            .collect();
        assert_eq!(live.len(), 2, "one stale item too many — recoverable");
        assert!(live.contains(&old_id));
        assert!(live.contains(&new_id));

        // The next sweep retires the stale one: the diff is computed from
        // snapshots, never from what the interrupted run managed to write.
        let resumed =
            apply_follow_up_sweep(&store_a, &scope_a, &full, swept_at).expect("resumed apply");
        assert_eq!(resumed.settled.len(), 1);
        assert_eq!(resumed.settled[0].obligation_id, old_id);
        let live_after: Vec<String> = store_a
            .outstanding(&scope_a, &audience(), swept_at)
            .expect("outstanding")
            .into_iter()
            .map(|held| held.obligation_id)
            .collect();
        assert_eq!(live_after, vec![new_id.clone()]);

        // ── Arm two: what settling first would have left — nothing at all.
        let (_tmp_b, store_b, scope_b) = register();
        apply_follow_up_sweep(&store_b, &scope_b, &seed, first_visit + Duration::days(5))
            .expect("apply");
        let mut settle_half = full.clone();
        settle_half.to_record.clear();
        apply_follow_up_sweep(&store_b, &scope_b, &settle_half, swept_at).expect("apply");
        assert_eq!(
            store_b
                .outstanding(&scope_b, &audience(), swept_at)
                .expect("outstanding")
                .len(),
            0,
            "the follow-up is gone: the old row released, the new one never written"
        );
    }

    /// A settle for a row the register never held is REPORTED, not raised.
    /// Raising would wedge every future sweep of the room behind a row that
    /// cannot exist — and the records in the same call have already landed, so
    /// the failure would also be half-applied.
    #[test]
    fn settling_a_row_the_register_never_held_is_reported_not_an_error() {
        let (_tmp, store, scope) = register();
        let roster = vec![SharedToken::new("alice", shared_at())];
        let first_visit = shared_at() + Duration::days(1);
        let second_visit = shared_at() + Duration::days(8);
        let events_v1 = vec![event("alice", 1, None, first_visit)];
        let mut events_v2 = events_v1.clone();
        events_v2.push(event("alice", 2, None, second_visit));
        let snapshot_v1 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v1).expect("snapshot");
        let snapshot_v2 =
            snapshot_from_events(ROOM, &roster, &documents(), &events_v2).expect("snapshot");

        // The first sweep was never applied, so the superseded row was never
        // recorded — but the diff still names it.
        let swept_at = shared_at() + Duration::days(14);
        let sweep = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot_v1,
            &snapshot_v2,
            &policy(),
            swept_at,
        )
        .expect("sweep");
        let missing_id = sweep.to_settle[0].obligation_id.clone();

        let applied = apply_follow_up_sweep(&store, &scope, &sweep, swept_at).expect("apply");
        assert_eq!(applied.settled.len(), 0);
        assert_eq!(applied.absent, vec![missing_id]);
        assert_eq!(applied.recorded.len(), 1, "the recording half still ran");
        assert_eq!(
            store
                .for_audience(&scope, &audience())
                .expect("register")
                .len(),
            1
        );
    }

    /// Unknown is never permission to settle. A token that had a ripened
    /// obligation and is missing from the current snapshot is refused: "not in
    /// the current set" is trivially true of every row when the current set is
    /// short or empty, and a vacuous membership test there would release the
    /// whole register in one call.
    #[test]
    fn a_dropped_token_with_a_ripened_follow_up_is_refused() {
        let (_tmp, _store, scope) = register();
        let last_seen = shared_at() + Duration::days(1);
        let now = last_seen + Duration::days(6);
        let previous = vec![
            context("alice", 1, shared_at(), Some(last_seen), false),
            context("bob", 1, shared_at(), Some(last_seen), false),
        ];

        // The empty current snapshot — the bug at its purest.
        let error = sweep_follow_ups(&scope, &audience(), ROOM, &previous, &[], &policy(), now)
            .expect_err("an empty current snapshot must not release the register");
        assert!(
            format!("{error:#}").contains("unknown, not resolved"),
            "the error must say why: {error:#}"
        );

        // And a partial one: bob survived, alice vanished.
        let partial = vec![context("bob", 1, shared_at(), Some(last_seen), false)];
        let error = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &previous,
            &partial,
            &policy(),
            now,
        )
        .expect_err("a dropped ripened token must be refused");
        assert!(
            format!("{error:#}").contains("`alice`"),
            "the error must name the token: {error:#}"
        );
    }

    /// The refusal is precise, not blanket: a token that never ripened costs
    /// nothing when it drops off a roster, and refusing that harmless case is
    /// how a guard becomes something callers route around.
    #[test]
    fn a_dropped_token_that_never_ripened_is_not_refused() {
        let (_tmp, _store, scope) = register();
        // Shared yesterday, never opened: inside the three-day window.
        let previous = vec![context("alice", 0, shared_at(), None, false)];
        let now = shared_at() + Duration::days(1);

        let sweep = sweep_follow_ups(&scope, &audience(), ROOM, &previous, &[], &policy(), now)
            .expect("an unripened token may drop off");
        assert_eq!(sweep.to_record.len(), 0);
        assert_eq!(sweep.to_settle.len(), 0);
    }

    /// Every caller string that reaches a derived id is refused if it carries
    /// U+001F. The separator is what keeps the id's components apart, and one
    /// bleeding into another could address a different tenant's, relationship's,
    /// room's or reader's register row — settling one obligation by acting on
    /// another.
    #[test]
    fn a_separator_in_any_id_bearing_string_is_refused() {
        let (_tmp, _store, scope) = register();
        let last_seen = shared_at() + Duration::days(1);
        let now = last_seen + Duration::days(6);
        let clean = vec![context("alice", 1, shared_at(), Some(last_seen), false)];

        let fused_principal = ObligationScope::new("anon\u{1f}ymous", "default");
        let error = sweep_follow_ups(
            &fused_principal,
            &audience(),
            ROOM,
            &clean,
            &clean,
            &policy(),
            now,
        )
        .expect_err("a fused principal is refused");
        assert!(
            format!("{error:#}").contains("principal and workspace must not contain U+001F"),
            "{error:#}"
        );

        let fused_workspace = ObligationScope::new("anonymous", "def\u{1f}ault");
        let error = sweep_follow_ups(
            &fused_workspace,
            &audience(),
            ROOM,
            &clean,
            &clean,
            &policy(),
            now,
        )
        .expect_err("a fused workspace is refused");
        assert!(
            format!("{error:#}").contains("principal and workspace must not contain U+001F"),
            "{error:#}"
        );

        let fused_audience = AudienceRef::engagement("eng\u{1f}1");
        let error = sweep_follow_ups(
            &scope,
            &fused_audience,
            ROOM,
            &clean,
            &clean,
            &policy(),
            now,
        )
        .expect_err("a fused audience is refused");
        assert!(
            format!("{error:#}").contains("an audience id must not contain U+001F"),
            "{error:#}"
        );

        let error = sweep_follow_ups(
            &scope,
            &audience(),
            "room\u{1f}7c2f",
            &clean,
            &clean,
            &policy(),
            now,
        )
        .expect_err("a fused room id is refused");
        assert!(
            format!("{error:#}").contains("a room id must not contain U+001F"),
            "{error:#}"
        );

        // In either snapshot — the previous one feeds the settle handles, so a
        // fused token there would settle one reader's row by acting on another.
        let fused_token = vec![context(
            "ali\u{1f}ce",
            1,
            shared_at(),
            Some(last_seen),
            false,
        )];
        let error = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &clean,
            &fused_token,
            &policy(),
            now,
        )
        .expect_err("a fused token in the current snapshot is refused");
        assert!(
            format!("{error:#}").contains("a token must not contain U+001F"),
            "{error:#}"
        );
        let error = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &fused_token,
            &clean,
            &policy(),
            now,
        )
        .expect_err("a fused token in the previous snapshot is refused");
        assert!(
            format!("{error:#}").contains("a token must not contain U+001F"),
            "{error:#}"
        );
    }

    /// An unnamed audience and a blank room id are refused. The register
    /// refuses an unnamed audience at the write, so pairing one would hand back
    /// a settle handle for a row that can never exist; a blank room id would
    /// make one room's follow-ups indistinguishable from another's in the text
    /// that carries register identity.
    #[test]
    fn an_unnamed_audience_or_a_blank_room_id_is_refused() {
        let (_tmp, _store, scope) = register();
        let last_seen = shared_at() + Duration::days(1);
        let now = last_seen + Duration::days(6);
        let snapshot = vec![context("alice", 1, shared_at(), Some(last_seen), false)];

        let unnamed = AudienceRef::new(AudienceKind::Engagement, "   ");
        let error = sweep_follow_ups(&scope, &unnamed, ROOM, &snapshot, &snapshot, &policy(), now)
            .expect_err("an unnamed audience is refused");
        assert!(
            format!("{error:#}").contains("name the relationship"),
            "the error must say why: {error:#}"
        );

        let error = sweep_follow_ups(
            &scope,
            &audience(),
            "   ",
            &snapshot,
            &snapshot,
            &policy(),
            now,
        )
        .expect_err("a blank room id is refused");
        assert!(
            format!("{error:#}").contains("name the room"),
            "the error must say why: {error:#}"
        );
    }

    /// A settled follow-up never resurrects. A later sweep re-derives the same
    /// tuple — the basis it was raised on has not moved — and recording it must
    /// resume the settled row rather than open a fresh one: an obligation that
    /// came back to life after being retired would be a register nobody could
    /// trust as a record of what happened.
    #[test]
    fn a_settled_follow_up_never_resurrects_when_the_sweep_re_derives_it() {
        let (_tmp, store, scope) = register();
        let roster = vec![SharedToken::new("alice", shared_at())];
        let visit = shared_at() + Duration::days(1);
        let snapshot = snapshot_from_events(
            ROOM,
            &roster,
            &documents(),
            &[event("alice", 1, None, visit)],
        )
        .expect("snapshot");
        let ripened_at = visit + Duration::days(5);

        let sweep = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            ripened_at,
        )
        .expect("sweep");
        let id = sweep.to_record[0].obligation_id.clone();
        apply_follow_up_sweep(&store, &scope, &sweep, ripened_at).expect("apply");

        // Somebody released it out of band — the room was closed, say.
        store
            .settle(
                &scope,
                &audience(),
                &id,
                Settlement::Released {
                    reason: "the room was closed".to_string(),
                },
                ripened_at + Duration::hours(1),
            )
            .expect("settle");

        // The next sweep still derives the same tuple, because the basis has
        // not moved. Recording it must not reopen it.
        let later = ripened_at + Duration::days(2);
        let again = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            later,
        )
        .expect("sweep");
        assert_eq!(again.to_record[0].obligation_id, id);

        let applied = apply_follow_up_sweep(&store, &scope, &again, later).expect("apply");
        assert_eq!(applied.recorded.len(), 1);
        assert_eq!(
            applied.recorded[0].settlement,
            Some(Settlement::Released {
                reason: "the room was closed".to_string()
            }),
            "the terminal state survives a re-derivation"
        );
        assert_eq!(applied.recorded[0].state(later), ObligationState::Released);
        assert_eq!(
            store
                .outstanding(&scope, &audience(), later)
                .expect("outstanding")
                .len(),
            0
        );
    }

    /// Visits come from SEQUENCE, never the event count. One sitting that views
    /// the index and then opens a document is two events and one visit —
    /// counting events would report "came back" for somebody who came once and
    /// clicked twice, and the follow-up window would then run off a return that
    /// never happened.
    #[test]
    fn the_snapshot_counts_visits_from_sequence_not_from_events() {
        let visit = shared_at() + Duration::days(1);
        let roster = vec![SharedToken::new("alice", shared_at())];
        let events = vec![
            event("alice", 1, None, visit),
            event("alice", 1, Some("deck"), visit + Duration::seconds(20)),
        ];

        let snapshot =
            snapshot_from_events(ROOM, &roster, &documents(), &events).expect("snapshot");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].attention.visits, 1);
        assert_eq!(snapshot[0].attention.presentations, 2);
        assert_eq!(snapshot[0].attention.index_views, 1);
        assert_eq!(snapshot[0].attention.signal(), AttentionSignal::OpenedOnce);
        assert_eq!(
            snapshot[0].attention.last_seen,
            Some(visit + Duration::seconds(20))
        );
        assert_eq!(snapshot[0].attention.documents_opened, vec!["deck"]);
        assert_eq!(snapshot[0].attention.documents_unopened, vec!["financials"]);
    }

    /// A ghost token — shared with, never appeared — survives the snapshot as
    /// never-opened and ripens into a DELIVERY question, our own task. Building
    /// the roster from the events instead would delete exactly the people worth
    /// worrying about, which is the signal that earns the whole feature.
    #[test]
    fn a_ghost_token_survives_the_snapshot_and_ripens_into_a_delivery_question() {
        let (_tmp, _store, scope) = register();
        let roster = vec![
            SharedToken::new("alice", shared_at()),
            SharedToken::new("ghost", shared_at()),
        ];
        let visit = shared_at() + Duration::days(1);
        let events = vec![event("alice", 1, None, visit)];

        let snapshot =
            snapshot_from_events(ROOM, &roster, &documents(), &events).expect("snapshot");
        assert_eq!(snapshot.len(), 2);
        let ghost = snapshot
            .iter()
            .find(|held| held.attention.token_issued_to == "ghost")
            .expect("the ghost is still in the snapshot");
        assert_eq!(ghost.attention.visits, 0);
        assert_eq!(ghost.attention.presentations, 0);
        assert_eq!(ghost.attention.last_seen, None);
        assert_eq!(ghost.attention.signal(), AttentionSignal::NeverOpened);
        assert!(ghost.attention.signal().is_delivery_question());

        let now = shared_at() + Duration::days(3);
        let sweep = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            now,
        )
        .expect("sweep");
        assert_eq!(sweep.to_record.len(), 1);
        assert_eq!(
            sweep.to_record[0].request.what,
            "verify delivery of data room 'room-7c2f' to ghost: never opened; confirm the link \
             arrived or try another channel"
        );
        assert_eq!(sweep.to_record[0].request.due_at, now);
        assert_eq!(
            sweep.to_record[0].request.direction,
            ObligationDirection::OwedByUs,
            "a delivery question is our work item, never a nudge at them"
        );

        // And with nobody holding a link there is nobody to consider — which is
        // not a claim that the room is healthy.
        let nobody = snapshot_from_events(ROOM, &[], &documents(), &[]).expect("snapshot");
        assert_eq!(nobody.len(), 0);
        let sweep = sweep_follow_ups(&scope, &audience(), ROOM, &nobody, &nobody, &policy(), now)
            .expect("sweep");
        assert!(sweep.is_empty());
    }

    /// Events from another room are refused, never filtered. `AccessStore` keeps
    /// one log per room, so a mismatch is a caller error — and quietly dropping
    /// the foreign events would report a thoroughly read room as never opened,
    /// manufacturing a delivery question out of a wiring mistake.
    #[test]
    fn events_from_another_room_are_refused_by_the_snapshot_builder() {
        let roster = vec![SharedToken::new("alice", shared_at())];
        let mut elsewhere = event("alice", 1, None, shared_at() + Duration::days(1));
        elsewhere.room_id = "room-91aa".to_string();

        let error = snapshot_from_events(ROOM, &roster, &documents(), &[elsewhere])
            .expect_err("a foreign event is refused");
        assert!(
            format!("{error:#}").contains("somebody else's history"),
            "the error must say why: {error:#}"
        );
    }

    /// A duplicated token in the roster contributes ONCE, first occurrence wins
    /// — the derivation's own rule, so the snapshot and the sweep cannot
    /// disagree about which share instant speaks. The duplicate here was shared
    /// LATER, so a last-wins fold would push the delivery question's due date
    /// out and betray itself.
    #[test]
    fn a_duplicated_shared_token_contributes_once_first_occurrence_wins() {
        let (_tmp, _store, scope) = register();
        let roster = vec![
            SharedToken::new("alice", shared_at()),
            SharedToken::new("alice", shared_at() + Duration::days(4)),
        ];

        let snapshot = snapshot_from_events(ROOM, &roster, &documents(), &[]).expect("snapshot");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].shared_at, shared_at());

        let now = shared_at() + Duration::days(3);
        let sweep = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &snapshot,
            &snapshot,
            &policy(),
            now,
        )
        .expect("sweep");
        assert_eq!(sweep.to_record.len(), 1);
        assert_eq!(
            sweep.to_record[0].request.due_at,
            shared_at() + Duration::days(3),
            "the first share instant is the basis, not the later duplicate's"
        );
    }

    /// A reply retires a follow-up already recorded. Suppression only stops
    /// FUTURE derivation, so without the settling half the row raised before the
    /// reply stays open forever — the leak the pairing exists to close. The
    /// reply is supplied by the caller: the room cannot see a mail, a call or a
    /// meeting.
    #[test]
    fn a_reply_supplied_by_the_caller_settles_the_row_already_recorded() {
        let (_tmp, store, scope) = register();
        let visit = shared_at() + Duration::days(1);
        let events = vec![event("alice", 1, None, visit)];
        let silent = vec![SharedToken::new("alice", shared_at())];
        let answered = vec![SharedToken::new("alice", shared_at()).with_reply(true)];
        let before = snapshot_from_events(ROOM, &silent, &documents(), &events).expect("snapshot");
        let after = snapshot_from_events(ROOM, &answered, &documents(), &events).expect("snapshot");
        assert!(after[0].replied);

        let ripened_at = visit + Duration::days(5);
        let raised = sweep_follow_ups(
            &scope,
            &audience(),
            ROOM,
            &before,
            &before,
            &policy(),
            ripened_at,
        )
        .expect("sweep");
        let id = raised.to_record[0].obligation_id.clone();
        apply_follow_up_sweep(&store, &scope, &raised, ripened_at).expect("apply");

        let later = ripened_at + Duration::days(1);
        let sweep = sweep_follow_ups(&scope, &audience(), ROOM, &before, &after, &policy(), later)
            .expect("sweep");
        assert_eq!(
            sweep.to_record.len(),
            0,
            "a reply suppresses future derivation"
        );
        assert_eq!(sweep.to_settle.len(), 1);
        assert_eq!(sweep.to_settle[0].obligation_id, id);

        let applied = apply_follow_up_sweep(&store, &scope, &sweep, later).expect("apply");
        assert_eq!(applied.settled.len(), 1);
        assert_eq!(applied.settled[0].obligation_id, id);
        assert_eq!(
            applied.settled[0].state(later),
            ObligationState::Released,
            "released, not met — nobody did the follow-up, it stopped applying"
        );
        assert_eq!(
            store
                .outstanding(&scope, &audience(), later)
                .expect("outstanding")
                .len(),
            0
        );
    }
}
