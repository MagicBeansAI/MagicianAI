//! Per-server tool cards for governed MCP packs.
//!
//! A governed MCP pack exposes three actions (`status`, `list_tools`,
//! `call_tool`); the server's real tools sit behind `call_tool`. Without a map
//! of them the model pages through `list_tools` mid-task — in one run it read
//! tools 6 and 9 of a fifteen-tool list four times. Every discovery already
//! fetches the full tool list, so it records a compact card here: one line per
//! remote tool with its required and optional arguments and the first sentence
//! of its description. The card is keyed by a fingerprint of the projected
//! catalog and rewritten only when the server's tools change, and it is shown
//! with the pack's guide whenever the pack is loaded.
//!
//! The card is derived mechanically from the server's own definitions; nothing
//! in it is generated, so it cannot invent a tool or an argument.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const CARD_DIR: &str = "runtime/mcp_affordances";
const DESCRIPTION_CHARS: usize = 180;
const MAX_CARD_TOOLS: usize = 80;

#[derive(Debug, Serialize, Deserialize)]
struct StoredCard {
    fingerprint: String,
    pack: String,
    namespace: String,
    tool_count: usize,
    card: String,
}

fn card_path(storage_root: &Path, principal: &str, workspace: &str, pack: &str) -> PathBuf {
    let file = pack
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    ArtifactV2Workspace::new(storage_root.to_path_buf())
        .scope_root(principal, workspace)
        .join(CARD_DIR)
        .join(format!("{file}.json"))
}

fn fingerprint(tools: &[Value]) -> String {
    let canonical = serde_json::to_string(tools).unwrap_or_default();
    blake3::hash(canonical.as_bytes()).to_hex()[..32].to_string()
}

fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = match flat.find(". ") {
        Some(end) => &flat[..=end],
        None => flat.as_str(),
    };
    if sentence.chars().count() > DESCRIPTION_CHARS {
        format!(
            "{}…",
            sentence.chars().take(DESCRIPTION_CHARS).collect::<String>()
        )
    } else {
        sentence.trim().to_string()
    }
}

fn tool_line(tool: &Value) -> Option<String> {
    let name = tool.get("name")?.as_str()?;
    let short_name = name.rsplit('.').next().unwrap_or(name);
    let schema = tool
        .get("parameters")
        .or_else(|| tool.get("inputSchema"))
        .or_else(|| tool.get("input_schema"));
    let properties: Vec<String> = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    let required: Vec<String> = schema
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let optional: Vec<&String> = properties
        .iter()
        .filter(|p| !required.contains(p))
        .collect();
    let mut signature = required.join(", ");
    if !optional.is_empty() {
        if !signature.is_empty() {
            signature.push_str("; ");
        }
        signature.push_str("optional: ");
        signature.push_str(
            &optional
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .map(first_sentence)
        .unwrap_or_default();
    Some(if description.is_empty() {
        format!("- `{short_name}`({signature})")
    } else {
        format!("- `{short_name}`({signature}) — {description}")
    })
}

/// The card text for one projected catalog.
pub(crate) fn render_card(pack: &str, tools: &[Value]) -> String {
    let mut lines = vec![format!(
        "Remote tools behind `{pack}` (from the server's live tool list; run one with \
         `call_tool` {{tool_name, arguments_json}} — no need to page through `list_tools`):"
    )];
    lines.extend(tools.iter().take(MAX_CARD_TOOLS).filter_map(tool_line));
    if tools.len() > MAX_CARD_TOOLS {
        lines.push(format!(
            "- … {} more; call `list_tools` for the rest.",
            tools.len() - MAX_CARD_TOOLS
        ));
    }
    lines.join("\n")
}

/// Record the card for a freshly discovered catalog. Cheap when nothing
/// changed (one small read); failures are logged and otherwise ignored — the
/// card is a convenience, never a dependency of the call.
pub(crate) fn record_catalog(
    storage_root: &Path,
    principal: Option<&str>,
    workspace: Option<&str>,
    pack: &str,
    namespace: &str,
    tools: &[Value],
) {
    let (Some(principal), Some(workspace)) = (principal, workspace) else {
        return;
    };
    if pack.trim().is_empty() || tools.is_empty() {
        return;
    }
    let path = card_path(storage_root, principal, workspace, pack);
    let fingerprint = fingerprint(tools);
    if let Ok(existing) = std::fs::read(&path) {
        if serde_json::from_slice::<StoredCard>(&existing)
            .is_ok_and(|stored| stored.fingerprint == fingerprint)
        {
            return;
        }
    }
    let stored = StoredCard {
        fingerprint,
        pack: pack.to_string(),
        namespace: namespace.to_string(),
        tool_count: tools.len(),
        card: render_card(pack, tools),
    };
    let result = path
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .and_then(|_| {
            let bytes = serde_json::to_vec_pretty(&stored).map_err(std::io::Error::other)?;
            crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes)
                .map_err(std::io::Error::other)
        });
    if let Err(error) = result {
        tracing::debug!(%error, pack, "could not record MCP tool card");
    }
}

/// The recorded card for a pack, if its server has been discovered before.
pub(crate) fn load_card(
    storage_root: &Path,
    principal: Option<&str>,
    workspace: Option<&str>,
    pack: &str,
) -> Option<String> {
    let path = card_path(storage_root, principal?, workspace?, pack);
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<StoredCard>(&bytes)
        .ok()
        .map(|stored| stored.card)
        .filter(|card| !card.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tools() -> Vec<Value> {
        vec![
            json!({
                "name": "metabase-mcp.metabase.execute_question",
                "description": "Run an existing saved question by its numeric id and return its rows. Prefer this when a saved question answers the request.",
                "parameters": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}
            }),
            json!({
                "name": "metabase-mcp.metabase.query",
                "description": "Run a prepared query and page through large results.",
                "parameters": {"type": "object", "properties": {"query": {}, "query_handle": {}, "continuation_token": {}}, "required": ["query_handle"]}
            }),
        ]
    }

    #[test]
    fn card_lists_each_tool_with_required_and_optional_arguments() {
        let card = render_card("metabase-mcp", &tools());
        assert!(card.contains("- `execute_question`(id) — Run an existing saved question by its numeric id and return its rows."));
        assert!(card.contains("- `query`(query_handle; optional: continuation_token, query)"));
        assert!(
            !card.contains("Prefer this"),
            "only the first sentence is kept"
        );
    }

    #[test]
    fn record_rewrites_only_when_the_catalog_changes_and_load_reads_it_back() {
        let root = tempfile::tempdir().unwrap();
        let mut tools = tools();
        record_catalog(
            root.path(),
            Some("anon"),
            Some("default"),
            "metabase-mcp",
            "ns",
            &tools,
        );
        let card = load_card(root.path(), Some("anon"), Some("default"), "metabase-mcp").unwrap();
        assert!(card.contains("execute_question"));
        tools.pop();
        record_catalog(
            root.path(),
            Some("anon"),
            Some("default"),
            "metabase-mcp",
            "ns",
            &tools,
        );
        let card = load_card(root.path(), Some("anon"), Some("default"), "metabase-mcp").unwrap();
        assert!(!card.contains("`query`"));
        assert!(load_card(root.path(), Some("other"), Some("default"), "metabase-mcp").is_none());
    }
}
