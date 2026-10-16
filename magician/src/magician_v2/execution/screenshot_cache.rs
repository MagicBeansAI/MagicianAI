///! Screenshot Storage for Execution Observability
///!
///! Provides disk-based storage of page screenshots for fetch-on-demand
///! via the `/api/magician/v2/observations/{observation_id}/screenshot` endpoint.
///!
///! Screenshots are stored on disk in the execution's canonical V3 runtime
///! observation directory:
///! - Task-backed path:
///!   `magician_data_v3/scopes/<principal>/<workspace>/tasks/<task_id>/executions/{execution_id}/observations/{observation_id}.png`
///! - Non-task runtime path:
///!   `magician_data_v3/scopes/<principal>/<workspace>/executions/{execution_id}/observations/{observation_id}.png`
///! - Metadata:
///!   `.../observations/{observation_id}.json`
///!
///! SoM (Set-of-Mark) observations include additional files:
///! - Annotated screenshot:
///!   `magician_data_v3/scopes/<principal>/<workspace>/tasks/<task_id>/executions/{execution_id}/observations/{observation_id}_annotated.png`
///! - SoM data:
///!   `magician_data_v3/scopes/<principal>/<workspace>/tasks/<task_id>/executions/{execution_id}/observations/{observation_id}_som.json`
///!
///! Benefits of disk storage vs in-memory cache:
///! - Persistent across restarts (useful for debugging)
///! - No memory concerns (screenshots are 200-500KB each)
///! - Inspectable (can manually view screenshots)
///! - No expiration (keep as long as needed)
use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::{ArtifactV2Error, ArtifactV2Service};
use crate::magician_v2::execution::types::SpatialSurfaceInfo;
use crate::magician_v2::execution::AgenticExecutionSummaryRecord;
use crate::magician_v2::object_owners::{store_for_any_owner, BlobAccess};

/// Screenshot metadata stored alongside the image file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotMetadata {
    /// When this screenshot was captured
    pub captured_at: DateTime<Utc>,
    /// Execution ID for tracking
    #[serde(rename = "execution_id")]
    pub execution_id: String,
    /// Plan ID for tracking
    pub plan_id: String,
    /// Observation ID
    pub observation_id: String,
    /// Step ID this observation belongs to
    pub step_id: Option<String>,
    /// Step index in execution sequence (0-based)
    pub step_index: Option<usize>,
    /// Observation trigger type (Pre or Post action)
    pub trigger: Option<String>, // "pre" | "post" | "retry" | "recovery"
    /// Page URL at time of capture
    pub url: Option<String>,
    /// Page stage at time of capture
    pub page_stage: Option<String>,
    /// Last action executed before this observation (for loop debugging)
    /// Format: "browser:click(#btn)", "browser:scroll(Down)", etc.
    #[serde(default)]
    pub last_action: Option<String>,
    /// Whether a PNG screenshot file exists on disk for this observation.
    /// Text-first observations don't have screenshots; SoM/raw observations do.
    #[serde(default = "default_has_screenshot")]
    pub has_screenshot: bool,
}

fn default_has_screenshot() -> bool {
    true // backwards compat: existing ScreenshotMetadata files always have PNGs
}

/// Screenshot storage result
pub struct StoredScreenshot {
    /// Path to the PNG file
    pub png_path: PathBuf,
    /// Metadata
    pub metadata: ScreenshotMetadata,
}

// ============================================================================
// SoM (Set-of-Mark) Observation Storage
// ============================================================================

/// SoM observation data for debugging and observability.
///
/// This captures everything the LLM sees when making a SoM-based decision:
/// - The annotated screenshot with [N] labels
/// - The element list with selectors and bounds
/// - Any overlays detected
/// - The LLM's decision and reasoning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoMObservationData {
    /// Observation ID (matches the raw screenshot observation_id)
    pub observation_id: String,
    /// When this SoM observation was created
    pub created_at: DateTime<Utc>,
    /// Execution ID
    #[serde(rename = "execution_id")]
    pub execution_id: String,
    /// Step ID
    pub step_id: Option<String>,
    /// Iteration number in the agentic loop
    pub iteration: usize,
    /// Page URL
    pub url: String,
    /// Page title
    pub title: String,
    /// Goal being pursued
    pub goal: String,
    /// Elements with their [N] IDs (what the LLM can click)
    pub elements: Vec<SoMElementRecord>,
    /// Detected overlays (modals, chatbots, cookie banners)
    pub overlays: Vec<SoMOverlayRecord>,
    /// LLM decision made (populated after decision)
    pub decision: Option<SoMDecisionRecord>,
    /// Whether this was a retry with extended thinking
    pub was_retry: bool,
    /// Error from first attempt (if retry)
    pub retry_reason: Option<String>,
    /// Viewport and scroll state (for parity with text-first mode)
    #[serde(default)]
    pub viewport: Option<ViewportState>,
    /// Spatial surface hints captured alongside the observation.
    #[serde(default)]
    pub spatial_surfaces: Vec<SpatialSurfaceInfo>,
    /// Mode marker for identification
    #[serde(default = "default_som_mode")]
    pub mode: String,
}

fn default_som_mode() -> String {
    "som".to_string()
}

/// Record of a SoM element for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoMElementRecord {
    /// Element ID [N] shown on screenshot
    pub id: usize,
    /// HTML tag name
    pub tag: String,
    /// ARIA role or inferred role (for parity with text-first mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Visible text (truncated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Placeholder text (for inputs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Input type for form controls (for parity with text-first mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// CSS selector for targeting
    pub selector: String,
    /// Bounding box [x, y, width, height]
    pub bounds: [f64; 4],
    /// Visibility status (for parity with text-first mode):
    /// - "fully_visible": Element is fully visible in viewport (viewport_visibility=2)
    /// - "partial": Element is partially visible (viewport_visibility=1)
    /// - "above_viewport": Element is above the viewport (needs scroll up)
    /// - "below_viewport": Element is below the viewport (needs scroll down)
    /// - "hidden": Element is not clickable or occluded
    #[serde(default)]
    pub visibility: String,
    /// Whether element is inside an overlay
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_overlay: bool,
    /// Overlay type if in overlay
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay_type: Option<String>,
    /// Iframe context if in iframe (for parity with text-first mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iframe_context: Option<String>,
    /// Whether this is a non-interactive text element (for LLM context)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_text_element: bool,
    /// Framework detection hint: "native", "react", "vue", "angular", "custom-element", "shadow"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_evaluate_hint: Option<String>,
    /// CSS selector of the scroll container that clips this element (if clipped)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_container_selector: Option<String>,
    /// Direction clipped relative to scroll container: "above_in_container", "below_in_container", etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_direction: Option<String>,
    /// Approximate pixels to scroll to bring element into view within container
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_distance: Option<i32>,
    /// Whether the scroll container can actually be scrolled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_can_scroll: Option<bool>,
}

/// Record of a detected overlay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoMOverlayRecord {
    /// Overlay ID
    pub id: usize,
    /// Overlay type (modal, chatbot, cookie_banner)
    pub overlay_type: String,
    /// Z-index of the overlay
    pub z_index: i32,
    /// Close button element ID if detected
    pub close_button_id: Option<usize>,
}

/// Record of the LLM's decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoMDecisionRecord {
    /// Decision type (execute, goal_reached, cannot_proceed, need_user_input)
    pub decision_type: String,
    /// LLM's thinking/reasoning
    pub thinking: Option<String>,
    /// Evidence for goal_reached/cannot_proceed decisions
    pub evidence: Option<String>,
    /// Element ID chosen (for execute decisions)
    pub element_id: Option<usize>,
    /// Tool name (click, type, scroll, etc.)
    pub tool_name: Option<String>,
    /// Additional parameters (text for type, direction for scroll)
    pub parameters: Option<serde_json::Value>,
    /// Raw LLM response (for debugging)
    pub raw_response: String,
    /// Whether validation passed
    pub validation_passed: bool,
    /// Validation error if failed
    pub validation_error: Option<String>,
}

/// Standalone decision record for all execution modes.
/// Written to `{execution}/observations/{iteration:06}_decision.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub iteration: usize,
    pub timestamp: DateTime<Utc>,
    pub decision_type: String,
    pub action_summary: Option<String>,
    pub confidence: f64,
    pub thinking: Option<String>,
    pub result_success: Option<bool>,
    pub result_output: Option<String>,
    pub result_error: Option<String>,
    pub result_duration_ms: Option<u64>,
}

/// Viewport and scroll state for observation context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportState {
    /// Scroll position (y offset in pixels)
    pub scroll_y: Option<f64>,
    /// Viewport height in pixels
    pub viewport_height: Option<u32>,
    /// Document height (total scrollable height)
    pub document_height: Option<u32>,
    /// Focused element context (if any)
    pub focus_context: Option<String>,
    /// Page loading state
    pub loading_state: Option<String>,
}

// ============================================================================
// Text-First Observation Storage
// ============================================================================

/// Record of a text-first element for storage (parity with SoMElementRecord)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextFirstElementRecord {
    /// Element ID [N] shown in text output
    pub id: usize,
    /// HTML tag name
    pub tag: String,
    /// ARIA role or inferred role
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Computed accessible name
    pub name: String,
    /// Input type for form controls
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// Element states (focused, checked, disabled, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// CSS selector for targeting
    pub selector: String,
    /// Visibility status:
    /// - "fully_visible": Element is fully visible in viewport
    /// - "partial": Element is partially visible (edge visible)
    /// - "above_viewport": Element is above the viewport (text-first mode only)
    /// - "below_viewport": Element is below the viewport (text-first mode only)
    /// - "offscreen": Element is outside viewport, direction unknown (SoM fallback path)
    /// - "hidden": Element is not visible (display:none, visibility:hidden)
    /// - "unknown": Visibility could not be determined
    pub visibility: String,
    /// Bounding box [x, y, width, height] for parity with SoM mode
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f64; 4]>,
    /// Whether element is in a modal
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_modal: bool,
    /// Whether element is blocked by a modal
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked_by_modal: bool,
    /// Iframe context if in iframe
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iframe_context: Option<String>,
    /// Whether this is a non-interactive text element (for LLM context)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_text_element: bool,
    /// Framework detection hint: "native", "react", "vue", "angular", "custom-element", "shadow"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_evaluate_hint: Option<String>,
    /// CSS selector of the scroll container that clips this element (if clipped)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_container_selector: Option<String>,
    /// Direction clipped relative to scroll container: "above_in_container", "below_in_container", etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_direction: Option<String>,
    /// Approximate pixels to scroll to bring element into view within container
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_distance: Option<i32>,
    /// Whether the scroll container can actually be scrolled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_clip_can_scroll: Option<bool>,
}

/// Text-first observation data for debugging and observability.
///
/// Similar to SoMObservationData but for text-first mode (no screenshot).
/// Captures the page state the LLM sees when making text-based decisions:
/// - Page URL, title, and focus state
/// - Interactive elements with visibility markers
/// - Scroll position and viewport info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextFirstObservationData {
    /// Observation ID (unique identifier)
    pub observation_id: String,
    /// When this observation was created
    pub created_at: DateTime<Utc>,
    /// Execution ID
    #[serde(rename = "execution_id")]
    pub execution_id: String,
    /// Step ID
    pub step_id: Option<String>,
    /// Iteration number in the agentic loop
    pub iteration: usize,
    /// Page URL
    pub url: String,
    /// Page title
    pub title: String,
    /// Goal being pursued
    pub goal: String,
    /// Elements with their details (for parity with SoM mode)
    #[serde(default)]
    pub elements: Vec<TextFirstElementRecord>,
    /// Number of interactive elements detected
    pub element_count: usize,
    /// Number of visible elements (in viewport)
    pub visible_element_count: usize,
    /// Number of partial elements (partially visible)
    pub partial_element_count: usize,
    /// Number of off-screen elements
    pub offscreen_element_count: usize,
    /// Scroll position (y offset)
    pub scroll_y: Option<f64>,
    /// Viewport height
    pub viewport_height: Option<u32>,
    /// Document height (total scrollable)
    pub document_height: Option<u32>,
    /// Focus context from page state
    pub focus_context: Option<String>,
    /// Loading state
    pub loading_state: Option<String>,
    /// Spatial surface hints captured alongside the observation.
    #[serde(default)]
    pub spatial_surfaces: Vec<SpatialSurfaceInfo>,
    /// LLM decision made (populated after decision)
    pub decision: Option<SoMDecisionRecord>,
    /// Whether this was a retry with extended thinking (parity with SoM mode)
    #[serde(default)]
    pub was_retry: bool,
    /// Error from first attempt if retry (parity with SoM mode)
    #[serde(default)]
    pub retry_reason: Option<String>,
    /// Mode marker for identification
    pub mode: String, // "text_first"
}

/// Result of storing text-first observation
pub struct StoredTextFirstObservation {
    /// Path to the JSON file
    pub json_path: PathBuf,
    /// The stored data
    pub data: TextFirstObservationData,
}

/// Result of storing SoM observation
pub struct StoredSoMObservation {
    /// Path to the annotated PNG file
    pub annotated_png_path: PathBuf,
    /// Path to the SoM data JSON file
    pub som_data_path: PathBuf,
    /// The stored data
    pub data: SoMObservationData,
}

/// Disk-based screenshot storage
#[derive(Clone)]
pub struct ScreenshotStorage {
    v3_service: Arc<RwLock<Option<Arc<ArtifactV2Service>>>>,
}

impl ScreenshotStorage {
    /// Create a new screenshot storage with default base path
    pub fn new() -> Self {
        Self::with_base_path(PathBuf::from("magician_data_v3"))
    }

    /// Create with custom base path (e.g., from config storage_path)
    pub fn with_base_path(_base_path: PathBuf) -> Self {
        Self {
            v3_service: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_v3_service(self, v3_service: Arc<ArtifactV2Service>) -> Self {
        *self
            .v3_service
            .write()
            .expect("screenshot v3_service lock poisoned") = Some(v3_service);
        self
    }

    pub fn set_v3_service(&self, v3_service: Arc<ArtifactV2Service>) {
        *self
            .v3_service
            .write()
            .expect("screenshot v3_service lock poisoned") = Some(v3_service);
    }

    async fn runtime_execution_dir(&self, execution_id: &str) -> Option<PathBuf> {
        let v3_service = self
            .v3_service
            .read()
            .expect("screenshot v3_service lock poisoned")
            .clone()?;
        v3_service
            .resolve_runtime_execution_dir(execution_id)
            .await
            .ok()?
    }

    fn missing_execution_scope_error(execution_id: &str) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("missing_v3_execution_scope:{execution_id}"),
        )
    }

    fn artifact_error_to_io(error: ArtifactV2Error) -> std::io::Error {
        std::io::Error::other(error.to_string())
    }

    fn workspace_layout(&self) -> Option<ArtifactV2Workspace> {
        self.v3_service
            .read()
            .expect("screenshot v3_service lock poisoned")
            .as_ref()
            .map(|service| service.workspace().clone())
    }

    async fn create_dir_all_path(&self, path: &PathBuf) -> Result<(), std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .create_dir_all_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::create_dir_all(path).await
    }

    async fn read_path(&self, path: &PathBuf) -> Result<Vec<u8>, std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            if let Some((store, rel)) = store_for_any_owner(&layout, path) {
                return store
                    .get(&rel)
                    .await
                    .map_err(|error| std::io::Error::other(error.to_string()));
            }
            return layout
                .read_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::read(path).await
    }

    async fn read_to_string_path(&self, path: &PathBuf) -> Result<String, std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .read_to_string_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::read_to_string(path).await
    }

    async fn write_path(&self, path: &PathBuf, bytes: &[u8]) -> Result<(), std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            if let Some((store, rel)) = store_for_any_owner(&layout, path) {
                return store
                    .put(&rel, bytes)
                    .await
                    .map_err(|error| std::io::Error::other(error.to_string()));
            }
            return layout
                .write_path(path, bytes)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::write(path, bytes).await
    }

    async fn remove_file_path(&self, path: &PathBuf) -> Result<(), std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .remove_file_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::remove_file(path).await
    }

    async fn remove_dir_path(&self, path: &PathBuf) -> Result<(), std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .remove_dir_all_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        fs::remove_dir(path).await
    }

    async fn path_exists(&self, path: &PathBuf) -> Result<bool, std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .metadata_path(path)
                .await
                .map(|metadata| metadata.is_some())
                .map_err(Self::artifact_error_to_io);
        }
        Ok(path.exists())
    }

    async fn metadata_path(
        &self,
        path: &PathBuf,
    ) -> Result<Option<std::fs::Metadata>, std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            return layout
                .metadata_path(path)
                .await
                .map_err(Self::artifact_error_to_io);
        }
        match fs::metadata(path).await {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn read_dir_paths(&self, dir: &PathBuf) -> Result<Vec<(PathBuf, bool)>, std::io::Error> {
        if let Some(layout) = self.workspace_layout() {
            let entries = layout
                .read_dir_path(dir)
                .await
                .map_err(Self::artifact_error_to_io)?;
            return Ok(entries
                .into_iter()
                .map(|entry| (dir.join(entry.file_name), entry.is_file))
                .collect());
        }

        let mut entries = fs::read_dir(dir).await?;
        let mut paths = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            paths.push((entry.path(), file_type.is_file()));
        }
        Ok(paths)
    }

    async fn execution_dir(&self, execution_id: &str) -> Result<PathBuf, std::io::Error> {
        self.runtime_execution_dir(execution_id)
            .await
            .ok_or_else(|| Self::missing_execution_scope_error(execution_id))
    }

    async fn execution_dir_for_read(&self, execution_id: &str) -> Option<PathBuf> {
        self.runtime_execution_dir(execution_id).await
    }

    /// Get the observation directory for an execution.
    /// Path:
    /// `magician_data_v3/scopes/<principal>/<workspace>/(tasks/<task_id>/)?executions/<execution_id>/observations/`
    async fn observation_dir(&self, execution_id: &str) -> Result<PathBuf, std::io::Error> {
        Ok(self.execution_dir(execution_id).await?.join("observations"))
    }

    async fn observation_dir_for_read(&self, execution_id: &str) -> Option<PathBuf> {
        self.execution_dir_for_read(execution_id)
            .await
            .map(|dir| dir.join("observations"))
    }

    /// Get the execution summary directory for an execution.
    async fn execution_summary_dir(&self, execution_id: &str) -> Result<PathBuf, std::io::Error> {
        Ok(self.execution_dir(execution_id).await?.join("execution"))
    }

    async fn execution_summary_dir_for_read(&self, execution_id: &str) -> Option<PathBuf> {
        self.execution_dir_for_read(execution_id)
            .await
            .map(|dir| dir.join("execution"))
    }

    fn sanitize_path_component(raw: &str) -> String {
        let mut sanitized = String::with_capacity(raw.len());
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                sanitized.push(ch);
            } else {
                sanitized.push('_');
            }
        }
        sanitized
    }

    pub async fn store_execution_summary(
        &self,
        summary: &AgenticExecutionSummaryRecord,
    ) -> Result<PathBuf, std::io::Error> {
        let execution_summary_dir = self.execution_summary_dir(&summary.execution_id).await?;
        self.create_dir_all_path(&execution_summary_dir).await?;

        let timestamp = summary.timestamp.max(0);
        let plan_component = Self::sanitize_path_component(&summary.plan_id);
        let historical_path =
            execution_summary_dir.join(format!("{}_{}_summary.json", timestamp, plan_component));
        let latest_path = execution_summary_dir.join("latest_summary.json");
        let json = serde_json::to_string_pretty(summary).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("execution summary serialization failed: {}", e),
            )
        })?;

        self.write_path(&historical_path, json.as_bytes()).await?;
        self.write_path(&latest_path, json.as_bytes()).await?;

        Ok(latest_path)
    }

    pub async fn get_execution_summary(
        &self,
        execution_id: &str,
    ) -> Result<Option<AgenticExecutionSummaryRecord>, std::io::Error> {
        let Some(execution_summary_dir) = self.execution_summary_dir_for_read(execution_id).await
        else {
            return Ok(None);
        };
        let latest_path = execution_summary_dir.join("latest_summary.json");
        if !self.path_exists(&latest_path).await? {
            return Ok(None);
        }

        let json = self.read_to_string_path(&latest_path).await?;
        let summary =
            serde_json::from_str::<AgenticExecutionSummaryRecord>(&json).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("execution summary deserialization failed: {}", e),
                )
            })?;

        Ok(Some(summary))
    }

    /// Store a screenshot on disk
    ///
    /// Saves both the PNG file and metadata JSON.
    /// Returns the observation_id that can be used to fetch the screenshot later.
    pub async fn store(
        &self,
        observation_id: String,
        screenshot_data: String, // base64-encoded PNG
        execution_id: String,
        plan_id: String,
        step_id: Option<String>,
        step_index: Option<usize>,
        trigger: Option<String>,
        url: Option<String>,
        page_stage: Option<String>,
        last_action: Option<String>,
    ) -> Result<StoredScreenshot, std::io::Error> {
        let obs_dir = self.observation_dir(&execution_id).await?;

        // Create directory if it doesn't exist
        self.create_dir_all_path(&obs_dir).await?;

        let png_path = obs_dir.join(format!("{}.png", observation_id));
        let metadata_path = obs_dir.join(format!("{}.json", observation_id));

        // Decode base64 to binary PNG
        let png_bytes = match general_purpose::STANDARD.decode(&screenshot_data) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!(
                    observation_id = %observation_id,
                    error = %e,
                    "Failed to decode base64 screenshot"
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Base64 decode failed: {}", e),
                ));
            },
        };

        // Write PNG to disk
        self.write_path(&png_path, &png_bytes).await?;

        // Create and write metadata
        let metadata = ScreenshotMetadata {
            captured_at: Utc::now(),
            execution_id: execution_id.clone(),
            plan_id: plan_id.clone(),
            observation_id: observation_id.clone(),
            step_id,
            step_index,
            trigger,
            url,
            page_stage,
            last_action,
            has_screenshot: true, // we just wrote the PNG above
        };

        let metadata_json = serde_json::to_string_pretty(&metadata).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Metadata serialization failed: {}", e),
            )
        })?;

        self.write_path(&metadata_path, metadata_json.as_bytes())
            .await?;

        debug!(
            observation_id = %observation_id,
            execution_id = %execution_id,
            plan_id = %plan_id,
            png_size = png_bytes.len(),
            path = %png_path.display(),
            "Stored screenshot to disk"
        );

        Ok(StoredScreenshot { png_path, metadata })
    }

    /// Retrieve a screenshot from disk
    ///
    /// Returns the PNG data as bytes and metadata, or None if not found.
    pub async fn get(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<Option<(Vec<u8>, ScreenshotMetadata)>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(None);
        };
        let png_path = obs_dir.join(format!("{}.png", observation_id));
        let annotated_path = obs_dir.join(format!("{}_annotated.png", observation_id));
        let metadata_path = obs_dir.join(format!("{}.json", observation_id));
        let som_metadata_path = obs_dir.join(format!("{}_som.json", observation_id));

        // Check if files exist — try standard PNG first, then SoM annotated PNG
        let actual_png_path = if self.path_exists(&png_path).await? {
            &png_path
        } else if self.path_exists(&annotated_path).await? {
            &annotated_path
        } else {
            debug!(
                observation_id = %observation_id,
                execution_id = %execution_id,
                "Screenshot not found on disk"
            );
            return Ok(None);
        };

        // Prefer SoM metadata path if standard doesn't exist
        let actual_metadata_path = if self.path_exists(&metadata_path).await? {
            &metadata_path
        } else {
            &som_metadata_path
        };

        // Read PNG file
        let png_bytes = self.read_path(actual_png_path).await?;

        // Read metadata (optional - if missing, create basic metadata)
        let metadata = if self.path_exists(actual_metadata_path).await? {
            let metadata_json = self.read_to_string_path(actual_metadata_path).await?;
            // Try ScreenshotMetadata first, then SoMObservationData
            if let Ok(meta) = serde_json::from_str::<ScreenshotMetadata>(&metadata_json) {
                meta
            } else if let Ok(som_obs) = serde_json::from_str::<SoMObservationData>(&metadata_json) {
                ScreenshotMetadata {
                    captured_at: som_obs.created_at,
                    execution_id: som_obs.execution_id,
                    plan_id: String::new(),
                    observation_id: som_obs.observation_id,
                    step_id: som_obs.step_id,
                    step_index: None,
                    trigger: None,
                    url: Some(som_obs.url),
                    page_stage: Some(format!("som iter={}", som_obs.iteration)),
                    last_action: None,
                    has_screenshot: true,
                }
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Metadata deserialization failed for all known types",
                ));
            }
        } else {
            // Fallback if metadata is missing — the PNG exists since we're in get_screenshot
            ScreenshotMetadata {
                captured_at: Utc::now(),
                execution_id: execution_id.to_string(),
                plan_id: "unknown".to_string(),
                observation_id: observation_id.to_string(),
                step_id: None,
                step_index: None,
                trigger: None,
                url: None,
                page_stage: None,
                last_action: None,
                has_screenshot: true,
            }
        };

        debug!(
            observation_id = %observation_id,
            execution_id = %execution_id,
            size = png_bytes.len(),
            "Retrieved screenshot from disk"
        );

        Ok(Some((png_bytes, metadata)))
    }

    /// Retrieve a screenshot from disk as base64 string.
    ///
    /// This is a convenience method for LLM calls that expect base64-encoded images.
    pub async fn get_base64(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<Option<String>, std::io::Error> {
        match self.get(observation_id, execution_id).await? {
            Some((png_bytes, _metadata)) => {
                let base64_str = general_purpose::STANDARD.encode(&png_bytes);
                Ok(Some(base64_str))
            },
            None => Ok(None),
        }
    }

    /// Delete a screenshot from disk
    pub async fn delete(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<bool, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(false);
        };
        let png_path = obs_dir.join(format!("{}.png", observation_id));
        let metadata_path = obs_dir.join(format!("{}.json", observation_id));

        let mut deleted = false;

        if self.path_exists(&png_path).await? {
            self.remove_file_path(&png_path).await?;
            deleted = true;
        }

        if self.path_exists(&metadata_path).await? {
            self.remove_file_path(&metadata_path).await?;
        }

        if deleted {
            info!(
                observation_id = %observation_id,
                execution_id = %execution_id,
                "Deleted screenshot from disk"
            );
        }

        Ok(deleted)
    }

    /// Cleanup old screenshots for an execution
    ///
    /// Deletes screenshots older than the specified duration.
    pub async fn cleanup_old(
        &self,
        execution_id: &str,
        older_than_days: u64,
    ) -> Result<usize, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(0);
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(0);
        }

        let cutoff = Utc::now() - chrono::Duration::days(older_than_days as i64);
        let mut deleted_count = 0;

        for (path, _) in self.read_dir_paths(&obs_dir).await? {
            // Only process .json metadata files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Read metadata
            let metadata_json = match self.read_to_string_path(&path).await {
                Ok(json) => json,
                Err(_) => continue,
            };

            let metadata: ScreenshotMetadata = match serde_json::from_str(&metadata_json) {
                Ok(meta) => meta,
                Err(_) => continue,
            };

            // Check if older than cutoff
            if metadata.captured_at < cutoff {
                let observation_id = &metadata.observation_id;
                if self.delete(observation_id, execution_id).await? {
                    deleted_count += 1;
                }
            }
        }

        if deleted_count > 0 {
            info!(
                execution_id = %execution_id,
                deleted = deleted_count,
                older_than_days = older_than_days,
                "Cleaned up old screenshots"
            );
        }

        Ok(deleted_count)
    }

    /// Clear all screenshots for an execution
    ///
    /// Used when deleting an execution to clean up all associated data.
    pub async fn clear_all(&self, execution_id: &str) -> Result<usize, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(0);
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(0);
        }

        let mut deleted_count = 0;
        for (path, is_file) in self.read_dir_paths(&obs_dir).await? {
            if is_file {
                if let Err(e) = self.remove_file_path(&path).await {
                    error!(
                        path = %path.display(),
                        error = %e,
                        "Failed to delete observation file"
                    );
                } else {
                    // Only count .png files (not .json metadata)
                    if path.extension().and_then(|s| s.to_str()) == Some("png") {
                        deleted_count += 1;
                    }
                }
            }
        }

        // Try to remove the empty directory
        let _ = self.remove_dir_path(&obs_dir).await;

        if deleted_count > 0 {
            info!(
                execution_id = %execution_id,
                deleted = deleted_count,
                "Cleared all screenshots for execution"
            );
        }

        Ok(deleted_count)
    }

    /// Clear screenshots for a specific step in an execution
    ///
    /// Used when re-running a step to prevent accumulation of old observations.
    /// Only deletes observations that match the given step_id.
    pub async fn clear_for_step(
        &self,
        execution_id: &str,
        step_id: &str,
    ) -> Result<usize, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(0);
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(0);
        }

        let mut deleted_count = 0;
        for (path, _) in self.read_dir_paths(&obs_dir).await? {
            // Only process .json metadata files to find matching step_id
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Read metadata to check step_id
            let metadata_json = match self.read_to_string_path(&path).await {
                Ok(json) => json,
                Err(_) => continue,
            };

            let metadata: ScreenshotMetadata = match serde_json::from_str(&metadata_json) {
                Ok(meta) => meta,
                Err(_) => continue,
            };

            // Check if this observation belongs to the step we're clearing
            if metadata.step_id.as_deref() == Some(step_id) {
                let observation_id = &metadata.observation_id;

                // Delete both .png and .json files
                let png_path = obs_dir.join(format!("{}.png", observation_id));
                if self.path_exists(&png_path).await? {
                    if let Err(e) = self.remove_file_path(&png_path).await {
                        error!(
                            path = %png_path.display(),
                            error = %e,
                            "Failed to delete screenshot file"
                        );
                    } else {
                        deleted_count += 1;
                    }
                }

                // Delete the metadata file
                if let Err(e) = self.remove_file_path(&path).await {
                    error!(
                        path = %path.display(),
                        error = %e,
                        "Failed to delete metadata file"
                    );
                }
            }
        }

        if deleted_count > 0 {
            info!(
                execution_id = %execution_id,
                step_id = %step_id,
                deleted = deleted_count,
                "Cleared screenshots for step"
            );
        }

        Ok(deleted_count)
    }

    /// Get storage statistics for an execution
    pub async fn stats(&self, execution_id: &str) -> Result<StorageStats, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(StorageStats {
                total_screenshots: 0,
                total_size_bytes: 0,
            });
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(StorageStats {
                total_screenshots: 0,
                total_size_bytes: 0,
            });
        }

        let mut total_screenshots = 0;
        let mut total_size_bytes = 0;

        for (path, _) in self.read_dir_paths(&obs_dir).await? {
            // Only count .png files
            if path.extension().and_then(|s| s.to_str()) == Some("png") {
                total_screenshots += 1;
                if let Some(metadata) = self.metadata_path(&path).await? {
                    total_size_bytes += metadata.len();
                }
            }
        }

        Ok(StorageStats {
            total_screenshots,
            total_size_bytes,
        })
    }

    /// List all observations for an execution with their metadata
    ///
    /// Returns observation metadata sorted by captured_at timestamp (newest first).
    pub async fn list_observations(
        &self,
        execution_id: &str,
    ) -> Result<Vec<ScreenshotMetadata>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(Vec::new());
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(Vec::new());
        }

        let mut observations = Vec::new();
        for (path, _) in self.read_dir_paths(&obs_dir).await? {
            // Only process .json metadata files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Note: _som.json files are SoM observation metadata with annotated
            // screenshots. They are parsed by the SoMObservationData branch below.

            // Read and parse metadata
            let metadata_json = match self.read_to_string_path(&path).await {
                Ok(json) => json,
                Err(e) => {
                    debug!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read observation metadata"
                    );
                    continue;
                },
            };

            // Try parsing as ScreenshotMetadata first, then fall back to
            // TextFirstObservationData and SoMObservationData which use different field names.
            let metadata = if let Ok(mut meta) =
                serde_json::from_str::<ScreenshotMetadata>(&metadata_json)
            {
                // Verify the PNG actually exists on disk
                let png_path = obs_dir.join(format!("{}.png", meta.observation_id));
                meta.has_screenshot = self.path_exists(&png_path).await.unwrap_or(false);
                meta
            } else if let Ok(text_obs) =
                serde_json::from_str::<TextFirstObservationData>(&metadata_json)
            {
                // Text-first observations never have screenshots
                ScreenshotMetadata {
                    captured_at: text_obs.created_at,
                    execution_id: text_obs.execution_id,
                    plan_id: String::new(),
                    observation_id: text_obs.observation_id,
                    step_id: text_obs.step_id,
                    step_index: None,
                    trigger: None,
                    url: Some(text_obs.url),
                    page_stage: Some(format!("text_first iter={}", text_obs.iteration)),
                    last_action: None,
                    has_screenshot: false,
                }
            } else if let Ok(som_obs) = serde_json::from_str::<SoMObservationData>(&metadata_json) {
                // SoM observations have annotated screenshots
                let annotated_path =
                    obs_dir.join(format!("{}_annotated.png", som_obs.observation_id));
                let has_screenshot = self.path_exists(&annotated_path).await.unwrap_or(false);
                ScreenshotMetadata {
                    captured_at: som_obs.created_at,
                    execution_id: som_obs.execution_id,
                    plan_id: String::new(),
                    observation_id: som_obs.observation_id,
                    step_id: som_obs.step_id,
                    step_index: None,
                    trigger: None,
                    url: Some(som_obs.url),
                    page_stage: Some(format!("som iter={}", som_obs.iteration)),
                    last_action: None,
                    has_screenshot,
                }
            } else {
                debug!(
                    path = %path.display(),
                    "Failed to parse observation metadata as any known type"
                );
                continue;
            };

            observations.push(metadata);
        }

        // Sort by captured_at timestamp (newest first)
        observations.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));

        debug!(
            execution_id = %execution_id,
            count = observations.len(),
            "Listed observations for execution"
        );

        Ok(observations)
    }

    // ========================================================================
    // SoM Observation Storage Methods
    // ========================================================================

    /// Store a SoM observation (annotated screenshot + element data).
    ///
    /// This captures everything the LLM sees when making a SoM-based decision
    /// for debugging and observability. Called after creating the SoM observation.
    ///
    /// Files created:
    /// - `{observation_id}_annotated.png` - Screenshot with [N] labels
    /// - `{observation_id}_som.json` - Elements, overlays, and decision data
    pub async fn store_som_observation(
        &self,
        data: SoMObservationData,
        annotated_screenshot: &str, // base64-encoded PNG
    ) -> Result<StoredSoMObservation, std::io::Error> {
        let obs_dir = self.observation_dir(&data.execution_id).await?;

        // Create directory if it doesn't exist
        self.create_dir_all_path(&obs_dir).await?;

        let annotated_png_path = obs_dir.join(format!("{}_annotated.png", data.observation_id));
        let som_data_path = obs_dir.join(format!("{}_som.json", data.observation_id));

        // Decode base64 to binary PNG
        let png_bytes = match general_purpose::STANDARD.decode(annotated_screenshot) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!(
                    observation_id = %data.observation_id,
                    error = %e,
                    "Failed to decode base64 annotated screenshot"
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Base64 decode failed: {}", e),
                ));
            },
        };

        // Write annotated PNG to disk
        self.write_path(&annotated_png_path, &png_bytes).await?;

        // Write SoM data JSON
        let som_json = serde_json::to_string_pretty(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SoM data serialization failed: {}", e),
            )
        })?;

        self.write_path(&som_data_path, som_json.as_bytes()).await?;

        debug!(
            observation_id = %data.observation_id,
            execution_id = %data.execution_id,
            elements = data.elements.len(),
            overlays = data.overlays.len(),
            annotated_size = png_bytes.len(),
            path = %annotated_png_path.display(),
            "Stored SoM observation to disk"
        );

        Ok(StoredSoMObservation {
            annotated_png_path,
            som_data_path,
            data,
        })
    }

    /// Update a SoM observation with the LLM's decision.
    ///
    /// Called after the LLM makes a decision to record what it chose.
    pub async fn update_som_decision(
        &self,
        observation_id: &str,
        execution_id: &str,
        decision: SoMDecisionRecord,
    ) -> Result<(), std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Err(Self::missing_execution_scope_error(execution_id));
        };
        let som_data_path = obs_dir.join(format!("{}_som.json", observation_id));

        // Read existing data
        if !self.path_exists(&som_data_path).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("SoM observation not found: {}", observation_id),
            ));
        }

        let som_json = self.read_to_string_path(&som_data_path).await?;
        let mut data: SoMObservationData = serde_json::from_str(&som_json).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SoM data deserialization failed: {}", e),
            )
        })?;

        // Update with decision
        data.decision = Some(decision);

        // Write back
        let updated_json = serde_json::to_string_pretty(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SoM data serialization failed: {}", e),
            )
        })?;

        self.write_path(&som_data_path, updated_json.as_bytes())
            .await?;

        debug!(
            observation_id = %observation_id,
            execution_id = %execution_id,
            "Updated SoM observation with decision"
        );

        Ok(())
    }

    /// Retrieve a SoM observation from disk.
    pub async fn get_som_observation(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<Option<SoMObservationData>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(None);
        };
        let som_data_path = obs_dir.join(format!("{}_som.json", observation_id));

        if !self.path_exists(&som_data_path).await? {
            return Ok(None);
        }

        let som_json = self.read_to_string_path(&som_data_path).await?;
        let data: SoMObservationData = serde_json::from_str(&som_json).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SoM data deserialization failed: {}", e),
            )
        })?;

        Ok(Some(data))
    }

    /// Retrieve the annotated screenshot as base64.
    pub async fn get_annotated_screenshot(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<Option<String>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(None);
        };
        let annotated_png_path = obs_dir.join(format!("{}_annotated.png", observation_id));

        if !self.path_exists(&annotated_png_path).await? {
            return Ok(None);
        }

        let png_bytes = self.read_path(&annotated_png_path).await?;
        let base64_str = general_purpose::STANDARD.encode(&png_bytes);

        Ok(Some(base64_str))
    }

    /// List all SoM observations for an execution.
    pub async fn list_som_observations(
        &self,
        execution_id: &str,
    ) -> Result<Vec<SoMObservationData>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(Vec::new());
        };

        if !self.path_exists(&obs_dir).await? {
            return Ok(Vec::new());
        }

        let mut observations = Vec::new();
        for (path, _) in self.read_dir_paths(&obs_dir).await? {
            // Only process _som.json files
            let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !filename.ends_with("_som.json") {
                continue;
            }

            // Read and parse SoM data
            let som_json = match self.read_to_string_path(&path).await {
                Ok(json) => json,
                Err(e) => {
                    debug!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read SoM observation"
                    );
                    continue;
                },
            };

            let data: SoMObservationData = match serde_json::from_str(&som_json) {
                Ok(d) => d,
                Err(e) => {
                    debug!(
                        path = %path.display(),
                        error = %e,
                        "Failed to parse SoM observation"
                    );
                    continue;
                },
            };

            observations.push(data);
        }

        // Sort by created_at timestamp (newest first)
        observations.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        debug!(
            execution_id = %execution_id,
            count = observations.len(),
            "Listed SoM observations for execution"
        );

        Ok(observations)
    }

    // ========================================================================
    // Text-First Observation Storage Methods
    // ========================================================================

    /// Store a text-first observation (metadata only, no screenshot).
    ///
    /// This captures the page state the LLM sees in text-first mode
    /// for debugging and observability.
    ///
    /// Files created:
    /// - `{observation_id}_textfirst.json` - Page state and element metadata
    pub async fn store_text_first_observation(
        &self,
        data: TextFirstObservationData,
    ) -> Result<StoredTextFirstObservation, std::io::Error> {
        let obs_dir = self.observation_dir(&data.execution_id).await?;

        // Create directory if it doesn't exist
        self.create_dir_all_path(&obs_dir).await?;

        let json_path = obs_dir.join(format!("{}_textfirst.json", data.observation_id));

        // Write text-first data JSON
        let json_str = serde_json::to_string_pretty(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Text-first data serialization failed: {}", e),
            )
        })?;

        self.write_path(&json_path, json_str.as_bytes()).await?;

        debug!(
            observation_id = %data.observation_id,
            execution_id = %data.execution_id,
            element_count = data.element_count,
            visible = data.visible_element_count,
            partial = data.partial_element_count,
            offscreen = data.offscreen_element_count,
            path = %json_path.display(),
            "Stored text-first observation to disk"
        );

        Ok(StoredTextFirstObservation { json_path, data })
    }

    /// Retrieve the raw JSON data for any observation type.
    ///
    /// Reads the JSON file from disk and returns it as a serde_json::Value.
    /// Works for text-first (_textfirst.json), SoM (_som.json), and screenshot (.json) observations.
    pub async fn get_observation_json(
        &self,
        observation_id: &str,
        execution_id: &str,
    ) -> Result<Option<serde_json::Value>, std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Ok(None);
        };

        // Try each observation file type in order of richness
        let candidates = [
            obs_dir.join(format!("{}_textfirst.json", observation_id)),
            obs_dir.join(format!("{}_som.json", observation_id)),
            obs_dir.join(format!("{}.json", observation_id)),
        ];

        for json_path in &candidates {
            if self.path_exists(json_path).await? {
                let json_str = self.read_to_string_path(json_path).await?;
                let value: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?;
                return Ok(Some(value));
            }
        }

        Ok(None)
    }

    /// Update a text-first observation with the LLM's decision.
    pub async fn update_text_first_decision(
        &self,
        observation_id: &str,
        execution_id: &str,
        decision: SoMDecisionRecord,
    ) -> Result<(), std::io::Error> {
        let Some(obs_dir) = self.observation_dir_for_read(execution_id).await else {
            return Err(Self::missing_execution_scope_error(execution_id));
        };
        let json_path = obs_dir.join(format!("{}_textfirst.json", observation_id));

        // Read existing data
        if !self.path_exists(&json_path).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Text-first observation not found: {}", observation_id),
            ));
        }

        let json_str = self.read_to_string_path(&json_path).await?;
        let mut data: TextFirstObservationData = serde_json::from_str(&json_str).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Text-first data deserialization failed: {}", e),
            )
        })?;

        // Update with decision
        data.decision = Some(decision);

        // Write back
        let updated_json = serde_json::to_string_pretty(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Text-first data serialization failed: {}", e),
            )
        })?;

        self.write_path(&json_path, updated_json.as_bytes()).await?;

        debug!(
            observation_id = %observation_id,
            execution_id = %execution_id,
            "Updated text-first observation with decision"
        );

        Ok(())
    }

    /// Persist a standalone decision record (for all execution modes).
    pub async fn store_decision_record(
        &self,
        execution_id: &str,
        iteration: usize,
        record: &DecisionRecord,
    ) {
        let obs_dir = match self.observation_dir(execution_id).await {
            Ok(path) => path,
            Err(e) => {
                warn!("[DECISION-RECORD] Missing V3 execution scope: {}", e);
                return;
            },
        };
        if let Err(e) = self.create_dir_all_path(&obs_dir).await {
            warn!("[DECISION-RECORD] Failed to create dir: {}", e);
            return;
        }
        let filename = format!("{:06}_decision.json", iteration);
        let path = obs_dir.join(filename);
        match serde_json::to_string_pretty(record) {
            Ok(json) => {
                if let Err(e) = self.write_path(&path, json.as_bytes()).await {
                    warn!("[DECISION-RECORD] Failed to write: {}", e);
                }
            },
            Err(e) => warn!("[DECISION-RECORD] Failed to serialize: {}", e),
        }
    }
}

impl Default for ScreenshotStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::AgenticExecutionSummaryRecord;
    use crate::magician_v2::test_support::build_test_artifact_v2_harness;

    async fn build_test_storage_with_runtime_execution(
        tempdir: &tempfile::TempDir,
        execution_id: &str,
    ) -> (ScreenshotStorage, String) {
        let (v3_service, orchestrator) = build_test_artifact_v2_harness(tempdir.path());
        let task = v3_service
            .create_task(crate::magician_v2::artifact_v2::CreateTaskInput {
                principal: "principal-a".to_string(),
                workspace: "workspace-a".to_string(),
                goal_id: None,
                title: "Scoped screenshot task".to_string(),
                description: "Persist execution summaries under V3".to_string(),
                agent_id: "personal-assistant".to_string(),
                ui_thread_id: "thread-1".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("create task");

        orchestrator
            .create_execution_with_id(
                "principal-a",
                "workspace-a",
                Some("Scoped screenshot task".to_string()),
                "personal-assistant",
                execution_id,
                Some(task.manifest.task_id.clone()),
                Some(execution_id.to_string()),
            )
            .await
            .expect("create runtime execution");

        (
            ScreenshotStorage::with_base_path(tempdir.path().join("magician_data_v3"))
                .with_v3_service(v3_service),
            task.manifest.task_id,
        )
    }

    #[tokio::test]
    async fn execution_summary_roundtrip_persists_latest_record() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let (storage, _task_id) =
            build_test_storage_with_runtime_execution(&tempdir, "exec-123").await;
        let summary = AgenticExecutionSummaryRecord {
            execution_id: "exec-123".to_string(),
            loop_mode: None,
            plan_id: "plan-456".to_string(),
            step_id: "step-789".to_string(),
            outcome: "success".to_string(),
            iterations_used: 7,
            artifacts: vec!["artifact://observation/obs-1".to_string()],
            duration_ms: 3210,
            summary: "completed benchmark case".to_string(),
            timestamp: 1_710_000_000_000,
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            yield_payload: None,
            child_deliverables: Vec::new(),
        };

        let latest_path = storage
            .store_execution_summary(&summary)
            .await
            .expect("store summary");
        assert!(latest_path.exists());

        let loaded = storage
            .get_execution_summary("exec-123")
            .await
            .expect("get summary")
            .expect("summary present");
        assert_eq!(loaded.execution_id, summary.execution_id);
        assert_eq!(loaded.plan_id, summary.plan_id);
    }

    #[tokio::test]
    async fn execution_summary_requires_v3_execution_scope() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let storage = ScreenshotStorage::with_base_path(tempdir.path().join("magician_data_v3"));
        let summary = AgenticExecutionSummaryRecord {
            execution_id: "missing-exec".to_string(),
            loop_mode: None,
            plan_id: "plan-456".to_string(),
            step_id: "step-789".to_string(),
            outcome: "success".to_string(),
            iterations_used: 1,
            artifacts: Vec::new(),
            duration_ms: 10,
            summary: "missing scope".to_string(),
            timestamp: 1_710_000_000_000,
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            yield_payload: None,
            child_deliverables: Vec::new(),
        };

        let error = storage
            .store_execution_summary(&summary)
            .await
            .expect_err("store summary should fail without a V3 execution scope");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            storage
                .get_execution_summary("missing-exec")
                .await
                .expect("read should succeed")
                .is_none(),
            "reads without a V3 execution scope should fail closed"
        );
    }

    #[tokio::test]
    async fn task_backed_execution_summary_writes_to_scoped_v3_execution_workspace() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let storage_root = tempdir.path().join("magician_data_v3");
        let (storage, task_id) =
            build_test_storage_with_runtime_execution(&tempdir, "exec-task-backed").await;
        let summary = AgenticExecutionSummaryRecord {
            execution_id: "exec-task-backed".to_string(),
            loop_mode: None,
            plan_id: "plan-v3".to_string(),
            step_id: "step-1".to_string(),
            outcome: "success".to_string(),
            iterations_used: 1,
            artifacts: Vec::new(),
            duration_ms: 42,
            summary: "stored in scoped workspace".to_string(),
            timestamp: 1_710_000_000_000,
            loop_detection_type: None,
            loop_repeated_action: None,
            loop_recommendation: None,
            loop_cycle_pattern: None,
            loop_similarity: None,
            budget_dimension: None,
            budget_details: None,
            cannot_proceed_reason: None,
            yield_payload: None,
            child_deliverables: Vec::new(),
        };

        let latest_path = storage
            .store_execution_summary(&summary)
            .await
            .expect("store summary");

        let expected = tempdir
            .path()
            .join("magician_data_v3")
            .join("scopes")
            .join("principal-a")
            .join("workspace-a")
            .join("tasks")
            .join(&task_id)
            .join("executions")
            .join("exec-task-backed")
            .join("execution")
            .join("latest_summary.json");

        assert_eq!(latest_path, expected);
        assert!(expected.exists());
        assert!(
            !storage_root
                .join("executions")
                .join("exec-task-backed")
                .exists(),
            "legacy top-level executions root should stay unused for task-backed writes"
        );
    }
}

/// Storage statistics
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub total_screenshots: usize,
    pub total_size_bytes: u64,
}
