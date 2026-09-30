//! Destination-owned apply seam for app-proposed meeting capture controls.
//!
//! The meetings console may prepare one of five closed commands — listen, join,
//! pause, resume, stop — but it does not own the capture rails. This module
//! verifies a trusted-desktop signature, binds the reviewed app scope, proves
//! recent user intent for the two START verbs, then delegates to **the same
//! entries the first-party `/meetings` API serves**: `join_meeting_with_scope`
//! for the attendee rail, `start_passive_listener` for the listener rail, and
//! the two session managers' own transitions for pause/resume/stop.
//!
//! Three things this seam adds that authority alone does not give:
//!
//! * **Intent, not just authority.** Routing a start through the shared join
//!   path proves the app is allowed to reach it; it does not prove a person
//!   asked. A START therefore carries a surface gesture with an expiry, and the
//!   destination re-checks that expiry against its OWN clock immediately before
//!   it touches a manager. A signed start cannot be banked. The gesture is an
//!   app-supplied field, so admission turns it into a host-minted intent
//!   ticket — `AppMeetingStartIntentTicket` — that only a live interactive
//!   session earns, that is bounded by the host's own clock, and that the
//!   destination SPENDS durably before it touches a manager. One act starts at
//!   most one capture, and a rail cannot answer `Started` without having spent
//!   one.
//! * **Concurrency refusal.** A START is refused outright while any capture
//!   session is live on either rail. The app path is deliberately stricter than
//!   the first-party API here: an app must never be the thing that opens a
//!   second room's microphone while the first is still hearing one.
//! * **The durable audit.** Every accepted and every refused control lands in
//!   the shared capture-control audit, tagged with the app installation, the
//!   signed decision and the gesture.
//! * **The receipt ledger.** A signed decision whose fate is FINAL — the
//!   application in hand spent the envelope, or the owner declined — projects
//!   exactly one `control_receipt` row, keyed by that decision. A replay mints
//!   nothing, a losing concurrent submission mints nothing, and a command that
//!   never proved the owner's signature mints nothing at all, so the package's
//!   receipts view is never a place an app can write its own claim that
//!   capture happened. That row lands in a host-owned ledger here; the
//!   authenticated route publishes it into the package's `control_receipt`
//!   entity from THAT ledger — never from this seam's return value, which an
//!   app could otherwise see minted for a fate the destination never recorded.
//!
//! What it never does: create a capture by any path of its own. There is no
//! second session constructor here — only calls into the shared owners.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use magician_app_contract::contribution::{
    content_digest, AppMeetingControlOwnerDecisionEnvelopeV1, AppMeetingControlOwnerDecisionV1,
    AppMeetingControlProposalV1, AppMeetingControlSourceHeaderV1, AppMeetingControlTargetV1,
    AppMeetingControlVerbV1, APP_CONTRIBUTION_MAX_TTL_MS, APP_MEETING_CONTROL_CONTRACT_ID,
    APP_MEETING_CONTROL_MAX_CLOCK_SKEW_MS, APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
};
use magician_app_contract::macos_host::app_macos_desktop_identity_digest;

use crate::magician_v2::{
    apps::authority::{AppScopeAuthentication, AuthenticatedAppScope},
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::agent_resources::AgentResources,
    execution::compiled_providers::join_meeting_with_scope,
    json_traversal::canonical_json_bytes,
    media_seam::meeting::{
        meeting_manager, passive_meeting_manager, record_capture_control_detached,
        reserve_app_capture_start, resolve_meeting_thread, CaptureControlOrigin,
        CaptureControlOutcome, CaptureControlRecord, CaptureControlVerb, MarkerContext,
        MeetingConfig, MeetingStatus, PassiveListenerConfig, PassiveStatus,
        ScopedMeetingMemoryWriter,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMeetingControlContributionError {
    InvalidContract(String),
    InvalidSignature(String),
    NotAdmissible(&'static str),
    Apply(String),
}

impl std::fmt::Display for AppMeetingControlContributionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidContract(error) => {
                write!(formatter, "invalid app meeting-control contract: {error}")
            },
            Self::InvalidSignature(error) => {
                write!(
                    formatter,
                    "app meeting-control owner signature is invalid: {error}"
                )
            },
            Self::NotAdmissible(rule) => {
                write!(formatter, "inadmissible app meeting control: {rule}")
            },
            Self::Apply(error) => write!(formatter, "app meeting-control apply failed: {error}"),
        }
    }
}

impl std::error::Error for AppMeetingControlContributionError {}

/// Canonical destination result. The session id and the resolved thread are
/// returned so a caller can project its package receipt without inventing a
/// second authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AppMeetingControlApplication {
    Started {
        session_id: String,
        thread_id: String,
        mode: &'static str,
    },
    Paused {
        session_id: String,
    },
    Resumed {
        session_id: String,
    },
    Stopped {
        session_id: String,
        final_summary: Option<String>,
    },
    OwnerDeclined,
}

/// Current registry identity resolved by the authenticated host immediately
/// before destination apply. The signed header must match every field; an
/// enabled installation id alone is not authority after update, reinstall, or
/// grant/schema replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMeetingControlCurrentAuthority {
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

impl AppMeetingControlCurrentAuthority {
    fn matches_header(&self, header: &AppMeetingControlSourceHeaderV1) -> bool {
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
pub fn validate_app_meeting_control(
    proposal: &AppMeetingControlProposalV1,
) -> Result<(), AppMeetingControlContributionError> {
    proposal
        .validate()
        .map_err(|error| AppMeetingControlContributionError::InvalidContract(error.to_string()))?;
    if proposal.header.destination_contract_id != APP_MEETING_CONTROL_CONTRACT_ID {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "meeting controls require the magician.meeting-control contract",
        ));
    }
    Ok(())
}

/// Apply one trusted-desktop capture control through the shared first-party
/// owners.
///
/// `resources` supplies the scoped memory writer, crash-marker root and event
/// broadcaster the first-party handlers pass — the same values, so a capture
/// started here is indistinguishable downstream from one started at
/// `/meetings/listen`.
#[allow(clippy::too_many_arguments)]
pub async fn apply_signed_app_meeting_control(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    desktop_identity_public_key_hex: &str,
    expected_desktop_pairing_generation: u64,
    expected_desktop_identity_key_id: &str,
    expected_desktop_identity_digest: &str,
    expected_installation_id: &str,
    current_authority: &AppMeetingControlCurrentAuthority,
    authenticated: &AuthenticatedAppScope,
    resources: Arc<AgentResources>,
    now: DateTime<Utc>,
) -> Result<AppMeetingControlApplication, AppMeetingControlContributionError> {
    let admitted = admit(
        envelope,
        desktop_identity_public_key_hex,
        expected_desktop_pairing_generation,
        expected_desktop_identity_key_id,
        expected_desktop_identity_digest,
        expected_installation_id,
        current_authority,
        authenticated,
        now,
    );
    let proposal = &envelope.review.proposal;
    let scope = authenticated.scope();
    let (principal, workspace) = (
        scope.principal.as_str().to_owned(),
        scope.workspace.as_str().to_owned(),
    );
    let workdirs_root = scope_workdirs_root(&resources.artifact_workspace, &principal, &workspace);

    // A refused control is audited exactly like an accepted one. An operator
    // asking "did an app try to open my microphone" must not have to infer the
    // answer from an absence.
    let audit = |outcome: CaptureControlOutcome,
                 session_id: Option<String>,
                 thread_id: Option<String>,
                 detail: Option<String>| {
        record_capture_control_detached(
            &workdirs_root,
            CaptureControlRecord::new(
                CaptureControlOrigin::AppControlDestination,
                audit_verb(proposal.verb),
                outcome,
                principal.clone(),
                workspace.clone(),
            )
            .with_session(session_id)
            .with_meeting(
                thread_id,
                start_title(&proposal.target).map(str::to_owned),
                proposal.target.meeting_url().map(str::to_owned),
            )
            .with_app(
                Some(proposal.header.installation_id.clone()),
                Some(envelope.decision_id.clone()),
                proposal
                    .gesture
                    .as_ref()
                    .map(|gesture| gesture.gesture_id.clone()),
            )
            .with_detail(detail),
        )
    };

    // A START's ticket rides out of admission; every other verb's is `None`.
    // It is minted before the owner's decision is read, which costs nothing: a
    // mint writes no ledger, so a declined command drops its ticket unspent.
    let intent = match admitted {
        Ok(intent) => intent,
        Err(refusal) => {
            let AppMeetingControlRefusal {
                error,
                signed_authority_proven,
            } = refusal;
            audit(
                CaptureControlOutcome::Refused,
                proposal.target.session_id().map(str::to_owned),
                None,
                Some(error.to_string()),
            );
            let refused = Err(error);
            // Refused before dispatch, so this call never claimed the decision.
            // Another application of the same envelope may be inside a manager
            // right now, and its fate — not this one's — is the row.
            record_control_receipt(
                &workdirs_root,
                envelope,
                &refused,
                signed_authority_proven,
                DecisionSpend::unspent(),
            );
            return refused;
        },
    };
    if envelope.decision != AppMeetingControlOwnerDecisionV1::Accept {
        audit(
            CaptureControlOutcome::Refused,
            proposal.target.session_id().map(str::to_owned),
            None,
            Some("the owner did not accept this command".to_owned()),
        );
        let declined = Ok(AppMeetingControlApplication::OwnerDeclined);
        // A decline is final by construction: the decision is inside the
        // signature, so re-submitting the same envelope declines again. It is
        // the one final fate that spends nothing.
        record_control_receipt(
            &workdirs_root,
            envelope,
            &declined,
            true,
            DecisionSpend::unspent(),
        );
        return declined;
    }

    // Whether THIS call is the one that spends the envelope. Dispatch fills it
    // in when its claim succeeds; nothing else may.
    let mut spend = DecisionSpend::unspent();
    let applied = dispatch(
        envelope,
        &resources,
        authenticated,
        &principal,
        &workspace,
        &workdirs_root,
        &mut spend,
        intent,
        now,
    )
    .await;
    match &applied {
        Ok(application) => audit(
            CaptureControlOutcome::Accepted,
            application_session_id(application).map(str::to_owned),
            application_thread_id(application).map(str::to_owned),
            None,
        ),
        Err(error) => audit(
            CaptureControlOutcome::Refused,
            proposal.target.session_id().map(str::to_owned),
            None,
            Some(error.to_string()),
        ),
    }
    // The audit records every attempt; the ledger records the fates. A refusal
    // that never spent the envelope is still retryable and mints nothing.
    record_control_receipt(&workdirs_root, envelope, &applied, true, spend);
    applied
}

/// A refusal, plus whether the command had proven BOTH the paired desktop's
/// signature and its binding to the current authority when it was refused.
///
/// Only the receipt ledger needs the second field, and it needs it badly:
/// `decision_id` is a digest over content the app itself supplies and carries
/// no signature of its own, so anyone who can reach this seam can post an
/// envelope bearing a real decision's id. A row minted for one of those would
/// be an app writing its own evidence that the owner saw its request.
struct AppMeetingControlRefusal {
    error: AppMeetingControlContributionError,
    signed_authority_proven: bool,
}

/// Everything that must hold before a manager is touched. Split out so the
/// audit above can record exactly one refusal reason for any of them.
///
/// A START comes back carrying the host-minted intent ticket the destination
/// must spend before it reaches a manager; every other verb comes back with
/// `None`, because there is no act to spend for a risk-reducing control.
#[allow(clippy::too_many_arguments)]
fn admit(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    desktop_identity_public_key_hex: &str,
    expected_desktop_pairing_generation: u64,
    expected_desktop_identity_key_id: &str,
    expected_desktop_identity_digest: &str,
    expected_installation_id: &str,
    current_authority: &AppMeetingControlCurrentAuthority,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<Option<AppMeetingStartIntentTicket>, AppMeetingControlRefusal> {
    prove_signed_authority(
        envelope,
        desktop_identity_public_key_hex,
        expected_desktop_pairing_generation,
        expected_desktop_identity_key_id,
        expected_desktop_identity_digest,
        expected_installation_id,
        current_authority,
        authenticated,
        now,
    )
    .map_err(|error| AppMeetingControlRefusal {
        error,
        signed_authority_proven: false,
    })?;
    admit_proven_command(envelope, current_authority, authenticated, now).map_err(|error| {
        AppMeetingControlRefusal {
            error,
            signed_authority_proven: true,
        }
    })
}

/// Prove the command's identity: the paired desktop signed it, and it names the
/// destination, installation, grant, package, schema, workflow, action and
/// source-record authority that is live right now.
///
/// Split from the admissibility rules below because the receipt ledger draws
/// its line exactly here — see [`AppMeetingControlRefusal`]. The order is
/// unchanged from when this was one function: nothing about the proposal is
/// believed before the signature over it verifies.
#[allow(clippy::too_many_arguments)]
fn prove_signed_authority(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    desktop_identity_public_key_hex: &str,
    expected_desktop_pairing_generation: u64,
    expected_desktop_identity_key_id: &str,
    expected_desktop_identity_digest: &str,
    expected_installation_id: &str,
    current_authority: &AppMeetingControlCurrentAuthority,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<(), AppMeetingControlContributionError> {
    authenticated.ensure_live_at(&now).map_err(|_| {
        AppMeetingControlContributionError::NotAdmissible("the authenticated app scope is not live")
    })?;
    // `AppMacosHostWireError` is a closed `Debug`-only enum, deliberately: its
    // variants name where a wire value failed, not anything about the value.
    let recomputed_identity = app_macos_desktop_identity_digest(
        expected_desktop_identity_key_id,
        desktop_identity_public_key_hex,
    )
    .map_err(|error| AppMeetingControlContributionError::InvalidSignature(format!("{error:?}")))?;
    if recomputed_identity != expected_desktop_identity_digest
        || envelope.review.desktop_pairing_generation != expected_desktop_pairing_generation
        || envelope.review.desktop_identity_key_id != expected_desktop_identity_key_id
        || envelope.review.desktop_identity_digest != expected_desktop_identity_digest
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command is not bound to the active paired desktop identity",
        ));
    }
    envelope
        .verify_signature(desktop_identity_public_key_hex)
        .map_err(|error| AppMeetingControlContributionError::InvalidSignature(error.to_string()))?;
    let proposal = &envelope.review.proposal;
    validate_app_meeting_control(proposal)?;
    if proposal.header.scope_binding_ref != authenticated.scope_binding_ref().as_str() {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command does not belong to the authenticated app scope",
        ));
    }
    if proposal.header.installation_id != expected_installation_id {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command does not belong to the expected app installation",
        ));
    }
    if !current_authority.matches_header(&proposal.header) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command does not match the current destination, installation, grant, package, schema, workflow, action, and source-record authority",
        ));
    }
    Ok(())
}

/// The admissibility rules for a command whose signature and authority binding
/// are already proven: the live source head says what the command says, the
/// actor is the authenticated one, the command is inside its own lifetime, the
/// owner signed it recently, and a START carries fresh intent.
fn admit_proven_command(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    current_authority: &AppMeetingControlCurrentAuthority,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<Option<AppMeetingStartIntentTicket>, AppMeetingControlContributionError> {
    let proposal = &envelope.review.proposal;
    validate_control_request_source(proposal, &current_authority.source_record_payload)?;
    if proposal.by != authenticated.actor_ref().as_str() {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command actor is not the authenticated actor",
        ));
    }
    let now_ms = now.timestamp_millis();
    if now_ms < proposal.header.issued_at_ms || now_ms >= proposal.header.expires_at_ms {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command is outside its own lifetime",
        ));
    }
    // The anchor. Everything else in this function is relative to values the
    // app chose; `decided_at_ms` is inside the desktop's signature, so it is
    // the one field that ties this command to real elapsed time. Without it a
    // START sealed with a lifetime thirty days out satisfies every relative
    // rule and applies, once, a month after it was signed.
    if !envelope.is_recently_decided_at(now_ms) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the owner's signature on this command is not recent",
        ));
    }
    // The intent check. A START without a gesture that is fresh RIGHT NOW is
    // refused, whatever the signature says: a banked start is exactly the
    // failure the gesture exists to prevent. The gesture is an app-supplied
    // field though, so what leaves this function is not the gesture but the
    // host's own ticket over it — see [`AppMeetingStartIntentTicket`].
    if proposal.verb.starts_capture() {
        return AppMeetingStartIntentTicket::mint(envelope, authenticated, now_ms).map(Some);
    }
    if proposal.gesture.is_some() {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "only a capture start carries a surface gesture",
        ));
    }
    Ok(None)
}

/// Host-minted, short-lived, single-use proof that THIS capture start was asked
/// for in a live interactive session and that the act behind it has not been
/// spent before.
///
/// The gap this closes is narrow and worth naming exactly. The owner's
/// signature proves the owner SAW a display of the window. The gesture proves
/// recency and names an act — but it is a field the app fills in, so on its own
/// it attests nothing the app could not have written. The ticket is the host's
/// own object over that field, and it adds four things the gesture cannot:
///
/// * **A live interactive session.** Only the two human-bearing authentication
///   classes mint one. A server-owned worker or a task execution holds a
///   perfectly valid scope and can still stop, pause and resume a capture — but
///   it can never be the principal that OPENS a microphone, because it has no
///   act to present. The authenticated route asks the same question today, and
///   that is exactly why the destination asks it again: a rule that lives only
///   in one handler is a rule the next handler will not have.
/// * **One act, one capture.** The applied-decision ledger binds one envelope
///   to one application. It cannot bind one gesture to one capture: the
///   decision id folds in the sealed proposal, so two proposals sealed from a
///   single click are two decisions and both would apply. The redemption below
///   claims the ACT durably, so a second start from one click is refused
///   whatever envelope carries it.
/// * **A host-owned window.** [`INTENT_TICKET_TTL_MS`] bounds the ticket on the
///   destination's own clock. Every other window in a START is a field the app
///   chose, capped by the contract; this one it cannot widen at all.
/// * **A binding, not a token.** The ticket names the decision, the sealed
///   proposal, the verb, the installation, the act and the authenticated scope,
///   and [`Self::redeem`] re-checks every one against the command actually
///   being applied.
///
/// Move-only by construction, following this crate's other non-wire proofs:
/// private fields, no `Deserialize`, no `Clone`, no `Debug` — a value that
/// cannot be printed cannot be leaked into a log — and consumed by value at
/// redemption. It is module-private on top of that, so nothing outside this
/// destination can even name one.
///
/// **What it still does not prove: that a human, rather than the app's own
/// script, produced the click.** `surface_session_id` is read frame-side and no
/// host registry witnesses the act itself; registering the act at the surface
/// bridge is what would make that provable, and it is owed work rather than a
/// claim made here.
struct AppMeetingStartIntentTicket {
    scope_binding_ref: String,
    actor_ref: String,
    session_ref: String,
    installation_id: String,
    surface_session_id: String,
    gesture_id: String,
    decision_id: String,
    proposal_digest: String,
    verb: AppMeetingControlVerbV1,
    /// Host clock. Bounded below by [`INTENT_TICKET_TTL_MS`] and never widened
    /// by an app-supplied window — only narrowed by one.
    expires_at_ms: i64,
}

/// Longest a minted intent ticket may sit unredeemed.
///
/// The mint-to-redeem gap is real work: the start reservation waits up to 45
/// seconds for a browser launch it cannot hurry, and registry and pairing I/O
/// sit in front of that. This is that bound plus headroom, and nothing more —
/// the ticket is an act, not a session. The gesture ceiling usually dominates;
/// this constant is what holds when it does not, because it is the one window
/// in a START that no app-supplied field can widen.
const INTENT_TICKET_TTL_MS: i64 = 60_000;

impl AppMeetingStartIntentTicket {
    /// Mint the ticket for one proven START, or refuse the command.
    ///
    /// Pure: minting writes nothing. A command that is admitted and then
    /// declined by the owner, or refused later on a clock rule, drops its
    /// ticket unspent and burns no act.
    fn mint(
        envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
        authenticated: &AuthenticatedAppScope,
        now_ms: i64,
    ) -> Result<Self, AppMeetingControlContributionError> {
        let proposal = &envelope.review.proposal;
        if !proposal.verb.starts_capture() {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "only a capture start earns a host-minted intent ticket",
            ));
        }
        // Exhaustive on purpose. A new authentication class must be classified
        // here by whoever adds it, and until then this refuses to compile
        // rather than silently admitting a principal nobody has thought about.
        match authenticated.authentication() {
            AppScopeAuthentication::AuthenticatedSession
            | AppScopeAuthentication::TrustedLoopbackSingleUser => {},
            AppScopeAuthentication::TaskExecution
            | AppScopeAuthentication::SystemWorker
            | AppScopeAuthentication::ReviewedBackgroundLaunch
            | AppScopeAuthentication::TrustedSystemPackageHost => {
                return Err(AppMeetingControlContributionError::NotAdmissible(
                    "a capture start requires a live interactive session; a server-owned principal has no intent to present",
                ));
            },
        }
        let gesture =
            proposal
                .gesture
                .as_ref()
                .ok_or(AppMeetingControlContributionError::NotAdmissible(
                    "a capture start requires a surface gesture",
                ))?;
        if !gesture.is_fresh_at(now_ms) {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "the surface gesture behind this capture start is not fresh",
            ));
        }
        // The narrowest of the three windows wins, and the host's own is one of
        // them. `checked_add` because `now_ms` is an i64 millisecond clock and
        // a saturating add would silently produce the widest window instead of
        // the narrowest.
        let expires_at_ms = now_ms
            .checked_add(INTENT_TICKET_TTL_MS)
            .map(|host_bound| {
                host_bound
                    .min(gesture.expires_at_ms)
                    .min(proposal.header.expires_at_ms)
            })
            .ok_or(AppMeetingControlContributionError::NotAdmissible(
                "the destination could not bound this capture start's intent window",
            ))?;
        if expires_at_ms <= now_ms {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "this capture start's intent window is already closed",
            ));
        }
        Ok(Self {
            scope_binding_ref: authenticated.scope_binding_ref().as_str().to_owned(),
            actor_ref: authenticated.actor_ref().as_str().to_owned(),
            session_ref: authenticated.session_ref().as_str().to_owned(),
            installation_id: proposal.header.installation_id.clone(),
            surface_session_id: gesture.surface_session_id.clone(),
            gesture_id: gesture.gesture_id.clone(),
            decision_id: envelope.decision_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            verb: proposal.verb,
            expires_at_ms,
        })
    }

    /// Spend the ticket on the command in hand, or refuse the start.
    ///
    /// Consumed by value: the in-process value is gone whatever the outcome, so
    /// a redemption cannot be retried by holding the ticket. The durable half
    /// is [`claim_spent_gesture`], which is what makes single-use survive a
    /// second submission, a second process and a restart.
    ///
    /// Every field is re-checked against the command actually being applied.
    /// Inside one application those are the same values the mint read, and that
    /// is the point: the ticket is a BINDING, so one that reached a different
    /// command, a different act or a different scope names nothing it may
    /// spend. `now_ms` is the clock re-sampled inside the start reservation,
    /// never the one admission used.
    fn redeem(
        self,
        workdirs_root: &std::path::Path,
        envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
        authenticated: &AuthenticatedAppScope,
        now_ms: i64,
    ) -> Result<SpentStartIntent, AppMeetingControlContributionError> {
        let proposal = &envelope.review.proposal;
        let gesture =
            proposal
                .gesture
                .as_ref()
                .ok_or(AppMeetingControlContributionError::NotAdmissible(
                    "a capture start requires a surface gesture",
                ))?;
        if self.decision_id != envelope.decision_id
            || self.proposal_digest != proposal.proposal_digest
            || self.verb != proposal.verb
            || self.installation_id != proposal.header.installation_id
            || self.surface_session_id != gesture.surface_session_id
            || self.gesture_id != gesture.gesture_id
            || self.scope_binding_ref != authenticated.scope_binding_ref().as_str()
            || self.actor_ref != authenticated.actor_ref().as_str()
            || self.session_ref != authenticated.session_ref().as_str()
        {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "the host-minted intent ticket does not name this capture start",
            ));
        }
        if now_ms >= self.expires_at_ms {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "the host-minted intent ticket for this capture start expired",
            ));
        }
        // Re-asked against the same clock: the ticket's own window may be the
        // host bound rather than the gesture's, and an act that went stale
        // while the reservation was held is not intent any more.
        if !gesture.is_fresh_at(now_ms) {
            return Err(AppMeetingControlContributionError::NotAdmissible(
                "the surface gesture behind this capture start went stale before the start",
            ));
        }
        claim_spent_gesture(workdirs_root, &self)?;
        Ok(SpentStartIntent {
            gesture_id: self.gesture_id,
        })
    }
}

/// The redeemed half of an intent ticket: one act, already burnt.
///
/// It exists so that "a capture started" is a statement only a spent act can
/// make. [`Self::into_started`] is the single constructor of
/// [`AppMeetingControlApplication::Started`] in this module's production half,
/// and the pin below holds it there — a rail that reached a manager without
/// redeeming a ticket would have no way to say a capture began.
struct SpentStartIntent {
    gesture_id: String,
}

impl SpentStartIntent {
    fn into_started(
        self,
        session_id: String,
        thread_id: String,
        mode: &'static str,
    ) -> AppMeetingControlApplication {
        // The act stops here. The audit already tags the gesture from the
        // signed proposal and no receipt row carries one, so the id is dropped
        // rather than forwarded; naming it keeps this value a record of WHICH
        // act was spent instead of an anonymous token.
        drop(self.gesture_id);
        AppMeetingControlApplication::Started {
            session_id,
            thread_id,
            mode,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    resources: &Arc<AgentResources>,
    authenticated: &AuthenticatedAppScope,
    principal: &str,
    workspace: &str,
    workdirs_root: &std::path::Path,
    spend: &mut DecisionSpend,
    intent: Option<AppMeetingStartIntentTicket>,
    now: DateTime<Utc>,
) -> Result<AppMeetingControlApplication, AppMeetingControlContributionError> {
    let proposal = &envelope.review.proposal;
    // Fail closed on a class/ticket mismatch before any arm runs. Admission
    // mints for exactly the START class, so either half being wrong means the
    // intent gate did not run over this command — and a risk-reducing verb
    // arriving with an act to spend is as wrong as a start arriving without
    // one.
    if intent.is_some() != proposal.verb.starts_capture() {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the host-minted intent ticket does not match this command's class",
        ));
    }
    match proposal.verb {
        AppMeetingControlVerbV1::Listen | AppMeetingControlVerbV1::Join => {
            let intent = intent.ok_or(AppMeetingControlContributionError::NotAdmissible(
                "a capture start requires a host-minted intent ticket",
            ))?;
            start_capture(
                envelope,
                resources,
                authenticated,
                principal,
                workspace,
                workdirs_root,
                spend,
                intent,
                now,
            )
            .await
        },
        AppMeetingControlVerbV1::Pause | AppMeetingControlVerbV1::Resume => {
            let session_id = live_session_target(proposal)?;
            ensure_session_belongs_to_scope(session_id, principal, workspace).await?;
            // Claim LAST, immediately before the manager call. Claiming at
            // admission would burn the envelope on refusals that provably
            // touched nothing — wrong scope, stale gesture, a capture already
            // live — forcing a second owner signature to retry something that
            // never happened.
            consume_decision(workdirs_root, &envelope.decision_id, spend)?;
            let pause = proposal.verb == AppMeetingControlVerbV1::Pause;
            set_paused(session_id, pause)
                .await
                .map_err(AppMeetingControlContributionError::Apply)?;
            Ok(if pause {
                AppMeetingControlApplication::Paused {
                    session_id: session_id.to_owned(),
                }
            } else {
                AppMeetingControlApplication::Resumed {
                    session_id: session_id.to_owned(),
                }
            })
        },
        AppMeetingControlVerbV1::Stop => {
            let session_id = live_session_target(proposal)?;
            ensure_session_belongs_to_scope(session_id, principal, workspace).await?;
            consume_decision(workdirs_root, &envelope.decision_id, spend)?;
            // Same rail-prefix dispatch the API's stop handler uses, so each
            // rail's own error surfaces instead of being masked by the other's
            // "unknown session" refusal.
            let final_summary = if session_id.starts_with(PASSIVE_SESSION_PREFIX) {
                passive_meeting_manager()
                    .request_stop(session_id)
                    .await
                    .map(|(_, latest_summary)| latest_summary)
                    .map_err(AppMeetingControlContributionError::Apply)?
            } else {
                meeting_manager()
                    .leave(session_id)
                    .await
                    .map_err(AppMeetingControlContributionError::Apply)?
            };
            Ok(AppMeetingControlApplication::Stopped {
                session_id: session_id.to_owned(),
                final_summary,
            })
        },
    }
}

const PASSIVE_SESSION_PREFIX: &str = "listen-";

/// Refuse to act on a capture that is not this scope's.
///
/// Both registries are process-global and keyed by session id ALONE. Without
/// this check a signed command naming another principal's session id would
/// stop that capture and return its final summary in the response body — the
/// exact cross-scope replay the claims destination forbids by deriving its
/// storage scope from the authenticated value. A session with no recorded
/// scope (a seam-level spawn) is refused too: "unknown" is not "mine".
async fn ensure_session_belongs_to_scope(
    session_id: &str,
    principal: &str,
    workspace: &str,
) -> Result<(), AppMeetingControlContributionError> {
    let owner = if session_id.starts_with(PASSIVE_SESSION_PREFIX) {
        passive_meeting_manager()
            .status(session_id)
            .await
            .map(|view| view.scope)
    } else {
        meeting_manager()
            .status(session_id)
            .await
            .map(|view| view.scope)
    };
    match owner {
        Some(Some((session_principal, session_workspace)))
            if session_principal == principal && session_workspace == workspace =>
        {
            Ok(())
        },
        // One refusal for "no such session", "not yours", and "no recorded
        // owner". Distinguishing them would confirm the existence of another
        // scope's session to a caller that may not name it.
        _ => Err(AppMeetingControlContributionError::NotAdmissible(
            "no capture session with that id belongs to this scope",
        )),
    }
}

async fn set_paused(session_id: &str, paused: bool) -> Result<(), String> {
    if session_id.starts_with(PASSIVE_SESSION_PREFIX) {
        passive_meeting_manager()
            .set_paused(session_id, paused)
            .await
    } else {
        meeting_manager().set_paused(session_id, paused).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_capture(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    resources: &Arc<AgentResources>,
    authenticated: &AuthenticatedAppScope,
    principal: &str,
    workspace: &str,
    workdirs_root: &std::path::Path,
    spend: &mut DecisionSpend,
    intent: AppMeetingStartIntentTicket,
    _admitted_at: DateTime<Utc>,
) -> Result<AppMeetingControlApplication, AppMeetingControlContributionError> {
    let proposal = &envelope.review.proposal;
    let AppMeetingControlTargetV1::NewCapture {
        url,
        title,
        date,
        capture_mic,
    } = &proposal.target
    else {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "a capture start requires a new-capture target",
        ));
    };

    // The probe and the start are ONE critical section. Releasing between them
    // lets two signed starts both observe "nothing live" and both proceed —
    // two rooms' microphones open, which the refusal exists to prevent.
    let _reservation = reserve_app_capture_start().await.map_err(|_| {
        AppMeetingControlContributionError::Apply(
            "another capture start is still in flight; retry once it settles".to_owned(),
        )
    })?;

    // Re-sample EVERYTHING time-dependent here, not at handler entry. Registry
    // and pairing I/O, the blocking-worker queue and this reservation all sit
    // between admission and this line, and for a capture start that gap is
    // precisely what the gesture's expiry measures.
    let admission_now = Utc::now();
    authenticated.ensure_live_at(&admission_now).map_err(|_| {
        AppMeetingControlContributionError::NotAdmissible(
            "the authenticated app scope expired before the capture start",
        )
    })?;
    let admission_ms = admission_now.timestamp_millis();
    if admission_ms < proposal.header.issued_at_ms || admission_ms >= proposal.header.expires_at_ms
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the signed command expired before the capture start",
        ));
    }
    let gesture =
        proposal
            .gesture
            .as_ref()
            .ok_or(AppMeetingControlContributionError::NotAdmissible(
                "a capture start requires a surface gesture",
            ))?;
    if !gesture.is_fresh_at(admission_ms) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the surface gesture behind this capture start went stale before the start",
        ));
    }
    if !envelope.is_recently_decided_at(admission_ms) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the owner's signature on this capture start went stale before the start",
        ));
    }

    // Concurrency refusal, sampled inside the reservation: an app must never
    // open a second room's ears while the first is open. Deliberately stricter
    // than the first-party API, which the operator drives directly.
    if live_capture_exists().await {
        return Err(AppMeetingControlContributionError::Apply(
            // Never name the live session: it may belong to another scope, and
            // an id leaked here is directly actionable as a stop target.
            "a capture session is already live; stop it before starting another".to_owned(),
        ));
    }

    // Last gates before a manager, and still inside the reservation so neither
    // claim can interleave with a second application.
    //
    // The ACT is spent before the envelope, and the order is the whole point.
    // The applied-decision ledger answers "has this envelope been applied";
    // only this one answers "has this click already opened a microphone",
    // which is a different question because two proposals sealed from one
    // click are two decisions. A start that got past the envelope claim and
    // then failed the act claim would have burnt a retryable envelope on an
    // act that was never spendable.
    let intent = intent.redeem(workdirs_root, envelope, authenticated, admission_ms)?;
    consume_decision(workdirs_root, &envelope.decision_id, spend)?;

    let resolved = resolve_meeting_thread(url.as_deref(), title.as_deref(), date.as_deref());
    let marker = MarkerContext::for_scope(
        crate::magician_v2::subprocess_owners::workdirs_root(
            &resources.artifact_workspace,
            principal,
            workspace,
        ),
        principal,
        workspace,
    );

    match proposal.verb {
        AppMeetingControlVerbV1::Listen => {
            let writer = Arc::new(ScopedMeetingMemoryWriter::new(
                Arc::clone(resources),
                principal.to_owned(),
                workspace.to_owned(),
            ));
            let config = PassiveListenerConfig {
                thread: resolved.thread.clone(),
                session_title: resolved.session_title.clone(),
                title: title.clone(),
                url: url.clone(),
                date: resolved.date.clone(),
                capture_mic: *capture_mic,
                summarize_every_turns: 40,
                audio_profile: None,
                audio_stage_options: Default::default(),
            };
            let session_id = crate::magician_v2::media_seam::meeting::start_passive_listener(
                config,
                writer,
                Some((principal.to_owned(), workspace.to_owned())),
                Some(marker),
                resources.event_broadcaster.clone(),
            )
            .await
            .map_err(AppMeetingControlContributionError::Apply)?;
            Ok(intent.into_started(session_id, resolved.thread, "passive"))
        },
        AppMeetingControlVerbV1::Join => {
            let meet_url =
                url.as_deref()
                    .ok_or(AppMeetingControlContributionError::NotAdmissible(
                        "an attendee join requires a meeting url",
                    ))?;
            let mut config = MeetingConfig {
                meet_url: meet_url.to_owned(),
                ..Default::default()
            };
            config.title = title.clone();
            config.meeting_date = date.clone();
            // THE single join path. There is no second attendee constructor in
            // this module, and the oracle pin proves it stays that way.
            let session_id = join_meeting_with_scope(
                Some((
                    Arc::clone(resources),
                    principal.to_owned(),
                    workspace.to_owned(),
                )),
                config,
                CaptureControlOrigin::AppControlDestination,
            )
            .await
            .map_err(AppMeetingControlContributionError::Apply)?;
            Ok(intent.into_started(session_id, resolved.thread, "attendee"))
        },
        _ => Err(AppMeetingControlContributionError::NotAdmissible(
            "only listen and join start a capture",
        )),
    }
}

/// Whether ANY capture is live on either rail. Deliberately returns a bool, not
/// an id: the registries are process-global, so an id from here may belong to a
/// scope the caller cannot name.
async fn live_capture_exists() -> bool {
    if passive_meeting_manager()
        .list()
        .await
        .iter()
        .any(|view| view.status == PassiveStatus::Listening)
    {
        return true;
    }
    meeting_manager()
        .list()
        .await
        .iter()
        .any(|row| !matches!(row.status, MeetingStatus::Left | MeetingStatus::Failed))
}

/// Directory (under the scope's capability workdirs root) holding one marker
/// per applied signed command.
const APPLIED_DECISION_DIR: &str = "meeting_control_applied";

/// The one refusal that must never mint a receipt.
///
/// It means an earlier application already claimed this decision, so the row
/// for it was written then — or was lost with the process that wrote it, and a
/// `refused` row now would report a capture that really started as one that
/// never did. Named rather than repeated so the receipt gate and the refusal
/// text cannot drift apart.
const ALREADY_APPLIED_REFUSAL: &str = "this signed capture command has already been applied";

/// Outcome of trying to claim a decision id for exactly-once application.
enum DecisionClaim {
    /// This process created the marker; nobody has applied this command.
    Fresh,
    /// The marker already existed.
    AlreadyApplied,
    /// The claim could not be recorded at all. Callers must fail CLOSED: an
    /// unrecorded claim means an unbounded replay window on a capture start.
    Unavailable(String),
}

/// Whether THIS application is the one that spent the envelope.
///
/// The applied-command marker cannot answer that. It says only that SOME call
/// claimed the decision, and a call that read a marker it did not create would
/// mistake another application's claim for its own fate — see
/// [`record_control_receipt`], which is the only thing that asks. So the answer
/// travels forward from the claim instead of being read back off disk: it
/// starts false, and only a fresh claim turns it true.
#[derive(Debug, Clone, Copy)]
struct DecisionSpend(bool);

impl DecisionSpend {
    /// The only starting value. A call proves a spend by claiming, never by
    /// looking.
    const fn unspent() -> Self {
        Self(false)
    }

    /// Called on a fresh claim and nowhere else.
    fn mark_spent(&mut self) {
        self.0 = true;
    }

    const fn spent_here(self) -> bool {
        self.0
    }
}

/// Claim a decision id, or refuse the command.
///
/// The thin wrapper the dispatch arms call: it turns the three claim outcomes
/// into the destination's own refusals so the claim can sit immediately before
/// each manager call rather than at admission. `spend` comes back marked only
/// on the claim that created the marker, so the caller carries proof of its own
/// spend rather than a fact about the envelope.
fn consume_decision(
    workdirs_root: &std::path::Path,
    decision_id: &str,
    spend: &mut DecisionSpend,
) -> Result<(), AppMeetingControlContributionError> {
    match claim_decision(workdirs_root, decision_id) {
        DecisionClaim::Fresh => {
            spend.mark_spent();
            Ok(())
        },
        DecisionClaim::AlreadyApplied => Err(AppMeetingControlContributionError::NotAdmissible(
            ALREADY_APPLIED_REFUSAL,
        )),
        // Fail CLOSED. Without a durable claim there is no replay bound, and an
        // unbounded replay of a capture start is the worst outcome here.
        DecisionClaim::Unavailable(error) => {
            warn!(
                error = %error,
                "App meeting-control decision claim was unavailable"
            );
            Err(AppMeetingControlContributionError::NotAdmissible(
                "the destination could not record this command as applied",
            ))
        },
    }
}

/// How long an applied-decision marker is retained.
///
/// Every signed command carries `header.expires_at_ms`, and admission already
/// refuses one past its own lifetime, so a marker older than the contract's
/// maximum proposal TTL can no longer gate anything. Retaining it forever would
/// grow one small file per capture control without bound.
const APPLIED_DECISION_RETAIN: std::time::Duration =
    std::time::Duration::from_millis(APP_CONTRIBUTION_MAX_TTL_MS as u64);

/// How long a spent-act marker is retained.
///
/// Much tighter than the applied-decision retention above, and for a reason
/// that is worth stating rather than inheriting: a gesture is only ever fresh
/// inside its own ceiling-wide window, and the marker is written while it is
/// fresh, so no gesture whose act was spent longer ago than the ceiling can be
/// fresh again. The clock-skew tolerance is headroom against a host clock that
/// moved under the filesystem's mtime, not a widening of the rule.
const SPENT_GESTURE_RETAIN: std::time::Duration = std::time::Duration::from_millis(
    (APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS + APP_MEETING_CONTROL_MAX_CLOCK_SKEW_MS) as u64,
);

/// Drop markers that can no longer gate anything. Best-effort and bounded: a
/// sweep failure never blocks a claim, and the walk stops after a fixed number
/// of entries so a pathological directory cannot stall a capture control.
///
/// `retain` is per-ledger because the two ledgers stop mattering at different
/// times: an applied-decision marker is dead once no command can still be
/// inside its own lifetime, a spent-act marker once no gesture can still be
/// fresh. Sweeping the acts on the decisions' 90-day clock would keep one file
/// per capture start for three months to gate a two-minute window.
fn prune_expired_claims(dir: &std::path::Path, retain: std::time::Duration) {
    const MAX_SWEPT: usize = 512;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(MAX_SWEPT) {
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified.elapsed().map(|age| age > retain).unwrap_or(false))
            .unwrap_or(false);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The path-safe half of a signed decision id.
///
/// The decision id is a `blake3:<64 lower hex>` digest, validated by the
/// contract long before this point. Only the hex ever reaches a path, and the
/// shape is re-checked here so a future contract change cannot turn a decision
/// id into a path component. All three durable ledgers — the applied-command
/// claim, the spent-act claim and the receipt row — key off this ONE function,
/// so none can drift into accepting a traversal the others refuse. The act
/// ledger's key is a digest this module computes rather than one the app
/// supplies, and it goes through here anyway: a key that skipped the check
/// because "we made it ourselves" is exactly how the next one gets in.
fn decision_hex(decision_id: &str) -> Option<&str> {
    let hex = decision_id.strip_prefix("blake3:")?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    Some(hex)
}

fn applied_decision_marker(workdirs_root: &std::path::Path, hex: &str) -> std::path::PathBuf {
    workdirs_root
        .join(APPLIED_DECISION_DIR)
        .join(format!("{hex}.json"))
}

/// Create-new-or-fail marker for one signed decision.
///
/// `create_new(true)` is the whole mechanism: on every platform this repo runs
/// on, it is an atomic "create if absent", so two concurrent applies of one
/// envelope cannot both see `Fresh`.
fn claim_decision(workdirs_root: &std::path::Path, decision_id: &str) -> DecisionClaim {
    let Some(hex) = decision_hex(decision_id) else {
        return DecisionClaim::Unavailable(
            "the signed decision id is not a recognizable lower-hex digest".to_owned(),
        );
    };
    let dir = workdirs_root.join(APPLIED_DECISION_DIR);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return DecisionClaim::Unavailable(format!(
            "the applied-command ledger directory is unavailable: {error}"
        ));
    }
    prune_expired_claims(&dir, APPLIED_DECISION_RETAIN);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(applied_decision_marker(workdirs_root, hex))
    {
        Ok(mut file) => {
            use std::io::Write;
            // Best-effort provenance inside the marker; its EXISTENCE is the
            // claim, so a failed body write does not release it. The durability
            // fence matters though: a crash between this write and the manager
            // call must not lose the claim and re-admit a replay, so the file is
            // synced before the caller is allowed to proceed.
            let _ = file.write_all(
                serde_json::to_vec(&json!({
                    "decision_id": decision_id,
                    "applied_at": Utc::now().to_rfc3339(),
                }))
                .unwrap_or_default()
                .as_slice(),
            );
            let _ = file.sync_all();
            DecisionClaim::Fresh
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            DecisionClaim::AlreadyApplied
        },
        Err(error) => DecisionClaim::Unavailable(format!(
            "the applied-command ledger could not be written: {error}"
        )),
    }
}

/// Directory (under the scope's capability workdirs root) holding one marker
/// per SPENT surface act.
///
/// A sibling of the applied-command claims rather than the same directory,
/// because it answers a different question. "Has this envelope been applied"
/// and "has this act already opened a microphone" come apart precisely when it
/// matters: the decision id folds in the sealed proposal, so one click sealed
/// into two proposals is two decisions, two markers, and — before this ledger
/// — two captures.
const SPENT_GESTURE_DIR: &str = "meeting_control_gestures";

/// The refusal a second start from one act earns.
///
/// Named rather than repeated because a test has to be able to ask for exactly
/// this refusal, and because it must never be confused with
/// [`ALREADY_APPLIED_REFUSAL`]: that one means the ENVELOPE is spent, this one
/// means the ACT is.
const GESTURE_ALREADY_SPENT_REFUSAL: &str =
    "the surface act behind this capture start has already been spent";

/// The path-safe key one spent act is recorded under.
///
/// Host-computed over the fields that identify the act inside this scope: the
/// installation the command came from, the scope binding it was authenticated
/// against, the bridge session the act happened in and the act itself. The
/// principal and workspace are already in `workdirs_root`, so they are not
/// repeated here — the ledger's location IS that half of the key.
///
/// Canonical bytes, not `format!`: two of these fields are app-supplied strings
/// and a separator-joined key would let one act's id impersonate another's by
/// carrying the separator.
fn spent_gesture_key(ticket: &AppMeetingStartIntentTicket) -> Option<String> {
    let bytes = canonical_json_bytes(&json!({
        "installation_id": ticket.installation_id,
        "scope_binding_ref": ticket.scope_binding_ref,
        "surface_session_id": ticket.surface_session_id,
        "gesture_id": ticket.gesture_id,
    }))
    .ok()?;
    Some(content_digest(&bytes))
}

/// Spend one act durably, or refuse the start.
///
/// The same atomic `create_new` the applied-command claim uses, for the same
/// reason: two concurrent starts naming one act cannot both create the marker,
/// so at most one of them reaches a manager. It fails CLOSED on an unavailable
/// ledger — an unrecorded act is an unbounded number of captures from a single
/// click, which is the outcome the whole ticket exists to prevent.
fn claim_spent_gesture(
    workdirs_root: &std::path::Path,
    ticket: &AppMeetingStartIntentTicket,
) -> Result<(), AppMeetingControlContributionError> {
    let key =
        spent_gesture_key(ticket).ok_or(AppMeetingControlContributionError::NotAdmissible(
            "the destination could not key this capture start's act",
        ))?;
    let hex = decision_hex(&key).ok_or(AppMeetingControlContributionError::NotAdmissible(
        "the destination could not key this capture start's act",
    ))?;
    let dir = workdirs_root.join(SPENT_GESTURE_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the destination could not record this capture start's act",
        ));
    }
    prune_expired_claims(&dir, SPENT_GESTURE_RETAIN);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(format!("{hex}.json")))
    {
        Ok(mut file) => {
            use std::io::Write;
            // The marker's EXISTENCE is the spend; the body is provenance for
            // an operator reading the ledger later. It deliberately carries no
            // gesture id or bridge session — the audit already has those, and
            // this file is keyed by a digest so that it need not.
            let _ = file.write_all(
                serde_json::to_vec(&json!({
                    "decision_id": ticket.decision_id,
                    "spent_at": Utc::now().to_rfc3339(),
                }))
                .unwrap_or_default()
                .as_slice(),
            );
            // Synced for the same reason the decision claim is: a crash between
            // here and the manager call must not release the act.
            let _ = file.sync_all();
            Ok(())
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(
            AppMeetingControlContributionError::NotAdmissible(GESTURE_ALREADY_SPENT_REFUSAL),
        ),
        Err(_) => Err(AppMeetingControlContributionError::NotAdmissible(
            "the destination could not record this capture start's act",
        )),
    }
}

/// Directory (under the scope's capability workdirs root) holding one
/// `control_receipt` row per signed decision.
const CONTROL_RECEIPT_DIR: &str = "meeting_control_receipts";

/// The scope's control-receipt ledger.
///
/// Deliberately a sibling of the applied-command markers rather than the same
/// directory: the markers are prunable once they can no longer gate anything,
/// and a receipt is the record — it outlives the gate that produced it.
pub fn control_receipt_dir(workdirs_root: &std::path::Path) -> std::path::PathBuf {
    workdirs_root.join(CONTROL_RECEIPT_DIR)
}

/// The closed outcome vocabulary of the package's `control_receipt` entity.
///
/// Value-for-value the entity's `outcome` enum. It is restated here rather than
/// derived from the manifest on purpose: the destination must be able to say
/// what happened without loading the package that will read it, and a projector
/// that quietly widened its vocabulary would write rows the declared receipts
/// view cannot render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppMeetingControlReceiptOutcome {
    Started,
    Paused,
    Resumed,
    Stopped,
    OwnerDeclined,
    Refused,
}

/// Why a refusal happened, as a closed code.
///
/// Deliberately NOT the destination's error text. That text names host paths,
/// registry state, and — on a wrong-scope refusal — the shape of another
/// principal's session; the capture-control audit keeps it for the operator.
/// What crosses back into the app's own data plane is a code its console can
/// branch on, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppMeetingControlReceiptErrorCode {
    InvalidContract,
    InvalidSignature,
    NotAdmissible,
    ApplyFailed,
}

/// One projected `control_receipt` row.
///
/// The field set is exactly the meetings package's `control_receipt` entity.
/// Every value is derived from the signed decision, a closed enum, or an
/// identifier the app itself proposed, so the row is bounded by construction
/// and carries no meeting content: a stop's `final_summary` goes back to the
/// authenticated caller and stops there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMeetingControlReceipt {
    pub receipt_id: String,
    pub decision_id: String,
    pub verb: CaptureControlVerb,
    pub outcome: AppMeetingControlReceiptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<AppMeetingControlReceiptErrorCode>,
    pub applied_at: String,
}

/// Project one destination result into the row the package's receipts view
/// reads.
///
/// Pure. Whether the row may be MINTED is a separate question, answered by
/// [`record_control_receipt`]; this function only says what the row would say.
fn project_control_receipt(
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    applied: &Result<AppMeetingControlApplication, AppMeetingControlContributionError>,
    applied_at: DateTime<Utc>,
) -> Option<AppMeetingControlReceipt> {
    let hex = decision_hex(&envelope.decision_id)?;
    let proposal = &envelope.review.proposal;
    let (outcome, session_id, thread_id, error_code) = match applied {
        Ok(AppMeetingControlApplication::Started {
            session_id,
            thread_id,
            ..
        }) => (
            AppMeetingControlReceiptOutcome::Started,
            Some(session_id.clone()),
            Some(thread_id.clone()),
            None,
        ),
        Ok(AppMeetingControlApplication::Paused { session_id }) => (
            AppMeetingControlReceiptOutcome::Paused,
            Some(session_id.clone()),
            None,
            None,
        ),
        Ok(AppMeetingControlApplication::Resumed { session_id }) => (
            AppMeetingControlReceiptOutcome::Resumed,
            Some(session_id.clone()),
            None,
            None,
        ),
        // `final_summary` is dropped here, not overlooked: it is what was said
        // in the room, and a durable app-readable row is not where that lives.
        Ok(AppMeetingControlApplication::Stopped { session_id, .. }) => (
            AppMeetingControlReceiptOutcome::Stopped,
            Some(session_id.clone()),
            None,
            None,
        ),
        Ok(AppMeetingControlApplication::OwnerDeclined) => (
            AppMeetingControlReceiptOutcome::OwnerDeclined,
            None,
            None,
            None,
        ),
        Err(error) => (
            AppMeetingControlReceiptOutcome::Refused,
            // The app's OWN target echoed back — never a session id the
            // destination discovered, which may name another scope's capture.
            proposal.target.session_id().map(str::to_owned),
            None,
            Some(receipt_error_code(error)),
        ),
    };
    Some(AppMeetingControlReceipt {
        // Keyed by the decision, so the receipt's identity IS the signed
        // result's identity and two rows cannot describe one decision.
        receipt_id: format!("receipt:{hex}"),
        decision_id: envelope.decision_id.clone(),
        verb: audit_verb(proposal.verb),
        outcome,
        session_id,
        thread_id,
        error_code,
        applied_at: applied_at.to_rfc3339(),
    })
}

fn receipt_error_code(
    error: &AppMeetingControlContributionError,
) -> AppMeetingControlReceiptErrorCode {
    match error {
        AppMeetingControlContributionError::InvalidContract(_) => {
            AppMeetingControlReceiptErrorCode::InvalidContract
        },
        AppMeetingControlContributionError::InvalidSignature(_) => {
            AppMeetingControlReceiptErrorCode::InvalidSignature
        },
        AppMeetingControlContributionError::NotAdmissible(_) => {
            AppMeetingControlReceiptErrorCode::NotAdmissible
        },
        AppMeetingControlContributionError::Apply(_) => {
            AppMeetingControlReceiptErrorCode::ApplyFailed
        },
    }
}

/// Mint the one receipt this signed decision earns, or mint nothing.
///
/// Three gates, each closing a different way of writing a row that lies:
///
/// * **Unproven.** Only a command that got past the paired desktop's signature
///   AND its binding to the current authority names a decision the owner really
///   signed — see [`AppMeetingControlRefusal`].
/// * **Replay.** An envelope that was already applied mints nothing: its row
///   exists, and a second one would either duplicate it or — if the first was
///   lost to a crash between the manager call and this line — record an
///   accepted capture as refused.
/// * **Not yet final.** The destination claims the decision LAST on purpose, so
///   a stale gesture or an already-live capture refuses without burning the
///   owner's signature. Minting a `refused` row for one of those would freeze
///   the ledger on a rehearsal: the retry that actually starts the capture would
///   find the row taken, and the console would read "refused" for a meeting
///   being recorded right now. A fate is final only for the call that SPENT the
///   envelope — its own claim created the applied-command marker, so it can
///   never apply again — or for an owner decline, which no retry can change.
///
///   `spend` is carried in from that claim rather than re-read from disk, and
///   the difference is the whole gate. Two submissions of one envelope can
///   overlap: the first sits inside `join_meeting_with_scope` for seconds while
///   the second, byte-identical, is refused on a rule that turns with the clock
///   alone — a gesture gone stale, a decision no longer recent, an expiry
///   crossed. A loser that read the winner's marker would mint `refused` first,
///   and `create_new` would then leave that row standing while the microphone
///   is live and the true `started` row can never be written.
///
///   A spent-act refusal ([`GESTURE_ALREADY_SPENT_REFUSAL`]) reads as final —
///   no retry can un-spend a click — and still mints nothing, for exactly this
///   reason: the call that spent the act may be the winner of a race over this
///   very envelope, sitting inside a manager right now. The rule stays "the
///   call that claimed the DECISION", because that is the only claim whose
///   holder is the one that reached a rail.
///
/// Best-effort in the same sense the audit is: a ledger failure is warned about,
/// never returned. Refusing to complete a capture control because a receipt
/// could not be written would invert the risk this record exists to manage.
fn record_control_receipt(
    workdirs_root: &std::path::Path,
    envelope: &AppMeetingControlOwnerDecisionEnvelopeV1,
    applied: &Result<AppMeetingControlApplication, AppMeetingControlContributionError>,
    signed_authority_proven: bool,
    spend: DecisionSpend,
) {
    if !signed_authority_proven {
        return;
    }
    // A replay carries no spend either, so this is the second lock on one door.
    // It stays because THIS refusal is the one whose row would lie outright:
    // the application that claimed the decision may have died between its
    // manager call and its own mint, and nothing can re-mint that row.
    if matches!(
        applied,
        Err(AppMeetingControlContributionError::NotAdmissible(rule))
            if *rule == ALREADY_APPLIED_REFUSAL
    ) {
        return;
    }
    let fate_is_final =
        matches!(applied, Ok(AppMeetingControlApplication::OwnerDeclined)) || spend.spent_here();
    if !fate_is_final {
        return;
    }
    let Some(receipt) = project_control_receipt(envelope, applied, Utc::now()) else {
        return;
    };
    mint_control_receipt(workdirs_root, &receipt);
}

/// Write the row exactly once, or leave the one that is already there.
///
/// `create_new(true)` is the same atomic "create if absent" the applied-command
/// claim uses, so two applications of one envelope — or two processes racing it
/// — cannot both mint. Returns whether THIS call created the row. A write that
/// fails part-way removes its own file rather than leaving an unparseable row
/// under a decision id nothing can ever re-mint, and no failure is retried under
/// a different key: a receipt not keyed by its own decision would break the one
/// property this ledger has.
fn mint_control_receipt(
    workdirs_root: &std::path::Path,
    receipt: &AppMeetingControlReceipt,
) -> bool {
    let Some(hex) = decision_hex(&receipt.decision_id) else {
        return false;
    };
    let dir = control_receipt_dir(workdirs_root);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!(
            target: "meet_bot",
            %error,
            dir = %dir.display(),
            "meeting control receipt ledger is unavailable; the receipt is not recorded"
        );
        return false;
    }
    let body = match serde_json::to_vec(receipt) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(
                target: "meet_bot",
                %error,
                "meeting control receipt could not be serialized"
            );
            return false;
        },
    };
    let path = dir.join(format!("{hex}.json"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            let written = match file.write_all(&body) {
                // Synced before the caller moves on for the same reason the
                // applied-command claim is: a crash must not leave a decision
                // recorded as spent with no row saying what it did.
                Ok(()) => file.sync_all(),
                Err(error) => Err(error),
            };
            drop(file);
            if let Err(error) = written {
                let _ = std::fs::remove_file(&path);
                warn!(
                    target: "meet_bot",
                    %error,
                    path = %path.display(),
                    "meeting control receipt could not be written; the row is not recorded"
                );
                return false;
            }
            true
        },
        // Already minted. One signed decision, one row.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            warn!(
                target: "meet_bot",
                %error,
                path = %path.display(),
                "meeting control receipt could not be added to the ledger"
            );
            false
        },
    }
}

/// Read the receipt one signed decision produced, if the ledger holds it.
///
/// The read half. A surface that shows a `control_receipt` reads it from here
/// rather than re-deriving a row from the destination's HTTP response, so what
/// the console shows is what the destination durably recorded — and an absent
/// row stays absent instead of being reconstructed as one.
pub fn read_control_receipt(
    workdirs_root: &std::path::Path,
    decision_id: &str,
) -> Option<AppMeetingControlReceipt> {
    let hex = decision_hex(decision_id)?;
    let body =
        std::fs::read(control_receipt_dir(workdirs_root).join(format!("{hex}.json"))).ok()?;
    serde_json::from_slice(&body).ok()
}

/// The capability workdirs root every durable ledger this seam writes lives
/// under — the capture-control audit, the applied-command claims and the
/// receipt rows.
///
/// One function so a reader can never derive a different root than the writer.
/// [`scope_control_receipt`] resolves the scope from the authenticated value
/// exactly as [`apply_signed_app_meeting_control`] did when it minted, and a
/// pin below holds the two together.
fn scope_workdirs_root(
    artifact_workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> std::path::PathBuf {
    crate::magician_v2::subprocess_owners::workdirs_root(artifact_workspace, principal, workspace)
}

/// The package entity a minted receipt is published into.
///
/// Restated here for the same reason the outcome vocabulary above is: this
/// module owns what a `control_receipt` says, so it owns where the row goes.
/// The name is the meetings package's declared entity.
pub const CONTROL_RECEIPT_ENTITY: &str = "control_receipt";

impl AppMeetingControlReceipt {
    /// The app-store record id this row occupies.
    ///
    /// Deliberately NOT `receipt_id`: a store record id is an opaque ASCII
    /// token and a colon is not one of its characters, so the entity's own
    /// `receipt:<hex>` key cannot double as the row's id. Both are keyed by the
    /// same decision hex, so there is still exactly one addressable row per
    /// signed decision — which is what makes a second publication of one
    /// decision a refused collision instead of a duplicate row.
    pub fn package_record_id(&self) -> Option<String> {
        decision_hex(&self.decision_id).map(|hex| format!("receipt-{hex}"))
    }
}

/// The receipt one signed decision earned, in the scope that applied it.
///
/// This is the seam the package's `control_receipt` entity is published from.
/// The scope comes from the authenticated value, never from the envelope: a
/// signed command names an installation, and an installation is not a claim
/// about whose ledger to read.
///
/// The ledger's other key is the decision id, and that is what binds a row to
/// ONE installation even though this root is scope-wide: a decision id digests
/// the sealed proposal, the sealed proposal digests the header, and the header
/// names an installation the route has already checked against its own path.
/// A caller cannot name a sibling installation's decision without failing the
/// contract's own `decision_id` check first — see the pin in this module's
/// tests, which is where that three-layer property is asserted rather than
/// assumed.
///
/// `None` means the destination minted nothing — the fate was not final, the
/// owner's signature was never proven, or the write failed — and an absent row
/// must stay absent rather than be reconstructed from an HTTP result.
pub fn scope_control_receipt(
    artifact_workspace: &ArtifactV2Workspace,
    authenticated: &AuthenticatedAppScope,
    decision_id: &str,
) -> Option<AppMeetingControlReceipt> {
    let scope = authenticated.scope();
    read_control_receipt(
        &scope_workdirs_root(
            artifact_workspace,
            scope.principal.as_str(),
            scope.workspace.as_str(),
        ),
        decision_id,
    )
}

fn live_session_target(
    proposal: &AppMeetingControlProposalV1,
) -> Result<&str, AppMeetingControlContributionError> {
    proposal
        .target
        .session_id()
        .ok_or(AppMeetingControlContributionError::NotAdmissible(
            "a pause, resume or stop requires a live-session target",
        ))
}

fn start_title(target: &AppMeetingControlTargetV1) -> Option<&str> {
    match target {
        AppMeetingControlTargetV1::NewCapture { title, .. } => title.as_deref(),
        AppMeetingControlTargetV1::LiveSession { .. } => None,
    }
}

fn application_thread_id(application: &AppMeetingControlApplication) -> Option<&str> {
    match application {
        AppMeetingControlApplication::Started { thread_id, .. } => Some(thread_id),
        _ => None,
    }
}

fn application_session_id(application: &AppMeetingControlApplication) -> Option<&str> {
    match application {
        AppMeetingControlApplication::Started { session_id, .. }
        | AppMeetingControlApplication::Paused { session_id }
        | AppMeetingControlApplication::Resumed { session_id }
        | AppMeetingControlApplication::Stopped { session_id, .. } => Some(session_id),
        AppMeetingControlApplication::OwnerDeclined => None,
    }
}

fn audit_verb(verb: AppMeetingControlVerbV1) -> CaptureControlVerb {
    match verb {
        AppMeetingControlVerbV1::Listen => CaptureControlVerb::Listen,
        AppMeetingControlVerbV1::Join => CaptureControlVerb::Join,
        AppMeetingControlVerbV1::Pause => CaptureControlVerb::Pause,
        AppMeetingControlVerbV1::Resume => CaptureControlVerb::Resume,
        AppMeetingControlVerbV1::Stop => CaptureControlVerb::Stop,
    }
}

/// Prove that the live source head says exactly what the sealed command says.
///
/// A digest match alone proves only that some current `control_request` row was
/// named. This closed comparison prevents a signer or host integration bug from
/// attaching a valid digest for a STOP to a command for a JOIN.
fn validate_control_request_source(
    proposal: &AppMeetingControlProposalV1,
    source: &Value,
) -> Result<(), AppMeetingControlContributionError> {
    const SOURCE_FIELDS: [&str; 10] = [
        "request_id",
        "verb",
        "target_kind",
        "target_ref",
        "gesture_id",
        "actor_ref",
        "note",
        "payload_json",
        "apply_state",
        "requested_at",
    ];
    let object = source
        .as_object()
        .ok_or(AppMeetingControlContributionError::NotAdmissible(
            "the control_request source payload is not an object",
        ))?;
    if object.len() != SOURCE_FIELDS.len()
        || SOURCE_FIELDS
            .iter()
            .any(|field| !object.contains_key(*field))
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request source payload does not have the closed package shape",
        ));
    }
    let source_text = |field: &'static str| {
        object.get(field).and_then(Value::as_str).ok_or(
            AppMeetingControlContributionError::NotAdmissible(
                "the control_request source payload has an invalid text field",
            ),
        )
    };

    let (target_kind, target_ref, expected_payload) = match &proposal.target {
        AppMeetingControlTargetV1::NewCapture {
            url,
            title,
            date,
            capture_mic,
        } => {
            let gesture = proposal.gesture.as_ref().ok_or(
                AppMeetingControlContributionError::NotAdmissible(
                    "a capture start source must retain its reviewed gesture",
                ),
            )?;
            (
                "new_capture",
                url.clone(),
                json!({
                    "request_id": proposal.header.dedupe_key,
                    "verb": proposal.verb.as_str(),
                    "url": url,
                    "title": title,
                    "date": date,
                    "capture_mic": capture_mic,
                    "gesture_id": gesture.gesture_id,
                    "surface_session_id": gesture.surface_session_id,
                    "gesture_observed_at_ms": gesture.observed_at_ms,
                    "gesture_expires_at_ms": gesture.expires_at_ms,
                }),
            )
        },
        AppMeetingControlTargetV1::LiveSession { session_id } => (
            "live_session",
            Some(session_id.clone()),
            json!({
                "request_id": proposal.header.dedupe_key,
                "verb": proposal.verb.as_str(),
                "session_id": session_id,
            }),
        ),
    };

    if source_text("request_id")? != proposal.header.dedupe_key
        || source_text("verb")? != proposal.verb.as_str()
        || source_text("target_kind")? != target_kind
        || source_text("apply_state")? != "recorded"
        // The host stamps the actor at admission; a package that pre-filled it
        // would be asserting an identity it does not own.
        || object.get("actor_ref") != Some(&Value::Null)
        || source_text("requested_at")?.is_empty()
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request identity, verb, target kind, or immutable state does not match the signed command",
        ));
    }
    let source_target_ref = object.get("target_ref").and_then(|value| match value {
        Value::Null => Some(None),
        Value::String(text) => Some(Some(text.clone())),
        _ => None,
    });
    if source_target_ref != Some(target_ref) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request target does not match the signed command",
        ));
    }
    let source_gesture = object.get("gesture_id").and_then(|value| match value {
        Value::Null => Some(None),
        Value::String(text) => Some(Some(text.as_str())),
        _ => None,
    });
    if source_gesture
        != Some(
            proposal
                .gesture
                .as_ref()
                .map(|gesture| gesture.gesture_id.as_str()),
        )
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request gesture does not match the signed command",
        ));
    }
    let source_note = object.get("note").and_then(|value| match value {
        Value::Null => Some(None),
        Value::String(text) => Some(Some(text.as_str())),
        _ => None,
    });
    if source_note != Some(proposal.note.as_deref()) {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request note does not match the signed command",
        ));
    }

    let encoded_payload = source_text("payload_json")?;
    let parsed_payload: Value = serde_json::from_str(encoded_payload).map_err(|_| {
        AppMeetingControlContributionError::NotAdmissible(
            "the control_request payload_json is not valid JSON",
        )
    })?;
    let canonical_payload = canonical_json_bytes(&parsed_payload).map_err(|_| {
        AppMeetingControlContributionError::NotAdmissible(
            "the control_request payload_json cannot be canonicalized",
        )
    })?;
    if parsed_payload != expected_payload
        || canonical_payload.as_slice() != encoded_payload.as_bytes()
    {
        return Err(AppMeetingControlContributionError::NotAdmissible(
            "the control_request payload_json is not the canonical exact action input",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_app_contract::contribution::{
        content_digest, AppMeetingControlGestureV1, AppMeetingControlOwnerReviewV1,
        APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
    };

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn header(verb: AppMeetingControlVerbV1) -> AppMeetingControlSourceHeaderV1 {
        AppMeetingControlSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: APP_MEETING_CONTROL_CONTRACT_ID.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("destination"),
            proposal_id: "proposal:control-1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope:owner".to_owned(),
            installation_id: "installation:meetings".to_owned(),
            installation_generation: 1,
            package_revision_ref: "package:meetings".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 1,
            grant_authority_digest: digest("grant"),
            schema_revision: 1,
            schema_digest: digest("schema"),
            source_entity_name: "control_request".to_owned(),
            source_record_id: "record:control-1".to_owned(),
            source_record_revision: 1,
            source_record_digest: digest("source"),
            workflow_id: verb.as_str().to_owned(),
            workflow_digest: digest("workflow"),
            action_id: verb.as_str().to_owned(),
            action_digest: digest("action"),
            // A START's signed lifetime is capped at the gesture ceiling, so the
            // fixture must live inside it too — the earlier ten-minute window
            // is now inadmissible by construction.
            issued_at_ms: 1_000,
            expires_at_ms: 1_000 + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
            dedupe_key: "request-1".to_owned(),
        }
    }

    fn gesture() -> AppMeetingControlGestureV1 {
        // Inside the header window above, which is itself gesture-ceiling wide.
        AppMeetingControlGestureV1 {
            surface_session_id: "surface-1".to_owned(),
            gesture_id: "gesture-1".to_owned(),
            observed_at_ms: 1_000,
            expires_at_ms: 1_000 + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
        }
    }

    fn listen_proposal() -> AppMeetingControlProposalV1 {
        AppMeetingControlProposalV1 {
            header: header(AppMeetingControlVerbV1::Listen),
            verb: AppMeetingControlVerbV1::Listen,
            target: AppMeetingControlTargetV1::NewCapture {
                url: Some("https://meet.example.com/abc".to_owned()),
                title: Some("Acme review".to_owned()),
                date: Some("2026-09-02".to_owned()),
                capture_mic: true,
            },
            gesture: Some(gesture()),
            by: "owner".to_owned(),
            note: None,
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed listen proposal")
    }

    fn stop_proposal() -> AppMeetingControlProposalV1 {
        AppMeetingControlProposalV1 {
            header: header(AppMeetingControlVerbV1::Stop),
            verb: AppMeetingControlVerbV1::Stop,
            target: AppMeetingControlTargetV1::LiveSession {
                session_id: "listen-abc".to_owned(),
            },
            gesture: None,
            by: "owner".to_owned(),
            note: None,
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed stop proposal")
    }

    fn canonical(value: &Value) -> String {
        String::from_utf8(canonical_json_bytes(value).expect("canonical")).expect("utf8")
    }

    fn listen_source(proposal: &AppMeetingControlProposalV1) -> Value {
        let gesture = proposal.gesture.as_ref().expect("gesture");
        let AppMeetingControlTargetV1::NewCapture {
            url,
            title,
            date,
            capture_mic,
        } = &proposal.target
        else {
            unreachable!("listen proposals carry a new-capture target")
        };
        let payload = json!({
            "request_id": proposal.header.dedupe_key,
            "verb": proposal.verb.as_str(),
            "url": url,
            "title": title,
            "date": date,
            "capture_mic": capture_mic,
            "gesture_id": gesture.gesture_id,
            "surface_session_id": gesture.surface_session_id,
            "gesture_observed_at_ms": gesture.observed_at_ms,
            "gesture_expires_at_ms": gesture.expires_at_ms,
        });
        json!({
            "request_id": proposal.header.dedupe_key,
            "verb": proposal.verb.as_str(),
            "target_kind": "new_capture",
            "target_ref": url,
            "gesture_id": gesture.gesture_id,
            "actor_ref": Value::Null,
            "note": Value::Null,
            "payload_json": canonical(&payload),
            "apply_state": "recorded",
            "requested_at": "2026-09-02T09:00:00Z",
        })
    }

    #[test]
    fn a_matching_control_request_source_admits() {
        let proposal = listen_proposal();
        validate_control_request_source(&proposal, &listen_source(&proposal))
            .expect("the exact source head admits");
    }

    #[test]
    fn a_source_for_another_command_is_refused() {
        let proposal = listen_proposal();
        let mut source = listen_source(&proposal);
        source["verb"] = json!("join");
        assert!(validate_control_request_source(&proposal, &source).is_err());

        let mut source = listen_source(&proposal);
        source["target_ref"] = json!("https://meet.example.com/other");
        assert!(validate_control_request_source(&proposal, &source).is_err());

        let mut source = listen_source(&proposal);
        source["gesture_id"] = json!("gesture-2");
        assert!(
            validate_control_request_source(&proposal, &source).is_err(),
            "a start must not borrow another act's gesture"
        );
    }

    #[test]
    fn a_host_stamped_actor_or_open_state_in_the_source_is_refused() {
        let proposal = listen_proposal();
        let mut source = listen_source(&proposal);
        source["actor_ref"] = json!("owner");
        assert!(validate_control_request_source(&proposal, &source).is_err());

        let mut source = listen_source(&proposal);
        source["apply_state"] = json!("applied");
        assert!(validate_control_request_source(&proposal, &source).is_err());
    }

    #[test]
    fn a_non_canonical_payload_is_refused() {
        let proposal = listen_proposal();
        let mut source = listen_source(&proposal);
        // Same JSON value, different byte order: the canonical comparison is
        // what makes the digest meaningful.
        let reordered = format!(
            "{{\"verb\":\"listen\",{}",
            &source["payload_json"].as_str().expect("payload")[1..]
        );
        source["payload_json"] = json!(reordered);
        assert!(validate_control_request_source(&proposal, &source).is_err());
    }

    #[test]
    fn an_extra_or_missing_source_field_is_refused() {
        let proposal = listen_proposal();
        let mut source = listen_source(&proposal);
        source["extra"] = json!("field");
        assert!(validate_control_request_source(&proposal, &source).is_err());

        let mut source = listen_source(&proposal);
        source.as_object_mut().expect("object").remove("note");
        assert!(validate_control_request_source(&proposal, &source).is_err());
    }

    #[test]
    fn the_contract_refuses_a_gesture_on_a_stop_and_demands_one_on_a_start() {
        let mut start = listen_proposal();
        start.gesture = None;
        assert!(
            start.validate().is_err(),
            "a start without intent is invalid"
        );

        let mut stop = stop_proposal();
        stop.gesture = Some(gesture());
        assert!(
            stop.validate().is_err(),
            "a stop must not carry a start's intent proof"
        );
    }

    #[test]
    fn the_contract_pins_each_verb_to_its_own_target_class() {
        let mut start = listen_proposal();
        start.target = AppMeetingControlTargetV1::LiveSession {
            session_id: "listen-abc".to_owned(),
        };
        assert!(start.validate().is_err());

        let mut stop = stop_proposal();
        stop.target = AppMeetingControlTargetV1::NewCapture {
            url: Some("https://meet.example.com/abc".to_owned()),
            title: None,
            date: None,
            capture_mic: false,
        };
        assert!(stop.validate().is_err());
    }

    #[test]
    fn a_start_cannot_be_sealed_with_a_window_that_opens_later() {
        // The width cap alone left the position free: a proposal could carry a
        // two-minute window opening a month out, be signed today against a
        // plausible display, and apply once, a month later. Pinning the gesture
        // inside the header AND capping the header is what closes it.
        let mut wide = listen_proposal();
        wide.header.expires_at_ms =
            wide.header.issued_at_ms + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS + 1;
        assert!(
            wide.clone().seal().is_err(),
            "a START's signed lifetime may not exceed the gesture ceiling"
        );

        let mut detached = listen_proposal();
        detached.gesture = Some(AppMeetingControlGestureV1 {
            surface_session_id: "surface-1".to_owned(),
            gesture_id: "gesture-1".to_owned(),
            observed_at_ms: detached.header.expires_at_ms + 1,
            expires_at_ms: detached.header.expires_at_ms
                + 1
                + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
        });
        assert!(
            detached.seal().is_err(),
            "a START's gesture may not sit outside its own proposal lifetime"
        );
    }

    #[test]
    fn the_decision_id_discriminates_two_identical_controls() {
        // pause → resume → pause must be possible. Without the sealed proposal
        // in the decision id, the three gesture-less verbs have NO
        // discriminator, so a destination consuming decision ids exactly-once
        // would refuse every repeat control on a session forever.
        let first = stop_proposal();
        let mut second_header = header(AppMeetingControlVerbV1::Stop);
        second_header.proposal_id = "proposal:control-2".to_owned();
        second_header.dedupe_key = "request-2".to_owned();
        let second = AppMeetingControlProposalV1 {
            header: second_header,
            ..first.clone()
        }
        .seal()
        .expect("second sealed stop proposal");
        assert_ne!(first.proposal_digest, second.proposal_digest);

        let envelope_of = |proposal: AppMeetingControlProposalV1| {
            AppMeetingControlOwnerDecisionEnvelopeV1::prepare(
                AppMeetingControlOwnerReviewV1::mint(
                    1,
                    "key-1".to_owned(),
                    digest("identity"),
                    proposal,
                )
                .expect("minted review"),
                AppMeetingControlOwnerDecisionV1::Accept,
                50_000,
            )
            .expect("prepared envelope")
        };
        assert_ne!(
            envelope_of(first).decision_id,
            envelope_of(second).decision_id,
            "two separately sealed controls are two decisions, not one replay"
        );
    }

    #[test]
    fn a_signature_that_is_not_recent_is_refused() {
        let envelope = AppMeetingControlOwnerDecisionEnvelopeV1::prepare(
            AppMeetingControlOwnerReviewV1::mint(
                1,
                "key-1".to_owned(),
                digest("identity"),
                listen_proposal(),
            )
            .expect("minted review"),
            AppMeetingControlOwnerDecisionV1::Accept,
            50_000,
        )
        .expect("prepared envelope");
        assert!(envelope.is_recently_decided_at(50_000));
        assert!(envelope.is_recently_decided_at(
            50_000 + magician_app_contract::contribution::APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS
        ));
        assert!(
            !envelope.is_recently_decided_at(
                50_000
                    + magician_app_contract::contribution::APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS
                    + 1
            ),
            "a month-old signature is not a fresh capture command"
        );
        assert!(
            !envelope.is_recently_decided_at(1),
            "a signature claiming the future beyond the skew tolerance is refused"
        );
    }

    #[test]
    fn a_stale_gesture_is_not_fresh() {
        let gesture = gesture();
        assert!(gesture.is_fresh_at(gesture.observed_at_ms));
        assert!(gesture.is_fresh_at(gesture.expires_at_ms - 1));
        assert!(!gesture.is_fresh_at(gesture.expires_at_ms));
        assert!(!gesture.is_fresh_at(gesture.observed_at_ms - 1));
    }

    fn envelope(
        proposal: AppMeetingControlProposalV1,
        decision: AppMeetingControlOwnerDecisionV1,
    ) -> AppMeetingControlOwnerDecisionEnvelopeV1 {
        AppMeetingControlOwnerDecisionEnvelopeV1::prepare(
            AppMeetingControlOwnerReviewV1::mint(
                1,
                "key-1".to_owned(),
                digest("identity"),
                proposal,
            )
            .expect("minted review"),
            decision,
            50_000,
        )
        .expect("prepared envelope")
    }

    fn started() -> Result<AppMeetingControlApplication, AppMeetingControlContributionError> {
        Ok(AppMeetingControlApplication::Started {
            session_id: "listen-abc".to_owned(),
            thread_id: "meeting-acme-review-2026-09-02".to_owned(),
            mode: "passive",
        })
    }

    /// Spend the envelope the way `dispatch` does, and hand back the proof the
    /// spending call carries, so the fate under test is final for the same
    /// reason production's is.
    fn spend(workdirs_root: &std::path::Path, decision_id: &str) -> DecisionSpend {
        let mut spend = DecisionSpend::unspent();
        consume_decision(workdirs_root, decision_id, &mut spend).expect("a fresh claim");
        assert!(spend.spent_here());
        spend
    }

    #[test]
    fn a_signed_result_projects_exactly_the_control_receipt_entity_row() {
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let receipt = project_control_receipt(&envelope, &started(), Utc::now())
            .expect("a signed result projects a receipt");
        assert_eq!(receipt.decision_id, envelope.decision_id);
        assert!(
            receipt.receipt_id.ends_with(
                envelope
                    .decision_id
                    .strip_prefix("blake3:")
                    .expect("digest")
            ),
            "the receipt's identity is the signed decision's identity"
        );
        assert_eq!(receipt.verb, CaptureControlVerb::Listen);
        assert_eq!(receipt.outcome, AppMeetingControlReceiptOutcome::Started);
        assert_eq!(receipt.session_id.as_deref(), Some("listen-abc"));
        assert_eq!(
            receipt.thread_id.as_deref(),
            Some("meeting-acme-review-2026-09-02")
        );
        assert!(receipt.error_code.is_none());

        // The package's `control_receipt` entity, field for field. A projector
        // that grew or renamed one would write rows the declared receipts view
        // cannot render, and nothing else in this repo would notice.
        let row = serde_json::to_value(&receipt).expect("row");
        let mut fields = row
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        fields.sort();
        assert_eq!(
            fields,
            vec![
                "applied_at",
                "decision_id",
                "outcome",
                "receipt_id",
                "session_id",
                "thread_id",
                "verb",
            ]
        );
    }

    #[test]
    fn a_refusal_receipt_carries_a_closed_code_and_no_host_detail() {
        let envelope = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let refused = Err(AppMeetingControlContributionError::Apply(
            "the applied-command ledger could not be written: /Users/owner/workdirs".to_owned(),
        ));
        let receipt = project_control_receipt(&envelope, &refused, Utc::now()).expect("receipt");
        assert_eq!(receipt.outcome, AppMeetingControlReceiptOutcome::Refused);
        assert_eq!(
            receipt.error_code,
            Some(AppMeetingControlReceiptErrorCode::ApplyFailed)
        );
        // The app proposed this session id itself; echoing it back discloses
        // nothing it did not already name.
        assert_eq!(receipt.session_id.as_deref(), Some("listen-abc"));
        assert!(
            !serde_json::to_string(&receipt)
                .expect("row")
                .contains("/Users/owner"),
            "the destination's free text stays in the operator audit"
        );
    }

    #[test]
    fn a_stopped_receipt_never_carries_the_final_summary() {
        let envelope = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let stopped = Ok(AppMeetingControlApplication::Stopped {
            session_id: "listen-abc".to_owned(),
            final_summary: Some("we agreed to ship on friday".to_owned()),
        });
        let receipt = project_control_receipt(&envelope, &stopped, Utc::now()).expect("receipt");
        assert_eq!(receipt.outcome, AppMeetingControlReceiptOutcome::Stopped);
        assert!(
            !serde_json::to_string(&receipt)
                .expect("row")
                .contains("ship on friday"),
            "a durable app-readable row is not where a meeting's words live"
        );
    }

    #[test]
    fn an_unproven_command_mints_no_receipt() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        // Spent, even — the decision id is a digest over content the app
        // supplies, so an unverified caller can name a real decision's id.
        let spent = spend(temp.path(), &envelope.decision_id);
        record_control_receipt(
            temp.path(),
            &envelope,
            &Err(AppMeetingControlContributionError::InvalidSignature(
                "the owner did not sign this".to_owned(),
            )),
            false,
            spent,
        );
        assert!(
            read_control_receipt(temp.path(), &envelope.decision_id).is_none(),
            "an app must not be able to mint its own evidence that the owner saw a request"
        );
    }

    /// The ledger key is (scope, decision id), and the row is read back by the
    /// route to publish into ONE installation's entity store. Nothing in this
    /// module makes those the same installation; the contract does, three
    /// layers away: a decision id digests the sealed proposal, the sealed
    /// proposal digests the header, and the header names the installation the
    /// route checks against its own path before anything reads this ledger.
    ///
    /// Pinned because every one of those links is justified in the contract for
    /// a DIFFERENT reason — `proposal_digest` is in the decision id so
    /// pause/resume cycling is possible at all — so narrowing any of them would
    /// look local and would silently make one installation's receipt readable
    /// under a sibling's decision id, with the row's `session_id` in it.
    #[test]
    fn a_decision_id_is_bound_to_the_installation_its_command_named() {
        let mine = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let mut sibling = listen_proposal();
        sibling.header.installation_id = "installation:other".to_owned();
        let sibling = envelope(
            sibling.seal().expect("sealed sibling proposal"),
            AppMeetingControlOwnerDecisionV1::Accept,
        );
        assert_ne!(
            mine.decision_id, sibling.decision_id,
            "two installations must never name one row in the scope-wide ledger"
        );
    }

    #[test]
    fn a_refusal_that_never_spent_the_envelope_mints_no_receipt() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        record_control_receipt(
            temp.path(),
            &envelope,
            &Err(AppMeetingControlContributionError::Apply(
                "a capture session is already live; stop it before starting another".to_owned(),
            )),
            true,
            DecisionSpend::unspent(),
        );
        assert!(
            read_control_receipt(temp.path(), &envelope.decision_id).is_none(),
            "a retryable refusal must not take the row the retry's real fate needs"
        );

        // The retry claims the decision and starts the capture. THAT is the
        // fate, and the console must read it rather than the rehearsal.
        let spent = spend(temp.path(), &envelope.decision_id);
        record_control_receipt(temp.path(), &envelope, &started(), true, spent);
        assert_eq!(
            read_control_receipt(temp.path(), &envelope.decision_id)
                .expect("receipt")
                .outcome,
            AppMeetingControlReceiptOutcome::Started
        );
    }

    #[test]
    fn a_losing_concurrent_submission_never_takes_the_row_the_winner_owes() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);

        // The application that will really start the capture claims the
        // decision and is still blocked inside the listener rail.
        let spent = spend(temp.path(), &envelope.decision_id);

        // The same envelope, submitted again inside that window and refused on
        // a rule that turns with the clock alone. Its marker probe would say
        // "spent" — by the other call — and that must not read as its own fate.
        record_control_receipt(
            temp.path(),
            &envelope,
            &Err(AppMeetingControlContributionError::NotAdmissible(
                "the surface gesture behind this capture start is not fresh",
            )),
            true,
            DecisionSpend::unspent(),
        );
        assert!(
            read_control_receipt(temp.path(), &envelope.decision_id).is_none(),
            "a call that claimed nothing must not mint the row"
        );

        // The winner returns and records what actually happened to the mic.
        record_control_receipt(temp.path(), &envelope, &started(), true, spent);
        assert_eq!(
            read_control_receipt(temp.path(), &envelope.decision_id)
                .expect("receipt")
                .outcome,
            AppMeetingControlReceiptOutcome::Started,
            "the ledger says the capture started, not that it was refused"
        );
    }

    #[test]
    fn only_the_call_that_claims_the_decision_carries_the_spend() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let first = spend(temp.path(), &envelope.decision_id);
        assert!(first.spent_here());

        let mut second = DecisionSpend::unspent();
        let refusal = consume_decision(temp.path(), &envelope.decision_id, &mut second)
            .expect_err("a second claim on one decision is refused");
        assert!(matches!(
            refusal,
            AppMeetingControlContributionError::NotAdmissible(rule)
                if rule == ALREADY_APPLIED_REFUSAL
        ));
        assert!(
            !second.spent_here(),
            "reading another call's marker is not spending the envelope"
        );

        // A ledger that cannot record a claim refuses AND carries no spend: an
        // unrecorded claim is not a spent envelope.
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write blocker");
        let mut unavailable = DecisionSpend::unspent();
        assert!(consume_decision(&blocked, &envelope.decision_id, &mut unavailable).is_err());
        assert!(!unavailable.spent_here());
    }

    #[test]
    fn a_failure_after_the_claim_still_mints_its_refusal() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        // The envelope is burnt — no retry can reach a manager with it — so the
        // apply failure IS this decision's fate and the console must see it.
        let spent = spend(temp.path(), &envelope.decision_id);
        record_control_receipt(
            temp.path(),
            &envelope,
            &Err(AppMeetingControlContributionError::Apply(
                "the passive listener could not start".to_owned(),
            )),
            true,
            spent,
        );
        let receipt = read_control_receipt(temp.path(), &envelope.decision_id).expect("receipt");
        assert_eq!(receipt.outcome, AppMeetingControlReceiptOutcome::Refused);
        assert_eq!(
            receipt.error_code,
            Some(AppMeetingControlReceiptErrorCode::ApplyFailed)
        );
    }

    #[test]
    fn a_replayed_signed_result_does_not_mint_a_second_receipt() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let spent = spend(temp.path(), &envelope.decision_id);
        record_control_receipt(temp.path(), &envelope, &started(), true, spent);

        // The replay: the envelope is spent, so the destination refuses it —
        // and the refusing call, having claimed nothing, carries no spend.
        record_control_receipt(
            temp.path(),
            &envelope,
            &Err(AppMeetingControlContributionError::NotAdmissible(
                ALREADY_APPLIED_REFUSAL,
            )),
            true,
            DecisionSpend::unspent(),
        );
        assert_eq!(
            read_control_receipt(temp.path(), &envelope.decision_id)
                .expect("receipt")
                .outcome,
            AppMeetingControlReceiptOutcome::Started,
            "a replay must not overwrite the application's own outcome"
        );
        assert_eq!(
            std::fs::read_dir(control_receipt_dir(temp.path()))
                .expect("ledger")
                .count(),
            1,
            "one signed decision mints exactly one receipt row"
        );
    }

    #[test]
    fn an_owner_decline_is_final_without_spending_the_envelope() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Reject);
        record_control_receipt(
            temp.path(),
            &envelope,
            &Ok(AppMeetingControlApplication::OwnerDeclined),
            true,
            DecisionSpend::unspent(),
        );
        let receipt = read_control_receipt(temp.path(), &envelope.decision_id).expect("receipt");
        assert_eq!(
            receipt.outcome,
            AppMeetingControlReceiptOutcome::OwnerDeclined
        );
        assert!(receipt.session_id.is_none());
        assert!(receipt.error_code.is_none());
    }

    #[test]
    fn a_decision_id_that_is_not_a_lower_hex_digest_reaches_no_ledger_path() {
        let hex = "a".repeat(64);
        for decision_id in [
            "../escape".to_owned(),
            "blake3:../escape".to_owned(),
            format!("blake3:{}", hex.to_uppercase()),
            format!("blake3:{}", &hex[..63]),
            format!("sha256:{hex}"),
        ] {
            assert!(
                decision_hex(&decision_id).is_none(),
                "`{decision_id}` must never become a ledger path component"
            );
        }
        let decision_id = format!("blake3:{hex}");
        assert_eq!(decision_hex(&decision_id), Some(hex.as_str()));
    }

    #[test]
    fn an_unwritable_receipt_ledger_never_wedges_a_control() {
        // A file where a directory is expected. The mint must report failure
        // and return, never unwind into a capture control that already ran.
        let temp = tempfile::tempdir().expect("temp");
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write blocker");
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let receipt = project_control_receipt(&envelope, &started(), Utc::now()).expect("receipt");
        assert!(!mint_control_receipt(&blocked, &receipt));
    }

    #[test]
    fn a_session_id_without_a_rail_prefix_or_with_a_traversal_is_refused() {
        for session_id in ["abc", "../listen-1", "listen-1/../meet-2", "meet\\1"] {
            let mut stop = stop_proposal();
            stop.target = AppMeetingControlTargetV1::LiveSession {
                session_id: session_id.to_owned(),
            };
            assert!(
                stop.validate().is_err(),
                "`{session_id}` must not be an admissible session target"
            );
        }
    }

    /// A minimal authenticated scope. `scope_control_receipt` reads only the
    /// principal and workspace off it, and the system-worker constructor is the
    /// one that needs no session fixture to mint.
    fn worker_scope(principal: &str, workspace: &str) -> AuthenticatedAppScope {
        use crate::magician_v2::apps::models::{AppReference, AppScopeBindingRef};
        use crate::magician_v2::apps::records::AppScope;

        let issued_at = Utc::now();
        AuthenticatedAppScope::from_system_worker(
            AppScope {
                principal: AppReference::parse(principal).expect("principal"),
                workspace: AppReference::parse(workspace).expect("workspace"),
            },
            AppScopeBindingRef::parse(format!("scope-{principal}-{workspace}"))
                .expect("scope binding"),
            AppReference::parse("worker:meeting-control-receipt").expect("worker ref"),
            AppReference::parse("run:meeting-control-receipt").expect("run ref"),
            issued_at,
            issued_at + chrono::Duration::minutes(1),
        )
        .expect("system worker scope")
    }

    /// The publisher and the destination must agree on one root. They run in
    /// different crates on different call stacks, and a drift between them
    /// fails SILENTLY — the receipts view simply stays empty, which is the
    /// exact state this projection exists to end.
    #[test]
    fn the_receipt_publisher_reads_the_root_the_destination_wrote_under() {
        let temp = tempfile::tempdir().expect("temp artifact workspace");
        let artifact_workspace = ArtifactV2Workspace::new(temp.path());
        let authenticated = worker_scope("owner", "default");
        let scope = authenticated.scope();
        // Mint where the destination mints: under the root it derives from the
        // SAME authenticated scope the publisher will resolve.
        let workdirs_root = scope_workdirs_root(
            &artifact_workspace,
            scope.principal.as_str(),
            scope.workspace.as_str(),
        );
        let envelope = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let spent = spend(&workdirs_root, &envelope.decision_id);
        record_control_receipt(&workdirs_root, &envelope, &started(), true, spent);

        let published =
            scope_control_receipt(&artifact_workspace, &authenticated, &envelope.decision_id)
                .expect("the publisher finds the row the destination minted");
        assert_eq!(published.decision_id, envelope.decision_id);
        assert_eq!(published.outcome, AppMeetingControlReceiptOutcome::Started);

        assert!(
            scope_control_receipt(
                &artifact_workspace,
                &worker_scope("intruder", "default"),
                &envelope.decision_id,
            )
            .is_none(),
            "a receipt belongs to the scope that applied the command"
        );
    }

    /// The row has to be nameable in the app store, or the publish is refused
    /// and the receipts view stays empty for a reason no reader would connect
    /// back to this module.
    #[test]
    fn a_published_receipt_is_one_addressable_row_per_signed_decision() {
        let listen = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let stop = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let receipt = project_control_receipt(&listen, &started(), Utc::now()).expect("receipt");
        let record_id = receipt.package_record_id().expect("record id");

        // An app-store record id is an opaque ASCII token: alphanumeric first
        // byte, then only `_`, `-` and `.`, at most 128 bytes.
        assert!(record_id.len() <= 128);
        assert!(record_id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric()));
        assert!(record_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')));
        // The store mints its own ids under `rec_` and refuses a caller that
        // aims into that namespace.
        assert!(!record_id.starts_with("rec_"));

        let hex = listen.decision_id.strip_prefix("blake3:").expect("digest");
        assert_eq!(record_id, format!("receipt-{hex}"));
        assert!(
            receipt.receipt_id.ends_with(hex),
            "the store id and the entity's own key name one decision"
        );

        assert_ne!(
            record_id,
            project_control_receipt(&stop, &started(), Utc::now())
                .expect("receipt")
                .package_record_id()
                .expect("record id"),
            "two decisions are two rows"
        );

        assert_eq!(CONTROL_RECEIPT_ENTITY, "control_receipt");
    }

    /// The store admits a create only when every value matches its compiled
    /// field type, and a refused create leaves the receipts view exactly as
    /// empty as it was before this projection existed. Those admission rules
    /// live in another module, so they are asked of the projector here: a drift
    /// between them has no other symptom.
    #[test]
    fn every_projected_value_is_one_the_entity_store_admits() {
        // The store's OWN validators, not a restatement of them: an enum value
        // has to be a legal app name and a published row has to be nameable as
        // a record, or the create the route issues is refused.
        use crate::magician_v2::apps::models::{AppName, AppRecordId};

        let listen = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let stop = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let fates = [
            (&listen, started()),
            (&listen, Ok(AppMeetingControlApplication::OwnerDeclined)),
            (
                &stop,
                Ok(AppMeetingControlApplication::Stopped {
                    session_id: "listen-abc".to_owned(),
                    final_summary: None,
                }),
            ),
            (
                &stop,
                Err(AppMeetingControlContributionError::NotAdmissible(
                    "a pause, resume or stop requires a live-session target",
                )),
            ),
        ];
        for (envelope, applied) in fates {
            let receipt = project_control_receipt(envelope, &applied, Utc::now()).expect("receipt");
            let row = serde_json::to_value(&receipt).expect("row");
            let row = row.as_object().expect("the row is an object");

            // A `timestamp` field is admitted by an RFC3339 parse, never by
            // merely being string-shaped.
            let applied_at = row
                .get("applied_at")
                .and_then(Value::as_str)
                .expect("applied_at is text");
            assert!(
                DateTime::parse_from_rfc3339(applied_at).is_ok(),
                "`{applied_at}` is not a value the entity's timestamp field accepts"
            );

            // An `enum` field is admitted only when its value is a legal app
            // name AND one the manifest declared.
            let verbs = ["listen", "join", "pause", "resume", "stop"];
            let outcomes = [
                "started",
                "paused",
                "resumed",
                "stopped",
                "owner_declined",
                "refused",
            ];
            for (field, declared) in [("verb", verbs.as_slice()), ("outcome", outcomes.as_slice())]
            {
                let value = row
                    .get(field)
                    .and_then(Value::as_str)
                    .expect("an enum value is text");
                assert!(
                    declared.contains(&value),
                    "`{value}` is not a declared `{field}`"
                );
                assert!(
                    AppName::parse(value).is_ok(),
                    "an enum value the store cannot name is not admissible"
                );
            }

            // `error_code` is declared `text`, so a refusal's closed code has to
            // reach the row as a string rather than as a nested object.
            if let Some(error_code) = row.get("error_code") {
                assert!(
                    error_code.is_string(),
                    "a refusal code must be text in the published row"
                );
            }

            assert!(
                receipt
                    .package_record_id()
                    .is_some_and(|id| AppRecordId::parse(id).is_ok()),
                "every earned receipt must be nameable in the app store"
            );
        }
    }

    /// A live interactive session — the class a capture start requires.
    /// [`worker_scope`] cannot stand in for it here: refusing exactly that
    /// class is one of the things the intent ticket adds.
    fn interactive_scope() -> AuthenticatedAppScope {
        use crate::magician_v2::apps::models::{AppReference, AppRevision, AppScopeBindingRef};
        use crate::magician_v2::apps::records::AppScope;

        let issued_at = Utc::now();
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: AppReference::parse("owner").expect("principal"),
                workspace: AppReference::parse("default").expect("workspace"),
            },
            AppScopeBindingRef::parse("scope-owner-default").expect("scope binding"),
            AppReference::parse("owner").expect("actor ref"),
            AppReference::parse("session:meetings-console").expect("session ref"),
            AppRevision::new(1).expect("authentication revision"),
            issued_at,
            issued_at + chrono::Duration::minutes(5),
        )
        .expect("interactive session scope")
    }

    /// A second START sealed from the SAME click. Two proposals, so two
    /// decisions — which is exactly the pair the applied-decision ledger
    /// cannot tell apart from two separate acts.
    fn second_listen_proposal_from_the_same_act() -> AppMeetingControlProposalV1 {
        let mut header = header(AppMeetingControlVerbV1::Listen);
        header.proposal_id = "proposal:control-2".to_owned();
        header.dedupe_key = "request-2".to_owned();
        AppMeetingControlProposalV1 {
            header,
            ..listen_proposal()
        }
        .seal()
        .expect("second sealed listen proposal")
    }

    #[test]
    fn only_a_live_interactive_session_mints_a_start_intent_ticket() {
        let start = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        assert!(
            AppMeetingStartIntentTicket::mint(&start, &interactive_scope(), 1_000).is_ok(),
            "a start asked for in a live session earns a ticket"
        );
        // A server-owned principal holds a perfectly valid scope. It may stop,
        // pause and resume a capture; it has no act to present, so it may never
        // be the principal that opens a microphone.
        assert!(
            matches!(
                AppMeetingStartIntentTicket::mint(&start, &worker_scope("owner", "default"), 1_000,),
                Err(AppMeetingControlContributionError::NotAdmissible(_))
            ),
            "a background principal cannot supply intent"
        );
    }

    #[test]
    fn only_a_capture_start_earns_a_ticket() {
        let stop = envelope(stop_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        assert!(
            matches!(
                AppMeetingStartIntentTicket::mint(&stop, &interactive_scope(), 1_000),
                Err(AppMeetingControlContributionError::NotAdmissible(_))
            ),
            "a risk-reducing verb has no act to spend"
        );
    }

    #[test]
    fn the_host_bounds_the_intent_window_and_no_app_field_widens_it() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let scope = interactive_scope();
        let start = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let Ok(ticket) = AppMeetingStartIntentTicket::mint(&start, &scope, 1_000) else {
            panic!("a start asked for in a live session earns a ticket");
        };
        // The fixture's gesture and proposal windows are both ceiling-wide, so
        // the host's own bound is the one that closes first. That is the whole
        // point of it: it is the one window in a START no app field can widen.
        assert_eq!(ticket.expires_at_ms, 1_000 + INTENT_TICKET_TTL_MS);
        assert!(matches!(
            ticket.redeem(temp.path(), &start, &scope, 1_000 + INTENT_TICKET_TTL_MS),
            Err(AppMeetingControlContributionError::NotAdmissible(_))
        ));
        assert!(
            std::fs::read_dir(temp.path().join(SPENT_GESTURE_DIR)).is_err(),
            "a refused redemption spends no act"
        );

        // And an act that is already stale never mints one at all.
        assert!(matches!(
            AppMeetingStartIntentTicket::mint(
                &start,
                &scope,
                1_000 + APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS,
            ),
            Err(AppMeetingControlContributionError::NotAdmissible(_))
        ));
    }

    #[test]
    fn a_ticket_minted_for_one_start_cannot_redeem_another() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let scope = interactive_scope();
        let first = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let second = envelope(
            second_listen_proposal_from_the_same_act(),
            AppMeetingControlOwnerDecisionV1::Accept,
        );
        let Ok(ticket) = AppMeetingStartIntentTicket::mint(&first, &scope, 1_000) else {
            panic!("a start asked for in a live session earns a ticket");
        };
        assert!(
            matches!(
                ticket.redeem(temp.path(), &second, &scope, 2_000),
                Err(AppMeetingControlContributionError::NotAdmissible(rule))
                    if rule == "the host-minted intent ticket does not name this capture start"
            ),
            "the ticket is a binding, not a bearer token"
        );
        assert!(
            std::fs::read_dir(temp.path().join(SPENT_GESTURE_DIR)).is_err(),
            "a ticket that names another command spends nothing"
        );
    }

    #[test]
    fn one_surface_act_starts_at_most_one_capture() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let scope = interactive_scope();
        let first = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let second = envelope(
            second_listen_proposal_from_the_same_act(),
            AppMeetingControlOwnerDecisionV1::Accept,
        );
        assert_ne!(
            first.decision_id, second.decision_id,
            "two sealed proposals are two decisions even when one click made both"
        );
        let (Ok(first_ticket), Ok(second_ticket)) = (
            AppMeetingStartIntentTicket::mint(&first, &scope, 1_000),
            AppMeetingStartIntentTicket::mint(&second, &scope, 1_000),
        ) else {
            panic!("each start is admissible on its own");
        };
        assert!(
            first_ticket
                .redeem(temp.path(), &first, &scope, 2_000)
                .is_ok(),
            "the first start spends the act"
        );
        assert!(
            matches!(
                second_ticket.redeem(temp.path(), &second, &scope, 2_000),
                Err(AppMeetingControlContributionError::NotAdmissible(rule))
                    if rule == GESTURE_ALREADY_SPENT_REFUSAL
            ),
            "a second capture from one click is refused whatever envelope carries it"
        );

        // The measurement that makes this ledger necessary rather than
        // redundant: the applied-decision ledger would have let the second
        // start straight through, because its envelope is a different one.
        let mut spend = DecisionSpend::unspent();
        assert!(
            consume_decision(temp.path(), &second.decision_id, &mut spend).is_ok(),
            "the envelope claim cannot see that two decisions share one act"
        );
    }

    #[test]
    fn an_unrecordable_act_fails_the_capture_start_closed() {
        let temp = tempfile::tempdir().expect("temp");
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write blocker");
        let start = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let Ok(ticket) = AppMeetingStartIntentTicket::mint(&start, &interactive_scope(), 1_000)
        else {
            panic!("a start asked for in a live session earns a ticket");
        };
        assert!(
            claim_spent_gesture(&blocked, &ticket).is_err(),
            "an act the destination cannot record is an unbounded number of captures from one click"
        );
    }

    #[test]
    fn a_spent_act_refusal_mints_no_receipt() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let start = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        record_control_receipt(
            temp.path(),
            &start,
            &Err(AppMeetingControlContributionError::NotAdmissible(
                GESTURE_ALREADY_SPENT_REFUSAL,
            )),
            true,
            DecisionSpend::unspent(),
        );
        assert!(
            read_control_receipt(temp.path(), &start.decision_id).is_none(),
            "the call that lost the act may be racing the winner of this very envelope"
        );
    }

    #[test]
    fn the_two_claim_ledgers_are_siblings_and_never_one_file() {
        let temp = tempfile::tempdir().expect("temp workdirs root");
        let scope = interactive_scope();
        let start = envelope(listen_proposal(), AppMeetingControlOwnerDecisionV1::Accept);
        let Ok(ticket) = AppMeetingStartIntentTicket::mint(&start, &scope, 1_000) else {
            panic!("a start asked for in a live session earns a ticket");
        };
        assert!(ticket.redeem(temp.path(), &start, &scope, 2_000).is_ok());
        let _ = spend(temp.path(), &start.decision_id);
        assert_ne!(SPENT_GESTURE_DIR, APPLIED_DECISION_DIR);
        for dir in [SPENT_GESTURE_DIR, APPLIED_DECISION_DIR] {
            assert_eq!(
                std::fs::read_dir(temp.path().join(dir))
                    .expect("ledger directory")
                    .count(),
                1,
                "`{dir}` holds exactly the one claim this start made"
            );
        }
    }
}

/// **Single-join-path pin.** A second attendee-session constructor cannot
/// compile into this module: the only session-creating calls it makes are the
/// two shared owners the first-party API calls, by name.
///
/// The assertion reads the module's PRODUCTION half only — everything before
/// the first `#[cfg(test)]`. Scanning the whole file would be worthless in both
/// directions: the forbidden names appear in this very list, so the negative
/// assertions could never pass, and the required names would be satisfied by
/// their own string literals even if every production call were deleted. The
/// claims sibling splits the same way for the same reason.
///
/// The receipt gate's pin lives here too: it is the same question asked of the
/// same production half — which call sites may reach a durable ledger — and it
/// needs this module's split to ask it.
#[cfg(test)]
mod single_path_pin {
    const SOURCE: &str = include_str!("meeting_control_contribution.rs");

    fn production() -> &'static str {
        SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a production half")
    }

    #[test]
    fn capture_is_created_only_through_the_two_shared_first_party_entries() {
        let production = production();
        assert!(
            production.contains("join_meeting_with_scope("),
            "the attendee rail must be reached through the shared join path"
        );
        assert!(
            production.contains("start_passive_listener("),
            "the listener rail must be reached through the shared start path"
        );
        for forbidden in [
            "meeting_manager().join(",
            "meeting_manager().spawn(",
            "passive_meeting_manager().spawn(",
            "PassiveMeetingSession::new(",
            "MeetingSession::new(",
        ] {
            assert!(
                !production.contains(forbidden),
                "`{forbidden}` would be a second capture-creation path in the app destination"
            );
        }
    }

    #[test]
    fn the_receipt_gate_never_re_reads_the_marker_to_decide_finality() {
        // The bug this closes was one line of plausible code: ask the
        // filesystem whether the envelope is spent. The answer is true for the
        // call that spent it AND for every call that merely lost to it, so the
        // question can only be asked of the claim.
        let production = production();
        assert_eq!(
            production.matches("mark_spent").count(),
            2,
            "the spend is set by the fresh claim and nowhere else"
        );
        assert_eq!(
            production.matches("applied_decision_marker(").count(),
            2,
            "the marker path is built by its definition and the claim; a second \
             reader would be a per-envelope answer to a per-call question"
        );
    }

    #[test]
    fn a_capture_start_spends_a_host_minted_act_before_it_reaches_a_rail() {
        // Ordering is the guarantee, and only source order can state it: the
        // act must be spent inside the start reservation and before either
        // rail, or two starts naming one click can both observe an unspent act
        // and both open a microphone.
        let production = production();
        let reservation = production
            .find("reserve_app_capture_start()")
            .expect("the start destination takes the start reservation");
        let redeem = production
            .find("intent.redeem(")
            .expect("the start destination redeems its intent ticket");
        assert!(
            reservation < redeem,
            "the act is spent inside the reservation, or two starts can both spend it"
        );
        let after_redeem = &production[redeem..];
        let claim = after_redeem
            .find("consume_decision(")
            .expect("the envelope is claimed after the act is spent");
        for rail in ["start_passive_listener(", "join_meeting_with_scope("] {
            assert!(
                after_redeem
                    .find(rail)
                    .is_some_and(|reached| reached > claim),
                "`{rail}` must sit after both the act claim and the envelope claim"
            );
        }
        assert_eq!(
            production.matches("claim_spent_gesture(").count(),
            2,
            "the act ledger is written by its definition and the redemption; a \
             second writer would be a second way to spend one click"
        );
    }

    #[test]
    fn only_a_spent_act_can_answer_started() {
        let production = production();
        assert_eq!(
            production.matches("intent.into_started(").count(),
            2,
            "each rail answers Started by consuming the redeemed intent"
        );
        // Three reads — the receipt projector and the two accessors — and ONE
        // constructor, inside `into_started`. A fifth occurrence is a rail
        // saying a capture began without an act having been spent for it, so
        // re-justify it here before changing this number.
        assert_eq!(
            production
                .matches("AppMeetingControlApplication::Started {")
                .count(),
            4,
            "Started is constructed only by spending a redeemed intent ticket"
        );
    }

    #[test]
    fn the_pin_reads_only_the_production_half() {
        // Guards the guard: if the split ever stopped working, `production`
        // would contain this module's own forbidden-name list and the test
        // above would fail for the wrong reason.
        assert!(
            !production().contains("mod single_path_pin"),
            "the production half must not include the pin's own source"
        );
        assert!(production().contains("pub async fn apply_signed_app_meeting_control"));
    }

    #[test]
    fn the_destination_mints_a_receipt_and_never_publishes_one() {
        // The split IS the guarantee. This module writes the host-owned ledger;
        // the authenticated route reads that ledger and publishes the row. A
        // publisher compiled in here would have the seam's in-memory result in
        // hand, and the first convenient refactor would publish THAT — a row
        // for a fate no ledger ever recorded, which is the one thing an app
        // must never be able to show.
        let production = production();
        for forbidden in ["owner_mutate(", "surface_mutate(", "workflow_mutate("] {
            assert!(
                !production.contains(forbidden),
                "`{forbidden}` would publish a receipt from inside the destination seam"
            );
        }
        assert!(
            production.contains("pub fn scope_control_receipt"),
            "the route reaches the ledger through this read and no other"
        );
    }
}
