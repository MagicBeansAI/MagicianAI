use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::secrets::{BrokerAccessRequest, SecretBroker};
use crate::magician_v2::strategy::plan::PlanStep;

pub const TREASURER_TOOL_NAME: &str = "treasurer";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TreasurerOperation {
    ListAvailable,
    RequestCredential,
}

#[derive(Debug, Clone, Deserialize)]
struct TreasurerInvocation {
    operation: TreasurerOperation,
    #[serde(default, alias = "secret_id")]
    credential_id: Option<String>,
    #[serde(default)]
    target_tool: Option<String>,
    #[serde(default)]
    target_action: Option<String>,
    #[serde(default, alias = "domain")]
    target_domain: Option<String>,
    #[serde(default)]
    ttl_secs: Option<i64>,
}

/// Thin compiled adapter over the shared secret broker contract.
///
/// Secret logic stays in `magician_v2/secrets/`. This provider only exposes the
/// broker over planner-visible tool transport so remote and external callers can
/// request grants without duplicating policy or vault behavior in a second code
/// path. The provider must be backed by a non-capability broker implementation
/// such as `LocalSecretBroker`; wiring `CapabilitySecretBroker` into this
/// provider would recurse back into `treasurer`.
pub struct TreasurerCapabilityProvider {
    broker: Arc<dyn SecretBroker>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for TreasurerCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreasurerCapabilityProvider")
            .field("broker", &"SecretBroker")
            .field("pack_def", &self.pack_def)
            .finish()
    }
}

impl TreasurerCapabilityProvider {
    pub fn new(broker: Arc<dyn SecretBroker>) -> Self {
        Self {
            broker,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for TreasurerCapabilityProvider {
    fn tool_name(&self) -> &str {
        TREASURER_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: TREASURER_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: TREASURER_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let resolved_params = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } if capability_name == TREASURER_TOOL_NAME => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "TreasurerCapabilityProvider received a non-treasurer action".to_string(),
                ))
            },
        };

        let invocation: TreasurerInvocation = serde_json::from_value(serde_json::Value::Object(
            resolved_params
                .clone()
                .into_iter()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ))
        .map_err(|err| ExecutionError::Step(format!("invalid treasurer params: {}", err)))?;

        let content = match invocation.operation {
            TreasurerOperation::ListAvailable => serde_json::to_string_pretty(
                &self
                    .broker
                    .list_available()
                    .await
                    .map_err(|err| ExecutionError::Step(err.to_string()))?,
            )
            .map_err(|err| {
                ExecutionError::Step(format!(
                    "failed to serialize treasurer list_available result: {}",
                    err
                ))
            })?,
            TreasurerOperation::RequestCredential => {
                let credential_id = required_string("credential_id", invocation.credential_id)?;
                let target_tool = required_string("target_tool", invocation.target_tool)?;
                let target_action = required_string("target_action", invocation.target_action)?;
                let response = self
                    .broker
                    .request_credential(BrokerAccessRequest {
                        credential_id,
                        tool: target_tool,
                        action: target_action,
                        domain: invocation.target_domain,
                        ttl_secs: invocation.ttl_secs,
                    })
                    .await
                    .map_err(|err| ExecutionError::Step(err.to_string()))?;
                serde_json::to_string_pretty(&response).map_err(|err| {
                    ExecutionError::Step(format!(
                        "failed to serialize treasurer request_credential result: {}",
                        err
                    ))
                })?
            },
        };

        Ok(ActionResult::Text { content })
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|d| d.execution.as_ref())
            .and_then(|e| e.default_timeout_secs)
            .unwrap_or(10)
    }
}

fn required_string(field: &str, value: Option<String>) -> Result<String, ExecutionError> {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| ExecutionError::Step(format!("treasurer requires non-empty '{}'", field)))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::magician_v2::secrets::{
        InMemoryKeyProvider, InjectionTarget, LocalSecretBroker, SecretPolicy, SecretStore,
    };

    fn test_store() -> Arc<SecretStore> {
        let base =
            std::env::temp_dir().join(format!("treasurer-provider-test-{}", uuid::Uuid::new_v4()));
        Arc::new(SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            base,
        ))
    }

    fn provision_secret(store: &SecretStore) {
        store
            .store_provisioned(
                "payments_api",
                "Payments API",
                HashMap::from([("value".to_string(), "secret-token".to_string())]),
                InjectionTarget::Header {
                    name: "Authorization".to_string(),
                    prefix: Some("Bearer ".to_string()),
                },
                SecretPolicy {
                    allowed_tools: vec!["http:post".to_string()],
                    allowed_domains: vec!["api.example.com".to_string()],
                    max_uses_per_day: None,
                    requires_approval: false,
                },
            )
            .expect("secret should be provisioned");
    }

    fn provider(store: Arc<SecretStore>) -> TreasurerCapabilityProvider {
        TreasurerCapabilityProvider::new(Arc::new(LocalSecretBroker::new(store)))
    }

    fn pack_action(params: HashMap<String, serde_json::Value>) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: TREASURER_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: TREASURER_TOOL_NAME.to_string(),
            },
            resolved_params: params,
        }
    }

    #[tokio::test]
    async fn lists_available_secrets() {
        let store = test_store();
        provision_secret(store.as_ref());

        let result = provider(store)
            .execute(
                &pack_action(HashMap::from([(
                    "operation".to_string(),
                    serde_json::Value::String("list_available".to_string()),
                )])),
                None,
                10,
            )
            .await
            .expect("treasurer execute should succeed");

        let ActionResult::Text { content } = result else {
            panic!("expected text action result");
        };
        assert!(content.contains("payments_api"));
        assert!(content.contains("Payments API"));
    }

    #[tokio::test]
    async fn requests_grant_tokens() {
        let store = test_store();
        provision_secret(store.as_ref());

        let result = provider(store)
            .execute(
                &pack_action(HashMap::from([
                    (
                        "operation".to_string(),
                        serde_json::Value::String("request_credential".to_string()),
                    ),
                    (
                        "credential_id".to_string(),
                        serde_json::Value::String("payments_api".to_string()),
                    ),
                    (
                        "target_tool".to_string(),
                        serde_json::Value::String("http".to_string()),
                    ),
                    (
                        "target_action".to_string(),
                        serde_json::Value::String("post".to_string()),
                    ),
                    (
                        "target_domain".to_string(),
                        serde_json::Value::String("api.example.com".to_string()),
                    ),
                ])),
                None,
                10,
            )
            .await
            .expect("treasurer execute should succeed");

        let ActionResult::Text { content } = result else {
            panic!("expected text action result");
        };
        assert!(content.contains("\"status\": \"issued\""));
        assert!(content.contains("\"credential_token\":"));
    }

    #[tokio::test]
    async fn rejects_missing_request_fields() {
        let result = provider(test_store())
            .execute(
                &pack_action(HashMap::from([(
                    "operation".to_string(),
                    serde_json::Value::String("request_credential".to_string()),
                )])),
                None,
                10,
            )
            .await
            .expect_err("missing credential_id should fail");

        assert!(result.to_string().contains("credential_id"));
    }
}
