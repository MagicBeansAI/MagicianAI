//! AgentMail ingestor — Magican's OWN email inbox (`MAGICIAN_AGENT_EMAIL`,
//! the ENVOY lane), via the `agentmail` CLI.
//!
//! AgentMail is an email API for agents; this is the second email-shaped
//! channel (after Gmail), so it reuses the Gmail address helpers
//! (`parse_single_address`, `recipient_domains`) — To reduces to DOMAINS at
//! ingest, bodies never persist. Unlike Gmail (owner accounts via gws
//! profiles), AgentMail is a single agent-owned inbox authenticated by an
//! inbox-scoped key. It only READS; sending is the separate capped
//! `agentmail-send` skill and is never touched here.
//!
//! ## CLI shapes (docs.agentmail.to; `--format json` is a GLOBAL flag that
//! must PRECEDE the subcommand in the urfave/cli parser)
//!
//! `agentmail --format json inboxes:messages list --inbox-id <email>
//! --limit N [--page-token <cursor>]` →
//! ```text
//! { "count": N, "limit": N, "next_page_token": "<cursor>|null",
//!   "messages": [ {
//!     "message_id": "…", "thread_id": "…", "subject": "…",
//!     "from": "Name <user@domain>", "to": ["a@x", …],
//!     "timestamp": "2026-07-05T09:30:00Z",   // ISO 8601, newest-first
//!     "labels": ["…"], "preview": "…"        // preview = snippet, NEVER stored
//!   } ] }
//! ```
//! `agentmail --format json inboxes:messages get --inbox-id <email>
//! --message-id <id>` → one message with `text` / `extracted_text` /
//! `html` bodies (CONTENT — fetched ONLY at distill time, never persisted).
//!
//! ## Mapping
//!
//! - `message_id`/`thread_id`/`subject`/`labels` map directly; `from` →
//!   `from_name`/`from_address` (`parse_single_address`); `to` → domains
//!   only (`recipient_domains`); `timestamp` (ISO 8601) → `internal_date`
//!   millis; direction = Outbound iff `from` is the inbox address, else
//!   Inbound.
//! - The message BODY (`text`/`extracted_text`) is fetched ONLY at distill
//!   time by [`AgentMailContentFetcher`] and never persisted.
//!
//! ## Windowing
//!
//! The list is newest-first with `next_page_token`. The watermark cursor is
//! the max `internal_date` (millis) seen; the worker pages until a page is
//! entirely below the floor, the thread cap is hit, or the page budget is
//! spent, filtering in Rust (`internal_date >= floor`). The store dedups by
//! message id, so a boundary re-read is harmless.

use std::collections::HashMap;
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
use super::gws_client::{parse_single_address, recipient_domains};
use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::registry::ChannelAccount;
use super::sensitivity::is_sensitive;
use super::types::{
    DistillState, MailMessageMeta, MailRecordOrigin, MailThreadRecord, MessageDirection,
    SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};

/// Provider key for Presto's AgentMail inbox (the envoy email lane).
pub const AGENTMAIL_PROVIDER: &str = "agentmail";
/// The AgentMail account alias — the single agent-owned inbox. Matches the
/// `agentmail_accounts` entry name in `operator-config.yaml` (`work`).
pub const AGENTMAIL_ACCOUNT_ALIAS: &str = "work";

const AGENTMAIL_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 100;
/// Page budget per pass — a very active inbox can't wedge a tick.
const MAX_PAGES: usize = 20;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn agentmail_binary() -> String {
    std::env::var("AGENTMAIL_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "agentmail".to_string())
}

/// Env-var slug for an account alias: upper-case, non-alphanumerics → `_`,
/// trailing `_` stripped (mirrors the `agentmail-run.sh` wrapper).
fn alias_slug(alias: &str) -> String {
    let mut s: String = alias
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    while s.ends_with('_') {
        s.pop();
    }
    s
}

fn operator_config() -> Option<serde_yaml::Value> {
    let raw = std::fs::read_to_string(runtime_config_path(
        "operator-config.yaml",
        "skillshub/operator-config.yaml",
    ))
    .ok()?;
    serde_yaml::from_str(&raw).ok()
}

/// Resolve the inbox-scoped key for an account: `AGENT_MAIL_KEY_<SLUG>`
/// (env or `secrets:`), falling back to the bare `AGENT_MAIL_KEY` for the
/// default `work` account. Mirrors the skill wrapper's resolution.
fn resolve_key(alias: &str, cfg: Option<&serde_yaml::Value>) -> Option<String> {
    let slug = alias_slug(alias);
    let named_env = format!("AGENT_MAIL_KEY_{slug}");
    if let Ok(k) = std::env::var(&named_env) {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    if alias == AGENTMAIL_ACCOUNT_ALIAS {
        if let Ok(k) = std::env::var("AGENT_MAIL_KEY") {
            if !k.trim().is_empty() {
                return Some(k);
            }
        }
    }
    let secrets = cfg.and_then(|c| c.get("secrets"));
    if let Some(k) = secrets
        .and_then(|s| s.get(&named_env))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(k.to_string());
    }
    if alias == AGENTMAIL_ACCOUNT_ALIAS {
        return secrets
            .and_then(|s| s.get("AGENT_MAIL_KEY"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
    }
    None
}

/// Resolve the inbox email for an account: an `AGENTMAIL_ACCOUNT_<SLUG>_EMAIL`
/// env override, the matching `agentmail_accounts` entry's `email`, else the
/// identity layer's `MAGICIAN_AGENT_EMAIL`. There is deliberately no hardcoded
/// fallback — an unconfigured inbox must fail loudly, never point at someone
/// else's mailbox.
fn resolve_inbox(alias: &str, cfg: Option<&serde_yaml::Value>) -> Option<String> {
    let slug = alias_slug(alias);
    if let Ok(email) = std::env::var(format!("AGENTMAIL_ACCOUNT_{slug}_EMAIL")) {
        if !email.trim().is_empty() {
            return Some(email);
        }
    }
    let from_cfg = cfg
        .and_then(|c| c.get("agentmail_accounts"))
        .and_then(|a| a.as_sequence())
        .and_then(|seq| {
            seq.iter()
                .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(alias))
                .or_else(|| seq.first())
        })
        .and_then(|e| e.get("email"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    from_cfg.or_else(|| {
        std::env::var("MAGICIAN_AGENT_EMAIL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// `(api_key, inbox_email)` for an account, or `None` when the key is absent
/// (the account then silently drops out, like an unauthenticated gmail
/// alias) or when no inbox email is configured (that case warns loudly —
/// a keyed account with no address is an operator setup error).
fn agentmail_credentials(alias: &str) -> Option<(String, String)> {
    let cfg = operator_config();
    let key = resolve_key(alias, cfg.as_ref())?;
    let Some(inbox) = resolve_inbox(alias, cfg.as_ref()) else {
        tracing::warn!(
            "[AGENTMAIL] account '{alias}' has an API key but no inbox email — set \
             agentmail_accounts[].email in operator-config.yaml or MAGICIAN_AGENT_EMAIL \
             (make setup-identity); skipping the account"
        );
        return None;
    };
    Some((key, inbox))
}

/// The configured inbox email for display (discovery), independent of the
/// key. `None` when the operator has not configured one — display surfaces
/// show the gap instead of a made-up address.
pub fn agentmail_inbox_email() -> Option<String> {
    resolve_inbox(AGENTMAIL_ACCOUNT_ALIAS, operator_config().as_ref())
}

// ---------------------------------------------------------------------------
// CLI client (adapter interior)
// ---------------------------------------------------------------------------

struct AgentMailClient {
    api_key: String,
    inbox: String,
    scope_paths: Option<CapabilityScopePaths>,
}

impl AgentMailClient {
    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        // Augment PATH so a bare `agentmail` resolves (nvm/global install),
        // fail-safe to the inherited PATH. Computed first so the binary is
        // resolved against the PATH the child will see and the spawn stays on
        // `posix_spawn` (see `runtime_core::process`).
        let child_path = self.scope_paths.as_ref().and_then(|sp| {
            let parent = std::env::var("PATH").unwrap_or_default();
            sp.subprocess_bin_path(None, &parent)
        });
        let mut cmd = Command::new(runtime_core::process::resolve_program_str(
            &agentmail_binary(),
            child_path.as_deref(),
        ));
        // Global `--format json` MUST precede the subcommand (urfave/cli).
        cmd.arg("--format").arg("json");
        for arg in args {
            cmd.arg(arg);
        }
        cmd.env("AGENTMAIL_API_KEY", &self.api_key)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(p) = child_path.as_deref() {
            cmd.env("PATH", p);
        }
        let output = tokio::time::timeout(AGENTMAIL_TIMEOUT, cmd.output())
            .await
            .map_err(|_| anyhow!("agentmail timed out"))?
            .map_err(|e| anyhow!("spawn agentmail: {e}"))?;
        if !output.status.success() {
            let detail: String = String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(300)
                .collect();
            anyhow::bail!("agentmail failed: {detail}");
        }
        Ok(output.stdout)
    }

    async fn list_messages(&self, limit: u32, page_token: Option<&str>) -> Result<AgentMailPage> {
        let lim = limit.to_string();
        let inbox = self.inbox.clone();
        let mut args = vec![
            "inboxes:messages",
            "list",
            "--inbox-id",
            inbox.as_str(),
            "--limit",
            lim.as_str(),
        ];
        if let Some(cursor) = page_token {
            args.push("--page-token");
            args.push(cursor);
        }
        let out = self.run(&args).await?;
        parse_messages_page(&out)
    }

    async fn get_message_body(&self, id: &str) -> Result<Option<String>> {
        let inbox = self.inbox.clone();
        let out = self
            .run(&[
                "inboxes:messages",
                "get",
                "--inbox-id",
                inbox.as_str(),
                "--message-id",
                id,
            ])
            .await?;
        Ok(parse_message_body(&out))
    }
}

// ---------------------------------------------------------------------------
// Pure parsing + mapping (unit-tested; no live CLI)
// ---------------------------------------------------------------------------

/// One AgentMail message row (no body — fetched at distill time).
#[derive(Debug, Clone)]
struct AgentMailRawMessage {
    message_id: String,
    thread_id: String,
    subject: Option<String>,
    from: Option<String>,
    to: Vec<String>,
    labels: Vec<String>,
    internal_date: i64,
}

struct AgentMailPage {
    messages: Vec<AgentMailRawMessage>,
    next_page_token: Option<String>,
}

/// Parse an AgentMail `timestamp`: ISO 8601 (the documented shape), tolerant
/// of epoch millis (int) as a fallback. Returns epoch MILLIS.
fn parse_timestamp_millis(value: &Value) -> i64 {
    if let Some(s) = value.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.timestamp_millis();
        }
        if let Ok(n) = s.parse::<i64>() {
            return n;
        }
    }
    if let Some(n) = value.as_i64() {
        return n;
    }
    0
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_messages_page(bytes: &[u8]) -> Result<AgentMailPage> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow!("parsing agentmail messages: {e}"))?;
    let empty = Vec::new();
    let data = v
        .get("messages")
        .and_then(|d| d.as_array())
        .unwrap_or(&empty);
    let mut messages = Vec::with_capacity(data.len());
    for m in data {
        let message_id = m
            .get("message_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if message_id.is_empty() {
            continue;
        }
        let gets = |field: &str| {
            m.get(field)
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        messages.push(AgentMailRawMessage {
            message_id,
            thread_id: gets("thread_id").unwrap_or_default(),
            subject: gets("subject"),
            from: gets("from"),
            to: string_array(m.get("to")),
            labels: string_array(m.get("labels")),
            internal_date: m.get("timestamp").map(parse_timestamp_millis).unwrap_or(0),
        });
    }
    let next_page_token = v
        .get("next_page_token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(AgentMailPage {
        messages,
        next_page_token,
    })
}

/// Extract a message body from a `messages get` response — prefer the
/// cleaned `extracted_text`, then `text`. HTML-only messages fall back to
/// `text`'s absence → None (metadata-only). `None`/empty → no distillable
/// text.
fn parse_message_body(bytes: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let m = v.get("data").unwrap_or(&v);
    for field in ["extracted_text", "text"] {
        if let Some(body) = m
            .get(field)
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            return Some(body.to_string());
        }
    }
    None
}

/// Group windowed messages by thread → neutral batch. `floor_millis` drops
/// anything older; `max_threads` caps distinct threads (insertion order =
/// newest-first from the API). `inbox` decides direction (from == inbox →
/// Outbound).
fn map_messages_to_batch(
    messages: &[AgentMailRawMessage],
    account: &ChannelAccount,
    inbox: &str,
    floor_millis: i64,
    suppress: bool,
    max_threads: usize,
    mode: &'static str,
) -> IngestBatch {
    let observed_at = now_millis();
    let inbox_lc = inbox.to_ascii_lowercase();
    let mut order: Vec<String> = Vec::new();
    let mut by_thread: HashMap<String, Vec<&AgentMailRawMessage>> = HashMap::new();
    let mut capped = false;
    for m in messages {
        if m.internal_date < floor_millis || m.thread_id.is_empty() {
            continue;
        }
        if !by_thread.contains_key(&m.thread_id) {
            if order.len() >= max_threads {
                capped = true;
                continue;
            }
            order.push(m.thread_id.clone());
        }
        by_thread.entry(m.thread_id.clone()).or_default().push(m);
    }

    let mut batch = IngestBatch {
        mode,
        ..IngestBatch::default()
    };
    for thread_id in &order {
        let msgs = &by_thread[thread_id];
        // Latest message drives the thread subject/participants.
        let latest = msgs.iter().max_by_key(|m| m.internal_date);
        let subject = latest.and_then(|m| m.subject.clone());

        let mut max_millis: i64 = 0;
        let mut latest_from_name: Option<String> = None;
        let mut latest_from_address: Option<String> = None;
        let mut recipient_domains_all: Vec<String> = Vec::new();
        let mut rows: Vec<MailMessageMeta> = Vec::with_capacity(msgs.len());
        let mut thread_sensitive = false;

        for m in msgs {
            let (from_name, from_address) = match m.from.as_deref() {
                Some(v) => parse_single_address(v),
                None => (None, None),
            };
            let to_domains = recipient_domains(&m.to.join(", "));
            for d in &to_domains {
                if !recipient_domains_all.contains(d) {
                    recipient_domains_all.push(d.clone());
                }
            }
            let direction = match from_address.as_deref() {
                Some(addr) if addr.eq_ignore_ascii_case(&inbox_lc) => {
                    Some(MessageDirection::Outbound)
                },
                Some(_) => Some(MessageDirection::Inbound),
                None => None,
            };
            if m.internal_date >= max_millis {
                max_millis = m.internal_date;
                latest_from_name = from_name.clone();
                latest_from_address = from_address.clone();
            }
            let msg_sensitive =
                suppress && is_sensitive(m.subject.as_deref(), from_address.as_deref());
            thread_sensitive = thread_sensitive || msg_sensitive;
            rows.push(MailMessageMeta {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: AGENTMAIL_PROVIDER.to_string(),
                account_alias: account.account_alias.clone(),
                account_email: Some(inbox.to_string()),
                thread_id: thread_id.clone(),
                message_id: m.message_id.clone(),
                provider_cursor: Some(m.internal_date.to_string()),
                label_ids: m.labels.clone(),
                subject: if msg_sensitive {
                    None
                } else {
                    m.subject.clone()
                },
                from_name,
                from_address,
                to_domains,
                cc_domains: Vec::new(),
                internal_date: m.internal_date,
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
                distill_state: if msg_sensitive {
                    DistillState::Suppressed
                } else {
                    DistillState::Pending
                },
                distill_attempts: 0,
                sensitive_suppressed: msg_sensitive,
                origin: MailRecordOrigin::MetadataSync,
            });
        }
        if rows.is_empty() {
            continue;
        }

        batch.threads.push(MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: AGENTMAIL_PROVIDER.to_string(),
            account_alias: account.account_alias.clone(),
            account_email: Some(inbox.to_string()),
            thread_id: thread_id.clone(),
            lane: account.lane,
            subject: Some(if thread_sensitive {
                REDACTED_SUBJECT_PLACEHOLDER.to_string()
            } else {
                subject.unwrap_or_else(|| thread_id.clone())
            }),
            latest_summary: None,
            latest_from_name,
            latest_from_address,
            recipient_domains: recipient_domains_all,
            label_ids: latest.map(|m| m.labels.clone()).unwrap_or_default(),
            message_count: rows.len() as i64,
            last_message_at: Some(max_millis),
            provider_cursor: Some(max_millis.to_string()),
            sensitive_suppressed: thread_sensitive,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        });
        batch.max_internal_date = Some(batch.max_internal_date.unwrap_or(0).max(max_millis));
        batch.next_provider_cursor = Some(
            batch
                .next_provider_cursor
                .as_deref()
                .and_then(|c| c.parse::<i64>().ok())
                .unwrap_or(0)
                .max(max_millis)
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

/// AgentMail [`ChannelIngestor`] — Presto's own email inbox, envoy lane.
/// One recent inbound message as automatic verification-code retrieval
/// reads it (secure HITL P6). In memory only; `Debug` never prints the body.
pub struct AgentMailVerificationMessage {
    pub message_id: String,
    pub received_at_ms: i64,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub body: String,
}

impl std::fmt::Debug for AgentMailVerificationMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMailVerificationMessage")
            .field("message_id", &self.message_id)
            .field("received_at_ms", &self.received_at_ms)
            .finish_non_exhaustive()
    }
}

/// The named mailbox's inbound messages received at or after `since_ms`,
/// newest first, at most `limit`, each with its text body — read directly
/// through the provider CLI for a live verification challenge, never through
/// ingest (no model-facing copy is made). Messages the inbox itself sent are
/// skipped.
pub async fn recent_messages_for_verification(
    alias: &str,
    scope_paths: Option<CapabilityScopePaths>,
    since_ms: i64,
    limit: usize,
) -> Result<Vec<AgentMailVerificationMessage>> {
    let Some((api_key, inbox)) = agentmail_credentials(alias) else {
        anyhow::bail!("agentmail account has no credentials");
    };
    let client = AgentMailClient {
        api_key,
        inbox: inbox.clone(),
        scope_paths,
    };
    let page = client
        .list_messages(limit.clamp(1, 20) as u32, None)
        .await?;
    let mut out = Vec::new();
    for raw in select_for_verification(page.messages, &inbox, since_ms, limit) {
        let body = client
            .get_message_body(&raw.message_id)
            .await?
            .unwrap_or_default();
        out.push(AgentMailVerificationMessage {
            message_id: raw.message_id,
            received_at_ms: raw.internal_date,
            from: raw.from,
            subject: raw.subject,
            body,
        });
    }
    Ok(out)
}

/// The rows of one list page a verification read fetches bodies for:
/// received at or after `since_ms` (the provider's timestamp, never the
/// message's own `Date`), not sent by the inbox itself, at most `limit`,
/// newest first.
fn select_for_verification(
    messages: Vec<AgentMailRawMessage>,
    inbox: &str,
    since_ms: i64,
    limit: usize,
) -> Vec<AgentMailRawMessage> {
    let inbox_lower = inbox.to_ascii_lowercase();
    let mut rows: Vec<AgentMailRawMessage> = messages
        .into_iter()
        .filter(|raw| raw.internal_date >= since_ms)
        .filter(|raw| {
            !raw.from
                .as_deref()
                .is_some_and(|from| from.to_ascii_lowercase().contains(&inbox_lower))
        })
        .collect();
    rows.sort_by(|a, b| b.internal_date.cmp(&a.internal_date));
    rows.truncate(limit);
    rows
}

pub struct AgentMailIngestor;

#[async_trait]
impl ChannelIngestor for AgentMailIngestor {
    fn provider(&self) -> &'static str {
        AGENTMAIL_PROVIDER
    }

    /// AgentMail key present (env or operator-config `secrets:`). Absent →
    /// the account silently drops out.
    fn account_ready(&self, _ctx: &IngestContext, account: &ChannelAccount) -> bool {
        agentmail_credentials(&account.account_alias).is_some()
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let (api_key, inbox) = agentmail_credentials(&account.account_alias)
            .ok_or_else(|| anyhow!("agentmail credentials unavailable"))?;
        let client = AgentMailClient {
            api_key,
            inbox: inbox.clone(),
            scope_paths: Some(ctx.scope_paths.clone()),
        };

        let cursor_millis = watermark
            .and_then(|w| w.provider_cursor.as_deref())
            .and_then(|c| c.parse::<i64>().ok());
        let (floor_millis, mode) = match cursor_millis {
            Some(millis) => (millis.max(ctx.min_internal_date), "incremental"),
            None => (ctx.min_internal_date, "backfill"),
        };

        // Page newest-first, stopping once a page is entirely below the
        // floor, the thread cap is met, or the page budget is spent.
        let mut collected: Vec<AgentMailRawMessage> = Vec::new();
        let mut page_token: Option<String> = None;
        let mut distinct: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut capped = false;
        for page_index in 0..MAX_PAGES {
            let page = client
                .list_messages(DEFAULT_LIMIT, page_token.as_deref())
                .await?;
            if page.messages.is_empty() {
                break;
            }
            let page_max = page
                .messages
                .iter()
                .map(|m| m.internal_date)
                .max()
                .unwrap_or(0);
            for m in &page.messages {
                if m.internal_date >= floor_millis && !m.thread_id.is_empty() {
                    distinct.insert(m.thread_id.clone());
                }
            }
            let has_next = page.next_page_token.is_some();
            collected.extend(page.messages);
            if page_max < floor_millis {
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
            page_token = page.next_page_token;
        }

        let mut batch = map_messages_to_batch(
            &collected,
            account,
            &inbox,
            floor_millis,
            ctx.suppress_sensitive,
            ctx.max_threads,
            mode,
        );
        if capped {
            batch.max_internal_date = None;
            batch.next_provider_cursor = cursor_millis.map(|m| m.to_string());
        } else if batch.next_provider_cursor.is_none() {
            // Empty pass keeps the incoming cursor (watermark never regresses).
            batch.next_provider_cursor = cursor_millis.map(|m| m.to_string());
        }
        Ok(batch)
    }
}

/// AgentMail content path: fetch the message body by id at distill time
/// (in-memory only) → chunk. Bodies are the cleaned `extracted_text`/`text`.
pub struct AgentMailContentFetcher;

#[async_trait]
impl ContentFetcher for AgentMailContentFetcher {
    fn provider(&self) -> &'static str {
        AGENTMAIL_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let (api_key, inbox) = agentmail_credentials(&message.account_alias)
            .ok_or_else(|| anyhow!("agentmail credentials unavailable"))?;
        let client = AgentMailClient {
            api_key,
            inbox,
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

    /// Secure HITL P6: what a verification read selects from a recorded
    /// list page — window by provider timestamp, the inbox's own mail
    /// skipped, newest first, bounded.
    #[test]
    fn a_verification_read_selects_recent_inbound_rows_only() {
        let page = parse_messages_page(
            br#"{"count": 4, "messages": [
              {"message_id": "old", "thread_id": "t", "from": "codes@example.test", "timestamp": "2026-07-05T00:00:00Z"},
              {"message_id": "mine", "thread_id": "t", "from": "Assistant <assistant@agentmail.to>", "timestamp": "2026-07-05T00:02:00Z"},
              {"message_id": "fresh", "thread_id": "t", "from": "codes@example.test", "subject": "Your code", "timestamp": "2026-07-05T00:01:30Z"},
              {"message_id": "freshest", "thread_id": "t", "from": "codes@example.test", "timestamp": "2026-07-05T00:03:00Z"}
            ]}"#,
        )
        .unwrap();
        let since = D5_MILLIS + 60_000;
        let selected =
            select_for_verification(page.messages.clone(), "assistant@agentmail.to", since, 10);
        let ids: Vec<&str> = selected.iter().map(|m| m.message_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["freshest", "fresh"],
            "before the window and the inbox's own mail are out; newest first"
        );
        assert_eq!(selected[1].subject.as_deref(), Some("Your code"));
        assert_eq!(selected[1].internal_date, D5_MILLIS + 90_000);
        assert_eq!(
            select_for_verification(page.messages, "assistant@agentmail.to", since, 1).len(),
            1
        );
        let message = AgentMailVerificationMessage {
            message_id: "fresh".into(),
            received_at_ms: since,
            from: Some("codes@example.test".into()),
            subject: Some("Your code 482913".into()),
            body: "Your verification code is 482913".into(),
        };
        let printed = format!("{message:?}");
        assert!(!printed.contains("482913"), "{printed}");
    }

    fn acct() -> ChannelAccount {
        ChannelAccount {
            provider: AGENTMAIL_PROVIDER.to_string(),
            account_alias: AGENTMAIL_ACCOUNT_ALIAS.to_string(),
            lane: ChannelLane::Envoy,
            enabled: true,
        }
    }

    // D5 = 2026-07-05T00:00:00Z in millis.
    const D5_MILLIS: i64 = 1_783_209_600_000;

    const PAGE: &str = r#"{
      "count": 2, "limit": 100, "next_page_token": "CUR2",
      "messages": [
        { "message_id": "m1", "thread_id": "t1", "subject": "Invoice #42",
          "from": "Alice <alice@acme.example>", "to": ["agent@example.com"],
          "timestamp": "2026-07-05T09:30:00Z", "labels": ["inbox"], "preview": "hi" },
        { "message_id": "m2", "thread_id": "t1", "subject": "Re: Invoice #42",
          "from": "agent@example.com", "to": ["alice@acme.example", "bob@beta.example"],
          "timestamp": "2026-07-05T10:00:00Z", "labels": ["sent"], "preview": "on it" }
      ]
    }"#;

    #[test]
    fn alias_slug_matches_wrapper_convention() {
        assert_eq!(alias_slug("work"), "WORK");
        assert_eq!(alias_slug("Presto-Ops"), "PRESTO_OPS");
        assert_eq!(alias_slug("a.b_"), "A_B");
    }

    #[test]
    fn parses_page_shape_and_cursor() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.next_page_token.as_deref(), Some("CUR2"));
        assert_eq!(page.messages[0].message_id, "m1");
        assert_eq!(page.messages[0].thread_id, "t1");
        assert_eq!(page.messages[0].subject.as_deref(), Some("Invoice #42"));
        // 2026-07-05T09:30:00Z = D5 + 9h30m.
        assert_eq!(page.messages[0].internal_date, D5_MILLIS + 34_200_000);
        assert_eq!(page.messages[0].to, vec!["agent@example.com"]);
    }

    #[test]
    fn timestamp_tolerant_of_iso_and_epoch() {
        assert_eq!(
            parse_timestamp_millis(&serde_json::json!("2026-07-05T00:00:00Z")),
            D5_MILLIS
        );
        assert_eq!(
            parse_timestamp_millis(&serde_json::json!(1_783_209_600_000i64)),
            D5_MILLIS
        );
        assert_eq!(parse_timestamp_millis(&serde_json::json!("garbage")), 0);
    }

    #[test]
    fn null_cursor_parses_as_none() {
        let json = br#"{"count":0,"messages":[],"next_page_token":null}"#;
        let page = parse_messages_page(json).unwrap();
        assert!(page.messages.is_empty());
        assert!(page.next_page_token.is_none());
    }

    #[test]
    fn message_body_prefers_extracted_text_then_text() {
        let a = br#"{"extracted_text":"clean body","text":"raw body","html":"<p>x</p>"}"#;
        assert_eq!(parse_message_body(a).as_deref(), Some("clean body"));
        let b = br#"{"text":"only text body","html":"<p>x</p>"}"#;
        assert_eq!(parse_message_body(b).as_deref(), Some("only text body"));
        let c = br#"{"html":"<p>only html</p>"}"#;
        assert_eq!(parse_message_body(c), None);
    }

    #[test]
    fn maps_thread_direction_and_domains_only() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        let batch = map_messages_to_batch(
            &page.messages,
            &acct(),
            "agent@example.com",
            0,
            true,
            100,
            "backfill",
        );
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.threads[0].thread_id, "t1");
        assert_eq!(batch.threads[0].lane, ChannelLane::Envoy);
        // Latest message (10:00, from presto) → subject "Re: Invoice #42".
        assert_eq!(batch.threads[0].subject.as_deref(), Some("Re: Invoice #42"));
        // Inbound (from Alice) + Outbound (from the inbox itself).
        let dirs: Vec<_> = batch.messages.iter().map(|m| m.direction).collect();
        assert!(dirs.contains(&Some(MessageDirection::Inbound)));
        assert!(dirs.contains(&Some(MessageDirection::Outbound)));
        // To reduces to DOMAINS only — no full addresses persisted.
        assert!(batch
            .messages
            .iter()
            .all(|m| !m.to_domains.iter().any(|d| d.contains('@'))));
        let m2 = batch
            .messages
            .iter()
            .find(|m| m.message_id == "m2")
            .unwrap();
        assert_eq!(m2.to_domains, vec!["acme.example", "beta.example"]);
        // Cursor = max internal_date millis (2026-07-05T10:00:00Z = D5 + 10h).
        let expect_cursor = (D5_MILLIS + 36_000_000).to_string();
        assert_eq!(
            batch.next_provider_cursor.as_deref(),
            Some(expect_cursor.as_str())
        );
        assert_eq!(batch.max_internal_date, Some(D5_MILLIS + 36_000_000));
    }

    #[test]
    fn floor_excludes_old_messages() {
        let page = parse_messages_page(PAGE.as_bytes()).unwrap();
        // Floor between the two messages (2026-07-05T09:45:00Z = D5 + 9h45m).
        let batch = map_messages_to_batch(
            &page.messages,
            &acct(),
            "agent@example.com",
            D5_MILLIS + 35_100_000,
            true,
            100,
            "incremental",
        );
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(batch.messages[0].message_id, "m2");
    }

    #[test]
    fn sensitive_subject_redacts_and_suppresses() {
        let json = r#"{"messages":[
          {"message_id":"x","thread_id":"ts","subject":"Your OTP verification code",
           "from":"bank@secure.example","to":["agent@example.com"],
           "timestamp":"2026-07-05T09:30:00Z","labels":["inbox"]}
        ],"next_page_token":null}"#;
        let page = parse_messages_page(json.as_bytes()).unwrap();
        let batch = map_messages_to_batch(
            &page.messages,
            &acct(),
            "agent@example.com",
            0,
            true,
            100,
            "backfill",
        );
        assert_eq!(
            batch.threads[0].subject.as_deref(),
            Some(REDACTED_SUBJECT_PLACEHOLDER)
        );
        assert!(batch.threads[0].sensitive_suppressed);
        assert!(batch
            .messages
            .iter()
            .all(|m| m.distill_state == DistillState::Suppressed && m.subject.is_none()));
    }
}
