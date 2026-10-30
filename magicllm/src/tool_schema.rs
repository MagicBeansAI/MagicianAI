//! Model-facing projection of a tool's canonical parameter schema.
//!
//! A tool's canonical schema may state which argument combinations make a
//! valid call with top-level `anyOf` / `oneOf` / `allOf` / `not` (for example
//! "a `command`, or a status/cancel `action` with a `job_id`"). That contract is
//! internal: candidate construction, proposal validation and dispatch read it
//! from the canonical tool spec.
//!
//! Models do not get it as schema. Most providers reject those keywords at the
//! top level of a tool's parameters and answer the whole request with a 400:
//! Anthropic ("input_schema does not support oneOf, allOf, or anyOf at the top
//! level"), OpenAI Chat Completions ("must … not have 'oneOf'/'anyOf'/'allOf'/
//! 'enum'/'const'/'not' at the top level") and xAI ("tool parameter root must
//! be an object type"), measured 2026-10-30; Gemini's dialect has no
//! combinators at all. OpenAI Responses accepts them but does not enforce them
//! outside strict mode. Every model-facing boundary (each provider adapter and
//! the MCP tool catalog) therefore sends [`project_whole_call_constraints`]:
//! the plain object schema, with the rules restated in the description as
//! compact JSON Schema so a model still learns which calls are valid. The same
//! guard keeps a third-party (MCP) schema with such keywords from failing a
//! whole request.

use serde_json::{Map, Value};
use std::borrow::Cow;

use crate::types::LLMToolSpec;

/// Top-level keywords that express requirements across fields.
const WHOLE_CALL_KEYWORDS: [&str; 4] = ["allOf", "anyOf", "oneOf", "not"];

/// Prefix of the description line that carries the moved rules.
const CONSTRAINT_NOTE: &str = "Valid argument combinations (JSON Schema): ";

/// The description and parameters a model receives for `tool`. Borrows both
/// unchanged when the schema has no top-level whole-call rules.
pub fn project_whole_call_constraints(tool: &LLMToolSpec) -> (Cow<'_, str>, Cow<'_, Value>) {
    project_schema(&tool.description, &tool.parameters)
}

/// [`project_whole_call_constraints`] for a description and schema held apart.
pub fn project_schema<'a>(
    description: &'a str,
    parameters: &'a Value,
) -> (Cow<'a, str>, Cow<'a, Value>) {
    let Some(schema) = parameters.as_object() else {
        return (Cow::Borrowed(description), Cow::Borrowed(parameters));
    };
    if !WHOLE_CALL_KEYWORDS
        .iter()
        .any(|key| schema.contains_key(*key))
    {
        return (Cow::Borrowed(description), Cow::Borrowed(parameters));
    }

    let mut parameters = schema.clone();
    let mut rules = Map::new();
    for key in WHOLE_CALL_KEYWORDS {
        if let Some(rule) = parameters.remove(key) {
            rules.insert(key.to_string(), rule);
        }
    }
    let note = format!("{CONSTRAINT_NOTE}{}", Value::Object(rules));
    let description = if description.trim().is_empty() {
        note
    } else {
        format!("{}\n\n{note}", description.trim_end())
    };
    (
        Cow::Owned(description),
        Cow::Owned(Value::Object(parameters)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(parameters: Value) -> LLMToolSpec {
        LLMToolSpec {
            name: "shell".to_string(),
            description: "Run a command.".to_string(),
            parameters,
        }
    }

    #[test]
    fn a_schema_without_whole_call_rules_passes_through_borrowed() {
        let tool = spec(json!({
            "type": "object",
            "properties": {"command": {"type": "string", "anyOf": [{"minLength": 1}]}},
            "required": ["command"],
        }));
        let (description, parameters) = project_whole_call_constraints(&tool);
        assert!(matches!(description, Cow::Borrowed(_)));
        assert!(matches!(parameters, Cow::Borrowed(_)));
        assert_eq!(parameters.as_ref(), &tool.parameters);
    }

    #[test]
    fn whole_call_rules_move_into_the_description() {
        let tool = spec(json!({
            "type": "object",
            "properties": {"command": {"type": "string"}, "job_id": {"type": "string"}},
            "required": [],
            "additionalProperties": false,
            "allOf": [{"anyOf": [{"required": ["command"]}, {"required": ["job_id"]}]}],
        }));
        let (description, parameters) = project_whole_call_constraints(&tool);

        assert_eq!(
            parameters.as_ref(),
            &json!({
                "type": "object",
                "properties": {"command": {"type": "string"}, "job_id": {"type": "string"}},
                "required": [],
                "additionalProperties": false,
            })
        );
        let (head, note) = description.split_once("\n\n").unwrap();
        assert_eq!(head, "Run a command.");
        let rules: Value =
            serde_json::from_str(note.strip_prefix(CONSTRAINT_NOTE).unwrap()).unwrap();
        assert_eq!(rules, json!({"allOf": tool.parameters["allOf"].clone()}));
        // Nested combinators inside properties are left for the provider.
        assert!(parameters.get("allOf").is_none());
    }

    #[test]
    fn every_top_level_combinator_moves_and_a_blank_description_gets_only_the_note() {
        let mut tool = spec(json!({
            "type": "object",
            "properties": {},
            "anyOf": [{"required": ["a"]}],
            "oneOf": [{"required": ["b"]}],
            "not": {"required": ["c"]},
        }));
        tool.description = "  ".to_string();
        let (description, parameters) = project_whole_call_constraints(&tool);
        for key in WHOLE_CALL_KEYWORDS {
            assert!(parameters.get(key).is_none(), "{key} stayed in the schema");
        }
        assert!(description.starts_with(CONSTRAINT_NOTE));
        let rules: Value =
            serde_json::from_str(description.strip_prefix(CONSTRAINT_NOTE).unwrap()).unwrap();
        assert_eq!(rules.as_object().unwrap().len(), 3);
    }
}
