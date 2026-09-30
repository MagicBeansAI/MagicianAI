//! Evidence bridge (unified observe+assist U1) — channel-assist's locally
//! distilled rows feed the work-evidence-graph.
//!
//! The legacy email digest was a scheduled `executive-assistant` task that
//! read mail via the gmail skill and wrote metadata rows to the
//! `user.email_evidence` memory tier, which the generic `tier_distill` turns
//! into WEG evidence. channel-assist already ingests the same mail AND distills
//! it locally into `{summary, intent}` — a strict superset of the digest's
//! metadata. This bridge writes those distilled rows into the SAME tier
//! (`user.email_evidence` for email, `user.chat_evidence` for WhatsApp), so
//! the existing `tier_distill → WEG` path is unchanged downstream and the
//! digest can be retired (U3).
//!
//! Privacy: the bridged fields are the LOCALLY-derived summaries/intents
//! (never raw bodies, never a remote model — the distill guard already
//! governs them); suppressed rows are never bridged.
//!
//! Idempotence: rows are grouped by `(channel, account, day)` and written
//! under the digest's `"{key_prefix}:{account}:{day}"` field key, so a
//! re-run of the same day merges rather than duplicates. A per-scope
//! watermark (the `mail_sync_watermarks` revision-v2 row's legacy-named
//! `last_internal_date` field) advances by monotonic distill revision.

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

use super::store::{DistilledBridgeRow, MailAssistStore};
use super::types::{SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION};

const LOG_TARGET: &str = "channel_assist::evidence_bridge";
const DEFAULT_INTERVAL_SECS: u64 = 120;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 150;
const BRIDGE_BATCH: usize = 500;
/// Cap the per-group detail list so a very busy day can't bloat a tier row.
const MAX_NOTES_PER_GROUP: usize = 12;
/// Synthetic watermark key (a scope has one bridge cursor, not per-account).
const BRIDGE_WATERMARK_KEY: &str = "__bridge_distill_revision_v2__";

/// UTC `YYYY-MM-DD` for an epoch-millis timestamp (matches the digest's
/// day-keying).
fn day_of(internal_date_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(internal_date_ms)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// One tier row to write: which tier, the field key, the fields object.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeRow {
    pub tier: String,
    pub field_key: String,
    pub fields: Value,
}

/// Pure: group distilled rows by `(channel-tier, account, day)` into
/// digest-shaped tier rows. `subjects`/`senders`/`count` give the tier
/// distiller context; `summary` is a factual headline; `notes` carries the
/// local per-message summaries (the WEG signal, bounded).
pub fn build_bridge_rows(rows: &[DistilledBridgeRow]) -> Vec<BridgeRow> {
    // BTreeMap keeps output deterministic (tests + stable field keys).
    struct Group {
        tier: &'static str,
        key_prefix: &'static str,
        source_type: &'static str,
        account: String,
        day: String,
        subjects: Vec<String>,
        senders: Vec<String>,
        threads: std::collections::BTreeSet<String>,
        notes: Vec<String>,
        intents: Vec<String>,
        needs_reply_count: u64,
        follow_up_kinds: Vec<String>,
        follow_up_actors: Vec<String>,
        follow_up_due_texts: Vec<String>,
        follow_up_urgencies: Vec<String>,
    }
    let mut groups: BTreeMap<(String, String, String), Group> = BTreeMap::new();
    for row in rows {
        let ct = super::channel_providers::evidence_tier(&row.provider);
        let day = day_of(row.internal_date);
        let key = (ct.tier.to_string(), row.account_alias.clone(), day.clone());
        let g = groups.entry(key).or_insert_with(|| Group {
            tier: ct.tier,
            key_prefix: ct.key_prefix,
            source_type: ct.source_type,
            account: row.account_alias.clone(),
            day: day.clone(),
            subjects: Vec::new(),
            senders: Vec::new(),
            threads: std::collections::BTreeSet::new(),
            notes: Vec::new(),
            intents: Vec::new(),
            needs_reply_count: 0,
            follow_up_kinds: Vec::new(),
            follow_up_actors: Vec::new(),
            follow_up_due_texts: Vec::new(),
            follow_up_urgencies: Vec::new(),
        });
        g.threads.insert(row.thread_id.clone());
        push_unique(&mut g.subjects, row.subject.as_deref());
        push_unique(
            &mut g.senders,
            row.from_name.as_deref().or(row.from_address.as_deref()),
        );
        if let Some(s) = row
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if g.notes.len() < MAX_NOTES_PER_GROUP && !g.notes.iter().any(|n| n == s) {
                g.notes.push(s.to_string());
            }
        }
        push_unique(&mut g.intents, row.intent.as_deref());
        if row.needs_reply_hint {
            g.needs_reply_count = g.needs_reply_count.saturating_add(1);
        }
        if let Some(hint) = &row.follow_up_hint {
            push_unique(&mut g.follow_up_kinds, Some(hint.kind.as_str()));
            push_unique(&mut g.follow_up_actors, hint.actor.as_deref());
            push_unique(&mut g.follow_up_due_texts, hint.due_text.as_deref());
            push_unique(&mut g.follow_up_urgencies, hint.urgency.as_deref());
        }
    }

    groups
        .into_values()
        .map(|g| {
            let thread_count = g.threads.len();
            let channel_word = if g.key_prefix == "email" {
                "email"
            } else {
                "chat"
            };
            let summary = format!(
                "{thread_count} {channel_word} thread{} on {} for {}",
                if thread_count == 1 { "" } else { "s" },
                g.day,
                g.account
            );
            let mut fields = Map::new();
            fields.insert("account".into(), json!(g.account));
            fields.insert("day".into(), json!(g.day));
            fields.insert("source_type".into(), json!(g.source_type));
            fields.insert("summary".into(), json!(summary));
            fields.insert("subjects".into(), json!(g.subjects));
            fields.insert("senders".into(), json!(g.senders));
            fields.insert("thread_count".into(), json!(thread_count));
            fields.insert("intents".into(), json!(g.intents));
            fields.insert("needs_reply_count".into(), json!(g.needs_reply_count));
            fields.insert("follow_up_kinds".into(), json!(g.follow_up_kinds));
            fields.insert("follow_up_actors".into(), json!(g.follow_up_actors));
            fields.insert("follow_up_due_texts".into(), json!(g.follow_up_due_texts));
            fields.insert("follow_up_urgencies".into(), json!(g.follow_up_urgencies));
            fields.insert("notes".into(), json!(g.notes));
            BridgeRow {
                tier: g.tier.to_string(),
                field_key: format!("{}:{}:{}", g.key_prefix, g.account, g.day),
                fields: Value::Object(fields),
            }
        })
        .collect()
}

fn push_unique(dst: &mut Vec<String>, value: Option<&str>) {
    if let Some(v) = value.map(str::trim).filter(|s| !s.is_empty()) {
        if !dst.iter().any(|x| x == v) {
            dst.push(v.to_string());
        }
    }
}

/// One bridge pass for a scope: read watermark → distilled rows → build
/// tier rows → merge into the memory tiers → advance the watermark.
pub async fn run_bridge_pass(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
) -> Result<usize> {
    let watermark = store
        .get_watermark(
            principal,
            workspace,
            BRIDGE_WATERMARK_KEY,
            BRIDGE_WATERMARK_KEY,
        )
        .await?;
    let since = watermark
        .as_ref()
        .and_then(|w| w.last_internal_date)
        .unwrap_or(0);
    let rows = store
        .list_distilled_for_bridge(principal, workspace, since, BRIDGE_BATCH)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let max_distill_revision = rows.last().map(|row| row.distill_revision).unwrap_or(since);
    let bridge_rows = build_bridge_rows(&rows);

    let resolver = AgentMemoryResolver::with_workspace_layout(process_storage::workspace());
    let mut written = 0usize;
    let mut failed = 0usize;
    for br in &bridge_rows {
        let mut fields = Map::new();
        fields.insert(br.field_key.clone(), br.fields.clone());
        let outcome =
            merge_user_memory_tier_fields(&resolver, principal, workspace, &br.tier, &fields).await;
        if outcome.get("status").and_then(Value::as_str) == Some("error") {
            // Compute the reason OUTSIDE the macro: inside `warn!`, a bare
            // `Value` resolves to tracing's `Value` trait, not serde_json's.
            let reason = outcome.get("reason").and_then(Value::as_str).unwrap_or("");
            warn!(
                target: LOG_TARGET,
                tier = br.tier.as_str(),
                reason,
                "evidence bridge tier write failed"
            );
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
            max_distill_revision,
            "evidence bridge pass kept watermark for retry"
        );
        return Ok(written);
    }

    // Advance the watermark only after writes (idempotent re-run on failure).
    store
        .set_watermark(
            principal,
            workspace,
            SyncWatermark {
                schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                provider: BRIDGE_WATERMARK_KEY.to_string(),
                account_alias: BRIDGE_WATERMARK_KEY.to_string(),
                last_internal_date: Some(max_distill_revision),
                provider_cursor: None,
                last_synced_at: chrono::Utc::now().timestamp_millis(),
                last_error: None,
            },
        )
        .await?;
    debug!(
        target: LOG_TARGET,
        groups = bridge_rows.len(),
        written,
        max_distill_revision,
        "evidence bridge pass complete"
    );
    Ok(written)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

pub struct ChannelEvidenceBridgeWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelEvidenceBridgeWorker {
    /// `CHANNEL_EVIDENCE_BRIDGE_ENABLED` (default on) — the bin gates the spawn.
    pub fn enabled_from_env() -> bool {
        std::env::var("CHANNEL_EVIDENCE_BRIDGE_ENABLED")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true)
    }

    /// Spawn the bridge loop for the DEFAULT scope (like the sync/distill
    /// workers — owns its own cancellation token).
    pub fn spawn(store: MailAssistStore) -> Self {
        let interval = Duration::from_secs(env_secs(
            "CHANNEL_EVIDENCE_BRIDGE_INTERVAL_SECS",
            DEFAULT_INTERVAL_SECS,
        ));
        let startup = Duration::from_secs(env_secs(
            "CHANNEL_EVIDENCE_BRIDGE_STARTUP_DELAY_SECS",
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
                match run_bridge_pass(&store, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
                    .await
                {
                    Ok(n) if n > 0 => {
                        debug!(target: LOG_TARGET, written = n, "bridge tick wrote rows")
                    },
                    Ok(_) => {},
                    Err(error) => warn!(target: LOG_TARGET, error = %error, "bridge tick failed"),
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

    fn row(
        provider: &str,
        account: &str,
        thread: &str,
        ts: i64,
        summary: &str,
        subject: &str,
    ) -> DistilledBridgeRow {
        DistilledBridgeRow {
            provider: provider.to_string(),
            account_alias: account.to_string(),
            thread_id: thread.to_string(),
            internal_date: ts,
            summary: Some(summary.to_string()),
            intent: Some("needs_reply".to_string()),
            subject: Some(subject.to_string()),
            from_name: Some("Alice".to_string()),
            from_address: Some("alice@example.com".to_string()),
            needs_reply_hint: false,
            follow_up_hint: None,
            message_id: format!("{account}-{thread}-{ts}"),
            distill_brief: None,
            distill_revision: ts,
        }
    }

    // 2026-07-05 and 2026-07-06 UTC midnights (ms).
    const D5: i64 = 1_783_209_600_000;
    const D6: i64 = 1_783_296_000_000;

    #[test]
    fn groups_by_channel_account_day_with_digest_key() {
        let rows = vec![
            row(
                "gmail",
                "business",
                "t1",
                D5 + 1000,
                "replied to Bob",
                "Invoice",
            ),
            row(
                "gmail",
                "business",
                "t2",
                D5 + 2000,
                "scheduled a call",
                "Meeting",
            ),
            row(
                "gmail",
                "business",
                "t3",
                D6 + 1000,
                "next day thread",
                "Followup",
            ),
        ];
        let out = build_bridge_rows(&rows);
        assert_eq!(out.len(), 2); // two days
        let day5 = out
            .iter()
            .find(|r| r.field_key == "email:business:2026-07-05")
            .unwrap();
        assert_eq!(day5.tier, "user.email_evidence");
        assert_eq!(day5.fields["thread_count"], json!(2));
        assert_eq!(day5.fields["source_type"], json!("email_capture"));
        assert_eq!(day5.fields["notes"].as_array().unwrap().len(), 2);
        assert!(out
            .iter()
            .any(|r| r.field_key == "email:business:2026-07-06"));
    }

    #[test]
    fn chat_channels_route_to_chat_tier() {
        let rows = vec![
            row("whatsapp", "self", "c1", D5 + 500, "family plans", "Mom"),
            row(
                "whatsapp_kapso",
                "presto",
                "c2",
                D5 + 700,
                "customer question",
                "Client",
            ),
        ];
        let out = build_bridge_rows(&rows);
        assert!(out.iter().all(|r| r.tier == "user.chat_evidence"));
        assert!(out.iter().any(|r| r.field_key == "chat:self:2026-07-05"));
        assert!(out.iter().any(|r| r.field_key == "chat:presto:2026-07-05"));
    }

    #[test]
    fn distinct_threads_subjects_senders_deduped() {
        let rows = vec![
            row("gmail", "business", "t1", D5 + 1, "a", "Same Subject"),
            row("gmail", "business", "t1", D5 + 2, "a", "Same Subject"), // same thread + summary
        ];
        let out = build_bridge_rows(&rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].fields["thread_count"], json!(1));
        assert_eq!(out[0].fields["subjects"].as_array().unwrap().len(), 1);
        assert_eq!(out[0].fields["notes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn day_boundary_utc() {
        assert_eq!(day_of(1_783_209_600_000), "2026-07-05");
        assert_eq!(day_of(1_783_209_600_000 - 1), "2026-07-04");
    }
}
