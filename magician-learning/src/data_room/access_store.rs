//! Where a presentation is written down — the durable lane under
//! [`super::access_log`].
//!
//! The access log defines what a presentation *is* ([`AccessEvent`]) and what a
//! set of them *means* ([`attention_for`](super::access_log::attention_for),
//! [`attention_across`](super::access_log::attention_across),
//! [`document_reach`](super::access_log::document_reach)) — and nothing could
//! store one. Every derivation in that module therefore ran over data that
//! could not exist: `NeverOpened` for everybody, always, whatever the
//! counterparty actually did. This module is the missing half, and it is
//! deliberately nothing more than the lane: it holds events and hands them back
//! in observed order, and every judgement about them stays in `access_log`.
//!
//! # Possession, never identity
//!
//! The stored row is an [`AccessEvent`] unchanged, which means it carries
//! [`token_issued_to`](AccessEvent::token_issued_to) and **no identity field**.
//! That absence is the access log's whole correction after external review — a
//! capability URL proves possession of a link, and links get forwarded — and a
//! store that added an `identity` column "for convenience" would reintroduce
//! precisely the claim the type was reshaped to make unwritable. Nothing here
//! infers who was at the keyboard, and nothing here should ever be extended to.
//!
//! # A visit is a sequence, a presentation is a reach at an instant
//!
//! [`TokenAttention::visits`](super::access_log::TokenAttention::visits) is the
//! highest `sequence` seen, never the event count, because one visit that views
//! the index and then opens a document is **two events and one visit**. So the
//! identity of a row cannot be `(room, token, sequence)` alone — that would fuse
//! the index view and the document open of a single sitting into one row and
//! silently lose half of what the room observed. A row is
//! `(room, token, sequence, document_ref, occurred_at)`: which visit, what it
//! reached, when. A retried write replays the same event and resumes; a genuine
//! second reach lands at a different instant and is its own row.
//!
//! # Append-only, folded on read
//!
//! Like every store in this set: the file is the history, the current answer is
//! the fold, and read faults keep their two meanings apart per
//! [`magician::magician_v2::jsonl`] — only a **missing** log reads as an empty one.
//! An unreadable log folded to "empty" would report `NeverOpened` for a room
//! that was read thoroughly, turning a disk fault into the strongest delivery
//! signal the feature has.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::access_log::{attention_across, AccessEvent, TokenAttention};

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessScope {
    pub principal: String,
    pub workspace: String,
}

impl AccessScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One presentation as the lane holds it: the event, and the id it was filed
/// under.
///
/// The id is returned rather than hidden so a caller can tell a fresh record
/// from a resumed one without re-reading the log, and so a retry can be proven
/// to have produced the same row rather than a second one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAccess {
    pub access_id: String,
    pub event: AccessEvent,
}

/// One line in a room's access log.
///
/// A tagged enum with a single variant on purpose: the format has room for a
/// later record kind (a redaction, a correction note) without the existing
/// lines becoming unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum AccessRecord {
    Presented {
        access_id: String,
        event: AccessEvent,
    },
}

/// The durable lane for room access.
#[derive(Debug, Clone)]
pub struct AccessStore {
    workspace_layout: ArtifactV2Workspace,
}

impl AccessStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &AccessScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("room_access")
    }

    /// One room, one log.
    ///
    /// The file name is the **hash** of the room id, never the id itself: the
    /// id arrives from a caller here (unlike
    /// [`DataRoomStore`](super::store::DataRoomStore), which derives its own),
    /// and interpolating caller text into a path is how a room id of `../..`
    /// writes somewhere it was never meant to.
    fn room_path(&self, scope: &AccessScope, room_id: &str) -> PathBuf {
        self.root(scope)
            .join(format!("{}.jsonl", stable_id(room_id)))
    }

    /// The rooms this scope has ever recorded access for.
    ///
    /// A log rather than a directory scan: the per-room files are named by
    /// hash, so the ids are not recoverable from the filesystem, and a scan
    /// would also depend on a listing capability not every file provider
    /// offers.
    fn index_path(&self, scope: &AccessScope) -> PathBuf {
        self.root(scope).join("rooms.jsonl")
    }

    /// Write down one presentation of a room token.
    ///
    /// Idempotent on `(room, token, sequence, document_ref, occurred_at)` — the
    /// visit, what it reached, and when. **A retried write is one event**:
    /// recording is on the path of an HTTP handler that may be retried by a
    /// proxy, a client, or a caller that never saw our response, and a lane
    /// that double-counted those would inflate `presentations` and
    /// `document_reach` into evidence of interest that nobody showed.
    ///
    /// An identical replay **resumes** and returns the row already written. A
    /// replay whose remaining observations differ — a different audience, dwell
    /// or user-agent class against the same presentation — is **an error, not a
    /// silent no-op**: two contradictory observations of one instant is a
    /// caller bug, and discarding the second would leave the caller believing
    /// the log holds what it just sent while the log holds something else.
    ///
    /// Ordering is **index before row** (see [`Self::rooms_with_access`]).
    pub fn record_access(
        &self,
        scope: &AccessScope,
        room_id: &str,
        event: &AccessEvent,
    ) -> Result<RecordedAccess> {
        let room_id = room_id.trim();
        validate(scope, room_id, event)?;

        let access_id = derive_access_id(scope, room_id, event);
        if let Some(existing) = self
            .presentations(scope, room_id)?
            .into_iter()
            .find(|held| held.access_id == access_id)
        {
            if existing.event == *event {
                return Ok(existing);
            }
            anyhow::bail!(
                "presentation `{access_id}` is already recorded with different observations: \
                 the same token, visit, document and instant cannot have carried two different \
                 audiences, dwells or user-agent classes. An identical replay resumes; a changed \
                 payload is a correction, and silently keeping the first would leave the caller \
                 believing the log holds what it sent"
            );
        }

        // Index BEFORE row: "the row exists" must imply "the index exists". A
        // room whose events exist but whose id no sweep can enumerate is a room
        // where nothing ever ripens and nothing ever settles — invisible, and
        // indistinguishable from a quiet counterparty. A crash between the two
        // writes must therefore leave a dangling index entry, which resolves to
        // an empty event list and reads as `NeverOpened` — the conservative
        // answer, and the one a retry immediately corrects.
        if !self
            .indexed_rooms(scope)?
            .iter()
            .any(|indexed| indexed == room_id)
        {
            self.append_room_index(scope, room_id)?;
        }
        self.append(
            &self.room_path(scope, room_id),
            &AccessRecord::Presented {
                access_id: access_id.clone(),
                event: event.clone(),
            },
        )?;
        Ok(RecordedAccess {
            access_id,
            event: event.clone(),
        })
    }

    /// Every presentation recorded against one room, **oldest observed first**.
    ///
    /// Ordered by `occurred_at` rather than by file order because appends
    /// interleave: two readers presenting tokens at the same moment land in
    /// whichever order the writes completed, and a caller reasoning about
    /// "first seen" and "last seen" off file order would read the room's
    /// history in a sequence that never happened. Ties break on
    /// `(token, sequence, document_ref, access_id)` so the answer is the same
    /// on every read.
    ///
    /// An empty result means **nothing has been recorded**, which is exactly
    /// the `NeverOpened` case and must never be read as "everyone is fine": a
    /// missing log is the state of a room that was shared and never opened, the
    /// one signal §6 says earns the whole feature. An unreadable log is an
    /// error, not an empty one.
    pub fn events_for(&self, scope: &AccessScope, room_id: &str) -> Result<Vec<AccessEvent>> {
        Ok(self
            .presentations(scope, room_id.trim())?
            .into_iter()
            .map(|recorded| recorded.event)
            .collect())
    }

    /// What the room can say about each token it was shared with.
    ///
    /// `shared_with` is **supplied**, never derived from the events, exactly as
    /// [`attention_across`] requires: a token that never appears in the log is
    /// the never-opened case, and deriving the roster from the log would make
    /// the most important signal in the feature invisible by construction.
    ///
    /// An empty `shared_with` yields an empty answer. That is "nobody holds a
    /// link", never "everybody is fine" — a caller must not read the empty list
    /// as a healthy room.
    pub fn attention_snapshot(
        &self,
        scope: &AccessScope,
        room_id: &str,
        shared_with: &[String],
        room_documents: &[String],
    ) -> Result<Vec<TokenAttention>> {
        let events = self.events_for(scope, room_id)?;
        Ok(attention_across(shared_with, room_documents, &events))
    }

    /// Every room this scope has recorded access for, sorted.
    ///
    /// Served by the index rather than a scan. A dangling entry — a record torn
    /// between the index write and the row — names a room whose log is empty,
    /// which resolves to no events: index-before-row means this list may
    /// **over-promise, never under-deliver**, and an over-promise here costs a
    /// wasted read while an under-delivery would hide a room from every sweep.
    pub fn rooms_with_access(&self, scope: &AccessScope) -> Result<Vec<String>> {
        let mut rooms = self.indexed_rooms(scope)?;
        rooms.sort();
        Ok(rooms)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    /// The folded log: one row per `access_id`, **first write wins**.
    ///
    /// Defensively as well as at the write. A duplicate line — a replay from an
    /// older binary, two processes appending at once — must not turn one
    /// presentation into two, because the counts derived from these rows are
    /// the whole product.
    fn presentations(&self, scope: &AccessScope, room_id: &str) -> Result<Vec<RecordedAccess>> {
        let path = self.room_path(scope, room_id);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };

        let mut seen = BTreeSet::new();
        let mut out: Vec<RecordedAccess> = Vec::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in magician::magician_v2::jsonl::parse_log_lines::<AccessRecord>(&raw, &path)? {
            let AccessRecord::Presented { access_id, event } = record;
            if seen.insert(access_id.clone()) {
                out.push(RecordedAccess { access_id, event });
            }
        }
        out.sort_by(|left, right| {
            left.event
                .occurred_at
                .cmp(&right.event.occurred_at)
                .then_with(|| left.event.token_issued_to.cmp(&right.event.token_issued_to))
                .then_with(|| left.event.sequence.cmp(&right.event.sequence))
                .then_with(|| left.event.document_ref.cmp(&right.event.document_ref))
                .then_with(|| left.access_id.cmp(&right.access_id))
        });
        Ok(out)
    }

    /// The room ids the index names, deduplicated preserving first-seen order.
    ///
    /// An unparseable **interior** entry fails the read: a skipped entry is a
    /// room no sweep will ever look at, which is the exact silence the index
    /// exists to break. A torn **final** line is an index append that never
    /// completed, and index-before-row means the row it would have named was
    /// never written, so reading past it drops nothing findable.
    fn indexed_rooms(&self, scope: &AccessScope) -> Result<Vec<String>> {
        let path = self.index_path(scope);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for room_id in magician::magician_v2::jsonl::parse_log_lines::<String>(&raw, &path)? {
            if seen.insert(room_id.clone()) {
                out.push(room_id);
            }
        }
        Ok(out)
    }

    /// One index line: the room id, JSON-encoded rather than raw so an id
    /// carrying a newline cannot shear the line format.
    fn append_room_index(&self, scope: &AccessScope, room_id: &str) -> Result<()> {
        let path = self.index_path(scope);
        let mut line = serde_json::to_vec(room_id)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending index {}", path.display()))?;
        Ok(())
    }

    fn append(&self, path: &Path, record: &AccessRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &Path) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" would report a
        // thoroughly read room as never opened, which is the strongest signal
        // in the feature manufactured out of a disk fault. Shared semantics
        // live in `magician_v2::jsonl`.
        magician::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// What this lane refuses to write down.
///
/// Every check here is about a row that would still parse but would answer a
/// later question wrongly — the failures a schema cannot catch.
fn validate(scope: &AccessScope, room_id: &str, event: &AccessEvent) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator \
             that keeps a presentation id's components from bleeding into each other, and a \
             scope carrying it could fuse two tenants' access into one row"
        );
    }
    if room_id.is_empty() {
        anyhow::bail!(
            "a presentation must name the room it happened in; without one there is no \
             history for anybody to read back"
        );
    }
    if room_id.contains(FIELD_SEP) {
        anyhow::bail!(
            "a room id must not contain U+001F: it is the separator that keeps a presentation \
             id's components from bleeding into each other, and a room id carrying it could \
             fuse one room's visit with another's"
        );
    }
    if event.room_id != room_id {
        anyhow::bail!(
            "this event names room `{}`, but it is being filed under `{room_id}`: a \
             presentation in the wrong log would answer 'what have they seen' with somebody \
             else's history",
            event.room_id
        );
    }
    if event.token_issued_to.trim().is_empty() {
        anyhow::bail!(
            "a presentation must name the link that was used; the token is the only thing the \
             room actually observed, and a row without one records that somebody came without \
             recording anything about how"
        );
    }
    if event.token_issued_to.contains(FIELD_SEP) {
        anyhow::bail!(
            "a token must not contain U+001F: it is the separator that keeps a presentation \
             id's components from bleeding into each other, and a token carrying it could fuse \
             two readers' visits into one row"
        );
    }
    if !event.audience.is_named() {
        anyhow::bail!(
            "a presentation must name the audience the room serves; an unnamed relationship \
             leaves a log line that cannot be read without loading the room it came from, \
             which is the coupling carrying the audience on the event exists to avoid"
        );
    }
    match event.document_ref.as_deref() {
        Some(document) if document.trim().is_empty() => anyhow::bail!(
            "a blank document reference is indistinguishable from an index view (`None`), and \
             the difference is 'they looked and opened nothing' versus 'they opened this' — \
             the distinction the whole partial-read signal rests on"
        ),
        Some(document) if document.contains(FIELD_SEP) => anyhow::bail!(
            "a document reference must not contain U+001F: it is the separator that keeps a \
             presentation id's components from bleeding into each other, and a reference \
             carrying it could fuse two documents' reads into one row"
        ),
        _ => {},
    }
    if event.sequence == 0 {
        anyhow::bail!(
            "a presentation's sequence starts at 1 — first visit, or nth. Visits derive from \
             the highest sequence seen, so a row recorded at zero would leave a token that \
             plainly came reading as never-opened, manufacturing the delivery question that \
             earns the whole feature out of somebody who was already reading"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one presentation.
///
/// The tuple is the visit, what it reached, and when:
///
/// - `scope.principal`, `scope.workspace` — two tenants recording against the
///   same room id must not resume each other's rows;
/// - `room_id` — whose history this belongs to;
/// - `token_issued_to` — which link was presented, never who presented it;
/// - `sequence` — which visit, the field
///   [`TokenAttention::visits`](super::access_log::TokenAttention::visits)
///   counts from;
/// - `document_ref` — what that visit reached, `None` (the index) written as
///   the empty string, which no valid reference can collide with because a
///   blank one is refused;
/// - `occurred_at` — when. Without it the index view and the document open of a
///   single sitting share a tuple and the second is silently dropped; with it a
///   retried write still replays one instant and still resumes.
fn derive_access_id(scope: &AccessScope, room_id: &str, event: &AccessEvent) -> String {
    format!(
        "acc-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            room_id,
            event.token_issued_to,
            event.sequence,
            event.document_ref.as_deref().unwrap_or(""),
            event.occurred_at.to_rfc3339(),
        ))
    )
}

#[cfg(test)]
mod tests {
    //! The lane's contract, as behaviour.

    use chrono::{DateTime, Duration, TimeZone, Utc};

    use crate::data_room::access_log::{attention_for, AttentionSignal, UserAgentClass};
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::{AudienceKind, AudienceRef};

    use super::{AccessEvent, AccessScope, AccessStore};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn store() -> (tempfile::TempDir, AccessStore, AccessScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = AccessStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, AccessScope::new("anonymous", "default"))
    }

    fn event(
        token: &str,
        sequence: u32,
        document: Option<&str>,
        occurred_at: DateTime<Utc>,
    ) -> AccessEvent {
        AccessEvent {
            room_id: "room-1".to_string(),
            audience: AudienceRef::engagement("eng-1"),
            token_issued_to: token.to_string(),
            document_ref: document.map(str::to_string),
            occurred_at,
            dwell_ms: None,
            sequence,
            user_agent_class: UserAgentClass::Desktop,
        }
    }

    /// The lane exists at all: an event written down is an event read back.
    /// Every derivation in `access_log` ran over data nothing could store, so
    /// this is the round-trip the whole feature was missing — and the read is
    /// ordered by OBSERVED time, not by file order, because appends interleave
    /// and a caller reading "first seen" off write order reads a sequence that
    /// never happened.
    #[test]
    fn presentations_read_back_in_observed_order_not_write_order() {
        let (_tmp, store, scope) = store();
        // Appended newest-first, on purpose.
        for (token, sequence, at) in [
            ("bob", 1, now()),
            ("alice", 2, now() - Duration::hours(1)),
            ("alice", 1, now() - Duration::days(2)),
        ] {
            store
                .record_access(&scope, "room-1", &event(token, sequence, None, at))
                .expect("record");
        }

        let events = store.events_for(&scope, "room-1").expect("read");
        let observed: Vec<(&str, u32, DateTime<Utc>)> = events
            .iter()
            .map(|held| {
                (
                    held.token_issued_to.as_str(),
                    held.sequence,
                    held.occurred_at,
                )
            })
            .collect();
        assert_eq!(
            observed,
            vec![
                ("alice", 1, now() - Duration::days(2)),
                ("alice", 2, now() - Duration::hours(1)),
                ("bob", 1, now()),
            ]
        );
    }

    /// A retried write is ONE event. Recording sits on a path a proxy or a
    /// client may replay, and a lane that counted the replay would inflate
    /// `presentations` and `document_reach` into evidence of interest nobody
    /// showed. The resumed call returns the row already written — same id.
    #[test]
    fn a_retried_write_is_one_presentation() {
        let (_tmp, store, scope) = store();
        let presented = event("alice", 1, Some("deck"), now());

        let first = store
            .record_access(&scope, "room-1", &presented)
            .expect("record");
        let retry = store
            .record_access(&scope, "room-1", &presented)
            .expect("retry");

        assert_eq!(
            first.access_id, retry.access_id,
            "the retry resumes the row"
        );
        assert_eq!(retry.event, presented);
        assert_eq!(store.events_for(&scope, "room-1").expect("read").len(), 1);
    }

    /// Visits derive from SEQUENCE, not from the event count. One sitting that
    /// views the index and then opens a document is two events and one visit —
    /// so both rows must survive (an id keyed on `(room, token, sequence)`
    /// alone would fuse them) while the signal stays `OpenedOnce`. Counting
    /// events here would report "came back" for somebody who came once and
    /// clicked twice.
    #[test]
    fn one_visit_can_hold_two_presentations_and_is_still_one_visit() {
        let (_tmp, store, scope) = store();
        store
            .record_access(&scope, "room-1", &event("alice", 1, None, now()))
            .expect("index view");
        store
            .record_access(
                &scope,
                "room-1",
                &event("alice", 1, Some("deck"), now() + Duration::seconds(20)),
            )
            .expect("document open");

        let events = store.events_for(&scope, "room-1").expect("read");
        assert_eq!(events.len(), 2, "the index view and the open are both rows");

        let attention = attention_for(
            "alice",
            &["deck".to_string(), "financials".to_string()],
            &events,
        );
        assert_eq!(attention.visits, 1);
        assert_eq!(attention.presentations, 2);
        assert_eq!(attention.index_views, 1);
        assert_eq!(attention.signal(), AttentionSignal::OpenedOnce);
        assert_eq!(attention.documents_opened, vec!["deck".to_string()]);
        assert_eq!(attention.documents_unopened, vec!["financials".to_string()]);
        assert!(attention.is_partial());
    }

    /// Idempotency must not swallow a correction. The same visit, document and
    /// instant carrying a different observation is two contradictory readings
    /// of one moment: an error the caller sees, never a silent no-op that would
    /// leave them believing the log holds what they just sent.
    #[test]
    fn a_changed_payload_at_the_same_presentation_is_an_error() {
        let (_tmp, store, scope) = store();
        let first = event("alice", 1, Some("deck"), now());
        store
            .record_access(&scope, "room-1", &first)
            .expect("record");

        let mut corrected = first.clone();
        corrected.dwell_ms = Some(40_000);
        let error = store
            .record_access(&scope, "room-1", &corrected)
            .expect_err("a changed payload is refused");
        assert!(
            format!("{error:#}").contains("already recorded with different observations"),
            "the error must say why: {error:#}"
        );

        let events = store.events_for(&scope, "room-1").expect("read");
        assert_eq!(events, vec![first], "the refusal wrote nothing");
    }

    /// Zero is not a visit. Visits are the highest sequence seen, so a row at
    /// sequence zero would leave a token that plainly came reading as
    /// never-opened — manufacturing the delivery question out of somebody who
    /// was already reading.
    #[test]
    fn a_presentation_recorded_as_visit_zero_is_refused() {
        let (_tmp, store, scope) = store();
        let error = store
            .record_access(&scope, "room-1", &event("alice", 0, None, now()))
            .expect_err("sequence zero is refused");
        assert!(
            format!("{error:#}").contains("sequence starts at 1"),
            "the error must say why: {error:#}"
        );
        assert!(store.events_for(&scope, "room-1").expect("read").is_empty());
    }

    /// A caller string that carries the id separator is refused, every one of
    /// them. U+001F is what keeps the id's components apart, and a component
    /// carrying it could fuse two different rooms', tokens' or documents'
    /// presentations into one row — the row that then reads as a retry and is
    /// dropped.
    #[test]
    fn a_separator_in_any_caller_string_is_refused() {
        let (_tmp, store, scope) = store();

        let mut fused_room = event("alice", 1, None, now());
        fused_room.room_id = "room\u{1f}1".to_string();
        assert!(store
            .record_access(&scope, "room\u{1f}1", &fused_room)
            .is_err());

        let mut fused_token = event("alice\u{1f}bob", 1, None, now());
        fused_token.room_id = "room-1".to_string();
        assert!(store.record_access(&scope, "room-1", &fused_token).is_err());

        assert!(store
            .record_access(
                &scope,
                "room-1",
                &event("alice", 1, Some("deck\u{1f}x"), now())
            )
            .is_err());

        let fused_scope = AccessScope::new("anon\u{1f}ymous", "default");
        assert!(store
            .record_access(&fused_scope, "room-1", &event("alice", 1, None, now()))
            .is_err());

        assert!(store.events_for(&scope, "room-1").expect("read").is_empty());
    }

    /// An event names the room it happened in, and filing it elsewhere would
    /// answer "what have they seen" with somebody else's history.
    #[test]
    fn an_event_filed_under_another_room_is_refused() {
        let (_tmp, store, scope) = store();
        let error = store
            .record_access(&scope, "room-2", &event("alice", 1, None, now()))
            .expect_err("mismatched room is refused");
        assert!(
            format!("{error:#}").contains("filed under"),
            "the error must say why: {error:#}"
        );
    }

    /// A blank document reference is indistinguishable from an index view, and
    /// that difference is "they looked and opened nothing" versus "they opened
    /// this" — the distinction the partial-read signal rests on.
    #[test]
    fn a_blank_document_reference_is_refused() {
        let (_tmp, store, scope) = store();
        assert!(store
            .record_access(&scope, "room-1", &event("alice", 1, Some("   "), now()))
            .is_err());
    }

    /// An unnamed audience leaves a line nobody can read without loading the
    /// room it came from, which is the coupling carrying the audience on the
    /// event exists to avoid.
    #[test]
    fn an_unnamed_audience_is_refused() {
        let (_tmp, store, scope) = store();
        let mut unnamed = event("alice", 1, None, now());
        unnamed.audience = AudienceRef::new(AudienceKind::Engagement, "  ");
        assert!(store.record_access(&scope, "room-1", &unnamed).is_err());
    }

    /// Fail closed on a read fault. An unreadable log folded to "empty" would
    /// report a thoroughly read room as never opened — the strongest signal in
    /// the feature manufactured out of a disk fault — so the read fails
    /// instead, and so does everything derived from it.
    #[test]
    fn an_unreadable_log_is_never_an_empty_one() {
        let (_tmp, store, scope) = store();
        store
            .record_access(&scope, "room-1", &event("alice", 1, None, now()))
            .expect("record");

        let path = store.room_path(&scope, "room-1");
        std::fs::write(&path, b"\xFF\xFEnot utf-8").expect("corrupt log");

        let error = store
            .events_for(&scope, "room-1")
            .expect_err("an unreadable log must not read as an empty one");
        assert!(
            format!("{error:#}")
                .contains("an unreadable log must never be treated as an empty one"),
            "the error must say why: {error:#}"
        );
        assert!(store
            .attention_snapshot(&scope, "room-1", &["alice".to_string()], &[])
            .is_err());
    }

    /// A silent room is not a healthy room. With nothing recorded, every token
    /// the room was shared with reads as `NeverOpened` and as a DELIVERY
    /// question — the vacuous read ("no events, so nobody is waiting") is the
    /// bug, and the roster is supplied precisely so an absent token is visible
    /// rather than invisible.
    #[test]
    fn an_empty_log_reads_as_never_opened_for_everyone_shared_with() {
        let (_tmp, store, scope) = store();
        let shared = vec!["alice".to_string(), "bob".to_string()];

        let attention = store
            .attention_snapshot(&scope, "room-1", &shared, &["deck".to_string()])
            .expect("snapshot");
        assert_eq!(attention.len(), 2);
        for held in &attention {
            assert_eq!(held.visits, 0);
            assert_eq!(held.presentations, 0);
            assert_eq!(held.signal(), AttentionSignal::NeverOpened);
            assert!(held.signal().is_delivery_question());
            assert!(!held.is_partial(), "never came is not a partial read");
            assert_eq!(held.documents_unopened, vec!["deck".to_string()]);
        }

        // And with nobody holding a link there is nobody to report on — which
        // is "no one to consider", not "everyone is fine".
        assert!(store
            .attention_snapshot(&scope, "room-1", &[], &["deck".to_string()])
            .expect("snapshot")
            .is_empty());
    }

    /// Index before row: a room is enumerable from its first presentation, and
    /// the index does not grow with the log. A room whose rows existed but
    /// whose id no sweep could enumerate is a room where nothing ever ripens.
    #[test]
    fn the_index_names_a_room_once_from_its_first_presentation() {
        let (_tmp, store, scope) = store();
        assert!(store.rooms_with_access(&scope).expect("index").is_empty());

        for sequence in 1..=3 {
            store
                .record_access(
                    &scope,
                    "room-1",
                    &event(
                        "alice",
                        sequence,
                        None,
                        now() + Duration::hours(sequence as i64),
                    ),
                )
                .expect("record");
        }
        let mut second = event("bob", 1, None, now());
        second.room_id = "room-2".to_string();
        store
            .record_access(&scope, "room-2", &second)
            .expect("record");

        assert_eq!(
            store.rooms_with_access(&scope).expect("index"),
            vec!["room-1".to_string(), "room-2".to_string()],
            "one entry per room however many presentations it holds"
        );
    }

    /// The index may over-promise, never under-deliver. A record torn between
    /// the index write and the row leaves a room whose log is empty, and that
    /// resolves to no events — a wasted read, corrected by the retry — rather
    /// than an error or a room nobody looks at again.
    #[test]
    fn a_dangling_index_entry_resolves_to_no_events() {
        let (_tmp, store, scope) = store();
        store
            .record_access(&scope, "room-1", &event("alice", 1, None, now()))
            .expect("record");
        // The index half of a record that crashed before its row.
        store
            .append_room_index(&scope, "room-torn")
            .expect("index only");

        assert_eq!(
            store.rooms_with_access(&scope).expect("index"),
            vec!["room-1".to_string(), "room-torn".to_string()]
        );
        assert!(store
            .events_for(&scope, "room-torn")
            .expect("an unwritten room is empty, not an error")
            .is_empty());
    }

    /// Two tenants recording against the same room id do not see each other's
    /// history, and neither resumes the other's rows.
    #[test]
    fn presentations_are_scoped() {
        let (_tmp, store, scope) = store();
        let other = AccessScope::new("someone-else", "default");
        let presented = event("alice", 1, Some("deck"), now());

        let mine = store
            .record_access(&scope, "room-1", &presented)
            .expect("record");
        let theirs = store
            .record_access(&other, "room-1", &presented)
            .expect("record");

        assert_ne!(mine.access_id, theirs.access_id);
        assert_eq!(store.events_for(&scope, "room-1").expect("read").len(), 1);
        assert_eq!(store.events_for(&other, "room-1").expect("read").len(), 1);
        assert_eq!(
            store.rooms_with_access(&scope).expect("index"),
            vec!["room-1".to_string()]
        );
    }
}
