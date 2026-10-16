//! One scoped policy and one boot-local ledger for bounded Observe catch-up.
//!
//! Source adapters retain their native checkpoint formats. This module owns
//! the user-facing invariant above them: automatic work admitted because the
//! service restarted has one history window, per-source cap, total cap, and
//! wall-clock deadline. Manual `Check now` operations are never disguised as
//! startup work and therefore do not consume this ledger.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    artifacts::durable_store::{open_local_durable_artifacts, DurableFrontmatter},
};

pub const CATCH_UP_NAMESPACE: &str = "observe_catch_up";
pub const CATCH_UP_CONFIG_NAME: &str = "config.json";
pub const CATCH_UP_SCHEMA_VERSION: u32 = 1;
pub const CATCH_UP_LOOKBACK_OPTIONS: [u32; 5] = [1, 5, 7, 14, 30];
pub const CATCH_UP_SOURCE_CAP_OPTIONS: [usize; 5] = [10, 25, 50, 100, 200];
pub const CATCH_UP_TOTAL_CAP_OPTIONS: [usize; 5] = [50, 100, 200, 500, 1_000];
pub const CATCH_UP_DURATION_OPTIONS: [u32; 4] = [5, 15, 30, 60];

fn default_true() -> bool {
    true
}

fn default_lookback_days() -> u32 {
    7
}

fn default_source_cap() -> usize {
    50
}

fn default_total_cap() -> usize {
    200
}

fn default_duration_minutes() -> u32 {
    15
}

/// Durable, scope-local user policy. `revision` is an optimistic-concurrency
/// token for the UI; stale tabs cannot overwrite a newer choice silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveCatchUpPolicy {
    #[serde(default = "catch_up_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_lookback_days")]
    pub lookback_days: u32,
    #[serde(default = "default_source_cap")]
    pub max_items_per_source: usize,
    #[serde(default = "default_total_cap")]
    pub max_total_items: usize,
    #[serde(default = "default_duration_minutes")]
    pub max_duration_minutes: u32,
}

impl Default for ObserveCatchUpPolicy {
    fn default() -> Self {
        Self {
            schema_version: CATCH_UP_SCHEMA_VERSION,
            revision: 0,
            enabled: true,
            lookback_days: default_lookback_days(),
            max_items_per_source: default_source_cap(),
            max_total_items: default_total_cap(),
            max_duration_minutes: default_duration_minutes(),
        }
    }
}

impl ObserveCatchUpPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CATCH_UP_SCHEMA_VERSION {
            bail!("unsupported Observe catch-up policy schema");
        }
        if !CATCH_UP_LOOKBACK_OPTIONS.contains(&self.lookback_days) {
            bail!("history window must be one of 1, 5, 7, 14, or 30 days");
        }
        if !CATCH_UP_SOURCE_CAP_OPTIONS.contains(&self.max_items_per_source) {
            bail!("per-source cap must be one of 10, 25, 50, 100, or 200");
        }
        if !CATCH_UP_TOTAL_CAP_OPTIONS.contains(&self.max_total_items) {
            bail!("total cap must be one of 50, 100, 200, 500, or 1000");
        }
        if self.max_total_items < self.max_items_per_source {
            bail!("total cap cannot be smaller than the per-source cap");
        }
        if !CATCH_UP_DURATION_OPTIONS.contains(&self.max_duration_minutes) {
            bail!("duration must be one of 5, 15, 30, or 60 minutes");
        }
        Ok(())
    }
}

const fn catch_up_schema_version() -> u32 {
    CATCH_UP_SCHEMA_VERSION
}

pub async fn load_observe_catch_up_policy(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> ObserveCatchUpPolicy {
    let Ok(store) = open_local_durable_artifacts(workspace_layout, principal, workspace) else {
        return ObserveCatchUpPolicy::default();
    };
    if let Ok((_, body)) = store.read(CATCH_UP_NAMESPACE, CATCH_UP_CONFIG_NAME).await {
        if let Some(policy) = serde_json::from_str::<ObserveCatchUpPolicy>(&body)
            .ok()
            .filter(|policy| policy.validate().is_ok())
        {
            return policy;
        }
    }

    // Preserve the history choice users already made on the old Mail card.
    // The new policy becomes canonical after this one-time seed.
    let mut policy = ObserveCatchUpPolicy::default();
    if let Some(channel_config) = crate::magician_v2::observe_connectors::read_channel_observe(
        workspace_layout,
        principal,
        workspace,
    )
    .await
    {
        let existing = channel_config.history_lookback_days;
        if CATCH_UP_LOOKBACK_OPTIONS.contains(&existing) {
            policy.lookback_days = existing;
        }
    }
    if let Err(error) =
        write_observe_catch_up_policy(workspace_layout, principal, workspace, &policy).await
    {
        tracing::warn!(principal, workspace, %error, "could not seed Observe catch-up policy");
    }
    policy
}

async fn write_observe_catch_up_policy(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    policy: &ObserveCatchUpPolicy,
) -> Result<()> {
    let store = open_local_durable_artifacts(workspace_layout, principal, workspace)?;
    let body = serde_json::to_string_pretty(policy)?;
    store
        .write(
            CATCH_UP_NAMESPACE,
            CATCH_UP_CONFIG_NAME,
            &body,
            DurableFrontmatter {
                namespace: CATCH_UP_NAMESPACE.to_string(),
                name: CATCH_UP_CONFIG_NAME.to_string(),
                created_by: "observe_catch_up".to_string(),
                last_updated_by: "observe_catch_up".to_string(),
                last_updated: Utc::now(),
                content_type: Some("application/json".to_string()),
                source_execution_id: None,
                source_task_id: None,
                source_workflow_instance_id: None,
                source_run_id: None,
                source_cycle_id: None,
                source_agent_id: Some(principal.to_string()),
                producer_stage: Some("observe_catch_up_policy".to_string()),
            },
        )
        .await
        .context("persisting Observe catch-up policy")?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpPhase {
    Waiting,
    Active,
    Completed,
    Disabled,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpReplayMode {
    /// The provider has a durable cursor/history API and can replay the
    /// configured interval (subject to its own retention).
    CheckpointedReplay,
    /// RSS and similar endpoints expose their current snapshot only. A six-day
    /// outage cannot be reconstructed if the feed no longer contains it.
    CurrentSnapshotOnly,
    /// This producer already owns a bounded scheduled window rather than a
    /// provider cursor (currently calendar evidence).
    ScheduledWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatchUpSourceStatus {
    pub source_id: String,
    pub display_name: String,
    pub item_unit: String,
    pub replay_mode: CatchUpReplayMode,
    pub admitted: usize,
    pub processed: usize,
    pub runs: u64,
    pub failures: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub limitation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObserveCatchUpStatus {
    pub policy: ObserveCatchUpPolicy,
    pub phase: CatchUpPhase,
    pub boot_id: String,
    pub boot_started_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catch_up_started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    pub admitted_items: usize,
    pub processed_items: usize,
    pub reserved_items: usize,
    pub remaining_items: usize,
    pub sources: Vec<CatchUpSourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpAdmission {
    token: String,
    generation: u64,
    pub source_id: String,
    pub max_items: usize,
    pub historical_floor_ms: i64,
    pub boot_started_at_ms: i64,
    pub deadline_at_ms: i64,
    /// No successful items have yet been accepted for this source in this
    /// boot. Cursor-based sources use this to discard a stale pre-boot page
    /// offset exactly once while keeping retry attempts safe.
    pub initial_batch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatchUpDecision {
    Admit(CatchUpAdmission),
    /// Automatic polling should advance/check its native cursor while only
    /// retaining items at or after `retain_from_ms`. This prevents a disabled
    /// or exhausted catch-up from reappearing as an unbounded later tick.
    SkipHistorical {
        retain_from_ms: i64,
        reason: &'static str,
    },
    /// A post-boot baseline has been persisted, so the source can use its
    /// native cursor for ordinary future polling while retaining the boot
    /// floor as defence in depth.
    Normal,
}

#[derive(Debug, Clone)]
struct Reservation {
    source_id: String,
    items: usize,
}

#[derive(Debug, Clone)]
struct SourceLedger {
    display_name: String,
    item_unit: String,
    replay_mode: CatchUpReplayMode,
    limitation: String,
    admitted: usize,
    processed: usize,
    runs: u64,
    failures: u64,
    last_error: Option<String>,
    state: SourceCatchUpState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceCatchUpState {
    Pending,
    /// Historical replay completed and one post-boot baseline check must run
    /// before the source switches to its ordinary polling path.
    BaselineRequired,
    /// The source established its post-boot baseline. Runtimes still retain
    /// the boot floor defensively while using their ordinary polling path.
    Completed,
}

#[derive(Debug)]
struct RuntimeLedger {
    generation: u64,
    policy: ObserveCatchUpPolicy,
    started_at_ms: Option<i64>,
    admitted: usize,
    processed: usize,
    reservations: BTreeMap<String, Reservation>,
    sources: BTreeMap<String, SourceLedger>,
}

impl RuntimeLedger {
    fn new(policy: ObserveCatchUpPolicy) -> Self {
        Self {
            generation: 0,
            policy,
            started_at_ms: None,
            admitted: 0,
            processed: 0,
            reservations: BTreeMap::new(),
            sources: BTreeMap::new(),
        }
    }

    fn reset(&mut self, policy: ObserveCatchUpPolicy) {
        self.generation = self.generation.saturating_add(1);
        self.policy = policy;
        self.started_at_ms = None;
        self.admitted = 0;
        self.processed = 0;
        self.reservations.clear();
        self.sources.clear();
    }
}

/// Shared by all automatic Observe producers in one process. The durable
/// policy is scoped; the counters intentionally reset on boot because they
/// describe this boot's catch-up rather than historical lifetime totals.
#[derive(Debug)]
pub struct ObserveCatchUpController {
    workspace_layout: ArtifactV2Workspace,
    boot_id: String,
    boot_started_at_ms: i64,
    scopes: Mutex<BTreeMap<(String, String), RuntimeLedger>>,
    mutation_lock: tokio::sync::Mutex<()>,
}

impl ObserveCatchUpController {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            boot_id: Uuid::new_v4().to_string(),
            boot_started_at_ms: Utc::now().timestamp_millis(),
            scopes: Mutex::new(BTreeMap::new()),
            mutation_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn ensure_scope_policy(&self, principal: &str, workspace: &str) {
        let key = (principal.to_string(), workspace.to_string());
        if self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&key)
        {
            return;
        }
        let policy =
            load_observe_catch_up_policy(&self.workspace_layout, principal, workspace).await;
        self.scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key)
            .or_insert_with(|| RuntimeLedger::new(policy));
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn begin(
        &self,
        principal: &str,
        workspace: &str,
        source_id: &str,
        display_name: &str,
        item_unit: &str,
        replay_mode: CatchUpReplayMode,
        requested_items: usize,
        limitation: &str,
    ) -> CatchUpDecision {
        self.ensure_scope_policy(principal, workspace).await;
        let now = Utc::now().timestamp_millis();
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ledger = scopes
            .get_mut(&(principal.to_string(), workspace.to_string()))
            .expect("scope policy was installed");
        if let Some(source) = ledger.sources.get(source_id) {
            match source.state {
                SourceCatchUpState::Completed => return CatchUpDecision::Normal,
                SourceCatchUpState::BaselineRequired => {
                    return CatchUpDecision::SkipHistorical {
                        retain_from_ms: self.boot_started_at_ms,
                        reason: "post_boot_baseline_required",
                    };
                },
                SourceCatchUpState::Pending => {},
            }
        }
        if !ledger.policy.enabled {
            return CatchUpDecision::SkipHistorical {
                retain_from_ms: self.boot_started_at_ms,
                reason: "disabled",
            };
        }
        let started = *ledger.started_at_ms.get_or_insert(now);
        let deadline = started
            .saturating_add(i64::from(ledger.policy.max_duration_minutes).saturating_mul(60_000));
        if now >= deadline {
            return CatchUpDecision::SkipHistorical {
                retain_from_ms: self.boot_started_at_ms,
                reason: "duration_exhausted",
            };
        }
        let reserved_total = ledger
            .reservations
            .values()
            .map(|reservation| reservation.items)
            .sum::<usize>();
        ledger
            .sources
            .entry(source_id.to_string())
            .or_insert_with(|| SourceLedger {
                display_name: display_name.to_string(),
                item_unit: item_unit.to_string(),
                replay_mode,
                limitation: limitation.to_string(),
                admitted: 0,
                processed: 0,
                runs: 0,
                failures: 0,
                last_error: None,
                state: SourceCatchUpState::Pending,
            });
        let source_reserved = ledger
            .reservations
            .values()
            .filter(|reservation| reservation.source_id == source_id)
            .map(|reservation| reservation.items)
            .sum::<usize>();
        let source_admitted = ledger
            .sources
            .get(source_id)
            .map(|source| source.admitted)
            .unwrap_or_default();
        let initial_batch = source_admitted == 0 && source_reserved == 0;
        let source_remaining = ledger
            .policy
            .max_items_per_source
            .saturating_sub(source_admitted.saturating_add(source_reserved));
        let total_remaining = ledger
            .policy
            .max_total_items
            .saturating_sub(ledger.admitted.saturating_add(reserved_total));
        let admitted = requested_items.min(source_remaining).min(total_remaining);
        if admitted == 0 {
            return CatchUpDecision::SkipHistorical {
                retain_from_ms: self.boot_started_at_ms,
                reason: if source_remaining == 0 {
                    "source_budget_exhausted"
                } else {
                    "total_budget_exhausted"
                },
            };
        }
        let token = Uuid::new_v4().to_string();
        ledger.reservations.insert(
            token.clone(),
            Reservation {
                source_id: source_id.to_string(),
                items: admitted,
            },
        );
        CatchUpDecision::Admit(CatchUpAdmission {
            token,
            generation: ledger.generation,
            source_id: source_id.to_string(),
            max_items: admitted,
            historical_floor_ms: self
                .boot_started_at_ms
                .saturating_sub(i64::from(ledger.policy.lookback_days).saturating_mul(86_400_000)),
            boot_started_at_ms: self.boot_started_at_ms,
            deadline_at_ms: deadline,
            initial_batch,
        })
    }

    pub fn complete(
        &self,
        principal: &str,
        workspace: &str,
        admission: CatchUpAdmission,
        processed_items: usize,
        error: Option<&str>,
    ) {
        self.complete_with_exhaustion(
            principal,
            workspace,
            admission,
            processed_items,
            error,
            true,
        );
    }

    /// Finish one admitted batch. One-shot acquisition sources use
    /// [`Self::complete`]; queue-like local processors can leave the source
    /// pending until their eligible queue is empty or its boot cap is spent.
    pub fn complete_with_exhaustion(
        &self,
        principal: &str,
        workspace: &str,
        admission: CatchUpAdmission,
        processed_items: usize,
        error: Option<&str>,
        exhausted: bool,
    ) {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(ledger) = scopes.get_mut(&(principal.to_string(), workspace.to_string())) else {
            return;
        };
        if ledger.generation != admission.generation {
            return;
        }
        let Some(reservation) = ledger.reservations.remove(&admission.token) else {
            return;
        };
        let accepted = processed_items.min(reservation.items);
        ledger.admitted = ledger.admitted.saturating_add(accepted);
        ledger.processed = ledger.processed.saturating_add(accepted);
        if let Some(source) = ledger.sources.get_mut(&reservation.source_id) {
            source.admitted = source.admitted.saturating_add(accepted);
            source.processed = source.processed.saturating_add(accepted);
            source.runs = source.runs.saturating_add(1);
            if let Some(error) = error {
                source.failures = source.failures.saturating_add(1);
                source.last_error = Some(error.chars().take(240).collect());
            } else if exhausted {
                source.last_error = None;
                source.state = SourceCatchUpState::BaselineRequired;
            } else {
                source.last_error = None;
            }
        }
    }

    pub fn finish_skipped(
        &self,
        principal: &str,
        workspace: &str,
        source_id: &str,
        display_name: &str,
        item_unit: &str,
        replay_mode: CatchUpReplayMode,
        limitation: &str,
        error: Option<&str>,
        resume_normal_polling: bool,
    ) {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(ledger) = scopes.get_mut(&(principal.to_string(), workspace.to_string())) else {
            return;
        };
        let source = ledger
            .sources
            .entry(source_id.to_string())
            .or_insert_with(|| SourceLedger {
                display_name: display_name.to_string(),
                item_unit: item_unit.to_string(),
                replay_mode,
                limitation: limitation.to_string(),
                admitted: 0,
                processed: 0,
                runs: 0,
                failures: 0,
                last_error: None,
                state: SourceCatchUpState::Pending,
            });
        source.runs = source.runs.saturating_add(1);
        if let Some(error) = error {
            source.failures = source.failures.saturating_add(1);
            source.last_error = Some(error.chars().take(240).collect());
        } else {
            source.last_error = None;
            source.state = if resume_normal_polling {
                SourceCatchUpState::Completed
            } else {
                SourceCatchUpState::BaselineRequired
            };
        }
    }

    pub async fn status(&self, principal: &str, workspace: &str) -> ObserveCatchUpStatus {
        self.ensure_scope_policy(principal, workspace).await;
        let now = Utc::now().timestamp_millis();
        let scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ledger = scopes
            .get(&(principal.to_string(), workspace.to_string()))
            .expect("scope policy was installed");
        let reserved = ledger
            .reservations
            .values()
            .map(|reservation| reservation.items)
            .sum::<usize>();
        let deadline = ledger.started_at_ms.map(|started| {
            started.saturating_add(
                i64::from(ledger.policy.max_duration_minutes).saturating_mul(60_000),
            )
        });
        let all_started_sources_settled = !ledger.sources.is_empty()
            && reserved == 0
            && ledger
                .sources
                .values()
                .all(|source| source.state != SourceCatchUpState::Pending);
        let phase = if !ledger.policy.enabled {
            CatchUpPhase::Disabled
        } else if deadline.is_some_and(|deadline| now >= deadline) {
            CatchUpPhase::Expired
        } else if ledger.admitted.saturating_add(reserved) >= ledger.policy.max_total_items {
            CatchUpPhase::Completed
        } else if all_started_sources_settled {
            CatchUpPhase::Completed
        } else if ledger.started_at_ms.is_some() {
            CatchUpPhase::Active
        } else {
            CatchUpPhase::Waiting
        };
        let mut sources = baseline_source_statuses();
        for (source_id, source) in &ledger.sources {
            if let Some(baseline_id) = source_baseline_id(source_id, &source.item_unit) {
                sources.retain(|item| {
                    item.source_id != baseline_id || item.source_id == source_id.as_str()
                });
            }
            if let Some(existing) = sources.iter_mut().find(|item| item.source_id == *source_id) {
                existing.admitted = source.admitted;
                existing.processed = source.processed;
                existing.runs = source.runs;
                existing.failures = source.failures;
                existing.last_error = source.last_error.clone();
            } else {
                sources.push(CatchUpSourceStatus {
                    source_id: source_id.clone(),
                    display_name: source.display_name.clone(),
                    item_unit: source.item_unit.clone(),
                    replay_mode: source.replay_mode,
                    admitted: source.admitted,
                    processed: source.processed,
                    runs: source.runs,
                    failures: source.failures,
                    last_error: source.last_error.clone(),
                    limitation: source.limitation.clone(),
                });
            }
        }
        sources.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        ObserveCatchUpStatus {
            policy: ledger.policy.clone(),
            phase,
            boot_id: self.boot_id.clone(),
            boot_started_at_ms: self.boot_started_at_ms,
            catch_up_started_at_ms: ledger.started_at_ms,
            deadline_at_ms: deadline,
            admitted_items: ledger.admitted,
            processed_items: ledger.processed,
            reserved_items: reserved,
            remaining_items: ledger
                .policy
                .max_total_items
                .saturating_sub(ledger.admitted.saturating_add(reserved)),
            sources,
        }
    }

    pub async fn replace_policy(
        &self,
        principal: &str,
        workspace: &str,
        expected_revision: u64,
        mut policy: ObserveCatchUpPolicy,
    ) -> Result<ObserveCatchUpStatus> {
        policy.validate()?;
        let _mutation = self.mutation_lock.lock().await;
        let current =
            load_observe_catch_up_policy(&self.workspace_layout, principal, workspace).await;
        if current.revision != expected_revision {
            bail!(
                "Observe catch-up policy changed (expected revision {}, current {})",
                expected_revision,
                current.revision
            );
        }
        policy.schema_version = CATCH_UP_SCHEMA_VERSION;
        policy.revision = current.revision.saturating_add(1);
        write_observe_catch_up_policy(&self.workspace_layout, principal, workspace, &policy)
            .await?;
        self.scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry((principal.to_string(), workspace.to_string()))
            .and_modify(|ledger| ledger.reset(policy.clone()))
            .or_insert_with(|| RuntimeLedger::new(policy));
        drop(_mutation);
        Ok(self.status(principal, workspace).await)
    }

    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }

    pub fn boot_started_at_ms(&self) -> i64 {
        self.boot_started_at_ms
    }
}

fn baseline_source_statuses() -> Vec<CatchUpSourceStatus> {
    vec![
        CatchUpSourceStatus {
            source_id: "message_processing".to_string(),
            display_name: "Local message understanding".to_string(),
            item_unit: "messages".to_string(),
            replay_mode: CatchUpReplayMode::CheckpointedReplay,
            admitted: 0,
            processed: 0,
            runs: 0,
            failures: 0,
            last_error: None,
            limitation: "Only metadata admitted by message sync is considered; bodies remain local and sensitive rows stay suppressed.".to_string(),
        },
        CatchUpSourceStatus {
            source_id: "messages".to_string(),
            display_name: "Mail & chat".to_string(),
            item_unit: "threads per account".to_string(),
            replay_mode: CatchUpReplayMode::CheckpointedReplay,
            admitted: 0,
            processed: 0,
            runs: 0,
            failures: 0,
            last_error: None,
            limitation: "Provider history retention still applies; sensitive content remains suppressed and bodies are processed only by the configured local model.".to_string(),
        },
        CatchUpSourceStatus {
            source_id: "calendar".to_string(),
            display_name: "Calendar".to_string(),
            item_unit: "days".to_string(),
            replay_mode: CatchUpReplayMode::ScheduledWindow,
            admitted: 0,
            processed: 0,
            runs: 0,
            failures: 0,
            last_error: None,
            limitation: "The scheduled evidence writer reads the configured window once when overdue; missed cron ticks are not replayed one by one.".to_string(),
        },
        CatchUpSourceStatus {
            source_id: "public_feeds".to_string(),
            display_name: "Product Hunt, arXiv & RSS".to_string(),
            item_unit: "candidates per subscription".to_string(),
            replay_mode: CatchUpReplayMode::CurrentSnapshotOnly,
            admitted: 0,
            processed: 0,
            runs: 0,
            failures: 0,
            last_error: None,
            limitation: "Feeds expose their current entries. Items that aged out during downtime cannot be reconstructed without a separate archive API.".to_string(),
        },
        CatchUpSourceStatus {
            source_id: "notes".to_string(),
            display_name: "Published notes".to_string(),
            item_unit: "notes".to_string(),
            replay_mode: CatchUpReplayMode::CheckpointedReplay,
            admitted: 0,
            processed: 0,
            runs: 0,
            failures: 0,
            last_error: None,
            limitation: "Only notes published into the configured observation source are eligible.".to_string(),
        },
    ]
}

fn source_baseline_id<'a>(source_id: &'a str, item_unit: &str) -> Option<&'a str> {
    if source_id.starts_with("messages:") {
        Some("messages")
    } else if source_id == "message_processing" {
        Some("message_processing")
    } else if source_id.starts_with("observe:") && item_unit == "notes" {
        Some("notes")
    } else if source_id.starts_with("observe:") && item_unit == "candidates" {
        Some("public_feeds")
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutObserveCatchUpPolicy {
    pub expected_revision: u64,
    pub enabled: bool,
    pub lookback_days: u32,
    pub max_items_per_source: usize,
    pub max_total_items: usize,
    pub max_duration_minutes: u32,
}

impl From<PutObserveCatchUpPolicy> for ObserveCatchUpPolicy {
    fn from(value: PutObserveCatchUpPolicy) -> Self {
        Self {
            schema_version: CATCH_UP_SCHEMA_VERSION,
            revision: value.expected_revision,
            enabled: value.enabled,
            lookback_days: value.lookback_days,
            max_items_per_source: value.max_items_per_source,
            max_total_items: value.max_total_items,
            max_duration_minutes: value.max_duration_minutes,
        }
    }
}

pub type SharedObserveCatchUpController = Arc<ObserveCatchUpController>;

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_non_product_values_and_inverted_caps() {
        let mut policy = ObserveCatchUpPolicy::default();
        policy.lookback_days = 365;
        assert!(policy.validate().is_err());
        policy.lookback_days = 7;
        policy.max_items_per_source = 200;
        policy.max_total_items = 100;
        assert!(policy.validate().is_err());
    }

    #[tokio::test]
    async fn admission_is_tree_wide_and_returns_unused_reservation() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let controller = ObserveCatchUpController::new(workspace);
        let first = controller
            .begin(
                "owner",
                "default",
                "messages:gmail:work",
                "Gmail · work",
                "threads",
                CatchUpReplayMode::CheckpointedReplay,
                50,
                "test",
            )
            .await;
        let CatchUpDecision::Admit(first) = first else {
            panic!("first source should be admitted");
        };
        controller.complete("owner", "default", first, 7, None);
        let status = controller.status("owner", "default").await;
        assert_eq!(status.processed_items, 7);
        assert_eq!(status.reserved_items, 0);
        assert_eq!(status.remaining_items, 193);
        assert!(status
            .sources
            .iter()
            .any(|source| source.source_id == "messages:gmail:work"));
        assert!(!status
            .sources
            .iter()
            .any(|source| source.source_id == "messages"));
    }

    #[tokio::test]
    async fn concurrent_source_reservations_cannot_exceed_the_shared_cap() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let controller = ObserveCatchUpController::new(ArtifactV2Workspace::new(temp.path()));
        for index in 0..4 {
            let decision = controller
                .begin(
                    "owner",
                    "default",
                    &format!("feed:{index}"),
                    "Feed",
                    "candidates",
                    CatchUpReplayMode::CurrentSnapshotOnly,
                    200,
                    "test",
                )
                .await;
            assert!(matches!(decision, CatchUpDecision::Admit(_)));
        }
        assert_eq!(
            controller
                .begin(
                    "owner",
                    "default",
                    "feed:overflow",
                    "Overflow feed",
                    "candidates",
                    CatchUpReplayMode::CurrentSnapshotOnly,
                    200,
                    "test",
                )
                .await,
            CatchUpDecision::SkipHistorical {
                retain_from_ms: controller.boot_started_at_ms(),
                reason: "total_budget_exhausted",
            }
        );
        let status = controller.status("owner", "default").await;
        assert_eq!(status.reserved_items, 200);
        assert_eq!(status.remaining_items, 0);
        assert_eq!(status.phase, CatchUpPhase::Completed);
    }

    #[tokio::test]
    async fn completed_historical_pass_requires_a_post_boot_baseline() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let controller = ObserveCatchUpController::new(workspace);
        let decision = controller
            .begin(
                "owner",
                "default",
                "observe:feed",
                "Feed",
                "candidates",
                CatchUpReplayMode::CurrentSnapshotOnly,
                10,
                "test",
            )
            .await;
        let CatchUpDecision::Admit(admission) = decision else {
            panic!("startup pass should be admitted");
        };
        controller.complete("owner", "default", admission, 3, None);

        assert!(matches!(
            controller
                .begin(
                    "owner",
                    "default",
                    "observe:feed",
                    "Feed",
                    "candidates",
                    CatchUpReplayMode::CurrentSnapshotOnly,
                    10,
                    "test",
                )
                .await,
            CatchUpDecision::SkipHistorical {
                reason: "post_boot_baseline_required",
                ..
            }
        ));
        controller.finish_skipped(
            "owner",
            "default",
            "observe:feed",
            "Feed",
            "candidates",
            CatchUpReplayMode::CurrentSnapshotOnly,
            "test",
            None,
            true,
        );
        assert_eq!(
            controller
                .begin(
                    "owner",
                    "default",
                    "observe:feed",
                    "Feed",
                    "candidates",
                    CatchUpReplayMode::CurrentSnapshotOnly,
                    10,
                    "test",
                )
                .await,
            CatchUpDecision::Normal
        );
    }

    #[tokio::test]
    async fn failed_or_unavailable_pass_keeps_the_source_eligible() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let controller = ObserveCatchUpController::new(workspace);
        let decision = controller
            .begin(
                "owner",
                "default",
                "message_processing",
                "Local message understanding",
                "messages",
                CatchUpReplayMode::CheckpointedReplay,
                10,
                "test",
            )
            .await;
        let CatchUpDecision::Admit(admission) = decision else {
            panic!("startup pass should be admitted");
        };
        assert!(admission.initial_batch);
        controller.complete_with_exhaustion(
            "owner",
            "default",
            admission,
            0,
            Some("local provider unavailable"),
            false,
        );

        let retry = controller
            .begin(
                "owner",
                "default",
                "message_processing",
                "Local message understanding",
                "messages",
                CatchUpReplayMode::CheckpointedReplay,
                10,
                "test",
            )
            .await;
        let CatchUpDecision::Admit(retry) = retry else {
            panic!("failed source should remain eligible");
        };
        assert!(retry.initial_batch);
        controller.complete_with_exhaustion("owner", "default", retry, 5, None, false);
        let next_batch = controller
            .begin(
                "owner",
                "default",
                "message_processing",
                "Local message understanding",
                "messages",
                CatchUpReplayMode::CheckpointedReplay,
                10,
                "test",
            )
            .await;
        let CatchUpDecision::Admit(next_batch) = next_batch else {
            panic!("non-exhausted source should receive another bounded batch");
        };
        assert!(!next_batch.initial_batch);
    }

    #[tokio::test]
    async fn policy_migrates_the_existing_message_history_choice() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let mut channel = crate::magician_v2::observe_connectors::ChannelObserveConfig::default();
        channel.history_lookback_days = 30;
        crate::magician_v2::observe_connectors::write_channel_observe(
            &workspace, "owner", "default", &channel,
        )
        .await
        .expect("write old channel preference");

        let policy = load_observe_catch_up_policy(&workspace, "owner", "default").await;
        assert_eq!(policy.lookback_days, 30);
        assert!(open_local_durable_artifacts(&workspace, "owner", "default")
            .expect("scope store")
            .read(CATCH_UP_NAMESPACE, CATCH_UP_CONFIG_NAME)
            .await
            .is_ok());
    }
}
