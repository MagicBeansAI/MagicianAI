//! The simulator, and the graph run through it.
//!
//! The sweep test is the one that earns its keep: it plays the whole flow
//! against every described machine and every single-capability selection, and
//! fails if any of them breaks an invariant. Adding a component to the graph
//! puts it through all of that without anybody writing a test for it.

use magician_components::loader;

use super::*;

fn graph() -> Graph {
    loader::try_load(&loader::Paths {
        data_root: "/tmp/d".into(),
        home: "/tmp/h".into(),
    })
    .expect("graph")
}

#[test]
fn no_described_machine_breaks_an_invariant() {
    let graph = graph();
    let (report, broken) = sweep(&graph);
    assert_eq!(broken, 0, "\n{report}");
}

#[test]
fn the_sweep_covers_every_capability_and_several_machines() {
    // A sweep that quietly shrank would still pass the test above while
    // checking almost nothing.
    let graph = graph();
    let scenarios: Vec<Scenario> = standard_scenarios(&graph)
        .into_iter()
        .chain(one_per_capability(&graph))
        .collect();
    assert!(
        scenarios.len() >= graph.features.len() + 8,
        "{} scenarios",
        scenarios.len()
    );
    for feature in &graph.features {
        assert!(
            scenarios
                .iter()
                .any(|s| s.wanted == vec![feature.id.clone()]),
            "{} is never chosen alone",
            feature.id
        );
    }
}

#[test]
fn a_small_intel_mac_is_never_planned_a_local_model() {
    // The refusal that matters most: several gigabytes of download onto a
    // machine that cannot run the result.
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let scenario = Scenario::new("small intel")
        .on("macos", "x86_64", 8)
        .wanting(&every);
    let t = run(&graph, &scenario);
    assert!(
        !t.install.contains(&"ollama-generation".to_string()),
        "{:?}",
        t.install
    );
    assert!(
        t.unsupported
            .iter()
            .any(|(name, _)| name.contains("model") || name.to_lowercase().contains("ollama")),
        "the refusal is reported: {:?}",
        t.unsupported
    );
}

#[test]
fn a_capable_mac_is_planned_the_local_model() {
    // The other half of the same rule. Without this, a plan that installs
    // nothing anywhere would pass the refusal test.
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("big mac")
            .on("macos", "arm64", 36)
            .wanting(&every),
    );
    assert!(
        t.install.contains(&"ollama-generation".to_string()),
        "{:?}",
        t.install
    );
}

#[test]
fn declining_every_prompt_changes_nothing() {
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("nervous")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .declining(),
    );
    assert!(t.acted.is_empty(), "{:?}", t.acted);
    assert!(!t.steps.is_empty(), "it still walked the plan");
    assert!(
        t.steps.iter().all(|(_, o)| matches!(
            o,
            StepOutcome::Declined | StepOutcome::Blocked { .. } | StepOutcome::WithRuntime
        )),
        "{:?}",
        t.steps
    );
}

#[test]
fn a_machine_that_already_has_everything_plans_nothing() {
    let graph = graph();
    let all: Vec<&str> = graph.components.iter().map(|c| c.id.as_str()).collect();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("fully installed")
            .on("macos", "arm64", 36)
            .with(&all)
            .wanting(&every),
    );
    assert!(t.install.is_empty(), "{:?}", t.install);
    assert!(t.asked.is_empty(), "and asks for no keys: {:?}", t.asked);
}

#[test]
fn a_package_install_refuses_the_steps_that_need_a_makefile() {
    // No source, no make. The step has to be refused with a reason rather than
    // attempted and reported as a mysterious failure.
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("package")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .from_a_package(),
    );
    let make_only: Vec<&str> = graph
        .components
        .iter()
        .filter(|c| matches!(&c.install, InstallAction::Make { script: None, .. }))
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        !make_only.is_empty(),
        "the graph has make-only components to check"
    );
    for id in make_only {
        if let Some(outcome) = t.outcome(id) {
            assert!(
                matches!(
                    outcome,
                    StepOutcome::Unsupported { .. } | StepOutcome::Blocked { .. }
                ),
                "{id} was attempted without a Makefile: {outcome:?}"
            );
        }
    }
}

#[test]
fn nobody_is_asked_for_a_key_they_did_not_ask_for() {
    // Choosing notes must not produce a prompt for an image-generation key.
    let graph = graph();
    for scenario in one_per_capability(&graph) {
        let t = run(&graph, &scenario);
        for variable in &t.asked {
            let owner = graph
                .components
                .iter()
                .find(|c| match &c.install {
                    InstallAction::Manual { secrets, .. } => {
                        secrets.iter().any(|s| &s.variable == variable)
                    },
                    _ => false,
                })
                .expect("some component declares it");
            assert!(
                t.install.contains(&owner.id),
                "{}: asked for {variable} for {}, which is not in the plan",
                scenario.name,
                owner.id
            );
        }
    }
}

#[test]
fn pasting_a_key_finishes_the_step_and_stops_the_asking() {
    // Any-of: one accepted value ends the prompting for that component. Asking
    // for the other five providers afterwards is how a wizard loses somebody.
    let graph = graph();
    let with_secrets: Vec<(&str, Vec<&str>)> = graph
        .components
        .iter()
        .filter_map(|c| match &c.install {
            InstallAction::Manual { secrets, .. } if secrets.len() > 1 => Some((
                c.id.as_str(),
                secrets.iter().map(|s| s.variable.as_str()).collect(),
            )),
            _ => None,
        })
        .collect();
    assert!(
        !with_secrets.is_empty(),
        "the graph has any-of secrets to check"
    );
    let (component, variables) = &with_secrets[0];
    let feature = graph
        .features
        .iter()
        .find(|f| {
            f.needs
                .iter()
                .any(|n| matches!(n, Need::Component(id) if id == component))
        })
        .map(|f| f.id.clone());
    let Some(feature) = feature else { return };
    let t = run(
        &graph,
        &Scenario::new("holds one key")
            .on("macos", "arm64", 36)
            .wanting(&[feature.as_str()])
            .pasting(&[variables[0]]),
    );
    assert_eq!(
        t.outcome(component),
        Some(&StepOutcome::Installed),
        "{:?}",
        t.steps
    );
    assert_eq!(
        t.asked
            .iter()
            .filter(|v| variables.contains(&v.as_str()))
            .count(),
        1
    );
}

#[test]
fn an_unanswerable_probe_asks_the_person_rather_than_failing() {
    // Screen and microphone permissions cannot be read by a process that lacks
    // them. Reporting that as a failure would tell somebody their working
    // machine is broken.
    let graph = graph();
    let manual: Vec<&str> = graph
        .components
        .iter()
        .filter(|c| matches!(c.probe, magician_components::ProbeSpec::Manual { .. }))
        .map(|c| c.id.as_str())
        .collect();
    if manual.is_empty() {
        return;
    }
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("permissions unreadable")
            .on("macos", "arm64", 36)
            .cannot_tell(&manual)
            .wanting(&every),
    );
    for id in manual {
        if let Some(outcome) = t.outcome(id) {
            assert!(
                !matches!(outcome, StepOutcome::NotVerified { .. }),
                "{id} reported as failed when the probe simply could not answer"
            );
        }
    }
}

#[test]
fn a_violation_is_actually_detected() {
    // The invariants are only worth having if they can fail. A scenario that
    // claims a component is present while the plan installs it is exactly the
    // contradiction rule two exists for, so build one by hand.
    let graph = graph();
    let scenario = Scenario::new("contradiction").on("macos", "arm64", 36);
    let mut transcript = run(&graph, &scenario);
    let planted = graph.components[0].id.clone();
    transcript.install.push(planted.clone());
    let mut scenario = scenario;
    scenario.present.push(planted);
    let found = violations(&graph, &scenario, &transcript);
    assert!(
        !found.is_empty(),
        "the checker cannot fail, so it proves nothing"
    );
}

#[test]
fn the_sweep_reads_as_a_report() {
    // Printed, because a wall of assertions cannot tell you the output is
    // unreadable. This is the surface another agent reads.
    let graph = graph();
    let t = run(
        &graph,
        &Scenario::new("a bare Apple Silicon Mac, three capabilities")
            .on("macos", "arm64", 36)
            .wanting(
                &graph
                    .features
                    .iter()
                    .take(3)
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>(),
            ),
    );
    println!("{}", t.render());
    assert!(t.render().contains("plan:"));
}

#[test]
fn nothing_downstream_of_the_runtime_is_attempted_before_it_is_up() {
    // A browser extension cannot pair with an executor that is not running.
    // The plan still lists it — the wizard is not going to hide a capability
    // somebody asked for — but the step reports what is actually in the way
    // rather than trying and failing on its own terms.
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("runtime down")
            .on("macos", "arm64", 36)
            .wanting(&every)
            .before_the_runtime_is_up(),
    );
    let runtime_ids: Vec<&str> = graph
        .components
        .iter()
        .filter(|c| matches!(c.install, InstallAction::WithRuntime))
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        !runtime_ids.is_empty(),
        "the graph has components that arrive with the runtime"
    );
    // Only the ones something asked for reach a step at all; the runtime binary
    // itself is nobody's dependency.
    let stepped: Vec<&&str> = runtime_ids
        .iter()
        .filter(|id| t.outcome(id).is_some())
        .collect();
    assert!(!stepped.is_empty(), "at least one of them is in this plan");
    for id in stepped {
        assert_eq!(t.outcome(id), Some(&StepOutcome::WithRuntime), "{id}");
    }
    let dependants: Vec<&str> = graph
        .components
        .iter()
        .filter(|c| c.requires.iter().any(|r| runtime_ids.contains(&r.as_str())))
        .map(|c| c.id.as_str())
        .collect();
    assert!(!dependants.is_empty(), "and components that depend on them");
    for id in dependants {
        assert!(
            matches!(t.outcome(id), Some(StepOutcome::Blocked { .. })),
            "{id} was attempted anyway: {:?}",
            t.outcome(id)
        );
    }
}

#[test]
fn the_same_machine_with_the_runtime_up_gets_on_with_it() {
    // The other half of the rule, or the test above would pass on a plan that
    // blocked everything forever.
    let graph = graph();
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("runtime up")
            .on("macos", "arm64", 36)
            .wanting(&every),
    );
    assert!(
        !t.steps
            .iter()
            .any(|(_, o)| matches!(o, StepOutcome::Blocked { .. })),
        "{:?}",
        t.steps
    );
}

#[test]
fn a_half_installed_machine_is_not_offered_what_it_has() {
    // The scenario has to actually be half installed, or it is a bare machine
    // under another name — which is what it was until the simulator said the
    // plans were identical.
    let graph = graph();
    let half = every_other(&graph);
    assert!(
        half.len() > 3,
        "half of {} components",
        graph.components.len()
    );
    let every: Vec<&str> = graph.features.iter().map(|f| f.id.as_str()).collect();
    let t = run(
        &graph,
        &Scenario::new("half")
            .on("macos", "arm64", 36)
            .with(&half)
            .wanting(&every),
    );
    for id in &half {
        assert!(
            !t.install.contains(&id.to_string()),
            "{id} was offered again"
        );
    }
    assert!(!t.install.is_empty(), "and the other half is still offered");
}
