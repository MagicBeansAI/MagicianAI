//! Lossless optional-field transport for reviewed App model output schemas.
//!
//! Structured-output providers require every object property to be present.
//! Null stands for omission only where the reviewed field was optional and
//! non-nullable. Explicitly nullable values retain their original meaning.
use serde_json::{json, Value};

const MAX_DEPTH: usize = 64;

pub(super) fn transport_schema(reviewed: &Value) -> Result<Value, &'static str> {
    fn walk(schema: &Value, depth: usize) -> Result<Value, &'static str> {
        if depth > MAX_DEPTH {
            return Err("model output schema exceeds its depth bound");
        }
        let mut result = schema.clone();
        let object = result
            .as_object_mut()
            .ok_or("invalid model output schema")?;
        if let Some(value) = object.remove("const") {
            // Canonical tagged-union discriminators are string constants.
            if !value.is_string() {
                return Err("unsupported model output discriminator");
            }
            object.insert("type".into(), json!("string"));
            object.insert("enum".into(), json!([value]));
        }
        if let Some(alternatives) = object.remove("oneOf") {
            // Reviewed unions have distinct required discriminator constants,
            // so their branches are disjoint and anyOf preserves the contract.
            object.insert("anyOf".into(), alternatives);
        }
        if let Some(alternatives) = object.get_mut("anyOf") {
            for branch in alternatives.as_array_mut().ok_or("invalid output union")? {
                *branch = walk(branch, depth + 1)?;
            }
        }
        if let Some(items) = object.get_mut("items") {
            *items = walk(items, depth + 1)?;
        }
        let required = schema.get("required").and_then(Value::as_array);
        if let Some(properties) = object.get_mut("properties") {
            let properties = properties.as_object_mut().ok_or("invalid output record")?;
            for (name, field) in properties.iter_mut() {
                let optional = !required
                    .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(name.as_str())));
                let original_nullable = accepts_null(field);
                *field = walk(field, depth + 1)?;
                if optional && !original_nullable {
                    *field = json!({"anyOf": [field, {"type": "null"}]});
                }
            }
            let names: Vec<_> = properties.keys().cloned().collect();
            object.insert("required".into(), json!(names));
            object.insert("additionalProperties".into(), json!(false));
        }
        Ok(result)
    }
    walk(reviewed, 0)
}

fn accepts_null(schema: &Value) -> bool {
    schema.get("type").is_some_and(|kind| {
        kind == "null"
            || kind
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "null"))
    }) || schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| branches.iter().any(accepts_null))
}

/// Decode transport-only nulls before the caller validates the exact reviewed
/// schema. Unknown fields, missing required fields and invalid non-null values
/// are left intact for that validation to reject; this is not a repair pass.
pub(super) fn decode_transport(reviewed: &Value, output: &mut Value) -> Result<(), &'static str> {
    fn walk(schema: &Value, output: &mut Value, depth: usize) -> Result<(), &'static str> {
        if depth > MAX_DEPTH {
            return Err("model output exceeds its depth bound");
        }
        if output.is_null() && accepts_null(schema) {
            return Ok(());
        }
        if let Some(branches) = schema
            .get("oneOf")
            .or_else(|| schema.get("anyOf"))
            .and_then(Value::as_array)
        {
            // The canonical projection emits only nullable pairs or tagged
            // unions. Select the latter by their reviewed discriminator.
            let matching: Vec<_> = branches
                .iter()
                .filter(|branch| {
                    if output.is_null() {
                        return accepts_null(branch);
                    }
                    if branch.get("type").is_some_and(|kind| kind == "null") {
                        return false;
                    }
                    branch
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_none_or(|fields| {
                            fields
                                .iter()
                                .filter_map(|(name, field)| {
                                    field.get("const").map(|tag| (name, tag))
                                })
                                .all(|(name, tag)| output.get(name) == Some(tag))
                        })
                })
                .collect();
            if matching.len() != 1 {
                return Err("model output does not select one reviewed union branch");
            }
            return walk(matching[0], output, depth + 1);
        }
        if let (Some(items), Some(values)) = (schema.get("items"), output.as_array_mut()) {
            for value in values {
                walk(items, value, depth + 1)?;
            }
        }
        if let (Some(properties), Some(values)) = (
            schema.get("properties").and_then(Value::as_object),
            output.as_object_mut(),
        ) {
            let required = schema.get("required").and_then(Value::as_array);
            for (name, field) in properties {
                let optional = !required
                    .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some(name.as_str())));
                if optional && !accepts_null(field) && values.get(name).is_some_and(Value::is_null)
                {
                    values.remove(name);
                } else if let Some(value) = values.get_mut(name) {
                    walk(field, value, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    walk(reviewed, output, 0)
}
