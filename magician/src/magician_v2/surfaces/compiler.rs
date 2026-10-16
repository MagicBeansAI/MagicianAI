use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;

use super::types::SurfaceSpec;
use crate::magician_v2::agents::storage::sanitize_segment;
use crate::magician_v2::gaui::{
    DefaultComponentRegistry, MuijComponent, MuijDocument, MuijValidationError,
};

const MAX_SECTION_COUNT: usize = 16;
const MAX_METRIC_ITEMS: usize = 24;
const MAX_TABLE_COLUMNS: usize = 12;
const MAX_TABLE_ROWS: usize = 200;
const MAX_ACTIVITY_ITEMS: usize = 100;
const MAX_TEXT_CHARS: usize = 12_000;

#[derive(Debug, Error)]
pub enum SurfaceCompileError {
    #[error("surface actions are not supported by the MVP compiler")]
    UnsupportedActions,
    #[error("surface contains {count} sections, exceeding the limit of {limit}")]
    TooManySections { count: usize, limit: usize },
    #[error("surface section at index {index} is invalid: {detail}")]
    InvalidSection { index: usize, detail: String },
    #[error("surface section '{section_id}' exceeds the limit of {limit} items")]
    SectionTooLarge { section_id: String, limit: usize },
    #[error("surface text payload exceeds the limit of {limit} characters")]
    TextTooLarge { limit: usize },
    #[error("compiled MUIJ validation failed: {0}")]
    Validation(#[from] MuijValidationError),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SurfaceSectionSpec {
    Text {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        text: String,
    },
    Markdown {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        content: String,
    },
    MetricGrid {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        items: Vec<MetricItemSpec>,
    },
    Table {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        columns: Vec<TableColumnSpec>,
        rows: Vec<Vec<Value>>,
    },
    ActivityFeed {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        items: Vec<ActivityItemSpec>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct MetricItemSpec {
    label: String,
    value: String,
    #[serde(default)]
    trend: Option<String>,
    #[serde(default)]
    trend_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum TableColumnSpec {
    Key(String),
    Detailed {
        key: String,
        #[serde(default)]
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ActivityItemSpec {
    id: String,
    #[serde(default = "default_activity_type")]
    item_type: String,
    actor: String,
    action: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    timestamp: Option<i64>,
}

fn default_activity_type() -> String {
    "update".to_string()
}

pub fn compile_surface_spec(
    spec: &SurfaceSpec,
    surface_id: &str,
    document_key: &str,
) -> Result<MuijDocument, SurfaceCompileError> {
    if !spec.actions.is_empty() {
        return Err(SurfaceCompileError::UnsupportedActions);
    }
    if spec.sections.len() > MAX_SECTION_COUNT {
        return Err(SurfaceCompileError::TooManySections {
            count: spec.sections.len(),
            limit: MAX_SECTION_COUNT,
        });
    }

    let mut root_children = Vec::new();
    if let Some(title) = spec
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        root_children.push(text_component(
            "surface-header-title",
            title,
            json!({ "children": title, "variant": "title" }),
        ));
    }
    if let Some(summary) = spec
        .summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        ensure_text_within_limit(summary)?;
        root_children.push(text_component(
            "surface-header-summary",
            summary,
            json!({ "children": summary, "variant": "body", "className": "surface-summary" }),
        ));
    }

    for (index, raw_section) in spec.sections.iter().enumerate() {
        let section: SurfaceSectionSpec =
            serde_json::from_value(raw_section.clone()).map_err(|err| {
                SurfaceCompileError::InvalidSection {
                    index,
                    detail: err.to_string(),
                }
            })?;
        root_children.push(compile_section(index, section)?);
    }

    let root = MuijComponent {
        id: "surface-root".to_string(),
        component_type: "Stack".to_string(),
        label: spec
            .title
            .clone()
            .unwrap_or_else(|| format!("Surface {}", surface_id)),
        source: None,
        query: None,
        props: json!({
            "gap": "var(--space-lg)"
        }),
        static_snapshot: None,
        children: root_children,
    };

    let doc = MuijDocument {
        muij_version: "1.0".to_string(),
        agent_id: document_key.to_string(),
        layout: vec![root],
        generated_at: Utc::now(),
    };

    let registry = DefaultComponentRegistry;
    doc.validate(&registry)?;
    Ok(doc)
}

fn compile_section(
    index: usize,
    section: SurfaceSectionSpec,
) -> Result<MuijComponent, SurfaceCompileError> {
    let (section_id, title, child) = match section {
        SurfaceSectionSpec::Text { id, title, text } => {
            ensure_text_within_limit(&text)?;
            let section_id = normalized_section_id(id.as_deref(), index);
            let title = title.unwrap_or_else(|| "Overview".to_string());
            (
                section_id.clone(),
                title.clone(),
                text_component(
                    &format!("section-{}-body", section_id),
                    &title,
                    json!({ "children": text, "variant": "body" }),
                ),
            )
        },
        SurfaceSectionSpec::Markdown { id, title, content } => {
            ensure_text_within_limit(&content)?;
            let section_id = normalized_section_id(id.as_deref(), index);
            let title = title.unwrap_or_else(|| "Details".to_string());
            (
                section_id.clone(),
                title.clone(),
                MuijComponent {
                    id: format!("section-{}-markdown", section_id),
                    component_type: "Markdown".to_string(),
                    label: title.clone(),
                    source: None,
                    query: None,
                    props: json!({ "content": content }),
                    static_snapshot: None,
                    children: Vec::new(),
                },
            )
        },
        SurfaceSectionSpec::MetricGrid { id, title, items } => {
            let section_id = normalized_section_id(id.as_deref(), index);
            if items.len() > MAX_METRIC_ITEMS {
                return Err(SurfaceCompileError::SectionTooLarge {
                    section_id,
                    limit: MAX_METRIC_ITEMS,
                });
            }
            let title = title.unwrap_or_else(|| "Metrics".to_string());
            let children = items
                .into_iter()
                .enumerate()
                .map(|(metric_index, metric)| MuijComponent {
                    id: format!("section-{}-metric-{}", section_id, metric_index),
                    component_type: "MetricCard".to_string(),
                    label: metric.label.clone(),
                    source: None,
                    query: None,
                    props: json!({
                        "label": metric.label,
                        "value": metric.value,
                        "trend": metric.trend,
                        "trendLabel": metric.trend_label,
                    }),
                    static_snapshot: None,
                    children: Vec::new(),
                })
                .collect();
            (
                section_id.clone(),
                title.clone(),
                MuijComponent {
                    id: format!("section-{}-grid", section_id),
                    component_type: "Grid".to_string(),
                    label: title.clone(),
                    source: None,
                    query: None,
                    props: json!({
                        "columns": 3,
                        "autoFit": true,
                        "minColumnWidth": "180px",
                        "gap": "var(--space-md)"
                    }),
                    static_snapshot: None,
                    children,
                },
            )
        },
        SurfaceSectionSpec::Table {
            id,
            title,
            columns,
            rows,
        } => {
            let section_id = normalized_section_id(id.as_deref(), index);
            if columns.len() > MAX_TABLE_COLUMNS {
                return Err(SurfaceCompileError::SectionTooLarge {
                    section_id,
                    limit: MAX_TABLE_COLUMNS,
                });
            }
            if rows.len() > MAX_TABLE_ROWS {
                return Err(SurfaceCompileError::SectionTooLarge {
                    section_id,
                    limit: MAX_TABLE_ROWS,
                });
            }
            let title = title.unwrap_or_else(|| "Records".to_string());
            let normalized_columns = columns
                .iter()
                .map(|column| match column {
                    TableColumnSpec::Key(key) => json!({ "key": key, "label": key }),
                    TableColumnSpec::Detailed { key, label } => {
                        json!({ "key": key, "label": label.clone().unwrap_or_else(|| key.clone()) })
                    },
                })
                .collect::<Vec<_>>();
            let rows = rows
                .into_iter()
                .map(|row| row_to_object(&columns, row))
                .collect::<Vec<_>>();
            (
                section_id.clone(),
                title.clone(),
                MuijComponent {
                    id: format!("section-{}-table", section_id),
                    component_type: "Table".to_string(),
                    label: title.clone(),
                    source: None,
                    query: None,
                    props: json!({
                        "columns": normalized_columns,
                        "rows": rows,
                        "presorted": true
                    }),
                    static_snapshot: None,
                    children: Vec::new(),
                },
            )
        },
        SurfaceSectionSpec::ActivityFeed { id, title, items } => {
            let section_id = normalized_section_id(id.as_deref(), index);
            if items.len() > MAX_ACTIVITY_ITEMS {
                return Err(SurfaceCompileError::SectionTooLarge {
                    section_id,
                    limit: MAX_ACTIVITY_ITEMS,
                });
            }
            let title = title.unwrap_or_else(|| "Activity".to_string());
            let items = items
                .into_iter()
                .map(|item| {
                    json!({
                        "id": item.id,
                        "type": item.item_type,
                        "actor": item.actor,
                        "action": item.action,
                        "target": item.target,
                        "timestamp": item.timestamp.unwrap_or_else(|| Utc::now().timestamp_millis()),
                    })
                })
                .collect::<Vec<_>>();
            (
                section_id.clone(),
                title.clone(),
                MuijComponent {
                    id: format!("section-{}-activity", section_id),
                    component_type: "ActivityFeed".to_string(),
                    label: title.clone(),
                    source: None,
                    query: None,
                    props: json!({
                        "items": items
                    }),
                    static_snapshot: None,
                    children: Vec::new(),
                },
            )
        },
    };

    Ok(MuijComponent {
        id: format!("section-{}-card", section_id),
        component_type: "Card".to_string(),
        label: title.clone(),
        source: None,
        query: None,
        props: json!({
            "title": title
        }),
        static_snapshot: None,
        children: vec![child],
    })
}

fn text_component(id: &str, label: &str, props: Value) -> MuijComponent {
    MuijComponent {
        id: id.to_string(),
        component_type: "Text".to_string(),
        label: label.to_string(),
        source: None,
        query: None,
        props,
        static_snapshot: None,
        children: Vec::new(),
    }
}

fn ensure_text_within_limit(text: &str) -> Result<(), SurfaceCompileError> {
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(SurfaceCompileError::TextTooLarge {
            limit: MAX_TEXT_CHARS,
        });
    }
    Ok(())
}

fn normalized_section_id(raw: Option<&str>, index: usize) -> String {
    let base = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_segment)
        .unwrap_or_else(|| format!("section-{}", index + 1));
    if base.is_empty() {
        format!("section-{}", index + 1)
    } else {
        base
    }
}

fn row_to_object(columns: &[TableColumnSpec], row: Vec<Value>) -> serde_json::Map<String, Value> {
    let mut object = serde_json::Map::new();
    for (column_index, column) in columns.iter().enumerate() {
        let key = match column {
            TableColumnSpec::Key(key) => key,
            TableColumnSpec::Detailed { key, .. } => key,
        };
        let value = row.get(column_index).cloned().unwrap_or(Value::Null);
        object.insert(key.clone(), value);
    }
    object
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn compile_surface_spec_generates_valid_muij_document() {
        let spec = SurfaceSpec {
            surface_version: "1.0".to_string(),
            target_route: "/briefing".to_string(),
            title: Some("Campaign Briefing".to_string()),
            summary: Some("Key metrics for this week.".to_string()),
            sections: vec![
                json!({
                    "kind": "metric_grid",
                    "id": "kpis",
                    "title": "KPIs",
                    "items": [
                        { "label": "Revenue", "value": "$12.4k", "trend": "up", "trend_label": "+12%" },
                        { "label": "Conversions", "value": "48", "trend": "flat", "trend_label": "steady" }
                    ]
                }),
                json!({
                    "kind": "table",
                    "id": "records",
                    "title": "Top Campaigns",
                    "columns": ["campaign", "revenue"],
                    "rows": [
                        ["Search", "$8.1k"],
                        ["Social", "$4.3k"]
                    ]
                }),
            ],
            actions: Vec::new(),
        };

        let doc = compile_surface_spec(&spec, "surf-123", "surface-surf-123").unwrap();

        assert_eq!(doc.agent_id, "surface-surf-123");
        assert_eq!(doc.layout.len(), 1);
        assert_eq!(doc.layout[0].component_type, "Stack");
        assert!(doc.layout[0]
            .children
            .iter()
            .any(|component| component.component_type == "Card"));
    }

    #[test]
    fn compile_surface_spec_rejects_actions() {
        let spec = SurfaceSpec {
            surface_version: "1.0".to_string(),
            target_route: "/briefing".to_string(),
            title: Some("Unsafe".to_string()),
            summary: None,
            sections: Vec::new(),
            actions: vec![json!({ "kind": "open" })],
        };

        let err = compile_surface_spec(&spec, "surf-unsafe", "surface-surf-unsafe").unwrap_err();
        assert!(matches!(err, SurfaceCompileError::UnsupportedActions));
    }
}
