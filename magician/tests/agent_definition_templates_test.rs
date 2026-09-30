use std::path::{Path, PathBuf};

use magician::magician_v2::agents::types::UserMemoryIsolation;
use magician::magician_v2::agents::{AgentDefinition, InvocationSurface, SurfaceAudience};

fn collect_agent_definition_paths(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root)
        .unwrap_or_else(|err| panic!("read agent template directory {}: {err}", root.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|err| panic!("read entry under {}: {err}", root.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_agent_definition_paths(&path, out);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("definition.agent.yaml") {
            out.push(path);
        }
    }
}

#[test]
fn checked_in_agent_definitions_parse_and_validate() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    // The checked-in agent definitions are the canonical seed templates under
    // system/agent_templates. The per-scope agent_runtime defs are runtime
    // materializations (no longer tracked in the repo), so they aren't validated here.
    let roots = [workspace_root.join("magician_data_v3/system/agent_templates/agents")];

    let mut definitions = Vec::new();
    for root in roots {
        collect_agent_definition_paths(&root, &mut definitions);
    }
    definitions.sort();

    assert!(
        !definitions.is_empty(),
        "expected at least one checked-in agent definition under {}",
        workspace_root.display()
    );

    let mut failures = Vec::new();
    for path in definitions {
        if let Err(err) = AgentDefinition::from_yaml_file(&path) {
            failures.push(format!("{}: {err}", path.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "agent definitions should parse and validate:\n{}",
        failures.join("\n")
    );
}

#[test]
fn checked_in_agent_templates_are_scope_neutral() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");
    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();

    let mut scoped = Vec::new();
    for path in paths {
        let definition = AgentDefinition::from_yaml_file(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        if definition.principal.is_some() || definition.workspace.is_some() {
            scoped.push(format!(
                "{} -> principal={:?}, workspace={:?}",
                definition.agent_id, definition.principal, definition.workspace
            ));
        }
    }

    assert!(
        scoped.is_empty(),
        "system templates must not carry one developer's materialized runtime scope; scope is stamped during materialization:\n{}",
        scoped.join("\n")
    );
}

/// The envoy agent is the security boundary for ordinary public-chat contacts:
/// the grant IS the boundary (fail-closed). This pins the least-privilege
/// posture so a future edit can't silently widen it — only the four owner-
/// relay escalations, no cross-agent/owner memory reads, and fully-isolated
/// memory.
#[test]
fn envoy_definition_is_least_privilege() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let envoy_path = workspace_root
        .join("magician_data_v3/system/agent_templates/agents/envoy/definition.agent.yaml");

    let def = AgentDefinition::from_yaml_file(&envoy_path)
        .expect("envoy definition should parse and validate");

    // Escalation only. These four reach the owner; they never act as him.
    // Anything else on this grant would widen an untrusted surface.
    let mut tools = def.tools.clone();
    tools.sort();
    assert_eq!(
        tools,
        [
            "ask_owner",
            "notify_owner",
            "propose_meeting",
            "request_owner_action",
        ]
    );

    // The owner-consent tools (notify_owner + ask_owner / propose_meeting /
    // request_owner_action) are plain compiled tools — UserRequest emitters with
    // no HarnessScope / program-state / governance gate — NOT harness tools. So
    // envoy must NOT be harness-enabled: harness is the autonomous-coordinator
    // capability (fleet management + program steering), which a stranger-facing
    // gatekeeper has no business holding.
    assert!(
        def.harness.is_none(),
        "envoy must NOT be harness-enabled — its owner-relay tools are plain compiled tools"
    );

    // The grant must never include the dangerous fleet/lifecycle harness tools.
    for forbidden in [
        "create_agent",
        "update_agent",
        "retire_agent",
        "update_delegation",
        "create_task",
        "reassign_task",
        "create_proposal",
        "create_dashboard",
        "update_program_state",
        "evaluate_harness",
    ] {
        assert!(
            !def.tools.iter().any(|tool| tool == forbidden),
            "envoy must NOT grant the dangerous harness tool `{forbidden}`"
        );
    }

    // No cross-agent / owner memory reads.
    assert!(
        def.readable_agents.is_empty(),
        "envoy must not read any other agent's memory"
    );

    // Memory is fully isolated from the owner's tiers. This single field is the
    // seal against CROSS-MEETING reach as well as owner reach: meeting
    // takeaways land in the shared user root, and this is what redirects
    // envoy's user-memory reads away from it. See the tier assertion below for
    // why `memory_tiers: []` is not the mechanism it looks like.
    assert_eq!(
        def.user_memory_isolation,
        UserMemoryIsolation::FullyIsolated,
        "envoy memory must be fully isolated — this is what stops one room \
         reading another room's takeaways, not just the owner's"
    );

    // No upward delegation (cannot hand work to Presto or anyone else).
    assert!(
        def.delegation_targets.is_empty(),
        "envoy must not delegate to any other agent"
    );

    assert_eq!(
        def.default_personality.as_deref(),
        Some("witty"),
        "envoy is public-facing Presto and should keep Presto's default personality"
    );

    // Outward surfaces ONLY. The ambassador exists to be reachable from places
    // the owner's assistant must never be reachable from; listing an owner
    // surface here would make it reachable from both, which is the one shape
    // that defeats having a separate agent at all.
    let mut surfaces = def.invocation_policy.allowed_direct_surfaces.clone();
    surfaces.sort_by_key(|surface| surface.as_str());
    assert_eq!(
        surfaces,
        [InvocationSurface::Meeting, InvocationSurface::PublicEnvoy],
        "envoy must be reachable from outward surfaces and nothing else"
    );

    // `memory_tiers: []` in the YAML does NOT mean "no memory", and reading it
    // that way is a trap. Envoy is `kind: personal`, so `apply_defaults()`
    // injects the standard personal tier set PRECISELY BECAUSE the list is
    // empty (verified 2026-08-18). The seal against cross-meeting reach is
    // therefore `fully_isolated` above — never this list.
    //
    // What that seal actually does: a meeting's takeaways are filed under
    // `meeting:<thread-id>` into the USER tier `user.research_findings`, merged
    // by principal + workspace with no agent, so they live in the SHARED user
    // root. `FullyIsolated` redirects envoy's user-memory reads into
    // `agents/envoy/memory/user_memory/`, which is a different place — so the
    // meeting tier is unreachable from a room. Flip that field to `shared` and
    // every meeting becomes readable from every other one.
    //
    // This assertion is the tripwire for the other direction: envoy carrying a
    // HAND-WRITTEN tier list would suppress the defaults and means someone
    // deliberately gave the ambassador memory shaped for a purpose. That is a
    // decision that must be reviewed, not inherited silently.
    let mut tiers: Vec<&str> = def.memory_tiers.iter().map(|t| t.name.as_str()).collect();
    tiers.sort_unstable();
    assert_eq!(
        tiers,
        [
            "archive",
            "entities",
            "environment_knowledge",
            "insights",
            "recent_activity",
            "task_progress",
        ],
        "envoy should carry exactly the injected personal defaults; a hand-written \
         tier list on the ambassador needs review, not silent inheritance"
    );
    assert!(
        !def.memory_tiers
            .iter()
            .any(|tier| tier.name.contains("research_findings")),
        "envoy must not hold the tier meeting takeaways are filed into"
    );
}

/// Approval envelopes plan phase 1 — *"tag every current `requires_approval`
/// rule with its consequence class"*.
///
/// The phase exists to answer one question before anything is built on the
/// taxonomy: does it hold against the real inventory? So this walks the SHIPPED
/// rules rather than a list written from memory — a hardcoded list would agree
/// with itself and miss the rule somebody adds next month.
///
/// The classification carries a hard consequence: a rule that classifies as
/// `CommitmentOrTransaction` may **never** be covered by an envelope, standing
/// or reviewed batch. So a change here is a change to what an owner will always
/// be asked about.
#[test]
fn every_shipped_approval_rule_has_a_consequence_class() {
    use magician::magician_v2::agents::{
        consequence_class_for_approval_rule, ActionPattern, ConsequenceClass,
    };

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();

    let mut classified: Vec<String> = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        for rule in &def.constraints.requires_approval {
            // The rule's action matcher may name several actions or a wildcard;
            // classify each, because one commitment action in a rule makes the
            // whole rule reach a commitment.
            let actions: Vec<String> = match &rule.action {
                ActionPattern::Single(one) => vec![one.clone()],
                ActionPattern::Multiple(many) => many.clone(),
            };
            let mut classes: Vec<ConsequenceClass> = actions
                .iter()
                .map(|action| consequence_class_for_approval_rule(&rule.tool, action))
                .collect();
            classes.sort();
            classes.dedup();
            for class in classes {
                // The invariant, asserted per rule rather than per snapshot: an
                // author wrote `requires_approval` here, so the class must still
                // demand a gate. A snapshot alone would let a future rule
                // classify as private/local — needing no gate — and read as a
                // green because somebody updated the expected list to match.
                assert!(
                    class.requires_gate(),
                    "{} · {} is behind requires_approval but classifies as {}, \
                     which needs no gate. Once the envelope resolver decides \
                     from the class, this approval silently disappears.",
                    def.agent_id,
                    rule.tool,
                    class.as_str()
                );
                classified.push(format!(
                    "{} · {} -> {}",
                    def.agent_id,
                    rule.tool,
                    class.as_str()
                ));
            }
        }
    }
    classified.sort();
    classified.dedup();

    assert_eq!(
        classified,
        vec![
            // Every sending capability the ambassador holds is gated per act, in
            // addition to the central capture posture. All three classify as
            // bounded communication, so a standing envelope MAY cover them —
            // which is exactly why the class is pinned here rather than assumed.
            "ambassador · agentmail-send -> bounded_communication".to_string(),
            "ambassador · kapso-whatsapp-send -> bounded_communication".to_string(),
            "ambassador · presto-gmail -> bounded_communication".to_string(),
            // Not outward and not a known commitment, so it is an act somebody
            // gated whose consequence nobody has classified. It fails closed to
            // commitment rather than to private/local, which would have deleted
            // the gate its author asked for.
            "ceo · propose_program_missions -> commitment_or_transaction".to_string(),
            "cmo · agentmail-send -> bounded_communication".to_string(),
            // 2026-08-20, §5A.1 identity split: the company assistant's rules
            // moved off the OWNER's `gmail`/`calendar` onto the four company
            // channels. The rows it replaced were `company-assistant · gmail`
            // and `company-assistant · calendar`.
            "company-assistant · agentmail-send -> bounded_communication".to_string(),
            "company-assistant · kapso-whatsapp-send -> bounded_communication".to_string(),
            // Commitment, not bounded communication, and NOT a mistake. None of
            // `events_insert` / `events_patch` / `events_delete` is in
            // `outward_actions::SENDING_ACTIONS`, so the STATIC classifier a
            // rule is scored with cannot tell this guards an invitation, and it
            // fails closed. The rule this replaced named `create_event` and
            // `invite` — tokens no shipped Google skill has — so it scored as
            // bounded communication while matching nothing at all: a class that
            // read safer than the truth on a gate that did not exist.
            // `outward_dispatch_class` DOES catch the real dispatch, through
            // the required `args` passthrough rather than the token. When
            // `SENDING_ACTIONS` gains the real tokens this becomes
            // bounded_communication.
            "company-assistant · presto-calendar -> commitment_or_transaction".to_string(),
            "company-assistant · presto-gmail -> bounded_communication".to_string(),
            // The same rule's `raw` action, classified separately. `raw` is an
            // unbounded argv passthrough — `action=raw args=["+send", …]` sends
            // mail while the token says nothing (readiness review §9A) — so a
            // static (capability, action) pair cannot see that it transmits and
            // fails closed. That is the intended answer: it puts the one action
            // that can smuggle a send beyond the reach of any envelope.
            "company-assistant · presto-gmail -> commitment_or_transaction".to_string(),
            "executive-assistant · imessage_send -> bounded_communication".to_string(),
            "executive-assistant · swiggy-mcp -> commitment_or_transaction".to_string(),
            "executive-assistant · zepto-mcp -> commitment_or_transaction".to_string(),
            // Deck production is an owner-visible artifact mutation and remains
            // outside standing approval envelopes.
            "presentation-maker · office-powerpoint -> commitment_or_transaction".to_string(),
        ],
        "the approval inventory's consequence classes changed. A rule classified \
         as commitment can NEVER be covered by an envelope, so this is a change \
         to what the owner will always be asked about."
    );
}

/// Readiness review §9 step 7 — *"pause every outward agent individually"*.
///
/// The step is an operator action against an endpoint that already exists. What
/// did not exist was any way to answer **which** agents are outward, so the step
/// reduced to remembering — and a control you have to remember is not a control.
///
/// This pins the set. It is derived from the same table the dispatch capture
/// gate uses, so "outward" cannot mean two different things in two places, and
/// an agent that GAINS a sending tool shows up here as a failing test rather
/// than as a quiet new way to reach the world.
///
/// If this fails after a deliberate grant change, update the list — and pause
/// the new agent before the next unpause.
#[test]
fn the_set_of_outward_capable_shipped_agents_is_exactly_this() {
    use magician::magician_v2::agents::outward_actions::agent_outward_capabilities;

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();

    let mut outward: Vec<String> = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let reach = agent_outward_capabilities(&def.tools, &def.denied_tools);
        if !reach.is_empty() {
            let names: Vec<&str> = reach.iter().map(|(tool, _)| tool.as_str()).collect();
            outward.push(format!("{} -> {}", def.agent_id, names.join(", ")));
        }
    }
    outward.sort();

    assert_eq!(
        outward,
        vec![
            // The company's outward actor, and the only agent here whose whole
            // reason to exist is reaching people outside the company. Note WHICH
            // identities: `agentmail-send`, `kapso-whatsapp-send` and
            // `presto-gmail` are the company's own — `gmail` and `calendar`, the
            // owner's, are denied on that definition rather than merely absent.
            // It is paused individually like every other row here.
            //
            // 2026-08-21: `browser` LEFT this row. It was never gated the way
            // the row implied — the submitting-actions table matches `submit` /
            // `submit_form`, which the shipped skill does not declare, so a
            // submit-by-click classified as nothing and the capture gate never
            // saw it; and a visit defaults to CDP, the owner's own signed-in
            // Chrome. It is now in the ambassador's `denied_tools`, which
            // `agent_outward_capabilities` subtracts. The rows below still hold
            // it under exactly the same two gaps.
            "ambassador -> agentmail-send, kapso-whatsapp-send, presto-gmail".to_string(),
            // A company OFFICER that can email directly. Repointing its
            // delegation to `company-assistant` (§9 step 3) does NOT remove
            // this — which is why the capture gate had to be central rather
            // than a per-assistant rule. A per-agent approach would have
            // missed this entirely.
            "cmo -> agentmail-send".to_string(),
            // 2026-08-20, §5A.1 identity split: was `calendar, gmail` — the
            // OWNER's Google. The company assistant now reaches the world only
            // through Presto's own channels. Its reach did not shrink, so it
            // still must be paused individually before a global unpause; what
            // changed is WHOSE accounts a mistake would send from.
            "company-assistant -> agentmail-send, kapso-whatsapp-send, presto-calendar, presto-gmail"
                .to_string(),
            // `browser` is outward through the submitting-actions table: a
            // click can press somebody else's Submit control.
            "cpo -> browser".to_string(),
            "cro -> browser".to_string(),
            "executive-assistant -> browser, calendar, gmail, imessage_send, telegram, whatsapp"
                .to_string(),
            // The owner's primary agent. Outward by design, and captured by the
            // same central gate as everything else. `presto-calendar` joined
            // this row when it was added to `OUTWARD_CAPABILITIES` — the grant
            // was always here, and the inventory simply could not see it.
            "personal-assistant -> agentmail-send, browser, kapso-whatsapp-send, presto-calendar, presto-gmail"
                .to_string(),
            // Deck QA may submit through a fresh browser context. The
            // presentation maker is therefore outward-capable even though its
            // final package release is approval-gated separately.
            "presentation-maker -> browser".to_string(),
            // Granted `gmail` to READ financial mail — its persona says exactly
            // that — but the capability is not split, so the grant carries
            // send. "Agents that CAN send" is a wider set than "agents meant
            // to", and this is the difference.
            "wealth-manager -> gmail".to_string(),
            "web-researcher -> browser".to_string(),
            "web-researcher-opc -> browser".to_string(),
        ],
        "the set of agents that can reach a third party changed. Every agent \
         listed here must be paused individually before a global unpause \
         (§9 step 7), so a change to this set is a change to what must be paused."
    );
}

/// Readiness review §9 step 3 — the owner/company identity split.
///
/// No company officer may reach the OWNER's assistant. Before this split, `ceo`,
/// `cmo`, `cpo` and `cro` all listed `executive-assistant` as a delegation
/// target, so an officer running autonomously could reach the owner's inbox,
/// calendar and contacts through a delegation edge — a permission nobody granted
/// deliberately, and the reason the review puts this before global unpause.
///
/// Asserted over every shipped definition rather than the four that were edited,
/// because the failure to catch is a NEW officer added later with the old target
/// copied from a sibling.
#[test]
fn no_company_officer_may_delegate_to_the_owners_assistant() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();

    let mut offenders = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        // The owner's own agents may of course reach it; the company's may not.
        if def.agent_id == "personal-assistant" || def.agent_id == "executive-assistant" {
            continue;
        }
        if def
            .delegation_targets
            .iter()
            .any(|target| target == "executive-assistant")
        {
            offenders.push(def.agent_id.clone());
        }
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "these agents can still delegate to the OWNER's assistant, so the \
         company harness reaches the owner's accounts: {offenders:?}"
    );
}

/// The vacuity in the test directly above, closed.
///
/// That test asks whether the literal string `executive-assistant` appears in a
/// delegation list. `creative-mind` shipped `delegation_targets: ['*']`, which
/// the resolver expands to every enabled non-system agent — the owner's
/// assistant included — and which contains that string nowhere. So the test
/// passed while the route it exists to close was open, from `ceo` in two hops
/// (`ceo -> cto -> creative-mind` and `ceo -> vc-researcher -> creative-mind`).
/// A test that passes because the offending value is spelled differently is not
/// a weaker test, it is a green light for the exact case it was written for.
///
/// This pins the spelling instead of the expansion, because the spelling is
/// what the sibling test cannot see. Exactly ONE id may carry it, and it is not
/// an agent: `system:scheduler`, for which the resolver returns no targets at
/// all (`source.is_system_agent()` short-circuits before expansion), so the
/// wildcard there grants nothing.
///
/// `personal-assistant` was the last real holder and was narrowed on 2026-08-21
/// (see `the_owners_router_names_its_delegates` below). It was the owner's own
/// router, so its wildcard was never an officer's route in — but it reached
/// `executive-assistant` and the whole company roster without naming either,
/// which is the property this test exists to deny.
///
/// If this fails, do not add the new agent to the list without checking the
/// sibling test above — a wildcard is how an officer reaches the owner without
/// ever naming him.
#[test]
fn no_shipped_agent_may_carry_a_wildcard_delegation_target() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "expected shipped agent definitions");

    let mut wildcard_holders = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        if def.delegation_targets.iter().any(|target| target == "*") {
            wildcard_holders.push(def.agent_id.clone());
        }
    }
    wildcard_holders.sort();

    assert_eq!(
        wildcard_holders,
        ["system:scheduler"],
        "an agent gained `delegation_targets: ['*']`, which reaches the owner's \
         assistant without naming it and therefore passes \
         `no_company_officer_may_delegate_to_the_owners_assistant` vacuously"
    );
}

/// `creative-mind`'s replacement list is EMPTY, and empty is the enforced answer.
///
/// The wildcard could be undone two ways that both look like tidying: putting
/// `'*'` back, or "restoring" a plausible-looking list of helpers. Neither is
/// supported by anything — no test, prompt, program or recorded execution shows
/// this agent delegating, it has never owned a task, and its charter describes a
/// producer that publishes artifacts rather than a router. It is a leaf in every
/// route that exists (`cto`, `executive-assistant` and `vc-researcher` delegate
/// INTO it).
///
/// The second assertion is the one that makes the first mean something:
/// `resolve_effective_delegation_target_ids_for_surface` returns nothing when
/// the declared list is empty, so no target is reachable and the catalogue stops
/// offering `delegate_to_agent` / `handover_to_agent` entirely. Without it, an
/// empty list could later come to mean "unrestricted" — the same trap as an
/// empty `memory_tiers` on a personal agent — and this file would not notice.
#[test]
fn creative_mind_declares_no_delegation_targets_and_empty_grants_none() {
    use magician::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface;

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let path = workspace_root
        .join("magician_data_v3/system/agent_templates/agents/creative-mind/definition.agent.yaml");
    let def = AgentDefinition::from_yaml_file(&path)
        .expect("creative-mind definition should parse and validate");

    assert_eq!(
        def.delegation_targets,
        Vec::<String>::new(),
        "creative-mind must declare no delegation targets; `['*']` here is a \
         second route from the company harness to the owner's assistant"
    );

    let mut roster = Vec::new();
    let mut paths = Vec::new();
    collect_agent_definition_paths(
        &workspace_root.join("magician_data_v3/system/agent_templates/agents"),
        &mut paths,
    );
    paths.sort();
    for path in &paths {
        roster.push(
            AgentDefinition::from_yaml_file(path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display())),
        );
    }

    let disabled = magician::magician_v2::agents::disabled_agent_hierarchy(roster.iter());
    for surface in [InvocationSurface::Delegation, InvocationSurface::Handover] {
        let resolved = resolve_effective_delegation_target_ids_for_surface(
            &def,
            roster.iter(),
            &disabled,
            surface,
        );
        assert_eq!(
            resolved,
            Vec::<String>::new(),
            "an empty declaration must resolve to no targets on {}; if this \
             fails, empty now means unrestricted and every leaf agent in the \
             roster just gained the whole roster",
            surface.as_str()
        );
    }
}

/// The owner's router NAMES every agent it can hand work to.
///
/// `personal-assistant` carried `delegation_targets: ['*']` until 2026-08-21 —
/// the last wildcard on a non-system agent. The resolver expanded it to every
/// enabled non-system agent that permits wildcard delegation: 23 of them, which
/// is the count the 2026-07-11 harness log recorded being injected into a single
/// PA prompt. Two of those 23 mattered more than the rest. `executive-assistant`
/// holds the OWNER's Gmail, Calendar, Sheets, WhatsApp, Telegram and iMessage,
/// and was reached without ever being named — the same vacuity §5A.1 closed on
/// `creative-mind`. The company officers (`ceo` and onward to `ambassador`, the
/// company's outward voice) were reached the same way.
///
/// Three distinct failures are pinned here, and the third is the one a reviewer
/// is most likely to skip.
///
/// 1. The DECLARED list, by value. A list is only a boundary if changing it is
///    visible, and "somebody restored a plausible-looking roster" reads as
///    tidying in a diff.
/// 2. The RESOLVED list, by value, on both owner-transition surfaces. This is
///    what catches a target id that does not exist or no longer admits
///    delegation: a misspelled or renamed agent silently resolves to nothing
///    while still reading as a grant in the YAML, which is precisely how a
///    capability gets quietly deleted. `resolved == declared` proves every id
///    here is live.
/// 3. The company harness is absent from the resolved set. Naming the exclusion
///    rather than trusting the list above means a future addition of `cto` (say)
///    fails here with the reason, not just with a changed vector.
///
/// If a real owner ask fails because a specialist is missing, add that ONE id
/// with its evidence — never `'*'`, and never an officer.
#[test]
fn the_owners_router_names_its_delegates() {
    use magician::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface;

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let agents_root = workspace_root.join("magician_data_v3/system/agent_templates/agents");
    let def = AgentDefinition::from_yaml_file(
        &agents_root.join("personal-assistant/definition.agent.yaml"),
    )
    .expect("personal-assistant definition should parse and validate");

    // Declaration order is the evidence order the definition's own comment
    // explains: edges proven by a shipped artifact first, then those resting on
    // the definition's own text, then the four the OWNER confirmed on
    // 2026-08-21 — which the repo could not prove either way, and which are
    // therefore evidence no reading of the source could have supplied.
    let expected = vec![
        "mac-operator".to_string(),
        "android-operator".to_string(),
        "web-researcher".to_string(),
        "creative-mind".to_string(),
        "simple-data-analyst".to_string(),
        "executive-assistant".to_string(),
        "writing-assistant".to_string(),
        "internal-system-analyst".to_string(),
        "harness-sre".to_string(),
        "wealth-manager".to_string(),
    ];
    assert_eq!(
        def.delegation_targets, expected,
        "the owner's router must name its delegates. `['*']` here is the last \
         wildcard route to the owner's own accounts and to the company roster"
    );
    assert!(
        !def.delegation_targets.iter().any(|target| target == "*"),
        "a wildcard mixed into an otherwise explicit list still expands to the \
         whole roster"
    );

    let mut roster = Vec::new();
    let mut paths = Vec::new();
    collect_agent_definition_paths(&agents_root, &mut paths);
    paths.sort();
    for path in &paths {
        roster.push(
            AgentDefinition::from_yaml_file(path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display())),
        );
    }
    let disabled = magician::magician_v2::agents::disabled_agent_hierarchy(roster.iter());

    for surface in [InvocationSurface::Delegation, InvocationSurface::Handover] {
        let resolved = resolve_effective_delegation_target_ids_for_surface(
            &def,
            roster.iter(),
            &disabled,
            surface,
        );
        assert_eq!(
            resolved,
            expected,
            "every declared target must resolve on {}; a target that resolves \
             away is a capability deleted while the YAML still reads as a grant",
            surface.as_str()
        );

        // The company harness, named so a regression says WHY rather than only
        // WHAT. `ceo` owns the edge to `ambassador`, the company's outward
        // voice, so an officer in this set is an outward route the owner's
        // router should not hold.
        for company in [
            "ceo",
            "cmo",
            "cro",
            "cpo",
            "cto",
            "ambassador",
            "company-assistant",
            "vc-researcher",
        ] {
            assert!(
                !resolved.iter().any(|target| target == company),
                "`{company}` is the company harness; the owner's router reached \
                 it only through the wildcard this list replaced ({})",
                surface.as_str()
            );
        }
    }
}

/// The company assistant exists, is terminal, and cannot read the owner.
///
/// Each assertion is the counterpart of a way the split could be undone without
/// touching the officers at all: give this agent the owner's memory, let it
/// delegate onward, or hand it the messaging tools the owner's assistant holds.
#[test]
fn the_company_assistant_is_sealed_from_the_owner() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let path = workspace_root.join(
        "magician_data_v3/system/agent_templates/agents/company-assistant/definition.agent.yaml",
    );

    let def = AgentDefinition::from_yaml_file(&path)
        .expect("company-assistant definition should parse and validate");

    assert_eq!(
        def.user_memory_isolation,
        UserMemoryIsolation::FullyIsolated,
        "the company assistant must not read the owner's user memory"
    );
    assert!(
        def.delegation_targets.is_empty(),
        "the company assistant must be terminal — an assistant that can \
         re-delegate is a path back to the owner's agents by another route"
    );
    assert!(
        def.readable_agents.is_empty(),
        "the company assistant must not read another agent's memory"
    );

    // `kind: worker` receives no injected personal-agent memory defaults, so an
    // empty list here genuinely means none — unlike a personal agent, where the
    // same line triggers the six-tier default set.
    assert!(
        def.memory_tiers.is_empty(),
        "a worker's empty tier list should stay empty; if this fails, worker \
         agents now receive injected defaults and the comment above is wrong"
    );

    // `gmail`, `calendar` and `sheets` were GRANTED here until 2026-08-20, which
    // made the split half-done: the officers stopped delegating to the owner's
    // assistant and reached the owner's Google through this agent instead. They
    // are the first four entries for that reason, not the last.
    for forbidden in [
        "gmail",
        "calendar",
        "sheets",
        "zepto-mcp",
        "swiggy-mcp",
        "imessage_send",
        "whatsapp",
        "telegram",
        "search_memory",
    ] {
        assert!(
            !def.tools.iter().any(|tool| tool == forbidden),
            "the company assistant must not hold `{forbidden}`"
        );
        assert!(
            def.denied_tools.iter().any(|tool| tool == forbidden),
            "`{forbidden}` must be explicitly DENIED, not merely ungranted — an \
             ungranted tool becomes reachable the day a default widens"
        );
    }

    // The other half of the swap: the job still gets done, on the agent's own
    // accounts. A seal with nothing behind it would mean the officers simply
    // lost scheduling and correspondence, and the next person to notice would
    // re-grant the owner's Google to fix it.
    for required in [
        "presto-gmail",
        "presto-calendar",
        "presto-sheets",
        "agentmail-read",
        "agentmail-send",
        "kapso-whatsapp-read",
        "kapso-whatsapp-send",
    ] {
        assert!(
            def.tools.iter().any(|tool| tool == required),
            "the company assistant must hold `{required}` — it is the company \
             identity that replaces the owner's account for this job"
        );
    }

    // Every approval rule names an action the shipped skill actually exposes.
    // The rules this replaced named `calendar · create_event` and
    // `calendar · invite`, and NEITHER token exists on the shipped `calendar`
    // skill (it inserts events as `events_insert`), so the per-act gate this
    // agent is documented to carry matched nothing at all.
    let rules: Vec<(&str, Vec<String>)> = def
        .constraints
        .requires_approval
        .iter()
        .map(|rule| {
            let actions = match &rule.action {
                magician::magician_v2::agents::ActionPattern::Single(one) => vec![one.clone()],
                magician::magician_v2::agents::ActionPattern::Multiple(many) => many.clone(),
            };
            (rule.tool.as_str(), actions)
        })
        .collect();

    assert_eq!(
        rules,
        vec![
            (
                "presto-gmail",
                vec![
                    "send".to_string(),
                    "reply".to_string(),
                    "reply_all".to_string(),
                    "forward".to_string(),
                    "raw".to_string(),
                ]
            ),
            (
                "presto-calendar",
                vec![
                    "events_insert".to_string(),
                    "events_patch".to_string(),
                    "events_delete".to_string(),
                    "raw".to_string(),
                ]
            ),
            (
                "agentmail-send",
                vec![
                    "send".to_string(),
                    "reply".to_string(),
                    "forward".to_string(),
                ]
            ),
            ("kapso-whatsapp-send", vec!["send".to_string()]),
        ],
        "the company assistant's approval rules changed. Every action token here \
         must be an id in the tool's own SKILL.md `runtime_actions`, because \
         `approval::rule_matches` compares the token exactly — a plausible name \
         that the skill does not expose gates nothing while reading like a gate"
    );
}

fn ambassador_definition() -> AgentDefinition {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let path = workspace_root
        .join("magician_data_v3/system/agent_templates/agents/ambassador/definition.agent.yaml");
    AgentDefinition::from_yaml_file(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// OPC engagements plan §5A — the outward actor holds COMPANY identities only.
///
/// The failure this pins is a one-line edit: adding `gmail` or `calendar` to
/// this grant hands the agent that answers strangers a direct call into the
/// owner's mail and diary, and every other control in the tree — capture mode,
/// per-act approval, the suppression register — would still pass, because none
/// of them asks *whose account* an act used.
///
/// The exact list is asserted rather than a subset, because for this agent the
/// grant IS the boundary: `discoverability: surface_only` withholds the
/// universal tool rail entirely, so anything not written here is not offered at
/// all. A subset assertion would let an addition through unseen.
#[test]
fn the_ambassador_holds_company_identities_and_never_the_owners() {
    let def = ambassador_definition();

    assert_eq!(
        def.tools,
        vec![
            "agentmail-read".to_string(),
            "agentmail-send".to_string(),
            "kapso-whatsapp-read".to_string(),
            "kapso-whatsapp-send".to_string(),
            "presto-gmail".to_string(),
            "notify_owner".to_string(),
            "time_math".to_string(),
            "work-modules".to_string(),
        ],
        "the ambassador's catalogue changed. With the universal rail off, this \
         list is everything it can ever call, so an addition here is a new way \
         for the company's outward voice to act"
    );

    // Sealed rather than absent. An ungranted capability becomes reachable the
    // day a default widens or a pack joins the universal rail; a denied one
    // stays refused in both the offered catalogue and the dispatch ceiling.
    for forbidden in [
        // The owner's Google and the owner's personal messaging.
        "gmail",
        "calendar",
        "sheets",
        "whatsapp",
        "telegram",
        "telegram-self",
        "imessage",
        "imessage_send",
        // Commerce: the company's outward voice never spends.
        "zepto-mcp",
        "swiggy-mcp",
        "treasurer",
        // Memory and the filesystem — the latter holds the live OAuth tokens.
        "search_memory",
        "shell",
        "files",
        "read_file",
        "write_file",
        "edit_file",
        "http",
        // A company identity that is simply not in this actor's grant: the
        // ambassador speaks and listens, and booking is somebody else's job.
        // (`presto-calendar` IS in `OUTWARD_CAPABILITIES` as of 2026-08-20, so
        // an invitation through it would be captured — the denial here is about
        // scope of role, not a hole in the gate.)
        "presto-calendar",
        // Denied 2026-08-21, and this one IS about a hole in the gate. A form
        // is submitted by clicking its control, and `click` classifies as
        // nothing, so the outward block — disclosure, bindability refusal,
        // capture — never runs for a submit from this seat. A browse also
        // defaults to CDP, which is the owner's own signed-in Chrome, and the
        // §5A.2 refusal of CDP fires only for an engagement-BOUND execution
        // while this agent's cycle is created unbound.
        "browser",
    ] {
        assert!(
            !def.tools.iter().any(|tool| tool == forbidden),
            "the ambassador must not hold `{forbidden}`"
        );
        assert!(
            def.denied_tools.iter().any(|tool| tool == forbidden),
            "`{forbidden}` must be explicitly DENIED on the ambassador, not \
             merely ungranted"
        );
    }

    // `untrusted` reads stricter and is the wrong answer: the shipped policy
    // allows an untrusted level only `files.read` and `search.*`, so every send
    // in the grant above would be refused at dispatch and the agent would look
    // configured while doing nothing. `reviewed` is the level whose ceiling
    // admits this grant; the seal comes from surface_only + denied_tools.
    assert_eq!(
        def.trust_level.0, "reviewed",
        "the ambassador's trust level decides whether its grant can dispatch at \
         all — `untrusted` would silently refuse every send"
    );
}

/// The ambassador is enterable only through the two routes it declares, and can
/// hand work onward only to the one delegate it names.
///
/// Two distinct failures are pinned. First, adding `chat` or `realtime_voice`
/// would put the owner's own words into this agent's episodes, which consolidate
/// into its tiers, which render into the next outward draft — a leak with no
/// tool call in it. Second, a `'*'` in `delegation_targets`, or a
/// `delegation: wildcard` policy, would make every wildcard-delegating agent in
/// the roster able to route work through the one agent that can mail strangers.
#[test]
fn the_ambassador_is_reachable_only_by_the_routes_it_declares() {
    use magician::magician_v2::agents::{AgentDelegationPolicy, AgentDiscoverability};

    let def = ambassador_definition();

    assert_eq!(
        def.invocation_policy.discoverability,
        AgentDiscoverability::SurfaceOnly,
        "surface_only is what sets `universal_packs = &[]` for this agent — \
         relaxing it silently re-injects shell/files/http/search_memory"
    );
    assert_eq!(
        def.invocation_policy.delegation,
        AgentDelegationPolicy::Explicit,
        "wildcard delegation would let any agent carrying `'*'` reach the \
         company's outward voice without naming it. No shipped agent carries \
         one any more, but this is the property, not the census"
    );
    assert!(
        !def.invocation_policy.permits_wildcard_delegation(),
        "a wildcard delegator must not resolve to the ambassador"
    );

    let mut surfaces = def.invocation_policy.allowed_direct_surfaces.clone();
    surfaces.sort_by_key(|surface| surface.as_str());
    assert_eq!(
        surfaces,
        [InvocationSurface::Delegation, InvocationSurface::Task],
        "the ambassador serves its own scheduled cycle and delegated officer \
         work, and nothing else"
    );

    for closed in [
        InvocationSurface::Chat,
        InvocationSurface::RealtimeVoice,
        InvocationSurface::Meeting,
        InvocationSurface::PublicEnvoy,
        InvocationSurface::Plane,
    ] {
        assert!(
            !def.invocation_policy.permits_direct_surface(closed),
            "`{}` must not reach the ambassador",
            closed.as_str()
        );
    }

    // Stated as the property rather than as the list above, so a surface added
    // to the enum later cannot quietly qualify: this agent composes outward
    // messages, and it must never also be the live voice on an untrusted one.
    for surface in &def.invocation_policy.allowed_direct_surfaces {
        assert_eq!(
            surface.audience(),
            SurfaceAudience::Owner,
            "`{}` faces an untrusted audience; the ambassador drafts outward \
             messages under approval, it does not speak live to strangers",
            surface.as_str()
        );
    }

    assert_eq!(
        def.delegation_targets,
        vec!["web-researcher-opc".to_string()],
        "the ambassador's delegates are an explicit list. `vc-researcher` holds \
         read_file/write_file/edit_file and would reach the filesystem this \
         grant seals; `company-assistant` still holds the owner's gmail"
    );
    assert_eq!(
        def.constraints.coordination.max_delegation_depth, 1,
        "depth 1 is what stops an officer-delegated run (already at depth 1) \
         from handing work on again"
    );
    assert!(
        !def.constraints.coordination.allow_transitive_delegation,
        "transitive delegation would make the one research hop into a chain"
    );
}

/// The tiers are hand-written, and that is the point.
///
/// `memory_tiers: []` on a `kind: personal` agent does NOT mean "no memory":
/// `apply_defaults()` injects the standard six-tier personal set precisely
/// BECAUSE the list is empty. On this agent that default would be actively
/// wrong — `entities` accumulates the people it has spoken to across every
/// counterparty, and the default rule set promotes `tiers(insights)` into
/// `user.preferences`, a Normative tier where a laundered claim can suppress
/// alerts. So this pins the deliberate set and the absence of any `user.*`
/// promotion.
#[test]
fn the_ambassador_carries_deliberate_memory_and_promotes_nothing_to_the_owner() {
    let def = ambassador_definition();

    let mut tiers: Vec<&str> = def.memory_tiers.iter().map(|t| t.name.as_str()).collect();
    tiers.sort_unstable();
    assert_eq!(
        tiers,
        [
            "channel_knowledge",
            "representation_boundaries",
            "task_progress",
        ],
        "the ambassador's tiers changed. An empty list here would restore the \
         injected personal defaults, which carry cross-counterparty entity \
         memory and a promotion into user.preferences"
    );

    for injected_default in ["entities", "insights", "recent_activity", "archive"] {
        assert!(
            !def.memory_tiers
                .iter()
                .any(|tier| tier.name == injected_default),
            "`{injected_default}` is an injected personal default that carries \
             one counterparty's material into another's reply"
        );
    }

    for rule in &def.memory_consolidation {
        assert!(
            !rule.target.starts_with("user."),
            "consolidation rule `{}` promotes into `{}`; an outward agent must \
             write no user memory at all — that path is where conversation \
             content becomes a stated rule",
            rule.name,
            rule.target
        );
    }

    assert_eq!(
        def.user_memory_isolation,
        UserMemoryIsolation::FullyIsolated,
        "this is the field that actually redirects the ambassador's user-memory \
         reads away from the shared user root every other agent files into"
    );
    assert!(
        def.readable_agents.is_empty(),
        "the ambassador must not read another agent's memory"
    );
}

/// Every capability on this agent that can reach a third party is gated per
/// act, and there is no longer a class sitting outside that rule.
///
/// Derived from the same central tables the dispatch capture gate uses rather
/// than from a list written here, so a sending capability added to the grant
/// without a matching approval rule fails this test instead of shipping as an
/// ungated send. Capture mode is a posture an operator flips in one place; the
/// approval rule is the decision about one message, and turning sending on must
/// not also delete the second one.
///
/// # The exemption this test used to carry, and why it was wrong
///
/// It excused `browser` from needing a rule on the grounds that a
/// `FormSubmission` has no restricted form, "so a classified submitting
/// dispatch is refused outright for an autonomous agent". Every word of that is
/// true and it protected nothing, because *no browser dispatch is ever
/// classified*: `SUBMITTING_CAPABILITIES` keys on `submit` / `submit_form` and
/// the shipped skill declares neither, so a form submitted by clicking its
/// control reaches `OutwardDisposition::NotOutward` and skips the whole outward
/// block. The grant was removed rather than papered over with a rule that
/// cannot tell a submit from a page read; `browser` is now denied, so the
/// assertion below is that the ambassador's reach contains no unclassifiable
/// class at all.
#[test]
fn every_outward_capability_the_ambassador_holds_is_gated_per_act() {
    use magician::magician_v2::agents::outward_actions::{
        agent_outward_capabilities, OutwardClass,
    };

    let def = ambassador_definition();
    let outward = agent_outward_capabilities(&def.tools, &def.denied_tools);

    let reach: Vec<(&str, &str)> = outward
        .iter()
        .map(|(tool, class)| (tool.as_str(), class.as_str()))
        .collect();
    assert_eq!(
        reach,
        [
            ("agentmail-send", "mail"),
            ("kapso-whatsapp-send", "message"),
            ("presto-gmail", "mail"),
        ],
        "the ambassador's outward reach changed — this is the set an operator \
         must pause individually, and the set every gate below is derived from"
    );

    for (tool, class) in &outward {
        assert_ne!(
            *class,
            OutwardClass::FormSubmission,
            "`{tool}` reaches a site through a capability whose submitting \
             tokens match nothing it can dispatch, so neither the capture gate \
             nor a per-act rule can see the act; it must not be in this grant"
        );

        let rule = def
            .constraints
            .requires_approval
            .iter()
            .find(|rule| &rule.tool == tool)
            .unwrap_or_else(|| {
                panic!("`{tool}` can reach a named third party with no requires_approval rule")
            });
        assert_eq!(
            rule.ttl_secs,
            Some(86_400),
            "`{tool}`'s approval must expire; an approval with no TTL is a \
             standing permission nobody granted"
        );
    }
}

/// Every action the shipped browser skill declares, read out of the
/// `runtime_actions` block of its frontmatter.
///
/// An action name sits at exactly eight spaces of indent under
/// `runtime_actions.actions`, with no value on the line; its own fields are
/// indented deeper and are skipped. The scan stops at the frontmatter
/// terminator so the prose body — which spells command names inside code
/// fences — cannot contribute a token the runtime does not actually expose.
fn declared_browser_actions(skill_md: &str) -> Vec<String> {
    let after = skill_md
        .split_once("runtime_actions:")
        .expect("the browser skill declares a runtime_actions block")
        .1;
    let block = after
        .split_once("\n---")
        .expect("the browser skill frontmatter is terminated")
        .0;
    block
        .lines()
        .filter_map(|line| {
            let key = line.strip_prefix("        ")?;
            if key.starts_with(' ') {
                return None;
            }
            let name = key.strip_suffix(':')?;
            (!name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'))
            .then(|| name.to_string())
        })
        .collect()
}

/// THE GAP the ambassador's `browser` denial names, pinned against the shipped
/// vocabulary: the outward gate's submitting tokens match no action the browser
/// can dispatch, so a form submitted from an agent reaches nothing.
///
/// `BROWSER_SUBMITTING_ACTIONS` is `["submit", "submit_form"]`, and a form is
/// submitted by clicking its control — the skill declares `click` and declares
/// neither submitting token. `outward_action_class` therefore answers `None`
/// for every action the capability actually has, `classify_outward_dispatch`
/// answers `None` with it, and the entire outward block in
/// `execute_action_inner` — the disclosure record, §4A's bindability refusal,
/// the suppression check and capture mode — is skipped for a real submission.
/// The claim that "capture mode stands in front of it" was never true: capture
/// only ever sees a dispatch the classifier already claimed.
///
/// Asserted forward over the declared vocabulary rather than a list written
/// here, so the day the skill grows a submit action this test fails and the two
/// table entries stop being decorative.
#[test]
fn no_action_the_browser_skill_declares_is_seen_by_the_outward_gate() {
    use magician::magician_v2::agents::outward_actions::{outward_action_class, OutwardClass};

    let declared = declared_browser_actions(BROWSER_SKILL);

    // GUARD FIRST. A broken parse yields an empty list, and every assertion
    // below would then pass by finding nothing to be wrong about.
    for expected in [
        "click", "press", "fill", "find", "batch", "eval", "open", "snapshot",
    ] {
        assert!(
            declared.iter().any(|action| action == expected),
            "`{expected}` is missing from the parsed browser vocabulary, so this \
             scan is no longer reading the skill's declared actions"
        );
    }

    for token in ["submit", "submit_form"] {
        assert!(
            !declared.iter().any(|action| action == token),
            "the browser skill now declares `{token}`. The outward gate has been \
             waiting for exactly that: re-read whether a submit is classified, \
             and whether the ambassador's denial can be lifted"
        );
        assert_eq!(
            outward_action_class("browser", token),
            Some(OutwardClass::FormSubmission),
            "`{token}` is what the gate watches for, and it must stay classified \
             so a vocabulary that grows one does not arrive ungated"
        );
    }

    for action in &declared {
        assert_eq!(
            outward_action_class("browser", action),
            None,
            "`browser`/`{action}` is dispatchable and unclassified — no disclosure \
             record, no capture, no refusal"
        );
    }
}

/// The officers that have no way to reach the outside world can reach it
/// through the ambassador — and only those.
///
/// Asserted as an exact set over every shipped definition, because both
/// directions are failures. Losing an edge strands the officer that needs an
/// outward actor and it starts improvising with its own tools; gaining one
/// silently — a new officer copying a sibling's list — widens who can put words
/// in the company's mouth without anyone deciding to.
#[test]
fn only_the_officers_that_need_an_outward_actor_may_delegate_to_the_ambassador() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();

    let mut sources = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        if def
            .delegation_targets
            .iter()
            .any(|target| target == "ambassador")
        {
            sources.push(def.agent_id.clone());
        }
    }
    sources.sort();

    assert_eq!(
        sources,
        [
            // Composes the position and the reply, and holds no outward
            // capability of its own. It must never be IN an outward thread —
            // this edge is how it stays out of one.
            "ceo".to_string(),
            // Customer discovery, partners and pipeline follow-up. Holds
            // `browser` for its own checks and no way to mail or message.
            "cro".to_string(),
            // `cmo` is deliberately absent: it already holds `agentmail-send`,
            // so this edge would widen it onto WhatsApp and Presto's Gmail
            // rather than give it a route it lacks. `cpo` is absent because its
            // work is internal.
        ],
        "who can speak through the company's outward actor changed"
    );
}

/// Scenario B, the reachability half: an agent is reachable from a room only if
/// it deliberately says so. Asserted over every SHIPPED definition rather than a
/// synthetic one, because the risk is that a real agent acquires reachability —
/// by gaining an allowlist entry, or by a future change to what an empty
/// allowlist means — and no synthetic fixture would notice.
///
/// "the ambassador" in this name is the agent a ROOM binds, which is
/// `EnvoyConfig::envoy_agent_id` and ships as `envoy`. The OPC officer whose
/// agent_id is literally `ambassador` is deliberately NOT here: a room and guest
/// public chat read the same single config value, so making that agent
/// room-eligible today would also hand it every unknown guest message. Adding
/// `meeting` to its surfaces is a change to that config shape first, and to this
/// list second.
#[test]
fn no_shipped_agent_except_the_ambassador_is_reachable_from_a_room() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "expected shipped agent definitions");

    let mut reachable = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        if def
            .invocation_policy
            .permits_direct_surface(InvocationSurface::Meeting)
        {
            reachable.push(def.agent_id.clone());
        }
    }
    reachable.sort();

    assert_eq!(
        reachable,
        ["envoy"],
        "only the ambassador may be reachable from a room; found {reachable:?}"
    );
}

/// Scenario B for the plane: a terminal reaches an agent only if that agent
/// names `plane`. Empty `allowed_direct_surfaces` is the defaults, and Plane
/// is not one of them, so a silent definition stays unreachable. Presto is
/// the one seed agent opted in (Task 2 of the plane vertical slice).
#[test]
fn no_shipped_agent_except_presto_is_reachable_from_the_plane() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let root = workspace_root.join("magician_data_v3/system/agent_templates/agents");

    let mut paths = Vec::new();
    collect_agent_definition_paths(&root, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "expected shipped agent definitions");

    let mut reachable = Vec::new();
    for path in &paths {
        let def = AgentDefinition::from_yaml_file(path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        if def
            .invocation_policy
            .permits_direct_surface(InvocationSurface::Plane)
        {
            reachable.push(def.agent_id.clone());
        }
    }
    reachable.sort();

    assert_eq!(
        reachable,
        ["personal-assistant"],
        "only Presto may be reachable from the plane; found {reachable:?}"
    );
}

/// Opting Presto into the plane must not drop a default product surface: a
/// non-empty `allowed_direct_surfaces` is an exact allowlist, so the seed
/// restates every default plus `plane`.
#[test]
fn presto_keeps_every_default_surface_when_opting_into_the_plane() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let def = AgentDefinition::from_yaml_file(&workspace_root.join(
        "magician_data_v3/system/agent_templates/agents/personal-assistant/definition.agent.yaml",
    ))
    .expect("personal-assistant definition should parse and validate");

    for surface in InvocationSurface::ALL {
        let permitted = def.invocation_policy.permits_direct_surface(surface);
        if surface.is_default_direct_surface() || surface == InvocationSurface::Plane {
            assert!(
                permitted,
                "Presto lost `{surface:?}` while opting into the plane"
            );
        } else {
            assert!(
                !permitted,
                "Presto unexpectedly reached `{surface:?}` from the plane opt-in"
            );
        }
    }
}

/// The same set, stated the other way round: a room is not a default surface,
/// so an agent that says nothing about surfaces is unreachable from one. This
/// is what makes the assertion above hold for agents nobody thought about.
#[test]
fn a_room_is_never_a_default_surface_for_a_silent_definition() {
    assert!(
        !InvocationSurface::Meeting.is_default_direct_surface(),
        "a definition that lists no surfaces must not become room-reachable"
    );
    assert!(
        !InvocationSurface::PublicEnvoy.is_default_direct_surface(),
        "the same must hold for every untrusted surface, not just meetings"
    );
    assert!(
        !InvocationSurface::Plane.is_default_direct_surface(),
        "a definition that lists no surfaces must not become terminal-reachable"
    );
    for surface in InvocationSurface::ALL {
        if surface.audience() == SurfaceAudience::Untrusted {
            assert!(
                !surface.is_default_direct_surface(),
                "{} is untrusted and must not be a default direct surface",
                surface.as_str()
            );
        }
    }
}

#[test]
fn writing_assistant_definition_is_neutral_and_scoped() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate should live under workspace root");
    let writing_path = workspace_root.join(
        "magician_data_v3/system/agent_templates/agents/writing-assistant/definition.agent.yaml",
    );

    let def = AgentDefinition::from_yaml_file(&writing_path)
        .expect("writing-assistant definition should parse and validate");

    assert_eq!(def.agent_id, "writing-assistant");
    assert_eq!(def.default_personality.as_deref(), Some("professional"));
    assert_eq!(def.user_memory_isolation, UserMemoryIsolation::Shared);
    assert!(
        !def.is_primary,
        "writing assistant must not become the primary agent"
    );

    for expected in [
        "email-etiquette",
        "commit-message-style",
        "meeting-prep-brief-format",
        "presentation-design-principles",
    ] {
        assert!(
            def.tools.iter().any(|tool| tool == expected),
            "writing assistant should expose writing playbook `{expected}`"
        );
    }

    assert!(
        def.memory_tiers
            .iter()
            .any(|tier| tier.name == "style_preferences"),
        "writing assistant should keep style learning in its own agent memory"
    );
}

// ── Send caps: the number the agent is told and the number enforced ─────────
//
// These live in this file rather than a new test target on purpose: it already
// hosts the shipped-configuration inventories for outward safety (§9 steps 3
// and 7), and an extra integration binary links the whole crate again.

// Spliced at first use rather than `include_str!`: the router tables live in
// `llm-router.yaml`, so the config file alone has no profiles or operation
// mapping — and `#[serde(default)]` means that parses cleanly into an empty
// router instead of failing.
static ROOT_CONFIG: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(magician::config::shipped_repo_config_yaml);
const AGENTMAIL_SEND_SKILL: &str = include_str!("../../skillshub/agentmail-send/SKILL.md");
const KAPSO_SEND_SKILL: &str = include_str!("../../skillshub/kapso-whatsapp-send/SKILL.md");
const BROWSER_SKILL: &str = include_str!("../../skillshub/browser/SKILL.md");

/// Collapse every run of whitespace to a single space.
///
/// Both cap sentences are line-wrapped by the YAML frontmatter writer, and they
/// wrap in different places — one breaks after `Capped at 50`, the other in the
/// middle of `Capped at`. A scan over raw lines finds one and misses the other,
/// which would make half of the assertion below quietly vacuous.
fn flatten(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The daily ceiling `resource_authority` actually enforces for a tool-scoped
/// budget row, read out of the shipped config text.
fn configured_daily_ceiling(config: &str, tool_id: &str, commodity: &str) -> u32 {
    let needle = format!("id: {tool_id}");
    let after = config
        .split_once(needle.as_str())
        .unwrap_or_else(|| panic!("no `{needle}` budget row in the shipped config"))
        .1;
    let row = flatten(after);
    let expected_commodity = format!("commodity: {commodity}");
    assert!(
        row.starts_with(expected_commodity.as_str()),
        "the `{tool_id}` budget row no longer declares `{expected_commodity}`; the \
         parse below would then read some other row's ceiling"
    );
    let digits: String = row
        .split_once("ceiling: \"")
        .unwrap_or_else(|| panic!("`{tool_id}` budget row has no quoted ceiling"))
        .1
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("`{tool_id}` ceiling is not a whole number: {digits:?}"))
}

/// Every daily cap a tool's SKILL.md states to the agent, in the order found.
///
/// Both spellings are collected — the frontmatter `description` says
/// "Capped at N" and the body says "Cap: N sends/day" — because an author
/// updating one and not the other is exactly how the numbers drifted apart in
/// the first place.
fn declared_daily_caps(skill_md: &str) -> Vec<u32> {
    let flat = flatten(skill_md);
    let mut found = Vec::new();
    for marker in ["Capped at ", "Cap: "] {
        let mut rest = flat.as_str();
        while let Some((_, after)) = rest.split_once(marker) {
            let digits: String = after
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect();
            assert!(
                !digits.is_empty(),
                "`{marker}` in a send skill is no longer followed by a number, so \
                 this scan can no longer see what the agent is told"
            );
            found.push(digits.parse().expect("digits parse"));
            rest = after;
        }
    }
    found
}

/// Readiness review §9 step 4, second half — *"reconcile the skill docs with the
/// budget rows"*.
///
/// The enforced ceilings were 200 EMAIL_SENDS/day and 1000 WHATSAPP_SENDS/day
/// while both SKILL.md files told the agent it had 50, and the comment sitting
/// directly above the rows also said 50. A 4x and a 20x gap, running in the one
/// direction that costs sends rather than prompts: the agent reads its skill
/// doc, plans conservatively, and its own self-restraint is then the only thing
/// keeping a campaign inside a number anyone reviewed.
///
/// This pins the agreement rather than either number, because the failure to
/// catch is not "the cap is wrong" — it is "the cap was raised in one file".
#[test]
fn the_send_cap_the_agent_is_told_is_the_send_cap_enforced() {
    for (tool_id, commodity, skill_md) in [
        ("agentmail-send", "EMAIL_SENDS", AGENTMAIL_SEND_SKILL),
        ("kapso-whatsapp-send", "WHATSAPP_SENDS", KAPSO_SEND_SKILL),
    ] {
        let enforced = configured_daily_ceiling(&ROOT_CONFIG, tool_id, commodity);
        let declared = declared_daily_caps(skill_md);

        assert_eq!(
            declared,
            vec![50, 50],
            "`{tool_id}`'s SKILL.md must state 50/day in both its frontmatter \
             description and its body; found {declared:?}"
        );
        assert_eq!(
            enforced, 50,
            "`{tool_id}` enforces {enforced}/day. Conservative caps are a gate in \
             front of global unpause, and the enforced number must not exceed the \
             number the agent plans against"
        );
    }
}

/// A ceiling nobody enforces is a number, not a cap.
///
/// `resource_authority.enabled` gates every declared budget at once: with it
/// off, each pack's `spend:` block stays dormant and the rows above become
/// documentation. The test directly above would still pass — it compares two
/// pieces of text — so without this one, "the caps agree" could be true while
/// neither bound anything.
#[test]
fn the_send_caps_are_enforced_and_not_merely_declared() {
    for (label, config) in [("magician-config.yaml", ROOT_CONFIG.as_str())] {
        let authority = config
            .split_once("\nresource_authority:")
            .unwrap_or_else(|| panic!("{label} has no active resource_authority block"))
            .1;
        let head = flatten(authority);
        assert!(
            head.starts_with("enabled: true"),
            "{label}: resource_authority no longer opens with `enabled: true`, so \
             the send ceilings below it are declared and not enforced"
        );
    }
}
