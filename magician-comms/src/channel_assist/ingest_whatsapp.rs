//! WhatsApp ingestor (Phase 1b N4) — the user's own WhatsApp lane.
//!
//! Reads the baileys bot's local wu-cli sqlite store; it never touches
//! WhatsApp's network. The bot (`skillshub/bots/whatsapp`, driven by
//! `@ibrahimwithi/wu-cli`) already persists every chat and message to
//! `<scope>/workdirs/home/.wu/wu.db` (seeded by
//! `skillshub/scripts/install_bot_bundles.py::seed_whatsapp_wu_config`,
//! `WU_HOME=<scope>/workdirs/home/.wu`). This ingestor pulls neutral
//! thread/message METADATA from that db and hands raw text to the local
//! distiller ONLY on demand (never stored) via [`WhatsappContentFetcher`].
//!
//! ## wu.db schema (source: `@ibrahimwithi/wu-cli/dist/db/schema.js`,
//! read from source — the live db is never queried during a build)
//!
//! - `messages(id TEXT PK, chat_jid, sender_jid, sender_name, body,
//!   type, is_from_me INTEGER, timestamp INTEGER, …)` — `timestamp` is
//!   the WhatsApp message time (epoch; unit-agnostic here — see below).
//! - `chats(jid TEXT PK, name, type, participant_count, last_message_at,
//!   …)`.
//! - `contacts`, `group_participants` — unused in Phase 1.
//!
//! ## Identity / lane mapping
//!
//! - `chat_jid` → `thread_id` (via [`normalize_jid`], mirroring
//!   `adapter.ts::normalizeJid`: strip device suffix, bare-number →
//!   `…@s.whatsapp.net`).
//! - `messages.id` → `message_id`; `sender_jid`/`sender_name` →
//!   `from_address`/`from_name`; `is_from_me` → direction (always
//!   derivable, unlike gmail).
//! - `chats.name` → thread subject-equivalent.
//! - **Groups**: WhatsApp has no email-style domains, so `recipient_domains`
//!   stays EMPTY (privacy contract holds trivially). Group-ness is flagged
//!   via a synthetic `"group"` label (group JIDs end in `@g.us`);
//!   `participant_count` is NOT persisted (no column) — it can be folded
//!   into the distilled summary later.
//!
//! ## Timestamp units
//!
//! The `timestamp` column is epoch seconds in current wu-cli, but some
//! baileys stores use millis. The watermark cursor is the RAW max
//! `timestamp` value (unit-agnostic: incremental passes compare
//! `timestamp >= cursor` against the same column), and per-row
//! `internal_date` is normalized to millis via [`epoch_to_millis`]. The
//! backfill window is applied in Rust against the normalized millis, so
//! it is correct whichever unit the column holds.
//!
//! ## Concurrency
//!
//! The bot writes wu.db live. We open READ-ONLY with a busy timeout; a
//! locked/busy read surfaces as an account error (the worker's per-account
//! isolation records it and moves on) and never blocks the sync loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags};

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

/// Provider key for the user's own WhatsApp lane.
pub const WHATSAPP_PROVIDER: &str = "whatsapp";

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(3000);
/// Per-chat message pull cap on a first (backfill) pass, so one very
/// active chat can't dominate the batch.
const BACKFILL_PER_CHAT_LIMIT: usize = 200;

/// wu.db path for the scope: `<workdirs>/home/.wu/wu.db` (matches the
/// seed script's `WU_HOME`).
fn wu_db_path(scope_paths: &CapabilityScopePaths) -> PathBuf {
    scope_paths
        .workdirs_root
        .join("home")
        .join(".wu")
        .join("wu.db")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Normalize a raw epoch value (seconds OR millis) to millis. Values
/// below ~1e11 are treated as seconds (1e11 s ≈ year 5138; 1e11 ms ≈
/// 1973) and scaled up.
fn epoch_to_millis(raw: i64) -> i64 {
    if raw.abs() < 100_000_000_000 {
        raw.saturating_mul(1000)
    } else {
        raw
    }
}

/// Rust mirror of `adapter.ts::normalizeJid`: strip a device suffix
/// (`1234:12@s.whatsapp.net` → `1234@s.whatsapp.net`); a bare digit
/// string (optionally `+`-prefixed) → `…@s.whatsapp.net`; anything else
/// passes through.
pub fn normalize_jid(jid: &str) -> String {
    match jid.find('@') {
        None => {
            let stripped = jid.strip_prefix('+').unwrap_or(jid);
            if !stripped.is_empty() && stripped.bytes().all(|b| b.is_ascii_digit()) {
                format!("{stripped}@s.whatsapp.net")
            } else {
                jid.to_string()
            }
        },
        Some(at) => {
            let local = jid[..at].split(':').next().unwrap_or("");
            let domain = &jid[at..];
            format!("{local}{domain}")
        },
    }
}

fn is_group_jid(jid: &str) -> bool {
    jid.ends_with("@g.us")
}

fn nonempty_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn jid_local(jid: &str) -> &str {
    jid.split('@')
        .next()
        .unwrap_or(jid)
        .split(':')
        .next()
        .unwrap_or(jid)
}

fn display_fallback_from_jid(jid: &str) -> Option<String> {
    let normalized = normalize_jid(jid);
    if normalized.ends_with("@s.whatsapp.net") {
        let local = jid_local(&normalized);
        if !local.is_empty() && local.bytes().all(|b| b.is_ascii_digit()) {
            return Some(format!("+{local}"));
        }
    }
    if normalized.ends_with("@lid") {
        return nonempty_trimmed(Some(jid_local(&normalized)));
    }
    None
}

fn clean_display_name(raw: Option<&str>, jid: Option<&str>) -> Option<String> {
    let value = nonempty_trimmed(raw)?;
    let Some(jid) = jid else {
        return Some(value);
    };
    let normalized = normalize_jid(jid);
    let local = jid_local(&normalized);
    let value_digits = value.trim_start_matches('+');
    if value == jid
        || value == normalized
        || value.ends_with("@s.whatsapp.net")
        || value.ends_with("@g.us")
        || (!local.is_empty() && value_digits == local)
    {
        return None;
    }
    Some(value)
}

fn quote_ident(identifier: &str) -> Option<String> {
    if identifier
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        Some(format!("\"{}\"", identifier))
    } else {
        None
    }
}

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let Some(table) = quote_ident(table) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return Vec::new();
    };
    stmt.query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.filter_map(|row| row.ok()).collect())
        .unwrap_or_default()
}

fn find_column(columns: &[String], candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        columns
            .iter()
            .find(|column| column.eq_ignore_ascii_case(candidate))
            .cloned()
    })
}

fn select_table_name_by_jid(
    conn: &Connection,
    table: &str,
    jid_columns: &[&str],
    name_columns: &[&str],
    jid_values: &[&str],
) -> Option<String> {
    let columns = table_columns(conn, table);
    if columns.is_empty() {
        return None;
    }
    let jid_col = find_column(&columns, jid_columns)?;
    let table_sql = quote_ident(table)?;
    let jid_sql = quote_ident(&jid_col)?;
    let normalized_values: Vec<String> = jid_values
        .iter()
        .flat_map(|jid| [jid.to_string(), normalize_jid(jid)])
        .collect();
    for name_col in name_columns
        .iter()
        .filter_map(|candidate| find_column(&columns, &[*candidate]))
    {
        let Some(name_sql) = quote_ident(&name_col) else {
            continue;
        };
        let sql = format!(
            "SELECT CAST({name_sql} AS TEXT) FROM {table_sql} \
             WHERE {jid_sql} = ?1 AND {name_sql} IS NOT NULL \
               AND TRIM(CAST({name_sql} AS TEXT)) <> '' LIMIT 1"
        );
        for jid in &normalized_values {
            let value: Option<String> = conn
                .query_row(&sql, [jid.as_str()], |row| row.get::<_, Option<String>>(0))
                .ok()
                .flatten();
            if let Some(name) = clean_display_name(value.as_deref(), Some(jid)) {
                return Some(name);
            }
        }
    }
    None
}

fn contact_display_name(conn: &Connection, jid: &str) -> Option<String> {
    select_table_name_by_jid(
        conn,
        "contacts",
        &[
            "jid",
            "id",
            "wa_id",
            "waId",
            "phone",
            "phone_number",
            "phoneNumber",
        ],
        &[
            "name",
            "display_name",
            "displayName",
            "push_name",
            "pushName",
            "notify_name",
            "notifyName",
            "notify",
            "verified_name",
            "verifiedName",
            "short_name",
            "shortName",
            "contact_name",
            "contactName",
        ],
        &[jid],
    )
}

fn group_participant_display_name(
    conn: &Connection,
    chat_jid: &str,
    sender_jid: &str,
) -> Option<String> {
    let columns = table_columns(conn, "group_participants");
    if columns.is_empty() {
        return None;
    }
    let participant_col = find_column(
        &columns,
        &[
            "participant_jid",
            "participantJid",
            "user_jid",
            "userJid",
            "contact_jid",
            "contactJid",
            "jid",
            "id",
        ],
    )?;
    let group_col = find_column(
        &columns,
        &[
            "chat_jid",
            "chatJid",
            "group_jid",
            "groupJid",
            "parent_jid",
            "parentJid",
        ],
    );
    let table_sql = quote_ident("group_participants")?;
    let participant_sql = quote_ident(&participant_col)?;
    let participant_values = [sender_jid.to_string(), normalize_jid(sender_jid)];
    let group_values = [chat_jid.to_string(), normalize_jid(chat_jid)];
    for name_col in [
        "name",
        "display_name",
        "displayName",
        "push_name",
        "pushName",
        "notify_name",
        "notifyName",
        "contact_name",
        "contactName",
    ] {
        let Some(name_col) = find_column(&columns, &[name_col]) else {
            continue;
        };
        let Some(name_sql) = quote_ident(&name_col) else {
            continue;
        };
        let base = format!(
            "SELECT CAST({name_sql} AS TEXT) FROM {table_sql} \
             WHERE {participant_sql} = ?1 AND {name_sql} IS NOT NULL \
               AND TRIM(CAST({name_sql} AS TEXT)) <> ''"
        );
        let mut queries = Vec::new();
        if let Some(group_col) = group_col.as_ref().filter(|g| *g != &participant_col) {
            if let Some(group_sql) = quote_ident(group_col) {
                queries.push(format!("{base} AND {group_sql} = ?2 LIMIT 1"));
            }
        }
        queries.push(format!("{base} LIMIT 1"));
        for sql in queries {
            for participant in &participant_values {
                if sql.contains("?2") {
                    for group in &group_values {
                        let value: Option<String> = conn
                            .query_row(&sql, [participant.as_str(), group.as_str()], |row| {
                                row.get::<_, Option<String>>(0)
                            })
                            .ok()
                            .flatten();
                        if let Some(name) = clean_display_name(value.as_deref(), Some(sender_jid)) {
                            return Some(name);
                        }
                    }
                } else {
                    let value: Option<String> = conn
                        .query_row(&sql, [participant.as_str()], |row| {
                            row.get::<_, Option<String>>(0)
                        })
                        .ok()
                        .flatten();
                    if let Some(name) = clean_display_name(value.as_deref(), Some(sender_jid)) {
                        return Some(name);
                    }
                }
            }
        }
    }
    None
}

fn chat_display_name(
    conn: &Connection,
    chat_jid: &str,
    chat_name: Option<&str>,
    is_group: bool,
) -> Option<String> {
    clean_display_name(chat_name, Some(chat_jid))
        .or_else(|| {
            (!is_group)
                .then(|| contact_display_name(conn, chat_jid))
                .flatten()
        })
        .or_else(|| display_fallback_from_jid(chat_jid))
}

fn sender_identity(
    conn: &Connection,
    chat_jid: &str,
    sender_jid: Option<&str>,
    sender_name: Option<&str>,
    is_from_me: bool,
) -> (Option<String>, Option<String>) {
    if is_from_me {
        let address = sender_jid.map(normalize_jid);
        return (Some("You".to_string()), address);
    }
    let address = sender_jid.or(Some(chat_jid)).map(normalize_jid);
    let display = clean_display_name(sender_name, address.as_deref()).or_else(|| {
        address.as_deref().and_then(|jid| {
            group_participant_display_name(conn, chat_jid, jid)
                .or_else(|| contact_display_name(conn, jid))
                .or_else(|| display_fallback_from_jid(jid))
        })
    });
    (display, address)
}

/// Open the wu.db read-only with a busy timeout. Errors (missing, locked)
/// bubble to the worker's per-account isolation.
fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening wu.db read-only at {}", path.display()))?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    Ok(conn)
}

/// One raw message row from wu.db (no body — fetched at distill time).
/// The `chat_jid` isn't carried: rows are always read per-chat, so the
/// caller already holds it.
struct RawMessage {
    id: String,
    sender_jid: Option<String>,
    sender_name: Option<String>,
    is_from_me: bool,
    timestamp: i64,
}

// ---------------------------------------------------------------------------
// Ingestor
// ---------------------------------------------------------------------------

/// WhatsApp [`ChannelIngestor`]: reads the bot's local wu.db (metadata
/// only) into neutral rows. One wu.db per scope backs the single linked
/// WhatsApp number.
pub struct WhatsappWuIngestor;

#[async_trait]
impl ChannelIngestor for WhatsappWuIngestor {
    fn provider(&self) -> &'static str {
        WHATSAPP_PROVIDER
    }

    /// The scope's wu.db file exists (the bot has been linked). No open
    /// here — presence check only, so an unlinked WhatsApp account simply
    /// drops out like a gmail alias without a gws profile.
    fn account_ready(&self, ctx: &IngestContext, _account: &ChannelAccount) -> bool {
        wu_db_path(&ctx.scope_paths).exists()
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let db_path = wu_db_path(&ctx.scope_paths);
        let account = account.clone();
        let cursor = watermark.and_then(|w| w.provider_cursor.clone());
        let suppress = ctx.suppress_sensitive;
        let min_internal_date = ctx.min_internal_date;
        let max_threads = ctx.max_threads;
        tokio::task::spawn_blocking(move || {
            read_batch(
                &db_path,
                &account,
                cursor.as_deref(),
                suppress,
                min_internal_date,
                max_threads,
            )
        })
        .await
        .map_err(|e| anyhow!("whatsapp sync task join error: {e}"))?
    }
}

/// Blocking sqlite read → one neutral batch. Backfill (no cursor) takes
/// the most-recent `max_threads` chats and their recent messages within
/// the backfill window; incremental (cursor present) takes every message
/// with `timestamp >= cursor` (store dedups by message_id).
fn read_batch(
    db_path: &Path,
    account: &ChannelAccount,
    cursor: Option<&str>,
    suppress: bool,
    min_internal_date: i64,
    max_threads: usize,
) -> Result<IngestBatch> {
    let conn = open_readonly(db_path)?;
    let observed_at = now_millis();

    let (chat_jids, chat_cap_reached, mode): (Vec<String>, bool, &'static str) = match cursor {
        Some(raw) => {
            let cursor_ts: i64 = raw
                .parse()
                .with_context(|| format!("parsing whatsapp cursor {raw:?}"))?;
            let (chats, capped) = active_chats_since(&conn, cursor_ts, max_threads)?;
            (chats, capped, "incremental")
        },
        None => {
            let (chats, capped) = recent_chats(&conn, max_threads)?;
            (chats, capped, "backfill")
        },
    };

    let mut batch = IngestBatch {
        mode,
        ..IngestBatch::default()
    };
    let mut capped = chat_cap_reached;

    for chat_jid in &chat_jids {
        let (name, participant_group) = chat_meta(&conn, chat_jid)?;
        let chat_name = chat_display_name(&conn, chat_jid, name.as_deref(), participant_group);
        let messages = match cursor {
            Some(raw) => {
                let cursor_ts: i64 = raw.parse().unwrap_or(0);
                messages_since(&conn, chat_jid, cursor_ts)?
            },
            None => {
                let (messages, message_cap_reached) =
                    messages_recent(&conn, chat_jid, BACKFILL_PER_CHAT_LIMIT)?;
                capped = capped || message_cap_reached;
                messages
            },
        };
        if messages.is_empty() {
            continue;
        }

        let thread_id = normalize_jid(chat_jid);
        // Thread is sensitive if the chat name trips the shared heuristics
        // (the WhatsApp analogue of a sensitive subject/sender). Suppressed
        // threads stay redacted-metadata-only, exactly like gmail.
        let thread_sensitive = suppress && is_sensitive(chat_name.as_deref(), None);

        let mut thread_message_rows: Vec<MailMessageMeta> = Vec::new();
        let mut max_ts_raw: i64 = 0;
        let mut latest_from_name: Option<String> = None;
        let mut latest_from_address: Option<String> = None;
        let mut latest_millis: i64 = 0;

        for msg in messages {
            // Backfill: enforce the window in Rust (unit-agnostic).
            let ts_millis = epoch_to_millis(msg.timestamp);
            if ts_millis < min_internal_date {
                continue;
            }
            max_ts_raw = max_ts_raw.max(msg.timestamp);

            let (sender_display, sender_address) = sender_identity(
                &conn,
                chat_jid,
                msg.sender_jid.as_deref(),
                msg.sender_name.as_deref(),
                msg.is_from_me,
            );
            let msg_sensitive =
                thread_sensitive || (suppress && is_sensitive(None, sender_address.as_deref()));
            let direction = if msg.is_from_me {
                MessageDirection::Outbound
            } else {
                MessageDirection::Inbound
            };

            if ts_millis >= latest_millis {
                latest_millis = ts_millis;
                latest_from_name = sender_display.clone();
                latest_from_address = sender_address.clone();
            }

            thread_message_rows.push(MailMessageMeta {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: WHATSAPP_PROVIDER.to_string(),
                account_alias: account.account_alias.clone(),
                account_email: None,
                thread_id: thread_id.clone(),
                message_id: msg.id.clone(),
                provider_cursor: Some(msg.timestamp.to_string()),
                label_ids: Vec::new(),
                // WhatsApp messages have no subject line; understanding
                // comes from the local distiller, not a header.
                subject: None,
                from_name: sender_display,
                from_address: sender_address,
                to_domains: Vec::new(),
                cc_domains: Vec::new(),
                internal_date: ts_millis,
                observed_at,
                direction: Some(direction),
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

        if thread_message_rows.is_empty() {
            continue;
        }

        let total_count = chat_message_count(&conn, chat_jid)?;
        let label_ids = if participant_group {
            vec!["group".to_string()]
        } else {
            Vec::new()
        };

        let record = MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: WHATSAPP_PROVIDER.to_string(),
            account_alias: account.account_alias.clone(),
            account_email: None,
            thread_id: thread_id.clone(),
            lane: account.lane,
            subject: Some(if thread_sensitive {
                REDACTED_SUBJECT_PLACEHOLDER.to_string()
            } else {
                chat_name.clone().unwrap_or_else(|| {
                    display_fallback_from_jid(&thread_id).unwrap_or_else(|| thread_id.clone())
                })
            }),
            latest_summary: None,
            latest_from_name,
            latest_from_address,
            recipient_domains: Vec::new(),
            label_ids,
            message_count: total_count,
            last_message_at: Some(latest_millis),
            provider_cursor: Some(max_ts_raw.to_string()),
            sensitive_suppressed: thread_sensitive,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        };

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
        batch.threads.push(record);
        batch.messages.extend(thread_message_rows);
    }

    if capped {
        batch.max_internal_date = None;
        batch.next_provider_cursor = cursor.map(str::to_string);
    } else if batch.next_provider_cursor.is_none() {
        // Keep the incoming cursor if nothing new arrived, so the watermark
        // never regresses on an empty incremental pass.
        batch.next_provider_cursor = cursor.map(str::to_string);
    }
    Ok(batch)
}

/// Most-recently-active chats (first sync), newest first, capped.
fn recent_chats(conn: &Connection, max_threads: usize) -> Result<(Vec<String>, bool)> {
    let mut stmt =
        conn.prepare("SELECT jid FROM chats ORDER BY COALESCE(last_message_at, 0) DESC LIMIT ?1")?;
    let mut rows = stmt
        .query_map([max_threads.saturating_add(1) as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let capped = rows.len() > max_threads;
    rows.truncate(max_threads);
    Ok((rows, capped))
}

/// Chats with a message at or after the cursor (incremental), newest
/// first, capped.
fn active_chats_since(
    conn: &Connection,
    cursor_ts: i64,
    max_threads: usize,
) -> Result<(Vec<String>, bool)> {
    let mut stmt = conn.prepare(
        "SELECT chat_jid, MAX(timestamp) mt FROM messages WHERE timestamp >= ?1 \
         GROUP BY chat_jid ORDER BY mt DESC LIMIT ?2",
    )?;
    let mut rows = stmt
        .query_map([cursor_ts, max_threads.saturating_add(1) as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let capped = rows.len() > max_threads;
    rows.truncate(max_threads);
    Ok((rows, capped))
}

/// Chat name + group-ness. Missing chat row → (None, jid-suffix group check).
fn chat_meta(conn: &Connection, chat_jid: &str) -> Result<(Option<String>, bool)> {
    let name: Option<String> = conn
        .query_row("SELECT name FROM chats WHERE jid = ?1", [chat_jid], |row| {
            row.get::<_, Option<String>>(0)
        })
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok((name, is_group_jid(chat_jid)))
}

fn chat_message_count(conn: &Connection, chat_jid: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE chat_jid = ?1",
        [chat_jid],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn map_raw_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMessage> {
    Ok(RawMessage {
        id: row.get(0)?,
        sender_jid: row.get(1)?,
        sender_name: row.get(2)?,
        is_from_me: row.get::<_, i64>(3)? != 0,
        timestamp: row.get(4)?,
    })
}

fn messages_since(conn: &Connection, chat_jid: &str, cursor_ts: i64) -> Result<Vec<RawMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, sender_jid, sender_name, is_from_me, timestamp \
         FROM messages WHERE chat_jid = ?1 AND timestamp >= ?2 ORDER BY timestamp DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![chat_jid, cursor_ts], map_raw_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn messages_recent(
    conn: &Connection,
    chat_jid: &str,
    limit: usize,
) -> Result<(Vec<RawMessage>, bool)> {
    let mut stmt = conn.prepare(
        "SELECT id, sender_jid, sender_name, is_from_me, timestamp \
         FROM messages WHERE chat_jid = ?1 ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let mut rows = stmt
        .query_map(
            rusqlite::params![chat_jid, limit.saturating_add(1) as i64],
            map_raw_message,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let capped = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, capped))
}

// ---------------------------------------------------------------------------
// Content fetcher (distill-time only; body never persisted)
// ---------------------------------------------------------------------------

/// WhatsApp content path: read the message body from the local wu.db by
/// id at distill time (read-only, in-memory only) → chunk for the local
/// model. WhatsApp bodies are already clean text (no MIME/HTML/quoting),
/// so this skips the gmail parse pipeline and chunks directly.
pub struct WhatsappContentFetcher;

#[async_trait]
impl ContentFetcher for WhatsappContentFetcher {
    fn provider(&self) -> &'static str {
        WHATSAPP_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let db_path = wu_db_path(&ctx.scope_paths);
        let message_id = message.message_id.clone();
        let chunk_chars = ctx.chunk_chars;
        let max_chunks = ctx.max_chunks;
        tokio::task::spawn_blocking(move || -> Result<DistillContent> {
            let conn = open_readonly(&db_path)?;
            let body: Option<String> = conn
                .query_row(
                    "SELECT body FROM messages WHERE id = ?1",
                    [message_id.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            let Some(text) = body.filter(|b| !b.trim().is_empty()) else {
                return Ok(DistillContent::default());
            };
            let prepared = prepare_for_distill(&text, chunk_chars, max_chunks);
            Ok(DistillContent {
                chunks: prepared.chunks,
                truncated: prepared.truncated,
                had_html: false,
                attachment_count: 0,
            })
        })
        .await
        .map_err(|e| anyhow!("whatsapp content fetch task join error: {e}"))?
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::ChannelLane;
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    const SCHEMA: &str = "\
        CREATE TABLE messages (id TEXT PRIMARY KEY, chat_jid TEXT NOT NULL, sender_jid TEXT, \
        sender_name TEXT, body TEXT, type TEXT, is_from_me INTEGER DEFAULT 0, \
        timestamp INTEGER NOT NULL); \
        CREATE TABLE chats (jid TEXT PRIMARY KEY, name TEXT, type TEXT, participant_count INTEGER, \
        last_message_at INTEGER);";

    fn write_db(dir: &Path) -> PathBuf {
        let path = dir.join("wu.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        path
    }

    fn insert_chat(conn: &Connection, jid: &str, name: &str, last: i64) {
        conn.execute(
            "INSERT INTO chats (jid, name, type, participant_count, last_message_at) \
             VALUES (?1, ?2, 'individual', NULL, ?3)",
            rusqlite::params![jid, name, last],
        )
        .unwrap();
    }

    fn insert_msg(conn: &Connection, id: &str, chat: &str, from_me: i64, ts: i64, body: &str) {
        conn.execute(
            "INSERT INTO messages (id, chat_jid, sender_jid, sender_name, body, type, is_from_me, timestamp) \
             VALUES (?1, ?2, ?3, 'Sender Name', ?4, 'text', ?5, ?6)",
            rusqlite::params![id, chat, format!("{chat}"), body, from_me, ts],
        )
        .unwrap();
    }

    fn acct() -> ChannelAccount {
        ChannelAccount {
            provider: WHATSAPP_PROVIDER.to_string(),
            account_alias: "self".to_string(),
            lane: ChannelLane::UserAssist,
            enabled: true,
        }
    }

    #[test]
    fn normalize_jid_mirrors_adapter_rules() {
        assert_eq!(
            normalize_jid("1234:12@s.whatsapp.net"),
            "1234@s.whatsapp.net"
        );
        assert_eq!(
            normalize_jid("+919876543210"),
            "919876543210@s.whatsapp.net"
        );
        assert_eq!(normalize_jid("919876543210"), "919876543210@s.whatsapp.net");
        assert_eq!(normalize_jid("group-xyz@g.us"), "group-xyz@g.us");
        assert_eq!(normalize_jid("not-a-number"), "not-a-number");
    }

    #[test]
    fn epoch_normalizes_seconds_and_millis() {
        assert_eq!(epoch_to_millis(1_700_000_000), 1_700_000_000_000);
        assert_eq!(epoch_to_millis(1_700_000_000_000), 1_700_000_000_000);
    }

    #[test]
    fn backfill_reads_recent_chats_and_maps_direction() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now = now_millis() / 1000;
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "111@s.whatsapp.net", "Alice", now);
            insert_msg(&conn, "m1", "111@s.whatsapp.net", 0, now - 100, "hi there");
            insert_msg(&conn, "m2", "111@s.whatsapp.net", 1, now - 50, "reply out");
        }
        let batch = read_batch(&db, &acct(), None, true, 0, 100).unwrap();
        assert_eq!(batch.mode, "backfill");
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.threads[0].thread_id, "111@s.whatsapp.net");
        let dirs: Vec<_> = batch.messages.iter().map(|m| m.direction).collect();
        assert!(dirs.contains(&Some(MessageDirection::Inbound)));
        assert!(dirs.contains(&Some(MessageDirection::Outbound)));
        // All pending, no recipient domains, subjects None.
        assert!(batch
            .messages
            .iter()
            .all(|m| m.distill_state == DistillState::Pending));
        assert!(batch
            .messages
            .iter()
            .all(|m| m.to_domains.is_empty() && m.subject.is_none()));
        // Cursor advanced to the max raw timestamp.
        assert_eq!(
            batch.next_provider_cursor.as_deref(),
            Some((now - 50).to_string().as_str())
        );
    }

    #[test]
    fn backfill_window_excludes_old_messages() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now = now_millis() / 1000;
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "111@s.whatsapp.net", "Alice", now);
            insert_msg(
                &conn,
                "old",
                "111@s.whatsapp.net",
                0,
                now - 40 * 86_400,
                "ancient",
            );
            insert_msg(&conn, "new", "111@s.whatsapp.net", 0, now - 100, "fresh");
        }
        let min_internal_date = (now - 14 * 86_400) * 1000;
        let batch = read_batch(&db, &acct(), None, true, min_internal_date, 100).unwrap();
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(batch.messages[0].message_id, "new");
    }

    #[test]
    fn incremental_takes_only_rows_at_or_after_cursor() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let base = 1_700_000_000i64;
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "111@s.whatsapp.net", "Alice", base + 20);
            insert_msg(&conn, "a", "111@s.whatsapp.net", 0, base + 5, "one");
            insert_msg(&conn, "b", "111@s.whatsapp.net", 0, base + 15, "two");
            insert_msg(&conn, "c", "111@s.whatsapp.net", 0, base + 25, "three");
        }
        let batch = read_batch(&db, &acct(), Some(&(base + 15).to_string()), true, 0, 100).unwrap();
        assert_eq!(batch.mode, "incremental");
        let ids: Vec<_> = batch
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        assert_eq!(ids, vec!["c".to_string(), "b".to_string()]);
        assert_eq!(
            batch.next_provider_cursor.as_deref(),
            Some((base + 25).to_string().as_str())
        );
    }

    #[test]
    fn empty_incremental_keeps_cursor() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "111@s.whatsapp.net", "Alice", 1_700_000_000);
            insert_msg(&conn, "a", "111@s.whatsapp.net", 0, 1_700_000_000, "one");
        }
        let batch = read_batch(&db, &acct(), Some("1800000000"), true, 0, 100).unwrap();
        assert!(batch.messages.is_empty());
        assert_eq!(batch.next_provider_cursor.as_deref(), Some("1800000000"));
    }

    #[test]
    fn thread_cap_limits_distinct_chats() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now = now_millis() / 1000;
        {
            let conn = Connection::open(&db).unwrap();
            for i in 0..5 {
                let jid = format!("{i}@s.whatsapp.net");
                insert_chat(&conn, &jid, &format!("Chat {i}"), now - i);
                insert_msg(&conn, &format!("m{i}"), &jid, 0, now - i, "hey");
            }
        }
        let batch = read_batch(&db, &acct(), None, true, 0, 2).unwrap();
        assert_eq!(batch.threads.len(), 2);
    }

    #[test]
    fn group_jid_gets_group_label_and_no_domains() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now = now_millis() / 1000;
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "grp123@g.us", "Team Chat", now);
            insert_msg(&conn, "gm1", "grp123@g.us", 0, now - 10, "team msg");
        }
        let batch = read_batch(&db, &acct(), None, true, 0, 100).unwrap();
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.threads[0].label_ids, vec!["group".to_string()]);
        assert!(batch.threads[0].recipient_domains.is_empty());
    }

    #[test]
    fn sensitive_chat_name_redacts_and_suppresses() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now = now_millis() / 1000;
        {
            let conn = Connection::open(&db).unwrap();
            // "verification code" trips the shared sensitive-subject set.
            insert_chat(&conn, "999@s.whatsapp.net", "Your verification code", now);
            insert_msg(&conn, "s1", "999@s.whatsapp.net", 0, now - 5, "123456");
        }
        let batch = read_batch(&db, &acct(), None, true, 0, 100).unwrap();
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
    fn missing_db_is_a_clean_error_not_a_panic() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("home/.wu/wu.db");
        let err = read_batch(&missing, &acct(), None, true, 0, 100);
        assert!(err.is_err());
    }

    #[test]
    fn content_body_read_by_id() {
        // Exercises the same query the fetcher uses (fetch() itself needs a
        // DistillContext/tokio runtime; the sqlite read is the substance).
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            insert_chat(&conn, "111@s.whatsapp.net", "Alice", 1_700_000_000);
            insert_msg(
                &conn,
                "m1",
                "111@s.whatsapp.net",
                0,
                1_700_000_000,
                "the message body",
            );
        }
        let conn = open_readonly(&db).unwrap();
        let body: Option<String> = conn
            .query_row("SELECT body FROM messages WHERE id = ?1", ["m1"], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body.as_deref(), Some("the message body"));
        let prepared = prepare_for_distill(body.as_deref().unwrap(), 6000, 8);
        assert_eq!(prepared.chunks, vec!["the message body".to_string()]);
    }
}
