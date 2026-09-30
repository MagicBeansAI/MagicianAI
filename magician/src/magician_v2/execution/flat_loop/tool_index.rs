//! Server-side tool index for the flat loop. Holds the full schema for every
//! leaf tool the runtime knows about, keyed by leaf name, plus a pack→leaves
//! map so the catalog builder can expand a pack the agent allowlisted into its
//! per-primitive leaves. Not part of any prompt — consulted only by
//! `tool_search` (schema fetch) and `build_flat_loop_tools` (catalog assembly).
//!
//! Promotion rule (the heart of Phase 1):
//! 1. A pack with non-empty `native_action_schemas` → one leaf per
//!    `(action, schema)`, named `<pack>__<action>`. Action overrides take
//!    precedence over the canonical pack parameter schemas.
//! 2. A pack with empty `native_action_schemas` → one leaf named after the
//!    pack, parameters from pack-level `parameters` via
//!    `derive_param_schema_for_emission` (mirrors `pack_def_to_direct_capability`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::magician_v2::execution::capability::{
    derive_param_schema_for_emission, CapabilityPackDefinition, NativeActionSchemaDef,
};
use crate::magician_v2::tool_result_projection::ProjectionContractSpec;

/// One fetchable tool leaf. `name` is what the LLM calls; `parameters_schema`
/// is the full JSON Schema returned by `tool_search(select:<name>)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolIndexEntry {
    pub name: String,
    pub description: String,
    pub parameters_schema: Value,
    /// Optional pack-level one-liner used for keyword-search ranking and shown
    /// once in the deferred-tools prompt.
    pub search_hint: Option<String>,
    /// Optional excerpt of the pack's `guide:` returned alongside the schema on
    /// first fetch (helps the LLM use a high-cardinality pack correctly).
    pub pack_guide_excerpt: Option<String>,
    /// The pack this leaf belongs to (`duckdb` for `duckdb__query`).
    pub pack_name: String,
    /// Capability-owned result policy retained in the immutable server-side
    /// index. It is intentionally absent from provider tool schemas/prompts.
    pub result_projection: Option<ProjectionContractSpec>,
    /// Coarse grouping for the deferred bare-name block (e.g. "memory",
    /// "introspection", "pack"). Static classification.
    pub group: &'static str,
}

/// Process-level index of all known tool leaves.
#[derive(Debug, Clone, Default)]
pub struct ToolIndex {
    by_name: HashMap<String, ToolIndexEntry>,
    by_pack: HashMap<String, Vec<String>>,
    /// Full pack `guide:` text by pack name. Per-leaf entries only carry a short
    /// excerpt; this holds the complete guide so the flat-loop prompt can inject
    /// a pack's whole playbook (e.g. the browser iframe/shadow/drag handling)
    /// once the agent loads that pack's tools — the flat-loop replacement for
    /// the per-pack inner-loop system prompt.
    pack_guides: HashMap<String, String>,
}

impl ToolIndex {
    pub fn from_entries(entries: Vec<ToolIndexEntry>) -> Self {
        let mut by_name = HashMap::new();
        let mut by_pack: HashMap<String, Vec<String>> = HashMap::new();
        for e in entries {
            by_pack
                .entry(e.pack_name.clone())
                .or_default()
                .push(e.name.clone());
            by_name.insert(e.name.clone(), e);
        }
        Self {
            by_name,
            by_pack,
            pack_guides: HashMap::new(),
        }
    }

    /// Full `guide:` text for a pack, if it declared one. Used by the flat-loop
    /// prompt to surface a loaded pack's complete playbook.
    pub fn guide_for_pack(&self, pack_name: &str) -> Option<&str> {
        self.pack_guides.get(pack_name).map(String::as_str)
    }

    pub fn get(&self, name: &str) -> Option<&ToolIndexEntry> {
        self.by_name.get(name)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Leaf names belonging to `pack_name`. For a single-leaf pack this is just
    /// `[pack_name]`; for a multi-primitive pack it's `["<pack>__<action>", …]`.
    pub fn leaf_names_for_pack(&self, pack_name: &str) -> Vec<String> {
        self.by_pack.get(pack_name).cloned().unwrap_or_default()
    }

    /// Deterministic complete pack-name catalog for scope-aware policy
    /// projection and introspection. This stays server-side; callers still
    /// receive only the subset admitted by their agent definition.
    pub fn pack_names(&self) -> Vec<String> {
        let mut names = self.by_pack.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    /// The pack a selected name loads when it is not an exact leaf: the pack's
    /// own name, or the pack head of a `<pack>__<action>` name whose leaf this
    /// index does not carry. A whole-pack surface collapses a pack's leaves
    /// into one entry named after the pack, and a model that learned the leaf
    /// names elsewhere still selects `browser__open` there.
    pub fn pack_for_selected_name(&self, name: &str) -> Option<&str> {
        if let Some((pack, _)) = self.by_pack.get_key_value(name) {
            return Some(pack.as_str());
        }
        let (head, _) = name.split_once("__")?;
        self.by_pack
            .get_key_value(head)
            .map(|(pack, _)| pack.as_str())
    }

    /// Direct fetch: entries for the named leaves, in request order. A name
    /// that is not a leaf resolves through [`Self::pack_for_selected_name`] to
    /// the entries of its pack — what a select of that name loads. Unknown
    /// names are skipped and no entry repeats.
    pub fn select(&self, names: &[String]) -> Vec<&ToolIndexEntry> {
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for name in names {
            let resolved = match self.by_name.get(name) {
                Some(entry) => vec![entry],
                None => self
                    .pack_for_selected_name(name)
                    .map(|pack| self.leaf_names_for_pack(pack))
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|leaf| self.by_name.get(leaf))
                    .collect(),
            };
            for entry in resolved {
                if seen.insert(entry.name.as_str()) {
                    entries.push(entry);
                }
            }
        }
        entries
    }

    /// Keyword search with CC-style weighted scoring. `query` is whitespace-
    /// split into terms; a term prefixed `+` is *required* (pre-filter). The
    /// remaining terms (and the required ones) contribute to the score. Returns
    /// up to `max_results` entries sorted by descending score (ties broken by
    /// name for determinism).
    pub fn search(&self, query: &str, max_results: usize) -> Vec<&ToolIndexEntry> {
        let mut required: Vec<String> = Vec::new();
        let mut terms: Vec<String> = Vec::new();
        for raw in query.split_whitespace() {
            let lower = raw.to_ascii_lowercase();
            if let Some(stripped) = lower.strip_prefix('+') {
                if !stripped.is_empty() {
                    required.push(stripped.to_string());
                }
            } else {
                terms.push(lower);
            }
        }
        // A required term also contributes to scoring.
        let scoring_terms: Vec<&String> = terms.iter().chain(required.iter()).collect();

        let mut scored: Vec<(i32, &ToolIndexEntry)> = self
            .by_name
            .values()
            .filter(|e| required.iter().all(|r| entry_contains(e, r)))
            .map(|e| (score_entry(e, &scoring_terms), e))
            .filter(|(s, _)| *s > 0 || scoring_terms.is_empty())
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored
            .into_iter()
            .take(max_results)
            .map(|(_, e)| e)
            .collect()
    }
}

/// True if any searchable field of the entry contains `needle` (lowercased).
fn entry_contains(e: &ToolIndexEntry, needle: &str) -> bool {
    e.name.to_ascii_lowercase().contains(needle)
        || e.description.to_ascii_lowercase().contains(needle)
        || e.search_hint
            .as_deref()
            .map(|h| h.to_ascii_lowercase().contains(needle))
            .unwrap_or(false)
}

/// CC-style weighted score for one entry against the scoring terms.
fn score_entry(e: &ToolIndexEntry, terms: &[&String]) -> i32 {
    let name_lower = e.name.to_ascii_lowercase();
    // Name parts split on the `__` pack/action boundary and on underscores.
    let name_parts: Vec<&str> = name_lower.split("__").flat_map(|p| p.split('_')).collect();
    let desc_lower = e.description.to_ascii_lowercase();
    let hint_lower = e.search_hint.as_deref().map(|h| h.to_ascii_lowercase());

    let mut score = 0;
    for term in terms {
        let term = term.as_str();
        if name_parts.contains(&term) {
            score += 12; // exact name-part hit
        } else if name_parts.iter().any(|p| p.contains(term)) {
            score += 6; // partial name-part hit
        } else if name_lower.contains(term) {
            score += 3; // full-name fallback
        }
        if let Some(h) = &hint_lower {
            if h.contains(term) {
                score += 4;
            }
        }
        if desc_lower.contains(term) {
            score += 2;
        }
    }
    score
}

/// Default coarse group for a leaf. Phase 1 keeps this simple: a small static
/// map for the well-known universal tools, "pack" for everything else. The
/// deferred bare-name block uses this only for visual grouping.
fn classify_group(pack_name: &str) -> &'static str {
    match pack_name {
        "save_preference" | "update_memory_tier" | "search_memory" | "forget_memory"
        | "list_memory_tiers" => "memory",
        "list_tasks"
        | "list_agents"
        | "inspect_agent"
        | "list_artifacts"
        | "list_scheduled_tasks"
        | "list_episodes"
        | "list_proposals"
        | "get_active_executions"
        | "get_execution_history"
        | "read_program_state"
        | "read_trace"
        | "system_status" => "introspection",
        "stop_task" | "update_task" | "delete_task" | "refine_task" => "task_ops",
        "create_dashboard" | "unpublish_dashboard" => "artifacts",
        "switch_personality" | "activate_skill" | "deactivate_skill" => "identity",
        "need_user_input" | "spawn_sub_goal" | "handover_to_agent" => "control",
        "files" | "read_file" | "write_file" | "edit_file" | "glob" | "grep" => "filesystem",
        "http" | "web_fetch" | "web_search" | "web_answer" => "web",
        "time_math" => "time",
        _ => "pack",
    }
}

/// Build a leaf from its declared parameter subset, retaining pack-level
/// types and constraints unless the action supplies an explicit override.
fn primitive_parameters_schema(
    pack: &CapabilityPackDefinition,
    schema: &NativeActionSchemaDef,
) -> Value {
    let mut properties = serde_json::Map::new();
    for param_name in &schema.parameters {
        let prop = schema
            .parameter_overrides
            .get(param_name)
            .cloned()
            .or_else(|| {
                pack.parameters
                    .iter()
                    .find(|param| &param.name == param_name)
                    .map(derive_param_schema_for_emission)
            })
            .unwrap_or_else(|| serde_json::json!({ "type": "string" }));
        properties.insert(param_name.clone(), prop);
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": schema.required.clone(),
        "additionalProperties": false,
    })
}

/// First ~400 chars of the pack guide, trimmed at a line boundary. Keeps the
/// `tool_search` first-fetch payload bounded.
fn guide_excerpt(guide: &str) -> String {
    const MAX: usize = 400;
    if guide.len() <= MAX {
        return guide.trim().to_string();
    }
    let cut = guide[..MAX].rfind('\n').unwrap_or(MAX);
    guide[..cut].trim().to_string()
}

/// Normalize a pack description into a concise selector hint shared by its
/// leaves. The full action description still explains mechanics after the
/// action is selected; this hint carries only pack-level routing context.
fn pack_search_hint(description: Option<&str>) -> Option<String> {
    const MAX_CHARS: usize = 180;
    let normalized = description?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= MAX_CHARS {
        return Some(normalized);
    }
    let mut truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    if let Some(last_space) = truncated.rfind(' ') {
        truncated.truncate(last_space);
    }
    Some(format!("{truncated}..."))
}

/// Promote one pack definition into its index leaves.
pub fn promote_pack_to_leaves(pack: &CapabilityPackDefinition) -> Vec<ToolIndexEntry> {
    let guide_snippet = pack.guide.as_deref().map(guide_excerpt);
    let search_hint = pack_search_hint(pack.description.as_deref());
    let group = classify_group(&pack.name);

    if pack.native_action_schemas.is_empty() {
        // Single-leaf pack: name == pack name, schema from pack-level params.
        return vec![ToolIndexEntry {
            name: pack.name.clone(),
            description: pack.description.clone().unwrap_or_default(),
            parameters_schema: pack_level_parameters_schema(pack),
            // A single-leaf pack already exposes its pack description as the
            // action description, so a duplicate hint would only double-weight it.
            search_hint: None,
            pack_guide_excerpt: guide_snippet,
            pack_name: pack.name.clone(),
            result_projection: pack.result_projection.clone(),
            group,
        }];
    }

    // Multi-primitive pack: one leaf per native_action_schema.
    let mut leaves = Vec::with_capacity(pack.native_action_schemas.len());
    for (action, schema) in &pack.native_action_schemas {
        let description = schema
            .description
            .clone()
            .or_else(|| pack.description.clone())
            .unwrap_or_default();
        leaves.push(ToolIndexEntry {
            name: format!("{}__{}", pack.name, action),
            description,
            parameters_schema: primitive_parameters_schema(pack, schema),
            search_hint: search_hint.clone(),
            pack_guide_excerpt: guide_snippet.clone(),
            pack_name: pack.name.clone(),
            result_projection: pack.result_projection.clone(),
            group,
        });
    }
    leaves
}

/// JSON Schema for a pack called at the pack boundary: its pack-level
/// `parameters`, the shape `pack_def_to_direct_capability` emits.
fn pack_level_parameters_schema(pack: &CapabilityPackDefinition) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<Value> = Vec::new();
    for param in &pack.parameters {
        properties.insert(param.name.clone(), derive_param_schema_for_emission(param));
        if param.required {
            required.push(Value::String(param.name.clone()));
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// The one tool a whole-pack surface sees for a multi-primitive pack: the pack
/// name, its pack-level parameters, and a description that still names every
/// action so keyword search ranks the pack for the work its leaves do.
fn collapse_pack_to_surface_tool(pack: &CapabilityPackDefinition) -> ToolIndexEntry {
    let mut actions = pack
        .native_action_schemas
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    actions.sort();
    let mut description = pack.description.clone().unwrap_or_default();
    if !description.is_empty() && !description.ends_with('.') {
        description.push('.');
    }
    if !description.is_empty() {
        description.push(' ');
    }
    // A surface that dispatches whole packs hands the call to a sub-run that
    // drives the actions itself. Said plainly, because the pack's own text
    // describes those actions as commands and a model reading "open, read,
    // snapshot" sent them one call at a time — four sub-runs whose goals were
    // `open …`, `snapshot -i`, `read`, none with a page to act on.
    description.push_str(
        "Call this ONCE with the complete objective; a dedicated run performs every step \
         itself (it can ",
    );
    description.push_str(&actions.join(", "));
    description.push_str(") and returns the outcome. Never send one action or command per call.");
    let mut parameters_schema = pack_level_parameters_schema(pack);
    describe_objective_parameter(&mut parameters_schema);
    ToolIndexEntry {
        name: pack.name.clone(),
        description,
        parameters_schema,
        search_hint: pack_search_hint(pack.description.as_deref()),
        pack_guide_excerpt: pack.guide.as_deref().map(guide_excerpt),
        pack_name: pack.name.clone(),
        result_projection: pack.result_projection.clone(),
        group: classify_group(&pack.name),
    }
}

/// The pack-level intent parameter (`command` for CLI packs, or `goal` /
/// `task` / `instruction`) is described as the whole objective on the surface.
/// The loader's own text — "Describe the bounded operation for the inner tool
/// loop" — reads as one operation, and next to a `command` name it reads as a
/// CLI command.
fn describe_objective_parameter(schema: &mut Value) {
    const INTENT_KEYS: [&str; 6] = ["command", "goal", "intent", "task", "instruction", "prompt"];
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    for key in INTENT_KEYS {
        if let Some(Value::Object(property)) = properties.get_mut(key) {
            property.insert(
                "description".to_string(),
                Value::String(
                    "The complete objective for this pack, in plain language — everything the \
                     run must achieve and report, not a single step or command."
                        .to_string(),
                ),
            );
            return;
        }
    }
}

/// Whether a surface that dispatches whole packs still sees this pack's
/// leaves: a declared chat-inline adapter owns the leaf route itself.
fn pack_dispatches_leaves_inline(pack: &CapabilityPackDefinition) -> bool {
    pack.execution
        .as_ref()
        .is_some_and(|metadata| metadata.chat_inline_adapter.is_some())
}

fn pack_guides(packs: &[CapabilityPackDefinition]) -> HashMap<String, String> {
    let mut guides = HashMap::new();
    for pack in packs {
        if let Some(guide) = pack.guide.as_deref() {
            let trimmed = guide.trim();
            if !trimmed.is_empty() {
                guides.insert(pack.name.clone(), trimmed.to_string());
            }
        }
    }
    guides
}

/// Build the full index from every loaded pack definition (embedded + skillshub).
pub fn build_tool_index(packs: &[CapabilityPackDefinition]) -> ToolIndex {
    let entries = packs.iter().flat_map(promote_pack_to_leaves).collect();
    let mut index = ToolIndex::from_entries(entries);
    index.pack_guides = pack_guides(packs);
    index
}

/// Build the index for a surface that dispatches whole packs rather than
/// leaves (Chat, realtime voice): every multi-primitive pack collapses to one
/// pack-named tool carrying its pack-level parameters, so what the model loads
/// with `tool_search` is exactly what `dispatch_capability_pack` can run. A
/// pack that declares a chat-inline adapter keeps its leaves — the adapter
/// dispatches them inline. Single-leaf packs are the same entry as in
/// [`build_tool_index`].
pub fn build_surface_tool_index(packs: &[CapabilityPackDefinition]) -> ToolIndex {
    let entries = packs
        .iter()
        .flat_map(|pack| {
            if pack.native_action_schemas.is_empty() || pack_dispatches_leaves_inline(pack) {
                promote_pack_to_leaves(pack)
            } else {
                vec![collapse_pack_to_surface_tool(pack)]
            }
        })
        .collect();
    let mut index = ToolIndex::from_entries(entries);
    index.pack_guides = pack_guides(packs);
    index
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use serde_json::json;

    fn entry(name: &str, desc: &str) -> ToolIndexEntry {
        ToolIndexEntry {
            name: name.to_string(),
            description: desc.to_string(),
            parameters_schema: json!({"type": "object", "properties": {}}),
            search_hint: None,
            pack_guide_excerpt: None,
            pack_name: name.split("__").next().unwrap_or(name).to_string(),
            result_projection: None,
            group: "pack",
        }
    }

    fn pack_from_yaml(yaml: &str) -> CapabilityPackDefinition {
        serde_yaml::from_str::<CapabilityPackDefinition>(yaml).expect("pack parses")
    }

    const COMPILED_PACK_YAML: &str = r#"
name: save_preference
description: Remember a user preference.
parameters:
  - name: key
    required: true
    param_type: string
    description: preference key
  - name: value
    required: true
    param_type: string
    description: preference value
implementation:
  type: compiled
  provider_name: save_preference
"#;

    const PROJECTED_PACK_YAML: &str = r#"
name: search_memory_fixture
description: Search ranked records.
result_projection:
  contract_id: ranked_records_v1
  records_paths: [/matches]
  priority_fields: [id, relationship, value, status]
  spoken_fields: [voice_summary]
implementation:
  type: compiled
  provider_name: search_memory
"#;

    const INNER_LOOP_PACK_YAML: &str = r#"
name: duckdb
description: Query data with DuckDB.
guide: |
  Use duckdb__query for SQL.
parameters:
  - name: sql
    required: false
    param_type: string
native_action_schemas:
  query:
    description: Execute one DuckDB SQL statement.
    parameters: [sql, output_format]
    required: [sql]
    parameter_overrides:
      sql: { type: string, description: "SQL to run" }
      output_format: { type: string, enum: [json, csv, table], default: json }
  preview:
    description: Preview first N rows of a file.
    parameters: [source, limit]
    required: [source]
    parameter_overrides:
      source: { type: string }
      limit: { type: integer, default: 10 }
implementation:
  type: primitive
  provider_name: duckdb
"#;

    #[test]
    fn index_looks_up_by_exact_name() {
        let idx = ToolIndex::from_entries(vec![
            entry("duckdb__query", "run sql"),
            entry("save_preference", "remember a preference"),
        ]);
        assert!(idx.get("duckdb__query").is_some());
        assert!(idx.get("missing").is_none());
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn index_groups_leaves_by_pack() {
        let idx = ToolIndex::from_entries(vec![
            entry("duckdb__query", "q"),
            entry("duckdb__preview", "p"),
            entry("save_preference", "s"),
        ]);
        let mut duck = idx.leaf_names_for_pack("duckdb");
        duck.sort();
        assert_eq!(duck, vec!["duckdb__preview", "duckdb__query"]);
        assert_eq!(
            idx.leaf_names_for_pack("save_preference"),
            vec!["save_preference"]
        );
        assert!(idx.leaf_names_for_pack("nonexistent").is_empty());
    }

    #[test]
    fn pack_names_are_complete_unique_and_deterministic() {
        let idx = ToolIndex::from_entries(vec![
            entry("zeta__query", "q"),
            entry("alpha", "a"),
            entry("zeta__preview", "p"),
        ]);

        assert_eq!(idx.pack_names(), vec!["alpha", "zeta"]);
        assert_eq!(idx.pack_names(), idx.pack_names());
    }

    #[test]
    fn compiled_pack_promotes_to_single_leaf_named_after_pack() {
        let pack = pack_from_yaml(COMPILED_PACK_YAML);
        let leaves = promote_pack_to_leaves(&pack);
        assert_eq!(leaves.len(), 1);
        let leaf = &leaves[0];
        assert_eq!(leaf.name, "save_preference");
        assert_eq!(leaf.pack_name, "save_preference");
        assert!(leaf.search_hint.is_none());
        let props = leaf.parameters_schema.get("properties").unwrap();
        assert!(props.get("key").is_some());
        assert!(props.get("value").is_some());
    }

    #[test]
    fn projection_contract_metadata_survives_pack_promotion_without_prompt_exposure() {
        let pack = pack_from_yaml(PROJECTED_PACK_YAML);
        let leaves = promote_pack_to_leaves(&pack);
        let contract = leaves[0]
            .result_projection
            .as_ref()
            .expect("projection policy survives in server-side index");
        assert_eq!(contract.contract_id.as_str(), "ranked_records_v1");
        assert!(contract
            .records_paths
            .iter()
            .any(|path| path.as_str() == "/matches"));
        assert_eq!(contract.spoken_fields, vec!["voice_summary"]);
        let provider_schema = serde_json::to_string(&leaves[0].parameters_schema)
            .expect("provider schema serializes");
        assert!(!provider_schema.contains("result_projection"));
        assert!(!provider_schema.contains("ranked_records_v1"));
    }

    #[test]
    fn primitive_pack_promotes_to_prefixed_primitive_leaves() {
        let pack = pack_from_yaml(INNER_LOOP_PACK_YAML);
        let mut leaves = promote_pack_to_leaves(&pack);
        leaves.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].name, "duckdb__preview");
        assert_eq!(leaves[1].name, "duckdb__query");
        assert!(leaves.iter().all(|l| l.pack_name == "duckdb"));
        assert!(leaves
            .iter()
            .all(|leaf| leaf.search_hint.as_deref() == Some("Query data with DuckDB.")));
        assert!(leaves[1]
            .pack_guide_excerpt
            .as_deref()
            .unwrap()
            .contains("duckdb__query"));
        let q = &leaves[1].parameters_schema;
        assert_eq!(q["properties"]["output_format"]["default"], json!("json"));
        assert_eq!(q["required"], json!(["sql"]));
        assert_eq!(q["additionalProperties"], json!(false));
    }

    #[test]
    fn scoped_map_leaf_preserves_the_pack_integer_and_lifecycle_schemas() {
        let pack = pack_from_yaml(include_str!(
            "../embedded_pack_defs/thinking_maps_data.yaml"
        ));
        let leaves = promote_pack_to_leaves(&pack);
        let list = leaves
            .iter()
            .find(|leaf| leaf.name == "thinking_maps_data__list_maps")
            .unwrap();
        let props = &list.parameters_schema["properties"];
        assert_eq!(props["limit"]["type"], json!("integer"));
        assert_eq!(
            props["lifecycle"]["enum"],
            json!(["active", "paused", "archived", "deleted"])
        );
        assert!(props.get("map_id").is_none());
        assert_eq!(list.parameters_schema["required"], json!([]));
        assert_eq!(list.parameters_schema["additionalProperties"], json!(false));
    }

    #[test]
    fn leaf_overrides_precede_canonical_pack_schemas_without_exposing_extra_parameters() {
        let mut pack = pack_from_yaml(INNER_LOOP_PACK_YAML);
        let mut parameter = pack_from_yaml(include_str!(
            "../embedded_pack_defs/thinking_maps_data.yaml"
        ))
        .parameters
        .into_iter()
        .find(|param| param.name == "limit")
        .unwrap();
        parameter.schema = json!({"type": "integer", "minimum": 1, "maximum": 25});
        pack.parameters.push(parameter);
        let leaves = promote_pack_to_leaves(&pack);
        let preview = leaves
            .iter()
            .find(|leaf| leaf.name == "duckdb__preview")
            .unwrap();
        assert_eq!(
            preview.parameters_schema["properties"]["limit"],
            json!({"type": "integer", "default": 10})
        );
        let query = leaves
            .iter()
            .find(|leaf| leaf.name == "duckdb__query")
            .unwrap();
        assert!(query.parameters_schema["properties"].get("limit").is_none());
    }

    #[test]
    fn build_tool_index_indexes_all_packs() {
        let packs = vec![
            pack_from_yaml(COMPILED_PACK_YAML),
            pack_from_yaml(INNER_LOOP_PACK_YAML),
        ];
        let idx = build_tool_index(&packs);
        assert!(idx.get("save_preference").is_some());
        assert!(idx.get("duckdb__query").is_some());
        assert!(idx.get("duckdb__preview").is_some());
        assert_eq!(idx.len(), 3);
        let mut duck = idx.leaf_names_for_pack("duckdb");
        duck.sort();
        assert_eq!(duck, vec!["duckdb__preview", "duckdb__query"]);
    }

    #[test]
    fn select_returns_named_leaves_in_order_skipping_unknown() {
        let idx = build_tool_index(&[pack_from_yaml(INNER_LOOP_PACK_YAML)]);
        let got = idx.select(&[
            "duckdb__query".into(),
            "nope".into(),
            "duckdb__preview".into(),
        ]);
        let names: Vec<_> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["duckdb__query", "duckdb__preview"]);
    }

    /// A model that learned the leaf names from the executor's catalog still
    /// asks for `duckdb__query` on a whole-pack surface, where the pack is one
    /// entry named `duckdb`. On 2026-09-20 the chat mouth selected seven
    /// `browser__*` leaves, got `matches: []` plus "these tools are now
    /// available", and fell back to `http` — MakeMyTrip "unreachable".
    #[test]
    fn selecting_a_leaf_a_surface_index_collapsed_resolves_to_its_pack_tool() {
        let idx = build_surface_tool_index(&[pack_from_yaml(INNER_LOOP_PACK_YAML)]);
        let got = idx.select(&[
            "duckdb__query".into(),
            "duckdb__preview".into(),
            "nope__query".into(),
        ]);
        let names: Vec<_> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["duckdb"],
            "one collapsed entry, once, unknown pack skipped"
        );
    }

    /// On the leaf index the same resolution makes a pack-name select return
    /// the leaves it loads instead of nothing.
    #[test]
    fn selecting_a_pack_name_on_the_leaf_index_returns_its_leaves() {
        let idx = build_tool_index(&[pack_from_yaml(INNER_LOOP_PACK_YAML)]);
        let got = idx.select(&["duckdb".into()]);
        let mut names: Vec<_> = got.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["duckdb__preview", "duckdb__query"]);
    }

    #[test]
    fn search_ranks_name_matches_above_description_matches() {
        let idx = build_tool_index(&[
            pack_from_yaml(COMPILED_PACK_YAML),
            pack_from_yaml(INNER_LOOP_PACK_YAML),
        ]);
        let hits = idx.search("query", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].name, "duckdb__query");
    }

    #[test]
    fn search_required_term_filters_out_non_matches() {
        let idx = build_tool_index(&[
            pack_from_yaml(COMPILED_PACK_YAML),
            pack_from_yaml(INNER_LOOP_PACK_YAML),
        ]);
        let hits = idx.search("+preview source", 5);
        assert!(hits.iter().all(|e| e.name.contains("preview")));
        assert!(hits.iter().any(|e| e.name == "duckdb__preview"));
    }

    #[test]
    fn search_respects_max_results() {
        let idx = build_tool_index(&[pack_from_yaml(INNER_LOOP_PACK_YAML)]);
        let hits = idx.search("duckdb", 1);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn index_builds_over_all_embedded_packs_without_panic() {
        let packs =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs();
        let idx = build_tool_index(&packs);
        assert!(idx.get("save_preference").is_some());
        assert!(idx.get("shell").is_some());
        let duck = idx.leaf_names_for_pack("duckdb");
        assert!(
            duck.iter().any(|n| n == "duckdb__query"),
            "duckdb primitives promoted: {duck:?}"
        );
    }

    const CHAT_INLINE_ADAPTER_PACK_YAML: &str = r#"
name: screen-draw
description: Draw on the shared screen overlay.
native_action_schemas:
  draw:
    description: Draw one shape.
    parameters: [shape_json]
    required: [shape_json]
    parameter_overrides:
      shape_json: { type: string }
  clear:
    description: Clear the overlay.
    parameters: []
    required: []
execution:
  chat_inline_adapter: tutor_screen_draw
implementation:
  type: primitive
  provider_name: screen_draw
"#;

    /// A surface that dispatches whole packs — Chat hands a pack call to a
    /// task-backed sub-run and cannot execute a single leaf — must see the
    /// pack as ONE callable tool carrying the pack-level parameters, not the
    /// executor's per-action leaves. Otherwise the model loads `duckdb__query`,
    /// calls it, and the pack dispatcher spawns a run scoped to a pack named
    /// `duckdb__query`, which exists nowhere.
    #[test]
    fn a_surface_index_collapses_a_multi_leaf_pack_to_its_pack_tool() {
        let pack = pack_from_yaml(INNER_LOOP_PACK_YAML);
        let idx = build_surface_tool_index(&[pack]);

        assert!(
            idx.get("duckdb__query").is_none(),
            "leaves must not be callable on the surface"
        );
        assert_eq!(
            idx.leaf_names_for_pack("duckdb"),
            vec!["duckdb".to_string()]
        );
        let entry = idx.get("duckdb").expect("the pack is the surface tool");
        assert_eq!(entry.pack_name, "duckdb");
        assert_eq!(
            entry.parameters_schema["properties"]["sql"]["type"],
            serde_json::json!("string"),
            "the pack-level parameters are the tool's schema: {}",
            entry.parameters_schema
        );
        assert!(
            entry.description.contains("query") && entry.description.contains("preview"),
            "the collapsed tool names the actions it can run so keyword search still finds them: {}",
            entry.description
        );
        assert!(
            entry
                .description
                .contains("ONCE with the complete objective")
                && entry
                    .description
                    .contains("Never send one action or command per call"),
            "the surface tool is a delegation of the whole objective, not a command line: {}",
            entry.description
        );
        assert_eq!(
            idx.guide_for_pack("duckdb"),
            Some("Use duckdb__query for SQL.")
        );
    }

    /// A CLI pack's pack-level `command` parameter is described by the loader
    /// as "the bounded operation for the inner tool loop"; next to the name
    /// `command` a chat model read it as a CLI command and sent
    /// `open https://…`, then `snapshot -i`, then `read` — one sub-run each.
    #[test]
    fn a_surface_tools_intent_parameter_asks_for_the_whole_objective() {
        let pack = pack_from_yaml(
            r#"
name: browser
description: Drive a browser to accomplish the current task.
parameters:
  - name: command
    required: true
    param_type: string
    description: Describe the bounded operation for the inner tool loop.
  - name: timeout_secs
    required: false
    param_type: integer
native_action_schemas:
  open:
    description: Open a URL.
    parameters: [url]
    required: [url]
  snapshot:
    description: Snapshot the page.
    parameters: []
    required: []
implementation:
  type: primitive
"#,
        );
        let idx = build_surface_tool_index(&[pack]);
        let entry = idx.get("browser").expect("pack tool");
        let command = &entry.parameters_schema["properties"]["command"]["description"];
        assert!(
            command
                .as_str()
                .unwrap_or("")
                .contains("complete objective"),
            "{command}"
        );
        assert!(
            !command.as_str().unwrap_or("").contains("bounded operation"),
            "{command}"
        );
        assert_eq!(
            entry.parameters_schema["properties"]["timeout_secs"]["description"],
            Value::Null,
            "other parameters keep their own text"
        );
    }

    /// A pack that declares a chat-inline adapter IS dispatched leaf by leaf
    /// from Chat (the adapter owns the route), so the surface keeps its leaves.
    #[test]
    fn a_chat_inline_adapter_pack_keeps_its_leaves_on_the_surface() {
        let pack = pack_from_yaml(CHAT_INLINE_ADAPTER_PACK_YAML);
        let idx = build_surface_tool_index(&[pack]);

        assert!(idx.get("screen-draw__draw").is_some());
        assert!(idx.get("screen-draw__clear").is_some());
        assert!(idx.get("screen-draw").is_none());
    }

    /// Single-leaf packs are already one tool; the surface index is the same
    /// entry the executor sees.
    #[test]
    fn a_single_leaf_pack_is_unchanged_on_the_surface() {
        let pack = pack_from_yaml(COMPILED_PACK_YAML);
        let surface = build_surface_tool_index(std::slice::from_ref(&pack));
        let full = build_tool_index(&[pack]);
        assert_eq!(surface.get("save_preference"), full.get("save_preference"));
    }
}
