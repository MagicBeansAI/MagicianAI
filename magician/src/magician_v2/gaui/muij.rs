//! MUIJ (Magician UI JSON) types, validation, and file-backed storage.
//!
//! Provides the core GAUI-α type set: [`MuijDocument`], [`MuijComponent`],
//! [`MuijDelta`], [`DefaultComponentRegistry`], and [`MuijStorage`]
//! for persisting agent-owned and route-owned layouts under the configured UI
//! storage root.

use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{fs, sync::Mutex};
use tracing::warn;

use crate::magician_v2::agents::storage::validate_agent_identifier;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum MuijValidationError {
    #[error("muij_version must be \"1.0\", got \"{0}\"")]
    UnsupportedVersion(String),
    #[error("duplicate component id \"{0}\"")]
    DuplicateComponentId(String),
    #[error("unknown component type \"{0}\"")]
    UnknownComponentType(String),
    #[error("component id must not be empty")]
    EmptyComponentId,
    #[error("component \"{0}\" props must be a JSON object")]
    InvalidPropsShape(String),
    #[error("invalid agent_id: \"{0}\"")]
    InvalidAgentId(String),
    #[error("component nesting depth exceeds maximum of {0}")]
    MaxDepthExceeded(usize),
    #[error("layout exceeds maximum component count of {0}")]
    MaxComponentCount(usize),
    #[error("graph nodes exceed maximum of {0}")]
    MaxGraphNodes(usize),
    #[error("graph edges exceed maximum of {0}")]
    MaxGraphEdges(usize),
    #[error("component \"{component_id}\" graph props are invalid: {reason}")]
    InvalidGraphProps {
        component_id: String,
        reason: String,
    },
}

/// Maximum allowed nesting depth for component children (R64).
const MAX_COMPONENT_DEPTH: usize = 32;
/// R585: Maximum number of top-level components in a layout.
/// Matches the layout cap used in the emitter (MAX_LAYOUT_COMPONENTS).
const MAX_LAYOUT_COMPONENTS: usize = 500;
/// Graph family: maximum declared nodes per `Graph` component. Mirrors the
/// mobile renderers' row-admission caps so one document renders everywhere.
pub const MAX_GRAPH_NODES: usize = 200;
/// Graph family: maximum declared edges per `Graph` component.
pub const MAX_GRAPH_EDGES: usize = 400;
/// Graph family: maximum characters in a graph node id.
const MAX_GRAPH_NODE_ID_CHARS: usize = 128;
/// Graph family: maximum characters in node/edge display text.
const MAX_GRAPH_TEXT_CHARS: usize = 200;
/// Graph family: maximum metadata entries per node.
const MAX_GRAPH_METADATA_ENTRIES: usize = 8;
/// Graph family: maximum characters in a metadata key.
const MAX_GRAPH_METADATA_KEY_CHARS: usize = 64;

#[derive(Debug, Error)]
pub enum MuijStorageError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("layout already exists for key: \"{0}\"")]
    AlreadyExists(String),
    /// Wraps a [`MuijValidationError`]. Not raised by storage methods
    /// directly — exists so callers can chain `doc.validate(registry)?` inside
    /// a function returning `Result<_, MuijStorageError>`.
    #[error("validation error: {0}")]
    Validation(#[from] MuijValidationError),
    /// Raised by storage methods when `agent_id` fails path-safety checks.
    /// Distinct from [`MuijValidationError::InvalidAgentId`] which is returned
    /// by [`MuijDocument::validate`] — callers that only use storage methods
    /// only need to match this variant.
    #[error("invalid agent id: \"{0}\"")]
    InvalidAgentId(String),
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuijDocument {
    pub muij_version: String,
    pub agent_id: String,
    #[serde(default)]
    pub layout: Vec<MuijComponent>,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuijComponent {
    pub id: String,
    /// Intentionally a `String` (not an enum) for runtime extensibility.
    /// New component types can be added without recompiling. Validated at
    /// runtime via [`DefaultComponentRegistry::is_known_type`].
    pub component_type: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default = "default_props")]
    pub props: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_snapshot: Option<serde_json::Value>,
    /// Nested child components for layout containers (R50).
    /// Defaults to empty; omitted from serialization when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<MuijComponent>,
}

fn default_props() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MuijDelta {
    Upsert {
        component_id: String,
        data: serde_json::Value,
    },
    Remove {
        component_id: String,
    },
    Reorder {
        ids: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Graph family (declarative node/edge surfaces, plan 1.5)
// ---------------------------------------------------------------------------

/// Deterministic layout hint for a `Graph` component. Physics-free: the
/// renderer derives every position from node declaration order alone.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MuijGraphLayout {
    /// Topological tiers, roots at the top (cycle members form the last tier).
    #[default]
    Layered,
    /// Single ring, nodes placed in declaration order.
    Radial,
    /// Vertical stack with an adjacency summary per node.
    List,
}

/// Scalar metadata value attached to a graph node. Only scalars are admitted;
/// nested objects/arrays would re-open unbounded document sub-trees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MuijGraphMetaValue {
    Flag(bool),
    Number(f64),
    Text(String),
}

/// One declared graph node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuijGraphNode {
    pub id: String,
    pub label: String,
    /// Optional kind tag (for example a status or domain label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional bounded flat metadata shown by the detail/expand interaction.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, MuijGraphMetaValue>,
}

/// One declared graph edge. `from`/`to` must reference declared node ids.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuijGraphEdge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Typed view of a `Graph` component's props. `MuijDocument::validate`
/// deserializes these props fail-closed; unknown prop keys are tolerated the
/// same way every other MUIJ component tolerates them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MuijGraphSpec {
    #[serde(default)]
    pub nodes: Vec<MuijGraphNode>,
    #[serde(default)]
    pub edges: Vec<MuijGraphEdge>,
    #[serde(default)]
    pub layout: MuijGraphLayout,
    /// Initial focus/selection target. Must reference a declared node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_node_id: Option<String>,
    /// Node ids in reveal order for the staggered reveal interaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reveal_order: Vec<String>,
}

impl MuijGraphSpec {
    /// Deserialize a `Graph` component's props into the typed spec.
    /// Shape errors surface as a human-readable reason string; cross-field
    /// rules are enforced by [`MuijGraphSpec::validate`].
    pub fn from_props(props: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(props.clone())
            .map_err(|err| format!("graph props decode failed: {err}"))
    }

    /// Serialize the spec back into component props.
    pub fn to_props(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("graph spec is always serializable")
    }

    /// Fail-closed cross-field validation: caps, duplicate ids, dangling edge
    /// endpoints, focus/reveal references, and bounded text lengths.
    /// `component_id` names the owning component in error payloads.
    pub fn validate(&self, component_id: &str) -> Result<(), MuijValidationError> {
        let invalid = |reason: String| MuijValidationError::InvalidGraphProps {
            component_id: component_id.to_string(),
            reason,
        };

        if self.nodes.len() > MAX_GRAPH_NODES {
            return Err(MuijValidationError::MaxGraphNodes(MAX_GRAPH_NODES));
        }
        if self.edges.len() > MAX_GRAPH_EDGES {
            return Err(MuijValidationError::MaxGraphEdges(MAX_GRAPH_EDGES));
        }

        let mut seen = std::collections::HashSet::new();
        for node in &self.nodes {
            if node.id.is_empty() {
                return Err(invalid("node id must not be empty".to_string()));
            }
            if !seen.insert(node.id.clone()) {
                return Err(invalid(format!("duplicate node id \"{}\"", node.id)));
            }
            if node.id.chars().count() > MAX_GRAPH_NODE_ID_CHARS {
                return Err(invalid(format!(
                    "node id \"{}\" exceeds {MAX_GRAPH_NODE_ID_CHARS} characters",
                    node.id
                )));
            }
            if node.label.chars().count() > MAX_GRAPH_TEXT_CHARS {
                return Err(invalid(format!(
                    "node label on \"{}\" exceeds {MAX_GRAPH_TEXT_CHARS} characters",
                    node.id
                )));
            }
            if let Some(kind) = &node.kind {
                if kind.chars().count() > MAX_GRAPH_TEXT_CHARS {
                    return Err(invalid(format!(
                        "node kind on \"{}\" exceeds {MAX_GRAPH_TEXT_CHARS} characters",
                        node.id
                    )));
                }
            }
            if node.metadata.len() > MAX_GRAPH_METADATA_ENTRIES {
                return Err(invalid(format!(
                    "node \"{}\" metadata exceeds {MAX_GRAPH_METADATA_ENTRIES} entries",
                    node.id
                )));
            }
            for (key, value) in &node.metadata {
                if key.chars().count() > MAX_GRAPH_METADATA_KEY_CHARS {
                    return Err(invalid(format!(
                        "metadata key on \"{}\" exceeds {MAX_GRAPH_METADATA_KEY_CHARS} characters",
                        node.id
                    )));
                }
                if let MuijGraphMetaValue::Text(text) = value {
                    if text.chars().count() > MAX_GRAPH_TEXT_CHARS {
                        return Err(invalid(format!(
                            "metadata value for \"{key}\" on \"{}\" exceeds {MAX_GRAPH_TEXT_CHARS} characters",
                            node.id
                        )));
                    }
                }
            }
        }

        for (index, edge) in self.edges.iter().enumerate() {
            let undeclared = [&edge.from, &edge.to]
                .into_iter()
                .find(|id| !seen.contains(*id));
            if let Some(id) = undeclared {
                return Err(invalid(format!(
                    "edge {index} references undeclared node \"{id}\""
                )));
            }
            if let Some(label) = &edge.label {
                if label.chars().count() > MAX_GRAPH_TEXT_CHARS {
                    return Err(invalid(format!(
                        "edge {index} label exceeds {MAX_GRAPH_TEXT_CHARS} characters"
                    )));
                }
            }
        }

        if let Some(focus) = &self.focus_node_id {
            if !seen.contains(focus) {
                return Err(invalid(format!("focus node \"{focus}\" is not declared")));
            }
        }

        if self.reveal_order.len() > MAX_GRAPH_NODES {
            return Err(invalid(format!(
                "reveal order exceeds {MAX_GRAPH_NODES} entries"
            )));
        }
        let mut revealed = std::collections::HashSet::new();
        for id in &self.reveal_order {
            if !seen.contains(id) {
                return Err(invalid(format!(
                    "reveal order references undeclared node \"{id}\""
                )));
            }
            if !revealed.insert(id.clone()) {
                return Err(invalid(format!("reveal order repeats node \"{id}\"")));
            }
        }
        Ok(())
    }
}

impl MuijComponent {
    /// Build a `Graph` component from a typed spec. The spec is not
    /// re-validated here; documents are fail-closed by
    /// [`MuijDocument::validate`] before persistence or broadcast, matching
    /// every other constructor in this module.
    pub fn graph(id: impl Into<String>, label: impl Into<String>, spec: &MuijGraphSpec) -> Self {
        Self {
            id: id.into(),
            component_type: "Graph".to_string(),
            label: label.into(),
            source: None,
            query: None,
            props: spec.to_props(),
            static_snapshot: None,
            children: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Component registry
// ---------------------------------------------------------------------------

/// Registry containing the GAUI-α/β/γ component set (R132: direct impl, no trait).
#[derive(Debug, Clone)]
pub struct DefaultComponentRegistry;

impl DefaultComponentRegistry {
    pub fn is_known_type(&self, component_type: &str) -> bool {
        // R191: Trim whitespace before matching to tolerate minor formatting differences.
        matches!(
            component_type.trim(),
            // GAUI-α + GAUI-γ baseline
            "Gauge" | "TerminalTransient" | "Card" | "Text" | "ActionBus"
            // GAUI-β: EntityGrid (GB-F01)
            | "EntityGrid"
            // GAUI-β: Layout (GB-F03)
            | "Container" | "Stack" | "Grid" | "SplitPanel" | "Tabs" | "Panel" | "ScrollArea" | "Divider"
            // GAUI-β: Data display (GB-F04)
            | "Table" | "DataList" | "MetricCard" | "Progress" | "Badge" | "Tag"
            // GAUI-β: Input (GB-F05)
            | "Button" | "TextField" | "Select"
            // GAUI-γ: Charts (GC-F02)
            | "PieChart" | "BarChart" | "LineChart" | "AreaChart"
            | "ScatterChart" | "TrendChart" | "Sparkline" | "Heatmap"
            // GAUI-γ: Form components (GC-F03)
            | "Form" | "TextArea" | "NumberField" | "Slider" | "Checkbox"
            | "RadioGroup" | "MultiSelect" | "DatePicker" | "Toggle" | "SearchInput"
            // GAUI-γ: Feedback components (GC-F04)
            | "Alert" | "Toast" | "Notification" | "ProgressBar" | "Spinner"
            | "Skeleton" | "EmptyState" | "ConfirmDialog" | "Tooltip"
            // GAUI-δ: LiveSelectors (GD-F01)
            | "LiveSelectors"
            // GAUI-δ: Media components (GD-F02)
            | "Image" | "Video" | "Audio" | "CodeBlock" | "DiffViewer" | "Markdown" | "QRCode"
            // GAUI-δ: Collaboration components (GD-F03)
            | "CommentThread" | "ReactionBar" | "PresenceAvatars" | "ActivityFeed" | "ApprovalFlow"
            // GAUI-ε: Phase 5 components
            | "Tree" | "TreeNode"
            // Graph family: declarative node/edge surfaces (plan 1.5)
            | "Graph"
        )
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl MuijDocument {
    /// Create a new document with `muij_version` "1.0", `generated_at` set to
    /// now, and an empty layout.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            muij_version: "1.0".to_string(),
            agent_id: agent_id.into(),
            layout: Vec::new(),
            generated_at: Utc::now(),
        }
    }

    pub fn validate(&self, registry: &DefaultComponentRegistry) -> Result<(), MuijValidationError> {
        // Validate agent_id using the same rules as storage paths.
        if validate_agent_identifier(&self.agent_id).is_err() {
            return Err(MuijValidationError::InvalidAgentId(self.agent_id.clone()));
        }

        // R190: Trim whitespace before comparison to tolerate minor formatting differences.
        if self.muij_version.trim() != "1.0" {
            return Err(MuijValidationError::UnsupportedVersion(
                self.muij_version.clone(),
            ));
        }

        // R585: Enforce maximum top-level component count
        if self.layout.len() > MAX_LAYOUT_COMPONENTS {
            return Err(MuijValidationError::MaxComponentCount(
                MAX_LAYOUT_COMPONENTS,
            ));
        }

        let mut seen_ids = std::collections::HashSet::new();
        Self::validate_components(&self.layout, registry, &mut seen_ids, 0)
    }

    /// Recursively validate a list of components and their children (R50).
    /// Enforces a maximum nesting depth to prevent stack overflow (R64).
    fn validate_components(
        components: &[MuijComponent],
        registry: &DefaultComponentRegistry,
        seen_ids: &mut std::collections::HashSet<String>,
        depth: usize,
    ) -> Result<(), MuijValidationError> {
        if depth >= MAX_COMPONENT_DEPTH {
            return Err(MuijValidationError::MaxDepthExceeded(MAX_COMPONENT_DEPTH));
        }
        for component in components {
            if component.id.is_empty() {
                return Err(MuijValidationError::EmptyComponentId);
            }
            if !seen_ids.insert(component.id.clone()) {
                return Err(MuijValidationError::DuplicateComponentId(
                    component.id.clone(),
                ));
            }
            if !registry.is_known_type(&component.component_type) {
                return Err(MuijValidationError::UnknownComponentType(
                    component.component_type.clone(),
                ));
            }
            if !component.props.is_object() {
                return Err(MuijValidationError::InvalidPropsShape(component.id.clone()));
            }
            // Graph family: props must decode into a bounded, self-consistent
            // spec (fail-closed). Other component types keep their existing
            // shape-only props check.
            if component.component_type.trim() == "Graph" {
                validate_graph_props(component)?;
            }
            // Recurse into children
            if !component.children.is_empty() {
                Self::validate_components(&component.children, registry, seen_ids, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Decode and validate a `Graph` component's props. Decode failures are
/// wrapped into [`MuijValidationError::InvalidGraphProps`] so callers see the
/// same error type as every other validation rule.
fn validate_graph_props(component: &MuijComponent) -> Result<(), MuijValidationError> {
    let spec = MuijGraphSpec::from_props(&component.props).map_err(|reason| {
        MuijValidationError::InvalidGraphProps {
            component_id: component.id.clone(),
            reason,
        }
    })?;
    spec.validate(&component.id)
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// File-backed MUIJ layout storage.
/// Stores layouts under the configured agent UI root.
///
/// A write gate (`Mutex`) serializes `write_layout` and `delete_layout` calls
/// to prevent lost-update races when multiple tasks modify the same layout
/// concurrently.  Reads are lock-free.
#[derive(Debug, Clone)]
pub struct MuijStorage {
    base_dir: PathBuf,
    /// Serializes write/delete operations to prevent lost-update races.
    write_gate: Arc<Mutex<()>>,
}

const MUIJ_FILENAME: &str = "ui_layout.muij.json";

impl MuijStorage {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    /// Derive the layout path for a logical namespace and document key.
    fn layout_path_in_namespace(
        &self,
        namespace: &str,
        document_key: &str,
    ) -> Result<PathBuf, MuijStorageError> {
        validate_agent_identifier(document_key)
            .map_err(|_| MuijStorageError::InvalidAgentId(document_key.to_string()))?;
        Ok(self
            .base_dir
            .join(namespace)
            .join(document_key)
            .join(MUIJ_FILENAME))
    }

    /// Derive the layout path for an agent.
    fn layout_path(&self, agent_id: &str) -> Result<PathBuf, MuijStorageError> {
        self.layout_path_in_namespace("agents", agent_id)
    }

    /// Read the MUIJ layout for an agent. Returns `None` if no layout file exists.
    pub async fn read_layout(
        &self,
        agent_id: &str,
    ) -> Result<Option<MuijDocument>, MuijStorageError> {
        let path = self.layout_path(agent_id)?;
        match fs::read_to_string(&path).await {
            Ok(content) => {
                let doc: MuijDocument = serde_json::from_str(&content)?;
                Ok(Some(doc))
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MuijStorageError::Io(err)),
        }
    }

    /// Read the MUIJ layout for a published surface document.
    pub async fn read_surface_layout(
        &self,
        document_key: &str,
    ) -> Result<Option<MuijDocument>, MuijStorageError> {
        let path = self.layout_path_in_namespace("surfaces", document_key)?;
        match fs::read_to_string(&path).await {
            Ok(content) => {
                let doc: MuijDocument = serde_json::from_str(&content)?;
                Ok(Some(doc))
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MuijStorageError::Io(err)),
        }
    }

    /// Write the MUIJ layout for an agent. Creates parent directories if needed.
    /// The shared durable writer supplies the atomic write: unique temp, fsync,
    /// rename, parent-dir sync. Acquires the write gate to serialize concurrent
    /// writes.
    pub async fn write_layout(
        &self,
        agent_id: &str,
        document: &MuijDocument,
    ) -> Result<(), MuijStorageError> {
        let _guard = self.write_gate.lock().await;
        let path = self.layout_path(agent_id)?;
        let content = serde_json::to_vec_pretty(document)?;
        crate::magician_v2::artifact_v2::io::write_bytes_durably(&path, &content).await?;
        Ok(())
    }

    /// Write the MUIJ layout for a published surface document.
    pub async fn write_surface_layout(
        &self,
        document_key: &str,
        document: &MuijDocument,
    ) -> Result<(), MuijStorageError> {
        let _guard = self.write_gate.lock().await;
        let path = self.layout_path_in_namespace("surfaces", document_key)?;
        // Create-once semantics. The existence check races nothing: every
        // writer of this path serializes on the write gate above.
        if fs::try_exists(&path).await? {
            return Err(MuijStorageError::AlreadyExists(document_key.to_string()));
        }
        let content = serde_json::to_vec_pretty(document)?;
        crate::magician_v2::artifact_v2::io::write_bytes_durably(&path, &content).await?;
        Ok(())
    }

    /// Delete the MUIJ layout for an agent. Returns Ok even if the file doesn't exist.
    /// Acquires the write gate to serialize with concurrent writes.
    pub async fn delete_layout(&self, agent_id: &str) -> Result<(), MuijStorageError> {
        let _guard = self.write_gate.lock().await;
        let path = self.layout_path(agent_id)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(MuijStorageError::Io(err)),
        }
    }

    /// Delete the MUIJ layout for a published surface document.
    pub async fn delete_surface_layout(&self, document_key: &str) -> Result<(), MuijStorageError> {
        let _guard = self.write_gate.lock().await;
        let path = self.layout_path_in_namespace("surfaces", document_key)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(MuijStorageError::Io(err)),
        }
    }

    /// Check whether a layout file exists for an agent.
    pub async fn layout_exists(&self, agent_id: &str) -> Result<bool, MuijStorageError> {
        let path = self.layout_path(agent_id)?;
        match fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => {
                // R724: Log I/O errors instead of silently swallowing them.
                // Still return false (conservative) but now the error is observable.
                warn!(
                    path = %path.display(), error = %e,
                    "layout_exists I/O error — treating as non-existent (R724)"
                );
                Ok(false)
            },
        }
    }

    /// Check whether a surface layout file exists for a published surface document.
    pub async fn surface_layout_exists(
        &self,
        document_key: &str,
    ) -> Result<bool, MuijStorageError> {
        let path = self.layout_path_in_namespace("surfaces", document_key)?;
        match fs::try_exists(&path).await {
            Ok(exists) => Ok(exists),
            Err(e) => {
                // R724: Log I/O errors instead of silently swallowing them.
                // Still return false (conservative) but now the error is observable.
                warn!(
                    path = %path.display(), error = %e,
                    "layout_exists I/O error — treating as non-existent (R724)"
                );
                Ok(false)
            },
        }
    }

    /// List all published surface document keys currently present on disk.
    pub async fn list_surface_document_keys(&self) -> Result<Vec<String>, MuijStorageError> {
        let surfaces_dir = self.base_dir.join("surfaces");
        let mut keys = Vec::new();
        let mut entries = match fs::read_dir(&surfaces_dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
            Err(err) => return Err(MuijStorageError::Io(err)),
        };

        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_dir() {
                continue;
            }

            let Some(document_key) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if validate_agent_identifier(&document_key).is_err() {
                warn!(
                    document_key = %document_key,
                    "ignoring invalid surface layout directory during listing"
                );
                continue;
            }
            keys.push(document_key);
        }

        keys.sort();
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn sample_component(id: &str, component_type: &str) -> MuijComponent {
        MuijComponent {
            id: id.to_string(),
            component_type: component_type.to_string(),
            label: format!("{id} label"),
            source: None,
            query: None,
            props: json!({}),
            static_snapshot: None,
            children: vec![],
        }
    }

    fn sample_document() -> MuijDocument {
        MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test-agent".to_string(),
            layout: vec![
                sample_component("gauge_1", "Gauge"),
                sample_component("log_1", "TerminalTransient"),
            ],
            generated_at: Utc::now(),
        }
    }

    fn registry() -> DefaultComponentRegistry {
        DefaultComponentRegistry
    }

    // ── Serde roundtrip tests ───────────────────────────────────────────

    #[test]
    fn muij_document_serde_roundtrip() {
        let doc = sample_document();
        let json_str = serde_json::to_string_pretty(&doc).unwrap();
        let deserialized: MuijDocument = serde_json::from_str(&json_str).unwrap();
        assert_eq!(doc, deserialized);
    }

    #[test]
    fn muij_document_matches_v1_envelope_schema() {
        let doc = sample_document();
        let value: serde_json::Value = serde_json::to_value(&doc).unwrap();

        // Must have top-level fields matching GENERATIVE_AGENT_UI.md §3.2
        assert!(value.get("muij_version").is_some());
        assert!(value.get("agent_id").is_some());
        assert!(value.get("layout").is_some());
        assert!(value["layout"].is_array());

        // Each layout component must have id, component_type, label
        let first = &value["layout"][0];
        assert!(first.get("id").is_some());
        assert!(first.get("component_type").is_some());
        assert!(first.get("label").is_some());
    }

    #[test]
    fn muij_component_optional_fields_omitted_when_none() {
        let component = sample_component("g1", "Gauge");
        let value: serde_json::Value = serde_json::to_value(&component).unwrap();

        // source and query should not appear in JSON when None
        assert!(value.get("source").is_none());
        assert!(value.get("query").is_none());
        assert!(value.get("static_snapshot").is_none());
    }

    #[test]
    fn muij_component_with_source_and_query() {
        let mut component = sample_component("chart_1", "Gauge");
        component.source = Some("memory.tier[financials]".to_string());
        component.query = Some("$.items[*].{label: date, value: margin_pct}".to_string());

        let json_str = serde_json::to_string(&component).unwrap();
        let deserialized: MuijComponent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(component, deserialized);
        assert_eq!(
            deserialized.source.as_deref(),
            Some("memory.tier[financials]")
        );
    }

    #[test]
    fn muij_delta_upsert_serde() {
        let delta = MuijDelta::Upsert {
            component_id: "gauge_1".to_string(),
            data: json!({"fill": 0.5}),
        };
        let json_str = serde_json::to_string(&delta).unwrap();
        let deserialized: MuijDelta = serde_json::from_str(&json_str).unwrap();
        assert_eq!(delta, deserialized);

        // Check that tag is present
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["op"], "upsert");
    }

    #[test]
    fn muij_delta_remove_serde() {
        let delta = MuijDelta::Remove {
            component_id: "gauge_1".to_string(),
        };
        let json_str = serde_json::to_string(&delta).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["op"], "remove");
        assert_eq!(value["component_id"], "gauge_1");
    }

    #[test]
    fn muij_delta_reorder_serde() {
        let delta = MuijDelta::Reorder {
            ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        let json_str = serde_json::to_string(&delta).unwrap();
        let deserialized: MuijDelta = serde_json::from_str(&json_str).unwrap();
        assert_eq!(delta, deserialized);
    }

    #[test]
    fn empty_layout_roundtrip() {
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "empty-agent".to_string(),
            layout: vec![],
            generated_at: Utc::now(),
        };
        let json_str = serde_json::to_string(&doc).unwrap();
        let deserialized: MuijDocument = serde_json::from_str(&json_str).unwrap();
        assert_eq!(doc, deserialized);
        assert!(deserialized.layout.is_empty());
    }

    #[test]
    fn props_default_is_empty_object_when_omitted() {
        let json_str = r#"{
            "id": "g1",
            "component_type": "Gauge",
            "label": "test"
        }"#;
        let component: MuijComponent = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            component.props,
            json!({}),
            "props should default to empty object when omitted"
        );
    }

    // ── Validation tests ────────────────────────────────────────────────

    #[test]
    fn validate_accepts_valid_document() {
        let doc = sample_document();
        assert!(doc.validate(&registry()).is_ok());
    }

    #[test]
    fn validate_rejects_unsupported_version() {
        let mut doc = sample_document();
        doc.muij_version = "2.0".to_string();
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::UnsupportedVersion(_)));
    }

    #[test]
    fn validate_rejects_duplicate_component_ids() {
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![
                sample_component("same_id", "Gauge"),
                sample_component("same_id", "Card"),
            ],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::DuplicateComponentId(_)));
    }

    #[test]
    fn validate_rejects_unknown_component_type() {
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![sample_component("c1", "UnknownWidget")],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::UnknownComponentType(_)));
    }

    #[test]
    fn validate_rejects_empty_component_id() {
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![sample_component("", "Gauge")],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::EmptyComponentId));
    }

    #[test]
    fn validate_rejects_non_object_props() {
        let mut bad = sample_component("g1", "Gauge");
        bad.props = json!(null);
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![bad],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::InvalidPropsShape(id) if id == "g1"));
    }

    #[test]
    fn validate_accepts_empty_layout() {
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![],
            generated_at: Utc::now(),
        };
        assert!(doc.validate(&registry()).is_ok());
    }

    #[test]
    fn validate_recurses_into_children() {
        let mut parent = sample_component("container-1", "Container");
        parent.children = vec![sample_component("child-1", "Gauge")];
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![parent],
            generated_at: Utc::now(),
        };
        assert!(doc.validate(&registry()).is_ok());
    }

    #[test]
    fn validate_rejects_duplicate_id_in_children() {
        let mut parent = sample_component("container-1", "Container");
        // Child has same id as parent — should fail
        parent.children = vec![sample_component("container-1", "Gauge")];
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![parent],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::DuplicateComponentId(_)));
    }

    #[test]
    fn validate_rejects_unknown_type_in_children() {
        let mut parent = sample_component("container-1", "Container");
        parent.children = vec![sample_component("child-1", "UnknownWidget")];
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![parent],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::UnknownComponentType(_)));
    }

    #[test]
    fn children_omitted_from_json_when_empty() {
        let component = sample_component("c1", "Gauge");
        let json_str = serde_json::to_string(&component).unwrap();
        assert!(
            !json_str.contains("children"),
            "empty children should be omitted from JSON"
        );
    }

    #[test]
    fn children_roundtrip_serde() {
        let mut parent = sample_component("container-1", "Container");
        parent.children = vec![
            sample_component("child-1", "Gauge"),
            sample_component("child-2", "Text"),
        ];
        let json_str = serde_json::to_string(&parent).unwrap();
        assert!(
            json_str.contains("children"),
            "non-empty children should be in JSON"
        );
        let deserialized: MuijComponent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.children.len(), 2);
        assert_eq!(deserialized.children[0].id, "child-1");
        assert_eq!(deserialized.children[1].id, "child-2");
    }

    #[test]
    fn validate_rejects_excessive_depth() {
        // Build a chain deeper than MAX_COMPONENT_DEPTH
        let mut current = sample_component("leaf", "Gauge");
        for i in (0..35).rev() {
            let mut parent = sample_component(&format!("depth-{}", i), "Container");
            parent.children = vec![current];
            current = parent;
        }
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![current],
            generated_at: Utc::now(),
        };
        let err = doc.validate(&registry()).unwrap_err();
        assert!(
            matches!(err, MuijValidationError::MaxDepthExceeded(_)),
            "should reject excessive nesting depth, got: {:?}",
            err
        );
    }

    #[test]
    fn validate_accepts_depth_at_limit() {
        // Build a chain at exactly MAX_COMPONENT_DEPTH levels (32) — should pass
        let mut current = sample_component("leaf", "Gauge");
        for i in (0..31).rev() {
            // R129: 31 containers + 1 leaf = 32 levels = depth 31 at deepest
            let mut parent = sample_component(&format!("depth-{}", i), "Container");
            parent.children = vec![current];
            current = parent;
        }
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test".to_string(),
            layout: vec![current],
            generated_at: Utc::now(),
        };
        assert!(
            doc.validate(&registry()).is_ok(),
            "depth at limit (32) should be accepted"
        );
    }

    #[test]
    fn default_registry_knows_alpha_components() {
        let reg = DefaultComponentRegistry;
        assert!(reg.is_known_type("Gauge"));
        assert!(reg.is_known_type("TerminalTransient"));
        assert!(reg.is_known_type("Card"));
        assert!(reg.is_known_type("Text"));
        assert!(reg.is_known_type("ActionBus"));
        assert!(!reg.is_known_type(""));
    }

    #[test]
    fn default_registry_supports_presto_route_required_component_set() {
        let reg = DefaultComponentRegistry;
        for component_type in [
            "Card",
            "Button",
            "Form",
            "Grid",
            "Table",
            "DataList",
            "MetricCard",
        ] {
            assert!(
                reg.is_known_type(component_type),
                "required presto route component must be supported: {component_type}"
            );
        }
    }

    #[test]
    fn default_registry_knows_beta_components() {
        let reg = DefaultComponentRegistry;
        // GB-F01: EntityGrid
        assert!(reg.is_known_type("EntityGrid"));
        // GB-F03: Layout
        assert!(reg.is_known_type("Container"));
        assert!(reg.is_known_type("Stack"));
        assert!(reg.is_known_type("Grid"));
        assert!(reg.is_known_type("SplitPanel"));
        assert!(reg.is_known_type("Tabs"));
        assert!(reg.is_known_type("Panel"));
        assert!(reg.is_known_type("ScrollArea"));
        assert!(reg.is_known_type("Divider"));
        // GB-F04: Data display
        assert!(reg.is_known_type("Table"));
        assert!(reg.is_known_type("DataList"));
        assert!(reg.is_known_type("MetricCard"));
        assert!(reg.is_known_type("Progress"));
        assert!(reg.is_known_type("Badge"));
        assert!(reg.is_known_type("Tag"));
        // GB-F05: Input
        assert!(reg.is_known_type("Button"));
        assert!(reg.is_known_type("TextField"));
        assert!(reg.is_known_type("Select"));
    }

    #[test]
    fn default_registry_knows_gamma_chart_components() {
        let reg = DefaultComponentRegistry;
        assert!(reg.is_known_type("PieChart"));
        assert!(reg.is_known_type("BarChart"));
        assert!(reg.is_known_type("LineChart"));
        assert!(reg.is_known_type("AreaChart"));
        assert!(reg.is_known_type("ScatterChart"));
        assert!(reg.is_known_type("TrendChart"));
        assert!(reg.is_known_type("Sparkline"));
        assert!(reg.is_known_type("Heatmap"));
    }

    #[test]
    fn default_registry_knows_gamma_form_components() {
        let reg = DefaultComponentRegistry;
        assert!(reg.is_known_type("Form"));
        assert!(reg.is_known_type("TextArea"));
        assert!(reg.is_known_type("NumberField"));
        assert!(reg.is_known_type("Slider"));
        assert!(reg.is_known_type("Checkbox"));
        assert!(reg.is_known_type("RadioGroup"));
        assert!(reg.is_known_type("MultiSelect"));
        assert!(reg.is_known_type("DatePicker"));
        assert!(reg.is_known_type("Toggle"));
        assert!(reg.is_known_type("SearchInput"));
    }

    #[test]
    fn default_registry_knows_epsilon_components() {
        let reg = DefaultComponentRegistry;
        assert!(reg.is_known_type("Tree"));
        assert!(reg.is_known_type("TreeNode"));
    }

    #[test]
    fn default_registry_knows_gamma_feedback_components() {
        let reg = DefaultComponentRegistry;
        assert!(reg.is_known_type("Alert"));
        assert!(reg.is_known_type("Toast"));
        assert!(reg.is_known_type("Notification"));
        assert!(reg.is_known_type("ProgressBar"));
        assert!(reg.is_known_type("Spinner"));
        assert!(reg.is_known_type("Skeleton"));
        assert!(reg.is_known_type("EmptyState"));
        assert!(reg.is_known_type("ConfirmDialog"));
        assert!(reg.is_known_type("Tooltip"));
    }

    // ── Storage tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn storage_write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let doc = sample_document();

        storage.write_layout("test-agent", &doc).await.unwrap();
        let loaded = storage.read_layout("test-agent").await.unwrap();

        assert_eq!(loaded, Some(doc));
    }

    #[tokio::test]
    async fn storage_read_nonexistent_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let loaded = storage.read_layout("no-such-agent").await.unwrap();
        assert_eq!(loaded, None);
    }

    #[tokio::test]
    async fn storage_write_creates_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let doc = sample_document();

        // Parent dirs (agents/test-agent/) don't exist yet
        storage.write_layout("test-agent", &doc).await.unwrap();

        // File should exist at expected path
        let expected_path = tmp
            .path()
            .join("agents")
            .join("test-agent")
            .join(MUIJ_FILENAME);
        assert!(expected_path.exists());
    }

    #[tokio::test]
    async fn storage_overwrite_replaces_document() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let mut doc = sample_document();
        storage.write_layout("test-agent", &doc).await.unwrap();

        // Overwrite with different layout
        doc.layout.push(sample_component("card_1", "Card"));
        storage.write_layout("test-agent", &doc).await.unwrap();

        let loaded = storage.read_layout("test-agent").await.unwrap().unwrap();
        assert_eq!(loaded.layout.len(), 3); // 2 original + 1 added
    }

    #[tokio::test]
    async fn storage_delete_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let doc = sample_document();

        storage.write_layout("test-agent", &doc).await.unwrap();
        assert!(storage.layout_exists("test-agent").await.unwrap());

        storage.delete_layout("test-agent").await.unwrap();
        assert!(!storage.layout_exists("test-agent").await.unwrap());

        let loaded = storage.read_layout("test-agent").await.unwrap();
        assert_eq!(loaded, None);
    }

    #[tokio::test]
    async fn storage_delete_nonexistent_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        // Should not error
        storage.delete_layout("no-such-agent").await.unwrap();
    }

    #[tokio::test]
    async fn storage_layout_exists_false_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        assert!(!storage.layout_exists("no-agent").await.unwrap());
    }

    #[tokio::test]
    async fn storage_surface_layout_roundtrip_uses_surface_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut doc = sample_document();
        doc.agent_id = "surface-doc".to_string();

        storage
            .write_surface_layout("surface-doc", &doc)
            .await
            .unwrap();
        let loaded = storage.read_surface_layout("surface-doc").await.unwrap();

        assert_eq!(loaded, Some(doc));
        assert!(tmp
            .path()
            .join("surfaces")
            .join("surface-doc")
            .join(MUIJ_FILENAME)
            .exists());
        assert!(!tmp
            .path()
            .join("agents")
            .join("surface-doc")
            .join(MUIJ_FILENAME)
            .exists());
    }

    #[tokio::test]
    async fn storage_surface_and_agent_layouts_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let mut agent_doc = sample_document();
        agent_doc.agent_id = "shared-key".to_string();
        let mut surface_doc = sample_document();
        surface_doc.agent_id = "shared-key".to_string();
        surface_doc
            .layout
            .push(sample_component("surface-only", "Card"));

        storage
            .write_layout("shared-key", &agent_doc)
            .await
            .unwrap();
        storage
            .write_surface_layout("shared-key", &surface_doc)
            .await
            .unwrap();

        let loaded_agent = storage.read_layout("shared-key").await.unwrap().unwrap();
        let loaded_surface = storage
            .read_surface_layout("shared-key")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(loaded_agent.layout.len(), 2);
        assert_eq!(loaded_surface.layout.len(), 3);
    }

    #[tokio::test]
    async fn storage_surface_layout_rejects_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut doc = sample_document();
        doc.agent_id = "surface-doc".to_string();

        storage
            .write_surface_layout("surface-doc", &doc)
            .await
            .unwrap();
        let err = storage
            .write_surface_layout("surface-doc", &doc)
            .await
            .unwrap_err();

        assert!(matches!(err, MuijStorageError::AlreadyExists(key) if key == "surface-doc"));
    }

    #[tokio::test]
    async fn storage_lists_surface_document_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let mut doc = sample_document();

        doc.agent_id = "surface-a".to_string();
        storage
            .write_surface_layout("surface-a", &doc)
            .await
            .unwrap();
        doc.agent_id = "surface-b".to_string();
        storage
            .write_surface_layout("surface-b", &doc)
            .await
            .unwrap();

        let keys = storage.list_surface_document_keys().await.unwrap();

        assert_eq!(keys, vec!["surface-a".to_string(), "surface-b".to_string()]);
    }

    // ── Path traversal rejection tests ──────────────────────────────────

    #[tokio::test]
    async fn storage_rejects_path_traversal_dotdot() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let doc = sample_document();

        let err = storage.write_layout("../etc", &doc).await.unwrap_err();
        assert!(matches!(err, MuijStorageError::InvalidAgentId(_)));
    }

    #[tokio::test]
    async fn storage_rejects_path_traversal_slash() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let err = storage.read_layout("foo/bar").await.unwrap_err();
        assert!(matches!(err, MuijStorageError::InvalidAgentId(_)));
    }

    #[tokio::test]
    async fn storage_rejects_dot_agent_id() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let err = storage.delete_layout(".").await.unwrap_err();
        assert!(matches!(err, MuijStorageError::InvalidAgentId(_)));
    }

    #[tokio::test]
    async fn storage_rejects_empty_agent_id() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let err = storage.layout_exists("").await.unwrap_err();
        assert!(matches!(err, MuijStorageError::InvalidAgentId(_)));
    }

    #[tokio::test]
    async fn storage_rejects_backslash_agent_id() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        let err = storage.read_layout("foo\\bar").await.unwrap_err();
        assert!(matches!(err, MuijStorageError::InvalidAgentId(_)));
    }

    // ── Corrupt JSON test ───────────────────────────────────────────────

    #[tokio::test]
    async fn storage_read_corrupt_json_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agents").join("corrupt-agent");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        tokio::fs::write(agent_dir.join(MUIJ_FILENAME), b"{ not valid json }")
            .await
            .unwrap();

        let storage = MuijStorage::new(tmp.path());
        let err = storage.read_layout("corrupt-agent").await.unwrap_err();
        assert!(matches!(err, MuijStorageError::Json(_)));
    }

    // ── No leftover temp files ──────────────────────────────────────────

    #[tokio::test]
    async fn storage_write_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());
        let doc = sample_document();

        storage.write_layout("test-agent", &doc).await.unwrap();

        let agent_dir = tmp.path().join("agents").join("test-agent");
        let mut entries = tokio::fs::read_dir(&agent_dir).await.unwrap();
        let mut file_names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            file_names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(
            file_names,
            vec![MUIJ_FILENAME],
            "only the final file should remain, no temp artifacts"
        );
    }

    // ── Write gate concurrency test ──────────────────────────────────────

    #[tokio::test]
    async fn storage_concurrent_writes_do_not_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = MuijStorage::new(tmp.path());

        // Spawn 10 concurrent writes, each with a unique layout length.
        let mut handles = Vec::new();
        for i in 0..10u32 {
            let s = storage.clone();
            handles.push(tokio::spawn(async move {
                let mut doc = MuijDocument::new("race-agent");
                for j in 0..=i {
                    doc.layout.push(MuijComponent {
                        id: format!("c_{i}_{j}"),
                        component_type: "Gauge".to_string(),
                        label: format!("label_{i}_{j}"),
                        source: None,
                        query: None,
                        props: serde_json::Value::Object(Default::default()),
                        static_snapshot: None,
                        children: vec![],
                    });
                }
                s.write_layout("race-agent", &doc).await.unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // The final file must be valid JSON and one of the 10 documents.
        let loaded = storage.read_layout("race-agent").await.unwrap().unwrap();
        assert!(
            !loaded.layout.is_empty(),
            "layout should not be empty after concurrent writes"
        );
        // Verify it round-trips cleanly (no corruption).
        let json_str = serde_json::to_string_pretty(&loaded).unwrap();
        let re_parsed: MuijDocument = serde_json::from_str(&json_str).unwrap();
        assert_eq!(loaded, re_parsed);
    }

    // ── validate() agent_id checks ─────────────────────────────────────

    #[test]
    fn validate_rejects_path_traversal_agent_id() {
        let mut doc = sample_document();
        doc.agent_id = "../evil".to_string();
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::InvalidAgentId(_)));
    }

    #[test]
    fn validate_rejects_empty_agent_id() {
        let mut doc = sample_document();
        doc.agent_id = "".to_string();
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::InvalidAgentId(_)));
    }

    // ── Unknown delta op deserialization ────────────────────────────────

    #[test]
    fn unknown_delta_op_returns_serde_error() {
        let json_str = r#"{"op": "explode", "component_id": "g1"}"#;
        let result = serde_json::from_str::<MuijDelta>(json_str);
        assert!(result.is_err(), "unknown op should fail deserialization");
    }

    // ── MuijDocument::new() constructor ────────────────────────────────

    #[test]
    fn document_new_sets_defaults() {
        let doc = MuijDocument::new("my-agent");
        assert_eq!(doc.muij_version, "1.0");
        assert_eq!(doc.agent_id, "my-agent");
        assert!(doc.layout.is_empty());
        // generated_at should be very recent (within last second)
        let elapsed = Utc::now() - doc.generated_at;
        assert!(elapsed.num_seconds() < 2);
    }

    #[test]
    fn document_new_validates_cleanly() {
        let doc = MuijDocument::new("valid-agent");
        assert!(doc.validate(&registry()).is_ok());
    }

    // R585: Validation rejects documents exceeding max component count
    #[test]
    fn validate_rejects_too_many_components() {
        let mut doc = MuijDocument::new("test-agent");
        for i in 0..=MAX_LAYOUT_COMPONENTS {
            doc.layout
                .push(sample_component(&format!("comp-{i}"), "Gauge"));
        }
        let result = doc.validate(&registry());
        assert!(result.is_err());
        match result.unwrap_err() {
            MuijValidationError::MaxComponentCount(cap) => {
                assert_eq!(cap, MAX_LAYOUT_COMPONENTS);
            },
            other => panic!("expected MaxComponentCount, got {:?}", other),
        }
    }

    // R585: Validation accepts exactly MAX_LAYOUT_COMPONENTS
    #[test]
    fn validate_accepts_max_components() {
        let mut doc = MuijDocument::new("test-agent");
        for i in 0..MAX_LAYOUT_COMPONENTS {
            doc.layout
                .push(sample_component(&format!("comp-{i}"), "Gauge"));
        }
        assert!(doc.validate(&registry()).is_ok());
    }

    // ── Graph family tests ──────────────────────────────────────────────

    fn sample_graph_spec() -> MuijGraphSpec {
        MuijGraphSpec {
            nodes: vec![
                MuijGraphNode {
                    id: "root".to_string(),
                    label: "Root".to_string(),
                    kind: Some("core".to_string()),
                    metadata: std::collections::BTreeMap::new(),
                },
                MuijGraphNode {
                    id: "leaf".to_string(),
                    label: "Leaf".to_string(),
                    kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            ],
            edges: vec![MuijGraphEdge {
                from: "root".to_string(),
                to: "leaf".to_string(),
                label: Some("drives".to_string()),
            }],
            layout: MuijGraphLayout::Layered,
            focus_node_id: Some("root".to_string()),
            reveal_order: vec!["root".to_string(), "leaf".to_string()],
        }
    }

    fn graph_document(spec: &MuijGraphSpec) -> MuijDocument {
        let mut doc = MuijDocument::new("test-agent");
        doc.layout
            .push(MuijComponent::graph("graph-1", "Graph", spec));
        doc
    }

    #[test]
    fn default_registry_knows_graph_component() {
        assert!(DefaultComponentRegistry.is_known_type("Graph"));
    }

    #[test]
    fn graph_component_builder_emits_graph_type_and_props() {
        let component = MuijComponent::graph("g1", "Pipeline", &sample_graph_spec());
        assert_eq!(component.component_type, "Graph");
        assert_eq!(component.props["layout"], json!("layered"));
        assert_eq!(component.props["nodes"][0]["id"], json!("root"));
        assert_eq!(component.props["edges"][0]["from"], json!("root"));
        assert_eq!(component.props["focus_node_id"], json!("root"));
        assert_eq!(component.props["reveal_order"], json!(["root", "leaf"]));
        assert!(component.children.is_empty());
    }

    #[test]
    fn validate_accepts_well_formed_graph() {
        let doc = graph_document(&sample_graph_spec());
        assert!(doc.validate(&registry()).is_ok());
    }

    #[test]
    fn validate_accepts_empty_graph() {
        let doc = graph_document(&MuijGraphSpec::default());
        assert!(doc.validate(&registry()).is_ok());
    }

    #[test]
    fn graph_spec_roundtrips_through_props() {
        let spec = sample_graph_spec();
        let parsed = MuijGraphSpec::from_props(&spec.to_props()).unwrap();
        assert_eq!(parsed, spec);
    }

    #[test]
    fn graph_layout_defaults_to_layered_when_omitted() {
        let props = json!({"nodes": [], "edges": []});
        let spec = MuijGraphSpec::from_props(&props).unwrap();
        assert_eq!(spec.layout, MuijGraphLayout::Layered);
    }

    #[test]
    fn validate_rejects_unknown_graph_layout() {
        let mut doc = graph_document(&sample_graph_spec());
        doc.layout[0].props["layout"] = json!("physics");
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("decode failed")
        ));
    }

    #[test]
    fn validate_rejects_non_object_graph_node() {
        let mut doc = graph_document(&sample_graph_spec());
        doc.layout[0].props["nodes"] = json!(["not-an-object"]);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::InvalidGraphProps { .. }));
    }

    #[test]
    fn validate_rejects_duplicate_graph_node_ids() {
        let spec = MuijGraphSpec {
            nodes: vec![
                MuijGraphNode {
                    id: "dup".to_string(),
                    label: "First".to_string(),
                    kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
                MuijGraphNode {
                    id: "dup".to_string(),
                    label: "Second".to_string(),
                    kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            ],
            ..MuijGraphSpec::default()
        };
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("duplicate node id")
        ));
    }

    #[test]
    fn validate_rejects_edge_referencing_undeclared_node() {
        let spec = MuijGraphSpec {
            nodes: vec![MuijGraphNode {
                id: "solo".to_string(),
                label: "Solo".to_string(),
                kind: None,
                metadata: std::collections::BTreeMap::new(),
            }],
            edges: vec![MuijGraphEdge {
                from: "solo".to_string(),
                to: "ghost".to_string(),
                label: None,
            }],
            ..MuijGraphSpec::default()
        };
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("undeclared node \"ghost\"")
        ));
    }

    #[test]
    fn validate_rejects_undeclared_focus_node() {
        let mut spec = sample_graph_spec();
        spec.focus_node_id = Some("ghost".to_string());
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("focus node")
        ));
    }

    #[test]
    fn validate_rejects_reveal_order_repeating_a_node() {
        let mut spec = sample_graph_spec();
        spec.reveal_order = vec!["root".to_string(), "root".to_string()];
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("reveal order repeats")
        ));
    }

    #[test]
    fn validate_rejects_graph_node_cap() {
        let mut spec = MuijGraphSpec::default();
        for i in 0..=MAX_GRAPH_NODES {
            spec.nodes.push(MuijGraphNode {
                id: format!("n{i}"),
                label: format!("N{i}"),
                kind: None,
                metadata: std::collections::BTreeMap::new(),
            });
        }
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::MaxGraphNodes(cap) if cap == MAX_GRAPH_NODES));
    }

    #[test]
    fn validate_rejects_graph_edge_cap() {
        let spec = MuijGraphSpec {
            nodes: vec![
                MuijGraphNode {
                    id: "a".to_string(),
                    label: "A".to_string(),
                    kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
                MuijGraphNode {
                    id: "b".to_string(),
                    label: "B".to_string(),
                    kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            ],
            edges: (0..=MAX_GRAPH_EDGES)
                .map(|i| MuijGraphEdge {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    label: Some(format!("e{i}")),
                })
                .collect(),
            ..MuijGraphSpec::default()
        };
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(err, MuijValidationError::MaxGraphEdges(cap) if cap == MAX_GRAPH_EDGES));
    }

    #[test]
    fn validate_rejects_oversized_node_metadata() {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("ok".to_string(), MuijGraphMetaValue::Flag(true));
        metadata.insert("count".to_string(), MuijGraphMetaValue::Number(3.0));
        let spec = MuijGraphSpec {
            nodes: vec![MuijGraphNode {
                id: "meta".to_string(),
                label: "Meta".to_string(),
                kind: None,
                metadata,
            }],
            ..MuijGraphSpec::default()
        };
        let doc = graph_document(&spec);
        assert!(doc.validate(&registry()).is_ok());

        let mut big = spec;
        let mut metadata = std::collections::BTreeMap::new();
        for i in 0..9 {
            metadata.insert(format!("k{i}"), MuijGraphMetaValue::Flag(false));
        }
        big.nodes[0].metadata = metadata;
        let doc = graph_document(&big);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("metadata exceeds")
        ));
    }

    #[test]
    fn validate_rejects_oversized_graph_node_label() {
        let spec = MuijGraphSpec {
            nodes: vec![MuijGraphNode {
                id: "big".to_string(),
                label: "x".repeat(500),
                kind: None,
                metadata: std::collections::BTreeMap::new(),
            }],
            ..MuijGraphSpec::default()
        };
        let doc = graph_document(&spec);
        let err = doc.validate(&registry()).unwrap_err();
        assert!(matches!(
            err,
            MuijValidationError::InvalidGraphProps { ref reason, .. }
                if reason.contains("exceeds")
        ));
    }

    #[test]
    fn graph_spec_all_layouts_are_accepted() {
        for layout in [
            MuijGraphLayout::Layered,
            MuijGraphLayout::Radial,
            MuijGraphLayout::List,
        ] {
            let mut spec = sample_graph_spec();
            spec.layout = layout;
            let doc = graph_document(&spec);
            assert!(
                doc.validate(&registry()).is_ok(),
                "layout {layout:?} must validate"
            );
        }
    }

    #[test]
    fn graph_meta_value_untagged_serde_is_flat_scalars() {
        let node: MuijGraphNode = serde_json::from_value(json!({
            "id": "m1",
            "label": "M1",
            "metadata": {"flag": true, "count": 2, "note": "hi"}
        }))
        .unwrap();
        assert_eq!(
            node.metadata.get("flag"),
            Some(&MuijGraphMetaValue::Flag(true))
        );
        assert_eq!(
            node.metadata.get("count"),
            Some(&MuijGraphMetaValue::Number(2.0))
        );
        assert_eq!(
            node.metadata.get("note"),
            Some(&MuijGraphMetaValue::Text("hi".to_string()))
        );
    }
}
