//! `tool_search` — universal schema-fetcher for deferred tools.
//!
//! Flat-loop catalog (Phase 1 of the flatten plan) surfaces most pack actions
//! as bare names; this tool fetches their JSON schemas on demand so they become
//! callable. Two query forms:
//! - `select:Tool1,Tool2,...` — direct fetch by name (returns full schemas).
//! - plain keywords — CC-style ranked search across the deferred index.
//!
//! When no [`ToolIndex`] is installed on `AgentResources` (inner-loop mode,
//! where every tool is already loaded eagerly), the handler returns an inactive
//! shape so the LLM knows to pick from the tools it can already see.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::flat_loop::{
    selected_tool_names_from_query, ToolIndex, ToolIndexEntry,
};

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    if query.is_empty() {
        return Ok(json!({
            "status": "error",
            "reason": "tool_search requires a `query` argument. Use `select:Tool1,Tool2,...` for direct fetch by name, or plain keywords for ranked search.",
        }));
    }

    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(5)
        .max(1);

    let index = resources.tool_index();
    Ok(search_response(index.as_deref(), &query, max_results))
}

/// Pure response builder — testable without constructing `AgentResources`.
/// `index` is `None` when flat loop is inactive for this scope.
pub fn search_response(index: Option<&ToolIndex>, query: &str, max_results: usize) -> Value {
    search_response_filtered(index, query, max_results, None)
}

/// Agent-bound response builder. `allowed_names` is the immutable deferred
/// ceiling from the current owner policy; both exact selection and ranked
/// search are filtered before any schema or descriptive metadata is returned.
pub fn search_response_filtered(
    index: Option<&ToolIndex>,
    query: &str,
    max_results: usize,
    allowed_names: Option<&HashSet<String>>,
) -> Value {
    let Some(index) = index else {
        // Flat loop not active for this scope — every tool is already eager.
        return json!({
            "status": "ok",
            "mode": "inactive",
            "query": query,
            "matches": [],
            "total_deferred_tools": 0,
            "note": "tool_search inactive — execution is in inner-loop mode where the deferred-tool catalog is not used. Every tool the agent has access to is already loaded eagerly with its full schema; pick the tool you need directly from the tools you can see.",
        });
    };

    if let Some(names) = selected_tool_names_from_query(query) {
        let matches: Vec<Value> = index
            .select(&names)
            .into_iter()
            .filter(|entry| {
                allowed_names.map_or(true, |allowed| allowed.contains(entry.name.as_str()))
            })
            .map(|e| entry_to_match(e))
            .collect();
        // Spell out the next step so the model neither stalls on a perceived
        // "schema loaded but uncallable" gap nor believes a miss loaded
        // something: told "these tools are now available" over an empty match
        // list, a model called a name that was never loaded and fell back to
        // a worse tool.
        let how_to_call = if matches.is_empty() {
            "Nothing was loaded: none of the requested names is in this catalog. Search with plain keywords to find the right name, then call `select:<name>`."
        } else {
            "These tools are now available in your active tool list. On your next turn, call the one you need directly, like any other tool, passing the arguments from its schema above."
        };
        return json!({
            "status": "ok",
            "mode": "select",
            "query": query,
            "matches": matches,
            "total_deferred_tools": allowed_names.map(HashSet::len).unwrap_or_else(|| index.len()),
            "how_to_call": how_to_call,
        });
    }

    let matches: Vec<Value> = index
        // Rank the complete bounded in-process index before applying the
        // owner's immutable ceiling. Otherwise forbidden high-ranked entries
        // could consume `max_results` and hide a lower-ranked allowed match.
        .search(query, index.len())
        .into_iter()
        .filter(|entry| allowed_names.map_or(true, |allowed| allowed.contains(entry.name.as_str())))
        .take(max_results)
        .map(|e| entry_to_match(e))
        .collect();
    json!({
        "status": "ok",
        "mode": "search",
        "query": query,
        "matches": matches,
        "total_deferred_tools": allowed_names.map(HashSet::len).unwrap_or_else(|| index.len()),
        // A listing is not a load: a model that read a match as callable
        // called it, found it absent from its tool list, and gave up on it.
        "how_to_load": "Search results only — these tools are not loaded yet. To make one callable, call tool_search again with `select:<name>` (comma-separate several); it is in your tool list on the next turn.",
    })
}

fn entry_to_match(e: &ToolIndexEntry) -> Value {
    json!({
        "name": e.name,
        "description": e.description,
        "parameters_schema": e.parameters_schema,
        "guide": e.pack_guide_excerpt,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::flat_loop::{build_tool_index, ToolIndex};

    fn embedded_index() -> ToolIndex {
        let packs =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs();
        build_tool_index(&packs)
    }

    #[test]
    fn select_mode_returns_full_schema() {
        let index = embedded_index();
        let out = search_response(Some(&index), "select:save_preference", 5);
        let matches = out["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["name"], "save_preference");
        assert!(matches[0]["parameters_schema"]["properties"].is_object());
    }

    #[test]
    fn keyword_mode_ranks_results() {
        let index = embedded_index();
        let out = search_response(Some(&index), "duckdb", 3);
        let matches = out["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert!(matches.len() <= 3);
    }

    #[test]
    fn policy_filter_hides_forbidden_exact_and_ranked_matches() {
        let index = embedded_index();
        let allowed = HashSet::from(["save_preference".to_string()]);

        let exact = search_response_filtered(
            Some(&index),
            "select:save_preference,search_memory",
            5,
            Some(&allowed),
        );
        let exact_names = exact["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(exact_names, vec!["save_preference"]);

        let ranked = search_response_filtered(Some(&index), "preference", 1, Some(&allowed));
        assert!(!ranked["matches"].as_array().unwrap().is_empty());
        assert!(ranked["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"] == "save_preference"));
        assert_eq!(ranked["total_deferred_tools"], json!(1));
    }

    /// A select that matched nothing used to answer "these tools are now
    /// available in your active tool list"; the model believed it, called a
    /// name that was never loaded, and fell back to a worse tool.
    #[test]
    fn a_select_that_matches_nothing_says_nothing_was_loaded() {
        let index = embedded_index();
        let out = search_response(Some(&index), "select:no_such_tool,nor_this", 5);
        assert_eq!(out["mode"], "select");
        assert!(out["matches"].as_array().unwrap().is_empty());
        let how = out["how_to_call"].as_str().unwrap();
        assert!(how.contains("Nothing was loaded"), "{how}");
        assert!(how.contains("select:"), "{how}");
        assert!(!how.contains("now available"), "{how}");
    }

    /// Keyword results are a listing, not a load; the model must be told the
    /// next step or it calls a listed name that is not in its tool list.
    #[test]
    fn keyword_mode_tells_the_model_to_select_before_calling() {
        let index = embedded_index();
        let out = search_response(Some(&index), "preference", 3);
        assert_eq!(out["mode"], "search");
        let how = out["how_to_load"].as_str().unwrap();
        assert!(how.contains("select:<name>"), "{how}");
        assert!(how.contains("not loaded"), "{how}");
    }

    #[test]
    fn no_index_returns_inactive_shape() {
        let out = search_response(None, "anything", 5);
        assert_eq!(out["total_deferred_tools"], json!(0));
        assert_eq!(out["mode"], "inactive");
    }
}
