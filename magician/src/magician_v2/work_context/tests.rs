//! §2.1's rule, as behaviour: a work context identifies capabilities; it never
//! attaches them.

use super::{
    delegation_needs, effective_capabilities, resolve, CapabilityStanding, WorkContext,
    WorkContextKind,
};

fn agent(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn fundraising() -> WorkContext {
    WorkContext::new(WorkContextKind::Program("fundraising".to_string()))
        .needing(&["agentmail-send", "presentation-maker"])
        .with_playbooks(&["form-filling-playbook"])
}

/// **The safety property.** Intersection can only narrow, so no combination
/// reaches `Usable` unless the agent already holds the capability.
///
/// Walked exhaustively rather than sampled: this is the one rule the whole
/// composition model rests on, and a single path that granted by naming would
/// turn a program into a way to hand any agent any tool.
#[test]
fn naming_a_capability_never_grants_it() {
    let work = fundraising();

    for capability in [
        "agentmail-send",        // needed, held below or not
        "presentation-maker",    // needed
        "form-filling-playbook", // needed, a playbook
        "websearch",             // not needed
    ] {
        let without = resolve(&agent(&[]), &work, capability);
        assert!(
            !without.permits_direct_use(),
            "an agent holding nothing must never reach usable for `{capability}`, however the \
             work describes itself"
        );
    }

    // Held but not needed is not usable here either — and is not a revocation.
    assert_eq!(
        resolve(&agent(&["websearch"]), &work, "websearch"),
        CapabilityStanding::OutsideThisWork
    );

    // Only held AND needed permits.
    assert_eq!(
        resolve(&agent(&["agentmail-send"]), &work, "agentmail-send"),
        CapabilityStanding::Usable
    );
    assert!(CapabilityStanding::Usable.permits_direct_use());
}

/// The four arms, exhaustively.
#[test]
fn the_four_standings_are_exactly_the_four_combinations() {
    let work = fundraising();
    let holder = agent(&["agentmail-send", "websearch"]);

    assert_eq!(
        resolve(&holder, &work, "agentmail-send"),
        CapabilityStanding::Usable,
        "held and needed"
    );
    assert_eq!(
        resolve(&holder, &work, "presentation-maker"),
        CapabilityStanding::RequiresDelegation,
        "needed, not held"
    );
    assert_eq!(
        resolve(&holder, &work, "websearch"),
        CapabilityStanding::OutsideThisWork,
        "held, not needed"
    );
    assert_eq!(
        resolve(&holder, &work, "duckdb"),
        CapabilityStanding::Unavailable,
        "neither"
    );
}

/// The discovery half. An agent that cannot see what the work needs would
/// improvise with what it has — which turns a narrow grant into a wrong answer
/// rather than a handoff.
#[test]
fn an_agent_can_see_what_it_must_delegate() {
    let work = fundraising();
    let holder = agent(&["agentmail-send"]);

    assert_eq!(
        delegation_needs(&holder, &work),
        vec![
            "form-filling-playbook".to_string(),
            "presentation-maker".to_string()
        ],
        "it can see the deck work and the playbook it cannot do"
    );
    assert!(resolve(&holder, &work, "presentation-maker").should_delegate());
    assert!(!resolve(&holder, &work, "agentmail-send").should_delegate());
}

/// Playbooks are resolved the same way as tools — §2.1 names both, and a
/// procedure skill an agent does not hold is as much a delegation pointer as a
/// tool is.
#[test]
fn playbooks_resolve_like_capabilities() {
    let work = fundraising();
    assert_eq!(
        resolve(
            &agent(&["form-filling-playbook"]),
            &work,
            "form-filling-playbook"
        ),
        CapabilityStanding::Usable
    );
    assert_eq!(
        resolve(&agent(&[]), &work, "form-filling-playbook"),
        CapabilityStanding::RequiresDelegation
    );
}

/// The effective set is the intersection and nothing else.
#[test]
fn the_effective_set_is_the_intersection() {
    let work = fundraising();
    let holder = agent(&[
        "agentmail-send",
        "form-filling-playbook",
        "websearch",
        "duckdb",
    ]);

    assert_eq!(
        effective_capabilities(&holder, &work),
        vec![
            "agentmail-send".to_string(),
            "form-filling-playbook".to_string()
        ],
        "held ∩ needed — `websearch` and `duckdb` are held but not part of this work, \
         `presentation-maker` is needed but not held"
    );

    // An agent holding nothing gets nothing, whatever the work names.
    assert!(effective_capabilities(&agent(&[]), &work).is_empty());
    // And a work that needs nothing narrows everything away.
    let empty_work = WorkContext::new(WorkContextKind::Program("idle".to_string()));
    assert!(effective_capabilities(&holder, &empty_work).is_empty());
}

/// A program is authored by a human in YAML and a grant elsewhere. Requiring
/// them to agree on capitalisation would make composition fail for a reason
/// nobody could see.
#[test]
fn capability_names_match_across_spelling() {
    let work = WorkContext::new(WorkContextKind::Program("p".to_string()))
        .needing(&["  AgentMail-Send  "]);
    assert_eq!(
        resolve(&agent(&["agentmail-send"]), &work, "AGENTMAIL-SEND"),
        CapabilityStanding::Usable
    );
    assert_eq!(
        effective_capabilities(&agent(&["AgentMail-Send"]), &work),
        vec!["agentmail-send".to_string()]
    );
}

/// Both kinds of work context behave identically — the generalisation is that
/// the middle term widened, not that programs got special treatment.
#[test]
fn a_program_and_an_engagement_narrow_the_same_way() {
    let holder = agent(&["agentmail-send"]);
    for kind in [
        WorkContextKind::Program("fundraising".to_string()),
        WorkContextKind::Engagement("eng-1".to_string()),
    ] {
        let work =
            WorkContext::new(kind.clone()).needing(&["agentmail-send", "presentation-maker"]);
        assert_eq!(
            resolve(&holder, &work, "agentmail-send"),
            CapabilityStanding::Usable,
            "{kind:?}"
        );
        assert_eq!(
            resolve(&holder, &work, "presentation-maker"),
            CapabilityStanding::RequiresDelegation,
            "{kind:?}"
        );
    }

    assert_eq!(
        WorkContextKind::Program("f".to_string()).as_key(),
        "program:f"
    );
    assert_eq!(
        WorkContextKind::Engagement("e".to_string()).as_key(),
        "engagement:e"
    );
}

/// Reads are stable and deduplicated, so two callers asking the same question
/// get the same answer in the same order.
#[test]
fn reads_are_deterministic_and_deduplicated() {
    let work = WorkContext::new(WorkContextKind::Program("p".to_string()))
        .needing(&["b-tool", "a-tool", "b-tool"])
        .with_playbooks(&["a-tool"]);
    let holder = agent(&["a-tool", "b-tool", "a-tool"]);

    assert_eq!(
        effective_capabilities(&holder, &work),
        vec!["a-tool".to_string(), "b-tool".to_string()],
        "sorted, and a name repeated in either list appears once"
    );
    assert!(delegation_needs(&holder, &work).is_empty());
}

/// Pins: `id()` returns the bare id for EVERY kind, prefix-free.
///
/// The failure it stops is a guard that is applied to the wrong string. A caller
/// refusing a blank work id, or one carrying the U+001F separator derived ids
/// are joined with, has to check the id itself — `as_key()` is prefixed, so a
/// blank-id check against it reads `"program:"` as non-blank and lets an
/// unusable work context through as if it named something.
#[test]
fn id_returns_the_bare_id_for_every_kind() {
    assert_eq!(
        WorkContextKind::Program("fundraising".to_string()).id(),
        "fundraising"
    );
    assert_eq!(
        WorkContextKind::Engagement("eng-1".to_string()).id(),
        "eng-1"
    );

    // The exact confusion this exists to prevent: the key is never blank even
    // when the id is, so a guard must not be applied to the key.
    let blank = WorkContextKind::Program(String::new());
    assert_eq!(blank.id(), "");
    assert_eq!(blank.as_key(), "program:");
    assert!(
        blank.id().is_empty() && !blank.as_key().is_empty(),
        "a blank id must be visible as blank through id(), and is not through as_key()"
    );
}

/// The summary is what §2.1 step 2 asks for, and its interesting half is the
/// list the agent does NOT hold.
///
/// An agent can already see its own catalogue. What the work needs and it lacks
/// is invisible everywhere else, and it is the only part that should change the
/// plan — so a summary that omitted it would be decoration.
#[test]
fn the_summary_names_what_the_work_needs_and_the_agent_lacks() {
    use super::capability_summary;

    let work = WorkContext::new(WorkContextKind::Program("fundraising".to_string()))
        .needing(&["agentmail-send", "presentation-maker"])
        .with_playbooks(&["form-filling-playbook"]);
    let holder = agent(&["agentmail-send", "browser"]);

    let summary = capability_summary(&holder, &work);
    assert!(summary.contains("program"), "{summary}");
    assert!(summary.contains("fundraising"), "{summary}");
    assert!(
        summary.contains("`agentmail-send`"),
        "held and needed: {summary}"
    );
    assert!(
        summary.contains("`presentation-maker`") && summary.contains("`form-filling-playbook`"),
        "the delegation half is the point: {summary}"
    );
    assert!(
        !summary.contains("`browser`"),
        "a capability the work does not name is not part of this work: {summary}"
    );
    assert!(summary.contains("Delegate"), "{summary}");
}

/// Nothing to say means nothing rendered — no header, no "(none)".
///
/// This runs on every turn of every execution, and most executions have no work
/// context at all. A section that costs prompt budget to say nothing is a
/// section that gets skimmed past when it does have something to say.
#[test]
fn a_work_that_names_nothing_renders_nothing() {
    use super::capability_summary;

    let empty = WorkContext::new(WorkContextKind::Engagement("acme".to_string()));
    assert_eq!(capability_summary(&agent(&["browser"]), &empty), "");
    // And an agent holding nothing still sees the delegation half, because
    // "you hold none of this" is the most useful thing the summary can say.
    let work = WorkContext::new(WorkContextKind::Engagement("acme".to_string()))
        .needing(&["agentmail-send"]);
    let summary = capability_summary(&agent(&[]), &work);
    assert!(summary.contains("`agentmail-send`"), "{summary}");
    assert!(summary.contains("Delegate"), "{summary}");
}
