//! Model-facing argument shapes derived from the same types the store parses.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Value};

use super::models::{
    AppExpectedRecordRevision, AppMutationOperation, AppPredicate, AppQueryOrder,
    AppRelationExpansion,
};

/// Package schema metadata only: no records, policies, grants or cursor data.
/// Both query and terminal tools use the same current compiled field catalog.
pub(super) fn entity_catalog(active: &super::entity_store::ActiveAppEntitySchema) -> Value {
    let mut entities = serde_json::Map::new();
    for (entity, runtime) in active.runtime_contracts() {
        let mut fields = serde_json::Map::new();
        for (name, field) in runtime.fields() {
            let mut contract = json!({
                "type": field.kind(),
                "required": field.required(),
                "nullable": field.nullable(),
            });
            if !field.enum_values().is_empty() {
                contract["values"] = json!(field.enum_values());
            }
            if let Some(target) = field.reference_entity() {
                contract["entity"] = json!(target);
                contract["max_traversal_depth"] = json!(field.relation_max_depth());
            }
            fields.insert(name.to_string(), contract);
        }
        entities.insert(entity.to_string(), Value::Object(fields));
    }
    Value::Object(entities)
}

fn inline_schema<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::default();
    settings.inline_subschemas = true;
    let mut schema = settings
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    // Each parameter is embedded below the tool's root schema. Inlining keeps
    // its references valid there; a parameter is not a separate schema root.
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
    }
    schema
}

pub(super) fn predicate_schema() -> Value {
    inline_schema::<AppPredicate>()
}

pub(super) fn order_schema() -> Value {
    let mut schema = inline_schema::<Vec<AppQueryOrder>>();
    schema["maxItems"] = json!(256);
    schema
}

pub(super) fn relation_expansions_schema() -> Value {
    let mut schema = inline_schema::<Vec<AppRelationExpansion>>();
    schema["maxItems"] = json!(256);
    schema
}

pub(super) fn mutation_operations_schema(allow_mutations: bool) -> Value {
    let mut schema = inline_schema::<Vec<AppMutationOperation>>();
    schema["maxItems"] = json!(if allow_mutations { 1_000 } else { 0 });
    schema
}

pub(super) fn expected_record_revisions_schema() -> Value {
    let mut schema = inline_schema::<Vec<AppExpectedRecordRevision>>();
    schema["maxItems"] = json!(1_000);
    schema
}
