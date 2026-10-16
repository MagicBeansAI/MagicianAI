//! Declarative budget config — Layer 2 of the resource-authority design.
//!
//! Source-of-truth shape is YAML in `magician-config.yaml`; this
//! module owns the structs that section deserializes into plus the
//! validation pass that runs at orchestrator boot.
//!
//! Off by default. Existing deployments see no behavior change until
//! an operator flips `resource_authority.enabled: true` and adds
//! `budgets:` rows. See `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::token::{CarryoverPolicy, CeilingPeriod, SystemCeiling};
use super::types::{canonicalize_commodity, WILDCARD_OWNER_ID};

/// Top-level config block (`resource_authority:` in YAML).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceAuthorityConfig {
    /// Master switch. `false` (default) ⇒ providers continue to
    /// return `Bare` even when a YAML pack carries a `spend:` block
    /// (the gate stays dormant). Flip to `true` to activate
    /// enforcement; once on, callers MUST route gated actions through
    /// a gating-aware dispatcher or the call rejects.
    #[serde(default)]
    pub enabled: bool,

    /// System-wide ceilings — passed verbatim into the `SystemCeiling`
    /// stack that `spend_session::admit` reads at reserve time.
    /// Multiple ceilings stack; ALL are checked per call.
    #[serde(default)]
    pub system_ceilings: Vec<SystemCeiling>,

    /// Budget rows — each declares a ceiling at a specific scope
    /// (principal / agent / tool) for one commodity. The resolver
    /// (`spend_token_resolver.rs`) walks these at dispatch time and
    /// returns the token ids that apply to the call's (principal,
    /// agent_id, tool_name, commodity) tuple.
    #[serde(default)]
    pub budgets: Vec<BudgetRow>,
}

/// One declarative budget row. Lazily materializes into a `SpendToken`
/// on first lookup by the resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetRow {
    /// Which axis this row applies to — see `BudgetScope` below.
    pub scope: BudgetScope,
    /// Scope-qualified id (principal id, agent_id, or tool name).
    /// Use `*` to match any id at this scope; the resulting token is a
    /// shared pool (`issued_to` = `{scope}:*`), not a per-identity copy.
    pub id: String,
    /// Commodity this budget governs (e.g. `USD`, `EMAIL_SENDS`).
    /// Compared case-insensitively after trim (`usd` == `USD`).
    pub commodity: String,
    /// Ceiling for the period — same semantics as `SpendToken.ceiling`.
    pub ceiling: Decimal,
    /// Window the ceiling resets on (or `Total` for one-shot budgets).
    pub period: CeilingPeriod,
    /// Carryover policy at period rollover.
    #[serde(default = "default_carryover")]
    pub carryover: CarryoverPolicy,
    /// Optional system-ceiling id linking this budget to a stacked
    /// system-wide cap. Defaults to `None` (token stands alone).
    #[serde(default)]
    pub system_ceiling_id: Option<String>,
}

fn default_carryover() -> CarryoverPolicy {
    CarryoverPolicy::None
}

/// Budget scope axis. The resolver matches (principal, agent_id,
/// tool_name) against `id` per-scope: `principal:owner` matches a
/// principal-row with `id: owner`, etc. Tokens at different scopes
/// for the same commodity stack — the gate enforces ALL of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    Principal,
    Agent,
    Tool,
}

impl BudgetScope {
    /// Owner-key prefix for the budget account / token id namespace.
    /// Mirrors the convention used by autonomous agent-issued tokens
    /// (`agent:cfo:available:USD`, etc.) so journal entries are
    /// readable across scopes.
    pub fn owner_prefix(&self) -> &'static str {
        match self {
            BudgetScope::Principal => "principal",
            BudgetScope::Agent => "agent",
            BudgetScope::Tool => "tool",
        }
    }
}

impl ResourceAuthorityConfig {
    /// Static validation pass. Catches:
    /// - Duplicate (scope, id, commodity) tuples — ambiguous which row wins.
    /// - `system_ceiling_id` references that don't match any
    ///   declared `system_ceilings[i].id`.
    /// - Empty `id` / `commodity` strings.
    pub fn validate(&self) -> Result<(), ResourceAuthorityConfigError> {
        let mut seen: std::collections::HashSet<(BudgetScope, String, String)> =
            std::collections::HashSet::new();
        let known_ceiling_ids: std::collections::HashSet<&str> =
            self.system_ceilings.iter().map(|c| c.id.as_str()).collect();

        for row in &self.budgets {
            if row.id.trim().is_empty() {
                return Err(ResourceAuthorityConfigError::EmptyField(
                    "budget.id".to_string(),
                ));
            }
            if canonicalize_commodity(&row.commodity).is_empty() {
                return Err(ResourceAuthorityConfigError::EmptyField(
                    "budget.commodity".to_string(),
                ));
            }
            let commodity = canonicalize_commodity(&row.commodity);
            let id = row.id.trim();
            let key = (row.scope, id.to_string(), commodity.clone());
            if !seen.insert(key) {
                return Err(ResourceAuthorityConfigError::DuplicateBudgetRow {
                    scope: row.scope,
                    id: id.to_string(),
                    commodity,
                });
            }
            if let Some(ceiling_id) = row.system_ceiling_id.as_deref() {
                if !known_ceiling_ids.contains(ceiling_id) {
                    return Err(ResourceAuthorityConfigError::UnknownSystemCeiling {
                        referenced: ceiling_id.to_string(),
                        from_row_id: row.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Find the budget row matching (scope, id, commodity).
    ///
    /// Exact `id` wins. If none exists, a row whose `id` is `*` at the same
    /// scope + commodity is used. Commodity comparison is case-insensitive.
    /// Returns `None` when no row is declared — the resolver then contributes
    /// no token at that scope.
    pub fn find_row(&self, scope: BudgetScope, id: &str, commodity: &str) -> Option<&BudgetRow> {
        let commodity = canonicalize_commodity(commodity);
        let id = id.trim();
        let exact = self.budgets.iter().find(|row| {
            row.scope == scope
                && row.id.trim() == id
                && canonicalize_commodity(&row.commodity) == commodity
        });
        if exact.is_some() {
            return exact;
        }
        self.budgets.iter().find(|row| {
            row.scope == scope
                && row.id.trim() == WILDCARD_OWNER_ID
                && canonicalize_commodity(&row.commodity) == commodity
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceAuthorityConfigError {
    #[error("budget row has empty `{0}` field")]
    EmptyField(String),
    #[error(
        "duplicate budget row for ({scope:?}, `{id}`, `{commodity}`) — \
         each (scope, id, commodity) tuple may appear only once"
    )]
    DuplicateBudgetRow {
        scope: BudgetScope,
        id: String,
        commodity: String,
    },
    #[error(
        "budget row `{from_row_id}` references unknown system ceiling \
         `{referenced}` — add it to `resource_authority.system_ceilings`"
    )]
    UnknownSystemCeiling {
        referenced: String,
        from_row_id: String,
    },
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn principal_row(id: &str) -> BudgetRow {
        BudgetRow {
            scope: BudgetScope::Principal,
            id: id.to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::from(100),
            period: CeilingPeriod::Monthly,
            carryover: CarryoverPolicy::None,
            system_ceiling_id: None,
        }
    }

    #[test]
    fn default_config_is_disabled_and_empty() {
        let cfg = ResourceAuthorityConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.system_ceilings.is_empty());
        assert!(cfg.budgets.is_empty());
        cfg.validate().unwrap();
    }

    #[test]
    fn duplicate_budget_row_fails_validation() {
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_row("owner"), principal_row("owner")],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            ResourceAuthorityConfigError::DuplicateBudgetRow { .. }
        ));
    }

    #[test]
    fn unknown_system_ceiling_reference_fails() {
        let mut row = principal_row("owner");
        row.system_ceiling_id = Some("does-not-exist".to_string());
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![row],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            ResourceAuthorityConfigError::UnknownSystemCeiling { .. }
        ));
    }

    #[test]
    fn empty_id_fails() {
        let mut row = principal_row("");
        row.id = "".to_string();
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![row],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ResourceAuthorityConfigError::EmptyField(_)));
    }

    #[test]
    fn find_row_returns_match() {
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_row("owner")],
        };
        let row = cfg
            .find_row(BudgetScope::Principal, "owner", "USD")
            .expect("row");
        assert_eq!(row.ceiling, Decimal::from(100));
        assert!(cfg
            .find_row(BudgetScope::Principal, "other", "USD")
            .is_none());
        assert!(cfg.find_row(BudgetScope::Agent, "owner", "USD").is_none());
    }

    #[test]
    fn find_row_matches_commodity_case_insensitively() {
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_row("owner")],
        };
        assert!(cfg
            .find_row(BudgetScope::Principal, "owner", "usd")
            .is_some());
        assert!(cfg
            .find_row(BudgetScope::Principal, "owner", " Usd ")
            .is_some());
    }

    #[test]
    fn usd_and_usd_are_duplicate_rows() {
        let mut lower = principal_row("owner");
        lower.commodity = "usd".to_string();
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![principal_row("owner"), lower],
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            ResourceAuthorityConfigError::DuplicateBudgetRow { .. }
        ));
    }

    #[test]
    fn wildcard_row_matches_any_id_at_that_scope() {
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![BudgetRow {
                scope: BudgetScope::Agent,
                id: "*".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::from(10),
                period: CeilingPeriod::Daily,
                carryover: CarryoverPolicy::None,
                system_ceiling_id: None,
            }],
        };
        let row = cfg
            .find_row(BudgetScope::Agent, "presto", "usd")
            .expect("wildcard");
        assert_eq!(row.id, "*");
        assert!(cfg
            .find_row(BudgetScope::Principal, "presto", "USD")
            .is_none());
    }

    #[test]
    fn exact_id_wins_over_wildcard() {
        let cfg = ResourceAuthorityConfig {
            enabled: true,
            system_ceilings: vec![],
            budgets: vec![
                BudgetRow {
                    scope: BudgetScope::Agent,
                    id: "*".to_string(),
                    commodity: "USD".to_string(),
                    ceiling: Decimal::from(10),
                    period: CeilingPeriod::Daily,
                    carryover: CarryoverPolicy::None,
                    system_ceiling_id: None,
                },
                BudgetRow {
                    scope: BudgetScope::Agent,
                    id: "presto".to_string(),
                    commodity: "USD".to_string(),
                    ceiling: Decimal::from(3),
                    period: CeilingPeriod::Daily,
                    carryover: CarryoverPolicy::None,
                    system_ceiling_id: None,
                },
            ],
        };
        let row = cfg
            .find_row(BudgetScope::Agent, "presto", "USD")
            .expect("exact");
        assert_eq!(row.id, "presto");
        assert_eq!(row.ceiling, Decimal::from(3));
        let other = cfg
            .find_row(BudgetScope::Agent, "bob", "USD")
            .expect("wildcard");
        assert_eq!(other.id, "*");
    }
}
