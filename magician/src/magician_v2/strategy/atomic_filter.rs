//! Atomic tool grouping and formatting utilities
//!
//! This module groups the planner-visible catalog by composition category and
//! formats it for llm-reasoning prompts in the atomic composition strategy.
//! It does not narrow the catalog — see [`group_atomic_tools`].

use std::collections::HashMap;

use tracing::{debug, info};

fn stringify_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(stringify_json_value).collect();
            format!("[{}]", inner.join(", "))
        },
        serde_json::Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
        serde_json::Value::Null => "null".to_string(),
    }
}

use runtime_core::ToolInfo;

/// Filtered atomic tools grouped by composition category
#[derive(Debug, Clone)]
pub struct AtomicToolSet {
    /// All atomic tools
    pub tools: Vec<ToolInfo>,
    /// Tools grouped by composition_category (e.g., "shell_operations",
    /// "browser_navigation")
    pub by_category: HashMap<String, Vec<ToolInfo>>,
    /// Total count of atomic tools
    pub total_count: usize,
}

impl AtomicToolSet {
    /// Create a new empty atomic tool set
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            by_category: HashMap::new(),
            total_count: 0,
        }
    }

    /// Format tools for llm-reasoning prompt with interleaved browser/shell ordering
    ///
    /// Produces a structured list with atomic indicators, interleaving browser and shell
    /// tools to avoid ordering bias:
    /// ```text
    /// ATOMIC TOOLS AVAILABLE:
    ///
    /// [Category: browser_navigation]
    /// - browser_navigate: Navigate browser to a URL
    ///
    /// [Category: shell_operations]
    /// - shell: Execute a single shell command and return output
    ///
    /// [Category: browser_interaction]
    /// - browser_click: Click on an element
    /// ```
    pub fn format_for_prompt(&self) -> String {
        let mut output = String::from("ATOMIC TOOLS AVAILABLE:\n\n");

        // Helper to check if a category or its tools are browser-related
        fn is_browser_category(category: &str, tools: &[ToolInfo]) -> bool {
            category.contains("browser") || tools.iter().any(|t| t.name.contains("browser"))
        }

        // Helper to check if a category or its tools are shell-related
        fn is_shell_category(category: &str, tools: &[ToolInfo]) -> bool {
            category.contains("shell")
                || category.contains("bash")
                || tools
                    .iter()
                    .any(|t| t.name.contains("shell") || t.name.contains("bash"))
        }

        // Separate tools into browser, shell, and other categories
        let mut browser_categories = Vec::new();
        let mut shell_categories = Vec::new();
        let mut other_categories = Vec::new();

        for (category, tools) in &self.by_category {
            if is_browser_category(category, tools) {
                browser_categories.push((category.clone(), tools.clone()));
            } else if is_shell_category(category, tools) {
                shell_categories.push((category.clone(), tools.clone()));
            } else {
                other_categories.push((category.clone(), tools.clone()));
            }
        }

        // Sort each group by category name for consistency
        browser_categories.sort_by(|a, b| a.0.cmp(&b.0));
        shell_categories.sort_by(|a, b| a.0.cmp(&b.0));
        other_categories.sort_by(|a, b| a.0.cmp(&b.0));

        // Interleave browser and shell categories to avoid bias
        let max_len = browser_categories.len().max(shell_categories.len());
        let mut interleaved = Vec::new();

        for i in 0..max_len {
            if i < browser_categories.len() {
                interleaved.push(browser_categories[i].clone());
            }
            if i < shell_categories.len() {
                interleaved.push(shell_categories[i].clone());
            }
        }

        // Append other categories at the end
        interleaved.extend(other_categories);

        // Log the interleaved tool order
        let tool_names: Vec<String> = interleaved
            .iter()
            .flat_map(|(cat, tools)| {
                let cat = cat.clone();
                tools.iter().map(move |t| format!("{}:{}", cat, t.name))
            })
            .collect();

        info!(
            "[MAGICIAN-V2-STRATEGY] Atomic tools in prompt order (interleaved): {}",
            tool_names.join(", ")
        );

        for (category, tools) in &interleaved {
            output.push_str(&format!("[Category: {}]\n", category));

            for tool in tools {
                output.push_str(&format!("- {}: {}\n", tool.name, tool.description));

                if let Some(enhanced) = &tool.enhanced_description {
                    output.push_str(&format!("  Enhanced Context: {}\n", enhanced));
                }

                if !tool.categories.is_empty() {
                    output.push_str(&format!("  Tags: {}\n", tool.categories.join(", ")));
                }

                if !tool.parameters.is_empty() {
                    output.push_str("  Parameters:\n");
                    for param in &tool.parameters {
                        let requirement = if param.required {
                            "REQUIRED"
                        } else {
                            "optional"
                        };
                        output.push_str(&format!(
                            "    - {} ({}): {} [{}]\n",
                            param.name,
                            param.param_type,
                            if param.description.is_empty() {
                                "no description provided"
                            } else {
                                param.description.as_str()
                            },
                            requirement
                        ));

                        if let Some(default) = &param.default_value {
                            output.push_str(&format!(
                                "      default/example: {}\n",
                                stringify_json_value(default)
                            ));
                        }
                    }
                }

                if !tool.use_cases.is_empty() {
                    output.push_str("  Sample use-cases:\n");
                    for case in &tool.use_cases {
                        output.push_str(&format!("    - {}\n", case));
                    }
                }

                if !tool.keywords.is_empty() {
                    output.push_str(&format!("  Keywords: {}\n", tool.keywords.join(", ")));
                }

                output.push('\n');
            }

            output.push('\n');
        }

        output
    }

    /// Format tools for planning prompt in compact form (progressive disclosure).
    ///
    /// Outputs name + description, and optionally parameter signatures. Omits:
    /// - Enhanced context / guide text (~520 chars per tool)
    /// - Sample use-cases
    /// - Keywords
    /// - Tags
    ///
    /// Convenience wrapper that includes parameter signatures.
    ///
    /// Production callers use
    /// [`Self::format_for_prompt_compact_with_options`] directly, because the
    /// answer differs by planning phase — see
    /// `AtomicCompositionStrategy::render_atomic_tool_prompt`. This wrapper
    /// once passed `false` while its own doc promised `true`, which is how the
    /// planner came to receive no parameter schema at all: not from here, and
    /// not from `StrategyContext::render_planner_tool_catalog`, which emits
    /// name/categories/description only. The atomic plan contract still
    /// requires a `parameters` object per step and `validate_plan` books every
    /// unfilled required parameter as an `UnresolvedInput`, so the expansion
    /// was structurally guaranteed to leave a hole for every tool that takes
    /// arguments.
    pub fn format_for_prompt_compact(&self) -> String {
        self.format_for_prompt_compact_with_options(true)
    }

    /// Compact format with explicit control over parameter inclusion.
    pub fn format_for_prompt_compact_with_options(&self, include_params: bool) -> String {
        let mut output = String::from("ATOMIC TOOLS AVAILABLE:\n\n");

        // Reuse the same interleaving logic for ordering consistency
        fn is_browser_category(category: &str, tools: &[ToolInfo]) -> bool {
            category.contains("browser") || tools.iter().any(|t| t.name.contains("browser"))
        }
        fn is_shell_category(category: &str, tools: &[ToolInfo]) -> bool {
            category.contains("shell")
                || category.contains("bash")
                || tools
                    .iter()
                    .any(|t| t.name.contains("shell") || t.name.contains("bash"))
        }

        // Borrowed, not cloned. This used to deep-copy every `ToolInfo` —
        // including each parameter's `serde_json::Value` schema — once into the
        // per-group vectors and a second time while interleaving. It runs on
        // every prompt render (the outline plus up to three expansion
        // retries), and the catalog it copies is now ~80 tools rather than
        // ~40, so the copying grew with the fix it supports. Nothing here
        // mutates a tool, so references are sufficient.
        let mut browser_categories: Vec<(&str, &[ToolInfo])> = Vec::new();
        let mut shell_categories: Vec<(&str, &[ToolInfo])> = Vec::new();
        let mut other_categories: Vec<(&str, &[ToolInfo])> = Vec::new();

        for (category, tools) in &self.by_category {
            if is_browser_category(category, tools) {
                browser_categories.push((category.as_str(), tools.as_slice()));
            } else if is_shell_category(category, tools) {
                shell_categories.push((category.as_str(), tools.as_slice()));
            } else {
                other_categories.push((category.as_str(), tools.as_slice()));
            }
        }

        browser_categories.sort_by(|a, b| a.0.cmp(b.0));
        shell_categories.sort_by(|a, b| a.0.cmp(b.0));
        other_categories.sort_by(|a, b| a.0.cmp(b.0));

        let max_len = browser_categories.len().max(shell_categories.len());
        let mut interleaved: Vec<(&str, &[ToolInfo])> = Vec::new();
        for i in 0..max_len {
            if i < browser_categories.len() {
                interleaved.push(browser_categories[i]);
            }
            if i < shell_categories.len() {
                interleaved.push(shell_categories[i]);
            }
        }
        interleaved.extend(other_categories);

        // By value, not by reference: the entries are `(&str, &[ToolInfo])`,
        // which is `Copy`. Iterating `&interleaved` would bind `tools` as
        // `&&[ToolInfo]`, and `IntoIterator` is not implemented for a double
        // reference.
        for (category, tools) in interleaved {
            output.push_str(&format!("[Category: {}]\n", category));

            for tool in tools {
                output.push_str(&format!("- {}: {}\n", tool.name, tool.description));

                if include_params && !tool.parameters.is_empty() {
                    output.push_str("  Parameters:\n");
                    for param in &tool.parameters {
                        let requirement = if param.required { " [REQUIRED]" } else { "" };
                        output.push_str(&format!(
                            "    - {} ({}): {}{}\n",
                            param.name,
                            param.param_type,
                            if param.description.is_empty() {
                                "no description provided"
                            } else {
                                param.description.as_str()
                            },
                            requirement
                        ));
                    }
                }

                output.push('\n');
            }

            output.push('\n');
        }

        output
    }

    /// Get tool count by category
    pub fn category_counts(&self) -> HashMap<String, usize> {
        self.by_category
            .iter()
            .map(|(cat, tools)| (cat.clone(), tools.len()))
            .collect()
    }
}

impl Default for AtomicToolSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Group the planner's tools by composition category.
///
/// Every tool is kept. An earlier revision filtered to an "atomic-only"
/// subset, but the marker it filtered on stopped being populated and the body
/// became a pass-through while the name and log line still claimed to filter.
/// Restoring a filter here would only narrow a catalog that
/// `merge_universal_backend_packs` exists to widen, so the honest behaviour is
/// grouping — the name now says so.
///
/// Tools without a `composition_category` land under `uncategorized`. That is
/// load-bearing downstream: `plan_validator` reads the category to decide
/// whether a step needs browser session metadata or a shell/file rationale.
///
/// # Arguments
/// * `all_tools` - the planner-visible catalog
///
/// # Returns
/// * `AtomicToolSet` - the same tools, indexed by composition category
pub fn group_atomic_tools(all_tools: &[ToolInfo]) -> AtomicToolSet {
    info!(
        "[MAGICIAN-V2-STRATEGY] Grouping {} planner tools by composition category",
        all_tools.len()
    );

    let mut atomic_tools = Vec::new();
    let mut by_category: HashMap<String, Vec<ToolInfo>> = HashMap::new();

    for tool in all_tools {
        atomic_tools.push(tool.clone());

        // Group by composition category if available
        let category = tool
            .composition_category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());

        by_category.entry(category).or_default().push(tool.clone());
    }

    let total_count = atomic_tools.len();

    info!(
        "[MAGICIAN-V2-STRATEGY] Filtered to {} atomic tools across {} categories",
        total_count,
        by_category.len()
    );

    debug!(
        "[MAGICIAN-V2-STRATEGY] Atomic tools by category: {:?}",
        by_category
            .iter()
            .map(|(k, v)| (k, v.len()))
            .collect::<HashMap<_, _>>()
    );

    AtomicToolSet {
        tools: atomic_tools,
        by_category,
        total_count,
    }
}

/// Get composition category for a tool
pub fn get_composition_category(tool: &ToolInfo) -> String {
    tool.composition_category
        .clone()
        .unwrap_or_else(|| "uncategorized".to_string())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn create_test_tool(name: &str, category: Option<&str>) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: format!("{} description", name),
            category: "test".to_string(),
            categories: vec!["test".to_string()], // Already populated correctly
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: category.map(|s| s.to_string()),
            providing_agent_id: None,
        }
    }

    #[test]
    fn test_group_atomic_tools() {
        let tools = vec![
            create_test_tool("shell", Some("shell_operations")),
            create_test_tool("complex_tool", None),
            create_test_tool("browser_click", Some("browser_navigation")),
            create_test_tool("another_complex", None),
            create_test_tool("shell_pipeline", Some("shell_operations")),
        ];

        let atomic_set = group_atomic_tools(&tools);

        assert_eq!(atomic_set.total_count, 5);
        assert_eq!(atomic_set.tools.len(), 5);
        assert_eq!(atomic_set.by_category.len(), 3);
        assert_eq!(
            atomic_set
                .by_category
                .get("shell_operations")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            atomic_set
                .by_category
                .get("browser_navigation")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            atomic_set.by_category.get("uncategorized").unwrap().len(),
            2
        );
    }

    #[test]
    fn test_group_atomic_tools_falls_back_when_markers_missing() {
        let tools = vec![
            create_test_tool("tool_a", Some("group_a")),
            create_test_tool("tool_b", Some("group_b")),
        ];
        let atomic_set = group_atomic_tools(&tools);

        assert_eq!(atomic_set.total_count, 2);
        assert_eq!(atomic_set.tools.len(), 2);
        assert_eq!(atomic_set.by_category.len(), 2);
    }

    #[test]
    fn test_format_for_prompt() {
        let tools = vec![
            create_test_tool("shell", Some("shell_operations")),
            create_test_tool("browser_click", Some("browser_navigation")),
        ];

        let atomic_set = group_atomic_tools(&tools);
        let formatted = atomic_set.format_for_prompt();

        assert!(formatted.contains("ATOMIC TOOLS AVAILABLE"));
        assert!(formatted.contains("[Category: shell_operations]"));
        assert!(formatted.contains("[Category: browser_navigation]"));
        assert!(formatted.contains("shell"));
        assert!(formatted.contains("browser_click"));
    }

    #[test]
    fn test_category_counts() {
        let tools = vec![
            create_test_tool("shell1", Some("shell")),
            create_test_tool("shell2", Some("shell")),
            create_test_tool("browser1", Some("browser")),
        ];

        let atomic_set = group_atomic_tools(&tools);
        let counts = atomic_set.category_counts();

        assert_eq!(counts.get("shell"), Some(&2));
        assert_eq!(counts.get("browser"), Some(&1));
    }
}
