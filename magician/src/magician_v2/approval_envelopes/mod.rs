//! Approval envelopes — consent per outcome, not per act.
//!
//! Plan: `docs/plans/2026-08-07-opc-approval-envelopes.md`. Doc:
//! `docs/components/magician/approval-envelopes.md`.
//!
//! **A primitive, not an OPC feature.** §5 of the plan is explicit that nothing
//! in the model is specific to the one-person company; it was found there first.
//! So this is a sibling of `resource_authority` rather than something living
//! under `agents/`: RA governs *how much of a commodity* an act may spend,
//! envelopes govern *what class of outcome* an act may cause. Both resolve at
//! dispatch, on the same tuple, from durable grants with a consumption ledger.
//!
//! # What is built here
//!
//! Plan **phases 2 and 3**: the schema, the ledger, the resolver, and the
//! dispatch gate that applies them. The mode defaults to [`EnvelopeMode::Off`];
//! `Shadow` logs what *would* have been covered while still asking for
//! everything. Phase 4's owner surface is [`owner_view`], and phase 5's
//! generalisation past outward work is [`waiver`] — envelopes in front of
//! `requires_approval`, which is the one place every approval ask converges.
//!
//! # The one inversion worth remembering
//!
//! Resource Authority fails **open**: an action with no budget row runs
//! uncounted, because capping a declared commodity must not block every other
//! tool a user explicitly asked for. Envelopes fail **closed**: no envelope
//! means ask. These acts are autonomous, self-initiated and outward — nobody
//! asked for them, so a missing envelope must read as silence, never as consent.

use std::sync::OnceLock;

pub mod gate;
pub mod owner_view;
pub mod resolver;
pub mod store;
pub mod types;
pub mod waiver;

#[cfg(test)]
mod tests;

pub use gate::{
    envelope_scope_from_id, reason_label, shadow_log_line, DispatchContext, EnvelopeGate,
    GateOutcome,
};
pub use owner_view::{
    ConsumptionRow, EnvelopeDetail, EnvelopeStanding, EnvelopeSummary, Headroom, OwnerView,
};
pub use resolver::{resolve, resolve_any};
pub use store::{ApprovalEnvelopeStore, EnvelopeStoreScope, GrantEnvelope};
pub use types::{
    ActFacts, ApprovalEnvelope, BatchInstance, BoundaryPredicate, ConsumptionEntry,
    EnvelopeDecision, EnvelopeKind, EnvelopeLimits, EnvelopeMode, EnvelopeScope, EnvelopeState,
    NotCoveredReason,
};
pub use waiver::{
    preview_approval_waiver, resolve_approval_waiver, ApprovalContext, ApprovalWaiver,
};

static MODE: OnceLock<EnvelopeMode> = OnceLock::new();

/// Install the mode at boot. Returns `false` if one was already installed — the
/// first call wins, so a later caller cannot quietly promote shadow to
/// enforcing.
pub fn install_envelope_mode(mode: EnvelopeMode) -> bool {
    MODE.set(mode).is_ok()
}

/// The mode in force.
///
/// **Defaults to [`EnvelopeMode::Off`] when nothing was installed.** A process
/// that failed to wire its config — a test, a partially-initialised binary, an
/// entry point nobody remembered to update — must not thereby start
/// authorising outward acts nobody approved. This default is also the plan's
/// first acceptance criterion: *"with no envelopes, behaviour is byte-for-byte
/// today's"*, which is only true if the unconfigured path is silent.
pub fn envelope_mode() -> EnvelopeMode {
    *MODE.get().unwrap_or(&EnvelopeMode::Off)
}

/// Read a posture out of configuration.
///
/// `None` means the value names no posture at all — a typo, an old spelling, a
/// mode a newer binary understands and this one does not. It is deliberately
/// distinct from `Some(Off)` so a caller can say out loud that it did not
/// understand, but every caller must still treat it as `Off`: an unreadable
/// posture is not permission, and guessing at the nearest match would let a
/// misspelling authorise outward acts.
pub fn parse_envelope_mode(raw: &str) -> Option<EnvelopeMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => Some(EnvelopeMode::Off),
        "shadow" => Some(EnvelopeMode::Shadow),
        "enforcing" => Some(EnvelopeMode::Enforcing),
        _ => None,
    }
}

/// Install the posture a configuration names, and report the one now in force.
///
/// This is the bridge that was missing: [`install_envelope_mode`] had no caller
/// outside tests and no config key fed it, so [`envelope_mode`] answered `Off`
/// in every running process and the resolver never ran at all — including in
/// shadow, whose entire job is to be observed before anything is enforced.
///
/// Three properties, each of which has a way of going wrong:
///
/// - an unrecognised value degrades to `Off` rather than failing the config
///   load, because a process that will not boot is a worse answer than one that
///   asks about everything — and worse still would be guessing;
/// - the first install wins, so a config reloaded mid-process cannot promote
///   shadow to enforcing behind the operator's back;
/// - the posture is announced at whatever level it deserves. `Enforcing` means
///   some outward acts will now happen without anyone being asked, and that is
///   a warning, not an info line.
///
/// Returns the mode actually in force, which is not necessarily the one
/// requested — see the first-install rule above.
pub fn install_configured_envelope_mode(raw: &str) -> EnvelopeMode {
    let requested = match parse_envelope_mode(raw) {
        Some(mode) => mode,
        None => {
            tracing::warn!(
                configured = %raw,
                "[ENVELOPES] `approval_envelopes.mode` names no posture; reading it as `off` \
                 — an unreadable posture is not consent"
            );
            EnvelopeMode::Off
        },
    };

    let installed = install_envelope_mode(requested);
    let in_force = envelope_mode();
    if !installed && in_force != requested {
        tracing::warn!(
            requested = requested.as_str(),
            in_force = in_force.as_str(),
            "[ENVELOPES] a posture was already installed; the first one wins and this request \
             was ignored"
        );
    }

    match in_force {
        EnvelopeMode::Off => tracing::info!(
            "[ENVELOPES] approval envelopes are OFF; every gated act is asked about, exactly as \
             before envelopes existed"
        ),
        EnvelopeMode::Shadow => tracing::info!(
            "[ENVELOPE-SHADOW] approval envelopes are resolved for observation only; every gated \
             act is still asked about"
        ),
        EnvelopeMode::Enforcing => tracing::warn!(
            "[ENVELOPE-ENFORCING] a covered act will now proceed WITHOUT asking \
             (approval_envelopes.mode = enforcing)"
        ),
    }

    in_force
}

#[cfg(test)]
mod mode_parse_tests {
    use super::*;

    /// Each posture the plan names is readable, case- and whitespace-tolerantly,
    /// so `Shadow` in a hand-edited config is not silently `Off`.
    #[test]
    fn every_named_posture_parses_to_itself() {
        assert_eq!(parse_envelope_mode("off"), Some(EnvelopeMode::Off));
        assert_eq!(parse_envelope_mode("shadow"), Some(EnvelopeMode::Shadow));
        assert_eq!(
            parse_envelope_mode("enforcing"),
            Some(EnvelopeMode::Enforcing)
        );
        assert_eq!(
            parse_envelope_mode("  Shadow  "),
            Some(EnvelopeMode::Shadow)
        );
        assert_eq!(
            parse_envelope_mode("ENFORCING"),
            Some(EnvelopeMode::Enforcing)
        );
    }

    /// A value naming no posture is refused rather than rounded to the nearest
    /// one. `enforce`, `on` and `true` are all plausible ways to typo "let the
    /// acts through"; matching any of them would authorise outward acts on the
    /// strength of a misspelling.
    #[test]
    fn an_unrecognised_posture_is_refused_not_guessed() {
        for raw in [
            "",
            "   ",
            "enforce",
            "enforced",
            "on",
            "true",
            "1",
            "shadow-mode",
            "off!",
        ] {
            assert_eq!(
                parse_envelope_mode(raw),
                None,
                "`{raw}` names no posture and must not resolve to one"
            );
        }
    }

    /// The refusal above is only safe because the fallback authorises nothing.
    /// Pins the pairing: unreadable reads as `Off`, and `Off` lets nothing
    /// through.
    #[test]
    fn the_fallback_for_an_unreadable_posture_authorises_nothing() {
        let fallback = parse_envelope_mode("enforce").unwrap_or(EnvelopeMode::Off);
        assert_eq!(fallback, EnvelopeMode::Off);
        assert!(!fallback.may_authorise());
        assert!(
            !EnvelopeMode::Shadow.may_authorise(),
            "shadow observes and still asks; a shadow that authorised would not be a shadow"
        );
        assert!(EnvelopeMode::Enforcing.may_authorise());
    }
}
