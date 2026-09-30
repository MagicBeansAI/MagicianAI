//! Task-start recipe lookup: task id, deterministic template, then LLM confirm.

use super::recipe::{TaskInputSchema, TaskRecipe};
use super::recipe_compiler::shape::{
    source_task_fingerprint, template_captures, template_input_names, template_regex,
};
use super::recipe_store::RecipeStore;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};
use magician_core::prompts::PromptManager;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct TaskShapeQuery<'a> {
    pub task_id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub agent_id: &'a str,
    pub principal: &'a str,
    pub workspace: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MatchKind {
    TaskId,
    Template,
    LlmConfirmed(f32),
}

#[derive(Debug, Clone)]
pub struct RecipeMatchResult {
    pub recipe: TaskRecipe,
    pub kind: MatchKind,
    pub inputs: HashMap<String, String>,
}

pub struct RecipeMatcher<'a> {
    store: &'a RecipeStore,
    router: Option<Arc<OperationLlmRouter>>,
    prompt_manager: Option<Arc<PromptManager>>,
    confirm_threshold: f32,
}

pub fn eligible_for_fuzzy(recipe: &TaskRecipe) -> bool {
    let mut names = HashSet::new();
    recipe.is_read_only()
        && recipe
            .current()
            .is_some_and(|version| !version.answer_spec.is_empty())
        // A fuzzy reply supplies only one value per input and cannot prove
        // that repeated task occurrences still agree. Keep these recipes on
        // the deterministic rung, including slots shared across title/body.
        && !std::iter::once(recipe.shape.template.as_str())
            .chain(recipe.shape.description_template.as_deref())
            .flat_map(template_input_names)
            .any(|name| !names.insert(name))
}

/// Shape-only payload for the fuzzy confirmation rung. Keeping construction in
/// one pure function makes the privacy boundary directly testable: candidates
/// contribute identifiers, templates and input schemas, never traces, example
/// values, headers, bodies, auth material or prior answers.
pub fn serialize_match_payload(
    title: &str,
    description: &str,
    candidates: &[&TaskRecipe],
) -> String {
    serde_json::json!({
        "task": {"title": title, "description": description},
        "candidates": candidates.iter().map(|recipe| serde_json::json!({
            "recipe_id": recipe.id,
            "template": recipe.shape.template,
            "description_template": recipe.shape.description_template,
            "inputs": recipe.shape.inputs.iter().map(|input| serde_json::json!({
                "name": input.name,
                "schema": input.schema,
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
    .to_string()
}

impl<'a> RecipeMatcher<'a> {
    pub fn deterministic(store: &'a RecipeStore) -> Self {
        Self {
            store,
            router: None,
            prompt_manager: None,
            confirm_threshold: 1.0,
        }
    }

    pub fn with_llm(
        store: &'a RecipeStore,
        router: Arc<OperationLlmRouter>,
        prompt_manager: Arc<PromptManager>,
        confirm_threshold: f32,
    ) -> Self {
        Self {
            store,
            router: Some(router),
            prompt_manager: Some(prompt_manager),
            confirm_threshold: confirm_threshold.clamp(0.0, 1.0),
        }
    }

    pub async fn find(&self, query: &TaskShapeQuery<'_>) -> Option<RecipeMatchResult> {
        if let Ok(Some(recipe)) = self.store.find_by_task(query.task_id) {
            if recipe.agent_id == query.agent_id
                && recipe.scope_principal == query.principal
                && recipe.scope_workspace == query.workspace
            {
                if let Some(inputs) = extract_inputs_for_exact_task(&recipe, query) {
                    if replayable_with_inputs(&recipe, &inputs) {
                        return Some(RecipeMatchResult {
                            recipe,
                            kind: MatchKind::TaskId,
                            inputs,
                        });
                    }
                }
                // The current task text is authoritative. Compile-time input
                // examples are reused only while the normalized source hash
                // is unchanged; after any edit, deterministic/template or
                // fuzzy confirmation must recover current values or fail
                // through to the browser.
            }
        }

        let mut candidates: Vec<_> = self
            .store
            .list()
            .ok()?
            .into_iter()
            .filter(|recipe| {
                recipe.agent_id == query.agent_id
                    && recipe.scope_principal == query.principal
                    && recipe.scope_workspace == query.workspace
            })
            .collect();
        // Most-specific templates win when more than one can match.
        candidates.sort_by_key(|recipe| std::cmp::Reverse(recipe.shape.template.len()));
        for recipe in &candidates {
            if let Some(inputs) = extract_inputs_for_query(recipe, query, false) {
                if replayable_with_inputs(recipe, &inputs) {
                    return Some(RecipeMatchResult {
                        recipe: recipe.clone(),
                        kind: MatchKind::Template,
                        inputs,
                    });
                }
            }
        }

        let (Some(router), Some(prompt_manager)) =
            (self.router.clone(), self.prompt_manager.clone())
        else {
            return None;
        };
        let task_text = format!("{} {}", query.title, query.description);
        let mut ranked: Vec<_> = candidates
            .iter()
            .filter(|recipe| eligible_for_fuzzy(recipe))
            .map(|recipe| (token_overlap(&task_text, &recipe.shape.template), recipe))
            .filter(|(score, _)| *score >= 0.3)
            .collect();
        ranked.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top: Vec<_> = ranked.iter().take(3).map(|(_, recipe)| *recipe).collect();
        if top.is_empty() {
            return None;
        }
        let prompt = prompt_manager
            .get_prompt(
                crate::magician_v2::prompts::names::RECIPE_MATCH_SYSTEM,
                crate::magician_v2::prompts::versions::RECIPE_MATCH_SYSTEM,
            )
            .await
            .ok()?;
        let system = prompt.render(&HashMap::new()).ok()?;
        let payload = serialize_match_payload(query.title, query.description, &top);
        let raw = router
            .generate_for_operation_with_system(&LLMOperation::RecipeMatch, Some(&system), &payload)
            .await
            .ok()?;
        let cleaned = raw
            .content
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let response: serde_json::Value = serde_json::from_str(cleaned).ok()?;
        let recipe_id = response.get("recipe_id")?.as_str()?;
        let confidence = response.get("confidence")?.as_f64()? as f32;
        if !confidence.is_finite() || confidence < self.confirm_threshold || confidence > 1.0 {
            return None;
        }
        let recipe = (*top.iter().find(|recipe| recipe.id == recipe_id)?).clone();
        let object = response.get("inputs")?.as_object()?;
        if object.len() != recipe.shape.inputs.len() {
            return None;
        }
        let mut inputs = HashMap::with_capacity(object.len());
        for spec in &recipe.shape.inputs {
            let value = object.get(&spec.name)?.as_str()?.trim();
            if value.is_empty() || !valid_for_schema(value, spec.schema) {
                return None;
            }
            inputs.insert(spec.name.clone(), value.to_owned());
        }
        if !replayable_with_inputs(&recipe, &inputs) {
            return None;
        }
        Some(RecipeMatchResult {
            recipe,
            kind: MatchKind::LlmConfirmed(confidence),
            inputs,
        })
    }
}

fn replayable_with_inputs(recipe: &TaskRecipe, inputs: &HashMap<String, String>) -> bool {
    if !recipe
        .current()
        .is_some_and(|version| !version.answer_spec.is_empty())
    {
        return false;
    }
    super::recipe_runner::validate_recipe_preflight(
        recipe,
        &super::recipe_runner::RecipeRunInputs {
            inputs: inputs.clone(),
            timeout_ms: None,
            approved_write_steps: Default::default(),
        },
    )
    .is_ok()
}

fn extract_inputs_for_exact_task(
    recipe: &TaskRecipe,
    query: &TaskShapeQuery<'_>,
) -> Option<HashMap<String, String>> {
    extract_inputs_for_query(recipe, query, true)
}

fn extract_inputs_for_query(
    recipe: &TaskRecipe,
    query: &TaskShapeQuery<'_>,
    allow_examples: bool,
) -> Option<HashMap<String, String>> {
    let expression = template_regex(&recipe.shape.template)?;
    let captures = template_captures(&recipe.shape.template, &expression, query.title.trim())?;
    let current_task_text_fingerprint = source_task_fingerprint(query.title, query.description);
    let unchanged_source = recipe.current().is_some_and(|version| {
        version.compiled_from.task_text_fingerprint.as_deref()
            == Some(current_task_text_fingerprint.as_str())
    });
    let description_expression = match recipe.shape.description_template.as_deref() {
        Some(template) => Some(template_regex(template)?),
        None if unchanged_source => None,
        None => return None,
    };
    let description_captures = match description_expression.as_ref() {
        Some(expression) => Some(template_captures(
            recipe.shape.description_template.as_deref()?,
            expression,
            query.description.trim(),
        )?),
        None => None,
    };
    let mut inputs = HashMap::with_capacity(recipe.shape.inputs.len());
    for input in &recipe.shape.inputs {
        let title_value = captures
            .name(&input.name)
            .map(|value| value.as_str().trim());
        let description_value = description_captures
            .as_ref()
            .and_then(|captures| captures.name(&input.name))
            .map(|value| value.as_str().trim());
        let value = match (title_value, description_value) {
            (Some(left), Some(right)) if left != right => return None,
            (Some(value), _) | (_, Some(value)) => value,
            _ if allow_examples && unchanged_source => input.example_value.trim(),
            _ => return None,
        };
        if value.is_empty() || !valid_for_schema(value, input.schema) {
            return None;
        }
        inputs.insert(input.name.clone(), value.to_owned());
    }
    Some(inputs)
}

fn valid_for_schema(value: &str, schema: TaskInputSchema) -> bool {
    schema.accepts(value)
}

fn token_overlap(text: &str, template: &str) -> f32 {
    fn words(value: &str) -> HashSet<String> {
        value
            .to_lowercase()
            .split(|character: char| !character.is_alphanumeric())
            .filter(|word| word.len() > 2)
            .map(str::to_owned)
            .collect()
    }
    fn without_slots(template: &str) -> String {
        let mut result = String::with_capacity(template.len());
        let mut in_slot = false;
        for character in template.chars() {
            match character {
                '{' => {
                    in_slot = true;
                    result.push(' ');
                },
                '}' if in_slot => {
                    in_slot = false;
                    result.push(' ');
                },
                _ if !in_slot => result.push(character),
                _ => {},
            }
        }
        result
    }
    let text = words(text);
    let template = words(&without_slots(template));
    if template.is_empty() {
        return 0.0;
    }
    text.intersection(&template).count() as f32 / template.len() as f32
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::recipe::{
        CompiledFrom, RecipeAuth, RecipeShape, RecipeVersion, TaskInput, TaskInputSource,
    };
    use crate::magician_v2::api_mining::workflow::{ReplayStats, WorkflowMaturity};

    fn browser_typed_recipe() -> TaskRecipe {
        TaskRecipe {
            id: "rcp_typed".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "find the selected item".into(),
                fingerprint: "shape".into(),
                inputs: vec![TaskInput {
                    name: "query".into(),
                    schema: TaskInputSchema::String,
                    example_value: "blue".into(),
                    source: TaskInputSource::BrowserTyped,
                }],
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: Vec::new(),
                steps: Vec::new(),
                data_flows: Vec::new(),
                answer_spec: Vec::new(),
                auth: RecipeAuth::default(),
                maturity: WorkflowMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task".into(),
                    execution_id: "execution".into(),
                    task_text_fingerprint: Some(source_task_fingerprint(
                        "Find the selected item",
                        "Return its title",
                    )),
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        }
    }

    fn query<'a>(description: &'a str) -> TaskShapeQuery<'a> {
        TaskShapeQuery {
            task_id: "task",
            title: "Find the selected item",
            description,
            agent_id: "assistant",
            principal: "owner",
            workspace: "default",
        }
    }

    #[test]
    fn unchanged_exact_task_may_reuse_browser_typed_input() {
        let inputs =
            extract_inputs_for_exact_task(&browser_typed_recipe(), &query("Return its title"))
                .unwrap();
        assert_eq!(inputs.get("query").map(String::as_str), Some("blue"));
    }

    #[test]
    fn edited_task_cannot_reuse_browser_typed_input() {
        assert!(extract_inputs_for_exact_task(
            &browser_typed_recipe(),
            &query("Return the red item's title"),
        )
        .is_none());
    }

    #[test]
    fn deterministic_matching_honors_description_and_recovers_its_inputs() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "find item".into();
        recipe.shape.description_template = Some("return the title for {query}".into());
        let mut current = query("Return the title for red");
        current.title = "Find item";
        let inputs = extract_inputs_for_query(&recipe, &current, false).unwrap();
        assert_eq!(inputs["query"], "red");
        current.description = "Delete the item red";
        assert!(extract_inputs_for_query(&recipe, &current, true).is_none());
        assert!(extract_inputs_for_query(&recipe, &current, false).is_none());
    }

    #[test]
    fn changed_description_blocks_even_a_zero_input_title_match() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.inputs.clear();
        recipe.shape.description_template = Some("return its title".into());
        assert!(extract_inputs_for_query(&recipe, &query("Delete it instead"), true).is_none());
        assert!(extract_inputs_for_query(&recipe, &query("Delete it instead"), false).is_none());
        recipe.shape.description_template = None;
        assert!(extract_inputs_for_query(&recipe, &query("Delete it instead"), true).is_none());
    }

    #[test]
    fn conflicting_title_and_description_values_do_not_replay() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "find {query}".into();
        recipe.shape.description_template = Some("find {query}".into());
        let mut current = query("Find red");
        current.title = "Find blue";
        assert!(extract_inputs_for_query(&recipe, &current, true).is_none());
    }

    #[test]
    fn repeated_title_inputs_cannot_diverge_on_either_deterministic_rung() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "compare {query} with {query}".into();
        recipe.shape.description_template = Some("return both prices".into());
        let mut current = query("Return both prices");
        for allow_examples in [true, false] {
            current.title = "Compare red with blue";
            assert!(extract_inputs_for_query(&recipe, &current, allow_examples).is_none());
            current.title = "Compare red with red";
            assert_eq!(
                extract_inputs_for_query(&recipe, &current, allow_examples).unwrap()["query"],
                "red"
            );
        }
    }

    #[test]
    fn repeated_description_inputs_must_agree_with_each_other_and_the_title() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "compare {query}".into();
        recipe.shape.description_template = Some("compare {query} with {query}".into());
        let mut current = query("Compare red with blue");
        current.title = "Compare red";
        for allow_examples in [true, false] {
            for description in ["Compare red with blue", "Compare blue with blue"] {
                current.description = description;
                assert!(extract_inputs_for_query(&recipe, &current, allow_examples).is_none());
            }
            current.description = "Compare red with red";
            assert_eq!(
                extract_inputs_for_query(&recipe, &current, allow_examples).unwrap()["query"],
                "red"
            );
        }
    }

    #[test]
    fn multiline_descriptions_match_the_learned_shape_and_extract_current_inputs() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "find item".into();
        recipe.shape.description_template = Some("search for {query} and return its title".into());
        let mut current = query("Search for red\n\nand return its title");
        current.title = "Find\titem";
        for allow_examples in [true, false] {
            assert_eq!(
                extract_inputs_for_query(&recipe, &current, allow_examples).unwrap()["query"],
                "red"
            );
        }
        current.description = "Search for red\n\nand delete its title";
        assert!(extract_inputs_for_query(&recipe, &current, true).is_none());
    }

    #[test]
    fn numeric_inputs_reject_non_json_numbers() {
        assert!(valid_for_schema("42.5", TaskInputSchema::Number));
        for value in ["NaN", "inf", "+42", "01", "1.", ".5", "1e999"] {
            assert!(!valid_for_schema(value, TaskInputSchema::Number), "{value}");
        }
    }

    #[test]
    fn numeric_task_edits_must_be_renderable_on_both_deterministic_rungs() {
        let mut recipe = browser_typed_recipe();
        recipe.shape.template = "set {query}".into();
        recipe.shape.description_template = Some(String::new());
        recipe.shape.inputs[0].schema = TaskInputSchema::Number;
        recipe.shape.inputs[0].example_value = "42".into();
        let mut current = query("");
        for allow_examples in [true, false] {
            for title in ["Set +42", "Set 01", "Set .5", "Set 1."] {
                current.title = title;
                assert!(extract_inputs_for_query(&recipe, &current, allow_examples).is_none());
            }
            current.title = "Set 1e+3";
            assert_eq!(
                extract_inputs_for_query(&recipe, &current, allow_examples).unwrap()["query"],
                "1e+3"
            );
        }
    }

    #[test]
    fn overlap_scores_static_intent_but_not_placeholder_names() {
        assert_eq!(
            token_overlap("book hotel in Paris", "book hotel in {city}"),
            1.0
        );
        assert_eq!(
            token_overlap("city permit status", "find weather in {city}"),
            0.0
        );
    }
}
