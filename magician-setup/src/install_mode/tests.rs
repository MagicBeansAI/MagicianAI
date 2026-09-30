//! The offer rules, against machines nobody has to own.

use super::*;

fn machine() -> Machine {
    Machine {
        os: "macos".into(),
        source_present: true,
        prebuilt_source: None,
        artifacts_fresh: false,
        installed: None,
    }
}

fn offer(m: &Machine, mode: InstallMode) -> Offer {
    offers(m)
        .into_iter()
        .find(|o| o.mode == mode)
        .expect("every mode is always offered")
}

#[test]
fn every_path_is_always_listed_even_when_it_cannot_be_taken() {
    // A hidden choice reads as a product that does not do the thing. A greyed
    // one with a reason reads as one that does not do it yet.
    for m in [
        machine(),
        Machine {
            os: "linux".into(),
            source_present: false,
            ..machine()
        },
    ] {
        let all = offers(&m);
        assert_eq!(all.len(), 3, "the list never shrinks with the machine");
        assert!(
            all.iter().all(|o| !o.detail.is_empty()),
            "every choice says what it means"
        );
    }
}

#[test]
fn an_unavailable_path_always_says_why() {
    let m = Machine {
        os: "linux".into(),
        source_present: false,
        ..machine()
    };
    for o in offers(&m) {
        if let OfferState::Unavailable(reason) = &o.state {
            assert!(
                !reason.trim().is_empty(),
                "{} was greyed with no reason",
                o.label
            );
        }
    }
}

#[test]
fn container_is_coming_soon_rather_than_broken() {
    assert_eq!(
        offer(&machine(), InstallMode::Container).state,
        OfferState::Unavailable("coming soon".into())
    );
}

#[test]
fn prebuilt_is_offered_only_when_something_is_actually_published() {
    // Nothing is published yet, and the greyed reason is the honest form of
    // that. Offering it and failing at download would be the dishonest one.
    let greyed = offer(&machine(), InstallMode::Prebuilt);
    assert!(!greyed.state.is_ready());
    match greyed.state {
        // The refusal has to name the way out, or the path is simply dead to
        // anyone holding a package.
        OfferState::Unavailable(reason) => assert!(reason.contains("MAGICIAN_PACKAGE"), "{reason}"),
        OfferState::Ready => unreachable!(),
    }

    let have_one = Machine {
        prebuilt_source: Some("/tmp/pkg.tar.gz".into()),
        ..machine()
    };
    let ready = offer(&have_one, InstallMode::Prebuilt);
    assert!(ready.state.is_ready());
    assert!(
        ready.detail.contains("/tmp/pkg.tar.gz"),
        "say where it comes from: {}",
        ready.detail
    );
}

#[test]
fn a_missing_toolchain_never_disqualifies_building_from_source() {
    // Installing what is missing is the whole point of that path, so its
    // absence cannot be the reason the path is refused.
    assert!(offer(&machine(), InstallMode::FromSource).state.is_ready());
}

#[test]
fn without_source_the_from_source_path_says_so_and_points_somewhere() {
    let m = Machine {
        source_present: false,
        ..machine()
    };
    match offer(&m, InstallMode::FromSource).state {
        OfferState::Unavailable(reason) => {
            assert!(reason.contains("clone"), "{reason}");
            assert!(
                reason.contains("prebuilt"),
                "it must name the way out: {reason}"
            );
        },
        OfferState::Ready => panic!("cannot build from source that is not here"),
    }
}

#[test]
fn linux_is_coming_soon_rather_than_silently_missing() {
    let m = Machine {
        os: "linux".into(),
        ..machine()
    };
    match offer(&m, InstallMode::FromSource).state {
        OfferState::Unavailable(reason) => assert!(reason.contains("coming"), "{reason}"),
        OfferState::Ready => panic!("the prerequisite installer is macOS-only today"),
    }
}

#[test]
fn an_existing_install_is_named_on_every_route_that_would_replace_it() {
    // Every route ends at the same prefix, so "what is already there" is one
    // fact, not one per route — and replacing something silently is how a
    // person loses a version they were relying on.
    let installed = Machine {
        installed: Some("0.4.2".into()),
        prebuilt_source: Some("/tmp/pkg.tar.gz".into()),
        ..machine()
    };
    for mode in [InstallMode::Prebuilt, InstallMode::FromSource] {
        let detail = offer(&installed, mode).detail;
        assert!(
            detail.contains("0.4.2"),
            "{mode:?} did not mention it: {detail}"
        );
    }
    // And a clean machine is not told it is replacing anything.
    for mode in [InstallMode::Prebuilt, InstallMode::FromSource] {
        assert!(!offer(&machine(), mode).detail.contains("Replaces"));
    }
}

#[test]
fn a_second_run_says_it_will_reuse_what_is_already_built() {
    // This is the "run it again and it behaves like the prebuilt path" ask,
    // and it has to be visible before the run, not discovered during it.
    let fresh = Machine {
        artifacts_fresh: true,
        ..machine()
    };
    assert!(offer(&fresh, InstallMode::FromSource)
        .detail
        .contains("reuse"));
    assert!(offer(&machine(), InstallMode::FromSource)
        .detail
        .contains("builds"));
}
