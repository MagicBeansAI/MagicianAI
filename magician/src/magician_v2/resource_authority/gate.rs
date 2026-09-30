//! Hard gate: reserve/commit/rollback with system freeze, ceiling enforcement,
//! velocity limits, and period-aware carryover.
//!
//! Lock ordering (documented per design doc): ledger first, then token_store.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::ledger::{JournalEntry, LedgerError, Reservation, ResourceLedger};
use super::token::{
    period_window_start, period_window_start_at, previous_period_window, CarryoverPolicy,
    CeilingPeriod, SpendToken, SystemCeiling, TokenStatus,
};
use super::token_store::{TokenStore, TokenStoreError};
use super::types::{
    canonicalize_commodity, commodities_eq, wildcard_issued_to_covers, ReservationId,
};

// ---------------------------------------------------------------------------
// System Freeze
// ---------------------------------------------------------------------------

/// System-wide emergency halt. When frozen, ALL `reserve_spend` calls fail immediately.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemFreezeState {
    pub frozen: bool,
    pub frozen_at: Option<DateTime<Utc>>,
    pub frozen_by: Option<String>,
    pub reason: Option<String>,
}

impl SystemFreezeState {
    pub fn freeze(&mut self, by: &str, reason: &str) {
        self.frozen = true;
        self.frozen_at = Some(Utc::now());
        self.frozen_by = Some(by.to_string());
        self.reason = Some(reason.to_string());
    }

    pub fn unfreeze(&mut self) {
        self.frozen = false;
        self.frozen_at = None;
        self.frozen_by = None;
        self.reason = None;
    }
}

// ---------------------------------------------------------------------------
// Budget Rejection
// ---------------------------------------------------------------------------

/// All possible reasons a reserve_spend can fail.
#[derive(Debug, Clone)]
pub enum BudgetRejection {
    SystemFrozen {
        frozen_at: Option<DateTime<Utc>>,
        reason: Option<String>,
    },
    TokenInactive(String),
    TokenExpired(String),
    TokenExceeded {
        token_id: String,
        remaining: Decimal,
        proposed: Decimal,
    },
    SystemCeilingExceeded {
        hard_ceiling: Decimal,
        period: CeilingPeriod,
        spent_in_period: Decimal,
        reserved_in_period: Decimal,
        proposed: Decimal,
    },
    VelocityExceeded {
        token_id: String,
        max_amount: Decimal,
        window_seconds: u64,
        recent_spend: Decimal,
        proposed: Decimal,
    },
    UnauthorizedSpender {
        token_id: String,
        issued_to: String,
        active_owner_agent_id: String,
    },
    NoActiveTokenForCommodity(String),
    LedgerError(LedgerError),
    TokenStoreError(TokenStoreError),
}

impl std::fmt::Display for BudgetRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetRejection::SystemFrozen { reason, .. } => {
                write!(
                    f,
                    "System frozen: {}",
                    reason.as_deref().unwrap_or("no reason")
                )
            },
            BudgetRejection::TokenInactive(id) => write!(f, "Token inactive: {}", id),
            BudgetRejection::TokenExpired(id) => write!(f, "Token expired: {}", id),
            BudgetRejection::TokenExceeded {
                token_id,
                remaining,
                proposed,
            } => {
                write!(
                    f,
                    "Token {} exceeded: remaining={}, proposed={}",
                    token_id, remaining, proposed
                )
            },
            BudgetRejection::SystemCeilingExceeded {
                hard_ceiling,
                spent_in_period,
                reserved_in_period,
                proposed,
                ..
            } => {
                write!(
                    f,
                    "System ceiling exceeded: ceiling={}, spent={}, reserved={}, proposed={}",
                    hard_ceiling, spent_in_period, reserved_in_period, proposed
                )
            },
            BudgetRejection::VelocityExceeded {
                token_id,
                max_amount,
                recent_spend,
                proposed,
                ..
            } => {
                write!(
                    f,
                    "Velocity exceeded for token {}: max={}, recent={}, proposed={}",
                    token_id, max_amount, recent_spend, proposed
                )
            },
            BudgetRejection::UnauthorizedSpender {
                token_id,
                issued_to,
                active_owner_agent_id,
            } => {
                write!(
                    f,
                    "Token {} is issued to {}, not authorized for active owner {}",
                    token_id, issued_to, active_owner_agent_id
                )
            },
            BudgetRejection::NoActiveTokenForCommodity(c) => {
                write!(f, "No active token for commodity: {}", c)
            },
            BudgetRejection::LedgerError(e) => write!(f, "Ledger error: {}", e),
            BudgetRejection::TokenStoreError(e) => write!(f, "Token store error: {}", e),
        }
    }
}

impl std::error::Error for BudgetRejection {}

impl From<LedgerError> for BudgetRejection {
    fn from(e: LedgerError) -> Self {
        BudgetRejection::LedgerError(e)
    }
}

impl From<TokenStoreError> for BudgetRejection {
    fn from(e: TokenStoreError) -> Self {
        BudgetRejection::TokenStoreError(e)
    }
}

// ---------------------------------------------------------------------------
// reserve_spend — the hard gate
// ---------------------------------------------------------------------------

fn token_holder_is_authorized(
    token: &SpendToken,
    active_owner_agent_id: &str,
    owner_authorization_chain: &[String],
) -> bool {
    token.issued_to == active_owner_agent_id
        || owner_authorization_chain
            .iter()
            .any(|owner_agent_id| owner_agent_id == &token.issued_to)
        || wildcard_issued_to_covers(&token.issued_to, active_owner_agent_id)
        || owner_authorization_chain
            .iter()
            .any(|owner_agent_id| wildcard_issued_to_covers(&token.issued_to, owner_agent_id))
}

/// Re-fund a periodic token when the current period window has advanced.
///
/// `CeilingPeriod::Total` is a lifetime pool and is not refilled. On rollover:
/// leftover is clawed back per carryover (`none` loses it, `capped` keeps up
/// to the cap, `full` keeps all), then `token.ceiling` is allocated again.
pub fn ensure_period_funding(
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    token_id: &str,
) -> Result<(), BudgetRejection> {
    let snapshot = token_store
        .get(token_id)
        .map_err(BudgetRejection::from)?
        .clone();
    if matches!(snapshot.period, CeilingPeriod::Total) {
        return Ok(());
    }
    let current_start = period_window_start(&snapshot.period);
    let last = snapshot
        .last_period_start
        .unwrap_or_else(|| period_window_start_at(&snapshot.period, snapshot.created_at));
    if last == current_start {
        if snapshot.last_period_start.is_none() {
            token_store
                .get_mut(token_id)
                .map_err(BudgetRejection::from)?
                .last_period_start = Some(current_start);
        }
        return Ok(());
    }

    let leftover = ledger
        .balance(&snapshot.budget_account())
        .max(Decimal::ZERO);
    let clawback = match &snapshot.carryover {
        CarryoverPolicy::None => leftover,
        CarryoverPolicy::Full => Decimal::ZERO,
        CarryoverPolicy::Capped { cap } => (leftover - *cap).max(Decimal::ZERO),
    };
    if clawback > Decimal::ZERO {
        ledger.record(JournalEntry::period_clawback(
            &snapshot.id,
            &snapshot.commodity,
            clawback,
        ))?;
    }
    if snapshot.ceiling > Decimal::ZERO {
        ledger.record(JournalEntry::period_refill(
            &snapshot.id,
            &snapshot.commodity,
            snapshot.ceiling,
        ))?;
    }
    token_store
        .get_mut(token_id)
        .map_err(BudgetRejection::from)?
        .last_period_start = Some(current_start);
    Ok(())
}

fn check_system_ceilings(
    ledger: &ResourceLedger,
    commodity: &str,
    proposed_amount: Decimal,
    system_ceilings: &[SystemCeiling],
) -> Result<(), BudgetRejection> {
    for sc in system_ceilings
        .iter()
        .filter(|sc| commodities_eq(&sc.commodity, commodity))
    {
        let window_start = period_window_start(&sc.period);
        let spent_in_period = ledger.total_spent_since(commodity, window_start);
        // In-flight holds occupy the live ceiling even when their reserve
        // journal row accrued in the previous period (midnight checkout).
        let reserved_in_flight = ledger.in_flight_reserved_for_commodity(commodity);
        let effective_ceiling =
            compute_effective_ceiling(sc.ceiling, &sc.carryover, &sc.period, |since, until| {
                ledger.total_spent_since_until(commodity, since, until)
            });
        let hard_ceiling = effective_ceiling * (Decimal::ONE + sc.relaxation);
        if spent_in_period + reserved_in_flight + proposed_amount > hard_ceiling {
            return Err(BudgetRejection::SystemCeilingExceeded {
                hard_ceiling,
                period: sc.period.clone(),
                spent_in_period,
                reserved_in_period: reserved_in_flight,
                proposed: proposed_amount,
            });
        }
    }
    Ok(())
}

fn reserve_spend_impl(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    token: &SpendToken,
    system_ceilings: &[SystemCeiling],
    proposed_amount: Decimal,
    actor_agent_id: &str,
    authorization: Option<(&str, &[String])>,
    batch_id: Option<String>,
) -> Result<ReservationId, BudgetRejection> {
    // 0. System freeze check — absolute first, before everything
    if system_freeze.frozen {
        return Err(BudgetRejection::SystemFrozen {
            frozen_at: system_freeze.frozen_at,
            reason: system_freeze.reason.clone(),
        });
    }

    // 1. Token status check
    if token.status != TokenStatus::Active {
        return Err(BudgetRejection::TokenInactive(token.id.clone()));
    }

    // 2. Token expiry check
    if token.is_expired() {
        return Err(BudgetRejection::TokenExpired(token.id.clone()));
    }

    if let Some((active_owner_agent_id, owner_authorization_chain)) = authorization {
        if !token_holder_is_authorized(token, active_owner_agent_id, owner_authorization_chain) {
            return Err(BudgetRejection::UnauthorizedSpender {
                token_id: token.id.clone(),
                issued_to: token.issued_to.clone(),
                active_owner_agent_id: active_owner_agent_id.to_string(),
            });
        }
    }

    // 3. Budget balance check (simple: is there enough in the budget account?)
    let budget_balance = ledger.balance(&token.budget_account());
    if proposed_amount > budget_balance {
        return Err(BudgetRejection::TokenExceeded {
            token_id: token.id.clone(),
            remaining: budget_balance,
            proposed: proposed_amount,
        });
    }

    // 4. Check ALL system-wide ceilings for this commodity (once per
    // admit — stacked sibling tokens must not multiply the proposed amount).
    check_system_ceilings(ledger, &token.commodity, proposed_amount, system_ceilings)?;

    // 5. Check per-token ceiling (period-aware, carryover-aware)
    let token_window = period_window_start(&token.period);
    let token_spent = ledger.total_spent_for_token_since(&token.id, token_window);
    let token_reserved = ledger.balance(&token.reserved_account()).max(Decimal::ZERO);

    let effective_token_ceiling = compute_effective_ceiling(
        token.ceiling,
        &token.carryover,
        &token.period,
        |since, until| ledger.total_spent_for_token_since_until(&token.id, since, until),
    );

    if token_spent + token_reserved + proposed_amount > effective_token_ceiling {
        return Err(BudgetRejection::TokenExceeded {
            token_id: token.id.clone(),
            remaining: effective_token_ceiling - token_spent - token_reserved,
            proposed: proposed_amount,
        });
    }

    // 6. Velocity check (optional, per-token)
    if let Some(vel) = &token.velocity_limit {
        let vel_window = Utc::now() - Duration::seconds(vel.window_seconds as i64);
        let recent_spend = ledger.total_spent_for_token_since(&token.id, vel_window);
        let recent_reserved = ledger.balance(&token.reserved_account()).max(Decimal::ZERO);
        if recent_spend + recent_reserved + proposed_amount > vel.max_amount {
            return Err(BudgetRejection::VelocityExceeded {
                token_id: token.id.clone(),
                max_amount: vel.max_amount,
                window_seconds: vel.window_seconds,
                recent_spend: recent_spend + recent_reserved,
                proposed: proposed_amount,
            });
        }
    }

    // 7. Record the reservation
    let reservation_id = ReservationId::new();
    let reservation = Reservation {
        id: reservation_id.clone(),
        token_id: token.id.clone(),
        commodity: token.commodity.clone(),
        amount: proposed_amount,
        agent_id: actor_agent_id.to_string(),
        created_at: Utc::now(),
        idempotency_key: format!("{}:{}", reservation_id, Utc::now().timestamp()),
        max_duration_secs: 120,
        batch_id,
    };
    ledger.record(JournalEntry::reserve(
        &token.id,
        &token.commodity,
        proposed_amount,
        &reservation,
    ))?;
    ledger
        .active_reservations
        .insert(reservation.id.clone(), reservation);
    Ok(reservation_id)
}

/// Full check order: freeze -> token status -> expiry -> budget balance ->
/// system ceiling (period-aware, carryover-aware) -> token ceiling (period-aware,
/// carryover-aware) -> velocity (includes reservations) -> record reservation.
pub fn reserve_spend(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    token: &SpendToken,
    system_ceilings: &[SystemCeiling],
    proposed_amount: Decimal,
    agent_id: &str,
) -> Result<ReservationId, BudgetRejection> {
    reserve_spend_impl(
        system_freeze,
        ledger,
        token,
        system_ceilings,
        proposed_amount,
        agent_id,
        None,
        None,
    )
}

/// Owner-aware reserve path for execution-owned runtime. The active owner may
/// spend tokens issued directly to itself or to any suspended owner in the
/// inherited owner stack.
pub fn reserve_spend_with_authority(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    token: &SpendToken,
    system_ceilings: &[SystemCeiling],
    proposed_amount: Decimal,
    active_owner_agent_id: &str,
    owner_authorization_chain: &[String],
) -> Result<ReservationId, BudgetRejection> {
    reserve_spend_impl(
        system_freeze,
        ledger,
        token,
        system_ceilings,
        proposed_amount,
        active_owner_agent_id,
        Some((active_owner_agent_id, owner_authorization_chain)),
        None,
    )
}

/// Compute effective ceiling including carryover from previous period.
/// For CeilingPeriod::Total, carryover is meaningless (no "previous period") — returns base ceiling.
fn compute_effective_ceiling<F>(
    base_ceiling: Decimal,
    carryover: &CarryoverPolicy,
    period: &CeilingPeriod,
    spent_in_range: F,
) -> Decimal
where
    F: Fn(DateTime<Utc>, DateTime<Utc>) -> Decimal,
{
    // Total period has no previous period — carryover doesn't apply.
    if matches!(period, CeilingPeriod::Total) {
        return base_ceiling;
    }

    match carryover {
        CarryoverPolicy::None => base_ceiling,
        CarryoverPolicy::Full => {
            let (prev_start, prev_end) = previous_period_window(period);
            let spent_prev = spent_in_range(prev_start, prev_end);
            let unspent_prev = (base_ceiling - spent_prev).max(Decimal::ZERO);
            base_ceiling + unspent_prev
        },
        CarryoverPolicy::Capped { cap } => {
            let (prev_start, prev_end) = previous_period_window(period);
            let spent_prev = spent_in_range(prev_start, prev_end);
            let unspent_prev = (base_ceiling - spent_prev).max(Decimal::ZERO);
            base_ceiling + unspent_prev.min(*cap)
        },
    }
}

// ---------------------------------------------------------------------------
// commit_spend
// ---------------------------------------------------------------------------

/// Commit a reservation. `actual_cost`:
/// - None => committed (full reservation amount)
/// - Some(actual) <= reservation => commit actual, return delta to budget
/// - Some(actual) > reservation => commit full reservation + overage from budget
pub fn commit_spend(
    ledger: &mut ResourceLedger,
    reservation_id: &ReservationId,
    actual_cost: Option<Decimal>,
) -> Result<(), LedgerError> {
    let reservation = ledger
        .active_reservations
        .remove(reservation_id)
        .ok_or_else(|| LedgerError::ReservationNotFound(reservation_id.clone()))?;

    match actual_cost {
        None => {
            // Committed/Counted: commit full reserved amount
            ledger.record(JournalEntry::commit(&reservation))?;
        },
        Some(actual) if actual <= reservation.amount => {
            // Metered, actual <= estimate: commit actual, return delta
            ledger.record(JournalEntry::commit_with_amount(&reservation, actual))?;
            let delta = reservation.amount - actual;
            if delta > Decimal::ZERO {
                ledger.record(JournalEntry::return_delta(&reservation, delta))?;
            }
        },
        Some(actual) => {
            // Metered, actual > estimate: commit full reservation + overage from budget
            ledger.record(JournalEntry::commit(&reservation))?;
            let overage = actual - reservation.amount;
            ledger.record(JournalEntry::overage(&reservation, overage))?;
        },
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rollback_spend
// ---------------------------------------------------------------------------

/// Rollback a reservation — returns reserved funds to the budget account.
pub fn rollback_spend(
    ledger: &mut ResourceLedger,
    reservation_id: &ReservationId,
) -> Result<(), LedgerError> {
    let reservation = ledger
        .active_reservations
        .get(reservation_id)
        .ok_or_else(|| LedgerError::ReservationNotFound(reservation_id.clone()))?
        .clone();
    ledger.record(JournalEntry::rollback(&reservation))?;
    ledger.active_reservations.remove(reservation_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// find_active_token — with lazy expiry
// ---------------------------------------------------------------------------

/// Find an active token for a commodity from the given token IDs.
/// If an Active token is past its expiry, lazily transitions it to Expired
/// and records the revert journal entry (returning unspent budget to issuer).
///
/// Lock ordering: ledger first, then token_store.
pub fn find_active_token(
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
) -> Result<SpendToken, BudgetRejection> {
    let mut tokens = find_active_tokens(ledger, token_store, spend_token_ids, commodity)?;
    Ok(tokens.remove(0))
}

/// All active tokens for `commodity` among `spend_token_ids`, after lazy expiry
/// and period refill. Order follows `spend_token_ids`.
pub fn find_active_tokens(
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
) -> Result<Vec<SpendToken>, BudgetRejection> {
    let commodity = canonicalize_commodity(commodity);
    let mut found = Vec::new();
    for id in spend_token_ids {
        if let Ok(token) = token_store.get(id) {
            if commodities_eq(&token.commodity, &commodity) && token.status == TokenStatus::Active {
                if token.is_expired() {
                    // Lazy expiry: revert unspent budget, mark expired
                    let unspent = ledger.balance(&token.budget_account());
                    let issued_by = token.issued_by.clone();
                    let token_id = token.id.clone();
                    let token_commodity = token.commodity.clone();
                    if unspent > Decimal::ZERO {
                        ledger.record(JournalEntry::token_revert(
                            &issued_by,
                            &token_id,
                            &token_commodity,
                            unspent,
                        ))?;
                    }
                    token_store
                        .mark_expired(id)
                        .map_err(BudgetRejection::from)?;
                    continue;
                }
                ensure_period_funding(ledger, token_store, id)?;
                found.push(token_store.get(id).map_err(BudgetRejection::from)?.clone());
            }
        }
    }
    if found.is_empty() {
        return Err(BudgetRejection::NoActiveTokenForCommodity(commodity));
    }
    Ok(found)
}

/// Find an active token for a commodity that is authorized for the current
/// execution owner or any inherited owner in `owner_stack`.
pub fn find_authorized_active_token(
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
    active_owner_agent_id: &str,
    owner_authorization_chain: &[String],
) -> Result<SpendToken, BudgetRejection> {
    let mut tokens = find_authorized_active_tokens(
        ledger,
        token_store,
        spend_token_ids,
        commodity,
        active_owner_agent_id,
        owner_authorization_chain,
    )?;
    Ok(tokens.remove(0))
}

/// Every authorized active token for `commodity` among `spend_token_ids`.
/// Tokens are period-refilled before return. Empty / unauthorized uses the
/// same errors as [`find_authorized_active_token`].
pub fn find_authorized_active_tokens(
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
    active_owner_agent_id: &str,
    owner_authorization_chain: &[String],
) -> Result<Vec<SpendToken>, BudgetRejection> {
    let commodity = canonicalize_commodity(commodity);
    let mut authorized = Vec::new();
    let mut unauthorized_token: Option<SpendToken> = None;

    for id in spend_token_ids {
        if let Ok(token) = token_store.get(id) {
            if commodities_eq(&token.commodity, &commodity) && token.status == TokenStatus::Active {
                if token.is_expired() {
                    let unspent = ledger.balance(&token.budget_account());
                    let issued_by = token.issued_by.clone();
                    let token_id = token.id.clone();
                    let token_commodity = token.commodity.clone();
                    if unspent > Decimal::ZERO {
                        ledger.record(JournalEntry::token_revert(
                            &issued_by,
                            &token_id,
                            &token_commodity,
                            unspent,
                        ))?;
                    }
                    token_store
                        .mark_expired(id)
                        .map_err(BudgetRejection::from)?;
                    continue;
                }

                if token_holder_is_authorized(
                    token,
                    active_owner_agent_id,
                    owner_authorization_chain,
                ) {
                    ensure_period_funding(ledger, token_store, id)?;
                    authorized.push(token_store.get(id).map_err(BudgetRejection::from)?.clone());
                    continue;
                }

                if unauthorized_token.is_none() {
                    unauthorized_token = Some(token.clone());
                }
            }
        }
    }

    if !authorized.is_empty() {
        return Ok(authorized);
    }

    if let Some(token) = unauthorized_token {
        return Err(BudgetRejection::UnauthorizedSpender {
            token_id: token.id,
            issued_to: token.issued_to,
            active_owner_agent_id: active_owner_agent_id.to_string(),
        });
    }

    Err(BudgetRejection::NoActiveTokenForCommodity(commodity))
}

/// Reserve `amount` against every authorized token (principal + agent + tool
/// stack). On the first rejection, already-created reservations are rolled
/// back. Shared `batch_id` lets REST commit/rollback one member of the stack.
pub fn reserve_authorized_stack(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
    proposed_amount: Decimal,
    active_owner_agent_id: &str,
    owner_authorization_chain: &[String],
    system_ceilings: &[SystemCeiling],
) -> Result<Vec<ReservationId>, BudgetRejection> {
    let tokens = find_authorized_active_tokens(
        ledger,
        token_store,
        spend_token_ids,
        commodity,
        active_owner_agent_id,
        owner_authorization_chain,
    )?;
    reserve_token_stack(
        system_freeze,
        ledger,
        &tokens,
        system_ceilings,
        proposed_amount,
        active_owner_agent_id,
        Some((active_owner_agent_id, owner_authorization_chain)),
    )
}

/// Reserve against every active token in `spend_token_ids` for `commodity`
/// (no owner-chain check). Used by the REST bridge.
pub fn reserve_active_stack(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    token_store: &mut TokenStore,
    spend_token_ids: &[String],
    commodity: &str,
    proposed_amount: Decimal,
    agent_id: &str,
    system_ceilings: &[SystemCeiling],
) -> Result<Vec<ReservationId>, BudgetRejection> {
    let tokens = find_active_tokens(ledger, token_store, spend_token_ids, commodity)?;
    reserve_token_stack(
        system_freeze,
        ledger,
        &tokens,
        system_ceilings,
        proposed_amount,
        agent_id,
        None,
    )
}

fn reserve_token_stack(
    system_freeze: &SystemFreezeState,
    ledger: &mut ResourceLedger,
    tokens: &[SpendToken],
    system_ceilings: &[SystemCeiling],
    proposed_amount: Decimal,
    actor_agent_id: &str,
    authorization: Option<(&str, &[String])>,
) -> Result<Vec<ReservationId>, BudgetRejection> {
    if system_freeze.frozen {
        return Err(BudgetRejection::SystemFrozen {
            frozen_at: system_freeze.frozen_at,
            reason: system_freeze.reason.clone(),
        });
    }
    if let Some(token) = tokens.first() {
        check_system_ceilings(ledger, &token.commodity, proposed_amount, system_ceilings)?;
    }
    let batch_id = if tokens.len() > 1 {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };
    let mut reserved = Vec::new();
    for token in tokens {
        match reserve_spend_impl(
            system_freeze,
            ledger,
            token,
            &[],
            proposed_amount,
            actor_agent_id,
            authorization,
            batch_id.clone(),
        ) {
            Ok(id) => {
                reserved.push(id);
            },
            Err(error) => {
                for id in reserved.iter().rev() {
                    let _ = rollback_spend(ledger, id);
                }
                return Err(error);
            },
        }
    }
    Ok(reserved)
}

/// Commit or roll back every active reservation that shares `id`'s batch
/// (or just `id` when it is not stacked).
pub fn commit_spend_group(
    ledger: &mut ResourceLedger,
    reservation_id: &ReservationId,
    actual_cost: Option<Decimal>,
) -> Result<(), LedgerError> {
    let ids = reservation_group_ids(ledger, reservation_id);
    for id in ids {
        commit_spend(ledger, &id, actual_cost)?;
    }
    Ok(())
}

/// Roll back every active reservation that shares `id`'s batch.
pub fn rollback_spend_group(
    ledger: &mut ResourceLedger,
    reservation_id: &ReservationId,
) -> Result<(), LedgerError> {
    let ids = reservation_group_ids(ledger, reservation_id);
    for id in ids {
        rollback_spend(ledger, &id)?;
    }
    Ok(())
}

fn reservation_group_ids(
    ledger: &ResourceLedger,
    reservation_id: &ReservationId,
) -> Vec<ReservationId> {
    let Some(reservation) = ledger.active_reservations.get(reservation_id) else {
        return vec![reservation_id.clone()];
    };
    match reservation.batch_id.as_ref() {
        Some(batch) => ledger
            .active_reservations
            .iter()
            .filter(|(_, item)| item.batch_id.as_ref() == Some(batch))
            .map(|(id, _)| id.clone())
            .collect(),
        None => vec![reservation_id.clone()],
    }
}

// ---------------------------------------------------------------------------
// check_stale_reservations
// ---------------------------------------------------------------------------

/// Returns reservations that have exceeded their max_duration.
/// Does NOT auto-rollback — the external action may have succeeded.
/// These must be flagged for reconciliation.
pub fn check_stale_reservations(ledger: &ResourceLedger) -> Vec<Reservation> {
    let now = Utc::now();
    ledger
        .active_reservations
        .values()
        .filter(|r| {
            now.signed_duration_since(r.created_at) > Duration::seconds(r.max_duration_secs)
        })
        .cloned()
        .collect()
}
