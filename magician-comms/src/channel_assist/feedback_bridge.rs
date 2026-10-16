//! Feedback → memory bridge — turns the owner's channel Follow-up triage decisions
//! ("Do it" / "Acknowledged" / "Dismiss + reason") into a durable memory tier
//! (`user.channel_feedback`), so the memory system reflects what the owner acts on
//! vs merely acknowledges vs rejects (and why). This is the CONSUMER of the
//! feedback verdicts the Follow-up actions record — without it those verdicts
//! were write-only.
//!
//! Mirrors [`super::evidence_bridge`]: a per-scope watermark
//! (`mail_sync_watermarks` `__feedback_bridge__` row, cursor = feedback
//! `created_at`), day-keyed field writes so a re-run merges rather than
//! duplicates, and a best-effort background loop.
//!
//! Privacy: the bridged fields are the owner's own decisions + the classifier's
//! label/subject — never a raw body.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Map, Value};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use magician::magician_v2::agents::memory::AgentMemoryResolver;
use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::chat::service::merge_user_memory_tier_fields;
use magician::magician_v2::process_storage;

use super::store::{FeedbackBridgeRow, MailAssistStore};
use super::types::{SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION};

const LOG_TARGET: &str = "channel_assist::feedback_bridge";
const DEFAULT_INTERVAL_SECS: u64 = 300;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 200;
const FEEDBACK_BATCH: usize = 500;
/// Cap the per-group note list so a busy day can't bloat a tier row.
const MAX_NOTES_PER_GROUP: usize = 12;
const WATERMARK_KEY: &str = "__feedback_bridge__";
/// The tier this bridge writes — the shared tier-name contract (plan 3.1
/// prerequisite (b)), not a local string.
const TIER: &str = magician::magician_v2::evidence::tier_contracts::CHANNEL_FEEDBACK_TIER;

fn day_of(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// One tier row to write: the day/account field key + its fields.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackTierRow {
    pub field_key: String,
    pub fields: Value,
}

/// Triage bucket for a feedback row. The annotation STATE is authoritative
/// (the verdict alone can't tell "did" from "acknowledged" — both `helpful`);
/// the verdict is the fallback when the annotation is gone.
fn bucket_of(row: &FeedbackBridgeRow) -> &'static str {
    match row.state.as_deref() {
        Some("approved") => "did",
        Some("acknowledged") => "acknowledged",
        Some("dismissed") => "dismissed",
        _ => match row.verdict.as_str() {
            "helpful" => "acknowledged",
            "not_helpful" => "dismissed",
            _ => "other",
        },
    }
}

fn push_note(dst: &mut Vec<String>, note: String) {
    if dst.len() < MAX_NOTES_PER_GROUP && !dst.iter().any(|n| *n == note) {
        dst.push(note);
    }
}

/// Pure: aggregate feedback rows into per-`(account, day)` preference rows.
pub fn build_feedback_rows(rows: &[FeedbackBridgeRow]) -> Vec<FeedbackTierRow> {
    struct Group {
        account: String,
        day: String,
        did: u64,
        acknowledged: u64,
        dismissed: u64,
        dismiss_reasons: BTreeMap<String, u64>,
        notes: Vec<String>,
    }
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    for r in rows {
        let day = day_of(r.created_at);
        let g = groups
            .entry((r.account_alias.clone(), day.clone()))
            .or_insert_with(|| Group {
                account: r.account_alias.clone(),
                day: day.clone(),
                did: 0,
                acknowledged: 0,
                dismissed: 0,
                dismiss_reasons: BTreeMap::new(),
                notes: Vec::new(),
            });
        let subject = r
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("(thread)");
        match bucket_of(r) {
            "did" => {
                g.did += 1;
                push_note(&mut g.notes, format!("acted on: {subject}"));
            },
            "acknowledged" => {
                g.acknowledged += 1;
            },
            "dismissed" => {
                g.dismissed += 1;
                match r
                    .comment
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(reason) => {
                        *g.dismiss_reasons.entry(reason.to_string()).or_default() += 1;
                        push_note(&mut g.notes, format!("dismissed ({reason}): {subject}"));
                    },
                    None => push_note(&mut g.notes, format!("dismissed: {subject}")),
                }
            },
            _ => {},
        }
    }

    groups
        .into_values()
        .map(|g| {
            let reasons: Vec<String> = g
                .dismiss_reasons
                .iter()
                .map(|(k, v)| format!("{k}×{v}"))
                .collect();
            let summary = format!(
                "On {} for {}: acted on {}, acknowledged {}, dismissed {}{}.",
                g.day,
                g.account,
                g.did,
                g.acknowledged,
                g.dismissed,
                if reasons.is_empty() {
                    String::new()
                } else {
                    format!(" (reasons: {})", reasons.join(", "))
                },
            );
            let mut fields = Map::new();
            fields.insert("account".into(), json!(g.account));
            fields.insert("day".into(), json!(g.day));
            fields.insert("did".into(), json!(g.did));
            fields.insert("acknowledged".into(), json!(g.acknowledged));
            fields.insert("dismissed".into(), json!(g.dismissed));
            fields.insert("dismiss_reasons".into(), json!(g.dismiss_reasons));
            fields.insert("summary".into(), json!(summary));
            fields.insert("notes".into(), json!(g.notes));
            FeedbackTierRow {
                field_key: format!("feedback:{}:{}", g.account, g.day),
                fields: Value::Object(fields),
            }
        })
        .collect()
}

/// One bridge pass for a scope: read watermark → feedback rows → build tier
/// rows → merge into `user.channel_feedback` → advance the watermark.
pub async fn run_feedback_bridge_pass(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
) -> Result<usize> {
    let watermark = store
        .get_watermark(principal, workspace, WATERMARK_KEY, WATERMARK_KEY)
        .await?;
    let since = watermark
        .as_ref()
        .and_then(|w| w.last_internal_date)
        .unwrap_or(0);
    let after_cursor = watermark
        .as_ref()
        .and_then(|w| w.provider_cursor.as_deref());
    let rows = store
        .list_feedback_since(principal, workspace, since, after_cursor, FEEDBACK_BATCH)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let (max_ts, next_cursor) = rows
        .last()
        .map(|r| (r.created_at, r.bridge_cursor()))
        .unwrap_or((since, after_cursor.unwrap_or_default().to_string()));
    let tier_rows = build_feedback_rows(&rows);

    let resolver = AgentMemoryResolver::with_workspace_layout(process_storage::workspace());
    let mut written = 0usize;
    let mut failed = 0usize;
    for tr in &tier_rows {
        let mut fields = Map::new();
        fields.insert(tr.field_key.clone(), tr.fields.clone());
        let outcome =
            merge_user_memory_tier_fields(&resolver, principal, workspace, TIER, &fields).await;
        if outcome.get("status").and_then(Value::as_str) == Some("error") {
            let reason = outcome.get("reason").and_then(Value::as_str).unwrap_or("");
            warn!(target: LOG_TARGET, tier = TIER, reason, "feedback bridge tier write failed");
            failed += 1;
        } else {
            written += 1;
        }
    }

    if failed > 0 {
        warn!(
            target: LOG_TARGET,
            written,
            failed,
            max_ts,
            "feedback bridge pass kept watermark for retry"
        );
        return Ok(written);
    }

    // Advance only after writes so a failed pass re-runs idempotently.
    store
        .set_watermark(
            principal,
            workspace,
            SyncWatermark {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: WATERMARK_KEY.to_string(),
                account_alias: WATERMARK_KEY.to_string(),
                last_internal_date: Some(max_ts),
                provider_cursor: Some(next_cursor),
                last_synced_at: chrono::Utc::now().timestamp_millis(),
                last_error: None,
            },
        )
        .await?;
    debug!(
        target: LOG_TARGET,
        groups = tier_rows.len(),
        written,
        max_ts,
        "feedback bridge pass complete"
    );
    Ok(written)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

pub struct ChannelFeedbackBridgeWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelFeedbackBridgeWorker {
    /// `CHANNEL_FEEDBACK_BRIDGE_ENABLED` (default on) — the bin gates the spawn.
    pub fn enabled_from_env() -> bool {
        std::env::var("CHANNEL_FEEDBACK_BRIDGE_ENABLED")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true)
    }

    pub fn spawn(store: MailAssistStore) -> Self {
        let interval = Duration::from_secs(env_secs(
            "CHANNEL_FEEDBACK_BRIDGE_INTERVAL_SECS",
            DEFAULT_INTERVAL_SECS,
        ));
        let startup = Duration::from_secs(env_secs(
            "CHANNEL_FEEDBACK_BRIDGE_STARTUP_DELAY_SECS",
            DEFAULT_STARTUP_DELAY_SECS,
        ));
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel_for_task)
                .await
            {
                return;
            }
            tokio::select! {
                _ = cancel_for_task.cancelled() => return,
                _ = tokio::time::sleep(startup) => {},
            }
            let mut tick = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel_for_task.cancelled() => return,
                    _ = tick.tick() => {},
                }
                match run_feedback_bridge_pass(
                    &store,
                    DEFAULT_SCOPE_PRINCIPAL,
                    DEFAULT_SCOPE_WORKSPACE,
                )
                .await
                {
                    Ok(n) if n > 0 => {
                        debug!(target: LOG_TARGET, written = n, "feedback bridge tick wrote rows")
                    },
                    Ok(_) => {},
                    Err(error) => {
                        warn!(target: LOG_TARGET, error = %error, "feedback bridge tick failed")
                    },
                }
            }
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn fb(
        account: &str,
        ts: i64,
        state: &str,
        verdict: &str,
        comment: Option<&str>,
        subject: &str,
    ) -> FeedbackBridgeRow {
        FeedbackBridgeRow {
            provider: "gmail".to_string(),
            account_alias: account.to_string(),
            thread_id: format!("t{ts}"),
            created_at: ts,
            state: Some(state.to_string()),
            label: Some("needs_reply".to_string()),
            subject: Some(subject.to_string()),
            verdict: verdict.to_string(),
            comment: comment.map(str::to_string),
            event_id: format!("e{ts}"),
        }
    }

    const D5: i64 = 1_783_209_600_000;

    #[test]
    fn aggregates_buckets_and_reasons_per_account_day() {
        let rows = vec![
            fb("business", D5 + 1, "approved", "helpful", None, "Invoice"),
            fb("business", D5 + 2, "acknowledged", "helpful", None, "FYI"),
            fb(
                "business",
                D5 + 3,
                "dismissed",
                "not_helpful",
                Some("spam"),
                "Promo",
            ),
            fb(
                "business",
                D5 + 4,
                "dismissed",
                "not_helpful",
                Some("spam"),
                "Promo2",
            ),
        ];
        let out = build_feedback_rows(&rows);
        assert_eq!(out.len(), 1);
        let f = &out[0].fields;
        assert_eq!(out[0].field_key, "feedback:business:2026-07-05");
        assert_eq!(f["did"], json!(1));
        assert_eq!(f["acknowledged"], json!(1));
        assert_eq!(f["dismissed"], json!(2));
        assert_eq!(f["dismiss_reasons"]["spam"], json!(2));
    }

    #[test]
    fn falls_back_to_verdict_when_annotation_gone() {
        let mut r = fb("self", D5 + 1, "", "not_helpful", None, "x");
        r.state = None;
        let out = build_feedback_rows(&[r]);
        assert_eq!(out[0].fields["dismissed"], json!(1));
    }
}
