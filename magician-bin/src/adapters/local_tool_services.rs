use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use magician::magician_v2::{
    artifact_v2::CapabilityWorkspaceManager,
    execution::{
        pack_defs_to_tool_infos, prune_runtime_disabled_pack_defs, prune_unexecutable_pack_defs,
        PackToolInfo,
    },
    secrets::SecretRuntimeCapabilities,
};
use runtime_core::{
    context::ExecutionContext,
    services::{SemanticSearch, ToolCatalog, ToolMatching, CORE_UTILITY_CATEGORY},
    tooling::{
        MultipleToolMatchResult, ParameterDefinition, SemanticMatch, SuccessMetrics, ToolInfo,
        ToolMatch, ToolMatchResult, ToolMetadata,
    },
};
use serde_json::Value;
use tool_runtime_core::{
    config::Config as RuntimeConfig,
    registry::{types::ToolDefinition, RegistryService},
    tool_discovery::semantic::SemanticSearchService,
};
use tracing::{info, warn};

#[derive(Clone, Default)]
struct PackSurface {
    tools: HashMap<String, ToolDefinition>,
    guides: HashMap<String, String>,
}

#[derive(Clone)]
pub struct LocalToolServices {
    registry: Arc<RegistryService>,
    semantic_search: Option<Arc<SemanticSearchService>>,
    pack_surface: Arc<RwLock<PackSurface>>,
    available_shell_binaries: Vec<String>,
    capability_workspace: Option<Arc<CapabilityWorkspaceManager>>,
    secret_capabilities: SecretRuntimeCapabilities,
}

impl LocalToolServices {
    pub async fn from_config_path(
        config_path: &Path,
        pack_tool_infos: Vec<PackToolInfo>,
        capability_workspace: Option<Arc<CapabilityWorkspaceManager>>,
        secret_capabilities: SecretRuntimeCapabilities,
    ) -> Result<Self> {
        let config = RuntimeConfig::load(config_path, None, None).unwrap_or_else(|err| {
            warn!(
                "Failed to load '{}': {}. Using defaults.",
                config_path.display(),
                err
            );
            RuntimeConfig::default()
        });

        let registry = RegistryService::new(config.registry.clone())
            .await
            .context("failed to initialize local registry service")?;

        // Keep pack-derived tools outside the shared registry so they can be
        // resolved per scope at query time instead of being frozen at startup.
        let mut pack_tools = HashMap::new();
        let mut tool_guides = HashMap::new();
        info!(
            "Registering {} pack-derived tool definitions",
            pack_tool_infos.len(),
        );
        for (name, def, guide) in pack_tool_infos {
            pack_tools.insert(name.clone(), def);
            if let Some(text) = guide {
                tool_guides.insert(name, Self::compact_text(&text, 520));
            }
        }

        let registry = Arc::new(registry);

        let loaded_tool_count = registry.get_all_tools().len() + pack_tools.len();
        if loaded_tool_count == 0 {
            bail!(
                "No tools were loaded. Capability packs were empty (system skills, \
                 extra-system paths {:?}, scope overlay, and embedded compiled defs \
                 all produced zero entries). Cannot start without an executable tool surface.",
                config.registry.paths
            );
        }
        info!(
            "Local tool registry initialized with {} tools",
            loaded_tool_count
        );

        let semantic_search = Self::initialize_semantic_search(
            config.semantic_search,
            Arc::clone(&registry),
            &pack_tools,
        )
        .await;
        let available_shell_binaries =
            Self::detect_available_binaries(&["jq", "python3", "curl", "rg"]);
        info!(
            "Detected shell helper binaries on host: {}",
            if available_shell_binaries.is_empty() {
                "none".to_string()
            } else {
                available_shell_binaries.join(", ")
            }
        );

        Ok(Self {
            registry,
            semantic_search,
            pack_surface: Arc::new(RwLock::new(PackSurface {
                tools: pack_tools,
                guides: tool_guides,
            })),
            available_shell_binaries,
            capability_workspace,
            secret_capabilities,
        })
    }

    /// A services instance for tests that exercise the surface lock: a real
    /// (empty) registry, an optional semantic index, and no host probing.
    #[cfg(test)]
    fn for_test(
        registry: Arc<RegistryService>,
        semantic_search: Option<Arc<SemanticSearchService>>,
        capability_workspace: Arc<CapabilityWorkspaceManager>,
    ) -> Self {
        Self {
            registry,
            semantic_search,
            pack_surface: Arc::new(RwLock::new(PackSurface::default())),
            available_shell_binaries: Vec::new(),
            capability_workspace: Some(capability_workspace),
            secret_capabilities: SecretRuntimeCapabilities::fully_available("test"),
        }
    }

    fn detect_available_binaries(candidates: &[&str]) -> Vec<String> {
        candidates
            .iter()
            .copied()
            .filter(|binary| {
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("command -v {} >/dev/null 2>&1", binary))
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            })
            .map(ToString::to_string)
            .collect()
    }

    fn compact_text(raw: &str, max_chars: usize) -> String {
        let compact = raw.split_whitespace().collect::<Vec<&str>>().join(" ");
        if compact.chars().count() <= max_chars {
            return compact;
        }
        let mut truncated = String::new();
        for ch in compact.chars().take(max_chars.saturating_sub(3)) {
            truncated.push(ch);
        }
        truncated.push_str("...");
        truncated
    }

    fn base_pack_surface(&self) -> PackSurface {
        self.pack_surface
            .read()
            .expect("pack_surface lock poisoned")
            .clone()
    }

    /// Remove pack-derived tools and their guides at runtime. Used by
    /// startup deferred-capability registration (bin/magician.rs) to
    /// strip a tool entry when its pack failed to load.
    pub fn remove_pack_tools(&self, names: &[String]) {
        // The refresh below reads the surface back through `base_pack_surface`,
        // so the write guard must be gone before it runs: `RwLock` is not
        // re-entrant, and holding it here deadlocked the caller on its own lock
        // — at boot, the main thread, with the HTTP server never coming up.
        {
            let mut surface = self
                .pack_surface
                .write()
                .expect("pack_surface lock poisoned");
            for name in names {
                surface.tools.remove(name);
                surface.guides.remove(name);
            }
        }
        self.refresh_semantic_index();
    }

    async fn initialize_semantic_search(
        semantic_config: Option<tool_runtime_core::config::SemanticSearchConfig>,
        registry: Arc<RegistryService>,
        pack_tools: &HashMap<String, ToolDefinition>,
    ) -> Option<Arc<SemanticSearchService>> {
        let semantic_config = semantic_config.unwrap_or_default();
        if !semantic_config.enabled {
            info!("Semantic search disabled in config; using lexical fallback matching");
            return None;
        }

        let semantic_search = Arc::new(SemanticSearchService::new(semantic_config));
        if let Err(err) = semantic_search.initialize().await {
            warn!(
                "Failed to initialize semantic search for local backend: {}. Using lexical fallback matching.",
                err
            );
            return None;
        }

        let tools: Vec<ToolDefinition> = registry
            .get_all_tools()
            .into_values()
            .chain(pack_tools.values().cloned())
            .collect();
        semantic_search.replace_index(tools).await;

        Some(semantic_search)
    }

    fn refresh_semantic_index(&self) {
        let Some(semantic_search) = &self.semantic_search else {
            return;
        };
        let base_surface = self.base_pack_surface();
        let tools: Vec<ToolDefinition> = self
            .registry
            .get_all_tools()
            .into_values()
            .chain(base_surface.tools.into_values())
            .collect();
        let semantic_search = Arc::clone(semantic_search);
        tokio::spawn(async move {
            semantic_search.replace_index(tools).await;
        });
    }

    fn scope_pack_surface(&self, context: &ExecutionContext) -> PackSurface {
        let Some(workspace) = &self.capability_workspace else {
            return self.base_pack_surface();
        };
        if context.principal.is_empty()
            || context.workspace.is_empty()
            || (context.principal == "system" && context.workspace == "default")
        {
            return self.base_pack_surface();
        }

        if let Err(err) = workspace.materialize_scope(&context.principal, &context.workspace) {
            warn!(
                "Failed to materialize capability scope {}/{} for tool catalog; falling back to startup surface: {}",
                context.principal, context.workspace, err
            );
            return self.base_pack_surface();
        }

        let mut pack_defs =
            workspace.load_pack_defs_for_scope(&context.principal, &context.workspace);
        prune_unexecutable_pack_defs(&mut pack_defs);
        prune_runtime_disabled_pack_defs(&mut pack_defs, &self.secret_capabilities);

        let mut tools = HashMap::new();
        let mut guides = HashMap::new();
        for (name, def, guide) in pack_defs_to_tool_infos(&pack_defs) {
            tools.insert(name.clone(), def);
            if let Some(text) = guide {
                guides.insert(name, Self::compact_text(&text, 520));
            }
        }
        PackSurface { tools, guides }
    }

    fn merged_tool_map(&self, context: &ExecutionContext) -> HashMap<String, ToolDefinition> {
        let mut tools = self.registry.get_all_tools();
        let scoped_surface = self.scope_pack_surface(context);
        for (name, tool) in scoped_surface.tools {
            tools.insert(name, tool);
        }
        tools
    }

    fn tool_for_context(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<ToolDefinition> {
        self.merged_tool_map(context).remove(tool_name)
    }

    fn guide_for_tool(&self, tool_name: &str, context: &ExecutionContext) -> Option<String> {
        self.scope_pack_surface(context)
            .guides
            .get(tool_name)
            .cloned()
    }

    /// Returns tools visible to a specific agent based on their tool whitelist.
    #[allow(dead_code)] // Phase 1 infrastructure — wired when StrategyContext carries agent tools
    ///
    /// `combined_catalog = union of agent's resolved tools + delegation targets' resolved tools`.
    ///
    /// Filtering matches against both tool `name` and `categories` fields.
    /// When `tools` is empty, all visible tools are included (no whitelist restriction).
    ///
    /// # Arguments
    /// * `_agent_id` — the requesting agent (reserved for future per-agent overrides)
    /// * `tools` — tool names or category names the agent exposes (whitelist)
    /// * `excluded_tools` — tool names or category names the agent explicitly excludes (blacklist)
    /// * `delegate_packs` — `(delegate_id, allowed_tools, excluded_tools)` for
    ///   each delegate whose tools should be merged into the catalog
    pub fn tools_for_agent(
        &self,
        _agent_id: &str,
        tools: &[String],
        excluded_tools: &[String],
        delegate_packs: &[(&str, &[String], &[String])],
    ) -> Vec<ToolDefinition> {
        let all_tools = self.visible_enabled_tools(&ExecutionContext::default());

        // Resolve own tools.
        let own_tools = Self::resolve_tools(&all_tools, tools, excluded_tools);

        // Resolve each delegate's tools and union.
        let mut combined = own_tools;
        for (_delegate_id, d_tools, d_excluded) in delegate_packs {
            let delegate_tools = Self::resolve_tools(&all_tools, d_tools, d_excluded);
            for tool in delegate_tools {
                if !combined.iter().any(|t| t.name == tool.name) {
                    combined.push(tool);
                }
            }
        }
        combined
    }

    /// Filter tools by allowed/excluded tool names and categories.
    #[allow(dead_code)] // Used by tools_for_agent above
    ///
    /// - When `tools` is empty, all tools pass the whitelist check.
    /// - Tools in `core_utility` pass unless explicitly excluded.
    /// - A tool matches if its `name` matches or any of its `categories` matches.
    fn resolve_tools(
        all_tools: &[ToolDefinition],
        tools: &[String],
        excluded_tools: &[String],
    ) -> Vec<ToolDefinition> {
        all_tools
            .iter()
            .filter(|tool| {
                let in_allowed = tools.is_empty()
                    || tools.iter().any(|t| t == &tool.name)
                    || tool
                        .categories
                        .iter()
                        .any(|cat| tools.iter().any(|t| t == cat))
                    || tool
                        .categories
                        .iter()
                        .any(|cat| cat == CORE_UTILITY_CATEGORY);
                let not_excluded = !excluded_tools.iter().any(|t| t == &tool.name)
                    && !tool
                        .categories
                        .iter()
                        .any(|cat| excluded_tools.iter().any(|t| t == cat));
                in_allowed && not_excluded
            })
            .cloned()
            .collect()
    }

    fn visible_enabled_tools(&self, context: &ExecutionContext) -> Vec<ToolDefinition> {
        self.visible_enabled_tool_surface(context).0
    }

    /// Resolve the scoped pack surface once for catalog-shaped reads.
    ///
    /// `all_tools` and category filtering need both tool definitions and their
    /// compact guides. Loading `scope_pack_surface` again from `tool_to_info`
    /// for every visible tool reparsed every scoped YAML pack N times and could
    /// starve the async HTTP workers while a plan was being prepared.
    fn visible_enabled_tool_surface(
        &self,
        context: &ExecutionContext,
    ) -> (Vec<ToolDefinition>, HashMap<String, String>) {
        let PackSurface {
            tools: scoped_tools,
            guides,
        } = self.scope_pack_surface(context);
        let mut tools = self.registry.get_all_tools();
        for (name, tool) in scoped_tools {
            tools.insert(name, tool);
        }
        let visible = tools
            .into_iter()
            .filter_map(|(name, tool)| {
                if self.registry.is_tool_hidden_hierarchical(&name, &tool) {
                    return None;
                }
                if !self.registry.is_tool_enabled_hierarchical(&name, &tool) {
                    return None;
                }
                Some(tool)
            })
            .collect();
        (visible, guides)
    }

    fn tool_parameters(tool: &ToolDefinition) -> Vec<ParameterDefinition> {
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object());
        let required_fields: HashSet<String> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();

        match props {
            Some(props) => props
                .iter()
                .map(|(name, schema)| ParameterDefinition {
                    name: name.clone(),
                    param_type: schema
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("string")
                        .to_string(),
                    required: required_fields.contains(name),
                    description: schema
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    validation_rules: vec![],
                    default_value: schema.get("default").cloned(),
                    enum_values: schema.get("enum").and_then(|e| e.as_array()).map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(ToString::to_string))
                            .collect()
                    }),
                    // Carry the JSON Schema we just parsed straight through —
                    // it's already a JSON Schema, no need to re-derive.
                    schema: schema.clone(),
                })
                .collect(),
            None => vec![],
        }
    }

    fn tool_category(tool: &ToolDefinition) -> String {
        tool.categories
            .first()
            .cloned()
            .unwrap_or_else(|| "uncategorized".to_string())
    }

    fn shell_binaries_hint(&self) -> Option<String> {
        if self.available_shell_binaries.is_empty() {
            None
        } else {
            Some(format!(
                "Detected shell helpers on host: {}",
                self.available_shell_binaries.join(", ")
            ))
        }
    }

    fn tool_to_info(&self, tool: &ToolDefinition, guide: Option<&str>) -> ToolInfo {
        let guide = guide.map(ToString::to_string);
        let shell_hint = if tool.name == "shell" {
            self.shell_binaries_hint()
        } else {
            None
        };
        let enhanced_description = match (guide, shell_hint) {
            (Some(g), Some(h)) => Some(format!("{} {}", g, h)),
            (Some(g), None) => Some(g),
            (None, Some(h)) => Some(h),
            (None, None) => None,
        };

        ToolInfo {
            name: tool.name.clone(),
            description: tool.description.clone(),
            category: Self::tool_category(tool),
            categories: tool.categories.clone(),
            parameters: Self::tool_parameters(tool),
            enhanced_description,
            keywords: vec![],
            use_cases: vec![],
            composition_category: tool
                .metadata
                .get("composition_category")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            providing_agent_id: tool
                .metadata
                .get("providing_agent_id")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
        }
    }

    fn tool_to_metadata(
        &self,
        tool: &ToolDefinition,
        confidence: f32,
        context: &ExecutionContext,
    ) -> ToolMetadata {
        let guide = self.guide_for_tool(&tool.name, context);
        let shell_hint = if tool.name == "shell" {
            self.shell_binaries_hint()
        } else {
            None
        };
        let enhanced_description = match (guide, shell_hint) {
            (Some(g), Some(h)) => Some(format!("{} {}", g, h)),
            (Some(g), None) => Some(g),
            (None, Some(h)) => Some(h),
            (None, None) => None,
        };

        ToolMetadata {
            name: tool.name.clone(),
            description: tool.description.clone(),
            category: Self::tool_category(tool),
            typical_use_cases: vec![],
            input_schema: tool.input_schema.clone(),
            output_schema: serde_json::json!({}),
            success_rate: confidence,
            avg_execution_time: 0.0,
            enhanced_description,
            keywords: vec![],
            use_cases: vec![],
            confidence_score: confidence,
            success_metrics: SuccessMetrics {
                success_rate: confidence,
                avg_execution_time: 0.0,
                reliability_score: confidence,
                last_updated: chrono::Utc::now().timestamp_millis(),
            },
            last_updated: chrono::Utc::now().timestamp_millis(),
        }
    }

    fn lexical_score(query: &str, tool: &ToolDefinition) -> f32 {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return 0.0;
        }

        let mut score = 0.0_f32;
        let name = tool.name.to_lowercase();
        let description = tool.description.to_lowercase();

        if name == query {
            score += 1.0;
        } else if name.contains(&query) {
            score += 0.8;
        }

        if description.contains(&query) {
            score += 0.6;
        }

        for token in query.split_whitespace().filter(|t| t.len() > 2) {
            if name.contains(token) {
                score += 0.2;
            }
            if description.contains(token) {
                score += 0.1;
            }
            if tool
                .categories
                .iter()
                .any(|c| c.to_lowercase().contains(token))
            {
                score += 0.1;
            }
        }

        score.min(1.0)
    }

    async fn lexical_matches(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> Vec<(ToolDefinition, f32)> {
        let mut scored: Vec<(ToolDefinition, f32)> = self
            .visible_enabled_tools(context)
            .into_iter()
            .map(|tool| {
                let score = Self::lexical_score(task_description, &tool);
                (tool, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    async fn ranked_matches(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> Vec<(ToolDefinition, f32)> {
        let visible: HashMap<String, ToolDefinition> = self
            .visible_enabled_tools(context)
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect();

        let lexical = self.lexical_matches(task_description, context).await;
        let mut ranked: HashMap<String, (ToolDefinition, f32)> = lexical
            .into_iter()
            .map(|(tool, score)| (tool.name.clone(), (tool, score)))
            .collect();

        if let Some(semantic_search) = &self.semantic_search {
            match semantic_search
                .search_all_tools_with_scores(task_description)
                .await
            {
                Ok(semantic_matches) => {
                    for semantic in semantic_matches {
                        if let Some(tool) = visible.get(&semantic.tool_name).cloned() {
                            let score = semantic.similarity_score as f32;
                            ranked
                                .entry(tool.name.clone())
                                .and_modify(|(_, existing)| {
                                    if score > *existing {
                                        *existing = score;
                                    }
                                })
                                .or_insert((tool, score));
                        }
                    }
                },
                Err(err) => {
                    warn!(
                        "Semantic matching failed (query='{}'): {}. Using lexical-only ranking.",
                        task_description, err
                    );
                },
            }
        }

        let mut ranked: Vec<(ToolDefinition, f32)> = ranked.into_values().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }
}

#[async_trait]
impl ToolCatalog for LocalToolServices {
    async fn list_tool_names(&self, context: &ExecutionContext) -> Vec<String> {
        self.visible_enabled_tools(context)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    async fn available_categories(&self, context: &ExecutionContext) -> Vec<String> {
        let mut categories = HashSet::new();
        for tool in self.visible_enabled_tools(context) {
            for category in &tool.categories {
                if !category.is_empty() {
                    categories.insert(category.clone());
                }
            }
        }
        let mut categories: Vec<String> = categories.into_iter().collect();
        categories.sort();
        categories
    }

    async fn get_tool_metadata(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, Value>> {
        let tool = self.tool_for_context(tool_name, context)?;
        if self.registry.is_tool_hidden_hierarchical(tool_name, &tool) {
            return None;
        }
        if !self.registry.is_tool_enabled_hierarchical(tool_name, &tool) {
            return None;
        }

        let mut metadata = tool.metadata.clone();
        metadata.insert("name".to_string(), Value::String(tool.name.clone()));
        metadata.insert(
            "description".to_string(),
            Value::String(tool.description.clone()),
        );
        metadata.insert(
            "categories".to_string(),
            Value::Array(
                tool.categories
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect::<Vec<Value>>(),
            ),
        );
        metadata.insert("input_schema".to_string(), tool.input_schema.clone());
        if let Some(guide) = self.guide_for_tool(tool_name, context) {
            metadata.insert("guide".to_string(), Value::String(guide));
        }
        if tool_name == "shell" {
            metadata.insert(
                "available_binaries".to_string(),
                Value::Array(
                    self.available_shell_binaries
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        Some(metadata)
    }

    async fn filtered_tools_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        let (visible, guides) = self.visible_enabled_tool_surface(context);
        let filtered = if categories.is_empty() {
            visible
        } else {
            visible
                .into_iter()
                .filter(|tool| {
                    tool.categories.iter().any(|tool_category| {
                        categories
                            .iter()
                            .any(|requested| tool_category.eq_ignore_ascii_case(requested))
                    })
                })
                .collect()
        };

        Ok(filtered
            .into_iter()
            .map(|tool| {
                let guide = guides.get(&tool.name).map(String::as_str);
                self.tool_to_info(&tool, guide)
            })
            .collect())
    }

    async fn all_tools(&self, context: &ExecutionContext) -> Result<Vec<ToolInfo>> {
        let (visible, guides) = self.visible_enabled_tool_surface(context);
        Ok(visible
            .into_iter()
            .map(|tool| {
                let guide = guides.get(&tool.name).map(String::as_str);
                self.tool_to_info(&tool, guide)
            })
            .collect())
    }

    async fn category_tool_counts(
        &self,
        context: &ExecutionContext,
    ) -> Result<HashMap<String, usize>> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for tool in self.visible_enabled_tools(context) {
            for category in &tool.categories {
                if !category.is_empty() {
                    *counts.entry(category.clone()).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }
}

#[async_trait]
impl ToolMatching for LocalToolServices {
    async fn best_match(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> ToolMatchResult {
        let matches = self.ranked_matches(task_description, context).await;

        let best = matches.into_iter().next();
        match best {
            Some((tool, confidence)) if confidence > 0.0 => ToolMatchResult {
                primary_match: Some(ToolMatch {
                    tool_name: tool.name.clone(),
                    capability_match: confidence,
                    parameter_mapping: HashMap::new(),
                    execution_confidence: confidence,
                    tool_metadata: self.tool_to_metadata(&tool, confidence, context),
                }),
                match_confidence: confidence,
                missing_capabilities: vec![],
                parameter_coverage: 1.0,
                executable: true,
            },
            _ => ToolMatchResult::default(),
        }
    }

    async fn multiple_matches(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> MultipleToolMatchResult {
        let matches = self.ranked_matches(task_description, context).await;

        let top_matches: Vec<ToolMatch> = matches
            .into_iter()
            .take(10)
            .filter(|(_, score)| *score > 0.0)
            .map(|(tool, score)| ToolMatch {
                tool_name: tool.name.clone(),
                capability_match: score,
                parameter_mapping: HashMap::new(),
                execution_confidence: score,
                tool_metadata: self.tool_to_metadata(&tool, score, context),
            })
            .collect();

        if top_matches.is_empty() {
            return MultipleToolMatchResult::default();
        }

        let confidences: Vec<f32> = top_matches.iter().map(|m| m.execution_confidence).collect();
        let confidence_spread = confidences.iter().cloned().fold(0.0_f32, f32::max)
            - confidences.iter().cloned().fold(1.0_f32, f32::min);

        MultipleToolMatchResult {
            matches: top_matches,
            match_strategies: vec![if self.semantic_search.is_some() {
                "semantic_search".to_string()
            } else {
                "lexical_fallback".to_string()
            }],
            confidence_spread,
            recommended_approach: Some(0),
            any_executable: true,
            aggregate_missing_capabilities: vec![],
        }
    }

    async fn is_tool_available(&self, tool_name: &str, context: &ExecutionContext) -> bool {
        self.tool_for_context(tool_name, context)
            .map(|tool| {
                !self.registry.is_tool_hidden_hierarchical(tool_name, &tool)
                    && self.registry.is_tool_enabled_hierarchical(tool_name, &tool)
            })
            .unwrap_or(false)
    }

    async fn validate_tool_execution(
        &self,
        tool_name: &str,
        parameters: &Value,
        context: &ExecutionContext,
    ) -> bool {
        let tool = match self.tool_for_context(tool_name, context) {
            Some(tool) => tool,
            None => return false,
        };

        let required = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();

        if required.is_empty() {
            return true;
        }
        match parameters.as_object() {
            Some(params) => required.iter().all(|field| params.contains_key(field)),
            None => false,
        }
    }

    async fn estimate_execution_cost(
        &self,
        tool_name: &str,
        _parameters: &Value,
        _context: &ExecutionContext,
    ) -> f64 {
        match tool_name {
            name if name.contains("file") || name.contains("read") => 1.0,
            name if name.contains("write") || name.contains("create") => 2.0,
            name if name.contains("execute") || name.contains("run") => 3.0,
            name if name.contains("network") || name.contains("http") => 2.5,
            _ => 1.5,
        }
    }
}

#[async_trait]
impl SemanticSearch for LocalToolServices {
    async fn search_all_tools_with_scores(&self, query: &str) -> Result<Vec<SemanticMatch>> {
        if let Some(semantic_search) = &self.semantic_search {
            let matches = semantic_search
                .search_all_tools_with_scores(query)
                .await
                .map_err(|e| anyhow::anyhow!("semantic search failed: {}", e))?;
            return Ok(matches
                .into_iter()
                .map(|m| SemanticMatch {
                    tool_name: m.tool_name,
                    similarity_score: m.similarity_score,
                    enabled: m.enabled,
                    hidden: m.hidden,
                })
                .collect());
        }

        let context = ExecutionContext::default();
        let lexical = self.lexical_matches(query, &context).await;
        Ok(lexical
            .into_iter()
            .map(|(tool, score)| SemanticMatch {
                tool_name: tool.name,
                similarity_score: score as f64,
                enabled: true,
                hidden: false,
            })
            .collect())
    }

    async fn search_categories(&self, category: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        if let Some(semantic_search) = &self.semantic_search {
            return semantic_search
                .search_categories(category, top_k)
                .await
                .map_err(|e| anyhow::anyhow!("semantic category search failed: {}", e));
        }

        let mut categories: Vec<(String, f32)> = self
            .registry
            .get_all_tools()
            .into_iter()
            .flat_map(|(_, tool)| tool.categories.into_iter())
            .collect::<HashSet<String>>()
            .into_iter()
            .map(|candidate| {
                let score = if candidate.eq_ignore_ascii_case(category) {
                    1.0
                } else if candidate.to_lowercase().contains(&category.to_lowercase())
                    || category.to_lowercase().contains(&candidate.to_lowercase())
                {
                    0.7
                } else {
                    0.0
                };
                (candidate, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        categories.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        categories.truncate(top_k);
        Ok(categories)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use tool_runtime_core::config::{RegistryConfig, SemanticSearchConfig};

    use super::*;

    /// `remove_pack_tools` mutates the pack surface and then refreshes the
    /// semantic index, which reads the surface back. Holding the write guard
    /// across that refresh deadlocks the caller on its own lock — at boot,
    /// that is the main thread, and the HTTP server never comes up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removing_pack_tools_does_not_wait_on_its_own_surface_lock() {
        let root = std::env::temp_dir().join(format!(
            "local-tool-services-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let workspace = ArtifactV2Workspace::new(root.join("runtime"));
        let capability_workspace = Arc::new(CapabilityWorkspaceManager::new(workspace, &root));
        let registry = Arc::new(
            RegistryService::new(RegistryConfig::default())
                .await
                .expect("an empty registry needs no tool paths"),
        );
        let semantic_search = Arc::new(SemanticSearchService::new(SemanticSearchConfig::default()));
        let services =
            LocalToolServices::for_test(registry, Some(semantic_search), capability_workspace);

        // A detached OS thread carrying the runtime handle: `refresh_semantic_index`
        // spawns onto the runtime, and a deadlocked thread must not be one the
        // runtime waits for at shutdown, or a regression hangs instead of failing.
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        {
            let services = services.clone();
            let handle = tokio::runtime::Handle::current();
            std::thread::spawn(move || {
                let _runtime = handle.enter();
                services.remove_pack_tools(&["phantom_probe".to_string()]);
                let _ = done_tx.send(());
            });
        }
        tokio::time::timeout(Duration::from_secs(5), done_rx)
            .await
            .expect("remove_pack_tools must not wait on its own surface lock")
            .expect("the removal thread reports");
        assert!(services.base_pack_surface().tools.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
