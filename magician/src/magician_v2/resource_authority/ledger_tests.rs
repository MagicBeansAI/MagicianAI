use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use super::ledger::*;
use super::persistence;
use super::types::*;

/// Helper macro to create Decimal literals without a separate macro crate.
#[cfg(test)]
macro_rules! dec {
    ($val:expr) => {
        Decimal::from($val)
    };
}

// ===========================================================================
// Helpers
// ===========================================================================

fn make_balanced_entry(
    dr_account: &str,
    cr_account: &str,
    commodity: &str,
    amount: Decimal,
) -> JournalEntry {
    JournalEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        accrual_date: Utc::now(),
        entries: vec![
            LedgerEntry {
                account: dr_account.to_string(),
                amount: ResourceAmount {
                    value: amount,
                    commodity: commodity.to_string(),
                },
            },
            LedgerEntry {
                account: cr_account.to_string(),
                amount: ResourceAmount {
                    value: -amount,
                    commodity: commodity.to_string(),
                },
            },
        ],
        reference: "test".to_string(),
        agent_id: "test_agent".to_string(),
        metadata: HashMap::new(),
    }
}

fn make_reservation(token_id: &str, commodity: &str, amount: Decimal) -> Reservation {
    Reservation {
        id: ReservationId::new(),
        token_id: token_id.to_string(),
        commodity: commodity.to_string(),
        amount,
        agent_id: "test_agent".to_string(),
        created_at: Utc::now(),
        idempotency_key: format!("test-{}", Uuid::new_v4()),
        max_duration_secs: 120,
        batch_id: None,
    }
}

/// Bootstrap an agent with funds and return the ledger.
fn bootstrap_ledger(agent_id: &str, commodity: &str, amount: Decimal) -> ResourceLedger {
    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap(agent_id, commodity, amount))
        .expect("bootstrap should succeed");
    ledger
}

/// Set up a full token flow: bootstrap agent, issue token, return (ledger, token_id).
fn setup_token_flow(
    agent_id: &str,
    token_id: &str,
    commodity: &str,
    bootstrap_amount: Decimal,
    token_ceiling: Decimal,
) -> ResourceLedger {
    let mut ledger = bootstrap_ledger(agent_id, commodity, bootstrap_amount);
    ledger
        .record(JournalEntry::token_issuance(
            agent_id,
            token_id,
            commodity,
            token_ceiling,
        ))
        .expect("token issuance should succeed");
    ledger
}

// ===========================================================================
// Conservation invariant tests
// ===========================================================================

#[test]
fn test_every_journal_entry_sums_to_zero() {
    let mut ledger = ResourceLedger::new();

    // Record 100 balanced entries
    for i in 0..100 {
        let amount = Decimal::from(i + 1);
        let entry = make_balanced_entry(
            &format!("account:dr:{}", i),
            &format!("account:cr:{}", i),
            "USD",
            amount,
        );
        ledger.record(entry).expect("balanced entry should succeed");
    }

    assert!(
        ledger.audit().is_ok(),
        "audit should pass for balanced entries"
    );
}

#[test]
fn test_imbalanced_entry_rejected() {
    let mut ledger = ResourceLedger::new();

    let entry = JournalEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        accrual_date: Utc::now(),
        entries: vec![
            LedgerEntry {
                account: "account:a".to_string(),
                amount: ResourceAmount {
                    value: dec!(100),
                    commodity: "USD".to_string(),
                },
            },
            LedgerEntry {
                account: "account:b".to_string(),
                amount: ResourceAmount {
                    value: dec!(-99),
                    commodity: "USD".to_string(),
                },
            },
        ],
        reference: "bad".to_string(),
        agent_id: "test".to_string(),
        metadata: HashMap::new(),
    };

    let result = ledger.record(entry);
    assert!(result.is_err(), "imbalanced entry should be rejected");
    match result.unwrap_err() {
        LedgerError::NotBalanced { commodity, net } => {
            assert_eq!(commodity, "USD");
            assert_eq!(net, dec!(1));
        },
        other => panic!("expected NotBalanced, got {:?}", other),
    }
}

#[test]
fn test_audit_catches_corruption() {
    let mut ledger = ResourceLedger::new();

    // Record a valid entry
    ledger
        .record(make_balanced_entry("a", "b", "USD", dec!(100)))
        .unwrap();

    // Manually corrupt a cached balance
    if let Some(account) = ledger.accounts.get_mut("a") {
        account.cached_balance += dec!(1);
    }

    let result = ledger.audit();
    assert!(result.is_err(), "audit should catch corrupted cache");
}

#[test]
fn test_empty_entry_rejected() {
    let mut ledger = ResourceLedger::new();

    let entry = JournalEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        accrual_date: Utc::now(),
        entries: vec![],
        reference: "empty".to_string(),
        agent_id: "test".to_string(),
        metadata: HashMap::new(),
    };

    let result = ledger.record(entry);
    assert!(result.is_err(), "empty entry should be rejected");
    assert!(matches!(result.unwrap_err(), LedgerError::EmptyEntry));
}

// ===========================================================================
// Balance query tests
// ===========================================================================

#[test]
fn test_balance_starts_at_zero() {
    let ledger = ResourceLedger::new();
    assert_eq!(ledger.balance(&"nonexistent:account".to_string()), dec!(0));
}

#[test]
fn test_balance_after_single_entry() {
    let ledger = bootstrap_ledger("cfo", "USD", dec!(5000));

    assert_eq!(
        ledger.balance(&"agent:cfo:available:USD".to_string()),
        dec!(5000)
    );
    assert_eq!(
        ledger.balance(&"system:allocation:USD".to_string()),
        dec!(-5000)
    );
}

#[test]
fn test_balance_after_multiple_entries() {
    let mut ledger = bootstrap_ledger("cfo", "USD", dec!(5000));

    // Issue token
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "rct_ads",
            "USD",
            dec!(500),
        ))
        .unwrap();

    assert_eq!(
        ledger.balance(&"agent:cfo:available:USD".to_string()),
        dec!(4500)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:budget:USD".to_string()),
        dec!(500)
    );

    // Reserve spend
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve(
            "rct_ads",
            "USD",
            dec!(100),
            &reservation,
        ))
        .unwrap();

    assert_eq!(
        ledger.balance(&"token:rct_ads:budget:USD".to_string()),
        dec!(400)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:reserved:USD".to_string()),
        dec!(100)
    );

    // Commit
    ledger.record(JournalEntry::commit(&reservation)).unwrap();

    assert_eq!(
        ledger.balance(&"token:rct_ads:reserved:USD".to_string()),
        dec!(0)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:expense:USD".to_string()),
        dec!(100)
    );
}

#[test]
fn test_cached_balance_matches_computed() {
    let mut ledger = ResourceLedger::new();

    // Perform a bunch of operations
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(10000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(3000),
        ))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok2",
            "USD",
            dec!(2000),
        ))
        .unwrap();

    let r1 = make_reservation("tok1", "USD", dec!(500));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(500), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    let r2 = make_reservation("tok2", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok2", "USD", dec!(200), &r2))
        .unwrap();
    ledger.record(JournalEntry::rollback(&r2)).unwrap();

    // Recompute from journal and compare
    let mut computed: HashMap<String, Decimal> = HashMap::new();
    for je in &ledger.journal {
        for le in &je.entries {
            *computed.entry(le.account.clone()).or_insert(dec!(0)) += le.amount.value;
        }
    }

    for (account_id, account) in &ledger.accounts {
        let expected = computed.get(account_id).copied().unwrap_or(dec!(0));
        assert_eq!(
            account.cached_balance, expected,
            "cached balance mismatch for account {}",
            account_id
        );
    }

    assert!(ledger.audit().is_ok());
}

// ===========================================================================
// Multi-commodity tests
// ===========================================================================

#[test]
fn test_commodities_are_independent() {
    let mut ledger = ResourceLedger::new();

    // Bootstrap USD
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();

    // Bootstrap EMAIL_SENDS
    ledger
        .record(JournalEntry::bootstrap("cto", "EMAIL_SENDS", dec!(1000)))
        .unwrap();

    // Issue tokens
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok_usd",
            "USD",
            dec!(500),
        ))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cto",
            "tok_email",
            "EMAIL_SENDS",
            dec!(100),
        ))
        .unwrap();

    // Spend USD
    let r1 = make_reservation("tok_usd", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve("tok_usd", "USD", dec!(100), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // USD expense tracked
    assert_eq!(ledger.total_spent_for_commodity("USD"), dec!(100));
    // EMAIL_SENDS NOT affected
    assert_eq!(ledger.total_spent_for_commodity("EMAIL_SENDS"), dec!(0));

    // EMAIL_SENDS balance unchanged
    assert_eq!(
        ledger.balance(&"token:tok_email:budget:EMAIL_SENDS".to_string()),
        dec!(100)
    );
}

#[test]
fn test_audit_per_commodity() {
    let mut ledger = ResourceLedger::new();

    ledger
        .record(JournalEntry::bootstrap("a", "USD", dec!(1000)))
        .unwrap();
    ledger
        .record(JournalEntry::bootstrap("b", "GITHUB_PUSHES", dec!(50)))
        .unwrap();

    assert!(
        ledger.audit().is_ok(),
        "independent commodities should not violate conservation"
    );
}

#[test]
fn test_total_spent_for_commodity_filters_correctly() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(5000), dec!(1000));

    // Spend some USD
    let r1 = make_reservation("tok1", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(200), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // Also set up a different commodity
    ledger
        .record(JournalEntry::bootstrap("cto", "GITHUB_PUSHES", dec!(100)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cto",
            "tok2",
            "GITHUB_PUSHES",
            dec!(50),
        ))
        .unwrap();
    let r2 = make_reservation("tok2", "GITHUB_PUSHES", dec!(3));
    ledger
        .record(JournalEntry::reserve("tok2", "GITHUB_PUSHES", dec!(3), &r2))
        .unwrap();
    ledger.record(JournalEntry::commit(&r2)).unwrap();

    assert_eq!(ledger.total_spent_for_commodity("USD"), dec!(200));
    assert_eq!(ledger.total_spent_for_commodity("GITHUB_PUSHES"), dec!(3));
    assert_eq!(ledger.total_spent_for_commodity("NONEXISTENT"), dec!(0));
}

// ===========================================================================
// Token-scoped query tests
// ===========================================================================

#[test]
fn test_total_spent_for_token() {
    let mut ledger = ResourceLedger::new();

    // Bootstrap and create two tokens
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(10000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok_a",
            "USD",
            dec!(3000),
        ))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok_b",
            "USD",
            dec!(2000),
        ))
        .unwrap();

    // Spend from token A
    let r1 = make_reservation("tok_a", "USD", dec!(500));
    ledger
        .record(JournalEntry::reserve("tok_a", "USD", dec!(500), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // Spend from token B
    let r2 = make_reservation("tok_b", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok_b", "USD", dec!(200), &r2))
        .unwrap();
    ledger.record(JournalEntry::commit(&r2)).unwrap();

    assert_eq!(ledger.total_spent_for_token("tok_a"), dec!(500));
    assert_eq!(ledger.total_spent_for_token("tok_b"), dec!(200));
    assert_eq!(ledger.total_spent_for_token("tok_c"), dec!(0));
}

#[test]
fn test_total_reserved_for_commodity() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(5000), dec!(1000));

    // Create a second token
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok2",
            "USD",
            dec!(1000),
        ))
        .unwrap();

    // Reserve from both tokens
    let r1 = make_reservation("tok1", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(100), &r1))
        .unwrap();

    let r2 = make_reservation("tok2", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok2", "USD", dec!(200), &r2))
        .unwrap();

    assert_eq!(ledger.total_reserved_for_commodity("USD"), dec!(300));

    // Commit one — reserved should go down
    ledger.record(JournalEntry::commit(&r1)).unwrap();
    // After commit, tok1 reserved account goes to 0, tok2 reserved account stays at 200
    assert_eq!(ledger.total_reserved_for_commodity("USD"), dec!(200));
}

#[test]
fn in_flight_reserved_counts_a_stacked_batch_once() {
    let mut ledger = ResourceLedger::new();
    let batch = "stack-1".to_string();
    let mut first = make_reservation("tok1", "USD", dec!(100));
    first.batch_id = Some(batch.clone());
    let mut second = make_reservation("tok2", "USD", dec!(100));
    second.batch_id = Some(batch);
    ledger.active_reservations.insert(first.id.clone(), first);
    ledger.active_reservations.insert(second.id.clone(), second);
    assert_eq!(ledger.in_flight_reserved_for_commodity("usd"), dec!(100));
    assert_eq!(ledger.in_flight_reserved_for_commodity("INR"), dec!(0));
}

// ===========================================================================
// Time-windowed query tests
// ===========================================================================

#[test]
fn test_total_spent_since() {
    let mut ledger = ResourceLedger::new();

    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(10000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(5000),
        ))
        .unwrap();

    // Record a spend with accrual_date = now
    let r1 = make_reservation("tok1", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(100), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // Query since 1 hour ago — should include this spend
    let since = Utc::now() - Duration::hours(1);
    assert_eq!(ledger.total_spent_since("USD", since), dec!(100));

    // Query since 1 hour from now — should not include
    let future = Utc::now() + Duration::hours(1);
    assert_eq!(ledger.total_spent_since("USD", future), dec!(0));
}

#[test]
fn test_total_spent_for_token_since() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));

    let r1 = make_reservation("tok1", "USD", dec!(300));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(300), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    let since = Utc::now() - Duration::hours(1);
    assert_eq!(ledger.total_spent_for_token_since("tok1", since), dec!(300));
    assert_eq!(ledger.total_spent_for_token_since("tok2", since), dec!(0));
}

#[test]
fn test_total_spent_since_until() {
    let mut ledger = ResourceLedger::new();

    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(10000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(5000),
        ))
        .unwrap();

    // Create an entry with a specific accrual_date
    let r1 = make_reservation("tok1", "USD", dec!(200));
    let mut reserve_entry = JournalEntry::reserve("tok1", "USD", dec!(200), &r1);
    let past = Utc::now() - Duration::hours(2);
    reserve_entry.accrual_date = past;
    ledger.record(reserve_entry).unwrap();

    let mut commit_entry = JournalEntry::commit(&r1);
    commit_entry.accrual_date = past;
    ledger.record(commit_entry).unwrap();

    // Window that includes the entry
    let start = Utc::now() - Duration::hours(3);
    let end = Utc::now() - Duration::hours(1);
    assert_eq!(ledger.total_spent_since_until("USD", start, end), dec!(200));

    // Window that excludes the entry
    let start2 = Utc::now() - Duration::minutes(30);
    let end2 = Utc::now();
    assert_eq!(ledger.total_spent_since_until("USD", start2, end2), dec!(0));
}

#[test]
fn test_total_reserved_since() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));

    let r1 = make_reservation("tok1", "USD", dec!(150));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(150), &r1))
        .unwrap();

    let since = Utc::now() - Duration::hours(1);
    assert_eq!(ledger.total_reserved_since("USD", since), dec!(150));
}

#[test]
fn test_total_reserved_for_token_since() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));

    let r1 = make_reservation("tok1", "USD", dec!(250));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(250), &r1))
        .unwrap();

    let since = Utc::now() - Duration::hours(1);
    assert_eq!(
        ledger.total_reserved_for_token_since("tok1", since),
        dec!(250)
    );
    assert_eq!(
        ledger.total_reserved_for_token_since("tok2", since),
        dec!(0)
    );
}

#[test]
fn test_total_spent_for_token_since_until() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));

    let r1 = make_reservation("tok1", "USD", dec!(400));
    let past = Utc::now() - Duration::hours(2);

    let mut reserve_entry = JournalEntry::reserve("tok1", "USD", dec!(400), &r1);
    reserve_entry.accrual_date = past;
    ledger.record(reserve_entry).unwrap();

    let mut commit_entry = JournalEntry::commit(&r1);
    commit_entry.accrual_date = past;
    ledger.record(commit_entry).unwrap();

    let start = Utc::now() - Duration::hours(3);
    let end = Utc::now() - Duration::hours(1);
    assert_eq!(
        ledger.total_spent_for_token_since_until("tok1", start, end),
        dec!(400)
    );
    assert_eq!(
        ledger.total_spent_for_token_since_until("tok2", start, end),
        dec!(0)
    );
}

// ===========================================================================
// JournalEntry constructor tests
// ===========================================================================

#[test]
fn test_bootstrap_constructor() {
    let entry = JournalEntry::bootstrap("cfo", "USD", dec!(5000));
    assert_eq!(entry.entries.len(), 2);
    assert_eq!(entry.entries[0].account, "agent:cfo:available:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(5000));
    assert_eq!(entry.entries[1].account, "system:allocation:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-5000));
}

#[test]
fn test_token_issuance_constructor() {
    let entry = JournalEntry::token_issuance("cfo", "rct_ads", "USD", dec!(500));
    assert_eq!(entry.entries[0].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(500));
    assert_eq!(entry.entries[1].account, "agent:cfo:available:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-500));
}

#[test]
fn test_reserve_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::reserve("rct_ads", "USD", dec!(100), &reservation);
    assert_eq!(entry.entries[0].account, "token:rct_ads:reserved:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(100));
    assert_eq!(entry.entries[1].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-100));
}

#[test]
fn test_commit_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::commit(&reservation);
    assert_eq!(entry.entries[0].account, "token:rct_ads:expense:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(100));
    assert_eq!(entry.entries[1].account, "token:rct_ads:reserved:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-100));
}

#[test]
fn test_commit_with_amount_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::commit_with_amount(&reservation, dec!(75));
    assert_eq!(entry.entries[0].account, "token:rct_ads:expense:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(75));
    assert_eq!(entry.entries[1].account, "token:rct_ads:reserved:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-75));
}

#[test]
fn test_return_delta_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::return_delta(&reservation, dec!(25));
    assert_eq!(entry.entries[0].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(25));
    assert_eq!(entry.entries[1].account, "token:rct_ads:reserved:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-25));
}

#[test]
fn test_overage_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::overage(&reservation, dec!(50));
    assert_eq!(entry.entries[0].account, "token:rct_ads:expense:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(50));
    assert_eq!(entry.entries[1].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-50));
}

#[test]
fn test_rollback_constructor() {
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    let entry = JournalEntry::rollback(&reservation);
    assert_eq!(entry.entries[0].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(100));
    assert_eq!(entry.entries[1].account, "token:rct_ads:reserved:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-100));
}

#[test]
fn test_token_revert_constructor() {
    let entry = JournalEntry::token_revert("cfo", "rct_ads", "USD", dec!(200));
    assert_eq!(entry.entries[0].account, "agent:cfo:available:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(200));
    assert_eq!(entry.entries[1].account, "token:rct_ads:budget:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-200));
}

#[test]
fn test_withdrawal_constructor() {
    let entry = JournalEntry::withdrawal("cfo", "USD", dec!(2000));
    assert_eq!(entry.entries[0].account, "system:allocation:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(2000));
    assert_eq!(entry.entries[1].account, "agent:cfo:available:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-2000));
}

#[test]
fn test_refund_constructor() {
    let entry = JournalEntry::refund("tok1", "USD", dec!(50), "invalid click refund");
    assert_eq!(entry.entries[0].account, "token:tok1:budget:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(50));
    assert_eq!(entry.entries[1].account, "token:tok1:expense:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-50));
    assert_eq!(
        entry.metadata.get("reason").unwrap(),
        "invalid click refund"
    );
}

#[test]
fn test_vendor_credit_constructor() {
    let entry = JournalEntry::vendor_credit("cfo", "USD", dec!(1000), "AWS promo");
    assert_eq!(entry.entries[0].account, "agent:cfo:available:USD");
    assert_eq!(entry.entries[0].amount.value, dec!(1000));
    assert_eq!(entry.entries[1].account, "system:vendor_credits:USD");
    assert_eq!(entry.entries[1].amount.value, dec!(-1000));
}

#[test]
fn test_post_close_adjustment_constructor_and_predicate() {
    let entries = vec![
        LedgerEntry {
            account: "token:tok1:expense:USD".to_string(),
            amount: ResourceAmount {
                value: dec!(50),
                commodity: "USD".to_string(),
            },
        },
        LedgerEntry {
            account: "token:tok1:budget:USD".to_string(),
            amount: ResourceAmount {
                value: dec!(-50),
                commodity: "USD".to_string(),
            },
        },
    ];

    let entry = JournalEntry::post_close_adjustment("USD", entries, "billing correction");
    assert!(entry.is_post_close_adjustment());
    assert!(entry
        .reference
        .starts_with("post_close_adjustment:billing correction"));

    // Regular entries should not be post-close adjustments
    let regular = JournalEntry::bootstrap("a", "USD", dec!(100));
    assert!(!regular.is_post_close_adjustment());
}

// ===========================================================================
// Period close enforcement tests
// ===========================================================================

#[test]
fn test_period_close_rejects_backdated_entries() {
    let mut ledger = ResourceLedger::new();

    // Close a period
    let period_end = Utc::now() - Duration::hours(1);
    ledger.period_closes.push(PeriodClose {
        commodity: "USD".to_string(),
        period_end,
        closed_at: Utc::now(),
        closed_by: "admin".to_string(),
    });

    // Try to record an entry with accrual_date before period_end
    let mut entry = JournalEntry::bootstrap("a", "USD", dec!(100));
    entry.accrual_date = period_end - Duration::hours(1);

    let result = ledger.record(entry);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        LedgerError::PeriodClosed { .. }
    ));
}

#[test]
fn test_period_close_allows_post_close_adjustment() {
    let mut ledger = ResourceLedger::new();

    let period_end = Utc::now() - Duration::hours(1);
    ledger.period_closes.push(PeriodClose {
        commodity: "USD".to_string(),
        period_end,
        closed_at: Utc::now(),
        closed_by: "admin".to_string(),
    });

    // Post-close adjustment should be allowed even with backdated accrual
    let entries = vec![
        LedgerEntry {
            account: "token:tok1:expense:USD".to_string(),
            amount: ResourceAmount {
                value: dec!(50),
                commodity: "USD".to_string(),
            },
        },
        LedgerEntry {
            account: "token:tok1:budget:USD".to_string(),
            amount: ResourceAmount {
                value: dec!(-50),
                commodity: "USD".to_string(),
            },
        },
    ];

    let mut entry = JournalEntry::post_close_adjustment("USD", entries, "billing correction");
    entry.accrual_date = period_end - Duration::hours(1);

    let result = ledger.record(entry);
    assert!(result.is_ok(), "post-close adjustment should be allowed");
}

#[test]
fn test_period_close_allows_future_entries() {
    let mut ledger = ResourceLedger::new();

    let period_end = Utc::now() - Duration::hours(1);
    ledger.period_closes.push(PeriodClose {
        commodity: "USD".to_string(),
        period_end,
        closed_at: Utc::now(),
        closed_by: "admin".to_string(),
    });

    // Entry with accrual_date AFTER period_end should be fine
    let entry = JournalEntry::bootstrap("a", "USD", dec!(100));
    // accrual_date defaults to now, which is after period_end
    let result = ledger.record(entry);
    assert!(
        result.is_ok(),
        "future entries should not be blocked by period close"
    );
}

// ===========================================================================
// Persistence tests
// ===========================================================================

#[test]
fn test_save_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(1000),
        ))
        .unwrap();

    let r1 = make_reservation("tok1", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(200), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // Save
    persistence::save_journal(&ledger, &path).unwrap();

    // Load
    let replay = persistence::load_journal(&path).unwrap();
    assert_eq!(replay.torn_tail_bytes, 0, "clean file, nothing dropped");
    let loaded = replay.ledger;

    // Verify same journal size
    assert_eq!(loaded.journal.len(), ledger.journal.len());

    // Verify balances match
    assert_eq!(
        loaded.balance(&"agent:cfo:available:USD".to_string()),
        ledger.balance(&"agent:cfo:available:USD".to_string())
    );
    assert_eq!(
        loaded.balance(&"system:allocation:USD".to_string()),
        ledger.balance(&"system:allocation:USD".to_string())
    );
    assert_eq!(
        loaded.balance(&"token:tok1:budget:USD".to_string()),
        ledger.balance(&"token:tok1:budget:USD".to_string())
    );
    assert_eq!(
        loaded.balance(&"token:tok1:expense:USD".to_string()),
        ledger.balance(&"token:tok1:expense:USD".to_string())
    );

    // Audit passes
    assert!(loaded.audit().is_ok());
}

#[test]
fn test_append_only_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();

    // First save
    persistence::save_journal(&ledger, &path).unwrap();
    let size1 = std::fs::metadata(&path).unwrap().len();

    // Add more entries
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(1000),
        ))
        .unwrap();

    // Append only the new entries
    persistence::append_journal(&ledger, &path, 1).unwrap();
    let size2 = std::fs::metadata(&path).unwrap().len();

    assert!(
        size2 > size1,
        "file should grow after append: {} > {}",
        size2,
        size1
    );

    // Load and verify everything is there
    let loaded = persistence::load_journal(&path).unwrap().ledger;
    assert_eq!(loaded.journal.len(), 2);
}

/// A torn TRAILING record — a crash inside `append_journal` before the
/// newline — replays every committed record and drops only the fragment. The
/// fragment was never newline-committed, so no acknowledged spend is in it, but
/// the reader still reports that it dropped bytes.
#[test]
fn test_torn_trailing_record_replays_up_to_last_good_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(1000),
        ))
        .unwrap();
    persistence::save_journal(&ledger, &path).unwrap();

    // Simulate the crash: append the first half of a third record, no newline.
    let third = JournalEntry::token_issuance("cfo", "tok2", "USD", dec!(250));
    let serialized = serde_json::to_string(&third).unwrap();
    let fragment = &serialized[..serialized.len() / 2];
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(fragment.as_bytes());
    std::fs::write(&path, &bytes).unwrap();

    let replay = persistence::load_journal(&path).unwrap();
    assert_eq!(
        replay.ledger.journal.len(),
        2,
        "both committed records must survive"
    );
    assert_eq!(replay.torn_tail_bytes, fragment.len());
    // Balances are the ones the committed prefix implies — not zeroed.
    assert_eq!(
        replay.ledger.balance(&"token:tok1:budget:USD".to_string()),
        ledger.balance(&"token:tok1:budget:USD".to_string())
    );
    assert!(replay.ledger.audit().is_ok());
}

/// An INTERIOR unparseable line is not a torn append — those bytes WERE
/// newline-committed, and skipping them would silently understate spend. It
/// must surface as an error rather than replaying a short ledger.
#[test]
fn test_interior_corruption_is_surfaced_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(1000),
        ))
        .unwrap();
    persistence::save_journal(&ledger, &path).unwrap();

    // Mangle the FIRST line, leaving its newline intact.
    let content = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    assert_eq!(lines.len(), 2);
    lines[0] = "{ not json at all".to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let result = persistence::load_journal(&path);
    assert!(
        matches!(
            result,
            Err(persistence::PersistenceError::CorruptFile { line: 1, .. })
        ),
        "interior corruption must be reported, got {result:?}"
    );
}

/// A trailing line that IS newline-terminated but will not parse is interior
/// damage, not a torn append: the writer only fsyncs after the newline, so
/// those bytes were committed. Dropping them would understate spend.
#[test]
fn test_terminated_trailing_garbage_is_not_treated_as_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();
    persistence::save_journal(&ledger, &path).unwrap();

    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{ not json at all }\n");
    std::fs::write(&path, &bytes).unwrap();

    let result = persistence::load_journal(&path);
    assert!(
        matches!(
            result,
            Err(persistence::PersistenceError::CorruptFile { .. })
        ),
        "a committed garbage line must not be silently dropped, got {result:?}"
    );
}

#[test]
fn test_clean_journal_replays_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    let mut ledger = ResourceLedger::new();
    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();
    ledger
        .record(JournalEntry::token_issuance(
            "cfo",
            "tok1",
            "USD",
            dec!(1000),
        ))
        .unwrap();
    let r1 = make_reservation("tok1", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(200), &r1))
        .unwrap();
    persistence::save_journal(&ledger, &path).unwrap();

    let replay = persistence::load_journal(&path).unwrap();
    assert_eq!(replay.torn_tail_bytes, 0);
    assert_eq!(replay.ledger.journal.len(), ledger.journal.len());
    assert_eq!(replay.ledger.accounts.len(), ledger.accounts.len());
    for (id, account) in &ledger.accounts {
        assert_eq!(
            replay.ledger.balance(id),
            account.cached_balance,
            "balance for {id} must round-trip unchanged"
        );
    }
}

#[test]
fn test_empty_file_loads_as_empty_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    // Create empty file
    std::fs::write(&path, "").unwrap();

    let loaded = persistence::load_journal(&path).unwrap().ledger;
    assert!(loaded.journal.is_empty());
    assert!(loaded.accounts.is_empty());
}

#[test]
fn test_nonexistent_file_loads_as_empty_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.jsonl");

    let replay = persistence::load_journal(&path).unwrap();
    assert!(replay.ledger.journal.is_empty());
    assert!(replay.ledger.accounts.is_empty());
    assert_eq!(replay.torn_tail_bytes, 0);
}

// ===========================================================================
// Edge case tests
// ===========================================================================

#[test]
fn test_zero_amount_entry() {
    let mut ledger = ResourceLedger::new();

    // Zero amount should be allowed (marker entries)
    let entry = make_balanced_entry("a", "b", "USD", dec!(0));
    let result = ledger.record(entry);
    assert!(result.is_ok(), "zero amount entries should be allowed");
}

#[test]
fn test_negative_balance_allowed() {
    let ledger = bootstrap_ledger("cfo", "USD", dec!(5000));

    // system:allocation goes negative by design
    let alloc_balance = ledger.balance(&"system:allocation:USD".to_string());
    assert_eq!(alloc_balance, dec!(-5000));
    assert!(
        alloc_balance < dec!(0),
        "system:allocation should have negative balance"
    );
}

#[test]
fn test_very_large_amounts() {
    let mut ledger = ResourceLedger::new();

    let large = Decimal::MAX / Decimal::from(2);

    let entry = make_balanced_entry("a", "b", "USD", large);
    let result = ledger.record(entry);
    assert!(result.is_ok(), "large amounts should not panic");

    assert_eq!(ledger.balance(&"a".to_string()), large);
    assert_eq!(ledger.balance(&"b".to_string()), -large);
    assert!(ledger.audit().is_ok());
}

// ===========================================================================
// Full flow integration tests
// ===========================================================================

#[test]
fn test_full_reserve_commit_flow_conservation() {
    let mut ledger = setup_token_flow("cfo", "rct_ads", "USD", dec!(5000), dec!(500));

    // Reserve
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve(
            "rct_ads",
            "USD",
            dec!(100),
            &reservation,
        ))
        .unwrap();

    // Commit
    ledger.record(JournalEntry::commit(&reservation)).unwrap();

    // Audit must pass
    assert!(ledger.audit().is_ok());

    // Balances
    assert_eq!(
        ledger.balance(&"token:rct_ads:budget:USD".to_string()),
        dec!(400)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:reserved:USD".to_string()),
        dec!(0)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:expense:USD".to_string()),
        dec!(100)
    );
}

#[test]
fn test_full_reserve_rollback_flow_conservation() {
    let mut ledger = setup_token_flow("cfo", "rct_ads", "USD", dec!(5000), dec!(500));

    // Reserve
    let reservation = make_reservation("rct_ads", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve(
            "rct_ads",
            "USD",
            dec!(100),
            &reservation,
        ))
        .unwrap();

    // Rollback
    ledger.record(JournalEntry::rollback(&reservation)).unwrap();

    // Audit must pass
    assert!(ledger.audit().is_ok());

    // Budget restored
    assert_eq!(
        ledger.balance(&"token:rct_ads:budget:USD".to_string()),
        dec!(500)
    );
    assert_eq!(
        ledger.balance(&"token:rct_ads:reserved:USD".to_string()),
        dec!(0)
    );
}

#[test]
fn test_metered_commit_actual_less_than_estimate() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(5000), dec!(1000));

    // Reserve estimated 200
    let reservation = make_reservation("tok1", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve(
            "tok1",
            "USD",
            dec!(200),
            &reservation,
        ))
        .unwrap();

    // Actual cost was 150 — commit actual, return delta
    ledger
        .record(JournalEntry::commit_with_amount(&reservation, dec!(150)))
        .unwrap();
    let delta = dec!(200) - dec!(150);
    ledger
        .record(JournalEntry::return_delta(&reservation, delta))
        .unwrap();

    assert!(ledger.audit().is_ok());
    assert_eq!(
        ledger.balance(&"token:tok1:expense:USD".to_string()),
        dec!(150)
    );
    // Budget = 1000 - 200 (reserve) + 50 (return_delta) = 850
    assert_eq!(
        ledger.balance(&"token:tok1:budget:USD".to_string()),
        dec!(850)
    );
    // Reserved should be 0 after commit_with_amount(150) and return_delta(50): 200 - 150 - 50 = 0
    assert_eq!(
        ledger.balance(&"token:tok1:reserved:USD".to_string()),
        dec!(0)
    );
}

#[test]
fn test_metered_commit_actual_exceeds_estimate() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(5000), dec!(1000));

    // Reserve estimated 100
    let reservation = make_reservation("tok1", "USD", dec!(100));
    ledger
        .record(JournalEntry::reserve(
            "tok1",
            "USD",
            dec!(100),
            &reservation,
        ))
        .unwrap();

    // Actual cost was 150 — commit full reservation + overage
    ledger.record(JournalEntry::commit(&reservation)).unwrap();
    let overage = dec!(150) - dec!(100);
    ledger
        .record(JournalEntry::overage(&reservation, overage))
        .unwrap();

    assert!(ledger.audit().is_ok());
    assert_eq!(
        ledger.balance(&"token:tok1:expense:USD".to_string()),
        dec!(150)
    );
    // Budget = 1000 - 100 (reserve) - 50 (overage) = 850
    assert_eq!(
        ledger.balance(&"token:tok1:budget:USD".to_string()),
        dec!(850)
    );
}

#[test]
fn test_withdrawal_flow() {
    let mut ledger = bootstrap_ledger("cfo", "USD", dec!(5000));

    // Withdraw
    ledger
        .record(JournalEntry::withdrawal("cfo", "USD", dec!(2000)))
        .unwrap();

    assert_eq!(
        ledger.balance(&"agent:cfo:available:USD".to_string()),
        dec!(3000)
    );
    assert_eq!(
        ledger.balance(&"system:allocation:USD".to_string()),
        dec!(-3000)
    );
    assert!(ledger.audit().is_ok());
}

#[test]
fn test_refund_flow() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(5000), dec!(1000));

    // Spend
    let r1 = make_reservation("tok1", "USD", dec!(200));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(200), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    assert_eq!(
        ledger.balance(&"token:tok1:expense:USD".to_string()),
        dec!(200)
    );

    // Refund
    ledger
        .record(JournalEntry::refund(
            "tok1",
            "USD",
            dec!(50),
            "invalid click",
        ))
        .unwrap();

    assert_eq!(
        ledger.balance(&"token:tok1:expense:USD".to_string()),
        dec!(150)
    );
    assert_eq!(
        ledger.balance(&"token:tok1:budget:USD".to_string()),
        dec!(850)
    );
    assert!(ledger.audit().is_ok());
}

#[test]
fn test_vendor_credit_flow() {
    let mut ledger = ResourceLedger::new();

    ledger
        .record(JournalEntry::vendor_credit(
            "cfo",
            "USD",
            dec!(1000),
            "AWS promo",
        ))
        .unwrap();

    assert_eq!(
        ledger.balance(&"agent:cfo:available:USD".to_string()),
        dec!(1000)
    );
    assert_eq!(
        ledger.balance(&"system:vendor_credits:USD".to_string()),
        dec!(-1000)
    );
    assert!(ledger.audit().is_ok());
}

// ===========================================================================
// Burn rate and projection tests
// ===========================================================================

#[test]
fn test_burn_rate_with_no_spend() {
    let ledger = bootstrap_ledger("cfo", "USD", dec!(5000));
    let rate = ledger.burn_rate("USD", Duration::days(30));
    assert_eq!(rate, dec!(0));
}

#[test]
fn test_burn_rate_with_spend() {
    let mut ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));

    // Spend 300
    let r1 = make_reservation("tok1", "USD", dec!(300));
    ledger
        .record(JournalEntry::reserve("tok1", "USD", dec!(300), &r1))
        .unwrap();
    ledger.record(JournalEntry::commit(&r1)).unwrap();

    // Over 30 days: 300 / 30 = 10 per day
    let rate = ledger.burn_rate("USD", Duration::days(30));
    assert_eq!(rate, dec!(10));
}

#[test]
fn test_projected_exhaustion_no_budget() {
    let ledger = ResourceLedger::new();
    assert_eq!(ledger.projected_exhaustion("nonexistent"), None);
}

#[test]
fn test_days_of_runway_no_budget() {
    let ledger = ResourceLedger::new();
    assert_eq!(ledger.days_of_runway("nonexistent"), None);
}

#[test]
fn test_days_of_runway_no_spend() {
    let ledger = setup_token_flow("cfo", "tok1", "USD", dec!(10000), dec!(5000));
    // No spend means no burn rate — runway is infinite
    assert_eq!(ledger.days_of_runway("tok1"), None);
}

// ===========================================================================
// Accounts created on demand test
// ===========================================================================

#[test]
fn test_accounts_created_on_demand() {
    let mut ledger = ResourceLedger::new();
    assert!(ledger.accounts.is_empty());

    ledger
        .record(JournalEntry::bootstrap("cfo", "USD", dec!(5000)))
        .unwrap();

    assert!(ledger.accounts.contains_key("agent:cfo:available:USD"));
    assert!(ledger.accounts.contains_key("system:allocation:USD"));
    assert_eq!(ledger.accounts.len(), 2);
}

// ===========================================================================
// Multi-commodity zero-sum validation
// ===========================================================================

#[test]
fn test_cross_commodity_entry_requires_per_commodity_balance() {
    let mut ledger = ResourceLedger::new();

    // An entry that mixes commodities — each must individually sum to zero
    let entry = JournalEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        accrual_date: Utc::now(),
        entries: vec![
            LedgerEntry {
                account: "a:usd".to_string(),
                amount: ResourceAmount {
                    value: dec!(100),
                    commodity: "USD".to_string(),
                },
            },
            LedgerEntry {
                account: "b:usd".to_string(),
                amount: ResourceAmount {
                    value: dec!(-100),
                    commodity: "USD".to_string(),
                },
            },
            LedgerEntry {
                account: "a:email".to_string(),
                amount: ResourceAmount {
                    value: dec!(50),
                    commodity: "EMAIL_SENDS".to_string(),
                },
            },
            LedgerEntry {
                account: "b:email".to_string(),
                amount: ResourceAmount {
                    value: dec!(-50),
                    commodity: "EMAIL_SENDS".to_string(),
                },
            },
        ],
        reference: "multi_commodity".to_string(),
        agent_id: "test".to_string(),
        metadata: HashMap::new(),
    };

    assert!(
        ledger.record(entry).is_ok(),
        "multi-commodity entry with per-commodity balance should succeed"
    );

    // Now try one where USD balances but EMAIL_SENDS doesn't
    let bad_entry = JournalEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        accrual_date: Utc::now(),
        entries: vec![
            LedgerEntry {
                account: "a:usd".to_string(),
                amount: ResourceAmount {
                    value: dec!(100),
                    commodity: "USD".to_string(),
                },
            },
            LedgerEntry {
                account: "b:usd".to_string(),
                amount: ResourceAmount {
                    value: dec!(-100),
                    commodity: "USD".to_string(),
                },
            },
            LedgerEntry {
                account: "a:email".to_string(),
                amount: ResourceAmount {
                    value: dec!(50),
                    commodity: "EMAIL_SENDS".to_string(),
                },
            },
            // Missing the corresponding credit for EMAIL_SENDS
        ],
        reference: "bad_multi".to_string(),
        agent_id: "test".to_string(),
        metadata: HashMap::new(),
    };

    let result = ledger.record(bad_entry);
    assert!(result.is_err(), "imbalanced commodity should be rejected");
    match result.unwrap_err() {
        LedgerError::NotBalanced { commodity, .. } => {
            assert_eq!(commodity, "EMAIL_SENDS");
        },
        other => panic!("expected NotBalanced, got {:?}", other),
    }
}
