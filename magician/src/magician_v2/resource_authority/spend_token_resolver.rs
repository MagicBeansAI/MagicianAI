//! Resolves `(principal, agent_id, tool_name, commodity)` → token ids for
//! `spend_session::admit`.
//!
//! Layer 2 of `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`.
//! The resolver walks `ResourceAuthorityConfig::budgets`, lazy-issues a
//! `SpendToken` per matched (scope, id, commodity) tuple on first lookup,
//! and returns the token ids that authorize the call.
//!
//! Token-issuance scheme (consistent prefix to avoid collision with the
//! autonomous agent-issued token namespace):
//!   id           : `cfg-{scope}-{owner_id}-{commodity}`
//!   issued_by    : `system`
//!   issued_to    : `{scope}:{owner_id}` (e.g. `principal:owner`)
//!
//! At dispatch time, the chat fast path passes
//!   active_owner_agent_id   = `chat:{session_id}` (synthetic — won't match)
//!   owner_authorization_chain = [`principal:X`, `agent:Y`, `tool:Z`]
//! so every token whose `issued_to` matches a chain entry is authorized.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::config::{BudgetRow, BudgetScope, ResourceAuthorityConfig};
use super::ledger::{JournalEntry, ResourceLedger};
use super::token::{period_window_start_at, CeilingPeriod, SpendToken, TokenStatus};
use super::token_store::TokenStore;
use super::types::canonicalize_commodity;

/// Synthetic issuer used for all config-driven tokens. Differentiated
/// from any real `agent_id` by the suffix so journal entries are
/// readable in the audit trail.
const CONFIG_ISSUER: &str = "system-config";

/// Walks declarative budgets and returns the token ids that should
/// gate a given call.
#[async_trait]
pub trait SpendTokenResolver: Send + Sync + std::fmt::Debug {
    /// Returns the token ids authorizing a call. Empty Vec means no
    /// budget is configured for ANY relevant scope; the caller MUST
    /// decide whether to reject the call or allow it (see chat
    /// fast-path rejection logic).
    async fn resolve(
        &self,
        principal: &str,
        agent_id: &str,
        tool_name: &str,
        commodity: &str,
    ) -> Vec<String>;
}

/// Owner-authorization chain to pair with `resolve()`'s output. Lets
/// `find_authorized_active_token` in `gate.rs` match each scope-token
/// against its corresponding chain entry (since `token.issued_to` is
/// `{scope}:{id}`, not the active chat agent's raw id).
pub fn owner_authorization_chain(principal: &str, agent_id: &str, tool_name: &str) -> Vec<String> {
    vec![
        format!("{}:{}", BudgetScope::Principal.owner_prefix(), principal),
        format!("{}:{}", BudgetScope::Agent.owner_prefix(), agent_id),
        format!("{}:{}", BudgetScope::Tool.owner_prefix(), tool_name),
    ]
}

/// Default `SpendTokenResolver`: reads from a static `ResourceAuthorityConfig`,
/// lazily materializes tokens into `TokenStore` + funding journal entries
/// into `ResourceLedger`.
pub struct ConfigSpendTokenResolver {
    config: ResourceAuthorityConfig,
    ledger: Arc<RwLock<ResourceLedger>>,
    token_store: Arc<RwLock<TokenStore>>,
    /// Cache of materialized token ids keyed by `(scope, owner_id, commodity)`.
    /// Avoids re-checking the token store on every resolve.
    issued: RwLock<HashMap<TokenKey, String>>,
}

impl std::fmt::Debug for ConfigSpendTokenResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigSpendTokenResolver")
            .field("enabled", &self.config.enabled)
            .field("budget_rows", &self.config.budgets.len())
            .field("system_ceilings", &self.config.system_ceilings.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenKey {
    scope: BudgetScope,
    owner_id: String,
    commodity: String,
}

impl ConfigSpendTokenResolver {
    pub fn new(
        config: ResourceAuthorityConfig,
        ledger: Arc<RwLock<ResourceLedger>>,
        token_store: Arc<RwLock<TokenStore>>,
    ) -> Self {
        Self {
            config,
            ledger,
            token_store,
            issued: RwLock::new(HashMap::new()),
        }
    }

    /// Lazily ensure a token exists for `(scope, owner_id, commodity)`.
    /// Returns the token id when a matching `BudgetRow` exists in
    /// config (and the issuance succeeded), otherwise `None`.
    ///
    /// A wildcard row (`id: *`) issues one shared token keyed on `*`, not
    /// a per-caller copy.
    async fn ensure_token(
        &self,
        scope: BudgetScope,
        owner_id: &str,
        commodity: &str,
    ) -> Option<String> {
        let row = self.config.find_row(scope, owner_id, commodity)?.clone();
        let commodity = canonicalize_commodity(&row.commodity);
        let token_owner_id = row.id.trim();
        let key = TokenKey {
            scope,
            owner_id: token_owner_id.to_string(),
            commodity: commodity.clone(),
        };

        // Fast path: cache hit.
        if let Some(id) = self.issued.read().await.get(&key).cloned() {
            return Some(id);
        }

        let token_id = config_token_id(scope, token_owner_id, &commodity);

        // Slow path: serialize on the write lock and re-check (covers
        // the lost-update race when two callers hit a cold cache).
        let mut issued_guard = self.issued.write().await;
        if let Some(existing) = issued_guard.get(&key).cloned() {
            return Some(existing);
        }

        // Already in TokenStore from a previous run? Reuse the id (do
        // not re-fund) but copy the live YAML ceiling/period/carryover
        // onto the token so an operator edit takes effect after restart
        // instead of leaving the first-issue snapshot in force.
        {
            let mut store_guard = self.token_store.write().await;
            if let Ok(token) = store_guard.get_mut(&token_id) {
                token.ceiling = row.ceiling;
                token.period = row.period.clone();
                token.carryover = row.carryover.clone();
                token.system_ceiling_id = row.system_ceiling_id.clone();
                issued_guard.insert(key.clone(), token_id.clone());
                return Some(token_id);
            }
        }

        // Issue from scratch: write the funding journal entry + create
        // the SpendToken. Failures log and bail so the call rejects
        // rather than silently un-budgeted.
        match issue_config_token(
            &row,
            &token_id,
            scope,
            token_owner_id,
            &self.ledger,
            &self.token_store,
        )
        .await
        {
            Ok(()) => {
                issued_guard.insert(key.clone(), token_id.clone());
                Some(token_id)
            },
            Err(error) => {
                warn!(
                    scope = ?scope,
                    owner_id,
                    commodity,
                    error,
                    "[RESOURCE-AUTHORITY] config-driven token issuance failed; \
                     call will see no token for this scope and may reject"
                );
                None
            },
        }
    }
}

#[async_trait]
impl SpendTokenResolver for ConfigSpendTokenResolver {
    async fn resolve(
        &self,
        principal: &str,
        agent_id: &str,
        tool_name: &str,
        commodity: &str,
    ) -> Vec<String> {
        if !self.config.enabled {
            return Vec::new();
        }
        let commodity = canonicalize_commodity(commodity);
        let mut tokens = Vec::new();
        for (scope, owner_id) in [
            (BudgetScope::Principal, principal),
            (BudgetScope::Agent, agent_id),
            (BudgetScope::Tool, tool_name),
        ] {
            if let Some(id) = self.ensure_token(scope, owner_id, &commodity).await {
                tokens.push(id);
            }
        }
        debug!(
            principal,
            agent_id,
            tool_name,
            commodity,
            token_count = tokens.len(),
            "[RESOURCE-AUTHORITY] resolved spend tokens"
        );
        tokens
    }
}

/// Deterministic config-token id so a second run of the same process
/// (or a restarted process reading the same token-store JSON) doesn't
/// double-issue.
fn config_token_id(scope: BudgetScope, owner_id: &str, commodity: &str) -> String {
    format!("cfg-{}-{}-{}", scope.owner_prefix(), owner_id, commodity)
}

async fn issue_config_token(
    row: &BudgetRow,
    token_id: &str,
    scope: BudgetScope,
    owner_id: &str,
    ledger: &Arc<RwLock<ResourceLedger>>,
    token_store: &Arc<RwLock<TokenStore>>,
) -> Result<(), String> {
    let issued_to = format!("{}:{}", scope.owner_prefix(), owner_id);
    let commodity = canonicalize_commodity(&row.commodity);
    let now = Utc::now();
    let last_period_start = match row.period {
        CeilingPeriod::Total => None,
        _ => Some(period_window_start_at(&row.period, now)),
    };
    let token = SpendToken {
        id: token_id.to_string(),
        issued_by: CONFIG_ISSUER.to_string(),
        issued_to: issued_to.clone(),
        commodity: commodity.clone(),
        ceiling: row.ceiling,
        period: row.period.clone(),
        carryover: row.carryover.clone(),
        conditions: Vec::new(),
        velocity_limit: None,
        status: TokenStatus::Active,
        expires_at: None,
        created_at: now,
        system_ceiling_id: row.system_ceiling_id.clone(),
        last_period_start,
    };

    // Acquire locks in the documented order: ledger first, then store.
    let mut ledger_guard = ledger.write().await;
    let mut store_guard = token_store.write().await;

    // Fund the token directly from system_allocation. Skips the
    // intermediate "fund a synthetic agent, then issue from it" dance
    // that `JournalEntry::bootstrap` + `token_issuance` would do —
    // identical end state (token.budget = +ceiling, system.allocation
    // = -ceiling), one journal entry instead of two.
    let funding = config_token_funding_entry(token_id, &commodity, row.ceiling);
    ledger_guard
        .record(funding)
        .map_err(|e| format!("ledger record: {e}"))?;

    store_guard
        .create(token)
        .map_err(|e| format!("token store create: {e}"))?;

    Ok(())
}

/// Direct funding journal entry: skips the intermediate synthetic
/// agent account that `JournalEntry::bootstrap` + `token_issuance`
/// would create. End state is identical (token budget +ceiling,
/// system allocation -ceiling) but it's a single zero-sum entry.
fn config_token_funding_entry(token_id: &str, commodity: &str, ceiling: Decimal) -> JournalEntry {
    use super::ledger::{LedgerEntry, ResourceAmount};
    use uuid::Uuid;

    let now = Utc::now();
    JournalEntry {
        id: Uuid::new_v4(),
        timestamp: now,
        accrual_date: now,
        entries: vec![
            LedgerEntry {
                account: format!("token:{}:budget:{}", token_id, commodity),
                amount: ResourceAmount {
                    value: ceiling,
                    commodity: commodity.to_string(),
                },
            },
            LedgerEntry {
                account: format!("system:allocation:{}", commodity),
                amount: ResourceAmount {
                    value: -ceiling,
                    commodity: commodity.to_string(),
                },
            },
        ],
        reference: format!("config_token_funding:{}", token_id),
        agent_id: CONFIG_ISSUER.to_string(),
        metadata: HashMap::new(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::resource_authority::token::{CarryoverPolicy, CeilingPeriod};

    fn budget_row(scope: BudgetScope, id: &str, ceiling: i64) -> BudgetRow {
        BudgetRow {
            scope,
            id: id.to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::from(ceiling),
            period: CeilingPeriod::Monthly,
            carryover: CarryoverPolicy::None,
            system_ceiling_id: None,
        }
    }

    fn resolver_with_rows(rows: Vec<BudgetRow>) -> ConfigSpendTokenResolver {
        let config = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: rows,
        };
        ConfigSpendTokenResolver::new(
            config,
            Arc::new(RwLock::new(ResourceLedger::new())),
            Arc::new(RwLock::new(TokenStore::new())),
        )
    }

    #[tokio::test]
    async fn resolve_returns_empty_when_disabled() {
        let mut cfg = ResourceAuthorityConfig::default();
        cfg.enabled = false;
        cfg.budgets = vec![budget_row(BudgetScope::Principal, "owner", 100)];
        let resolver = ConfigSpendTokenResolver::new(
            cfg,
            Arc::new(RwLock::new(ResourceLedger::new())),
            Arc::new(RwLock::new(TokenStore::new())),
        );
        let ids = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn resolve_stacks_principal_agent_and_tool_tokens() {
        let resolver = resolver_with_rows(vec![
            budget_row(BudgetScope::Principal, "owner", 100),
            budget_row(BudgetScope::Agent, "presto", 50),
            budget_row(BudgetScope::Tool, "create_task", 5),
        ]);
        let ids = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().any(|id| id == "cfg-principal-owner-USD"));
        assert!(ids.iter().any(|id| id == "cfg-agent-presto-USD"));
        assert!(ids.iter().any(|id| id == "cfg-tool-create_task-USD"));
    }

    #[tokio::test]
    async fn resolve_returns_only_configured_scopes() {
        // Only principal-scope is configured; agent + tool unbudgeted.
        let resolver = resolver_with_rows(vec![budget_row(BudgetScope::Principal, "owner", 100)]);
        let ids = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        assert_eq!(ids, vec!["cfg-principal-owner-USD"]);
    }

    #[tokio::test]
    async fn resolve_is_idempotent_across_calls() {
        let resolver = resolver_with_rows(vec![budget_row(BudgetScope::Principal, "owner", 100)]);
        let first = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        let second = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        assert_eq!(first, second);

        // Verify no double-issuance: the journal should have ONE
        // funding entry, not two.
        let ledger = resolver.ledger.read().await;
        let funding_entries: Vec<_> = ledger
            .journal
            .iter()
            .filter(|e| e.reference.starts_with("config_token_funding:"))
            .collect();
        assert_eq!(funding_entries.len(), 1);
    }

    #[tokio::test]
    async fn owner_authorization_chain_has_all_three_scopes() {
        let chain = owner_authorization_chain("owner", "presto", "create_task");
        assert_eq!(
            chain,
            vec![
                "principal:owner".to_string(),
                "agent:presto".to_string(),
                "tool:create_task".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn resolve_writes_zero_sum_funding_entry() {
        let resolver = resolver_with_rows(vec![budget_row(BudgetScope::Principal, "owner", 100)]);
        let _ = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        let ledger = resolver.ledger.read().await;
        let token_balance =
            ledger.balance(&"token:cfg-principal-owner-USD:budget:USD".to_string());
        let system_balance = ledger.balance(&"system:allocation:USD".to_string());
        assert_eq!(token_balance, Decimal::from(100));
        assert_eq!(system_balance, -Decimal::from(100));
    }

    #[tokio::test]
    async fn resolve_canonicalizes_commodity_and_token_id() {
        let resolver = resolver_with_rows(vec![budget_row(BudgetScope::Principal, "owner", 100)]);
        let ids = resolver
            .resolve("owner", "presto", "create_task", "usd")
            .await;
        assert_eq!(ids, vec!["cfg-principal-owner-USD"]);
    }

    #[tokio::test]
    async fn wildcard_agent_row_issues_one_shared_token() {
        let resolver = resolver_with_rows(vec![BudgetRow {
            scope: BudgetScope::Agent,
            id: "*".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::from(10),
            period: CeilingPeriod::Daily,
            carryover: crate::magician_v2::resource_authority::token::CarryoverPolicy::None,
            system_ceiling_id: None,
        }]);
        let first = resolver
            .resolve("owner", "presto", "agentmail-send", "USD")
            .await;
        let second = resolver
            .resolve("owner", "bob", "kapso-whatsapp-send", "usd")
            .await;
        assert_eq!(first, vec!["cfg-agent-*-USD"]);
        assert_eq!(second, first);

        let ledger = resolver.ledger.read().await;
        let funding_entries: Vec<_> = ledger
            .journal
            .iter()
            .filter(|e| e.reference.starts_with("config_token_funding:"))
            .collect();
        assert_eq!(funding_entries.len(), 1);
        let store = resolver.token_store.read().await;
        let token = store.get("cfg-agent-*-USD").unwrap();
        assert_eq!(token.issued_to, "agent:*");
        assert_eq!(token.commodity, "USD");
    }

    #[tokio::test]
    async fn existing_token_picks_up_a_yaml_ceiling_edit() {
        let ledger = Arc::new(RwLock::new(ResourceLedger::new()));
        let store = Arc::new(RwLock::new(TokenStore::new()));
        let first = ConfigSpendTokenResolver::new(
            ResourceAuthorityConfig {
                enabled: true,
                system_ceilings: vec![],
                budgets: vec![budget_row(BudgetScope::Agent, "*", 10)],
            },
            Arc::clone(&ledger),
            Arc::clone(&store),
        );
        let ids = first
            .resolve("owner", "presto", "agentmail-send", "USD")
            .await;
        assert_eq!(ids, vec!["cfg-agent-*-USD"]);
        assert_eq!(
            store.read().await.get("cfg-agent-*-USD").unwrap().ceiling,
            Decimal::from(10)
        );

        let second = ConfigSpendTokenResolver::new(
            ResourceAuthorityConfig {
                enabled: true,
                system_ceilings: vec![],
                budgets: vec![BudgetRow {
                    scope: BudgetScope::Agent,
                    id: "*".to_string(),
                    commodity: "USD".to_string(),
                    ceiling: Decimal::from(4),
                    period: CeilingPeriod::Daily,
                    carryover: CarryoverPolicy::None,
                    system_ceiling_id: None,
                }],
            },
            Arc::clone(&ledger),
            Arc::clone(&store),
        );
        let ids = second
            .resolve("owner", "presto", "agentmail-send", "USD")
            .await;
        assert_eq!(ids, vec!["cfg-agent-*-USD"]);
        let token = store.read().await.get("cfg-agent-*-USD").unwrap().clone();
        assert_eq!(token.ceiling, Decimal::from(4));
        assert_eq!(token.period, CeilingPeriod::Daily);
        assert_eq!(
            ledger
                .read()
                .await
                .journal
                .iter()
                .filter(|entry| entry.reference.starts_with("config_token_funding:"))
                .count(),
            1,
            "a YAML ceiling edit must not mint a second funding entry"
        );
    }

    #[tokio::test]
    async fn exact_agent_row_is_preferred_over_wildcard() {
        let resolver = resolver_with_rows(vec![
            BudgetRow {
                scope: BudgetScope::Agent,
                id: "*".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::from(10),
                period: CeilingPeriod::Daily,
                carryover: crate::magician_v2::resource_authority::token::CarryoverPolicy::None,
                system_ceiling_id: None,
            },
            budget_row(BudgetScope::Agent, "presto", 4),
        ]);
        let ids = resolver
            .resolve("owner", "presto", "create_task", "USD")
            .await;
        assert_eq!(ids, vec!["cfg-agent-presto-USD"]);
        let other = resolver
            .resolve("owner", "bob", "create_task", "USD")
            .await;
        assert_eq!(other, vec!["cfg-agent-*-USD"]);
    }
}
