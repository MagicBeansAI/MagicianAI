//! Unified `channel_observe` config (unified observe+assist U2).
//!
//! ONE durable config both observe pipelines read: the channel-assist
//! message-channel worker (email/whatsapp/kapso/telegram) and the calendar digest.
//! It replaces the two parallel surfaces the feature grew up with — the
//! per-producer `email_observe`/`calendar_observe` consent configs and the
//! `channel_assist` account registry — by folding both into one
//! `{suppress_sensitive, cadence, channels[]}` document.
//!
//! Migration is one-time and lazy: [`load_or_migrate`] reads the unified
//! config, and if it is absent synthesizes it from the three legacy sources
//! and persists it once. After that seed, the unified config is the source
//! of truth; `/observe` and `/channel-assist/channels` write paths both upsert
//! here (see [`upsert_producer_channels`] / [`upsert_message_channels`]) so UI
//! edits reach the workers through one config document.
//!
//! Channel vs provider: the config speaks in user-facing *channels*
//! (`email` | `whatsapp` | `whatsapp_kapso` | `telegram` | `calendar`); the
//! mail worker speaks in *providers* (`gmail` | `whatsapp` |
//! `whatsapp_kapso` | `telegram`). `email` maps to `gmail`; `calendar` is not
//! a message provider (the digest owns it) and is filtered out of the worker's
//! account set.

pub use magician::magician_v2::observe_connectors::{read_channel_observe, write_channel_observe};
use magician::magician_v2::observe_connectors::{
    ChannelEntry, ChannelObserveConfig, ObserveCadence, CALENDAR_CHANNEL,
    DEFAULT_HISTORY_LOOKBACK_DAYS,
};
use serde_yaml;
use std::sync::OnceLock;

use magician::magician_v2::artifact_v2::workspace::{runtime_config_path, ArtifactV2Workspace};
use magician::magician_v2::observe_connectors::{load_producer_observe_config, ObserveConfig};

use super::registry::{read_channel_registry, ChannelAccount, ChannelAccountRegistry};
use super::types::ChannelLane;

/// Durable namespace holding the unified config (sibling of the retired
/// `email_observe` / `channel_assist` namespaces).
/// Artifact name inside the namespace (same idiom as the legacy configs).

/// The `email` channel resolves to the `gmail` provider; the `calendar`
/// channel stays on the digest and is never a message provider.

/// User-facing history window choices for message-channel ingest. Keep this
/// deliberately small so a UI save cannot accidentally trigger a huge backfill.
pub const HISTORY_LOOKBACK_DAY_OPTIONS: [u32; 5] = [1, 5, 7, 14, 30];

pub fn normalize_history_lookback_days(days: u32) -> u32 {
    if HISTORY_LOOKBACK_DAY_OPTIONS.contains(&days) {
        days
    } else {
        DEFAULT_HISTORY_LOOKBACK_DAYS
    }
}

/// Map a user-facing channel to the mail worker's provider. `None` means the
/// channel is not a registered message provider (e.g. `calendar`, which stays
/// on the digest). Sourced from the data-driven [`super::channel_providers`]
/// registry — no hardcoded provider list here.
pub fn channel_to_provider(channel: &str) -> Option<&'static str> {
    super::channel_providers::provider_for_channel(channel)
}

/// Map a mail worker provider back to its user-facing channel name (from the
/// [`super::channel_providers`] registry; `gmail` → `email`).
pub fn provider_to_channel(provider: &str) -> &str {
    super::channel_providers::channel_for_provider(provider)
}

/// Enabled MESSAGE-channel accounts the mail worker ingests — calendar (and
/// any non-message channel) filtered out, projected to the worker's
/// [`ChannelAccount`] shape. This is what `resolve_channel_accounts` used to
/// compute from the registry + observe fallback.
pub fn message_accounts(config: &ChannelObserveConfig) -> Vec<ChannelAccount> {
    config
        .channels
        .iter()
        .filter(|entry| entry.enabled)
        .filter_map(|entry| {
            channel_to_provider(&entry.channel).map(|provider| ChannelAccount {
                provider: provider.to_string(),
                account_alias: entry.account.clone(),
                lane: entry.lane,
                enabled: true,
            })
        })
        .collect()
}

/// Every message-channel account (enabled or not) as a [`ChannelAccount`],
/// preserving the `enabled` flag — for the consent UI, which shows disabled
/// accounts too. [`message_accounts`] (enabled-only, `enabled: true`) is what
/// the worker ingests.
pub fn message_accounts_all(config: &ChannelObserveConfig) -> Vec<ChannelAccount> {
    config
        .channels
        .iter()
        .filter_map(|entry| {
            channel_to_provider(&entry.channel).map(|provider| ChannelAccount {
                provider: provider.to_string(),
                account_alias: entry.account.clone(),
                lane: entry.lane,
                enabled: entry.enabled,
            })
        })
        .collect()
}

/// Synthesize the unified config from the three legacy sources (pure). The
/// registry is authoritative for message channels; the gmail fallback (empty
/// registry → derive email channels from `email_observe`) preserves Phase-1
/// behavior; calendar channels come from `calendar_observe`. Cadence and
/// `suppress_sensitive` are carried from the email producer (the message /
/// digest cadence).
pub fn migrate_config(
    email: &ObserveConfig,
    calendar: &ObserveConfig,
    registry: &ChannelAccountRegistry,
) -> ChannelObserveConfig {
    let mut channels: Vec<ChannelEntry> = registry
        .accounts
        .iter()
        .map(|account| ChannelEntry {
            channel: provider_to_channel(&account.provider).to_string(),
            account: account.account_alias.clone(),
            lane: account.lane,
            enabled: account.enabled,
            purposes: Vec::new(),
        })
        .collect();

    // Gmail fallback: iff the registry lists NO gmail entries, derive email
    // channels from the email_observe consent config (Phase-1-identical:
    // user_assist, enabled tracking the producer toggle).
    let registry_has_gmail = registry
        .accounts
        .iter()
        .any(|account| account.provider == "gmail");
    if !registry_has_gmail {
        for alias in &email.accounts {
            channels.push(ChannelEntry {
                channel: "email".to_string(),
                account: alias.clone(),
                lane: ChannelLane::UserAssist,
                enabled: email.enabled,
                purposes: Vec::new(),
            });
        }
    }

    // Calendar channels from the calendar_observe config (the digest keeps
    // owning them; the unified config just holds them for the worker-blind UI).
    for alias in &calendar.accounts {
        let already = channels
            .iter()
            .any(|entry| entry.channel == CALENDAR_CHANNEL && &entry.account == alias);
        if !already {
            channels.push(ChannelEntry {
                channel: CALENDAR_CHANNEL.to_string(),
                account: alias.clone(),
                lane: account_lane(CALENDAR_CHANNEL, alias),
                enabled: calendar.enabled,
                purposes: Vec::new(),
            });
        }
    }

    ChannelObserveConfig {
        suppress_sensitive: email.suppress_sensitive,
        history_lookback_days: DEFAULT_HISTORY_LOOKBACK_DAYS,
        cadence: ObserveCadence {
            frequency: email.frequency.clone(),
            time: email.time.clone(),
        },
        channels,
    }
}

/// The single entry point both workers use: read the unified config,
/// migrating (and persisting) it once from the legacy sources if absent.
pub async fn load_or_migrate(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> ChannelObserveConfig {
    if let Some(config) = read_channel_observe(workspace_layout, principal, workspace).await {
        return config;
    }
    let email = load_producer_observe_config(workspace_layout, principal, workspace, "email").await;
    let calendar =
        load_producer_observe_config(workspace_layout, principal, workspace, "calendar").await;
    let registry = read_channel_registry(workspace_layout, principal, workspace).await;
    let migrated = migrate_config(&email, &calendar, &registry);
    // Best-effort seed: a failed write just means we migrate again next read.
    let _ = write_channel_observe(workspace_layout, principal, workspace, &migrated).await;
    migrated
}

/// Built-in agent (`envoy` "Presto" lane) accounts — the `(provider, alias)`
/// pairs that are the AGENT's own identities across channels. These match the
/// shipped identities; the `agent_accounts:` block in `operator-config.yaml`
/// ADDS to this set (it never drops a shipped default). `calendar` is the
/// digest channel for Presto's Google calendar.
fn builtin_agent_accounts() -> Vec<(String, String)> {
    [
        ("gmail", "presto"),
        ("calendar", "presto"),
        ("whatsapp_kapso", "presto"),
        ("telegram", "presto"),
        ("agentmail", "work"),
    ]
    .iter()
    .map(|(p, a)| (p.to_string(), a.to_string()))
    .collect()
}

/// Overlay the operator-config `agent_accounts:` list onto the built-in set
/// (pure — the config value is injected so this is unit-testable). Each entry
/// is `{provider, alias}`; unknown/partial entries are skipped.
fn merge_agent_accounts(
    mut base: Vec<(String, String)>,
    cfg: &serde_yaml::Value,
) -> Vec<(String, String)> {
    let Some(seq) = cfg
        .get("agent_accounts")
        .and_then(serde_yaml::Value::as_sequence)
    else {
        return base;
    };
    for entry in seq {
        let entry: &serde_yaml::Value = entry;
        let provider: Option<&str> = entry
            .get("provider")
            .and_then(|p: &serde_yaml::Value| p.as_str());
        let alias: Option<&str> = entry
            .get("alias")
            .and_then(|a: &serde_yaml::Value| a.as_str());
        if let (Some(provider), Some(alias)) = (provider, alias) {
            let pair = (provider.to_string(), alias.to_string());
            if !base.contains(&pair) {
                base.push(pair);
            }
        }
    }
    base
}

fn agent_accounts() -> &'static [(String, String)] {
    static ACCOUNTS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ACCOUNTS.get_or_init(|| {
        let raw = std::fs::read_to_string(runtime_config_path(
            "operator-config.yaml",
            "skillshub/operator-config.yaml",
        ))
        .ok();
        let cfg: Option<serde_yaml::Value> =
            raw.as_deref().and_then(|r| serde_yaml::from_str(r).ok());
        match cfg {
            Some(cfg) => merge_agent_accounts(builtin_agent_accounts(), &cfg),
            None => builtin_agent_accounts(),
        }
    })
}

fn resolve_lane(provider: &str, alias: &str, accounts: &[(String, String)]) -> ChannelLane {
    if accounts.iter().any(|(p, a)| p == provider && a == alias) {
        ChannelLane::Envoy
    } else {
        ChannelLane::UserAssist
    }
}

/// The lane a `(provider, alias)` account belongs to: the `envoy` ("Presto")
/// lane iff it's one of the agent's own accounts (see [`agent_accounts`]),
/// else the owner's `user_assist` ("You") lane. Data-driven — the shipped
/// `presto`/AgentMail-`work` identities are defaults, extendable via
/// `operator-config.yaml` `agent_accounts:` — so there's no hardcoded
/// `== "presto"` scattered across discovery.
pub fn account_lane(provider: &str, alias: &str) -> ChannelLane {
    resolve_lane(provider, alias, agent_accounts())
}

/// Adapter for the `/observe/{producer}` PUT. Post-U4 the message channels
/// (email/whatsapp/kapso/telegram) are owned by the Mail & chat card
/// ([`upsert_message_channels`]), so this handles only NON-message producers —
/// today just `calendar`, which sends its COMPLETE account set (the owner's
/// "You" calendars + Presto's `envoy` calendar). A stray message-producer PUT
/// bails early rather than clobbering the mail card's channels. The calendar
/// card is the digest producer, so its cadence is the unified cadence.
pub async fn upsert_producer_channels(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    producer: &str,
    config: &ObserveConfig,
) -> anyhow::Result<()> {
    if channel_to_provider(producer).is_some() {
        return Ok(());
    }
    let mut unified = load_or_migrate(workspace_layout, principal, workspace).await;
    unified.history_lookback_days = normalize_history_lookback_days(unified.history_lookback_days);
    // Replace all of this (non-message) channel's entries; other channels are
    // untouched. Per-account lane: Presto's calendar is the envoy lane.
    // Purposes the owner granted survive the rewrite: they are a separate
    // grant (`set_channel_purpose`), keyed by the same account.
    let purposes = purposes_by_key(&unified);
    unified.channels.retain(|entry| entry.channel != producer);
    for alias in &config.accounts {
        unified.channels.push(ChannelEntry {
            channel: producer.to_string(),
            account: alias.clone(),
            lane: account_lane(producer, alias),
            enabled: config.enabled,
            purposes: purposes
                .get(&(producer.to_string(), alias.clone()))
                .cloned()
                .unwrap_or_default(),
        });
    }
    unified.cadence = ObserveCadence {
        frequency: config.frequency.clone(),
        time: config.time.clone(),
    };
    write_channel_observe(workspace_layout, principal, workspace, &unified).await
}

/// Adapter for the `/channel-assist/channels` PUT: replace ALL message
/// channels (email/whatsapp/kapso/telegram) with the registry's account set,
/// keeping calendar (and any non-message channel) untouched.
pub async fn upsert_message_channels(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    registry: &ChannelAccountRegistry,
    history_lookback_days: Option<u32>,
) -> anyhow::Result<()> {
    let mut unified = load_or_migrate(workspace_layout, principal, workspace).await;
    if let Some(days) = history_lookback_days {
        unified.history_lookback_days = normalize_history_lookback_days(days);
    } else {
        unified.history_lookback_days =
            normalize_history_lookback_days(unified.history_lookback_days);
    }
    let purposes = purposes_by_key(&unified);
    unified
        .channels
        .retain(|entry| channel_to_provider(&entry.channel).is_none());
    for account in &registry.accounts {
        let channel = provider_to_channel(&account.provider).to_string();
        // Turning observation OFF withdraws every purpose granted beyond it.
        // Carrying them silently meant re-enabling the account later, for an
        // unrelated reason, restored authority to read login codes from that
        // inbox with no second decision — the opposite of the separate,
        // reversible grant the panel promises (secure HITL P6).
        let carried = if account.enabled {
            purposes
                .get(&(channel.clone(), account.account_alias.clone()))
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        unified.channels.push(ChannelEntry {
            purposes: carried,
            channel,
            account: account.account_alias.clone(),
            lane: account.lane,
            enabled: account.enabled,
        });
    }
    write_channel_observe(workspace_layout, principal, workspace, &unified).await
}

fn purposes_by_key(
    config: &ChannelObserveConfig,
) -> std::collections::HashMap<(String, String), Vec<String>> {
    config
        .channels
        .iter()
        .filter(|entry| !entry.purposes.is_empty())
        .map(|entry| {
            (
                (entry.channel.clone(), entry.account.clone()),
                entry.purposes.clone(),
            )
        })
        .collect()
}

/// Grant or withdraw one purpose on one configured account (secure HITL P6:
/// `verification_codes`). Returns `false` when no such account is
/// configured — a purpose is never granted to an account that is not there.
pub async fn set_channel_purpose(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    channel: &str,
    account: &str,
    purpose: &str,
    granted: bool,
) -> anyhow::Result<bool> {
    let mut unified = load_or_migrate(workspace_layout, principal, workspace).await;
    let Some(entry) = unified
        .channels
        .iter_mut()
        .find(|entry| entry.channel == channel && entry.account == account)
    else {
        return Ok(false);
    };
    entry.purposes.retain(|p| p != purpose);
    if granted {
        entry.purposes.push(purpose.to_string());
    }
    write_channel_observe(workspace_layout, principal, workspace, &unified).await?;
    Ok(true)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn observe(enabled: bool, accounts: &[&str]) -> ObserveConfig {
        ObserveConfig {
            enabled,
            accounts: accounts.iter().map(|a| a.to_string()).collect(),
            ..ObserveConfig::default()
        }
    }

    fn reg(accounts: Vec<ChannelAccount>) -> ChannelAccountRegistry {
        ChannelAccountRegistry { accounts }
    }

    fn account(provider: &str, alias: &str, lane: ChannelLane, enabled: bool) -> ChannelAccount {
        ChannelAccount {
            provider: provider.to_string(),
            account_alias: alias.to_string(),
            lane,
            enabled,
        }
    }

    #[test]
    fn migrates_from_email_observe_only_via_gmail_fallback() {
        // No registry gmail entries → derive email channels from email_observe.
        let cfg = migrate_config(
            &observe(true, &["business", "personal"]),
            &observe(false, &[]),
            &reg(vec![]),
        );
        assert_eq!(
            cfg.channels,
            vec![
                ChannelEntry {
                    channel: "email".into(),
                    account: "business".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: "email".into(),
                    account: "personal".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn migrates_from_registry_only_mapping_provider_to_channel() {
        // Registry has gmail → the fallback is suppressed; providers map to
        // channels (gmail→email; whatsapp_kapso stays).
        let cfg = migrate_config(
            &observe(true, &["ignored-because-registry-has-gmail"]),
            &observe(false, &[]),
            &reg(vec![
                account("gmail", "presto", ChannelLane::Envoy, true),
                account("whatsapp_kapso", "presto", ChannelLane::Envoy, false),
            ]),
        );
        assert_eq!(
            cfg.channels,
            vec![
                ChannelEntry {
                    channel: "email".into(),
                    account: "presto".into(),
                    lane: ChannelLane::Envoy,
                    enabled: true,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: "whatsapp_kapso".into(),
                    account: "presto".into(),
                    lane: ChannelLane::Envoy,
                    enabled: false,
                    purposes: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn migrates_from_both_sources_plus_calendar() {
        let cfg = migrate_config(
            &observe(true, &["ignored"]),
            &observe(true, &["business"]),
            &reg(vec![
                account("gmail", "business", ChannelLane::UserAssist, true),
                account("whatsapp", "self", ChannelLane::UserAssist, true),
            ]),
        );
        // registry gmail + whatsapp, then calendar appended from calendar_observe.
        assert_eq!(
            cfg.channels,
            vec![
                ChannelEntry {
                    channel: "email".into(),
                    account: "business".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: "whatsapp".into(),
                    account: "self".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: CALENDAR_CHANNEL.into(),
                    account: "business".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn message_accounts_excludes_calendar_and_disabled() {
        let cfg = ChannelObserveConfig {
            channels: vec![
                ChannelEntry {
                    channel: "email".into(),
                    account: "business".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: "whatsapp".into(),
                    account: "muted".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: false,
                    purposes: Vec::new(),
                },
                ChannelEntry {
                    channel: CALENDAR_CHANNEL.into(),
                    account: "business".into(),
                    lane: ChannelLane::UserAssist,
                    enabled: true,
                    purposes: Vec::new(),
                },
            ],
            ..ChannelObserveConfig::default()
        };
        let accounts = message_accounts(&cfg);
        // Only the enabled, message-channel account survives — as a gmail provider.
        assert_eq!(
            accounts,
            vec![ChannelAccount {
                provider: "gmail".into(),
                account_alias: "business".into(),
                lane: ChannelLane::UserAssist,
                enabled: true,
            }]
        );
    }

    #[test]
    fn builtin_agent_accounts_cover_every_shipped_envoy_identity() {
        let a = builtin_agent_accounts();
        let has = |p: &str, al: &str| a.iter().any(|(x, y)| x == p && y == al);
        // Presto's identities across channels — incl. AgentMail's `work` alias
        // (the old alias == "presto" rule missed this one).
        assert!(has("gmail", "presto"));
        assert!(has("calendar", "presto"));
        assert!(has("whatsapp_kapso", "presto"));
        assert!(has("telegram", "presto"));
        assert!(has("agentmail", "work"));
    }

    #[test]
    fn resolve_lane_is_envoy_only_for_agent_accounts() {
        let accts = builtin_agent_accounts();
        assert_eq!(resolve_lane("gmail", "presto", &accts), ChannelLane::Envoy);
        assert_eq!(
            resolve_lane("agentmail", "work", &accts),
            ChannelLane::Envoy
        );
        assert_eq!(
            resolve_lane("calendar", "presto", &accts),
            ChannelLane::Envoy
        );
        // Owner accounts + the SAME alias on a non-agent provider → You.
        assert_eq!(
            resolve_lane("gmail", "business", &accts),
            ChannelLane::UserAssist
        );
        assert_eq!(
            resolve_lane("calendar", "business", &accts),
            ChannelLane::UserAssist
        );
        // `work` is only the agent on agentmail, not on gmail.
        assert_eq!(
            resolve_lane("gmail", "work", &accts),
            ChannelLane::UserAssist
        );
    }

    #[test]
    fn config_agent_accounts_extend_the_defaults() {
        let cfg: serde_yaml::Value =
            serde_yaml::from_str("agent_accounts:\n  - { provider: slack, alias: presto-bot }\n")
                .unwrap();
        let merged = merge_agent_accounts(builtin_agent_accounts(), &cfg);
        assert_eq!(
            resolve_lane("slack", "presto-bot", &merged),
            ChannelLane::Envoy
        );
        // Shipped defaults are never dropped by a partial config list.
        assert_eq!(resolve_lane("gmail", "presto", &merged), ChannelLane::Envoy);
    }

    #[test]
    fn migrated_presto_calendar_carries_the_envoy_lane() {
        let cfg = migrate_config(
            &observe(false, &[]),
            &observe(true, &["business", "presto"]),
            &reg(vec![]),
        );
        let presto_cal = cfg
            .channels
            .iter()
            .find(|c| c.channel == CALENDAR_CHANNEL && c.account == "presto")
            .expect("presto calendar channel present");
        assert_eq!(presto_cal.lane, ChannelLane::Envoy);
        let business_cal = cfg
            .channels
            .iter()
            .find(|c| c.channel == CALENDAR_CHANNEL && c.account == "business")
            .expect("business calendar channel present");
        assert_eq!(business_cal.lane, ChannelLane::UserAssist);
    }

    #[test]
    fn suppress_and_cadence_carry_from_email_producer() {
        let email = ObserveConfig {
            enabled: true,
            accounts: vec!["business".into()],
            frequency: "twice-daily".into(),
            time: "06:30".into(),
            suppress_sensitive: false,
            ..ObserveConfig::default()
        };
        let cfg = migrate_config(&email, &observe(false, &[]), &reg(vec![]));
        assert!(!cfg.suppress_sensitive);
        assert_eq!(cfg.cadence.frequency, "twice-daily");
        assert_eq!(cfg.cadence.time, "06:30");
    }
}
