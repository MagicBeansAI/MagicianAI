//! The reading that explains a register row — the delivery half of §6.
//!
//! Doc: `docs/components/magician/obligations.md`. Plan:
//! `docs/plans/2026-08-07-opc-deal-close.md` §6.
//!
//! [`data_room::cycle`](magician_learning::data_room::cycle) composes §6's own
//! opening sentence —
//!
//! > *"Opened the deck three times, never opened the financials"*
//!
//! — and it was complete, tested, and called by **nothing**: its only caller
//! was a `#[cfg(test)]` assertion one crate over. The register meanwhile filled
//! from [`super::sweep_scope`] with rows whose text reads *"follow up on the
//! room shared with X; they opened it and have not replied"*, identically for
//! somebody who glanced once and somebody who came back five times and never
//! reached the second document. So the register said *what is owed* and the one
//! module that knew *why* reached no one.
//!
//! This is the composition that closes that, and it is the same shape as its
//! sibling [`super::sweep_scope`]: the stores are read here, the derivations
//! stay pure, and nothing about a kind of relationship or a kind of work
//! appears anywhere below.
//!
//! # It follows the register, it does not compete with it
//!
//! There is no second queue and no second store. A note is **derived on
//! demand** and keyed to the register row it explains, so the ordering, the
//! filtering and the idea of what is outstanding all stay the register's.
//! [`explain_register`] is the join, and it joins against the rows the caller
//! already loaded rather than against a second read — two reads could straddle
//! a write and disagree about what exists.
//!
//! # A note is never recorded
//!
//! [`AttentionNote::headline`] carries a visit count. The register's `what` is
//! part of its identity tuple, so a text carrying a count would mint a fresh
//! obligation on every visit — the flooding failure the follow-up derivation is
//! arranged to prevent. Deriving on demand is not a shortcut here; it is the
//! only shape that keeps the count out of the tuple.
//!
//! # Where the id comes from
//!
//! Never from here. [`attention_notes`] runs the sweep's own
//! [`derive_follow_ups`](magician_learning::data_room::derive_follow_ups) and
//! the register's own
//! [`obligation_id_for`](magician::magician_v2::obligations::store::obligation_id_for),
//! so the handle on a note is the handle the register recorded under or it is
//! `None`. A re-derivation that drifted by one character would attach every
//! note to a row that does not exist, which reads exactly like a room nobody
//! looked at.
//!
//! The one thing that CAN still drift is the **policy**: `due_at` is
//! `shared_at + delivery_question_after` or `last_seen + follow_up_after`, so
//! notes derived under windows the sweep did not run with address rows that
//! were never written. [`explain_register`] refuses to hide that — a handle the
//! register does not hold is reported as
//! [`row_not_in_register`](RegisterReadings::row_not_in_register), never quietly
//! attached to the nearest row and never silently dropped.
//!
//! # Fail closed
//!
//! - **An unreadable store is an error, never an empty book.** Every read
//!   propagates. "No one is waiting on us" out of a disk fault is the most
//!   reassuring wrong answer this composition could give.
//! - **A membership test over an empty register attaches nothing.** The join
//!   asks *"is this handle in the register"*, so an empty register attaches
//!   zero notes rather than passing vacuously.
//! - **Two absences are kept apart.** A note with no handle at all (they
//!   replied, the window has not closed, the room recorded a visit with no
//!   time) is [`not_yet_raised`](RegisterReadings::not_yet_raised) — expected,
//!   and nothing is wrong. A note whose handle the register does not hold is
//!   [`row_not_in_register`](RegisterReadings::row_not_in_register) — the sweep
//!   has not run, or it ran under different windows. Folding them together
//!   would bury the second in the noise of the first.
//!
//! # Nothing here names a kind of relationship
//!
//! Rooms carry an [`AudienceRef`](magician::magician_v2::audience::AudienceRef)
//! and it is passed straight through, exactly as [`super::sweep_scope`] passes
//! it. A support account's pack, a cohort's materials and a supplier's
//! diligence room compose through the identical code path, and no arm of
//! [`AudienceKind`](magician::magician_v2::audience::AudienceKind) is named below.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::obligations::{Obligation, ObligationScope};
use magician::magician_v2::share_links::{ShareLinkScope, ShareLinkStore};
use magician_learning::data_room::access_store::{AccessScope, AccessStore};
use magician_learning::data_room::{
    attention_notes, AttentionNote, DataRoomScope, DataRoomStore, FollowUpPolicy, SharedRoom,
};

use super::snapshot_room;

/// Every room in one scope, read as the agent whose book it is would read it.
///
/// Walks the same rooms [`super::sweep_scope`] walks, through the same
/// [`snapshot_room`] assembly, and hands them to
/// [`attention_notes`](magician_learning::data_room::attention_notes). The
/// roster, the documents and the events are therefore the identical view the
/// sweep derived its rows from — the property that lets a note carry a handle
/// the register actually holds.
///
/// `policy` must be the windows the **sweep** ran under. It is taken as a
/// parameter rather than read from configuration here for the reason every
/// derivation in this programme takes its inputs: a module that read
/// configuration would serve exactly one caller's cadence. What happens when
/// the two disagree is [`explain_register`]'s job, and it is visible rather
/// than silent.
///
/// Ordering is [`attention_notes`]': never-opened first — the only signal in
/// the set that suggests a message did not arrive, and the only one that cannot
/// wait a day — then returned-then-silent, then read-then-silent, then
/// answered.
///
/// # What an empty result means
///
/// *"This scope holds no rooms, or nobody holds a link to any of them."*
/// **Never** *"everybody is fine"*. An empty book is the vacuous case, and a
/// caller reading it as health is the bug this whole programme is climbing out
/// of.
///
/// # Errors
///
/// Any store that cannot be read, and any room the note derivation refuses — a
/// blank room id, an unnamed audience, or a `U+001F` anywhere in a component
/// that reaches the derivation. A refusal fails the whole composition rather
/// than skipping the room: a book missing the one relationship whose id was
/// malformed is a book that reads complete and is not.
pub fn attention_notes_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    scope: &ObligationScope,
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Result<Vec<AttentionNote>> {
    let rooms = DataRoomStore::new(workspace_layout.clone())
        .list(&DataRoomScope::new(
            scope.principal.clone(),
            scope.workspace.clone(),
        ))
        .with_context(|| {
            format!(
                "listing the data rooms of `{}`/`{}` to read what the counterparty did",
                scope.principal, scope.workspace
            )
        })?;

    let links = ShareLinkStore::new(workspace_layout.clone());
    let link_scope = ShareLinkScope::new(scope.principal.clone(), scope.workspace.clone());
    let access = AccessStore::new(workspace_layout.clone());
    let access_scope = AccessScope::new(scope.principal.clone(), scope.workspace.clone());

    let mut shared = Vec::with_capacity(rooms.len());
    for room in &rooms {
        let tokens = snapshot_room(&links, &link_scope, &access, &access_scope, room)?;
        shared.push(SharedRoom {
            audience: room.audience.clone(),
            // The room's stable id, which is what the store keys by. Its
            // display label is mutable and reaches the derivation, so a label
            // here would mint a handle for a row that does not exist.
            room_id: room.room_id.clone(),
            tokens,
        });
    }

    attention_notes(scope, &shared, policy, now)
}

/// The notes, sorted into what they can honestly say about the register.
///
/// Three buckets because there are three different facts, and a caller that
/// could not tell them apart would read the second as the third.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterReadings {
    /// Keyed by `obligation_id`, and only for handles the register **holds**.
    ///
    /// This is what a cycle joins onto the row it is about to act on: the row
    /// says a follow-up is owed, and this says they came back three times and
    /// never reached the second document.
    pub attached: BTreeMap<String, AttentionNote>,
    /// Notes carrying a handle the register does not hold.
    ///
    /// The alarming bucket, and the reason it is not folded into the one below.
    /// It means the room ripened something the register has no row for: the
    /// sweep has not run since, or it ran under different windows. Reported
    /// rather than raised — this module writes nothing — but never hidden.
    pub row_not_in_register: Vec<AttentionNote>,
    /// Notes with no handle at all, which is the ordinary case.
    ///
    /// They replied, the waiting window has not closed, or the room recorded a
    /// visit with no time and so has no ripening instant to derive from.
    /// Nothing is owed and nothing is wrong; the reading is carried anyway so a
    /// cycle can tell a live conversation from a quiet one rather than
    /// inferring silence from the absence of a note.
    pub not_yet_raised: Vec<AttentionNote>,
}

impl RegisterReadings {
    /// The note explaining one row, if the register holds a handle for it.
    pub fn for_obligation(&self, obligation_id: &str) -> Option<&AttentionNote> {
        self.attached.get(obligation_id)
    }
}

/// Join notes onto the register rows the caller already loaded.
///
/// `rows` is the caller's **unfiltered** read of the register. Joining against
/// a filtered view would report a note as addressing nothing merely because a
/// direction or a lapsed-only filter hid its row, which turns a display choice
/// into an alarm.
///
/// # Fail closed
///
/// - Membership is tested against the rows, so an **empty** register attaches
///   nothing rather than passing vacuously.
/// - A note whose handle is absent from `rows` is never attached to the nearest
///   row and never dropped; it lands in
///   [`row_not_in_register`](RegisterReadings::row_not_in_register).
/// - Two notes deriving the same handle cannot happen — the follow-up text
///   carries the room id and the token — but if a future derivation made it
///   possible, the first wins and the rest are surfaced as unmatched rather
///   than silently overwriting the explanation of a row somebody is about to
///   act on.
pub fn explain_register(rows: &[Obligation], notes: Vec<AttentionNote>) -> RegisterReadings {
    let recorded: BTreeSet<&str> = rows
        .iter()
        .map(|held| held.obligation_id.as_str())
        .collect();

    let mut readings = RegisterReadings::default();
    for note in notes {
        let Some(obligation_id) = note.obligation_id.clone() else {
            readings.not_yet_raised.push(note);
            continue;
        };
        if !recorded.contains(obligation_id.as_str())
            || readings.attached.contains_key(&obligation_id)
        {
            readings.row_not_in_register.push(note);
            continue;
        }
        readings.attached.insert(obligation_id, note);
    }
    readings
}

#[cfg(test)]
mod tests {
    //! What the counterparty did, reaching the row that says we owe them
    //! something.
    //!
    //! Every test seeds a room, a grant and real access events, then asserts
    //! the seeded thing is readable before asserting anything about the join. A
    //! test over an empty room would pass against the very bug this module
    //! exists to fix.

    use chrono::{Duration, TimeZone};

    use magician::magician_v2::audience::{Audience, AudienceRef};
    use magician::magician_v2::evidence::OutwardAssertionStore;
    use magician::magician_v2::obligations::ObligationStore;
    use magician::magician_v2::share_links::IssueShareLink;
    use magician_learning::data_room::access_log::{AccessEvent, UserAgentClass};
    use magician_learning::data_room::{
        AttentionReading, DocumentVisibility, GrantDisclosure, OpenDataRoom,
    };

    use super::super::{sweep_scope, SweepMemory, SweepPolicy};
    use super::*;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";
    const DECK: &str = "artifact://deck@1";
    const FINANCIALS: &str = "artifact://financials@1";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn scope() -> ObligationScope {
        ObligationScope::new(PRINCIPAL, WORKSPACE)
    }

    /// Two days for a delivery question, five for a follow-up — the same shape
    /// the worker's defaults describe, and the windows every test here derives
    /// under so a handle drift would show up as a failure rather than as a
    /// plausible empty answer.
    fn follow_up_policy() -> FollowUpPolicy {
        FollowUpPolicy::new(Duration::days(2), Duration::days(5)).expect("follow-up policy")
    }

    fn sweep_policy() -> SweepPolicy {
        SweepPolicy::new(Duration::days(3), follow_up_policy()).expect("sweep policy")
    }

    fn workspace() -> (tempfile::TempDir, ArtifactV2Workspace) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        (tmp, layout)
    }

    /// A room holding the deck and the financials, shared `days_ago` with one
    /// identity.
    fn seed_room(
        layout: &ArtifactV2Workspace,
        audience: &AudienceRef,
        issued_to: &str,
        days_ago: i64,
    ) -> String {
        let rooms = DataRoomStore::new(layout.clone());
        let room_scope = DataRoomScope::new(PRINCIPAL, WORKSPACE);
        let room = rooms
            .open(
                &room_scope,
                &OpenDataRoom {
                    audience: audience.clone(),
                    opened_by: "owner".to_string(),
                    closes_at: None,
                },
                now() - Duration::days(days_ago + 1),
            )
            .expect("open the room");
        ShareLinkStore::new(layout.clone())
            .issue(
                &ShareLinkScope::new(PRINCIPAL, WORKSPACE),
                &IssueShareLink {
                    resource_ref: room.room_id.clone(),
                    audience: audience.clone(),
                    issued_to: issued_to.to_string(),
                    secret: "a-secret".to_string(),
                    expires_at: now() + Duration::days(30),
                },
                now() - Duration::days(days_ago),
            )
            .expect("issue the grant");

        let assertions = OutwardAssertionStore::new(layout.clone());
        let roster = Audience::new(audience.clone(), vec![issued_to.to_string()]);
        let holders = vec![issued_to.to_string()];
        for artifact_ref in [DECK, FINANCIALS] {
            rooms
                .add_document(
                    &room_scope,
                    &room.room_id,
                    artifact_ref,
                    DocumentVisibility::Everyone,
                    "owner",
                    &GrantDisclosure {
                        assertions: &assertions,
                        audience: &roster,
                        holders: &holders,
                        disclosed_by: "owner",
                    },
                    now() - Duration::days(days_ago),
                )
                .expect("put the document in the room");
        }
        room.room_id
    }

    /// One presentation of a token, `days_ago`, against `document`.
    ///
    /// The room's own audience is carried on the event, exactly as the reader
    /// surface carries it: a log line has to be readable without loading the
    /// room it came from.
    fn seed_visit(
        layout: &ArtifactV2Workspace,
        room_id: &str,
        audience: &AudienceRef,
        issued_to: &str,
        sequence: u32,
        document: Option<&str>,
        days_ago: i64,
    ) {
        AccessStore::new(layout.clone())
            .record_access(
                &AccessScope::new(PRINCIPAL, WORKSPACE),
                room_id,
                &AccessEvent {
                    room_id: room_id.to_string(),
                    audience: audience.clone(),
                    token_issued_to: issued_to.to_string(),
                    document_ref: document.map(str::to_string),
                    occurred_at: now() - Duration::days(days_ago),
                    dwell_ms: None,
                    sequence,
                    user_agent_class: UserAgentClass::Desktop,
                },
            )
            .expect("record the visit");
    }

    /// §6's opening sentence reaches the row the sweep wrote.
    ///
    /// Pins the island: `data_room::cycle::attention_notes` had no production
    /// caller, so the register carried "they opened it and have not replied"
    /// and the count of visits and the document they never reached existed
    /// nowhere an agent could read. The assertion that matters is the
    /// **handle** — a note whose `obligation_id` is not a row the sweep
    /// recorded is a note attached to nothing.
    #[test]
    fn the_note_explaining_a_row_carries_that_row_s_own_handle() {
        let (_tmp, layout) = workspace();
        let audience = AudienceRef::engagement("eng-1");
        let room_id = seed_room(&layout, &audience, "partner@example.test", 12);
        // Three visits, and only the deck was ever reached.
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            1,
            None,
            11,
        );
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            2,
            Some(DECK),
            10,
        );
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            3,
            Some(DECK),
            9,
        );

        // The seeded activity is readable before anything is asserted about the
        // register or the join.
        let events = AccessStore::new(layout.clone())
            .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &room_id)
            .expect("read the access lane");
        assert_eq!(events.len(), 3, "the seeded visits must be readable");

        let (report, _next) = sweep_scope(
            &layout,
            &scope(),
            &SweepMemory::unseeded(),
            &sweep_policy(),
            now(),
        )
        .expect("sweep");
        assert_eq!(report.rooms_seen, 1);
        assert_eq!(report.recorded, 1, "the quiet room owes one follow-up");

        let rows = ObligationStore::new(layout.clone())
            .all_obligations(&scope())
            .expect("read the register");
        assert_eq!(rows.len(), 1);
        // The register CANNOT carry the nuance: its `what` is part of the
        // identity tuple, so it holds no count and names no document.
        assert!(
            !rows[0].what.contains("3 times") && !rows[0].what.contains(FINANCIALS),
            "the recorded text must carry neither the visit count nor the \
             unread document: `{}`",
            rows[0].what
        );

        let notes = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("notes");
        assert_eq!(notes.len(), 1);
        let note = &notes[0];
        assert_eq!(
            note.obligation_id.as_deref(),
            Some(rows[0].obligation_id.as_str()),
            "the note must address the row the sweep actually recorded"
        );
        // And the nuance the register threw away survives here.
        assert_eq!(note.reading, AttentionReading::ReturnedThenSilent);
        assert_eq!(note.visits, 3);
        assert_eq!(note.documents_opened, vec![DECK.to_string()]);
        assert_eq!(note.documents_unopened, vec![FINANCIALS.to_string()]);
        assert!(note.is_partial());
        assert_eq!(
            note.headline,
            format!(
                "partner@example.test has opened room '{room_id}' 3 times and has not replied; \
                 opened 1 of 2 documents, 1 never reached"
            )
        );

        let readings = explain_register(&rows, notes);
        assert_eq!(readings.attached.len(), 1);
        assert!(readings.row_not_in_register.is_empty());
        assert!(readings.not_yet_raised.is_empty());
        assert_eq!(
            readings
                .for_obligation(&rows[0].obligation_id)
                .expect("the row is explained")
                .visits,
            3
        );
    }

    /// Opened-once and opened-repeatedly are one row and two readings.
    ///
    /// Pins what `follow_ups` deliberately collapses. Both rooms record the
    /// same follow-up sentence — it must, or the identity tuple would move on
    /// every visit — so an agent reading only the register cannot tell a skim
    /// from a study. The note is where that distinction survives, and this
    /// asserts both halves at once.
    #[test]
    fn one_visit_and_five_visits_record_the_same_row_and_read_differently() {
        let (_tmp, layout) = workspace();
        let skimmer = AudienceRef::engagement("eng-skim");
        let student = AudienceRef::engagement("eng-study");
        let skim_room = seed_room(&layout, &skimmer, "one@example.test", 12);
        let study_room = seed_room(&layout, &student, "many@example.test", 12);

        seed_visit(
            &layout,
            &skim_room,
            &skimmer,
            "one@example.test",
            1,
            Some(DECK),
            10,
        );
        for sequence in 1..=5 {
            seed_visit(
                &layout,
                &study_room,
                &student,
                "many@example.test",
                sequence,
                Some(DECK),
                10,
            );
        }

        sweep_scope(
            &layout,
            &scope(),
            &SweepMemory::unseeded(),
            &sweep_policy(),
            now(),
        )
        .expect("sweep");
        let rows = ObligationStore::new(layout.clone())
            .all_obligations(&scope())
            .expect("read the register");
        assert_eq!(rows.len(), 2, "one follow-up per quiet room");

        let skim_row = rows
            .iter()
            .find(|held| held.what.contains(&skim_room))
            .expect("the skimmer's row");
        let study_row = rows
            .iter()
            .find(|held| held.what.contains(&study_room))
            .expect("the student's row");
        // The recorded texts differ ONLY in the room and the token. Substitute
        // those and the two are byte-identical: the register has no slot for
        // "came back five times".
        assert_eq!(
            skim_row
                .what
                .replace(&skim_room, "R")
                .replace("one@example.test", "T"),
            study_row
                .what
                .replace(&study_room, "R")
                .replace("many@example.test", "T"),
            "the register's text cannot distinguish a skim from a study"
        );

        let notes = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("notes");
        let readings = explain_register(&rows, notes);
        assert_eq!(readings.attached.len(), 2);
        let skim = readings
            .for_obligation(&skim_row.obligation_id)
            .expect("the skimmer's reading");
        let study = readings
            .for_obligation(&study_row.obligation_id)
            .expect("the student's reading");
        assert_eq!(skim.reading, AttentionReading::ReadThenSilent);
        assert_eq!(skim.visits, 1);
        assert_eq!(study.reading, AttentionReading::ReturnedThenSilent);
        assert_eq!(study.visits, 5);
    }

    /// A room shared and never opened is a delivery question, and it is
    /// explained too.
    ///
    /// §6 calls never-opened *"the one that earns the whole feature"*: it is
    /// the only evidence the system can produce that a message did not arrive.
    /// Pins that the ghost token — present in the roster and in no event —
    /// survives the composition rather than being filtered out for having no
    /// history, and that its note addresses the delivery-question row.
    #[test]
    fn a_never_opened_room_is_explained_as_a_delivery_question() {
        let (_tmp, layout) = workspace();
        let audience = AudienceRef::account("acct-1");
        let room_id = seed_room(&layout, &audience, "silent@example.test", 6);

        // Nothing was ever presented, which is the point.
        assert!(
            AccessStore::new(layout.clone())
                .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &room_id)
                .expect("read the access lane")
                .is_empty(),
            "the room must have no visits for this to be the never-opened case"
        );

        sweep_scope(
            &layout,
            &scope(),
            &SweepMemory::unseeded(),
            &sweep_policy(),
            now(),
        )
        .expect("sweep");
        let rows = ObligationStore::new(layout.clone())
            .all_obligations(&scope())
            .expect("read the register");
        assert_eq!(rows.len(), 1);

        let notes = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("notes");
        assert_eq!(notes.len(), 1, "the ghost token must survive");
        assert_eq!(notes[0].reading, AttentionReading::NeverOpened);
        assert!(notes[0].reading.is_delivery_question());
        assert_eq!(notes[0].visits, 0);

        let readings = explain_register(&rows, notes);
        assert_eq!(
            readings
                .for_obligation(&rows[0].obligation_id)
                .expect("the delivery question is explained")
                .reading,
            AttentionReading::NeverOpened
        );
    }

    /// Sweeping twice leaves one row and one explanation.
    ///
    /// Pins the flooding failure from the note's side: a handle that moved
    /// between sweeps would leave the second run's note pointing at a row the
    /// first run recorded under a different id, and the register would carry
    /// two chases for one silence.
    #[test]
    fn a_second_sweep_leaves_the_same_row_and_the_same_handle() {
        let (_tmp, layout) = workspace();
        let audience = AudienceRef::engagement("eng-1");
        let room_id = seed_room(&layout, &audience, "partner@example.test", 12);
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            1,
            Some(DECK),
            10,
        );

        let (_, memory) = sweep_scope(
            &layout,
            &scope(),
            &SweepMemory::unseeded(),
            &sweep_policy(),
            now(),
        )
        .expect("first sweep");
        let first = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("first notes");

        let (report, _) =
            sweep_scope(&layout, &scope(), &memory, &sweep_policy(), now()).expect("second sweep");
        assert_eq!(report.settled, 0, "an unchanged basis settles nothing");

        let rows = ObligationStore::new(layout.clone())
            .all_obligations(&scope())
            .expect("read the register");
        assert_eq!(rows.len(), 1, "two sweeps over one silence leave one row");
        let second = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("second notes");
        assert_eq!(
            first[0].obligation_id, second[0].obligation_id,
            "the handle must not move between sweeps"
        );
        assert_eq!(
            explain_register(&rows, second).attached.len(),
            1,
            "and it must still address the one row"
        );
    }

    /// A handle the register does not hold is reported, never attached.
    ///
    /// Pins the fail-closed direction of the join. The membership test runs
    /// against the rows the caller loaded, so an empty register attaches
    /// nothing — the predicate must not pass vacuously — and the note surfaces
    /// as unmatched so "the sweep has not run" is visible rather than reading
    /// like a room nobody looked at.
    #[test]
    fn a_note_whose_row_is_absent_is_never_attached_to_a_row() {
        let (_tmp, layout) = workspace();
        let audience = AudienceRef::engagement("eng-1");
        let room_id = seed_room(&layout, &audience, "partner@example.test", 12);
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            1,
            Some(DECK),
            10,
        );

        // The note ripens whether or not the sweep ever ran.
        let notes = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("notes");
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].obligation_id.is_some(),
            "the reading is ripe, so it names the row it would explain"
        );

        let register = ObligationStore::new(layout.clone())
            .all_obligations(&scope())
            .expect("read the register");
        assert!(register.is_empty(), "no sweep has run, so nothing is owed");

        let readings = explain_register(&register, notes);
        assert!(
            readings.attached.is_empty(),
            "a membership test over an empty register must attach nothing"
        );
        assert_eq!(readings.row_not_in_register.len(), 1);
        assert!(
            readings.not_yet_raised.is_empty(),
            "an absent row is a different fact from a reading that never ripened"
        );
    }

    /// A reading that raised nothing is kept apart from one whose row is
    /// missing.
    ///
    /// Pins the distinction the two absences would otherwise lose. A visit
    /// inside the waiting window owes nothing and is entirely healthy; a ripe
    /// reading with no row means the sweep did not run. Folding them into one
    /// bucket buries the second in the noise of the first.
    #[test]
    fn a_reading_inside_the_waiting_window_is_not_reported_as_a_missing_row() {
        let (_tmp, layout) = workspace();
        let audience = AudienceRef::engagement("eng-1");
        // Shared yesterday and read yesterday: inside both windows.
        let room_id = seed_room(&layout, &audience, "partner@example.test", 1);
        seed_visit(
            &layout,
            &room_id,
            &audience,
            "partner@example.test",
            1,
            Some(DECK),
            1,
        );

        let (report, _next) = sweep_scope(
            &layout,
            &scope(),
            &SweepMemory::unseeded(),
            &sweep_policy(),
            now(),
        )
        .expect("sweep");
        assert_eq!(report.rooms_seen, 1, "the room was seen");
        assert_eq!(report.recorded, 0, "and nothing has ripened yet");

        let notes = attention_notes_for_scope(&layout, &scope(), &follow_up_policy(), now())
            .expect("notes");
        assert_eq!(notes.len(), 1, "the reading exists even with nothing owed");
        assert_eq!(notes[0].reading, AttentionReading::ReadThenSilent);
        assert!(notes[0].obligation_id.is_none());

        let readings = explain_register(&[], notes);
        assert_eq!(readings.not_yet_raised.len(), 1);
        assert!(
            readings.row_not_in_register.is_empty(),
            "a reading that raised nothing must not be reported as a missing row"
        );
        assert!(readings.attached.is_empty());
    }
}
