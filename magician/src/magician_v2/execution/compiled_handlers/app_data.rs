//! Shared authority minting for personal-agent app-data compiled tools.

use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::warn;

use crate::magician_v2::apps::authority::AuthenticatedAppScope;
use crate::magician_v2::apps::boundary::{
    AppDirectOwnerExecutionEvidence, AppPersonalAgentPublicationFence,
    AppPersonalAgentReadAuthority, AppStoreReadAudience,
};
use crate::magician_v2::apps::processing_boundary::{
    current_personal_agent_provider_grant,
    reattest_personal_agent_publication as reattest_from_config,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::apps::boundary::AppAgentProcessingClass;
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::apps::records::AppDisclosureProviderClass;

pub struct PersonalAgentToolContext {
    pub authenticated: AuthenticatedAppScope,
    pub authority: AppPersonalAgentReadAuthority,
    /// Non-serializable proof retained across asynchronous tool work. Callers
    /// must re-attest it immediately before publishing bytes or crossing an
    /// effect boundary; the read authority's admission-time sample is not a
    /// substitute for that final check.
    pub(crate) publication_fence: AppPersonalAgentPublicationFence,
    pub(crate) calling_profile_name: String,
    /// Exact target agent from the typed invocation carrier. Model arguments
    /// cannot replace this identity when selecting a memory tier or another
    /// agent-scoped consequence.
    pub target_agent_id: String,
}

pub fn require_direct_personal_agent_context(
    resources: Option<&AgentResources>,
    _args: &Value,
    tool_name: &str,
) -> Result<PersonalAgentToolContext, ExecutionError> {
    let invocation =
        crate::magician_v2::execution::compiled_dispatch::current_compiled_invocation_context()
            .ok_or_else(|| {
                ExecutionError::Step(format!(
                    "{tool_name} requires server-authenticated invocation provenance"
                ))
            })?;
    let now = Utc::now();
    let owner_credential = crate::magician_v2::execution::compiled_dispatch::current_compiled_app_owner_execution_credential()
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{tool_name} requires a current authenticated owner-chat credential"
            ))
        })?;
    let bound_owner = owner_credential
        .bind_guard_preserving_chat_inline(&invocation, now.to_owned())
        .map_err(|_| {
            ExecutionError::Step(format!(
                "{tool_name} requires a current authenticated owner-chat credential"
            ))
        })?;
    let (authenticated, execution_ref) = bound_owner.into_parts();
    let profile_name =
        crate::magician_v2::execution::compiled_dispatch::current_compiled_calling_profile_name()
            .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{tool_name} requires an attested calling-model profile"
            ))
        })?;
    let resources = resources.ok_or_else(|| {
        ExecutionError::Step(format!(
            "{tool_name} calling-model trust registry is unavailable"
        ))
    })?;
    let config = resources.magician_config_snapshot();
    let grant =
        current_personal_agent_provider_grant(&config, &profile_name, now).map_err(|error| {
            warn!(
                error = ?error,
                tool_name,
                "personal-agent provider grant admission failed"
            );
            personal_agent_authority_unavailable(tool_name)
        })?;
    let evidence = AppDirectOwnerExecutionEvidence::from_resolved_execution(
        &authenticated,
        &invocation,
        execution_ref,
        grant,
        now,
    )
    .map_err(|error| {
        warn!(
            error = ?error,
            tool_name,
            "personal-agent execution-evidence admission failed"
        );
        personal_agent_authority_unavailable(tool_name)
    })?;
    let authority =
        AppPersonalAgentReadAuthority::from_current_execution(&authenticated, evidence, now)
            .map_err(|error| {
                warn!(
                    error = ?error,
                    tool_name,
                    "personal-agent read authority admission failed"
                );
                personal_agent_authority_unavailable(tool_name)
            })?;
    let publication_fence = authority.publication_fence();
    Ok(PersonalAgentToolContext {
        authenticated,
        authority,
        publication_fence,
        calling_profile_name: profile_name,
        target_agent_id: invocation.target_agent_id.clone(),
    })
}

/// Re-open the current provider registry and authenticated owner binding after
/// asynchronous work. Returning the audience only after the exact original
/// attestation matches keeps handlers from publishing with a stale profile
/// name, trust revision, endpoint digest, session, or expiry sample.
pub(crate) fn reattest_personal_agent_publication(
    resources: &AgentResources,
    authenticated: &AuthenticatedAppScope,
    profile_name: &str,
    fence: &AppPersonalAgentPublicationFence,
    tool_name: &str,
    now: DateTime<Utc>,
) -> Result<AppStoreReadAudience, ExecutionError> {
    let config = resources.magician_config_snapshot();
    reattest_from_config(&config, authenticated, profile_name, fence, now).map_err(|error| {
        warn!(
            error = ?error,
            tool_name,
            "personal-agent publication re-attestation failed"
        );
        personal_agent_authority_unavailable(tool_name)
    })
}

fn personal_agent_authority_unavailable(tool_name: &str) -> ExecutionError {
    ExecutionError::Step(format!(
        "{tool_name} personal-agent authority is unavailable"
    ))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn app_data_processing_class(
    provider_class: AppDisclosureProviderClass,
    remote_processing_enabled: bool,
) -> Result<AppAgentProcessingClass, &'static str> {
    match provider_class {
        AppDisclosureProviderClass::LocalModel => Ok(AppAgentProcessingClass::LocalModel),
        AppDisclosureProviderClass::RemoteModel if remote_processing_enabled => {
            Ok(AppAgentProcessingClass::RemoteModel)
        },
        AppDisclosureProviderClass::RemoteModel => Err("remote processing disabled"),
        AppDisclosureProviderClass::Deterministic | AppDisclosureProviderClass::ExternalTool => {
            Err("calling profile is not a model provider")
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use chrono::Duration;

    use super::*;
    use crate::magician_v2::agents::{
        AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
    };

    #[test]
    fn operational_authority_failures_have_one_content_free_model_error() {
        let error = personal_agent_authority_unavailable("app_data_query");
        assert_eq!(
            error.to_string(),
            "step execution failed: app_data_query personal-agent authority is unavailable"
        );
        for forbidden in ["profile", "revision", "digest", "endpoint", "session"] {
            assert!(!error.to_string().contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn forged_json_provenance_cannot_mint_personal_agent_authority() {
        let args = serde_json::json!({
            "__principal": "anonymous",
            "__workspace": "default",
            "__source_kind": "chat_inline",
            "__surface": "chat",
            "__chat_session_id": "forged-session",
            "__execution_id": "forged-execution"
        });
        let error = match require_direct_personal_agent_context(None, &args, "app_data_query") {
            Err(error) => error,
            Ok(_) => panic!("JSON alone must not mint typed invocation provenance"),
        };
        assert!(error
            .to_string()
            .contains("server-authenticated invocation provenance"));
    }

    #[tokio::test]
    async fn typed_invocation_without_authenticated_owner_credential_fails_closed() {
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::ChatInline,
            chat_session_id: Some("session-1".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        };
        let args = serde_json::json!({
            "__principal": "anonymous",
            "__workspace": "default",
            "__agent_id": "personal-assistant",
            "__execution_id": "execution:1"
        });
        let result = crate::magician_v2::execution::compiled_dispatch::COMPILED_INVOCATION_CONTEXT
            .scope(Some(invocation), async {
                require_direct_personal_agent_context(None, &args, "app_data_query")
            })
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("missing owner credential must fail closed"),
        };
        assert!(error
            .to_string()
            .contains("authenticated owner-chat credential"));
    }

    #[tokio::test]
    async fn typed_owner_credential_does_not_depend_on_hidden_scope_arguments() {
        use crate::magician_v2::apps::boundary::AppOwnerExecutionCredential;
        use crate::magician_v2::apps::models::{
            AppDigest, AppReference, AppRevision, AppScopeBindingRef,
        };
        use crate::magician_v2::apps::records::AppScope;

        let now = Utc::now();
        let scope = AppScope {
            principal: AppReference::parse("anonymous").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        };
        let digest = AppDigest::blake3(b"anonymous\0default");
        let authenticated = AuthenticatedAppScope::from_verified_session(
            scope,
            AppScopeBindingRef::parse(format!(
                "scope_{}",
                digest.as_str().trim_start_matches("blake3:")
            ))
            .unwrap(),
            AppReference::parse("actor:test-owner").unwrap(),
            AppReference::parse("session:test-owner").unwrap(),
            AppRevision::new(7).unwrap(),
            now.to_owned() - Duration::seconds(1),
            now.to_owned() + Duration::seconds(60),
        )
        .unwrap();
        let credential = Arc::new(
            AppOwnerExecutionCredential::from_authenticated_chat(
                authenticated,
                "session-1",
                "personal-assistant",
                now.to_owned(),
            )
            .unwrap(),
        );
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::ChatInline,
            chat_session_id: Some("session-1".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        };

        for args in [
            serde_json::json!({}),
            serde_json::json!({
                "__principal": "forged-principal",
                "__workspace": "forged-workspace"
            }),
        ] {
            let result = crate::magician_v2::execution::compiled_dispatch::COMPILED_APP_OWNER_EXECUTION_CREDENTIAL
                .scope(Some(Arc::clone(&credential)),
                    crate::magician_v2::execution::compiled_dispatch::COMPILED_INVOCATION_CONTEXT
                        .scope(Some(invocation.clone()), async {
                            require_direct_personal_agent_context(None, &args, "app_data_query")
                        }))
                .await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("missing provider profile must fail after typed owner admission"),
            };
            assert!(error.to_string().contains("attested calling-model profile"));
        }
    }

    #[test]
    fn remote_profile_requires_the_independent_operator_switch() {
        assert_eq!(
            app_data_processing_class(AppDisclosureProviderClass::RemoteModel, false),
            Err("remote processing disabled")
        );
        assert_eq!(
            app_data_processing_class(AppDisclosureProviderClass::RemoteModel, true),
            Ok(AppAgentProcessingClass::RemoteModel)
        );
    }
}
