use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::types::TokenId;

/// Period for ceiling/token rate limiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingPeriod {
    Total,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Annual,
}

/// Policy for carrying unspent budget into the next period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum CarryoverPolicy {
    /// Use-it-or-lose-it: unspent budget disappears at period end.
    None,
    /// All unspent rolls to next period.
    Full,
    /// Carry up to a capped amount.
    Capped { cap: Decimal },
}

/// System-wide ceiling for a commodity. Multiple ceilings can stack (all checked).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemCeiling {
    pub id: String,
    pub commodity: String,
    pub ceiling: Decimal,
    pub relaxation: Decimal,
    pub period: CeilingPeriod,
    pub carryover: CarryoverPolicy,
}

/// Token status lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStatus {
    Active,
    Revoked,
    Expired,
}

/// Rate limit within a token's period — prevents burning the entire budget in minutes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocityLimit {
    pub max_amount: Decimal,
    pub window_seconds: u64,
}

/// A delegatable spending permission for any commodity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendToken {
    pub id: TokenId,
    pub issued_by: String,
    pub issued_to: String,

    pub commodity: String,
    pub ceiling: Decimal,

    pub period: CeilingPeriod,
    pub carryover: CarryoverPolicy,

    pub conditions: Vec<String>,
    pub velocity_limit: Option<VelocityLimit>,

    pub status: TokenStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,

    pub system_ceiling_id: Option<String>,
    /// Start of the period window this token's budget account was last funded
    /// for. `None` on tokens issued before period-refill existed: inferred from
    /// `created_at` on first reserve. `CeilingPeriod::Total` ignores this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_period_start: Option<DateTime<Utc>>,
}

impl SpendToken {
    /// Check whether this token is past its expiry time.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() > exp,
            None => false,
        }
    }

    /// The budget account for this token: `token:{id}:budget:{commodity}`.
    pub fn budget_account(&self) -> String {
        format!("token:{}:budget:{}", self.id, self.commodity)
    }

    /// The reserved account for this token: `token:{id}:reserved:{commodity}`.
    pub fn reserved_account(&self) -> String {
        format!("token:{}:reserved:{}", self.id, self.commodity)
    }

    /// The expense account for this token: `token:{id}:expense:{commodity}`.
    pub fn expense_account(&self) -> String {
        format!("token:{}:expense:{}", self.id, self.commodity)
    }
}

/// Returns the start of the current period window for the given period type.
pub fn period_window_start(period: &CeilingPeriod) -> DateTime<Utc> {
    period_window_start_at(period, Utc::now())
}

/// Returns the start of the period window that contains `at`.
pub fn period_window_start_at(period: &CeilingPeriod, at: DateTime<Utc>) -> DateTime<Utc> {
    match period {
        CeilingPeriod::Total => DateTime::<Utc>::MIN_UTC,
        CeilingPeriod::Hourly => at
            .date_naive()
            .and_hms_opt(at.time().hour(), 0, 0)
            .unwrap()
            .and_utc(),
        CeilingPeriod::Daily => at.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
        CeilingPeriod::Weekly => start_of_week_utc(at),
        CeilingPeriod::Monthly => at
            .with_day(1)
            .unwrap()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc(),
        CeilingPeriod::Quarterly => start_of_quarter_utc(at),
        CeilingPeriod::Annual => NaiveDate::from_ymd_opt(at.year(), 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc(),
    }
}

/// Returns (start, end) of the previous period.
/// For example, for Monthly on March 15: returns (Feb 1 00:00, Mar 1 00:00).
pub fn previous_period_window(period: &CeilingPeriod) -> (DateTime<Utc>, DateTime<Utc>) {
    let current_start = period_window_start(period);
    match period {
        CeilingPeriod::Total => (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MIN_UTC),
        CeilingPeriod::Hourly => {
            let prev_start = current_start - Duration::hours(1);
            (prev_start, current_start)
        },
        CeilingPeriod::Daily => {
            let prev_start = current_start - Duration::days(1);
            (prev_start, current_start)
        },
        CeilingPeriod::Weekly => {
            let prev_start = current_start - Duration::weeks(1);
            (prev_start, current_start)
        },
        CeilingPeriod::Monthly => {
            let current_naive = current_start.date_naive();
            let prev_month = if current_naive.month() == 1 {
                NaiveDate::from_ymd_opt(current_naive.year() - 1, 12, 1).unwrap()
            } else {
                NaiveDate::from_ymd_opt(current_naive.year(), current_naive.month() - 1, 1).unwrap()
            };
            let prev_start = prev_month.and_hms_opt(0, 0, 0).unwrap().and_utc();
            (prev_start, current_start)
        },
        CeilingPeriod::Quarterly => {
            let prev_start = {
                let current_naive = current_start.date_naive();
                let (year, month) = if current_naive.month() <= 3 {
                    (current_naive.year() - 1, 10)
                } else {
                    (current_naive.year(), current_naive.month() - 3)
                };
                NaiveDate::from_ymd_opt(year, month, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
            };
            (prev_start, current_start)
        },
        CeilingPeriod::Annual => {
            let current_year = current_start.date_naive().year();
            let prev_start = NaiveDate::from_ymd_opt(current_year - 1, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            (prev_start, current_start)
        },
    }
}

/// Computes the start of the ISO week (Monday) for the given datetime.
fn start_of_week_utc(dt: DateTime<Utc>) -> DateTime<Utc> {
    let weekday = dt.weekday().num_days_from_monday(); // Monday=0 .. Sunday=6
    let monday = dt.date_naive() - Duration::days(weekday as i64);
    monday.and_hms_opt(0, 0, 0).unwrap().and_utc()
}

/// Computes the start of the quarter for the given datetime.
fn start_of_quarter_utc(dt: DateTime<Utc>) -> DateTime<Utc> {
    let month = dt.month();
    let quarter_start_month = match month {
        1..=3 => 1,
        4..=6 => 4,
        7..=9 => 7,
        _ => 10,
    };
    NaiveDate::from_ymd_opt(dt.year(), quarter_start_month, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

use chrono::Timelike;

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_spend_token_budget_account() {
        let token = SpendToken {
            id: "rct_123".to_string(),
            issued_by: "cfo".to_string(),
            issued_to: "cmo".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(500, 0),
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: None,
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        };
        assert_eq!(token.budget_account(), "token:rct_123:budget:USD");
        assert_eq!(token.reserved_account(), "token:rct_123:reserved:USD");
        assert_eq!(token.expense_account(), "token:rct_123:expense:USD");
    }

    #[test]
    fn test_is_expired_none() {
        let token = SpendToken {
            id: "t1".to_string(),
            issued_by: "a".to_string(),
            issued_to: "b".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(100, 0),
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: None,
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        };
        assert!(!token.is_expired());
    }

    #[test]
    fn test_is_expired_past() {
        let token = SpendToken {
            id: "t1".to_string(),
            issued_by: "a".to_string(),
            issued_to: "b".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(100, 0),
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: Some(Utc::now() - Duration::hours(1)),
            created_at: Utc::now() - Duration::hours(2),
            system_ceiling_id: None,
            last_period_start: None,
        };
        assert!(token.is_expired());
    }

    #[test]
    fn test_is_expired_future() {
        let token = SpendToken {
            id: "t1".to_string(),
            issued_by: "a".to_string(),
            issued_to: "b".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(100, 0),
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: Some(Utc::now() + Duration::hours(1)),
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        };
        assert!(!token.is_expired());
    }

    #[test]
    fn test_period_window_start_total() {
        let start = period_window_start(&CeilingPeriod::Total);
        assert_eq!(start, DateTime::<Utc>::MIN_UTC);
    }

    #[test]
    fn test_previous_period_window_monthly() {
        let (prev_start, prev_end) = previous_period_window(&CeilingPeriod::Monthly);
        let current_start = period_window_start(&CeilingPeriod::Monthly);
        assert_eq!(prev_end, current_start);
        // prev_start should be the 1st of previous month
        assert_eq!(prev_start.day(), 1);
    }

    #[test]
    fn test_period_window_start_at_daily_is_midnight_of_that_day() {
        let at = NaiveDate::from_ymd_opt(2026, 3, 15)
            .unwrap()
            .and_hms_opt(18, 42, 9)
            .unwrap()
            .and_utc();
        let start = period_window_start_at(&CeilingPeriod::Daily, at);
        assert_eq!(
            start,
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
        );
    }
}
