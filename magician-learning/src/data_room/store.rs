//! Durable data rooms — plan phase 1.
//!
//! Append-only, like the other stores in this set: the first line opens the
//! room, each later line is a change, and the current room is the fold. A room's
//! history is the thing an audit reads, so nothing is edited in place.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceRef};
use magician::magician_v2::evidence::{OutwardActDisclosure, OutwardAssertionStore, OutwardScope};

use super::disclosure_bridge::record_room_disclosures;
use super::types::{DataRoom, DocumentEntry, DocumentVisibility, OpenDataRoom};

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRoomScope {
    pub principal: String,
    pub workspace: String,
}

impl DataRoomScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What a room mutation needs in order to **account for** the grant it is
/// about to make.
///
/// Making a document visible to someone holding a live link is a disclosure —
/// the outward-assertions register's own write point for a room
/// (`super::disclosure_bridge`). This carries the three things that write point
/// cannot discover for itself, so the room keeps the decoupling the bridge
/// documents: the roster belongs to whoever owns the relationship, and the
/// holder list to whoever issues links. A room that read either store directly
/// would tie every caller to that one source.
///
/// Generic by construction: nothing here names a kind of deal, a document
/// class, or a flow. Any caller that can answer *"whose relationship is this,
/// and who can get in right now"* can grant through it.
pub struct GrantDisclosure<'a> {
    /// The one authoritative copy of what was told to whom. Consumed, never
    /// owned: a second copy is how a disclosure ends up recorded in one
    /// subsystem and missing from another.
    pub assertions: &'a OutwardAssertionStore,
    /// The roster of the relationship the room was opened for. A roster for
    /// any **other** relationship is an error rather than an absence — see
    /// [`record_room_disclosures`] — because recording against it would
    /// attribute confidential disclosures to the wrong counterparty.
    pub audience: &'a Audience,
    /// The identities holding a **live** share link right now.
    ///
    /// Empty is an honest absence — no link, no grant to account for — and
    /// never a licence: every act that IS recorded names exactly one holder,
    /// so nothing produced here can satisfy a recipient predicate vacuously.
    pub holders: &'a [String],
    /// Who granted it. Blank is refused by the bridge: `effective_sender` is
    /// how the register answers *who told them*, and a blank one would record
    /// an assertion nobody made.
    pub disclosed_by: &'a str,
}

impl GrantDisclosure<'_> {
    /// Record one disclosure per `(present document, admitted holder)` of
    /// `room`.
    ///
    /// `room` is the room **as it will be** once the mutation lands, never as
    /// the caller wishes it were: recording against a state the room will not
    /// reach would put an act in the register claiming a visibility that never
    /// existed.
    ///
    /// The outward scope is derived from the room's scope rather than supplied
    /// beside it. They are the same principal and workspace by definition, and
    /// a caller free to hand in a different one could file a room's
    /// disclosures where no reverse lookup for that room will ever go.
    ///
    /// Likewise the two forms of `now` are one instant: the register speaks
    /// RFC 3339 strings and the room speaks a typed clock, so the string is
    /// derived here from the caller's own `now`. Taking both from the caller
    /// would let two different moments arrive as one grant, and nothing
    /// downstream could tell.
    fn record(
        &self,
        scope: &DataRoomScope,
        room: &DataRoom,
        now: DateTime<Utc>,
    ) -> Result<Vec<OutwardActDisclosure>> {
        // The holder is one component of the grant's idempotency key, which is
        // separator-joined. A holder carrying the separator could shift bytes
        // across it and fuse two people's disclosures into one record — after
        // which "who saw this" has a wrong answer rather than a missing one.
        if let Some(holder) = self.holders.iter().find(|held| held.contains(FIELD_SEP)) {
            anyhow::bail!(
                "link holder `{}` contains U+001F: it is the separator the grant's idempotency \
                 key is built from, and a holder carrying it could fuse two people's \
                 disclosures into one record",
                holder.escape_debug()
            );
        }
        record_room_disclosures(
            self.assertions,
            &OutwardScope::new(scope.principal.clone(), scope.workspace.clone()),
            room,
            self.audience,
            self.holders,
            self.disclosed_by,
            now,
            &now.to_rfc3339(),
        )
    }
}

/// One line in a room's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum RoomRecord {
    Opened(DataRoom),
    DocumentAdded(DocumentEntry),
    DocumentWithdrawn {
        artifact_ref: String,
        at: DateTime<Utc>,
    },
    Closed {
        at: DateTime<Utc>,
        by: String,
    },
}

/// Durable store for data rooms.
#[derive(Debug, Clone)]
pub struct DataRoomStore {
    workspace_layout: ArtifactV2Workspace,
}

impl DataRoomStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &DataRoomScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("data_rooms")
    }

    fn room_path(&self, scope: &DataRoomScope, room_id: &str) -> PathBuf {
        self.root(scope).join(format!("{room_id}.jsonl"))
    }

    /// Open a room for an audience.
    ///
    /// The id derives from the audience, so **one audience has one room**.
    /// Re-opening returns the existing room rather than creating a second: two
    /// rooms for one audience would mean two answers to "what have they seen",
    /// and the wrong one would be whichever a caller happened to reach.
    ///
    /// The audience's **kind** is part of that identity, so the same company as
    /// a live deal and as a standing client get different rooms — the thing
    /// widening this binding could most easily have broken.
    pub fn open(
        &self,
        scope: &DataRoomScope,
        request: &OpenDataRoom,
        now: DateTime<Utc>,
    ) -> Result<DataRoom> {
        if !request.audience.is_named() {
            anyhow::bail!(
                "a data room is the document channel OF an audience; without one there is \
                 nothing for access to derive from"
            );
        }

        let room_id = derive_room_id(scope, &request.audience);
        if let Some(existing) = self.load(scope, &room_id)? {
            return Ok(existing);
        }

        let room = DataRoom {
            room_id: room_id.clone(),
            audience: request.audience.clone(),
            documents: Vec::new(),
            opened_at: now,
            opened_by: request.opened_by.clone(),
            closes_at: request.closes_at,
            closed_at: None,
        };
        self.append(
            &self.room_path(scope, &room_id),
            &RoomRecord::Opened(room.clone()),
        )?;
        Ok(room)
    }

    /// The room for an audience, if one has been opened.
    pub fn for_audience(
        &self,
        scope: &DataRoomScope,
        audience: &AudienceRef,
    ) -> Result<Option<DataRoom>> {
        self.load(scope, &derive_room_id(scope, audience))
    }

    /// Put a reference in the room — the visibility grant, and the one place a
    /// document becomes visible to a link holder.
    ///
    /// Idempotent on `artifact_ref`: adding the same document twice returns the
    /// room unchanged rather than listing it twice. **Re-adding a withdrawn
    /// document restores it**, which is what "add" plainly means — and the
    /// withdrawal stays in the log, so the history still says it was out.
    ///
    /// # Record the disclosure, then grant — or grant nothing
    ///
    /// The order is `grant.record(...)` **then** the `DocumentAdded` append,
    /// and it is not a preference. If recording fails, the append never runs
    /// and there is no grant at all: a grant that succeeded while its record
    /// failed would be a disclosure nobody can account for — the exact state
    /// the outward-assertions register exists to make impossible, and the
    /// reason `record_room_disclosures` tells its callers to fail closed. The
    /// reverse order cannot give that. A crash between a successful append and
    /// a failed record leaves the document visible and unrecorded, and no
    /// later sweep can distinguish that room from one nobody was ever shown.
    ///
    /// The recorded room is the room **as it will be**, and never more: on the
    /// idempotent path — the document is already present, so nothing will be
    /// appended — the recording runs against the room unchanged, so no act can
    /// claim a visibility the room never took on. Recording still happens on
    /// that path, because the holder list may have grown since the document
    /// was added: someone who gained a live link yesterday is being granted
    /// visibility of a document that was already there, and that is a
    /// disclosure like any other.
    ///
    /// Restoring a withdrawn document **resumes** its original acts rather
    /// than opening second ones: the grant's identity is
    /// `(room, artifact ref, holder)`, and the same document shown to the same
    /// person through the same room is the same disclosure. The withdrawal and
    /// the restoration both stay in this log, which is where the gap is
    /// legible.
    #[allow(clippy::too_many_arguments)]
    pub fn add_document(
        &self,
        scope: &DataRoomScope,
        room_id: &str,
        artifact_ref: &str,
        visibility: DocumentVisibility,
        added_by: &str,
        grant: &GrantDisclosure<'_>,
        now: DateTime<Utc>,
    ) -> Result<DataRoom> {
        if artifact_ref.trim().is_empty() {
            anyhow::bail!("a document entry must name an artifact reference");
        }
        // The reference is one component of the grant's separator-joined
        // idempotency key. One carrying the separator could shift bytes across
        // it and fuse two documents' disclosures into a single record, which
        // gives "what did they see" a wrong answer rather than a missing one.
        if artifact_ref.contains(FIELD_SEP) {
            anyhow::bail!(
                "a document's artifact reference must not contain U+001F: it is the separator \
                 the grant's idempotency key is built from, and a reference carrying it could \
                 fuse two different documents' disclosures into one record"
            );
        }
        // §4: a room references an immutable REVISION, never "latest".
        //
        // Refused at the write, because there is nowhere later to catch it: a
        // room holding a moving pointer serves whoever opens it whatever the
        // artifact says today, and a reader comparing what they were shown
        // against what was cleared has no way to tell they differ. Stale is
        // visible; swapped is not, which is why the plan calls the second one
        // worse.
        //
        // It also makes correction propagation ANSWERABLE. A retraction asks
        // which rooms carry the affected revision; against an unpinned
        // reference the honest answer is "we cannot tell", and a correction
        // that cannot tell is one somebody has to chase by hand.
        if !super::types::names_a_revision(artifact_ref) {
            let separators = artifact_ref.matches(super::types::REVISION_SEP).count();
            if separators > 1 {
                anyhow::bail!(
                    "`{artifact_ref}` carries {separators} `{}` separators, so which part names \
                     the revision is a guess. Reading the last one would make an unpinned \
                     reference that merely contains a separator look pinned to a revision that \
                     matches nothing — and a retraction would then silently fail to flag this \
                     room. Spell the artifact half without a separator",
                    super::types::REVISION_SEP
                );
            }
            anyhow::bail!(
                "a document reference must name a revision, as `{artifact_ref}{}<revision>`. \
                 An unpinned reference serves whatever the artifact says at read time, so a \
                 room would silently show a different document than the one that was cleared — \
                 and a later retraction could not say whether this room carries the claim",
                super::types::REVISION_SEP
            );
        }
        let Some(room) = self.load(scope, room_id)? else {
            anyhow::bail!("no data room `{room_id}`");
        };
        // A closed room takes nothing. Adding to one would make it look, later,
        // as though a document was available when it was not.
        if !room.standing(now).is_open() {
            anyhow::bail!(
                "data room `{room_id}` is {} and cannot take documents",
                room.standing(now).as_str()
            );
        }
        let already_present = room
            .documents
            .iter()
            .any(|entry| entry.artifact_ref == artifact_ref && entry.is_present());

        let entry = DocumentEntry {
            artifact_ref: artifact_ref.to_string(),
            visibility,
            added_at: now,
            added_by: added_by.to_string(),
            withdrawn_at: None,
        };
        // The room as it WILL BE. On the idempotent path nothing is appended,
        // so the room as it will be is the room as it is — folding the entry
        // in anyway would record a visibility this call is not about to grant.
        let granted = if already_present {
            room.clone()
        } else {
            let mut next = room.clone();
            match next
                .documents
                .iter_mut()
                .find(|held| held.artifact_ref == entry.artifact_ref)
            {
                Some(held) => *held = entry.clone(),
                None => next.documents.push(entry.clone()),
            }
            next
        };
        // Record before the grant takes effect. On `Err` nothing below runs,
        // so the room is left exactly as it was — see the ordering note above.
        grant.record(scope, &granted, now)?;

        if already_present {
            return Ok(room);
        }

        self.append(
            &self.room_path(scope, room_id),
            &RoomRecord::DocumentAdded(entry),
        )?;
        self.load(scope, room_id)?
            .context("data room vanished immediately after a document was added")
    }

    /// Take a reference out of the room.
    ///
    /// The entry is **kept and marked**, never deleted: *"this was in the room
    /// between March and April"* is the question an audit asks, and a deleted
    /// row cannot answer it.
    pub fn withdraw_document(
        &self,
        scope: &DataRoomScope,
        room_id: &str,
        artifact_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<DataRoom> {
        let Some(room) = self.load(scope, room_id)? else {
            anyhow::bail!("no data room `{room_id}`");
        };
        if !room
            .documents
            .iter()
            .any(|entry| entry.artifact_ref == artifact_ref && entry.is_present())
        {
            return Ok(room);
        }
        self.append(
            &self.room_path(scope, room_id),
            &RoomRecord::DocumentWithdrawn {
                artifact_ref: artifact_ref.to_string(),
                at: now,
            },
        )?;
        self.load(scope, room_id)?
            .context("data room vanished immediately after a withdrawal")
    }

    /// Close the room deliberately.
    ///
    /// Idempotent. Closing keeps every document entry: what was in the room is
    /// exactly what an owner needs after closing it.
    pub fn close(
        &self,
        scope: &DataRoomScope,
        room_id: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<DataRoom> {
        let Some(room) = self.load(scope, room_id)? else {
            anyhow::bail!("no data room `{room_id}`");
        };
        if room.closed_at.is_some() {
            return Ok(room);
        }
        self.append(
            &self.room_path(scope, room_id),
            &RoomRecord::Closed {
                at: now,
                by: by.to_string(),
            },
        )?;
        self.load(scope, room_id)?
            .context("data room vanished immediately after being closed")
    }

    /// Every room in a scope, oldest-opened first.
    ///
    /// The owner-facing answer to *"what have I opened"*. Without it the only
    /// way to reach a room is to already know the audience it was opened for,
    /// which is not a listing — it is a lookup for somebody who did not need
    /// one.
    ///
    /// # Absent is empty; unreadable is not
    ///
    /// The directory walk is [`magician::magician_v2::jsonl::list_log_paths`],
    /// which maps only a missing directory to an empty list — a scope that has
    /// never opened a room — and propagates every other listing fault. A room
    /// log that will not fold propagates too: a listing that silently dropped
    /// the room it could not read would tell an owner they have shared less
    /// than they have, which is the fail-open this whole set exists to close.
    pub fn list(&self, scope: &DataRoomScope) -> Result<Vec<DataRoom>> {
        let paths = magician::magician_v2::jsonl::list_log_paths(
            &self.workspace_layout,
            &self.root(scope),
        )?;

        let mut rooms = Vec::new();
        for path in paths {
            // The id is the file stem this store itself wrote, so nothing a
            // caller supplied is being interpolated back into a path here.
            let Some(room_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if let Some(room) = self.load(scope, room_id)? {
                rooms.push(room);
            }
        }
        // Opened order, then id: two rooms opened in the same instant still
        // list in one fixed order, so two listings of an unchanged scope agree.
        rooms.sort_by(|left, right| {
            left.opened_at
                .cmp(&right.opened_at)
                .then_with(|| left.room_id.cmp(&right.room_id))
        });
        Ok(rooms)
    }

    /// The folded room.
    pub fn load(&self, scope: &DataRoomScope, room_id: &str) -> Result<Option<DataRoom>> {
        let path = self.room_path(scope, room_id);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(None);
        };

        let mut room: Option<DataRoom> = None;
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in magician::magician_v2::jsonl::parse_log_lines::<RoomRecord>(&raw, &path)? {
            match record {
                RoomRecord::Opened(opened) => room = Some(opened),
                RoomRecord::DocumentAdded(entry) => {
                    if let Some(current) = room.as_mut() {
                        // Re-adding a withdrawn document restores it, which is
                        // what "add" means. The earlier withdrawal stays in the
                        // log, so the history still records the gap.
                        match current
                            .documents
                            .iter_mut()
                            .find(|held| held.artifact_ref == entry.artifact_ref)
                        {
                            Some(held) => *held = entry,
                            None => current.documents.push(entry),
                        }
                    }
                },
                RoomRecord::DocumentWithdrawn { artifact_ref, at } => {
                    if let Some(current) = room.as_mut() {
                        if let Some(held) = current
                            .documents
                            .iter_mut()
                            .find(|held| held.artifact_ref == artifact_ref)
                        {
                            held.withdrawn_at = Some(at);
                        }
                    }
                },
                RoomRecord::Closed { at, .. } => {
                    if let Some(current) = room.as_mut() {
                        current.closed_at = Some(at);
                    }
                },
            }
        }
        Ok(room)
    }

    fn append(&self, path: &PathBuf, record: &RoomRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        magician::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" fails open —
        // guards pass vacuously and removals report success while removing
        // nothing. Shared semantics live in `magician_v2::jsonl`.
        magician::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// One audience, one room.
///
/// Derived rather than allocated, so the property holds without anyone checking
/// it: two rooms for one audience would mean two answers to "what have they
/// seen", and the wrong one would be whichever a caller reached.
///
/// Folds in [`AudienceRef::as_key`], which carries the **kind** as well as the
/// id — so `engagement:acme` and `account:acme` are different rooms. Keying on
/// the id alone would merge two relationships that happen to share a name, which
/// is the specific way widening this binding could have gone wrong.
fn derive_room_id(scope: &DataRoomScope, audience: &AudienceRef) -> String {
    format!(
        "room-{}",
        &blake3::hash(
            format!(
                "{}{FIELD_SEP}{}{FIELD_SEP}{}",
                scope.principal,
                scope.workspace,
                audience.as_key()
            )
            .as_bytes()
        )
        .to_hex()[..32]
    )
}
