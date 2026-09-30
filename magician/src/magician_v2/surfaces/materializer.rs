use serde_json::{json, Map, Value};
use thiserror::Error;

use super::{
    compiler::{compile_surface_spec, SurfaceCompileError},
    types::{SurfaceSpec, SURFACE_SCHEMA_VERSION},
};
use crate::magician_v2::gaui::MuijDocument;

const MAX_METRICS: usize = 8;
const MAX_TABLE_COLUMNS: usize = 8;
const MAX_TABLE_ROWS: usize = 20;
const MAX_TEXT_CHARS: usize = 12_000;

#[derive(Debug, Error)]
pub enum SurfaceMaterializationError {
    #[error("unsupported publication media type for dashboard materialization: {0}")]
    UnsupportedMediaType(String),
    #[error("dashboard materialization requires renderable source content")]
    MissingContent,
    #[error("surface compile failed: {0}")]
    Compile(#[from] SurfaceCompileError),
}

/// Optional source-file metadata so the materialised surface can carry an
/// inline "Download source" affordance alongside the rendered content. The
/// caller in `artifact_v2/service.rs::materialize_published_surface_if_requested`
/// builds this from the resolved `OutputRef` (and the richer-deliverable
/// path when the v0.6.441 swap fires).
#[derive(Debug, Clone)]
pub struct SurfaceDownloadHint<'a> {
    pub display_name: &'a str,
    pub url: &'a str,
    pub size_bytes: Option<u64>,
}

pub fn materialize_output_as_muij(
    surface_id: &str,
    document_key: &str,
    route: &str,
    title: &str,
    summary: Option<&str>,
    media_type: &str,
    text_content: Option<&str>,
    json_content: Option<&Value>,
    download_hint: Option<&SurfaceDownloadHint<'_>>,
) -> Result<MuijDocument, SurfaceMaterializationError> {
    let spec = build_surface_spec(
        route,
        title,
        summary,
        media_type,
        text_content,
        json_content,
        download_hint,
    )?;
    Ok(compile_surface_spec(&spec, surface_id, document_key)?)
}

fn build_surface_spec(
    route: &str,
    title: &str,
    summary: Option<&str>,
    media_type: &str,
    text_content: Option<&str>,
    json_content: Option<&Value>,
    download_hint: Option<&SurfaceDownloadHint<'_>>,
) -> Result<SurfaceSpec, SurfaceMaterializationError> {
    let mut spec = SurfaceSpec {
        surface_version: SURFACE_SCHEMA_VERSION.to_string(),
        target_route: route.to_string(),
        title: Some(title.to_string()),
        summary: summary.map(|value| truncate_chars(value.trim(), MAX_TEXT_CHARS)),
        sections: Vec::new(),
        actions: Vec::new(),
    };

    match media_type {
        "text/markdown" => {
            let content = require_text(text_content)?;
            spec.sections.push(json!({
                "kind": "markdown",
                "id": "overview",
                "title": "Overview",
                "content": truncate_chars(content, MAX_TEXT_CHARS),
            }));
        },
        "text/plain" => {
            let content = require_text(text_content)?;
            spec.sections.push(json!({
                "kind": "text",
                "id": "overview",
                "title": "Overview",
                "text": truncate_chars(content, MAX_TEXT_CHARS),
            }));
        },
        "text/html" => {
            let content = require_text(text_content)?;
            spec.sections.push(json!({
                "kind": "markdown",
                "id": "html",
                "title": "HTML",
                "content": fenced_block("html", content),
            }));
        },
        "application/xml" | "text/xml" => {
            let content = require_text(text_content)?;
            spec.sections.push(json!({
                "kind": "markdown",
                "id": "xml",
                "title": "XML",
                "content": fenced_block("xml", content),
            }));
        },
        "application/json" => {
            if let Some(value) = json_content {
                append_json_sections(&mut spec.sections, value);
            } else if let Some(content) = text_content
                .map(str::trim)
                .filter(|content| !content.is_empty())
            {
                // Oversized or structurally unsafe JSON is deliberately not
                // parsed into a complete Value merely to publish a surface.
                // Preserve a bounded source preview and the download hint.
                spec.sections.push(json!({
                    "kind": "markdown",
                    "id": "json",
                    "title": "JSON preview",
                    "content": fenced_block("json", content),
                }));
            }
        },
        other => {
            return Err(SurfaceMaterializationError::UnsupportedMediaType(
                other.to_string(),
            ));
        },
    }

    if spec.sections.is_empty() {
        return Err(SurfaceMaterializationError::MissingContent);
    }

    // Append a small "Source" section with a download link to the
    // materialised file so the published surface always exposes the
    // raw artefact alongside the inline render. Triggered by
    // `materialize_published_surface_if_requested` after the v0.6.441
    // richer-content swap so the link points at the file whose bytes
    // were inlined (markdown briefing, JSON dump, etc.) — not at the
    // auto user-summary stub.
    if let Some(hint) = download_hint {
        if !hint.url.trim().is_empty() {
            let label = if hint.display_name.trim().is_empty() {
                "source".to_string()
            } else {
                hint.display_name.to_string()
            };
            let size_suffix = match hint.size_bytes {
                Some(bytes) if bytes > 0 => format!(" ({})", format_size_for_display(bytes)),
                _ => String::new(),
            };
            spec.sections.push(json!({
                "kind": "markdown",
                "id": "surface-source-download",
                "title": "Source",
                "content": format!(
                    "[Download {label}{size_suffix}]({url})",
                    label = escape_markdown_link_label(&label),
                    size_suffix = size_suffix,
                    url = hint.url,
                ),
            }));
        }
    }

    Ok(spec)
}

fn escape_markdown_link_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn format_size_for_display(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn append_json_sections(sections: &mut Vec<Value>, value: &Value) {
    match value {
        Value::Object(map) => {
            if let Some(section) = metric_grid_from_object(map) {
                sections.push(section);
            }
            if let Some(section) = first_table_section_from_object(map) {
                sections.push(section);
            }
            if sections.is_empty() {
                sections.push(json!({
                    "kind": "markdown",
                    "id": "json",
                    "title": "JSON",
                    "content": fenced_block("json", &pretty_json(value)),
                }));
            }
        },
        Value::Array(items) => {
            if let Some(section) = table_section_from_array("data", "Data", items) {
                sections.push(section);
            } else {
                sections.push(json!({
                    "kind": "markdown",
                    "id": "json",
                    "title": "JSON",
                    "content": fenced_block("json", &pretty_json(value)),
                }));
            }
        },
        _ => {
            sections.push(json!({
                "kind": "text",
                "id": "value",
                "title": "Value",
                "text": render_json_scalar(value),
            }));
        },
    }
}

fn metric_grid_from_object(map: &Map<String, Value>) -> Option<Value> {
    let items = map
        .iter()
        .filter_map(|(key, value)| {
            if !is_scalar_json(value) {
                return None;
            }
            Some(json!({
                "label": humanize_key(key),
                "value": render_json_scalar(value),
            }))
        })
        .take(MAX_METRICS)
        .collect::<Vec<_>>();

    if items.is_empty() {
        None
    } else {
        Some(json!({
            "kind": "metric_grid",
            "id": "metrics",
            "title": "Metrics",
            "items": items,
        }))
    }
}

fn first_table_section_from_object(map: &Map<String, Value>) -> Option<Value> {
    map.iter().find_map(|(key, value)| match value {
        Value::Array(items) => table_section_from_array(key, &humanize_key(key), items),
        _ => None,
    })
}

fn table_section_from_array(id: &str, title: &str, items: &[Value]) -> Option<Value> {
    if items.is_empty() {
        return None;
    }

    if items.iter().all(Value::is_object) {
        let columns = object_columns(items)?;
        let rows = items
            .iter()
            .take(MAX_TABLE_ROWS)
            .filter_map(Value::as_object)
            .map(|row| {
                columns
                    .iter()
                    .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        return Some(json!({
            "kind": "table",
            "id": sanitize_section_id(id),
            "title": title,
            "columns": columns,
            "rows": rows,
        }));
    }

    if items.iter().all(is_scalar_json) {
        let rows = items
            .iter()
            .take(MAX_TABLE_ROWS)
            .map(|item| vec![item.clone()])
            .collect::<Vec<_>>();
        return Some(json!({
            "kind": "table",
            "id": sanitize_section_id(id),
            "title": title,
            "columns": ["value"],
            "rows": rows,
        }));
    }

    None
}

fn object_columns(items: &[Value]) -> Option<Vec<String>> {
    let mut columns = Vec::new();
    for item in items {
        let object = item.as_object()?;
        for key in object.keys() {
            if columns.iter().any(|existing| existing == key) {
                continue;
            }
            columns.push(key.clone());
            if columns.len() >= MAX_TABLE_COLUMNS {
                return Some(columns);
            }
        }
    }
    if columns.is_empty() {
        None
    } else {
        Some(columns)
    }
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn require_text(text_content: Option<&str>) -> Result<&str, SurfaceMaterializationError> {
    text_content
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(SurfaceMaterializationError::MissingContent)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut iter = value.chars();
    let truncated = iter.by_ref().take(max_chars).collect::<String>();
    if iter.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn fenced_block(language: &str, content: &str) -> String {
    format!(
        "```{language}\n{}\n```",
        truncate_chars(content.trim(), MAX_TEXT_CHARS)
    )
}

fn render_json_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => truncate_chars(value, MAX_TEXT_CHARS),
        other => truncate_chars(&other.to_string(), MAX_TEXT_CHARS),
    }
}

fn is_scalar_json(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn humanize_key(key: &str) -> String {
    key.replace(['_', '-'], " ")
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_section_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "section".to_string()
    } else {
        trimmed
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn materialize_markdown_output_as_muij() {
        let doc = materialize_output_as_muij(
            "surface-1",
            "published-surface-1",
            "/briefing",
            "Briefing",
            Some("Summary"),
            "text/markdown",
            Some("# Hello\n\nWorld"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(doc.agent_id, "published-surface-1");
        assert!(!doc.layout.is_empty());
    }

    #[test]
    fn materialize_json_output_as_muij() {
        let doc = materialize_output_as_muij(
            "surface-2",
            "published-surface-2",
            "/briefing",
            "Metrics",
            None,
            "application/json",
            None,
            Some(&json!({
                "revenue": 1200,
                "conversions": 42,
                "rows": [
                    {"name": "A", "value": 1},
                    {"name": "B", "value": 2}
                ]
            })),
            None,
        )
        .unwrap();

        assert!(!doc.layout.is_empty());
    }

    #[test]
    fn materialize_bounded_json_text_preview_without_complete_value() {
        let doc = materialize_output_as_muij(
            "surface-json-preview",
            "published-json-preview",
            "/briefing",
            "Large JSON",
            None,
            "application/json",
            Some("{\"rows\":[1,2\n\n[Inline preview truncated]"),
            None,
            Some(&SurfaceDownloadHint {
                display_name: "rows.json",
                url: "/outputs/rows.json",
                size_bytes: Some(8 * 1024 * 1024),
            }),
        )
        .expect("bounded JSON preview remains publishable");

        assert!(!doc.layout.is_empty());
    }
}
