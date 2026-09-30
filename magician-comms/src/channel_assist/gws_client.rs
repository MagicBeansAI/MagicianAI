//! Thin Gmail-metadata wrapper over the `gws` CLI (Mail Assist Phase 1, M2).
//!
//! # CLI capability probe (2026-07-05, gws from `skillshub/node_modules/.bin`)
//!
//! The gws CLI mirrors the Gmail REST discovery surface as
//! `gws gmail users <resource> <method> --params '<JSON>' --format json`;
//! `gws schema gmail.users.<resource>.<method>` prints each method's
//! parameter/response schema. Probe conclusions the wrapper rides on:
//!
//! - **`messages list`** takes `{userId,q,maxResults(≤500),pageToken,
//!   labelIds,includeSpamTrash}` and returns ONLY
//!   `{messages:[{id,threadId}],nextPageToken,resultSizeEstimate}` — no
//!   headers, no dates. Metadata therefore requires a second call per
//!   message or (cheaper) per thread.
//! - **`threads get`** with `format:"metadata"` +
//!   `metadataHeaders:["Subject","From","To","Cc"]` returns the whole
//!   thread: `{id,historyId,messages:[…]}` where each message carries
//!   `id`, `threadId`, `historyId`, `labelIds:[String]`,
//!   `internalDate` (STRING of epoch millis), `sizeEstimate`, and
//!   `payload:{mimeType,headers:[{name,value}]}` filtered to the
//!   requested header names (absent headers simply don't appear).
//!   **`snippet` is returned even in metadata format** — the privacy
//!   contract forbids storing it, so the raw structs below never
//!   deserialize it and it cannot leave this module.
//! - **`messages get`** (Phase 1b N2, flags confirmed via
//!   `gws schema gmail.users.messages.get`) takes `{userId,id,format}`
//!   where `format:"full"` returns the parsed MIME tree:
//!   `payload:{partId,mimeType,filename,headers,body:{attachmentId,
//!   data,size},parts:[…recursive…]}` with `body.data` base64url-encoded.
//!   Attachment parts carry a non-empty `filename` and (when large) an
//!   `attachmentId` instead of inline data — this wrapper NEVER
//!   deserializes `attachmentId`, so attachment bytes cannot even be
//!   requested from here. `snippet` stays un-deserialized as everywhere
//!   else in this module.
//! - **`history list` IS exposed** (`gws gmail users history list`) with
//!   `{userId,startHistoryId,historyTypes:["messageAdded",…],maxResults,
//!   pageToken,labelId}` → M3 can use true incremental sync. An invalid/
//!   expired `startHistoryId` returns HTTP 404 (surface via
//!   [`GwsGmailError::history_expired`]) meaning "fall back to a full
//!   re-list". `getProfile` supplies the baseline `historyId`.
//! - Errors: exit code 1 with `{"error":{"code":404,"message":…,
//!   "reason":…}}` on STDOUT and keyring noise on stderr — handled by the
//!   shared `gws_cli` helpers.
//! - Output flags: `--format json` (default) prints one JSON document;
//!   `--page-all` emits NDJSON pages — the wrapper paginates manually via
//!   `pageToken` instead so page budgets stay explicit.
//!
//! # Privacy contract
//!
//! Raw CLI JSON (which contains full To/Cc recipient lists and snippets)
//! is parsed inside this module only; the metadata surface exposes
//! [`MailMessageMeta`] with recipient DOMAINS only and no snippet/body
//! fields, per the design's captured-fields contract
//! (`docs/plans/2026-07-05-mail-assist-phase1-design.md` §1).
//!
//! Phase 1b amends this with ONE deliberate, narrow exception
//! (`2026-07-05-channel-assist-phase1b-design.md`, "Local-only
//! distillation"): [`GwsGmailClient::get_message_full`] returns the
//! message's MIME payload tree ([`GmailFullMessage`]) so the local
//! distillation pipeline can derive `{summary, intent}` on the local
//! model. Body content exists ONLY in process memory on that path — the
//! full-format types deliberately do NOT implement `Serialize`, so they
//! cannot land in store rows, API responses, or logs-by-serialization;
//! `super::content` parses them and the N3 distill queue discards the
//! text after distilling. Attachments are never fetched or decoded
//! (`attachmentId` is not even deserialized).

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;

use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use magician::magician_v2::artifact_v2::CapabilityScopePaths;
use magician::magician_v2::gws_cli::{
    gws_binary, gws_error_api_code, gws_error_detail, GWS_TIMEOUT,
};

use super::types::{DistillState, MailMessageMeta, MailRecordOrigin, MAIL_ASSIST_SCHEMA_VERSION};

/// Headers requested from `format=metadata` calls — exactly the
/// captured-fields contract, nothing more.
const METADATA_HEADERS: [&str; 4] = ["Subject", "From", "To", "Cc"];

/// Headers a verification-code read asks for in metadata format. Only the
/// plain `Authentication-Results`: an `ARC-Authentication-Results` set is
/// chain material a receiver preserves as it arrived (RFC 8617), so a
/// sender can ship one naming Gmail's own server and it reaches us intact.
const VERIFICATION_HEADERS: [&str; 3] = ["From", "Subject", "Authentication-Results"];
/// The authentication server whose `Authentication-Results` count: Gmail's
/// own receiving MX. A header any earlier hop — or the sender — wrote is
/// text, not a verdict (a receiver strips an inbound plain header that
/// claims its own authserv-id, RFC 8601).
const GMAIL_AUTHSERV_ID: &str = "mx.google.com";

/// Manual-pagination safety budget for [`GwsGmailClient::list_message_metadata`]
/// (mirrors the CLI's own `--page-limit` default).
const MAX_LIST_PAGES: usize = 10;

/// Failure from a gws Gmail invocation. `api_code` carries the HTTP status
/// from the CLI's stdout error JSON when one was present, so callers can
/// branch on specific API failures without string matching.
#[derive(Debug, Clone)]
pub struct GwsGmailError {
    pub detail: String,
    pub api_code: Option<i64>,
}

impl GwsGmailError {
    fn other(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            api_code: None,
        }
    }

    /// Gmail returns 404 from `history.list` when `startHistoryId` has
    /// expired (typically valid ~a week) — the caller must fall back to a
    /// full watermark re-list.
    pub fn history_expired(&self) -> bool {
        self.api_code == Some(404)
    }

    pub fn not_found(&self) -> bool {
        self.api_code == Some(404)
    }
}

impl std::fmt::Display for GwsGmailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for GwsGmailError {}

/// One row from `messages list` / `history list` — ids only (that is all
/// the API returns at list time; see the module header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailMessageRef {
    pub message_id: String,
    pub thread_id: String,
}

/// One page of `messages list` output.
#[derive(Debug, Clone)]
pub struct GmailMessageRefPage {
    pub refs: Vec<GmailMessageRef>,
    pub next_page_token: Option<String>,
    pub result_size_estimate: Option<u64>,
}

/// A whole thread's metadata — the thread-grouping input for the M1 store
/// (`MailThreadRecord` is derived from these by the sync worker, which
/// owns observed-at bookkeeping and sensitive suppression). Messages are
/// in the API's order (oldest first).
#[derive(Debug, Clone)]
pub struct GmailThreadMeta {
    pub thread_id: String,
    /// The thread's latest history id — the incremental-sync cursor.
    pub history_id: Option<String>,
    pub messages: Vec<MailMessageMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmailHistoryChangeKind {
    MessageAdded,
    MessageDeleted,
    LabelsAdded,
    LabelsRemoved,
}

/// One Gmail history delta. Label-only changes are retained because archive,
/// trash, and spam transitions may reconcile work without adding a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailHistoryChange {
    pub history_id: Option<String>,
    pub kind: GmailHistoryChangeKind,
    pub message: GmailMessageRef,
    pub label_ids: Vec<String>,
}

/// One page of `history list` output, reduced to ids, label changes, and the
/// next cursor. No body or snippet fields are deserialized.
#[derive(Debug, Clone)]
pub struct GmailHistoryPage {
    /// The mailbox's current history id — store as the next cursor once
    /// all pages are consumed.
    pub latest_history_id: Option<String>,
    /// Messages added since the start cursor (kept for compatibility and
    /// convenient incremental-thread collection).
    pub added: Vec<GmailMessageRef>,
    /// All relevant history changes, including delete and label-only deltas.
    pub changes: Vec<GmailHistoryChange>,
    pub next_page_token: Option<String>,
}

/// `users getProfile` — supplies the account email for record stamping and
/// the baseline `historyId` for starting incremental sync.
#[derive(Debug, Clone)]
pub struct GmailProfile {
    pub email_address: Option<String>,
    pub history_id: Option<String>,
    pub messages_total: Option<u64>,
}

/// One message in `format=full` — the parsed MIME payload tree, fetched
/// per pending message by the distillation queue (Phase 1b N2/N3).
///
/// PRIVACY: this type carries BODY CONTENT and therefore lives only in
/// process memory. It (and [`GmailMimePart`]/[`GmailMimeBody`])
/// intentionally implements `Deserialize` but NOT `Serialize`, so the
/// content cannot be written into store rows or API payloads by
/// accident. `snippet` and header values are not deserialized at all on
/// this path (`super::content` needs only the body tree; metadata comes
/// from the established `format=metadata` calls).
#[derive(Debug, Clone)]
pub struct GmailFullMessage {
    pub message_id: String,
    pub thread_id: String,
    /// The MIME tree root; absent on malformed/empty API responses.
    pub payload: Option<GmailMimePart>,
}

/// One node of the Gmail `format=full` MIME tree (`MessagePart` in the
/// discovery schema): containers (`multipart/*`) carry `parts`, leaves
/// carry `body.data`. See [`GmailFullMessage`] for the privacy contract
/// (in-memory only, deliberately not `Serialize`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GmailMimePart {
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Non-empty ⇒ this part is an attachment. Presence is NOTED by the
    /// content extractor (never persisted); attachment bytes are never
    /// fetched or decoded — the `body.attachmentId` needed to fetch them
    /// is deliberately not deserialized.
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub body: Option<GmailMimeBody>,
    #[serde(default)]
    pub parts: Vec<GmailMimePart>,
}

/// Leaf body of a [`GmailMimePart`]: `data` is base64url-encoded bytes
/// (may be absent for container parts and for attachments whose data
/// lives behind the un-deserialized `attachmentId`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GmailMimeBody {
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

/// Thin Gmail-metadata client shelling the `gws` CLI per account, using
/// the meetings-api auth seam: per-account profile dirs under the scope
/// capability auth root (`{auth_root}/gws-{account}`), env-pair injection,
/// 20s timeout, stdout-JSON error extraction. Rust never touches tokens.
#[derive(Debug, Clone)]
pub struct GwsGmailClient {
    auth_root: PathBuf,
    scope_paths: Option<CapabilityScopePaths>,
}

impl GwsGmailClient {
    pub fn new(auth_root: impl Into<PathBuf>, scope_paths: Option<CapabilityScopePaths>) -> Self {
        Self {
            auth_root: auth_root.into(),
            scope_paths,
        }
    }

    fn account_config_dir(&self, account: &str) -> PathBuf {
        self.auth_root.join(format!("gws-{account}"))
    }

    /// Run one `gws gmail users <resource> <method>` invocation for an
    /// account and return raw stdout on success.
    async fn run_gmail(
        &self,
        account: &str,
        method_args: &[&str],
        params: serde_json::Value,
    ) -> Result<Vec<u8>, GwsGmailError> {
        let config_dir = self.account_config_dir(account);
        if !config_dir.exists() {
            return Err(GwsGmailError::other(format!(
                "no gws profile for '{account}' under the scope auth root"
            )));
        }
        let cloudsdk_config_dir = config_dir.join("cloudsdk");
        fs::create_dir_all(&cloudsdk_config_dir).map_err(|e| {
            GwsGmailError::other(format!(
                "create Cloud SDK config dir for '{account}' at {}: {e}",
                cloudsdk_config_dir.display()
            ))
        })?;

        // Augment PATH with the scope's tool-bin dirs so a bare `gws`, or
        // the skillshub-local gws whose `#!/usr/bin/env node` shebang needs
        // node on PATH, resolves instead of exiting 127. Fail-safe: no
        // scope_paths / no existing bin dirs → leave the inherited PATH.
        // Computed first so the binary is resolved against the PATH the
        // child will see and the spawn stays on `posix_spawn` (see
        // `runtime_core::process`).
        let child_path = self.scope_paths.as_ref().and_then(|sp| {
            let parent = std::env::var("PATH").unwrap_or_default();
            sp.subprocess_bin_path(None, &parent)
        });
        let mut cmd = Command::new(runtime_core::process::resolve_program(
            gws_binary().as_os_str(),
            child_path.as_deref().map(std::ffi::OsStr::new),
        ));
        cmd.arg("gmail").arg("users");
        for arg in method_args {
            cmd.arg(arg);
        }
        cmd.arg("--params")
            .arg(params.to_string())
            .arg("--format")
            .arg("json")
            .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &config_dir)
            .env("CLOUDSDK_CONFIG", &cloudsdk_config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(p) = child_path.as_deref() {
            cmd.env("PATH", p);
        }

        let output = tokio::time::timeout(GWS_TIMEOUT, cmd.output())
            .await
            .map_err(|_| GwsGmailError::other("gws gmail timed out"))?
            .map_err(|e| GwsGmailError::other(format!("spawn gws: {e}")))?;
        if !output.status.success() {
            return Err(GwsGmailError {
                detail: gws_error_detail("gmail", &output),
                api_code: gws_error_api_code(&output),
            });
        }
        Ok(output.stdout)
    }

    /// `messages list` — ids only (see module header). `query` is a Gmail
    /// search-box query (e.g. an `after:`/`newer_than:` window).
    pub async fn list_message_refs(
        &self,
        account: &str,
        query: Option<&str>,
        max_results: u32,
        page_token: Option<&str>,
    ) -> Result<GmailMessageRefPage, GwsGmailError> {
        let mut params = json!({
            "userId": "me",
            "maxResults": max_results.clamp(1, 500),
        });
        if let Some(q) = query {
            params["q"] = json!(q);
        }
        if let Some(token) = page_token {
            params["pageToken"] = json!(token);
        }
        let stdout = self
            .run_gmail(account, &["messages", "list"], params)
            .await?;
        parse_list_response(&stdout)
    }

    /// `threads get` in metadata format — the whole thread's message
    /// metadata in one call, already converted to the M1 contract
    /// (recipient domains only; snippet never parsed).
    pub async fn get_thread_metadata(
        &self,
        account: &str,
        thread_id: &str,
    ) -> Result<GmailThreadMeta, GwsGmailError> {
        let params = json!({
            "userId": "me",
            "id": thread_id,
            "format": "metadata",
            "metadataHeaders": METADATA_HEADERS,
        });
        let stdout = self.run_gmail(account, &["threads", "get"], params).await?;
        parse_thread_metadata(&stdout, account, now_millis())
    }

    /// `history list` since a cursor. No `historyTypes` filter is sent so
    /// message additions/deletions and label-only archive/trash changes are
    /// observed in one ordered cursor stream. A 404
    /// (`GwsGmailError::history_expired`) means the cursor lapsed and the
    /// caller must fall back to a full watermark re-list.
    pub async fn history_since(
        &self,
        account: &str,
        start_history_id: &str,
        page_token: Option<&str>,
        max_results: usize,
    ) -> Result<GmailHistoryPage, GwsGmailError> {
        let mut params = json!({
            "userId": "me",
            "startHistoryId": start_history_id,
            "maxResults": max_results.clamp(1, 500),
        });
        if let Some(token) = page_token {
            params["pageToken"] = json!(token);
        }
        let stdout = self
            .run_gmail(account, &["history", "list"], params)
            .await?;
        parse_history_response(&stdout)
    }

    /// `messages get` in FULL format — the message's MIME payload tree
    /// for the local-distillation content path (Phase 1b N2). The result
    /// carries body content and must never be persisted; see
    /// [`GmailFullMessage`] for the in-memory-only contract. Consumed by
    /// `super::assist::content::extract_text` on the way into the N3 distill
    /// queue.
    pub async fn get_message_full(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailFullMessage, GwsGmailError> {
        let params = json!({
            "userId": "me",
            "id": message_id,
            "format": "full",
        });
        let stdout = self
            .run_gmail(account, &["messages", "get"], params)
            .await?;
        parse_full_message(&stdout)
    }

    /// `messages get` in metadata format, read for automatic
    /// verification-code retrieval (secure HITL P6): the provider's own
    /// receive time and the `From`, `Subject` and authentication headers —
    /// enough to tell whether a message is inside a challenge's window and
    /// from its sender before any body is fetched.
    pub async fn get_message_headers_for_verification(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationHeaders, GwsGmailError> {
        let params = json!({
            "userId": "me",
            "id": message_id,
            "format": "metadata",
            "metadataHeaders": VERIFICATION_HEADERS,
        });
        let stdout = self
            .run_gmail(account, &["messages", "get"], params)
            .await?;
        parse_verification_headers(&stdout)
    }

    /// `messages get` in full format, read for automatic verification-code
    /// retrieval (secure HITL P6): the provider's own receive time, the
    /// `From`, `Subject` and `Authentication-Results` headers and the MIME
    /// tree. In memory only; the caller extracts one code and drops it.
    pub async fn get_message_for_verification(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationMessage, GwsGmailError> {
        let params = json!({
            "userId": "me",
            "id": message_id,
            "format": "full",
        });
        let stdout = self
            .run_gmail(account, &["messages", "get"], params)
            .await?;
        parse_verification_message(&stdout)
    }

    /// `getProfile` — account email + the mailbox's current history id
    /// (the baseline cursor for incremental sync).
    pub async fn get_profile(&self, account: &str) -> Result<GmailProfile, GwsGmailError> {
        let stdout = self
            .run_gmail(account, &["getProfile"], json!({"userId": "me"}))
            .await?;
        parse_profile(&stdout)
    }

    /// Convenience for backfill: list up to `max_messages` message refs
    /// matching `query`, then fetch metadata for each DISTINCT thread they
    /// belong to. Returned threads include ALL their messages (Gmail
    /// returns whole threads), which may reach outside the query window —
    /// the store dedups by message id, and full threads are exactly what
    /// thread grouping wants. Fails fast on the first thread error so the
    /// caller's per-account isolation sees a real error, not a partial
    /// silent result.
    pub async fn list_message_metadata(
        &self,
        account: &str,
        query: Option<&str>,
        max_messages: u32,
    ) -> Result<Vec<GmailThreadMeta>, GwsGmailError> {
        let mut refs: Vec<GmailMessageRef> = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let remaining = (max_messages as usize).saturating_sub(refs.len());
            if remaining == 0 {
                break;
            }
            let page = self
                .list_message_refs(account, query, remaining as u32, page_token.as_deref())
                .await?;
            refs.extend(page.refs);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        refs.truncate(max_messages as usize);

        // Distinct thread ids in first-seen order.
        let mut seen = HashSet::new();
        let mut thread_ids = Vec::new();
        for r in &refs {
            if seen.insert(r.thread_id.clone()) {
                thread_ids.push(r.thread_id.clone());
            }
        }

        let mut threads = Vec::with_capacity(thread_ids.len());
        for thread_id in &thread_ids {
            threads.push(self.get_thread_metadata(account, thread_id).await?);
        }
        Ok(threads)
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// Raw CLI JSON shapes (private — full recipient lists must not leave this
// module, and `snippet` is deliberately never deserialized).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListResponse {
    #[serde(default)]
    messages: Vec<RawMessageRef>,
    next_page_token: Option<String>,
    result_size_estimate: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessageRef {
    id: String,
    thread_id: String,
}

/// A message as returned by `format=metadata` calls. Field selection IS the
/// privacy filter: `snippet` and any body/attachment fields are absent here
/// on purpose, so they are dropped at parse time.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmailMessageMetaRaw {
    id: String,
    thread_id: String,
    history_id: Option<String>,
    #[serde(default)]
    label_ids: Vec<String>,
    /// Epoch millis, returned by the API as a STRING (probe-confirmed).
    internal_date: Option<String>,
    payload: Option<RawPayload>,
}

#[derive(Debug, Deserialize)]
struct RawPayload {
    #[serde(default)]
    headers: Vec<RawHeader>,
}

#[derive(Debug, Deserialize)]
struct RawHeader {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    id: String,
    history_id: Option<String>,
    #[serde(default)]
    messages: Vec<GmailMessageMetaRaw>,
}

/// A message as returned by `format=full`. Field selection again IS the
/// filter: `snippet`, `labelIds`, and header values are not deserialized
/// on this path — only the identity pair and the MIME body tree that the
/// content extractor needs.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFullMessage {
    id: String,
    thread_id: String,
    payload: Option<GmailMimePart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryResponse {
    #[serde(default)]
    history: Vec<RawHistoryRecord>,
    history_id: Option<String>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryRecord {
    id: Option<String>,
    #[serde(default)]
    messages_added: Vec<RawMessageWrapper>,
    #[serde(default)]
    messages_deleted: Vec<RawMessageWrapper>,
    #[serde(default)]
    labels_added: Vec<RawLabelChangeWrapper>,
    #[serde(default)]
    labels_removed: Vec<RawLabelChangeWrapper>,
}

#[derive(Debug, Deserialize)]
struct RawMessageWrapper {
    message: Option<RawMessageRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLabelChangeWrapper {
    message: Option<RawMessageRef>,
    #[serde(default)]
    label_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProfile {
    email_address: Option<String>,
    history_id: Option<String>,
    messages_total: Option<u64>,
}

// ---------------------------------------------------------------------------
// Parsers (pure — unit tested on synthetic fixtures matching the probed
// shapes).
// ---------------------------------------------------------------------------

fn parse_list_response(stdout: &[u8]) -> Result<GmailMessageRefPage, GwsGmailError> {
    let raw: RawListResponse = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws messages list output: {e}")))?;
    Ok(GmailMessageRefPage {
        refs: raw
            .messages
            .into_iter()
            .map(|m| GmailMessageRef {
                message_id: m.id,
                thread_id: m.thread_id,
            })
            .collect(),
        next_page_token: raw.next_page_token,
        result_size_estimate: raw.result_size_estimate,
    })
}

fn parse_thread_metadata(
    stdout: &[u8],
    account_alias: &str,
    observed_at: i64,
) -> Result<GmailThreadMeta, GwsGmailError> {
    let raw: RawThread = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws threads get output: {e}")))?;
    let mut messages = Vec::with_capacity(raw.messages.len());
    for m in raw.messages {
        messages.push(convert_message(m, account_alias, observed_at)?);
    }
    Ok(GmailThreadMeta {
        thread_id: raw.id,
        history_id: raw.history_id,
        messages,
    })
}

fn parse_history_response(stdout: &[u8]) -> Result<GmailHistoryPage, GwsGmailError> {
    let raw: RawHistoryResponse = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws history list output: {e}")))?;
    let mut added = Vec::new();
    let mut seen_added = HashSet::new();
    let mut changes = Vec::new();
    let mut seen_changes = HashSet::new();
    for record in raw.history {
        let history_id = record.id.clone();
        for wrapper in record.messages_added {
            if let Some(m) = wrapper.message {
                let message = GmailMessageRef {
                    message_id: m.id,
                    thread_id: m.thread_id,
                };
                if seen_added.insert(message.message_id.clone()) {
                    added.push(message.clone());
                }
                push_history_change(
                    &mut changes,
                    &mut seen_changes,
                    history_id.as_deref(),
                    GmailHistoryChangeKind::MessageAdded,
                    message,
                    Vec::new(),
                );
            }
        }
        for wrapper in record.messages_deleted {
            if let Some(m) = wrapper.message {
                push_history_change(
                    &mut changes,
                    &mut seen_changes,
                    history_id.as_deref(),
                    GmailHistoryChangeKind::MessageDeleted,
                    GmailMessageRef {
                        message_id: m.id,
                        thread_id: m.thread_id,
                    },
                    Vec::new(),
                );
            }
        }
        for wrapper in record.labels_added {
            if let Some(m) = wrapper.message {
                push_history_change(
                    &mut changes,
                    &mut seen_changes,
                    history_id.as_deref(),
                    GmailHistoryChangeKind::LabelsAdded,
                    GmailMessageRef {
                        message_id: m.id,
                        thread_id: m.thread_id,
                    },
                    wrapper.label_ids,
                );
            }
        }
        for wrapper in record.labels_removed {
            if let Some(m) = wrapper.message {
                push_history_change(
                    &mut changes,
                    &mut seen_changes,
                    history_id.as_deref(),
                    GmailHistoryChangeKind::LabelsRemoved,
                    GmailMessageRef {
                        message_id: m.id,
                        thread_id: m.thread_id,
                    },
                    wrapper.label_ids,
                );
            }
        }
    }
    Ok(GmailHistoryPage {
        latest_history_id: raw.history_id,
        added,
        changes,
        next_page_token: raw.next_page_token,
    })
}

fn push_history_change(
    changes: &mut Vec<GmailHistoryChange>,
    seen: &mut HashSet<String>,
    history_id: Option<&str>,
    kind: GmailHistoryChangeKind,
    message: GmailMessageRef,
    mut label_ids: Vec<String>,
) {
    label_ids.sort();
    label_ids.dedup();
    let key = format!(
        "{}:{kind:?}:{}:{}:{}",
        history_id.unwrap_or_default(),
        message.thread_id,
        message.message_id,
        label_ids.join(",")
    );
    if seen.insert(key) {
        changes.push(GmailHistoryChange {
            history_id: history_id.map(str::to_string),
            kind,
            message,
            label_ids,
        });
    }
}

fn parse_full_message(stdout: &[u8]) -> Result<GmailFullMessage, GwsGmailError> {
    let raw: RawFullMessage = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws messages get output: {e}")))?;
    Ok(GmailFullMessage {
        message_id: raw.id,
        thread_id: raw.thread_id,
        payload: raw.payload,
    })
}

/// A full message as verification-code retrieval reads it. Deliberately not
/// `Serialize` and `Debug`-redacted: the body must never leave the watcher.
pub struct GmailVerificationMessage {
    pub message_id: String,
    pub thread_id: String,
    /// `internalDate` — when Gmail received it, in epoch millis.
    pub internal_date_ms: i64,
    pub from: Option<String>,
    pub subject: Option<String>,
    /// The plain `Authentication-Results` header(s) as they arrived; only
    /// the ones Gmail's own MX wrote are a verdict (see
    /// [`sender_authentication_verdict`]).
    pub authentication_results: Vec<String>,
    pub payload: Option<GmailMimePart>,
}

impl std::fmt::Debug for GmailVerificationMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailVerificationMessage")
            .field("message_id", &self.message_id)
            .field("internal_date_ms", &self.internal_date_ms)
            .field("authentication_results", &self.authentication_results.len())
            .finish_non_exhaustive()
    }
}

impl GmailVerificationMessage {
    /// The provider's verdict on the sender, aligned with the `From`
    /// domain (see [`sender_authentication_verdict`]).
    pub fn sender_authenticated(&self) -> Option<bool> {
        sender_authentication_verdict(&self.authentication_results, self.from.as_deref())
    }
}

/// The headers of a message a verification-code read starts with.
/// `Debug`-redacted like the full message: the subject is never printed.
pub struct GmailVerificationHeaders {
    pub message_id: String,
    /// `internalDate` — when Gmail received it, in epoch millis.
    pub internal_date_ms: i64,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub authentication_results: Vec<String>,
}

impl std::fmt::Debug for GmailVerificationHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailVerificationHeaders")
            .field("message_id", &self.message_id)
            .field("internal_date_ms", &self.internal_date_ms)
            .field("authentication_results", &self.authentication_results.len())
            .finish_non_exhaustive()
    }
}

impl GmailVerificationHeaders {
    pub fn sender_authenticated(&self) -> Option<bool> {
        sender_authentication_verdict(&self.authentication_results, self.from.as_deref())
    }
}

/// Whether Gmail's own receiving server authenticated the message *as the
/// sender it claims*: `dmarc=pass` for the same `From` the matcher reads, or
/// `dkim=pass` / `spf=pass` whose signing or envelope domain shares that
/// `From`'s registrable domain (DMARC's relaxed alignment).
///
/// # Only the first `mx.google.com` header counts, and a second one refuses
///
/// A verdict is only worth anything if the receiver wrote it. Gmail's MX
/// prepends its own `Authentication-Results`, so **the first** header
/// claiming the `mx.google.com` authserv-id is Gmail's; anything below it
/// arrived with the message. Walking the whole list and taking any pass was
/// forgeable two ways, neither of them hypothetical: a sender can include a
/// header claiming Gmail's own authserv-id (RFC 8601 says a receiver SHOULD
/// strip those, which is not a guarantee), and Gmail-to-Gmail forwarding
/// preserves the upstream hop's genuine pass while the new hop's SPF fails —
/// so DMARC-failing forwarded mail read as authenticated. A second header
/// claiming `mx.google.com` therefore makes the provenance ambiguous and the
/// verdict `Some(false)`: refusing costs the person one typed code, while
/// accepting hands an attacker's digits to their bank.
///
/// `dmarc=pass` is only accepted for an aligned `header.from`. DMARC is
/// evaluated against the `From` *Gmail* saw; this reads the first `From`
/// header for the same reason, so the verdict and the identity the matcher
/// checks cannot refer to two different senders.
///
/// `Some(false)` when Gmail's header is there but nothing aligned passed;
/// `None` when Gmail wrote no verdict at all (the caller decides what an
/// absent verdict is worth — see `matching::judge`, which refuses it for any
/// source that can authenticate a sender).
pub fn sender_authentication_verdict(headers: &[String], from: Option<&str>) -> Option<bool> {
    let from_domain = from
        .map(super::verification_sources::gmail::address_of)
        .and_then(|address| address.rsplit('@').next().map(str::to_string))
        .and_then(|domain| registrable_domain(&domain));
    let mut gmail_verdicts = headers.iter().filter_map(|header| gmail_results(header));
    let Some(results) = gmail_verdicts.next() else {
        return None;
    };
    if gmail_verdicts.next().is_some() {
        // Two headers claim the receiver's own identity: one of them is not
        // the receiver's. Nothing here can tell which.
        return Some(false);
    }
    for clause in results.split(';') {
        let clause = clause.trim();
        let lower = clause.to_ascii_lowercase();
        let Some(method) = lower.split_whitespace().next() else {
            continue;
        };
        let aligned = |property: &str| -> bool {
            let Some(from_domain) = from_domain.as_deref() else {
                return false;
            };
            property_value(&lower, property)
                .and_then(|value| {
                    registrable_domain(
                        value
                            .trim_start_matches('@')
                            .rsplit('@')
                            .next()
                            .unwrap_or(value),
                    )
                })
                .is_some_and(|domain| domain == from_domain)
        };
        match method {
            "dmarc=pass" if aligned("header.from") => return Some(true),
            "dkim=pass" if aligned("header.i") || aligned("header.d") => return Some(true),
            "spf=pass" if aligned("smtp.mailfrom") => return Some(true),
            _ => {},
        }
    }
    Some(false)
}

/// The result clauses of a plain `Authentication-Results` header Gmail's
/// MX wrote, or `None` for any other authserv-id (an ARC instance
/// `i=1; …` is never one).
fn gmail_results(header: &str) -> Option<&str> {
    let (authserv, results) = header.trim().split_once(';')?;
    let authserv = authserv.split_whitespace().next().unwrap_or_default();
    authserv
        .eq_ignore_ascii_case(GMAIL_AUTHSERV_ID)
        .then_some(results)
}

/// `header.i=@example.test` → `@example.test` from one lowercased clause.
fn property_value<'a>(clause: &'a str, property: &str) -> Option<&'a str> {
    let start = clause.find(&format!("{property}="))? + property.len() + 1;
    let value = &clause[start..];
    let end = value
        .find(|c: char| c.is_whitespace() || c == ';' || c == '(')
        .unwrap_or(value.len());
    Some(&value[..end])
}

fn registrable_domain(domain: &str) -> Option<String> {
    magician::magician_v2::verification_codes::registrable_domain(domain)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVerificationMessage {
    id: String,
    internal_date: Option<String>,
    payload: Option<RawVerificationPayload>,
}

fn parse_verification_headers(stdout: &[u8]) -> Result<GmailVerificationHeaders, GwsGmailError> {
    let raw: RawVerificationMessage = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws messages get output: {e}")))?;
    let internal_date_ms = raw
        .internal_date
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| {
            GwsGmailError::other(format!("gmail message {} missing internalDate", raw.id))
        })?;
    let (from, subject, authentication_results) =
        verification_headers(raw.payload.map(|p| p.headers).unwrap_or_default());
    Ok(GmailVerificationHeaders {
        message_id: raw.id,
        internal_date_ms,
        from,
        subject,
        authentication_results,
    })
}

fn verification_headers(headers: Vec<RawHeader>) -> (Option<String>, Option<String>, Vec<String>) {
    let mut from = None;
    let mut subject = None;
    let mut authentication_results = Vec::new();
    for header in headers {
        if header.name.eq_ignore_ascii_case("From") {
            // The FIRST `From`, for the same reason the first
            // `Authentication-Results` is the one that counts: that is the
            // identity the receiver evaluated. A later duplicate is the
            // sender's, and letting it win would let the verdict and the
            // identity refer to two different senders.
            from = from.or(Some(header.value));
        } else if header.name.eq_ignore_ascii_case("Subject") {
            subject = Some(header.value);
        } else if header.name.eq_ignore_ascii_case("Authentication-Results") {
            authentication_results.push(header.value);
        }
    }
    (from, subject, authentication_results)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVerificationPayload {
    #[serde(default)]
    headers: Vec<RawHeader>,
}

fn parse_verification_message(stdout: &[u8]) -> Result<GmailVerificationMessage, GwsGmailError> {
    let full = parse_full_message(stdout)?;
    let raw: RawVerificationMessage = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws messages get output: {e}")))?;
    let internal_date_ms = raw
        .internal_date
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| {
            GwsGmailError::other(format!("gmail message {} missing internalDate", raw.id))
        })?;
    let (from, subject, authentication_results) =
        verification_headers(raw.payload.map(|p| p.headers).unwrap_or_default());
    Ok(GmailVerificationMessage {
        message_id: full.message_id,
        thread_id: full.thread_id,
        internal_date_ms,
        from,
        subject,
        authentication_results,
        payload: full.payload,
    })
}

fn parse_profile(stdout: &[u8]) -> Result<GmailProfile, GwsGmailError> {
    let raw: RawProfile = serde_json::from_slice(stdout)
        .map_err(|e| GwsGmailError::other(format!("parse gws getProfile output: {e}")))?;
    Ok(GmailProfile {
        email_address: raw.email_address,
        history_id: raw.history_id,
        messages_total: raw.messages_total,
    })
}

/// Convert one raw metadata message to the M1 contract type. This is the
/// privacy choke point: To/Cc header values are reduced to domains HERE
/// and the full addresses are dropped. `sensitive_suppressed` is left
/// false — suppression heuristics are the sync worker's job at ingest.
fn convert_message(
    raw: GmailMessageMetaRaw,
    account_alias: &str,
    observed_at: i64,
) -> Result<MailMessageMeta, GwsGmailError> {
    let internal_date = raw
        .internal_date
        .as_deref()
        .ok_or_else(|| {
            GwsGmailError::other(format!("gmail message {} missing internalDate", raw.id))
        })?
        .parse::<i64>()
        .map_err(|e| {
            GwsGmailError::other(format!("gmail message {} bad internalDate: {e}", raw.id))
        })?;

    let mut subject = None;
    let mut from_raw: Option<String> = None;
    let mut to_domains = Vec::new();
    let mut cc_domains = Vec::new();
    for header in raw.payload.map(|p| p.headers).unwrap_or_default() {
        if header.name.eq_ignore_ascii_case("Subject") {
            subject = Some(header.value);
        } else if header.name.eq_ignore_ascii_case("From") {
            from_raw = Some(header.value);
        } else if header.name.eq_ignore_ascii_case("To") {
            to_domains.extend(recipient_domains(&header.value));
        } else if header.name.eq_ignore_ascii_case("Cc") {
            cc_domains.extend(recipient_domains(&header.value));
        }
    }
    dedup_preserving_order(&mut to_domains);
    dedup_preserving_order(&mut cc_domains);
    let (from_name, from_address) = match from_raw.as_deref() {
        Some(v) => parse_single_address(v),
        None => (None, None),
    };

    Ok(MailMessageMeta {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: "gmail".to_string(),
        account_alias: account_alias.to_string(),
        account_email: None,
        thread_id: raw.thread_id,
        message_id: raw.id,
        provider_cursor: raw.history_id,
        label_ids: raw.label_ids,
        subject,
        from_name,
        from_address,
        to_domains,
        cc_domains,
        internal_date,
        observed_at,
        // Direction/lane stamping and suppression are the sync worker's
        // job at ingest; rows leave here pending distillation with no
        // derived fields.
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
    })
}

pub fn dedup_preserving_order(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|v| seen.insert(v.clone()));
}

// ---------------------------------------------------------------------------
// Address parsing — small, deliberately conservative helpers. Anything that
// doesn't look like a domain is SKIPPED rather than guessed at (a missed
// domain is harmless; a mangled one pollutes analytics).
// ---------------------------------------------------------------------------

/// Split an RFC-5322-ish address-list header value on commas, respecting
/// double-quoted display names (`"Doe, Jane" <j@x.example>`) and
/// angle-bracket sections.
fn split_address_list(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut angle_depth = 0usize;
    for c in value.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            },
            '<' if !in_quotes => {
                angle_depth += 1;
                current.push(c);
            },
            '>' if !in_quotes => {
                angle_depth = angle_depth.saturating_sub(1);
                current.push(c);
            },
            ',' if !in_quotes && angle_depth == 0 => {
                parts.push(std::mem::take(&mut current));
            },
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The addr-spec of one address-list entry: the text inside `<…>` when
/// present, otherwise the whole trimmed entry.
fn addr_spec(entry: &str) -> String {
    let entry = entry.trim();
    if let Some(lt) = entry.rfind('<') {
        let rest = &entry[lt + 1..];
        rest.split('>').next().unwrap_or(rest).trim().to_string()
    } else {
        entry.to_string()
    }
}

/// The lowercased domain of one address-list entry, or None when the entry
/// doesn't carry a plausible domain. Rules: take the text after the LAST
/// `@` of the addr-spec; require non-empty, no whitespace, no `@`, and at
/// least one `.` (drops bare hosts and garbage).
fn address_domain(entry: &str) -> Option<String> {
    let spec = addr_spec(entry);
    let at = spec.rfind('@')?;
    // RFC-5322 group syntax terminates the list with `;` — strip it before
    // validation so the final group member's domain isn't stored mangled.
    let domain = spec[at + 1..]
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains(char::is_whitespace)
        || domain.contains('@')
        || !domain.contains('.')
    {
        return None;
    }
    Some(domain)
}

/// Reduce a To/Cc header value to the DISTINCT domains of its addresses,
/// in first-seen order. The full addresses never leave this function.
pub fn recipient_domains(header_value: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in split_address_list(header_value) {
        if let Some(domain) = address_domain(&entry) {
            if seen.insert(domain.clone()) {
                out.push(domain);
            }
        }
    }
    out
}

/// Parse a single From-style mailbox into (display name, address).
/// `Name <a@b.example>` → both; bare `a@b.example` → address only; a
/// value with no `@` anywhere → name only.
pub fn parse_single_address(value: &str) -> (Option<String>, Option<String>) {
    let value = value.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Some(lt) = value.rfind('<') {
        let spec = addr_spec(value);
        let name = value[..lt].trim().trim_matches('"').trim();
        let name = (!name.is_empty()).then(|| name.to_string());
        let address = spec.contains('@').then_some(spec);
        (name, address)
    } else if value.contains('@') {
        (None, Some(value.to_string()))
    } else {
        (Some(value.trim_matches('"').to_string()), None)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // All fixtures are SYNTHETIC, matching the probed CLI shapes.

    fn verification_fixture(authentication: &str) -> Vec<u8> {
        format!(
            r#"{{
            "id": "vvv111",
            "threadId": "ttt111",
            "internalDate": "1719990000000",
            "snippet": "must never be read",
            "payload": {{
                "mimeType": "text/plain",
                "headers": [
                    {{"name": "From", "value": "Example Security <no-reply@accounts.example.test>"}},
                    {{"name": "Subject", "value": "Your code"}},
                    {{"name": "Authentication-Results", "value": "{authentication}"}}
                ],
                "body": {{"size": 30, "data": "WW91ciB2ZXJpZmljYXRpb24gY29kZSBpcyAwNDI5MTc="}}
            }}
        }}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_verification_read_carries_the_receive_time_the_sender_and_the_verdict() {
        let stdout = verification_fixture("mx.google.com; dkim=pass header.i=@example.test header.s=s1; spf=pass smtp.mailfrom=bounce@mail.example.test; dmarc=pass header.from=accounts.example.test");
        let headers = parse_verification_headers(&stdout).unwrap();
        assert_eq!(headers.message_id, "vvv111");
        assert_eq!(headers.internal_date_ms, 1_719_990_000_000);
        assert_eq!(
            headers.from.as_deref(),
            Some("Example Security <no-reply@accounts.example.test>")
        );
        assert_eq!(headers.subject.as_deref(), Some("Your code"));
        assert_eq!(headers.sender_authenticated(), Some(true));
        assert!(!format!("{headers:?}").contains("Your code"));
        let full = parse_verification_message(&stdout).unwrap();
        assert_eq!(full.internal_date_ms, headers.internal_date_ms);
        assert_eq!(full.sender_authenticated(), Some(true));
        assert!(full.payload.is_some());
        assert!(!format!("{full:?}").contains("Your code"));
        let missing_date = br#"{"id": "x", "threadId": "t", "payload": {"headers": []}}"#;
        assert!(parse_verification_headers(missing_date).is_err());
    }

    #[test]
    fn only_gmails_own_aligned_verdict_authenticates_the_sender() {
        let from = Some("Example Security <no-reply@accounts.example.test>");
        let verdict = |header: &str| sender_authentication_verdict(&[header.to_string()], from);
        assert_eq!(
            verdict("mx.google.com; dmarc=pass (p=REJECT) header.from=accounts.example.test"),
            Some(true)
        );
        assert_eq!(
            verdict("mx.google.com; dkim=pass header.i=@mail.example.test header.s=k1; spf=none"),
            Some(true),
            "a DKIM signature by a sibling of the From domain aligns"
        );
        assert_eq!(
            verdict("mx.google.com; dkim=pass header.d=example.test; dmarc=fail"),
            Some(true),
            "DKIM alignment by header.d"
        );
        assert_eq!(
            verdict(
                "mx.google.com; spf=pass (sender ip) smtp.mailfrom=bounce@example.test; dkim=none"
            ),
            Some(true),
            "SPF alignment by the envelope domain"
        );
        // A valid signature for another domain says nothing about the From.
        assert_eq!(verdict("mx.google.com; dkim=pass header.i=@attacker.example; spf=pass smtp.mailfrom=x@attacker.example; dmarc=fail header.from=accounts.example.test"), Some(false));
        assert_eq!(
            verdict("mx.google.com; dkim=fail; spf=softfail; dmarc=fail"),
            Some(false)
        );
        // A header the sender wrote is not Gmail's verdict.
        assert_eq!(
            verdict("attacker.example; dkim=pass header.i=@accounts.example.test; dmarc=pass"),
            None
        );
        // An ARC set travels intact from the sender: never a verdict, even naming Gmail.
        assert_eq!(
            verdict("i=1; mx.google.com; dmarc=pass header.from=accounts.example.test"),
            None
        );
        let forged_arc = br#"{"id": "a", "threadId": "t", "internalDate": "1", "payload": {"headers": [
            {"name": "From", "value": "no-reply@accounts.example.test"},
            {"name": "ARC-Authentication-Results", "value": "i=1; mx.google.com; dmarc=pass header.from=accounts.example.test"}
        ]}}"#;
        assert_eq!(
            parse_verification_headers(forged_arc)
                .unwrap()
                .sender_authenticated(),
            None,
            "ARC headers are not even collected"
        );
        assert_eq!(sender_authentication_verdict(&[], from), None);
        assert_eq!(
            sender_authentication_verdict(
                &["mx.google.com; dkim=pass header.i=@example.test".to_string()],
                None
            ),
            Some(false),
            "no From to align with"
        );
        // Several headers: any aligned pass from Gmail wins, an attacker's own header adds nothing.
        assert_eq!(
            sender_authentication_verdict(
                &[
                    "attacker.example; dmarc=pass".to_string(),
                    "mx.google.com; dkim=fail; dmarc=fail".to_string()
                ],
                from
            ),
            Some(false)
        );
    }

    #[test]
    fn parses_message_list_page() {
        let fixture = br#"{
            "messages": [
                {"id": "aaa111", "threadId": "ttt111"},
                {"id": "bbb222", "threadId": "ttt111"}
            ],
            "nextPageToken": "tok-1",
            "resultSizeEstimate": 42
        }"#;
        let page = parse_list_response(fixture).expect("parse list");
        assert_eq!(page.refs.len(), 2);
        assert_eq!(page.refs[0].message_id, "aaa111");
        assert_eq!(page.refs[0].thread_id, "ttt111");
        assert_eq!(page.next_page_token.as_deref(), Some("tok-1"));
        assert_eq!(page.result_size_estimate, Some(42));
    }

    #[test]
    fn parses_empty_list_without_messages_key() {
        let page = parse_list_response(br#"{"resultSizeEstimate": 0}"#).expect("parse empty");
        assert!(page.refs.is_empty());
        assert!(page.next_page_token.is_none());
    }

    fn thread_fixture() -> Vec<u8> {
        // Mirrors the probed threads.get(format=metadata) shape, including
        // the snippet field the API returns even in metadata format — the
        // parser must drop it.
        br#"{
            "id": "ttt111",
            "historyId": "90001",
            "messages": [
                {
                    "id": "aaa111",
                    "threadId": "ttt111",
                    "historyId": "90000",
                    "labelIds": ["INBOX", "UNREAD"],
                    "internalDate": "1719990000000",
                    "sizeEstimate": 4321,
                    "snippet": "must never be stored",
                    "payload": {
                        "mimeType": "multipart/alternative",
                        "headers": [
                            {"name": "Subject", "value": "Quarterly sync"},
                            {"name": "From", "value": "Sender Person <sender@alpha.example>"},
                            {"name": "To", "value": "\"Doe, Jane\" <jane@beta.example>, bob@gamma.example"},
                            {"name": "Cc", "value": "Team List <team@beta.example>"}
                        ]
                    }
                }
            ]
        }"#
        .to_vec()
    }

    #[test]
    fn thread_metadata_converts_to_domains_only() {
        let thread = parse_thread_metadata(&thread_fixture(), "business", 1_720_000_000_000)
            .expect("parse thread");
        assert_eq!(thread.thread_id, "ttt111");
        assert_eq!(thread.history_id.as_deref(), Some("90001"));
        assert_eq!(thread.messages.len(), 1);

        let m = &thread.messages[0];
        assert_eq!(m.provider, "gmail");
        assert_eq!(m.account_alias, "business");
        assert_eq!(m.message_id, "aaa111");
        assert_eq!(m.thread_id, "ttt111");
        assert_eq!(m.provider_cursor.as_deref(), Some("90000"));
        assert_eq!(m.label_ids, vec!["INBOX", "UNREAD"]);
        assert_eq!(m.subject.as_deref(), Some("Quarterly sync"));
        assert_eq!(m.from_name.as_deref(), Some("Sender Person"));
        assert_eq!(m.from_address.as_deref(), Some("sender@alpha.example"));
        // The quoted display name contains a comma — the splitter must not
        // break on it, and only DOMAINS may survive conversion.
        assert_eq!(m.to_domains, vec!["beta.example", "gamma.example"]);
        assert_eq!(m.cc_domains, vec!["beta.example"]);
        assert_eq!(m.internal_date, 1_719_990_000_000);
        assert_eq!(m.observed_at, 1_720_000_000_000);
        assert!(!m.sensitive_suppressed);
        assert_eq!(m.origin, MailRecordOrigin::MetadataSync);

        // Privacy: neither the snippet nor any full recipient address may
        // appear anywhere in the serialized contract type.
        let serialized = serde_json::to_string(m).expect("serialize meta");
        assert!(!serialized.contains("must never be stored"));
        assert!(!serialized.contains("jane@"));
        assert!(!serialized.contains("bob@"));
        assert!(!serialized.contains("team@"));
    }

    #[test]
    fn message_without_optional_fields_still_converts() {
        let fixture = br#"{
            "id": "ttt222",
            "messages": [
                {
                    "id": "ccc333",
                    "threadId": "ttt222",
                    "internalDate": "1719990001000"
                }
            ]
        }"#;
        let thread = parse_thread_metadata(fixture, "personal", 1).expect("parse minimal");
        let m = &thread.messages[0];
        assert!(m.subject.is_none());
        assert!(m.from_name.is_none());
        assert!(m.from_address.is_none());
        assert!(m.to_domains.is_empty());
        assert!(m.cc_domains.is_empty());
        assert!(m.label_ids.is_empty());
        assert!(m.provider_cursor.is_none());
        assert!(thread.history_id.is_none());
    }

    #[test]
    fn missing_internal_date_is_an_error_not_a_panic() {
        let fixture = br#"{
            "id": "ttt333",
            "messages": [{"id": "ddd444", "threadId": "ttt333"}]
        }"#;
        let err = parse_thread_metadata(fixture, "business", 1).expect_err("must fail");
        assert!(err.detail.contains("internalDate"));
    }

    #[test]
    fn non_numeric_internal_date_is_an_error() {
        let fixture = br#"{
            "id": "ttt444",
            "messages": [
                {"id": "eee555", "threadId": "ttt444", "internalDate": "not-a-number"}
            ]
        }"#;
        assert!(parse_thread_metadata(fixture, "business", 1).is_err());
    }

    #[test]
    fn malformed_json_is_an_error_everywhere() {
        assert!(parse_list_response(b"{ nope").is_err());
        assert!(parse_thread_metadata(b"[]", "a", 1).is_err());
        assert!(parse_history_response(b"<html>gateway timeout</html>").is_err());
        assert!(parse_profile(b"").is_err());
    }

    #[test]
    fn parses_full_message_mime_tree() {
        // Synthetic format=full shape (probed via
        // `gws schema gmail.users.messages.get`): nested multipart/mixed →
        // multipart/alternative(plain, html) + an attachment part whose
        // body carries attachmentId instead of inline data. snippet and
        // attachmentId must be DROPPED at parse time.
        let fixture = br#"{
            "id": "aaa111",
            "threadId": "ttt111",
            "labelIds": ["INBOX"],
            "snippet": "full-format snippet must never be parsed",
            "historyId": "90000",
            "internalDate": "1719990000000",
            "sizeEstimate": 5432,
            "payload": {
                "partId": "",
                "mimeType": "multipart/mixed",
                "filename": "",
                "headers": [{"name": "Subject", "value": "Quarterly sync"}],
                "body": {"size": 0},
                "parts": [
                    {
                        "partId": "0",
                        "mimeType": "multipart/alternative",
                        "filename": "",
                        "body": {"size": 0},
                        "parts": [
                            {
                                "partId": "0.0",
                                "mimeType": "text/plain",
                                "filename": "",
                                "body": {"size": 20, "data": "Rml4dHVyZSBwbGFpbiBib2R5Lgo="}
                            },
                            {
                                "partId": "0.1",
                                "mimeType": "text/html",
                                "filename": "",
                                "body": {"size": 32, "data": "PHA-Rml4dHVyZSA8Yj5odG1sPC9iPiBib2R5LjwvcD4="}
                            }
                        ]
                    },
                    {
                        "partId": "1",
                        "mimeType": "application/pdf",
                        "filename": "agenda.pdf",
                        "body": {"size": 12345, "attachmentId": "att-opaque-id"}
                    }
                ]
            }
        }"#;
        let full = parse_full_message(fixture).expect("parse full message");
        assert_eq!(full.message_id, "aaa111");
        assert_eq!(full.thread_id, "ttt111");

        let payload = full.payload.as_ref().expect("payload");
        assert_eq!(payload.mime_type.as_deref(), Some("multipart/mixed"));
        assert_eq!(payload.parts.len(), 2);

        let alt = &payload.parts[0];
        assert_eq!(alt.mime_type.as_deref(), Some("multipart/alternative"));
        assert_eq!(alt.parts.len(), 2);
        assert_eq!(alt.parts[0].mime_type.as_deref(), Some("text/plain"));
        assert_eq!(
            alt.parts[0].body.as_ref().and_then(|b| b.data.as_deref()),
            Some("Rml4dHVyZSBwbGFpbiBib2R5Lgo=")
        );
        assert_eq!(alt.parts[1].mime_type.as_deref(), Some("text/html"));

        let attachment = &payload.parts[1];
        assert_eq!(attachment.filename.as_deref(), Some("agenda.pdf"));
        // The attachment's inline data is absent and its attachmentId is
        // not even a field on the parsed type.
        assert!(attachment.body.as_ref().unwrap().data.is_none());

        // snippet and attachmentId are structurally unrepresentable in
        // the parsed value — nothing in its debug rendering may leak them.
        let rendered = format!("{full:?}");
        assert!(!rendered.contains("full-format snippet"));
        assert!(!rendered.contains("att-opaque-id"));
    }

    #[test]
    fn full_message_without_payload_still_parses() {
        let full = parse_full_message(br#"{"id": "m1", "threadId": "t1"}"#).expect("parse");
        assert_eq!(full.message_id, "m1");
        assert!(full.payload.is_none());
        assert!(parse_full_message(b"{ nope").is_err());
    }

    #[test]
    fn parses_history_page_added_messages() {
        let fixture = br#"{
            "history": [
                {
                    "id": "90002",
                    "messagesAdded": [
                        {"message": {"id": "fff666", "threadId": "ttt555", "labelIds": ["INBOX"]}}
                    ]
                },
                {
                    "id": "90003",
                    "messagesAdded": [
                        {"message": {"id": "fff666", "threadId": "ttt555"}},
                        {"message": {"id": "ggg777", "threadId": "ttt666"}}
                    ]
                },
                {"id": "90004"}
            ],
            "historyId": "90010",
            "nextPageToken": "tok-h"
        }"#;
        let page = parse_history_response(fixture).expect("parse history");
        // Duplicate adds collapse; records without messagesAdded are fine.
        assert_eq!(page.added.len(), 2);
        assert_eq!(page.added[0].message_id, "fff666");
        assert_eq!(page.added[1].thread_id, "ttt666");
        assert_eq!(page.changes.len(), 3);
        assert!(page
            .changes
            .iter()
            .all(|change| change.kind == GmailHistoryChangeKind::MessageAdded));
        assert_eq!(page.latest_history_id.as_deref(), Some("90010"));
        assert_eq!(page.next_page_token.as_deref(), Some("tok-h"));
    }

    #[test]
    fn empty_history_response_yields_no_adds() {
        let page = parse_history_response(br#"{"historyId": "90010"}"#).expect("parse empty");
        assert!(page.added.is_empty());
        assert!(page.changes.is_empty());
        assert!(page.next_page_token.is_none());
    }

    #[test]
    fn parses_delete_archive_and_trash_history_changes() {
        let fixture = br#"{
            "history": [{
                "id": "91000",
                "messagesDeleted": [
                    {"message": {"id": "m-deleted", "threadId": "t-deleted"}}
                ],
                "labelsAdded": [
                    {"message": {"id": "m-trash", "threadId": "t-trash"}, "labelIds": ["TRASH"]}
                ],
                "labelsRemoved": [
                    {"message": {"id": "m-archive", "threadId": "t-archive"}, "labelIds": ["INBOX"]}
                ]
            }],
            "historyId": "91001"
        }"#;
        let page = parse_history_response(fixture).expect("parse changes");
        assert!(page.added.is_empty());
        assert_eq!(page.changes.len(), 3);
        assert_eq!(page.changes[0].kind, GmailHistoryChangeKind::MessageDeleted);
        assert_eq!(page.changes[1].label_ids, vec!["TRASH"]);
        assert_eq!(page.changes[2].kind, GmailHistoryChangeKind::LabelsRemoved);
        assert_eq!(page.changes[2].history_id.as_deref(), Some("91000"));
    }

    #[test]
    fn parses_profile() {
        let fixture = br#"{
            "emailAddress": "owner@alpha.example",
            "messagesTotal": 1234,
            "threadsTotal": 456,
            "historyId": "90010"
        }"#;
        let profile = parse_profile(fixture).expect("parse profile");
        assert_eq!(
            profile.email_address.as_deref(),
            Some("owner@alpha.example")
        );
        assert_eq!(profile.history_id.as_deref(), Some("90010"));
        assert_eq!(profile.messages_total, Some(1234));
    }

    #[test]
    fn domain_extraction_handles_bracket_and_bare_forms() {
        assert_eq!(
            recipient_domains("Person Name <a@one.example>"),
            vec!["one.example"]
        );
        assert_eq!(recipient_domains("a@one.example"), vec!["one.example"]);
        assert_eq!(
            recipient_domains("a@one.example, Second <b@two.example>, c@three.example"),
            vec!["one.example", "two.example", "three.example"]
        );
    }

    #[test]
    fn domain_extraction_respects_quoted_commas_and_dedups_case() {
        assert_eq!(
            recipient_domains(r#""Doe, Jane" <jane@one.example>, "Roe, R" <r@ONE.example>"#),
            vec!["one.example"]
        );
    }

    #[test]
    fn domain_extraction_skips_bad_addresses() {
        // No @, empty domain, dotless host, whitespace domain, group-name
        // only — all skipped, valid neighbor still extracted.
        assert_eq!(
            recipient_domains("nobody, bad@, x@localhost, y@bad domain, ok@fine.example"),
            vec!["fine.example"]
        );
        assert!(recipient_domains("").is_empty());
        assert!(recipient_domains("undisclosed-recipients:;").is_empty());
        // Populated group syntax: the trailing `;` is stripped from the
        // final member instead of polluting its domain.
        assert_eq!(
            recipient_domains("Team: a@one.example, b@two.example;"),
            vec!["one.example", "two.example"]
        );
    }

    #[test]
    fn from_parsing_covers_named_bare_and_nameless_forms() {
        assert_eq!(
            parse_single_address("Sender Person <s@one.example>"),
            (
                Some("Sender Person".to_string()),
                Some("s@one.example".to_string())
            )
        );
        assert_eq!(
            parse_single_address(r#""Quoted Name" <q@one.example>"#),
            (
                Some("Quoted Name".to_string()),
                Some("q@one.example".to_string())
            )
        );
        assert_eq!(
            parse_single_address("bare@one.example"),
            (None, Some("bare@one.example".to_string()))
        );
        assert_eq!(
            parse_single_address("Just A Name"),
            (Some("Just A Name".to_string()), None)
        );
        assert_eq!(parse_single_address("  "), (None, None));
        // Angle brackets with no plausible address inside → name only.
        assert_eq!(
            parse_single_address("Broken <not-an-address>"),
            (Some("Broken".to_string()), None)
        );
    }

    #[test]
    fn history_expired_maps_from_404() {
        let expired = GwsGmailError {
            detail: "gws gmail failed: Requested entity was not found.".to_string(),
            api_code: Some(404),
        };
        assert!(expired.history_expired());
        assert!(!GwsGmailError::other("timeout").history_expired());
    }
}
