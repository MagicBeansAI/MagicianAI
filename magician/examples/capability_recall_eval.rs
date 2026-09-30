//! Provider-free recall eval for tool_search and find_agents_for_capability.
//!
//! Walks every shipped agent template and compiled pack, plus a golden set of
//! natural-language queries (the Instinct voice failures: "web research",
//! "web search"). Prints JSON. Golden misses fail the process.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use magician::magician_v2::agents::AgentDefinition;
use magician::magician_v2::chat::service::resolved_chat_tools_from_definition;
use magician::magician_v2::execution::compiled_handlers::find_agents_for_capability::{
    identity_owns_capability, normalize_capability_token,
};
use magician::magician_v2::execution::compiled_handlers::tool_search::search_response;
use magician::magician_v2::execution::embedded_compiled_pack_defs;
use magician::magician_v2::execution::flat_loop::{build_tool_index, ToolIndex};
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;
const GENERATED_BY: &str = "magician::capability_recall_eval";
const TOOL_TOP_K: usize = 8;

#[derive(Serialize)]
struct EvalExport {
    schema_version: u32,
    generated_by: &'static str,
    gate: &'static str,
    agent_count: usize,
    tool_leaf_count: usize,
    golden_passed: usize,
    golden_total: usize,
    auto_passed: usize,
    auto_total: usize,
    cases: Vec<RecallCase>,
}

#[derive(Serialize)]
struct RecallCase {
    kind: &'static str,
    query: String,
    expected: Vec<String>,
    hits: Vec<String>,
    golden: bool,
    passed: bool,
    note: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn load_agents(root: &Path) -> Vec<AgentDefinition> {
    let dir = root.join("magician_data_v3/system/agent_templates/agents");
    let mut agents = Vec::new();
    let entries = fs::read_dir(&dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("agent dir");
        let yaml = entry.path().join("definition.agent.yaml");
        if !yaml.is_file() {
            continue;
        }
        let text = fs::read_to_string(&yaml).expect("read agent yaml");
        match AgentDefinition::from_yaml_str(&text) {
            Ok(definition) => agents.push(definition),
            Err(error) => eprintln!("skip {}: {error}", yaml.display()),
        }
    }
    agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    agents
}

fn agent_hits(
    query: &str,
    agents: &[AgentDefinition],
    reachable: &BTreeSet<String>,
    index: &ToolIndex,
    pack_names: &[String],
) -> Vec<String> {
    let mut hits = Vec::new();
    let needle = normalize_capability_token(query);
    for agent in agents {
        if !reachable.contains(&agent.agent_id) {
            continue;
        }
        if identity_owns_capability(query, &agent.agent_id, &agent.name, &agent.aliases) {
            hits.push(agent.agent_id.clone());
            continue;
        }
        let tools = resolved_chat_tools_from_definition(agent, Some(pack_names));
        let pack_hit = tools.iter().any(|pack| {
            normalize_capability_token(pack) == needle
                || index
                    .leaf_names_for_pack(pack)
                    .iter()
                    .any(|leaf| normalize_capability_token(leaf) == needle)
        });
        if pack_hit {
            hits.push(agent.agent_id.clone());
        }
    }
    hits
}

fn tool_hits(query: &str, index: &ToolIndex) -> Vec<String> {
    search_response(Some(index), query, TOOL_TOP_K)["matches"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|entry| entry["name"].as_str().map(str::to_string))
        .collect()
}

fn case_hits_expected(hits: &[String], expected: &[String]) -> bool {
    expected.iter().any(|want| {
        hits.iter().any(|hit| {
            hit == want
                || hit.split("__").next() == Some(want.as_str())
                || want.split("__").next() == Some(hit.as_str())
        })
    })
}

fn main() {
    let root = repo_root();
    let agents = load_agents(&root);
    let packs = embedded_compiled_pack_defs();
    let index = build_tool_index(&packs);
    let pack_names: Vec<String> = packs.iter().map(|pack| pack.name.clone()).collect();
    let pa = agents
        .iter()
        .find(|agent| agent.agent_id == "personal-assistant")
        .expect("personal-assistant template");
    let mut reachable: BTreeSet<String> = pa.delegation_targets.iter().cloned().collect();
    reachable.insert(pa.agent_id.clone());

    let golden_agents: &[(&str, &[&str])] = &[
        ("web research", &["web-researcher"]),
        ("web_research", &["web-researcher"]),
        ("web-researcher", &["web-researcher"]),
        ("sleuth", &["web-researcher"]),
        ("researcher", &["web-researcher"]),
        ("magican", &["personal-assistant"]),
        ("executive assistant", &["executive-assistant"]),
        ("mac operator", &["mac-operator"]),
        ("android operator", &["android-operator"]),
        ("wealth manager", &["wealth-manager"]),
        ("writing assistant", &["writing-assistant"]),
        ("creative mind", &["creative-mind"]),
        ("data analyst", &["simple-data-analyst"]),
        ("harness sre", &["harness-sre"]),
    ];
    let golden_tools: &[(&str, &[&str])] = &[
        ("web search", &["web_search"]),
        ("web_search", &["web_search"]),
        ("search the web", &["web_search", "content_search"]),
        ("content search", &["content_search"]),
        ("search memory", &["search_memory"]),
        ("duckdb", &["duckdb", "duckdb__query"]),
        ("shell", &["shell"]),
        ("http", &["http"]),
        ("find agents", &["find_agents_for_capability"]),
        ("read result", &["read_result"]),
        ("join meeting", &["meeting"]),
        ("send email", &["agentmail-send", "presto-gmail"]),
        ("browser", &["browser"]),
    ];

    let mut cases = Vec::new();
    for (query, expected) in golden_agents {
        let expected: Vec<String> = expected.iter().map(|id| (*id).to_string()).collect();
        let hits = agent_hits(query, &agents, &reachable, &index, &pack_names);
        let passed = case_hits_expected(&hits, &expected);
        cases.push(RecallCase {
            kind: "agent",
            query: (*query).to_string(),
            expected,
            hits,
            golden: true,
            passed,
            note: if passed {
                "golden agent recall".into()
            } else {
                "golden agent miss".into()
            },
        });
    }
    for (query, expected) in golden_tools {
        let expected: Vec<String> = expected.iter().map(|id| (*id).to_string()).collect();
        let hits = tool_hits(query, &index);
        let passed = case_hits_expected(&hits, &expected);
        cases.push(RecallCase {
            kind: "tool",
            query: (*query).to_string(),
            expected,
            hits,
            golden: true,
            passed,
            note: if passed {
                "golden tool recall".into()
            } else {
                "golden tool miss".into()
            },
        });
    }

    for agent in &agents {
        if agent.agent_id.starts_with("system:") {
            continue;
        }
        let mut queries = vec![
            agent.agent_id.clone(),
            agent.agent_id.replace('-', " "),
            agent.name.clone(),
        ];
        queries.extend(agent.aliases.iter().cloned());
        queries.sort();
        queries.dedup();
        for query in queries {
            if query.trim().is_empty() {
                continue;
            }
            if cases
                .iter()
                .any(|case| case.kind == "agent" && case.query.eq_ignore_ascii_case(&query))
            {
                continue;
            }
            let expected = vec![agent.agent_id.clone()];
            let hits = agent_hits(&query, &agents, &reachable, &index, &pack_names);
            let passed = case_hits_expected(&hits, &expected);
            cases.push(RecallCase {
                kind: "agent",
                query,
                expected,
                hits,
                golden: false,
                passed,
                note: "auto agent id/name/alias".into(),
            });
        }
    }

    let mut tool_names: Vec<String> = Vec::new();
    for pack in index.pack_names() {
        tool_names.extend(index.leaf_names_for_pack(&pack));
    }
    tool_names.sort();
    tool_names.dedup();
    for name in tool_names {
        for query in [
            name.clone(),
            name.replace('_', " "),
            name.replace("__", " "),
        ] {
            if query.trim().is_empty() {
                continue;
            }
            if cases
                .iter()
                .any(|case| case.kind == "tool" && case.query.eq_ignore_ascii_case(&query))
            {
                continue;
            }
            let expected = vec![name.clone()];
            let hits = tool_hits(&query, &index);
            let passed = case_hits_expected(&hits, &expected);
            cases.push(RecallCase {
                kind: "tool",
                query,
                expected,
                hits,
                golden: false,
                passed,
                note: "auto tool name".into(),
            });
        }
    }

    let golden_total = cases.iter().filter(|case| case.golden).count();
    let golden_passed = cases
        .iter()
        .filter(|case| case.golden && case.passed)
        .count();
    let auto_total = cases.iter().filter(|case| !case.golden).count();
    let auto_passed = cases
        .iter()
        .filter(|case| !case.golden && case.passed)
        .count();
    let gate = if golden_passed == golden_total {
        "PASS"
    } else {
        "FAIL"
    };
    let export = EvalExport {
        schema_version: SCHEMA_VERSION,
        generated_by: GENERATED_BY,
        gate,
        agent_count: agents.len(),
        tool_leaf_count: index.len(),
        golden_passed,
        golden_total,
        auto_passed,
        auto_total,
        cases,
    };
    println!("{}", serde_json::to_string_pretty(&export).expect("json"));
    if gate != "PASS" {
        std::process::exit(1);
    }
}
