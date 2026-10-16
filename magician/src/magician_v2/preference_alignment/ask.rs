//! The alignment ask: what an alignment hold persists with its pause, and
//! how an owner's reply to it is resolved (plan §10.1–§10.2, §11.1–§11.2;
//! P5.1, P5.2, P5.4, P5.6).
//!
//! # One interaction, on the existing pause
//!
//! A hold is an ordinary confirmation pause — the same canonical
//! `hitl.requested`, the same `POST /hitl/{correlation_id}/respond`, the same
//! ask identity, journal and restart recovery — carrying an
//! [`AlignmentAsk`] in its pause state. Keeping the pause kind
//! `confirmation` is deliberate: every client already renders and answers a
//! confirmation, and the reconnect and stateless-terminal paths only recover
//! pause kinds they know. A client that knows nothing about alignment still
//! gets a working approve (the ask's one consent choice) and deny (cancel).
//! A client that reads `input_schema.alignment` offers the typed choices and
//! free text.
//!
//! # Replies
//!
//! [`resolve_reply`] turns an authenticated reply into a [`Resolution`]. It is
//! structural: a choice is accepted only by an id the ask offered, and the
//! one bare affirmative an ask accepts is its single consent choice. Free
//! text that names no choice is the owner's own instruction: no receipt is
//! issued, the run continues with the reply as the owner's current turn, and
//! whatever the agent proposes next is checked again with that turn as
//! evidence. Free text never mints consent by itself.
//!
//! # Deadlines
//!
//! Every ask carries a response deadline. A reply after it is refused and the
//! act is cancelled: a timeout is never consent. Expiry is enforced when the
//! reply arrives (the same lazy enforcement the pause store uses for
//! collection windows); a background hold is also published with its deadline
//! so the owner gets an attention alert.
//!
//! # Audience
//!
//! The canonical `hitl.requested` event fans out to the chat escalation
//! listener, channel alerts, voice and the attention lifecycle log, so its
//! prompt is [`GENERIC_ASK_TEXT`] and its schema carries only choice ids and
//! non-revealing labels ([`AlignmentAsk::public_projection`]). The
//! owner-private detail ([`AskDetail`]: which preference, why, the exact
//! wording of a proposed change) stays in the pause record and is served only
//! by the owner-private read ([`AlignmentAsk::private_projection`], P6).
//!
//! # Lasting changes (P7.1)
//!
//! "Remember this as my preference" and "Change my preference" never write on
//! the first answer. They move the ask to a wording stage
//! ([`AskStage::ConfirmWording`]) that holds the exact proposed statement,
//! the wording it supersedes and the source revision it was read at
//! ([`LastingWrite`]). The confirmation must carry that stage's token, which
//! only the private projection shows: a bare "yes" from a shared surface, or
//! from a client that never displayed the wording, saves nothing. Neither
//! choice is consent for the held act.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::intention::{AssertionStatus, Intention, IntentionKind, IntentionSource};
use super::receipts::AlignmentReceiptPurpose;
use crate::magician_v2::agents::memory_lifecycle::digest;

pub const ALIGNMENT_ASK_SCHEMA_VERSION: &str = "alignment_ask.v1";

/// What every shared or unknown surface sees for an alignment hold. Names no
/// preference, quote, conflict category or alternative.
pub const GENERIC_ASK_TEXT: &str =
    "An action needs your review before it runs. Open Magician privately to see why and choose.";

/// The owner's own app surfaces. Only an answer from one of these may give a
/// hold's consent or confirm a lasting change: a shared or unknown audience
/// (a group bot, speaker voice, a plane terminal, an unknown channel) never
/// stands in for the owner's informed consent (plan §10.1).
pub const CONSENT_SURFACES: &[&str] = &["web", "ios", "android", "desktop"];

/// Whether an answer from `channel` may consent. An answer that names no
/// channel is the respond route's default, `web`.
pub fn is_consent_surface(channel: Option<&str>) -> bool {
    channel.map_or(true, |channel| CONSENT_SURFACES.contains(&channel.trim()))
}

/// What one offered choice does when chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    /// An informed exception for this one act; the intention stays.
    AllowOnce,
    /// Continue although the check could not run (plan §11.1).
    ProceedWithoutCheck,
    /// Run the check again.
    Retry,
    /// Do something else instead; the owner says what.
    UseAlternative,
    /// The owner wants the saved preference changed; carries the new wording.
    ChangePreference,
    /// Keep an inferred preference as an explicit one (P7.1).
    RememberPreference,
    /// Save the exact wording the private surface read back.
    ConfirmWording,
    /// Leave the preference as it is and go back to the held act's choices.
    KeepAsIs,
    /// Stop the act.
    Cancel,
}

impl ResolutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::ProceedWithoutCheck => "proceed_without_check",
            Self::Retry => "retry",
            Self::UseAlternative => "use_alternative",
            Self::ChangePreference => "change_preference",
            Self::RememberPreference => "remember_preference",
            Self::ConfirmWording => "confirm_wording",
            Self::KeepAsIs => "keep_as_is",
            Self::Cancel => "cancel",
        }
    }

    /// A choice that changes what Magician remembers rather than deciding
    /// the act. Clients show these apart from the act's choices, and never
    /// preselect them.
    pub fn is_lasting(self) -> bool {
        matches!(
            self,
            Self::ChangePreference | Self::RememberPreference | Self::ConfirmWording
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskChoice {
    /// Stable id a client answers with. Equal to the kind's name.
    pub id: String,
    pub kind: ResolutionKind,
    /// Non-revealing label, safe on any surface.
    pub label: String,
}

impl AskChoice {
    fn of(kind: ResolutionKind) -> Self {
        let label = match kind {
            ResolutionKind::AllowOnce => "Allow this once",
            ResolutionKind::ProceedWithoutCheck => "Proceed without the check",
            ResolutionKind::Retry => "Try the check again",
            ResolutionKind::UseAlternative => "Do something else instead",
            ResolutionKind::ChangePreference => "Change my preference",
            ResolutionKind::RememberPreference => "Remember this as my preference",
            ResolutionKind::ConfirmWording => "Save this wording",
            ResolutionKind::KeepAsIs => "Don't change it",
            ResolutionKind::Cancel => "Cancel",
        };
        Self {
            id: kind.as_str().to_string(),
            kind,
            label: label.to_string(),
        }
    }
}

/// One intention as a hold cites it. Owner-private: only the private
/// projection and the pause record carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskIntention {
    pub id: String,
    /// The source revision the judge read.
    pub revision: String,
    pub source: IntentionSource,
    pub kind: IntentionKind,
    pub assertion_status: AssertionStatus,
    pub statement: String,
}

impl From<&Intention> for AskIntention {
    fn from(intention: &Intention) -> Self {
        Self {
            id: intention.id.clone(),
            revision: intention.revision.clone(),
            source: intention.source.clone(),
            kind: intention.kind,
            assertion_status: intention.assertion_status,
            statement: intention.statement.clone(),
        }
    }
}

impl AskIntention {
    /// The native tier a lasting write goes back to. Profile and app sources
    /// are not written by alignment in v1 (plan §5.3).
    pub fn native_tier(&self) -> Option<&str> {
        match &self.source {
            IntentionSource::NativeTier { tier } => Some(tier),
            _ => None,
        }
    }
}

/// What the owner sees on a private surface: why the act was held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskDetail {
    /// The judge's brief, grounded explanation.
    #[serde(default)]
    pub explanation: String,
    /// The held act, in plain words.
    #[serde(default)]
    pub act_summary: String,
    /// The intentions the hold rests on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intentions: Vec<AskIntention>,
    /// An inferred intention the judge offered to remember.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<AskIntention>,
}

impl AskDetail {
    /// The intention a "change my preference" answer rewrites: the first
    /// cited one alignment can write.
    pub fn change_target(&self) -> Option<&AskIntention> {
        self.intentions
            .iter()
            .find(|intention| intention.native_tier().is_some())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LastingWriteKind {
    /// Mark an inferred record as the owner's confirmed preference. The
    /// wording is unchanged.
    Remember,
    /// Supersede a record with the owner's new wording.
    Replace,
}

/// A proposed lasting write, bound to the exact text and source revision the
/// owner is asked to confirm (plan §10.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastingWrite {
    pub kind: LastingWriteKind,
    pub intention_id: String,
    pub tier: String,
    /// The record's revision when the wording was read back. A different
    /// revision at write time voids the confirmation.
    pub expected_revision: String,
    /// For `Remember`, the statement as it stands; for `Replace`, the new
    /// value in the owner's own words.
    pub wording: String,
    /// The statement being replaced, read back with the new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<String>,
    /// Binds a confirmation to this exact proposal. Shown only privately.
    pub token: String,
}

impl LastingWrite {
    /// A remember proposal outside an ask (a nudge's offer), bound to `key`
    /// and the intention's current revision. `None` for a source alignment
    /// does not write.
    pub fn remember(key: &str, target: &AskIntention) -> Option<Self> {
        let tier = target.native_tier()?.to_string();
        Some(Self::new(
            key,
            LastingWriteKind::Remember,
            target,
            &tier,
            target.statement.clone(),
            None,
        ))
    }

    fn new(
        ask_id: &str,
        kind: LastingWriteKind,
        target: &AskIntention,
        tier: &str,
        wording: String,
        superseded: Option<String>,
    ) -> Self {
        let token = digest(&json!([
            ask_id,
            kind,
            target.id,
            target.revision,
            wording,
            superseded,
        ]))[..24]
            .to_string();
        Self {
            kind,
            intention_id: target.id.clone(),
            tier: tier.to_string(),
            expected_revision: target.revision.clone(),
            wording,
            superseded,
            token,
        }
    }
}

/// Another gate whose requirement an ask also presents (plan §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoveredGate {
    /// A configured approval rule matched the act.
    ApprovalRule,
    /// The act's criticality needs a human confirmation.
    Criticality,
    /// The model flagged the act as needing confirmation.
    ModelFlag,
    /// A harness call matched the grant's approval rules.
    PlaneApproval,
}

impl CoveredGate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRule => "approval_rule",
            Self::Criticality => "criticality",
            Self::ModelFlag => "model_flag",
            Self::PlaneApproval => "plane_approval",
        }
    }
}

/// One other requirement a combined ask presents, with the gate's own
/// reason. The reason is shown only on the owner-private surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveredRequirement {
    pub gate: CoveredGate,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskStage {
    /// Decide what happens to the held act.
    #[default]
    Decide,
    /// Confirm the exact wording of a lasting change.
    ConfirmWording,
}

/// Longest new wording a "change my preference" answer may carry.
pub const MAX_WORDING_CHARS: usize = 600;

/// The typed ask persisted with an alignment hold's pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentAsk {
    pub schema_version: String,
    pub ask_id: String,
    /// What the ask's one consent choice issues.
    pub purpose: AlignmentReceiptPurpose,
    /// `alignment_binding.v1` of the held act.
    pub binding_id: String,
    /// The run the ask was raised in; its answer covers this run only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub choices: Vec<AskChoice>,
    /// Intention ids the hold rests on (exception asks), or the instructions
    /// left unchecked (waiver asks). Ids only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intention_ids: Vec<String>,
    pub created_at_ms: i64,
    pub deadline_at_ms: i64,
    /// Raised by work nobody is watching (no chat session).
    #[serde(default)]
    pub background: bool,
    /// Owner-private: why the act was held. Never in the public projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<AskDetail>,
    #[serde(default)]
    pub stage: AskStage,
    /// The proposal a wording stage asks the owner to confirm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lasting_write: Option<LastingWrite>,
    /// The act's choices, kept while a wording stage is open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_choices: Vec<AskChoice>,
    /// Other gates' requirements this ask presents beside the alignment
    /// review (a combined ask, plan §9.4). Its consent choice resolves them
    /// too, because the ask names them; no other choice resolves any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_covers: Vec<CoveredRequirement>,
}

impl AlignmentAsk {
    /// An ask for a hold the judge decided (`allow_once` consent).
    ///
    /// `allow_once` and `cancel` are always offered. `use_alternative` and
    /// `change_preference` are offered when the judge's validated decision
    /// offered them (`change_preference` also stands for `correct_memory`),
    /// and `change_preference` only when a cited intention is one alignment
    /// can write. `remember_preference` is offered when the judge offered to
    /// remember an inferred native intention. The choices stay grounded in
    /// the decision.
    #[allow(clippy::too_many_arguments)]
    pub fn for_hold(
        ask_id: String,
        binding_id: String,
        execution_id: Option<String>,
        intention_ids: Vec<String>,
        judge_choices: &[super::contract::ResolutionChoice],
        created_at_ms: i64,
        deadline_secs: u64,
        background: bool,
        detail: Option<AskDetail>,
    ) -> Self {
        use super::contract::ResolutionChoice;
        let mut choices = vec![AskChoice::of(ResolutionKind::AllowOnce)];
        if judge_choices.contains(&ResolutionChoice::UseAlternative) {
            choices.push(AskChoice::of(ResolutionKind::UseAlternative));
        }
        let writable = detail
            .as_ref()
            .is_some_and(|detail| detail.change_target().is_some());
        if writable
            && judge_choices.iter().any(|choice| {
                matches!(
                    choice,
                    ResolutionChoice::ChangePreference | ResolutionChoice::CorrectMemory
                )
            })
        {
            choices.push(AskChoice::of(ResolutionKind::ChangePreference));
        }
        let rememberable = detail
            .as_ref()
            .and_then(|detail| detail.remember.as_ref())
            .is_some_and(|remember| {
                remember.native_tier().is_some()
                    && remember.assertion_status == AssertionStatus::Inferred
            });
        if rememberable {
            choices.push(AskChoice::of(ResolutionKind::RememberPreference));
        }
        choices.push(AskChoice::of(ResolutionKind::Cancel));
        let mut ask = Self::new(
            ask_id,
            AlignmentReceiptPurpose::Exception,
            binding_id,
            execution_id,
            choices,
            intention_ids,
            created_at_ms,
            deadline_secs,
            background,
        );
        ask.detail = detail;
        ask
    }

    /// An ask for a check that could not run (`proceed_without_check`
    /// consent, plan §11.1). Answering it needs no judge call.
    pub fn for_unresolved_check(
        ask_id: String,
        binding_id: String,
        execution_id: Option<String>,
        unchecked_ids: Vec<String>,
        created_at_ms: i64,
        deadline_secs: u64,
        background: bool,
    ) -> Self {
        Self::new(
            ask_id,
            AlignmentReceiptPurpose::CheckWaiver,
            binding_id,
            execution_id,
            vec![
                AskChoice::of(ResolutionKind::Retry),
                AskChoice::of(ResolutionKind::ProceedWithoutCheck),
                AskChoice::of(ResolutionKind::Cancel),
            ],
            unchecked_ids,
            created_at_ms,
            deadline_secs,
            background,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        ask_id: String,
        purpose: AlignmentReceiptPurpose,
        binding_id: String,
        execution_id: Option<String>,
        choices: Vec<AskChoice>,
        intention_ids: Vec<String>,
        created_at_ms: i64,
        deadline_secs: u64,
        background: bool,
    ) -> Self {
        let deadline_ms = i64::try_from(deadline_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        Self {
            schema_version: ALIGNMENT_ASK_SCHEMA_VERSION.to_string(),
            ask_id,
            purpose,
            binding_id,
            execution_id,
            choices,
            intention_ids,
            created_at_ms,
            deadline_at_ms: created_at_ms.saturating_add(deadline_ms),
            background,
            detail: None,
            stage: AskStage::Decide,
            lasting_write: None,
            held_choices: Vec::new(),
            also_covers: Vec::new(),
        }
    }

    /// Present `covers` in this ask as well: one ask instead of an approval
    /// and then a review. The consent choice is relabelled to say it
    /// approves the act too, so the owner sees what the one answer settles.
    pub fn with_also_covers(mut self, covers: Vec<CoveredRequirement>) -> Self {
        if covers.is_empty() {
            return self;
        }
        let consent = self.consent_kind();
        let label = match consent {
            ResolutionKind::ProceedWithoutCheck => "Proceed without the check and approve it",
            _ => "Allow this once and approve it",
        };
        for choice in self.choices.iter_mut().chain(self.held_choices.iter_mut()) {
            if choice.kind == consent {
                choice.label = label.to_string();
            }
        }
        self.also_covers = covers;
        self
    }

    /// Open the wording stage for `write`. The act's choices are kept for a
    /// "don't change it" answer; the act itself stays held.
    pub fn with_wording_stage(&self, write: LastingWrite) -> Self {
        let mut ask = self.clone();
        if ask.stage == AskStage::Decide {
            ask.held_choices = std::mem::take(&mut ask.choices);
        }
        ask.stage = AskStage::ConfirmWording;
        ask.lasting_write = Some(write);
        ask.choices = vec![
            AskChoice::of(ResolutionKind::ConfirmWording),
            AskChoice::of(ResolutionKind::KeepAsIs),
            AskChoice::of(ResolutionKind::Cancel),
        ];
        ask
    }

    /// Back to deciding the held act, with no lasting change pending.
    pub fn with_decide_stage(&self) -> Self {
        let mut ask = self.clone();
        if ask.stage == AskStage::ConfirmWording && !ask.held_choices.is_empty() {
            ask.choices = std::mem::take(&mut ask.held_choices);
        }
        ask.stage = AskStage::Decide;
        ask.lasting_write = None;
        ask
    }

    /// The ask's one consent choice: what a bare "approve" means.
    pub fn consent_kind(&self) -> ResolutionKind {
        match self.purpose {
            AlignmentReceiptPurpose::Exception => ResolutionKind::AllowOnce,
            AlignmentReceiptPurpose::CheckWaiver => ResolutionKind::ProceedWithoutCheck,
        }
    }

    fn choice(&self, id: &str) -> Option<&AskChoice> {
        self.choices.iter().find(|choice| choice.id == id)
    }

    /// The `input_schema.alignment` block on `hitl.requested`: ids, safe
    /// labels and the deadline. Nothing that names the owner's intentions.
    pub fn public_projection(&self) -> Value {
        let mut projection = json!({
            "schema_version": self.schema_version,
            "kind": "preference_alignment",
            "ask_id": self.ask_id,
            "purpose": self.consent_kind().as_str(),
            "choices": self.choices.iter().map(|choice| json!({
                "id": choice.id,
                "label": choice.label,
            })).collect::<Vec<_>>(),
            "free_text": true,
            "deadline_at_ms": self.deadline_at_ms,
            "stage": self.stage,
        });
        // Which other gates the consent also settles: gate names only, the
        // reasons stay private.
        if !self.also_covers.is_empty() {
            projection["also_covers"] = json!(self
                .also_covers
                .iter()
                .map(|covered| covered.gate.as_str())
                .collect::<Vec<_>>());
        }
        // A background hold is time-bound for delivery: the attention
        // pipeline alerts the owner's verified channels with its expiry.
        if self.background {
            projection["collection_deadline_ms"] = json!(self.deadline_at_ms);
        }
        projection
    }

    /// The `input_schema` of this ask's canonical `hitl.requested`: the
    /// confirmation fields every client understands, plus the public
    /// projection. Shared by the first announcement and every re-announce.
    pub fn hitl_input_schema(&self, action_type: &str) -> Value {
        let projection = self.public_projection();
        let consent = match self.stage {
            AskStage::Decide => self.consent_kind(),
            AskStage::ConfirmWording => ResolutionKind::ConfirmWording,
        };
        let mut schema = json!({
            "action_type": action_type,
            "escalation_trigger": "preference_alignment",
            "confirm_label": self.choice(consent.as_str()).map(|choice| choice.label.clone()),
            "deny_label": "Cancel",
            "alignment": projection,
        });
        if let Some(deadline) = schema["alignment"].get("collection_deadline_ms").cloned() {
            schema["collection_deadline_ms"] = deadline;
        }
        schema
    }

    /// What an authenticated owner-private surface shows (P6): the reason,
    /// the act, the cited intentions, and for a wording stage the exact
    /// readback and the token its confirmation must carry.
    pub fn private_projection(&self) -> Value {
        let mut projection = self.public_projection();
        let detail = self.detail.clone().unwrap_or_default();
        projection["explanation"] = json!(detail.explanation);
        projection["act_summary"] = json!(detail.act_summary);
        projection["intentions"] = json!(detail
            .intentions
            .iter()
            .map(|intention| json!({
                "id": intention.id,
                "statement": intention.statement,
                "kind": intention.kind,
                "assertion_status": intention.assertion_status,
                "editable": intention.native_tier().is_some(),
            }))
            .collect::<Vec<_>>());
        if let Some(remember) = &detail.remember {
            projection["remember"] = json!({
                "id": remember.id,
                "statement": remember.statement,
            });
        }
        projection["choices"] = json!(self
            .choices
            .iter()
            .map(|choice| json!({
                "id": choice.id,
                "label": choice.label,
                "lasting": choice.kind.is_lasting(),
                "consent": choice.kind == self.consent_kind() && self.stage == AskStage::Decide,
                "needs_wording": choice.kind == ResolutionKind::ChangePreference,
            }))
            .collect::<Vec<_>>());
        if !self.also_covers.is_empty() {
            projection["also_covers"] = json!(self
                .also_covers
                .iter()
                .map(|covered| json!({
                    "gate": covered.gate.as_str(),
                    "reason": covered.reason,
                }))
                .collect::<Vec<_>>());
        }
        if let Some(write) = &self.lasting_write {
            projection["readback"] = json!({
                "kind": write.kind,
                "wording": write.wording,
                "superseded": write.superseded,
                "token": write.token,
            });
        }
        projection
    }
}

/// An authenticated reply to the active ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerReply<'a> {
    Confirm(bool),
    Choice(&'a str),
    /// A choice that carries a value: the new wording for
    /// `change_preference`, the readback token for `confirm_wording`.
    ChoiceWith(&'a str, &'a str),
    Text(&'a str),
}

impl AlignmentAsk {
    /// Whether a choice's attached value is part of the answer (a wording or
    /// a token) rather than the owner's free-text reply.
    pub fn choice_takes_value(&self, id: &str) -> bool {
        self.choice(id).is_some_and(|choice| {
            matches!(
                choice.kind,
                ResolutionKind::ChangePreference | ResolutionKind::ConfirmWording
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Issue the ask's receipt; the act may run once.
    Grant {
        purpose: AlignmentReceiptPurpose,
        kind: ResolutionKind,
    },
    /// Stop the act.
    Cancel,
    /// No receipt; the run re-proposes and the act is checked again.
    Retry,
    /// No receipt; the run continues with the owner's own words as its
    /// current owner turn.
    Instruction { text: String },
    /// The deadline passed. The act is cancelled; the reply is not consent.
    Expired,
    /// Nothing the ask can act on. The hold stays and the ask is re-issued.
    Rejected { reason: String },
    /// Open the wording stage for this proposal; nothing is written yet.
    ProposeWrite(LastingWrite),
    /// The owner confirmed exactly this proposal on a private surface.
    ApplyWrite(LastingWrite),
    /// Close the wording stage without writing.
    BackToDecision,
}

impl Resolution {
    /// A resolution only the owner's app surfaces may reach: consent for the
    /// act, or a step of a lasting change. Decided on what the reply
    /// resolved to, so a typed label or id is judged like a tapped choice.
    pub fn needs_consent_surface(&self) -> bool {
        matches!(
            self,
            Self::Grant { .. } | Self::ProposeWrite(_) | Self::ApplyWrite(_)
        )
    }
}

const PRIVATE_WORDING_REASON: &str =
    "Open Magician privately to see the exact wording and confirm it there.";

/// Longest free-text reply carried forward as the owner's turn.
pub const MAX_REPLY_CHARS: usize = 600;

fn normalize(text: &str) -> String {
    text.trim()
        .trim_end_matches(|c| c == '.' || c == '!')
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve a reply against the ask. Pure: the caller has authenticated the
/// sender and matched the ask identity.
pub fn resolve_reply(ask: &AlignmentAsk, reply: OwnerReply<'_>, now_ms: i64) -> Resolution {
    if now_ms > ask.deadline_at_ms {
        return Resolution::Expired;
    }
    let wording_write = |kind: LastingWriteKind, words: Option<&str>| -> Resolution {
        let detail = ask.detail.as_ref();
        let target = match kind {
            LastingWriteKind::Remember => detail.and_then(|detail| detail.remember.as_ref()),
            LastingWriteKind::Replace => detail.and_then(AskDetail::change_target),
        };
        let Some((target, tier)) =
            target.and_then(|target| target.native_tier().map(|tier| (target, tier)))
        else {
            return Resolution::Rejected {
                reason: "That preference can only be changed in your profile editor.".to_string(),
            };
        };
        match kind {
            LastingWriteKind::Remember => Resolution::ProposeWrite(LastingWrite::new(
                &ask.ask_id,
                kind,
                target,
                tier,
                target.statement.clone(),
                None,
            )),
            LastingWriteKind::Replace => {
                let words = words.map(str::trim).unwrap_or_default();
                if words.is_empty() {
                    return Resolution::Rejected {
                        reason: "Type the new wording for your preference with \
                                 \"Change my preference\"."
                            .to_string(),
                    };
                }
                if words.chars().count() > MAX_WORDING_CHARS {
                    return Resolution::Rejected {
                        reason: format!(
                            "The new wording is longer than {MAX_WORDING_CHARS} characters."
                        ),
                    };
                }
                Resolution::ProposeWrite(LastingWrite::new(
                    &ask.ask_id,
                    kind,
                    target,
                    tier,
                    words.to_string(),
                    Some(target.statement.clone()),
                ))
            },
        }
    };
    let confirm_wording = |token: Option<&str>| -> Resolution {
        match (&ask.lasting_write, token) {
            (Some(write), Some(token)) if ask.stage == AskStage::ConfirmWording && token.trim() == write.token => {
                Resolution::ApplyWrite(write.clone())
            },
            _ => Resolution::Rejected {
                reason: PRIVATE_WORDING_REASON.to_string(),
            },
        }
    };
    let by_kind = |kind: ResolutionKind| match kind {
        ResolutionKind::AllowOnce | ResolutionKind::ProceedWithoutCheck => {
            if kind == ask.consent_kind() {
                Resolution::Grant {
                    purpose: ask.purpose,
                    kind,
                }
            } else {
                Resolution::Rejected {
                    reason: "That choice was not offered for this action.".to_string(),
                }
            }
        },
        ResolutionKind::Cancel => Resolution::Cancel,
        ResolutionKind::Retry => Resolution::Retry,
        ResolutionKind::UseAlternative => Resolution::Instruction {
            text: "The owner wants something else instead of the held action. Do not run \
                   it; ask them what to do instead if they have not said."
                .to_string(),
        },
        // A bare choice carries no wording: ask for it.
        ResolutionKind::ChangePreference => wording_write(LastingWriteKind::Replace, None),
        ResolutionKind::RememberPreference => wording_write(LastingWriteKind::Remember, None),
        // Only a confirmation that carries the private token saves anything.
        ResolutionKind::ConfirmWording => confirm_wording(None),
        ResolutionKind::KeepAsIs => Resolution::BackToDecision,
    };
    let not_offered = || Resolution::Rejected {
        reason: "That choice was not offered for this action.".to_string(),
    };
    match reply {
        // A bare approve is the act's consent while deciding; on a wording
        // stage it carries no token, so it saves nothing.
        OwnerReply::Confirm(true) => match ask.stage {
            AskStage::Decide => by_kind(ask.consent_kind()),
            AskStage::ConfirmWording => confirm_wording(None),
        },
        OwnerReply::Confirm(false) => Resolution::Cancel,
        OwnerReply::Choice(id) => match ask.choice(id) {
            Some(choice) => by_kind(choice.kind),
            None => not_offered(),
        },
        OwnerReply::ChoiceWith(id, value) => match ask.choice(id).map(|choice| choice.kind) {
            Some(ResolutionKind::ChangePreference) => {
                wording_write(LastingWriteKind::Replace, Some(value))
            },
            Some(ResolutionKind::ConfirmWording) => confirm_wording(Some(value)),
            Some(kind) => by_kind(kind),
            None => not_offered(),
        },
        OwnerReply::Text(text) => {
            let normalized = normalize(text);
            if normalized.is_empty() {
                return Resolution::Rejected {
                    reason: "The reply was empty.".to_string(),
                };
            }
            // Only an exact choice id or label selects a choice. Anything
            // else is the owner's own instruction. Typing a lasting choice's
            // label selects it like a tap, which still only opens (or asks
            // for) the private wording confirmation.
            if let Some(choice) = ask.choices.iter().find(|choice| {
                normalized == choice.id.replace('_', " ")
                    || normalized == choice.id
                    || normalized == normalize(&choice.label)
            }) {
                return by_kind(choice.kind);
            }
            let reply = text.trim();
            let mut text: String = reply.chars().take(MAX_REPLY_CHARS).collect();
            if reply.chars().count() > MAX_REPLY_CHARS {
                text.push('…');
            }
            Resolution::Instruction { text }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold() -> AlignmentAsk {
        AlignmentAsk::for_hold(
            "ask-1".into(),
            "alignment_binding.v1:x".into(),
            Some("exec-1".into()),
            vec!["pref-1".into()],
            &[crate::magician_v2::preference_alignment::contract::ResolutionChoice::UseAlternative],
            1_000,
            60,
            false,
            None,
        )
    }

    fn cited(id: &str, status: AssertionStatus, statement: &str) -> AskIntention {
        AskIntention {
            id: id.into(),
            revision: format!("rev-{id}"),
            source: IntentionSource::NativeTier {
                tier: "preferences".into(),
            },
            kind: IntentionKind::Taste,
            assertion_status: status,
            statement: statement.into(),
        }
    }

    /// A hold citing an explicit preference, offering to change it and to
    /// remember an inferred one.
    fn rich_hold() -> AlignmentAsk {
        use crate::magician_v2::preference_alignment::contract::ResolutionChoice;
        AlignmentAsk::for_hold(
            "ask-3".into(),
            "alignment_binding.v1:x".into(),
            Some("exec-1".into()),
            vec!["pref-1".into()],
            &[ResolutionChoice::ChangePreference],
            1_000,
            60,
            false,
            Some(AskDetail {
                explanation: "You asked to keep dinners vegetarian.".into(),
                act_summary: "Order butter chicken".into(),
                intentions: vec![cited(
                    "pref-1",
                    AssertionStatus::DirectOwnerStatement,
                    "diet: vegetarian dinners",
                )],
                remember: Some(cited("inf-1", AssertionStatus::Inferred, "spice: mild")),
            }),
        )
    }

    fn waiver() -> AlignmentAsk {
        AlignmentAsk::for_unresolved_check(
            "ask-2".into(),
            "alignment_binding.v1:x".into(),
            None,
            vec![],
            1_000,
            60,
            true,
        )
    }

    #[test]
    fn a_bare_approve_means_the_one_consent_the_ask_offered() {
        assert_eq!(
            resolve_reply(&hold(), OwnerReply::Confirm(true), 1_001),
            Resolution::Grant {
                purpose: AlignmentReceiptPurpose::Exception,
                kind: ResolutionKind::AllowOnce
            }
        );
        assert_eq!(
            resolve_reply(&waiver(), OwnerReply::Confirm(true), 1_001),
            Resolution::Grant {
                purpose: AlignmentReceiptPurpose::CheckWaiver,
                kind: ResolutionKind::ProceedWithoutCheck
            }
        );
        assert_eq!(resolve_reply(&hold(), OwnerReply::Confirm(false), 1_001), Resolution::Cancel);
    }

    #[test]
    fn a_choice_the_ask_did_not_offer_keeps_the_hold() {
        // A waiver cannot be selected on an exception ask, or the reverse.
        assert!(matches!(
            resolve_reply(&hold(), OwnerReply::Choice("proceed_without_check"), 1_001),
            Resolution::Rejected { .. }
        ));
        assert!(matches!(
            resolve_reply(&waiver(), OwnerReply::Choice("allow_once"), 1_001),
            Resolution::Rejected { .. }
        ));
        assert!(matches!(
            resolve_reply(&hold(), OwnerReply::Choice("allow_always"), 1_001),
            Resolution::Rejected { .. }
        ));
        assert_eq!(resolve_reply(&waiver(), OwnerReply::Choice("retry"), 1_001), Resolution::Retry);
    }

    #[test]
    fn free_text_is_an_instruction_unless_it_names_a_choice_exactly() {
        assert_eq!(
            resolve_reply(&hold(), OwnerReply::Text("Allow this once."), 1_001),
            Resolution::Grant {
                purpose: AlignmentReceiptPurpose::Exception,
                kind: ResolutionKind::AllowOnce
            }
        );
        assert_eq!(resolve_reply(&hold(), OwnerReply::Text(" cancel "), 1_001), Resolution::Cancel);
        // "yes go ahead" is not a choice: it goes to the agent as the owner's
        // words, and the next check judges the act with it as evidence.
        assert_eq!(
            resolve_reply(&hold(), OwnerReply::Text("yes go ahead, it's for a guest"), 1_001),
            Resolution::Instruction {
                text: "yes go ahead, it's for a guest".into()
            }
        );
        assert!(matches!(
            resolve_reply(&hold(), OwnerReply::Text("   "), 1_001),
            Resolution::Rejected { .. }
        ));
    }

    #[test]
    fn hold_choices_follow_the_judges_offer() {
        let ask = hold();
        let ids: Vec<&str> = ask.choices.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(ids, ["allow_once", "use_alternative", "cancel"]);
        // Not offered by the judge, so not answerable by id.
        assert!(matches!(
            resolve_reply(&ask, OwnerReply::Choice("change_preference"), 1_001),
            Resolution::Rejected { .. }
        ));
    }

    #[test]
    fn a_reply_after_the_deadline_is_never_consent() {
        let ask = hold();
        assert_eq!(
            resolve_reply(&ask, OwnerReply::Confirm(true), ask.deadline_at_ms + 1),
            Resolution::Expired
        );
        assert_eq!(
            resolve_reply(&ask, OwnerReply::Choice("allow_once"), ask.deadline_at_ms + 1),
            Resolution::Expired
        );
    }

    #[test]
    fn lasting_choices_are_offered_only_for_what_alignment_can_write() {
        let ids: Vec<String> = rich_hold().choices.iter().map(|c| c.id.clone()).collect();
        assert_eq!(
            ids,
            ["allow_once", "change_preference", "remember_preference", "cancel"]
        );
        // A profile-sourced citation cannot be changed here, and an explicit
        // preference is not "remembered" again.
        let mut detail = rich_hold().detail.unwrap();
        detail.intentions[0].source = IntentionSource::TasteProfile;
        detail.remember.as_mut().unwrap().assertion_status = AssertionStatus::Confirmed;
        let ask = AlignmentAsk::for_hold(
            "ask-4".into(),
            "b".into(),
            None,
            vec![],
            &[crate::magician_v2::preference_alignment::contract::ResolutionChoice::ChangePreference],
            1_000,
            60,
            false,
            Some(detail),
        );
        let ids: Vec<&str> = ask.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["allow_once", "cancel"]);
    }

    #[test]
    fn remembering_reads_the_wording_back_before_anything_is_saved() {
        let ask = rich_hold();
        let Resolution::ProposeWrite(write) =
            resolve_reply(&ask, OwnerReply::Choice("remember_preference"), 1_001)
        else {
            panic!("remember opens the wording stage");
        };
        assert_eq!(write.kind, LastingWriteKind::Remember);
        assert_eq!(write.wording, "spice: mild");
        assert_eq!(write.expected_revision, "rev-inf-1");

        let staged = ask.with_wording_stage(write.clone());
        // A bare approve, a confirm without the token or with another token,
        // saves nothing.
        for reply in [
            OwnerReply::Confirm(true),
            OwnerReply::Choice("confirm_wording"),
            OwnerReply::ChoiceWith("confirm_wording", "not-the-token"),
            OwnerReply::Text("save this wording"),
        ] {
            assert!(
                matches!(resolve_reply(&staged, reply, 1_001), Resolution::Rejected { .. }),
                "{reply:?} must not save"
            );
        }
        assert_eq!(
            resolve_reply(
                &staged,
                OwnerReply::ChoiceWith("confirm_wording", &write.token),
                1_001
            ),
            Resolution::ApplyWrite(write)
        );
        // Neither stage choice is consent for the act.
        assert!(!matches!(
            resolve_reply(&staged, OwnerReply::Choice("allow_once"), 1_001),
            Resolution::Grant { .. }
        ));
        // "Don't change it" restores the act's choices.
        assert_eq!(
            resolve_reply(&staged, OwnerReply::Choice("keep_as_is"), 1_001),
            Resolution::BackToDecision
        );
        assert_eq!(staged.with_decide_stage().choices, ask.choices);
    }

    #[test]
    fn a_change_carries_the_owners_wording_and_the_text_it_replaces() {
        let ask = rich_hold();
        assert!(matches!(
            resolve_reply(&ask, OwnerReply::Choice("change_preference"), 1_001),
            Resolution::Rejected { .. }
        ));
        let Resolution::ProposeWrite(write) = resolve_reply(
            &ask,
            OwnerReply::ChoiceWith("change_preference", "  vegetarian on weekdays only "),
            1_001,
        ) else {
            panic!("a worded change opens the wording stage");
        };
        assert_eq!(write.kind, LastingWriteKind::Replace);
        assert_eq!(write.wording, "vegetarian on weekdays only");
        assert_eq!(write.superseded.as_deref(), Some("diet: vegetarian dinners"));
        assert_eq!(write.intention_id, "pref-1");
        // The same proposal gets the same token; different wording does not.
        let Resolution::ProposeWrite(again) = resolve_reply(
            &ask,
            OwnerReply::ChoiceWith("change_preference", "vegetarian on weekdays only"),
            1_001,
        ) else {
            panic!()
        };
        assert_eq!(again.token, write.token);
        let Resolution::ProposeWrite(other) = resolve_reply(
            &ask,
            OwnerReply::ChoiceWith("change_preference", "vegan"),
            1_001,
        ) else {
            panic!()
        };
        assert_ne!(other.token, write.token);
    }

    #[test]
    fn only_the_private_projection_carries_the_detail_and_the_token() {
        let ask = rich_hold();
        let Resolution::ProposeWrite(write) =
            resolve_reply(&ask, OwnerReply::Choice("remember_preference"), 1_001)
        else {
            panic!()
        };
        let staged = ask.with_wording_stage(write.clone());
        let public = staged.hitl_input_schema("confirmation").to_string();
        for secret in ["vegetarian", "mild", "butter", write.token.as_str(), "pref-1", "inf-1"] {
            assert!(!public.contains(secret), "public schema leaks {secret}");
        }
        let private = staged.private_projection();
        assert_eq!(private["readback"]["token"], json!(write.token));
        assert_eq!(private["readback"]["wording"], json!("spice: mild"));
        assert_eq!(private["explanation"], json!("You asked to keep dinners vegetarian."));
        assert_eq!(private["stage"], json!("confirm_wording"));
    }

    #[test]
    fn the_public_projection_names_no_intention() {
        let projection = hold().public_projection();
        let text = projection.to_string();
        assert!(!text.contains("pref-1"));
        assert!(!text.contains("exec-1"));
        assert!(projection.get("collection_deadline_ms").is_none());
        assert_eq!(
            waiver().public_projection()["collection_deadline_ms"],
            json!(waiver().deadline_at_ms)
        );
    }

    #[test]
    fn a_combined_ask_names_the_other_gate_and_only_its_consent_settles_it() {
        let ask = hold().with_also_covers(vec![CoveredRequirement {
            gate: CoveredGate::ApprovalRule,
            reason: "Action matched approval rule (tool=`swiggy-mcp`, action=\"*\")".to_string(),
        }]);
        let consent = ask
            .choices
            .iter()
            .find(|choice| choice.kind == ResolutionKind::AllowOnce)
            .expect("consent choice");
        assert_eq!(consent.label, "Allow this once and approve it");
        assert_eq!(
            ask.hitl_input_schema("pack")["confirm_label"],
            json!("Allow this once and approve it")
        );

        let public = ask.public_projection();
        assert_eq!(public["also_covers"], json!(["approval_rule"]));
        assert!(!public.to_string().contains("swiggy-mcp"), "the gate's reason stays private");
        assert_eq!(
            ask.private_projection()["also_covers"][0]["gate"],
            json!("approval_rule")
        );

        // The one consent grants; nothing else does.
        assert!(matches!(
            resolve_reply(&ask, OwnerReply::Confirm(true), ask.created_at_ms),
            Resolution::Grant { .. }
        ));
        assert_eq!(
            resolve_reply(&ask, OwnerReply::Choice("cancel"), ask.created_at_ms),
            Resolution::Cancel
        );
        assert!(matches!(
            resolve_reply(&ask, OwnerReply::Text("order the paneer one"), ask.created_at_ms),
            Resolution::Instruction { .. }
        ));

        // Nothing to present leaves the ask as it was.
        assert_eq!(hold().with_also_covers(Vec::new()), hold());
    }
}
