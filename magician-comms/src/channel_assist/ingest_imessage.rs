//! iMessage ingestor (Part B) — the user's own iMessage/SMS lane.
//!
//! Reads the macOS Messages `chat.db` SQLite store directly; it never
//! touches Apple's network. Messages.app already persists every chat and
//! message to `$HOME/Library/Messages/chat.db`. This ingestor pulls
//! neutral thread/message METADATA from that db and hands raw text to the
//! local distiller ONLY on demand (never stored) via
//! [`ImessageContentFetcher`].
//!
//! ## chat.db schema (macOS Messages)
//!
//! - `message(ROWID INTEGER PK, text, date INTEGER, is_from_me INTEGER,
//!   handle_id INTEGER, …)` — `date` is Apple-epoch (see below).
//! - `handle(ROWID INTEGER PK, id, service)` — `id` is the phone/email.
//! - `chat(ROWID INTEGER PK, guid, display_name, chat_identifier,
//!   service_name)`.
//! - `chat_message_join(chat_id, message_id)` — links messages to chats.
//!
//! ## Identity / lane mapping
//!
//! - `chat.guid` → `thread_id` (stable per conversation).
//! - `message.ROWID` (as text) → `message_id`; the linked handle's `id` →
//!   `from_address`; `is_from_me` → direction.
//! - `chat.display_name` (group name) → thread subject; a 1:1 chat has no
//!   display name, so the handle id is used instead.
//! - iMessage has no email-style domains, so `recipient_domains` stays
//!   EMPTY (privacy contract holds trivially).
//!
//! ## Timestamp units — Apple epoch
//!
//! `message.date` counts from the Apple reference date (2001-01-01 UTC).
//! Modern macOS stores NANOSECONDS; older rows may store seconds. The
//! magnitude guard in [`apple_date_to_millis`] normalizes either to unix
//! millis. The watermark cursor is the RAW max `ROWID` (a monotonically
//! increasing integer), so incremental passes compare `ROWID > cursor`.
//!
//! ## Concurrency
//!
//! Messages.app writes chat.db live. We open READ-ONLY with a busy
//! timeout + `PRAGMA query_only`; a locked/busy read surfaces as an
//! account error (the worker's per-account isolation records it and moves
//! on) and never blocks the sync loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags};

use super::assist::content::{prepare_for_distill, DistillContent};
use super::assist::distill::{ContentFetcher, DistillContext};
use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::registry::ChannelAccount;
use super::sensitivity::is_sensitive;
use super::types::{
    DistillState, MailMessageMeta, MailRecordOrigin, MailThreadRecord, MessageDirection,
    SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};

/// Provider key for the user's own iMessage lane.
pub const IMESSAGE_PROVIDER: &str = "imessage";

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(3000);

/// Milliseconds between the unix epoch (1970-01-01) and the Apple
/// reference date (2001-01-01 UTC): `978307200` seconds.
const APPLE_EPOCH_OFFSET_MS: i64 = 978_307_200_000;

/// chat.db path: `MAGICIAN_IMESSAGE_DB_PATH` override, else
/// `$HOME/Library/Messages/chat.db` (mirrors
/// `compiled_providers::resolve_messages_db_path`).
fn imessage_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("MAGICIAN_IMESSAGE_DB_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
        .join("Library/Messages/chat.db")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Normalize an Apple-epoch `message.date` (nanoseconds on modern macOS,
/// seconds on older rows) to unix millis. Values above ~1e12 are treated
/// as nanoseconds; anything smaller is treated as seconds.
fn apple_date_to_millis(raw: i64) -> i64 {
    if raw == 0 {
        return 0;
    }
    if raw.abs() > 1_000_000_000_000 {
        // Nanoseconds since the Apple epoch.
        raw / 1_000_000 + APPLE_EPOCH_OFFSET_MS
    } else {
        // Seconds since the Apple epoch.
        raw.saturating_mul(1000) + APPLE_EPOCH_OFFSET_MS
    }
}

fn nonempty_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Open chat.db read-only with a busy timeout + query-only pragma. Errors
/// (missing, locked) bubble to the worker's per-account isolation.
fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening chat.db read-only at {}", path.display()))?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .context("enabling chat.db read-only query mode")?;
    Ok(conn)
}

/// One raw message row from chat.db (no text — fetched at distill time).
struct RawMessage {
    rowid: i64,
    handle_id: Option<String>,
    is_from_me: bool,
    date: i64,
}

// ---------------------------------------------------------------------------
// Ingestor
// ---------------------------------------------------------------------------

/// iMessage [`ChannelIngestor`]: reads the local Messages `chat.db`
/// (metadata only) into neutral rows. One chat.db per host backs the
/// signed-in Apple ID's conversations.
pub struct ImessageWuIngestor;

#[async_trait]
impl ChannelIngestor for ImessageWuIngestor {
    fn provider(&self) -> &'static str {
        IMESSAGE_PROVIDER
    }

    /// The host's chat.db file exists (Messages is set up). No open here —
    /// presence check only, so a machine without Messages simply drops out
    /// like a gmail alias without a gws profile.
    fn account_ready(&self, _ctx: &IngestContext, _account: &ChannelAccount) -> bool {
        imessage_db_path().exists()
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let db_path = imessage_db_path();
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
        .map_err(|e| anyhow!("imessage sync task join error: {e}"))?
    }
}

/// Blocking sqlite read → one neutral batch. Backfill (no cursor) takes
/// every message at or after the backfill window; incremental (cursor
/// present) takes every message with `ROWID > cursor`. Both cap the number
/// of distinct chats at `max_threads` (newest-active first). The watermark
/// cursor is the max `message.ROWID` observed (a monotonic integer).
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

    let mode: &'static str = if cursor.is_some() {
        "incremental"
    } else {
        "backfill"
    };

    let cursor_rowid: i64 = match cursor {
        Some(raw) => raw
            .parse()
            .with_context(|| format!("parsing imessage cursor {raw:?}"))?,
        None => 0,
    };

    // Chats active since the cursor (incremental) or the newest chats
    // overall (backfill), newest-message first, capped at max_threads.
    let (chat_rowids, chat_cap_reached) = active_chats(&conn, cursor, cursor_rowid, max_threads)?;

    let mut batch = IngestBatch {
        mode,
        ..IngestBatch::default()
    };
    let capped = chat_cap_reached;

    for chat_rowid in &chat_rowids {
        let (guid, display_name) = chat_meta(&conn, *chat_rowid)?;
        let Some(guid) = guid else {
            continue;
        };
        let messages = messages_for_chat(&conn, *chat_rowid, cursor, cursor_rowid)?;
        if messages.is_empty() {
            continue;
        }

        // A 1:1 chat has no display_name; use the latest handle id as the
        // thread's subject-equivalent. Group chats carry a display_name.
        let display_name = nonempty_trimmed(display_name.as_deref());
        let latest_handle = messages
            .iter()
            .rev()
            .find_map(|m| nonempty_trimmed(m.handle_id.as_deref()));
        let subject_source = display_name
            .clone()
            .or_else(|| latest_handle.clone())
            .unwrap_or_else(|| guid.clone());

        // Thread is sensitive if the subject-equivalent or the handle id
        // trips the shared heuristics.
        let thread_sensitive = suppress
            && (is_sensitive(Some(&subject_source), None)
                || is_sensitive(None, latest_handle.as_deref()));

        let thread_id = guid.clone();
        let mut thread_message_rows: Vec<MailMessageMeta> = Vec::new();
        let mut max_rowid: i64 = 0;
        let mut latest_from_address: Option<String> = None;
        let mut latest_millis: i64 = 0;

        for msg in &messages {
            let ts_millis = apple_date_to_millis(msg.date);
            // Backfill: enforce the window in Rust (unit-agnostic).
            if ts_millis < min_internal_date {
                continue;
            }
            max_rowid = max_rowid.max(msg.rowid);

            let from_address = nonempty_trimmed(msg.handle_id.as_deref());
            let msg_sensitive =
                thread_sensitive || (suppress && is_sensitive(None, from_address.as_deref()));
            let direction = if msg.is_from_me {
                MessageDirection::Outbound
            } else {
                MessageDirection::Inbound
            };

            if ts_millis >= latest_millis {
                latest_millis = ts_millis;
                latest_from_address = from_address.clone();
            }

            thread_message_rows.push(MailMessageMeta {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: IMESSAGE_PROVIDER.to_string(),
                account_alias: account.account_alias.clone(),
                account_email: None,
                thread_id: thread_id.clone(),
                message_id: msg.rowid.to_string(),
                provider_cursor: Some(msg.rowid.to_string()),
                label_ids: Vec::new(),
                // iMessages have no subject line; understanding comes from
                // the local distiller, not a header.
                subject: None,
                from_name: None,
                from_address,
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

        let total_count = chat_message_count(&conn, *chat_rowid)?;

        let record = MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: IMESSAGE_PROVIDER.to_string(),
            account_alias: account.account_alias.clone(),
            account_email: None,
            thread_id: thread_id.clone(),
            lane: account.lane,
            subject: Some(if thread_sensitive {
                REDACTED_SUBJECT_PLACEHOLDER.to_string()
            } else {
                subject_source.clone()
            }),
            latest_summary: None,
            latest_from_name: None,
            latest_from_address,
            recipient_domains: Vec::new(),
            label_ids: Vec::new(),
            message_count: total_count,
            last_message_at: Some(latest_millis),
            provider_cursor: Some(max_rowid.to_string()),
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
                .max(max_rowid)
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

/// Distinct chats to ingest this pass, newest-active first, capped.
/// Incremental (cursor present) considers only chats with a message whose
/// `ROWID > cursor`; backfill considers every chat with any message.
fn active_chats(
    conn: &Connection,
    cursor: Option<&str>,
    cursor_rowid: i64,
    max_threads: usize,
) -> Result<(Vec<i64>, bool)> {
    let limit = max_threads.saturating_add(1) as i64;
    let mut rows: Vec<i64> = if cursor.is_some() {
        let mut stmt = conn.prepare(
            "SELECT cmj.chat_id, MAX(m.ROWID) mr FROM chat_message_join cmj \
             JOIN message m ON m.ROWID = cmj.message_id \
             WHERE m.ROWID > ?1 GROUP BY cmj.chat_id ORDER BY mr DESC LIMIT ?2",
        )?;
        let collected = stmt
            .query_map([cursor_rowid, limit], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    } else {
        let mut stmt = conn.prepare(
            "SELECT cmj.chat_id, MAX(m.ROWID) mr FROM chat_message_join cmj \
             JOIN message m ON m.ROWID = cmj.message_id \
             GROUP BY cmj.chat_id ORDER BY mr DESC LIMIT ?1",
        )?;
        let collected = stmt
            .query_map([limit], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    let capped = rows.len() > max_threads;
    rows.truncate(max_threads);
    Ok((rows, capped))
}

/// Chat guid + display_name. Missing chat row → (None, None).
fn chat_meta(conn: &Connection, chat_rowid: i64) -> Result<(Option<String>, Option<String>)> {
    conn.query_row(
        "SELECT guid, display_name FROM chat WHERE ROWID = ?1",
        [chat_rowid],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok((None, None)),
        other => Err(other),
    })
    .map_err(Into::into)
}

fn chat_message_count(conn: &Connection, chat_rowid: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_message_join WHERE chat_id = ?1",
        [chat_rowid],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn map_raw_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMessage> {
    Ok(RawMessage {
        rowid: row.get(0)?,
        handle_id: row.get(1)?,
        is_from_me: row.get::<_, i64>(2)? != 0,
        date: row.get(3)?,
    })
}

/// Messages in one chat, joined to their handle id, ascending by ROWID.
/// Incremental (cursor present) takes only `ROWID > cursor`.
fn messages_for_chat(
    conn: &Connection,
    chat_rowid: i64,
    cursor: Option<&str>,
    cursor_rowid: i64,
) -> Result<Vec<RawMessage>> {
    if cursor.is_some() {
        let mut stmt = conn.prepare(
            "SELECT m.ROWID, h.id, m.is_from_me, m.date FROM message m \
             JOIN chat_message_join cmj ON cmj.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE cmj.chat_id = ?1 AND m.ROWID > ?2 ORDER BY m.ROWID ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![chat_rowid, cursor_rowid], map_raw_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    } else {
        let mut stmt = conn.prepare(
            "SELECT m.ROWID, h.id, m.is_from_me, m.date FROM message m \
             JOIN chat_message_join cmj ON cmj.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE cmj.chat_id = ?1 ORDER BY m.ROWID ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![chat_rowid], map_raw_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Content fetcher (distill-time only; body never persisted)
// ---------------------------------------------------------------------------

/// iMessage content path: read the message text from the local chat.db by
/// ROWID at distill time (read-only, in-memory only) → chunk for the local
/// model. iMessage bodies are already clean text (no MIME/HTML/quoting),
/// so this skips the gmail parse pipeline and chunks directly.
/// One inbound message as automatic verification-code retrieval reads it
/// (secure HITL P6). `Debug` never prints the text.
pub struct ImessageVerificationRow {
    pub rowid: i64,
    pub received_at_ms: i64,
    /// The handle (a phone number, a short code, an address) — a label, not
    /// an authenticated identity.
    pub sender: Option<String>,
    pub text: String,
}

impl std::fmt::Debug for ImessageVerificationRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImessageVerificationRow")
            .field("rowid", &self.rowid)
            .field("received_at_ms", &self.received_at_ms)
            .finish_non_exhaustive()
    }
}

/// Inbound messages received at or after `since_ms`, newest first, at most
/// `limit`, with their text — one bounded read-only query of the local
/// `chat.db` for a live verification challenge. Messages this Mac sent are
/// excluded; nothing is written and nothing is kept.
pub async fn recent_inbound_for_verification(
    since_ms: i64,
    limit: usize,
) -> Result<Vec<ImessageVerificationRow>> {
    recent_inbound_for_verification_at(imessage_db_path(), since_ms, limit).await
}

async fn recent_inbound_for_verification_at(
    db_path: PathBuf,
    since_ms: i64,
    limit: usize,
) -> Result<Vec<ImessageVerificationRow>> {
    let limit = limit.clamp(1, 20) as i64;
    tokio::task::spawn_blocking(move || -> Result<Vec<ImessageVerificationRow>> {
        let conn = open_readonly(&db_path)?;
        // `date` is Apple-epoch nanoseconds on modern macOS and seconds on
        // older stores. The bound is written in seconds — small enough to
        // admit both units — and the window is enforced per row after the
        // unit-aware conversion; the newest-first LIMIT keeps the read small.
        let since_apple_secs = (since_ms - APPLE_EPOCH_OFFSET_MS).max(0) / 1000;
        let mut stmt = conn.prepare(
            "SELECT m.ROWID, h.id, m.date, m.text FROM message m \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE m.is_from_me = 0 AND m.date >= ?1 AND m.text IS NOT NULL AND trim(m.text) <> '' \
             ORDER BY m.date DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![since_apple_secs, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rows: Vec<ImessageVerificationRow> = rows
            .into_iter()
            .filter_map(|(rowid, sender, date, text)| {
                let received_at_ms = apple_date_to_millis(date);
                let text = text?.trim().to_string();
                (received_at_ms >= since_ms && !text.is_empty()).then_some(
                    ImessageVerificationRow {
                        rowid,
                        received_at_ms,
                        sender: sender
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                        text,
                    },
                )
            })
            .collect();
        // Newest first by the converted time: the store's own order is by
        // raw value, which a mixed-unit store would sort wrongly.
        rows.sort_by(|a, b| b.received_at_ms.cmp(&a.received_at_ms));
        Ok(rows)
    })
    .await
    .map_err(|e| anyhow!("imessage verification read task join error: {e}"))?
}

pub struct ImessageContentFetcher;

#[async_trait]
impl ContentFetcher for ImessageContentFetcher {
    fn provider(&self) -> &'static str {
        IMESSAGE_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let db_path = imessage_db_path();
        let message_id = message.message_id.clone();
        let chunk_chars = ctx.chunk_chars;
        let max_chunks = ctx.max_chunks;
        tokio::task::spawn_blocking(move || -> Result<DistillContent> {
            let conn = open_readonly(&db_path)?;
            let rowid: i64 = message_id
                .parse()
                .with_context(|| format!("parsing imessage message_id {message_id:?}"))?;
            let text: Option<String> = conn
                .query_row(
                    "SELECT text FROM message WHERE ROWID = ?1",
                    [rowid],
                    |row| row.get::<_, Option<String>>(0),
                )
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            let Some(text) = text.filter(|b| !b.trim().is_empty()) else {
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
        .map_err(|e| anyhow!("imessage content fetch task join error: {e}"))?
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::ChannelLane;
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    const SCHEMA: &str = "\
        CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, date INTEGER, \
        is_from_me INTEGER DEFAULT 0, handle_id INTEGER); \
        CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT, service TEXT); \
        CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid TEXT, display_name TEXT, \
        chat_identifier TEXT, service_name TEXT); \
        CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);";

    fn write_db(dir: &Path) -> PathBuf {
        let path = dir.join("chat.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        path
    }

    fn insert_handle(conn: &Connection, rowid: i64, id: &str) {
        conn.execute(
            "INSERT INTO handle (ROWID, id, service) VALUES (?1, ?2, 'iMessage')",
            rusqlite::params![rowid, id],
        )
        .unwrap();
    }

    fn insert_chat(conn: &Connection, rowid: i64, guid: &str, display_name: &str) {
        conn.execute(
            "INSERT INTO chat (ROWID, guid, display_name, chat_identifier, service_name) \
             VALUES (?1, ?2, ?3, ?4, 'iMessage')",
            rusqlite::params![rowid, guid, display_name, guid],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_msg(
        conn: &Connection,
        rowid: i64,
        chat_rowid: i64,
        handle_id: Option<i64>,
        from_me: i64,
        date: i64,
        text: &str,
    ) {
        conn.execute(
            "INSERT INTO message (ROWID, text, date, is_from_me, handle_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![rowid, text, date, from_me, handle_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?1, ?2)",
            rusqlite::params![chat_rowid, rowid],
        )
        .unwrap();
    }

    fn acct() -> ChannelAccount {
        ChannelAccount {
            provider: IMESSAGE_PROVIDER.to_string(),
            account_alias: "self".to_string(),
            lane: ChannelLane::UserAssist,
            enabled: true,
        }
    }

    /// A modern-macOS nanosecond Apple-epoch value for "now-ish".
    fn apple_ns(unix_ms: i64) -> i64 {
        (unix_ms - APPLE_EPOCH_OFFSET_MS) * 1_000_000
    }

    #[test]
    fn apple_date_normalizes_nanoseconds_and_seconds() {
        // Nanoseconds since the Apple epoch → 2023-11-14T22:13:20Z.
        let ns = 721_678_400 * 1_000_000_000i64;
        assert_eq!(
            apple_date_to_millis(ns),
            721_678_400 * 1000 + APPLE_EPOCH_OFFSET_MS
        );
        // Legacy seconds since the Apple epoch.
        let secs = 721_678_400i64;
        assert_eq!(
            apple_date_to_millis(secs),
            721_678_400 * 1000 + APPLE_EPOCH_OFFSET_MS
        );
        assert_eq!(apple_date_to_millis(0), 0);
    }

    /// Secure HITL P6: the verification read is inbound-only, windowed on
    /// the store's own receive time in either unit, newest first, bounded,
    /// and prints no text.
    #[tokio::test]
    async fn a_verification_read_returns_inbound_rows_inside_the_window_newest_first() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now_ms = 1_790_078_400_000;
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "VERIFY");
            insert_handle(&conn, 2, "+15551234567");
            insert_chat(&conn, 10, "SMS;-;VERIFY", "");
            insert_msg(
                &conn,
                100,
                10,
                Some(1),
                0,
                apple_ns(now_ms - 400_000),
                "Your code is 111000 (stale)",
            );
            insert_msg(
                &conn,
                101,
                10,
                Some(1),
                0,
                apple_ns(now_ms - 30_000),
                "Your verification code is 482913",
            );
            // A legacy seconds-unit row inside the window.
            insert_msg(
                &conn,
                102,
                10,
                Some(2),
                0,
                (now_ms - 10_000 - APPLE_EPOCH_OFFSET_MS) / 1000,
                "Login code 573920",
            );
            insert_msg(
                &conn,
                103,
                10,
                Some(1),
                1,
                apple_ns(now_ms - 5_000),
                "sent by me 999111",
            );
            insert_msg(&conn, 104, 10, Some(1), 0, apple_ns(now_ms - 1_000), "   ");
        }
        let rows = recent_inbound_for_verification_at(db.clone(), now_ms - 90_000, 10)
            .await
            .unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.rowid).collect();
        assert_eq!(
            ids,
            vec![102, 101],
            "outbound, stale and empty rows are out; newest first"
        );
        assert_eq!(rows[0].sender.as_deref(), Some("+15551234567"));
        assert_eq!(rows[0].received_at_ms / 1000, (now_ms - 10_000) / 1000);
        assert_eq!(rows[1].received_at_ms, now_ms - 30_000);
        assert_eq!(rows[1].text, "Your verification code is 482913");
        assert!(!format!("{:?}", rows[1]).contains("482913"));
        assert_eq!(
            recent_inbound_for_verification_at(db, now_ms - 90_000, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn backfill_reads_chat_and_maps_direction_and_cursor() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now_ms = now_millis();
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15551234567");
            insert_chat(&conn, 10, "iMessage;-;+15551234567", "");
            // Inbound from the handle, then an outbound reply.
            insert_msg(
                &conn,
                100,
                10,
                Some(1),
                0,
                apple_ns(now_ms - 100_000),
                "hi there",
            );
            insert_msg(
                &conn,
                101,
                10,
                Some(1),
                1,
                apple_ns(now_ms - 50_000),
                "reply out",
            );
        }
        let batch = read_batch(&db, &acct(), None, false, 0, 100).unwrap();
        assert_eq!(batch.mode, "backfill");
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 2);
        // thread_id = chat.guid; subject = handle id (no display_name).
        assert_eq!(batch.threads[0].thread_id, "iMessage;-;+15551234567");
        assert_eq!(batch.threads[0].subject.as_deref(), Some("+15551234567"));

        let dirs: Vec<_> = batch.messages.iter().map(|m| m.direction).collect();
        assert!(dirs.contains(&Some(MessageDirection::Inbound)));
        assert!(dirs.contains(&Some(MessageDirection::Outbound)));

        // Every message row carries the chat's thread_id.
        assert!(batch
            .messages
            .iter()
            .all(|m| m.thread_id == "iMessage;-;+15551234567"));
        // message_id is the ROWID string.
        let ids: Vec<_> = batch
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        assert!(ids.contains(&"100".to_string()));
        assert!(ids.contains(&"101".to_string()));
        // No subjects / no recipient domains.
        assert!(batch
            .messages
            .iter()
            .all(|m| m.to_domains.is_empty() && m.subject.is_none()));
        assert!(batch
            .messages
            .iter()
            .all(|m| m.distill_state == DistillState::Pending));
        // Cursor advanced to the max ROWID.
        assert_eq!(batch.next_provider_cursor.as_deref(), Some("101"));
    }

    #[test]
    fn group_chat_uses_display_name_as_subject() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15551110000");
            insert_chat(&conn, 20, "iMessage;+;group-guid", "Team Chat");
            insert_msg(
                &conn,
                200,
                20,
                Some(1),
                0,
                apple_ns(now_millis() - 10_000),
                "team msg",
            );
        }
        let batch = read_batch(&db, &acct(), None, false, 0, 100).unwrap();
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.threads[0].subject.as_deref(), Some("Team Chat"));
        assert!(batch.threads[0].recipient_domains.is_empty());
    }

    #[test]
    fn incremental_takes_only_rows_after_cursor() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now_ms = now_millis();
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15551234567");
            insert_chat(&conn, 10, "guid-a", "");
            insert_msg(&conn, 5, 10, Some(1), 0, apple_ns(now_ms - 300_000), "one");
            insert_msg(&conn, 15, 10, Some(1), 0, apple_ns(now_ms - 200_000), "two");
            insert_msg(
                &conn,
                25,
                10,
                Some(1),
                0,
                apple_ns(now_ms - 100_000),
                "three",
            );
        }
        let batch = read_batch(&db, &acct(), Some("15"), false, 0, 100).unwrap();
        assert_eq!(batch.mode, "incremental");
        let ids: Vec<_> = batch
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        assert_eq!(ids, vec!["25".to_string()]);
        assert_eq!(batch.next_provider_cursor.as_deref(), Some("25"));
    }

    #[test]
    fn empty_incremental_keeps_cursor() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15551234567");
            insert_chat(&conn, 10, "guid-a", "");
            insert_msg(
                &conn,
                5,
                10,
                Some(1),
                0,
                apple_ns(now_millis() - 100_000),
                "one",
            );
        }
        let batch = read_batch(&db, &acct(), Some("999"), false, 0, 100).unwrap();
        assert!(batch.messages.is_empty());
        assert_eq!(batch.next_provider_cursor.as_deref(), Some("999"));
    }

    #[test]
    fn thread_cap_limits_distinct_chats() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        let now_ms = now_millis();
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15550000000");
            for i in 0..5i64 {
                insert_chat(&conn, 10 + i, &format!("guid-{i}"), "");
                insert_msg(
                    &conn,
                    100 + i,
                    10 + i,
                    Some(1),
                    0,
                    apple_ns(now_ms - (i * 1000)),
                    "hey",
                );
            }
        }
        let batch = read_batch(&db, &acct(), None, false, 0, 2).unwrap();
        assert_eq!(batch.threads.len(), 2);
    }

    #[test]
    fn sensitive_handle_redacts_and_suppresses() {
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            // A group name that trips the shared sensitive-subject set.
            insert_handle(&conn, 1, "+15559990000");
            insert_chat(&conn, 30, "guid-otp", "Your verification code");
            insert_msg(
                &conn,
                300,
                30,
                Some(1),
                0,
                apple_ns(now_millis() - 5_000),
                "123456",
            );
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
        let missing = dir.path().join("Library/Messages/chat.db");
        let err = read_batch(&missing, &acct(), None, false, 0, 100);
        assert!(err.is_err());
    }

    #[test]
    fn content_text_read_by_rowid() {
        // Exercises the same query the fetcher uses (fetch() itself needs a
        // DistillContext/tokio runtime; the sqlite read is the substance).
        let dir = TempDir::new().unwrap();
        let db = write_db(dir.path());
        {
            let conn = Connection::open(&db).unwrap();
            insert_handle(&conn, 1, "+15551234567");
            insert_chat(&conn, 10, "guid-a", "");
            insert_msg(
                &conn,
                100,
                10,
                Some(1),
                0,
                apple_ns(now_millis()),
                "the message body",
            );
        }
        let conn = open_readonly(&db).unwrap();
        let text: Option<String> = conn
            .query_row("SELECT text FROM message WHERE ROWID = ?1", [100i64], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text.as_deref(), Some("the message body"));
        let prepared = prepare_for_distill(text.as_deref().unwrap(), 6000, 8);
        assert_eq!(prepared.chunks, vec!["the message body".to_string()]);
    }
}
