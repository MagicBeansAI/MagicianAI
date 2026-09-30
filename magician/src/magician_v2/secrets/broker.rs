use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::{SecretListEntry, SecretRef, SecretStore, SecretStoreError, SecretStoreResolver};
use crate::magician_v2::execution::actions::{ActionResult, ExecutableAction};
use crate::magician_v2::execution::capability::CapabilityRegistry;
use crate::magician_v2::execution::treasurer_provider::TREASURER_TOOL_NAME;
use crate::magician_v2::strategy::plan::PlanStep;

/// Canonical request shape for brokered credential access.
///
/// Both local runtime code and the optional `treasurer` capability use this
/// contract. The important design point is that semantics stay identical even
/// when transport changes. Local execution keeps an in-process fast path, while
/// external and cross-process callers can speak to the same broker contract
/// through the planner-visible capability layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrokerAccessRequest {
    pub credential_id: String,
    pub tool: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
}

impl BrokerAccessRequest {
    pub fn new(
        credential_id: impl Into<String>,
        tool: impl Into<String>,
        action: impl Into<String>,
        domain: Option<String>,
    ) -> Self {
        Self {
            credential_id: credential_id.into(),
            tool: tool.into(),
            action: action.into(),
            domain,
            ttl_secs: None,
        }
    }
}

/// Structured broker response for credential requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BrokerAccessResponse {
    Issued {
        credential_id: String,
        credential_token: String,
    },
    Denied {
        credential_id: String,
        reason: String,
    },
    NeedsApproval {
        credential_id: String,
        challenge_id: String,
    },
    NotFound {
        credential_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SecretBrokerError {
    #[error("broker request failed: {0}")]
    Internal(String),

    #[error("broker protocol violation: {0}")]
    Protocol(String),
}

impl From<SecretStoreError> for SecretBrokerError {
    fn from(value: SecretStoreError) -> Self {
        Self::Internal(value.to_string())
    }
}

#[async_trait]
pub trait SecretBroker: Send + Sync + std::fmt::Debug {
    async fn list_available(&self) -> Result<Vec<SecretListEntry>, SecretBrokerError>;

    async fn request_credential(
        &self,
        request: BrokerAccessRequest,
    ) -> Result<BrokerAccessResponse, SecretBrokerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretScope {
    principal: String,
    workspace: String,
}

tokio::task_local! {
    static ACTIVE_SECRET_SCOPE: SecretScope;
}

pub async fn with_secret_scope<F, T>(principal: &str, workspace: &str, future: F) -> T
where
    F: Future<Output = T>,
{
    ACTIVE_SECRET_SCOPE
        .scope(
            SecretScope {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
            },
            future,
        )
        .await
}

/// In-process broker implementation over the shared `SecretStore`.
///
/// This is the runtime fast path. The executor intentionally does not pretend it
/// must call the planner-visible `treasurer` tool just to talk to code in the
/// same process. Instead it uses the same broker semantics directly so local and
/// external callers share one contract without paying unnecessary transport and
/// logging overhead in the common case.
#[derive(Clone)]
pub struct LocalSecretBroker {
    store: Arc<SecretStore>,
}

impl std::fmt::Debug for LocalSecretBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSecretBroker")
            .field("store", &"SecretStore")
            .finish()
    }
}

impl LocalSecretBroker {
    pub fn new(store: Arc<SecretStore>) -> Self {
        Self { store }
    }
}

/// In-process broker implementation that resolves the correct scoped store from
/// the active V3 execution context instead of binding to a startup-global store.
#[derive(Clone)]
pub struct ScopedSecretBroker {
    resolver: Arc<SecretStoreResolver>,
}

impl std::fmt::Debug for ScopedSecretBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedSecretBroker")
            .field("resolver", &"SecretStoreResolver")
            .finish()
    }
}

impl ScopedSecretBroker {
    pub fn new(resolver: Arc<SecretStoreResolver>) -> Self {
        Self { resolver }
    }

    fn scoped_store(&self) -> Result<Arc<SecretStore>, SecretBrokerError> {
        let scope = ACTIVE_SECRET_SCOPE
            .try_with(|scope| scope.clone())
            .map_err(|_| {
                SecretBrokerError::Internal(
                    "missing scoped secret context for treasurer request".to_string(),
                )
            })?;
        self.resolver
            .resolve_for_scope(&scope.principal, &scope.workspace)
            .map_err(SecretBrokerError::from)
    }
}

#[async_trait]
impl SecretBroker for LocalSecretBroker {
    async fn list_available(&self) -> Result<Vec<SecretListEntry>, SecretBrokerError> {
        Ok(self.store.list_available())
    }

    async fn request_credential(
        &self,
        request: BrokerAccessRequest,
    ) -> Result<BrokerAccessResponse, SecretBrokerError> {
        match self.store.issue_grant(
            &request.credential_id,
            &request.tool,
            &request.action,
            request.domain.as_deref(),
            request.ttl_secs,
        ) {
            Ok(SecretRef::Grant(credential_token)) => Ok(BrokerAccessResponse::Issued {
                credential_id: request.credential_id,
                credential_token,
            }),
            Ok(other) => Err(SecretBrokerError::Protocol(format!(
                "secret broker expected a grant token, got {:?}",
                other
            ))),
            Err(SecretStoreError::SecretNotFound(_)) => Ok(BrokerAccessResponse::NotFound {
                credential_id: request.credential_id,
            }),
            Err(SecretStoreError::PolicyDenied(reason)) => Ok(BrokerAccessResponse::Denied {
                credential_id: request.credential_id,
                reason,
            }),
            Err(SecretStoreError::ApprovalRequired(challenge_id)) => {
                Ok(BrokerAccessResponse::NeedsApproval {
                    credential_id: request.credential_id,
                    challenge_id,
                })
            },
            Err(err) => Err(err.into()),
        }
    }
}

#[async_trait]
impl SecretBroker for ScopedSecretBroker {
    async fn list_available(&self) -> Result<Vec<SecretListEntry>, SecretBrokerError> {
        Ok(self.scoped_store()?.list_available())
    }

    async fn request_credential(
        &self,
        request: BrokerAccessRequest,
    ) -> Result<BrokerAccessResponse, SecretBrokerError> {
        let store = self.scoped_store()?;
        match store.issue_grant(
            &request.credential_id,
            &request.tool,
            &request.action,
            request.domain.as_deref(),
            request.ttl_secs,
        ) {
            Ok(SecretRef::Grant(credential_token)) => Ok(BrokerAccessResponse::Issued {
                credential_id: request.credential_id,
                credential_token,
            }),
            Ok(other) => Err(SecretBrokerError::Protocol(format!(
                "secret broker expected a grant token, got {:?}",
                other
            ))),
            Err(SecretStoreError::SecretNotFound(_)) => Ok(BrokerAccessResponse::NotFound {
                credential_id: request.credential_id,
            }),
            Err(SecretStoreError::PolicyDenied(reason)) => Ok(BrokerAccessResponse::Denied {
                credential_id: request.credential_id,
                reason,
            }),
            Err(SecretStoreError::ApprovalRequired(challenge_id)) => {
                Ok(BrokerAccessResponse::NeedsApproval {
                    credential_id: request.credential_id,
                    challenge_id,
                })
            },
            Err(err) => Err(err.into()),
        }
    }
}

/// Capability-backed broker client for planner-visible and future remote flows.
///
/// This is intentionally the opposite end of the same contract. It talks to the
/// compiled `treasurer` capability instead of reaching into `SecretStore`
/// directly. Keeping both clients behind one trait lets internal code choose the
/// cheapest in-process path today while external callers can use the exact same
/// request/response model through tool transport.
#[derive(Clone)]
pub struct CapabilitySecretBroker {
    registry: Arc<CapabilityRegistry>,
}

impl std::fmt::Debug for CapabilitySecretBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilitySecretBroker")
            .field("registry", &"CapabilityRegistry")
            .finish()
    }
}

impl CapabilitySecretBroker {
    pub fn new(registry: Arc<CapabilityRegistry>) -> Self {
        Self { registry }
    }

    async fn invoke_json<T: DeserializeOwned>(
        &self,
        parameters: HashMap<String, Value>,
    ) -> Result<T, SecretBrokerError> {
        let provider = self.registry.get(TREASURER_TOOL_NAME).ok_or_else(|| {
            SecretBrokerError::Protocol(
                "treasurer capability is not registered in this runtime".to_string(),
            )
        })?;
        let step = PlanStep {
            id: "treasurer-broker".to_string(),
            task: "broker secret access".to_string(),
            tool: Some(TREASURER_TOOL_NAME.to_string()),
            parameters,
            expected_outputs: Vec::new(),
            confidence: 1.0,
            metadata: HashMap::new(),
            timeout_override_secs: None,
            depends_on: Vec::new(),
            success_criteria: None,
            parameter_provenance: HashMap::new(),
            role: None,
            step_hash: None,
            providing_agent_id: None,
            readiness: None,
            sub_steps: Vec::new(),
        };
        let maybe_gated = provider.lower(&step).map_err(|err| {
            SecretBrokerError::Internal(format!("failed to lower treasurer request: {}", err))
        })?;
        let action = maybe_gated.inner_action();
        let result = provider
            .execute(action, None, provider.default_timeout_secs())
            .await
            .map_err(|err| {
                SecretBrokerError::Internal(format!("treasurer request failed: {}", err))
            })?;
        parse_json_action_result::<T>(action, result)
    }
}

#[async_trait]
impl SecretBroker for CapabilitySecretBroker {
    async fn list_available(&self) -> Result<Vec<SecretListEntry>, SecretBrokerError> {
        self.invoke_json(HashMap::from([(
            "operation".to_string(),
            Value::String("list_available".to_string()),
        )]))
        .await
    }

    async fn request_credential(
        &self,
        request: BrokerAccessRequest,
    ) -> Result<BrokerAccessResponse, SecretBrokerError> {
        let mut parameters = HashMap::from([
            (
                "operation".to_string(),
                Value::String("request_credential".to_string()),
            ),
            (
                "credential_id".to_string(),
                Value::String(request.credential_id),
            ),
            ("target_tool".to_string(), Value::String(request.tool)),
            ("target_action".to_string(), Value::String(request.action)),
        ]);
        if let Some(domain) = request.domain {
            parameters.insert("target_domain".to_string(), Value::String(domain));
        }
        if let Some(ttl_secs) = request.ttl_secs {
            parameters.insert(
                "ttl_secs".to_string(),
                Value::Number(serde_json::Number::from(ttl_secs)),
            );
        }
        self.invoke_json(parameters).await
    }
}

fn parse_json_action_result<T: DeserializeOwned>(
    action: &ExecutableAction,
    result: ActionResult,
) -> Result<T, SecretBrokerError> {
    match result {
        ActionResult::Text { content } => serde_json::from_str::<T>(&content).map_err(|err| {
            SecretBrokerError::Protocol(format!(
                "treasurer returned invalid JSON for action {:?}: {}",
                action.action_type_name(),
                err
            ))
        }),
        other => Err(SecretBrokerError::Protocol(format!(
            "treasurer returned unexpected action result: {:?}",
            other
        ))),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::magician_v2::execution::capability::{CapabilityPackDefinition, ImplementationType};
    use crate::magician_v2::execution::treasurer_provider::TreasurerCapabilityProvider;
    use crate::magician_v2::secrets::{InMemoryKeyProvider, InjectionTarget, SecretPolicy};

    fn test_store() -> Arc<SecretStore> {
        let base =
            std::env::temp_dir().join(format!("treasurer-broker-test-{}", uuid::Uuid::new_v4()));
        Arc::new(SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            base,
        ))
    }

    fn provision_secret(store: &SecretStore, requires_approval: bool) {
        let mut fields = HashMap::new();
        fields.insert("value".to_string(), "secret-token".to_string());
        store
            .store_provisioned(
                "payments_api",
                "Payments API",
                fields,
                InjectionTarget::Header {
                    name: "Authorization".to_string(),
                    prefix: Some("Bearer ".to_string()),
                },
                SecretPolicy {
                    allowed_tools: vec!["http:post".to_string()],
                    allowed_domains: vec!["api.example.com".to_string()],
                    max_uses_per_day: None,
                    requires_approval,
                },
            )
            .expect("secret should be provisioned");
    }

    fn treasurer_pack() -> CapabilityPackDefinition {
        CapabilityPackDefinition {
            name: TREASURER_TOOL_NAME.to_string(),
            description: Some("Treasurer broker".to_string()),
            version: Some("1.0.0".to_string()),
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: Vec::new(),
            implementation: ImplementationType::Compiled {
                provider_name: TREASURER_TOOL_NAME.to_string(),
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        }
    }

    fn test_registry(store: Arc<SecretStore>) -> Arc<CapabilityRegistry> {
        let registry = Arc::new(CapabilityRegistry::new());
        let provider = TreasurerCapabilityProvider::new(Arc::new(LocalSecretBroker::new(store)))
            .with_pack_def(treasurer_pack());
        registry.register(Arc::new(provider));
        registry
    }

    #[tokio::test]
    async fn local_broker_issues_token() {
        let store = test_store();
        provision_secret(store.as_ref(), false);
        let broker = LocalSecretBroker::new(store);

        let response = broker
            .request_credential(BrokerAccessRequest::new(
                "payments_api",
                "http",
                "post",
                Some("api.example.com".to_string()),
            ))
            .await
            .expect("broker should return");

        match response {
            BrokerAccessResponse::Issued {
                credential_id,
                credential_token,
            } => {
                assert_eq!(credential_id, "payments_api");
                assert!(!credential_token.is_empty());
            },
            other => panic!("expected issued response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn local_broker_returns_denied() {
        let store = test_store();
        provision_secret(store.as_ref(), false);
        let broker = LocalSecretBroker::new(store);

        let response = broker
            .request_credential(BrokerAccessRequest::new(
                "payments_api",
                "http",
                "post",
                Some("blocked.example.com".to_string()),
            ))
            .await
            .expect("broker should return");

        assert!(matches!(response, BrokerAccessResponse::Denied { .. }));
    }

    #[tokio::test]
    async fn local_broker_returns_needs_approval() {
        let store = test_store();
        provision_secret(store.as_ref(), true);
        let broker = LocalSecretBroker::new(store);

        let response = broker
            .request_credential(BrokerAccessRequest::new(
                "payments_api",
                "http",
                "post",
                Some("api.example.com".to_string()),
            ))
            .await
            .expect("broker should return");

        assert!(matches!(
            response,
            BrokerAccessResponse::NeedsApproval { .. }
        ));
    }

    #[tokio::test]
    async fn local_broker_returns_not_found() {
        let broker = LocalSecretBroker::new(test_store());

        let response = broker
            .request_credential(BrokerAccessRequest::new(
                "missing",
                "http",
                "post",
                Some("api.example.com".to_string()),
            ))
            .await
            .expect("broker should return");

        assert!(matches!(response, BrokerAccessResponse::NotFound { .. }));
    }

    #[tokio::test]
    async fn capability_broker_reuses_same_contract() {
        let store = test_store();
        provision_secret(store.as_ref(), false);
        let broker = CapabilitySecretBroker::new(test_registry(store));

        let listed = broker
            .list_available()
            .await
            .expect("listing should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "payments_api");

        let response = broker
            .request_credential(BrokerAccessRequest::new(
                "payments_api",
                "http",
                "post",
                Some("api.example.com".to_string()),
            ))
            .await
            .expect("broker should return");

        assert!(matches!(response, BrokerAccessResponse::Issued { .. }));
    }
}
