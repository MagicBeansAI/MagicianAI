//! Phase-0 app-platform threat and adversarial-case inventory.
//!
//! This matrix is executable documentation. `ContractSpecified` means a pure
//! dormant kernel test exists; it does not mean a route, store, dispatcher or
//! provider adapter is protected. Those claims remain explicitly pending until
//! a test crosses the named load-bearing boundary.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppThreatActor {
    PackageOrWorkflowAuthor,
    MutableSkillDependency,
    StoredOrExternalPromptInjection,
    ModelOutput,
    ForgedClient,
    ProviderOrEndpoint,
    SameUserLocalProcess,
    CrashReplayRecovery,
    CustomSurfaceDocument,
}

impl AppThreatActor {
    pub const ALL: [Self; 9] = [
        Self::PackageOrWorkflowAuthor,
        Self::MutableSkillDependency,
        Self::StoredOrExternalPromptInjection,
        Self::ModelOutput,
        Self::ForgedClient,
        Self::ProviderOrEndpoint,
        Self::SameUserLocalProcess,
        Self::CrashReplayRecovery,
        Self::CustomSurfaceDocument,
    ];
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppTrustZone {
    TrustedServerKernel,
    AuthenticatedSessionAdapter,
    UntrustedPackage,
    UntrustedModelOrExternalContent,
    GovernedSkillRuntime,
    ConsequentialDispatch,
    ScopedAppStore,
    GovernedMemoryStore,
    StorageGovernance,
    GovernedArchiveWriter,
    CanonicalResourceAuthority,
    RetainedThirdPartyProvider,
    SandboxedCustomSurface,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppRedCaseStatus {
    ContractSpecified,
    /// A dormant, non-deserializable enforcement fence and boundary test are
    /// present. This still does not claim that a later live route/store/runtime
    /// adopter has shipped.
    DormantBoundarySpecified,
    LoadBearingAdapterPending,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppRedCaseExpectation {
    FailClosed,
    MetadataOnly,
    ExactIdentityOnly,
    NoAuthorityChange,
    BoundedRejection,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRedCase {
    pub id: &'static str,
    pub threat: AppThreatActor,
    pub boundary: AppTrustZone,
    pub expectation: AppRedCaseExpectation,
    pub status: AppRedCaseStatus,
}

/// Canonical Phase-0 red-case matrix. Pending rows are deliberately retained:
/// projection-only/kernel tests cannot close a dispatch or storage claim.
pub const APP_PHASE0_RED_CASES: &[AppRedCase] = &[
    AppRedCase {
        id: "manifest-kernel-hostile-input-bounded",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "manifest-untrusted-input-bounded",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::UntrustedPackage,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "dependency-lock-kernel-requires-immutable-identity",
        threat: AppThreatActor::MutableSkillDependency,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "skill-lock-mutable-name-denied",
        threat: AppThreatActor::MutableSkillDependency,
        boundary: AppTrustZone::GovernedSkillRuntime,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "derived-content-label-join-narrows",
        threat: AppThreatActor::StoredOrExternalPromptInjection,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "forged-envelope-handling-label-rejected",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "prompt-injection-cannot-expand-dispatch",
        threat: AppThreatActor::StoredOrExternalPromptInjection,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "invented-tool-name-cannot-grant-authority",
        threat: AppThreatActor::ModelOutput,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "caller-supplied-scope-is-not-authentication",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "forged-client-cross-scope-route-concealed",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::AuthenticatedSessionAdapter,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "provider-name-is-not-locality",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "provider-dispatch-revalidates-endpoint-attestation",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "loopback-fallback-requires-actual-loopback-peer",
        threat: AppThreatActor::SameUserLocalProcess,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "loopback-route-still-resolves-current-authority",
        threat: AppThreatActor::SameUserLocalProcess,
        boundary: AppTrustZone::AuthenticatedSessionAdapter,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "lifecycle-reducer-rejects-invalid-replay",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "store-commit-recovery-is-idempotent",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::ScopedAppStore,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "scoped-store-cross-installation-call-denied",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::ScopedAppStore,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "scoped-store-stale-schema-call-denied",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::ScopedAppStore,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "stale-projected-authority-denied-at-dispatch",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "revoked-or-unavailable-authority-cannot-mint-boundary-evidence",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "delegated-parent-ceiling-narrows-dispatch",
        threat: AppThreatActor::ModelOutput,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "stale-replayed-cross-scope-or-session-install-approval-denied",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::AuthenticatedSessionAdapter,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "memory-candidate-requires-current-source-fence",
        threat: AppThreatActor::StoredOrExternalPromptInjection,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "memory-source-lifecycle-fails-closed",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "memory-retrieval-rechecks-current-source-fence",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::GovernedMemoryStore,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "memory-purge-reconciles-index-and-prompt-copies",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::GovernedMemoryStore,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "custom-surface-bridge-cannot-forge-authority",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "custom-surface-asset-cannot-reach-host-origin",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-bridge-op-forgery-and-replay-fails-closed",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-cross-installation-frame-isolation",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-navigation-and-redirect-escape-denied",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-resource-exhaustion-bounded",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-web-host-resource-residual-accepted",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-stale-asset-cache-cannot-serve-dead-revision",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-injected-content-display-cannot-escalate",
        threat: AppThreatActor::StoredOrExternalPromptInjection,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-engine-escape-blast-radius-bounded",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-devtools-vector-requires-local-authority",
        threat: AppThreatActor::SameUserLocalProcess,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "custom-surface-network-exfiltration-channel-absent",
        threat: AppThreatActor::CustomSurfaceDocument,
        boundary: AppTrustZone::SandboxedCustomSurface,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "hidden-diagnostics-are-metadata-only",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::MetadataOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "continuation-reuse-needs-exact-policy-identity",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "provider-capture-adapter-suppresses-ineligible-content",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::RetainedThirdPartyProvider,
        expectation: AppRedCaseExpectation::MetadataOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "provider-continuation-adapter-rejects-mismatched-history",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::RetainedThirdPartyProvider,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "portable-package-and-data-shapes-exclude-authority",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "secret-plaintext-archive-denied",
        threat: AppThreatActor::ForgedClient,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "package-digest-does-not-prove-publisher-lineage",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "unsigned-package-import-receives-local-fork-identity",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "purge-inventory-and-receipt-settle-every-store-class",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "provider-history-is-never-claimed-locally-deleted",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::MetadataOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "archive-writer-authenticates-every-encrypted-chunk",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::GovernedArchiveWriter,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "storage-governance-proves-purge-multi-store-outcomes",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::StorageGovernance,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "resource-fragmented-meters-cannot-authorize-tree-spend",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "resource-overrun-and-parallel-time-settle-exactly-once",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "resource-expired-uncertain-effect-remains-reserved",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "resource-replay-does-not-readmit-historical-work",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::NoAuthorityChange,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "resource-period-revision-cannot-regress",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::ContractSpecified,
    },
    AppRedCase {
        id: "resource-dispatch-reserves-before-consequential-effect",
        threat: AppThreatActor::ModelOutput,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "resource-durable-authority-settles-tree-and-installation-period",
        threat: AppThreatActor::CrashReplayRecovery,
        boundary: AppTrustZone::CanonicalResourceAuthority,
        expectation: AppRedCaseExpectation::ExactIdentityOnly,
        status: AppRedCaseStatus::LoadBearingAdapterPending,
    },
    AppRedCase {
        id: "overlay-draw-payload-outside-reviewed-recipe-vocabulary-rejected",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::UntrustedPackage,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "overlay-draw-returns-receipt-never-screen-observation",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::UntrustedPackage,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "overlay-draw-stays-non-dispatchable-until-reviewed-consumer-lands",
        threat: AppThreatActor::StoredOrExternalPromptInjection,
        boundary: AppTrustZone::ConsequentialDispatch,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "narration-input-caps-and-run-budget-fail-closed",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::UntrustedPackage,
        expectation: AppRedCaseExpectation::BoundedRejection,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "narration-carries-no-provider-voice-selection-authority",
        threat: AppThreatActor::ProviderOrEndpoint,
        boundary: AppTrustZone::UntrustedPackage,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::DormantBoundarySpecified,
    },
    AppRedCase {
        id: "app-voice-invocation-phrases-stay-unadmitted",
        threat: AppThreatActor::PackageOrWorkflowAuthor,
        boundary: AppTrustZone::TrustedServerKernel,
        expectation: AppRedCaseExpectation::FailClosed,
        status: AppRedCaseStatus::ContractSpecified,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn custom_surface_v1_rows_carry_their_kernel_tests_with_them() {
        // The plan-1.6 rows exist in this table; their test coverage is
        // honest and split: T1-T4 and T8 carry kernel tests in
        // `surface_scripted_host` (isolation constants, method set,
        // session binding, digest addressing) plus the budget tests for
        // T6; T5 is host-side only (iOS host tests); T7/T9/T10/T11 are
        // structural or ratified-residual rows with no dedicated test.
        let ids = APP_PHASE0_RED_CASES
            .iter()
            .map(|case| case.id)
            .collect::<BTreeSet<_>>();
        for id in [
            "custom-surface-asset-cannot-reach-host-origin",
            "custom-surface-bridge-cannot-forge-authority",
            "custom-surface-bridge-op-forgery-and-replay-fails-closed",
            "custom-surface-cross-installation-frame-isolation",
            "custom-surface-navigation-and-redirect-escape-denied",
            "custom-surface-resource-exhaustion-bounded",
            "custom-surface-web-host-resource-residual-accepted",
            "custom-surface-stale-asset-cache-cannot-serve-dead-revision",
            "custom-surface-injected-content-display-cannot-escalate",
            "custom-surface-engine-escape-blast-radius-bounded",
            "custom-surface-devtools-vector-requires-local-authority",
            "custom-surface-network-exfiltration-channel-absent",
        ] {
            assert!(ids.contains(id), "{id}");
        }
    }

    #[test]
    fn every_phase0_threat_has_a_unique_named_red_case() {
        let ids = APP_PHASE0_RED_CASES
            .iter()
            .map(|case| case.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), APP_PHASE0_RED_CASES.len());
        for threat in AppThreatActor::ALL {
            assert!(
                APP_PHASE0_RED_CASES
                    .iter()
                    .any(|case| case.threat == threat),
                "missing red case for {threat:?}"
            );
        }
    }

    #[test]
    fn load_bearing_zones_are_not_misreported_as_closed_by_kernel_tests() {
        for case in APP_PHASE0_RED_CASES {
            if case.status == AppRedCaseStatus::ContractSpecified {
                assert_eq!(case.boundary, AppTrustZone::TrustedServerKernel);
            }
        }
    }

    #[test]
    fn phase0_dormant_boundary_gate_has_no_projection_only_or_pending_rows() {
        const REQUIRED: &[&str] = &[
            "manifest-untrusted-input-bounded",
            "skill-lock-mutable-name-denied",
            "prompt-injection-cannot-expand-dispatch",
            "invented-tool-name-cannot-grant-authority",
            "forged-client-cross-scope-route-concealed",
            "loopback-route-still-resolves-current-authority",
            "scoped-store-cross-installation-call-denied",
            "scoped-store-stale-schema-call-denied",
            "stale-projected-authority-denied-at-dispatch",
            "revoked-or-unavailable-authority-cannot-mint-boundary-evidence",
            "delegated-parent-ceiling-narrows-dispatch",
            "stale-replayed-cross-scope-or-session-install-approval-denied",
        ];
        for id in REQUIRED {
            let case = APP_PHASE0_RED_CASES
                .iter()
                .find(|case| case.id == *id)
                .unwrap_or_else(|| panic!("missing Phase-0 boundary case `{id}`"));
            assert_eq!(case.status, AppRedCaseStatus::DormantBoundarySpecified);
        }
    }

    #[test]
    fn plan_1_3_experience_rows_stay_honest_until_a_consumer_lands() {
        // The two admitted experience classes (overlay-draw, narration) are
        // admission-only kernels: their rows must stay
        // DormantBoundarySpecified — flipping one to a load-bearing status
        // belongs to the reviewed consumer commit, never to this matrix.
        for id in [
            "overlay-draw-payload-outside-reviewed-recipe-vocabulary-rejected",
            "overlay-draw-returns-receipt-never-screen-observation",
            "overlay-draw-stays-non-dispatchable-until-reviewed-consumer-lands",
            "narration-input-caps-and-run-budget-fail-closed",
            "narration-carries-no-provider-voice-selection-authority",
        ] {
            let case = APP_PHASE0_RED_CASES
                .iter()
                .find(|case| case.id == id)
                .unwrap_or_else(|| panic!("missing plan-1.3 experience red case `{id}`"));
            assert_eq!(
                case.status,
                AppRedCaseStatus::DormantBoundarySpecified,
                "experience row `{id}`"
            );
        }
        // The R2 voice-invocation stop is the one closed kernel fact of the
        // three: no app wake-phrase admission exists, and the fail-closed
        // default is pinned by the kernel tests in the experience admission
        // module (`magician/src/magician_v2/apps/experience_capability.rs`).
        let stopped = APP_PHASE0_RED_CASES
            .iter()
            .find(|case| case.id == "app-voice-invocation-phrases-stay-unadmitted")
            .expect("voice-invocation stop row");
        assert_eq!(stopped.status, AppRedCaseStatus::ContractSpecified);
        assert_eq!(stopped.expectation, AppRedCaseExpectation::FailClosed);
        assert_eq!(stopped.boundary, AppTrustZone::TrustedServerKernel);
    }
}
