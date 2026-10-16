//! Presentation-maker end-to-end evals (landed 2026-09-01 with the agent).
//!
//! What "end to end" can mean in a hermetic unit harness: the checked-in seed
//! definitions, parsed by the real loader and checked against the real
//! enforcement predicates — approval gating, transport ceilings and
//! delegation shape. Live OfficeCLI qualification belongs to the explicit
//! tool-skill/live lane; an ordinary unit test must not depend on one
//! developer's materialized runtime scope or pass a deck canary after merely
//! running `officecli --help`.

use super::{
    types::{AgentDefinition, ChatInlinePolicy, UserMemoryIsolation},
    AgentDelegationPolicy, AgentDiscoverability, InvocationSurface,
};

const SEED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../magician_data_v3/system/agent_templates/agents/presentation-maker/definition.agent.yaml"
);
const RESEARCH_SEED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../magician_data_v3/system/agent_templates/agents/web-researcher-opc/definition.agent.yaml"
);

fn load(path: &str) -> AgentDefinition {
    AgentDefinition::from_yaml_file(path)
        .unwrap_or_else(|error| panic!("{path} must parse and validate: {error}"))
}

// ── 1. The shipped definitions are loadable and scope-neutral ────────────

#[test]
fn the_shipped_definitions_parse_validate_and_carry_no_runtime_scope() {
    let seed = load(SEED);
    let researcher = load(RESEARCH_SEED);
    assert_eq!(seed.agent_id, "presentation-maker");
    assert_eq!(researcher.agent_id, "web-researcher-opc");
    for definition in [&seed, &researcher] {
        assert!(
            definition.principal.is_none() && definition.workspace.is_none(),
            "system templates must be scope-neutral; materialization stamps the owning scope"
        );
    }
}

// ── 2. Ask → approve → deliver: production is gated by configuration ────

#[test]
fn deck_production_requires_owner_approval() {
    let def = load(SEED);
    // Every office-powerpoint action must pause: build, edit, export —
    // the whole toolskill is under the rule, so nothing is produced
    // without the owner's answer through the user_requests rail.
    for action in ["create", "edit", "render", "export", "validate"] {
        let params = [("action".to_string(), serde_json::json!(action))]
            .into_iter()
            .collect();
        assert!(
            super::approval::agent_requires_approval_for_tool(
                &def.constraints,
                "office-powerpoint",
                &params
            ),
            "office-powerpoint:{action} must require approval"
        );
    }
}

#[test]
fn ungated_tools_do_not_pause_the_craft() {
    let def = load(SEED);
    // Research and design reading stay free — the gate is on PRODUCTION,
    // not on thinking; a chatty gate on every read would train the owner
    // to rubber-stamp.
    for tool in [
        "content_search",
        "content_read",
        "presentation-design-principles",
    ] {
        let params = std::collections::HashMap::new();
        assert!(
            !super::approval::agent_requires_approval_for_tool(&def.constraints, tool, &params),
            "{tool} should not require approval"
        );
    }
}

// ── 3. The security posture survives contact with the predicates ────────

#[test]
fn research_goes_through_the_tight_seat_only() {
    let def = load(SEED);
    assert_eq!(
        def.delegation_targets,
        vec!["web-researcher-opc".to_string()],
        "deck requests may originate from untrusted parties; the general \
         web-researcher reaches the owner's Chrome over CDP"
    );
}

#[test]
fn the_maker_never_reaches_the_owner_chrome() {
    let def = load(SEED);
    let transports = def.browser_transports.clone();
    assert!(
        transports.iter().any(|t| t == "headless" || t == "headed"),
        "fresh contexts must be allowed for QA"
    );
    assert!(
        !transports.iter().any(|t| t == "cdp"),
        "cdp attaches to the owner's signed-in Chrome; the ceiling must omit it"
    );
}

#[test]
fn no_shell_and_no_native_search_twins() {
    let def = load(SEED);
    let tools = def.tools.clone();
    assert!(!tools.iter().any(|t| t.contains("shell")), "no shell in V0");
    for denied in ["websearch-via-claude", "deep-research-with-claude"] {
        assert!(
            def.denied_tools.iter().any(|d| d == denied),
            "{denied} must be denied like the researcher seat"
        );
    }
}

// ── 4. The delegation hop is closed on the research side too ────────────

#[test]
fn the_research_seat_delegates_to_nobody() {
    let researcher = load(RESEARCH_SEED);
    assert!(
        researcher.delegation_targets.is_empty(),
        "the OPC seat must be terminal — hop propagation closed by grant shape"
    );
    assert!(
        !researcher.browser_transports.iter().any(|t| t == "cdp"),
        "the OPC seat must never reach CDP"
    );
    assert_eq!(
        researcher.invocation_policy.discoverability,
        AgentDiscoverability::SurfaceOnly,
        "surface_only withholds the universal shell/files/http rail from the tight seat"
    );
    assert_eq!(
        researcher.invocation_policy.delegation,
        AgentDelegationPolicy::Explicit,
        "only a source that names web-researcher-opc may delegate to it"
    );
    assert_eq!(
        researcher.invocation_policy.allowed_direct_surfaces,
        [InvocationSurface::Delegation]
    );
    assert_eq!(researcher.chat_inline, Some(ChatInlinePolicy::Off));
    assert_eq!(
        researcher.user_memory_isolation,
        UserMemoryIsolation::FullyIsolated,
        "public-web findings must not enter the owner's shared user memory"
    );
    assert_eq!(
        researcher.aliases,
        vec![
            "sleuth-opc".to_owned(),
            "researcher-opc".to_owned(),
            "wr-opc".to_owned(),
        ],
        "the delegation-only clone must not duplicate the ambient researcher's aliases"
    );
}

// ── 5. The full ask→approve→produce chain, predicate-composed ───────────
//
// Simulates the owner's end-to-end hand-gate at the enforcement layer:
// an inbound request from an untrusted party reaches the maker; every
// production action the maker would take pauses; the owner's approval
// (represented by the TTL ledger clearing) is the ONLY way forward; and
// the research hop underneath never widens the browser ceiling.

#[test]
fn the_end_to_end_chain_gates_at_every_layer() {
    let def = load(SEED);

    // Layer 1 — inbound: untrusted origin means narrowed research.
    let researcher = load(RESEARCH_SEED);
    assert!(!researcher.browser_transports.iter().any(|t| t == "cdp"));

    // Layer 2 — production: the first deck action pauses.
    let params = [("action".to_string(), serde_json::json!("create"))]
        .into_iter()
        .collect();
    assert!(super::approval::agent_requires_approval_for_tool(
        &def.constraints,
        "office-powerpoint",
        &params
    ));

    // Layer 3 — the gate is honest: an empty constraint set (the
    // misconfiguration that would silently skip approval) is detectable.
    let mut ungated = def.constraints.clone();
    ungated.requires_approval = Vec::new();
    assert!(!super::approval::agent_requires_approval_for_tool(
        &ungated,
        "office-powerpoint",
        &params
    ));
}
