//! Generic artifact references produced during an inner-loop run.
//!
//! This is metadata only. File promotion, access control, image loading, and
//! artifact-v2 indexing are later runtime-context phases.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Metadata for a file or durable output discovered during one primitive
/// inner-loop tool call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrimitiveArtifact {
    /// Stable artifact id once promoted. During early capture this may be a
    /// deterministic execution-local id.
    pub artifact_id: String,
    /// Logical kind such as `screenshot`, `download`, `pdf`, `file`, or
    /// tool-specific output kind.
    pub kind: String,
    /// Primitive tool that produced this artifact.
    pub source_tool: String,
    /// Provider tool-call id that produced this artifact.
    pub source_tool_call_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_absolute_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_absolute_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_download_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_download_url: Option<String>,
}

impl PrimitiveArtifact {
    /// Build a capture record for a known output path.
    pub fn from_path(
        kind: impl Into<String>,
        source_tool: impl Into<String>,
        source_tool_call_id: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            kind: kind.into(),
            source_tool: source_tool.into(),
            source_tool_call_id: source_tool_call_id.into(),
            original_path: Some(path.into()),
            ..Self::default()
        }
    }

    /// Fill source metadata that a dispatcher may not know at dispatch time.
    pub fn fill_source_defaults(
        &mut self,
        default_kind: &str,
        source_tool: &str,
        source_tool_call_id: &str,
        iteration: usize,
        ordinal: usize,
    ) {
        if self.kind.trim().is_empty() {
            self.kind = default_kind.to_string();
        }
        if self.source_tool.trim().is_empty() {
            self.source_tool = source_tool.to_string();
        }
        if self.source_tool_call_id.trim().is_empty() {
            self.source_tool_call_id = source_tool_call_id.to_string();
        }
        if self.iteration.is_none() {
            self.iteration = Some(iteration);
        }
        if self.artifact_id.trim().is_empty() {
            self.artifact_id = format!(
                "primitive:{}:{}:{}:{}",
                iteration,
                sanitize_component(source_tool),
                sanitize_component(source_tool_call_id),
                ordinal
            );
        }
        if self.size_bytes.is_none() {
            self.size_bytes = self
                .effective_path()
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len());
        }
        if self.content_type.is_none() {
            self.content_type = self.effective_path().and_then(infer_content_type);
        }
    }

    /// Best path to show to a model/operator before artifact promotion.
    pub fn effective_path(&self) -> Option<&Path> {
        self.original_path.as_deref().or_else(|| {
            self.task_absolute_path
                .as_deref()
                .or(self.execution_absolute_path.as_deref())
                .map(Path::new)
        })
    }

    /// Best URL to show to a model/operator after artifact promotion.
    pub fn effective_url(&self) -> Option<&str> {
        self.task_download_url
            .as_deref()
            .or(self.execution_download_url.as_deref())
    }

    /// Compact JSON shape used in prompt results, traces, and outer outcomes.
    pub fn to_manifest_value(&self) -> Value {
        let mut value = json!({
            "artifact_id": self.artifact_id,
            "kind": self.kind,
            "source_tool": self.source_tool,
            "source_tool_call_id": self.source_tool_call_id,
        });
        let object = value.as_object_mut().expect("manifest value is object");
        if let Some(iteration) = self.iteration {
            object.insert("iteration".to_string(), json!(iteration));
        }
        if let Some(path) = self.effective_path() {
            object.insert(
                "path".to_string(),
                Value::String(path.display().to_string()),
            );
        }
        if let Some(content_type) = self.content_type.as_deref() {
            object.insert(
                "content_type".to_string(),
                Value::String(content_type.to_string()),
            );
        }
        if let Some(size_bytes) = self.size_bytes {
            object.insert("size_bytes".to_string(), json!(size_bytes));
        }
        if let Some(path) = self.task_absolute_path.as_deref() {
            object.insert(
                "task_absolute_path".to_string(),
                Value::String(path.to_string()),
            );
        }
        if let Some(path) = self.execution_absolute_path.as_deref() {
            object.insert(
                "execution_absolute_path".to_string(),
                Value::String(path.to_string()),
            );
        }
        if let Some(url) = self.task_download_url.as_deref() {
            object.insert(
                "task_download_url".to_string(),
                Value::String(url.to_string()),
            );
        }
        if let Some(url) = self.execution_download_url.as_deref() {
            object.insert(
                "execution_download_url".to_string(),
                Value::String(url.to_string()),
            );
        }
        value
    }
}

/// Discover common file path fields from a structured tool output. This is a
/// fallback behind explicit dispatcher-provided artifacts.
pub fn discover_artifact_paths(value: &Value) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    discover_artifact_paths_inner(value, None, &mut paths);
    dedupe_paths(paths)
}

fn discover_artifact_paths_inner(value: &Value, key_hint: Option<&str>, paths: &mut Vec<PathBuf>) {
    match value {
        Value::Array(items) => {
            for item in items {
                discover_artifact_paths_inner(item, key_hint, paths);
            }
        },
        Value::Object(map) => {
            for collection_key in ["images", "files", "artifacts", "outputs"] {
                if let Some(items) = map.get(collection_key) {
                    discover_artifact_paths_inner(items, Some(collection_key), paths);
                }
            }
            for key in [
                "path",
                "output_path",
                "output_file",
                "export_path",
                "save_path",
                "download_path",
            ] {
                if let Some(path) = map.get(key).and_then(Value::as_str) {
                    push_path(path, paths);
                }
            }
            if has_file_metadata(map) {
                if let Some(path) = map.get("file").and_then(Value::as_str) {
                    push_path(path, paths);
                }
            }
            for (key, child) in map {
                if matches!(
                    key.as_str(),
                    "images"
                        | "files"
                        | "artifacts"
                        | "outputs"
                        | "path"
                        | "output_path"
                        | "output_file"
                        | "export_path"
                        | "save_path"
                        | "download_path"
                        | "file"
                ) {
                    continue;
                }
                discover_artifact_paths_inner(child, key_hint.or(Some(key.as_str())), paths);
            }
        },
        Value::String(path)
            if matches!(
                key_hint,
                Some("images") | Some("files") | Some("artifacts") | Some("outputs")
            ) =>
        {
            push_path(path, paths);
        },
        _ => {},
    }
}

fn has_file_metadata(map: &serde_json::Map<String, Value>) -> bool {
    [
        "content_type",
        "mime_type",
        "size_bytes",
        "artifact_kind",
        "kind",
    ]
    .iter()
    .any(|key| map.contains_key(*key))
}

fn push_path(raw: &str, paths: &mut Vec<PathBuf>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return;
    }
    paths.push(PathBuf::from(trimmed));
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    deduped
}

fn infer_content_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let content_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        _ => return None,
    };
    Some(content_type.to_string())
}

fn sanitize_component(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fill_source_defaults_populates_missing_fields() {
        let mut artifact = PrimitiveArtifact::from_path("", "", "", "/tmp/shot.png");

        artifact.fill_source_defaults("screenshot", "screenshot", "tc-1", 7, 0);

        assert_eq!(artifact.kind, "screenshot");
        assert_eq!(artifact.source_tool, "screenshot");
        assert_eq!(artifact.source_tool_call_id, "tc-1");
        assert_eq!(artifact.iteration, Some(7));
        assert_eq!(artifact.artifact_id, "primitive:7:screenshot:tc-1:0");
    }

    #[test]
    fn manifest_value_uses_effective_path_and_url() {
        let artifact = PrimitiveArtifact {
            artifact_id: "art-1".into(),
            kind: "download".into(),
            source_tool: "download".into(),
            source_tool_call_id: "tc-2".into(),
            original_path: Some(PathBuf::from("/tmp/source.csv")),
            task_download_url: Some("/api/task/source.csv".into()),
            iteration: Some(3),
            ..PrimitiveArtifact::default()
        };

        let value = artifact.to_manifest_value();

        assert_eq!(value["artifact_id"], "art-1");
        assert_eq!(value["path"], "/tmp/source.csv");
        assert_eq!(value["task_download_url"], "/api/task/source.csv");
        assert_eq!(value["iteration"], 3);
    }

    #[test]
    fn discover_artifact_paths_reads_common_output_shapes() {
        let value = json!({
            "images": [{"path": "/tmp/a.png"}],
            "files": ["/tmp/b.csv"],
            "output_path": "/tmp/c.pdf",
            "nested": {"artifact": {"export_path": "/tmp/d.json"}},
            "file": "/tmp/ignored-without-metadata",
            "with_meta": {"file": "/tmp/e.txt", "content_type": "text/plain"}
        });

        let paths = discover_artifact_paths(&value);

        assert!(paths.contains(&PathBuf::from("/tmp/a.png")));
        assert!(paths.contains(&PathBuf::from("/tmp/b.csv")));
        assert!(paths.contains(&PathBuf::from("/tmp/c.pdf")));
        assert!(paths.contains(&PathBuf::from("/tmp/d.json")));
        assert!(paths.contains(&PathBuf::from("/tmp/e.txt")));
        assert!(!paths.contains(&PathBuf::from("/tmp/ignored-without-metadata")));
    }

    #[test]
    fn fill_source_defaults_infers_content_type_from_path() {
        let mut artifact = PrimitiveArtifact::from_path("screenshot", "", "", "/tmp/shot.png");

        artifact.fill_source_defaults("screenshot", "screenshot", "tc-1", 1, 0);

        assert_eq!(artifact.content_type.as_deref(), Some("image/png"));
    }
}
