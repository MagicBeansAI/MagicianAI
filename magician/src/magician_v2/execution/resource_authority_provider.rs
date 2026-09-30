//! ResourceAuthorityProvider — compiled provider for resource authority tools.
//!
//! Exposes ledger queries, token issuance, and token revocation as capability
//! provider tools. Three instances are registered in the capability registry,
//! one per tool:
//!   - `resource_ledger_read`  — query balances, spend history, remaining ceilings
//!   - `resource_token_issue`  — create SpendToken, record journal entry
//!   - `resource_token_revoke` — revoke token, revert unspent budget

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::ledger::{
    JournalEntry, LedgerEntry, ResourceAmount, ResourceLedger,
};
use crate::magician_v2::resource_authority::token::{
    CarryoverPolicy, CeilingPeriod, SpendToken, TokenStatus,
};
use crate::magician_v2::resource_authority::token_store::TokenStore;
use crate::magician_v2::strategy::plan::PlanStep;

/// A compiled provider for resource authority operations.
///
/// Each instance handles one tool name. Shared `Arc<RwLock<...>>` references
/// ensure all three tool instances see the same ledger and token store state.
pub struct ResourceAuthorityProvider {
    tool: String,
    ledger: Arc<RwLock<ResourceLedger>>,
    token_store: Arc<RwLock<TokenStore>>,
}

impl std::fmt::Debug for ResourceAuthorityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceAuthorityProvider")
            .field("tool", &self.tool)
            .finish()
    }
}

impl ResourceAuthorityProvider {
    pub fn new(
        tool: impl Into<String>,
        ledger: Arc<RwLock<ResourceLedger>>,
        token_store: Arc<RwLock<TokenStore>>,
    ) -> Self {
        Self {
            tool: tool.into(),
            ledger,
            token_store,
        }
    }

    // -----------------------------------------------------------------------
    // resource_ledger_read
    // -----------------------------------------------------------------------

    async fn execute_ledger_read(
        &self,
        params: &HashMap<String, Value>,
    ) -> Result<ActionResult, ExecutionError> {
        let query_type = params
            .get("query_type")
            .and_then(|v| v.as_str())
            .unwrap_or("balance");

        let ledger = self.ledger.read().await;

        let result = match query_type {
            "balance" => {
                let account = params.get("account").and_then(|v| v.as_str()).unwrap_or("");
                let balance = ledger.balance(&account.to_string());
                json!({ "account": account, "balance": balance.to_string() })
            },
            "total_spent" => {
                let commodity = params
                    .get("commodity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let total = ledger.total_spent_for_commodity(commodity);
                json!({ "commodity": commodity, "total_spent": total.to_string() })
            },
            "token_spent" => {
                let token_id = params
                    .get("token_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let total = ledger.total_spent_for_token(token_id);
                json!({ "token_id": token_id, "total_spent": total.to_string() })
            },
            "audit" => match ledger.audit() {
                Ok(()) => json!({ "status": "balanced", "imbalances": [] }),
                Err(imbalances) => {
                    let items: Vec<Value> = imbalances
                        .iter()
                        .map(|i| {
                            json!({
                                "commodity": i.commodity,
                                "expected": i.expected.to_string(),
                                "actual": i.actual.to_string(),
                            })
                        })
                        .collect();
                    json!({ "status": "imbalanced", "imbalances": items })
                },
            },
            "journal" => {
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let entries: Vec<Value> = ledger
                    .journal
                    .iter()
                    .rev()
                    .take(limit)
                    .map(|je| {
                        json!({
                            "id": je.id.to_string(),
                            "timestamp": je.timestamp.to_rfc3339(),
                            "reference": je.reference,
                            "agent_id": je.agent_id,
                            "entries": je.entries.iter().map(|e| json!({
                                "account": e.account,
                                "amount": e.amount.value.to_string(),
                                "commodity": e.amount.commodity,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                json!({ "entries": entries, "total_count": ledger.journal.len() })
            },
            other => {
                return Err(ExecutionError::Step(format!(
                    "Unknown ledger query_type: '{}'. Valid: balance, total_spent, token_spent, audit, journal",
                    other
                )));
            },
        };

        Ok(ActionResult::text(
            serde_json::to_string_pretty(&result).unwrap(),
        ))
    }

    // -----------------------------------------------------------------------
    // resource_token_issue
    // -----------------------------------------------------------------------

    async fn execute_token_issue(
        &self,
        params: &HashMap<String, Value>,
    ) -> Result<ActionResult, ExecutionError> {
        let issued_to = params
            .get("issued_to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step("resource_token_issue: missing 'issued_to'".into())
            })?
            .to_string();
        let commodity = params
            .get("commodity")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step("resource_token_issue: missing 'commodity'".into())
            })?
            .to_string();
        let ceiling_str = params
            .get("ceiling")
            .and_then(|v| v.as_str().or_else(|| v.as_f64().map(|_| "")))
            .unwrap_or("0");
        let ceiling: Decimal = if let Some(n) = params.get("ceiling").and_then(|v| v.as_f64()) {
            Decimal::from_f64_retain(n).unwrap_or_default()
        } else {
            ceiling_str.parse().unwrap_or_default()
        };
        let issued_by = params
            .get("issued_by")
            .and_then(|v| v.as_str())
            .unwrap_or("system")
            .to_string();
        let conditions: Vec<String> = params
            .get("conditions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let token_id = format!(
            "rct_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("x")
        );

        let token = SpendToken {
            id: token_id.clone(),
            issued_by: issued_by.clone(),
            issued_to: issued_to.clone(),
            commodity: commodity.clone(),
            ceiling,
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions,
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: None,
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        };

        // Record token issuance journal entry
        let now = Utc::now();
        let budget_account = format!("token:{}:budget:{}", token_id, commodity);
        let available_account = format!("agent:{}:available:{}", issued_by, commodity);

        let journal_entry = JournalEntry {
            id: uuid::Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: budget_account,
                    amount: ResourceAmount {
                        value: ceiling,
                        commodity: commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: available_account,
                    amount: ResourceAmount {
                        value: -ceiling,
                        commodity: commodity.clone(),
                    },
                },
            ],
            reference: format!("token_issuance:{}", token_id),
            agent_id: issued_by.clone(),
            metadata: HashMap::new(),
        };

        // Write to stores
        let mut ledger = self.ledger.write().await;
        ledger
            .record(journal_entry)
            .map_err(|e| ExecutionError::Step(format!("ledger record failed: {}", e)))?;

        let mut store = self.token_store.write().await;
        store
            .create(token)
            .map_err(|e| ExecutionError::Step(format!("token create failed: {}", e)))?;

        let result = json!({
            "token_id": token_id,
            "issued_by": issued_by,
            "issued_to": issued_to,
            "commodity": commodity,
            "ceiling": ceiling.to_string(),
            "status": "active",
        });

        Ok(ActionResult::text(
            serde_json::to_string_pretty(&result).unwrap(),
        ))
    }

    // -----------------------------------------------------------------------
    // resource_token_revoke
    // -----------------------------------------------------------------------

    async fn execute_token_revoke(
        &self,
        params: &HashMap<String, Value>,
    ) -> Result<ActionResult, ExecutionError> {
        let token_id = params
            .get("token_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step("resource_token_revoke: missing 'token_id'".into())
            })?
            .to_string();

        // Lock ordering: ledger FIRST, then token_store to prevent deadlocks
        let mut ledger = self.ledger.write().await;

        // Revoke in token store
        let mut store = self.token_store.write().await;
        let token = store
            .get(&token_id)
            .map_err(|e| ExecutionError::Step(format!("token not found: {}", e)))?
            .clone();
        store
            .revoke(&token_id)
            .map_err(|e| ExecutionError::Step(format!("token revoke failed: {}", e)))?;

        let budget_account = format!("token:{}:budget:{}", token_id, token.commodity);
        let unspent = ledger.balance(&budget_account);

        if unspent > Decimal::ZERO {
            let available_account =
                format!("agent:{}:available:{}", token.issued_by, token.commodity);
            let now = Utc::now();
            let revert_entry = JournalEntry {
                id: uuid::Uuid::new_v4(),
                timestamp: now,
                accrual_date: now,
                entries: vec![
                    LedgerEntry {
                        account: available_account,
                        amount: ResourceAmount {
                            value: unspent,
                            commodity: token.commodity.clone(),
                        },
                    },
                    LedgerEntry {
                        account: budget_account,
                        amount: ResourceAmount {
                            value: -unspent,
                            commodity: token.commodity.clone(),
                        },
                    },
                ],
                reference: format!("token_revert:{}", token_id),
                agent_id: token.issued_by.clone(),
                metadata: HashMap::new(),
            };
            ledger
                .record(revert_entry)
                .map_err(|e| ExecutionError::Step(format!("ledger revert failed: {}", e)))?;
        }

        let result = json!({
            "token_id": token_id,
            "status": "revoked",
            "unspent_reverted": unspent.to_string(),
            "commodity": token.commodity,
        });

        Ok(ActionResult::text(
            serde_json::to_string_pretty(&result).unwrap(),
        ))
    }
}

#[async_trait]
impl CapabilityProvider for ResourceAuthorityProvider {
    fn tool_name(&self) -> &str {
        &self.tool
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = step.parameters.clone();
        Ok(MaybeGatedAction::Bare(ExecutableAction::Pack {
            capability_name: self.tool.clone(),
            implementation: ImplementationType::Compiled {
                provider_name: self.tool.clone(),
            },
            resolved_params,
        }))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "ResourceAuthorityProvider received a non-pack action".to_string(),
                ))
            },
        };

        match self.tool.as_str() {
            "resource_ledger_read" => self.execute_ledger_read(params).await,
            "resource_token_issue" => self.execute_token_issue(params).await,
            "resource_token_revoke" => self.execute_token_revoke(params).await,
            other => Err(ExecutionError::Step(format!(
                "ResourceAuthorityProvider: unknown tool '{}'",
                other
            ))),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn make_shared() -> (Arc<RwLock<ResourceLedger>>, Arc<RwLock<TokenStore>>) {
        let ledger = Arc::new(RwLock::new(ResourceLedger::new()));
        let store = Arc::new(RwLock::new(TokenStore::new()));
        (ledger, store)
    }

    #[tokio::test]
    async fn test_issue_and_read_token() {
        let (ledger, store) = make_shared();
        let issue_provider =
            ResourceAuthorityProvider::new("resource_token_issue", ledger.clone(), store.clone());
        let read_provider =
            ResourceAuthorityProvider::new("resource_ledger_read", ledger.clone(), store.clone());

        // Issue a token
        let mut params = HashMap::new();
        params.insert("issued_to".to_string(), json!("agent-cmo"));
        params.insert("issued_by".to_string(), json!("agent-cfo"));
        params.insert("commodity".to_string(), json!("USD"));
        params.insert("ceiling".to_string(), json!("500"));

        let action = ExecutableAction::Pack {
            capability_name: "resource_token_issue".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "resource_token_issue".to_string(),
            },
            resolved_params: params,
        };
        let result = issue_provider.execute(&action, None, 30).await.unwrap();
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("Expected text result, got: {:?}", other),
        };
        let issued: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(issued["commodity"], "USD");
        assert_eq!(issued["ceiling"], "500");
        assert_eq!(issued["status"], "active");
        let token_id = issued["token_id"].as_str().unwrap();

        // Read the balance
        let mut read_params = HashMap::new();
        read_params.insert("query_type".to_string(), json!("token_spent"));
        read_params.insert("token_id".to_string(), json!(token_id));
        let read_action = ExecutableAction::Pack {
            capability_name: "resource_ledger_read".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "resource_ledger_read".to_string(),
            },
            resolved_params: read_params,
        };
        let read_result = read_provider.execute(&read_action, None, 30).await.unwrap();
        let read_text = match read_result {
            ActionResult::Text { content } => content,
            other => panic!("Expected text result, got: {:?}", other),
        };
        let read_data: Value = serde_json::from_str(&read_text).unwrap();
        assert_eq!(read_data["total_spent"], "0");
    }

    #[tokio::test]
    async fn test_issue_and_revoke_token() {
        let (ledger, store) = make_shared();
        let issue_provider =
            ResourceAuthorityProvider::new("resource_token_issue", ledger.clone(), store.clone());
        let revoke_provider =
            ResourceAuthorityProvider::new("resource_token_revoke", ledger.clone(), store.clone());

        // Issue
        let mut params = HashMap::new();
        params.insert("issued_to".to_string(), json!("agent-executor"));
        params.insert("issued_by".to_string(), json!("agent-cfo"));
        params.insert("commodity".to_string(), json!("EMAIL_SENDS"));
        params.insert("ceiling".to_string(), json!("100"));

        let action = ExecutableAction::Pack {
            capability_name: "resource_token_issue".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "resource_token_issue".to_string(),
            },
            resolved_params: params,
        };
        let result = issue_provider.execute(&action, None, 30).await.unwrap();
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("Expected text result, got: {:?}", other),
        };
        let issued: Value = serde_json::from_str(&text).unwrap();
        let token_id = issued["token_id"].as_str().unwrap().to_string();

        // Revoke
        let mut revoke_params = HashMap::new();
        revoke_params.insert("token_id".to_string(), json!(token_id));
        let revoke_action = ExecutableAction::Pack {
            capability_name: "resource_token_revoke".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "resource_token_revoke".to_string(),
            },
            resolved_params: revoke_params,
        };
        let revoke_result = revoke_provider
            .execute(&revoke_action, None, 30)
            .await
            .unwrap();
        let revoke_text = match revoke_result {
            ActionResult::Text { content } => content,
            other => panic!("Expected text result, got: {:?}", other),
        };
        let revoke_data: Value = serde_json::from_str(&revoke_text).unwrap();
        assert_eq!(revoke_data["status"], "revoked");
        assert_eq!(revoke_data["unspent_reverted"], "100");

        // Verify token is revoked in store
        let store_guard = store.read().await;
        let token = store_guard.get(&token_id).unwrap();
        assert_eq!(token.status, TokenStatus::Revoked);
    }

    #[tokio::test]
    async fn test_ledger_audit() {
        let (ledger, store) = make_shared();
        let read_provider =
            ResourceAuthorityProvider::new("resource_ledger_read", ledger.clone(), store.clone());

        let mut params = HashMap::new();
        params.insert("query_type".to_string(), json!("audit"));
        let action = ExecutableAction::Pack {
            capability_name: "resource_ledger_read".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "resource_ledger_read".to_string(),
            },
            resolved_params: params,
        };
        let result = read_provider.execute(&action, None, 30).await.unwrap();
        let text = match result {
            ActionResult::Text { content } => content,
            other => panic!("Expected text result, got: {:?}", other),
        };
        let data: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(data["status"], "balanced");
    }
}
