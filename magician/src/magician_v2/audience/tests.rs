//! The invariant: an audience is a named, enumerable set with a lifetime.

use chrono::{Duration, TimeZone, Utc};

use super::{Audience, AudienceKind, AudienceRef};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn roster() -> Vec<String> {
    vec![
        "alice@example.com".to_string(),
        "bob@example.com".to_string(),
    ]
}

/// Every kind names something with members you could list. There is no public
/// variant and no wildcard — if you cannot enumerate it, it is publication, a
/// different consequence class entirely.
#[test]
fn there_is_no_way_to_express_an_open_ended_audience() {
    let kinds = [
        AudienceKind::Engagement,
        AudienceKind::Program,
        AudienceKind::Account,
        AudienceKind::Panel,
        AudienceKind::Person,
    ];
    for kind in kinds {
        assert!(!kind.as_str().is_empty());
        // Every audience is built from an explicit identity list; there is no
        // constructor that omits one.
        let audience = Audience::new(AudienceRef::new(kind, "x"), roster());
        assert_eq!(audience.size(), 2);
    }

    // The serialised form carries no wildcard either — a stored audience cannot
    // be edited into an open one without inventing a variant that does not exist.
    let json = serde_json::to_string(&Audience::new(AudienceRef::engagement("e"), roster()))
        .expect("serialise");
    for forbidden in ["public", "anyone", "*", "everyone"] {
        assert!(
            !json.contains(forbidden),
            "an audience must never serialise something that reads as open-ended ({forbidden})"
        );
    }
}

/// The kind is part of the key, so two relationships that share an id stay
/// distinct. Without it, widening the binding would silently merge them.
#[test]
fn the_kind_is_part_of_the_identity() {
    assert_eq!(AudienceRef::engagement("acme").as_key(), "engagement:acme");
    assert_eq!(AudienceRef::account("acme").as_key(), "account:acme");
    assert_ne!(
        AudienceRef::engagement("acme").as_key(),
        AudienceRef::account("acme").as_key(),
        "one company as a live deal and as a standing client are different audiences"
    );
}

/// Membership and currency are checked together. Somebody on a relationship that
/// has ended is not a member — treating "was listed" as "may see" is how access
/// outlives the relationship it came from.
#[test]
fn a_member_of_an_ended_relationship_is_not_a_member() {
    let audience = Audience::new(AudienceRef::engagement("eng-1"), roster())
        .expiring_at(now() + Duration::days(7));

    assert!(audience.admits("alice@example.com", now()));
    assert!(!audience.admits("stranger@example.com", now()));

    assert!(
        !audience.admits("alice@example.com", now() + Duration::days(8)),
        "she was listed, and the relationship has ended"
    );
    // Expiry is inclusive, like every other clock here.
    assert!(!audience.admits("alice@example.com", now() + Duration::days(7)));
    assert!(!audience.is_current(now() + Duration::days(7)));
}

/// No scheduled end is not the same as permanent — it means nothing here closes
/// it, and whatever it governs may still be closed on its own terms.
#[test]
fn an_audience_with_no_expiry_stays_current() {
    let audience = Audience::new(AudienceRef::panel("audit-2026"), roster());
    assert!(audience.expires_at.is_none());
    assert!(audience.is_current(now() + Duration::days(3650)));
    assert!(audience.admits("bob@example.com", now() + Duration::days(3650)));
}

/// An empty audience admits nobody, which is a legitimate state: a room
/// assembled before anyone is invited.
#[test]
fn an_empty_audience_admits_nobody() {
    let audience = Audience::new(AudienceRef::program("q3-intake"), Vec::new());
    assert_eq!(audience.size(), 0);
    assert!(audience.is_current(now()));
    assert!(!audience.admits("alice@example.com", now()));
}

/// A reference with no id names nothing.
#[test]
fn an_audience_reference_must_name_something() {
    assert!(AudienceRef::engagement("eng-1").is_named());
    assert!(!AudienceRef::engagement("   ").is_named());
    assert!(!AudienceRef::program("").is_named());
}

/// **Every arm of the enum can be named from a string, and `ALL` is complete.**
///
/// Pins a kind that exists in Rust and is unreachable from a route, a config
/// file or a stored record — the shape that made `Engagement` the only arm
/// production ever constructed. The `match` is exhaustive on purpose: adding a
/// variant fails to compile here, which is what forces the author to decide
/// whether it belongs in [`AudienceKind::ALL`] rather than shipping a kind
/// nothing outside the crate can ask for.
#[test]
fn every_audience_kind_is_reachable_from_its_label() {
    for kind in AudienceKind::ALL {
        let label = match kind {
            AudienceKind::Engagement => "engagement",
            AudienceKind::Program => "program",
            AudienceKind::Account => "account",
            AudienceKind::Panel => "panel",
            AudienceKind::Person => "person",
        };
        assert_eq!(kind.as_str(), label);
        assert_eq!(AudienceKind::parse(label), Some(kind));
        // A kind typed on two different days is one kind.
        assert_eq!(
            AudienceKind::parse(&format!("  {} ", label.to_uppercase())),
            Some(kind)
        );
    }
    assert_eq!(
        AudienceKind::ALL.len(),
        5,
        "a new kind that is not in `ALL` can never be named from a string"
    );
    // The five are distinct keys, so widening a binding from one to another is
    // a visible change rather than a silent merge.
    let keys: std::collections::BTreeSet<String> = AudienceKind::ALL
        .into_iter()
        .map(|kind| AudienceRef::new(kind, "acme").as_key())
        .collect();
    assert_eq!(keys.len(), 5);
    assert!(keys.contains("account:acme"));
    assert!(keys.contains("engagement:acme"));
}

/// **An unrecognised kind is `None`, not the first arm.**
///
/// Pins the fallback that would put a panel's roster, a programme's or an
/// account's under `engagement:<id>` — the one key collision `AudienceRef`
/// exists to prevent. A near miss is refused for the same reason the
/// counterparty register refuses one: a wrong answer here is shaped exactly
/// like a right one.
#[test]
fn an_unrecognised_audience_kind_is_none_rather_than_a_default() {
    for unknown in [
        "",
        "   ",
        "engagements",
        "engagement ltd",
        "eng",
        "cohort",
        "public",
        "everyone",
    ] {
        assert_eq!(
            AudienceKind::parse(unknown),
            None,
            "`{unknown}` must not be read as a kind"
        );
    }
}
