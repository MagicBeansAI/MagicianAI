//! What the room knows, in the words a cycle reads — deal-close plan §6.
//!
//! Plan: `docs/plans/2026-08-07-opc-deal-close.md` §6, *"What is derived"*.
//!
//! §6 is explicit that the four derived states *"are what the owning agent's
//! cycle reads"*, and [`super::access_log`] derives them faithfully. Exactly
//! one thing consumes them — [`super::follow_ups`], which phrases them as
//! obligations — and in doing so it deliberately throws the intelligence away:
//! `OpenedOnce` and `OpenedRepeatedly` collapse into one sentence, and the
//! partial read disappears entirely. That is correct **there**, because the
//! obligation text is part of the register's identity tuple and anything
//! varying inside it would mint duplicate rows on every visit.
//!
//! The consequence is that the register carries *"follow up, they opened it and
//! did not reply"* and nothing anywhere carries §6's own opening sentence:
//!
//! > *"Opened the deck three times, never opened the financials"*
//!
//! This module is the second consumer, and it carries exactly that.
//!
//! # It follows the register, it does not compete with it
//!
//! The path a signal reaches an agent's cycle by is already decided:
//! [`ObligationStore::lapsed_across`](magician::magician_v2::obligations::ObligationStore::lapsed_across)
//! is documented as *"what an agent's cycle surfaces"*, and `super::sweep`
//! already puts the room's follow-ups there. So a note here is **not a second
//! queue**. Each one carries the
//! [`obligation_id`](AttentionNote::obligation_id) of the register row it
//! explains, so a cycle reading its book joins the two by id and needs no
//! second lookup path, no second ordering and no second idea of what is
//! outstanding.
//!
//! The id is obtained by running the sweep's own derivation
//! ([`derive_follow_ups`]) and the register's own
//! ([`obligation_id_for`]) — never by rebuilding either tuple here. A
//! re-derivation that drifted by one character would attach every note to a row
//! that does not exist, which reads exactly like a room nobody has looked at.
//!
//! # Pure, and it reads nothing
//!
//! Snapshots in, notes out. Same refusal as [`super::follow_ups`] and for the
//! same reason: whether they replied is not a fact the room has, so it arrives
//! as [`TokenContext::replied`] from whoever holds the correspondence. A module
//! that read an inbox would only ever serve flows on that channel.
//!
//! # Prose here, never in the register
//!
//! [`AttentionNote::headline`] is a sentence for a reader. It carries counts
//! and it is deterministic, but it is **presentation** and must never be
//! recorded: the register's `what` text is part of its identity tuple, and a
//! text carrying a visit count would mint a fresh obligation on every visit —
//! the flooding failure `super::follow_ups` is arranged to prevent. The
//! headline names the room's **stable id** for the same reason the recorded
//! text does; a display layer that holds the human label may substitute it when
//! it renders.
//!
//! # Generic first
//!
//! Nothing here names a domain. *"We shared a bounded set of documents with a
//! bounded set of people, and this is what they did with them"* is the same
//! shape for cohort materials, a client's deliverable pack, an audit request or
//! somebody's own records. The word "deck" appears in the plan's sentence and
//! nowhere in this API.

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::store::obligation_id_for;
use magician::magician_v2::obligations::ObligationScope;

use super::access_log::AttentionSignal;
use super::follow_ups::{derive_follow_ups, FollowUpPolicy, TokenContext};

/// The separator that keeps a derived id's components apart.
///
/// Restated rather than imported, exactly as `super::sweep` and
/// `scheduling::consumer` restate it: every module that lets caller text reach
/// a derivation has to refuse it itself, and borrowing somebody else's constant
/// would hide this module's refusal behind another module's invariant.
const FIELD_SEP: char = '\u{1f}';

/// What the room can say about one token, once reply knowledge is joined in.
///
/// This is §6's four states, and it is deliberately a **different type** from
/// [`AttentionSignal`]. The access log has three states because the fourth,
/// *"opened once, then silence"*, needs to know whether they replied — which
/// happens outside the room entirely. Adding it there would have meant the log
/// asserting knowledge it does not have. Here the join has been made, so the
/// fourth state exists and is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReading {
    /// Shared, and the token was never presented.
    ///
    /// §6: *"the one that earns the whole feature."* It is a **delivery
    /// question**, not a nudge — mail from a shared sending domain can land in
    /// spam while every layer reports success, and this is the first evidence
    /// the system can produce that a message did not arrive. Chasing somebody
    /// who never received it is the wrong act and reads as pestering.
    NeverOpened,
    /// Presented once, and nothing came back. §6's *"opened once, then
    /// silence"* — *"a real signal; different from never opened."*
    ReadThenSilent,
    /// They came back to it, and nothing came back to us. Interest, or
    /// confusion — §6 says it is *"worth asking which"*, and the visit count in
    /// the note is what lets an agent ask.
    ReturnedThenSilent,
    /// They replied, on whatever channel. There is nothing owed and no register
    /// row: carried so a cycle can tell a live conversation from a quiet one
    /// rather than inferring silence from the absence of a note.
    Answered,
}

impl AttentionReading {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverOpened => "never_opened",
            Self::ReadThenSilent => "read_then_silent",
            Self::ReturnedThenSilent => "returned_then_silent",
            Self::Answered => "answered",
        }
    }

    /// Whether this is a question about delivery rather than about them.
    ///
    /// Mirrors [`AttentionSignal::is_delivery_question`] so the two cannot
    /// disagree about which state is a delivery question.
    pub fn is_delivery_question(self) -> bool {
        matches!(self, Self::NeverOpened)
    }

    /// Whether anything is owed on this reading.
    ///
    /// False for [`Answered`](Self::Answered) alone. A cycle that surfaced
    /// answered rooms as work would ask the owner to chase a live conversation.
    pub fn needs_action(self) -> bool {
        !matches!(self, Self::Answered)
    }

    /// Sort rank — never-opened first, answered last.
    ///
    /// Never-opened leads because it is the only signal in the whole set that
    /// suggests a message did not arrive, and it is actionable in a way none of
    /// the others are: everything else can wait a day, and a message sitting in
    /// somebody's spam folder cannot.
    fn rank(self) -> u8 {
        match self {
            Self::NeverOpened => 0,
            Self::ReturnedThenSilent => 1,
            Self::ReadThenSilent => 2,
            Self::Answered => 3,
        }
    }
}

/// One room's attention, as the caller knows it.
///
/// The roster is inside [`TokenContext`], which is the shape
/// [`super::sweep::snapshot_from_events`] already produces — so a caller that
/// swept the register has the input for this with no extra assembly, and the
/// two cannot see different rosters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedRoom {
    pub audience: AudienceRef,
    /// The room's **stable identifier**, never its display label. It reaches
    /// the derivation that produces the obligation id, so a mutable label here
    /// would attach notes to rows that do not exist.
    pub room_id: String,
    pub tokens: Vec<TokenContext>,
}

/// One thing the room knows, for the agent whose book it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionNote {
    /// The register row this note explains, when the sweep has raised one.
    ///
    /// `None` in three honest cases, and the caller can tell them apart from
    /// [`reading`](Self::reading): they replied so nothing is owed; the waiting
    /// window has not closed yet; or the room recorded a visit with no time,
    /// which has no ripening instant to derive from and so yields no row.
    ///
    /// Obtained from the sweep's own derivation, never rebuilt here.
    pub obligation_id: Option<String>,
    pub audience: AudienceRef,
    pub room_id: String,
    /// The identity the token was **issued to** — not a claim about who
    /// presented it. Links get forwarded, and this module inherits that refusal
    /// verbatim from the access log.
    pub token_issued_to: String,
    pub reading: AttentionReading,
    /// Distinct visits, from the highest `sequence` seen — **not** the event
    /// count. One sitting that views the index and then opens a document is two
    /// events and one visit, and reporting it as a return would turn a single
    /// read into evidence of interest.
    pub visits: u32,
    /// Raw presentations. Three page views in one visit is a different fact
    /// from one, and neither is the visit count.
    pub presentations: usize,
    pub index_views: usize,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    /// Documents they reached, out of what is in the room now.
    pub documents_opened: Vec<String>,
    /// Documents in the room they never reached — §6's *"partial"*: it *"tells
    /// you what the next conversation is about."*
    pub documents_unopened: Vec<String>,
    /// A sentence for a reader. **Presentation, never identity** — see the
    /// module note. Never record it.
    pub headline: String,
}

impl AttentionNote {
    /// Whether some of the room was read and some was not.
    ///
    /// False when nothing was opened: *"they never came"* is the delivery
    /// question, not a partial read, and conflating the two would turn the
    /// strongest signal in the set into a content observation.
    pub fn is_partial(&self) -> bool {
        !self.documents_opened.is_empty() && !self.documents_unopened.is_empty()
    }
}

/// The notes for an agent's whole book of rooms.
///
/// # What it guarantees
///
/// - **One note per token per room.** A duplicated token contributes once,
///   first occurrence wins — the same rule [`derive_follow_ups`] applies, so
///   the note and the register row it points at cannot disagree about which
///   share instant speaks for a token.
/// - **A ghost token survives.** A token the room was shared with that appears
///   in no event is exactly the never-opened case, and it is the note that
///   earns the feature. It is present with zero visits rather than filtered out
///   for having no history.
/// - **The obligation id comes from the sweep's derivation.** Never rebuilt
///   here; see the module note.
/// - **Deterministic order.** Never-opened first, then returned-then-silent,
///   then read-then-silent, then answered; within a reading, most-visited
///   first, then by room and token. A cycle reads the delivery questions
///   before the nudges, which is the order §6 argues for.
///
/// # What it refuses
///
/// Anything that would produce an id no register row can match: a scope,
/// audience, room id or token carrying `U+001F`. That character is what keeps a
/// derived id's components apart, so a value containing it can fuse two rooms'
/// follow-ups into one id — attaching a note to somebody else's obligation, and
/// silently pointing an agent at the wrong counterparty.
///
/// An unnamed audience or a blank room id is refused for the same reason: the
/// register refuses them at the write, so a note pairing one would carry a
/// handle for a row that can never exist.
///
/// # What an empty result means
///
/// *"No one holds a link to any of these rooms"* — never *"everybody is fine"*.
/// An empty book yields an empty list, and reading that as health is the
/// vacuous-truth bug this codebase refuses everywhere.
pub fn attention_notes(
    scope: &ObligationScope,
    rooms: &[SharedRoom],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Result<Vec<AttentionNote>> {
    refuse_separator("the scope's principal", &scope.principal)?;
    refuse_separator("the scope's workspace", &scope.workspace)?;

    let mut notes = Vec::new();
    for room in rooms {
        let room_id = room.room_id.trim();
        if room_id.is_empty() {
            anyhow::bail!(
                "a note must name the room it is about: the register refuses an unnamed room at \
                 the write, so a note paired with one would carry a handle for a row that can \
                 never exist"
            );
        }
        if room.audience.id.trim().is_empty() {
            anyhow::bail!(
                "room `{room_id}` names an empty audience: the register refuses an unnamed \
                 audience, so every id derived for it would address nothing"
            );
        }
        refuse_separator("an audience id", &room.audience.id)?;
        refuse_separator("a room id", room_id)?;

        let mut seen: HashSet<&str> = HashSet::new();
        for token in &room.tokens {
            let token_issued_to = token.attention.token_issued_to.as_str();
            // First occurrence wins, matching `derive_follow_ups` exactly: two
            // contexts for one token would otherwise produce two notes pointing
            // at two different register rows for one person.
            if !seen.insert(token_issued_to) {
                continue;
            }
            refuse_separator("a token", token_issued_to)?;

            let reading = reading_for(token);
            // Run the sweep's own derivation over this single token, so the id
            // is the register's and the ripening rule is not restated here. A
            // token that has not ripened, or that replied, yields no row — and
            // no id, which is the honest answer.
            let obligation_id = derive_follow_ups(
                &room.audience,
                room_id,
                std::slice::from_ref(token),
                policy,
                now,
            )
            .first()
            .map(|request| obligation_id_for(scope, request));

            notes.push(AttentionNote {
                obligation_id,
                audience: room.audience.clone(),
                room_id: room_id.to_string(),
                token_issued_to: token_issued_to.to_string(),
                reading,
                visits: token.attention.visits,
                presentations: token.attention.presentations,
                index_views: token.attention.index_views,
                first_seen: token.attention.first_seen,
                last_seen: token.attention.last_seen,
                documents_opened: token.attention.documents_opened.clone(),
                documents_unopened: token.attention.documents_unopened.clone(),
                headline: headline_for(room_id, token_issued_to, reading, token),
            });
        }
    }

    notes.sort_by(|left, right| {
        left.reading
            .rank()
            .cmp(&right.reading.rank())
            // Most-visited first within a reading: five returns is a stronger
            // signal than two, and an agent reading top-down should meet it
            // first.
            .then_with(|| right.visits.cmp(&left.visits))
            .then_with(|| left.room_id.cmp(&right.room_id))
            .then_with(|| left.token_issued_to.cmp(&right.token_issued_to))
    });
    Ok(notes)
}

/// The notes that are somebody's next action.
///
/// [`AttentionReading::Answered`] removed, nothing else. Split out rather than
/// folded into a flag on [`attention_notes`] because both lists are wanted: a
/// cycle acts on this one, and a person asking *"where does this relationship
/// stand"* wants the answered rooms in the picture too.
pub fn actionable(notes: &[AttentionNote]) -> Vec<&AttentionNote> {
    notes
        .iter()
        .filter(|note| note.reading.needs_action())
        .collect()
}

// ── Internals ───────────────────────────────────────────────────────────────

fn refuse_separator(what: &str, value: &str) -> Result<()> {
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} contains U+001F, the separator that keeps a derived id's components apart: a \
             value carrying it can fuse two rooms' follow-ups into one id, attaching this note to \
             somebody else's obligation"
        );
    }
    Ok(())
}

/// Join the room's signal with the reply knowledge the caller supplied.
///
/// A reply wins over everything, the never-opened case included. A token that
/// never opened the room cannot have replied *through* it — but replies arrive
/// by mail, by phone, in meetings, and a reply proves the message landed, so
/// the delivery question is moot. Same precedence [`derive_follow_ups`] applies
/// when it suppresses on `replied`, so a note and a register row cannot
/// disagree about whether a conversation is live.
fn reading_for(token: &TokenContext) -> AttentionReading {
    if token.replied {
        return AttentionReading::Answered;
    }
    match token.attention.signal() {
        AttentionSignal::NeverOpened => AttentionReading::NeverOpened,
        AttentionSignal::OpenedOnce => AttentionReading::ReadThenSilent,
        AttentionSignal::OpenedRepeatedly => AttentionReading::ReturnedThenSilent,
    }
}

/// §6's sentence, built from what the room actually observed.
///
/// Deterministic and count-bearing. **Presentation only** — it carries a visit
/// count, which is exactly what must never enter the register's identity tuple.
fn headline_for(
    room_id: &str,
    token_issued_to: &str,
    reading: AttentionReading,
    token: &TokenContext,
) -> String {
    let opened = token.attention.documents_opened.len();
    let unopened = token.attention.documents_unopened.len();
    let mut text = match reading {
        AttentionReading::NeverOpened => format!(
            "{token_issued_to} was sent room '{room_id}' and has never opened it: check the link \
             arrived before chasing a reply"
        ),
        AttentionReading::ReadThenSilent => {
            format!("{token_issued_to} opened room '{room_id}' once and has not replied")
        },
        AttentionReading::ReturnedThenSilent => format!(
            "{token_issued_to} has opened room '{room_id}' {} times and has not replied",
            token.attention.visits
        ),
        AttentionReading::Answered => {
            format!("{token_issued_to} has replied on room '{room_id}'")
        },
    };
    // §6's own example of the partial read: what they did NOT open is what the
    // next conversation is about.
    if opened > 0 && unopened > 0 {
        text.push_str(&format!(
            "; opened {opened} of {} documents, {unopened} never reached",
            opened + unopened
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    //! §6's derived states, as behaviour on the agent's side of the room.

    use chrono::{Duration, TimeZone};

    use crate::data_room::access_log::TokenAttention;
    use magician::magician_v2::audience::AudienceKind;

    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, day, 12, 0, 0).unwrap()
    }

    fn audience() -> AudienceRef {
        AudienceRef {
            kind: AudienceKind::Engagement,
            id: "engagement-1".to_string(),
        }
    }

    fn scope() -> ObligationScope {
        ObligationScope::new("owner", "work")
    }

    fn policy() -> FollowUpPolicy {
        FollowUpPolicy::new(Duration::days(3), Duration::days(4)).expect("positive windows")
    }

    fn token(
        who: &str,
        visits: u32,
        opened: &[&str],
        unopened: &[&str],
        last_seen: Option<DateTime<Utc>>,
    ) -> TokenContext {
        TokenContext {
            attention: TokenAttention {
                token_issued_to: who.to_string(),
                visits,
                presentations: opened.len() + visits as usize,
                first_seen: last_seen,
                last_seen,
                documents_opened: opened.iter().map(|d| d.to_string()).collect(),
                documents_unopened: unopened.iter().map(|d| d.to_string()).collect(),
                index_views: 1,
            },
            shared_at: at(1),
            replied: false,
        }
    }

    fn room(tokens: Vec<TokenContext>) -> SharedRoom {
        SharedRoom {
            audience: audience(),
            room_id: "room-7".to_string(),
            tokens,
        }
    }

    /// §6's opening sentence has to survive to the agent.
    ///
    /// The register collapses `OpenedOnce` and `OpenedRepeatedly` into one text
    /// and drops the partial read entirely, because its `what` is part of an
    /// identity tuple. So "they came back three times and never opened the
    /// second document" existed nowhere: the agent saw "opened, no reply" and
    /// could not tell a skim from a study.
    #[test]
    fn a_repeated_visit_and_an_unread_document_both_reach_the_note() {
        let notes = attention_notes(
            &scope(),
            &[room(vec![token(
                "partner@example.test",
                3,
                &["overview"],
                &["numbers"],
                Some(at(2)),
            )])],
            &policy(),
            at(10),
        )
        .expect("notes");

        assert_eq!(notes.len(), 1);
        let note = &notes[0];
        assert_eq!(note.reading, AttentionReading::ReturnedThenSilent);
        assert_eq!(note.visits, 3);
        assert_eq!(note.documents_opened, vec!["overview".to_string()]);
        assert_eq!(note.documents_unopened, vec!["numbers".to_string()]);
        assert!(note.is_partial());
        assert_eq!(
            note.headline,
            "partner@example.test has opened room 'room-7' 3 times and has not replied; opened 1 \
             of 2 documents, 1 never reached"
        );
    }

    /// A note is not a second queue: it points at the register row the sweep
    /// already raised.
    ///
    /// If the id were rebuilt here rather than taken from the sweep's own
    /// derivation, a one-character drift would attach every note to a row that
    /// does not exist — which reads exactly like a room nobody has looked at,
    /// and the agent would act on the wrong thing while every layer reported
    /// success.
    #[test]
    fn a_ripened_note_carries_the_register_id_the_sweep_recorded() {
        let context = token("partner@example.test", 1, &["overview"], &[], Some(at(2)));
        let notes = attention_notes(&scope(), &[room(vec![context.clone()])], &policy(), at(10))
            .expect("notes");

        let recorded = derive_follow_ups(&audience(), "room-7", &[context], &policy(), at(10));
        assert_eq!(recorded.len(), 1, "the sweep raises exactly one row here");
        assert_eq!(
            notes[0].obligation_id,
            Some(obligation_id_for(&scope(), &recorded[0])),
            "the note must address the row the register actually holds"
        );
    }

    /// Inside the window there is no register row, and the note must say so
    /// rather than inventing a handle.
    ///
    /// A pairing that returned an id for a row nobody recorded would make every
    /// join silently miss, and a cycle would report an obligation the register
    /// has never heard of.
    #[test]
    fn an_unripened_note_carries_no_register_id() {
        let notes = attention_notes(
            &scope(),
            &[room(vec![token(
                "partner@example.test",
                1,
                &["overview"],
                &[],
                Some(at(9)),
            )])],
            &policy(),
            at(10),
        )
        .expect("notes");

        assert_eq!(notes[0].reading, AttentionReading::ReadThenSilent);
        assert_eq!(
            notes[0].obligation_id, None,
            "the follow-up window has not closed, so there is no row to point at"
        );
    }

    /// A token that never appeared in the log is the note that earns the
    /// feature, and it must lead the list.
    ///
    /// A room shared and never opened is the first evidence the system can
    /// produce that a message did not arrive. Deriving the roster from events
    /// would delete exactly the people worth worrying about, and burying it
    /// under the nudges would have an agent chase somebody whose mail is in a
    /// spam folder.
    #[test]
    fn a_never_opened_token_leads_and_reads_as_a_delivery_question() {
        let notes = attention_notes(
            &scope(),
            &[room(vec![
                token("reader@example.test", 5, &["overview"], &[], Some(at(2))),
                token("ghost@example.test", 0, &[], &["overview"], None),
            ])],
            &policy(),
            at(10),
        )
        .expect("notes");

        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].token_issued_to, "ghost@example.test");
        assert_eq!(notes[0].reading, AttentionReading::NeverOpened);
        assert!(notes[0].reading.is_delivery_question());
        assert!(
            !notes[0].is_partial(),
            "never opened is a delivery question, not a partial read"
        );
        assert!(notes[0].headline.contains("check the link arrived"));
        assert_eq!(notes[1].token_issued_to, "reader@example.test");
        assert_eq!(notes[1].reading, AttentionReading::ReturnedThenSilent);
    }

    /// A reply wins over every room signal, the delivery question included, and
    /// an answered room is not work.
    ///
    /// A cycle that surfaced answered rooms as outstanding would ask the owner
    /// to chase a live conversation — and a never-opened token that replied by
    /// another channel proves the message landed, so the delivery question is
    /// moot.
    #[test]
    fn a_reply_makes_a_room_live_rather_than_owed() {
        let mut replied = token("partner@example.test", 0, &[], &["overview"], None);
        replied.replied = true;
        let notes =
            attention_notes(&scope(), &[room(vec![replied])], &policy(), at(10)).expect("notes");

        assert_eq!(notes[0].reading, AttentionReading::Answered);
        assert!(!notes[0].reading.needs_action());
        assert_eq!(
            notes[0].obligation_id, None,
            "a reply suppresses the follow-up, so there is no row to point at"
        );
        assert!(actionable(&notes).is_empty());
    }

    /// One token named twice must not produce two notes pointing at two rows
    /// for one person.
    ///
    /// `derive_follow_ups` takes the first occurrence; a note list that took
    /// the last would attach to a different `due_at`, hence a different derived
    /// id, and the agent would see one person twice with one of the two
    /// pointing nowhere.
    #[test]
    fn a_duplicated_token_contributes_one_note_from_its_first_context() {
        let first = token("partner@example.test", 1, &["overview"], &[], Some(at(2)));
        let later = token("partner@example.test", 1, &["overview"], &[], Some(at(5)));
        let notes = attention_notes(
            &scope(),
            &[room(vec![first.clone(), later])],
            &policy(),
            at(10),
        )
        .expect("notes");

        assert_eq!(notes.len(), 1);
        let recorded = derive_follow_ups(&audience(), "room-7", &[first], &policy(), at(10));
        assert_eq!(
            notes[0].obligation_id,
            Some(obligation_id_for(&scope(), &recorded[0])),
            "the note must follow the first context, exactly as the sweep does"
        );
    }

    /// A value carrying the id separator would fuse two rooms' follow-ups into
    /// one id, pointing an agent at somebody else's obligation.
    #[test]
    fn a_separator_bearing_value_is_refused_rather_than_paired() {
        let mut poisoned = room(vec![token(
            "partner@example.test",
            1,
            &["overview"],
            &[],
            Some(at(2)),
        )]);
        poisoned.room_id = "room\u{1f}7".to_string();
        assert!(attention_notes(&scope(), &[poisoned], &policy(), at(10)).is_err());

        let mut poisoned_token = room(vec![token(
            "partner\u{1f}@example.test",
            1,
            &["overview"],
            &[],
            Some(at(2)),
        )]);
        poisoned_token.room_id = "room-7".to_string();
        assert!(attention_notes(&scope(), &[poisoned_token], &policy(), at(10)).is_err());

        let mut poisoned_audience = room(vec![token(
            "partner@example.test",
            1,
            &["overview"],
            &[],
            Some(at(2)),
        )]);
        poisoned_audience.audience.id = "engagement\u{1f}1".to_string();
        assert!(attention_notes(&scope(), &[poisoned_audience], &policy(), at(10)).is_err());
    }

    /// An empty book is "nobody holds a link", never "everybody is fine".
    ///
    /// Pinned because the vacuous reading is the failure mode: an agent whose
    /// cycle reads an empty note list must not conclude every relationship is
    /// healthy, and the emptiness has to be reachable without an error so a
    /// caller can tell it apart from a refusal.
    #[test]
    fn an_empty_book_yields_an_empty_list_and_not_an_error() {
        let notes = attention_notes(&scope(), &[], &policy(), at(10)).expect("notes");
        assert!(notes.is_empty());
        assert!(actionable(&notes).is_empty());

        let empty_room =
            attention_notes(&scope(), &[room(Vec::new())], &policy(), at(10)).expect("notes");
        assert!(empty_room.is_empty());
    }
}
