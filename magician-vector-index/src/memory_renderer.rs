//! Default memory tier renderer + value flattening helpers.
//!
//! Copied from `magician::magician_v2::agents::memory_tier_interpreter`
//! (`DefaultMemoryRenderer`, `primary_collection_field`, `value_to_text`,
//! `flatten_value`) so this crate has no reverse dependency on magician.
//! Keep these helpers in sync with the canonical magician-side
//! implementation; both define how memory candidate text is rendered for
//! prompt injection.

use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::memory_record::V3MemoryTierRecord;
use crate::memory_tiers::{MemoryRenderer, MemoryTierDefinition, TierFieldSchema};

#[derive(Debug, Clone, Default)]
pub struct DefaultMemoryRenderer;

impl MemoryRenderer for DefaultMemoryRenderer {
    fn render(&self, tier: &MemoryTierDefinition, data: &V3MemoryTierRecord) -> String {
        let original = &tier.render.template;
        let mut rendered = original.clone();
        rendered = rendered.replace("{tier_name}", &data.tier_name);
        rendered = rendered.replace("{last_updated}", &data.last_updated.to_rfc3339());

        for (field, value) in &data.fields {
            let value_text = value_to_text(value);
            rendered = rendered.replace(&format!("{{{field}}}"), &value_text);
        }

        if let Some(collection_field) = primary_collection_field(tier) {
            if rendered.contains(&format!("{{{collection_field}}}")) {
                if let Some(value) = data.fields.get("value") {
                    rendered =
                        rendered.replace(&format!("{{{collection_field}}}"), &value_to_text(value));
                } else if data.fields.len() == 1 {
                    // Resilience: an LLM consolidation often persists the collection
                    // under a NON-canonical key it chose itself (e.g. `research_patterns`
                    // instead of the declared `{collection_field}`). With exactly ONE
                    // field present, that field IS the collection regardless of its key
                    // — render it rather than leaving the placeholder unresolved and
                    // falling through to the per-render WARN + structured fallback.
                    if let Some((_, value)) = data.fields.iter().next() {
                        rendered = rendered
                            .replace(&format!("{{{collection_field}}}"), &value_to_text(value));
                    }
                } else if !data.fields.is_empty() {
                    // Some LLM consolidation prompts ask for a collection but emit a
                    // structured object keyed by topic instead of an `entries` array.
                    // Treat that object as the collection payload so indexing uses
                    // useful text and does not warn on every render.
                    rendered = rendered.replace(
                        &format!("{{{collection_field}}}"),
                        &render_unstructured_collection_fields(&data.fields),
                    );
                }
            }
        }

        if &rendered == original || contains_unresolved_template_placeholder(&rendered) {
            // Empty record (no consolidations yet) is the common case for
            // newly-seeded agents — log at DEBUG and produce the structured
            // empty representation silently. Records WITH fields that still
            // can't resolve the template indicate a real template/data
            // mismatch worth flagging at WARN.
            if data.fields.is_empty() {
                debug!(
                    tier = %data.tier_name,
                    "Memory tier has no consolidated data yet; rendering structured empty fallback"
                );
            } else {
                warn!(
                    tier = %data.tier_name,
                    field_count = data.fields.len(),
                    "Memory tier render template had unresolved placeholders despite present fields; using structured fallback"
                );
            }
            return render_tier_fields_fallback(tier, data);
        }

        rendered
    }
}

pub fn primary_collection_field(tier: &MemoryTierDefinition) -> Option<&str> {
    let mut candidates = tier.schema.iter().filter_map(|(field, schema)| {
        matches!(schema, TierFieldSchema::Collection { .. }).then_some(field.as_str())
    });
    let first = candidates.next()?;
    if candidates.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn contains_unresolved_template_placeholder(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            return false;
        };
        let token = after_start[..end].trim();
        if !token.is_empty()
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return true;
        }
        rest = &after_start[end + 1..];
    }
    false
}

fn render_tier_fields_fallback(tier: &MemoryTierDefinition, data: &V3MemoryTierRecord) -> String {
    let mut lines = vec![format!("Tier: {}", data.tier_name)];
    if let Some(collection_field) = primary_collection_field(tier) {
        if !data.fields.contains_key(collection_field) {
            if let Some(value) = data.fields.get("value") {
                lines.push(format!("{collection_field}: {}", value_to_text(value)));
            }
        }
    }
    for (field, value) in &data.fields {
        lines.push(format!("{field}: {}", value_to_text(value)));
    }
    lines.join("\n")
}

fn render_unstructured_collection_fields(fields: &HashMap<String, Value>) -> String {
    let mut keys = fields.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys.into_iter()
        .filter_map(|field| {
            fields
                .get(&field)
                .map(|value| format!("{field}: {}", value_to_text(value)))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn value_to_text(value: &Value) -> String {
    let mut out = Vec::new();
    flatten_value("", value, &mut out);
    out.join("; ")
}

pub fn flatten_value(path: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Null => {
            if path.is_empty() {
                out.push("none".to_string());
            } else {
                out.push(format!("{path}: none"));
            }
        },
        Value::Bool(flag) => {
            if path.is_empty() {
                out.push(flag.to_string());
            } else {
                out.push(format!("{path}: {flag}"));
            }
        },
        Value::Number(number) => {
            if path.is_empty() {
                out.push(number.to_string());
            } else {
                out.push(format!("{path}: {number}"));
            }
        },
        Value::String(text) => {
            let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if path.is_empty() {
                out.push(compact);
            } else {
                out.push(format!("{path}: {compact}"));
            }
        },
        Value::Array(items) => {
            if items.is_empty() {
                if path.is_empty() {
                    out.push("none".to_string());
                } else {
                    out.push(format!("{path}: none"));
                }
                return;
            }
            for (idx, item) in items.iter().enumerate() {
                let child = if path.is_empty() {
                    format!("item {}", idx + 1)
                } else {
                    format!("{path} item {}", idx + 1)
                };
                flatten_value(&child, item, out);
            }
        },
        Value::Object(map) => {
            if map.is_empty() {
                if path.is_empty() {
                    out.push("none".to_string());
                } else {
                    out.push(format!("{path}: none"));
                }
                return;
            }
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(value) = map.get(&key) {
                    flatten_value(&child, value, out);
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_record::V3MemoryTierRecord;
    use crate::memory_tiers::{RenderConfig, RetentionMode, TierScope};

    #[test]
    fn renderer_resolves_collection_placeholder_from_unstructured_object_fields() {
        let tier = MemoryTierDefinition {
            name: "project_patterns".to_string(),
            scope: TierScope::Agent,
            description: "Learned project patterns".to_string(),
            schema: std::collections::BTreeMap::from([(
                "entries".to_string(),
                TierFieldSchema::Collection {
                    max_items: Some(40),
                    item_schema: None,
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entries}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut record = V3MemoryTierRecord::new(
            tier.name.clone(),
            tier.scope.clone(),
            None,
            None,
            None,
            Some("junior-frontend-engineer"),
        );
        record.fields.insert(
            "file_organization".to_string(),
            serde_json::json!({
                "status": "partially_observed",
                "patterns": ["Frontend lives under ui/unified-ui"]
            }),
        );
        record.fields.insert(
            "css_methodology".to_string(),
            serde_json::json!({
                "status": "insufficient_evidence",
                "patterns": []
            }),
        );

        let rendered = DefaultMemoryRenderer.render(&tier, &record);
        assert!(
            !rendered.starts_with("Tier:"),
            "renderer fell through to structured fallback: {rendered}"
        );
        assert!(rendered.contains("file_organization"));
        assert!(rendered.contains("Frontend lives under ui/unified-ui"));
        assert!(rendered.contains("css_methodology"));
    }
}
