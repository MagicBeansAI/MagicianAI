//! Planning behaviour, with no terminal and no stack.

use std::collections::BTreeMap;

use magician_components::{loader, Observed};

use super::*;

fn selection(present: &[&str]) -> Selection {
    let graph = loader::try_load(&loader::Paths {
        data_root: "/tmp/d".into(),
        home: "/tmp/h".into(),
    })
    .expect("graph");
    let observed = present
        .iter()
        .map(|id| (id.to_string(), Observed::Present("up".into())))
        .collect::<BTreeMap<_, _>>();
    Selection::with_remembered(graph, observed, |_| None)
}

fn want_only(sel: &mut Selection, ids: &[&str]) {
    let all: Vec<String> = sel.graph.features.iter().map(|f| f.id.clone()).collect();
    for id in all {
        let wanted = ids.contains(&id.as_str());
        if sel.is_wanted(&id) != wanted {
            sel.cursor = sel.graph.features.iter().position(|f| f.id == id).unwrap();
            sel.toggle_at_cursor();
        }
    }
}

#[test]
fn selection_starts_from_what_already_works() {
    // Re-running on a configured machine must not ask someone to re-choose what
    // they already have.
    let sel = selection(&["ollama-generation", "magician-runtime"]);
    assert!(sel.is_wanted("notes"), "notes works, so it starts selected");
    assert!(
        !sel.is_wanted("whatsapp"),
        "whatsapp does not work, so it does not"
    );
}

#[test]
fn a_plan_lists_dependencies_before_the_things_that_need_them() {
    // The extension needs magicutor. Installing them the other way round would
    // fail, so the order is part of the plan, not a detail of rendering.
    let mut sel = selection(&[]);
    want_only(&mut sel, &["read-open-tabs"]);
    let plan = sel.plan();
    let at = |id: &str| plan.install.iter().position(|c| c == id);
    let (mc, ext) = (at("magicutor"), at("chrome-extension"));
    assert!(
        ext.is_some(),
        "the extension must be planned: {:?}",
        plan.install
    );
    assert!(
        mc.is_some(),
        "its dependency must be planned too: {:?}",
        plan.install
    );
    assert!(mc < ext, "magicutor must come first: {:?}", plan.install);
}

#[test]
fn nothing_already_present_is_planned_again() {
    // Both halves of local Ollama, because a generation model with no daemon
    // under it is BlockedBy the embedder and does not satisfy anything.
    let mut sel = selection(&["ollama-embedding", "ollama-generation"]);
    want_only(&mut sel, &["chat"]);
    assert!(
        sel.plan().install.is_empty(),
        "a running model already satisfies chat: {:?}",
        sel.plan().install
    );
}

#[test]
fn one_provider_is_chosen_for_a_requirement_not_all_of_them() {
    // Installing both a local model and a cloud key to satisfy "a model" would
    // be absurd; the plan picks the first declared and the screen states it.
    let mut sel = selection(&[]);
    want_only(&mut sel, &["chat"]);
    let plan = sel.plan();
    let providers = ["remote-models", "ollama-generation"];
    let chosen: Vec<&&str> = providers
        .iter()
        .filter(|p| plan.install.contains(&p.to_string()))
        .collect();
    assert_eq!(chosen.len(), 1, "exactly one provider: {:?}", plan.install);
    // And a remote one. Chatting is what most people arrive for and it does not
    // need a local model; defaulting to Ollama would put several GB of download
    // in front of the first question the product is for.
    assert_eq!(
        *chosen[0], "remote-models",
        "chat defaults to a remote provider, not a local model: {:?}",
        plan.install
    );
}

#[test]
fn two_features_sharing_a_requirement_install_it_once() {
    let mut sel = selection(&[]);
    want_only(&mut sel, &["chat", "browser-research"]);
    let plan = sel.plan();
    let models = plan
        .install
        .iter()
        .filter(|c| *c == "ollama-generation" || *c == "remote-models")
        .count();
    assert_eq!(models, 1, "one model for both features: {:?}", plan.install);
}

#[test]
fn what_is_left_off_is_stated_with_a_reason() {
    // Declining has to be a stated cost. A plan that silently omits half the
    // product is the failure the installer gates had.
    let mut sel = selection(&[]);
    want_only(&mut sel, &["chat"]);
    let plan = sel.plan();
    assert!(!plan.left_off.is_empty(), "something must be left off");
    for (name, reason) in &plan.left_off {
        assert!(
            !reason.trim().is_empty(),
            "'{name}' is left off with no reason"
        );
    }
}

#[test]
fn the_projected_report_shows_the_outcome_not_the_present() {
    // The plan screen has to answer "what will I have", not "what do I have".
    let mut sel = selection(&[]);
    want_only(&mut sel, &["notes"]);
    assert!(!sel
        .report()
        .feature("notes")
        .unwrap()
        .availability
        .is_available());
    assert!(
        sel.projected_report()
            .feature("notes")
            .unwrap()
            .availability
            .is_available(),
        "after installing what the plan lists, notes must work"
    );
}

#[test]
fn the_cursor_wraps_in_both_directions() {
    let mut sel = selection(&[]);
    let last = sel.graph.features.len() - 1;
    sel.move_cursor(-1);
    assert_eq!(sel.cursor, last);
    sel.move_cursor(1);
    assert_eq!(sel.cursor, 0);
}

/// A machine the local generation model is not offered on. Described rather
/// than detected, because the refusal cannot be covered on a machine that
/// qualifies — which is every machine this suite runs on today.
fn small_machine() -> magician_components::Host {
    magician_components::Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 8,
    }
}

fn intel() -> magician_components::Host {
    magician_components::Host {
        os: "macos".into(),
        arch: "x86_64".into(),
        memory_gb: 64,
    }
}

#[test]
fn a_small_machine_is_planned_a_remote_model_rather_than_a_local_one() {
    // The interesting part is not that it refuses — it is that chat still
    // happens. A provider the machine cannot have is no provider, so the
    // requirement falls through to the next one instead of failing.
    let mut sel = selection(&[]);
    sel.host = small_machine();
    want_only(&mut sel, &["chat"]);
    let plan = sel.plan();

    assert!(
        plan.install.contains(&"remote-models".to_string()),
        "chat must still be possible: {:?}",
        plan.install
    );
    assert!(
        !plan.install.contains(&"ollama-generation".to_string()),
        "8 GB must never be planned a local model: {:?}",
        plan.install
    );
}

#[test]
fn what_the_machine_cannot_have_is_named_with_its_reason() {
    // Silently dropping it would leave someone wondering why on-device
    // processing never appeared.
    let mut sel = selection(&[]);
    sel.host = small_machine();
    want_only(&mut sel, &["private-processing"]);
    let plan = sel.plan();

    assert!(plan.install.iter().all(|id| id != "ollama-generation"));
    let (name, reason) = plan
        .unsupported
        .iter()
        .find(|(n, _)| n.contains("generation"))
        .expect("the refusal must be reported, not swallowed");
    assert!(
        reason.contains("16") && reason.contains('8'),
        "{name}: {reason}"
    );
}

#[test]
fn the_embedder_is_planned_on_a_machine_that_cannot_have_the_generation_model() {
    // The two halves are gated separately on purpose: memory is indexed
    // locally everywhere, and only generation needs the bigger machine.
    let mut sel = selection(&[]);
    sel.host = intel();
    want_only(&mut sel, &[]);
    let plan = sel.plan();

    assert!(
        plan.install.contains(&"ollama-embedding".to_string()),
        "the embedder is compulsory on every machine: {:?}",
        plan.install
    );
    assert!(sel.unsupported("ollama-embedding").is_none());
    assert!(sel
        .unsupported("ollama-generation")
        .unwrap()
        .contains("arm64"));
}
