//! Channel metadata sync worker (Mail Assist Phase 1 M3; registry-driven
//! since Phase 1b N1).
//!
//! A `MemoryEvalRunner`-style tokio interval loop. Each tick, for the
//! DEFAULT scope: resolve the channel-account list from `channel_observe`,
//! then run each account through its provider's [`ChannelIngestor`]
//! discovered from the adapter registry and persist the returned batch.
//! Provider-specific pull mechanics live under `channel_assist::adapters`.
//!
//! Sensitive suppression (config `suppress_sensitive`): conservative
//! OTP/verification-code/2FA/banking heuristics on subject + sender mark
//! rows `sensitive_suppressed` and redact the subject at ingest (ids and
//! sender retained so reconciliation still works — M1 contract).
//!
//! Failure posture: per-account isolation — one account failing records a
//! `last_error` on its watermark row (cursors preserved) plus one
//! `gws auth status` probe for a better message (gmail only), and never
//! stops the other accounts or the loop. The watermark NEVER advances on
//! failure: ingestors only RETURN batches; persistence + watermark
//! advancement happen here, after a successful pull. Kill-switch:
//! `CHANNEL_SYNC_ENABLED=0` skips the spawn; the loop additionally no-ops
//! while no channel accounts resolve (observe config disabled and
//! registry empty).
//!
//! Structure: provider ingestors from [`super::adapter_registry`] + the
//! generic worker loop persisting through [`MailAssistStore`].

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use magician::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use magician::magician_v2::artifacts::durable_store::open_local_durable_artifacts;
use magician::magician_v2::observe_catchup::{
    CatchUpAdmission, CatchUpDecision, CatchUpReplayMode, ObserveCatchUpController,
};
use magician::magician_v2::observe_connectors::ObserveConfig;

use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::registry::ChannelAccount;
use super::store::MailAssistStore;
use super::types::{SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION};

use super::adapter_registry::GMAIL_PROVIDER;
use super::adapters::gmail::auth_status_probe;
#[allow(unused_imports)]
pub use super::sensitivity::is_sensitive;

const LOG_TARGET: &str = "channel_assist::sync";

const DEFAULT_INTERVAL_SECS: u64 = 900;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 90;
const DEFAULT_MAX_THREADS: usize = 500;

/// Durable namespace + artifact name of the email observe consent config
/// (kept in sync with `observe_connectors_api`, which owns writes).
const EMAIL_OBSERVE_NAMESPACE: &str = "email_observe";
const OBSERVE_CONFIG_NAME: &str = "config.json";

// ---------------------------------------------------------------------------
// Config + worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChannelSyncConfig {
    /// `CHANNEL_SYNC_ENABLED` kill-switch (default on). The observe consent
    /// config gates each tick independently.
    pub enabled: bool,
    pub interval: Duration,
    pub startup_delay: Duration,
    /// `CHANNEL_SYNC_MAX_THREADS`: per-account thread cap per pass.
    pub max_threads: usize,
}

impl Default for ChannelSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
            max_threads: DEFAULT_MAX_THREADS,
        }
    }
}

impl ChannelSyncConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("CHANNEL_SYNC_ENABLED") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_SYNC_INTERVAL_SECS") {
            if secs > 0 {
                config.interval = Duration::from_secs(secs);
            }
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_SYNC_STARTUP_DELAY_SECS") {
            config.startup_delay = Duration::from_secs(secs);
        }
        if let Some(cap) = env_parse::<usize>("CHANNEL_SYNC_MAX_THREADS") {
            if cap > 0 {
                config.max_threads = cap;
            }
        }
        config
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// Background sync worker handle (MemoryEvalRunner pattern).
#[derive(Debug)]
pub struct ChannelSyncWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelSyncWorker {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        store: MailAssistStore,
        config: ChannelSyncConfig,
        catch_up: Option<std::sync::Arc<ObserveCatchUpController>>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_periodic(workspace_layout, store, config, catch_up, cancel_for_task).await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_periodic(
    workspace_layout: ArtifactV2Workspace,
    store: MailAssistStore,
    config: ChannelSyncConfig,
    catch_up: Option<std::sync::Arc<ObserveCatchUpController>>,
    cancel: CancellationToken,
) {
    if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    run_pass_with_logging(&workspace_layout, &store, &config, catch_up.as_deref()).await;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {
                run_pass_with_logging(&workspace_layout, &store, &config, catch_up.as_deref()).await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

async fn run_pass_with_logging(
    workspace_layout: &ArtifactV2Workspace,
    store: &MailAssistStore,
    config: &ChannelSyncConfig,
    catch_up: Option<&ObserveCatchUpController>,
) {
    let outcome = run_sync_pass_inner(
        workspace_layout,
        store,
        DEFAULT_SCOPE_PRINCIPAL,
        DEFAULT_SCOPE_WORKSPACE,
        config,
        catch_up,
    )
    .await;
    if !outcome.enabled {
        debug!(
            target: LOG_TARGET,
            "no channel accounts resolved for the default scope (observe disabled, registry \
             empty); mail sync tick skipped"
        );
        return;
    }
    let failed = outcome
        .accounts
        .iter()
        .filter(|a| a.error.is_some())
        .count();
    let threads: usize = outcome.accounts.iter().map(|a| a.threads_synced).sum();
    let messages: usize = outcome.accounts.iter().map(|a| a.messages_inserted).sum();
    if failed > 0 {
        warn!(
            target: LOG_TARGET,
            accounts = outcome.accounts.len(),
            failed,
            threads,
            messages,
            "mail sync pass completed with account failures"
        );
    } else if !outcome.accounts.is_empty() {
        info!(
            target: LOG_TARGET,
            accounts = outcome.accounts.len(),
            threads,
            messages,
            "mail sync pass completed"
        );
    } else {
        debug!(
            target: LOG_TARGET,
            "channel sync pass found no eligible channel accounts"
        );
    }
}

// ---------------------------------------------------------------------------
// One sync pass (shared by the loop tick and `POST /channel-assist/sync/run`)
// ---------------------------------------------------------------------------

/// Per-account result of one pass — the `sync/run` response body.
#[derive(Debug, Clone, Serialize)]
pub struct AccountSyncSummary {
    pub account: String,
    /// `backfill` | `incremental` | `relist` | `relist_fallback` |
    /// `backfill_fallback` | `error`.
    pub mode: String,
    pub threads_synced: usize,
    pub messages_inserted: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncPassOutcome {
    /// Whether ANY channel ingestion is active for the scope: the
    /// `email_observe` producer is enabled OR the `channel_assist`
    /// registry resolved accounts.
    pub enabled: bool,
    pub accounts: Vec<AccountSyncSummary>,
}

/// Read the scope's `email_observe` consent config, READ-ONLY. Missing or
/// unreadable config means "disabled" — never an error, never a write.
pub async fn load_email_observe_config(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> ObserveConfig {
    let store = match open_local_durable_artifacts(workspace_layout, principal, workspace) {
        Ok(store) => store,
        Err(_) => return ObserveConfig::default(),
    };
    match store
        .read(EMAIL_OBSERVE_NAMESPACE, OBSERVE_CONFIG_NAME)
        .await
    {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => ObserveConfig::default(),
    }
}

/// Run one full sync pass for a scope: resolve the channel accounts
/// (registry + gmail observe fallback), then run each through its
/// provider's ingestor and persist the batch. Per-account isolation: an
/// account failure is recorded on its watermark row (with a one-shot
/// `gws auth status` probe appended for diagnosis on gmail) and reported
/// in the summary; the remaining accounts still run. The watermark only
/// advances after a batch persisted.
pub async fn run_sync_pass(
    workspace_layout: &ArtifactV2Workspace,
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    config: &ChannelSyncConfig,
) -> SyncPassOutcome {
    run_sync_pass_inner(workspace_layout, store, principal, workspace, config, None).await
}

async fn run_sync_pass_inner(
    workspace_layout: &ArtifactV2Workspace,
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    config: &ChannelSyncConfig,
    catch_up: Option<&ObserveCatchUpController>,
) -> SyncPassOutcome {
    // Unified observe+assist U2: both workers read the one `channel_observe`
    // config (migrated once from email_observe + channel_assist). "Enabled"
    // now means "has ≥1 enabled message-channel account" — calendar is
    // filtered out (it stays on the digest).
    let channel_config =
        super::channel_observe::load_or_migrate(workspace_layout, principal, workspace).await;
    let accounts = super::channel_observe::message_accounts(&channel_config);
    if accounts.is_empty() {
        return SyncPassOutcome {
            enabled: false,
            accounts: Vec::new(),
        };
    }
    let auth_root = workspace_layout.capability_auth_root(principal, workspace);
    // Same PATH-augmentation seam as the meetings surface: repo_root =
    // process CWD (the assumption `gws_binary()` already makes).
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scope_paths = CapabilityWorkspaceManager::new(workspace_layout.clone(), repo_root)
        .scope_paths(principal, workspace);
    // The scoped catch-up policy is the canonical history choice. The value
    // retained in channel_observe exists only for old clients and migration.
    let history_lookback_days = if let Some(controller) = catch_up {
        controller
            .status(principal, workspace)
            .await
            .policy
            .lookback_days
    } else {
        magician::magician_v2::observe_catchup::load_observe_catch_up_policy(
            workspace_layout,
            principal,
            workspace,
        )
        .await
        .lookback_days
    };
    let base_ctx = IngestContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        workspace_layout: workspace_layout.clone(),
        auth_root: auth_root.clone(),
        scope_paths: scope_paths.clone(),
        suppress_sensitive: channel_config.suppress_sensitive,
        backfill_days: history_lookback_days,
        min_internal_date: now_millis() - (history_lookback_days as i64) * 86_400_000,
        max_threads: config.max_threads,
        ignore_provider_cursor: false,
    };
    let ingestors = super::adapter_registry::default_channel_ingestors();

    let mut summaries = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let Some(ingestor) = ingestors
            .iter()
            .find(|ingestor| ingestor.provider() == account.provider)
        else {
            debug!(
                target: LOG_TARGET,
                provider = account.provider.as_str(),
                account = account.account_alias.as_str(),
                "no ingestor for provider yet; account skipped"
            );
            continue;
        };
        if !ingestor.account_ready(&base_ctx, account) {
            if let Some(reason) = ingestor.unavailable_reason(&base_ctx, account) {
                if record_account_unavailable(store, principal, workspace, account, &reason).await {
                    info!(
                        target: LOG_TARGET,
                        provider = account.provider.as_str(),
                        account = account.account_alias.as_str(),
                        reason = reason.as_str(),
                        "channel adapter unavailable; skipping account without spawning"
                    );
                }
                summaries.push(AccountSyncSummary {
                    account: account.account_alias.clone(),
                    mode: "unavailable".to_string(),
                    threads_synced: 0,
                    messages_inserted: 0,
                    error: Some(reason),
                });
            }
            // Phase-1 eligibility behavior: an account without local
            // credentials silently drops out — it is not an error.
            continue;
        }
        let source_id = format!("messages:{}:{}", account.provider, account.account_alias);
        let mut skipped_catch_up = false;
        let (ctx, admission): (IngestContext, Option<CatchUpAdmission>) = if let Some(controller) =
            catch_up
        {
            match controller
                .begin(
                    principal,
                    workspace,
                    &source_id,
                    &format!("{} · {}", account.provider, account.account_alias),
                    "threads",
                    CatchUpReplayMode::CheckpointedReplay,
                    config.max_threads,
                    "Provider retention and atomic page sizes can further constrain replay.",
                )
                .await
            {
                CatchUpDecision::Admit(admission) => {
                    let mut ctx = base_ctx.clone();
                    ctx.backfill_days = ((admission.boot_started_at_ms
                        - admission.historical_floor_ms)
                        / 86_400_000)
                        .max(1) as u32;
                    ctx.min_internal_date = admission.historical_floor_ms;
                    ctx.max_threads = admission.max_items;
                    ctx.ignore_provider_cursor = false;
                    (ctx, Some(admission))
                },
                CatchUpDecision::SkipHistorical {
                    retain_from_ms,
                    reason,
                } => {
                    debug!(target: LOG_TARGET, provider = account.provider, account = account.account_alias, reason, "historical channel catch-up skipped; advancing from boot boundary");
                    let mut ctx = base_ctx.clone();
                    ctx.backfill_days = 1;
                    ctx.min_internal_date = retain_from_ms;
                    ctx.ignore_provider_cursor = true;
                    skipped_catch_up = true;
                    (ctx, None)
                },
                CatchUpDecision::Normal => {
                    let mut ctx = base_ctx.clone();
                    ctx.backfill_days = 1;
                    ctx.min_internal_date = controller.boot_started_at_ms();
                    (ctx, None)
                },
            }
        } else {
            (base_ctx.clone(), None)
        };
        // The catch-up duration is an admission deadline. Once admitted, the
        // adapter finishes through its normal provider timeout so cancellation
        // cannot leave its cursor/lease state ambiguous.
        let sync_result = sync_one_account(store, &ctx, ingestor.as_ref(), account).await;
        match sync_result {
            Ok(summary) => {
                if let (Some(controller), Some(admission)) = (catch_up, admission) {
                    controller.complete(
                        principal,
                        workspace,
                        admission,
                        summary.threads_synced,
                        None,
                    );
                } else if let Some(controller) = catch_up.filter(|_| skipped_catch_up) {
                    controller.finish_skipped(
                        principal,
                        workspace,
                        &source_id,
                        &format!("{} · {}", account.provider, account.account_alias),
                        "threads",
                        CatchUpReplayMode::CheckpointedReplay,
                        "Provider retention and atomic page sizes can further constrain replay.",
                        None,
                        true,
                    );
                }
                summaries.push(summary)
            },
            Err(error) => {
                let mut detail = error.to_string();
                if account.provider == GMAIL_PROVIDER {
                    if let Some(probe) =
                        auth_status_probe(&auth_root, &account.account_alias, &scope_paths).await
                    {
                        detail = format!("{detail}; auth probe: {probe}");
                    }
                }
                warn!(
                    target: LOG_TARGET,
                    provider = account.provider.as_str(),
                    account = account.account_alias.as_str(),
                    error = detail.as_str(),
                    "mail sync failed for account"
                );
                record_account_failure(store, principal, workspace, account, &detail).await;
                if let (Some(controller), Some(admission)) = (catch_up, admission) {
                    controller.complete(principal, workspace, admission, 0, Some(&detail));
                } else if let Some(controller) = catch_up.filter(|_| skipped_catch_up) {
                    controller.finish_skipped(
                        principal,
                        workspace,
                        &source_id,
                        &format!("{} · {}", account.provider, account.account_alias),
                        "threads",
                        CatchUpReplayMode::CheckpointedReplay,
                        "Provider retention and atomic page sizes can further constrain replay.",
                        Some(&detail),
                        false,
                    );
                }
                summaries.push(AccountSyncSummary {
                    account: account.account_alias.clone(),
                    mode: "error".to_string(),
                    threads_synced: 0,
                    messages_inserted: 0,
                    error: Some(detail),
                });
            },
        }
    }
    SyncPassOutcome {
        enabled: true,
        accounts: summaries,
    }
}

/// One account pass: watermark → ingestor pull → persist → watermark
/// advance. Any error leaves the previous watermark untouched (the
/// never-advances-on-failure contract).
async fn sync_one_account(
    store: &MailAssistStore,
    ctx: &IngestContext,
    ingestor: &dyn ChannelIngestor,
    account: &ChannelAccount,
) -> Result<AccountSyncSummary> {
    let watermark = store
        .get_watermark(
            &ctx.principal,
            &ctx.workspace,
            &account.provider,
            &account.account_alias,
        )
        .await?;
    let effective_watermark = if ctx.ignore_provider_cursor {
        None
    } else {
        watermark.as_ref()
    };
    let batch = ingestor
        .sync_account(ctx, account, effective_watermark)
        .await?;
    persist_batch(store, ctx, account, watermark.as_ref(), batch).await
}

/// Persist one successful ingest batch and advance the account watermark.
/// No content travels with the batch: the rows' `distill_state = pending`
/// is the durable obligation, and the distill queue (`distill`) re-fetches
/// content by provider when it drains them — see the `ingest` module
/// header's content-handoff contract.
async fn persist_batch(
    store: &MailAssistStore,
    ctx: &IngestContext,
    account: &ChannelAccount,
    previous: Option<&SyncWatermark>,
    batch: IngestBatch,
) -> Result<AccountSyncSummary> {
    if let Some(account_email) = batch.account_email.as_deref() {
        store
            .reconcile_account_email(
                &ctx.principal,
                &ctx.workspace,
                &account.provider,
                &account.account_alias,
                account_email,
            )
            .await?;
    }
    let threads_synced = batch.threads.len();
    for record in batch.threads {
        store
            .upsert_thread(&ctx.principal, &ctx.workspace, record)
            .await?;
    }
    let mut messages = batch.messages;
    messages.sort_by(|a, b| {
        b.internal_date
            .cmp(&a.internal_date)
            .then_with(|| a.message_id.cmp(&b.message_id))
    });
    let messages_inserted = store
        .append_messages(&ctx.principal, &ctx.workspace, messages)
        .await?;
    store
        .append_provider_changes(&ctx.principal, &ctx.workspace, batch.provider_changes)
        .await?;
    let last_internal_date = max_option(
        previous.and_then(|w| w.last_internal_date),
        batch.max_internal_date,
    );
    let last_internal_date = if ctx.ignore_provider_cursor {
        Some(
            last_internal_date
                .unwrap_or(ctx.min_internal_date)
                .max(ctx.min_internal_date),
        )
    } else {
        last_internal_date
    };
    store
        .set_watermark(
            &ctx.principal,
            &ctx.workspace,
            SyncWatermark {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: account.provider.clone(),
                account_alias: account.account_alias.clone(),
                last_internal_date,
                provider_cursor: batch.next_provider_cursor,
                last_synced_at: now_millis(),
                last_error: None,
            },
        )
        .await?;
    Ok(AccountSyncSummary {
        account: account.account_alias.clone(),
        mode: batch.mode.to_string(),
        threads_synced,
        messages_inserted,
        error: None,
    })
}

/// Persist a failed pass on the account's watermark row: cursors survive
/// (only the error surface changes) so the next pass resumes where the
/// last SUCCESSFUL one stopped.
async fn record_account_failure(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    account: &ChannelAccount,
    detail: &str,
) {
    let previous = match store
        .get_watermark(
            principal,
            workspace,
            &account.provider,
            &account.account_alias,
        )
        .await
    {
        Ok(previous) => previous,
        Err(error) => {
            // Availability is diagnostic state. Never replace an unreadable
            // successful cursor with an empty watermark merely to record it.
            warn!(
                target: LOG_TARGET,
                account = account.account_alias.as_str(),
                error = %error,
                "failed to read watermark before recording adapter availability"
            );
            return;
        },
    };
    let watermark = SyncWatermark {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: account.provider.clone(),
        account_alias: account.account_alias.clone(),
        last_internal_date: previous.as_ref().and_then(|w| w.last_internal_date),
        provider_cursor: previous.as_ref().and_then(|w| w.provider_cursor.clone()),
        last_synced_at: now_millis(),
        last_error: Some(detail.chars().take(500).collect()),
    };
    if let Err(error) = store.set_watermark(principal, workspace, watermark).await {
        warn!(
            target: LOG_TARGET,
            account = account.account_alias.as_str(),
            error = %error,
            "failed to persist mail sync error status"
        );
    }
}

/// Persist stable adapter availability without rewriting the watermark on
/// every polling tick. `last_synced_at` describes provider work, so preserve
/// it when no subprocess was attempted; a later successful pass clears the
/// error through the ordinary batch commit.
async fn record_account_unavailable(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    account: &ChannelAccount,
    detail: &str,
) -> bool {
    let bounded_detail: String = detail.chars().take(500).collect();
    let previous = match store
        .get_watermark(
            principal,
            workspace,
            &account.provider,
            &account.account_alias,
        )
        .await
    {
        Ok(previous) => previous,
        Err(error) => {
            // This state is only diagnostic. If the durable cursor cannot be
            // read, do not replace it with a synthetic empty watermark.
            warn!(
                target: LOG_TARGET,
                account = account.account_alias.as_str(),
                error = %error,
                "failed to read watermark before recording adapter unavailability"
            );
            return false;
        },
    };
    if previous
        .as_ref()
        .and_then(|watermark| watermark.last_error.as_deref())
        == Some(bounded_detail.as_str())
    {
        return false;
    }
    let watermark = SyncWatermark {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: account.provider.clone(),
        account_alias: account.account_alias.clone(),
        last_internal_date: previous.as_ref().and_then(|w| w.last_internal_date),
        provider_cursor: previous.as_ref().and_then(|w| w.provider_cursor.clone()),
        // Zero means the account has never completed provider work. Do not
        // turn a local preflight failure into a fictitious successful sync.
        last_synced_at: previous
            .as_ref()
            .map_or(0, |watermark| watermark.last_synced_at),
        last_error: Some(bounded_detail),
    };
    match store.set_watermark(principal, workspace, watermark).await {
        Ok(()) => true,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                account = account.account_alias.as_str(),
                error = %error,
                "failed to persist channel adapter availability status"
            );
            false
        },
    }
}

fn max_option(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_adapter_preserves_the_last_successful_watermark() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = MailAssistStore::open(temp.path()).expect("mail store");
        let account = ChannelAccount {
            provider: "whatsapp_kapso".to_string(),
            account_alias: "envoy".to_string(),
            lane: Default::default(),
            enabled: true,
        };
        store
            .set_watermark(
                "owner",
                "default",
                SyncWatermark {
                    schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                    provider: account.provider.clone(),
                    account_alias: account.account_alias.clone(),
                    last_internal_date: Some(40),
                    provider_cursor: Some("cursor-1".to_string()),
                    last_synced_at: 42,
                    last_error: None,
                },
            )
            .await
            .unwrap();

        assert!(
            record_account_unavailable(
                &store,
                "owner",
                "default",
                &account,
                "Kapso CLI is unavailable",
            )
            .await
        );
        assert!(
            !record_account_unavailable(
                &store,
                "owner",
                "default",
                &account,
                "Kapso CLI is unavailable",
            )
            .await
        );

        let watermark = store
            .get_watermark(
                "owner",
                "default",
                &account.provider,
                &account.account_alias,
            )
            .await
            .unwrap()
            .expect("watermark");
        assert_eq!(watermark.last_synced_at, 42);
        assert_eq!(watermark.last_internal_date, Some(40));
        assert_eq!(watermark.provider_cursor.as_deref(), Some("cursor-1"));
        assert_eq!(
            watermark.last_error.as_deref(),
            Some("Kapso CLI is unavailable")
        );
    }

    #[tokio::test]
    async fn never_synced_unavailable_adapter_does_not_invent_a_success_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = MailAssistStore::open(temp.path()).expect("mail store");
        let account = ChannelAccount {
            provider: "whatsapp_kapso".to_string(),
            account_alias: "envoy".to_string(),
            lane: Default::default(),
            enabled: true,
        };

        assert!(
            record_account_unavailable(
                &store,
                "owner",
                "default",
                &account,
                "Kapso CLI is unavailable",
            )
            .await
        );
        let watermark = store
            .get_watermark(
                "owner",
                "default",
                &account.provider,
                &account.account_alias,
            )
            .await
            .unwrap()
            .expect("unavailable watermark");
        assert_eq!(watermark.last_synced_at, 0);
        assert_eq!(watermark.last_internal_date, None);
        assert_eq!(watermark.provider_cursor, None);
    }
}
