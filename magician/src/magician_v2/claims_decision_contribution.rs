//! Destination-owned apply seam for app-proposed claims decisions.
//!
//! The claims-review app may prepare one of four closed commands, but it does
//! not own the claims or commitments registers and cannot settle them through
//! the generic, hypothesis-only contribution terminal. This module verifies a
//! trusted-desktop signature, binds the reviewed app scope and target revision,
//! then delegates to the same receipt-bearing expected-revision methods used by
//! first-party handlers. Named-decider, extractor-self-confirm, terminal claim,
//! confirmed-claim-to-unconfirmed-commitment, and commitment confirmation rules
//! remain exclusively with those canonical stores.
//!
//! # The staged-ingest seam beside it
//!
//! [`apply_staged_app_ingest`] is the same shape of act for the package's
//! `ingest_request` rows: an app-written row an authenticated host applies.
//! It is here because this is the file that already holds an authenticated
//! actor beside an app-scoped register, and that actor is the one field of a
//! staged apply the caller may not choose.
//!
//! It deliberately carries **no owner signature**. A decision seam settles a
//! claim — it moves an existing register row to a terminal state — so it is
//! bound to a paired desktop key. An ingest records that a room happened and
//! queues what it appeared to claim for review; the first-party owner route
//! `POST /transcripts/ingest` already records exactly such acts on an
//! authenticated session alone, with attribution the caller supplies outright.
//! Requiring more here than the canonical entry requires would not close a
//! door, and treating it as a settlement would.

use chrono::{DateTime, Utc};
use magician_app_contract::contribution::{
    AppClaimsDecisionAudienceKindV1, AppClaimsDecisionOwnerDecisionEnvelopeV1,
    AppClaimsDecisionOwnerDecisionV1, AppClaimsDecisionProposalV1, AppClaimsDecisionSourceHeaderV1,
    AppClaimsDecisionTargetV1, AppClaimsDecisionVerbV1, APP_CLAIMS_DECISION_CONTRACT_ID,
};
use magician_app_contract::macos_host::app_macos_desktop_identity_digest;
use serde::Serialize;
use serde_json::{json, Value};

use crate::magician_v2::{
    agents::ConsequenceClass,
    apps::authority::AuthenticatedAppScope,
    audience::{AudienceKind, AudienceRef},
    commitments::{CommitmentDecisionOutcome, CommitmentScope, Commitments},
    evidence::transcript_ingestion::{
        StagedIngestApplication, StagedIngestConsumer, StagedIngestHostContext,
        StagedIngestRequest, StagedIngestRoster,
    },
    evidence::{
        ClaimDecisionOutcome, EvidenceDecisionScope, OutwardScope, OwnerDecision,
        ReviewReceiptProjector, TranscriptIngestion,
    },
    json_traversal::canonical_json_bytes,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppClaimsDecisionContributionError {
    InvalidContract(String),
    InvalidSignature(String),
    NotAdmissible(&'static str),
    Apply(String),
}

impl std::fmt::Display for AppClaimsDecisionContributionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidContract(error) => {
                write!(formatter, "invalid app claims-decision contract: {error}")
            },
            Self::InvalidSignature(error) => {
                write!(
                    formatter,
                    "app claims-decision owner signature is invalid: {error}"
                )
            },
            Self::NotAdmissible(rule) => {
                write!(formatter, "inadmissible app claims decision: {rule}")
            },
            Self::Apply(error) => write!(formatter, "app claims-decision apply failed: {error}"),
        }
    }
}

impl std::error::Error for AppClaimsDecisionContributionError {}

/// Canonical destination result. Receipts are returned unchanged so a caller
/// can project its package ledger without inventing a second authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "target_kind", content = "outcome", rename_all = "snake_case")]
pub enum AppClaimsDecisionApplication {
    Claim(ClaimDecisionOutcome),
    Commitment(CommitmentDecisionOutcome),
    OwnerDeclined,
}

/// Current registry identity resolved by the authenticated host immediately
/// before destination apply. The signed header must match every field; an
/// enabled installation id alone is not authority after update, reinstall, or
/// grant/schema replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppClaimsDecisionCurrentAuthority {
    pub destination_schema_digest: String,
    pub installation_generation: u64,
    pub package_revision_ref: String,
    pub package_content_digest: String,
    pub grant_revision: u64,
    pub grant_authority_digest: String,
    pub schema_revision: u64,
    pub schema_digest: String,
    pub source_entity_name: String,
    pub source_record_id: String,
    pub source_record_revision: u64,
    pub source_record_digest: String,
    pub source_record_payload: Value,
    pub workflow_id: String,
    pub workflow_digest: String,
    pub action_id: String,
    pub action_digest: String,
}

impl AppClaimsDecisionCurrentAuthority {
    fn matches_header(&self, header: &AppClaimsDecisionSourceHeaderV1) -> bool {
        header.destination_schema_digest == self.destination_schema_digest
            && header.installation_generation == self.installation_generation
            && header.package_revision_ref == self.package_revision_ref
            && header.package_content_digest == self.package_content_digest
            && header.grant_revision == self.grant_revision
            && header.grant_authority_digest == self.grant_authority_digest
            && header.schema_revision == self.schema_revision
            && header.schema_digest == self.schema_digest
            && header.source_entity_name == self.source_entity_name
            && header.source_record_id == self.source_record_id
            && header.source_record_revision == self.source_record_revision
            && header.source_record_digest == self.source_record_digest
            && header.workflow_id == self.workflow_id
            && header.workflow_digest == self.workflow_digest
            && header.action_id == self.action_id
            && header.action_digest == self.action_digest
    }
}

/// Validate the sealed command's destination identity.
///
/// Contract validation also enforces the dedicated claims-command authority
/// header, its exact `review_decision` source head, closed verb/target pair,
/// positive expected revision, bounded actor/note, and all authority digests.
pub fn validate_app_claims_decision(
    proposal: &AppClaimsDecisionProposalV1,
) -> Result<(), AppClaimsDecisionContributionError> {
    proposal
        .validate()
        .map_err(|error| AppClaimsDecisionContributionError::InvalidContract(error.to_string()))?;
    if proposal.header.destination_contract_id != APP_CLAIMS_DECISION_CONTRACT_ID {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "claims decisions require the magician.claims-decision contract",
        ));
    }
    if proposal.header.workflow_id != proposal.verb.as_str()
        || proposal.header.action_id != proposal.verb.as_str()
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command verb does not match its exact reviewed workflow and action",
        ));
    }
    Ok(())
}

/// Apply one trusted-desktop decision through the canonical CAS transitions.
///
/// The caller supplies a server-minted authenticated app scope. The destination
/// derives its storage scope from that same value, preventing a valid envelope
/// reviewed for one app scope from being replayed against a second scope that
/// happens to contain the same friendly target id. The sealed `by` identity
/// must also be the authenticated actor reference. Accepted envelopes enter a
/// destination-locked replay/CAS path: exact receipts remain replayable, while
/// proposal lifetime and authenticated-scope validity are re-sampled before
/// any new durable write. Rejected owner proposals return without mutation.
pub fn apply_signed_app_claims_decision(
    envelope: &AppClaimsDecisionOwnerDecisionEnvelopeV1,
    desktop_identity_public_key_hex: &str,
    expected_desktop_pairing_generation: u64,
    expected_desktop_identity_key_id: &str,
    expected_desktop_identity_digest: &str,
    expected_installation_id: &str,
    current_authority: &AppClaimsDecisionCurrentAuthority,
    authenticated: &AuthenticatedAppScope,
    ingestion: &TranscriptIngestion,
    commitments: &Commitments,
    now: DateTime<Utc>,
) -> Result<AppClaimsDecisionApplication, AppClaimsDecisionContributionError> {
    authenticated.ensure_live_at(&now).map_err(|_| {
        AppClaimsDecisionContributionError::NotAdmissible("the authenticated app scope is not live")
    })?;
    let recomputed_identity = app_macos_desktop_identity_digest(
        expected_desktop_identity_key_id,
        desktop_identity_public_key_hex,
    )
    .map_err(|error| AppClaimsDecisionContributionError::InvalidSignature(error.to_string()))?;
    if recomputed_identity != expected_desktop_identity_digest
        || envelope.review.desktop_pairing_generation != expected_desktop_pairing_generation
        || envelope.review.desktop_identity_key_id != expected_desktop_identity_key_id
        || envelope.review.desktop_identity_digest != expected_desktop_identity_digest
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command is not bound to the active paired desktop identity",
        ));
    }
    envelope
        .verify_signature(desktop_identity_public_key_hex)
        .map_err(|error| AppClaimsDecisionContributionError::InvalidSignature(error.to_string()))?;
    let proposal = &envelope.review.proposal;
    validate_app_claims_decision(proposal)?;
    if proposal.header.scope_binding_ref != authenticated.scope_binding_ref().as_str() {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command does not belong to the authenticated app scope",
        ));
    }
    if proposal.header.installation_id != expected_installation_id {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command does not belong to the expected app installation",
        ));
    }
    if !current_authority.matches_header(&proposal.header) {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command does not match the current destination, installation, grant, package, schema, workflow, action, and source-record authority",
        ));
    }
    validate_review_decision_source(proposal, &current_authority.source_record_payload)?;
    let authenticated_scope = authenticated.scope();
    if proposal.by != authenticated.actor_ref().as_str() {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the signed command decider is not the authenticated actor",
        ));
    }
    if envelope.decision != AppClaimsDecisionOwnerDecisionV1::Accept {
        return Ok(AppClaimsDecisionApplication::OwnerDeclined);
    }
    let scope = OutwardScope::new(
        authenticated_scope.principal.as_str(),
        authenticated_scope.workspace.as_str(),
    );
    let applied = match proposal.verb {
        AppClaimsDecisionVerbV1::ConfirmClaim => {
            let (claim_id, expected_revision) = claim_target(&proposal.target)?;
            let decision = OwnerDecision {
                by: proposal.by.clone(),
                note: proposal.note.clone(),
            };
            ingestion
                .confirm_claim_at_revision_guarded(
                    &scope,
                    claim_id,
                    expected_revision,
                    &envelope.decision_id,
                    &decision,
                    now,
                    |prepared_at| {
                        admit_signed_destination_write(authenticated, proposal, prepared_at)
                    },
                )
                .map(AppClaimsDecisionApplication::Claim)
                .map_err(apply_error)
        },
        AppClaimsDecisionVerbV1::RejectClaim => {
            let (claim_id, expected_revision) = claim_target(&proposal.target)?;
            let decision = OwnerDecision {
                by: proposal.by.clone(),
                note: proposal.note.clone(),
            };
            ingestion
                .reject_claim_at_revision_guarded(
                    &scope,
                    claim_id,
                    expected_revision,
                    &envelope.decision_id,
                    &decision,
                    now,
                    || admit_signed_destination_write(authenticated, proposal, None),
                )
                .map(AppClaimsDecisionApplication::Claim)
                .map_err(apply_error)
        },
        AppClaimsDecisionVerbV1::RecordCommitment => {
            let (claim_id, expected_revision) = claim_target(&proposal.target)?;
            ingestion
                .record_commitment_from_claim_at_revision_guarded(
                    commitments,
                    &scope,
                    claim_id,
                    expected_revision,
                    &envelope.decision_id,
                    now,
                    || admit_signed_destination_write(authenticated, proposal, None),
                )
                .map(AppClaimsDecisionApplication::Commitment)
                .map_err(apply_error)
        },
        AppClaimsDecisionVerbV1::ConfirmCommitment => {
            let (audience_kind, audience_id, commitment_id, expected_revision) =
                commitment_target(&proposal.target)?;
            let audience = destination_audience(audience_kind, audience_id.to_owned());
            let commitment_scope =
                CommitmentScope::new(scope.principal.as_str(), scope.workspace.as_str());
            commitments
                .confirm_at_revision_guarded(
                    &commitment_scope,
                    &audience,
                    commitment_id,
                    expected_revision,
                    &envelope.decision_id,
                    &proposal.by,
                    now,
                    || admit_signed_destination_write(authenticated, proposal, None),
                )
                .map(AppClaimsDecisionApplication::Commitment)
                .map_err(apply_error)
        },
    };
    project_completed_review_receipts(&scope, ingestion, commitments);
    applied
}

/// Project every completion the registers have journalled but not yet published
/// into the package's `review_receipt` ledger.
///
/// Runs on the tail of the destination call, after the registers have released
/// their decision locks, so the row a console is about to refresh for is
/// usually there before the response is. It is not the only route, and must not
/// be: the register writes its receipt first and journals second, so a process
/// that dies in between leaves a completion this call never saw. The journal
/// cursor is what makes that recoverable — the next decision in the scope
/// drains it, and the row it mints is byte-identical to the one this call would
/// have written.
///
/// **Best-effort, deliberately.** A ledger failure is warned about and never
/// returned. The decision itself is already durable and authoritative; failing
/// the owner's signed command because a projection could not be written would
/// invert the risk this record exists to manage, and would tempt a caller into
/// retrying a decision that already applied.
///
/// It runs on the refusal path too. Nothing new was journalled there, so the
/// drain reads a cursor and a head and stops — but a scope carrying an earlier
/// interrupted projection heals on the cheapest call that reaches it.
fn project_completed_review_receipts(
    scope: &OutwardScope,
    ingestion: &TranscriptIngestion,
    commitments: &Commitments,
) {
    let projector = ReviewReceiptProjector::over(ingestion);
    let journal_scope =
        EvidenceDecisionScope::new(scope.principal.clone(), scope.workspace.clone());
    if let Err(error) = projector.drain_all(&journal_scope, ingestion, commitments) {
        tracing::warn!(
            target: "apps",
            %error,
            "the review receipt projection could not be advanced; the decision is applied and \
             the next drain will republish it"
        );
    }
}

/// Everything about a staged ingest that only the host may say, as the
/// authenticated route resolved it.
///
/// The applier is deliberately **not** a field. It is the authenticated actor
/// and [`apply_staged_app_ingest`] takes it from the scope, because recording
/// the applier as the extractor is what makes the self-confirm rule bite:
/// whoever applies a staged room may not then confirm the claims it queued. A
/// caller-supplied applier would be a caller choosing who is allowed to decide.
#[derive(Debug, Clone)]
pub struct AppStagedIngestHostResolution {
    /// Which named people are ours and which are the other side.
    ///
    /// Host identity resolution only. The staged document maps a speaker key
    /// to a named person — a person's typed intent, which the package owns —
    /// but the side that person is on decides whether their words become an
    /// outward assertion in our name, so it may never be read off the row.
    pub roster: StagedIngestRoster,
    /// Our side's identity in the room. The roster must place it on our side,
    /// which the register checks rather than assumes.
    pub effective_speaker: String,
    /// What the room cost if what was said in it was wrong. Supplied, never
    /// inferred: understating it is the direction that misleads a reviewer.
    pub consequence_class: ConsequenceClass,
    /// When the words were said — neither when they were staged nor when they
    /// are being applied.
    pub occurred_at: DateTime<Utc>,
}

/// Apply one staged `ingest_request` row through the canonical ingestion entry.
///
/// The row arrives as the exact live head the caller read under a single
/// installation/grant/schema snapshot; this seam owns what the row may say and
/// the register beneath owns what may be recorded. Everything ends at
/// `TranscriptIngestion::ingest_transcript`, the same entry the first-party
/// transcript route serves, so no second path decides that a room was outward.
///
/// # What it refuses
///
/// Every check belongs to the layer that can prove it, and none of them are
/// repeated here: the closed row shape, `apply_state: recorded`, absent
/// host-stamped fields and the bounded document come from
/// `StagedIngestRequest::from_staged_row`; an unplaceable name, a counterparty
/// effective speaker and a private/local class come from the consumer. This
/// function adds the one thing neither can see — that the app scope applying
/// the row is still live at the moment of the write.
///
/// # Idempotent, and that is the receipt
///
/// One `ingest_id` derives one transcript key, one act and one set of claim
/// ids, so an apply retried after a failed projection resumes the same act
/// rather than queueing the room twice. That is what lets a caller stamp its
/// row after the fact and simply retry when the stamp does not land.
pub fn apply_staged_app_ingest(
    staged_row: &Value,
    resolution: AppStagedIngestHostResolution,
    authenticated: &AuthenticatedAppScope,
    ingestion: &TranscriptIngestion,
    now: DateTime<Utc>,
) -> Result<StagedIngestApplication, AppClaimsDecisionContributionError> {
    // Sampled by the caller at the mutation boundary, not at handler entry:
    // registry I/O and blocking-worker admission may outlive the timestamp a
    // route started with, and an expired scope may not write an act.
    authenticated.ensure_live_at(&now).map_err(|_| {
        AppClaimsDecisionContributionError::NotAdmissible("the authenticated app scope is not live")
    })?;
    let request = StagedIngestRequest::from_staged_row(staged_row).map_err(|error| {
        AppClaimsDecisionContributionError::InvalidContract(format!("{error:#}"))
    })?;
    let scope = OutwardScope::new(
        authenticated.scope().principal.as_str(),
        authenticated.scope().workspace.as_str(),
    );
    let context = StagedIngestHostContext {
        applied_by: authenticated.actor_ref().as_str().to_owned(),
        effective_speaker: resolution.effective_speaker,
        roster: resolution.roster,
        consequence_class: resolution.consequence_class,
        occurred_at: resolution.occurred_at,
    };
    StagedIngestConsumer::over(ingestion)
        .apply(&scope, &request, &context, now)
        .map_err(apply_error)
}

/// Prove that the live source head says exactly what the sealed command says.
///
/// A digest match alone proves only that some current `review_decision` row was
/// named. This closed comparison prevents a signer or host integration bug
/// from attaching a valid digest for claim A to a command for claim B. The
/// source remains immutable: host-stamped actor and destination receipt fields
/// are deliberately required to be absent from this row.
fn validate_review_decision_source(
    proposal: &AppClaimsDecisionProposalV1,
    source: &Value,
) -> Result<(), AppClaimsDecisionContributionError> {
    const SOURCE_FIELDS: [&str; 11] = [
        "request_id",
        "decision_id",
        "target_kind",
        "target_id",
        "decision",
        "expected_revision",
        "actor_ref",
        "reason",
        "payload_json",
        "apply_state",
        "decided_at",
    ];
    let object = source
        .as_object()
        .ok_or(AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision source payload is not an object",
        ))?;
    if object.len() != SOURCE_FIELDS.len()
        || SOURCE_FIELDS
            .iter()
            .any(|field| !object.contains_key(*field))
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision source payload does not have the closed package shape",
        ));
    }
    let source_text = |field: &'static str| {
        object.get(field).and_then(Value::as_str).ok_or(
            AppClaimsDecisionContributionError::NotAdmissible(
                "the review_decision source payload has an invalid text field",
            ),
        )
    };
    if source_text("request_id")? != proposal.header.dedupe_key
        || source_text("decision")? != proposal.verb.as_str()
        || source_text("target_id")? != proposal.target.target_id()
        || object.get("expected_revision").and_then(Value::as_u64)
            != Some(proposal.target.expected_revision())
        || source_text("apply_state")? != "recorded"
        || object.get("decision_id") != Some(&Value::Null)
        || object.get("actor_ref") != Some(&Value::Null)
        || source_text("decided_at")?.is_empty()
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision source identity, target, revision, or immutable state does not match the signed command",
        ));
    }

    let request_id = source_text("request_id")?;
    let (target_kind, source_reason, expected_payload) = match (proposal.verb, &proposal.target) {
        (
            AppClaimsDecisionVerbV1::ConfirmClaim | AppClaimsDecisionVerbV1::RejectClaim,
            AppClaimsDecisionTargetV1::Claim {
                claim_id,
                expected_revision,
            },
        ) => {
            let note = proposal.note.as_deref().ok_or(
                AppClaimsDecisionContributionError::NotAdmissible(
                    "a package claim decision must retain its reviewed reason",
                ),
            )?;
            if object.get("reason").and_then(Value::as_str) != Some(note) {
                return Err(AppClaimsDecisionContributionError::NotAdmissible(
                    "the review_decision reason does not match the signed command note",
                ));
            }
            (
                "claim",
                Some(note),
                json!({
                    "request_id": request_id,
                    "claim_id": claim_id,
                    "expected_revision": expected_revision,
                    "reason": note,
                }),
            )
        },
        (
            AppClaimsDecisionVerbV1::RecordCommitment,
            AppClaimsDecisionTargetV1::Claim {
                claim_id,
                expected_revision,
            },
        ) => (
            "claim",
            None,
            json!({
                "request_id": request_id,
                "claim_id": claim_id,
                "expected_revision": expected_revision,
            }),
        ),
        (
            AppClaimsDecisionVerbV1::ConfirmCommitment,
            AppClaimsDecisionTargetV1::Commitment {
                audience_kind,
                audience_id,
                commitment_id,
                expected_revision,
            },
        ) => (
            "commitment",
            None,
            json!({
                "request_id": request_id,
                "commitment_id": commitment_id,
                "audience_kind": audience_kind.as_str(),
                "audience_id": audience_id,
                "expected_revision": expected_revision,
            }),
        ),
        _ => {
            return Err(AppClaimsDecisionContributionError::NotAdmissible(
                "the review_decision source target kind does not match the signed command",
            ))
        },
    };
    if source_text("target_kind")? != target_kind
        || (source_reason.is_none() && object.get("reason") != Some(&Value::Null))
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision source target kind or reason does not match the signed command",
        ));
    }

    let encoded_payload = source_text("payload_json")?;
    let parsed_payload: Value = serde_json::from_str(encoded_payload).map_err(|_| {
        AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision payload_json is not valid JSON",
        )
    })?;
    let canonical_payload = canonical_json_bytes(&parsed_payload).map_err(|_| {
        AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision payload_json cannot be canonicalized",
        )
    })?;
    if parsed_payload != expected_payload
        || canonical_payload.as_slice() != encoded_payload.as_bytes()
    {
        return Err(AppClaimsDecisionContributionError::NotAdmissible(
            "the review_decision payload_json is not the canonical exact action input",
        ));
    }
    Ok(())
}

fn claim_target(
    target: &AppClaimsDecisionTargetV1,
) -> Result<(&str, u64), AppClaimsDecisionContributionError> {
    match target {
        AppClaimsDecisionTargetV1::Claim {
            claim_id,
            expected_revision,
        } => Ok((claim_id, *expected_revision)),
        AppClaimsDecisionTargetV1::Commitment { .. } => {
            Err(AppClaimsDecisionContributionError::NotAdmissible(
                "a claim command requires a claim target",
            ))
        },
    }
}

fn commitment_target(
    target: &AppClaimsDecisionTargetV1,
) -> Result<(AppClaimsDecisionAudienceKindV1, &str, &str, u64), AppClaimsDecisionContributionError>
{
    match target {
        AppClaimsDecisionTargetV1::Commitment {
            audience_kind,
            audience_id,
            commitment_id,
            expected_revision,
        } => Ok((
            *audience_kind,
            audience_id,
            commitment_id,
            *expected_revision,
        )),
        AppClaimsDecisionTargetV1::Claim { .. } => {
            Err(AppClaimsDecisionContributionError::NotAdmissible(
                "a commitment command requires a commitment target",
            ))
        },
    }
}

fn destination_audience(kind: AppClaimsDecisionAudienceKindV1, id: String) -> AudienceRef {
    let kind = match kind {
        AppClaimsDecisionAudienceKindV1::Engagement => AudienceKind::Engagement,
        AppClaimsDecisionAudienceKindV1::Program => AudienceKind::Program,
        AppClaimsDecisionAudienceKindV1::Account => AudienceKind::Account,
        AppClaimsDecisionAudienceKindV1::Panel => AudienceKind::Panel,
        AppClaimsDecisionAudienceKindV1::Person => AudienceKind::Person,
    };
    AudienceRef::new(kind, id)
}

#[derive(Debug)]
struct DestinationWriteNotAdmissible(&'static str);

impl std::fmt::Display for DestinationWriteNotAdmissible {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for DestinationWriteNotAdmissible {}

/// Re-sample the mutable authority immediately before a destination write.
///
/// `prepared_at` is destination evidence that this exact claim confirmation
/// crossed its first WAL boundary while the proposal was live. Such a
/// transaction may finish after proposal expiry so it cannot wedge the claim,
/// but it never bypasses current authenticated-scope validity. Commitment
/// operations and claim rejection have no equivalent partial target mutation,
/// so they always pass `None` and may not start or continue writes after expiry.
fn admit_signed_destination_write(
    authenticated: &AuthenticatedAppScope,
    proposal: &AppClaimsDecisionProposalV1,
    prepared_at: Option<DateTime<Utc>>,
) -> anyhow::Result<()> {
    let admission_now = Utc::now();
    authenticated.ensure_live_at(&admission_now).map_err(|_| {
        anyhow::Error::new(DestinationWriteNotAdmissible(
            "the authenticated app scope is not live at destination write admission",
        ))
    })?;

    let in_proposal_lifetime = |sample: &DateTime<Utc>| {
        sample.timestamp_millis() >= proposal.header.issued_at_ms
            && sample.timestamp_millis() < proposal.header.expires_at_ms
    };
    if let Some(prepared_at) = prepared_at {
        if !in_proposal_lifetime(&prepared_at) {
            return Err(anyhow::Error::new(DestinationWriteNotAdmissible(
                "the claim confirmation was not prepared during the signed command lifetime",
            )));
        }
        return Ok(());
    }
    if !in_proposal_lifetime(&admission_now) {
        return Err(anyhow::Error::new(DestinationWriteNotAdmissible(
            "the signed command expired before its first destination write",
        )));
    }
    Ok(())
}

fn apply_error(error: anyhow::Error) -> AppClaimsDecisionContributionError {
    if let Some(denied) = error.downcast_ref::<DestinationWriteNotAdmissible>() {
        return AppClaimsDecisionContributionError::NotAdmissible(denied.0);
    }
    AppClaimsDecisionContributionError::Apply(format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> String {
        magician_app_contract::contribution::content_digest(label.as_bytes())
    }

    fn claim_proposal() -> AppClaimsDecisionProposalV1 {
        AppClaimsDecisionProposalV1 {
            header: AppClaimsDecisionSourceHeaderV1 {
                contract_version: 1,
                destination_contract_id: APP_CLAIMS_DECISION_CONTRACT_ID.to_owned(),
                destination_contract_version: 1,
                destination_schema_digest: digest("destination"),
                proposal_id: "proposal:claim-1".to_owned(),
                proposal_revision: 1,
                scope_binding_ref: "scope:owner".to_owned(),
                installation_id: "installation:claims-review".to_owned(),
                installation_generation: 1,
                package_revision_ref: "package:claims-review".to_owned(),
                package_content_digest: digest("package"),
                grant_revision: 1,
                grant_authority_digest: digest("grant"),
                schema_revision: 1,
                schema_digest: digest("schema"),
                source_entity_name: "review_decision".to_owned(),
                source_record_id: "record:decision-1".to_owned(),
                source_record_revision: 1,
                source_record_digest: digest("source"),
                workflow_id: "confirm_claim".to_owned(),
                workflow_digest: digest("workflow"),
                action_id: "confirm_claim".to_owned(),
                action_digest: digest("action"),
                issued_at_ms: 1,
                expires_at_ms: 2,
                dedupe_key: "request:claim-1".to_owned(),
            },
            verb: AppClaimsDecisionVerbV1::ConfirmClaim,
            target: AppClaimsDecisionTargetV1::Claim {
                claim_id: "claim_1".to_owned(),
                expected_revision: 3,
            },
            by: "actor:owner".to_owned(),
            note: Some("Reviewed reason".to_owned()),
            proposal_digest: digest("proposal"),
        }
    }

    fn claim_source(payload_json: &str) -> Value {
        json!({
            "request_id": "request:claim-1",
            "decision_id": null,
            "target_kind": "claim",
            "target_id": "claim_1",
            "decision": "confirm_claim",
            "expected_revision": 3,
            "actor_ref": null,
            "reason": "Reviewed reason",
            "payload_json": payload_json,
            "apply_state": "recorded",
            "decided_at": "2026-09-02T00:00:00Z",
        })
    }

    #[test]
    fn audience_mapping_is_closed_and_identity_preserving() {
        for (source, expected) in [
            (
                AppClaimsDecisionAudienceKindV1::Engagement,
                AudienceKind::Engagement,
            ),
            (
                AppClaimsDecisionAudienceKindV1::Program,
                AudienceKind::Program,
            ),
            (
                AppClaimsDecisionAudienceKindV1::Account,
                AudienceKind::Account,
            ),
            (AppClaimsDecisionAudienceKindV1::Panel, AudienceKind::Panel),
            (
                AppClaimsDecisionAudienceKindV1::Person,
                AudienceKind::Person,
            ),
        ] {
            let audience = destination_audience(source, "shared-id".to_owned());
            assert_eq!(audience.kind, expected);
            assert_eq!(audience.id, "shared-id");
        }
    }

    #[test]
    fn source_oracle_keeps_the_seam_on_receipt_bearing_cas_methods() {
        let source = include_str!("claims_decision_contribution.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production half");
        for canonical_method in [
            ".ensure_live_at(",
            ".scope_binding_ref()",
            ".scope()",
            ".confirm_claim_at_revision_guarded(",
            ".reject_claim_at_revision_guarded(",
            ".record_commitment_from_claim_at_revision_guarded(",
            ".confirm_at_revision_guarded(",
        ] {
            assert!(
                production.contains(canonical_method),
                "missing canonical route {canonical_method}"
            );
        }
        for forbidden_route in [
            "apps::contribution_terminal",
            ".confirm_claim(",
            ".reject_claim(",
            ".record_commitment_from_claim(",
            ".confirm(",
        ] {
            assert!(
                !production.contains(forbidden_route),
                "claims decisions must not use legacy/non-CAS route {forbidden_route}"
            );
        }
    }

    #[test]
    fn source_semantics_require_the_canonical_exact_action_input() {
        let proposal = claim_proposal();
        let canonical = "{\"claim_id\":\"claim_1\",\"expected_revision\":3,\"reason\":\"Reviewed reason\",\"request_id\":\"request:claim-1\"}";
        assert!(validate_review_decision_source(&proposal, &claim_source(canonical)).is_ok());

        let reordered = "{\"request_id\":\"request:claim-1\",\"claim_id\":\"claim_1\",\"expected_revision\":3,\"reason\":\"Reviewed reason\"}";
        assert!(validate_review_decision_source(&proposal, &claim_source(reordered)).is_err());
    }

    #[test]
    fn source_semantics_refuse_a_borrowed_or_host_mutated_row() {
        let proposal = claim_proposal();
        let canonical = "{\"claim_id\":\"claim_1\",\"expected_revision\":3,\"reason\":\"Reviewed reason\",\"request_id\":\"request:claim-1\"}";
        let mut source = claim_source(canonical);
        source["target_id"] = json!("claim_2");
        assert!(validate_review_decision_source(&proposal, &source).is_err());

        let mut source = claim_source(canonical);
        source["actor_ref"] = json!("actor:owner");
        assert!(validate_review_decision_source(&proposal, &source).is_err());
    }

    // ── The staged-ingest seam ──────────────────────────────────────────────

    use crate::magician_v2::apps::models::{AppReference, AppRevision, AppScopeBindingRef};
    use crate::magician_v2::apps::records::AppScope;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    fn staged_ingestion() -> (tempfile::TempDir, TranscriptIngestion, OutwardScope) {
        let temporary = tempfile::tempdir().expect("temp dir");
        let ingestion = TranscriptIngestion::new(ArtifactV2Workspace::new(temporary.path()));
        (
            temporary,
            ingestion,
            OutwardScope::new("anonymous", "default"),
        )
    }

    fn moment(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn owner_scope(expires_at: &str) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: AppReference::parse("anonymous").expect("principal"),
                workspace: AppReference::parse("default").expect("workspace"),
            },
            AppScopeBindingRef::parse("scope_1").expect("scope binding"),
            AppReference::parse("actor:owner").expect("actor"),
            AppReference::parse("session:1").expect("session"),
            AppRevision::new(1).expect("authentication revision"),
            moment("2026-08-19T09:00:00Z"),
            moment(expires_at),
        )
        .expect("authenticated scope")
    }

    fn staged_row(apply_state: &str) -> Value {
        let mapping = json!({
            "speakers": { "a": "founder@example.com", "b": "alice@example.com" },
            "utterances": [
                { "speaker": "a", "text": "we can start in March" },
                { "speaker": "b", "text": "what does the pipeline look like" },
            ],
        })
        .to_string();
        json!({
            "ingest_id": "ingest-1",
            "transcript_text": "founder: we can start in March\nalice: and the pipeline?",
            "speaker_mapping_json": mapping,
            "audience_kind": "engagement",
            "audience_id": "eng-1",
            "outwardness_reason": "a diligence call with the counterparty",
            "actor_ref": Value::Null,
            "apply_state": apply_state,
            "act_ref": Value::Null,
            "submitted_at": "2026-08-19T09:30:00Z",
        })
    }

    fn host_resolution() -> AppStagedIngestHostResolution {
        AppStagedIngestHostResolution {
            roster: StagedIngestRoster::new()
                .resolved_ours("founder@example.com")
                .expect("ours")
                .resolved_counterparty("alice@example.com")
                .expect("counterparty"),
            effective_speaker: "founder@example.com".to_owned(),
            consequence_class: ConsequenceClass::BoundedCommunication,
            occurred_at: moment("2026-08-19T09:00:00Z"),
        }
    }

    /// The applier is the authenticated actor, and it lands as the register's
    /// extractor — the field the self-confirm rule reads. A caller cannot name
    /// somebody else there and keep the right to confirm what it queued.
    #[test]
    fn a_staged_apply_records_the_authenticated_actor_as_the_extractor() {
        let (_temporary, ingestion, scope) = staged_ingestion();

        let applied = apply_staged_app_ingest(
            &staged_row("recorded"),
            host_resolution(),
            &owner_scope("2026-08-19T12:00:00Z"),
            &ingestion,
            moment("2026-08-19T10:00:00Z"),
        )
        .expect("staged apply");

        assert_eq!(applied.transcript_key, "staged-ingest:ingest-1");
        let pending = ingestion.pending_claims(&scope).expect("pending claims");
        assert_eq!(
            pending
                .iter()
                .map(|claim| (claim.extracted_by.as_str(), claim.speaker.as_str()))
                .collect::<Vec<_>>(),
            vec![("actor:owner", "founder@example.com")],
            "only our side's words become a candidate, and the applier owns the extraction"
        );
        assert_eq!(pending[0].outward_act_ref, applied.outward_act_ref());
    }

    /// A row that already left `recorded` is not applied a second time. The
    /// package cannot re-present a settled row as fresh work.
    #[test]
    fn a_staged_apply_refuses_a_row_that_is_not_recorded() {
        let (_temporary, ingestion, scope) = staged_ingestion();

        let error = apply_staged_app_ingest(
            &staged_row("applied"),
            host_resolution(),
            &owner_scope("2026-08-19T12:00:00Z"),
            &ingestion,
            moment("2026-08-19T10:00:00Z"),
        )
        .expect_err("an already-applied row is inadmissible");

        assert!(
            matches!(
                error,
                AppClaimsDecisionContributionError::InvalidContract(_)
            ),
            "expected a contract refusal, got {error}"
        );
        assert!(
            ingestion.claims(&scope).expect("claims").is_empty(),
            "a refused staged row records nothing"
        );
    }

    /// The scope is re-sampled at the write, not at handler entry: registry
    /// I/O and worker admission can outlive the timestamp a route started with.
    #[test]
    fn a_staged_apply_refuses_an_expired_app_scope() {
        let (_temporary, ingestion, scope) = staged_ingestion();

        let error = apply_staged_app_ingest(
            &staged_row("recorded"),
            host_resolution(),
            &owner_scope("2026-08-19T09:30:00Z"),
            &ingestion,
            moment("2026-08-19T10:00:00Z"),
        )
        .expect_err("an expired scope is inadmissible");

        assert!(
            matches!(
                error,
                AppClaimsDecisionContributionError::NotAdmissible(
                    "the authenticated app scope is not live"
                )
            ),
            "expected the liveness refusal, got {error}"
        );
        assert!(ingestion.claims(&scope).expect("claims").is_empty());
    }

    /// The row names people; only the host says which side each is on. A name
    /// the roster cannot place refuses the whole request — so a package cannot
    /// get words into our column by naming somebody the host never resolved.
    #[test]
    fn a_staged_apply_refuses_a_name_the_host_cannot_place() {
        let (_temporary, ingestion, scope) = staged_ingestion();
        let partial = AppStagedIngestHostResolution {
            roster: StagedIngestRoster::new()
                .resolved_ours("founder@example.com")
                .expect("ours"),
            ..host_resolution()
        };

        let error = apply_staged_app_ingest(
            &staged_row("recorded"),
            partial,
            &owner_scope("2026-08-19T12:00:00Z"),
            &ingestion,
            moment("2026-08-19T10:00:00Z"),
        )
        .expect_err("an unplaceable name is inadmissible");

        assert!(
            matches!(error, AppClaimsDecisionContributionError::Apply(_)),
            "expected the register's refusal, got {error}"
        );
        assert!(
            ingestion.claims(&scope).expect("claims").is_empty(),
            "the refusal is before any write: no act, not a partial room"
        );
    }
}
