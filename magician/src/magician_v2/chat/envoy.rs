//! Envoy routing — "Presto in envoy mode".
//!
//! A least-privilege agent handles messages from anyone who ISN'T the owner.
//! This module provides the pure routing helpers: classify a (channel,
//! address) pair as owner vs guest, and derive a stable per-sender guest
//! thread id. No routing is wired here — see Task A2.
//!
//! Config lives in [`crate::config::EnvoyConfig`]; design doc (archived):
//! `docs/archive/plans/2026-06-09-external-contact-envoy-agent-design.md`.
//!
//! # The lanes have names
//!
//! [`InboundLane`] replaces the `(ui_thread_id, force_agent_id)` tuple
//! [`route_for_inbound`] used to compute directly. The tuple said where a
//! message goes and never why, which made the one branch worth reviewing —
//! *does this sender carry a counterparty's authority?* — invisible at the call
//! site. `route_for_inbound` is now a projection of the lane and returns the
//! same two answers it always did.
//!
//! The third lane, [`InboundLane::Engagement`], is reachable only through
//! [`resolve_inbound_lane`] with a lane minted from an **authoritative**
//! inbound identification, and even then it projects to the guest pair until
//! [`EnvoyConfig::engagement_forwarding_enabled`] is switched on. Phase 2 of
//! `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §6.

use crate::config::EnvoyConfig;
use crate::magician_v2::counterparty_consumers::InboundIdentification;

/// `web` (the owner's UI) is always owner. Otherwise the (channel,address) must
/// be on the owner allowlist AND the message must be `verified`; everyone else
/// is a guest.
///
/// `verified` is the sender-spoofing guard: an allowlisted address only confers
/// owner trust when the transport authenticated the sender. An UNVERIFIED
/// message — e.g. an AgentMail email carrying the `unauthenticated` label (the
/// `from` is spoofable) — is treated, fail-closed, as a guest even if the
/// address is on the allowlist. Channels whose transport authenticates the
/// sender (kapso phone numbers; the local web UI) pass `verified = true`, and
/// the chat handler defaults a missing `channel_verified` to `true`, so
/// non-agentmail channels are unaffected.
///
/// Matching is exact-string, so callers must pass a **canonicalized** address
/// (e.g. E.164 for phone numbers, lower-cased for email). An un-normalized
/// address can miss the allowlist and be treated — fail-closed — as a guest.
/// Whether this channel's sender identity was established by something the
/// CALLER could not forge — readiness review §9 step 6, *"server-supplied
/// verification, so engagement routing has a real root"*.
///
/// Before this, `channel_verified` was read straight from the query string and
/// defaulted to **true**, so any caller that could reach the endpoint could
/// assert its own verification and — with an allowlisted address — be routed as
/// the owner. A verification signal the subject supplies about itself is not a
/// verification signal.
///
/// Two inputs, neither of which the caller controls:
///
/// - `request_authenticated` — whether the outer boundary attached a
///   `VerifiedRequestIdentity`. The middleware inserts it only after Cloudflare
///   Access, a paired device, or an actual loopback peer has been proved, and
///   the type is deliberately not deserializable from a payload.
/// - the channel's own transport — a phone network establishes who sent a
///   WhatsApp message; SMTP does not establish a `From:` header.
///
/// A caller MAY still de-escalate: `Some(false)` is honoured, because an adapter
/// sees things the server cannot — AgentMail's `unauthenticated` label is
/// exactly that. What a caller may never do is RAISE. That asymmetry is the
/// whole mechanism.
pub fn channel_is_verified(
    channel_type: &str,
    request_authenticated: bool,
    caller_claim: Option<bool>,
) -> bool {
    // De-escalation is always honoured; escalation never is.
    if caller_claim == Some(false) {
        return false;
    }
    request_authenticated && channel_transport_authenticates_sender(channel_type)
}

/// Whether the channel's own transport establishes who sent a message.
///
/// Unknown channels fail closed. A channel nobody has classified is not one
/// whose sender we can vouch for, and defaulting it to verified is how the
/// original hole came to exist.
fn channel_transport_authenticates_sender(channel_type: &str) -> bool {
    match channel_type.trim().to_ascii_lowercase().as_str() {
        // The request IS the sender: an authenticated browser or loopback
        // session, already proved by the middleware.
        "web" => true,
        // Platform-authenticated sender identity — a number or account the
        // provider verified before delivering to us.
        "kapso" | "whatsapp" | "telegram" | "imessage" | "sms" => true,
        // SMTP does not authenticate `From:`. This is the case the original
        // `channel_verified=false` was introduced for, now the default rather
        // than an adapter's courtesy.
        "agentmail" | "email" | "mail" => false,
        _ => false,
    }
}

/// Whether an inbound message is from the owner.
///
/// # Web is the owner's UI only once the request is proved
///
/// `web` has no address to allowlist — the authenticated request *is* the
/// sender — so it resolves on `verified` alone rather than on an identity match.
///
/// It must still resolve on `verified`. This previously returned `true` for
/// `web` unconditionally, and `channel_type` is caller-supplied and **defaults
/// to `web`** (`chat_api::resolve_inbound_routing`), so any request that simply
/// omitted `channel` was routed as the owner no matter what the outer boundary
/// said. `channel_is_verified` was computed and then discarded on exactly the
/// default path, which left §9 step 6 closed for allowlisted addresses on other
/// channels and wide open on the one everything actually uses.
///
/// A genuine local UI is unaffected: the middleware attaches a verified identity
/// for Cloudflare Access, a paired device or a real loopback peer, and
/// `channel_is_verified("web", true, None)` is `true`.
///
/// The channel is normalised here because [`channel_transport_authenticates_sender`]
/// normalises too — otherwise `"Web"` and `"web"` would take different paths
/// through two functions that must agree.
pub fn is_owner(cfg: &EnvoyConfig, channel_type: &str, address: &str, verified: bool) -> bool {
    if channel_type.trim().eq_ignore_ascii_case("web") {
        return verified;
    }
    verified && cfg.has_owner_identity(channel_type, address)
}

/// Stable per-sender thread id for a guest. Same sender → same thread.
///
/// The address is used verbatim, so callers must pass the same **canonicalized**
/// address they use for [`is_owner`] — the same sender in two forms would
/// otherwise split into two threads.
pub fn guest_thread_id(channel_type: &str, address: &str) -> String {
    format!("ext:{channel_type}:{address}")
}

/// Stable per-engagement thread id. Every proved identity of one counterparty
/// lands on the engagement's own thread rather than on a per-sender one,
/// because the engagement is the conversation — two people at the same
/// organisation writing about the same work are one thread, not two.
///
/// Deliberately a different prefix from [`guest_thread_id`]: a guest thread and
/// an engagement thread carry different authority, and a shared namespace would
/// let a crafted address collide with an engagement's thread.
pub fn engagement_thread_id(engagement_id: &str) -> String {
    format!("eng:{engagement_id}")
}

// ---------------------------------------------------------------------------
// Typed lanes
// ---------------------------------------------------------------------------

/// Where a message that is not the owner's goes: a stable per-sender thread,
/// bound to the configured envoy agent.
///
/// Exactly the pair [`route_for_inbound`] has always produced for everyone who
/// is not the owner, given a name so the next change can be reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestLane {
    pub ui_thread_id: String,
    pub agent_id: String,
}

/// A live engagement an inbound message is allowed to be forwarded to.
///
/// **The type is the gate.** Its fields are private and
/// [`EngagementLane::for_authoritative_sender`] is the only way to build one:
/// it answers `None` for every [`InboundIdentification`] variant except
/// [`InboundIdentification::Authoritative`]. `ContextOnly` — the address is on
/// file but either the channel did not prove the sender or nobody proved the
/// address — cannot mint one, and neither can `Unrecognised`.
///
/// That asymmetry is the whole point of the engagements plan. Resolving an
/// address is cheap; treating a resolution as authority hands one
/// counterparty's context to whoever can spell its address, and a wrong answer
/// there is shaped exactly like a right one. So the promotion from *context*
/// to *authority* is an owner act (§3.2), and no code path here performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngagementLane {
    engagement_id: String,
    owner_agent_id: String,
    ui_thread_id: String,
}

impl EngagementLane {
    /// The only constructor, and it refuses everything that is not proof.
    ///
    /// `None` when the sender is not authoritative, and `None` when any of the
    /// three strings is blank — a blank agent id would force selection of an
    /// agent named `""`, which is a worse answer than the guest lane rather
    /// than a weaker one.
    pub fn for_authoritative_sender(
        identification: &InboundIdentification,
        engagement_id: &str,
        owner_agent_id: &str,
        ui_thread_id: &str,
    ) -> Option<Self> {
        // `authority()` answers `Some` for exactly one variant. `None` is never
        // permission — it covers both a stranger and a known address nobody
        // proved this message came from.
        identification.authority()?;
        let engagement_id = engagement_id.trim();
        let owner_agent_id = owner_agent_id.trim();
        let ui_thread_id = ui_thread_id.trim();
        if engagement_id.is_empty() || owner_agent_id.is_empty() || ui_thread_id.is_empty() {
            return None;
        }
        Some(Self {
            engagement_id: engagement_id.to_string(),
            owner_agent_id: owner_agent_id.to_string(),
            ui_thread_id: ui_thread_id.to_string(),
        })
    }

    pub fn engagement_id(&self) -> &str {
        &self.engagement_id
    }

    pub fn owner_agent_id(&self) -> &str {
        &self.owner_agent_id
    }

    pub fn ui_thread_id(&self) -> &str {
        &self.ui_thread_id
    }
}

/// Which lane an inbound message belongs in.
///
/// The tuple this replaces said *where* a message goes and never *why*, so the
/// two decisions that matter — is this the owner, and is this a counterparty we
/// have authority-bearing proof of — were invisible at every call site. Naming
/// them is the entire content of Phase 2 of the engagements plan; the routing
/// they project to is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundLane {
    /// Full trust. Keeps the requested thread and forces no agent, so owner
    /// sessions retain normal preferred-agent selection.
    Owner { ui_thread_id: String },
    /// The sender is an authoritatively proved identity of a counterparty with
    /// one live engagement.
    ///
    /// Carries the guest lane it would otherwise have taken, so the projection
    /// while forwarding is off is the previous answer verbatim rather than one
    /// recomputed from remembered inputs.
    Engagement {
        engagement: EngagementLane,
        guest_fallback: GuestLane,
    },
    /// Everyone else, fail-closed.
    Guest(GuestLane),
}

impl InboundLane {
    /// A stable label for logs and review. Not a routing input.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Owner { .. } => "owner",
            Self::Engagement { .. } => "engagement",
            Self::Guest(_) => "guest",
        }
    }

    /// The engagement this message was resolved into, whether or not
    /// forwarding is enabled. Read this to report which messages *would* have
    /// matched (Phase 1's observation) without changing where any of them go.
    pub fn engagement(&self) -> Option<&EngagementLane> {
        match self {
            Self::Engagement { engagement, .. } => Some(engagement),
            Self::Owner { .. } | Self::Guest(_) => None,
        }
    }

    /// Project the lane onto the `(ui_thread_id, force_agent_id)` pair the
    /// active-session endpoint consumes.
    ///
    /// `Owner` and `Guest` project to exactly what [`route_for_inbound`]
    /// returned before this enum existed. `Engagement` projects to the guest
    /// pair too unless [`EnvoyConfig::engagement_forwarding_enabled`] is on —
    /// that flag is default-off, which is what makes Phase 2 a no-op.
    pub fn into_routing(self, cfg: &EnvoyConfig) -> (String, Option<String>) {
        match self {
            Self::Owner { ui_thread_id } => (ui_thread_id, None),
            Self::Guest(guest) => (guest.ui_thread_id, Some(guest.agent_id)),
            Self::Engagement {
                engagement,
                guest_fallback,
            } => {
                if cfg.engagement_forwarding_enabled {
                    (engagement.ui_thread_id, Some(engagement.owner_agent_id))
                } else {
                    (guest_fallback.ui_thread_id, Some(guest_fallback.agent_id))
                }
            },
        }
    }
}

/// Pure lane decision for an inbound message.
///
/// Resolution order, fail-closed at every step (§4.2 of the engagements plan):
///
/// 1. **owner** — unchanged, and still first. An allowlisted sender on a
///    verified channel that did not explicitly disclaim control intent is the
///    owner whether or not an engagement was resolved for it.
/// 2. **engagement** — a lane that was minted from an authoritative
///    identification, on a message the transport also proved. Both legs are
///    required: `verified` is re-checked here even though
///    [`EngagementLane::for_authoritative_sender`] already refused a
///    non-authoritative sender, because this is the branch that grants and a
///    single check is a single place to get it wrong.
/// 3. **guest** — everyone else, including every message for which `engagement`
///    is `None`. An expired, revoked, unknown or ambiguous engagement arrives
///    here as `None` and therefore degrades to guest with no special case.
///
/// `control_intent` gates only the owner branch, exactly as before. It is a
/// statement about whether *the owner* asked for Magician control, and a
/// counterparty's ordinary message must not be pushed out of its engagement by
/// the absence of a control prefix it would never send.
#[allow(clippy::too_many_arguments)]
pub fn resolve_inbound_lane(
    cfg: &EnvoyConfig,
    channel_type: &str,
    address: &str,
    verified: bool,
    control_intent: Option<bool>,
    requested_ui_thread_id: &str,
    engagement: Option<EngagementLane>,
) -> InboundLane {
    if control_intent != Some(false) && is_owner(cfg, channel_type, address, verified) {
        return InboundLane::Owner {
            ui_thread_id: requested_ui_thread_id.to_string(),
        };
    }
    let guest_fallback = GuestLane {
        ui_thread_id: guest_thread_id(channel_type, address),
        agent_id: cfg.envoy_agent_id.clone(),
    };
    match engagement {
        Some(engagement) if verified => InboundLane::Engagement {
            engagement,
            guest_fallback,
        },
        _ => InboundLane::Guest(guest_fallback),
    }
}

/// Pure routing decision for an inbound message. Returns the
/// `(ui_thread_id, force_agent_id)` the active-session endpoint should use.
///
/// - **Non-control** (`control_intent = Some(false)`) is treated as a guest even
///   when the sender is allowlisted. This lets agent-owned surfaces like Kapso
///   behave as normal envoy conversations unless the owner explicitly invokes a
///   Magician control prefix.
/// - **Owner** (web, or an allowlisted channel/address, and not explicitly
///   non-control) keeps the requested thread and does NOT force an agent
///   (`force_agent_id = None`) — owner sessions retain normal preferred-agent
///   selection.
/// - **Guest** (everyone else, fail-closed) is routed to a stable per-sender
///   thread and bound to the configured envoy agent for any NEW session.
///
/// The address must be **canonicalized** the same way as for [`is_owner`].
/// `verified` is threaded straight to [`is_owner`] — an unverified message from
/// an allowlisted address routes to the guest thread + envoy, not the owner's.
///
/// # This is now a projection of [`resolve_inbound_lane`]
///
/// The lane is the decision and this pair is a view of it. Callers that only
/// need somewhere to put the message keep this signature; callers that need to
/// know *which lane and why* — logging, review, the Phase 3 forwarding switch —
/// call [`resolve_inbound_lane`] and read the enum.
///
/// It passes `engagement: None`, so [`InboundLane::Engagement`] is
/// **unreachable through this function** and the two remaining lanes project to
/// the same two answers this returned before the enum existed. That is the
/// no-op, by construction rather than by inspection.
pub fn route_for_inbound(
    cfg: &EnvoyConfig,
    channel_type: &str,
    address: &str,
    verified: bool,
    control_intent: Option<bool>,
    requested_ui_thread_id: &str,
) -> (String, Option<String>) {
    resolve_inbound_lane(
        cfg,
        channel_type,
        address,
        verified,
        control_intent,
        requested_ui_thread_id,
        None,
    )
    .into_routing(cfg)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// §9 step 6: verification must have a root the caller cannot supply.
    ///
    /// The hole this closes: `channel_verified` was read from the query string
    /// and defaulted to TRUE, so anyone who could reach the endpoint was
    /// verified by default — and with an allowlisted address, routed as the
    /// OWNER.
    #[test]
    fn a_caller_cannot_assert_its_own_verification() {
        // The old default. An unauthenticated request claiming verification
        // must not get it, however it asks.
        for claim in [None, Some(true)] {
            assert!(
                !channel_is_verified("whatsapp", false, claim),
                "an unauthenticated request was treated as verified (claim={claim:?})"
            );
        }
        // Authenticated request on a transport that names its sender: verified.
        assert!(channel_is_verified("whatsapp", true, None));
        assert!(channel_is_verified("whatsapp", true, Some(true)));
    }

    /// A caller may DE-escalate but never escalate. The asymmetry is the whole
    /// mechanism: an adapter sees things the server cannot (AgentMail's
    /// `unauthenticated` label), so its "this was not verified" is information;
    /// its "this was verified" is a claim about itself.
    #[test]
    fn a_caller_may_lower_verification_and_never_raise_it() {
        // Lowering is honoured even when the server would have said verified.
        assert!(!channel_is_verified("whatsapp", true, Some(false)));
        assert!(!channel_is_verified("web", true, Some(false)));

        // Raising is ignored on a transport that cannot name its sender.
        assert!(
            !channel_is_verified("agentmail", true, Some(true)),
            "SMTP does not authenticate `From:`; a caller saying otherwise \
             cannot make it so"
        );
    }

    /// A channel nobody classified is not verified. Defaulting an unknown
    /// channel to trusted is precisely how the original hole came to exist, so
    /// the new code must not reintroduce it for the next channel added.
    #[test]
    fn an_unclassified_channel_fails_closed() {
        for channel in ["", "slack", "discord", "some-future-channel", "  "] {
            assert!(
                !channel_is_verified(channel, true, None),
                "unclassified channel `{channel}` was treated as verified"
            );
        }
    }

    /// Owner routing is the consequence that made this worth fixing: the
    /// allowlist alone is not enough, the channel must also be verified.
    #[test]
    fn an_unverified_channel_is_never_the_owner_even_on_the_allowlist() {
        let mut cfg = EnvoyConfig::default();
        cfg.owner_identities
            .insert("whatsapp".to_string(), vec!["+911234567890".to_string()]);

        let verified = channel_is_verified("whatsapp", true, None);
        assert!(is_owner(&cfg, "whatsapp", "+911234567890", verified));

        // Same allowlisted address, unauthenticated request.
        let unverified = channel_is_verified("whatsapp", false, Some(true));
        assert!(
            !is_owner(&cfg, "whatsapp", "+911234567890", unverified),
            "an unauthenticated caller reached OWNER routing by claiming \
             verification with an allowlisted address"
        );
    }

    fn cfg() -> EnvoyConfig {
        let mut m = std::collections::HashMap::new();
        m.insert("kapso".into(), vec!["+15551234567".into()]);
        m.insert("agentmail".into(), vec!["owner@example.com".into()]);
        EnvoyConfig {
            owner_identities: m,
            envoy_agent_id: "envoy".into(),
            ..EnvoyConfig::default()
        }
    }
    #[test]
    fn owner_phone_is_owner() {
        assert!(is_owner(&cfg(), "kapso", "+15551234567", true));
    }
    #[test]
    fn other_phone_is_guest() {
        assert!(!is_owner(&cfg(), "kapso", "+19998887777", true));
    }
    /// `web` has no address to allowlist, so it resolves on `verified` alone —
    /// but it MUST resolve on it.
    ///
    /// `channel_type` is caller-supplied and defaults to `web`, so a `web` that
    /// ignored `verified` meant every request omitting `channel` was the owner
    /// whatever the outer boundary said. That is the hole §9 step 6 exists to
    /// close, sitting on the default path.
    #[test]
    fn web_is_owner_only_once_the_request_is_proved() {
        assert!(
            is_owner(&cfg(), "web", "", true),
            "a proved request on the owner's UI is the owner"
        );
        assert!(
            !is_owner(&cfg(), "web", "", false),
            "an unproved request must not become the owner by naming the default channel"
        );
    }

    /// The two functions that decide who is speaking must normalise the channel
    /// the same way, or `Web` and `web` take different paths through them.
    #[test]
    fn channel_matching_is_case_and_whitespace_insensitive() {
        for spelling in ["web", "Web", " WEB "] {
            assert!(is_owner(&cfg(), spelling, "", true), "{spelling} verified");
            assert!(
                !is_owner(&cfg(), spelling, "", false),
                "{spelling} unverified"
            );
        }
    }

    /// The end-to-end shape of the hole: the default channel, unauthenticated,
    /// must route to the guest thread and the envoy agent rather than the
    /// owner's thread.
    #[test]
    fn an_unauthenticated_default_channel_request_routes_as_a_guest() {
        let verified = channel_is_verified("web", false, None);
        assert!(!verified);

        let (thread, forced_agent) =
            route_for_inbound(&cfg(), "web", "", verified, None, "owner-ui-thread");
        assert_ne!(
            thread, "owner-ui-thread",
            "must not reach the owner's thread"
        );
        assert!(
            forced_agent.is_some(),
            "an unproved caller must be bound to the envoy agent"
        );
    }
    #[test]
    fn agentmail_owner_email_verified_is_owner() {
        // An authenticated email from an allowlisted owner address → owner.
        assert!(is_owner(&cfg(), "agentmail", "owner@example.com", true));
    }
    #[test]
    fn agentmail_owner_email_unverified_is_guest() {
        // THE SPOOF GATE: an `unauthenticated` email from the owner's address is
        // treated as a guest — a spoofed `from` can't claim owner trust.
        assert!(!is_owner(&cfg(), "agentmail", "owner@example.com", false));
    }
    #[test]
    fn guest_thread_id_is_stable_and_per_sender() {
        assert_eq!(guest_thread_id("kapso", "+1999"), "ext:kapso:+1999");
        assert_ne!(
            guest_thread_id("kapso", "+1999"),
            guest_thread_id("kapso", "+1888")
        );
    }
    #[test]
    fn unknown_sender_is_guest() {
        // Fail-closed: an address not on the allowlist is a guest even when verified.
        assert!(!is_owner(&cfg(), "agentmail", "someone@example.com", true));
    }
    #[test]
    fn default_config_uses_envoy_agent() {
        // The manual Default impl exists to guarantee these (a derived Default
        // would set envoy_agent_id to "").
        let d = EnvoyConfig::default();
        assert_eq!(d.envoy_agent_id, "envoy");
        assert!(d.owner_identities.is_empty());
        assert!(d.owner_identity_envs.is_empty());
    }

    #[test]
    fn route_owner_web_keeps_requested_thread_no_force() {
        // Web is always owner: keep the requested thread, force no agent.
        assert_eq!(
            route_for_inbound(&cfg(), "web", "", true, None, "general"),
            ("general".to_string(), None)
        );
    }

    #[test]
    fn route_owner_allowlisted_kapso_keeps_requested_thread_no_force() {
        // Allowlisted phone is the owner: keep requested thread, no forced agent.
        assert_eq!(
            route_for_inbound(&cfg(), "kapso", "+15551234567", true, None, "general"),
            ("general".to_string(), None)
        );
    }

    #[test]
    fn route_non_control_allowlisted_kapso_uses_guest_thread_and_envoy_agent() {
        // An owner texting the agent-owned Kapso number without an explicit
        // control prefix is an ordinary envoy conversation, not full-trust
        // Magician control.
        assert_eq!(
            route_for_inbound(
                &cfg(),
                "kapso",
                "+15551234567",
                true,
                Some(false),
                "general"
            ),
            (
                "ext:kapso:+15551234567".to_string(),
                Some("envoy".to_string())
            )
        );
    }

    #[test]
    fn route_control_intent_does_not_bypass_sender_auth_gate() {
        // `control_intent=true` means "the message asked for control"; it is not
        // authentication. Spoof-gated channels still need verified sender auth.
        assert_eq!(
            route_for_inbound(
                &cfg(),
                "agentmail",
                "owner@example.com",
                false,
                Some(true),
                "general"
            ),
            (
                "ext:agentmail:owner@example.com".to_string(),
                Some("envoy".to_string())
            )
        );
    }

    #[test]
    fn route_guest_kapso_uses_guest_thread_and_envoy_agent() {
        // Unknown sender (guest): per-sender thread + bind the envoy agent.
        assert_eq!(
            route_for_inbound(&cfg(), "kapso", "+1999", true, None, "general"),
            ("ext:kapso:+1999".to_string(), Some("envoy".to_string()))
        );
    }

    #[test]
    fn route_unverified_owner_email_goes_to_guest_thread() {
        // THE SPOOF GATE end-to-end: an unauthenticated email from the owner's
        // own address routes to the per-sender guest thread + envoy, NOT the
        // owner's thread — so a spoofed `from` never reaches full-trust Presto.
        assert_eq!(
            route_for_inbound(
                &cfg(),
                "agentmail",
                "owner@example.com",
                false,
                None,
                "general"
            ),
            (
                "ext:agentmail:owner@example.com".to_string(),
                Some("envoy".to_string())
            )
        );
    }

    // ── Phase 2: the typed lane, and the proof it changed nothing ───────────

    use crate::magician_v2::counterparty_consumers::NotAuthoritative;
    use crate::magician_v2::counterparty_types::CounterpartyRef;

    /// Every routing input this file already pins, as data, so the no-op claim
    /// is checked against the same branches the old tuple was checked against.
    ///
    /// `(channel, address, verified, control_intent, requested_thread)` paired
    /// with the exact pair the pre-enum `route_for_inbound` returned.
    #[allow(clippy::type_complexity)]
    fn routing_branches() -> Vec<(
        (&'static str, &'static str, bool, Option<bool>, &'static str),
        (&'static str, Option<&'static str>),
    )> {
        vec![
            // Owner: web, proved request.
            (("web", "", true, None, "general"), ("general", None)),
            // Web, unproved: the default-channel hole, guest.
            (
                ("web", "", false, None, "owner-ui-thread"),
                ("ext:web:", Some("envoy")),
            ),
            // Owner: allowlisted phone on a transport that names its sender.
            (
                ("kapso", "+15551234567", true, None, "general"),
                ("general", None),
            ),
            // Allowlisted, but explicitly not a control message.
            (
                ("kapso", "+15551234567", true, Some(false), "general"),
                ("ext:kapso:+15551234567", Some("envoy")),
            ),
            // Control intent is not authentication.
            (
                (
                    "agentmail",
                    "owner@example.com",
                    false,
                    Some(true),
                    "general",
                ),
                ("ext:agentmail:owner@example.com", Some("envoy")),
            ),
            // Unknown sender.
            (
                ("kapso", "+1999", true, None, "general"),
                ("ext:kapso:+1999", Some("envoy")),
            ),
            // The spoof gate.
            (
                ("agentmail", "owner@example.com", false, None, "general"),
                ("ext:agentmail:owner@example.com", Some("envoy")),
            ),
            // Case/whitespace spellings of the default channel.
            ((" WEB ", "", true, None, "general"), ("general", None)),
            (
                (" WEB ", "", false, None, "general"),
                ("ext: WEB :", Some("envoy")),
            ),
        ]
    }

    /// **The refactor must not move a single message.**
    ///
    /// The failure this pins is the one that makes the whole change worthless:
    /// `route_for_inbound` is now a projection of [`InboundLane`], so a lane
    /// that resolved the owner branch differently — or that lost the guest
    /// thread's exact spelling — would silently re-route live traffic while
    /// looking like a tidy-up. Values, not shapes: every branch asserts the
    /// literal thread id and forced agent the tuple returned before the enum
    /// existed.
    #[test]
    fn the_typed_lane_projects_to_exactly_the_previous_routing() {
        let cfg = cfg();
        for ((channel, address, verified, control_intent, requested), (thread, agent)) in
            routing_branches()
        {
            let expected = (thread.to_string(), agent.map(str::to_string));

            assert_eq!(
                route_for_inbound(&cfg, channel, address, verified, control_intent, requested),
                expected,
                "route_for_inbound changed for ({channel}, {address}, verified={verified}, \
                 control_intent={control_intent:?})"
            );

            // And the lane it is a projection of agrees, with no engagement in
            // play — which is every call the one production call site makes.
            let lane = resolve_inbound_lane(
                &cfg,
                channel,
                address,
                verified,
                control_intent,
                requested,
                None,
            );
            assert_eq!(
                lane.clone().into_routing(&cfg),
                expected,
                "the lane projected somewhere else for ({channel}, {address})"
            );
            assert!(
                lane.engagement().is_none(),
                "no engagement was supplied, so no engagement lane may be resolved"
            );
        }
    }

    /// Naming the lanes must not merge two of them: an owner keeps the
    /// requested thread and forces no agent, a guest gets neither.
    ///
    /// Pins the mistake of an enum whose projection is right for the pair but
    /// whose variant is wrong — routing correct today, and wrong the moment
    /// anything reads `lane.name()` to decide what a message may do.
    #[test]
    fn each_branch_lands_in_the_lane_it_is_named_after() {
        let cfg = cfg();
        let lane_name = |channel, address, verified, control_intent| {
            resolve_inbound_lane(
                &cfg,
                channel,
                address,
                verified,
                control_intent,
                "general",
                None,
            )
            .name()
        };
        assert_eq!(lane_name("web", "", true, None), "owner");
        assert_eq!(lane_name("web", "", false, None), "guest");
        assert_eq!(lane_name("kapso", "+15551234567", true, None), "owner");
        assert_eq!(
            lane_name("kapso", "+15551234567", true, Some(false)),
            "guest"
        );
        assert_eq!(lane_name("kapso", "+1999", true, None), "guest");
        assert_eq!(
            lane_name("agentmail", "owner@example.com", false, None),
            "guest"
        );
    }

    fn authoritative() -> InboundIdentification {
        InboundIdentification::Authoritative {
            counterparty: CounterpartyRef::new("cp-acme"),
            identity_id: "id-1".to_string(),
            channel: "whatsapp".to_string(),
            normalised: "+15550001111".to_string(),
        }
    }

    fn context_only(why: NotAuthoritative) -> InboundIdentification {
        InboundIdentification::ContextOnly {
            counterparty: CounterpartyRef::new("cp-acme"),
            identity_id: "id-1".to_string(),
            channel: "email".to_string(),
            normalised: "ops@acme.com".to_string(),
            why,
        }
    }

    fn lane() -> EngagementLane {
        EngagementLane::for_authoritative_sender(
            &authoritative(),
            "eng-1",
            "ambassador",
            &engagement_thread_id("eng-1"),
        )
        .expect("an authoritative sender mints a lane")
    }

    /// **An unverified resolution must never mint an engagement lane.**
    ///
    /// This is the escalation the engagements plan exists to prevent: the
    /// address is on file, so `for_context` answers, and a constructor that
    /// took a `CounterpartyRef` instead of the identification would accept it
    /// happily. A `ContextOnly` sender is one SMTP `From:` away from being
    /// anybody, and `Unrecognised` is nobody at all.
    #[test]
    fn only_an_authoritative_identification_can_mint_an_engagement_lane() {
        let thread = engagement_thread_id("eng-1");
        for why in [
            NotAuthoritative::ChannelDidNotProveSender,
            NotAuthoritative::AddressNotVerified,
            NotAuthoritative::NeitherProved,
        ] {
            let identification = context_only(why);
            assert!(
                identification.for_context().is_some(),
                "the fixture must be the dangerous case: on file, but unproved"
            );
            assert_eq!(
                EngagementLane::for_authoritative_sender(
                    &identification,
                    "eng-1",
                    "ambassador",
                    &thread
                ),
                None,
                "a {why:?} sender minted an engagement lane"
            );
        }

        let stranger = InboundIdentification::Unrecognised {
            channel: "whatsapp".to_string(),
            normalised: "+15559999999".to_string(),
        };
        assert_eq!(
            EngagementLane::for_authoritative_sender(&stranger, "eng-1", "ambassador", &thread),
            None,
            "a stranger minted an engagement lane"
        );

        let minted = EngagementLane::for_authoritative_sender(
            &authoritative(),
            "eng-1",
            "ambassador",
            &thread,
        )
        .expect("an authoritative sender mints a lane");
        assert_eq!(minted.engagement_id(), "eng-1");
        assert_eq!(minted.owner_agent_id(), "ambassador");
        assert_eq!(minted.ui_thread_id(), "eng:eng-1");
    }

    /// A blank field is not a weaker lane, it is a broken one.
    ///
    /// Pins the failure where a missing owner agent forces selection of an
    /// agent named `""` — worse than the guest lane, and invisible until a
    /// message lands nowhere.
    #[test]
    fn a_lane_with_a_blank_field_is_refused_rather_than_forced() {
        let identification = authoritative();
        for (engagement_id, agent, thread) in [
            ("", "ambassador", "eng:eng-1"),
            ("eng-1", "", "eng:eng-1"),
            ("eng-1", "ambassador", ""),
            ("   ", "ambassador", "eng:eng-1"),
            ("eng-1", "  ", "eng:eng-1"),
        ] {
            assert_eq!(
                EngagementLane::for_authoritative_sender(
                    &identification,
                    engagement_id,
                    agent,
                    thread
                ),
                None,
                "({engagement_id:?}, {agent:?}, {thread:?}) minted a lane"
            );
        }
    }

    /// **Phase 2 is a no-op: a resolved engagement changes nothing yet.**
    ///
    /// The failure pinned is a flag that is either absent or defaults on — the
    /// lane would then start forwarding counterparty mail to an agent holding
    /// engagement authority the moment this code merged, which is Phase 3 and a
    /// separate deliberate act.
    #[test]
    fn an_engagement_lane_routes_exactly_like_a_guest_while_forwarding_is_off() {
        let cfg = cfg();
        assert!(
            !cfg.engagement_forwarding_enabled,
            "engagement forwarding must be off unless somebody turned it on"
        );
        assert!(
            !EnvoyConfig::default().engagement_forwarding_enabled,
            "the default config must not forward"
        );

        let resolved = resolve_inbound_lane(
            &cfg,
            "kapso",
            "+15550001111",
            true,
            None,
            "general",
            Some(lane()),
        );
        assert_eq!(resolved.name(), "engagement", "the lane must still resolve");
        assert_eq!(
            resolved.engagement().map(EngagementLane::engagement_id),
            Some("eng-1"),
            "the resolution is the observable half of Phase 2"
        );
        assert_eq!(
            resolved.into_routing(&cfg),
            (
                "ext:kapso:+15550001111".to_string(),
                Some("envoy".to_string())
            ),
            "a resolved engagement must land byte-for-byte where the guest lane lands"
        );
    }

    /// The flag is the only thing that moves the message — and when it moves
    /// it, it moves it to the engagement's thread and its owner agent.
    ///
    /// Pins a projection that ignores the flag in the other direction: a lane
    /// that never forwards makes Phase 3 unshippable and the enum decorative.
    #[test]
    fn switching_forwarding_on_sends_the_message_to_the_engagements_agent() {
        let cfg = EnvoyConfig {
            engagement_forwarding_enabled: true,
            ..cfg()
        };
        let resolved = resolve_inbound_lane(
            &cfg,
            "kapso",
            "+15550001111",
            true,
            None,
            "general",
            Some(lane()),
        );
        assert_eq!(
            resolved.into_routing(&cfg),
            ("eng:eng-1".to_string(), Some("ambassador".to_string()))
        );
    }

    /// **An unverified message never reaches the engagement lane**, even when a
    /// lane was minted for the sender on a previous, proved message.
    ///
    /// Pins the second half of the spoof gate: `for_authoritative_sender`
    /// guards who the sender is, this guards whether *this message* was proved.
    /// A caller that resolved a lane once and reused it across an unverified
    /// inbound would otherwise hand an engagement to a spoofed `From:`.
    #[test]
    fn an_unverified_message_falls_back_to_guest_even_holding_a_lane() {
        let cfg = EnvoyConfig {
            engagement_forwarding_enabled: true,
            ..cfg()
        };
        let resolved = resolve_inbound_lane(
            &cfg,
            "agentmail",
            "ops@acme.com",
            false,
            None,
            "general",
            Some(lane()),
        );
        assert_eq!(resolved.name(), "guest");
        assert_eq!(
            resolved.into_routing(&cfg),
            (
                "ext:agentmail:ops@acme.com".to_string(),
                Some("envoy".to_string())
            )
        );
    }

    /// The owner branch stays first. An engagement resolved for an allowlisted
    /// owner address must not demote the owner into a counterparty's lane.
    ///
    /// Pins the ordering mistake: resolve the engagement before the owner check
    /// and the owner's own thread disappears behind an engagement thread, with
    /// a forced agent where there was none.
    #[test]
    fn an_engagement_never_outranks_the_owner() {
        let cfg = EnvoyConfig {
            engagement_forwarding_enabled: true,
            ..cfg()
        };
        let resolved = resolve_inbound_lane(
            &cfg,
            "kapso",
            "+15551234567",
            true,
            None,
            "general",
            Some(lane()),
        );
        assert_eq!(resolved.name(), "owner");
        assert_eq!(resolved.into_routing(&cfg), ("general".to_string(), None));
    }

    /// Guest and engagement threads must not share a namespace.
    ///
    /// Pins a collision: with one prefix, a sender whose address spelled
    /// another engagement's id would land on that engagement's thread.
    #[test]
    fn engagement_threads_and_guest_threads_are_different_namespaces() {
        assert_eq!(engagement_thread_id("eng-1"), "eng:eng-1");
        assert_eq!(guest_thread_id("kapso", "+1999"), "ext:kapso:+1999");
        assert_ne!(
            engagement_thread_id("kapso:+1999"),
            guest_thread_id("kapso", "+1999")
        );
    }
}
