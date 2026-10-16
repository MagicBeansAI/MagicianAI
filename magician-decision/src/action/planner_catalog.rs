//! Lossless, engine-owned planner catalog encoding. Execution still validates
//! against the original catalog; no schema constraint or tool is discarded.
use decision_engine_contract::action::ActionTool;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

fn key(value: &Value) -> Option<String> {
    let fields = value.as_object()?;
    if !["type", "properties", "anyOf", "oneOf", "allOf", "enum"]
        .iter()
        .any(|name| fields.contains_key(*name))
    {
        return None;
    }
    let bytes = serde_json::to_vec(value).ok()?;
    (bytes.len() >= 160).then(|| format!("s_{:x}", Sha256::digest(&bytes)))
}

// Traverse schema positions only. A properties map is not itself a schema;
// enum/const/default/example objects are literal values and must stay literal.
fn map_schema_children(value: &Value, mut map: impl FnMut(&Value) -> Value) -> Value {
    let Some(fields) = value.as_object() else {
        return value.clone();
    };
    Value::Object(
        fields
            .iter()
            .map(|(name, child)| {
                let next = match name.as_str() {
                    "properties" | "patternProperties" | "$defs" | "definitions"
                    | "dependentSchemas" => child
                        .as_object()
                        .map(|fields| {
                            Value::Object(
                                fields
                                    .iter()
                                    .map(|(name, schema)| (name.clone(), map(schema)))
                                    .collect(),
                            )
                        })
                        .unwrap_or_else(|| child.clone()),
                    "allOf" | "anyOf" | "oneOf" | "prefixItems" => child
                        .as_array()
                        .map(|items| Value::Array(items.iter().map(&mut map).collect()))
                        .unwrap_or_else(|| child.clone()),
                    "items" if child.is_array() => {
                        Value::Array(child.as_array().unwrap().iter().map(&mut map).collect())
                    },
                    "items"
                    | "additionalProperties"
                    | "additionalItems"
                    | "unevaluatedProperties"
                    | "unevaluatedItems"
                    | "not"
                    | "contains"
                    | "propertyNames"
                    | "if"
                    | "then"
                    | "else" => map(child),
                    _ => child.clone(),
                };
                (name.clone(), next)
            })
            .collect(),
    )
}

fn count(value: &Value, counts: &mut HashMap<String, usize>) {
    if let Some(key) = key(value) {
        *counts.entry(key).or_default() += 1;
    }
    map_schema_children(value, |child| {
        count(child, counts);
        Value::Null
    });
}

fn children(
    value: &Value,
    counts: &HashMap<String, usize>,
    definitions: &mut Map<String, Value>,
) -> Value {
    map_schema_children(value, |child| encode(child, counts, definitions))
}

fn encode(
    value: &Value,
    counts: &HashMap<String, usize>,
    definitions: &mut Map<String, Value>,
) -> Value {
    if let Some(key) = key(value).filter(|key| counts.get(key).copied().unwrap_or(0) > 1) {
        if !definitions.contains_key(&key) {
            let body = children(value, counts, definitions);
            definitions.insert(key.clone(), body);
        }
        json!({"$ref":format!("#/schema_definitions/{key}")})
    } else {
        children(value, counts, definitions)
    }
}

pub(super) fn compact(tools: &[ActionTool]) -> (Value, Value) {
    let mut counts = HashMap::new();
    for tool in tools {
        count(&tool.parameters, &mut counts);
    }
    let mut definitions = Map::new();
    let encoded: Vec<_> = tools
        .iter()
        .map(|tool| {
            json!({
                "name":tool.name, "description":tool.description,
                "parameters":encode(&tool.parameters, &counts, &mut definitions)
            })
        })
        .collect();
    let encoded = Value::Array(encoded);
    let definitions = Value::Object(definitions);
    let original = serde_json::to_value(tools).expect("tool catalog is serializable");
    if encoded.to_string().len() + definitions.to_string().len() < original.to_string().len() {
        (encoded, definitions)
    } else {
        (original, json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expand(value: &Value, definitions: &Value) -> Value {
        if let Some(key) = value
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|s| s.strip_prefix("#/schema_definitions/"))
        {
            return expand(&definitions[key], definitions);
        }
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), expand(value, definitions)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|value| expand(value, definitions))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
    #[test]
    fn repeated_schemas_round_trip_with_every_constraint_and_tool() {
        let common = json!({"type":"string","description":"Keep this complete guidance. ".repeat(30),"enum":["local","remote"],"default":"local"});
        let tools: Vec<_> = (0..30).map(|i|ActionTool {
            name:format!("arbitrary_{i}"),description:format!("Tool {i}"),
            parameters:json!({"type":"object","properties":{"connection":common,"item":{"type":"integer","minimum":i}},"required":["connection","item"],"additionalProperties":false})
        }).collect();
        let (catalog, definitions) = compact(&tools);
        let original = serde_json::to_value(&tools).unwrap();
        assert_eq!(expand(&catalog, &definitions), original);
        assert!(
            catalog.to_string().len() + definitions.to_string().len()
                < original.to_string().len() / 2
        );
    }
    #[test]
    fn schema_shaped_literal_values_and_property_maps_stay_literal() {
        let literal = json!({"type":"literal","description":"literal ".repeat(40)});
        let parameters = json!({"type":"object","properties":{"type":{"type":"string"},"value":{"type":"object","const":literal,"default":literal,"enum":[literal]}}});
        let tools: Vec<_> = (0..4)
            .map(|i| ActionTool {
                name: format!("t{i}"),
                description: String::new(),
                parameters: parameters.clone(),
            })
            .collect();
        let (catalog, definitions) = compact(&tools);
        for definition in definitions.as_object().unwrap().values() {
            if let Some(value) = definition.get("const") {
                assert_eq!(value, &literal);
            }
            if let Some(properties) = definition.get("properties") {
                assert!(properties.get("$ref").is_none());
            }
        }
        assert_eq!(
            expand(&catalog, &definitions),
            serde_json::to_value(tools).unwrap()
        );
    }

    #[test]
    fn unique_schemas_do_not_inflate_or_change() {
        let tools = vec![ActionTool {
            name: "read".into(),
            description: "Read".into(),
            parameters: json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
        }];
        let (catalog, definitions) = compact(&tools);
        assert_eq!(catalog, serde_json::to_value(tools).unwrap());
        assert_eq!(definitions, json!({}));
    }
}
