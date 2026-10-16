#[cfg(feature = "test-fixtures")]
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
#[cfg(feature = "test-fixtures")]
use chrono::{DateTime, Utc};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
#[cfg(feature = "test-fixtures")]
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::recipient_compliance::{
    install_compliance_scheduling_reader, ComplianceNegotiation, ComplianceNegotiationState,
    ComplianceReply, ComplianceSchedulingReader,
};

use super::{NegotiationState, SchedulingScope, SchedulingStore};
#[cfg(feature = "test-fixtures")]
use super::{Reply, ReplyKind, Slot};

struct MediaComplianceSchedulingReader;

impl ComplianceSchedulingReader for MediaComplianceSchedulingReader {
    fn negotiations(
        &self,
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ComplianceNegotiation>> {
        read_recipient_compliance_negotiations(workspace_layout, principal, workspace)
    }
}

/// Install the scheduling owner's read-only projection into the core
/// recipient-compliance gate. Repeated installation is harmless: the first
/// process-wide owner remains authoritative.
pub fn install_recipient_compliance_scheduling_reader() -> bool {
    install_compliance_scheduling_reader(Arc::new(MediaComplianceSchedulingReader))
}

/// Project the append-only scheduling owner into the closed facts consumed by
/// recipient compliance. The core gate never receives a store or write handle.
pub fn read_recipient_compliance_negotiations(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<Vec<ComplianceNegotiation>> {
    SchedulingStore::new(workspace_layout.clone())
        .all_negotiations(&SchedulingScope::new(principal, workspace))?
        .into_iter()
        .map(|negotiation| {
            let state = match negotiation.state() {
                NegotiationState::AwaitingReply => ComplianceNegotiationState::AwaitingReply,
                NegotiationState::Accepted => ComplianceNegotiationState::Accepted,
                NegotiationState::Declined => ComplianceNegotiationState::Declined,
                NegotiationState::Countered => ComplianceNegotiationState::Countered,
                NegotiationState::Held => ComplianceNegotiationState::Held,
                NegotiationState::Closed => ComplianceNegotiationState::Closed,
            };
            let latest_reply = negotiation.replies.last().map(|reply| ComplianceReply {
                at: reply.at,
                kind: reply.kind.as_str().to_string(),
            });
            Ok(ComplianceNegotiation {
                negotiation_id: negotiation.negotiation_id,
                audience: negotiation.audience,
                counterparty: negotiation.counterparty,
                purpose: negotiation.purpose,
                state,
                latest_reply,
            })
        })
        .collect()
}

/// Test-cycle bridge for the core crate's unit tests. Cargo builds the core
/// test harness and this satellite's core dependency as distinct nominal
/// crates, so typed values cannot cross that dev-dependency cycle. JSON keeps
/// the bridge read-only while exercising the real scheduling store.
#[cfg(feature = "test-fixtures")]
pub fn read_recipient_compliance_negotiations_json(
    base_root: &Path,
    principal: &str,
    workspace: &str,
) -> Result<Vec<u8>> {
    let projected = read_recipient_compliance_negotiations(
        &ArtifactV2Workspace::new(base_root),
        principal,
        workspace,
    )?;
    Ok(serde_json::to_vec(&projected)?)
}

#[cfg(feature = "test-fixtures")]
pub fn open_recipient_compliance_negotiation_for_test(
    base_root: &Path,
    principal: &str,
    workspace: &str,
    audience_json: &[u8],
    counterparty: &str,
    purpose: &str,
    slots: &[Slot],
    now: DateTime<Utc>,
) -> Result<String> {
    let audience: AudienceRef = serde_json::from_slice(audience_json)?;
    Ok(SchedulingStore::new(ArtifactV2Workspace::new(base_root))
        .open(
            &SchedulingScope::new(principal, workspace),
            &audience,
            counterparty,
            purpose,
            slots,
            None,
            now,
        )?
        .negotiation_id)
}

#[cfg(feature = "test-fixtures")]
pub fn absorb_recipient_compliance_reply_for_test(
    base_root: &Path,
    principal: &str,
    workspace: &str,
    audience_json: &[u8],
    negotiation_id: &str,
    source_ref: String,
    at: DateTime<Utc>,
    kind: ReplyKind,
) -> Result<()> {
    let audience: AudienceRef = serde_json::from_slice(audience_json)?;
    SchedulingStore::new(ArtifactV2Workspace::new(base_root)).absorb(
        &SchedulingScope::new(principal, workspace),
        &audience,
        negotiation_id,
        &Reply {
            source_ref,
            at,
            kind,
        },
    )?;
    Ok(())
}

#[cfg(feature = "test-fixtures")]
pub fn close_recipient_compliance_negotiation_for_test(
    base_root: &Path,
    principal: &str,
    workspace: &str,
    audience_json: &[u8],
    negotiation_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let audience: AudienceRef = serde_json::from_slice(audience_json)?;
    SchedulingStore::new(ArtifactV2Workspace::new(base_root)).close(
        &SchedulingScope::new(principal, workspace),
        &audience,
        negotiation_id,
        reason,
        now,
    )?;
    Ok(())
}
