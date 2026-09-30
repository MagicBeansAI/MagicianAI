//! Common typestate kernel for one Apps effect.
//!
//! The kernel does not replace lifecycle, grant, resource, disclosure or
//! result-retention owners. It closes the identity gap between them: one
//! move-only permit binds their exact evidence to one primitive action,
//! physical target and canonical input. The resource authority remains the
//! durable exact-once settlement owner; consuming the typestate returns its
//! original move-only permit with one unambiguous settlement disposition.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::ResolvedAppAuthority,
    models::{AppDigest, AppInstallationId, AppReference, AppRevision},
    package_lock::{AppLockedPrimitiveActionBinding, AppLockedPrimitiveBinding, AppPackageLock},
    records::AppPackageRevision,
    resource_contract::AppEffectDispatchAbortReason,
    tool_disclosure::{AppToolDisclosureError, AppToolDisclosurePermit, AttestedAppToolTarget},
};

pub const MAX_APP_EFFECT_RESULT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const APP_EFFECT_BINDING_SCHEMA_V1: &str = "magician.app-effect-binding.v1";
pub(crate) const APP_EFFECT_BINDING_SCHEMA_V2: &str = "magician.app-effect-binding.v2";

/// Physical-owner evidence. It is neither serializable nor deserializable;
/// only a trusted compiled, OS-jail, browser or device adapter can construct
/// it after resolving its concrete target.
pub(crate) struct AppEffectPhysicalTarget {
    target_ref: AppReference,
    owner_bytes_digest: AppDigest,
    target: AttestedAppToolTarget,
}

impl AppEffectPhysicalTarget {
    pub(crate) fn from_owner(
        target_ref: AppReference,
        owner_bytes_digest: AppDigest,
        target: AttestedAppToolTarget,
    ) -> Self {
        Self {
            target_ref,
            owner_bytes_digest,
            target,
        }
    }

    pub(crate) fn target(&self) -> &AttestedAppToolTarget {
        &self.target
    }
}

/// Crate-private adapter seam shared by compiled, jail and interactive owners.
/// Implementations may attest identity only; effect authority is minted here.
pub(crate) trait AppEffectPhysicalOwner {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError>;
}

/// Immutable audit identity for one admitted effect.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEffectBinding {
    schema: &'static str,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    package_content_digest: AppDigest,
    package_lock_digest: AppDigest,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    schema_revision: AppRevision,
    schema_digest: AppDigest,
    authority_digest: AppDigest,
    primitive_ref: AppReference,
    primitive_descriptor_digest: AppDigest,
    primitive_source_digest: AppDigest,
    action_ref: AppReference,
    action_digest: AppDigest,
    input_schema_digest: Option<AppDigest>,
    result_schema_digest: Option<AppDigest>,
    implementation_plan_digest: AppDigest,
    physical_target_ref: AppReference,
    physical_owner_bytes_digest: AppDigest,
    physical_target_digest: AppDigest,
    invocation_ref: AppReference,
    canonical_input_digest: AppDigest,
    canonical_input_bytes: u64,
    result_byte_ceiling: u64,
    admitted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    binding_digest: AppDigest,
}

impl AppEffectBinding {
    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }

    pub fn primitive_ref(&self) -> &AppReference {
        &self.primitive_ref
    }

    pub fn action_ref(&self) -> &AppReference {
        &self.action_ref
    }

    pub fn physical_target_ref(&self) -> &AppReference {
        &self.physical_target_ref
    }

    pub fn result_byte_ceiling(&self) -> u64 {
        self.result_byte_ceiling
    }

    pub fn matches_input_at(&self, bytes: &[u8], now: &DateTime<Utc>) -> bool {
        now >= &self.admitted_at
            && now < &self.expires_at
            && usize::try_from(self.canonical_input_bytes).ok() == Some(bytes.len())
            && self.canonical_input_digest == AppDigest::blake3(bytes)
    }
}

/// Borrowed current evidence. Callers must already have resolved current
/// lifecycle/grant/schema and the current locked descriptor.
pub(crate) struct AppEffectAdmission<'a> {
    pub authority: &'a ResolvedAppAuthority,
    pub package: &'a AppPackageRevision,
    pub package_lock: &'a AppPackageLock,
    /// Canonical digest of the current active schema revision, supplied by the
    /// schema owner that resolved `authority.schema_revision`.
    pub schema_digest: &'a AppDigest,
    pub primitive: &'a AppLockedPrimitiveBinding,
    pub action: &'a AppLockedPrimitiveActionBinding,
    pub physical_target: AppEffectPhysicalTarget,
    pub invocation_ref: AppReference,
    pub canonical_input: &'a [u8],
    pub result_byte_ceiling: usize,
    pub admitted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// One common permit. `R` is the resource owner's existing move-only permit.
/// This type deliberately implements neither `Clone` nor Serde.
pub(crate) struct AppEffectPermit<R> {
    binding: AppEffectBinding,
    resource_permit: R,
    disclosure_permit: AppToolDisclosurePermit,
}

/// I/O has crossed. Cancellation can no longer claim proven-unspent state.
pub(crate) struct AppEffectInFlight<R> {
    binding: AppEffectBinding,
    resource_permit: R,
    disclosure_permit: AppToolDisclosurePermit,
    provider_io_authorization_available: bool,
}

/// Owned one-shot proof that common app admission reached the post-start,
/// post-final-fence provider-I/O state. It is Send/'static and intentionally
/// carries no settlement/resource authority, so a blocking physical owner may
/// consume it without moving the borrowed workflow operation into
/// `spawn_blocking`. It implements neither Clone nor Serde.
pub(crate) struct AppEffectProviderIoAuthorization {
    binding: AppEffectBinding,
}

impl AppEffectProviderIoAuthorization {
    pub(crate) fn binding(&self) -> &AppEffectBinding {
        &self.binding
    }
}

/// Durable dispatch-start is recorded, but the physical provider has not been
/// polled. Consuming this state chooses exactly one transition: abort with a
/// typed no-poll proof, or begin provider I/O. After `begin_io`, abort is no
/// longer representable.
pub(crate) struct AppEffectStartedNotPolled<R> {
    binding: AppEffectBinding,
    resource_permit: R,
    disclosure_permit: AppToolDisclosurePermit,
}

pub(crate) struct AppEffectAbortProof {
    effect_binding_digest: AppDigest,
    reason: AppEffectDispatchAbortReason,
}

impl AppEffectAbortProof {
    pub(crate) fn effect_binding_digest(&self) -> &AppDigest {
        &self.effect_binding_digest
    }

    pub(crate) fn reason(&self) -> AppEffectDispatchAbortReason {
        self.reason
    }
}

pub(crate) struct AppEffectAbortedBeforeIo<R> {
    resource_permit: R,
    disclosure_permit: AppToolDisclosurePermit,
    proof: AppEffectAbortProof,
}

impl<R> AppEffectAbortedBeforeIo<R> {
    pub(crate) fn into_parts(self) -> (R, AppToolDisclosurePermit, AppEffectAbortProof) {
        (self.resource_permit, self.disclosure_permit, self.proof)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectSettlementDisposition {
    ReleaseProvenUnspent,
    CommitObserved,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectStage {
    Admitted,
    FinalPreIoFence,
    ProviderIo,
    ResultMaterialization,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEffectResultLabel {
    pub effect_binding_digest: AppDigest,
    pub result_digest: AppDigest,
    pub result_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEffectSettlementReceipt {
    pub effect_binding_digest: AppDigest,
    pub disposition: AppEffectSettlementDisposition,
    pub terminal_stage: AppEffectStage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AppEffectResultLabel>,
}

impl AppEffectSettlementReceipt {
    pub fn effect_binding_digest(&self) -> &AppDigest {
        &self.effect_binding_digest
    }

    pub fn disposition(&self) -> AppEffectSettlementDisposition {
        self.disposition
    }

    pub fn terminal_stage(&self) -> AppEffectStage {
        self.terminal_stage
    }

    pub fn result(&self) -> Option<&AppEffectResultLabel> {
        self.result.as_ref()
    }

    /// Canonical committed receipt material used by the sealed workflow
    /// completion intent. This does not grant settlement authority; recovery
    /// must still re-open the exact journal reservation and effect binding.
    pub(crate) fn committed_result(
        effect_binding_digest: AppDigest,
        canonical_result_bytes: &[u8],
    ) -> Result<Self, AppEffectKernelError> {
        let result_bytes = u64::try_from(canonical_result_bytes.len())
            .map_err(|_| AppEffectKernelError::ResultTooLarge)?;
        Ok(Self {
            effect_binding_digest: effect_binding_digest.clone(),
            disposition: AppEffectSettlementDisposition::CommitObserved,
            terminal_stage: AppEffectStage::ResultMaterialization,
            result: Some(AppEffectResultLabel {
                effect_binding_digest,
                result_digest: AppDigest::blake3(canonical_result_bytes),
                result_bytes,
            }),
        })
    }

    pub(crate) fn matches_committed_result(
        &self,
        effect_binding_digest: &AppDigest,
        canonical_result_bytes: &[u8],
    ) -> bool {
        self.disposition == AppEffectSettlementDisposition::CommitObserved
            && self.terminal_stage == AppEffectStage::ResultMaterialization
            && &self.effect_binding_digest == effect_binding_digest
            && self.result.as_ref().is_some_and(|result| {
                &result.effect_binding_digest == effect_binding_digest
                    && usize::try_from(result.result_bytes).ok()
                        == Some(canonical_result_bytes.len())
                    && result.result_digest == AppDigest::blake3(canonical_result_bytes)
            })
    }
}

/// Terminal typestate. Consuming it returns the exact original resource permit
/// plus one disposition for the canonical resource authority.
pub(crate) struct AppEffectSettlement<R> {
    resource_permit: R,
    disclosure_permit: Option<AppToolDisclosurePermit>,
    receipt: AppEffectSettlementReceipt,
}

impl<R> AppEffectSettlement<R> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        R,
        Option<AppToolDisclosurePermit>,
        AppEffectSettlementReceipt,
    ) {
        (self.resource_permit, self.disclosure_permit, self.receipt)
    }
}

#[allow(dead_code)] // Alternate typed transition helpers remain part of the sealed effect API.
impl<R> AppEffectPermit<R> {
    pub(crate) fn authorize_binding(
        admission: &AppEffectAdmission<'_>,
        disclosure_permit: &AppToolDisclosurePermit,
    ) -> Result<AppEffectBinding, AppEffectKernelError> {
        let binding = binding_from_admission(admission)?;
        validate_disclosure(admission, disclosure_permit)?;
        Ok(binding)
    }

    /// Complete the move-only handoff after a final fence already called
    /// `authorize_binding`. The binding's fields are private, so callers cannot
    /// manufacture this state from transport or persisted data.
    pub(crate) fn from_authorized(
        binding: AppEffectBinding,
        resource_permit: R,
        disclosure_permit: AppToolDisclosurePermit,
    ) -> Self {
        Self {
            binding,
            resource_permit,
            disclosure_permit,
        }
    }

    pub(crate) fn binding(&self) -> &AppEffectBinding {
        &self.binding
    }

    pub(crate) fn cancel_before_io(self, stage: AppEffectStage) -> AppEffectSettlement<R> {
        let stage = match stage {
            AppEffectStage::Admitted | AppEffectStage::FinalPreIoFence => stage,
            AppEffectStage::ProviderIo | AppEffectStage::ResultMaterialization => {
                AppEffectStage::FinalPreIoFence
            },
        };
        AppEffectSettlement {
            resource_permit: self.resource_permit,
            disclosure_permit: None,
            receipt: AppEffectSettlementReceipt {
                effect_binding_digest: self.binding.binding_digest,
                disposition: AppEffectSettlementDisposition::ReleaseProvenUnspent,
                terminal_stage: stage,
                result: None,
            },
        }
    }

    /// Consume an already-authorized permit after the durable dispatch-start
    /// mutation has been attempted. From this point no local validation or
    /// lost response may manufacture `ReleaseProvenUnspent`; the journal may
    /// already contain the exact effect identity.
    pub(crate) fn outcome_uncertain_after_dispatch_start(
        self,
        stage: AppEffectStage,
    ) -> AppEffectSettlement<R> {
        AppEffectInFlight {
            binding: self.binding,
            resource_permit: self.resource_permit,
            disclosure_permit: self.disclosure_permit,
            provider_io_authorization_available: false,
        }
        .outcome_uncertain(stage)
    }

    pub(crate) fn mark_dispatch_started(self) -> AppEffectStartedNotPolled<R> {
        AppEffectStartedNotPolled {
            binding: self.binding,
            resource_permit: self.resource_permit,
            disclosure_permit: self.disclosure_permit,
        }
    }
}

impl<R> AppEffectStartedNotPolled<R> {
    pub(crate) fn abort_before_io(
        self,
        reason: AppEffectDispatchAbortReason,
    ) -> AppEffectAbortedBeforeIo<R> {
        AppEffectAbortedBeforeIo {
            proof: AppEffectAbortProof {
                effect_binding_digest: self.binding.binding_digest,
                reason,
            },
            resource_permit: self.resource_permit,
            disclosure_permit: self.disclosure_permit,
        }
    }

    /// Revalidate after a successful durable dispatch-start. A mismatch is an
    /// uncertain outcome, never a proven-unspent cancellation, because the
    /// start checkpoint is already authoritative.
    pub(crate) fn begin_io(
        self,
        current: &AppEffectBinding,
        canonical_input: &[u8],
        now: DateTime<Utc>,
    ) -> Result<AppEffectInFlight<R>, AppEffectSettlement<R>> {
        if current != &self.binding
            || !self.binding.matches_input_at(canonical_input, &now)
            || !self
                .disclosure_permit
                .matches_disclosure_bytes_at(canonical_input, &now)
        {
            return Err(AppEffectInFlight {
                binding: self.binding,
                resource_permit: self.resource_permit,
                disclosure_permit: self.disclosure_permit,
                provider_io_authorization_available: false,
            }
            .outcome_uncertain(AppEffectStage::FinalPreIoFence));
        }
        Ok(AppEffectInFlight {
            binding: self.binding,
            resource_permit: self.resource_permit,
            disclosure_permit: self.disclosure_permit,
            provider_io_authorization_available: true,
        })
    }
}

fn validate_disclosure(
    admission: &AppEffectAdmission<'_>,
    disclosure_permit: &AppToolDisclosurePermit,
) -> Result<(), AppEffectKernelError> {
    if disclosure_permit.target() != admission.physical_target.target()
        || disclosure_permit.invocation_ref() != &admission.invocation_ref
        || disclosure_permit.authority_digest() != &admission.authority.authority_digest
        || disclosure_permit.result_byte_ceiling()
            < u64::try_from(admission.result_byte_ceiling).unwrap_or(u64::MAX)
        || !disclosure_permit
            .matches_disclosure_bytes_at(admission.canonical_input, &admission.admitted_at)
    {
        return Err(AppEffectKernelError::DisclosureMismatch);
    }
    Ok(())
}

impl<R> AppEffectInFlight<R> {
    pub(crate) fn take_provider_io_authorization(
        &mut self,
    ) -> Option<AppEffectProviderIoAuthorization> {
        if !self.provider_io_authorization_available {
            return None;
        }
        self.provider_io_authorization_available = false;
        Some(AppEffectProviderIoAuthorization {
            binding: self.binding.clone(),
        })
    }

    pub(crate) fn commit_result(
        self,
        result_bytes: &[u8],
    ) -> Result<AppEffectSettlement<R>, AppEffectSettlement<R>> {
        let result_len = u64::try_from(result_bytes.len()).unwrap_or(u64::MAX);
        if result_len > self.binding.result_byte_ceiling {
            return Err(self.outcome_uncertain(AppEffectStage::ResultMaterialization));
        }
        let label = AppEffectResultLabel {
            effect_binding_digest: self.binding.binding_digest.clone(),
            result_digest: AppDigest::blake3(result_bytes),
            result_bytes: result_len,
        };
        Ok(AppEffectSettlement {
            resource_permit: self.resource_permit,
            disclosure_permit: Some(self.disclosure_permit),
            receipt: AppEffectSettlementReceipt {
                effect_binding_digest: self.binding.binding_digest,
                disposition: AppEffectSettlementDisposition::CommitObserved,
                terminal_stage: AppEffectStage::ResultMaterialization,
                result: Some(label),
            },
        })
    }

    pub(crate) fn outcome_uncertain(self, stage: AppEffectStage) -> AppEffectSettlement<R> {
        AppEffectSettlement {
            resource_permit: self.resource_permit,
            disclosure_permit: Some(self.disclosure_permit),
            receipt: AppEffectSettlementReceipt {
                effect_binding_digest: self.binding.binding_digest,
                disposition: AppEffectSettlementDisposition::OutcomeUncertain,
                terminal_stage: match stage {
                    AppEffectStage::Admitted | AppEffectStage::FinalPreIoFence => {
                        AppEffectStage::ProviderIo
                    },
                    AppEffectStage::ProviderIo | AppEffectStage::ResultMaterialization => stage,
                },
                result: None,
            },
        }
    }
}

fn binding_from_admission(
    admission: &AppEffectAdmission<'_>,
) -> Result<AppEffectBinding, AppEffectKernelError> {
    if admission.result_byte_ceiling == 0
        || admission.result_byte_ceiling > MAX_APP_EFFECT_RESULT_BYTES
        || admission.action.transport_result_byte_ceiling()
            != u64::try_from(admission.result_byte_ceiling).ok()
        || admission.canonical_input.is_empty()
        || admission.expires_at <= admission.admitted_at
        || admission
            .authority
            .canonical_authority_digest()
            .map_err(|error| AppEffectKernelError::Authority(error.to_string()))?
            != admission.authority.authority_digest
        || !admission
            .authority
            .permits_tool(admission.physical_target.target().tool_ref())
        || admission.package.content_digest != *admission.package_lock.bundle_digest()
        || admission.package_lock.lock_digest() != &admission.package.dependency_lock_digest
        || admission.primitive.source_content_digest()
            != &admission.physical_target.owner_bytes_digest
        || admission.action.dispatchable() == false
        || !admission
            .primitive
            .actions()
            .iter()
            .any(|action| action == admission.action)
        || admission.physical_target.target().tool_ref()
            != admission
                .package_lock
                .capability_for_primitive(admission.primitive.primitive_ref())
                .map(|dependency| dependency.dependency_ref())
                .ok_or(AppEffectKernelError::PrimitiveNotLocked)?
    {
        return Err(AppEffectKernelError::IdentityMismatch);
    }
    let canonical_input_bytes = u64::try_from(admission.canonical_input.len())
        .map_err(|_| AppEffectKernelError::InputTooLarge)?;
    let result_byte_ceiling = u64::try_from(admission.result_byte_ceiling)
        .map_err(|_| AppEffectKernelError::ResultTooLarge)?;
    let physical_target_digest = admission.physical_target.target.identity_digest()?;
    let mut binding = AppEffectBinding {
        schema: APP_EFFECT_BINDING_SCHEMA_V2,
        installation_id: admission.authority.installation_id.clone(),
        installation_generation: admission.authority.installation_generation,
        package_revision_ref: admission.authority.package_revision_ref.clone(),
        package_content_digest: admission.package.content_digest.clone(),
        package_lock_digest: admission.package_lock.lock_digest().clone(),
        grant_revision: admission.authority.grant_revision,
        grant_digest: admission.authority.grant_authority_digest.clone(),
        schema_revision: admission.authority.schema_revision,
        schema_digest: admission.schema_digest.clone(),
        authority_digest: admission.authority.authority_digest.clone(),
        primitive_ref: admission.primitive.primitive_ref().clone(),
        primitive_descriptor_digest: admission.primitive.descriptor_digest().clone(),
        primitive_source_digest: admission.primitive.source_content_digest().clone(),
        action_ref: admission.action.action_ref().clone(),
        action_digest: admission.action.action_digest().clone(),
        input_schema_digest: admission.action.input_schema_digest().cloned(),
        result_schema_digest: admission.action.result_schema_digest().cloned(),
        implementation_plan_digest: admission
            .action
            .implementation_plan_digest()
            .cloned()
            .ok_or(AppEffectKernelError::IdentityMismatch)?,
        physical_target_ref: admission.physical_target.target_ref.clone(),
        physical_owner_bytes_digest: admission.physical_target.owner_bytes_digest.clone(),
        physical_target_digest,
        invocation_ref: admission.invocation_ref.clone(),
        canonical_input_digest: AppDigest::blake3(admission.canonical_input),
        canonical_input_bytes,
        result_byte_ceiling,
        admitted_at: admission.admitted_at,
        expires_at: admission.expires_at,
        binding_digest: AppDigest::blake3(b"pending-app-effect-binding"),
    };
    binding.binding_digest = binding_digest(&binding)?;
    Ok(binding)
}

/// Stable idempotency identity for one logical effect. V1 incorrectly hashed
/// the short-lived admission window, making byte-identical crash recovery
/// impossible because a fresh current-authority fence necessarily has new
/// timestamps. V2 retains those timestamps in the move-only binding and
/// checks them at every live transition, but deliberately excludes them from
/// the durable identity digest.
fn binding_digest(binding: &AppEffectBinding) -> Result<AppDigest, AppEffectKernelError> {
    #[derive(Serialize)]
    struct DigestMaterial<'a> {
        schema: &'static str,
        installation_id: &'a AppInstallationId,
        installation_generation: u64,
        package_revision_ref: &'a AppReference,
        package_content_digest: &'a AppDigest,
        package_lock_digest: &'a AppDigest,
        grant_revision: AppRevision,
        grant_digest: &'a AppDigest,
        schema_revision: AppRevision,
        schema_digest: &'a AppDigest,
        authority_digest: &'a AppDigest,
        primitive_ref: &'a AppReference,
        primitive_descriptor_digest: &'a AppDigest,
        primitive_source_digest: &'a AppDigest,
        action_ref: &'a AppReference,
        action_digest: &'a AppDigest,
        input_schema_digest: Option<&'a AppDigest>,
        result_schema_digest: Option<&'a AppDigest>,
        implementation_plan_digest: &'a AppDigest,
        physical_target_ref: &'a AppReference,
        physical_owner_bytes_digest: &'a AppDigest,
        physical_target_digest: &'a AppDigest,
        invocation_ref: &'a AppReference,
        canonical_input_digest: &'a AppDigest,
        canonical_input_bytes: u64,
        result_byte_ceiling: u64,
    }
    let material = DigestMaterial {
        schema: binding.schema,
        installation_id: &binding.installation_id,
        installation_generation: binding.installation_generation,
        package_revision_ref: &binding.package_revision_ref,
        package_content_digest: &binding.package_content_digest,
        package_lock_digest: &binding.package_lock_digest,
        grant_revision: binding.grant_revision,
        grant_digest: &binding.grant_digest,
        schema_revision: binding.schema_revision,
        schema_digest: &binding.schema_digest,
        authority_digest: &binding.authority_digest,
        primitive_ref: &binding.primitive_ref,
        primitive_descriptor_digest: &binding.primitive_descriptor_digest,
        primitive_source_digest: &binding.primitive_source_digest,
        action_ref: &binding.action_ref,
        action_digest: &binding.action_digest,
        input_schema_digest: binding.input_schema_digest.as_ref(),
        result_schema_digest: binding.result_schema_digest.as_ref(),
        implementation_plan_digest: &binding.implementation_plan_digest,
        physical_target_ref: &binding.physical_target_ref,
        physical_owner_bytes_digest: &binding.physical_owner_bytes_digest,
        physical_target_digest: &binding.physical_target_digest,
        invocation_ref: &binding.invocation_ref,
        canonical_input_digest: &binding.canonical_input_digest,
        canonical_input_bytes: binding.canonical_input_bytes,
        result_byte_ceiling: binding.result_byte_ceiling,
    };
    AppDigest::blake3_canonical_json(
        &serde_json::to_value(material)
            .map_err(|error| AppEffectKernelError::Encoding(error.to_string()))?,
    )
    .map_err(|error| AppEffectKernelError::Encoding(error.to_string()))
}

#[derive(Debug, Error)]
pub enum AppEffectKernelError {
    #[error("app effect identity does not match current authority or immutable package evidence")]
    IdentityMismatch,
    #[error("app effect primitive is absent from the package lock")]
    PrimitiveNotLocked,
    #[error("app effect disclosure permit does not match the physical target or input bytes")]
    DisclosureMismatch,
    #[error("app effect input exceeds the supported identity range")]
    InputTooLarge,
    #[error("app effect result ceiling exceeds the supported bound")]
    ResultTooLarge,
    #[error("failed to encode app effect identity: {0}")]
    Encoding(String),
    #[error("app effect authority evidence is invalid: {0}")]
    Authority(String),
    #[error(transparent)]
    Disclosure(#[from] AppToolDisclosureError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("reference")
    }

    fn revision() -> AppRevision {
        AppRevision::new(1).expect("revision")
    }

    fn test_binding(admitted_at: DateTime<Utc>, expires_at: DateTime<Utc>) -> AppEffectBinding {
        let digest = AppDigest::blake3(b"identity");
        let mut binding = AppEffectBinding {
            schema: APP_EFFECT_BINDING_SCHEMA_V2,
            installation_id: AppInstallationId::parse("install_effect").expect("installation"),
            installation_generation: 1,
            package_revision_ref: reference("package:revision"),
            package_content_digest: digest.clone(),
            package_lock_digest: digest.clone(),
            grant_revision: revision(),
            grant_digest: digest.clone(),
            schema_revision: revision(),
            schema_digest: digest.clone(),
            authority_digest: digest.clone(),
            primitive_ref: reference("primitive:effect"),
            primitive_descriptor_digest: digest.clone(),
            primitive_source_digest: digest.clone(),
            action_ref: reference("action:effect"),
            action_digest: digest.clone(),
            input_schema_digest: Some(digest.clone()),
            result_schema_digest: Some(digest.clone()),
            implementation_plan_digest: digest.clone(),
            physical_target_ref: reference("physical:effect"),
            physical_owner_bytes_digest: digest.clone(),
            physical_target_digest: digest.clone(),
            invocation_ref: reference("invocation:effect"),
            canonical_input_digest: AppDigest::blake3(b"{}"),
            canonical_input_bytes: 2,
            result_byte_ceiling: 1_024,
            admitted_at,
            expires_at,
            binding_digest: AppDigest::blake3(b"pending"),
        };
        binding.binding_digest = binding_digest(&binding).expect("binding digest");
        binding
    }

    #[test]
    fn v2_identity_is_recoverable_but_each_live_window_remains_enforced() {
        let first_start = DateTime::parse_from_rfc3339("2026-08-22T00:00:00Z")
            .expect("time")
            .with_timezone(&Utc);
        let second_start = DateTime::parse_from_rfc3339("2026-08-22T01:00:00Z")
            .expect("time")
            .with_timezone(&Utc);
        let first = test_binding(first_start, first_start + chrono::Duration::seconds(5));
        let second = test_binding(second_start, second_start + chrono::Duration::seconds(5));

        assert_eq!(first.schema, APP_EFFECT_BINDING_SCHEMA_V2);
        assert_eq!(first.binding_digest(), second.binding_digest());
        assert!(first.matches_input_at(b"{}", &(first_start + chrono::Duration::seconds(1))));
        assert!(!first.matches_input_at(b"{}", &second_start));
        assert!(second.matches_input_at(b"{}", &(second_start + chrono::Duration::seconds(1)),));
    }
}
