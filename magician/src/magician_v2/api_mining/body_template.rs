use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyPlaceholderKind {
    String,
    Number,
    Boolean,
    Json,
}

impl BodyPlaceholderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Json => "json",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyTemplateParam {
    pub name: String,
    pub kind: BodyPlaceholderKind,
}

pub fn body_placeholder(kind: BodyPlaceholderKind, name: &str) -> String {
    format!("{{{{{}:{}}}}}", kind.as_str(), name)
}

pub fn extract_body_template_params(template: &str) -> Vec<BodyTemplateParam> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0usize;

    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'}' && bytes[end + 1] == b'}') {
                end += 1;
            }
            if end + 1 < bytes.len() {
                let raw = &template[start..end];
                if let Some((kind, name)) = parse_placeholder(raw) {
                    let dedup_key = format!("{}:{}", kind.as_str(), name);
                    if seen.insert(dedup_key) {
                        out.push(BodyTemplateParam {
                            name: name.to_string(),
                            kind,
                        });
                    }
                }
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }

    out
}

pub fn render_body_template_with_strings(
    template: &str,
    params: &HashMap<String, String>,
) -> Result<String, String> {
    let json_params = params
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect::<HashMap<_, _>>();
    render_body_template_with_values(template, &json_params)
}

pub fn render_body_template_with_values(
    template: &str,
    params: &HashMap<String, Value>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'}' && bytes[end + 1] == b'}') {
                end += 1;
            }
            if end + 1 < bytes.len() {
                let raw = &template[start..end];
                if let Some((kind, name)) = parse_placeholder(raw) {
                    let value = params
                        .get(name)
                        .ok_or_else(|| format!("Missing body template parameter '{}'", name))?;
                    out.push_str(&render_placeholder(kind, value)?);
                    i = end + 2;
                    continue;
                }
            }
        }
        // Advance by one UTF-8 char to avoid corrupting multi-byte sequences.
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&template[i..i + ch_len]);
        i += ch_len;
    }

    Ok(out)
}

pub fn extract_body_template_values(
    template: &str,
    body: &str,
) -> Result<HashMap<String, String>, String> {
    let trimmed = template.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return extract_json_body_template_values(template, body);
    }

    extract_form_body_template_values(template, body)
}

fn parse_placeholder(raw: &str) -> Option<(BodyPlaceholderKind, &str)> {
    let trimmed = raw.trim();
    let (kind_raw, name_raw) = trimmed.split_once(':')?;
    let kind = BodyPlaceholderKind::from_str(kind_raw.trim())?;
    let name = name_raw.trim();
    if !name.starts_with("body_") {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((kind, name))
}

fn parse_full_placeholder(raw: &str) -> Option<(BodyPlaceholderKind, &str)> {
    raw.strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
        .and_then(parse_placeholder)
}

fn render_placeholder(kind: BodyPlaceholderKind, value: &Value) -> Result<String, String> {
    match kind {
        BodyPlaceholderKind::String => {
            let rendered = match value {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            serde_json::to_string(&rendered)
                .map_err(|e| format!("Failed to encode string placeholder: {}", e))
        },
        BodyPlaceholderKind::Number => render_number(value),
        BodyPlaceholderKind::Boolean => render_boolean(value),
        BodyPlaceholderKind::Json => render_json(value),
    }
}

fn render_number(value: &Value) -> Result<String, String> {
    match value {
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => {
            let parsed: Value = serde_json::from_str(s)
                .map_err(|_| format!("Expected numeric body template value, got '{}'", s))?;
            match parsed {
                Value::Number(n) => Ok(n.to_string()),
                _ => Err(format!("Expected numeric body template value, got '{}'", s)),
            }
        },
        other => Err(format!(
            "Expected numeric body template value, got {}",
            other
        )),
    }
}

fn render_boolean(value: &Value) -> Result<String, String> {
    match value {
        Value::Bool(v) => Ok(v.to_string()),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            _ => Err(format!("Expected boolean body template value, got '{}'", s)),
        },
        other => Err(format!(
            "Expected boolean body template value, got {}",
            other
        )),
    }
}

fn render_json(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                Ok(parsed.to_string())
            } else {
                serde_json::to_string(s)
                    .map_err(|e| format!("Failed to encode JSON placeholder string: {}", e))
            }
        },
        other => Ok(other.to_string()),
    }
}

fn extract_json_body_template_values(
    template: &str,
    body: &str,
) -> Result<HashMap<String, String>, String> {
    let (template_with_markers, markers) = replace_placeholders_with_markers(template)?;
    let template_json: Value = serde_json::from_str(&template_with_markers)
        .map_err(|e| format!("Failed to parse JSON body template: {}", e))?;
    let body_json: Value = serde_json::from_str(body)
        .map_err(|e| format!("Failed to parse JSON request body: {}", e))?;

    let mut values = HashMap::new();
    collect_json_template_values(&template_json, &body_json, &markers, &mut values)?;
    Ok(values)
}

fn extract_form_body_template_values(
    template: &str,
    body: &str,
) -> Result<HashMap<String, String>, String> {
    let template_pairs = url::form_urlencoded::parse(template.as_bytes()).collect::<Vec<_>>();
    let actual_pairs = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect::<HashMap<_, _>>();

    let mut values = HashMap::new();
    for (key, template_value) in template_pairs {
        let actual_value = actual_pairs
            .get(key.as_ref())
            .ok_or_else(|| format!("Missing form body key '{}'", key))?;

        if let Some((_, name)) = parse_full_placeholder(&template_value) {
            values.insert(name.to_string(), actual_value.clone());
        } else if template_value != *actual_value {
            return Err(format!(
                "Form body literal mismatch for '{}': expected '{}', got '{}'",
                key, template_value, actual_value
            ));
        }
    }

    Ok(values)
}

fn replace_placeholders_with_markers(
    template: &str,
) -> Result<(String, HashMap<String, BodyTemplateParam>), String> {
    let mut markers = HashMap::new();
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0usize;
    let mut counter = 0usize;

    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'}' && bytes[end + 1] == b'}') {
                end += 1;
            }
            if end + 1 < bytes.len() {
                let raw = &template[start..end];
                if let Some((kind, name)) = parse_placeholder(raw) {
                    let marker = format!("__magician_body_template_marker_{}__", counter);
                    counter += 1;
                    markers.insert(
                        marker.clone(),
                        BodyTemplateParam {
                            name: name.to_string(),
                            kind,
                        },
                    );
                    out.push_str(
                        &serde_json::to_string(&marker)
                            .map_err(|e| format!("Failed to encode placeholder marker: {}", e))?,
                    );
                    i = end + 2;
                    continue;
                }
            }
        }

        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&template[i..i + ch_len]);
        i += ch_len;
    }

    Ok((out, markers))
}

fn collect_json_template_values(
    template: &Value,
    actual: &Value,
    markers: &HashMap<String, BodyTemplateParam>,
    out: &mut HashMap<String, String>,
) -> Result<(), String> {
    match template {
        Value::String(marker) if markers.contains_key(marker) => {
            let param = markers
                .get(marker)
                .ok_or_else(|| "Missing template marker".to_string())?;
            out.insert(
                param.name.clone(),
                extract_placeholder_value(param.kind, actual)?,
            );
            Ok(())
        },
        Value::Object(template_map) => {
            let actual_map = actual.as_object().ok_or_else(|| {
                "Expected JSON object while extracting template values".to_string()
            })?;
            for (key, template_value) in template_map {
                let actual_value = actual_map
                    .get(key)
                    .ok_or_else(|| format!("Missing JSON body key '{}'", key))?;
                collect_json_template_values(template_value, actual_value, markers, out)?;
            }
            Ok(())
        },
        Value::Array(template_values) => {
            let actual_values = actual.as_array().ok_or_else(|| {
                "Expected JSON array while extracting template values".to_string()
            })?;
            if template_values.len() != actual_values.len() {
                return Err(format!(
                    "JSON array length mismatch: expected {}, got {}",
                    template_values.len(),
                    actual_values.len()
                ));
            }
            for (template_value, actual_value) in template_values.iter().zip(actual_values.iter()) {
                collect_json_template_values(template_value, actual_value, markers, out)?;
            }
            Ok(())
        },
        _ if template == actual => Ok(()),
        _ => Err("JSON body literal mismatch while extracting template values".to_string()),
    }
}

fn extract_placeholder_value(kind: BodyPlaceholderKind, actual: &Value) -> Result<String, String> {
    match kind {
        BodyPlaceholderKind::String => Ok(actual
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| actual.to_string())),
        BodyPlaceholderKind::Number => match actual {
            Value::Number(number) => Ok(number.to_string()),
            Value::String(value) => {
                let parsed: Value = serde_json::from_str(value)
                    .map_err(|_| format!("Expected number for placeholder, got '{}'", value))?;
                match parsed {
                    Value::Number(number) => Ok(number.to_string()),
                    _ => Err(format!("Expected number for placeholder, got '{}'", value)),
                }
            },
            other => Err(format!("Expected number for placeholder, got {}", other)),
        },
        BodyPlaceholderKind::Boolean => match actual {
            Value::Bool(value) => Ok(value.to_string()),
            Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
                "true" => Ok("true".to_string()),
                "false" => Ok("false".to_string()),
                _ => Err(format!("Expected boolean for placeholder, got '{}'", value)),
            },
            other => Err(format!("Expected boolean for placeholder, got {}", other)),
        },
        BodyPlaceholderKind::Json => Ok(actual.to_string()),
    }
}

/// Returns the byte length of the UTF-8 character starting at `lead_byte`.
fn utf8_char_len(lead_byte: u8) -> usize {
    match lead_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1, // continuation byte — shouldn't happen at a char boundary
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_extract_body_template_params_ignores_graphql_braces() {
        let template = r#"{"query":"query GetUser { user { id } }","variables":{"id":{{string:body_variables_id}}}}"#;
        let params = extract_body_template_params(template);
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "body_variables_id");
        assert_eq!(params[0].kind, BodyPlaceholderKind::String);
    }

    #[test]
    fn test_render_body_template_typed_placeholders() {
        let template = r#"{"id":{{string:body_id}},"limit":{{number:body_limit}},"vars":{{json:body_vars}},"enabled":{{boolean:body_enabled}}}"#;
        let mut params = HashMap::new();
        params.insert("body_id".to_string(), Value::String("abc-123".to_string()));
        params.insert("body_limit".to_string(), Value::String("25".to_string()));
        params.insert(
            "body_vars".to_string(),
            Value::String(r#"{"cursor":"next"}"#.to_string()),
        );
        params.insert("body_enabled".to_string(), Value::Bool(true));

        let rendered = render_body_template_with_values(template, &params).unwrap();
        assert_eq!(
            rendered,
            r#"{"id":"abc-123","limit":25,"vars":{"cursor":"next"},"enabled":true}"#
        );
    }

    #[test]
    fn test_extract_json_body_template_values() {
        let template = r#"{"id":{{string:body_id}},"limit":{{number:body_limit}},"vars":{{json:body_vars}},"enabled":{{boolean:body_enabled}}}"#;
        let extracted = extract_body_template_values(
            template,
            r#"{"id":"abc-123","limit":25,"vars":{"cursor":"next"},"enabled":true}"#,
        )
        .unwrap();

        assert_eq!(extracted.get("body_id"), Some(&"abc-123".to_string()));
        assert_eq!(extracted.get("body_limit"), Some(&"25".to_string()));
        assert_eq!(
            extracted.get("body_vars"),
            Some(&r#"{"cursor":"next"}"#.to_string())
        );
        assert_eq!(extracted.get("body_enabled"), Some(&"true".to_string()));
    }

    #[test]
    fn test_extract_form_body_template_values() {
        let template = "query={{string:body_form_query}}&page=1";
        let extracted =
            extract_body_template_values(template, "query=latest+report&page=1").unwrap();
        assert_eq!(
            extracted.get("body_form_query"),
            Some(&"latest report".to_string())
        );
    }
}
