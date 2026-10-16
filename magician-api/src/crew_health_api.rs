//! Durable, scoped crew-health read model.
//!
//! Health is an operational projection, not a diagnosis or an execution gate.
//! The live overall score combines rolling seven-day LLM reliability, recent
//! activity, and current runtime/task state. One versioned snapshot per UTC day
//! is retained so clients can render trends without inventing their own score.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{NaiveDate, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::analytics_api::query_agent_llm_health_rollups;
use crate::analytics_api::AgentLlmHealthRollup;
use magician::magician_v2::artifact_v2::{
    service::ArtifactV2Error, workspace::ArtifactV2Workspace,
};

pub const CREW_HEALTH_SCHEMA_VERSION: &str = "crew_health.v1";
pub const CREW_HEALTH_FORMULA_VERSION: u32 = 1;
const CREW_HEALTH_HISTORY_SCHEMA_VERSION: &str = "crew_health_history.v1";
const HISTORY_RETENTION_DAYS: i64 = 90;
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX_SCOPES: usize = 16;
const ANALYTICS_QUERY_TIMEOUT: Duration = Duration::from_secs(15);
const DAILY_SNAPSHOT_MIN_WRITE_INTERVAL_MS: i64 = 60 * 60 * 1_000;

#[derive(Debug, Clone)]
pub(crate) struct AgentHealthInput {
    pub agent_id: String,
    pub runtime_status: String,
    pub state: AgentHealthState,
    pub last_task_activity_at_ms: Option<i64>,
    pub task_activity_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealthState {
    Working,
    NeedsAttention,
    Paused,
    Offline,
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealthBand {
    Good,
    Fair,
    Poor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealthConfidenceLevel {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthCoverage {
    pub ratio: f64,
    pub level: AgentHealthConfidenceLevel,
    pub present: Vec<String>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthInputs {
    pub runtime_status: String,
    pub state: AgentHealthState,
    pub observation_window_started_at_ms: i64,
    pub observation_window_ended_at_ms: i64,
    pub calls_7d: u64,
    pub spend_usd_7d: f64,
    #[serde(default)]
    pub cost_observed_calls: u64,
    pub success_rate_7d: Option<f64>,
    pub last_llm_call_at_ms: Option<i64>,
    pub last_task_activity_at_ms: Option<i64>,
    pub last_activity_at_ms: Option<i64>,
    pub llm_analytics_available: bool,
    pub task_activity_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentHealthContributions {
    pub baseline: i32,
    pub quality: i32,
    pub recency: i32,
    pub state: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentOverallHealth {
    pub score: i32,
    pub band: AgentHealthBand,
    pub observed_at_ms: i64,
    pub formula_version: u32,
    pub coverage: AgentHealthCoverage,
    pub inputs: AgentHealthInputs,
    pub contributions: AgentHealthContributions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthSnapshot {
    pub date: String,
    pub observed_at_ms: i64,
    pub score: i32,
    pub band: AgentHealthBand,
    pub formula_version: u32,
    pub coverage: AgentHealthCoverage,
    pub inputs: AgentHealthInputs,
    pub contributions: AgentHealthContributions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealthTrend {
    Improving,
    Stable,
    Declining,
    InsufficientHistory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthRolling7d {
    pub window_started_at_ms: i64,
    pub window_ended_at_ms: i64,
    pub calls: u64,
    pub spend_usd: f64,
    #[serde(default)]
    pub cost_observed_calls: u64,
    pub success_rate: Option<f64>,
    pub last_call_at_ms: Option<i64>,
    pub score_average: Option<f64>,
    pub score_delta: Option<i32>,
    pub trend: AgentHealthTrend,
    pub sample_days: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthProjection {
    pub agent_id: String,
    pub overall: AgentOverallHealth,
    pub rolling_7d: AgentHealthRolling7d,
    pub history: Vec<AgentHealthSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrewHealthAvailabilityStatus {
    Available,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrewHealthAvailability {
    pub status: CrewHealthAvailabilityStatus,
    pub llm_analytics: bool,
    pub durable_history: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrewHealthScope {
    pub principal: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrewHealthResponse {
    pub schema_version: String,
    pub formula_version: u32,
    pub generated_at: String,
    pub scope: CrewHealthScope,
    pub availability: CrewHealthAvailability,
    pub agents: Vec<AgentHealthProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrewAgentHealthResponse {
    pub schema_version: String,
    pub formula_version: u32,
    pub generated_at: String,
    pub scope: CrewHealthScope,
    pub availability: CrewHealthAvailability,
    pub agent: AgentHealthProjection,
}

impl CrewHealthResponse {
    pub fn agent_response(&self, agent_id: &str) -> Option<CrewAgentHealthResponse> {
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)?
            .clone();
        Some(CrewAgentHealthResponse {
            schema_version: self.schema_version.clone(),
            formula_version: self.formula_version,
            generated_at: self.generated_at.clone(),
            scope: self.scope.clone(),
            availability: self.availability.clone(),
            agent,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrewHealthHistoryFile {
    schema_version: String,
    formula_version: u32,
    updated_at_ms: i64,
    #[serde(default)]
    agents: BTreeMap<String, Vec<AgentHealthSnapshot>>,
}

impl CrewHealthHistoryFile {
    fn empty(now_ms: i64) -> Self {
        Self {
            schema_version: CREW_HEALTH_HISTORY_SCHEMA_VERSION.to_string(),
            formula_version: CREW_HEALTH_FORMULA_VERSION,
            updated_at_ms: now_ms,
            agents: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct CrewHealthService {
    workspace: ArtifactV2Workspace,
    cache: Arc<RwLock<HashMap<(String, String), CachedCrewHealth>>>,
    refresh_gate: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct CachedCrewHealth {
    cached_at: Instant,
    response: CrewHealthResponse,
}

impl CrewHealthService {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            workspace,
            cache: Arc::new(RwLock::new(HashMap::new())),
            refresh_gate: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) async fn project(
        &self,
        principal: String,
        workspace: String,
        mut inputs: Vec<AgentHealthInput>,
    ) -> CrewHealthResponse {
        let cache_key = (principal.clone(), workspace.clone());
        if let Some(response) = self.cached_response(&cache_key).await {
            return response;
        }

        let _refresh_guard = self.refresh_gate.lock().await;
        if let Some(response) = self.cached_response(&cache_key).await {
            return response;
        }

        inputs.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        let now_ms = Utc::now().timestamp_millis();
        let (llm_rollups, llm_analytics_available, analytics_limitation) =
            self.load_llm_rollups(&principal, &workspace).await;

        let drafts = inputs
            .into_iter()
            .map(|input| {
                let agent_id = input.agent_id.clone();
                let rollup = llm_rollups.get(&input.agent_id);
                let health_inputs =
                    build_health_inputs(input, rollup, llm_analytics_available, now_ms);
                let overall = compute_overall_health(health_inputs, now_ms);
                let snapshot = snapshot_from_overall(&overall, now_ms);
                (agent_id, overall, snapshot)
            })
            .collect::<Vec<_>>();
        let mut limitations = Vec::new();
        if let Some(limitation) = analytics_limitation {
            limitations.push(limitation);
        }

        let history_result = self.read_history(&principal, &workspace, now_ms).await;
        let (mut persisted_history, history_read_ok) = match history_result {
            Ok(history) => (history, true),
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error,
                    "failed to read durable crew health history"
                );
                limitations.push("durable_history_read_failed".to_string());
                (CrewHealthHistoryFile::empty(now_ms), false)
            },
        };

        let mut projections = Vec::new();
        let mut history_changed = false;
        let history_cutoff = utc_date(now_ms)
            .checked_sub_signed(chrono::Duration::days(HISTORY_RETENTION_DAYS - 1))
            .unwrap_or_else(|| utc_date(now_ms));

        for snapshots in persisted_history.agents.values_mut() {
            history_changed |= retain_history(snapshots, history_cutoff);
        }
        let history_agent_count = persisted_history.agents.len();
        persisted_history
            .agents
            .retain(|_, snapshots| !snapshots.is_empty());
        history_changed |= history_agent_count != persisted_history.agents.len();

        for (agent_id, overall, live_snapshot) in drafts {
            let snapshots = persisted_history
                .agents
                .entry(agent_id.clone())
                .or_default();
            let should_persist = merge_daily_snapshot(snapshots, live_snapshot.clone(), false);
            history_changed |= should_persist;

            let mut display_history = snapshots.clone();
            merge_daily_snapshot(&mut display_history, live_snapshot, true);
            retain_history(&mut display_history, history_cutoff);
            display_history.sort_by(|left, right| left.date.cmp(&right.date));

            projections.push(AgentHealthProjection {
                agent_id,
                rolling_7d: rolling_7d(&overall, &display_history, now_ms),
                overall,
                history: display_history,
            });
        }

        let mut durable_history_available = history_read_ok;
        if history_read_ok && history_changed {
            persisted_history.updated_at_ms = now_ms;
            if let Err(error) = self
                .write_history(&principal, &workspace, &persisted_history)
                .await
            {
                tracing::warn!(
                    principal,
                    workspace,
                    error,
                    "failed to persist durable crew health history"
                );
                limitations.push("durable_history_write_failed".to_string());
                durable_history_available = false;
            }
        }

        let availability = CrewHealthAvailability {
            status: if limitations.is_empty() {
                CrewHealthAvailabilityStatus::Available
            } else {
                CrewHealthAvailabilityStatus::Partial
            },
            llm_analytics: llm_analytics_available,
            durable_history: durable_history_available,
            limitations,
        };
        let response = CrewHealthResponse {
            schema_version: CREW_HEALTH_SCHEMA_VERSION.to_string(),
            formula_version: CREW_HEALTH_FORMULA_VERSION,
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            scope: CrewHealthScope {
                principal,
                workspace,
            },
            availability,
            agents: projections,
        };
        self.cache_response(cache_key, response.clone()).await;
        response
    }

    async fn cached_response(&self, key: &(String, String)) -> Option<CrewHealthResponse> {
        self.cache
            .read()
            .await
            .get(key)
            .filter(|cached| cached.cached_at.elapsed() < CACHE_TTL)
            .map(|cached| cached.response.clone())
    }

    async fn cache_response(&self, key: (String, String), response: CrewHealthResponse) {
        let mut cache = self.cache.write().await;
        cache.retain(|_, cached| cached.cached_at.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX_SCOPES && !cache.contains_key(&key) {
            if let Some(oldest_key) = cache
                .iter()
                .max_by_key(|(_, cached)| cached.cached_at.elapsed())
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest_key);
            }
        }
        cache.insert(
            key,
            CachedCrewHealth {
                cached_at: Instant::now(),
                response,
            },
        );
    }

    async fn load_llm_rollups(
        &self,
        principal: &str,
        workspace: &str,
    ) -> (BTreeMap<String, AgentLlmHealthRollup>, bool, Option<String>) {
        let layout = self.workspace.clone();
        let principal_owned = principal.to_string();
        let workspace_owned = workspace.to_string();
        let result = tokio::time::timeout(
            ANALYTICS_QUERY_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                query_agent_llm_health_rollups(&layout, &principal_owned, &workspace_owned)
            }),
        )
        .await;

        match result {
            Ok(Ok(Ok(rollups))) => (
                rollups
                    .into_iter()
                    .map(|rollup| (rollup.agent_id.clone(), rollup))
                    .collect(),
                true,
                None,
            ),
            Ok(Ok(Err(error))) => {
                tracing::warn!(principal, workspace, error = %error, "crew health LLM rollup unavailable");
                (
                    BTreeMap::new(),
                    false,
                    Some("llm_analytics_unavailable".to_string()),
                )
            },
            Ok(Err(error)) => {
                tracing::warn!(principal, workspace, error = %error, "crew health LLM rollup task failed");
                (
                    BTreeMap::new(),
                    false,
                    Some("llm_analytics_task_failed".to_string()),
                )
            },
            Err(_) => {
                tracing::warn!(principal, workspace, "crew health LLM rollup timed out");
                (
                    BTreeMap::new(),
                    false,
                    Some("llm_analytics_timed_out".to_string()),
                )
            },
        }
    }

    async fn read_history(
        &self,
        principal: &str,
        workspace: &str,
        now_ms: i64,
    ) -> Result<CrewHealthHistoryFile, String> {
        let path = self
            .workspace
            .analytics_agent_health_history_path(principal, workspace);
        match self
            .workspace
            .read_json_path::<CrewHealthHistoryFile, _>(&path)
            .await
        {
            Ok(history)
                if history.schema_version == CREW_HEALTH_HISTORY_SCHEMA_VERSION
                    && history.formula_version == CREW_HEALTH_FORMULA_VERSION =>
            {
                Ok(history)
            },
            Ok(history) => Err(format!(
                "unsupported history schema/formula {}/{}",
                history.schema_version, history.formula_version
            )),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CrewHealthHistoryFile::empty(now_ms))
            },
            Err(error) => Err(error.to_string()),
        }
    }

    async fn write_history(
        &self,
        principal: &str,
        workspace: &str,
        history: &CrewHealthHistoryFile,
    ) -> Result<(), String> {
        let root = self
            .workspace
            .analytics_agent_health_root(principal, workspace);
        self.workspace
            .create_dir_all_path(&root)
            .await
            .map_err(|error| error.to_string())?;
        self.workspace
            .write_json_atomic_path(
                self.workspace
                    .analytics_agent_health_history_path(principal, workspace),
                history,
            )
            .await
            .map_err(|error| error.to_string())
    }
}

fn build_health_inputs(
    input: AgentHealthInput,
    rollup: Option<&AgentLlmHealthRollup>,
    llm_analytics_available: bool,
    now_ms: i64,
) -> AgentHealthInputs {
    let last_llm_call_at_ms = rollup.and_then(|rollup| rollup.last_call_at_ms);
    AgentHealthInputs {
        runtime_status: input.runtime_status,
        state: input.state,
        observation_window_started_at_ms: now_ms.saturating_sub(7 * 24 * 60 * 60 * 1_000),
        observation_window_ended_at_ms: now_ms,
        calls_7d: rollup.map_or(0, |rollup| rollup.calls_7d),
        spend_usd_7d: rollup.map_or(0.0, |rollup| rollup.spend_usd_7d),
        cost_observed_calls: rollup.map_or(0, |rollup| rollup.cost_observed_calls),
        success_rate_7d: rollup.and_then(|rollup| rollup.success_rate_7d),
        last_llm_call_at_ms,
        last_task_activity_at_ms: input.last_task_activity_at_ms,
        last_activity_at_ms: max_optional(last_llm_call_at_ms, input.last_task_activity_at_ms),
        llm_analytics_available,
        task_activity_available: input.task_activity_available,
    }
}

fn compute_overall_health(inputs: AgentHealthInputs, now_ms: i64) -> AgentOverallHealth {
    let quality = inputs.success_rate_7d.map_or(0, |rate| {
        (((rate.clamp(0.0, 1.0) - 0.9) * 200.0).clamp(-25.0, 20.0)).round() as i32
    });
    let recency = inputs.last_activity_at_ms.map_or(0, |last_activity_at_ms| {
        let age_ms = now_ms.saturating_sub(last_activity_at_ms).max(0);
        if age_ms < 60 * 60 * 1_000 {
            10
        } else if age_ms < 24 * 60 * 60 * 1_000 {
            5
        } else if age_ms > 7 * 24 * 60 * 60 * 1_000 {
            -15
        } else {
            0
        }
    });
    let state = match inputs.state {
        AgentHealthState::Working => 5,
        AgentHealthState::NeedsAttention => -10,
        AgentHealthState::Paused => -5,
        AgentHealthState::Offline => -25,
        AgentHealthState::Idle => 0,
    };
    let contributions = AgentHealthContributions {
        baseline: 70,
        quality,
        recency,
        state,
    };
    let score = (contributions.baseline
        + contributions.quality
        + contributions.recency
        + contributions.state)
        .clamp(5, 100);
    AgentOverallHealth {
        score,
        band: health_band(score),
        observed_at_ms: now_ms,
        formula_version: CREW_HEALTH_FORMULA_VERSION,
        coverage: health_coverage(&inputs),
        inputs,
        contributions,
    }
}

fn health_coverage(inputs: &AgentHealthInputs) -> AgentHealthCoverage {
    let mut present = vec!["runtime_state".to_string()];
    let mut missing = Vec::new();
    if inputs.task_activity_available {
        present.push("task_activity_source".to_string());
    } else {
        missing.push("task_activity_source".to_string());
    }
    if inputs.llm_analytics_available {
        present.push("llm_analytics_source".to_string());
    } else {
        missing.push("llm_analytics_source".to_string());
    }
    if inputs.last_activity_at_ms.is_some() {
        present.push("recent_activity".to_string());
    } else {
        missing.push("recent_activity".to_string());
    }
    if inputs.success_rate_7d.is_some() {
        present.push("llm_quality_7d".to_string());
    } else {
        missing.push("llm_quality_7d".to_string());
    }
    let ratio = present.len() as f64 / 5.0;
    let level = if ratio >= 0.8 {
        AgentHealthConfidenceLevel::High
    } else if ratio >= 0.5 {
        AgentHealthConfidenceLevel::Medium
    } else {
        AgentHealthConfidenceLevel::Low
    };
    AgentHealthCoverage {
        ratio: (ratio * 100.0).round() / 100.0,
        level,
        present,
        missing,
    }
}

fn snapshot_from_overall(overall: &AgentOverallHealth, now_ms: i64) -> AgentHealthSnapshot {
    AgentHealthSnapshot {
        date: utc_date(now_ms).format("%Y-%m-%d").to_string(),
        observed_at_ms: now_ms,
        score: overall.score,
        band: overall.band.clone(),
        formula_version: overall.formula_version,
        coverage: overall.coverage.clone(),
        inputs: overall.inputs.clone(),
        contributions: overall.contributions.clone(),
    }
}

fn merge_daily_snapshot(
    snapshots: &mut Vec<AgentHealthSnapshot>,
    snapshot: AgentHealthSnapshot,
    force_live: bool,
) -> bool {
    if let Some(existing) = snapshots
        .iter_mut()
        .find(|existing| existing.date == snapshot.date)
    {
        if force_live
            || snapshot
                .observed_at_ms
                .saturating_sub(existing.observed_at_ms)
                >= DAILY_SNAPSHOT_MIN_WRITE_INTERVAL_MS
        {
            *existing = snapshot;
            return !force_live;
        }
        return false;
    }
    snapshots.push(snapshot);
    true
}

fn retain_history(snapshots: &mut Vec<AgentHealthSnapshot>, cutoff: NaiveDate) -> bool {
    let before = snapshots.clone();
    snapshots.retain(|snapshot| {
        NaiveDate::parse_from_str(&snapshot.date, "%Y-%m-%d")
            .map(|date| date >= cutoff)
            .unwrap_or(false)
    });
    let mut by_date = BTreeMap::new();
    for snapshot in snapshots.drain(..) {
        let should_replace =
            by_date
                .get(&snapshot.date)
                .is_none_or(|existing: &AgentHealthSnapshot| {
                    snapshot.observed_at_ms >= existing.observed_at_ms
                });
        if should_replace {
            by_date.insert(snapshot.date.clone(), snapshot);
        }
    }
    snapshots.extend(by_date.into_values());
    *snapshots != before
}

fn rolling_7d(
    overall: &AgentOverallHealth,
    history: &[AgentHealthSnapshot],
    now_ms: i64,
) -> AgentHealthRolling7d {
    let window_started_at_ms = now_ms.saturating_sub(7 * 24 * 60 * 60 * 1_000);
    let cutoff = utc_date(window_started_at_ms);
    let samples = history
        .iter()
        .filter(|snapshot| {
            NaiveDate::parse_from_str(&snapshot.date, "%Y-%m-%d")
                .map(|date| date >= cutoff)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let score_average = (!samples.is_empty()).then(|| {
        let total = samples.iter().map(|snapshot| snapshot.score).sum::<i32>();
        ((total as f64 / samples.len() as f64) * 10.0).round() / 10.0
    });
    let score_delta = if samples.len() >= 2 {
        Some(samples.last().unwrap().score - samples.first().unwrap().score)
    } else {
        None
    };
    let trend = match score_delta {
        Some(delta) if delta >= 3 => AgentHealthTrend::Improving,
        Some(delta) if delta <= -3 => AgentHealthTrend::Declining,
        Some(_) => AgentHealthTrend::Stable,
        None => AgentHealthTrend::InsufficientHistory,
    };
    AgentHealthRolling7d {
        window_started_at_ms,
        window_ended_at_ms: now_ms,
        calls: overall.inputs.calls_7d,
        spend_usd: overall.inputs.spend_usd_7d,
        cost_observed_calls: overall.inputs.cost_observed_calls,
        success_rate: overall.inputs.success_rate_7d,
        last_call_at_ms: overall.inputs.last_llm_call_at_ms,
        score_average,
        score_delta,
        trend,
        sample_days: samples.len(),
    }
}

fn health_band(score: i32) -> AgentHealthBand {
    if score >= 70 {
        AgentHealthBand::Good
    } else if score >= 40 {
        AgentHealthBand::Fair
    } else {
        AgentHealthBand::Poor
    }
}

fn utc_date(timestamp_ms: i64) -> NaiveDate {
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .unwrap_or_else(Utc::now)
        .date_naive()
}

fn max_optional(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(state: AgentHealthState) -> AgentHealthInputs {
        AgentHealthInputs {
            runtime_status: "idle".to_string(),
            state,
            observation_window_started_at_ms: 0,
            observation_window_ended_at_ms: 10_000_000,
            calls_7d: 10,
            spend_usd_7d: 1.25,
            cost_observed_calls: 10,
            success_rate_7d: Some(0.9),
            last_llm_call_at_ms: Some(9_900_000),
            last_task_activity_at_ms: None,
            last_activity_at_ms: Some(9_900_000),
            llm_analytics_available: true,
            task_activity_available: true,
        }
    }

    #[test]
    fn overall_health_preserves_v1_quality_recency_and_state_weights() {
        let overall = compute_overall_health(input(AgentHealthState::Working), 10_000_000);
        assert_eq!(overall.score, 85);
        assert_eq!(overall.band, AgentHealthBand::Good);
        assert_eq!(overall.contributions.quality, 0);
        assert_eq!(overall.contributions.recency, 10);
        assert_eq!(overall.contributions.state, 5);
    }

    #[test]
    fn rolling_window_reports_average_delta_and_direction() {
        let now_ms = 1_800_000_000_000;
        let overall = compute_overall_health(input(AgentHealthState::Idle), now_ms);
        let mut history = Vec::new();
        for (days_ago, score) in [(6, 55), (3, 63), (0, 72)] {
            let observed_at_ms = now_ms - days_ago * 24 * 60 * 60 * 1_000;
            let mut snapshot = snapshot_from_overall(&overall, observed_at_ms);
            snapshot.score = score;
            snapshot.band = health_band(score);
            history.push(snapshot);
        }
        let rolling = rolling_7d(&overall, &history, now_ms);
        assert_eq!(rolling.sample_days, 3);
        assert_eq!(rolling.score_average, Some(63.3));
        assert_eq!(rolling.score_delta, Some(17));
        assert_eq!(rolling.trend, AgentHealthTrend::Improving);
    }

    #[test]
    fn daily_snapshot_is_bounded_to_one_row_and_refreshes_after_interval() {
        let now_ms = 1_800_000_000_000;
        let overall = compute_overall_health(input(AgentHealthState::Idle), now_ms);
        let initial = snapshot_from_overall(&overall, now_ms);
        let mut snapshots = vec![initial.clone()];

        let mut too_soon = initial.clone();
        too_soon.observed_at_ms += 30 * 60 * 1_000;
        too_soon.score = 50;
        assert!(!merge_daily_snapshot(&mut snapshots, too_soon, false));
        assert_eq!(snapshots[0].score, initial.score);

        let mut later = initial;
        later.observed_at_ms += DAILY_SNAPSHOT_MIN_WRITE_INTERVAL_MS;
        later.score = 60;
        assert!(merge_daily_snapshot(&mut snapshots, later, false));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].score, 60);
    }

    #[test]
    fn retention_removes_expired_and_duplicate_daily_snapshots() {
        let now_ms = 1_800_000_000_000;
        let overall = compute_overall_health(input(AgentHealthState::Idle), now_ms);
        let mut current = snapshot_from_overall(&overall, now_ms);
        current.score = 75;
        let mut newer_duplicate = current.clone();
        newer_duplicate.observed_at_ms += 1;
        newer_duplicate.score = 80;
        let expired_at_ms = now_ms - 100 * 24 * 60 * 60 * 1_000;
        let expired = snapshot_from_overall(&overall, expired_at_ms);
        let mut snapshots = vec![current, newer_duplicate, expired];

        assert!(retain_history(
            &mut snapshots,
            utc_date(now_ms) - chrono::Duration::days(HISTORY_RETENTION_DAYS - 1)
        ));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].score, 80);
    }

    #[tokio::test]
    async fn service_persists_scoped_daily_history_and_returns_overall_health() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let service = CrewHealthService::new(workspace.clone());
        let response = service
            .project(
                "principal-a".to_string(),
                "workspace-a".to_string(),
                vec![AgentHealthInput {
                    agent_id: "agent-a".to_string(),
                    runtime_status: "running".to_string(),
                    state: AgentHealthState::Working,
                    last_task_activity_at_ms: Some(Utc::now().timestamp_millis()),
                    task_activity_available: true,
                }],
            )
            .await;

        assert_eq!(response.agents.len(), 1);
        assert_eq!(response.agents[0].agent_id, "agent-a");
        assert_eq!(response.agents[0].history.len(), 1);
        assert_eq!(response.agents[0].rolling_7d.sample_days, 1);
        assert!(workspace
            .analytics_agent_health_history_path("principal-a", "workspace-a")
            .is_file());

        let history: CrewHealthHistoryFile = workspace
            .read_json_path(
                workspace.analytics_agent_health_history_path("principal-a", "workspace-a"),
            )
            .await
            .expect("durable history");
        assert_eq!(history.agents["agent-a"].len(), 1);
    }
}
