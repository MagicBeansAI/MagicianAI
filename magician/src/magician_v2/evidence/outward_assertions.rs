//! What we told whom, through which exact payload — OPC phase 2, plan phase 1.
//!
//! `docs/plans/2026-08-07-opc-outward-assertions.md`. This is the schema, the
//! indexes, and the §4 ordering with fail-closed preparation.
//!
//! **Writers, and where they are not.** `run_state::submit` records the
//! covering act for a form submission and is reachable from
//! `POST /work/runs/{id}/submit`; `data_room::disclosure_bridge` writes here
//! from the room-grant path, which records before a share link is minted;
//! `evidence::observed_statements` records the observed side and is reached
//! from `evidence::transcript_ingestion`, whose one owner route
//! (`magician-api/src/transcript_claims_api.rs`) `magician-bin` mounts — so
//! every writer named here is live. The restricted outward *adapters* — the
//! ones that would report provider delivery, bounce and complaint — are still
//! absent, which is why
//! `DeliveryState` is `unknown` for almost everything this store holds.
//!
//! Magician already records evidence (`EvidenceRecord`) and reads claims
//! (`ClaimRecord`, ephemeral by design), and Artifact V2 owns immutable output
//! revisions. What none of them records is the **disclosure** — that a claim was
//! asserted, to these people, through this exact artifact revision, on this
//! channel, at this time. Without it a corrected figure cannot find the emails
//! that stated it, and correction propagation is impossible.
//!
//! # Two records, because not every outward act makes a claim
//!
//! "Tuesday at 3 works." A calendar invitation. An unsubscribe confirmation.
//! None carries an approved factual claim, and one record requiring a claim ref
//! would force inventing a fake one for every logistical message. So the act and
//! its claims are separate, and every controlled outward act gets the first
//! whether or not it gets any of the second.
//!
//! # Two invariants enforced by WHERE a file lands, not by bookkeeping
//!
//! - *"an idempotent retry RESUMES the same record, never duplicates it"* — the
//!   act ref is derived from `(scope, idempotency_key)`, so a retry resolves to
//!   the same path and finds the existing record.
//! - *"one record per `(outward_act_ref, approved_claim_ref, audience)`"* — the
//!   assertion-use id is derived from exactly that triple.
//!
//! Bookkeeping that must be kept in step with reality eventually is not; a path
//! cannot drift from itself.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{AudienceKind, AudienceRef};
use crate::magician_v2::work_context::WorkContextKind;

/// The axis every act is filed under, whatever else is known about it.
///
/// Named because it is the one axis with no condition on it: a work axis holds
/// only the acts whose disclosure carried that kind of work, so *"how many acts
/// does this scope hold at all"* can only be asked here. That question is what
/// separates "the work axes found nothing because there is nothing" from "the
/// work axes found nothing because nothing was attributed to a work".
pub const ARTIFACT_AXIS: &str = "artifact";
/// The axis a recipient's acts and assertion uses are filed under.
///
/// **Mixed**, deliberately: [`OutwardAssertionStore::index_act`] files act refs
/// here and `index_assertion_use` files assertion-use ids, because a
/// correction has to find both. So it is the one axis a reader must not treat
/// as a list of acts.
pub const RECIPIENT_AXIS: &str = "recipient";
/// The axis an approved claim's assertion uses are filed under.
pub const CLAIM_AXIS: &str = "claim";
/// The axis a piece of evidence's assertion uses are filed under.
pub const EVIDENCE_AXIS: &str = "evidence";
/// The axis an act is filed under by the PROVIDER MESSAGE it became.
///
/// The reverse direction of every other axis here. The others answer *"what did
/// this act/claim/recipient touch"*; this one answers the only question a
/// provider event can ask, because a bounce or a complaint arrives carrying the
/// provider's own message id and nothing else — not an act ref, not a claim, not
/// a workspace. Without this index a receipt has no way back to the disclosure
/// it belongs to, and `delivery::DeliveryLedger::reconcile` — which takes an act
/// ref it cannot discover for itself — can never be called from a real event.
///
/// The axis value is `provider` and the provider's message id joined by
/// [`FIELD_SEP`], so two providers reusing one id string cannot collide onto one
/// act.
pub const PROVIDER_MESSAGE_AXIS: &str = "provider_message";

/// The axis an act performed for an **audience** is filed under — one per kind.
///
/// A `match`, never [`AudienceKind::as_str`]: a sixth audience kind must fail to
/// compile here rather than be filed under a name no sweep reads, or — worse —
/// under whichever existing kind happened to be nearest. Owning the axis names
/// here also means the audience module's wire tokens can change without
/// silently re-pointing every act ever filed.
///
/// # Why these are not the work axes, though two of the words match
///
/// An audience is named by the relationship it is **drawn from**, and that is a
/// different id space from the work an act was performed inside.
/// `CounterpartyAudiences::living_audience` resolves an [`AudienceRef`] through
/// the counterparty register, so an [`AudienceKind::Engagement`] audience is
/// named by the **counterparty**, while [`WorkContextKind::Engagement`] is named
/// by the engagement itself. Filing both under `engagement` would answer *"which
/// acts were performed inside engagement X"* with acts that merely went to a
/// counterparty whose id spells the same — the merge
/// [`AudienceRef::as_key`] exists to prevent, arriving through the index instead.
pub fn audience_axis(kind: AudienceKind) -> &'static str {
    match kind {
        AudienceKind::Engagement => "audience_engagement",
        AudienceKind::Program => "audience_program",
        AudienceKind::Account => "audience_account",
        AudienceKind::Panel => "audience_panel",
        AudienceKind::Person => "audience_person",
    }
}

/// The channel an outward act travelled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutwardChannel {
    Email,
    /// Renamed for serde because `rename_all = "snake_case"` derives
    /// `whats_app` while [`OutwardChannel::as_str`] — the token every log line
    /// and comparison uses — says `whatsapp`. Two live spellings of one channel
    /// would split reverse lookups in two. The alias keeps any record already
    /// on disk readable.
    #[serde(rename = "whatsapp", alias = "whats_app")]
    WhatsApp,
    Room,
    Form,
    Meeting,
}

impl OutwardChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::WhatsApp => "whatsapp",
            Self::Room => "room",
            Self::Form => "form",
            Self::Meeting => "meeting",
        }
    }

    /// Whether the system composes the payload before the act.
    ///
    /// A controlled channel can be persisted *before* anything leaves, so
    /// preparation can fail the send closed. An observed channel — a live
    /// meeting — cannot: the words are already spoken by the time anything is
    /// recorded, so there is no prepare step to fail closed. That difference is
    /// the mechanical reason terms conversations stay founder-attended.
    pub fn is_controlled(self) -> bool {
        !matches!(self, Self::Meeting)
    }
}

/// Where an act has got to. "Sent" is three different facts, so it is three
/// different states.
///
/// ```text
/// prepared → dispatching → provider_accepted → delivered
///                       ↘ dispatch_unknown            ↘ corrected | retracted
///                       ↘ failed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutwardActStatus {
    /// Recorded, nothing has left yet.
    Prepared,
    /// The side effect is in flight.
    Dispatching,
    /// The provider accepted it. **This is not delivery.**
    ProviderAccepted,
    /// The provider reported it reached the recipient.
    Delivered,
    /// The process died between the act and the receipt, and the adapter cannot
    /// reconcile. Neither evidence of a send nor of a non-send — a question,
    /// held open rather than guessed either way.
    DispatchUnknown,
    Failed,
    /// A successor disclosure corrects this one. The original is never edited.
    Corrected,
    Retracted,
}

impl OutwardActStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Dispatching => "dispatching",
            Self::ProviderAccepted => "provider_accepted",
            Self::Delivered => "delivered",
            Self::DispatchUnknown => "dispatch_unknown",
            Self::Failed => "failed",
            Self::Corrected => "corrected",
            Self::Retracted => "retracted",
        }
    }

    /// Whether a correction obligation could still attach to this act. A failed
    /// act told nobody anything; a delivered one did.
    pub fn is_active_disclosure(self) -> bool {
        matches!(
            self,
            Self::Dispatching
                | Self::ProviderAccepted
                | Self::Delivered
                | Self::DispatchUnknown
                | Self::Corrected
        )
    }
}

/// One controlled outward act. Always written, claim or no claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutwardActDisclosure {
    pub outward_act_ref: String,
    pub principal: String,
    pub workspace: String,
    /// Who this act was performed FOR, when the caller knew.
    ///
    /// Distinct from `intended_audience`, which is the list of addresses the
    /// act reached: this names the RELATIONSHIP those addresses were drawn
    /// from, so a later reader can ask "what have we told this counterparty"
    /// without re-deriving it from a recipient list that may since have changed.
    ///
    /// Set by [`OutwardAssertionStore::prepare_for_audience`]. `None` on an act
    /// prepared through [`OutwardAssertionStore::prepare`], which is honest:
    /// a dispatch-path act knows the work it served, not the relationship.
    ///
    /// `#[serde(default)]` because the register is append-only and every row
    /// written before this field existed must still read back. An absent value
    /// means *"nobody recorded one"*, never *"there was none"* — the two are
    /// only distinguishable by the row's `prepared_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<AudienceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    /// The engagement this act was performed **inside** — a
    /// [`WorkContextKind::Engagement`] id, and the value every reverse lookup
    /// under [`WorkContextKind::ENGAGEMENT_TOKEN`] asks with.
    ///
    /// **Not** the counterparty an [`AudienceRef`] is drawn from. Those are
    /// different id spaces — see [`audience_axis`] — and an earlier cut of the
    /// data-room bridge put a room's audience id, which the counterparty
    /// register resolves, straight into this field.
    ///
    /// Rows are never rewritten (the register is append-only, and a history
    /// edited to look clean is worse than one that says where it was wrong), so
    /// a reader auditing old rows should treat this rule as the test: an act
    /// whose `channel` is [`OutwardChannel::Room`] and whose `engagement_id` or
    /// `program_id` is set predates [`OutwardAssertionStore::prepare_for_audience`],
    /// and that id is a **counterparty** id however this field is named. The
    /// engagement axis over-answers for exactly those rows.
    ///
    /// How many exist is a question for the store and not for this comment: the
    /// room-grant path is live (`data_room::store::GrantDisclosure` records
    /// before a link is minted), so a workspace that has granted a room link
    /// has some, and one that has not has none. Nothing here counts them,
    /// because a number written into a comment is wrong the day after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engagement_id: Option<String>,
    /// **Immutable**: the artifact revision that actually went out, not a
    /// "latest" pointer. What was reviewed and what was sent cannot diverge.
    pub exact_payload_artifact_ref: String,
    /// Resolved by the runtime, never chosen by the caller.
    pub effective_sender: String,
    pub intended_audience: Vec<String>,
    pub channel: OutwardChannel,
    /// Envelopes §3. Carried, not interpreted here.
    pub consequence_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_receipt_ref: Option<String>,
    /// Which provider carried this act — `agentmail`, `kapso`, an operator's
    /// own name for a channel it handed off by hand.
    ///
    /// `None` until the send's own result names one, and `None` is **not**
    /// permission to assume a default: an act whose provider is unknown is an
    /// act nothing can reconcile, which is what
    /// [`OutwardActStatus::DispatchUnknown`] already says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The provider's own id for the message this act became — the anchor a
    /// later bounce, complaint or delivery event is matched against.
    ///
    /// Serde-defaulted because every record written before this field existed
    /// has none, and a record with none must keep reading as *"we do not know
    /// which message this was"* rather than failing to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    pub status: OutwardActStatus,
    pub prepared_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    /// True when the record was written AFTER the words left — a live meeting.
    ///
    /// It preserves its own uncertainty: being transcribed later is not the same
    /// as having been cleared beforehand, and this record must never be read as
    /// pre-authorisation because it exists.
    #[serde(default)]
    pub observed: bool,
}

/// One claim asserted by one act to one audience. Zero or more per act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutwardAssertionUse {
    pub assertion_use_id: String,
    pub outward_act_ref: String,
    pub approved_claim_ref: String,
    pub evidence_refs: Vec<String>,
    pub audience: String,
    /// Assertion uses this one replaces. Append-only: the superseded rows stay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_ref: Option<String>,
    pub recorded_at: String,
}

/// One appended state transition on an act. The act's file is the log; its
/// current state is the last line. Nothing is ever edited in place, so a
/// correction is a successor and history survives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutwardActTransition {
    pub outward_act_ref: String,
    pub status: OutwardActStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_receipt_ref: Option<String>,
    /// The provider binding this transition established, if it established one.
    ///
    /// Carried on the transition and not only folded onto the act, because the
    /// file IS the history: an act whose binding were recorded only in the fold
    /// could not say *when* it learned which message it had become, and "we
    /// knew by then" is the question every dispute about a send turns on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The dispatch attempt that drove this transition, when one did.
    ///
    /// On the TRANSITION rather than on the act, because the act is keyed by
    /// what it discloses — capability, action, exact payload — so a replay of the
    /// same send resolves to the same record, which is the property the store
    /// is built on. An attempt id in that key would break it: two attempts at
    /// one disclosure would become two disclosures, and "what did we tell them"
    /// would answer twice for a thing said once.
    ///
    /// The history is where the attempt belongs anyway. The file IS the log, so
    /// an act that was dispatched, lost its response, and was reconciled later
    /// can now say WHICH attempt did each of those things — which is the
    /// question "did it fire" actually reduces to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
    pub at: String,
}

/// What a work-axis re-index found.
///
/// Counts, not a boolean. *"Repaired the index"* over a scope where every act
/// was unattributed is the same sentence as over one where forty entries were
/// restored, and only the second is a repair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkAxisBackfill {
    pub acts_seen: usize,
    /// Entries this run ADDED, per axis. Not the number of acts filed under
    /// them — an act already indexed contributes to `already_indexed` instead.
    pub engagement_entries: usize,
    pub program_entries: usize,
    /// Entries that were already there.
    ///
    /// Reported because it is almost always the whole answer: acts are indexed
    /// at write time, so a healthy store re-indexes to nothing. *"Repaired 400"*
    /// when 400 were already correct is the sentence this count exists to
    /// prevent — and a run where it is LOW is the one worth looking at.
    pub already_indexed: usize,
    /// Acts whose record names no work at all. **Not repairable**: the work was
    /// never recorded, so there is nothing to re-index and no honest way to
    /// invent it. A non-zero count here is the answer to "why is the sweep
    /// still finding nothing", and it will not change by running this again.
    ///
    /// It is *"no work"*, not *"no filing"*. An act performed for an audience
    /// rather than inside a work — a data room's grant — is counted here and is
    /// still findable, under the audience axis
    /// [`OutwardAssertionStore::prepare_for_audience`] filed it on. This
    /// re-index cannot confirm or repair that filing, because the audience is
    /// not on the record: the axis entry written at prepare time is the only
    /// copy. So a scope of room grants reads as fully unattributed here while
    /// being fully indexed, and a reader chasing "why is nothing attributed"
    /// must check the audience axes before concluding anything was lost.
    pub unattributed: usize,
}

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutwardScope {
    pub principal: String,
    pub workspace: String,
}

impl OutwardScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What a caller must supply to prepare an act. Deliberately not the record
/// itself: `outward_act_ref`, `status` and `prepared_at` are the store's to
/// decide, so a caller cannot pre-declare an act as already accepted.
#[derive(Debug, Clone)]
pub struct PrepareOutwardAct {
    /// Stable per-act key. A retry with the same key RESUMES the same record.
    pub idempotency_key: String,
    pub program_id: Option<String>,
    pub engagement_id: Option<String>,
    pub exact_payload_artifact_ref: String,
    pub effective_sender: String,
    pub intended_audience: Vec<String>,
    pub channel: OutwardChannel,
    pub consequence_class: String,
}

/// Durable store for outward disclosures and the claims they carried.
///
/// Owned by the evidence subsystem (plan §2). Presentation Maker, Demo Maker and
/// outward answering **consume; never own** — a second authoritative copy is how
/// a claim ends up verified in one subsystem and stale in another.
#[derive(Debug, Clone)]
pub struct OutwardAssertionStore {
    workspace_layout: ArtifactV2Workspace,
    /// The dispatch attempt this handle was opened for; stamped onto every
    /// transition it appends. See [`Self::with_effect_id`].
    effect_id: Option<String>,
}

impl OutwardAssertionStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            effect_id: None,
        }
    }

    /// Name the dispatch attempt every transition this handle appends belongs
    /// to.
    ///
    /// On the handle rather than on each call, because
    /// `append_transition_with_binding` is deliberately the ONE place a
    /// transition row is built and its own comment argues against growing its
    /// parameter list — a chain of `None`s at six call sites is how the wrong
    /// one gets passed. A handle is opened per dispatch, so it can hold the
    /// attempt without any of that.
    ///
    /// Which means: a handle must not outlive the attempt it names. Every
    /// construction in the loop is per-dispatch and every other construction
    /// leaves this unset, so a stale id cannot be inherited — the failure mode
    /// would be an act whose history blames an attempt that had nothing to do
    /// with it.
    #[must_use]
    pub fn with_effect_id(mut self, effect_id: Option<String>) -> Self {
        self.effect_id = effect_id;
        self
    }

    fn root(&self, scope: &OutwardScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("outward_assertions")
    }

    /// The file one act's log lands in.
    ///
    /// # Why this does NOT refuse an underived ref
    ///
    /// A first cut made this reject anything that was not [`derive_act_ref`]'s
    /// shape, reasoning that "no record" is read downstream as evidence nothing
    /// was sent. The reasoning about the hazard was right; the placement was
    /// wrong, and the test suite said so in four places.
    ///
    /// Reading an act by an id that turns out not to BE an act is a supported
    /// question here, and callers use the `None` as their discriminator — see
    /// `recipient_compliance`'s duplicate scan, which walks an index of mixed id
    /// kinds and counts the misses in a variable called `not_an_act`. Refusing
    /// here converted that designed answer into a hard error and took the whole
    /// scan down with it.
    ///
    /// So the guard belongs where absence is read as a **safety signal** rather
    /// than as an answer — `reconcile_outward_effect`, which is the one caller
    /// that turns "no record" into a licence to re-send. It refuses there, and
    /// answers `StillUnknown` rather than `DidNotFire`. See also
    /// `effects::validate_reconcile_ref`, which refuses at parse, where such a
    /// ref first arrives off the wire.
    fn act_path(&self, scope: &OutwardScope, outward_act_ref: &str) -> PathBuf {
        self.root(scope)
            .join("acts")
            .join(format!("{outward_act_ref}.jsonl"))
    }

    fn assertion_use_path(&self, scope: &OutwardScope, assertion_use_id: &str) -> PathBuf {
        self.root(scope)
            .join("assertion_uses")
            .join(format!("{assertion_use_id}.json"))
    }

    /// Reverse index file for one axis value. The reverse lookup IS the feature,
    /// so the pointers are written when the row is, not derived by scanning
    /// later — a scan would be correct and would also be the thing nobody runs.
    fn index_path(&self, scope: &OutwardScope, axis: &str, value: &str) -> PathBuf {
        self.root(scope)
            .join("index")
            .join(axis)
            .join(format!("{}.jsonl", stable_id(value)))
    }

    // ── §4 ordering, controlled dispatch ────────────────────────────────────

    /// Step 1: record the act BEFORE anything leaves.
    ///
    /// Step 2 is the caller's: **if this returns `Err`, the send must fail
    /// closed.** An outward act that succeeded while its record failed is a
    /// disclosure nobody can find later — precisely the state this store exists
    /// to make impossible. Better to not send.
    ///
    /// Idempotent by construction: the same `idempotency_key` resolves to the
    /// same path, so a retry returns the existing record rather than a second
    /// one.
    ///
    /// # The unavoidable race, and its arbiter
    ///
    /// Two concurrent prepares of one act can both see no existing record and
    /// both append a head — append-only files offer no compare-and-append.
    /// The fold arbitrates: a duplicate head for the same act ref is skipped as
    /// the race losing honestly, so both callers converge on one record instead
    /// of the second head bricking the log as corruption.
    pub fn prepare(
        &self,
        scope: &OutwardScope,
        request: &PrepareOutwardAct,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        // `None`, and honestly so: a dispatch-path act knows the WORK it served,
        // not the relationship its recipients were drawn from. Guessing one
        // from the recipient list is how a counterparty id ended up in
        // `engagement_id` in the first place.
        self.prepare_with_audience(scope, request, None, now)
    }

    /// [`Self::prepare`], plus the audience the act was performed for.
    ///
    /// Private, and one body rather than two: the preparation rules — the
    /// controlled-channel guard, the derived ref, the idempotent resume, the
    /// index-before-row ordering and the race arbiter — are the same whether or
    /// not a relationship is known, and two copies of them would drift on the
    /// one path where a wrong answer is unrecoverable.
    fn prepare_with_audience(
        &self,
        scope: &OutwardScope,
        request: &PrepareOutwardAct,
        audience: Option<&AudienceRef>,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        if !request.channel.is_controlled() {
            anyhow::bail!(
                "channel `{}` is observed, not controlled: nothing can be prepared before the \
                 words leave. Use `record_observed_act`.",
                request.channel.as_str()
            );
        }
        let outward_act_ref = derive_act_ref(scope, &request.idempotency_key);

        // A retry resumes rather than duplicates.
        if let Some(existing) = self.load_act(scope, &outward_act_ref)? {
            return Ok(existing);
        }

        let disclosure = OutwardActDisclosure {
            outward_act_ref: outward_act_ref.clone(),
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            audience: audience.cloned(),
            program_id: request.program_id.clone(),
            engagement_id: request.engagement_id.clone(),
            exact_payload_artifact_ref: request.exact_payload_artifact_ref.clone(),
            effective_sender: request.effective_sender.clone(),
            intended_audience: request.intended_audience.clone(),
            channel: request.channel,
            consequence_class: request.consequence_class.clone(),
            effect_receipt_ref: None,
            provider: None,
            provider_message_id: None,
            status: OutwardActStatus::Prepared,
            prepared_at: now.to_string(),
            dispatched_at: None,
            settled_at: None,
            observed: false,
        };
        // Index BEFORE the row — see `record_assertion_use`'s comment on the
        // same ordering. The reverse is a crash-safety hole: a retry with the
        // same idempotency key early-returns on `load_act` finding the row
        // (above), so a crash between the two writes in the other order
        // permanently loses the index entries for a row that does exist.
        self.index_act(scope, &disclosure)?;
        self.append_json(&self.act_path(scope, &outward_act_ref), &disclosure)?;
        Ok(disclosure)
    }

    /// Prepare an act performed for an **audience**, and file it under that
    /// audience's own axis.
    ///
    /// # The audience is carried, never converted into an id it is not
    ///
    /// A caller that holds an [`AudienceRef`] holds a kind and an id, and the
    /// id belongs to the kind's own space — a data room's audience id is the
    /// **counterparty** the room was opened for, because
    /// `CounterpartyAudiences::living_audience` is what resolves it. Writing
    /// that id into [`OutwardActDisclosure::engagement_id`] is a false statement
    /// at write time: that field names the engagement an act was performed
    /// *inside*, which is what [`WorkContextKind::Engagement`] carries and what
    /// every reverse lookup under [`WorkContextKind::ENGAGEMENT_TOKEN`] asks
    /// for. So the audience is filed under [`audience_axis`] instead, derived
    /// from the KIND — and an `Account`, `Panel` or `Person` room, which has no
    /// work field it could ever honestly fill, becomes findable rather than
    /// being mis-filed as an engagement or dropped.
    ///
    /// `request`'s own `program_id` / `engagement_id` are left exactly as the
    /// caller set them and are still filed under the work axes by
    /// [`Self::index_act`]. The two are orthogonal: an act can be performed
    /// inside a programme *and* shown to a panel, and neither filing is
    /// evidence for the other.
    ///
    /// # Ordering, and why it is the reverse of [`Self::prepare`]'s own
    ///
    /// `prepare` indexes before it writes the row because a retry early-returns
    /// on finding the row, so an index written after it would be lost for good
    /// by a crash in between. This index is written **after** `prepare` returns
    /// and is reached on the early-return path too, so the same crash is
    /// repaired by the next call rather than made permanent — and no index entry
    /// is left pointing at an act that a refused `prepare` never wrote.
    ///
    /// Checked before appending, for the reason
    /// [`Self::reindex_work_axes`] gives: a re-swept room calls this again for
    /// every pair it already recorded, and appending unconditionally would grow
    /// the file on every sweep. The reader de-duplicates, so the answers would
    /// stay right and only the reads would get slower — forever.
    pub fn prepare_for_audience(
        &self,
        scope: &OutwardScope,
        request: &PrepareOutwardAct,
        audience: &AudienceRef,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        if !audience.is_named() {
            anyhow::bail!(
                "audience of kind `{}` has a blank id: every unnamed audience would share one \
                 index file, so a reverse lookup would answer with acts performed for somebody \
                 else's relationship",
                audience.kind.as_str()
            );
        }
        let mut filed = self.audience_index_set(scope, audience)?;
        self.prepare_for_audience_filed(scope, request, audience, &mut filed, now)
    }

    /// Every act ref already filed under this audience, read once.
    ///
    /// Pair with [`Self::prepare_for_audience_filed`] when preparing more than
    /// one act for the same audience: read this ONCE outside the loop, and the
    /// per-act containment check becomes a set lookup instead of a file read.
    pub fn audience_index_set(
        &self,
        scope: &OutwardScope,
        audience: &AudienceRef,
    ) -> Result<BTreeSet<String>> {
        Ok(self
            .index_entries(scope, audience_axis(audience.kind), &audience.id)?
            .into_iter()
            .collect())
    }

    /// [`Self::prepare_for_audience`], against an index already in hand.
    ///
    /// The single-act form reads the audience's whole index file to answer one
    /// containment question. That is fine once and quadratic in a loop — and
    /// the file being read is the one the loop is growing, so the cost climbs
    /// as it runs. `record_room_disclosures` prepares one act per
    /// `(present document × live holder)` pair, which for a counterparty with
    /// a few rooms of documents shown to a handful of holders is hundreds of
    /// calls against a file with hundreds of lines.
    ///
    /// `filed` is READ AND UPDATED, so an act appended by one iteration is
    /// visible to the next without re-reading. That is what keeps the batch
    /// form from re-appending a ref it just wrote.
    pub fn prepare_for_audience_filed(
        &self,
        scope: &OutwardScope,
        request: &PrepareOutwardAct,
        audience: &AudienceRef,
        filed: &mut BTreeSet<String>,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        if !audience.is_named() {
            anyhow::bail!(
                "audience of kind `{}` has a blank id: every unnamed audience would share one \
                 index file, so a reverse lookup would answer with acts performed for somebody \
                 else's relationship",
                audience.kind.as_str()
            );
        }
        let disclosure = self.prepare_with_audience(scope, request, Some(audience), now)?;
        if filed.insert(disclosure.outward_act_ref.clone()) {
            self.append_index(
                scope,
                audience_axis(audience.kind),
                &audience.id,
                &disclosure.outward_act_ref,
            )?;
        }
        Ok(disclosure)
    }

    /// Step 3: the side effect is in flight.
    pub fn mark_dispatching(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        now: &str,
    ) -> Result<()> {
        self.append_transition(
            scope,
            outward_act_ref,
            OutwardActStatus::Dispatching,
            None,
            None,
            now,
        )
    }

    /// Step 4: the provider accepted it.
    ///
    /// Acceptance is not delivery, and the state name says so. Delivery, bounce
    /// and complaint arrive later through [`record_delivered`](Self::record_delivered)
    /// and [`mark_failed`](Self::mark_failed).
    pub fn record_provider_receipt(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        effect_receipt_ref: &str,
        now: &str,
    ) -> Result<()> {
        self.append_transition(
            scope,
            outward_act_ref,
            OutwardActStatus::ProviderAccepted,
            Some(effect_receipt_ref.to_string()),
            None,
            now,
        )
    }

    pub fn record_delivered(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        now: &str,
    ) -> Result<()> {
        self.append_transition(
            scope,
            outward_act_ref,
            OutwardActStatus::Delivered,
            None,
            None,
            now,
        )
    }

    pub fn mark_failed(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        detail: &str,
        now: &str,
    ) -> Result<()> {
        self.append_transition(
            scope,
            outward_act_ref,
            OutwardActStatus::Failed,
            None,
            Some(detail.to_string()),
            now,
        )
    }

    /// Step 6: the process died between the act and the receipt, and this
    /// adapter cannot reconcile.
    ///
    /// Held open deliberately. A `prepared` record with no receipt is not
    /// evidence of a send *or* of a non-send; resolving it either way would be
    /// inventing an answer, and an idempotency key we merely stored proves
    /// nothing about what the provider did with it.
    pub fn mark_dispatch_unknown(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        detail: &str,
        now: &str,
    ) -> Result<()> {
        self.append_transition(
            scope,
            outward_act_ref,
            OutwardActStatus::DispatchUnknown,
            None,
            Some(detail.to_string()),
            now,
        )
    }

    // ── Observed channels ───────────────────────────────────────────────────

    /// Record something already said on an observed channel.
    ///
    /// There is no prepare step, because the words are gone. The record is
    /// stamped `observed: true` and enters at `ProviderAccepted` — it happened —
    /// but it must never be read as pre-authorised. Being transcribed after the
    /// fact is not the same as having been cleared beforehand.
    pub fn record_observed_act(
        &self,
        scope: &OutwardScope,
        request: &PrepareOutwardAct,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        if request.channel.is_controlled() {
            anyhow::bail!(
                "channel `{}` is controlled: it must be prepared before the act, not observed \
                 after it",
                request.channel.as_str()
            );
        }
        let outward_act_ref = derive_act_ref(scope, &request.idempotency_key);
        if let Some(existing) = self.load_act(scope, &outward_act_ref)? {
            return Ok(existing);
        }
        let disclosure = OutwardActDisclosure {
            outward_act_ref: outward_act_ref.clone(),
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            // `None`. An observed act is recorded after the words already
            // left, and this entry point is handed a `PrepareOutwardAct` and
            // nothing else — it has no relationship to name. Deriving one from
            // the recipient list would be the same guess this field exists to
            // stop, so it says nothing rather than something plausible.
            audience: None,
            program_id: request.program_id.clone(),
            engagement_id: request.engagement_id.clone(),
            exact_payload_artifact_ref: request.exact_payload_artifact_ref.clone(),
            effective_sender: request.effective_sender.clone(),
            intended_audience: request.intended_audience.clone(),
            channel: request.channel,
            consequence_class: request.consequence_class.clone(),
            effect_receipt_ref: None,
            provider: None,
            provider_message_id: None,
            status: OutwardActStatus::ProviderAccepted,
            prepared_at: now.to_string(),
            dispatched_at: Some(now.to_string()),
            settled_at: None,
            observed: true,
        };
        // Index BEFORE the row — see `record_assertion_use`'s comment and
        // `prepare`'s matching fix on the same ordering.
        self.index_act(scope, &disclosure)?;
        self.append_json(&self.act_path(scope, &outward_act_ref), &disclosure)?;
        Ok(disclosure)
    }

    // ── Assertion uses ──────────────────────────────────────────────────────

    /// Record that an act asserted a claim to an audience.
    ///
    /// One row per `(act, claim, audience)`, enforced by deriving the id from
    /// exactly those three: a repeat write resolves to the same file.
    pub fn record_assertion_use(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        approved_claim_ref: &str,
        audience: &str,
        evidence_refs: &[String],
        supersedes: &[String],
        now: &str,
    ) -> Result<OutwardAssertionUse> {
        if self.load_act(scope, outward_act_ref)?.is_none() {
            anyhow::bail!(
                "no outward act `{outward_act_ref}`: a claim cannot be asserted by an act that \
                 was never recorded"
            );
        }
        let assertion_use_id =
            derive_assertion_use_id(outward_act_ref, approved_claim_ref, audience);
        let path = self.assertion_use_path(scope, &assertion_use_id);
        if let Some(existing) = self.read_json_if_present::<OutwardAssertionUse>(&path)? {
            return Ok(existing);
        }
        let use_row = OutwardAssertionUse {
            assertion_use_id: assertion_use_id.clone(),
            outward_act_ref: outward_act_ref.to_string(),
            approved_claim_ref: approved_claim_ref.to_string(),
            evidence_refs: evidence_refs.to_vec(),
            audience: audience.to_string(),
            supersedes: supersedes.to_vec(),
            correction_ref: None,
            recorded_at: now.to_string(),
        };
        // Index BEFORE the row, so that "the row exists" implies "the index
        // exists". The other order leaves a window where a crash writes a row
        // that the reverse lookup can never find — and the retry early-returns
        // on the row it can see, so the pointer is lost permanently. The reverse
        // lookup IS the feature here, so a disclosure nobody can find is the one
        // outcome worth ordering around.
        //
        // The cost of this order is a dangling index entry if the row write then
        // fails; `index_entries` deduplicates and callers load by id, so a
        // pointer to a row that never appeared is inert.
        self.index_assertion_use(scope, &use_row)?;
        self.write_json(&path, &use_row)?;
        Ok(use_row)
    }

    // ── Reads ───────────────────────────────────────────────────────────────

    /// The act's current state: the last line of its append-only log.
    pub fn load_act(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
    ) -> Result<Option<OutwardActDisclosure>> {
        let path = self.act_path(scope, outward_act_ref);
        let Some(raw) = self.read_to_string_if_present(&path)? else {
            return Ok(None);
        };
        let mut current: Option<OutwardActDisclosure> = None;
        // A torn append is UNTERMINATED — a terminated unparseable line is
        // corruption, not a tear. Same discriminator as `magician_v2::jsonl`.
        // Anchored to the last line OF THE FILE, not the last line that survives
        // the blank filter below. A file ending in a whitespace-only unterminated
        // fragment would otherwise hand the tear exemption to the complete,
        // newline-terminated record above it — silently dropping real corruption
        // as though it were a torn write. If the final raw line is blank, nothing
        // parseable was torn and no surviving line has earned the exemption.
        let tail_may_be_torn = !raw.is_empty()
            && !raw.ends_with('\n')
            && raw
                .lines()
                .next_back()
                .is_some_and(|line| !line.trim().is_empty());
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let last = lines.len().saturating_sub(1);
        for (position, line) in lines.iter().enumerate() {
            if current.is_none() {
                current = match serde_json::from_str(line) {
                    Ok(head) => Some(head),
                    // A torn HEAD is a prepare that never completed: no act.
                    Err(_) if position == last && tail_may_be_torn => return Ok(None),
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("parsing act head in {}", path.display()))
                    },
                };
                continue;
            }
            let transition: OutwardActTransition = match serde_json::from_str(line) {
                Ok(transition) => transition,
                // A torn FINAL transition is an append that never completed:
                // the state change never happened.
                Err(_) if position == last && tail_may_be_torn => break,
                Err(head_error) => {
                    // The prepare() race's arbiter: two concurrent prepares of
                    // one act can both pass the load_act check and both append
                    // the head. The second head is not corruption — it is the
                    // race losing honestly — so an identical duplicate head is
                    // skipped. Anything else is real corruption and refuses.
                    if let Ok(duplicate) = serde_json::from_str::<OutwardActDisclosure>(line) {
                        if current
                            .as_ref()
                            .is_some_and(|held| held.outward_act_ref == duplicate.outward_act_ref)
                        {
                            continue;
                        }
                    }
                    return Err(head_error)
                        .with_context(|| format!("parsing act transition in {}", path.display()));
                },
            };
            if let Some(act) = current.as_mut() {
                act.status = transition.status;
                if transition.effect_receipt_ref.is_some() {
                    act.effect_receipt_ref = transition.effect_receipt_ref.clone();
                }
                // A binding is only ever SET by a transition, never cleared by
                // one. A later transition that names no provider — a delivery,
                // a correction — must not erase which message the act became,
                // or a receipt arriving after it would find an act that had
                // forgotten its own id.
                if transition.provider.is_some() {
                    act.provider = transition.provider.clone();
                }
                if transition.provider_message_id.is_some() {
                    act.provider_message_id = transition.provider_message_id.clone();
                }
                match transition.status {
                    OutwardActStatus::Dispatching => {
                        act.dispatched_at = Some(transition.at.clone())
                    },
                    OutwardActStatus::Delivered
                    | OutwardActStatus::Failed
                    | OutwardActStatus::Retracted => act.settled_at = Some(transition.at.clone()),
                    _ => {},
                }
            }
        }
        Ok(current)
    }

    /// The act's full history, oldest first. Append-only means this is complete.
    pub fn load_act_history(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
    ) -> Result<Vec<OutwardActStatus>> {
        let path = self.act_path(scope, outward_act_ref);
        let Some(raw) = self.read_to_string_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // Anchored to the last line OF THE FILE, not the last line that survives
        // the blank filter below. A file ending in a whitespace-only unterminated
        // fragment would otherwise hand the tear exemption to the complete,
        // newline-terminated record above it — silently dropping real corruption
        // as though it were a torn write. If the final raw line is blank, nothing
        // parseable was torn and no surviving line has earned the exemption.
        let tail_may_be_torn = !raw.is_empty()
            && !raw.ends_with('\n')
            && raw
                .lines()
                .next_back()
                .is_some_and(|line| !line.trim().is_empty());
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let last = lines.len().saturating_sub(1);
        for (position, line) in lines.iter().enumerate() {
            if position == 0 {
                let head: OutwardActDisclosure = match serde_json::from_str(line) {
                    Ok(head) => head,
                    Err(_) if position == last && tail_may_be_torn => break,
                    Err(error) => return Err(error.into()),
                };
                out.push(head.status);
            } else {
                let transition: OutwardActTransition = match serde_json::from_str(line) {
                    Ok(transition) => transition,
                    Err(_) if position == last && tail_may_be_torn => break,
                    Err(error) => {
                        // A racing duplicate head is the prepare() race losing
                        // honestly, not history — skip it (see `load_act`).
                        if serde_json::from_str::<OutwardActDisclosure>(line).is_ok() {
                            continue;
                        }
                        return Err(error.into());
                    },
                };
                out.push(transition.status);
            }
        }
        Ok(out)
    }

    /// Which dispatch attempts drove this act, in the order they did.
    ///
    /// The reader for the `effect_id` on `OutwardActTransition`. Written since
    /// the effect-identity work and, until now, never read — a field recorded
    /// for an answer nobody could ask for. §3 of the turn-boundary contract is
    /// what asks: *"work done by a holder that never reported is discarded, not
    /// replayed"*, which requires knowing whether a given attempt is already in
    /// this act's history.
    ///
    /// Transitions with no attempt recorded are skipped rather than represented,
    /// because "some attempt we cannot name" answers nothing. Tolerates the same
    /// torn tail and racing-duplicate-head as `load_act_history`, for the same
    /// reasons.
    pub fn act_attempts(&self, scope: &OutwardScope, outward_act_ref: &str) -> Result<Vec<String>> {
        let path = self.act_path(scope, outward_act_ref);
        let Some(raw) = self.read_to_string_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // Anchored to the last line OF THE FILE, not the last line that survives
        // the blank filter below. A file ending in a whitespace-only unterminated
        // fragment would otherwise hand the tear exemption to the complete,
        // newline-terminated record above it — silently dropping real corruption
        // as though it were a torn write. If the final raw line is blank, nothing
        // parseable was torn and no surviving line has earned the exemption.
        let tail_may_be_torn = !raw.is_empty()
            && !raw.ends_with('\n')
            && raw
                .lines()
                .next_back()
                .is_some_and(|line| !line.trim().is_empty());
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let last = lines.len().saturating_sub(1);
        for (position, line) in lines.iter().enumerate() {
            // Line zero is the disclosure, which carries no attempt: the act is
            // keyed by what it discloses, so its creation names no one.
            if position == 0 {
                continue;
            }
            match serde_json::from_str::<OutwardActTransition>(line) {
                Ok(transition) => {
                    if let Some(effect_id) = transition.effect_id {
                        out.push(effect_id);
                    }
                },
                Err(_) if position == last && tail_may_be_torn => break,
                Err(error) => {
                    if serde_json::from_str::<OutwardActDisclosure>(line).is_ok() {
                        continue;
                    }
                    return Err(error.into());
                },
            }
        }
        Ok(out)
    }

    pub fn load_assertion_use(
        &self,
        scope: &OutwardScope,
        assertion_use_id: &str,
    ) -> Result<Option<OutwardAssertionUse>> {
        self.read_json_if_present(&self.assertion_use_path(scope, assertion_use_id))
    }

    /// Every assertion-use id recorded against an index axis value.
    ///
    /// The axes are those the plan names: `claim`, `evidence`, `artifact`,
    /// `engagement`, `recipient`. Reading returns ids rather than rows so a
    /// caller decides what to load — the reverse-lookup QUERY and the
    /// obligations it raises are plan phase 4, not this one.
    pub fn index_entries(
        &self,
        scope: &OutwardScope,
        axis: &str,
        value: &str,
    ) -> Result<Vec<String>> {
        let path = self.index_path(scope, axis, value);
        let Some(raw) = self.read_to_string_if_present(&path)? else {
            return Ok(Vec::new());
        };
        // `Vec::dedup` removes only CONSECUTIVE duplicates, so an id appearing
        // as A, B, A survived it. Index files are append-only and interleaved
        // across assertion uses, so that shape is the normal one — and a
        // duplicate here means the same disclosure listed twice in a reverse
        // lookup, or an obligation raised against it twice.
        let mut seen = std::collections::HashSet::new();
        let out: Vec<String> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| seen.insert(line.to_string()))
            .map(str::to_string)
            .collect();
        Ok(out)
    }

    /// Every act ref filed under **any value** of one axis.
    ///
    /// The index file names are `blake3(value)`, so the values themselves
    /// cannot be recovered from the directory — but the entries can, and the
    /// entries are what a sweep needs. This is therefore the only way to ask
    /// *"which acts in this scope were performed inside some engagement"*
    /// without already holding a roster of engagement ids, and a roster is
    /// exactly the thing that makes work nobody remembered to list invisible to
    /// the sweep whose job is remembering.
    ///
    /// # Fail closed twice
    ///
    /// - A **missing** axis directory answers with an empty list, because a
    ///   scope that has filed nothing under an axis has no directory. Every
    ///   other listing failure propagates: an unlistable directory read as
    ///   empty would answer *"this scope performed no acts"* from a permissions
    ///   fault, and a silence sweep would then report a clean pass having
    ///   considered nothing.
    /// - [`RECIPIENT_AXIS`] is **refused**, not served. That axis holds
    ///   assertion-use ids as well as act refs, and handing a caller the mixed
    ///   list as acts would have it load ids that resolve to no act and report
    ///   them as acts the store does not hold.
    ///
    /// Order is by index file name and then by position within the file, so two
    /// listings over an unchanged scope agree.
    pub fn act_refs_under_axis(&self, scope: &OutwardScope, axis: &str) -> Result<Vec<String>> {
        if axis == RECIPIENT_AXIS {
            anyhow::bail!(
                "`{RECIPIENT_AXIS}` holds assertion-use ids as well as act refs, so it cannot be \
                 enumerated as acts: every id in it that is not an act would be reported as an \
                 act the store does not hold"
            );
        }
        // The axis is a directory name here, not a hashed file name — this
        // LISTS a directory where `index_entries` reads one `blake3(value)`
        // file. A traversing or empty axis would therefore point the listing at
        // a directory outside the index and report whatever it found as this
        // scope's outward acts.
        if axis.is_empty() || axis.contains('/') || axis.contains('\\') || axis.contains('.') {
            anyhow::bail!(
                "`{axis}` is not an index axis: an axis names one directory below the scope's \
                 index, and a value that can traverse would list somebody else's directory and \
                 report it as this scope's acts"
            );
        }
        let dir = self.root(scope).join("index").join(axis);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for path in crate::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &dir)? {
            let Some(raw) = self.read_to_string_if_present(&path)? else {
                // Listed a moment ago and gone now: a concurrent write cannot
                // remove an index file, so this is a race with nothing, and
                // treating it as empty is the only reading available.
                continue;
            };
            for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
                if seen.insert(line.to_string()) {
                    out.push(line.to_string());
                }
            }
        }
        Ok(out)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn append_transition(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        status: OutwardActStatus,
        effect_receipt_ref: Option<String>,
        detail: Option<String>,
        now: &str,
    ) -> Result<()> {
        self.append_transition_with_binding(
            scope,
            outward_act_ref,
            status,
            effect_receipt_ref,
            detail,
            None,
            now,
        )
    }

    /// The one place a transition row is built, binding or no binding.
    ///
    /// Split from [`Self::append_transition`] rather than adding two parameters
    /// to it: every other transition in this file names no provider, and a pair
    /// of `None, None` at six call sites is an invitation to pass the wrong one.
    #[allow(clippy::too_many_arguments)]
    fn append_transition_with_binding(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        status: OutwardActStatus,
        effect_receipt_ref: Option<String>,
        detail: Option<String>,
        binding: Option<(&str, &str)>,
        now: &str,
    ) -> Result<()> {
        if self.load_act(scope, outward_act_ref)?.is_none() {
            anyhow::bail!(
                "no outward act `{outward_act_ref}`: a transition cannot be recorded for an act \
                 that was never prepared"
            );
        }
        let transition = OutwardActTransition {
            outward_act_ref: outward_act_ref.to_string(),
            status,
            effect_receipt_ref,
            provider: binding.map(|(provider, _)| provider.to_string()),
            provider_message_id: binding.map(|(_, message_id)| message_id.to_string()),
            detail,
            effect_id: self.effect_id.clone(),
            at: now.to_string(),
        };
        self.append_json(&self.act_path(scope, outward_act_ref), &transition)
    }

    /// File one act under every axis it can be found by.
    ///
    /// # One axis per kind of work, and the kind decides the name
    ///
    /// The work axes are named by [`WorkContextKind`]'s own wire tokens rather
    /// than by literals, because those tokens are what a reverse lookup asks
    /// under. This used to index `engagement_id` and nothing else, and the cost
    /// was not a missing convenience: an act performed inside a **programme**
    /// was filed under artifact and recipient only, so a sweep enumerating a
    /// programme's outward acts found none — and an index nobody wrote to reads
    /// exactly like a programme that never said anything. A silence sweep over
    /// that answer would have reported a clean pass having considered nothing.
    ///
    /// A kind of work with no field on the disclosure is still unfindable by
    /// work; see [`Self::act_refs_under_axis`] for what that costs.
    ///
    /// The **audience** axes are not written here, because the audience is not
    /// on the record: [`Self::prepare_for_audience`] files them from the
    /// [`AudienceRef`] its caller holds. That is why this is the honest place to
    /// say what an audience is not — an act performed for a counterparty must
    /// never acquire an entry under [`WorkContextKind::ENGAGEMENT_TOKEN`] by
    /// having its audience id copied into `engagement_id` first.
    fn index_act(&self, scope: &OutwardScope, act: &OutwardActDisclosure) -> Result<()> {
        self.append_index(
            scope,
            ARTIFACT_AXIS,
            &act.exact_payload_artifact_ref,
            &act.outward_act_ref,
        )?;
        if let Some(engagement_id) = act.engagement_id.as_deref() {
            self.append_index(
                scope,
                WorkContextKind::ENGAGEMENT_TOKEN,
                engagement_id,
                &act.outward_act_ref,
            )?;
        }
        if let Some(program_id) = act.program_id.as_deref() {
            self.append_index(
                scope,
                WorkContextKind::PROGRAM_TOKEN,
                program_id,
                &act.outward_act_ref,
            )?;
        }
        for recipient in &act.intended_audience {
            self.append_index(scope, RECIPIENT_AXIS, recipient, &act.outward_act_ref)?;
        }
        Ok(())
    }

    /// Re-file every act in this scope under the work axes its own record names.
    ///
    /// # What this can and cannot repair
    ///
    /// It **re-indexes**; it does not reconstruct. An act whose record carries
    /// an `engagement_id` or a `program_id` and has no index entry for it gets
    /// one. An act whose record carries neither is counted as `unattributed`
    /// and left alone, because the work it belonged to was never written down
    /// and there is nowhere to read it from. Guessing — from the recipient, the
    /// nearest engagement, the time it was sent — would file real acts under a
    /// relationship nobody chose, and `RecipientInEngagement` would then read
    /// them as evidence about a counterparty.
    ///
    /// That distinction is the whole answer to *"can we backfill the history?"*
    /// and it is worth being blunt: for every act this runtime dispatched
    /// before 2026-08-21 the answer is **no**, because the dispatch path passed
    /// `None` for both fields. The count comes back so the caller learns that
    /// rather than reading a successful run as a repaired index.
    ///
    /// # It does not see the audience axes at all
    ///
    /// This walks the **work** axes, and an act performed for an audience
    /// carries no work. Such an act is counted `unattributed` — see that field
    /// for the difference between *"no work"* and *"not filed"* — and its
    /// audience filing is neither checked nor repairable here, because the
    /// record does not carry the audience: the entry
    /// [`Self::prepare_for_audience`] wrote is the only copy. Reading a high
    /// `unattributed` over a scope of data-room grants as *"nothing was
    /// indexed"* is therefore wrong in the safest-sounding direction, which is
    /// why it is written down rather than left to be inferred.
    ///
    /// # Idempotent, and it no longer pays a line to be
    ///
    /// A second run over an unchanged scope appends **nothing**: every entry it
    /// would write is already held, and each is counted under
    /// [`WorkAxisBackfill::already_indexed`] instead of being written again.
    /// The index reader de-duplicates, so an unconditional append never put the
    /// answers at risk — it put the file at risk, and an index that grows on
    /// every repair makes the reads it exists to speed up slower each time
    /// somebody runs one. Re-run it freely: a repair somebody is afraid to
    /// re-run is a repair that gets run once, halfway.
    pub fn reindex_work_axes(&self, scope: &OutwardScope) -> Result<WorkAxisBackfill> {
        let dir = self.root(scope).join("acts");
        let mut report = WorkAxisBackfill::default();
        // What each (axis, value) index already holds, read ONCE and then kept
        // in step as entries are appended. The membership check below is what
        // keeps a re-run from doubling the index; asking the file that question
        // once per act would re-read a file that grows with every act filed
        // under the same work, so the repair would cost time quadratic in the
        // size of the index it exists to keep fast. Bounded by the acts this
        // loop already reads one by one, so it adds no new scale.
        let mut filed: std::collections::HashMap<
            (&'static str, String),
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for path in crate::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &dir)? {
            let Some(act_ref) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            // A listing failure propagates; an act listed a moment ago and gone
            // now does not. Reading the fold rather than the first line so a
            // record whose work was set by a later transition is seen as it
            // stands.
            let Some(act) = self.load_act(scope, act_ref)? else {
                continue;
            };
            report.acts_seen += 1;
            let mut attributed = false;
            for (axis, value) in [
                (
                    WorkContextKind::ENGAGEMENT_TOKEN,
                    act.engagement_id.as_deref(),
                ),
                (WorkContextKind::PROGRAM_TOKEN, act.program_id.as_deref()),
            ] {
                let Some(value) = value else {
                    continue;
                };
                attributed = true;
                // Checked before appending, which is the difference between a
                // repair and a bloat. Acts are indexed at WRITE time, so on a
                // healthy store almost every entry is already there — appending
                // unconditionally would double the index on the first run and
                // again on every re-run. The reader de-duplicates so the answers
                // would stay correct; the file would just grow, and the reads
                // this index exists to make fast would get slower each time
                // somebody ran the repair.
                let held = match filed.entry((axis, value.to_string())) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                        self.index_entries(scope, axis, value)?
                            .into_iter()
                            .collect(),
                    ),
                };
                // `insert` answers *"was it new"*, so the same act named twice
                // by this run — or already named by a previous one — is counted
                // rather than appended a second time.
                if !held.insert(act.outward_act_ref.clone()) {
                    report.already_indexed += 1;
                    continue;
                }
                self.append_index(scope, axis, value, &act.outward_act_ref)?;
                if axis == WorkContextKind::ENGAGEMENT_TOKEN {
                    report.engagement_entries += 1;
                } else {
                    report.program_entries += 1;
                }
            }
            if !attributed {
                report.unattributed += 1;
            }
        }
        Ok(report)
    }

    fn index_assertion_use(&self, scope: &OutwardScope, row: &OutwardAssertionUse) -> Result<()> {
        self.append_index(
            scope,
            CLAIM_AXIS,
            &row.approved_claim_ref,
            &row.assertion_use_id,
        )?;
        for evidence_ref in &row.evidence_refs {
            self.append_index(scope, EVIDENCE_AXIS, evidence_ref, &row.assertion_use_id)?;
        }
        self.append_index(scope, RECIPIENT_AXIS, &row.audience, &row.assertion_use_id)
    }

    fn append_index(
        &self,
        scope: &OutwardScope,
        axis: &str,
        value: &str,
        entry_id: &str,
    ) -> Result<()> {
        let path = self.index_path(scope, axis, value);
        let mut line = entry_id.as_bytes().to_vec();
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending index {}", path.display()))?;
        Ok(())
    }

    fn append_json<T: Serialize>(&self, path: &PathBuf, value: &T) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn write_json<T: Serialize>(&self, path: &PathBuf, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.workspace_layout
            .write_atomic_path_sync(path, &bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    fn read_to_string_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as absence; an unreadable log
        // treated as empty fails open (see `magician_v2::jsonl`).
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }

    fn read_json_if_present<T: for<'de> Deserialize<'de>>(
        &self,
        path: &PathBuf,
    ) -> Result<Option<T>> {
        let Some(raw) = self.read_to_string_if_present(path)? else {
            return Ok(None);
        };
        Ok(Some(
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?,
        ))
    }
}

/// One disclosure affected by a claim change — plan phase 4.
///
/// Carries the act's status at lookup time, because what to do about a
/// disclosure depends on whether it reached anyone: a `failed` act told nobody,
/// a `delivered` one did, and a `dispatch_unknown` one might have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedDisclosure {
    pub outward_act_ref: String,
    pub assertion_use_id: String,
    pub approved_claim_ref: String,
    pub audience: String,
    pub status: OutwardActStatus,
    /// The exact payload that carried the claim. What has to be corrected is
    /// this revision, not whatever the artifact says now.
    pub exact_payload_artifact_ref: String,
}

/// What one audience's index answered, and what it could not.
///
/// Counts and not a bare list, for the reason [`WorkAxisBackfill`] carries
/// them: *"we have told this counterparty nothing"* and *"we have told it two
/// hundred things and you asked for fifty"* are different facts, and a caller
/// holding only [`Self::acts`] cannot tell them apart.
///
/// Two identities hold on every value this store returns, and a caller may
/// rely on them:
///
/// - `examined == acts.len() + unresolved` — every entry this read opened
///   either became an act or is counted as one it could not.
/// - `acts.len() == confirmed_by_record + audience_not_on_record +
///   recorded_for_another_audience` — every act returned is in exactly one of
///   the three, so the partition sums rather than overlapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudienceActs {
    /// The relationship that was asked about, echoed so a response read on its
    /// own says which audience it is the answer for.
    pub audience: AudienceRef,
    /// Distinct act refs the audience axis holds — the denominator.
    ///
    /// Counted before the cap, so it is the whole filing count even when
    /// [`Self::acts`] is shorter than it. It counts **filings, not acts**: an
    /// entry that resolves to nothing is counted here and again in
    /// [`Self::unresolved`], so this is an upper bound on the acts this
    /// audience holds and equals them only when `unresolved` is zero.
    pub indexed: usize,
    /// How many entries were still ahead of the cursor before this page was
    /// capped. Equal to [`Self::indexed`] on an uncursored read.
    ///
    /// Reported beside `indexed` so a caller paging through can tell "I am
    /// nearly done" from "I have barely started", which one number alone
    /// cannot say.
    pub remaining: usize,
    /// The act ref to pass as `after` for the next page, or `None` at the end.
    ///
    /// `None` is the ONLY end-of-relationship signal. A caller must not infer
    /// the end from a short [`Self::acts`]: the cap counts index entries
    /// OPENED, and an entry that resolved to nothing is opened and counted in
    /// [`Self::unresolved`] without adding an act — so a fully capped page can
    /// still come back with fewer acts than `limit` and more behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
    /// Index entries this read **opened**: `min(remaining, limit)`.
    ///
    /// Measured from [`Self::remaining`] and NOT from [`Self::indexed`],
    /// because the cursor filter runs before the cap: on any page but the
    /// first, the entries earlier pages already served are in `indexed` and
    /// were not opened again here.
    ///
    /// Attempts, not successes. The entries that resolved to nothing are in
    /// here too — that is what makes `examined == acts.len() + unresolved`
    /// hold, and reading this as a count of records found would leave
    /// [`Self::unresolved`] looking like it was double-counted.
    pub examined: usize,
    /// The cap, when one was hit: `Some(limit)` exactly when
    /// [`Self::remaining`] `> limit`.
    ///
    /// **Against `remaining`, not [`Self::indexed`].** It says *"this PAGE was
    /// cut short"*, so on the last page of a paged walk it is `None` even
    /// though `indexed` is far above `limit` — everything left fitted. Reading
    /// it as *"the relationship is bigger than one page"* would have a caller
    /// that paged to the end conclude it had been truncated.
    ///
    /// Reported rather than applied silently. A truncated list read as the
    /// whole list is a correction that stops halfway and reports itself
    /// complete, which is worse than one that refuses to start.
    pub capped_at: Option<usize>,
    /// The acts themselves, in act-ref order.
    pub acts: Vec<OutwardActDisclosure>,
    /// Entries naming an act this store could not resolve to a record.
    ///
    /// Counted, never dropped. `resolve_affected` skips a vanished row because
    /// it is chasing a claim and one missing row must not hide the other
    /// disclosures that ARE affected; here the entry is the only surviving
    /// statement that we told this audience anything at all, so an entry that
    /// resolves to nothing is itself the loss and has to be visible.
    ///
    /// [`OutwardAssertionStore::load_act`] answers `None` for an absent file
    /// and for a torn head — a prepare that never completed — and propagates
    /// every other read fault, so this counts *"nothing to read"* and never
    /// *"could not read"*.
    pub unresolved: usize,
    /// Acts whose own record names this same audience. The unambiguous answer.
    pub confirmed_by_record: usize,
    /// Acts filed here whose record carries no audience at all.
    ///
    /// **Not *"performed for nobody"*.** The index entry is the evidence; the
    /// record's silence is the absence of a second copy. Two ways to get here,
    /// both real:
    ///
    /// - the row predates [`OutwardActDisclosure::audience`], which arrived
    ///   with [`OutwardAssertionStore::prepare_for_audience`];
    /// - the row was written by a bare [`OutwardAssertionStore::prepare`] and
    ///   the audience filing was added afterwards by the resume path, which
    ///   returns the existing record untouched — the append-only register never
    ///   rewrites a row to make it look tidier than its history was.
    ///
    /// A reader chasing a correction must treat these exactly like
    /// [`Self::confirmed_by_record`]: they are disclosures to this audience,
    /// found the only way they can be found.
    pub audience_not_on_record: usize,
    /// Acts filed here whose record names a DIFFERENT audience.
    ///
    /// Reachable, not hypothetical: the act ref derives from
    /// `(scope, idempotency_key)`, so preparing the same key a second time for
    /// a second audience resumes the first record — audience and all — and then
    /// files it under the second audience's axis too. Both filings are then
    /// true and the record names only the first.
    ///
    /// Counted apart because it is the one bucket where the two sources
    /// disagree, and an owner deciding whether a correction is owed to *this*
    /// counterparty needs to know the record did not say so.
    pub recorded_for_another_audience: usize,
}

/// The cap [`OutwardAssertionStore::acts_for_audience`] is given when a caller
/// has no reason to choose one.
///
/// Sized for a person, not for storage: this is the number of disclosures
/// somebody chasing a correction can still read and act on in one pass. A
/// relationship with more than this is one where the caller has to say what it
/// wants.
pub const DEFAULT_AUDIENCE_ACT_LIMIT: usize = 200;

/// The largest cap [`OutwardAssertionStore::acts_for_audience`] will accept.
///
/// A ceiling on a caller-supplied bound, because the limit is a number of file
/// opens and a route passing a query parameter straight through would otherwise
/// let a URL ask for an unbounded scan.
///
/// **Refused, not clamped.** Serving a thousand to a caller that asked for a
/// million answers a different question from the one asked, and the caller has
/// no way to notice.
pub const MAX_AUDIENCE_ACT_LIMIT: usize = 1000;

/// Where an obligation has got to. Deliberately two states: this store finds
/// what is affected and records that something is owed. **Deciding whether to
/// correct is not its job** (plan §10) — the agent and owner decide, and record
/// the outcome here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationState {
    Open,
    Resolved,
}

/// Something owed because a claim was retracted or corrected after it had
/// already been asserted to someone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionObligation {
    pub obligation_id: String,
    /// The correction or retraction that raised this.
    pub correction_ref: String,
    pub approved_claim_ref: String,
    pub outward_act_ref: String,
    pub assertion_use_id: String,
    pub audience: String,
    /// The act's status when the obligation was raised. Kept rather than looked
    /// up later, because it is why the obligation exists and a later status
    /// change does not retire the debt.
    pub disclosure_status_at_raise: OutwardActStatus,
    pub state: ObligationState,
    pub raised_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

/// An appended state change on an obligation. Same shape as an act: the file is
/// the log, the current state is the fold, nothing is edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ObligationTransition {
    obligation_id: String,
    state: ObligationState,
    resolution: Option<String>,
    at: String,
}

impl OutwardAssertionStore {
    fn obligation_path(&self, scope: &OutwardScope, obligation_id: &str) -> PathBuf {
        self.root(scope)
            .join("obligations")
            .join(format!("{obligation_id}.jsonl"))
    }

    // ── Payloads and the dispatch write point (plan phase 2) ────────────────

    fn payload_path(&self, scope: &OutwardScope, payload_ref: &str) -> PathBuf {
        self.root(scope).join("payloads").join(format!(
            "{}.json",
            payload_ref.trim_start_matches("payload://")
        ))
    }

    /// Store the exact bytes that are about to go out, and return a ref naming
    /// them.
    ///
    /// Content-addressed, so the ref cannot later name different bytes — which
    /// is the property the plan needs when it says the payload must be an
    /// immutable revision, so *"what was reviewed and what was sent cannot
    /// diverge"*.
    ///
    /// **Deliberate deviation from plan §2**, which assigns immutable refs to
    /// Artifact V2. Artifact V2's write path requires a `task_id`, and an
    /// outward send does not always have one; a ref that existed only for
    /// task-originated sends would leave the rest of them unrecordable. When
    /// Artifact V2 gains an outward-payload revision, this ref migrates and the
    /// stored bytes are the migration input.
    pub fn store_payload(&self, scope: &OutwardScope, payload: &[u8]) -> Result<String> {
        let payload_ref = payload_ref_of(payload);
        let path = self.payload_path(scope, &payload_ref);
        // Content-addressed: identical bytes are already stored under this ref,
        // so rewriting them would be a no-op with a chance of a torn file.
        if self.read_to_string_if_present(&path)?.is_none() {
            self.workspace_layout
                .write_atomic_path_sync(&path, payload)
                .with_context(|| format!("writing payload {}", path.display()))?;
        }
        Ok(payload_ref)
    }

    pub fn load_payload(&self, scope: &OutwardScope, payload_ref: &str) -> Result<Option<String>> {
        self.read_to_string_if_present(&self.payload_path(scope, payload_ref))
    }

    /// Prepare the disclosure for one outward dispatch — the plan's phase 2
    /// write point, expressed once for every outward capability rather than per
    /// adapter.
    ///
    /// Returns `Err` if anything about recording fails, and the caller **must
    /// then not send**: an outward act that succeeded while its record failed is
    /// a disclosure nobody can find later.
    ///
    /// The idempotency key is derived from the dispatch itself — capability,
    /// action and exact parameters — so an identical retry resumes one record
    /// rather than opening a second.
    ///
    /// `intended_audience` is supplied by the caller from
    /// [`execution::resolve_effective_action`], never parsed from the payload
    /// here. A skill template can widen the recipient list through a passthrough
    /// parameter, so the payload's typed fields name a SUBSET of who is actually
    /// reached — and a disclosure recording that subset would aim correction
    /// propagation at the wrong people, which is the failure this store exists
    /// to prevent arriving through its own front door.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_dispatch(
        &self,
        scope: &OutwardScope,
        capability: &str,
        action: &str,
        channel: OutwardChannel,
        effective_sender: &str,
        payload: &[u8],
        intended_audience: Vec<String>,
        work: Option<&WorkContextKind>,
        consequence_class: crate::magician_v2::agents::ConsequenceClass,
        now: &str,
    ) -> Result<OutwardActDisclosure> {
        let exact_payload_artifact_ref = self.store_payload(scope, payload)?;
        let idempotency_key =
            dispatch_idempotency_key(capability, action, &exact_payload_artifact_ref)?;
        // Which axis the act is filed under is DERIVED from the work it was
        // performed inside, never chosen by the caller. A caller free to name
        // `engagement_id` for program-shaped work would file an act under a
        // relationship it does not belong to, and `RecipientInEngagement` —
        // which is a predicate about a bilateral relationship — would then read
        // that act as evidence about a counterparty nobody ever contacted.
        //
        // A `match`, not a lookup: a third kind of work must fail to compile
        // here rather than quietly file under neither axis, which is the state
        // this whole line existed in before — `program_id` was hardcoded `None`,
        // so a programme's outward history was unreachable to every sweep that
        // reads that axis.
        let (program_id, engagement_id) = match work {
            Some(WorkContextKind::Program(id)) => (Some(id.clone()), None),
            Some(WorkContextKind::Engagement(id)) => (None, Some(id.clone())),
            None => (None, None),
        };
        let request = PrepareOutwardAct {
            idempotency_key,
            program_id,
            engagement_id,
            exact_payload_artifact_ref,
            effective_sender: effective_sender.to_string(),
            intended_audience,
            channel,
            // Carried, not interpreted: this store records what class the act
            // was, and the envelope resolver decides what may be done about it.
            //
            // Supplied by the caller rather than derived here. The caller holds
            // the resolved arguments, and classification has to be
            // argument-aware — a passthrough-argv send whose token says nothing
            // must not persist as `private_local`, the one class needing no gate
            // at all. Deriving it here would also give the evidence subsystem a
            // dependency on the classifier's internals for no gain.
            consequence_class: consequence_class.as_str().to_string(),
        };
        self.prepare(scope, &request, now)
    }

    /// The act ref [`Self::prepare_dispatch`] resolves for this dispatch,
    /// derived and **not written**.
    ///
    /// Exists so a caller holding the same dispatch a second time — the
    /// post-dispatch settle, which sees the tool's result and must find the
    /// disclosure the pre-dispatch gate wrote — can name the record without
    /// preparing one. `prepare_dispatch` is idempotent and would have returned
    /// the same ref, but calling it would also CREATE a disclosure for a
    /// dispatch that was captured or refused before anything was recorded,
    /// which is exactly the act that must never acquire a send's record.
    ///
    /// Derived through the same key as `prepare_dispatch` rather than beside
    /// it: two derivations of one id disagree eventually, and the disagreement
    /// would look like a send whose disclosure had vanished.
    pub fn dispatch_act_ref(
        &self,
        scope: &OutwardScope,
        capability: &str,
        action: &str,
        payload: &[u8],
    ) -> Result<String> {
        let key = dispatch_idempotency_key(capability, action, &payload_ref_of(payload))?;
        Ok(derive_act_ref(scope, &key))
    }

    // ── Provider bindings (plan phase 2, the receipt half) ──────────────

    /// Record WHICH provider message this act became.
    ///
    /// # Why the act needs this at all
    ///
    /// A bounce, a complaint and a delivery event all arrive naming the
    /// provider's own message id and nothing else. Until an act carries that id
    /// there is no join between a provider event and the disclosure it is about,
    /// so `delivery::DeliveryLedger::reconcile` — which takes an act ref it
    /// cannot discover for itself — has nothing to be called with. That is the
    /// whole reason every live send has sat at
    /// [`OutwardActStatus::DispatchUnknown`] forever.
    ///
    /// # What it is NOT
    ///
    /// It is not delivery, and it is not a delivery-ledger observation. The
    /// act moves to [`OutwardActStatus::ProviderAccepted`], whose own
    /// documentation says *"This is not delivery"*, and nothing is written to
    /// the ledger — because a send response says the provider took the message,
    /// not that anybody received it. Writing an `accepted` observation here
    /// would empty `DeliveryLedger::unreconciled`, and that sweep's whole value
    /// is naming the sends no provider event has ever come back about.
    ///
    /// # Fail closed
    ///
    /// - A blank id, or one carrying [`FIELD_SEP`] or any other control
    ///   character, is **refused**: it feeds an index key, and one that could
    ///   shift a component boundary would file this act under another act's
    ///   provider message.
    /// - An act that never left — `prepared`, which is where a CAPTURED act
    ///   rests — is refused. A rehearsal must never acquire a real message id.
    /// - A terminal act is refused. `delivered`, `failed`, `retracted` and
    ///   `corrected` do not resurrect.
    /// - A provider message already bound to a DIFFERENT act is refused: one
    ///   provider message is one act, and a second claim on it would carry a
    ///   bounce onto a disclosure that never sent it.
    ///
    /// Idempotent: the identical binding again resumes the record and appends
    /// nothing. A *different* id on an already-bound act is an error, not a
    /// silent overwrite — two ids for one message means somebody upstream is
    /// mis-parsing, and quietly keeping the first would hide it.
    pub fn record_provider_message(
        &self,
        scope: &OutwardScope,
        outward_act_ref: &str,
        provider: &str,
        provider_message_id: &str,
        now: &str,
    ) -> Result<()> {
        let provider = validated_component(provider, "a provider name")?;
        let provider_message_id =
            validated_component(provider_message_id, "a provider message id")?;

        let Some(act) = self.load_act(scope, outward_act_ref)? else {
            anyhow::bail!(
                "no outward act `{outward_act_ref}`: a provider message cannot be bound to an \
                 act that was never prepared"
            );
        };

        if let (Some(held_provider), Some(held_id)) =
            (act.provider.as_deref(), act.provider_message_id.as_deref())
        {
            if held_provider == provider && held_id == provider_message_id {
                return Ok(());
            }
            anyhow::bail!(
                "outward act `{outward_act_ref}` is already bound to provider message \
                 `{held_provider}:{held_id}` and this binding says `{provider}:\
                 {provider_message_id}`: an identical replay resumes the record, but two ids for \
                 one message means somebody upstream is mis-parsing them and keeping the first \
                 would hide whichever is wrong"
            );
        }

        match act.status {
            OutwardActStatus::Dispatching
            | OutwardActStatus::DispatchUnknown
            | OutwardActStatus::ProviderAccepted => {},
            OutwardActStatus::Prepared => anyhow::bail!(
                "outward act `{outward_act_ref}` is still `prepared`: nothing left, so there is \
                 no provider message to bind. A captured act rests here, and a rehearsal that \
                 acquired a real message id would be reconcilable evidence of a send that never \
                 happened"
            ),
            terminal => anyhow::bail!(
                "outward act `{outward_act_ref}` is `{}`: a settled act does not reopen, and \
                 binding a provider message to it would resurrect it",
                terminal.as_str()
            ),
        }

        if let Some(bound) =
            self.act_for_provider_message(scope, &provider, &provider_message_id)?
        {
            if bound != outward_act_ref {
                anyhow::bail!(
                    "provider message `{provider}:{provider_message_id}` already belongs to act \
                     `{bound}` and cannot also belong to `{outward_act_ref}`: one provider \
                     message is one act, and a second claim on it would move a bounce or a \
                     complaint onto a disclosure that never carried it"
                );
            }
        }

        // Index BEFORE the row, the same ordering every other writer here uses:
        // a pointer to a transition that never landed is inert, while a
        // transition nothing points at is a binding no provider event can ever
        // find — which is the exact failure this whole door exists to end.
        self.append_index(
            scope,
            PROVIDER_MESSAGE_AXIS,
            &provider_message_axis_value(&provider, &provider_message_id),
            outward_act_ref,
        )?;
        self.append_transition_with_binding(
            scope,
            outward_act_ref,
            OutwardActStatus::ProviderAccepted,
            None,
            None,
            Some((&provider, &provider_message_id)),
            now,
        )
    }

    /// Which act a provider message belongs to — the join a receipt needs.
    ///
    /// The **only** direction a provider event can travel: it knows an id and
    /// nothing else. Answers `None` for an id nothing was ever bound to, and
    /// refuses when the index names more than one act — an ambiguous join must
    /// never be resolved by picking one, because the wrong pick lands a bounce
    /// or a complaint on a disclosure that never sent it.
    pub fn act_for_provider_message(
        &self,
        scope: &OutwardScope,
        provider: &str,
        provider_message_id: &str,
    ) -> Result<Option<String>> {
        let provider = validated_component(provider, "a provider name")?;
        let provider_message_id =
            validated_component(provider_message_id, "a provider message id")?;
        let entries = self.index_entries(
            scope,
            PROVIDER_MESSAGE_AXIS,
            &provider_message_axis_value(&provider, &provider_message_id),
        )?;
        match entries.len() {
            0 => Ok(None),
            1 => Ok(entries.into_iter().next()),
            _ => anyhow::bail!(
                "provider message `{provider}:{provider_message_id}` is filed against {} acts \
                 ({}): one provider message is one act, and choosing between them would attach \
                 a receipt to a disclosure that never sent it",
                entries.len(),
                entries.join(", ")
            ),
        }
    }

    // ── Reverse lookup (plan phase 4) ───────────────────────────────────────

    /// Every disclosure that carried this claim.
    ///
    /// *"Did we tell YC something different from Techstars"* becomes a query
    /// rather than recollection. Reads the `claim` index written when each
    /// assertion-use row was — not a scan, because a scan would be correct and
    /// would also be the thing nobody runs.
    ///
    /// Rows whose act has since vanished are skipped rather than erroring: a
    /// missing act is a data problem to surface elsewhere, and failing the whole
    /// lookup would hide every other disclosure that IS affected.
    pub fn disclosures_carrying_claim(
        &self,
        scope: &OutwardScope,
        approved_claim_ref: &str,
    ) -> Result<Vec<AffectedDisclosure>> {
        let use_ids = self.index_entries(scope, "claim", approved_claim_ref)?;
        self.resolve_affected(scope, &use_ids)
    }

    /// Every disclosure that rested on this evidence.
    ///
    /// The other direction of the same question: when a source turns out to be
    /// wrong, what did we say on the strength of it?
    pub fn disclosures_resting_on_evidence(
        &self,
        scope: &OutwardScope,
        evidence_ref: &str,
    ) -> Result<Vec<AffectedDisclosure>> {
        let use_ids = self.index_entries(scope, "evidence", evidence_ref)?;
        self.resolve_affected(scope, &use_ids)
    }

    fn resolve_affected(
        &self,
        scope: &OutwardScope,
        use_ids: &[String],
    ) -> Result<Vec<AffectedDisclosure>> {
        let mut out = Vec::new();
        for use_id in use_ids {
            let Some(use_row) = self.load_assertion_use(scope, use_id)? else {
                continue;
            };
            let Some(act) = self.load_act(scope, &use_row.outward_act_ref)? else {
                continue;
            };
            out.push(AffectedDisclosure {
                outward_act_ref: act.outward_act_ref.clone(),
                assertion_use_id: use_row.assertion_use_id.clone(),
                approved_claim_ref: use_row.approved_claim_ref.clone(),
                audience: use_row.audience.clone(),
                status: act.status,
                exact_payload_artifact_ref: act.exact_payload_artifact_ref.clone(),
            });
        }
        out.sort_by(|a, b| a.assertion_use_id.cmp(&b.assertion_use_id));
        Ok(out)
    }

    /// **What have we told this counterparty?** — every act filed under one
    /// audience.
    ///
    /// The read direction of [`Self::prepare_for_audience`]. That writer files
    /// an entry under [`audience_axis`] on every room grant; until this
    /// existed nothing outside this file's own tests read those entries, so
    /// the one question an audience exists to answer could not be asked.
    ///
    /// Shaped after [`Self::disclosures_carrying_claim`] on purpose — one
    /// index read, entries resolved to records, a deterministic order — because
    /// a correction chasing a claim and one chasing a counterparty must agree
    /// about what they found. Where it deliberately differs is said at
    /// [`AudienceActs::unresolved`].
    ///
    /// # Fail closed
    ///
    /// An unreadable index propagates: [`Self::index_entries`] reads absence
    /// only from `NotFound`, so a permissions fault or a torn read is an `Err`
    /// here and never an empty list. An audience with no index file answers
    /// `indexed: 0` — a relationship we have told nothing has no file, and
    /// that is the honest zero. Those two are the whole distinction this
    /// surface turns on, and folding a read fault into *"we told them
    /// nothing"* is the one wrong answer that ends an investigation.
    ///
    /// # A zero here does not mean a zero before today
    ///
    /// This reads the audience axis and nothing else. The axis, the writer and
    /// [`OutwardActDisclosure::audience`] all arrived together, so **no act
    /// written before them is filed under any audience** — not mis-filed,
    /// simply absent — and a workspace with years of room grants answers
    /// `indexed: 0`. That is *"not filed"*, never *"never told"*, and nothing
    /// can repair it in the other direction for rows that carry no audience of
    /// their own: [`Self::reindex_work_axes`] says the same thing from the
    /// work side.
    ///
    /// # De-duplication is [`Self::index_entries`]'s, and is not repeated here
    ///
    /// That reader filters the append-only file through a `HashSet`, so what it
    /// returns is already distinct — which is what makes the count a count of
    /// acts rather than of appends. The writer's own read-before-append guard
    /// is not what this relies on: a second writer, a repair, or a sweep that
    /// races one could all append a line that is already there, and the answer
    /// still may not list one disclosure twice.
    ///
    /// # The cap, and which entries it drops
    ///
    /// `limit` bounds the number of ACT FILES opened. The index file itself is
    /// read whole, because its length is [`AudienceActs::indexed`] — the
    /// denominator a partial answer is only honest beside.
    ///
    /// Which entries survive is act-ref order, and an act ref is a `blake3`
    /// digest, so that order is stable and **arbitrary — not newest first**.
    /// Answering chronologically would mean loading every act to sort by
    /// `prepared_at`, which is precisely the read the cap exists to prevent.
    /// One that needs the most recent needs an index this store does not keep,
    /// and should not be sold this one as it.
    ///
    /// # `limit` has a ceiling; `after` is how you get past it
    ///
    /// [`MAX_AUDIENCE_ACT_LIMIT`] bounds ONE page, not the relationship. A
    /// caller wanting the whole of an audience larger than the ceiling pages
    /// through it with `after`: pass the previous answer's
    /// [`AudienceActs::next_after`] and the read resumes at the first act ref
    /// strictly greater than it.
    ///
    /// A **keyset** cursor, not an offset. The refs are sorted before the
    /// cursor is applied, so *"everything after this one"* names the same set
    /// even when the index gains entries or is rewritten by a repair between
    /// pages; an offset would silently skip or repeat rows in exactly that
    /// case, and on a correction a skipped row is a disclosure nobody chases.
    ///
    /// `after` is a FILTER over this scope's own index entries and never
    /// reaches a path, so a caller-supplied cursor cannot address another
    /// scope's index however it is spelled.
    ///
    /// Termination and exactly-once, both by construction:
    ///
    /// - [`AudienceActs::next_after`] is `Some` only when entries were dropped
    ///   by the cap, and a capped page holds exactly `limit` refs — `limit` is
    ///   refused below one — so every continuation advances the cursor by at
    ///   least one ref and the walk cannot loop.
    /// - The last page is the one whose `remaining <= limit`. It returns every
    ///   entry that was left and clears `next_after`, including the boundary
    ///   case where `remaining == limit` exactly — no empty page follows.
    /// - Each page takes refs strictly greater than the previous page's last,
    ///   so no ref is served twice.
    ///
    /// [`AudienceActs::indexed`] stays the WHOLE relationship on every page,
    /// so an owner can always see how much of it they are looking at rather
    /// than only how much this page holds; [`AudienceActs::remaining`] is what
    /// was still ahead of the cursor.
    pub fn acts_for_audience(
        &self,
        scope: &OutwardScope,
        audience: &AudienceRef,
        limit: usize,
        after: Option<&str>,
    ) -> Result<AudienceActs> {
        // The same refusal the writer makes, for the same reason: every unnamed
        // audience hashes to one index file, so a blank id would answer with
        // acts performed for somebody else's relationship. Asked here too
        // because a reader that admitted what the writer refuses would serve
        // that pooled file to anyone who asked for it.
        if !audience.is_named() {
            anyhow::bail!(
                "audience of kind `{}` has a blank id: every unnamed audience shares one index \
                 file, so this would answer with acts performed for somebody else's relationship",
                audience.kind.as_str()
            );
        }
        if limit == 0 || limit > MAX_AUDIENCE_ACT_LIMIT {
            anyhow::bail!(
                "`limit` must be between 1 and {MAX_AUDIENCE_ACT_LIMIT}, not {limit}: a zero \
                 limit reports `no acts` for an audience that has some, and an unbounded one \
                 opens one file per index entry"
            );
        }
        let mut act_refs = self.index_entries(scope, audience_axis(audience.kind), &audience.id)?;
        let indexed = act_refs.len();
        // Sorted BEFORE the cap. Which entries a partial answer drops has to be
        // a property of the act refs, not of the order two sweeps happened to
        // append in — otherwise the same query over an unchanged scope returns
        // different acts once the index has been rewritten by a repair.
        // `resolve_affected` sorts at the end because it resolves everything;
        // here the sort has to come first or the cap decides nothing stable.
        act_refs.sort();
        // A KEYSET cursor, not an offset. The refs are sorted, so "everything
        // after this one" is stable even when the index gains entries or is
        // rewritten by a repair between pages — an offset would silently skip
        // or repeat rows in exactly that case. `indexed` keeps meaning the whole
        // index, so a caller can always see how much of the relationship it is
        // looking at rather than only how much this page holds.
        if let Some(after) = after {
            act_refs.retain(|held| held.as_str() > after);
        }
        let remaining = act_refs.len();
        let capped_at = (remaining > limit).then_some(limit);
        act_refs.truncate(limit);
        // Named only when there IS a next page. `Some` is an instruction to ask
        // again; `None` means this is the end of the relationship, and a caller
        // that paged to here has seen all of it.
        let next_after = capped_at
            .is_some()
            .then(|| act_refs.last().cloned())
            .flatten();

        let mut answer = AudienceActs {
            audience: audience.clone(),
            indexed,
            remaining,
            next_after,
            examined: act_refs.len(),
            capped_at,
            acts: Vec::with_capacity(act_refs.len()),
            unresolved: 0,
            confirmed_by_record: 0,
            audience_not_on_record: 0,
            recorded_for_another_audience: 0,
        };
        for act_ref in &act_refs {
            // A malformed entry reads as absent and is counted here like any
            // other entry that resolves to nothing. It needs no check of its own:
            // an underived ref names a file that was never written, which is
            // exactly what `unresolved` means.
            let Some(act) = self.load_act(scope, act_ref)? else {
                answer.unresolved += 1;
                continue;
            };
            // Exactly one arm per act, so the three counts partition `acts`.
            match act.audience.as_ref() {
                Some(held) if held == audience => answer.confirmed_by_record += 1,
                Some(_) => answer.recorded_for_another_audience += 1,
                None => answer.audience_not_on_record += 1,
            }
            answer.acts.push(act);
        }
        Ok(answer)
    }

    // ── Correction obligations (plan phase 4) ───────────────────────────────

    /// Raise one obligation per **active** affected disclosure.
    ///
    /// Two properties the plan is explicit about:
    ///
    /// - *"retraction creates obligations against every affected active
    ///   disclosure. It never silently rewrites history."* Nothing here touches
    ///   the assertion-use rows or the acts. The obligation points at them; they
    ///   do not learn about it. A record that could be edited by a later event
    ///   is not an audit trail.
    /// - Only ACTIVE disclosures. A `prepared` act never left, and a `failed`
    ///   one told nobody — raising a correction obligation for either would be
    ///   inventing a debt. A `dispatch_unknown` act DOES raise one, because it
    ///   might have reached someone and the honest position is that we owe a
    ///   check.
    ///
    /// Idempotent: the obligation id derives from
    /// `(correction_ref, assertion_use_id)`, so raising twice for the same
    /// correction returns the same obligations rather than doubling them.
    pub fn raise_correction_obligations(
        &self,
        scope: &OutwardScope,
        approved_claim_ref: &str,
        correction_ref: &str,
        now: &str,
    ) -> Result<Vec<CorrectionObligation>> {
        let affected = self.disclosures_carrying_claim(scope, approved_claim_ref)?;
        let mut raised = Vec::new();
        for disclosure in affected {
            if !disclosure.status.is_active_disclosure() {
                continue;
            }
            let obligation_id = derive_obligation_id(correction_ref, &disclosure.assertion_use_id);
            if let Some(existing) = self.load_obligation(scope, &obligation_id)? {
                raised.push(existing);
                continue;
            }
            let obligation = CorrectionObligation {
                obligation_id: obligation_id.clone(),
                correction_ref: correction_ref.to_string(),
                approved_claim_ref: disclosure.approved_claim_ref.clone(),
                outward_act_ref: disclosure.outward_act_ref.clone(),
                assertion_use_id: disclosure.assertion_use_id.clone(),
                audience: disclosure.audience.clone(),
                disclosure_status_at_raise: disclosure.status,
                state: ObligationState::Open,
                raised_at: now.to_string(),
                resolution: None,
                resolved_at: None,
            };
            self.append_json(&self.obligation_path(scope, &obligation_id), &obligation)?;
            self.append_index(scope, "correction", correction_ref, &obligation_id)?;
            self.append_index(
                scope,
                "obligation_act",
                &disclosure.outward_act_ref,
                &obligation_id,
            )?;
            raised.push(obligation);
        }
        Ok(raised)
    }

    /// Record what was decided about an obligation.
    ///
    /// The store does not decide — correct, replace, withdraw or nothing is the
    /// agent's and owner's call (plan §10). "Nothing" is a legitimate resolution
    /// and must be recordable, or the register fills with debts nobody can
    /// close and stops being read.
    pub fn resolve_obligation(
        &self,
        scope: &OutwardScope,
        obligation_id: &str,
        resolution: &str,
        now: &str,
    ) -> Result<()> {
        if self.load_obligation(scope, obligation_id)?.is_none() {
            anyhow::bail!("no correction obligation `{obligation_id}`");
        }
        let transition = ObligationTransition {
            obligation_id: obligation_id.to_string(),
            state: ObligationState::Resolved,
            resolution: Some(resolution.to_string()),
            at: now.to_string(),
        };
        self.append_json(&self.obligation_path(scope, obligation_id), &transition)
    }

    pub fn load_obligation(
        &self,
        scope: &OutwardScope,
        obligation_id: &str,
    ) -> Result<Option<CorrectionObligation>> {
        let path = self.obligation_path(scope, obligation_id);
        let Some(raw) = self.read_to_string_if_present(&path)? else {
            return Ok(None);
        };
        let mut current: Option<CorrectionObligation> = None;
        // Anchored to the last line OF THE FILE, not the last line that survives
        // the blank filter below. A file ending in a whitespace-only unterminated
        // fragment would otherwise hand the tear exemption to the complete,
        // newline-terminated record above it — silently dropping real corruption
        // as though it were a torn write. If the final raw line is blank, nothing
        // parseable was torn and no surviving line has earned the exemption.
        let tail_may_be_torn = !raw.is_empty()
            && !raw.ends_with('\n')
            && raw
                .lines()
                .next_back()
                .is_some_and(|line| !line.trim().is_empty());
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let last = lines.len().saturating_sub(1);
        for (position, line) in lines.iter().enumerate() {
            if current.is_none() {
                current = match serde_json::from_str(line) {
                    Ok(head) => Some(head),
                    // A torn head is a raise that never completed: no debt.
                    Err(_) if position == last && tail_may_be_torn => return Ok(None),
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("parsing obligation head in {}", path.display())
                        })
                    },
                };
                continue;
            }
            let transition: ObligationTransition = match serde_json::from_str(line) {
                Ok(transition) => transition,
                Err(_) if position == last && tail_may_be_torn => break,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("parsing obligation transition in {}", path.display())
                    })
                },
            };
            if let Some(obligation) = current.as_mut() {
                obligation.state = transition.state;
                obligation.resolution = transition.resolution.clone();
                obligation.resolved_at = Some(transition.at.clone());
            }
        }
        Ok(current)
    }

    /// Every obligation raised by one correction, in whatever state.
    pub fn obligations_for_correction(
        &self,
        scope: &OutwardScope,
        correction_ref: &str,
    ) -> Result<Vec<CorrectionObligation>> {
        let ids = self.index_entries(scope, "correction", correction_ref)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(obligation) = self.load_obligation(scope, &id)? {
                out.push(obligation);
            }
        }
        out.sort_by(|a, b| a.obligation_id.cmp(&b.obligation_id));
        Ok(out)
    }

    /// Obligations still owed for one correction.
    pub fn open_obligations_for_correction(
        &self,
        scope: &OutwardScope,
        correction_ref: &str,
    ) -> Result<Vec<CorrectionObligation>> {
        Ok(self
            .obligations_for_correction(scope, correction_ref)?
            .into_iter()
            .filter(|obligation| obligation.state == ObligationState::Open)
            .collect())
    }
}

/// The id for one `(correction, assertion use)` pair, so raising the same
/// correction twice cannot double the debt.
fn derive_obligation_id(correction_ref: &str, assertion_use_id: &str) -> String {
    format!(
        "obl-{}",
        stable_id(&format!("{correction_ref}{FIELD_SEP}{assertion_use_id}"))
    )
}

/// Field separator for derived ids. A unit separator cannot appear in a ref,
/// a claim name or an address, so `(a, b)` and `(ab, "")` cannot collide into
/// one id. Declared as a char rather than written inline: inside a `format!`
/// string a brace is also a format token, and the two readings of `{` are worth
/// not making a reader disambiguate.
const FIELD_SEP: char = '\u{1f}';

/// Stable, filesystem-safe id for an arbitrary string.
fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The prefix every act ref carries. See [`is_derived_act_ref`].
pub const ACT_REF_PREFIX: &str = "act-";

/// Width of the digest half of an act ref: [`stable_id`] truncates blake3's
/// hex to 32, and `to_hex` is lowercase.
pub const ACT_REF_DIGEST_HEX: usize = 32;

/// Whether a value has the shape [`derive_act_ref`] produces.
///
/// This lives beside the minter deliberately. An act ref is a digest, so there
/// is no such thing as a *nearly* correct one — a value that is not exactly the
/// derived form names a record that was never written, and the reader of that
/// absence concludes nothing was sent. That makes the shape a safety property,
/// and a safety property with two spellings is a safety property with a drift
/// bug: whoever changes `stable_id`'s width must find every checker. Keeping the
/// predicate next to the minter means there is one place to change and
/// `the_minted_shape_is_the_shape_the_checker_accepts` fails if they disagree.
pub fn is_derived_act_ref(value: &str) -> bool {
    value.strip_prefix(ACT_REF_PREFIX).is_some_and(|digest| {
        digest.len() == ACT_REF_DIGEST_HEX
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The content-addressed ref for a payload's exact bytes.
///
/// Pure, so the ref can be computed by a caller that must NOT write — see
/// [`OutwardAssertionStore::dispatch_act_ref`].
fn payload_ref_of(payload: &[u8]) -> String {
    format!("payload://blake3:{}", blake3::hash(payload).to_hex())
}

/// The idempotency key for one outward dispatch.
///
/// The single derivation, so the pre-dispatch write point and the
/// post-dispatch settle cannot land on two different act refs for one send.
///
/// Refuses a caller string carrying [`FIELD_SEP`]. The key is three components
/// joined by it, so a capability or action token holding one could shift a
/// component boundary and resume — or shadow — a different act's record, which
/// is a send filed as somebody else's disclosure.
fn dispatch_idempotency_key(
    capability: &str,
    action: &str,
    exact_payload_artifact_ref: &str,
) -> Result<String> {
    for (value, what) in [
        (capability, "a capability name"),
        (action, "an action token"),
        (exact_payload_artifact_ref, "a payload ref"),
    ] {
        if value.contains(FIELD_SEP) {
            anyhow::bail!(
                "{what} must not contain U+001F: it is the separator that keeps an act ref's \
                 components apart, and a crafted one would resume another act's record"
            );
        }
    }
    Ok(format!(
        "{capability}{FIELD_SEP}{action}{FIELD_SEP}{exact_payload_artifact_ref}"
    ))
}

/// The index value one provider message is filed under.
fn provider_message_axis_value(provider: &str, provider_message_id: &str) -> String {
    format!("{provider}{FIELD_SEP}{provider_message_id}")
}

/// A caller string that feeds a provider-message index key, checked and
/// trimmed.
///
/// Blank is refused because an unidentifiable binding joins nothing. U+001F is
/// refused because it is the separator between provider and id, so a crafted
/// value could file this act under a different provider's message. Every other
/// control character is refused too: one arriving here means the value was
/// mis-parsed upstream, and a newline would tear the append-only index line in
/// two — a torn line is a pointer to nowhere, and a receipt that lands on
/// nowhere is a receipt nobody ever sees.
fn validated_component(value: &str, what: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{what} is required: a binding that names nothing reconciles nothing");
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!(
            "{what} must not contain control characters (U+001F is the index separator, and a \
             newline would tear the log line in two)"
        );
    }
    Ok(trimmed.to_string())
}

/// The act ref for a scope and idempotency key.
///
/// Derived rather than allocated, so *"a retry resumes the same record"* is a
/// property of the path rather than a lookup someone has to remember to do.
/// The scope is folded in so two scopes cannot collide on one key.
pub(crate) fn derive_act_ref(scope: &OutwardScope, idempotency_key: &str) -> String {
    format!(
        "{ACT_REF_PREFIX}{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal, scope.workspace, idempotency_key
        ))
    )
}

/// The id for one `(act, claim, audience)` triple — the plan's uniqueness
/// invariant, expressed as where the file lands.
fn derive_assertion_use_id(
    outward_act_ref: &str,
    approved_claim_ref: &str,
    audience: &str,
) -> String {
    format!(
        "use-{}",
        stable_id(&format!(
            "{outward_act_ref}{FIELD_SEP}{approved_claim_ref}{FIELD_SEP}{audience}"
        ))
    )
}

#[cfg(test)]
mod tests {
    /// DRIFT GUARD. `is_derived_act_ref` restates `derive_act_ref`'s shape, and a
    /// restated invariant is one that can silently stop matching. This asserts
    /// against a ref the minter actually produced rather than against a literal,
    /// so widening `stable_id`'s truncation — or letting `to_hex` return
    /// uppercase — fails here instead of at a caller that can no longer address
    /// any of its own records.
    ///
    /// The negative half matters as much: a checker that accepted everything
    /// would pass the first assertion alone.
    #[test]
    fn the_minted_shape_is_the_shape_the_checker_accepts() {
        let scope = OutwardScope {
            principal: "p".to_string(),
            workspace: "w".to_string(),
        };
        let minted = derive_act_ref(&scope, "idempotency-key");
        assert!(
            is_derived_act_ref(&minted),
            "the minter produced {minted:?}, which its own checker rejects"
        );

        for refused in [
            "",
            "act-",
            "abc",
            &minted[..minted.len() - 1],
            &format!("{minted}a"),
            &minted.to_uppercase(),
            &minted.replace('-', "_"),
            &format!("{minted}/../elsewhere"),
        ] {
            assert!(
                !is_derived_act_ref(refused),
                "{refused:?} is not a derived act ref but the checker accepted it"
            );
        }
    }

    use super::*;

    fn store() -> (tempfile::TempDir, OutwardAssertionStore, OutwardScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, OutwardScope::new("anonymous", "default"))
    }

    /// A live send that has left but has no receipt yet: the exact state every
    /// act reaches on the dispatch path, and the only one a binding is
    /// admitted from.
    fn dispatched_act(store: &OutwardAssertionStore, scope: &OutwardScope, key: &str) -> String {
        let act = store
            .prepare(scope, &email_request(key), "t0")
            .expect("prepare");
        let act_ref = act.outward_act_ref;
        store.mark_dispatching(scope, &act_ref, "t1").expect("t1");
        store
            .mark_dispatch_unknown(scope, &act_ref, "dispatched; no provider message yet", "t2")
            .expect("t2");
        act_ref
    }

    /// The whole point of tier 4: a send that could not be reconciled becomes
    /// one that can.
    ///
    /// Before this, an act carried no provider message id at all, so a bounce
    /// or a complaint naming that id had no way back to the disclosure it was
    /// about — which is why `DeliveryLedger::reconcile`, which takes an act ref
    /// it cannot discover, never had a caller.
    #[test]
    fn a_bound_send_can_be_found_from_the_provider_message_alone() {
        let (_tmp, store, scope) = store();
        let act_ref = dispatched_act(&store, &scope, "send-1");

        store
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t3")
            .expect("bind");

        let act = store
            .load_act(&scope, &act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(act.provider.as_deref(), Some("agentmail"));
        assert_eq!(act.provider_message_id.as_deref(), Some("msg_01H8"));
        assert_eq!(
            act.status,
            OutwardActStatus::ProviderAccepted,
            "the provider took the message; acceptance is not delivery and the state says so"
        );

        // The direction a provider event actually travels: it knows an id and
        // nothing else.
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_01H8")
                .expect("lookup"),
            Some(act_ref.clone())
        );
        // A provider id nobody bound resolves to nothing, not to the last act.
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_NEVER")
                .expect("lookup"),
            None
        );
        // Same id, different provider: two providers reusing one id string must
        // not collide onto one act.
        assert_eq!(
            store
                .act_for_provider_message(&scope, "kapso", "msg_01H8")
                .expect("lookup"),
            None
        );
    }

    /// An identical replay resumes the record; a changed one is an error.
    #[test]
    fn a_replayed_binding_resumes_and_a_changed_one_refuses() {
        let (_tmp, store, scope) = store();
        let act_ref = dispatched_act(&store, &scope, "send-1");

        store
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t3")
            .expect("bind");
        store
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t4")
            .expect("the identical binding again is one record, not two");
        assert_eq!(
            store
                .load_act_history(&scope, &act_ref)
                .expect("history")
                .len(),
            4,
            "prepared, dispatching, dispatch_unknown, provider_accepted — the replay appended \
             nothing"
        );

        let error = store
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_DIFFERENT", "t5")
            .expect_err("two ids for one message is somebody upstream mis-parsing");
        let error = error.to_string();
        assert!(error.contains("msg_01H8"), "{error}");
        assert!(error.contains("msg_DIFFERENT"), "{error}");
    }

    /// A rehearsal must never acquire a real message id.
    ///
    /// A captured act rests at `prepared` — recorded, nothing left. Binding a
    /// provider message to it would make a send that never happened
    /// reconcilable evidence that it did.
    #[test]
    fn a_captured_act_cannot_be_bound_to_a_provider_message() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("captured"), "t0")
            .expect("prepare");

        let error = store
            .record_provider_message(&scope, &act.outward_act_ref, "agentmail", "msg_X", "t1")
            .expect_err("nothing left, so there is no provider message")
            .to_string();
        assert!(error.contains("still `prepared`"), "{error}");

        // And nothing was filed, so the id cannot be walked back to this act.
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_X")
                .expect("lookup"),
            None
        );
    }

    /// Terminal states never resurrect.
    #[test]
    fn a_settled_act_cannot_be_reopened_by_a_binding() {
        let (_tmp, store, scope) = store();

        let refused = dispatched_act(&store, &scope, "refused");
        store
            .mark_failed(&scope, &refused, "refused: recipient suppressed", "t3")
            .expect("failed");
        let error = store
            .record_provider_message(&scope, &refused, "agentmail", "msg_A", "t4")
            .expect_err("a failed act does not reopen")
            .to_string();
        assert!(error.contains("`failed`"), "{error}");

        let arrived = dispatched_act(&store, &scope, "arrived");
        store
            .record_provider_message(&scope, &arrived, "agentmail", "msg_B", "t3")
            .expect("bind");
        store
            .record_delivered(&scope, &arrived, "t4")
            .expect("delivered");
        let error = store
            .record_provider_message(&scope, &arrived, "agentmail", "msg_C", "t5")
            .expect_err("a delivered act does not reopen")
            .to_string();
        assert!(error.contains("msg_B"), "{error}");
    }

    /// A later transition that names no provider must not erase the binding.
    ///
    /// The fold sets a binding and never clears one. Otherwise a delivery — or
    /// any correction — arriving after the binding would leave the act unable
    /// to say which message it had been, and the next receipt would find
    /// nothing.
    #[test]
    fn a_later_transition_does_not_erase_the_binding() {
        let (_tmp, store, scope) = store();
        let act_ref = dispatched_act(&store, &scope, "send-1");
        store
            .record_provider_message(&scope, &act_ref, "kapso", "wamid.HBgL", "t3")
            .expect("bind");
        store
            .record_delivered(&scope, &act_ref, "t4")
            .expect("delivered");

        let act = store
            .load_act(&scope, &act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(act.status, OutwardActStatus::Delivered);
        assert_eq!(act.provider.as_deref(), Some("kapso"));
        assert_eq!(act.provider_message_id.as_deref(), Some("wamid.HBgL"));
    }

    /// One provider message is one act.
    ///
    /// A second act claiming an id already bound elsewhere would carry that
    /// message's bounce or complaint onto a disclosure that never sent it.
    #[test]
    fn one_provider_message_cannot_belong_to_two_acts() {
        let (_tmp, store, scope) = store();
        let first = dispatched_act(&store, &scope, "send-1");
        let second = dispatched_act(&store, &scope, "send-2");
        assert_ne!(first, second);

        store
            .record_provider_message(&scope, &first, "agentmail", "msg_SHARED", "t3")
            .expect("bind");
        let error = store
            .record_provider_message(&scope, &second, "agentmail", "msg_SHARED", "t4")
            .expect_err("one provider message is one act")
            .to_string();
        assert!(error.contains(&first), "{error}");
        assert!(error.contains(&second), "{error}");

        // The join still answers with the act that really sent it.
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_SHARED")
                .expect("lookup"),
            Some(first)
        );
    }

    /// A crafted id cannot shift the index key's component boundary.
    ///
    /// The key is `provider` and the id joined by U+001F, so an id carrying one
    /// could file this act under a different provider's message — and a bounce
    /// for that message would then land here.
    #[test]
    fn an_id_carrying_the_field_separator_is_refused() {
        let (_tmp, store, scope) = store();
        let act_ref = dispatched_act(&store, &scope, "send-1");

        for (provider, id) in [
            ("agentmail", "msg\u{1f}kapso"),
            ("agent\u{1f}mail", "msg_1"),
            ("agentmail", "msg\nid"),
            ("agentmail", "   "),
        ] {
            let error = store
                .record_provider_message(&scope, &act_ref, provider, id, "t3")
                .expect_err("a crafted or blank component is refused")
                .to_string();
            assert!(
                error.contains("required") || error.contains("control characters"),
                "{error}"
            );
        }

        let act = store
            .load_act(&scope, &act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(
            act.status,
            OutwardActStatus::DispatchUnknown,
            "a refused binding leaves the act exactly where it was: unknown, not accepted"
        );
        assert!(act.provider_message_id.is_none());
    }

    /// The settle finds the SAME act the gate wrote, without writing one.
    ///
    /// Two derivations of one act ref would look like a send whose disclosure
    /// had vanished, so the derivation lives in one place and this pins that
    /// `dispatch_act_ref` agrees with `prepare_dispatch` — and that asking does
    /// not create a record for a dispatch that was captured or refused.
    #[test]
    fn the_act_ref_can_be_derived_without_preparing_one() {
        let (_tmp, store, scope) = store();
        let payload = br#"{"to":"investor@example.com","subject":"hi"}"#;

        let derived = store
            .dispatch_act_ref(&scope, "agentmail-send", "send", payload)
            .expect("derive");
        assert!(
            store.load_act(&scope, &derived).expect("load").is_none(),
            "deriving must not create a disclosure: a captured or refused dispatch would \
             otherwise acquire a send's record"
        );

        let prepared = store
            .prepare_dispatch(
                &scope,
                "agentmail-send",
                "send",
                OutwardChannel::Email,
                "presto",
                payload,
                vec!["investor@example.com".to_string()],
                None,
                crate::magician_v2::agents::ConsequenceClass::BoundedCommunication,
                "t0",
            )
            .expect("prepare");
        assert_eq!(prepared.outward_act_ref, derived);

        // A different action on the same payload is a different act.
        let other = store
            .dispatch_act_ref(&scope, "agentmail-send", "forward", payload)
            .expect("derive");
        assert_ne!(other, derived);
    }

    /// A capability or action carrying the separator is refused, so a send
    /// cannot be filed as a different act's disclosure.
    #[test]
    fn a_crafted_capability_cannot_resume_another_acts_record() {
        let (_tmp, store, scope) = store();
        let error = store
            .dispatch_act_ref(&scope, "agentmail-send\u{1f}send", "", b"{}")
            .expect_err("U+001F would shift the component boundary")
            .to_string();
        assert!(error.contains("U+001F"), "{error}");
    }

    fn email_request(key: &str) -> PrepareOutwardAct {
        PrepareOutwardAct {
            idempotency_key: key.to_string(),
            program_id: Some("prog-1".to_string()),
            engagement_id: Some("eng-1".to_string()),
            exact_payload_artifact_ref: "artifact://draft@rev7".to_string(),
            effective_sender: "founder@example.com".to_string(),
            intended_audience: vec!["investor@example.com".to_string()],
            channel: OutwardChannel::Email,
            consequence_class: "disclosure".to_string(),
        }
    }

    /// §9: "No controlled outward act completes without a committed
    /// OutwardActDisclosure, or the act fails." The record exists before
    /// anything leaves, and it enters at `prepared` — not at anything a caller
    /// could mistake for sent.
    #[test]
    fn an_act_is_recorded_before_anything_leaves() {
        let (_tmp, store, scope) = store();

        let act = store
            .prepare(&scope, &email_request("send-1"), "2026-08-19T10:00:00Z")
            .expect("prepare");

        assert_eq!(act.status, OutwardActStatus::Prepared);
        assert_eq!(act.exact_payload_artifact_ref, "artifact://draft@rev7");
        assert!(!act.observed);
        assert!(act.dispatched_at.is_none());

        let loaded = store
            .load_act(&scope, &act.outward_act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, act);
    }

    /// §9: "A retried send resumes one record; no duplicates exist for one act."
    /// Enforced by the path, so it holds even for a caller that never checks.
    #[test]
    fn a_retry_resumes_the_same_record_rather_than_duplicating_it() {
        let (_tmp, store, scope) = store();

        let first = store
            .prepare(&scope, &email_request("send-1"), "2026-08-19T10:00:00Z")
            .expect("first");
        store
            .mark_dispatching(&scope, &first.outward_act_ref, "2026-08-19T10:00:01Z")
            .expect("dispatching");

        // The same key again — a retry, not a new act.
        let second = store
            .prepare(&scope, &email_request("send-1"), "2026-08-19T10:00:05Z")
            .expect("retry");

        assert_eq!(second.outward_act_ref, first.outward_act_ref);
        assert_eq!(
            second.status,
            OutwardActStatus::Dispatching,
            "a retry must resume the record's CURRENT state, not reset it to prepared"
        );
        assert_eq!(second.prepared_at, "2026-08-19T10:00:00Z");

        // A different key is a different act.
        let other = store
            .prepare(&scope, &email_request("send-2"), "2026-08-19T10:00:06Z")
            .expect("other");
        assert_ne!(other.outward_act_ref, first.outward_act_ref);
    }

    /// The full §4 ordering, and the distinction the state names exist to make:
    /// provider acceptance is not delivery.
    #[test]
    fn acceptance_and_delivery_are_different_facts() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .expect("prepare");
        let act_ref = act.outward_act_ref.clone();

        store.mark_dispatching(&scope, &act_ref, "t1").expect("t1");
        store
            .record_provider_receipt(&scope, &act_ref, "receipt-abc", "t2")
            .expect("t2");

        let accepted = store.load_act(&scope, &act_ref).unwrap().unwrap();
        assert_eq!(accepted.status, OutwardActStatus::ProviderAccepted);
        assert_eq!(accepted.effect_receipt_ref.as_deref(), Some("receipt-abc"));
        assert!(
            accepted.settled_at.is_none(),
            "provider acceptance is not delivery and must not settle the record"
        );

        store.record_delivered(&scope, &act_ref, "t3").expect("t3");
        let delivered = store.load_act(&scope, &act_ref).unwrap().unwrap();
        assert_eq!(delivered.status, OutwardActStatus::Delivered);
        assert_eq!(delivered.settled_at.as_deref(), Some("t3"));

        // Append-only: every state survives, nothing was overwritten.
        assert_eq!(
            store.load_act_history(&scope, &act_ref).unwrap(),
            vec![
                OutwardActStatus::Prepared,
                OutwardActStatus::Dispatching,
                OutwardActStatus::ProviderAccepted,
                OutwardActStatus::Delivered,
            ]
        );
    }

    /// §9: "An adapter with no reconciliation capability leaves
    /// dispatch_unknown rather than guessing." The state is a question held
    /// open, and it counts as an active disclosure because it MIGHT have told
    /// someone something.
    #[test]
    fn an_unreconcilable_act_stays_a_question() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        let act_ref = act.outward_act_ref.clone();

        store.mark_dispatching(&scope, &act_ref, "t1").unwrap();
        store
            .mark_dispatch_unknown(&scope, &act_ref, "provider has no receipt lookup", "t2")
            .unwrap();

        let unknown = store.load_act(&scope, &act_ref).unwrap().unwrap();
        assert_eq!(unknown.status, OutwardActStatus::DispatchUnknown);
        assert!(
            unknown.status.is_active_disclosure(),
            "an act that may have reached someone must still attract correction \
             obligations; treating it as harmless is the guess this state exists \
             to avoid"
        );
        assert!(
            unknown.settled_at.is_none(),
            "an unresolved act must not look settled"
        );
    }

    /// §9: "An act carrying no approved claim produces a disclosure and zero
    /// assertion-use rows — never a fabricated claim." A calendar invite asserts
    /// nothing, and must not be made to.
    #[test]
    fn an_act_with_no_claim_produces_no_assertion_rows() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("logistics"), "t0")
            .unwrap();

        assert!(store
            .index_entries(&scope, "claim", "any-claim")
            .unwrap()
            .is_empty());
        // The disclosure itself still exists, and is indexed by what it DID have.
        assert_eq!(
            store
                .index_entries(&scope, "artifact", "artifact://draft@rev7")
                .unwrap(),
            vec![act.outward_act_ref]
        );
    }

    /// §9: "one record per (outward_act_ref, approved_claim_ref, audience)" and
    /// "A claim change returns every disclosure carrying it, by reverse index."
    /// Both spellings of every channel are live at once — serde in the stored
    /// record, `as_str` in logs and comparisons — and `WhatsApp` had genuinely
    /// diverged (`whats_app` vs `whatsapp`) until review caught it. Pinned so a
    /// renamed variant fails a test instead of silently splitting a reverse
    /// lookup in two. The old spelling stays readable via a serde alias.
    #[test]
    fn channel_serde_and_as_str_never_diverge() {
        for channel in [
            OutwardChannel::Email,
            OutwardChannel::WhatsApp,
            OutwardChannel::Room,
            OutwardChannel::Form,
            OutwardChannel::Meeting,
        ] {
            assert_eq!(
                serde_json::to_string(&channel).expect("serialise"),
                format!("\"{}\"", channel.as_str()),
                "{channel:?} serialises differently from how it is logged"
            );
        }
        // And the pre-fix spelling still parses, so records already on disk
        // are not orphaned by the rename.
        let legacy: OutwardChannel =
            serde_json::from_str("\"whats_app\"").expect("alias for the old spelling");
        assert_eq!(legacy, OutwardChannel::WhatsApp);
    }

    /// `Vec::dedup` removes only CONSECUTIVE duplicates, so an id repeated as
    /// A, B, A survived it — and index files are append-only and interleaved
    /// across assertion uses, so that is the normal shape rather than an edge
    /// case. A duplicate means one disclosure listed twice in a reverse lookup.
    #[test]
    fn a_reverse_index_never_returns_one_id_twice() {
        let (_tmp, store, scope) = store();
        let path = store.index_path(&scope, "claim", "claim-1");

        // The interleaved shape: A, B, A.
        for id in ["use-a", "use-b", "use-a"] {
            let mut line = id.as_bytes().to_vec();
            line.push(b'\n');
            store
                .workspace_layout
                .append_path_sync(&path, &line)
                .expect("append");
        }

        let entries = store
            .index_entries(&scope, "claim", "claim-1")
            .expect("index read");
        assert_eq!(
            entries,
            vec!["use-a".to_string(), "use-b".to_string()],
            "a non-consecutive repeat must not survive, and first-seen order must hold"
        );
    }

    #[test]
    fn a_claim_is_recorded_once_per_audience_and_found_by_reverse_index() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        let act_ref = act.outward_act_ref.clone();
        let evidence = vec!["ev-1".to_string()];

        let first = store
            .record_assertion_use(
                &scope,
                &act_ref,
                "claim-arr",
                "investor@example.com",
                &evidence,
                &[],
                "t1",
            )
            .expect("first use");

        // Same triple again — one row, not two.
        let repeat = store
            .record_assertion_use(
                &scope,
                &act_ref,
                "claim-arr",
                "investor@example.com",
                &evidence,
                &[],
                "t2",
            )
            .expect("repeat");
        assert_eq!(repeat.assertion_use_id, first.assertion_use_id);
        assert_eq!(
            repeat.recorded_at, "t1",
            "the repeat must return the ORIGINAL row, not restamp it"
        );

        // A different audience is a different assertion, because who was told
        // is the thing correction propagation needs.
        let other_audience = store
            .record_assertion_use(
                &scope,
                &act_ref,
                "claim-arr",
                "other@example.com",
                &evidence,
                &[],
                "t3",
            )
            .expect("other audience");
        assert_ne!(other_audience.assertion_use_id, first.assertion_use_id);

        // The reverse lookup: a changed claim finds both.
        let by_claim = store.index_entries(&scope, "claim", "claim-arr").unwrap();
        assert_eq!(by_claim.len(), 2);
        assert!(by_claim.contains(&first.assertion_use_id));
        assert!(by_claim.contains(&other_audience.assertion_use_id));

        // And by the evidence underneath it.
        assert_eq!(
            store
                .index_entries(&scope, "evidence", "ev-1")
                .unwrap()
                .len(),
            2
        );
    }

    /// §9: "An observed-channel record never asserts pre-authorisation."
    ///
    /// It records that something WAS said, keeps its `observed` mark, and cannot
    /// be created through the prepare path — because on an observed channel
    /// there is no prepare step that could have failed closed.
    #[test]
    fn an_observed_statement_never_claims_it_was_pre_authorised() {
        let (_tmp, store, scope) = store();
        let mut request = email_request("meeting-1");
        request.channel = OutwardChannel::Meeting;

        // Preparing an observed channel is refused: there is nothing to prepare.
        let refused = store.prepare(&scope, &request, "t0");
        assert!(
            refused.is_err(),
            "a meeting cannot be prepared before the words leave"
        );

        let observed = store
            .record_observed_act(&scope, &request, "t0")
            .expect("observed");
        assert!(
            observed.observed,
            "the record must carry its own uncertainty"
        );
        assert_eq!(
            observed.status,
            OutwardActStatus::ProviderAccepted,
            "it happened — but via the observed path, which never passed a prepare gate"
        );

        // And the reverse: a controlled channel cannot be back-filled as observed.
        let controlled = store.record_observed_act(&scope, &email_request("send-1"), "t0");
        assert!(
            controlled.is_err(),
            "a controlled act must be prepared before it, not observed after it"
        );
    }

    /// A transition or a claim cannot attach to an act that was never recorded.
    /// That is the fail-closed property seen from the other side: if preparation
    /// did not happen, nothing downstream can pretend it did.
    ///
    /// The ref here is **well-formed and unprepared**, which is the case the
    /// property is about. An earlier version used `"act-nonexistent"`, and once
    /// `act_path` began refusing an underived shape that value stopped reaching
    /// the existence check at all — the three assertions would have passed on
    /// the malformed-ref refusal alone, and deleting the "act must exist" rule
    /// would not have failed this test.
    #[test]
    fn nothing_can_attach_to_an_act_that_was_never_prepared() {
        let (_tmp, store, scope) = store();
        let unprepared = derive_act_ref(&scope, "an-idempotency-key-nobody-prepared");
        assert!(
            is_derived_act_ref(&unprepared),
            "the fixture must clear the shape check, or it tests the wrong refusal"
        );

        assert!(store.mark_dispatching(&scope, &unprepared, "t0").is_err());
        assert!(store
            .record_provider_receipt(&scope, &unprepared, "r", "t0")
            .is_err());
        assert!(store
            .record_assertion_use(&scope, &unprepared, "c", "a", &[], &[], "t0")
            .is_err());
    }

    /// REGRESSION GUARD against a fix that was put in the wrong place.
    ///
    /// An attempt to close the "underived ref reads as `DidNotFire`" hazard made
    /// `act_path` itself refuse an underived ref. That broke four unrelated
    /// callers, because reading an act by an id that turns out not to BE an act
    /// is a supported question here and callers use the `None` as their
    /// discriminator — `recipient_compliance`'s duplicate scan counts the misses
    /// in a variable called `not_an_act`, and `acts_for_audience` would have let
    /// ONE corrupt index entry fail an entire relationship read.
    ///
    /// The guard now lives in `reconcile_outward_effect`, the one reader that
    /// turns absence into a licence to re-send. This test pins the store's half:
    /// absence stays an ANSWER here, not an error.
    #[test]
    fn a_malformed_index_entry_is_unresolved_rather_than_fatal() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::person("dana@counterparty.test");
        store
            .prepare_for_audience(&scope, &audience_request("person-1"), &audience, "t0")
            .expect("prepare");
        store
            .append_index(
                &scope,
                audience_axis(AudienceKind::Person),
                &audience.id,
                "act-not-a-derived-ref",
            )
            .expect("malformed entry");

        let answer = store
            .acts_for_audience(&scope, &audience, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("a corrupt entry must not fail the whole read");
        assert_eq!(
            answer.unresolved, 1,
            "the malformed entry counts as unresolved"
        );

        // The store answers, rather than refusing. `reconciliation_from_act` is
        // where an underived ref is refused — see
        // `a_ref_one_character_off_reads_a_real_send_as_evidence_that_nothing_left`
        // in `outward_settle`, which pins that half.
        assert!(
            store
                .load_act(&scope, "act-not-a-derived-ref")
                .expect("an id that is not an act is a question, not an error")
                .is_none(),
            "an underived ref names no record, and saying so is the answer callers rely on"
        );
    }

    /// Scopes do not collide on a shared idempotency key, and one scope's
    /// disclosures are not visible from another's index.
    #[test]
    fn one_scopes_disclosures_are_not_another_scopes() {
        let (_tmp, store, scope) = store();
        let other = OutwardScope::new("anonymous", "other-workspace");

        let mine = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        let theirs = store
            .prepare(&other, &email_request("send-1"), "t0")
            .unwrap();

        assert_ne!(
            mine.outward_act_ref, theirs.outward_act_ref,
            "the same idempotency key in two scopes must not resolve to one act"
        );
        assert!(store
            .load_act(&other, &mine.outward_act_ref)
            .unwrap()
            .is_none());
        assert!(store
            .index_entries(&other, "recipient", "investor@example.com")
            .unwrap()
            .iter()
            .all(|entry| entry != &mine.outward_act_ref));
    }

    /// The dispatch write point: the payload is content-addressed, the audience
    /// is whatever the caller resolved, and an identical dispatch resumes one
    /// record.
    ///
    /// The audience is passed IN rather than parsed here, and that is the whole
    /// point — a skill template can widen the recipient list through a
    /// passthrough, so anything parsed from the payload's typed fields would
    /// name a subset.
    #[test]
    fn a_dispatch_records_the_resolved_audience_and_an_immutable_payload() {
        let (_tmp, store, scope) = store();
        let payload = br#"{"to":"first@example.com","subject":"hi"}"#;
        // What the caller resolved: FIVE recipients, four of which the typed
        // field does not mention.
        let resolved = vec![
            "first@example.com".to_string(),
            "second@example.com".to_string(),
            "third@example.com".to_string(),
        ];

        let act = store
            .prepare_dispatch(
                &scope,
                "agentmail-send",
                "send",
                OutwardChannel::Email,
                "company-assistant",
                payload,
                resolved.clone(),
                None,
                crate::magician_v2::agents::ConsequenceClass::BoundedCommunication,
                "t0",
            )
            .expect("prepare dispatch");

        assert_eq!(
            act.intended_audience, resolved,
            "the record must name everyone the act reaches, not the typed subset"
        );
        assert!(act
            .exact_payload_artifact_ref
            .starts_with("payload://blake3:"));
        assert_eq!(act.status, OutwardActStatus::Prepared);

        // The payload is retrievable and is exactly what was dispatched.
        let stored = store
            .load_payload(&scope, &act.exact_payload_artifact_ref)
            .expect("load payload")
            .expect("payload present");
        assert_eq!(stored.as_bytes(), payload);

        // Same dispatch again resumes the same record.
        let again = store
            .prepare_dispatch(
                &scope,
                "agentmail-send",
                "send",
                OutwardChannel::Email,
                "company-assistant",
                payload,
                resolved,
                None,
                crate::magician_v2::agents::ConsequenceClass::BoundedCommunication,
                "t1",
            )
            .expect("retry");
        assert_eq!(again.outward_act_ref, act.outward_act_ref);
        assert_eq!(again.prepared_at, "t0");

        // A different payload is a different act — the key folds the payload
        // ref in, so an edited draft cannot resume the record of the old one.
        let edited = store
            .prepare_dispatch(
                &scope,
                "agentmail-send",
                "send",
                OutwardChannel::Email,
                "company-assistant",
                br#"{"to":"first@example.com","subject":"hi again"}"#,
                vec!["first@example.com".to_string()],
                None,
                crate::magician_v2::agents::ConsequenceClass::BoundedCommunication,
                "t2",
            )
            .expect("edited");
        assert_ne!(edited.outward_act_ref, act.outward_act_ref);
    }

    /// §9: "A claim change returns every disclosure carrying it, by reverse
    /// index." Across acts and audiences, with the act's status attached —
    /// because what to do about a disclosure depends on whether it reached
    /// anyone.
    #[test]
    fn a_changed_claim_finds_every_disclosure_that_carried_it() {
        let (_tmp, store, scope) = store();

        // Two acts, three audiences, one shared claim.
        let first = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        store
            .mark_dispatching(&scope, &first.outward_act_ref, "t1")
            .unwrap();
        store
            .record_provider_receipt(&scope, &first.outward_act_ref, "r1", "t2")
            .unwrap();

        let mut second_request = email_request("send-2");
        second_request.intended_audience = vec!["techstars@example.com".to_string()];
        second_request.exact_payload_artifact_ref = "artifact://deck@rev2".to_string();
        let second = store.prepare(&scope, &second_request, "t3").unwrap();
        store
            .record_delivered(&scope, &second.outward_act_ref, "t4")
            .unwrap();

        for (act_ref, audience) in [
            (&first.outward_act_ref, "yc@example.com"),
            (&first.outward_act_ref, "angel@example.com"),
            (&second.outward_act_ref, "techstars@example.com"),
        ] {
            store
                .record_assertion_use(
                    &scope,
                    act_ref,
                    "claim-arr",
                    audience,
                    &["ev-1".to_string()],
                    &[],
                    "t5",
                )
                .unwrap();
        }
        // An unrelated claim must not come back.
        store
            .record_assertion_use(
                &scope,
                &first.outward_act_ref,
                "claim-headcount",
                "yc@example.com",
                &[],
                &[],
                "t5",
            )
            .unwrap();

        let affected = store
            .disclosures_carrying_claim(&scope, "claim-arr")
            .unwrap();

        assert_eq!(
            affected.len(),
            3,
            "one row per (act, audience) that carried it"
        );
        assert!(affected
            .iter()
            .all(|row| row.approved_claim_ref == "claim-arr"));
        let audiences: std::collections::HashSet<&str> =
            affected.iter().map(|row| row.audience.as_str()).collect();
        assert!(audiences.contains("yc@example.com"));
        assert!(audiences.contains("techstars@example.com"));

        // The payload that carried it, not whatever the artifact says now.
        let techstars = affected
            .iter()
            .find(|row| row.audience == "techstars@example.com")
            .unwrap();
        assert_eq!(techstars.exact_payload_artifact_ref, "artifact://deck@rev2");
        assert_eq!(techstars.status, OutwardActStatus::Delivered);

        // The other direction: what did we say on the strength of this source?
        assert_eq!(
            store
                .disclosures_resting_on_evidence(&scope, "ev-1")
                .unwrap()
                .len(),
            3
        );
    }

    /// §9: "A retraction raises an obligation per affected active disclosure and
    /// alters no prior record."
    ///
    /// Both halves are asserted, and the second is the one that would rot
    /// quietly: an audit trail a later event can edit is not an audit trail.
    #[test]
    fn a_retraction_raises_obligations_and_alters_nothing() {
        let (_tmp, store, scope) = store();

        // One delivered act (told someone) and one failed act (told nobody).
        let delivered = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        store
            .record_delivered(&scope, &delivered.outward_act_ref, "t1")
            .unwrap();
        let mut failed_request = email_request("send-2");
        failed_request.intended_audience = vec!["bounced@example.com".to_string()];
        let failed = store.prepare(&scope, &failed_request, "t2").unwrap();
        store
            .mark_failed(&scope, &failed.outward_act_ref, "hard bounce", "t3")
            .unwrap();

        let live_use = store
            .record_assertion_use(
                &scope,
                &delivered.outward_act_ref,
                "claim-arr",
                "yc@example.com",
                &[],
                &[],
                "t4",
            )
            .unwrap();
        store
            .record_assertion_use(
                &scope,
                &failed.outward_act_ref,
                "claim-arr",
                "bounced@example.com",
                &[],
                &[],
                "t4",
            )
            .unwrap();

        // Capture prior state so "alters no prior record" is checked, not assumed.
        let act_before = store
            .load_act(&scope, &delivered.outward_act_ref)
            .unwrap()
            .unwrap();
        let use_before = store
            .load_assertion_use(&scope, &live_use.assertion_use_id)
            .unwrap()
            .unwrap();

        let raised = store
            .raise_correction_obligations(&scope, "claim-arr", "correction-1", "t5")
            .unwrap();

        assert_eq!(
            raised.len(),
            1,
            "only the delivered act owes anything; the failed one told nobody"
        );
        let obligation = &raised[0];
        assert_eq!(obligation.outward_act_ref, delivered.outward_act_ref);
        assert_eq!(obligation.state, ObligationState::Open);
        assert_eq!(
            obligation.disclosure_status_at_raise,
            OutwardActStatus::Delivered
        );

        // Nothing prior moved.
        assert_eq!(
            store
                .load_act(&scope, &delivered.outward_act_ref)
                .unwrap()
                .unwrap(),
            act_before,
            "raising an obligation edited the act it points at"
        );
        assert_eq!(
            store
                .load_assertion_use(&scope, &live_use.assertion_use_id)
                .unwrap()
                .unwrap(),
            use_before,
            "raising an obligation edited the assertion use it points at"
        );
    }

    /// An act that MIGHT have reached someone owes a check. Held separately
    /// because it is the case most likely to be quietly dropped: it is neither
    /// clearly sent nor clearly not, and the tempting simplification is to treat
    /// it as harmless.
    #[test]
    fn an_unresolved_dispatch_still_owes_a_correction() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        store
            .mark_dispatching(&scope, &act.outward_act_ref, "t1")
            .unwrap();
        store
            .mark_dispatch_unknown(&scope, &act.outward_act_ref, "no receipt lookup", "t2")
            .unwrap();
        store
            .record_assertion_use(
                &scope,
                &act.outward_act_ref,
                "claim-arr",
                "yc@example.com",
                &[],
                &[],
                "t3",
            )
            .unwrap();

        let raised = store
            .raise_correction_obligations(&scope, "claim-arr", "correction-1", "t4")
            .unwrap();

        assert_eq!(raised.len(), 1);
        assert_eq!(
            raised[0].disclosure_status_at_raise,
            OutwardActStatus::DispatchUnknown
        );
    }

    /// A record that never left owes nothing. Pinned so a future widening of
    /// `is_active_disclosure` cannot start inventing debts for unsent drafts.
    #[test]
    fn a_prepared_act_that_never_left_owes_nothing() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        store
            .record_assertion_use(
                &scope,
                &act.outward_act_ref,
                "claim-arr",
                "yc@example.com",
                &[],
                &[],
                "t1",
            )
            .unwrap();

        let raised = store
            .raise_correction_obligations(&scope, "claim-arr", "correction-1", "t2")
            .unwrap();
        assert!(raised.is_empty(), "a prepared act told nobody anything");
    }

    /// Raising the same correction twice must not double the debt, and
    /// resolving must append rather than edit.
    #[test]
    fn obligations_do_not_double_and_resolution_is_appended() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &email_request("send-1"), "t0")
            .unwrap();
        store
            .record_delivered(&scope, &act.outward_act_ref, "t1")
            .unwrap();
        store
            .record_assertion_use(
                &scope,
                &act.outward_act_ref,
                "claim-arr",
                "yc@example.com",
                &[],
                &[],
                "t2",
            )
            .unwrap();

        let first = store
            .raise_correction_obligations(&scope, "claim-arr", "correction-1", "t3")
            .unwrap();
        let again = store
            .raise_correction_obligations(&scope, "claim-arr", "correction-1", "t4")
            .unwrap();

        assert_eq!(first.len(), 1);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].obligation_id, first[0].obligation_id);
        assert_eq!(
            again[0].raised_at, "t3",
            "a repeat raise must return the ORIGINAL obligation, not restamp it"
        );
        assert_eq!(
            store
                .obligations_for_correction(&scope, "correction-1")
                .unwrap()
                .len(),
            1
        );

        // "Nothing" is a legitimate outcome — the store finds what is affected,
        // it does not decide (plan §10).
        assert_eq!(
            store
                .open_obligations_for_correction(&scope, "correction-1")
                .unwrap()
                .len(),
            1
        );
        store
            .resolve_obligation(
                &scope,
                &first[0].obligation_id,
                "no action: figure unchanged for this audience",
                "t5",
            )
            .unwrap();

        let resolved = store
            .load_obligation(&scope, &first[0].obligation_id)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.state, ObligationState::Resolved);
        assert_eq!(resolved.resolved_at.as_deref(), Some("t5"));
        assert_eq!(
            resolved.raised_at, "t3",
            "resolution must append, leaving when it was raised intact"
        );
        assert!(store
            .open_obligations_for_correction(&scope, "correction-1")
            .unwrap()
            .is_empty());
    }

    /// A failed act told nobody anything; a delivered one did. Correction
    /// obligations (plan phase 4) will run over the active set, so the
    /// distinction is pinned here where it is defined.
    #[test]
    fn only_acts_that_may_have_reached_someone_are_active_disclosures() {
        for status in [
            OutwardActStatus::Dispatching,
            OutwardActStatus::ProviderAccepted,
            OutwardActStatus::Delivered,
            OutwardActStatus::DispatchUnknown,
            OutwardActStatus::Corrected,
        ] {
            assert!(status.is_active_disclosure(), "{status:?} should be active");
        }
        for status in [
            OutwardActStatus::Prepared,
            OutwardActStatus::Failed,
            OutwardActStatus::Retracted,
        ] {
            assert!(
                !status.is_active_disclosure(),
                "{status:?} reached nobody or was withdrawn"
            );
        }
    }

    /// An act performed for an audience must be findable by that audience and
    /// must NOT appear under the work axis of the same word.
    ///
    /// The failure this pins is the one an earlier data-room bridge shipped: a
    /// room's audience id is a counterparty id (the counterparty register is
    /// what resolves it), and copying it into `engagement_id` filed a
    /// counterparty under the axis that answers *"performed inside engagement
    /// X"*. `RecipientInEngagement` and every other reverse lookup would then
    /// read that act as evidence about an engagement nobody ever opened.
    #[test]
    fn an_audience_files_under_its_own_axis_and_never_the_work_axis() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::engagement("counterparty-acme");
        let request = audience_request("room-1");

        let act = store
            .prepare_for_audience(&scope, &request, &audience, "t0")
            .expect("prepare");

        assert_eq!(
            store
                .index_entries(
                    &scope,
                    audience_axis(AudienceKind::Engagement),
                    "counterparty-acme"
                )
                .expect("audience axis"),
            vec![act.outward_act_ref.clone()],
            "the audience the act was performed for is how it must be found"
        );
        assert!(
            store
                .index_entries(
                    &scope,
                    WorkContextKind::ENGAGEMENT_TOKEN,
                    "counterparty-acme"
                )
                .expect("work axis")
                .is_empty(),
            "a counterparty id under the engagement work axis is a false statement of fact: it \
             claims the act was performed inside an engagement of that id"
        );
        assert!(
            store
                .act_refs_under_axis(&scope, WorkContextKind::ENGAGEMENT_TOKEN)
                .expect("work axis listing")
                .is_empty(),
            "no work was named, so no work axis may hold this act"
        );
        assert_eq!(
            (act.engagement_id.as_deref(), act.program_id.as_deref()),
            (None, None),
            "the record must not claim a work it was never told about"
        );
        // Still filed under everything unconditional, so nothing was traded
        // away for the audience filing.
        assert_eq!(
            store
                .act_refs_under_axis(&scope, ARTIFACT_AXIS)
                .expect("artifact axis"),
            vec![act.outward_act_ref],
        );
    }

    /// Every audience kind gets its own axis, and none of them is a work axis.
    ///
    /// Pins the two ways a kind could be lost: sharing an axis with another
    /// kind (an account's acts answering a panel's lookup — the merge
    /// `AudienceRef::as_key` exists to prevent, arriving through the index
    /// instead), or colliding with `WorkContextKind`'s tokens, which would put
    /// two id spaces in one directory. Walks `AudienceKind::ALL`, so a kind
    /// added later is tested the day it is declared.
    #[test]
    fn every_audience_kind_files_under_a_distinct_non_work_axis() {
        let mut seen = std::collections::HashSet::new();
        for kind in AudienceKind::ALL {
            let axis = audience_axis(kind);
            assert!(
                seen.insert(axis),
                "`{axis}` is used by two audience kinds, so one kind's acts would answer the \
                 other's reverse lookup"
            );
            assert!(
                !WorkContextKind::KIND_TOKENS.contains(&axis),
                "`{axis}` is also a work axis: an audience id and a work id would share one \
                 index file"
            );
            for reserved in [
                ARTIFACT_AXIS,
                RECIPIENT_AXIS,
                CLAIM_AXIS,
                EVIDENCE_AXIS,
                PROVIDER_MESSAGE_AXIS,
            ] {
                assert_ne!(
                    axis, reserved,
                    "`{axis}` is already this store's `{reserved}` axis"
                );
            }
        }
    }

    /// A re-swept room re-prepares every pair it already recorded. That must
    /// cost one index entry, not one per sweep: the reader de-duplicates, so
    /// unbounded appends would keep answering correctly while making every read
    /// of that index slower forever.
    #[test]
    fn re_preparing_an_audience_act_files_one_entry_not_two() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::panel("audit-2026");
        let request = audience_request("room-2");

        let first = store
            .prepare_for_audience(&scope, &request, &audience, "t0")
            .expect("prepare");
        let second = store
            .prepare_for_audience(&scope, &request, &audience, "t1")
            .expect("re-prepare");
        assert_eq!(
            first.outward_act_ref, second.outward_act_ref,
            "a retry resumes one record"
        );

        // The raw file, not `index_entries` — that reader de-duplicates, so it
        // would report one entry over a file that had grown to a hundred lines.
        let path = store.index_path(&scope, audience_axis(AudienceKind::Panel), "audit-2026");
        let raw = store
            .read_to_string_if_present(&path)
            .expect("read")
            .expect("the index file exists");
        assert_eq!(
            raw.lines().filter(|line| !line.trim().is_empty()).count(),
            1,
            "the second prepare appended a duplicate line: the index grows on every sweep"
        );
    }

    /// A crash between the act's row and its audience entry must be repaired by
    /// the next sweep, not made permanent.
    ///
    /// The act row is written first here (the reverse of `prepare`'s internal
    /// order), which is only safe because the audience append is reached on the
    /// resume path too — `prepare` early-returns the existing act and this
    /// still files it. Pinned by reproducing the window with a bare `prepare`.
    #[test]
    fn an_act_recorded_without_its_audience_entry_is_repaired_on_the_next_call() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::account("acme-group");
        let request = audience_request("room-3");

        // The window: the row exists, the audience axis does not.
        let crashed = store.prepare(&scope, &request, "t0").expect("prepare");
        assert!(
            store
                .index_entries(&scope, audience_axis(AudienceKind::Account), "acme-group")
                .expect("audience axis")
                .is_empty(),
            "the window this test exists for did not happen"
        );

        let resumed = store
            .prepare_for_audience(&scope, &request, &audience, "t1")
            .expect("resume");
        assert_eq!(resumed.outward_act_ref, crashed.outward_act_ref);
        assert_eq!(
            store
                .index_entries(&scope, audience_axis(AudienceKind::Account), "acme-group")
                .expect("audience axis"),
            vec![crashed.outward_act_ref],
            "a disclosure whose only record of who it was for is this entry would otherwise be \
             unfindable by that relationship for good"
        );
    }

    /// A blank audience id is refused rather than filed.
    ///
    /// Every unnamed audience hashes to one index file, so an empty id would
    /// pool unrelated relationships' acts into a single answer — and the caller
    /// would read that answer as *"acts performed for this relationship"*.
    #[test]
    fn an_audience_with_a_blank_id_is_refused() {
        let (_tmp, store, scope) = store();
        let request = audience_request("room-4");

        let error = store
            .prepare_for_audience(&scope, &request, &AudienceRef::person("   "), "t0")
            .expect_err("a blank id must refuse")
            .to_string();
        assert!(error.contains("person"), "{error}");
        assert!(
            store
                .load_act(&scope, &derive_act_ref(&scope, &request.idempotency_key))
                .expect("load")
                .is_none(),
            "the refusal must come before the record: an act filed under nobody is worse than no \
             act at all"
        );
    }

    /// A room grant names no work, so the work-axis re-index reports it as
    /// `unattributed` — and that count must not be read as *"not indexed"*.
    ///
    /// This is the honest half of the reviewer's complaint: the old bridge made
    /// these acts count as `attributed` by writing a counterparty id into
    /// `engagement_id`, which turned the one count that separates *"the index is
    /// stale"* from *"the history was never attributed"* into a confident lie.
    /// They now count as unattributed WHILE being fully findable by audience.
    #[test]
    fn an_audience_act_reads_as_unattributed_work_while_staying_findable() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::program("q3-intake");
        let act = store
            .prepare_for_audience(&scope, &audience_request("room-5"), &audience, "t0")
            .expect("prepare");

        let report = store.reindex_work_axes(&scope).expect("reindex");
        assert_eq!(
            (report.acts_seen, report.unattributed),
            (1, 1),
            "no work was named, and this re-index must not invent one"
        );
        assert_eq!(
            (
                report.engagement_entries,
                report.program_entries,
                report.already_indexed
            ),
            (0, 0, 0),
            "nothing was repaired, and the counts must reconcile with what was examined"
        );
        assert_eq!(
            store
                .index_entries(&scope, audience_axis(AudienceKind::Program), "q3-intake")
                .expect("audience axis"),
            vec![act.outward_act_ref],
            "unattributed by work is not unindexed: the audience entry is the act's filing"
        );
    }

    /// An act that names a work is indexed when it is WRITTEN, so the repair
    /// route must report it as already filed and append nothing.
    ///
    /// The failure this pins: an unconditional append doubled the index on the
    /// first run and again on every re-run. The reader de-duplicates, so every
    /// answer stayed correct while every read of that index got slower — a
    /// regression a caller reading the 200 and a non-zero repair count would
    /// have read as success. It is also the only test that reaches the
    /// `already_indexed` branch at all; without it that counter, and the
    /// membership check it reports, are never executed.
    #[test]
    fn re_indexing_an_already_filed_act_counts_it_and_appends_nothing() {
        let (_tmp, store, scope) = store();
        let mut request = audience_request("work-1");
        request.engagement_id = Some("eng-1".to_string());
        let act = store.prepare(&scope, &request, "t0").expect("prepare");

        let report = store.reindex_work_axes(&scope).expect("reindex");
        assert_eq!(
            (
                report.acts_seen,
                report.engagement_entries,
                report.program_entries,
                report.already_indexed,
                report.unattributed
            ),
            (1, 0, 0, 1, 0),
            "the act was filed when it was written, so this run repaired nothing — and the \
             counts must still add up to the one act examined"
        );

        // The raw file, not `index_entries` — that reader de-duplicates, so it
        // would report one entry over a file that had grown to a hundred lines.
        let path = store.index_path(&scope, WorkContextKind::ENGAGEMENT_TOKEN, "eng-1");
        let raw = store
            .read_to_string_if_present(&path)
            .expect("read")
            .expect("the index file exists");
        assert_eq!(
            raw.lines().filter(|line| !line.trim().is_empty()).count(),
            1,
            "the repair appended a duplicate: the index grows every time somebody runs it"
        );
        assert_eq!(
            store
                .index_entries(&scope, WorkContextKind::ENGAGEMENT_TOKEN, "eng-1")
                .expect("work axis"),
            vec![act.outward_act_ref],
            "the entry the repair left alone is still the one the axis answers with"
        );
    }

    /// The question an audience exists to answer, asked.
    ///
    /// The failure this pins is an island in the READ direction:
    /// `prepare_for_audience` filed every room grant under the audience axis
    /// and nothing outside this crate's tests read the axis, so *"what have we
    /// told this counterparty"* had no answer at all — and the acts were
    /// deliberately absent from the work axes, which is where anybody looking
    /// would have looked next and found nothing.
    #[test]
    fn an_audiences_acts_are_reachable_by_the_relationship_they_were_performed_for() {
        let (_tmp, store, scope) = store();
        let acme = AudienceRef::engagement("counterparty-acme");
        let other = AudienceRef::engagement("counterparty-globex");

        let first = store
            .prepare_for_audience(&scope, &audience_request("acme-1"), &acme, "t0")
            .expect("prepare");
        let second = store
            .prepare_for_audience(&scope, &audience_request("acme-2"), &acme, "t1")
            .expect("prepare");
        store
            .prepare_for_audience(&scope, &audience_request("globex-1"), &other, "t2")
            .expect("prepare");

        let answer = store
            .acts_for_audience(&scope, &acme, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");

        let mut expected = vec![
            first.outward_act_ref.clone(),
            second.outward_act_ref.clone(),
        ];
        expected.sort();
        assert_eq!(
            answer
                .acts
                .iter()
                .map(|act| act.outward_act_ref.clone())
                .collect::<Vec<_>>(),
            expected,
            "the acts performed for this counterparty are exactly what this must answer with — \
             and another counterparty's act appearing here would put a correction in front of \
             the wrong reader"
        );
        assert_eq!(
            (
                answer.indexed,
                answer.examined,
                answer.capped_at,
                answer.unresolved
            ),
            (2, 2, None, 0),
            "nothing was capped and nothing was lost, so the denominators must say so"
        );
        assert_eq!(
            (
                answer.confirmed_by_record,
                answer.audience_not_on_record,
                answer.recorded_for_another_audience
            ),
            (2, 0, 0),
            "both records name this audience themselves, so the partition must put both in the \
             one bucket that needs no caveat"
        );
        assert_eq!(
            answer.examined,
            answer.acts.len() + answer.unresolved,
            "the counts must reconcile with what was examined"
        );
        assert_eq!(
            answer.audience, acme,
            "the answer must say what it answered about"
        );
    }

    /// A partial answer must say it is partial.
    ///
    /// The failure this pins is the quiet one: a correction that reads a capped
    /// list as the whole list stops halfway and reports itself complete, and
    /// the disclosures past the cap are never chased. So `indexed` is the real
    /// size and `capped_at` says a cut was made — and which acts survive it is
    /// act-ref order, so the same query twice over an unchanged scope cannot
    /// answer with two different halves.
    #[test]
    fn a_capped_audience_read_reports_the_cap_instead_of_looking_complete() {
        let (_tmp, store, scope) = store();
        let panel = AudienceRef::panel("audit-2026");
        let mut all = Vec::new();
        for key in ["cap-1", "cap-2", "cap-3"] {
            all.push(
                store
                    .prepare_for_audience(&scope, &audience_request(key), &panel, "t0")
                    .expect("prepare")
                    .outward_act_ref,
            );
        }
        all.sort();

        let answer = store
            .acts_for_audience(&scope, &panel, 2, None)
            .expect("read");
        assert_eq!(
            (answer.indexed, answer.examined, answer.capped_at),
            (3, 2, Some(2)),
            "three are filed and two were read: an answer that reported only the two would be \
             read as `we told this panel two things`"
        );
        assert_eq!(
            answer
                .acts
                .iter()
                .map(|act| act.outward_act_ref.clone())
                .collect::<Vec<_>>(),
            all[..2].to_vec(),
            "the cap must fall in a stable place, or two reads of an unchanged scope disagree"
        );

        // The same read with room for all three reports no cap at all — so
        // `capped_at` tracks the answer and is not merely the limit echoed.
        let whole = store
            .acts_for_audience(&scope, &panel, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");
        assert_eq!(
            (whole.indexed, whole.examined, whole.capped_at),
            (3, 3, None),
            "nothing was dropped, so nothing may be reported as dropped"
        );
    }

    /// An audience with more filings than one page holds can be read WHOLE.
    ///
    /// The cap alone could only ever show the first N. Without a cursor, an
    /// audience past the limit was unenumerable by any caller — and on a
    /// correction, an act nobody can reach is a room nobody flags.
    ///
    /// Paging is asserted to terminate and to return each act exactly once.
    /// Both matter: a cursor that never clears `next_after` loops for ever, and
    /// one that re-serves its own last row double-counts a disclosure.
    #[test]
    fn a_relationship_larger_than_one_page_can_still_be_read_whole() {
        let (_tmp, store, scope) = store();
        let panel = AudienceRef::panel("audit-2026");
        let mut all = Vec::new();
        for key in ["page-1", "page-2", "page-3", "page-4", "page-5"] {
            all.push(
                store
                    .prepare_for_audience(&scope, &audience_request(key), &panel, "t0")
                    .expect("prepare")
                    .outward_act_ref,
            );
        }
        all.sort();

        let mut seen: Vec<String> = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..10 {
            let page = store
                .acts_for_audience(&scope, &panel, 2, after.as_deref())
                .expect("read");
            assert_eq!(
                page.indexed, 5,
                "`indexed` is the whole relationship on every page, not what this page holds —                  a caller must be able to see how much of it they are looking at"
            );
            seen.extend(page.acts.iter().map(|act| act.outward_act_ref.clone()));
            after = page.next_after.clone();
            if after.is_none() {
                break;
            }
        }
        assert!(
            after.is_none(),
            "paging must terminate: a cursor that never clears loops for ever"
        );
        assert_eq!(
            seen, all,
            "every act exactly once, in the same order the index sorts — a cursor that re-serves              its own last row would double-count a disclosure"
        );
    }

    /// An act whose record carries no audience is still this audience's act.
    ///
    /// The shape is live, not hypothetical: `prepare` writes the row with no
    /// audience, and `prepare_for_audience` on the same idempotency key resumes
    /// that row untouched — the register is append-only — and adds only the
    /// filing. A row written before the record field existed reads the same way
    /// once anything files it. Reading that empty field as *"performed for
    /// nobody"* would drop a real disclosure out of a correction, so it is
    /// counted apart and still answered.
    #[test]
    fn an_act_filed_by_the_resume_path_answers_though_its_record_names_no_audience() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::account("acme-group");
        let request = audience_request("resume-1");

        let crashed = store.prepare(&scope, &request, "t0").expect("prepare");
        assert_eq!(
            crashed.audience, None,
            "the window this test exists for did not happen"
        );
        let resumed = store
            .prepare_for_audience(&scope, &request, &audience, "t1")
            .expect("resume");
        assert_eq!(
            resumed.audience, None,
            "an append-only row is not rewritten"
        );

        let answer = store
            .acts_for_audience(&scope, &audience, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");
        assert_eq!(
            answer
                .acts
                .iter()
                .map(|act| act.outward_act_ref.clone())
                .collect::<Vec<_>>(),
            vec![crashed.outward_act_ref],
            "the index entry is the only surviving statement of who this act was for; dropping \
             it here would lose the disclosure entirely"
        );
        assert_eq!(
            (
                answer.confirmed_by_record,
                answer.audience_not_on_record,
                answer.recorded_for_another_audience
            ),
            (0, 1, 0),
            "the record says nothing about the audience, and the answer must say THAT rather \
             than claiming the record agreed"
        );
    }

    /// One key prepared for a second audience is filed under both and names
    /// only the first — and the reader has to say so.
    ///
    /// Reachable because the act ref derives from `(scope, idempotency_key)`:
    /// the second prepare resumes the first record, audience and all, then
    /// files it under the second audience's axis. Both filings are true. An
    /// owner deciding whether a correction is owed to the second counterparty
    /// needs to know the record itself never said so.
    #[test]
    fn one_key_prepared_for_two_audiences_answers_both_and_flags_the_disagreement() {
        let (_tmp, store, scope) = store();
        let first = AudienceRef::engagement("counterparty-acme");
        let second = AudienceRef::panel("audit-2026");
        let request = audience_request("shared-key");

        let act = store
            .prepare_for_audience(&scope, &request, &first, "t0")
            .expect("prepare");
        let resumed = store
            .prepare_for_audience(&scope, &request, &second, "t1")
            .expect("re-prepare");
        assert_eq!(
            (resumed.outward_act_ref.as_str(), resumed.audience.as_ref()),
            (act.outward_act_ref.as_str(), Some(&first)),
            "one key is one record, and the record keeps the audience it was written with"
        );

        let owned = store
            .acts_for_audience(&scope, &first, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");
        assert_eq!(
            (
                owned.indexed,
                owned.confirmed_by_record,
                owned.recorded_for_another_audience
            ),
            (1, 1, 0),
            "the audience the record names must read as confirmed"
        );

        let borrowed = store
            .acts_for_audience(&scope, &second, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");
        assert_eq!(
            borrowed
                .acts
                .iter()
                .map(|held| held.outward_act_ref.clone())
                .collect::<Vec<_>>(),
            vec![act.outward_act_ref],
            "the second filing is real: this act was prepared for this panel too"
        );
        assert_eq!(
            (
                borrowed.confirmed_by_record,
                borrowed.audience_not_on_record,
                borrowed.recorded_for_another_audience
            ),
            (0, 0, 1),
            "the record names somebody else, and reporting this as confirmed would tell an owner \
             the disclosure said so"
        );
    }

    /// An index entry that resolves to no act is counted, never quietly dropped.
    ///
    /// `resolve_affected` skips a vanished row because it is chasing a claim
    /// through many, and failing the whole lookup would hide the disclosures
    /// that ARE affected. Here the entry is the only record that we told this
    /// audience anything, so a silent skip is the loss itself — and the counts
    /// would stop reconciling with what was examined.
    #[test]
    fn an_index_entry_pointing_at_no_act_is_counted_rather_than_dropped() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::person("dana@counterparty.test");
        let act = store
            .prepare_for_audience(&scope, &audience_request("person-1"), &audience, "t0")
            .expect("prepare");
        // Well-formed and pointing at nothing — which is what "dangling" means
        // here. The literal this replaced carried 31 hex digits, one short of
        // the derived width, so once the walk grew a shape check it would have
        // been counted unresolved by THAT and never reached the missing-file
        // path this test is named for. Derived from a key nothing prepared, so
        // it cannot drift out of shape when `stable_id` changes.
        let dangling = derive_act_ref(&scope, "an-act-that-was-never-written");
        assert!(
            is_derived_act_ref(&dangling),
            "the fixture must clear the shape check, or this tests the wrong path"
        );
        store
            .append_index(
                &scope,
                audience_axis(AudienceKind::Person),
                &audience.id,
                &dangling,
            )
            .expect("dangling entry");

        let answer = store
            .acts_for_audience(&scope, &audience, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect("read");
        assert_eq!(
            (answer.indexed, answer.examined, answer.unresolved),
            (2, 2, 1),
            "two entries were examined and one of them resolved to nothing: an answer of `1 act` \
             with no denominator hides that this store lost a disclosure it had filed"
        );
        assert_eq!(
            answer.examined,
            answer.acts.len() + answer.unresolved,
            "the outcomes must sum to what was examined"
        );
        assert_eq!(
            answer
                .acts
                .iter()
                .map(|held| held.outward_act_ref.clone())
                .collect::<Vec<_>>(),
            vec![act.outward_act_ref],
            "the act that does exist is still served"
        );
    }

    /// An index that cannot be read must not answer *"we told them nothing"*.
    ///
    /// The one wrong answer that ends an investigation. Reproduced by standing
    /// a directory where the index file goes, so the read fails with something
    /// that is not `NotFound` — the only condition `jsonl::read_log_if_present`
    /// is allowed to read as absence.
    #[test]
    fn an_unreadable_audience_index_refuses_rather_than_reporting_no_disclosures() {
        let (_tmp, store, scope) = store();
        let audience = AudienceRef::account("acme-group");
        let path = store.index_path(&scope, audience_axis(AudienceKind::Account), &audience.id);
        std::fs::create_dir_all(&path).expect("stand a directory where the index file belongs");

        let error = store
            .acts_for_audience(&scope, &audience, DEFAULT_AUDIENCE_ACT_LIMIT, None)
            .expect_err("an unreadable index is not an audience we have told nothing")
            .to_string();
        assert!(
            error.contains("unreadable"),
            "the refusal must say the log could not be READ; anything vaguer will be read as \
             `nothing found` by the next person: {error}"
        );
    }

    /// A blank audience and an unbounded limit are both refused.
    ///
    /// A blank id hashes to the file every unnamed audience shares, so serving
    /// it would answer with acts performed for somebody else's relationship —
    /// the same refusal the writer makes, made again on the way out. A zero
    /// limit would report `no acts` for an audience that has some, which is the
    /// exact lie this whole surface is built to prevent; an unbounded one turns
    /// a query string into one file open per entry.
    #[test]
    fn a_blank_audience_or_a_limit_outside_the_bound_is_refused() {
        let (_tmp, store, scope) = store();
        let blank = store
            .acts_for_audience(
                &scope,
                &AudienceRef::person("   "),
                DEFAULT_AUDIENCE_ACT_LIMIT,
                None,
            )
            .expect_err("a blank id must refuse")
            .to_string();
        assert!(blank.contains("person"), "{blank}");

        let named = AudienceRef::panel("audit-2026");
        for limit in [0, MAX_AUDIENCE_ACT_LIMIT + 1] {
            let error = store
                .acts_for_audience(&scope, &named, limit, None)
                .expect_err("a limit outside the bound must refuse")
                .to_string();
            assert!(
                error.contains("`limit`"),
                "limit {limit} was accepted or refused for the wrong reason: {error}"
            );
        }
    }

    /// A request carrying neither work field — the shape every audience-bound
    /// caller produces, because an audience id is not a work id.
    fn audience_request(key: &str) -> PrepareOutwardAct {
        PrepareOutwardAct {
            idempotency_key: key.to_string(),
            program_id: None,
            engagement_id: None,
            exact_payload_artifact_ref: format!("artifact://{key}@rev1"),
            effective_sender: "company-assistant".to_string(),
            intended_audience: vec!["holder@counterparty.test".to_string()],
            channel: OutwardChannel::Room,
            consequence_class: "confidential_disclosure".to_string(),
        }
    }
}
