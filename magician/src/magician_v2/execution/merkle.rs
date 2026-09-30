//! Merkle Tree for Page State Tracking
//!
//! This module implements a Merkle tree for efficient page state change detection.
//! By hashing the accessibility tree into a Merkle structure, we achieve:
//! - O(1) change detection via root hash comparison
//! - Targeted diffs for LLM context (only changed subtrees)
//! - Efficient polling during wait-for-state operations
//! - Reduced memory by storing hashes instead of full snapshots
//!
//! ## Implementation Status
//!
//! ### DONE:
//! - [x] Core data structures (MerkleNode, PageMerkleTree, MerkleDiff)
//! - [x] Tree building from accessibility tree JSON
//! - [x] Semantic fingerprinting for stable node IDs
//! - [x] Hash stratification (structural vs content hashes)
//! - [x] Name normalization to reduce hash churn
//! - [x] Diff computation with O(1) root comparison
//! - [x] Changed subtree root detection
//! - [x] LCS-based sibling matching for reorder handling
//! - [x] Large sibling list fallback (threshold-based)
//! - [x] Coverage warnings for low node counts
//!
//! ### DONE (Phase 2 - Integration):
//! - [x] Integration with agentic executor - builds tree after DOM/a11y observation
//! - [x] Integration with ValidationAgent.detect_changes() - Merkle diff → PageChange
//! - [x] Integration with recovery wait_for_state_change() - O(1) hash comparison, poll budget, backoff
//!
//! ### DONE (Phase 3 - LLM Context Optimization):
//! - [x] DOM attribute extraction from backend properties (HashableProperties.from_node())
//! - [x] Targeted diff descriptions for recovery LLM (MerkleDiff.to_llm_description())
//! - [x] Token-efficient diff serialization (MerkleDiff.to_compact_json())
//! - [x] RecoveryContext.with_merkle_diff() for recovery prompts
//!
//! ### TODO (Future):
//! - [ ] Visual region Merkle tree extension
//! - [ ] Async DOM attribute fetching via CDP

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};

use super::types::PageStage;

/// Accessibility input is external page data. The builders and extraction
/// passes are iterative, and this retained-tree cap also prevents hostile pages
/// from turning one observation into unbounded memory and hashing work.
const MAX_ACCESSIBILITY_TREE_DEPTH: usize = 64;

// ============================================================================
// Core Data Structures
// ============================================================================

/// A node in the page state Merkle tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleNode {
    /// Unique stable ID based on semantic fingerprint (survives sibling reordering)
    /// Format: "parent_id/fingerprint_hash" where fingerprint = role:name_prefix
    /// Example: "0/a1b2c3d4e5f6" for a button named "Submit" under root
    pub node_id: String,

    /// Structural hash: role + name + children structure (STABLE across content changes)
    /// Use this for recovery polling - won't change on every keystroke
    pub structural_hash: String,

    /// Content hash: structural + volatile props (changes with input values, checkbox state, etc.)
    /// Use this for validation - detects if form fill actually worked
    pub content_hash: String,

    /// Node type for semantic understanding
    pub node_type: MerkleNodeType,

    /// Original accessibility tree node ID (for tracing back to CDP)
    pub ax_node_id: Option<String>,

    /// Parent node ID (None for root)
    pub parent_id: Option<String>,

    /// Children node IDs (not hashes - preserves duplicates)
    pub children: Vec<String>,

    /// Depth from root (for path building)
    pub depth: usize,

    /// Stored attributes for diff computation (CDP-sourced trees only)
    /// Contains significant attributes like aria-*, disabled, checked, class, etc.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<String, String>,

    /// Location context for this node (main document, iframe, shadow root, etc.)
    /// Used by diff to correctly label element changes without path inference.
    #[serde(default)]
    pub location: ElementLocation,
}

/// Node type classification for semantic understanding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MerkleNodeType {
    /// Leaf node representing a DOM element
    Element {
        role: String,
        name: Option<String>,
        selector: Option<String>,
    },
    /// Internal node representing a region/subtree
    Region { description: String },
    /// Root node
    Root,
}

/// Bounding box for visual region tracking in Merkle trees (Phase 3).
/// Note: Separate from types::BoundingBox which uses u32 for VisualElement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleBoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Properties included in content hash (beyond role/name).
/// Extended in Phase 3 with DOM attributes and richer context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HashableProperties {
    // Core accessibility properties
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub selected: Option<bool>,
    pub disabled: Option<bool>,
    pub expanded: Option<bool>,
    pub pressed: Option<bool>,
    pub level: Option<u32>,
    pub position_in_set: Option<u32>,
    pub aria_hidden: Option<bool>,

    // Phase 3: Extended properties for richer context
    pub required: Option<bool>,
    pub invalid: Option<bool>,
    pub value_text: Option<String>,
    pub description: Option<String>,

    // DOM-derived properties (for structural hash stability)
    pub tag_name: Option<String>,
    pub html_id: Option<String>,
    #[serde(default)]
    pub class_list: Vec<String>,
    pub href: Option<String>,
    pub src: Option<String>,
    pub placeholder: Option<String>,
    pub input_type: Option<String>,

    // ARIA attributes
    pub aria_label: Option<String>,
    pub aria_describedby: Option<String>,
    pub aria_controls: Option<String>,
    pub aria_live: Option<String>,
    pub aria_busy: Option<bool>,
    pub aria_role: Option<String>, // HTML role attribute (e.g., role="dialog")

    // HTML state attributes (not covered by ARIA)
    pub html_hidden: Option<bool>,
    pub html_open: Option<bool>,

    // Bounding box (for visual region tracking)
    pub bounds: Option<MerkleBoundingBox>,
}

/// Complete Merkle tree for a page state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMerkleTree {
    /// Root node ID (always "0" for the document root)
    pub root_id: String,

    /// Root structural hash - STABLE across content changes (typing, checkbox toggles)
    /// Use for recovery polling (waiting for page structure to change)
    pub root_structural_hash: String,

    /// Root content hash - changes with ANY content change
    /// Use for validation (did form fill work?)
    pub root_hash: String,

    /// All nodes indexed by semantic node_id (e.g., "0/a1b2c3d4/f5e6d7c8")
    /// Keying by semantic ID preserves duplicate subtrees and survives reordering
    pub nodes: HashMap<String, MerkleNode>,

    /// Capture metadata
    pub captured_at: DateTime<Utc>,
    pub url: Option<String>,
    pub page_stage: PageStage,

    /// Statistics for debugging
    pub node_count: usize,
    pub leaf_count: usize,
    pub max_depth: usize,

    /// Cross-origin iframe count (detected during CDP DOM tree build)
    pub cross_origin_iframe_count: usize,
}

/// Diff between two Merkle trees
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleDiff {
    /// Node IDs that exist in `other` but not `self` (structurally added)
    pub added: Vec<String>,

    /// Node IDs that exist in `self` but not `other` (structurally removed)
    pub removed: Vec<String>,

    /// Node IDs that exist in BOTH but have different content_hash
    /// (same semantic identity, content changed - e.g., checkbox toggled, input value changed)
    pub modified: Vec<String>,

    /// Changed subtree roots (for LLM context)
    /// These are highest-level nodes where change occurred (parent unchanged)
    pub changed_subtrees: Vec<ChangedSubtree>,

    /// Summary for logging
    pub summary: String,
}

/// Description of a changed subtree for LLM context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedSubtree {
    /// Path from root (e.g., "Main Content > Form > Input")
    pub path: String,

    /// Depth from root (helps LLM understand nesting)
    pub depth: usize,

    /// Sibling context: "item 3 of 5 in list"
    pub sibling_context: Option<String>,

    /// Parent role for context: "inside form", "inside dialog"
    pub parent_role: Option<String>,

    /// Subtree size (helps LLM gauge significance)
    pub subtree_size: usize,

    /// Type of change
    pub change_type: SubtreeChangeType,

    /// Affected element descriptions
    pub affected_elements: Vec<String>,
}

/// Type of change for a subtree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubtreeChangeType {
    Added,
    Removed,
    Modified,
}

/// Internal statistics during tree building
#[derive(Default)]
struct TreeStats {
    total_nodes: usize,
    leaf_nodes: usize,
    max_depth: usize,
}

// ============================================================================
// Name Normalization (reduces hash churn from dynamic content)
// ============================================================================

/// Compiled regexes for name normalization (static, compiled once)
static RE_TIME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{1,2}:\d{2}(:\d{2})?\s*(AM|PM|am|pm)?").unwrap());

static RE_DATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{1,2}/\d{1,2}/\d{2,4}|\d{4}-\d{2}-\d{2}").unwrap());

static RE_COUNT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+\s+").unwrap());

static RE_PRICE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$[\d,]+\.?\d*").unwrap());

/// Normalize accessible name to reduce hash churn from dynamic content
fn normalize_name(name: &str) -> String {
    let mut result = name.to_string();

    // Strip timestamps: "Updated 3:45 PM" → "Updated"
    result = RE_TIME.replace_all(&result, "").to_string();

    // Strip dates: "Dec 5, 2024" patterns
    result = RE_DATE.replace_all(&result, "").to_string();

    // Strip leading counts: "3 items" → "items"
    result = RE_COUNT.replace_all(&result, "").to_string();

    // Strip prices: "$142.50" → ""
    result = RE_PRICE.replace_all(&result, "").to_string();

    // Clean up whitespace
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ============================================================================
// A11y Form State Extraction (for merging into DOM-based Merkle trees)
// ============================================================================

/// Form state from accessibility tree (runtime values, not initial HTML attributes)
#[derive(Debug, Clone, Default)]
struct A11yFormState {
    /// Current value (for inputs, textareas)
    pub value: Option<String>,
    /// Checked state (for checkboxes, radio buttons)
    pub checked: Option<bool>,
    /// Selected state (for options)
    pub selected: Option<bool>,
    /// Role from a11y tree
    pub role: Option<String>,
}

/// Map of element identification key -> form state
/// Key is normalized: lowercase(role:name) or lowercase(placeholder) or lowercase(aria-label)
type A11yFormStateMap = HashMap<String, A11yFormState>;

/// Extract form state from accessibility tree into a lookup map
/// This captures runtime values (what user typed) vs initial HTML attributes
fn extract_a11y_form_state(a11y_tree: &serde_json::Value) -> A11yFormStateMap {
    let mut map = A11yFormStateMap::new();
    let mut pending = vec![a11y_tree];
    while let Some(node) = pending.pop() {
        extract_a11y_form_state_node(node, &mut map);
        if let Some(children) = node["children"].as_array() {
            pending.extend(children.iter().rev());
        }
    }
    map
}

fn extract_a11y_form_state_node(node: &serde_json::Value, map: &mut A11yFormStateMap) {
    let role = node["role"]["value"].as_str();
    let name = node["name"]["value"].as_str();

    // Only extract state for form-related roles
    let is_form_element = matches!(
        role,
        Some(
            "textbox"
                | "searchbox"
                | "spinbutton"
                | "slider"
                | "checkbox"
                | "radio"
                | "combobox"
                | "listbox"
                | "option"
                | "menuitemcheckbox"
                | "menuitemradio"
                | "switch"
        )
    );

    if is_form_element {
        let mut state = A11yFormState::default();
        state.role = role.map(String::from);

        // Extract properties from CDP accessibility tree format
        if let Some(properties) = node["properties"].as_array() {
            for prop in properties {
                if let (Some(prop_name), Some(value_obj)) =
                    (prop["name"].as_str(), prop["value"].as_object())
                {
                    match prop_name {
                        "checked" => {
                            // Can be "true", "false", or "mixed"
                            if let Some(v) = value_obj["value"].as_str() {
                                state.checked = Some(v == "true");
                            } else if let Some(v) = value_obj["value"].as_bool() {
                                state.checked = Some(v);
                            }
                        },
                        "selected" => {
                            state.selected = value_obj["value"].as_bool();
                        },
                        _ => {},
                    }
                }
            }
        }

        // Value is in the "value" property for inputs
        if let Some(value_prop) = node["value"]["value"].as_str() {
            state.value = Some(value_prop.to_string());
        }

        // Create multiple keys for matching (name, role:name, placeholder)
        if let Some(n) = name {
            let normalized_name = n.to_lowercase();
            if !normalized_name.is_empty() {
                // Key by name alone
                map.insert(normalized_name.clone(), state.clone());

                // Also key by role:name for disambiguation
                if let Some(r) = role {
                    map.insert(format!("{}:{}", r, normalized_name), state.clone());
                }
            }
        }
    }
}

// ============================================================================
// PageMerkleTree Implementation
// ============================================================================

impl Default for PageMerkleTree {
    fn default() -> Self {
        Self {
            root_id: String::new(),
            root_structural_hash: String::new(),
            root_hash: String::new(),
            nodes: HashMap::new(),
            captured_at: Utc::now(),
            url: None,
            page_stage: PageStage::Unknown,
            node_count: 0,
            leaf_count: 0,
            max_depth: 0,
            cross_origin_iframe_count: 0,
        }
    }
}

impl PageMerkleTree {
    /// Build Merkle tree from accessibility tree JSON (CDP format)
    pub fn from_accessibility_tree(
        tree: &serde_json::Value,
        url: Option<&str>,
        page_stage: PageStage,
    ) -> Self {
        let mut nodes = HashMap::new();
        let mut stats = TreeStats::default();

        // Root node always has ID "0"
        let root_id = "0".to_string();
        let (root_structural_hash, root_hash) = Self::build_iterative(
            tree,
            &mut nodes,
            &mut stats,
            root_id.clone(),
            None, // root has no parent
            0,    // depth 0
            &mut HashMap::new(),
        );

        // Coverage warning: low node counts may indicate accessibility issues
        const MIN_EXPECTED_NODES: usize = 10;
        const LOW_COVERAGE_THRESHOLD: usize = 50;

        if stats.total_nodes < MIN_EXPECTED_NODES {
            warn!(
                "[MERKLE] Suspiciously low node count: {} nodes. \
                 Page may have poor accessibility or use canvas/WebGL/iframes. \
                 Merkle diff may miss changes.",
                stats.total_nodes
            );
        } else if stats.total_nodes < LOW_COVERAGE_THRESHOLD {
            debug!(
                "[MERKLE] Low node count: {} nodes. Consider visual diff fallback for this page.",
                stats.total_nodes
            );
        }

        PageMerkleTree {
            root_id,
            root_structural_hash,
            root_hash,
            nodes,
            captured_at: Utc::now(),
            url: url.map(String::from),
            page_stage,
            node_count: stats.total_nodes,
            leaf_count: stats.leaf_nodes,
            max_depth: stats.max_depth,
            cross_origin_iframe_count: 0, // Accessibility tree can't detect cross-origin
        }
    }

    /// Build Merkle tree from full DOM snapshot (outerHTML).
    ///
    /// Provides 100% coverage vs ~10-50% from accessibility tree.
    /// When both DOM and accessibility tree are available, DOM takes precedence
    /// but accessibility roles are merged for semantic understanding.
    ///
    /// # Arguments
    /// * `dom_html` - Full HTML snapshot (document.body.outerHTML or similar)
    /// * `accessibility_tree` - Optional accessibility tree for role enrichment
    /// * `url` - Page URL
    /// * `page_stage` - Current page stage
    pub fn from_dom_snapshot(
        dom_html: &str,
        accessibility_tree: Option<&serde_json::Value>,
        url: Option<&str>,
        page_stage: PageStage,
    ) -> Self {
        let mut nodes = HashMap::new();
        let mut stats = TreeStats::default();

        // Extract runtime form state from a11y tree BEFORE building DOM tree
        // This captures what user actually typed vs initial HTML attribute values
        let a11y_form_state = accessibility_tree
            .map(extract_a11y_form_state)
            .unwrap_or_default();

        // Track seen fingerprints for disambiguation (prevents duplicate node_ids)
        let mut seen_fingerprints: HashMap<String, usize> = HashMap::new();

        // Parse DOM and build tree with a11y form state for accurate content hashing
        let root_id = "0".to_string();
        let (root_structural_hash, root_hash) = Self::build_from_dom_recursive(
            dom_html,
            &mut nodes,
            &mut stats,
            root_id.clone(),
            None, // root has no parent
            0,    // depth 0
            &a11y_form_state,
            &mut seen_fingerprints,
        );

        // Merge accessibility tree roles if available (for role enrichment, not state)
        if let Some(a11y_tree) = accessibility_tree {
            Self::merge_accessibility_roles(&mut nodes, a11y_tree);
        }

        debug!(
            "[MERKLE-DOM] Built tree from DOM: {} nodes, {} leaves, max_depth={}",
            stats.total_nodes, stats.leaf_nodes, stats.max_depth
        );

        PageMerkleTree {
            root_id,
            root_structural_hash,
            root_hash,
            nodes,
            captured_at: Utc::now(),
            url: url.map(String::from),
            page_stage,
            node_count: stats.total_nodes,
            leaf_count: stats.leaf_nodes,
            max_depth: stats.max_depth,
            cross_origin_iframe_count: 0, // DOM snapshot can't detect cross-origin
        }
    }

    /// Build Merkle tree recursively from DOM HTML
    fn build_from_dom_recursive(
        html: &str,
        nodes: &mut HashMap<String, MerkleNode>,
        stats: &mut TreeStats,
        node_id: String,
        parent_id: Option<String>,
        depth: usize,
        a11y_form_state: &A11yFormStateMap,
        seen_fingerprints: &mut HashMap<String, usize>,
    ) -> (String, String) {
        stats.total_nodes += 1;
        stats.max_depth = stats.max_depth.max(depth);

        // Cap depth to prevent stack overflow on deeply nested DOM
        const MAX_DEPTH: usize = 50;
        if depth > MAX_DEPTH {
            let hash = blake3::hash(html.as_bytes()).to_hex()[..16].to_string();
            let node = MerkleNode {
                node_id: node_id.clone(),
                structural_hash: hash.clone(),
                content_hash: hash.clone(),
                node_type: MerkleNodeType::Element {
                    role: "truncated".to_string(),
                    name: Some("Depth limit reached".to_string()),
                    selector: None,
                },
                ax_node_id: None,
                parent_id,
                children: vec![],
                depth,
                attributes: HashMap::new(),
                location: ElementLocation::MainDocument, // Depth-truncated nodes are in main doc
            };
            nodes.insert(node_id, node);
            return (hash.clone(), hash);
        }

        // Extract tag info from HTML
        let (tag, attributes, inner_html, is_self_closing) = Self::parse_element(html);

        // Extract key attributes for fingerprinting
        let id = attributes.get("id").cloned();
        let class = attributes.get("class").cloned();
        let text_content = Self::extract_text_content(&inner_html);

        // Build properties for hashing
        let mut props = HashableProperties::default();
        props.tag_name = Some(tag.clone());
        props.html_id = id.clone();
        if let Some(cls) = &class {
            props.class_list = cls.split_whitespace().map(String::from).collect();
        }
        props.aria_label = attributes.get("aria-label").cloned();
        props.placeholder = attributes.get("placeholder").cloned();
        props.input_type = attributes.get("type").cloned();
        props.href = attributes.get("href").cloned();

        // Extract form input values for content hashing
        // Without these, form fill changes are invisible to content diff
        if matches!(tag.as_str(), "input" | "textarea" | "select" | "option") {
            // Start with HTML attribute values (initial state)
            props.value = attributes.get("value").cloned();
            props.checked = Some(attributes.contains_key("checked"));
            props.selected = Some(attributes.contains_key("selected"));
            props.disabled = Some(attributes.contains_key("disabled"));

            // Override with a11y form state (runtime values from what user actually typed/selected)
            // This is critical: HTML "value" attribute only has initial value, but a11y tree
            // has the current runtime value after user interaction
            let lookup_keys = [
                // Try placeholder (common for inputs)
                props.placeholder.clone().map(|p| p.to_lowercase()),
                // Try aria-label
                props.aria_label.clone().map(|a| a.to_lowercase()),
                // Try text content
                text_content.clone().map(|t| t.to_lowercase()),
            ];

            for key in lookup_keys.into_iter().flatten() {
                if let Some(a11y_state) = a11y_form_state.get(&key) {
                    // Override with runtime values from a11y tree
                    if a11y_state.value.is_some() {
                        props.value = a11y_state.value.clone();
                    }
                    if a11y_state.checked.is_some() {
                        props.checked = a11y_state.checked;
                    }
                    if a11y_state.selected.is_some() {
                        props.selected = a11y_state.selected;
                    }
                    break; // Found a match, stop looking
                }
            }
        }

        // Parse children
        let mut children_ids = Vec::new();
        let mut children_structural = Vec::new();
        let mut children_content = Vec::new();

        if !is_self_closing && !inner_html.is_empty() {
            // Extract child elements
            let child_elements = Self::extract_child_elements(&inner_html);
            for child_html in child_elements.iter() {
                // Parse child to get tag and attributes for semantic ID generation
                let (child_tag, child_attrs, child_inner, _) = Self::parse_element(child_html);
                let child_text = Self::extract_text_content(&child_inner);

                // Generate semantic ID based on child's content, not positional index
                // This prevents cascading diffs when elements are inserted/reordered
                let child_id = Self::generate_semantic_id_for_dom(
                    &child_tag,
                    &child_attrs,
                    child_text.as_deref(),
                    &node_id,
                    seen_fingerprints,
                );

                let (struct_hash, content_hash) = Self::build_from_dom_recursive(
                    child_html,
                    nodes,
                    stats,
                    child_id.clone(),
                    Some(node_id.clone()),
                    depth + 1,
                    a11y_form_state,
                    seen_fingerprints,
                );
                children_ids.push(child_id);
                children_structural.push(struct_hash);
                children_content.push(content_hash);
            }
        }

        let is_leaf = children_ids.is_empty();
        if is_leaf {
            stats.leaf_nodes += 1;
        }

        // Compute hashes
        let (structural_hash, content_hash) = Self::compute_node_hashes(
            &tag,
            text_content.as_deref(),
            &props,
            &children_structural,
            &children_content,
        );

        // Create node - store significant attributes for diff
        let significant_attrs: HashMap<String, String> = attributes
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k.as_str(),
                    "aria-expanded"
                        | "aria-hidden"
                        | "aria-selected"
                        | "aria-checked"
                        | "aria-pressed"
                        | "aria-busy"
                        | "aria-disabled"
                        | "aria-label"
                        | "disabled"
                        | "hidden"
                        | "checked"
                        | "selected"
                        | "open"
                        | "class"
                        | "value"
                        | "type"
                        | "href"
                        | "src"
                        | "role"
                )
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let node = MerkleNode {
            node_id: node_id.clone(),
            structural_hash: structural_hash.clone(),
            content_hash: content_hash.clone(),
            node_type: if depth == 0 {
                MerkleNodeType::Root
            } else if is_leaf {
                MerkleNodeType::Element {
                    role: tag.clone(),
                    name: text_content.clone(),
                    selector: Self::generate_css_selector(&tag, &attributes),
                }
            } else {
                MerkleNodeType::Region {
                    description: format!("{} container", tag),
                }
            },
            ax_node_id: None,
            parent_id,
            children: children_ids,
            depth,
            attributes: significant_attrs,
            location: ElementLocation::MainDocument, // DOM snapshot is main doc only
        };

        nodes.insert(node_id, node);
        (structural_hash, content_hash)
    }

    /// Parse an HTML element to extract tag, attributes, and inner HTML
    fn parse_element(html: &str) -> (String, HashMap<String, String>, String, bool) {
        let html = html.trim();

        // Find opening tag
        if !html.starts_with('<') {
            // Text node
            return ("text".to_string(), HashMap::new(), html.to_string(), true);
        }

        // Extract tag name
        let tag_end = html[1..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map(|i| i + 1)
            .unwrap_or(html.len());
        let tag = html[1..tag_end].to_lowercase();

        // Extract attributes
        let mut attributes = HashMap::new();
        let attr_start = tag_end;
        if let Some(tag_close) = html[attr_start..].find('>') {
            let attr_section = &html[attr_start..attr_start + tag_close];

            // Simple attribute parsing
            let mut remaining = attr_section;
            while let Some(eq_pos) = remaining.find('=') {
                let before_eq = remaining[..eq_pos].trim();
                let attr_name = before_eq
                    .rsplit(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_lowercase();

                let after_eq = &remaining[eq_pos + 1..];
                let after_eq = after_eq.trim_start();

                let (value, end_pos) = if after_eq.starts_with('"') {
                    if let Some(end) = after_eq[1..].find('"') {
                        (after_eq[1..end + 1].to_string(), eq_pos + 2 + end + 1)
                    } else {
                        break;
                    }
                } else if after_eq.starts_with('\'') {
                    if let Some(end) = after_eq[1..].find('\'') {
                        (after_eq[1..end + 1].to_string(), eq_pos + 2 + end + 1)
                    } else {
                        break;
                    }
                } else {
                    let end = after_eq
                        .find(|c: char| c.is_whitespace() || c == '>')
                        .unwrap_or(after_eq.len());
                    (after_eq[..end].to_string(), eq_pos + 1 + end)
                };

                if !attr_name.is_empty() {
                    attributes.insert(attr_name, value);
                }

                if end_pos >= remaining.len() {
                    break;
                }
                remaining = &remaining[end_pos..];
            }
        }

        // Check for self-closing
        let is_self_closing = html.contains("/>")
            || matches!(
                tag.as_str(),
                "br" | "hr"
                    | "img"
                    | "input"
                    | "meta"
                    | "link"
                    | "area"
                    | "base"
                    | "col"
                    | "embed"
                    | "source"
                    | "track"
                    | "wbr"
            );

        // Extract inner HTML
        let inner_html = if is_self_closing {
            String::new()
        } else {
            let close_tag = format!("</{}>", tag);
            if let Some(open_end) = html.find('>') {
                if let Some(close_start) = html.rfind(&close_tag) {
                    html[open_end + 1..close_start].to_string()
                } else {
                    html[open_end + 1..].to_string()
                }
            } else {
                String::new()
            }
        };

        (tag, attributes, inner_html, is_self_closing)
    }

    /// Extract child elements from inner HTML
    fn extract_child_elements(inner_html: &str) -> Vec<String> {
        let mut children = Vec::new();
        let mut remaining = inner_html.trim();

        while !remaining.is_empty() {
            if remaining.starts_with('<') {
                // Element node
                if let Some(child_html) = Self::extract_next_element(remaining) {
                    let len = child_html.len();
                    children.push(child_html);
                    remaining = remaining[len..].trim_start();
                } else {
                    break;
                }
            } else {
                // Text node - find next element or end
                let text_end = remaining.find('<').unwrap_or(remaining.len());
                let text = remaining[..text_end].trim();
                if !text.is_empty() {
                    children.push(text.to_string());
                }
                remaining = &remaining[text_end..];
            }
        }

        children
    }

    /// Extract the next complete element from HTML
    fn extract_next_element(html: &str) -> Option<String> {
        if !html.starts_with('<') {
            return None;
        }

        // Get tag name
        let tag_end = html[1..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map(|i| i + 1)?;
        let tag = &html[1..tag_end];

        // Check for comment or doctype
        if tag.starts_with('!') {
            if let Some(end) = html.find('>') {
                return Some(html[..end + 1].to_string());
            }
            return None;
        }

        // Self-closing check
        if html[..html.find('>')? + 1].contains("/>") {
            let end = html.find("/>")? + 2;
            return Some(html[..end].to_string());
        }

        let self_closing_tags = [
            "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "source",
            "track", "wbr",
        ];
        if self_closing_tags.contains(&tag.to_lowercase().as_str()) {
            let end = html.find('>')? + 1;
            return Some(html[..end].to_string());
        }

        // Find matching close tag (handle nesting)
        let close_tag = format!("</{}>", tag.to_lowercase());
        let open_tag_start = format!("<{}", tag.to_lowercase());

        let mut depth = 1;
        let mut pos = html.find('>')? + 1;

        while depth > 0 && pos < html.len() {
            let next_open = html[pos..].find(&open_tag_start).map(|i| pos + i);
            let next_close = html[pos..].find(&close_tag).map(|i| pos + i);

            match (next_open, next_close) {
                (Some(open), Some(close)) if open < close => {
                    // Check if it's actually an opening tag (not just substring match)
                    let after_tag = open + open_tag_start.len();
                    if after_tag < html.len() {
                        let next_char = html.chars().nth(after_tag).unwrap_or(' ');
                        if next_char.is_whitespace() || next_char == '>' || next_char == '/' {
                            depth += 1;
                        }
                    }
                    pos = open + 1;
                },
                (_, Some(close)) => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(html[..close + close_tag.len()].to_string());
                    }
                    pos = close + 1;
                },
                _ => break,
            }
        }

        // Fallback: return up to first >
        html.find('>').map(|end| html[..end + 1].to_string())
    }

    /// Extract text content from HTML (strips tags)
    fn extract_text_content(html: &str) -> Option<String> {
        let mut result = String::new();
        let mut in_tag = false;

        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(c),
                _ => {},
            }
        }

        let trimmed = result.trim().to_string();
        if trimmed.is_empty() || trimmed.len() > 200 {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Generate CSS selector from tag and attributes
    fn generate_css_selector(tag: &str, attrs: &HashMap<String, String>) -> Option<String> {
        // Priority: id > data-testid > data-cy > name > tag
        if let Some(id) = attrs.get("id") {
            if !id.contains("__") && !id.starts_with("ember") && id.parse::<u64>().is_err() {
                return Some(format!("#{}", id));
            }
        }

        if let Some(testid) = attrs.get("data-testid") {
            return Some(format!("[data-testid=\"{}\"]", testid));
        }

        if let Some(cy) = attrs.get("data-cy") {
            return Some(format!("[data-cy=\"{}\"]", cy));
        }

        if let Some(name) = attrs.get("name") {
            if matches!(tag, "input" | "select" | "textarea") {
                return Some(format!("{}[name=\"{}\"]", tag, name));
            }
        }

        None
    }

    /// Merge accessibility tree roles into DOM-based nodes
    fn merge_accessibility_roles(
        nodes: &mut HashMap<String, MerkleNode>,
        a11y_tree: &serde_json::Value,
    ) {
        // Build a map of text content -> role from a11y tree
        let mut role_map: HashMap<String, String> = HashMap::new();
        Self::extract_roles_iterative(a11y_tree, &mut role_map);

        // Update nodes with roles where text matches
        for node in nodes.values_mut() {
            if let MerkleNodeType::Element { role, name, .. } = &mut node.node_type {
                if let Some(n) = name {
                    let normalized = n.to_lowercase();
                    if let Some(a11y_role) = role_map.get(&normalized) {
                        *role = a11y_role.clone();
                    }
                }
            }
        }
    }

    /// Extract text -> role mappings without making page depth native-stack depth.
    fn extract_roles_iterative(root: &serde_json::Value, role_map: &mut HashMap<String, String>) {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if let Some(role) = node["role"]["value"].as_str() {
                if let Some(name) = node["name"]["value"].as_str() {
                    let normalized = name.to_lowercase();
                    if !normalized.is_empty() {
                        role_map.insert(normalized, role.to_string());
                    }
                }
            }

            if let Some(children) = node["children"].as_array() {
                pending.extend(children.iter().rev());
            }
        }
    }

    /// O(1) structural comparison - ignores content changes like typing
    /// Use for recovery polling (waiting for page transition)
    pub fn structure_changed(&self, other: &PageMerkleTree) -> bool {
        self.root_structural_hash != other.root_structural_hash
    }

    /// O(1) full comparison - includes all content changes
    /// Use for validation (did form fill work?)
    pub fn content_changed(&self, other: &PageMerkleTree) -> bool {
        self.root_hash != other.root_hash
    }

    /// Returns (structural_hash, content_hash)
    fn build_iterative(
        root: &serde_json::Value,
        nodes: &mut HashMap<String, MerkleNode>,
        stats: &mut TreeStats,
        root_node_id: String,
        root_parent_id: Option<String>,
        root_depth: usize,
        seen_fingerprints: &mut HashMap<String, usize>,
    ) -> (String, String) {
        enum BuildEvent<'a> {
            Enter {
                node: &'a serde_json::Value,
                node_id: String,
                parent_id: Option<String>,
                depth: usize,
            },
            Exit {
                node: &'a serde_json::Value,
                node_id: String,
                parent_id: Option<String>,
                children_node_ids: Vec<String>,
                depth: usize,
            },
        }

        let mut pending = vec![BuildEvent::Enter {
            node: root,
            node_id: root_node_id.clone(),
            parent_id: root_parent_id,
            depth: root_depth,
        }];

        while let Some(event) = pending.pop() {
            match event {
                BuildEvent::Enter {
                    node,
                    node_id,
                    parent_id,
                    depth,
                } => {
                    stats.total_nodes = stats.total_nodes.saturating_add(1);
                    stats.max_depth = stats.max_depth.max(depth);

                    let children = (depth < MAX_ACCESSIBILITY_TREE_DEPTH)
                        .then(|| node["children"].as_array())
                        .flatten();
                    let children_node_ids = children
                        .map(|children| {
                            children
                                .iter()
                                .enumerate()
                                .map(|(index, child)| {
                                    Self::generate_semantic_id(
                                        child,
                                        &node_id,
                                        index,
                                        seen_fingerprints,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    pending.push(BuildEvent::Exit {
                        node,
                        node_id: node_id.clone(),
                        parent_id,
                        children_node_ids: children_node_ids.clone(),
                        depth,
                    });
                    if let Some(children) = children {
                        for (child, child_id) in children.iter().zip(children_node_ids).rev() {
                            pending.push(BuildEvent::Enter {
                                node: child,
                                node_id: child_id,
                                parent_id: Some(node_id.clone()),
                                depth: depth.saturating_add(1),
                            });
                        }
                    }
                },
                BuildEvent::Exit {
                    node,
                    node_id,
                    parent_id,
                    children_node_ids,
                    depth,
                } => {
                    let role = node["role"]["value"].as_str().unwrap_or("unknown");
                    let name = node["name"]["value"].as_str();
                    let ax_node_id = node["nodeId"].as_str();
                    let props = Self::extract_hashable_properties(node);
                    let children_structural_hashes = children_node_ids
                        .iter()
                        .filter_map(|child_id| {
                            nodes
                                .get(child_id)
                                .map(|child| child.structural_hash.clone())
                        })
                        .collect::<Vec<_>>();
                    let children_content_hashes = children_node_ids
                        .iter()
                        .filter_map(|child_id| {
                            nodes.get(child_id).map(|child| child.content_hash.clone())
                        })
                        .collect::<Vec<_>>();
                    let is_leaf = children_node_ids.is_empty();
                    if is_leaf {
                        stats.leaf_nodes = stats.leaf_nodes.saturating_add(1);
                    }
                    let (structural_hash, content_hash) = Self::compute_node_hashes(
                        role,
                        name,
                        &props,
                        &children_structural_hashes,
                        &children_content_hashes,
                    );
                    let merkle_node = MerkleNode {
                        node_id: node_id.clone(),
                        structural_hash,
                        content_hash,
                        node_type: if depth == root_depth {
                            MerkleNodeType::Root
                        } else if is_leaf {
                            MerkleNodeType::Element {
                                role: role.to_string(),
                                name: name.map(String::from),
                                selector: None,
                            }
                        } else {
                            MerkleNodeType::Region {
                                description: format!("{role} container"),
                            }
                        },
                        ax_node_id: ax_node_id.map(String::from),
                        parent_id,
                        children: children_node_ids,
                        depth,
                        attributes: HashMap::new(),
                        location: ElementLocation::MainDocument,
                    };
                    nodes.insert(node_id, merkle_node);
                },
            }
        }

        let root_node = nodes
            .get(&root_node_id)
            .expect("iterative accessibility build must retain its root");
        (
            root_node.structural_hash.clone(),
            root_node.content_hash.clone(),
        )
    }

    /// Generate a stable semantic ID from node content (survives sibling reordering)
    /// Format: "parent_id/fingerprint_hash" where fingerprint includes stable attributes
    /// Duplicates within same parent get a disambiguation suffix
    fn generate_semantic_id(
        node: &serde_json::Value,
        parent_id: &str,
        _sibling_index: usize, // Kept for signature but not used in fingerprint
        seen_fingerprints: &mut HashMap<String, usize>,
    ) -> String {
        let role = node["role"]["value"].as_str().unwrap_or("unknown");
        let name = node["name"]["value"].as_str().unwrap_or("");

        // Extract stable DOM attributes (id, data-testid) from backend DOM properties
        let dom_id = Self::extract_dom_attribute(node, "id");
        let test_id = Self::extract_dom_attribute(node, "data-testid");

        // Normalize name to reduce hash churn from dynamic content
        let normalized_name = normalize_name(name);
        let name_prefix: String = normalized_name.chars().take(20).collect();

        // Build fingerprint with priority: id > data-testid > role:name
        // Stable DOM attributes produce more reliable fingerprints
        let base_fingerprint = if let Some(id) = &dom_id {
            format!("id:{}", id)
        } else if let Some(tid) = &test_id {
            format!("tid:{}", tid)
        } else {
            format!("{}:{}", role, name_prefix)
        };

        // Use 12-char hash prefix (prevents collisions at 50k+ nodes)
        let hash = blake3::hash(base_fingerprint.as_bytes());
        let short_hash = &hash.to_hex()[..12];

        // Track duplicates within parent and append disambiguation suffix
        let key = format!("{}/{}", parent_id, short_hash);
        let count = seen_fingerprints.entry(key.clone()).or_insert(0);
        *count += 1;

        // First occurrence: no suffix. Subsequent: append .1, .2, etc.
        if *count == 1 {
            key
        } else {
            format!("{}.{}", key, *count - 1)
        }
    }

    /// Generate stable semantic ID for DOM elements (for `build_from_dom_recursive`)
    ///
    /// Similar to `generate_semantic_id` for a11y nodes, but works with parsed HTML attributes.
    /// Priority: id > data-testid > tag:text_content
    ///
    /// This prevents cascading diffs when elements are inserted/reordered.
    fn generate_semantic_id_for_dom(
        tag: &str,
        attributes: &HashMap<String, String>,
        text_content: Option<&str>,
        parent_id: &str,
        seen_fingerprints: &mut HashMap<String, usize>,
    ) -> String {
        // Build fingerprint with priority: id > data-testid > tag:text_content
        let base_fingerprint = if let Some(id) = attributes.get("id") {
            if !id.is_empty() {
                format!("id:{}", id)
            } else {
                Self::build_dom_fingerprint_fallback(tag, attributes, text_content)
            }
        } else if let Some(tid) = attributes.get("data-testid") {
            if !tid.is_empty() {
                format!("tid:{}", tid)
            } else {
                Self::build_dom_fingerprint_fallback(tag, attributes, text_content)
            }
        } else {
            Self::build_dom_fingerprint_fallback(tag, attributes, text_content)
        };

        // Use 12-char hash prefix (prevents collisions at 50k+ nodes)
        let hash = blake3::hash(base_fingerprint.as_bytes());
        let short_hash = &hash.to_hex()[..12];

        // Track duplicates within parent and append disambiguation suffix
        let key = format!("{}/{}", parent_id, short_hash);
        let count = seen_fingerprints.entry(key.clone()).or_insert(0);
        *count += 1;

        // First occurrence: no suffix. Subsequent: append .1, .2, etc.
        if *count == 1 {
            key
        } else {
            format!("{}.{}", key, *count - 1)
        }
    }

    /// Build fallback fingerprint for DOM elements without id or data-testid
    fn build_dom_fingerprint_fallback(
        tag: &str,
        attributes: &HashMap<String, String>,
        text_content: Option<&str>,
    ) -> String {
        // Try aria-label first (good for buttons, links)
        if let Some(label) = attributes.get("aria-label") {
            if !label.is_empty() {
                let normalized = normalize_name(label);
                let prefix: String = normalized.chars().take(20).collect();
                return format!("{}:aria:{}", tag, prefix);
            }
        }

        // Try placeholder (good for inputs)
        if let Some(ph) = attributes.get("placeholder") {
            if !ph.is_empty() {
                let normalized = normalize_name(ph);
                let prefix: String = normalized.chars().take(20).collect();
                return format!("{}:ph:{}", tag, prefix);
            }
        }

        // Try name attribute (good for form elements)
        if let Some(name) = attributes.get("name") {
            if !name.is_empty() {
                return format!("{}:name:{}", tag, name);
            }
        }

        // Try href for links (use path only to avoid query string churn)
        if tag == "a" {
            if let Some(href) = attributes.get("href") {
                if !href.is_empty() && !href.starts_with('#') && !href.starts_with("javascript:") {
                    // Extract path part only
                    let path = href.split('?').next().unwrap_or(href);
                    let prefix: String = path.chars().take(30).collect();
                    return format!("a:href:{}", prefix);
                }
            }
        }

        // Fall back to text content
        if let Some(text) = text_content {
            let normalized = normalize_name(text);
            let prefix: String = normalized.chars().take(20).collect();
            if !prefix.is_empty() {
                return format!("{}:{}", tag, prefix);
            }
        }

        // Last resort: just the tag (will rely on disambiguation suffix)
        tag.to_string()
    }

    /// Extract DOM attribute from accessibility node's backend DOM properties
    fn extract_dom_attribute(node: &serde_json::Value, attr_name: &str) -> Option<String> {
        // CDP provides backendDOMNodeId which can be used to query DOM attributes
        // Check if the attribute is exposed via properties or description
        node["properties"]
            .as_array()
            .and_then(|props| {
                props.iter().find_map(|p| {
                    if p["name"].as_str() == Some(attr_name) {
                        p["value"]["value"].as_str().map(String::from)
                    } else {
                        None
                    }
                })
            })
            .or_else(|| {
                // Fallback: check description for common patterns like "id=foo"
                node["description"]["value"].as_str().and_then(|desc| {
                    let pattern = format!("{}=", attr_name);
                    if let Some(start) = desc.find(&pattern) {
                        let value_start = start + pattern.len();
                        let value_end = desc[value_start..]
                            .find(' ')
                            .map(|i| value_start + i)
                            .unwrap_or(desc.len());
                        Some(desc[value_start..value_end].to_string())
                    } else {
                        None
                    }
                })
            })
    }

    /// Extract HashableProperties from CDP accessibility node properties array
    fn extract_hashable_properties(node: &serde_json::Value) -> HashableProperties {
        let mut props = HashableProperties::default();

        // CDP format: node["properties"] is array of {name, value: {type, value}}
        if let Some(properties) = node["properties"].as_array() {
            for prop in properties {
                let name = prop["name"].as_str().unwrap_or("");
                let value_obj = &prop["value"];

                match name {
                    "value" => props.value = value_obj["value"].as_str().map(String::from),
                    "checked" => props.checked = value_obj["value"].as_bool(),
                    "selected" => props.selected = value_obj["value"].as_bool(),
                    "disabled" => props.disabled = value_obj["value"].as_bool(),
                    "expanded" => props.expanded = value_obj["value"].as_bool(),
                    "pressed" => props.pressed = value_obj["value"].as_bool(),
                    "level" => props.level = value_obj["value"].as_u64().map(|v| v as u32),
                    "posinset" => {
                        props.position_in_set = value_obj["value"].as_u64().map(|v| v as u32)
                    },
                    "hidden" => props.aria_hidden = value_obj["value"].as_bool(),
                    _ => {}, // Ignore other properties
                }
            }
        }

        props
    }

    /// Compute BOTH structural and content hashes (Hash Stratification)
    /// - Structural hash: role + name + children structure (STABLE)
    /// - Content hash: structural + volatile props (changes with content)
    fn compute_node_hashes(
        role: &str,
        name: Option<&str>,
        props: &HashableProperties,
        children_structural: &[String],
        children_content: &[String],
    ) -> (String, String) {
        use blake3::Hasher;

        // === STRUCTURAL HASH: Stable across content changes ===
        let mut struct_hasher = Hasher::new();
        struct_hasher.update(b"role:");
        struct_hasher.update(role.as_bytes());
        if let Some(n) = name {
            // Apply normalize_name to ignore timestamps, dates, counts, prices
            // This prevents dynamic content like "Updated 3:45 PM" from causing structural changes
            let normalized = normalize_name(n);
            if !normalized.is_empty() {
                struct_hasher.update(b"|name:");
                struct_hasher.update(normalized.as_bytes());
            }
        }
        // Children's STRUCTURAL hashes (not content!)
        for child_hash in children_structural {
            struct_hasher.update(b"|child:");
            struct_hasher.update(child_hash.as_bytes());
        }
        let structural_hash = struct_hasher.finalize().to_hex()[..16].to_string();

        // === CONTENT HASH: Includes volatile props ===
        let mut content_hasher = Hasher::new();
        // Start from structural hash
        content_hasher.update(structural_hash.as_bytes());

        // Add NORMALIZED volatile props
        if let Some(v) = &props.value {
            // Normalize: trim whitespace, truncate to 100 chars to reduce churn
            let normalized: String = v.trim().chars().take(100).collect();
            content_hasher.update(b"|value:");
            content_hasher.update(normalized.as_bytes());
        }
        if let Some(v) = props.checked {
            content_hasher.update(b"|checked:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.selected {
            content_hasher.update(b"|selected:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.disabled {
            content_hasher.update(b"|disabled:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.expanded {
            content_hasher.update(b"|expanded:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.pressed {
            content_hasher.update(b"|pressed:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.level {
            content_hasher.update(b"|level:");
            content_hasher.update(v.to_string().as_bytes());
        }
        if let Some(v) = props.position_in_set {
            content_hasher.update(b"|posinset:");
            content_hasher.update(v.to_string().as_bytes());
        }
        if let Some(v) = props.aria_hidden {
            content_hasher.update(b"|hidden:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }

        // Add significant attributes that affect diff detection
        // These must be included to detect attribute-only changes (e.g., class, href, src)
        if !props.class_list.is_empty() {
            content_hasher.update(b"|class:");
            // Sort for deterministic hashing
            let mut sorted_classes = props.class_list.clone();
            sorted_classes.sort();
            content_hasher.update(sorted_classes.join(" ").as_bytes());
        }
        if let Some(v) = &props.href {
            content_hasher.update(b"|href:");
            content_hasher.update(v.as_bytes());
        }
        if let Some(v) = &props.src {
            content_hasher.update(b"|src:");
            content_hasher.update(v.as_bytes());
        }
        if let Some(v) = &props.input_type {
            content_hasher.update(b"|type:");
            content_hasher.update(v.as_bytes());
        }
        if let Some(v) = &props.aria_label {
            content_hasher.update(b"|aria-label:");
            content_hasher.update(v.as_bytes());
        }
        if let Some(v) = props.aria_busy {
            content_hasher.update(b"|aria-busy:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.html_hidden {
            content_hasher.update(b"|html-hidden:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = props.html_open {
            content_hasher.update(b"|html-open:");
            content_hasher.update(if v { b"1" } else { b"0" });
        }
        if let Some(v) = &props.aria_role {
            content_hasher.update(b"|role:");
            content_hasher.update(v.as_bytes());
        }

        // Children's CONTENT hashes (for full comparison)
        for child_hash in children_content {
            content_hasher.update(b"|child:");
            content_hasher.update(child_hash.as_bytes());
        }

        let content_hash = content_hasher.finalize().to_hex()[..16].to_string();

        (structural_hash, content_hash)
    }

    /// Compute diff between two trees
    /// Uses semantic node_ids to compare structure (stable across sibling reordering)
    pub fn diff(&self, other: &PageMerkleTree) -> MerkleDiff {
        // O(1) check: if roots match, trees are identical
        if self.root_hash == other.root_hash {
            return MerkleDiff {
                added: vec![],
                removed: vec![],
                modified: vec![],
                changed_subtrees: vec![],
                summary: "No changes detected".to_string(),
            };
        }

        // Collect node_ids from both trees
        let self_ids: HashSet<_> = self.nodes.keys().collect();
        let other_ids: HashSet<_> = other.nodes.keys().collect();

        // Use LCS-based sibling matching for root's children to detect reorders
        // This handles duplicate elements (e.g., list items) that have same fingerprint
        // but different disambiguation suffixes when reordered
        let (lcs_added, lcs_removed, lcs_matched) = self.diff_children_with_lcs(other);

        // Combine LCS results with global set comparison
        // LCS catches reorders that set comparison misses due to disambiguation suffixes

        // Added: in `other` but not `self`, excluding LCS-matched and LCS-added elements
        // (LCS-added elements will be added via extend, so exclude them to avoid duplicates)
        let mut added: Vec<_> = other_ids
            .difference(&self_ids)
            .filter(|id| !lcs_matched.iter().any(|(_, after)| after == **id))
            .filter(|id| !lcs_added.contains(*id))
            .map(|id| (*id).clone())
            .collect();
        added.extend(lcs_added);

        // Removed: in `self` but not `other`, excluding LCS-matched and LCS-removed elements
        // (LCS-removed elements will be added via extend, so exclude them to avoid duplicates)
        let mut removed: Vec<_> = self_ids
            .difference(&other_ids)
            .filter(|id| !lcs_matched.iter().any(|(before, _)| before == **id))
            .filter(|id| !lcs_removed.contains(*id))
            .map(|id| (*id).clone())
            .collect();
        removed.extend(lcs_removed);

        // Modified: exist in BOTH with different content_hash, OR matched by LCS with content change
        let mut modified: Vec<_> = self_ids
            .intersection(&other_ids)
            .filter_map(|id| {
                let self_node = self.nodes.get(*id)?;
                let other_node = other.nodes.get(*id)?;
                if self_node.content_hash != other_node.content_hash {
                    Some((*id).clone())
                } else {
                    None
                }
            })
            .collect();

        // Add LCS-matched pairs that have content changes
        for (before_id, after_id) in &lcs_matched {
            if before_id != after_id {
                // Different IDs means they were matched by LCS despite disambiguation suffix change
                if let (Some(before_node), Some(after_node)) =
                    (self.nodes.get(before_id), other.nodes.get(after_id))
                {
                    if before_node.content_hash != after_node.content_hash
                        && !modified.contains(after_id)
                    {
                        modified.push(after_id.clone());
                    }
                }
            }
        }

        // Find changed subtree roots (highest-level nodes that changed)
        let changed_subtrees = self.find_changed_subtree_roots(other, &added, &removed, &modified);

        let summary = format!(
            "{} added, {} removed, {} modified, {} subtree roots",
            added.len(),
            removed.len(),
            modified.len(),
            changed_subtrees.len()
        );

        MerkleDiff {
            added,
            removed,
            modified,
            changed_subtrees,
            summary,
        }
    }

    /// Use LCS-based matching for children of nodes that exist in both trees
    /// Returns (added, removed, matched) where matched pairs have potentially different IDs
    /// (due to disambiguation suffix changes from reordering)
    fn diff_children_with_lcs(
        &self,
        other: &PageMerkleTree,
    ) -> (Vec<String>, Vec<String>, Vec<(String, String)>) {
        let mut all_added = Vec::new();
        let mut all_removed = Vec::new();
        let mut all_matched = Vec::new();

        // Find parent nodes that exist in both trees
        let common_parents: Vec<_> = self
            .nodes
            .keys()
            .filter(|id| other.nodes.contains_key(*id))
            .collect();

        for parent_id in common_parents {
            let self_node = match self.nodes.get(parent_id) {
                Some(n) => n,
                None => continue,
            };
            let other_node = match other.nodes.get(parent_id) {
                Some(n) => n,
                None => continue,
            };

            // Skip if children are identical
            if self_node.children == other_node.children {
                continue;
            }

            // Use LCS-based sibling matching for this parent's children
            let result = diff_siblings(&self_node.children, &other_node.children);

            // Collect results, avoiding duplicates
            for id in result.added {
                if !all_added.contains(&id) {
                    all_added.push(id);
                }
            }
            for id in result.removed {
                if !all_removed.contains(&id) {
                    all_removed.push(id);
                }
            }
            for pair in result.matched {
                if !all_matched.contains(&pair) {
                    all_matched.push(pair);
                }
            }
        }

        (all_added, all_removed, all_matched)
    }

    fn find_changed_subtree_roots(
        &self,
        other: &PageMerkleTree,
        added: &[String],
        removed: &[String],
        modified: &[String],
    ) -> Vec<ChangedSubtree> {
        let mut subtrees = Vec::new();

        // Find highest-level added nodes (parent is NOT also added)
        for node_id in added {
            if let Some(node) = other.nodes.get(node_id) {
                // Check if parent is also added - if so, not a subtree root
                let parent_also_added = node
                    .parent_id
                    .as_ref()
                    .map(|pid| added.contains(pid))
                    .unwrap_or(false);

                if !parent_also_added {
                    let (path, depth, sibling_context, parent_role) =
                        Self::build_rich_path(other, node_id);
                    subtrees.push(ChangedSubtree {
                        path,
                        depth,
                        sibling_context,
                        parent_role,
                        subtree_size: Self::count_subtree_nodes(other, node_id),
                        change_type: SubtreeChangeType::Added,
                        affected_elements: Self::describe_subtree(other, node_id),
                    });
                }
            }
        }

        // Find highest-level removed nodes (parent is NOT also removed)
        for node_id in removed {
            if let Some(node) = self.nodes.get(node_id) {
                let parent_also_removed = node
                    .parent_id
                    .as_ref()
                    .map(|pid| removed.contains(pid))
                    .unwrap_or(false);

                if !parent_also_removed {
                    let (path, depth, sibling_context, parent_role) =
                        Self::build_rich_path(self, node_id);
                    subtrees.push(ChangedSubtree {
                        path,
                        depth,
                        sibling_context,
                        parent_role,
                        subtree_size: Self::count_subtree_nodes(self, node_id),
                        change_type: SubtreeChangeType::Removed,
                        affected_elements: Self::describe_subtree(self, node_id),
                    });
                }
            }
        }

        // Find highest-level modified nodes (parent is NOT also modified)
        // Modified = same semantic identity, different content hash (value changed, etc.)
        for node_id in modified {
            if let Some(node) = other.nodes.get(node_id) {
                let parent_also_modified = node
                    .parent_id
                    .as_ref()
                    .map(|pid| modified.contains(pid))
                    .unwrap_or(false);

                if !parent_also_modified {
                    let (path, depth, sibling_context, parent_role) =
                        Self::build_rich_path(other, node_id);
                    subtrees.push(ChangedSubtree {
                        path,
                        depth,
                        sibling_context,
                        parent_role,
                        subtree_size: Self::count_subtree_nodes(other, node_id),
                        change_type: SubtreeChangeType::Modified,
                        affected_elements: Self::describe_subtree(other, node_id),
                    });
                }
            }
        }

        subtrees
    }

    /// Build human-readable path with rich context
    fn build_rich_path(
        tree: &PageMerkleTree,
        node_id: &str,
    ) -> (String, usize, Option<String>, Option<String>) {
        let mut path_parts = Vec::new();
        let mut current_id = Some(node_id.to_string());
        let mut depth = 0;
        let mut sibling_context = None;
        let mut parent_role = None;

        while let Some(id) = current_id {
            if let Some(node) = tree.nodes.get(&id) {
                depth = node.depth;

                let part = match &node.node_type {
                    MerkleNodeType::Element { role, name, .. } => {
                        // Capture parent role for context
                        if parent_role.is_none() && path_parts.len() == 1 {
                            parent_role = Some(role.clone());
                        }
                        name.clone().unwrap_or_else(|| role.clone())
                    },
                    MerkleNodeType::Region { description } => {
                        if parent_role.is_none() && path_parts.len() == 1 {
                            parent_role = Some(description.clone());
                        }
                        description.clone()
                    },
                    MerkleNodeType::Root => "Root".to_string(),
                };

                // Compute sibling context on first iteration (the changed node)
                if path_parts.is_empty() {
                    if let Some(parent_id) = &node.parent_id {
                        if let Some(parent) = tree.nodes.get(parent_id) {
                            let sibling_count = parent.children.len();
                            if sibling_count > 1 {
                                let position = parent
                                    .children
                                    .iter()
                                    .position(|c| c == &id)
                                    .map(|p| p + 1)
                                    .unwrap_or(0);
                                sibling_context = Some(format!(
                                    "item {} of {} in {}",
                                    position,
                                    sibling_count,
                                    match &parent.node_type {
                                        MerkleNodeType::Element { role, .. } => role.as_str(),
                                        MerkleNodeType::Region { description } =>
                                            description.as_str(),
                                        _ => "container",
                                    }
                                ));
                            }
                        }
                    }
                }

                path_parts.push(part);
                current_id = node.parent_id.clone();
            } else {
                break;
            }
        }

        path_parts.reverse();

        // Smart truncation: keep first 2 + last 2 for context
        let path = if path_parts.len() > 5 {
            let first_two = &path_parts[..2];
            let last_two = &path_parts[path_parts.len() - 2..];
            format!("{} > ... > {}", first_two.join(" > "), last_two.join(" > "))
        } else {
            path_parts.join(" > ")
        };

        (path, depth, sibling_context, parent_role)
    }

    fn count_subtree_nodes(tree: &PageMerkleTree, node_id: &str) -> usize {
        let mut count = 1;
        if let Some(node) = tree.nodes.get(node_id) {
            for child_id in &node.children {
                count += Self::count_subtree_nodes(tree, child_id);
            }
        }
        count
    }

    fn describe_subtree(tree: &PageMerkleTree, node_id: &str) -> Vec<String> {
        let mut descriptions = Vec::new();
        if let Some(node) = tree.nodes.get(node_id) {
            match &node.node_type {
                MerkleNodeType::Element { role, name, .. } => {
                    descriptions.push(format!(
                        "{}: {}",
                        role,
                        name.as_deref().unwrap_or("unnamed")
                    ));
                },
                MerkleNodeType::Region { description } => {
                    descriptions.push(description.clone());
                    // Recursively get first few children
                    for child_id in node.children.iter().take(3) {
                        descriptions.extend(Self::describe_subtree(tree, child_id));
                    }
                    if node.children.len() > 3 {
                        descriptions.push(format!("...and {} more", node.children.len() - 3));
                    }
                },
                _ => {},
            }
        }
        descriptions
    }

    /// Extract the base fingerprint from a semantic node_id (strip disambiguation suffix)
    /// "0/a1b2c3d4.2" -> "0/a1b2c3d4"
    pub fn extract_base_fingerprint(node_id: &str) -> String {
        // Find last '.' that's after the last '/' and strip it
        if let Some(last_slash) = node_id.rfind('/') {
            let suffix = &node_id[last_slash..];
            if let Some(dot_pos) = suffix.rfind('.') {
                // Check if what follows the dot is a number (disambiguation suffix)
                let after_dot = &suffix[dot_pos + 1..];
                if after_dot.chars().all(|c| c.is_ascii_digit()) {
                    return node_id[..last_slash + dot_pos].to_string();
                }
            }
        }
        node_id.to_string()
    }
}

// ============================================================================
// LCS-Based Sibling Matching
// ============================================================================

/// Threshold for LCS matching - prevents O(n*m) memory on very large sibling lists
const LCS_SIBLING_THRESHOLD: usize = 1000;

/// Result of sibling matching
#[derive(Debug)]
pub struct SiblingMatchResult {
    pub matched: Vec<(String, String)>, // (before_id, after_id) pairs
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Smart sibling diff with automatic fallback for large lists
/// Uses LCS for small/medium lists, simple comparison for very large lists
pub fn diff_siblings(before_ids: &[String], after_ids: &[String]) -> SiblingMatchResult {
    if before_ids.len() > LCS_SIBLING_THRESHOLD || after_ids.len() > LCS_SIBLING_THRESHOLD {
        // Fall back to simple comparison for very large sibling lists
        // This avoids O(n*m) memory allocation that could cause OOM
        warn!(
            "[MERKLE] Large sibling list ({} x {}), using simple diff instead of LCS",
            before_ids.len(),
            after_ids.len()
        );
        simple_sibling_diff(before_ids, after_ids)
    } else {
        match_siblings_lcs(before_ids, after_ids)
    }
}

/// Simple sibling diff using HashSet (O(n+m) memory, may have false positives on reorder)
fn simple_sibling_diff(before_ids: &[String], after_ids: &[String]) -> SiblingMatchResult {
    let before_set: HashSet<_> = before_ids.iter().collect();
    let after_set: HashSet<_> = after_ids.iter().collect();

    let added: Vec<_> = after_ids
        .iter()
        .filter(|id| !before_set.contains(id))
        .cloned()
        .collect();

    let removed: Vec<_> = before_ids
        .iter()
        .filter(|id| !after_set.contains(id))
        .cloned()
        .collect();

    // For simple diff, matched = intersection (but we lose pairing info)
    let matched: Vec<_> = before_ids
        .iter()
        .filter(|id| after_set.contains(id))
        .map(|id| (id.clone(), id.clone()))
        .collect();

    SiblingMatchResult {
        matched,
        added,
        removed,
    }
}

/// LCS-based sibling matching for handling duplicate element reordering
/// Used when simple ID comparison produces noisy diffs for identical siblings
pub fn match_siblings_lcs(before_ids: &[String], after_ids: &[String]) -> SiblingMatchResult {
    // Extract base fingerprints (without disambiguation suffix)
    let before_fps: Vec<String> = before_ids
        .iter()
        .map(|id| PageMerkleTree::extract_base_fingerprint(id))
        .collect();
    let after_fps: Vec<String> = after_ids
        .iter()
        .map(|id| PageMerkleTree::extract_base_fingerprint(id))
        .collect();

    // LCS to find stable matches
    let lcs = longest_common_subsequence(&before_fps, &after_fps);

    // Build match result
    let mut matched = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();

    let mut before_matched: HashSet<usize> = HashSet::new();
    let mut after_matched: HashSet<usize> = HashSet::new();

    // Mark LCS matches
    for (before_idx, after_idx) in lcs {
        matched.push((before_ids[before_idx].clone(), after_ids[after_idx].clone()));
        before_matched.insert(before_idx);
        after_matched.insert(after_idx);
    }

    // Unmatched in before = removed
    for (idx, id) in before_ids.iter().enumerate() {
        if !before_matched.contains(&idx) {
            removed.push(id.clone());
        }
    }

    // Unmatched in after = added
    for (idx, id) in after_ids.iter().enumerate() {
        if !after_matched.contains(&idx) {
            added.push(id.clone());
        }
    }

    SiblingMatchResult {
        matched,
        added,
        removed,
    }
}

/// Standard LCS algorithm returning matched indices
fn longest_common_subsequence(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let m = a.len();
    let n = b.len();

    if m == 0 || n == 0 {
        return vec![];
    }

    let mut dp = vec![vec![0; n + 1]; m + 1];

    // Build DP table
    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find matches
    let mut matches = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            matches.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    matches.reverse();
    matches
}

// ============================================================================
// Tests
// ============================================================================
// Phase 3: LLM Context Optimization
// ============================================================================

impl MerkleDiff {
    /// Generate human-readable diff description for LLM prompts.
    /// Produces concise, actionable descriptions of what changed.
    ///
    /// Example output:
    /// ```text
    /// Page Changes:
    /// - ADDED: "Submit" button appeared in form section (1 element)
    /// - REMOVED: "Loading" spinner disappeared from header
    /// - MODIFIED: Input field "Email" content changed in login form
    /// ```
    pub fn to_llm_description(&self) -> String {
        if self.changed_subtrees.is_empty() {
            return "No significant changes detected.".to_string();
        }

        let mut lines = vec!["Page Changes:".to_string()];

        for subtree in &self.changed_subtrees {
            let change_verb = match subtree.change_type {
                SubtreeChangeType::Added => "ADDED",
                SubtreeChangeType::Removed => "REMOVED",
                SubtreeChangeType::Modified => "MODIFIED",
            };

            // Build context string
            let context = match (&subtree.parent_role, &subtree.sibling_context) {
                (Some(parent), Some(sibling)) => format!(" in {} ({})", parent, sibling),
                (Some(parent), None) => format!(" in {}", parent),
                (None, Some(sibling)) => format!(" ({})", sibling),
                (None, None) => String::new(),
            };

            // Build element description
            let elements_desc = if subtree.affected_elements.is_empty() {
                subtree.path.clone()
            } else if subtree.affected_elements.len() == 1 {
                subtree.affected_elements[0].clone()
            } else {
                format!(
                    "{} and {} more",
                    subtree.affected_elements[0],
                    subtree.affected_elements.len() - 1
                )
            };

            // Size context for significance
            let size_hint = if subtree.subtree_size > 10 {
                format!(" ({} elements)", subtree.subtree_size)
            } else if subtree.subtree_size > 1 {
                format!(" ({} elements)", subtree.subtree_size)
            } else {
                String::new()
            };

            lines.push(format!(
                "- {}: \"{}\"{}{}",
                change_verb, elements_desc, context, size_hint
            ));
        }

        // Add summary stats if many changes
        if self.added.len() + self.removed.len() + self.modified.len() > 5 {
            lines.push(format!(
                "\nSummary: {} added, {} removed, {} modified",
                self.added.len(),
                self.removed.len(),
                self.modified.len()
            ));
        }

        lines.join("\n")
    }

    /// Serialize diff to compact JSON for token-efficient LLM context.
    /// Only includes essential information, no full tree data.
    ///
    /// Typical reduction: ~2000 tokens (full page) → ~200 tokens (diff only)
    pub fn to_compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "has_changes": !self.changed_subtrees.is_empty(),
            "counts": {
                "added": self.added.len(),
                "removed": self.removed.len(),
                "modified": self.modified.len()
            },
            "changes": self.changed_subtrees.iter().map(|s| {
                serde_json::json!({
                    "type": match s.change_type {
                        SubtreeChangeType::Added => "added",
                        SubtreeChangeType::Removed => "removed",
                        SubtreeChangeType::Modified => "modified",
                    },
                    "path": s.path,
                    "depth": s.depth,
                    "size": s.subtree_size,
                    "context": s.parent_role,
                    "elements": s.affected_elements.iter().take(3).collect::<Vec<_>>()
                })
            }).collect::<Vec<_>>()
        })
    }

    /// Check if any structural changes occurred (elements added/removed).
    pub fn has_structural_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }

    /// Check if only content changes occurred (no structural changes).
    pub fn is_content_only(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && !self.modified.is_empty()
    }

    /// Get the most significant change for quick summary.
    pub fn primary_change(&self) -> Option<&ChangedSubtree> {
        // Prioritize: largest subtree, then added > removed > modified
        self.changed_subtrees.iter().max_by_key(|s| {
            let type_priority = match s.change_type {
                SubtreeChangeType::Added => 3,
                SubtreeChangeType::Removed => 2,
                SubtreeChangeType::Modified => 1,
            };
            (type_priority, s.subtree_size)
        })
    }
}

// ============================================================================
// DOM Attribute Extraction (Phase 3)
// ============================================================================

impl HashableProperties {
    /// Extract properties from an accessibility tree node.
    /// Handles both Chrome DevTools Protocol and custom formats.
    /// Note: role and name are extracted separately in MerkleNodeType.
    pub fn from_node(node: &serde_json::Value) -> Self {
        let mut props = HashableProperties::default();

        // Extract description
        props.description = node
            .get("description")
            .and_then(|d| d.get("value").or(Some(d)))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract properties array (Chrome DevTools Protocol format)
        if let Some(properties) = node.get("properties").and_then(|p| p.as_array()) {
            for prop in properties {
                let prop_name = prop.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let prop_value = prop.get("value");

                match prop_name {
                    "checked" => {
                        props.checked = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    "selected" => {
                        props.selected = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    "expanded" => {
                        props.expanded = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    "disabled" => {
                        props.disabled = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    "required" => {
                        props.required = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    "invalid" => {
                        props.invalid = prop_value
                            .and_then(|v| v.get("value").or(Some(v)))
                            .and_then(|v| v.as_bool());
                    },
                    _ => {},
                }
            }
        }

        // Extract backend DOM node properties if available
        if let Some(backend) = node.get("backendDOMNodeId") {
            // Backend DOM node ID present - could fetch DOM properties via CDP
            debug!("Backend DOM node available: {:?}", backend);
        }

        // Extract value (for inputs)
        props.value = node
            .get("value")
            .and_then(|v| v.get("value").or(Some(v)))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract from DOM attributes if present (custom format)
        if let Some(attrs) = node.get("domAttributes").and_then(|a| a.as_object()) {
            props.html_id = attrs
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.class_list = attrs
                .get("class")
                .and_then(|v| v.as_str())
                .map(|s| s.split_whitespace().map(|c| c.to_string()).collect())
                .unwrap_or_default();
            props.href = attrs
                .get("href")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.src = attrs
                .get("src")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.placeholder = attrs
                .get("placeholder")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.input_type = attrs
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.tag_name = attrs
                .get("tagName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // ARIA attributes
            props.aria_label = attrs
                .get("aria-label")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.aria_describedby = attrs
                .get("aria-describedby")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.aria_controls = attrs
                .get("aria-controls")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            props.aria_live = attrs
                .get("aria-live")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        // Extract bounding box if present
        if let Some(bounds) = node.get("boundingBox").and_then(|b| b.as_object()) {
            props.bounds = Some(MerkleBoundingBox {
                x: bounds.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                y: bounds.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
                width: bounds.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0),
                height: bounds.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0),
            });
        }

        props
    }

    /// Compute structural hash from DOM properties (stable across content changes).
    /// Note: Role is passed separately since it's in MerkleNodeType, not HashableProperties.
    pub fn structural_hash_with_role(&self, role: &str) -> String {
        use blake3::Hasher;
        let mut hasher = Hasher::new();

        hasher.update(role.as_bytes());
        if let Some(ref tag) = self.tag_name {
            hasher.update(tag.as_bytes());
        }
        if let Some(ref id) = self.html_id {
            hasher.update(id.as_bytes());
        }
        // Include sorted class list for stability
        let mut classes = self.class_list.clone();
        classes.sort();
        for class in &classes {
            hasher.update(class.as_bytes());
        }
        // Include href/src patterns (not full URLs to avoid query param churn)
        if let Some(ref href) = self.href {
            hasher.update(extract_url_pattern(href).as_bytes());
        }
        if let Some(ref src) = self.src {
            hasher.update(extract_url_pattern(src).as_bytes());
        }

        let hash = hasher.finalize();
        hex::encode(&hash.as_bytes()[..6]) // 12-char hex (48 bits)
    }

    /// Compute content hash from all properties.
    /// Note: Role and name are passed separately since they're in MerkleNodeType.
    pub fn content_hash_with_context(&self, role: &str, name: Option<&str>) -> String {
        use blake3::Hasher;
        let mut hasher = Hasher::new();

        // Include structural components
        hasher.update(self.structural_hash_with_role(role).as_bytes());

        // Add content-specific properties
        if let Some(name) = name {
            hasher.update(normalize_name(name).as_bytes());
        }
        if let Some(ref value) = self.value {
            hasher.update(value.as_bytes());
        }
        if let Some(ref value_text) = self.value_text {
            hasher.update(value_text.as_bytes());
        }

        // State booleans
        if let Some(checked) = self.checked {
            hasher.update(&[checked as u8]);
        }
        if let Some(selected) = self.selected {
            hasher.update(&[selected as u8]);
        }
        if let Some(expanded) = self.expanded {
            hasher.update(&[expanded as u8]);
        }
        if let Some(disabled) = self.disabled {
            hasher.update(&[disabled as u8]);
        }
        if let Some(invalid) = self.invalid {
            hasher.update(&[invalid as u8]);
        }

        let hash = hasher.finalize();
        hex::encode(&hash.as_bytes()[..6])
    }

    /// Generate human-readable description for LLM context.
    /// Note: Role and name are passed separately for context.
    pub fn to_description_with_context(&self, role: &str, name: Option<&str>) -> String {
        let mut parts = Vec::new();

        // Role and name
        if let Some(name) = name {
            parts.push(format!("{}: \"{}\"", role, name));
        } else {
            parts.push(role.to_string());
        }

        // State indicators
        let mut states = Vec::new();
        if self.checked == Some(true) {
            states.push("checked");
        }
        if self.selected == Some(true) {
            states.push("selected");
        }
        if self.expanded == Some(true) {
            states.push("expanded");
        }
        if self.disabled == Some(true) {
            states.push("disabled");
        }
        if self.required == Some(true) {
            states.push("required");
        }
        if self.invalid == Some(true) {
            states.push("invalid");
        }
        if !states.is_empty() {
            parts.push(format!("[{}]", states.join(", ")));
        }

        // Value if present
        if let Some(ref value) = self.value {
            if !value.is_empty() {
                let truncated = if value.chars().count() > 30 {
                    // Find safe truncation point at character boundary
                    let end_idx = value
                        .char_indices()
                        .nth(27)
                        .map(|(i, _)| i)
                        .unwrap_or(value.len());
                    format!("{}...", &value[..end_idx])
                } else {
                    value.clone()
                };
                parts.push(format!("value=\"{}\"", truncated));
            }
        }

        parts.join(" ")
    }
}

/// Extract URL pattern (path without query params) for stable hashing.
fn extract_url_pattern(url: &str) -> String {
    // Remove query params and fragments
    url.split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url)
        .to_string()
}

// ============================================================================
// CDP DOM Tree Structures (Phase 2: CDP-Based Comprehensive Merkle)
// ============================================================================

/// Wrapper for the comprehensive DOM capture response from extension.
///
/// This matches the output of `captureCompleteDomTree()` in observe.js which
/// wraps the DOM tree with stats, hash, and timing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComprehensiveDomCapture {
    /// The actual CDP DOM tree
    pub tree: CdpDomNode,

    /// Capture statistics (node counts, shadow roots, iframes)
    #[serde(default)]
    pub stats: CdpCaptureStats,

    /// Hash of the tree for quick comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,

    /// Capture timing information
    #[serde(default)]
    pub timing: CdpCaptureTiming,

    /// Unix timestamp when captured
    #[serde(default, alias = "captured_at")]
    pub captured_at: u64,
}

/// Statistics from CDP DOM capture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpCaptureStats {
    /// Total node count
    #[serde(default)]
    pub total: usize,

    /// Number of element nodes
    #[serde(default)]
    pub elements: usize,

    /// Number of text nodes
    #[serde(default)]
    pub text_nodes: usize,

    /// Number of shadow roots captured
    #[serde(default)]
    pub shadow_roots: usize,

    /// Number of iframe documents captured
    #[serde(default)]
    pub iframe_documents: usize,

    /// Number of cross-origin iframe documents
    /// (iframe documentURL origin differs from main document origin)
    #[serde(default)]
    pub cross_origin_iframes: usize,

    /// Maximum depth reached
    #[serde(default)]
    pub max_depth: usize,
}

/// Timing information from CDP DOM capture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpCaptureTiming {
    /// Time to call DOM.getDocument (ms)
    #[serde(default)]
    pub cdp_get_document_ms: u64,

    /// Time to serialize the tree (ms)
    #[serde(default)]
    pub serialize_ms: u64,

    /// Time to compute hash (ms)
    #[serde(default)]
    pub hash_ms: u64,

    /// Total capture time (ms)
    #[serde(default)]
    pub total_ms: u64,
}

/// CDP DOM node structure from extension's `captureCompleteDomTree()`.
///
/// This matches the output of `serializeDomNode()` in observe.js which uses
/// CDP `DOM.getDocument` with `depth: -1, pierce: true` for 100% coverage
/// including cross-origin iframes and closed shadow DOM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpDomNode {
    /// Node type (1=Element, 3=Text, 8=Comment, 9=Document, 10=DocumentType)
    pub node_type: u8,

    /// Node name (e.g., "DIV", "#text", "#document")
    pub node_name: String,

    /// CDP node ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u64>,

    /// Backend node ID (stable across frames)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<u64>,

    /// Node value for text/comment nodes (truncated to 500 chars by extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_value: Option<String>,

    /// Attributes as key-value map (converted from CDP's flat array format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<HashMap<String, String>>,

    /// Child nodes (regular DOM children)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<CdpDomNode>>,

    /// Shadow roots (CDP captures both open AND closed shadow roots!)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_roots: Option<Vec<CdpDomNode>>,

    /// Iframe content document (CDP bypasses same-origin policy!)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_document: Option<Box<CdpDomNode>>,

    /// Template content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_content: Option<Box<CdpDomNode>>,

    /// Pseudo elements (::before, ::after, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pseudo_elements: Option<Vec<CdpDomNode>>,

    /// Document URL (for document nodes, used for cross-origin detection)
    /// Note: CDP uses "documentURL" (capital URL), not "documentUrl" (camelCase)
    #[serde(skip_serializing_if = "Option::is_none", rename = "documentURL")]
    pub document_url: Option<String>,

    /// Base URL (for document nodes)
    /// Note: CDP uses "baseURL" (capital URL), not "baseUrl" (camelCase)
    #[serde(skip_serializing_if = "Option::is_none", rename = "baseURL")]
    pub base_url: Option<String>,

    /// Frame ID (for iframe content documents, from CDP frameId)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

/// Location of an element in the document structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ElementLocation {
    /// In main document
    #[default]
    MainDocument,
    /// Inside a shadow root
    ShadowRoot,
    /// Inside an iframe
    IframeDocument,
    /// Inside a cross-origin iframe (CDP-only access)
    CrossOriginIframe,
}

// ============================================================================
// Rich Merkle Diff Structures (Phase 3: MutationObserver-style output)
// ============================================================================

/// Rich diff output that replaces MutationObserver.
/// Provides detailed, structured change information for LLM context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RichMerkleDiff {
    // === Quick Summary ===
    /// Whether any change was detected
    pub changed: bool,

    /// Whether structure changed (new/removed elements)
    pub structural_changed: bool,

    /// Whether content changed (text, attributes)
    pub content_changed: bool,

    // === Node Counts ===
    pub nodes_added: usize,
    pub nodes_removed: usize,
    pub nodes_modified: usize,

    // === Detailed Changes (like MutationObserver output) ===
    /// Elements that were added
    pub added_elements: Vec<RichElementChange>,

    /// Elements that were removed
    pub removed_elements: Vec<RichElementChange>,

    /// Attribute changes on existing elements
    pub attribute_changes: Vec<RichAttributeChange>,

    /// Text content changes
    pub text_changes: Vec<RichTextChange>,

    // === High-Level Signals for LLM ===
    /// Semantic signals (e.g., "popup_opened", "form_submitted", "loading_complete")
    pub signals: Vec<String>,

    // === Scope Info ===
    /// What was covered in this diff
    pub scope: DiffScope,
}

/// Describes an element that was added or removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichElementChange {
    /// Semantic node ID from Merkle tree
    pub node_id: String,

    /// HTML tag name (e.g., "DIV", "BUTTON")
    pub tag_name: String,

    /// Key attributes for identification
    pub attributes: HashMap<String, String>,

    /// Where in the document structure
    pub location: ElementLocation,

    /// Human-readable path for context
    pub path: Option<String>,
}

/// Describes an attribute change on an existing element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichAttributeChange {
    /// Semantic node ID from Merkle tree
    pub node_id: String,

    /// HTML tag name
    pub tag_name: String,

    /// Attribute that changed
    pub attribute: String,

    /// Previous value (None if newly added)
    pub old_value: Option<String>,

    /// New value (None if removed)
    pub new_value: Option<String>,
}

/// Describes a text content change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichTextChange {
    /// Semantic node ID from Merkle tree
    pub node_id: String,

    /// Parent element's tag name
    pub parent_tag: String,

    /// Previous text (truncated)
    pub old_text: Option<String>,

    /// New text (truncated)
    pub new_text: Option<String>,
}

/// Scope of what was covered in the diff.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiffScope {
    /// Main document was included
    pub main_document: bool,

    /// Number of shadow roots captured
    pub shadow_roots_count: usize,

    /// Number of iframe documents captured
    pub iframe_documents_count: usize,

    /// Number of cross-origin iframes (CDP-only)
    pub cross_origin_iframes_count: usize,
}

/// Statistics from CDP DOM tree traversal.
#[derive(Debug, Clone, Default)]
struct CdpTreeStats {
    total_nodes: usize,
    element_nodes: usize,
    text_nodes: usize,
    leaf_nodes: usize,
    shadow_roots: usize,
    iframe_documents: usize,
    cross_origin_iframes: usize,
    max_depth: usize,
    /// Main document URL for cross-origin detection
    main_document_url: Option<String>,
}

impl RichMerkleDiff {
    /// Create a "no changes" diff.
    pub fn no_changes() -> Self {
        Self {
            changed: false,
            structural_changed: false,
            content_changed: false,
            nodes_added: 0,
            nodes_removed: 0,
            nodes_modified: 0,
            added_elements: vec![],
            removed_elements: vec![],
            attribute_changes: vec![],
            text_changes: vec![],
            signals: vec![],
            scope: DiffScope::default(),
        }
    }

    /// Generate human-readable description for LLM.
    pub fn to_llm_description(&self) -> String {
        if !self.changed {
            return "No changes detected.".to_string();
        }

        let mut lines = vec!["Page Changes:".to_string()];

        // Report signals first (high-level semantic meaning)
        if !self.signals.is_empty() {
            lines.push(format!("Signals: {}", self.signals.join(", ")));
        }

        // Added elements
        for elem in self.added_elements.iter().take(5) {
            let attrs_str = elem
                .attributes
                .iter()
                .take(3)
                .map(|(k, v)| format!("{}=\"{}\"", k, truncate_str(v, 20)))
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!(
                "- ADDED: <{}{}> in {:?}",
                elem.tag_name.to_lowercase(),
                if attrs_str.is_empty() {
                    String::new()
                } else {
                    format!(" {}", attrs_str)
                },
                elem.location
            ));
        }
        if self.added_elements.len() > 5 {
            lines.push(format!(
                "  ...and {} more added",
                self.added_elements.len() - 5
            ));
        }

        // Removed elements
        for elem in self.removed_elements.iter().take(5) {
            lines.push(format!(
                "- REMOVED: <{}> from {:?}",
                elem.tag_name.to_lowercase(),
                elem.location
            ));
        }
        if self.removed_elements.len() > 5 {
            lines.push(format!(
                "  ...and {} more removed",
                self.removed_elements.len() - 5
            ));
        }

        // Attribute changes
        for attr in self.attribute_changes.iter().take(5) {
            let change_desc = match (&attr.old_value, &attr.new_value) {
                (Some(old), Some(new)) => format!(
                    "\"{}\" → \"{}\"",
                    truncate_str(old, 15),
                    truncate_str(new, 15)
                ),
                (None, Some(new)) => format!("added \"{}\"", truncate_str(new, 20)),
                (Some(old), None) => format!("removed \"{}\"", truncate_str(old, 20)),
                (None, None) => "changed".to_string(),
            };
            lines.push(format!(
                "- ATTR: <{}>.{} {}",
                attr.tag_name.to_lowercase(),
                attr.attribute,
                change_desc
            ));
        }
        if self.attribute_changes.len() > 5 {
            lines.push(format!(
                "  ...and {} more attribute changes",
                self.attribute_changes.len() - 5
            ));
        }

        // Text changes
        if !self.text_changes.is_empty() {
            lines.push(format!(
                "- TEXT: {} text nodes changed",
                self.text_changes.len()
            ));
        }

        // Summary
        lines.push(format!(
            "\nTotal: {} added, {} removed, {} modified | Scope: {} shadow roots, {} iframes",
            self.nodes_added,
            self.nodes_removed,
            self.nodes_modified,
            self.scope.shadow_roots_count,
            self.scope.iframe_documents_count
        ));

        lines.join("\n")
    }

    /// Generate compact JSON for token-efficient LLM context.
    pub fn to_compact_json(&self) -> serde_json::Value {
        serde_json::json!({
            "changed": self.changed,
            "signals": self.signals,
            "counts": {
                "added": self.nodes_added,
                "removed": self.nodes_removed,
                "modified": self.nodes_modified
            },
            "scope": {
                "shadows": self.scope.shadow_roots_count,
                "iframes": self.scope.iframe_documents_count,
                "crossOrigin": self.scope.cross_origin_iframes_count
            }
        })
    }
}

/// Truncate string with ellipsis (UTF-8 safe).
/// Uses char boundaries to avoid panicking on multi-byte characters.
fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

// ============================================================================
// CDP DOM Tree Builder (Phase 2)
// ============================================================================

impl PageMerkleTree {
    /// Build Merkle tree from CDP DOM tree (comprehensive 100% coverage).
    ///
    /// This captures EVERYTHING including:
    /// - Cross-origin iframes (CDP bypasses same-origin policy)
    /// - Closed shadow DOM (CDP can access)
    /// - All nested content (recursive traversal)
    ///
    /// Use this instead of `from_dom_snapshot()` when you need complete coverage.
    pub fn from_cdp_dom_tree(
        cdp_root: &CdpDomNode,
        url: Option<&str>,
        page_stage: PageStage,
    ) -> Self {
        let mut nodes = HashMap::new();
        let mut stats = CdpTreeStats {
            // Initialize main_document_url from root's documentURL or passed url
            main_document_url: cdp_root
                .document_url
                .clone()
                .or_else(|| url.map(String::from)),
            ..Default::default()
        };
        let mut seen_fingerprints: HashMap<String, usize> = HashMap::new();

        let root_id = "0".to_string();
        let (root_structural_hash, root_hash) = Self::build_from_cdp_recursive(
            cdp_root,
            &mut nodes,
            &mut stats,
            root_id.clone(),
            None,
            0,
            &mut seen_fingerprints,
            ElementLocation::MainDocument,
        );

        debug!(
            "[MERKLE-CDP] Built tree from CDP DOM: {} nodes ({} elements, {} text), \
             {} shadow roots, {} iframes ({} cross-origin), max_depth={}",
            stats.total_nodes,
            stats.element_nodes,
            stats.text_nodes,
            stats.shadow_roots,
            stats.iframe_documents,
            stats.cross_origin_iframes,
            stats.max_depth
        );

        PageMerkleTree {
            root_id,
            root_structural_hash,
            root_hash,
            nodes,
            captured_at: Utc::now(),
            url: url.map(String::from),
            page_stage,
            node_count: stats.total_nodes,
            leaf_count: stats.leaf_nodes,
            max_depth: stats.max_depth,
            cross_origin_iframe_count: stats.cross_origin_iframes,
        }
    }

    /// Recursively build Merkle tree from CDP DOM node.
    fn build_from_cdp_recursive(
        node: &CdpDomNode,
        nodes: &mut HashMap<String, MerkleNode>,
        stats: &mut CdpTreeStats,
        node_id: String,
        parent_id: Option<String>,
        depth: usize,
        seen_fingerprints: &mut HashMap<String, usize>,
        location: ElementLocation,
    ) -> (String, String) {
        stats.total_nodes += 1;
        stats.max_depth = stats.max_depth.max(depth);

        // Cap depth to prevent stack overflow
        const MAX_DEPTH: usize = 50;
        if depth > MAX_DEPTH {
            let hash = blake3::hash(node.node_name.as_bytes()).to_hex()[..16].to_string();
            let truncated_node = MerkleNode {
                node_id: node_id.clone(),
                structural_hash: hash.clone(),
                content_hash: hash.clone(),
                node_type: MerkleNodeType::Element {
                    role: "truncated".to_string(),
                    name: Some("Depth limit reached".to_string()),
                    selector: None,
                },
                ax_node_id: None,
                parent_id,
                children: vec![],
                depth,
                attributes: HashMap::new(),
                location: location.clone(), // Preserve location context even when truncated
            };
            nodes.insert(node_id, truncated_node);
            return (hash.clone(), hash);
        }

        // Track node types
        match node.node_type {
            1 => stats.element_nodes += 1, // Element
            3 => stats.text_nodes += 1,    // Text
            _ => {},
        }

        // Extract attributes for hashing
        let attrs = node.attributes.clone().unwrap_or_default();
        let tag = node.node_name.to_uppercase();

        // Build hashable properties from CDP attributes
        let mut props = HashableProperties::default();
        props.tag_name = Some(tag.clone());
        props.html_id = attrs.get("id").cloned();
        if let Some(cls) = attrs.get("class") {
            props.class_list = cls.split_whitespace().map(String::from).collect();
        }
        props.aria_label = attrs.get("aria-label").cloned();
        props.placeholder = attrs.get("placeholder").cloned();
        props.input_type = attrs.get("type").cloned();
        props.href = attrs.get("href").cloned();
        props.src = attrs.get("src").cloned();

        // Extract form state from attributes
        if matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT" | "OPTION") {
            props.value = attrs.get("value").cloned();
            props.checked = Some(attrs.contains_key("checked"));
            props.selected = Some(attrs.contains_key("selected"));
            props.disabled = Some(attrs.contains_key("disabled"));
        }

        // ARIA state
        props.expanded = attrs.get("aria-expanded").map(|v| v == "true");
        props.pressed = attrs.get("aria-pressed").map(|v| v == "true");
        props.aria_hidden = attrs.get("aria-hidden").map(|v| v == "true");
        props.aria_busy = attrs.get("aria-busy").map(|v| v == "true");

        // HTML state attributes (not covered by ARIA)
        props.html_hidden = Some(attrs.contains_key("hidden"));
        props.html_open = Some(attrs.contains_key("open"));
        props.aria_role = attrs.get("role").cloned();

        // Process all children (regular, shadow, iframe, template)
        let mut children_ids = Vec::new();
        let mut children_structural = Vec::new();
        let mut children_content = Vec::new();

        // Regular children
        if let Some(children) = &node.children {
            for child in children {
                let child_id = Self::generate_cdp_semantic_id(child, &node_id, seen_fingerprints);
                let (struct_hash, content_hash) = Self::build_from_cdp_recursive(
                    child,
                    nodes,
                    stats,
                    child_id.clone(),
                    Some(node_id.clone()),
                    depth + 1,
                    seen_fingerprints,
                    location.clone(),
                );
                children_ids.push(child_id);
                children_structural.push(struct_hash);
                children_content.push(content_hash);
            }
        }

        // Shadow roots (CDP captures even closed ones!)
        if let Some(shadows) = &node.shadow_roots {
            stats.shadow_roots += shadows.len();
            for (i, shadow) in shadows.iter().enumerate() {
                let shadow_id = format!("{}/shadow:{}", node_id, i);
                let (struct_hash, content_hash) = Self::build_from_cdp_recursive(
                    shadow,
                    nodes,
                    stats,
                    shadow_id.clone(),
                    Some(node_id.clone()),
                    depth + 1,
                    seen_fingerprints,
                    ElementLocation::ShadowRoot,
                );
                children_ids.push(shadow_id);
                children_structural.push(struct_hash);
                children_content.push(content_hash);
            }
        }

        // Iframe content document (CDP bypasses same-origin!)
        if let Some(content_doc) = &node.content_document {
            stats.iframe_documents += 1;
            let doc_id = format!("{}/iframe", node_id);

            // Detect cross-origin by comparing documentURL origins
            let mut is_cross_origin = false;
            if let (Some(main_url), Some(iframe_url)) =
                (&stats.main_document_url, &content_doc.document_url)
            {
                if let (Ok(main_parsed), Ok(iframe_parsed)) =
                    (url::Url::parse(main_url), url::Url::parse(iframe_url))
                {
                    if main_parsed.origin() != iframe_parsed.origin() {
                        is_cross_origin = true;
                        stats.cross_origin_iframes += 1;
                        debug!(
                            "[MERKLE-CDP] Cross-origin iframe detected: {} (main: {})",
                            iframe_parsed.origin().ascii_serialization(),
                            main_parsed.origin().ascii_serialization()
                        );
                    }
                }
            }

            // Use CrossOriginIframe location for cross-origin, IframeDocument for same-origin
            let iframe_location = if is_cross_origin {
                ElementLocation::CrossOriginIframe
            } else {
                ElementLocation::IframeDocument
            };
            let (struct_hash, content_hash) = Self::build_from_cdp_recursive(
                content_doc,
                nodes,
                stats,
                doc_id.clone(),
                Some(node_id.clone()),
                depth + 1,
                seen_fingerprints,
                iframe_location,
            );
            children_ids.push(doc_id);
            children_structural.push(struct_hash);
            children_content.push(content_hash);
        }

        // Template content
        if let Some(template) = &node.template_content {
            let template_id = format!("{}/template", node_id);
            let (struct_hash, content_hash) = Self::build_from_cdp_recursive(
                template,
                nodes,
                stats,
                template_id.clone(),
                Some(node_id.clone()),
                depth + 1,
                seen_fingerprints,
                location.clone(),
            );
            children_ids.push(template_id);
            children_structural.push(struct_hash);
            children_content.push(content_hash);
        }

        // Pseudo elements (::before, ::after, etc.)
        // CDP captures these when using DOM.getDocument with pierce:true
        if let Some(pseudos) = &node.pseudo_elements {
            for pseudo in pseudos.iter() {
                // Use pseudo identifier from node_name (e.g., "::before", "::after")
                let pseudo_type = pseudo.node_name.trim_start_matches(':');
                let pseudo_id = format!("{}/pseudo:{}", node_id, pseudo_type);
                let (struct_hash, content_hash) = Self::build_from_cdp_recursive(
                    pseudo,
                    nodes,
                    stats,
                    pseudo_id.clone(),
                    Some(node_id.clone()),
                    depth + 1,
                    seen_fingerprints,
                    location.clone(),
                );
                children_ids.push(pseudo_id);
                children_structural.push(struct_hash);
                children_content.push(content_hash);
            }
        }

        // Get text content from node_value (for text nodes)
        let text_content = node
            .node_value
            .as_ref()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && v.len() <= 200)
            .map(String::from);

        // Compute hashes
        let role = match node.node_type {
            1 => tag.to_lowercase(), // Element
            3 => "text".to_string(), // Text node
            9 => "document".to_string(),
            _ => "other".to_string(),
        };

        let (structural_hash, content_hash) = Self::compute_node_hashes(
            &role,
            text_content.as_deref(),
            &props,
            &children_structural,
            &children_content,
        );

        // Create Merkle node
        let is_leaf = children_ids.is_empty();
        if is_leaf {
            stats.leaf_nodes += 1;
        }

        // Store significant attributes for diff computation
        let significant_attrs: HashMap<String, String> = attrs
            .iter()
            .filter(|(k, _)| {
                // Only store attributes useful for diffing and semantic signal detection
                matches!(
                    k.as_str(),
                    "aria-expanded"
                        | "aria-hidden"
                        | "aria-selected"
                        | "aria-checked"
                        | "aria-pressed"
                        | "aria-busy"
                        | "aria-disabled"
                        | "aria-label"
                        | "disabled"
                        | "hidden"
                        | "checked"
                        | "selected"
                        | "open"
                        | "class"
                        | "value"
                        | "type"
                        | "href"
                        | "src"
                        | "role"
                )
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let merkle_node = MerkleNode {
            node_id: node_id.clone(),
            structural_hash: structural_hash.clone(),
            content_hash: content_hash.clone(),
            node_type: if depth == 0 {
                MerkleNodeType::Root
            } else if is_leaf {
                MerkleNodeType::Element {
                    role: role.clone(),
                    name: text_content.or_else(|| attrs.get("aria-label").cloned()),
                    selector: Self::generate_css_selector(&tag, &attrs),
                }
            } else {
                MerkleNodeType::Region {
                    description: format!("{} container", role),
                }
            },
            ax_node_id: node.backend_node_id.map(|id| id.to_string()),
            parent_id,
            children: children_ids,
            depth,
            attributes: significant_attrs,
            location, // Propagated location (MainDocument, IframeDocument, CrossOriginIframe, ShadowRoot)
        };

        nodes.insert(node_id, merkle_node);
        (structural_hash, content_hash)
    }

    /// Generate semantic ID for CDP DOM node.
    fn generate_cdp_semantic_id(
        node: &CdpDomNode,
        parent_id: &str,
        seen_fingerprints: &mut HashMap<String, usize>,
    ) -> String {
        let attrs = node.attributes.as_ref();
        let tag = &node.node_name;

        // Priority: id > data-testid > tag:text
        let base_fingerprint =
            if let Some(id) = attrs.and_then(|a| a.get("id")).filter(|s| !s.is_empty()) {
                format!("id:{}", id)
            } else if let Some(tid) = attrs
                .and_then(|a| a.get("data-testid"))
                .filter(|s| !s.is_empty())
            {
                format!("tid:{}", tid)
            } else if let Some(text) = &node.node_value {
                let normalized = normalize_name(text);
                let prefix: String = normalized.chars().take(20).collect();
                if !prefix.is_empty() {
                    format!("{}:{}", tag.to_lowercase(), prefix)
                } else {
                    tag.to_lowercase()
                }
            } else {
                // Try aria-label, placeholder, name
                attrs
                    .and_then(|a| {
                        a.get("aria-label")
                            .or_else(|| a.get("placeholder"))
                            .or_else(|| a.get("name"))
                    })
                    .map(|v| {
                        let normalized = normalize_name(v);
                        let prefix: String = normalized.chars().take(20).collect();
                        format!("{}:{}", tag.to_lowercase(), prefix)
                    })
                    .unwrap_or_else(|| tag.to_lowercase())
            };

        // Hash and disambiguate
        let hash = blake3::hash(base_fingerprint.as_bytes());
        let short_hash = &hash.to_hex()[..12];

        let key = format!("{}/{}", parent_id, short_hash);
        let count = seen_fingerprints.entry(key.clone()).or_insert(0);
        *count += 1;

        if *count == 1 {
            key
        } else {
            format!("{}.{}", key, *count - 1)
        }
    }

    /// Compute rich diff between two CDP-based trees.
    /// Returns detailed MutationObserver-style output.
    pub fn rich_diff(&self, other: &PageMerkleTree) -> RichMerkleDiff {
        // Quick check: if hashes match, no changes
        if self.root_hash == other.root_hash {
            return RichMerkleDiff::no_changes();
        }

        let structural_changed = self.root_structural_hash != other.root_structural_hash;

        // Collect node sets
        let self_ids: HashSet<_> = self.nodes.keys().collect();
        let other_ids: HashSet<_> = other.nodes.keys().collect();

        // Find added/removed/modified
        let added_ids: Vec<_> = other_ids.difference(&self_ids).cloned().collect();
        let removed_ids: Vec<_> = self_ids.difference(&other_ids).cloned().collect();

        let mut modified_ids = Vec::new();
        for id in self_ids.intersection(&other_ids) {
            if let (Some(self_node), Some(other_node)) = (self.nodes.get(*id), other.nodes.get(*id))
            {
                if self_node.content_hash != other_node.content_hash {
                    modified_ids.push((*id).clone());
                }
            }
        }

        // Build detailed changes
        let added_elements = Self::build_element_changes(other, &added_ids);
        let removed_elements = Self::build_element_changes(self, &removed_ids);
        let attribute_changes = Self::extract_attribute_changes(self, other, &modified_ids);
        let text_changes = Self::extract_text_changes(self, other, &modified_ids);

        // Generate semantic signals
        let signals =
            Self::generate_semantic_signals(&added_elements, &removed_elements, &attribute_changes);

        // Build scope info (use max of before/after counts for comprehensive coverage)
        let scope = DiffScope {
            main_document: true,
            shadow_roots_count: self.count_shadow_roots().max(other.count_shadow_roots()),
            iframe_documents_count: self
                .count_iframe_documents()
                .max(other.count_iframe_documents()),
            cross_origin_iframes_count: self
                .count_cross_origin_iframes()
                .max(other.count_cross_origin_iframes()),
        };

        RichMerkleDiff {
            changed: true,
            structural_changed,
            content_changed: true,
            nodes_added: added_ids.len(),
            nodes_removed: removed_ids.len(),
            nodes_modified: modified_ids.len(),
            added_elements,
            removed_elements,
            attribute_changes,
            text_changes,
            signals,
            scope,
        }
    }

    /// Build RichElementChange list from node IDs.
    /// Location is taken from each node's stored location field.
    /// Uses node.attributes for semantic signal detection (role, class, etc.)
    fn build_element_changes(
        tree: &PageMerkleTree,
        node_ids: &[&String],
    ) -> Vec<RichElementChange> {
        node_ids
            .iter()
            .filter_map(|id| {
                tree.nodes.get(*id).map(|node| {
                    let tag_name = match &node.node_type {
                        MerkleNodeType::Element { role, .. } => role.to_uppercase(),
                        MerkleNodeType::Region { description } => description.clone(),
                        MerkleNodeType::Root => "ROOT".to_string(),
                    };

                    // Use the actual stored attributes from the node for signal detection
                    // This includes role, class, aria-* attributes needed by generate_semantic_signals
                    let attributes = node.attributes.clone();

                    // Use stored location from node (set during tree construction)
                    // This correctly distinguishes CrossOriginIframe from IframeDocument
                    let location = node.location.clone();

                    RichElementChange {
                        node_id: (*id).clone(),
                        tag_name,
                        attributes,
                        location,
                        path: None, // Could be populated with rich path
                    }
                })
            })
            .collect()
    }

    /// Extract attribute changes from modified nodes.
    /// Compares actual attributes stored in MerkleNode to produce real attribute diffs.
    fn extract_attribute_changes(
        before: &PageMerkleTree,
        after: &PageMerkleTree,
        modified_ids: &[String],
    ) -> Vec<RichAttributeChange> {
        let mut changes = Vec::new();

        for id in modified_ids {
            if let (Some(before_node), Some(after_node)) =
                (before.nodes.get(id), after.nodes.get(id))
            {
                // Compare structural hashes - if different, structure changed (not just content)
                if before_node.structural_hash != after_node.structural_hash {
                    // This is a structural change, not attribute
                    continue;
                }

                // Get tag name
                let tag_name = match &after_node.node_type {
                    MerkleNodeType::Element { role, .. } => role.to_uppercase(),
                    _ => "UNKNOWN".to_string(),
                };

                // Compare actual attributes for real diffs
                let before_attrs = &before_node.attributes;
                let after_attrs = &after_node.attributes;

                // Find modified and removed attributes
                for (attr_name, old_value) in before_attrs {
                    match after_attrs.get(attr_name) {
                        Some(new_value) if new_value != old_value => {
                            // Attribute value changed
                            changes.push(RichAttributeChange {
                                node_id: id.clone(),
                                tag_name: tag_name.clone(),
                                attribute: attr_name.clone(),
                                old_value: Some(old_value.clone()),
                                new_value: Some(new_value.clone()),
                            });
                        },
                        None => {
                            // Attribute removed
                            changes.push(RichAttributeChange {
                                node_id: id.clone(),
                                tag_name: tag_name.clone(),
                                attribute: attr_name.clone(),
                                old_value: Some(old_value.clone()),
                                new_value: None,
                            });
                        },
                        _ => {}, // Unchanged
                    }
                }

                // Find added attributes
                for (attr_name, new_value) in after_attrs {
                    if !before_attrs.contains_key(attr_name) {
                        changes.push(RichAttributeChange {
                            node_id: id.clone(),
                            tag_name: tag_name.clone(),
                            attribute: attr_name.clone(),
                            old_value: None,
                            new_value: Some(new_value.clone()),
                        });
                    }
                }

                // If no attribute changes detected but content hash changed,
                // report as generic content change (e.g., text content changed)
                if changes.iter().all(|c| c.node_id != *id)
                    && before_node.content_hash != after_node.content_hash
                {
                    changes.push(RichAttributeChange {
                        node_id: id.clone(),
                        tag_name,
                        attribute: "textContent".to_string(),
                        old_value: None, // Text content not stored in attributes
                        new_value: None,
                    });
                }
            }
        }

        changes
    }

    /// Extract text changes from modified nodes.
    fn extract_text_changes(
        before: &PageMerkleTree,
        after: &PageMerkleTree,
        modified_ids: &[String],
    ) -> Vec<RichTextChange> {
        let mut changes = Vec::new();

        for id in modified_ids {
            if let (Some(before_node), Some(after_node)) =
                (before.nodes.get(id), after.nodes.get(id))
            {
                // Check if this is a text-like element
                if let MerkleNodeType::Element {
                    role,
                    name: old_name,
                    ..
                } = &before_node.node_type
                {
                    if role == "text" || role == "staticText" {
                        if let MerkleNodeType::Element { name: new_name, .. } =
                            &after_node.node_type
                        {
                            if old_name != new_name {
                                changes.push(RichTextChange {
                                    node_id: id.clone(),
                                    parent_tag: role.clone(),
                                    old_text: old_name.clone(),
                                    new_text: new_name.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        changes
    }

    /// Generate high-level semantic signals from changes.
    fn generate_semantic_signals(
        added: &[RichElementChange],
        removed: &[RichElementChange],
        attrs: &[RichAttributeChange],
    ) -> Vec<String> {
        let mut signals = Vec::new();

        // Check for popup/modal opened
        let added_tags: Vec<_> = added.iter().map(|e| e.tag_name.to_lowercase()).collect();
        let added_tags_str = added_tags.join(" ");

        if added_tags_str.contains("dialog")
            || added_tags.iter().any(|t| t.contains("modal"))
            || added.iter().any(|e| {
                e.attributes
                    .get("role")
                    .map(|r| r == "dialog" || r == "alertdialog")
                    .unwrap_or(false)
            })
        {
            signals.push("popup_opened".to_string());
        }

        // Check for popup/modal closed
        let removed_tags: Vec<_> = removed.iter().map(|e| e.tag_name.to_lowercase()).collect();
        if removed_tags
            .iter()
            .any(|t| t.contains("dialog") || t.contains("modal"))
        {
            signals.push("popup_closed".to_string());
        }

        // Check for loading states
        if removed.iter().any(|e| {
            let tag = e.tag_name.to_lowercase();
            tag.contains("spinner")
                || tag.contains("loading")
                || e.attributes
                    .get("class")
                    .map(|c| c.contains("loading") || c.contains("spinner"))
                    .unwrap_or(false)
        }) {
            signals.push("loading_complete".to_string());
        }

        if added.iter().any(|e| {
            let tag = e.tag_name.to_lowercase();
            tag.contains("spinner")
                || tag.contains("loading")
                || e.attributes
                    .get("class")
                    .map(|c| c.contains("loading") || c.contains("spinner"))
                    .unwrap_or(false)
        }) {
            signals.push("loading_started".to_string());
        }

        // Check for expanded/collapsed state changes
        for attr in attrs {
            if attr.attribute == "aria-expanded" {
                match (&attr.old_value, &attr.new_value) {
                    (Some(old), Some(new)) if old.contains("false") && new.contains("true") => {
                        signals.push("expanded_state_changed".to_string());
                    },
                    (Some(old), Some(new)) if old.contains("true") && new.contains("false") => {
                        signals.push("collapsed_state_changed".to_string());
                    },
                    _ => {},
                }
            }
        }

        // Check for content added/removed
        if !added.is_empty() && removed.is_empty() {
            signals.push("content_added".to_string());
        }
        if !removed.is_empty() && added.is_empty() {
            signals.push("content_removed".to_string());
        }

        // Deduplicate
        signals.sort();
        signals.dedup();
        signals
    }

    /// Count shadow roots in the tree.
    fn count_shadow_roots(&self) -> usize {
        self.nodes
            .keys()
            .filter(|id| id.contains("/shadow:"))
            .count()
    }

    /// Count iframe documents in the tree.
    fn count_iframe_documents(&self) -> usize {
        self.nodes
            .keys()
            .filter(|id| id.contains("/iframe"))
            .count()
    }

    /// Get cross-origin iframe count (set during CDP tree building).
    fn count_cross_origin_iframes(&self) -> usize {
        self.cross_origin_iframe_count
    }
}

// ============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    fn dom_semantic_id(html: &str, parent_id: &str, seen: &mut HashMap<String, usize>) -> String {
        let (tag, attrs, inner, _) = PageMerkleTree::parse_element(html);
        let text = PageMerkleTree::extract_text_content(&inner);
        PageMerkleTree::generate_semantic_id_for_dom(&tag, &attrs, text.as_deref(), parent_id, seen)
    }

    fn make_tree_with_children(root_hash: &str, child_ids: &[&str]) -> PageMerkleTree {
        let mut nodes = HashMap::new();
        let root_id = "0".to_string();
        let child_ids: Vec<String> = child_ids.iter().map(|id| (*id).to_string()).collect();

        for child_id in &child_ids {
            nodes.insert(
                child_id.clone(),
                MerkleNode {
                    node_id: child_id.clone(),
                    structural_hash: format!("struct-{}", child_id),
                    content_hash: format!("content-{}", child_id),
                    node_type: MerkleNodeType::Element {
                        role: "item".to_string(),
                        name: None,
                        selector: None,
                    },
                    ax_node_id: None,
                    parent_id: Some(root_id.clone()),
                    children: vec![],
                    depth: 1,
                    attributes: HashMap::new(),
                    location: ElementLocation::MainDocument,
                },
            );
        }

        nodes.insert(
            root_id.clone(),
            MerkleNode {
                node_id: root_id.clone(),
                structural_hash: "struct-root".to_string(),
                content_hash: root_hash.to_string(),
                node_type: MerkleNodeType::Root,
                ax_node_id: None,
                parent_id: None,
                children: child_ids.clone(),
                depth: 0,
                attributes: HashMap::new(),
                location: ElementLocation::MainDocument,
            },
        );

        PageMerkleTree {
            root_id,
            root_structural_hash: "struct-root".to_string(),
            root_hash: root_hash.to_string(),
            nodes,
            captured_at: Utc::now(),
            url: None,
            page_stage: PageStage::Unknown,
            node_count: child_ids.len() + 1,
            leaf_count: child_ids.len(),
            max_depth: 1,
            cross_origin_iframe_count: 0,
        }
    }

    #[test]
    fn test_normalize_name_strips_timestamps() {
        assert_eq!(normalize_name("Updated 3:45 PM"), "Updated");
        assert_eq!(normalize_name("Last seen 10:30:45 AM"), "Last seen");
    }

    #[test]
    fn test_normalize_name_strips_dates() {
        assert_eq!(normalize_name("Created on 12/5/2024"), "Created on");
        assert_eq!(normalize_name("Modified 2024-12-05"), "Modified");
    }

    #[test]
    fn test_normalize_name_strips_counts() {
        assert_eq!(normalize_name("3 items in cart"), "items in cart");
        assert_eq!(normalize_name("42 messages"), "messages");
    }

    #[test]
    fn test_normalize_name_strips_prices() {
        assert_eq!(normalize_name("Total: $142.50"), "Total:");
        assert_eq!(normalize_name("Price $1,234.56 USD"), "Price USD");
    }

    #[test]
    fn test_empty_tree() {
        let tree_json = json!({
            "role": {"value": "RootWebArea"},
            "name": {"value": "Empty Page"}
        });

        let tree = PageMerkleTree::from_accessibility_tree(
            &tree_json,
            Some("https://test.com"),
            PageStage::Unknown,
        );

        assert_eq!(tree.node_count, 1);
        assert_eq!(tree.leaf_count, 1);
        assert_eq!(tree.max_depth, 0);
        assert!(tree.nodes.contains_key("0"));
    }

    #[test]
    fn test_simple_tree_building() {
        let tree_json = json!({
            "role": {"value": "RootWebArea"},
            "name": {"value": "Test Page"},
            "children": [
                {
                    "role": {"value": "button"},
                    "name": {"value": "Submit"}
                },
                {
                    "role": {"value": "link"},
                    "name": {"value": "Cancel"}
                }
            ]
        });

        let tree = PageMerkleTree::from_accessibility_tree(
            &tree_json,
            Some("https://test.com"),
            PageStage::Form,
        );

        assert_eq!(tree.node_count, 3);
        assert_eq!(tree.leaf_count, 2);
        assert_eq!(tree.max_depth, 1);
        assert_eq!(tree.page_stage, PageStage::Form);
    }

    #[test]
    fn default_stack_accessibility_depth_is_iterative_and_retained_tree_is_capped() {
        let mut tree_json = json!({
            "role": {"value": "textbox"},
            "name": {"value": "Deep input"},
            "value": {"value": "retained value"}
        });
        for _ in 0..2_048 {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert("role".to_string(), json!({"value": "group"}));
            wrapper.insert("name".to_string(), json!({"value": "Nested group"}));
            wrapper.insert("children".to_string(), Value::Array(vec![tree_json]));
            tree_json = Value::Object(wrapper);
        }

        let form_state = extract_a11y_form_state(&tree_json);
        assert_eq!(
            form_state
                .get("deep input")
                .and_then(|state| state.value.as_deref()),
            Some("retained value")
        );
        let tree = PageMerkleTree::from_accessibility_tree(
            &tree_json,
            Some("https://deep.example"),
            PageStage::Form,
        );
        assert_eq!(tree.max_depth, MAX_ACCESSIBILITY_TREE_DEPTH);
        assert_eq!(tree.node_count, MAX_ACCESSIBILITY_TREE_DEPTH + 1);
        crate::magician_v2::json_traversal::discard_json_iteratively(tree_json);
    }

    #[test]
    fn test_identical_trees_no_diff() {
        let tree_json = json!({
            "role": {"value": "RootWebArea"},
            "name": {"value": "Test Page"},
            "children": [
                {"role": {"value": "button"}, "name": {"value": "Submit"}}
            ]
        });

        let tree1 = PageMerkleTree::from_accessibility_tree(&tree_json, None, PageStage::Unknown);
        let tree2 = PageMerkleTree::from_accessibility_tree(&tree_json, None, PageStage::Unknown);

        let diff = tree1.diff(&tree2);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
        assert_eq!(diff.summary, "No changes detected");
    }

    #[test]
    fn test_structure_vs_content_hash() {
        // Same structure, different content (checkbox state)
        let tree1_json = json!({
            "role": {"value": "checkbox"},
            "name": {"value": "Remember me"},
            "properties": [
                {"name": "checked", "value": {"value": false}}
            ]
        });

        let tree2_json = json!({
            "role": {"value": "checkbox"},
            "name": {"value": "Remember me"},
            "properties": [
                {"name": "checked", "value": {"value": true}}
            ]
        });

        let tree1 = PageMerkleTree::from_accessibility_tree(&tree1_json, None, PageStage::Unknown);
        let tree2 = PageMerkleTree::from_accessibility_tree(&tree2_json, None, PageStage::Unknown);

        // Structural hashes should be the same
        assert_eq!(tree1.root_structural_hash, tree2.root_structural_hash);
        assert!(!tree1.structure_changed(&tree2));

        // Content hashes should differ
        assert_ne!(tree1.root_hash, tree2.root_hash);
        assert!(tree1.content_changed(&tree2));
    }

    #[test]
    fn test_extract_base_fingerprint() {
        assert_eq!(
            PageMerkleTree::extract_base_fingerprint("0/a1b2c3d4"),
            "0/a1b2c3d4"
        );
        assert_eq!(
            PageMerkleTree::extract_base_fingerprint("0/a1b2c3d4.1"),
            "0/a1b2c3d4"
        );
        assert_eq!(
            PageMerkleTree::extract_base_fingerprint("0/a1b2c3d4.10"),
            "0/a1b2c3d4"
        );
        assert_eq!(
            PageMerkleTree::extract_base_fingerprint("0/parent/a1b2c3d4.5"),
            "0/parent/a1b2c3d4"
        );
    }

    #[test]
    fn test_lcs_sibling_matching() {
        let before = vec![
            "0/abc".to_string(),
            "0/def".to_string(),
            "0/ghi".to_string(),
        ];
        let after = vec![
            "0/xyz".to_string(), // added
            "0/abc".to_string(), // kept
            "0/def".to_string(), // kept
                                 // ghi removed
        ];

        let result = match_siblings_lcs(&before, &after);

        assert_eq!(result.matched.len(), 2);
        assert_eq!(result.added, vec!["0/xyz".to_string()]);
        assert_eq!(result.removed, vec!["0/ghi".to_string()]);
    }

    #[test]
    fn test_simple_sibling_diff() {
        let before = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let after = vec!["b".to_string(), "c".to_string(), "d".to_string()];

        let result = simple_sibling_diff(&before, &after);

        assert_eq!(result.matched.len(), 2); // b and c
        assert_eq!(result.added, vec!["d".to_string()]);
        assert_eq!(result.removed, vec!["a".to_string()]);
    }

    #[test]
    fn test_diff_with_added_element() {
        let tree1_json = json!({
            "role": {"value": "RootWebArea"},
            "name": {"value": "Page"},
            "children": [
                {"role": {"value": "button"}, "name": {"value": "Submit"}}
            ]
        });

        let tree2_json = json!({
            "role": {"value": "RootWebArea"},
            "name": {"value": "Page"},
            "children": [
                {"role": {"value": "button"}, "name": {"value": "Submit"}},
                {"role": {"value": "button"}, "name": {"value": "Cancel"}}
            ]
        });

        let tree1 = PageMerkleTree::from_accessibility_tree(&tree1_json, None, PageStage::Unknown);
        let tree2 = PageMerkleTree::from_accessibility_tree(&tree2_json, None, PageStage::Unknown);

        let diff = tree1.diff(&tree2);

        // One new element (Cancel button) added
        assert_eq!(diff.added.len(), 1);
        assert!(diff.removed.is_empty());

        // Parent nodes are marked as "modified" because their children hashes changed
        // This is correct Merkle behavior - hash changes propagate up
        assert!(!diff.modified.is_empty());

        // Changed subtrees include both the added button AND the modified root
        // (since root's children list changed)
        assert!(!diff.changed_subtrees.is_empty());

        // Verify at least one subtree is the Added button
        let has_added_subtree = diff
            .changed_subtrees
            .iter()
            .any(|s| matches!(s.change_type, SubtreeChangeType::Added));
        assert!(has_added_subtree);
    }

    #[test]
    fn test_dom_semantic_ids_prevent_cascade_on_insert() {
        let before_html = concat!(
            "<ul>",
            "<li data-testid=\"alpha\">Alpha</li>",
            "<li data-testid=\"beta\">Beta</li>",
            "<li data-testid=\"gamma\">Gamma</li>",
            "</ul>"
        );
        let after_html = concat!(
            "<ul>",
            "<li data-testid=\"new\">New</li>",
            "<li data-testid=\"alpha\">Alpha</li>",
            "<li data-testid=\"beta\">Beta</li>",
            "<li data-testid=\"gamma\">Gamma</li>",
            "</ul>"
        );

        let before = PageMerkleTree::from_dom_snapshot(before_html, None, None, PageStage::Unknown);
        let after = PageMerkleTree::from_dom_snapshot(after_html, None, None, PageStage::Unknown);

        let mut seen = HashMap::new();
        let alpha_id = dom_semantic_id("<li data-testid=\"alpha\">Alpha</li>", "0", &mut seen);
        let beta_id = dom_semantic_id("<li data-testid=\"beta\">Beta</li>", "0", &mut seen);
        let gamma_id = dom_semantic_id("<li data-testid=\"gamma\">Gamma</li>", "0", &mut seen);
        let new_id = dom_semantic_id("<li data-testid=\"new\">New</li>", "0", &mut seen);

        assert!(before.nodes.contains_key(&alpha_id));
        assert!(before.nodes.contains_key(&beta_id));
        assert!(before.nodes.contains_key(&gamma_id));
        assert!(after.nodes.contains_key(&alpha_id));
        assert!(after.nodes.contains_key(&beta_id));
        assert!(after.nodes.contains_key(&gamma_id));

        let diff = before.diff(&after);

        assert!(diff.removed.is_empty());
        assert!(diff.added.contains(&new_id));
        assert!(!diff.added.contains(&alpha_id));
        assert!(!diff.added.contains(&beta_id));
        assert!(!diff.added.contains(&gamma_id));
    }

    #[test]
    fn test_diff_uses_lcs_for_suffix_changes() {
        let before = make_tree_with_children("root-before", &["0/abc", "0/abc.1"]);
        let after = make_tree_with_children("root-after", &["0/abc.1", "0/abc.2"]);

        let diff = before.diff(&after);

        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }
}
