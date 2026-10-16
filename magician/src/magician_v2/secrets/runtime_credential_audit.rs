//! Dormant product audit adapter for generic-runtime credential receipts.
//!
//! A receipt is already metadata-only when it reaches this module. The adapter first
//! appends a structured record to the existing scope-owned secret audit journal and
//! only then emits the corresponding schemaless analytics event. No production route
//! invokes this adapter before the universal runtime migration gate opens. Redacted
//! output records remain caller-owned; this adapter deliberately does not publish them
//! to artifacts, logs, traces, analytics payloads, or any other product surface.

#![allow(
    dead_code,
    reason = "Phase 3D adapter remains dormant until governed execution integration"
)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tool_runtime_core::{
    credential_injection::{
        CredentialExecutionFailure, CredentialExecutionOutcome, CredentialInjectionReceipt,
    },
    credential_persistence::{
        persist_credential_audit_receipt, CredentialAuditSink, CredentialPersistenceBatch,
        CredentialPersistenceError,
    },
    credential_profiles::CredentialProfileBinding,
    manifest::AuthKind,
};

use crate::magician_v2::analytics::{self, event_sink::AnalyticsEvent};

use super::store::{SecretAuditEvent, SecretStoreError, SecretStoreResolver};

pub const RUNTIME_CREDENTIAL_AUDIT_EVENT: &str = "tool_runtime_credential_injection";

/// Product persistence projection of the provider-neutral receipt. Every string is
/// derived from a validated identifier or a finite enum; values, references, paths,
/// argv, output, and provider diagnostics have no field in this type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCredentialAuditReceipt {
    pub persistence_schema_version: String,
    pub schema_version: String,
    pub call_id: String,
    pub principal: String,
    pub workspace: String,
    pub auth_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_profile_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_profile_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_profile_binding: Option<CredentialProfileBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit_profile_binding: Option<CredentialProfileBinding>,
    pub prepared_binding_count: usize,
    pub injection_count: usize,
    pub inherited_environment_name_count: usize,
    pub environment_target_count: usize,
    pub stdin_target_count: usize,
    pub scoped_file_target_count: usize,
    pub config_directory_target_count: usize,
    pub redacted_record_count: usize,
    pub redacted_total_bytes: usize,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl RuntimeCredentialAuditReceipt {
    fn from_receipt(
        receipt: &CredentialInjectionReceipt,
        persistence_schema_version: &str,
        redacted_record_count: usize,
        redacted_total_bytes: usize,
    ) -> Self {
        let (outcome, failure) = outcome_parts(receipt.outcome());
        let selected_profile = receipt.selected_profile();
        let implicit_profile = receipt.implicit_profile();
        let target_counts = receipt.target_counts();
        Self {
            persistence_schema_version: persistence_schema_version.to_owned(),
            schema_version: receipt.schema_version().to_owned(),
            call_id: receipt.call_id().as_str().to_owned(),
            principal: receipt.scope().principal.as_str().to_owned(),
            workspace: receipt.scope().workspace.as_str().to_owned(),
            auth_kind: auth_kind_name(receipt.auth_kind()).to_owned(),
            selected_provider: selected_profile.map(|profile| profile.provider.as_str().to_owned()),
            selected_profile_alias: selected_profile
                .map(|profile| profile.alias.as_str().to_owned()),
            selected_profile_revision: receipt
                .selected_profile_revision()
                .map(|revision| revision.get()),
            selected_profile_binding: selected_profile.map(|profile| profile.binding.clone()),
            implicit_provider: implicit_profile.map(|(provider, _)| provider.to_owned()),
            implicit_profile_binding: implicit_profile.map(|(_, binding)| binding.clone()),
            prepared_binding_count: receipt.prepared_binding_count(),
            injection_count: receipt.injection_count(),
            inherited_environment_name_count: receipt.inherited_environment_name_count(),
            environment_target_count: target_counts.environment,
            stdin_target_count: target_counts.stdin,
            scoped_file_target_count: target_counts.scoped_file,
            config_directory_target_count: target_counts.config_directory,
            redacted_record_count,
            redacted_total_bytes,
            outcome: outcome.to_owned(),
            failure: failure.map(str::to_owned),
        }
    }
}

struct SecretStoreCredentialAuditSink<'a> {
    resolver: &'a SecretStoreResolver,
    occurred_at: DateTime<Utc>,
    persistence_schema_version: &'static str,
    redacted_record_count: usize,
    redacted_total_bytes: usize,
}

impl CredentialAuditSink for SecretStoreCredentialAuditSink<'_> {
    type Error = SecretStoreError;

    fn persist(&mut self, receipt: &CredentialInjectionReceipt) -> Result<(), Self::Error> {
        let store = self.resolver.resolve_for_scope(
            receipt.scope().principal.as_str(),
            receipt.scope().workspace.as_str(),
        )?;
        let projected = RuntimeCredentialAuditReceipt::from_receipt(
            receipt,
            self.persistence_schema_version,
            self.redacted_record_count,
            self.redacted_total_bytes,
        );
        let event =
            SecretAuditEvent::new_at(RUNTIME_CREDENTIAL_AUDIT_EVENT, self.occurred_at.timestamp())
                .with_runtime_credential_receipt(projected.clone());

        // Audit is the fail-closed boundary. The best-effort analytics copy is emitted
        // only after the append succeeds, so it can never claim an unaudited call.
        store.try_audit_event(event)?;
        analytics::emit(analytics_event(self.occurred_at, projected));
        Ok(())
    }
}

/// Persist only the metadata receipt and redacted-record counts for `batch`.
/// The batch records themselves remain with the caller for destination-specific
/// persistence after each later migration boundary is implemented.
pub fn audit_runtime_credential_batch(
    resolver: &SecretStoreResolver,
    batch: &CredentialPersistenceBatch,
) -> Result<(), CredentialPersistenceError> {
    let mut sink = SecretStoreCredentialAuditSink {
        resolver,
        occurred_at: Utc::now(),
        persistence_schema_version: batch.schema_version(),
        redacted_record_count: batch.records().len(),
        redacted_total_bytes: batch.total_bytes(),
    };
    persist_credential_audit_receipt(&mut sink, batch.receipt())
}

#[cfg(any(test, feature = "test-fixtures"))]
fn persist_runtime_credential_receipt_for_test(
    resolver: &SecretStoreResolver,
    receipt: &CredentialInjectionReceipt,
) -> Result<(), CredentialPersistenceError> {
    let mut sink = SecretStoreCredentialAuditSink {
        resolver,
        occurred_at: Utc::now(),
        persistence_schema_version:
            tool_runtime_core::credential_persistence::CREDENTIAL_PERSISTENCE_V1,
        redacted_record_count: 0,
        redacted_total_bytes: 0,
    };
    persist_credential_audit_receipt(&mut sink, receipt)
}

fn analytics_event(
    occurred_at: DateTime<Utc>,
    receipt: RuntimeCredentialAuditReceipt,
) -> AnalyticsEvent {
    AnalyticsEvent {
        timestamp: occurred_at,
        event_type: RUNTIME_CREDENTIAL_AUDIT_EVENT.to_owned(),
        source: receipt.call_id.clone(),
        principal: Some(receipt.principal.clone()),
        workspace: Some(receipt.workspace.clone()),
        payload: serde_json::json!({
            "receipt": receipt,
        }),
    }
}

const fn auth_kind_name(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::None => "none",
        AuthKind::Secrets => "secrets",
        AuthKind::CliProfile => "cli_profile",
        AuthKind::OAuthSession => "oauth_session",
        AuthKind::BrowserProfile => "browser_profile",
        AuthKind::NativePermission => "native_permission",
        AuthKind::DelegatedCredential => "delegated_credential",
    }
}

const fn outcome_parts(
    outcome: CredentialExecutionOutcome,
) -> (&'static str, Option<&'static str>) {
    match outcome {
        CredentialExecutionOutcome::Succeeded => ("succeeded", None),
        CredentialExecutionOutcome::Cancelled => ("cancelled", None),
        CredentialExecutionOutcome::Failed { failure } => {
            ("failed", Some(execution_failure_name(failure)))
        },
    }
}

const fn execution_failure_name(failure: CredentialExecutionFailure) -> &'static str {
    match failure {
        CredentialExecutionFailure::ResolutionFailed => "resolution_failed",
        CredentialExecutionFailure::AuthorizationDenied => "authorization_denied",
        CredentialExecutionFailure::ApprovalDenied => "approval_denied",
        CredentialExecutionFailure::EnvironmentUnavailable => "environment_unavailable",
        CredentialExecutionFailure::StdinUnavailable => "stdin_unavailable",
        CredentialExecutionFailure::ScopedPathUnavailable => "scoped_path_unavailable",
        CredentialExecutionFailure::MaterializationFailed => "materialization_failed",
        CredentialExecutionFailure::ProcessCancelled => "process_cancelled",
        CredentialExecutionFailure::ProcessTimedOut => "process_timed_out",
        CredentialExecutionFailure::ProcessMemoryExceeded => "process_memory_exceeded",
        CredentialExecutionFailure::ProcessExitedNonZero => "process_exited_non_zero",
        CredentialExecutionFailure::OutputMalformed => "output_malformed",
        CredentialExecutionFailure::OutputTruncated => "output_truncated",
        CredentialExecutionFailure::RedactionFailed => "redaction_failed",
        CredentialExecutionFailure::CleanupFailed => "cleanup_failed",
        CredentialExecutionFailure::AuditFailed => "audit_failed",
        CredentialExecutionFailure::Internal => "internal",
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use tool_runtime_core::{
        credential_injection::{
            ChildEnvironmentBaseline, CredentialCallId, CredentialInjectionPlan,
            CredentialInjectionReceipt,
        },
        credential_persistence::CredentialPersistenceErrorCode,
        credential_preparation::CredentialPreparationPlan,
        credential_profiles::{
            CanonicalCredentialUrl, CreateCredentialProfileReference,
            CredentialProfileAvailability, CredentialProfileBinding, CredentialProfileError,
            CredentialProfileKey, CredentialProfileMetadata, CredentialProfileRegistry,
            CredentialProfileRegistrySnapshot, CredentialProfileRevision, CredentialProfileStatus,
            CredentialScope, SetCredentialProfileDisabled, UpdateCredentialProfileMetadata,
        },
        manifest::{
            AuthContract, AuthRequirement, AuthState, CliInteraction, McpDiscoveryPolicy,
            McpOAuthConnectionPolicy, McpTransport, PolicyFloor, ProfileSelection, RuntimeLimits,
            RuntimeProtocol, RuntimeRequirements, SkillRuntimeContract,
            SkillRuntimeContractVersion, StdinContract, WorkingDirectoryContract,
        },
        manifest_validation::validate_skill_runtime_contract,
        profile_selection::{select_credential_profile, CredentialProfileSelectionRequest},
    };

    use crate::magician_v2::secrets::{
        InMemoryKeyProvider, SecretRuntimeCapabilities, SecretStoreResolver,
    };

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct SnapshotRegistry {
        snapshot: CredentialProfileRegistrySnapshot,
    }

    impl CredentialProfileRegistry for SnapshotRegistry {
        fn snapshot(
            &self,
            scope: &CredentialScope,
        ) -> Result<CredentialProfileRegistrySnapshot, CredentialProfileError> {
            if self.snapshot.scope() != scope {
                return Err(CredentialProfileError::registry_unavailable());
            }
            Ok(self.snapshot.clone())
        }

        fn status(
            &self,
            key: &CredentialProfileKey,
        ) -> Result<Option<CredentialProfileStatus>, CredentialProfileError> {
            Ok(self
                .snapshot
                .profiles()
                .iter()
                .find(|profile| profile.key() == key)
                .cloned())
        }

        fn create_reference(
            &self,
            _request: CreateCredentialProfileReference,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }

        fn update_metadata(
            &self,
            _request: UpdateCredentialProfileMetadata,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }

        fn set_disabled(
            &self,
            _request: SetCredentialProfileDisabled,
        ) -> Result<CredentialProfileStatus, CredentialProfileError> {
            Err(CredentialProfileError::registry_unavailable())
        }
    }

    fn select_from_snapshot(
        request: &CredentialProfileSelectionRequest,
        snapshot: CredentialProfileRegistrySnapshot,
    ) -> tool_runtime_core::profile_selection::CredentialProfileSelectionDecision {
        select_credential_profile(&SnapshotRegistry { snapshot }, request).expect("selection")
    }

    fn scoped_receipt(
        call_id: &str,
        principal: &str,
        workspace: &str,
        outcome: CredentialExecutionOutcome,
    ) -> CredentialInjectionReceipt {
        let scope = CredentialScope::new(principal, workspace).expect("scope");
        let contract = SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements {
                bins: ["fixture-cli".to_owned()].into_iter().collect(),
                entrypoint: Default::default(),
                environment: Default::default(),
            },
            runtime: RuntimeProtocol::Cli {
                command_prefix: Vec::new(),
                interaction: CliInteraction::Batch,
                stdin: StdinContract::default(),
                working_directory: WorkingDirectoryContract::default(),
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract::default(),
            policy_floor: PolicyFloor::default(),
        };
        let validated = validate_skill_runtime_contract(&contract).expect("validated contract");
        let request = CredentialProfileSelectionRequest::new(
            scope.clone(),
            None,
            CredentialProfileBinding::Provider,
            &ProfileSelection::None,
            None,
        )
        .expect("selection request");
        let snapshot =
            CredentialProfileRegistrySnapshot::new(scope.clone(), Vec::new()).expect("snapshot");
        let selection = select_from_snapshot(&request, snapshot);
        let preparation =
            CredentialPreparationPlan::new(scope, AuthKind::None, &selection, Vec::new())
                .expect("preparation");
        let injection = CredentialInjectionPlan::compile(
            validated,
            &preparation,
            ChildEnvironmentBaseline::hermetic(),
        )
        .expect("injection");
        CredentialInjectionReceipt::new(
            CredentialCallId::new(call_id).expect("call id"),
            &injection,
            outcome,
        )
    }

    fn receipt(outcome: CredentialExecutionOutcome) -> CredentialInjectionReceipt {
        scoped_receipt("call-audit-fixture", "owner", "default", outcome)
    }

    fn mcp_binding() -> CredentialProfileBinding {
        CredentialProfileBinding::McpOauth {
            resource_url: CanonicalCredentialUrl::new("https://provider.example/mcp")
                .expect("resource"),
            authorization_issuer: CanonicalCredentialUrl::new("https://issuer.example/tenant")
                .expect("issuer"),
        }
    }

    fn mcp_contract(profile_selection: ProfileSelection) -> SkillRuntimeContract {
        SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements::default(),
            runtime: RuntimeProtocol::Mcp {
                transport: McpTransport::StreamableHttp {
                    endpoint: "https://provider.example/mcp".to_owned(),
                },
                discovery: McpDiscoveryPolicy {
                    oauth: Some(McpOAuthConnectionPolicy {
                        authorization_issuer: "https://issuer.example/tenant".to_owned(),
                        scopes: BTreeSet::new(),
                    }),
                    ..McpDiscoveryPolicy::default()
                },
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract {
                kind: AuthKind::OAuthSession,
                requirement: AuthRequirement::Required,
                provider: Some("provider-mcp".to_owned()),
                profile_selection,
                ..AuthContract::default()
            },
            policy_floor: PolicyFloor::default(),
        }
    }

    fn selected_mcp_receipt() -> CredentialInjectionReceipt {
        let scope = CredentialScope::new("owner", "default").expect("scope");
        let binding = mcp_binding();
        let key = CredentialProfileKey::new(scope.clone(), "provider-mcp", "work", binding)
            .expect("profile key");
        let metadata = CredentialProfileMetadata::new(
            key,
            None,
            true,
            CredentialProfileAvailability::Enabled,
            CredentialProfileRevision::new(9).expect("revision"),
        )
        .expect("metadata");
        let status = CredentialProfileStatus::new(metadata, AuthState::Ready).expect("status");
        let snapshot =
            CredentialProfileRegistrySnapshot::new(scope.clone(), vec![status]).expect("snapshot");
        let selection_policy = ProfileSelection::Fixed {
            alias: "work".to_owned(),
        };
        let request = CredentialProfileSelectionRequest::new(
            scope.clone(),
            Some("provider-mcp"),
            mcp_binding(),
            &selection_policy,
            None,
        )
        .expect("selection request");
        let selection = select_from_snapshot(&request, snapshot);
        let preparation =
            CredentialPreparationPlan::new(scope, AuthKind::OAuthSession, &selection, Vec::new())
                .expect("preparation");
        let contract = mcp_contract(selection_policy);
        let injection = CredentialInjectionPlan::compile(
            validate_skill_runtime_contract(&contract).expect("validated contract"),
            &preparation,
            ChildEnvironmentBaseline::hermetic(),
        )
        .expect("injection");
        CredentialInjectionReceipt::new(
            CredentialCallId::new("call-mcp-audit").expect("call id"),
            &injection,
            CredentialExecutionOutcome::Succeeded,
        )
    }

    fn implicit_mcp_receipt() -> CredentialInjectionReceipt {
        let scope = CredentialScope::new("owner", "default").expect("scope");
        let selection_policy = ProfileSelection::Implicit;
        let request = CredentialProfileSelectionRequest::new(
            scope.clone(),
            Some("provider-mcp"),
            mcp_binding(),
            &selection_policy,
            None,
        )
        .expect("selection request");
        let snapshot =
            CredentialProfileRegistrySnapshot::new(scope.clone(), Vec::new()).expect("snapshot");
        let selection = select_from_snapshot(&request, snapshot);
        let preparation =
            CredentialPreparationPlan::new(scope, AuthKind::OAuthSession, &selection, Vec::new())
                .expect("preparation");
        let contract = mcp_contract(selection_policy);
        let injection = CredentialInjectionPlan::compile(
            validate_skill_runtime_contract(&contract).expect("validated contract"),
            &preparation,
            ChildEnvironmentBaseline::hermetic(),
        )
        .expect("injection");
        CredentialInjectionReceipt::new(
            CredentialCallId::new("call-mcp-implicit-audit").expect("call id"),
            &injection,
            CredentialExecutionOutcome::Succeeded,
        )
    }

    fn fixture_path(label: &str) -> std::path::PathBuf {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "runtime-credential-audit-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    fn resolver(root: std::path::PathBuf) -> SecretStoreResolver {
        SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            root,
            SecretRuntimeCapabilities::fully_available("in_memory"),
        )
    }

    #[test]
    fn receipt_appends_structured_audit_and_projects_matching_analytics() {
        let root = fixture_path("success");
        let resolver = resolver(root.clone());
        let receipt = receipt(CredentialExecutionOutcome::Failed {
            failure: CredentialExecutionFailure::OutputMalformed,
        });

        persist_runtime_credential_receipt_for_test(&resolver, &receipt).expect("persist receipt");
        let audit_root = resolver.workspace_layout().secrets_root("owner", "default");
        let audit = fs::read_to_string(audit_root.join(super::super::store::SECRET_AUDIT_FILENAME))
            .expect("audit journal");
        let event: SecretAuditEvent = serde_json::from_str(audit.trim()).expect("audit event");
        let projected = event
            .runtime_credential_receipt()
            .expect("credential receipt");
        assert_eq!(projected.call_id, "call-audit-fixture");
        assert_eq!(projected.failure.as_deref(), Some("output_malformed"));
        assert_eq!(projected.redacted_record_count, 0);

        let analytics = analytics_event(Utc::now(), projected.clone());
        assert_eq!(analytics.event_type, RUNTIME_CREDENTIAL_AUDIT_EVENT);
        assert_eq!(analytics.source, "call-audit-fixture");
        assert_eq!(analytics.principal.as_deref(), Some("owner"));
        assert_eq!(analytics.payload["receipt"]["prepared_binding_count"], 0);
        let serialized = serde_json::to_string(&(event, analytics.payload)).expect("serialize");
        assert!(!serialized.contains("credential-value-canary"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_preserves_selected_profile_revision_and_full_mcp_binding() {
        let receipt = selected_mcp_receipt();
        let projected = RuntimeCredentialAuditReceipt::from_receipt(
            &receipt,
            tool_runtime_core::credential_persistence::CREDENTIAL_PERSISTENCE_V1,
            0,
            0,
        );

        assert_eq!(projected.selected_provider.as_deref(), Some("provider-mcp"));
        assert_eq!(projected.selected_profile_alias.as_deref(), Some("work"));
        assert_eq!(projected.selected_profile_revision, Some(9));
        assert_eq!(
            projected.selected_profile_binding,
            Some(CredentialProfileBinding::McpOauth {
                resource_url: CanonicalCredentialUrl::new("https://provider.example/mcp")
                    .expect("resource"),
                authorization_issuer: CanonicalCredentialUrl::new("https://issuer.example/tenant",)
                    .expect("issuer"),
            })
        );
    }

    #[test]
    fn projection_preserves_implicit_provider_and_full_mcp_binding() {
        let receipt = implicit_mcp_receipt();
        let projected = RuntimeCredentialAuditReceipt::from_receipt(
            &receipt,
            tool_runtime_core::credential_persistence::CREDENTIAL_PERSISTENCE_V1,
            0,
            0,
        );

        assert!(projected.selected_provider.is_none());
        assert!(projected.selected_profile_alias.is_none());
        assert!(projected.selected_profile_revision.is_none());
        assert!(projected.selected_profile_binding.is_none());
        assert_eq!(projected.implicit_provider.as_deref(), Some("provider-mcp"));
        assert_eq!(projected.implicit_profile_binding, Some(mcp_binding()));
    }

    #[test]
    fn older_selected_profile_audit_json_remains_readable() {
        let projected = RuntimeCredentialAuditReceipt::from_receipt(
            &selected_mcp_receipt(),
            tool_runtime_core::credential_persistence::CREDENTIAL_PERSISTENCE_V1,
            0,
            0,
        );
        let mut legacy = serde_json::to_value(projected).expect("audit json");
        let object = legacy.as_object_mut().expect("audit object");
        object.remove("selected_profile_revision");
        object.remove("selected_profile_binding");

        let restored: RuntimeCredentialAuditReceipt =
            serde_json::from_value(legacy).expect("legacy audit receipt");
        assert_eq!(restored.selected_provider.as_deref(), Some("provider-mcp"));
        assert_eq!(restored.selected_profile_alias.as_deref(), Some("work"));
        assert!(restored.selected_profile_revision.is_none());
        assert!(restored.selected_profile_binding.is_none());
    }

    #[test]
    fn native_audit_errors_collapse_without_paths_or_values() {
        let root = fixture_path("credential-value-canary");
        let resolver = resolver(root.clone());
        let blocked = resolver.workspace_layout().secrets_root("owner", "default");
        fs::create_dir_all(blocked.parent().expect("secrets parent")).expect("scope root");
        fs::write(&blocked, b"not-a-directory").expect("blocking file");

        let error = persist_runtime_credential_receipt_for_test(
            &resolver,
            &receipt(CredentialExecutionOutcome::Succeeded),
        )
        .expect_err("audit failure");
        assert_eq!(error.code, CredentialPersistenceErrorCode::AuditFailed);
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains("credential-value-canary"));
        assert!(!diagnostic.contains(blocked.to_string_lossy().as_ref()));
        let _ = fs::remove_file(blocked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_keeps_receipts_in_their_exact_scope() {
        let root = fixture_path("scope-isolation");
        let resolver = resolver(root.clone());
        let first = scoped_receipt(
            "call-owner",
            "owner",
            "default",
            CredentialExecutionOutcome::Succeeded,
        );
        let second = scoped_receipt(
            "call-other",
            "other",
            "work",
            CredentialExecutionOutcome::Succeeded,
        );

        persist_runtime_credential_receipt_for_test(&resolver, &first).expect("owner receipt");
        persist_runtime_credential_receipt_for_test(&resolver, &second).expect("other receipt");

        for (principal, workspace, expected_call) in [
            ("owner", "default", "call-owner"),
            ("other", "work", "call-other"),
        ] {
            let path = resolver
                .workspace_layout()
                .secrets_root(principal, workspace)
                .join(super::super::store::SECRET_AUDIT_FILENAME);
            let event: SecretAuditEvent =
                serde_json::from_str(fs::read_to_string(path).expect("scoped journal").trim())
                    .expect("scoped event");
            let projected = event.runtime_credential_receipt().expect("runtime receipt");
            assert_eq!(projected.call_id, expected_call);
            assert_eq!(projected.principal, principal);
            assert_eq!(projected.workspace, workspace);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_receipts_remain_complete_json_lines() {
        let root = fixture_path("concurrency");
        let resolver = resolver(root.clone());
        let mut workers = Vec::new();
        for index in 0..32usize {
            let resolver = resolver.clone();
            workers.push(std::thread::spawn(move || {
                let call_id = format!("call-audit-{index:02}");
                let receipt = scoped_receipt(
                    &call_id,
                    "owner",
                    "default",
                    CredentialExecutionOutcome::Succeeded,
                );
                persist_runtime_credential_receipt_for_test(&resolver, &receipt)
            }));
        }
        for worker in workers {
            worker
                .join()
                .expect("audit worker")
                .expect("persist concurrent receipt");
        }

        let path = resolver
            .workspace_layout()
            .secrets_root("owner", "default")
            .join(super::super::store::SECRET_AUDIT_FILENAME);
        let journal = fs::read_to_string(path).expect("concurrent journal");
        let mut call_ids = journal
            .lines()
            .map(|line| {
                serde_json::from_str::<SecretAuditEvent>(line)
                    .expect("complete JSON line")
                    .runtime_credential_receipt()
                    .expect("runtime receipt")
                    .call_id
                    .clone()
            })
            .collect::<Vec<_>>();
        call_ids.sort();
        call_ids.dedup();
        assert_eq!(call_ids.len(), 32);
        let _ = fs::remove_dir_all(root);
    }
}
