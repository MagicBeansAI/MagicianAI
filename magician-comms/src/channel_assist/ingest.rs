//! Channel ingestor trait (Phase 1b design, "Ingestor trait — the
//! rule-of-two arrives").
//!
//! Three pull-shaped channels exist or are landing (gws CLI, wu.db
//! sqlite, Kapso CLI), so the MINIMAL trait is extracted now, designed
//! from the real gmail implementation rather than speculation. The store
//! remains the boundary: push channels can still bypass the trait and
//! write rows directly (Phase 1 §2b contract, unchanged).
//!
//! ## Shape
//!
//! An ingestor is a stateless provider adapter. The sync worker owns ALL
//! persistence: it loads the account's neutral watermark, calls
//! [`ChannelIngestor::sync_account`], stores the returned batch, and only
//! then advances the watermark — so the Phase-1
//! watermark-never-advances-on-failure and per-account-isolation
//! semantics live in ONE place instead of per provider. The ingestor gets
//! the whole [`SyncWatermark`] (not just the opaque cursor) because real
//! providers need both halves: gmail's expired-history fallback re-lists
//! from `last_internal_date`, wu.db/Kapso cursor on rowid/timestamp.
//!
//! ## Content handoff (decided here, consumed by the N3 distill queue)
//!
//! Raw message content is NEVER part of the persisted rows. The batch's
//! message rows carry only `distill_state` (`pending` for distillable
//! rows, `suppressed` for sensitive ones — set by the ingestor BEFORE
//! the rows ever reach the store). The distill queue ALWAYS re-fetches
//! content by provider through the [`super::assist::distill::ContentFetcher`]
//! registry when it drains a row: `distill_state = pending` is the ONE
//! durable obligation, and the re-fetch is the ONE content path.
//!
//! An earlier draft carried an in-memory warm-start map
//! (`content_by_message_id`) on this batch so ingestors that already
//! held text (wu.db/Kapso listings) could skip the re-fetch. It was
//! removed in N3: the re-fetch path must exist anyway (process restarts
//! orphan any in-memory map while the pending state survives), so the
//! warm map was a second, lossy content path plus a cross-worker handoff
//! channel for zero durable benefit. Providers whose listings include
//! text simply re-read locally in their fetcher (wu.db is a local
//! sqlite; Kapso re-pulls by id).

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::CapabilityScopePaths;

use super::registry::ChannelAccount;
use super::types::{MailMessageMeta, MailThreadRecord, ProviderThreadChange, SyncWatermark};

/// Everything a provider adapter may need to reach its channel for one
/// scope. Built once per sync pass and shared across accounts.
#[derive(Debug, Clone)]
pub struct IngestContext {
    pub principal: String,
    pub workspace: String,
    /// Runtime workspace layout, used by local-store-backed providers.
    pub workspace_layout: ArtifactV2Workspace,
    /// Capability auth root — CLI-profile-backed providers (gws) resolve
    /// `<auth_root>/<profile>` under it.
    pub auth_root: PathBuf,
    /// Scope paths for subprocess PATH augmentation (CLI providers).
    pub scope_paths: CapabilityScopePaths,
    /// Redact affirmatively-sensitive rows at ingest (observe config;
    /// applies to every channel's suppression heuristics).
    pub suppress_sensitive: bool,
    /// First-sync window in days for providers that backfill by query.
    pub backfill_days: u32,
    /// Hard lower bound for messages accepted from provider responses
    /// (epoch millis). Providers can over-fetch for cursor safety, but
    /// ingestors must not return rows older than this.
    pub min_internal_date: i64,
    /// Per-account thread/conversation cap per pass.
    pub max_threads: usize,
    /// Automatic catch-up was disabled or exhausted. Providers establish a
    /// fresh post-boot baseline instead of walking an older durable cursor.
    pub ignore_provider_cursor: bool,
}

/// One account-pass worth of neutral rows plus the watermark advance the
/// worker should apply on success.
#[derive(Debug, Default)]
pub struct IngestBatch {
    /// Pass mode for the sync summary (`backfill` | `incremental` | …).
    pub mode: &'static str,
    /// Provider-resolved owner email for this account pass. Gmail supplies
    /// this from `users.getProfile` on every pass so persistence can repair
    /// legacy/incremental rows whose account identity was lost. Providers
    /// without an email-shaped account identity leave it unset.
    pub account_email: Option<String>,
    /// Thread rows to upsert — `lane` already stamped from the account.
    pub threads: Vec<MailThreadRecord>,
    /// Message metadata rows to append (deduped by the store).
    /// `distill_state` is pre-set by the ingestor (see module header).
    pub messages: Vec<MailMessageMeta>,
    /// Provider-side changes that may carry no new message row (archive,
    /// trash/delete, label-only updates). Persisted before watermark advance
    /// so reconciliation never misses a cursor delta.
    pub provider_changes: Vec<ProviderThreadChange>,
    /// Opaque provider cursor for the next incremental pass (None keeps
    /// the account on the watermark re-list path).
    pub next_provider_cursor: Option<String>,
    /// Max message timestamp observed in this batch; the worker merges it
    /// into the watermark's `last_internal_date` (never backwards).
    pub max_internal_date: Option<i64>,
}

/// A pull-shaped channel adapter. Implementations are provider-specific
/// (gmail today; wu.db and Kapso in N4/N5) and store-blind.
#[async_trait]
pub trait ChannelIngestor: Send + Sync {
    /// Provider key this ingestor serves — matched against
    /// [`ChannelAccount::provider`] by the worker loop.
    fn provider(&self) -> &'static str;

    /// Whether the account's credentials/artifacts are present for this
    /// scope. Not-ready accounts are skipped SILENTLY (Phase-1 behavior:
    /// an observe alias without a gws profile simply drops out — it is
    /// not an error).
    fn account_ready(&self, _ctx: &IngestContext, _account: &ChannelAccount) -> bool {
        true
    }

    /// A configured account can be unavailable for a local runtime reason
    /// (for example, a missing CLI). Unlike absent credentials this is useful
    /// operator state and is surfaced without attempting a doomed subprocess.
    fn unavailable_reason(
        &self,
        _ctx: &IngestContext,
        _account: &ChannelAccount,
    ) -> Option<String> {
        None
    }

    /// Run one pull for the account from its previous watermark and
    /// return the batch to persist. MUST NOT write to the channel-assist
    /// store — persistence and watermark advancement belong to the
    /// worker.
    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch>;
}
