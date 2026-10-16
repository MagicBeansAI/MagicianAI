//! Request and response JSON types for the Resource Authority REST API.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::token::{CarryoverPolicy, CeilingPeriod, VelocityLimit};

// ---------------------------------------------------------------------------
// System Ceilings
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct CeilingRequest {
    pub commodity: String,
    pub ceiling: Decimal,
    #[serde(default)]
    pub relaxation: Decimal,
    pub period: CeilingPeriod,
    #[serde(default = "default_carryover")]
    pub carryover: CarryoverPolicy,
    /// Optional ID for updates. If absent, a new ceiling is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

fn default_carryover() -> CarryoverPolicy {
    CarryoverPolicy::None
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CeilingResponse {
    pub id: String,
    pub commodity: String,
    pub ceiling: Decimal,
    pub relaxation: Decimal,
    pub period: CeilingPeriod,
    pub carryover: CarryoverPolicy,
    pub period_start: DateTime<Utc>,
    pub spent_in_period: Decimal,
    pub reserved_in_period: Decimal,
    pub remaining_in_period: Decimal,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CeilingsListResponse {
    pub ceilings: Vec<CeilingResponse>,
}

// ---------------------------------------------------------------------------
// Bootstrap / Withdraw
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapRequest {
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapResponse {
    pub ok: bool,
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
    pub journal_entry_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WithdrawRequest {
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WithdrawResponse {
    pub ok: bool,
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
    pub remaining: Decimal,
    pub journal_entry_id: String,
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenSummaryResponse {
    pub id: String,
    pub issued_by: String,
    pub issued_to: String,
    pub commodity: String,
    pub ceiling: Decimal,
    pub period: CeilingPeriod,
    pub carryover: CarryoverPolicy,
    pub period_start: DateTime<Utc>,
    pub spent_in_period: Decimal,
    pub remaining_in_period: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub velocity_limit: Option<VelocityLimit>,
    pub burn_rate_per_day: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_exhaustion: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_of_runway: Option<f64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub conditions: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokensListResponse {
    pub tokens: Vec<TokenSummaryResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JournalEntryResponse {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub accrual_date: DateTime<Utc>,
    pub reference: String,
    pub agent_id: String,
    pub entries: Vec<LedgerEntryResponse>,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LedgerEntryResponse {
    pub account: String,
    pub amount: Decimal,
    pub commodity: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenDetailResponse {
    pub token: TokenSummaryResponse,
    pub spend_history: Vec<JournalEntryResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenRevokeResponse {
    pub ok: bool,
    pub token_id: String,
    pub previous_status: String,
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LedgerQueryParams {
    pub commodity: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub agent_id: Option<String>,
    pub token_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LedgerQueryResponse {
    pub entries: Vec<JournalEntryResponse>,
    pub total_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct BalancesQueryParams {
    pub commodity: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountBalanceResponse {
    pub account: String,
    pub commodity: String,
    pub balance: Decimal,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalancesResponse {
    pub balances: Vec<AccountBalanceResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imbalances: Option<Vec<AuditImbalanceResponse>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditImbalanceResponse {
    pub commodity: String,
    pub expected: Decimal,
    pub actual: Decimal,
}

// ---------------------------------------------------------------------------
// Reservations
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ReservationResponse {
    pub id: String,
    pub token_id: String,
    pub commodity: String,
    pub amount: Decimal,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub max_duration_secs: i64,
    pub is_stale: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReservationsListResponse {
    pub reservations: Vec<ReservationResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReservationActionResponse {
    pub ok: bool,
    pub reservation_id: String,
    pub action: String,
}

/// Body for `POST /resource-authority/reservations` — reserve `amount` of
/// `commodity` against the tool-scoped daily budget for `tool`. Used by the
/// Zepto/Swiggy `.mjs` wrappers (over the localhost REST bridge) to hold an
/// order's rupee total against the daily ceiling BEFORE placing a
/// `checkout_or_payment` call. `agent_id` is optional: tool-scoped budgets match
/// on `tool` regardless of the dispatching agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReserveRequest {
    pub tool: String,
    pub commodity: String,
    pub amount: Decimal,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReserveResponse {
    pub ok: bool,
    pub reservation_id: String,
    pub tool: String,
    pub commodity: String,
    pub amount: Decimal,
}

/// Optional body for `POST /resource-authority/reservations/{id}/commit`.
/// Absent body (or absent `actual`) commits the full reserved amount —
/// backward-compatible with callers that POST no body. A present `actual`
/// commits the metered actual cost (returning any delta to budget, or recording
/// an overage when `actual` exceeds the reservation), lighting up
/// `gate::commit_spend`'s variable-cost path so the wrapper can reserve the cart
/// estimate then commit the true order total.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CommitReservationRequest {
    #[serde(default)]
    pub actual: Option<Decimal>,
}

// ---------------------------------------------------------------------------
// Freeze
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct FreezeRequest {
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnfreezeRequest {
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FreezeStatusResponse {
    pub frozen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FreezeActionResponse {
    pub ok: bool,
    pub frozen: bool,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Period Close
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PeriodCloseRequest {
    pub commodity: String,
    pub period: CeilingPeriod,
    pub period_end: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeriodCloseResponse {
    pub ok: bool,
    pub commodity: String,
    pub period_end: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeriodCloseEntryResponse {
    pub commodity: String,
    pub period_end: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub closed_by: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeriodClosesListResponse {
    pub period_closes: Vec<PeriodCloseEntryResponse>,
}

// ---------------------------------------------------------------------------
// Refund / Credit
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct RefundRequest {
    pub token_id: String,
    pub amount: Decimal,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefundResponse {
    pub ok: bool,
    pub token_id: String,
    pub amount: Decimal,
    pub journal_entry_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreditRequest {
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreditResponse {
    pub ok: bool,
    pub agent_id: String,
    pub commodity: String,
    pub amount: Decimal,
    pub journal_entry_id: String,
}
