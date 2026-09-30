//! Distilled-comms corpus source: the scope's locally-distilled messages.
//!
//! Reads the channel-assist store's DISTILLED message rows through
//! [`ChannelAssistStore::list_distilled_for_bridge`] and yields one [`CorpusItem`]
//! per DONE-distilled, non-suppressed message with a newer distill revision — the
//! "something someone said to us earlier" the resurfacing engine brings back.
//!
//! **Privacy — distilled summaries ONLY.** A comm may enter the resurfacing
//! corpus solely as its locally-derived `summary` (the mail distiller's output),
//! never a raw body. Bodies exist only in process memory during distillation
//! and are never persisted; this adapter reads the already-computed `summary`
//! column and nothing else content-bearing. `list_distilled_for_bridge` already
//! filters `distill_state = 'done' AND sensitive_suppressed = FALSE`, so
//! suppressed/undistilled rows never reach the mapper, and the mapper skips any
//! row lacking a non-empty summary as a belt-and-braces guard.
//!
//! **Why not `list_pattern_corpus`.** That passive-synthesis reader
//! (`PatternCorpusRow`) exposes only `internal_date/subject/from_address/summary`
//! — it carries no stable per-message identifier and does not gate on
//! `distill_state = 'done'`. `list_distilled_for_bridge`/`ChannelDistilledBridgeRow`
//! is the store's canonical "distilled messages since a watermark" reader: it
//! gates on `done`, applies the strict `distill_revision > watermark` contract,
//! and carries stable provider/message identifiers so correcting an old message
//! updates the same candidate row.
//!
//! **Granularity — per message.** One corpus item per distilled message. The
//! source_ref keys on provider/account/thread/message ids plus the provider
//! timestamp, so distinct messages in a thread stay distinct and the same
//! message maps to the same ref on every scan.
//!
//! **Surface boundary.** This source only enforces corpus safety: distilled,
//! non-suppressed rows with a usable summary. Lane eligibility belongs to the
//! shared attention router. If a comm already has an active Follow-up, the
//! resurfacing curator records the canonical `active_follow_up_exists` drop
//! instead of this adapter silently filtering it before observability.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::channel_assist::channel::{
    ChannelAssistStore, ChannelDetailStatus, ChannelDistilledBridgeRow, ChannelTemporalKind,
};
use magician::magician_v2::attention::resurfacing::source_refs::comm_source_ref_parts;
use magician::magician_v2::attention::resurfacing::types::{
    CorpusItem, ResurfacingChangeFact, ResurfacingContentDetails, ResurfacingDetailStatus,
    ResurfacingTemporalFact, SourceKind,
};

use magician::magician_v2::attention::resurfacing::sources::ResurfacingSource;

/// Upper bound on distilled rows read per scan. The reader is newest-relevant
/// (`internal_date > since` ordered oldest-first) so a sane cap keeps a single
/// scan bounded; the worker advances the watermark and drains the rest next pass.
const DISTILLED_CORPUS_CAP: usize = 500;
const DISTILLED_CORPUS_PAGE_LIMIT: usize = 10;
const COMMS_REVISION_CORPUS_KIND: &str = "comm_distill_revision_v2";

/// Adapts a scope's distilled comms into resurfacing corpus items.
///
/// Holds the shared [`ChannelAssistStore`] the rest of the runtime uses to read
/// scoped comms metadata, mirroring how
/// [`MemorySource`](magician::magician_v2::attention::resurfacing::sources::memory::MemorySource) holds its `AgentMemoryResolver`
/// and [`TaskEpisodeSource`](magician::magician_v2::attention::resurfacing::sources::task_episode::TaskEpisodeSource) holds its
/// `ArtifactV2Service`.
#[derive(Debug, Clone)]
pub struct CommsSource {
    store: ChannelAssistStore,
}

impl CommsSource {
    /// Construct over the shared channel-assist store (the same handle the
    /// comms data plane and API layer already hold).
    pub fn new(store: ChannelAssistStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ResurfacingSource for CommsSource {
    fn corpus_kind(&self) -> &'static str {
        // Version the watermark key: pre-V2 stores hold an epoch-millisecond
        // cursor under `comm`, which would permanently outrank small monotonic
        // revisions. Candidate source_kind remains `comm`, so replay updates
        // stable candidate IDs instead of creating duplicates.
        COMMS_REVISION_CORPUS_KIND
    }

    async fn list_changed_since(
        &self,
        principal: &str,
        workspace: &str,
        watermark: i64,
    ) -> Result<Vec<CorpusItem>> {
        let mut rows = Vec::new();
        let mut page_revision = watermark;
        for _ in 0..DISTILLED_CORPUS_PAGE_LIMIT {
            let page = self
                .store
                .list_distilled_for_bridge(
                    principal,
                    workspace,
                    page_revision,
                    DISTILLED_CORPUS_CAP,
                )
                .await
                .with_context(|| {
                    format!("list distilled comms for {principal}/{workspace} for resurfacing")
                })?;
            let full_page = page.len() == DISTILLED_CORPUS_CAP;
            let last = page.last().cloned();
            rows.extend(page);
            let Some(last) = last else {
                break;
            };
            if !full_page {
                break;
            }
            page_revision = last.distill_revision;
        }
        Ok(map_distilled_rows(&rows, watermark))
    }
}

/// Map distilled message rows into corpus items whose source cursor
/// (`distill_revision`) is strictly greater than `since_revision`.
///
/// Pure over the already-loaded [`ChannelDistilledBridgeRow`]s so the
/// row→[`CorpusItem`] mapping is unit-testable without seeding the store,
/// exactly as the memory/task sources factored their `map_*` helpers. Rows
/// without a non-empty distilled summary (the ONLY comm text allowed into the
/// corpus — never a body) or without a usable timestamp are skipped.
fn map_distilled_rows(rows: &[ChannelDistilledBridgeRow], since_revision: i64) -> Vec<CorpusItem> {
    let mut items = Vec::new();
    for row in rows {
        // Distilled summary is the sole comm text that may enter the corpus.
        let Some(summary) = clean_summary(row) else {
            continue; // undistilled / empty summary: nothing safe to surface
        };
        let Some(occurred_at) = occurred_at_secs(row.internal_date) else {
            continue; // no usable timestamp → cannot compare against the watermark
        };
        if row.distill_revision <= since_revision {
            continue;
        }
        let title = row_title(row);
        let content_details = row.distill_brief.as_ref().map(map_content_details);
        items.push(CorpusItem {
            source_kind: SourceKind::Comm,
            source_ref: comm_source_ref(row),
            title: title.clone(),
            digest: summary.clone(),
            content_details,
            content_revision: Some(row.distill_revision.to_string()),
            occurred_at,
            watermark_cursor: row.distill_revision,
            embedding_text: format!("{title}\n{summary}"),
        });
    }
    items
}

/// The distilled summary, trimmed; `None` when absent or blank. This is the
/// only content-bearing field copied into the corpus — never a raw body.
fn clean_summary(row: &ChannelDistilledBridgeRow) -> Option<String> {
    row.summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Normalize a Gmail `internalDate` (epoch millis) to unix seconds, matching
/// the resurfacing watermark/`occurred_at` unit. Sub-second precision is
/// intentionally dropped (mirrors the task source's whole-second stamps).
/// Non-positive stamps are treated as unusable.
fn occurred_at_secs(internal_date_ms: i64) -> Option<i64> {
    if internal_date_ms <= 0 {
        None
    } else {
        Some(internal_date_ms / 1000)
    }
}

fn map_content_details(
    brief: &crate::channel_assist::channel::ChannelInformationBrief,
) -> ResurfacingContentDetails {
    ResurfacingContentDetails {
        schema_version: brief.schema_version,
        key_facts: brief.key_facts.clone(),
        changes: brief
            .changes
            .iter()
            .map(|change| ResurfacingChangeFact {
                aspect: change.aspect.clone(),
                before: change.before.clone(),
                after: change.after.clone(),
                effective_text: change.effective_text.clone(),
            })
            .collect(),
        temporal_facts: brief
            .temporal_facts
            .iter()
            .map(|fact| ResurfacingTemporalFact {
                kind: channel_temporal_kind(fact.kind).to_string(),
                text: fact.text.clone(),
                at_ms: fact.at_ms,
                timezone: fact.timezone.clone(),
            })
            .collect(),
        detail_status: match brief.detail_status {
            ChannelDetailStatus::Complete => ResurfacingDetailStatus::Complete,
            ChannelDetailStatus::Partial => ResurfacingDetailStatus::Partial,
            ChannelDetailStatus::SourceOmitsDetails => ResurfacingDetailStatus::SourceOmitsDetails,
        },
        missing_details: brief.missing_details.clone(),
    }
}

fn channel_temporal_kind(kind: ChannelTemporalKind) -> &'static str {
    match kind {
        ChannelTemporalKind::Due => "due",
        ChannelTemporalKind::Expiry => "expiry",
        ChannelTemporalKind::Effective => "effective",
        ChannelTemporalKind::Scheduled => "scheduled",
        ChannelTemporalKind::Occurred => "occurred",
        ChannelTemporalKind::PeriodStart => "period_start",
        ChannelTemporalKind::PeriodEnd => "period_end",
        ChannelTemporalKind::Other => "other",
    }
}

/// Human-facing title: prefer the message subject, falling back to a sender
/// label (name, then address) when the subject is blank, and finally the
/// thread id so the curator always has a stable handle.
fn row_title(row: &ChannelDistilledBridgeRow) -> String {
    for candidate in [&row.subject, &row.from_name, &row.from_address] {
        if let Some(raw) = candidate {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    row.thread_id.clone()
}

/// Stable per-message ref:
/// `provider/account_alias/thread_id/message_id@internal_date`. Message id is
/// the primary disambiguator; the timestamp keeps refs understandable and
/// protects adapters whose ids are only unique inside a thread.
pub fn comm_source_ref(row: &ChannelDistilledBridgeRow) -> String {
    comm_source_ref_parts(
        &row.provider,
        &row.account_alias,
        &row.thread_id,
        &row.message_id,
        row.internal_date,
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::channel_assist::channel::{
        ChannelAnnotation, ChannelAnnotationState, ChannelAssistActor, ChannelChangeFact,
        ChannelDetailStatus, ChannelFollowUpHint, ChannelInformationBrief, ChannelInformationType,
        ChannelLane, ChannelMessageMeta, ChannelRecordOrigin, ChannelTemporalFact,
        ChannelTemporalKind, ChannelThreadRecord, DistillState, MessageDirection,
        CHANNEL_ASSIST_SCHEMA_VERSION,
    };
    use tempfile::TempDir;

    fn bridge_row(
        thread: &str,
        internal_date: i64,
        subject: Option<&str>,
        from_name: Option<&str>,
        from_address: Option<&str>,
        summary: Option<&str>,
    ) -> ChannelDistilledBridgeRow {
        ChannelDistilledBridgeRow {
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: thread.to_string(),
            internal_date,
            summary: summary.map(str::to_string),
            intent: Some("fyi".to_string()),
            subject: subject.map(str::to_string),
            from_name: from_name.map(str::to_string),
            from_address: from_address.map(str::to_string),
            needs_reply_hint: false,
            follow_up_hint: None,
            message_id: format!("{thread}-{internal_date}"),
            distill_brief: None,
            distill_revision: internal_date,
        }
    }

    #[test]
    fn map_filters_by_watermark_maps_summary_and_title_fallback() {
        // The fixture uses its provider timestamp as a convenient synthetic
        // distill revision; occurred_at remains unix seconds for scoring.
        let watermark = 3_000_000;
        let rows = vec![
            // Older than the watermark (1_000s <= 3_000s) → dropped.
            bridge_row(
                "t-old",
                1_000_000,
                Some("Old subject"),
                Some("Old Sender"),
                Some("old@example.com"),
                Some("an old summary"),
            ),
            // Exactly on the watermark boundary (3_000s) → dropped (strict >).
            bridge_row(
                "t-boundary",
                3_000_000,
                Some("Boundary subject"),
                None,
                None,
                Some("boundary summary"),
            ),
            // Newer, has a subject → survives; subject→title, summary→digest.
            bridge_row(
                "t-new",
                5_000_000,
                Some("Fresh subject"),
                Some("Alice"),
                Some("alice@example.com"),
                Some("a fresh summary"),
            ),
            // Newer, blank subject → title falls back to the sender name.
            bridge_row(
                "t-fallback",
                6_000_000,
                Some("   "),
                Some("Bob Sender"),
                Some("bob@example.com"),
                Some("fallback summary"),
            ),
            // Newer but summary-less → skipped (nothing safe to surface).
            bridge_row(
                "t-nosum",
                7_000_000,
                Some("No summary subject"),
                None,
                None,
                None,
            ),
        ];

        let items = map_distilled_rows(&rows, watermark);

        assert_eq!(items.len(), 2, "only the two newer, distilled rows survive");

        let subject_item = &items[0];
        assert_eq!(subject_item.source_kind, SourceKind::Comm);
        assert_eq!(
            subject_item.source_ref,
            "gmail/business/t-new/t-new-5000000@5000000"
        );
        assert_eq!(subject_item.title, "Fresh subject");
        assert_eq!(subject_item.digest, "a fresh summary");
        assert_eq!(
            subject_item.embedding_text,
            "Fresh subject\na fresh summary"
        );
        assert_eq!(subject_item.occurred_at, 5_000);
        assert_eq!(subject_item.watermark_cursor, 5_000_000);
        assert!(subject_item.watermark_cursor > watermark);

        let fallback_item = &items[1];
        assert_eq!(
            fallback_item.source_ref,
            "gmail/business/t-fallback/t-fallback-6000000@6000000"
        );
        assert_eq!(
            fallback_item.title, "Bob Sender",
            "blank subject falls back to the sender name"
        );
        assert_eq!(fallback_item.digest, "fallback summary");

        // The summary-less and out-of-window rows never appear.
        let refs: Vec<&str> = items.iter().map(|it| it.source_ref.as_str()).collect();
        assert!(refs.iter().all(|r| !r.contains("t-nosum")));
        assert!(refs.iter().all(|r| !r.contains("t-old")));
        assert!(refs.iter().all(|r| !r.contains("t-boundary")));
    }

    #[test]
    fn map_keeps_actionable_hint_rows_for_router_observability() {
        let watermark = 0;
        let mut needs_reply = bridge_row(
            "t-reply",
            5_000_000,
            Some("Reply needed"),
            Some("Alice"),
            Some("alice@example.com"),
            Some("Alice expects a response"),
        );
        needs_reply.needs_reply_hint = true;

        let mut promise_hint = bridge_row(
            "t-promise",
            6_000_000,
            Some("Promise"),
            Some("Bob"),
            Some("bob@example.com"),
            Some("Bob is waiting on the owner"),
        );
        promise_hint.follow_up_hint = Some(ChannelFollowUpHint {
            kind: "owner_owes".to_string(),
            actor: Some("owner".to_string()),
            counterparty: Some("Bob".to_string()),
            due_text: Some("this week".to_string()),
            urgency: Some("normal".to_string()),
            rationale: Some("The owner committed to send it.".to_string()),
            key_details: vec!["Due this week".to_string()],
        });

        let mut legacy_action = bridge_row(
            "t-action",
            7_000_000,
            Some("Action"),
            Some("Carol"),
            Some("carol@example.com"),
            Some("Carol asks the owner to do something"),
        );
        legacy_action.intent = Some("action_request".to_string());

        let passive = bridge_row(
            "t-passive",
            8_000_000,
            Some("FYI"),
            Some("Dana"),
            Some("dana@example.com"),
            Some("Dana shared background context"),
        );

        let items = map_distilled_rows(
            &[needs_reply, promise_hint, legacy_action, passive],
            watermark,
        );

        let refs: Vec<&str> = items.iter().map(|item| item.source_ref.as_str()).collect();
        assert_eq!(items.len(), 4);
        assert!(refs.contains(&"gmail/business/t-reply/t-reply-5000000@5000000"));
        assert!(refs.contains(&"gmail/business/t-promise/t-promise-6000000@6000000"));
        assert!(refs.contains(&"gmail/business/t-action/t-action-7000000@7000000"));
        assert!(refs.contains(&"gmail/business/t-passive/t-passive-8000000@8000000"));
    }

    #[test]
    fn map_uses_message_id_refs_and_millisecond_cursor_for_same_second_rows() {
        let rows = vec![
            bridge_row(
                "t-same",
                5_000_100,
                Some("First"),
                Some("Alice"),
                Some("alice@example.com"),
                Some("first same-second summary"),
            ),
            bridge_row(
                "t-same",
                5_000_200,
                Some("Second"),
                Some("Alice"),
                Some("alice@example.com"),
                Some("second same-second summary"),
            ),
        ];

        let items = map_distilled_rows(&rows, 5_000_150);

        assert_eq!(items.len(), 1, "cursor precision filters within a second");
        assert_eq!(
            items[0].source_ref, "gmail/business/t-same/t-same-5000200@5000200",
            "message id remains part of the stable candidate ref"
        );
        assert_eq!(
            items[0].occurred_at, 5_000,
            "scoring timestamp remains unix seconds"
        );
        assert_eq!(items[0].watermark_cursor, 5_000_200);
    }

    #[test]
    fn corrected_old_message_maps_safe_details_by_new_distill_revision() {
        let mut corrected = bridge_row(
            "old-thread",
            1_000,
            Some("Policy update"),
            Some("Provider"),
            None,
            Some("The monthly limit changed from 5 to 10 on July 1."),
        );
        corrected.distill_revision = 42;
        corrected.distill_brief = Some(ChannelInformationBrief {
            schema_version: 2,
            information_type: ChannelInformationType::ChangeNotice,
            summary: corrected.summary.clone().unwrap(),
            key_facts: vec!["Monthly limit: 10".to_string()],
            changes: vec![ChannelChangeFact {
                aspect: "Monthly limit".to_string(),
                before: Some("5".to_string()),
                after: Some("10".to_string()),
                effective_text: Some("July 1".to_string()),
            }],
            temporal_facts: vec![ChannelTemporalFact {
                kind: ChannelTemporalKind::Effective,
                text: "July 1".to_string(),
                at_ms: None,
                timezone: None,
            }],
            stated_action: None,
            detail_status: ChannelDetailStatus::Complete,
            missing_details: Vec::new(),
        });

        let items = map_distilled_rows(&[corrected], 41);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].watermark_cursor, 42);
        assert_eq!(items[0].content_revision.as_deref(), Some("42"));
        let details = items[0].content_details.as_ref().unwrap();
        assert_eq!(details.key_facts, vec!["Monthly limit: 10"]);
        assert_eq!(details.changes[0].before.as_deref(), Some("5"));
        assert_eq!(details.changes[0].after.as_deref(), Some("10"));
        assert!(map_distilled_rows(
            &[bridge_row(
                "old-thread",
                1_000,
                Some("Policy update"),
                None,
                None,
                Some("summary"),
            )],
            1_000,
        )
        .is_empty());
    }

    #[test]
    fn comm_source_ref_roundtrips_priority_message_identity() {
        let source_ref = comm_source_ref_parts(
            "telegram",
            "public/bot",
            "thread/with/slash",
            "message@42",
            123,
        );
        assert_eq!(
            magician::magician_v2::attention::resurfacing::source_refs::parse_comm_source_message_key(
                &source_ref
            ),
            Some((
                "telegram".to_string(),
                "public/bot".to_string(),
                "message@42".to_string(),
            ))
        );
    }

    fn seed_thread() -> ChannelThreadRecord {
        ChannelThreadRecord {
            schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            account_email: Some("owner@example.com".to_string()),
            thread_id: "t1".to_string(),
            lane: ChannelLane::UserAssist,
            subject: Some("Invoice from Acme".to_string()),
            latest_summary: None,
            latest_from_name: Some("Acme Billing".to_string()),
            latest_from_address: Some("billing@acme.example".to_string()),
            recipient_domains: vec!["example.com".to_string()],
            label_ids: vec!["INBOX".to_string()],
            message_count: 1,
            last_message_at: Some(1_783_209_600_000),
            provider_cursor: None,
            sensitive_suppressed: false,
            origin: ChannelRecordOrigin::MetadataSync,
            first_observed_at: 1_783_209_600_000,
            last_observed_at: 1_783_209_600_000,
        }
    }

    fn seed_message() -> ChannelMessageMeta {
        ChannelMessageMeta {
            schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            account_email: Some("owner@example.com".to_string()),
            thread_id: "t1".to_string(),
            message_id: "msg-1".to_string(),
            provider_cursor: None,
            label_ids: vec!["INBOX".to_string()],
            subject: Some("Invoice from Acme".to_string()),
            from_name: Some("Acme Billing".to_string()),
            from_address: Some("billing@acme.example".to_string()),
            to_domains: vec!["example.com".to_string()],
            cc_domains: Vec::new(),
            // Gmail internalDate, epoch millis → occurred_at 1_783_209_600 s.
            internal_date: 1_783_209_600_000,
            observed_at: 1_783_209_600_500,
            direction: Some(MessageDirection::Inbound),
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
            origin: ChannelRecordOrigin::MetadataSync,
        }
    }

    fn seed_needs_approval_annotation() -> ChannelAnnotation {
        ChannelAnnotation {
            schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
            id: "ann-1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            lane: ChannelLane::UserAssist,
            state: ChannelAnnotationState::NeedsApproval,
            label: Some("follow_up".to_string()),
            confidence: Some(0.9),
            reason: Some("The owner owes a follow-up.".to_string()),
            evidence_refs: Vec::new(),
            evidence_message_id: Some("msg-1".to_string()),
            evidence_message_at: Some(1_783_209_600_000),
            classification_input_revision: None,
            semantic_features: None,
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "owner_owes",
                "action_owner": "owner",
            })),
            provenance: Some("test".to_string()),
            created_at: 1_783_209_601_000,
            updated_at: 1_783_209_601_000,
        }
    }

    /// Real read path: seed one message, mark it DONE-distilled with a
    /// locally-derived summary, then prove `list_changed_since` surfaces exactly
    /// one `Comm` item whose digest is the summary (never a body) and honors the
    /// strict watermark.
    #[tokio::test]
    async fn list_changed_since_surfaces_distilled_comm_summary() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let (principal, workspace) = ("anonymous", "default");

        store
            .upsert_thread(principal, workspace, seed_thread())
            .await
            .expect("seed thread");
        store
            .append_messages(principal, workspace, vec![seed_message()])
            .await
            .expect("seed message");
        // Distillation output is the local model's summary — never a raw body.
        store
            .set_distill_result(
                principal,
                workspace,
                "gmail",
                "business",
                "msg-1",
                "Acme sent the signed invoice",
                "fyi",
                false,
                None,
            )
            .await
            .expect("mark distilled");

        let source = CommsSource::new(store);
        assert_eq!(source.corpus_kind(), COMMS_REVISION_CORPUS_KIND);

        let watermark = 0;
        let items = source
            .list_changed_since(principal, workspace, watermark)
            .await
            .expect("list changed");

        assert_eq!(items.len(), 1, "exactly the one distilled message");
        let item = &items[0];
        assert_eq!(item.source_kind, SourceKind::Comm);
        assert_eq!(item.source_ref, "gmail/business/t1/msg-1@1783209600000");
        assert_eq!(item.title, "Invoice from Acme");
        assert_eq!(
            item.digest, "Acme sent the signed invoice",
            "digest is the distilled summary, never a body"
        );
        assert_eq!(item.occurred_at, 1_783_209_600);
        assert_eq!(item.watermark_cursor, 1);

        // A watermark at/after the message filters it out (strict >).
        let none = source
            .list_changed_since(principal, workspace, 1)
            .await
            .expect("list changed future");
        assert!(
            none.is_empty(),
            "nothing is strictly newer than its own second"
        );
    }

    #[tokio::test]
    async fn redistilling_old_message_reemits_same_source_ref_at_new_revision() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let (principal, workspace) = ("anonymous", "default");
        store
            .upsert_thread(principal, workspace, seed_thread())
            .await
            .unwrap();
        store
            .append_messages(principal, workspace, vec![seed_message()])
            .await
            .unwrap();
        store
            .set_distill_result(
                principal,
                workspace,
                "gmail",
                "business",
                "msg-1",
                "A generic policy notice arrived.",
                "fyi",
                false,
                None,
            )
            .await
            .unwrap();

        let source = CommsSource::new(store.clone());
        let initial = source
            .list_changed_since(principal, workspace, 0)
            .await
            .unwrap();
        assert_eq!(initial[0].watermark_cursor, 1);

        store
            .set_distill_result(
                principal,
                workspace,
                "gmail",
                "business",
                "msg-1",
                "The policy raises the monthly limit from 5 to 10.",
                "fyi",
                false,
                None,
            )
            .await
            .unwrap();
        let corrected = source
            .list_changed_since(principal, workspace, 1)
            .await
            .unwrap();
        assert_eq!(corrected.len(), 1);
        assert_eq!(corrected[0].source_ref, initial[0].source_ref);
        assert_eq!(corrected[0].watermark_cursor, 2);
        assert_eq!(
            corrected[0].digest,
            "The policy raises the monthly limit from 5 to 10."
        );
        assert!(source
            .list_changed_since(principal, workspace, 2)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn list_changed_since_keeps_active_follow_up_rows_for_router_observability() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let (principal, workspace) = ("anonymous", "default");

        store
            .upsert_thread(principal, workspace, seed_thread())
            .await
            .expect("seed thread");
        store
            .append_messages(principal, workspace, vec![seed_message()])
            .await
            .expect("seed message");
        store
            .set_distill_result(
                principal,
                workspace,
                "gmail",
                "business",
                "msg-1",
                "Acme sent the signed invoice",
                "fyi",
                false,
                None,
            )
            .await
            .expect("mark distilled");
        store
            .create_annotation(
                principal,
                workspace,
                seed_needs_approval_annotation(),
                ChannelAssistActor::Worker,
            )
            .await
            .expect("seed needs approval annotation");

        let source = CommsSource::new(store);
        let items = source
            .list_changed_since(principal, workspace, 0)
            .await
            .expect("list changed");

        assert_eq!(
            items.len(),
            1,
            "safe source ingestion keeps the row so router observability can record active-follow-up overlap"
        );
        assert_eq!(items[0].source_ref, "gmail/business/t1/msg-1@1783209600000");
    }

    /// The adapter is usable behind `Box<dyn ResurfacingSource>` (object-safe)
    /// and exposes the stable `"comm"` corpus kind on an empty scope.
    #[tokio::test]
    async fn is_object_safe_behind_dyn() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let source: Box<dyn ResurfacingSource> = Box::new(CommsSource::new(store));
        assert_eq!(source.corpus_kind(), COMMS_REVISION_CORPUS_KIND);
        let items = source
            .list_changed_since("anonymous", "default", 0)
            .await
            .expect("empty scope lists cleanly");
        assert!(items.is_empty(), "no messages seeded → no corpus items");
    }
}
