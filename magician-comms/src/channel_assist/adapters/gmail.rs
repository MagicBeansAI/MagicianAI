use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;

use anyhow::Result;
use async_trait::async_trait;
use tracing::info;

use magician::magician_v2::artifact_v2::CapabilityScopePaths;
use magician::magician_v2::gws_cli::{gws_binary, GWS_TIMEOUT};

use super::super::adapter_registry::{
    ChannelAdapter, ChannelCapabilities, ChannelConnectionStatus, ChannelDeepLinker,
    ChannelThreadRef,
};
use super::super::assist::content::{prepare_message_content, DistillContent};
use super::super::assist::distill::{ContentFetcher, DistillContext};
use super::super::gws_client::{
    GmailHistoryChange, GmailHistoryChangeKind, GmailMessageRef, GmailProfile, GmailThreadMeta,
    GwsGmailClient, GwsGmailError,
};
use super::super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::super::registry::ChannelAccount;
use super::super::sensitivity::is_sensitive;
use super::super::types::{
    ChannelLane, DistillState, MailMessageMeta, MailRecordOrigin, MailThreadRecord,
    MessageDirection, ProviderThreadChange, ProviderThreadChangeKind, SyncWatermark,
    MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};

pub const GMAIL_PROVIDER: &str = "gmail";

const LOG_TARGET: &str = "channel_assist::sync";

/// Page budgets per account pass — a runaway mailbox can't wedge a tick.
const MAX_LIST_PAGES: usize = 10;
const MAX_HISTORY_PAGES: usize = 10;
/// `messages list` page size (the API maximum).
const LIST_PAGE_SIZE: u32 = 500;

/// Overlap subtracted from the internal-date watermark when building the
/// `after:` re-list query. Gmail's `after:` takes epoch SECONDS and its
/// boundary behavior is fuzzy; over-fetching is harmless because the store
/// dedups messages by id, while under-fetching silently drops mail.
const RELIST_OVERLAP_SECS: i64 = 3600;
const HISTORY_CONTINUATION_PREFIX: &str = "gmail-history-page:v1:";

pub struct GmailAdapter;

impl ChannelAdapter for GmailAdapter {
    fn provider(&self) -> &'static str {
        GMAIL_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::pull_with_content()
            .with_deep_link()
            .with_connection_status()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(GmailIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(GmailContentFetcher))
    }

    fn deep_linker(&self) -> Option<&dyn ChannelDeepLinker> {
        Some(self)
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        Some(self)
    }
}

impl ChannelDeepLinker for GmailAdapter {
    fn thread_url(&self, thread: ChannelThreadRef<'_>) -> Option<String> {
        if thread.provider != GMAIL_PROVIDER || thread.thread_id.is_empty() {
            return None;
        }
        let email = thread
            .account_email
            .map(str::trim)
            .filter(|email| !email.is_empty())?;
        Some(format!(
            "https://mail.google.com/mail/?authuser={}#all/{}",
            urlencoding::encode(email),
            thread.thread_id
        ))
    }
}

impl ChannelConnectionStatus for GmailAdapter {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        let ingestor = GmailIngestor;
        ingestor.account_ready(ctx, account)
    }
}

/// Gmail [`ChannelIngestor`]: wraps the Phase-1 backfill / incremental /
/// suppression logic unchanged, minus persistence (the worker owns that).
pub struct GmailIngestor;

#[async_trait]
impl ChannelIngestor for GmailIngestor {
    fn provider(&self) -> &'static str {
        GMAIL_PROVIDER
    }

    /// Authenticated `gws-{alias}` profile present for the scope. This is
    /// the Phase-1 `eligible_accounts` filter: aliases without a profile dir
    /// drop out here.
    fn account_ready(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        ctx.auth_root
            .join(format!("gws-{}", account.account_alias))
            .exists()
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let client = GwsGmailClient::new(ctx.auth_root.clone(), Some(ctx.scope_paths.clone()));
        match decide_sync_plan(watermark, ctx.backfill_days) {
            SyncPlan::Backfill { query } => {
                run_list_plan(&client, ctx, account, &query, "backfill").await
            },
            SyncPlan::Relist { query } => {
                run_list_plan(&client, ctx, account, &query, "relist").await
            },
            SyncPlan::Incremental {
                start_history_id,
                page_token,
            } => {
                match run_incremental(
                    &client,
                    ctx,
                    account,
                    &start_history_id,
                    page_token.as_deref(),
                )
                .await
                {
                    Ok(batch) => Ok(batch),
                    Err(error)
                        if error
                            .downcast_ref::<GwsGmailError>()
                            .map(GwsGmailError::history_expired)
                            .unwrap_or(false) =>
                    {
                        // history_expired() is true for ANY gws 404, not only a
                        // lapsed cursor — safe here because the fallback
                        // (watermark re-list + profile re-baseline) is harmless:
                        // the store dedups messages by id.
                        info!(
                            target: LOG_TARGET,
                            account = account.account_alias.as_str(),
                            recovery = "watermark_relist",
                            "gmail history cursor expired; recovering with watermark re-list"
                        );
                        let (query, mode) = relist_fallback_plan(watermark, ctx.backfill_days);
                        run_list_plan(&client, ctx, account, &query, mode).await
                    },
                    Err(error) => Err(error),
                }
            },
        }
    }
}

/// Gmail content path: `messages.get format=full` → the deterministic
/// `content` pipeline (MIME walk → quote strip → chunk).
pub struct GmailContentFetcher;

#[async_trait]
impl ContentFetcher for GmailContentFetcher {
    fn provider(&self) -> &'static str {
        GMAIL_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let client = GwsGmailClient::new(ctx.auth_root.clone(), Some(ctx.scope_paths.clone()));
        let full = client
            .get_message_full(&message.account_alias, &message.message_id)
            .await
            .map_err(anyhow::Error::new)?;
        Ok(prepare_message_content(
            &full,
            ctx.chunk_chars,
            ctx.max_chunks,
        ))
    }
}

/// Backfill or watermark re-list: list refs for a query window, fetch each
/// distinct thread's metadata (cap enforced), and batch the rows with the
/// profile-baselined history cursor when the window was fully drained.
async fn run_list_plan(
    client: &GwsGmailClient,
    ctx: &IngestContext,
    account: &ChannelAccount,
    query: &str,
    mode: &'static str,
) -> Result<IngestBatch> {
    // Baseline the history cursor BEFORE listing: any change racing the
    // list is then replayed by the next incremental pass and deduped by
    // the store, instead of silently skipped. Doubles as the account-email
    // stamp and an early auth check.
    let profile = client
        .get_profile(&account.account_alias)
        .await
        .map_err(anyhow::Error::new)?;
    let account_email = required_account_email(&profile, &account.account_alias)?;

    let mut refs: Vec<GmailMessageRef> = Vec::new();
    let mut distinct_threads: HashSet<String> = HashSet::new();
    let mut page_token: Option<String> = None;
    let mut capped = false;
    for page_index in 0..MAX_LIST_PAGES {
        let page = client
            .list_message_refs(
                &account.account_alias,
                Some(query),
                LIST_PAGE_SIZE,
                page_token.as_deref(),
            )
            .await
            .map_err(anyhow::Error::new)?;
        for r in &page.refs {
            distinct_threads.insert(r.thread_id.clone());
        }
        refs.extend(page.refs);
        page_token = page.next_page_token;
        let has_next = page_token.is_some();
        let hit_thread_cap = if ctx.max_threads == 0 {
            !distinct_threads.is_empty() || has_next
        } else {
            distinct_threads.len() > ctx.max_threads
                || (distinct_threads.len() >= ctx.max_threads && has_next)
        };
        let hit_page_cap = page_index + 1 == MAX_LIST_PAGES && has_next;
        if hit_thread_cap || hit_page_cap {
            capped = true;
            break;
        }
        if !has_next {
            break;
        }
    }

    let thread_ids = dedupe_thread_ids(&refs, ctx.max_threads);
    let (mut batch, _, _) = collect_threads(
        client,
        ctx,
        account,
        Some(&account_email),
        &thread_ids,
        None,
    )
    .await?;
    batch.mode = mode;
    batch.account_email = Some(account_email);
    if capped {
        batch.max_internal_date = None;
    } else {
        batch.next_provider_cursor = profile.history_id.clone();
    }
    Ok(batch)
}

/// True incremental sync via `history.list(startHistoryId)`. Errors
/// bubble raw so the caller can branch on `history_expired()`.
async fn run_incremental(
    client: &GwsGmailClient,
    ctx: &IngestContext,
    account: &ChannelAccount,
    start_history_id: &str,
    initial_page_token: Option<&str>,
) -> Result<IngestBatch> {
    // Account identity is part of the durable row contract, not a backfill-only
    // optimization. Fetch it even for an empty history pass: persistence uses
    // it to heal rows written by older incremental syncs with NULL identity.
    let profile = client
        .get_profile(&account.account_alias)
        .await
        .map_err(anyhow::Error::new)?;
    let account_email = required_account_email(&profile, &account.account_alias)?;
    let mut affected: Vec<GmailMessageRef> = Vec::new();
    let mut history_changes: Vec<GmailHistoryChange> = Vec::new();
    let mut latest_history_id: Option<String> = None;
    let mut page_token = initial_page_token.map(str::to_string);
    let mut capped = false;
    for page_index in 0..MAX_HISTORY_PAGES {
        let page = client
            .history_since(
                &account.account_alias,
                start_history_id,
                page_token.as_deref(),
                ctx.max_threads,
            )
            .await
            .map_err(anyhow::Error::new)?;
        affected.extend(page.changes.iter().map(|change| change.message.clone()));
        history_changes.extend(page.changes);
        if page.latest_history_id.is_some() {
            latest_history_id = page.latest_history_id;
        }
        page_token = page.next_page_token;
        if page_token.is_none() {
            break;
        }
        let affected_threads = affected
            .iter()
            .map(|message| message.thread_id.as_str())
            .collect::<HashSet<_>>()
            .len();
        if ctx.max_threads == 0 || affected_threads >= ctx.max_threads {
            capped = true;
            break;
        }
        if page_index + 1 == MAX_HISTORY_PAGES {
            capped = true;
        }
    }

    let deleted_thread_ids = history_changes
        .iter()
        .filter(|change| change.kind == GmailHistoryChangeKind::MessageDeleted)
        .map(|change| change.message.thread_id.clone())
        .collect::<HashSet<_>>();
    // Once a history page has been consumed its opaque page token is the only
    // lossless continuation. Drain every affected thread in this bounded page
    // segment rather than applying the backfill thread cap and silently losing
    // the overflow when the cursor advances.
    let thread_ids = dedupe_thread_ids(&affected, usize::MAX);
    let (mut batch, missing_thread_ids, current_thread_labels) = collect_threads(
        client,
        ctx,
        account,
        Some(&account_email),
        &thread_ids,
        Some(&deleted_thread_ids),
    )
    .await?;
    batch.mode = "incremental";
    batch.account_email = Some(account_email);
    let observed_at = now_millis();
    let provider_changes = history_changes
        .into_iter()
        .filter_map(|change| {
            normalize_history_change(
                account,
                change,
                observed_at,
                &missing_thread_ids,
                &current_thread_labels,
            )
        })
        .collect();
    batch.provider_changes = provider_changes;
    if capped {
        // Do not advance the date fallback beyond an unfinished history
        // segment. The opaque page continuation is authoritative until every
        // affected thread in the segment has been consumed.
        batch.max_internal_date = None;
        batch.next_provider_cursor = page_token
            .as_deref()
            .map(|token| encode_history_continuation(start_history_id, token));
    } else {
        // No pages consumed (empty history) keeps the old cursor.
        batch.next_provider_cursor =
            latest_history_id.or_else(|| Some(start_history_id.to_string()));
    }
    Ok(batch)
}

fn required_account_email(profile: &GmailProfile, account_alias: &str) -> Result<String> {
    profile
        .email_address
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "gmail profile for account alias `{account_alias}` did not return emailAddress"
            )
        })
}

/// Fetch each thread's metadata, apply suppression + direction/lane
/// stamping at ingest, and derive the thread record — collecting rows
/// into a batch instead of persisting (the worker stores it, then
/// advances the watermark). Fails fast on the first thread error so
/// per-account isolation sees a real error, not a silent partial.
async fn collect_threads(
    client: &GwsGmailClient,
    ctx: &IngestContext,
    account: &ChannelAccount,
    account_email: Option<&str>,
    thread_ids: &[String],
    missing_thread_ids: Option<&HashSet<String>>,
) -> Result<(IngestBatch, HashSet<String>, HashMap<String, Vec<String>>)> {
    let mut batch = IngestBatch::default();
    let mut missing = HashSet::new();
    let mut current_thread_labels = HashMap::new();
    for thread_id in thread_ids {
        let mut thread = match client
            .get_thread_metadata(&account.account_alias, thread_id)
            .await
        {
            Ok(thread) => thread,
            Err(error)
                if error.not_found()
                    && missing_thread_ids.is_some_and(|ids| ids.contains(thread_id)) =>
            {
                // A history delta may identify a thread whose last message was
                // deleted. The normalized delete event is still persisted; a
                // missing metadata resource must not roll back that cursor.
                missing.insert(thread_id.clone());
                continue;
            },
            Err(error) => return Err(anyhow::Error::new(error)),
        };
        if let Some(latest) = thread
            .messages
            .iter()
            .max_by_key(|message| message.internal_date)
        {
            current_thread_labels.insert(thread_id.clone(), latest.label_ids.clone());
        }
        thread
            .messages
            .retain(|message| message.internal_date >= ctx.min_internal_date);
        if thread.messages.is_empty() {
            continue;
        }
        if ctx.suppress_sensitive {
            suppress_sensitive_messages(&mut thread.messages);
        }
        for message in &mut thread.messages {
            if let Some(email) = account_email {
                message.account_email = Some(email.to_string());
            }
            message.direction = Some(derive_gmail_direction(
                &message.label_ids,
                message.from_address.as_deref(),
                account_email,
            ));
        }
        let observed_at = now_millis();
        let Some(record) = build_thread_record(
            &thread,
            &account.account_alias,
            account.lane,
            account_email,
            observed_at,
        ) else {
            continue; // messageless thread — nothing to store
        };
        batch.max_internal_date = max_option(batch.max_internal_date, record.last_message_at);
        batch.threads.push(record);
        batch.messages.append(&mut thread.messages);
    }
    // No content is collected here: the rows' pending distill_state keeps
    // the obligation durable, and the distill queue fetches bodies via
    // `messages.get format=full` when it drains them.
    Ok((batch, missing, current_thread_labels))
}

fn normalize_history_change(
    account: &ChannelAccount,
    change: GmailHistoryChange,
    observed_at: i64,
    missing_thread_ids: &HashSet<String>,
    current_thread_labels: &HashMap<String, Vec<String>>,
) -> Option<ProviderThreadChange> {
    let kind = match change.kind {
        GmailHistoryChangeKind::MessageAdded => return None,
        GmailHistoryChangeKind::MessageDeleted => ProviderThreadChangeKind::MessageDeleted,
        GmailHistoryChangeKind::LabelsAdded => ProviderThreadChangeKind::LabelsAdded,
        GmailHistoryChangeKind::LabelsRemoved => ProviderThreadChangeKind::LabelsRemoved,
    };
    let cursor = change
        .history_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let thread_removed = missing_thread_ids.contains(&change.message.thread_id);
    let current_label_ids = current_thread_labels
        .get(&change.message.thread_id)
        .cloned()
        .unwrap_or_default();
    Some(ProviderThreadChange {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: format!(
            "provider-change:gmail:{}:{}:{}:{}:{}:{}",
            account.account_alias,
            cursor,
            kind.as_db_str(),
            change.message.thread_id,
            change.message.message_id,
            change.label_ids.join(",")
        ),
        provider: GMAIL_PROVIDER.to_string(),
        account_alias: account.account_alias.clone(),
        thread_id: change.message.thread_id,
        message_id: Some(change.message.message_id),
        kind,
        thread_removed,
        label_ids: change.label_ids,
        current_label_ids,
        provider_cursor: change.history_id,
        observed_at,
    })
}

/// One `gws auth status` probe run ONLY after a sync failure (matches the
/// /observe/accounts presence-check convention of not probing eagerly) so
/// the persisted error says WHY — lapsed token vs missing profile — not
/// just a bare API error.
pub async fn auth_status_probe(
    auth_root: &Path,
    account: &str,
    scope_paths: &CapabilityScopePaths,
) -> Option<String> {
    let config_dir = auth_root.join(format!("gws-{account}"));
    if !config_dir.exists() {
        return Some("no gws profile dir for account".to_string());
    }
    let cloudsdk_config_dir = config_dir.join("cloudsdk");
    // The scope's tool-bin PATH is computed first so the binary is resolved
    // against the PATH the child will see and the spawn stays on
    // `posix_spawn` (see `runtime_core::process`).
    let parent = std::env::var("PATH").unwrap_or_default();
    let child_path = scope_paths.subprocess_bin_path(None, &parent);
    let mut cmd = tokio::process::Command::new(runtime_core::process::resolve_program(
        gws_binary().as_os_str(),
        child_path.as_deref().map(std::ffi::OsStr::new),
    ));
    cmd.arg("auth")
        .arg("status")
        .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &config_dir)
        .env("CLOUDSDK_CONFIG", &cloudsdk_config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path) = child_path {
        cmd.env("PATH", path);
    }
    let output = tokio::time::timeout(GWS_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let text = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        } else {
            stdout
        }
    };
    let condensed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let brief: String = condensed.chars().take(200).collect();
    (!brief.is_empty()).then_some(brief)
}

/// What one account pass should do, decided purely from its watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPlan {
    /// First sync (or a watermark with no usable cursor): query window.
    Backfill { query: String },
    /// Cursorless watermark: re-list from the internal-date cursor.
    Relist { query: String },
    /// History cursor available: true incremental.
    Incremental {
        start_history_id: String,
        page_token: Option<String>,
    },
}

pub fn decide_sync_plan(watermark: Option<&SyncWatermark>, backfill_days: u32) -> SyncPlan {
    match watermark {
        None => SyncPlan::Backfill {
            query: backfill_query(backfill_days),
        },
        Some(w) => match (&w.provider_cursor, w.last_internal_date) {
            (Some(history_id), _) => {
                let (start_history_id, page_token) = decode_history_continuation(history_id);
                SyncPlan::Incremental {
                    start_history_id,
                    page_token,
                }
            },
            (None, Some(internal_date_ms)) => SyncPlan::Relist {
                query: relist_query(internal_date_ms),
            },
            (None, None) => SyncPlan::Backfill {
                query: backfill_query(backfill_days),
            },
        },
    }
}

fn encode_history_continuation(start_history_id: &str, page_token: &str) -> String {
    let payload = serde_json::to_string(&(start_history_id, page_token))
        .expect("serializing Gmail history continuation cannot fail");
    format!("{HISTORY_CONTINUATION_PREFIX}{payload}")
}

fn decode_history_continuation(cursor: &str) -> (String, Option<String>) {
    let Some(payload) = cursor.strip_prefix(HISTORY_CONTINUATION_PREFIX) else {
        return (cursor.to_string(), None);
    };
    serde_json::from_str::<(String, String)>(payload)
        .map(|(history_id, page_token)| (history_id, Some(page_token)))
        // A malformed internal continuation fails safe by treating the stored
        // value as an ordinary provider cursor. Gmail will reject it and the
        // established 404 re-list recovery will re-baseline the account.
        .unwrap_or_else(|_| (cursor.to_string(), None))
}

/// The plan after `history.list` 404s: re-list from the stored
/// internal-date watermark when one exists, else a fresh backfill window.
pub fn relist_fallback_plan(
    watermark: Option<&SyncWatermark>,
    backfill_days: u32,
) -> (String, &'static str) {
    match watermark.and_then(|w| w.last_internal_date) {
        Some(internal_date_ms) => (relist_query(internal_date_ms), "relist_fallback"),
        None => (backfill_query(backfill_days), "backfill_fallback"),
    }
}

fn backfill_query(days: u32) -> String {
    format!("newer_than:{days}d")
}

/// `after:` takes epoch SECONDS; subtract [`RELIST_OVERLAP_SECS`] so
/// boundary messages re-fetch (deduped) rather than drop.
fn relist_query(last_internal_date_ms: i64) -> String {
    let secs = (last_internal_date_ms / 1000)
        .saturating_sub(RELIST_OVERLAP_SECS)
        .max(0);
    format!("after:{secs}")
}

/// Distinct thread ids in first-seen order, capped — the per-account
/// thread budget for one pass.
pub fn dedupe_thread_ids(refs: &[GmailMessageRef], cap: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        if out.len() >= cap {
            break;
        }
        if seen.insert(r.thread_id.as_str()) {
            out.push(r.thread_id.clone());
        }
    }
    out
}

/// Redact matching messages in place (subject → placeholder, flag set;
/// ids/sender retained per the M1 contract) and short-circuit them out of
/// the distillation queue: suppressed rows are appended with
/// `distill_state = suppressed` DIRECTLY, so they never sit in `pending`
/// and no summary is ever derived for them. Returns whether any matched.
pub fn suppress_sensitive_messages(messages: &mut [MailMessageMeta]) -> bool {
    let mut any = false;
    for message in messages.iter_mut() {
        if is_sensitive(message.subject.as_deref(), message.from_address.as_deref()) {
            message.subject = Some(REDACTED_SUBJECT_PLACEHOLDER.to_string());
            message.sensitive_suppressed = true;
            message.distill_state = DistillState::Suppressed;
            any = true;
        }
    }
    any
}

/// Cheap gmail direction derivation (design: "gmail rows derive it from
/// sender==self where cheap"): a message the account sent carries the
/// SENT label; the sender-address match uses the profile identity fetched on
/// both list and incremental passes. Everything else in a synced mailbox is
/// inbound.
pub fn derive_gmail_direction(
    label_ids: &[String],
    from_address: Option<&str>,
    account_email: Option<&str>,
) -> MessageDirection {
    let sent_label = label_ids.iter().any(|label| label == "SENT");
    let from_self = matches!(
        (from_address, account_email),
        (Some(from), Some(own)) if from.eq_ignore_ascii_case(own)
    );
    if sent_label || from_self {
        MessageDirection::Outbound
    } else {
        MessageDirection::Inbound
    }
}

/// Derive the thread's current-state row from its (already suppressed)
/// messages: headline fields from the latest message, recipient domains
/// unioned across all messages, redacted subject + sensitive flag when ANY
/// message was suppressed, lane stamped from the ingesting account.
/// `None` for a messageless thread.
pub fn build_thread_record(
    thread: &GmailThreadMeta,
    account_alias: &str,
    lane: ChannelLane,
    account_email: Option<&str>,
    observed_at: i64,
) -> Option<MailThreadRecord> {
    let latest = thread.messages.iter().max_by_key(|m| m.internal_date)?;
    let sensitive = thread.messages.iter().any(|m| m.sensitive_suppressed);
    let mut seen = HashSet::new();
    let mut recipient_domains = Vec::new();
    for message in &thread.messages {
        for domain in message.to_domains.iter().chain(message.cc_domains.iter()) {
            if seen.insert(domain.clone()) {
                recipient_domains.push(domain.clone());
            }
        }
    }
    Some(MailThreadRecord {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: GMAIL_PROVIDER.to_string(),
        account_alias: account_alias.to_string(),
        account_email: account_email.map(str::to_string),
        thread_id: thread.thread_id.clone(),
        lane,
        subject: if sensitive {
            Some(REDACTED_SUBJECT_PLACEHOLDER.to_string())
        } else {
            latest.subject.clone()
        },
        // Only the distiller writes summaries; the upsert's COALESCE keeps an
        // existing one when this None lands on re-sync.
        latest_summary: None,
        latest_from_name: latest.from_name.clone(),
        latest_from_address: latest.from_address.clone(),
        recipient_domains,
        label_ids: latest.label_ids.clone(),
        message_count: thread.messages.len() as i64,
        last_message_at: Some(latest.internal_date),
        provider_cursor: thread.history_id.clone(),
        sensitive_suppressed: sensitive,
        origin: MailRecordOrigin::MetadataSync,
        first_observed_at: observed_at,
        last_observed_at: observed_at,
    })
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

    fn watermark(last_internal_date: Option<i64>, provider_cursor: Option<&str>) -> SyncWatermark {
        SyncWatermark {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: GMAIL_PROVIDER.to_string(),
            account_alias: "acct-a".to_string(),
            last_internal_date,
            provider_cursor: provider_cursor.map(str::to_string),
            last_synced_at: 1_000,
            last_error: None,
        }
    }

    fn message(id: &str, subject: Option<&str>, from: Option<&str>) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: GMAIL_PROVIDER.to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "ttt111".to_string(),
            message_id: id.to_string(),
            provider_cursor: None,
            label_ids: vec!["INBOX".to_string()],
            subject: subject.map(str::to_string),
            from_name: None,
            from_address: from.map(str::to_string),
            to_domains: vec!["one.example".to_string()],
            cc_domains: Vec::new(),
            internal_date: 1_000,
            observed_at: 1_100,
            direction: None,
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[test]
    fn deleted_message_only_closes_work_when_the_thread_resource_is_gone() {
        let account = ChannelAccount {
            provider: GMAIL_PROVIDER.to_string(),
            account_alias: "acct-a".to_string(),
            lane: ChannelLane::UserAssist,
            enabled: true,
        };
        let history_change = GmailHistoryChange {
            history_id: Some("history-2".to_string()),
            kind: GmailHistoryChangeKind::MessageDeleted,
            message: GmailMessageRef {
                message_id: "message-1".to_string(),
                thread_id: "thread-1".to_string(),
            },
            label_ids: Vec::new(),
        };

        let live_thread_change = normalize_history_change(
            &account,
            history_change.clone(),
            2_000,
            &HashSet::new(),
            &HashMap::new(),
        )
        .expect("normalized delete");
        assert!(!live_thread_change.thread_removed);
        assert!(!live_thread_change.closes_active_work());

        let removed_threads = HashSet::from(["thread-1".to_string()]);
        let removed_thread_change = normalize_history_change(
            &account,
            history_change,
            2_000,
            &removed_threads,
            &HashMap::new(),
        )
        .expect("normalized delete");
        assert!(removed_thread_change.thread_removed);
        assert!(removed_thread_change.closes_active_work());
    }

    #[test]
    fn no_watermark_selects_backfill_window() {
        assert_eq!(
            decide_sync_plan(None, 14),
            SyncPlan::Backfill {
                query: "newer_than:14d".to_string()
            }
        );
    }

    #[test]
    fn history_cursor_selects_incremental() {
        let w = watermark(Some(9_999_000), Some("hist-42"));
        assert_eq!(
            decide_sync_plan(Some(&w), 14),
            SyncPlan::Incremental {
                start_history_id: "hist-42".to_string(),
                page_token: None,
            }
        );
    }

    #[test]
    fn history_page_continuation_round_trips_through_the_opaque_watermark() {
        let encoded = encode_history_continuation("hist-42", "page/token+2=");
        let w = watermark(Some(9_999_000), Some(&encoded));
        assert_eq!(
            decide_sync_plan(Some(&w), 14),
            SyncPlan::Incremental {
                start_history_id: "hist-42".to_string(),
                page_token: Some("page/token+2=".to_string()),
            }
        );
    }

    #[test]
    fn gmail_profile_email_is_required_and_trimmed_for_durable_identity() {
        let profile = GmailProfile {
            email_address: Some("  owner@example.com  ".to_string()),
            history_id: Some("hist-1".to_string()),
            messages_total: Some(1),
        };
        assert_eq!(
            required_account_email(&profile, "work").unwrap(),
            "owner@example.com"
        );

        let missing = GmailProfile {
            email_address: None,
            history_id: Some("hist-1".to_string()),
            messages_total: Some(1),
        };
        assert!(required_account_email(&missing, "work")
            .unwrap_err()
            .to_string()
            .contains("emailAddress"));

        let blank = GmailProfile {
            email_address: Some("   ".to_string()),
            history_id: Some("hist-1".to_string()),
            messages_total: Some(1),
        };
        assert!(required_account_email(&blank, "work").is_err());
    }

    #[test]
    fn cursorless_watermark_relists_from_internal_date_with_overlap() {
        let w = watermark(Some(10_000_000), None);
        assert_eq!(
            decide_sync_plan(Some(&w), 14),
            SyncPlan::Relist {
                query: "after:6400".to_string()
            }
        );
        let early = watermark(Some(1_000), None);
        assert_eq!(
            decide_sync_plan(Some(&early), 14),
            SyncPlan::Relist {
                query: "after:0".to_string()
            }
        );
    }

    #[test]
    fn empty_watermark_backfills() {
        let w = watermark(None, None);
        assert_eq!(
            decide_sync_plan(Some(&w), 7),
            SyncPlan::Backfill {
                query: "newer_than:7d".to_string()
            }
        );
    }

    #[test]
    fn history_expired_fallback_relists_when_internal_date_known() {
        let w = watermark(Some(10_000_000), Some("hist-42"));
        assert_eq!(
            relist_fallback_plan(Some(&w), 14),
            ("after:6400".to_string(), "relist_fallback")
        );
    }

    #[test]
    fn history_expired_fallback_backfills_without_internal_date() {
        let w = watermark(None, Some("hist-42"));
        assert_eq!(
            relist_fallback_plan(Some(&w), 14),
            ("newer_than:14d".to_string(), "backfill_fallback")
        );
        assert_eq!(
            relist_fallback_plan(None, 14),
            ("newer_than:14d".to_string(), "backfill_fallback")
        );
    }

    #[test]
    fn thread_cap_dedupes_and_truncates_in_first_seen_order() {
        let refs: Vec<GmailMessageRef> = ["t1", "t2", "t1", "t3", "t2", "t4"]
            .iter()
            .enumerate()
            .map(|(i, tid)| GmailMessageRef {
                message_id: format!("m{i}"),
                thread_id: tid.to_string(),
            })
            .collect();
        assert_eq!(dedupe_thread_ids(&refs, 3), vec!["t1", "t2", "t3"]);
        assert_eq!(dedupe_thread_ids(&refs, 100), vec!["t1", "t2", "t3", "t4"]);
        assert!(dedupe_thread_ids(&refs, 0).is_empty());
    }

    #[test]
    fn suppression_redacts_matches_only_and_keeps_ids() {
        let mut messages = vec![
            message("m1", Some("Your OTP for login"), None),
            message("m2", Some("Quarterly sync notes"), None),
        ];
        assert!(suppress_sensitive_messages(&mut messages));

        assert!(messages[0].sensitive_suppressed);
        assert_eq!(
            messages[0].subject.as_deref(),
            Some(REDACTED_SUBJECT_PLACEHOLDER)
        );
        assert_eq!(messages[0].distill_state, DistillState::Suppressed);
        assert_eq!(messages[0].message_id, "m1");
        assert_eq!(messages[0].thread_id, "ttt111");

        assert!(!messages[1].sensitive_suppressed);
        assert_eq!(messages[1].subject.as_deref(), Some("Quarterly sync notes"));
        assert_eq!(messages[1].distill_state, DistillState::Pending);

        let mut clean = vec![message("m3", Some("Lunch tomorrow?"), None)];
        assert!(!suppress_sensitive_messages(&mut clean));
        assert!(!clean[0].sensitive_suppressed);
        assert_eq!(clean[0].distill_state, DistillState::Pending);
    }

    #[test]
    fn gmail_direction_uses_sent_label_or_self_sender_else_inbound() {
        let sent = vec!["SENT".to_string(), "IMPORTANT".to_string()];
        let inbox = vec!["INBOX".to_string()];
        assert_eq!(
            derive_gmail_direction(&sent, None, None),
            MessageDirection::Outbound
        );
        assert_eq!(
            derive_gmail_direction(&inbox, Some("Owner@One.Example"), Some("owner@one.example")),
            MessageDirection::Outbound
        );
        assert_eq!(
            derive_gmail_direction(
                &inbox,
                Some("colleague@partner.example"),
                Some("owner@one.example")
            ),
            MessageDirection::Inbound
        );
        assert_eq!(
            derive_gmail_direction(&inbox, Some("colleague@partner.example"), None),
            MessageDirection::Inbound
        );
        assert_eq!(
            derive_gmail_direction(&[], None, None),
            MessageDirection::Inbound
        );
    }

    #[test]
    fn thread_record_derives_headline_from_latest_and_unions_domains() {
        let mut older = message("m1", Some("first subject"), Some("a@one.example"));
        older.internal_date = 1_000;
        older.cc_domains = vec!["two.example".to_string()];
        let mut newer = message("m2", Some("latest subject"), Some("b@two.example"));
        newer.internal_date = 2_000;
        newer.to_domains = vec!["three.example".to_string(), "one.example".to_string()];
        let thread = GmailThreadMeta {
            thread_id: "ttt111".to_string(),
            history_id: Some("h9".to_string()),
            messages: vec![older, newer],
        };

        let record = build_thread_record(
            &thread,
            "acct-a",
            ChannelLane::UserAssist,
            Some("owner@one.example"),
            5_000,
        )
        .expect("record");
        assert_eq!(record.subject.as_deref(), Some("latest subject"));
        assert_eq!(record.lane, ChannelLane::UserAssist);
        assert_eq!(record.latest_summary, None);
        assert_eq!(record.latest_from_address.as_deref(), Some("b@two.example"));
        assert_eq!(record.message_count, 2);
        assert_eq!(record.last_message_at, Some(2_000));
        assert_eq!(record.provider_cursor.as_deref(), Some("h9"));
        assert_eq!(
            record.recipient_domains,
            vec!["one.example", "two.example", "three.example"]
        );
        assert!(!record.sensitive_suppressed);
        assert_eq!(record.first_observed_at, 5_000);
        assert_eq!(record.account_email.as_deref(), Some("owner@one.example"));

        let empty = GmailThreadMeta {
            thread_id: "ttt-empty".to_string(),
            history_id: None,
            messages: Vec::new(),
        };
        assert!(build_thread_record(&empty, "acct-a", ChannelLane::UserAssist, None, 1).is_none());
    }

    #[test]
    fn thread_record_stamps_the_account_lane() {
        let thread = GmailThreadMeta {
            thread_id: "ttt111".to_string(),
            history_id: None,
            messages: vec![message("m1", Some("envoy correspondence"), None)],
        };
        let record = build_thread_record(&thread, "presto", ChannelLane::Envoy, None, 5_000)
            .expect("record");
        assert_eq!(record.lane, ChannelLane::Envoy);
        assert_eq!(record.account_alias, "presto");
    }

    #[test]
    fn thread_record_redacts_when_any_message_suppressed() {
        let mut suppressed = message("m1", Some("Your OTP for login"), None);
        suppressed.internal_date = 1_000;
        let mut clean = vec![suppressed, message("m2", Some("normal reply"), None)];
        clean[1].internal_date = 2_000;
        suppress_sensitive_messages(&mut clean);
        let thread = GmailThreadMeta {
            thread_id: "ttt111".to_string(),
            history_id: None,
            messages: clean,
        };
        let record = build_thread_record(&thread, "acct-a", ChannelLane::UserAssist, None, 5_000)
            .expect("record");
        assert!(record.sensitive_suppressed);
        assert_eq!(
            record.subject.as_deref(),
            Some(REDACTED_SUBJECT_PLACEHOLDER)
        );
        assert_eq!(record.thread_id, "ttt111");
    }
}
