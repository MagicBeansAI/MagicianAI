//! Who we must not contact, and why — the suppression register.
//!
//! A register of identities that are off limits, each with the reason and the
//! evidence that established it.
//!
//! # Who actually consults it
//!
//! One caller on a live path:
//! [`contact_refusal`](crate::magician_v2::agents::outward_gate::contact_refusal),
//! run from `execute_action_inner` before any outward act leaves and under
//! capture as well as live. It always calls
//! [`SuppressionRegister::screen`] — never
//! [`SuppressionRegister::is_suppressed`] directly, though `screen` calls that
//! once per recipient — and it sees the acts the executor's dispatch classifier
//! recognises as outward.
//!
//! And one owner-facing read: `magician-api`'s `suppression_api`, whose
//! `GET /suppressions/check` answers *"may we contact this person"* through the
//! same [`SuppressionRegister::screen`] call rather than a second opinion of
//! its own. A surface that derived the answer another way could say `clear`
//! about somebody the gate refuses, and nothing would tell an owner which of
//! the two was lying.
//!
//! # Who writes to it
//!
//! Two producers, both named, because a register with a reader and no writer is
//! a fail-closed guard rendered vacuous by an empty store — which is exactly
//! what this module was for its whole first life:
//!
//! - `magician-api`'s `suppression_api`: `POST /suppressions` records one and
//!   `POST /suppressions/lift` reverses one, on an explicit owner act.
//! - [`crate::magician_v2::delivery_hygiene::worker::SuppressionSweepWorker`],
//!   which turns the delivery ledger's hard bounces and complaints into
//!   [`SuppressionRegister::ingest`] calls on a configured cadence. It never
//!   lifts.
//!
//! Two outward classes can reach a send without consulting the register at all,
//! and deliberately so: a calendar entry and a form submission *may* legitimately
//! reach nobody, so one of those carrying an empty recipient list has nobody to
//! screen and `contact_refusal` returns before the register is opened. Every act
//! that names a recipient is screened, whatever its class.
//!
//! # The calling contract, in one line
//!
//! **`Err` means DO NOT SEND.** Not "retry without the check", not "log and
//! continue". Three answers, two of which stop a send:
//!
//! | answer | meaning |
//! |---|---|
//! | `Ok(None)` | nothing on the register — clear to send |
//! | `Ok(Some(_))` | suppressed — do not send, and the row says why |
//! | `Err(_)` | the register could not be read — **do not send** |
//!
//! The third row is the whole reason this module exists rather than a `HashSet`
//! somewhere. A register that answers "nobody is suppressed" because its log
//! was unreadable is worse than no register at all: it manufactures confidence
//! for exactly the send that should have been held. Reads therefore go through
//! [`crate::magician_v2::jsonl`], where only a genuinely absent file reads as an
//! empty register and every other fault propagates.
//!
//! # Generic
//!
//! An identity, a reason, evidence, and a clock. Nothing here knows what is
//! being sent, through which channel, or on whose behalf: mail, messaging,
//! calls, postal runs and any future rail bind the same register. Consumers
//! *consume; never own* — a flow that keeps its own copy of "who opted out" is
//! a copy that will drift, and the drift is a compliance incident.
//!
//! # Scope: global by default
//!
//! [`SuppressionRegister::global`] is the safe constructor and the one almost
//! every caller wants. Somebody who opted out of one programme has **not**
//! consented to another, so a suppression is a fact about the owner's
//! relationship with a person, not about one campaign.
//!
//! [`SuppressionRegister::scoped_to_audience`] exists for the narrower case —
//! "stop including this person in *this* cohort's mailings" — and is
//! deliberately longer to type. It is not an escape hatch: an audience-scoped
//! register still consults the global register on every read, so a global
//! opt-out blocks a per-audience send, and a per-audience register **cannot
//! lift a global suppression**. A narrower scope may only ever add.
//!
//! # Forward-only
//!
//! Nothing is ever deleted or rewritten. A suppression is lifted by recording a
//! [`Lift`] beside it, never by removing the row — and an [`OptOut`] or a
//! [`Complaint`] can only be lifted by an explicit
//! [`LiftAuthority::OwnerAct`] carrying evidence. An opt-out that a sweep,
//! a re-import or a "cleanup" job can silently reverse is precisely the
//! compliance failure this module was built to make impossible.
//!
//! [`OptOut`]: SuppressionReason::OptOut
//! [`Complaint`]: SuppressionReason::Complaint

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;

const LOG_TARGET: &str = "magician_v2::suppression";

const FIELD_SEP: char = '\u{1f}';

/// Scope for a register call: whose register this is.
///
/// Suppression is global **to this owner**. Two tenants never share a register,
/// and no derivation here can collide across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressionScope {
    pub principal: String,
    pub workspace: String,
}

impl SuppressionScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// Why an identity is off limits.
///
/// Two of these are **consent decisions** and the other three are operational
/// facts. The split is not cosmetic: it decides what it takes to lift them, and
/// it is the reason [`SuppressionReason::requires_owner_act_to_lift`] exists
/// rather than a free-for-all `lift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    /// They asked. The strongest entry on the register, and the one nothing
    /// automated may ever undo.
    OptOut,
    /// The address is dead — a permanent delivery failure reported by the
    /// transport. Operational: if the transport was wrong, an operator may lift
    /// it.
    HardBounce,
    /// They reported us. Treated exactly as strictly as an opt-out: a complaint
    /// is a consent decision expressed through a third party.
    Complaint,
    /// The owner decided, for their own reasons. Operational — the owner may
    /// change their own mind — but it still needs evidence.
    OwnerBlocked,
    /// A legal or policy hold: a quiet period, a jurisdiction we may not
    /// contact into, an unresolved dispute. Frequently time-boxed, which is why
    /// [`SuppressionRegister::suppress_until`] exists.
    RegulatoryHold,
}

impl SuppressionReason {
    /// Every reason, in a fixed order, so reports are deterministic.
    pub const ALL: [SuppressionReason; 5] = [
        SuppressionReason::OptOut,
        SuppressionReason::HardBounce,
        SuppressionReason::Complaint,
        SuppressionReason::OwnerBlocked,
        SuppressionReason::RegulatoryHold,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptOut => "opt_out",
            Self::HardBounce => "hard_bounce",
            Self::Complaint => "complaint",
            Self::OwnerBlocked => "owner_blocked",
            Self::RegulatoryHold => "regulatory_hold",
        }
    }

    /// Whether clearing this needs the owner personally, with evidence.
    ///
    /// True for [`Self::OptOut`] and [`Self::Complaint`]: both are the person's
    /// own decision, and no operational tidy-up — a bounce reclassification, a
    /// list re-import, a sweep that "noticed the address works again" — has any
    /// standing to reverse it.
    pub fn requires_owner_act_to_lift(self) -> bool {
        matches!(self, Self::OptOut | Self::Complaint)
    }

    /// Whether this reason may be given an end date.
    ///
    /// False for the consent decisions. An opt-out with a timer is an opt-out
    /// that expires quietly while nobody is looking, which is the same failure
    /// as a sweep reversing it — only slower and harder to notice.
    pub fn may_be_time_boxed(self) -> bool {
        !self.requires_owner_act_to_lift()
    }
}

/// What established a suppression, or a lift.
///
/// Both halves are required. *When* is the moment the establishing act happened
/// — the transport's bounce timestamp, the instant they clicked unsubscribe —
/// which is deliberately **not** the moment we recorded it: a register imported
/// late must not claim the opt-out happened late. *The ref* is what an auditor
/// follows to check: a message id, a webhook event id, a ticket, a signed note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppressionEvidence {
    /// When the establishing act happened, per whoever observed it.
    pub established_at: DateTime<Utc>,
    /// The ref an auditor follows. Never blank — a suppression nobody can check
    /// is an assertion, and this register holds records.
    pub evidence_ref: String,
    /// Who or what recorded it: an operator, an agent, an ingest worker.
    pub recorded_by: String,
}

impl SuppressionEvidence {
    pub fn new(
        established_at: DateTime<Utc>,
        evidence_ref: impl Into<String>,
        recorded_by: impl Into<String>,
    ) -> Self {
        Self {
            established_at,
            evidence_ref: evidence_ref.into(),
            recorded_by: recorded_by.into(),
        }
    }
}

/// Who may lift a suppression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiftAuthority {
    /// A routine correction: a transport that mis-reported a bounce, a hold
    /// whose basis went away. Cannot touch a consent decision.
    Operational,
    /// The owner, personally, with evidence. The only thing that clears an
    /// opt-out or a complaint.
    OwnerAct,
}

impl LiftAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::OwnerAct => "owner_act",
        }
    }
}

/// A recorded lift — the only way a suppression stops applying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lift {
    pub authority: LiftAuthority,
    pub evidence: SuppressionEvidence,
}

/// How far a suppression reaches.
///
/// [`Self::Global`] is the default and covers every outward send by this owner.
/// [`Self::Audience`] covers one named relationship only, and never narrows a
/// global entry — the register consults both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "span", rename_all = "snake_case")]
pub enum SuppressionSpan {
    Global,
    Audience { audience: AudienceRef },
}

impl SuppressionSpan {
    /// A stable key for paths and id derivation.
    ///
    /// Built on [`AudienceRef::as_key`], so the audience *kind* is part of it:
    /// `engagement:acme` and `account:acme` are different spans, exactly as
    /// they are different audiences. Without the kind, widening a binding would
    /// silently merge two relationships that share an id.
    pub fn as_key(&self) -> String {
        match self {
            Self::Global => "global".to_string(),
            Self::Audience { audience } => format!("audience:{}", audience.as_key()),
        }
    }

    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

/// One entry on the register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suppression {
    pub suppression_id: String,
    /// The **normalised** identity — see [`normalise_identity`] for the exact
    /// rules. Stored normalised so the register cannot hold two rows that a
    /// human would read as the same address.
    pub identity: String,
    pub reason: SuppressionReason,
    pub evidence: SuppressionEvidence,
    pub span: SuppressionSpan,
    /// When we recorded it — not when the act happened; that is
    /// `evidence.established_at`.
    pub suppressed_at: DateTime<Utc>,
    /// When a time-boxed entry stops applying. `None` means **indefinite**,
    /// which is the safe reading and the default: an absent end date is never
    /// "no suppression".
    pub in_force_until: Option<DateTime<Utc>>,
    pub lifted_at: Option<DateTime<Utc>>,
    pub lift: Option<Lift>,
}

/// What an entry is **right now**, derived from the clock.
///
/// Derived, never stored: nothing writes `expired`. A register whose entries
/// only lapse once a sweep has run is a register that suppresses whoever the
/// sweep last remembered, and the whole point is that the answer is correct on
/// every read without anything having run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionState {
    InForce,
    /// Lifted by a recorded act. Terminal — the row never returns to
    /// [`Self::InForce`]. A fresh suppressing event writes a **new** row.
    Lifted,
    /// A time-boxed entry whose end has arrived.
    Expired,
}

impl Suppression {
    /// The entry's state at `now`.
    ///
    /// A lift outranks an expiry: if both happened, the record says it was
    /// lifted, and that is the fact a review needs to see.
    ///
    /// Expiry is **inclusive**, like every other expiry in this codebase: an
    /// entry in force until noon is expired *at* noon. The window is
    /// half-open — `[recorded, in_force_until)`.
    pub fn state(&self, now: DateTime<Utc>) -> SuppressionState {
        if self.lifted_at.is_some() {
            return SuppressionState::Lifted;
        }
        if self.in_force_until.is_some_and(|until| now >= until) {
            return SuppressionState::Expired;
        }
        SuppressionState::InForce
    }

    /// Whether this entry blocks a send at `now`.
    ///
    /// There is no "not yet in force" state. A recorded suppression applies
    /// immediately, whatever `suppressed_at` says, because a start window would
    /// open a period in which a known opt-out reads as clear — and clock skew
    /// alone would be enough to find it.
    pub fn is_in_force(&self, now: DateTime<Utc>) -> bool {
        self.state(now) == SuppressionState::InForce
    }
}

/// The answer to *"which of these may we contact?"*.
///
/// Both halves are explicit. Nothing here is inferred from a collection being
/// empty: a caller reads `sendable`, never `blocked.is_empty()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screened {
    /// Normalised identities with nothing on the register.
    pub sendable: Vec<String>,
    /// The entries that block the rest — one per blocked identity, naming the
    /// reason so the caller can say why.
    pub blocked: Vec<Suppression>,
}

/// What a batch ingest put on file, and how much of it this call wrote.
///
/// Two numbers rather than one, and the distinction is the point: a sweep that
/// re-reads ground it already covered SHOULD write nothing, and a caller that
/// cannot tell "recorded" from "already held" reports the same bounce as a new
/// suppression on every tick. Counts, not a boolean — "resumed 40 of 40" and
/// "resumed 1 of 1" are different operational facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// Every signal's entry as it now stands, whether written here or resumed.
    pub recorded: Vec<Suppression>,
    /// How many of `recorded` this call actually appended.
    pub newly_recorded: usize,
    /// Signals this pass could not read, by the identity they claimed.
    ///
    /// Reported rather than raised, because the sweep that reads them re-reads
    /// the same window until its cursor advances: one unreadable row raised is
    /// every later bounce never suppressed. Non-empty means a person should
    /// look — but the rest of the batch was still recorded.
    pub unreadable: Vec<String>,
}

impl Ingested {
    /// How many were already on file. Derived rather than stored so the two
    /// numbers cannot disagree.
    pub fn already_held(&self) -> usize {
        self.recorded.len().saturating_sub(self.newly_recorded)
    }
}

/// One incoming signal for [`SuppressionRegister::ingest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressionSignal {
    pub identity: String,
    pub reason: SuppressionReason,
    pub evidence: SuppressionEvidence,
}

/// One line in an identity's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum SuppressionRecord {
    Suppressed(Suppression),
    Lifted {
        suppression_id: String,
        at: DateTime<Utc>,
        authority: LiftAuthority,
        evidence: SuppressionEvidence,
    },
}

/// The register: append-only, one JSONL log per identity.
///
/// One log per identity is what makes the send-time check a single read rather
/// than a scan of everything ever suppressed — and a check that is expensive is
/// a check somebody eventually skips. A per-span index of identities serves the
/// reporting reads, which are the only ones that need to enumerate.
///
/// Every input is **supplied**. The register reads no mailbox, no transport, no
/// consent UI and no config: whatever noticed the bounce or the unsubscribe
/// click hands it in, so any rail can feed the same register.
#[derive(Debug, Clone)]
pub struct SuppressionRegister {
    workspace_layout: ArtifactV2Workspace,
    span: SuppressionSpan,
}

impl SuppressionRegister {
    /// The register every outward send should use.
    ///
    /// Global to the owner: a person who opted out is off limits everywhere,
    /// because consent given to one programme is not consent given to the next
    /// one.
    pub fn global(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            span: SuppressionSpan::Global,
        }
    }

    /// A register scoped to one audience — the narrow case, named at length on
    /// purpose.
    ///
    /// Use it only when the fact really is about one relationship: *"drop them
    /// from this cohort's mailings"*, not *"they unsubscribed"*. The difference
    /// matters because this constructor can only ever **add**:
    ///
    /// - reads consult the global register as well as this audience's, so a
    ///   global opt-out still blocks the send;
    /// - [`Self::suppress`] writes into this audience's span only;
    /// - [`Self::lift`] refuses to touch a global entry — a narrower scope
    ///   cannot undo a broader decision.
    ///
    /// Refuses an unnamed audience: a register bound to a blank id would answer
    /// for every anonymous caller at once.
    pub fn scoped_to_audience(
        workspace_layout: ArtifactV2Workspace,
        audience: AudienceRef,
    ) -> Result<Self> {
        if !audience.is_named() {
            anyhow::bail!(
                "a per-audience suppression register must name its audience: a blank id would \
                 merge every unnamed caller's entries into one register"
            );
        }
        if audience.id.contains(FIELD_SEP) {
            anyhow::bail!(
                "an audience id must not contain U+001F: it is the separator that keeps a \
                 suppression id's components from bleeding into each other, and a crafted id \
                 could otherwise resume — or shadow — another audience's entry"
            );
        }
        Ok(Self {
            workspace_layout,
            span: SuppressionSpan::Audience { audience },
        })
    }

    /// The span this register writes to.
    pub fn span(&self) -> &SuppressionSpan {
        &self.span
    }

    // ── The check ───────────────────────────────────────────────────────────

    /// Is this identity off limits right now?
    ///
    /// **The contract: `Err` means DO NOT SEND.** `Ok(None)` is the only clear
    /// answer this function gives. An unreadable register is an error precisely
    /// so that it cannot be mistaken for an empty one — the fail-open that
    /// [`crate::magician_v2::jsonl`] exists to close, and the one that would
    /// mail an address somebody explicitly asked us to stop mailing.
    ///
    /// The identity is normalised first — see [`normalise_identity`] — so the
    /// same address in different casing is the same person. A malformed
    /// identity is refused rather than reported clear: "we could not parse the
    /// recipient" is not permission to contact them.
    ///
    /// When several entries apply, the returned one is the entry a human most
    /// needs to be told about: consent decisions first, then the earliest
    /// established, then by id. A send blocked by both a bounce and an opt-out
    /// must not report the bounce, because the bounce reads as something an
    /// operator may fix.
    pub fn is_suppressed(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Suppression>> {
        let identity = normalise_identity(identity)?;
        let mut in_force: Vec<Suppression> = self
            .entries_for(scope, &identity)?
            .into_iter()
            .filter(|entry| entry.is_in_force(now))
            .collect();
        in_force.sort_by(|left, right| {
            right
                .reason
                .requires_owner_act_to_lift()
                .cmp(&left.reason.requires_owner_act_to_lift())
                .then_with(|| {
                    left.evidence
                        .established_at
                        .cmp(&right.evidence.established_at)
                })
                .then_with(|| left.suppression_id.cmp(&right.suppression_id))
        });
        Ok(in_force.into_iter().next())
    }

    /// Split a recipient list into who may be contacted and who may not.
    ///
    /// **Refuses an empty list.** "No recipient is suppressed" over zero
    /// recipients is vacuously true and tells the caller nothing, yet reads
    /// exactly like a clean bill of health — so a resolver that returned
    /// nothing, or a filter that removed everyone, would sail through the one
    /// gate meant to stop it. An empty list is a bug at the caller, and this is
    /// the module that must not be the one to wave it through.
    ///
    /// A malformed identity fails the whole screen rather than being skipped:
    /// silently dropping a recipient from the *check* while the mailer still
    /// holds it in the *send* is the same fail-open wearing a different hat.
    ///
    /// Identities are normalised and deduplicated, first occurrence winning, so
    /// a list holding an address twice yields one decision.
    pub fn screen(
        &self,
        scope: &SuppressionScope,
        identities: &[String],
        now: DateTime<Utc>,
    ) -> Result<Screened> {
        if identities.is_empty() {
            anyhow::bail!(
                "refusing to screen an empty recipient list: `no recipient is suppressed` over \
                 zero recipients is vacuously true and reads like a clean bill of health, so a \
                 resolver that returned nothing would pass the one gate meant to stop it"
            );
        }
        let mut seen = BTreeSet::new();
        let mut sendable = Vec::new();
        let mut blocked = Vec::new();
        for raw in identities {
            let identity = normalise_identity(raw)?;
            if !seen.insert(identity.clone()) {
                continue;
            }
            match self.is_suppressed(scope, &identity, now)? {
                Some(entry) => blocked.push(entry),
                None => sendable.push(identity),
            }
        }
        Ok(Screened { sendable, blocked })
    }

    // ── Writes ──────────────────────────────────────────────────────────────

    /// Add an identity to the register, indefinitely.
    ///
    /// Idempotent per `(identity, reason, evidence)`: replaying the same signal
    /// — a webhook redelivered, an import re-run — resumes the entry already
    /// written, keeping its original `suppressed_at`, rather than stacking
    /// duplicates. A replay whose *payload* differs (a different recorder, a
    /// different end date) under the same evidence is an **error**, not a quiet
    /// no-op: two different accounts of the same act must be reconciled by a
    /// human, and swallowing the second would drop a correction.
    ///
    /// A **new** act — different evidence — is a new entry, which is what lets
    /// somebody opt out again after a lift.
    pub fn suppress(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        reason: SuppressionReason,
        evidence: SuppressionEvidence,
        at: DateTime<Utc>,
    ) -> Result<Suppression> {
        self.record(scope, identity, reason, evidence, None, at)
    }

    /// Add a **time-boxed** entry — the explicitly-named form.
    ///
    /// For holds with an end: a quiet period, a dispute window. The end is
    /// derived on read, never swept, and is inclusive: in force until noon
    /// means clear at noon.
    ///
    /// Refused for [`SuppressionReason::OptOut`] and
    /// [`SuppressionReason::Complaint`]. A consent decision with a timer is a
    /// consent decision that expires quietly while nobody is looking.
    ///
    /// Refused when the end is already at or behind `at`: an entry that is
    /// expired the instant it is written records nothing and would read, to
    /// anybody auditing later, as a suppression that was honoured.
    pub fn suppress_until(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        reason: SuppressionReason,
        evidence: SuppressionEvidence,
        until: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<Suppression> {
        if !reason.may_be_time_boxed() {
            anyhow::bail!(
                "`{}` may not be time-boxed: a consent decision with an end date is one that \
                 expires quietly while nobody is looking, which is the same failure as a sweep \
                 reversing it. Record it indefinitely and lift it by an owner act if it is \
                 ever genuinely withdrawn.",
                reason.as_str()
            );
        }
        if until <= at {
            anyhow::bail!(
                "a hold whose end is already at or behind the moment it is recorded suppresses \
                 nothing — expiry is inclusive — yet would read to an auditor as a suppression \
                 that was honoured"
            );
        }
        self.record(scope, identity, reason, evidence, Some(until), at)
    }

    /// Record a batch of signals.
    ///
    /// Takes a plain slice: whoever noticed the bounces or the unsubscribes
    /// hands them in, and the register neither knows nor cares which transport
    /// they came from.
    ///
    /// **Every signal is validated before any is written.** A malformed entry
    /// mid-batch would otherwise abort the run with the earlier signals
    /// recorded and the later ones lost — silently under-suppressing exactly
    /// the identities nobody then looks at again. Refusing the whole batch up
    /// front leaves the caller with a fault it must fix and a batch it can
    /// safely re-run, because recording is idempotent per
    /// `(identity, reason, evidence)`.
    ///
    /// # Why the answer is not just the rows
    ///
    /// It returns [`Ingested`], which separates *what is now on file* from *what
    /// this call wrote*. Returning the rows alone made the two indistinguishable,
    /// and the one production caller counted every returned row as a fresh
    /// suppression: a sweep that re-read the same bounce reported it as a new
    /// suppression on every tick, forever, while its `already_held` count stayed
    /// permanently zero. The register itself was always correct — one row, one
    /// `suppressed_at` — so nothing downstream of it was wrong; what was wrong
    /// was the number an operator reads to decide whether anything is happening.
    pub fn ingest(
        &self,
        scope: &SuppressionScope,
        signals: &[SuppressionSignal],
        at: DateTime<Utc>,
    ) -> Result<Ingested> {
        validate_scope(scope)?;
        // A signal that fails validation is SKIPPED and counted, not raised.
        //
        // Raising was the obvious thing and it was wrong. The hygiene sweep
        // re-reads the same ledger window on every tick until its cursor
        // advances, and its cursor advances only on a clean pass. So one
        // unparseable identity — a ledger row from an older writer, a provider
        // returning something novel — did not lose one bounce, it stopped every
        // future bounce from ever being suppressed, and the failure looked like
        // an error in a log rather than mail still going to addresses that had
        // already bounced. The batch now makes what progress it can and reports
        // exactly what it could not read, so the pass is honest AND unwedged.
        let mut recorded = Vec::with_capacity(signals.len());
        let mut newly_recorded = 0usize;
        let mut unreadable = Vec::new();
        for signal in signals {
            let checked = normalise_identity(&signal.identity).and_then(|identity| {
                validated_evidence(&signal.evidence).map(|evidence| (identity, evidence))
            });
            let (identity, evidence) = match checked {
                Ok(checked) => checked,
                Err(error) => {
                    warn!(
                        target: LOG_TARGET,
                        identity = %signal.identity,
                        "a delivery signal could not be read and was skipped; it is reported in \
                         the sweep's `unreadable` count and the rest of the batch still \
                         recorded: {error:#}"
                    );
                    unreadable.push(signal.identity.clone());
                    continue;
                },
            };
            let (entry, wrote) =
                self.record_reporting(scope, &identity, signal.reason, evidence, None, at)?;
            if wrote {
                newly_recorded += 1;
            }
            recorded.push(entry);
        }
        Ok(Ingested {
            recorded,
            newly_recorded,
            unreadable,
        })
    }

    /// Stop a suppression applying — the only way, and never by deletion.
    ///
    /// A lift is a **new recorded act** beside the original entry, so the
    /// register still shows that the person was suppressed, when, on what
    /// evidence, and who reversed it on what evidence. There is no code path in
    /// this module that removes a row.
    ///
    /// [`SuppressionReason::OptOut`] and [`SuppressionReason::Complaint`]
    /// require [`LiftAuthority::OwnerAct`]. An
    /// [`LiftAuthority::Operational`] attempt is refused outright rather than
    /// downgraded or logged-and-ignored, because a sweep that can reverse an
    /// opt-out is the compliance failure this whole module exists to prevent.
    ///
    /// A per-audience register may not lift a global entry: a narrower scope
    /// cannot undo a broader decision, and the refusal names the global entry
    /// so the caller knows where to go.
    ///
    /// Idempotent: replaying the identical lift returns the entries already
    /// lifted, keeping the original `lifted_at` — *"we lifted it on Tuesday"*
    /// is a fact, and a retry must not move it. A **different** lift act
    /// against an already-lifted entry is an error, not a silent overwrite.
    pub fn lift(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        reason: SuppressionReason,
        authority: LiftAuthority,
        evidence: SuppressionEvidence,
        now: DateTime<Utc>,
    ) -> Result<Vec<Suppression>> {
        validate_scope(scope)?;
        let identity = normalise_identity(identity)?;
        let evidence = validated_evidence(&evidence)?;
        if reason.requires_owner_act_to_lift() && authority != LiftAuthority::OwnerAct {
            anyhow::bail!(
                "`{}` can only be lifted by an explicit owner act with evidence: an opt-out a \
                 sweep, an import or a bounce reclassification can reverse is the compliance \
                 failure this register exists to prevent",
                reason.as_str()
            );
        }

        let own: Vec<Suppression> = self
            .fold(scope, &self.span, &identity)?
            .into_iter()
            .filter(|entry| entry.reason == reason)
            .collect();
        let in_force: Vec<Suppression> = own
            .iter()
            .filter(|entry| entry.is_in_force(now))
            .cloned()
            .collect();

        if in_force.is_empty() {
            let already: Vec<Suppression> = own
                .iter()
                .filter(|entry| entry.lifted_at.is_some())
                .cloned()
                .collect();
            if !already.is_empty() {
                let replay = Lift {
                    authority,
                    evidence: evidence.clone(),
                };
                if already
                    .iter()
                    .all(|entry| entry.lift.as_ref() == Some(&replay))
                {
                    return Ok(already);
                }
                anyhow::bail!(
                    "`{identity}` was already lifted for `{}` by a different act; a second, \
                     differing account of the same reversal must be reconciled rather than \
                     silently overwriting who lifted it and when",
                    reason.as_str()
                );
            }
            if !self.span.is_global()
                && self
                    .fold(scope, &SuppressionSpan::Global, &identity)?
                    .iter()
                    .any(|entry| entry.reason == reason && entry.is_in_force(now))
            {
                anyhow::bail!(
                    "`{identity}` is suppressed for `{}` on the GLOBAL register; a per-audience \
                     register may not lift it, because a narrower scope cannot undo a broader \
                     decision — lift it on the global register or not at all",
                    reason.as_str()
                );
            }
            anyhow::bail!(
                "nothing in force to lift: `{identity}` has no live `{}` entry on `{}`",
                reason.as_str(),
                self.span.as_key()
            );
        }

        for entry in &in_force {
            self.append(
                &self.identity_path(scope, &self.span, &identity),
                &SuppressionRecord::Lifted {
                    suppression_id: entry.suppression_id.clone(),
                    at: now,
                    authority,
                    evidence: evidence.clone(),
                },
            )?;
        }

        let lifted_ids: BTreeSet<String> = in_force
            .iter()
            .map(|entry| entry.suppression_id.clone())
            .collect();
        let out: Vec<Suppression> = self
            .fold(scope, &self.span, &identity)?
            .into_iter()
            .filter(|entry| lifted_ids.contains(&entry.suppression_id))
            .collect();
        if out.len() != lifted_ids.len() {
            anyhow::bail!("an entry vanished immediately after being lifted");
        }
        Ok(out)
    }

    // ── Reporting ───────────────────────────────────────────────────────────

    /// Every entry recorded at or after `at`, oldest first.
    ///
    /// Inclusive of `at` itself, like every other boundary here.
    ///
    /// **Lifted entries are included.** The record of a suppression that was
    /// later reversed, and by whom, is the single most interesting row in a
    /// compliance review — a report that hid it would hide exactly the acts
    /// somebody is checking. Read [`Suppression::state`] to tell them apart.
    ///
    /// On a per-audience register this reports the audience's entries **and**
    /// the global ones that apply to it, matching what the check would answer.
    pub fn suppressed_since(
        &self,
        scope: &SuppressionScope,
        at: DateTime<Utc>,
    ) -> Result<Vec<Suppression>> {
        // A report is still a read of somebody's register. Without this, a
        // scope carrying the separator would be sanitised into a *different*
        // owner's directory and enumerate their entries.
        validate_scope(scope)?;
        let mut out = Vec::new();
        for span in self.spans_consulted() {
            for identity in self.indexed_identities(scope, &span)? {
                for entry in self.fold(scope, &span, &identity)? {
                    if entry.suppressed_at >= at {
                        out.push(entry);
                    }
                }
            }
        }
        out.sort_by(|left, right| {
            left.suppressed_at
                .cmp(&right.suppressed_at)
                .then_with(|| left.suppression_id.cmp(&right.suppression_id))
        });
        Ok(out)
    }

    /// How many entries each reason accounts for since `at`.
    ///
    /// **Counts, never a rate.** A "suppression rate" needs a denominator this
    /// register does not own — how many were sent, or attempted, or resolved —
    /// and a denominator taken from somewhere convenient is how a compliance
    /// number becomes fiction. Whoever owns the sends can divide.
    ///
    /// Every reason appears, zeros included, in
    /// [`SuppressionReason::ALL`] order: a report that omits *complaint: 0*
    /// leaves the reader unable to tell "none" from "not measured".
    pub fn counts_since(
        &self,
        scope: &SuppressionScope,
        at: DateTime<Utc>,
    ) -> Result<Vec<(SuppressionReason, usize)>> {
        let entries = self.suppressed_since(scope, at)?;
        Ok(SuppressionReason::ALL
            .into_iter()
            .map(|reason| {
                (
                    reason,
                    entries
                        .iter()
                        .filter(|entry| entry.reason == reason)
                        .count(),
                )
            })
            .collect())
    }

    /// Every entry ever recorded for one identity, across the spans this
    /// register consults — lifted and expired ones included.
    ///
    /// The history behind [`Self::is_suppressed`]. Ordered by
    /// `(suppressed_at, suppression_id)` so it reads the same on every call.
    pub fn history(&self, scope: &SuppressionScope, identity: &str) -> Result<Vec<Suppression>> {
        let identity = normalise_identity(identity)?;
        self.entries_for(scope, &identity)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    /// Which spans a read consults.
    ///
    /// A per-audience register consults the **global** register too. Without
    /// that, `SuppressionRegister::scoped_to_audience` would be a way to send
    /// to somebody who globally opted out simply by asking a narrower
    /// question — the register would have become the loophole.
    fn spans_consulted(&self) -> Vec<SuppressionSpan> {
        match &self.span {
            SuppressionSpan::Global => vec![SuppressionSpan::Global],
            audience => vec![SuppressionSpan::Global, audience.clone()],
        }
    }

    fn entries_for(&self, scope: &SuppressionScope, identity: &str) -> Result<Vec<Suppression>> {
        validate_scope(scope)?;
        let mut out = Vec::new();
        for span in self.spans_consulted() {
            out.extend(self.fold(scope, &span, identity)?);
        }
        out.sort_by(|left, right| {
            left.suppressed_at
                .cmp(&right.suppressed_at)
                .then_with(|| left.suppression_id.cmp(&right.suppression_id))
        });
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        reason: SuppressionReason,
        evidence: SuppressionEvidence,
        in_force_until: Option<DateTime<Utc>>,
        at: DateTime<Utc>,
    ) -> Result<Suppression> {
        self.record_reporting(scope, identity, reason, evidence, in_force_until, at)
            .map(|(entry, _wrote)| entry)
    }

    /// [`Self::record`], and whether THIS call wrote the row.
    ///
    /// The flag exists because the two outcomes are indistinguishable from the
    /// returned entry — a resumed row and a fresh one are the same shape, and
    /// deriving it from `suppressed_at == at` is the kind of guess that is right
    /// until two calls land on one instant. A caller that reports counts has to
    /// be able to tell them apart, or its "suppressed" number counts every
    /// signal it ever saw and its "already held" number is permanently zero.
    #[allow(clippy::too_many_arguments)]
    fn record_reporting(
        &self,
        scope: &SuppressionScope,
        identity: &str,
        reason: SuppressionReason,
        evidence: SuppressionEvidence,
        in_force_until: Option<DateTime<Utc>>,
        at: DateTime<Utc>,
    ) -> Result<(Suppression, bool)> {
        validate_scope(scope)?;
        let identity = normalise_identity(identity)?;
        let evidence = validated_evidence(&evidence)?;

        let suppression_id = derive_suppression_id(scope, &self.span, &identity, reason, &evidence);
        if let Some(existing) = self
            .fold(scope, &self.span, &identity)?
            .into_iter()
            .find(|entry| entry.suppression_id == suppression_id)
        {
            if existing.evidence == evidence && existing.in_force_until == in_force_until {
                return Ok((existing, false));
            }
            anyhow::bail!(
                "`{identity}` already has a `{}` entry on the same evidence with a different \
                 payload; an identical replay resumes, but a changed one is a correction that \
                 must be reconciled rather than silently dropped",
                reason.as_str()
            );
        }

        let entry = Suppression {
            suppression_id,
            identity: identity.clone(),
            reason,
            evidence,
            span: self.span.clone(),
            suppressed_at: at,
            in_force_until,
            lifted_at: None,
            lift: None,
        };

        // Index BEFORE row: "the row exists" must imply "the index exists". A
        // suppression the report cannot enumerate is one nobody audits, so a
        // crash between the two writes must leave a dangling index entry —
        // harmless, it folds to nothing — rather than an unindexed entry that
        // blocks sends while being invisible to every review.
        self.append_identity_index(scope, &self.span, &identity)?;
        self.append(
            &self.identity_path(scope, &self.span, &identity),
            &SuppressionRecord::Suppressed(entry.clone()),
        )?;
        Ok((entry, true))
    }

    /// Fold one identity's log in one span.
    ///
    /// **First wins, defensively as well as at the write.** A duplicated
    /// `Suppressed` line — a replay from an older binary, a torn concurrent
    /// append — must not move when somebody was suppressed or on what evidence;
    /// a duplicated `Lifted` line must not move when it was reversed. A
    /// `Lifted` naming an id this log does not hold is ignored: a lift can end
    /// an entry, never invent one.
    ///
    /// Read faults keep their two meanings apart, per
    /// [`crate::magician_v2::jsonl`]: a torn FINAL line is an append that never
    /// completed, and an unreadable file or a torn INTERIOR line is an error —
    /// never an empty register.
    fn fold(
        &self,
        scope: &SuppressionScope,
        span: &SuppressionSpan,
        identity: &str,
    ) -> Result<Vec<Suppression>> {
        let path = self.identity_path(scope, span, identity);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut out: Vec<Suppression> = Vec::new();
        for record in crate::magician_v2::jsonl::parse_log_lines::<SuppressionRecord>(&raw, &path)?
        {
            match record {
                SuppressionRecord::Suppressed(entry) => {
                    if !out
                        .iter()
                        .any(|held| held.suppression_id == entry.suppression_id)
                    {
                        out.push(entry);
                    }
                },
                SuppressionRecord::Lifted {
                    suppression_id,
                    at,
                    authority,
                    evidence,
                } => {
                    if let Some(held) = out
                        .iter_mut()
                        .find(|held| held.suppression_id == suppression_id)
                    {
                        if held.lift.is_none() {
                            held.lifted_at = Some(at);
                            held.lift = Some(Lift {
                                authority,
                                evidence,
                            });
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    /// The identities a span's index names, deduplicated, first-seen order.
    ///
    /// An unparseable INTERIOR entry fails the read: a silently skipped index
    /// line is an identity the compliance report never mentions, which is the
    /// exact silent miss the index exists to prevent. A torn FINAL line is an
    /// index append that never completed — and index-before-row means the entry
    /// it would have named was never written, so reading past it drops nothing.
    fn indexed_identities(
        &self,
        scope: &SuppressionScope,
        span: &SuppressionSpan,
    ) -> Result<Vec<String>> {
        let path = self.index_path(scope, span);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for identity in crate::magician_v2::jsonl::parse_log_lines::<String>(&raw, &path)? {
            if seen.insert(identity.clone()) {
                out.push(identity);
            }
        }
        Ok(out)
    }

    fn root(&self, scope: &SuppressionScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("suppression")
    }

    /// A span's directory. The audience key is hashed rather than used raw, so
    /// a caller-supplied audience id can never traverse out of the scope root.
    fn span_root(&self, scope: &SuppressionScope, span: &SuppressionSpan) -> PathBuf {
        match span {
            SuppressionSpan::Global => self.root(scope).join("global"),
            SuppressionSpan::Audience { .. } => self
                .root(scope)
                .join("audience")
                .join(stable_id(&span.as_key())),
        }
    }

    fn identity_path(
        &self,
        scope: &SuppressionScope,
        span: &SuppressionSpan,
        identity: &str,
    ) -> PathBuf {
        self.span_root(scope, span)
            .join("identities")
            .join(format!("{}.jsonl", stable_id(identity)))
    }

    fn index_path(&self, scope: &SuppressionScope, span: &SuppressionSpan) -> PathBuf {
        self.span_root(scope, span).join("index.jsonl")
    }

    /// One index line: the normalised identity, JSON-encoded rather than raw so
    /// nothing in it can shear the line format.
    fn append_identity_index(
        &self,
        scope: &SuppressionScope,
        span: &SuppressionSpan,
        identity: &str,
    ) -> Result<()> {
        let path = self.index_path(scope, span);
        let mut line = serde_json::to_vec(identity)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending index {}", path.display()))?;
        Ok(())
    }

    fn append(&self, path: &Path, record: &SuppressionRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    /// Read a log, distinguishing "absent" from "unreadable".
    ///
    /// Delegates to [`crate::magician_v2::jsonl::read_log_if_present`]: only a
    /// missing file reads as an empty register, and every other failure —
    /// EACCES, EIO, invalid UTF-8 from a torn write — propagates to the caller
    /// as the error that means DO NOT SEND.
    fn read_if_present(&self, path: &Path) -> Result<Option<String>> {
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// The canonical form of an identity, and the only form the register stores.
///
/// A suppressed address that reads as unsuppressed under different casing is
/// not a corner case: transports, forms and address books each hand back their
/// own capitalisation of the same mailbox, so without normalisation the
/// register would suppress a string rather than a person.
///
/// The rules, exactly:
///
/// 1. **Trim** leading and trailing whitespace.
/// 2. **Unwrap** surrounding angle brackets, trimming again after each pair.
///    Angle-wrapping is how addresses arrive from most mail tooling. Repeated
///    rather than done once, because normalising has to be **idempotent**:
///    [`SuppressionRegister::ingest`] and [`SuppressionRegister::screen`]
///    normalise and then hand the result to a callee that normalises again, so
///    a single unwrap left a doubly-wrapped address stored under one key and
///    looked up under another — suppressed, and reading as clear.
/// 3. **Lowercase**, in full Unicode, over the *whole* identity.
/// 4. **Refuse** what is left if it is empty, or holds U+001F, or holds any
///    other control character.
///
/// Two deliberate choices:
///
/// - **The local part is lowercased too**, though SMTP permits it to be
///   case-sensitive. Every mainstream provider treats it case-insensitively,
///   and the two possible mistakes are not symmetric: over-matching declines to
///   send to somebody we might have been allowed to contact, while
///   under-matching mails somebody who told us to stop. This register
///   over-matches on purpose, and the same reasoning applies to the non-email
///   identities it also holds — phone numbers, handles, opaque ids.
/// - **No provider-specific tricks.** Plus-tags are not stripped, dots are not
///   removed, no domain aliasing is applied. Those rules are true at some
///   providers and false at others, and a normaliser that guessed wrong would
///   fold two genuinely different people into one row — a suppression on one
///   silently gagging the other.
///
/// A malformed identity is an error, never a pass. "We could not parse the
/// recipient" is not permission to contact them.
pub fn normalise_identity(raw: &str) -> Result<String> {
    let mut unwrapped = raw.trim();
    // Every pair, not just the outermost: normalising must be idempotent, or
    // the paths that normalise twice disagree with the ones that normalise once
    // about which row an identity belongs to. Each turn removes at least two
    // characters, so this terminates.
    while let Some(inner) = unwrapped
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
    {
        unwrapped = inner.trim();
    }
    if unwrapped.is_empty() {
        anyhow::bail!(
            "a blank identity cannot be checked against the register, and an uncheckable \
             recipient is never a permitted one"
        );
    }
    if unwrapped.contains(FIELD_SEP) {
        anyhow::bail!(
            "an identity must not contain U+001F: it is the separator that keeps a suppression \
             id's components from bleeding into each other, and a crafted identity could \
             otherwise resume — or shadow — another identity's entry"
        );
    }
    if unwrapped.chars().any(char::is_control) {
        anyhow::bail!(
            "an identity must not contain control characters: one arriving here means the \
             recipient was mis-parsed upstream, and a mis-parsed recipient must be refused \
             rather than checked against the wrong row"
        );
    }
    Ok(unwrapped.to_lowercase())
}

/// Evidence, checked and trimmed.
///
/// Both halves are required. An entry whose evidence ref is blank cannot be
/// audited, so it is an assertion rather than a record — and an unauditable
/// suppression is indistinguishable from a mistake somebody will eventually
/// "clean up".
fn validated_evidence(evidence: &SuppressionEvidence) -> Result<SuppressionEvidence> {
    let evidence_ref = evidence.evidence_ref.trim();
    let recorded_by = evidence.recorded_by.trim();
    if evidence_ref.is_empty() {
        anyhow::bail!(
            "a suppression must cite the ref that established it: an entry nobody can check is \
             an assertion, not a record, and the first person to doubt it will remove it"
        );
    }
    if recorded_by.is_empty() {
        anyhow::bail!(
            "a suppression must say who recorded it: a register that cannot answer who acted \
             cannot be reviewed"
        );
    }
    if evidence_ref.contains(FIELD_SEP) {
        anyhow::bail!(
            "an evidence ref must not contain U+001F: it is the separator that keeps a \
             suppression id's components from bleeding into each other, and a crafted ref could \
             otherwise collide with another entry's id"
        );
    }
    if recorded_by.contains(FIELD_SEP) {
        anyhow::bail!("a recorder's name must not contain U+001F: it is the id separator");
    }
    Ok(SuppressionEvidence {
        established_at: evidence.established_at,
        evidence_ref: evidence_ref.to_string(),
        recorded_by: recorded_by.to_string(),
    })
}

fn validate_scope(scope: &SuppressionScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator \
             that keeps a suppression id's components from bleeding into each other, and a \
             crafted scope could otherwise resume another owner's entry"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one entry.
///
/// Derived, never assigned, from `(owner, span, identity, reason, evidence)` —
/// the same tuple [`SuppressionRegister::ingest`] is idempotent over, so a
/// redelivered webhook resumes the row it already wrote instead of stacking a
/// second one.
///
/// The **evidence** is in the tuple, and that is the load-bearing choice. Were
/// the id `(identity, reason)` alone, a fresh opt-out arriving after a lift
/// would resume the *lifted* row and read as not suppressed — a new act of
/// consent silently swallowed by an old reversal. With evidence in the tuple, a
/// new act is a new row that is in force from the moment it lands.
fn derive_suppression_id(
    scope: &SuppressionScope,
    span: &SuppressionSpan,
    identity: &str,
    reason: SuppressionReason,
    evidence: &SuppressionEvidence,
) -> String {
    format!(
        "sup-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            span.as_key(),
            identity,
            reason.as_str(),
            evidence.evidence_ref,
            evidence.established_at.to_rfc3339(),
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{Duration, TimeZone};

    fn scope() -> SuppressionScope {
        SuppressionScope::new("anonymous", "default")
    }

    /// A fixed clock. Every boundary below is asserted against an exact
    /// instant, never "roughly now".
    fn t(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0)
            .single()
            .expect("a real instant")
    }

    fn register() -> (tempfile::TempDir, SuppressionRegister) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let register = SuppressionRegister::global(ArtifactV2Workspace::new(tmp.path()));
        (tmp, register)
    }

    fn evidence(reference: &str, established: DateTime<Utc>) -> SuppressionEvidence {
        SuppressionEvidence::new(established, reference, "ingest-worker")
    }

    fn signal(
        identity: &str,
        reason: SuppressionReason,
        reference: &str,
        established: DateTime<Utc>,
    ) -> SuppressionSignal {
        SuppressionSignal {
            identity: identity.to_string(),
            reason,
            evidence: evidence(reference, established),
        }
    }

    fn zero_counts() -> Vec<(SuppressionReason, usize)> {
        vec![
            (SuppressionReason::OptOut, 0),
            (SuppressionReason::HardBounce, 0),
            (SuppressionReason::Complaint, 0),
            (SuppressionReason::OwnerBlocked, 0),
            (SuppressionReason::RegulatoryHold, 0),
        ]
    }

    // ── Fail closed ─────────────────────────────────────────────────────────

    /// **The reason this module is not a `HashSet`.** A register whose log
    /// cannot be read must answer `Err` — DO NOT SEND — and never `Ok(None)`.
    /// Answering "nobody is suppressed" because the disk faulted manufactures
    /// confidence for exactly the send that should have been held, and mails
    /// the person who asked us to stop.
    #[test]
    fn an_unreadable_register_is_an_error_never_nobody_is_suppressed() {
        let (_tmp, register) = register();
        let scope = scope();
        register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("recorded");

        // A directory where the identity's log belongs reproduces the whole
        // class of non-NotFound read faults portably — EACCES, EIO, ENOTDIR, an
        // unmounted volume. Which errno arrives is not the point; that none of
        // them folds to an empty register is.
        let path = register.identity_path(&scope, &SuppressionSpan::Global, "quiet@example.test");
        std::fs::remove_file(&path).expect("remove the log");
        std::fs::create_dir_all(&path).expect("a directory in its place");

        let single = register
            .is_suppressed(&scope, "quiet@example.test", t(11))
            .expect_err("an unreadable register must not read as an empty one");
        assert!(single.to_string().contains("unreadable"), "{single}");

        let batch = register
            .screen(&scope, &["quiet@example.test".to_string()], t(11))
            .expect_err("the batch path fails closed on the same fault");
        assert!(batch.to_string().contains("unreadable"), "{batch}");

        let report = register
            .suppressed_since(&scope, t(0))
            .expect_err("a compliance report must not silently under-report");
        assert!(report.to_string().contains("unreadable"), "{report}");
    }

    /// The other half of that contract: an **absent** log is the one fault that
    /// legitimately reads as an empty register, and it answers `None` rather
    /// than erroring — otherwise every first send would be blocked and callers
    /// would learn to ignore the gate.
    #[test]
    fn an_empty_register_answers_none_rather_than_erroring() {
        let (_tmp, register) = register();
        let scope = scope();

        assert_eq!(
            register
                .is_suppressed(&scope, "nobody@example.test", t(10))
                .expect("an absent log is an empty register"),
            None
        );
        assert_eq!(
            register
                .history(&scope, "nobody@example.test")
                .expect("history"),
            Vec::<Suppression>::new()
        );
        assert_eq!(
            register.suppressed_since(&scope, t(0)).expect("report"),
            Vec::<Suppression>::new()
        );
        assert_eq!(
            register.counts_since(&scope, t(0)).expect("counts"),
            zero_counts(),
            "every reason appears at zero: `none` must be distinguishable from `not measured`"
        );
    }

    /// Vacuous truth is a bug. "No recipient is suppressed" over zero
    /// recipients is true and useless, yet reads exactly like a clean bill of
    /// health — so a resolver that returned nothing, or a filter that removed
    /// everyone, would sail through the one gate meant to stop it.
    #[test]
    fn screening_an_empty_recipient_list_is_refused() {
        let (_tmp, register) = register();
        let error = register
            .screen(&scope(), &[], t(10))
            .expect_err("an empty screen is a bug at the caller");
        assert!(error.to_string().contains("vacuously true"), "{error}");
    }

    /// A malformed recipient fails the **whole** screen. Dropping it from the
    /// check while the mailer still holds it in the send is the same fail-open
    /// wearing a different hat.
    #[test]
    fn one_malformed_recipient_fails_the_whole_screen() {
        let (_tmp, register) = register();
        let scope = scope();

        let blank = register
            .screen(
                &scope,
                &[
                    "first@example.test".to_string(),
                    "   ".to_string(),
                    "second@example.test".to_string(),
                ],
                t(10),
            )
            .expect_err("a blank recipient is not a skippable one");
        assert!(blank.to_string().contains("blank identity"), "{blank}");

        let torn = register
            .screen(
                &scope,
                &["first@example.test".to_string(), "a\nb".to_string()],
                t(10),
            )
            .expect_err("a mis-parsed recipient is refused, not checked against the wrong row");
        assert!(torn.to_string().contains("control characters"), "{torn}");
    }

    // ── Normalisation ───────────────────────────────────────────────────────

    /// A suppressed address must stay suppressed however it is spelled back to
    /// us. Transports, forms and address books each hand back their own casing
    /// and wrapping of the same mailbox; without this the register suppresses a
    /// *string* rather than a *person*, and the next spelling mails them.
    #[test]
    fn a_suppressed_address_stays_suppressed_under_any_casing_or_wrapping() {
        let (_tmp, register) = register();
        let scope = scope();

        let recorded = register
            .suppress(
                &scope,
                "  <Quiet.Person@Example.TEST>  ",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("recorded");
        assert_eq!(recorded.identity, "quiet.person@example.test");

        for spelling in [
            "quiet.person@example.test",
            "QUIET.PERSON@EXAMPLE.TEST",
            "  <Quiet.Person@Example.Test>  ",
        ] {
            assert_eq!(
                register
                    .is_suppressed(&scope, spelling, t(11))
                    .expect("read")
                    .map(|entry| entry.suppression_id),
                Some(recorded.suppression_id.clone()),
                "`{spelling}` must reach the same row"
            );
        }

        // And the other direction: recorded in lower case, checked in upper.
        let lower = register
            .suppress(
                &scope,
                "second@example.test",
                SuppressionReason::Complaint,
                evidence("abuse-1", t(9)),
                t(10),
            )
            .expect("recorded");
        assert_eq!(
            register
                .is_suppressed(&scope, "<SECOND@Example.TEST>", t(11))
                .expect("read")
                .map(|entry| entry.suppression_id),
            Some(lower.suppression_id)
        );

        // One person, one row — not one row per spelling.
        assert_eq!(
            register
                .history(&scope, "QUIET.PERSON@example.TEST")
                .expect("history")
                .len(),
            1
        );
    }

    /// Normalising must be **idempotent**, because `ingest` and `screen`
    /// normalise and then hand the result to a callee that normalises again.
    /// When only the outermost pair of angle brackets was unwrapped, a
    /// doubly-wrapped address was STORED under one key and LOOKED UP under
    /// another: suppressed, and reading as clear.
    #[test]
    fn normalising_is_idempotent_so_the_write_and_the_check_agree() {
        assert_eq!(
            normalise_identity("<<Quiet@Example.TEST>>").expect("normalised"),
            "quiet@example.test"
        );
        for raw in [
            "<Quiet@Example.TEST>",
            "<< quiet@example.test >>",
            "  quiet@example.test  ",
        ] {
            let once = normalise_identity(raw).expect("once");
            assert_eq!(
                normalise_identity(&once).expect("twice"),
                once,
                "normalising `{raw}` twice must not move the key"
            );
        }
        // An unbalanced bracket is left alone rather than half-stripped.
        assert_eq!(
            normalise_identity("<Quiet@Example.TEST").expect("unbalanced"),
            "<quiet@example.test"
        );

        let (_tmp, register) = register();
        let scope = scope();
        register
            .ingest(
                &scope,
                &[signal(
                    "<<Quiet@Example.TEST>>",
                    SuppressionReason::OptOut,
                    "unsub-1",
                    t(9),
                )],
                t(10),
            )
            .expect("ingested");

        let held = register
            .is_suppressed(&scope, "<<Quiet@Example.TEST>>", t(11))
            .expect("read")
            .expect("the opt-out must block the very string it was ingested from");
        assert_eq!(held.identity, "quiet@example.test");
        assert_eq!(held.reason, SuppressionReason::OptOut);
    }

    // ── Time ────────────────────────────────────────────────────────────────

    /// A suppression applies at the **exact instant** it is recorded — there is
    /// no "not yet in force" window in which a known opt-out reads as clear,
    /// because clock skew alone would be enough to find one. A time-boxed entry
    /// expires **inclusively**, at its end instant rather than after it, and
    /// the state is derived from the clock on every read: nothing sweeps.
    #[test]
    fn a_suppression_is_in_force_at_its_exact_instant_and_expires_inclusively() {
        let (_tmp, register) = register();
        let scope = scope();

        let recorded = register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(12),
            )
            .expect("recorded");
        assert_eq!(recorded.state(t(12)), SuppressionState::InForce);
        assert_eq!(
            recorded.state(t(12) - Duration::hours(1)),
            SuppressionState::InForce,
            "no start window: a recorded suppression applies whatever `suppressed_at` says"
        );
        assert_eq!(
            register
                .is_suppressed(&scope, "quiet@example.test", t(12))
                .expect("read")
                .map(|entry| entry.suppression_id),
            Some(recorded.suppression_id)
        );

        let hold = register
            .suppress_until(
                &scope,
                "held@example.test",
                SuppressionReason::RegulatoryHold,
                evidence("hold-1", t(9)),
                t(18),
                t(12),
            )
            .expect("hold");
        assert_eq!(hold.in_force_until, Some(t(18)));
        assert_eq!(
            hold.state(t(18) - Duration::seconds(1)),
            SuppressionState::InForce
        );
        assert_eq!(
            hold.state(t(18)),
            SuppressionState::Expired,
            "in force until 18:00 means clear AT 18:00, not after it"
        );
        assert_eq!(
            register
                .is_suppressed(&scope, "held@example.test", t(18) - Duration::seconds(1))
                .expect("read")
                .map(|entry| entry.reason),
            Some(SuppressionReason::RegulatoryHold)
        );
        assert_eq!(
            register
                .is_suppressed(&scope, "held@example.test", t(18))
                .expect("read"),
            None
        );

        // Derived, never stored: a fresh fold agrees, and nothing was written.
        let reread = register
            .history(&scope, "held@example.test")
            .expect("history");
        assert_eq!(reread.len(), 1);
        assert_eq!(reread[0].state(t(18)), SuppressionState::Expired);
        assert_eq!(reread[0].lifted_at, None, "an expiry is not a lift");
    }

    /// A consent decision with a timer is one that expires quietly while nobody
    /// is looking — the same failure as a sweep reversing it, only slower and
    /// harder to notice. And a hold already over when it is written suppresses
    /// nothing while reading, to an auditor, as one that was honoured.
    #[test]
    fn a_consent_decision_may_not_be_time_boxed_and_a_dead_window_is_refused() {
        assert!(!SuppressionReason::OptOut.may_be_time_boxed());
        assert!(!SuppressionReason::Complaint.may_be_time_boxed());
        assert!(SuppressionReason::HardBounce.may_be_time_boxed());
        assert!(SuppressionReason::OwnerBlocked.may_be_time_boxed());
        assert!(SuppressionReason::RegulatoryHold.may_be_time_boxed());

        let (_tmp, register) = register();
        let scope = scope();
        for reason in [SuppressionReason::OptOut, SuppressionReason::Complaint] {
            let error = register
                .suppress_until(
                    &scope,
                    "quiet@example.test",
                    reason,
                    evidence("unsub-1", t(9)),
                    t(18),
                    t(12),
                )
                .expect_err("a consent decision with an end date");
            assert!(
                error.to_string().contains("may not be time-boxed"),
                "{error}"
            );
        }
        assert_eq!(
            register
                .is_suppressed(&scope, "quiet@example.test", t(13))
                .expect("read"),
            None,
            "a refused write leaves nothing behind"
        );

        let dead = register
            .suppress_until(
                &scope,
                "held@example.test",
                SuppressionReason::RegulatoryHold,
                evidence("hold-1", t(9)),
                t(12),
                t(12),
            )
            .expect_err("expiry is inclusive, so an end at `at` suppresses nothing");
        assert!(dead.to_string().contains("suppresses nothing"), "{dead}");
    }

    // ── Lifting ─────────────────────────────────────────────────────────────

    /// An opt-out a sweep, a re-import or a bounce reclassification can reverse
    /// is the compliance failure this whole module exists to prevent. Only an
    /// explicit owner act, carrying evidence an auditor can follow, clears one
    /// — and an unauditable lift is refused just as firmly as an unauthorised
    /// one.
    #[test]
    fn a_consent_decision_is_liftable_only_by_an_owner_act_carrying_evidence() {
        let (_tmp, register) = register();
        let scope = scope();

        for reason in [SuppressionReason::OptOut, SuppressionReason::Complaint] {
            let identity = format!("{}@example.test", reason.as_str());
            register
                .suppress(&scope, &identity, reason, evidence("act-1", t(9)), t(10))
                .expect("recorded");

            let refused = register
                .lift(
                    &scope,
                    &identity,
                    reason,
                    LiftAuthority::Operational,
                    evidence("ops-sweep-1", t(11)),
                    t(11),
                )
                .expect_err("an operational lift of a consent decision");
            assert!(
                refused.to_string().contains("explicit owner act"),
                "{refused}"
            );
            assert_eq!(
                register
                    .is_suppressed(&scope, &identity, t(12))
                    .expect("read")
                    .map(|entry| entry.reason),
                Some(reason),
                "the refusal must leave the suppression standing"
            );
        }

        let identity = "opt_out@example.test";
        let unciteable = register
            .lift(
                &scope,
                identity,
                SuppressionReason::OptOut,
                LiftAuthority::OwnerAct,
                SuppressionEvidence::new(t(11), "   ", "owner"),
                t(11),
            )
            .expect_err("an owner act nobody can check");
        assert!(
            unciteable.to_string().contains("cite the ref"),
            "{unciteable}"
        );

        let anonymous = register
            .lift(
                &scope,
                identity,
                SuppressionReason::OptOut,
                LiftAuthority::OwnerAct,
                SuppressionEvidence::new(t(11), "owner-note-1", "  "),
                t(11),
            )
            .expect_err("a lift nobody signed");
        assert!(
            anonymous.to_string().contains("who recorded it"),
            "{anonymous}"
        );
        assert_eq!(
            register
                .is_suppressed(&scope, identity, t(11))
                .expect("read")
                .map(|entry| entry.reason),
            Some(SuppressionReason::OptOut)
        );

        let lifted = register
            .lift(
                &scope,
                identity,
                SuppressionReason::OptOut,
                LiftAuthority::OwnerAct,
                SuppressionEvidence::new(t(11), "  owner-note-1  ", "  owner  "),
                t(12),
            )
            .expect("an owner act with evidence");
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].lifted_at, Some(t(12)));
        assert_eq!(
            lifted[0].lift,
            Some(Lift {
                authority: LiftAuthority::OwnerAct,
                evidence: SuppressionEvidence::new(t(11), "owner-note-1", "owner"),
            })
        );
        assert_eq!(lifted[0].state(t(12)), SuppressionState::Lifted);
        assert_eq!(
            register
                .is_suppressed(&scope, identity, t(13))
                .expect("read"),
            None
        );
    }

    /// A lift is a **new recorded act beside the entry**, never a deletion:
    /// the register must still be able to say that the person was suppressed,
    /// when, on what evidence, and who reversed it on what evidence.
    #[test]
    fn a_lift_is_recorded_beside_the_entry_and_never_deletes_it() {
        let (_tmp, register) = register();
        let scope = scope();
        let recorded = register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(10),
            )
            .expect("recorded");
        register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("transport-correction-1", t(11)),
                t(12),
            )
            .expect("an operational reason may be lifted operationally");

        assert_eq!(
            register
                .is_suppressed(&scope, "bounced@example.test", t(13))
                .expect("read"),
            None
        );

        let history = register
            .history(&scope, "bounced@example.test")
            .expect("history");
        assert_eq!(history.len(), 1, "the original row survives the lift");
        assert_eq!(history[0].suppression_id, recorded.suppression_id);
        assert_eq!(history[0].evidence, evidence("bounce-1", t(9)));
        assert_eq!(history[0].suppressed_at, t(10));
        assert_eq!(history[0].lifted_at, Some(t(12)));
        assert_eq!(
            history[0].lift.as_ref().map(|lift| lift.authority),
            Some(LiftAuthority::Operational)
        );
        assert_eq!(history[0].state(t(13)), SuppressionState::Lifted);

        // Append-only on disk: the suppression line is still there, with the
        // lift recorded after it.
        let path = register.identity_path(&scope, &SuppressionSpan::Global, "bounced@example.test");
        let raw = std::fs::read_to_string(&path).expect("the log");
        let lines: Vec<&str> = raw.lines().filter(|line| !line.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "two records, nothing rewritten: {raw}");
        assert!(
            lines[0].contains("\"record\":\"suppressed\""),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("\"record\":\"lifted\""), "{}", lines[1]);
    }

    /// *"We lifted it on Tuesday"* is a fact, and a retry must not move it — but
    /// a **different** account of the same reversal is a correction somebody has
    /// to reconcile, not something to overwrite silently.
    #[test]
    fn replaying_a_lift_resumes_it_while_a_differing_one_is_an_error() {
        let (_tmp, register) = register();
        let scope = scope();
        register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(10),
            )
            .expect("recorded");
        let first = register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(12),
            )
            .expect("lifted");
        assert_eq!(first[0].lifted_at, Some(t(12)));

        let replay = register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(14),
            )
            .expect("an identical replay resumes");
        assert_eq!(
            replay[0].lifted_at,
            Some(t(12)),
            "a retry must not move when it was reversed"
        );
        let path = register.identity_path(&scope, &SuppressionSpan::Global, "bounced@example.test");
        let raw = std::fs::read_to_string(&path).expect("the log");
        assert_eq!(
            raw.lines().filter(|line| !line.trim().is_empty()).count(),
            2,
            "the replay appended nothing: {raw}"
        );

        let different_ref = register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-2", t(11)),
                t(15),
            )
            .expect_err("a second, differing account of the reversal");
        assert!(
            different_ref.to_string().contains("reconciled"),
            "{different_ref}"
        );

        let different_authority = register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::OwnerAct,
                evidence("correction-1", t(11)),
                t(15),
            )
            .expect_err("the same evidence under a different authority is a different act");
        assert!(
            different_authority.to_string().contains("already lifted"),
            "{different_authority}"
        );
    }

    /// A lift can end an entry, never invent one: lifting what is not in force
    /// is an error rather than a no-op, so a caller cannot be told a
    /// suppression was cleared when none was found.
    #[test]
    fn lifting_what_is_not_in_force_is_an_error() {
        let (_tmp, register) = register();
        let error = register
            .lift(
                &scope(),
                "stranger@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(12),
            )
            .expect_err("nothing to lift");
        assert!(
            error.to_string().contains("nothing in force to lift"),
            "{error}"
        );
    }

    // ── Terminal states, replays and new acts ───────────────────────────────

    /// A fresh act of consent after a lift is a **new row**, in force from the
    /// moment it lands. Were the id `(identity, reason)` alone, the new opt-out
    /// would resume the *lifted* row and read as clear — a new decision
    /// silently swallowed by an old reversal.
    #[test]
    fn an_identity_lifted_and_then_suppressed_again_is_suppressed() {
        let (_tmp, register) = register();
        let scope = scope();
        let first = register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("first opt-out");
        register
            .lift(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                LiftAuthority::OwnerAct,
                evidence("owner-note-1", t(11)),
                t(12),
            )
            .expect("lifted");
        assert_eq!(
            register
                .is_suppressed(&scope, "quiet@example.test", t(13))
                .expect("read"),
            None
        );

        let again = register
            .suppress(
                &scope,
                "QUIET@Example.TEST",
                SuppressionReason::OptOut,
                evidence("unsub-2", t(13)),
                t(14),
            )
            .expect("they opted out again");
        assert_ne!(again.suppression_id, first.suppression_id);
        assert_eq!(again.lifted_at, None);

        let held = register
            .is_suppressed(&scope, "quiet@example.test", t(15))
            .expect("read")
            .expect("the new act is in force");
        assert_eq!(held.suppression_id, again.suppression_id);
        assert_eq!(held.evidence.evidence_ref, "unsub-2");
        assert_eq!(
            register
                .history(&scope, "quiet@example.test")
                .expect("history")
                .len(),
            2,
            "both acts are on the record"
        );
    }

    /// Terminal states never resurrect. Replaying the **original** signal after
    /// a lift resumes the lifted row — it is the same act, already reversed —
    /// rather than writing a live one, which is what a redelivered webhook or a
    /// re-run import would otherwise do.
    #[test]
    fn replaying_the_original_signal_after_a_lift_does_not_resurrect_it() {
        let (_tmp, register) = register();
        let scope = scope();
        register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(10),
            )
            .expect("recorded");
        register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(12),
            )
            .expect("lifted");

        let replay = register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(14),
            )
            .expect("the replay resumes the row it already wrote");
        assert_eq!(replay.suppressed_at, t(10));
        assert_eq!(replay.lifted_at, Some(t(12)));
        assert_eq!(
            register
                .is_suppressed(&scope, "bounced@example.test", t(15))
                .expect("read"),
            None
        );
        assert_eq!(
            register
                .history(&scope, "bounced@example.test")
                .expect("history")
                .len(),
            1
        );
    }

    /// One unreadable signal must not cost the batch it arrived in.
    ///
    /// The hygiene sweep re-reads the same ledger window until its cursor
    /// advances, and the cursor advances only on a clean pass — so raising on a
    /// bad row did not lose one bounce, it stopped every later bounce from ever
    /// being suppressed, while the failure looked like a log line rather than
    /// mail still going to addresses that had already bounced.
    #[test]
    fn an_unreadable_signal_is_skipped_and_counted_not_raised() {
        let (_tmp, register) = register();
        let scope = scope();
        let ingested = register
            .ingest(
                &scope,
                &[
                    signal(
                        "first@example.test",
                        SuppressionReason::HardBounce,
                        "b-1",
                        t(9),
                    ),
                    // Blank: uncheckable against the register, and the whole
                    // batch used to die on it.
                    signal("   ", SuppressionReason::HardBounce, "b-2", t(9)),
                    signal(
                        "third@example.test",
                        SuppressionReason::HardBounce,
                        "b-3",
                        t(9),
                    ),
                ],
                t(10),
            )
            .expect("a batch with one bad signal still records the good ones");

        assert_eq!(
            ingested.newly_recorded, 2,
            "both readable signals were written"
        );
        assert_eq!(
            ingested.unreadable.len(),
            1,
            "and the one that could not be read is REPORTED, not swallowed"
        );
        assert_eq!(
            ingested.newly_recorded + ingested.already_held() + ingested.unreadable.len(),
            3,
            "every signal offered is accounted for under exactly one outcome"
        );
        for identity in ["first@example.test", "third@example.test"] {
            assert!(
                register
                    .is_suppressed(&scope, identity, t(11))
                    .expect("read")
                    .is_some(),
                "`{identity}` bounced and must be suppressed even though a sibling signal \
                 could not be read"
            );
        }
    }

    /// Idempotent per `(identity, reason, evidence)`: a redelivered webhook or a
    /// re-run import resumes the entry already written, keeping its original
    /// `suppressed_at`, instead of stacking duplicates that make the register
    /// unreadable. Casing is part of that — the same address in a new spelling
    /// is the same signal.
    #[test]
    fn ingest_is_idempotent_per_identity_reason_and_evidence() {
        let (_tmp, register) = register();
        let scope = scope();
        let first = register
            .ingest(
                &scope,
                &[signal(
                    "Bounced@Example.TEST",
                    SuppressionReason::HardBounce,
                    "bounce-1",
                    t(9),
                )],
                t(10),
            )
            .expect("ingested");
        assert_eq!(first.recorded.len(), 1);
        assert_eq!(first.newly_recorded, 1, "the first ingest writes the row");
        assert_eq!(first.recorded[0].identity, "bounced@example.test");
        assert_eq!(first.recorded[0].suppressed_at, t(10));

        for (spelling, when) in [
            ("Bounced@Example.TEST", t(14)),
            ("<bounced@example.test>", t(15)),
        ] {
            let replay = register
                .ingest(
                    &scope,
                    &[signal(
                        spelling,
                        SuppressionReason::HardBounce,
                        "bounce-1",
                        t(9),
                    )],
                    when,
                )
                .expect("redelivered");
            assert_eq!(
                replay.recorded[0].suppression_id,
                first.recorded[0].suppression_id
            );
            assert_eq!(
                replay.newly_recorded, 0,
                "a replay resumes the row it already wrote; counting it as new is how a sweep \
                 reports the same bounce forever"
            );
            assert_eq!(replay.already_held(), 1);
            assert_eq!(
                replay.recorded[0].suppressed_at,
                t(10),
                "a replay must not move when `{spelling}` was suppressed"
            );
        }
        assert_eq!(
            register
                .history(&scope, "bounced@example.test")
                .expect("history")
                .len(),
            1
        );
        assert_eq!(
            register.counts_since(&scope, t(0)).expect("counts"),
            vec![
                (SuppressionReason::OptOut, 0),
                (SuppressionReason::HardBounce, 1),
                (SuppressionReason::Complaint, 0),
                (SuppressionReason::OwnerBlocked, 0),
                (SuppressionReason::RegulatoryHold, 0),
            ]
        );

        // A genuinely new act — different evidence — is a new entry.
        let fresh = register
            .ingest(
                &scope,
                &[signal(
                    "bounced@example.test",
                    SuppressionReason::HardBounce,
                    "bounce-2",
                    t(16),
                )],
                t(17),
            )
            .expect("a second bounce");
        assert_ne!(
            fresh.recorded[0].suppression_id,
            first.recorded[0].suppression_id
        );
        assert_eq!(
            fresh.newly_recorded, 1,
            "different evidence is a genuinely new entry"
        );
        assert_eq!(
            register
                .history(&scope, "bounced@example.test")
                .expect("history")
                .len(),
            2
        );
    }

    /// An unreadable signal is reported without wedging the readable siblings.
    #[test]
    fn a_malformed_signal_is_reported_while_readable_siblings_are_recorded() {
        let (_tmp, register) = register();
        let scope = scope();
        let ingested = register
            .ingest(
                &scope,
                &[
                    signal(
                        "first@example.test",
                        SuppressionReason::HardBounce,
                        "bounce-1",
                        t(9),
                    ),
                    SuppressionSignal {
                        identity: "second@example.test".to_string(),
                        reason: SuppressionReason::OptOut,
                        evidence: SuppressionEvidence::new(t(9), "  ", "ingest-worker"),
                    },
                    signal(
                        "third@example.test",
                        SuppressionReason::HardBounce,
                        "bounce-2",
                        t(9),
                    ),
                ],
                t(10),
            )
            .expect("readable siblings remain ingestible");
        assert_eq!(ingested.newly_recorded, 2);
        assert_eq!(ingested.unreadable, vec!["second@example.test"]);
        for identity in ["first@example.test", "third@example.test"] {
            assert!(register
                .is_suppressed(&scope, identity, t(11))
                .expect("read")
                .is_some());
        }
        assert_eq!(
            register
                .is_suppressed(&scope, "second@example.test", t(11))
                .expect("read"),
            None
        );
    }

    /// An identical replay resumes; a **changed** payload under the same
    /// evidence is an error, not a silent no-op. Two different accounts of the
    /// same act have to be reconciled by a human, and swallowing the second
    /// would drop a correction.
    #[test]
    fn an_identical_replay_resumes_but_a_changed_payload_is_an_error() {
        let (_tmp, register) = register();
        let scope = scope();
        let first = register
            .suppress(
                &scope,
                "blocked@example.test",
                SuppressionReason::OwnerBlocked,
                SuppressionEvidence::new(t(9), "ticket-1", "alice"),
                t(10),
            )
            .expect("recorded");

        let replay = register
            .suppress(
                &scope,
                "blocked@example.test",
                SuppressionReason::OwnerBlocked,
                SuppressionEvidence::new(t(9), "ticket-1", "alice"),
                t(14),
            )
            .expect("an identical replay resumes");
        assert_eq!(replay.suppression_id, first.suppression_id);
        assert_eq!(replay.suppressed_at, t(10));

        let different_recorder = register
            .suppress(
                &scope,
                "blocked@example.test",
                SuppressionReason::OwnerBlocked,
                SuppressionEvidence::new(t(9), "ticket-1", "bob"),
                t(14),
            )
            .expect_err("a second account of who acted");
        assert!(
            different_recorder.to_string().contains("reconciled"),
            "{different_recorder}"
        );

        let different_window = register
            .suppress_until(
                &scope,
                "blocked@example.test",
                SuppressionReason::OwnerBlocked,
                SuppressionEvidence::new(t(9), "ticket-1", "alice"),
                t(18),
                t(14),
            )
            .expect_err("the same act with an end date bolted on");
        assert!(
            different_window.to_string().contains("reconciled"),
            "{different_window}"
        );

        assert_eq!(
            register
                .history(&scope, "blocked@example.test")
                .expect("history")
                .len(),
            1,
            "a refused correction writes nothing"
        );
    }

    // ── Which entry the caller is told about ────────────────────────────────

    /// A send blocked by both a bounce and an opt-out must report the
    /// **opt-out**: a bounce reads as something an operator may fix, and the
    /// consent decision is the one a human needs to be told about — even when
    /// the bounce was established first.
    #[test]
    fn the_answer_names_the_consent_decision_when_several_entries_apply() {
        let (_tmp, register) = register();
        let scope = scope();
        register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(1)),
                t(10),
            )
            .expect("bounce");
        register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("opt-out");

        let held = register
            .is_suppressed(&scope, "quiet@example.test", t(11))
            .expect("read")
            .expect("suppressed");
        assert_eq!(held.reason, SuppressionReason::OptOut);
        assert_eq!(held.evidence.evidence_ref, "unsub-1");
        assert_eq!(
            register
                .history(&scope, "quiet@example.test")
                .expect("history")
                .len(),
            2,
            "both entries stay on the record"
        );
    }

    /// The batch answer is explicit on both halves — a caller reads `sendable`,
    /// never `blocked.is_empty()` — and a list holding one address twice, in two
    /// spellings, yields one decision rather than two.
    #[test]
    fn screening_answers_in_values_and_deduplicates_spellings() {
        let (_tmp, register) = register();
        let scope = scope();
        let blocked = register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("recorded");

        let screened = register
            .screen(
                &scope,
                &[
                    "Quiet@Example.TEST".to_string(),
                    "clear-one@example.test".to_string(),
                    "<quiet@example.test>".to_string(),
                    "CLEAR-ONE@example.test".to_string(),
                    "clear-two@example.test".to_string(),
                ],
                t(11),
            )
            .expect("screened");
        assert_eq!(
            screened,
            Screened {
                sendable: vec![
                    "clear-one@example.test".to_string(),
                    "clear-two@example.test".to_string(),
                ],
                blocked: vec![blocked],
            }
        );
    }

    // ── Scope ───────────────────────────────────────────────────────────────

    /// Global by default. Somebody who opted out of one programme has **not**
    /// consented to the next one, so a global decision reaches every programme
    /// — while a per-programme entry stays where it was made, because a
    /// narrower scope may only ever add.
    #[test]
    fn a_global_opt_out_blocks_every_programme_while_a_programme_entry_stays_local() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let scope = scope();
        let global = SuppressionRegister::global(workspace.clone());
        let spring = SuppressionRegister::scoped_to_audience(
            workspace.clone(),
            AudienceRef::program("spring"),
        )
        .expect("a named audience");
        let autumn =
            SuppressionRegister::scoped_to_audience(workspace, AudienceRef::program("autumn"))
                .expect("a named audience");

        global
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("global opt-out");
        spring
            .suppress(
                &scope,
                "cohort@example.test",
                SuppressionReason::OwnerBlocked,
                evidence("note-1", t(9)),
                t(10),
            )
            .expect("programme entry");

        for register in [&global, &spring, &autumn] {
            assert_eq!(
                register
                    .is_suppressed(&scope, "quiet@example.test", t(11))
                    .expect("read")
                    .map(|entry| (entry.reason, entry.span)),
                Some((SuppressionReason::OptOut, SuppressionSpan::Global)),
                "a global opt-out is not something a narrower question can route around"
            );
        }

        assert_eq!(
            spring
                .is_suppressed(&scope, "cohort@example.test", t(11))
                .expect("read")
                .map(|entry| entry.span),
            Some(SuppressionSpan::Audience {
                audience: AudienceRef::program("spring"),
            })
        );
        assert_eq!(
            autumn
                .is_suppressed(&scope, "cohort@example.test", t(11))
                .expect("read"),
            None,
            "one programme's entry is not another programme's fact"
        );
        assert_eq!(
            global
                .is_suppressed(&scope, "cohort@example.test", t(11))
                .expect("read"),
            None
        );
    }

    /// A per-audience register may not lift a global entry — a narrower scope
    /// cannot undo a broader decision — but it may lift its own.
    #[test]
    fn a_programme_register_cannot_lift_a_global_decision() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let scope = scope();
        let global = SuppressionRegister::global(workspace.clone());
        let spring =
            SuppressionRegister::scoped_to_audience(workspace, AudienceRef::program("spring"))
                .expect("a named audience");

        global
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("global opt-out");

        let refused = spring
            .lift(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                LiftAuthority::OwnerAct,
                evidence("owner-note-1", t(11)),
                t(12),
            )
            .expect_err("a narrower scope reaching for a broader decision");
        assert!(refused.to_string().contains("GLOBAL"), "{refused}");
        assert_eq!(
            spring
                .is_suppressed(&scope, "quiet@example.test", t(13))
                .expect("read")
                .map(|entry| entry.reason),
            Some(SuppressionReason::OptOut)
        );

        spring
            .suppress(
                &scope,
                "cohort@example.test",
                SuppressionReason::OwnerBlocked,
                evidence("note-1", t(9)),
                t(10),
            )
            .expect("its own entry");
        let lifted = spring
            .lift(
                &scope,
                "cohort@example.test",
                SuppressionReason::OwnerBlocked,
                LiftAuthority::Operational,
                evidence("note-2", t(11)),
                t(12),
            )
            .expect("a register may lift what it recorded");
        assert_eq!(lifted[0].lifted_at, Some(t(12)));
        assert_eq!(
            spring
                .is_suppressed(&scope, "cohort@example.test", t(13))
                .expect("read"),
            None
        );
    }

    /// A register bound to a blank audience would answer for every anonymous
    /// caller at once — one programme's opt-outs silently gagging another's.
    #[test]
    fn a_per_audience_register_must_name_its_audience() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let error = SuppressionRegister::scoped_to_audience(workspace, AudienceRef::program("   "))
            .expect_err("an unnamed audience");
        assert!(
            error.to_string().contains("must name its audience"),
            "{error}"
        );
    }

    /// The audience **kind** is part of the span key, so two relationships that
    /// happen to share an id are two spans. Without it, widening the binding
    /// would silently merge them.
    #[test]
    fn two_audience_kinds_sharing_an_id_are_two_spans() {
        assert_eq!(
            SuppressionSpan::Audience {
                audience: AudienceRef::program("acme"),
            }
            .as_key(),
            "audience:program:acme"
        );
        assert_ne!(
            SuppressionSpan::Audience {
                audience: AudienceRef::account("acme"),
            }
            .as_key(),
            SuppressionSpan::Audience {
                audience: AudienceRef::program("acme"),
            }
            .as_key()
        );
        assert!(SuppressionSpan::Global.is_global());
        assert_eq!(SuppressionSpan::Global.as_key(), "global");
    }

    // ── Id integrity ────────────────────────────────────────────────────────

    /// Every caller string that feeds an id derivation is refused if it holds
    /// the field separator. Without that, a crafted value could resume — or
    /// shadow — another identity's, another audience's, or another owner's
    /// entry by making the id's components bleed into each other.
    #[test]
    fn the_field_separator_is_refused_everywhere_it_could_forge_an_id() {
        let separator = '\u{1f}';

        let identity = normalise_identity(&format!("a{separator}b@example.test"))
            .expect_err("an identity carrying the separator");
        assert!(identity.to_string().contains("U+001F"), "{identity}");

        let (_tmp, register) = register();
        let scope = scope();
        let evidence_ref = register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                SuppressionEvidence::new(t(9), format!("unsub{separator}1"), "ingest-worker"),
                t(10),
            )
            .expect_err("an evidence ref carrying the separator");
        assert!(
            evidence_ref.to_string().contains("U+001F"),
            "{evidence_ref}"
        );

        let recorder = register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                SuppressionEvidence::new(t(9), "unsub-1", format!("worker{separator}1")),
                t(10),
            )
            .expect_err("a recorder carrying the separator");
        assert!(recorder.to_string().contains("U+001F"), "{recorder}");

        let crafted = SuppressionScope::new(format!("anonymous{separator}other"), "default");
        let write = register
            .suppress(
                &crafted,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect_err("a scope carrying the separator");
        assert!(write.to_string().contains("U+001F"), "{write}");
        let read = register
            .is_suppressed(&crafted, "quiet@example.test", t(10))
            .expect_err("the check refuses it too");
        assert!(read.to_string().contains("U+001F"), "{read}");
        let report = register
            .suppressed_since(&crafted, t(0))
            .expect_err("and so does the report");
        assert!(report.to_string().contains("U+001F"), "{report}");

        let tmp = tempfile::tempdir().expect("temp dir");
        let audience = SuppressionRegister::scoped_to_audience(
            ArtifactV2Workspace::new(tmp.path()),
            AudienceRef::program(format!("spring{separator}2026")),
        )
        .expect_err("an audience id carrying the separator");
        assert!(audience.to_string().contains("U+001F"), "{audience}");
    }

    // ── Reporting ───────────────────────────────────────────────────────────

    /// Index-before-row: everything recorded is enumerable by the report, the
    /// `since` boundary is inclusive, lifted entries stay visible — they are the
    /// most interesting row in a review — and the counts name every reason,
    /// zeros included, so "none" is distinguishable from "not measured".
    #[test]
    fn the_report_enumerates_every_entry_including_the_lifted_ones() {
        let (_tmp, register) = register();
        let scope = scope();
        register
            .suppress(
                &scope,
                "quiet@example.test",
                SuppressionReason::OptOut,
                evidence("unsub-1", t(9)),
                t(10),
            )
            .expect("opt-out");
        register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(11),
            )
            .expect("bounce");
        register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(12),
            )
            .expect("lifted");

        let all = register.suppressed_since(&scope, t(0)).expect("report");
        assert_eq!(
            all.iter()
                .map(|entry| entry.identity.as_str())
                .collect::<Vec<_>>(),
            vec!["quiet@example.test", "bounced@example.test"],
            "oldest first, and the lifted entry is still on the report"
        );
        assert_eq!(all[1].state(t(13)), SuppressionState::Lifted);

        let since = register.suppressed_since(&scope, t(11)).expect("report");
        assert_eq!(
            since
                .iter()
                .map(|entry| entry.identity.as_str())
                .collect::<Vec<_>>(),
            vec!["bounced@example.test"],
            "the `since` boundary is inclusive of the instant itself"
        );

        assert_eq!(
            register.counts_since(&scope, t(0)).expect("counts"),
            vec![
                (SuppressionReason::OptOut, 1),
                (SuppressionReason::HardBounce, 1),
                (SuppressionReason::Complaint, 0),
                (SuppressionReason::OwnerBlocked, 0),
                (SuppressionReason::RegulatoryHold, 0),
            ]
        );

        // The crash window between the index write and the row write leaves a
        // dangling index line. It must fold to nothing — never error, never
        // invent a row — because the alternative ordering hides a live
        // suppression from every review.
        register
            .append_identity_index(&scope, &SuppressionSpan::Global, "ghost@example.test")
            .expect("a dangling index line");
        let after = register.suppressed_since(&scope, t(0)).expect("report");
        assert_eq!(
            after
                .iter()
                .map(|entry| entry.identity.as_str())
                .collect::<Vec<_>>(),
            vec!["quiet@example.test", "bounced@example.test"]
        );
        assert_eq!(
            register
                .is_suppressed(&scope, "ghost@example.test", t(13))
                .expect("read"),
            None
        );
    }

    /// The fold is first-wins, defensively as well as at the write: a duplicated
    /// `Suppressed` line — a replay from an older binary, a torn concurrent
    /// append — must not move when somebody was suppressed, and must not unlift
    /// an entry that has already been reversed.
    #[test]
    fn a_duplicated_record_never_moves_or_resurrects_an_entry() {
        let (_tmp, register) = register();
        let scope = scope();
        let recorded = register
            .suppress(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                evidence("bounce-1", t(9)),
                t(10),
            )
            .expect("recorded");
        register
            .lift(
                &scope,
                "bounced@example.test",
                SuppressionReason::HardBounce,
                LiftAuthority::Operational,
                evidence("correction-1", t(11)),
                t(12),
            )
            .expect("lifted");

        // A stale duplicate of the ORIGINAL suppression lands after the lift.
        let path = register.identity_path(&scope, &SuppressionSpan::Global, "bounced@example.test");
        let mut stale = recorded.clone();
        stale.suppressed_at = t(20);
        register
            .append(&path, &SuppressionRecord::Suppressed(stale))
            .expect("a stale duplicate");

        let history = register
            .history(&scope, "bounced@example.test")
            .expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].suppressed_at, t(10), "first wins");
        assert_eq!(
            history[0].lifted_at,
            Some(t(12)),
            "a duplicate cannot unlift"
        );
        assert_eq!(
            register
                .is_suppressed(&scope, "bounced@example.test", t(21))
                .expect("read"),
            None
        );
    }

    /// The reason taxonomy is the thing that decides what a lift takes, so the
    /// split is pinned directly rather than only through behaviour.
    #[test]
    fn consent_decisions_are_the_two_that_need_an_owner_act() {
        assert!(SuppressionReason::OptOut.requires_owner_act_to_lift());
        assert!(SuppressionReason::Complaint.requires_owner_act_to_lift());
        assert!(!SuppressionReason::HardBounce.requires_owner_act_to_lift());
        assert!(!SuppressionReason::OwnerBlocked.requires_owner_act_to_lift());
        assert!(!SuppressionReason::RegulatoryHold.requires_owner_act_to_lift());
        assert_eq!(
            SuppressionReason::ALL
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>(),
            vec![
                "opt_out",
                "hard_bounce",
                "complaint",
                "owner_blocked",
                "regulatory_hold",
            ]
        );
        assert_eq!(LiftAuthority::OwnerAct.as_str(), "owner_act");
        assert_eq!(LiftAuthority::Operational.as_str(), "operational");
    }

    /// Everything reaches this register through `screen`, and nothing calls
    /// `is_suppressed` from outside this file.
    ///
    /// Pins the header sentence that had drifted: it said *"every outward send
    /// passes [`SuppressionRegister::is_suppressed`] first; a batch passes
    /// [`SuppressionRegister::screen`]"*, describing a single-recipient path
    /// that no caller has ever taken. It matters because the two entries fail
    /// differently on the case that counts — `screen` refuses an empty list and
    /// `is_suppressed` has no list to refuse — so a reader who believed the
    /// header would have looked for the empty-list guard in the wrong function.
    ///
    /// The screener list is exact rather than a floor, so a third module
    /// deciding who may be contacted has to be named in the header before this
    /// passes again.
    #[test]
    fn the_live_gate_screens_and_nobody_calls_is_suppressed_directly() {
        use crate::magician_v2::doc_wiring_scan::scan_workspace;

        const OWN_FILE: &str = "magician/src/magician_v2/suppression/mod.rs";

        // `is_suppressed` is defined here and called here, from `screen`.
        // Anywhere else would be a caller the header does not describe. The
        // needle carries its leading dot so an unrelated free function whose
        // name merely ends in `is_suppressed` is not mistaken for one.
        let direct = scan_workspace(".is_suppressed(", &[OWN_FILE]);
        assert!(
            direct.files_searched > 100,
            "only {} files were read, so this proves nothing",
            direct.files_searched
        );
        assert_eq!(
            direct.hits,
            Vec::<String>::new(),
            "something calls `is_suppressed` directly now; the header says only `screen` does"
        );

        // Match the method call rather than its receiver spelling: rustfmt may
        // wrap `register` and `.screen` onto separate lines, but that does not
        // change which module owns the screening decision.
        let screeners = scan_workspace(".screen(", &[OWN_FILE]);
        assert_eq!(
            screeners.hits,
            vec![
                // The owner-facing check. Deliberately the SAME call the gate
                // makes: a surface with its own opinion could clear somebody
                // the gate refuses.
                "magician-api/src/suppression_api.rs".to_string(),
                // The live gate the header names.
                "magician/src/magician_v2/agents/outward_gate.rs".to_string(),
                // The hygiene sweep's own test, which screens the identity it
                // has just suppressed to prove the entry actually reaches the
                // gate. A fixture, not a third screener — and asserting through
                // `screen` rather than through `history` is the point of it.
                "magician/src/magician_v2/delivery_hygiene/mod.rs".to_string(),
            ],
            "the set of modules screening recipients changed; the header names them"
        );
    }
}
