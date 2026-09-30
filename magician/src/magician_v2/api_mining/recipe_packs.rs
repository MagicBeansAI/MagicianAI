//! Planner-visible capability records for Candidate+ task recipes.

use super::capability::SideEffects;
use super::recipe::{RecipeMaturity, TaskInputSchema, TaskRecipe};
use crate::magician_v2::execution::capability::{
    CapabilityPackDefinition, CapabilityReliabilityMetadata, ExecutionMetadata, ImplementationType,
    ParameterDef, ParameterType,
};
use crate::magician_v2::execution::capability_eval::CapabilityLifecycleStatus;
use crate::magician_v2::execution::capability_pack::{
    CapabilityPackMetadata, CapabilityPackRecord, CapabilityPackSource, CapabilityPackStore,
};
use std::collections::HashMap;
use std::collections::HashSet;

pub const PROVIDER_NAME: &str = "replay_recipe";

pub fn is_reserved_input_name(name: &str) -> bool {
    // GenericCompiledProvider injects timeout_secs even when the model omits
    // it. A task parameter with that name must not inherit the pack default.
    matches!(name, "recipe_id" | "inputs" | "timeout_secs") || name.starts_with("__")
}

pub fn recipe_to_pack_definition(recipe: &TaskRecipe) -> Option<CapabilityPackDefinition> {
    let version = recipe.current()?;
    if version.maturity == RecipeMaturity::Draft {
        return None;
    }
    if version.steps.is_empty()
        || version.answer_spec.is_empty()
        || version
            .steps
            .iter()
            .any(|step| step.side_effects == SideEffects::Unknown)
    {
        return None;
    }
    let validation_inputs = super::recipe_runner::RecipeRunInputs {
        inputs: recipe
            .shape
            .inputs
            .iter()
            .map(|input| (input.name.clone(), input.example_value.clone()))
            .collect(),
        timeout_ms: None,
        approved_write_steps: HashSet::new(),
    };
    if super::recipe_runner::validate_recipe_preflight(recipe, &validation_inputs).is_err() {
        return None;
    }
    // A planner may hold an older schema while compilation publishes a new
    // graph. Bind the invoked alias to the exact version, not merely the id.
    let recipe_digest =
        blake3::hash(format!("{}\0{}", recipe.id, version.version).as_bytes()).to_hex();
    let short = &recipe_digest[..16];
    let nested_only = recipe
        .shape
        .inputs
        .iter()
        .any(|input| is_reserved_input_name(&input.name));
    let mut parameters: Vec<ParameterDef> = recipe
        .shape
        .inputs
        .iter()
        .filter(|_| !nested_only)
        .map(|input| {
            let param_type = match input.schema {
                TaskInputSchema::String => ParameterType::String,
                TaskInputSchema::Number => ParameterType::Number,
                TaskInputSchema::Boolean => ParameterType::Boolean,
            };
            ParameterDef {
                name: input.name.clone(),
                required: true,
                default: None,
                // Do not publish captured example values into a generated
                // planner schema. The name and scalar type are sufficient,
                // while examples would resend prior task data to every later
                // planning request that includes this tool.
                description: Some(format!(
                    "Value for {{{}}} in '{}'",
                    input.name, recipe.shape.template
                )),
                param_type: Some(param_type.clone()),
                aliases: Vec::new(),
                enum_values: None,
                schema: serde_json::json!({
                    "type": match param_type {
                        ParameterType::Number => "number",
                        ParameterType::Boolean => "boolean",
                        _ => "string",
                    }
                }),
            }
        })
        .collect();
    parameters.push(ParameterDef {
        name: "recipe_id".into(),
        required: false,
        default: Some(recipe.id.clone()),
        description: Some("Server-bound recipe id; do not override.".into()),
        param_type: Some(ParameterType::String),
        aliases: Vec::new(),
        enum_values: None,
        schema: serde_json::json!({"type": "string", "default": recipe.id}),
    });
    // The handler accepts a nested `inputs` object. Per-recipe tools expose
    // friendly top-level inputs, which the handler normalizes below.
    parameters.push(ParameterDef {
        name: "inputs".into(),
        required: nested_only,
        default: None,
        description: Some(if nested_only {
            "Required task input object. Task field names are nested to avoid runtime control arguments.".into()
        } else {
            "Optional explicit input object; top-level named inputs are also accepted.".into()
        }),
        param_type: Some(ParameterType::Object),
        aliases: Vec::new(),
        enum_values: None,
        schema: if nested_only {
            let properties: serde_json::Map<String, serde_json::Value> = recipe.shape.inputs.iter().map(|input| {
                (input.name.clone(), serde_json::json!({"type": match input.schema {
                    TaskInputSchema::String => "string",
                    TaskInputSchema::Number => "number",
                    TaskInputSchema::Boolean => "boolean",
                }}))
            }).collect();
            serde_json::json!({
                "type": "object", "properties": properties,
                "required": recipe.shape.inputs.iter().map(|input| &input.name).collect::<Vec<_>>(),
                "additionalProperties": false,
            })
        } else {
            serde_json::json!({"type": "object", "additionalProperties": true})
        },
    });

    let is_read_only = recipe.is_read_only();
    Some(CapabilityPackDefinition {
        // OpenAI-compatible tool names are capped at 64 characters:
        // `recipe__` (8) + intent (39) + `_` (1) + digest (16).
        name: format!("recipe__{}_{}", slug(&recipe.shape.template), short),
        description: Some(format!(
            "Learned no-browser API recipe for '{}' ({} steps across {} origin(s)){}",
            recipe.shape.template,
            version.steps.len(),
            version.origins.len(),
            if recipe.has_write_steps() {
                "; write steps require a scoped replay grant"
            } else {
                ""
            }
        )),
        version: Some(format!("1.{}.0", version.version)),
        guide: Some(format!(
            "Runs learned recipe {}. Supply the named task inputs; the runtime extracts the final answer without opening a browser. Read failures may fall back. A write_outcome_uncertain result is terminal and must never be retried through an API or browser.",
            recipe.id,
        )),
        native_action_schemas: HashMap::new(),
        parameters,
        implementation: ImplementationType::Compiled {
            provider_name: PROVIDER_NAME.into(),
        },
        execution: Some(ExecutionMetadata {
            requires_browser_session: Some(false),
            default_timeout_secs: Some(60),
            chat_inline_adapter: None,
            categories: vec!["api-mining".into(), "recipe".into(), "learned".into()],
            sandbox: Some("none".into()),
            composition_category: Some("action".into()),
            spend: None,
        }),
        auth: None,
        reliability: Some(CapabilityReliabilityMetadata {
            read_only: is_read_only,
            idempotent: is_read_only,
            ..Default::default()
        }),
        result_projection: None,
    })
}

pub fn recipe_to_pack_record(recipe: &TaskRecipe) -> Option<CapabilityPackRecord> {
    let version = recipe.current()?;
    let definition = recipe_to_pack_definition(recipe)?;
    let status = match version.maturity {
        RecipeMaturity::Draft => return None,
        RecipeMaturity::Candidate => CapabilityLifecycleStatus::Trial,
        RecipeMaturity::Validated => CapabilityLifecycleStatus::Validated,
        RecipeMaturity::Trusted => CapabilityLifecycleStatus::Trusted,
    };
    let now = chrono::Utc::now().timestamp();
    let attempts = version
        .replay_stats
        .successful_replays
        .saturating_add(version.replay_stats.failed_replays);
    let score = if attempts == 0 {
        0.0
    } else {
        version.replay_stats.successful_replays as f64 / attempts as f64
    };
    Some(CapabilityPackRecord {
        definition,
        metadata: CapabilityPackMetadata {
            source: CapabilityPackSource::TaskRecipe,
            status,
            attempts,
            successes: version.replay_stats.successful_replays,
            last_score: score,
            source_ref: Some(recipe.id.clone()),
            created_at: now,
            updated_at: now,
        },
    })
}

pub fn publish_recipe_pack(
    store: &CapabilityPackStore,
    scope_skills_root: &std::path::Path,
    recipe: &TaskRecipe,
) -> Result<bool, String> {
    let Some(record) = recipe_to_pack_record(recipe) else {
        // Publication is a synchronization boundary, not an insert-only
        // helper. A newly recompiled Draft or a demoted/invalid current
        // version must not leave an older Candidate+ tool callable from the
        // planner catalog. The catalog is authoritative; skill cleanup is
        // best-effort after that fail-closed removal.
        let recipe_ids = HashSet::from([recipe.id.clone()]);
        let removed =
            store.remove_source_records(CapabilityPackSource::TaskRecipe, Some(&recipe_ids))?;
        for stale in &removed {
            if let Err(error) =
                super::skill_emitter::remove_evolved_skill_for_record(scope_skills_root, stale)
            {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    pack_name = %stale.definition.name,
                    %error,
                    "task recipe pack was unpublished but its derived skill could not be removed"
                );
            }
        }
        return Ok(!removed.is_empty());
    };
    // Naming rules are versioned implementation detail. If an older binary
    // emitted a different alias for this recipe, remove that source-owned row
    // and skill before publishing the canonical name. Avoid doing this on the
    // normal replay path so pack refresh remains one catalog write.
    let has_stale_name = store.load_catalog()?.packs.iter().any(|candidate| {
        candidate.metadata.source == CapabilityPackSource::TaskRecipe
            && candidate.metadata.source_ref.as_deref() == Some(recipe.id.as_str())
            && candidate.definition.name != record.definition.name
    });
    if has_stale_name {
        let recipe_ids = HashSet::from([recipe.id.clone()]);
        let stale =
            store.remove_source_records(CapabilityPackSource::TaskRecipe, Some(&recipe_ids))?;
        for old in &stale {
            if let Err(error) =
                super::skill_emitter::remove_evolved_skill_for_record(scope_skills_root, old)
            {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    pack_name = %old.definition.name,
                    %error,
                    "stale task recipe alias was unpublished but its emitted skill could not be removed"
                );
            }
        }
    }
    store.upsert_record(
        record.clone(),
        format!("task recipe {} reached Candidate+", recipe.id),
    )?;
    match super::skill_emitter::emit_evolved_skill(scope_skills_root, &record) {
        super::skill_emitter::EmitOutcome::Failed { reason } => Err(reason),
        _ => Ok(true),
    }
}

/// Remove planner catalog rows and emitted skill folders for deleted recipes.
/// The catalog commit happens first, so a skill cleanup failure leaves the
/// runtime safe (tool absent) and is reported for operator follow-up.
pub fn purge_recipe_packs(
    store: &CapabilityPackStore,
    scope_skills_root: &std::path::Path,
    recipe_ids: Option<&HashSet<String>>,
) -> Result<usize, String> {
    let removed = store.remove_source_records(CapabilityPackSource::TaskRecipe, recipe_ids)?;
    let mut errors = Vec::new();
    for record in &removed {
        if let Err(error) =
            super::skill_emitter::remove_evolved_skill_for_record(scope_skills_root, record)
        {
            errors.push(format!("{}: {error}", record.definition.name));
        }
    }
    if errors.is_empty() {
        Ok(removed.len())
    } else {
        Err(format!(
            "recipe pack catalog was cleaned but generated skill cleanup failed: {}",
            errors.join("; ")
        ))
    }
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(39));
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            out.push(character);
            separator = false;
        } else {
            separator = true;
        }
        if out.len() >= 39 {
            break;
        }
    }
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::recipe::{
        request_shape_fingerprint, AnswerField, CompiledFrom, Extractor, RecipeAuth, RecipeShape,
        RecipeStep, RecipeVersion,
    };
    use crate::magician_v2::api_mining::workflow::{ReplayStats, WorkflowMaturity};
    use tempfile::TempDir;

    fn candidate_recipe(id: &str) -> TaskRecipe {
        TaskRecipe {
            id: id.into(),
            scope_principal: "anonymous".into(),
            scope_workspace: "default".into(),
            agent_id: "personal-assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "find items".into(),
                fingerprint: "shape".into(),
                inputs: Vec::new(),
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec!["https://api.example".into()],
                steps: vec![RecipeStep {
                    id: "s0".into(),
                    origin: "https://api.example".into(),
                    method: "GET".into(),
                    url_template: "https://api.example/items".into(),
                    headers_template: HashMap::new(),
                    body_template: None,
                    capability_id: None,
                    param_sources: HashMap::new(),
                    body_param_types: HashMap::new(),
                    side_effects: SideEffects::ReadOnly,
                    request_shape_fingerprint: request_shape_fingerprint(
                        "GET",
                        "https://api.example/items",
                        None,
                    ),
                    verify_with: None,
                    browser_fallback: None,
                    transport_hint: None,
                }],
                data_flows: Vec::new(),
                answer_spec: vec![AnswerField {
                    field: "items".into(),
                    step_id: "s0".into(),
                    extractor: Extractor::JsonPath {
                        path: "$.items".into(),
                    },
                }],
                auth: RecipeAuth::default(),
                maturity: WorkflowMaturity::Candidate,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task".into(),
                    execution_id: "execution".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn purge_recipe_packs_removes_catalog_record_and_emitted_skill() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityPackStore::with_base_path(temp.path());
        let skills = temp.path().join("skills");
        let recipe = candidate_recipe("rcp_purge");
        assert!(publish_recipe_pack(&store, &skills, &recipe).unwrap());
        let emitted = std::fs::read_dir(skills.join("evolved"))
            .unwrap()
            .flatten()
            .count();
        assert_eq!(emitted, 1);
        let emitted_dir = std::fs::read_dir(skills.join("evolved"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let run_script = std::fs::read_to_string(emitted_dir.join("scripts/run.sh")).unwrap();
        assert!(run_script.contains("published_only=true"));
        assert!(run_script.contains("expected_version=1"));

        let ids = HashSet::from([recipe.id.clone()]);
        assert_eq!(purge_recipe_packs(&store, &skills, Some(&ids)).unwrap(), 1);
        assert!(store.load_catalog().unwrap().packs.is_empty());
        assert_eq!(
            std::fs::read_dir(skills.join("evolved"))
                .unwrap()
                .flatten()
                .count(),
            0
        );
    }

    #[test]
    fn selective_pack_purge_preserves_unrelated_recipe_tool_and_skill() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityPackStore::with_base_path(temp.path());
        let skills = temp.path().join("skills");
        let removed_recipe = candidate_recipe("rcp_removed");
        let kept_recipe = candidate_recipe("rcp_kept");
        let removed_name = recipe_to_pack_definition(&removed_recipe).unwrap().name;
        let kept_name = recipe_to_pack_definition(&kept_recipe).unwrap().name;
        publish_recipe_pack(&store, &skills, &removed_recipe).unwrap();
        publish_recipe_pack(&store, &skills, &kept_recipe).unwrap();

        let ids = HashSet::from([removed_recipe.id.clone()]);
        assert_eq!(purge_recipe_packs(&store, &skills, Some(&ids)).unwrap(), 1);

        let catalog = store.load_catalog().unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].definition.name, kept_name);
        assert!(!skills.join("evolved").join(removed_name).exists());
        assert!(skills.join("evolved").join(kept_name).exists());
    }

    #[test]
    fn malformed_or_unknown_recipes_never_publish_planner_tools() {
        let mut recipe = candidate_recipe("rcp_invalid");
        recipe.current_mut().unwrap().steps.clear();
        assert!(recipe_to_pack_record(&recipe).is_none());

        recipe = candidate_recipe("rcp_unknown");
        recipe.current_mut().unwrap().steps[0].side_effects = SideEffects::Unknown;
        assert!(recipe_to_pack_record(&recipe).is_none());
    }

    #[test]
    fn generated_pack_names_are_bounded_and_bind_the_full_recipe_id() {
        let mut first = candidate_recipe("rcp_prefix_one_shared_suffix");
        first.shape.template = "a very long intent fragment ".repeat(8);
        let mut second = first.clone();
        second.id = "rcp_prefix_two_shared_suffix".into();

        let first_name = recipe_to_pack_definition(&first).unwrap().name;
        let second_name = recipe_to_pack_definition(&second).unwrap().name;

        assert!(first_name.len() <= 64);
        assert!(second_name.len() <= 64);
        assert_ne!(first_name, second_name);
    }

    #[test]
    fn recompiled_graph_gets_a_new_tool_identity_and_retires_the_old_alias() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityPackStore::with_base_path(temp.path());
        let skills = temp.path().join("skills");
        let mut recipe = candidate_recipe("rcp_versioned");
        let old_name = recipe_to_pack_definition(&recipe).unwrap().name;
        publish_recipe_pack(&store, &skills, &recipe).unwrap();
        recipe.current_mut().unwrap().version = 2;
        recipe.current_version = 2;
        let new_name = recipe_to_pack_definition(&recipe).unwrap().name;
        assert_ne!(old_name, new_name);
        publish_recipe_pack(&store, &skills, &recipe).unwrap();
        let catalog = store.load_catalog().unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].definition.name, new_name);
        assert!(!skills.join("evolved").join(old_name).exists());
    }

    #[test]
    fn reserved_task_inputs_are_nested_without_duplicate_control_parameters() {
        use crate::magician_v2::api_mining::recipe::{TaskInput, TaskInputSource};
        for name in ["recipe_id", "inputs", "timeout_secs", "__principal"] {
            let mut recipe = candidate_recipe("rcp_reserved");
            recipe.shape.inputs.push(TaskInput {
                name: name.into(),
                schema: TaskInputSchema::String,
                example_value: "private-task-value".into(),
                source: TaskInputSource::TaskText,
            });
            let step = &mut recipe.current_mut().unwrap().steps[0];
            step.url_template = format!("https://api.example/items?{name}={{{name}}}");
            step.param_sources.insert(
                name.into(),
                super::super::recipe::RecipeParamSource::TaskInput { name: name.into() },
            );
            let definition = recipe_to_pack_definition(&recipe).unwrap();
            let names: Vec<_> = definition
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect();
            assert_eq!(names, vec!["recipe_id", "inputs"]);
            let inputs = &definition.parameters[1];
            assert!(inputs.required);
            assert_eq!(inputs.schema["properties"][name]["type"], "string");
            assert_eq!(inputs.schema["required"], serde_json::json!([name]));
            assert!(!serde_json::to_string(&definition)
                .unwrap()
                .contains("private-task-value"));
        }
    }

    #[test]
    fn publication_replaces_a_stale_alias_owned_by_the_same_recipe() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityPackStore::with_base_path(temp.path());
        let skills = temp.path().join("skills");
        let recipe = candidate_recipe("rcp_alias_migration");
        let canonical_name = recipe_to_pack_definition(&recipe).unwrap().name;
        let mut stale = recipe_to_pack_record(&recipe).unwrap();
        stale.definition.name = "recipe__legacy_alias".into();
        store
            .upsert_record(stale.clone(), "legacy fixture")
            .unwrap();
        assert!(matches!(
            super::super::skill_emitter::emit_evolved_skill(&skills, &stale),
            super::super::skill_emitter::EmitOutcome::Emitted { .. }
        ));

        assert!(publish_recipe_pack(&store, &skills, &recipe).unwrap());

        let catalog = store.load_catalog().unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].definition.name, canonical_name);
        assert!(!skills.join("evolved/recipe__legacy_alias").exists());
    }

    #[test]
    fn publishing_a_draft_unpublishes_its_stale_candidate_tool_and_skill() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityPackStore::with_base_path(temp.path());
        let skills = temp.path().join("skills");
        let mut recipe = candidate_recipe("rcp_demoted");
        assert!(publish_recipe_pack(&store, &skills, &recipe).unwrap());
        assert_eq!(store.load_catalog().unwrap().packs.len(), 1);

        recipe.current_mut().unwrap().maturity = WorkflowMaturity::Draft;
        assert!(publish_recipe_pack(&store, &skills, &recipe).unwrap());
        assert!(store.load_catalog().unwrap().packs.is_empty());
        assert_eq!(
            std::fs::read_dir(skills.join("evolved"))
                .unwrap()
                .flatten()
                .count(),
            0
        );
    }
}
