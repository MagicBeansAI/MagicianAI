//! The owner-set spend envelope: money is the only budget that matters.
//!
//! One process-wide envelope counts, in micro-USD, what every model call costs
//! and refuses *autonomous* LLM work once the owner's window ceiling
//! (`execution.budgets.spend_envelope`, default $250 a UTC day) is spent. The
//! per-run valves (`native_max_cost_usd`, `harness_max_cost_usd`) stay as
//! runaway guards underneath it; token and per-run pause prompts no longer
//! stand between the owner and a long run.
//!
//! # Where it bites
//!
//! - **Router gate.** `magicllm`'s router calls [`EnvelopeGate`] (installed by
//!   [`install_process_envelope`]) before each physical provider attempt
//!   ([`SpendEnvelope::admit`]) and settles each attempt exactly once
//!   ([`SpendEnvelope::settle`], the actual cost from the pricing table, or an
//!   estimate for a stream its consumer abandoned or a provider cut short).
//!   Embeddings are admitted and settled the same way; free local models are
//!   skipped. Every
//!   model call the router sees is therefore counted; no producer feeds the
//!   envelope twice, and the analytics trace funnel is deliberately *not* a
//!   feed.
//! - **Producers that bypass the router** charge explicitly through
//!   [`charge_external`]: harness (CLI) turns, the Decision Engine's model
//!   calls and the coding cost ledger.
//! - **Classes.** [`is_gated`] work is refused when the envelope is spent:
//!   autonomous tasks, scheduled work, ambient, memory and comms assistance.
//!   Foreground chat, interactive tasks, evaluation and system calls are never
//!   refused (the owner must always be able to reach the tools that grant
//!   more) but they are counted.
//!
//! # Parking
//!
//! A refused call fails with `LLMError::SpendEnvelopeExhausted`. The agentic
//! loop's decide-error path ([`park_wait_for_error`]) turns that into a
//! `BoundaryOutcome::Retry(wait)`: the run neither fails nor asks anyone, it
//! decides again after `wait` (the stateless driver commits that as a durable
//! retry time; no worker is held). No retry or abort counter moves and nothing
//! is re-charged on the way back in.
//!
//! # Cost of the hot path
//!
//! Admission is a clock read and a few relaxed atomic loads. Settlement is a
//! handful of atomic adds plus one short, uncontended lock for the per-scope
//! breakdown. Nothing is written per call: a ticker task checkpoints at most
//! every five seconds (plain write-and-rename, never `fsync`; the transient
//! loss window of a crash is five seconds of spend) and once on shutdown.
//!
//! # Durable state
//!
//! - `system/spend_envelope.json` — the checkpoint (this window's spend and
//!   its breakdowns, lifetime spend).
//! - `system/spend_envelope_grants.json` — the owner's append-only top-ups,
//!   each `add_cost_usd` for the window it was made in. Never unlimited.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use magicllm::{
    LLMError, LLMProviderKind, LLMRequest, LlmWorkloadClass, SpendGate, SpendSettlement,
    SPEND_ENVELOPE_EXHAUSTED_MARKER,
};
use serde::{Deserialize, Serialize};

use crate::config::{BudgetCapUsd, SpendEnvelopeSettings, SpendEnvelopeWindow, UnpricedPolicy};
use crate::magician_v2::artifact_v2::ScopeRef;
use crate::magician_v2::feed::types::{FeedItem, FeedItemStatus, FeedItemType};
use crate::magician_v2::resource_authority::token::{period_window_start_at, CeilingPeriod};

/// The checkpoint, under the runtime root's `system/` directory.
pub const STATE_FILE: &str = "spend_envelope.json";
/// The owner's append-only top-ups, beside the checkpoint.
pub const GRANTS_FILE: &str = "spend_envelope_grants.json";
/// The one attention item raised while the envelope is spent.
pub const EXHAUSTED_ITEM_ID: &str = "spend_envelope:exhausted";
/// The warning item raised at `warn_at_pct`.
pub const WARNING_ITEM_ID: &str = "spend_envelope:warning";
/// `source` of a grant made through the owner API.
pub const OWNER_GRANT_SOURCE: &str = "owner";

/// Sentinel for "no ceiling" in the atomic limits.
const UNLIMITED: u64 = u64::MAX;
/// The checkpoint is written at most this often.
const CHECKPOINT_EVERY_MS: i64 = 5_000;
/// How often the ticker reconciles attention items and checks the debounce.
const TICK: Duration = Duration::from_secs(2);
/// A parked run decides again no sooner than this...
pub const MIN_PARK_WAIT: Duration = Duration::from_secs(60);
/// ...and no later than this. Bounded so an owner grant wakes a parked run
/// within minutes, and so the stateless driver's committed retry time stays
/// well under its one-hour clamp.
pub const MAX_PARK_WAIT: Duration = Duration::from_secs(10 * 60);
/// Per-scope breakdown entries kept; the rest fold into `"other"`.
const MAX_SCOPES: usize = 256;

const CLASSES: [LlmWorkloadClass; 9] = [
    LlmWorkloadClass::ForegroundChat,
    LlmWorkloadClass::InteractiveTask,
    LlmWorkloadClass::AutonomousTask,
    LlmWorkloadClass::Scheduled,
    LlmWorkloadClass::Ambient,
    LlmWorkloadClass::CommsAssist,
    LlmWorkloadClass::Memory,
    LlmWorkloadClass::Evaluation,
    LlmWorkloadClass::System,
];

fn class_index(class: LlmWorkloadClass) -> usize {
    match class {
        LlmWorkloadClass::ForegroundChat => 0,
        LlmWorkloadClass::InteractiveTask => 1,
        LlmWorkloadClass::AutonomousTask => 2,
        LlmWorkloadClass::Scheduled => 3,
        LlmWorkloadClass::Ambient => 4,
        LlmWorkloadClass::CommsAssist => 5,
        LlmWorkloadClass::Memory => 6,
        LlmWorkloadClass::Evaluation => 7,
        LlmWorkloadClass::System => 8,
    }
}

/// Whether work of this class is refused once the envelope is spent.
///
/// Autonomous tasks, scheduled work, ambient, memory and comms assistance are
/// gated. Foreground chat, interactive tasks, evaluation and system calls are
/// exempt but still counted: the owner must always be able to reach the tools
/// that grant more, so a guard here would be gated behind itself.
pub fn is_gated(class: LlmWorkloadClass) -> bool {
    match class {
        LlmWorkloadClass::AutonomousTask
        | LlmWorkloadClass::Scheduled
        | LlmWorkloadClass::Ambient
        | LlmWorkloadClass::Memory
        | LlmWorkloadClass::CommsAssist => true,
        LlmWorkloadClass::ForegroundChat
        | LlmWorkloadClass::InteractiveTask
        | LlmWorkloadClass::Evaluation
        | LlmWorkloadClass::System => false,
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn micro_to_usd(micro: u64) -> f64 {
    micro as f64 / 1_000_000.0
}

fn usd_to_micro(usd: f64) -> Option<u64> {
    magicllm::pricing::usd_to_microusd_ceil(usd)
}

fn cap_to_micro(cap: BudgetCapUsd) -> u64 {
    match cap {
        BudgetCapUsd::Usd(usd) => usd_to_micro(usd).unwrap_or(UNLIMITED),
        BudgetCapUsd::Unlimited => UNLIMITED,
    }
}

fn limit_to_usd(micro: u64) -> Option<f64> {
    (micro != UNLIMITED).then(|| micro_to_usd(micro))
}

// ---------------------------------------------------------------------------
// Window math
// ---------------------------------------------------------------------------

pub(crate) fn period_of(window: SpendEnvelopeWindow) -> CeilingPeriod {
    match window {
        SpendEnvelopeWindow::Hourly => CeilingPeriod::Hourly,
        SpendEnvelopeWindow::Daily => CeilingPeriod::Daily,
        SpendEnvelopeWindow::Weekly => CeilingPeriod::Weekly,
        SpendEnvelopeWindow::Monthly => CeilingPeriod::Monthly,
    }
}

fn window_code(window: SpendEnvelopeWindow) -> u8 {
    match window {
        SpendEnvelopeWindow::Hourly => 0,
        SpendEnvelopeWindow::Daily => 1,
        SpendEnvelopeWindow::Weekly => 2,
        SpendEnvelopeWindow::Monthly => 3,
    }
}

fn window_from_code(code: u8) -> SpendEnvelopeWindow {
    match code {
        0 => SpendEnvelopeWindow::Hourly,
        2 => SpendEnvelopeWindow::Weekly,
        3 => SpendEnvelopeWindow::Monthly,
        _ => SpendEnvelopeWindow::Daily,
    }
}

fn offset_ms(offset_minutes: i32) -> i64 {
    i64::from(offset_minutes.clamp(-1439, 1439)) * 60_000
}

/// Start (epoch millis) of the window containing `at_ms`: the UTC period
/// boundary of the shifted clock, shifted back by `offset_minutes`. With an
/// offset of 420 a daily window runs 07:00 UTC to 07:00 UTC.
pub fn window_start_ms(window: SpendEnvelopeWindow, offset_minutes: i32, at_ms: i64) -> i64 {
    let shift = offset_ms(offset_minutes);
    let shifted = DateTime::<Utc>::from_timestamp_millis(at_ms.saturating_sub(shift))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    period_window_start_at(&period_of(window), shifted)
        .timestamp_millis()
        .saturating_add(shift)
}

/// Start (epoch millis) of the window after the one that began at `start_ms`.
pub fn next_window_start_ms(
    window: SpendEnvelopeWindow,
    offset_minutes: i32,
    start_ms: i64,
) -> i64 {
    const HOUR_MS: i64 = 3_600_000;
    const DAY_MS: i64 = 24 * HOUR_MS;
    match window {
        SpendEnvelopeWindow::Hourly => start_ms.saturating_add(HOUR_MS),
        SpendEnvelopeWindow::Daily => start_ms.saturating_add(DAY_MS),
        SpendEnvelopeWindow::Weekly => start_ms.saturating_add(7 * DAY_MS),
        // A month is 28-31 days: 32 days past its first instant is always
        // inside the next month, whose own boundary is the answer.
        SpendEnvelopeWindow::Monthly => {
            window_start_ms(window, offset_minutes, start_ms.saturating_add(32 * DAY_MS))
        },
    }
}

// ---------------------------------------------------------------------------
// Persisted shapes
// ---------------------------------------------------------------------------

/// The debounced checkpoint: enough to resume the window's count after a
/// restart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedEnvelope {
    #[serde(default)]
    pub version: u32,
    /// Start of the window `spent_micro` belongs to.
    #[serde(default)]
    pub window_start_ms: i64,
    #[serde(default)]
    pub spent_micro: u64,
    /// Lifetime spend across every window.
    #[serde(default)]
    pub total_spent_micro: u64,
    /// Lifetime micro-USD that was estimated rather than priced.
    #[serde(default)]
    pub estimated_micro: u64,
    #[serde(default)]
    pub estimated_calls: u64,
    #[serde(default)]
    pub by_class: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_scope: BTreeMap<String, u64>,
    /// Spend counted but not yet posted to resource authority's ledger, by
    /// `principal/workspace` (`spend_envelope_ledger`). Not per window: a
    /// window rolling over never drops it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unposted_by_scope: BTreeMap<String, u64>,
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// One owner top-up: more money for the window it was made in. There is no
/// unlimited form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeGrant {
    pub granted_at: DateTime<Utc>,
    pub add_cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantsFile {
    #[serde(default)]
    grants: Vec<EnvelopeGrant>,
}

fn grants_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// The recorded grants. A missing file is none; an unreadable one is logged
/// and counts as none (the conservative reading: less money, never more).
pub fn load_grants(path: &Path) -> Vec<EnvelopeGrant> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<GrantsFile>(&bytes) {
            Ok(file) => file.grants,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "[SPEND-ENVELOPE] grants file unreadable; no grants are applied"
                );
                Vec::new()
            },
        },
        Err(_) => Vec::new(),
    }
}

/// Refuse a grant that adds nothing or adds a non-positive / non-finite
/// amount.
pub fn validate_grant_usd(add_cost_usd: f64) -> Result<(), String> {
    if add_cost_usd.is_finite() && add_cost_usd > 0.0 && usd_to_micro(add_cost_usd).is_some() {
        Ok(())
    } else {
        Err(format!(
            "add_cost_usd must be a positive finite dollar amount, got {add_cost_usd}"
        ))
    }
}

/// Append one grant through the shared durable writer. Returns every grant
/// now on record.
pub fn append_grant(path: &Path, grant: EnvelopeGrant) -> std::io::Result<Vec<EnvelopeGrant>> {
    validate_grant_usd(grant.add_cost_usd)
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why))?;
    let _guard = grants_write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut grants = match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice::<GrantsFile>(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
                .grants
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    grants.push(grant);
    let body = serde_json::to_vec_pretty(&GrantsFile {
        grants: grants.clone(),
    })
    .map_err(std::io::Error::other)?;
    magician_core::durable_io::write_bytes_durably_sync(path, &body)?;
    Ok(grants)
}

/// Write the checkpoint through the shared durable writer (unique staging
/// file, file sync, atomic rename, parent sync). It runs at most every five
/// seconds and only when something changed, never per model call.
fn write_state_file(path: &Path, state: &PersistedEnvelope) -> std::io::Result<()> {
    static WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let body = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    magician_core::durable_io::write_bytes_durably_sync(path, &body)
}

fn read_state_file(path: &Path) -> Option<PersistedEnvelope> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<PersistedEnvelope>(&bytes) {
        Ok(state) => Some(state),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "[SPEND-ENVELOPE] checkpoint unreadable; counting restarts from zero"
            );
            None
        },
    }
}

// ---------------------------------------------------------------------------
// Status + notifications
// ---------------------------------------------------------------------------

/// One scope's share of the window's spend.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopeSpend {
    pub scope: String,
    pub spent_usd: f64,
}

/// What the owner sees (`GET /budgets/envelope`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvelopeStatus {
    pub enabled: bool,
    pub window: &'static str,
    pub reset_offset_minutes: i32,
    pub window_start_ms: i64,
    pub resets_at_ms: i64,
    /// The configured ceiling per window; `None` is unlimited.
    pub daily_limit_usd: Option<f64>,
    /// Top-ups made in this window.
    pub granted_usd: f64,
    /// Ceiling plus this window's grants; `None` is unlimited.
    pub effective_limit_usd: Option<f64>,
    pub spent_usd: f64,
    pub remaining_usd: Option<f64>,
    pub used_pct: Option<f64>,
    pub total_limit_usd: Option<f64>,
    pub total_granted_usd: f64,
    pub total_spent_usd: f64,
    pub warn_at_pct: u8,
    pub warning: bool,
    pub exhausted: bool,
    /// `"window"` or `"total"` while exhausted.
    pub exhausted_by: Option<&'static str>,
    pub unpriced: &'static str,
    /// Lifetime spend that was estimated because the model was unpriced.
    pub estimated_usd: f64,
    pub estimated_calls: u64,
    pub by_class: BTreeMap<String, f64>,
    pub by_scope: Vec<ScopeSpend>,
    pub gated_classes: Vec<&'static str>,
    pub exempt_classes: Vec<&'static str>,
}

/// Where the envelope stands, for attention reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeLevel {
    Ok,
    Warning,
    Exhausted,
}

/// Receives the envelope's attention transitions. Methods are synchronous and
/// must not block (implementations spawn their work).
pub trait EnvelopeNotifier: Send + Sync {
    /// The envelope just became exhausted: raise `spend_envelope:exhausted`.
    fn exhausted(&self, scope: &ScopeRef, status: &EnvelopeStatus);
    /// The envelope is no longer exhausted (window reset or grant).
    fn exhausted_cleared(&self, scope: &ScopeRef);
    /// Spend crossed `warn_at_pct`.
    fn warning(&self, scope: &ScopeRef, status: &EnvelopeStatus);
    fn warning_cleared(&self, scope: &ScopeRef);
}

fn exhausted_feed_item(scope: &ScopeRef, status: &EnvelopeStatus, now_ms: i64) -> FeedItem {
    let reason = match status.exhausted_by {
        Some("total") => "its lifetime ceiling".to_string(),
        _ => format!(
            "this {} window's ceiling of ${:.2}",
            status.window,
            status.effective_limit_usd.unwrap_or_default()
        ),
    };
    let until = if status.exhausted_by == Some("total") {
        "until you raise the lifetime ceiling or grant more".to_string()
    } else {
        format!(
            "until the window resets at {} or you grant more",
            DateTime::<Utc>::from_timestamp_millis(status.resets_at_ms)
                .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "its next boundary".to_string())
        )
    };
    FeedItem {
        id: EXHAUSTED_ITEM_ID.to_string(),
        principal: scope.principal().to_string(),
        workspace: scope.workspace().to_string(),
        item_type: FeedItemType::Escalation,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: "Spend envelope reached".to_string(),
        summary: Some(format!(
            "Autonomous work has spent ${:.2} against {reason}. It is parked {until}. \
             Owner chat and interactive work are unaffected. Grant more with \
             POST /api/magician/v2/budgets/envelope/grant.",
            status.spent_usd
        )),
        status: FeedItemStatus::NeedsAction,
        created_at: now_ms,
        updated_at: now_ms,
        actions: Vec::new(),
        metadata: serde_json::json!({
            "attention_kind": "spend_envelope_exhausted",
            "window": status.window,
            "resets_at_ms": status.resets_at_ms,
            "spent_usd": status.spent_usd,
            "effective_limit_usd": status.effective_limit_usd,
            "exhausted_by": status.exhausted_by,
        }),
    }
}

fn warning_feed_item(scope: &ScopeRef, status: &EnvelopeStatus, now_ms: i64) -> FeedItem {
    FeedItem {
        id: WARNING_ITEM_ID.to_string(),
        principal: scope.principal().to_string(),
        workspace: scope.workspace().to_string(),
        item_type: FeedItemType::Escalation,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: "Spend envelope nearly reached".to_string(),
        summary: Some(format!(
            "Autonomous work has used {:.0}% of this {} window's ${:.2}. Autonomous work parks \
             when it is spent.",
            status.used_pct.unwrap_or_default(),
            status.window,
            status.effective_limit_usd.unwrap_or_default()
        )),
        status: FeedItemStatus::Info,
        created_at: now_ms,
        updated_at: now_ms,
        actions: Vec::new(),
        metadata: serde_json::json!({
            "attention_kind": "spend_envelope_warning",
            "window": status.window,
            "resets_at_ms": status.resets_at_ms,
            "spent_usd": status.spent_usd,
            "used_pct": status.used_pct,
            "warn_at_pct": status.warn_at_pct,
        }),
    }
}

/// Raises and removes the envelope's feed items through the V3 feed
/// projection (the `upsert_monitor_attention_item` /
/// `remove_monitor_attention_item` pattern).
pub struct FeedEnvelopeNotifier {
    adapter: Arc<crate::magician_v2::feed::v3_projection::V3FeedProjectionAdapter>,
}

impl FeedEnvelopeNotifier {
    pub fn new(
        adapter: Arc<crate::magician_v2::feed::v3_projection::V3FeedProjectionAdapter>,
    ) -> Self {
        Self { adapter }
    }

    fn spawn(&self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(work);
            },
            Err(_) => tracing::debug!(
                "[SPEND-ENVELOPE] no async runtime; attention item change not applied"
            ),
        }
    }

    fn upsert(&self, scope: &ScopeRef, item: FeedItem) {
        let adapter = Arc::clone(&self.adapter);
        let scope = scope.clone();
        self.spawn(async move {
            // A previous owner dismissal must not hide a NEW exhaustion.
            if let Err(error) = adapter
                .record_monitor_attention_dismissal(&scope, &item.id, false)
                .await
            {
                tracing::warn!(%error, "[SPEND-ENVELOPE] could not clear a stale dismissal");
            }
            let id = item.id.clone();
            if let Err(error) = adapter.upsert_monitor_attention_item(item).await {
                tracing::warn!(%error, item_id = %id, "[SPEND-ENVELOPE] attention item not raised");
            }
        });
    }

    fn remove(&self, scope: &ScopeRef, item_id: &'static str) {
        let adapter = Arc::clone(&self.adapter);
        let scope = scope.clone();
        self.spawn(async move {
            if let Err(error) = adapter.remove_monitor_attention_item(&scope, item_id).await {
                tracing::warn!(%error, item_id, "[SPEND-ENVELOPE] attention item not removed");
            }
        });
    }
}

impl EnvelopeNotifier for FeedEnvelopeNotifier {
    fn exhausted(&self, scope: &ScopeRef, status: &EnvelopeStatus) {
        self.upsert(scope, exhausted_feed_item(scope, status, now_ms()));
    }

    fn exhausted_cleared(&self, scope: &ScopeRef) {
        self.remove(scope, EXHAUSTED_ITEM_ID);
    }

    fn warning(&self, scope: &ScopeRef, status: &EnvelopeStatus) {
        self.upsert(scope, warning_feed_item(scope, status, now_ms()));
    }

    fn warning_cleared(&self, scope: &ScopeRef) {
        self.remove(scope, WARNING_ITEM_ID);
    }
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/// Where the checkpoint and the grants live.
#[derive(Debug, Clone)]
struct EnvelopePaths {
    state: PathBuf,
    grants: PathBuf,
}

/// The process-wide spend envelope. All hot-path state is atomic; the settings
/// the hot path reads are mirrored into atomics by [`Self::apply_settings`].
pub struct SpendEnvelope {
    // Settings (mirrored).
    enabled: AtomicBool,
    daily_micro: AtomicU64,
    total_micro: AtomicU64,
    window: AtomicU8,
    offset_minutes: AtomicI32,
    warn_at_pct: AtomicU8,
    block_unpriced: AtomicBool,
    // The current window. `window_end_ms == 0` means "not started".
    window_start_ms: AtomicI64,
    window_end_ms: AtomicI64,
    spent_micro: AtomicU64,
    window_grant_micro: AtomicU64,
    by_class: [AtomicU64; 9],
    scopes: Mutex<HashMap<String, u64>>,
    /// Spend not yet posted to the ledger, by scope; drained by the poster.
    unposted: Mutex<HashMap<String, u64>>,
    roll_lock: Mutex<()>,
    // Lifetime.
    total_spent_micro: AtomicU64,
    total_grant_micro: AtomicU64,
    estimated_micro: AtomicU64,
    estimated_calls: AtomicU64,
    // Persistence + attention.
    dirty: AtomicBool,
    last_checkpoint_ms: AtomicI64,
    paths: Mutex<Option<EnvelopePaths>>,
    notifier: Mutex<Option<Arc<dyn EnvelopeNotifier>>>,
    notified: Mutex<Option<EnvelopeLevel>>,
    owner_scope: Mutex<(String, String)>,
}

impl Default for SpendEnvelope {
    fn default() -> Self {
        Self::with_settings(&SpendEnvelopeSettings::default())
    }
}

impl SpendEnvelope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_settings(settings: &SpendEnvelopeSettings) -> Self {
        let envelope = Self {
            enabled: AtomicBool::new(true),
            daily_micro: AtomicU64::new(UNLIMITED),
            total_micro: AtomicU64::new(UNLIMITED),
            window: AtomicU8::new(window_code(SpendEnvelopeWindow::Daily)),
            offset_minutes: AtomicI32::new(0),
            warn_at_pct: AtomicU8::new(80),
            block_unpriced: AtomicBool::new(false),
            window_start_ms: AtomicI64::new(0),
            window_end_ms: AtomicI64::new(0),
            spent_micro: AtomicU64::new(0),
            window_grant_micro: AtomicU64::new(0),
            by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            scopes: Mutex::new(HashMap::new()),
            unposted: Mutex::new(HashMap::new()),
            roll_lock: Mutex::new(()),
            total_spent_micro: AtomicU64::new(0),
            total_grant_micro: AtomicU64::new(0),
            estimated_micro: AtomicU64::new(0),
            estimated_calls: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            last_checkpoint_ms: AtomicI64::new(0),
            paths: Mutex::new(None),
            notifier: Mutex::new(None),
            notified: Mutex::new(None),
            owner_scope: Mutex::new((
                crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL.to_string(),
                crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE.to_string(),
            )),
        };
        envelope.apply_settings(settings, now_ms());
        envelope
    }

    /// Mirror the owner's settings into the hot-path atomics. A change of
    /// window kind or offset re-derives the window's bounds in place; the
    /// window's spend is kept.
    pub fn apply_settings(&self, settings: &SpendEnvelopeSettings, now_ms: i64) {
        self.enabled.store(settings.enabled, Ordering::Relaxed);
        self.daily_micro
            .store(cap_to_micro(settings.daily_usd), Ordering::Relaxed);
        self.total_micro
            .store(cap_to_micro(settings.total_usd), Ordering::Relaxed);
        self.warn_at_pct
            .store(settings.warn_at_pct.clamp(1, 100), Ordering::Relaxed);
        self.block_unpriced.store(
            settings.unpriced == UnpricedPolicy::Block,
            Ordering::Relaxed,
        );
        let window_changed = self
            .window
            .swap(window_code(settings.window), Ordering::Relaxed)
            != window_code(settings.window);
        let offset = settings.reset_offset_minutes.clamp(-1439, 1439);
        let offset_changed = self.offset_minutes.swap(offset, Ordering::Relaxed) != offset;
        if window_changed || offset_changed {
            self.rederive_bounds(now_ms);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn window_end_ms(&self) -> i64 {
        self.window_end_ms.load(Ordering::Acquire)
    }

    fn window_kind(&self) -> SpendEnvelopeWindow {
        window_from_code(self.window.load(Ordering::Relaxed))
    }

    fn offset(&self) -> i32 {
        self.offset_minutes.load(Ordering::Relaxed)
    }

    /// Re-derive the current window's bounds without resetting its spend.
    fn rederive_bounds(&self, now_ms: i64) {
        let _guard = self
            .roll_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.window_end_ms.load(Ordering::Acquire) == 0 {
            return;
        }
        let start = window_start_ms(self.window_kind(), self.offset(), now_ms);
        let end = next_window_start_ms(self.window_kind(), self.offset(), start);
        self.window_start_ms.store(start, Ordering::Relaxed);
        self.window_end_ms.store(end, Ordering::Release);
    }

    /// Start a new window when `now_ms` has passed the current one's end (or
    /// none has started). Spend, grants and breakdowns of the old window are
    /// dropped; lifetime counters stay.
    fn roll_if_due(&self, now_ms: i64) {
        if now_ms < self.window_end_ms.load(Ordering::Acquire) {
            return;
        }
        let _guard = self
            .roll_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if now_ms < self.window_end_ms.load(Ordering::Acquire) {
            return;
        }
        let start = window_start_ms(self.window_kind(), self.offset(), now_ms);
        let end = next_window_start_ms(self.window_kind(), self.offset(), start);
        self.spent_micro.store(0, Ordering::Relaxed);
        self.window_grant_micro.store(0, Ordering::Relaxed);
        for class in &self.by_class {
            class.store(0, Ordering::Relaxed);
        }
        self.scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.window_start_ms.store(start, Ordering::Relaxed);
        self.window_end_ms.store(end, Ordering::Release);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Whether the envelope is spent, and when to look again: the window's end
    /// for the window ceiling, an hour out for the lifetime ceiling (it has no
    /// automatic reset).
    fn exhaustion(&self, now_ms: i64) -> Option<(i64, &'static str)> {
        let daily = self.daily_micro.load(Ordering::Relaxed);
        if daily != UNLIMITED {
            let limit = daily.saturating_add(self.window_grant_micro.load(Ordering::Relaxed));
            if self.spent_micro.load(Ordering::Relaxed) >= limit {
                return Some((self.window_end_ms.load(Ordering::Acquire), "window"));
            }
        }
        let total = self.total_micro.load(Ordering::Relaxed);
        if total != UNLIMITED {
            let limit = total.saturating_add(self.total_grant_micro.load(Ordering::Relaxed));
            if self.total_spent_micro.load(Ordering::Relaxed) >= limit {
                return Some((now_ms.saturating_add(3_600_000), "total"));
            }
        }
        None
    }

    /// Admission for a workload class: `Err(resets_at_ms)` when the class is
    /// gated and the envelope is spent. Exempt classes always pass.
    pub fn check_at(&self, class: LlmWorkloadClass, now_ms: i64) -> Result<(), i64> {
        if !self.enabled() {
            return Ok(());
        }
        self.roll_if_due(now_ms);
        if !is_gated(class) {
            return Ok(());
        }
        match self.exhaustion(now_ms) {
            Some((resets_at_ms, _)) => Err(resets_at_ms),
            None => Ok(()),
        }
    }

    /// The router's pre-call gate: refuses a gated class when the envelope is
    /// spent, and (under `unpriced = block`) a model the pricing table cannot
    /// price.
    pub fn admit(
        &self,
        class: LlmWorkloadClass,
        provider: &LLMProviderKind,
        model: &str,
        now_ms: i64,
    ) -> Result<(), LLMError> {
        if let Err(resets_at_ms) = self.check_at(class, now_ms) {
            return Err(LLMError::SpendEnvelopeExhausted { resets_at_ms });
        }
        if self.enabled()
            && is_gated(class)
            && self.block_unpriced.load(Ordering::Relaxed)
            && !magicllm::model_is_priced_at(provider, model, now_ms)
        {
            return Err(LLMError::Validation(format!(
                "spend envelope: model `{provider}/{model}` has no pricing row and \
                 execution.budgets.spend_envelope.unpriced is `block`"
            )));
        }
        Ok(())
    }

    /// Count `micro` micro-USD of spend. Exempt classes are counted too.
    pub fn charge_at(
        &self,
        class: LlmWorkloadClass,
        principal: &str,
        workspace: &str,
        micro: u64,
        estimated: bool,
        now_ms: i64,
    ) {
        self.roll_if_due(now_ms);
        if micro > 0 {
            self.spent_micro.fetch_add(micro, Ordering::Relaxed);
            self.total_spent_micro.fetch_add(micro, Ordering::Relaxed);
            self.by_class[class_index(class)].fetch_add(micro, Ordering::Relaxed);
            let mut scopes = self
                .scopes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let key = format!("{principal}/{workspace}");
            if scopes.len() >= MAX_SCOPES && !scopes.contains_key(&key) {
                *scopes.entry("other".to_string()).or_insert(0) += micro;
            } else {
                *scopes.entry(key.clone()).or_insert(0) += micro;
            }
            drop(scopes);
            let mut unposted = self
                .unposted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if unposted.len() >= MAX_SCOPES && !unposted.contains_key(&key) {
                *unposted.entry("other".to_string()).or_insert(0) += micro;
            } else {
                *unposted.entry(key).or_insert(0) += micro;
            }
        }
        if estimated && micro > 0 {
            self.estimated_calls.fetch_add(1, Ordering::Relaxed);
            self.estimated_micro.fetch_add(micro, Ordering::Relaxed);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// The router's post-call settlement: the exact priced cost when the
    /// pricing table had it, else (`unpriced = estimate`, and also for exempt
    /// classes under `block`) the highest rate the table knows for the
    /// provider on the reported tokens, or an output-ceiling estimate when the
    /// provider reported none.
    pub fn settle_at(&self, settlement: &SpendSettlement<'_>, now_ms: i64) {
        let (micro, estimated) = match settlement.priced_micro_usd {
            Some(micro) => (micro, false),
            None => (
                magicllm::estimate_unpriced_cost_microusd(
                    settlement.provider,
                    settlement.usage,
                    settlement.output_ceiling_tokens,
                    settlement.at_ms,
                ),
                true,
            ),
        };
        self.charge_at(
            settlement.workload,
            settlement.principal,
            settlement.workspace,
            micro,
            estimated,
            now_ms,
        );
    }

    /// Record an owner top-up. It counts for this window when `granted_at_ms`
    /// falls inside it, and always toward the lifetime ceiling.
    pub fn record_grant(&self, add_cost_usd: f64, granted_at_ms: i64, now_ms: i64) {
        let Some(micro) = usd_to_micro(add_cost_usd).filter(|m| *m > 0) else {
            return;
        };
        self.roll_if_due(now_ms);
        self.total_grant_micro.fetch_add(micro, Ordering::Relaxed);
        if granted_at_ms >= self.window_start_ms.load(Ordering::Relaxed)
            && granted_at_ms < self.window_end_ms.load(Ordering::Acquire)
        {
            self.window_grant_micro.fetch_add(micro, Ordering::Relaxed);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Replace the grant totals from the recorded grants (boot).
    pub fn load_grants(&self, grants: &[EnvelopeGrant], now_ms: i64) {
        self.roll_if_due(now_ms);
        let start = self.window_start_ms.load(Ordering::Relaxed);
        let end = self.window_end_ms.load(Ordering::Acquire);
        let mut total = 0_u64;
        let mut window = 0_u64;
        for grant in grants {
            let Some(micro) = usd_to_micro(grant.add_cost_usd).filter(|m| *m > 0) else {
                continue;
            };
            total = total.saturating_add(micro);
            let at = grant.granted_at.timestamp_millis();
            if at >= start && at < end {
                window = window.saturating_add(micro);
            }
        }
        self.total_grant_micro.store(total, Ordering::Relaxed);
        self.window_grant_micro.store(window, Ordering::Relaxed);
    }

    // -- ledger posting -----------------------------------------------------

    /// Take every scope's spend not yet posted to the ledger
    /// (`spend_envelope_ledger`). What a post fails to record goes back
    /// through [`Self::return_unposted`]; the checkpoint carries the rest.
    pub fn take_unposted(&self) -> Vec<(String, u64)> {
        let taken: Vec<(String, u64)> = self
            .unposted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .filter(|(_, micro)| *micro > 0)
            .collect();
        if !taken.is_empty() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        taken
    }

    /// Put back spend a ledger post could not record.
    pub fn return_unposted(&self, scope: &str, micro: u64) {
        if micro == 0 {
            return;
        }
        *self
            .unposted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(scope.to_string())
            .or_insert(0) += micro;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// The scope owner attention items and top-ups belong to.
    pub fn owner_scope(&self) -> (String, String) {
        self.owner_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    // -- persistence --------------------------------------------------------

    pub fn snapshot_persisted(&self, now_ms: i64) -> PersistedEnvelope {
        let by_class = CLASSES
            .iter()
            .filter_map(|class| {
                let micro = self.by_class[class_index(*class)].load(Ordering::Relaxed);
                (micro > 0).then(|| (class.as_str().to_string(), micro))
            })
            .collect();
        let by_scope = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(scope, micro)| (scope.clone(), *micro))
            .collect();
        let unposted_by_scope = self
            .unposted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, micro)| **micro > 0)
            .map(|(scope, micro)| (scope.clone(), *micro))
            .collect();
        PersistedEnvelope {
            version: 1,
            window_start_ms: self.window_start_ms.load(Ordering::Relaxed),
            spent_micro: self.spent_micro.load(Ordering::Relaxed),
            total_spent_micro: self.total_spent_micro.load(Ordering::Relaxed),
            estimated_micro: self.estimated_micro.load(Ordering::Relaxed),
            estimated_calls: self.estimated_calls.load(Ordering::Relaxed),
            by_class,
            by_scope,
            unposted_by_scope,
            updated_at_ms: now_ms,
        }
    }

    /// Resume from a checkpoint. Lifetime counters always come back; the
    /// window's spend and breakdowns only when the checkpoint belongs to the
    /// window `now_ms` is in (otherwise the window starts empty, lazily).
    pub fn restore(&self, persisted: &PersistedEnvelope, now_ms: i64) {
        let _guard = self
            .roll_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.total_spent_micro
            .store(persisted.total_spent_micro, Ordering::Relaxed);
        self.estimated_micro
            .store(persisted.estimated_micro, Ordering::Relaxed);
        self.estimated_calls
            .store(persisted.estimated_calls, Ordering::Relaxed);
        // Unposted spend is not per window: it comes back whatever window
        // the checkpoint was written in.
        *self
            .unposted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = persisted
            .unposted_by_scope
            .iter()
            .map(|(scope, micro)| (scope.clone(), *micro))
            .collect();
        let current_start = window_start_ms(self.window_kind(), self.offset(), now_ms);
        if persisted.window_start_ms != current_start {
            return;
        }
        let end = next_window_start_ms(self.window_kind(), self.offset(), current_start);
        self.spent_micro
            .store(persisted.spent_micro, Ordering::Relaxed);
        for class in CLASSES {
            let micro = persisted.by_class.get(class.as_str()).copied().unwrap_or(0);
            self.by_class[class_index(class)].store(micro, Ordering::Relaxed);
        }
        *self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = persisted
            .by_scope
            .iter()
            .map(|(scope, micro)| (scope.clone(), *micro))
            .collect();
        self.window_start_ms.store(current_start, Ordering::Relaxed);
        self.window_end_ms.store(end, Ordering::Release);
    }

    /// Whether a checkpoint is due: something changed and the debounce has
    /// passed. Marks the checkpoint taken.
    fn take_checkpoint_due(&self, now_ms: i64) -> Option<PersistedEnvelope> {
        if !self.dirty.load(Ordering::Relaxed) {
            return None;
        }
        let last = self.last_checkpoint_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < CHECKPOINT_EVERY_MS {
            return None;
        }
        self.dirty.store(false, Ordering::Relaxed);
        self.last_checkpoint_ms.store(now_ms, Ordering::Relaxed);
        Some(self.snapshot_persisted(now_ms))
    }

    fn state_path(&self) -> Option<PathBuf> {
        self.paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|paths| paths.state.clone())
    }

    /// The grants file, once the envelope is attached to a runtime root.
    pub fn grants_path(&self) -> Option<PathBuf> {
        self.paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|paths| paths.grants.clone())
    }

    /// Write the checkpoint now when one is due. The write runs on the
    /// blocking pool; a failure re-arms the debounce.
    pub async fn checkpoint_if_due(self: &Arc<Self>, now_ms: i64) {
        let Some(path) = self.state_path() else {
            return;
        };
        let Some(state) = self.take_checkpoint_due(now_ms) else {
            return;
        };
        let envelope = Arc::clone(self);
        let result = tokio::task::spawn_blocking(move || write_state_file(&path, &state)).await;
        if !matches!(result, Ok(Ok(()))) {
            envelope.dirty.store(true, Ordering::Relaxed);
            tracing::warn!(?result, "[SPEND-ENVELOPE] checkpoint not written");
        }
    }

    /// Write the checkpoint unconditionally (shutdown).
    pub async fn flush(self: &Arc<Self>) {
        let Some(path) = self.state_path() else {
            return;
        };
        let state = self.snapshot_persisted(now_ms());
        self.dirty.store(false, Ordering::Relaxed);
        let result = tokio::task::spawn_blocking(move || write_state_file(&path, &state)).await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(?result, "[SPEND-ENVELOPE] final checkpoint not written");
        }
    }

    // -- status + attention -------------------------------------------------

    pub fn status_at(&self, now_ms: i64) -> EnvelopeStatus {
        self.roll_if_due(now_ms);
        let daily = self.daily_micro.load(Ordering::Relaxed);
        let granted = self.window_grant_micro.load(Ordering::Relaxed);
        let spent = self.spent_micro.load(Ordering::Relaxed);
        let effective = (daily != UNLIMITED).then(|| daily.saturating_add(granted));
        let total = self.total_micro.load(Ordering::Relaxed);
        let total_granted = self.total_grant_micro.load(Ordering::Relaxed);
        let total_spent = self.total_spent_micro.load(Ordering::Relaxed);
        let used_pct = effective.map(|limit| {
            if limit == 0 {
                100.0
            } else {
                spent as f64 * 100.0 / limit as f64
            }
        });
        let total_pct = (total != UNLIMITED).then(|| {
            let limit = total.saturating_add(total_granted);
            if limit == 0 {
                100.0
            } else {
                total_spent as f64 * 100.0 / limit as f64
            }
        });
        let worst_pct = match (used_pct, total_pct) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let exhaustion = self.exhaustion(now_ms);
        let exhausted = self.enabled() && exhaustion.is_some();
        let warn_at = self.warn_at_pct.load(Ordering::Relaxed);
        let warning =
            self.enabled() && !exhausted && worst_pct.is_some_and(|pct| pct >= f64::from(warn_at));
        let by_class = CLASSES
            .iter()
            .filter_map(|class| {
                let micro = self.by_class[class_index(*class)].load(Ordering::Relaxed);
                (micro > 0).then(|| (class.as_str().to_string(), micro_to_usd(micro)))
            })
            .collect();
        let mut by_scope: Vec<ScopeSpend> = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(scope, micro)| ScopeSpend {
                scope: scope.clone(),
                spent_usd: micro_to_usd(*micro),
            })
            .collect();
        by_scope.sort_by(|a, b| {
            b.spent_usd
                .partial_cmp(&a.spent_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.scope.cmp(&b.scope))
        });
        by_scope.truncate(20);
        EnvelopeStatus {
            enabled: self.enabled(),
            window: self.window_kind().as_str(),
            reset_offset_minutes: self.offset(),
            window_start_ms: self.window_start_ms.load(Ordering::Relaxed),
            resets_at_ms: self.window_end_ms.load(Ordering::Acquire),
            daily_limit_usd: limit_to_usd(daily),
            granted_usd: micro_to_usd(granted),
            effective_limit_usd: effective.map(micro_to_usd),
            spent_usd: micro_to_usd(spent),
            remaining_usd: effective.map(|limit| micro_to_usd(limit.saturating_sub(spent))),
            used_pct,
            total_limit_usd: limit_to_usd(total),
            total_granted_usd: micro_to_usd(total_granted),
            total_spent_usd: micro_to_usd(total_spent),
            warn_at_pct: warn_at,
            warning,
            exhausted,
            exhausted_by: exhaustion.filter(|_| exhausted).map(|(_, by)| by),
            unpriced: if self.block_unpriced.load(Ordering::Relaxed) {
                UnpricedPolicy::Block.as_str()
            } else {
                UnpricedPolicy::Estimate.as_str()
            },
            estimated_usd: micro_to_usd(self.estimated_micro.load(Ordering::Relaxed)),
            estimated_calls: self.estimated_calls.load(Ordering::Relaxed),
            by_class,
            by_scope,
            gated_classes: CLASSES
                .iter()
                .filter(|class| is_gated(**class))
                .map(|class| class.as_str())
                .collect(),
            exempt_classes: CLASSES
                .iter()
                .filter(|class| !is_gated(**class))
                .map(|class| class.as_str())
                .collect(),
        }
    }

    /// Remember which scope the owner's attention items belong to.
    pub fn note_owner_scope(&self, principal: &str, workspace: &str) {
        *self
            .owner_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            (principal.to_string(), workspace.to_string());
    }

    fn owner_scope_ref(&self) -> ScopeRef {
        let (principal, workspace) = self
            .owner_scope
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        ScopeRef::system_internal_unauthenticated(&principal, &workspace)
    }

    pub fn set_notifier(&self, notifier: Arc<dyn EnvelopeNotifier>) {
        *self
            .notifier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(notifier);
        // Re-announce the current level to the new sink.
        *self
            .notified
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Bring the attention items in line with the envelope's level: raise
    /// `spend_envelope:exhausted` once when it is spent, remove it when it is
    /// not (window reset or a grant), and likewise the warning at
    /// `warn_at_pct`. The first call after a notifier is attached also clears
    /// whatever a previous process left behind.
    pub fn reconcile_at(&self, now_ms: i64) {
        let notifier = self
            .notifier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(notifier) = notifier else {
            return;
        };
        let status = self.status_at(now_ms);
        let level = if status.exhausted {
            EnvelopeLevel::Exhausted
        } else if status.warning {
            EnvelopeLevel::Warning
        } else {
            EnvelopeLevel::Ok
        };
        let scope = self.owner_scope_ref();
        let mut notified = self
            .notified
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = *notified;
        if previous == Some(level) {
            return;
        }
        *notified = Some(level);
        let unknown = previous.is_none();
        match level {
            EnvelopeLevel::Exhausted => {
                if unknown || previous == Some(EnvelopeLevel::Warning) {
                    notifier.warning_cleared(&scope);
                }
                tracing::warn!(
                    spent_usd = status.spent_usd,
                    limit_usd = ?status.effective_limit_usd,
                    resets_at_ms = status.resets_at_ms,
                    "[SPEND-ENVELOPE] exhausted; autonomous LLM work is parked"
                );
                notifier.exhausted(&scope, &status);
            },
            EnvelopeLevel::Warning => {
                if unknown || previous == Some(EnvelopeLevel::Exhausted) {
                    notifier.exhausted_cleared(&scope);
                }
                tracing::warn!(
                    spent_usd = status.spent_usd,
                    used_pct = ?status.used_pct,
                    "[SPEND-ENVELOPE] warning threshold crossed"
                );
                notifier.warning(&scope, &status);
            },
            EnvelopeLevel::Ok => {
                if unknown || previous == Some(EnvelopeLevel::Exhausted) {
                    notifier.exhausted_cleared(&scope);
                }
                if unknown || previous == Some(EnvelopeLevel::Warning) {
                    notifier.warning_cleared(&scope);
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Router gate
// ---------------------------------------------------------------------------

/// The `magicllm` gate over an envelope.
pub struct EnvelopeGate {
    envelope: Arc<SpendEnvelope>,
}

impl EnvelopeGate {
    pub fn new(envelope: Arc<SpendEnvelope>) -> Self {
        Self { envelope }
    }
}

impl SpendGate for EnvelopeGate {
    fn admit(&self, request: &LLMRequest, provider: &LLMProviderKind) -> Result<(), LLMError> {
        self.envelope.admit(
            magicllm::spend_gate::workload_of(request),
            provider,
            &request.model,
            now_ms(),
        )
    }

    fn settle(&self, settlement: &SpendSettlement<'_>) {
        self.envelope.settle_at(settlement, now_ms());
    }

    fn admit_embedding(
        &self,
        request: &magicllm::EmbeddingRequest,
        provider: &LLMProviderKind,
    ) -> Result<(), LLMError> {
        self.envelope.admit(
            magicllm::spend_gate::workload_of_metadata(&request.metadata),
            provider,
            &request.model,
            now_ms(),
        )
    }
}

// ---------------------------------------------------------------------------
// Process-wide singleton
// ---------------------------------------------------------------------------

static ENVELOPE: OnceLock<Arc<SpendEnvelope>> = OnceLock::new();

fn published_settings() -> &'static Mutex<SpendEnvelopeSettings> {
    static PUBLISHED: OnceLock<Mutex<SpendEnvelopeSettings>> = OnceLock::new();
    PUBLISHED.get_or_init(|| Mutex::new(SpendEnvelopeSettings::default()))
}

/// The installed envelope, once [`install_process_envelope`] ran. `None` in
/// unit tests and CLI commands: nothing is gated or counted there.
pub fn envelope() -> Option<&'static Arc<SpendEnvelope>> {
    ENVELOPE.get()
}

/// Whether the envelope now governs spend, so the per-run token meter stands
/// down: the meter counts cached tokens, so it tracks context resend, not
/// money.
pub fn supersedes_token_meter() -> bool {
    envelope().is_some_and(|envelope| envelope.enabled())
}

/// Publish `execution.budgets.spend_envelope` (called from
/// `configure_execution_budgets`, at boot, on reload and by the PUT
/// endpoint). Before the envelope is installed the settings are only
/// remembered.
pub fn publish_settings(settings: &SpendEnvelopeSettings) {
    *published_settings()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = *settings;
    if let Some(envelope) = envelope() {
        envelope.apply_settings(settings, now_ms());
    }
}

/// Install the process-wide envelope under `base_root` (the runtime root):
/// load the checkpoint and the grants, install the router gate into
/// `magicllm`, and start the ticker that checkpoints and reconciles attention
/// items. Call once at server start, inside the async runtime. Idempotent.
pub fn install_process_envelope(base_root: &Path) -> Arc<SpendEnvelope> {
    let settings = *published_settings()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut fresh = false;
    let envelope = ENVELOPE.get_or_init(|| {
        fresh = true;
        Arc::new(SpendEnvelope::with_settings(&settings))
    });
    if !fresh {
        return Arc::clone(envelope);
    }
    let system = base_root.join("system");
    let paths = EnvelopePaths {
        state: system.join(STATE_FILE),
        grants: system.join(GRANTS_FILE),
    };
    let now = now_ms();
    if let Some(persisted) = read_state_file(&paths.state) {
        envelope.restore(&persisted, now);
    }
    envelope.load_grants(&load_grants(&paths.grants), now);
    *envelope
        .paths
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(paths);
    if !magicllm::install_spend_gate(Arc::new(EnvelopeGate::new(Arc::clone(envelope)))) {
        tracing::warn!("[SPEND-ENVELOPE] a spend gate was already installed; keeping it");
    }
    spawn_ticker(Arc::clone(envelope));
    let status = envelope.status_at(now);
    tracing::info!(
        enabled = status.enabled,
        window = status.window,
        limit_usd = ?status.effective_limit_usd,
        spent_usd = status.spent_usd,
        "[SPEND-ENVELOPE] installed"
    );
    Arc::clone(envelope)
}

fn spawn_ticker(envelope: Arc<SpendEnvelope>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let now = now_ms();
            envelope.reconcile_at(now);
            envelope.checkpoint_if_due(now).await;
        }
    });
}

/// Route the envelope's attention items through the V3 feed projection.
/// Call once the feed store exists.
pub fn attach_feed_notifier(
    adapter: Arc<crate::magician_v2::feed::v3_projection::V3FeedProjectionAdapter>,
) {
    if let Some(envelope) = envelope() {
        envelope.set_notifier(Arc::new(FeedEnvelopeNotifier::new(adapter)));
    }
}

/// Final checkpoint at shutdown.
pub async fn flush_on_shutdown() {
    if let Some(envelope) = envelope() {
        envelope.flush().await;
    }
}

// ---------------------------------------------------------------------------
// Explicit producers (calls that never reach the router)
// ---------------------------------------------------------------------------

/// Count `cost_usd` of spend from a producer that bypasses the router
/// (harness turns, the Decision Engine's model calls, the coding cost
/// ledger). A no-op when no envelope is installed. Never double-charge a call
/// that also flows through the router.
pub fn charge_external(class: LlmWorkloadClass, principal: &str, workspace: &str, cost_usd: f64) {
    let Some(envelope) = envelope() else {
        return;
    };
    if let Some(micro) = usd_to_micro(cost_usd).filter(|m| *m > 0) {
        envelope.charge_at(class, principal, workspace, micro, false, now_ms());
    }
}

/// [`charge_external`] for a cost already in micro-USD.
pub fn charge_external_micro(
    class: LlmWorkloadClass,
    principal: &str,
    workspace: &str,
    micro_usd: u64,
) {
    let Some(envelope) = envelope() else {
        return;
    };
    if micro_usd > 0 {
        envelope.charge_at(class, principal, workspace, micro_usd, false, now_ms());
    }
}

/// Refuse to start an autonomous harness turn while the envelope is spent.
/// The error is the same typed refusal the router raises, so the loop parks
/// the run instead of failing it.
pub fn preflight_autonomous_turn() -> anyhow::Result<()> {
    let Some(envelope) = envelope() else {
        return Ok(());
    };
    envelope
        .check_at(LlmWorkloadClass::AutonomousTask, now_ms())
        .map_err(|resets_at_ms| {
            anyhow::Error::from(LLMError::SpendEnvelopeExhausted { resets_at_ms })
        })
}

// ---------------------------------------------------------------------------
// Parking
// ---------------------------------------------------------------------------

/// How long a parked run waits before it decides again, given when the window
/// resets: until then, but never less than [`MIN_PARK_WAIT`] nor more than
/// [`MAX_PARK_WAIT`] (so a grant wakes it within minutes).
pub fn park_wait(resets_at_ms: Option<i64>, now_ms: i64) -> Duration {
    resets_at_ms
        .map(|at| at.saturating_sub(now_ms))
        .filter(|remaining| *remaining > 0)
        .map(|remaining| Duration::from_millis(remaining as u64))
        .unwrap_or(MIN_PARK_WAIT)
        .clamp(MIN_PARK_WAIT, MAX_PARK_WAIT)
}

/// When `error` is (or wraps) the envelope's refusal, how long the run should
/// park before deciding again. Recognised by type anywhere in the error chain,
/// else by the stable marker in its text (an error stringified on the way up).
pub fn park_wait_for_error(error: &anyhow::Error) -> Option<Duration> {
    let typed = error.chain().find_map(|cause| {
        cause
            .downcast_ref::<LLMError>()
            .and_then(LLMError::spend_envelope_resets_at_ms)
    });
    let marked = typed.is_some()
        || error
            .chain()
            .any(|cause| cause.to_string().contains(SPEND_ENVELOPE_EXHAUSTED_MARKER));
    if !marked {
        return None;
    }
    let now = now_ms();
    let resets_at = typed.or_else(|| envelope().map(|envelope| envelope.window_end_ms()));
    Some(park_wait(resets_at, now))
}

/// Whether `error` is the envelope's refusal.
pub fn is_exhaustion_error(error: &anyhow::Error) -> bool {
    park_wait_for_error(error).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    const DAY: i64 = 86_400_000;
    /// 2026-10-13T00:00:00Z.
    const MIDNIGHT: i64 = 1_791_849_600_000;

    fn envelope_with(daily_usd: f64) -> SpendEnvelope {
        SpendEnvelope::with_settings(&SpendEnvelopeSettings {
            daily_usd: BudgetCapUsd::Usd(daily_usd),
            ..SpendEnvelopeSettings::default()
        })
    }

    fn charge(envelope: &SpendEnvelope, class: LlmWorkloadClass, usd: f64, at: i64) {
        envelope.charge_at(
            class,
            "owner",
            "default",
            usd_to_micro(usd).unwrap(),
            false,
            at,
        );
    }

    #[test]
    fn windows_start_at_utc_boundaries_and_follow_the_offset() {
        let noon = MIDNIGHT + 12 * 3_600_000;
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 0, noon),
            MIDNIGHT
        );
        assert_eq!(
            next_window_start_ms(SpendEnvelopeWindow::Daily, 0, MIDNIGHT),
            MIDNIGHT + DAY
        );
        // The boundary instant belongs to the NEW window; the instant before
        // it to the old one.
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 0, MIDNIGHT + DAY),
            MIDNIGHT + DAY
        );
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 0, MIDNIGHT + DAY - 1),
            MIDNIGHT
        );
        // Offset 420: the day runs 07:00 UTC to 07:00 UTC.
        let seven = MIDNIGHT + 7 * 3_600_000;
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 420, noon),
            seven
        );
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 420, seven - 1),
            seven - DAY
        );
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Daily, 420, seven),
            seven
        );
        // 2026-10-13 is a Tuesday; hourly, weekly and monthly work too.
        assert_eq!(
            window_start_ms(SpendEnvelopeWindow::Hourly, 0, noon + 1_234),
            noon
        );
        let monday = window_start_ms(SpendEnvelopeWindow::Weekly, 0, noon);
        assert_eq!(monday, MIDNIGHT - DAY);
        assert_eq!(
            next_window_start_ms(SpendEnvelopeWindow::Weekly, 0, monday),
            monday + 7 * DAY
        );
        // Monthly: October 1st to November 1st.
        let october = window_start_ms(SpendEnvelopeWindow::Monthly, 0, noon);
        assert_eq!(october, MIDNIGHT - 12 * DAY);
        assert_eq!(
            next_window_start_ms(SpendEnvelopeWindow::Monthly, 0, october),
            october + 31 * DAY
        );
    }

    #[test]
    fn the_window_rolls_at_its_boundary_and_forgets_the_old_spend() {
        let envelope = envelope_with(100.0);
        charge(
            &envelope,
            LlmWorkloadClass::AutonomousTask,
            60.0,
            MIDNIGHT + 1_000,
        );
        assert_eq!(
            envelope.status_at(MIDNIGHT + DAY - 1).spent_usd,
            60.0,
            "the last millisecond is still the same window"
        );
        let next = envelope.status_at(MIDNIGHT + DAY);
        assert_eq!(next.spent_usd, 0.0);
        assert_eq!(next.window_start_ms, MIDNIGHT + DAY);
        assert_eq!(next.resets_at_ms, MIDNIGHT + 2 * DAY);
        // Lifetime spend survives the roll.
        assert_eq!(next.total_spent_usd, 60.0);
        assert!(next.by_class.is_empty());
        assert!(next.by_scope.is_empty());
    }

    #[test]
    fn a_checkpoint_resumes_this_windows_spend_and_drops_a_stale_one() {
        let envelope = envelope_with(100.0);
        charge(
            &envelope,
            LlmWorkloadClass::AutonomousTask,
            12.5,
            MIDNIGHT + 10,
        );
        charge(
            &envelope,
            LlmWorkloadClass::ForegroundChat,
            2.5,
            MIDNIGHT + 20,
        );
        let persisted = envelope.snapshot_persisted(MIDNIGHT + 30);
        let round_trip: PersistedEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&persisted).unwrap()).unwrap();
        assert_eq!(round_trip, persisted);

        let same_window = envelope_with(100.0);
        same_window.restore(&round_trip, MIDNIGHT + 3_600_000);
        let status = same_window.status_at(MIDNIGHT + 3_600_000);
        assert_eq!(status.spent_usd, 15.0);
        assert_eq!(status.total_spent_usd, 15.0);
        assert_eq!(status.by_class.get("autonomous_task"), Some(&12.5));
        assert_eq!(status.by_class.get("foreground_chat"), Some(&2.5));
        assert_eq!(status.by_scope[0].scope, "owner/default");

        let next_day = envelope_with(100.0);
        next_day.restore(&round_trip, MIDNIGHT + DAY + 5);
        let status = next_day.status_at(MIDNIGHT + DAY + 5);
        assert_eq!(status.spent_usd, 0.0, "yesterday's window is not today's");
        assert_eq!(status.total_spent_usd, 15.0, "lifetime spend is kept");
    }

    #[test]
    fn concurrent_charges_are_all_counted() {
        let envelope = Arc::new(envelope_with(1_000_000.0));
        let at = MIDNIGHT + 5;
        envelope.roll_if_due(at);
        std::thread::scope(|scope| {
            for thread in 0..8 {
                let envelope = Arc::clone(&envelope);
                scope.spawn(move || {
                    for _ in 0..1_000 {
                        envelope.charge_at(
                            if thread % 2 == 0 {
                                LlmWorkloadClass::AutonomousTask
                            } else {
                                LlmWorkloadClass::ForegroundChat
                            },
                            "owner",
                            "default",
                            1_250,
                            false,
                            at,
                        );
                    }
                });
            }
        });
        let status = envelope.status_at(at);
        assert_eq!(status.spent_usd, micro_to_usd(8 * 1_000 * 1_250));
        assert_eq!(
            status.by_class["autonomous_task"],
            micro_to_usd(4 * 1_000 * 1_250)
        );
        assert_eq!(
            status.by_class["foreground_chat"],
            micro_to_usd(4 * 1_000 * 1_250)
        );
        assert_eq!(status.by_scope.len(), 1);
    }

    #[test]
    fn autonomous_work_is_refused_when_spent_but_interactive_work_passes_and_is_counted() {
        let envelope = envelope_with(10.0);
        let at = MIDNIGHT + 1_000;
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 10.0, at);
        for class in CLASSES {
            let outcome = envelope.check_at(class, at);
            if is_gated(class) {
                assert_eq!(
                    outcome,
                    Err(MIDNIGHT + DAY),
                    "{} must be refused until the window resets",
                    class.as_str()
                );
            } else {
                assert_eq!(outcome, Ok(()), "{} must always pass", class.as_str());
            }
        }
        // The exempt classes are still counted.
        charge(&envelope, LlmWorkloadClass::ForegroundChat, 1.0, at);
        charge(&envelope, LlmWorkloadClass::System, 0.5, at);
        charge(&envelope, LlmWorkloadClass::Evaluation, 0.25, at);
        charge(&envelope, LlmWorkloadClass::InteractiveTask, 0.25, at);
        let status = envelope.status_at(at);
        assert_eq!(status.spent_usd, 12.0);
        assert!(status.exhausted);
        assert_eq!(status.exhausted_by, Some("window"));
        assert_eq!(status.gated_classes.len(), 5);
        assert_eq!(status.exempt_classes.len(), 4);
        // The router-facing gate speaks the typed error.
        let refusal = envelope
            .admit(
                LlmWorkloadClass::Memory,
                &LLMProviderKind::OpenAI,
                "gpt-5.5",
                at,
            )
            .expect_err("memory work is gated");
        assert_eq!(refusal.spend_envelope_resets_at_ms(), Some(MIDNIGHT + DAY));
        assert!(envelope
            .admit(
                LlmWorkloadClass::ForegroundChat,
                &LLMProviderKind::OpenAI,
                "gpt-5.5",
                at
            )
            .is_ok());
    }

    #[test]
    fn a_grant_lifts_the_block_and_the_reset_resumes_work() {
        let envelope = envelope_with(10.0);
        let at = MIDNIGHT + 1_000;
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 10.0, at);
        assert!(envelope
            .check_at(LlmWorkloadClass::AutonomousTask, at)
            .is_err());

        envelope.record_grant(5.0, at, at);
        assert_eq!(
            envelope.check_at(LlmWorkloadClass::AutonomousTask, at),
            Ok(()),
            "a $5 grant on top of the spent $10 reopens the window"
        );
        let status = envelope.status_at(at);
        assert_eq!(status.granted_usd, 5.0);
        assert_eq!(status.effective_limit_usd, Some(15.0));
        assert_eq!(status.remaining_usd, Some(5.0));
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 5.0, at);
        assert!(envelope
            .check_at(LlmWorkloadClass::AutonomousTask, at)
            .is_err());

        // The grant belongs to the window it was made in.
        assert_eq!(
            envelope.check_at(LlmWorkloadClass::AutonomousTask, MIDNIGHT + DAY),
            Ok(()),
            "the reset resumes autonomous work"
        );
        let next = envelope.status_at(MIDNIGHT + DAY);
        assert_eq!(next.granted_usd, 0.0);
        assert_eq!(next.effective_limit_usd, Some(10.0));
    }

    #[test]
    fn grants_are_reloaded_for_their_own_window_only() {
        let envelope = envelope_with(10.0);
        let grants = vec![
            EnvelopeGrant {
                granted_at: DateTime::<Utc>::from_timestamp_millis(MIDNIGHT - 5_000).unwrap(),
                add_cost_usd: 7.0,
                source: Some(OWNER_GRANT_SOURCE.to_string()),
                note: None,
            },
            EnvelopeGrant {
                granted_at: DateTime::<Utc>::from_timestamp_millis(MIDNIGHT + 5_000).unwrap(),
                add_cost_usd: 3.0,
                source: None,
                note: Some("weekend".to_string()),
            },
        ];
        envelope.load_grants(&grants, MIDNIGHT + 10_000);
        let status = envelope.status_at(MIDNIGHT + 10_000);
        assert_eq!(status.granted_usd, 3.0);
        assert_eq!(status.total_granted_usd, 10.0);
        assert_eq!(status.effective_limit_usd, Some(13.0));
    }

    #[test]
    fn the_grants_file_is_append_only_positive_and_never_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system").join(GRANTS_FILE);
        assert!(load_grants(&path).is_empty());
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(validate_grant_usd(bad).is_err(), "{bad}");
            let grant = EnvelopeGrant {
                granted_at: Utc::now(),
                add_cost_usd: bad,
                source: None,
                note: None,
            };
            assert!(append_grant(&path, grant).is_err(), "{bad}");
        }
        let first = EnvelopeGrant {
            granted_at: Utc::now(),
            add_cost_usd: 25.0,
            source: Some(OWNER_GRANT_SOURCE.to_string()),
            note: None,
        };
        let second = EnvelopeGrant {
            add_cost_usd: 10.5,
            note: Some("again".to_string()),
            ..first.clone()
        };
        assert_eq!(append_grant(&path, first.clone()).unwrap().len(), 1);
        assert_eq!(append_grant(&path, second.clone()).unwrap().len(), 2);
        assert_eq!(load_grants(&path), vec![first, second]);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("unlimited"), "{raw}");
    }

    #[test]
    fn the_checkpoint_file_round_trips_through_disk_without_a_stray_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system").join(STATE_FILE);
        let envelope = envelope_with(50.0);
        charge(&envelope, LlmWorkloadClass::Scheduled, 3.0, MIDNIGHT + 1);
        let state = envelope.snapshot_persisted(MIDNIGHT + 2);
        write_state_file(&path, &state).unwrap();
        assert_eq!(read_state_file(&path), Some(state));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(STATE_FILE)]);
    }

    #[test]
    fn the_checkpoint_is_debounced() {
        let envelope = envelope_with(50.0);
        let at = MIDNIGHT + 10_000;
        charge(&envelope, LlmWorkloadClass::Memory, 1.0, at);
        assert!(
            envelope.take_checkpoint_due(at).is_some(),
            "first change flushes"
        );
        charge(&envelope, LlmWorkloadClass::Memory, 1.0, at + 1_000);
        assert!(
            envelope.take_checkpoint_due(at + 1_000).is_none(),
            "a second change inside five seconds waits"
        );
        assert!(envelope.take_checkpoint_due(at + 5_000).is_some());
        assert!(
            envelope.take_checkpoint_due(at + 20_000).is_none(),
            "nothing changed, nothing to write"
        );
    }

    #[derive(Default)]
    struct Recorder {
        events: StdMutex<Vec<&'static str>>,
    }

    impl Recorder {
        fn take(&self) -> Vec<&'static str> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    impl EnvelopeNotifier for Recorder {
        fn exhausted(&self, _: &ScopeRef, _: &EnvelopeStatus) {
            self.events.lock().unwrap().push("exhausted");
        }
        fn exhausted_cleared(&self, _: &ScopeRef) {
            self.events.lock().unwrap().push("exhausted_cleared");
        }
        fn warning(&self, _: &ScopeRef, _: &EnvelopeStatus) {
            self.events.lock().unwrap().push("warning");
        }
        fn warning_cleared(&self, _: &ScopeRef) {
            self.events.lock().unwrap().push("warning_cleared");
        }
    }

    #[test]
    fn the_attention_item_is_raised_once_and_removed_at_reset_or_on_a_grant() {
        let envelope = envelope_with(100.0);
        let recorder = Arc::new(Recorder::default());
        envelope.set_notifier(recorder.clone());
        let at = MIDNIGHT + 1_000;

        // First call also sweeps whatever a previous process left behind.
        envelope.reconcile_at(at);
        assert_eq!(
            recorder.take(),
            vec!["exhausted_cleared", "warning_cleared"]
        );
        envelope.reconcile_at(at);
        assert!(recorder.take().is_empty(), "no change, no event");

        // 80% crosses the warning.
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 80.0, at);
        envelope.reconcile_at(at);
        assert_eq!(recorder.take(), vec!["warning"]);
        envelope.reconcile_at(at);
        assert!(recorder.take().is_empty());

        // Spent: the warning gives way to ONE exhausted item, however many
        // refused calls follow.
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 20.0, at);
        for _ in 0..5 {
            let _ = envelope.check_at(LlmWorkloadClass::AutonomousTask, at);
            envelope.reconcile_at(at);
        }
        assert_eq!(recorder.take(), vec!["warning_cleared", "exhausted"]);

        // A grant removes it: $100 of $150 is below the warning line.
        envelope.record_grant(50.0, at, at);
        envelope.reconcile_at(at);
        assert_eq!(recorder.take(), vec!["exhausted_cleared"]);

        // Spend into the grant: 120 of 150 is 80% again, then all of it.
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 20.0, at);
        envelope.reconcile_at(at);
        assert_eq!(recorder.take(), vec!["warning"]);
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 30.0, at);
        envelope.reconcile_at(at);
        assert_eq!(recorder.take(), vec!["warning_cleared", "exhausted"]);

        // The window resets: the item is removed.
        envelope.reconcile_at(MIDNIGHT + DAY);
        assert_eq!(recorder.take(), vec!["exhausted_cleared"]);
    }

    #[test]
    fn the_feed_items_carry_the_stable_ids() {
        let envelope = envelope_with(10.0);
        charge(
            &envelope,
            LlmWorkloadClass::AutonomousTask,
            10.0,
            MIDNIGHT + 1,
        );
        let status = envelope.status_at(MIDNIGHT + 1);
        let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
        let exhausted = exhausted_feed_item(&scope, &status, MIDNIGHT);
        assert_eq!(exhausted.id, "spend_envelope:exhausted");
        assert_eq!(exhausted.status, FeedItemStatus::NeedsAction);
        assert!(exhausted.summary.unwrap().contains("parked"));
        let warning = warning_feed_item(&scope, &status, MIDNIGHT);
        assert_eq!(warning.id, "spend_envelope:warning");
        assert_eq!(warning.status, FeedItemStatus::Info);
    }

    fn settlement<'a>(
        provider: &'a LLMProviderKind,
        model: &'a str,
        workload: LlmWorkloadClass,
        priced_micro_usd: Option<u64>,
        usage: Option<&'a magicllm::TokenUsage>,
    ) -> SpendSettlement<'a> {
        SpendSettlement {
            workload,
            principal: "owner",
            workspace: "default",
            operation: "agentic_decision",
            provider,
            model,
            at_ms: MIDNIGHT,
            priced_micro_usd,
            usage,
            output_ceiling_tokens: 1_000,
        }
    }

    #[test]
    fn a_priced_call_is_charged_exactly_and_an_unpriced_one_at_the_highest_provider_rate() {
        let envelope = envelope_with(1_000.0);
        let openai = LLMProviderKind::OpenAI;
        envelope.settle_at(
            &settlement(
                &openai,
                "gpt-5.5",
                LlmWorkloadClass::AutonomousTask,
                Some(1_234_567),
                None,
            ),
            MIDNIGHT,
        );
        let status = envelope.status_at(MIDNIGHT);
        assert_eq!(status.spent_usd, micro_to_usd(1_234_567));
        assert_eq!(status.estimated_calls, 0);

        // An id the pricing table has never heard of: reported tokens at the
        // dearest OpenAI rate the table knows.
        let usage = magicllm::TokenUsage {
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(100_000),
            ..Default::default()
        };
        let unlisted = "gpt-9-not-in-any-table";
        assert!(!magicllm::model_is_priced_at(&openai, unlisted, MIDNIGHT));
        envelope.settle_at(
            &settlement(
                &openai,
                unlisted,
                LlmWorkloadClass::AutonomousTask,
                None,
                Some(&usage),
            ),
            MIDNIGHT,
        );
        let after = envelope.status_at(MIDNIGHT);
        let charged = after.spent_usd - status.spent_usd;
        assert!(charged > 0.0, "an unpriced call is never free");
        assert_eq!(after.estimated_calls, 1);
        assert!((after.estimated_usd - charged).abs() < 1e-6);
        let expected =
            magicllm::estimate_unpriced_cost_microusd(&openai, Some(&usage), 1_000, MIDNIGHT);
        assert_eq!(envelope.estimated_micro.load(Ordering::Relaxed), expected);

        // No usage at all: an output-ceiling estimate, still not free.
        envelope.settle_at(
            &settlement(&openai, unlisted, LlmWorkloadClass::Memory, None, None),
            MIDNIGHT,
        );
        assert_eq!(envelope.status_at(MIDNIGHT).estimated_calls, 2);
        assert!(envelope.status_at(MIDNIGHT).spent_usd > after.spent_usd);
    }

    #[test]
    fn under_block_an_unpriced_model_is_refused_for_gated_work_only() {
        let envelope = SpendEnvelope::with_settings(&SpendEnvelopeSettings {
            unpriced: UnpricedPolicy::Block,
            ..SpendEnvelopeSettings::default()
        });
        let openai = LLMProviderKind::OpenAI;
        let unlisted = "gpt-9-not-in-any-table";
        let at = MIDNIGHT;
        let refusal = envelope
            .admit(LlmWorkloadClass::AutonomousTask, &openai, unlisted, at)
            .expect_err("unpriced + block + gated is refused");
        assert!(matches!(refusal, LLMError::Validation(_)), "{refusal:?}");
        assert!(envelope
            .admit(LlmWorkloadClass::AutonomousTask, &openai, "gpt-5.5", at)
            .is_ok());
        assert!(
            envelope
                .admit(LlmWorkloadClass::ForegroundChat, &openai, unlisted, at)
                .is_ok(),
            "the owner's own chat is never blocked"
        );
        // `estimate` (the default) lets the same model through.
        let estimating = envelope_with(10.0);
        assert!(estimating
            .admit(LlmWorkloadClass::AutonomousTask, &openai, unlisted, at)
            .is_ok());
    }

    #[test]
    fn the_lifetime_ceiling_parks_work_until_it_is_raised_or_granted() {
        let envelope = SpendEnvelope::with_settings(&SpendEnvelopeSettings {
            daily_usd: BudgetCapUsd::Unlimited,
            total_usd: BudgetCapUsd::Usd(20.0),
            ..SpendEnvelopeSettings::default()
        });
        let at = MIDNIGHT + 1_000;
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 20.0, at);
        let blocked = envelope.check_at(LlmWorkloadClass::AutonomousTask, at);
        assert_eq!(blocked, Err(at + 3_600_000));
        assert_eq!(envelope.status_at(at).exhausted_by, Some("total"));
        // The window rolling does not lift a lifetime ceiling.
        assert!(envelope
            .check_at(LlmWorkloadClass::AutonomousTask, MIDNIGHT + DAY)
            .is_err());
        envelope.record_grant(10.0, MIDNIGHT + DAY, MIDNIGHT + DAY);
        assert!(envelope
            .check_at(LlmWorkloadClass::AutonomousTask, MIDNIGHT + DAY)
            .is_ok());
    }

    #[test]
    fn a_disabled_envelope_gates_nothing_but_keeps_counting() {
        let envelope = SpendEnvelope::with_settings(&SpendEnvelopeSettings {
            enabled: false,
            daily_usd: BudgetCapUsd::Usd(1.0),
            ..SpendEnvelopeSettings::default()
        });
        let at = MIDNIGHT + 1;
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 50.0, at);
        assert_eq!(
            envelope.check_at(LlmWorkloadClass::AutonomousTask, at),
            Ok(())
        );
        assert_eq!(envelope.status_at(at).spent_usd, 50.0);
        assert!(!envelope.status_at(at).exhausted);
        // Turning it on enforces what was already spent.
        envelope.apply_settings(
            &SpendEnvelopeSettings {
                daily_usd: BudgetCapUsd::Usd(1.0),
                ..SpendEnvelopeSettings::default()
            },
            at,
        );
        assert!(envelope
            .check_at(LlmWorkloadClass::AutonomousTask, at)
            .is_err());
    }

    #[test]
    fn changing_the_offset_rederives_the_window_without_forgetting_spend() {
        let envelope = envelope_with(100.0);
        let at = MIDNIGHT + 10 * 3_600_000;
        charge(&envelope, LlmWorkloadClass::AutonomousTask, 40.0, at);
        envelope.apply_settings(
            &SpendEnvelopeSettings {
                daily_usd: BudgetCapUsd::Usd(100.0),
                reset_offset_minutes: 720,
                ..SpendEnvelopeSettings::default()
            },
            at,
        );
        let status = envelope.status_at(at);
        assert_eq!(status.spent_usd, 40.0);
        assert_eq!(status.window_start_ms, MIDNIGHT + 12 * 3_600_000 - DAY);
        assert_eq!(status.resets_at_ms, MIDNIGHT + 12 * 3_600_000);
    }

    #[test]
    fn an_exhausted_child_or_parent_parks_instead_of_failing() {
        // The typed refusal anywhere in an error chain, wrapped or routed.
        let refusal = LLMError::SpendEnvelopeExhausted {
            resets_at_ms: now_ms() + 30 * 60_000,
        }
        .with_route("agentic-decision", LLMProviderKind::OpenAI, "gpt-5.5");
        let error = anyhow::Error::from(refusal).context("decision call failed");
        let wait = park_wait_for_error(&error).expect("recognised");
        assert_eq!(
            wait, MAX_PARK_WAIT,
            "a distant reset is polled every few minutes"
        );
        assert!(is_exhaustion_error(&error));

        // Stringified on the way up: the marker still identifies it.
        let stringified =
            anyhow::anyhow!("{}", LLMError::SpendEnvelopeExhausted { resets_at_ms: 0 });
        assert!(park_wait_for_error(&stringified).is_some());

        // Anything else is not a park.
        assert!(park_wait_for_error(&anyhow::anyhow!("429 rate limit")).is_none());
        assert!(park_wait_for_error(&anyhow::Error::from(LLMError::Timeout)).is_none());

        // The wait never busy-polls and never outlives a poll interval.
        assert_eq!(park_wait(Some(1_000), 900), MIN_PARK_WAIT);
        assert_eq!(park_wait(Some(0), 900), MIN_PARK_WAIT);
        assert_eq!(park_wait(None, 900), MIN_PARK_WAIT);
        assert_eq!(
            park_wait(Some(900 + 120_000), 900),
            Duration::from_secs(120)
        );
        assert_eq!(park_wait(Some(900 + DAY), 900), MAX_PARK_WAIT);
    }

    #[test]
    fn unit_tests_run_without_an_installed_envelope() {
        // Nothing installed: nothing counted, nothing refused, the token
        // meter stays in force. (`install_process_envelope` is production
        // bootstrap only.)
        assert!(envelope().is_none());
        assert!(!supersedes_token_meter());
        assert!(preflight_autonomous_turn().is_ok());
        charge_external(LlmWorkloadClass::AutonomousTask, "owner", "default", 5.0);
    }
}
