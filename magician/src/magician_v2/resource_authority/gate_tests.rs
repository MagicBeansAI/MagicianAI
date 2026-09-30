#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::{Duration, Utc};
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    use crate::magician_v2::resource_authority::gate::{
        check_stale_reservations, commit_spend, find_active_token, find_authorized_active_token,
        reserve_spend, reserve_spend_with_authority, rollback_spend, BudgetRejection,
        SystemFreezeState,
    };
    use crate::magician_v2::resource_authority::ledger::{
        JournalEntry, LedgerEntry, PeriodClose, Reservation, ResourceAmount, ResourceLedger,
    };
    use crate::magician_v2::resource_authority::recovery::find_unmatched_reservations;
    use crate::magician_v2::resource_authority::token::{
        CarryoverPolicy, CeilingPeriod, SpendToken, SystemCeiling, TokenStatus, VelocityLimit,
    };
    use crate::magician_v2::resource_authority::token_store::TokenStore;
    use crate::magician_v2::resource_authority::types::ReservationId;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_ledger() -> ResourceLedger {
        ResourceLedger::new()
    }

    fn make_token(id: &str, commodity: &str, ceiling: Decimal) -> SpendToken {
        SpendToken {
            id: id.to_string(),
            issued_by: "cfo".to_string(),
            issued_to: "agent_x".to_string(),
            commodity: commodity.to_string(),
            ceiling,
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: None,
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        }
    }

    fn make_system_ceiling(
        commodity: &str,
        ceiling: Decimal,
        relaxation: Decimal,
    ) -> SystemCeiling {
        SystemCeiling {
            id: format!("sc_{}", commodity),
            commodity: commodity.to_string(),
            ceiling,
            relaxation,
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
        }
    }

    /// Bootstrap a ledger with initial funds for a token.
    fn bootstrap_token(ledger: &mut ResourceLedger, token: &SpendToken) {
        // Bootstrap: DR agent:{issuer}:available:{commodity}, CR system:allocation
        let bootstrap = JournalEntry::bootstrap(&token.issued_by, &token.commodity, token.ceiling);
        ledger.record(bootstrap).unwrap();
        // Issue: DR token:{id}:budget:{commodity}, CR agent:{issuer}:available:{commodity}
        let issuance = JournalEntry::token_issuance(
            &token.issued_by,
            &token.id,
            &token.commodity,
            token.ceiling,
        );
        ledger.record(issuance).unwrap();
    }

    fn default_freeze() -> SystemFreezeState {
        SystemFreezeState::default()
    }

    // -----------------------------------------------------------------------
    // Token lifecycle
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_token_active() {
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        assert_eq!(token.status, TokenStatus::Active);
        assert_eq!(token.ceiling, Decimal::new(500, 0));
    }

    #[test]
    fn test_revoke_sets_status() {
        let mut store = TokenStore::new();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        store.create(token).unwrap();
        store.revoke("t1").unwrap();
        assert_eq!(store.get("t1").unwrap().status, TokenStatus::Revoked);
    }

    #[test]
    fn test_expired_token_detected() {
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        token.expires_at = Some(Utc::now() - Duration::hours(1));
        assert!(token.is_expired());
    }

    #[test]
    fn test_revoked_token_blocks_spend() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);
        token.status = TokenStatus::Revoked;

        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(10, 0),
            "agent",
        );
        assert!(matches!(result, Err(BudgetRejection::TokenInactive(_))));
    }

    // -----------------------------------------------------------------------
    // Reserve/commit/rollback
    // -----------------------------------------------------------------------

    #[test]
    fn test_reserve_deducts_budget() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        let budget = ledger.balance(&token.budget_account());
        assert_eq!(budget, Decimal::new(400, 0));
        assert!(ledger.active_reservations.contains_key(&rid));
    }

    #[test]
    fn test_commit_moves_to_expense() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        commit_spend(&mut ledger, &rid, None).unwrap();

        let expense = ledger.balance(&token.expense_account());
        assert_eq!(expense, Decimal::new(100, 0));
        let reserved = ledger.balance(&token.reserved_account());
        assert_eq!(reserved, Decimal::ZERO);
        assert!(!ledger.active_reservations.contains_key(&rid));
    }

    #[test]
    fn test_rollback_restores_budget() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        rollback_spend(&mut ledger, &rid).unwrap();

        let budget = ledger.balance(&token.budget_account());
        assert_eq!(budget, Decimal::new(500, 0));
        let reserved = ledger.balance(&token.reserved_account());
        assert_eq!(reserved, Decimal::ZERO);
    }

    #[test]
    fn test_reserve_commit_conservation() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &rid, None).unwrap();

        assert!(ledger.audit().is_ok());
    }

    #[test]
    fn test_reserve_rollback_conservation() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        rollback_spend(&mut ledger, &rid).unwrap();

        assert!(ledger.audit().is_ok());
    }

    #[test]
    fn test_double_commit_rejected() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &rid, None).unwrap();
        assert!(commit_spend(&mut ledger, &rid, None).is_err());
    }

    #[test]
    fn test_double_rollback_rejected() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        rollback_spend(&mut ledger, &rid).unwrap();
        assert!(rollback_spend(&mut ledger, &rid).is_err());
    }

    // -----------------------------------------------------------------------
    // Ceiling enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn test_reserve_within_ceiling_succeeds() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);
        let sc = make_system_ceiling("USD", Decimal::new(5000, 0), Decimal::ZERO);

        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(100, 0),
            "agent",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_reserve_exceeding_ceiling_rejected() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(600, 0),
            "agent",
        );
        assert!(matches!(result, Err(BudgetRejection::TokenExceeded { .. })));
    }

    #[test]
    fn test_system_ceiling_with_relaxation() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(5200, 0));
        bootstrap_token(&mut ledger, &token);

        // System ceiling is 5000 with 2% relaxation => hard ceiling = 5100
        let sc = SystemCeiling {
            id: "sc_usd".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(5000, 0),
            relaxation: Decimal::new(2, 2), // 0.02
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
        };

        // $5050 is within relaxation (5000 * 1.02 = 5100)
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(5050, 0),
            "agent",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_system_ceiling_includes_reservations() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(5000, 0));
        bootstrap_token(&mut ledger, &token);
        let sc = make_system_ceiling("USD", Decimal::new(5000, 0), Decimal::ZERO);

        // First reservation: 3000
        let _rid1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc.clone()],
            Decimal::new(3000, 0),
            "agent",
        )
        .unwrap();

        // Second reservation: 2500 — should fail because budget is now 2000
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(2500, 0),
            "agent",
        );
        assert!(matches!(result, Err(BudgetRejection::TokenExceeded { .. })));
    }

    #[test]
    fn test_concurrent_reserves_cannot_overshoot() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        // First reserve takes 300
        let _r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(300, 0),
            "agent1",
        )
        .unwrap();

        // Second reserve tries 300 — should fail because budget is now 200
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(300, 0),
            "agent2",
        );
        assert!(matches!(result, Err(BudgetRejection::TokenExceeded { .. })));

        // 200 should succeed
        let r2 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(200, 0),
            "agent2",
        );
        assert!(r2.is_ok());
    }

    // -----------------------------------------------------------------------
    // Velocity limits
    // -----------------------------------------------------------------------

    #[test]
    fn test_velocity_limit_blocks_fast_spend() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(5000, 0));
        token.velocity_limit = Some(VelocityLimit {
            max_amount: Decimal::new(500, 0),
            window_seconds: 3600,
        });
        bootstrap_token(&mut ledger, &token);

        // First spend: 400
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(400, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &r1, None).unwrap();

        // Second spend: 200 — would exceed velocity (400 + 200 > 500)
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(200, 0),
            "agent",
        );
        assert!(matches!(
            result,
            Err(BudgetRejection::VelocityExceeded { .. })
        ));
    }

    #[test]
    fn test_velocity_limit_allows_slow_spend() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(5000, 0));
        token.velocity_limit = Some(VelocityLimit {
            max_amount: Decimal::new(500, 0),
            window_seconds: 3600,
        });
        bootstrap_token(&mut ledger, &token);

        // Spend 400
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(400, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &r1, None).unwrap();

        // Spend 100 more — still within velocity (400 + 100 = 500)
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_velocity_independent_of_period_ceiling() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(50000, 0));
        token.velocity_limit = Some(VelocityLimit {
            max_amount: Decimal::new(100, 0),
            window_seconds: 3600,
        });
        bootstrap_token(&mut ledger, &token);

        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(80, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &r1, None).unwrap();

        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(30, 0),
            "agent",
        );
        assert!(matches!(
            result,
            Err(BudgetRejection::VelocityExceeded { .. })
        ));
    }

    #[test]
    fn test_velocity_includes_reservations() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(5000, 0));
        token.velocity_limit = Some(VelocityLimit {
            max_amount: Decimal::new(500, 0),
            window_seconds: 3600,
        });
        bootstrap_token(&mut ledger, &token);

        // Reserve 400 (not committed yet)
        let _r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(400, 0),
            "agent",
        )
        .unwrap();

        // Another 200 should be blocked by velocity (400 reserved + 200 > 500)
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(200, 0),
            "agent",
        );
        assert!(matches!(
            result,
            Err(BudgetRejection::VelocityExceeded { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // Budget freeze
    // -----------------------------------------------------------------------

    #[test]
    fn test_budget_freeze_blocks_all_reserves() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let mut freeze = SystemFreezeState::default();
        freeze.freeze("admin", "Emergency stop");

        let result = reserve_spend(
            &freeze,
            &mut ledger,
            &token,
            &[],
            Decimal::new(10, 0),
            "agent",
        );
        assert!(matches!(result, Err(BudgetRejection::SystemFrozen { .. })));
    }

    #[test]
    fn test_budget_freeze_doesnt_affect_commits() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        // Commit after freeze — should still work
        let result = commit_spend(&mut ledger, &rid, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unfreeze_resumes_spending() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let mut freeze = SystemFreezeState::default();
        freeze.freeze("admin", "Emergency stop");

        assert!(reserve_spend(
            &freeze,
            &mut ledger,
            &token,
            &[],
            Decimal::new(10, 0),
            "agent",
        )
        .is_err());

        freeze.unfreeze();

        let result = reserve_spend(
            &freeze,
            &mut ledger,
            &token,
            &[],
            Decimal::new(10, 0),
            "agent",
        );
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // Lazy token expiry in find_active_token
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_active_skips_expired() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();

        let mut t1 = make_token("t1", "USD", Decimal::new(500, 0));
        t1.expires_at = Some(Utc::now() - Duration::hours(1));
        bootstrap_token(&mut ledger, &t1);
        store.create(t1).unwrap();

        let t2 = make_token("t2", "USD", Decimal::new(300, 0));
        bootstrap_token(&mut ledger, &t2);
        store.create(t2).unwrap();

        let result = find_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string(), "t2".to_string()],
            "USD",
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "t2");
    }

    #[test]
    fn test_lazy_expiry_records_revert() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();

        let mut t1 = make_token("t1", "USD", Decimal::new(500, 0));
        t1.expires_at = Some(Utc::now() - Duration::hours(1));
        bootstrap_token(&mut ledger, &t1);
        store.create(t1).unwrap();

        let t2 = make_token("t2", "USD", Decimal::new(100, 0));
        bootstrap_token(&mut ledger, &t2);
        store.create(t2).unwrap();

        let issuer_balance_before = ledger.balance(&"agent:cfo:available:USD".to_string());

        let _result = find_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string(), "t2".to_string()],
            "USD",
        );

        let issuer_balance_after = ledger.balance(&"agent:cfo:available:USD".to_string());
        assert_eq!(
            issuer_balance_after - issuer_balance_before,
            Decimal::new(500, 0)
        );
    }

    #[test]
    fn test_lazy_expiry_sets_status() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();

        let mut t1 = make_token("t1", "USD", Decimal::new(500, 0));
        t1.expires_at = Some(Utc::now() - Duration::hours(1));
        bootstrap_token(&mut ledger, &t1);
        store.create(t1).unwrap();

        let t2 = make_token("t2", "USD", Decimal::new(100, 0));
        bootstrap_token(&mut ledger, &t2);
        store.create(t2).unwrap();

        let _result = find_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string(), "t2".to_string()],
            "USD",
        );

        assert_eq!(store.get("t1").unwrap().status, TokenStatus::Expired);
    }

    #[test]
    fn test_already_expired_no_double_revert() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();

        let mut t1 = make_token("t1", "USD", Decimal::new(500, 0));
        t1.expires_at = Some(Utc::now() - Duration::hours(1));
        bootstrap_token(&mut ledger, &t1);
        store.create(t1).unwrap();

        let t2 = make_token("t2", "USD", Decimal::new(100, 0));
        bootstrap_token(&mut ledger, &t2);
        store.create(t2).unwrap();

        // First call triggers lazy expiry
        let _r1 = find_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string(), "t2".to_string()],
            "USD",
        );

        let issuer_balance_after_first = ledger.balance(&"agent:cfo:available:USD".to_string());

        // Second call — t1 is already Expired, should NOT revert again
        let _r2 = find_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string(), "t2".to_string()],
            "USD",
        );

        let issuer_balance_after_second = ledger.balance(&"agent:cfo:available:USD".to_string());
        assert_eq!(issuer_balance_after_first, issuer_balance_after_second);
    }

    // -----------------------------------------------------------------------
    // Owner-aware authorization
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_authorized_active_token_accepts_active_owner() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        token.issued_to = "target_agent".to_string();
        bootstrap_token(&mut ledger, &token);
        store.create(token).unwrap();

        let result = find_authorized_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string()],
            "USD",
            "target_agent",
            &[],
        )
        .unwrap();
        assert_eq!(result.id, "t1");
    }

    #[test]
    fn test_find_authorized_active_token_accepts_inherited_owner() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        token.issued_to = "agent_a".to_string();
        bootstrap_token(&mut ledger, &token);
        store.create(token).unwrap();

        let result = find_authorized_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string()],
            "USD",
            "agent_b",
            &["agent_a".to_string()],
        )
        .unwrap();
        assert_eq!(result.id, "t1");
    }

    #[test]
    fn test_find_authorized_active_token_rejects_unrelated_owner() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        token.issued_to = "target".to_string();
        bootstrap_token(&mut ledger, &token);
        store.create(token).unwrap();

        let result = find_authorized_active_token(
            &mut ledger,
            &mut store,
            &["t1".to_string()],
            "USD",
            "agent_b",
            &["agent_a".to_string()],
        );
        assert!(matches!(
            result,
            Err(BudgetRejection::UnauthorizedSpender {
                token_id,
                issued_to,
                active_owner_agent_id,
            }) if token_id == "t1"
                && issued_to == "target"
                && active_owner_agent_id == "agent_b"
        ));
    }

    #[test]
    fn test_reserve_spend_with_authority_records_active_owner() {
        let mut ledger = make_ledger();
        let mut token = make_token("t1", "USD", Decimal::new(500, 0));
        token.issued_to = "agent_a".to_string();
        bootstrap_token(&mut ledger, &token);

        let reservation_id = reserve_spend_with_authority(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(25, 0),
            "agent_b",
            &["agent_a".to_string()],
        )
        .unwrap();

        let reservation = ledger
            .active_reservations
            .get(&reservation_id)
            .expect("reservation should exist");
        assert_eq!(reservation.agent_id, "agent_b");
    }

    // -----------------------------------------------------------------------
    // Stacked ceilings
    // -----------------------------------------------------------------------

    #[test]
    fn test_stacked_ceilings_all_checked() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(200000, 0));
        bootstrap_token(&mut ledger, &token);

        let ceilings = vec![
            SystemCeiling {
                id: "annual".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(600000, 0),
                relaxation: Decimal::ZERO,
                period: CeilingPeriod::Total,
                carryover: CarryoverPolicy::None,
            },
            SystemCeiling {
                id: "quarterly".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(150000, 0),
                relaxation: Decimal::ZERO,
                period: CeilingPeriod::Total,
                carryover: CarryoverPolicy::None,
            },
            SystemCeiling {
                id: "monthly".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(50000, 0),
                relaxation: Decimal::ZERO,
                period: CeilingPeriod::Total,
                carryover: CarryoverPolicy::None,
            },
        ];

        // $40k — within all three
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &ceilings,
            Decimal::new(40000, 0),
            "agent",
        );
        assert!(r1.is_ok());
    }

    #[test]
    fn test_stacked_ceilings_most_restrictive_wins() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(200000, 0));
        bootstrap_token(&mut ledger, &token);

        let ceilings = vec![
            SystemCeiling {
                id: "annual".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(600000, 0),
                relaxation: Decimal::ZERO,
                period: CeilingPeriod::Total,
                carryover: CarryoverPolicy::None,
            },
            SystemCeiling {
                id: "restrictive".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(1000, 0),
                relaxation: Decimal::ZERO,
                period: CeilingPeriod::Total,
                carryover: CarryoverPolicy::None,
            },
        ];

        // $2000 passes 600k but fails 1k
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &ceilings,
            Decimal::new(2000, 0),
            "agent",
        );
        assert!(matches!(
            result,
            Err(BudgetRejection::SystemCeilingExceeded { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // Period-aware ceiling enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn test_ceiling_period_total_never_resets() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(1000, 0));
        bootstrap_token(&mut ledger, &token);
        let sc = make_system_ceiling("USD", Decimal::new(1000, 0), Decimal::ZERO);

        // Spend 800
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc.clone()],
            Decimal::new(800, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &r1, None).unwrap();

        // Try 300 more — should fail (800 + 300 > 1000)
        let result = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(300, 0),
            "agent",
        );
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Carryover enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn test_carryover_none_resets_cleanly() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(5000, 0));
        bootstrap_token(&mut ledger, &token);

        let sc = SystemCeiling {
            id: "sc".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(5000, 0),
            relaxation: Decimal::ZERO,
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
        };

        // Spend 3000
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc.clone()],
            Decimal::new(3000, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &r1, None).unwrap();

        // Remaining = 2000
        let r2 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(2000, 0),
            "agent",
        );
        assert!(r2.is_ok());
    }

    #[test]
    fn test_carryover_no_negative_rollover() {
        // Test that compute_effective_ceiling clamps unspent to zero via .max(Decimal::ZERO).
        // With CeilingPeriod::Total and Full carryover, previous_period_window returns
        // (MIN_UTC, MIN_UTC) so no previous spend exists, meaning unspent = ceiling.
        // For a direct test of the .max(0) guard: we verify that the system ceiling
        // with CarryoverPolicy::None just uses base ceiling (no rollover at all).
        // Also verify that with relaxation, the hard ceiling is ceiling * (1 + relaxation).
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(6000, 0));
        bootstrap_token(&mut ledger, &token);

        // System ceiling: 5000 with 2% relaxation => hard ceiling = 5100
        // CarryoverPolicy::None means effective ceiling = base ceiling = 5000
        let sc = SystemCeiling {
            id: "sc".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(5000, 0),
            relaxation: Decimal::new(2, 2), // 0.02 => 5100
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
        };

        // Spend up to 5050 (within relaxation)
        let r1 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc.clone()],
            Decimal::new(5050, 0),
            "agent",
        );
        assert!(r1.is_ok());

        let rid = r1.unwrap();
        commit_spend(&mut ledger, &rid, None).unwrap();

        // Try 100 more: 5050 + 100 = 5150 > 5100 = hard ceiling. Should fail.
        let r2 = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[sc],
            Decimal::new(100, 0),
            "agent",
        );
        assert!(matches!(
            r2,
            Err(BudgetRejection::SystemCeilingExceeded { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // Metered commit scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn test_metered_commit_actual_less_than_estimate() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        commit_spend(&mut ledger, &rid, Some(Decimal::new(60, 0))).unwrap();

        let budget = ledger.balance(&token.budget_account());
        assert_eq!(budget, Decimal::new(440, 0)); // 500 - 100 + 40 = 440
        let expense = ledger.balance(&token.expense_account());
        assert_eq!(expense, Decimal::new(60, 0));
        assert!(ledger.audit().is_ok());
    }

    #[test]
    fn test_metered_commit_actual_exceeds_estimate() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        commit_spend(&mut ledger, &rid, Some(Decimal::new(150, 0))).unwrap();

        let budget = ledger.balance(&token.budget_account());
        assert_eq!(budget, Decimal::new(350, 0)); // 500 - 100 - 50 = 350
        let expense = ledger.balance(&token.expense_account());
        assert_eq!(expense, Decimal::new(150, 0));
        assert!(ledger.audit().is_ok());
    }

    // -----------------------------------------------------------------------
    // Stale reservation detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_stale_reservation_detection() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let reservation_id = ReservationId::new();
        let reservation = Reservation {
            id: reservation_id.clone(),
            token_id: token.id.clone(),
            commodity: "USD".to_string(),
            amount: Decimal::new(50, 0),
            agent_id: "agent".to_string(),
            created_at: Utc::now() - Duration::minutes(5),
            idempotency_key: "test".to_string(),
            max_duration_secs: 60, // 1 minute — already stale
            batch_id: None,
        };

        ledger
            .record(JournalEntry::reserve(
                &token.id,
                &token.commodity,
                Decimal::new(50, 0),
                &reservation,
            ))
            .unwrap();
        ledger
            .active_reservations
            .insert(reservation_id.clone(), reservation);

        let stale = check_stale_reservations(&ledger);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, reservation_id);
    }

    // -----------------------------------------------------------------------
    // Crash recovery
    // -----------------------------------------------------------------------

    #[test]
    fn test_unmatched_reserve_detected() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        let unmatched = find_unmatched_reservations(&ledger);
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].reservation_id, rid);
    }

    #[test]
    fn test_matched_reserve_not_flagged() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &rid, None).unwrap();

        let unmatched = find_unmatched_reservations(&ledger);
        assert_eq!(unmatched.len(), 0);
    }

    #[test]
    fn test_recovery_after_restart() {
        use crate::magician_v2::resource_authority::recovery::reconstruct_active_reservations;

        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(500, 0));
        bootstrap_token(&mut ledger, &token);

        let rid = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();

        // Simulate crash: clear active_reservations
        ledger.active_reservations.clear();

        // Reconstruct from journal
        reconstruct_active_reservations(&mut ledger);

        // The reservation should be reconstructed
        assert!(ledger.active_reservations.contains_key(&rid));

        let unmatched = find_unmatched_reservations(&ledger);
        assert_eq!(unmatched.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Period close
    // -----------------------------------------------------------------------

    #[test]
    fn test_period_close_rejects_backdated() {
        let mut ledger = make_ledger();

        let yesterday = Utc::now() - Duration::days(1);
        let close = PeriodClose {
            commodity: "USD".to_string(),
            period_end: yesterday,
            closed_at: Utc::now(),
            closed_by: "admin".to_string(),
        };
        ledger.period_closes.push(close);

        let entry = JournalEntry {
            id: uuid::Uuid::new_v4(),
            timestamp: Utc::now(),
            accrual_date: yesterday - Duration::hours(2),
            entries: vec![
                LedgerEntry {
                    account: "agent:x:available:USD".to_string(),
                    amount: ResourceAmount {
                        value: Decimal::new(100, 0),
                        commodity: "USD".to_string(),
                    },
                },
                LedgerEntry {
                    account: "system:allocation:USD".to_string(),
                    amount: ResourceAmount {
                        value: Decimal::new(-100, 0),
                        commodity: "USD".to_string(),
                    },
                },
            ],
            reference: "test_backdated".to_string(),
            agent_id: "test".to_string(),
            metadata: HashMap::new(),
        };

        let result = ledger.record(entry);
        assert!(result.is_err());
    }

    #[test]
    fn wildcard_issued_to_authorizes_any_matching_scope() {
        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("cfg-agent-*-USD", "USD", Decimal::new(10, 0));
        token.issued_to = "agent:*".to_string();
        bootstrap_token(&mut ledger, &token);
        store.create(token).unwrap();

        let found = find_authorized_active_token(
            &mut ledger,
            &mut store,
            &["cfg-agent-*-USD".to_string()],
            "usd",
            "chat:session",
            &["principal:owner".to_string(), "agent:presto".to_string()],
        )
        .unwrap();
        assert_eq!(found.id, "cfg-agent-*-USD");
    }

    #[test]
    fn stacked_tokens_all_reserve_and_second_failure_rolls_back_first() {
        use crate::magician_v2::resource_authority::gate::{
            reserve_authorized_stack, rollback_spend_group,
        };

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut principal = make_token("cfg-principal-owner-USD", "USD", Decimal::new(100, 0));
        principal.issued_to = "principal:owner".to_string();
        let mut tool = make_token("cfg-tool-send-USD", "USD", Decimal::new(1, 0));
        tool.issued_to = "tool:send".to_string();
        bootstrap_token(&mut ledger, &principal);
        bootstrap_token(&mut ledger, &tool);
        store.create(principal).unwrap();
        store.create(tool).unwrap();

        let chain = vec![
            "principal:owner".to_string(),
            "agent:presto".to_string(),
            "tool:send".to_string(),
        ];
        let freeze = default_freeze();
        let ids = reserve_authorized_stack(
            &freeze,
            &mut ledger,
            &mut store,
            &[
                "cfg-principal-owner-USD".to_string(),
                "cfg-tool-send-USD".to_string(),
            ],
            "USD",
            Decimal::new(1, 0),
            "chat:1",
            &chain,
            &[],
        )
        .unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(
            ledger.balance(&"token:cfg-principal-owner-USD:budget:USD".to_string()),
            Decimal::new(99, 0)
        );
        assert_eq!(
            ledger.balance(&"token:cfg-tool-send-USD:budget:USD".to_string()),
            Decimal::ZERO
        );

        rollback_spend_group(&mut ledger, &ids[0]).unwrap();

        let rejected = reserve_authorized_stack(
            &freeze,
            &mut ledger,
            &mut store,
            &[
                "cfg-principal-owner-USD".to_string(),
                "cfg-tool-send-USD".to_string(),
            ],
            "USD",
            Decimal::new(2, 0),
            "chat:1",
            &chain,
            &[],
        );
        assert!(rejected.is_err());
        assert_eq!(
            ledger.balance(&"token:cfg-principal-owner-USD:budget:USD".to_string()),
            Decimal::new(100, 0),
            "failed stack must roll back the principal reservation"
        );
        assert_eq!(
            ledger.balance(&"token:cfg-tool-send-USD:budget:USD".to_string()),
            Decimal::new(1, 0)
        );
    }

    #[test]
    fn stacked_reserve_batch_survives_journal_replay() {
        use crate::magician_v2::resource_authority::gate::{
            commit_spend_group, reserve_authorized_stack,
        };
        use crate::magician_v2::resource_authority::recovery::reconstruct_active_reservations;

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut principal = make_token("cfg-principal-owner-USD", "USD", Decimal::new(100, 0));
        principal.issued_to = "principal:owner".to_string();
        let mut tool = make_token("cfg-tool-send-USD", "USD", Decimal::new(10, 0));
        tool.issued_to = "tool:send".to_string();
        bootstrap_token(&mut ledger, &principal);
        bootstrap_token(&mut ledger, &tool);
        store.create(principal).unwrap();
        store.create(tool).unwrap();

        let chain = vec![
            "principal:owner".to_string(),
            "agent:presto".to_string(),
            "tool:send".to_string(),
        ];
        let ids = reserve_authorized_stack(
            &default_freeze(),
            &mut ledger,
            &mut store,
            &[
                "cfg-principal-owner-USD".to_string(),
                "cfg-tool-send-USD".to_string(),
            ],
            "USD",
            Decimal::new(1, 0),
            "chat:1",
            &chain,
            &[],
        )
        .unwrap();
        assert_eq!(ids.len(), 2);
        let live_batch = ledger
            .active_reservations
            .get(&ids[0])
            .and_then(|reservation| reservation.batch_id.clone());
        assert!(live_batch.is_some());
        assert_eq!(
            ledger
                .active_reservations
                .get(&ids[1])
                .and_then(|reservation| reservation.batch_id.clone()),
            live_batch
        );

        ledger.active_reservations.clear();
        reconstruct_active_reservations(&mut ledger);
        assert_eq!(
            ledger
                .active_reservations
                .get(&ids[0])
                .and_then(|reservation| reservation.batch_id.clone()),
            live_batch,
            "replay must restore the stack batch so commit settles every member"
        );
        commit_spend_group(&mut ledger, &ids[0], None).unwrap();
        assert_eq!(
            ledger.balance(&"token:cfg-principal-owner-USD:reserved:USD".to_string()),
            Decimal::ZERO
        );
        assert_eq!(
            ledger.balance(&"token:cfg-tool-send-USD:reserved:USD".to_string()),
            Decimal::ZERO
        );
    }

    #[test]
    fn stacked_tokens_count_once_against_a_system_ceiling() {
        use crate::magician_v2::resource_authority::gate::reserve_authorized_stack;

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut principal = make_token("cfg-principal-owner-USD", "USD", Decimal::new(200, 0));
        principal.issued_to = "principal:owner".to_string();
        let mut tool = make_token("cfg-tool-send-USD", "USD", Decimal::new(200, 0));
        tool.issued_to = "tool:send".to_string();
        bootstrap_token(&mut ledger, &principal);
        bootstrap_token(&mut ledger, &tool);
        store.create(principal).unwrap();
        store.create(tool).unwrap();

        let ceiling = SystemCeiling {
            id: "usd-cap".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(150, 0),
            relaxation: Decimal::ZERO,
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
        };
        let chain = vec![
            "principal:owner".to_string(),
            "agent:presto".to_string(),
            "tool:send".to_string(),
        ];
        let ids = reserve_authorized_stack(
            &default_freeze(),
            &mut ledger,
            &mut store,
            &[
                "cfg-principal-owner-USD".to_string(),
                "cfg-tool-send-USD".to_string(),
            ],
            "USD",
            Decimal::new(100, 0),
            "chat:1",
            &chain,
            &[ceiling.clone()],
        )
        .expect("one $100 spend must not be counted twice against a $150 system ceiling");
        assert_eq!(ids.len(), 2);
        crate::magician_v2::resource_authority::gate::commit_spend_group(
            &mut ledger,
            &ids[0],
            None,
        )
        .unwrap();
        let second = reserve_authorized_stack(
            &default_freeze(),
            &mut ledger,
            &mut store,
            &[
                "cfg-principal-owner-USD".to_string(),
                "cfg-tool-send-USD".to_string(),
            ],
            "USD",
            Decimal::new(40, 0),
            "chat:1",
            &chain,
            &[ceiling],
        );
        assert!(
            second.is_ok(),
            "committed stacked $100 must count as $100 against a $150 system ceiling, not $200"
        );
    }

    #[test]
    fn in_flight_reservation_from_the_previous_period_still_counts_against_the_ceiling() {
        let mut ledger = make_ledger();
        let token = make_token("t1", "USD", Decimal::new(200, 0));
        bootstrap_token(&mut ledger, &token);
        let ceiling = SystemCeiling {
            id: "usd-daily".to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(150, 0),
            relaxation: Decimal::ZERO,
            period: CeilingPeriod::Daily,
            carryover: CarryoverPolicy::None,
        };
        let first = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[ceiling.clone()],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        let yesterday = Utc::now() - Duration::days(1);
        for entry in &mut ledger.journal {
            if entry.reference == format!("reserve:{}", first) {
                entry.accrual_date = yesterday;
            }
        }
        let blocked = reserve_spend(
            &default_freeze(),
            &mut ledger,
            &token,
            &[ceiling],
            Decimal::new(60, 0),
            "agent",
        );
        assert!(
            matches!(blocked, Err(BudgetRejection::SystemCeilingExceeded { .. })),
            "a hold that crossed midnight must still occupy today's system ceiling, got {blocked:?}"
        );
    }

    #[test]
    fn period_refill_does_not_ignore_an_in_flight_token_hold() {
        use crate::magician_v2::resource_authority::gate::ensure_period_funding;
        use crate::magician_v2::resource_authority::token::previous_period_window;

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("daily-usd", "USD", Decimal::new(100, 0));
        token.period = CeilingPeriod::Daily;
        token.last_period_start = Some(
            crate::magician_v2::resource_authority::token::period_window_start(
                &CeilingPeriod::Daily,
            ),
        );
        bootstrap_token(&mut ledger, &token);
        store.create(token.clone()).unwrap();

        let freeze = default_freeze();
        let first = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-usd").unwrap(),
            &[],
            Decimal::new(100, 0),
            "agent",
        )
        .unwrap();
        let yesterday = Utc::now() - Duration::days(1);
        for entry in &mut ledger.journal {
            if entry.reference == format!("reserve:{}", first) {
                entry.accrual_date = yesterday;
            }
        }
        store.get_mut("daily-usd").unwrap().last_period_start =
            Some(previous_period_window(&CeilingPeriod::Daily).0);
        ensure_period_funding(&mut ledger, &mut store, "daily-usd").unwrap();
        let blocked = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-usd").unwrap(),
            &[],
            Decimal::new(100, 0),
            "agent",
        );
        assert!(
            matches!(blocked, Err(BudgetRejection::TokenExceeded { .. })),
            "new-period funding must not stack on top of yesterday's still-reserved hold, got {blocked:?}"
        );
    }

    #[test]
    fn daily_period_refills_after_window_rolls() {
        use crate::magician_v2::resource_authority::gate::ensure_period_funding;
        use crate::magician_v2::resource_authority::token::{
            period_window_start, previous_period_window,
        };

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("daily-email", "EMAIL_SENDS", Decimal::new(2, 0));
        token.period = CeilingPeriod::Daily;
        token.last_period_start = Some(period_window_start(&CeilingPeriod::Daily));
        bootstrap_token(&mut ledger, &token);
        store.create(token.clone()).unwrap();

        let freeze = default_freeze();
        let first = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-email").unwrap(),
            &[],
            Decimal::new(2, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &first, None).unwrap();
        assert_eq!(
            ledger.balance(&"token:daily-email:budget:EMAIL_SENDS".to_string()),
            Decimal::ZERO
        );

        let blocked = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-email").unwrap(),
            &[],
            Decimal::new(1, 0),
            "agent",
        );
        assert!(
            blocked.is_err(),
            "lifetime budget still empty before refill"
        );

        let previous_period = previous_period_window(&CeilingPeriod::Daily).0;
        for entry in &mut ledger.journal {
            if entry.reference == format!("reserve:{first}")
                || entry.reference == format!("commit:{first}")
            {
                entry.accrual_date = previous_period;
            }
        }
        store.get_mut("daily-email").unwrap().last_period_start = Some(previous_period);
        ensure_period_funding(&mut ledger, &mut store, "daily-email").unwrap();
        let after_refill = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-email").unwrap(),
            &[],
            Decimal::new(2, 0),
            "agent",
        );
        assert!(after_refill.is_ok(), "new period must re-fund the ceiling");
    }

    #[test]
    fn daily_none_carryover_does_not_keep_leftover() {
        use crate::magician_v2::resource_authority::gate::ensure_period_funding;
        use crate::magician_v2::resource_authority::token::previous_period_window;

        let mut ledger = make_ledger();
        let mut store = TokenStore::new();
        let mut token = make_token("daily-usd", "USD", Decimal::new(10, 0));
        token.period = CeilingPeriod::Daily;
        token.carryover = CarryoverPolicy::None;
        token.last_period_start = Some(
            crate::magician_v2::resource_authority::token::period_window_start(
                &CeilingPeriod::Daily,
            ),
        );
        bootstrap_token(&mut ledger, &token);
        store.create(token.clone()).unwrap();

        let freeze = default_freeze();
        let rid = reserve_spend(
            &freeze,
            &mut ledger,
            store.get("daily-usd").unwrap(),
            &[],
            Decimal::new(3, 0),
            "agent",
        )
        .unwrap();
        commit_spend(&mut ledger, &rid, None).unwrap();
        assert_eq!(
            ledger.balance(&"token:daily-usd:budget:USD".to_string()),
            Decimal::new(7, 0)
        );

        store.get_mut("daily-usd").unwrap().last_period_start =
            Some(previous_period_window(&CeilingPeriod::Daily).0);
        ensure_period_funding(&mut ledger, &mut store, "daily-usd").unwrap();
        assert_eq!(
            ledger.balance(&"token:daily-usd:budget:USD".to_string()),
            Decimal::new(10, 0),
            "use-it-or-lose-it leftover must be clawed back before the new ceiling is funded"
        );
    }

    #[test]
    fn test_period_close_allows_post_close_adjustment() {
        let mut ledger = make_ledger();

        let yesterday = Utc::now() - Duration::days(1);
        let close = PeriodClose {
            commodity: "USD".to_string(),
            period_end: yesterday,
            closed_at: Utc::now(),
            closed_by: "admin".to_string(),
        };
        ledger.period_closes.push(close);

        let entry = JournalEntry::post_close_adjustment(
            "USD",
            vec![
                LedgerEntry {
                    account: "agent:x:available:USD".to_string(),
                    amount: ResourceAmount {
                        value: Decimal::new(50, 0),
                        commodity: "USD".to_string(),
                    },
                },
                LedgerEntry {
                    account: "system:allocation:USD".to_string(),
                    amount: ResourceAmount {
                        value: Decimal::new(-50, 0),
                        commodity: "USD".to_string(),
                    },
                },
            ],
            "billing correction",
        );

        let result = ledger.record(entry);
        assert!(result.is_ok());
    }
}
