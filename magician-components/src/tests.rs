//! The resolver's interesting behaviour, with no stack running.
//!
//! Each test pins a decision that produced a real defect or would have: the
//! difference between declining a component and losing one, OR satisfaction,
//! observation beating an empty declaration on an existing install, and the fact
//! that a component with a broken dependency is not usable however healthy its
//! own probe looked.

use std::collections::BTreeMap;

use crate::*;

#[test]
fn cua_setup_is_available_on_each_desktop_platform_without_mac_permissions() {
    let graph = loader::load(&loader::Paths {
        data_root: "/fixture/data".into(),
        home: "/fixture/home".into(),
    });
    let cua = graph.component("cua-driver").unwrap();
    // The component carries the one pinned release Desktop installs.
    let pin = setup::cua_driver_release().unwrap();
    assert!(matches!(
        cua.setup.as_ref().map(|setup| &setup.driver),
        Some(setup::SetupDriver::CuaDriver { version, .. }) if *version == pin.version
    ));
    for os in ["windows", "linux", "macos"] {
        assert!(cua
            .unsupported_on(&Host {
                os: os.into(),
                arch: "x86_64".into(),
                memory_gb: 4
            })
            .is_none());
    }
    assert!(cua.requires.is_empty());
    let observed = BTreeMap::from([(
        "cua-driver".into(),
        Observed::Present("verified fixture".into()),
    )]);
    let report = resolve(&graph, &BTreeMap::new(), &observed);
    assert!(report
        .feature("computer-use")
        .unwrap()
        .availability
        .is_available());
    assert!(!report
        .feature("mac-skills")
        .unwrap()
        .availability
        .is_available());
    assert!(!report
        .feature("imessage")
        .unwrap()
        .availability
        .is_available());
}

fn component(id: &str, provides: &[&str], requires: &[&str], core: bool) -> Component {
    Component {
        id: id.to_string(),
        name: id.to_string(),
        core,
        provides: provides.iter().map(|s| s.to_string()).collect(),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        cost: String::new(),
        pricing: Pricing::Free,
        install: InstallAction::WithRuntime,
        probe: ProbeSpec::Manual {
            hint: "test fixture".into(),
        },
        setup: None,
        required: false,
        host: None,
    }
}

/// A component that cannot be declined but still has to be installed, and one
/// that only some machines can have. Both are states `core` could not express.
fn required_component(id: &str, provides: &[&str]) -> Component {
    Component {
        required: true,
        ..component(id, provides, &[], false)
    }
}

fn gated_component(id: &str, host: HostRequirement) -> Component {
    Component {
        host: Some(host),
        ..component(id, &[], &[], false)
    }
}

#[test]
fn required_is_not_core_and_neither_can_be_declined() {
    // `core` conflated "not a choice" with "arrives with the runtime". The
    // local embedding model is the first that is the former and not the latter.
    let plain = component("silverbullet", &[], &[], false);
    let required = required_component("ollama-embedding", &["embeddings"]);
    let core = component("magicutor", &[], &[], true);

    assert!(plain.declinable(), "an ordinary component is a choice");
    assert!(
        !required.declinable(),
        "a required component is not a choice"
    );
    assert!(!core.declinable(), "core was never a choice");
    assert!(
        !required.core,
        "required must not imply core, or it stops being installed"
    );
}

#[test]
fn a_host_requirement_says_what_is_wrong_and_why() {
    // "Unsupported" on its own tells nobody what to do next, so the reason the
    // limit exists travels with the limit.
    let gated = gated_component(
        "ollama-generation",
        HostRequirement {
            min_memory_gb: Some(16),
            arch: vec!["arm64".to_string()],
            os: vec!["macos".to_string()],
            because: "It swaps below that.".to_string(),
        },
    );
    let big = Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 36,
    };
    assert!(
        gated.unsupported_on(&big).is_none(),
        "a machine that meets it is not blocked"
    );

    let small = Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 8,
    };
    let reason = gated.unsupported_on(&small).expect("8 GB is under the bar");
    assert!(reason.contains("16 GB") && reason.contains("8"), "{reason}");
    assert!(
        reason.contains("It swaps below that."),
        "the why travels with it: {reason}"
    );

    let intel = Host {
        os: "macos".into(),
        arch: "x86_64".into(),
        memory_gb: 64,
    };
    assert!(gated.unsupported_on(&intel).unwrap().contains("arm64"));

    let linux = Host {
        os: "linux".into(),
        arch: "arm64".into(),
        memory_gb: 64,
    };
    assert!(gated.unsupported_on(&linux).unwrap().contains("macos"));

    // Every fault is reported, not just the first, so one install attempt tells
    // the whole story rather than one thing at a time.
    let tiny_intel = Host {
        os: "linux".into(),
        arch: "x86_64".into(),
        memory_gb: 4,
    };
    let all = gated.unsupported_on(&tiny_intel).unwrap();
    for expected in ["16 GB", "arm64", "macos"] {
        assert!(all.contains(expected), "{expected} missing from {all}");
    }
}

#[test]
fn a_component_with_no_host_block_runs_anywhere() {
    let anywhere = component("silverbullet", &[], &[], false);
    let potato = Host {
        os: "linux".into(),
        arch: "riscv64".into(),
        memory_gb: 1,
    };
    assert!(anywhere.unsupported_on(&potato).is_none());
}

fn graph() -> Graph {
    Graph {
        components: vec![
            component("ollama", &["model"], &[], false),
            component("api-key", &["model"], &[], false),
            component("magicutor", &[], &[], true),
            component("chrome-extension", &["browser"], &["magicutor"], false),
            component("headless-browser", &["browser"], &[], false),
            component("silverbullet", &[], &[], false),
        ],
        requirements: vec![
            Requirement {
                id: "model".into(),
                name: "a model".into(),
                question: "How should the agent run models?".into(),
            },
            Requirement {
                id: "browser".into(),
                name: "a browser driver".into(),
                question: "How should the agent drive a browser?".into(),
            },
        ],
        features: vec![
            Feature {
                id: "read-tabs".into(),
                name: "Read my browser tabs".into(),
                description: String::new(),
                needs: vec![
                    Need::Requirement("browser".into()),
                    Need::Requirement("model".into()),
                ],
                surfaces: vec![Surface::Web, Surface::Desktop],
            },
            Feature {
                id: "notes".into(),
                name: "Notes".into(),
                description: String::new(),
                needs: vec![Need::Component("silverbullet".into())],
                surfaces: vec![Surface::Web, Surface::Ios],
            },
        ],
    }
}

fn present(ids: &[&str]) -> BTreeMap<String, Observed> {
    ids.iter()
        .map(|id| (id.to_string(), Observed::Present("up".into())))
        .collect()
}

#[test]
fn the_authored_graph_references_nothing_that_does_not_exist() {
    assert!(graph().dangling().is_empty());
}

#[test]
fn a_dangling_reference_is_reported_rather_than_surfacing_as_unavailable() {
    let mut g = graph();
    g.features[0].needs.push(Need::Component("nope".into()));
    let bad = g.dangling();
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("nope"), "{bad:?}");
}

#[test]
fn either_provider_satisfies_a_requirement() {
    let g = graph();
    let declared = BTreeMap::new();

    let with_local = resolve(
        &g,
        &declared,
        &present(&["ollama", "magicutor", "chrome-extension"]),
    );
    assert!(with_local
        .feature("read-tabs")
        .unwrap()
        .availability
        .is_available());

    // Swapping the local model for the cloud one changes nothing about the
    // feature: that is what makes it a choice rather than a dependency.
    let with_cloud = resolve(
        &g,
        &declared,
        &present(&["api-key", "magicutor", "chrome-extension"]),
    );
    assert!(with_cloud
        .feature("read-tabs")
        .unwrap()
        .availability
        .is_available());
}

#[test]
fn a_missing_requirement_names_every_way_to_satisfy_it() {
    let g = graph();
    let report = resolve(
        &g,
        &BTreeMap::new(),
        &present(&["magicutor", "chrome-extension"]),
    );
    match &report.feature("read-tabs").unwrap().availability {
        Availability::Unavailable { reason, missing } => {
            assert_eq!(missing, &vec!["model".to_string()]);
            assert!(reason.starts_with("needs "), "{reason}");
            assert!(reason.contains("ollama"), "{reason}");
            assert!(reason.contains("api-key"), "{reason}");
        },
        other => panic!("expected unavailable, got {other:?}"),
    }
}

#[test]
fn declining_a_component_is_not_the_same_as_losing_one() {
    let g = graph();
    let observed = present(&["magicutor"]);

    let declined: BTreeMap<String, Declared> = [("silverbullet".to_string(), Declared::Declined)]
        .into_iter()
        .collect();
    assert_eq!(
        resolve(&g, &declined, &observed)
            .component("silverbullet")
            .unwrap()
            .state,
        ComponentState::Declined,
        "a component the operator turned down must not read as a fault"
    );

    let wanted: BTreeMap<String, Declared> = [("silverbullet".to_string(), Declared::Wanted)]
        .into_iter()
        .collect();
    assert!(
        matches!(
            resolve(&g, &wanted, &observed)
                .component("silverbullet")
                .unwrap()
                .state,
            ComponentState::Missing { .. }
        ),
        "a component the operator asked for and did not get is a fault"
    );

    // Either way the feature is unavailable — intent changes the explanation,
    // never whether the thing works.
    for declared in [declined, wanted] {
        assert!(!resolve(&g, &declared, &observed)
            .feature("notes")
            .unwrap()
            .availability
            .is_available());
    }
}

#[test]
fn observation_beats_an_empty_declaration() {
    // Every install predating this registry has no declarations at all. Those
    // must report what is actually running, not a wall of "not installed".
    let g = graph();
    let report = resolve(
        &g,
        &BTreeMap::new(),
        &present(&["silverbullet", "magicutor"]),
    );
    assert!(report.component("silverbullet").unwrap().state.usable());
    assert!(report.feature("notes").unwrap().availability.is_available());
}

#[test]
fn a_component_whose_dependency_is_down_is_not_usable() {
    // chrome-extension probes healthy, but magicutor — which it needs — is not
    // running. Trusting its own probe would offer a browser driver that fails on
    // first use.
    let g = graph();
    let report = resolve(
        &g,
        &BTreeMap::new(),
        &present(&["chrome-extension", "ollama"]),
    );
    assert_eq!(
        report.component("chrome-extension").unwrap().state,
        ComponentState::BlockedBy {
            component: "magicutor".to_string()
        }
    );
    assert!(!report
        .feature("read-tabs")
        .unwrap()
        .availability
        .is_available());
}

#[test]
fn a_core_component_is_never_reported_as_declined() {
    let g = graph();
    let declared: BTreeMap<String, Declared> = [("magicutor".to_string(), Declared::Declined)]
        .into_iter()
        .collect();
    // Declining a core component is not expressible; absence stays a fault.
    assert!(matches!(
        resolve(&g, &declared, &BTreeMap::new())
            .component("magicutor")
            .unwrap()
            .state,
        ComponentState::Missing { .. }
    ));
}

#[test]
fn an_unprobeable_component_is_unknown_rather_than_absent() {
    let g = graph();
    let observed: BTreeMap<String, Observed> = [(
        "silverbullet".to_string(),
        Observed::Unknown("cannot probe from here".into()),
    )]
    .into_iter()
    .collect();
    let report = resolve(&g, &BTreeMap::new(), &observed);
    assert!(matches!(
        report.component("silverbullet").unwrap().state,
        ComponentState::Unknown { .. }
    ));
    assert!(!report.component("silverbullet").unwrap().state.usable());
}

#[test]
fn each_surface_gets_only_what_it_can_deliver() {
    let g = graph();
    let report = resolve(
        &g,
        &BTreeMap::new(),
        &present(&["silverbullet", "magicutor"]),
    );

    // Notes is available and shows on web and iOS; read-tabs is blocked.
    let web: Vec<&str> = report
        .available_on(Surface::Web)
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(web, vec!["notes"]);

    // Android declares neither feature, so it renders nothing rather than
    // inheriting another surface's list.
    assert!(report.available_on(Surface::Android).is_empty());

    // The blocked list is what turns a hidden entry into an offer.
    let blocked: Vec<&str> = report
        .blocked_on(Surface::Web)
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(blocked, vec!["read-tabs"]);
}

#[test]
fn what_costs_money_says_so_and_nothing_else_does() {
    // Most of the graph is the operator's own machine, so labelling every free
    // component would be noise that buries the few that are not free.
    let free = component("silverbullet", &[], &[], false);
    assert!(!free.pricing.costs_money(), "free is the default");

    let metered = Component {
        pricing: Pricing::FreeTier,
        ..component("search", &[], &[], false)
    };
    let billed = Component {
        pricing: Pricing::Paid,
        ..component("media", &[], &[], false)
    };
    assert!(
        metered.pricing.costs_money(),
        "a free tier still ends in a bill"
    );
    assert!(billed.pricing.costs_money());
    assert_eq!(metered.pricing.label(), "free tier");
    assert_eq!(billed.pricing.label(), "paid");
}

#[test]
fn the_report_carries_pricing_to_whatever_renders_it() {
    // Asked of the status list, not of the graph, so it has to survive resolve.
    let mut g = graph();
    g.components.push(Component {
        pricing: Pricing::Paid,
        ..component("media-generation", &[], &[], false)
    });
    let report = resolve(&g, &BTreeMap::new(), &BTreeMap::new());
    let status = report.component("media-generation").expect("in the report");
    assert_eq!(status.pricing, Pricing::Paid);
}
