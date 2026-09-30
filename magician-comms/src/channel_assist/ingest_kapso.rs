//! Kapso WhatsApp ingestor (Phase 1b N5) — Presto's OWN WhatsApp number
//! (the ENVOY lane), via the Kapso/Meta-Cloud CLI.
//!
//! Unlike the wu.db lane (the user's personal WhatsApp), this reads
//! Presto's agent-identity number through the `kapso` CLI (`KAPSO_API_KEY`
//! from `operator-config.yaml` `secrets:`, per-number reads scoped with
//! `--phone-number-id $KAPSO_PHONE_NUMBER_ID`). It only READS; sending is
//! the separate capped send skill and is never touched here.
//!
//! ## Kapso CLI shapes (captured from a live read-only probe, 2026-07)
//!
//! `kapso whatsapp messages list --phone-number-id <pnid> --per-page N
//! [--after <cursor>] --output json` →
//! ```text
//! { "data": [ {
//!     "id": "wamid.…",                    // message id
//!     "timestamp": "1782353802",          // unix SECONDS (string)
//!     "type": "text", "to": "…",
//!     "text": { "body": "…" },            // CONTENT — never persisted
//!     "kapso": {
//!       "direction": "inbound|outbound",
//!       "contact_name": "…",
//!       "phone_number": "…",
//!       "whatsapp_conversation_id": "…",  // thread id
//!       "has_media": false,
//!       "content": "…"                    // CONTENT — never persisted
//!     } } ],
//!   "paging": { "next": "<cursor>|null", … } }
//! ```
//! `kapso whatsapp messages get <id> --output json` → one message object
//! (root or under `data`) with the same `text.body` / `kapso.content`.
//!
//! ## Mapping
//!
//! - `id` → `message_id`; `kapso.whatsapp_conversation_id` → `thread_id`;
//!   `kapso.direction` → direction (always present); `kapso.contact_name` /
//!   `kapso.phone_number` → thread subject + `from_name`/`from_address`;
//!   `timestamp` (unix secs) → `internal_date` millis.
//! - No email-style domains → `recipient_domains` empty; the lane comes
//!   from the account (envoy).
//! - Message BODY (`kapso.content` / `text.body`) is fetched ONLY at
//!   distill time via [`KapsoContentFetcher`] and never persisted.
//!
//! ## Windowing
//!
//! Kapso is cursor-paginated (`paging.next` → `--after`) newest-first.
//! The watermark cursor is the max `timestamp` (unix secs) seen; the
//! worker filters in Rust (`timestamp >= floor`) and stops paging once a
//! page falls entirely below the floor or the thread cap is hit. The
//! store dedups by message id, so a boundary re-read is harmless.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use magician::magician_v2::artifact_v2::workspace::runtime_config_path;
use magician::magician_v2::artifact_v2::CapabilityScopePaths;

use super::assist::content::{prepare_for_distill, DistillContent};
use super::assist::distill::{ContentFetcher, DistillContext};
use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::registry::ChannelAccount;
use super::sensitivity::is_sensitive;
use super::types::{
    DistillState, MailMessageMeta, MailRecordOrigin, MailThreadRecord, MessageDirection,
    SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};

/// Provider key for the envoy WhatsApp lane (Presto's Kapso number).
pub const KAPSO_PROVIDER: &str = "whatsapp_kapso";

const KAPSO_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PER_PAGE: u32 = 100;
/// Page budget per pass — a very active number can't wedge a tick.
const MAX_PAGES: usize = 20;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn kapso_binary() -> String {
    std::env::var("KAPSO_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "kapso".to_string())
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn executable_on_path(binary: &str, search_path: Option<&OsStr>) -> bool {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return executable_file(candidate);
    }
    search_path.is_some_and(|path| {
        std::env::split_paths(path).any(|directory| executable_file(&directory.join(binary)))
    })
}

fn kapso_binary_available(ctx: &IngestContext) -> bool {
    let binary = kapso_binary();
    let inherited = std::env::var("PATH").unwrap_or_default();
    let augmented = ctx.scope_paths.subprocess_bin_path(None, &inherited);
    executable_on_path(
        &binary,
        augmented
            .as_deref()
            .map(OsStr::new)
            .or_else(|| Some(OsStr::new(&inherited))),
    )
}

fn scoped_kapso_environment(
    scope_paths: &CapabilityScopePaths,
    inherited_path: &str,
) -> (Option<String>, std::path::PathBuf) {
    (
        scope_paths.subprocess_bin_path(None, inherited_path),
        scope_paths.home_root.clone(),
    )
}

/// `(api_key, phone_number_id)` from env overrides, else the
/// `operator-config.yaml` `secrets:` block. `None` when either is absent
/// (the account then silently drops out, like an unauthenticated gmail
/// alias).
fn kapso_credentials() -> Option<(String, String)> {
    let env_key = std::env::var("KAPSO_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_pnid = std::env::var("KAPSO_PHONE_NUMBER_ID")
        .ok()
        .filter(|s| !s.trim().is_empty());
    if let (Some(k), Some(p)) = (&env_key, &env_pnid) {
        return Some((k.clone(), p.clone()));
    }
    let raw = std::fs::read_to_string(runtime_config_path(
        "operator-config.yaml",
        "skillshub/operator-config.yaml",
    ))
    .ok()?;
    let cfg: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
    let secrets = cfg.get("secrets");
    let key = env_key.or_else(|| {
        secrets
            .and_then(|s| s.get("KAPSO_API_KEY"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })?;
    let pnid = env_pnid.or_else(|| {
        secrets
            .and_then(|s| s.get("KAPSO_PHONE_NUMBER_ID"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })?;
    if key.trim().is_empty() || pnid.trim().is_empty() {
        return None;
    }
    Some((key, pnid))
}

// ---------------------------------------------------------------------------
// CLI client (adapter interior)
// ---------------------------------------------------------------------------

struct KapsoClient {
    api_key: String,
    phone_number_id: String,
    scope_paths: Option<CapabilityScopePaths>,
}

impl KapsoClient {
    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        // The executable is installation-shared, while all writable CLI state
        // is scope-owned. This lets many workspaces reuse one immutable Kapso
        // package without sharing login/cache state through the host HOME.
        // The scoped environment is computed first so the binary is resolved
        // against the PATH the child will see and the spawn stays on
        // `posix_spawn` (see `runtime_core::process`).
        let scoped = self.scope_paths.as_ref().map(|sp| {
            let parent = std::env::var("PATH").unwrap_or_default();
            scoped_kapso_environment(sp, &parent)
        });
        let child_path = scoped.as_ref().and_then(|(path, _)| path.as_deref());
        let mut cmd = Command::new(runtime_core::process::resolve_program_str(
            &kapso_binary(),
            child_path,
        ));
        cmd.arg("whatsapp");
        for arg in args {
            cmd.arg(arg);
        }
        cmd.arg("--output")
            .arg("json")
            .env("KAPSO_API_KEY", &self.api_key)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some((path, home)) = scoped {
            tokio::fs::create_dir_all(&home)
                .await
                .map_err(|_| anyhow!("failed to prepare scoped Kapso home"))?;
            if let Some(p) = path {
                cmd.env("PATH", p);
            }
            cmd.env("HOME", home);
        }
        let output = tokio::time::timeout(KAPSO_TIMEOUT, cmd.output())
            .await
            .map_err(|_| anyhow!("kapso timed out"))?
            .map_err(|e| anyhow!("spawn kapso: {e}"))?;
        if !output.status.success() {
            let detail: String = String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(300)
                .collect();
            anyhow::bail!("kapso failed: {detail}");
        }
        Ok(output.stdout)
    }

    async fn list_messages(&self, per_page: u32, after: Option<&str>) -> Result<KapsoMessagesPage> {
        let per = per_page.to_string();
        let pnid = self.phone_number_id.clone();
        let mut args = vec![
            "messages",
            "list",
            "--phone-number-id",
            pnid.as_str(),
            "--per-page",
            per.as_str(),
        ];
        if let Some(cursor) = after {
            args.push("--after");
            args.push(cursor);
        }
        let out = self.run(&args).await?;
        parse_messages_page(&out)
    }

    async fn get_message_body(&self, id: &str) -> Result<Option<String>> {
        let out = self.run(&["messages", "get", id]).await?;
        Ok(parse_message_body(&out))
    }
}

// ---------------------------------------------------------------------------
// Pure parsing + mapping (unit-tested; no live CLI)
// ---------------------------------------------------------------------------

/// One kapso message row (no body — fetched at distill time).
#[derive(Debug, Clone)]
struct KapsoRawMessage {
    id: String,
    conversation_id: String,
    direction: Option<String>,
    contact_name: Option<String>,
    phone_number: Option<String>,
    timestamp_secs: i64,
}

struct KapsoMessagesPage {
    messages: Vec<KapsoRawMessage>,
    next_cursor: Option<String>,
}

/// Parse a `timestamp` field: unix-seconds string (the observed shape),
/// tolerant of an integer or an RFC3339 string.
fn parse_timestamp(value: &Value) -> i64 {
    if let Some(n) = value.as_i64() {
        return n;
    }
    if let Some(s) = value.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return n;
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.timestamp();
        }
    }
    0
}

fn parse_messages_page(bytes: &[u8]) -> Result<KapsoMessagesPage> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow!("parsing kapso messages: {e}"))?;
    let empty = Vec::new();
    let data = v.get("data").and_then(|d| d.as_array()).unwrap_or(&empty);
    let mut messages = Vec::with_capacity(data.len());
    for m in data {
        let id = m
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let kapso = m.get("kapso");
        let getk = |field: &str| {
            kapso
                .and_then(|k| k.get(field))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        };
        messages.push(KapsoRawMessage {
            id,
            conversation_id: getk("whatsapp_conversation_id").unwrap_or_default(),
            direction: getk("direction"),
            contact_name: getk("contact_name"),
            phone_number: getk("phone_number"),
            timestamp_secs: m.get("timestamp").map(parse_timestamp).unwrap_or(0),
        });
    }
    let next_cursor = v
        .get("paging")
        .and_then(|p| p.get("next"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(KapsoMessagesPage {
        messages,
        next_cursor,
    })
}

/// Extract a message body from a `messages get` response (root or under
/// `data`); `None`/empty → no distillable text.
fn parse_message_body(bytes: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let m = v.get("data").unwrap_or(&v);
    let body = m
        .get("kapso")
        .and_then(|k| k.get("content"))
        .and_then(|x| x.as_str())
        .or_else(|| {
            m.get("text")
                .and_then(|t| t.get("body"))
                .and_then(|x| x.as_str())
        });
    body.map(str::to_string).filter(|b| !b.trim().is_empty())
}

fn nonempty_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn phone_display(value: Option<&str>) -> Option<String> {
    let raw = nonempty_trimmed(value)?;
    let digits = raw.trim_start_matches('+');
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("+{digits}"))
    } else {
        Some(raw)
    }
}

fn contact_display_name(name: Option<&str>, phone: Option<&str>) -> Option<String> {
    nonempty_trimmed(name).or_else(|| phone_display(phone))
}

/// Group windowed messages by conversation → neutral batch. `floor_secs`
/// drops anything older; `max_threads` caps distinct conversations
/// (insertion order = newest-first from the API).
fn map_messages_to_batch(
    messages: &[KapsoRawMessage],
    account: &ChannelAccount,
    floor_secs: i64,
    suppress: bool,
    max_threads: usize,
    mode: &'static str,
) -> IngestBatch {
    let observed_at = now_millis();
    let mut order: Vec<String> = Vec::new();
    let mut by_conv: HashMap<String, Vec<&KapsoRawMessage>> = HashMap::new();
    let mut capped = false;
    for m in messages {
        if m.timestamp_secs < floor_secs || m.conversation_id.is_empty() {
            continue;
        }
        if !by_conv.contains_key(&m.conversation_id) {
            if order.len() >= max_threads {
                capped = true;
                continue;
            }
            order.push(m.conversation_id.clone());
        }
        by_conv
            .entry(m.conversation_id.clone())
            .or_default()
            .push(m);
    }

    let mut batch = IngestBatch {
        mode,
        ..IngestBatch::default()
    };
    for conv in &order {
        let msgs = &by_conv[conv];
        let name = msgs.iter().find_map(|m| {
            contact_display_name(m.contact_name.as_deref(), m.phone_number.as_deref())
        });
        let thread_sensitive = suppress && is_sensitive(name.as_deref(), None);

        let mut max_ts_raw: i64 = 0;
        let mut latest_millis: i64 = 0;
        let mut latest_from_name: Option<String> = None;
        let mut latest_from_address: Option<String> = None;
        let mut rows: Vec<MailMessageMeta> = Vec::with_capacity(msgs.len());

        for m in msgs {
            let ts_millis = m.timestamp_secs.saturating_mul(1000);
            max_ts_raw = max_ts_raw.max(m.timestamp_secs);
            if ts_millis >= latest_millis {
                latest_millis = ts_millis;
                latest_from_name =
                    contact_display_name(m.contact_name.as_deref(), m.phone_number.as_deref());
                latest_from_address = m.phone_number.clone();
            }
            let direction = match m.direction.as_deref() {
                Some("outbound") => Some(MessageDirection::Outbound),
                Some("inbound") => Some(MessageDirection::Inbound),
                _ => None,
            };
            rows.push(MailMessageMeta {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: KAPSO_PROVIDER.to_string(),
                account_alias: account.account_alias.clone(),
                account_email: None,
                thread_id: conv.clone(),
                message_id: m.id.clone(),
                provider_cursor: Some(m.timestamp_secs.to_string()),
                label_ids: Vec::new(),
                subject: None,
                from_name: contact_display_name(
                    m.contact_name.as_deref(),
                    m.phone_number.as_deref(),
                ),
                from_address: m.phone_number.clone(),
                to_domains: Vec::new(),
                cc_domains: Vec::new(),
                internal_date: ts_millis,
                observed_at,
                direction,
                summary: None,
                intent: None,
                needs_reply_hint: false,
                follow_up_hint: None,
                distill_brief: None,
                distill_contract_version: None,
                distilled_at: None,
                distill_revision: None,
                distill_state: if thread_sensitive {
                    DistillState::Suppressed
                } else {
                    DistillState::Pending
                },
                distill_attempts: 0,
                sensitive_suppressed: thread_sensitive,
                origin: MailRecordOrigin::MetadataSync,
            });
        }
        if rows.is_empty() {
            continue;
        }

        batch.threads.push(MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: KAPSO_PROVIDER.to_string(),
            account_alias: account.account_alias.clone(),
            account_email: None,
            thread_id: conv.clone(),
            lane: account.lane,
            subject: Some(if thread_sensitive {
                REDACTED_SUBJECT_PLACEHOLDER.to_string()
            } else {
                name.unwrap_or_else(|| conv.clone())
            }),
            latest_summary: None,
            latest_from_name,
            latest_from_address,
            recipient_domains: Vec::new(),
            label_ids: Vec::new(),
            message_count: rows.len() as i64,
            last_message_at: Some(latest_millis),
            provider_cursor: Some(max_ts_raw.to_string()),
            sensitive_suppressed: thread_sensitive,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        });
        batch.max_internal_date = Some(batch.max_internal_date.unwrap_or(0).max(latest_millis));
        batch.next_provider_cursor = Some(
            batch
                .next_provider_cursor
                .as_deref()
                .and_then(|c| c.parse::<i64>().ok())
                .unwrap_or(0)
                .max(max_ts_raw)
                .to_string(),
        );
        batch.messages.extend(rows);
    }
    if capped {
        batch.max_internal_date = None;
        batch.next_provider_cursor = None;
    }
    batch
}

// ---------------------------------------------------------------------------
// Ingestor + content fetcher
// ---------------------------------------------------------------------------

/// Kapso [`ChannelIngestor`] — Presto's own WhatsApp number, envoy lane.
pub struct WhatsappKapsoIngestor;

#[async_trait]
impl ChannelIngestor for WhatsappKapsoIngestor {
    fn provider(&self) -> &'static str {
        KAPSO_PROVIDER
    }

    /// Credentials and the local CLI must both be present. This avoids a
    /// guaranteed spawn failure on every sync tick while still recovering
    /// automatically when the CLI is installed later.
    fn account_ready(&self, ctx: &IngestContext, _account: &ChannelAccount) -> bool {
        kapso_credentials().is_some() && kapso_binary_available(ctx)
    }

    fn unavailable_reason(&self, ctx: &IngestContext, _account: &ChannelAccount) -> Option<String> {
        (kapso_credentials().is_some() && !kapso_binary_available(ctx)).then(|| {
            format!(
                "Kapso CLI `{}` is not installed or executable; install it or disable this account",
                kapso_binary()
            )
        })
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let (api_key, phone_number_id) =
            kapso_credentials().ok_or_else(|| anyhow!("kapso credentials unavailable"))?;
        let client = KapsoClient {
            api_key,
            phone_number_id,
            scope_paths: Some(ctx.scope_paths.clone()),
        };

        let cursor_secs = watermark
            .and_then(|w| w.provider_cursor.as_deref())
            .and_then(|c| c.parse::<i64>().ok());
        let min_secs = ctx.min_internal_date / 1000;
        let (floor_secs, mode) = match cursor_secs {
            Some(secs) => (secs.max(min_secs), "incremental"),
            None => (min_secs, "backfill"),
        };

        // Page newest-first, stopping once a page is entirely below the
        // floor, the thread cap is met, or the page budget is spent.
        let mut collected: Vec<KapsoRawMessage> = Vec::new();
        let mut after: Option<String> = None;
        let mut distinct: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut capped = false;
        for page_index in 0..MAX_PAGES {
            let page = client
                .list_messages(DEFAULT_PER_PAGE, after.as_deref())
                .await?;
            if page.messages.is_empty() {
                break;
            }
            let page_max_ts = page
                .messages
                .iter()
                .map(|m| m.timestamp_secs)
                .max()
                .unwrap_or(0);
            for m in &page.messages {
                if m.timestamp_secs >= floor_secs && !m.conversation_id.is_empty() {
                    distinct.insert(m.conversation_id.clone());
                }
            }
            let has_next = page.next_cursor.is_some();
            collected.extend(page.messages);
            // Entire page older than the floor, or enough conversations, or
            // no further pages → stop.
            if page_max_ts < floor_secs {
                break;
            }
            let hit_thread_cap = ctx.max_threads == 0
                || distinct.len() > ctx.max_threads
                || (distinct.len() >= ctx.max_threads && has_next);
            let hit_page_cap = page_index + 1 == MAX_PAGES && has_next;
            if hit_thread_cap || hit_page_cap {
                capped = true;
                break;
            }
            if !has_next {
                break;
            }
            after = page.next_cursor;
        }

        let mut batch = map_messages_to_batch(
            &collected,
            account,
            floor_secs,
            ctx.suppress_sensitive,
            ctx.max_threads,
            mode,
        );
        if capped {
            batch.max_internal_date = None;
            batch.next_provider_cursor = cursor_secs.map(|s| s.to_string());
        } else if batch.next_provider_cursor.is_none() {
            // Empty pass keeps the incoming cursor (watermark never regresses).
            batch.next_provider_cursor = cursor_secs.map(|s| s.to_string());
        }
        Ok(batch)
    }
}

/// Kapso content path: fetch the message body by id at distill time
/// (in-memory only) → chunk. Bodies are clean text (no MIME).
pub struct KapsoContentFetcher;

#[async_trait]
impl ContentFetcher for KapsoContentFetcher {
    fn provider(&self) -> &'static str {
        KAPSO_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let (api_key, phone_number_id) =
            kapso_credentials().ok_or_else(|| anyhow!("kapso credentials unavailable"))?;
        let client = KapsoClient {
            api_key,
            phone_number_id,
            scope_paths: Some(ctx.scope_paths.clone()),
        };
        let Some(text) = client.get_message_body(&message.message_id).await? else {
            return Ok(DistillContent::default());
        };
        let prepared = prepare_for_distill(&text, ctx.chunk_chars, ctx.max_chunks);
        Ok(DistillContent {
            chunks: prepared.chunks,
            truncated: prepared.truncated,
            had_html: false,
            attachment_count: 0,
        })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::ChannelLane;
    use super::*;

    fn acct() -> ChannelAccount {
        ChannelAccount {
            provider: KAPSO_PROVIDER.to_string(),
            account_alias: "presto".to_string(),
            lane: ChannelLane::Envoy,
            enabled: true,
        }
    }

    const PAGE: &str = r#"{
      "data": [
        { "id": "wamid.A", "timestamp": "1782353800", "type": "text",
          "text": { "body": "hello presto" },
          "kapso": { "direction": "inbound", "contact_name": "Alice",
                     "phone_number": "15551230000", "whatsapp_conversation_id": "conv-1",
                     "has_media": false, "content": "hello presto" } },
        { "id": "wamid.B", "timestamp": "1782353900", "type": "text",
          "text": { "body": "on it" },
          "kapso": { "direction": "outbound", "contact_name": "Alice",
                     "phone_number": "15551230000", "whatsapp_conversation_id": "conv-1",
                     "has_media": false, "content": "on it" } }
      ],
      "paging": { "next": "CURSOR2", "previous": null }
    }"#;

    #[test]
    fn parses_page_shape_and_cursor() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.next_cursor.as_deref(), Some("CURSOR2"));
        assert_eq!(page.messages[0].id, "wamid.A");
        assert_eq!(page.messages[0].conversation_id, "conv-1");
        assert_eq!(page.messages[0].timestamp_secs, 1782353800);
        assert_eq!(page.messages[0].direction.as_deref(), Some("inbound"));
    }

    #[cfg(unix)]
    #[test]
    fn executable_preflight_distinguishes_missing_and_non_executable_cli() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let binary = directory.path().join("kapso");
        std::fs::write(&binary, b"#!/bin/sh\n").expect("fixture binary");
        let search_path = std::env::join_paths([directory.path()]).expect("search path");

        assert!(!executable_on_path("missing-kapso", Some(&search_path)));
        assert!(!executable_on_path("kapso", Some(&search_path)));
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&binary, permissions).unwrap();
        assert!(executable_on_path("kapso", Some(&search_path)));
        assert!(executable_on_path(
            binary.to_str().expect("utf8 fixture path"),
            None
        ));
    }

    #[test]
    fn shared_kapso_binary_path_keeps_workspace_homes_isolated() {
        let directory = tempfile::tempdir().expect("tempdir");
        let shared_bin = directory.path().join("skillshub/node_modules/.bin");
        std::fs::create_dir_all(&shared_bin).expect("shared bin");

        let scope = |principal: &str, workspace: &str| CapabilityScopePaths {
            principal: principal.to_owned(),
            workspace: workspace.to_owned(),
            capabilities_root: directory.path().join(principal).join(workspace),
            bots_root: directory
                .path()
                .join(principal)
                .join(workspace)
                .join("bots"),
            auth_root: directory
                .path()
                .join(principal)
                .join(workspace)
                .join("auth"),
            workdirs_root: directory
                .path()
                .join(principal)
                .join(workspace)
                .join("workdirs"),
            home_root: directory
                .path()
                .join(principal)
                .join(workspace)
                .join("workdirs/home"),
            node_modules_bin: shared_bin.clone(),
            node_bin: directory.path().join("skillshub/.node/bin"),
            venv_bin: directory.path().join("skillshub/.venv/bin"),
        };
        let first = scope("owner-a", "work-a");
        let second = scope("owner-b", "work-b");
        let (first_path, first_home) = scoped_kapso_environment(&first, "/usr/bin:/bin");
        let (second_path, second_home) = scoped_kapso_environment(&second, "/usr/bin:/bin");

        assert_eq!(first_path, second_path, "immutable CLI path is shared");
        assert!(first_path
            .as_deref()
            .is_some_and(|path| path.starts_with(shared_bin.to_string_lossy().as_ref())));
        assert_ne!(first_home, second_home, "writable CLI homes are scoped");
        assert_eq!(first_home, first.home_root);
        assert_eq!(second_home, second.home_root);
    }

    #[test]
    fn parses_null_cursor_as_none() {
        let json = br#"{"data":[],"paging":{"next":null}}"#;
        let page = parse_messages_page(json).unwrap();
        assert!(page.messages.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn timestamp_tolerant_of_int_and_iso() {
        assert_eq!(
            parse_timestamp(&serde_json::json!("1782353800")),
            1782353800
        );
        assert_eq!(
            parse_timestamp(&serde_json::json!(1782353800i64)),
            1782353800
        );
        assert_eq!(
            parse_timestamp(&serde_json::json!("2026-07-05T00:00:00Z")),
            1783209600
        );
        assert_eq!(parse_timestamp(&serde_json::json!("garbage")), 0);
    }

    #[test]
    fn message_body_from_kapso_content_or_text() {
        let a = br#"{"data":{"kapso":{"content":"body via kapso"},"text":{"body":"ignored"}}}"#;
        assert_eq!(parse_message_body(a).as_deref(), Some("body via kapso"));
        let b = br#"{"text":{"body":"body via text"}}"#;
        assert_eq!(parse_message_body(b).as_deref(), Some("body via text"));
        let c = br#"{"kapso":{"content":"   "}}"#;
        assert_eq!(parse_message_body(c), None);
    }

    #[test]
    fn map_groups_by_conversation_and_maps_direction() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        let batch = map_messages_to_batch(&page.messages, &acct(), 0, true, 100, "backfill");
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.threads[0].thread_id, "conv-1");
        assert_eq!(batch.threads[0].subject.as_deref(), Some("Alice"));
        assert_eq!(batch.threads[0].lane, ChannelLane::Envoy);
        let dirs: Vec<_> = batch.messages.iter().map(|m| m.direction).collect();
        assert!(dirs.contains(&Some(MessageDirection::Inbound)));
        assert!(dirs.contains(&Some(MessageDirection::Outbound)));
        assert!(batch
            .messages
            .iter()
            .all(|m| m.to_domains.is_empty() && m.subject.is_none()));
        // Cursor = max timestamp secs.
        assert_eq!(batch.next_provider_cursor.as_deref(), Some("1782353900"));
        assert_eq!(batch.max_internal_date, Some(1782353900_000));
    }

    #[test]
    fn floor_excludes_old_messages() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        // Floor between the two messages.
        let batch = map_messages_to_batch(
            &page.messages,
            &acct(),
            1782353850,
            true,
            100,
            "incremental",
        );
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(batch.messages[0].message_id, "wamid.B");
    }

    #[test]
    fn thread_cap_limits_conversations() {
        let json = r#"{"data":[
          {"id":"a","timestamp":"100","kapso":{"direction":"inbound","whatsapp_conversation_id":"c1"}},
          {"id":"b","timestamp":"101","kapso":{"direction":"inbound","whatsapp_conversation_id":"c2"}},
          {"id":"c","timestamp":"102","kapso":{"direction":"inbound","whatsapp_conversation_id":"c3"}}
        ],"paging":{"next":null}}"#;
        let page = parse_messages_page(json.as_bytes()).unwrap();
        let batch = map_messages_to_batch(&page.messages, &acct(), 0, true, 2, "backfill");
        assert_eq!(batch.threads.len(), 2);
    }

    #[test]
    fn sensitive_contact_name_redacts_and_suppresses() {
        let json = r#"{"data":[
          {"id":"x","timestamp":"100","kapso":{"direction":"inbound","contact_name":"OTP verification code","whatsapp_conversation_id":"cs"}}
        ],"paging":{"next":null}}"#;
        let page = parse_messages_page(json.as_bytes()).unwrap();
        let batch = map_messages_to_batch(&page.messages, &acct(), 0, true, 100, "backfill");
        assert_eq!(
            batch.threads[0].subject.as_deref(),
            Some(REDACTED_SUBJECT_PLACEHOLDER)
        );
        assert!(batch.threads[0].sensitive_suppressed);
        assert!(batch
            .messages
            .iter()
            .all(|m| m.distill_state == DistillState::Suppressed));
    }

    #[test]
    fn suppression_off_keeps_pending() {
        let json = r#"{"data":[
          {"id":"x","timestamp":"100","kapso":{"direction":"inbound","contact_name":"OTP verification code","whatsapp_conversation_id":"cs"}}
        ],"paging":{"next":null}}"#;
        let page = parse_messages_page(json.as_bytes()).unwrap();
        let batch = map_messages_to_batch(&page.messages, &acct(), 0, false, 100, "backfill");
        assert!(!batch.threads[0].sensitive_suppressed);
        assert!(batch
            .messages
            .iter()
            .all(|m| m.distill_state == DistillState::Pending));
    }
}
