//! Channel-account registry (Phase 1b design, "Account registry").
//!
//! One durable-config namespace (`channel_assist`) lists every account the
//! channel-assist sync worker ingests: `{provider, account_alias, lane,
//! enabled}`. WhatsApp/Kapso entries reference their credential sources by
//! alias (wu.db path via scope workdirs; Kapso key via operator config) —
//! the registry never duplicates secrets.
//!
//! Gmail back-compat: when the registry carries NO gmail entries at all,
//! the sync worker derives gmail accounts from the `email_observe`
//! consent config exactly as Phase 1 did (lane `user_assist`, gated on
//! the observe producer being enabled). The moment ANY gmail entry exists
//! in the registry — enabled or not — the registry list REPLACES the
//! fallback entirely: a disabled gmail entry is an explicit opt-out, not
//! a fall-through, and registry gmail accounts (e.g. `presto`) are
//! governed by their own `enabled` flag rather than the observe UI.

use serde::{Deserialize, Serialize};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableFrontmatter,
};
use magician::magician_v2::observe_connectors::ObserveConfig;

use super::types::ChannelLane;

/// Durable namespace holding the registry (sibling of `email_observe`).
pub const CHANNEL_ASSIST_NAMESPACE: &str = "channel_assist";
/// Artifact name inside the namespace (same idiom as the observe config).
pub const REGISTRY_CONFIG_NAME: &str = "config.json";

fn default_true() -> bool {
    true
}

/// One ingestable channel account. `provider` selects the ingestor
/// (`gmail` | `whatsapp` | `whatsapp_kapso`); `account_alias` is the
/// provider-local credential alias (gws profile, wu.db owner, Kapso
/// number); `lane` tags every row the account produces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelAccount {
    pub provider: String,
    pub account_alias: String,
    #[serde(default)]
    pub lane: ChannelLane,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// The persisted registry document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelAccountRegistry {
    #[serde(default)]
    pub accounts: Vec<ChannelAccount>,
}

/// Read the scope's registry. Missing or unreadable config is an EMPTY
/// registry — never an error, never a write (same posture as
/// `load_email_observe_config`).
pub async fn read_channel_registry(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> ChannelAccountRegistry {
    let store = match open_local_durable_artifacts(workspace_layout, principal, workspace) {
        Ok(store) => store,
        Err(_) => return ChannelAccountRegistry::default(),
    };
    match store
        .read(CHANNEL_ASSIST_NAMESPACE, REGISTRY_CONFIG_NAME)
        .await
    {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => ChannelAccountRegistry::default(),
    }
}

/// Persist the registry for a scope (owner-side configuration writes; the
/// sync worker itself only reads).
pub async fn write_channel_registry(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    registry: &ChannelAccountRegistry,
) -> anyhow::Result<()> {
    let store = open_local_durable_artifacts(workspace_layout, principal, workspace)?;
    let body = serde_json::to_string_pretty(registry)?;
    let frontmatter = DurableFrontmatter {
        namespace: CHANNEL_ASSIST_NAMESPACE.to_string(),
        name: REGISTRY_CONFIG_NAME.to_string(),
        created_by: "channel_assist".to_string(),
        last_updated_by: "channel_assist".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(principal.to_string()),
        producer_stage: Some("channel_registry".to_string()),
    };
    store
        .write(
            CHANNEL_ASSIST_NAMESPACE,
            REGISTRY_CONFIG_NAME,
            &body,
            frontmatter,
        )
        .await?;
    Ok(())
}

/// Resolve the accounts one sync pass should ingest (pure — the policy
/// seam the module header describes):
///
/// - enabled registry entries pass through for every provider;
/// - gmail fallback: iff the registry has NO gmail entries AND the
///   `email_observe` producer is enabled, each observe account becomes a
///   `gmail`/`user_assist` entry (Phase-1-identical behavior).
pub fn resolve_channel_accounts(
    registry: &ChannelAccountRegistry,
    observe: &ObserveConfig,
) -> Vec<ChannelAccount> {
    let registry_has_gmail = registry
        .accounts
        .iter()
        .any(|account| account.provider == "gmail");
    let mut accounts: Vec<ChannelAccount> = registry
        .accounts
        .iter()
        .filter(|account| account.enabled)
        .cloned()
        .collect();
    if !registry_has_gmail && observe.enabled {
        accounts.extend(observe.accounts.iter().map(|alias| ChannelAccount {
            provider: "gmail".to_string(),
            account_alias: alias.clone(),
            lane: ChannelLane::UserAssist,
            enabled: true,
        }));
    }
    accounts
}

/// The message-channel accounts one sync pass should ingest — what the
/// worker loop, `sync/run`, and the `/channel-assist/channels` resolved-set
/// read. Unified observe+assist U2: this now reads the single
/// `channel_observe` config (migrated once from this registry +
/// `email_observe`), not the registry directly, so email/whatsapp/kapso/telegram all
/// flow from one source. [`resolve_channel_accounts`] remains the pure
/// registry→accounts projection the migration builds on.
pub async fn load_channel_accounts(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Vec<ChannelAccount> {
    let config =
        super::channel_observe::load_or_migrate(workspace_layout, principal, workspace).await;
    super::channel_observe::message_accounts(&config)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn account(provider: &str, alias: &str, lane: ChannelLane, enabled: bool) -> ChannelAccount {
        ChannelAccount {
            provider: provider.to_string(),
            account_alias: alias.to_string(),
            lane,
            enabled,
        }
    }

    fn observe(enabled: bool, accounts: &[&str]) -> ObserveConfig {
        ObserveConfig {
            enabled,
            accounts: accounts.iter().map(|a| a.to_string()).collect(),
            ..ObserveConfig::default()
        }
    }

    #[test]
    fn empty_registry_derives_gmail_accounts_from_enabled_observe_config() {
        let registry = ChannelAccountRegistry::default();
        let resolved = resolve_channel_accounts(&registry, &observe(true, &["acct-a", "acct-b"]));
        assert_eq!(
            resolved,
            vec![
                account("gmail", "acct-a", ChannelLane::UserAssist, true),
                account("gmail", "acct-b", ChannelLane::UserAssist, true),
            ]
        );
        // Disabled observe producer → no fallback (Phase-1-identical gate).
        assert!(resolve_channel_accounts(&registry, &observe(false, &["acct-a"])).is_empty());
    }

    #[test]
    fn any_gmail_registry_entry_replaces_the_observe_fallback_entirely() {
        let registry = ChannelAccountRegistry {
            accounts: vec![account("gmail", "presto", ChannelLane::Envoy, true)],
        };
        let resolved = resolve_channel_accounts(&registry, &observe(true, &["acct-a", "acct-b"]));
        // The observe accounts do NOT appear — the registry list won.
        assert_eq!(
            resolved,
            vec![account("gmail", "presto", ChannelLane::Envoy, true)]
        );

        // Even a registry whose only gmail entries are DISABLED suppresses
        // the fallback: an explicit opt-out, not a fall-through.
        let disabled_only = ChannelAccountRegistry {
            accounts: vec![account("gmail", "acct-a", ChannelLane::UserAssist, false)],
        };
        assert!(resolve_channel_accounts(&disabled_only, &observe(true, &["acct-a"])).is_empty());
    }

    #[test]
    fn non_gmail_entries_pass_through_and_do_not_suppress_the_gmail_fallback() {
        let registry = ChannelAccountRegistry {
            accounts: vec![
                account("whatsapp", "self", ChannelLane::UserAssist, true),
                account("whatsapp_kapso", "presto", ChannelLane::Envoy, true),
                account("whatsapp", "disabled-alias", ChannelLane::UserAssist, false),
            ],
        };
        let resolved = resolve_channel_accounts(&registry, &observe(true, &["acct-a"]));
        assert_eq!(
            resolved,
            vec![
                account("whatsapp", "self", ChannelLane::UserAssist, true),
                account("whatsapp_kapso", "presto", ChannelLane::Envoy, true),
                account("gmail", "acct-a", ChannelLane::UserAssist, true),
            ]
        );
    }

    #[test]
    fn registry_entries_default_lane_and_enabled_when_omitted() {
        let parsed: ChannelAccountRegistry =
            serde_json::from_str(r#"{"accounts":[{"provider":"gmail","account_alias":"acct-a"}]}"#)
                .unwrap();
        assert_eq!(parsed.accounts[0].lane, ChannelLane::UserAssist);
        assert!(parsed.accounts[0].enabled);
        // Unreadable/empty documents parse as the empty registry.
        assert_eq!(
            serde_json::from_str::<ChannelAccountRegistry>("{}").unwrap(),
            ChannelAccountRegistry::default()
        );
    }

    #[tokio::test]
    async fn registry_roundtrips_through_the_durable_namespace() {
        let tmp = TempDir::new().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());

        // Missing config reads as empty, never errors.
        let empty = read_channel_registry(&layout, "alpha", "prod").await;
        assert_eq!(empty, ChannelAccountRegistry::default());

        let registry = ChannelAccountRegistry {
            accounts: vec![
                account("gmail", "presto", ChannelLane::Envoy, true),
                account("whatsapp", "self", ChannelLane::UserAssist, true),
            ],
        };
        write_channel_registry(&layout, "alpha", "prod", &registry)
            .await
            .unwrap();
        let fetched = read_channel_registry(&layout, "alpha", "prod").await;
        assert_eq!(fetched, registry);
        // Scope isolation: another scope still reads empty.
        let other = read_channel_registry(&layout, "beta", "prod").await;
        assert_eq!(other, ChannelAccountRegistry::default());
    }
}
