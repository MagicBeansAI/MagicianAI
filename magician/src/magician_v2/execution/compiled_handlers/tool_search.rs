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

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::flat_loop::{
    selected_tool_names_from_query, ToolIndex, ToolIndexEntry,
};
use crate::magician_v2::execution::SkillLoadFailure;

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
    search_response_with_loaded(index, query, max_results, allowed_names, None)
}

/// Why a tool is never offered to the model, if it is not: the §4A projection
/// withholds it (an outward capability with no bindable form, refused at
/// dispatch on every call). The catalog keeps such a tool out of both tiers;
/// `tool_search` must agree. When it did not, a chat model searched for
/// `agentmail-send`, selected it, was told it was loaded, and re-selected a
/// tool that never reached its list until the turn timed out (2026-10-11).
pub fn withheld_from_offer(index: &ToolIndex, name: &str) -> Option<String> {
    use crate::magician_v2::execution::restricted_toolset::{project_index_entry, ToolProjection};
    let entry = index.get(name)?;
    match project_index_entry(entry) {
        ToolProjection::Withheld { reason } => Some(reason),
        _ => None,
    }
}

/// [`search_response_filtered`] for a surface that knows which tools are
/// already in the caller's provider tool list (`callable_names`). A keyword
/// match that is already callable is marked `"loaded": true` and the
/// guidance says so. Without the mark every listing read "not loaded yet":
/// on 2026-10-08 a chat turn searched for a tool it could already call,
/// re-selected it, and spent loop iterations re-loading it.
pub fn search_response_with_loaded(
    index: Option<&ToolIndex>,
    query: &str,
    max_results: usize,
    allowed_names: Option<&HashSet<String>>,
    callable_names: Option<&HashSet<String>>,
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
        let allowed = |name: &str| allowed_names.map_or(true, |allowed| allowed.contains(name));
        let unavailable: Vec<Value> = index
            .select(&names)
            .into_iter()
            .filter(|entry| allowed(&entry.name))
            .filter_map(|entry| {
                withheld_from_offer(index, &entry.name)
                    .map(|reason| json!({ "name": entry.name, "reason": reason, "withheld": true }))
            })
            .collect();
        let matches: Vec<Value> = index
            .select(&names)
            .into_iter()
            .filter(|entry| allowed(&entry.name))
            .filter(|entry| withheld_from_offer(index, &entry.name).is_none())
            .map(|e| entry_to_match(e))
            .collect();
        if matches.is_empty() && !unavailable.is_empty() {
            return json!({
                "status": "ok",
                "mode": "select",
                "query": query,
                "matches": [],
                "unavailable": unavailable,
                "total_deferred_tools": allowed_names.map(HashSet::len).unwrap_or_else(|| index.len()),
                "how_to_call": "Nothing was loaded: the requested tool cannot be called in this run (see `unavailable`). Do not select it again. Use another tool or agent that can do the job, or tell the user it is not available here.",
            });
        }
        // Spell out the next step so the model neither stalls on a perceived
        // "schema loaded but uncallable" gap nor believes a miss loaded
        // something: told "these tools are now available" over an empty match
        // list, a model called a name that was never loaded and fell back to
        // a worse tool.
        // A tool already in the list says so. Told "now available … on your
        // next turn" for a tool it already had, a chat model re-selected it
        // until the spin guard ended the turn (run7, 2026-10-12).
        let mut matches = matches;
        let mut all_loaded = !matches.is_empty();
        for value in &mut matches {
            let loaded = value["name"]
                .as_str()
                .is_some_and(|name| callable_names.is_some_and(|callable| callable.contains(name)));
            if loaded {
                value["loaded"] = Value::Bool(true);
            }
            all_loaded &= loaded;
        }
        let how_to_call = if matches.is_empty() {
            "Nothing was loaded: none of the requested names is in this catalog. Search with plain keywords to find the right name, then call `select:<name>`."
        } else if all_loaded {
            "Already in your tool list: call it now, directly, with the arguments from its schema above. Do not select it again."
        } else {
            "These tools are now in your active tool list. Make your next tool call the one you need, directly, like any other tool, passing the arguments from its schema above. Do not select them again."
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

    // Rank the complete bounded in-process index before applying the owner's
    // immutable ceiling. Otherwise forbidden high-ranked entries could consume
    // `max_results` and hide a lower-ranked allowed match.
    let ranked = index
        .search(query, index.len())
        .into_iter()
        .filter(|entry| allowed_names.map_or(true, |allowed| allowed.contains(entry.name.as_str())))
        .collect::<Vec<_>>();
    // A granted tool this surface withholds is named, not hidden: a model
    // that found nothing for "send email" never learned to hand the work on.
    let unavailable: Vec<Value> = ranked
        .iter()
        .filter_map(|entry| {
            withheld_from_offer(index, &entry.name)
                .map(|reason| json!({ "name": entry.name, "reason": reason, "withheld": true }))
        })
        .take(max_results)
        .collect();
    let matches: Vec<Value> = ranked
        .into_iter()
        .filter(|entry| withheld_from_offer(index, &entry.name).is_none())
        .take(max_results)
        .map(|e| {
            let mut value = entry_to_match(e);
            if callable_names.is_some_and(|callable| callable.contains(e.name.as_str())) {
                if let Some(object) = value.as_object_mut() {
                    object.insert("loaded".to_string(), Value::Bool(true));
                }
            }
            value
        })
        .collect();
    let any_loaded = matches
        .iter()
        .any(|value| value["loaded"] == Value::Bool(true));
    // A listing is not a load: a model that read a match as callable
    // called it, found it absent from its tool list, and gave up on it. The
    // converse also holds: a match that IS callable must say so, or the model
    // re-selects a tool it already has.
    let how_to_load = if any_loaded {
        "Search results only. Matches marked `\"loaded\": true` are already in your tool list — call them directly, do not select them again. The others are not loaded yet: to make one callable, call tool_search again with `select:<name>` (comma-separate several); you can call it on your next tool call."
    } else {
        "Search results only — these tools are not loaded yet. To make one callable, call tool_search again with `select:<name>` (comma-separate several); you can call it on your next tool call."
    };
    let mut response = json!({
        "status": "ok",
        "mode": "search",
        "query": query,
        "matches": matches,
        "total_deferred_tools": allowed_names.map(HashSet::len).unwrap_or_else(|| index.len()),
        "how_to_load": how_to_load,
    });
    if !unavailable.is_empty() {
        response["unavailable"] = Value::Array(unavailable);
        response["unavailable_note"] =
            json!("Tools in `unavailable` cannot be called on this surface; do not select them.");
    }
    response
}

/// Stamp a committed load result with what is callable right now.
///
/// The system prompt is written once per turn and never says what loaded
/// since, so this result is where the model learns it. It is the last thing
/// the model reads before its next call, it reaches every provider as an
/// ordinary tool result, and the prompt prefix stays cached. A prompt that
/// still listed a loaded tool as deferred made a chat model doubt the tool
/// was callable: it re-selected `agentmail-send` until the turn ended
/// ("need the loaded tool available in this runtime's active tool
/// namespace", 2026-10-12).
pub fn stamp_callable_now(response: &mut Value, loaded_tools: &[String]) {
    const NAMED: usize = 20;
    let Some(object) = response.as_object_mut() else {
        return;
    };
    if loaded_tools.is_empty() {
        return;
    }
    let mut names: Vec<String> = loaded_tools
        .iter()
        .take(NAMED)
        .map(|name| format!("`{name}`"))
        .collect();
    if loaded_tools.len() > NAMED {
        names.push(format!("and {} more", loaded_tools.len() - NAMED));
    }
    let status = format!(
        "Loaded and callable now: {}. This is the current state of your tools and replaces \
         any earlier list that says otherwise.",
        names.join(", ")
    );
    let how = object
        .get("how_to_call")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    object.insert("callable_now".to_string(), json!(loaded_tools));
    object.insert(
        "how_to_call".to_string(),
        json!(format!("{status} {how}").trim_end().to_string()),
    );
}

/// Name, for each withheld tool in `response["unavailable"]`, the agents the
/// caller can delegate to that run it in its checked form. Chat sees a pack as
/// one free-text umbrella, which §4A withholds for anything that sends, so
/// chat sends by delegating. Told only "unavailable", a chat model handed an
/// email to an agent with no mail grant (2026-10-11). `owners` maps a tool name
/// to those agents; it must leave out the caller, which cannot run it here.
pub fn annotate_withheld_delegates(response: &mut Value, owners: impl Fn(&str) -> Vec<String>) {
    let Some(entries) = response
        .get_mut("unavailable")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let mut any = false;
    for entry in entries.iter_mut() {
        if entry["withheld"] != Value::Bool(true) {
            continue;
        }
        let Some(name) = entry["name"].as_str().map(str::to_string) else {
            continue;
        };
        let delegates = owners(&name);
        if !delegates.is_empty() {
            any = true;
            entry["delegate_to"] = json!(delegates);
        }
    }
    if any {
        let note = "This surface cannot call the tools in `unavailable`. Hand the work to an \
                    agent listed in that tool's `delegate_to` with `delegate_to_agent`; it runs \
                    the tool in its checked form. Do not select it again.";
        response["unavailable_note"] = json!(note);
        if response["mode"] == "select" && response["matches"].as_array().is_some_and(Vec::is_empty)
        {
            response["how_to_call"] = json!(note);
        }
    }
}

/// Granted tool names whose skill failed to load in this scope, mapped to the
/// loader's reason. A grant names a skill's pack or one of its
/// `<pack>__<action>` leaves; a name the index still serves (another root
/// loaded the same skill) is not unavailable. Only granted names are
/// returned, so the owner policy ceiling still decides what a caller learns.
pub fn unavailable_granted_tools<'a>(
    granted: impl IntoIterator<Item = &'a str>,
    failures: &[SkillLoadFailure],
    index: &ToolIndex,
) -> BTreeMap<String, String> {
    let mut unavailable = BTreeMap::new();
    if failures.is_empty() {
        return unavailable;
    }
    for name in granted {
        let Some(failure) = failures.iter().find(|failure| {
            !failure.skill.is_empty()
                && (name == failure.skill
                    || name
                        .strip_prefix(failure.skill.as_str())
                        .is_some_and(|rest| rest.starts_with("__")))
        }) else {
            continue;
        };
        if index.select(&[name.to_string()]).is_empty() {
            unavailable.insert(name.to_string(), failure.reason.clone());
        }
    }
    unavailable
}

/// Name the granted tools a query asked for that failed to load. Without
/// this, an exact-name search for a granted tool whose skill was rejected at
/// load answered `matches: []`, indistinguishable from "no such tool": the
/// model searched again and again, then told the user the action did not
/// exist. Select mode matches names exactly; keyword mode matches any term
/// contained in a name, as the ranked search's name scoring does.
pub fn annotate_unavailable_grants(
    response: &mut Value,
    query: &str,
    unavailable: &BTreeMap<String, String>,
) {
    if unavailable.is_empty() || response["status"] != "ok" {
        return;
    }
    let named: Vec<(&String, &String)> = match selected_tool_names_from_query(query) {
        Some(selected) => unavailable
            .iter()
            .filter(|(name, _)| selected.iter().any(|selected| selected == *name))
            .collect(),
        None => {
            let terms: Vec<String> = query
                .split_whitespace()
                .map(|term| term.trim_start_matches('+').to_ascii_lowercase())
                .filter(|term| !term.is_empty())
                .collect();
            unavailable
                .iter()
                .filter(|(name, _)| {
                    let name = name.to_ascii_lowercase();
                    terms.iter().any(|term| name.contains(term.as_str()))
                })
                .collect()
        },
    };
    if named.is_empty() {
        return;
    }
    // Appended: the search may already have named tools this surface
    // withholds, and a load failure does not replace them.
    let mut entries = response["unavailable"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    entries.extend(
        named
            .iter()
            .map(|(name, reason)| json!({"name": name, "reason": reason})),
    );
    response["unavailable"] = Value::Array(entries);
    response["unavailable_note"] = json!(
        "These tools are granted to you but failed to load in this workspace, so they cannot be \
         called and searching again will not find them. Tell the user the action is unavailable \
         because its tool failed to load."
    );
    if response["mode"] == "select"
        && response["matches"]
            .as_array()
            .is_some_and(|matches| matches.is_empty())
    {
        response["how_to_call"] = json!(
            "Nothing was loaded: the requested tools are granted but failed to load (see \
             `unavailable`)."
        );
    }
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

    /// A send-only entry that is NOT a whole-pack sub-run, with only a free-text
    /// `command`: a direct send §4A cannot bind, so it is withheld. A select
    /// must say it is unavailable — not that it is loaded — and a keyword
    /// search must not list it as callable. (Chat's real whole-pack entry is a
    /// sub-run and is offered; see `restricted_toolset`.)
    #[test]
    fn a_withheld_tool_is_neither_listed_nor_loaded() {
        let entry = |name: &str, pack: &str, schema: Value| ToolIndexEntry {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters_schema: schema,
            search_hint: None,
            pack_guide_excerpt: None,
            pack_name: pack.to_string(),
            result_projection: None,
            group: "pack",
            pack_definition_digest: String::new(),
            provenance: String::new(),
            runs_as_subrun: false,
        };
        let index = ToolIndex::from_entries(vec![
            entry(
                "agentmail-send",
                "agentmail-send",
                json!({"type": "object", "properties": {"command": {"type": "string"}},
                       "required": ["command"]}),
            ),
            entry(
                "agentmail-read",
                "agentmail-read",
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            ),
        ]);
        assert!(withheld_from_offer(&index, "agentmail-send").is_some());
        assert!(withheld_from_offer(&index, "agentmail-read").is_none());

        let selected = search_response(Some(&index), "select:agentmail-send", 5);
        assert!(selected["matches"].as_array().unwrap().is_empty());
        assert_eq!(selected["unavailable"][0]["name"], "agentmail-send");
        assert!(selected["how_to_call"]
            .as_str()
            .unwrap()
            .starts_with("Nothing was loaded"));

        let searched = search_response(Some(&index), "agentmail", 5);
        let names = searched["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["agentmail-read".to_string()]);
        // Named as unavailable, not hidden, so the model can hand it on.
        assert_eq!(searched["unavailable"][0]["name"], "agentmail-send");
        assert_eq!(searched["unavailable"][0]["withheld"], true);

        // The caller learns who can run it, and is told to delegate.
        let mut selected = selected;
        annotate_withheld_delegates(&mut selected, |name| {
            assert_eq!(name, "agentmail-send");
            vec!["company-assistant".to_string(), "envoy".to_string()]
        });
        assert_eq!(
            selected["unavailable"][0]["delegate_to"],
            json!(["company-assistant", "envoy"])
        );
        assert!(selected["how_to_call"]
            .as_str()
            .unwrap()
            .contains("delegate_to_agent"));

        // No reachable owner: the plain "unavailable" guidance stays.
        let mut alone = search_response(Some(&index), "select:agentmail-send", 5);
        annotate_withheld_delegates(&mut alone, |_| Vec::new());
        assert!(alone["unavailable"][0].get("delegate_to").is_none());
        assert!(alone["how_to_call"]
            .as_str()
            .unwrap()
            .starts_with("Nothing was loaded"));
    }

    #[test]
    fn a_committed_load_states_what_is_callable_now_first() {
        let index = embedded_index();
        let mut out = search_response(Some(&index), "select:save_preference", 5);
        let loaded = vec!["save_preference".to_string(), "agentmail-send".to_string()];
        stamp_callable_now(&mut out, &loaded);
        assert_eq!(out["callable_now"], json!(loaded));
        let how = out["how_to_call"].as_str().unwrap();
        assert!(how.starts_with("Loaded and callable now: `save_preference`, `agentmail-send`."));
        // The earlier guidance follows the status, not replaced by it.
        assert!(how.contains("Do not select them again"));

        let many: Vec<String> = (0..25).map(|i| format!("tool_{i}")).collect();
        let mut out = search_response(Some(&index), "select:save_preference", 5);
        stamp_callable_now(&mut out, &many);
        assert!(out["how_to_call"].as_str().unwrap().contains("and 5 more"));

        let mut untouched = search_response(Some(&index), "select:save_preference", 5);
        let before = untouched.clone();
        stamp_callable_now(&mut untouched, &[]);
        assert_eq!(untouched, before);
    }

    /// Selecting a tool the caller already has says so, instead of "now
    /// available on your next turn", which a model read as "select again".
    #[test]
    fn selecting_an_already_loaded_tool_says_call_it_now() {
        let index = embedded_index();
        let callable = HashSet::from(["save_preference".to_string()]);
        let out = search_response_with_loaded(
            Some(&index),
            "select:save_preference",
            5,
            None,
            Some(&callable),
        );
        assert_eq!(out["matches"][0]["loaded"], true);
        assert!(out["how_to_call"].as_str().unwrap().starts_with("Already in your tool list"));
        let fresh = search_response_with_loaded(Some(&index), "select:save_preference", 5, None, None);
        let fresh = fresh["how_to_call"].as_str().unwrap();
        assert!(fresh.starts_with("These tools are now in your active tool list"));
        // "Next turn" read as the owner's next message in chat.
        assert!(fresh.contains("next tool call") && !fresh.contains("next turn"));
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

    /// A tool already in the caller's tool list must not read "not loaded
    /// yet": a chat turn re-selected `tau2_get_order_details` it could already
    /// call, burning loop iterations on the reload.
    #[test]
    fn keyword_mode_marks_matches_that_are_already_callable() {
        let index = embedded_index();
        let callable = HashSet::from(["save_preference".to_string()]);
        let out = search_response_with_loaded(Some(&index), "preference", 5, None, Some(&callable));
        let matches = out["matches"].as_array().unwrap();
        let loaded = matches
            .iter()
            .find(|entry| entry["name"] == "save_preference")
            .expect("save_preference matches `preference`");
        assert_eq!(loaded["loaded"], json!(true));
        assert!(matches
            .iter()
            .filter(|entry| entry["name"] != "save_preference")
            .all(|entry| entry.get("loaded").is_none()));
        let how = out["how_to_load"].as_str().unwrap();
        assert!(how.contains("already in your tool list"), "{how}");
        assert!(how.contains("select:<name>"), "{how}");

        // No callable set: the legacy listing text, no marks.
        let plain = search_response_filtered(Some(&index), "preference", 5, None);
        assert!(plain["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry.get("loaded").is_none()));
    }

    fn broken(skill: &str) -> SkillLoadFailure {
        SkillLoadFailure {
            skill: skill.to_string(),
            path: format!("/scope/skills/{skill}/SKILL.md"),
            reason: "failed to compile governed runtime package: bad bounds".to_string(),
        }
    }

    /// A granted skill rejected at load used to answer an exact-name search
    /// with `matches: []`; the model searched six more times and told the
    /// user the action did not exist.
    #[test]
    fn a_granted_tool_that_failed_to_load_is_named_with_its_reason() {
        let index = embedded_index();
        let failures = vec![broken("exchange_items"), broken("not_granted")];
        let unavailable =
            unavailable_granted_tools(["exchange_items", "save_preference"], &failures, &index);
        assert_eq!(
            unavailable.keys().collect::<Vec<_>>(),
            vec!["exchange_items"]
        );

        for query in ["select:exchange_items", "exchange_items", "exchange"] {
            let mut out = search_response(Some(&index), query, 5);
            annotate_unavailable_grants(&mut out, query, &unavailable);
            let named = out["unavailable"].as_array().expect(query);
            assert_eq!(named.len(), 1, "{query}");
            assert_eq!(named[0]["name"], "exchange_items");
            assert!(named[0]["reason"]
                .as_str()
                .unwrap()
                .contains("failed to compile"));
            assert!(!named[0].to_string().contains("/scope/"), "no host paths");
        }
        let mut select = search_response(Some(&index), "select:exchange_items", 5);
        annotate_unavailable_grants(&mut select, "select:exchange_items", &unavailable);
        assert!(select["how_to_call"]
            .as_str()
            .unwrap()
            .contains("failed to load"));

        let mut unrelated = search_response(Some(&index), "preference", 5);
        annotate_unavailable_grants(&mut unrelated, "preference", &unavailable);
        assert!(unrelated.get("unavailable").is_none());
    }

    #[test]
    fn a_served_name_is_not_unavailable_and_leaf_grants_match_their_pack() {
        let index = embedded_index();
        // Another root loaded `save_preference`, so the failed copy hides nothing.
        let shadowed =
            unavailable_granted_tools(["save_preference"], &[broken("save_preference")], &index);
        assert!(shadowed.is_empty());
        // A `<pack>__<action>` grant belongs to its failed pack; a name that
        // merely shares a prefix does not.
        let leaves =
            unavailable_granted_tools(["gizmo__run", "gizmo_extra"], &[broken("gizmo")], &index);
        assert_eq!(leaves.keys().collect::<Vec<_>>(), vec!["gizmo__run"]);
    }

    #[test]
    fn no_index_returns_inactive_shape() {
        let out = search_response(None, "anything", 5);
        assert_eq!(out["total_deferred_tools"], json!(0));
        assert_eq!(out["mode"], "inactive");
    }
}
