//! Who came back to what — plan phase 3, §6.
//!
//! §6: *"The audit log is the product, not the paperwork."* It is **the only
//! signal in this whole set that reports what the counterparty did** rather than
//! what the agent did. Everything else — sends, obligations, engagement stages —
//! observes our own side of the conversation.
//!
//! # Possession, never proof of identity
//!
//! Corrected in the plan after external review, and it shapes the type. A
//! capability URL proves **possession of the link**, and links get forwarded. So
//! the honest record is
//!
//! > the token issued to identity X was presented
//!
//! and **never** *"identity X opened this document"*. An associate forwarding a
//! room to a partner is normal behaviour, not an anomaly, and the log must not
//! assert otherwise. The field is therefore named
//! [`token_issued_to`](AccessEvent::token_issued_to) — there is no field called
//! `identity`, and that absence is deliberate: a reader cannot casually write
//! down a claim the system cannot support.
//!
//! # Revocation is forward-only
//!
//! Closing a room prevents **future** access. It does not recall a file already
//! downloaded, and nothing here should be described as if it could.
//!
//! # No covert measurement
//!
//! A room is observable because a room is a place you visit. An email is not.
//! There are no pixels and no beacons anywhere in this design, and dwell is
//! best-effort — presence is reliable, duration is not, and no decision should
//! rest on a number the browser was never obliged to report honestly.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::AudienceRef;

/// Coarse only. Enough to tell a phone from a laptop, and deliberately not
/// enough to fingerprint anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserAgentClass {
    Desktop,
    Mobile,
    Unknown,
}

impl UserAgentClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
            Self::Unknown => "unknown",
        }
    }
}

/// One presentation of a room token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEvent {
    pub room_id: String,
    /// The audience the room serves — an engagement, a cohort, an account, a
    /// panel. Carried on the event so a log line is readable without loading the
    /// room it came from.
    pub audience: AudienceRef,
    /// The identity the token was **issued to** — not a claim about who
    /// presented it. See the module note: links get forwarded, and asserting
    /// otherwise would make normal behaviour look like an anomaly.
    pub token_issued_to: String,
    /// `None` means the index was viewed rather than a document. An index view
    /// is an access too — it is how "they looked but opened nothing" is
    /// distinguishable from "they never came".
    pub document_ref: Option<String>,
    pub occurred_at: DateTime<Utc>,
    /// Best-effort, and `None` is the honest common case.
    pub dwell_ms: Option<u64>,
    /// First presentation, or nth, for this token.
    pub sequence: u32,
    pub user_agent_class: UserAgentClass,
}

/// What the room can say about one token's attention.
///
/// Deliberately **not** the plan's four states verbatim. Three of them — never
/// opened, opened once, opened repeatedly — are things a room knows. The fourth,
/// *"opened once, then silence"*, requires knowing whether they **replied**,
/// which happens outside the room entirely.
///
/// Reporting that here would mean claiming knowledge the room does not have, so
/// the join is left to the caller who has both halves. [`OpenedOnce`] is the
/// honest half of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSignal {
    /// Shared, and the token was never presented.
    ///
    /// §6: *"the one that earns the whole feature."* Mail from a shared sending
    /// domain can land in spam while every layer reports success, so a room that
    /// was shared and never opened is **the first evidence the system can
    /// produce that a message did not arrive** — inferred, not certain, and the
    /// only such signal available anywhere in the set.
    NeverOpened,
    /// Presented once. Combine with reply data for the plan's
    /// "opened once, then silence".
    OpenedOnce,
    /// They came back. Interest, or confusion — worth asking which.
    OpenedRepeatedly,
}

impl AttentionSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverOpened => "never_opened",
            Self::OpenedOnce => "opened_once",
            Self::OpenedRepeatedly => "opened_repeatedly",
        }
    }

    /// Whether this is a delivery question rather than a follow-up.
    ///
    /// §6: never-opened *"becomes a delivery question rather than a nudge"*.
    /// Nudging someone who never received the mail is the wrong action and reads
    /// as pestering.
    pub fn is_delivery_question(self) -> bool {
        matches!(self, Self::NeverOpened)
    }
}

/// One token's attention across a room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAttention {
    pub token_issued_to: String,
    /// Distinct visits, from the highest `sequence` seen.
    ///
    /// **Not the event count.** One visit that views the index and then opens a
    /// document produces two events and is still one visit — counting events
    /// would report "returned to it" for somebody who came once and clicked
    /// twice, which is the difference between interest and a single read.
    ///
    /// `sequence` is the plan's own field for this: *"first open, or nth"*.
    pub visits: u32,
    /// Raw events, kept because "three page views in one visit" is a different
    /// fact from "one page view" and neither is the visit count.
    pub presentations: usize,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    /// Documents this token was presented against, sorted.
    pub documents_opened: Vec<String>,
    /// Documents in the room it never reached — §6's "partial": *"deck opened,
    /// financials not. Tells you what the next conversation is about."*
    pub documents_unopened: Vec<String>,
    /// Index views, which are accesses without a document.
    pub index_views: usize,
}

impl TokenAttention {
    /// Derived from **visits**, not from events.
    ///
    /// A token that viewed the index and then opened a document in one sitting
    /// came once. Reporting that as "opened repeatedly" would turn a single read
    /// into evidence of interest.
    pub fn signal(&self) -> AttentionSignal {
        match self.visits {
            0 => AttentionSignal::NeverOpened,
            1 => AttentionSignal::OpenedOnce,
            _ => AttentionSignal::OpenedRepeatedly,
        }
    }

    /// Whether some of the room was read and some was not.
    ///
    /// False when nothing was opened: "they never came" is `NeverOpened`, not a
    /// partial read, and conflating them would turn the strongest delivery
    /// signal in the set into a content observation.
    pub fn is_partial(&self) -> bool {
        !self.documents_opened.is_empty() && !self.documents_unopened.is_empty()
    }
}

/// Summarise what a set of events says about one token.
///
/// `room_documents` is passed in so "unopened" means *unopened out of what is
/// actually in the room* — a document withdrawn last week is not something they
/// failed to read.
pub fn attention_for(
    token_issued_to: &str,
    room_documents: &[String],
    events: &[AccessEvent],
) -> TokenAttention {
    let mine: Vec<&AccessEvent> = events
        .iter()
        .filter(|event| event.token_issued_to == token_issued_to)
        .collect();

    let mut opened: BTreeSet<&str> = BTreeSet::new();
    let mut index_views = 0usize;
    for event in &mine {
        match event.document_ref.as_deref() {
            Some(document) => {
                opened.insert(document);
            },
            None => index_views += 1,
        }
    }

    let present: BTreeSet<&str> = room_documents.iter().map(String::as_str).collect();
    // Intersected with the room's current contents: a document opened and later
    // withdrawn is not still "in the room", and one withdrawn before they looked
    // is not something they failed to read.
    let documents_opened: Vec<String> = opened
        .iter()
        .filter(|document| present.contains(*document))
        .map(|document| (*document).to_string())
        .collect();
    let documents_unopened: Vec<String> = present
        .iter()
        .filter(|document| !opened.contains(*document))
        .map(|document| (*document).to_string())
        .collect();

    TokenAttention {
        token_issued_to: token_issued_to.to_string(),
        // The highest sequence a token reached IS its visit count — the plan's
        // own field, rather than a session heuristic this module would be
        // inventing.
        visits: mine.iter().map(|event| event.sequence).max().unwrap_or(0),
        presentations: mine.len(),
        first_seen: mine.iter().map(|event| event.occurred_at).min(),
        last_seen: mine.iter().map(|event| event.occurred_at).max(),
        documents_opened,
        documents_unopened,
        index_views,
    }
}

/// Attention for every token a room was shared with.
///
/// `shared_with` is supplied rather than derived from the events, and that is
/// the whole point: a token that never appears in the log is exactly the
/// `NeverOpened` case, and deriving the list from events would make the most
/// important signal in the feature invisible.
pub fn attention_across(
    shared_with: &[String],
    room_documents: &[String],
    events: &[AccessEvent],
) -> Vec<TokenAttention> {
    let mut out: Vec<TokenAttention> = shared_with
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|token| attention_for(token, room_documents, events))
        .collect();
    // Never-opened first: it is the one that earns the feature, and it is a
    // delivery question rather than a nudge.
    out.sort_by_key(|attention| (attention.visits, attention.token_issued_to.clone()));
    out
}

/// How many times each document was reached, across every token.
///
/// *"Opened the deck three times, never opened the financials"* — the sentence
/// §6 opens with.
pub fn document_reach(events: &[AccessEvent]) -> BTreeMap<String, usize> {
    let mut reach = BTreeMap::new();
    for event in events {
        if let Some(document) = event.document_ref.as_deref() {
            *reach.entry(document.to_string()).or_insert(0) += 1;
        }
    }
    reach
}

/// The notice a room must carry.
///
/// §6: *"the room says it is logged, on the room itself — not buried in a
/// policy."* Provided here so the wording lives next to the thing it describes
/// and cannot drift from what is actually recorded.
pub const ROOM_LOGGING_NOTICE: &str =
    "Access to this room is logged: which link was used, which documents were \
     opened, and when. Links may be forwarded, so the log records the link \
     rather than the person.";
