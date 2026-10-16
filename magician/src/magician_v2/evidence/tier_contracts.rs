//! The channel/evidence tier-name contracts (plan workstream 3.1
//! prerequisite (b),
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! The `user.email_evidence`-style tier names are a cross-crate wire
//! contract: the comms bridges write them (`channel_providers`,
//! `writing_preferences`, `feedback_bridge`, `pattern_synthesis`), the
//! generic tier distiller reads them (`tier_distill::producer_spec`), and
//! the chat service's user-memory tier registry must list each lane's
//! suffix or the writes silently fail (the registry comment records that
//! this exact drift once dropped `chat_evidence` writes). Before 3.1 the
//! same handful of strings was hardcoded independently in `tier_distill.rs`,
//! `chat/service.rs`, and four comms modules.
//!
//! This module is the one home: full tier names as constants, the registry
//! suffix forms the chat service renders, and the contract list. Plan 3.1
//! formalized the names WITHOUT changing any string: every site now
//! references these constants, and the tests pin that the full name is
//! always `"user." + suffix` and that the producer registry, the chat tier
//! registry, and the comms bridges agree.
//!
//! Adding a lane (a new evidence tier) means: a full-name constant here, a
//! suffix constant, an entry in [`OBSERVED_EVIDENCE_AND_CHANNEL_TIERS`], a
//! `producer_spec` arm (if tier-distilled), and a `USER_MEMORY_TIERS` entry.

/// The full user-memory tier names (as written by the bridges and read by
/// the generic tier distiller). Keep each value identical to the string it
/// replaced — these are persisted-data keys, not display labels.
pub const RESEARCH_FINDINGS_TIER: &str = "user.research_findings";
pub const EMAIL_EVIDENCE_TIER: &str = "user.email_evidence";
pub const CALENDAR_EVIDENCE_TIER: &str = "user.calendar_evidence";
pub const CHAT_EVIDENCE_TIER: &str = "user.chat_evidence";
pub const WORK_EVIDENCE_TIER: &str = "user.work_evidence";
pub const CHANNEL_FEEDBACK_TIER: &str = "user.channel_feedback";
pub const CHANNEL_PATTERNS_TIER: &str = "user.channel_patterns";
pub const CHANNEL_WRITING_PREFERENCES_TIER: &str = "user.channel_writing_preferences";

/// The tier-suffix forms the chat service's `USER_MEMORY_TIERS` registry
/// renders (the registry names tiers without the `user.` prefix; the memory
/// store addresses them with it — see `normalized_user_memory_tier_name`
/// in `chat/service.rs`).
pub const RESEARCH_FINDINGS_TIER_NAME: &str = "research_findings";
pub const EMAIL_EVIDENCE_TIER_NAME: &str = "email_evidence";
pub const CALENDAR_EVIDENCE_TIER_NAME: &str = "calendar_evidence";
pub const CHAT_EVIDENCE_TIER_NAME: &str = "chat_evidence";
pub const CHANNEL_FEEDBACK_TIER_NAME: &str = "channel_feedback";
pub const CHANNEL_PATTERNS_TIER_NAME: &str = "channel_patterns";
pub const CHANNEL_WRITING_PREFERENCES_TIER_NAME: &str = "channel_writing_preferences";

/// Every observed evidence/channel lane this contract covers, as
/// `(full tier name, registry suffix name)` pairs. The pair shape IS the
/// contract: a tier reachable only under one spelling is the drift that
/// once dropped writes.
pub const OBSERVED_EVIDENCE_AND_CHANNEL_TIERS: &[(&str, &str)] = &[
    (RESEARCH_FINDINGS_TIER, RESEARCH_FINDINGS_TIER_NAME),
    (EMAIL_EVIDENCE_TIER, EMAIL_EVIDENCE_TIER_NAME),
    (CALENDAR_EVIDENCE_TIER, CALENDAR_EVIDENCE_TIER_NAME),
    (CHAT_EVIDENCE_TIER, CHAT_EVIDENCE_TIER_NAME),
    (CHANNEL_FEEDBACK_TIER, CHANNEL_FEEDBACK_TIER_NAME),
    (CHANNEL_PATTERNS_TIER, CHANNEL_PATTERNS_TIER_NAME),
    (
        CHANNEL_WRITING_PREFERENCES_TIER,
        CHANNEL_WRITING_PREFERENCES_TIER_NAME,
    ),
];

/// The registry suffix for a full tier name, if the tier is part of this
/// contract. The inverse lookup the chat registry and the bridges need to
/// stay in agreement.
pub fn tier_name_for(tier: &str) -> Option<&'static str> {
    OBSERVED_EVIDENCE_AND_CHANNEL_TIERS
        .iter()
        .find(|(full, _)| *full == tier)
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan 3.1 prerequisite (b): the tier-name contract list. Every pair is
    /// `"user." + suffix`, the producer registry agrees with the constants,
    /// and the chat tier registry lists every suffix — so a rename in any one
    /// place fails here instead of silently dropping tier writes.
    #[test]
    fn full_names_are_user_prefixed_suffixes() {
        assert!(!OBSERVED_EVIDENCE_AND_CHANNEL_TIERS.is_empty());
        for (full, name) in OBSERVED_EVIDENCE_AND_CHANNEL_TIERS {
            assert_eq!(
                format!("user.{name}"),
                *full,
                "tier {full} must be user.{name}"
            );
            assert_eq!(tier_name_for(full), Some(*name));
        }
        assert_eq!(tier_name_for("user.not_a_tier"), None);
    }

    #[test]
    fn tier_distill_producer_registry_matches_the_contract() {
        // The producers that write tier roll-up rows must name exactly the
        // contracted tiers (work_evidence is stamped, not tier-distilled,
        // and is listed here for completeness).
        let expected: &[&str] = &[
            RESEARCH_FINDINGS_TIER, // meeting
            EMAIL_EVIDENCE_TIER,    // email
            CALENDAR_EVIDENCE_TIER, // calendar
            CHAT_EVIDENCE_TIER,     // chat
            WORK_EVIDENCE_TIER,     // work_outcome
        ];
        for producer in ["meeting", "email", "calendar", "chat", "work_outcome"] {
            let spec = super::super::tier_distill::producer_spec(producer)
                .unwrap_or_else(|| panic!("producer {producer} must be registered"));
            assert!(
                expected.contains(&spec.tier),
                "producer {producer} tier {} is not a contracted tier",
                spec.tier
            );
        }
    }

    #[test]
    fn chat_user_memory_registry_lists_every_contracted_suffix() {
        // USER_MEMORY_TIERS is the acceptance check for tier writes: a lane
        // missing from it writes nowhere (the chat_evidence incident).
        let registered: Vec<&str> = crate::magician_v2::chat::service::USER_MEMORY_TIERS
            .iter()
            .map(|tier| tier.name)
            .collect();
        for (_, name) in OBSERVED_EVIDENCE_AND_CHANNEL_TIERS {
            assert!(
                registered.contains(name),
                "tier suffix {name} must stay listed in USER_MEMORY_TIERS"
            );
        }
    }
}
