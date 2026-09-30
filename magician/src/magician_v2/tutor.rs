//! Personal Tutor orchestration contracts.
//!
//! This module intentionally starts as a pure contract layer. The live tutor
//! still runs through chat/tool dispatch, but these types and validators keep
//! the desired multi-state behavior explicit and testable before we introduce
//! a durable step executor.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// The VibeDev invoke grammar lives in `chat::invoke_grammar` (plan 1.2b);
// its rail parser is imported only for the grammar-pinning tests below.
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::chat::invoke_grammar::parse_vibedev_rail_invocation;
use crate::magician_v2::chat::invoke_grammar::{voice_command_tokens, VoiceCommandToken};

// The App Copilot rail's product logic — rail identity, manual-action
// check receipts, user-action preemption, and the demo-and-cleanup guards
// — lives in `tutor::copilot_rail` (plan 1.2c).
use crate::magician_v2::tutor::copilot_rail::tutor_prompt_requests_demo_cleanup;
use crate::magician_v2::tutor::copilot_rail::CopilotActionCheckReceipt;
use crate::magician_v2::tutor::copilot_rail::TutorProductRail;

pub const TUTOR_MARKER_INVOKE_WORDS: [&str; 2] = ["@tutor", "@tutur"];
pub const TUTOR_SPOKEN_INVOKE_WORDS: [&str; 2] = ["tutor", "tutur"];
pub const APP_COPILOT_MARKER_INVOKE_WORDS: [&str; 4] =
    ["@copilot", "@appcopilot", "@app-copilot", "@app_copilot"];
pub const APP_COPILOT_SPOKEN_INVOKE_WORDS: [&str; 1] = ["copilot"];
pub const TUTOR_QUICK_FLAG: &str = "#quick";
const TUTOR_DRAW_GROUP_MAX_DEPTH: usize = 8;
const TUTOR_DRAW_MAX_DELAY_MS: u64 = 60_000;
const TUTOR_DRAW_MAX_DURATION_MS: u64 = 120_000;
const TUTOR_DRAW_MAX_REVEAL_ORDER: u64 = 1_000;
const TUTOR_DRAW_MAX_REVEAL_ID_CHARS: usize = 128;
const TUTOR_DRAW_MAX_STORYBOARD_STEP_ID_CHARS: usize = 128;
const TUTOR_DRAW_MAX_STEP_LABEL_CHARS: usize = 160;
const TUTOR_DRAW_MAX_NARRATION_CHARS: usize = 1_200;
const TUTOR_DRAW_MAX_PATH_CHARS: usize = 8_000;
const TUTOR_CHART_FRAME_MIN_AXIS_LENGTH: f64 = 80.0;
const TUTOR_CHART_FRAME_MIN_SERIES_EXTENT: f64 = 20.0;
const TUTOR_CHART_FRAME_MIN_SLACK_PX: f64 = 32.0;
/// A product lane is conversational state, not permanent session authority.
/// Thirty minutes preserves natural follow-up turns while preventing an
/// abandoned Tutor/App Copilot run from elevating unrelated later messages.
const TUTOR_ACTIVE_LANE_IDLE_TIMEOUT_MS: i64 = 30 * 60 * 1_000;
const TUTOR_LESSON_MAX_MILESTONES: usize = 64;
const TUTOR_LESSON_MAX_MILESTONE_ID_CHARS: usize = 96;
const TUTOR_LESSON_MAX_MILESTONE_OBJECTIVE_CHARS: usize = 320;

pub const VISUAL_STORYBOARD_RUNTIME_FALLBACK: &str = r#"## Visual Storyboard Runtime
The user explicitly invoked Personal Tutor or App Copilot. If the backend has not already started a run, call start_tutor_run first; if runtime preflight already started one, do not start a duplicate.

Treat drawing, overlay narration, tutor/copilot speech, and final chat text as one storyboard. Every meaningful tutor/copilot draw/reveal should include tutor_step_label or step_label plus narration inside shape_json itself; top-level tool metadata alone is not enough. A Personal Tutor run carries a lesson_contract and lesson_progress in runtime results on both screen-overlay and blackboard canvases. On its first draw, pass tutor_lesson_plan with milestone_id/objective pairs, plus a role of setup/core/conclusion on each. A deep lesson is only plan_ready with at least two core milestones and at least as many core as setup+conclusion combined, so the hard step is decomposed rather than asserted. Normal Tutor is progressive: its runtime 1/2/3 depth is a minimum floor, not a fixed lesson length, so add every prerequisite, reasoning link, example, or conclusion the concept genuinely requires. Tutor Quick is condensed: choose only the essential conceptual spine within its returned range (focused 1, standard 1-2, deep/concept 2-6), render the first small milestone immediately in the same tool call that supplies the plan, keep each narration at most 240 characters, and complete as soon as those essential milestones are taught. Quick reduces both time to first useful overlay and total lesson length, but it must not pack several milestones into one first draw. App Copilot retains its independent action policy and has no lesson plan. Each storyboard_step_id/reveal_id must match a planned milestone_id. Later plan revisions may only append genuinely new objectives and must remain within the contract maximum when one is present. Do not finalize until plan_ready is true and remaining_storyboard_steps is zero. Each covered step needs meaningful narration and drawing; a Say step, unplanned or duplicate/case-variant id, or a renamed id carrying the same narration does not count. Completion also requires a storyboard-bound drawing after the latest overlay clear. Use separate screen-draw calls or distinct child reveal steps with their own reveal_id/storyboard_step_id, reveal_order, label, and narration so the overlay reveals and speaks one step at a time. For instructional cursive letters/words, use text-backed cursive_text; it renders with the selected tutor cursive font. Use handwriting for casual handwritten labels. Use path, curve, or freehand for stroke motion, smooth paths, organic curves, and non-angular diagrams instead of approximating curves with straight lines. For synthetic chart/trend drawings, keep plotted path/curve/freehand series inside the rectangle bounded by the horizontal and vertical axes. The final chat reply should only recap the same visible/narrated sequence instead of inventing a separate explanation."#;

pub const PERSONAL_TUTOR_POLICY_FALLBACK: &str = r#"## Personal Tutor Policy
`@tutor` / `hey tutor` are concept-teaching invokes. Source-free turns use blackboard canvas; source-backed turns use screen_overlay concept tutoring. Personal Tutor must not click, type, scroll, hotkey, or mutate apps. For visible math, physics, computer-science, diagrams, formulas, code, PDFs, images, and paused frames, identify relevant visual entities and draw complete progressive explanations grounded in the latest observation. When synthesizing a chart from visible values, frame the plotted series with axes that cover the whole line."#;

pub const APP_COPILOT_POLICY_FALLBACK: &str = r#"## App Copilot Policy
`@copilot` / `hey copilot` are app-help invokes for hybrid mutation. Use `@tutor` for explanation-only teaching. App Copilot must use screen_overlay, observe the live app, draw and narrate the next reversible UI step, immediately check whether the user already performed the highlighted step, then proceed with mac-operator automation without an artificial wait. If the user acts while automation is queued or running, backend preemption cancels the delegated execution before advancing the run. Verify every user-performed or delegated action before continuing. Every UI-changing tutor_action must copy the storyboard_step_id or storyboard_step_label from both the immediately preceding screen-draw preview and its fresh check_for_copilot_user_action result; the server authorization is single-use, short-lived, and rejected if missing, stale, or mismatched. Delegate envelope-bound click/type_text/hotkey/scroll actions to mac-operator as bounded state-change requests; mac-operator should choose the cheapest correct engine from its decision table, preferring script/JXA for scriptable apps and CUA for non-scriptable or coordinate-bound UI. For type_text, include the exact text in action_instruction/expected_state or ask for the missing content. Demo-and-cleanup must record created objects and cannot complete until each run-owned object is successfully removed. Cleanup only objects created in this same run unless the user confirms."#;

/// Data-driven drawing primitives: recipe model, validation, and the scoped
/// loader/merger that serves the tutor primitive set to clients.
pub mod primitives;

/// The App Copilot rail's product logic (rail identity, action-check
/// receipts, preemption, demo-and-cleanup guards), split out of this
/// module behind the 1.2c lane seam; re-exported above.
pub mod copilot_rail;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorStepKind {
    Observe,
    ResolveTarget,
    Draw,
    Say,
    Wait,
    Click,
    TypeText,
    Hotkey,
    Scroll,
    Verify,
    ClearDrawings,
    Confirm,
    Recover,
}

impl TutorStepKind {
    pub fn as_str(self) -> &'static str {
        tutor_step_kind_name(self)
    }

    pub fn requires_fresh_observation(self) -> bool {
        matches!(
            self,
            TutorStepKind::ResolveTarget
                | TutorStepKind::Draw
                | TutorStepKind::Click
                | TutorStepKind::TypeText
                | TutorStepKind::Hotkey
                | TutorStepKind::Scroll
                | TutorStepKind::Verify
        )
    }

    pub fn changes_ui_state(self) -> bool {
        matches!(
            self,
            TutorStepKind::Click
                | TutorStepKind::TypeText
                | TutorStepKind::Hotkey
                | TutorStepKind::Scroll
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorSafetyLevel {
    VisualOnly,
    ReversibleAction,
    SessionOwnedDestructive,
    DestructiveRequiresConfirmation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorVisualEntityKind {
    Point,
    LineSegment,
    Ray,
    Angle,
    Polygon,
    Circle,
    Arc,
    Axis,
    Vector,
    Region,
    TextRegion,
    FormulaRegion,
    CodeRegion,
    TableRegion,
    DiagramNode,
    DiagramEdge,
}

impl TutorVisualEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TutorVisualEntityKind::Point => "point",
            TutorVisualEntityKind::LineSegment => "line_segment",
            TutorVisualEntityKind::Ray => "ray",
            TutorVisualEntityKind::Angle => "angle",
            TutorVisualEntityKind::Polygon => "polygon",
            TutorVisualEntityKind::Circle => "circle",
            TutorVisualEntityKind::Arc => "arc",
            TutorVisualEntityKind::Axis => "axis",
            TutorVisualEntityKind::Vector => "vector",
            TutorVisualEntityKind::Region => "region",
            TutorVisualEntityKind::TextRegion => "text_region",
            TutorVisualEntityKind::FormulaRegion => "formula_region",
            TutorVisualEntityKind::CodeRegion => "code_region",
            TutorVisualEntityKind::TableRegion => "table_region",
            TutorVisualEntityKind::DiagramNode => "diagram_node",
            TutorVisualEntityKind::DiagramEdge => "diagram_edge",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorCoordinateSpace {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorVisualEntity {
    pub id: String,
    pub kind: TutorVisualEntityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub geometry: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorVisualEntityMap {
    pub observation_id: String,
    pub coordinate_space: TutorCoordinateSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<TutorVisualEntity>,
}

impl TutorVisualEntityMap {
    pub fn entity_ids(&self) -> HashSet<&str> {
        self.entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect()
    }

    pub fn compact_context(&self) -> String {
        let mut lines = vec![format!(
            "VisualEntityMap observation_id={} coordinate_space={}x{} entity_count={}",
            self.observation_id,
            self.coordinate_space.width,
            self.coordinate_space.height,
            self.entities.len()
        )];
        for entity in self.entities.iter().take(24) {
            let label = entity
                .label
                .as_deref()
                .or(entity.text.as_deref())
                .unwrap_or("");
            let confidence = entity
                .confidence
                .map(|value| format!(" confidence={value}"))
                .unwrap_or_default();
            let geometry = compact_json_value(&entity.geometry, 180);
            let evidence = if entity.source_evidence.is_empty() {
                String::new()
            } else {
                format!(
                    " evidence={}",
                    entity
                        .source_evidence
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            };
            lines.push(format!(
                "- {} kind={} label=\"{}\"{} geometry={}{}",
                entity.id,
                entity.kind.as_str(),
                label,
                confidence,
                geometry,
                evidence
            ));
        }
        if self.entities.len() > 24 {
            lines.push(format!(
                "- ... {} more entities omitted",
                self.entities.len() - 24
            ));
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorStep {
    pub kind: TutorStepKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_state: Option<String>,
    pub safety: TutorSafetyLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_entity_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_entity_map: Option<TutorVisualEntityMap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorSessionPlan {
    pub goal: String,
    pub steps: Vec<TutorStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorRunMode {
    /// Point, label, and explain. No app mutation.
    ExplainOnly,
    /// Point, explain, and perform user-requested reversible UI actions.
    GuidedAction,
    /// Create a temporary/demo object, explain it, and clean up only objects
    /// created inside the same tutor run.
    DemoAndCleanup,
    /// Explain a visible concept, diagram, formula, question, code sample, or
    /// paused frame using overlays and narration. No app mutation.
    ConceptExplainer,
    /// Work through a visible problem/question step by step. No app mutation.
    GuidedSolution,
    /// Demonstrate a concept with temporary overlays or mini-diagrams. No app
    /// mutation.
    ConceptDemo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorCanvasMode {
    ScreenOverlay,
    Blackboard,
}

/// One explicit voice request to enter a guided visual flow.
///
/// Voice uses a deliberately small command grammar rather than asking the
/// realtime model to guess whether words such as "this" authorize a screen
/// capture. The parser accepts the product names at the start of the utterance
/// (optionally after `hey`, `start`, `open`, `launch`, or `use`), normalizes
/// spoken `quick` into the canonical `#quick` flag, and makes the visual source
/// explicit:
///
/// - `Tutor [Quick] screen ...` captures the current display.
/// - `Tutor [Quick] blackboard ...` deliberately captures nothing.
/// - source-free Tutor defaults to blackboard.
/// - App Copilot is always screen-bound.
///
/// The canonical text begins with the trusted typed marker consumed by the
/// normal Tutor/App Copilot authorization path. Parsing is iterative and
/// bounded by transcript length; it performs no recursive descent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceGuidedFlowInvocation {
    pub feature_mode: crate::magician_v2::agents::FeatureMode,
    pub canvas_mode: TutorCanvasMode,
    pub quick: bool,
    pub canonical_text: String,
}

impl VoiceGuidedFlowInvocation {
    pub fn requires_screen_capture(&self) -> bool {
        self.canvas_mode == TutorCanvasMode::ScreenOverlay
    }
}

fn voice_command_requests_screen(tokens: &[VoiceCommandToken], start: usize) -> bool {
    let words = tokens
        .iter()
        .skip(start)
        .map(|token| token.normalized.as_str())
        .collect::<Vec<_>>();
    let Some(first) = words.first().copied() else {
        return false;
    };
    if matches!(
        first,
        "screen" | "screenshot" | "screencap" | "screen-capture" | "display"
    ) {
        return true;
    }
    if !matches!(first, "take" | "capture" | "use" | "share" | "show") {
        return false;
    }
    let source = if words.get(1).copied() == Some("a") {
        words.get(2).copied()
    } else {
        words.get(1).copied()
    };
    matches!(source, Some("screenshot" | "screen"))
}

/// Parse the explicit Web/live-voice guided-flow grammar and normalize it onto
/// the existing typed chat feature contract.
pub fn parse_voice_guided_flow_invocation(text: &str) -> Option<VoiceGuidedFlowInvocation> {
    use crate::magician_v2::agents::FeatureMode;

    let tokens = voice_command_tokens(text);
    let mut cursor = 0usize;
    let first = tokens.get(cursor)?.normalized.as_str();
    if matches!(first, "hey" | "start" | "open" | "launch" | "use") {
        cursor += 1;
    }

    let mut quick = false;
    if tokens
        .get(cursor)
        .is_some_and(|token| matches!(token.normalized.as_str(), "quick" | "#quick"))
    {
        quick = true;
        cursor += 1;
    }

    let feature_mode = match tokens.get(cursor)?.normalized.as_str() {
        "tutor" | "tutur" | "@tutor" | "@tutur" => {
            cursor += 1;
            FeatureMode::Tutor
        },
        "copilot" | "app-copilot" | "@copilot" | "@appcopilot" | "@app-copilot"
        | "@app_copilot" => {
            cursor += 1;
            FeatureMode::AppCopilot
        },
        "app"
            if tokens
                .get(cursor + 1)
                .is_some_and(|token| token.normalized == "copilot") =>
        {
            cursor += 2;
            FeatureMode::AppCopilot
        },
        _ => return None,
    };

    if tokens
        .get(cursor)
        .is_some_and(|token| matches!(token.normalized.as_str(), "quick" | "#quick"))
    {
        quick = true;
        cursor += 1;
    }
    let has_canonical_quick_flag = tokens
        .iter()
        .skip(cursor)
        .any(|token| token.normalized == TUTOR_QUICK_FLAG);
    quick |= has_canonical_quick_flag;

    // The source is an admission-bearing selector, not a topic classifier.
    // Only the first token after `Tutor [Quick]` may choose blackboard/screen;
    // words such as "my app" or "screenshot" later in the user's question
    // never silently authorize capture or override an earlier selector.
    let explicit_blackboard = tokens
        .get(cursor)
        .is_some_and(|token| token.normalized == "blackboard");
    let screen_requested = feature_mode == FeatureMode::AppCopilot
        || (!explicit_blackboard && voice_command_requests_screen(&tokens, cursor));
    let canvas_mode = if screen_requested {
        TutorCanvasMode::ScreenOverlay
    } else {
        TutorCanvasMode::Blackboard
    };

    let command_end = tokens
        .get(cursor.saturating_sub(1))
        .map(|token| token.end)
        .unwrap_or_else(|| tokens.first().map(|token| token.end).unwrap_or(0));
    let remainder = text
        .get(command_end..)
        .unwrap_or_default()
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';' | '-'))
        .trim();
    let marker = if feature_mode == FeatureMode::AppCopilot {
        "@copilot"
    } else {
        "@tutor"
    };
    let mut canonical_text = marker.to_string();
    if quick && !has_canonical_quick_flag {
        canonical_text.push_str(" #quick");
    }
    if !remainder.is_empty() {
        canonical_text.push(' ');
        canonical_text.push_str(remainder);
    }

    Some(VoiceGuidedFlowInvocation {
        feature_mode,
        canvas_mode,
        quick,
        canonical_text,
    })
}

impl Default for TutorCanvasMode {
    fn default() -> Self {
        Self::ScreenOverlay
    }
}

impl TutorCanvasMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TutorCanvasMode::ScreenOverlay => "screen_overlay",
            TutorCanvasMode::Blackboard => "blackboard",
        }
    }

    pub fn is_blackboard(self) -> bool {
        self == TutorCanvasMode::Blackboard
    }
}

pub fn parse_tutor_canvas_mode(raw: &str) -> Result<TutorCanvasMode, String> {
    match raw.trim() {
        "screen_overlay" => Ok(TutorCanvasMode::ScreenOverlay),
        "blackboard" => Ok(TutorCanvasMode::Blackboard),
        other => Err(format!(
            "unsupported tutor canvas_mode `{other}`; expected screen_overlay or blackboard"
        )),
    }
}

impl TutorRunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TutorRunMode::ExplainOnly => "explain_only",
            TutorRunMode::GuidedAction => "guided_action",
            TutorRunMode::DemoAndCleanup => "demo_and_cleanup",
            TutorRunMode::ConceptExplainer => "concept_explainer",
            TutorRunMode::GuidedSolution => "guided_solution",
            TutorRunMode::ConceptDemo => "concept_demo",
        }
    }

    pub fn is_concept_mode(self) -> bool {
        matches!(
            self,
            TutorRunMode::ConceptExplainer
                | TutorRunMode::GuidedSolution
                | TutorRunMode::ConceptDemo
        )
    }

    pub fn allows_ui_mutation(self) -> bool {
        matches!(
            self,
            TutorRunMode::GuidedAction | TutorRunMode::DemoAndCleanup
        )
    }
}

pub fn classify_tutor_canvas_mode(_text: &str, has_visual_source: bool) -> TutorCanvasMode {
    if has_visual_source {
        return TutorCanvasMode::ScreenOverlay;
    }
    TutorCanvasMode::Blackboard
}

pub fn classify_tutor_turn_mode(text: &str) -> TutorRunMode {
    let normalized = normalize_tutor_prompt_text(text);
    if normalized.is_empty() {
        return TutorRunMode::ExplainOnly;
    }
    if tutor_prompt_requests_guided_solution(&normalized) {
        return TutorRunMode::GuidedSolution;
    }
    if tutor_prompt_requests_concept_demo(&normalized) {
        return TutorRunMode::ConceptDemo;
    }
    if tutor_prompt_requests_concept_explainer(&normalized) {
        return TutorRunMode::ConceptExplainer;
    }
    if tutor_prompt_requests_strict_visual_only(&normalized) {
        return TutorRunMode::ExplainOnly;
    }
    TutorRunMode::ExplainOnly
}

pub fn classify_app_copilot_turn_mode(text: &str) -> TutorRunMode {
    let normalized = normalize_tutor_prompt_text(text);
    if tutor_prompt_requests_demo_cleanup(&normalized) {
        return TutorRunMode::DemoAndCleanup;
    }
    TutorRunMode::GuidedAction
}

pub fn classify_tutor_or_app_copilot_turn_mode(text: &str) -> TutorRunMode {
    classify_tutor_or_app_copilot_turn_mode_for_lane(text, is_app_copilot_prompt(text))
}

/// Classify a turn after the product route has already authenticated its lane.
/// The boolean is policy-bearing; `text` is intent content only and cannot
/// switch a Tutor turn into App Copilot (or vice versa).
pub fn classify_tutor_or_app_copilot_turn_mode_for_lane(
    text: &str,
    app_copilot_lane: bool,
) -> TutorRunMode {
    if app_copilot_lane {
        classify_app_copilot_turn_mode(text)
    } else {
        classify_tutor_turn_mode(text)
    }
}

pub fn classify_tutor_or_app_copilot_canvas_mode(
    text: &str,
    has_visual_source: bool,
) -> TutorCanvasMode {
    classify_tutor_or_app_copilot_canvas_mode_for_lane(
        text,
        has_visual_source,
        is_app_copilot_prompt(text),
    )
}

/// Resolve the canvas from an authenticated lane rather than a repeated
/// substring check. App Copilot is always screen-bound; Tutor may use the
/// blackboard when no visual source is present.
pub fn classify_tutor_or_app_copilot_canvas_mode_for_lane(
    text: &str,
    has_visual_source: bool,
    app_copilot_lane: bool,
) -> TutorCanvasMode {
    if app_copilot_lane {
        TutorCanvasMode::ScreenOverlay
    } else {
        classify_tutor_canvas_mode(text, has_visual_source)
    }
}

pub fn tutor_or_copilot_turn_guidance(text: &str) -> &'static str {
    tutor_or_copilot_turn_guidance_for_lane(text, is_app_copilot_prompt(text))
}

/// Render lane guidance after the server has authenticated the typed product
/// route. This keeps legacy prompt parsing as a compatibility adapter only;
/// downstream feature behavior is selected by the typed lane.
pub fn tutor_or_copilot_turn_guidance_for_lane(text: &str, app_copilot_lane: bool) -> &'static str {
    if app_copilot_lane {
        return match classify_app_copilot_turn_mode(text) {
            TutorRunMode::DemoAndCleanup => {
                "## App Copilot Turn Intent\nThis prompt invokes App Copilot for a hybrid demo flow with cleanup. Start `start_tutor_run` with `mode=\"demo_and_cleanup\"` and `canvas_mode=\"screen_overlay\"`. Track every object created with `record_tutor_created_object`. For each step: observe, resolve, draw/narrate the target, call `check_for_copilot_user_action` once without waiting, and if no user action is already present immediately delegate the reversible/session-owned action to `mac-operator` with `tutor_action` that includes the preceding screen-draw `storyboard_step_id` or `storyboard_step_label`. If the user acts while automation is queued or running, backend preemption cancels the delegated execution when possible. Observe/verify after user action or automation. Mac-operator chooses the cheapest correct engine: script/JXA for scriptable apps, CUA for non-scriptable or coordinate-bound UI."
            },
            TutorRunMode::ExplainOnly => {
                "## App Copilot Turn Intent\nThis prompt invoked App Copilot but was classified visual-only. Prefer `@tutor` for explanation-only teaching. Start `start_tutor_run` with `mode=\"explain_only\"` and `canvas_mode=\"screen_overlay\"`, draw/label/explain, and do not mutate the app."
            },
            _ => {
                "## App Copilot Turn Intent\nThis prompt invokes App Copilot for hybrid app mutation. Start `start_tutor_run` with `mode=\"guided_action\"` and `canvas_mode=\"screen_overlay\"`. For each UI-changing step: observe, resolve, draw/narrate the target, call `check_for_copilot_user_action` once without waiting, and if it returns `no_user_action` immediately delegate the reversible UI action to `mac-operator` with `tutor_action` that includes the preceding screen-draw `storyboard_step_id` or `storyboard_step_label`. If it returns `user_acted`, do not automate that step; observe/verify and continue. If the user acts while automation is queued or running, backend preemption cancels the delegated execution when possible. Do not stop after only drawing. Mac-operator chooses the cheapest correct engine: script/JXA for scriptable apps, CUA for non-scriptable or coordinate-bound UI."
            },
        };
    }

    match classify_tutor_turn_mode(text) {
        TutorRunMode::GuidedAction => {
            "## Personal Tutor Turn Intent\nThis tutor prompt is action-oriented. Start `start_tutor_run` with `mode=\"guided_action\"`. After each visual highlight, delegate the corresponding reversible UI action to `mac-operator` with `tutor_action`, then observe/verify before continuing. Do not stop after only drawing."
        },
        TutorRunMode::DemoAndCleanup => {
            "## Personal Tutor Turn Intent\nThis tutor prompt asks for a demo flow with cleanup. Start `start_tutor_run` with `mode=\"demo_and_cleanup\"`. Track every object created with `record_tutor_created_object`, draw before acting, delegate reversible/session-owned actions to `mac-operator` with `tutor_action`, then observe/verify after each action."
        },
        TutorRunMode::ConceptExplainer => {
            "## Personal Tutor Turn Intent\nThis tutor prompt asks for live concept explanation over the current screen. Start `start_tutor_run` with `mode=\"concept_explainer\"`. Treat the visible screen as the canvas, identify the relevant math/physics/computer-science entities, include a `visual_entity_map` on observe when possible, cite `source_entity_ids` on resolve/draw steps, draw/label/explain progressively, and do not click/type/mutate the underlying app."
        },
        TutorRunMode::GuidedSolution => {
            "## Personal Tutor Turn Intent\nThis tutor prompt asks to solve or work through a visible problem step by step. Start `start_tutor_run` with `mode=\"guided_solution\"`. Use the current screen as the source of truth, include a `visual_entity_map` on observe when possible, cite `source_entity_ids` on resolve/draw steps, reveal reasoning progressively with overlays and narration, and do not click/type/mutate the underlying app."
        },
        TutorRunMode::ConceptDemo => {
            "## Personal Tutor Turn Intent\nThis tutor prompt asks for a visual concept demonstration. Start `start_tutor_run` with `mode=\"concept_demo\"`. Use temporary overlays or mini-diagram marks on the visible screen, include a `visual_entity_map` on observe when possible, cite `source_entity_ids` on resolve/draw steps, explain each reveal, and do not click/type/mutate the underlying app."
        },
        TutorRunMode::ExplainOnly => "## Personal Tutor Turn Intent\nThis tutor prompt is concept/explanation-oriented. If there is no visual source attached to the turn, continue the blackboard tutor run and create a synthetic teaching diagram in overlay/model coordinates. If a screenshot/crop/PDF/image/video frame is attached, use screen_overlay concept tutoring. Do not click, type, scroll, or mutate the app.",
    }
}

fn normalize_tutor_prompt_text(text: &str) -> String {
    text.to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn tutor_prompt_requests_strict_visual_only(normalized: &str) -> bool {
    let strict_visual_only_phrases = [
        "where is",
        "where do i",
        "point to",
        "highlight",
        "show me where",
        "tell me where",
        "what is",
        "what does",
        "only show",
        "only point",
        "do not click",
        "don t click",
        "dont click",
        "don t type",
        "dont type",
        "without clicking",
        "without typing",
    ];
    strict_visual_only_phrases
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn tutor_prompt_requests_guided_solution(normalized: &str) -> bool {
    if !contains_concept_domain_signal(normalized) {
        return false;
    }
    let solution_phrases = [
        "solve",
        "solve this",
        "work through",
        "walk through",
        "step by step",
        "answer this",
        "question paper",
        "practice problem",
        "problem",
        "equation",
        "derivation",
    ];
    solution_phrases
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn tutor_prompt_requests_concept_demo(normalized: &str) -> bool {
    if !contains_concept_domain_signal(normalized) {
        return false;
    }
    let demo_phrases = [
        "demonstrate",
        "demo",
        "show why",
        "prove",
        "proof",
        "intuition",
        "visualize",
        "shade region",
        "annotate graph",
        "show transformation",
        "draw rectangles",
        "show the idea",
    ];
    demo_phrases
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn tutor_prompt_requests_concept_explainer(normalized: &str) -> bool {
    if !contains_concept_domain_signal(normalized) {
        return false;
    }
    let explainer_phrases = [
        "explain",
        "explain how",
        "explain why",
        "how this",
        "how that",
        "how the",
        "show me how this",
        "show me how that",
        "show me how the",
        "why",
        "works",
        "working",
        "doing",
        "what means",
        "what does",
        "understand",
        "teach me",
        "concept",
        "diagram",
        "formula",
        "theorem",
        "code",
        "screenshot",
        "paused frame",
    ];
    explainer_phrases
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn contains_concept_domain_signal(normalized: &str) -> bool {
    let domain_terms = [
        // Mathematics / geometry.
        "math",
        "mathematics",
        "geometry",
        "triangle",
        "theorem",
        "proof",
        "angle",
        "side",
        "area",
        "square",
        "rectangle",
        "circle",
        "algebra",
        "equation",
        "formula",
        "graph",
        "coordinate",
        "calculus",
        "derivative",
        "integral",
        "matrix",
        // Physics.
        "physics",
        "force",
        "free body",
        "vector",
        "velocity",
        "acceleration",
        "motion",
        "trajectory",
        "energy",
        "momentum",
        "circuit",
        "field",
        "unit",
        // Computer science.
        "computer science",
        "cs",
        "code",
        "program",
        "algorithm",
        "recursion",
        "stack",
        "heap",
        "pointer",
        "reference",
        "async",
        "event loop",
        "data structure",
        "tree",
        "graph",
        "complexity",
        "big o",
        // Screen-visible learning artifacts.
        "question",
        "question paper",
        "worksheet",
        "diagram",
        "slide",
        "whiteboard",
        "screenshot",
        "pdf",
        "image",
        "video",
        "paused",
    ];
    domain_terms.iter().any(|term| normalized.contains(term))
}

// `tutor_prompt_requests_demo_cleanup` moved to `copilot_rail` with the
// demo-and-cleanup guard that reads it.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorRunStatus {
    Planned,
    Running,
    WaitingForConfirmation,
    Completed,
    Failed,
}

impl TutorRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TutorRunStatus::Planned => "planned",
            TutorRunStatus::Running => "running",
            TutorRunStatus::WaitingForConfirmation => "waiting_for_confirmation",
            TutorRunStatus::Completed => "completed",
            TutorRunStatus::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorStepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    WaitingForConfirmation,
    Skipped,
}

impl TutorStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TutorStepStatus::Pending => "pending",
            TutorStepStatus::Running => "running",
            TutorStepStatus::Succeeded => "succeeded",
            TutorStepStatus::Failed => "failed",
            TutorStepStatus::WaitingForConfirmation => "waiting_for_confirmation",
            TutorStepStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorCreatedObject {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorStepRecord {
    pub step: TutorStep,
    pub status: TutorStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storyboard_steps: Vec<TutorDrawStoryboardStep>,
}

/// Runtime-enforced semantic depth for a Personal Tutor lesson. Progressive
/// Tutor treats this as a minimum floor and may plan more objectives. Tutor
/// Quick uses the same depth signal to choose a smaller condensed range. App
/// Copilot deliberately does not receive a lesson contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorLessonDepth {
    Focused,
    Standard,
    Deep,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorLessonPacing {
    #[default]
    Progressive,
    Condensed,
}

/// What a milestone is FOR, so depth can be spent where the difficulty is.
///
/// Without this the runtime could count milestones but not weigh them: a deep
/// lesson could satisfy every rule with five evenly-sized steps and still skip
/// the one thing the learner is stuck on. `Core` marks that thing — the
/// transformation, the non-obvious substitution, the step whose reason is not
/// visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorMilestoneRole {
    /// Definitions, restating the goal, labelling the figure. Cheap.
    Setup,
    /// The hard part. A deep lesson must decompose this across several
    /// milestones rather than asserting it in one.
    Core,
    /// Result, check, worked example. Also cheap.
    Conclusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorLessonMilestone {
    pub milestone_id: String,
    pub objective: String,
    /// Absent on plans authored before roles existed, and on lessons with no
    /// depth requirement. Absent is NOT `Setup` — an unroled plan simply has
    /// not answered the question, which is why deep lessons gate on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<TutorMilestoneRole>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorLessonContract {
    pub depth: TutorLessonDepth,
    #[serde(default)]
    pub pacing: TutorLessonPacing,
    pub minimum_storyboard_steps: usize,
    /// Tutor Quick only: the maximum number of essential milestones in its
    /// condensed lesson. Progressive Tutor remains open-ended (subject to the
    /// broad per-run safety cap) because concept complexity determines length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_storyboard_steps: Option<usize>,
    /// Concept-specific teaching milestones selected by the Tutor model. The
    /// plan is empty at deterministic run preflight and is attached atomically
    /// to the first successful Personal Tutor draw. Later revisions may add
    /// milestones but cannot silently remove already planned lesson scope or
    /// exceed a condensed Quick maximum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub milestones: Vec<TutorLessonMilestone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorLessonProgress {
    pub depth: TutorLessonDepth,
    pub pacing: TutorLessonPacing,
    pub minimum_storyboard_steps: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_storyboard_steps: Option<usize>,
    pub plan_ready: bool,
    pub planned_milestones: usize,
    pub completed_storyboard_steps: usize,
    pub remaining_storyboard_steps: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_milestone_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining_milestones: Vec<TutorLessonMilestone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorActionEnvelope {
    pub run_id: String,
    pub step_kind: TutorStepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_label: Option<String>,
    pub target: String,
    pub expected_state: String,
    pub safety: TutorSafetyLevel,
    pub observation_evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_instruction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_object: Option<TutorCreatedObject>,
}

impl TutorActionEnvelope {
    pub fn to_step(&self) -> TutorStep {
        TutorStep {
            kind: self.step_kind,
            label: self
                .action_instruction
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| tutor_step_kind_name(self.step_kind))
                .to_string(),
            target: Some(self.target.clone()),
            expected_state: Some(self.expected_state.clone()),
            safety: self.safety,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorActionResultStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorActionResult {
    pub run_id: String,
    pub status: TutorActionResultStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorActionResultApplication {
    pub run_id: String,
    pub applied: bool,
    pub status: TutorActionResultStatus,
    pub note: String,
    pub run: Option<TutorRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorUserActionEvent {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub evidence: String,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorUserActionRecordResult {
    pub event: TutorUserActionEvent,
    #[serde(default)]
    pub applied_to_pending_action: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preempted_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TutorRun>,
}

/// Terminal result returned when a scoped tutor run is explicitly cancelled.
/// The delegated execution id is captured before the run is failed so callers
/// can best-effort preempt any still-running mac-operator work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorRunCancellation {
    pub run: TutorRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preempted_execution_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorGuidedContinuation {
    pub run_id: String,
    pub mode: TutorRunMode,
    pub reason: String,
    pub instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorToolLoopContinuation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub mode: TutorRunMode,
    pub reason: String,
    pub instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorRun {
    pub run_id: String,
    pub mode: TutorRunMode,
    #[serde(default)]
    pub canvas_mode: TutorCanvasMode,
    pub status: TutorRunStatus,
    pub goal: String,
    /// Stable product rail. This survives delegated task boundaries and keeps
    /// App Copilot continuations from being reinterpreted as Tutor turns.
    #[serde(default)]
    pub product_rail: TutorProductRail,
    /// Whether the initiating product request opted into the condensed quick
    /// path. Retained for App Copilot background continuations.
    #[serde(default)]
    pub quick: bool,
    /// Completion contract for a Personal Tutor lesson. Normal Tutor uses
    /// progressive pacing, Tutor Quick uses condensed pacing, and App Copilot
    /// deliberately keeps its independent action-completion policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson_contract: Option<TutorLessonContract>,
    #[serde(default)]
    pub step_history: Vec<TutorStepRecord>,
    #[serde(default)]
    pub created_objects: Vec<TutorCreatedObject>,
    #[serde(default)]
    pub has_fresh_observation: bool,
    #[serde(default)]
    pub has_resolved_target: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_visual_entity_map: Option<TutorVisualEntityMap>,
    #[serde(default)]
    pub pending_action: Option<TutorStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_action_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_created_object: Option<TutorCreatedObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_created_object_labels: Vec<String>,
    /// Server-minted proof that the immediately preceding App Copilot
    /// storyboard step was checked for a manual user action. It is consumed
    /// atomically when automation is admitted and invalidated by visual-state
    /// changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot_action_check: Option<CopilotActionCheckReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub destructive_confirmed: bool,
    pub retry_count: u8,
    pub max_retries: u8,
    /// Bounded Thinking Map digest attached when the owner registered a
    /// map→tutor binding for this chat session before the run started
    /// (`POST /thinking-maps/{id}/tutor-context` → the
    /// [`crate::magician_v2::thinking_map::tutor_context`] registry, consumed
    /// by [`TutorRunStore::start_run`]). REFERENCE CONTEXT ONLY — the tutor
    /// keeps full ownership of narration/storyboard, and the digest preserves
    /// assertion origins (model-inferred content is tagged `[AI-suggested]`).
    /// `None` (and skipped on the wire) for every run without a binding, so
    /// all pre-existing tutor flows serialize byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_map_context: Option<String>,
}

impl TutorRun {
    pub fn new_incremental(
        run_id: impl Into<String>,
        mode: TutorRunMode,
        goal: impl Into<String>,
    ) -> Result<Self, String> {
        Self::new_incremental_with_canvas(run_id, mode, goal, TutorCanvasMode::ScreenOverlay)
    }

    pub fn new_incremental_with_canvas(
        run_id: impl Into<String>,
        mode: TutorRunMode,
        goal: impl Into<String>,
        canvas_mode: TutorCanvasMode,
    ) -> Result<Self, String> {
        let goal = goal.into();
        if goal.trim().is_empty() {
            return Err("tutor run goal cannot be empty".to_string());
        }
        let lesson_contract = personal_tutor_lesson_contract(mode, &goal);
        let product_rail = if is_app_copilot_prompt(&goal) {
            TutorProductRail::AppCopilot
        } else {
            TutorProductRail::PersonalTutor
        };
        let quick = has_tutor_quick_flag(&goal);
        Ok(Self {
            run_id: run_id.into(),
            mode,
            canvas_mode,
            status: TutorRunStatus::Running,
            goal,
            product_rail,
            quick,
            lesson_contract,
            step_history: Vec::new(),
            created_objects: Vec::new(),
            has_fresh_observation: false,
            has_resolved_target: false,
            latest_visual_entity_map: None,
            pending_action: None,
            pending_action_execution_id: None,
            pending_created_object: None,
            removed_created_object_labels: Vec::new(),
            copilot_action_check: None,
            terminal_reason: None,
            destructive_confirmed: false,
            retry_count: 0,
            max_retries: 2,
            thinking_map_context: None,
        })
    }

    pub fn current_step(&self) -> Option<&TutorStep> {
        self.step_history.last().map(|record| &record.step)
    }

    // `is_app_copilot` and `has_matching_copilot_action_check` live in the
    // `copilot_rail` impl block: they exist only because a run can ride the
    // App Copilot rail.

    pub fn has_successful_ui_action(&self) -> bool {
        self.step_history.iter().any(|record| {
            record.status == TutorStepStatus::Succeeded && record.step.kind.changes_ui_state()
        })
    }

    /// A completed visual lesson must still have storyboard-bound drawing
    /// evidence after the most recent successful overlay clear. Earlier steps
    /// remain useful for semantic coverage, but cannot prove that the final
    /// overlay is visible/replayable after it was explicitly removed.
    pub fn has_visible_draw_storyboard_evidence(&self) -> bool {
        let after_latest_clear = self
            .step_history
            .iter()
            .rposition(|record| {
                record.status == TutorStepStatus::Succeeded
                    && record.step.kind == TutorStepKind::ClearDrawings
            })
            .map(|index| index.saturating_add(1))
            .unwrap_or(0);
        self.step_history
            .iter()
            .skip(after_latest_clear)
            .any(|record| {
                record.status == TutorStepStatus::Succeeded
                    && record.step.kind == TutorStepKind::Draw
                    && record
                        .storyboard_steps
                        .iter()
                        .any(|step| step.figure_backed)
            })
    }

    /// Count distinct semantic storyboard ids, not draw calls or drawable
    /// primitives. Children inheriting one group-level storyboard id remain
    /// one narrated teaching step; independently identified reveal steps count
    /// separately even when delivered in one renderer call. Once an adaptive
    /// lesson plan is present, only storyboard ids matching its milestones
    /// satisfy the contract; extra examples remain valid but do not silently
    /// replace planned teaching objectives.
    pub fn lesson_progress(&self) -> Option<TutorLessonProgress> {
        let contract = self.lesson_contract.as_ref()?;
        let mut seen_step_ids = HashSet::new();
        let mut seen_narrations = HashSet::new();
        for step in self
            .step_history
            .iter()
            .filter(|record| {
                record.status == TutorStepStatus::Succeeded
                    && record.step.kind == TutorStepKind::Draw
            })
            .flat_map(|record| record.storyboard_steps.iter())
            // Text-only steps still render and still narrate; they just do not
            // satisfy a milestone. This is the same rule a `say` step already
            // lived under, applied to a draw that happens to be all words.
            .filter(|step| step.figure_backed)
        {
            // IDs are protocol identities, so casing and punctuation are not
            // meaningful distinctions. Narration is the semantic evidence:
            // changing only an id/label around the same spoken explanation
            // must not manufacture another lesson step.
            let normalized_step_id = normalize_tutor_lesson_coverage_text(&step.step_id);
            let normalized_narration = normalize_tutor_lesson_coverage_text(&step.narration);
            if normalized_step_id.is_empty()
                || normalized_narration.is_empty()
                || seen_step_ids.contains(&normalized_step_id)
                || seen_narrations.contains(&normalized_narration)
            {
                continue;
            }
            seen_step_ids.insert(normalized_step_id);
            seen_narrations.insert(normalized_narration);
        }

        let plan_ready = contract.milestones.len() >= contract.minimum_storyboard_steps
            && lesson_plan_spends_depth_where_it_is_hard(contract);
        let (completed_milestone_ids, remaining_milestones) = if plan_ready {
            contract
                .milestones
                .iter()
                .cloned()
                .partition::<Vec<_>, _>(|milestone| {
                    seen_step_ids.contains(&normalize_tutor_lesson_coverage_text(
                        &milestone.milestone_id,
                    ))
                })
        } else {
            (Vec::new(), Vec::new())
        };
        let completed_storyboard_steps = if plan_ready {
            completed_milestone_ids.len()
        } else {
            seen_step_ids.len()
        };
        let required_storyboard_steps = if plan_ready {
            contract.milestones.len()
        } else {
            contract.minimum_storyboard_steps
        };
        Some(TutorLessonProgress {
            depth: contract.depth,
            pacing: contract.pacing,
            minimum_storyboard_steps: contract.minimum_storyboard_steps,
            maximum_storyboard_steps: contract.maximum_storyboard_steps,
            plan_ready,
            planned_milestones: contract.milestones.len(),
            completed_storyboard_steps,
            remaining_storyboard_steps: required_storyboard_steps
                .saturating_sub(completed_storyboard_steps),
            completed_milestone_ids: completed_milestone_ids
                .into_iter()
                .map(|milestone| milestone.milestone_id)
                .collect(),
            remaining_milestones,
        })
    }

    pub fn lesson_contract_is_satisfied(&self) -> bool {
        self.lesson_progress()
            .map(|progress| progress.plan_ready && progress.remaining_storyboard_steps == 0)
            .unwrap_or(true)
    }

    /// Install the adaptive plan selected for this Personal Tutor lesson. A
    /// progressive plan may contain any concept-appropriate number above its
    /// floor; a Quick plan must fit its condensed range. Revisions are
    /// append-only so a model cannot make an unfinished lesson appear complete
    /// by deleting prior scope.
    pub fn set_or_expand_lesson_milestones(
        &mut self,
        milestones: Vec<TutorLessonMilestone>,
    ) -> Result<(), String> {
        if self.status.is_terminal() {
            return Err(format!(
                "cannot update tutor lesson plan while run is {:?}",
                self.status
            ));
        }
        let Some(contract) = self.lesson_contract.as_mut() else {
            return Err(
                "adaptive lesson milestones apply only to Personal Tutor runs, not App Copilot"
                    .to_string(),
            );
        };
        let milestones = validate_tutor_lesson_milestones(
            milestones,
            contract.minimum_storyboard_steps,
            contract.maximum_storyboard_steps,
        )?;
        let preserves_existing_scope = milestones.len() >= contract.milestones.len()
            && milestones
                .iter()
                .zip(contract.milestones.iter())
                .all(|(next, existing)| {
                    normalize_tutor_lesson_coverage_text(&next.milestone_id)
                        == normalize_tutor_lesson_coverage_text(&existing.milestone_id)
                        && normalize_tutor_lesson_coverage_text(&next.objective)
                            == normalize_tutor_lesson_coverage_text(&existing.objective)
                });
        if !preserves_existing_scope {
            return Err(
                "Personal Tutor lesson-plan revisions must preserve every existing milestone in order and may only append newly discovered teaching objectives"
                    .to_string(),
            );
        }
        let existing_len = contract.milestones.len();
        contract
            .milestones
            .extend(milestones.into_iter().skip(existing_len));
        Ok(())
    }

    /// Return the latest unresolved, explicitly recorded step failure. A later
    /// successful non-speech step is recovery evidence and clears the blocker;
    /// a text-only Say/Wait record must not hide it.
    pub fn terminal_blocker_reason(&self) -> Option<String> {
        let record =
            self.step_history.iter().rev().find(|record| {
                !matches!(record.step.kind, TutorStepKind::Say | TutorStepKind::Wait)
            })?;
        if record.status != TutorStepStatus::Failed {
            return None;
        }
        record
            .step
            .expected_state
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .or_else(|| {
                let label = record.step.label.trim();
                (!label.is_empty()).then_some(label)
            })
            .map(ToOwned::to_owned)
    }

    pub fn visual_entity_prompt_context(&self) -> Option<String> {
        if !self.has_fresh_observation {
            return None;
        }
        self.latest_visual_entity_map
            .as_ref()
            .map(TutorVisualEntityMap::compact_context)
    }

    /// The bounded Thinking Map grounding digest attached at run start, if the
    /// owner registered a map→tutor binding for this chat session. Reference
    /// context only — never a step directive.
    pub fn thinking_map_prompt_context(&self) -> Option<&str> {
        self.thinking_map_context.as_deref()
    }

    pub fn propose_step(&mut self, step: TutorStep) -> Result<(), String> {
        self.propose_step_with_storyboard_steps(step, Vec::new())
    }

    pub fn propose_step_with_storyboard_steps(
        &mut self,
        step: TutorStep,
        storyboard_steps: Vec<TutorDrawStoryboardStep>,
    ) -> Result<(), String> {
        if self.status.is_terminal() {
            return Err(format!(
                "cannot accept tutor step while run is {:?}",
                self.status
            ));
        }
        validate_next_tutor_step(self, &step)?;
        if step.kind == TutorStepKind::Draw {
            validate_storyboard_steps_against_lesson_plan(self, &storyboard_steps)?;
        }
        if step.kind == TutorStepKind::Confirm {
            self.destructive_confirmed = true;
        }
        if !matches!(step.kind, TutorStepKind::Say | TutorStepKind::Wait) {
            self.copilot_action_check = None;
        }
        self.status = TutorRunStatus::Running;
        apply_step_state_transition(self, &step);
        let storyboard_steps = if step.kind == TutorStepKind::Draw {
            storyboard_steps
        } else {
            Vec::new()
        };
        self.step_history.push(TutorStepRecord {
            step,
            status: TutorStepStatus::Succeeded,
            storyboard_steps,
        });
        self.retry_count = 0;
        Ok(())
    }

    pub fn complete(&mut self) -> Result<(), String> {
        if self.status.is_terminal() {
            return Err(format!(
                "cannot complete tutor run while it is {:?}",
                self.status
            ));
        }
        if let Some(pending_action) = self.pending_action.as_ref() {
            return Err(format!(
                "cannot complete tutor run before verifying `{}`",
                pending_action.label
            ));
        }
        if let Some(blocker) = self.terminal_blocker_reason() {
            return Err(format!(
                "cannot complete tutor run with an unresolved recorded failure: {blocker}. Recover with a successful runtime step or fail the run"
            ));
        }
        // The App Copilot demo-and-cleanup guard lives in `copilot_rail`;
        // `None` frees this shared completion path to continue.
        if let Some(error) = self.app_copilot_demo_cleanup_completion_error() {
            return Err(error);
        }
        if !self.has_successful_ui_action() && !self.has_visible_draw_storyboard_evidence() {
            return Err(
                "cannot complete tutor run without a successful storyboard-bound draw after the latest overlay clear"
                    .to_string(),
            );
        }
        if let Some(progress) = self
            .lesson_progress()
            .filter(|progress| !progress.plan_ready || progress.remaining_storyboard_steps > 0)
        {
            let lesson_label = match progress.pacing {
                TutorLessonPacing::Progressive => "normal Tutor progressive lesson",
                TutorLessonPacing::Condensed => "Tutor Quick condensed lesson",
            };
            if !progress.plan_ready {
                return Err(format!(
                    "{lesson_label} is incomplete: an adaptive milestone plan with at least {} substantive objective(s) must be attached to the first successful draw before completing the run",
                    progress.minimum_storyboard_steps,
                ));
            }
            return Err(format!(
                "{lesson_label} is incomplete: {} of {} planned narrated visual milestones are complete; continue with {} substantive milestone(s) before completing the run. A Say step, unplanned storyboard id, or repeated storyboard id does not satisfy visual lesson coverage",
                progress.completed_storyboard_steps,
                progress.planned_milestones,
                progress.remaining_storyboard_steps,
            ));
        }
        self.status = TutorRunStatus::Completed;
        self.terminal_reason = None;
        Ok(())
    }

    pub fn fail(&mut self) {
        self.fail_with_reason(None);
    }

    pub fn fail_with_reason(&mut self, reason: Option<String>) {
        self.status = TutorRunStatus::Failed;
        self.terminal_reason = reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(ToOwned::to_owned);
        self.pending_action = None;
        self.pending_action_execution_id = None;
        self.pending_created_object = None;
        self.copilot_action_check = None;
    }

    pub fn record_step_failure(&mut self, step: TutorStep) {
        self.retry_count = self.retry_count.saturating_add(1);
        if step.kind == TutorStepKind::Observe || step.kind.changes_ui_state() {
            self.has_fresh_observation = false;
            self.has_resolved_target = false;
            self.latest_visual_entity_map = None;
        }
        self.step_history.push(TutorStepRecord {
            step,
            status: TutorStepStatus::Failed,
            storyboard_steps: Vec::new(),
        });
        if self.retry_count > self.max_retries {
            self.status = TutorRunStatus::Failed;
            self.terminal_reason = Some("tutor retry limit exceeded".to_string());
        } else {
            self.status = TutorRunStatus::Running;
        }
    }

    pub fn record_action_result_failure(&mut self, evidence: impl Into<String>) {
        let pending = self.pending_action.take();
        self.pending_action_execution_id = None;
        self.pending_created_object = None;
        let label = pending
            .as_ref()
            .map(|step| format!("verify failed for {}", step.label))
            .unwrap_or_else(|| "verify delegated tutor action failed".to_string());
        let target = pending.as_ref().and_then(|step| step.target.clone());
        let step = TutorStep {
            kind: TutorStepKind::Verify,
            label,
            target,
            expected_state: Some(evidence.into()),
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        };
        self.record_step_failure(step);
        self.has_fresh_observation = false;
        self.has_resolved_target = false;
        self.latest_visual_entity_map = None;
        self.destructive_confirmed = false;
    }

    pub fn record_created_object(&mut self, object: TutorCreatedObject) {
        if !self
            .created_objects
            .iter()
            .any(|candidate| candidate.label == object.label)
        {
            self.created_objects.push(object);
        }
    }

    pub fn has_successful_draw_storyboard_binding(
        &self,
        storyboard_step_id: Option<&str>,
        storyboard_step_label: Option<&str>,
    ) -> bool {
        let storyboard_step_id = storyboard_step_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let storyboard_step_label = storyboard_step_label
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if storyboard_step_id.is_none() && storyboard_step_label.is_none() {
            return false;
        }

        let latest_resolve_index = self
            .step_history
            .iter()
            .rposition(|record| {
                record.status == TutorStepStatus::Succeeded
                    && record.step.kind == TutorStepKind::ResolveTarget
            })
            .map(|index| index + 1)
            .unwrap_or(0);

        self.step_history
            .iter()
            .skip(latest_resolve_index)
            .any(|record| {
                record.status == TutorStepStatus::Succeeded
                    && record.step.kind == TutorStepKind::Draw
                    && record.storyboard_steps.iter().any(|step| {
                        storyboard_step_id
                            .map(|expected| step.step_id == expected)
                            .unwrap_or(false)
                            || storyboard_step_label
                                .map(|expected| step.label == expected)
                                .unwrap_or(false)
                    })
            })
    }

    pub fn owns_object_label(&self, label: &str) -> bool {
        let label = label.trim().to_ascii_lowercase();
        !label.is_empty()
            && self.created_objects.iter().any(|object| {
                object.label.trim().eq_ignore_ascii_case(&label)
                    || object
                        .evidence
                        .as_deref()
                        .map(|evidence| evidence.to_ascii_lowercase().contains(&label))
                        .unwrap_or(false)
            })
    }
}

fn personal_tutor_lesson_contract(mode: TutorRunMode, goal: &str) -> Option<TutorLessonContract> {
    if is_app_copilot_prompt(goal)
        || matches!(
            mode,
            TutorRunMode::GuidedAction | TutorRunMode::DemoAndCleanup
        )
    {
        return None;
    }

    let normalized = normalize_tutor_prompt_text(goal);
    let deep_request = tutor_goal_requests_deep_lesson(&normalized);
    let focused_request = !deep_request && tutor_goal_requests_focused_lesson(&normalized);
    let depth = if focused_request {
        TutorLessonDepth::Focused
    } else if deep_request || mode.is_concept_mode() {
        TutorLessonDepth::Deep
    } else {
        TutorLessonDepth::Standard
    };
    let quick = has_tutor_quick_flag(goal);
    let (pacing, minimum_storyboard_steps, maximum_storyboard_steps) = if quick {
        match depth {
            TutorLessonDepth::Focused => (TutorLessonPacing::Condensed, 1, Some(1)),
            TutorLessonDepth::Standard => (TutorLessonPacing::Condensed, 1, Some(2)),
            // Deep was capped at 4, which cannot hold a transformation. The
            // step a learner actually gets stuck on — unrolling a surface,
            // rearranging a region — needs before / the cut / mid / after plus
            // the quantity preserved across it, and that is five before any
            // setup or conclusion. Quick stays roughly half a progressive
            // lesson; it just stops being too short to teach the hard part,
            // which was the only part worth condensing around.
            TutorLessonDepth::Deep => (TutorLessonPacing::Condensed, 2, Some(6)),
        }
    } else {
        let minimum = match depth {
            TutorLessonDepth::Focused => 1,
            TutorLessonDepth::Standard => 2,
            TutorLessonDepth::Deep => 3,
        };
        (TutorLessonPacing::Progressive, minimum, None)
    };
    Some(TutorLessonContract {
        depth,
        pacing,
        minimum_storyboard_steps,
        maximum_storyboard_steps,
        milestones: Vec::new(),
    })
}

/// Whether a plan concentrates its milestones on the hard part.
///
/// Only deep lessons are gated, and the bar is deliberately structural rather
/// than a judgement about content: at least two `Core` milestones, so the hard
/// step is a SEQUENCE and not a single assertion, and at least as many `Core`
/// as setup and conclusion combined, so the lesson is not mostly scaffolding.
///
/// This is a `plan_ready` condition, not a rejection. A plan that fails it
/// still renders; the run simply is not finishable until the model revises it,
/// and revisions may add roles because scope preservation compares only
/// `milestone_id` and `objective`. Rejecting outright would risk a retry loop
/// against a bounded `max_retries`; withholding readiness is the same feedback
/// channel the minimum floor already uses.
fn lesson_plan_spends_depth_where_it_is_hard(contract: &TutorLessonContract) -> bool {
    if contract.depth != TutorLessonDepth::Deep {
        return true;
    }
    let mut core = 0usize;
    let mut scaffolding = 0usize;
    for milestone in &contract.milestones {
        match milestone.role {
            Some(TutorMilestoneRole::Core) => core += 1,
            // Unroled counts as scaffolding on purpose: an unanswered question
            // must not read as "the hard part is covered".
            _ => scaffolding += 1,
        }
    }
    core >= 2 && core >= scaffolding
}

fn validate_tutor_lesson_milestones(
    milestones: Vec<TutorLessonMilestone>,
    minimum_storyboard_steps: usize,
    maximum_storyboard_steps: Option<usize>,
) -> Result<Vec<TutorLessonMilestone>, String> {
    if milestones.len() < minimum_storyboard_steps {
        return Err(format!(
            "Personal Tutor lesson plan requires at least {minimum_storyboard_steps} substantive milestone(s) for this lesson depth and pacing; received {}",
            milestones.len()
        ));
    }
    let maximum = maximum_storyboard_steps
        .unwrap_or(TUTOR_LESSON_MAX_MILESTONES)
        .min(TUTOR_LESSON_MAX_MILESTONES);
    if milestones.len() > maximum {
        return Err(format!(
            "Personal Tutor lesson plan contains {} milestones; this lesson pacing allows at most {maximum}. Use the essential condensed objectives for Tutor Quick, or continue an unusually large progressive curriculum as a follow-up lesson",
            milestones.len(),
        ));
    }

    let mut normalized_ids = HashSet::new();
    let mut normalized_objectives = HashSet::new();
    let mut validated = Vec::with_capacity(milestones.len());
    for (index, milestone) in milestones.into_iter().enumerate() {
        let milestone_id = milestone.milestone_id.trim().to_string();
        let objective = milestone.objective.trim().to_string();
        if milestone_id.is_empty() || objective.is_empty() {
            return Err(format!(
                "Personal Tutor lesson milestone {} requires non-empty `milestone_id` and `objective`",
                index + 1
            ));
        }
        if milestone_id.chars().count() > TUTOR_LESSON_MAX_MILESTONE_ID_CHARS {
            return Err(format!(
                "Personal Tutor lesson milestone `{milestone_id}` exceeds the {TUTOR_LESSON_MAX_MILESTONE_ID_CHARS}-character id limit"
            ));
        }
        if objective.chars().count() > TUTOR_LESSON_MAX_MILESTONE_OBJECTIVE_CHARS {
            return Err(format!(
                "Personal Tutor lesson milestone `{milestone_id}` exceeds the {TUTOR_LESSON_MAX_MILESTONE_OBJECTIVE_CHARS}-character objective limit"
            ));
        }
        let normalized_id = normalize_tutor_lesson_coverage_text(&milestone_id);
        let normalized_objective = normalize_tutor_lesson_coverage_text(&objective);
        if normalized_id.is_empty() || normalized_objective.is_empty() {
            return Err(format!(
                "Personal Tutor lesson milestone {} must contain meaningful letters or numbers",
                index + 1
            ));
        }
        if !normalized_ids.insert(normalized_id) {
            return Err(format!(
                "Personal Tutor lesson milestone id `{milestone_id}` is duplicated after normalization"
            ));
        }
        if !normalized_objectives.insert(normalized_objective) {
            return Err(format!(
                "Personal Tutor lesson milestone `{milestone_id}` duplicates another teaching objective"
            ));
        }
        validated.push(TutorLessonMilestone {
            milestone_id,
            objective,
            // Carried through, not dropped. Validation rebuilds the milestone
            // from trimmed parts, so forgetting this would silently discard
            // every role and leave the depth gate permanently unsatisfiable.
            role: milestone.role,
        });
    }
    Ok(validated)
}

fn validate_storyboard_steps_against_lesson_plan(
    run: &TutorRun,
    storyboard_steps: &[TutorDrawStoryboardStep],
) -> Result<(), String> {
    let Some(contract) = run
        .lesson_contract
        .as_ref()
        .filter(|contract| !contract.milestones.is_empty())
    else {
        return Ok(());
    };
    let milestone_ids = contract
        .milestones
        .iter()
        .map(|milestone| normalize_tutor_lesson_coverage_text(&milestone.milestone_id))
        .collect::<HashSet<_>>();
    let unplanned = storyboard_steps
        .iter()
        .filter(|step| {
            !milestone_ids.contains(&normalize_tutor_lesson_coverage_text(&step.step_id))
        })
        .map(|step| step.step_id.as_str())
        .collect::<Vec<_>>();
    if unplanned.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Personal Tutor storyboard step(s) [{}] are not present in the adaptive lesson plan. Use a planned milestone id, or resend the full existing `tutor_lesson_plan` with genuinely new objectives appended before drawing them",
        unplanned.join(", ")
    ))
}

fn normalize_tutor_lesson_coverage_text(text: &str) -> String {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn tutor_goal_requests_deep_lesson(normalized: &str) -> bool {
    [
        "why",
        "logic behind",
        "reason behind",
        "how it works",
        "how does it work",
        "step by step",
        "walk me through",
        "prove",
        "proof",
        "derive",
        "derivation",
        "intuition",
        "fundamentally",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

fn tutor_goal_requests_focused_lesson(normalized: &str) -> bool {
    [
        "single marker",
        "one marker",
        "single pointer",
        "one pointer",
        "only point",
        "just point",
        "point to",
        "only highlight",
        "just highlight",
        "highlight the",
        "circle the",
        "locate the",
        "show me where",
        "tell me where",
        "where is",
        "summary",
        "overview",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

pub fn guided_tutor_finalization_continuation(run: &TutorRun) -> Option<TutorGuidedContinuation> {
    if !matches!(
        run.mode,
        TutorRunMode::GuidedAction | TutorRunMode::DemoAndCleanup
    ) {
        return None;
    }
    if run.status != TutorRunStatus::Running {
        return None;
    }
    if run.pending_action.is_some() {
        return None;
    }

    let reason = if run.has_successful_ui_action() {
        "The latest delegated tutor action has been verified, but the tutor run has not been completed. Continue with the next observed step or explicitly complete the run if the user's workflow is done."
    } else {
        "No UI-changing tutor action has been delegated yet; drawing or explaining is only the preview for a guided-action tutor prompt."
    };

    Some(TutorGuidedContinuation {
        run_id: run.run_id.clone(),
        mode: run.mode,
        reason: reason.to_string(),
        instruction: format!(
            "## Personal Tutor Runtime Continuation Required\n\
             Active tutor run `{}` is `{}` and is still running.\n\n\
             Reason: {reason}\n\n\
             Do not finalize the chat answer yet. Continue the tutor loop from the latest state:\n\
             - If the user's requested workflow is already complete, call `complete_tutor_run`.\n\
             - Otherwise observe the latest screen, resolve the next target, draw/explain it, call `check_for_copilot_user_action` when this is an App Copilot run, then immediately delegate the reversible UI action to `mac-operator` with a validated `tutor_action` envelope if no user action was already reported.\n\
             - If the next action is unsafe or blocked, record the failure/recovery step or request confirmation instead of claiming success.",
            run.run_id,
            run.mode.as_str()
        ),
    })
}

pub fn tutor_tool_loop_continuation_for_no_tool_response(
    current_user_text: &str,
    active_run: Option<&TutorRun>,
) -> Option<TutorToolLoopContinuation> {
    if !is_tutor_or_app_copilot_prompt(current_user_text) {
        return None;
    }

    tutor_tool_loop_continuation_for_active_lane(
        active_run,
        classify_tutor_or_app_copilot_turn_mode(current_user_text),
    )
}

/// Enforce the no-text-finalization contract after a typed Tutor/App Copilot
/// lane has already been authenticated. Active lane state, not a repeated
/// marker in the latest user message, controls continuation turns.
pub fn tutor_tool_loop_continuation_for_active_lane(
    active_run: Option<&TutorRun>,
    fallback_mode: TutorRunMode,
) -> Option<TutorToolLoopContinuation> {
    if let Some(run) = active_run {
        if run.status == TutorRunStatus::Running {
            // An explicit failure is a valid terminal outcome, not successful
            // lesson coverage. Allow the model to explain the blocker; the
            // chat finalizer will fail and release the run rather than mark it
            // completed or leave it active.
            if run.terminal_blocker_reason().is_some() {
                return None;
            }
            if let Some(progress) = run
                .lesson_progress()
                .filter(|progress| !progress.plan_ready || progress.remaining_storyboard_steps > 0)
            {
                let quick = progress.pacing == TutorLessonPacing::Condensed;
                let lesson_label = if quick {
                    "Tutor Quick condensed lesson"
                } else {
                    "normal Tutor progressive lesson"
                };
                let reason = if progress.plan_ready {
                    format!(
                        "The {lesson_label} has covered {} of {} adaptive narrated visual milestones; {} substantive milestone(s) remain: {}.",
                        progress.completed_storyboard_steps,
                        progress.planned_milestones,
                        progress.remaining_storyboard_steps,
                        progress
                            .remaining_milestones
                            .iter()
                            .map(|milestone| format!(
                                "{} ({})",
                                milestone.milestone_id, milestone.objective
                            ))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                } else {
                    format!(
                        "The {lesson_label} does not yet have its adaptive milestone plan. This {:?} lesson requires a concept-specific plan with at least {} substantive objective(s){}.",
                        progress.depth,
                        progress.minimum_storyboard_steps,
                        progress
                            .maximum_storyboard_steps
                            .map(|maximum| format!(" and at most {maximum}"))
                            .unwrap_or_default(),
                    )
                };
                let coverage_instruction = if quick {
                    "If `plan_ready` is false, pass `tutor_lesson_plan` with the complete condensed plan on the next `screen-draw` call. Select only the essential conceptual spine and stay within the runtime maximum; combine secondary detail into the nearest essential objective instead of expanding toward normal-Tutor depth. Each `milestone_id` must become the matching `storyboard_step_id`/`reveal_id` when taught. Keep each narration short and direct. Teach the remaining planned milestone(s), then complete immediately when progress reaches zero."
                } else {
                    "If `plan_ready` is false, pass `tutor_lesson_plan` with the full adaptive milestone plan on the next `screen-draw` call. For a DEEP lesson each milestone also needs a `role` of `setup`, `core`, or `conclusion`, and readiness requires at least two `core` milestones and at least as many `core` as setup+conclusion combined: the step a learner actually gets stuck on must be decomposed across several milestones, not asserted in one, and definitions and worked examples are cheap by comparison. Roles may be added to already-planned milestones by resending the full plan; use at least the runtime floor, but add every extra objective this concept genuinely requires. Each plan `milestone_id` must become the matching `storyboard_step_id`/`reveal_id` when taught. If a new prerequisite or logical part is discovered later, resend the full existing plan with that milestone appended before drawing it. Add each missing milestone with a short label, meaningful narration, and corresponding drawing."
                };
                return Some(TutorToolLoopContinuation {
                    run_id: Some(run.run_id.clone()),
                    mode: run.mode,
                    reason: reason.clone(),
                    instruction: format!(
                        "## {} Coverage Required\n\
                         Active Tutor run `{}` is still teaching `{}`.\n\n\
                         {reason}\n\n\
                         Discard the attempted final answer and continue the visual lesson. {coverage_instruction} The milestones may use separate `screen-draw` calls or independently timed child reveals in one group. Do not rename or split the same idea merely to satisfy a bound, remove existing plan scope, or substitute a text-only `say` step. Call `complete_tutor_run` only after `plan_ready` is true and the lesson contract reports zero remaining steps. If grounded drawing is genuinely blocked, record the concrete failure rather than claiming completion.",
                        if quick {
                            "Tutor Quick Condensed Lesson"
                        } else {
                            "Normal Tutor Progressive Lesson"
                        },
                        run.run_id,
                        run.goal,
                    ),
                });
            }
        }
        if let Some(continuation) = guided_tutor_finalization_continuation(run) {
            return Some(TutorToolLoopContinuation {
                run_id: Some(continuation.run_id),
                mode: continuation.mode,
                reason: continuation.reason,
                instruction: continuation.instruction,
            });
        }
        if run.status != TutorRunStatus::Running {
            return None;
        }
        let has_tutor_playback_step = run.step_history.iter().any(|record| {
            record.status == TutorStepStatus::Succeeded
                && matches!(record.step.kind, TutorStepKind::Draw | TutorStepKind::Say)
        });
        if has_tutor_playback_step {
            return None;
        }
        let reason = "An explicit tutor/copilot run is active, but this model response had no tool calls and no successful draw/narration or recorded blocker yet.";
        return Some(TutorToolLoopContinuation {
            run_id: Some(run.run_id.clone()),
            mode: run.mode,
            reason: reason.to_string(),
            instruction: tutor_no_tool_continuation_instruction(run.mode, reason, true),
        });
    }

    let mode = fallback_mode;
    let reason = "The current user message explicitly invoked Personal Tutor or App Copilot, but the model response had no tool calls, so no visual runtime or playback started.";
    Some(TutorToolLoopContinuation {
        run_id: None,
        mode,
        reason: reason.to_string(),
        instruction: tutor_no_tool_continuation_instruction(mode, reason, false),
    })
}

fn tutor_no_tool_continuation_instruction(
    mode: TutorRunMode,
    reason: &str,
    run_already_started: bool,
) -> String {
    let start_instruction = if run_already_started {
        "Continue the active tutor/copilot run. Do not start a duplicate run."
    } else {
        "First call `start_tutor_run` with the user's goal and the mode shown below. Use screen_overlay for App Copilot; use blackboard for source-free Personal Tutor."
    };
    let app_copilot_instruction = if matches!(
        mode,
        TutorRunMode::GuidedAction | TutorRunMode::DemoAndCleanup
    ) {
        "\n         - For App Copilot, after the storyboard-bearing `screen-draw` preview, call `check_for_copilot_user_action` once without waiting; if it returns `user_acted`, observe/verify and continue without automation, and if it returns `no_user_action`, immediately delegate the same reversible step through `mac-operator` with `tutor_action`."
    } else {
        ""
    };
    format!(
        "## Personal Tutor / App Copilot Runtime Takeover Required\n\
         {reason}\n\n\
         The previous draft for this same turn must be discarded. This turn is tutor/copilot-owned: do not answer directly and do not claim that anything was highlighted, drawn, narrated, or shown until a runtime tool has actually run.\n\n\
         Required next action:\n\
         - {start_instruction}\n\
         - Required mode: `{}`.\n\
         - Then run the normal visual loop: observe if needed, resolve the visible target from fresh screen context, and call `screen-draw` with a storyboard-bearing `shape_json`.\n\
         - Every meaningful draw group must include `storyboard_step_id`, `tutor_step_label` or `step_label`, and `narration` inside `shape_json`.\n\
         {app_copilot_instruction}\n\
         - If drawing cannot be grounded safely, call `record_tutor_step_failure` with the blocker instead of producing a direct answer.\n\
         - Only after a draw/narration step or recorded blocker exists may you produce final chat text.",
        mode.as_str()
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TutorRunScope {
    pub principal: String,
    pub workspace: String,
    pub session_id: String,
}

impl TutorRunScope {
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            session_id: session_id.into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct TutorRunStore {
    inner: Mutex<TutorRunStoreInner>,
}

#[derive(Debug, Default)]
struct TutorRunStoreInner {
    runs: HashMap<String, TutorRun>,
    active_by_scope: HashMap<TutorRunScope, String>,
    scopes_by_run: HashMap<String, TutorRunScope>,
    user_action_events_by_run: HashMap<String, Vec<TutorUserActionEvent>>,
    active_last_seen_at_ms: HashMap<String, i64>,
}

static GLOBAL_TUTOR_RUN_STORE: OnceLock<Arc<TutorRunStore>> = OnceLock::new();

pub fn tutor_run_store() -> Arc<TutorRunStore> {
    GLOBAL_TUTOR_RUN_STORE
        .get_or_init(|| Arc::new(TutorRunStore::default()))
        .clone()
}

impl TutorRunStore {
    pub fn start_run(
        &self,
        scope: TutorRunScope,
        mode: TutorRunMode,
        canvas_mode: TutorCanvasMode,
        goal: impl Into<String>,
        initial_observation: bool,
        initial_visual_entity_map: Option<TutorVisualEntityMap>,
    ) -> Result<TutorRun, String> {
        if canvas_mode.is_blackboard() && initial_observation {
            return Err(
                "blackboard tutor runs must start with initial_observation=false; use screen_overlay for visible screen sources"
                    .to_string(),
            );
        }
        if canvas_mode.is_blackboard() && initial_visual_entity_map.is_some() {
            return Err(
                "blackboard tutor runs cannot start with a visual_entity_map; use screen_overlay for visible screen sources"
                    .to_string(),
            );
        }
        let run_id = format!("tutor-{}", Uuid::new_v4().simple());
        let mut run =
            TutorRun::new_incremental_with_canvas(run_id.clone(), mode, goal, canvas_mode)?;
        // Thinking Map → Tutor adapter (plan Phase 8 item 8): if the owner
        // registered a map→tutor binding for this chat session, attach the
        // bounded, origin-annotated digest as grounding context. Reference
        // material only — the tutor keeps narration/storyboard ownership. The
        // lookup is TTL-bounded and scope-keyed, so all other sessions/flows
        // are untouched (and get `None`, keeping serialization byte-identical).
        run.thinking_map_context =
            crate::magician_v2::tutor_map_context::tutor_map_context_registry()
                .current(&scope.principal, &scope.workspace, &scope.session_id)
                .map(|binding| binding.context);
        if initial_observation {
            let observe_step = TutorStep {
                kind: TutorStepKind::Observe,
                label: "initial screen observation".to_string(),
                target: None,
                expected_state: Some(
                    "Current HUD/screen prompt includes a fresh screen observation".to_string(),
                ),
                safety: TutorSafetyLevel::VisualOnly,
                source_entity_ids: Vec::new(),
                visual_entity_map: initial_visual_entity_map,
            };
            run.propose_step(observe_step)?;
        }

        let mut inner = self.lock_inner()?;
        inner
            .scopes_by_run
            .insert(run.run_id.clone(), scope.clone());
        inner.active_by_scope.insert(scope, run.run_id.clone());
        inner
            .active_last_seen_at_ms
            .insert(run.run_id.clone(), current_unix_time_ms());
        inner.runs.insert(run.run_id.clone(), run.clone());
        Ok(run)
    }

    pub fn active_run(&self, scope: &TutorRunScope) -> Result<Option<TutorRun>, String> {
        let mut inner = self.lock_inner()?;
        let Some(run_id) = inner.active_by_scope.get(scope).cloned() else {
            return Ok(None);
        };
        let Some(run) = inner.runs.get(&run_id).cloned() else {
            inner.active_by_scope.remove(scope);
            inner.active_last_seen_at_ms.remove(&run_id);
            return Ok(None);
        };
        if run.status.is_terminal() {
            Self::release_active_run(&mut inner, &run_id);
            return Ok(None);
        }
        let now_ms = current_unix_time_ms();
        let last_seen_ms = inner
            .active_last_seen_at_ms
            .get(&run_id)
            .copied()
            .unwrap_or(now_ms);
        if now_ms.saturating_sub(last_seen_ms) > TUTOR_ACTIVE_LANE_IDLE_TIMEOUT_MS {
            // A delegated App Copilot action has its own bounded task watcher.
            // Do not orphan that execution merely because the conversational
            // lane was idle while automation was running.
            if run.is_app_copilot() && run.pending_action_execution_id.is_some() {
                inner.active_last_seen_at_ms.insert(run_id, now_ms);
                return Ok(Some(run));
            }
            if let Some(stale_run) = inner.runs.get_mut(&run_id) {
                stale_run.fail_with_reason(Some(
                    "Tutor/App Copilot lane expired after 30 minutes without activity".to_string(),
                ));
            }
            Self::release_active_run(&mut inner, &run_id);
            return Ok(None);
        }
        inner.active_last_seen_at_ms.insert(run_id, now_ms);
        Ok(Some(run))
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<TutorRun>, String> {
        let inner = self.lock_inner()?;
        Ok(inner.runs.get(run_id).cloned())
    }

    pub fn scope_for_run(&self, run_id: &str) -> Result<Option<TutorRunScope>, String> {
        let inner = self.lock_inner()?;
        Ok(inner.scopes_by_run.get(run_id).cloned())
    }

    pub fn propose_step(&self, run_id: &str, step: TutorStep) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        run.propose_step(step)?;
        Ok(run.clone())
    }

    pub fn propose_step_with_storyboard_steps(
        &self,
        run_id: &str,
        step: TutorStep,
        storyboard_steps: Vec<TutorDrawStoryboardStep>,
    ) -> Result<TutorRun, String> {
        self.propose_step_with_lesson_milestones(run_id, step, storyboard_steps, None)
    }

    /// Atomically commit an optional Personal Tutor plan revision and the
    /// successful renderer-backed storyboard step. This prevents a failed host
    /// draw from advancing either progressive/condensed scope or coverage.
    pub fn propose_step_with_lesson_milestones(
        &self,
        run_id: &str,
        step: TutorStep,
        storyboard_steps: Vec<TutorDrawStoryboardStep>,
        lesson_milestones: Option<Vec<TutorLessonMilestone>>,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        let mut candidate = run.clone();
        if let Some(milestones) = lesson_milestones {
            candidate.set_or_expand_lesson_milestones(milestones)?;
        }
        candidate.propose_step_with_storyboard_steps(step, storyboard_steps)?;
        *run = candidate;
        Ok(run.clone())
    }

    pub fn propose_delegated_action_step(
        &self,
        run_id: &str,
        step: TutorStep,
        execution_id: impl Into<String>,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        let changes_ui_state = step.kind.changes_ui_state();
        run.propose_step(step)?;
        if changes_ui_state {
            let execution_id = execution_id.into();
            if !execution_id.trim().is_empty() {
                run.pending_action_execution_id = Some(execution_id);
            }
        }
        Ok(run.clone())
    }

    /// Atomically revalidate and admit an envelope-bound delegated action.
    /// App Copilot additionally consumes the server-owned no-user-action
    /// receipt. Personal Tutor action-mode behavior is intentionally unchanged.
    pub fn propose_delegated_action_envelope(
        &self,
        envelope: &TutorActionEnvelope,
        execution_id: impl Into<String>,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(&envelope.run_id) else {
            return Err(format!("tutor run `{}` not found", envelope.run_id));
        };
        let step = validate_tutor_action_envelope_for_run(run, envelope)?;
        let changes_ui_state = step.kind.changes_ui_state();
        run.propose_step(step)?;
        run.pending_created_object = envelope.created_object.clone();
        if changes_ui_state {
            let execution_id = execution_id.into();
            if !execution_id.trim().is_empty() {
                run.pending_action_execution_id = Some(execution_id);
            }
        }
        Ok(run.clone())
    }

    // `record_copilot_action_check`, `record_user_action_event_and_preempt_pending`,
    // and `finalize_user_action_preemption` live in the `copilot_rail` impl
    // block: the rail's receipt and preemption entry points, moved without
    // change.

    pub fn record_step_failure(&self, run_id: &str, step: TutorStep) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        if run.status.is_terminal() {
            return Err(format!(
                "cannot record tutor step failure while run `{run_id}` is {:?}",
                run.status
            ));
        }
        run.record_step_failure(step);
        let updated = run.clone();
        if updated.status.is_terminal() {
            Self::release_active_run(&mut inner, run_id);
        }
        Ok(updated)
    }

    pub fn record_created_object(
        &self,
        run_id: &str,
        object: TutorCreatedObject,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        if run.status.is_terminal() {
            return Err(format!(
                "cannot record created object while run `{run_id}` is {:?}",
                run.status
            ));
        }
        run.record_created_object(object);
        Ok(run.clone())
    }

    pub fn complete_run(&self, run_id: &str) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        run.complete()?;
        let completed = run.clone();
        Self::release_active_run(&mut inner, run_id);
        Ok(completed)
    }

    pub fn fail_run(&self, run_id: &str) -> Result<TutorRun, String> {
        self.fail_run_with_reason(run_id, None)
    }

    pub fn fail_run_with_reason(
        &self,
        run_id: &str,
        reason: Option<String>,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        if run.status == TutorRunStatus::Completed {
            let completed = run.clone();
            Self::release_active_run(&mut inner, run_id);
            return Ok(completed);
        }
        if run.status == TutorRunStatus::Failed {
            if run.terminal_reason.is_none() {
                run.terminal_reason = reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .map(ToOwned::to_owned);
            }
            let failed = run.clone();
            Self::release_active_run(&mut inner, run_id);
            return Ok(failed);
        }
        run.fail_with_reason(reason);
        let failed = run.clone();
        Self::release_active_run(&mut inner, run_id);
        Ok(failed)
    }

    /// Fails the active run for a scope and releases its active index. This is
    /// intentionally scoped instead of session-store-backed, so stale in-memory
    /// runs can still be cancelled after their persisted chat session is gone.
    pub fn cancel_active_run(
        &self,
        scope: &TutorRunScope,
        reason: impl Into<String>,
    ) -> Result<Option<TutorRunCancellation>, String> {
        let mut inner = self.lock_inner()?;
        let Some(run_id) = inner.active_by_scope.get(scope).cloned() else {
            return Ok(None);
        };
        let Some(run) = inner.runs.get_mut(&run_id) else {
            inner.active_by_scope.remove(scope);
            return Ok(None);
        };
        if run.status.is_terminal() {
            Self::release_active_run(&mut inner, &run_id);
            return Ok(None);
        }
        let preempted_execution_id = run.pending_action_execution_id.clone();
        run.fail_with_reason(Some(reason.into()));
        let cancelled = TutorRunCancellation {
            run: run.clone(),
            preempted_execution_id,
        };
        Self::release_active_run(&mut inner, &run_id);
        Ok(Some(cancelled))
    }

    pub fn apply_action_result(
        &self,
        result: TutorActionResult,
    ) -> Result<TutorActionResultApplication, String> {
        let mut inner = self.lock_inner()?;
        let run_id = result.run_id.clone();
        let Some(run) = inner.runs.get_mut(&run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        if run.pending_action.is_none() {
            return Ok(TutorActionResultApplication {
                run_id,
                applied: false,
                status: result.status,
                note: "No pending tutor action to verify; result was already applied or stale."
                    .to_string(),
                run: Some(run.clone()),
            });
        }

        let application = match result.status {
            TutorActionResultStatus::Succeeded => {
                let pending = run.pending_action.clone();
                let created_object = run.pending_created_object.take();
                let removed_object_label = pending.as_ref().and_then(|step| {
                    (step.safety == TutorSafetyLevel::SessionOwnedDestructive)
                        .then(|| step.target.clone())
                        .flatten()
                });
                run.propose_step(TutorStep {
                    kind: TutorStepKind::Observe,
                    label: "observe after delegated tutor action".to_string(),
                    target: pending.as_ref().and_then(|step| step.target.clone()),
                    expected_state: Some(result.evidence.clone()),
                    safety: TutorSafetyLevel::VisualOnly,
                    source_entity_ids: Vec::new(),
                    visual_entity_map: None,
                })?;
                run.propose_step(TutorStep {
                    kind: TutorStepKind::Verify,
                    label: pending
                        .as_ref()
                        .map(|step| format!("verify {}", step.label))
                        .unwrap_or_else(|| "verify delegated tutor action".to_string()),
                    target: pending.as_ref().and_then(|step| step.target.clone()),
                    expected_state: Some(result.evidence),
                    safety: TutorSafetyLevel::VisualOnly,
                    source_entity_ids: Vec::new(),
                    visual_entity_map: None,
                })?;
                if let Some(object) = created_object {
                    run.record_created_object(object);
                }
                if let Some(label) = removed_object_label {
                    if run.owns_object_label(&label)
                        && !run.removed_created_object_labels.contains(&label)
                    {
                        run.removed_created_object_labels.push(label);
                    }
                }
                // Pin the match's `Result` error type to the fn's `String` — no arm
                // produces an `Err`, so `E` is otherwise unconstrained (E0282).
                Ok::<_, String>(TutorActionResultApplication {
                    run_id: run_id.clone(),
                    applied: true,
                    status: result.status,
                    note: "Recorded delegated tutor action observation and verification."
                        .to_string(),
                    run: Some(run.clone()),
                })
            },
            TutorActionResultStatus::Failed => {
                run.record_action_result_failure(result.evidence);
                Ok(TutorActionResultApplication {
                    run_id: run_id.clone(),
                    applied: true,
                    status: result.status,
                    note: "Recorded delegated tutor action failure and cleared pending action."
                        .to_string(),
                    run: Some(run.clone()),
                })
            },
        }?;
        if application
            .run
            .as_ref()
            .is_some_and(|run| run.status.is_terminal())
        {
            Self::release_active_run(&mut inner, &run_id);
        }
        Ok(application)
    }

    /// Convert a terminal delegated execution that produced no valid
    /// `TUTOR_ACTION_RESULT` into an explicit failed action result. This keeps
    /// the run recoverable instead of leaving a permanent pending action.
    pub fn fail_pending_action_for_execution(
        &self,
        execution_id: &str,
        evidence: impl Into<String>,
    ) -> Result<Option<TutorActionResultApplication>, String> {
        let execution_id = execution_id.trim();
        if execution_id.is_empty() {
            return Ok(None);
        }
        let mut inner = self.lock_inner()?;
        let Some(run_id) = inner.runs.iter().find_map(|(run_id, run)| {
            (run.pending_action_execution_id.as_deref() == Some(execution_id))
                .then(|| run_id.clone())
        }) else {
            return Ok(None);
        };
        let run = inner
            .runs
            .get_mut(&run_id)
            .ok_or_else(|| format!("tutor run `{run_id}` not found"))?;
        run.record_action_result_failure(evidence);
        let application = TutorActionResultApplication {
            run_id,
            applied: true,
            status: TutorActionResultStatus::Failed,
            note: "Delegated App Copilot action ended without a valid TUTOR_ACTION_RESULT; cleared the pending action for recovery."
                .to_string(),
            run: Some(run.clone()),
        };
        Ok(Some(application))
    }

    pub fn record_user_action_event(
        &self,
        event: TutorUserActionEvent,
    ) -> Result<TutorUserActionEvent, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get(&event.run_id) else {
            return Err(format!("tutor run `{}` not found", event.run_id));
        };
        if run.status.is_terminal() {
            return Err(format!(
                "cannot record user action for terminal tutor run `{}`",
                event.run_id
            ));
        }
        let events = inner
            .user_action_events_by_run
            .entry(event.run_id.clone())
            .or_default();
        events.push(event.clone());
        if events.len() > 32 {
            let drop_count = events.len().saturating_sub(32);
            events.drain(0..drop_count);
        }
        Ok(event)
    }

    /// Apply a storyboard-bound action that the user completed before
    /// automation was admitted. The user event is the authoritative action
    /// evidence for this branch; the run advances through the same
    /// action→observe→verify state machine used by delegated results.
    pub fn apply_user_completed_copilot_step(
        &self,
        event: &TutorUserActionEvent,
        action_step: TutorStep,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(&event.run_id) else {
            return Err(format!("tutor run `{}` not found", event.run_id));
        };
        if !run.is_app_copilot() {
            return Err(
                "manual App Copilot action evidence cannot mutate a Personal Tutor run".to_string(),
            );
        }
        let mut candidate = run.clone();
        candidate.propose_step(action_step.clone())?;
        candidate.propose_step(TutorStep {
            kind: TutorStepKind::Observe,
            label: "observe user-performed App Copilot action".to_string(),
            target: action_step.target.clone(),
            expected_state: Some(event.evidence.clone()),
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        })?;
        candidate.propose_step(TutorStep {
            kind: TutorStepKind::Verify,
            label: format!("verify user performed {}", action_step.label),
            target: action_step.target,
            expected_state: Some(event.evidence.clone()),
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        })?;
        *run = candidate;
        Ok(run.clone())
    }

    pub fn take_matching_user_action_event(
        &self,
        run_id: &str,
        storyboard_step_id: Option<&str>,
        storyboard_step_label: Option<&str>,
        since_ms: i64,
    ) -> Result<Option<TutorUserActionEvent>, String> {
        let mut inner = self.lock_inner()?;
        if !inner.runs.contains_key(run_id) {
            return Err(format!("tutor run `{run_id}` not found"));
        }
        let Some(events) = inner.user_action_events_by_run.get_mut(run_id) else {
            return Ok(None);
        };
        let match_index = events.iter().position(|event| {
            if event.occurred_at_ms < since_ms {
                return false;
            }
            if let Some(step_id) = storyboard_step_id {
                if event.storyboard_step_id.as_deref() == Some(step_id) {
                    return true;
                }
                if event.storyboard_step_id.is_some() {
                    return false;
                }
            }
            if let Some(step_label) = storyboard_step_label {
                if event.storyboard_step_label.as_deref() == Some(step_label) {
                    return true;
                }
                if event.storyboard_step_label.is_some() {
                    return false;
                }
            }
            if storyboard_step_id.is_some() || storyboard_step_label.is_some() {
                return event.storyboard_step_id.is_none() && event.storyboard_step_label.is_none();
            }
            true
        });
        Ok(match_index.map(|index| events.remove(index)))
    }

    fn lock_inner(&self) -> Result<MutexGuard<'_, TutorRunStoreInner>, String> {
        self.inner
            .lock()
            .map_err(|_| "tutor run store lock is poisoned".to_string())
    }

    fn release_active_run(inner: &mut TutorRunStoreInner, run_id: &str) {
        let scopes = inner
            .active_by_scope
            .iter()
            .filter_map(|(scope, active_run_id)| (active_run_id == run_id).then(|| scope.clone()))
            .collect::<Vec<_>>();
        for scope in scopes {
            inner.active_by_scope.remove(&scope);
        }
        inner.active_last_seen_at_ms.remove(run_id);
        inner.user_action_events_by_run.remove(run_id);
    }
}

fn current_unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

pub fn is_tutor_prompt(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    if TUTOR_MARKER_INVOKE_WORDS
        .iter()
        .any(|invoke_word| lowercase.contains(invoke_word))
    {
        return true;
    }

    let words = lowercase
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    words
        .windows(2)
        .any(|window| window[0] == "hey" && TUTOR_SPOKEN_INVOKE_WORDS.contains(&window[1]))
}

pub fn is_app_copilot_prompt(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    if APP_COPILOT_MARKER_INVOKE_WORDS
        .iter()
        .any(|invoke_word| lowercase.contains(invoke_word))
    {
        return true;
    }

    let words = lowercase
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    words
        .windows(2)
        .any(|window| window[0] == "hey" && APP_COPILOT_SPOKEN_INVOKE_WORDS.contains(&window[1]))
        || words
            .windows(3)
            .any(|window| window[0] == "hey" && window[1] == "app" && window[2] == "copilot")
}

pub fn is_tutor_or_app_copilot_prompt(text: &str) -> bool {
    is_tutor_prompt(text) || is_app_copilot_prompt(text)
}

pub fn has_tutor_quick_flag(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        token
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '#')
            .eq_ignore_ascii_case(TUTOR_QUICK_FLAG)
    })
}

pub fn is_tutor_quick_prompt(text: &str) -> bool {
    is_tutor_or_app_copilot_prompt(text) && has_tutor_quick_flag(text)
}

pub fn validate_tutor_plan(plan: &TutorSessionPlan) -> Result<(), String> {
    if plan.goal.trim().is_empty() {
        return Err("tutor plan goal cannot be empty".to_string());
    }
    if plan.steps.is_empty() {
        return Err("tutor plan must contain at least one step".to_string());
    }

    let mut has_fresh_observation = false;
    let mut pending_action_verification: Option<&TutorStep> = None;
    let mut destructive_confirmed = false;
    let mut latest_entity_ids: Option<HashSet<String>> = None;

    for (index, step) in plan.steps.iter().enumerate() {
        let step_number = index + 1;
        if step.label.trim().is_empty() {
            return Err(format!("tutor step {step_number} label cannot be empty"));
        }
        if let Some(map) = step.visual_entity_map.as_ref() {
            if step.kind != TutorStepKind::Observe {
                return Err(format!(
                    "tutor step {step_number} can attach `visual_entity_map` only to observe"
                ));
            }
            validate_visual_entity_map(map).map_err(|reason| {
                format!("tutor step {step_number} visual entity map: {reason}")
            })?;
        }
        if !step.source_entity_ids.is_empty() {
            let Some(entity_ids) = latest_entity_ids.as_ref() else {
                return Err(format!(
                    "tutor step {step_number} references visual entities but no fresh visual entity map is active"
                ));
            };
            let missing = step
                .source_entity_ids
                .iter()
                .filter(|id| !entity_ids.contains(id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "tutor step {step_number} references unknown visual entity ids: {}",
                    missing.join(", ")
                ));
            }
        }

        match step.kind {
            TutorStepKind::Observe => {
                has_fresh_observation = true;
                destructive_confirmed = false;
                latest_entity_ids = step.visual_entity_map.as_ref().map(|map| {
                    map.entities
                        .iter()
                        .map(|entity| entity.id.clone())
                        .collect::<HashSet<_>>()
                });
            },
            TutorStepKind::Confirm => {
                destructive_confirmed = true;
            },
            TutorStepKind::Recover => {
                has_fresh_observation = false;
                destructive_confirmed = false;
                latest_entity_ids = None;
            },
            TutorStepKind::Verify => {
                if !has_fresh_observation {
                    return Err(format!(
                        "tutor step {step_number} `verify` requires a fresh observation"
                    ));
                }
                pending_action_verification = None;
                destructive_confirmed = false;
            },
            _ => {
                if step.kind.requires_fresh_observation() && !has_fresh_observation {
                    return Err(format!(
                        "tutor step {step_number} `{}` requires a fresh observation",
                        tutor_step_kind_name(step.kind)
                    ));
                }
            },
        }

        if step.kind.changes_ui_state() {
            if let Some(pending) = pending_action_verification {
                return Err(format!(
                    "tutor step {step_number} starts another UI-changing action before verifying `{}`",
                    pending.label
                ));
            }
            if step.safety == TutorSafetyLevel::DestructiveRequiresConfirmation
                && !destructive_confirmed
            {
                return Err(format!(
                    "tutor step {step_number} `{}` needs confirmation before destructive action",
                    step.label
                ));
            }
            pending_action_verification = Some(step);
            has_fresh_observation = false;
            destructive_confirmed = false;
            latest_entity_ids = None;
        }
    }

    if let Some(step) = pending_action_verification {
        return Err(format!(
            "tutor UI-changing action `{}` must be followed by a verify step",
            step.label
        ));
    }

    Ok(())
}

pub fn validate_next_tutor_step(run: &TutorRun, step: &TutorStep) -> Result<(), String> {
    if step.label.trim().is_empty() {
        return Err("tutor step label cannot be empty".to_string());
    }
    if let Some(map) = step.visual_entity_map.as_ref() {
        if step.kind != TutorStepKind::Observe {
            return Err("visual_entity_map can be attached only to observe steps".to_string());
        }
        validate_visual_entity_map(map)?;
    }
    validate_step_source_entity_refs(run, step)?;
    if !run.mode.allows_ui_mutation() && step.kind.changes_ui_state() {
        if run.mode == TutorRunMode::ExplainOnly {
            return Err("explain-only tutor runs cannot change app UI state".to_string());
        }
        return Err(format!(
            "{} tutor runs cannot change app UI state",
            run.mode.as_str()
        ));
    }
    if step.kind == TutorStepKind::Verify
        && run.pending_action.is_none()
        && run.mode.allows_ui_mutation()
    {
        return Err("verify step requires a pending UI-changing action".to_string());
    }
    if step.kind == TutorStepKind::Draw
        && !run.has_resolved_target
        && !run.canvas_mode.is_blackboard()
    {
        return Err(
            "draw step requires resolving the target from the latest observation".to_string(),
        );
    }
    if step.kind.changes_ui_state() {
        if let Some(pending) = run.pending_action.as_ref() {
            return Err(format!(
                "cannot start another UI-changing action before verifying `{}`",
                pending.label
            ));
        }
        if step.safety == TutorSafetyLevel::DestructiveRequiresConfirmation
            && !run.destructive_confirmed
        {
            return Err(format!(
                "tutor step `{}` needs confirmation before destructive action",
                step.label
            ));
        }
        if step.safety == TutorSafetyLevel::SessionOwnedDestructive {
            let target_label = step
                .target
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(step.label.as_str());
            if !run.owns_object_label(target_label) {
                return Err(format!(
                    "tutor step `{}` claims session-owned destructive safety, but `{target_label}` was not recorded as created in this run",
                    step.label
                ));
            }
        }
    }
    if step.kind.requires_fresh_observation()
        && !run.has_fresh_observation
        && !(run.canvas_mode.is_blackboard()
            && matches!(
                step.kind,
                TutorStepKind::Draw | TutorStepKind::Say | TutorStepKind::ClearDrawings
            ))
    {
        return Err(format!(
            "tutor step `{}` requires a fresh observation",
            tutor_step_kind_name(step.kind)
        ));
    }
    if matches!(
        step.kind,
        TutorStepKind::Click
            | TutorStepKind::TypeText
            | TutorStepKind::Hotkey
            | TutorStepKind::Scroll
    ) && !run.has_resolved_target
    {
        return Err(format!(
            "tutor step `{}` requires a resolved target from the latest observation",
            tutor_step_kind_name(step.kind)
        ));
    }
    Ok(())
}

pub fn validate_visual_entity_map(map: &TutorVisualEntityMap) -> Result<(), String> {
    if map.observation_id.trim().is_empty() {
        return Err("observation_id cannot be empty".to_string());
    }
    if map.coordinate_space.width == 0 || map.coordinate_space.height == 0 {
        return Err("coordinate_space width and height must be positive".to_string());
    }
    if map.entities.len() > 200 {
        return Err("visual entity map cannot contain more than 200 entities".to_string());
    }
    let mut ids = HashSet::new();
    for entity in &map.entities {
        let id = entity.id.trim();
        if id.is_empty() {
            return Err("visual entity id cannot be empty".to_string());
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("duplicate visual entity id `{id}`"));
        }
        if entity.geometry.is_null() {
            return Err(format!("visual entity `{id}` requires geometry"));
        }
        if entity
            .label
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
            && entity
                .text
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            && entity.source_evidence.is_empty()
        {
            return Err(format!(
                "visual entity `{id}` needs a label, text, or source_evidence"
            ));
        }
    }
    Ok(())
}

pub fn validate_tutor_draw_payload_shape(shape: &Value) -> Result<Vec<String>, String> {
    let mut source_entity_ids = Vec::new();
    validate_tutor_draw_payload_node(shape, 0, &mut source_entity_ids)?;
    validate_tutor_chart_frame_layout(shape)?;
    dedupe_preserving_order(source_entity_ids)
}

pub fn validate_tutor_draw_payload_for_run(
    run: &TutorRun,
    shape: &Value,
) -> Result<Vec<String>, String> {
    let source_entity_ids = validate_tutor_draw_payload_shape(shape)?;
    if source_entity_ids.is_empty() {
        return Ok(source_entity_ids);
    }
    let step = TutorStep {
        kind: TutorStepKind::Draw,
        label: "validate entity-grounded tutor draw payload".to_string(),
        target: None,
        expected_state: None,
        safety: TutorSafetyLevel::VisualOnly,
        source_entity_ids: source_entity_ids.clone(),
        visual_entity_map: None,
    };
    validate_step_source_entity_refs(run, &step)?;
    Ok(source_entity_ids)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TutorDrawStoryboardStep {
    pub step_id: String,
    pub label: String,
    pub narration: String,
    /// Whether any primitive carrying this step id actually DRAWS something —
    /// a figure, not more text.
    ///
    /// A milestone is meant to be taught visually, and the runtime already
    /// refuses to count a text-only `say` step. But a draw built from
    /// `formula` + `callout` is equally pure prose and used to satisfy
    /// coverage, so "explain it with a diagram" was advice the contract could
    /// not enforce. Only figure-backed steps count toward the lesson plan;
    /// text-only ones still render, they just do not tick a milestone.
    ///
    /// `serde(default)` is false so a persisted run from before this existed
    /// re-earns coverage on its next draw rather than being retroactively
    /// judged against a rule it never saw.
    #[serde(default)]
    pub figure_backed: bool,
}

/// Whether a shape type puts marks on the canvas rather than words.
///
/// The excluded set is every primitive whose entire output is text. Anything
/// else — geometry, arrows, regions, highlights, solids — is a figure. New
/// primitives are figures by default, which is the safe direction: a wrongly
/// classified figure costs nothing, while a wrongly classified text shape
/// would let prose satisfy a visual milestone.
pub fn tutor_shape_type_is_figure(shape_type: &str) -> bool {
    !matches!(
        shape_type,
        "label"
            | "callout"
            | "formula"
            | "unit_label"
            | "side_label"
            | "cursive_text"
            | "handwriting"
            | "timeline_tick"
    )
}

pub fn validate_tutor_draw_storyboard_payload(
    shape: &Value,
) -> Result<Vec<TutorDrawStoryboardStep>, String> {
    if tutor_draw_shape_is_clear(shape) {
        return Ok(Vec::new());
    }
    let mut steps = Vec::new();
    collect_tutor_draw_storyboard_steps(
        shape,
        &TutorDrawStoryboardInheritance::default(),
        &mut steps,
    )?;
    // Dedupe on semantic identity, but OR the figure flag: a group whose
    // children are [formula, path] emits two leaves under one id, and that step
    // IS figure-backed. Dropping the later leaf without merging its flag would
    // decide visual coverage by child ordering.
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<TutorDrawStoryboardStep> = Vec::new();
    for step in steps {
        let key = format!("{}\n{}\n{}", step.step_id, step.label, step.narration);
        match index_by_key.get(&key) {
            Some(&at) => deduped[at].figure_backed |= step.figure_backed,
            None => {
                index_by_key.insert(key, deduped.len());
                deduped.push(step);
            },
        }
    }
    if deduped.is_empty() {
        return Err(
            "tutor screen-draw payload must include at least one storyboard step with `tutor_step_label`/`step_label` and `narration`"
                .to_string(),
        );
    }
    Ok(deduped)
}

pub fn is_supported_tutor_draw_shape_type(shape_type: &str) -> bool {
    matches!(
        shape_type,
        "clear"
            | "group"
            | "label"
            | "callout"
            | "line"
            | "arrow"
            | "rect"
            | "highlight"
            | "polygon"
            | "circle"
            // Parametric solids: the model supplies real measurements and the
            // recipe derives the projection, so a cone's base cannot disagree
            // with its own body.
            | "cone"
            | "sector"
            | "arc"
            | "path"
            | "curve"
            | "freehand"
            | "handwriting"
            | "cursive_text"
            | "mask"
            | "spotlight"
            | "angle_marker"
            | "right_angle_marker"
            | "side_label"
            | "perpendicular_marker"
            | "parallel_marker"
            | "square_on_segment"
            | "area_fill"
            | "measurement_tick"
            | "formula"
            | "vector_arrow"
            | "force_arrow"
            | "component_vector"
            | "axis"
            | "trajectory"
            | "field_line"
            | "free_body_body"
            | "unit_label"
            | "code_highlight"
            | "stack_frame"
            | "heap_object"
            | "pointer_arrow"
            | "state_box"
            | "flow_node"
            | "flow_edge"
            | "timeline_tick"
            | "memory_cell"
    )
}

#[derive(Debug, Clone, Default)]
struct TutorDrawStoryboardInheritance {
    step_id: Option<String>,
    label: Option<String>,
    narration: Option<String>,
}

fn collect_tutor_draw_storyboard_steps(
    shape: &Value,
    inherited: &TutorDrawStoryboardInheritance,
    steps: &mut Vec<TutorDrawStoryboardStep>,
) -> Result<(), String> {
    let object = shape
        .as_object()
        .ok_or_else(|| "draw payload must be a JSON object".to_string())?;
    let shape_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "draw payload requires non-empty `type`".to_string())?;
    if shape_type == "clear" {
        return Ok(());
    }

    let next = TutorDrawStoryboardInheritance {
        step_id: storyboard_optional_string(object, "storyboard_step_id")
            .or_else(|| storyboard_optional_string(object, "reveal_id"))
            .or_else(|| inherited.step_id.clone()),
        label: storyboard_optional_string(object, "tutor_step_label")
            .or_else(|| storyboard_optional_string(object, "step_label"))
            .or_else(|| inherited.label.clone()),
        narration: storyboard_optional_string(object, "narration")
            .or_else(|| inherited.narration.clone()),
    };

    if shape_type == "group" {
        let children = object
            .get("shapes")
            .or_else(|| object.get("children"))
            .ok_or_else(|| "draw group requires `shapes` or `children`".to_string())?;
        let children = children
            .as_array()
            .ok_or_else(|| "draw group `shapes` must be an array".to_string())?;
        for child in children {
            collect_tutor_draw_storyboard_steps(child, &next, steps)?;
        }
        return Ok(());
    }

    let Some(label) = next.label.clone() else {
        return Err(format!(
            "tutor storyboard step for draw shape `{shape_type}` requires `tutor_step_label` or `step_label`"
        ));
    };
    let Some(narration) = next.narration.clone() else {
        return Err(format!(
            "tutor storyboard step `{label}` for draw shape `{shape_type}` requires `narration`"
        ));
    };
    let step_id = next
        .step_id
        .clone()
        .unwrap_or_else(|| tutor_storyboard_step_id_from_label(&label));
    steps.push(TutorDrawStoryboardStep {
        step_id,
        label,
        narration,
        figure_backed: tutor_shape_type_is_figure(shape_type),
    });
    Ok(())
}

fn storyboard_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn tutor_storyboard_step_id_from_label(label: &str) -> String {
    let slug = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "tutor-step".to_string()
    } else {
        slug.chars()
            .take(TUTOR_DRAW_MAX_STORYBOARD_STEP_ID_CHARS)
            .collect()
    }
}

fn tutor_draw_shape_is_clear(shape: &Value) -> bool {
    let shape_type = shape
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if shape_type == "clear" {
        return true;
    }
    if shape_type != "group" {
        return false;
    }
    let children = ["shapes", "children"]
        .into_iter()
        .filter_map(|key| shape.get(key).and_then(Value::as_array))
        .flatten()
        .collect::<Vec<_>>();
    !children.is_empty() && children.into_iter().all(tutor_draw_shape_is_clear)
}

fn validate_tutor_draw_payload_node(
    shape: &Value,
    depth: usize,
    source_entity_ids: &mut Vec<String>,
) -> Result<(), String> {
    if depth > TUTOR_DRAW_GROUP_MAX_DEPTH {
        return Err(format!(
            "draw group nesting exceeds maximum depth of {TUTOR_DRAW_GROUP_MAX_DEPTH}"
        ));
    }
    let object = shape
        .as_object()
        .ok_or_else(|| "draw payload must be a JSON object".to_string())?;
    let shape_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "draw payload requires non-empty `type`".to_string())?;
    if !is_supported_tutor_draw_shape_type(shape_type) {
        return Err(format!("unsupported draw shape type `{shape_type}`"));
    }
    append_shape_source_entity_ids(object.get("source_entity_ids"), source_entity_ids)?;
    append_shape_source_entity_id(object.get("source_entity_id"), source_entity_ids)?;
    validate_shape_geometry_numbers(object, shape_type)?;
    validate_shape_reveal_metadata(object, shape_type)?;
    if shape_type == "group" {
        let children = object
            .get("shapes")
            .or_else(|| object.get("children"))
            .ok_or_else(|| "draw group requires `shapes` or `children`".to_string())?;
        let children = children
            .as_array()
            .ok_or_else(|| "draw group `shapes` must be an array".to_string())?;
        if children.is_empty() {
            return Err("draw group must contain at least one child shape".to_string());
        }
        for child in children {
            validate_tutor_draw_payload_node(child, depth + 1, source_entity_ids)?;
        }
    }
    Ok(())
}

fn validate_tutor_chart_frame_layout(shape: &Value) -> Result<(), String> {
    let Some(object) = shape.as_object() else {
        return Ok(());
    };
    let shape_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if shape_type != "group" {
        return Ok(());
    }
    let Some(children) = object
        .get("shapes")
        .or_else(|| object.get("children"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    validate_tutor_chart_frame_group(children)?;
    for child in children {
        validate_tutor_chart_frame_layout(child)?;
    }
    Ok(())
}

fn validate_tutor_chart_frame_group(children: &[Value]) -> Result<(), String> {
    let Some(frame) = tutor_chart_frame_from_axes(children) else {
        return Ok(());
    };
    let slack = tutor_chart_frame_slack(frame);
    for child in children {
        let Some(shape_type) = tutor_chart_plotted_shape_type(child) else {
            continue;
        };
        let Some(bounds) = tutor_draw_shape_bounds(child) else {
            continue;
        };
        if !bounds.has_extent(TUTOR_CHART_FRAME_MIN_SERIES_EXTENT) {
            continue;
        }
        if !frame.contains_with_slack(bounds, slack) {
            return Err(format!(
                "chart frame layout invalid: plotted `{shape_type}` extends outside the axis frame. Keep plotted path/curve/freehand series inside the rectangle bounded by the horizontal and vertical axes, or enlarge/reposition the axes to cover the whole plotted series"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct TutorDrawBounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl TutorDrawBounds {
    fn from_point(x: f64, y: f64) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
        }
    }

    fn include_point(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    fn height(self) -> f64 {
        self.max_y - self.min_y
    }

    fn has_extent(self, min_extent: f64) -> bool {
        self.width().abs() >= min_extent || self.height().abs() >= min_extent
    }

    fn contains_with_slack(self, other: TutorDrawBounds, slack: f64) -> bool {
        other.min_x >= self.min_x - slack
            && other.max_x <= self.max_x + slack
            && other.min_y >= self.min_y - slack
            && other.max_y <= self.max_y + slack
    }
}

fn tutor_chart_frame_slack(frame: TutorDrawBounds) -> f64 {
    TUTOR_CHART_FRAME_MIN_SLACK_PX.max(frame.width().max(frame.height()) * 0.08)
}

fn tutor_chart_frame_from_axes(children: &[Value]) -> Option<TutorDrawBounds> {
    let mut horizontal: Option<TutorDrawBounds> = None;
    let mut vertical: Option<TutorDrawBounds> = None;
    for child in children {
        if tutor_draw_shape_type(child) != Some("axis") {
            continue;
        }
        let bounds = tutor_segment_bounds(child)?;
        let dx = bounds.width().abs();
        let dy = bounds.height().abs();
        if dx >= dy && dx >= TUTOR_CHART_FRAME_MIN_AXIS_LENGTH {
            if horizontal
                .map(|current| dx > current.width().abs())
                .unwrap_or(true)
            {
                horizontal = Some(bounds);
            }
        } else if dy > dx
            && dy >= TUTOR_CHART_FRAME_MIN_AXIS_LENGTH
            && vertical
                .map(|current| dy > current.height().abs())
                .unwrap_or(true)
        {
            vertical = Some(bounds);
        }
    }
    let horizontal = horizontal?;
    let vertical = vertical?;
    let frame = TutorDrawBounds {
        min_x: horizontal.min_x.min(horizontal.max_x),
        max_x: horizontal.min_x.max(horizontal.max_x),
        min_y: vertical.min_y.min(vertical.max_y),
        max_y: vertical.min_y.max(vertical.max_y),
    };
    if frame.width() >= TUTOR_CHART_FRAME_MIN_AXIS_LENGTH
        && frame.height() >= TUTOR_CHART_FRAME_MIN_AXIS_LENGTH
    {
        Some(frame)
    } else {
        None
    }
}

fn tutor_chart_plotted_shape_type(shape: &Value) -> Option<&str> {
    match tutor_draw_shape_type(shape)? {
        "path" | "curve" | "freehand" => tutor_draw_shape_type(shape),
        _ => None,
    }
}

fn tutor_draw_shape_type(shape: &Value) -> Option<&str> {
    shape
        .as_object()?
        .get("type")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tutor_draw_shape_bounds(shape: &Value) -> Option<TutorDrawBounds> {
    let object = shape.as_object()?;
    match tutor_draw_shape_type(shape)? {
        "path" => tutor_svg_path_bounds(
            object
                .get("d")
                .or_else(|| object.get("path"))?
                .as_str()?
                .trim(),
        ),
        "curve" => tutor_curve_bounds(shape),
        "freehand" => object.get("points").and_then(tutor_points_bounds),
        _ => None,
    }
}

fn tutor_curve_bounds(shape: &Value) -> Option<TutorDrawBounds> {
    let object = shape.as_object()?;
    let mut bounds = tutor_segment_bounds(shape)?;
    for fields in [
        ["control_x", "control_y"],
        ["control1_x", "control1_y"],
        ["control2_x", "control2_y"],
        ["c1x", "c1y"],
        ["c2x", "c2y"],
    ] {
        if let Some((x, y)) = tutor_numeric_point(object, &fields) {
            bounds.include_point(x, y);
        }
    }
    Some(bounds)
}

fn tutor_segment_bounds(shape: &Value) -> Option<TutorDrawBounds> {
    let object = shape.as_object()?;
    let start = tutor_numeric_point(object, &["x1", "y1"])
        .or_else(|| tutor_numeric_point(object, &["from_x", "from_y"]))?;
    let end = tutor_numeric_point(object, &["x2", "y2"])
        .or_else(|| tutor_numeric_point(object, &["to_x", "to_y"]))?;
    let mut bounds = TutorDrawBounds::from_point(start.0, start.1);
    bounds.include_point(end.0, end.1);
    Some(bounds)
}

fn tutor_numeric_point(
    object: &serde_json::Map<String, Value>,
    fields: &[&str; 2],
) -> Option<(f64, f64)> {
    let x = object.get(fields[0])?.as_f64()?;
    let y = object.get(fields[1])?.as_f64()?;
    if x.is_finite() && y.is_finite() {
        Some((x, y))
    } else {
        None
    }
}

fn tutor_points_bounds(points: &Value) -> Option<TutorDrawBounds> {
    let points = points.as_array()?;
    let mut bounds: Option<TutorDrawBounds> = None;
    for point in points {
        let point = if let Some(values) = point.as_array() {
            let x = values.first()?.as_f64()?;
            let y = values.get(1)?.as_f64()?;
            (x, y)
        } else if let Some(object) = point.as_object() {
            let x = object.get("x")?.as_f64()?;
            let y = object.get("y")?.as_f64()?;
            (x, y)
        } else {
            continue;
        };
        if !point.0.is_finite() || !point.1.is_finite() {
            continue;
        }
        if let Some(existing) = bounds.as_mut() {
            existing.include_point(point.0, point.1);
        } else {
            bounds = Some(TutorDrawBounds::from_point(point.0, point.1));
        }
    }
    bounds
}

fn tutor_svg_path_bounds(path: &str) -> Option<TutorDrawBounds> {
    let values = tutor_svg_path_numeric_values(path);
    if values.len() < 4 {
        return None;
    }
    let mut bounds: Option<TutorDrawBounds> = None;
    for pair in values.chunks_exact(2) {
        let x = pair[0];
        let y = pair[1];
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        if let Some(existing) = bounds.as_mut() {
            existing.include_point(x, y);
        } else {
            bounds = Some(TutorDrawBounds::from_point(x, y));
        }
    }
    bounds
}

fn tutor_svg_path_numeric_values(path: &str) -> Vec<f64> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;
    for ch in path.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == 'e' || ch == 'E' {
            current.push(ch);
        } else if ch == '-' || ch == '+' {
            if current.is_empty() || matches!(previous, Some('e' | 'E')) {
                current.push(ch);
            } else {
                if let Ok(value) = current.parse::<f64>() {
                    values.push(value);
                }
                current.clear();
                current.push(ch);
            }
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<f64>() {
                values.push(value);
            }
            current.clear();
        }
        previous = Some(ch);
    }
    if !current.is_empty() {
        if let Ok(value) = current.parse::<f64>() {
            values.push(value);
        }
    }
    values
}

fn validate_shape_reveal_metadata(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    validate_optional_string_field(
        object,
        "storyboard_step_id",
        TUTOR_DRAW_MAX_STORYBOARD_STEP_ID_CHARS,
        shape_type,
    )?;
    validate_optional_string_field(
        object,
        "reveal_id",
        TUTOR_DRAW_MAX_REVEAL_ID_CHARS,
        shape_type,
    )?;
    validate_optional_string_field(
        object,
        "persist_until_step",
        TUTOR_DRAW_MAX_REVEAL_ID_CHARS,
        shape_type,
    )?;
    validate_optional_string_field(
        object,
        "tutor_step_label",
        TUTOR_DRAW_MAX_STEP_LABEL_CHARS,
        shape_type,
    )?;
    validate_optional_string_field(
        object,
        "step_label",
        TUTOR_DRAW_MAX_STEP_LABEL_CHARS,
        shape_type,
    )?;
    validate_optional_string_field(
        object,
        "narration",
        TUTOR_DRAW_MAX_NARRATION_CHARS,
        shape_type,
    )?;
    validate_optional_bool_field(object, "wait_for_voice", shape_type)?;
    validate_optional_bool_field(object, "clear_previous", shape_type)?;
    validate_optional_u64_field(
        object,
        "reveal_order",
        0,
        TUTOR_DRAW_MAX_REVEAL_ORDER,
        shape_type,
    )?;
    validate_optional_u64_field(object, "delay_ms", 0, TUTOR_DRAW_MAX_DELAY_MS, shape_type)?;
    validate_optional_u64_field(
        object,
        "duration_ms",
        1,
        TUTOR_DRAW_MAX_DURATION_MS,
        shape_type,
    )?;
    Ok(())
}

fn validate_optional_string_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    max_chars: usize,
    shape_type: &str,
) -> Result<(), String> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(format!(
            "draw shape `{shape_type}` field `{field}` must be a non-empty string"
        ));
    };
    if text.chars().count() > max_chars {
        return Err(format!(
            "draw shape `{shape_type}` field `{field}` exceeds {max_chars} characters"
        ));
    }
    Ok(())
}

fn validate_optional_bool_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    shape_type: &str,
) -> Result<(), String> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    if value.as_bool().is_none() {
        return Err(format!(
            "draw shape `{shape_type}` field `{field}` must be a boolean"
        ));
    }
    Ok(())
}

fn validate_optional_u64_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    min: u64,
    max: u64,
    shape_type: &str,
) -> Result<(), String> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let number = json_value_as_u64(value).ok_or_else(|| {
        format!("draw shape `{shape_type}` field `{field}` must be a non-negative integer")
    })?;
    if number < min || number > max {
        return Err(format!(
            "draw shape `{shape_type}` field `{field}` must be between {min} and {max}"
        ));
    }
    Ok(())
}

fn json_value_as_u64(value: &Value) -> Option<u64> {
    if let Some(value) = value.as_u64() {
        return Some(value);
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    if number > u64::MAX as f64 {
        return None;
    }
    Some(number as u64)
}

fn append_shape_source_entity_ids(
    value: Option<&Value>,
    source_entity_ids: &mut Vec<String>,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(values) = value.as_array() else {
        return Err("source_entity_ids must be an array of strings".to_string());
    };
    for value in values {
        append_shape_source_entity_id(Some(value), source_entity_ids)?;
    }
    Ok(())
}

fn append_shape_source_entity_id(
    value: Option<&Value>,
    source_entity_ids: &mut Vec<String>,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(id) = value.as_str().map(str::trim).filter(|id| !id.is_empty()) else {
        return Err(
            "source_entity_id/source_entity_ids must contain non-empty strings".to_string(),
        );
    };
    source_entity_ids.push(id.to_string());
    Ok(())
}

fn validate_shape_geometry_numbers(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    for key in [
        "x",
        "y",
        "w",
        "h",
        "x1",
        "y1",
        "x2",
        "y2",
        "cx",
        "cy",
        "r",
        "radius",
        "size",
        "start_angle",
        "end_angle",
        "from_x",
        "from_y",
        "to_x",
        "to_y",
        "control_x",
        "control_y",
        "control1_x",
        "control1_y",
        "control2_x",
        "control2_y",
        "c1x",
        "c1y",
        "c2x",
        "c2y",
        "font_size",
    ] {
        if let Some(value) = object.get(key) {
            let Some(number) = value.as_f64() else {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` must be numeric"
                ));
            };
            if !number.is_finite() {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` must be finite"
                ));
            }
            if matches!(key, "w" | "h" | "r" | "radius" | "size" | "font_size") && number < 0.0 {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` cannot be negative"
                ));
            }
        }
    }
    if let Some(points) = object.get("points") {
        validate_shape_points(points, shape_type)?;
    }
    validate_shape_required_geometry(object, shape_type)?;
    Ok(())
}

fn validate_shape_required_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    match shape_type {
        "clear" | "group" => Ok(()),
        "line" | "arrow" | "axis" | "trajectory" | "field_line" | "measurement_tick"
        | "vector_arrow" | "force_arrow" | "component_vector" | "pointer_arrow" | "flow_edge"
        | "side_label" | "square_on_segment" => require_shape_segment_geometry(object, shape_type),
        "rect" | "highlight" | "mask" | "spotlight" | "free_body_body" | "code_highlight"
        | "stack_frame" | "heap_object" | "state_box" | "flow_node" | "memory_cell" => {
            require_shape_rect_geometry(object, shape_type)
        },
        "polygon" => require_shape_points_geometry(object, shape_type),
        "path" => require_shape_path_geometry(object, shape_type),
        "curve" => require_shape_curve_geometry(object, shape_type),
        "freehand" => require_shape_points_geometry(object, shape_type),
        "area_fill" => {
            if object.get("points").is_some() {
                require_shape_points_geometry(object, shape_type)
            } else {
                require_shape_rect_geometry(object, shape_type)
            }
        },
        // `cone`/`sector` derive every point from a centre and a radius — the
        // cone's `r` is its base radius, the sector's is its slant length — so
        // they need exactly what a circle needs. Validating here rather than
        // falling through to `_ => Ok(())` means a solid missing its defining
        // measurement is refused at the seam instead of drawing as nothing.
        "circle" | "cone" | "sector" => require_shape_circle_geometry(object, shape_type),
        "arc"
        | "angle_marker"
        | "right_angle_marker"
        | "perpendicular_marker"
        | "parallel_marker" => require_shape_anchor_geometry(object, shape_type),
        "label" | "callout" | "formula" | "unit_label" | "timeline_tick" => {
            require_shape_anchor_geometry(object, shape_type)
        },
        "handwriting" | "cursive_text" => {
            require_shape_anchor_geometry(object, shape_type)?;
            require_shape_text_geometry(object, shape_type)
        },
        _ => Ok(()),
    }
}

fn require_shape_rect_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    require_numeric_fields(object, shape_type, &["x", "y", "w", "h"])
}

fn require_shape_segment_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    let has_x1y1 = has_numeric_fields(object, &["x1", "y1"]);
    let has_from = has_numeric_fields(object, &["from_x", "from_y"]);
    let has_x2y2 = has_numeric_fields(object, &["x2", "y2"]);
    let has_to = has_numeric_fields(object, &["to_x", "to_y"]);
    if (has_x1y1 || has_from) && (has_x2y2 || has_to) {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires segment coordinates (`x1`,`y1`,`x2`,`y2`) or (`from_x`,`from_y`,`to_x`,`to_y`)"
    ))
}

fn require_shape_points_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if object.get("points").is_some() {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires non-empty `points`"
    ))
}

fn require_shape_text_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if object
        .get("text")
        .or_else(|| object.get("label"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires non-empty `text` or `label`"
    ))
}

fn require_shape_path_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    let Some(path) = object
        .get("d")
        .or_else(|| object.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(format!(
            "draw shape `{shape_type}` requires SVG path data in `d` or `path`"
        ));
    };
    if path.len() > TUTOR_DRAW_MAX_PATH_CHARS {
        return Err(format!(
            "draw shape `{shape_type}` path data must be {TUTOR_DRAW_MAX_PATH_CHARS} characters or fewer"
        ));
    }
    if !path.chars().all(is_supported_svg_path_char) {
        return Err(format!(
            "draw shape `{shape_type}` path data contains unsupported characters"
        ));
    }
    Ok(())
}

fn is_supported_svg_path_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            'M' | 'm'
                | 'Z'
                | 'z'
                | 'L'
                | 'l'
                | 'H'
                | 'h'
                | 'V'
                | 'v'
                | 'C'
                | 'c'
                | 'S'
                | 's'
                | 'Q'
                | 'q'
                | 'T'
                | 't'
                | 'A'
                | 'a'
                | 'E'
                | 'e'
                | ','
                | '.'
                | '-'
                | '+'
                | ' '
                | '\n'
                | '\r'
                | '\t'
        )
}

fn require_shape_curve_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    require_shape_segment_geometry(object, shape_type)?;
    let has_control = has_numeric_fields(object, &["control_x", "control_y"])
        || has_numeric_fields(object, &["c1x", "c1y"])
        || has_numeric_fields(object, &["control1_x", "control1_y"]);
    if has_control {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires a Bezier control point (`control_x`,`control_y`) or cubic controls (`control1_x`,`control1_y`,`control2_x`,`control2_y`)"
    ))
}

fn require_shape_circle_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if (has_numeric_fields(object, &["cx", "cy"]) || has_numeric_fields(object, &["x", "y"]))
        && (has_numeric_field(object, "r") || has_numeric_field(object, "radius"))
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires center (`cx`,`cy`) or (`x`,`y`) plus `r` or `radius`"
    ))
}

fn require_shape_anchor_geometry(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if has_numeric_fields(object, &["x", "y"])
        || has_numeric_fields(object, &["cx", "cy"])
        || (has_numeric_fields(object, &["x1", "y1"])
            || has_numeric_fields(object, &["from_x", "from_y"]))
        || (has_numeric_fields(object, &["x2", "y2"])
            || has_numeric_fields(object, &["to_x", "to_y"]))
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires an anchor point such as (`x`,`y`), (`cx`,`cy`), or segment coordinates"
    ))
}

fn require_numeric_fields(
    object: &serde_json::Map<String, Value>,
    shape_type: &str,
    fields: &[&str],
) -> Result<(), String> {
    if has_numeric_fields(object, fields) {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires numeric fields: {}",
        fields.join(", ")
    ))
}

fn has_numeric_fields(object: &serde_json::Map<String, Value>, fields: &[&str]) -> bool {
    fields.iter().all(|field| has_numeric_field(object, field))
}

fn has_numeric_field(object: &serde_json::Map<String, Value>, field: &str) -> bool {
    object
        .get(field)
        .and_then(Value::as_f64)
        .map(f64::is_finite)
        .unwrap_or(false)
}

fn validate_shape_points(points: &Value, shape_type: &str) -> Result<(), String> {
    let Some(points) = points.as_array() else {
        return Err(format!("draw shape `{shape_type}` points must be an array"));
    };
    if points.is_empty() {
        return Err(format!("draw shape `{shape_type}` points cannot be empty"));
    }
    for point in points {
        if let Some(values) = point.as_array() {
            if values.len() < 2 {
                return Err(format!(
                    "draw shape `{shape_type}` point arrays must contain x and y"
                ));
            }
            for value in values.iter().take(2) {
                let Some(number) = value.as_f64() else {
                    return Err(format!(
                        "draw shape `{shape_type}` point values must be numeric"
                    ));
                };
                if !number.is_finite() {
                    return Err(format!(
                        "draw shape `{shape_type}` point values must be finite"
                    ));
                }
            }
        } else if let Some(object) = point.as_object() {
            for key in ["x", "y"] {
                let Some(number) = object.get(key).and_then(Value::as_f64) else {
                    return Err(format!(
                        "draw shape `{shape_type}` point objects require numeric `{key}`"
                    ));
                };
                if !number.is_finite() {
                    return Err(format!(
                        "draw shape `{shape_type}` point object `{key}` must be finite"
                    ));
                }
            }
        } else {
            return Err(format!(
                "draw shape `{shape_type}` points must be arrays or objects"
            ));
        }
    }
    Ok(())
}

fn dedupe_preserving_order(values: Vec<String>) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if !seen.insert(value.clone()) {
            continue;
        }
        deduped.push(value);
    }
    Ok(deduped)
}

fn validate_step_source_entity_refs(run: &TutorRun, step: &TutorStep) -> Result<(), String> {
    if step.source_entity_ids.is_empty() {
        return Ok(());
    }
    if step.kind == TutorStepKind::Observe {
        return Err("observe steps cannot reference source_entity_ids".to_string());
    }
    if !run.has_fresh_observation {
        return Err(
            "source_entity_ids require a fresh observation; re-observe before using entity ids"
                .to_string(),
        );
    }
    let Some(map) = run.latest_visual_entity_map.as_ref() else {
        return Err("source_entity_ids require a latest visual_entity_map".to_string());
    };
    let entity_ids = map.entity_ids();
    let missing = step
        .source_entity_ids
        .iter()
        .filter(|id| !entity_ids.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "unknown source_entity_ids for latest observation `{}`: {}",
            map.observation_id,
            missing.join(", ")
        ));
    }
    Ok(())
}

fn compact_json_value(value: &Value, max_chars: usize) -> String {
    if value.is_null() {
        return "null".to_string();
    }
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string());
    if rendered.chars().count() <= max_chars {
        return rendered;
    }
    let mut truncated = rendered
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

pub fn parse_tutor_action_envelope(value: &Value) -> Result<TutorActionEnvelope, String> {
    let envelope: TutorActionEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid tutor_action envelope: {error}"))?;
    validate_tutor_action_envelope_shape(&envelope)?;
    Ok(envelope)
}

pub fn extract_tutor_action_result(text: &str) -> Option<Result<TutorActionResult, String>> {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(raw_json) = trimmed.strip_prefix("TUTOR_ACTION_RESULT:") else {
            continue;
        };
        return Some(parse_tutor_action_result_json(raw_json.trim()));
    }
    None
}

pub fn apply_tutor_action_result_from_text(
    text: &str,
) -> Option<Result<TutorActionResultApplication, String>> {
    let result = match extract_tutor_action_result(text)? {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };
    Some(tutor_run_store().apply_action_result(result))
}

fn parse_tutor_action_result_json(raw_json: &str) -> Result<TutorActionResult, String> {
    if raw_json.is_empty() {
        return Err("TUTOR_ACTION_RESULT line is missing JSON payload".to_string());
    }
    let value: Value = serde_json::from_str(raw_json)
        .map_err(|error| format!("TUTOR_ACTION_RESULT JSON is invalid: {error}"))?;
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "TUTOR_ACTION_RESULT requires non-empty `run_id`".to_string())?
        .to_string();
    let status = match value
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
    {
        "succeeded" | "success" | "ok" => TutorActionResultStatus::Succeeded,
        "failed" | "failure" | "error" => TutorActionResultStatus::Failed,
        other => {
            return Err(format!(
                "TUTOR_ACTION_RESULT status `{other}` is unsupported; expected succeeded or failed"
            ));
        },
    };
    let evidence = value
        .get("evidence")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "TUTOR_ACTION_RESULT requires non-empty `evidence`".to_string())?
        .to_string();
    Ok(TutorActionResult {
        run_id,
        status,
        evidence,
    })
}

pub fn validate_tutor_action_envelope_for_run(
    run: &TutorRun,
    envelope: &TutorActionEnvelope,
) -> Result<TutorStep, String> {
    validate_tutor_action_envelope_shape(envelope)?;
    if envelope.run_id != run.run_id {
        return Err(format!(
            "tutor_action run_id `{}` does not match active tutor run `{}`",
            envelope.run_id, run.run_id
        ));
    }
    if envelope.step_kind.changes_ui_state() && envelope.safety == TutorSafetyLevel::VisualOnly {
        return Err("UI-changing tutor actions cannot use visual_only safety".to_string());
    }
    if envelope.step_kind.changes_ui_state()
        && !run.has_successful_draw_storyboard_binding(
            envelope.storyboard_step_id.as_deref(),
            envelope.storyboard_step_label.as_deref(),
        )
    {
        return Err(
            "UI-changing tutor_action must reference a prior successful screen-draw storyboard step in this run"
                .to_string(),
        );
    }
    if envelope.step_kind.changes_ui_state()
        && run.is_app_copilot()
        && !run.has_matching_copilot_action_check(envelope)
    {
        return Err(
            "App Copilot automation requires a fresh no-user-action check for the same storyboard step immediately before delegation"
                .to_string(),
        );
    }
    if !matches!(
        envelope.step_kind,
        TutorStepKind::Click
            | TutorStepKind::TypeText
            | TutorStepKind::Hotkey
            | TutorStepKind::Scroll
            | TutorStepKind::Verify
    ) {
        return Err(format!(
            "tutor_action step_kind `{}` is not a mac-operator action kind",
            tutor_step_kind_name(envelope.step_kind)
        ));
    }
    let step = envelope.to_step();
    validate_next_tutor_step(run, &step)?;
    Ok(step)
}

// `storyboard_identity_matches_receipt` moved to `copilot_rail` with the
// receipt check that reads it.

fn validate_tutor_action_envelope_shape(envelope: &TutorActionEnvelope) -> Result<(), String> {
    if envelope.run_id.trim().is_empty() {
        return Err("tutor_action requires non-empty `run_id`".to_string());
    }
    if envelope.target.trim().is_empty() {
        return Err("tutor_action requires non-empty `target`".to_string());
    }
    if envelope.expected_state.trim().is_empty() {
        return Err("tutor_action requires non-empty `expected_state`".to_string());
    }
    if envelope.observation_evidence.trim().is_empty() {
        return Err("tutor_action requires non-empty `observation_evidence`".to_string());
    }
    if envelope.step_kind.changes_ui_state()
        && envelope
            .storyboard_step_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && envelope
            .storyboard_step_label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(
            "UI-changing tutor_action requires `storyboard_step_id` or `storyboard_step_label` from the preceding screen-draw preview"
                .to_string(),
        );
    }
    Ok(())
}

fn apply_step_state_transition(run: &mut TutorRun, step: &TutorStep) {
    match step.kind {
        TutorStepKind::Observe => {
            run.has_fresh_observation = true;
            run.has_resolved_target = false;
            run.latest_visual_entity_map = step.visual_entity_map.clone();
            run.destructive_confirmed = false;
        },
        TutorStepKind::ResolveTarget => {
            run.has_resolved_target = true;
        },
        TutorStepKind::Confirm => {
            run.destructive_confirmed = true;
        },
        TutorStepKind::Verify => {
            run.pending_action = None;
            run.pending_action_execution_id = None;
            run.pending_created_object = None;
            run.destructive_confirmed = false;
        },
        kind if kind.changes_ui_state() => {
            run.pending_action = Some(step.clone());
            run.pending_action_execution_id = None;
            run.has_fresh_observation = false;
            run.has_resolved_target = false;
            run.latest_visual_entity_map = None;
            run.destructive_confirmed = false;
        },
        TutorStepKind::Recover => {
            run.has_fresh_observation = false;
            run.has_resolved_target = false;
            run.latest_visual_entity_map = None;
            run.destructive_confirmed = false;
        },
        _ => {},
    }
}

fn tutor_step_kind_name(kind: TutorStepKind) -> &'static str {
    match kind {
        TutorStepKind::Observe => "observe",
        TutorStepKind::ResolveTarget => "resolve_target",
        TutorStepKind::Draw => "draw",
        TutorStepKind::Say => "say",
        TutorStepKind::Wait => "wait",
        TutorStepKind::Click => "click",
        TutorStepKind::TypeText => "type_text",
        TutorStepKind::Hotkey => "hotkey",
        TutorStepKind::Scroll => "scroll",
        TutorStepKind::Verify => "verify",
        TutorStepKind::ClearDrawings => "clear_drawings",
        TutorStepKind::Confirm => "confirm",
        TutorStepKind::Recover => "recover",
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn step(kind: TutorStepKind, label: &str) -> TutorStep {
        TutorStep {
            kind,
            label: label.to_string(),
            target: None,
            expected_state: None,
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        }
    }

    /// **Prose does not teach a milestone.** A draw built only from `formula`
    /// and `callout` is as text-only as a `say` step, which the contract
    /// already refuses to count — but it used to satisfy coverage, so "explain
    /// the hard part with a diagram" was advice the runtime could not enforce.
    #[test]
    fn a_text_only_draw_narrates_but_does_not_cover_a_milestone() {
        let text_only = serde_json::json!({
            "type": "group",
            "storyboard_step_id": "why-it-unrolls",
            "tutor_step_label": "Why it unrolls",
            "narration": "The curved side flattens into a sector.",
            "shapes": [
                { "type": "formula", "text": "A = pi r l", "x": 10, "y": 10 },
                { "type": "callout", "text": "note the slant", "x": 10, "y": 40 }
            ]
        });
        let steps = validate_tutor_draw_storyboard_payload(&text_only)
            .expect("a text-only draw is still a valid, renderable payload");
        assert_eq!(steps.len(), 1, "one semantic step");
        assert!(
            !steps[0].figure_backed,
            "formula + callout is prose; it must not tick a milestone"
        );

        // The same step, with one real figure among the words, does count.
        let with_figure = serde_json::json!({
            "type": "group",
            "storyboard_step_id": "why-it-unrolls",
            "tutor_step_label": "Why it unrolls",
            "narration": "The curved side flattens into a sector.",
            "shapes": [
                { "type": "formula", "text": "A = pi r l", "x": 10, "y": 10 },
                { "type": "sector", "cx": 100, "cy": 100, "r": 80, "size": 30 }
            ]
        });
        let steps = validate_tutor_draw_storyboard_payload(&with_figure).expect("valid");
        assert_eq!(steps.len(), 1, "still one semantic step after dedupe");
        assert!(
            steps[0].figure_backed,
            "the flag must OR across children, not depend on their order"
        );
    }

    #[test]
    fn every_shipped_text_primitive_is_classified_as_prose() {
        for prose in ["label", "callout", "formula", "unit_label", "side_label"] {
            assert!(
                !tutor_shape_type_is_figure(prose),
                "{prose} draws only words"
            );
        }
        // And a new/unknown primitive is a figure by default — the safe
        // direction, since a misclassified figure costs nothing while a
        // misclassified text shape would let prose satisfy a visual milestone.
        for figure in [
            "cone",
            "sector",
            "arc",
            "path",
            "polygon",
            "brand_new_primitive",
        ] {
            assert!(
                tutor_shape_type_is_figure(figure),
                "{figure} puts marks on the canvas"
            );
        }
    }

    fn storyboard_step(step_id: &str, label: &str) -> TutorDrawStoryboardStep {
        TutorDrawStoryboardStep {
            step_id: step_id.to_string(),
            label: label.to_string(),
            narration: format!("Preview {label}."),
            // These fixtures stand in for taught milestones, so they are
            // figure-backed; the text-only case has its own test below.
            figure_backed: true,
        }
    }

    /// A `core` milestone — what a deep lesson's hard step is made of.
    fn core_milestone(milestone_id: &str, objective: &str) -> TutorLessonMilestone {
        TutorLessonMilestone {
            role: Some(TutorMilestoneRole::Core),
            ..lesson_milestone(milestone_id, objective)
        }
    }

    fn lesson_milestone(milestone_id: &str, objective: &str) -> TutorLessonMilestone {
        TutorLessonMilestone {
            milestone_id: milestone_id.to_string(),
            objective: objective.to_string(),
            role: None,
        }
    }

    fn sample_visual_entity_map() -> TutorVisualEntityMap {
        TutorVisualEntityMap {
            observation_id: "obs-1".to_string(),
            coordinate_space: TutorCoordinateSpace {
                width: 2048,
                height: 1152,
                unit: Some("model_points".to_string()),
            },
            screen_hash: Some("screen-a".to_string()),
            display_id: Some("main".to_string()),
            entities: vec![
                TutorVisualEntity {
                    id: "line-main".to_string(),
                    kind: TutorVisualEntityKind::LineSegment,
                    label: Some("increasing line".to_string()),
                    text: None,
                    confidence: Some(91),
                    geometry: serde_json::json!({"x1": 160, "y1": 440, "x2": 620, "y2": 180}),
                    source_evidence: vec!["visible plotted line".to_string()],
                },
                TutorVisualEntity {
                    id: "equation-main".to_string(),
                    kind: TutorVisualEntityKind::FormulaRegion,
                    label: Some("linear equation".to_string()),
                    text: Some("y = 2x + 1".to_string()),
                    confidence: Some(88),
                    geometry: serde_json::json!({"x": 180, "y": 220, "w": 220, "h": 42}),
                    source_evidence: vec!["visible equation text".to_string()],
                },
            ],
        }
    }

    fn sample_math_entity_map() -> TutorVisualEntityMap {
        TutorVisualEntityMap {
            observation_id: "obs-math-1".to_string(),
            coordinate_space: TutorCoordinateSpace {
                width: 2048,
                height: 1152,
                unit: Some("model_points".to_string()),
            },
            screen_hash: Some("screen-math-a".to_string()),
            display_id: Some("main".to_string()),
            entities: vec![
                TutorVisualEntity {
                    id: "axis-x".to_string(),
                    kind: TutorVisualEntityKind::Axis,
                    label: Some("x-axis".to_string()),
                    text: None,
                    confidence: Some(92),
                    geometry: serde_json::json!({"x1": 100, "y1": 500, "x2": 700, "y2": 500}),
                    source_evidence: vec!["visible graph axis".to_string()],
                },
                TutorVisualEntity {
                    id: "axis-y".to_string(),
                    kind: TutorVisualEntityKind::Axis,
                    label: Some("y-axis".to_string()),
                    text: None,
                    confidence: Some(92),
                    geometry: serde_json::json!({"x1": 100, "y1": 500, "x2": 100, "y2": 120}),
                    source_evidence: vec!["visible graph axis".to_string()],
                },
                TutorVisualEntity {
                    id: "line-main".to_string(),
                    kind: TutorVisualEntityKind::LineSegment,
                    label: Some("increasing line".to_string()),
                    text: None,
                    confidence: Some(90),
                    geometry: serde_json::json!({"x1": 160, "y1": 440, "x2": 620, "y2": 180}),
                    source_evidence: vec!["visible plotted line".to_string()],
                },
                TutorVisualEntity {
                    id: "equation-main".to_string(),
                    kind: TutorVisualEntityKind::FormulaRegion,
                    label: Some("linear equation".to_string()),
                    text: Some("2x + 3 = 11".to_string()),
                    confidence: Some(89),
                    geometry: serde_json::json!({"x": 180, "y": 220, "w": 220, "h": 42}),
                    source_evidence: vec!["visible equation text".to_string()],
                },
                TutorVisualEntity {
                    id: "region-main".to_string(),
                    kind: TutorVisualEntityKind::Region,
                    label: Some("shaded comparison region".to_string()),
                    text: None,
                    confidence: Some(87),
                    geometry: serde_json::json!({
                        "points": [[160, 440], [620, 180], [620, 500], [160, 500]]
                    }),
                    source_evidence: vec!["visible bounded graph region".to_string()],
                },
            ],
        }
    }

    fn sample_physics_entity_map() -> TutorVisualEntityMap {
        TutorVisualEntityMap {
            observation_id: "obs-physics-1".to_string(),
            coordinate_space: TutorCoordinateSpace {
                width: 2048,
                height: 1152,
                unit: Some("model_points".to_string()),
            },
            screen_hash: Some("screen-physics-a".to_string()),
            display_id: Some("main".to_string()),
            entities: vec![
                TutorVisualEntity {
                    id: "body-main".to_string(),
                    kind: TutorVisualEntityKind::DiagramNode,
                    label: Some("block".to_string()),
                    text: None,
                    confidence: Some(92),
                    geometry: serde_json::json!({"x": 360, "y": 330, "w": 140, "h": 90}),
                    source_evidence: vec!["visible free-body object".to_string()],
                },
                TutorVisualEntity {
                    id: "force-weight".to_string(),
                    kind: TutorVisualEntityKind::Vector,
                    label: Some("weight".to_string()),
                    text: Some("mg".to_string()),
                    confidence: Some(90),
                    geometry: serde_json::json!({"from_x": 430, "from_y": 375, "to_x": 430, "to_y": 520}),
                    source_evidence: vec!["visible downward force arrow".to_string()],
                },
                TutorVisualEntity {
                    id: "force-normal".to_string(),
                    kind: TutorVisualEntityKind::Vector,
                    label: Some("normal force".to_string()),
                    text: Some("N".to_string()),
                    confidence: Some(88),
                    geometry: serde_json::json!({"from_x": 430, "from_y": 375, "to_x": 430, "to_y": 235}),
                    source_evidence: vec!["visible upward force arrow".to_string()],
                },
                TutorVisualEntity {
                    id: "axis-main".to_string(),
                    kind: TutorVisualEntityKind::Axis,
                    label: Some("force axes".to_string()),
                    text: None,
                    confidence: Some(86),
                    geometry: serde_json::json!({"x1": 260, "y1": 560, "x2": 600, "y2": 560}),
                    source_evidence: vec!["visible diagram axis".to_string()],
                },
            ],
        }
    }

    fn sample_cs_entity_map() -> TutorVisualEntityMap {
        TutorVisualEntityMap {
            observation_id: "obs-cs-1".to_string(),
            coordinate_space: TutorCoordinateSpace {
                width: 2048,
                height: 1152,
                unit: Some("model_points".to_string()),
            },
            screen_hash: Some("screen-cs-a".to_string()),
            display_id: Some("main".to_string()),
            entities: vec![
                TutorVisualEntity {
                    id: "code-loop".to_string(),
                    kind: TutorVisualEntityKind::CodeRegion,
                    label: Some("recursive call".to_string()),
                    text: Some("return n * fact(n - 1)".to_string()),
                    confidence: Some(91),
                    geometry: serde_json::json!({"x": 120, "y": 180, "w": 460, "h": 44}),
                    source_evidence: vec!["visible code line".to_string()],
                },
                TutorVisualEntity {
                    id: "stack-main".to_string(),
                    kind: TutorVisualEntityKind::DiagramNode,
                    label: Some("current stack frame".to_string()),
                    text: Some("fact(3)".to_string()),
                    confidence: Some(89),
                    geometry: serde_json::json!({"x": 720, "y": 180, "w": 180, "h": 70}),
                    source_evidence: vec!["visible stack frame".to_string()],
                },
                TutorVisualEntity {
                    id: "heap-object".to_string(),
                    kind: TutorVisualEntityKind::DiagramNode,
                    label: Some("heap object".to_string()),
                    text: Some("Node".to_string()),
                    confidence: Some(86),
                    geometry: serde_json::json!({"x": 980, "y": 300, "w": 170, "h": 90}),
                    source_evidence: vec!["visible heap box".to_string()],
                },
                TutorVisualEntity {
                    id: "pointer-main".to_string(),
                    kind: TutorVisualEntityKind::DiagramEdge,
                    label: Some("reference".to_string()),
                    text: None,
                    confidence: Some(84),
                    geometry: serde_json::json!({"from_x": 900, "from_y": 215, "to_x": 980, "to_y": 345}),
                    source_evidence: vec!["visible pointer arrow".to_string()],
                },
            ],
        }
    }

    #[test]
    fn tutor_prompt_detection_accepts_primary_and_typo_markers() {
        assert!(is_tutor_prompt("@tutor explain recursion"));
        assert!(is_tutor_prompt("please @TUTUR explain the target field"));
        assert!(is_tutor_prompt("hey tutor explain recursion"));
        assert!(is_tutor_prompt("hey, tutur, explain the target field"));
        assert!(is_app_copilot_prompt("@copilot show me where to click"));
        assert!(is_app_copilot_prompt("@app-copilot show me where to click"));
        assert!(is_app_copilot_prompt("hey copilot show me where to click"));
        assert!(is_app_copilot_prompt(
            "hey app copilot show me where to click"
        ));
        assert!(is_tutor_or_app_copilot_prompt(
            "@copilot show me where to click"
        ));
        assert!(is_tutor_quick_prompt(
            "@tutor #quick explain this visible graph"
        ));
        assert!(is_tutor_quick_prompt(
            "hey copilot #quick show me the next step"
        ));
        assert!(!is_tutor_quick_prompt("#quick summarize this"));
        assert!(!is_tutor_quick_prompt(
            "@tutor #quickly explain this visible graph"
        ));
        assert!(!is_tutor_prompt("show me where to click"));
        assert!(!is_app_copilot_prompt("show me where to click"));
        assert!(!is_tutor_prompt("tutor me on this concept"));
    }

    #[test]
    fn vibedev_rail_marker_parses_the_prompt_and_the_discuss_flag() {
        let build =
            parse_vibedev_rail_invocation("@vibedev fix the footer").expect("a vibedev rail turn");
        assert!(!build.discuss, "a plain @vibedev turn builds");
        assert_eq!(build.prompt, "fix the footer");

        let discuss = parse_vibedev_rail_invocation("@vibedev #discuss should we use X")
            .expect("a vibedev rail turn");
        assert!(discuss.discuss);
        assert_eq!(
            discuss.prompt, "should we use X",
            "the flag is a mode selector, not part of the request"
        );

        // Markers and flags are matched case-insensitively like every other
        // invoke word, but the request passes through verbatim: it carries
        // identifiers, paths and file names whose case is load-bearing.
        let mixed_case =
            parse_vibedev_rail_invocation("@VibeDev Fix The Footer").expect("a vibedev rail turn");
        assert!(!mixed_case.discuss);
        assert_eq!(mixed_case.prompt, "Fix The Footer");

        let mixed_case_flag = parse_vibedev_rail_invocation("@VIBEDEV #DISCUSS Should We Use X")
            .expect("a vibedev rail turn");
        assert!(mixed_case_flag.discuss);
        assert_eq!(mixed_case_flag.prompt, "Should We Use X");
    }

    /// `@vibedev` on its own is still the rail, just with nothing to build. This
    /// layer reports what it saw; refusing an empty request is the caller's
    /// call, since only the caller can say so in the user's own thread.
    #[test]
    fn vibedev_rail_marker_alone_is_a_turn_with_an_empty_prompt() {
        let bare =
            parse_vibedev_rail_invocation("@vibedev").expect("a bare marker is still the rail");
        assert!(!bare.discuss);
        assert_eq!(bare.prompt, "", "nothing follows the marker");

        let flag_only = parse_vibedev_rail_invocation("@vibedev #discuss")
            .expect("a bare marker is still the rail");
        assert!(flag_only.discuss);
        assert_eq!(flag_only.prompt, "");
    }

    /// The marker matches as a **whole token**, in leading position only. No
    /// handle collides with `@vibedev` today, and the guard is not there for one:
    /// it is what keeps a longer handle, a quoted example and a mid-sentence
    /// mention out of the rail, and it costs nothing to keep.
    #[test]
    fn vibedev_rail_marker_does_not_match_longer_handles_or_the_bare_word() {
        for text in [
            "@vibedevops is the deploy bot",
            "@vibedev-review this diff please",
            "@vibedeveloper what changed today",
        ] {
            assert!(parse_vibedev_rail_invocation(text).is_none(), "{text}");
        }

        // Deliberately asymmetric with `@tutor`, which also answers to the bare
        // word `tutor`: the VibeDev rail is marker-only, so naming the product
        // in an ordinary sentence cannot spend compute.
        for text in [
            "vibedev fix the footer",
            "can you check what vibedev did",
            "vibedev is stuck again",
            "what does vibedev do with the repo path",
        ] {
            assert!(parse_vibedev_rail_invocation(text).is_none(), "{text}");
        }
        assert!(
            is_tutor_prompt("hey tutor explain recursion"),
            "the bare word still invokes the tutor rail; only @vibedev is strict"
        );

        // The marker opens the turn or it is not an invoke: mentioned mid
        // sentence, or quoted while explaining the feature, it stays chat.
        for text in [
            "please @vibedev fix the footer",
            "\"@vibedev fix the footer\" is how you start a build",
        ] {
            assert!(parse_vibedev_rail_invocation(text).is_none(), "{text}");
        }
    }

    #[test]
    fn an_unmarked_turn_is_not_a_vibedev_turn_and_classifies_as_before() {
        assert!(parse_vibedev_rail_invocation("can you look at the footer spacing").is_none());
        assert!(parse_vibedev_rail_invocation("").is_none());
        assert!(parse_vibedev_rail_invocation("   ").is_none());

        // The regression bar: the new rail must not disturb what the existing
        // markers already recognize, in either direction.
        assert!(!is_tutor_prompt("@vibedev fix the footer"));
        assert!(!is_app_copilot_prompt("@vibedev fix the footer"));
        assert!(is_tutor_prompt("@tutor explain recursion"));
        assert!(is_app_copilot_prompt("@copilot show me where to click"));
        // Task 4 wired the rail: the authorization-grade classifier now answers
        // `Vibedev` for a `@vibedev` turn, and it answers it BY CALLING THIS
        // PARSER — `parse_leading_feature_marker` delegates rather than
        // re-tokenizing, so the two can never drift. The pair below is that
        // equivalence, checked from this side as well as from
        // `policy_snapshot`'s tests.
        for text in [
            "@vibedev fix the footer",
            "@vibedev #discuss plan it",
            "@vibedev",
            "@vibedevops is the deploy bot",
            "please @vibedev fix the footer",
            "can you look at the footer spacing",
        ] {
            let classified =
                crate::magician_v2::execution::agentic::parse_leading_feature_invocation(text)
                    == crate::magician_v2::agents::FeatureMode::Vibedev;
            assert_eq!(
                classified,
                parse_vibedev_rail_invocation(text).is_some(),
                "{text}"
            );
        }
        assert_eq!(
            crate::magician_v2::execution::agentic::parse_leading_feature_invocation(
                "@vibedev fix the footer"
            ),
            crate::magician_v2::agents::FeatureMode::Vibedev,
            "the marker is now a lane invoke; the chat turn routes on it"
        );
    }

    /// The spoken invoke. ASR cannot produce `@vibedev`, so voice says a phrase
    /// — and it reaches the rail through the SAME parser the typed marker uses,
    /// so the classifier cannot disagree with the payload about it either.
    #[test]
    fn vibedev_rail_spoken_phrase_parses_the_prompt_and_the_plan_mode() {
        let build = parse_vibedev_rail_invocation("Start a vibedev build fix the footer spacing")
            .expect("the spoken invoke is a vibedev rail turn");
        assert!(!build.discuss);
        assert_eq!(
            build.prompt, "fix the footer spacing",
            "the phrase is a selector; the request passes through verbatim"
        );

        // `plan` is the spoken `#discuss`: a hash flag cannot be said aloud.
        let plan =
            parse_vibedev_rail_invocation("start a vibedev plan should we split ChatPanel.svelte?")
                .expect("the spoken plan invoke is a vibedev rail turn");
        assert!(plan.discuss);
        assert_eq!(plan.prompt, "should we split ChatPanel.svelte?");

        // Every accepted spelling of the phrase, and the punctuation ASR likes
        // to insert after a command.
        for text in [
            "Start a vibedev build fix the footer",
            "start the vibedev build fix the footer",
            "start vibedev build fix the footer",
            "run a vibedev build fix the footer",
            "launch a vibedev build fix the footer",
            "begin a vibe dev build fix the footer",
            "hey start a vibedev build fix the footer",
            "Start a vibedev build: fix the footer",
            "Start a vibedev build, fix the footer",
        ] {
            let invocation = parse_vibedev_rail_invocation(text).unwrap_or_else(|| {
                panic!("expected the spoken invoke to parse: {text}");
            });
            assert!(!invocation.discuss, "{text}");
            assert_eq!(invocation.prompt, "fix the footer", "{text}");
        }

        // Like the bare marker, a phrase with nothing after it is still the
        // rail — the caller refuses it in the user's own thread.
        let bare = parse_vibedev_rail_invocation("start a vibedev build")
            .expect("a bare spoken invoke is still the rail");
        assert_eq!(bare.prompt, "");

        // One reader, both spellings: the authorization-grade classifier reports
        // exactly what this parser decided.
        for text in [
            "start a vibedev build fix the footer",
            "start a vibedev plan should we split this",
            "start coding on the login page",
            "vibedev build failed on main",
        ] {
            let classified =
                crate::magician_v2::execution::agentic::parse_leading_feature_invocation(text)
                    == crate::magician_v2::agents::FeatureMode::Vibedev;
            assert_eq!(
                classified,
                parse_vibedev_rail_invocation(text).is_some(),
                "{text}"
            );
        }
    }

    /// **A false positive here starts a real build.** These are ordinary things
    /// to dictate, and each one fails a different slot of the phrase.
    #[test]
    fn vibedev_rail_spoken_phrase_does_not_fire_on_ordinary_dictation() {
        for text in [
            // Reaches the subject, then says something that is not a mode.
            "start vibedev on the login page",
            "start a vibedev review of the diff",
            "start the vibedev review with Priya",
            "start a vibedev session with the team",
            "run the vibedev",
            "open the vibedev",
            // No verb: the phrase does not open with anyone asking for a build.
            "vibedev build failed on main",
            "the vibedev build is red again",
            "vibedev build times are getting worse",
            // No subject: a "build" in this repository is as likely to be a
            // container image as a VibeDev run.
            "start a build of the docker image",
            "run the build again please",
            // Not in leading position, exactly as for the typed marker.
            "can you start a vibedev build for the footer",
            "we should start a vibedev build once the tests pass",
            "\"start a vibedev build\" is how you say it out loud",
            // Truncated: ASR cut the utterance before the mode noun.
            "start a vibedev",
            "start a vibe dev",
            // **The retired subjects.** `code` and `coding` once opened this
            // phrase, under the rail's earlier marker. Neither is the rail's
            // name, so an engineer saying either is dictating, not asking for a
            // build — which is also why "start a code review of the diff" no
            // longer even reaches the mode noun.
            "start a code build",
            "start a code plan",
            "start a coding build fix the footer",
            "start a code build fix the footer",
        ] {
            assert!(
                parse_vibedev_rail_invocation(text).is_none(),
                "this must stay ordinary speech: {text}"
            );
        }

        // And the other spoken lanes keep their own grammar untouched.
        assert!(parse_vibedev_rail_invocation("hey tutor explain recursion").is_none());
        assert!(parse_vibedev_rail_invocation("start tutor quick screen explain this").is_none());
        assert_eq!(
            parse_voice_guided_flow_invocation("Start Tutor Quick screen explain this graph")
                .map(|invocation| invocation.feature_mode),
            Some(crate::magician_v2::agents::FeatureMode::Tutor)
        );
    }

    #[test]
    fn voice_guided_flow_parser_selects_blackboard_screen_quick_and_copilot() {
        use crate::magician_v2::agents::FeatureMode;

        let blackboard =
            parse_voice_guided_flow_invocation("Hey Tutor blackboard explain recursion")
                .expect("blackboard tutor command");
        assert_eq!(blackboard.feature_mode, FeatureMode::Tutor);
        assert_eq!(blackboard.canvas_mode, TutorCanvasMode::Blackboard);
        assert!(!blackboard.quick);
        assert_eq!(
            blackboard.canonical_text,
            "@tutor blackboard explain recursion"
        );

        let quick_screen =
            parse_voice_guided_flow_invocation("Start Tutor Quick screen explain this graph")
                .expect("quick screen tutor command");
        assert_eq!(quick_screen.feature_mode, FeatureMode::Tutor);
        assert_eq!(quick_screen.canvas_mode, TutorCanvasMode::ScreenOverlay);
        assert!(quick_screen.quick);
        assert!(quick_screen.requires_screen_capture());
        assert_eq!(
            quick_screen.canonical_text,
            "@tutor #quick screen explain this graph"
        );

        let screenshot =
            parse_voice_guided_flow_invocation("Tutor take a screenshot and explain the error")
                .expect("screenshot tutor command");
        assert!(screenshot.requires_screen_capture());

        let copilot =
            parse_voice_guided_flow_invocation("App Copilot show me how to create a note")
                .expect("app copilot command");
        assert_eq!(copilot.feature_mode, FeatureMode::AppCopilot);
        assert_eq!(copilot.canvas_mode, TutorCanvasMode::ScreenOverlay);
        assert_eq!(
            copilot.canonical_text,
            "@copilot show me how to create a note"
        );
    }

    #[test]
    fn voice_guided_flow_parser_requires_a_leading_command_and_honors_blackboard() {
        assert!(
            parse_voice_guided_flow_invocation("Please ask hey tutor to explain recursion")
                .is_none()
        );
        assert!(parse_voice_guided_flow_invocation(
            "I mentioned app copilot later in this sentence"
        )
        .is_none());

        let blackboard = parse_voice_guided_flow_invocation(
            "Tutor blackboard explain what a screen reader does",
        )
        .expect("explicit blackboard command");
        assert_eq!(blackboard.canvas_mode, TutorCanvasMode::Blackboard);
        assert!(!blackboard.requires_screen_capture());

        let copilot = parse_voice_guided_flow_invocation(
            "App Copilot blackboard show me how to create a note",
        )
        .expect("app copilot command");
        assert_eq!(copilot.canvas_mode, TutorCanvasMode::ScreenOverlay);
        assert!(copilot.requires_screen_capture());

        for text in [
            "Tutor explain my app",
            "Tutor explain the current window",
            "Tutor explain why the screenshot is blurry",
        ] {
            let invocation =
                parse_voice_guided_flow_invocation(text).expect("source-free tutor command");
            assert_eq!(
                invocation.canvas_mode,
                TutorCanvasMode::Blackboard,
                "{text}"
            );
            assert!(!invocation.requires_screen_capture(), "{text}");
        }

        let screen_with_blackboard_topic =
            parse_voice_guided_flow_invocation("Tutor screen explain the blackboard controls")
                .expect("explicit screen command");
        assert_eq!(
            screen_with_blackboard_topic.canvas_mode,
            TutorCanvasMode::ScreenOverlay
        );

        let blackboard_with_screenshot_topic =
            parse_voice_guided_flow_invocation("Tutor blackboard explain how screenshots work")
                .expect("explicit blackboard command");
        assert_eq!(
            blackboard_with_screenshot_topic.canvas_mode,
            TutorCanvasMode::Blackboard
        );
    }

    #[test]
    fn tutor_no_tool_guard_forces_start_when_no_run_exists() {
        let continuation = tutor_tool_loop_continuation_for_no_tool_response(
            "hey tutor explain recursion stack and heap reference",
            None,
        )
        .expect("explicit tutor invoke should force runtime continuation");

        assert_eq!(continuation.run_id, None);
        assert_eq!(continuation.mode, TutorRunMode::ConceptExplainer);
        assert!(continuation.instruction.contains("start_tutor_run"));
        assert!(continuation.instruction.contains("screen-draw"));
        assert!(continuation.instruction.contains("do not answer directly"));
    }

    #[test]
    fn copilot_no_tool_guard_forces_start_when_no_run_exists() {
        let continuation = tutor_tool_loop_continuation_for_no_tool_response(
            "hey copilot show me how to create a note",
            None,
        )
        .expect("explicit copilot invoke should force runtime continuation");

        assert_eq!(continuation.run_id, None);
        assert_eq!(continuation.mode, TutorRunMode::GuidedAction);
        assert!(continuation.instruction.contains("screen_overlay"));
        assert!(continuation
            .instruction
            .contains("check_for_copilot_user_action"));
        assert!(continuation.instruction.contains("do not answer directly"));
    }

    #[test]
    fn tutor_no_tool_guard_ignores_non_invokes() {
        assert!(
            tutor_tool_loop_continuation_for_no_tool_response("tutor me on recursion", None,)
                .is_none()
        );
    }

    #[test]
    fn typed_active_lane_continues_without_repeating_marker_text() {
        let run = TutorRun::new_incremental(
            "typed-lane-run",
            TutorRunMode::ConceptExplainer,
            "explain recursion",
        )
        .expect("run");

        let continuation = tutor_tool_loop_continuation_for_active_lane(
            Some(&run),
            TutorRunMode::ConceptExplainer,
        )
        .expect("typed active lane should continue");
        assert_eq!(continuation.run_id.as_deref(), Some("typed-lane-run"));
        assert!(continuation
            .instruction
            .contains("Normal Tutor Progressive Lesson Coverage Required"));
    }

    #[test]
    fn normal_tutor_no_tool_guard_rejects_finalization_after_one_deep_step() {
        let mut run = TutorRun::new_incremental(
            "run-test",
            TutorRunMode::ConceptExplainer,
            "@tutor explain the logic behind recursion",
        )
        .expect("run");
        run.set_or_expand_lesson_milestones(vec![
            core_milestone("stack-shape", "Show recursive stack growth"),
            core_milestone("base-case", "Explain how the base case stops recursion"),
            core_milestone("unwind", "Explain return-value unwinding"),
        ])
        .expect("adaptive recursion lesson plan");
        run.step_history.push(TutorStepRecord {
            step: step(TutorStepKind::Draw, "draw stack frames"),
            status: TutorStepStatus::Succeeded,
            storyboard_steps: vec![storyboard_step("stack-shape", "Stack shape")],
        });

        let continuation = tutor_tool_loop_continuation_for_no_tool_response(
            "@tutor explain the logic behind recursion",
            Some(&run),
        )
        .expect("one step must not finish a deep normal Tutor lesson");
        assert!(continuation.reason.contains("1 of 3"));
        assert!(continuation.instruction.contains("substantive"));
    }

    #[test]
    fn tutor_quick_uses_the_same_condensed_contract_on_overlay_and_blackboard() {
        for canvas_mode in [TutorCanvasMode::ScreenOverlay, TutorCanvasMode::Blackboard] {
            let mut run = TutorRun::new_incremental_with_canvas(
                format!("run-quick-{}", canvas_mode.as_str()),
                TutorRunMode::ConceptExplainer,
                "@tutor #quick explain the logic behind recursion",
                canvas_mode,
            )
            .expect("run");
            assert_eq!(
                run.lesson_contract.as_ref().map(|contract| (
                    contract.depth,
                    contract.pacing,
                    contract.minimum_storyboard_steps,
                    contract.maximum_storyboard_steps,
                )),
                Some((
                    TutorLessonDepth::Deep,
                    TutorLessonPacing::Condensed,
                    2,
                    Some(6),
                )),
            );
            assert!(run
                .set_or_expand_lesson_milestones(vec![
                    lesson_milestone("one", "One"),
                    lesson_milestone("two", "Two"),
                    lesson_milestone("three", "Three"),
                    lesson_milestone("four", "Four"),
                    lesson_milestone("five", "Five"),
                    lesson_milestone("six", "Six"),
                    lesson_milestone("seven", "Seven"),
                ])
                .expect_err("Quick must reject a plan beyond its condensed maximum")
                .contains("at most 6"));
            run.set_or_expand_lesson_milestones(vec![
                core_milestone("stack-shape", "Show recursive stack growth"),
                core_milestone(
                    "stop-and-unwind",
                    "Connect the base case to return-value unwinding",
                ),
            ])
            .expect("condensed recursion lesson plan");
            run.step_history.push(TutorStepRecord {
                step: step(TutorStepKind::Draw, "draw stack frames"),
                status: TutorStepStatus::Succeeded,
                storyboard_steps: vec![storyboard_step("stack-shape", "Stack shape")],
            });

            let progress = run.lesson_progress().expect("Quick lesson progress");
            assert!(progress.plan_ready);
            assert_eq!(progress.completed_storyboard_steps, 1);
            assert_eq!(progress.remaining_storyboard_steps, 1);
            let continuation = tutor_tool_loop_continuation_for_no_tool_response(
                "@tutor #quick explain the logic behind recursion",
                Some(&run),
            )
            .expect("deep Quick cannot finish after its first milestone");
            assert!(continuation.reason.contains("1 of 2"));
            assert!(continuation.instruction.contains("condensed"));
            assert!(run.clone().complete().is_err());

            run.step_history.push(TutorStepRecord {
                step: step(TutorStepKind::Draw, "connect stop and unwind"),
                status: TutorStepStatus::Succeeded,
                storyboard_steps: vec![storyboard_step(
                    "stop-and-unwind",
                    "The base case stops growth; returns then unwind the stack.",
                )],
            });
            assert_eq!(
                run.lesson_progress()
                    .expect("completed Quick progress")
                    .remaining_storyboard_steps,
                0,
            );
            assert!(tutor_tool_loop_continuation_for_no_tool_response(
                "@tutor #quick explain the logic behind recursion",
                Some(&run),
            )
            .is_none());
            run.complete().expect("completed condensed Quick lesson");
        }
    }

    #[test]
    fn normal_tutor_lesson_contract_distinguishes_focused_standard_and_deep() {
        let focused = TutorRun::new_incremental(
            "focused",
            TutorRunMode::ExplainOnly,
            "@tutor highlight the product name",
        )
        .expect("focused run");
        assert_eq!(
            focused.lesson_contract,
            Some(TutorLessonContract {
                depth: TutorLessonDepth::Focused,
                pacing: TutorLessonPacing::Progressive,
                minimum_storyboard_steps: 1,
                maximum_storyboard_steps: None,
                milestones: Vec::new(),
            })
        );

        let standard = TutorRun::new_incremental(
            "standard",
            TutorRunMode::ExplainOnly,
            "@tutor explain what this toolbar does",
        )
        .expect("standard run");
        assert_eq!(
            standard.lesson_contract,
            Some(TutorLessonContract {
                depth: TutorLessonDepth::Standard,
                pacing: TutorLessonPacing::Progressive,
                minimum_storyboard_steps: 2,
                maximum_storyboard_steps: None,
                milestones: Vec::new(),
            })
        );

        let deep = TutorRun::new_incremental(
            "deep",
            TutorRunMode::ConceptExplainer,
            "@tutor explain the logic behind the Pythagorean theorem",
        )
        .expect("deep run");
        assert_eq!(
            deep.lesson_contract,
            Some(TutorLessonContract {
                depth: TutorLessonDepth::Deep,
                pacing: TutorLessonPacing::Progressive,
                minimum_storyboard_steps: 3,
                maximum_storyboard_steps: None,
                milestones: Vec::new(),
            })
        );

        let app_copilot = TutorRun::new_incremental(
            "app-copilot",
            TutorRunMode::ConceptExplainer,
            "@copilot explain this screen",
        )
        .expect("App Copilot run");
        assert!(
            app_copilot.lesson_contract.is_none(),
            "App Copilot must retain its independent completion policy even if a model supplies a concept mode"
        );
    }

    #[test]
    fn tutor_quick_contract_uses_small_depth_aware_ranges_without_affecting_app_copilot() {
        let cases = [
            (
                "focused-quick",
                TutorRunMode::ExplainOnly,
                "@tutor #quick highlight the submit button",
                TutorLessonDepth::Focused,
                1,
                1,
            ),
            (
                "standard-quick",
                TutorRunMode::ExplainOnly,
                "@tutor #quick explain what this toolbar does",
                TutorLessonDepth::Standard,
                1,
                2,
            ),
            (
                // Six, not four. A deep Quick lesson still has to fit the step
                // the learner is actually stuck on — before, the cut, mid, after
                // and the quantity preserved — which is five before any setup or
                // conclusion. Four made Quick too short to teach the hard part,
                // which is the one part condensing should never remove.
                "deep-quick",
                TutorRunMode::ConceptExplainer,
                "@tutor #quick explain why recursion works",
                TutorLessonDepth::Deep,
                2,
                6,
            ),
        ];
        for (run_id, mode, goal, depth, minimum, maximum) in cases {
            let run = TutorRun::new_incremental(run_id, mode, goal).expect("Quick run");
            let contract = run.lesson_contract.expect("Quick lesson contract");
            assert_eq!(contract.depth, depth);
            assert_eq!(contract.pacing, TutorLessonPacing::Condensed);
            assert_eq!(contract.minimum_storyboard_steps, minimum);
            assert_eq!(contract.maximum_storyboard_steps, Some(maximum));
        }

        let app_copilot = TutorRun::new_incremental(
            "app-copilot-quick",
            TutorRunMode::ConceptExplainer,
            "@copilot #quick explain this screen",
        )
        .expect("App Copilot Quick run");
        assert!(app_copilot.lesson_contract.is_none());
    }

    #[test]
    fn legacy_lesson_contract_payloads_default_to_progressive_unbounded_pacing() {
        let contract: TutorLessonContract = serde_json::from_value(serde_json::json!({
            "depth": "standard",
            "minimum_storyboard_steps": 2,
            "milestones": []
        }))
        .expect("legacy lesson contract remains readable");
        assert_eq!(contract.pacing, TutorLessonPacing::Progressive);
        assert_eq!(contract.maximum_storyboard_steps, None);
    }

    #[test]
    fn normal_tutor_requires_adaptive_plan_even_after_minimum_floor_is_drawn() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "missing-plan",
            TutorRunMode::ExplainOnly,
            "@tutor explain this toolbar",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "draw two plausible parts"),
            vec![
                storyboard_step("toolbar-layout", "Toolbar layout"),
                storyboard_step("toolbar-actions", "Toolbar actions"),
            ],
        )
        .expect("draw before plan is available");

        let progress = run.lesson_progress().expect("lesson progress");
        assert!(!progress.plan_ready);
        assert_eq!(progress.minimum_storyboard_steps, 2);
        assert_eq!(progress.remaining_storyboard_steps, 0);
        assert!(!run.lesson_contract_is_satisfied());
        assert!(run
            .complete()
            .expect_err("a fixed count cannot replace an adaptive plan")
            .contains("adaptive milestone plan"));
    }

    #[test]
    fn adaptive_lesson_can_exceed_depth_floor_on_overlay_and_blackboard() {
        for canvas_mode in [TutorCanvasMode::ScreenOverlay, TutorCanvasMode::Blackboard] {
            let mut run = TutorRun::new_incremental_with_canvas(
                format!("adaptive-{canvas_mode:?}"),
                TutorRunMode::ExplainOnly,
                "@tutor explain how this system works",
                canvas_mode,
            )
            .expect("run");
            if !canvas_mode.is_blackboard() {
                run.propose_step(step(TutorStepKind::Observe, "observe source"))
                    .expect("observe");
                run.propose_step(step(TutorStepKind::ResolveTarget, "resolve source"))
                    .expect("resolve");
            }
            run.set_or_expand_lesson_milestones(vec![
                lesson_milestone("context", "Establish the relevant context"),
                lesson_milestone("parts", "Identify the important parts"),
                lesson_milestone("flow", "Trace how information moves"),
                lesson_milestone("tradeoffs", "Explain the important tradeoffs"),
                lesson_milestone("conclusion", "Connect the parts to the conclusion"),
            ])
            .expect("five-milestone adaptive plan");
            run.propose_step_with_storyboard_steps(
                step(TutorStepKind::Draw, "teach first two milestones"),
                vec![
                    storyboard_step("context", "Relevant context"),
                    storyboard_step("parts", "Important parts"),
                ],
            )
            .expect("partial draw");

            let progress = run.lesson_progress().expect("lesson progress");
            assert!(progress.plan_ready);
            assert_eq!(progress.minimum_storyboard_steps, 2);
            assert_eq!(progress.planned_milestones, 5);
            assert_eq!(progress.completed_storyboard_steps, 2);
            assert_eq!(progress.remaining_storyboard_steps, 3);
            assert_eq!(
                progress
                    .remaining_milestones
                    .iter()
                    .map(|milestone| milestone.milestone_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["flow", "tradeoffs", "conclusion"]
            );
            assert!(run
                .complete()
                .expect_err("two-step floor is not the adaptive lesson length")
                .contains("2 of 5"));

            run.propose_step_with_storyboard_steps(
                step(TutorStepKind::Draw, "teach remaining milestones"),
                vec![
                    storyboard_step("flow", "Information flow"),
                    storyboard_step("tradeoffs", "Important tradeoffs"),
                    storyboard_step("conclusion", "Connected conclusion"),
                ],
            )
            .expect("remaining draw");
            run.complete().expect("adaptive lesson completes");
        }
    }

    #[test]
    fn adaptive_lesson_plan_is_semantic_unique_and_append_only() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "append-only",
            TutorRunMode::ConceptExplainer,
            "@tutor explain recursion",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        let initial = vec![
            lesson_milestone("call", "Show the recursive call"),
            lesson_milestone("base", "Explain the base case"),
            lesson_milestone("return", "Trace the returning value"),
        ];
        run.set_or_expand_lesson_milestones(initial.clone())
            .expect("initial plan");

        let mut expanded = initial.clone();
        expanded.push(lesson_milestone(
            "complexity",
            "Relate stack depth to complexity",
        ));
        run.set_or_expand_lesson_milestones(expanded)
            .expect("append new prerequisite");
        assert_eq!(
            run.lesson_contract
                .as_ref()
                .expect("contract")
                .milestones
                .len(),
            4
        );
        assert!(run
            .set_or_expand_lesson_milestones(initial)
            .expect_err("plan scope cannot shrink")
            .contains("may only append"));

        let mut duplicate = run
            .lesson_contract
            .as_ref()
            .expect("contract")
            .milestones
            .clone();
        duplicate.push(lesson_milestone(
            "another-return",
            "Trace the returning value",
        ));
        assert!(run
            .set_or_expand_lesson_milestones(duplicate)
            .expect_err("duplicate semantics cannot inflate the plan")
            .contains("duplicates another teaching objective"));
    }

    #[test]
    fn planned_normal_tutor_rejects_unplanned_storyboard_ids() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "unplanned-step",
            TutorRunMode::ExplainOnly,
            "@tutor explain this toolbar",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        run.set_or_expand_lesson_milestones(vec![
            lesson_milestone("layout", "Explain the toolbar layout"),
            lesson_milestone("actions", "Explain the available actions"),
        ])
        .expect("plan");

        assert!(run
            .propose_step_with_storyboard_steps(
                step(TutorStepKind::Draw, "draw unrelated aside"),
                vec![storyboard_step("aside", "Unplanned aside")],
            )
            .expect_err("unplanned draw must expand the plan first")
            .contains("not present in the adaptive lesson plan"));
    }

    #[test]
    fn lesson_plan_and_renderer_backed_draw_commit_atomically() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("owner", "workspace", "adaptive-atomic"),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::Blackboard,
                "@tutor explain this toolbar",
                false,
                None,
            )
            .expect("start run");
        let plan = vec![
            lesson_milestone("layout", "Explain the toolbar layout"),
            lesson_milestone("actions", "Explain the toolbar actions"),
        ];

        assert!(store
            .propose_step_with_lesson_milestones(
                &run.run_id,
                step(TutorStepKind::Draw, "draw unplanned aside"),
                vec![storyboard_step("aside", "Unplanned aside")],
                Some(plan),
            )
            .expect_err("invalid draw must not partially commit its plan")
            .contains("not present in the adaptive lesson plan"));
        let stored = store
            .get_run(&run.run_id)
            .expect("read run")
            .expect("stored run");
        assert!(stored.step_history.is_empty());
        assert!(stored
            .lesson_contract
            .expect("normal lesson contract")
            .milestones
            .is_empty());
    }

    #[test]
    fn normal_tutor_completion_counts_distinct_draw_storyboards_not_say_or_duplicates() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "pythagoras",
            TutorRunMode::ConceptExplainer,
            "@tutor explain the logic behind the Pythagorean theorem",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "identify triangle"),
            vec![storyboard_step("triangle", "The right triangle")],
        )
        .expect("first draw");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "repeat triangle"),
            vec![storyboard_step("triangle", "The same triangle")],
        )
        .expect("duplicate draw");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "rename triangle"),
            vec![TutorDrawStoryboardStep {
                step_id: "TRIANGLE-RENAMED".to_string(),
                label: "Renamed triangle".to_string(),
                narration: "Preview The right triangle.".to_string(),
                figure_backed: true,
            }],
        )
        .expect("renamed duplicate narration");
        run.propose_step(step(TutorStepKind::Say, "explain areas in text"))
            .expect("say");
        run.set_or_expand_lesson_milestones(vec![
            core_milestone("triangle", "Identify the right triangle and its sides"),
            core_milestone("square-areas", "Relate each side to a square area"),
            core_milestone(
                "area-rearrangement",
                "Show why the two smaller areas equal the largest area",
            ),
        ])
        .expect("adaptive lesson plan");

        let progress = run.lesson_progress().expect("lesson progress");
        assert_eq!(progress.completed_storyboard_steps, 1);
        assert_eq!(progress.remaining_storyboard_steps, 2);
        let error = run
            .complete()
            .expect_err("incomplete lesson must not finish");
        assert!(error.contains("1 of 3"));

        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "show square areas"),
            vec![
                storyboard_step("square-areas", "Squares represent areas"),
                storyboard_step("area-rearrangement", "Rearrange equal areas"),
            ],
        )
        .expect("remaining draw");
        assert!(run.lesson_contract_is_satisfied());
        run.complete().expect("complete covered lesson");
    }

    #[test]
    fn unresolved_recorded_failure_allows_terminal_failure_text_but_recovery_resumes_coverage() {
        let mut run = TutorRun::new_incremental(
            "blocked",
            TutorRunMode::ConceptExplainer,
            "@tutor explain the logic behind recursion",
        )
        .expect("run");
        let mut failed_draw = step(TutorStepKind::Draw, "screen target is unavailable");
        failed_draw.expected_state = Some("No renderable screen target was available".to_string());
        run.record_step_failure(failed_draw);

        assert_eq!(
            run.terminal_blocker_reason().as_deref(),
            Some("No renderable screen target was available")
        );
        assert!(run
            .complete()
            .expect_err("an unresolved failure cannot be marked completed")
            .contains("unresolved recorded failure"));
        assert!(
            tutor_tool_loop_continuation_for_active_lane(
                Some(&run),
                TutorRunMode::ConceptExplainer,
            )
            .is_none(),
            "a recorded terminal blocker must be reportable instead of forcing repeated visual coverage attempts"
        );

        run.propose_step(step(TutorStepKind::Observe, "screen became available"))
            .expect("successful recovery observation");
        assert!(run.terminal_blocker_reason().is_none());
        assert!(
            tutor_tool_loop_continuation_for_active_lane(
                Some(&run),
                TutorRunMode::ConceptExplainer,
            )
            .is_some(),
            "successful recovery must restore the normal lesson coverage guard"
        );
    }

    #[test]
    fn normal_tutor_lesson_coverage_preserves_non_ascii_narration() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "multilingual",
            TutorRunMode::ExplainOnly,
            "@tutor explain this toolbar",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        run.set_or_expand_lesson_milestones(vec![
            lesson_milestone("पहला-चरण", "मुख्य बटन को पहचानना"),
            lesson_milestone("दूसरा-चरण", "बटन के परिणाम को समझना"),
        ])
        .expect("adaptive multilingual lesson plan");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "draw Hindi storyboard"),
            vec![
                TutorDrawStoryboardStep {
                    step_id: "पहला-चरण".to_string(),
                    label: "पहला चरण".to_string(),
                    narration: "पहले मुख्य बटन को पहचानें।".to_string(),
                    figure_backed: true,
                },
                TutorDrawStoryboardStep {
                    step_id: "दूसरा-चरण".to_string(),
                    label: "दूसरा चरण".to_string(),
                    narration: "फिर उसके परिणाम को देखें।".to_string(),
                    figure_backed: true,
                },
            ],
        )
        .expect("multilingual draw");

        assert_eq!(
            run.lesson_progress()
                .expect("normal lesson progress")
                .completed_storyboard_steps,
            2
        );
        run.complete().expect("multilingual lesson completes");
    }

    #[test]
    fn normal_tutor_cannot_complete_after_clearing_all_visual_evidence() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "cleared",
            TutorRunMode::ExplainOnly,
            "@tutor explain this toolbar",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        run.set_or_expand_lesson_milestones(vec![
            lesson_milestone("toolbar-layout", "Explain the toolbar layout"),
            lesson_milestone("toolbar-actions", "Explain the toolbar actions"),
        ])
        .expect("adaptive toolbar lesson plan");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "draw toolbar lesson"),
            vec![
                storyboard_step("toolbar-layout", "Toolbar layout"),
                storyboard_step("toolbar-actions", "Toolbar actions"),
            ],
        )
        .expect("covered draw");
        run.propose_step(step(TutorStepKind::ClearDrawings, "clear overlay"))
            .expect("clear");

        assert!(run.lesson_contract_is_satisfied());
        assert!(!run.has_visible_draw_storyboard_evidence());
        assert!(run
            .complete()
            .expect_err("cleared lesson cannot claim visible completion")
            .contains("after the latest overlay clear"));

        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "restore final recap"),
            vec![storyboard_step("toolbar-layout", "Visible recap")],
        )
        .expect("restore visible evidence");
        run.complete().expect("visible covered lesson completes");
    }

    #[test]
    fn tutor_turn_classifier_is_concept_only() {
        assert_eq!(
            classify_tutor_turn_mode("@tutor show me how to open the requested item"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor walk me through filling the target field"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor explain how to create a new item"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode(
                "@tutor draw and explain each step, then actually open the requested item"
            ),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("@tutor show me where the primary action button is"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor highlight the open button"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("@tutor what does the create button do"),
            TutorRunMode::ExplainOnly
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor only highlight the target field, do not click"),
            TutorRunMode::ExplainOnly
        );
    }

    #[test]
    fn app_copilot_turn_classifier_defaults_to_guided_action() {
        assert_eq!(
            classify_tutor_or_app_copilot_turn_mode(
                "@copilot show me how to open the requested item"
            ),
            TutorRunMode::GuidedAction
        );
        assert_eq!(
            classify_tutor_or_app_copilot_turn_mode(
                "hey copilot walk me through filling the target field"
            ),
            TutorRunMode::GuidedAction
        );
        assert_eq!(
            classify_tutor_or_app_copilot_turn_mode(
                "@copilot show me how to open the requested item"
            ),
            TutorRunMode::GuidedAction
        );
        assert_eq!(
            classify_tutor_or_app_copilot_turn_mode(
                "@copilot demonstrate a temporary note and clean it up"
            ),
            TutorRunMode::DemoAndCleanup
        );
        assert_eq!(
            classify_tutor_or_app_copilot_turn_mode(
                "@copilot show me where the primary action button is"
            ),
            TutorRunMode::GuidedAction
        );
    }

    #[test]
    fn tutor_turn_classifier_routes_visible_concepts_to_concept_modes() {
        assert_eq!(
            classify_tutor_turn_mode("@tutor explain this line graph"),
            TutorRunMode::ConceptExplainer
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor solve this physics question step by step"),
            TutorRunMode::GuidedSolution
        );
        assert_eq!(
            classify_tutor_turn_mode("@tutor demonstrate how slope changes on this graph"),
            TutorRunMode::ConceptDemo
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor show me what this recursion code is doing"),
            TutorRunMode::ConceptExplainer
        );
        assert_eq!(
            classify_tutor_turn_mode("hey tutor show me how this algorithm works"),
            TutorRunMode::ConceptExplainer
        );
        assert_eq!(
            classify_tutor_turn_mode("@tutor show me how the graph changes with slope"),
            TutorRunMode::ConceptExplainer
        );
    }

    #[test]
    fn copilot_turn_guidance_for_action_warns_not_to_stop_after_draw() {
        let guidance =
            tutor_or_copilot_turn_guidance("@copilot show me how to open the requested item");
        assert!(guidance.contains("guided_action"));
        assert!(guidance.contains("Do not stop after only drawing"));
        assert!(guidance.contains("check_for_copilot_user_action"));
    }

    #[test]
    fn copilot_turn_guidance_for_bare_prompt_documents_hybrid_default() {
        let guidance =
            tutor_or_copilot_turn_guidance("@copilot show me how to open the requested item");
        assert!(guidance.contains("hybrid app mutation"));
        assert!(guidance.contains("check_for_copilot_user_action"));
        assert!(guidance.contains("returns `user_acted`"));
    }

    #[test]
    fn tutor_turn_guidance_for_concepts_forbids_app_mutation() {
        let guidance = tutor_or_copilot_turn_guidance("@tutor solve this geometry problem");
        assert!(guidance.contains("guided_solution"));
        assert!(guidance.contains("do not click/type/mutate"));
    }

    #[test]
    fn validates_observe_act_verify_loop() {
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let plan = TutorSessionPlan {
            goal: "show how to create an item".to_string(),
            steps: vec![
                step(TutorStepKind::Observe, "observe current app"),
                step(TutorStepKind::ResolveTarget, "find primary action button"),
                step(TutorStepKind::Draw, "highlight primary action button"),
                click,
                step(TutorStepKind::Observe, "observe destination view"),
                step(TutorStepKind::Verify, "verify destination view opened"),
            ],
        };

        assert_eq!(validate_tutor_plan(&plan), Ok(()));
    }

    #[test]
    fn rejects_action_without_fresh_observation() {
        let mut click = step(TutorStepKind::Click, "click guessed button");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let plan = TutorSessionPlan {
            goal: "click something".to_string(),
            steps: vec![click],
        };

        let error = validate_tutor_plan(&plan).unwrap_err();
        assert!(error.contains("requires a fresh observation"));
    }

    #[test]
    fn rejects_second_action_before_verification() {
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let mut type_text = step(TutorStepKind::TypeText, "type requested text");
        type_text.safety = TutorSafetyLevel::ReversibleAction;
        let plan = TutorSessionPlan {
            goal: "create and fill item".to_string(),
            steps: vec![
                step(TutorStepKind::Observe, "observe current app"),
                click,
                step(TutorStepKind::Observe, "observe destination view"),
                type_text,
            ],
        };

        let error = validate_tutor_plan(&plan).unwrap_err();
        assert!(error.contains("before verifying"));
    }

    #[test]
    fn requires_confirmation_for_non_owned_destructive_action() {
        let mut delete = step(TutorStepKind::Click, "delete existing item");
        delete.safety = TutorSafetyLevel::DestructiveRequiresConfirmation;
        let plan = TutorSessionPlan {
            goal: "delete item".to_string(),
            steps: vec![step(TutorStepKind::Observe, "observe current app"), delete],
        };

        let error = validate_tutor_plan(&plan).unwrap_err();
        assert!(error.contains("needs confirmation"));
    }

    #[test]
    fn allows_session_owned_destructive_cleanup_with_verification() {
        let mut delete = step(
            TutorStepKind::Click,
            "delete item created in this tutor session",
        );
        delete.safety = TutorSafetyLevel::SessionOwnedDestructive;
        let plan = TutorSessionPlan {
            goal: "clean up demo item".to_string(),
            steps: vec![
                step(TutorStepKind::Observe, "observe created item"),
                delete,
                step(TutorStepKind::Observe, "observe current app list"),
                step(TutorStepKind::Verify, "verify created item is gone"),
            ],
        };

        assert_eq!(validate_tutor_plan(&plan), Ok(()));
    }

    #[test]
    fn validates_visual_entity_refs_in_static_plan() {
        let mut observe = step(TutorStepKind::Observe, "observe diagram");
        observe.visual_entity_map = Some(sample_visual_entity_map());
        let mut resolve = step(TutorStepKind::ResolveTarget, "resolve graph line");
        resolve.source_entity_ids = vec!["line-main".to_string()];
        let plan = TutorSessionPlan {
            goal: "explain diagram".to_string(),
            steps: vec![observe, resolve],
        };

        assert_eq!(validate_tutor_plan(&plan), Ok(()));
    }

    #[test]
    fn rolling_run_allows_explain_only_observe_resolve_draw_say_loop() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ExplainOnly,
            "show what the toolbar buttons do",
        )
        .expect("run");

        run.propose_step(step(TutorStepKind::Observe, "observe toolbar"))
            .expect("observe");
        run.propose_step(step(
            TutorStepKind::ResolveTarget,
            "resolve toolbar buttons",
        ))
        .expect("resolve");
        run.set_or_expand_lesson_milestones(vec![
            lesson_milestone("toolbar-layout", "Explain the toolbar layout"),
            lesson_milestone("toolbar-actions", "Explain the toolbar actions"),
        ])
        .expect("adaptive toolbar lesson plan");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "label toolbar buttons"),
            vec![
                storyboard_step("toolbar-layout", "Toolbar layout"),
                storyboard_step("toolbar-actions", "Toolbar actions"),
            ],
        )
        .expect("draw");
        run.propose_step(step(TutorStepKind::Say, "explain toolbar buttons"))
            .expect("say");
        run.complete().expect("complete");

        assert_eq!(run.status, TutorRunStatus::Completed);
        assert_eq!(run.step_history.len(), 4);
        assert!(!run
            .step_history
            .iter()
            .any(|record| record.step.kind.changes_ui_state()));
    }

    #[test]
    fn rolling_run_allows_visual_verify_without_pending_action_in_concept_mode() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptExplainer,
            "explain the visible concept",
        )
        .expect("run");

        run.propose_step(step(TutorStepKind::Observe, "observe visible concept"))
            .expect("observe");
        run.propose_step(step(
            TutorStepKind::ResolveTarget,
            "resolve visible concept",
        ))
        .expect("resolve");
        run.propose_step(step(TutorStepKind::Draw, "draw concept overlay"))
            .expect("draw");
        run.propose_step(step(
            TutorStepKind::Verify,
            "verify overlay matches the visible concept",
        ))
        .expect("visual verify");

        assert!(run.pending_action.is_none());
    }

    #[test]
    fn rolling_run_rejects_guided_action_verify_without_pending_action() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");

        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        let error = run
            .propose_step(step(
                TutorStepKind::Verify,
                "verify without delegated action",
            ))
            .expect_err("guided-action verify still needs a pending action");

        assert!(error.contains("pending UI-changing action"));
    }

    #[test]
    fn rolling_run_records_visual_entity_map_and_allows_entity_grounded_steps() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptExplainer,
            "explain the line graph",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe line graph");
        observe.visual_entity_map = Some(sample_visual_entity_map());

        run.propose_step(observe).expect("observe with entity map");

        assert!(run.latest_visual_entity_map.is_some());
        let context = run
            .visual_entity_prompt_context()
            .expect("visual entity context");
        assert!(context.contains("line-main"));
        assert!(context.contains("equation-main"));

        let mut resolve = step(TutorStepKind::ResolveTarget, "resolve graph line");
        resolve.source_entity_ids = vec!["line-main".to_string(), "equation-main".to_string()];
        run.propose_step(resolve).expect("resolve entities");

        let mut draw = step(TutorStepKind::Draw, "highlight plotted line");
        draw.source_entity_ids = vec!["line-main".to_string()];
        run.propose_step(draw).expect("draw entity-linked overlay");
    }

    #[test]
    fn rolling_run_rejects_unknown_visual_entity_ids() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptExplainer,
            "explain the diagram",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe diagram");
        observe.visual_entity_map = Some(sample_visual_entity_map());
        run.propose_step(observe).expect("observe");

        let mut resolve = step(TutorStepKind::ResolveTarget, "resolve missing entity");
        resolve.source_entity_ids = vec!["missing-side".to_string()];

        let error = run
            .propose_step(resolve)
            .expect_err("unknown entity id should fail");
        assert!(error.contains("unknown source_entity_ids"));
    }

    #[test]
    fn ui_mutation_invalidates_visual_entity_map() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the target after pointing",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe target");
        observe.visual_entity_map = Some(sample_visual_entity_map());
        run.propose_step(observe).expect("observe");

        let mut resolve = step(TutorStepKind::ResolveTarget, "resolve target");
        resolve.source_entity_ids = vec!["line-main".to_string()];
        run.propose_step(resolve).expect("resolve");

        let mut click = step(TutorStepKind::Click, "click target");
        click.safety = TutorSafetyLevel::ReversibleAction;
        click.source_entity_ids = vec!["line-main".to_string()];
        run.propose_step(click).expect("click");

        assert!(!run.has_fresh_observation);
        assert!(run.latest_visual_entity_map.is_none());

        let mut draw = step(TutorStepKind::Draw, "draw stale target");
        draw.source_entity_ids = vec!["line-main".to_string()];
        let error = run
            .propose_step(draw)
            .expect_err("stale entity id should fail");
        assert!(error.contains("source_entity_ids require a fresh observation"));
    }

    #[test]
    fn math_v0_validates_coordinate_slope_overlay_without_concept_specific_builder() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptExplainer,
            "explain slope on this graph",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe graph and equation");
        observe.visual_entity_map = Some(sample_math_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "slope-teaching-step",
            "source_entity_ids": ["axis-x", "axis-y", "line-main"],
            "shapes": [
                {"type": "axis", "x1": 100, "y1": 500, "x2": 700, "y2": 500, "source_entity_ids": ["axis-x"]},
                {"type": "axis", "x1": 100, "y1": 500, "x2": 100, "y2": 120, "source_entity_ids": ["axis-y"]},
                {"type": "line", "x1": 160, "y1": 440, "x2": 620, "y2": 180, "source_entity_ids": ["line-main"]},
                {"type": "formula", "x": 430, "y": 145, "text": "slope = rise / run"}
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(ids, vec!["axis-x", "axis-y", "line-main"]);
    }

    #[test]
    fn draw_payload_accepts_generic_curved_strokes() {
        let payload = serde_json::json!({
            "type": "group",
            "shapes": [
                {
                    "type": "path",
                    "d": "M 120 420 C 170 330 240 500 300 400 S 430 360 480 430",
                    "label": "cursive stroke"
                },
                {
                    "type": "curve",
                    "from_x": 140,
                    "from_y": 520,
                    "control_x": 220,
                    "control_y": 440,
                    "to_x": 320,
                    "to_y": 520
                },
                {
                    "type": "freehand",
                    "points": [[360, 510], [390, 465], [430, 520], [470, 465], [510, 520]]
                }
            ]
        });

        validate_tutor_draw_payload_shape(&payload).expect("curved strokes are valid");
    }

    #[test]
    fn draw_payload_rejects_unsafe_svg_path_data() {
        let payload = serde_json::json!({
            "type": "path",
            "d": "M 10 10 <script>alert(1)</script>"
        });

        let error = validate_tutor_draw_payload_shape(&payload).unwrap_err();
        assert!(error.contains("unsupported characters"));
    }

    #[test]
    fn draw_payload_accepts_chart_series_inside_axis_frame() {
        let payload = serde_json::json!({
            "type": "group",
            "id": "trend-chart",
            "shapes": [
                {"type": "axis", "x1": 100, "y1": 500, "x2": 700, "y2": 500},
                {"type": "axis", "x1": 100, "y1": 500, "x2": 100, "y2": 100},
                {"type": "path", "d": "M 140 460 C 260 280 420 360 660 160", "color": "pink"},
                {"type": "label", "x": 720, "y": 130, "text": "peak"}
            ]
        });

        validate_tutor_draw_payload_shape(&payload).expect("chart series stays inside axes");
    }

    #[test]
    fn draw_payload_rejects_chart_series_outside_axis_frame() {
        let payload = serde_json::json!({
            "type": "group",
            "id": "bad-trend-chart",
            "shapes": [
                {"type": "axis", "x1": 100, "y1": 500, "x2": 700, "y2": 500},
                {"type": "axis", "x1": 100, "y1": 500, "x2": 100, "y2": 100},
                {"type": "freehand", "points": [[160, 450], [400, 300], [760, 220], [920, 180]], "color": "pink"}
            ]
        });

        let error = validate_tutor_draw_payload_shape(&payload)
            .expect_err("series escaping axes should fail");
        assert!(error.contains("chart frame layout invalid"));
    }

    #[test]
    fn draw_payload_allows_curved_annotations_without_chart_axes() {
        let payload = serde_json::json!({
            "type": "group",
            "id": "curved-callout",
            "shapes": [
                {"type": "path", "d": "M 120 180 C 260 20 420 360 700 120", "color": "cyan"},
                {"type": "callout", "x": 720, "y": 120, "text": "smooth motion"}
            ]
        });

        validate_tutor_draw_payload_shape(&payload).expect("non-chart curved callout is valid");
    }

    #[test]
    fn draw_payload_accepts_text_backed_handwriting() {
        let payload = serde_json::json!({
            "type": "cursive_text",
            "x": 180,
            "y": 430,
            "text": "we",
            "font_size": 96,
            "color": "cyan"
        });

        validate_tutor_draw_payload_shape(&payload).expect("cursive text primitive is valid");
    }

    #[test]
    fn draw_payload_rejects_handwriting_without_text() {
        let payload = serde_json::json!({
            "type": "handwriting",
            "x": 180,
            "y": 430,
            "font_size": 96
        });

        let error = validate_tutor_draw_payload_shape(&payload)
            .expect_err("handwriting without text should fail");
        assert!(error.contains("text") || error.contains("label"));
    }

    #[test]
    fn math_v0_validates_algebra_annotation_overlay_without_concept_specific_builder() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedSolution,
            "solve the visible linear equation",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe equation");
        observe.visual_entity_map = Some(sample_math_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "linear-equation-step",
            "shapes": [
                {"type": "formula", "x": 180, "y": 220, "text": "2x + 3 = 11", "source_entity_ids": ["equation-main"]},
                {"type": "callout", "x": 180, "y": 260, "text": "subtract 3 from both sides", "source_entity_ids": ["equation-main"]},
                {"type": "formula", "x": 180, "y": 310, "text": "2x = 8"}
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(ids, vec!["equation-main"]);
    }

    #[test]
    fn math_v0_validates_area_fill_and_label_overlay_without_concept_specific_builder() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptDemo,
            "explain the shaded region on this graph",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe bounded graph region");
        observe.visual_entity_map = Some(sample_math_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "region-label-step",
            "source_entity_ids": ["region-main", "line-main"],
            "shapes": [
                {
                    "type": "area_fill",
                    "points": [[160, 440], [620, 180], [620, 500], [160, 500]],
                    "color": "orange",
                    "opacity": 0.24,
                    "source_entity_ids": ["region-main"]
                },
                {
                    "type": "label",
                    "x": 360,
                    "y": 360,
                    "text": "area under the line",
                    "source_entity_ids": ["region-main"]
                },
                {
                    "type": "side_label",
                    "x1": 160,
                    "y1": 440,
                    "x2": 620,
                    "y2": 180,
                    "text": "increasing line",
                    "source_entity_ids": ["line-main"]
                }
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(ids, vec!["region-main", "line-main"]);
    }

    #[test]
    fn draw_payload_validation_rejects_missing_required_geometry() {
        for payload in [
            serde_json::json!({"type": "line", "x1": 10, "y1": 10}),
            serde_json::json!({"type": "formula", "text": "x = 1"}),
            serde_json::json!({"type": "highlight", "x": 10, "y": 20, "w": 120}),
            serde_json::json!({"type": "circle", "cx": 40, "cy": 40}),
            serde_json::json!({"type": "polygon"}),
        ] {
            let error = validate_tutor_draw_payload_shape(&payload)
                .expect_err("missing geometry should be rejected");
            assert!(
                error.contains("requires"),
                "expected required-geometry error, got {error}"
            );
        }
    }

    #[test]
    fn draw_payload_validation_accepts_rect_like_area_fill() {
        let payload = serde_json::json!({
            "type": "area_fill",
            "x": 120,
            "y": 240,
            "w": 300,
            "h": 80,
            "label": "region"
        });

        validate_tutor_draw_payload_shape(&payload).expect("rect-like area fill should be valid");
    }

    #[test]
    fn phase6_validates_timed_reveal_overlay_metadata() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptDemo,
            "explain slope in three timed reveals",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe graph and equation");
        observe.visual_entity_map = Some(sample_math_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "slope-reveal-sequence",
            "ttl_ms": 16000,
            "shapes": [
                {
                    "type": "axis",
                    "x1": 100,
                    "y1": 500,
                    "x2": 700,
                    "y2": 500,
                    "source_entity_ids": ["axis-x"],
                    "reveal_id": "identify-axis",
                    "reveal_order": 1,
                    "tutor_step_label": "Identify the x-axis",
                    "narration": "First, anchor the horizontal change on the x-axis.",
                    "duration_ms": 900
                },
                {
                    "type": "line",
                    "x1": 160,
                    "y1": 440,
                    "x2": 620,
                    "y2": 180,
                    "source_entity_ids": ["line-main"],
                    "reveal_id": "show-line",
                    "reveal_order": 2,
                    "delay_ms": 900,
                    "tutor_step_label": "Show the changing line",
                    "narration": "Now connect the visual run to the line's rise.",
                    "wait_for_voice": true,
                    "duration_ms": 1100
                },
                {
                    "type": "formula",
                    "x": 430,
                    "y": 145,
                    "text": "slope = rise / run",
                    "reveal_id": "connect-formula",
                    "reveal_order": 3,
                    "delay_ms": 1800,
                    "clear_previous": true,
                    "persist_until_step": "summary",
                    "tutor_step_label": "Connect to the formula",
                    "narration": "Finally, the formula names that ratio."
                }
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(ids, vec!["axis-x", "line-main"]);
    }

    #[test]
    fn phase6_rejects_unbounded_or_malformed_reveal_metadata() {
        let payload = serde_json::json!({
            "type": "group",
            "id": "bad-reveal",
            "shapes": [
                {
                    "type": "formula",
                    "x": 100,
                    "y": 100,
                    "text": "x = 1",
                    "reveal_id": "late",
                    "delay_ms": 120000
                }
            ]
        });
        let error = validate_tutor_draw_payload_shape(&payload).expect_err("delay should fail");
        assert!(error.contains("delay_ms"));

        let payload = serde_json::json!({
            "type": "formula",
            "x": 100,
            "y": 100,
            "text": "x = 1",
            "wait_for_voice": "yes"
        });
        let error =
            validate_tutor_draw_payload_shape(&payload).expect_err("wait_for_voice should fail");
        assert!(error.contains("wait_for_voice"));
    }

    #[test]
    fn tutor_draw_storyboard_rejects_label_only_highlight() {
        let payload = serde_json::json!({
            "type": "highlight",
            "x": 100,
            "y": 100,
            "w": 300,
            "h": 50,
            "label": "Click here"
        });

        let error = validate_tutor_draw_storyboard_payload(&payload)
            .expect_err("label-only tutor draw should not be a storyboard");

        assert!(error.contains("tutor_step_label") || error.contains("step_label"));
    }

    #[test]
    fn tutor_draw_storyboard_accepts_group_inherited_label_and_narration() {
        let payload = serde_json::json!({
            "type": "group",
            "storyboard_step_id": "new-note-button",
            "tutor_step_label": "New note button",
            "narration": "This highlighted button creates a new note.",
            "shapes": [
                {"type": "highlight", "x": 100, "y": 100, "w": 120, "h": 42},
                {"type": "arrow", "from_x": 70, "from_y": 160, "to_x": 120, "to_y": 122}
            ]
        });

        let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid storyboard");

        assert_eq!(
            steps,
            vec![TutorDrawStoryboardStep {
                step_id: "new-note-button".to_string(),
                label: "New note button".to_string(),
                narration: "This highlighted button creates a new note.".to_string(),
                figure_backed: true,
            }]
        );
    }

    #[test]
    fn tutor_draw_storyboard_preserves_timed_reveal_steps() {
        let payload = serde_json::json!({
            "type": "group",
            "id": "three-step-proof",
            "shapes": [
                {
                    "type": "highlight",
                    "x": 100,
                    "y": 120,
                    "w": 280,
                    "h": 44,
                    "reveal_id": "read-equation",
                    "reveal_order": 1,
                    "tutor_step_label": "Read the equation",
                    "narration": "Start by reading the visible equation exactly as written."
                },
                {
                    "type": "formula",
                    "x": 120,
                    "y": 190,
                    "text": "x = 4",
                    "reveal_id": "solve-variable",
                    "reveal_order": 2,
                    "tutor_step_label": "Solve for x",
                    "narration": "Now isolate x on one side."
                }
            ]
        });

        let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid storyboard");

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_id, "read-equation");
        assert_eq!(
            steps[0].narration,
            "Start by reading the visible equation exactly as written."
        );
        assert_eq!(steps[1].step_id, "solve-variable");
        assert_eq!(steps[1].label, "Solve for x");
    }

    #[test]
    fn physics_v0_validates_free_body_overlay_without_concept_specific_builder() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptExplainer,
            "explain this free body diagram",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe free-body diagram");
        observe.visual_entity_map = Some(sample_physics_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "free-body-step",
            "source_entity_ids": ["body-main", "force-weight", "force-normal", "axis-main"],
            "shapes": [
                {"type": "free_body_body", "x": 360, "y": 330, "w": 140, "h": 90, "label": "body", "source_entity_ids": ["body-main"]},
                {"type": "force_arrow", "from_x": 430, "from_y": 375, "to_x": 430, "to_y": 520, "label": "mg", "source_entity_ids": ["force-weight"]},
                {"type": "force_arrow", "from_x": 430, "from_y": 375, "to_x": 430, "to_y": 235, "label": "N", "source_entity_ids": ["force-normal"]},
                {"type": "component_vector", "from_x": 430, "from_y": 375, "to_x": 550, "to_y": 375, "label": "x component"},
                {"type": "axis", "x1": 260, "y1": 560, "x2": 600, "y2": 560, "source_entity_ids": ["axis-main"]},
                {"type": "unit_label", "x": 570, "y": 535, "text": "forces in N"}
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(
            ids,
            vec!["body-main", "force-weight", "force-normal", "axis-main"]
        );
    }

    #[test]
    fn cs_v0_validates_code_stack_pointer_overlay_without_concept_specific_builder() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::ConceptDemo,
            "explain this recursion stack and heap reference",
        )
        .expect("run");
        let mut observe = step(TutorStepKind::Observe, "observe code and memory diagram");
        observe.visual_entity_map = Some(sample_cs_entity_map());
        run.propose_step(observe).expect("observe");

        let payload = serde_json::json!({
            "type": "group",
            "id": "recursion-stack-step",
            "source_entity_ids": ["code-loop", "stack-main", "heap-object", "pointer-main"],
            "shapes": [
                {"type": "code_highlight", "x": 120, "y": 180, "w": 460, "h": 44, "source_entity_ids": ["code-loop"]},
                {"type": "stack_frame", "x": 720, "y": 180, "w": 180, "h": 70, "label": "fact(3)", "source_entity_ids": ["stack-main"]},
                {"type": "heap_object", "x": 980, "y": 300, "w": 170, "h": 90, "label": "Node", "source_entity_ids": ["heap-object"]},
                {"type": "pointer_arrow", "from_x": 900, "from_y": 215, "to_x": 980, "to_y": 345, "label": "ref", "source_entity_ids": ["pointer-main"]},
                {"type": "flow_node", "x": 720, "y": 275, "w": 180, "h": 50, "label": "next call"},
                {"type": "flow_edge", "from_x": 810, "from_y": 250, "to_x": 810, "to_y": 275}
            ]
        });

        let ids = validate_tutor_draw_payload_for_run(&run, &payload).expect("valid draw");

        assert_eq!(
            ids,
            vec!["code-loop", "stack-main", "heap-object", "pointer-main"]
        );
    }

    #[test]
    fn blackboard_canvas_is_selected_for_source_free_concepts() {
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor explain recursion", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("@tutor #quick explain recursion", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor draw a diagram of momentum", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor what is recursion", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor explain vector fields", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor explain page faults", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor explain recursion", true),
            TutorCanvasMode::ScreenOverlay
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor explain this screenshot", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_canvas_mode("hey tutor show me how to open a new note", false),
            TutorCanvasMode::Blackboard
        );
        assert_eq!(
            classify_tutor_or_app_copilot_canvas_mode(
                "hey copilot show me how to open a new note",
                false
            ),
            TutorCanvasMode::ScreenOverlay
        );
    }

    #[test]
    fn blackboard_draw_can_proceed_without_observe_or_resolved_target() {
        let mut run = TutorRun::new_incremental_with_canvas(
            "run-1",
            TutorRunMode::ConceptDemo,
            "explain recursion",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");

        run.propose_step(step(TutorStepKind::Draw, "draw stack frames"))
            .expect("blackboard draw");

        assert_eq!(run.step_history.len(), 1);
        assert!(!run.has_fresh_observation);
        assert!(!run.has_resolved_target);
    }

    #[test]
    fn blackboard_store_rejects_initial_observation() {
        let store = TutorRunStore::default();

        let error = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-blackboard"),
                TutorRunMode::ConceptDemo,
                TutorCanvasMode::Blackboard,
                "explain recursion",
                true,
                None,
            )
            .unwrap_err();

        assert!(error.contains("initial_observation=false"));
    }

    #[test]
    fn start_run_attaches_registered_thinking_map_context_for_its_scope_only() {
        use crate::magician_v2::tutor_map_context::{
            tutor_map_context_registry, TutorMapContextBinding,
        };

        let store = TutorRunStore::default();
        // Unique session ids: the registry is process-global and tests run in
        // parallel, so this test must never collide with another scope.
        let bound_session = format!("session-map-ctx-{}", Uuid::new_v4().simple());
        let unbound_session = format!("session-no-map-ctx-{}", Uuid::new_v4().simple());

        tutor_map_context_registry().register(
            "anonymous",
            "default",
            &bound_session,
            TutorMapContextBinding {
                map_id: "map-1".to_string(),
                node_id: Some("n1".to_string()),
                context: "[Thinking-map reference context] test digest".to_string(),
                registered_at_ms: current_unix_time_ms(),
            },
        );

        let bound_run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", bound_session.clone()),
                TutorRunMode::ConceptExplainer,
                TutorCanvasMode::Blackboard,
                "teach the mapped decision",
                false,
                None,
            )
            .expect("bound run");
        assert_eq!(
            bound_run.thinking_map_prompt_context(),
            Some("[Thinking-map reference context] test digest")
        );

        let unbound_run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", unbound_session),
                TutorRunMode::ConceptExplainer,
                TutorCanvasMode::Blackboard,
                "teach something unrelated",
                false,
                None,
            )
            .expect("unbound run");
        assert_eq!(unbound_run.thinking_map_prompt_context(), None);

        tutor_map_context_registry().clear("anonymous", "default", &bound_session);
    }

    #[test]
    fn blackboard_draw_rejects_source_entities_without_observation() {
        let run = TutorRun::new_incremental_with_canvas(
            "run-1",
            TutorRunMode::ConceptDemo,
            "explain recursion",
            TutorCanvasMode::Blackboard,
        )
        .expect("run");
        let payload = serde_json::json!({
            "type": "group",
            "canvas_mode": "blackboard",
            "source_entity_ids": ["visible-entity"],
            "shapes": [
                {
                    "type": "stack_frame",
                    "x": 500,
                    "y": 250,
                    "w": 220,
                    "h": 90,
                    "label": "call frame",
                    "source_entity_ids": ["visible-entity"]
                }
            ]
        });

        let error = validate_tutor_draw_payload_for_run(&run, &payload).unwrap_err();

        assert!(
            error.contains("visual") || error.contains("source_entity"),
            "{error}"
        );
    }

    #[test]
    fn rolling_run_requires_observe_and_resolve_before_drawing_or_acting() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");

        let draw_error = run
            .propose_step(step(TutorStepKind::Draw, "draw guessed target"))
            .unwrap_err();
        assert!(draw_error.contains("resolving"));

        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        let action_error = run
            .propose_step({
                let mut click = step(TutorStepKind::Click, "click guessed target");
                click.safety = TutorSafetyLevel::ReversibleAction;
                click
            })
            .unwrap_err();
        assert!(action_error.contains("resolved target"));
    }

    #[test]
    fn rolling_run_forces_verify_after_each_ui_action() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::GuidedAction, "create an item")
                .expect("run");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let mut type_text = step(TutorStepKind::TypeText, "type requested text");
        type_text.safety = TutorSafetyLevel::ReversibleAction;

        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step(click).expect("click");
        run.propose_step(step(TutorStepKind::Observe, "observe destination view"))
            .expect("post observe");
        let error = run.propose_step(type_text).unwrap_err();

        assert!(error.contains("before verifying"));
    }

    #[test]
    fn rolling_run_accepts_observe_then_verify_after_action() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::GuidedAction, "create an item")
                .expect("run");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;

        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step(click).expect("click");
        assert!(run.pending_action.is_some());
        run.propose_step(step(TutorStepKind::Observe, "observe destination view"))
            .expect("post observe");
        run.propose_step(step(
            TutorStepKind::Verify,
            "verify destination view opened",
        ))
        .expect("verify");

        assert!(run.pending_action.is_none());
    }

    #[test]
    fn rolling_run_requires_confirmation_for_preexisting_destructive_action() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::GuidedAction, "delete selected item")
                .expect("run");
        let mut delete = step(TutorStepKind::Click, "delete selected item");
        delete.safety = TutorSafetyLevel::DestructiveRequiresConfirmation;

        run.propose_step(step(TutorStepKind::Observe, "observe selected item"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve delete button"))
            .expect("resolve");
        let error = run.propose_step(delete.clone()).unwrap_err();
        assert!(error.contains("needs confirmation"));

        run.propose_step(step(TutorStepKind::Confirm, "confirm destructive delete"))
            .expect("confirm");
        run.propose_step(delete).expect("delete after confirmation");
    }

    #[test]
    fn rolling_run_tracks_session_owned_created_objects() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::DemoAndCleanup, "demo item cleanup")
                .expect("run");

        run.record_created_object(TutorCreatedObject {
            label: "temporary item".to_string(),
            object_type: Some("item".to_string()),
            evidence: Some("created during run-1".to_string()),
        });

        assert!(run.owns_object_label("temporary item"));
        assert!(!run.owns_object_label("old item"));
    }

    #[test]
    fn store_starts_active_run_with_initial_observation() {
        let store = TutorRunStore::default();
        let scope = TutorRunScope::new("anonymous", "default", "session-1");

        let run = store
            .start_run(
                scope.clone(),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::ScreenOverlay,
                "show me the primary action button",
                true,
                None,
            )
            .expect("start run");

        assert_eq!(run.mode, TutorRunMode::ExplainOnly);
        assert!(run.has_fresh_observation);
        assert_eq!(run.step_history.len(), 1);
        assert_eq!(
            store
                .active_run(&scope)
                .expect("active lookup")
                .expect("active run")
                .run_id,
            run.run_id
        );
    }

    #[test]
    fn store_records_incremental_steps_and_completion() {
        let store = TutorRunStore::default();
        let scope = TutorRunScope::new("anonymous", "default", "session-1");
        let run = store
            .start_run(
                scope.clone(),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::ScreenOverlay,
                "explain toolbar",
                true,
                None,
            )
            .expect("start run");

        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve toolbar"),
            )
            .expect("resolve");
        assert!(run.has_resolved_target);

        let run = store
            .propose_step_with_lesson_milestones(
                &run.run_id,
                step(TutorStepKind::Draw, "highlight toolbar"),
                vec![
                    storyboard_step("toolbar-layout", "Toolbar layout"),
                    storyboard_step("toolbar-actions", "Toolbar actions"),
                ],
                Some(vec![
                    lesson_milestone("toolbar-layout", "Explain the toolbar layout"),
                    lesson_milestone("toolbar-actions", "Explain the toolbar actions"),
                ]),
            )
            .expect("draw");
        assert_eq!(run.step_history.len(), 3);

        let run = store.complete_run(&run.run_id).expect("complete");
        assert_eq!(run.status, TutorRunStatus::Completed);
        assert!(store.active_run(&scope).expect("active lookup").is_none());
        assert_eq!(
            store
                .scope_for_run(&run.run_id)
                .expect("historical scope lookup"),
            Some(scope)
        );
        let late_failure = store
            .fail_run_with_reason(&run.run_id, Some("late helper callback".to_string()))
            .expect("late failure is ignored");
        assert_eq!(late_failure.status, TutorRunStatus::Completed);
    }

    #[test]
    fn store_cancels_active_run_and_returns_pending_execution_for_preemption() {
        let store = TutorRunStore::default();
        let scope = TutorRunScope::new("anonymous", "default", "session-cancel");
        let run = store
            .start_run(
                scope.clone(),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the requested item",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve primary action"),
            )
            .expect("resolve target");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let run = store
            .propose_delegated_action_step(&run.run_id, click, "execution-1")
            .expect("delegate click");

        let cancelled = store
            .cancel_active_run(&scope, "operator cancelled the tutor")
            .expect("cancel run")
            .expect("active run cancellation");

        assert_eq!(cancelled.run.run_id, run.run_id);
        assert_eq!(cancelled.run.status, TutorRunStatus::Failed);
        assert_eq!(
            cancelled.run.terminal_reason.as_deref(),
            Some("operator cancelled the tutor")
        );
        assert_eq!(
            cancelled.preempted_execution_id.as_deref(),
            Some("execution-1")
        );
        assert!(cancelled.run.pending_action.is_none());
        assert!(store.active_run(&scope).expect("active lookup").is_none());
        assert!(store
            .cancel_active_run(&scope, "repeat cancel")
            .expect("idempotent cancel")
            .is_none());
    }

    #[test]
    fn active_lane_expires_fail_closed_after_idle_timeout() {
        let store = TutorRunStore::default();
        let scope = TutorRunScope::new("anonymous", "default", "session-expired");
        let run = store
            .start_run(
                scope.clone(),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::ScreenOverlay,
                "explain toolbar",
                true,
                None,
            )
            .expect("start run");
        {
            let mut inner = store.lock_inner().expect("store lock");
            inner.active_last_seen_at_ms.insert(
                run.run_id.clone(),
                current_unix_time_ms()
                    .saturating_sub(TUTOR_ACTIVE_LANE_IDLE_TIMEOUT_MS)
                    .saturating_sub(1),
            );
        }

        assert!(store.active_run(&scope).expect("active lookup").is_none());
        let expired = store
            .get_run(&run.run_id)
            .expect("load run")
            .expect("stored run");
        assert_eq!(expired.status, TutorRunStatus::Failed);
        assert!(expired
            .terminal_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("expired after 30 minutes")));
    }

    #[test]
    fn store_releases_scope_when_failure_retry_limit_is_exceeded() {
        let store = TutorRunStore::default();
        let scope = TutorRunScope::new("anonymous", "default", "session-retry-limit");
        let run = store
            .start_run(
                scope.clone(),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::ScreenOverlay,
                "explain toolbar",
                true,
                None,
            )
            .expect("start run");

        for _ in 0..=run.max_retries {
            store
                .record_step_failure(&run.run_id, step(TutorStepKind::Recover, "retry failed"))
                .expect("record failure");
        }

        let failed = store
            .get_run(&run.run_id)
            .expect("load run")
            .expect("stored run");
        assert_eq!(failed.status, TutorRunStatus::Failed);
        assert!(store.active_run(&scope).expect("active lookup").is_none());
    }

    #[test]
    fn explain_only_run_rejects_ui_mutation() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-1"),
                TutorRunMode::ExplainOnly,
                TutorCanvasMode::ScreenOverlay,
                "show how to click primary action",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve primary action"),
            )
            .expect("resolve");

        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let error = store.propose_step(&run.run_id, click).unwrap_err();

        assert!(error.contains("explain-only"));
    }

    #[test]
    fn concept_modes_reject_ui_mutation() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedSolution,
            "solve visible geometry problem",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe problem"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve triangle"))
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click answer choice");
        click.safety = TutorSafetyLevel::ReversibleAction;

        let error = run.propose_step(click).unwrap_err();

        assert!(error.contains("guided_solution tutor runs cannot change app UI state"));
    }

    #[test]
    fn concept_modes_allow_visual_teaching_loop() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::ConceptDemo, "explain slope visually")
                .expect("run");

        run.propose_step(step(TutorStepKind::Observe, "observe line graph"))
            .expect("observe");
        run.propose_step(step(
            TutorStepKind::ResolveTarget,
            "resolve axes, plotted line, and equation",
        ))
        .expect("resolve");
        run.set_or_expand_lesson_milestones(vec![
            core_milestone("axes", "Establish how to read the graph axes"),
            core_milestone("rise", "Measure the vertical rise"),
            core_milestone("run", "Measure the horizontal run"),
        ])
        .expect("adaptive slope lesson plan");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "draw slope run and rise annotations"),
            vec![
                storyboard_step("axes", "Read the axes"),
                storyboard_step("rise", "Measure the rise"),
                storyboard_step("run", "Measure the run"),
            ],
        )
        .expect("draw");
        run.propose_step(step(
            TutorStepKind::Say,
            "explain how rise over run gives the line slope",
        ))
        .expect("say");
        run.complete().expect("complete");

        assert_eq!(run.status, TutorRunStatus::Completed);
    }

    #[test]
    fn session_owned_destructive_requires_recorded_created_object() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::DemoAndCleanup, "delete demo object")
                .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe demo object"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve delete button"))
            .expect("resolve");

        let mut delete = step(TutorStepKind::Click, "delete temporary item");
        delete.target = Some("temporary item".to_string());
        delete.safety = TutorSafetyLevel::SessionOwnedDestructive;
        let error = run.propose_step(delete.clone()).unwrap_err();
        assert!(error.contains("was not recorded"));

        run.record_created_object(TutorCreatedObject {
            label: "temporary item".to_string(),
            object_type: Some("item".to_string()),
            evidence: None,
        });
        run.propose_step(delete).expect("session-owned cleanup");
    }

    #[test]
    fn validates_tutor_action_envelope_for_guided_action() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "preview primary action"),
            vec![storyboard_step(
                "primary-action-preview",
                "Primary action preview",
            )],
        )
        .expect("draw preview");
        let envelope = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "run-1",
            "step_kind": "click",
            "storyboard_step_id": "primary-action-preview",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "reversible_action",
            "observation_evidence": "Primary action button is visible in the latest screenshot",
            "action_instruction": "Click the primary action button"
        }))
        .expect("envelope");

        let step = validate_tutor_action_envelope_for_run(&run, &envelope).expect("valid action");

        assert_eq!(step.kind, TutorStepKind::Click);
        assert_eq!(step.target.as_deref(), Some("primary action button"));
    }

    #[test]
    fn rejects_tutor_action_envelope_with_wrong_run_or_visual_only_action() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "preview primary action"),
            vec![storyboard_step(
                "primary-action-preview",
                "Primary action preview",
            )],
        )
        .expect("draw preview");
        let wrong_run = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "other-run",
            "step_kind": "click",
            "storyboard_step_id": "primary-action-preview",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "reversible_action",
            "observation_evidence": "button visible",
            "action_instruction": "Click it"
        }))
        .expect("wrong run envelope");
        let error = validate_tutor_action_envelope_for_run(&run, &wrong_run).unwrap_err();
        assert!(error.contains("does not match"));

        let visual_only = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "run-1",
            "step_kind": "click",
            "storyboard_step_id": "primary-action-preview",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "visual_only",
            "observation_evidence": "button visible",
            "action_instruction": "Click it"
        }))
        .expect("visual-only envelope");
        let error = validate_tutor_action_envelope_for_run(&run, &visual_only).unwrap_err();
        assert!(error.contains("visual_only"));
    }

    #[test]
    fn rejects_ui_changing_tutor_action_without_storyboard_binding() {
        let error = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "run-1",
            "step_kind": "click",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "reversible_action",
            "observation_evidence": "button visible",
            "action_instruction": "Click it"
        }))
        .unwrap_err();

        assert!(error.contains("storyboard_step_id"));
    }

    #[test]
    fn rejects_ui_changing_tutor_action_with_stale_storyboard_binding() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "preview primary action"),
            vec![storyboard_step(
                "primary-action-preview",
                "Primary action preview",
            )],
        )
        .expect("draw preview");
        let envelope = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "run-1",
            "step_kind": "click",
            "storyboard_step_id": "different-preview",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "reversible_action",
            "observation_evidence": "button visible",
            "action_instruction": "Click it"
        }))
        .expect("envelope");

        let error = validate_tutor_action_envelope_for_run(&run, &envelope).unwrap_err();
        assert!(error.contains("prior successful screen-draw storyboard step"));
    }

    #[test]
    fn rejects_ui_changing_tutor_action_bound_to_draw_before_latest_resolve() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "open the requested item",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve primary action"))
            .expect("resolve");
        run.propose_step_with_storyboard_steps(
            step(TutorStepKind::Draw, "preview old primary action"),
            vec![storyboard_step(
                "primary-action-preview",
                "Primary action preview",
            )],
        )
        .expect("old draw preview");
        run.propose_step(step(TutorStepKind::Observe, "observe changed app"))
            .expect("observe changed app");
        run.propose_step(step(
            TutorStepKind::ResolveTarget,
            "resolve current primary action",
        ))
        .expect("resolve current target");
        let envelope = parse_tutor_action_envelope(&serde_json::json!({
            "run_id": "run-1",
            "step_kind": "click",
            "storyboard_step_id": "primary-action-preview",
            "target": "primary action button",
            "expected_state": "requested destination view is visible",
            "safety": "reversible_action",
            "observation_evidence": "button visible",
            "action_instruction": "Click it"
        }))
        .expect("envelope");

        let error = validate_tutor_action_envelope_for_run(&run, &envelope).unwrap_err();
        assert!(error.contains("prior successful screen-draw storyboard step"));
    }

    #[test]
    fn parses_tutor_action_result_line() {
        let result = extract_tutor_action_result(
            "Done.\nTUTOR_ACTION_RESULT: {\"run_id\":\"run-1\",\"status\":\"succeeded\",\"evidence\":\"destination view opened\"}\n",
        )
        .expect("result line")
        .expect("parse");

        assert_eq!(result.run_id, "run-1");
        assert_eq!(result.status, TutorActionResultStatus::Succeeded);
        assert_eq!(result.evidence, "destination view opened");
    }

    #[test]
    fn store_applies_successful_tutor_action_result_idempotently() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-result"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the requested item",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve primary action"),
            )
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let run = store.propose_step(&run.run_id, click).expect("click");
        assert!(run.pending_action.is_some());

        let application = store
            .apply_action_result(TutorActionResult {
                run_id: run.run_id.clone(),
                status: TutorActionResultStatus::Succeeded,
                evidence: "requested destination view is visible".to_string(),
            })
            .expect("apply success");

        assert!(application.applied);
        let updated = application.run.expect("updated run");
        assert!(updated.pending_action.is_none());
        assert!(updated
            .step_history
            .iter()
            .any(|record| record.step.kind == TutorStepKind::Verify));

        let duplicate = store
            .apply_action_result(TutorActionResult {
                run_id: run.run_id,
                status: TutorActionResultStatus::Succeeded,
                evidence: "requested destination view is visible".to_string(),
            })
            .expect("duplicate no-op");
        assert!(!duplicate.applied);
    }

    #[test]
    fn store_preempts_pending_delegated_action_from_copilot_user_action() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-preempt"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the requested item",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve primary action"),
            )
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let run = store
            .propose_delegated_action_step(&run.run_id, click, "exec-preempt-1")
            .expect("delegated click");
        assert_eq!(
            run.pending_action_execution_id.as_deref(),
            Some("exec-preempt-1")
        );

        let event = TutorUserActionEvent {
            run_id: run.run_id.clone(),
            storyboard_step_id: None,
            storyboard_step_label: None,
            target: Some("primary action".to_string()),
            evidence: "User clicked inside the highlighted region".to_string(),
            occurred_at_ms: 10_000,
        };
        let record = store
            .record_user_action_event_and_preempt_pending(event.clone())
            .expect("record preempt");

        assert!(record.applied_to_pending_action);
        assert_eq!(
            record.preempted_execution_id.as_deref(),
            Some("exec-preempt-1")
        );
        let awaiting_cancellation = record.run.expect("updated run");
        assert!(awaiting_cancellation.pending_action.is_some());
        assert_eq!(
            awaiting_cancellation.pending_action_execution_id.as_deref(),
            Some("exec-preempt-1")
        );

        let updated = store
            .finalize_user_action_preemption(&event)
            .expect("finalize after cancellation");
        assert!(updated.pending_action.is_none());
        assert!(updated.pending_action_execution_id.is_none());
        assert!(updated
            .step_history
            .iter()
            .any(|record| record.step.kind == TutorStepKind::Verify));

        let stale_delegate_result = store
            .apply_action_result(TutorActionResult {
                run_id: run.run_id,
                status: TutorActionResultStatus::Failed,
                evidence: "delegated action was cancelled after user preemption".to_string(),
            })
            .expect("stale delegated result");
        assert!(!stale_delegate_result.applied);
    }

    #[test]
    fn app_copilot_action_requires_and_consumes_matching_manual_check_receipt() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-copilot-receipt"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "@copilot open the highlighted item #quick",
                true,
                None,
            )
            .expect("start App Copilot run");
        assert!(run.is_app_copilot());
        assert!(run.quick);
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve open button"),
            )
            .expect("resolve");
        store
            .propose_step_with_storyboard_steps(
                &run.run_id,
                step(TutorStepKind::Draw, "preview open button"),
                vec![storyboard_step("open-step", "Open item")],
            )
            .expect("draw");
        let envelope = TutorActionEnvelope {
            run_id: run.run_id.clone(),
            step_kind: TutorStepKind::Click,
            storyboard_step_id: Some("open-step".to_string()),
            storyboard_step_label: None,
            target: "Open button".to_string(),
            expected_state: "Item is open".to_string(),
            safety: TutorSafetyLevel::ReversibleAction,
            observation_evidence: "Open button is visible".to_string(),
            action_instruction: Some("Click Open".to_string()),
            created_object: None,
        };

        let error = store
            .propose_delegated_action_envelope(&envelope, "exec-without-check")
            .expect_err("automation without a check must fail");
        assert!(error.contains("fresh no-user-action check"));

        store
            .record_copilot_action_check(&run.run_id, Some("different-step".to_string()), None)
            .expect_err("check must reference the drawn storyboard");
        store
            .record_copilot_action_check(&run.run_id, Some("open-step".to_string()), None)
            .expect("record matching check");
        let admitted = store
            .propose_delegated_action_envelope(&envelope, "exec-checked")
            .expect("checked action admitted");
        assert_eq!(
            admitted.pending_action_execution_id.as_deref(),
            Some("exec-checked")
        );
        assert!(admitted.copilot_action_check.is_none());
    }

    #[test]
    fn terminal_child_without_action_result_clears_pending_for_recovery() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-missing-result"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the requested item",
                true,
                None,
            )
            .expect("start");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve action"),
            )
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let _run = store
            .propose_delegated_action_step(&run.run_id, click, "exec-missing-result")
            .expect("delegate");

        let application = store
            .fail_pending_action_for_execution(
                "exec-missing-result",
                "terminal task omitted TUTOR_ACTION_RESULT",
            )
            .expect("failure application")
            .expect("matching execution");
        assert!(application.applied);
        let recovered = application.run.expect("run");
        assert!(recovered.pending_action.is_none());
        assert!(recovered.pending_action_execution_id.is_none());
        assert!(recovered.terminal_blocker_reason().is_some());
    }

    #[test]
    fn store_records_and_takes_matching_copilot_user_action_event() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-action"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the highlighted item",
                true,
                None,
            )
            .expect("start run");
        let event = TutorUserActionEvent {
            run_id: run.run_id.clone(),
            storyboard_step_id: Some("step-open".to_string()),
            storyboard_step_label: Some("Open item".to_string()),
            target: Some("Open button".to_string()),
            evidence: "User clicked the highlighted button".to_string(),
            occurred_at_ms: 10_000,
        };
        store
            .record_user_action_event(event.clone())
            .expect("record event");

        assert!(store
            .take_matching_user_action_event(&run.run_id, Some("other-step"), None, 0)
            .expect("take miss")
            .is_none());
        let taken = store
            .take_matching_user_action_event(&run.run_id, Some("step-open"), None, 9_500)
            .expect("take match")
            .expect("matching event");
        assert_eq!(taken, event);
        assert!(store
            .take_matching_user_action_event(&run.run_id, Some("step-open"), None, 0)
            .expect("take consumed")
            .is_none());
    }

    #[test]
    fn store_accepts_recent_untagged_copilot_user_action_for_tagged_wait() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-action-fallback"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the highlighted item",
                true,
                None,
            )
            .expect("start run");
        let event = TutorUserActionEvent {
            run_id: run.run_id.clone(),
            storyboard_step_id: None,
            storyboard_step_label: None,
            target: Some("Open button".to_string()),
            evidence: "User clicked inside a Copilot action region".to_string(),
            occurred_at_ms: 10_000,
        };
        store
            .record_user_action_event(event.clone())
            .expect("record event");

        let taken = store
            .take_matching_user_action_event(
                &run.run_id,
                Some("step-open"),
                Some("Open item"),
                9_500,
            )
            .expect("take fallback")
            .expect("recent untagged event");
        assert_eq!(taken, event);
    }

    #[test]
    fn guided_finalization_guard_rejects_draw_only_completion() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedAction,
            "show me how to open the requested item",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve button"))
            .expect("resolve");
        run.propose_step(step(TutorStepKind::Draw, "highlight button"))
            .expect("draw");

        let continuation = guided_tutor_finalization_continuation(&run).expect("continuation");

        assert_eq!(continuation.run_id, "run-1");
        assert!(continuation.reason.contains("No UI-changing tutor action"));
        assert!(continuation.instruction.contains("Do not finalize"));
    }

    #[test]
    fn guided_finalization_guard_allows_pending_delegated_action_ack() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::GuidedAction, "open the item")
                .expect("run");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        run.propose_step(step(TutorStepKind::Observe, "observe current app"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve button"))
            .expect("resolve");
        run.propose_step(click).expect("delegate click");

        assert!(guided_tutor_finalization_continuation(&run).is_none());
    }

    #[test]
    fn guided_finalization_guard_requires_complete_after_verified_action() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-result"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the item",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve button"),
            )
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let run = store.propose_step(&run.run_id, click).expect("click");
        let application = store
            .apply_action_result(TutorActionResult {
                run_id: run.run_id.clone(),
                status: TutorActionResultStatus::Succeeded,
                evidence: "destination opened".to_string(),
            })
            .expect("apply action result");
        let run = application.run.expect("run");

        let continuation = guided_tutor_finalization_continuation(&run).expect("continuation");

        assert!(continuation.reason.contains("has not been completed"));
        let completed = store.complete_run(&run.run_id).expect("complete");
        assert!(guided_tutor_finalization_continuation(&completed).is_none());
    }

    #[test]
    fn guided_finalization_guard_ignores_explain_only_runs() {
        let mut run =
            TutorRun::new_incremental("run-1", TutorRunMode::ExplainOnly, "explain toolbar")
                .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe toolbar"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve toolbar"))
            .expect("resolve");
        run.propose_step(step(TutorStepKind::Draw, "label toolbar"))
            .expect("draw");

        assert!(guided_tutor_finalization_continuation(&run).is_none());
    }

    #[test]
    fn guided_finalization_guard_ignores_concept_runs() {
        let mut run = TutorRun::new_incremental(
            "run-1",
            TutorRunMode::GuidedSolution,
            "solve visible physics problem",
        )
        .expect("run");
        run.propose_step(step(TutorStepKind::Observe, "observe question"))
            .expect("observe");
        run.propose_step(step(TutorStepKind::ResolveTarget, "resolve force diagram"))
            .expect("resolve");
        run.propose_step(step(TutorStepKind::Draw, "draw forces and axes"))
            .expect("draw");

        assert!(guided_tutor_finalization_continuation(&run).is_none());
    }

    #[test]
    fn store_applies_failed_tutor_action_result_as_failed_verification() {
        let store = TutorRunStore::default();
        let run = store
            .start_run(
                TutorRunScope::new("anonymous", "default", "session-failed-result"),
                TutorRunMode::GuidedAction,
                TutorCanvasMode::ScreenOverlay,
                "open the requested item",
                true,
                None,
            )
            .expect("start run");
        let run = store
            .propose_step(
                &run.run_id,
                step(TutorStepKind::ResolveTarget, "resolve primary action"),
            )
            .expect("resolve");
        let mut click = step(TutorStepKind::Click, "click primary action");
        click.safety = TutorSafetyLevel::ReversibleAction;
        let run = store.propose_step(&run.run_id, click).expect("click");

        let application = store
            .apply_action_result(TutorActionResult {
                run_id: run.run_id,
                status: TutorActionResultStatus::Failed,
                evidence: "button disappeared before click".to_string(),
            })
            .expect("apply failure");

        assert!(application.applied);
        let updated = application.run.expect("updated run");
        assert!(updated.pending_action.is_none());
        assert!(updated.step_history.iter().any(|record| {
            record.step.kind == TutorStepKind::Verify && record.status == TutorStepStatus::Failed
        }));
    }
}
