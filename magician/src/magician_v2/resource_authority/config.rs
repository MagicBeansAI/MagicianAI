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

    /// Model spend limits: what every model call may cost per window, in
    /// total, and per agent run (`docs/plans/2026-10-26-model-spend-in-resource-authority.md`).
    /// The spend envelope enforces them in memory and posts what it counted
    /// to the ledger; `None` is a config written before the move, which takes
    /// the legacy `execution.budgets` values ([`Self::model_spend_or_legacy`]).
    /// A typed section, not `budgets` rows, so an older binary ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_spend: Option<ModelSpendConfig>,
}

/// `resource_authority.model_spend`. Every amount is USD: a positive number,
/// or `unlimited`.
///
/// Unknown keys are ignored, like the rest of `resource_authority`, so a
/// config a later build adds a key to still loads in this one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelSpendConfig {
    /// All model calls in one window (default $250 a day).
    #[serde(default = "default_window_usd")]
    pub window_usd: crate::config::BudgetCapUsd,
    #[serde(default)]
    pub window: crate::config::SpendEnvelopeWindow,
    /// All model calls, ever (default unlimited).
    #[serde(default = "default_unlimited")]
    pub lifetime_usd: crate::config::BudgetCapUsd,
    /// One agent run on Magician's own loop (default $100).
    #[serde(default = "default_run_magician_usd")]
    pub run_magician_usd: crate::config::BudgetCapUsd,
    /// One agent run on a coding harness (default $100).
    #[serde(default = "default_run_harness_usd")]
    pub run_harness_usd: crate::config::BudgetCapUsd,
}

fn default_window_usd() -> crate::config::BudgetCapUsd {
    crate::config::BudgetCapUsd::Usd(crate::config::DEFAULT_SPEND_ENVELOPE_DAILY_USD)
}

fn default_unlimited() -> crate::config::BudgetCapUsd {
    crate::config::BudgetCapUsd::Unlimited
}

fn default_run_magician_usd() -> crate::config::BudgetCapUsd {
    crate::config::ExecutionBudgetSettings::default().native_max_cost_usd
}

fn default_run_harness_usd() -> crate::config::BudgetCapUsd {
    crate::config::ExecutionBudgetSettings::default().harness_max_cost_usd
}

impl Default for ModelSpendConfig {
    fn default() -> Self {
        Self::from_legacy(&crate::config::ExecutionBudgetSettings::default())
    }
}

impl ModelSpendConfig {
    /// The limits a config written before the move expressed in
    /// `execution.budgets`.
    pub fn from_legacy(legacy: &crate::config::ExecutionBudgetSettings) -> Self {
        Self {
            window_usd: legacy.spend_envelope.daily_usd,
            window: legacy.spend_envelope.window,
            lifetime_usd: legacy.spend_envelope.total_usd,
            run_magician_usd: legacy.native_max_cost_usd,
            run_harness_usd: legacy.harness_max_cost_usd,
        }
    }

    /// `legacy` with these limits in place: the cell the per-run valves and
    /// the spend envelope read (`crate::config::configure_execution_budgets`).
    /// Envelope behaviour (`enabled`, `reset_offset_minutes`, `warn_at_pct`,
    /// `unpriced`) stays in `execution.budgets.spend_envelope`.
    pub fn apply_to(
        &self,
        legacy: &crate::config::ExecutionBudgetSettings,
    ) -> crate::config::ExecutionBudgetSettings {
        let mut effective = *legacy;
        effective.native_max_cost_usd = self.run_magician_usd;
        effective.harness_max_cost_usd = self.run_harness_usd;
        effective.spend_envelope.daily_usd = self.window_usd;
        effective.spend_envelope.window = self.window;
        effective.spend_envelope.total_usd = self.lifetime_usd;
        effective
    }
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

/// Move the model spend limits of a config written before the move into
/// `resource_authority.model_spend`, once, at a serving boot: the section is
/// written from the legacy `execution.budgets` values and those limit keys
/// are removed (envelope behaviour keys stay). A config that already has the
/// section, or no file, is left alone. The effective limits are checked to be
/// identical before anything is written. Returns whether it migrated.
pub fn migrate_model_spend_limits(path: &std::path::Path) -> anyhow::Result<bool> {
    use crate::magician_v2::runtime_settings::{
        apply_yaml_section, config_file_lock, set_yaml_path, write_text_file_atomic,
    };
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let document: serde_yaml::Value = serde_yaml::from_str(&original)?;
    let has_section = document
        .get("resource_authority")
        .and_then(|section| section.get("model_spend"))
        .is_some_and(|section| !section.is_null());
    if has_section {
        return Ok(false);
    }
    let legacy_in =
        |document: &serde_yaml::Value| -> anyhow::Result<crate::config::ExecutionBudgetSettings> {
            Ok(
                match document
                    .get("execution")
                    .and_then(|execution| execution.get("budgets"))
                {
                    Some(budgets) if !budgets.is_null() => serde_yaml::from_value(budgets.clone())?,
                    _ => crate::config::ExecutionBudgetSettings::default(),
                },
            )
        };
    let legacy = legacy_in(&document)?;
    let model_spend = ModelSpendConfig::from_legacy(&legacy);
    let mut next = apply_yaml_section(
        &original,
        &["resource_authority", "model_spend"],
        &None::<ModelSpendConfig>,
        &Some(model_spend),
    )?;
    for key in [
        &["execution", "budgets", "native_max_cost_usd"][..],
        &["execution", "budgets", "harness_max_cost_usd"],
        &["execution", "budgets", "spend_envelope", "daily_usd"],
        &["execution", "budgets", "spend_envelope", "total_usd"],
        &["execution", "budgets", "spend_envelope", "window"],
    ] {
        let present = key
            .iter()
            .try_fold(&document, |value, segment| value.get(*segment))
            .is_some();
        if present {
            next = set_yaml_path(&next, key, None)?;
        }
    }
    let migrated: serde_yaml::Value = serde_yaml::from_str(&next)?;
    let section: ModelSpendConfig =
        serde_yaml::from_value(migrated["resource_authority"]["model_spend"].clone())?;
    anyhow::ensure!(
        section == model_spend
            && section.apply_to(&legacy_in(&migrated)?) == model_spend.apply_to(&legacy),
        "model spend limits did not move intact"
    );
    write_text_file_atomic(path, &next, None)?;
    Ok(true)
}

impl ResourceAuthorityConfig {
    /// The model spend limits in force: this section, or the legacy
    /// `execution.budgets` values a config written before the move holds.
    pub fn model_spend_or_legacy(
        &self,
        legacy: &crate::config::ExecutionBudgetSettings,
    ) -> ModelSpendConfig {
        self.model_spend
            .unwrap_or_else(|| ModelSpendConfig::from_legacy(legacy))
    }

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

#[cfg(test)]
mod model_spend_tests {
    use super::*;
    use crate::config::BudgetCapUsd;

    #[test]
    fn legacy_limits_move_into_resource_authority_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        let original = "execution:\n  # valves\n  budgets:\n    native_max_cost_usd: 15\n    harness_max_cost_usd: unlimited\n    spend_envelope:\n      daily_usd: 40\n      window: weekly\n      warn_at_pct: 70\nresource_authority:\n  enabled: true\n  budgets: []\n";
        std::fs::write(&path, original).unwrap();
        assert!(migrate_model_spend_limits(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        let section: ModelSpendConfig =
            serde_yaml::from_value(parsed["resource_authority"]["model_spend"].clone()).unwrap();
        assert_eq!(section.window_usd, BudgetCapUsd::Usd(40.0));
        assert_eq!(section.window, crate::config::SpendEnvelopeWindow::Weekly);
        assert_eq!(section.run_magician_usd, BudgetCapUsd::Usd(15.0));
        assert_eq!(section.run_harness_usd, BudgetCapUsd::Unlimited);
        let budgets = &parsed["execution"]["budgets"];
        assert!(budgets.get("native_max_cost_usd").is_none(), "{text}");
        assert!(
            budgets["spend_envelope"].get("daily_usd").is_none(),
            "{text}"
        );
        assert_eq!(
            budgets["spend_envelope"]["warn_at_pct"],
            serde_yaml::Value::from(70)
        );
        assert!(text.contains("  # valves\n"), "{text}");
        assert_eq!(parsed["resource_authority"]["enabled"], true);
        // Once moved, nothing more happens.
        assert!(!migrate_model_spend_limits(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_config_without_budgets_gets_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        std::fs::write(&path, "chat:\n  x: 1\n").unwrap();
        assert!(migrate_model_spend_limits(&path).unwrap());
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let section: ModelSpendConfig =
            serde_yaml::from_value(parsed["resource_authority"]["model_spend"].clone()).unwrap();
        assert_eq!(section, ModelSpendConfig::default());
        assert_eq!(section.window_usd, BudgetCapUsd::Usd(250.0));
        assert!(!migrate_model_spend_limits(&dir.path().join("missing.yaml")).unwrap());
    }
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
            model_spend: None,
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
