//! The shipped graph must parse, refer only to things that exist, and describe
//! the system honestly. These run on every build, which is what lets `load`
//! panic rather than degrade.

use super::*;

fn paths() -> Paths {
    Paths {
        data_root: "/tmp/data".into(),
        home: "/tmp/home".into(),
    }
}

fn graph() -> Graph {
    try_load(&paths()).expect("graph.yaml must parse")
}

#[test]
fn the_shipped_graph_parses_and_has_no_dangling_references() {
    let g = graph();
    assert!(!g.components.is_empty());
    assert!(!g.features.is_empty());
    assert!(g.dangling().is_empty());
}

#[test]
fn placeholders_are_expanded_everywhere_they_appear() {
    let g = graph();
    for component in &g.components {
        let rendered = format!("{:?} {:?}", component.probe, component.install);
        assert!(
            !rendered.contains("{data_root}") && !rendered.contains("{home}"),
            "component '{}' still carries an unexpanded placeholder: {rendered}",
            component.id
        );
    }
    // And the expansion actually happened, rather than there being nothing to do.
    let key = g.component("remote-models").expect("remote-models");
    assert!(
        format!("{:?}", key.probe).contains("/tmp/data"),
        "{:?}",
        key.probe
    );
}

#[test]
fn interactive_components_resolve_setup_definitions() {
    let graph = graph();
    let whatsapp = graph
        .component("whatsapp-session")
        .and_then(|component| component.setup.as_ref())
        .expect("WhatsApp setup");
    assert!(matches!(
        &whatsapp.driver,
        crate::setup::SetupDriver::ManagedBot { .. }
    ));

    let workspace_file = graph
        .component("workspace-oauth")
        .and_then(|component| component.setup.as_ref())
        .expect("Workspace client-file setup");
    assert!(matches!(
        &workspace_file.driver,
        crate::setup::SetupDriver::ConfigurationFile { .. }
    ));

    let embedding = graph
        .component("ollama-embedding")
        .expect("required PPLX component");
    assert!(matches!(
        embedding.setup.as_ref().map(|setup| &setup.driver),
        Some(crate::setup::SetupDriver::ModelRuntime { .. })
    ));

    let extension = graph
        .component("chrome-extension")
        .expect("browser extension component");
    assert!(matches!(
        extension.setup.as_ref().map(|setup| &setup.driver),
        Some(crate::setup::SetupDriver::BrowserExtension { .. })
    ));

    let cua = graph.component("cua-driver").expect("CuaDriver component");
    assert!(matches!(
        cua.setup.as_ref().map(|setup| &setup.driver),
        Some(crate::setup::SetupDriver::CuaDriver { .. })
    ));

    let generation = graph
        .component("ollama-generation")
        .expect("local-generation component");
    assert!(matches!(
        generation.probe,
        crate::ProbeSpec::ConfiguredOllamaModelPresent { .. }
    ));
}

#[test]
fn every_requirement_has_at_least_one_provider() {
    // A requirement nothing provides can never be satisfied, so every feature
    // needing it is permanently unavailable — a modelling mistake, not a state.
    let g = graph();
    for requirement in &g.requirements {
        assert!(
            !g.providers(&requirement.id).is_empty(),
            "requirement '{}' has no provider",
            requirement.id
        );
    }
}

#[test]
fn every_optional_component_says_what_it_costs_and_how_to_get_it() {
    // The alternatives carry the interesting information; a choice offered with
    // no stated trade is a choice nobody can make.
    let g = graph();
    for component in g.components.iter().filter(|c| !c.core) {
        // WithRuntime means "arrives with the runtime", which is a claim only a
        // core component can honestly make.
        assert!(
            !matches!(component.install, crate::InstallAction::WithRuntime),
            "optional component '{}' claims it arrives with the runtime",
            component.id
        );
        if let crate::InstallAction::Manual { steps, .. } = &component.install {
            assert!(
                !steps.is_empty(),
                "manual component '{}' gives no instructions",
                component.id
            );
        }
    }
    for requirement in &g.requirements {
        let providers = g.providers(&requirement.id);
        if providers.len() < 2 {
            continue;
        }
        for provider in providers {
            assert!(
                !provider.cost.trim().is_empty(),
                "'{}' is one of several ways to satisfy '{}' but states no trade-off",
                provider.id,
                requirement.id
            );
        }
    }
}

#[test]
fn core_components_are_never_offered_as_a_choice() {
    let g = graph();
    for component in g.components.iter().filter(|c| c.core) {
        assert!(
            component.provides.is_empty(),
            "core component '{}' provides a requirement, which would make it look optional",
            component.id
        );
    }
}

#[test]
fn every_feature_reaches_at_least_one_surface() {
    let g = graph();
    for feature in &g.features {
        assert!(
            !feature.surfaces.is_empty(),
            "feature '{}' appears nowhere, so nothing can render it",
            feature.id
        );
        assert!(
            !feature.needs.is_empty(),
            "feature '{}' needs nothing",
            feature.id
        );
    }
}

#[test]
fn a_bare_stack_reports_the_features_that_still_work_and_why_the_rest_do_not() {
    // Nothing installed, nothing declared: the state of a first run. Every
    // feature should be unavailable with a reason naming what to install, and
    // no reason should be empty.
    use std::collections::BTreeMap;
    let g = graph();
    let report = crate::resolve(&g, &BTreeMap::new(), &BTreeMap::new());
    for feature in &report.features {
        match &feature.availability {
            crate::Availability::Unavailable { reason, missing } => {
                assert!(
                    !reason.trim().is_empty(),
                    "'{}' gives no reason",
                    feature.id
                );
                assert!(
                    !missing.is_empty(),
                    "'{}' names nothing missing",
                    feature.id
                );
            },
            crate::Availability::Available => {
                panic!("'{}' claims to work with nothing installed", feature.id)
            },
        }
    }
}

#[test]
fn declining_local_models_leaves_chat_working_through_the_cloud_key() {
    // The point of an OR: one provider going away must not take the feature with
    // it. This is the case the installer gates got wrong.
    use std::collections::BTreeMap;
    let g = graph();
    let observed: BTreeMap<String, crate::Observed> = [(
        "remote-models".to_string(),
        crate::Observed::Present("set".into()),
    )]
    .into_iter()
    .collect();
    let declared: BTreeMap<String, crate::Declared> =
        [("ollama-generation".to_string(), crate::Declared::Declined)]
            .into_iter()
            .collect();

    let report = crate::resolve(&g, &declared, &observed);
    assert!(
        report.feature("chat").unwrap().availability.is_available(),
        "chat should survive declining local models when a key is set"
    );
    // Memory does not: embeddings have only the local provider today, and the
    // report should say so rather than implying chat's key covers it.
    assert!(!report
        .feature("memory")
        .unwrap()
        .availability
        .is_available());
}

#[test]
fn a_bundled_browser_researches_the_web_but_cannot_see_your_open_tabs() {
    // Both are "browser work", and modelling them the same would promise
    // something the isolated browser cannot do: it has no signed-in session and
    // none of your tabs. Research takes either driver; tabs take the extension.
    use std::collections::BTreeMap;
    let g = graph();
    let observed: BTreeMap<String, crate::Observed> =
        [("bundled-browser", "present"), ("remote-models", "set")]
            .into_iter()
            .map(|(id, d)| (id.to_string(), crate::Observed::Present(d.into())))
            .collect();

    let report = crate::resolve(&g, &BTreeMap::new(), &observed);
    assert!(report
        .feature("browser-research")
        .unwrap()
        .availability
        .is_available());
    match &report.feature("read-open-tabs").unwrap().availability {
        crate::Availability::Unavailable { reason, .. } => {
            assert!(reason.contains("Chrome extension"), "{reason}")
        },
        other => panic!("a bundled browser must not claim your open tabs: {other:?}"),
    }
}

#[test]
fn a_cloud_key_runs_the_work_but_does_not_keep_it_on_this_machine() {
    // The distinction the graph could not express before. Classification and
    // distillation work with a remote model; what the on-device model buys is
    // that the content never leaves. Those are different questions and a single
    // "a model" requirement answers only the first.
    use std::collections::BTreeMap;
    let g = graph();
    let cloud: BTreeMap<String, crate::Observed> = [(
        "remote-models".to_string(),
        crate::Observed::Present("set".into()),
    )]
    .into_iter()
    .collect();
    let report = crate::resolve(&g, &BTreeMap::new(), &cloud);
    assert!(
        report.feature("chat").unwrap().availability.is_available(),
        "a cloud key is enough to work"
    );
    assert!(
        !report
            .feature("private-processing")
            .unwrap()
            .availability
            .is_available(),
        "a cloud key must never satisfy on-device processing"
    );

    // The local generation model satisfies both, since it also provides "a
    // model" — but only alongside the embedder, which is what installs Ollama.
    // Observing generation alone leaves it BlockedBy, which is the dependency
    // running the right way round: the embedder is the compulsory half.
    let generation_only: BTreeMap<String, crate::Observed> = [(
        "ollama-generation".to_string(),
        crate::Observed::Present("up".into()),
    )]
    .into_iter()
    .collect();
    let report = crate::resolve(&g, &BTreeMap::new(), &generation_only);
    assert_eq!(
        report.component("ollama-generation").unwrap().state,
        crate::ComponentState::BlockedBy {
            component: "ollama-embedding".to_string()
        },
        "a generation model without the daemon that serves it is not usable"
    );

    let local: BTreeMap<String, crate::Observed> =
        [("ollama-embedding", "up"), ("ollama-generation", "up")]
            .into_iter()
            .map(|(id, d)| (id.to_string(), crate::Observed::Present(d.into())))
            .collect();
    let report = crate::resolve(&g, &BTreeMap::new(), &local);
    assert!(report.feature("chat").unwrap().availability.is_available());
    assert!(report
        .feature("private-processing")
        .unwrap()
        .availability
        .is_available());
}

#[test]
fn the_embedding_model_is_compulsory_and_the_generation_model_is_not() {
    // The half of local Ollama that cannot be declined is the embedder: memory
    // is indexed on this machine in both processing modes. The generation model
    // is the choice, and it is the one with a machine requirement.
    let g = graph();
    let embedding = g.component("ollama-embedding").expect("ollama-embedding");
    let generation = g.component("ollama-generation").expect("ollama-generation");

    assert!(
        !embedding.declinable(),
        "declining the embedder is declining memory"
    );
    assert!(
        !embedding.core,
        "it is still a multi-gigabyte pull, not shipped with the binary"
    );
    assert!(
        generation.declinable(),
        "the generation model is a real choice"
    );

    assert!(
        embedding.host.is_none(),
        "the embedder runs wherever Magician does"
    );
    let gate = generation
        .host
        .as_ref()
        .expect("local generation is gated on the machine");
    let small = crate::Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 8,
    };
    assert!(
        gate.unmet_on(&small).is_some(),
        "8 GB must not be offered a local model"
    );
    let big = crate::Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 36,
    };
    assert!(gate.unmet_on(&big).is_none());
}

#[test]
fn the_phone_works_on_the_local_network_without_a_tunnel() {
    // Modelling mobile as requiring the tunnel would overstate what declining
    // the tunnel costs. WhatsApp genuinely does require it.
    use std::collections::BTreeMap;
    let g = graph();
    let observed: BTreeMap<String, crate::Observed> = [(
        "lan-origin".to_string(),
        crate::Observed::Present("set".into()),
    )]
    .into_iter()
    .collect();
    let report = crate::resolve(&g, &BTreeMap::new(), &observed);
    assert!(report
        .feature("mobile-apps")
        .unwrap()
        .availability
        .is_available());
    assert!(!report
        .feature("whatsapp")
        .unwrap()
        .availability
        .is_available());
}

#[test]
fn imessage_asks_for_both_approvals_not_just_the_reading_one() {
    // Reading goes straight at chat.db; replying drives Messages.app over
    // AppleScript, which macOS gates separately and per controlled app.
    // Declaring only the first left "read and reply" delivering half of itself
    // with nothing said about the other half.
    let g = graph();
    let f = g
        .features
        .iter()
        .find(|f| f.id == "imessage")
        .expect("imessage");
    let needs: Vec<&str> = f
        .needs
        .iter()
        .filter_map(|n| match n {
            crate::Need::Component(id) => Some(id.as_str()),
            crate::Need::Requirement(_) => None,
        })
        .collect();
    assert!(needs.contains(&"tcc-full-disk"), "reading: {needs:?}");
    assert!(
        needs.contains(&"tcc-automation-messages"),
        "replying: {needs:?}"
    );
}

#[test]
fn the_full_disk_probe_reads_rather_than_looking() {
    // chat.db exists whether or not the permission was ever granted.
    let g = graph();
    let c = g.component("tcc-full-disk").expect("tcc-full-disk");
    assert!(
        matches!(c.probe, crate::ProbeSpec::FileReadable { .. }),
        "existence is not the question: {:?}",
        c.probe
    );
}
