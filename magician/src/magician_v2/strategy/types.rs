//! Core types for strategy-based exploration system

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as FmtWrite,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
};

use serde::{Deserialize, Serialize};

use super::plan::{PlanGraph, PlanStep};
use crate::magician_v2::{
    ask_loop::{
        budget::AskDecision,
        ledger::{BudgetLedger, BudgetLedgerError},
    },
    query_analysis::UnifiedQueryAnalysis,
    state_tracker::{StageContext, StageResumeAction, StageResumePolicy},
};
use runtime_core::{ExecutionContext, ToolCatalog, ToolMatchResult, ToolMatching};

// =============================================================================
// HARDCODED CONFIGURATION CONSTANTS
// =============================================================================

/// Default maximum LLM calls per exploration
pub const DEFAULT_MAX_LLM_CALLS: u32 = 100;

/// Default maximum time in milliseconds (2 hours)
///
/// This is an extreme safety fallback - individual step timeouts are the real protection.
/// Long workflows (20-50 steps) can easily take 1-2 hours, which is perfectly valid.
/// Individual tools are capped at 10 minutes max (see routing/timeout.rs).
pub const DEFAULT_MAX_TIME_MS: u64 = 7200000; // 2 hours (120 minutes)

/// Default maximum tokens per exploration
pub const DEFAULT_MAX_TOKENS: u32 = 80000;

/// Minimum confidence threshold for tool matches
pub const CONFIDENCE_THRESHOLD: f32 = 0.7;

/// Beam width for guided search strategy
pub const BEAM_WIDTH: usize = 5;

/// Complexity multiplier for LLM budget scaling
pub const COMPLEXITY_LLM_MULTIPLIER: f32 = 1.5;

// =============================================================================
// CORE STRATEGY TYPES
// =============================================================================

/// Strategy types available for exploration
///
/// Unified architecture: Guided search adapts to all complexity levels
/// - Simple queries: single-step evaluation
/// - Greedy queries: narrow beam with sequential execution
/// - Medium queries: medium-width guided beam search
/// - Complex queries: wider guided beam with deeper exploration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StrategyType {
    /// Adaptive guided search that handles all complexity levels
    GuidedSearch,
    /// Final fallback: atomic tool composition using reasoning LLM with progressive parameter resolution
    AtomicComposition,
}

/// Context passed to all strategies containing analysis results and resources
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningSnapshot {
    /// Latest overall confidence (0.0 - 1.0)
    pub confidence_overall: f64,
    /// Per-slot confidence map (slot_id → confidence)
    #[serde(default)]
    pub confidence_per_slot: HashMap<String, f64>,
    /// Rolling confidence history (timestamp_ms, confidence)
    #[serde(default)]
    pub confidence_history: Vec<(i64, f64)>,
    /// Most recent confidence slope (per second)
    pub confidence_slope: Option<f64>,
    /// Whether additional clarification is recommended before planning
    pub needs_clarification: bool,
    /// Stage context associated with the latest snapshot
    #[serde(default)]
    pub stage_context: StageContext,
}

/// Summary describing how the slot graph changed between planning rounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SlotDiffStats {
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
}

impl SlotDiffStats {
    #[inline]
    pub fn is_noop(&self) -> bool {
        self.inserted == 0 && self.updated == 0 && self.removed == 0
    }
}

/// Summary of a tool provided by a delegate agent.
///
/// Used by planners to identify which agent owns a tool so they can populate
/// `providing_agent_id` on generated `PlanStep`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateToolEntry {
    /// The tool name as it appears in the tool catalog.
    pub tool_name: String,
    /// Description of the tool (optional, for planner context).
    pub description: Option<String>,
}

/// Relationship between the active planning agent and a catalog entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlannerAgentRole {
    SelfAgent,
    DelegateAgent,
}

impl PlannerAgentRole {
    fn prompt_label(self) -> &'static str {
        match self {
            Self::SelfAgent => "Current agent",
            Self::DelegateAgent => "Delegate agent",
        }
    }
}

/// Planner-facing view of one agent's visible tool surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerAgentCatalogEntry {
    pub agent_id: String,
    pub agent_name: String,
    pub role: PlannerAgentRole,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub persona: String,
    /// Raw allowlist selectors from the agent definition (tool names or categories).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Raw prompt-exclusion selectors from the agent definition.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
    /// Structurally denied tools that must never appear in plan steps.
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Visible tools after applying allow/exclude/deny filters.
    #[serde(default)]
    pub tools: Vec<runtime_core::ToolInfo>,
}

impl PlannerAgentCatalogEntry {
    fn capability_summary(&self) -> Option<&str> {
        if !self.description.trim().is_empty() {
            Some(self.description.trim())
        } else if !self.persona.trim().is_empty() {
            Some(self.persona.trim())
        } else {
            None
        }
    }

    fn tool_policy_summary(&self) -> String {
        let mut parts = Vec::new();
        if self.allowed_tools.is_empty() {
            parts.push("all scoped tools".to_string());
        } else {
            parts.push(format!("allowlist: {}", self.allowed_tools.join(", ")));
        }
        if !self.excluded_tools.is_empty() {
            parts.push(format!("excluded: {}", self.excluded_tools.join(", ")));
        }
        if !self.denied_tools.is_empty() {
            parts.push(format!("denied: {}", self.denied_tools.join(", ")));
        }
        parts.join("; ")
    }
}

#[derive(Clone)]
pub struct StrategyContext {
    /// Original query analysis from Stage 1
    pub query_analysis: UnifiedQueryAnalysis,
    /// Resource budget for this exploration
    pub resource_budget: ResourceBudget,
    /// Categories suggested by LLM for tool filtering
    pub suggested_categories: Vec<String>,
    /// Tool catalog service for listings and metadata
    pub tool_catalog: Arc<dyn ToolCatalog>,
    /// Tool matching service for best/multi-match resolution
    pub tool_matching: Arc<dyn ToolMatching>,
    /// Execution context (principal, workspace, metadata) for tenant scoping
    pub execution_context: ExecutionContext,
    /// LLM service for entity mapping and other AI tasks
    pub llm_service:
        Arc<dyn crate::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM>,
    /// Prompt manager for loading prompt templates
    pub prompt_manager: Arc<crate::magician_v2::prompts::PromptManager>,
    /// V2 Tool Matcher for 4-tier progressive filtering (optional, falls back
    /// to V1 if None)
    pub v2_tool_matcher: Option<Arc<crate::magician_v2::tool_matcher::V2ToolMatcher>>,
    /// Execution ID for correlation and event broadcasting
    pub execution_id: Option<String>,
    /// Correlation ID for tracing requests
    pub correlation_id: Option<String>,
    /// Turn ID for saving incremental exploration results
    pub turn_id: Option<String>,
    /// Conversation store for incremental saving of exploration data
    pub conversation_store: Option<Arc<dyn crate::magician_v2::storage::V2ConversationStore>>,
    /// Event broadcaster for real-time progress updates
    pub event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    /// Aggregated LLM call counter shared across strategies
    pub llm_call_counter: Arc<AtomicU32>,
    /// Aggregated LLM token counter shared across strategies
    pub llm_token_counter: Arc<AtomicU64>,
    /// Optional elicitation manager bridging planner and execution
    pub elicitation_manager: Option<Arc<crate::magician_v2::elicitation::ElicitationManager>>,
    /// Snapshot of planning/confidence state for strategies
    pub planning_snapshot: Option<PlanningSnapshot>,
    /// Stage context for the active planning attempt
    pub stage_context: StageContext,
    /// Shared budget ledger for stage-aware ask/budget checks
    pub budget_ledger: Option<Arc<BudgetLedger>>,
    /// Clarified task produced by the elicitation pipeline (if available)
    pub clarified_task: Option<crate::magician_v2::slot_graph::ClarifiedTask>,
    /// Latest slot graph records associated with the workflow
    pub slot_graph: Vec<crate::magician_v2::slot_graph::SlotRecord>,
    /// Summary describing which slots changed for this clarified round
    pub slot_diff: Option<SlotDiffStats>,
    /// Resume policy describing which stages can be skipped or rehydrated
    pub stage_resume_policy: StageResumePolicy,
    /// Whether to allow LLM to generate consent_flags in atomic plans
    pub allow_consent_slots: bool,
    /// Tool whitelist for per-agent tool filtering (matches tool name or category).
    /// Empty = no restriction (all visible tools are available).
    pub tools: Vec<String>,
    /// Tool blacklist for per-agent tool filtering (matches tool name or category).
    /// Tools whose name or categories intersect these entries are excluded.
    pub excluded_tools: Vec<String>,
    /// Structurally denied tools for the planning agent.
    pub denied_tools: Vec<String>,
    /// Tool catalogs from delegate agents, keyed by agent_id.
    ///
    /// Populated from the current agent's `delegation_targets` when the
    /// orchestrator constructs the strategy context.  Planners use this map
    /// to set `providing_agent_id` on generated `PlanStep`s when the selected
    /// tool comes from a delegate rather than the current agent's own catalog.
    pub delegate_tool_catalog: HashMap<String, Vec<runtime_core::ToolInfo>>,
    /// Agent-grouped planner catalog describing the current agent and delegates.
    pub planner_agent_catalog: Vec<PlannerAgentCatalogEntry>,
    /// Procedure playbooks the current agent lists in its `tools:` block, as
    /// `(name, description)`.
    ///
    /// A procedure skill carries no `runtime_contract:` and so produces no
    /// capability pack, which means it can never appear in the tool catalog
    /// above. The only way to reach one is `activate_skill`, whose `name`
    /// parameter is a free-form string — so without this list a planner cannot
    /// name a playbook that exists. Mirrors the executor's
    /// `AgenticContext::available_procedure_skills`, and is populated from the
    /// same `skills::agent_procedure_skill_catalog`.
    pub available_procedure_skills: Vec<(String, String)>,
}

impl StrategyContext {
    /// Record an LLM usage event for monitoring and budgeting.
    pub fn record_llm_usage(&self, tokens: Option<u32>) {
        self.llm_call_counter.fetch_add(1, Ordering::Relaxed);
        if let Some(token_count) = tokens {
            self.llm_token_counter
                .fetch_add(token_count as u64, Ordering::Relaxed);
        }
    }

    /// Bridge planning calls made directly through `OperationLlmRouter` into
    /// the canonical usage/cost stream. The legacy counters above enforce
    /// planning budgets but do not feed `/llm` analytics.
    pub fn emit_operation_llm_telemetry(
        &self,
        fallback_operation: &str,
        response: &crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
        latency_ms: u64,
    ) {
        let Some((context, attribution)) = self.operation_llm_telemetry_context("planning") else {
            return;
        };
        context.emit_success(fallback_operation, response, latency_ms, attribution);
    }

    pub fn operation_llm_telemetry_context(
        &self,
        capability: &str,
    ) -> Option<(
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext,
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution,
    )> {
        let broadcaster = self.event_broadcaster.as_ref()?;
        let metadata = &self.execution_context.metadata;
        Some((
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                self.execution_context.principal.clone(),
                self.execution_context.workspace.clone(),
                capability,
            ),
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: self.execution_id.clone(),
                task_id: metadata.get("task_id").cloned(),
                agent_id: metadata
                    .get("agent_id")
                    .cloned()
                    .or_else(|| metadata.get("source_agent_id").cloned()),
                delegated_agent_id: metadata.get("delegated_agent_id").cloned(),
                chat_session_id: metadata.get("chat_session_id").cloned(),
                ..Default::default()
            },
        ))
    }

    /// Snapshot of aggregated LLM usage (calls, tokens).
    pub fn llm_usage_snapshot(&self) -> (u32, u64) {
        (
            self.llm_call_counter.load(Ordering::Relaxed),
            self.llm_token_counter.load(Ordering::Relaxed),
        )
    }

    /// Evaluate whether the budget policy recommends pausing for clarification.
    pub async fn evaluate_budget_guard(&self) -> Result<Option<AskDecision>, BudgetLedgerError> {
        let (ledger, execution_id) = match (&self.budget_ledger, &self.execution_id) {
            (Some(ledger), Some(execution_id)) => (ledger, execution_id),
            _ => return Ok(None),
        };

        let decision = ledger
            .evaluate_ask_decision_for_stage(execution_id, self.stage_context)
            .await?;
        if decision.should_ask && self.budget_guard_has_actionable_clarification() {
            Ok(Some(decision))
        } else {
            if decision.should_ask {
                tracing::debug!(
                    execution_id,
                    stage = %self.stage_context,
                    reason = %decision.reason,
                    "Skipping planning budget hold because the authoritative planning snapshot has no actionable clarification"
                );
            }
            Ok(None)
        }
    }

    /// Planning can pause only when its authoritative snapshot still has an
    /// actionable clarification. The budget policy deliberately uses a more
    /// conservative confidence floor than the elicitor; applying that signal
    /// after the elicitor has resolved every slot creates an invisible hold
    /// with no question for Plan Inspector or Attention to present. Execution
    /// and legacy callers without a snapshot preserve the existing behavior.
    fn budget_guard_has_actionable_clarification(&self) -> bool {
        if !matches!(
            self.stage_context,
            StageContext::PlanningBootstrap | StageContext::PlanningIteration
        ) {
            return true;
        }

        self.planning_snapshot
            .as_ref()
            .map(|snapshot| snapshot.needs_clarification)
            .unwrap_or(true)
    }

    /// Lookup the resume action for a given stage name, if available.
    pub fn stage_resume_action(&self, stage_name: &str) -> Option<StageResumeAction> {
        self.stage_resume_policy
            .decision_for(stage_name)
            .map(|decision| decision.action)
    }

    /// Return the agent-filtered tool catalog merged with delegate worker tools.
    ///
    /// Calls [`ToolCatalog::agent_filtered_tools`] on the underlying catalog
    /// (filtered by Personal agent's tools) then merges delegate worker tools:
    /// - Tools that exist in both Personal and Worker lists: the Personal's copy
    ///   is tagged with `providing_agent_id = worker_id` so the executor delegates
    ///   to the specialized Worker for better performance.
    /// - Worker-only tools (not in Personal's list) are appended with
    ///   `providing_agent_id` set, making them available via delegation.
    pub async fn merged_agent_tools(&self) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
        // 1. The self agent's resolved surface.
        //
        // Prefer the orchestrator's `planner_agent_catalog` whenever it is
        // populated: that entry comes from `visible_tools_for_definition`,
        // which unions the universal backend packs and applies the harness
        // inject/strip rules. Re-deriving the surface from
        // `agent_filtered_tools` here resolved only the YAML allowlist — which
        // is how the planner lost `shell` / `files` / `http` / `read_file`
        // while the executor kept injecting them, leaving `AtomicComposition`
        // unable to see the primitives it decomposes into. Keeping one
        // resolver means the two cannot drift again.
        //
        // The fallback covers callers with no agent context: unit tests and
        // agent-less planning, where an empty allowlist means "no restriction".
        let mut tools = match self
            .planner_agent_catalog
            .iter()
            .find(|entry| entry.role == PlannerAgentRole::SelfAgent)
        {
            Some(entry) => entry.tools.clone(),
            None => {
                // An empty allowlist means "no restriction", so this path still
                // sees the substrate and is the ordinary agent-less case. A
                // NON-empty allowlist here is the degraded one: the catalog
                // builder bailed (it returns an empty vec when the owner record
                // fails to load) while the tool filters resolved through a
                // different loader, so the planner silently reverts to the
                // pre-fix surface with no `shell` / `files` / `http`. Say so —
                // this defect went unnoticed for months precisely because it
                // was silent.
                if !self.tools.is_empty() {
                    tracing::warn!(
                        allowlist_entries = self.tools.len(),
                        "[MAGICIAN-V2-STRATEGY] No planner agent catalog for this run — \
                         falling back to the raw allowlist, so the universal backend packs \
                         (shell, files, http, read_file, …) will NOT be visible to the planner"
                    );
                }
                self.tool_catalog
                    .agent_filtered_tools(
                        &self.tools,
                        &self.excluded_tools,
                        &self.execution_context,
                    )
                    .await?
            },
        };
        // Merge delegate agent tools — delegates win over local copies because
        // a delegate is a specialized agent explicitly configured for that capability.
        //
        // With ONE exception: a delegate never displaces the universal
        // substrate. "The delegate is the specialist" holds for an allowlisted
        // tool, not for an ambient one the owner runs natively anyway — and
        // four live delegates do list universal names (`vc-researcher` has
        // `read_file` / `write_file` / `edit_file`, `web-researcher` has
        // `web_fetch` / `content_read` / `content_search`,
        // `internal-system-analyst` has `create_dashboard`,
        // `executive-assistant` has `update_memory_tier`). Letting those win
        // would put "Providing agent: vc-researcher" on a plan step for a
        // trivial file read.
        //
        // It would also diverge from the executor, which is the authority
        // here: `extract_direct_capabilities` drops every delegate-owned tool
        // (`providing_agent_id.is_none()`) and then re-adds the substrate from
        // the embedded defs, so at run time these are the OWNER's direct
        // capabilities. The orchestrator's own delegate merge
        // (`merge_delegate_tool_entries`) already gives direct grants
        // precedence; this brings the planner's copy in line with both.
        if !self.delegate_tool_catalog.is_empty() {
            use crate::magician_v2::execution::agentic::native_integration::is_universal_backend_pack;
            for (agent_id, worker_tools) in &self.delegate_tool_catalog {
                for worker_tool in worker_tools {
                    let owner_runs_it_natively = is_universal_backend_pack(&worker_tool.name)
                        && tools.iter().any(|tool| {
                            tool.name == worker_tool.name && tool.providing_agent_id.is_none()
                        });
                    if owner_runs_it_natively {
                        continue;
                    }
                    // Remove local copy if present — the delegate is the specialist.
                    tools.retain(|t| t.name != worker_tool.name);
                    let mut tool = worker_tool.clone();
                    tool.providing_agent_id = Some(agent_id.clone());
                    tools.push(tool);
                }
            }
        }

        // Deny AFTER the delegate merge, not before it.
        //
        // `denied_tools` is documented as "structurally denied tools that must
        // never appear in plan steps", but the retain used to run before the
        // merge, so a delegate exposing a denied name pushed it straight back
        // in and the plan could name it. Filtering last closes that without
        // changing what deny means: it can only ever remove.
        //
        // Matched with `tool_name_matches_block_entry`, not `Vec::contains`.
        // The executor denies through that helper, which trims and maps
        // `core_utility` → `time_math`; an exact compare here would leave
        // `time_math` visible to the planner for an agent that denies
        // `core_utility`, and `time_math` is a universal pack, so the union
        // above makes exactly that reachable.
        if !self.denied_tools.is_empty() {
            tools.retain(|tool| {
                !self.denied_tools.iter().any(|denied| {
                    crate::magician_v2::agents::types::tool_name_matches_block_entry(
                        &tool.name, denied,
                    )
                })
            });
        }

        Ok(tools)
    }

    /// Resolve which delegate agent owns the given tool, if any.
    pub fn providing_agent_id_for_tool(&self, tool_name: &str) -> Option<String> {
        if !self.planner_agent_catalog.is_empty() {
            if let Some(agent_id) = self
                .planner_agent_catalog
                .iter()
                .filter(|entry| entry.role == PlannerAgentRole::DelegateAgent)
                .find_map(|entry| {
                    entry
                        .tools
                        .iter()
                        .any(|tool| tool.name == tool_name)
                        .then(|| entry.agent_id.clone())
                })
            {
                return Some(agent_id);
            }
        }

        self.delegate_tool_catalog
            .iter()
            .find_map(|(agent_id, tools)| {
                tools
                    .iter()
                    .any(|tool_info| tool_info.name == tool_name)
                    .then(|| agent_id.clone())
            })
    }

    /// Render the activatable procedure playbooks for the planning prompt.
    ///
    /// Deliberately parallel to the executor's
    /// `decision::build_available_procedure_skills_section`, including the
    /// 280-character description cap that stops a long-winded `SKILL.md`
    /// frontmatter from crowding out the tool catalog. Returns an empty string
    /// when the agent allowlists no playbooks, so the caller can append
    /// unconditionally.
    pub fn render_available_procedure_skills(&self) -> String {
        if self.available_procedure_skills.is_empty() {
            return String::new();
        }
        let mut section = String::from("\nACTIVATABLE PROCEDURE PLAYBOOKS:\n");
        section.push_str(
            "These are playbooks, not tools — they take no plan step of their own. Plan a \
             step calling `activate_skill` with the exact name below when a playbook's \
             guidance should shape the steps that follow, and `deactivate_skill` when it \
             no longer applies. Only one can be active at a time.\n",
        );
        for (name, description) in &self.available_procedure_skills {
            let trimmed = description.trim();
            if trimmed.is_empty() {
                let _ = writeln!(section, "- `{name}`");
                continue;
            }
            let preview = if trimmed.chars().count() > 280 {
                let truncated: String = trimmed.chars().take(280).collect();
                format!("{}…", truncated.trim_end())
            } else {
                trimmed.to_string()
            };
            let _ = writeln!(section, "- `{name}` — {preview}");
        }
        section
    }

    /// Render a prompt-friendly, agent-grouped catalog for the provided tools.
    pub fn render_planner_tool_catalog(&self, visible_tools: &[runtime_core::ToolInfo]) -> String {
        if visible_tools.is_empty() {
            return String::new();
        }

        let mut tools_by_provider: BTreeMap<Option<String>, Vec<&runtime_core::ToolInfo>> =
            BTreeMap::new();
        for tool in visible_tools {
            tools_by_provider
                .entry(tool.providing_agent_id.clone())
                .or_default()
                .push(tool);
        }

        let mut sections = Vec::new();
        for entry in &self.planner_agent_catalog {
            let provider_key = match entry.role {
                PlannerAgentRole::SelfAgent => None,
                PlannerAgentRole::DelegateAgent => Some(entry.agent_id.clone()),
            };
            let Some(mut tools) = tools_by_provider.remove(&provider_key) else {
                continue;
            };
            tools.sort_by(|left, right| left.name.cmp(&right.name));

            let mut section = String::new();
            let _ = writeln!(
                section,
                "{}: {} ({})",
                entry.role.prompt_label(),
                entry.agent_name,
                entry.agent_id
            );
            if let Some(summary) = entry.capability_summary() {
                let _ = writeln!(section, "Capability: {}", summary);
            }
            let policy_summary = entry.tool_policy_summary();
            if !policy_summary.is_empty() {
                let _ = writeln!(section, "Tool policy: {}", policy_summary);
            }
            // A delegate also receives the universal substrate (shell, files,
            // http, the memory and introspection reads, …) when it runs; only
            // the current agent's copy is enumerated, because listing the whole
            // universal block once per delegate would dominate this prompt for
            // an owner with `delegation_targets: ['*']`.
            if entry.role == PlannerAgentRole::DelegateAgent {
                let _ = writeln!(
                    section,
                    "Also gets the universal tool substrate at run time (shell, files, \
                     http, read/write/edit_file, glob, grep, web search/fetch, memory \
                     and introspection reads); an untrusted or surface-only delegate \
                     receives a narrower subset, and its deny list still applies."
                );
            }
            let _ = writeln!(section, "Visible tools:");
            for tool in tools {
                let categories = if tool.categories.is_empty() {
                    String::new()
                } else {
                    format!(" [categories: {}]", tool.categories.join(", "))
                };
                let _ = writeln!(
                    section,
                    "- {}{}: {}",
                    tool.name, categories, tool.description
                );
            }
            sections.push(section.trim_end().to_string());
        }

        if !tools_by_provider.is_empty() {
            for (_, mut tools) in tools_by_provider {
                tools.sort_by(|left, right| left.name.cmp(&right.name));
                let mut section = String::from("Unattributed tools:\n");
                for tool in tools {
                    let categories = if tool.categories.is_empty() {
                        String::new()
                    } else {
                        format!(" [categories: {}]", tool.categories.join(", "))
                    };
                    let _ = writeln!(
                        section,
                        "- {}{}: {}",
                        tool.name, categories, tool.description
                    );
                }
                sections.push(section.trim_end().to_string());
            }
        }

        sections.join("\n\n")
    }
}

/// Resource budget tracking and enforcement
#[derive(Debug, Clone)]
pub struct ResourceBudget {
    /// Maximum LLM calls allowed
    pub max_llm_calls: u32,
    /// Maximum time in milliseconds
    pub max_time_ms: u64,
    /// Maximum tokens allowed
    pub max_tokens: u32,
    /// LLM calls consumed so far
    pub consumed_llm_calls: u32,
    /// Time consumed so far (tracked via start_time)
    pub consumed_time_ms: u64,
    /// Tokens consumed so far
    pub consumed_tokens: u32,
    /// Start time for elapsed calculation
    pub start_time: std::time::Instant,
}

impl ResourceBudget {
    /// Create budget from query analysis with complexity-based scaling
    pub fn from_analysis(analysis: &UnifiedQueryAnalysis) -> Self {
        let complexity_factor = 1.0 + (analysis.complexity.score * COMPLEXITY_LLM_MULTIPLIER);

        Self {
            max_llm_calls: (DEFAULT_MAX_LLM_CALLS as f32 * complexity_factor) as u32,
            max_time_ms: (DEFAULT_MAX_TIME_MS as f32 * complexity_factor) as u64,
            max_tokens: (DEFAULT_MAX_TOKENS as f32 * complexity_factor) as u32,
            consumed_llm_calls: 0,
            consumed_time_ms: 0,
            consumed_tokens: 0,
            start_time: std::time::Instant::now(),
        }
    }

    /// Check if exploration can continue within budget
    pub fn can_continue(&self) -> bool {
        self.consumed_llm_calls < self.max_llm_calls
            && self.start_time.elapsed().as_millis() < self.max_time_ms as u128
    }

    /// Check if guided search strategy is viable with current budget
    pub fn allows_guided_search(&self) -> bool {
        self.remaining_llm_calls() >= 5 && self.remaining_time_ms() >= 10000
    }

    /// Get remaining LLM calls
    pub fn remaining_llm_calls(&self) -> u32 {
        self.max_llm_calls.saturating_sub(self.consumed_llm_calls)
    }

    /// Get remaining time in milliseconds
    pub fn remaining_time_ms(&self) -> u64 {
        let elapsed = self.start_time.elapsed().as_millis() as u64;
        self.max_time_ms.saturating_sub(elapsed)
    }

    /// Mark LLM call as consumed
    pub fn consume_llm_call(&mut self, tokens: u32) {
        self.consumed_llm_calls += 1;
        self.consumed_tokens += tokens;
    }

    /// Update consumed time (called at end of operations)
    pub fn update_time(&mut self) {
        self.consumed_time_ms = self.start_time.elapsed().as_millis() as u64;
    }
}

// =============================================================================
// EXPLORATION TREE TYPES
// =============================================================================

/// Context information accumulated during task exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// Original root task that started the exploration
    pub root_task: String,
    /// Task hierarchy from root to current task
    pub task_path: Vec<String>,
    /// Accumulated context from parent tasks
    pub parent_context: String,
    /// Categories relevant to this task and its parents
    pub relevant_categories: Vec<String>,
    /// Metadata from parent task analysis
    pub parent_metadata: HashMap<String, String>,
}

impl TaskContext {
    /// Create new root context
    pub fn new_root(root_task: String) -> Self {
        Self {
            root_task: root_task.clone(),
            task_path: vec![root_task],
            parent_context: String::new(),
            relevant_categories: Vec::new(),
            parent_metadata: HashMap::new(),
        }
    }

    /// Create child context inheriting from parent
    pub fn create_child(
        &self,
        child_task: String,
        parent_analysis: Option<&UnifiedQueryAnalysis>,
    ) -> Self {
        let mut child_path = self.task_path.clone();
        child_path.push(child_task);

        // Build accumulated context
        let mut context_parts = vec![];
        if !self.parent_context.is_empty() {
            context_parts.push(self.parent_context.clone());
        }
        if let Some(current_task) = self.task_path.last() {
            context_parts.push(format!("Parent: {}", current_task));
        }
        let accumulated_context = context_parts.join(" | ");

        // Inherit categories from parent analysis
        let mut categories = self.relevant_categories.clone();
        if let Some(analysis) = parent_analysis {
            categories.extend(analysis.categories.categories.clone());
            categories.sort();
            categories.dedup();
        }

        // Add metadata from parent analysis
        let mut metadata = self.parent_metadata.clone();
        if let Some(analysis) = parent_analysis {
            metadata.insert(
                "parent_complexity".to_string(),
                analysis.complexity.score.to_string(),
            );
            metadata.insert(
                "parent_reasoning".to_string(),
                analysis.complexity.reasoning.clone(),
            );
        }

        Self {
            root_task: self.root_task.clone(),
            task_path: child_path,
            parent_context: accumulated_context,
            relevant_categories: categories,
            parent_metadata: metadata,
        }
    }

    /// Get the immediate parent task
    pub fn get_parent_task(&self) -> Option<&String> {
        if self.task_path.len() > 1 {
            self.task_path.get(self.task_path.len() - 2)
        } else {
            None
        }
    }

    /// Get the current task
    pub fn get_current_task(&self) -> Option<&String> {
        self.task_path.last()
    }

    /// Check if this context has a similar ancestor (for cycle detection)
    /// Excludes the last element (current task) to avoid comparing task to
    /// itself
    pub fn has_similar_ancestor(&self, task: &str, threshold: f32) -> bool {
        // Skip the last element if it exists (current task comparing to itself)
        let ancestors = if self.task_path.is_empty() {
            &self.task_path[..]
        } else {
            &self.task_path[..self.task_path.len() - 1]
        };

        for ancestor in ancestors {
            if calculate_text_similarity(task, ancestor) > threshold {
                return true;
            }
        }
        false
    }
}

impl Default for TaskContext {
    fn default() -> Self {
        Self::new_root("default".to_string())
    }
}

/// Simple text similarity calculation for cycle detection
fn calculate_text_similarity(text1: &str, text2: &str) -> f32 {
    let words1: std::collections::HashSet<String> = text1
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let words2: std::collections::HashSet<String> = text2
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let intersection = words1.intersection(&words2).count();
    let union = words1.union(&words2).count();

    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

/// Node in exploration tree (used by guided search strategies)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationNode {
    /// Unique identifier for this node
    pub id: String,
    /// Task description for this node
    pub task: String,
    /// Parent node ID (None for root)
    pub parent_id: Option<String>,
    /// Child node IDs
    pub children: Vec<String>,
    /// Tool match result if found
    pub tool_match: Option<ToolMatchResult>,
    /// Confidence score for this node
    pub confidence: f32,
    /// Number of times this node was visited
    pub visits: u32,
    /// Accumulated value for this node
    pub value: f32,
    /// Depth in the exploration tree
    pub depth: u32,
    /// Category hints for tool filtering
    pub categories: Vec<String>,
    /// Whether this node represents a complete solution
    pub is_terminal: bool,
    /// Analysis result for this task (cached for performance)
    pub analysis: Option<UnifiedQueryAnalysis>,
    /// Context accumulated from parent tasks
    pub task_context: TaskContext,
    /// Dependencies on other nodes in the tree
    pub dependencies: Vec<String>,
    /// Priority score for this node (higher = more important)
    pub priority: f32,

    // ========== Parameter Management (NEW) ==========
    /// Parameters extracted from THIS node's task
    #[serde(default)]
    pub extracted_parameters: HashMap<String, String>,
    /// Parameters inherited from parent chain
    #[serde(default)]
    pub inherited_parameters: HashMap<String, String>,
    /// Combined view: inherited + extracted (extracted overrides inherited)
    #[serde(default)]
    pub available_parameters: HashMap<String, String>,
    /// Partial plan metadata captured during simulation
    pub partial_plan: Option<PlanStep>,
}

impl ExplorationNode {
    /// Create a new exploration node
    pub fn new(id: String, task: String, parent_id: Option<String>, depth: u32) -> Self {
        let task_context = TaskContext::new_root(task.clone());

        Self {
            id,
            task,
            parent_id,
            children: Vec::new(),
            tool_match: None,
            confidence: 0.0,
            visits: 0,
            value: 0.0,
            depth,
            categories: Vec::new(),
            is_terminal: false,
            analysis: None,
            task_context,
            dependencies: Vec::new(),
            priority: 1.0,
            extracted_parameters: HashMap::new(),
            inherited_parameters: HashMap::new(),
            available_parameters: HashMap::new(),
            partial_plan: None,
        }
    }

    /// Create a new exploration node with inherited context from parent
    pub fn new_with_context(
        id: String,
        task: String,
        parent_id: Option<String>,
        depth: u32,
        parent_context: TaskContext,
        parent_analysis: Option<&UnifiedQueryAnalysis>,
    ) -> Self {
        let task_context = parent_context.create_child(task.clone(), parent_analysis);

        // Extract categories from parent analysis if available
        let categories = if let Some(analysis) = parent_analysis {
            analysis.categories.categories.clone()
        } else {
            Vec::new()
        };

        // Set priority based on parent analysis complexity
        let priority = if let Some(analysis) = parent_analysis {
            analysis.complexity.score
        } else {
            1.0
        };

        Self {
            id,
            task,
            parent_id,
            children: Vec::new(),
            tool_match: None,
            confidence: 0.0,
            visits: 0,
            value: 0.0,
            depth,
            categories,
            is_terminal: false,
            analysis: None,
            task_context,
            dependencies: Vec::new(),
            priority,
            extracted_parameters: HashMap::new(),
            inherited_parameters: HashMap::new(),
            available_parameters: HashMap::new(),
            partial_plan: None,
        }
    }

    /// Add a child node
    pub fn add_child(&mut self, child_id: String) {
        if !self.children.contains(&child_id) {
            self.children.push(child_id);
        }
    }

    /// Check if this node needs expansion (has no children and no tool match)
    pub fn needs_expansion(&self) -> bool {
        self.children.is_empty() && self.tool_match.is_none() && !self.is_terminal
    }

    /// Get average value for scoring
    pub fn average_value(&self) -> f32 {
        if self.visits == 0 {
            0.0
        } else {
            self.value / self.visits as f32
        }
    }

    /// Create a new exploration node with parameter inheritance
    ///
    /// This is the PREFERRED constructor for creating child nodes in
    /// guided search. It properly inherits parameters from parent and
    /// merges with newly extracted parameters.
    pub fn new_with_parameters(
        id: String,
        task: String,
        parent_id: Option<String>,
        depth: u32,
        parent_parameters: HashMap<String, String>,
        query_analysis: Option<&UnifiedQueryAnalysis>,
        parent_context: TaskContext,
    ) -> Self {
        // Extract parameters from query analysis if available
        let extracted = if let Some(analysis) = query_analysis {
            analysis.extracted_entities.entities.clone()
        } else {
            HashMap::new()
        };

        // Merge: parent + extracted (extracted wins on conflicts)
        let mut available = parent_parameters.clone();
        for (key, value) in &extracted {
            available.insert(key.clone(), value.clone());
        }

        // Create child context with accumulated parameters
        let task_context = parent_context.create_child(task.clone(), query_analysis);

        // Extract categories from analysis if available
        let categories = if let Some(analysis) = query_analysis {
            analysis.categories.categories.clone()
        } else {
            Vec::new()
        };

        // Set priority from analysis complexity if available
        let priority = if let Some(analysis) = query_analysis {
            analysis.complexity.score
        } else {
            1.0
        };

        Self {
            id,
            task,
            parent_id,
            children: Vec::new(),
            tool_match: None,
            confidence: 0.0,
            visits: 0,
            value: 0.0,
            depth,
            categories,
            is_terminal: false,
            analysis: query_analysis.cloned(),
            task_context,
            dependencies: Vec::new(),
            priority,
            extracted_parameters: extracted,
            inherited_parameters: parent_parameters,
            available_parameters: available,
            partial_plan: None,
        }
    }

    /// Get all available parameters (inherited + extracted)
    pub fn get_available_parameters(&self) -> &HashMap<String, String> {
        &self.available_parameters
    }

    /// Map extracted entities to tool parameters using semantic mapping
    ///
    /// This method should be called AFTER tool_match is set to enable
    /// intelligent entity-to-parameter mapping based on the tool's schema.
    ///
    /// # Arguments
    /// * `entity_mapper` - Entity mapping service
    /// * `query_text` - Original query for context
    ///
    /// # Returns
    /// * `Result<()>` - Ok if mapping succeeded, Err otherwise
    pub async fn map_entities_to_tool_parameters(
        &mut self,
        entity_mapper: &crate::magician_v2::strategy::entity_mapper::EntityMapper,
        query_text: &str,
    ) -> Result<(), anyhow::Error> {
        use tracing::debug;

        // Only map if we have both a tool match and entities
        if self.tool_match.is_none() {
            debug!("[MAGICIAN-V2-STRATEGY] No tool match available for entity mapping");
            return Ok(());
        }

        if self.extracted_parameters.is_empty() {
            debug!("[MAGICIAN-V2-STRATEGY] No entities to map");
            return Ok(());
        }

        let tool_match = self.tool_match.as_ref().unwrap();

        // Get tool schema from primary match
        if let Some(ref primary_match) = tool_match.primary_match {
            let tool_schema = &primary_match.tool_metadata.input_schema;

            debug!(
                "[MAGICIAN-V2-STRATEGY] Mapping {} entities to tool '{}' parameters",
                self.extracted_parameters.len(),
                primary_match.tool_name
            );

            // Perform semantic mapping
            let mapping_result = entity_mapper
                .map_entities_to_parameters(tool_schema, &self.extracted_parameters, query_text)
                .await?;

            // Convert mapped parameters from serde_json::Value to String
            // (maintaining compatibility with current HashMap<String, String> type)
            let mapped_as_strings: HashMap<String, String> = mapping_result
                .mapped_parameters
                .into_iter()
                .map(|(key, value)| {
                    let value_str = match value {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        other => serde_json::to_string(&other).unwrap_or_default(),
                    };
                    (key, value_str)
                })
                .collect();

            debug!(
                "[MAGICIAN-V2-STRATEGY] Mapped {} parameters with {:.2}% confidence",
                mapped_as_strings.len(),
                mapping_result.confidence * 100.0
            );

            // Update extracted_parameters with intelligently mapped values
            self.extracted_parameters = mapped_as_strings.clone();

            // Rebuild available_parameters with new mappings
            self.available_parameters = self.inherited_parameters.clone();
            self.available_parameters.extend(mapped_as_strings);

            debug!(
                "[MAGICIAN-V2-STRATEGY] Total available parameters after mapping: {}",
                self.available_parameters.len()
            );
        }

        Ok(())
    }
}

// =============================================================================
// RESULT TYPES
// =============================================================================

/// Result of strategy exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationResult {
    /// Root node of exploration
    pub root_node: ExplorationNode,
    /// All nodes created during exploration
    pub all_nodes: HashMap<String, ExplorationNode>,
    /// Best path found (node IDs from root to best leaf)
    pub best_path: Vec<String>,
    /// Overall confidence of best solution
    pub confidence: f32,
    /// Structured plan graph produced by the strategy (if available)
    pub plan: Option<PlanGraph>,
    /// Resources consumed during exploration
    pub resources_consumed: ResourceUsage,
    /// Strategy metadata
    pub strategy_metadata: StrategyMetadata,
    /// Parameter extraction and usage statistics
    #[serde(default)]
    pub parameter_statistics: ParameterStatistics,
}

/// Resource usage tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// LLM calls made
    pub llm_calls: u32,
    /// Time spent in milliseconds
    pub time_ms: u64,
    /// Tokens consumed
    pub tokens: u32,
}

/// Strategy execution metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyMetadata {
    /// Strategy type used
    pub strategy_type: StrategyType,
    /// Number of iterations performed
    pub iterations: u32,
    /// Number of nodes explored
    pub nodes_explored: u32,
    /// Maximum depth reached
    pub max_depth: u32,
    /// Whether exploration completed or was terminated
    pub completed: bool,
    /// Termination reason if not completed
    pub termination_reason: Option<String>,
}

/// Parameter extraction and usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterStatistics {
    pub total_parameters_extracted: usize,
    pub parameters_in_best_path: usize,
    pub average_parameter_coverage: f32,
    pub extraction_confidence: f32,
    pub nodes_with_sufficient_parameters: usize,
    pub total_nodes_requiring_parameters: usize,
    /// Slots auto-filled using inherited parameters
    #[serde(default)]
    pub slots_auto_filled: usize,
    /// Slots answered manually by the user
    #[serde(default)]
    pub slots_user_filled: usize,
}

impl Default for ParameterStatistics {
    fn default() -> Self {
        Self {
            total_parameters_extracted: 0,
            parameters_in_best_path: 0,
            average_parameter_coverage: 0.0,
            extraction_confidence: 0.0,
            nodes_with_sufficient_parameters: 0,
            total_nodes_requiring_parameters: 0,
            slots_auto_filled: 0,
            slots_user_filled: 0,
        }
    }
}

impl ParameterStatistics {
    /// Calculate statistics from exploration result
    pub fn from_exploration(
        root_node: &ExplorationNode,
        all_nodes: &HashMap<String, ExplorationNode>,
        best_path: &[String],
    ) -> Self {
        // Count total parameters from root extraction
        let total_parameters_extracted = root_node.extracted_parameters.len();

        // Get extraction confidence from root analysis
        let extraction_confidence = root_node
            .analysis
            .as_ref()
            .map(|a| a.extracted_entities.extraction_confidence)
            .unwrap_or(0.0);

        // Count parameters in best path
        let parameters_in_best_path = best_path
            .iter()
            .filter_map(|id| all_nodes.get(id))
            .map(|node| node.available_parameters.len())
            .sum();

        // Calculate coverage statistics across all nodes with tool matches
        let mut total_coverage = 0.0;
        let mut nodes_with_tools = 0;
        let mut nodes_with_sufficient_parameters = 0;

        for node in all_nodes.values() {
            if let Some(ref tool_match) = node.tool_match {
                if let Some(ref primary_match) = tool_match.primary_match {
                    nodes_with_tools += 1;

                    // Calculate coverage for this node
                    let coverage = crate::magician_v2::strategy::parameter_utils::calculate_parameter_compatibility(
                        &primary_match.tool_metadata.input_schema,
                        &node.available_parameters,
                    );

                    total_coverage += coverage;

                    if coverage >= 0.7 {
                        nodes_with_sufficient_parameters += 1;
                    }
                }
            }
        }

        let average_parameter_coverage = if nodes_with_tools > 0 {
            total_coverage / nodes_with_tools as f32
        } else {
            0.0
        };

        Self {
            total_parameters_extracted,
            parameters_in_best_path,
            average_parameter_coverage,
            extraction_confidence,
            nodes_with_sufficient_parameters,
            total_nodes_requiring_parameters: nodes_with_tools,
            slots_auto_filled: 0,
            slots_user_filled: 0,
        }
    }

    /// Calculate statistics from atomic composition plan
    /// For plan-based strategies (atomic composition), we count steps with/without unresolved inputs
    pub fn from_atomic_plan(root_node: &ExplorationNode, plan: &PlanGraph) -> Self {
        let total_parameters_extracted = root_node.extracted_parameters.len();
        let extraction_confidence = root_node
            .analysis
            .as_ref()
            .map(|a| a.extracted_entities.extraction_confidence)
            .unwrap_or(0.0);

        // Total steps in plan
        let total_steps = plan.steps.len();

        // Find unique steps affected by unresolved inputs
        let affected_steps: std::collections::HashSet<&str> = plan
            .unresolved_inputs
            .iter()
            .filter_map(|input| input.step_id.as_deref())
            .collect();

        let steps_with_unresolved = affected_steps.len();
        let nodes_with_sufficient_parameters = total_steps.saturating_sub(steps_with_unresolved);

        // Calculate average coverage
        let average_parameter_coverage = if total_steps > 0 {
            nodes_with_sufficient_parameters as f32 / total_steps as f32
        } else {
            0.0
        };

        Self {
            total_parameters_extracted,
            parameters_in_best_path: total_parameters_extracted,
            average_parameter_coverage,
            extraction_confidence,
            nodes_with_sufficient_parameters,
            total_nodes_requiring_parameters: total_steps,
            slots_auto_filled: 0,
            slots_user_filled: 0,
        }
    }
}

// =============================================================================
// QUERY PATTERN TYPES
// =============================================================================

/// Extracted pattern from query for strategy selection
#[derive(Debug, Clone)]
pub struct QueryPattern {
    /// Number of words in query
    pub word_count: usize,
    /// Whether query contains conjunctions
    pub has_conjunction: bool,
    /// Estimated number of tools needed
    pub estimated_tools: usize,
    /// Complexity score from analysis
    pub complexity_score: f32,
    /// Categories detected
    pub categories: Vec<String>,
}

// =============================================================================
// FAILURE HANDLING TYPES
// =============================================================================

/// Types of strategy failures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyFailure {
    /// No tools found matching the query
    NoToolsFound,
    /// Best match below confidence threshold
    LowConfidence(f32),
    /// Failed to decompose task further
    DecompositionFailed,
    /// Exceeded time budget
    TimeoutExceeded,
    /// Exceeded LLM call budget
    LLMBudgetExceeded,
    /// Internal error during exploration
    InternalError(String),
}

/// Next action to take after strategy failure
#[derive(Debug, Clone)]
pub enum NextAction {
    /// Escalate to more sophisticated strategy
    Escalate(StrategyType),
    /// Simplify to less resource-intensive strategy
    Simplify(StrategyType),
    /// Abort exploration with reason
    Abort(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    use anyhow::Result;
    use async_trait::async_trait;
    use runtime_core::{MultipleToolMatchResult, ParameterDefinition, ToolInfo};

    struct MockToolCatalog {
        tools: Vec<ToolInfo>,
    }

    #[async_trait]
    impl ToolCatalog for MockToolCatalog {
        async fn list_tool_names(&self, _context: &ExecutionContext) -> Vec<String> {
            self.tools.iter().map(|tool| tool.name.clone()).collect()
        }

        async fn available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
            Vec::new()
        }

        async fn get_tool_metadata(
            &self,
            _tool_name: &str,
            _context: &ExecutionContext,
        ) -> Option<HashMap<String, serde_json::Value>> {
            None
        }

        async fn filtered_tools_by_categories(
            &self,
            _categories: &[String],
            _context: &ExecutionContext,
        ) -> Result<Vec<ToolInfo>> {
            Ok(self.tools.clone())
        }

        async fn all_tools(&self, _context: &ExecutionContext) -> Result<Vec<ToolInfo>> {
            Ok(self.tools.clone())
        }

        async fn category_tool_counts(
            &self,
            _context: &ExecutionContext,
        ) -> Result<HashMap<String, usize>> {
            Ok(HashMap::new())
        }
    }

    struct MockToolMatching;

    #[async_trait]
    impl ToolMatching for MockToolMatching {
        async fn best_match(
            &self,
            _task_description: &str,
            _context: &ExecutionContext,
        ) -> ToolMatchResult {
            ToolMatchResult::default()
        }

        async fn multiple_matches(
            &self,
            _task_description: &str,
            _context: &ExecutionContext,
        ) -> MultipleToolMatchResult {
            MultipleToolMatchResult::default()
        }

        async fn is_tool_available(&self, _tool_name: &str, _context: &ExecutionContext) -> bool {
            false
        }

        async fn validate_tool_execution(
            &self,
            _tool_name: &str,
            _parameters: &serde_json::Value,
            _context: &ExecutionContext,
        ) -> bool {
            false
        }

        async fn estimate_execution_cost(
            &self,
            _tool_name: &str,
            _parameters: &serde_json::Value,
            _context: &ExecutionContext,
        ) -> f64 {
            0.0
        }
    }

    struct MockQueryAnalysisLlm;

    #[async_trait]
    impl crate::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM
        for MockQueryAnalysisLlm
    {
        async fn generate_analysis(&self, _prompt: &str) -> anyhow::Result<SimplifiedLLMResponse> {
            Ok(SimplifiedLLMResponse::content_only("{}".to_string()))
        }
    }

    struct MockPromptStore;

    #[async_trait]
    impl crate::magician_v2::prompts::PromptStore for MockPromptStore {
        async fn get_prompt(
            &self,
            _name: &str,
            _version: &str,
        ) -> anyhow::Result<crate::magician_v2::prompts::Prompt> {
            Ok(crate::magician_v2::prompts::Prompt {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                content: "test".to_string(),
                variables: Vec::new(),
                metadata: crate::magician_v2::prompts::PromptMetadata {
                    category: crate::magician_v2::prompts::PromptCategory::QueryAnalysis,
                    description: "test".to_string(),
                    author: "test".to_string(),
                    created_at: chrono::Utc::now(),
                    tags: Vec::new(),
                    changelog: String::new(),
                    estimated_tokens: Some(1),
                },
            })
        }

        async fn list_versions(&self, _name: &str) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_prompt_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn save_prompt(
            &self,
            _prompt: &crate::magician_v2::prompts::Prompt,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn delete_prompt(&self, _name: &str, _version: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, _name: &str, _version: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn latest_version(&self, _name: &str) -> anyhow::Result<String> {
            Ok("1.0.0".to_string())
        }

        async fn initialize(&self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    fn make_tool(name: &str, category: &str, provider: Option<&str>) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: format!("{name} description"),
            category: category.to_string(),
            categories: vec![category.to_string()],
            parameters: vec![ParameterDefinition {
                name: "input".to_string(),
                param_type: "string".to_string(),
                description: "input".to_string(),
                required: false,
                validation_rules: Vec::new(),
                default_value: None,
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            enhanced_description: None,
            keywords: Vec::new(),
            use_cases: Vec::new(),
            composition_category: Some(category.to_string()),
            providing_agent_id: provider.map(str::to_string),
        }
    }

    fn make_query_analysis() -> UnifiedQueryAnalysis {
        use crate::magician_v2::query_analysis::{
            CategoryAnalysis, ComplexityAnalysis, DependencyAnalysis, ExtractedEntities,
            QueryIntent, ResourceEstimate,
        };

        UnifiedQueryAnalysis {
            original_query: "test query".to_string(),
            complexity: ComplexityAnalysis {
                score: 0.4,
                factors: Vec::new(),
                reasoning: "test".to_string(),
            },
            categories: CategoryAnalysis {
                categories: vec!["browser".to_string()],
                reasoning: "test".to_string(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: Vec::new(),
                workflow_steps: Vec::new(),
                reasoning: "test".to_string(),
                required_capabilities: Vec::new(),
            },
            resource_estimate: ResourceEstimate {
                expected_tokens: 10,
                expected_duration_ms: 1000,
                expected_iterations: 1,
            },
            extracted_entities: ExtractedEntities::default(),
            intent: QueryIntent::NewTask,
            slot_match: None,
            llm_calls_used: 0,
            task_clarity: crate::magician_v2::query_analysis::TaskClarity::default(),
        }
    }

    fn make_context(
        catalog_tools: Vec<ToolInfo>,
        delegate_tool_catalog: HashMap<String, Vec<ToolInfo>>,
        planner_agent_catalog: Vec<PlannerAgentCatalogEntry>,
        denied_tools: Vec<String>,
    ) -> StrategyContext {
        let query_analysis = make_query_analysis();
        StrategyContext {
            query_analysis: query_analysis.clone(),
            resource_budget: ResourceBudget::from_analysis(&query_analysis),
            suggested_categories: query_analysis.categories.categories.clone(),
            tool_catalog: Arc::new(MockToolCatalog {
                tools: catalog_tools,
            }),
            tool_matching: Arc::new(MockToolMatching),
            execution_context: ExecutionContext::default(),
            llm_service: Arc::new(MockQueryAnalysisLlm),
            prompt_manager: Arc::new(crate::magician_v2::prompts::PromptManager::new(Arc::new(
                MockPromptStore,
            ))),
            v2_tool_matcher: None,
            execution_id: None,
            correlation_id: None,
            turn_id: None,
            conversation_store: None,
            event_broadcaster: None,
            llm_call_counter: Arc::new(AtomicU32::new(0)),
            llm_token_counter: Arc::new(AtomicU64::new(0)),
            elicitation_manager: None,
            planning_snapshot: None,
            stage_context: StageContext::PlanningBootstrap,
            budget_ledger: None,
            clarified_task: None,
            slot_graph: Vec::new(),
            slot_diff: None,
            stage_resume_policy: StageResumePolicy::default(),
            allow_consent_slots: true,
            tools: Vec::new(),
            excluded_tools: Vec::new(),
            denied_tools,
            delegate_tool_catalog,
            planner_agent_catalog,
            available_procedure_skills: Vec::new(),
        }
    }

    #[tokio::test]
    async fn merged_agent_tools_filters_denied_tools_and_prefers_delegate_tools() {
        let local_safe = make_tool("local_safe", "browser", None);
        let local_denied = make_tool("dangerous_local", "shell", None);
        let local_overlap = make_tool("shared_tool", "browser", None);
        let delegate_overlap = make_tool("shared_tool", "browser", Some("delegate-1"));

        let context = make_context(
            vec![local_safe.clone(), local_denied, local_overlap],
            HashMap::from([("delegate-1".to_string(), vec![delegate_overlap.clone()])]),
            Vec::new(),
            vec!["dangerous_local".to_string()],
        );

        let merged = context.merged_agent_tools().await.unwrap();
        assert!(merged.iter().any(|tool| tool.name == "local_safe"));
        assert!(!merged.iter().any(|tool| tool.name == "dangerous_local"));

        let shared = merged
            .iter()
            .find(|tool| tool.name == "shared_tool")
            .expect("shared tool should remain available");
        assert_eq!(shared.providing_agent_id.as_deref(), Some("delegate-1"));
    }

    #[test]
    fn planner_catalog_rendering_groups_self_and_delegate_tools() {
        let self_tool = make_tool("browser_open", "browser", None);
        let delegate_tool = make_tool("github_search", "research", Some("delegate-1"));

        let context = make_context(
            Vec::new(),
            HashMap::new(),
            vec![
                PlannerAgentCatalogEntry {
                    agent_id: "owner".to_string(),
                    agent_name: "Owner".to_string(),
                    role: PlannerAgentRole::SelfAgent,
                    description: "Primary planner".to_string(),
                    persona: String::new(),
                    allowed_tools: vec!["browser".to_string()],
                    excluded_tools: Vec::new(),
                    denied_tools: vec!["rm_rf".to_string()],
                    tools: vec![self_tool.clone()],
                },
                PlannerAgentCatalogEntry {
                    agent_id: "delegate-1".to_string(),
                    agent_name: "GitHub Worker".to_string(),
                    role: PlannerAgentRole::DelegateAgent,
                    description: "GitHub specialist".to_string(),
                    persona: String::new(),
                    allowed_tools: vec!["github".to_string()],
                    excluded_tools: vec!["git_push".to_string()],
                    denied_tools: Vec::new(),
                    tools: vec![delegate_tool.clone()],
                },
            ],
            Vec::new(),
        );

        let rendered = context.render_planner_tool_catalog(&[self_tool, delegate_tool]);
        assert!(rendered.contains("Current agent: Owner (owner)"));
        assert!(rendered.contains("Delegate agent: GitHub Worker (delegate-1)"));
        assert!(rendered.contains("Tool policy: allowlist: browser; denied: rm_rf"));
        assert!(rendered.contains("browser_open"));
        assert!(rendered.contains("github_search"));
    }

    #[test]
    fn providing_agent_lookup_uses_planner_catalog() {
        let delegate_tool = make_tool("delegate_only", "research", Some("delegate-1"));
        let context = make_context(
            Vec::new(),
            HashMap::new(),
            vec![PlannerAgentCatalogEntry {
                agent_id: "delegate-1".to_string(),
                agent_name: "Delegate".to_string(),
                role: PlannerAgentRole::DelegateAgent,
                description: String::new(),
                persona: String::new(),
                allowed_tools: Vec::new(),
                excluded_tools: Vec::new(),
                denied_tools: Vec::new(),
                tools: vec![delegate_tool],
            }],
            Vec::new(),
        );

        assert_eq!(
            context
                .providing_agent_id_for_tool("delegate_only")
                .as_deref(),
            Some("delegate-1")
        );
    }

    #[test]
    fn resolved_planning_snapshot_prevents_questionless_budget_hold() {
        let mut context = make_context(Vec::new(), HashMap::new(), Vec::new(), Vec::new());
        context.planning_snapshot = Some(PlanningSnapshot {
            confidence_overall: 0.76,
            needs_clarification: false,
            stage_context: StageContext::PlanningBootstrap,
            ..PlanningSnapshot::default()
        });

        assert!(!context.budget_guard_has_actionable_clarification());
    }

    #[test]
    fn unresolved_planning_and_execution_preserve_budget_holds() {
        let mut context = make_context(Vec::new(), HashMap::new(), Vec::new(), Vec::new());
        context.planning_snapshot = Some(PlanningSnapshot {
            confidence_overall: 0.42,
            needs_clarification: true,
            stage_context: StageContext::PlanningIteration,
            ..PlanningSnapshot::default()
        });
        context.stage_context = StageContext::PlanningIteration;
        assert!(context.budget_guard_has_actionable_clarification());

        context.stage_context = StageContext::ExecutionCycle;
        context.planning_snapshot = Some(PlanningSnapshot {
            needs_clarification: false,
            ..PlanningSnapshot::default()
        });
        assert!(context.budget_guard_has_actionable_clarification());
    }

    /// The defect this pins: `merged_agent_tools` used to re-derive the self
    /// agent's surface from `agent_filtered_tools`, which resolves only the
    /// YAML allowlist. The executor separately injects the universal backend
    /// packs, so the planner never saw `shell` / `files` / `http` /
    /// `read_file` — the primitives `AtomicComposition` decomposes into.
    /// Sourcing the orchestrator-resolved `SelfAgent` entry keeps one
    /// resolver; `shell` here stands in for the universal substrate, and it is
    /// deliberately absent from the mock catalog so it can only have arrived
    /// via that entry.
    #[tokio::test]
    async fn merged_agent_tools_uses_the_orchestrator_resolved_self_surface() {
        let allowlisted = make_tool("browser", "browser", None);
        let substrate = make_tool("shell", "shell", None);

        let context = make_context(
            vec![allowlisted.clone()],
            HashMap::new(),
            vec![PlannerAgentCatalogEntry {
                agent_id: "owner".to_string(),
                agent_name: "Owner".to_string(),
                role: PlannerAgentRole::SelfAgent,
                description: String::new(),
                persona: String::new(),
                allowed_tools: vec!["browser".to_string()],
                excluded_tools: Vec::new(),
                denied_tools: Vec::new(),
                tools: vec![allowlisted, substrate],
            }],
            Vec::new(),
        );

        let merged = context.merged_agent_tools().await.unwrap();
        assert!(
            merged.iter().any(|tool| tool.name == "shell"),
            "planner lost the universal substrate: {:?}",
            merged.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        assert!(merged.iter().any(|tool| tool.name == "browser"));
    }

    /// Sourcing the resolved surface must not become a way around the deny
    /// list. `denied_tools` is a structural boundary, so it applies to the
    /// orchestrator-resolved entry exactly as it applied to the catalog path.
    #[tokio::test]
    async fn merged_agent_tools_still_denies_over_the_resolved_self_surface() {
        let denied = make_tool("shell", "shell", None);

        let context = make_context(
            Vec::new(),
            HashMap::new(),
            vec![PlannerAgentCatalogEntry {
                agent_id: "owner".to_string(),
                agent_name: "Owner".to_string(),
                role: PlannerAgentRole::SelfAgent,
                description: String::new(),
                persona: String::new(),
                allowed_tools: Vec::new(),
                excluded_tools: Vec::new(),
                denied_tools: vec!["shell".to_string()],
                tools: vec![denied],
            }],
            vec!["shell".to_string()],
        );

        let merged = context.merged_agent_tools().await.unwrap();
        assert!(!merged.iter().any(|tool| tool.name == "shell"));
    }

    /// A procedure playbook is not a pack and can never appear in a tool
    /// catalog, so the planner needs it enumerated or it cannot name one when
    /// planning an `activate_skill` step.
    #[test]
    fn procedure_playbooks_render_for_the_planner_and_cap_long_descriptions() {
        let mut context = make_context(Vec::new(), HashMap::new(), Vec::new(), Vec::new());
        assert!(
            context.render_available_procedure_skills().is_empty(),
            "no playbooks should render nothing so callers can append blindly"
        );

        context.available_procedure_skills = vec![
            ("eli5-explainer".to_string(), "Explain simply.".to_string()),
            ("verbose-playbook".to_string(), "x".repeat(400)),
        ];
        let rendered = context.render_available_procedure_skills();
        assert!(rendered.contains("`eli5-explainer` — Explain simply."));
        assert!(rendered.contains("activate_skill"));
        assert!(
            rendered.contains('…'),
            "a 400-char description must be capped so frontmatter cannot crowd out the catalog"
        );
        assert!(!rendered.contains(&"x".repeat(300)));
    }

    /// The universal substrate is enumerated once for the current agent. Each
    /// delegate gets a one-line note instead, because an owner with
    /// `delegation_targets: ['*']` would otherwise repeat the whole universal
    /// block per delegate and crowd out its own catalog.
    #[test]
    fn delegate_sections_note_the_substrate_instead_of_listing_it() {
        let self_tool = make_tool("browser", "browser", None);
        let delegate_tool = make_tool("github_search", "research", Some("delegate-1"));

        let context = make_context(
            Vec::new(),
            HashMap::new(),
            vec![
                PlannerAgentCatalogEntry {
                    agent_id: "owner".to_string(),
                    agent_name: "Owner".to_string(),
                    role: PlannerAgentRole::SelfAgent,
                    description: String::new(),
                    persona: String::new(),
                    allowed_tools: Vec::new(),
                    excluded_tools: Vec::new(),
                    denied_tools: Vec::new(),
                    tools: vec![self_tool.clone()],
                },
                PlannerAgentCatalogEntry {
                    agent_id: "delegate-1".to_string(),
                    agent_name: "Worker".to_string(),
                    role: PlannerAgentRole::DelegateAgent,
                    description: String::new(),
                    persona: String::new(),
                    allowed_tools: Vec::new(),
                    excluded_tools: Vec::new(),
                    denied_tools: Vec::new(),
                    tools: vec![delegate_tool.clone()],
                },
            ],
            Vec::new(),
        );

        let rendered = context.render_planner_tool_catalog(&[self_tool, delegate_tool]);
        assert_eq!(
            rendered.matches("universal tool substrate").count(),
            1,
            "exactly the one delegate section carries the note"
        );
        let (owner_section, delegate_section) = rendered
            .split_once("Delegate agent:")
            .expect("both sections should render");
        assert!(!owner_section.contains("universal tool substrate"));
        assert!(delegate_section.contains("universal tool substrate"));
    }
    /// A delegate may specialise an allowlisted tool, but not an ambient one.
    ///
    /// Four live delegates list universal names in their YAML (`vc-researcher`
    /// has `read_file`, `web-researcher` has `web_fetch`, …). Once the owner's
    /// surface carries the substrate, the old unconditional
    /// "delegate replaces local copy" rule would attribute a trivial file read
    /// to a VC-research agent — and diverge from the executor, which drops
    /// delegate-owned tools and re-adds the substrate as the OWNER's own
    /// direct capability.
    #[tokio::test]
    async fn delegates_do_not_displace_the_owner_universal_substrate() {
        let substrate = make_tool("read_file", "filesystem", None);
        let specialist = make_tool("github_search", "research", None);
        let delegate_substrate = make_tool("read_file", "filesystem", Some("vc-researcher"));
        let delegate_specialist = make_tool("github_search", "research", Some("vc-researcher"));

        let context = make_context(
            Vec::new(),
            HashMap::from([(
                "vc-researcher".to_string(),
                vec![delegate_substrate, delegate_specialist],
            )]),
            vec![PlannerAgentCatalogEntry {
                agent_id: "owner".to_string(),
                agent_name: "Owner".to_string(),
                role: PlannerAgentRole::SelfAgent,
                description: String::new(),
                persona: String::new(),
                allowed_tools: Vec::new(),
                excluded_tools: Vec::new(),
                denied_tools: Vec::new(),
                tools: vec![substrate, specialist],
            }],
            Vec::new(),
        );

        let merged = context.merged_agent_tools().await.unwrap();

        let read_file = merged
            .iter()
            .find(|tool| tool.name == "read_file")
            .expect("the owner keeps the substrate tool");
        assert_eq!(
            read_file.providing_agent_id, None,
            "a universal pack must stay the owner's own capability"
        );
        assert_eq!(
            merged.iter().filter(|t| t.name == "read_file").count(),
            1,
            "the guard must skip the delegate copy, not add a duplicate"
        );

        // Unchanged for a genuinely allowlisted tool: the delegate is the
        // specialist and still wins.
        let github = merged
            .iter()
            .find(|tool| tool.name == "github_search")
            .expect("delegate specialist should remain available");
        assert_eq!(github.providing_agent_id.as_deref(), Some("vc-researcher"));
    }
    /// `denied_tools` is documented as "must never appear in plan steps", but
    /// the retain ran before the delegate merge, so a delegate exposing a
    /// denied name pushed it back in and the planner could name it.
    #[tokio::test]
    async fn a_delegate_cannot_reintroduce_a_denied_tool() {
        let delegate_denied = make_tool("dangerous_tool", "shell", Some("delegate-1"));

        let context = make_context(
            Vec::new(),
            HashMap::from([("delegate-1".to_string(), vec![delegate_denied])]),
            Vec::new(),
            vec!["dangerous_tool".to_string()],
        );

        let merged = context.merged_agent_tools().await.unwrap();
        assert!(
            !merged.iter().any(|tool| tool.name == "dangerous_tool"),
            "a denied name must not survive via a delegate: {:?}",
            merged.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }
    /// Deny is matched with the executor's `tool_name_matches_block_entry`,
    /// which trims and maps `core_utility` → `time_math`. A plain
    /// `Vec::contains` left `time_math` — itself a universal pack, so present
    /// on every owner surface — visible to a planner whose agent denies
    /// `core_utility`, while the executor refused it.
    #[tokio::test]
    async fn deny_uses_the_same_matcher_as_the_executor() {
        let context = make_context(
            Vec::new(),
            HashMap::new(),
            vec![PlannerAgentCatalogEntry {
                agent_id: "owner".to_string(),
                agent_name: "Owner".to_string(),
                role: PlannerAgentRole::SelfAgent,
                description: String::new(),
                persona: String::new(),
                allowed_tools: Vec::new(),
                excluded_tools: Vec::new(),
                denied_tools: Vec::new(),
                tools: vec![
                    make_tool("time_math", "utility", None),
                    make_tool("shell", "shell", None),
                ],
            }],
            // `core_utility` denies `time_math` by the executor's rule, and a
            // padded entry must still match after trimming.
            vec!["core_utility".to_string(), "  shell  ".to_string()],
        );

        let merged = context.merged_agent_tools().await.unwrap();
        assert!(
            !merged.iter().any(|tool| tool.name == "time_math"),
            "denying `core_utility` must remove `time_math`"
        );
        assert!(
            !merged.iter().any(|tool| tool.name == "shell"),
            "a padded deny entry must still match"
        );
    }
}
