//! GAUI Delta Emitter — translates agentic execution events into MUIJ deltas
//! and re-broadcasts them via `RuntimeTransportBroadcaster`.
//!
//! The emitter subscribes to the broadcast channel and maps the following event
//! types into `MuijDelta` values:
//!
//! - `AgenticExecutionStarted` — registers execution→agent mapping, emits Gauge(fill=0)
//! - `AgenticIterationStarted` — updates Gauge fill, emits TerminalTransient boundary line
//! - `AgenticDecisionMade` — emits TerminalTransient line
//! - `AgenticActionExecuted` — emits TerminalTransient line
//! - `AgenticWaitingForUser` — emits TerminalTransient line
//! - `AgenticWaitingForConfirmation` — emits TerminalTransient line
//! - `AgenticExecutionCompleted` — emits Gauge at 100% fill
//! - `AgentCycleStarted` — binds cycle_id to unbound executions
//! - `AgentCycleCompleted` — cleans up execution mappings and seq counters
//! - `agent.execution.mapping` (AgentEvent) — recovery path for lagged registrations
//!
//! A `execution_id → agent context` mapping (built from `AgenticExecutionStarted`)
//! allows subsequent per-execution events to be routed to the correct agent.
//! Cleanup happens on `AgentCycleCompleted`.
//!
//! Deltas are fed into a three-task coalescing pipeline before broadcast:
//! 1. **Event loop** (Task 1) — translates events, feeds coalescer
//! 2. **Coalescer** (Task 2) — merges coalescable upserts within 200ms window
//! 3. **Broadcast** (Task 3) — persists to `MuijStorage`, wraps in
//!    `AgentEventEnvelope`, broadcasts via `RuntimeTransportBroadcaster`
//!
//! - Gauge deltas are **coalescable** (last write wins within 200ms window).
//! - TerminalTransient deltas are **non-coalescable** (every line matters, R49).
//! The broadcast task wraps coalesced/pass-through deltas in `AgentEventEnvelope`
//! for broadcast, eliminating the double serialization from the original design (R51).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::gaui::coalesce::{CoalescedBatch, CoalescerInput, MuijCoalescer};
use crate::magician_v2::gaui::{
    agent_snapshot_cache_key, DefaultComponentRegistry, MuijComponent, MuijDelta, MuijDocument,
    MuijStorage,
};
use crate::magician_v2::realtime_events::{
    AgentEventEnvelope, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};

// ---------------------------------------------------------------------------
// R560/R679: Shared document cache for snapshot coherence
// ---------------------------------------------------------------------------

/// Shared in-memory cache of MUIJ documents. The broadcast task writes here
/// after applying deltas; the WebSocket snapshot handler reads here first
/// (falling back to disk when the agent isn't cached). This prevents stale
/// snapshots when the cache is ahead of disk.
pub type MuijDocumentCache = Arc<RwLock<HashMap<String, MuijDocument>>>;

// ---------------------------------------------------------------------------
// Internal tracking
// ---------------------------------------------------------------------------

/// Stale execution entries older than this are evicted during periodic sweeps.
/// 30 minutes is generous — agent cycles typically complete in seconds.
const STALE_ENTRY_SECS: u64 = 30 * 60;

/// Number of events between stale-entry sweeps. Keeps per-event overhead at
/// zero except once every N events.
const SWEEP_INTERVAL: u64 = 500;
/// R541: Time-based sweep fallback — sweep at least every 5 minutes even under low event rates.
const SWEEP_TIME_INTERVAL_SECS: u64 = 5 * 60;
/// Hard cap on tracked execution contexts to prevent memory exhaustion.
const MAX_EXECUTION_MAP_ENTRIES: usize = 10_000;
/// R450: Maximum characters for raw LLM reasoning/action_summary in terminal lines.
/// Prevents unbounded string growth in TerminalTransient deltas.
const MAX_REASONING_CHARS: usize = 2048;
/// R569/R645: Maximum components in a single MuijDocument layout.
/// Prevents unbounded growth when many upserts create new components.
/// 500 matches the doc_cache eviction threshold used elsewhere.
const MAX_LAYOUT_COMPONENTS: usize = 500;
/// R568/R647: Maximum entries in agent_seq_map. Uses same cap as execution_map
/// since there's at most one seq entry per unique agent_id.
const MAX_SEQ_MAP_ENTRIES: usize = MAX_EXECUTION_MAP_ENTRIES;

/// Per-execution agent context tracked by the emitter.
#[derive(Debug)]
struct TrackedAgent {
    agent_id: String,
    principal: Option<String>,
    workspace: Option<String>,
    max_iterations: usize,
    registered_at: Instant,
    /// Optional cycle binding used for overlap-safe cleanup.
    cycle_id: Option<String>,
}

impl TrackedAgent {
    fn cache_key(&self) -> String {
        agent_snapshot_cache_key(
            &self.agent_id,
            self.principal.as_deref(),
            self.workspace.as_deref(),
        )
    }

    fn matches_scope(&self, principal: Option<&str>, workspace: Option<&str>) -> bool {
        match (principal, workspace) {
            (Some(principal), Some(workspace)) => {
                self.principal.as_deref() == Some(principal)
                    && self.workspace.as_deref() == Some(workspace)
            },
            _ => self.principal.is_none() && self.workspace.is_none(),
        }
    }
}

#[derive(Debug, Clone)]
struct RouteMetadata {
    agent_id: String,
    principal: Option<String>,
    workspace: Option<String>,
}

impl RouteMetadata {
    fn cache_key(&self) -> String {
        agent_snapshot_cache_key(
            &self.agent_id,
            self.principal.as_deref(),
            self.workspace.as_deref(),
        )
    }
}

/// Selector entry for LiveSelectors component (GD-F01).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SelectorEntry {
    selector: String,
    action_type: String,
    timestamp: i64,
    outcome: String,
    dom_changes: Option<DomChangeSummary>,
}

/// DOM change summary for selector entries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DomChangeSummary {
    total: u32,
    added: u32,
    removed: u32,
}

/// Maximum selectors to keep per agent (GD-F01).
const MAX_SELECTORS_PER_AGENT: usize = 10;

/// Valid outcome values for selector entries (R953).
const VALID_OUTCOMES: &[&str] = &["success", "failed"];

/// Normalize outcome string to valid value (R953).
/// Falls back to "pending" for unknown values.
fn normalize_outcome(outcome: &str) -> String {
    if VALID_OUTCOMES.contains(&outcome) {
        outcome.to_string()
    } else {
        "pending".to_string()
    }
}

fn route_metadata(
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
) -> RouteMetadata {
    RouteMetadata {
        agent_id: agent_id.to_string(),
        principal: principal.map(str::to_string),
        workspace: workspace.map(str::to_string),
    }
}

fn parse_route_metadata(cache_key: &str) -> Option<RouteMetadata> {
    let mut parts = cache_key.splitn(3, '\0');
    let principal = parts.next()?;
    let workspace = parts.next()?;
    let agent_id = parts.next()?;
    Some(RouteMetadata {
        agent_id: agent_id.to_string(),
        principal: (!principal.is_empty()).then(|| principal.to_string()),
        workspace: (!workspace.is_empty()).then(|| workspace.to_string()),
    })
}

// ---------------------------------------------------------------------------
// EmittedDelta — output of handle_event (R51: no double serialization)
// ---------------------------------------------------------------------------

/// A raw delta with routing metadata, produced by `handle_event()`.
/// Replaces the previous `AgentEventEnvelope` return, eliminating the
/// serialize → deserialize round-trip in the coalescer pipeline (R51).
///
/// R116: Delta payload size bounds. The `data` field (serde_json::Value) in each
/// `MuijDelta::Upsert` is constructed entirely within `handle_event()` from
/// bounded event fields — `execution_id`, `agent_id`, `action_type`, `target`,
/// `decision_type`, `reasoning`, `action_summary`, `question`, and `input_type`
/// are all bounded strings originating from the agentic event system. No
/// arbitrary user data is passed through. The `reasoning` field is truncated
/// to `MAX_REASONING_CHARS` (R450) to bound raw LLM output. A theoretical
/// upper bound is well under 64 KB per delta, so no runtime size check is
/// necessary.
#[derive(Debug)]
pub struct EmittedDelta {
    pub agent_id: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub cache_key: String,
    pub delta: MuijDelta,
    /// If true, the coalescer may merge this with other upserts for the same
    /// `component_id` (last write wins). If false, the delta passes through
    /// immediately — used for log-style components like TerminalTransient
    /// where every entry matters (R49).
    pub coalescable: bool,
}

// ---------------------------------------------------------------------------
// MuijDeltaEmitter
// ---------------------------------------------------------------------------

pub struct MuijDeltaEmitter {
    /// execution_id → agent context
    execution_map: HashMap<String, TrackedAgent>,
    /// Latest active cycle per agent, from AgentCycleStarted.
    /// Used to suppress stale-cycle selector updates during overlap windows (R970).
    active_cycle_map: HashMap<String, String>,
    /// Per-agent monotonic counter for TerminalTransient dedup (R17, R85).
    /// Keyed by agent_id (not execution_id) so multi-execution agents share one
    /// counter, preventing seq collisions on the shared terminal component.
    agent_seq_map: HashMap<String, u64>,
    /// Per-agent selector history for LiveSelectors component (GD-F01).
    /// Rolling buffer of recent selectors, capped at MAX_SELECTORS_PER_AGENT.
    agent_selectors_map: HashMap<String, Vec<SelectorEntry>>,
    /// Monotonic event counter for periodic sweeps.
    event_count: u64,
    /// R541: Last time a stale-entry sweep was performed.
    last_sweep_at: Instant,
    /// R97: Cumulative count of events dropped due to broadcast channel lag.
    /// When the emitter cannot keep up with the broadcaster, tokio's broadcast
    /// channel drops oldest events. Registration events (AgenticExecutionStarted)
    /// lost during lag windows cannot be replayed — subsequent deltas for those
    /// agents are silently discarded until their next execution cycle.
    cumulative_lagged: u64,
}

impl std::fmt::Debug for MuijDeltaEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuijDeltaEmitter")
            .field("execution_map", &self.execution_map)
            .field("active_cycle_map", &self.active_cycle_map)
            .field("agent_seq_map", &self.agent_seq_map)
            .field("agent_selectors_map", &self.agent_selectors_map)
            .field("event_count", &self.event_count)
            .field("last_sweep_at", &self.last_sweep_at)
            .field("cumulative_lagged", &self.cumulative_lagged)
            .finish_non_exhaustive()
    }
}

impl MuijDeltaEmitter {
    fn hinted_route_for_agent(&self, agent_id: &str) -> Option<RouteMetadata> {
        let mut matches = self
            .active_cycle_map
            .keys()
            .filter_map(|cache_key| parse_route_metadata(cache_key))
            .filter(|route| route.agent_id == agent_id);
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first)
        }
    }

    /// Process a broadcast event and return zero or more emitted deltas.
    ///
    /// Returns `EmittedDelta` values with routing metadata and coalescability
    /// flags — no serialization happens here (R51). Serialization is deferred
    /// to the broadcast task.
    ///
    /// This is a pure, synchronous function — no async, no I/O — so it can be
    /// called directly from unit tests without a tokio runtime.
    fn handle_event(&mut self, event: &RuntimeTransportEvent) -> Vec<EmittedDelta> {
        self.maybe_sweep_stale_entries();

        match event {
            // -- Execution started: register execution → agent mapping, emit Gauge(fill=0)
            RuntimeTransportEvent::AgenticExecutionStarted {
                execution_id,
                principal,
                workspace,
                agent_id: Some(aid),
                max_iterations,
                ..
            } => {
                // R106: Hard cap on execution_map size to prevent memory exhaustion
                // R526: Trigger an immediate stale sweep before rejecting — clears
                // entries older than 30 min, giving new agents a recovery path
                // instead of permanent lockout.
                if self.execution_map.len() >= MAX_EXECUTION_MAP_ENTRIES {
                    self.force_sweep_stale_entries();
                    if self.execution_map.len() >= MAX_EXECUTION_MAP_ENTRIES {
                        warn!(execution_id = %execution_id, cap = MAX_EXECUTION_MAP_ENTRIES, "execution_map at capacity after sweep, rejecting new entry");
                        return vec![];
                    }
                }
                // R289: Don't carry over stale cycle_id from prior registration —
                // the new execution may belong to a different cycle. AgentCycleStarted
                // or agent.execution.mapping will bind the correct cycle_id later.
                // R509: Normalize max_iterations=0 → 1 consistently with recovery path
                let safe_max_iterations = (*max_iterations).max(1);
                // R474: If execution is already registered for the same agent, update
                // without resetting cycle_id — avoids erasing binding and Gauge flash
                if let Some(existing) = self.execution_map.get_mut(execution_id) {
                    if existing.agent_id == *aid {
                        existing.principal = principal.clone();
                        existing.workspace = workspace.clone();
                        existing.max_iterations = safe_max_iterations;
                        existing.registered_at = Instant::now();
                        // Don't reset cycle_id — keep existing binding
                    } else {
                        // Different agent re-using execution ID — full replacement
                        self.execution_map.insert(
                            execution_id.clone(),
                            TrackedAgent {
                                agent_id: aid.clone(),
                                principal: principal.clone(),
                                workspace: workspace.clone(),
                                max_iterations: safe_max_iterations,
                                registered_at: Instant::now(),
                                cycle_id: None,
                            },
                        );
                    }
                } else {
                    self.execution_map.insert(
                        execution_id.clone(),
                        TrackedAgent {
                            agent_id: aid.clone(),
                            principal: principal.clone(),
                            workspace: workspace.clone(),
                            max_iterations: safe_max_iterations,
                            registered_at: Instant::now(),
                            cycle_id: None,
                        },
                    );
                }
                if let Some(hinted_route) = self.hinted_route_for_agent(aid) {
                    if let Some(existing) = self.execution_map.get_mut(execution_id) {
                        if existing.principal.is_none() && existing.workspace.is_none() {
                            existing.principal = hinted_route.principal.clone();
                            existing.workspace = hinted_route.workspace.clone();
                        }
                    }
                }
                let route = self
                    .execution_map
                    .get(execution_id)
                    .map(|ctx| {
                        route_metadata(
                            &ctx.agent_id,
                            ctx.principal.as_deref(),
                            ctx.workspace.as_deref(),
                        )
                    })
                    .unwrap_or_else(|| route_metadata(aid, None, None));
                vec![EmittedDelta {
                    agent_id: aid.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-gauge", aid),
                        data: serde_json::json!({
                            "component_type": "Gauge",
                            "label": "Execution Progress",
                            "props": { "fill": 0.0, "max_iterations": safe_max_iterations }
                        }),
                    },
                    coalescable: true,
                }]
            },

            // Recovery channel for lagged-registration windows:
            // executor emits this generic mapping periodically so execution routing
            // can recover even if AgenticExecutionStarted was dropped.
            RuntimeTransportEvent::AgentEvent { event }
                if event.event_type == "agent.execution.mapping" =>
            {
                let Some(payload) = event.payload.as_object() else {
                    return vec![];
                };
                let Some(execution_id) = payload.get("execution_id").and_then(|v| v.as_str())
                else {
                    return vec![];
                };
                let Some(agent_id) = payload.get("agent_id").and_then(|v| v.as_str()) else {
                    return vec![];
                };
                let max_iterations = payload
                    .get("max_iterations")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(1)
                    .max(1);
                let cycle_id = payload
                    .get("cycle_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                if self.execution_map.len() >= MAX_EXECUTION_MAP_ENTRIES
                    && !self.execution_map.contains_key(execution_id)
                {
                    // R526: Sweep before rejecting
                    self.force_sweep_stale_entries();
                    if self.execution_map.len() >= MAX_EXECUTION_MAP_ENTRIES
                        && !self.execution_map.contains_key(execution_id)
                    {
                        warn!(
                            execution_id = %execution_id, cap = MAX_EXECUTION_MAP_ENTRIES,
                            "execution_map at capacity after sweep, rejecting mapping event"
                        );
                        return vec![];
                    }
                }

                // R742: If the execution is already registered for the same agent,
                // update without overwriting an existing cycle_id binding.
                // Recovery mappings may arrive without cycle_id, and should not
                // downgrade a bound entry to unbound.
                if let Some(existing) = self.execution_map.get_mut(execution_id) {
                    if existing.agent_id == agent_id {
                        existing.principal = event.principal.clone();
                        existing.workspace = event.workspace.clone();
                        existing.max_iterations = max_iterations;
                        existing.registered_at = Instant::now();
                        if let Some(ref cid) = cycle_id {
                            existing.cycle_id = Some(cid.clone());
                        }
                        return vec![];
                    }
                }
                self.execution_map.insert(
                    execution_id.to_string(),
                    TrackedAgent {
                        agent_id: agent_id.to_string(),
                        principal: event.principal.clone(),
                        workspace: event.workspace.clone(),
                        max_iterations,
                        registered_at: Instant::now(),
                        cycle_id,
                    },
                );
                vec![]
            },

            // If lifecycle start arrives after execution start, bind any unbound
            // executions for this agent to the concrete cycle ID.
            RuntimeTransportEvent::AgentCycleStarted {
                principal,
                workspace,
                agent_id,
                cycle_id,
                ..
            } => {
                let event_cache_key =
                    agent_snapshot_cache_key(agent_id, principal.as_deref(), workspace.as_deref());
                let previous_active_cycle = self
                    .active_cycle_map
                    .insert(event_cache_key.clone(), cycle_id.clone());
                // R965/R971: Clear selector history on a true cycle transition.
                //
                // Use the previous active-cycle marker rather than execution-map
                // prebinding. Mapping events can legitimately arrive before the
                // first AgentCycleStarted for a new cycle; that ordering should
                // still reset stale selector history.
                //
                // Preserve history only for duplicate starts of the same cycle.
                let duplicate_start = previous_active_cycle.as_deref() == Some(cycle_id.as_str());
                if !duplicate_start {
                    self.agent_selectors_map.remove(&event_cache_key);
                }
                for ctx in self.execution_map.values_mut() {
                    if ctx.agent_id == *agent_id
                        && ctx.cycle_id.is_none()
                        && ctx.matches_scope(principal.as_deref(), workspace.as_deref())
                    {
                        ctx.cycle_id = Some(cycle_id.clone());
                    }
                }
                vec![]
            },

            // No agent_id → orchestrator-driven, nothing to emit
            RuntimeTransportEvent::AgenticExecutionStarted { agent_id: None, .. } => vec![],

            // -- Iteration started: update Gauge fill + emit TerminalTransient boundary line (R16)
            RuntimeTransportEvent::AgenticIterationStarted {
                execution_id,
                iteration,
                ..
            } => {
                let (route, fill, max_iters) = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "IterationStarted", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    let fill = (*iteration as f64 / ctx.max_iterations.max(1) as f64).min(1.0);
                    (
                        route_metadata(
                            &ctx.agent_id,
                            ctx.principal.as_deref(),
                            ctx.workspace.as_deref(),
                        ),
                        fill,
                        ctx.max_iterations,
                    )
                };
                let agent_id = route.agent_id.clone();

                // Per-agent seq counter — shared across all executions for this agent (R85)
                let seq = self.next_agent_seq(route.cache_key().as_str());

                let boundary_line = format!("[iter {}] started", iteration);
                let gauge_id = format!("{}-gauge", agent_id);
                let terminal_id = format!("{}-terminal", agent_id);

                vec![
                    // Gauge: coalescable (last value wins)
                    EmittedDelta {
                        agent_id: agent_id.clone(),
                        principal: route.principal.clone(),
                        workspace: route.workspace.clone(),
                        cache_key: route.cache_key(),
                        delta: MuijDelta::Upsert {
                            component_id: gauge_id,
                            data: serde_json::json!({
                                "component_type": "Gauge",
                                "label": "Execution Progress",
                                "props": { "fill": fill, "iteration": iteration, "max_iterations": max_iters }
                            }),
                        },
                        coalescable: true,
                    },
                    // TerminalTransient: NOT coalescable — every line matters (R49)
                    EmittedDelta {
                        agent_id,
                        principal: route.principal.clone(),
                        workspace: route.workspace.clone(),
                        cache_key: route.cache_key(),
                        delta: MuijDelta::Upsert {
                            component_id: terminal_id,
                            data: serde_json::json!({
                                "component_type": "TerminalTransient",
                                "label": "Agent Log",
                                "props": { "line": boundary_line, "seq": seq }
                            }),
                        },
                        coalescable: false,
                    },
                ]
            },

            // -- Decision made: TerminalTransient line
            RuntimeTransportEvent::AgenticDecisionMade {
                execution_id,
                decision_type,
                reasoning,
                action_summary,
                ..
            } => {
                let route = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "DecisionMade", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    route_metadata(
                        &ctx.agent_id,
                        ctx.principal.as_deref(),
                        ctx.workspace.as_deref(),
                    )
                };
                let agent_id = route.agent_id.clone();
                // R450: Truncate raw LLM text to MAX_REASONING_CHARS to bound delta size
                let raw_text = action_summary.as_deref().unwrap_or(reasoning);
                let truncated = truncate_str(raw_text, MAX_REASONING_CHARS);
                let line = format!("[{}] {}", decision_type, truncated);
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- Action executed: TerminalTransient line
            RuntimeTransportEvent::AgenticActionExecuted {
                execution_id,
                action_type,
                target,
                success,
                error,
                ..
            } => {
                let route = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "ActionExecuted", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    route_metadata(
                        &ctx.agent_id,
                        ctx.principal.as_deref(),
                        ctx.workspace.as_deref(),
                    )
                };
                let agent_id = route.agent_id.clone();
                let status = if *success { "ok" } else { "err" };
                // R643: Truncate target/error to bound TerminalTransient line size
                let truncated_target = truncate_str(target, MAX_REASONING_CHARS);
                let line = match error {
                    Some(e) => {
                        let truncated_error = truncate_str(e, MAX_REASONING_CHARS);
                        format!(
                            "[{} {}] {} → {}",
                            action_type, status, truncated_target, truncated_error
                        )
                    },
                    None => format!("[{} {}] {}", action_type, status, truncated_target),
                };
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- R111: Waiting for user input — TerminalTransient line.
            // Post-H7.4 lifecycle slim: the HITL payload (question /
            // input_type) lives on canonical `HitlRequested`; this
            // legacy variant is a lifecycle marker only, so the terminal
            // line is now generic. The richer detail is in the canonical
            // envelope — render that on surfaces that consume it
            // directly.
            RuntimeTransportEvent::AgenticWaitingForUser { execution_id, .. } => {
                let route = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "WaitingForUser", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    route_metadata(
                        &ctx.agent_id,
                        ctx.principal.as_deref(),
                        ctx.workspace.as_deref(),
                    )
                };
                let agent_id = route.agent_id.clone();
                let line = "[waiting] Awaiting user input".to_string();
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- R111: Waiting for confirmation — TerminalTransient line.
            // Same lifecycle-slim story as above; the HITL payload now
            // lives on canonical `HitlRequested { source: "agentic",
            // input_schema.action_type }`.
            RuntimeTransportEvent::AgenticWaitingForConfirmation { execution_id, .. } => {
                let route = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "WaitingForConfirmation", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    route_metadata(
                        &ctx.agent_id,
                        ctx.principal.as_deref(),
                        ctx.workspace.as_deref(),
                    )
                };
                let agent_id = route.agent_id.clone();
                let line = "[waiting] Awaiting confirmation".to_string();
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- Execution completed: emit Gauge at 100% fill (R110) + TerminalTransient (R739)
            RuntimeTransportEvent::AgenticExecutionCompleted {
                execution_id,
                outcome,
                ..
            } => {
                let Some(ctx) = self.execution_map.get(execution_id) else {
                    debug!(execution_id = %execution_id, event = "ExecutionCompleted", "Unregistered execution — delta dropped (R471)");
                    return vec![];
                };
                let route = route_metadata(
                    &ctx.agent_id,
                    ctx.principal.as_deref(),
                    ctx.workspace.as_deref(),
                );
                let agent_id = route.agent_id.clone();
                let max_iters = ctx.max_iterations;
                let success = outcome == "success";
                let label = if success {
                    "Execution Complete"
                } else {
                    "Execution Failed"
                };
                // R739: Emit TerminalTransient in addition to Gauge
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![
                    EmittedDelta {
                        agent_id: agent_id.clone(),
                        principal: route.principal.clone(),
                        workspace: route.workspace.clone(),
                        cache_key: route.cache_key(),
                        delta: MuijDelta::Upsert {
                            component_id: format!("{}-gauge", agent_id),
                            data: serde_json::json!({
                                "component_type": "Gauge",
                                "label": label,
                                "props": { "fill": 1.0, "iteration": max_iters, "max_iterations": max_iters }
                            }),
                        },
                        coalescable: true,
                    },
                    EmittedDelta {
                        agent_id: agent_id.clone(),
                        principal: route.principal.clone(),
                        workspace: route.workspace.clone(),
                        cache_key: route.cache_key(),
                        delta: MuijDelta::Upsert {
                            component_id: format!("{}-terminal", agent_id),
                            data: serde_json::json!({
                                "component_type": "TerminalTransient",
                                "label": "Agent Log",
                                "props": { "line": format!("[completed] {}", label), "seq": seq }
                            }),
                        },
                        coalescable: false,
                    },
                ]
            },

            // -- Cycle completed: clean up execution mappings and seq counter for this agent
            RuntimeTransportEvent::AgentCycleCompleted {
                principal,
                workspace,
                agent_id,
                cycle_id,
                ..
            } => {
                let event_cache_key =
                    agent_snapshot_cache_key(agent_id, principal.as_deref(), workspace.as_deref());
                let before = self.execution_map.len();
                let now = Instant::now();
                // R363: Also remove entries with cycle_id: None for this agent,
                // but R379/R463: only if they were registered more than 5 seconds ago.
                // Freshly-registered entries (< 5s) with cycle_id: None likely belong
                // to a new overlapping cycle that hasn't received its AgentCycleStarted yet.
                let freshness_threshold = Duration::from_secs(5);
                self.execution_map.retain(|_, ctx| {
                    if ctx.agent_id != *agent_id {
                        return true; // different agent — keep
                    }
                    if !ctx.matches_scope(principal.as_deref(), workspace.as_deref()) {
                        return true; // same id, different scope — keep
                    }
                    if ctx.cycle_id.as_deref() == Some(cycle_id.as_str()) {
                        return false; // matching cycle — remove
                    }
                    if ctx.cycle_id.is_none()
                        && now.duration_since(ctx.registered_at) > freshness_threshold
                    {
                        return false; // stale unbound entry — remove (R363)
                    }
                    true // fresh unbound or different cycle — keep (R379/R463)
                });
                let removed = before.saturating_sub(self.execution_map.len());
                debug!(
                    agent_id = %agent_id, cycle_id = %cycle_id, removed_executions = removed,
                    "Cycle cleanup complete"
                );
                if self.active_cycle_map.get(&event_cache_key) == Some(cycle_id) {
                    self.active_cycle_map.remove(&event_cache_key);
                }
                if !self.execution_map.values().any(|ctx| {
                    ctx.agent_id == *agent_id
                        && ctx.matches_scope(principal.as_deref(), workspace.as_deref())
                }) {
                    self.active_cycle_map.remove(&event_cache_key);
                    self.agent_seq_map.remove(&event_cache_key);
                    self.agent_selectors_map.remove(&event_cache_key);
                }
                vec![]
            },

            // R640: Resumed after user input — emit TerminalTransient line
            RuntimeTransportEvent::AgenticResumed {
                execution_id,
                resumed_from_iteration,
                input_type,
                user_responded,
                ..
            } => {
                let route = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "Resumed", "Unregistered execution — delta dropped (R471)");
                        return vec![];
                    };
                    route_metadata(
                        &ctx.agent_id,
                        ctx.principal.as_deref(),
                        ctx.workspace.as_deref(),
                    )
                };
                let agent_id = route.agent_id.clone();
                let status = if *user_responded {
                    "input provided"
                } else {
                    "aborted"
                };
                let line = format!(
                    "[resumed] Execution resumed from iteration {} ({}: {})",
                    resumed_from_iteration, input_type, status
                );
                let seq = self.next_agent_seq(route.cache_key().as_str());
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- GD-B01: DOM change detected via SSE backchannel
            // -- GD-F01: Also emit LiveSelectors delta for element-targeting actions
            RuntimeTransportEvent::DomChangeDetected {
                execution_id,
                total_changes,
                nodes_added,
                nodes_removed,
                signals,
                initial_url,
                final_url,
                action_type,
                selector,
                outcome,
                timestamp,
                ..
            } => {
                let (route, execution_cycle_id) = {
                    let Some(ctx) = self.execution_map.get(execution_id) else {
                        debug!(execution_id = %execution_id, event = "DomChangeDetected", "Unregistered execution — delta dropped");
                        return vec![];
                    };
                    (
                        route_metadata(
                            &ctx.agent_id,
                            ctx.principal.as_deref(),
                            ctx.workspace.as_deref(),
                        ),
                        ctx.cycle_id.clone(),
                    )
                };
                let agent_id = route.agent_id.clone();
                let cache_key = route.cache_key();
                let active_cycle_id = self.active_cycle_map.get(&cache_key).cloned();

                let mut parts = vec![format!("{} changes", total_changes)];
                if *nodes_added > 0 {
                    parts.push(format!("+{} nodes", nodes_added));
                }
                if *nodes_removed > 0 {
                    parts.push(format!("-{} nodes", nodes_removed));
                }
                if let (Some(initial), Some(final_)) = (initial_url, final_url) {
                    if initial != final_ {
                        parts.push("url→".to_string());
                    }
                }
                if !signals.is_empty() {
                    parts.push(format!("signals:{}", signals.len()));
                }

                let line = format!("[dom] {}", parts.join(", "));
                let seq = self.next_agent_seq(cache_key.as_str());

                let mut deltas = vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: cache_key.clone(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": { "line": line, "seq": seq }
                        }),
                    },
                    coalescable: false,
                }];

                if let (Some(at), Some(sel)) = (action_type, selector) {
                    if !sel.is_empty() {
                        // R970: During overlap windows, suppress selector updates from
                        // threads bound to a non-active cycle for this agent.
                        let stale_cycle_event =
                            match (active_cycle_id.as_deref(), execution_cycle_id.as_deref()) {
                                (Some(active_cycle), Some(thread_cycle)) => {
                                    thread_cycle != active_cycle
                                },
                                _ => false,
                            };
                        if stale_cycle_event {
                            debug!(
                                execution_id = %execution_id,
                                agent_id = %agent_id,
                                active_cycle_id = ?active_cycle_id,
                                execution_cycle_id = ?execution_cycle_id,
                                "Skipping stale-cycle LiveSelectors update"
                            );
                            return deltas;
                        }
                        let entries = self
                            .agent_selectors_map
                            .entry(cache_key.clone())
                            .or_default();
                        entries.push(SelectorEntry {
                            selector: sel.clone(),
                            action_type: at.clone(),
                            timestamp: *timestamp,
                            outcome: normalize_outcome(outcome),
                            dom_changes: Some(DomChangeSummary {
                                total: *total_changes,
                                added: *nodes_added,
                                removed: *nodes_removed,
                            }),
                        });
                        while entries.len() > MAX_SELECTORS_PER_AGENT {
                            entries.remove(0);
                        }

                        deltas.push(EmittedDelta {
                            agent_id: agent_id.clone(),
                            principal: route.principal.clone(),
                            workspace: route.workspace.clone(),
                            cache_key: cache_key.clone(),
                            delta: MuijDelta::Upsert {
                                component_id: format!("{}-selectors", agent_id),
                                data: serde_json::json!({
                                    "component_type": "LiveSelectors",
                                    "label": "Target Elements",
                                    "props": {
                                        "selectors": entries,
                                        "maxEntries": MAX_SELECTORS_PER_AGENT
                                    }
                                }),
                            },
                            coalescable: true,
                        });
                    }
                }

                deltas
            },

            // -- Sub-goal requested: emit TerminalTransient for visibility
            RuntimeTransportEvent::SubGoalRequested {
                execution_id,
                sub_goal,
                budget_iterations,
                depth,
                ..
            } => {
                let Some(ctx) = self.execution_map.get(execution_id) else {
                    return vec![];
                };
                let route = route_metadata(
                    &ctx.agent_id,
                    ctx.principal.as_deref(),
                    ctx.workspace.as_deref(),
                );
                let agent_id = route.agent_id.clone();
                let seq = self.next_agent_seq(route.cache_key().as_str());
                let truncated_goal: String = if sub_goal.chars().count() > 60 {
                    format!("{}...", sub_goal.chars().take(57).collect::<String>())
                } else {
                    sub_goal.clone()
                };
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": {
                                "line": format!("[sub-goal depth={}] spawning: {} (budget={})", depth, truncated_goal, budget_iterations),
                                "seq": seq
                            }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // -- Sub-goal outcome: emit TerminalTransient for visibility
            RuntimeTransportEvent::SubGoalOutcome {
                execution_id,
                sub_goal,
                outcome,
                iterations_used,
                duration_ms,
                ..
            } => {
                let Some(ctx) = self.execution_map.get(execution_id) else {
                    return vec![];
                };
                let route = route_metadata(
                    &ctx.agent_id,
                    ctx.principal.as_deref(),
                    ctx.workspace.as_deref(),
                );
                let agent_id = route.agent_id.clone();
                let seq = self.next_agent_seq(route.cache_key().as_str());
                let truncated_goal: String = if sub_goal.chars().count() > 40 {
                    format!("{}...", sub_goal.chars().take(37).collect::<String>())
                } else {
                    sub_goal.clone()
                };
                vec![EmittedDelta {
                    agent_id: agent_id.clone(),
                    principal: route.principal.clone(),
                    workspace: route.workspace.clone(),
                    cache_key: route.cache_key(),
                    delta: MuijDelta::Upsert {
                        component_id: format!("{}-terminal", agent_id),
                        data: serde_json::json!({
                            "component_type": "TerminalTransient",
                            "label": "Agent Log",
                            "props": {
                                "line": format!("[sub-goal] {}: {} (iters={}, {}ms)", outcome, truncated_goal, iterations_used, duration_ms),
                                "seq": seq
                            }
                        }),
                    },
                    coalescable: false,
                }]
            },

            // IMPORTANT: AgentEvent must remain empty — the emitter re-broadcasts
            // deltas as AgentEvent, so matching here would create an infinite loop.
            RuntimeTransportEvent::AgentEvent { .. } => vec![],

            _ => vec![],
        }
    }

    /// Get-and-increment the per-agent seq counter (R85).
    /// Returns the current value and advances the counter for next call.
    fn next_agent_seq(&mut self, agent_id: &str) -> u64 {
        // R118: Avoid allocation when entry already exists
        if let Some(counter) = self.agent_seq_map.get_mut(agent_id) {
            let seq = *counter;
            *counter += 1;
            return seq;
        }
        // R568/R647: Cap agent_seq_map to prevent unbounded growth from
        // agents that never receive AgentCycleCompleted (e.g. crashes).
        // Evict entries for agents no longer in execution_map before rejecting.
        if self.agent_seq_map.len() >= MAX_SEQ_MAP_ENTRIES {
            let active_agents: std::collections::HashSet<String> = self
                .execution_map
                .values()
                .map(TrackedAgent::cache_key)
                .collect();
            self.agent_seq_map
                .retain(|id, _| active_agents.contains(id));
            if self.agent_seq_map.len() >= MAX_SEQ_MAP_ENTRIES {
                warn!(
                    cap = MAX_SEQ_MAP_ENTRIES,
                    "agent_seq_map at capacity after eviction — new agent starts at seq 0 (R647)"
                );
            }
        }
        self.agent_seq_map.insert(agent_id.to_string(), 1);
        0
    }

    /// Periodically evict execution_map entries older than `STALE_ENTRY_SECS`.
    /// Guards against unbounded growth when `AgentCycleCompleted` is never
    /// emitted (e.g. agent process crash).
    fn maybe_sweep_stale_entries(&mut self) {
        self.event_count += 1;
        let time_elapsed = self.last_sweep_at.elapsed().as_secs() >= SWEEP_TIME_INTERVAL_SECS;
        // R541: Sweep on event count OR time-based fallback (whichever fires first)
        if !self.event_count.is_multiple_of(SWEEP_INTERVAL) && !time_elapsed {
            return;
        }
        self.force_sweep_stale_entries();
    }

    /// R470: Expose cumulative broadcast-lag event count for external monitoring.
    /// Callers (health endpoints, metrics) can poll this to detect channel saturation.
    #[allow(dead_code)]
    pub fn cumulative_lagged(&self) -> u64 {
        self.cumulative_lagged
    }

    /// R526: Unconditional stale-entry sweep. Called from `maybe_sweep_stale_entries`
    /// and also on-demand when execution_map is at capacity.
    fn force_sweep_stale_entries(&mut self) {
        self.last_sweep_at = Instant::now();
        let cutoff = Instant::now() - std::time::Duration::from_secs(STALE_ENTRY_SECS);
        let before = self.execution_map.len();
        self.execution_map
            .retain(|_, ctx| ctx.registered_at > cutoff);
        let evicted = before - self.execution_map.len();
        if evicted > 0 {
            debug!(
                evicted_executions = evicted,
                ttl_secs = STALE_ENTRY_SECS,
                "Evicted stale execution_map entries"
            );
        }

        // R957: keep selector history bounded to active agents tracked in execution_map.
        // If AgentCycleCompleted is missed (crash/disconnect), stale-execution eviction is
        // the fallback lifecycle boundary for reclaiming selector buffers.
        let active_agents: std::collections::HashSet<String> = self
            .execution_map
            .values()
            .map(TrackedAgent::cache_key)
            .collect();
        self.active_cycle_map
            .retain(|agent_id, _| active_agents.contains(agent_id));
        let selectors_before = self.agent_selectors_map.len();
        self.agent_selectors_map
            .retain(|agent_id, _| active_agents.contains(agent_id));
        let evicted_selector_agents =
            selectors_before.saturating_sub(self.agent_selectors_map.len());
        if evicted_selector_agents > 0 {
            debug!(
                evicted_agents = evicted_selector_agents,
                "Evicted stale LiveSelectors histories"
            );
        }

        // R462: Do NOT sweep agent_seq_map here. Seq counters are lightweight (u64)
        // and removing them causes non-monotonic regression if the agent re-registers.
        // Seq counters are cleaned up only in AgentCycleCompleted, which is the
        // proper lifecycle boundary. The seq_map is bounded by MAX_EXECUTION_MAP_ENTRIES
        // (max one entry per unique agent_id, far fewer than thread entries).
    }

    /// Spawn a background task that subscribes to the broadcaster, translates
    /// agentic events into MUIJ deltas, coalesces rapid upserts, persists
    /// documents to `MuijStorage`, and re-broadcasts via `AgentEventEnvelope`.
    ///
    /// Uses the default coalescing window (200ms). Returns the task handle and
    /// a shared document cache that snapshot handlers can read from (R560/R679).
    pub fn spawn(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        muij_storage: MuijStorage,
    ) -> (tokio::task::JoinHandle<()>, MuijDocumentCache) {
        Self::spawn_with_coalescing_inner(
            broadcaster,
            MuijCoalescer::default_window(),
            None,
            Some(muij_storage),
        )
    }

    pub fn spawn_scoped(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        workspace_layout: ArtifactV2Workspace,
    ) -> (tokio::task::JoinHandle<()>, MuijDocumentCache) {
        let default_storage = MuijStorage::new(
            workspace_layout
                .scoped_agent_runtime_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
        );
        Self::spawn_with_coalescing_inner(
            broadcaster,
            MuijCoalescer::default_window(),
            Some(workspace_layout),
            Some(default_storage),
        )
    }

    /// Spawn with a configurable coalescing window.
    ///
    /// Three internal tasks are created:
    /// 1. **Event loop** — subscribes to broadcaster, runs `handle_event`, feeds
    ///    `EmittedDelta` tuples directly into the coalescer (no serialization, R51).
    /// 2. **Coalescer** — merges coalescable upserts per agent within `coalescing_window`,
    ///    passes non-coalescable upserts/Remove/Reorder through immediately (R49).
    /// 3. **Broadcast task** — reads coalesced batches, serializes deltas into
    ///    `AgentEventEnvelope`, and broadcasts. Single serialization point (R51).
    ///
    /// Returns the event loop handle and a shared document cache (R560/R679).
    pub fn spawn_with_coalescing(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        coalescing_window: Duration,
    ) -> (tokio::task::JoinHandle<()>, MuijDocumentCache) {
        Self::spawn_with_coalescing_inner(broadcaster, coalescing_window, None, None)
    }

    fn spawn_with_coalescing_inner(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        coalescing_window: Duration,
        workspace_layout: Option<ArtifactV2Workspace>,
        muij_storage: Option<MuijStorage>,
    ) -> (tokio::task::JoinHandle<()>, MuijDocumentCache) {
        let coalescer = MuijCoalescer::new(coalescing_window);
        let (coal_tx, coal_rx) = mpsc::channel::<CoalescerInput>(256);
        let (batch_tx, mut batch_rx) = mpsc::channel::<CoalescedBatch>(256);

        // R560/R679: Shared document cache — readable by snapshot handlers
        let shared_cache: MuijDocumentCache = Arc::new(RwLock::new(HashMap::new()));
        let cache_for_broadcast = Arc::clone(&shared_cache);

        // Scoped route metadata and latest execution id per routed agent cache key.
        let route_exec_map: Arc<RwLock<HashMap<String, String>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let route_exec_map_for_broadcast = Arc::clone(&route_exec_map);
        let route_meta_map: Arc<RwLock<HashMap<String, RouteMetadata>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let route_meta_map_for_broadcast = Arc::clone(&route_meta_map);

        // Task 2: Coalescer — merges coalescable upserts within the window
        let coalescer_handle = tokio::spawn(async move {
            coalescer.run(coal_rx, batch_tx).await;
            debug!("[GAUI-EMITTER] Coalescer task exiting normally");
        });

        // Task 3: Broadcast — reads coalesced batches, persists, then re-broadcasts.
        // Single serialization point for the entire pipeline (R51).
        // R162: Persist-before-broadcast ordering ensures storage is durable
        //       before clients can request a snapshot of the new state.
        let bcast = Arc::clone(&broadcaster);
        let storage_opt = muij_storage.clone();
        let workspace_layout_for_broadcast = workspace_layout.clone();
        let broadcast_handle = tokio::spawn(async move {
            let mut doc_cache: HashMap<String, MuijDocument> = HashMap::new();
            // R237: Track consecutive persist failures per agent for log rate-limiting
            let mut persist_fail_count: HashMap<String, u32> = HashMap::new();
            // R244: Per-agent quarantine — skip persist attempts until backoff expires
            let mut quarantine_until: HashMap<String, Instant> = HashMap::new();
            // R268/R423: Periodic sweep of stale tracking entries from decommissioned
            // agents. Runs every 500 batches to limit transient memory pressure from
            // agent churn between sweeps.
            let mut batch_count: u64 = 0;
            // R691: Track last batch number per agent for stale doc_cache eviction
            let mut last_batch_seen: HashMap<String, u64> = HashMap::new();
            while let Some((cache_key, deltas)) = batch_rx.recv().await {
                let route = route_meta_map_for_broadcast
                    .read()
                    .await
                    .get(&cache_key)
                    .cloned()
                    .or_else(|| parse_route_metadata(&cache_key))
                    .unwrap_or_else(|| route_metadata(&cache_key, None, None));
                let agent_id = route.agent_id.clone();
                batch_count += 1;
                last_batch_seen.insert(cache_key.clone(), batch_count);
                if batch_count.is_multiple_of(500) {
                    let now = Instant::now();
                    // Remove expired quarantine entries (agent stopped sending batches)
                    quarantine_until.retain(|_, until| *until > now);
                    // Remove orphaned fail counts: agent not in cache and not quarantined
                    persist_fail_count.retain(|id, _| {
                        doc_cache.contains_key(id) || quarantine_until.contains_key(id)
                    });
                    // R691: Evict doc_cache entries for agents that haven't produced
                    // deltas in the last 2000 batches (completed or crashed agents).
                    // Quarantined agents are retained — they need cache-only mode.
                    let stale_threshold = batch_count.saturating_sub(2000);
                    let before_cache = doc_cache.len();
                    doc_cache.retain(|id, _| {
                        last_batch_seen.get(id).copied().unwrap_or(0) > stale_threshold
                            || quarantine_until.contains_key(id)
                    });
                    let evicted_cache = before_cache.saturating_sub(doc_cache.len());
                    if evicted_cache > 0 {
                        debug!(
                            evicted = evicted_cache,
                            remaining = doc_cache.len(),
                            "Periodic doc_cache eviction (R691)"
                        );
                    }
                    // Clean up corresponding last_batch_seen entries
                    last_batch_seen
                        .retain(|id, last| *last > stale_threshold || doc_cache.contains_key(id));
                }
                // R162: Persist first — best-effort durability before broadcast.
                // R562: If persist fails, still broadcast to clients — preventing
                // broadcast on disk failure causes split-brain where clients never
                // see deltas that were successfully applied to the in-memory cache.
                // R567: Quarantine skips disk persistence but still broadcasts to
                // clients so they receive real-time updates.
                let mut persist_failed = false;
                let scoped_storage = if let (Some(principal), Some(workspace), Some(layout)) = (
                    route.principal.as_deref(),
                    route.workspace.as_deref(),
                    workspace_layout_for_broadcast.as_ref(),
                ) {
                    Some(MuijStorage::new(
                        layout.scoped_agent_runtime_root(principal, workspace),
                    ))
                } else {
                    storage_opt.clone()
                };
                if let Some(storage) = scoped_storage.as_ref() {
                    // R253/R260/R261: Clear stale quarantine/fail state only for
                    // genuinely fresh agents — those with no entry in ANY tracking map.
                    if !doc_cache.contains_key(&cache_key)
                        && !persist_fail_count.contains_key(&cache_key)
                        && !quarantine_until.contains_key(&cache_key)
                    {
                        // Genuinely fresh agent — no stale state to clear.
                    }
                    // R244: Skip persist if agent is quarantined (backoff active)
                    // R249: Cache-only apply — apply deltas to in-memory doc without
                    // persisting, so non-coalescable TerminalTransient lines aren't lost.
                    // When quarantine lifts, the next persist writes the full cached doc.
                    // R567: Still broadcast to clients after cache-only apply.
                    let mut skip_persist = false;
                    if let Some(&until) = quarantine_until.get(&cache_key) {
                        if Instant::now() < until {
                            if let Some(doc) = doc_cache.get_mut(&cache_key) {
                                // R539: Check doc size before applying more deltas
                                if doc.layout.len() >= 500 {
                                    warn!(
                                        agent_id = %agent_id, components = doc.layout.len(),
                                        "Quarantine cache-only doc exceeds size cap — evicting (R539)"
                                    );
                                    doc_cache.remove(&cache_key);
                                    persist_fail_count.remove(&cache_key);
                                    // Keep quarantine — prevents immediate re-read
                                    continue;
                                }
                                for delta in &deltas {
                                    apply_delta_to_document(doc, delta);
                                }
                                doc.generated_at = Utc::now();
                                // R567: Skip persist but fall through to broadcast
                                persist_failed = true;
                                skip_persist = true;
                            } else {
                                // R364: Doc cache slot was evicted (R236) while quarantine
                                // was retained (R270). Cannot apply cache-only — remove
                                // quarantine and fall through to the normal persist path.
                                warn!(
                                    agent_id = %agent_id,
                                    "Quarantine active but doc_cache absent, lifting quarantine (R364)"
                                );
                                quarantine_until.remove(&cache_key);
                            }
                        } else {
                            // R723: Log quarantine expiry
                            info!(agent_id = %agent_id, "Quarantine expired, resuming persist (R723)");
                            quarantine_until.remove(&cache_key);
                        }
                    }
                    if !skip_persist {
                        if let Err(reason) = persist_deltas_to_storage(
                            storage,
                            &mut doc_cache,
                            &cache_key,
                            &agent_id,
                            &deltas,
                        )
                        .await
                        {
                            // R237/R265: Geometric log schedule — reduces silent gap between
                            // first failure and count 50 while still rate-limiting at high counts.
                            let count = persist_fail_count.entry(cache_key.clone()).or_insert(0);
                            *count += 1;
                            // R395: Include error details in rate-limited warning
                            if matches!(*count, 1 | 5 | 10 | 25) || (*count).is_multiple_of(50) {
                                warn!(
                                    agent_id = %agent_id, fail_count = *count, reason = %reason,
                                    "Persist failed — broadcasting anyway to prevent split-brain (R562)"
                                );
                            }
                            // R244: Exponential backoff — quarantine agent to reduce I/O pressure
                            let backoff_secs = match *count {
                                1..=5 => 0,
                                6..=20 => 2,
                                21..=100 => 10,
                                _ => 30,
                            };
                            if backoff_secs > 0 {
                                // R723: Log quarantine entry with backoff duration
                                info!(
                                    agent_id = %agent_id, backoff_secs = backoff_secs,
                                    fail_count = *count, "Agent entering quarantine (R723)"
                                );
                                quarantine_until.insert(
                                    cache_key.clone(),
                                    Instant::now() + Duration::from_secs(backoff_secs),
                                );
                            }
                            // R236: Evict oversized invalid docs to bound in-memory growth
                            let should_evict = doc_cache
                                .get(&cache_key)
                                .map(|doc| doc.layout.len() >= 500)
                                .unwrap_or(false);
                            if should_evict {
                                let evicted_len = doc_cache
                                    .remove(&cache_key)
                                    .map(|d| d.layout.len())
                                    .unwrap_or(0);
                                // R381/R536: Do NOT reset persist_fail_count on eviction.
                                // Resetting creates an evict-re-read-re-fail oscillation:
                                // evict → count=0 → no backoff → re-read same oversized doc →
                                // fail again → evict → repeat. Keep the counter so backoff
                                // continues to grow.
                                // R270: Keep quarantine_until after eviction — prevents tight
                                // evict-reload loop when oversized doc persists on disk. The backoff
                                // gives the agent time before re-reading the same oversized doc.
                                warn!(
                                    agent_id = %agent_id, components = evicted_len,
                                    "Evicting oversized cached doc (R236)"
                                );
                            }
                            // R241: Cap check on failure path — prevents unbounded growth
                            // when many distinct agents are all failing persistently.
                            // R250: Keep quarantine_until intact — clearing it would lift
                            // all backoffs simultaneously, causing a thundering herd of
                            // 500+ agents retrying I/O at once.
                            // R266: Retain quarantined docs to preserve R249 cache-only deltas.
                            if doc_cache.len() >= 500 {
                                let before = doc_cache.len();
                                doc_cache.retain(|id, _| quarantine_until.contains_key(id));
                                persist_fail_count
                                    .retain(|id, _| quarantine_until.contains_key(id));
                                // R731: Rate-limit cap-clear warnings to avoid log flooding
                                if batch_count.is_multiple_of(100) || batch_count <= 5 {
                                    warn!(
                                before = before, after = doc_cache.len(),
                                "doc_cache cap clear on failure path — retained quarantined (R241/R266)"
                            );
                                }
                                // Fallback: if quarantined docs alone exceed cap, full clear
                                if doc_cache.len() >= 500 {
                                    warn!(
                                        remaining = doc_cache.len(),
                                        "doc_cache full clear — quarantined docs exceed cap (R472)"
                                    );
                                    doc_cache.clear();
                                    persist_fail_count.clear();
                                    // R294: Also clear quarantine to prevent stale quarantine
                                    // entries without corresponding cache (R249 cache-only path
                                    // would find None and drop entire batch).
                                    quarantine_until.clear();
                                }
                            }
                            // R562: Don't skip broadcast — fall through to broadcast below
                            persist_failed = true;
                        }
                    } // close if !skip_persist
                }
                // Reset fail counter and quarantine on successful persist
                if !persist_failed {
                    persist_fail_count.remove(&cache_key);
                    quarantine_until.remove(&cache_key);
                }
                // R160/R241: Cap doc_cache to prevent unbounded growth from many agents.
                // R250: Keep quarantine_until intact on cap clear to avoid thundering herd.
                // R266: Retain quarantined docs to preserve R249 cache-only deltas.
                // R383: Also retain the just-persisted agent to avoid redundant disk reads.
                if !persist_failed && doc_cache.len() >= 500 {
                    let before = doc_cache.len();
                    doc_cache.retain(|id, _| quarantine_until.contains_key(id) || id == &cache_key);
                    persist_fail_count.retain(|id, _| quarantine_until.contains_key(id));
                    // R731: Rate-limit cap-clear warnings to avoid log flooding
                    if batch_count.is_multiple_of(100) || batch_count <= 5 {
                        warn!(
                        before = before, after = doc_cache.len(),
                        "doc_cache cap clear on success path — retained quarantined (R160/R266)"
                    );
                    }
                    if doc_cache.len() >= 500 {
                        warn!(
                            remaining = doc_cache.len(),
                            "doc_cache full clear — quarantined docs exceed cap (R472)"
                        );
                        doc_cache.clear();
                        persist_fail_count.clear();
                        // R294: Also clear quarantine — prevents stale quarantine entries
                        // without cache causing R249 cache-only path to drop batches.
                        quarantine_until.clear();
                    }
                }
                // Resolve execution_id once per batch for scope injection.
                let exec_id_for_batch = route_exec_map_for_broadcast
                    .read()
                    .await
                    .get(&cache_key)
                    .cloned();
                let scope_for_batch = route
                    .principal
                    .as_ref()
                    .zip(route.workspace.as_ref())
                    .map(|(principal, workspace)| (principal.clone(), workspace.clone()))
                    .or_else(|| {
                        workspace_layout_for_broadcast.as_ref().map(|_| {
                            (
                                DEFAULT_SCOPE_PRINCIPAL.to_string(),
                                DEFAULT_SCOPE_WORKSPACE.to_string(),
                            )
                        })
                    });

                let mut broadcast_failures = 0u32;
                for delta in &deltas {
                    match serde_json::to_value(delta) {
                        Ok(mut payload) => {
                            // Inject execution_id so progress channel normalizer
                            // can resolve lineage (principal/workspace).
                            if let (Some(exec_id), Some(obj)) =
                                (&exec_id_for_batch, payload.as_object_mut())
                            {
                                obj.insert(
                                    "execution_id".to_string(),
                                    serde_json::Value::String(exec_id.clone()),
                                );
                            }
                            if let (Some((principal, workspace)), Some(obj)) =
                                (&scope_for_batch, payload.as_object_mut())
                            {
                                obj.insert(
                                    "principal".to_string(),
                                    serde_json::Value::String(principal.clone()),
                                );
                                obj.insert(
                                    "workspace".to_string(),
                                    serde_json::Value::String(workspace.clone()),
                                );
                            }
                            let envelope =
                                if let Some((principal, workspace)) = scope_for_batch.as_ref() {
                                    AgentEventEnvelope::new_scoped(
                                        "agent.ui.delta",
                                        &agent_id,
                                        principal,
                                        workspace,
                                        payload,
                                    )
                                } else {
                                    AgentEventEnvelope::new("agent.ui.delta", &agent_id, payload)
                                };
                            bcast.emit_agent_transport_event(envelope);
                        },
                        Err(e) => {
                            // R744/R495: Serialization failure after persist creates
                            // state divergence — persisted state is ahead of clients.
                            // Log at error level. The shared cache sync below ensures
                            // the next snapshot request serves the correct state, so
                            // the divergence self-heals on reconnect/snapshot.
                            broadcast_failures += 1;
                            error!(
                                agent_id = %agent_id, error = %e,
                                "Delta serialization failed after persist — client state diverged (R744)"
                            );
                        },
                    }
                }
                if broadcast_failures > 0 {
                    warn!(
                        agent_id = %agent_id, failed = broadcast_failures, total = deltas.len(),
                        "Broadcast had serialization failures — clients should snapshot to reconcile (R744)"
                    );
                }
                // R560/R679: Sync current agent's doc to shared cache for snapshot coherence.
                // Only write if we have a cached doc — avoids holding the write lock when
                // there's nothing to share.
                if let Some(doc) = doc_cache.get(&cache_key) {
                    let shared_cache_key = scope_for_batch
                        .as_ref()
                        .map(|(principal, workspace)| {
                            agent_snapshot_cache_key(
                                &agent_id,
                                Some(principal.as_str()),
                                Some(workspace.as_str()),
                            )
                        })
                        .unwrap_or_else(|| cache_key.clone());
                    let mut shared = cache_for_broadcast.write().await;
                    shared.insert(shared_cache_key.clone(), doc.clone());
                    // Mirror cap behavior: if shared cache exceeds limit, trim it
                    if shared.len() >= 500 {
                        let keys: Vec<String> = shared
                            .keys()
                            .filter(|k| *k != &shared_cache_key)
                            .take(shared.len() - 400)
                            .cloned()
                            .collect();
                        for k in keys {
                            shared.remove(&k);
                        }
                    }
                }
            }
            debug!("[GAUI-EMITTER] Broadcast task exiting normally");
        });

        // Task 1: Event loop — translates agentic events into EmittedDelta,
        // feeds coalescer directly (no serialization, R51).
        // R125: Retains JoinHandles and monitors them via select! — if either
        // downstream task panics, the event loop detects it and shuts down
        // instead of silently losing deltas.
        let handle = tokio::spawn(async move {
            let mut emitter = MuijDeltaEmitter {
                execution_map: HashMap::new(),
                active_cycle_map: HashMap::new(),
                agent_seq_map: HashMap::new(),
                agent_selectors_map: HashMap::new(),
                event_count: 0,
                last_sweep_at: Instant::now(),
                cumulative_lagged: 0,
            };
            let mut rx = broadcaster.subscribe();

            let mut coalescer_handle = coalescer_handle;
            let mut broadcast_handle = broadcast_handle;

            let mut cumulative_dropped: u64 = 0;
            loop {
                tokio::select! {
                    result = rx.recv() => {
                        match result {
                            Ok(event) => {
                                match &event {
                                    RuntimeTransportEvent::AgentEvent { event }
                                        if event.event_type == "agent.execution.mapping" =>
                                    {
                                        let cache_key = agent_snapshot_cache_key(
                                            &event.agent_id,
                                            event.principal.as_deref(),
                                            event.workspace.as_deref(),
                                        );
                                        if let Some(execution_id) = event
                                            .payload
                                            .get("execution_id")
                                            .and_then(|value| value.as_str())
                                        {
                                            route_exec_map
                                                .write()
                                                .await
                                                .insert(cache_key.clone(), execution_id.to_string());
                                        }
                                        route_meta_map
                                            .write()
                                            .await
                                            .insert(
                                                cache_key,
                                                route_metadata(
                                                    &event.agent_id,
                                                    event.principal.as_deref(),
                                                    event.workspace.as_deref(),
                                                ),
                                            );
                                    },
                                    RuntimeTransportEvent::AgentCycleStarted {
                                        principal: Some(principal),
                                        workspace: Some(workspace),
                                        agent_id,
                                        ..
                                    }
                                    | RuntimeTransportEvent::AgentCycleCompleted {
                                        principal: Some(principal),
                                        workspace: Some(workspace),
                                        agent_id,
                                        ..
                                    }
                                    | RuntimeTransportEvent::AgentTriggered {
                                        principal: Some(principal),
                                        workspace: Some(workspace),
                                        agent_id,
                                        ..
                                    } => {
                                        route_meta_map
                                            .write()
                                            .await
                                            .insert(
                                                agent_snapshot_cache_key(
                                                    agent_id,
                                                    Some(principal.as_str()),
                                                    Some(workspace.as_str()),
                                                ),
                                                route_metadata(
                                                    agent_id,
                                                    Some(principal.as_str()),
                                                    Some(workspace.as_str()),
                                                ),
                                            );
                                    },
                                    _ => {},
                                }
                                for emitted in emitter.handle_event(&event) {
                                    route_meta_map
                                        .write()
                                        .await
                                        .insert(
                                            emitted.cache_key.clone(),
                                            route_metadata(
                                                &emitted.agent_id,
                                                emitted.principal.as_deref(),
                                                emitted.workspace.as_deref(),
                                            ),
                                        );
                                    let item = (
                                        emitted.cache_key.clone(),
                                        emitted.delta,
                                        emitted.coalescable,
                                    );
                                    if emitted.coalescable {
                                        // R454/R502: Coalescable deltas use try_send —
                                        // dropping is acceptable (last-write-wins semantics).
                                        match coal_tx.try_send(item) {
                                            Ok(()) => {},
                                            Err(mpsc::error::TrySendError::Full(_)) => {
                                                cumulative_dropped += 1;
                                                if cumulative_dropped.is_power_of_two() {
                                                    warn!(
                                                        agent_id = %emitted.agent_id,
                                                        cumulative_dropped = cumulative_dropped,
                                                        "Coalescer channel full — coalescable delta dropped (R454)"
                                                    );
                                                }
                                            },
                                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                                info!("Coalescer channel closed, shutting down");
                                                return;
                                            },
                                        }
                                    } else {
                                        // R561: Non-coalescable deltas (TerminalTransient)
                                        // use send().await with a short timeout — every
                                        // line matters (R49), so we wait briefly for
                                        // backpressure to clear rather than dropping.
                                        match tokio::time::timeout(
                                            Duration::from_millis(100),
                                            coal_tx.send(item),
                                        ).await {
                                            Ok(Ok(())) => {},
                                            Ok(Err(_)) => {
                                                info!("Coalescer channel closed, shutting down");
                                                return;
                                            },
                                            Err(_) => {
                                                cumulative_dropped += 1;
                                                if cumulative_dropped.is_power_of_two() {
                                                    warn!(
                                                        agent_id = %emitted.agent_id,
                                                        cumulative_dropped = cumulative_dropped,
                                                        "Coalescer channel full after 100ms — non-coalescable delta dropped (R561)"
                                                    );
                                                }
                                            },
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                // R97: Track cumulative lag.
                                emitter.cumulative_lagged += n;
                                warn!(
                                    skipped = n, cumulative_lagged = emitter.cumulative_lagged,
                                    "Broadcast receiver lagged — registration events lost in this window"
                                );
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                info!("Broadcast channel closed, shutting down");
                                break;
                            }
                        }
                    }
                    result = &mut coalescer_handle => {
                        match result {
                            Ok(()) => warn!("Coalescer task exited unexpectedly"),
                            Err(e) => error!(error = %e, "Coalescer task panicked"),
                        }
                        break;
                    }
                    result = &mut broadcast_handle => {
                        match result {
                            Ok(()) => warn!("Broadcast task exited unexpectedly"),
                            Err(e) => error!(error = %e, "Broadcast task panicked"),
                        }
                        break;
                    }
                }
            }
            // R460/R380/R505: Drop the coalescer input channel before aborting.
            // This signals the coalescer to flush pending upserts and cascade
            // shutdown to the broadcast task, instead of aborting mid-flush.
            drop(coal_tx);
            // Give the coalescer a brief window to flush (coalescing window + margin)
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !coalescer_handle.is_finished() {
                coalescer_handle.abort();
            }
            if !broadcast_handle.is_finished() {
                broadcast_handle.abort();
            }
        });

        (handle, shared_cache)
    }
}

fn upsert_component<'a>(
    layout: &'a mut [MuijComponent],
    component_id: &str,
) -> Option<&'a mut MuijComponent> {
    // R595: Trim whitespace to match Reorder path and frontend normalization
    let trimmed = component_id.trim();
    layout.iter_mut().find(|c| c.id == trimmed)
}

fn shallow_merge_props(existing: &Value, incoming: Option<&Value>) -> Value {
    // R703: Explicit guard for null/non-object existing props. Structurally
    // impossible via serde (props defaults to empty object), but defends
    // against corrupted stored documents.
    let mut merged = match existing.as_object() {
        Some(obj) => obj.clone(),
        None => {
            tracing::warn!(
                "shallow_merge_props: existing props is not an object — using empty (R703)"
            );
            serde_json::Map::new()
        },
    };
    if let Some(Value::Object(delta_obj)) = incoming {
        for (k, v) in delta_obj {
            merged.insert(k.clone(), v.clone());
        }
    }
    Value::Object(merged)
}

fn apply_delta_to_document(doc: &mut MuijDocument, delta: &MuijDelta) {
    match delta {
        MuijDelta::Upsert { component_id, data } => {
            // R396: Guard against non-object `data` — silently skip
            let Some(data_obj) = data.as_object() else {
                warn!(
                    component_id = %component_id,
                    "Upsert delta has non-object data — skipping (R396)"
                );
                return;
            };
            if let Some(existing) = upsert_component(&mut doc.layout, component_id) {
                // R458: component_type is immutable after creation — ignore attempts to change it
                if let Some(incoming_type) = data_obj.get("component_type").and_then(|v| v.as_str())
                {
                    let trimmed = incoming_type.trim();
                    if !trimmed.is_empty() && trimmed != existing.component_type {
                        debug!(
                            component_id = %component_id,
                            existing_type = %existing.component_type,
                            incoming_type = %trimmed,
                            "Ignoring component_type change on existing component (R458)"
                        );
                    }
                }
                // R592: Coerce non-string labels to string (matching frontend normalization).
                // as_str() returns None for numbers/bools, causing label to stay unchanged
                // while frontend would show the stringified value.
                if let Some(label_val) = data_obj.get("label") {
                    existing.label = match label_val.as_str() {
                        Some(s) => s.to_string(),
                        None if !label_val.is_null() => label_val.to_string(),
                        _ => existing.label.clone(),
                    };
                }
                // R590: Coerce non-string source/query to string (matching frontend
                // normalization and R592 label coercion pattern).
                if let Some(source) = data_obj.get("source") {
                    existing.source = match source {
                        v if v.is_null() => None,
                        v if v.is_string() => v.as_str().map(|s| s.to_string()),
                        v => Some(v.to_string()),
                    };
                }
                if let Some(query) = data_obj.get("query") {
                    existing.query = match query {
                        v if v.is_null() => None,
                        v if v.is_string() => v.as_str().map(|s| s.to_string()),
                        v => Some(v.to_string()),
                    };
                }
                if let Some(snapshot) = data_obj.get("static_snapshot") {
                    existing.static_snapshot = if snapshot.is_null() {
                        None
                    } else {
                        Some(snapshot.clone())
                    };
                }
                if let Some(children) = data_obj.get("children") {
                    match serde_json::from_value::<Vec<MuijComponent>>(children.clone()) {
                        Ok(mut parsed) => {
                            // R534: Deduplicate children IDs (first-wins, matching frontend)
                            dedup_children_by_id(&mut parsed);
                            existing.children = parsed;
                        },
                        Err(e) => {
                            // R397/R558: Log and normalize to empty — matches frontend behavior
                            // where malformed children result in an empty array, not retention
                            // of stale prior children.
                            warn!(
                                component_id = %component_id, error = %e,
                                "Children deserialization failed on update — normalizing to empty (R397/R558)"
                            );
                            existing.children = vec![];
                        },
                    }
                }
                existing.props = shallow_merge_props(&existing.props, data_obj.get("props"));
            } else {
                let props = data_obj
                    .get("props")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or_else(|| Value::Object(Default::default()));
                let mut children = data_obj
                    .get("children")
                    .and_then(|v| serde_json::from_value::<Vec<MuijComponent>>(v.clone()).ok())
                    .unwrap_or_default();
                // R534: Deduplicate children IDs (first-wins, matching frontend)
                dedup_children_by_id(&mut children);
                let static_snapshot = data_obj.get("static_snapshot").and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        Some(v.clone())
                    }
                });
                // R457: Trim component_type to match frontend normalization
                let raw_type = data_obj
                    .get("component_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                let component_type = raw_type.trim();
                let component_type = if component_type.is_empty() {
                    "Unknown"
                } else {
                    component_type
                };
                let component = MuijComponent {
                    // R595: Trim whitespace to match Reorder path and frontend normalization
                    id: component_id.trim().to_string(),
                    component_type: component_type.to_string(),
                    // R592: Coerce non-string labels to string (matching frontend)
                    label: match data_obj.get("label") {
                        Some(v) if v.is_string() => v.as_str().unwrap_or_default().to_string(),
                        Some(v) if !v.is_null() => v.to_string(),
                        _ => String::new(),
                    },
                    // R590: Coerce non-string source/query to string (matching frontend)
                    source: match data_obj.get("source") {
                        Some(v) if v.is_string() => v.as_str().map(|s| s.to_string()),
                        Some(v) if !v.is_null() => Some(v.to_string()),
                        _ => None,
                    },
                    query: match data_obj.get("query") {
                        Some(v) if v.is_string() => v.as_str().map(|s| s.to_string()),
                        Some(v) if !v.is_null() => Some(v.to_string()),
                        _ => None,
                    },
                    props,
                    static_snapshot,
                    children,
                };
                // R569/R645: Cap layout size to prevent unbounded growth
                if doc.layout.len() >= MAX_LAYOUT_COMPONENTS {
                    warn!(
                        component_id = %component_id, cap = MAX_LAYOUT_COMPONENTS,
                        "Layout at capacity — new component rejected (R569)"
                    );
                } else {
                    doc.layout.push(component);
                }
            }
        },
        MuijDelta::Remove { component_id } => {
            // R595: Trim whitespace to match Reorder path and frontend normalization
            let trimmed = component_id.trim();
            doc.layout.retain(|c| c.id != trimmed);
        },
        MuijDelta::Reorder { ids } => {
            let mut remaining = std::mem::take(&mut doc.layout);
            let mut reordered = Vec::with_capacity(remaining.len());
            for id in ids {
                // R548: Trim whitespace from IDs to match frontend normalization
                let trimmed_id = id.trim();
                if let Some(pos) = remaining.iter().position(|c| c.id == trimmed_id) {
                    reordered.push(remaining.remove(pos));
                }
            }
            reordered.extend(remaining);
            doc.layout = reordered;
        },
    }
}

/// R534: Deduplicate children by ID, keeping first occurrence (matches frontend behavior).
fn dedup_children_by_id(children: &mut Vec<MuijComponent>) {
    let mut seen = std::collections::HashSet::new();
    children.retain(|c| seen.insert(c.id.clone()));
}

fn dedup_component_tree_ids(
    components: &mut Vec<MuijComponent>,
    seen: &mut std::collections::HashSet<String>,
) {
    let mut index = 0;
    while index < components.len() {
        let keep = {
            let component = &mut components[index];
            if !seen.insert(component.id.clone()) {
                false
            } else {
                dedup_component_tree_ids(&mut component.children, seen);
                true
            }
        };
        if keep {
            index += 1;
        } else {
            components.remove(index);
        }
    }
}

/// R450: Truncate a string to at most `max_chars` characters.
/// Returns a substring at a char boundary — does not append an ellipsis
/// (callers concatenate the truncated slice into structured JSON fields).
fn truncate_str(s: &str, max_chars: usize) -> &str {
    // R704: Early return checks char count (not byte length) for consistency
    // with the fallback path. For ASCII strings these are identical, but
    // multi-byte UTF-8 strings could bypass truncation if we checked bytes.
    if s.chars().count() <= max_chars {
        return s;
    }
    // Find the byte index of the char boundary at or before max_chars
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s, // string has fewer chars than max_chars (len was bytes, not chars)
    }
}

/// Persist deltas to storage. Returns `Ok(())` if persistence succeeded (or was
/// skipped because deltas were empty), `Err(reason)` if an error prevented
/// durable writes. Callers should skip broadcast on `Err` to honour R162's
/// persist-before-broadcast guarantee (R189). The error string includes
/// actionable diagnostics (R395).
async fn persist_deltas_to_storage(
    storage: &MuijStorage,
    cache: &mut HashMap<String, MuijDocument>,
    cache_key: &str,
    agent_id: &str,
    deltas: &[MuijDelta],
) -> Result<(), String> {
    if deltas.is_empty() {
        return Ok(());
    }
    if !cache.contains_key(cache_key) {
        match storage.read_layout(agent_id).await {
            Ok(Some(doc)) => {
                cache.insert(cache_key.to_string(), doc);
            },
            Ok(None) => {
                cache.insert(
                    cache_key.to_string(),
                    MuijDocument::new(agent_id.to_string()),
                );
            },
            Err(e) => {
                // R205: Transient read error — return Err instead of creating
                // a fresh empty doc that would overwrite the rich on-disk document
                // with only the current batch's deltas after a momentary I/O error.
                return Err(format!("read_layout failed: {e}"));
            },
        }
    }
    let Some(doc) = cache.get(cache_key) else {
        return Err("cache slot missing after insert".into());
    };
    // R382: Apply deltas to a clone and validate before committing.
    // If validation fails, the original (valid) doc stays in cache — prevents
    // permanently invalid cache state while avoiding the evict-reread loop (R206).
    let mut candidate = doc.clone();
    for delta in deltas {
        apply_delta_to_document(&mut candidate, delta);
    }
    candidate.generated_at = Utc::now();

    // Normalize duplicate ids across the full document tree before validation.
    // Some emitters already deduplicate immediate child arrays, but nested
    // descendants can still collide and should not poison persistence.
    let mut seen_ids = std::collections::HashSet::new();
    dedup_component_tree_ids(&mut candidate.layout, &mut seen_ids);

    // R195: Validate the evolved document before writing — prevents persisting
    // structurally invalid state that would later fail R157/R159 validation.
    let registry = DefaultComponentRegistry;
    if let Err(e) = candidate.validate(&registry) {
        return Err(format!("validation failed: {e}"));
    }

    if let Err(e) = storage.write_layout(agent_id, &candidate).await {
        return Err(format!("write_layout failed: {e}"));
    }
    // Commit the validated candidate to the cache
    cache.insert(cache_key.to_string(), candidate);
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn unscoped_cache_key(agent_id: &str) -> String {
        agent_snapshot_cache_key(agent_id, None, None)
    }

    /// Helper: create a bare emitter for testing.
    fn test_emitter() -> MuijDeltaEmitter {
        MuijDeltaEmitter {
            execution_map: HashMap::new(),
            active_cycle_map: HashMap::new(),
            agent_seq_map: HashMap::new(),
            agent_selectors_map: HashMap::new(),
            event_count: 0,
            last_sweep_at: Instant::now(),
            cumulative_lagged: 0,
        }
    }

    fn ts() -> i64 {
        1_000_000
    }

    async fn wait_for_layout(storage: &MuijStorage, agent_id: &str, label: &str) -> MuijDocument {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match storage.read_layout(agent_id).await {
                Ok(Some(doc)) => return doc,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                },
                Ok(None) => panic!("{label} scoped layout should exist"),
                Err(err) => panic!("failed to read {label} scoped layout: {err}"),
            }
        }
    }

    /// Helper: extract the MuijDelta from an EmittedDelta (moves it).
    fn take_delta(emitted: EmittedDelta) -> MuijDelta {
        emitted.delta
    }

    // 1. execution_started_with_agent_id_emits_gauge_delta
    #[test]
    fn execution_started_with_agent_id_emits_gauge_delta() {
        let mut emitter = test_emitter();
        let event = RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "test goal".into(),
            success_criteria: "done".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-a".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter
            .handle_event(&event)
            .into_iter()
            .next()
            .expect("should emit delta");
        assert_eq!(emitted.agent_id, "agent-a");
        assert!(emitted.coalescable, "Gauge should be coalescable");

        match take_delta(emitted) {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-a-gauge");
                assert_eq!(data["component_type"], "Gauge");
                assert_eq!(data["props"]["fill"], 0.0);
                assert_eq!(data["props"]["max_iterations"], 5);
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 2. execution_started_without_agent_id_produces_no_delta
    #[test]
    fn execution_started_without_agent_id_produces_no_delta() {
        let mut emitter = test_emitter();
        let event = RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "test".into(),
            success_criteria: "done".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        assert!(emitter.handle_event(&event).is_empty());
    }

    // 3. iteration_started_emits_gauge_and_terminal
    #[test]
    fn iteration_started_emits_gauge_upsert_with_fill() {
        let mut emitter = test_emitter();

        // Register agent first
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 10,
            hint_action: None,
            agent_id: Some("agent-b".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 3,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event);
        assert_eq!(emitted.len(), 2, "should emit Gauge + TerminalTransient");

        // First: Gauge (coalescable)
        let gauge = &emitted[0];
        assert!(gauge.coalescable, "Gauge should be coalescable");
        match &gauge.delta {
            MuijDelta::Upsert { data, .. } => {
                let fill = data["props"]["fill"].as_f64().unwrap();
                assert!(
                    (fill - 0.3).abs() < 1e-9,
                    "fill should be 3/10 = 0.3, got {}",
                    fill
                );
                assert_eq!(data["props"]["iteration"], 3);
                assert_eq!(data["props"]["max_iterations"], 10);
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        // Second: TerminalTransient (NOT coalescable, R49)
        let term = &emitted[1];
        assert!(
            !term.coalescable,
            "TerminalTransient should NOT be coalescable (R49)"
        );
        match &term.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-b-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                assert_eq!(data["label"], "Agent Log");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(
                    line.contains("[iter 3]"),
                    "boundary line should contain iter number"
                );
                assert!(data["props"]["seq"].is_number(), "should have seq prop");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 3b. iteration_started_emits_terminal_boundary_line (R16)
    #[test]
    fn iteration_started_emits_terminal_boundary_line() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-boundary".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let emitted = emitter.handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 2,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        assert_eq!(emitted.len(), 2, "should emit Gauge + TerminalTransient");

        // Second is the boundary line
        let term = &emitted[1];
        match &term.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-boundary-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                assert_eq!(data["label"], "Agent Log");
                assert_eq!(data["props"]["line"], "[iter 2] started");
                assert_eq!(
                    data["props"]["seq"], 0,
                    "first terminal emission should be seq 0"
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        // Second iteration should increment seq
        let emitted2 = emitter.handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 3,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        match &emitted2[1].delta {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(
                    data["props"]["seq"], 1,
                    "second terminal emission should be seq 1"
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 4. iteration_started_unknown_thread_returns_none
    #[test]
    fn iteration_started_unknown_thread_returns_none() {
        let mut emitter = test_emitter();
        let event = RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "unknown".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };
        assert!(emitter.handle_event(&event).is_empty());
    }

    // 5. decision_made_emits_terminal_transient_upsert
    #[test]
    fn decision_made_emits_terminal_transient_upsert() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-c".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticDecisionMade {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 1,
            decision_type: "execute_action".into(),
            action_summary: Some("click submit".into()),
            reasoning: "fallback reasoning".into(),
            confidence: 0.9,
            thinking: None,
            evidence: None,
            tool_name: None,
            action_type: None,
            element_id: None,
            candidates_count: None,
            raw_decision: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        assert_eq!(emitted.agent_id, "agent-c");
        assert!(
            !emitted.coalescable,
            "TerminalTransient should NOT be coalescable"
        );

        match take_delta(emitted) {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-c-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                assert_eq!(data["label"], "Agent Log");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("execute_action"));
                assert!(line.contains("click submit"));
                assert!(data["props"]["seq"].is_number(), "should have seq prop");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 5b. decision_made_without_action_summary_falls_back_to_reasoning
    #[test]
    fn decision_made_without_action_summary_falls_back_to_reasoning() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-c2".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticDecisionMade {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 2,
            decision_type: "goal_achieved".into(),
            action_summary: None,
            reasoning: "target element found on page".into(),
            confidence: 0.95,
            thinking: None,
            evidence: None,
            tool_name: None,
            action_type: None,
            element_id: None,
            candidates_count: None,
            raw_decision: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        match take_delta(emitted) {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(data["label"], "Agent Log");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("goal_achieved"));
                assert!(line.contains("target element found on page"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 6. action_executed_emits_terminal_transient_upsert
    #[test]
    fn action_executed_emits_terminal_transient_upsert() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-d".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticActionExecuted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 1,
            action_type: "click".into(),
            target: "#submit-btn".into(),
            success: true,
            latency_ms: 42,
            error: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        assert!(
            !emitted.coalescable,
            "TerminalTransient should NOT be coalescable"
        );

        match take_delta(emitted) {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-d-terminal");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[click ok]"));
                assert!(line.contains("#submit-btn"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 7. action_executed_with_error_includes_error_in_line
    #[test]
    fn action_executed_with_error_includes_error_in_line() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-e".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticActionExecuted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 1,
            action_type: "navigate".into(),
            target: "https://example.com".into(),
            success: false,
            latency_ms: 500,
            error: Some("timeout".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        match take_delta(emitted) {
            MuijDelta::Upsert { data, .. } => {
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[navigate err]"));
                assert!(line.contains("timeout"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 8. cycle_completed_cleans_up_execution_map
    #[test]
    fn cycle_completed_cleans_up_execution_map() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-f".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t2".into(),
            plan_id: "p1".into(),
            step_id: "s2".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-f".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert_eq!(emitter.execution_map.len(), 2);

        // Bind both threads to cycle c1 (normally done via mapping events from executor).
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-f",
                serde_json::json!({
                    "execution_id": "t1",
                    "agent_id": "agent-f",
                    "cycle_id": "c1",
                    "max_iterations": 5
                }),
            ),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-f",
                serde_json::json!({
                    "execution_id": "t2",
                    "agent_id": "agent-f",
                    "cycle_id": "c1",
                    "max_iterations": 5
                }),
            ),
        });

        emitter.handle_event(&RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-f".into(),
            goal_id: "g1".into(),
            cycle_id: "c1".into(),
            execution_id: None,
            outcome: "success".into(),
            iterations_used: 3,
            timestamp: ts(),
        });

        assert!(
            emitter.execution_map.is_empty(),
            "all threads for agent-f should be cleaned up"
        );
    }

    #[test]
    fn cycle_completed_only_cleans_matching_cycle() {
        let mut emitter = test_emitter();
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-z",
                serde_json::json!({
                    "execution_id": "t-cycle-1",
                    "agent_id": "agent-z",
                    "cycle_id": "c1",
                    "max_iterations": 5
                }),
            ),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-z",
                serde_json::json!({
                    "execution_id": "t-cycle-2",
                    "agent_id": "agent-z",
                    "cycle_id": "c2",
                    "max_iterations": 5
                }),
            ),
        });

        emitter.handle_event(&RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-z".into(),
            goal_id: "g1".into(),
            cycle_id: "c1".into(),
            execution_id: None,
            outcome: "success".into(),
            iterations_used: 3,
            timestamp: ts(),
        });

        assert!(!emitter.execution_map.contains_key("t-cycle-1"));
        assert!(emitter.execution_map.contains_key("t-cycle-2"));
    }

    #[test]
    fn execution_mapping_agent_event_recovers_routing_without_start_registration() {
        let mut emitter = test_emitter();
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-map",
                serde_json::json!({
                    "execution_id": "t-map",
                    "agent_id": "agent-map",
                    "cycle_id": "cycle-map",
                    "max_iterations": 10
                }),
            ),
        });

        let emitted = emitter.handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t-map".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 1,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert_eq!(emitted.len(), 2, "mapping event should recover routing");
        assert_eq!(emitted[0].agent_id, "agent-map");
    }

    // 9. emitted_delta_has_correct_structure
    #[test]
    fn emitted_delta_has_correct_structure() {
        let mut emitter = test_emitter();

        let event = RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-g".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        assert_eq!(emitted.agent_id, "agent-g");
        assert!(emitted.coalescable);

        match emitted.delta {
            MuijDelta::Upsert { component_id, .. } => {
                assert_eq!(component_id, "agent-g-gauge");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // 10. multiple_agents_tracked_independently
    #[test]
    fn multiple_agents_tracked_independently() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-alpha".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 10,
            hint_action: None,
            agent_id: Some("alpha".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-beta".into(),
            plan_id: "p2".into(),
            step_id: "s2".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 20,
            hint_action: None,
            agent_id: Some("beta".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Iteration on alpha's execution (first emitted is Gauge)
        let alpha_emitted = emitter.handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t-alpha".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 5,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert_eq!(alpha_emitted[0].agent_id, "alpha");
        match &alpha_emitted[0].delta {
            MuijDelta::Upsert { data, .. } => {
                let fill = data["props"]["fill"].as_f64().unwrap();
                assert!((fill - 0.5).abs() < 1e-9, "alpha: 5/10 = 0.5");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        // Iteration on beta's execution
        let beta_emitted = emitter.handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "t-beta".into(),
            plan_id: "p2".into(),
            step_id: "s2".into(),
            iteration: 10,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert_eq!(beta_emitted[0].agent_id, "beta");
        match &beta_emitted[0].delta {
            MuijDelta::Upsert { data, .. } => {
                let fill = data["props"]["fill"].as_f64().unwrap();
                assert!((fill - 0.5).abs() < 1e-9, "beta: 10/20 = 0.5");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "alpha",
                serde_json::json!({
                    "execution_id": "t-alpha",
                    "agent_id": "alpha",
                    "cycle_id": "c",
                    "max_iterations": 10
                }),
            ),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "beta",
                serde_json::json!({
                    "execution_id": "t-beta",
                    "agent_id": "beta",
                    "cycle_id": "c-beta",
                    "max_iterations": 20
                }),
            ),
        });

        // Complete alpha — only alpha's executions removed
        emitter.handle_event(&RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "alpha".into(),
            goal_id: "g".into(),
            cycle_id: "c".into(),
            execution_id: None,
            outcome: "success".into(),
            iterations_used: 5,
            timestamp: ts(),
        });

        assert!(
            emitter
                .handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
                    execution_id: "t-alpha".into(),
                    plan_id: "p1".into(),
                    step_id: "s1".into(),
                    iteration: 6,
                    environment_type: "browser".into(),
                    principal: None,
                    workspace: None,
                    timestamp: ts(),
                })
                .is_empty(),
            "alpha execution should be cleaned up"
        );

        assert!(
            !emitter
                .handle_event(&RuntimeTransportEvent::AgenticIterationStarted {
                    execution_id: "t-beta".into(),
                    plan_id: "p2".into(),
                    step_id: "s2".into(),
                    iteration: 11,
                    environment_type: "browser".into(),
                    principal: None,
                    workspace: None,
                    timestamp: ts(),
                })
                .is_empty(),
            "beta execution should still be active"
        );
    }

    // 11. spawn_subscribes_translates_and_rebroadcasts (via coalescer pipeline)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_subscribes_translates_and_rebroadcasts() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));

        // Use a short coalescing window (10ms) so the test doesn't wait long
        let (handle, _cache) = MuijDeltaEmitter::spawn_with_coalescing(
            Arc::clone(&broadcaster),
            std::time::Duration::from_millis(10),
        );

        // Give the spawned tasks time to start and subscribe
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Subscribe AFTER emitter is running — this receiver sees events
        // broadcast from this point forward (including emitter's re-broadcasts).
        let mut rx = broadcaster.subscribe();

        // Emit a runtime fact that the emitter should translate.
        // No canonical scope is registered in this test, so `emit(...)`
        // still behaves as transport delivery only.
        broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-int".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("integ-agent".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Wait for emitter to process event, coalescer to flush, and broadcast
        // (10ms window + margin for task scheduling)
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Drain events — we expect the original + the emitter's AgentEvent delta
        let mut found_delta = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(RuntimeTransportEvent::AgentEvent { ref event }))
                    if event.event_type == "agent.ui.delta" =>
                {
                    assert_eq!(event.agent_id, "integ-agent");
                    // Verify the payload is a valid MuijDelta
                    let delta: MuijDelta = serde_json::from_value(event.payload.clone())
                        .expect("payload should be MuijDelta");
                    assert!(matches!(delta, MuijDelta::Upsert { .. }));
                    found_delta = true;
                    break;
                },
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(
            found_delta,
            "should have received an agent.ui.delta AgentEvent via coalescer pipeline"
        );

        // Clean up: abort the event loop task. Coalescer and broadcast tasks
        // self-terminate via channel closure cascade.
        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_cycle_context_is_rebroadcast_on_agent_ui_delta_events() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let (handle, _cache) = MuijDeltaEmitter::spawn_with_coalescing(
            Arc::clone(&broadcaster),
            std::time::Duration::from_millis(10),
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut rx = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::AgentCycleStarted {
            principal: Some("alpha".into()),
            workspace: Some("prod".into()),
            agent_id: "integ-agent".into(),
            goal_id: "harness:integ-agent:daily".into(),
            cycle_id: "cycle-1".into(),
            execution_id: Some("t-int".into()),
            goal: "Run daily".into(),
            timestamp: ts(),
        });
        broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-int".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("integ-agent".into()),
            principal: Some("alpha".into()),
            workspace: Some("prod".into()),
            timestamp: ts(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let mut found_scoped_delta = false;
        for _ in 0..12 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(RuntimeTransportEvent::AgentEvent { ref event }))
                    if event.event_type == "agent.ui.delta" =>
                {
                    if event.agent_id != "integ-agent" {
                        continue;
                    }
                    assert_eq!(event.principal.as_deref(), Some("alpha"));
                    assert_eq!(event.workspace.as_deref(), Some("prod"));
                    let payload = event
                        .payload
                        .as_object()
                        .expect("agent.ui.delta payload should be an object");
                    assert_eq!(payload.get("principal"), Some(&serde_json::json!("alpha")));
                    assert_eq!(payload.get("workspace"), Some(&serde_json::json!("prod")));
                    found_scoped_delta = true;
                    break;
                },
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => continue,
                Err(_) => break,
            }
        }

        assert!(
            found_scoped_delta,
            "should have received an agent.ui.delta event with scoped metadata"
        );

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_agent_id_batches_persist_to_distinct_scoped_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        workspace.ensure_root_sync().unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let (handle, _cache) =
            MuijDeltaEmitter::spawn_scoped(Arc::clone(&broadcaster), workspace.clone());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        for (principal, workspace_name, execution_id, target) in [
            ("alpha", "one", "exec-alpha", "alpha-target"),
            ("beta", "two", "exec-beta", "beta-target"),
        ] {
            broadcaster.emit(RuntimeTransportEvent::AgentEvent {
                event: AgentEventEnvelope::new_scoped(
                    "agent.execution.mapping",
                    "shared-agent",
                    principal,
                    workspace_name,
                    serde_json::json!({
                        "execution_id": execution_id,
                        "agent_id": "shared-agent",
                        "cycle_id": format!("cycle-{principal}"),
                        "max_iterations": 5
                    }),
                ),
            });
            broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
                execution_id: execution_id.into(),
                plan_id: "p".into(),
                step_id: "s".into(),
                goal: "g".into(),
                success_criteria: "c".into(),
                max_iterations: 5,
                hint_action: None,
                agent_id: Some("shared-agent".into()),
                principal: Some(principal.into()),
                workspace: Some(workspace_name.into()),
                timestamp: ts(),
            });
            broadcaster.emit(RuntimeTransportEvent::AgenticActionExecuted {
                execution_id: execution_id.into(),
                plan_id: "p".into(),
                step_id: "s".into(),
                iteration: 1,
                action_type: "click".into(),
                target: target.into(),
                success: true,
                latency_ms: 1,
                error: None,
                principal: Some(principal.into()),
                workspace: Some(workspace_name.into()),
                timestamp: ts(),
            });
        }

        let alpha_storage = MuijStorage::new(workspace.scoped_agent_runtime_root("alpha", "one"));
        let beta_storage = MuijStorage::new(workspace.scoped_agent_runtime_root("beta", "two"));
        let alpha_doc = wait_for_layout(&alpha_storage, "shared-agent", "alpha").await;
        let beta_doc = wait_for_layout(&beta_storage, "shared-agent", "beta").await;

        let alpha_line = alpha_doc
            .layout
            .iter()
            .find(|component| component.id == "shared-agent-terminal")
            .and_then(|component| component.props.get("line"))
            .and_then(|value| value.as_str())
            .expect("alpha terminal line should exist");
        let beta_line = beta_doc
            .layout
            .iter()
            .find(|component| component.id == "shared-agent-terminal")
            .and_then(|component| component.props.get("line"))
            .and_then(|value| value.as_str())
            .expect("beta terminal line should exist");

        assert!(alpha_line.contains("alpha-target"));
        assert!(beta_line.contains("beta-target"));
        assert_ne!(alpha_line, beta_line);

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unscoped_batches_persist_to_default_scope_under_spawn_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        workspace.ensure_root_sync().unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let (handle, _cache) =
            MuijDeltaEmitter::spawn_scoped(Arc::clone(&broadcaster), workspace.clone());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "exec-default".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("default-agent".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        broadcaster.emit(RuntimeTransportEvent::AgenticActionExecuted {
            execution_id: "exec-default".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            action_type: "click".into(),
            target: "default-target".into(),
            success: true,
            latency_ms: 1,
            error: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let default_storage = MuijStorage::new(
            workspace.scoped_agent_runtime_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
        );
        let doc = default_storage
            .read_layout("default-agent")
            .await
            .unwrap()
            .expect("default scoped layout should exist");
        let line = doc
            .layout
            .iter()
            .find(|component| component.id == "default-agent-terminal")
            .and_then(|component| component.props.get("line"))
            .and_then(|value| value.as_str())
            .expect("default terminal line should exist");
        assert!(line.contains("default-target"));

        handle.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unscoped_batches_broadcast_and_cache_as_default_scope_under_spawn_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        workspace.ensure_root_sync().unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let (handle, cache) =
            MuijDeltaEmitter::spawn_scoped(Arc::clone(&broadcaster), workspace.clone());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut rx = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "exec-default-live".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("default-live-agent".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        broadcaster.emit(RuntimeTransportEvent::AgenticActionExecuted {
            execution_id: "exec-default-live".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            action_type: "click".into(),
            target: "default-live-target".into(),
            success: true,
            latency_ms: 1,
            error: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let mut found_scoped_delta = false;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await {
                Ok(Ok(RuntimeTransportEvent::AgentEvent { ref event }))
                    if event.event_type == "agent.ui.delta"
                        && event.agent_id == "default-live-agent" =>
                {
                    assert_eq!(event.principal.as_deref(), Some(DEFAULT_SCOPE_PRINCIPAL));
                    assert_eq!(event.workspace.as_deref(), Some(DEFAULT_SCOPE_WORKSPACE));
                    let payload = event
                        .payload
                        .as_object()
                        .expect("agent.ui.delta payload should be an object");
                    assert_eq!(
                        payload.get("principal"),
                        Some(&serde_json::json!(DEFAULT_SCOPE_PRINCIPAL))
                    );
                    assert_eq!(
                        payload.get("workspace"),
                        Some(&serde_json::json!(DEFAULT_SCOPE_WORKSPACE))
                    );
                    found_scoped_delta = true;
                    break;
                },
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => continue,
                Err(_) => break,
            }
        }

        assert!(
            found_scoped_delta,
            "should have received a default-scope agent.ui.delta event"
        );

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let scoped_cache_key = agent_snapshot_cache_key(
            "default-live-agent",
            Some(DEFAULT_SCOPE_PRINCIPAL),
            Some(DEFAULT_SCOPE_WORKSPACE),
        );
        assert!(
            cache.read().await.contains_key(&scoped_cache_key),
            "shared cache should mirror the default-scope cache key"
        );

        handle.abort();
    }

    // 13. coalescer_merges_rapid_upserts_in_integration
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coalescer_merges_rapid_upserts_in_integration() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));

        // 50ms window — enough to merge rapid events
        let (handle, _cache) = MuijDeltaEmitter::spawn_with_coalescing(
            Arc::clone(&broadcaster),
            std::time::Duration::from_millis(50),
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut rx = broadcaster.subscribe();

        // Register agent
        broadcaster.emit(RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-coal".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 10,
            hint_action: None,
            agent_id: Some("coal-agent".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Rapid-fire 3 iterations in quick succession — the Gauge upserts
        // for the same component_id should coalesce (last write wins)
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        for iter in 1..=3 {
            broadcaster.emit(RuntimeTransportEvent::AgenticIterationStarted {
                execution_id: "t-coal".into(),
                plan_id: "p".into(),
                step_id: "s".into(),
                iteration: iter,
                environment_type: "browser".into(),
                principal: None,
                workspace: None,
                timestamp: ts(),
            });
        }

        // Wait for coalescing window + processing
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Collect all AgentEvent deltas
        let mut gauge_deltas = Vec::new();
        let mut terminal_deltas = Vec::new();
        for _ in 0..50 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(RuntimeTransportEvent::AgentEvent { ref event }))
                    if event.event_type == "agent.ui.delta" =>
                {
                    let delta: MuijDelta = serde_json::from_value(event.payload.clone()).unwrap();
                    match &delta {
                        MuijDelta::Upsert { component_id, .. }
                            if component_id.ends_with("-gauge") =>
                        {
                            gauge_deltas.push(delta);
                        },
                        MuijDelta::Upsert { component_id, .. }
                            if component_id.ends_with("-terminal") =>
                        {
                            terminal_deltas.push(delta);
                        },
                        _ => {},
                    }
                },
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => continue,
                Err(_) => break,
            }
        }

        // The initial ExecutionStarted produces 1 gauge upsert (fill=0).
        // The 3 rapid IterationStarted events each produce a gauge upsert
        // (fill=0.1, 0.2, 0.3) — but the coalescer merges by component_id,
        // so we should see fewer than 4 gauge broadcasts.
        // The exact count depends on timing: the initial gauge might be in a
        // separate batch, but the 3 iteration gauges should coalesce.
        assert!(
            gauge_deltas.len() < 4,
            "coalescer should merge some gauge upserts, got {} (expected < 4)",
            gauge_deltas.len()
        );
        assert!(
            !gauge_deltas.is_empty(),
            "should have at least one gauge delta"
        );

        // The last gauge delta should reflect iteration 3 (fill = 0.3)
        let last_gauge = gauge_deltas.last().unwrap();
        match last_gauge {
            MuijDelta::Upsert { data, .. } => {
                let fill = data["props"]["fill"].as_f64().unwrap();
                assert!(
                    (fill - 0.3).abs() < 1e-9,
                    "last gauge should have fill=0.3 (iter 3/10), got {}",
                    fill
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        handle.abort();
    }

    // 12. stale_entries_evicted_after_sweep_interval
    #[test]
    fn stale_entries_evicted_after_sweep_interval() {
        let mut emitter = MuijDeltaEmitter {
            execution_map: HashMap::new(),
            active_cycle_map: HashMap::new(),
            agent_seq_map: HashMap::new(),
            agent_selectors_map: HashMap::new(),
            event_count: 0,
            last_sweep_at: Instant::now(),
            cumulative_lagged: 0,
        };

        // Insert an execution entry with an artificially old timestamp
        emitter.execution_map.insert(
            "stale-exec".into(),
            TrackedAgent {
                agent_id: "old-agent".into(),
                principal: None,
                workspace: None,
                max_iterations: 5,
                registered_at: Instant::now()
                    - std::time::Duration::from_secs(STALE_ENTRY_SECS + 1),
                cycle_id: None,
            },
        );
        // Insert a fresh entry
        emitter.execution_map.insert(
            "fresh-exec".into(),
            TrackedAgent {
                agent_id: "new-agent".into(),
                principal: None,
                workspace: None,
                max_iterations: 5,
                registered_at: Instant::now(),
                cycle_id: None,
            },
        );
        emitter.agent_selectors_map.insert(
            unscoped_cache_key("old-agent"),
            vec![SelectorEntry {
                selector: "#old".into(),
                action_type: "Click".into(),
                timestamp: 1,
                outcome: "success".into(),
                dom_changes: None,
            }],
        );
        emitter.agent_selectors_map.insert(
            unscoped_cache_key("new-agent"),
            vec![SelectorEntry {
                selector: "#new".into(),
                action_type: "Click".into(),
                timestamp: 2,
                outcome: "success".into(),
                dom_changes: None,
            }],
        );
        assert_eq!(emitter.execution_map.len(), 2);
        assert_eq!(emitter.agent_selectors_map.len(), 2);

        // Fast-forward event_count to just before a sweep
        emitter.event_count = SWEEP_INTERVAL - 1;

        // Next handle_event triggers the sweep
        let event = RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: "fresh-exec".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            environment_type: "browser".into(),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };
        emitter.handle_event(&event);

        // Stale entry should be gone, fresh entry remains
        assert_eq!(emitter.execution_map.len(), 1);
        assert!(emitter.execution_map.contains_key("fresh-exec"));
        assert!(!emitter.execution_map.contains_key("stale-exec"));
        assert_eq!(emitter.agent_selectors_map.len(), 1);
        assert!(emitter
            .agent_selectors_map
            .contains_key(&unscoped_cache_key("new-agent")));
        assert!(!emitter
            .agent_selectors_map
            .contains_key(&unscoped_cache_key("old-agent")));
    }

    // R438: MAX_EXECUTION_MAP_ENTRIES cap rejects new entries
    #[test]
    fn execution_map_cap_rejects_when_full() {
        let mut emitter = test_emitter();
        // Fill to capacity
        for i in 0..MAX_EXECUTION_MAP_ENTRIES {
            emitter.execution_map.insert(
                format!("exec-{i}"),
                TrackedAgent {
                    agent_id: format!("agent-{i}"),
                    principal: None,
                    workspace: None,
                    max_iterations: 5,
                    registered_at: Instant::now(),
                    cycle_id: None,
                },
            );
        }
        assert_eq!(emitter.execution_map.len(), MAX_EXECUTION_MAP_ENTRIES);

        // New entry should be rejected
        let result = emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "new-exec".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("new-agent".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert!(result.is_empty(), "should reject when at capacity");
        assert_eq!(emitter.execution_map.len(), MAX_EXECUTION_MAP_ENTRIES);
    }

    // R438: Cap also applies to recovery mapping path
    #[test]
    fn execution_map_cap_rejects_mapping_when_full() {
        let mut emitter = test_emitter();
        for i in 0..MAX_EXECUTION_MAP_ENTRIES {
            emitter.execution_map.insert(
                format!("exec-{i}"),
                TrackedAgent {
                    agent_id: format!("agent-{i}"),
                    principal: None,
                    workspace: None,
                    max_iterations: 5,
                    registered_at: Instant::now(),
                    cycle_id: None,
                },
            );
        }

        let result = emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "new-agent",
                serde_json::json!({
                    "execution_id": "new-exec",
                    "agent_id": "new-agent",
                    "max_iterations": 5
                }),
            ),
        });
        assert!(result.is_empty());
        assert_eq!(emitter.execution_map.len(), MAX_EXECUTION_MAP_ENTRIES);
    }

    // R440: apply_delta_to_document — upsert new component
    #[test]
    fn apply_delta_upsert_new_component() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        let delta = MuijDelta::Upsert {
            component_id: "comp-1".into(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "Test",
                "props": { "fill": 0.5 }
            }),
        };
        apply_delta_to_document(&mut doc, &delta);
        assert_eq!(doc.layout.len(), 1);
        assert_eq!(doc.layout[0].id, "comp-1");
        assert_eq!(doc.layout[0].component_type, "Gauge");
        assert_eq!(doc.layout[0].label, "Test");
        assert_eq!(doc.layout[0].props["fill"], 0.5);
    }

    // R440: apply_delta_to_document — upsert merge on existing component
    #[test]
    fn apply_delta_upsert_merge_existing() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        // Create initial component
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-1".into(),
                data: serde_json::json!({
                    "component_type": "Gauge",
                    "label": "Initial",
                    "props": { "fill": 0.0, "max_iterations": 5 }
                }),
            },
        );
        // Update with new props (shallow merge)
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-1".into(),
                data: serde_json::json!({
                    "label": "Updated",
                    "props": { "fill": 0.5 }
                }),
            },
        );
        assert_eq!(doc.layout.len(), 1, "should not create duplicate");
        assert_eq!(doc.layout[0].label, "Updated");
        assert_eq!(
            doc.layout[0].component_type, "Gauge",
            "type should be immutable (R458)"
        );
        assert_eq!(doc.layout[0].props["fill"], 0.5, "new prop value");
        assert_eq!(
            doc.layout[0].props["max_iterations"], 5,
            "existing prop preserved"
        );
    }

    // R440: apply_delta_to_document — remove
    #[test]
    fn apply_delta_remove() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-1".into(),
                data: serde_json::json!({"component_type": "Gauge", "label": "X", "props": {}}),
            },
        );
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-2".into(),
                data: serde_json::json!({"component_type": "Gauge", "label": "Y", "props": {}}),
            },
        );
        assert_eq!(doc.layout.len(), 2);

        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Remove {
                component_id: "comp-1".into(),
            },
        );
        assert_eq!(doc.layout.len(), 1);
        assert_eq!(doc.layout[0].id, "comp-2");
    }

    // R440: apply_delta_to_document — reorder
    #[test]
    fn apply_delta_reorder() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        for id in ["a", "b", "c"] {
            apply_delta_to_document(
                &mut doc,
                &MuijDelta::Upsert {
                    component_id: id.into(),
                    data: serde_json::json!({"component_type": "Gauge", "label": id, "props": {}}),
                },
            );
        }
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Reorder {
                ids: vec!["c".into(), "a".into(), "b".into()],
            },
        );
        let order: Vec<&str> = doc.layout.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, vec!["c", "a", "b"]);
    }

    // R396: Non-object data is rejected
    #[test]
    fn apply_delta_non_object_data_rejected() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-bad".into(),
                data: serde_json::json!("not an object"),
            },
        );
        assert!(doc.layout.is_empty(), "non-object data should be rejected");
    }

    // R457: component_type trimming on creation
    #[test]
    fn apply_delta_trims_component_type() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-trim".into(),
                data: serde_json::json!({
                    "component_type": "  Gauge  ",
                    "label": "X",
                    "props": {}
                }),
            },
        );
        assert_eq!(doc.layout[0].component_type, "Gauge", "should be trimmed");
    }

    // R458: component_type immutable on update
    #[test]
    fn apply_delta_component_type_immutable_on_update() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-immut".into(),
                data: serde_json::json!({
                    "component_type": "Gauge",
                    "label": "X",
                    "props": {}
                }),
            },
        );
        // Try to change type
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "comp-immut".into(),
                data: serde_json::json!({
                    "component_type": "TerminalTransient",
                    "label": "Y",
                    "props": {}
                }),
            },
        );
        assert_eq!(
            doc.layout[0].component_type, "Gauge",
            "type should remain Gauge"
        );
        assert_eq!(doc.layout[0].label, "Y", "other fields should update");
    }

    // R534: Children deduplication
    #[test]
    fn apply_delta_deduplicates_children() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "panel-1".into(),
                data: serde_json::json!({
                    "component_type": "Panel",
                    "label": "P",
                    "props": {},
                    "children": [
                        {"id": "child-1", "component_type": "Gauge", "label": "A", "props": {}},
                        {"id": "child-1", "component_type": "Gauge", "label": "B", "props": {}},
                        {"id": "child-2", "component_type": "Gauge", "label": "C", "props": {}}
                    ]
                }),
            },
        );
        assert_eq!(
            doc.layout[0].children.len(),
            2,
            "duplicate child-1 should be deduped"
        );
        assert_eq!(doc.layout[0].children[0].label, "A", "first-wins on dedup");
    }

    #[tokio::test]
    async fn persist_deltas_normalizes_duplicate_nested_component_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        let deltas = vec![MuijDelta::Upsert {
            component_id: "taskplan_tabs".to_string(),
            data: serde_json::json!({
                "component_type": "Tabs",
                "label": "Task Plan",
                "props": {},
                "children": [
                    {
                        "id": "taskplan_artifacts_panel",
                        "component_type": "Panel",
                        "label": "Artifacts",
                        "props": {},
                        "children": [
                            {
                                "id": "artifact_2",
                                "component_type": "DataList",
                                "label": "Artifact A",
                                "props": {}
                            },
                            {
                                "id": "artifact_2",
                                "component_type": "DataList",
                                "label": "Artifact B",
                                "props": {}
                            }
                        ]
                    }
                ]
            }),
        }];

        persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas,
        )
        .await
        .expect("nested duplicate ids should be normalized");

        let stored = storage
            .read_layout("test-agent")
            .await
            .expect("read layout")
            .expect("layout exists");
        let artifacts_panel = &stored.layout[0].children[0];
        assert_eq!(artifacts_panel.children.len(), 1);
        assert_eq!(artifacts_panel.children[0].id, "artifact_2");
        assert_eq!(artifacts_panel.children[0].label, "Artifact A");
    }

    // R509: max_iterations=0 normalization
    #[test]
    fn max_iterations_zero_normalized_to_one() {
        let mut emitter = test_emitter();
        let result = emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-zero".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 0,
            hint_action: None,
            agent_id: Some("agent-zero".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        assert_eq!(result.len(), 1);
        match &result[0].delta {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(
                    data["props"]["max_iterations"], 1,
                    "should normalize 0 to 1"
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R474: Duplicate ExecutionStarted is idempotent
    #[test]
    fn duplicate_execution_started_preserves_cycle_id() {
        let mut emitter = test_emitter();
        // Register with cycle binding
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-dup",
                serde_json::json!({
                    "execution_id": "t-dup",
                    "agent_id": "agent-dup",
                    "cycle_id": "c-dup",
                    "max_iterations": 5
                }),
            ),
        });
        assert_eq!(
            emitter
                .execution_map
                .get("t-dup")
                .unwrap()
                .cycle_id
                .as_deref(),
            Some("c-dup")
        );

        // Duplicate registration for same agent should keep cycle_id
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-dup".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 10,
            hint_action: None,
            agent_id: Some("agent-dup".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });
        let ctx = emitter.execution_map.get("t-dup").unwrap();
        assert_eq!(
            ctx.cycle_id.as_deref(),
            Some("c-dup"),
            "cycle_id should be preserved"
        );
        assert_eq!(ctx.max_iterations, 10, "max_iterations should be updated");
    }

    // R548: Reorder trims whitespace from IDs
    #[test]
    fn reorder_trims_whitespace_from_ids() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        for id in ["a", "b"] {
            apply_delta_to_document(
                &mut doc,
                &MuijDelta::Upsert {
                    component_id: id.into(),
                    data: serde_json::json!({"component_type": "Gauge", "label": id, "props": {}}),
                },
            );
        }
        // Reorder with whitespace-padded IDs
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Reorder {
                ids: vec!["  b  ".into(), " a ".into()],
            },
        );
        let order: Vec<&str> = doc.layout.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, vec!["b", "a"], "should match trimmed IDs");
    }

    // R450: Long reasoning text is truncated in terminal line
    #[test]
    fn decision_made_truncates_long_reasoning() {
        let mut emitter = test_emitter();
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t-trunc".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-trunc".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Create a very long reasoning string (3000+ chars)
        let long_reasoning = "x".repeat(3000);
        let event = RuntimeTransportEvent::AgenticDecisionMade {
            execution_id: "t-trunc".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            decision_type: "execute_action".into(),
            action_summary: None,
            reasoning: long_reasoning,
            confidence: 0.9,
            thinking: None,
            evidence: None,
            tool_name: None,
            action_type: None,
            element_id: None,
            candidates_count: None,
            raw_decision: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        match emitted.delta {
            MuijDelta::Upsert { data, .. } => {
                let line = data["props"]["line"].as_str().unwrap();
                // Line should be truncated to MAX_REASONING_CHARS + prefix
                assert!(
                    line.len() < 3000,
                    "line should be truncated, got {} chars",
                    line.len()
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R558: Malformed children normalize to empty vec (match frontend)
    #[test]
    fn apply_delta_malformed_children_normalize_to_empty() {
        let mut doc = MuijDocument::new(String::from("agent-test"));
        // Create component with valid children
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "panel-bad".into(),
                data: serde_json::json!({
                    "component_type": "Panel",
                    "label": "P",
                    "props": {},
                    "children": [
                        {"id": "child-1", "component_type": "Gauge", "label": "A", "props": {}}
                    ]
                }),
            },
        );
        assert_eq!(doc.layout[0].children.len(), 1);

        // Update with malformed children (string instead of array)
        apply_delta_to_document(
            &mut doc,
            &MuijDelta::Upsert {
                component_id: "panel-bad".into(),
                data: serde_json::json!({
                    "children": "not-an-array"
                }),
            },
        );
        // R558: Should normalize to empty, not retain stale children
        assert!(
            doc.layout[0].children.is_empty(),
            "malformed children should normalize to empty (R558)"
        );
    }

    // R450: truncate_str helper
    #[test]
    fn truncate_str_works_correctly() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("", 5), "");
        // Multi-byte: should not panic
        let emoji = "😀😀😀";
        let result = truncate_str(emoji, 2);
        assert_eq!(result.chars().count(), 2);
    }

    // R470: cumulative_lagged accessor
    #[test]
    fn cumulative_lagged_exposed() {
        let mut emitter = test_emitter();
        assert_eq!(emitter.cumulative_lagged(), 0);
        emitter.cumulative_lagged = 42;
        assert_eq!(emitter.cumulative_lagged(), 42);
    }

    // R569/R645: layout size cap prevents unbounded growth
    #[test]
    fn apply_delta_rejects_new_component_when_layout_at_cap() {
        let mut doc = MuijDocument::new("test-agent");
        // Fill layout to MAX_LAYOUT_COMPONENTS
        for i in 0..MAX_LAYOUT_COMPONENTS {
            doc.layout.push(MuijComponent {
                id: format!("c-{}", i),
                component_type: "Gauge".to_string(),
                label: "test".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({}),
                static_snapshot: None,
                children: vec![],
            });
        }
        assert_eq!(doc.layout.len(), MAX_LAYOUT_COMPONENTS);

        // Upsert of a NEW component should be rejected
        let delta = MuijDelta::Upsert {
            component_id: "overflow-component".to_string(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "overflow",
                "props": {}
            }),
        };
        apply_delta_to_document(&mut doc, &delta);
        assert_eq!(
            doc.layout.len(),
            MAX_LAYOUT_COMPONENTS,
            "should not exceed cap"
        );

        // Upsert of an EXISTING component should still work (update, not insert)
        let update_delta = MuijDelta::Upsert {
            component_id: "c-0".to_string(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "updated",
                "props": { "fill": 0.5 }
            }),
        };
        apply_delta_to_document(&mut doc, &update_delta);
        assert_eq!(doc.layout.len(), MAX_LAYOUT_COMPONENTS);
        assert_eq!(doc.layout[0].label, "updated");
    }

    // R643: AgenticActionExecuted truncates target and error fields
    #[test]
    fn action_executed_truncates_long_target_and_error() {
        let mut emitter = test_emitter();
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-trunc".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let long_target = "x".repeat(MAX_REASONING_CHARS + 500);
        let long_error = "e".repeat(MAX_REASONING_CHARS + 500);
        let event = RuntimeTransportEvent::AgenticActionExecuted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 1,
            action_type: "click".into(),
            target: long_target,
            success: false,
            latency_ms: 100,
            error: Some(long_error),
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event).into_iter().next().unwrap();
        match emitted.delta {
            MuijDelta::Upsert { data, .. } => {
                let line = data["props"]["line"].as_str().unwrap();
                // Line should be bounded — well under the full 2*(MAX_REASONING_CHARS+500) length
                assert!(
                    line.len() < MAX_REASONING_CHARS * 3,
                    "line should be bounded, got len={}",
                    line.len()
                );
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // ── R599: persist_deltas_to_storage tests ─────────────────────────────

    #[tokio::test]
    async fn persist_deltas_success_creates_and_updates_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        let deltas = vec![MuijDelta::Upsert {
            component_id: "g1".to_string(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "Test",
                "props": { "fill": 0.5 }
            }),
        }];

        let result = persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas,
        )
        .await;
        assert!(result.is_ok(), "persist should succeed: {:?}", result);
        let cache_key = unscoped_cache_key("test-agent");
        assert!(cache.contains_key(&cache_key), "cache should be populated");
        assert_eq!(cache[&cache_key].layout.len(), 1);
        assert_eq!(cache[&cache_key].layout[0].id, "g1");

        // Verify persisted to disk
        let on_disk = storage.read_layout("test-agent").await.unwrap().unwrap();
        assert_eq!(on_disk.layout.len(), 1);
    }

    #[tokio::test]
    async fn persist_deltas_empty_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        let result = persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &[],
        )
        .await;
        assert!(result.is_ok());
        assert!(cache.is_empty(), "empty deltas should not populate cache");
    }

    #[tokio::test]
    async fn persist_deltas_validation_failure_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        // Create a delta that would produce an invalid component type
        let deltas = vec![MuijDelta::Upsert {
            component_id: "bad1".to_string(),
            data: serde_json::json!({
                "component_type": "UnknownWidget",
                "label": "Bad",
                "props": {}
            }),
        }];

        let result = persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas,
        )
        .await;
        assert!(result.is_err(), "should fail validation");
        let err = result.unwrap_err();
        assert!(
            err.contains("validation failed"),
            "error should mention validation: {err}"
        );
    }

    #[tokio::test]
    async fn persist_deltas_uses_cache_on_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        // First persist
        let deltas1 = vec![MuijDelta::Upsert {
            component_id: "g1".to_string(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "First",
                "props": { "fill": 0.1 }
            }),
        }];
        persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas1,
        )
        .await
        .unwrap();

        // Second persist — should use cache (not re-read from disk)
        let deltas2 = vec![MuijDelta::Upsert {
            component_id: "g2".to_string(),
            data: serde_json::json!({
                "component_type": "TerminalTransient",
                "label": "Second",
                "props": { "line": "hello", "seq": 0 }
            }),
        }];
        persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas2,
        )
        .await
        .unwrap();

        let cache_key = unscoped_cache_key("test-agent");
        assert_eq!(
            cache[&cache_key].layout.len(),
            2,
            "both components should be in cache"
        );
    }

    #[tokio::test]
    async fn persist_deltas_quarantine_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut cache: HashMap<String, MuijDocument> = HashMap::new();

        // Populate cache with a valid doc
        let deltas = vec![MuijDelta::Upsert {
            component_id: "g1".to_string(),
            data: serde_json::json!({
                "component_type": "Gauge",
                "label": "Test",
                "props": { "fill": 0.5 }
            }),
        }];
        persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &deltas,
        )
        .await
        .unwrap();

        // Now try a delta that would fail validation — cache should keep old valid doc
        let bad_deltas = vec![MuijDelta::Upsert {
            component_id: "bad".to_string(),
            data: serde_json::json!({
                "component_type": "NonexistentType",
                "label": "Bad",
                "props": {}
            }),
        }];
        let result = persist_deltas_to_storage(
            &storage,
            &mut cache,
            &unscoped_cache_key("test-agent"),
            "test-agent",
            &bad_deltas,
        )
        .await;
        assert!(result.is_err());

        // Cache should still have the original valid doc (R382: clone-validate pattern)
        let cache_key = unscoped_cache_key("test-agent");
        assert_eq!(cache[&cache_key].layout.len(), 1);
        assert_eq!(cache[&cache_key].layout[0].id, "g1");
    }

    // R596: Test AgenticWaitingForUser handler
    #[test]
    fn waiting_for_user_emits_terminal_transient() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-wait-user".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticWaitingForUser {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 2,
            pause_state_id: None,
            correlation_id: None,
            is_retry: None,
            retry_count: None,
            agent_id: Some("agent-wait-user".into()),
            goal_id: None,
            cycle_id: None,
            escalation_trigger: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event);
        assert_eq!(emitted.len(), 1);

        let delta = &emitted[0];
        assert_eq!(delta.agent_id, "agent-wait-user");
        assert!(
            !delta.coalescable,
            "WaitingForUser should be non-coalescable"
        );

        match &delta.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-wait-user-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[waiting]"));
                assert!(data["props"]["seq"].is_number());
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R596: WaitingForUser for unregistered execution
    #[test]
    fn waiting_for_user_unknown_thread_returns_empty() {
        let mut emitter = test_emitter();
        let event = RuntimeTransportEvent::AgenticWaitingForUser {
            execution_id: "unknown".into(),
            plan_id: "p".into(),
            step_id: "s".into(),
            iteration: 1,
            pause_state_id: None,
            correlation_id: None,
            is_retry: None,
            retry_count: None,
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            escalation_trigger: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };
        assert!(emitter.handle_event(&event).is_empty());
    }

    // R597: Test AgenticWaitingForConfirmation handler
    #[test]
    fn waiting_for_confirmation_emits_terminal_transient() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-wait-confirm".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticWaitingForConfirmation {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            iteration: 3,
            pause_state_id: None,
            agent_id: Some("agent-wait-confirm".into()),
            goal_id: None,
            cycle_id: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event);
        assert_eq!(emitted.len(), 1);

        let delta = &emitted[0];
        assert_eq!(delta.agent_id, "agent-wait-confirm");
        assert!(!delta.coalescable);

        match &delta.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-wait-confirm-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[waiting]"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R598: Test AgenticExecutionCompleted handler
    #[test]
    fn execution_completed_emits_gauge_and_terminal() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 10,
            hint_action: None,
            agent_id: Some("agent-complete".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticExecutionCompleted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            outcome: "success".into(),
            iterations_used: 7,
            artifacts: vec![],
            duration_ms: 5000,
            summary: "completed".into(),
            timestamp: ts(),
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            refinement_pass_index: 0,
            refinement_pending: false,
            principal: None,
            workspace: None,
            yield_payload: None,
        };

        let emitted = emitter.handle_event(&event);
        // R739: Should emit both Gauge and TerminalTransient
        assert_eq!(emitted.len(), 2, "should emit Gauge + TerminalTransient");

        // First: Gauge (coalescable)
        let gauge = &emitted[0];
        assert!(gauge.coalescable);
        match &gauge.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-complete-gauge");
                assert_eq!(data["component_type"], "Gauge");
                assert_eq!(data["label"], "Execution Complete");
                assert_eq!(data["props"]["fill"], 1.0);
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        // Second: TerminalTransient (non-coalescable)
        let term = &emitted[1];
        assert!(!term.coalescable);
        match &term.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-complete-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[completed]"));
                assert!(line.contains("Execution Complete"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R598: Test failure outcome
    #[test]
    fn execution_completed_failure_emits_failed_label() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-fail".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticExecutionCompleted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            outcome: "failure".into(),
            iterations_used: 5,
            artifacts: vec![],
            duration_ms: 3000,
            summary: "failed".into(),
            timestamp: ts(),
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            refinement_pass_index: 0,
            refinement_pending: false,
            principal: None,
            workspace: None,
            yield_payload: None,
        };

        let emitted = emitter.handle_event(&event);
        assert_eq!(emitted.len(), 2);

        match &emitted[0].delta {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(data["label"], "Execution Failed");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        match &emitted[1].delta {
            MuijDelta::Upsert { data, .. } => {
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("Execution Failed"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R640: Test AgenticResumed handler
    #[test]
    fn resumed_emits_terminal_transient() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-resume".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticResumed {
            execution_id: "t1".into(),
            pause_state_id: None,
            plan_id: "p1".into(),
            step_id: "s1".into(),
            resumed_from_iteration: 3,
            input_type: "text".into(),
            user_responded: true,
            agent_id: Some("agent-resume".into()),
            goal_id: None,
            cycle_id: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event);
        assert_eq!(emitted.len(), 1);
        assert!(!emitted[0].coalescable);

        match &emitted[0].delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-resume-terminal");
                assert_eq!(data["component_type"], "TerminalTransient");
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("[resumed]"));
                assert!(line.contains("iteration 3"));
                assert!(line.contains("text"));
                assert!(line.contains("input provided"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R640: Test resumed with aborted input
    #[test]
    fn resumed_aborted_shows_aborted_status() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-abort".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let event = RuntimeTransportEvent::AgenticResumed {
            execution_id: "t1".into(),
            pause_state_id: None,
            plan_id: "p1".into(),
            step_id: "s1".into(),
            resumed_from_iteration: 1,
            input_type: "confirmation".into(),
            user_responded: false,
            agent_id: Some("agent-abort".into()),
            goal_id: None,
            cycle_id: None,
            principal: None,
            workspace: None,
            timestamp: ts(),
        };

        let emitted = emitter.handle_event(&event);
        match &emitted[0].delta {
            MuijDelta::Upsert { data, .. } => {
                let line = data["props"]["line"].as_str().unwrap();
                assert!(line.contains("aborted"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    // R602: Test cycle cleanup freshness threshold
    #[test]
    fn cycle_cleanup_preserves_fresh_unbound_entries() {
        let mut emitter = test_emitter();

        // Register an execution without cycle_id binding
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-fresh".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Without binding to a cycle, the execution has cycle_id: None
        // and was just registered (< 5 second threshold)
        assert_eq!(emitter.execution_map.len(), 1);
        assert!(emitter.execution_map.get("t1").unwrap().cycle_id.is_none());

        // Complete a different cycle for the same agent
        emitter.handle_event(&RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-fresh".into(),
            goal_id: "g1".into(),
            cycle_id: "c-other".into(),
            execution_id: None,
            outcome: "success".into(),
            iterations_used: 3,
            timestamp: ts(),
        });

        // The fresh unbound execution should be preserved (< 5s threshold, R379/R463)
        assert_eq!(
            emitter.execution_map.len(),
            1,
            "fresh unbound execution should survive cycle cleanup"
        );
    }

    // R742: Test mapping recovery preserves existing cycle_id
    #[test]
    fn mapping_recovery_preserves_existing_cycle_id() {
        let mut emitter = test_emitter();

        // Register execution and bind to cycle
        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-map".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Bind to cycle via mapping event
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-map",
                serde_json::json!({
                    "execution_id": "t1",
                    "agent_id": "agent-map",
                    "cycle_id": "c1",
                    "max_iterations": 5
                }),
            ),
        });

        assert_eq!(
            emitter.execution_map.get("t1").unwrap().cycle_id.as_deref(),
            Some("c1")
        );

        // Recovery mapping without cycle_id should NOT overwrite
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-map",
                serde_json::json!({
                    "execution_id": "t1",
                    "agent_id": "agent-map",
                    "max_iterations": 10
                }),
            ),
        });

        // R742: cycle_id should be preserved
        let entry = emitter.execution_map.get("t1").unwrap();
        assert_eq!(
            entry.cycle_id.as_deref(),
            Some("c1"),
            "cycle_id should be preserved (R742)"
        );
        assert_eq!(entry.max_iterations, 10, "max_iterations should be updated");
    }

    // GD-F01: Test DomChangeDetected with selector emits LiveSelectors delta
    #[test]
    fn dom_change_with_selector_emits_live_selectors_delta() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-selectors".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let emitted = emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t1".into(),
            correlation_id: "corr1".into(),
            total_changes: 5,
            nodes_added: 2,
            nodes_removed: 1,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#submit-button".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });

        assert_eq!(
            emitted.len(),
            2,
            "should emit both TerminalTransient and LiveSelectors"
        );

        let live_selectors_delta = &emitted[1];
        assert_eq!(live_selectors_delta.agent_id, "agent-selectors");
        assert!(
            live_selectors_delta.coalescable,
            "LiveSelectors should be coalescable"
        );

        match &live_selectors_delta.delta {
            MuijDelta::Upsert { component_id, data } => {
                assert_eq!(component_id, "agent-selectors-selectors");
                assert_eq!(data["component_type"], "LiveSelectors");
                assert_eq!(data["label"], "Target Elements");

                let selectors = data["props"]["selectors"].as_array().unwrap();
                assert_eq!(selectors.len(), 1);
                assert_eq!(selectors[0]["selector"], "#submit-button");
                assert_eq!(selectors[0]["action_type"], "Click");
                assert_eq!(selectors[0]["outcome"], "success");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        assert!(emitter
            .agent_selectors_map
            .contains_key(&unscoped_cache_key("agent-selectors")));
    }

    // GD-F01: Test LiveSelectors capped at MAX_SELECTORS_PER_AGENT
    #[test]
    fn live_selectors_capped_at_max_entries() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-cap".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        for i in 0..15 {
            emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
                execution_id: "t1".into(),
                correlation_id: format!("corr{}", i),
                total_changes: 1,
                nodes_added: 0,
                nodes_removed: 0,
                signals: vec![],
                initial_url: None,
                final_url: None,
                action_type: Some("Click".into()),
                selector: Some(format!("#btn-{}", i)),
                outcome: "success".into(),
                principal: None,
                workspace: None,
                timestamp: 1000 + i as i64,
            });
        }

        let entries = emitter
            .agent_selectors_map
            .get(&unscoped_cache_key("agent-cap"))
            .unwrap();
        assert_eq!(entries.len(), MAX_SELECTORS_PER_AGENT);

        assert_eq!(entries[0].selector, "#btn-5");
        assert_eq!(entries[9].selector, "#btn-14");
    }

    // GD-F01: Test LiveSelectors cleared on AgentCycleCompleted when no threads remain
    #[test]
    fn live_selectors_cleared_on_cycle_completed() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-clear".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Bind execution to cycle so it gets removed on cycle completion
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-clear",
                serde_json::json!({
                    "execution_id": "t1",
                    "agent_id": "agent-clear",
                    "cycle_id": "c1",
                    "max_iterations": 5
                }),
            ),
        });

        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t1".into(),
            correlation_id: "corr1".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#btn".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });

        assert!(emitter
            .agent_selectors_map
            .contains_key(&unscoped_cache_key("agent-clear")));

        // Complete the cycle - this removes the execution since it matches cycle_id
        emitter.handle_event(&RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-clear".into(),
            goal_id: "g".into(),
            cycle_id: "c1".into(),
            execution_id: None,
            outcome: "success".into(),
            iterations_used: 1,
            timestamp: ts(),
        });

        assert!(
            !emitter
                .agent_selectors_map
                .contains_key(&unscoped_cache_key("agent-clear")),
            "selectors should be cleared when no threads remain for agent"
        );
    }

    // R965: Starting a new cycle should reset selector history for that agent.
    #[test]
    fn live_selectors_reset_on_new_cycle_start() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-cycle-reset".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        // Seed selector history for prior cycle.
        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t1".into(),
            correlation_id: "corr-old".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#old-selector".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });
        assert_eq!(
            emitter
                .agent_selectors_map
                .get(&unscoped_cache_key("agent-cycle-reset"))
                .map(|entries| entries.len()),
            Some(1)
        );

        emitter.handle_event(&RuntimeTransportEvent::AgentCycleStarted {
            principal: None,
            workspace: None,
            agent_id: "agent-cycle-reset".into(),
            goal_id: "g".into(),
            cycle_id: "c2".into(),
            execution_id: None,
            goal: "g".into(),
            timestamp: ts(),
        });

        // First selector in the new cycle should not include old history.
        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t1".into(),
            correlation_id: "corr-new".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Type".into()),
            selector: Some("#new-selector".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 2000,
        });

        let entries = emitter
            .agent_selectors_map
            .get(&unscoped_cache_key("agent-cycle-reset"))
            .expect("selector history should exist for new cycle");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].selector, "#new-selector");
        assert_eq!(entries[0].action_type, "Type");
    }

    // R970: stale DOM events from overlapping old cycles should not repopulate selector history.
    #[test]
    fn live_selectors_ignore_stale_cycle_dom_changes_during_overlap() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-overlap",
                serde_json::json!({
                    "execution_id": "t-old",
                    "agent_id": "agent-overlap",
                    "cycle_id": "c-old",
                    "max_iterations": 5
                }),
            ),
        });

        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t-old".into(),
            correlation_id: "corr-old-1".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#old-before-reset".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });

        emitter.handle_event(&RuntimeTransportEvent::AgentCycleStarted {
            principal: None,
            workspace: None,
            agent_id: "agent-overlap".into(),
            goal_id: "g".into(),
            cycle_id: "c-new".into(),
            execution_id: None,
            goal: "g".into(),
            timestamp: ts(),
        });
        assert!(
            !emitter
                .agent_selectors_map
                .contains_key(&unscoped_cache_key("agent-overlap")),
            "new cycle start should reset selector history"
        );

        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-overlap",
                serde_json::json!({
                    "execution_id": "t-new",
                    "agent_id": "agent-overlap",
                    "cycle_id": "c-new",
                    "max_iterations": 5
                }),
            ),
        });

        let stale = emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t-old".into(),
            correlation_id: "corr-old-2".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#old-after-reset".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 2000,
        });
        assert_eq!(
            stale.len(),
            1,
            "stale-cycle event should only emit terminal delta"
        );

        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t-new".into(),
            correlation_id: "corr-new-1".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Type".into()),
            selector: Some("#new-cycle-selector".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 3000,
        });

        let entries = emitter
            .agent_selectors_map
            .get(&unscoped_cache_key("agent-overlap"))
            .expect("new-cycle selector history should exist");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].selector, "#new-cycle-selector");
        assert_eq!(entries[0].action_type, "Type");
    }

    // R971: mapping for new cycle can arrive before cycle-start event; selector
    // history must still reset when that start event arrives.
    #[test]
    fn live_selectors_reset_when_new_cycle_start_arrives_after_prebound_mapping() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-prebound",
                serde_json::json!({
                    "execution_id": "t-old",
                    "agent_id": "agent-prebound",
                    "cycle_id": "c-old",
                    "max_iterations": 5
                }),
            ),
        });
        emitter.handle_event(&RuntimeTransportEvent::AgentCycleStarted {
            principal: None,
            workspace: None,
            agent_id: "agent-prebound".into(),
            goal_id: "g".into(),
            cycle_id: "c-old".into(),
            execution_id: None,
            goal: "g".into(),
            timestamp: ts(),
        });
        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t-old".into(),
            correlation_id: "corr-old".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Click".into()),
            selector: Some("#old-selector".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });
        assert!(
            emitter
                .agent_selectors_map
                .contains_key(&unscoped_cache_key("agent-prebound")),
            "precondition: old-cycle selector history exists"
        );

        // New-cycle mapping arrives BEFORE AgentCycleStarted for new cycle.
        emitter.handle_event(&RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "agent.execution.mapping",
                "agent-prebound",
                serde_json::json!({
                    "execution_id": "t-new",
                    "agent_id": "agent-prebound",
                    "cycle_id": "c-new",
                    "max_iterations": 5
                }),
            ),
        });

        emitter.handle_event(&RuntimeTransportEvent::AgentCycleStarted {
            principal: None,
            workspace: None,
            agent_id: "agent-prebound".into(),
            goal_id: "g".into(),
            cycle_id: "c-new".into(),
            execution_id: None,
            goal: "g".into(),
            timestamp: ts(),
        });
        assert!(
            !emitter
                .agent_selectors_map
                .contains_key(&unscoped_cache_key("agent-prebound")),
            "new-cycle start must reset stale selector history even when mapping is pre-bound"
        );

        emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t-new".into(),
            correlation_id: "corr-new".into(),
            total_changes: 1,
            nodes_added: 0,
            nodes_removed: 0,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: Some("Type".into()),
            selector: Some("#new-selector".into()),
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 2000,
        });

        let entries = emitter
            .agent_selectors_map
            .get(&unscoped_cache_key("agent-prebound"))
            .expect("new-cycle selector history should exist");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].selector, "#new-selector");
        assert_eq!(entries[0].action_type, "Type");
    }

    // GD-F01: Test DomChangeDetected without selector only emits TerminalTransient
    #[test]
    fn dom_change_without_selector_only_emits_terminal() {
        let mut emitter = test_emitter();

        emitter.handle_event(&RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "t1".into(),
            plan_id: "p1".into(),
            step_id: "s1".into(),
            goal: "g".into(),
            success_criteria: "c".into(),
            max_iterations: 5,
            hint_action: None,
            agent_id: Some("agent-no-sel".into()),
            principal: None,
            workspace: None,
            timestamp: ts(),
        });

        let emitted = emitter.handle_event(&RuntimeTransportEvent::DomChangeDetected {
            execution_id: "t1".into(),
            correlation_id: "corr1".into(),
            total_changes: 5,
            nodes_added: 2,
            nodes_removed: 1,
            signals: vec![],
            initial_url: None,
            final_url: None,
            action_type: None,
            selector: None,
            outcome: "success".into(),
            principal: None,
            workspace: None,
            timestamp: 1000,
        });

        assert_eq!(emitted.len(), 1, "should only emit TerminalTransient");
        match &emitted[0].delta {
            MuijDelta::Upsert { component_id, .. } => {
                assert!(component_id.ends_with("-terminal"));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }

        assert!(!emitter
            .agent_selectors_map
            .contains_key(&unscoped_cache_key("agent-no-sel")));
    }

    // R953: Test normalize_outcome function
    #[test]
    fn normalize_outcome_valid_values_passthrough() {
        assert_eq!(normalize_outcome("success"), "success");
        assert_eq!(normalize_outcome("failed"), "failed");
    }

    #[test]
    fn normalize_outcome_unknown_values_fallback_to_pending() {
        assert_eq!(normalize_outcome("error"), "pending");
        assert_eq!(normalize_outcome("unknown"), "pending");
        assert_eq!(normalize_outcome(""), "pending");
        assert_eq!(normalize_outcome("SUCCESS"), "pending");
    }
}
