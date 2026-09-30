//! What the market read — cross-audience aggregation, plan phase 4, §6.
//!
//! §6: *"Aggregate across engagements and the access log stops being per-deal
//! telemetry and becomes positioning research from behaviour instead of
//! opinion: 'the financials were opened by nine of eleven; the team slide by
//! two'. That is a fact about what this market finds load-bearing… Same for
//! the charter: the questions asked most often across applications are the
//! questions your positioning has not yet answered."*
//!
//! # Counts only
//!
//! The sentence the owner reads is *"opened by nine of eleven"*, never a
//! ratio. Every output here is a count travelling with the total it came out
//! of, so the reader weighs it themselves — the same doctrine as
//! [`CohortSummary`](super::proposal::CohortSummary): a single number invites
//! comparison without looking at what produced it.
//!
//! # Pure functions over supplied inputs
//!
//! There is no store here and no read of one. The caller assembles
//! [`RoomAttention`] values from wherever its access log lives; this module
//! only folds what it is handed. Coupling market aggregation to one access-log
//! store would make it unusable by any flow that observes attention some other
//! way, and the fold itself has no opinion about where attention was observed.
//!
//! # What this deliberately does not do
//!
//! - **No identity claims.** The unit is the token. The access log's own
//!   doctrine is that a capability URL proves possession of the link, not
//!   identity, and an aggregate must not launder *"the token issued to X was
//!   presented"* into *"X read it"* by summing across rooms.
//! - **No rates, scores or rankings.** Counts with totals. Outputs are sorted
//!   so the most-opened reads first, which is readability, not a ranking
//!   claim.
//! - **No open-ended audiences.** Every count of audiences is a count of
//!   named, enumerable [`AudienceRef`]s, keyed by
//!   [`AudienceRef::as_key`] so the kind survives aggregation — an engagement
//!   and an account sharing an id never merge into one "counterparty".
//! - **No semantic clustering of questions.** See [`question_frequency`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::data_room::access_log::TokenAttention;
use magician::magician_v2::audience::AudienceRef;

use super::proposal::MINIMUM_COUNTERPARTIES;

/// One room's attention, as the caller assembled it from its access log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomAttention {
    /// The relationship the room serves. Kinds may be mixed freely across a
    /// market read — a book of engagements, accounts and programmes aggregates
    /// as one market.
    pub audience: AudienceRef,
    /// Every token the room was shared with — the share list itself.
    ///
    /// Supplied, never derived from `attentions`: a token that never appears
    /// in any log line is exactly the never-opened case, and deriving the
    /// share list from observed events would make the most important signal in
    /// the set invisible. The access log's own rule, kept at market scale.
    pub shared_with: Vec<String>,
    /// Per-token attention, as the caller computed it from the room's events.
    pub attentions: Vec<TokenAttention>,
}

/// What the market did with one document.
///
/// All three fields are counts of distinct things and they travel together, so
/// the owner reads *"opened by nine of eleven, in three audiences"* — the
/// plan's sentence — and never a ratio pretending to precision the sample does
/// not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRead {
    pub document: String,
    /// Distinct tokens, across every room, that opened it. A token that opened
    /// it twice — or was handed it in two rooms — is one reader, because the
    /// question is *who* found it load-bearing, not how many clicks landed.
    pub opened_by: usize,
    /// Distinct tokens whose room actually held the document — the *eleven* in
    /// "nine of eleven".
    ///
    /// For a token with an attention row, containment is derived from the
    /// union of its `documents_opened` and `documents_unopened`, because its
    /// unopened list is scoped to what its room held for it — a document a
    /// room never contained is not something its readers declined. For a
    /// token on the share list with **no** attention row at all — the
    /// never-opened case — containment is its room's whole document set, the
    /// union across that room's rows: the room was shared with it, so
    /// everything the room is known to hold was. Every opener is contained by
    /// construction, so `opened_by` can never exceed `shared_with`.
    pub shared_with: usize,
    /// Distinct audiences in which at least one open happened. This is what
    /// [`market_confidence_floor`] reads: breadth across relationships, not
    /// volume within one.
    pub audiences: usize,
}

/// One question the market asked, counted literally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionCount {
    /// The question after [normalisation](question_frequency) — still the
    /// literal words an audience used, so the owner recognises it on sight.
    pub question: String,
    /// Distinct audiences that asked it. The market signal.
    pub audiences: usize,
    /// Times it was asked in total, repeats included. Kept beside `audiences`
    /// so one insistent audience is visible as exactly that.
    pub total: usize,
}

/// # An honest limit: ghosts assume room-uniform visibility
///
/// A ghost token (shared, never appeared) is imputed the room's whole document
/// set, derived from other tokens' rows. Under per-identity visibility (deal
/// close phase 5, not yet in earnest) that can attribute a restricted document
/// to a ghost who could never see it, slightly inflating its denominator. The
/// alternative — dropping ghosts — inflates apparent engagement instead, which
/// is the worse dishonesty. When phase 5 lands, the input shape should carry
/// per-token visibility rather than this fold guessing.

/// Fold every room's attention into one market read per document.
///
/// §6's sentence, produced: *"the financials were opened by nine of eleven;
/// the team slide by two"* — a fact about what this market finds load-bearing,
/// from behaviour instead of opinion.
///
/// A document nobody opened anywhere still appears, with `opened_by` zero:
/// the silent document is the finding, and dropping it would hide exactly what
/// the aggregation exists to surface.
///
/// The share list is the authority on who was shared — the same rule
/// [`never_opened_across`] reads by. A token on `shared_with` with no
/// attention row at all is exactly the never-opened case, so it counts toward
/// `shared_with` for every document its room is known to hold; dropping it
/// would report a room half of whose shares went silent as fully read,
/// inflating the very number the denominator exists to keep honest. One
/// honest limit: a room with **zero** attention rows has an unknowable
/// document set — this fold learns what a room held only from its rows — so
/// such a room contributes nothing here, while its silent shares still
/// surface through [`never_opened_across`], which needs no document to name
/// them.
///
/// Sorted by `opened_by` descending, then document name, so the same market
/// produces the same page every time it is read.
pub fn document_market_read(rooms: &[RoomAttention]) -> Vec<DocumentRead> {
    #[derive(Default)]
    struct Tally {
        opened_by: BTreeSet<String>,
        shared_with: BTreeSet<String>,
        audiences: BTreeSet<String>,
    }

    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for room in rooms {
        // The room's document set and which tokens have an attention row —
        // both feed the ghost pass below.
        let mut room_documents: BTreeSet<&str> = BTreeSet::new();
        let mut has_row: BTreeSet<&str> = BTreeSet::new();
        for attention in &room.attentions {
            let token = &attention.token_issued_to;
            has_row.insert(token.as_str());
            for document in &attention.documents_opened {
                room_documents.insert(document.as_str());
                let tally = tallies.entry(document.clone()).or_default();
                // Sets, not increments: the same token opening in two visits,
                // two rooms, or two attention rows is one reader.
                tally.opened_by.insert(token.clone());
                tally.shared_with.insert(token.clone());
                // as_key(), never the bare id: an engagement and an account
                // sharing an id are two audiences, and merging them would
                // overstate breadth.
                tally.audiences.insert(room.audience.as_key());
            }
            for document in &attention.documents_unopened {
                room_documents.insert(document.as_str());
                // Unopened still means the token's room held it — this is the
                // denominator, and it is what keeps "nine of eleven" honest
                // when two of the eleven never came at all.
                tallies
                    .entry(document.clone())
                    .or_default()
                    .shared_with
                    .insert(token.clone());
            }
        }
        // The ghost pass: a token on the share list with no attention row is
        // the never-opened case, and it belongs in the denominator of every
        // document its room is known to hold — the share list is the
        // authority on who was shared, the same rule `never_opened_across`
        // reads by. Skipping it would report a share-and-silence room as
        // fully engaged. A room with zero attention rows leaves
        // `room_documents` empty, so its ghosts add nothing here: what that
        // room held is unknowable to this fold, and inventing a document to
        // hang the ghost on would be a claim, not a count.
        for token in &room.shared_with {
            if has_row.contains(token.as_str()) {
                continue;
            }
            for document in &room_documents {
                tallies
                    .entry((*document).to_string())
                    .or_default()
                    .shared_with
                    .insert(token.clone());
            }
        }
    }

    let mut out: Vec<DocumentRead> = tallies
        .into_iter()
        .map(|(document, tally)| DocumentRead {
            document,
            opened_by: tally.opened_by.len(),
            shared_with: tally.shared_with.len(),
            audiences: tally.audiences.len(),
        })
        .collect();
    out.sort_by(|left, right| {
        right
            .opened_by
            .cmp(&left.opened_by)
            .then_with(|| left.document.cmp(&right.document))
    });
    out
}

/// The delivery-question list at market scale.
///
/// Phase 3's rule, aggregated: a token that was shared a room and never
/// presented it is *"a delivery question rather than a nudge"* — the first
/// evidence the system can produce that a message did not arrive. Each pair
/// names the relationship to chase delivery on and the token that went silent.
///
/// Two deliberate readings of the inputs:
///
/// - **The share list is the authority on who was shared.** A token on
///   `shared_with` with no attention row at all is listed: absence from the
///   log *is* the never-opened signal, not missing data to skip. Conversely a
///   token that appears only in `attentions` and not on the share list is not
///   listed — this module will not claim "shared and silent" about a share it
///   was never told happened.
/// - **Presenting the token at all is arrival.** A token that visited and only
///   viewed the index opened no document, but the mail plainly arrived, so
///   there is no delivery question to raise about it.
///
/// Sorted by audience key then token; a pair appearing in several rooms of the
/// same audience is listed once.
pub fn never_opened_across(rooms: &[RoomAttention]) -> Vec<(AudienceRef, String)> {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out: Vec<(AudienceRef, String)> = Vec::new();
    for room in rooms {
        let presented: BTreeSet<&str> = room
            .attentions
            .iter()
            .filter(|attention| !attention.signal().is_delivery_question())
            .map(|attention| attention.token_issued_to.as_str())
            .collect();
        for token in &room.shared_with {
            if presented.contains(token.as_str()) {
                continue;
            }
            // First-seen dedupe: a duplicated share entry, or two rooms on the
            // same relationship, must not double a line the owner acts on.
            if seen.insert((room.audience.as_key(), token.clone())) {
                out.push((room.audience.clone(), token.clone()));
            }
        }
    }
    out.sort_by(|left, right| {
        left.0
            .as_key()
            .cmp(&right.0.as_key())
            .then_with(|| left.1.cmp(&right.1))
    });
    out
}

/// Whether one document's market read is broad enough to be about the market.
///
/// Reuses [`MINIMUM_COUNTERPARTIES`] — one floor, one place, so the loop
/// cannot end up with two opinions about what "enough of a market" means. §5:
/// *"one rejection is a fact about that counterparty, not about the pitch"*,
/// and one audience opening — or ignoring — a document is likewise a fact
/// about them, not about positioning.
///
/// The floor reads `audiences`, which counts audiences in which an open
/// **happened**. A document never opened anywhere therefore has zero and
/// fails: an empty read must not pass a market-breadth check vacuously, even
/// though the silence itself remains reported by
/// [`document_market_read`].
pub fn market_confidence_floor(read: &DocumentRead) -> bool {
    read.audiences >= MINIMUM_COUNTERPARTIES
}

/// §6's charter half: *"the questions asked most often across applications are
/// the questions your positioning has not yet answered."*
///
/// Input is supplied as `(audience, questions asked by it)` pairs — this
/// module does not read a charter, an inbox or a transcript store, so any flow
/// that collects questions can use it.
///
/// Normalisation is **literal**: trim, lowercase, collapse runs of whitespace.
/// Stemming and semantic clustering are deliberately absent — the owner reads
/// the market's actual words, and deciding that two differently-worded
/// questions "mean the same thing" would be an LLM judgment inside the loop,
/// which §5 forbids. Two synonymous questions stay two rows; merging them is
/// the owner's editorial call, made looking at both.
///
/// Sorted by `audiences` descending, then `total` descending, then question.
/// Breadth outranks volume on purpose: a question asked five times by one
/// audience is `audiences == 1` — visible, but it is that audience's
/// preoccupation, not yet the market's.
///
/// A string that normalises to nothing is not a question and is not counted:
/// letting blank extraction rows accumulate would inflate a table the owner
/// treats as evidence.
pub fn question_frequency(asked: &[(AudienceRef, Vec<String>)]) -> Vec<QuestionCount> {
    #[derive(Default)]
    struct Tally {
        audiences: BTreeSet<String>,
        total: usize,
    }

    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for (audience, questions) in asked {
        for question in questions {
            let question = normalise_question(question);
            if question.is_empty() {
                continue;
            }
            let tally = tallies.entry(question).or_default();
            tally.audiences.insert(audience.as_key());
            tally.total += 1;
        }
    }

    let mut out: Vec<QuestionCount> = tallies
        .into_iter()
        .map(|(question, tally)| QuestionCount {
            question,
            audiences: tally.audiences.len(),
            total: tally.total,
        })
        .collect();
    out.sort_by(|left, right| {
        right
            .audiences
            .cmp(&left.audiences)
            .then_with(|| right.total.cmp(&left.total))
            .then_with(|| left.question.cmp(&right.question))
    });
    out
}

/// Trim, lowercase, collapse whitespace — and nothing more. See
/// [`question_frequency`] for why nothing more.
fn normalise_question(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn attention(token: &str, opened: &[&str], unopened: &[&str]) -> TokenAttention {
        let visits = u32::from(!opened.is_empty());
        TokenAttention {
            token_issued_to: token.to_string(),
            visits,
            presentations: opened.len(),
            first_seen: (visits > 0).then(now),
            last_seen: (visits > 0).then(now),
            documents_opened: opened.iter().map(|document| document.to_string()).collect(),
            documents_unopened: unopened
                .iter()
                .map(|document| document.to_string())
                .collect(),
            index_views: 0,
        }
    }

    fn room(
        audience: AudienceRef,
        shared_with: &[&str],
        attentions: Vec<TokenAttention>,
    ) -> RoomAttention {
        RoomAttention {
            audience,
            shared_with: shared_with.iter().map(|token| token.to_string()).collect(),
            attentions,
        }
    }

    /// §6's market, built exactly: three rooms with overlapping tokens (t04
    /// spans the two engagements, t08 spans an engagement and the programme),
    /// eleven distinct tokens whose rooms held the financials, nine of whom
    /// opened them, and a team slide only two ever reached. Two tokens never
    /// presented at all, one of each silent shape: t03 has an attention row
    /// with everything unopened, and t10 is a ghost — on the share list with
    /// no attention row at all — so the *eleven* includes a share the log
    /// never saw.
    fn fixture() -> Vec<RoomAttention> {
        vec![
            room(
                AudienceRef::engagement("alpha"),
                &["t01", "t02", "t03", "t04"],
                vec![
                    attention("t01", &["financials", "team-slide"], &[]),
                    attention("t02", &["financials"], &["team-slide"]),
                    attention("t03", &[], &["financials", "team-slide"]),
                    attention("t04", &["financials"], &["team-slide"]),
                ],
            ),
            room(
                AudienceRef::engagement("beta"),
                &["t04", "t05", "t06", "t07", "t08"],
                vec![
                    attention("t04", &["financials"], &["team-slide"]),
                    attention("t05", &["financials", "team-slide"], &[]),
                    attention("t06", &["financials"], &["team-slide"]),
                    attention("t07", &["financials"], &["team-slide"]),
                    attention("t08", &["financials"], &["team-slide"]),
                ],
            ),
            room(
                AudienceRef::program("q3-intake"),
                &["t08", "t09", "t10", "t11"],
                // t10 is deliberately absent from the attentions: a shared
                // token the log never saw, held in the eleven by the share
                // list alone.
                vec![
                    attention("t08", &["financials"], &["team-slide"]),
                    attention("t09", &["financials"], &["team-slide"]),
                    attention("t11", &["financials"], &["team-slide"]),
                ],
            ),
        ]
    }

    /// §6's own sentence: *"the financials were opened by nine of eleven; the
    /// team slide by two"* — counts with their totals, never a ratio, most
    /// -opened first. The eleven includes t10, the ghost share with no
    /// attention row: dropping ghosts from the denominator read "nine of
    /// ten", shrinking the total by exactly the silence it exists to keep
    /// visible.
    #[test]
    fn the_financials_were_opened_by_nine_of_eleven_the_team_slide_by_two() {
        assert_eq!(
            document_market_read(&fixture()),
            vec![
                DocumentRead {
                    document: "financials".to_string(),
                    opened_by: 9,
                    shared_with: 11,
                    audiences: 3,
                },
                DocumentRead {
                    document: "team-slide".to_string(),
                    opened_by: 2,
                    shared_with: 11,
                    audiences: 2,
                },
            ]
        );
    }

    /// A token that opened a document in two visits — or was handed it in two
    /// rooms — is one reader. The question is who found it load-bearing, and
    /// counting presentations would report clicks as people.
    #[test]
    fn a_token_opening_twice_is_one_reader() {
        let rooms = vec![
            room(
                AudienceRef::engagement("alpha"),
                &["t1"],
                vec![attention("t1", &["deck"], &[])],
            ),
            room(
                AudienceRef::account("acme"),
                &["t1"],
                vec![attention("t1", &["deck"], &[])],
            ),
        ];
        assert_eq!(
            document_market_read(&rooms),
            vec![DocumentRead {
                document: "deck".to_string(),
                opened_by: 1,
                shared_with: 1,
                audiences: 2,
            }]
        );
    }

    /// The denominator is honest: a token whose room held the document counts
    /// toward `shared_with` even if it never opened anything, because its
    /// unopened list is scoped to what its room held. Without this, the
    /// silent two of the eleven would vanish from the total.
    #[test]
    fn shared_with_counts_tokens_that_never_came() {
        let rooms = vec![room(
            AudienceRef::engagement("alpha"),
            &["t1", "t2"],
            vec![
                attention("t1", &["deck"], &[]),
                attention("t2", &[], &["deck"]),
            ],
        )];
        assert_eq!(
            document_market_read(&rooms),
            vec![DocumentRead {
                document: "deck".to_string(),
                opened_by: 1,
                shared_with: 2,
                audiences: 1,
            }]
        );
    }

    /// §5: *"one rejection is a fact about that counterparty, not about the
    /// pitch"* — one audience's read is a fact about them, not the market. The
    /// floor is proposal's MINIMUM_COUNTERPARTIES — one floor, one place — and
    /// it flips exactly there.
    #[test]
    fn the_market_floor_flips_exactly_at_minimum_counterparties() {
        let mut read = DocumentRead {
            document: "financials".to_string(),
            opened_by: 5,
            shared_with: 6,
            audiences: MINIMUM_COUNTERPARTIES - 1,
        };
        assert!(!market_confidence_floor(&read));
        read.audiences = MINIMUM_COUNTERPARTIES;
        assert!(market_confidence_floor(&read));

        // From real rooms: one audience opening is below the floor; a second
        // audience opening reaches it.
        let mut rooms = vec![room(
            AudienceRef::engagement("alpha"),
            &["t1"],
            vec![attention("t1", &["deck"], &[])],
        )];
        assert!(!market_confidence_floor(&document_market_read(&rooms)[0]));
        rooms.push(room(
            AudienceRef::program("q3-intake"),
            &["t2"],
            vec![attention("t2", &["deck"], &[])],
        ));
        assert!(market_confidence_floor(&document_market_read(&rooms)[0]));

        // A document never opened anywhere has zero audiences and must fail —
        // an empty market read cannot pass a market-breadth check vacuously.
        let silent = vec![room(
            AudienceRef::engagement("alpha"),
            &["t1"],
            vec![attention("t1", &[], &["deck"])],
        )];
        let silent_read = document_market_read(&silent);
        assert_eq!(silent_read[0].audiences, 0);
        assert!(!market_confidence_floor(&silent_read[0]));
    }

    /// §6: *"the questions asked most often across applications are the
    /// questions your positioning has not yet answered."* Normalisation is
    /// literal — case and whitespace collapse, synonyms do not: merging two
    /// differently-worded questions would be an LLM judgment §5 forbids.
    #[test]
    fn question_normalisation_is_literal_not_semantic() {
        let asked = vec![
            (
                AudienceRef::program("intake-a"),
                vec!["  What is your   CHURN? ".to_string()],
            ),
            (
                AudienceRef::program("intake-b"),
                vec!["what is your churn?".to_string()],
            ),
            (
                AudienceRef::program("intake-c"),
                vec!["what is your retention?".to_string()],
            ),
        ];
        assert_eq!(
            question_frequency(&asked),
            vec![
                QuestionCount {
                    question: "what is your churn?".to_string(),
                    audiences: 2,
                    total: 2,
                },
                // A near-synonym stays its own row, on purpose.
                QuestionCount {
                    question: "what is your retention?".to_string(),
                    audiences: 1,
                    total: 1,
                },
            ]
        );
    }

    /// A question asked five times by ONE audience is `audiences == 1` —
    /// visible, but it does not outrank a question two audiences asked once
    /// each. Breadth is what "the market keeps asking" means; volume from one
    /// voice is that voice's preoccupation.
    #[test]
    fn one_insistent_audience_does_not_outrank_the_market() {
        let asked = vec![
            (
                AudienceRef::engagement("loud"),
                vec!["why now?".to_string(); 5],
            ),
            (
                AudienceRef::engagement("a"),
                vec!["who are the team?".to_string()],
            ),
            (
                AudienceRef::engagement("b"),
                vec!["who are the team?".to_string()],
            ),
        ];
        assert_eq!(
            question_frequency(&asked),
            vec![
                QuestionCount {
                    question: "who are the team?".to_string(),
                    audiences: 2,
                    total: 2,
                },
                QuestionCount {
                    question: "why now?".to_string(),
                    audiences: 1,
                    total: 5,
                },
            ]
        );
    }

    /// The delivery-question list at market scale names exactly the tokens
    /// that were shared and never presented, with the relationship each one
    /// belongs to — and nobody else. Both silent shapes appear: t03, whose
    /// row shows nothing opened, and t10, the ghost with no row at all.
    #[test]
    fn never_opened_across_lists_exactly_the_silent_tokens() {
        assert_eq!(
            never_opened_across(&fixture()),
            vec![
                (AudienceRef::engagement("alpha"), "t03".to_string()),
                (AudienceRef::program("q3-intake"), "t10".to_string()),
            ]
        );
    }

    /// The share list is the authority on who was shared: a token with no
    /// attention row at all IS the never-opened signal, not missing data to
    /// skip. And a token that presented but only viewed the index arrived —
    /// no delivery question about it.
    #[test]
    fn absence_from_the_log_is_the_signal_and_an_index_view_is_arrival() {
        let mut index_only = attention("t-index", &[], &["deck"]);
        index_only.visits = 1;
        index_only.presentations = 1;
        index_only.index_views = 1;
        index_only.first_seen = Some(now());
        index_only.last_seen = Some(now());

        let rooms = vec![room(
            AudienceRef::engagement("alpha"),
            &["t-index", "t-ghost"],
            // t-ghost has no attention row at all.
            vec![index_only],
        )];
        assert_eq!(
            never_opened_across(&rooms),
            vec![(AudienceRef::engagement("alpha"), "t-ghost".to_string())]
        );
    }

    /// Pins the ghost-denominator failure: a token on the share list with no
    /// attention row at all was dropped from `shared_with`, so a room where
    /// half the shares went silent read as opened-by-one-of-one — full
    /// engagement manufactured from the exact silence the module exists to
    /// surface. The ghost counts toward the denominator of every document its
    /// room is known to hold, and the same token is the delivery-question
    /// line in `never_opened_across`.
    #[test]
    fn a_ghost_share_holds_the_denominator_and_raises_the_delivery_question() {
        let rooms = vec![room(
            AudienceRef::engagement("alpha"),
            &["t1", "t-ghost"],
            // t-ghost has no attention row: never presented, never logged.
            vec![attention("t1", &["deck"], &["appendix"])],
        )];
        assert_eq!(
            document_market_read(&rooms),
            vec![
                DocumentRead {
                    document: "deck".to_string(),
                    opened_by: 1,
                    shared_with: 2,
                    audiences: 1,
                },
                DocumentRead {
                    document: "appendix".to_string(),
                    opened_by: 0,
                    shared_with: 2,
                    audiences: 0,
                },
            ]
        );
        assert_eq!(
            never_opened_across(&rooms),
            vec![(AudienceRef::engagement("alpha"), "t-ghost".to_string())]
        );
    }

    /// The honest limit of the ghost rule: a room with zero attention rows
    /// has an unknowable document set — the fold learns what a room held only
    /// from its rows — so its ghost shares add nothing to any denominator.
    /// The delivery question still fires for them: `never_opened_across`
    /// needs no document to name a silent share.
    #[test]
    fn a_room_with_no_rows_has_no_documents_to_impute_to_its_ghosts() {
        let rooms = vec![
            room(
                AudienceRef::engagement("alpha"),
                &["t1"],
                vec![attention("t1", &["deck"], &[])],
            ),
            room(AudienceRef::engagement("beta"), &["t-ghost"], vec![]),
        ];
        assert_eq!(
            document_market_read(&rooms),
            vec![DocumentRead {
                document: "deck".to_string(),
                opened_by: 1,
                shared_with: 1,
                audiences: 1,
            }]
        );
        assert_eq!(
            never_opened_across(&rooms),
            vec![(AudienceRef::engagement("beta"), "t-ghost".to_string())]
        );
    }

    /// Equal counts break by document name, so the same market produces the
    /// same page every time it is read.
    #[test]
    fn document_order_is_deterministic_under_ties() {
        let rooms = vec![room(
            AudienceRef::engagement("alpha"),
            &["t1"],
            vec![attention("t1", &["zeta", "alpha-doc"], &[])],
        )];
        assert_eq!(
            document_market_read(&rooms),
            vec![
                DocumentRead {
                    document: "alpha-doc".to_string(),
                    opened_by: 1,
                    shared_with: 1,
                    audiences: 1,
                },
                DocumentRead {
                    document: "zeta".to_string(),
                    opened_by: 1,
                    shared_with: 1,
                    audiences: 1,
                },
            ]
        );
    }

    /// A market with no rooms has nothing to say and says nothing — empty in,
    /// empty out, never a panic. A room shared with nobody likewise
    /// contributes no rows: no members means no member claims.
    #[test]
    fn empty_inputs_produce_empty_outputs() {
        assert_eq!(document_market_read(&[]), Vec::<DocumentRead>::new());
        assert_eq!(
            never_opened_across(&[]),
            Vec::<(AudienceRef, String)>::new()
        );
        assert_eq!(question_frequency(&[]), Vec::<QuestionCount>::new());

        let empty_room = vec![room(AudienceRef::engagement("alpha"), &[], vec![])];
        assert_eq!(
            document_market_read(&empty_room),
            Vec::<DocumentRead>::new()
        );
        assert_eq!(
            never_opened_across(&empty_room),
            Vec::<(AudienceRef, String)>::new()
        );
    }
}
