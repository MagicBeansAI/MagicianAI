use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::{
    AdapterAuth, AdapterCost, ContentAcquisitionCatalog, ContentAcquisitionService,
    ContentCandidate, ContentDocument, ContentInvocationSource, ContentPrivacy,
    ContentSourceDescriptor, DiscoveryRequest, FreshnessPolicy, ReadDepth, ReadRequest,
    ReadSelectionEvidence, ReadSelectionReason, RemoteDataPolicy, RetrievalAuthority,
    RetrievalOperation, RetrievalRung, CONTENT_SOURCE_SCHEMA_VERSION,
};
use crate::magician_v2::analytics::runtime_activity_layer::KIND_AGENT;

pub const RETRIEVAL_NEED_SCHEMA_VERSION: u32 = 1;
const MAX_RETRIEVAL_ACTIONS: usize = 64;
const MAX_RETRIEVAL_RUNGS: usize = 16;
const MAX_RECEIPTS: usize = 4_096;
const MAX_REQUIRED_METADATA_KEYS: usize = 32;
const MAX_BROWSER_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const MAX_BROWSER_DOCUMENT_CHARS: usize = 4 * 1024 * 1024;
const MAX_BROWSER_COMMAND_TIMEOUT_SECS: u64 = 120;
const MAX_BROWSER_RENDER_WAIT_MS: u64 = 30_000;
const MAX_BROWSER_APPROVAL_TTL_SECS: u64 = 3_600;
const MAX_AUTHORITY_GRANTS: usize = 1_024;
pub const META_ACTUAL_TRANSPORT: &str = "_retrieval_actual_transport";
pub const META_REQUESTED_BROWSER_MODE: &str = "_retrieval_requested_browser_mode";
pub const META_RESOLVED_BROWSER_MODE: &str = "_retrieval_resolved_browser_mode";
pub const META_BROWSER_ENGINE: &str = "_retrieval_browser_engine";
pub const META_BROWSER_SESSION_FINGERPRINT: &str = "_retrieval_browser_session_fingerprint";
pub const META_SESSION_OUTCOME: &str = "_retrieval_session_outcome";
pub const META_RENDER_MS: &str = "_retrieval_render_ms";
pub const META_EXTRACT_MS: &str = "_retrieval_extract_ms";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserRetrievalSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub public_headless_reads: bool,
    #[serde(default = "default_true")]
    pub public_handoffs: bool,
    #[serde(default = "default_true")]
    pub authenticated_cdp: bool,
    #[serde(default = "default_true")]
    pub verified_api_replay: bool,
    #[serde(default)]
    pub engine: Option<String>,
    /// Soft preference used only by the isolated deterministic public
    /// headless reader. Failed attempts advance through this preference,
    /// `engine`, and bundled Chrome for Testing; ordinary, headed, visual, and
    /// identity-bearing browser paths continue to use `engine` directly.
    #[serde(default)]
    pub public_read_engine: Option<String>,
    #[serde(default = "default_browser_command_timeout_secs")]
    pub command_timeout_secs: u64,
    #[serde(default = "default_browser_render_wait_ms")]
    pub render_wait_ms: u64,
    #[serde(default = "default_browser_capture_bytes")]
    pub max_capture_bytes: usize,
    #[serde(default = "default_browser_document_chars")]
    pub max_document_chars: usize,
    #[serde(default = "default_browser_approval_ttl_secs")]
    pub approval_ttl_secs: u64,
    #[serde(default = "default_browser_cdp_url")]
    pub cdp_url: String,
}

impl Default for BrowserRetrievalSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            public_headless_reads: true,
            public_handoffs: true,
            authenticated_cdp: true,
            verified_api_replay: true,
            engine: None,
            public_read_engine: None,
            command_timeout_secs: default_browser_command_timeout_secs(),
            render_wait_ms: default_browser_render_wait_ms(),
            max_capture_bytes: default_browser_capture_bytes(),
            max_document_chars: default_browser_document_chars(),
            approval_ttl_secs: default_browser_approval_ttl_secs(),
            cdp_url: default_browser_cdp_url(),
        }
    }
}

impl BrowserRetrievalSettings {
    pub fn validate_bounds(&self) -> Result<()> {
        if self.command_timeout_secs == 0
            || self.command_timeout_secs > MAX_BROWSER_COMMAND_TIMEOUT_SECS
            || self.render_wait_ms == 0
            || self.render_wait_ms > MAX_BROWSER_RENDER_WAIT_MS
            || self.max_capture_bytes == 0
            || self.max_capture_bytes > MAX_BROWSER_CAPTURE_BYTES
            || self.max_document_chars == 0
            || self.max_document_chars > MAX_BROWSER_DOCUMENT_CHARS
            || self.approval_ttl_secs == 0
            || self.approval_ttl_secs > MAX_BROWSER_APPROVAL_TTL_SECS
        {
            bail!("browser retrieval bounds must be positive and within hard limits");
        }
        for (label, engine) in [
            ("engine", self.engine.as_deref()),
            ("public_read_engine", self.public_read_engine.as_deref()),
        ] {
            if engine.is_some_and(|engine| {
                !crate::magician_v2::execution::primitive_dispatch::browser::is_valid_browser_engine_name(
                    engine,
                )
            }) {
                bail!("browser retrieval {label} must be a safe bounded skill name");
            }
        }
        let cdp = url::Url::parse(&self.cdp_url)
            .map_err(|error| anyhow::anyhow!("browser retrieval cdp_url is invalid: {error}"))?;
        let local_host = matches!(
            cdp.host_str().map(str::to_ascii_lowercase).as_deref(),
            Some("127.0.0.1" | "::1" | "localhost")
        );
        if !matches!(cdp.scheme(), "ws" | "wss")
            || !local_host
            || !cdp.username().is_empty()
            || cdp.password().is_some()
            || cdp.path() != "/devtools/browser/magicutor-proxy"
            || cdp.query().is_some()
            || cdp.fragment().is_some()
        {
            bail!(
                "browser retrieval cdp_url must be the credential-free local Magicutor proxy \
                 endpoint"
            );
        }
        Ok(())
    }
}

const MAX_WORKING_SET_AUTO_CAPTURE_AGENTS: usize = 32;
const MAX_WORKING_SET_AUTO_CAPTURE_AGENT_ID_CHARS: usize = 128;
const MAX_WORKING_SET_ACTIVATION_LANES: usize = 16;
const MAX_WORKING_SET_ACTIVATION_LANE_CHARS: usize = 64;

/// Trusted deployment policy for research evidence snapshots. This controls
/// only automatic capture after controller-owned retrieval; it never grants an
/// agent a model-authored evidence-write capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingSetCaptureSettings {
    #[serde(default = "default_working_set_auto_capture_agents")]
    pub auto_capture_agents: Vec<String>,
    /// Boundary B routing gate. Empty `enabled_lanes` keeps working-set
    /// routing off everywhere; a lane is added only after the evaluation
    /// suite demonstrates better source-grounded outcomes for it.
    #[serde(default)]
    pub activation: WorkingSetActivationSettings,
}

impl Default for WorkingSetCaptureSettings {
    fn default() -> Self {
        Self {
            auto_capture_agents: default_working_set_auto_capture_agents(),
            activation: WorkingSetActivationSettings::default(),
        }
    }
}

/// When a qualifying research task should take the working-set path instead
/// of ordinary context packing.
///
/// This is Boundary B's activation rule from the bounded-research plan: the
/// path activates only for an explicitly enabled lane AND only when the task
/// exceeds at least one explicit threshold — input volume, source count, or
/// investigation depth. The decision is pure so the evaluation suite and the
/// eventual router share one implementation, and the reasons are sentences so
/// a routing decision is auditable after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingSetActivationSettings {
    /// Lanes the Boundary B decision gate has opened. Deliberately empty by
    /// default: evaluation evidence comes first, routing second.
    #[serde(default)]
    pub enabled_lanes: Vec<String>,
    /// Which agents belong to which lane. Configuration, not code: the router
    /// asks `lane_for_agent`, and an agent in no lane never activates however
    /// much it reads. The shipped default puts the web researcher in
    /// `web-research`, the lane the evaluation suite measured.
    #[serde(default = "default_activation_lanes")]
    pub lanes: std::collections::BTreeMap<String, Vec<String>>,
    /// Total input volume at or above which the path qualifies: past this,
    /// context packing will not keep everything the task read, so the
    /// durable set pays even when every page was shown whole.
    #[serde(default = "default_activation_min_total_source_bytes")]
    pub min_total_source_bytes: u64,
    /// Bytes the ordinary page window could not show — summed over distinct
    /// pages, each page's bytes past the window — at or above which the path
    /// qualifies. This is the measure that says the set holds something the
    /// model has not seen. Source count and read depth used to be legs of
    /// this rule; they put ordinary research on the path (three pages over
    /// two rounds is any comparison) and were removed the day the lane was
    /// opened.
    #[serde(default = "default_activation_min_beyond_window_bytes")]
    pub min_beyond_window_bytes: u64,
}

fn default_activation_lanes() -> std::collections::BTreeMap<String, Vec<String>> {
    std::collections::BTreeMap::from([(
        "web-research".to_string(),
        vec!["web-researcher".to_string()],
    )])
}

impl Default for WorkingSetActivationSettings {
    fn default() -> Self {
        Self {
            enabled_lanes: Vec::new(),
            lanes: default_activation_lanes(),
            min_total_source_bytes: default_activation_min_total_source_bytes(),
            min_beyond_window_bytes: default_activation_min_beyond_window_bytes(),
        }
    }
}

/// What one candidate task looks like to the activation rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingSetActivationProbe<'a> {
    pub lane: &'a str,
    pub total_source_bytes: u64,
    /// What the ordinary window could not show, summed over distinct pages.
    pub beyond_window_bytes: u64,
}

/// The activation outcome, with the sentence that justifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingSetActivationDecision {
    Activate { reason: String },
    Stay { reason: String },
}

impl WorkingSetActivationSettings {
    /// The lane an agent works in, if it is in one.
    pub fn lane_for_agent(&self, agent_id: &str) -> Option<&str> {
        self.lanes
            .iter()
            .find(|(_, agents)| agents.iter().any(|configured| configured == agent_id))
            .map(|(lane, _)| lane.as_str())
    }

    /// Whether the owner has opened a lane's gate.
    pub fn lane_is_open(&self, lane: &str) -> bool {
        self.enabled_lanes.iter().any(|enabled| enabled == lane)
    }

    pub fn decide(&self, probe: &WorkingSetActivationProbe<'_>) -> WorkingSetActivationDecision {
        if !self.lane_is_open(probe.lane) {
            return WorkingSetActivationDecision::Stay {
                reason: format!(
                    "lane `{}` has no working-set evaluation evidence yet; the routing gate is \
                     closed",
                    probe.lane
                ),
            };
        }
        if probe.beyond_window_bytes >= self.min_beyond_window_bytes {
            return WorkingSetActivationDecision::Activate {
                reason: format!(
                    "{} bytes beyond the window meet the {}-byte activation threshold",
                    probe.beyond_window_bytes, self.min_beyond_window_bytes
                ),
            };
        }
        if probe.total_source_bytes >= self.min_total_source_bytes {
            return WorkingSetActivationDecision::Activate {
                reason: format!(
                    "input volume {} bytes meets the {}-byte activation threshold",
                    probe.total_source_bytes, self.min_total_source_bytes
                ),
            };
        }
        WorkingSetActivationDecision::Stay {
            reason: format!(
                "below every activation threshold ({} of {} bytes beyond the window, {} of {} \
                 bytes in total)",
                probe.beyond_window_bytes,
                self.min_beyond_window_bytes,
                probe.total_source_bytes,
                self.min_total_source_bytes
            ),
        }
    }

    pub fn validate_bounds(&self) -> Result<()> {
        if self.enabled_lanes.len() > MAX_WORKING_SET_ACTIVATION_LANES {
            bail!(
                "working-set activation has more than {MAX_WORKING_SET_ACTIVATION_LANES} enabled \
                 lanes"
            );
        }
        let mut seen = std::collections::HashSet::new();
        for lane in &self.enabled_lanes {
            if lane.trim().is_empty()
                || lane.trim() != lane
                || lane.chars().count() > MAX_WORKING_SET_ACTIVATION_LANE_CHARS
                || !seen.insert(lane)
            {
                bail!(
                    "working-set activation lanes must be unique, trimmed, non-empty ids of at \
                     most {MAX_WORKING_SET_ACTIVATION_LANE_CHARS} characters"
                );
            }
        }
        if self.lanes.len() > MAX_WORKING_SET_ACTIVATION_LANES {
            bail!("working-set activation maps more than {MAX_WORKING_SET_ACTIVATION_LANES} lanes");
        }
        let mut lane_agents = std::collections::HashSet::new();
        for (lane, agents) in &self.lanes {
            if lane.trim().is_empty()
                || lane.trim() != lane
                || lane.chars().count() > MAX_WORKING_SET_ACTIVATION_LANE_CHARS
            {
                bail!(
                    "working-set activation lane names must be trimmed, non-empty ids of at \
                     most {MAX_WORKING_SET_ACTIVATION_LANE_CHARS} characters"
                );
            }
            if agents.is_empty() || agents.len() > MAX_WORKING_SET_AUTO_CAPTURE_AGENTS {
                bail!("working-set activation lane `{lane}` must list 1-{MAX_WORKING_SET_AUTO_CAPTURE_AGENTS} agents");
            }
            for agent_id in agents {
                if agent_id.trim().is_empty()
                    || agent_id.trim() != agent_id
                    || agent_id.chars().count() > MAX_WORKING_SET_AUTO_CAPTURE_AGENT_ID_CHARS
                {
                    bail!(
                        "working-set activation lane `{lane}` has an agent id that is not a \
                         trimmed, non-empty id of at most \
                         {MAX_WORKING_SET_AUTO_CAPTURE_AGENT_ID_CHARS} characters"
                    );
                }
                // One agent, one lane: a routing decision must not depend on
                // which of two lanes was checked first.
                if !lane_agents.insert(agent_id.as_str()) {
                    bail!("agent `{agent_id}` is listed in more than one working-set lane");
                }
            }
        }
        // A zero threshold would activate on every task in an enabled lane,
        // which contradicts the plan's requirement of explicit thresholds.
        if self.min_total_source_bytes == 0 || self.min_beyond_window_bytes == 0 {
            bail!("working-set activation thresholds must be positive");
        }
        Ok(())
    }
}

fn default_activation_min_total_source_bytes() -> u64 {
    // Roughly a 24k-token corpus: below this, ordinary context packing ships
    // the whole input anyway and the durable snapshot buys nothing.
    96 * 1024
}

fn default_activation_min_beyond_window_bytes() -> u64 {
    // Four ordinary windows' worth hidden. Activation narrows every
    // over-window page to a short head, so the set must hold clearly more
    // than the narrowing takes away before the path pays; one long
    // documentation page and a half is about this, a pricing comparison of
    // three ordinary pages is not.
    24 * 1024
}

impl WorkingSetCaptureSettings {
    pub fn automatically_captures(&self, agent_id: &str) -> bool {
        self.auto_capture_agents
            .iter()
            .any(|configured_agent| configured_agent == agent_id)
    }

    pub fn validate_bounds(&self) -> Result<()> {
        if self.auto_capture_agents.len() > MAX_WORKING_SET_AUTO_CAPTURE_AGENTS {
            bail!(
                "working-set capture has more than {MAX_WORKING_SET_AUTO_CAPTURE_AGENTS} \
                 automatic agents"
            );
        }
        let mut seen = std::collections::HashSet::new();
        for agent_id in &self.auto_capture_agents {
            if agent_id.trim().is_empty()
                || agent_id.trim() != agent_id
                || agent_id.chars().count() > MAX_WORKING_SET_AUTO_CAPTURE_AGENT_ID_CHARS
                || !seen.insert(agent_id)
            {
                bail!(
                    "working-set automatic capture agents must be unique, trimmed, non-empty ids \
                     of at most {MAX_WORKING_SET_AUTO_CAPTURE_AGENT_ID_CHARS} characters"
                );
            }
        }
        self.activation.validate_bounds()
    }
}

impl ContentAcquisitionSettings {
    pub fn validate_bounds(&self) -> Result<()> {
        self.progressive_retrieval.validate_bounds()?;
        self.browser.validate_bounds()?;
        self.observe.validate_bounds()?;
        self.working_sets.validate_bounds()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressiveRetrievalRollout {
    Off,
    OptIn,
    Authoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRungMode {
    Sequential,
    EligibleParallel,
    /// Phase 7 accepts this mode but deliberately preserves configured order.
    /// Health-based reordering is owned by Phase 9.
    AdaptiveWithinRung,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalRungConfig {
    pub id: String,
    pub mode: RetrievalRungMode,
    #[serde(default)]
    pub actions: Vec<String>,
    /// Missing optional actions are reported in readiness and skipped. They
    /// never turn into an implicit provider or alter required action order.
    #[serde(default)]
    pub optional_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressiveRetrievalSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_rollout")]
    pub rollout_mode: ProgressiveRetrievalRollout,
    #[serde(default = "default_deadline_ms")]
    pub default_deadline_ms: u64,
    /// Maximum wall-clock time given to one discovery provider before the
    /// sequential ladder advances. The request deadline remains the outer
    /// bound, so callers cannot expand either limit.
    #[serde(default = "default_discovery_attempt_timeout_ms")]
    pub discovery_attempt_timeout_ms: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
    #[serde(default = "default_max_parallel_actions")]
    pub max_parallel_actions: usize,
    #[serde(default = "default_circuit_failure_threshold")]
    pub circuit_failure_threshold: u32,
    #[serde(default = "default_circuit_cooldown_secs")]
    pub circuit_cooldown_secs: u64,
    #[serde(default = "default_selection_receipt_ttl_secs")]
    pub selection_receipt_ttl_secs: u64,
    #[serde(default)]
    pub max_cost_microunits: BTreeMap<String, u64>,
    #[serde(default = "default_discover_ladder")]
    pub discover: Vec<RetrievalRungConfig>,
    #[serde(default = "default_read_ladder")]
    pub read: Vec<RetrievalRungConfig>,
}

impl Default for ProgressiveRetrievalSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            rollout_mode: default_rollout(),
            default_deadline_ms: default_deadline_ms(),
            discovery_attempt_timeout_ms: default_discovery_attempt_timeout_ms(),
            max_attempts: default_max_attempts(),
            max_parallel_actions: default_max_parallel_actions(),
            circuit_failure_threshold: default_circuit_failure_threshold(),
            circuit_cooldown_secs: default_circuit_cooldown_secs(),
            selection_receipt_ttl_secs: default_selection_receipt_ttl_secs(),
            max_cost_microunits: BTreeMap::new(),
            discover: default_discover_ladder(),
            read: default_read_ladder(),
        }
    }
}

impl ProgressiveRetrievalSettings {
    pub fn validate_bounds(&self) -> Result<()> {
        if self.default_deadline_ms == 0
            || self.discovery_attempt_timeout_ms == 0
            || self.max_attempts == 0
            || self.max_attempts > MAX_RETRIEVAL_ACTIONS
            || self.max_parallel_actions == 0
            || self.max_parallel_actions > self.max_attempts
            || self.circuit_failure_threshold == 0
            || self.circuit_cooldown_secs == 0
            || self.selection_receipt_ttl_secs == 0
        {
            bail!("progressive retrieval bounds must be positive and internally consistent");
        }
        if self.max_cost_microunits.len() > MAX_RETRIEVAL_ACTIONS {
            bail!("progressive retrieval has too many cost commodities");
        }
        for (commodity, limit) in &self.max_cost_microunits {
            if commodity.trim().is_empty() || *limit == 0 {
                bail!("progressive retrieval cost limits require a commodity and positive value");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ContentAcquisitionSettings {
    #[serde(default)]
    pub progressive_retrieval: ProgressiveRetrievalSettings,
    #[serde(default)]
    pub browser: BrowserRetrievalSettings,
    #[serde(default)]
    pub observe: super::ObservableSourceSettings,
    #[serde(default)]
    pub working_sets: WorkingSetCaptureSettings,
}

fn default_browser_command_timeout_secs() -> u64 {
    20
}

fn default_browser_render_wait_ms() -> u64 {
    750
}

fn default_browser_capture_bytes() -> usize {
    2 * 1024 * 1024
}

fn default_browser_document_chars() -> usize {
    1024 * 1024
}

fn default_browser_approval_ttl_secs() -> u64 {
    5 * 60
}

fn default_browser_cdp_url() -> String {
    crate::magician_v2::execution::primitive_dispatch::browser::DEFAULT_MAGICUTOR_PROXY_URL
        .to_string()
}

fn default_working_set_auto_capture_agents() -> Vec<String> {
    vec!["web-researcher".to_string()]
}

fn default_true() -> bool {
    true
}

fn default_rollout() -> ProgressiveRetrievalRollout {
    ProgressiveRetrievalRollout::Authoritative
}

fn default_deadline_ms() -> u64 {
    20_000
}

fn default_discovery_attempt_timeout_ms() -> u64 {
    15_000
}

fn default_max_attempts() -> usize {
    8
}

fn default_max_parallel_actions() -> usize {
    2
}

/// Upper bound on the cause text carried on a failed attempt receipt.
const MAX_FAILURE_DETAIL_CHARS: usize = 200;

fn default_circuit_failure_threshold() -> u32 {
    3
}

fn default_circuit_cooldown_secs() -> u64 {
    60
}

fn default_selection_receipt_ttl_secs() -> u64 {
    15 * 60
}

fn rung(
    id: &str,
    mode: RetrievalRungMode,
    actions: &[&str],
    optional_actions: &[&str],
) -> RetrievalRungConfig {
    RetrievalRungConfig {
        id: id.to_string(),
        mode,
        actions: actions.iter().map(|value| (*value).to_string()).collect(),
        optional_actions: optional_actions
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    }
}

fn default_discover_ladder() -> Vec<RetrievalRungConfig> {
    vec![
        rung(
            "source_native",
            RetrievalRungMode::EligibleParallel,
            &["rss.discover"],
            &[],
        ),
        rung("public_search", RetrievalRungMode::Sequential, &[], &[]),
        rung(
            "public_browser_handoff",
            RetrievalRungMode::Sequential,
            &[],
            &["browser.headless.discover_handoff"],
        ),
    ]
}

fn default_read_ladder() -> Vec<RetrievalRungConfig> {
    vec![
        rung("public_static", RetrievalRungMode::Sequential, &[], &[]),
        rung(
            "verified_replay",
            RetrievalRungMode::Sequential,
            &[],
            &["api_replay.read"],
        ),
        rung(
            "public_rendered",
            RetrievalRungMode::Sequential,
            &[],
            &["browser.headless.read"],
        ),
        rung(
            "owner_assisted",
            RetrievalRungMode::Sequential,
            &[],
            &["browser.headed.read_handoff"],
        ),
        rung(
            "authenticated",
            RetrievalRungMode::Sequential,
            &[],
            &["browser.cdp.read"],
        ),
        rung(
            "interaction_handoff",
            RetrievalRungMode::Sequential,
            &[],
            &["browser.cdp.interact_handoff"],
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryEvidenceGoal {
    #[serde(default = "default_one")]
    pub min_candidates: usize,
    #[serde(default = "default_one")]
    pub min_relevant_candidates: usize,
    #[serde(default = "default_one")]
    pub min_independent_sources: usize,
    #[serde(default = "default_relevance_threshold")]
    pub relevance_threshold: f64,
}

fn default_one() -> usize {
    1
}

fn default_relevance_threshold() -> f64 {
    0.15
}

impl Default for DiscoveryEvidenceGoal {
    fn default() -> Self {
        Self {
            min_candidates: 1,
            min_relevant_candidates: 1,
            min_independent_sources: 1,
            relevance_threshold: default_relevance_threshold(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadEvidenceGoal {
    pub depth: ReadDepth,
    /// Defaults to the output implied by `depth`. Structured extraction is
    /// explicit so an ordinary article read does not invoke schema readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<super::RetrievalOutputKind>,
    #[serde(default)]
    pub required_metadata: Vec<String>,
    #[serde(default)]
    pub min_chars: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceGoal {
    Discovery(DiscoveryEvidenceGoal),
    Read(ReadEvidenceGoal),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetrievalTarget {
    Query {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        #[serde(default)]
        targets: Vec<String>,
        #[serde(default)]
        inline_candidates: Vec<ContentCandidate>,
        #[serde(default = "default_query_limit")]
        limit: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default)]
        options: BTreeMap<String, Value>,
    },
    Candidate {
        candidate: ContentCandidate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection_receipt: Option<String>,
    },
}

fn default_query_limit() -> usize {
    10
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalNeed {
    pub schema_version: u32,
    pub principal: String,
    pub workspace: String,
    pub operation: RetrievalOperation,
    pub target: RetrievalTarget,
    pub goal: EvidenceGoal,
    pub freshness: FreshnessPolicy,
    pub remote_query_policy: RemoteDataPolicy,
    pub remote_content_policy: RemoteDataPolicy,
    pub invocation_source: ContentInvocationSource,
    pub maximum_authority: RetrievalAuthority,
    /// Opaque process-issued grant for an authenticated deterministic read.
    /// Merely raising `maximum_authority` never grants identity-bearing access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<usize>,
    #[serde(default)]
    pub cost_budget_microunits: BTreeMap<String, u64>,
    /// Empty means the configured ladder. A non-empty list can only narrow it.
    #[serde(default)]
    pub allowed_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalAuthorityGrant {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub authority: RetrievalAuthority,
    pub domain: String,
    pub action_id: String,
    pub private_content_to_assistant: bool,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalHandoffKind {
    PublicHeadlessNavigation,
    OwnerAssistedHeaded,
    AuthenticatedReadApproval,
    AuthenticatedInteractionApproval,
    AuthenticatedInteraction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalHandoff {
    pub id: String,
    pub kind: RetrievalHandoffKind,
    pub browser_session_id: String,
    pub requested_mode: String,
    pub action_id: String,
    pub required_authority: RetrievalAuthority,
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub requires_approval: bool,
    pub expires_at_ms: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("retrieval requires a browser handoff")]
pub struct RetrievalHandoffRequired {
    pub handoff: RetrievalHandoff,
}

#[derive(Debug, Clone, Default)]
pub struct RetrievalTransportTrace {
    pub actual_transport: Option<String>,
    pub requested_browser_mode: Option<String>,
    pub resolved_browser_mode: Option<String>,
    pub browser_engine: Option<String>,
    pub browser_session_fingerprint: Option<String>,
    pub session_outcome: Option<String>,
    pub render_ms: Option<u64>,
    pub extract_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RetrievalTransportFailure {
    pub message: String,
    pub trace: RetrievalTransportTrace,
}

impl RetrievalNeed {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != RETRIEVAL_NEED_SCHEMA_VERSION {
            bail!(
                "unsupported retrieval need schema version {}",
                self.schema_version
            );
        }
        super::types::validate_content_scope_component(&self.principal, "retrieval principal")?;
        super::types::validate_content_scope_component(&self.workspace, "retrieval workspace")?;
        if self.deadline_ms == Some(0) || self.max_attempts == Some(0) {
            bail!("retrieval deadline and attempt bound must be positive when present");
        }
        if self
            .authority_grant_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty() || id.len() > 128)
        {
            bail!("retrieval authority grant id is invalid");
        }
        if self.allowed_actions.len() > MAX_RETRIEVAL_ACTIONS {
            bail!("retrieval allowed_actions exceeds {MAX_RETRIEVAL_ACTIONS}");
        }
        let unique = self.allowed_actions.iter().collect::<BTreeSet<_>>();
        if unique.len() != self.allowed_actions.len() {
            bail!("retrieval allowed_actions contains duplicates");
        }
        if self.cost_budget_microunits.len() > MAX_RETRIEVAL_ACTIONS
            || self
                .cost_budget_microunits
                .iter()
                .any(|(commodity, limit)| commodity.trim().is_empty() || *limit == 0)
        {
            bail!("retrieval cost budgets require unique commodities and positive values");
        }
        match (&self.operation, &self.target, &self.goal) {
            (
                RetrievalOperation::Discover,
                RetrievalTarget::Query { query, limit, .. },
                EvidenceGoal::Discovery(goal),
            ) => {
                if query.trim().is_empty() || *limit == 0 || *limit > super::MAX_DISCOVERY_ITEMS {
                    bail!("discovery retrieval requires a query and a valid result limit");
                }
                if goal.min_candidates == 0
                    || goal.min_candidates > *limit
                    || goal.min_relevant_candidates > goal.min_candidates
                    || goal.min_independent_sources == 0
                    || goal.min_independent_sources > goal.min_candidates
                    || !goal.relevance_threshold.is_finite()
                    || !(0.0..=1.0).contains(&goal.relevance_threshold)
                {
                    bail!("discovery evidence goal is invalid for the requested limit");
                }
            },
            (
                RetrievalOperation::Read,
                RetrievalTarget::Candidate { candidate, .. },
                EvidenceGoal::Read(goal),
            ) => {
                candidate.validate()?;
                if matches!(
                    goal.output,
                    Some(
                        super::RetrievalOutputKind::Candidates
                            | super::RetrievalOutputKind::Handoff
                    )
                ) || matches!(
                    (goal.depth, goal.output),
                    (ReadDepth::Gist, Some(super::RetrievalOutputKind::FullText))
                        | (ReadDepth::FullText, Some(super::RetrievalOutputKind::Gist))
                ) {
                    bail!("read evidence output is invalid or inconsistent with depth");
                }
                if goal.required_metadata.len() > MAX_REQUIRED_METADATA_KEYS
                    || goal
                        .required_metadata
                        .iter()
                        .any(|key| key.trim().is_empty())
                    || goal.min_chars == Some(0)
                {
                    bail!("read evidence goal is invalid");
                }
            },
            _ => bail!("retrieval operation, target, and evidence goal do not match"),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalClassification {
    Sufficient,
    HandoffRequired,
    InsufficientQuality,
    NotFound,
    RateLimited,
    ProviderUnavailable,
    /// The provider is reachable but the operator's configuration prevents a
    /// usable call — a broken trust store, a missing CA bundle, an expired or
    /// untrusted certificate. Distinct from `ProviderUnavailable` because the
    /// condition is not transient: it stays broken until an operator fixes it,
    /// so retrying only spends the ladder's attempt budget.
    ProviderMisconfigured,
    JavascriptRequired,
    AuthenticationRequired,
    PolicyDenied,
    InvalidRequest,
    BudgetExhausted,
    DeadlineExceeded,
    Cancelled,
    CircuitOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStatus {
    Complete,
    Degraded,
    HandoffRequired,
    ApprovalRequired,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalSelectionReceipt {
    pub id: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievedCandidate {
    pub candidate: ContentCandidate,
    pub relevance_score: f64,
    pub selection_receipt: RetrievalSelectionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalQualityFinding {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalAttemptReceipt {
    pub fingerprint: String,
    pub rung_id: String,
    pub action_id: String,
    pub configured_order: usize,
    pub actual_order: usize,
    pub classification: RetrievalClassification,
    pub authority: RetrievalAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<ContentPrivacy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_reason: Option<String>,
    /// Bounded cause text for a failed attempt. The classification alone cannot
    /// distinguish a provider outage from a broken trust store, which is what
    /// made a zero-item run cost a full investigation to explain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_browser_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_browser_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_session_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract_ms: Option<u64>,
    pub latency_ms: u64,
    pub returned_items: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AdapterCost>,
    #[serde(default)]
    pub quality_findings: Vec<RetrievalQualityFinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub trace_id: String,
    pub status: RetrievalStatus,
    pub operation: RetrievalOperation,
    #[serde(default)]
    pub candidates: Vec<RetrievedCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ContentDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<RetrievalHandoff>,
    #[serde(default)]
    pub attempts: Vec<RetrievalAttemptReceipt>,
    #[serde(default)]
    pub unavailable_optional_actions: Vec<String>,
    #[serde(default)]
    pub unmet_goal_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_authority: Option<RetrievalAuthority>,
    #[serde(default)]
    pub total_cost_microunits: BTreeMap<String, u64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
struct StoredSelectionReceipt {
    principal: String,
    workspace: String,
    capability_revision: String,
    candidate_fingerprint: String,
    reason: ReadSelectionReason,
    relevance_score: f64,
    selected_at_ms: i64,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
struct CircuitState {
    failures: u32,
    opened_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct RetrievalHandoffSessionGrant {
    principal: String,
    workspace: String,
    action_id: String,
    target_domain: Option<String>,
    expected_connection_mode: String,
    expires_at_ms: i64,
}

#[derive(Debug, Clone)]
struct StoredAuthorityGrant {
    grant: RetrievalAuthorityGrant,
    claimed_by: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct AuthorityGrantClaim {
    pub expires_at_ms: i64,
    pub private_content_to_assistant: bool,
}

#[derive(Debug)]
pub struct RetrievalRuntimeState {
    receipts: Mutex<BTreeMap<String, StoredSelectionReceipt>>,
    circuits: Mutex<BTreeMap<String, CircuitState>>,
    authority_grants: Mutex<BTreeMap<String, StoredAuthorityGrant>>,
    handoff_sessions: Mutex<BTreeMap<String, RetrievalHandoffSessionGrant>>,
}

impl Default for RetrievalRuntimeState {
    fn default() -> Self {
        Self {
            receipts: Mutex::new(BTreeMap::new()),
            circuits: Mutex::new(BTreeMap::new()),
            authority_grants: Mutex::new(BTreeMap::new()),
            handoff_sessions: Mutex::new(BTreeMap::new()),
        }
    }
}

static GLOBAL_RETRIEVAL_RUNTIME_STATE: OnceLock<Arc<RetrievalRuntimeState>> = OnceLock::new();

pub fn global_retrieval_runtime_state() -> Arc<RetrievalRuntimeState> {
    GLOBAL_RETRIEVAL_RUNTIME_STATE
        .get_or_init(|| Arc::new(RetrievalRuntimeState::default()))
        .clone()
}

impl RetrievalRuntimeState {
    pub fn issue_authority_grant(
        &self,
        principal: &str,
        workspace: &str,
        authority: RetrievalAuthority,
        domain: &str,
        action_id: &str,
        private_content_to_assistant: bool,
        ttl: Duration,
    ) -> Result<RetrievalAuthorityGrant> {
        super::types::validate_content_scope_component(principal, "grant principal")?;
        super::types::validate_content_scope_component(workspace, "grant workspace")?;
        let domain = normalize_grant_domain(domain)?;
        if action_id.trim().is_empty() || action_id.len() > super::MAX_ADAPTER_ID_CHARS {
            bail!("authority grant action id is invalid");
        }
        let issued_at_ms = Utc::now().timestamp_millis();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let grant = RetrievalAuthorityGrant {
            id: format!("rag_{}", uuid::Uuid::new_v4().simple()),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            authority,
            domain,
            action_id: action_id.to_string(),
            private_content_to_assistant,
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(ttl_ms),
        };
        let mut grants = self
            .authority_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        grants.retain(|_, existing| existing.grant.expires_at_ms > issued_at_ms);
        if grants.len() >= MAX_AUTHORITY_GRANTS {
            if let Some(oldest) = grants
                .values()
                .min_by_key(|existing| existing.grant.expires_at_ms)
                .map(|existing| existing.grant.id.clone())
            {
                grants.remove(&oldest);
            }
        }
        grants.insert(
            grant.id.clone(),
            StoredAuthorityGrant {
                grant: grant.clone(),
                claimed_by: None,
            },
        );
        Ok(grant)
    }

    fn revoke_authority_grant(&self, grant_id: &str) -> bool {
        self.authority_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(grant_id)
            .is_some()
    }

    pub fn authority_grant_allows(
        &self,
        grant_id: Option<&str>,
        principal: &str,
        workspace: &str,
        required: RetrievalAuthority,
        action_id: &str,
        target_url: &str,
    ) -> bool {
        let Some(grant_id) = grant_id else {
            return false;
        };
        let now_ms = Utc::now().timestamp_millis();
        let target_domain = url::Url::parse(target_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
        let mut grants = self
            .authority_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        grants.retain(|_, stored| stored.grant.expires_at_ms > now_ms);
        let Some(stored) = grants.get(grant_id) else {
            return false;
        };
        if stored.claimed_by.is_some() {
            return false;
        }
        authority_grant_matches(
            &stored.grant,
            principal,
            workspace,
            required,
            action_id,
            target_domain.as_deref(),
        )
    }

    pub fn claim_authority_grant(
        &self,
        grant_id: Option<&str>,
        principal: &str,
        workspace: &str,
        required: RetrievalAuthority,
        action_id: &str,
        target_url: &str,
        claim_id: &str,
    ) -> Option<AuthorityGrantClaim> {
        let Some(grant_id) = grant_id else {
            return None;
        };
        if claim_id.trim().is_empty() {
            return None;
        }
        let now_ms = Utc::now().timestamp_millis();
        let target_domain = url::Url::parse(target_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
        let mut grants = self
            .authority_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        grants.retain(|_, stored| stored.grant.expires_at_ms > now_ms);
        let Some(stored) = grants.get_mut(grant_id) else {
            return None;
        };
        if !authority_grant_matches(
            &stored.grant,
            principal,
            workspace,
            required,
            action_id,
            target_domain.as_deref(),
        ) {
            return None;
        }
        match stored.claimed_by.as_deref() {
            Some(existing) if existing == claim_id => Some(AuthorityGrantClaim {
                expires_at_ms: stored.grant.expires_at_ms,
                private_content_to_assistant: stored.grant.private_content_to_assistant,
            }),
            Some(_) => None,
            None => {
                stored.claimed_by = Some(claim_id.to_string());
                Some(AuthorityGrantClaim {
                    expires_at_ms: stored.grant.expires_at_ms,
                    private_content_to_assistant: stored.grant.private_content_to_assistant,
                })
            },
        }
    }

    pub fn register_handoff_session(
        &self,
        handoff: &RetrievalHandoff,
        grant_id: Option<&str>,
    ) -> Result<()> {
        let authenticated = matches!(
            handoff.required_authority,
            RetrievalAuthority::AuthenticatedRead | RetrievalAuthority::AuthenticatedInteract
        );
        let (target_domain, authority_expires_at_ms) = if authenticated {
            let target_url = handoff
                .target_url
                .as_deref()
                .ok_or_else(|| anyhow!("authenticated browser handoff requires a target URL"))?;
            let claim = self
                .claim_authority_grant(
                    grant_id,
                    &handoff.principal,
                    &handoff.workspace,
                    handoff.required_authority,
                    &handoff.action_id,
                    target_url,
                    &handoff.browser_session_id,
                )
                .ok_or_else(|| {
                    anyhow!(
                        "authenticated browser handoff approval is absent, stale, or already used"
                    )
                })?;
            if !claim.private_content_to_assistant {
                bail!(
                    "authenticated browser handoff cannot expose private page content to the \
                     assistant without explicit approval"
                );
            }
            let target_domain = url::Url::parse(target_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                .ok_or_else(|| anyhow!("authenticated browser handoff target requires a domain"))?;
            (Some(target_domain), Some(claim.expires_at_ms))
        } else {
            if handoff.required_authority != RetrievalAuthority::PublicBrowserInteract
                || grant_id.is_some()
            {
                bail!("public browser handoff has an invalid authority contract");
            }
            (None, None)
        };
        let now_ms = Utc::now().timestamp_millis();
        let expected_connection_mode = match handoff.requested_mode.as_str() {
            "cdp" => "cdp",
            "headed" => "headed",
            "headless" => "headless",
            other => bail!("unsupported retrieval handoff mode `{other}`"),
        };
        if authenticated != (expected_connection_mode == "cdp") {
            bail!("retrieval handoff mode does not match its authority");
        }
        let expected_session_prefix = format!("retrieval-{expected_connection_mode}-rh_");
        if !handoff
            .browser_session_id
            .starts_with(&expected_session_prefix)
        {
            bail!("retrieval handoff session id does not match its mode");
        }
        let mut sessions = self
            .handoff_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at_ms > now_ms);
        if sessions.len() >= MAX_AUTHORITY_GRANTS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.expires_at_ms)
                .map(|(id, _)| id.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            handoff.browser_session_id.clone(),
            RetrievalHandoffSessionGrant {
                principal: handoff.principal.clone(),
                workspace: handoff.workspace.clone(),
                action_id: handoff.action_id.clone(),
                target_domain,
                expected_connection_mode: expected_connection_mode.into(),
                expires_at_ms: authority_expires_at_ms.map_or(handoff.expires_at_ms, |expires| {
                    expires.min(handoff.expires_at_ms)
                }),
            },
        );
        Ok(())
    }

    pub fn handoff_session_allows(
        &self,
        session_id: &str,
        principal: &str,
        workspace: &str,
        connection_mode: &str,
        action_id: Option<&str>,
        target_url: Option<&str>,
    ) -> bool {
        let now_ms = Utc::now().timestamp_millis();
        let mut sessions = self
            .handoff_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at_ms > now_ms);
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        let target_matches = session
            .target_domain
            .as_deref()
            .is_none_or(|approved_domain| {
                target_url.is_none_or(|target_url| {
                    url::Url::parse(target_url)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                        .as_deref()
                        == Some(approved_domain)
                })
            });
        session.principal == principal
            && session.workspace == workspace
            && session.expected_connection_mode == connection_mode
            && action_id.is_some_and(|action_id| action_id == session.action_id)
            && target_matches
    }

    pub fn revoke_handoff_session(&self, session_id: &str) -> bool {
        self.handoff_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
            .is_some()
    }
}

fn authority_grant_matches(
    grant: &RetrievalAuthorityGrant,
    principal: &str,
    workspace: &str,
    required: RetrievalAuthority,
    action_id: &str,
    target_domain: Option<&str>,
) -> bool {
    grant.principal == principal
        && grant.workspace == workspace
        && grant.action_id == action_id
        && target_domain == Some(grant.domain.as_str())
        && authority_allows(grant.authority, required)
}

fn normalize_grant_domain(domain: &str) -> Result<String> {
    let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 253
        || normalized.contains('/')
        || normalized.contains(':')
        || normalized.chars().any(char::is_whitespace)
    {
        bail!("authority grant domain is invalid");
    }
    Ok(normalized)
}

#[derive(Clone)]
pub struct RetrievalLadderController {
    service: Arc<ContentAcquisitionService>,
    settings: ProgressiveRetrievalSettings,
    state: Arc<RetrievalRuntimeState>,
}

impl RetrievalLadderController {
    pub fn new(
        service: Arc<ContentAcquisitionService>,
        settings: ProgressiveRetrievalSettings,
        state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        Self {
            service,
            settings,
            state,
        }
    }

    pub fn validate_configuration(&self) -> Result<Vec<String>> {
        validate_settings(&self.settings, &self.service.catalog())
    }

    /// Issue an opaque, process-owned approval receipt for one exact
    /// scope/domain/action. Callers must first complete their existing HITL or
    /// standing-authority check; retrieval never creates grants from model
    /// arguments.
    pub fn issue_authority_grant(
        &self,
        authority: RetrievalAuthority,
        domain: &str,
        action_id: &str,
        private_content_to_assistant: bool,
        ttl: Duration,
    ) -> Result<RetrievalAuthorityGrant> {
        if !matches!(
            authority,
            RetrievalAuthority::AuthenticatedRead | RetrievalAuthority::AuthenticatedInteract
        ) {
            bail!("authority grants are reserved for identity-bearing browser work");
        }
        if ttl.is_zero() {
            bail!("authority grant ttl must be positive");
        }
        self.state.issue_authority_grant(
            self.service.principal(),
            self.service.workspace(),
            authority,
            domain,
            action_id,
            private_content_to_assistant,
            ttl,
        )
    }

    pub fn revoke_authority_grant(&self, grant_id: &str) -> bool {
        self.state.revoke_authority_grant(grant_id)
    }

    /// One span per retrieval request — the whole ladder, not one per rung.
    ///
    /// The ladder climbs up to `max_attempts` rungs per request, so a span per
    /// attempt would put four or more rows on the wire for a single
    /// two-request search and answer no question the aggregate does not: the
    /// reader wants "this run went and fetched something, and it took 4s",
    /// and the rung that finally won is already carried by
    /// `RetrievalResult::attempts` in the durable receipt. Per-rung INFO lines
    /// still appear inside this span as progress rows, so the detail is not
    /// lost — it just does not each become a unit of its own.
    ///
    /// `need` carries the query and the evidence goal, both user content, so
    /// only the operation and scope are named.
    #[instrument(
        name = "content_retrieval",
        skip_all,
        fields(
            activity_kind = KIND_AGENT,
            principal = %need.principal,
            workspace = %need.workspace,
            operation = ?need.operation,
        )
    )]
    pub async fn retrieve(
        &self,
        need: RetrievalNeed,
        cancellation: CancellationToken,
    ) -> Result<RetrievalResult> {
        need.validate()?;
        if need.principal != self.service.principal() || need.workspace != self.service.workspace()
        {
            bail!("retrieval need scope does not match the bound acquisition service");
        }
        if !self.settings.enabled || self.settings.rollout_mode == ProgressiveRetrievalRollout::Off
        {
            bail!("progressive content retrieval is disabled");
        }
        let optional_unavailable = self.validate_configuration()?;
        validate_allowed_actions(&need, &self.service.catalog(), &self.settings)?;
        let started = Instant::now();
        let deadline = Duration::from_millis(
            need.deadline_ms
                .unwrap_or(self.settings.default_deadline_ms)
                .min(self.settings.default_deadline_ms),
        );
        let max_attempts = need
            .max_attempts
            .unwrap_or(self.settings.max_attempts)
            .min(self.settings.max_attempts);
        let trace_id = uuid::Uuid::new_v4().to_string();
        let mut result = RetrievalResult {
            trace_id: trace_id.clone(),
            status: RetrievalStatus::Failed,
            operation: need.operation,
            candidates: Vec::new(),
            document: None,
            handoff: None,
            attempts: Vec::new(),
            unavailable_optional_actions: optional_unavailable,
            unmet_goal_reasons: Vec::new(),
            required_authority: None,
            total_cost_microunits: BTreeMap::new(),
            duration_ms: 0,
        };

        // Each arm is heap-owned. Awaited inline, this one frame reserved room for
        // both ladders' state machines even though a need runs exactly one of them.
        match need.operation {
            RetrievalOperation::Discover => {
                Box::pin(self.retrieve_discovery(
                    &need,
                    &trace_id,
                    started,
                    deadline,
                    max_attempts,
                    &cancellation,
                    &mut result,
                ))
                .await?;
            },
            RetrievalOperation::Read => {
                Box::pin(self.retrieve_read(
                    &need,
                    &trace_id,
                    started,
                    deadline,
                    max_attempts,
                    &cancellation,
                    &mut result,
                ))
                .await?;
            },
        }
        result.duration_ms = elapsed_ms(started);
        for attempt in &result.attempts {
            let (cost_commodity, cost_microunits) = attempt
                .cost
                .as_ref()
                .map(|cost| (cost.commodity.as_str(), cost.amount_microunits))
                .unwrap_or(("none", 0));
            tracing::info!(
                retrieval_trace_id = %result.trace_id,
                rung_id = %attempt.rung_id,
                action_id = %attempt.action_id,
                action_fingerprint = %attempt.fingerprint,
                configured_order = attempt.configured_order,
                actual_order = attempt.actual_order,
                classification = ?attempt.classification,
                authority = ?attempt.authority,
                privacy = ?attempt.privacy,
                cache_outcome = attempt.cache_outcome.as_deref().unwrap_or("none"),
                escalation_reason = attempt.escalation_reason.as_deref().unwrap_or("none"),
                // Empty, not "none": this is present only on a failed attempt,
                // and a successful line should carry no cause text at all.
                failure_detail = attempt.failure_detail.as_deref().unwrap_or(""),
                actual_transport = attempt.actual_transport.as_deref().unwrap_or("none"),
                requested_browser_mode = attempt.requested_browser_mode.as_deref().unwrap_or("none"),
                resolved_browser_mode = attempt.resolved_browser_mode.as_deref().unwrap_or("none"),
                browser_engine = attempt.browser_engine.as_deref().unwrap_or("none"),
                browser_session_fingerprint = attempt.browser_session_fingerprint.as_deref().unwrap_or("none"),
                session_outcome = attempt.session_outcome.as_deref().unwrap_or("none"),
                approval_receipt_id = attempt.approval_receipt_id.as_deref().unwrap_or("none"),
                render_ms = attempt.render_ms.unwrap_or_default(),
                extract_ms = attempt.extract_ms.unwrap_or_default(),
                latency_ms = attempt.latency_ms,
                returned_items = attempt.returned_items,
                cost_commodity,
                cost_microunits,
                quality_findings = attempt.quality_findings.len(),
                "content retrieval attempt completed"
            );
        }
        tracing::info!(
            retrieval_trace_id = %result.trace_id,
            principal = %need.principal,
            workspace = %need.workspace,
            operation = ?result.operation,
            status = ?result.status,
            attempts = result.attempts.len(),
            candidates = result.candidates.len(),
            has_document = result.document.is_some(),
            has_handoff = result.handoff.is_some(),
            duration_ms = result.duration_ms,
            total_cost_microunits = ?result.total_cost_microunits,
            "content retrieval completed"
        );
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn retrieve_discovery(
        &self,
        need: &RetrievalNeed,
        trace_id: &str,
        started: Instant,
        deadline: Duration,
        max_attempts: usize,
        cancellation: &CancellationToken,
        result: &mut RetrievalResult,
    ) -> Result<()> {
        let RetrievalTarget::Query {
            query,
            intent,
            targets,
            inline_candidates,
            limit,
            cursor,
            options,
        } = &need.target
        else {
            unreachable!()
        };
        let EvidenceGoal::Discovery(goal) = &need.goal else {
            unreachable!()
        };
        let mut evidence = merge_candidates(Vec::new(), inline_candidates.clone(), *limit);
        if discovery_goal_satisfied(&evidence, query, goal) {
            result.status = RetrievalStatus::Complete;
            result.candidates = self.issue_candidate_receipts(need, query, evidence);
            return Ok(());
        }

        let catalog = self.service.catalog();
        let descriptors = catalog
            .discovery
            .into_iter()
            .map(|descriptor| (descriptor.retrieval.action_id.clone(), descriptor))
            .collect::<BTreeMap<_, _>>();
        let mut actual_order = result.attempts.len();
        let mut executed = 0usize;

        'rungs: for rung in &self.settings.discover {
            if cancellation.is_cancelled() {
                result.status = RetrievalStatus::Cancelled;
                result.unmet_goal_reasons.push("retrieval cancelled".into());
                result.candidates = self.issue_candidate_receipts(need, query, evidence);
                return Ok(());
            }
            if started.elapsed() >= deadline {
                result.status = degraded_status(!evidence.is_empty());
                result
                    .unmet_goal_reasons
                    .push("retrieval deadline exceeded".into());
                break;
            }
            let mut actions = Vec::new();
            for (configured_order, action_id) in effective_action_ids(rung, descriptors.values())?
                .into_iter()
                .enumerate()
            {
                let Some(descriptor) = descriptors.get(&action_id).cloned() else {
                    continue;
                };
                if !action_allowed(need, &descriptor) {
                    continue;
                }
                actions.push((configured_order, descriptor));
            }
            if actions.is_empty() {
                continue;
            }

            let request = DiscoveryRequest {
                principal: need.principal.clone(),
                workspace: need.workspace.clone(),
                intent: intent.clone(),
                query: Some(query.clone()),
                targets: targets.clone(),
                cursor: cursor.clone(),
                validators: BTreeMap::new(),
                limit: *limit,
                freshness: need.freshness,
                remote_query_policy: need.remote_query_policy,
                invocation_source: need.invocation_source,
                options: options.clone(),
            };

            let batch_size = if rung.mode == RetrievalRungMode::EligibleParallel {
                self.settings.max_parallel_actions.max(1)
            } else {
                1
            };
            for batch in actions.chunks(batch_size) {
                if executed >= max_attempts {
                    break 'rungs;
                }
                let mut projected_spend = result.total_cost_microunits.clone();
                let remaining_attempts = max_attempts.saturating_sub(executed);
                let mut admitted = Vec::new();
                for (configured_order, descriptor) in batch.iter().cloned() {
                    if admitted.len() >= remaining_attempts {
                        break;
                    }
                    let classification = if self.circuit_open(&descriptor.retrieval.action_id) {
                        Some(RetrievalClassification::CircuitOpen)
                    } else if !reserve_estimated_cost(
                        need,
                        &self.settings,
                        &descriptor,
                        &mut projected_spend,
                    ) {
                        Some(RetrievalClassification::BudgetExhausted)
                    } else {
                        None
                    };
                    if let Some(classification) = classification {
                        let mut receipt = attempt(
                            trace_id,
                            rung,
                            &descriptor,
                            configured_order,
                            classification,
                            0,
                        );
                        receipt.actual_order = actual_order;
                        actual_order += 1;
                        result.attempts.push(receipt);
                    } else {
                        admitted.push((configured_order, descriptor));
                    }
                }
                if admitted.is_empty() {
                    continue;
                }
                let mut outcomes: Vec<(usize, DiscoveryOutcome)> =
                    stream::iter(admitted.into_iter().map(|(order, descriptor)| {
                        let mut action_request = request.clone();
                        action_request
                            .options
                            .retain(|key, _| descriptor.retrieval.accepted_options.contains(key));
                        if !descriptor.retrieval.accepts_targets {
                            action_request.targets.clear();
                        }
                        // Heap-owned: `buffer_unordered` polls these inside this
                        // frame, so an inline branch nests one whole adapter call
                        // per concurrent slot.
                        Box::pin(async move {
                            let outcome = self
                                .execute_discovery(
                                    rung,
                                    order,
                                    descriptor,
                                    action_request,
                                    trace_id,
                                    started,
                                    deadline,
                                    cancellation,
                                )
                                .await;
                            (order, outcome)
                        })
                    }))
                    .buffer_unordered(batch_size)
                    .collect::<Vec<_>>()
                    .await;
                for (_, outcome) in &mut outcomes {
                    outcome.receipt.actual_order = actual_order;
                    actual_order += 1;
                }
                outcomes.sort_by_key(|(order, _)| *order);

                let mut terminal = None;
                for (_, mut outcome) in outcomes {
                    executed += 1;
                    if outcome.receipt.classification == RetrievalClassification::Sufficient {
                        let relevant = outcome
                            .items
                            .iter()
                            .filter(|candidate| {
                                candidate_relevance(candidate, query) >= goal.relevance_threshold
                            })
                            .count();
                        if relevant == 0 {
                            outcome.receipt.classification =
                                RetrievalClassification::InsufficientQuality;
                            outcome.receipt.quality_findings.push(finding(
                                "irrelevant_candidates",
                                "action returned no candidate above the requested relevance \
                                 threshold",
                            ));
                        }
                    }
                    if let Some(cost) = outcome.receipt.cost.as_ref() {
                        let total = result
                            .total_cost_microunits
                            .entry(cost.commodity.clone())
                            .or_default();
                        *total = total.saturating_add(cost.amount_microunits);
                    }
                    evidence = merge_candidates(evidence, outcome.items, *limit);
                    let classification = outcome.receipt.classification;
                    if terminal.is_none()
                        && matches!(
                            classification,
                            RetrievalClassification::HandoffRequired
                                | RetrievalClassification::PolicyDenied
                                | RetrievalClassification::InvalidRequest
                                | RetrievalClassification::AuthenticationRequired
                                | RetrievalClassification::Cancelled
                                | RetrievalClassification::DeadlineExceeded
                        )
                    {
                        terminal =
                            Some((classification, outcome.required_authority, outcome.handoff));
                    }
                    result.attempts.push(outcome.receipt);
                }

                if budget_exhausted(need, &self.settings, &result.total_cost_microunits) {
                    result.status = degraded_status(!evidence.is_empty());
                    result
                        .unmet_goal_reasons
                        .push("retrieval spend budget exhausted".into());
                    result.candidates = self.issue_candidate_receipts(need, query, evidence);
                    return Ok(());
                }
                if let Some((classification, authority, handoff)) = terminal {
                    match classification {
                        RetrievalClassification::HandoffRequired => {
                            let requires_approval = handoff
                                .as_ref()
                                .is_some_and(|handoff| handoff.requires_approval);
                            result.status = if requires_approval {
                                RetrievalStatus::ApprovalRequired
                            } else {
                                RetrievalStatus::HandoffRequired
                            };
                            result.required_authority = authority;
                            result.handoff = handoff;
                            result
                                .unmet_goal_reasons
                                .push("browser agent handoff required".into());
                        },
                        RetrievalClassification::PolicyDenied
                        | RetrievalClassification::InvalidRequest => {
                            result.status = RetrievalStatus::Failed;
                            result.unmet_goal_reasons.push(format!(
                                "{} stopped fallback",
                                classification_name(classification)
                            ));
                        },
                        RetrievalClassification::AuthenticationRequired => {
                            result.status = RetrievalStatus::ApprovalRequired;
                            result.required_authority = authority;
                            result
                                .unmet_goal_reasons
                                .push("additional authority required".into());
                        },
                        RetrievalClassification::Cancelled => {
                            result.status = RetrievalStatus::Cancelled;
                            result.unmet_goal_reasons.push("retrieval cancelled".into());
                        },
                        RetrievalClassification::DeadlineExceeded => {
                            result.status = degraded_status(!evidence.is_empty());
                            result
                                .unmet_goal_reasons
                                .push("retrieval deadline exceeded".into());
                        },
                        _ => unreachable!(),
                    }
                    result.candidates = self.issue_candidate_receipts(need, query, evidence);
                    return Ok(());
                }
                if discovery_goal_satisfied(&evidence, query, goal) {
                    result.status = RetrievalStatus::Complete;
                    result.candidates = self.issue_candidate_receipts(need, query, evidence);
                    return Ok(());
                }
                if executed >= max_attempts {
                    break 'rungs;
                }
            }
        }
        result.status = degraded_status(!evidence.is_empty());
        result.unmet_goal_reasons = discovery_unmet_reasons(&evidence, query, goal);
        if result
            .attempts
            .iter()
            .any(|attempt| attempt.classification == RetrievalClassification::BudgetExhausted)
        {
            result
                .unmet_goal_reasons
                .push("one or more configured actions exceeded the retrieval spend budget".into());
        }
        if result
            .attempts
            .iter()
            .any(|attempt| attempt.classification == RetrievalClassification::CircuitOpen)
        {
            result
                .unmet_goal_reasons
                .push("one or more provider circuits were open".into());
        }
        if executed >= max_attempts {
            result
                .unmet_goal_reasons
                .push("retrieval attempt bound exhausted".into());
        }
        result.candidates = self.issue_candidate_receipts(need, query, evidence);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn retrieve_read(
        &self,
        need: &RetrievalNeed,
        trace_id: &str,
        started: Instant,
        deadline: Duration,
        max_attempts: usize,
        cancellation: &CancellationToken,
        result: &mut RetrievalResult,
    ) -> Result<()> {
        let RetrievalTarget::Candidate {
            candidate,
            selection_receipt,
        } = &need.target
        else {
            unreachable!()
        };
        let EvidenceGoal::Read(goal) = &need.goal else {
            unreachable!()
        };
        let selection_check =
            self.selection_evidence(need, candidate, selection_receipt.as_deref());
        let inline_text_trusted = selection_receipt.is_some() && selection_check.is_ok();
        let selection = match selection_check {
            Ok(selection) => selection,
            Err(error)
                if need.invocation_source == ContentInvocationSource::InteractiveRead
                    && candidate.canonical_url.is_some() =>
            {
                // An interactive caller may always request a public URL
                // directly. A stale, replayed, or candidate-mismatched receipt
                // therefore disables the inline shortcut instead of blocking
                // the safe reader path. Non-interactive consumers still fail
                // closed because the receipt proves their selection authority.
                tracing::warn!(
                    trace_id,
                    error = %error,
                    "selection receipt rejected; ignoring inline candidate text and reading URL"
                );
                Some(ReadSelectionEvidence {
                    selected_at_ms: Utc::now().timestamp_millis(),
                    reason: ReadSelectionReason::UserRequested,
                    relevance_score: None,
                })
            },
            Err(error) => return Err(error),
        };
        let sanitized_candidate = (!inline_text_trusted).then(|| {
            let mut sanitized = candidate.clone();
            sanitized.cheap_text = sanitized.title.clone();
            sanitized.content_hash = None;
            sanitized
        });
        let candidate = sanitized_candidate.as_ref().unwrap_or(candidate);

        if inline_text_trusted && effective_read_output(goal) == super::RetrievalOutputKind::Gist {
            let inline = inline_document(candidate);
            let quality = evaluate_document(&inline, goal);
            if quality.sufficient {
                result.status = RetrievalStatus::Complete;
                result.document = Some(inline);
                return Ok(());
            }
        }

        let catalog = self.service.catalog();
        let descriptors = catalog
            .readers
            .into_iter()
            .map(|descriptor| (descriptor.retrieval.action_id.clone(), descriptor))
            .collect::<BTreeMap<_, _>>();
        let mut best: Option<(ContentDocument, usize)> = None;
        let mut executed = 0usize;
        let mut identity_required = false;
        for rung in &self.settings.read {
            for (configured_order, action_id) in effective_action_ids(rung, descriptors.values())?
                .into_iter()
                .enumerate()
            {
                let Some(descriptor) = descriptors.get(&action_id).cloned() else {
                    continue;
                };
                if !action_allowed(need, &descriptor) {
                    continue;
                }
                if identity_required && !requires_identity_grant(&descriptor) {
                    continue;
                }
                if self.circuit_open(&descriptor.retrieval.action_id) {
                    let mut receipt = attempt(
                        trace_id,
                        rung,
                        &descriptor,
                        configured_order,
                        RetrievalClassification::CircuitOpen,
                        0,
                    );
                    receipt.actual_order = result.attempts.len();
                    result.attempts.push(receipt);
                    continue;
                }
                if cancellation.is_cancelled() {
                    result.status = RetrievalStatus::Cancelled;
                    result.unmet_goal_reasons.push("retrieval cancelled".into());
                    result.document = best.map(|(document, _)| document);
                    return Ok(());
                }
                if started.elapsed() >= deadline || executed >= max_attempts {
                    result.status = degraded_status(best.is_some());
                    result.unmet_goal_reasons.push(if executed >= max_attempts {
                        "retrieval attempt bound exhausted".into()
                    } else {
                        "retrieval deadline exceeded".into()
                    });
                    result.document = best.map(|(document, _)| document);
                    return Ok(());
                }
                let request = ReadRequest {
                    principal: need.principal.clone(),
                    workspace: need.workspace.clone(),
                    candidate: candidate.clone(),
                    depth: goal.depth,
                    freshness: need.freshness,
                    remote_content_policy: need.remote_content_policy,
                    invocation_source: need.invocation_source,
                    selection: selection.clone(),
                    authority_grant_id: need.authority_grant_id.clone(),
                };
                if requires_identity_grant(&descriptor)
                    && !self.state.authority_grant_allows(
                        need.authority_grant_id.as_deref(),
                        &need.principal,
                        &need.workspace,
                        descriptor.retrieval.authority,
                        &descriptor.retrieval.action_id,
                        candidate.canonical_url.as_deref().unwrap_or_default(),
                    )
                {
                    let (kind, requested_mode) = match descriptor.retrieval.authority {
                        RetrievalAuthority::AuthenticatedInteract => (
                            RetrievalHandoffKind::AuthenticatedInteractionApproval,
                            "cdp",
                        ),
                        _ => (RetrievalHandoffKind::AuthenticatedReadApproval, "cdp"),
                    };
                    let handoff = new_read_handoff(
                        need,
                        candidate,
                        kind,
                        requested_mode,
                        &descriptor.retrieval.action_id,
                        descriptor.retrieval.authority,
                        true,
                    );
                    let mut receipt = attempt(
                        trace_id,
                        rung,
                        &descriptor,
                        configured_order,
                        RetrievalClassification::AuthenticationRequired,
                        0,
                    );
                    receipt.actual_order = result.attempts.len();
                    receipt.approval_receipt_id = need.authority_grant_id.clone();
                    result.attempts.push(receipt);
                    result.status = RetrievalStatus::ApprovalRequired;
                    result.required_authority = Some(descriptor.retrieval.authority);
                    result.handoff = Some(handoff);
                    result
                        .unmet_goal_reasons
                        .push("scoped authenticated browser approval is required".into());
                    result.document = best.map(|(document, _)| document);
                    return Ok(());
                }
                let action_started = Instant::now();
                let remaining = deadline.saturating_sub(started.elapsed());
                let read = tokio::select! {
                    _ = cancellation.cancelled() => Err((RetrievalClassification::Cancelled, None, None, None)),
                    result = tokio::time::timeout(
                        remaining,
                        self.service.read(&descriptor.adapter_id, &request),
                    ) => match result {
                        Ok(Ok(document)) => Ok(document),
                        Ok(Err(error)) => Err((
                            classify_read_error(&error, &descriptor),
                            handoff_from_error(&error),
                            transport_trace_from_error(&error),
                            Some(failure_detail(&error)),
                        )),
                        Err(_) => Err((RetrievalClassification::DeadlineExceeded, None, None, None)),
                    },
                };
                executed += 1;
                let actual_order = result.attempts.len();
                match read {
                    Ok(mut document) => {
                        self.record_success(&descriptor.retrieval.action_id);
                        let quality = evaluate_document(&document, goal);
                        let classification = classify_document_quality(&quality);
                        let score = quality.score;
                        let mut receipt = RetrievalAttemptReceipt {
                            fingerprint: action_fingerprint(
                                trace_id,
                                &rung.id,
                                &descriptor.retrieval.action_id,
                                configured_order,
                            ),
                            rung_id: rung.id.clone(),
                            action_id: descriptor.retrieval.action_id.clone(),
                            configured_order,
                            actual_order,
                            classification,
                            authority: descriptor.retrieval.authority,
                            privacy: Some(document.privacy),
                            cache_outcome: document
                                .metadata
                                .get(super::capability_reader::CONTENT_CACHE_OUTCOME_METADATA_KEY)
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            escalation_reason: escalation_reason(classification),
                            failure_detail: None,
                            actual_transport: None,
                            requested_browser_mode: None,
                            resolved_browser_mode: None,
                            browser_engine: None,
                            browser_session_fingerprint: None,
                            session_outcome: None,
                            approval_receipt_id: need.authority_grant_id.clone(),
                            render_ms: None,
                            extract_ms: None,
                            latency_ms: elapsed_ms(action_started),
                            returned_items: 1,
                            cost: None,
                            quality_findings: quality.findings,
                        };
                        project_attempt_transport(&mut document, &mut receipt);
                        result.attempts.push(receipt);
                        if classification == RetrievalClassification::Sufficient {
                            result.status = RetrievalStatus::Complete;
                            result.document = Some(document);
                            return Ok(());
                        }
                        if classification == RetrievalClassification::AuthenticationRequired {
                            if requires_identity_grant(&descriptor) {
                                result.status = RetrievalStatus::ApprovalRequired;
                                result.required_authority =
                                    Some(RetrievalAuthority::AuthenticatedRead);
                                result
                                    .unmet_goal_reasons
                                    .push("content requires authenticated read authority".into());
                                return Ok(());
                            }
                            identity_required = true;
                            result.required_authority = Some(RetrievalAuthority::AuthenticatedRead);
                        } else if best
                            .as_ref()
                            .is_none_or(|(_, best_score)| score > *best_score)
                        {
                            best = Some((document, score));
                        }
                    },
                    Err((classification, handoff, transport_trace, detail)) => {
                        self.record_failure(&descriptor.retrieval.action_id, classification);
                        let mut receipt = RetrievalAttemptReceipt {
                            fingerprint: action_fingerprint(
                                trace_id,
                                &rung.id,
                                &descriptor.retrieval.action_id,
                                configured_order,
                            ),
                            rung_id: rung.id.clone(),
                            action_id: descriptor.retrieval.action_id.clone(),
                            configured_order,
                            actual_order,
                            classification,
                            authority: descriptor.retrieval.authority,
                            privacy: Some(candidate.privacy),
                            cache_outcome: None,
                            escalation_reason: escalation_reason(classification),
                            failure_detail: detail,
                            actual_transport: None,
                            requested_browser_mode: None,
                            resolved_browser_mode: None,
                            browser_engine: None,
                            browser_session_fingerprint: None,
                            session_outcome: None,
                            approval_receipt_id: need.authority_grant_id.clone(),
                            render_ms: None,
                            extract_ms: None,
                            latency_ms: elapsed_ms(action_started),
                            returned_items: 0,
                            cost: None,
                            quality_findings: Vec::new(),
                        };
                        if let Some(handoff) = handoff {
                            receipt.requested_browser_mode = Some(handoff.requested_mode.clone());
                            receipt.resolved_browser_mode = Some(handoff.requested_mode.clone());
                            receipt.actual_transport = Some("browser_handoff".into());
                            receipt.browser_session_fingerprint = Some(
                                blake3::hash(handoff.browser_session_id.as_bytes())
                                    .to_hex()
                                    .to_string(),
                            );
                            receipt.session_outcome = Some("transferred".into());
                            receipt.approval_receipt_id = need.authority_grant_id.clone();
                            result.status = if handoff.requires_approval {
                                RetrievalStatus::ApprovalRequired
                            } else {
                                RetrievalStatus::HandoffRequired
                            };
                            result.required_authority = Some(handoff.required_authority);
                            result.handoff = Some(handoff);
                            result.attempts.push(receipt);
                            result.document = best.map(|(document, _)| document);
                            result
                                .unmet_goal_reasons
                                .push("browser agent handoff required".into());
                            return Ok(());
                        }
                        if let Some(trace) = transport_trace {
                            project_transport_trace(trace, &mut receipt);
                        } else {
                            project_declared_transport_after_abort(&descriptor, &mut receipt);
                        }
                        result.attempts.push(receipt);
                        match classification {
                            RetrievalClassification::PolicyDenied
                            | RetrievalClassification::InvalidRequest => {
                                result.status = RetrievalStatus::Failed;
                                result.unmet_goal_reasons.push(format!(
                                    "{} stopped fallback",
                                    classification_name(classification)
                                ));
                                result.document = best.map(|(document, _)| document);
                                return Ok(());
                            },
                            RetrievalClassification::AuthenticationRequired => {
                                if requires_identity_grant(&descriptor) {
                                    result.status = RetrievalStatus::ApprovalRequired;
                                    result.required_authority =
                                        Some(descriptor.retrieval.authority);
                                    result
                                        .unmet_goal_reasons
                                        .push("additional authority required".into());
                                    result.document = best.map(|(document, _)| document);
                                    return Ok(());
                                }
                                identity_required = true;
                                result.required_authority =
                                    Some(RetrievalAuthority::AuthenticatedRead);
                            },
                            RetrievalClassification::Cancelled => {
                                result.status = RetrievalStatus::Cancelled;
                                result.unmet_goal_reasons.push("retrieval cancelled".into());
                                result.document = best.map(|(document, _)| document);
                                return Ok(());
                            },
                            RetrievalClassification::DeadlineExceeded => {
                                result.status = degraded_status(best.is_some());
                                result
                                    .unmet_goal_reasons
                                    .push("retrieval deadline exceeded".into());
                                result.document = best.map(|(document, _)| document);
                                return Ok(());
                            },
                            _ => {},
                        }
                    },
                }
            }
        }
        result.status = if result.required_authority.is_some() {
            RetrievalStatus::ApprovalRequired
        } else {
            degraded_status(best.is_some())
        };
        result.document = best.map(|(document, _)| document);
        result
            .unmet_goal_reasons
            .push(if result.required_authority.is_some() {
                "content requires authenticated read authority".into()
            } else if result.attempts.iter().any(|attempt| {
                attempt.classification == RetrievalClassification::JavascriptRequired
            }) {
                "public static content requires JavaScript rendering".into()
            } else {
                "no configured reader satisfied the evidence goal".into()
            });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_discovery(
        &self,
        rung: &RetrievalRungConfig,
        configured_order: usize,
        descriptor: ContentSourceDescriptor,
        request: DiscoveryRequest,
        trace_id: &str,
        started: Instant,
        deadline: Duration,
        cancellation: &CancellationToken,
    ) -> DiscoveryOutcome {
        let action_started = Instant::now();
        let remaining = deadline.saturating_sub(started.elapsed());
        let configured_attempt_timeout =
            Duration::from_millis(self.settings.discovery_attempt_timeout_ms);
        let attempt_timeout = remaining.min(configured_attempt_timeout);
        let timeout_classification = if configured_attempt_timeout < remaining {
            RetrievalClassification::ProviderUnavailable
        } else {
            RetrievalClassification::DeadlineExceeded
        };
        let call = tokio::select! {
            _ = cancellation.cancelled() => Err((RetrievalClassification::Cancelled, None, None)),
            result = tokio::time::timeout(
                attempt_timeout,
                Box::pin(self.service.discover(&descriptor.adapter_id, &request)),
            ) => match result {
                Ok(Ok(page)) => Ok(page),
                Ok(Err(error)) => Err((
                    classify_discovery_error(&error, &descriptor),
                    handoff_from_error(&error),
                    Some(failure_detail(&error)),
                )),
                Err(_) => Err((
                    timeout_classification,
                    None,
                    Some(format!(
                        "discovery action timed out after {} ms",
                        attempt_timeout.as_millis()
                    )),
                )),
            },
        };
        match call {
            Ok(page) => {
                self.record_success(&descriptor.retrieval.action_id);
                let classification = if page.items.is_empty() {
                    RetrievalClassification::NotFound
                } else {
                    RetrievalClassification::Sufficient
                };
                let mut receipt = attempt(
                    trace_id,
                    rung,
                    &descriptor,
                    configured_order,
                    classification,
                    elapsed_ms(action_started),
                );
                receipt.returned_items = page.items.len();
                receipt.cost = page.cost;
                receipt.privacy = page
                    .items
                    .iter()
                    .map(|candidate| candidate.privacy)
                    .max_by_key(|privacy| privacy_level(*privacy));
                DiscoveryOutcome {
                    receipt,
                    items: page.items,
                    required_authority: None,
                    handoff: None,
                }
            },
            Err((classification, handoff, detail)) => {
                self.record_failure(&descriptor.retrieval.action_id, classification);
                let mut outcome = DiscoveryOutcome::empty(
                    attempt(
                        trace_id,
                        rung,
                        &descriptor,
                        configured_order,
                        classification,
                        elapsed_ms(action_started),
                    ),
                    (classification == RetrievalClassification::AuthenticationRequired)
                        .then_some(descriptor.retrieval.authority),
                );
                outcome.receipt.failure_detail = detail;
                if let Some(handoff) = handoff {
                    outcome.receipt.actual_transport = Some("browser_handoff".into());
                    outcome.receipt.requested_browser_mode = Some(handoff.requested_mode.clone());
                    outcome.receipt.resolved_browser_mode = Some(handoff.requested_mode.clone());
                    outcome.receipt.browser_session_fingerprint = Some(
                        blake3::hash(handoff.browser_session_id.as_bytes())
                            .to_hex()
                            .to_string(),
                    );
                    outcome.receipt.session_outcome = Some("transferred".into());
                    outcome.required_authority = Some(handoff.required_authority);
                    outcome.handoff = Some(handoff);
                }
                outcome
            },
        }
    }

    fn issue_candidate_receipts(
        &self,
        need: &RetrievalNeed,
        query: &str,
        candidates: Vec<ContentCandidate>,
    ) -> Vec<RetrievedCandidate> {
        let now_ms = Utc::now().timestamp_millis();
        let expires_at_ms = now_ms.saturating_add(
            i64::try_from(self.settings.selection_receipt_ttl_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000),
        );
        let reason = selection_reason(need.invocation_source);
        let mut receipts = self
            .state
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receipts.retain(|_, receipt| receipt.expires_at_ms > now_ms);
        let mut output = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let id = uuid::Uuid::new_v4().to_string();
            let relevance_score = candidate_relevance(&candidate, query);
            receipts.insert(
                id.clone(),
                StoredSelectionReceipt {
                    principal: need.principal.clone(),
                    workspace: need.workspace.clone(),
                    capability_revision: self.service.capability_revision().to_string(),
                    candidate_fingerprint: candidate_fingerprint(&candidate),
                    reason,
                    relevance_score,
                    selected_at_ms: now_ms,
                    expires_at_ms,
                },
            );
            output.push(RetrievedCandidate {
                candidate,
                relevance_score,
                selection_receipt: RetrievalSelectionReceipt { id, expires_at_ms },
            });
        }
        while receipts.len() > MAX_RECEIPTS {
            let Some(oldest) = receipts
                .iter()
                .min_by_key(|(_, receipt)| receipt.expires_at_ms)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            receipts.remove(&oldest);
        }
        output
    }

    fn selection_evidence(
        &self,
        need: &RetrievalNeed,
        candidate: &ContentCandidate,
        receipt_id: Option<&str>,
    ) -> Result<Option<ReadSelectionEvidence>> {
        if need.invocation_source == ContentInvocationSource::InteractiveRead
            && receipt_id.is_none()
        {
            return Ok(Some(ReadSelectionEvidence {
                selected_at_ms: Utc::now().timestamp_millis(),
                reason: ReadSelectionReason::UserRequested,
                relevance_score: None,
            }));
        }
        if need.invocation_source == ContentInvocationSource::InternalSystem && receipt_id.is_none()
        {
            return Ok(None);
        }
        let receipt_id = receipt_id.ok_or_else(|| {
            anyhow::anyhow!("non-interactive retrieval reads require a selection receipt")
        })?;
        let now_ms = Utc::now().timestamp_millis();
        let mut receipts = self
            .state
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let receipt = receipts
            .get(receipt_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("selection receipt is unknown or already consumed"))?;
        if receipt.expires_at_ms <= now_ms
            || receipt.principal != need.principal
            || receipt.workspace != need.workspace
            || receipt.capability_revision != self.service.capability_revision()
            || receipt.candidate_fingerprint != candidate_fingerprint(candidate)
            || receipt.reason != selection_reason(need.invocation_source)
        {
            bail!("selection receipt is expired or does not match this scoped candidate");
        }
        receipts.remove(receipt_id);
        Ok(Some(ReadSelectionEvidence {
            selected_at_ms: receipt.selected_at_ms,
            reason: receipt.reason,
            relevance_score: Some(receipt.relevance_score),
        }))
    }

    fn circuit_open(&self, action_id: &str) -> bool {
        let now_ms = Utc::now().timestamp_millis();
        let cooldown_ms = i64::try_from(self.settings.circuit_cooldown_secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000);
        let mut circuits = self
            .state
            .circuits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(circuit) = circuits.get_mut(action_id) else {
            return false;
        };
        if circuit.failures < self.settings.circuit_failure_threshold {
            return false;
        }
        if circuit
            .opened_at_ms
            .is_some_and(|opened| now_ms.saturating_sub(opened) < cooldown_ms)
        {
            true
        } else {
            circuit.failures = 0;
            circuit.opened_at_ms = None;
            false
        }
    }

    fn record_success(&self, action_id: &str) {
        self.state
            .circuits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(action_id);
    }

    fn record_failure(&self, action_id: &str, classification: RetrievalClassification) {
        // Exhaustive on purpose: a new classification must state whether it is
        // a circuit-bearing failure rather than inherit an answer from a
        // wildcard.
        let trip_immediately = match classification {
            // A misconfigured provider is not a transient outage. A broken trust
            // store, a missing CA bundle, or an expired certificate stays broken
            // until an operator repairs it, so spending the ordinary failure
            // threshold on it burns the ladder's attempt budget for a certainty.
            // Open the circuit on the first observation; the ordinary cooldown
            // still re-arms the action, which is what lets a repaired operator
            // configuration heal without a restart.
            RetrievalClassification::ProviderMisconfigured => true,
            RetrievalClassification::ProviderUnavailable | RetrievalClassification::RateLimited => {
                false
            },
            // Everything else is either a success, a caller-side problem, or an
            // outcome the ladder already handles by escalating. None of them say
            // anything about the provider's health, so none of them count.
            RetrievalClassification::Sufficient
            | RetrievalClassification::HandoffRequired
            | RetrievalClassification::InsufficientQuality
            | RetrievalClassification::NotFound
            | RetrievalClassification::JavascriptRequired
            | RetrievalClassification::AuthenticationRequired
            | RetrievalClassification::PolicyDenied
            | RetrievalClassification::InvalidRequest
            | RetrievalClassification::BudgetExhausted
            | RetrievalClassification::DeadlineExceeded
            | RetrievalClassification::Cancelled
            | RetrievalClassification::CircuitOpen => return,
        };
        let mut circuits = self
            .state
            .circuits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let circuit = circuits.entry(action_id.to_string()).or_default();
        circuit.failures = circuit.failures.saturating_add(1);
        if trip_immediately {
            circuit.failures = circuit
                .failures
                .max(self.settings.circuit_failure_threshold);
        }
        if circuit.failures >= self.settings.circuit_failure_threshold {
            circuit.opened_at_ms = Some(Utc::now().timestamp_millis());
        }
    }
}

struct DiscoveryOutcome {
    receipt: RetrievalAttemptReceipt,
    items: Vec<ContentCandidate>,
    required_authority: Option<RetrievalAuthority>,
    handoff: Option<RetrievalHandoff>,
}

impl DiscoveryOutcome {
    fn empty(
        receipt: RetrievalAttemptReceipt,
        required_authority: Option<RetrievalAuthority>,
    ) -> Self {
        Self {
            receipt,
            items: Vec::new(),
            required_authority,
            handoff: None,
        }
    }
}

fn validate_settings(
    settings: &ProgressiveRetrievalSettings,
    catalog: &ContentAcquisitionCatalog,
) -> Result<Vec<String>> {
    settings.validate_bounds()?;
    let descriptors = catalog
        .discovery
        .iter()
        .chain(catalog.readers.iter())
        .map(|descriptor| (descriptor.retrieval.action_id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let mut seen_actions = BTreeSet::new();
    let mut seen_rungs = BTreeSet::new();
    let mut unavailable = Vec::new();
    for (operation, rungs) in [
        (RetrievalOperation::Discover, settings.discover.as_slice()),
        (RetrievalOperation::Read, settings.read.as_slice()),
    ] {
        if rungs.len() > MAX_RETRIEVAL_RUNGS {
            bail!("retrieval ladder exceeds {MAX_RETRIEVAL_RUNGS} rungs");
        }
        for rung in rungs {
            if rung.id.trim().is_empty() || !seen_rungs.insert((operation, rung.id.as_str())) {
                bail!("retrieval ladder has an empty or duplicate rung id");
            }
            let expected_rung = rung_id_to_type(&rung.id)?;
            for (optional, action_id) in rung
                .actions
                .iter()
                .map(|id| (false, id))
                .chain(rung.optional_actions.iter().map(|id| (true, id)))
            {
                if !seen_actions.insert((operation, action_id.as_str())) {
                    bail!("retrieval action `{action_id}` is duplicated in its ladder");
                }
                if seen_actions.len() > MAX_RETRIEVAL_ACTIONS {
                    bail!("retrieval ladders exceed {MAX_RETRIEVAL_ACTIONS} configured actions");
                }
                let Some(descriptor) = descriptors.get(action_id.as_str()) else {
                    if optional {
                        unavailable.push(action_id.clone());
                        continue;
                    }
                    bail!("unknown required retrieval action `{action_id}`");
                };
                if descriptor.retrieval.operation != operation {
                    bail!("retrieval action `{action_id}` has the wrong operation");
                }
                if descriptor.retrieval.rung != expected_rung {
                    bail!("retrieval action `{action_id}` is configured in the wrong rung");
                }
                if rung.mode == RetrievalRungMode::EligibleParallel
                    && !descriptor.retrieval.parallel_safe
                {
                    bail!("retrieval action `{action_id}` is not parallel-safe");
                }
            }
            for descriptor in descriptors.values().copied().filter(|descriptor| {
                descriptor.retrieval.operation == operation
                    && descriptor.retrieval.rung == expected_rung
            }) {
                seen_actions.insert((operation, descriptor.retrieval.action_id.as_str()));
                if seen_actions.len() > MAX_RETRIEVAL_ACTIONS {
                    bail!("retrieval ladders exceed {MAX_RETRIEVAL_ACTIONS} effective actions");
                }
                if rung.mode == RetrievalRungMode::EligibleParallel
                    && !descriptor.retrieval.parallel_safe
                {
                    bail!(
                        "auto-discovered retrieval action `{}` is not parallel-safe for rung `{}`",
                        descriptor.retrieval.action_id,
                        rung.id
                    );
                }
            }
        }
    }
    Ok(unavailable)
}

fn validate_allowed_actions(
    need: &RetrievalNeed,
    catalog: &ContentAcquisitionCatalog,
    settings: &ProgressiveRetrievalSettings,
) -> Result<()> {
    if need.allowed_actions.is_empty() {
        return Ok(());
    }
    let actions = catalog
        .discovery
        .iter()
        .chain(catalog.readers.iter())
        .map(|descriptor| (descriptor.retrieval.action_id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let configured_rungs = match need.operation {
        RetrievalOperation::Discover => &settings.discover,
        RetrievalOperation::Read => &settings.read,
    };
    let mut configured = BTreeSet::new();
    for rung in configured_rungs {
        configured.extend(effective_action_ids(rung, actions.values().copied())?);
    }
    for action_id in &need.allowed_actions {
        let descriptor = actions
            .get(action_id.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown allowed retrieval action `{action_id}`"))?;
        if descriptor.retrieval.operation != need.operation {
            bail!("allowed retrieval action `{action_id}` has the wrong operation");
        }
        if !configured.contains(action_id) {
            bail!("allowed retrieval action `{action_id}` is not present in the configured ladder");
        }
        if !action_allowed(need, descriptor) {
            bail!("allowed retrieval action `{action_id}` is not eligible for the retrieval need");
        }
    }
    Ok(())
}

fn rung_id_to_type(id: &str) -> Result<RetrievalRung> {
    match id {
        "source_native" => Ok(RetrievalRung::SourceNative),
        "public_search" => Ok(RetrievalRung::PublicSearch),
        "public_static" => Ok(RetrievalRung::PublicStatic),
        "verified_replay" => Ok(RetrievalRung::VerifiedReplay),
        "public_rendered" => Ok(RetrievalRung::PublicRendered),
        "public_browser_handoff" => Ok(RetrievalRung::PublicBrowserHandoff),
        "owner_assisted" => Ok(RetrievalRung::OwnerAssisted),
        "authenticated" | "authenticated_browser_handoff" => Ok(RetrievalRung::Authenticated),
        "interaction_handoff" => Ok(RetrievalRung::InteractionHandoff),
        _ => bail!("unknown retrieval rung id `{id}`"),
    }
}

fn configured_actions(rung: &RetrievalRungConfig) -> Vec<&str> {
    rung.actions
        .iter()
        .chain(rung.optional_actions.iter())
        .map(String::as_str)
        .collect()
}

/// Build the effective rung order. Explicit configuration remains an ordering
/// override and may name unavailable optional actions; every installed adapter
/// whose own SKILL.md declares this rung is then appended deterministically.
/// This makes a governed skill genuinely drop-in without requiring a central
/// action-id edit for each new content adapter.
fn effective_action_ids<'a>(
    rung: &RetrievalRungConfig,
    descriptors: impl IntoIterator<Item = &'a ContentSourceDescriptor>,
) -> Result<Vec<String>> {
    let expected_rung = rung_id_to_type(&rung.id)?;
    let mut actions = configured_actions(rung)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut seen = actions.iter().cloned().collect::<BTreeSet<_>>();
    let mut discovered = descriptors
        .into_iter()
        .filter(|descriptor| descriptor.retrieval.rung == expected_rung)
        .filter(|descriptor| seen.insert(descriptor.retrieval.action_id.clone()))
        .map(|descriptor| {
            (
                descriptor.retrieval.priority,
                descriptor.retrieval.action_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    discovered.sort();
    actions.extend(discovered.into_iter().map(|(_, action_id)| action_id));
    Ok(actions)
}

fn action_allowed(need: &RetrievalNeed, descriptor: &ContentSourceDescriptor) -> bool {
    let has_targets = matches!(
        &need.target,
        RetrievalTarget::Query { targets, .. } if !targets.is_empty()
    );
    let explicitly_selected = need
        .allowed_actions
        .iter()
        .any(|action| action == &descriptor.retrieval.action_id);
    let source_native_eligible = descriptor.retrieval.rung != RetrievalRung::SourceNative
        || descriptor.retrieval.requires_targets
        || explicitly_selected;
    let read_output_eligible = match &need.goal {
        EvidenceGoal::Discovery(_) => true,
        EvidenceGoal::Read(goal) => {
            let output = effective_read_output(goal);
            descriptor.retrieval.outputs.contains(&output)
                || (output == super::RetrievalOutputKind::Gist
                    && descriptor
                        .retrieval
                        .outputs
                        .contains(&super::RetrievalOutputKind::FullText))
                || descriptor
                    .retrieval
                    .outputs
                    .contains(&super::RetrievalOutputKind::Handoff)
        },
    };
    let media_eligible = match &need.target {
        RetrievalTarget::Candidate { candidate, .. } => candidate_media_type_hint(candidate)
            .is_none_or(|media_type| {
                descriptor.retrieval.accepted_media_types.is_empty()
                    || descriptor
                        .retrieval
                        .accepted_media_types
                        .iter()
                        .any(|accepted| accepted.eq_ignore_ascii_case(media_type))
            }),
        RetrievalTarget::Query { .. } => true,
    };
    authority_allows(need.maximum_authority, descriptor.retrieval.authority)
        && source_native_eligible
        && read_output_eligible
        && media_eligible
        && (!descriptor.retrieval.requires_targets || has_targets)
        && (need.allowed_actions.is_empty() || explicitly_selected)
}

fn effective_read_output(goal: &ReadEvidenceGoal) -> super::RetrievalOutputKind {
    goal.output.unwrap_or(match goal.depth {
        ReadDepth::Gist => super::RetrievalOutputKind::Gist,
        ReadDepth::FullText => super::RetrievalOutputKind::FullText,
    })
}

fn candidate_media_type_hint(candidate: &ContentCandidate) -> Option<&str> {
    for key in ["media_type", "content_type"] {
        if let Some(value) = candidate.metadata.get(key).and_then(Value::as_str) {
            return value
                .split(';')
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
        }
    }
    candidate
        .canonical_url
        .as_deref()
        .and_then(|url| url::Url::parse(url).ok())
        .filter(|url| url.path().to_ascii_lowercase().ends_with(".pdf"))
        .map(|_| "application/pdf")
}

fn authority_allows(maximum: RetrievalAuthority, required: RetrievalAuthority) -> bool {
    use RetrievalAuthority::{
        AuthenticatedInteract, AuthenticatedRead, LocalOnly, PublicBrowserInteract,
        PublicBrowserRead, PublicRemoteRead,
    };
    match maximum {
        LocalOnly => required == LocalOnly,
        PublicRemoteRead => matches!(required, LocalOnly | PublicRemoteRead),
        PublicBrowserRead => matches!(required, LocalOnly | PublicRemoteRead | PublicBrowserRead),
        PublicBrowserInteract => matches!(
            required,
            LocalOnly | PublicRemoteRead | PublicBrowserRead | PublicBrowserInteract
        ),
        AuthenticatedRead => matches!(
            required,
            LocalOnly | PublicRemoteRead | PublicBrowserRead | AuthenticatedRead
        ),
        AuthenticatedInteract => true,
    }
}

fn attempt(
    trace_id: &str,
    rung: &RetrievalRungConfig,
    descriptor: &ContentSourceDescriptor,
    configured_order: usize,
    classification: RetrievalClassification,
    latency_ms: u64,
) -> RetrievalAttemptReceipt {
    RetrievalAttemptReceipt {
        fingerprint: action_fingerprint(
            trace_id,
            &rung.id,
            &descriptor.retrieval.action_id,
            configured_order,
        ),
        rung_id: rung.id.clone(),
        action_id: descriptor.retrieval.action_id.clone(),
        configured_order,
        actual_order: 0,
        classification,
        authority: descriptor.retrieval.authority,
        privacy: None,
        cache_outcome: None,
        escalation_reason: escalation_reason(classification),
        failure_detail: None,
        actual_transport: None,
        requested_browser_mode: None,
        resolved_browser_mode: None,
        browser_engine: None,
        browser_session_fingerprint: None,
        session_outcome: None,
        approval_receipt_id: None,
        render_ms: None,
        extract_ms: None,
        latency_ms,
        returned_items: 0,
        cost: None,
        quality_findings: Vec::new(),
    }
}

fn requires_identity_grant(descriptor: &ContentSourceDescriptor) -> bool {
    matches!(
        descriptor.retrieval.authority,
        RetrievalAuthority::AuthenticatedRead | RetrievalAuthority::AuthenticatedInteract
    )
}

fn handoff_from_error(error: &anyhow::Error) -> Option<RetrievalHandoff> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<RetrievalHandoffRequired>()
            .map(|required| required.handoff.clone())
    })
}

fn transport_trace_from_error(error: &anyhow::Error) -> Option<RetrievalTransportTrace> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<RetrievalTransportFailure>()
            .map(|failure| failure.trace.clone())
    })
}

fn project_transport_trace(trace: RetrievalTransportTrace, receipt: &mut RetrievalAttemptReceipt) {
    receipt.actual_transport = trace.actual_transport;
    receipt.requested_browser_mode = trace.requested_browser_mode;
    receipt.resolved_browser_mode = trace.resolved_browser_mode;
    receipt.browser_engine = trace.browser_engine;
    receipt.browser_session_fingerprint = trace.browser_session_fingerprint;
    receipt.session_outcome = trace.session_outcome;
    receipt.render_ms = trace.render_ms;
    receipt.extract_ms = trace.extract_ms;
}

fn project_declared_transport_after_abort(
    descriptor: &ContentSourceDescriptor,
    receipt: &mut RetrievalAttemptReceipt,
) {
    if !matches!(
        receipt.classification,
        RetrievalClassification::Cancelled | RetrievalClassification::DeadlineExceeded
    ) {
        return;
    }
    match descriptor.retrieval.action_id.as_str() {
        "browser.headless.read" => {
            receipt.actual_transport = Some("browser".into());
            receipt.requested_browser_mode = Some("public_headless_read".into());
            receipt.resolved_browser_mode = Some("public_headless_read".into());
            receipt.session_outcome = Some("cleanup_scheduled".into());
        },
        "browser.cdp.read" => {
            receipt.actual_transport = Some("browser".into());
            receipt.requested_browser_mode = Some("authenticated_cdp_read".into());
            receipt.resolved_browser_mode = Some("authenticated_cdp_read".into());
            receipt.browser_engine = Some("magicutor_cdp".into());
            receipt.session_outcome = Some("cleanup_scheduled".into());
        },
        "api_replay.read" => {
            receipt.actual_transport = Some("api_replay".into());
            receipt.session_outcome = Some("not_applicable".into());
        },
        _ => {},
    }
}

fn new_read_handoff(
    need: &RetrievalNeed,
    candidate: &ContentCandidate,
    kind: RetrievalHandoffKind,
    requested_mode: &str,
    action_id: &str,
    required_authority: RetrievalAuthority,
    requires_approval: bool,
) -> RetrievalHandoff {
    let id = format!("rh_{}", uuid::Uuid::new_v4().simple());
    RetrievalHandoff {
        browser_session_id: format!("retrieval-cdp-{id}"),
        id,
        kind,
        requested_mode: requested_mode.to_string(),
        action_id: action_id.to_string(),
        required_authority,
        principal: need.principal.clone(),
        workspace: need.workspace.clone(),
        target_url: candidate.canonical_url.clone(),
        query: None,
        requires_approval,
        expires_at_ms: Utc::now().timestamp_millis().saturating_add(5 * 60 * 1_000),
    }
}

fn project_attempt_transport(
    document: &mut ContentDocument,
    receipt: &mut RetrievalAttemptReceipt,
) {
    receipt.actual_transport = take_metadata_string(&mut document.metadata, META_ACTUAL_TRANSPORT);
    receipt.requested_browser_mode =
        take_metadata_string(&mut document.metadata, META_REQUESTED_BROWSER_MODE);
    receipt.resolved_browser_mode =
        take_metadata_string(&mut document.metadata, META_RESOLVED_BROWSER_MODE);
    receipt.browser_engine = take_metadata_string(&mut document.metadata, META_BROWSER_ENGINE);
    receipt.browser_session_fingerprint =
        take_metadata_string(&mut document.metadata, META_BROWSER_SESSION_FINGERPRINT);
    receipt.session_outcome = take_metadata_string(&mut document.metadata, META_SESSION_OUTCOME);
    receipt.render_ms = take_metadata_u64(&mut document.metadata, META_RENDER_MS);
    receipt.extract_ms = take_metadata_u64(&mut document.metadata, META_EXTRACT_MS);
}

fn take_metadata_string(metadata: &mut BTreeMap<String, Value>, key: &str) -> Option<String> {
    metadata.remove(key).and_then(|value| match value {
        Value::String(value) => Some(value),
        _ => None,
    })
}

fn take_metadata_u64(metadata: &mut BTreeMap<String, Value>, key: &str) -> Option<u64> {
    metadata.remove(key).and_then(|value| value.as_u64())
}

fn action_fingerprint(_trace_id: &str, rung: &str, action: &str, _order: usize) -> String {
    // Fingerprints identify the configured action contract across runs. The
    // per-invocation trace and actual order are recorded separately.
    blake3::hash(format!("retrieval-action-v1\0{rung}\0{action}").as_bytes())
        .to_hex()
        .to_string()
}

fn candidate_fingerprint(candidate: &ContentCandidate) -> String {
    // The receipt protects the complete evidence-bearing candidate, not only
    // its URL. In particular, binding `cheap_text` prevents a caller from
    // keeping a valid discovered URL while replacing its snippet with model-
    // generated claims. BTreeMap-backed metadata keeps this serialization
    // deterministic; the debug fallback is only defensive because a
    // serde_json::Value-backed candidate is always JSON serializable.
    let encoded =
        serde_json::to_vec(candidate).unwrap_or_else(|_| format!("{candidate:?}").into_bytes());
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"content-candidate-receipt-v2\0");
    hasher.update(&encoded);
    hasher.finalize().to_hex().to_string()
}

fn selection_reason(source: ContentInvocationSource) -> ReadSelectionReason {
    match source {
        ContentInvocationSource::UserFeed => ReadSelectionReason::FeedMatch,
        ContentInvocationSource::RecurringMonitor => ReadSelectionReason::MonitorMatch,
        ContentInvocationSource::ObservedSource => ReadSelectionReason::ObserveMatch,
        ContentInvocationSource::InteractiveRead => ReadSelectionReason::UserRequested,
        ContentInvocationSource::InternalSystem => ReadSelectionReason::InternalPolicy,
    }
}

fn merge_candidates(
    existing: Vec<ContentCandidate>,
    incoming: Vec<ContentCandidate>,
    limit: usize,
) -> Vec<ContentCandidate> {
    let mut seen = BTreeSet::new();
    existing
        .into_iter()
        .chain(incoming)
        .filter(|candidate| {
            let key = candidate.content_hash.as_ref().map_or_else(
                || {
                    candidate
                        .canonical_url
                        .as_deref()
                        .and_then(|url| super::canonicalize_http_url(url).ok())
                        .unwrap_or_else(|| {
                            format!(
                                "identity:{}:{}",
                                candidate.identity.adapter_id, candidate.identity.item_id
                            )
                        })
                },
                |content_hash| format!("content:{}", content_hash.trim().to_ascii_lowercase()),
            );
            seen.insert(key)
        })
        .take(limit)
        .collect()
}

fn candidate_relevance(candidate: &ContentCandidate, query: &str) -> f64 {
    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        return 1.0;
    }
    let text = lexical_tokens(&format!("{} {}", candidate.title, candidate.cheap_text));
    let overlap = query_tokens.intersection(&text).count();
    overlap as f64 / query_tokens.len() as f64
}

fn lexical_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn discovery_goal_satisfied(
    candidates: &[ContentCandidate],
    query: &str,
    goal: &DiscoveryEvidenceGoal,
) -> bool {
    if candidates.len() < goal.min_candidates {
        return false;
    }
    let relevant = candidates
        .iter()
        .filter(|candidate| candidate_relevance(candidate, query) >= goal.relevance_threshold)
        .count();
    if relevant < goal.min_relevant_candidates {
        return false;
    }
    independent_sources(candidates, query, goal.relevance_threshold) >= goal.min_independent_sources
}

fn independent_sources(
    candidates: &[ContentCandidate],
    query: &str,
    relevance_threshold: f64,
) -> usize {
    candidates
        .iter()
        .filter(|candidate| candidate_relevance(candidate, query) >= relevance_threshold)
        .map(|candidate| {
            candidate
                .canonical_url
                .as_deref()
                .and_then(|url| url::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                .unwrap_or_else(|| candidate.identity.adapter_id.clone())
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn discovery_unmet_reasons(
    candidates: &[ContentCandidate],
    query: &str,
    goal: &DiscoveryEvidenceGoal,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if candidates.len() < goal.min_candidates {
        reasons.push(format!(
            "found {} of {} required candidates",
            candidates.len(),
            goal.min_candidates
        ));
    }
    let relevant = candidates
        .iter()
        .filter(|candidate| candidate_relevance(candidate, query) >= goal.relevance_threshold)
        .count();
    if relevant < goal.min_relevant_candidates {
        reasons.push(format!(
            "found {relevant} of {} relevant candidates",
            goal.min_relevant_candidates
        ));
    }
    let independent = independent_sources(candidates, query, goal.relevance_threshold);
    if independent < goal.min_independent_sources {
        reasons.push(format!(
            "found {independent} of {} independent sources",
            goal.min_independent_sources
        ));
    }
    if reasons.is_empty() {
        reasons.push("configured retrieval ladder was exhausted".into());
    }
    reasons
}

struct DocumentQuality {
    sufficient: bool,
    score: usize,
    findings: Vec<RetrievalQualityFinding>,
}

fn evaluate_document(document: &ContentDocument, goal: &ReadEvidenceGoal) -> DocumentQuality {
    let mut findings = Vec::new();
    let chars = document.text.chars().count();
    let default_min = match goal.depth {
        ReadDepth::Gist => 80,
        ReadDepth::FullText => 200,
    };
    let min_chars = goal.min_chars.unwrap_or(default_min);
    if chars < min_chars {
        findings.push(finding(
            "too_short",
            format!("{chars} characters, requires {min_chars}"),
        ));
    }
    for code in noncontent_shell_codes(&document.text) {
        findings.push(finding(
            code,
            "page content matches a known non-content shell",
        ));
    }
    for key in &goal.required_metadata {
        if !document.metadata.contains_key(key) {
            findings.push(finding(
                "missing_metadata",
                format!("required metadata `{key}` is absent"),
            ));
        }
    }
    if effective_read_output(goal) == super::RetrievalOutputKind::Structured {
        let structured = serde_json::from_str::<Value>(&document.text).ok();
        if !structured.as_ref().is_some_and(|value| {
            value.as_array().is_some_and(|records| !records.is_empty())
                || value.as_object().is_some_and(|record| !record.is_empty())
        }) {
            findings.push(finding(
                "structured_schema_invalid",
                "structured evidence must be a non-empty JSON object or array",
            ));
        }
    }
    if goal.depth == ReadDepth::FullText
        && document
            .metadata
            .get("extraction_truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        findings.push(finding(
            "truncated",
            "full-text evidence was truncated by the extractor",
        ));
    }
    if document
        .metadata
        .get("extraction_quality")
        .and_then(|quality| quality.get("score"))
        .and_then(Value::as_f64)
        .is_some_and(|score| !score.is_finite() || score < 0.5)
    {
        findings.push(finding(
            "low_extraction_confidence",
            "extractor quality score is below the acceptance threshold",
        ));
    }
    let lines = document
        .text
        .lines()
        .map(str::trim)
        .filter(|line| line.chars().count() >= 8)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if lines.len() >= 6 {
        let mut frequencies = BTreeMap::new();
        for line in &lines {
            *frequencies.entry(line).or_insert(0usize) += 1;
        }
        if frequencies
            .values()
            .copied()
            .max()
            .is_some_and(|count| count.saturating_mul(2) >= lines.len())
        {
            findings.push(finding(
                "boilerplate_repetition",
                "repeated navigation or boilerplate dominates the extracted text",
            ));
        }
    }
    let unique_words = lexical_tokens(&document.text).len();
    if chars >= min_chars && unique_words < 12 {
        findings.push(finding(
            "low_information_density",
            "content has too few distinct words for its length",
        ));
    }
    DocumentQuality {
        sufficient: findings.is_empty(),
        score: chars.saturating_add(unique_words.saturating_mul(20)),
        findings,
    }
}

pub fn noncontent_shell_codes(text: &str) -> Vec<&'static str> {
    let normalized = text.to_ascii_lowercase();
    [
        (
            "login_wall",
            &["sign in to continue", "log in to continue"][..],
        ),
        (
            "error_page",
            &[
                "404 not found",
                "500 internal server error",
                "access denied",
            ][..],
        ),
        (
            "javascript_shell",
            &[
                "enable javascript",
                "javascript is required",
                "please turn on javascript",
            ][..],
        ),
        (
            "paywall",
            &["subscribe to continue", "subscription required"][..],
        ),
    ]
    .into_iter()
    .filter_map(|(code, needles)| {
        needles
            .iter()
            .any(|needle| normalized.contains(needle))
            .then_some(code)
    })
    .collect()
}

fn classify_document_quality(quality: &DocumentQuality) -> RetrievalClassification {
    if quality.sufficient {
        RetrievalClassification::Sufficient
    } else if quality
        .findings
        .iter()
        .any(|finding| matches!(finding.code.as_str(), "login_wall" | "paywall"))
    {
        RetrievalClassification::AuthenticationRequired
    } else if quality
        .findings
        .iter()
        .any(|finding| finding.code == "javascript_shell")
    {
        RetrievalClassification::JavascriptRequired
    } else {
        RetrievalClassification::InsufficientQuality
    }
}

fn finding(code: impl Into<String>, message: impl Into<String>) -> RetrievalQualityFinding {
    RetrievalQualityFinding {
        code: code.into(),
        message: message.into(),
    }
}

fn inline_document(candidate: &ContentCandidate) -> ContentDocument {
    let text = candidate.cheap_text.clone();
    let mut metadata = candidate.metadata.clone();
    metadata.insert("fetch_status".into(), Value::String("not_fetched".into()));
    metadata.insert(
        "evidence_role".into(),
        Value::String("discovery_only".into()),
    );
    metadata.insert("claim_eligible".into(), Value::Bool(false));
    metadata.insert(
        "inline_integrity".into(),
        Value::String("selection_receipt_verified".into()),
    );
    ContentDocument {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: candidate.identity.clone(),
        title: candidate.title.clone(),
        canonical_url: candidate.canonical_url.clone(),
        media_type: Some("text/plain; source=inline".into()),
        fetched_at_ms: Utc::now().timestamp_millis(),
        privacy: candidate.privacy,
        content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
        provenance: candidate.provenance.clone(),
        metadata,
        text,
    }
}

fn budget_exhausted(
    need: &RetrievalNeed,
    settings: &ProgressiveRetrievalSettings,
    spent: &BTreeMap<String, u64>,
) -> bool {
    spent.iter().any(|(commodity, amount)| {
        let need_limit = need.cost_budget_microunits.get(commodity).copied();
        let config_limit = settings.max_cost_microunits.get(commodity).copied();
        match (need_limit, config_limit) {
            (Some(left), Some(right)) => *amount > left.min(right),
            (Some(limit), None) | (None, Some(limit)) => *amount > limit,
            (None, None) => false,
        }
    })
}

fn reserve_estimated_cost(
    need: &RetrievalNeed,
    settings: &ProgressiveRetrievalSettings,
    descriptor: &ContentSourceDescriptor,
    projected: &mut BTreeMap<String, u64>,
) -> bool {
    if !descriptor.capabilities.metered {
        return true;
    }
    let has_any_budget =
        !need.cost_budget_microunits.is_empty() || !settings.max_cost_microunits.is_empty();
    let Some(estimate) = descriptor.retrieval.estimated_cost.as_ref() else {
        // An explicitly budgeted request cannot admit an unpriceable metered
        // action. With no budget, actual cost is still reconciled afterward.
        return !has_any_budget;
    };
    let need_limit = need
        .cost_budget_microunits
        .get(&estimate.commodity)
        .copied();
    let config_limit = settings
        .max_cost_microunits
        .get(&estimate.commodity)
        .copied();
    let limit = match (need_limit, config_limit) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(limit), None) | (None, Some(limit)) => Some(limit),
        (None, None) => None,
    };
    let next = projected
        .get(&estimate.commodity)
        .copied()
        .unwrap_or_default()
        .saturating_add(estimate.amount_microunits);
    if limit.is_some_and(|limit| next > limit) {
        return false;
    }
    projected.insert(estimate.commodity.clone(), next);
    true
}

fn classify_error(error: &anyhow::Error) -> RetrievalClassification {
    if handoff_from_error(error).is_some() {
        return RetrievalClassification::HandoffRequired;
    }
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<super::registry::ContentPolicyDenied>()
            .is_some()
    }) {
        return RetrievalClassification::PolicyDenied;
    }
    let message = error_message(error);
    if message.contains("policy") || message.contains("permission to send") {
        RetrievalClassification::PolicyDenied
    } else if message.contains("login_wall") || message.contains("paywall") {
        RetrievalClassification::AuthenticationRequired
    } else if message.contains("javascript_shell") {
        RetrievalClassification::JavascriptRequired
    } else if message.contains("error_page") || message.contains("non-content shell") {
        RetrievalClassification::InsufficientQuality
    } else if message.contains("authentication")
        || message.contains("unauthorized")
        || message.contains("status 401")
        || message.contains("status 403")
    {
        RetrievalClassification::AuthenticationRequired
    } else if message.contains("rate limit") || message.contains("status 429") {
        RetrievalClassification::RateLimited
    } else if message.contains("not found") || message.contains("status 404") {
        RetrievalClassification::NotFound
    } else if message.contains("invalid") || message.contains("requires") {
        RetrievalClassification::InvalidRequest
    } else if is_tls_trust_failure(&message) {
        // Last arm before the catch-all on purpose. Every more specific arm
        // above — policy, authentication, rate limit, not found, invalid
        // request — still wins for a message carrying both signals. A TLS
        // failure only ever lands here when nothing more specific matched,
        // which is exactly the bucket that used to swallow a missing CA bundle
        // as a provider outage.
        RetrievalClassification::ProviderMisconfigured
    } else {
        RetrievalClassification::ProviderUnavailable
    }
}

/// Refine provider-facing discovery failures with the adapter contract. A
/// public search provider's own API credential is not user identity, and a
/// malformed capability response is not a caller request error. Both must
/// advance the fallback ladder instead of asking the user for authority or
/// stopping the search entirely.
fn classify_discovery_error(
    error: &anyhow::Error,
    descriptor: &ContentSourceDescriptor,
) -> RetrievalClassification {
    let classification = classify_error(error);
    let message = error_message(error);
    let provider_contract_failure = message.contains("returned invalid json")
        || message.contains("capability output")
        || message.contains("mapped output");
    if provider_contract_failure
        && matches!(
            classification,
            RetrievalClassification::InvalidRequest | RetrievalClassification::ProviderUnavailable
        )
    {
        return RetrievalClassification::ProviderMisconfigured;
    }

    if classification == RetrievalClassification::AuthenticationRequired
        && !requires_identity_grant(descriptor)
        && descriptor.capabilities.auth == AdapterAuth::Required
    {
        return RetrievalClassification::ProviderMisconfigured;
    }

    let provider_credential_failure = !requires_identity_grant(descriptor)
        && descriptor.capabilities.auth == AdapterAuth::Required
        && matches!(
            classification,
            RetrievalClassification::InvalidRequest | RetrievalClassification::ProviderUnavailable
        )
        && ["credential", "api key", "api_key", "access token"]
            .iter()
            .any(|needle| message.contains(needle));
    if provider_credential_failure {
        RetrievalClassification::ProviderMisconfigured
    } else {
        classification
    }
}

/// Bounded, original-case cause text for the attempt receipt. Deliberately
/// short: this rides on every failed attempt's telemetry line, and its only job
/// is to name the cause the classification cannot.
///
/// The root cause, not the joined chain. The outer links are `running discovery
/// adapter <id>` and `invoking capability <name>`, which `action_id` and
/// `rung_id` on the same telemetry line already carry. Inside a 200-character
/// bound that boilerplate is what pushes the one token worth having —
/// CERTIFICATE_VERIFY_FAILED and its like — off the end.
/// Whether a message describes a TLS *trust* failure, not merely a message that
/// mentions TLS.
///
/// `ssl`, `tls`, and `certificate` are topic tokens, not failure tokens. A
/// retrieval error that echoes the fetched URL — which reqwest's `Display` does
/// in some versions — would otherwise classify a transient failure against a
/// host like `ssllabs.com` as a misconfiguration, and `ProviderMisconfigured`
/// is deliberately non-retryable. Requiring a failure token alongside the topic
/// token keeps a hostname from costing the ladder a rung.
fn is_tls_trust_failure(message: &str) -> bool {
    const TOPIC_TOKENS: &[&str] = &["certificate", "ssl", "tls"];
    const FAILURE_TOKENS: &[&str] = &[
        "verify failed",
        "handshake",
        "issuer",
        "self signed",
        "self-signed",
        "expired",
        "untrusted",
        "unable to get local",
        "wrong version number",
        "bad certificate",
    ];
    TOPIC_TOKENS.iter().any(|token| message.contains(token))
        && FAILURE_TOKENS.iter().any(|token| message.contains(token))
}

fn failure_detail(error: &anyhow::Error) -> String {
    error
        .root_cause()
        .to_string()
        .chars()
        .take(MAX_FAILURE_DETAIL_CHARS)
        .collect()
}

fn error_message(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
        .to_ascii_lowercase()
}

fn classify_read_error(
    error: &anyhow::Error,
    descriptor: &ContentSourceDescriptor,
) -> RetrievalClassification {
    let classification = classify_error(error);
    let message = error_message(error);
    let ambiguous_transport_denial = message.contains("status 401")
        || message.contains("status 403")
        || message.contains("forbidden");
    let explicit_identity_signal = message.contains("authentication")
        || message.contains("login_wall")
        || message.contains("paywall")
        || (message.contains("unauthorized") && !message.contains("status 401"));
    if classification == RetrievalClassification::AuthenticationRequired
        && !requires_identity_grant(descriptor)
        && ambiguous_transport_denial
        && !explicit_identity_signal
    {
        // A bare transport denial from an anonymous static HTTP client is
        // ambiguous: many public sites return 401/403 to non-browser clients
        // as bot protection. It is not proof that user identity is required.
        // Keep the request on the public-authority ladder so a rendered reader
        // can try next. If that reader observes an actual login wall/paywall,
        // document-quality classification still requests scoped authority.
        RetrievalClassification::JavascriptRequired
    } else {
        classification
    }
}

fn classification_name(classification: RetrievalClassification) -> &'static str {
    match classification {
        RetrievalClassification::PolicyDenied => "policy denial",
        RetrievalClassification::InvalidRequest => "invalid request",
        RetrievalClassification::ProviderMisconfigured => "provider misconfiguration",
        _ => "retrieval failure",
    }
}

fn escalation_reason(classification: RetrievalClassification) -> Option<String> {
    match classification {
        RetrievalClassification::HandoffRequired => Some("browser_handoff_required".into()),
        RetrievalClassification::JavascriptRequired => Some("javascript_required".into()),
        RetrievalClassification::AuthenticationRequired => Some("authentication_required".into()),
        RetrievalClassification::PolicyDenied => Some("policy_denied".into()),
        RetrievalClassification::BudgetExhausted => Some("budget_exhausted".into()),
        RetrievalClassification::DeadlineExceeded => Some("deadline_exceeded".into()),
        RetrievalClassification::Cancelled => Some("cancelled".into()),
        RetrievalClassification::CircuitOpen => Some("circuit_open".into()),
        RetrievalClassification::ProviderMisconfigured => Some("provider_misconfigured".into()),
        _ => None,
    }
}

fn privacy_level(privacy: ContentPrivacy) -> u8 {
    match privacy {
        ContentPrivacy::Public => 0,
        ContentPrivacy::Private => 1,
        ContentPrivacy::Restricted => 2,
    }
}

fn degraded_status(has_evidence: bool) -> RetrievalStatus {
    if has_evidence {
        RetrievalStatus::Degraded
    } else {
        RetrievalStatus::Failed
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use anyhow::Result;
    use async_trait::async_trait;
    use serde::Deserialize;

    use super::*;
    use crate::magician_v2::content_sources::{
        AdapterAuth, AdapterExecution, ContentProvenance, ContentReader, ContentSourceCapabilities,
        ContentSourceClass, DiscoveryAdapter, DiscoveryPage, RetrievalOutputKind, SourceIdentity,
    };

    #[derive(Deserialize)]
    struct OfflineEvalCorpus {
        schema_version: u32,
        read_cases: Vec<ReadEvalCase>,
        discovery_cases: Vec<DiscoveryEvalCase>,
        ladder_cases: Vec<LadderEvalCase>,
        merge_cases: Vec<MergeEvalCase>,
    }

    #[derive(Deserialize)]
    struct ReadEvalCase {
        id: String,
        depth: ReadDepth,
        #[serde(default)]
        output: Option<RetrievalOutputKind>,
        text: String,
        #[serde(default)]
        media_type: Option<String>,
        #[serde(default)]
        metadata: BTreeMap<String, Value>,
        expected: RetrievalClassification,
    }

    #[derive(Deserialize)]
    struct DiscoveryEvalCase {
        id: String,
        query: String,
        title: String,
        snippet: String,
        minimum_score: Option<f64>,
        maximum_score: Option<f64>,
    }

    #[derive(Deserialize)]
    struct LadderEvalCase {
        id: String,
        first: String,
        second: String,
        min_candidates: usize,
        expected_status: RetrievalStatus,
        expected_attempt_classifications: Vec<RetrievalClassification>,
        expected_direct_complete: bool,
        expected_calls: usize,
    }

    #[derive(Deserialize)]
    struct MergeEvalCase {
        id: String,
        inputs: Vec<MergeEvalInput>,
        expected_unique: usize,
    }

    #[derive(Deserialize)]
    struct MergeEvalInput {
        adapter_id: String,
        item_id: String,
        url: String,
        #[serde(default)]
        content_hash: Option<String>,
    }

    #[derive(Clone)]
    enum DiscoveryBehavior {
        Empty,
        One,
        Error(&'static str),
    }

    struct ScriptedDiscovery {
        descriptor: ContentSourceDescriptor,
        behavior: DiscoveryBehavior,
        delay_ms: u64,
        calls: Arc<AtomicUsize>,
        order: Arc<Mutex<Vec<String>>>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    struct ScriptedReader {
        descriptor: ContentSourceDescriptor,
        calls: Arc<AtomicUsize>,
        text: String,
        delay_ms: u64,
    }

    struct FailingReader {
        descriptor: ContentSourceDescriptor,
        calls: Arc<AtomicUsize>,
        error: &'static str,
    }

    #[async_trait]
    impl ContentReader for ScriptedReader {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            Ok(ContentDocument {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: request.candidate.identity.clone(),
                title: request.candidate.title.clone(),
                text: self.text.clone(),
                canonical_url: request.candidate.canonical_url.clone(),
                media_type: Some("text/plain".into()),
                fetched_at_ms: Utc::now().timestamp_millis(),
                privacy: request.candidate.privacy,
                content_hash: blake3::hash(b"reader document").to_hex().to_string(),
                provenance: request.candidate.provenance.clone(),
                metadata: BTreeMap::new(),
            })
        }
    }

    #[async_trait]
    impl ContentReader for FailingReader {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn read(&self, _request: &ReadRequest) -> Result<ContentDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!(self.error)
        }
    }

    #[async_trait]
    impl DiscoveryAdapter for ScriptedDiscovery {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn discover(&self, _request: &DiscoveryRequest) -> Result<DiscoveryPage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.order
                .lock()
                .unwrap()
                .push(self.descriptor.retrieval.action_id.clone());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            match &self.behavior {
                DiscoveryBehavior::Empty => Ok(DiscoveryPage {
                    items: Vec::new(),
                    next_cursor: None,
                    validators: BTreeMap::new(),
                    cost: None,
                    transport: Default::default(),
                }),
                DiscoveryBehavior::One => Ok(DiscoveryPage {
                    items: vec![candidate(
                        &self.descriptor.adapter_id,
                        &format!("{}.test", self.descriptor.adapter_id),
                    )],
                    next_cursor: None,
                    validators: BTreeMap::new(),
                    cost: self.descriptor.capabilities.metered.then_some(AdapterCost {
                        commodity: "credit".into(),
                        amount_microunits: 1_000_000,
                    }),
                    transport: Default::default(),
                }),
                DiscoveryBehavior::Error(message) => bail!(*message),
            }
        }
    }

    fn discovery_descriptor(
        adapter_id: &str,
        action_id: &str,
        rung_type: RetrievalRung,
        metered: bool,
    ) -> ContentSourceDescriptor {
        let mut retrieval = super::super::RetrievalActionMetadata::discovery(
            action_id,
            rung_type,
            RetrievalAuthority::PublicRemoteRead,
            true,
        );
        if metered {
            retrieval.estimated_cost = Some(AdapterCost {
                commodity: "credit".into(),
                amount_microunits: 1_000_000,
            });
        }
        ContentSourceDescriptor {
            adapter_id: adapter_id.into(),
            display_name: adapter_id.into(),
            class: ContentSourceClass::WebSearch,
            capabilities: ContentSourceCapabilities {
                discovery: true,
                full_content: false,
                cursor: false,
                conditional_fetch: false,
                execution: AdapterExecution::RemoteEndpoint,
                auth: AdapterAuth::None,
                sends_user_intent: true,
                metered,
            },
            retrieval,
        }
    }

    fn reader_descriptor(adapter_id: &str, action_id: &str) -> ContentSourceDescriptor {
        ContentSourceDescriptor {
            adapter_id: adapter_id.into(),
            display_name: adapter_id.into(),
            class: ContentSourceClass::WebPage,
            capabilities: ContentSourceCapabilities {
                discovery: false,
                full_content: true,
                cursor: false,
                conditional_fetch: true,
                execution: AdapterExecution::LocalProcess,
                auth: AdapterAuth::None,
                sends_user_intent: false,
                metered: false,
            },
            retrieval: super::super::RetrievalActionMetadata::reader(
                action_id,
                RetrievalRung::PublicStatic,
                RetrievalAuthority::PublicRemoteRead,
                vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
            ),
        }
    }

    fn candidate(adapter_id: &str, host: &str) -> ContentCandidate {
        ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new(adapter_id, "one").unwrap(),
            title: "rust runtime release".into(),
            cheap_text: "rust async runtime release notes".into(),
            canonical_url: Some(format!("https://{host}/one")),
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: ContentProvenance {
                source_label: host.into(),
                source_url: None,
                retrieved_by: adapter_id.into(),
            },
            metadata: BTreeMap::new(),
        }
    }

    fn scripted_adapter(
        descriptor: ContentSourceDescriptor,
        behavior: DiscoveryBehavior,
        delay_ms: u64,
        calls: Arc<AtomicUsize>,
        order: Arc<Mutex<Vec<String>>>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    ) -> Arc<ScriptedDiscovery> {
        Arc::new(ScriptedDiscovery {
            descriptor,
            behavior,
            delay_ms,
            calls,
            order,
            active,
            max_active,
        })
    }

    fn discovery_need(policy: RemoteDataPolicy, minimum: usize) -> RetrievalNeed {
        RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Discover,
            target: RetrievalTarget::Query {
                query: "rust runtime".into(),
                intent: None,
                targets: Vec::new(),
                inline_candidates: Vec::new(),
                limit: 10,
                cursor: None,
                options: BTreeMap::new(),
            },
            goal: EvidenceGoal::Discovery(DiscoveryEvidenceGoal {
                min_candidates: minimum,
                min_relevant_candidates: minimum,
                min_independent_sources: minimum,
                relevance_threshold: 0.5,
            }),
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: policy,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::PublicRemoteRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        }
    }

    fn controller_with(
        adapters: Vec<Arc<ScriptedDiscovery>>,
        rung_config: RetrievalRungConfig,
        max_parallel: usize,
    ) -> RetrievalLadderController {
        let mut registry = super::super::ContentSourceRegistry::new();
        for adapter in adapters {
            registry.register_discovery(adapter).unwrap();
        }
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            max_parallel_actions: max_parallel,
            discover: vec![rung_config],
            read: Vec::new(),
            ..ProgressiveRetrievalSettings::default()
        };
        service.retrieval_controller(settings)
    }

    #[test]
    fn defaults_keep_system_actions_explicit_and_order_skill_actions_from_metadata() {
        let settings = ProgressiveRetrievalSettings::default();
        let all = settings
            .discover
            .iter()
            .chain(settings.read.iter())
            .flat_map(configured_actions)
            .collect::<Vec<_>>();
        assert!(all.iter().any(|action| action.starts_with("browser.")));
        assert!(all.iter().any(|action| action.starts_with("api_replay.")));
        for rung in settings.discover.iter().chain(settings.read.iter()) {
            assert!(
                rung.actions.iter().all(|action| {
                    !action.starts_with("browser.") && !action.starts_with("api_replay.")
                }),
                "browser and replay defaults must remain availability-gated optional actions"
            );
        }

        let static_read = settings
            .read
            .iter()
            .find(|rung| rung.id == "public_static")
            .unwrap();
        assert!(static_read.actions.is_empty());
        assert!(static_read.optional_actions.is_empty());

        let mut static_http = reader_descriptor("static-http", "static_http.read");
        static_http.retrieval.priority = 100;
        let mut anydoc = reader_descriptor("document-markdown", "document_markdown.read");
        anydoc.retrieval.priority = 200;
        let mut poppler = reader_descriptor("pdf-text", "pdf_text.read");
        poppler.retrieval.priority = 300;
        let mut structured = reader_descriptor("structured", "structured_schema.read");
        structured.retrieval.priority = 400;

        assert_eq!(
            effective_action_ids(static_read, [&structured, &poppler, &anydoc, &static_http])
                .unwrap(),
            vec![
                "static_http.read",
                "document_markdown.read",
                "pdf_text.read",
                "structured_schema.read",
            ]
        );
    }

    #[test]
    fn lexical_goal_requires_relevance_and_source_diversity() {
        let candidate = |id: &str, host: &str, text: &str| ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: super::super::SourceIdentity::new("test", id).unwrap(),
            title: text.into(),
            cheap_text: text.into(),
            canonical_url: Some(format!("https://{host}/{id}")),
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: super::super::ContentProvenance {
                source_label: host.into(),
                source_url: None,
                retrieved_by: "test".into(),
            },
            metadata: BTreeMap::new(),
        };
        let candidates = vec![
            candidate("one", "a.test", "rust async runtime"),
            candidate("two", "b.test", "rust runtime guide"),
        ];
        let goal = DiscoveryEvidenceGoal {
            min_candidates: 2,
            min_relevant_candidates: 2,
            min_independent_sources: 2,
            relevance_threshold: 0.5,
        };
        assert!(discovery_goal_satisfied(&candidates, "rust runtime", &goal));
        assert!(!discovery_goal_satisfied(
            &candidates,
            "postgres index",
            &goal
        ));
    }

    #[test]
    fn source_native_actions_require_an_explicit_source_selection() {
        let descriptor = discovery_descriptor(
            "reddit",
            "reddit.discover",
            RetrievalRung::SourceNative,
            false,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        assert!(!action_allowed(&need, &descriptor));
        need.allowed_actions = vec!["reddit.discover".into()];
        assert!(action_allowed(&need, &descriptor));
    }

    #[test]
    fn read_actions_are_selected_by_requested_output_and_known_media_type() {
        let mut html = reader_descriptor("html", "html.read");
        html.retrieval.accepted_media_types = vec!["text/html".into()];
        let mut pdf = reader_descriptor("pdf", "pdf.read");
        pdf.retrieval.accepted_media_types = vec!["application/pdf".into()];
        let mut structured = reader_descriptor("structured", "structured.read");
        structured.retrieval.outputs =
            vec![RetrievalOutputKind::Gist, RetrievalOutputKind::Structured];
        structured.retrieval.accepted_media_types = vec!["text/html".into()];

        let mut selected = candidate("search", "docs.test");
        selected.canonical_url = Some("https://docs.test/reference.pdf".into());
        let mut need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: selected,
                selection_receipt: None,
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: None,
            }),
            freshness: FreshnessPolicy::CachedOk,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::PublicRemoteRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };
        assert!(!action_allowed(&need, &html));
        assert!(action_allowed(&need, &pdf));
        assert!(!action_allowed(&need, &structured));

        let RetrievalTarget::Candidate { candidate, .. } = &mut need.target else {
            unreachable!();
        };
        candidate.canonical_url = Some("https://docs.test/article".into());
        need.goal = EvidenceGoal::Read(ReadEvidenceGoal {
            depth: ReadDepth::Gist,
            output: Some(RetrievalOutputKind::Structured),
            required_metadata: Vec::new(),
            min_chars: None,
        });
        assert!(!action_allowed(&need, &html));
        assert!(!action_allowed(&need, &pdf));
        assert!(action_allowed(&need, &structured));

        need.allowed_actions = vec!["html.read".into()];
        let catalog = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "revision".into(),
            discovery: Vec::new(),
            readers: vec![html],
            unavailable: Vec::new(),
        };
        let settings = ProgressiveRetrievalSettings {
            discover: Vec::new(),
            read: vec![rung(
                "public_static",
                RetrievalRungMode::Sequential,
                &["html.read"],
                &[],
            )],
            ..ProgressiveRetrievalSettings::default()
        };
        assert!(validate_allowed_actions(&need, &catalog, &settings)
            .unwrap_err()
            .to_string()
            .contains("not eligible"));
    }

    #[test]
    fn authority_matrix_never_treats_authenticated_read_as_interaction_permission() {
        assert!(authority_allows(
            RetrievalAuthority::AuthenticatedRead,
            RetrievalAuthority::PublicBrowserRead
        ));
        assert!(!authority_allows(
            RetrievalAuthority::AuthenticatedRead,
            RetrievalAuthority::PublicBrowserInteract
        ));
        assert!(authority_allows(
            RetrievalAuthority::AuthenticatedInteract,
            RetrievalAuthority::PublicBrowserInteract
        ));
    }

    #[test]
    fn quality_gate_rejects_login_js_and_error_shells() {
        let candidate = ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: super::super::SourceIdentity::new("test", "one").unwrap(),
            title: "Page".into(),
            cheap_text: "Sign in to continue. Please enable JavaScript to view this page.".into(),
            canonical_url: Some("https://example.com/one".into()),
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: super::super::ContentProvenance {
                source_label: "Example".into(),
                source_url: None,
                retrieved_by: "test".into(),
            },
            metadata: BTreeMap::new(),
        };
        let quality = evaluate_document(
            &inline_document(&candidate),
            &ReadEvidenceGoal {
                depth: ReadDepth::Gist,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(20),
            },
        );
        assert!(!quality.sufficient);
        assert!(quality
            .findings
            .iter()
            .any(|finding| finding.code == "login_wall"));
        assert!(quality
            .findings
            .iter()
            .any(|finding| finding.code == "javascript_shell"));
    }

    #[test]
    fn quality_gate_rejects_truncated_low_confidence_and_repeated_boilerplate() {
        let repeated = "Home navigation\n".repeat(8);
        let mut candidate = candidate("test", "quality.test");
        candidate.cheap_text = repeated;
        let mut document = inline_document(&candidate);
        document
            .metadata
            .insert("extraction_truncated".into(), Value::Bool(true));
        document.metadata.insert(
            "extraction_quality".into(),
            serde_json::json!({"score": 0.2}),
        );
        let quality = evaluate_document(
            &document,
            &ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(40),
            },
        );
        let codes = quality
            .findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect::<BTreeSet<_>>();
        assert!(codes.contains("truncated"));
        assert!(codes.contains("low_extraction_confidence"));
        assert!(codes.contains("boilerplate_repetition"));
    }

    #[test]
    fn structured_quality_requires_non_empty_valid_json() {
        let mut valid = inline_document(&candidate("test", "schema.test"));
        valid.text = serde_json::json!([{
            "name": "retrieval",
            "version": "one",
            "author": "team",
            "date": "today",
            "description": "bounded structured evidence",
            "url": "https://schema.test",
            "category": "research",
            "language": "rust"
        }])
        .to_string();
        let goal = ReadEvidenceGoal {
            depth: ReadDepth::Gist,
            output: Some(RetrievalOutputKind::Structured),
            required_metadata: Vec::new(),
            min_chars: Some(20),
        };
        assert!(evaluate_document(&valid, &goal).sufficient);

        valid.text =
            "well formed prose with many distinct words but no structured JSON payload".repeat(3);
        let quality = evaluate_document(&valid, &goal);
        assert!(!quality.sufficient);
        assert!(quality
            .findings
            .iter()
            .any(|finding| finding.code == "structured_schema_invalid"));
    }

    #[test]
    fn candidate_merge_deduplicates_canonical_urls_and_content_hashes() {
        let first = candidate("first", "same.test");
        let mut same_url = candidate("second", "same.test");
        same_url.identity.item_id = "two".into();
        let mut same_content = candidate("third", "different.test");
        same_content.content_hash = Some("a".repeat(64));
        let mut syndicated = candidate("fourth", "syndicated.test");
        syndicated.content_hash = same_content.content_hash.clone();

        let merged = merge_candidates(vec![first], vec![same_url, same_content, syndicated], 10);

        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn versioned_offline_quality_eval_corpus_matches_expected_classifications() {
        let corpus: OfflineEvalCorpus = serde_json::from_str(include_str!(
            "../../../data/magician_v2/content_retrieval_eval_v1.json"
        ))
        .unwrap();
        assert_eq!(corpus.schema_version, 1);
        for case in corpus.read_cases {
            let candidate = ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new("eval", &case.id).unwrap(),
                title: case.id.clone(),
                cheap_text: case.text,
                canonical_url: Some(format!("https://example.test/{}", case.id)),
                published_at_ms: None,
                observed_at_ms: 1,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "eval".into(),
                    source_url: None,
                    retrieved_by: "eval".into(),
                },
                metadata: BTreeMap::new(),
            };
            let mut document = inline_document(&candidate);
            document.media_type = case.media_type;
            document.metadata = case.metadata;
            let quality = evaluate_document(
                &document,
                &ReadEvidenceGoal {
                    depth: case.depth,
                    output: case.output,
                    required_metadata: Vec::new(),
                    min_chars: Some(40),
                },
            );
            assert_eq!(
                classify_document_quality(&quality),
                case.expected,
                "read eval case {}",
                case.id
            );
        }
        for case in corpus.discovery_cases {
            let candidate = ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new("eval", &case.id).unwrap(),
                title: case.title,
                cheap_text: case.snippet,
                canonical_url: Some(format!("https://example.test/{}", case.id)),
                published_at_ms: None,
                observed_at_ms: 1,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "eval".into(),
                    source_url: None,
                    retrieved_by: "eval".into(),
                },
                metadata: BTreeMap::new(),
            };
            let score = candidate_relevance(&candidate, &case.query);
            if let Some(minimum) = case.minimum_score {
                assert!(
                    score >= minimum,
                    "discovery eval case {} scored {score}",
                    case.id
                );
            }
            if let Some(maximum) = case.maximum_score {
                assert!(
                    score <= maximum,
                    "discovery eval case {} scored {score}",
                    case.id
                );
            }
        }
        for case in corpus.merge_cases {
            let inputs = case
                .inputs
                .into_iter()
                .map(|input| {
                    let mut candidate = candidate(&input.adapter_id, "merge.test");
                    candidate.identity =
                        SourceIdentity::new(&input.adapter_id, &input.item_id).unwrap();
                    candidate.canonical_url = Some(input.url);
                    candidate.content_hash = input.content_hash;
                    candidate
                })
                .collect::<Vec<_>>();
            assert_eq!(
                merge_candidates(Vec::new(), inputs, 100).len(),
                case.expected_unique,
                "merge eval case {}",
                case.id
            );
        }
    }

    #[tokio::test]
    async fn versioned_ladder_eval_is_non_inferior_to_direct_first_provider() {
        let corpus: OfflineEvalCorpus = serde_json::from_str(include_str!(
            "../../../data/magician_v2/content_retrieval_eval_v1.json"
        ))
        .unwrap();
        let mut controller_completed = 0usize;
        let mut direct_completed = 0usize;
        for case in corpus.ladder_cases {
            let first_calls = Arc::new(AtomicUsize::new(0));
            let second_calls = Arc::new(AtomicUsize::new(0));
            let controller = controller_with(
                vec![
                    scripted_adapter(
                        discovery_descriptor(
                            "eval-first",
                            "eval_first.discover",
                            RetrievalRung::PublicSearch,
                            false,
                        ),
                        eval_discovery_behavior(&case.first),
                        0,
                        Arc::clone(&first_calls),
                        Arc::new(Mutex::new(Vec::new())),
                        Arc::new(AtomicUsize::new(0)),
                        Arc::new(AtomicUsize::new(0)),
                    ),
                    scripted_adapter(
                        discovery_descriptor(
                            "eval-second",
                            "eval_second.discover",
                            RetrievalRung::PublicSearch,
                            false,
                        ),
                        eval_discovery_behavior(&case.second),
                        0,
                        Arc::clone(&second_calls),
                        Arc::new(Mutex::new(Vec::new())),
                        Arc::new(AtomicUsize::new(0)),
                        Arc::new(AtomicUsize::new(0)),
                    ),
                ],
                rung(
                    "public_search",
                    RetrievalRungMode::Sequential,
                    &["eval_first.discover", "eval_second.discover"],
                    &[],
                ),
                1,
            );
            let result = controller
                .retrieve(
                    discovery_need(RemoteDataPolicy::Allow, case.min_candidates),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let complete = result.status == RetrievalStatus::Complete;
            let direct_complete = case.first == "one" && case.min_candidates == 1;
            assert_eq!(
                result.status, case.expected_status,
                "controller status drift for {}",
                case.id
            );
            assert_eq!(
                result
                    .attempts
                    .iter()
                    .map(|attempt| attempt.classification)
                    .collect::<Vec<_>>(),
                case.expected_attempt_classifications,
                "attempt classification drift for {}",
                case.id
            );
            assert_eq!(
                direct_complete, case.expected_direct_complete,
                "direct baseline drift for {}",
                case.id
            );
            assert_eq!(
                first_calls.load(Ordering::SeqCst) + second_calls.load(Ordering::SeqCst),
                case.expected_calls,
                "remote-call count drift for {}",
                case.id
            );
            for selected in &result.candidates {
                assert!(!selected.candidate.provenance.source_label.trim().is_empty());
                assert!(selected.candidate.canonical_url.is_some());
                assert!(!selected.selection_receipt.id.trim().is_empty());
            }
            controller_completed += usize::from(complete);
            direct_completed += usize::from(direct_complete);
        }
        assert!(controller_completed >= direct_completed);
    }

    fn eval_discovery_behavior(value: &str) -> DiscoveryBehavior {
        match value {
            "empty" => DiscoveryBehavior::Empty,
            "one" => DiscoveryBehavior::One,
            "provider_unavailable" => DiscoveryBehavior::Error("provider unavailable"),
            "rate_limited" => DiscoveryBehavior::Error("status 429"),
            "policy_denied" => DiscoveryBehavior::Error("policy denied"),
            "authentication_required" => DiscoveryBehavior::Error("status 401"),
            "invalid_request" => DiscoveryBehavior::Error("invalid request payload"),
            other => panic!("unknown ladder eval behavior {other}"),
        }
    }

    #[test]
    fn duplicate_and_mismatched_ladder_actions_fail_closed() {
        let descriptor = ContentSourceDescriptor {
            adapter_id: "search".into(),
            display_name: "Search".into(),
            class: super::super::ContentSourceClass::WebSearch,
            capabilities: super::super::ContentSourceCapabilities {
                discovery: true,
                full_content: false,
                cursor: false,
                conditional_fetch: false,
                execution: super::super::AdapterExecution::RemoteEndpoint,
                auth: super::super::AdapterAuth::None,
                sends_user_intent: true,
                metered: false,
            },
            retrieval: super::super::RetrievalActionMetadata::discovery(
                "search.discover",
                RetrievalRung::PublicSearch,
                RetrievalAuthority::PublicRemoteRead,
                true,
            ),
        };
        let catalog = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "r".into(),
            discovery: vec![descriptor],
            readers: Vec::new(),
            unavailable: Vec::new(),
        };
        let mut settings = ProgressiveRetrievalSettings::default();
        settings.discover = vec![rung(
            "public_search",
            RetrievalRungMode::Sequential,
            &["search.discover", "search.discover"],
            &[],
        )];
        settings.read = vec![rung(
            "public_static",
            RetrievalRungMode::Sequential,
            &[],
            &["missing.read"],
        )];
        assert!(validate_settings(&settings, &catalog).is_err());
    }

    #[tokio::test]
    async fn sequential_fallback_preserves_configured_order_and_stops_at_goal() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let third_calls = Arc::new(AtomicUsize::new(0));
        let adapters = vec![
            scripted_adapter(
                discovery_descriptor("a", "a.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::Empty,
                0,
                Arc::clone(&first_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ),
            scripted_adapter(
                discovery_descriptor("b", "b.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&second_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ),
            scripted_adapter(
                discovery_descriptor("c", "c.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&third_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ),
        ];
        let controller = controller_with(
            adapters,
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["a.discover", "b.discover", "c.discover"],
                &[],
            ),
            1,
        );
        let result = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(*order.lock().unwrap(), vec!["a.discover", "b.discover"]);
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(third_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn discovery_attempt_timeout_falls_through_before_the_request_deadline() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = super::super::ContentSourceRegistry::new();
        registry
            .register_discovery(scripted_adapter(
                discovery_descriptor("slow", "slow.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                200,
                Arc::clone(&first_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ))
            .unwrap();
        registry
            .register_discovery(scripted_adapter(
                discovery_descriptor(
                    "fallback",
                    "fallback.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&second_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ))
            .unwrap();
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            default_deadline_ms: 200,
            discovery_attempt_timeout_ms: 20,
            max_attempts: 2,
            max_parallel_actions: 1,
            discover: vec![rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["slow.discover", "fallback.discover"],
                &[],
            )],
            read: Vec::new(),
            ..ProgressiveRetrievalSettings::default()
        };
        let controller = service.retrieval_controller(settings);

        let result = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(
            *order.lock().unwrap(),
            vec!["slow.discover", "fallback.discover"]
        );
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result
                .attempts
                .iter()
                .map(|attempt| attempt.classification)
                .collect::<Vec<_>>(),
            vec![
                RetrievalClassification::ProviderUnavailable,
                RetrievalClassification::Sufficient,
            ]
        );
        assert!(result.attempts[0]
            .failure_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("timed out after 20 ms")));
    }

    #[test]
    fn discovery_attempt_timeout_must_be_positive() {
        let mut settings = ProgressiveRetrievalSettings::default();
        settings.discovery_attempt_timeout_ms = 0;
        assert!(settings.validate_bounds().is_err());
    }

    #[tokio::test]
    async fn policy_denial_stops_before_later_provider_execution() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let adapters = vec![
            scripted_adapter(
                discovery_descriptor("a", "a.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&first_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ),
            scripted_adapter(
                discovery_descriptor("b", "b.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&second_calls),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ),
        ];
        let controller = controller_with(
            adapters,
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["a.discover", "b.discover"],
                &[],
            ),
            1,
        );
        let result = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Deny, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.status, RetrievalStatus::Failed);
        assert_eq!(
            result.attempts[0].classification,
            RetrievalClassification::PolicyDenied
        );
        assert_eq!(first_calls.load(Ordering::SeqCst), 0);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn eligible_parallel_wave_never_exceeds_configured_concurrency() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut adapters = Vec::new();
        let mut actions = Vec::new();
        for index in 0..4 {
            let adapter_id = format!("p{index}");
            let action_id = format!("p{index}.discover");
            actions.push(action_id.clone());
            adapters.push(scripted_adapter(
                discovery_descriptor(&adapter_id, &action_id, RetrievalRung::SourceNative, false),
                DiscoveryBehavior::One,
                20,
                Arc::new(AtomicUsize::new(0)),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ));
        }
        let action_refs = actions.iter().map(String::as_str).collect::<Vec<_>>();
        let controller = controller_with(
            adapters,
            rung(
                "source_native",
                RetrievalRungMode::EligibleParallel,
                &action_refs,
                &[],
            ),
            2,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 4);
        need.allowed_actions = actions.clone();
        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert_eq!(result.attempts.len(), 4);
        assert_eq!(
            result
                .attempts
                .iter()
                .map(|attempt| attempt.configured_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[tokio::test]
    async fn eligible_parallel_stops_before_dispatching_a_second_unneeded_wave() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let calls = (0..4)
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect::<Vec<_>>();
        let mut adapters = Vec::new();
        let mut actions = Vec::new();
        for (index, call_count) in calls.iter().enumerate() {
            let adapter_id = format!("wave{index}");
            let action_id = format!("wave{index}.discover");
            actions.push(action_id.clone());
            adapters.push(scripted_adapter(
                discovery_descriptor(&adapter_id, &action_id, RetrievalRung::SourceNative, false),
                DiscoveryBehavior::One,
                5,
                Arc::clone(call_count),
                Arc::clone(&order),
                Arc::clone(&active),
                Arc::clone(&max_active),
            ));
        }
        let action_refs = actions.iter().map(String::as_str).collect::<Vec<_>>();
        let controller = controller_with(
            adapters,
            rung(
                "source_native",
                RetrievalRungMode::EligibleParallel,
                &action_refs,
                &[],
            ),
            2,
        );

        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.allowed_actions = actions.clone();
        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(result.attempts.len(), 2);
        assert_eq!(order.lock().unwrap().len(), 2);
        assert_eq!(
            calls
                .iter()
                .map(|calls| calls.load(Ordering::SeqCst))
                .sum::<usize>(),
            2
        );
    }

    #[tokio::test]
    async fn explicit_budget_skips_metered_action_before_provider_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let descriptor = discovery_descriptor(
            "metered",
            "metered.discover",
            RetrievalRung::PublicSearch,
            true,
        );
        let controller = controller_with(
            vec![scripted_adapter(
                descriptor,
                DiscoveryBehavior::One,
                0,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["metered.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.cost_budget_microunits.insert("credit".into(), 999_999);
        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.status, RetrievalStatus::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(result.attempts.len(), 1);
        assert_eq!(
            result.attempts[0].classification,
            RetrievalClassification::BudgetExhausted
        );
    }

    #[tokio::test]
    async fn sequential_budget_is_recomputed_after_an_uncharged_fallback() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let controller = controller_with(
            vec![
                scripted_adapter(
                    discovery_descriptor(
                        "first",
                        "first.discover",
                        RetrievalRung::PublicSearch,
                        true,
                    ),
                    DiscoveryBehavior::Empty,
                    0,
                    Arc::clone(&first_calls),
                    Arc::clone(&order),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
                scripted_adapter(
                    discovery_descriptor(
                        "second",
                        "second.discover",
                        RetrievalRung::PublicSearch,
                        true,
                    ),
                    DiscoveryBehavior::One,
                    0,
                    Arc::clone(&second_calls),
                    Arc::clone(&order),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
            ],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["first.discover", "second.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.cost_budget_microunits
            .insert("credit".into(), 1_000_000);

        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.total_cost_microunits.get("credit"), Some(&1_000_000));
        assert_eq!(
            order.lock().unwrap().as_slice(),
            ["first.discover", "second.discover"]
        );
    }

    #[tokio::test]
    async fn unknown_allowed_action_fails_before_provider_execution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor(
                    "known",
                    "known.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["known.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.allowed_actions = vec!["typo.discover".into()];

        let error = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("unknown allowed retrieval action"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn allowed_action_may_narrow_to_an_auto_enrolled_skill_action() {
        let configured_calls = Arc::new(AtomicUsize::new(0));
        let auto_enrolled_calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![
                scripted_adapter(
                    discovery_descriptor(
                        "configured",
                        "configured.discover",
                        RetrievalRung::PublicSearch,
                        false,
                    ),
                    DiscoveryBehavior::One,
                    0,
                    Arc::clone(&configured_calls),
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
                scripted_adapter(
                    discovery_descriptor(
                        "auto-enrolled",
                        "auto-enrolled.discover",
                        RetrievalRung::PublicSearch,
                        false,
                    ),
                    DiscoveryBehavior::One,
                    0,
                    Arc::clone(&auto_enrolled_calls),
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
            ],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["configured.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.allowed_actions = vec!["auto-enrolled.discover".into()];

        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.attempts.len(), 1);
        assert_eq!(result.attempts[0].action_id, "auto-enrolled.discover");
        assert_eq!(configured_calls.load(Ordering::SeqCst), 0);
        assert_eq!(auto_enrolled_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn configuration_bounds_optional_action_count() {
        let settings = ProgressiveRetrievalSettings {
            discover: vec![RetrievalRungConfig {
                id: "public_search".into(),
                mode: RetrievalRungMode::Sequential,
                actions: Vec::new(),
                optional_actions: (0..=MAX_RETRIEVAL_ACTIONS)
                    .map(|index| format!("optional_{index}.discover"))
                    .collect(),
            }],
            read: Vec::new(),
            ..ProgressiveRetrievalSettings::default()
        };
        let catalog = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "revision".into(),
            discovery: Vec::new(),
            readers: Vec::new(),
            unavailable: Vec::new(),
        };
        assert!(validate_settings(&settings, &catalog)
            .unwrap_err()
            .to_string()
            .contains("configured actions"));
    }

    #[tokio::test]
    async fn attempt_bound_stops_before_later_configured_actions() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let adapters = vec![
            scripted_adapter(
                discovery_descriptor(
                    "first",
                    "first.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::Empty,
                0,
                Arc::clone(&first_calls),
                Arc::clone(&order),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            ),
            scripted_adapter(
                discovery_descriptor(
                    "second",
                    "second.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&second_calls),
                Arc::clone(&order),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            ),
        ];
        let controller = controller_with(
            adapters,
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["first.discover", "second.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.max_attempts = Some(1);

        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Failed);
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
        assert_eq!(result.attempts.len(), 1);
    }

    #[tokio::test]
    async fn provider_failure_opens_and_recovers_its_circuit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor(
                    "flaky",
                    "flaky.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::Error("provider unavailable"),
                0,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["flaky.discover"],
                &[],
            ),
            1,
        );

        for _ in 0..3 {
            controller
                .retrieve(
                    discovery_need(RemoteDataPolicy::Allow, 1),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
        }
        let open = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            open.attempts[0].classification,
            RetrievalClassification::CircuitOpen
        );

        controller
            .state
            .circuits
            .lock()
            .unwrap()
            .get_mut("flaky.discover")
            .unwrap()
            .opened_at_ms = Some(0);
        controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn cancellation_and_deadline_return_bounded_results() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor("slow", "slow.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::Error("should be cancelled first"),
                100,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["slow.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 1);
        need.deadline_ms = Some(5);
        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.status, RetrievalStatus::Failed);
        assert_eq!(
            result.attempts[0].classification,
            RetrievalClassification::DeadlineExceeded
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_before_dispatch_does_not_call_a_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor("slow", "slow.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                100,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["slow.discover"],
                &[],
            ),
            1,
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = controller
            .retrieve(discovery_need(RemoteDataPolicy::Allow, 1), cancellation)
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Cancelled);
        assert!(result.attempts.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn in_flight_discovery_cancellation_returns_a_traceable_cancelled_result() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor("slow", "slow.discover", RetrievalRung::PublicSearch, false),
                DiscoveryBehavior::One,
                1_000,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["slow.discover"],
                &[],
            ),
            1,
        );
        let cancellation = CancellationToken::new();
        let cancel_after_dispatch = cancellation.clone();
        let cancellation_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel_after_dispatch.cancel();
        });

        let result = controller
            .retrieve(discovery_need(RemoteDataPolicy::Allow, 1), cancellation)
            .await
            .unwrap();
        cancellation_task.await.unwrap();

        assert_eq!(result.status, RetrievalStatus::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.attempts.len(), 1);
        assert_eq!(
            result.attempts[0].classification,
            RetrievalClassification::Cancelled
        );
        assert_eq!(
            result.attempts[0].escalation_reason.as_deref(),
            Some("cancelled")
        );
        assert!(!result.trace_id.is_empty());
        assert!(result.candidates.is_empty());
    }

    #[tokio::test]
    async fn useful_partial_discovery_survives_a_later_deadline() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![
                scripted_adapter(
                    discovery_descriptor(
                        "first",
                        "first.discover",
                        RetrievalRung::PublicSearch,
                        false,
                    ),
                    DiscoveryBehavior::One,
                    0,
                    Arc::clone(&first_calls),
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
                scripted_adapter(
                    discovery_descriptor(
                        "second",
                        "second.discover",
                        RetrievalRung::PublicSearch,
                        false,
                    ),
                    DiscoveryBehavior::One,
                    1_000,
                    Arc::clone(&second_calls),
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
            ],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["first.discover", "second.discover"],
                &[],
            ),
            1,
        );
        let mut need = discovery_need(RemoteDataPolicy::Allow, 2);
        need.deadline_ms = Some(25);

        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Degraded);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result
                .attempts
                .iter()
                .map(|attempt| attempt.classification)
                .collect::<Vec<_>>(),
            vec![
                RetrievalClassification::Sufficient,
                RetrievalClassification::DeadlineExceeded,
            ]
        );
        assert_eq!(
            result.attempts[1].escalation_reason.as_deref(),
            Some("deadline_exceeded")
        );
        assert!(!result.candidates[0].selection_receipt.id.is_empty());
    }

    #[test]
    fn action_fingerprint_is_stable_across_traces_and_execution_order() {
        assert_eq!(
            action_fingerprint("trace-a", "public_search", "exa.discover", 0),
            action_fingerprint("trace-b", "public_search", "exa.discover", 7)
        );
        assert_ne!(
            action_fingerprint("trace-a", "public_search", "exa.discover", 0),
            action_fingerprint("trace-a", "source_native", "exa.discover", 0)
        );
    }

    #[test]
    fn provider_errors_map_to_stable_controller_classifications() {
        for (message, expected) in [
            (
                "permission policy denied",
                RetrievalClassification::PolicyDenied,
            ),
            (
                "status 401",
                RetrievalClassification::AuthenticationRequired,
            ),
            (
                "status 403",
                RetrievalClassification::AuthenticationRequired,
            ),
            (
                "authentication required: status 401",
                RetrievalClassification::AuthenticationRequired,
            ),
            (
                "static extraction returned a non-content shell: paywall",
                RetrievalClassification::AuthenticationRequired,
            ),
            (
                "static extraction returned a non-content shell: javascript_shell",
                RetrievalClassification::JavascriptRequired,
            ),
            ("status 429", RetrievalClassification::RateLimited),
            ("status 404", RetrievalClassification::NotFound),
            ("invalid request", RetrievalClassification::InvalidRequest),
            (
                "connection reset",
                RetrievalClassification::ProviderUnavailable,
            ),
            (
                "certificate verify failed",
                RetrievalClassification::ProviderMisconfigured,
            ),
            // The broad `ssl`/`tls` tokens sit below every more specific arm, so
            // a message carrying both signals still classifies by the specific
            // one.
            (
                "status 401 from the tls endpoint",
                RetrievalClassification::AuthenticationRequired,
            ),
            (
                "invalid ssl request",
                RetrievalClassification::InvalidRequest,
            ),
            // A topic token alone must not convict. `ProviderMisconfigured` is
            // non-retryable, so a transient failure against a host whose name
            // merely contains `ssl` would otherwise cost the ladder a rung for
            // the life of the circuit.
            (
                "connection reset by peer while fetching https://ssllabs.com/analyze",
                RetrievalClassification::ProviderUnavailable,
            ),
            (
                "timed out reading from api.tlsfingerprint.io",
                RetrievalClassification::ProviderUnavailable,
            ),
            // A real trust failure still convicts on topic + failure token.
            (
                "ssl handshake aborted by peer",
                RetrievalClassification::ProviderMisconfigured,
            ),
        ] {
            assert_eq!(classify_error(&anyhow::anyhow!(message)), expected);
        }

        let wrapped = anyhow::anyhow!("status 401").context("running discovery adapter `remote`");
        assert_eq!(
            classify_error(&wrapped),
            RetrievalClassification::AuthenticationRequired
        );
    }

    #[test]
    fn public_discovery_provider_failures_never_masquerade_as_user_auth_or_input() {
        let mut descriptor = discovery_descriptor(
            "provider",
            "provider.discover",
            RetrievalRung::PublicSearch,
            false,
        );
        descriptor.capabilities.auth = AdapterAuth::Required;

        for (message, expected) in [
            ("status 429", RetrievalClassification::RateLimited),
            (
                "API key rate limit exceeded: status 429",
                RetrievalClassification::RateLimited,
            ),
            (
                "credential policy denied",
                RetrievalClassification::PolicyDenied,
            ),
            (
                "capability output blocked by policy",
                RetrievalClassification::PolicyDenied,
            ),
            (
                "invalid query requires a non-empty term",
                RetrievalClassification::InvalidRequest,
            ),
            ("status 500", RetrievalClassification::ProviderUnavailable),
            (
                "capability `provider` returned invalid JSON",
                RetrievalClassification::ProviderMisconfigured,
            ),
            (
                "required credential PROVIDER_API_KEY is missing",
                RetrievalClassification::ProviderMisconfigured,
            ),
            (
                "provider request failed with status 401",
                RetrievalClassification::ProviderMisconfigured,
            ),
        ] {
            assert_eq!(
                classify_discovery_error(&anyhow::anyhow!(message), &descriptor),
                expected,
                "classification drift for {message}"
            );
        }

        let mut public_without_provider_auth = descriptor.clone();
        public_without_provider_auth.capabilities.auth = AdapterAuth::None;
        assert_eq!(
            classify_discovery_error(
                &anyhow::anyhow!("public endpoint returned status 401"),
                &public_without_provider_auth,
            ),
            RetrievalClassification::AuthenticationRequired
        );

        descriptor.retrieval.authority = RetrievalAuthority::AuthenticatedRead;
        assert_eq!(
            classify_discovery_error(&anyhow::anyhow!("status 401"), &descriptor),
            RetrievalClassification::AuthenticationRequired
        );
    }

    #[tokio::test]
    async fn public_provider_faults_fall_through_to_the_next_discovery_action() {
        for (message, expected) in [
            ("status 429", RetrievalClassification::RateLimited),
            ("status 500", RetrievalClassification::ProviderUnavailable),
            (
                "capability `provider` returned invalid JSON",
                RetrievalClassification::ProviderMisconfigured,
            ),
            (
                "required credential PROVIDER_API_KEY is missing",
                RetrievalClassification::ProviderMisconfigured,
            ),
            (
                "provider request failed with status 401",
                RetrievalClassification::ProviderMisconfigured,
            ),
        ] {
            let first_calls = Arc::new(AtomicUsize::new(0));
            let second_calls = Arc::new(AtomicUsize::new(0));
            let order = Arc::new(Mutex::new(Vec::new()));
            let active = Arc::new(AtomicUsize::new(0));
            let max_active = Arc::new(AtomicUsize::new(0));
            let mut provider = discovery_descriptor(
                "provider",
                "provider.discover",
                RetrievalRung::PublicSearch,
                false,
            );
            provider.capabilities.auth = AdapterAuth::Required;
            let controller = controller_with(
                vec![
                    scripted_adapter(
                        provider,
                        DiscoveryBehavior::Error(message),
                        0,
                        Arc::clone(&first_calls),
                        Arc::clone(&order),
                        Arc::clone(&active),
                        Arc::clone(&max_active),
                    ),
                    scripted_adapter(
                        discovery_descriptor(
                            "fallback",
                            "fallback.discover",
                            RetrievalRung::PublicSearch,
                            false,
                        ),
                        DiscoveryBehavior::One,
                        0,
                        Arc::clone(&second_calls),
                        Arc::clone(&order),
                        Arc::clone(&active),
                        Arc::clone(&max_active),
                    ),
                ],
                rung(
                    "public_search",
                    RetrievalRungMode::Sequential,
                    &["provider.discover", "fallback.discover"],
                    &[],
                ),
                1,
            );

            let result = controller
                .retrieve(
                    discovery_need(RemoteDataPolicy::Allow, 1),
                    CancellationToken::new(),
                )
                .await
                .unwrap();

            assert_eq!(result.status, RetrievalStatus::Complete, "fault: {message}");
            assert_eq!(
                result.attempts[0].classification, expected,
                "fault: {message}"
            );
            assert_eq!(
                result.attempts[1].classification,
                RetrievalClassification::Sufficient,
                "fault: {message}"
            );
            assert_eq!(first_calls.load(Ordering::SeqCst), 1);
            assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        }
    }

    /// The exact shape a missing CA bundle produces: the governed adapter exits
    /// non-zero, its stderr envelope becomes the step error, and the controller
    /// classifies that text. Reporting it as a provider outage is what made the
    /// 2026-08-13 zero-item run invisible — a broken trust store is an operator
    /// misconfiguration that no amount of retrying repairs.
    const TLS_TRUST_FAILURE: &str = "invoking capability `semantic-websearch-via-exa` for \
                                     discovery adapter `exa`: {\"error\": {\"kind\": \
                                     \"URLError\", \"message\": \"<urlopen error [SSL: \
                                     CERTIFICATE_VERIFY_FAILED] certificate verify failed: unable \
                                     to get local issuer certificate (_ssl.c:1028)>\"}}";

    #[test]
    fn classify_error_separates_a_trust_store_failure_from_a_provider_outage() {
        assert_eq!(
            classify_error(&anyhow::anyhow!(TLS_TRUST_FAILURE)),
            RetrievalClassification::ProviderMisconfigured
        );
        assert_ne!(
            classify_error(&anyhow::anyhow!(TLS_TRUST_FAILURE)),
            RetrievalClassification::ProviderUnavailable
        );
    }

    #[tokio::test]
    async fn a_trust_store_misconfiguration_is_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with(
            vec![scripted_adapter(
                discovery_descriptor(
                    "misconfigured",
                    "misconfigured.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::Error(TLS_TRUST_FAILURE),
                0,
                Arc::clone(&calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            )],
            rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["misconfigured.discover"],
                &[],
            ),
            1,
        );

        let first = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let second = controller
            .retrieve(
                discovery_need(RemoteDataPolicy::Allow, 1),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            first.attempts[0].classification,
            RetrievalClassification::ProviderMisconfigured
        );
        assert_ne!(
            first.attempts[0].classification,
            RetrievalClassification::ProviderUnavailable
        );
        // One call, not the three a transient outage is allowed before its
        // circuit opens: the second retrieval must fast-fail on the open circuit.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second.attempts[0].classification,
            RetrievalClassification::CircuitOpen
        );

        // The cause has to reach the receipt, or the telemetry line is still
        // just `classification=... returned_items=0` with nothing to act on.
        let detail = first.attempts[0]
            .failure_detail
            .as_deref()
            .expect("a failed attempt must carry its cause");
        assert!(
            detail.contains("CERTIFICATE_VERIFY_FAILED"),
            "cause text lost the only diagnostic token it had: {detail}"
        );
        assert!(detail.chars().count() <= MAX_FAILURE_DETAIL_CHARS);
        assert_eq!(
            first.attempts[0].escalation_reason.as_deref(),
            Some("provider_misconfigured")
        );
        // An attempt that never reached the provider has no cause text to carry.
        assert_eq!(second.attempts[0].failure_detail, None);
    }

    #[tokio::test]
    async fn ambiguous_public_forbidden_falls_through_to_rendered_reader_without_identity() {
        let static_calls = Arc::new(AtomicUsize::new(0));
        let rendered_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = super::super::ContentSourceRegistry::new();
        registry
            .register_reader(Arc::new(FailingReader {
                descriptor: reader_descriptor("static-reader", "static.read"),
                calls: Arc::clone(&static_calls),
                error: "status 403",
            }))
            .unwrap();
        let mut rendered_descriptor = reader_descriptor("rendered-reader", "browser.headless.read");
        rendered_descriptor.retrieval.rung = RetrievalRung::PublicRendered;
        rendered_descriptor.retrieval.authority = RetrievalAuthority::PublicBrowserRead;
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: rendered_descriptor,
                calls: Arc::clone(&rendered_calls),
                text: "A complete public rendered document with enough authoritative information \
                       to satisfy the requested evidence goal without user identity or an \
                       authenticated session."
                    .into(),
                delay_ms: 0,
            }))
            .unwrap();
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            discover: Vec::new(),
            read: vec![
                rung(
                    "public_static",
                    RetrievalRungMode::Sequential,
                    &["static.read"],
                    &[],
                ),
                rung(
                    "public_rendered",
                    RetrievalRungMode::Sequential,
                    &["browser.headless.read"],
                    &[],
                ),
            ],
            ..ProgressiveRetrievalSettings::default()
        };
        let need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: candidate("search", "public.test"),
                selection_receipt: None,
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::PublicBrowserRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };

        let result = service
            .retrieval_controller(settings)
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Complete);
        assert_eq!(result.required_authority, None);
        assert_eq!(static_calls.load(Ordering::SeqCst), 1);
        assert_eq!(rendered_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result
                .attempts
                .iter()
                .map(|attempt| attempt.classification)
                .collect::<Vec<_>>(),
            vec![
                RetrievalClassification::JavascriptRequired,
                RetrievalClassification::Sufficient,
            ]
        );
    }

    #[tokio::test]
    async fn authentication_wall_stops_before_another_anonymous_reader() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let authenticated_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = super::super::ContentSourceRegistry::new();
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: reader_descriptor("first-reader", "first.read"),
                calls: Arc::clone(&first_calls),
                text: "Subscribe to continue. ".repeat(20),
                delay_ms: 0,
            }))
            .unwrap();
        let mut authenticated_descriptor =
            reader_descriptor("authenticated-reader", "browser.cdp.read");
        authenticated_descriptor.capabilities.auth = super::super::AdapterAuth::Required;
        authenticated_descriptor.retrieval.authority = RetrievalAuthority::AuthenticatedRead;
        authenticated_descriptor.retrieval.rung = RetrievalRung::Authenticated;
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: authenticated_descriptor,
                calls: Arc::clone(&authenticated_calls),
                text: "The authenticated reader must not run before approval.".into(),
                delay_ms: 0,
            }))
            .unwrap();
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: reader_descriptor("second-reader", "second.read"),
                calls: Arc::clone(&second_calls),
                text: "A complete public document with enough distinct information to satisfy the \
                       requested evidence goal without authentication or another transport."
                    .into(),
                delay_ms: 0,
            }))
            .unwrap();
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            discover: Vec::new(),
            read: vec![
                rung(
                    "public_static",
                    RetrievalRungMode::Sequential,
                    &["first.read", "second.read"],
                    &[],
                ),
                rung(
                    "authenticated",
                    RetrievalRungMode::Sequential,
                    &["browser.cdp.read"],
                    &[],
                ),
            ],
            ..ProgressiveRetrievalSettings::default()
        };
        let selected = candidate("search", "article.test");
        let need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: selected,
                selection_receipt: None,
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::AuthenticatedRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };

        let result = service
            .retrieval_controller(settings)
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::ApprovalRequired);
        assert_eq!(
            result.required_authority,
            Some(RetrievalAuthority::AuthenticatedRead)
        );
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
        assert_eq!(authenticated_calls.load(Ordering::SeqCst), 0);
        let handoff = result.handoff.unwrap();
        assert_eq!(handoff.requested_mode, "cdp");
        assert_eq!(handoff.action_id, "browser.cdp.read");
        assert!(handoff.requires_approval);
    }

    #[tokio::test]
    async fn in_flight_read_cancellation_stops_the_reader_and_preserves_attempt_metadata() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = super::super::ContentSourceRegistry::new();
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: reader_descriptor("slow-reader", "slow.read"),
                calls: Arc::clone(&calls),
                text: "This response must never complete after cancellation.".into(),
                delay_ms: 1_000,
            }))
            .unwrap();
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            discover: Vec::new(),
            read: vec![rung(
                "public_static",
                RetrievalRungMode::Sequential,
                &["slow.read"],
                &[],
            )],
            ..ProgressiveRetrievalSettings::default()
        };
        let need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: candidate("search", "cancel.test"),
                selection_receipt: None,
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::PublicRemoteRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };
        let cancellation = CancellationToken::new();
        let cancel_after_dispatch = cancellation.clone();
        let cancellation_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel_after_dispatch.cancel();
        });

        let result = service
            .retrieval_controller(settings)
            .retrieve(need, cancellation)
            .await
            .unwrap();
        cancellation_task.await.unwrap();

        assert_eq!(result.status, RetrievalStatus::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(result.document.is_none());
        assert_eq!(result.attempts.len(), 1);
        assert_eq!(result.attempts[0].action_id, "slow.read");
        assert_eq!(
            result.attempts[0].classification,
            RetrievalClassification::Cancelled
        );
        assert_eq!(
            result.attempts[0].escalation_reason.as_deref(),
            Some("cancelled")
        );
    }

    #[tokio::test]
    async fn only_receipt_verified_inline_snippets_can_satisfy_gist_without_a_reader() {
        let service = Arc::new(
            ContentAcquisitionService::new(
                "p",
                "w",
                "revision",
                super::super::ContentSourceRegistry::new(),
                Vec::new(),
            )
            .unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            discover: Vec::new(),
            read: Vec::new(),
            ..ProgressiveRetrievalSettings::default()
        };
        let mut selected = candidate("search", "inline.test");
        selected.cheap_text = "A sufficiently complete inline summary with distinct \
                               implementation details, public evidence, compatibility notes, \
                               citations, and concrete outcomes for a gist response."
            .into();
        let mut need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: selected.clone(),
                selection_receipt: None,
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::Gist,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::CachedOk,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::LocalOnly,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };
        let controller = service.retrieval_controller(settings);
        let issued = controller
            .issue_candidate_receipts(&need, "inline gist", vec![selected])
            .into_iter()
            .next()
            .unwrap();
        let RetrievalTarget::Candidate {
            selection_receipt, ..
        } = &mut need.target
        else {
            unreachable!()
        };
        *selection_receipt = Some(issued.selection_receipt.id);

        let result = controller
            .retrieve(need, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.status, RetrievalStatus::Complete);
        let document = result.document.unwrap();
        assert_eq!(
            document.metadata["evidence_role"],
            Value::String("discovery_only".into())
        );
        assert_eq!(document.metadata["claim_eligible"], Value::Bool(false));
        assert_eq!(
            document.metadata["fetch_status"],
            Value::String("not_fetched".into())
        );
        assert!(result.attempts.is_empty());

        let unverified_need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: issued.candidate,
                selection_receipt: Some("fabricated-or-expired-receipt".into()),
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::Gist,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::CachedOk,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            maximum_authority: RetrievalAuthority::PublicRemoteRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };
        let fallback = controller
            .retrieve(unverified_need, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fallback.status, RetrievalStatus::Failed);
        assert!(fallback.document.is_none());
        assert!(fallback.attempts.is_empty());
    }

    #[test]
    fn candidate_receipt_fingerprint_binds_inline_text_and_metadata() {
        let original = candidate("search", "inline.test");
        let original_fingerprint = candidate_fingerprint(&original);

        let mut changed_text = original.clone();
        changed_text.cheap_text = "model-generated replacement claims".into();
        assert_ne!(candidate_fingerprint(&changed_text), original_fingerprint);

        let mut changed_metadata = original;
        changed_metadata
            .metadata
            .insert("price".into(), Value::String("fabricated".into()));
        assert_ne!(
            candidate_fingerprint(&changed_metadata),
            original_fingerprint
        );
    }

    #[tokio::test]
    async fn selection_receipt_is_scope_revision_candidate_bound_and_single_use() {
        let discovery_calls = Arc::new(AtomicUsize::new(0));
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = super::super::ContentSourceRegistry::new();
        registry
            .register_discovery(scripted_adapter(
                discovery_descriptor(
                    "search",
                    "search.discover",
                    RetrievalRung::PublicSearch,
                    false,
                ),
                DiscoveryBehavior::One,
                0,
                Arc::clone(&discovery_calls),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            ))
            .unwrap();
        registry
            .register_reader(Arc::new(ScriptedReader {
                descriptor: ContentSourceDescriptor {
                    adapter_id: "reader".into(),
                    display_name: "Reader".into(),
                    class: ContentSourceClass::WebPage,
                    capabilities: ContentSourceCapabilities {
                        discovery: false,
                        full_content: true,
                        cursor: false,
                        conditional_fetch: true,
                        execution: AdapterExecution::LocalProcess,
                        auth: AdapterAuth::None,
                        sends_user_intent: false,
                        metered: false,
                    },
                    retrieval: super::super::RetrievalActionMetadata::reader(
                        "reader.read",
                        RetrievalRung::PublicStatic,
                        RetrievalAuthority::PublicRemoteRead,
                        vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
                    ),
                },
                calls: Arc::clone(&reader_calls),
                text: "A complete rust async runtime release document with implementation notes, \
                       compatibility guidance, migration details, and verified examples for users."
                    .into(),
                delay_ms: 0,
            }))
            .unwrap();
        let service = Arc::new(
            ContentAcquisitionService::new("p", "w", "revision", registry, Vec::new()).unwrap(),
        );
        let settings = ProgressiveRetrievalSettings {
            discover: vec![rung(
                "public_search",
                RetrievalRungMode::Sequential,
                &["search.discover"],
                &[],
            )],
            read: vec![rung(
                "public_static",
                RetrievalRungMode::Sequential,
                &["reader.read"],
                &[],
            )],
            ..ProgressiveRetrievalSettings::default()
        };
        let controller = service.retrieval_controller(settings);
        let mut discover_need = discovery_need(RemoteDataPolicy::Allow, 1);
        discover_need.invocation_source = ContentInvocationSource::UserFeed;
        let discovered = controller
            .retrieve(discover_need, CancellationToken::new())
            .await
            .unwrap();
        let selected = discovered.candidates.into_iter().next().unwrap();
        let read_need = RetrievalNeed {
            schema_version: RETRIEVAL_NEED_SCHEMA_VERSION,
            principal: "p".into(),
            workspace: "w".into(),
            operation: RetrievalOperation::Read,
            target: RetrievalTarget::Candidate {
                candidate: selected.candidate.clone(),
                selection_receipt: Some(selected.selection_receipt.id.clone()),
            },
            goal: EvidenceGoal::Read(ReadEvidenceGoal {
                depth: ReadDepth::FullText,
                output: None,
                required_metadata: Vec::new(),
                min_chars: Some(80),
            }),
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Deny,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::UserFeed,
            maximum_authority: RetrievalAuthority::PublicRemoteRead,
            authority_grant_id: None,
            deadline_ms: None,
            max_attempts: None,
            cost_budget_microunits: BTreeMap::new(),
            allowed_actions: Vec::new(),
        };
        let mut mismatched = read_need.clone();
        let RetrievalTarget::Candidate { candidate, .. } = &mut mismatched.target else {
            unreachable!()
        };
        candidate.identity = SourceIdentity::new("search", "different").unwrap();
        let mismatch = controller
            .retrieve(mismatched, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(mismatch.to_string().contains("does not match"));
        assert_eq!(reader_calls.load(Ordering::SeqCst), 0);

        let first = controller
            .retrieve(read_need.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(first.status, RetrievalStatus::Complete);
        assert_eq!(reader_calls.load(Ordering::SeqCst), 1);
        let replay = controller
            .retrieve(read_need, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(replay.to_string().contains("already consumed"));
        assert_eq!(reader_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn authenticated_grants_are_scope_domain_action_time_and_authority_bound() {
        let state = RetrievalRuntimeState::default();
        let grant = state
            .issue_authority_grant(
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "Example.COM.",
                "browser.cdp.read",
                true,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(state.authority_grant_allows(
            Some(&grant.id),
            "owner",
            "default",
            RetrievalAuthority::AuthenticatedRead,
            "browser.cdp.read",
            "https://example.com/private",
        ));
        assert!(!state.authority_grant_allows(
            Some(&grant.id),
            "other",
            "default",
            RetrievalAuthority::AuthenticatedRead,
            "browser.cdp.read",
            "https://example.com/private",
        ));
        assert!(!state.authority_grant_allows(
            Some(&grant.id),
            "owner",
            "default",
            RetrievalAuthority::AuthenticatedRead,
            "browser.cdp.read",
            "https://other.example/private",
        ));
        assert!(!state.authority_grant_allows(
            Some(&grant.id),
            "owner",
            "default",
            RetrievalAuthority::AuthenticatedInteract,
            "browser.cdp.interact_handoff",
            "https://example.com/private",
        ));
        assert!(state
            .claim_authority_grant(
                Some(&grant.id),
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "browser.cdp.read",
                "https://example.com/private",
                "operation-a",
            )
            .is_some());
        assert!(state
            .claim_authority_grant(
                Some(&grant.id),
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "browser.cdp.read",
                "https://example.com/private",
                "operation-a",
            )
            .is_some());
        assert!(state
            .claim_authority_grant(
                Some(&grant.id),
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "browser.cdp.read",
                "https://example.com/private",
                "operation-b",
            )
            .is_none());
        assert!(!state.authority_grant_allows(
            Some(&grant.id),
            "owner",
            "default",
            RetrievalAuthority::AuthenticatedRead,
            "browser.cdp.read",
            "https://example.com/private",
        ));
        assert!(state.revoke_authority_grant(&grant.id));
        assert!(!state.authority_grant_allows(
            Some(&grant.id),
            "owner",
            "default",
            RetrievalAuthority::AuthenticatedRead,
            "browser.cdp.read",
            "https://example.com/private",
        ));
    }

    #[test]
    fn read_authority_does_not_authorize_browser_interaction() {
        assert!(authority_allows(
            RetrievalAuthority::AuthenticatedRead,
            RetrievalAuthority::AuthenticatedRead
        ));
        assert!(!authority_allows(
            RetrievalAuthority::AuthenticatedRead,
            RetrievalAuthority::PublicBrowserInteract
        ));
        assert!(!authority_allows(
            RetrievalAuthority::AuthenticatedRead,
            RetrievalAuthority::AuthenticatedInteract
        ));
    }

    #[test]
    fn authenticated_handoff_session_is_scope_action_mode_domain_and_time_bound() {
        let state = RetrievalRuntimeState::default();
        let grant = state
            .issue_authority_grant(
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedInteract,
                "example.com",
                "browser.cdp.interact_handoff",
                true,
                Duration::from_secs(60),
            )
            .unwrap();
        let handoff = RetrievalHandoff {
            id: "rh_test".into(),
            kind: RetrievalHandoffKind::AuthenticatedInteraction,
            browser_session_id: "retrieval-cdp-rh_test".into(),
            requested_mode: "cdp".into(),
            action_id: "browser.cdp.interact_handoff".into(),
            required_authority: RetrievalAuthority::AuthenticatedInteract,
            principal: "owner".into(),
            workspace: "default".into(),
            target_url: Some("https://example.com/private".into()),
            query: None,
            requires_approval: false,
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        };
        state
            .register_handoff_session(&handoff, Some(&grant.id))
            .unwrap();
        let stored_expiry = state
            .handoff_sessions
            .lock()
            .unwrap()
            .get(&handoff.browser_session_id)
            .unwrap()
            .expires_at_ms;
        assert!(stored_expiry <= grant.expires_at_ms);
        state
            .register_handoff_session(&handoff, Some(&grant.id))
            .unwrap();
        let mut replayed_handoff = handoff.clone();
        replayed_handoff.browser_session_id = "retrieval-cdp-rh_replayed".into();
        assert!(state
            .register_handoff_session(&replayed_handoff, Some(&grant.id))
            .is_err());
        assert!(state.handoff_session_allows(
            &handoff.browser_session_id,
            "owner",
            "default",
            "cdp",
            Some("browser.cdp.interact_handoff"),
            Some("https://example.com/next"),
        ));
        assert!(!state.handoff_session_allows(
            &handoff.browser_session_id,
            "other",
            "default",
            "cdp",
            Some("browser.cdp.interact_handoff"),
            Some("https://example.com/next"),
        ));
        assert!(!state.handoff_session_allows(
            &handoff.browser_session_id,
            "owner",
            "default",
            "headless",
            Some("browser.cdp.interact_handoff"),
            Some("https://example.com/next"),
        ));
        assert!(!state.handoff_session_allows(
            &handoff.browser_session_id,
            "owner",
            "default",
            "cdp",
            Some("browser.cdp.read"),
            Some("https://example.com/next"),
        ));
        assert!(!state.handoff_session_allows(
            &handoff.browser_session_id,
            "owner",
            "default",
            "cdp",
            Some("browser.cdp.interact_handoff"),
            Some("https://other.example/next"),
        ));
        assert!(state.revoke_handoff_session(&handoff.browser_session_id));
    }

    #[test]
    fn browser_engine_is_explicit_config_and_rejects_path_traversal() {
        let default = BrowserRetrievalSettings::default();
        assert!(default.engine.is_none());
        assert!(default.public_read_engine.is_none());

        let mut configured = default.clone();
        configured.engine = Some("custom-browser.v2".into());
        configured.public_read_engine = Some("lightpanda".into());
        configured.cdp_url = "ws://127.0.0.1:3999/devtools/browser/magicutor-proxy".into();
        configured.validate_bounds().unwrap();

        configured.public_read_engine = Some("../lightpanda".into());
        assert!(configured.validate_bounds().is_err());
    }

    /// Lane membership is configuration. The shipped default puts the web
    /// researcher in the lane the evaluation measured; an agent in no lane has
    /// no lane, and so can never activate however much it reads.
    #[test]
    fn an_agent_s_lane_comes_from_configuration_and_only_from_it() {
        let settings = WorkingSetActivationSettings::default();
        assert_eq!(
            settings.lane_for_agent("web-researcher"),
            Some("web-research")
        );
        assert_eq!(settings.lane_for_agent("evidence-reviewer"), None);
        assert!(settings.validate_bounds().is_ok());

        let mut custom = settings.clone();
        custom
            .lanes
            .insert("vc-research".to_string(), vec!["vc-researcher".to_string()]);
        assert_eq!(custom.lane_for_agent("vc-researcher"), Some("vc-research"));
        assert!(custom.validate_bounds().is_ok());
    }

    /// One agent, one lane. A routing decision must not depend on which of
    /// two lanes happened to be checked first.
    #[test]
    fn an_agent_in_two_lanes_is_refused_at_validation() {
        let mut settings = WorkingSetActivationSettings::default();
        settings.lanes.insert(
            "other-research".to_string(),
            vec!["web-researcher".to_string()],
        );
        let error = settings
            .validate_bounds()
            .expect_err("two lanes for one agent");
        assert!(error.to_string().contains("more than one working-set lane"));

        let mut empty_lane = WorkingSetActivationSettings::default();
        empty_lane.lanes.insert("ghost".to_string(), Vec::new());
        assert!(
            empty_lane.validate_bounds().is_err(),
            "a lane with no agents is a typo"
        );
    }
}
