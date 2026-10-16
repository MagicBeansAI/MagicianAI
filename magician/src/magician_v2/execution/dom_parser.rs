//! DOM Parser for Merkle Tree Building
//!
//! Parses HTML DOM snapshots into structured `DomElement` objects with
//! CSS selector generation for Merkle tree construction and element targeting.
//!
//! # Overview
//!
//! This module processes raw DOM HTML and accessibility tree data to extract
//! interactive elements with stable, unique CSS selectors. The selectors
//! follow a priority order designed for automation reliability:
//!
//! 1. ID selector: `#login-btn`
//! 2. data-testid: `[data-testid="login-button"]`
//! 3. data-cy/data-test: `[data-cy="login"]`
//! 4. Unique aria-label: `[aria-label="Sign in"]`
//! 5. Unique class combo: `.btn.btn-primary.login-btn`
//! 6. Tag + attributes: `button[type="submit"]`
//! 7. Nth-child path: `form > div:nth-child(2) > button`
//!
//! # Usage
//!
//! ```ignore
//! let config = DomParserConfig::default();
//! let elements = parse_dom_elements(&dom_html, Some(&a11y_tree), &config)?;
//! for elem in elements {
//!     println!("Found {} at selector: {}", elem.tag, elem.selector);
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

/// Bounding box coordinates for spatial matching
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl BoundingBox {
    /// Calculate Intersection over Union (IoU) with another bounding box
    pub fn iou(&self, other: &BoundingBox) -> f64 {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x + self.width).min(other.x + other.width);
        let y2 = (self.y + self.height).min(other.y + other.height);

        if x2 <= x1 || y2 <= y1 {
            return 0.0; // No overlap
        }

        let intersection = (x2 - x1) * (y2 - y1);
        let area_self = self.width * self.height;
        let area_other = other.width * other.height;
        let union = area_self + area_other - intersection;

        if union <= 0.0 {
            0.0
        } else {
            intersection / union
        }
    }
}

/// Parsed DOM element with selector and attributes.
///
/// Created by `parse_dom_elements()`, used for Merkle tree building
/// and element attribute extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomElement {
    /// Generated CSS selector (stable, unique)
    pub selector: String,

    /// Fallback selectors in priority order
    pub selector_hints: Vec<String>,

    /// HTML tag name (button, input, a, div, etc.)
    pub tag: String,

    /// Visible text content (trimmed, normalized)
    pub text: Option<String>,

    /// All attributes (id, class, data-*, aria-*, etc.)
    pub attributes: HashMap<String, String>,

    /// Bounding box from CDP (for spatial matching)
    pub bounding_box: Option<BoundingBox>,

    /// Accessibility role (from a11y tree, if matched)
    pub role: Option<String>,

    /// Whether element is interactive (clickable, focusable)
    pub is_interactive: bool,

    /// XPath as ultimate fallback
    pub xpath: String,

    /// Confidence in this element detection (0.0 - 1.0)
    pub confidence: f64,
}

impl DomElement {
    /// Generate the most stable selector for this element.
    ///
    /// Priority order:
    /// 1. ID (most stable, if not auto-generated)
    /// 2. data-testid (designed for automation)
    /// 3. data-cy (Cypress convention)
    /// 4. data-test (alternative convention)
    /// 5. Unique aria-label
    /// 6. Fallback to less stable selectors
    pub fn generate_selector(&self) -> String {
        // Priority 1: ID (most stable, but skip auto-generated IDs)
        if let Some(id) = self.attributes.get("id") {
            if Self::is_stable_id(id) {
                return format!("#{}", Self::escape_css_selector(id));
            }
        }

        // Priority 2: data-testid (designed for automation)
        if let Some(testid) = self.attributes.get("data-testid") {
            return format!("[data-testid=\"{}\"]", Self::escape_attr_value(testid));
        }

        // Priority 3: data-cy (Cypress convention)
        if let Some(cy) = self.attributes.get("data-cy") {
            return format!("[data-cy=\"{}\"]", Self::escape_attr_value(cy));
        }

        // Priority 4: data-test
        if let Some(test) = self.attributes.get("data-test") {
            return format!("[data-test=\"{}\"]", Self::escape_attr_value(test));
        }

        // Priority 5: Unique aria-label
        if let Some(label) = self.attributes.get("aria-label") {
            if !label.is_empty() {
                return format!(
                    "{}[aria-label=\"{}\"]",
                    self.tag,
                    Self::escape_attr_value(label)
                );
            }
        }

        // Priority 6: name attribute (for form elements)
        if let Some(name) = self.attributes.get("name") {
            if !name.is_empty() && matches!(self.tag.as_str(), "input" | "select" | "textarea") {
                return format!("{}[name=\"{}\"]", self.tag, Self::escape_attr_value(name));
            }
        }

        // Priority 7: type attribute for inputs
        if self.tag == "input" {
            if let Some(input_type) = self.attributes.get("type") {
                if matches!(input_type.as_str(), "submit" | "button" | "reset") {
                    return format!("input[type=\"{}\"]", input_type);
                }
            }
        }

        // Fallback to xpath
        self.xpath.clone()
    }

    /// Generate all possible selectors in priority order
    pub fn generate_all_selectors(&self) -> Vec<String> {
        let mut selectors = Vec::new();

        // ID
        if let Some(id) = self.attributes.get("id") {
            if Self::is_stable_id(id) {
                selectors.push(format!("#{}", Self::escape_css_selector(id)));
            }
        }

        // data-testid
        if let Some(testid) = self.attributes.get("data-testid") {
            selectors.push(format!(
                "[data-testid=\"{}\"]",
                Self::escape_attr_value(testid)
            ));
        }

        // data-cy
        if let Some(cy) = self.attributes.get("data-cy") {
            selectors.push(format!("[data-cy=\"{}\"]", Self::escape_attr_value(cy)));
        }

        // aria-label
        if let Some(label) = self.attributes.get("aria-label") {
            if !label.is_empty() {
                selectors.push(format!(
                    "{}[aria-label=\"{}\"]",
                    self.tag,
                    Self::escape_attr_value(label)
                ));
            }
        }

        // name attribute
        if let Some(name) = self.attributes.get("name") {
            if !name.is_empty() {
                selectors.push(format!(
                    "{}[name=\"{}\"]",
                    self.tag,
                    Self::escape_attr_value(name)
                ));
            }
        }

        // XPath fallback
        if !self.xpath.is_empty() {
            selectors.push(self.xpath.clone());
        }

        selectors
    }

    /// Check if an ID is stable (not auto-generated)
    fn is_stable_id(id: &str) -> bool {
        // Skip common auto-generated ID patterns
        if id.contains("__") || id.contains("::") {
            return false;
        }
        // Skip framework-specific auto IDs
        if id.starts_with("ember") || id.starts_with("react-") || id.starts_with("vue-") {
            return false;
        }
        // Skip numeric-only or very short IDs
        if id.parse::<u64>().is_ok() || id.len() < 2 {
            return false;
        }
        // Skip IDs that look like hashes
        if id.len() > 20 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        true
    }

    /// Escape special characters in CSS selectors
    fn escape_css_selector(s: &str) -> String {
        let mut result = String::with_capacity(s.len() * 2);
        for c in s.chars() {
            match c {
                '!' | '"' | '#' | '$' | '%' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | '.'
                | '/' | ':' | ';' | '<' | '=' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '`'
                | '{' | '|' | '}' | '~' => {
                    result.push('\\');
                    result.push(c);
                },
                ' ' => {
                    result.push_str("\\ ");
                },
                _ => result.push(c),
            }
        }
        result
    }

    /// Escape special characters in attribute values
    fn escape_attr_value(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    }
}

/// Configuration for DOM parsing
#[derive(Debug, Clone)]
pub struct DomParserConfig {
    /// Maximum elements to parse (performance limit)
    pub max_elements: usize,
    /// Include non-interactive elements
    pub include_non_interactive: bool,
    /// Maximum DOM size to process (bytes)
    pub max_dom_size: usize,
}

impl Default for DomParserConfig {
    fn default() -> Self {
        Self {
            max_elements: 500,
            include_non_interactive: false,
            max_dom_size: 5 * 1024 * 1024, // 5MB
        }
    }
}

/// Parse DOM HTML into structured elements.
///
/// Extracts interactive elements with CSS selectors from raw HTML.
/// Optionally enriches with accessibility tree data for role information.
pub fn parse_dom_elements(
    dom_html: &str,
    accessibility_tree: Option<&serde_json::Value>,
    config: &DomParserConfig,
) -> Result<Vec<DomElement>, DomParserError> {
    // Check size limit
    if dom_html.len() > config.max_dom_size {
        warn!(
            "DOM snapshot exceeds size limit ({} > {}), truncating",
            dom_html.len(),
            config.max_dom_size
        );
    }

    let html_to_parse = if dom_html.len() > config.max_dom_size {
        &dom_html[..config.max_dom_size]
    } else {
        dom_html
    };

    // Parse HTML using regex-based extraction (lightweight, no external deps)
    // For production, consider using `scraper` crate for proper HTML5 parsing
    let mut elements = extract_interactive_elements(html_to_parse, config)?;

    // Enrich with accessibility tree if available
    if let Some(a11y_tree) = accessibility_tree {
        enrich_with_accessibility(a11y_tree, &mut elements);
    }

    // Limit element count
    if elements.len() > config.max_elements {
        debug!(
            "Limiting elements from {} to {}",
            elements.len(),
            config.max_elements
        );
        elements.truncate(config.max_elements);
    }

    Ok(elements)
}

/// Extract interactive elements from HTML using pattern matching.
///
/// This is a lightweight implementation. For production use with complex HTML,
/// consider using the `scraper` crate.
fn extract_interactive_elements(
    html: &str,
    config: &DomParserConfig,
) -> Result<Vec<DomElement>, DomParserError> {
    let mut elements = Vec::new();
    let mut element_index = 0;

    // Interactive tag patterns
    let interactive_tags = [
        "button", "a", "input", "select", "textarea", "label", "summary",
    ];

    // Role patterns for elements that might be interactive
    let interactive_roles = [
        "button",
        "link",
        "textbox",
        "checkbox",
        "radio",
        "combobox",
        "listbox",
        "menuitem",
        "tab",
        "switch",
        "searchbox",
    ];

    // Simple tag extraction pattern
    // Format: <tag ... attributes ... >content</tag> or <tag ... /> or <tag>content</tag>
    for tag in &interactive_tags {
        // Search for both <tag  (with space for attributes) and <tag> (no attributes)
        let tag_patterns = [format!("<{} ", tag), format!("<{}>", tag)];
        let mut search_start = 0;

        loop {
            // Find the next occurrence of either pattern
            let next_match = tag_patterns
                .iter()
                .filter_map(|pattern| {
                    html[search_start..]
                        .find(pattern)
                        .map(|pos| (pos, pattern.len()))
                })
                .min_by_key(|(pos, _)| *pos);

            let Some((relative_start, _)) = next_match else {
                break;
            };
            let abs_start = search_start + relative_start;

            // Find the end of the opening tag
            if let Some(tag_end) = html[abs_start..].find('>') {
                let tag_content = &html[abs_start..abs_start + tag_end + 1];

                // Extract attributes
                let attributes = extract_attributes(tag_content);

                // Extract text content (simplified - just look for closing tag)
                let text = extract_text_content(&html[abs_start + tag_end + 1..], tag);

                // Generate xpath
                element_index += 1;
                let xpath = format!("(//{tag})[{element_index}]");

                let element = DomElement {
                    selector: String::new(), // Will be generated
                    selector_hints: Vec::new(),
                    tag: tag.to_string(),
                    text,
                    attributes,
                    bounding_box: None,
                    role: None,
                    is_interactive: true,
                    xpath,
                    confidence: 0.8, // DOM-extracted elements have high base confidence
                };

                // Generate selector
                let mut element = element;
                element.selector = element.generate_selector();
                element.selector_hints = element.generate_all_selectors();

                elements.push(element);
            }

            search_start = abs_start + 1;

            if elements.len() >= config.max_elements {
                break;
            }
        }

        if elements.len() >= config.max_elements {
            break;
        }
    }

    // Also extract elements with interactive roles
    if !config.include_non_interactive {
        for role in &interactive_roles {
            let role_pattern = format!(r#"role="{}"#, role);
            let mut search_start = 0;

            while let Some(start) = html[search_start..].find(&role_pattern) {
                let abs_start = search_start + start;

                // Find the start of the tag
                if let Some(tag_start) = html[..abs_start].rfind('<') {
                    // Find the end of the opening tag
                    if let Some(tag_end) = html[tag_start..].find('>') {
                        let tag_content = &html[tag_start..tag_start + tag_end + 1];

                        // Extract tag name
                        if let Some(tag_name) = extract_tag_name(tag_content) {
                            // Skip if we already have this element (from interactive_tags)
                            if interactive_tags.contains(&tag_name.as_str()) {
                                search_start = abs_start + 1;
                                continue;
                            }

                            let attributes = extract_attributes(tag_content);
                            let text =
                                extract_text_content(&html[tag_start + tag_end + 1..], &tag_name);

                            element_index += 1;
                            let xpath = format!("(//*[@role='{}'])[{}]", role, element_index);

                            let element = DomElement {
                                selector: String::new(),
                                selector_hints: Vec::new(),
                                tag: tag_name,
                                text,
                                attributes,
                                bounding_box: None,
                                role: Some(role.to_string()),
                                is_interactive: true,
                                xpath,
                                confidence: 0.75,
                            };

                            let mut element = element;
                            element.selector = element.generate_selector();
                            element.selector_hints = element.generate_all_selectors();

                            elements.push(element);
                        }
                    }
                }

                search_start = abs_start + 1;

                if elements.len() >= config.max_elements {
                    break;
                }
            }

            if elements.len() >= config.max_elements {
                break;
            }
        }
    }

    Ok(elements)
}

/// Extract tag name from an opening tag
fn extract_tag_name(tag_content: &str) -> Option<String> {
    let trimmed = tag_content.trim_start_matches('<');
    let end = trimmed.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
    Some(trimmed[..end].to_lowercase())
}

/// Extract attributes from a tag string
fn extract_attributes(tag_content: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();

    // Pattern: attribute="value" or attribute='value' or attribute
    let mut remaining = tag_content;

    while let Some(eq_pos) = remaining.find('=') {
        // Find attribute name (word before =)
        let before_eq = &remaining[..eq_pos];
        let attr_start = before_eq
            .rfind(|c: char| c.is_whitespace())
            .map(|p| p + 1)
            .unwrap_or(0);
        let attr_name = before_eq[attr_start..].trim().to_lowercase();

        // Find attribute value (after =)
        let after_eq = &remaining[eq_pos + 1..];
        let after_eq = after_eq.trim_start();

        let (value, end_pos) = if after_eq.starts_with('"') {
            // Double-quoted value
            if let Some(end) = after_eq[1..].find('"') {
                (after_eq[1..end + 1].to_string(), eq_pos + 2 + end + 1)
            } else {
                break;
            }
        } else if after_eq.starts_with('\'') {
            // Single-quoted value
            if let Some(end) = after_eq[1..].find('\'') {
                (after_eq[1..end + 1].to_string(), eq_pos + 2 + end + 1)
            } else {
                break;
            }
        } else {
            // Unquoted value (up to whitespace or >)
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

    attributes
}

/// Extract text content from element
fn extract_text_content(html_after_tag: &str, tag_name: &str) -> Option<String> {
    let close_tag = format!("</{}>", tag_name);
    if let Some(end) = html_after_tag.find(&close_tag) {
        let content = &html_after_tag[..end];
        // Strip nested tags and get text only
        let text = strip_html_tags(content).trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// Strip HTML tags from content
fn strip_html_tags(html: &str) -> String {
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

    result
}

/// Enrich DOM elements with accessibility tree data
fn enrich_with_accessibility(a11y_tree: &serde_json::Value, elements: &mut [DomElement]) {
    // Build a map of normalized_text -> (role, name, states) from accessibility tree
    // Handles CDP Accessibility.getFullAXTree format: { role: { value: "..." }, name: { value: "..." }, ... }
    let mut ax_map: HashMap<String, AxInfo> = HashMap::new();

    // CDP returns a flat array of nodes
    if let Some(nodes) = a11y_tree.as_array() {
        for node in nodes {
            // Skip ignored nodes
            if node
                .get("ignored")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }

            // Extract role (CDP format: role.value)
            let role = node
                .get("role")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .map(String::from);

            // Skip generic/container roles
            if let Some(ref r) = role {
                if matches!(
                    r.as_str(),
                    "none" | "generic" | "group" | "paragraph" | "InlineTextBox" | "StaticText"
                ) {
                    continue;
                }
            }

            // Extract name (CDP format: name.value)
            let name = node
                .get("name")
                .and_then(|n| n.get("value"))
                .and_then(|v| v.as_str())
                .map(String::from);

            // Extract states from properties
            let mut states = Vec::new();
            if let Some(props) = node.get("properties").and_then(|p| p.as_array()) {
                for prop in props {
                    let prop_name = prop.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let prop_val = prop.get("value").and_then(|v| v.get("value"));

                    match prop_val {
                        Some(v) if v.as_bool() == Some(true) => {
                            states.push(prop_name.to_string());
                        },
                        Some(v) if prop_name == "checked" && v.as_str() == Some("mixed") => {
                            states.push("indeterminate".to_string());
                        },
                        Some(v) if prop_name == "expanded" => {
                            if v.as_bool() == Some(true) {
                                states.push("expanded".to_string());
                            } else if v.as_bool() == Some(false) {
                                states.push("collapsed".to_string());
                            }
                        },
                        _ => {},
                    }
                }
            }

            // Index by normalized name for text-based matching
            if let Some(ref n) = name {
                let normalized = n.to_lowercase().trim().to_string();
                if !normalized.is_empty() {
                    ax_map.insert(normalized, AxInfo { role, name, states });
                }
            }
        }
    } else {
        // Fallback to old recursive format (non-CDP format)
        extract_roles_recursive_legacy(a11y_tree, &mut ax_map);
    }

    // Enrich elements that have matching text
    for element in elements.iter_mut() {
        if let Some(text) = &element.text {
            let normalized_text = text.to_lowercase().trim().to_string();
            if let Some(ax_info) = ax_map.get(&normalized_text) {
                // Fill missing role
                if element.role.is_none() {
                    if let Some(ref role) = ax_info.role {
                        element.role = Some(role.clone());
                        element.confidence = (element.confidence + 0.1).min(1.0);
                    }
                }

                // Merge AX states into attributes (checked, disabled, expanded, etc.)
                for state in &ax_info.states {
                    let attr_name = format!("aria-{}", state);
                    element
                        .attributes
                        .entry(attr_name)
                        .or_insert_with(|| "true".to_string());
                }
            }
        }
    }
}

/// AX node info extracted from accessibility tree
struct AxInfo {
    role: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    states: Vec<String>,
}

/// Extract text->role mappings from legacy accessibility data without making
/// external tree depth native-stack depth.
fn extract_roles_recursive_legacy(root: &serde_json::Value, ax_map: &mut HashMap<String, AxInfo>) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if let Some(role) = node["role"].as_str() {
            if let Some(name) = node["name"].as_str() {
                let normalized = name.to_lowercase().trim().to_string();
                if !normalized.is_empty() {
                    ax_map.insert(
                        normalized,
                        AxInfo {
                            role: Some(role.to_string()),
                            name: Some(name.to_string()),
                            states: Vec::new(),
                        },
                    );
                }
            }
        }

        if let Some(children) = node["children"].as_array() {
            pending.extend(children.iter().rev());
        }
    }
}

/// Errors that can occur during DOM parsing
#[derive(Debug, thiserror::Error)]
pub enum DomParserError {
    #[error("DOM parsing failed: {0}")]
    ParseError(String),

    #[error("DOM size exceeds limit: {0} bytes")]
    SizeExceeded(usize),

    #[error("Invalid HTML structure")]
    InvalidHtml,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_extract_attributes() {
        let tag = r#"<button id="login" class="btn primary" data-testid="login-btn">"#;
        let attrs = extract_attributes(tag);

        assert_eq!(attrs.get("id"), Some(&"login".to_string()));
        assert_eq!(attrs.get("class"), Some(&"btn primary".to_string()));
        assert_eq!(attrs.get("data-testid"), Some(&"login-btn".to_string()));
    }

    #[test]
    fn test_generate_selector_priority() {
        // Test data-testid takes priority
        let elem = DomElement {
            selector: String::new(),
            selector_hints: vec![],
            tag: "button".to_string(),
            text: Some("Click me".to_string()),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert("id".to_string(), "btn-123".to_string());
                attrs.insert("data-testid".to_string(), "submit-btn".to_string());
                attrs
            },
            bounding_box: None,
            role: None,
            is_interactive: true,
            xpath: "//button[1]".to_string(),
            confidence: 0.8,
        };

        let selector = elem.generate_selector();
        // ID takes priority over data-testid since it's stable
        assert_eq!(selector, "#btn-123");
    }

    #[test]
    fn test_skip_auto_generated_ids() {
        let elem = DomElement {
            selector: String::new(),
            selector_hints: vec![],
            tag: "button".to_string(),
            text: Some("Click me".to_string()),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert("id".to_string(), "ember123".to_string());
                attrs.insert("data-testid".to_string(), "submit-btn".to_string());
                attrs
            },
            bounding_box: None,
            role: None,
            is_interactive: true,
            xpath: "//button[1]".to_string(),
            confidence: 0.8,
        };

        let selector = elem.generate_selector();
        // Should skip ember ID and use data-testid
        assert_eq!(selector, "[data-testid=\"submit-btn\"]");
    }

    #[test]
    fn test_bounding_box_iou() {
        let box1 = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let box2 = BoundingBox {
            x: 50.0,
            y: 50.0,
            width: 100.0,
            height: 100.0,
        };

        let iou = box1.iou(&box2);
        // Intersection is 50x50 = 2500
        // Union is 100*100 + 100*100 - 2500 = 17500
        // IoU = 2500 / 17500 ≈ 0.143
        assert!((iou - 0.143).abs() < 0.01);
    }

    #[test]
    fn test_parse_simple_html() {
        let html = r#"
            <button id="submit" data-testid="submit-btn">Submit</button>
            <a href="/login">Login</a>
            <input type="text" name="username" placeholder="Enter username">
        "#;

        let config = DomParserConfig::default();
        let elements = parse_dom_elements(html, None, &config).unwrap();

        assert_eq!(elements.len(), 3);
        assert!(elements.iter().any(|e| e.tag == "button"));
        assert!(elements.iter().any(|e| e.tag == "a"));
        assert!(elements.iter().any(|e| e.tag == "input"));
    }

    #[test]
    fn test_escape_css_selector() {
        assert_eq!(DomElement::escape_css_selector("simple"), "simple");
        assert_eq!(DomElement::escape_css_selector("with.dot"), "with\\.dot");
        assert_eq!(
            DomElement::escape_css_selector("with space"),
            "with\\ space"
        );
        assert_eq!(DomElement::escape_css_selector("id#123"), "id\\#123");
    }

    #[test]
    fn test_parse_tags_without_attributes() {
        // Test that tags without any attributes are correctly parsed
        // This was a bug fix: <button> (no space) was missed by pattern <button  (with space)
        let html = r#"
            <button>Click Me</button>
            <a>Link Text</a>
            <select>
                <option>Option 1</option>
            </select>
        "#;

        let config = DomParserConfig::default();
        let elements = parse_dom_elements(html, None, &config).unwrap();

        // Should find button, a, and select (option is also interactive)
        assert!(
            elements
                .iter()
                .any(|e| e.tag == "button" && e.text == Some("Click Me".to_string())),
            "Should find button without attributes"
        );
        assert!(
            elements
                .iter()
                .any(|e| e.tag == "a" && e.text == Some("Link Text".to_string())),
            "Should find anchor without attributes"
        );
        assert!(
            elements.iter().any(|e| e.tag == "select"),
            "Should find select without attributes"
        );
    }

    #[test]
    fn test_parse_mixed_tags_with_and_without_attributes() {
        // Test mix of tags with and without attributes
        let html = r#"
            <button id="btn1">With ID</button>
            <button>Without ID</button>
            <button class="primary">With Class</button>
        "#;

        let config = DomParserConfig::default();
        let elements = parse_dom_elements(html, None, &config).unwrap();

        let buttons: Vec<_> = elements.iter().filter(|e| e.tag == "button").collect();
        assert_eq!(buttons.len(), 3, "Should find all 3 buttons");

        // Verify each button was found
        assert!(
            buttons
                .iter()
                .any(|e| e.text == Some("With ID".to_string())),
            "Should find button with id"
        );
        assert!(
            buttons
                .iter()
                .any(|e| e.text == Some("Without ID".to_string())),
            "Should find button without attributes"
        );
        assert!(
            buttons
                .iter()
                .any(|e| e.text == Some("With Class".to_string())),
            "Should find button with class"
        );
    }
}
