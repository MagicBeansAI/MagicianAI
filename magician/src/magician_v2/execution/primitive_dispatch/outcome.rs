//! Terminal-decision artifact materialisation.
//!
//! The outcome-folding / evidence-ledger machinery this module once held was
//! retired with the nested inner loop (flat-loop Phase 7). What remains is the
//! helper the outer agentic loop still calls to write a terminal decision's
//! inline artifacts to disk.

use serde_json::{json, Value};
use tokio::fs;
use tracing::warn;

use crate::magician_v2::execution::agentic::Artifact as DecisionArtifact;

/// Sanitise an LLM-supplied artifact name into a safe filename. Strips path
/// separators and traversal, collapses unsafe characters to `_`, caps length.
/// Empty / all-stripped names fall back to `artifact`.
fn sanitize_artifact_filename(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "artifact".to_string();
    }
    let basename = std::path::Path::new(trimmed)
        .file_name()
        .and_then(|os| os.to_str())
        .unwrap_or(trimmed);
    let cleaned: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned
        .trim_matches(|c: char| c == '.' || c == '_')
        .to_string();
    let final_name = if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed
    };
    if final_name.len() > 200 {
        final_name[..200].to_string()
    } else {
        final_name
    }
}

/// Write a terminal decision's inline artifacts into `target_dir`, returning a
/// JSON manifest entry per materialised file. Skips empty payloads and tolerates
/// per-file write failures (logged, not fatal).
pub async fn persist_decision_artifacts_to_dir(
    artifacts: &[DecisionArtifact],
    target_dir: &std::path::Path,
    source_label: &'static str,
) -> Vec<Value> {
    if artifacts.is_empty() {
        return Vec::new();
    }
    if let Err(error) = fs::create_dir_all(target_dir).await {
        warn!(
            target_dir = %target_dir.display(),
            %error,
            source = source_label,
            "failed to create terminal-artifact output dir; skipping materialisation"
        );
        return Vec::new();
    }

    let mut materialised = Vec::with_capacity(artifacts.len());
    let mut taken_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        let DecisionArtifact {
            name,
            content_type,
            data,
            ..
        } = artifact;
        if data.is_empty() {
            continue;
        }
        let mut filename = sanitize_artifact_filename(name);
        if taken_names.contains(&filename) {
            filename = format!("{}_{}", filename, index);
        }
        taken_names.insert(filename.clone());
        let path = target_dir.join(&filename);
        if let Err(error) = fs::write(&path, data).await {
            warn!(
                path = %path.display(),
                artifact = %name,
                %error,
                source = source_label,
                "failed to write terminal artifact; skipping"
            );
            continue;
        }
        materialised.push(json!({
            "kind": "terminal_decision",
            "path": path.to_string_lossy().to_string(),
            "name": name,
            "content_type": content_type,
            "size_bytes": data.len(),
            "source": source_label,
        }));
    }
    materialised
}
