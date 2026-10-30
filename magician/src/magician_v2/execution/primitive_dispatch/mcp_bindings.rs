//! Reviewed, declarative argument bindings. Remote annotations confer no authority.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_runtime_core::action_overrides::WorkspacePathAccess;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpBindings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_profile: Option<crate::magician_v2::mcp_public_profile::McpPublicProfile>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub tools: BTreeMap<String, McpToolBindings>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpToolBindings {
    /// JSON pointers into the native tool's arguments. Values stay native JSON.
    #[serde(default)]
    pub paths: BTreeMap<String, WorkspacePathAccess>,
    #[serde(default)]
    pub redact_fields: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub artifacts: Vec<McpArtifactBinding>,
    pub outward: Option<McpOutwardBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commerce_quote: Option<super::mcp_commerce::QuoteBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commerce_checkout: Option<super::mcp_commerce::CheckoutBinding>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpArtifactBinding {
    pub pointer: String,
    /// Reviewed scope-workdirs output root, in addition to current-task outputs.
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpOutwardBinding {
    pub channel: crate::magician_v2::evidence::OutwardChannel,
    #[serde(default = "default_consequence")]
    pub consequence_class: crate::magician_v2::agents::ConsequenceClass,
    pub recipients: Vec<String>,
    #[serde(default)]
    pub audience_optional: bool,
    pub provider: String,
    #[serde(default)]
    pub tracks_delivery: bool,
    /// Pointer into the upstream structured result (or JSON text content).
    pub receipt: Option<String>,
}

fn default_consequence() -> crate::magician_v2::agents::ConsequenceClass {
    crate::magician_v2::agents::ConsequenceClass::SubmissionOrPublication
}

impl McpBindings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(profile) = &self.public_profile {
            profile.validate()?;
        }
        let pointer = |value: &str| value.starts_with('/') && value.len() <= 4096;
        if self.tools.len() > 4096 || self.environment.len() > 256 {
            return Err("MCP binding catalog exceeds local bounds".into());
        }
        for binding in self.tools.values() {
            if binding.commerce_quote.is_some() && binding.commerce_checkout.is_some() {
                return Err(
                    "a single MCP call cannot both produce and consume a checkout quote".into(),
                );
            }
            if let Some(quote) = &binding.commerce_quote {
                quote.validate()?;
            }
            if let Some(checkout) = &binding.commerce_checkout {
                checkout.validate()?;
                let quote = self
                    .tools
                    .get(&checkout.quote_tool)
                    .and_then(|t| t.commerce_quote.as_ref())
                    .ok_or("MCP checkout requires a locally declared cart-quote producer")?;
                if !checkout.endpoint_aliases.is_subset(&quote.endpoint_aliases) {
                    return Err("MCP checkout and cart quote endpoint bindings differ".into());
                }
            }
            if binding.paths.keys().any(|key| !pointer(key))
                || binding.artifacts.iter().any(|item| {
                    !pointer(&item.pointer)
                        || !std::path::Path::new(&item.root)
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                })
            {
                return Err(
                    "MCP file bindings require bounded JSON pointers and relative output roots"
                        .into(),
                );
            }
            if let Some(outward) = &binding.outward {
                if !outward.consequence_class.requires_gate()
                    || outward.recipients.iter().any(|key| !pointer(key))
                    || outward.receipt.as_deref().is_some_and(|key| !pointer(key))
                {
                    return Err(
                        "MCP outward bindings require an outward consequence and bounded pointers"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn recipients(
    binding: &McpOutwardBinding,
    arguments: &Value,
) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for pointer in &binding.recipients {
        let Some(value) = arguments.pointer(pointer) else {
            continue;
        };
        match value {
            Value::String(value) if !value.trim().is_empty() => result.push(value.clone()),
            Value::Array(values) => {
                for value in values {
                    let value = value
                        .as_str()
                        .filter(|v| !v.trim().is_empty())
                        .ok_or("MCP recipient binding requires nonempty strings")?;
                    result.push(value.to_owned());
                }
            },
            _ => return Err("MCP recipient binding requires a string or string array".into()),
        }
    }
    result.sort();
    result.dedup();
    if result.is_empty() && !binding.audience_optional {
        return Err("MCP outward action requires a bound recipient".into());
    }
    Ok(result)
}

pub(crate) fn native_documents(result: &Value) -> Vec<Value> {
    let mut documents = Vec::new();
    if let Some(value) = result
        .pointer("/result/structured_content")
        .filter(|v| !v.is_null())
    {
        documents.push(value.clone());
    }
    if let Some(contents) = result.pointer("/result/content").and_then(Value::as_array) {
        for content in contents {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                if let Ok(value) = serde_json::from_str(text) {
                    documents.push(value);
                }
            }
        }
    }
    documents
}

pub(crate) fn receipt_id(result: &Value, pointer: &str) -> Option<String> {
    let native = result
        .pointer("/result/structuredContent")
        .or_else(|| result.pointer("/result/structured_content"));
    let at = |v: &Value| {
        v.pointer(pointer)
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
    };
    if let Some(id) = native.and_then(at) {
        return Some(id);
    }
    result
        .pointer("/result/content")?
        .as_array()?
        .iter()
        .find_map(|content| {
            let value: Value = serde_json::from_str(content.get("text")?.as_str()?).ok()?;
            at(&value)
        })
}

pub(crate) fn redact_result(value: &mut Value, fields: &std::collections::BTreeSet<String>) {
    if fields.is_empty() {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if fields.contains(key) {
                    *value = Value::String("[REDACTED]".into());
                } else if key == "text" {
                    if let Some(text) = value.as_str() {
                        if let Ok(mut content) = serde_json::from_str::<Value>(text) {
                            redact_result(&mut content, fields);
                            *value = Value::String(content.to_string());
                        }
                    }
                } else {
                    redact_result(value, fields);
                }
            }
        },
        Value::Array(values) => {
            for value in values {
                redact_result(value, fields);
            }
        },
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binds_all_recipients_and_rejects_opaque_arguments() {
        let binding: McpOutwardBinding = serde_json::from_value(json!({
            "channel":"whatsapp", "provider":"whatsapp", "recipients":["/to", "/participants"]
        }))
        .unwrap();
        assert_eq!(
            recipients(&binding, &json!({"to":"a", "participants":["b","a"]})).unwrap(),
            ["a", "b"]
        );
        assert!(recipients(&binding, &json!({"to":{"jid":"hidden"}})).is_err());
        assert!(recipients(&binding, &json!({})).is_err());
    }

    #[test]
    fn rejects_escaping_output_roots_and_redacts_json_text_results() {
        let invalid: McpBindings = serde_json::from_value(
            json!({"tools":{"export":{"artifacts":[{"pointer":"/file","root":"../auth"}]}}}),
        )
        .unwrap();
        assert!(invalid.validate().is_err());
        let mut result = json!({"result":{"content":[{"type":"text","text":"{\"enrich\":{\"api_key\":\"private\"},\"mode\":\"local\"}"}]}});
        redact_result(
            &mut result,
            &std::collections::BTreeSet::from(["api_key".into()]),
        );
        let text = result
            .pointer("/result/content/0/text")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(!text.contains("private"));
        assert!(text.contains("local"));
    }

    #[test]
    fn reads_receipts_only_from_native_results() {
        assert_eq!(
            receipt_id(
                &json!({"result":{"content":[{"type":"text","text":"{\"id\":\"provider-1\"}"}]}}),
                "/id"
            )
            .as_deref(),
            Some("provider-1")
        );
        assert!(receipt_id(&json!({"id":"invented"}), "/id").is_none());
    }
}
