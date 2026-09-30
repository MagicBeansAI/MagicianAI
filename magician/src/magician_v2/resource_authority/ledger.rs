use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::types::{canonicalize_commodity, AccountId, ReservationId, TokenId};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, thiserror::Error)]
pub enum LedgerError {
    #[error("Journal entry does not balance: commodity {commodity} has net {net}")]
    NotBalanced { commodity: String, net: Decimal },

    #[error("Journal entry has no ledger entries")]
    EmptyEntry,

    #[error("Reservation not found: {0}")]
    ReservationNotFound(ReservationId),

    #[error("Period closed for commodity {commodity} at {period_end}")]
    PeriodClosed {
        commodity: String,
        period_end: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
pub struct LedgerImbalance {
    pub commodity: String,
    pub expected: Decimal,
    pub actual: Decimal,
}

// ---------------------------------------------------------------------------
// Period close tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodClose {
    pub commodity: String,
    pub period_end: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub closed_by: String,
}

// ---------------------------------------------------------------------------
// Core data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceAmount {
    pub value: Decimal,
    pub commodity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub commodity: String,
    pub cached_balance: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub account: AccountId,
    pub amount: ResourceAmount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub accrual_date: DateTime<Utc>,
    pub entries: Vec<LedgerEntry>,
    pub reference: String,
    pub agent_id: String,
    pub metadata: HashMap<String, String>,
}

/// In-flight reservation for reserve/commit protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub token_id: TokenId,
    pub commodity: String,
    pub amount: Decimal,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub idempotency_key: String,
    pub max_duration_secs: i64,
    /// When several stacked-scope tokens are reserved for one call, they share
    /// this id so REST commit/rollback of any member settles the whole stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

// ---------------------------------------------------------------------------
// ResourceLedger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLedger {
    pub accounts: HashMap<AccountId, Account>,
    pub journal: Vec<JournalEntry>,
    pub active_reservations: HashMap<ReservationId, Reservation>,
    #[serde(default)]
    pub period_closes: Vec<PeriodClose>,
}

impl Default for ResourceLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceLedger {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            journal: Vec::new(),
            active_reservations: HashMap::new(),
            period_closes: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // record() — the core write path
    // -----------------------------------------------------------------------

    /// Record a journal entry. Validates:
    /// 1. Entry has at least one ledger entry
    /// 2. Entries sum to zero per commodity
    /// 3. Accrual date is not in a closed period (unless post_close_adjustment)
    pub fn record(&mut self, entry: JournalEntry) -> Result<(), LedgerError> {
        // 1. Reject empty
        if entry.entries.is_empty() {
            return Err(LedgerError::EmptyEntry);
        }

        // 2. Check period close — reject entries with accrual_date in a closed period
        for close in &self.period_closes {
            if entry.accrual_date < close.period_end
                && entry
                    .entries
                    .iter()
                    .any(|e| e.amount.commodity == close.commodity)
                && !entry.is_post_close_adjustment()
            {
                return Err(LedgerError::PeriodClosed {
                    commodity: close.commodity.clone(),
                    period_end: close.period_end,
                });
            }
        }

        // 3. Validate zero-sum per commodity
        let mut sums: HashMap<String, Decimal> = HashMap::new();
        for le in &entry.entries {
            *sums
                .entry(le.amount.commodity.clone())
                .or_insert(Decimal::ZERO) += le.amount.value;
        }
        for (commodity, net) in &sums {
            if *net != Decimal::ZERO {
                return Err(LedgerError::NotBalanced {
                    commodity: commodity.clone(),
                    net: *net,
                });
            }
        }

        // 4. Apply — update cached balances, create accounts on demand
        for le in &entry.entries {
            let account = self
                .accounts
                .entry(le.account.clone())
                .or_insert_with(|| Account {
                    id: le.account.clone(),
                    commodity: le.amount.commodity.clone(),
                    cached_balance: Decimal::ZERO,
                });
            account.cached_balance += le.amount.value;
        }

        self.journal.push(entry);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Balance queries
    // -----------------------------------------------------------------------

    /// Get the current balance of an account. Returns 0 for unknown accounts.
    pub fn balance(&self, account: &AccountId) -> Decimal {
        self.accounts
            .get(account)
            .map(|a| a.cached_balance)
            .unwrap_or(Decimal::ZERO)
    }

    /// Total spent (sum of expense account debits) for a specific token.
    /// Looks at accounts matching `token:{token_id}:expense:*`.
    pub fn total_spent_for_token(&self, token_id: &str) -> Decimal {
        let prefix = format!("token:{}:expense:", token_id);
        self.accounts
            .iter()
            .filter(|(id, _)| id.starts_with(&prefix))
            .map(|(_, a)| a.cached_balance)
            .sum()
    }

    /// Total spent across ALL tokens for a given commodity.
    /// Sums all `token:*:expense:{commodity}` accounts.
    pub fn total_spent_for_commodity(&self, commodity: &str) -> Decimal {
        let suffix = format!(":expense:{}", commodity);
        self.accounts
            .iter()
            .filter(|(id, _)| id.starts_with("token:") && id.ends_with(&suffix))
            .map(|(_, a)| a.cached_balance)
            .sum()
    }

    /// Total currently reserved (in-flight) for a commodity.
    /// Sums all `token:*:reserved:{commodity}` account balances.
    pub fn total_reserved_for_commodity(&self, commodity: &str) -> Decimal {
        let suffix = format!(":reserved:{}", commodity);
        self.accounts
            .iter()
            .filter(|(id, _)| id.starts_with("token:") && id.ends_with(&suffix))
            .map(|(_, a)| a.cached_balance)
            .sum()
    }

    /// Currently reserved user-spend for `commodity`.
    ///
    /// Reads live `active_reservations` so a hold that began in a previous
    /// period still occupies the new window. Stacked-scope members that share
    /// a `batch_id` count once (one user spend, not one debit per pool).
    pub fn in_flight_reserved_for_commodity(&self, commodity: &str) -> Decimal {
        let commodity = canonicalize_commodity(commodity);
        let mut seen_batches = HashSet::new();
        let mut total = Decimal::ZERO;
        for reservation in self.active_reservations.values() {
            if canonicalize_commodity(&reservation.commodity) != commodity {
                continue;
            }
            if let Some(batch_id) = reservation
                .batch_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !seen_batches.insert(batch_id) {
                    continue;
                }
            }
            total += reservation.amount;
        }
        total
    }

    /// Audit the entire ledger for conservation invariant.
    /// Recomputes all account balances from journal entries and checks they sum
    /// to zero per commodity.
    pub fn audit(&self) -> Result<(), Vec<LedgerImbalance>> {
        // Recompute balances from journal
        let mut computed: HashMap<AccountId, Decimal> = HashMap::new();
        let mut commodity_sums: HashMap<String, Decimal> = HashMap::new();

        for je in &self.journal {
            for le in &je.entries {
                *computed.entry(le.account.clone()).or_insert(Decimal::ZERO) += le.amount.value;
                *commodity_sums
                    .entry(le.amount.commodity.clone())
                    .or_insert(Decimal::ZERO) += le.amount.value;
            }
        }

        // Check that all commodity sums are zero
        let mut imbalances = Vec::new();
        for (commodity, sum) in &commodity_sums {
            if *sum != Decimal::ZERO {
                imbalances.push(LedgerImbalance {
                    commodity: commodity.clone(),
                    expected: Decimal::ZERO,
                    actual: *sum,
                });
            }
        }

        // Also verify cached balances match computed
        for (account_id, account) in &self.accounts {
            let computed_balance = computed.get(account_id).copied().unwrap_or(Decimal::ZERO);
            if account.cached_balance != computed_balance {
                imbalances.push(LedgerImbalance {
                    commodity: format!("cache_mismatch:{}", account_id),
                    expected: computed_balance,
                    actual: account.cached_balance,
                });
            }
        }

        if imbalances.is_empty() {
            Ok(())
        } else {
            Err(imbalances)
        }
    }

    // -----------------------------------------------------------------------
    // Time-windowed queries (filter by accrual_date)
    // -----------------------------------------------------------------------

    /// Total spent for a commodity since a given timestamp.
    /// Filters journal entries by `accrual_date >= since`, sums expense entries.
    /// Stacked-scope members that share a `batch_id` count once (one user spend).
    pub fn total_spent_since(&self, commodity: &str, since: DateTime<Utc>) -> Decimal {
        let suffix = format!(":expense:{}", commodity);
        self.sum_commodity_accounts_since(&suffix, since, None)
    }

    /// Total reserved for a commodity since a given timestamp.
    pub fn total_reserved_since(&self, commodity: &str, since: DateTime<Utc>) -> Decimal {
        // Net balance of reserved accounts since the timestamp.
        // After reserve(+400) → commit(-400), the net is 0.
        // Stacked-scope members that share a `batch_id` count once.
        let suffix = format!(":reserved:{}", commodity);
        self.sum_commodity_accounts_since(&suffix, since, None)
    }

    /// Total spent for a specific token since a given timestamp.
    pub fn total_spent_for_token_since(&self, token_id: &str, since: DateTime<Utc>) -> Decimal {
        let prefix = format!("token:{}:expense:", token_id);
        self.journal
            .iter()
            .filter(|je| je.accrual_date >= since)
            .flat_map(|je| je.entries.iter())
            .filter(|le| le.account.starts_with(&prefix))
            .map(|le| le.amount.value)
            .sum()
    }

    /// Total reserved for a specific token since a given timestamp.
    pub fn total_reserved_for_token_since(&self, token_id: &str, since: DateTime<Utc>) -> Decimal {
        // Net balance of reserved account since the timestamp.
        // After reserve(+400) → commit(-400), the net is 0 (no longer in-flight).
        let prefix = format!("token:{}:reserved:", token_id);
        self.journal
            .iter()
            .filter(|je| je.accrual_date >= since)
            .flat_map(|je| je.entries.iter())
            .filter(|le| le.account.starts_with(&prefix))
            .map(|le| le.amount.value) // sum ALL entries (positive and negative = net)
            .sum()
    }

    /// Total spent for a commodity in a bounded time window [since, until).
    pub fn total_spent_since_until(
        &self,
        commodity: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Decimal {
        let suffix = format!(":expense:{}", commodity);
        self.sum_commodity_accounts_since(&suffix, since, Some(until))
    }

    fn sum_commodity_accounts_since(
        &self,
        account_suffix: &str,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> Decimal {
        let mut seen_batches = HashSet::new();
        let mut total = Decimal::ZERO;
        for entry in &self.journal {
            if entry.accrual_date < since {
                continue;
            }
            if until.is_some_and(|until| entry.accrual_date >= until) {
                continue;
            }
            let mut matched = Decimal::ZERO;
            let mut any = false;
            for line in &entry.entries {
                if line.account.starts_with("token:") && line.account.ends_with(account_suffix) {
                    any = true;
                    matched += line.amount.value;
                }
            }
            if !any {
                continue;
            }
            if let Some(batch_id) = entry
                .metadata
                .get("batch_id")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                let class = reservation_journal_class(&entry.reference);
                if !seen_batches.insert(format!("{class}:{batch_id}")) {
                    continue;
                }
            }
            total += matched;
        }
        total
    }

    /// Total spent for a specific token in a bounded time window [since, until).
    pub fn total_spent_for_token_since_until(
        &self,
        token_id: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Decimal {
        let prefix = format!("token:{}:expense:", token_id);
        self.journal
            .iter()
            .filter(|je| je.accrual_date >= since && je.accrual_date < until)
            .flat_map(|je| je.entries.iter())
            .filter(|le| le.account.starts_with(&prefix))
            .map(|le| le.amount.value)
            .sum()
    }

    // -----------------------------------------------------------------------
    // Analytics
    // -----------------------------------------------------------------------

    /// Average spend per day for a commodity over the given window duration.
    /// Looks back `window` from now.
    pub fn burn_rate(&self, commodity: &str, window: Duration) -> Decimal {
        let since = Utc::now() - window;
        let total = self.total_spent_since(commodity, since);
        let days = Decimal::from(window.num_days().max(1));
        total / days
    }

    /// At current burn rate, when will this token's budget hit zero?
    /// Returns None if burn rate is zero or budget is already zero.
    pub fn projected_exhaustion(&self, token_id: &str) -> Option<DateTime<Utc>> {
        // Get all commodities this token uses
        let prefix = format!("token:{}:budget:", token_id);
        for (id, account) in &self.accounts {
            if id.starts_with(&prefix) {
                let remaining = account.cached_balance;
                if remaining <= Decimal::ZERO {
                    return None;
                }
                let commodity = &account.commodity;
                let rate = self.burn_rate(commodity, Duration::days(30));
                if rate <= Decimal::ZERO {
                    return None;
                }
                let days_left = remaining / rate;
                // Convert Decimal days to i64 for chrono
                let days_i64 = days_left.to_string().parse::<f64>().ok()?;
                let secs = (days_i64 * 86400.0) as i64;
                return Some(Utc::now() + Duration::seconds(secs));
            }
        }
        None
    }

    /// How many days of budget remain for this token at current burn rate?
    /// Returns None if burn rate is zero or no budget account found.
    pub fn days_of_runway(&self, token_id: &str) -> Option<f64> {
        let prefix = format!("token:{}:budget:", token_id);
        for (id, account) in &self.accounts {
            if id.starts_with(&prefix) {
                let remaining = account.cached_balance;
                if remaining <= Decimal::ZERO {
                    return Some(0.0);
                }
                let commodity = &account.commodity;
                let rate = self.burn_rate(commodity, Duration::days(30));
                if rate <= Decimal::ZERO {
                    return None;
                }
                let days = remaining / rate;
                return days.to_string().parse::<f64>().ok();
            }
        }
        None
    }
}

fn reservation_journal_class(reference: &str) -> &'static str {
    if reference.starts_with("reserve:") {
        "reserve"
    } else if reference.starts_with("commit:") || reference.starts_with("commit_with_amount:") {
        "commit"
    } else if reference.starts_with("rollback:") {
        "rollback"
    } else if reference.starts_with("return_delta:") {
        "return_delta"
    } else if reference.starts_with("overage:") {
        "overage"
    } else {
        "other"
    }
}

fn reservation_journal_metadata(reservation: &Reservation) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("token_id".to_string(), reservation.token_id.clone());
    metadata.insert("commodity".to_string(), reservation.commodity.clone());
    metadata.insert("amount".to_string(), reservation.amount.to_string());
    metadata.insert(
        "idempotency_key".to_string(),
        reservation.idempotency_key.clone(),
    );
    if let Some(batch_id) = &reservation.batch_id {
        metadata.insert("batch_id".to_string(), batch_id.clone());
    }
    metadata
}

// ---------------------------------------------------------------------------
// JournalEntry constructors
// ---------------------------------------------------------------------------

impl JournalEntry {
    /// Create a journal entry for reserving spend from a token's budget.
    /// DR token:{id}:reserved:{commodity} +amount
    /// CR token:{id}:budget:{commodity}   -amount
    pub fn reserve(
        token_id: &str,
        commodity: &str,
        amount: Decimal,
        reservation: &Reservation,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("token:{}:reserved:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("token:{}:budget:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("reserve:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Commit a reservation: move from reserved to expense.
    /// DR token:{id}:expense:{commodity}   +amount
    /// CR token:{id}:reserved:{commodity}  -amount
    pub fn commit(reservation: &Reservation) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!(
                        "token:{}:expense:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: reservation.amount,
                        commodity: reservation.commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: format!(
                        "token:{}:reserved:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: -reservation.amount,
                        commodity: reservation.commodity.clone(),
                    },
                },
            ],
            reference: format!("commit:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Commit a reservation with a specific actual amount (for metered spend).
    /// DR token:{id}:expense:{commodity}   +actual
    /// CR token:{id}:reserved:{commodity}  -actual
    pub fn commit_with_amount(reservation: &Reservation, actual: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!(
                        "token:{}:expense:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: actual,
                        commodity: reservation.commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: format!(
                        "token:{}:reserved:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: -actual,
                        commodity: reservation.commodity.clone(),
                    },
                },
            ],
            reference: format!("commit_with_amount:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Return unused reservation delta back to budget.
    /// DR token:{id}:budget:{commodity}    +delta
    /// CR token:{id}:reserved:{commodity}  -delta
    pub fn return_delta(reservation: &Reservation, delta: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!(
                        "token:{}:budget:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: delta,
                        commodity: reservation.commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: format!(
                        "token:{}:reserved:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: -delta,
                        commodity: reservation.commodity.clone(),
                    },
                },
            ],
            reference: format!("return_delta:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Record overage: actual cost exceeded reservation, take from budget.
    /// DR token:{id}:expense:{commodity}  +overage
    /// CR token:{id}:budget:{commodity}   -overage
    pub fn overage(reservation: &Reservation, overage: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!(
                        "token:{}:expense:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: overage,
                        commodity: reservation.commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: format!(
                        "token:{}:budget:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: -overage,
                        commodity: reservation.commodity.clone(),
                    },
                },
            ],
            reference: format!("overage:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Rollback a reservation: return reserved amount to budget.
    /// DR token:{id}:budget:{commodity}    +amount
    /// CR token:{id}:reserved:{commodity}  -amount
    pub fn rollback(reservation: &Reservation) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!(
                        "token:{}:budget:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: reservation.amount,
                        commodity: reservation.commodity.clone(),
                    },
                },
                LedgerEntry {
                    account: format!(
                        "token:{}:reserved:{}",
                        reservation.token_id, reservation.commodity
                    ),
                    amount: ResourceAmount {
                        value: -reservation.amount,
                        commodity: reservation.commodity.clone(),
                    },
                },
            ],
            reference: format!("rollback:{}", reservation.id),
            agent_id: reservation.agent_id.clone(),
            metadata: reservation_journal_metadata(reservation),
        }
    }

    /// Token issuance: allocate budget from agent's available funds to a token.
    /// DR token:{id}:budget:{commodity}             +ceiling
    /// CR agent:{issuer}:available:{commodity}       -ceiling
    pub fn token_issuance(
        issuer_agent_id: &str,
        token_id: &str,
        commodity: &str,
        ceiling: Decimal,
    ) -> Self {
        let now = Utc::now();
        Self {
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
                    account: format!("agent:{}:available:{}", issuer_agent_id, commodity),
                    amount: ResourceAmount {
                        value: -ceiling,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("token_issuance:{}", token_id),
            agent_id: issuer_agent_id.to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Revert unspent token budget back to issuer agent.
    /// DR agent:{issuer}:available:{commodity}  +unspent
    /// CR token:{id}:budget:{commodity}         -unspent
    pub fn token_revert(
        issuer_agent_id: &str,
        token_id: &str,
        commodity: &str,
        unspent: Decimal,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("agent:{}:available:{}", issuer_agent_id, commodity),
                    amount: ResourceAmount {
                        value: unspent,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("token:{}:budget:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: -unspent,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("token_revert:{}", token_id),
            agent_id: issuer_agent_id.to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Bootstrap: human injects initial funds for an agent.
    /// DR agent:{id}:available:{commodity}    +amount
    /// CR system:allocation:{commodity}       -amount
    pub fn bootstrap(agent_id: &str, commodity: &str, amount: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("agent:{}:available:{}", agent_id, commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("system:allocation:{}", commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("bootstrap:{}:{}", agent_id, commodity),
            agent_id: "system".to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Withdrawal: remove funds from an agent back to system allocation.
    /// DR system:allocation:{commodity}        +amount
    /// CR agent:{id}:available:{commodity}     -amount
    pub fn withdrawal(agent_id: &str, commodity: &str, amount: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("system:allocation:{}", commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("agent:{}:available:{}", agent_id, commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("withdrawal:{}:{}", agent_id, commodity),
            agent_id: "system".to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Refund: reverse a previous expense back to the token's budget.
    /// DR token:{id}:budget:{commodity}   +amount
    /// CR token:{id}:expense:{commodity}  -amount
    pub fn refund(token_id: &str, commodity: &str, amount: Decimal, reason: &str) -> Self {
        let now = Utc::now();
        let mut metadata = HashMap::new();
        metadata.insert("reason".to_string(), reason.to_string());
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("token:{}:budget:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("token:{}:expense:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("refund:{}:{}", token_id, reason),
            agent_id: "system".to_string(),
            metadata,
        }
    }

    /// Vendor credit: credits not tied to a specific token.
    /// DR agent:{id}:available:{commodity}          +amount
    /// CR system:vendor_credits:{commodity}         -amount
    pub fn vendor_credit(agent_id: &str, commodity: &str, amount: Decimal, reason: &str) -> Self {
        let now = Utc::now();
        let mut metadata = HashMap::new();
        metadata.insert("reason".to_string(), reason.to_string());
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("agent:{}:available:{}", agent_id, commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("system:vendor_credits:{}", commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("vendor_credit:{}", reason),
            agent_id: "system".to_string(),
            metadata,
        }
    }

    /// Post-close adjustment: allowed to have accrual_date in a closed period.
    pub fn post_close_adjustment(commodity: &str, entries: Vec<LedgerEntry>, reason: &str) -> Self {
        let now = Utc::now();
        let mut metadata = HashMap::new();
        metadata.insert("reason".to_string(), reason.to_string());
        metadata.insert("commodity".to_string(), commodity.to_string());
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries,
            reference: format!("post_close_adjustment:{}", reason),
            agent_id: "system".to_string(),
            metadata,
        }
    }

    /// Period clawback: return leftover budget to system allocation (use-it-or-lose-it).
    /// DR system:allocation:{commodity}    +amount
    /// CR token:{id}:budget:{commodity}    -amount
    pub fn period_clawback(token_id: &str, commodity: &str, amount: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("system:allocation:{}", commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("token:{}:budget:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("period_clawback:{}", token_id),
            agent_id: "system-config".to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Period refill: allocate this period's ceiling from system allocation.
    /// DR token:{id}:budget:{commodity}    +amount
    /// CR system:allocation:{commodity}    -amount
    pub fn period_refill(token_id: &str, commodity: &str, amount: Decimal) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            timestamp: now,
            accrual_date: now,
            entries: vec![
                LedgerEntry {
                    account: format!("token:{}:budget:{}", token_id, commodity),
                    amount: ResourceAmount {
                        value: amount,
                        commodity: commodity.to_string(),
                    },
                },
                LedgerEntry {
                    account: format!("system:allocation:{}", commodity),
                    amount: ResourceAmount {
                        value: -amount,
                        commodity: commodity.to_string(),
                    },
                },
            ],
            reference: format!("period_refill:{}", token_id),
            agent_id: "system-config".to_string(),
            metadata: HashMap::new(),
        }
    }

    /// Returns true if this is a post-close adjustment entry.
    pub fn is_post_close_adjustment(&self) -> bool {
        self.reference.starts_with("post_close_adjustment:")
    }
}
