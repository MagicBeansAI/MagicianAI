//! Single spend writer: resolve tokens, reserve, persist, then commit/rollback.
//!
//! Chat dispatch (`execute_maybe_gated`), MCP commerce checkout, and the REST
//! reservation bridge all call [`admit`] instead of talking to the ledger
//! themselves. Policy differences stay on [`SpendIntent`]: fail-open vs
//! fail-closed when no budget row exists, and owner-chain vs active-token
//! authorization.

use std::sync::atomic::{AtomicBool, Ordering};

use rust_decimal::Decimal;
use tracing::warn;

use super::gate::{
    commit_spend_group, reserve_active_stack, reserve_authorized_stack, rollback_spend_group,
    BudgetRejection,
};
use super::scoped_authority::{ScopedAuthorityBundle, ScopedAuthorityResolver};
use super::spend_token_resolver::owner_authorization_chain;
use super::types::{canonicalize_commodity, ReservationId};

/// What to do when `resource_authority.enabled` is false or no budget row
/// matches the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingBudgetPolicy {
    /// Run the inner action without a reservation (dispatch `spend:` tools).
    Uncounted,
    /// Reject the call (commerce checkout, REST reserve).
    Reject,
}

/// How the reserved tokens must authorize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendOwnerPolicy {
    /// Dispatch / commerce: token `issued_to` must match `active_owner` or the
    /// principal/agent/tool chain (including `{scope}:*` wildcards).
    Authorized { active_owner: String },
    /// REST operator bridge: any active token for the commodity.
    ActiveOnly,
}

/// One attempted spend against the per-(principal, workspace) ledger.
#[derive(Debug, Clone)]
pub struct SpendIntent {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub tool_name: String,
    pub commodity: String,
    pub amount: Decimal,
    pub missing_budget: MissingBudgetPolicy,
    pub owner: SpendOwnerPolicy,
}

/// Result of [`admit`]: either nothing to debit, or a live reservation.
pub enum SpendAdmission {
    Uncounted,
    Reserved(SpendHold),
}

/// A reservation that has been persisted and must be committed or rolled back.
pub struct SpendHold {
    bundle: ScopedAuthorityBundle,
    head_id: ReservationId,
    /// Set by commit, rollback, or persist-for-recovery so Drop does not
    /// treat a deliberate handoff as a leaked reservation.
    released: AtomicBool,
}

#[derive(Debug, thiserror::Error)]
pub enum SpendSessionError {
    #[error("resource authority is disabled")]
    Disabled,
    #[error("no budget configured authorizing tool `{tool}` to spend `{commodity}`")]
    NoBudget { tool: String, commodity: String },
    #[error("budget gate: {0}")]
    Gate(String),
}

impl SpendHold {
    pub fn reservation_id(&self) -> &ReservationId {
        &self.head_id
    }

    pub async fn commit(self, actual: Option<Decimal>) -> Result<(), SpendSessionError> {
        {
            let mut ledger = self.bundle.ledger.write().await;
            commit_spend_group(&mut ledger, &self.head_id, actual)
                .map_err(|error| SpendSessionError::Gate(error.to_string()))?;
        }
        self.bundle.persist_state().await;
        self.released.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub async fn rollback(self) -> Result<(), SpendSessionError> {
        {
            let mut ledger = self.bundle.ledger.write().await;
            rollback_spend_group(&mut ledger, &self.head_id)
                .map_err(|error| SpendSessionError::Gate(error.to_string()))?;
        }
        self.bundle.persist_state().await;
        self.released.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub async fn persist(&self) {
        self.bundle.persist_state().await;
        self.released.store(true, Ordering::Relaxed);
    }
}

impl Drop for SpendHold {
    fn drop(&mut self) {
        if !self.released.load(Ordering::Relaxed) {
            tracing::error!(
                reservation = %self.head_id,
                "resource_authority: SpendHold dropped without commit or rollback; \
                 the reservation stays in the ledger until recovery"
            );
        }
    }
}

/// Resolve tokens and reserve. The only function that should open a spend
/// reservation from dispatch, commerce, or REST.
pub async fn admit(
    resolver: &dyn ScopedAuthorityResolver,
    intent: SpendIntent,
) -> Result<SpendAdmission, SpendSessionError> {
    if !resolver.is_enabled() {
        return match intent.missing_budget {
            MissingBudgetPolicy::Uncounted => Ok(SpendAdmission::Uncounted),
            MissingBudgetPolicy::Reject => Err(SpendSessionError::Disabled),
        };
    }

    let commodity = canonicalize_commodity(&intent.commodity);
    let bundle = resolver
        .resolve_scope(&intent.principal, &intent.workspace)
        .await;
    let token_ids = bundle
        .resolver
        .resolve(
            &intent.principal,
            &intent.agent_id,
            &intent.tool_name,
            &commodity,
        )
        .await;
    if token_ids.is_empty() {
        bundle.persist_state().await;
        return match intent.missing_budget {
            MissingBudgetPolicy::Uncounted => {
                warn!(
                    tool = %intent.tool_name,
                    commodity = %commodity,
                    principal = %intent.principal,
                    agent = %intent.agent_id,
                    "resource_authority: enabled but no budget row for this commodity — running uncounted (fail-open)"
                );
                Ok(SpendAdmission::Uncounted)
            },
            MissingBudgetPolicy::Reject => Err(SpendSessionError::NoBudget {
                tool: intent.tool_name,
                commodity,
            }),
        };
    }

    // A budget matched. Zero/negative is a lowering bug (missing
    // `cost_parameter`, empty checkout amount) — not a free debit.
    if intent.amount <= Decimal::ZERO {
        bundle.persist_state().await;
        return Err(SpendSessionError::Gate(format!(
            "spend amount must be positive (got {} {})",
            intent.amount, commodity
        )));
    }

    let head_id = reserve_on_bundle(&bundle, &token_ids, &commodity, &intent).await;
    bundle.persist_state().await;
    let head_id = head_id?;
    Ok(SpendAdmission::Reserved(SpendHold {
        bundle,
        head_id,
        released: AtomicBool::new(false),
    }))
}

async fn reserve_on_bundle(
    bundle: &ScopedAuthorityBundle,
    token_ids: &[String],
    commodity: &str,
    intent: &SpendIntent,
) -> Result<ReservationId, SpendSessionError> {
    let mut ledger = bundle.ledger.write().await;
    let mut token_store = bundle.token_store.write().await;
    let freeze = bundle.system_freeze.read().await;
    let ceilings = bundle.system_ceilings.read().await;
    let ids = match &intent.owner {
        SpendOwnerPolicy::Authorized { active_owner } => {
            let chain =
                owner_authorization_chain(&intent.principal, &intent.agent_id, &intent.tool_name);
            reserve_authorized_stack(
                &freeze,
                &mut ledger,
                &mut token_store,
                token_ids,
                commodity,
                intent.amount,
                active_owner,
                &chain,
                &ceilings,
            )
        },
        SpendOwnerPolicy::ActiveOnly => reserve_active_stack(
            &freeze,
            &mut ledger,
            &mut token_store,
            token_ids,
            commodity,
            intent.amount,
            &intent.agent_id,
            &ceilings,
        ),
    };
    ids.and_then(|ids| {
        ids.into_iter()
            .next()
            .ok_or_else(|| BudgetRejection::NoActiveTokenForCommodity(commodity.to_string()))
    })
    .map_err(|error| SpendSessionError::Gate(error.to_string()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::resource_authority::config::{
        BudgetRow, BudgetScope, ResourceAuthorityConfig,
    };
    use crate::magician_v2::resource_authority::scoped_authority::DiskBackedScopedResolver;
    use crate::magician_v2::resource_authority::token::{CarryoverPolicy, CeilingPeriod};
    use std::sync::Arc;

    fn usd_principal_config() -> ResourceAuthorityConfig {
        ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![BudgetRow {
                scope: BudgetScope::Principal,
                id: "alice".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::from(10),
                period: CeilingPeriod::Daily,
                carryover: CarryoverPolicy::None,
                system_ceiling_id: None,
            }],
        }
    }

    fn intent(amount: Decimal, missing: MissingBudgetPolicy) -> SpendIntent {
        SpendIntent {
            principal: "alice".to_string(),
            workspace: "default".to_string(),
            agent_id: "presto".to_string(),
            tool_name: "web_answer".to_string(),
            commodity: "USD".to_string(),
            amount,
            missing_budget: missing,
            owner: SpendOwnerPolicy::Authorized {
                active_owner: "agent:presto".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn zero_amount_with_a_matching_budget_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            usd_principal_config(),
            ArtifactV2Workspace::new(tmp.path()),
        ));
        let err = match admit(
            resolver.as_ref(),
            intent(Decimal::ZERO, MissingBudgetPolicy::Reject),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("zero debit against a live budget must fail closed"),
        };
        assert!(
            matches!(err, SpendSessionError::Gate(ref message) if message.contains("positive")),
            "got {err:?}"
        );
        let err = match admit(
            resolver.as_ref(),
            intent(Decimal::ZERO, MissingBudgetPolicy::Uncounted),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("zero debit against a live budget must not run uncounted"),
        };
        assert!(matches!(err, SpendSessionError::Gate(_)));
    }

    #[tokio::test]
    async fn zero_amount_without_a_budget_still_runs_uncounted() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver: Arc<dyn ScopedAuthorityResolver> = Arc::new(DiskBackedScopedResolver::new(
            ResourceAuthorityConfig {
                enabled: true,
                system_ceilings: vec![],
                budgets: vec![],
            },
            ArtifactV2Workspace::new(tmp.path()),
        ));
        let admission = admit(
            resolver.as_ref(),
            intent(Decimal::ZERO, MissingBudgetPolicy::Uncounted),
        )
        .await
        .expect("unbudgeted dispatch remains fail-open");
        assert!(matches!(admission, SpendAdmission::Uncounted));
    }
}
