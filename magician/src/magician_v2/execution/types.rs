use std::collections::HashMap;

use blake3;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;
use uuid::Uuid;

use super::actions::ExecutableAction;
use super::constants::INFERENCE_CONFIDENCE_THRESHOLD;
use super::inference::infer_from_page_stage;
use super::merkle::PageMerkleTree;
// NOTE: recovery module removed - agentic execution handles state recovery
use super::agentic::PendingInput;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::{PlanStep, UnresolvedInput};

/// Internal key used to keep track of the long-lived browser session.
pub const GLOBAL_SESSION_KEY: &str = "__global_browser_session__";

/// Lifecycle status of a plan execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PlanStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    /// Execution was cancelled by user
    Cancelled,
    /// Execution paused waiting for JIT clarification
    WaitingForClarification,
    /// Execution blocked by critical state mismatch requiring intervention
    BlockedRequiresIntervention,
}

/// Execution status for an individual step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    /// Step paused waiting for user input (agentic execution)
    WaitingForUserInput,
}

/// Rich execution context shared across the observe → act loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutionContext {
    pub execution_id: Uuid,
    pub runtime_execution_id: String,
    pub plan_id: String,
    pub current_step_idx: usize,
    pub session_map: HashMap<String, String>,
    pub magicutor_state_note: Option<String>,
    pub magicutor_captured_state: Option<MagicutorCapturedState>,
    /// Flag indicating full state (screenshot+accessibility+DOM+browserState) was just captured.
    /// Used to avoid redundant capture_magicutor_state calls after capture_full_state.
    #[serde(skip)]
    pub full_state_just_captured: bool,
    pub step_results: HashMap<String, StepExecutionResult>,
    pub status: PlanStatus,
    pub page_states: Vec<PageStateSnapshot>,
    pub current_page_state: Option<PageState>,
    pub understanding_confidence: f64,
    pub last_observation_time: Option<DateTime<Utc>>,
    pub observation_decisions: Vec<ObservationDecision>,
    pub used_patterns: Vec<UsedPattern>,
    pub learned_patterns: Vec<String>,
    pub goal_records: HashMap<String, GoalExecutionRecord>,

    // Budget tracking (Phase 1e)
    pub initial_budget: f64,
    pub remaining_budget: f64,
    pub budget_spent: Vec<BudgetSpendRecord>,

    // JIT clarification tracking (Phase 2)
    /// Unresolved inputs that may need clarification during execution
    pub unresolved_inputs: Vec<UnresolvedInput>,
    /// Resolved input values populated during execution
    pub resolved_input_values: HashMap<String, Value>,
    /// Pending clarification question ID (when status is WaitingForClarification)
    pub pending_clarification_question_id: Option<String>,

    // === Elicitation Unification (agentic input handling) ===
    /// Pending inputs converted from UnresolvedInput at execution start.
    /// These are tracked/resolved during agentic execution.
    pub pending_inputs: Vec<PendingInput>,

    // Enhanced observability (Phase 2 Priority #3)
    /// Observability collector for detailed execution traces
    pub observability: ObservabilityCollector,

    // NOTE: recovery_history removed - agentic execution handles state recovery
    /// Steps that were blocked and skipped (non-critical blockers)
    #[serde(default)]
    pub blocked_steps: Vec<BlockedStepRecord>,

    /// Task ID for completion result persistence through the direct agentic path.
    /// When set, agentic execution can write `set_task_completion_result()` so
    /// episode recording picks up the summary/outcome/artifacts.
    #[serde(default)]
    pub task_id: Option<String>,
}

impl PlanExecutionContext {
    pub fn new(runtime_execution_id: impl Into<String>, plan_id: impl Into<String>) -> Self {
        Self::with_budget(runtime_execution_id, plan_id, 1.0) // Default $1.00 budget
    }

    pub fn with_budget(
        runtime_execution_id: impl Into<String>,
        plan_id: impl Into<String>,
        initial_budget: f64,
    ) -> Self {
        Self {
            execution_id: Uuid::new_v4(),
            runtime_execution_id: runtime_execution_id.into(),
            plan_id: plan_id.into(),
            current_step_idx: 0,
            session_map: HashMap::new(),
            magicutor_state_note: None,
            magicutor_captured_state: None,
            full_state_just_captured: false,
            step_results: HashMap::new(),
            status: PlanStatus::Pending,
            page_states: Vec::new(),
            current_page_state: None,
            understanding_confidence: 0.0,
            last_observation_time: None,
            observation_decisions: Vec::new(),
            used_patterns: Vec::new(),
            learned_patterns: Vec::new(),
            goal_records: HashMap::new(),
            initial_budget,
            remaining_budget: initial_budget,
            budget_spent: Vec::new(),
            unresolved_inputs: Vec::new(),
            resolved_input_values: HashMap::new(),
            pending_clarification_question_id: None,
            pending_inputs: Vec::new(),
            observability: ObservabilityCollector::new(),
            blocked_steps: Vec::new(),
            task_id: None,
        }
    }

    pub fn set_status(&mut self, status: PlanStatus) {
        self.status = status;
    }

    pub fn increment_step(&mut self) {
        self.current_step_idx = self.current_step_idx.saturating_add(1);
    }

    pub fn global_session_id(&self) -> Option<&String> {
        self.session_map.get(GLOBAL_SESSION_KEY)
    }

    pub fn update_global_session_id(&mut self, session_id: String) {
        self.session_map
            .insert(GLOBAL_SESSION_KEY.to_string(), session_id);
    }

    pub fn session_for_step(&self, step_id: &str) -> Option<&String> {
        self.session_map.get(step_id)
    }

    pub fn record_session_for_step(&mut self, step_id: &str, session_id: String) {
        self.session_map
            .insert(step_id.to_string(), session_id.clone());
        self.update_global_session_id(session_id);
    }

    pub fn set_magicutor_captured_state(&mut self, state: MagicutorCapturedState) {
        self.magicutor_captured_state = Some(state);
    }

    /// Mark that full state (all 4 capture types) was just captured.
    /// This flag avoids redundant recapture inside the current execution flow.
    pub fn mark_full_state_captured(&mut self) {
        self.full_state_just_captured = true;
    }

    /// Check and clear the full_state_just_captured flag.
    /// Returns true if full state was captured and clears the flag.
    pub fn take_full_state_captured(&mut self) -> bool {
        let was_captured = self.full_state_just_captured;
        self.full_state_just_captured = false;
        was_captured
    }

    pub fn record_step_result(&mut self, result: StepExecutionResult) {
        self.finalize_goal(&result.step_id, result.status, result.validation.clone());
        self.step_results.insert(result.step_id.clone(), result);
    }

    pub fn record_page_state(
        &mut self,
        step_id: impl Into<String>,
        action_description: impl Into<String>,
        mut page_state: PageState,
        confidence: f64,
    ) {
        let step_id_str = step_id.into();
        let action_desc_str = action_description.into();

        // Build Merkle tree if not already populated (enables O(1) change detection)
        // This ensures all recorded observations have Merkle hashes for ValidationAgent,
        // structure_changed/content_changed checks, and recovery polling.
        if page_state.merkle_structural_root.is_none() && page_state.accessibility_tree.is_some() {
            page_state.build_merkle_tree(None);
        }

        // Log observation data capture
        let has_screenshot = page_state.screenshot_data.is_some();
        let has_accessibility = page_state.accessibility_tree.is_some();
        let has_merkle = page_state.merkle_structural_root.is_some();
        let url = page_state.url.as_deref().unwrap_or("unknown");

        info!(
            step_id = %step_id_str,
            action = %action_desc_str,
            has_screenshot = has_screenshot,
            has_accessibility = has_accessibility,
            has_merkle = has_merkle,
            url = %url,
            confidence = confidence,
            page_states_count = self.page_states.len() + 1,
            "📸 Recording page observation for step"
        );

        let snapshot = PageStateSnapshot {
            step_id: step_id_str,
            action_description: action_desc_str,
            page_state: page_state.clone(),
            timestamp: Utc::now(),
            confidence,
        };
        self.last_observation_time = Some(snapshot.timestamp);
        self.current_page_state = Some(page_state);
        self.page_states.push(snapshot);
    }

    pub fn push_observation_decision(&mut self, decision: ObservationDecision) {
        self.observation_decisions.push(decision);
    }

    pub fn record_derived_execution(&mut self, step_id: &str, derived_execution: DerivedExecution) {
        let record = self
            .goal_records
            .entry(step_id.to_string())
            .or_insert_with(|| GoalExecutionRecord {
                plan_step_id: step_id.to_string(),
                derived_executions: Vec::new(),
                final_status: StepStatus::Pending,
                validation: None,
            });
        record.derived_executions.push(derived_execution);
    }

    pub fn finalize_goal(
        &mut self,
        step_id: &str,
        final_status: StepStatus,
        validation: Option<ActionValidation>,
    ) {
        let record = self
            .goal_records
            .entry(step_id.to_string())
            .or_insert_with(|| GoalExecutionRecord {
                plan_step_id: step_id.to_string(),
                derived_executions: Vec::new(),
                final_status,
                validation: validation.clone(),
            });
        record.final_status = final_status;
        record.validation = validation;
    }

    /// Record budget spend for an operation (Phase 1e)
    pub fn record_budget_spend(
        &mut self,
        amount: f64,
        reason: impl Into<String>,
        step_id: Option<String>,
    ) {
        if amount <= 0.0 {
            return;
        }

        self.remaining_budget = (self.remaining_budget - amount).max(0.0);
        self.budget_spent.push(BudgetSpendRecord {
            amount,
            reason: reason.into(),
            step_id,
            timestamp: Utc::now(),
        });
    }

    /// Get total budget spent
    pub fn total_budget_spent(&self) -> f64 {
        self.budget_spent.iter().map(|s| s.amount).sum()
    }

    /// Check if budget is exhausted
    pub fn is_budget_exhausted(&self) -> bool {
        self.remaining_budget <= 0.0
    }

    /// Get budget utilization percentage (0.0 to 1.0)
    pub fn budget_utilization(&self) -> f64 {
        if self.initial_budget <= 0.0 {
            return 0.0;
        }
        (self.initial_budget - self.remaining_budget) / self.initial_budget
    }

    /// Check if an input is resolved (Phase 2 - JIT clarifications)
    pub fn is_input_resolved(&self, input_id: &str) -> bool {
        self.resolved_input_values.contains_key(input_id)
    }

    /// Get unresolved inputs required by a specific step (Phase 2 - JIT clarifications)
    pub fn get_unresolved_inputs_for_step(&self, step_id: &str) -> Vec<&UnresolvedInput> {
        self.unresolved_inputs
            .iter()
            .filter(|input| {
                // Input is required by this step if:
                // 1. step_id matches, OR
                // 2. step_id is in linked_steps
                input.step_id.as_deref() == Some(step_id)
                    || input.linked_steps.contains(&step_id.to_string())
            })
            .filter(|input| !self.is_input_resolved(&input.id))
            .collect()
    }

    /// Record a resolved input value (Phase 2 - JIT clarifications)
    pub fn record_resolved_input(&mut self, input_id: String, value: Value) {
        self.resolved_input_values.insert(input_id, value);
    }

    /// Attempt to infer input values from current page observations (vision analysis).
    ///
    /// This method tries to resolve unresolved inputs by inferring values from the
    /// current page state. For example, if we're on a Login page and have an
    /// "authorized" parameter, we can infer it should be `false`.
    ///
    /// Returns the list of inputs that could NOT be inferred (still need JIT clarification).
    pub fn try_infer_inputs_from_observations(
        &mut self,
        unresolved: &[UnresolvedInput],
    ) -> Vec<UnresolvedInput> {
        // Clone page stage to avoid borrow conflicts when recording inferred values
        let page_stage = match &self.current_page_state {
            Some(state) => state.current_stage,
            None => {
                info!("[INFERENCE] No page state available, skipping inference");
                return unresolved.to_vec();
            },
        };

        info!(
            "[INFERENCE] Attempting to infer {} unresolved inputs from page stage {:?}",
            unresolved.len(),
            page_stage
        );

        let mut still_unresolved = Vec::new();

        for input in unresolved {
            // Try to infer the value from page stage
            if let Some(inference) = infer_from_page_stage(
                &page_stage,
                &input.parameter,
                INFERENCE_CONFIDENCE_THRESHOLD as f32,
            )
            // f32 cast safe: 0.75 is exact in both precisions
            {
                info!(
                    "[INFERENCE] Successfully inferred '{}' = {} (confidence: {:.2}, reason: {})",
                    input.id, inference.value, inference.confidence, inference.reason
                );

                // Record the inferred value
                self.record_resolved_input(input.id.clone(), inference.value);
            } else {
                info!(
                    "[INFERENCE] Could not infer value for '{}' (parameter: {})",
                    input.id, input.parameter
                );
                still_unresolved.push(input.clone());
            }
        }

        info!(
            "[INFERENCE] Inference complete: {} resolved, {} still unresolved",
            unresolved.len() - still_unresolved.len(),
            still_unresolved.len()
        );

        still_unresolved
    }

    // =========================================================================
    // State Recovery Methods
    // =========================================================================

    /// Get the current page stage from the most recent observation
    pub fn current_page_stage(&self) -> PageStage {
        self.current_page_state
            .as_ref()
            .map(|s| s.current_stage)
            .unwrap_or(PageStage::Unknown)
    }

    /// Record a step that was blocked and skipped (non-critical)
    pub fn record_step_blocked(&mut self, step_id: impl Into<String>, reason: impl Into<String>) {
        let page_stage = self.current_page_stage();
        self.blocked_steps.push(BlockedStepRecord {
            step_id: step_id.into(),
            reason: reason.into(),
            page_stage,
            blocked_at: Utc::now(),
        });
    }

    // NOTE: get_recent_observations_serialized removed - was only used for legacy recovery
    // NOTE: invalidate_inferred_values removed - agentic execution handles state changes

    // === Elicitation Unification Methods ===

    /// Populate pending_inputs from unresolved_inputs at execution start.
    /// This is the handoff from planning → execution.
    pub fn populate_pending_inputs_from_unresolved(&mut self) {
        self.pending_inputs = self
            .unresolved_inputs
            .iter()
            .map(PendingInput::from_unresolved)
            .collect();
    }

    /// Prepare a specific step for execution by converting its unresolved inputs.
    ///
    /// This is a step-level handoff that:
    /// 1. Finds all unresolved inputs for this step
    /// 2. Converts them to PendingInput format (if not already done)
    /// 3. Returns the IDs of inputs that were newly converted (for session marking)
    ///
    /// This allows incremental handoff at step start rather than all at once.
    pub fn prepare_step_for_execution(&mut self, step_id: &str) -> Vec<String> {
        let mut newly_converted_ids = Vec::new();

        // Get unresolved inputs for this step
        let step_unresolved: Vec<_> = self
            .unresolved_inputs
            .iter()
            .filter(|input| input.step_id.as_deref() == Some(step_id) || input.step_id.is_none())
            .collect();

        for unresolved in step_unresolved {
            // Check if already converted to pending
            let already_pending = self.pending_inputs.iter().any(|p| p.id == unresolved.id);

            if !already_pending {
                // Convert and add
                let pending = PendingInput::from_unresolved(unresolved);
                newly_converted_ids.push(pending.id.clone());
                self.pending_inputs.push(pending);
            }
        }

        newly_converted_ids
    }

    /// Get pending inputs for a specific step that are not yet resolved.
    pub fn get_pending_inputs_for_step(&self, step_id: &str) -> Vec<&PendingInput> {
        self.pending_inputs
            .iter()
            .filter(|input| {
                // Input is relevant to this step if:
                // 1. step_id matches, OR
                // 2. step_id is None (shared input)
                input.step_id.as_deref() == Some(step_id) || input.step_id.is_none()
            })
            .filter(|input| !input.is_resolved())
            .collect()
    }

    /// Record a resolved pending input value.
    pub fn record_pending_input_resolved(&mut self, input_id: &str, value: Value) {
        if let Some(input) = self.pending_inputs.iter_mut().find(|i| i.id == input_id) {
            input.resolve(value.clone());
        }
        // Also record in resolved_input_values for backwards compatibility
        self.resolved_input_values
            .insert(input_id.to_string(), value);
    }
}

/// Record of budget spend during execution (Phase 1e)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSpendRecord {
    pub amount: f64,
    pub reason: String,
    pub step_id: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Record of a step that was blocked and skipped (State Recovery)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedStepRecord {
    /// The step ID that was blocked
    pub step_id: String,
    /// Why the step was blocked
    pub reason: String,
    /// The page stage that caused the block
    pub page_stage: PageStage,
    /// When the step was blocked
    pub blocked_at: DateTime<Utc>,
}

/// Observation captured after a step completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageStateSnapshot {
    pub step_id: String,
    pub action_description: String,
    pub page_state: PageState,
    pub timestamp: DateTime<Utc>,
    pub confidence: f64,
}

/// Unified understanding of the page (vision + DOM).
///
/// This is the canonical browser state type used across both agentic and non-agentic
/// execution flows. Includes Merkle tree support for O(1) change detection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PageState {
    /// Raw accessibility tree (CDP AXTree nodes)
    /// DEPRECATED: No longer captured - using DOM-only approach
    pub accessibility_tree: Option<Value>,
    /// AI-friendly accessibility snapshot (compact YAML-like text, Playwright-style).
    /// Authoritative observations no longer capture this separately; the
    /// provider-facing decision snapshot may use it as a bounded textual view
    /// of `accessibility_tree` to avoid cloning the complete JSON tree.
    #[serde(default, rename = "accessibilitySnapshotForAI")]
    pub accessibility_snapshot_for_ai: Option<String>,
    pub url: Option<String>,
    /// Page title (from document.title)
    #[serde(default)]
    pub title: Option<String>,
    pub ready_state: PageReadyState,
    pub interactive_elements: Vec<MappedElement>,
    pub current_stage: PageStage,
    pub available_actions: Vec<SuggestedAction>,
    pub captured_at: Option<DateTime<Utc>>,
    pub metadata: HashMap<String, String>,
    /// Error messages visible on the page (for validation)
    #[serde(default)]
    pub errors: Vec<String>,
    /// Whether the page appears to be loading (for validation)
    #[serde(default)]
    pub loading: bool,
    /// Visible text content on the page (for text-based analysis)
    #[serde(default)]
    pub visible_text: Option<String>,
    /// Screenshot data (base64) for vision analysis
    /// NOTE: This field is NOT serialized to avoid duplication. Screenshots are stored on disk
    /// under the scoped V3 execution workspace and referenced via observation_id.
    #[serde(skip_serializing)]
    pub screenshot_data: Option<String>,
    /// Observation ID for fetching screenshot from disk (replaces screenshot_data in serialization)
    /// Format: `obs_{seq:04d}_{capture_label}_{uuid}`. The sequence is a
    /// process-local ordering hint; the UUID is the storage authority across
    /// restarts.
    /// Screenshot stored under:
    /// `magician_data_v3/scopes/<principal>/<workspace>/tasks/<task_id>/executions/{execution_id}/observations/{observation_id}.png`
    #[serde(default)]
    pub observation_id: Option<String>,

    /// SOTA Phase 6: Screenshot hash for visual change detection.
    /// Computed from screenshot_data for fast comparison without storing full images.
    #[serde(default)]
    pub screenshot_hash: Option<u64>,

    // === DOM Snapshot (for Merkle Tree Building) ===
    /// Full DOM snapshot (outerHTML) for Merkle tree construction.
    /// When present, enables:
    /// 1. Full Merkle tree coverage (100% vs ~10-50% from a11y tree)
    /// 2. Better element attribute extraction for change detection
    /// 3. Accurate structural hash for state comparison
    #[serde(default)]
    pub dom_snapshot: Option<String>,

    /// CDP DOM capture from `captureCompleteDomTree()` with pierce:true.
    /// Provides 100% coverage including:
    /// - Cross-origin iframes (CDP bypasses same-origin policy)
    /// - Closed shadow DOM (CDP can access)
    /// - All nested content recursively
    ///
    /// Contains the tree, stats, hash, and timing info.
    /// When present, this is preferred over `dom_snapshot` for Merkle tree building.
    #[serde(default, rename = "comprehensiveDomTree")]
    pub cdp_dom_tree: Option<super::merkle::ComprehensiveDomCapture>,

    /// Parsed DOM elements with CSS selectors.
    /// Populated by parsing dom_snapshot during observation.
    /// NOTE: SelectorResolver removed - SoM visual grounding is now the primary mechanism.
    #[serde(skip)]
    pub dom_elements: Vec<super::dom_parser::DomElement>,

    // === Merkle Tree Fields (for efficient change detection) ===
    /// Merkle tree for efficient change detection (transient, not serialized)
    #[serde(skip)]
    pub merkle_tree: Option<PageMerkleTree>,

    /// Structural root hash - STABLE across content changes (typing, checkbox toggles)
    /// Use for recovery polling (waiting for page structure to change)
    #[serde(default)]
    pub merkle_structural_root: Option<String>,

    /// Content root hash - changes with ANY content change
    /// Use for validation (did form fill work?)
    #[serde(default)]
    pub merkle_content_root: Option<String>,

    // === SoM Interactive Element Fields (for Set-of-Mark visual grounding) ===
    /// Raw interactive elements from Magicutor (for SoM annotation)
    /// These contain bounding boxes and are used for screenshot annotation.
    #[serde(default, rename = "interactiveElements")]
    pub interactive_elements_raw: Option<Vec<SoMInteractiveElement>>,

    /// Detected overlay containers from Magicutor
    #[serde(default, rename = "overlayContainers")]
    pub overlay_containers: Option<Vec<SoMOverlayContainer>>,

    /// Device pixel ratio at time of capture (for coordinate scaling)
    #[serde(default, rename = "devicePixelRatio")]
    pub device_pixel_ratio: Option<f64>,

    /// Scroll offset at time of capture
    #[serde(default, rename = "scrollOffset")]
    pub scroll_offset: Option<ScrollOffset>,

    /// Viewport size at time of capture
    #[serde(default, rename = "viewportSize")]
    pub viewport_size: Option<ViewportSize>,

    /// Document size (total scrollable area)
    /// Used to determine if there's more content below the current viewport
    #[serde(default, rename = "documentSize")]
    pub document_size: Option<DocumentSize>,

    /// Detection statistics for hybrid element detection (SOTA enhancement)
    /// Includes counts by detection method, context, and clickability validation
    #[serde(default, rename = "detectionStats")]
    pub detection_stats: Option<DetectionStats>,

    // === Spatial Surface Detection ===
    /// Spatial work surfaces detected on the page (canvas, SVG, or DOM-backed editor surfaces).
    /// These hints let the executor stay SoM/DOM-first while recognizing when a step is
    /// fundamentally spatial and needs coordinate-native grounding or visual verification.
    #[serde(default, rename = "spatialSurfaces")]
    pub spatial_surfaces: Option<Vec<SpatialSurfaceInfo>>,

    // === SOTA Phase 0: Cross-Origin Iframe Support ===
    /// Information about cross-origin iframes detected on the page.
    /// These iframes (like Stripe checkout, YouTube embeds) require special
    /// handling via CDP Target.getTargets to extract interactive elements.
    #[serde(default, rename = "crossOriginFrames")]
    pub cross_origin_frames: Option<Vec<CrossOriginFrameInfo>>,

    // === SOTA Phase 1: SoM Scalability ===
    /// Dense regions on the page (areas with >10 elements in 200x200px cell).
    /// LLM can use this to decide whether to scroll or zoom for better annotation.
    #[serde(default, rename = "denseRegions")]
    pub dense_regions: Option<Vec<DenseRegion>>,

    /// Semantic clusters grouping related elements (forms, navs, tables, lists).
    /// Helps LLM understand page structure and focus on relevant element groups.
    #[serde(default, rename = "semanticClusters")]
    pub semantic_clusters: Option<Vec<SemanticCluster>>,

    // === SOTA Phase 13: Semantic Breadcrumbs (Flow Context) ===
    /// Flow context for multi-step workflows (breadcrumbs, active tabs, step indicators).
    /// Helps LLM understand where the user is in checkout flows, wizards, etc.
    #[serde(default, rename = "flowContext")]
    pub flow_context: Option<FlowContext>,

    // === SOTA Phase 2: Network & Console Error Integration ===
    /// Network and console error context captured from CDP.
    /// Surfaces XHR failures, HTTP error codes, console.error messages,
    /// rate limiting (429), auth errors (401/403), and CORS issues.
    #[serde(default, rename = "networkContext")]
    pub network_context: Option<NetworkContext>,

    // === SOTA Phase 4: Bot Detection & Adversarial Awareness ===
    /// Bot detection context - honeypots, CAPTCHAs, rate limits, challenge pages.
    /// Warns the LLM about anti-automation measures on the page.
    /// Risk levels: "none", "low", "medium", "high", "unknown"
    #[serde(default, rename = "botDetectionContext")]
    pub bot_detection_context: Option<BotDetectionContext>,

    // === SOTA Phase 5: Temporal Context ===
    /// Temporal context from 3-frame strip analysis.
    /// Detects loading spinners, animations, progress bars, and page stability.
    /// Helps agent decide when to wait vs when to act.
    #[serde(default, rename = "temporalContext")]
    pub temporal_context: Option<TemporalContext>,

    // === SOTA Phase 7: Hover Probe Summary ===
    /// Results from hover probing - elements discovered by hovering.
    /// Only populated when hover_probing is enabled in observe action.
    #[serde(default, rename = "hoverProbeSummary")]
    pub hover_probe_summary: Option<HoverProbeSummary>,

    /// Whether execution is paused (e.g., dialog pending).
    #[serde(default, rename = "executionPaused")]
    pub execution_paused: bool,

    /// Reason for execution pause (e.g., "dialog_pending").
    #[serde(default, rename = "pauseReason")]
    pub pause_reason: Option<String>,

    // === TEXT-FIRST: Page State Info ===
    /// Page state info from extension (focus, loading, errors, forms, modal)
    /// Captured when page_state=true in observe action.
    /// Used by text-first mode to provide rich context to the LLM.
    #[serde(default, rename = "pageStateInfo")]
    pub page_state_info: Option<RawPageStateInfo>,
}

// === TEXT-FIRST: Raw Page State Types (from extension) ===

/// Raw page state info from the extension's pageState capture.
/// Contains focused element, loading state, error messages, form states, modal detection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RawPageStateInfo {
    /// Currently focused element (keyboard focus)
    #[serde(default, rename = "focusedElement")]
    pub focused_element: Option<RawFocusedElement>,

    /// Whether the page appears to be loading
    #[serde(default, rename = "isLoading")]
    pub is_loading: bool,

    /// Loading indicators detected on the page
    #[serde(default, rename = "loadingIndicators")]
    pub loading_indicators: Vec<RawLoadingIndicator>,

    /// Error messages visible on the page
    #[serde(default, rename = "errorMessages")]
    pub error_messages: Vec<RawErrorMessage>,

    /// Form states for visible forms
    #[serde(default, rename = "formStates")]
    pub form_states: Vec<RawFormState>,

    /// Whether a modal/dialog is active
    #[serde(default, rename = "hasActiveModal")]
    pub has_active_modal: bool,

    /// Scroll position info
    #[serde(default, rename = "scrollPosition")]
    pub scroll_position: Option<RawScrollPosition>,
}

/// Focused element info from extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFocusedElement {
    /// HTML tag name (e.g., "input", "button")
    pub tag: String,
    /// ARIA role
    pub role: Option<String>,
    /// Element ID
    pub id: Option<String>,
    /// Element name attribute
    pub name: Option<String>,
    /// ARIA label
    #[serde(rename = "ariaLabel")]
    pub aria_label: Option<String>,
    /// Placeholder text
    pub placeholder: Option<String>,
    /// Input type (for input elements)
    #[serde(rename = "type")]
    pub input_type: Option<String>,
    /// Text content (for non-input elements)
    pub text: Option<String>,
}

/// Loading indicator from extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawLoadingIndicator {
    /// CSS selector that matched
    pub selector: String,
    /// ARIA label if present
    #[serde(rename = "ariaLabel")]
    pub aria_label: Option<String>,
}

/// Error message from extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawErrorMessage {
    /// Error text content
    pub text: String,
    /// CSS selector that matched
    pub selector: String,
    /// Associated field name/id
    #[serde(rename = "forField")]
    pub for_field: Option<String>,
}

/// Form state from extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFormState {
    /// Form index on page
    pub index: usize,
    /// Form ID
    pub id: Option<String>,
    /// Form name
    pub name: Option<String>,
    /// Form action URL
    pub action: Option<String>,
    /// Total number of fields
    #[serde(default, rename = "totalFields")]
    pub total_fields: usize,
    /// Number of filled fields
    #[serde(default, rename = "filledFields")]
    pub filled_fields: usize,
    /// Number of invalid fields
    #[serde(default, rename = "invalidFields")]
    pub invalid_fields: usize,
    /// Number of required but empty fields
    #[serde(default, rename = "requiredEmptyFields")]
    pub required_empty_fields: usize,
}

/// Scroll position from extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawScrollPosition {
    /// Scroll Y position
    pub top: f64,
    /// Total document height
    #[serde(default, rename = "scrollHeight")]
    pub scroll_height: f64,
    /// Viewport height
    #[serde(default, rename = "viewportHeight")]
    pub viewport_height: f64,
    /// Viewport width (for proper scroll context)
    #[serde(default, rename = "viewportWidth")]
    pub viewport_width: f64,
    /// Total document width
    #[serde(default, rename = "documentWidth")]
    pub document_width: f64,
    /// Percentage scrolled (0-100)
    #[serde(default, rename = "percentScrolled")]
    pub percent_scrolled: u8,
    /// Whether there's more content below
    #[serde(default, rename = "hasMoreBelow")]
    pub has_more_below: bool,
    /// Whether there's more content above
    #[serde(default, rename = "hasMoreAbove")]
    pub has_more_above: bool,
}

/// Statistics about hybrid element detection (SOTA enhancement).
///
/// Provides visibility into how elements were detected:
/// - Detection method (selector, event_listener, ax_tree)
/// - Context (main, shadow, iframe)
/// - Clickability validation (disabled, occluded, pointer-events)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetectionStats {
    /// Total number of interactive elements detected
    #[serde(default, rename = "totalElements")]
    pub total_elements: usize,

    /// Elements detected via DOM selectors (semantic)
    #[serde(default, rename = "viaSelector")]
    pub via_selector: usize,

    /// Elements detected via event listener instrumentation
    #[serde(default, rename = "viaEventListener")]
    pub via_event_listener: usize,

    /// Elements detected via CDP Accessibility Tree cross-reference
    #[serde(default, rename = "viaAxTree")]
    pub via_ax_tree: usize,

    /// Elements in main document context
    #[serde(default, rename = "inMain")]
    pub in_main: usize,

    /// Elements in shadow DOM contexts
    #[serde(default, rename = "inShadow")]
    pub in_shadow: usize,

    /// Elements in same-origin iframes
    #[serde(default, rename = "inIframe")]
    pub in_iframe: usize,

    /// Elements in shadow DOM within iframes
    #[serde(default, rename = "inIframeShadow")]
    pub in_iframe_shadow: usize,

    /// Whether event listener instrumentation was available
    #[serde(default, rename = "instrumentationAvailable")]
    pub instrumentation_available: bool,

    // SOTA 4: Clickability validation stats
    /// Number of elements that passed clickability validation
    #[serde(default)]
    pub clickable: usize,

    /// Number of elements that failed clickability validation
    #[serde(default, rename = "nonClickable")]
    pub non_clickable: usize,

    /// Number of elements occluded by other elements
    #[serde(default)]
    pub occluded: usize,

    /// Number of disabled elements
    #[serde(default)]
    pub disabled: usize,

    /// Number of elements with pointer-events: none
    #[serde(default, rename = "pointerEventsNone")]
    pub pointer_events_none: usize,

    // SOTA Phase 0: Cross-origin iframe detection stats
    /// Number of cross-origin iframes detected on the page
    #[serde(default, rename = "crossOriginFrameCount")]
    pub cross_origin_frame_count: usize,

    /// Number of elements extracted from cross-origin iframes
    #[serde(default, rename = "viaCrossOriginFrames")]
    pub via_cross_origin_frames: usize,

    /// Detailed stats about cross-origin frame extraction (optional)
    #[serde(default, rename = "crossOriginStats")]
    pub cross_origin_stats: Option<CrossOriginExtractionStats>,

    // SOTA Phase 1: Priority and viewport stats
    /// Number of elements with center fully in viewport
    #[serde(default, rename = "inViewport")]
    pub in_viewport: usize,

    /// Number of high-priority elements (priority >= 80: submit buttons, required inputs)
    #[serde(default, rename = "highPriority")]
    pub high_priority: usize,

    /// Number of medium-priority elements (priority 60-79: regular buttons, links)
    #[serde(default, rename = "mediumPriority")]
    pub medium_priority: usize,

    /// Number of low-priority elements (priority < 60: tabs, checkboxes, etc.)
    #[serde(default, rename = "lowPriority")]
    pub low_priority: usize,

    /// Number of dense regions (200x200px cells with >10 elements)
    #[serde(default, rename = "denseRegionCount")]
    pub dense_region_count: usize,

    /// Number of semantic clusters (forms, navs, tables, lists)
    #[serde(default, rename = "semanticClusterCount")]
    pub semantic_cluster_count: usize,
}

/// A dense region on the page (many elements in a small area).
/// Used to identify areas that may need hierarchical IDs or zooming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DenseRegion {
    /// Cell key (e.g., "2,3" for cell at x=400-600, y=600-800)
    pub cell_key: String,

    /// Bounding rect of the dense region (200x200px cell)
    pub rect: SoMBoundingRect,

    /// Number of elements in this region
    pub element_count: usize,

    /// IDs of elements in this region (first 20)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub element_ids: Vec<usize>,

    /// Breakdown of element types in this region
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub element_types: std::collections::HashMap<String, usize>,
}

/// A semantic cluster grouping related elements.
/// Used to identify forms, navigation, tables, and lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticCluster {
    /// Cluster type (form, navigation, table, list)
    #[serde(rename = "type")]
    pub cluster_type: String,

    /// Unique identifier for this cluster
    pub id: String,

    /// Name of the cluster (form name, aria-label, etc.)
    #[serde(default)]
    pub name: Option<String>,

    /// Number of elements in this cluster
    pub element_count: usize,

    /// IDs of elements in this cluster
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub element_ids: Vec<usize>,

    /// Form action URL (for form clusters only)
    #[serde(default)]
    pub action: Option<String>,
}

// =============================================================================
// SOTA Phase 13: Semantic Breadcrumbs (Flow Context)
// =============================================================================

/// Flow context for multi-step workflows.
/// Provides navigation context to help the LLM understand where the user is
/// in a multi-step process (checkout, wizards, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowContext {
    /// Breadcrumb navigation trail (e.g., "Home > Products > Shoes")
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breadcrumbs: Vec<Breadcrumb>,

    /// Currently active tab (if in a tabbed interface)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab: Option<ActiveTab>,

    /// Step indicator for multi-step workflows
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_indicator: Option<StepIndicator>,

    /// Human-readable summary of flow position
    /// e.g., "Path: Home > Checkout | Step 2 of 4"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_position: Option<String>,
}

/// A single breadcrumb in the navigation trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breadcrumb {
    /// Breadcrumb text (e.g., "Products")
    pub text: String,

    /// Whether this is the current/active breadcrumb
    #[serde(default)]
    pub is_current: bool,

    /// Link URL (if available)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// Active tab information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTab {
    /// Tab label text
    pub text: String,

    /// 1-based index of the active tab
    pub index: usize,

    /// Total number of tabs
    pub total_tabs: usize,
}

/// Step indicator for multi-step workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepIndicator {
    /// Current step number (1-based)
    pub current_step: usize,

    /// Total number of steps
    pub total_steps: usize,

    /// Label of the current step (e.g., "Payment")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_label: Option<String>,
}

// =============================================================================
// SOTA Phase 2: Network & Console Error Integration
// =============================================================================

/// Network and console error context captured from CDP.
///
/// Provides visibility into:
/// - HTTP error responses (4xx, 5xx)
/// - Failed network requests (CORS, DNS, connection errors)
/// - JavaScript console errors and warnings
/// - Uncaught exceptions
///
/// Error pattern detection:
/// - Rate limiting (429)
/// - Authentication errors (401, 403)
/// - CORS issues
/// - Server errors (5xx)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct NetworkContext {
    /// Network errors (HTTP 4xx/5xx, failed requests)
    #[serde(default)]
    pub network_errors: Vec<NetworkError>,

    /// Console errors and warnings
    #[serde(default)]
    pub console_errors: Vec<ConsoleError>,

    /// Summary of error patterns detected
    #[serde(default)]
    pub error_summary: NetworkErrorSummary,
}

/// A single network error (HTTP error or failed request).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct NetworkError {
    /// Request URL (may be None for failed requests)
    #[serde(default)]
    pub url: Option<String>,

    /// HTTP status code (0 for failed requests)
    #[serde(default)]
    pub status: u16,

    /// HTTP status text
    #[serde(default)]
    pub status_text: Option<String>,

    /// Error text (for failed requests: CORS, DNS, connection errors)
    #[serde(default)]
    pub error_text: Option<String>,

    /// Resource type (XHR, Fetch, Document, Script, etc.)
    #[serde(default)]
    pub resource_type: Option<String>,

    /// Timestamp when error was captured (milliseconds since epoch)
    #[serde(default)]
    pub timestamp: u64,
}

/// A console error or warning message.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct ConsoleError {
    /// Error level (error, warning)
    #[serde(default)]
    pub level: String,

    /// Error message text (truncated to 500 chars)
    #[serde(default)]
    pub text: String,

    /// Source URL where error occurred
    #[serde(default)]
    pub url: Option<String>,

    /// Line number in source file
    #[serde(default)]
    pub line_number: Option<u32>,

    /// Timestamp when error was captured (milliseconds since epoch)
    #[serde(default)]
    pub timestamp: u64,
}

/// Summary of detected error patterns for quick LLM reference.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct NetworkErrorSummary {
    /// Total number of network errors captured
    #[serde(default)]
    pub total_network_errors: usize,

    /// Total number of console errors (level=error)
    #[serde(default)]
    pub total_console_errors: usize,

    /// Total number of console warnings (level=warning)
    #[serde(default)]
    pub total_console_warnings: usize,

    /// Whether rate limiting was detected (HTTP 429)
    #[serde(default)]
    pub has_rate_limiting: bool,

    /// Whether auth errors were detected (HTTP 401 or 403)
    #[serde(default)]
    pub has_auth_errors: bool,

    /// Whether CORS errors were detected
    #[serde(default)]
    pub has_cors_errors: bool,

    /// Whether server errors were detected (HTTP 5xx)
    #[serde(default)]
    pub has_server_errors: bool,
}

// ============================================================================
// PHASE 4: Bot Detection Context
// ============================================================================

/// Bot detection context - signals about honeypots, CAPTCHAs, rate limits, etc.
/// Helps the LLM understand when a page has anti-automation measures.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct BotDetectionContext {
    /// Detected bot detection signals (honeypots, CAPTCHAs, etc.)
    #[serde(default)]
    pub signals: Vec<BotDetectionSignal>,

    /// Quick warning list for LLM reference
    #[serde(default)]
    pub warnings: Vec<String>,

    /// Summary of detection results
    #[serde(default)]
    pub summary: BotDetectionSummary,

    /// Timestamp when detection was performed
    #[serde(default)]
    pub detected_at: u64,
}

/// A single bot detection signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotDetectionSignal {
    /// Honeypot fields detected in forms
    HoneypotDetected {
        count: usize,
        fields: Vec<HoneypotField>,
        warning: String,
    },
    /// CAPTCHA services detected on page
    CaptchaDetected {
        captchas: Vec<CaptchaInfo>,
        warning: String,
    },
    /// Rate limiting / blocking messages detected
    RateLimitIndicator {
        phrases: Vec<String>,
        warning: String,
    },
    /// Challenge page detected (Cloudflare, etc.)
    ChallengePage {
        indicators: Vec<String>,
        warning: String,
    },
    /// Anti-automation scripts detected
    AntiAutomationScripts {
        services: Vec<String>,
        warning: String,
    },
}

/// Information about a detected honeypot field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct HoneypotField {
    /// CSS selector for the honeypot field (if available)
    #[serde(default)]
    pub selector: Option<String>,

    /// Field name or ID
    #[serde(default)]
    pub name: String,

    /// Index of the form containing this field
    #[serde(default)]
    pub form_index: usize,

    /// Confidence level (high, medium, low)
    #[serde(default)]
    pub confidence: String,
}

/// Information about a detected CAPTCHA.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct CaptchaInfo {
    /// CAPTCHA type/provider (recaptcha, hcaptcha, turnstile, etc.)
    #[serde(default, rename = "type")]
    pub captcha_type: String,

    /// Source URL (for iframe-based CAPTCHAs)
    #[serde(default)]
    pub src: Option<String>,

    /// CSS selector (for container-based detection)
    #[serde(default)]
    pub selector: Option<String>,

    /// Whether the CAPTCHA is currently visible
    #[serde(default)]
    pub visible: bool,
}

/// Summary of bot detection results.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct BotDetectionSummary {
    /// Number of honeypot fields detected
    #[serde(default)]
    pub honeypot_count: usize,

    /// Number of CAPTCHA elements detected
    #[serde(default)]
    pub captcha_count: usize,

    /// Whether rate limiting messages were found
    #[serde(default)]
    pub has_rate_limit_warning: bool,

    /// Whether a challenge page was detected
    #[serde(default)]
    pub has_challenge_page: bool,

    /// Number of anti-automation services detected
    #[serde(default)]
    pub anti_automation_services: usize,

    /// Overall risk level: "none", "low", "medium", "high", "unknown"
    #[serde(default)]
    pub risk_level: String,

    /// Error message if detection failed
    #[serde(default)]
    pub error: Option<String>,
}

// ============================================================================
// PHASE 5: Temporal Context (3-Frame Analysis)
// ============================================================================

/// Temporal context from 3-frame strip analysis.
/// Captures page state changes over ~1 second to detect animations, loading, and transitions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct TemporalContext {
    /// Detected temporal patterns (spinner, loading_bar, stable, etc.)
    #[serde(default)]
    pub patterns: Vec<TemporalPattern>,

    /// Overall page stability assessment
    #[serde(default)]
    pub stability: PageStability,

    /// Percentage of pixels that changed between frames (0.0-1.0)
    #[serde(default)]
    pub change_ratio: f64,

    /// Regions with detected motion (bounding boxes)
    #[serde(default)]
    pub motion_regions: Vec<MotionRegion>,

    /// Summary for LLM consumption
    #[serde(default)]
    pub summary: String,

    /// Guidance for the agent based on temporal analysis
    #[serde(default)]
    pub guidance: Option<String>,

    /// Timestamps of the 3 captured frames (ms since epoch)
    #[serde(default)]
    pub frame_timestamps: Vec<u64>,

    /// Error if temporal capture failed
    #[serde(default)]
    pub error: Option<String>,
}

/// Detected temporal pattern in the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TemporalPattern {
    /// Rotating/spinning element detected (loading spinner)
    Spinner {
        /// Approximate location on page
        region: Option<MotionRegion>,
        /// Confidence level (0.0-1.0)
        confidence: f64,
    },
    /// Progress bar detected (width changing)
    ProgressBar {
        /// Current progress percentage (0-100)
        progress: Option<u8>,
        /// Whether progress is advancing
        is_advancing: bool,
    },
    /// Fade animation detected (opacity changing)
    FadeAnimation {
        /// Whether fading in or out
        direction: String,
        /// Target element description
        target: Option<String>,
    },
    /// Content is loading (skeleton/placeholder visible)
    LoadingContent {
        /// Number of skeleton elements detected
        skeleton_count: usize,
    },
    /// Modal/overlay is opening or closing
    ModalTransition {
        /// "opening" or "closing"
        direction: String,
    },
    /// Page is completely stable (no changes detected)
    Stable,
    /// Page has constant motion (video, canvas animation)
    ContinuousMotion {
        /// Type of motion source
        source: String,
    },
}

/// Page stability assessment.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PageStability {
    /// Page is stable, safe to interact
    #[default]
    Stable,
    /// Page is loading, wait before interacting
    Loading,
    /// Page has active animations, may need to wait
    Animating,
    /// Page appears stuck (loading for too long)
    Stuck,
    /// Unknown state
    Unknown,
}

/// Region with detected motion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MotionRegion {
    /// Bounding box of the motion region
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Intensity of motion (0.0-1.0)
    pub intensity: f64,
    /// Description of what's in this region
    pub description: Option<String>,
}

/// Statistics about cross-origin iframe element extraction (SOTA Phase 0).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrossOriginExtractionStats {
    /// Number of cross-origin frames processed
    #[serde(default)]
    pub frames_processed: usize,

    /// Number of frames that successfully yielded elements
    #[serde(default)]
    pub frames_successful: usize,

    /// Total elements extracted from all cross-origin frames
    #[serde(default)]
    pub total_elements: usize,

    /// Per-frame details (optional, for debugging)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_details: Vec<CrossOriginFrameDetail>,
}

/// Detail about a single cross-origin frame extraction attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrossOriginFrameDetail {
    /// Origin of the iframe (e.g., "https://js.stripe.com")
    #[serde(default)]
    pub origin: String,

    /// Full URL of the iframe
    #[serde(default)]
    pub href: Option<String>,

    /// Whether extraction succeeded
    #[serde(default)]
    pub extracted: bool,

    /// Number of elements extracted (if successful)
    #[serde(default)]
    pub element_count: usize,

    /// Total interactive elements found (before limits)
    #[serde(default)]
    pub total_found: usize,

    /// Failure reason (if not extracted)
    #[serde(default)]
    pub reason: Option<String>,

    /// Phase 0.5: Occlusion info
    #[serde(default)]
    pub occlusion: Option<CrossOriginOcclusionInfo>,
}

/// Occlusion info for a cross-origin frame (Phase 0.5).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrossOriginOcclusionInfo {
    /// Number of elements occluded by parent frame overlays
    #[serde(default)]
    pub occluded_count: usize,

    /// Whether the parent frame has an active modal/overlay
    #[serde(default)]
    pub has_active_modal: bool,

    /// Whether the iframe was found in the parent frame
    #[serde(default)]
    pub iframe_found: bool,
}

/// Information about a cross-origin iframe detected on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossOriginFrameInfo {
    /// DOM index of the iframe (from parent document order)
    #[serde(default)]
    pub index: Option<usize>,

    /// Unique frame ID (UUID)
    #[serde(default)]
    pub frame_id: Option<String>,

    /// Chrome internal frame ID
    #[serde(default)]
    pub chrome_frame_id: Option<i64>,

    /// Origin of the iframe
    pub origin: String,

    /// Full URL of the iframe
    #[serde(default)]
    pub href: Option<String>,

    /// Bounding rect of the iframe in parent viewport coordinates
    #[serde(default)]
    pub iframe_rect: Option<SoMBoundingRect>,

    /// Whether elements were successfully extracted from this frame
    #[serde(default)]
    pub elements_extracted: bool,
}

/// Broad kind of spatial work surface detected during observation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpatialSurfaceKind {
    Canvas,
    Svg,
    DomSpatial,
    WebGl,
    WebGpu,
    #[default]
    Unknown,
}

impl SpatialSurfaceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Svg => "svg",
            Self::DomSpatial => "dom_spatial",
            Self::WebGl => "webgl",
            Self::WebGpu => "webgpu",
            Self::Unknown => "unknown",
        }
    }
}

/// Rendering stack hint for a detected spatial surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SurfaceRenderingKind {
    #[serde(rename = "canvas_2d", alias = "canvas2d")]
    Canvas2d,
    #[serde(rename = "svg")]
    Svg,
    #[serde(rename = "webgl", alias = "web_gl")]
    WebGl,
    #[serde(rename = "webgpu", alias = "web_gpu")]
    WebGpu,
    #[serde(rename = "dom_custom")]
    DomCustom,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

impl SurfaceRenderingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Canvas2d => "canvas_2d",
            Self::Svg => "svg",
            Self::WebGl => "webgl",
            Self::WebGpu => "webgpu",
            Self::DomCustom => "dom_custom",
            Self::Unknown => "unknown",
        }
    }
}

/// Frame context attached to a detected spatial surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SurfaceFrameContext {
    #[default]
    MainDocument,
    SameOriginIframe {
        #[serde(default)]
        iframe_selector: Option<String>,
        #[serde(default)]
        frame_id: Option<String>,
        #[serde(default)]
        iframe_index: Option<usize>,
        #[serde(default)]
        frame_path: Option<Vec<usize>>,
    },
    CrossOriginIframe {
        #[serde(default)]
        frame_target_id: Option<String>,
        #[serde(default)]
        origin: Option<String>,
        #[serde(default)]
        href: Option<String>,
        #[serde(default)]
        iframe_index: Option<usize>,
        #[serde(default)]
        frame_path: Option<Vec<usize>>,
    },
}

impl SurfaceFrameContext {
    pub fn is_main_document(&self) -> bool {
        matches!(self, Self::MainDocument)
    }
}

/// Mode and toolbar state hints near a spatial surface.
///
/// All fields are optional because observation should prefer "unknown" over
/// inventing editor state that is not clearly surfaced in surrounding chrome.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceModeState {
    #[serde(default)]
    pub active_tool: Option<String>,
    #[serde(default)]
    pub zoom_percent: Option<f64>,
    #[serde(default)]
    pub pan_mode_active: Option<bool>,
    #[serde(default)]
    pub selection_present: Option<bool>,
    #[serde(default)]
    pub selected_object_count: Option<usize>,
    #[serde(default)]
    pub snap_enabled: Option<bool>,
    #[serde(default)]
    pub grid_visible: Option<bool>,
    #[serde(default)]
    pub layer_hint: Option<String>,
}

/// Spatial work surface detected during observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpatialSurfaceInfo {
    pub id: String,
    pub surface_kind: SpatialSurfaceKind,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub frame_context: SurfaceFrameContext,
    pub rect_css: SoMBoundingRect,
    pub rect_scaled: SoMBoundingRect,
    #[serde(default)]
    pub z_index: i32,
    #[serde(default)]
    pub area_ratio: f64,
    #[serde(default)]
    pub likely_primary: bool,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub occluded: bool,
    #[serde(default)]
    pub same_origin_access: bool,
    #[serde(default)]
    pub rendering_kind: SurfaceRenderingKind,
    #[serde(default)]
    pub can_read_pixels: Option<bool>,
    #[serde(default)]
    pub has_pointer_listeners: Option<bool>,
    #[serde(default)]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub mode_state: Option<SurfaceModeState>,
}

/// Minimal detection quality signal for LLM consumption.
///
/// Full DetectionStats are kept on PageState for telemetry and agentic logic,
/// but only this derived signal should be surfaced to the LLM to avoid prompt
/// noise. The code should decide fallback behavior (vision, re-observe, ask user),
/// not the LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionQuality {
    /// Normal detection - elements found via standard methods
    Normal,
    /// Low quality - no instrumentation, relying on AX tree fallback
    Low,
    /// Empty - no interactive elements detected at all
    Empty,
    /// Degraded - instrumentation unavailable, may miss styled divs with handlers
    Degraded,
}

impl DetectionQuality {
    pub fn as_str(&self) -> &'static str {
        match self {
            DetectionQuality::Normal => "normal",
            DetectionQuality::Low => "low",
            DetectionQuality::Empty => "empty",
            DetectionQuality::Degraded => "degraded",
        }
    }
}

impl DetectionStats {
    /// Derive a minimal quality signal suitable for LLM context.
    ///
    /// Returns a simple enum that the LLM can use to adjust behavior,
    /// without exposing noisy counters that might mislead the model.
    pub fn quality(&self) -> DetectionQuality {
        if self.total_elements == 0 {
            return DetectionQuality::Empty;
        }

        // If we're relying primarily on AX tree (>50% of elements), quality is low
        if self.via_ax_tree > 0 && self.via_ax_tree > (self.via_selector + self.via_event_listener)
        {
            return DetectionQuality::Low;
        }

        // If instrumentation wasn't available, we might miss styled divs with handlers
        if !self.instrumentation_available && self.via_event_listener == 0 {
            return DetectionQuality::Degraded;
        }

        DetectionQuality::Normal
    }

    /// Returns true if detection found no interactive elements.
    /// This is a strong signal that vision-based detection might be needed.
    pub fn is_empty(&self) -> bool {
        self.total_elements == 0
    }

    /// Returns true if detection is degraded (missing instrumentation).
    /// Code may want to fall back to vision or re-observe with different settings.
    pub fn is_degraded(&self) -> bool {
        !self.instrumentation_available && self.via_event_listener == 0
    }

    /// Returns true if we're primarily relying on AX tree for detection.
    /// This indicates DOM-based detection may have missed elements.
    pub fn is_ax_tree_dominant(&self) -> bool {
        self.via_ax_tree > 0 && self.via_ax_tree > (self.via_selector + self.via_event_listener)
    }
}

pub fn compute_screenshot_hash(screenshot_data: Option<&str>) -> Option<u64> {
    let data = screenshot_data?;
    if data.is_empty() {
        return None;
    }
    let hash = blake3::hash(data.as_bytes());
    let bytes: [u8; 8] = hash.as_bytes()[..8]
        .try_into()
        .expect("blake3 hash must be at least 8 bytes");
    Some(u64::from_le_bytes(bytes))
}

impl PageState {
    pub fn spatial_surfaces(&self) -> &[SpatialSurfaceInfo] {
        self.spatial_surfaces.as_deref().unwrap_or(&[])
    }

    pub fn primary_spatial_surface(&self) -> Option<&SpatialSurfaceInfo> {
        self.spatial_surfaces()
            .iter()
            .find(|surface| surface.likely_primary)
            .or_else(|| self.spatial_surfaces().first())
    }

    /// O(1) structural comparison - ignores content changes like typing
    /// Use for recovery polling (waiting for page transition)
    ///
    /// Falls back to PageStage/URL comparison if Merkle hashes not available.
    pub fn structure_changed(&self, other: &PageState) -> bool {
        match (&self.merkle_structural_root, &other.merkle_structural_root) {
            (Some(a), Some(b)) => a != b,
            _ => self.current_stage != other.current_stage || self.url != other.url,
        }
    }

    /// O(1) full comparison - includes all content changes
    /// Use for validation (did form fill work?)
    ///
    /// Falls back to PageStage/URL comparison if Merkle hashes not available.
    pub fn content_changed(&self, other: &PageState) -> bool {
        match (&self.merkle_content_root, &other.merkle_content_root) {
            (Some(a), Some(b)) => a != b,
            _ => self.current_stage != other.current_stage || self.url != other.url,
        }
    }

    /// Build and attach Merkle tree with DOM-first strategy.
    ///
    /// Uses DOM HTML for 100% coverage when available, falls back to accessibility tree
    /// (~10-50% coverage) when DOM is not provided. Call this after setting the relevant
    /// data sources to enable O(1) change detection.
    ///
    /// # Arguments
    /// * `dom_html` - Optional DOM HTML snapshot (provides 100% coverage)
    ///
    /// # Coverage
    /// - With `dom_html`: 100% of page elements
    /// - Without `dom_html`: ~10-50% (accessibility tree only)
    pub fn build_merkle_tree(&mut self, dom_html: Option<&str>) {
        // Priority: CDP DOM tree (100% + iframes/shadow) > DOM snapshot (100%) > Accessibility tree (~10-50%)
        let merkle_tree = if let Some(cdp_capture) = &self.cdp_dom_tree {
            // CDP-first: 100% coverage including cross-origin iframes and closed shadow DOM
            // Extract the actual tree from the wrapper struct
            PageMerkleTree::from_cdp_dom_tree(
                &cdp_capture.tree,
                self.url.as_deref(),
                self.current_stage,
            )
        } else if let Some(html) = dom_html {
            // DOM snapshot: 100% main document coverage, merge accessibility roles when available
            PageMerkleTree::from_dom_snapshot(
                html,
                self.accessibility_tree.as_ref(),
                self.url.as_deref(),
                self.current_stage,
            )
        } else if let Some(tree_json) = &self.accessibility_tree {
            // Fallback: accessibility tree only (~10-50% coverage)
            PageMerkleTree::from_accessibility_tree(
                tree_json,
                self.url.as_deref(),
                self.current_stage,
            )
        } else {
            // No data sources available
            return;
        };

        self.merkle_structural_root = Some(merkle_tree.root_structural_hash.clone());
        self.merkle_content_root = Some(merkle_tree.root_hash.clone());
        self.merkle_tree = Some(merkle_tree);
    }

    /// Get detection quality signal suitable for LLM context.
    ///
    /// Returns a minimal derived signal that can be included in prompts
    /// without adding noise from full detection stats.
    pub fn detection_quality(&self) -> DetectionQuality {
        self.detection_stats
            .as_ref()
            .map(|s| s.quality())
            .unwrap_or(DetectionQuality::Normal)
    }

    /// Check if element detection found nothing.
    /// Strong signal to fall back to vision-based detection.
    pub fn has_empty_detection(&self) -> bool {
        self.detection_stats
            .as_ref()
            .map(|s| s.is_empty())
            .unwrap_or(false)
    }

    /// Check if detection is degraded (missing instrumentation).
    /// May want to fall back to vision or re-observe.
    pub fn has_degraded_detection(&self) -> bool {
        self.detection_stats
            .as_ref()
            .map(|s| s.is_degraded())
            .unwrap_or(false)
    }
}

/// Element mapping between vision and DOM.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MappedElement {
    pub visual_description: String,
    pub selector: String,
    pub selector_hint: Option<String>,
    pub element_type: Option<String>,
    pub suggested_action: Option<String>,
    pub confidence: f64,
    pub attributes: HashMap<String, String>,
}

/// Suggested action produced by page understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedAction {
    pub label: String,
    pub action_type: ActionType,
    pub selector: Option<String>,
    pub reasoning: Option<String>,
    pub confidence: f64,
}

impl Default for SuggestedAction {
    fn default() -> Self {
        Self {
            label: String::new(),
            action_type: ActionType::Custom("unknown".to_string()),
            selector: None,
            reasoning: None,
            confidence: 0.0,
        }
    }
}

/// Coarse-grained classification of suggested actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Navigate,
    Click,
    Type,
    Scroll,
    Extract,
    Wait,
    Custom(String),
}

impl Default for ActionType {
    fn default() -> Self {
        ActionType::Custom("unknown".into())
    }
}

/// High-level stage inferred for the current page.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PageStage {
    #[default]
    Unknown,
    Login,
    Dashboard,
    Search,
    Results,
    Form,
    Modal,
    Content,
    Error,
    Loading,
}

/// Document readiness.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PageReadyState {
    Loading,
    Interactive,
    Complete,
    #[default]
    Unknown,
}

// ===========================================================================================
// Set-of-Mark (SoM) Types for Visual Grounding
// ===========================================================================================

// === SOTA Phase 3: Selector Self-Healing Types ===

/// A selector alternative with stability score for self-healing (SOTA Phase 3).
///
/// Multiple alternatives are ranked by stability (higher = more reliable):
/// - data-testid: 99% (best - explicitly for testing)
/// - id: 95% (good but can change)
/// - aria-label: 90% (semantic, usually stable)
/// - name: 85% (form elements)
/// - class: 70% (can change with styling)
/// - nth-child: 50% (fragile, position-based)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct SelectorAlternative {
    /// CSS selector string
    pub selector: String,

    /// Stability score (0-100, higher = more stable)
    pub stability: u8,

    /// Type of selector (e.g., "data-testid", "id", "aria-label", "class", "nth-child")
    #[serde(rename = "type")]
    pub selector_type: String,
}

/// Closest "named" enclosing ancestor of an interactive element. Captured
/// by observe.js's `findNamedAncestor` and surfaced in observations to give
/// the LLM structural context for grouped UIs (forms, panels, list items,
/// test cards, etc.).
///
/// `kind` describes which signal we matched on: "id", "aria-label",
/// "role=region", "section", "article", "aside", "nav", or "form".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NamedAncestor {
    pub selector: String,
    pub name: String,
    pub kind: String,
}

/// Interactive element with bounding box for SoM annotation.
///
/// This struct receives data from Magicutor's `extractInteractiveElements` function.
/// Uses `#[serde(rename_all = "camelCase")]` to map JavaScript camelCase to Rust snake_case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoMInteractiveElement {
    pub id: usize,
    pub selector: String,
    pub tag: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub backend_node_id: Option<u64>,
    /// CSS-pixel bounding box (viewport-relative)
    pub rect: SoMBoundingRect,
    /// Screenshot-pixel bounding box (DPR-scaled)
    pub rect_scaled: SoMBoundingRect,
    #[serde(default)]
    pub z_index: i32,
    #[serde(default)]
    pub is_in_overlay: bool,
    #[serde(default)]
    pub overlay_id: Option<usize>,
    /// Closest "named" enclosing ancestor — gives the LLM structural context
    /// (e.g. "[42] in #test-4 'Misleading drag…'"). Captured by observe.js.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_ancestor: Option<NamedAncestor>,
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, Option<String>>,
    /// Element context (main, iframe, shadow) - Phase 2
    #[serde(default)]
    pub context: ElementContext,
    /// Whether this element's children were merged into its text (icon+text → single mark)
    #[serde(default)]
    pub merged_children: bool,
    /// How this element was detected:
    /// - "selector": Standard DOM query (tags, ARIA roles, attributes)
    /// - "event_listener": Via content script instrumentation (addEventListener interception)
    /// - "ax_tree": Via CDP Accessibility Tree cross-reference
    #[serde(default)]
    pub detected_via: Option<String>,

    // SOTA 4: Clickability validation fields
    /// Whether the element is actually clickable (passed validation)
    #[serde(default, rename = "isClickable")]
    pub is_clickable: bool,

    /// Reason why element is not clickable (if is_clickable is false)
    /// Values: "pointer-events-none", "disabled", "aria-disabled", "inert-subtree", "occluded"
    #[serde(default, rename = "clickabilityReason")]
    pub clickability_reason: Option<String>,

    /// Selector of the element occluding this one (if clickability_reason is "occluded")
    #[serde(default, rename = "occludedBy")]
    pub occluded_by: Option<String>,

    /// Whether the element center is outside the current viewport
    #[serde(default, rename = "outsideViewport")]
    pub outside_viewport: bool,

    // SOTA Phase 0.5: Parent-frame occlusion
    /// Whether this element (in a cross-origin iframe) is occluded by a parent-frame overlay
    /// This is detected by checking elementFromPoint from the parent frame's perspective.
    #[serde(default, rename = "isOccludedByParent")]
    pub is_occluded_by_parent: bool,

    // SOTA Phase 1: Priority and viewport visibility
    /// Element priority score (40-100, higher = more important).
    /// Tiers: submit buttons (100), primary CTAs (98), primary-styled (90),
    /// focused (85), required inputs (80), text inputs (75), links (65), buttons (60), etc.
    #[serde(default)]
    pub priority: u8,

    /// Whether the element's center is fully within the viewport.
    #[serde(default, rename = "inViewport")]
    pub in_viewport: bool,

    /// Viewport visibility level:
    /// - 0: Not in viewport (scrolled out)
    /// - 1: Partially in viewport (edge visible)
    /// - 2: Fully in viewport (center visible)
    #[serde(default, rename = "viewportVisibility")]
    pub viewport_visibility: u8,

    // SOTA Phase 3: Selector Self-Healing
    /// Alternative selectors ranked by stability score.
    /// Used for automatic fallback when primary selector fails.
    /// Each alternative includes stability % and selector type.
    #[serde(default, rename = "selectorChain")]
    pub selector_chain: Vec<SelectorAlternative>,

    // STABLE SoM: Stable element fingerprint for selector-first resolution
    /// Stable fingerprint that survives across observations.
    /// Priority: #id > [data-testid] > [data-cy] > [aria-label] > composite DOM path.
    /// Used for reliable element resolution at execution time.
    #[serde(default)]
    pub fingerprint: Option<String>,

    // SCROLL OPTIMIZATION: Nested scroll container detection
    /// Whether this element is inside a scrollable container (overflow: auto|scroll).
    /// When true, the scroll distance hint (rect.y) is approximate since it's viewport-relative,
    /// not container-relative. scrollIntoView() still works correctly.
    #[serde(default, rename = "inScrollContainer")]
    pub in_scroll_container: bool,

    /// CSS selector of the nearest scrollable ancestor container (always populated when in_scroll_container is true).
    /// Unlike scroll_clip_container_selector which is only set when the element is clipped,
    /// this field reports the container even when both container and child are off-screen together.
    #[serde(default, rename = "scrollContainerSelector")]
    pub scroll_container_selector: Option<String>,

    /// How well the scroll container itself is positioned in the main viewport (0.0-1.0).
    /// When < 0.5 and container is reasonably sized, scrolling the main page first may help.
    #[serde(default, rename = "scrollContainerVisibilityRatio")]
    pub scroll_container_visibility_ratio: Option<f64>,

    /// Hint suggesting scrolling the main page to reveal more of the scroll container.
    /// e.g., "scroll page down ~300px to reveal more"
    /// Only set when container is poorly positioned (< 50% visible) and not huge.
    #[serde(default, rename = "scrollContainerViewportHint")]
    pub scroll_container_viewport_hint: Option<String>,

    /// Whether the scroll container supports vertical scrolling (overflow-y: auto|scroll with scrollHeight > clientHeight).
    /// Critical for choosing correct scroll strategy - e.g., kanban board might only scroll horizontally.
    #[serde(default, rename = "scrollContainerCanScrollY")]
    pub scroll_container_can_scroll_y: Option<bool>,

    /// Whether the scroll container supports horizontal scrolling (overflow-x: auto|scroll with scrollWidth > clientWidth).
    /// Critical for choosing correct scroll strategy - e.g., data table column might only scroll vertically.
    #[serde(default, rename = "scrollContainerCanScrollX")]
    pub scroll_container_can_scroll_x: Option<bool>,

    // SCROLL CONTAINER CLIPPING: Info about clipping by scroll containers
    // Helps LLM decide between window scroll vs nested container scroll
    /// CSS selector of the scroll container that clips this element (if clipped).
    #[serde(default, rename = "scrollClipContainerSelector")]
    pub scroll_clip_container_selector: Option<String>,

    /// Direction this element is clipped relative to the scroll container.
    /// Values: "above_in_container", "below_in_container", "left_in_container", "right_in_container"
    #[serde(default, rename = "scrollClipDirection")]
    pub scroll_clip_direction: Option<String>,

    /// Approximate pixels needed to scroll to bring element into view within container.
    #[serde(default, rename = "scrollClipDistance")]
    pub scroll_clip_distance: Option<i32>,

    /// Whether the scroll container can actually be scrolled (has scroll overflow).
    /// False if overflow:hidden (clips but can't scroll interactively).
    #[serde(default, rename = "scrollClipCanScroll")]
    pub scroll_clip_can_scroll: Option<bool>,

    // SAME-ORIGIN IFRAME: Explicit fields for iframe element resolution
    // These replace the old >> string encoding in selectors.
    /// Whether this element is in a same-origin iframe (can access contentDocument).
    /// When true, use iframe_selector + selector for element resolution.
    #[serde(default, rename = "isSameOriginIframe")]
    pub is_same_origin_iframe: bool,

    /// CSS selector of the iframe container (for same-origin iframes).
    /// Used with contentDocument for element resolution: iframe.contentDocument.querySelector(selector)
    #[serde(default, rename = "iframeSelector")]
    pub iframe_selector_direct: Option<String>,

    /// Framework detection hint for whether JS evaluate is likely to work.
    /// Values: "native" (JS works), "react"/"vue"/"angular" (use keyboard instead),
    /// "custom-element" (Web Component), "shadow" (in shadow DOM).
    #[serde(default, rename = "jsEvaluateHint")]
    pub js_evaluate_hint: Option<String>,

    /// Spatial surface that appears to own this element's interaction region.
    /// Set when a dominant surface geometrically covers the element.
    #[serde(default, rename = "surfaceOwnerId")]
    pub surface_owner_id: Option<String>,

    /// Kind of the owning spatial surface ("canvas", "svg", "webgl", ...).
    #[serde(default, rename = "surfaceOwnerKind")]
    pub surface_owner_kind: Option<String>,

    /// Fraction of the element area overlapped by the owning surface.
    #[serde(default, rename = "surfaceOverlapRatio")]
    pub surface_overlap_ratio: Option<f64>,

    /// Whether the element center lies inside the owning surface rect.
    #[serde(default, rename = "surfaceCenterInside")]
    pub surface_center_inside: Option<bool>,

    /// Confidence score for the ownership binding (0.0-1.0).
    #[serde(default, rename = "surfaceOwnershipConfidence")]
    pub surface_ownership_confidence: Option<f64>,

    // TEXT ELEMENT COLLECTION: Non-interactive text elements
    /// Whether this is a non-interactive text element collected for LLM context.
    /// Text elements provide content (emails, names, labels) that the LLM needs
    /// to correlate with nearby interactive elements (e.g., "click Edit next to john@email.com").
    #[serde(default, rename = "isTextElement")]
    pub is_text_element: bool,
}

/// Bounding rectangle (in CSS or physical pixels).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct SoMBoundingRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Bounding rectangle for an iframe in parent viewport coordinates (SOTA Phase 0).
/// Simpler version of SoMBoundingRect for cross-origin iframe context.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct IframeBoundingRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SoMBoundingRect {
    /// Create a new bounding rect
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Get center point for coordinate-based click fallback
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Check if the rect has positive dimensions
    pub fn is_valid(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
}

/// Detected overlay/modal container.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoMOverlayContainer {
    pub id: usize,
    pub selector: String,
    pub rect: SoMBoundingRect,
    #[serde(default)]
    pub z_index: i32,
    #[serde(default)]
    pub overlay_type: String,
    #[serde(default)]
    pub has_close_button: bool,
    #[serde(default)]
    pub close_button_id: Option<usize>,
}

/// Scroll offset at time of capture.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct ScrollOffset {
    pub x: f64,
    pub y: f64,
}

/// Viewport size at time of capture.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct ViewportSize {
    pub width: u32,
    pub height: u32,
}

/// Document size (total scrollable area).
/// Used to determine if there's more content below/above the current viewport.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct DocumentSize {
    pub width: u32,
    pub height: u32,
}

/// Scroll context combining scroll position, viewport, and document size.
/// Used to help the LLM understand where on the page we are and if there's more content.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScrollContext {
    /// Current scroll position
    pub scroll_offset: ScrollOffset,
    /// Viewport dimensions
    pub viewport_size: ViewportSize,
    /// Total document size (scrollable area)
    pub document_size: DocumentSize,
    /// Computed: percentage scrolled through document (0-100)
    pub scroll_percentage: f64,
    /// Computed: whether there's more content below
    pub has_content_below: bool,
    /// Computed: whether there's more content above
    pub has_content_above: bool,
    /// Computed: pixels of content remaining below viewport
    pub pixels_below: f64,
    /// Computed: pixels of content above viewport (scroll offset)
    pub pixels_above: f64,
}

impl ScrollContext {
    /// Create a new ScrollContext and compute derived fields
    pub fn new(
        scroll_offset: ScrollOffset,
        viewport_size: ViewportSize,
        document_size: DocumentSize,
    ) -> Self {
        let scroll_y = scroll_offset.y;
        let viewport_height = viewport_size.height as f64;
        let doc_height = document_size.height as f64;

        // Calculate scroll percentage
        let max_scroll = (doc_height - viewport_height).max(0.0);
        let scroll_percentage = if max_scroll > 0.0 {
            ((scroll_y / max_scroll) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        // Determine if there's more content
        let has_content_above = scroll_y > 10.0; // 10px threshold
        let has_content_below = (scroll_y + viewport_height + 10.0) < doc_height;

        // Calculate pixel distances for smarter scrolling
        // pixels_above: how far we've scrolled from the top
        let pixels_above = scroll_y;
        // pixels_below: remaining content below current viewport
        let pixels_below = (doc_height - scroll_y - viewport_height).max(0.0);

        Self {
            scroll_offset,
            viewport_size,
            document_size,
            scroll_percentage,
            has_content_below,
            has_content_above,
            pixels_below,
            pixels_above,
        }
    }

    /// Format scroll context for LLM prompt
    pub fn to_llm_description(&self) -> String {
        let mut lines = Vec::new();

        // Position summary
        let position = if self.scroll_percentage < 5.0 {
            "at the TOP of the page"
        } else if self.scroll_percentage > 95.0 {
            "at the BOTTOM of the page"
        } else {
            "in the MIDDLE of the page"
        };

        lines.push(format!(
            "📍 Scroll Position: {} ({:.0}% scrolled)",
            position, self.scroll_percentage
        ));

        // Viewport info
        lines.push(format!(
            "📐 Viewport: {}x{} px | Document: {}x{} px",
            self.viewport_size.width,
            self.viewport_size.height,
            self.document_size.width,
            self.document_size.height
        ));

        // Content indicators with pixel distances for smart scrolling
        if self.has_content_above {
            lines.push(format!(
                "⬆️ Content above: {:.0}px (use scroll(Up, {:.0}) to reach top)",
                self.pixels_above,
                self.pixels_above.min(self.viewport_size.height as f64) // Don't suggest scrolling more than viewport
            ));
        }
        if self.has_content_below {
            lines.push(format!(
                "⬇️ Content below: {:.0}px (use scroll(Down, {:.0}) or scroll with a target element)",
                self.pixels_below,
                self.pixels_below.min(self.viewport_size.height as f64) // Don't suggest scrolling more than viewport
            ));
        }
        if !self.has_content_above && !self.has_content_below {
            lines.push("📄 All content visible (no scrolling needed)".to_string());
        }

        lines.join("\n")
    }
}

/// Element context for nested contexts (iframe, shadow DOM).
/// Phase 2 feature for handling elements inside iframes or shadow DOMs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContext {
    /// Context type: "main", "iframe", or "shadow"
    #[serde(default = "default_context_type")]
    pub context_type: String,

    // For iframe elements
    #[serde(default)]
    pub iframe_selector: Option<String>,
    #[serde(default)]
    pub iframe_index: Option<usize>,
    #[serde(default)]
    pub iframe_src: Option<String>,
    #[serde(default)]
    pub is_cross_origin: Option<bool>,
    #[serde(default)]
    pub frame_id: Option<String>,
    #[serde(default)]
    pub frame_path: Option<Vec<usize>>,

    // SOTA Phase 0: Cross-origin iframe context
    /// Origin of the cross-origin iframe (e.g., "https://js.stripe.com")
    #[serde(default)]
    pub origin: Option<String>,
    /// Full href of the cross-origin iframe (if available)
    #[serde(default)]
    pub href: Option<String>,
    /// Bounding rect of the iframe in parent viewport coordinates
    #[serde(default)]
    pub iframe_rect: Option<IframeBoundingRect>,
    /// SOTA Phase 0 Approach B: CDP target ID for frame-session actions.
    /// Used to attach to the iframe's CDP session for executing actions.
    #[serde(default)]
    pub frame_target_id: Option<String>,

    // For shadow DOM elements
    #[serde(default)]
    pub shadow_host_selector: Option<String>,
    #[serde(default)]
    pub shadow_depth: Option<usize>,
    #[serde(default)]
    pub shadow_path: Option<Vec<String>>,
    #[serde(default)]
    pub is_open_shadow: Option<bool>,

    // For elements inside nested scroll containers
    /// Whether this element is inside a scrollable container (overflow: auto|scroll).
    /// When true, the scroll distance hint may be less accurate as it's viewport-relative,
    /// not container-relative. scrollIntoView() still works correctly.
    ///
    /// **NOTE:** The extension sets `inScrollContainer` at the top level of each element,
    /// NOT inside the context object. This field may be `None` even when the element is
    /// in a scroll container. Use `SoMInteractiveElement.in_scroll_container` as the
    /// canonical source, or check both like: `elem.in_scroll_container || elem.context.in_scroll_container.unwrap_or(false)`
    #[serde(default)]
    pub in_scroll_container: Option<bool>,
}

fn default_context_type() -> String {
    "main".to_string()
}

impl Default for ElementContext {
    fn default() -> Self {
        Self {
            context_type: "main".to_string(),
            iframe_selector: None,
            iframe_index: None,
            iframe_src: None,
            is_cross_origin: None,
            frame_id: None,
            frame_path: None,
            origin: None,
            href: None,
            iframe_rect: None,
            frame_target_id: None,
            shadow_host_selector: None,
            shadow_depth: None,
            shadow_path: None,
            is_open_shadow: None,
            in_scroll_container: None,
        }
    }
}

impl ElementContext {
    /// Check if this element is in the main document
    pub fn is_main(&self) -> bool {
        self.context_type == "main"
    }

    /// Check if this element is inside an iframe
    pub fn is_iframe(&self) -> bool {
        self.context_type == "iframe"
    }

    /// Check if this element is inside a shadow DOM
    pub fn is_shadow(&self) -> bool {
        self.context_type == "shadow"
    }

    /// Check if this element is in a cross-origin iframe (SOTA Phase 0)
    pub fn is_cross_origin_iframe(&self) -> bool {
        self.context_type == "cross-origin-iframe" || self.is_cross_origin == Some(true)
    }

    /// Check if this element is inside a scrollable container.
    /// When true, scroll distance hints are approximate (viewport-relative, not container-relative).
    ///
    /// **NOTE:** This may return `false` even when the element is in a scroll container,
    /// because the extension sets `inScrollContainer` at the top level of the element,
    /// not inside the context. Prefer using `SoMInteractiveElement.in_scroll_container` directly.
    pub fn is_in_scroll_container(&self) -> bool {
        self.in_scroll_container == Some(true)
    }
}

/// Record describing a learned or reused pattern during execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsedPattern {
    pub pattern_id: String,
    pub description: Option<String>,
    pub confidence: f64,
}

/// Result for each executed plan step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: Value,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub retry_count: u32,
    pub recovery_strategy: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// The action that was executed (Browser, File, HTTP, or Bash)
    pub executed_action: Option<ExecutableAction>,
    pub pre_action_state: Option<PageState>,
    pub page_observation: Option<PageState>,
    pub validation: Option<ActionValidation>,
    pub success_confidence: f64,
    pub derived_executions: Vec<DerivedExecution>,
}

impl StepExecutionResult {
    pub fn failed(step_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            status: StepStatus::Failed,
            output: Value::Null,
            error: Some(error.into()),
            duration_ms: 0,
            retry_count: 0,
            recovery_strategy: None,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            executed_action: None,
            pre_action_state: None,
            page_observation: None,
            validation: None,
            success_confidence: 0.0,
            derived_executions: Vec::new(),
        }
    }
}

/// Result of validating a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionValidation {
    pub succeeded: bool,
    pub confidence: f64,
    pub detected_changes: Vec<PageChange>,
    pub expected_state: Option<String>,
    pub actual_state: Option<String>,
    pub reasoning: Option<String>,
}

/// Change detected between two observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageChange {
    UrlChanged {
        from: Option<String>,
        to: Option<String>,
    },
    ElementAppeared {
        description: String,
        selector: Option<String>,
    },
    ElementDisappeared {
        description: String,
    },
    TextChanged {
        selector: Option<String>,
        from: Option<String>,
        to: Option<String>,
    },
    StageChanged {
        from: PageStage,
        to: PageStage,
    },
    ModalOpened {
        description: String,
    },
    ModalClosed,
    ErrorAppeared {
        message: String,
    },
}

/// Trace emitted when runtime observation logic decides to capture or skip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationDecision {
    pub step_id: String,
    pub reason: String,
    pub decision: ObservationTrigger,
    pub decided_at: DateTime<Utc>,
    pub confidence: f64,
}

/// Serialized browser state captured from Magicutor for persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MagicutorCapturedState {
    pub raw: Value,
}

/// When to perform observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationTrigger {
    Skip,
    PreOnly,
    PostOnly,
    PreAndPost,
    RetryOnly,
}

/// Aggregate execution record for a PlanStep/goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalExecutionRecord {
    pub plan_step_id: String,
    pub derived_executions: Vec<DerivedExecution>,
    pub final_status: StepStatus,
    pub validation: Option<ActionValidation>,
}

/// Captures a single branch/derived execution under a PlanStep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedExecution {
    pub derived_id: String,
    pub branch_type: BranchType,
    pub triggered_by: TriggerReason,
    /// The action that was executed.
    pub action: ExecutableAction,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub status: StepStatus,
    pub output: Value,
    pub error: Option<String>,
    pub outcome: BranchOutcome,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchType {
    FastPath,
    Retry,
    AgentRecovery,
    VisionFallback,
    ElicitationBased,
    Escalation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchOutcome {
    Success,
    Failed,
    SkippedToNext,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerReason {
    FastPathSuccess,
    ValidationFailed,
    SelectorNotFound,
    AuthenticationExpired,
    TimeoutExceeded,
    UserRequested,
}

/// Lowered plan step that can be executed by the executor.
///
/// The `action` field holds the fully-typed `ExecutableAction`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableStep {
    pub plan_step: PlanStep,
    /// The executable action, possibly wrapped in a spend gate.
    pub action: MaybeGatedAction,
    pub session_id: Option<String>,
    pub timeout_secs: Option<u64>,
    pub reasoning: Option<String>,
}

impl ExecutableStep {
    pub fn new(plan_step: PlanStep, action: MaybeGatedAction) -> Self {
        Self {
            plan_step,
            action,
            session_id: None,
            timeout_secs: None,
            reasoning: None,
        }
    }

    /// Create an ExecutableStep from a bare `ExecutableAction` (convenience constructor).
    pub fn new_bare(plan_step: PlanStep, action: ExecutableAction) -> Self {
        Self::new(plan_step, MaybeGatedAction::Bare(action))
    }

    pub fn step_id(&self) -> &str {
        &self.plan_step.id
    }

    /// Returns the inner `ExecutableAction` regardless of gating.
    /// Used for logging, classification, and pattern matching.
    pub fn inner_action(&self) -> &ExecutableAction {
        self.action.inner_action()
    }

    /// Returns true if this is a browser action
    pub fn is_browser(&self) -> bool {
        self.action.is_browser()
    }
}

// ===========================================================================================
// Enhanced Observability Types (Phase 2 Priority #3)
// ===========================================================================================

/// Complete trace of an agent invocation with parameters, results, and performance metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTrace {
    /// Unique identifier for this trace
    pub trace_id: String,
    /// Step ID this agent was invoked for
    pub step_id: String,
    /// Agent capability used
    pub agent_capability: String,
    /// Input parameters to the agent
    pub request_params: Value,
    /// Agent response
    pub response: AgentTraceResponse,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// When the agent was invoked
    pub invoked_at: DateTime<Utc>,
    /// When the agent completed
    pub completed_at: DateTime<Utc>,
    /// Execution status
    pub status: AgentExecutionStatus,
    /// Error message if failed
    pub error: Option<String>,
    /// Confidence score from agent (0.0 - 1.0)
    pub confidence: f64,
    /// Whether this was a fallback invocation
    pub is_fallback: bool,
    /// Previous agent that failed (if this is fallback)
    pub fallback_from: Option<String>,
}

/// Agent execution status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentExecutionStatus {
    Success,
    Failed,
    Timeout,
    Skipped,
}

/// Agent response data for tracing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTraceResponse {
    /// Raw response data
    pub data: Value,
    /// Metadata from the agent
    pub metadata: HashMap<String, String>,
}

/// Record of a decision point in execution (why a choice was made).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionPoint {
    /// Unique identifier for this decision
    pub decision_id: String,
    /// Step ID where decision was made
    pub step_id: String,
    /// Type of decision
    pub decision_type: DecisionType,
    /// Why this decision was made
    pub reasoning: String,
    /// Options that were considered
    pub considered_options: Vec<DecisionOption>,
    /// The option that was chosen
    pub chosen_option: String,
    /// Confidence in this decision (0.0 - 1.0)
    pub confidence: f64,
    /// When the decision was made
    pub decided_at: DateTime<Utc>,
    /// Context data relevant to this decision
    pub context: HashMap<String, Value>,
}

/// Type of decision being made
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionType {
    /// Selecting which agent to use
    AgentSelection,
    /// Choosing a recovery strategy
    RecoveryStrategy,
    /// Validation threshold decision
    ValidationThreshold,
    /// Clarification requirement decision
    ClarificationTrigger,
    /// Other custom decision type
    Other,
}

/// An option considered during a decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionOption {
    /// Name/identifier of this option
    pub name: String,
    /// Description of what this option would do
    pub description: String,
    /// Score assigned to this option
    pub score: f64,
    /// Why this option was or wasn't chosen
    pub rationale: Option<String>,
}

/// Performance span tracking for operations (distributed tracing style).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSpan {
    /// Unique span identifier
    pub span_id: String,
    /// Parent span ID (for nested operations)
    pub parent_span_id: Option<String>,
    /// Root trace ID (for correlating all spans in an execution)
    pub trace_id: String,
    /// Step ID this span belongs to
    pub step_id: Option<String>,
    /// Operation name/type
    pub operation: String,
    /// Operation category
    pub category: SpanCategory,
    /// When the operation started
    pub started_at: DateTime<Utc>,
    /// When the operation completed
    pub completed_at: Option<DateTime<Utc>>,
    /// Duration in milliseconds
    pub duration_ms: Option<u64>,
    /// Operation status
    pub status: SpanStatus,
    /// Additional metadata
    pub attributes: HashMap<String, Value>,
    /// Child spans
    pub children: Vec<String>,
}

/// Category of span for filtering/grouping
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpanCategory {
    /// Browser session or observation work
    Browser,
    /// Agent invocation
    AgentExecution,
    /// Page observation/capture
    PageObservation,
    /// Validation check
    Validation,
    /// Recovery attempt
    Recovery,
    /// Clarification/AskLoop
    Clarification,
    /// Budget tracking
    Budget,
    /// State persistence
    StatePersistence,
    /// Other/custom category
    Other,
}

/// Status of a span
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    InProgress,
    Success,
    Failed,
    Cancelled,
}

/// Unified observability event for audit trail
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum ObservabilityEvent {
    /// Agent was invoked
    AgentInvoked { trace: AgentTrace },
    /// Decision point reached
    DecisionMade { decision: DecisionPoint },
    /// Execution span recorded
    SpanRecorded { span: ExecutionSpan },
    /// Recovery strategy triggered
    RecoveryTriggered {
        step_id: String,
        strategy: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Clarification requested
    ClarificationRequested {
        step_id: String,
        question_id: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Budget threshold crossed
    BudgetThreshold {
        threshold_type: String,
        current_utilization: f64,
        timestamp: DateTime<Utc>,
    },
}

/// Observability collector for tracking all execution events
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservabilityCollector {
    /// All agent traces
    pub agent_traces: Vec<AgentTrace>,
    /// All decision points
    pub decision_points: Vec<DecisionPoint>,
    /// All execution spans
    pub execution_spans: Vec<ExecutionSpan>,
    /// Unified event log
    pub events: Vec<ObservabilityEvent>,
    /// Active spans (span_id -> start time)
    #[serde(skip)]
    pub active_spans: HashMap<String, DateTime<Utc>>,
}

impl ObservabilityCollector {
    /// Create a new collector
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an agent trace
    pub fn record_agent_trace(&mut self, trace: AgentTrace) {
        self.events.push(ObservabilityEvent::AgentInvoked {
            trace: trace.clone(),
        });
        self.agent_traces.push(trace);
    }

    /// Record a decision point
    pub fn record_decision(&mut self, decision: DecisionPoint) {
        self.events.push(ObservabilityEvent::DecisionMade {
            decision: decision.clone(),
        });
        self.decision_points.push(decision);
    }

    /// Start a new execution span
    pub fn start_span(
        &mut self,
        span_id: String,
        trace_id: String,
        operation: String,
        category: SpanCategory,
        step_id: Option<String>,
        parent_span_id: Option<String>,
    ) -> String {
        let now = Utc::now();
        self.active_spans.insert(span_id.clone(), now);

        let span = ExecutionSpan {
            span_id: span_id.clone(),
            parent_span_id,
            trace_id,
            step_id,
            operation,
            category,
            started_at: now,
            completed_at: None,
            duration_ms: None,
            status: SpanStatus::InProgress,
            attributes: HashMap::new(),
            children: Vec::new(),
        };

        self.execution_spans.push(span);
        span_id
    }

    /// End an execution span
    pub fn end_span(&mut self, span_id: &str, status: SpanStatus) {
        if let Some(start_time) = self.active_spans.remove(span_id) {
            let now = Utc::now();
            let duration = (now - start_time).num_milliseconds() as u64;

            // Find and update the span
            if let Some(span) = self
                .execution_spans
                .iter_mut()
                .find(|s| s.span_id == span_id)
            {
                span.completed_at = Some(now);
                span.duration_ms = Some(duration);
                span.status = status;

                self.events
                    .push(ObservabilityEvent::SpanRecorded { span: span.clone() });
            }
        }
    }

    /// Add attributes to a span
    pub fn add_span_attributes(&mut self, span_id: &str, attributes: HashMap<String, Value>) {
        if let Some(span) = self
            .execution_spans
            .iter_mut()
            .find(|s| s.span_id == span_id)
        {
            span.attributes.extend(attributes);
        }
    }

    /// Record a recovery trigger event
    pub fn record_recovery_triggered(&mut self, step_id: String, strategy: String, reason: String) {
        self.events.push(ObservabilityEvent::RecoveryTriggered {
            step_id,
            strategy,
            reason,
            timestamp: Utc::now(),
        });
    }

    /// Record a clarification request event
    pub fn record_clarification_requested(
        &mut self,
        step_id: String,
        question_id: String,
        reason: String,
    ) {
        self.events
            .push(ObservabilityEvent::ClarificationRequested {
                step_id,
                question_id,
                reason,
                timestamp: Utc::now(),
            });
    }

    /// Record a budget threshold event
    pub fn record_budget_threshold(&mut self, threshold_type: String, current_utilization: f64) {
        self.events.push(ObservabilityEvent::BudgetThreshold {
            threshold_type,
            current_utilization,
            timestamp: Utc::now(),
        });
    }

    /// Get all traces for a specific step
    pub fn get_traces_for_step(&self, step_id: &str) -> Vec<&AgentTrace> {
        self.agent_traces
            .iter()
            .filter(|t| t.step_id == step_id)
            .collect()
    }

    /// Get all decisions for a specific step
    pub fn get_decisions_for_step(&self, step_id: &str) -> Vec<&DecisionPoint> {
        self.decision_points
            .iter()
            .filter(|d| d.step_id == step_id)
            .collect()
    }

    /// Get all spans for a specific step
    pub fn get_spans_for_step(&self, step_id: &str) -> Vec<&ExecutionSpan> {
        self.execution_spans
            .iter()
            .filter(|s| s.step_id.as_deref() == Some(step_id))
            .collect()
    }

    /// Get total execution time across all completed spans
    pub fn total_execution_time_ms(&self) -> u64 {
        self.execution_spans
            .iter()
            .filter_map(|s| s.duration_ms)
            .sum()
    }

    /// Get average agent response time
    pub fn average_agent_response_time_ms(&self) -> Option<u64> {
        if self.agent_traces.is_empty() {
            return None;
        }
        let total: u64 = self.agent_traces.iter().map(|t| t.duration_ms).sum();
        Some(total / self.agent_traces.len() as u64)
    }

    /// Get agent success rate
    pub fn agent_success_rate(&self) -> f64 {
        if self.agent_traces.is_empty() {
            return 0.0;
        }
        let successful = self
            .agent_traces
            .iter()
            .filter(|t| t.status == AgentExecutionStatus::Success)
            .count();
        successful as f64 / self.agent_traces.len() as f64
    }
}

// Note: ExecutionError and ExecutionResult are defined in error.rs, not here.
// See execution/error.rs for the canonical definitions.

// =============================================================================
// SOTA Phase 6: Action Verification Types
// =============================================================================

/// Optional action outcome annotation for history display.
///
/// Browser task success is decided by the visible LLM loop from tool results,
/// artifacts, screenshots, and terminal decisions. This struct may carry
/// explicit non-browser checks such as download existence or URL equality, but
/// it no longer computes hidden DOM/visual diffs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionVerification {
    /// Expected visual change predicted before action (from LLM decision)
    /// e.g., "Submit button will depress, form will show loading"
    #[serde(default)]
    pub expected_change: Option<String>,

    /// Hash/fingerprint of the screenshot before action
    #[serde(default)]
    pub pre_screenshot_hash: Option<u64>,

    /// Hash/fingerprint of the screenshot after action
    #[serde(default)]
    pub post_screenshot_hash: Option<u64>,

    /// URL before action
    #[serde(default)]
    pub pre_url: Option<String>,

    /// URL after action
    #[serde(default)]
    pub post_url: Option<String>,

    /// Whether verification detected a visual change
    #[serde(default)]
    pub visual_change_detected: bool,

    /// Whether URL changed after action
    #[serde(default)]
    pub url_changed: bool,

    /// Whether the change matches expected behavior
    #[serde(default)]
    pub matches_expectation: VerificationMatch,

    /// Confidence score for the verification (0.0-1.0)
    #[serde(default)]
    pub confidence: f64,

    /// Detailed assessment of what changed
    #[serde(default)]
    pub change_assessment: Option<String>,

    /// Suggestions for self-correction if mismatch detected
    #[serde(default)]
    pub self_correction_hint: Option<String>,

    /// Time between pre and post observation (ms)
    #[serde(default)]
    pub verification_latency_ms: u64,
}

impl Default for ActionVerification {
    fn default() -> Self {
        Self {
            expected_change: None,
            pre_screenshot_hash: None,
            post_screenshot_hash: None,
            pre_url: None,
            post_url: None,
            visual_change_detected: false,
            url_changed: false,
            matches_expectation: VerificationMatch::Unknown,
            confidence: 0.0,
            change_assessment: None,
            self_correction_hint: None,
            verification_latency_ms: 0,
        }
    }
}

impl ActionVerification {
    /// Create a new verification with pre-action state
    pub fn new_pre_action(pre_url: Option<String>, pre_screenshot_hash: Option<u64>) -> Self {
        Self {
            pre_url,
            pre_screenshot_hash,
            ..Default::default()
        }
    }

    /// Record post-action state and compute verification
    pub fn record_post_action(
        mut self,
        post_url: Option<String>,
        post_screenshot_hash: Option<u64>,
        expected_change: Option<String>,
        latency_ms: u64,
    ) -> Self {
        // Detect URL change
        self.url_changed = match (&self.pre_url, &post_url) {
            (Some(pre), Some(post)) => pre != post,
            _ => false,
        };

        // Detect visual change via hash comparison
        self.visual_change_detected = match (self.pre_screenshot_hash, post_screenshot_hash) {
            (Some(pre), Some(post)) => pre != post,
            _ => false,
        };

        self.post_url = post_url;
        self.post_screenshot_hash = post_screenshot_hash;
        self.expected_change = expected_change;
        self.verification_latency_ms = latency_ms;

        // Compute match assessment
        self.matches_expectation = if self.visual_change_detected || self.url_changed {
            // Something changed - likely action had effect
            VerificationMatch::Match
        } else {
            // Nothing changed - might be a silent failure
            VerificationMatch::NoChange
        };

        // Set confidence based on what we observed
        self.confidence = if self.visual_change_detected && self.url_changed {
            0.95 // High confidence - both visual and URL changed
        } else if self.url_changed {
            0.85 // URL changed is strong signal
        } else if self.visual_change_detected {
            0.75 // Visual change only
        } else {
            0.3 // No change detected - low confidence action worked
        };

        self
    }

    /// Check if this verification suggests action may have failed
    pub fn suggests_failure(&self) -> bool {
        matches!(
            self.matches_expectation,
            VerificationMatch::NoChange | VerificationMatch::Mismatch
        )
    }

    /// Generate a hint for self-correction
    pub fn generate_self_correction_hint(&mut self) {
        if self.suggests_failure() {
            self.self_correction_hint = Some(
                "Action may have failed silently - no visual change detected. \
                 Consider: 1) Element may be disabled/occluded 2) Wait for page load \
                 3) Try alternative selector 4) Scroll element into view"
                    .to_string(),
            );
            self.change_assessment =
                Some("No visual or URL change detected after action".to_string());
        } else if self.url_changed && self.visual_change_detected {
            self.change_assessment =
                Some("Both URL and visual content changed - action likely succeeded".to_string());
        } else if self.url_changed {
            self.change_assessment = Some("URL changed - navigation occurred".to_string());
        } else if self.visual_change_detected {
            self.change_assessment =
                Some("Visual change detected - action had some effect".to_string());
        }
    }
}

/// Result of verification matching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMatch {
    /// The observed change matches expected behavior
    Match,
    /// The observed change does not match expectations
    Mismatch,
    /// No change was detected (possible silent failure)
    NoChange,
    /// Verification could not be performed
    #[default]
    Unknown,
}

// =============================================================================
// SOTA Phase 7: Active Probing (Hover-to-Discover) Types
// =============================================================================

/// SOTA Phase 7: Configuration for active hover probing.
///
/// Active probing discovers hidden elements that only appear on hover,
/// such as dropdowns, tooltips, and context menus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverProbeConfig {
    /// Whether hover probing is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Maximum number of elements to probe per observation
    #[serde(default = "default_max_probe_targets")]
    pub max_probe_targets: usize,

    /// Delay after hover before re-observing (ms)
    #[serde(default = "default_probe_delay_ms")]
    pub probe_delay_ms: u64,

    /// Types of elements to consider for probing
    #[serde(default)]
    pub target_types: Vec<HoverTargetType>,
}

fn default_max_probe_targets() -> usize {
    5
}

fn default_probe_delay_ms() -> u64 {
    200
}

impl Default for HoverProbeConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default to avoid performance impact
            max_probe_targets: 5,
            probe_delay_ms: 200,
            target_types: vec![
                HoverTargetType::Dropdown,
                HoverTargetType::Menu,
                HoverTargetType::Tooltip,
            ],
        }
    }
}

/// Type of element that might reveal content on hover.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HoverTargetType {
    /// Dropdown menus (nav items, select-like elements)
    Dropdown,
    /// Navigation menus with submenus
    Menu,
    /// Elements with tooltips
    Tooltip,
    /// Context menu triggers (right-click targets)
    ContextMenu,
    /// Accordion/expandable sections
    Accordion,
    /// Any element with :hover CSS effects
    HoverEffect,
}

/// A potential target for hover probing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverTarget {
    /// SoM element ID
    pub element_id: usize,

    /// CSS selector for the element
    pub selector: String,

    /// Why this element was identified as a hover target
    pub reason: HoverTargetType,

    /// Confidence that hovering will reveal content (0.0-1.0)
    pub confidence: f64,

    /// Element text (for debugging)
    #[serde(default)]
    pub text: Option<String>,
}

/// Result of probing a single hover target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverProbeResult {
    /// The element that was hovered
    pub target: HoverTarget,

    /// Whether probing was successful
    pub success: bool,

    /// Number of new elements revealed
    pub elements_revealed: usize,

    /// IDs of newly revealed elements
    #[serde(default)]
    pub revealed_element_ids: Vec<usize>,

    /// Error message if probing failed
    #[serde(default)]
    pub error: Option<String>,

    /// Time taken for the probe (ms)
    pub probe_time_ms: u64,
}

/// Summary of all hover probing for an observation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HoverProbeSummary {
    /// Total number of targets probed
    pub targets_probed: usize,

    /// Number of successful probes
    pub successful_probes: usize,

    /// Total new elements discovered
    pub total_elements_revealed: usize,

    /// Results from each probe
    #[serde(default)]
    pub probe_results: Vec<HoverProbeResult>,

    /// Total time spent probing (ms)
    pub total_probe_time_ms: u64,

    /// Guidance for the agent based on probing results
    #[serde(default)]
    pub guidance: Option<String>,
}

impl HoverProbeSummary {
    /// Generate guidance based on probing results
    pub fn generate_guidance(&mut self) {
        if self.total_elements_revealed > 0 {
            self.guidance = Some(format!(
                "Hover probing discovered {} new interactive elements from {} targets. \
                 These elements may be in dropdown menus or tooltips.",
                self.total_elements_revealed, self.successful_probes
            ));
        } else if self.targets_probed > 0 {
            self.guidance = Some(
                "Hover probing found no new elements. The page may not have hidden hover content."
                    .to_string(),
            );
        }
    }

    /// Format for LLM consumption
    pub fn format_for_llm(&self) -> String {
        if self.targets_probed == 0 {
            return String::new();
        }

        let mut lines = vec![
            "## HOVER PROBING RESULTS".to_string(),
            format!("- Targets probed: {}", self.targets_probed),
            format!(
                "- New elements discovered: {}",
                self.total_elements_revealed
            ),
        ];

        if let Some(ref guidance) = self.guidance {
            lines.push(format!("- Guidance: {}", guidance));
        }

        for result in &self.probe_results {
            if result.elements_revealed > 0 {
                lines.push(format!(
                    "- Hover [{}] ({:?}): revealed {} new elements",
                    result.target.element_id, result.target.reason, result.elements_revealed
                ));
            }
        }

        lines.join("\n")
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // =========================================================================
    // SoM Type Tests
    // =========================================================================

    #[test]
    fn test_som_bounding_rect_center() {
        let rect = SoMBoundingRect::new(100.0, 200.0, 50.0, 30.0);
        let (cx, cy) = rect.center();
        assert!((cx - 125.0).abs() < 0.001);
        assert!((cy - 215.0).abs() < 0.001);
    }

    #[test]
    fn test_som_bounding_rect_is_valid() {
        let valid = SoMBoundingRect::new(10.0, 20.0, 100.0, 50.0);
        assert!(valid.is_valid());

        let zero_width = SoMBoundingRect::new(10.0, 20.0, 0.0, 50.0);
        assert!(!zero_width.is_valid());

        let negative_height = SoMBoundingRect::new(10.0, 20.0, 100.0, -5.0);
        assert!(!negative_height.is_valid());
    }

    #[test]
    fn test_som_interactive_element_deserialization() {
        // Simulates data from extractInteractiveElements JS function
        let json = r##"{
            "id": 5,
            "selector": "#submit-btn",
            "tag": "button",
            "text": "Submit",
            "role": "button",
            "rect": {"x": 100, "y": 200, "width": 80, "height": 40},
            "rectScaled": {"x": 200, "y": 400, "width": 160, "height": 80},
            "zIndex": 10,
            "isInOverlay": false,
            "overlayId": null,
            "attributes": {"type": "submit", "disabled": null},
            "context": {"contextType": "main"}
        }"##;

        let elem: SoMInteractiveElement = serde_json::from_str(json).unwrap();

        assert_eq!(elem.id, 5);
        assert_eq!(elem.selector, r##"#submit-btn"##);
        assert_eq!(elem.tag, "button");
        assert_eq!(elem.text, Some("Submit".to_string()));
        assert_eq!(elem.role, Some("button".to_string()));
        assert!((elem.rect.x - 100.0).abs() < 0.001);
        assert!((elem.rect_scaled.x - 200.0).abs() < 0.001);
        assert_eq!(elem.z_index, 10);
        assert!(!elem.is_in_overlay);
        assert!(elem.overlay_id.is_none());
        assert!(elem.context.is_main());
    }

    #[test]
    fn test_som_interactive_element_in_overlay() {
        let json = r#"{
            "id": 0,
            "selector": ".modal-close",
            "tag": "button",
            "text": "×",
            "role": "button",
            "rect": {"x": 450, "y": 50, "width": 30, "height": 30},
            "rectScaled": {"x": 900, "y": 100, "width": 60, "height": 60},
            "zIndex": 1001,
            "isInOverlay": true,
            "overlayId": 1,
            "attributes": {"aria-label": "Close"},
            "context": {"contextType": "main"}
        }"#;

        let elem: SoMInteractiveElement = serde_json::from_str(json).unwrap();

        assert_eq!(elem.id, 0);
        assert!(elem.is_in_overlay);
        assert_eq!(elem.overlay_id, Some(1));
        assert_eq!(elem.z_index, 1001);
    }

    #[test]
    fn test_som_overlay_container_deserialization() {
        let json = r#"{
            "id": 1,
            "selector": ".modal-overlay",
            "rect": {"x": 0, "y": 0, "width": 1920, "height": 1080},
            "zIndex": 1000,
            "overlayType": "modal",
            "hasCloseButton": true,
            "closeButtonId": 0
        }"#;

        let overlay: SoMOverlayContainer = serde_json::from_str(json).unwrap();

        assert_eq!(overlay.id, 1);
        assert_eq!(overlay.selector, ".modal-overlay");
        assert_eq!(overlay.z_index, 1000);
        assert_eq!(overlay.overlay_type, "modal");
        assert!(overlay.has_close_button);
        assert_eq!(overlay.close_button_id, Some(0));
    }

    #[test]
    fn test_element_context_types() {
        let main_ctx = ElementContext::default();
        assert!(main_ctx.is_main());
        assert!(!main_ctx.is_iframe());
        assert!(!main_ctx.is_shadow());

        let iframe_ctx = ElementContext {
            context_type: "iframe".to_string(),
            iframe_selector: Some(".my-iframe".to_string()),
            iframe_index: Some(0),
            ..Default::default()
        };
        assert!(!iframe_ctx.is_main());
        assert!(iframe_ctx.is_iframe());
        assert!(!iframe_ctx.is_shadow());

        let shadow_ctx = ElementContext {
            context_type: "shadow".to_string(),
            shadow_host_selector: Some(".shadow-host".to_string()),
            shadow_depth: Some(1),
            ..Default::default()
        };
        assert!(!shadow_ctx.is_main());
        assert!(!shadow_ctx.is_iframe());
        assert!(shadow_ctx.is_shadow());
    }

    #[test]
    fn test_scroll_offset_default() {
        let offset = ScrollOffset::default();
        assert!((offset.x - 0.0).abs() < 0.001);
        assert!((offset.y - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_viewport_size_default() {
        let size = ViewportSize::default();
        assert_eq!(size.width, 0);
        assert_eq!(size.height, 0);
    }

    #[test]
    fn test_page_state_with_som_fields() {
        // Test that PageState can be constructed with SoM fields
        let elem = SoMInteractiveElement {
            id: 0,
            selector: ".test-button".to_string(),
            tag: "button".to_string(),
            text: Some("Test".to_string()),
            role: None,
            backend_node_id: None,
            rect: SoMBoundingRect::new(10.0, 20.0, 100.0, 50.0),
            rect_scaled: SoMBoundingRect::new(20.0, 40.0, 200.0, 100.0),
            z_index: 1,
            is_in_overlay: false,
            overlay_id: None,
            attributes: std::collections::HashMap::new(),
            context: ElementContext::default(),
            merged_children: false,
            detected_via: None,
            is_clickable: true,
            clickability_reason: None,
            occluded_by: None,
            outside_viewport: false,
            is_occluded_by_parent: false,
            priority: 60,
            in_viewport: true,
            viewport_visibility: 2,
            selector_chain: vec![],
            fingerprint: Some("#test-button".to_string()),
            in_scroll_container: false,
            scroll_container_selector: None,
            scroll_container_visibility_ratio: None,
            scroll_container_viewport_hint: None,
            scroll_container_can_scroll_y: None,
            scroll_container_can_scroll_x: None,
            scroll_clip_container_selector: None,
            scroll_clip_direction: None,
            scroll_clip_distance: None,
            scroll_clip_can_scroll: None,
            is_same_origin_iframe: false,
            iframe_selector_direct: None,
            js_evaluate_hint: None,
            surface_owner_id: None,
            surface_owner_kind: None,
            surface_overlap_ratio: None,
            surface_center_inside: None,
            surface_ownership_confidence: None,
            is_text_element: false,
            named_ancestor: None,
        };

        let page_state = PageState {
            interactive_elements_raw: Some(vec![elem]),
            overlay_containers: Some(vec![]),
            device_pixel_ratio: Some(2.0),
            scroll_offset: Some(ScrollOffset { x: 0.0, y: 100.0 }),
            viewport_size: Some(ViewportSize {
                width: 1920,
                height: 1080,
            }),
            ..Default::default()
        };

        assert!(page_state.interactive_elements_raw.is_some());
        assert_eq!(
            page_state.interactive_elements_raw.as_ref().unwrap().len(),
            1
        );
        assert_eq!(page_state.device_pixel_ratio, Some(2.0));
        assert_eq!(page_state.scroll_offset.as_ref().unwrap().y, 100.0);
        assert_eq!(page_state.viewport_size.as_ref().unwrap().width, 1920);
    }

    #[test]
    fn test_page_state_primary_spatial_surface_prefers_likely_primary() {
        let page_state = PageState {
            spatial_surfaces: Some(vec![
                SpatialSurfaceInfo {
                    id: "canvas:main".to_string(),
                    surface_kind: SpatialSurfaceKind::Canvas,
                    selector: Some("#board".to_string()),
                    frame_context: SurfaceFrameContext::MainDocument,
                    rect_css: SoMBoundingRect::new(0.0, 0.0, 800.0, 600.0),
                    rect_scaled: SoMBoundingRect::new(0.0, 0.0, 1600.0, 1200.0),
                    z_index: 1,
                    area_ratio: 0.5,
                    likely_primary: false,
                    visible: true,
                    occluded: false,
                    same_origin_access: true,
                    rendering_kind: SurfaceRenderingKind::Canvas2d,
                    can_read_pixels: None,
                    has_pointer_listeners: Some(true),
                    role_hint: Some("editor".to_string()),
                    mode_state: None,
                },
                SpatialSurfaceInfo {
                    id: "svg:main".to_string(),
                    surface_kind: SpatialSurfaceKind::Svg,
                    selector: Some("#overlay".to_string()),
                    frame_context: SurfaceFrameContext::MainDocument,
                    rect_css: SoMBoundingRect::new(20.0, 20.0, 300.0, 180.0),
                    rect_scaled: SoMBoundingRect::new(40.0, 40.0, 600.0, 360.0),
                    z_index: 2,
                    area_ratio: 0.1,
                    likely_primary: true,
                    visible: true,
                    occluded: false,
                    same_origin_access: true,
                    rendering_kind: SurfaceRenderingKind::Svg,
                    can_read_pixels: Some(true),
                    has_pointer_listeners: None,
                    role_hint: None,
                    mode_state: None,
                },
            ]),
            ..Default::default()
        };

        let primary = page_state
            .primary_spatial_surface()
            .expect("primary surface should exist");
        assert_eq!(primary.id, "svg:main");
        assert_eq!(page_state.spatial_surfaces().len(), 2);
    }

    #[test]
    fn test_surface_rendering_kind_deserializes_wire_values_and_aliases() {
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""canvas_2d""#).unwrap(),
            SurfaceRenderingKind::Canvas2d
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""canvas2d""#).unwrap(),
            SurfaceRenderingKind::Canvas2d
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""webgl""#).unwrap(),
            SurfaceRenderingKind::WebGl
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""web_gl""#).unwrap(),
            SurfaceRenderingKind::WebGl
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""webgpu""#).unwrap(),
            SurfaceRenderingKind::WebGpu
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""web_gpu""#).unwrap(),
            SurfaceRenderingKind::WebGpu
        );
        assert_eq!(
            serde_json::from_str::<SurfaceRenderingKind>(r#""dom_custom""#).unwrap(),
            SurfaceRenderingKind::DomCustom
        );
    }

    #[test]
    fn test_som_bounding_rect_serialization_roundtrip() {
        let rect = SoMBoundingRect::new(123.5, 456.7, 89.1, 23.4);
        let json = serde_json::to_string(&rect).unwrap();
        let deserialized: SoMBoundingRect = serde_json::from_str(&json).unwrap();

        assert!((rect.x - deserialized.x).abs() < 0.001);
        assert!((rect.y - deserialized.y).abs() < 0.001);
        assert!((rect.width - deserialized.width).abs() < 0.001);
        assert!((rect.height - deserialized.height).abs() < 0.001);
    }
}
