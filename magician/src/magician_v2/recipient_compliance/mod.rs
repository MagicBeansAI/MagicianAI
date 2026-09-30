//! **The four recipient questions that belong below the agent, asked once.**
//!
//! Doc: `docs/plans/2026-08-07-opc-readiness-review.md` §9B names six checks
//! that must not be left to an agent's judgement. Two already existed —
//! suppression lookup (`agents::outward_gate::contact_refusal` over
//! [`crate::magician_v2::suppression`]) and bounce/complaint suppression
//! ([`crate::magician_v2::delivery_hygiene`]). The other four had no code
//! anywhere:
//!
//! 1. **Duplicate recipient across work.** Two programmes could both contact
//!    one person and neither knew.
//! 2. **Reply before follow-up.** Nothing stopped a follow-up going to somebody
//!    who has already answered and is waiting on *us*.
//! 3. **A jurisdiction hook.** [`crate::magician_v2::suppression`] has a
//!    `RegulatoryHold` *reason*, which is a manual hold an operator types in —
//!    not a hook anything can be plugged into.
//! 4. **Retention / deletion.** Nothing recorded that somebody asked to be
//!    forgotten, and nothing said what such a request does and does not reach.
//!
//! # One gate, four rules — not four modules
//!
//! An outward send asks **one** question and gets **one** typed answer. Four
//! separate gates would be four call sites to keep in step, four places for the
//! next one to be forgotten, and four chances for a caller to consult three of
//! them. [`screen`] answers all four and always returns exactly four
//! [`RuleFinding`]s, one per [`ComplianceRule::ALL`] entry, whatever happened —
//! so a decision that claims to have asked four questions carries four answers.
//!
//! # Which way the dependency runs
//!
//! A **coordinator**, in the shape [`crate::magician_v2::delivery_hygiene`],
//! [`crate::magician_v2::introductions`], [`crate::magician_v2::retraction`]
//! and [`crate::magician_v2::reply_routing`] already use. This module imports
//! [`crate::magician_v2::evidence::outward_assertions`] (who we contacted),
//! [`crate::magician_v2::scheduling`] (who answered and is waiting on us),
//! [`crate::magician_v2::suppression`] (identity normalisation, and the rows a
//! deletion must not touch) and [`crate::magician_v2::audience`]. **None of
//! them imports this one, and none of them learns about the others.** A
//! disclosure register that knew what a negotiation was would stop being usable
//! for a form submission; a negotiation log that knew about erasure requests
//! would stop being usable for booking one's own time.
//!
//! Nothing `agents`-shaped is imported either. `outward_gate::Addressing` and
//! `OutwardClass` are the dispatcher's own classifications, and a generic
//! coordinator that imported them would invert the rule this codebase holds to:
//! **specific imports generic, never the reverse.** The channel arrives as an
//! [`OutwardChannel`], which is the evidence subsystem's own generic type.
//!
//! # It reads. The one thing it writes is the request an owner filed
//!
//! [`screen`] writes nothing at all. [`ErasureRequestLog::record`] is the single
//! writer in this file, and its only production caller is the owner-facing
//! route in `magician-api/src/recipient_compliance_api.rs`. Nothing here
//! deletes anything, and [`erasure_proposal`] **proposes**: it says what a
//! deletion would reach, what is retained and why, and what this codebase
//! cannot speak for at all.
//!
//! # Where this is called from, and in what order
//!
//! Immediately **after** `outward_gate::contact_refusal` on the outward
//! dispatch path (`execution::agentic::executor`), and **before** the envelope
//! shadow — so the shadow still only describes acts that survived every gate
//! that decides. The ordering is load-bearing in one specific way:
//! `contact_refusal` owns the **empty recipient list** decision, because only it
//! can tell "we failed to parse who this reaches" from "this act reaches
//! nobody" (`Addressing::for_class`). All four rules here are per-identity and
//! have nothing to say about nobody, so [`compliance_refusal`] returns `None`
//! for an empty list rather than duplicating a judgement it cannot make.
//!
//! # Three limits, stated here rather than discovered later
//!
//! - **The recipient index is keyed on the unnormalised spelling.**
//!   `OutwardAssertionStore::prepare_dispatch` stores `intended_audience`
//!   verbatim and `index_act` hashes that raw string, so `Alice@Example.com`
//!   and `alice@example.com` are two different index files. Rule 1 probes every
//!   spelling it can derive (the string as supplied, its trim, and the
//!   normalised form) and reports [`DuplicateScan::spellings_probed`], so a
//!   decision never claims more coverage than it had — but a *third* spelling
//!   nobody passed in is invisible to it. The durable fix is normalising before
//!   `append_index` plus a re-index pass, and it belongs in
//!   `outward_assertions.rs`, not here.
//! - **Rules 1 and 2 abstain when the act names no work.** `Program` and
//!   `Engagement` are the only two kinds of work, and
//!   `work_binding_for_dispatch` now binds both — readiness review §9B's open
//!   decision 0, that a programme's acts reach dispatch unbound, has since been
//!   closed. What still arrives here with no work named is an execution
//!   carrying no work authority at all: `work_kind_of` maps
//!   `WorkBinding::Unbound` to `None` (and `Unresolvable` too, though the
//!   capability gate ahead of this one has already refused that). "Has some
//!   *other* work contacted this person" and "does some
//!   other work own this relationship" are both undefined for an act that names
//!   no work at all, so those two rules answer
//!   [`RuleVerdict::NotAssessed`] rather than refusing. Refusing instead would
//!   refuse every ordinary second contact and — worse — refuse *our own answer*
//!   to a counterparty who is waiting on it, which is the "a gate that refuses
//!   one hundred percent of a flow is a gate somebody routes around" failure
//!   `outward_gate` already names for form submissions. The evidence is still
//!   carried on the decision, so an owner reading
//!   `GET /recipient-compliance/check` sees the prior contacts the rule
//!   declined to decide on. Rule 2 abstains for the same reason over a row it
//!   could not read *at all*: without a work there is no ownership test to run,
//!   so an unattributed act cannot be asked rule 2's question about any row —
//!   and answering weaker evidence more harshly than stronger evidence, which
//!   refusing only the unreadable row would do, is not fail-closed, it is
//!   incoherent. The rows and their count still ride on the decision.
//! - **An unreadable counterparty is recovered two ways, and there is no
//!   third.** `SchedulingStore::open` only checks that a counterparty is
//!   non-blank once trimmed; it does not strip the angle brackets
//!   `normalise_identity` unwraps, and does not reject control characters. So
//!   two shapes are storable and unkeyable: one holding a control character —
//!   U+001F included — and one that unwraps to nothing, of which `<>` is the
//!   whole string. Such a row refuses, because it might be about the recipient in
//!   front of us; but it refuses **only** the recipients it could be about,
//!   worked out by `recoverable_identities`, which reads the control character
//!   as a separator and as noise inside one address and can speak for no other
//!   reading. The unwraps-to-nothing shape recovers nothing at all, and a row
//!   nothing could be recovered from refuses every recipient of any act that
//!   names a work, because it could be anyone. Every such row is named on
//!   [`ReplyScan::unmatchable`] and in the refusal, so the one remedy the
//!   append-only store actually offers — `SchedulingStore::close` the ask — is
//!   reachable without reading every negotiation in the workspace by hand.
//!   Repairing the counterparty in place is **not** on offer: the negotiation
//!   id is derived from it, so re-opening under a corrected spelling files a
//!   new ask and leaves the bad row live.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;
use crate::magician_v2::evidence::outward_assertions::{
    self, OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope,
};
use crate::magician_v2::suppression::{normalise_identity, SuppressionRegister, SuppressionScope};
use crate::magician_v2::work_context::WorkContextKind;

#[cfg(test)]
mod tests;

/// Field separator for derived ids across this codebase. A caller string that
/// feeds one is refused if it holds this character, here as everywhere else.
const FIELD_SEP: char = '\u{1f}';

/// Every refusal opens the same way — the **same** sentence
/// `agents::outward_gate` uses, deliberately. The model must never have to
/// notice *which* gate refused in order to understand that nothing was sent.
const REFUSAL_PREFIX: &str = "NOT SENT — this outward action was refused.";

/// How far back rule 1 looks for a contact by another work.
///
/// Named rather than passed as a literal at the call site, so the send-time
/// gate and the owner-facing route cannot drift apart about what "recently"
/// means. Fourteen days is one working fortnight: long enough that two
/// programmes running in the same quarter collide inside it, short enough that
/// a relationship a work legitimately returned to months later is not held
/// hostage by a single old send.
pub const DUPLICATE_CONTACT_WINDOW_DAYS: i64 = 14;

/// What is beyond a deletion request's reach, whatever this codebase does.
///
/// Named rather than left implicit, for the reason `retraction` states about a
/// revoked room: a proposal that listed only what it *can* delete reads as a
/// complete erasure, and the copies it never had a handle on are exactly the
/// ones somebody will be surprised by later.
const OUT_OF_REACH: [&str; 4] = [
    "provider-side copies — a mail provider's sent folder, a messaging provider's transcript",
    "anything the recipient or a counterparty already downloaded, forwarded or printed",
    "backups and snapshots taken before the request was recorded",
    "any third party a payload was disclosed to that this workspace never recorded",
];

// ── Enablement ──────────────────────────────────────────────────────────────

/// Whether this gate decides anything on the dispatch path.
///
/// Deliberately the same shape as
/// [`crate::magician_v2::approval_envelopes::EnvelopeMode`]: a posture named in
/// configuration, defaulting to the arm that changes nothing, installed once
/// when a config is accepted and read back from a process-global — because the
/// dispatch path holds no config snapshot and an entry point nobody remembered
/// to wire must not thereby start refusing sends.
///
/// # Two arms, not three
///
/// Envelopes carry a `Shadow` because a resolver can be observed without
/// deciding anything. This gate has no such halfway house: the only call site
/// ruled on is a blocking one, and offering a `shadow` an operator could write
/// while the dispatch path only ever asks *"does this block"* would be a
/// configuration value that silently means `off` — a key that lies is worse
/// than a key that is missing.
///
/// # Why the default is `Off`
///
/// The same argument `approval_envelopes` makes, pointed the other way. This
/// gate **refuses** sends, and four rules that fail closed on every unreadable
/// register are exactly the shape that takes a fleet down when switched on by
/// accident. An operator turns it on once, knowingly; nobody turns it on by
/// forgetting to set a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecipientComplianceMode {
    /// The gate is not consulted at dispatch. Behaviour is byte-for-byte what
    /// it was before this module existed.
    #[default]
    Off,
    /// The gate is consulted at dispatch and its refusal stops the send.
    Blocking,
}

impl RecipientComplianceMode {
    /// A stable label for logs, config and review.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Blocking => "blocking",
        }
    }

    /// Whether the dispatch path must consult the gate and honour its refusal.
    ///
    /// The one question the call site asks, named rather than spelled out
    /// there. Matched exhaustively rather than with `matches!`, so a third arm
    /// added later fails to compile **here** — where somebody has to decide
    /// whether it blocks — instead of being silently classified as `off` at a
    /// dispatch site nobody revisited.
    pub fn blocks(self) -> bool {
        match self {
            Self::Off => false,
            Self::Blocking => true,
        }
    }
}

/// The posture in force for this process. See [`recipient_compliance_mode`].
static MODE: OnceLock<RecipientComplianceMode> = OnceLock::new();

/// Install the posture at boot. `false` means one was already installed.
///
/// The first call wins, so a config reloaded mid-process can neither switch the
/// gate on nor switch it off behind the operator — a send refused at 10:00 and
/// permitted at 10:05 with no act in between is an audit trail nobody can read.
pub fn install_recipient_compliance_mode(mode: RecipientComplianceMode) -> bool {
    MODE.set(mode).is_ok()
}

/// The posture in force.
///
/// **[`RecipientComplianceMode::Off`] when nothing was installed.** A process
/// that failed to wire its config — a test, a partially-initialised binary, an
/// entry point nobody remembered to update — must not thereby begin refusing
/// outward sends over registers it was never told to consult.
///
/// This is the function the dispatch path calls before consulting
/// [`compliance_refusal`]; it is deliberately not consulted *inside* the gate,
/// because the owner-facing route in
/// `magician-api/src/recipient_compliance_api.rs` must be able to ask the four
/// questions whatever the dispatch posture is — a check an owner runs by hand
/// is how one decides whether to switch the gate on at all.
pub fn recipient_compliance_mode() -> RecipientComplianceMode {
    *MODE.get().unwrap_or(&RecipientComplianceMode::Off)
}

/// Read a posture out of configuration.
///
/// `None` means the value names no posture at all — a typo, an old spelling, a
/// mode a newer binary understands and this one does not. Kept distinct from
/// `Some(Off)` so a caller can say out loud that it did not understand, and so
/// the config load can log the fact rather than swallow it.
pub fn parse_recipient_compliance_mode(raw: &str) -> Option<RecipientComplianceMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => Some(RecipientComplianceMode::Off),
        "blocking" => Some(RecipientComplianceMode::Blocking),
        _ => None,
    }
}

/// Install the posture a configuration names, and report the one now in force.
///
/// Called from `config::load_magician_config_from_path` the moment a config is
/// accepted, for the reason `install_configured_envelope_mode` states: the
/// dispatch gate holds no config snapshot, and installing per entry point is
/// exactly how a switch stays unreachable in the one binary nobody remembered.
///
/// An unrecognised value degrades to `Off` rather than failing the config load
/// — and here that is the *permissive* degradation, so it is announced at
/// `warn`, never silently. Guessing at the nearest match would be worse in
/// either direction: reading `block` as `blocking` would start refusing sends
/// on the strength of a misspelling.
///
/// Returns the mode actually in force, which is not necessarily the one
/// requested — see [`install_recipient_compliance_mode`].
pub fn install_configured_recipient_compliance_mode(raw: &str) -> RecipientComplianceMode {
    let requested = match parse_recipient_compliance_mode(raw) {
        Some(mode) => mode,
        None => {
            tracing::warn!(
                configured = %raw,
                "[RECIPIENT-COMPLIANCE] `recipient_compliance.mode` names no posture; reading \
                 it as `off`, so the four recipient checks decide nothing at dispatch"
            );
            RecipientComplianceMode::Off
        },
    };

    let installed = install_recipient_compliance_mode(requested);
    let in_force = recipient_compliance_mode();
    if !installed && in_force != requested {
        tracing::warn!(
            requested = requested.as_str(),
            in_force = in_force.as_str(),
            "[RECIPIENT-COMPLIANCE] a posture was already installed; the first one wins and \
             this request was ignored"
        );
    }

    match in_force {
        RecipientComplianceMode::Off => tracing::info!(
            "[RECIPIENT-COMPLIANCE] the recipient-compliance gate is OFF; the four checks are \
             reachable only through the owner-facing route and decide nothing at dispatch"
        ),
        RecipientComplianceMode::Blocking => tracing::warn!(
            "[RECIPIENT-COMPLIANCE] the recipient-compliance gate is BLOCKING; outward sends \
             will now be refused when a recipient check refuses or cannot be read"
        ),
    }
    in_force
}

// ── The four rules ──────────────────────────────────────────────────────────

/// The four questions [`screen`] asks, in a fixed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceRule {
    /// Has another work contacted this person recently?
    DuplicateRecipient,
    /// Have they already answered, leaving the next move ours?
    ReplyPending,
    /// Does a supplied jurisdiction rule refuse this contact?
    Jurisdiction,
    /// Has this person asked to be forgotten?
    ErasureRequested,
}

impl ComplianceRule {
    /// Every rule, in a fixed order, so reports are deterministic — the same
    /// shape `SuppressionReason::ALL` uses, and for the same reason.
    pub const ALL: [ComplianceRule; 4] = [
        ComplianceRule::DuplicateRecipient,
        ComplianceRule::ReplyPending,
        ComplianceRule::Jurisdiction,
        ComplianceRule::ErasureRequested,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateRecipient => "duplicate_recipient",
            Self::ReplyPending => "reply_pending",
            Self::Jurisdiction => "jurisdiction",
            Self::ErasureRequested => "erasure_requested",
        }
    }

    /// What [`RuleFinding::examined`] counts for this rule.
    ///
    /// Reported rather than assumed: "examined 0" means opposite things for a
    /// rule that walks an index and one that asks a port per recipient, and a
    /// reader with one bare number cannot tell a clean answer from a rule that
    /// looked at nothing.
    pub fn unit_examined(self) -> &'static str {
        match self {
            Self::DuplicateRecipient => "recipient-axis index entries",
            Self::ReplyPending => "negotiations in this scope",
            Self::Jurisdiction => "recipients put to the jurisdiction rule",
            Self::ErasureRequested => "recipients looked up in the erasure log",
        }
    }
}

/// What one rule concluded.
///
/// [`Self::NotAssessed`] and [`Self::Clear`] are deliberately different arms. A
/// jurisdiction nothing ruled on must never read as a jurisdiction that cleared
/// the send, and a duplicate rule that could not define "another work" must
/// never read as one that looked and found none.
///
/// [`Self::Unreadable`] refuses, like [`Self::Refused`], and is still kept
/// apart from it: *"they are off limits"* and *"we could not check"* are
/// opposite facts and only one of them is actionable by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleVerdict {
    /// The rule looked and found nothing that refuses.
    Clear,
    /// The rule refuses, on the evidence it carries.
    Refused(Box<Refusal>),
    /// The rule could not be applied at all. Does **not** refuse, and does not
    /// clear either — `why` says which of the two it is short of.
    NotAssessed { why: &'static str },
    /// The rule's own inputs could not be read. Refuses.
    Unreadable { cause: String },
}

impl RuleVerdict {
    /// A stable label for logs and review.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Refused(_) => "refused",
            Self::NotAssessed { .. } => "not_assessed",
            Self::Unreadable { .. } => "unreadable",
        }
    }

    /// Whether this verdict stops the send.
    ///
    /// `Unreadable` stops it: an unconsultable register is never an empty one.
    pub fn refuses(&self) -> bool {
        matches!(self, Self::Refused(_) | Self::Unreadable { .. })
    }
}

/// One rule's answer, with how much it actually looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFinding {
    pub rule: ComplianceRule,
    pub verdict: RuleVerdict,
    /// How many records this rule inspected, in the unit
    /// [`ComplianceRule::unit_examined`] names.
    ///
    /// Present on every finding, whatever the verdict, so a `Clear` that
    /// examined nothing is visibly a `Clear` that examined nothing rather than
    /// a clean bill of health.
    pub examined: usize,
}

// ── The evidence a refusal rests on ─────────────────────────────────────────

/// One earlier outward act to the same person, by a work that is not this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriorContact {
    /// The identity spelling the index was found under — **not** normalised,
    /// because that is the spelling the earlier act actually used and an owner
    /// tracing the collision has to search for it.
    pub identity_as_indexed: String,
    pub outward_act_ref: String,
    /// The work (or works) the earlier act named. Plural because a disclosure
    /// can carry both a `program_id` and an `engagement_id`, and reporting one
    /// would hide the other from the owner who has to resolve the collision.
    pub works: Vec<WorkContextKind>,
    pub channel: OutwardChannel,
    pub prepared_at: DateTime<Utc>,
    pub status: OutwardActStatus,
}

/// What rule 1's walk of the recipient axis found, in counts that reconcile.
///
/// Counts, never rates. And every entry the index offered ends under exactly
/// one bucket: [`Self::accounted_for`] is asserted equal to [`Self::entries_seen`]
/// by test, the same discipline `HygieneSweep::accounted_for` holds to. A
/// silently dropped entry is a real contact turning into a clean pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DuplicateScan {
    /// Distinct ids the recipient axis offered, across every spelling probed.
    pub entries_seen: usize,
    /// Index entries this scan did NOT open, because the cap was reached.
    ///
    /// Non-zero means the answer is partial. A partial scan that found no
    /// collision has not established there is none, so rule 1 refuses rather
    /// than reporting `Clear` — see [`MAX_DUPLICATE_ACTS_EXAMINED`].
    pub not_examined: usize,
    /// How many spellings of the identity were probed. Not a bucket — see the
    /// module note on the unnormalised index.
    pub spellings_probed: usize,
    /// The act currently being screened, skipped. The disclosure is written
    /// before every gate runs, so the act is always in the index this rule
    /// walks; see [`duplicate_contacts`] for exactly what the skip buys, which
    /// is an honest count rather than a refusal that would otherwise fire.
    pub own_act: usize,
    /// Ids that resolved to no act. The recipient axis is mixed — it holds
    /// assertion-use ids as well as act refs — so this is the normal case, not
    /// a fault.
    pub not_an_act: usize,
    /// Acts whose `prepared_at` would not parse. Counted, never dropped: a
    /// dropped one is a real contact reading as no contact.
    ///
    pub unreadable_timestamp: usize,
    /// How many of [`Self::unreadable_timestamp`] would have been collisions if
    /// only the clock had parsed — an **active** disclosure naming a work this
    /// act does not name.
    ///
    /// **Not a bucket**: it is a subset of `unreadable_timestamp`, so it stays
    /// out of [`Self::accounted_for`]. It exists because it is the only one of
    /// the two counts that may refuse. An act still merely `prepared`, or one
    /// this act's own work performed, is not a collision whatever its clock
    /// says, and poisoning the rule on it would refuse a send over a record
    /// that could never have refused it. One that could have mattered is a
    /// contact this rule neither found nor ruled out, and [`screen`] answers
    /// [`RuleVerdict::Unreadable`] for it rather than [`RuleVerdict::Clear`] —
    /// otherwise the count is the only place the fact survives, and nothing on
    /// the dispatch path reads counts.
    pub unplaceable_collisions: usize,
    pub outside_window: usize,
    /// Acts that told nobody anything — failed, retracted, still merely
    /// prepared. `OutwardActStatus::is_active_disclosure` is the store's own
    /// rule, read here rather than re-derived.
    pub inactive_disclosure: usize,
    /// Acts naming a work this act also names. Not a collision — one work
    /// contacting the same person twice is a conversation.
    pub same_work: usize,
    /// Acts whose record names no work at all. **Not a collision**: an act that
    /// names no work cannot name a *colliding* work, and guessing one from the
    /// recipient would file a real act under a relationship nobody chose — the
    /// same refusal `WorkAxisBackfill::unattributed` already makes.
    pub unattributed: usize,
    /// The contacts that would refuse, one per act.
    pub collisions: Vec<PriorContact>,
}

impl DuplicateScan {
    /// How many of the entries seen ended under a named outcome.
    ///
    /// Equal to [`Self::entries_seen`] by construction and asserted against it
    /// in tests. A gap means entries the index offered that this report cannot
    /// say what became of.
    pub fn accounted_for(&self) -> usize {
        self.own_act
            + self.not_an_act
            + self.unreadable_timestamp
            + self.outside_window
            + self.inactive_disclosure
            + self.same_work
            + self.unattributed
            + self.collisions.len()
            // A capped entry is an OUTCOME — "not looked at" — not a gap in the
            // accounting. Left out, a capped scan would report fewer outcomes
            // than entries and the reconciliation that guards this whole
            // subsystem would fail on a scan that is behaving correctly.
            + self.not_examined
    }

    /// Fold another identity's scan into this one.
    fn absorb(&mut self, other: DuplicateScan) {
        self.entries_seen += other.entries_seen;
        // Summed like every other count, or a scan capped on one spelling would
        // vanish behind another that was not.
        self.not_examined += other.not_examined;
        self.spellings_probed += other.spellings_probed;
        self.own_act += other.own_act;
        self.not_an_act += other.not_an_act;
        self.unreadable_timestamp += other.unreadable_timestamp;
        self.unplaceable_collisions += other.unplaceable_collisions;
        self.outside_window += other.outside_window;
        self.inactive_disclosure += other.inactive_disclosure;
        self.same_work += other.same_work;
        self.unattributed += other.unattributed;
        self.collisions.extend(other.collisions);
    }
}

/// One ask whose latest word is theirs, so the next move is ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingReply {
    /// The counterparty, normalised — this is the key the index is built on.
    pub identity: String,
    pub negotiation_id: String,
    /// The relationship the ask lives in. This is what "owns" it, and the
    /// ownership test rule 2 turns on.
    pub audience: AudienceRef,
    pub purpose: String,
    pub replied_at: DateTime<Utc>,
    /// `accepted` or `countered` — the two states where the latest word is
    /// theirs and the next move is ours.
    pub reply_kind: String,
}

/// What building the reply index found, in counts that reconcile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplyScan {
    pub negotiations_seen: usize,
    /// Held or closed — settled, nothing more may be absorbed.
    pub settled: usize,
    /// Nobody has replied, so nobody is waiting on us. Not this rule's
    /// business: chasing silence is `scheduling`'s Module D, not a refusal.
    pub awaiting_reply: usize,
    /// A decline **is** an answer, so nobody is left waiting on us — the same
    /// reading `reply_routing` records, where a declined ask is deliberately
    /// not settled but is also not open work for us.
    pub declined: usize,
    /// Accepted or countered: the latest word is theirs.
    pub awaiting_our_move: usize,
    /// A negotiation whose latest word is theirs and which this rule could not
    /// key. `scheduling` deliberately does not normalise identities and only
    /// refuses a blank counterparty, so both sides must be normalised here —
    /// and one of these means a live ask that cannot be matched by key, which
    /// is "we could not check", not "clear".
    ///
    /// The count is kept beside [`Self::unmatchable`] and never derived from
    /// it at a second site: both are written by
    /// `ReplyIndex::record_unmatchable`, which is the only place either
    /// moves, so they cannot drift.
    pub unmatchable_counterparty: usize,
    /// The rows behind [`Self::unmatchable_counterparty`], named.
    ///
    /// A count is a number nobody can act on. The row that stops this rule is
    /// one row in one audience's log, and until the report says **which** row,
    /// the only way to find it is to read every negotiation in the workspace by
    /// hand. Carried on the scan so it reaches both the send-time refusal and
    /// `GET /recipient-compliance/check`, which renders the whole scan.
    pub unmatchable: Vec<UnmatchableAsk>,
}

impl ReplyScan {
    /// Every negotiation ends under exactly one bucket.
    pub fn accounted_for(&self) -> usize {
        self.settled
            + self.awaiting_reply
            + self.declined
            + self.awaiting_our_move
            + self.unmatchable_counterparty
    }
}

/// One live ask this rule could not key, named so somebody can repair it.
///
/// # What an unreadable counterparty can and cannot tell you
///
/// It cannot tell you who the ask is with — that is what "unreadable" means.
/// It can still tell you two things, and this record carries both: the
/// **relationship** the ask lives in, which is readable and is what
/// `work_owns` is tested against; and whatever an address recovered from the
/// raw string could plausibly be, via `recoverable_identities`.
///
/// It must never simply be ignored. The row might be about the very recipient
/// in front of us, and *"we could not check"* is never permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnmatchableAsk {
    pub negotiation_id: String,
    /// The relationship the ask lives in — the one field of an unreadable row
    /// that is itself readable.
    pub audience: AudienceRef,
    /// The counterparty as the scheduling log holds it, with every control
    /// character rendered as an escape and the whole thing bounded.
    ///
    /// Escaped rather than repeated verbatim because the fault being reported
    /// **is** that this string carries control characters, and a refusal
    /// travels into log lines, an owner's terminal and a model's prompt. See
    /// `escaped_for_display`.
    pub counterparty_as_recorded: String,
    pub purpose: String,
    /// The identities this row could still be about, normalised and in a fixed
    /// order.
    ///
    /// **Empty means "could be anyone"** — the opposite of "about nobody". See
    /// [`Self::could_be_about`], which is where that distinction decides
    /// something.
    pub candidate_identities: Vec<String>,
    /// Why the counterparty could not be keyed, in the normaliser's own words.
    pub why: String,
}

impl UnmatchableAsk {
    /// Whether this row could be about any of the identities being screened.
    ///
    /// Three cases, and the middle one is the whole point of this type:
    ///
    /// - **Nothing recoverable** → `true`. A row we cannot read even
    ///   approximately might be about the recipient in front of us.
    /// - **Recovered, and a candidate is one of them** → `true`. The row could
    ///   be exactly this person, and the rule still cannot say whether they are
    ///   waiting on us.
    /// - **Recovered, and every candidate names somebody else** → `false`. This
    ///   is the only case where the row demonstrably has nothing to say about
    ///   this send, and it is what keeps one malformed row from refusing every
    ///   send in the workspace.
    pub fn could_be_about(&self, screened: &BTreeSet<String>) -> bool {
        if self.candidate_identities.is_empty() {
            return true;
        }
        self.candidate_identities
            .iter()
            .any(|candidate| screened.contains(candidate))
    }

    /// The sentence an owner reads to go and find this row.
    ///
    /// **Every caller-supplied part is bounded here, not just the recorded
    /// counterparty.** The recovered candidates are derived from that same raw
    /// string, so rendering them whole would put back into this very sentence
    /// the megabyte `MAX_RENDERED_COUNTERPARTY` exists to keep out of it — and
    /// `purpose` and the audience id are caller-supplied too, since
    /// `SchedulingStore::open` checks only that they are non-blank. A bound
    /// that a neighbouring field in the same sentence walks around is not a
    /// bound.
    ///
    /// Only the **rendering** is bounded. `candidate_identities` itself stays
    /// whole, because [`Self::could_be_about`] matches over it and a truncated
    /// candidate list would narrow this row away from a recipient it really
    /// could be about — the one narrowing here that would be fail-open.
    fn describe(&self) -> String {
        let reach = if self.candidate_identities.is_empty() {
            "no address could be recovered from it, so it could be about anyone".to_string()
        } else {
            let listed = self.candidate_identities.len().min(MAX_NAMED_CANDIDATES);
            let named = self.candidate_identities[..listed]
                .iter()
                .map(|candidate| escaped_for_display(candidate))
                .collect::<Vec<_>>()
                .join(" or ");
            let rest = if self.candidate_identities.len() > listed {
                format!(
                    " (and {} more on `reply_scan.unmatchable`)",
                    self.candidate_identities.len() - listed
                )
            } else {
                String::new()
            };
            format!("it could be about {named}{rest}")
        };
        format!(
            "negotiation `{}` in `{}` about `{}`, counterparty as recorded `{}` — {}; {}",
            self.negotiation_id,
            escaped_for_display(&self.audience.as_key()),
            escaped_for_display(&self.purpose),
            self.counterparty_as_recorded,
            self.why,
            reach
        )
    }
}

/// The identity → pending-ask index, built **once** per [`screen`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyIndex {
    by_identity: BTreeMap<String, Vec<PendingReply>>,
    pub scan: ReplyScan,
}

/// Scheduling facts required by recipient compliance, projected by the media
/// satellite without exposing its append-only store to the core crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplianceNegotiationState {
    AwaitingReply,
    Accepted,
    Declined,
    Countered,
    Held,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceReply {
    pub at: DateTime<Utc>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceNegotiation {
    pub negotiation_id: String,
    pub audience: AudienceRef,
    pub counterparty: String,
    pub purpose: String,
    pub state: ComplianceNegotiationState,
    pub latest_reply: Option<ComplianceReply>,
}

/// Read-only port implemented by the extracted scheduling owner. Absence and
/// read failure are both fail-closed at the compliance rule.
pub trait ComplianceSchedulingReader: Send + Sync {
    fn negotiations(
        &self,
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ComplianceNegotiation>>;
}

static COMPLIANCE_SCHEDULING_READER: OnceLock<Arc<dyn ComplianceSchedulingReader>> =
    OnceLock::new();

pub fn install_compliance_scheduling_reader(reader: Arc<dyn ComplianceSchedulingReader>) -> bool {
    COMPLIANCE_SCHEDULING_READER.set(reader).is_ok()
}

fn configured_negotiations(
    workspace_layout: &ArtifactV2Workspace,
    scope: &ComplianceScope,
) -> Result<Vec<ComplianceNegotiation>> {
    COMPLIANCE_SCHEDULING_READER
        .get()
        .ok_or_else(|| anyhow::anyhow!("the scheduling compliance reader is not installed"))?
        .negotiations(
            workspace_layout,
            scope.principal.as_str(),
            scope.workspace.as_str(),
        )
}

impl ReplyIndex {
    /// Every ask in which this identity is waiting on us.
    pub fn pending_for(&self, identity: &str) -> &[PendingReply] {
        self.by_identity
            .get(identity)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Record one ask this rule could not key.
    ///
    /// The **only** writer of either half, so the count and the rows can never
    /// disagree about how many there were — the failure a report whose number
    /// and whose list contradict each other invites is a reader who stops
    /// trusting every other figure beside them.
    fn record_unmatchable(&mut self, ask: UnmatchableAsk) {
        self.scan.unmatchable_counterparty += 1;
        self.scan.unmatchable.push(ask);
    }
}

/// One jurisdiction ruling that refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JurisdictionRefusal {
    pub identity: String,
    /// Which rule refused, by its own name — so an owner knows what to go and
    /// read, and so two rules cannot be confused for one.
    pub rule_name: String,
    pub detail: String,
}

/// One recorded request to be forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasureRequest {
    /// Derived from `(principal, workspace, identity, evidence_ref)`, never
    /// assigned, so a replayed request resumes rather than stacking.
    pub request_id: String,
    /// **Normalised** — the same canonical form the suppression register
    /// stores, so a request filed under one spelling is found under every
    /// spelling of the same address.
    pub identity: String,
    pub requested_at: DateTime<Utc>,
    /// What an auditor follows to check the request was really made. Never
    /// blank, for the reason `validated_evidence` gives in `suppression`: a
    /// record nobody can check is an assertion, and the first person to doubt
    /// it removes it.
    pub evidence_ref: String,
    /// Who recorded it. Never blank — a log that cannot answer who acted cannot
    /// be reviewed.
    pub recorded_by: String,
}

/// Why one rule refused, with the evidence it refused on.
///
/// One arm per [`ComplianceRule`], each carrying **every** identity that
/// triggered it rather than the first: a refusal naming one of three recipients
/// invites the caller to retry with the other two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
// Tagged `refusal`, not `rule`: a rendered finding already carries a `rule`
// key holding the `ComplianceRule` token, and two different values under one
// key name at two nesting levels is a reader's trap.
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    ContactedByOtherWork { contacts: Vec<PriorContact> },
    AwaitingOurReply { pending: Vec<PendingReply> },
    JurisdictionRefused { rulings: Vec<JurisdictionRefusal> },
    ErasureRequested { requests: Vec<ErasureRequest> },
}

impl Refusal {
    /// The rule this refusal belongs to. Derived rather than stored beside it,
    /// so the two cannot disagree.
    pub fn rule(&self) -> ComplianceRule {
        match self {
            Self::ContactedByOtherWork { .. } => ComplianceRule::DuplicateRecipient,
            Self::AwaitingOurReply { .. } => ComplianceRule::ReplyPending,
            Self::JurisdictionRefused { .. } => ComplianceRule::Jurisdiction,
            Self::ErasureRequested { .. } => ComplianceRule::ErasureRequested,
        }
    }

    /// The sentence an owner — and the model — reads.
    pub fn detail(&self) -> String {
        match self {
            Self::ContactedByOtherWork { contacts } => {
                let mut named: Vec<String> = contacts
                    .iter()
                    .map(|contact| {
                        format!(
                            "`{}` was contacted on {} by {} (act `{}`, {})",
                            contact.identity_as_indexed,
                            contact.prepared_at.to_rfc3339(),
                            contact
                                .works
                                .iter()
                                .map(WorkContextKind::as_key)
                                .collect::<Vec<_>>()
                                .join(" and "),
                            contact.outward_act_ref,
                            contact.channel.as_str(),
                        )
                    })
                    .collect();
                named.sort();
                // EARLIER CONTACTS, not recipients: three acts to one person is
                // one recipient, and `contacts.len()` is the number of acts.
                // Saying "3 of this action's recipients" over a one-recipient
                // send is a count that does not reconcile against the list
                // beside it, and a reader who checks is told to stop trusting
                // every other number this gate reports. The recipient count is
                // deliberately NOT derived here: `identity_as_indexed` is the
                // spelling the earlier act used, so two spellings of one
                // address would count as two people.
                format!(
                    "Another work already made {} earlier contact(s) with this action's \
                     recipients inside the duplicate-contact window: {}. Two works contacting \
                     one person without knowing about each other is the collision this gate \
                     exists to surface — take it to whoever owns both, do not retry, and do not \
                     reach the same person through a different address.",
                    named.len(),
                    named.join("; ")
                )
            },
            Self::AwaitingOurReply { pending } => {
                let mut named: Vec<String> = pending
                    .iter()
                    .map(|ask| {
                        format!(
                            "`{}` {} on {} about `{}` (ask `{}` in {})",
                            ask.identity,
                            ask.reply_kind,
                            ask.replied_at.to_rfc3339(),
                            ask.purpose,
                            ask.negotiation_id,
                            ask.audience.as_key(),
                        )
                    })
                    .collect();
                named.sort();
                // Distinct RECIPIENTS, counted off the normalised identity —
                // one person with two open asks is one recipient, and
                // `pending.len()` is the number of asks. Both numbers are said,
                // because "two people are waiting" and "one person is waiting
                // about two things" are different facts and the reader has to
                // be able to check either against the list.
                let recipients = pending
                    .iter()
                    .map(|ask| ask.identity.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                format!(
                    "{recipients} of this action's recipients have already answered and are \
                     waiting on US, across {} ask(s): {}. A follow-up to somebody whose reply we \
                     have not acted on reads as not having been read. Answer the ask in the \
                     relationship that owns it instead.",
                    named.len(),
                    named.join("; ")
                )
            },
            Self::JurisdictionRefused { rulings } => {
                let mut named: Vec<String> = rulings
                    .iter()
                    .map(|ruling| {
                        format!(
                            "`{}` refused by `{}`: {}",
                            ruling.identity, ruling.rule_name, ruling.detail
                        )
                    })
                    .collect();
                named.sort();
                format!(
                    "A jurisdiction rule refuses {} of this action's recipients: {}. This \
                     runtime does not decide what the rule means — it was supplied, and it \
                     said no.",
                    named.len(),
                    named.join("; ")
                )
            },
            Self::ErasureRequested { requests } => {
                let mut named: Vec<String> = requests
                    .iter()
                    .map(|request| {
                        format!(
                            "`{}` asked on {} (request `{}`, evidence `{}`)",
                            request.identity,
                            request.requested_at.to_rfc3339(),
                            request.request_id,
                            request.evidence_ref,
                        )
                    })
                    .collect();
                named.sort();
                // Distinct RECIPIENTS, counted off the normalised identity. One
                // person may file twice — a form and a follow-up mail are two
                // acts of consent under two evidence refs, so they are two rows
                // — and `requests.len()` is the number of rows. A permanent
                // refusal that says "2 of this action's recipients" over a
                // one-recipient send is a notice whose own arithmetic an owner
                // can disprove from the list beside it.
                let recipients = requests
                    .iter()
                    .map(|request| request.identity.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                format!(
                    "{recipients} of this action's recipients have asked to be forgotten, across \
                     {} recorded request(s): {}. A recorded erasure request refuses contact \
                     permanently and there is no path here that lifts it. Do not retry and do \
                     not reach the same person through a different address.",
                    named.len(),
                    named.join("; ")
                )
            },
        }
    }
}

// ── The decision ────────────────────────────────────────────────────────────

/// The answer [`screen`] gives: four questions, four answers, and the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientComplianceDecision {
    /// The normalised, de-duplicated identities the four rules were asked
    /// about. Carried in full rather than as a count: a caller comparing this
    /// against what it is about to send is the only way to catch a screen that
    /// covered fewer people than the send does.
    pub identities: Vec<String>,
    /// Always four, one per [`ComplianceRule::ALL`], in that order.
    pub findings: Vec<RuleFinding>,
    /// Rule 1's walk, summed across identities.
    pub duplicate_scan: DuplicateScan,
    /// Rule 2's index, built once for the whole call.
    pub reply_scan: ReplyScan,
}

impl RecipientComplianceDecision {
    /// Whether the act may continue to the next gate.
    ///
    /// Read off every finding, never off `refusing_rules().is_empty()` — the
    /// reason `Screened` gives about `sendable` versus `blocked.is_empty()`: a
    /// decision that somehow refused nobody *and* cleared nobody must not read
    /// as permission.
    pub fn is_clear(&self) -> bool {
        self.findings.len() == ComplianceRule::ALL.len()
            && self.findings.iter().all(|finding| {
                matches!(
                    finding.verdict,
                    RuleVerdict::Clear | RuleVerdict::NotAssessed { .. }
                )
            })
    }

    /// One finding per rule, by rule.
    pub fn finding(&self, rule: ComplianceRule) -> Option<&RuleFinding> {
        self.findings.iter().find(|finding| finding.rule == rule)
    }

    /// Which rules refused, in [`ComplianceRule::ALL`] order.
    pub fn refusing_rules(&self) -> Vec<ComplianceRule> {
        self.findings
            .iter()
            .filter(|finding| finding.verdict.refuses())
            .map(|finding| finding.rule)
            .collect()
    }

    /// Which rules could not be applied, in [`ComplianceRule::ALL`] order.
    ///
    /// Reported beside the refusals rather than folded into them, because a
    /// clear decision with three unassessed rules and a clear decision with
    /// four assessed ones are different facts and only one is reassuring.
    pub fn not_assessed_rules(&self) -> Vec<(ComplianceRule, &'static str)> {
        self.findings
            .iter()
            .filter_map(|finding| match finding.verdict {
                RuleVerdict::NotAssessed { why } => Some((finding.rule, why)),
                _ => None,
            })
            .collect()
    }

    /// The refusal a caller hands back, or `None` when the act may continue.
    pub fn refusal_message(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for finding in &self.findings {
            match &finding.verdict {
                RuleVerdict::Refused(refusal) => {
                    parts.push(format!("[{}] {}", finding.rule.as_str(), refusal.detail()));
                },
                RuleVerdict::Unreadable { cause } => parts.push(format!(
                    "[{}] this rule's own records could not be read, and `we could not check` is \
                     never permission. Cause: {cause}",
                    finding.rule.as_str()
                )),
                RuleVerdict::Clear | RuleVerdict::NotAssessed { .. } => {},
            }
        }
        if parts.is_empty() {
            // Not `refusing_rules().is_empty()`: a decision carrying fewer than
            // four findings has not asked four questions, and must not pass.
            if self.is_clear() {
                return None;
            }
            return Some(format!(
                "{REFUSAL_PREFIX} The recipient-compliance gate returned {} of {} findings, so \
                 at least one of its four rules was never answered. A decision that did not ask \
                 every question is not a clear one.",
                self.findings.len(),
                ComplianceRule::ALL.len()
            ));
        }
        let unassessed: Vec<String> = self
            .not_assessed_rules()
            .into_iter()
            .map(|(rule, why)| format!("{} ({why})", rule.as_str()))
            .collect();
        let tail = if unassessed.is_empty() {
            String::new()
        } else {
            format!(
                " Also worth knowing: {} of the four rules could not be applied at all — {}.",
                unassessed.len(),
                unassessed.join(", ")
            )
        };
        Some(format!("{REFUSAL_PREFIX} {}{tail}", parts.join(" ")))
    }
}

// ── Policy ──────────────────────────────────────────────────────────────────

/// The one knob this gate has, and the reason it is a type rather than an
/// argument: a window supplied at each call site is a window that differs
/// between the send-time gate and the owner-facing route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompliancePolicy {
    duplicate_window: Duration,
}

impl CompliancePolicy {
    /// **Refuses a window that is zero or negative.**
    ///
    /// A non-positive window makes rule 1 vacuous: every prior contact falls
    /// outside it, so the rule reports a clean pass having decided nothing, and
    /// a clean pass is indistinguishable from a real one — the same refusal
    /// `IntroducerPolicy::new` makes about its own window.
    pub fn new(duplicate_window: Duration) -> Result<Self> {
        if duplicate_window <= Duration::zero() {
            anyhow::bail!(
                "the duplicate-contact window must be positive; `{duplicate_window:?}` places \
                 every prior contact outside it, so the rule would report a clean pass having \
                 examined everything and decided nothing"
            );
        }
        Ok(Self { duplicate_window })
    }

    /// The window the send-time gate uses. See [`DUPLICATE_CONTACT_WINDOW_DAYS`].
    pub fn standard() -> Self {
        Self {
            duplicate_window: Duration::days(DUPLICATE_CONTACT_WINDOW_DAYS),
        }
    }

    pub fn duplicate_window(&self) -> Duration {
        self.duplicate_window
    }
}

// ── Rule 3: the jurisdiction port ───────────────────────────────────────────

/// What a jurisdiction rule is asked.
#[derive(Debug, Clone, Copy)]
pub struct JurisdictionQuery<'a> {
    pub identity: &'a str,
    /// The channel the act would travel on, when the caller knows it. `None` is
    /// "we do not know", never "any" — a rule that must not answer without a
    /// channel returns [`JurisdictionRuling::NoRuleApplies`] and says so.
    pub channel: Option<OutwardChannel>,
    pub work: Option<&'a WorkContextKind>,
    pub now: DateTime<Utc>,
}

/// What a jurisdiction rule answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JurisdictionRuling {
    /// A rule looked at this contact and permits it.
    Permitted,
    /// A rule looked at this contact and refuses it.
    Refused { detail: String },
    /// No rule in this implementation has anything to say about this contact.
    /// **Not** permission — see the mapping in [`screen`].
    NoRuleApplies,
}

/// The **port** a jurisdiction rule is supplied through.
///
/// A port, in the shape `delivery_hygiene::receipts::ReceiptPuller` and
/// `run_inbox::RunInbox` already use here, and for the same reason: a quiet
/// period, a consent regime, a country this owner may not contact into and a
/// court order are all the same shape from this module, and **nothing in this
/// file learns any of their names**.
///
/// There is deliberately no legal rule anywhere in this module — no country
/// list, no quiet hours, no consent regime, no opt-in registry. Encoding one
/// would be this codebase deciding a question that is not its to decide, and
/// the rule it guessed would be right in one jurisdiction and wrong in the next.
pub trait JurisdictionRule: Send + Sync {
    /// This rule's own name, as it appears in a refusal an owner reads.
    fn name(&self) -> &str;

    /// Rule on one contact.
    ///
    /// `Err` is *"this rule could not answer"*, and [`screen`] maps it to
    /// [`RuleVerdict::Unreadable`], which refuses — a rule that could not
    /// answer has not answered yes.
    fn ruling(&self, query: &JurisdictionQuery<'_>) -> Result<JurisdictionRuling>;
}

/// The conservative default: no jurisdiction rule has been supplied.
///
/// It answers [`JurisdictionRuling::NoRuleApplies`] for everything, which
/// [`screen`] maps to [`RuleVerdict::NotAssessed`] — it does not refuse, and it
/// does not clear.
///
/// The two alternatives were both considered and both are worse. A default that
/// **refused** would refuse every outward send in this codebase and would be
/// deleted within a week, taking the port with it. A default that reported
/// [`RuleVerdict::Clear`] would let *"nothing looked"* render as *"a
/// jurisdiction cleared this"*, which is exactly the reassuring wrong answer
/// every fail-closed rule in this subsystem exists to avoid.
pub struct NoJurisdictionRule;

impl JurisdictionRule for NoJurisdictionRule {
    fn name(&self) -> &str {
        "none_supplied"
    }

    fn ruling(&self, _query: &JurisdictionQuery<'_>) -> Result<JurisdictionRuling> {
        Ok(JurisdictionRuling::NoRuleApplies)
    }
}

// ── Scope ───────────────────────────────────────────────────────────────────

/// Whose records this gate reads: one owner, one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceScope {
    pub principal: String,
    pub workspace: String,
}

impl ComplianceScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }

    pub fn outward(&self) -> OutwardScope {
        OutwardScope::new(self.principal.clone(), self.workspace.clone())
    }

    pub fn suppression(&self) -> SuppressionScope {
        SuppressionScope::new(self.principal.clone(), self.workspace.clone())
    }

    /// The same guard every scope in this subsystem applies, for the same
    /// reason: a scope carrying the id separator could address another owner's
    /// records.
    fn validate(&self) -> Result<()> {
        if self.principal.trim().is_empty() || self.workspace.trim().is_empty() {
            anyhow::bail!(
                "a compliance scope must name a principal and a workspace; a blank one would \
                 read a register nobody owns and report it as this owner's"
            );
        }
        if self.principal.contains(FIELD_SEP) || self.workspace.contains(FIELD_SEP) {
            anyhow::bail!(
                "a scope's principal and workspace must not contain U+001F: it is the separator \
                 derived ids are joined with, and a crafted scope could otherwise resume another \
                 owner's record"
            );
        }
        Ok(())
    }
}

// ── Rule 4: the erasure request log ─────────────────────────────────────────

/// Append-only log of who has asked to be forgotten.
///
/// Laid out exactly like the suppression register — one JSONL per identity
/// under `identities/`, with a sibling `index.jsonl` naming every identity —
/// because it is read under the same conditions and must fail the same way.
///
/// # There is no delete, and no fulfilment path that lifts a refusal
///
/// **Any recorded request refuses contact forever.** There is no
/// `mark_fulfilled` that re-permits, and there is no `remove`. The reason is
/// the rule the suppression register already enforces — *"stop a suppression
/// applying: the only way, and never by deletion"* — and it is exactly as
/// load-bearing here: the request row **is** the evidence the refusal rests on,
/// so deleting it in the name of honouring the request would resurrect the
/// contact the request asked to end. The person who asked to be forgotten would
/// be the first one contacted again.
///
/// What the request *does* reach is a separate question, and
/// [`erasure_proposal`] is where it is answered — by proposing, never by
/// purging.
#[derive(Debug, Clone)]
pub struct ErasureRequestLog {
    workspace_layout: ArtifactV2Workspace,
}

impl ErasureRequestLog {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &ComplianceScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("recipient_compliance")
            .join("erasure_requests")
    }

    /// One identity's log. The file name is `blake3(identity)` so a
    /// caller-supplied address can never traverse out of the scope root.
    fn identity_path(&self, scope: &ComplianceScope, identity: &str) -> PathBuf {
        self.root(scope)
            .join("identities")
            .join(format!("{}.jsonl", stable_id(identity)))
    }

    /// The roster of identities. In a directory of its own above
    /// `identities/`, so a listing of the identity logs can never pick it up
    /// and try to parse it as one.
    fn index_path(&self, scope: &ComplianceScope) -> PathBuf {
        self.root(scope).join("index.jsonl")
    }

    /// Record that somebody asked to be forgotten.
    ///
    /// Idempotent per `(identity, evidence_ref)`: replaying one request — a
    /// form re-submitted, an import re-run — resumes the row already written
    /// and keeps its original `requested_at`. A replay whose *payload* differs
    /// under the same evidence is an **error**, not a quiet no-op: two accounts
    /// of one act have to be reconciled by a person, and swallowing the second
    /// would drop a correction.
    pub fn record(
        &self,
        scope: &ComplianceScope,
        identity: &str,
        requested_at: DateTime<Utc>,
        evidence_ref: &str,
        recorded_by: &str,
    ) -> Result<ErasureRequest> {
        scope.validate()?;
        let identity = normalise_identity(identity)?;
        let evidence_ref = validated_field(evidence_ref, "evidence ref")?;
        let recorded_by = validated_field(recorded_by, "recorder's name")?;
        let request_id = derive_request_id(scope, &identity, &evidence_ref);

        let request = ErasureRequest {
            request_id,
            identity: identity.clone(),
            requested_at,
            evidence_ref,
            recorded_by,
        };

        for existing in self.requests_for(scope, &identity)? {
            if existing.request_id == request.request_id {
                if existing != request {
                    anyhow::bail!(
                        "an erasure request for `{identity}` already exists under evidence \
                         `{}` with a different payload. Two different accounts of one act must \
                         be reconciled by a person; recording the second silently would drop \
                         whichever of them is the correction",
                        existing.evidence_ref
                    );
                }
                return Ok(existing);
            }
        }

        // Index BEFORE the row, the same ordering every store in this subsystem
        // uses: "the row exists" must imply "the roster names it". The other
        // order leaves a window in which a request exists that no listing can
        // find, and a refusal nobody can enumerate is one nobody can audit.
        self.append_identity_index(scope, &identity)?;
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        let path = self.identity_path(scope, &identity);
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending erasure request {}", path.display()))?;
        Ok(request)
    }

    /// Every request ever recorded for one identity, oldest first.
    ///
    /// **Absent is empty; unreadable is not.** A missing file is an identity
    /// nobody asked about; every other failure propagates, because answering
    /// *"nobody asked to be forgotten"* from a file we could not open is the
    /// reassuring wrong answer this whole module exists to avoid.
    pub fn requests_for(
        &self,
        scope: &ComplianceScope,
        identity: &str,
    ) -> Result<Vec<ErasureRequest>> {
        scope.validate()?;
        let identity = normalise_identity(identity)?;
        let path = self.identity_path(scope, &identity);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Vec::new());
        };
        let rows: Vec<ErasureRequest> = crate::magician_v2::jsonl::parse_log_lines(&raw, &path)?;
        let mut seen = BTreeSet::new();
        let mut out: Vec<ErasureRequest> = rows
            .into_iter()
            .filter(|row| seen.insert(row.request_id.clone()))
            .collect();
        out.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        Ok(out)
    }

    /// Every identity that has ever asked, sorted.
    pub fn identities(&self, scope: &ComplianceScope) -> Result<Vec<String>> {
        scope.validate()?;
        let path = self.index_path(scope);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Vec::new());
        };
        let rows: Vec<String> = crate::magician_v2::jsonl::parse_log_lines(&raw, &path)?;
        // `BTreeSet` both de-duplicates and orders, so two reads of an
        // unchanged roster name the identities in the same order.
        Ok(rows
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    /// Every request in the scope, oldest first.
    ///
    /// One read of the roster and then one read per identity — N+1 by layout,
    /// and deliberately so: the send-time question is always *"this one
    /// person"*, which is one file, and a single flat log would make the read
    /// that actually runs on every send scan the whole register.
    pub fn all_requests(&self, scope: &ComplianceScope) -> Result<Vec<ErasureRequest>> {
        let mut out = Vec::new();
        for identity in self.identities(scope)? {
            out.extend(self.requests_for(scope, &identity)?);
        }
        out.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        Ok(out)
    }

    fn append_identity_index(&self, scope: &ComplianceScope, identity: &str) -> Result<()> {
        let path = self.index_path(scope);
        // JSON-encoded rather than raw, so nothing in an address can shear the
        // line format — the same encoding `suppression` uses for its roster.
        let mut line = serde_json::to_vec(identity)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending erasure index {}", path.display()))?;
        Ok(())
    }
}

/// Why a record survives a deletion request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainedBecause {
    /// A suppression row. Deleting it would resurrect a suppressed contact:
    /// the row **is** the evidence the refusal rests on.
    SuppressionEvidence,
    /// The erasure request itself. Deleting it would un-refuse the very contact
    /// the request asked to end.
    TheErasureRequestItself,
}

impl RetainedBecause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SuppressionEvidence => "suppression_evidence",
            Self::TheErasureRequestItself => "the_erasure_request_itself",
        }
    }
}

/// One record a deletion would reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReachableRecord {
    /// `outward_act` or `exact_payload_artifact`.
    pub kind: &'static str,
    pub reference: String,
}

/// One record that must survive, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetainedRecord {
    /// `suppression` or `erasure_request`.
    pub kind: &'static str,
    pub reference: String,
    pub because: RetainedBecause,
}

/// What a deletion for one identity would and would not reach.
///
/// **Proposes; writes nothing and deletes nothing.** Deciding to delete is an
/// owner's act with consequences no sweep should take on its behalf, and the
/// derivation stays testable without anything to write into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErasureProposal {
    /// The normalised identity the request names.
    pub identity: String,
    /// Every spelling the recipient index was probed under. See the module
    /// note: the index is keyed on the unnormalised spelling, so a proposal
    /// cannot claim to have found everything.
    pub spellings_probed: Vec<String>,
    pub request: ErasureRequest,
    /// Ids the recipient axis offered, across every spelling probed.
    pub index_entries_seen: usize,
    /// Ids that resolved to no act — the axis is mixed. Counted so
    /// [`Self::accounted_for`] reconciles against `index_entries_seen`.
    pub not_an_act: usize,
    pub reachable_acts: Vec<ReachableRecord>,
    /// The exact payload revisions those acts went out as, de-duplicated.
    /// Derived from the acts rather than found separately, so it is not a
    /// bucket of the index walk.
    pub reachable_payloads: Vec<ReachableRecord>,
    pub retained: Vec<RetainedRecord>,
    /// What this proposal cannot speak for at all.
    pub out_of_reach: Vec<&'static str>,
}

impl ErasureProposal {
    /// Every id the index offered ends under exactly one bucket.
    pub fn accounted_for(&self) -> usize {
        self.reachable_acts.len() + self.not_an_act
    }
}

/// What a deletion request for one identity reaches, and what it must not.
///
/// **Refuses an identity with no recorded request.** A proposal computed for
/// somebody who never asked is a deletion plan nobody requested, sitting one
/// click away from being executed.
pub fn erasure_proposal(
    erasure: &ErasureRequestLog,
    suppression: &SuppressionRegister,
    outward: &OutwardAssertionStore,
    scope: &ComplianceScope,
    identity: &str,
) -> Result<ErasureProposal> {
    let normalised = normalise_identity(identity)?;
    let requests = erasure.requests_for(scope, &normalised)?;
    let Some(request) = requests.first().cloned() else {
        anyhow::bail!(
            "no erasure request is on file for `{normalised}`: a deletion proposal for somebody \
             who never asked is a plan nobody requested, and this surface must not be the place \
             one comes into existence"
        );
    };

    let spellings = identity_spellings(identity, &normalised);
    let outward_scope = scope.outward();
    let mut ids: Vec<String> = Vec::new();
    let mut seen_ids = BTreeSet::new();
    for spelling in &spellings {
        for id in
            outward.index_entries(&outward_scope, outward_assertions::RECIPIENT_AXIS, spelling)?
        {
            if seen_ids.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    let index_entries_seen = ids.len();

    let mut not_an_act = 0usize;
    let mut reachable_acts = Vec::new();
    let mut payloads = BTreeSet::new();
    for id in ids {
        let Some(act) = outward.load_act(&outward_scope, &id)? else {
            not_an_act += 1;
            continue;
        };
        reachable_acts.push(ReachableRecord {
            kind: "outward_act",
            reference: act.outward_act_ref.clone(),
        });
        payloads.insert(act.exact_payload_artifact_ref.clone());
    }

    // Every suppression row is RETAINED, never reachable. This is the hard
    // constraint the register already enforces and the reason this whole
    // function proposes rather than purges: deleting the row that records why
    // somebody must not be contacted resurrects the contact.
    let mut retained: Vec<RetainedRecord> = suppression
        .history(&scope.suppression(), &normalised)?
        .into_iter()
        .map(|entry| RetainedRecord {
            kind: "suppression",
            reference: entry.suppression_id,
            because: RetainedBecause::SuppressionEvidence,
        })
        .collect();
    for row in &requests {
        retained.push(RetainedRecord {
            kind: "erasure_request",
            reference: row.request_id.clone(),
            because: RetainedBecause::TheErasureRequestItself,
        });
    }

    Ok(ErasureProposal {
        identity: normalised,
        spellings_probed: spellings,
        request,
        index_entries_seen,
        not_an_act,
        reachable_acts,
        reachable_payloads: payloads
            .into_iter()
            .map(|reference| ReachableRecord {
                kind: "exact_payload_artifact",
                reference,
            })
            .collect(),
        retained,
        out_of_reach: OUT_OF_REACH.to_vec(),
    })
}

// ── Rule 1: duplicate recipient across work ─────────────────────────────────

/// The spellings the recipient index is probed under.
///
/// The index is keyed on the **unnormalised** string the disclosure carried, so
/// probing only the normalised form would miss every act written under a
/// different capitalisation. Probing only the supplied form would miss every
/// act written under a different one. So both, plus the trim between them,
/// de-duplicated and in a fixed order so two reads agree.
fn identity_spellings(raw: &str, normalised: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in [normalised, raw.trim(), raw] {
        if candidate.is_empty() {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Who else has contacted this person inside the window.
///
/// Testable on its own, without a decision around it, which is the whole reason
/// it is a function rather than a block inside [`screen`].
#[allow(clippy::too_many_arguments)]
/// The most acts one duplicate scan will OPEN.
///
/// Rule 1 reads the recipient index — one line per act ever filed under an
/// address — and then opens each one, because the 14-day window can only be
/// applied after `prepared_at` is read off the act. Nothing prunes that index,
/// so the cost grows for the life of the workspace: an address written to two
/// thousand times costs two thousand file reads and JSONL folds, synchronously,
/// inside the dispatch path, before the message leaves — nearly all of them
/// discarded immediately as outside the window.
///
/// The index cannot be read newest-first to stop early: entries are in append
/// order, and `reindex_work_axes` re-appends in directory order, so position
/// says nothing reliable about time.
///
/// So the scan is bounded, and a bounded scan that found nothing MUST NOT
/// answer `Clear` — it did not look everywhere. It reports what it skipped and
/// the rule refuses, which is the fail-closed direction: "we could not check"
/// is not "there is nothing there".
pub const MAX_DUPLICATE_ACTS_EXAMINED: usize = 500;

pub fn duplicate_contacts(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    spellings: &[String],
    this_act_ref: Option<&str>,
    this_work: Option<&WorkContextKind>,
    window: Duration,
    now: DateTime<Utc>,
) -> Result<DuplicateScan> {
    let mut scan = DuplicateScan::default();
    // One index read per spelling — never `act_refs_under_axis`, which refuses
    // `RECIPIENT_AXIS` outright because that axis holds assertion-use ids as
    // well as act refs and would report them as acts the store does not hold.
    let mut ids: Vec<(String, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for spelling in spellings {
        scan.spellings_probed += 1;
        for id in store.index_entries(scope, outward_assertions::RECIPIENT_AXIS, spelling)? {
            if seen.insert(id.clone()) {
                ids.push((id, spelling.clone()));
            }
        }
    }
    scan.entries_seen = ids.len();
    if ids.len() > MAX_DUPLICATE_ACTS_EXAMINED {
        scan.not_examined = ids.len() - MAX_DUPLICATE_ACTS_EXAMINED;
        ids.truncate(MAX_DUPLICATE_ACTS_EXAMINED);
    }

    for (id, spelling) in ids {
        // `record_outward_disclosure` writes the act BEFORE every gate runs, so
        // the act being screened is already filed under the recipient axis by
        // the time this rule looks, and it must never be read as somebody
        // else's contact.
        //
        // On the dispatch path today the skip is what keeps the COUNT honest
        // rather than what averts a refusal: a freshly written act is still
        // `prepared`, so without the skip it lands in `inactive_disclosure`,
        // and a resumed one carries the same work the gate is handed, so it
        // lands in `same_work`. The one shape that would genuinely refuse is a
        // resumed act whose recorded work has since diverged from the binding —
        // which is exactly the fixture the guard's test uses, and the reason
        // this is not merely bookkeeping.
        if this_act_ref == Some(id.as_str()) {
            scan.own_act += 1;
            continue;
        }
        let Some(act) = store.load_act(scope, &id)? else {
            // Not a fault: the axis is mixed, and an id that resolves to no act
            // is an assertion-use id. Counted rather than dropped so the
            // outcomes still sum to `entries_seen`.
            scan.not_an_act += 1;
            continue;
        };
        // Both fields, not one: a disclosure may carry a programme and an
        // engagement, and reading only one would let an act whose engagement
        // matches ours refuse as a collision on its programme.
        //
        // Read BEFORE the clock so the unreadable-timestamp branch below can
        // say whether the act it could not place could have been a collision at
        // all. Which bucket an entry lands in is unchanged by moving this read
        // up — the tests that pin the buckets still hold.
        let mut works: Vec<WorkContextKind> = Vec::new();
        if let Some(program_id) = act.program_id.as_deref() {
            works.push(WorkContextKind::Program(program_id.to_string()));
        }
        if let Some(engagement_id) = act.engagement_id.as_deref() {
            works.push(WorkContextKind::Engagement(engagement_id.to_string()));
        }
        let by_other_work = act.status.is_active_disclosure()
            && !works.is_empty()
            && !this_work.is_some_and(|mine| works.iter().any(|work| work == mine));

        let Ok(prepared_at) = DateTime::parse_from_rfc3339(&act.prepared_at) else {
            // Counted, never dropped. A dropped one is a real contact that
            // silently reads as no contact.
            scan.unreadable_timestamp += 1;
            if by_other_work {
                scan.unplaceable_collisions += 1;
            }
            continue;
        };
        let prepared_at = prepared_at.with_timezone(&Utc);
        if now.signed_duration_since(prepared_at) > window {
            scan.outside_window += 1;
            continue;
        }
        if !act.status.is_active_disclosure() {
            scan.inactive_disclosure += 1;
            continue;
        }
        if works.is_empty() {
            scan.unattributed += 1;
            continue;
        }
        if this_work.is_some_and(|mine| works.iter().any(|work| work == mine)) {
            scan.same_work += 1;
            continue;
        }
        scan.collisions.push(PriorContact {
            identity_as_indexed: spelling,
            outward_act_ref: act.outward_act_ref.clone(),
            works,
            channel: act.channel,
            prepared_at,
            status: act.status,
        });
    }
    Ok(scan)
}

// ── Rule 2: reply before follow-up ──────────────────────────────────────────

/// Build the identity → pending-ask index for a whole scope, **once**.
///
/// `SchedulingStore::all_negotiations` folds every log in the scope, so calling
/// it per recipient would re-parse the whole record once per recipient —
/// invisible at three and quadratic at three hundred, on the read that decides
/// whether a send goes out. The same discipline `introductions::referral_graph`
/// documents about its own register: two reads, not N+1.
///
/// # Why the delivery ledger and the run store are deliberately not read
///
/// `delivery::DeliveryState` is the **provider's** account of *our* message, and
/// `run_state::Expectation` is *us* waiting on *them*. Neither holds a
/// counterparty's reply, so a rule written over either would pass vacuously
/// forever while reading like a guard. `scheduling` is the only register in this
/// codebase that records that they answered.
pub fn reply_index(negotiations: &[ComplianceNegotiation]) -> Result<ReplyIndex> {
    let mut index = ReplyIndex::default();
    for negotiation in negotiations {
        index.scan.negotiations_seen += 1;
        match negotiation.state {
            // Nothing more may be absorbed — the store's own rule, read rather
            // than re-derived.
            ComplianceNegotiationState::Held | ComplianceNegotiationState::Closed => {
                index.scan.settled += 1;
                continue;
            },
            // Nobody has answered, so nobody is waiting on us. Chasing silence
            // is Module D's job and is not a refusal.
            ComplianceNegotiationState::AwaitingReply => {
                index.scan.awaiting_reply += 1;
                continue;
            },
            // A decline IS an answer. `reply_routing` already records that a
            // declined ask is not settled and the counterparty may still
            // counter — but nobody is left waiting on US, which is the only
            // thing this rule is about.
            ComplianceNegotiationState::Declined => {
                index.scan.declined += 1;
                continue;
            },
            ComplianceNegotiationState::Accepted | ComplianceNegotiationState::Countered => {},
        }
        // `scheduling` deliberately resolves no identities and only refuses a
        // blank counterparty, so both sides are normalised here or the index
        // keys never meet the recipients. Read BEFORE the reply, so the
        // no-reply branch below can still say who its row might be about
        // instead of reporting a row with no reach at all — which `screen`
        // would then have to treat as "could be anyone".
        let keyed = normalise_identity(&negotiation.counterparty);
        let Some(reply) = negotiation.latest_reply.as_ref() else {
            // Unreachable for a valid projection: the media owner derives Accepted and
            // Countered from the last reply. Recorded as unmatchable rather
            // than silently skipped, because if it ever became reachable a skip
            // would be a live ask nothing can match.
            index.record_unmatchable(unmatchable_ask(
                &negotiation,
                "the ask's state says the latest word is theirs, but the log holds no reply to \
                 read"
                    .to_string(),
                match keyed.as_ref() {
                    Ok(identity) => vec![identity.clone()],
                    Err(_) => recoverable_identities(&negotiation.counterparty),
                },
            ));
            continue;
        };
        let identity = match keyed {
            Ok(identity) => identity,
            Err(error) => {
                // A live ask whose counterparty cannot be keyed is one this rule
                // cannot match by key — "we could not check", which `screen`
                // turns into a refusal. It is NOT a reason to refuse every send
                // in the workspace: `screen` narrows it to the recipients this
                // row could still be about, and names the row so the fault can
                // be repaired. Only pending asks are recorded: a settled one
                // with the same fault could refuse nothing anyway.
                index.record_unmatchable(unmatchable_ask(
                    &negotiation,
                    format!("{error:#}"),
                    recoverable_identities(&negotiation.counterparty),
                ));
                continue;
            },
        };
        index.scan.awaiting_our_move += 1;
        index
            .by_identity
            .entry(identity.clone())
            .or_default()
            .push(PendingReply {
                identity,
                negotiation_id: negotiation.negotiation_id.clone(),
                audience: negotiation.audience.clone(),
                purpose: negotiation.purpose.clone(),
                replied_at: reply.at,
                reply_kind: reply.kind.clone(),
            });
    }
    Ok(index)
}

/// How many characters of a recorded counterparty a refusal repeats.
///
/// Bounded because the value is caller-supplied and arrives here precisely
/// because it is malformed: an unbounded one would put a megabyte of somebody
/// else's data into a refusal that reaches a log line and a model's prompt.
const MAX_RENDERED_COUNTERPARTY: usize = 200;

/// How many unreadable rows one refusal names before it stops listing.
///
/// Bounded for the same reason, and the number of rows is reported whatever
/// happens — see [`unmatchable_reply_cause`], which says how many it listed out
/// of how many there were. A truncated list whose own count is honest is
/// actionable; a truncated list that pretends to be complete is not.
const MAX_NAMED_UNMATCHABLE: usize = 5;

/// How many recovered candidate identities one named row lists.
///
/// The candidates are derived from the malformed counterparty itself, so an
/// unbounded rendering of them would put back into the refusal exactly what
/// [`MAX_RENDERED_COUNTERPARTY`] exists to keep out of it. Bounded at the
/// **rendering** only: [`UnmatchableAsk::could_be_about`] still matches over
/// the whole list, because narrowing on a truncated one would let a row stop
/// refusing a recipient it really could be about.
const MAX_NAMED_CANDIDATES: usize = 5;

/// Build the record of one live ask this rule could not key.
fn unmatchable_ask(
    negotiation: &ComplianceNegotiation,
    why: String,
    candidate_identities: Vec<String>,
) -> UnmatchableAsk {
    UnmatchableAsk {
        negotiation_id: negotiation.negotiation_id.clone(),
        audience: negotiation.audience.clone(),
        counterparty_as_recorded: escaped_for_display(&negotiation.counterparty),
        purpose: negotiation.purpose.clone(),
        candidate_identities,
        why,
    }
}

/// What an un-keyable counterparty could still be about.
///
/// `normalise_identity` refuses a counterparty for exactly two reasons: it is
/// blank once unwrapped, or it holds a control character (U+001F included).
/// Blank recovers nothing at all. A control character has two plausible
/// readings, and this function takes **both**, because guessing one would be
/// this gate deciding which corruption happened:
///
/// 1. **A separator.** Two values were joined and one of them is the address —
///    the exact shape a crafted `alice@example.test\u{1f}x` has. So every
///    fragment between control characters is a candidate.
/// 2. **Noise inside one value.** A stray byte survived a bad decode, splitting
///    one address in half. So the whole string with its control characters
///    removed is a candidate too.
///
/// Any third reading — a truncation, a transposition, a different address
/// altogether — is out of reach, and this function does not pretend otherwise.
/// [`UnmatchableAsk::could_be_about`] is where that shortfall is answered for:
/// a row that recovered **nothing** is treated as possibly about anyone, so the
/// only rows narrowed away are ones an address was actually recovered from.
///
/// Returns identities already normalised, so they compare directly against the
/// screened set, de-duplicated and in a fixed order so two reads agree.
fn recoverable_identities(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let stripped: String = raw.chars().filter(|ch| !ch.is_control()).collect();
    for candidate in raw
        .split(char::is_control)
        .map(str::to_string)
        .chain(std::iter::once(stripped))
    {
        let Ok(identity) = normalise_identity(&candidate) else {
            continue;
        };
        if seen.insert(identity.clone()) {
            out.push(identity);
        }
    }
    out
}

/// A recorded value, safe to put in a refusal and bounded in length.
///
/// Control characters become their `\u{..}` escape rather than travelling into
/// a log line, an owner's terminal or a model's prompt — the fault being
/// reported is exactly that this string holds them. Truncation is **marked**,
/// never silent: an owner searching their scheduling log for a value this gate
/// printed has to be able to tell a whole one from a prefix.
///
/// The bound counts source characters, not output ones, so a value made
/// entirely of control characters truncates at the same place as any other.
fn escaped_for_display(raw: &str) -> String {
    let mut out = String::new();
    let mut taken = 0usize;
    for ch in raw.chars() {
        if taken == MAX_RENDERED_COUNTERPARTY {
            out.push_str("…(truncated)");
            break;
        }
        if ch.is_control() {
            out.push_str(&format!("\\u{{{:04x}}}", ch as u32));
        } else {
            out.push(ch);
        }
        taken += 1;
    }
    out
}

/// The sentence rule 2 refuses with when a row it could not read might be about
/// somebody this act is contacting.
///
/// **Both numbers, always.** `blocking` is what refuses this send; `total` is
/// every unreadable row in the scope. A refusal that reported only the narrowed
/// number would hide the rest of the damage from the one person positioned to
/// repair it — which is the opposite mistake to the workspace-wide refusal this
/// narrowing replaces, and just as bad.
///
/// **What it claims to be ignorant of is the NAMED ROWS, never the recipients.**
/// [`screen`] reaches this arm whenever any row blocks, including calls that
/// also hold fully-read pending asks — an `Unreadable` verdict and a `Refused`
/// one both stop the send, and only one verdict per rule is emitted. So a
/// sentence saying the rule "cannot say whether any of this action's recipients
/// has answered and is waiting on us" would be false in exactly that case: the
/// gate knows one of them has. The ignorance is therefore stated per row —
/// whose ask it is, and whether that person is among the recipients — which is
/// true whatever else the same call found.
fn unmatchable_reply_cause(blocking: &[&UnmatchableAsk], total: usize) -> String {
    // Sorted BEFORE truncating, so the same fixture names the same rows on
    // every run. Truncating first would list an arbitrary five and then order
    // those, which reads deterministic and is not.
    let mut named: Vec<String> = blocking.iter().map(|ask| ask.describe()).collect();
    named.sort();
    let listed = named.len().min(MAX_NAMED_UNMATCHABLE);
    let overflow = if named.len() > listed {
        format!(
            " Listing {listed} of {}; the rest are on `reply_scan.unmatchable`.",
            named.len()
        )
    } else {
        String::new()
    };
    named.truncate(listed);
    format!(
        "{} of {total} live ask(s) in this scope name a counterparty this rule could not turn \
         into a key AND could still be about this action's recipients, so for each of them this \
         rule cannot say WHO the ask is with, nor whether that person is one of this action's \
         recipients already waiting on us: {}.{overflow} Any remaining \
         unreadable row in this scope either recovered an address naming somebody else, or lives \
         in the relationship this act's own work owns — in which case this send would be our \
         answer to it — so it does not refuse this send. The remedy is to CLOSE the named ask \
         with a reason: a closed ask is settled and stops poisoning this rule. The counterparty \
         itself cannot be repaired in place — the scheduling log is append-only and offers no \
         rewrite, and the negotiation id is derived from the counterparty, so re-opening under \
         the corrected spelling files a NEW ask and leaves this row exactly as live and as \
         unreadable as it is now. Retrying this send unchanged will refuse again.",
        blocking.len(),
        named.join("; ")
    )
}

/// Whether the work this act is bound to owns the relationship the ask lives in.
///
/// **This test is what keeps rule 2 from deadlocking the conversation.** Our own
/// answer to a counterparty who countered is itself an outward act to that
/// counterparty; refusing it would mean the relationship could never move, which
/// is the "a gate that refuses one hundred percent gets routed around" failure
/// `outward_gate` already names.
///
/// `None` owns nothing — see the module note on why that abstains rather than
/// refuses.
fn work_owns(this_work: Option<&WorkContextKind>, audience: &AudienceRef) -> bool {
    match this_work {
        Some(WorkContextKind::Program(id)) => audience == &AudienceRef::program(id.clone()),
        Some(WorkContextKind::Engagement(id)) => audience == &AudienceRef::engagement(id.clone()),
        None => false,
    }
}

// ── The gate ────────────────────────────────────────────────────────────────

/// Ask all four questions about one outward act's recipients.
///
/// Writes nothing. Every `Err` from a store reaches a
/// [`RuleVerdict::Unreadable`] rather than folding into a clear verdict: an
/// unreadable register is never an empty one.
///
/// # Refuses an empty recipient list
///
/// *"No recipient is refused"* over zero recipients is vacuously true and reads
/// exactly like a clean bill of health — the same refusal
/// `SuppressionRegister::screen` makes, and for the same reason. The gate's
/// **wrapper**, [`compliance_refusal`], is where the empty case is handled,
/// because only the dispatch path can tell an act that reaches nobody from one
/// whose recipients we failed to parse — and `outward_gate::contact_refusal`
/// has already made that call by the time this runs.
#[allow(clippy::too_many_arguments)]
pub fn screen(
    workspace_layout: &ArtifactV2Workspace,
    scope: &ComplianceScope,
    recipients: &[String],
    channel: Option<OutwardChannel>,
    this_work: Option<&WorkContextKind>,
    this_act_ref: Option<&str>,
    policy: &CompliancePolicy,
    jurisdiction: &dyn JurisdictionRule,
    now: DateTime<Utc>,
) -> Result<RecipientComplianceDecision> {
    scope.validate()?;
    if recipients.is_empty() {
        anyhow::bail!(
            "refusing to screen an empty recipient list: all four recipient-compliance rules are \
             per-identity, so `no recipient is refused` over zero recipients is vacuously true \
             and reads exactly like a clean bill of health"
        );
    }

    // Normalise and de-duplicate first, occurrence order preserved, so a list
    // holding one address twice yields one decision — and so the spellings the
    // index is probed under are the ones the caller actually supplied.
    let mut identities: Vec<(String, Vec<String>)> = Vec::new();
    let mut position: BTreeMap<String, usize> = BTreeMap::new();
    for raw in recipients {
        // A malformed identity fails the whole screen rather than being
        // skipped: dropping a recipient from the CHECK while the dispatcher
        // still holds it in the SEND is fail-open wearing a different hat.
        let normalised = normalise_identity(raw)?;
        // Copied out before the match: the `None` arm mutates the same map, and
        // holding the lookup's borrow across the arms would not compile.
        let existing = position.get(&normalised).copied();
        match existing {
            Some(at) => {
                for spelling in identity_spellings(raw, &normalised) {
                    if !identities[at].1.contains(&spelling) {
                        identities[at].1.push(spelling);
                    }
                }
            },
            None => {
                position.insert(normalised.clone(), identities.len());
                let spellings = identity_spellings(raw, &normalised);
                identities.push((normalised, spellings));
            },
        }
    }

    let outward = OutwardAssertionStore::new(workspace_layout.clone());
    let outward_scope = scope.outward();
    let erasure = ErasureRequestLog::new(workspace_layout.clone());

    // ── Rule 1 ──
    let mut duplicate_scan = DuplicateScan::default();
    let mut duplicate_unreadable: Option<String> = None;
    for (_, spellings) in &identities {
        match duplicate_contacts(
            &outward,
            &outward_scope,
            spellings,
            this_act_ref,
            this_work,
            policy.duplicate_window(),
            now,
        ) {
            Ok(scan) => duplicate_scan.absorb(scan),
            Err(error) => {
                duplicate_unreadable = Some(format!("{error:#}"));
                break;
            },
        }
    }
    // A capped scan that found nothing has NOT established there is nothing.
    // Reported as unreadable rather than clear, because "we stopped looking" and
    // "there is none" are opposite facts and only one of them is safe to send
    // on. If a collision WAS found the cap is irrelevant — the rule already has
    // its answer, and refusing twice for the same reason helps nobody.
    if duplicate_unreadable.is_none()
        && duplicate_scan.not_examined > 0
        && duplicate_scan.collisions.is_empty()
        && duplicate_scan.unplaceable_collisions == 0
    {
        duplicate_unreadable = Some(format!(
            "the recipient history is longer than one send-time scan opens: {} of {} filed acts \
             were examined and {} were not, so whether another work has already contacted these \
             people recently is unknown. Prune or archive the recipient index, or raise the \
             scan bound",
            duplicate_scan.entries_seen - duplicate_scan.not_examined,
            duplicate_scan.entries_seen,
            duplicate_scan.not_examined,
        ));
    }
    let duplicate_finding = RuleFinding {
        rule: ComplianceRule::DuplicateRecipient,
        examined: duplicate_scan.entries_seen,
        verdict: match duplicate_unreadable {
            Some(cause) => RuleVerdict::Unreadable { cause },
            // Evidence exists but this act names no work, so "another work" is
            // undefined for it. Reported, not refused — see the module note.
            None if this_work.is_none()
                && (!duplicate_scan.collisions.is_empty()
                    || duplicate_scan.unplaceable_collisions > 0) =>
            {
                RuleVerdict::NotAssessed {
                    why: "act_names_no_work",
                }
            },
            None if !duplicate_scan.collisions.is_empty() => {
                RuleVerdict::Refused(Box::new(Refusal::ContactedByOtherWork {
                    contacts: duplicate_scan.collisions.clone(),
                }))
            },
            // A real earlier contact by another work that this rule could not
            // place in time. It is neither a collision it found nor one it
            // ruled out, and `Clear` asserts the second. The count alone is not
            // enough: nothing on the dispatch path reads the scan, so a bucket
            // is where an unreadable record goes to be forgotten unless the
            // verdict carries it too.
            None if duplicate_scan.unplaceable_collisions > 0 => RuleVerdict::Unreadable {
                cause: format!(
                    "{} active disclosure(s) to these recipients by another work carry a \
                     `prepared_at` that will not parse, so this rule cannot say whether they \
                     fall inside the duplicate-contact window; repair the act record rather \
                     than retrying",
                    duplicate_scan.unplaceable_collisions
                ),
            },
            None => RuleVerdict::Clear,
        },
    };

    // ── Rule 2 ── the index is built ONCE, outside the per-recipient loop.
    let reply = configured_negotiations(workspace_layout, scope)
        .and_then(|negotiations| reply_index(&negotiations));
    let (reply_scan, reply_finding) = match reply {
        // The `act names no work` abstention belongs HERE too, or the fix
        // above is half applied. Rule 2 asks *"is this send a follow-up, or is
        // it our own answer?"*, and only the work an act names can tell those
        // apart — so an act naming none cannot be asked that question about any
        // row, and whether the store could be READ changes nothing about that.
        // Left as an unconditional `Unreadable`, an unreadable store would
        // refuse exactly the acts a readable one abstains on: the same
        // asymmetry the ordering below removes, arriving through the error
        // path instead of the happy one.
        //
        // The fault is LOGGED rather than carried. `why` is a `&'static str`,
        // so the error text cannot travel in it — and an unreadable scheduling
        // store is a real fault that must not vanish just because this act
        // could not have been refused by it anyway. The `why` says an
        // unreadable store was the shape of this abstention, so a reader can
        // tell it from one taken over a store that read perfectly.
        Err(error) if this_work.is_none() => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "[RECIPIENT-COMPLIANCE] the scheduling store could not be read; this act names \
                 no work so rule 2 could not have been applied to it either way, and the send \
                 is not refused on that account — but the store is still unreadable"
            );
            (
                ReplyScan::default(),
                RuleFinding {
                    rule: ComplianceRule::ReplyPending,
                    examined: 0,
                    verdict: RuleVerdict::NotAssessed {
                        why: "act_names_no_work_and_the_scheduling_store_was_unreadable",
                    },
                },
            )
        },
        Err(error) => (
            ReplyScan::default(),
            RuleFinding {
                rule: ComplianceRule::ReplyPending,
                examined: 0,
                verdict: RuleVerdict::Unreadable {
                    cause: format!("{error:#}"),
                },
            },
        ),
        Ok(index) => {
            let examined = index.scan.negotiations_seen;
            // A row this rule could not key stops only what it could actually
            // be about. Two narrowings, and no others:
            //
            //  - an ask living in the relationship THIS act's work owns is one
            //    this send would be ANSWERING, exactly as for a readable
            //    pending ask below — `work_owns` is the same test, applied to
            //    the one field of an unreadable row that is still readable;
            //  - an ask whose counterparty recovered to identities that are
            //    none of this action's recipients is about somebody else.
            //
            // Everything else still refuses an act that names a work,
            // including a row nothing could be recovered from: that one might
            // be the recipient in front of us, and "we could not check" is
            // never permission. Before this narrowing a single
            // storable-but-malformed counterparty — the scheduling store
            // validates only that it is non-blank — refused every outward send
            // in the whole (principal, workspace).
            let screened: BTreeSet<String> = identities
                .iter()
                .map(|(identity, _)| identity.clone())
                .collect();
            let blocking: Vec<&UnmatchableAsk> = index
                .scan
                .unmatchable
                .iter()
                .filter(|ask| !work_owns(this_work, &ask.audience))
                .filter(|ask| ask.could_be_about(&screened))
                .collect();
            let waiting: Vec<PendingReply> = identities
                .iter()
                .flat_map(|(identity, _)| index.pending_for(identity))
                .filter(|ask| !work_owns(this_work, &ask.audience))
                .cloned()
                .collect();
            // The arm order is the rule, not a formatting accident, and the
            // `act names no work` arm sits ABOVE the unreadable one on
            // purpose. It used to sit below, and the asymmetry that produced
            // read as deliberate to everyone who came after: for one and the
            // same unattributed act, a pending ask this rule could FULLY READ
            // yielded `NotAssessed` while a row it could not read at all
            // yielded `Unreadable`, so the weaker evidence was answered more
            // harshly than the stronger. Rule 2's question is *"is this send a
            // follow-up, or is it our own answer?"*, and the only thing that
            // can tell those two apart is the work the act names. An act that
            // names none cannot be asked that question about ANY row —
            // readable or not — so both abstain, which is exactly what
            // `NotAssessed` means: the rule could not be applied at all.
            //
            // Abstaining is not falling silent. Every unreadable row is on
            // `reply_scan.unmatchable` on the returned decision whatever the
            // verdict, its count travels beside it, and `why` says an
            // unreadable row was among what this rule declined to decide on —
            // so `refusal_message`'s "also worth knowing" tail names it
            // whenever another rule refuses, and
            // `GET /recipient-compliance/check` renders the rows themselves.
            //
            // This ordering is also what bounds a row nothing could be
            // recovered from — the one shape `recoverable_identities` can
            // narrow nothing away from, reachable from a counterparty as small
            // as `<>`, and unrepairable because the store offers no way to
            // rewrite a counterparty. Every act whose execution carries no work
            // authority reaches this gate unbound (`work_kind_of` maps
            // `WorkBinding::Unbound` to `None`), and one such row used to
            // refuse every one of them across the whole (principal, workspace).
            // An act that DOES name a work is still refused by it, and must be:
            // a real counterparty really is waiting on us and nothing in reach
            // can say who.
            let verdict = if blocking.is_empty() && waiting.is_empty() {
                RuleVerdict::Clear
            } else if this_work.is_none() {
                RuleVerdict::NotAssessed {
                    // Two abstentions, kept apart. `RuleVerdict`'s own note
                    // holds that "they are off limits" and "we could not
                    // check" must never render as one answer, and `why` is the
                    // field that says which of the two this is short of.
                    why: if blocking.is_empty() {
                        "act_names_no_work"
                    } else {
                        "act_names_no_work_and_a_row_was_unreadable"
                    },
                }
            } else if !blocking.is_empty() {
                RuleVerdict::Unreadable {
                    // The count of ALL unreadable rows travels with the
                    // narrowed one, so a reader can tell a scope with one bad
                    // row from a scope with forty.
                    cause: unmatchable_reply_cause(&blocking, index.scan.unmatchable_counterparty),
                }
            } else {
                RuleVerdict::Refused(Box::new(Refusal::AwaitingOurReply { pending: waiting }))
            };
            (
                index.scan.clone(),
                RuleFinding {
                    rule: ComplianceRule::ReplyPending,
                    examined,
                    verdict,
                },
            )
        },
    };

    // ── Rule 3 ──
    let mut refused_rulings: Vec<JurisdictionRefusal> = Vec::new();
    let mut permitted = 0usize;
    let mut jurisdiction_unreadable: Option<String> = None;
    // Counted as the loop runs, never assumed to be `identities.len()`: the
    // loop BREAKS on the first rule that could not answer, so a finding that
    // reported the whole list would claim it put recipients to a rule it never
    // reached. An `examined` that cannot be checked against the work actually
    // done is worse than no `examined` at all.
    let mut jurisdiction_examined = 0usize;
    for (identity, _) in &identities {
        jurisdiction_examined += 1;
        let query = JurisdictionQuery {
            identity: identity.as_str(),
            channel,
            work: this_work,
            now,
        };
        match jurisdiction.ruling(&query) {
            Ok(JurisdictionRuling::Permitted) => permitted += 1,
            Ok(JurisdictionRuling::NoRuleApplies) => {},
            Ok(JurisdictionRuling::Refused { detail }) => {
                refused_rulings.push(JurisdictionRefusal {
                    identity: identity.clone(),
                    rule_name: jurisdiction.name().to_string(),
                    detail,
                });
            },
            Err(error) => {
                // A rule that could not answer has not answered yes.
                jurisdiction_unreadable = Some(format!("{error:#}"));
                break;
            },
        }
    }
    let jurisdiction_finding = RuleFinding {
        rule: ComplianceRule::Jurisdiction,
        examined: jurisdiction_examined,
        verdict: match jurisdiction_unreadable {
            Some(cause) => RuleVerdict::Unreadable { cause },
            None if !refused_rulings.is_empty() => {
                RuleVerdict::Refused(Box::new(Refusal::JurisdictionRefused {
                    rulings: refused_rulings,
                }))
            },
            // `Clear` only when a rule actually ruled on EVERY recipient.
            // Anything less would let "nothing looked at three of these five"
            // render as "a jurisdiction cleared this act".
            None if permitted == identities.len() => RuleVerdict::Clear,
            None => RuleVerdict::NotAssessed {
                why: "no_rule_applied_to_every_recipient",
            },
        },
    };

    // ── Rule 4 ──
    let mut erasure_hits: Vec<ErasureRequest> = Vec::new();
    let mut erasure_unreadable: Option<String> = None;
    // Counted as the loop runs — same reason as rule 3's: the loop breaks on
    // the first identity whose log could not be read, and reporting the whole
    // list would claim lookups that never happened.
    let mut erasure_examined = 0usize;
    for (identity, _) in &identities {
        erasure_examined += 1;
        match erasure.requests_for(scope, identity) {
            Ok(requests) => erasure_hits.extend(requests),
            Err(error) => {
                erasure_unreadable = Some(format!("{error:#}"));
                break;
            },
        }
    }
    let erasure_finding = RuleFinding {
        rule: ComplianceRule::ErasureRequested,
        examined: erasure_examined,
        verdict: match erasure_unreadable {
            Some(cause) => RuleVerdict::Unreadable { cause },
            None if erasure_hits.is_empty() => RuleVerdict::Clear,
            // No work test here, and deliberately: being forgotten is not
            // relative to a programme. Any recorded request refuses every work.
            None => RuleVerdict::Refused(Box::new(Refusal::ErasureRequested {
                requests: erasure_hits,
            })),
        },
    };

    // Exactly four, in `ComplianceRule::ALL` order, every time.
    let findings = vec![
        duplicate_finding,
        reply_finding,
        jurisdiction_finding,
        erasure_finding,
    ];
    debug_assert_eq!(
        findings
            .iter()
            .map(|finding| finding.rule)
            .collect::<Vec<_>>(),
        ComplianceRule::ALL.to_vec(),
        "the findings must be emitted in ComplianceRule::ALL order"
    );

    Ok(RecipientComplianceDecision {
        identities: identities
            .into_iter()
            .map(|(identity, _)| identity)
            .collect(),
        findings,
        duplicate_scan,
        reply_scan,
    })
}

/// The send-time wrapper: `Some(message)` refuses, `None` continues.
///
/// The same shape `outward_gate::contact_refusal` has, deliberately — this gate
/// sits immediately after it on the dispatch path and the caller should not have
/// to handle two different answer types for two consecutive questions.
///
/// # Every "I cannot check" branch refuses
///
/// - **No scoped store.** With no principal, workspace or artifact workspace
///   there is nothing to consult. An unconsultable register is not an empty one.
/// - **A screen that failed.** Refused, naming the cause.
/// - **An unreadable rule.** [`RuleVerdict::Unreadable`] refuses, inside the
///   decision.
///
/// # The one branch that returns `None` without checking anything
///
/// An **empty** recipient list. This gate is ordered strictly after
/// `outward_gate::contact_refusal`, which owns that decision: only the
/// dispatcher can tell "we failed to parse who this reaches"
/// (`Addressing::RecipientRequired`, refused there) from "this act reaches
/// nobody" (`Addressing::MayReachNobody`, a calendar entry on one's own day, a
/// form addressed to a site). All four rules here are per-identity, so by the
/// time an empty list reaches this function it has already been judged and the
/// four questions have nothing to say about nobody. Re-deciding it here would
/// mean either duplicating `OutwardClass` — two things to keep in step — or
/// importing an `agents`-specific type into a generic coordinator, which is the
/// dependency inversion this codebase forbids.
#[allow(clippy::too_many_arguments)]
pub fn compliance_refusal(
    workspace_layout: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace: Option<&str>,
    recipients: &[String],
    channel: Option<OutwardChannel>,
    this_work: Option<&WorkContextKind>,
    this_act_ref: Option<&str>,
    policy: &CompliancePolicy,
    jurisdiction: &dyn JurisdictionRule,
    now: DateTime<Utc>,
) -> Option<String> {
    if recipients.is_empty() {
        return None;
    }
    let (Some(workspace_layout), Some(principal), Some(workspace)) =
        (workspace_layout, principal, workspace)
    else {
        return Some(format!(
            "{REFUSAL_PREFIX} This execution carries no scoped store, so none of the four \
             recipient-compliance rules could be consulted — not the record of who else has \
             contacted these people, not who is waiting on our reply, and not who has asked to \
             be forgotten. An unconsultable register is not an empty one, and an unchecked \
             recipient is never a permitted one."
        ));
    };
    let scope = ComplianceScope::new(principal, workspace);
    match screen(
        workspace_layout,
        &scope,
        recipients,
        channel,
        this_work,
        this_act_ref,
        policy,
        jurisdiction,
        now,
    ) {
        Ok(decision) => decision.refusal_message(),
        Err(error) => Some(format!(
            "{REFUSAL_PREFIX} The recipient-compliance gate could not be run at all, and a gate \
             that could not run has refused nobody and cleared nobody. Nothing was attempted for \
             any recipient. Cause: {error:#}"
        )),
    }
}

// ── Internals ───────────────────────────────────────────────────────────────

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// A required free-text field, checked and trimmed.
///
/// Blank is refused for the reason `suppression::validated_evidence` gives: a
/// record nobody can check is an assertion rather than a record, and the first
/// person to doubt it will remove it — which here would un-refuse a contact
/// somebody asked to end.
fn validated_field(value: &str, what: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!(
            "an erasure request must carry its {what}: a request nobody can check is an \
             assertion, not a record, and this one refuses contact permanently"
        );
    }
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "an erasure request's {what} must not contain U+001F: it is the separator derived \
             ids are joined with, and a crafted value could collide with another request's id"
        );
    }
    Ok(value.to_string())
}

/// Derived, never assigned, from `(owner, identity, evidence)`.
///
/// The **evidence** is in the tuple for the same reason it is in a suppression
/// id: a second, later request from the same person is a second act of consent
/// and must be its own row, not a resumption of the first.
fn derive_request_id(scope: &ComplianceScope, identity: &str, evidence_ref: &str) -> String {
    stable_id(&format!(
        "{}{FIELD_SEP}{}{FIELD_SEP}{identity}{FIELD_SEP}{evidence_ref}",
        scope.principal, scope.workspace
    ))
}
