//! Posts the model spend the spend envelope counts to resource authority's
//! ledger (`docs/plans/2026-10-26-model-spend-in-resource-authority.md`).
//!
//! The envelope stays the enforcer: it admits and settles every model call
//! in memory. Resource authority's reserve/commit forces a disk write per
//! entry, so it never sits on that path. Instead, every [`POST_EVERY`] (and
//! at shutdown) each scope's spend since the last post is recorded against
//! the model spend token [`MODEL_SPEND_TOKEN`] in that scope's ledger: one
//! reserve and one metered commit, so the ledger, its audit and the `/budget`
//! dashboard see model spend beside the action limits.
//!
//! The token carries the window limit (`resource_authority.model_spend`) as
//! its ceiling and the window as its period, refilled by resource
//! authority's own period funding. Ledger periods are UTC windows; the
//! envelope's `reset_offset_minutes` shifts only the enforced window, so near
//! a shifted boundary the two can attribute a batch to adjacent days. The
//! envelope is the figure that is enforced.
//!
//! A post that cannot be recorded returns its amount to the envelope, which
//! keeps it in its checkpoint and posts it later. A crash between a post and
//! the envelope's next checkpoint (five seconds) can post one batch twice or
//! lose it; the envelope's own count, which is enforced, is unaffected.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use rust_decimal::Decimal;

use crate::magician_v2::resource_authority::gate::ensure_period_funding;
use crate::magician_v2::resource_authority::ledger::{JournalEntry, Reservation};
use crate::magician_v2::resource_authority::scoped_authority::{
    ScopedAuthorityBundle, ScopedAuthorityResolver,
};
use crate::magician_v2::resource_authority::token::{
    period_window_start_at, CarryoverPolicy, CeilingPeriod, SpendToken, TokenStatus,
};
use crate::magician_v2::resource_authority::types::ReservationId;

/// The ledger token model spend is recorded against, in every scope.
pub const MODEL_SPEND_TOKEN: &str = "cfg-model-window-USD";
const COMMODITY: &str = "USD";
/// Who the model spend token is issued to, as the dashboard shows it.
const ISSUED_TO: &str = "model:*";
const ISSUER: &str = "system-config";
/// How often model spend is posted.
pub const POST_EVERY: Duration = Duration::from_secs(15 * 60);

static RESOLVER: OnceLock<Arc<dyn ScopedAuthorityResolver>> = OnceLock::new();

/// Attach the process's resource authority and start posting. Call once,
/// after the spend envelope is installed; later calls do nothing.
pub fn install(resolver: Arc<dyn ScopedAuthorityResolver>) {
    if RESOLVER.set(resolver).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let mut interval = tokio::time::interval(POST_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires at once; nothing is pending then.
        interval.tick().await;
        loop {
            interval.tick().await;
            post_pending().await;
        }
    });
}

/// Post every scope's pending model spend now (the ticker, and shutdown
/// before the envelope's final checkpoint).
pub async fn post_pending() {
    let (Some(envelope), Some(resolver)) = (
        crate::magician_v2::spend_envelope::envelope(),
        RESOLVER.get(),
    ) else {
        return;
    };
    let (owner_principal, owner_workspace) = envelope.owner_scope();
    let mut by_scope: HashMap<(String, String), (u64, Vec<(String, u64)>)> = HashMap::new();
    for (key, micro) in envelope.take_unposted() {
        // Past the envelope's scope cap, spend is pooled as "other": it is
        // recorded in the owner's scope.
        let scope = key
            .split_once('/')
            .filter(|(principal, workspace)| !principal.is_empty() && !workspace.is_empty())
            .map(|(principal, workspace)| (principal.to_string(), workspace.to_string()))
            .unwrap_or_else(|| (owner_principal.clone(), owner_workspace.clone()));
        let entry = by_scope.entry(scope).or_default();
        entry.0 = entry.0.saturating_add(micro);
        entry.1.push((key, micro));
    }
    for ((principal, workspace), (micro, keys)) in by_scope {
        // Only a live scope gets a ledger: never one queued for purge (posting
        // would bring its folder back) or a sink. With resource authority off,
        // nothing is posted either; the envelope still enforces.
        if !resolver.is_enabled() {
            continue;
        }
        let bundle = resolver.resolve_scope(&principal, &workspace).await;
        match scope_standing_of(&bundle, &principal, &workspace) {
            crate::magician_v2::auth::workspace_registry::ScopeStanding::Live => {},
            // The registry could not be read: keep the spend for next time.
            crate::magician_v2::auth::workspace_registry::ScopeStanding::Unknown => {
                for (key, micro) in keys {
                    envelope.return_unposted(&key, micro);
                }
                continue;
            },
            _ => {
                tracing::debug!(%principal, %workspace, micro, "[SPEND-ENVELOPE] model spend not posted: not a live scope");
                continue;
            },
        }
        match post_scope(&bundle, micro).await {
            Ok(()) => tracing::debug!(
                %principal,
                %workspace,
                micro,
                "[SPEND-ENVELOPE] model spend posted to the ledger"
            ),
            Err(error) => {
                for (key, micro) in keys {
                    envelope.return_unposted(&key, micro);
                }
                tracing::warn!(
                    %principal,
                    %workspace,
                    %error,
                    "[SPEND-ENVELOPE] model spend not posted to the ledger; kept for the next post"
                );
            },
        }
    }
}

/// Record a top-up of the model spend window in the owner's ledger, so its
/// remaining matches the envelope's.
pub async fn credit_grant(add_cost_usd: f64) {
    let (Some(envelope), Some(resolver)) = (
        crate::magician_v2::spend_envelope::envelope(),
        RESOLVER.get(),
    ) else {
        return;
    };
    let Some(amount) = Decimal::from_f64_retain(add_cost_usd)
        .map(|amount| amount.round_dp(6))
        .filter(|amount| *amount > Decimal::ZERO)
    else {
        return;
    };
    let (principal, workspace) = envelope.owner_scope();
    if !resolver.is_enabled() {
        return;
    }
    let bundle = resolver.resolve_scope(&principal, &workspace).await;
    if !scope_is_live(&bundle, &principal, &workspace) {
        return;
    }
    let result = async {
        refuse_damaged(&bundle)?;
        let mut ledger = bundle.ledger.write().await;
        let mut store = bundle.token_store.write().await;
        ensure_model_token(&mut ledger, &mut store)?;
        // Into this period, never the last one: a refill a rollover then
        // claws back would lose the top-up.
        ensure_period_funding(&mut ledger, &mut store, MODEL_SPEND_TOKEN)
            .map_err(|error| format!("period funding: {error:?}"))?;
        ledger
            .record(JournalEntry::period_refill(
                MODEL_SPEND_TOKEN,
                COMMODITY,
                amount,
            ))
            .map_err(|error| format!("ledger record: {error}"))
    }
    .await;
    match result {
        Ok(()) => bundle.persist_state().await,
        Err(error) => tracing::warn!(%error, "[SPEND-ENVELOPE] top-up not recorded in the ledger"),
    }
}

/// Where `principal/workspace` stands in the resolver's own storage layout.
fn scope_standing_of(
    bundle: &ScopedAuthorityBundle,
    principal: &str,
    workspace: &str,
) -> crate::magician_v2::auth::workspace_registry::ScopeStanding {
    crate::magician_v2::auth::workspace_registry::scope_standing(
        &bundle.workspace_layout.scopes_root(),
        principal,
        workspace,
    )
}

/// Whether `principal/workspace` is a live scope (not a sink, not queued for
/// purge).
fn scope_is_live(bundle: &ScopedAuthorityBundle, principal: &str, workspace: &str) -> bool {
    scope_standing_of(bundle, principal, workspace)
        == crate::magician_v2::auth::workspace_registry::ScopeStanding::Live
}

fn refuse_damaged(bundle: &ScopedAuthorityBundle) -> Result<(), String> {
    match &bundle.ledger_load_failure {
        // Never write over a journal that would not replay.
        Some(failure) => Err(format!("the scope's ledger did not load: {failure}")),
        None => Ok(()),
    }
}

async fn post_scope(bundle: &ScopedAuthorityBundle, micro: u64) -> Result<(), String> {
    refuse_damaged(bundle)?;
    let amount = Decimal::from(micro) / Decimal::from(1_000_000u64);
    if amount <= Decimal::ZERO {
        return Ok(());
    }
    {
        let mut ledger = bundle.ledger.write().await;
        let mut store = bundle.token_store.write().await;
        ensure_model_token(&mut ledger, &mut store)?;
        ensure_period_funding(&mut ledger, &mut store, MODEL_SPEND_TOKEN)
            .map_err(|error| format!("period funding: {error:?}"))?;
        let reservation = Reservation {
            id: ReservationId::new(),
            token_id: MODEL_SPEND_TOKEN.to_string(),
            commodity: COMMODITY.to_string(),
            amount,
            agent_id: ISSUER.to_string(),
            created_at: Utc::now(),
            idempotency_key: format!("model-spend:{}", uuid::Uuid::new_v4()),
            max_duration_secs: 0,
            batch_id: None,
        };
        // Recorded after the fact: the envelope admitted it (chat is never
        // refused). Spend past the budget is booked to the overspend account
        // first, so the budget never goes below zero and a rollover's refill
        // is not eaten by yesterday's overrun.
        let budget_account = format!("token:{MODEL_SPEND_TOKEN}:budget:{COMMODITY}");
        let deficit = amount - ledger.balance(&budget_account).max(Decimal::ZERO);
        if deficit > Decimal::ZERO {
            ledger
                .record(overspend_entry(deficit))
                .map_err(|error| format!("ledger overspend: {error}"))?;
        }
        ledger
            .record(JournalEntry::reserve(
                MODEL_SPEND_TOKEN,
                COMMODITY,
                amount,
                &reservation,
            ))
            .map_err(|error| format!("ledger reserve: {error}"))?;
        ledger
            .record(JournalEntry::commit_with_amount(&reservation, amount))
            .map_err(|error| format!("ledger commit: {error}"))?;
    }
    bundle.persist_state().await;
    Ok(())
}

/// The model spend token, issued on first use and kept in step with the
/// limits in force (`resource_authority.model_spend`): its ceiling is the
/// window limit (zero for unlimited — spend is recorded, nothing refills)
/// and its period the window.
fn ensure_model_token(
    ledger: &mut crate::magician_v2::resource_authority::ledger::ResourceLedger,
    store: &mut crate::magician_v2::resource_authority::token_store::TokenStore,
) -> Result<(), String> {
    let settings = crate::config::execution_budget_settings().spend_envelope;
    let (ceiling, period) = match settings.daily_usd.as_cap() {
        Some(usd) => (
            Decimal::from_f64_retain(usd)
                .map(|ceiling| ceiling.round_dp(6))
                .unwrap_or(Decimal::ZERO),
            crate::magician_v2::spend_envelope::period_of(settings.window),
        ),
        None => (Decimal::ZERO, CeilingPeriod::Total),
    };
    if let Ok(token) = store.get_mut(MODEL_SPEND_TOKEN) {
        if token.ceiling == ceiling && token.period == period {
            return Ok(());
        }
        // A changed limit applies to this period now: the budget moves by
        // the difference, and a new period starts counting from its own
        // window (no extra refill on the next post).
        let budget_account = format!("token:{MODEL_SPEND_TOKEN}:budget:{COMMODITY}");
        if token.period != period {
            // A new window: the old one's leftover goes back and the new
            // window starts full, as a rollover would.
            let leftover = ledger.balance(&budget_account).max(Decimal::ZERO);
            if leftover > Decimal::ZERO {
                ledger
                    .record(JournalEntry::period_clawback(
                        MODEL_SPEND_TOKEN,
                        COMMODITY,
                        leftover,
                    ))
                    .map_err(|error| format!("ledger clawback: {error}"))?;
            }
            if ceiling > Decimal::ZERO {
                ledger
                    .record(JournalEntry::period_refill(
                        MODEL_SPEND_TOKEN,
                        COMMODITY,
                        ceiling,
                    ))
                    .map_err(|error| format!("ledger refill: {error}"))?;
            }
            token.last_period_start = match period {
                CeilingPeriod::Total => None,
                _ => Some(period_window_start_at(&period, Utc::now())),
            };
            token.ceiling = ceiling;
            token.period = period;
            return Ok(());
        }
        let delta = ceiling - token.ceiling;
        token.ceiling = ceiling;
        token.period = period;
        if delta > Decimal::ZERO {
            ledger
                .record(JournalEntry::period_refill(
                    MODEL_SPEND_TOKEN,
                    COMMODITY,
                    delta,
                ))
                .map_err(|error| format!("ledger refill: {error}"))?;
        } else if delta < Decimal::ZERO {
            let budget = ledger.balance(&budget_account).max(Decimal::ZERO);
            let clawback = (-delta).min(budget);
            if clawback > Decimal::ZERO {
                ledger
                    .record(JournalEntry::period_clawback(
                        MODEL_SPEND_TOKEN,
                        COMMODITY,
                        clawback,
                    ))
                    .map_err(|error| format!("ledger clawback: {error}"))?;
            }
        }
        return Ok(());
    }
    let now = Utc::now();
    let last_period_start = match period {
        CeilingPeriod::Total => None,
        _ => Some(period_window_start_at(&period, now)),
    };
    let token = SpendToken {
        id: MODEL_SPEND_TOKEN.to_string(),
        issued_by: ISSUER.to_string(),
        issued_to: ISSUED_TO.to_string(),
        commodity: COMMODITY.to_string(),
        ceiling,
        period,
        carryover: CarryoverPolicy::None,
        conditions: Vec::new(),
        velocity_limit: None,
        status: TokenStatus::Active,
        expires_at: None,
        created_at: now,
        system_ceiling_id: None,
        last_period_start,
    };
    if ceiling > Decimal::ZERO {
        ledger
            .record(
                crate::magician_v2::resource_authority::spend_token_resolver::config_token_funding_entry(
                    MODEL_SPEND_TOKEN,
                    COMMODITY,
                    ceiling,
                ),
            )
            .map_err(|error| format!("ledger funding: {error}"))?;
    }
    store
        .create(token)
        .map_err(|error| format!("token store create: {error}"))
}

/// Book spend past the model spend budget: the budget is funded by the
/// overspend account, which then reads what was spent over the limit.
fn overspend_entry(amount: Decimal) -> JournalEntry {
    use crate::magician_v2::resource_authority::ledger::{LedgerEntry, ResourceAmount};
    let now = Utc::now();
    JournalEntry {
        id: uuid::Uuid::new_v4(),
        timestamp: now,
        accrual_date: now,
        entries: vec![
            LedgerEntry {
                account: format!("token:{MODEL_SPEND_TOKEN}:budget:{COMMODITY}"),
                amount: ResourceAmount {
                    value: amount,
                    commodity: COMMODITY.to_string(),
                },
            },
            LedgerEntry {
                account: format!("system:model_overspend:{COMMODITY}"),
                amount: ResourceAmount {
                    value: -amount,
                    commodity: COMMODITY.to_string(),
                },
            },
        ],
        reference: format!("model_overspend:{MODEL_SPEND_TOKEN}"),
        agent_id: ISSUER.to_string(),
        metadata: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::resource_authority::ledger::ResourceLedger;
    use crate::magician_v2::resource_authority::token_store::TokenStore;

    fn spend(ledger: &mut ResourceLedger, token_id: &str, commodity: &str, amount: i64) {
        let amount = Decimal::from(amount);
        let reservation = Reservation {
            id: ReservationId::new(),
            token_id: token_id.to_string(),
            commodity: commodity.to_string(),
            amount,
            agent_id: ISSUER.to_string(),
            created_at: Utc::now(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            max_duration_secs: 0,
            batch_id: None,
        };
        ledger
            .record(JournalEntry::reserve(
                token_id,
                commodity,
                amount,
                &reservation,
            ))
            .unwrap();
        ledger
            .record(JournalEntry::commit_with_amount(&reservation, amount))
            .unwrap();
    }

    /// System ceilings count action spend, never model spend, and only in the
    /// commodity being summed.
    #[test]
    fn system_ceilings_leave_model_spend_out() {
        let mut ledger = ResourceLedger::new();
        let since = Utc::now() - chrono::Duration::hours(1);
        spend(&mut ledger, MODEL_SPEND_TOKEN, "USD", 300);
        spend(&mut ledger, "cfg-tool-paid-api-USD", "USD", 7);
        spend(&mut ledger, "cfg-tool-zepto-mcp-INR", "INR", 900);
        assert_eq!(ledger.total_spent_since("USD", since), Decimal::from(307));
        assert_eq!(
            ledger.ceiling_spent_since_until("USD", since, None),
            Decimal::from(7)
        );
        assert_eq!(
            ledger.ceiling_spent_since_until("INR", since, None),
            Decimal::from(900)
        );
        assert_eq!(
            ledger.ceiling_spent_since_until("EMAIL_SENDS", since, None),
            Decimal::ZERO
        );
    }

    #[test]
    fn the_model_token_follows_the_window_limit() {
        let _budgets = crate::config::execution_budget_test_lock();
        let previous = crate::config::execution_budget_settings();
        let mut settings = previous;
        settings.spend_envelope.daily_usd = crate::config::BudgetCapUsd::Usd(250.0);
        crate::config::configure_execution_budgets(&settings);

        let mut ledger = ResourceLedger::new();
        let mut store = TokenStore::new();
        ensure_model_token(&mut ledger, &mut store).unwrap();
        let token = store.get(MODEL_SPEND_TOKEN).unwrap();
        assert_eq!(token.ceiling, Decimal::from(250));
        assert_eq!(token.period, CeilingPeriod::Daily);
        assert_eq!(token.issued_to, ISSUED_TO);
        assert_eq!(ledger.balance(&token.budget_account()), Decimal::from(250));

        // A raised limit tops this period's budget up by the difference.
        settings.spend_envelope.daily_usd = crate::config::BudgetCapUsd::Usd(300.0);
        crate::config::configure_execution_budgets(&settings);
        ensure_model_token(&mut ledger, &mut store).unwrap();
        let token = store.get(MODEL_SPEND_TOKEN).unwrap().clone();
        assert_eq!(ledger.balance(&token.budget_account()), Decimal::from(300));

        // Spend past the budget is booked as overspend; the budget stays at zero.
        let over = Decimal::from(320) - ledger.balance(&token.budget_account());
        ledger.record(overspend_entry(over)).unwrap();
        assert_eq!(ledger.balance(&token.budget_account()), Decimal::from(320));
        assert_eq!(
            ledger.balance(&"system:model_overspend:USD".to_string()),
            Decimal::from(-20)
        );

        // An edited limit reaches the token; unlimited records spend only.
        settings.spend_envelope.daily_usd = crate::config::BudgetCapUsd::Unlimited;
        crate::config::configure_execution_budgets(&settings);
        ensure_model_token(&mut ledger, &mut store).unwrap();
        let token = store.get(MODEL_SPEND_TOKEN).unwrap();
        assert_eq!(token.ceiling, Decimal::ZERO);
        assert_eq!(token.period, CeilingPeriod::Total);

        crate::config::configure_execution_budgets(&previous);
    }
}
