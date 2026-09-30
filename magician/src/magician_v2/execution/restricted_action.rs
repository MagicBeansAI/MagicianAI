//! The bindable form of an outward action — plan §4A.
//!
//! A generic primitive, like its neighbour [`super::effective_action`]. Any gate
//! that grants authority needs an act whose argument surface is **closed**;
//! this builds one.
//!
//! # Why a restricted form rather than better inspection
//!
//! `resolve_effective_action` can recover recipients hidden in a passthrough,
//! which makes a *record* truthful. It cannot make the act safe to authorise,
//! because the next token nobody modelled changes the act again. The only fix is
//! an action that has no escape hatch at all — then
//! `EffectiveAction::is_bindable` is true by construction rather than reported
//! as false.
//!
//! §4A lists seven requirements. Six live here:
//!
//! 1. resolve the complete effective request, after argument expansion;
//! 2. **no arbitrary escape hatch** — no `extra_args`, no raw action passthrough;
//! 3. **bind sender identity, never caller-chosen**;
//! 4. canonicalise recipients;
//! 5. evaluate the envelope AFTER canonicalisation — the caller's obligation,
//!    which [`BoundDispatch`] makes natural by being the only thing carrying a
//!    bindable [`EffectiveAction`];
//! 6. an idempotency key, so a retry cannot double-send.
//!
//! The seventh — persist the provider receipt — belongs to the outward
//! assertions store, which already has the state for it.
//!
//! # The raw actions do not go away
//!
//! §4A: *"The raw actions remain for interactive and administrative use. An
//! autonomous agent receives only the restricted form."* A person driving a tool
//! directly is not the threat model; an autonomous loop composing arguments is.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::effective_action::{resolve_effective_action, EffectiveAction};

/// A closed argument surface for one outward capability.
///
/// `allowed_parameters` is exhaustive: anything not named is refused rather than
/// dropped. Dropping would be quieter and worse — an agent whose argument
/// vanished silently gets a send that is not the one it composed, and nobody
/// finds out until a person reads the mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictedAction {
    pub capability: &'static str,
    /// Parameters the caller may supply.
    pub allowed_parameters: &'static [&'static str],
    /// Parameters carrying the sender identity. **Overwritten** by the bound
    /// sender, whatever the caller supplied — §4A.3, and the reason a compromised
    /// agent cannot send as somebody else.
    pub sender_parameters: &'static [&'static str],
    /// Parameters whose values name recipients, canonicalised on the way through.
    pub recipient_parameters: &'static [&'static str],
}

/// Who the act is from. Resolved by the runtime from the engagement or the
/// program identity — never read from the act's own arguments.
///
/// §4A.3 splits two cases, and the split matters because the first outbound
/// contact is what *creates* an engagement, so there is no engagement to derive
/// a sender from yet:
///
/// - **first contact** → the approved program/company identity;
/// - **existing engagement** → that engagement's channel identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderIdentity {
    /// No engagement yet. Carries the program whose identity is being used, so
    /// the record says which programme spoke.
    FirstContact {
        program_id: String,
        identity: String,
    },
    /// An engagement exists and owns the channel identity.
    Engagement {
        engagement_id: String,
        identity: String,
    },
}

impl SenderIdentity {
    pub fn identity(&self) -> &str {
        match self {
            Self::FirstContact { identity, .. } | Self::Engagement { identity, .. } => identity,
        }
    }

    pub fn engagement_id(&self) -> Option<&str> {
        match self {
            Self::Engagement { engagement_id, .. } => Some(engagement_id),
            Self::FirstContact { .. } => None,
        }
    }
}

/// Why an act could not be reduced to a bindable form.
///
/// Every variant is a refusal, and each names what to do about it. A gate that
/// fails closed without being able to say why gets switched off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestrictionError {
    /// The capability has no restricted form. **Fails closed**: adding a new
    /// outward capability without one must not silently inherit authority.
    NoRestrictedForm { capability: String },
    /// An escape hatch was supplied.
    EscapeHatch { parameter: String },
    /// A parameter outside the closed set.
    UnknownParameter { parameter: String },
    /// Restriction ran and the result still is not bindable. Unreachable unless
    /// the spec and the passthrough table disagree — which is exactly the bug
    /// worth failing loudly on rather than assuming away.
    StillNotBindable { escape_hatches: Vec<String> },
    /// The same parameter twice under different spellings. Refused rather than
    /// resolved, because which value won would depend on hash order.
    DuplicateParameter { parameter: String },
}

impl RestrictionError {
    /// A message for the model that composed the act. Names the closed set, so
    /// the next attempt can succeed rather than repeat.
    pub fn guidance(&self, spec: Option<&RestrictedAction>) -> String {
        let allowed = spec
            .map(|spec| spec.allowed_parameters.join(", "))
            .unwrap_or_else(|| "none".to_string());
        match self {
            Self::NoRestrictedForm { capability } => format!(
                "`{capability}` has no restricted form, so an autonomous agent may not send \
                 through it. Ask the owner to perform this directly."
            ),
            Self::EscapeHatch { parameter } => format!(
                "`{parameter}` forwards raw arguments, so what this action does cannot be \
                 checked before it runs. Supply only: {allowed}."
            ),
            Self::UnknownParameter { parameter } => {
                format!("`{parameter}` is not part of this action. Supply only: {allowed}.")
            },
            Self::DuplicateParameter { parameter } => format!(
                "`{parameter}` was supplied more than once. Supply each of {allowed} at most \
                 once."
            ),
            Self::StillNotBindable { escape_hatches } => format!(
                "this action still accepts {escape_hatches:?} after restriction, which is a \
                 runtime defect rather than something to work around."
            ),
        }
    }
}

/// One dispatch reduced to its bindable form.
///
/// Holding an instance is the proof: `effective.is_bindable()` is true, the
/// sender is bound, and the recipients are canonical. An authority decision
/// taken on this is a decision on the act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundDispatch {
    pub capability: String,
    pub action: String,
    pub params: HashMap<String, Value>,
    pub effective: EffectiveAction,
    pub sender: SenderIdentity,
    /// Stable for identical content, so a retry resumes rather than re-sends.
    pub idempotency_key: String,
}

/// Keys the runtime uses to ROUTE a dispatch rather than to parameterise it.
///
/// `agents::approval::pack_action_for_approval` reads exactly these to decide
/// which action a pack is invoking, so they are present on essentially every
/// real dispatch. They are not caller payload and never reach the provider as
/// content, so they are exempt from the closed-set check and preserved verbatim.
///
/// Without this exemption the allowlist refuses every real outward act with
/// `UnknownParameter { "action" }` — a false refusal that looks exactly like the
/// gate working correctly, which is the worst possible failure mode for a
/// security control.
const DISPATCH_KEYS: &[&str] = &["action", "action_type", "operation", "tool_name"];

fn is_dispatch_key(name: &str) -> bool {
    DISPATCH_KEYS.contains(&name)
}

/// Whether a parameter name is routing metadata rather than caller payload.
///
/// Exposed so a toolset projection narrowing a capability's argument surface
/// keeps exactly the keys [`restrict`] exempts. Two copies of this list would
/// eventually disagree, and the disagreement would look like a tool that cannot
/// select its own action.
pub fn is_dispatch_routing_key(name: &str) -> bool {
    is_dispatch_key(name.trim().to_ascii_lowercase().as_str())
}

/// Reduce a dispatch to its bindable form, or refuse.
pub fn restrict(
    capability: &str,
    action: &str,
    params: &HashMap<String, Value>,
    sender: &SenderIdentity,
) -> Result<BoundDispatch, RestrictionError> {
    let Some(spec) = restricted_form_for(capability) else {
        return Err(RestrictionError::NoRestrictedForm {
            capability: capability.to_string(),
        });
    };

    // §4A.2 — refuse, never strip. An escape hatch that vanished silently gives
    // the agent a send it did not compose.
    let mut bound: BTreeMap<String, Value> = BTreeMap::new();
    for (key, value) in params {
        let name = key.trim().to_ascii_lowercase();
        if super::effective_action::is_passthrough_parameter(&name) {
            return Err(RestrictionError::EscapeHatch { parameter: name });
        }
        // Two keys differing only in case would otherwise collapse into one,
        // and WHICH one survived would depend on HashMap iteration order.
        // Checked before the routing-key branch below: `action` selects which
        // operation actually dispatches, so letting hash order pick between
        // two spellings of it would be worse than for ordinary content.
        if bound.contains_key(&name) {
            return Err(RestrictionError::DuplicateParameter { parameter: name });
        }
        // Routing metadata, not content. Preserved so the bound dispatch stays
        // complete enough to actually run.
        if is_dispatch_key(&name) {
            bound.insert(name, value.clone());
            continue;
        }
        if !spec.allowed_parameters.contains(&name.as_str()) {
            return Err(RestrictionError::UnknownParameter { parameter: name });
        }
        // A sender parameter the caller supplied is dropped here and rebound
        // below, so a caller cannot win by supplying one.
        if spec.sender_parameters.contains(&name.as_str()) {
            continue;
        }
        if spec.recipient_parameters.contains(&name.as_str()) {
            bound.insert(name, canonicalise_recipients(value));
            continue;
        }
        bound.insert(name, value.clone());
    }

    // §4A.3 — bind the sender AFTER the caller's arguments, unconditionally.
    for parameter in spec.sender_parameters {
        bound.insert(
            (*parameter).to_string(),
            Value::String(sender.identity().to_string()),
        );
    }

    let params: HashMap<String, Value> = bound.into_iter().collect();

    // §4A.1 and §4A.4 — the complete effective request, canonicalised.
    let effective = resolve_effective_action(capability, action, &params);
    if !effective.is_bindable() {
        return Err(RestrictionError::StillNotBindable {
            escape_hatches: effective.escape_hatches.clone(),
        });
    }

    // §4A.6 — identical content yields an identical key, so a retry resumes the
    // same outward record rather than sending twice. Derived from what the act
    // DOES, not from when it was attempted: a clock in here would make every
    // retry a new send.
    let idempotency_key = derive_idempotency_key(capability, action, sender, &effective, &params);

    Ok(BoundDispatch {
        capability: capability.to_string(),
        action: action.to_string(),
        params,
        effective,
        sender: sender.clone(),
        idempotency_key,
    })
}

/// The restricted form of a capability, if it has one.
///
/// Returning `None` is a refusal, not a fallback: a capability absent from this
/// table cannot be sent through autonomously. That is deliberate, and it is why
/// this list is shorter than the outward table in
/// `agents::outward_actions` — adding a way to reach the world should require
/// deciding what its bindable form looks like.
/// # A compiled leaf arrives as `pack__action`
///
/// `ExecutableAction::Pack` carries the tool name the model called, and a flat
/// leaf is named `whatsapp__send`. Matching that string against this table found
/// nothing, so `restrict` answered `NoRestrictedForm` and the outward gate
/// **refused every leaf-shaped send** — while `restricted_toolset`, which does
/// split the name, had already offered the same leaf as *restricted*. The
/// catalog said "compose this" and the gate said "no capability by that name",
/// and the refusal read exactly like the gate working.
///
/// Resolved through [`crate::magician_v2::agents::approval::pack_tool_for_approval`]
/// rather than by splitting on `__` here — the same delegation
/// `agents::outward_actions::capability_key` makes, and for the same reason: two
/// functions that must agree about *which capability is this* and do not share
/// code will disagree eventually, and here the disagreement is a gate that
/// refuses a send the catalog invited.
pub fn restricted_form_for(capability: &str) -> Option<&'static RestrictedAction> {
    let capability =
        crate::magician_v2::agents::approval::pack_tool_for_approval(capability.trim())
            .trim()
            .to_ascii_lowercase();
    RESTRICTED_ACTIONS
        .iter()
        .find(|spec| spec.capability == capability)
}

/// Whether an outward capability can be dispatched autonomously at all.
pub fn has_restricted_form(capability: &str) -> bool {
    restricted_form_for(capability).is_some()
}

/// The restricted forms, each checked against its shipped skill by
/// `every_allowed_parameter_exists_in_the_shipped_skill`.
///
/// **Every entry here was wrong when first written.** The originals were
/// composed from what a mail or calendar API plausibly looks like rather than
/// from the skills that actually ship, which invented `thread_id`, `template`
/// and `organizer`, missed `reply_to_message_id` and `labels`, and named `body`
/// where agentmail wants `text`. The effect would have been an allowlist that
/// refuses real sends while claiming to bind a sender it could not reach. The
/// drift test is the fix; this list is only its current answer.
const RESTRICTED_ACTIONS: &[RestrictedAction] = &[
    RestrictedAction {
        capability: "agentmail-send",
        allowed_parameters: &[
            "to",
            "cc",
            "bcc",
            "subject",
            "text",
            "html",
            "labels",
            "message_id",
            "reply_all",
            "inbox_id",
        ],
        // `inbox_id` IS the from-address: *"Optional inbox-id override (the
        // inbox email to send from)"*. Leaving it caller-chosen would let an
        // agent pick which identity the mail appears to come from.
        sender_parameters: &["inbox_id"],
        recipient_parameters: &["to", "cc", "bcc"],
    },
    RestrictedAction {
        capability: "gmail",
        allowed_parameters: &[
            "to",
            "cc",
            "bcc",
            "subject",
            "body",
            "reply_to_message_id",
            "account",
        ],
        // The skill's `profile_parameter` — `account`, one of business /
        // personal / work — decides which mailbox sends. It is the sender, and
        // the earlier spec bound a `from` parameter that does not exist, so the
        // binding was inert and an agent could have chosen its own account.
        sender_parameters: &["account"],
        recipient_parameters: &["to", "cc", "bcc"],
    },
    RestrictedAction {
        capability: "kapso-whatsapp-send",
        allowed_parameters: &["to", "text", "input"],
        // The transport sends from Presto's own configured number; there is no
        // caller-settable sender field, so there is nothing to bind. Inventing
        // one would add a parameter the skill would reject.
        sender_parameters: &[],
        recipient_parameters: &["to"],
    },
    // The three chat channels, each bindable only because its skill grew an
    // explicit `send` action that NAMES its recipient. While their only action
    // was a free-text `run`, no form could exist: one string parameter carrying
    // a whole command line has nothing to allow and no recipient to
    // canonicalise, and a form written over it would have been the `calendar`
    // fiction again.
    //
    // `run` still has no form and still needs none — it is not classified
    // outward, so `restrict` is never asked about it. That is the residual
    // recorded on `agents::outward_actions::OUTWARD_CAPABILITIES`, and it is
    // why these entries make the gated path work without closing the ungated
    // one.
    RestrictedAction {
        capability: "whatsapp",
        allowed_parameters: &["jid", "text", "reply_to"],
        // The session is the sender: `auth.profile_selection: mode: implicit`,
        // one logged-in WhatsApp Web account, no selector reaching the model.
        // Nothing to bind, and inventing a field would add one the skill
        // rejects — the same answer `kapso-whatsapp-send` gets.
        sender_parameters: &[],
        // `jid` is WhatsApp's address for a person or a group. It had to be
        // taught to `effective_action` before it could be named here: a
        // recipient parameter this module canonicalises and that module does
        // not see is the `attendees` bug, and on a `Message` act it is worse
        // than a silent mismatch — an empty recipient list refuses the send.
        recipient_parameters: &["jid"],
    },
    RestrictedAction {
        capability: "telegram",
        allowed_parameters: &["chat_id", "text"],
        // The bot identity comes from `TELEGRAM_TOKEN` in the scoped vault,
        // resolved after authorisation and never copied into arguments. There
        // is no sender to choose and the adapter reads none.
        sender_parameters: &[],
        recipient_parameters: &["chat_id"],
    },
    RestrictedAction {
        capability: "telegram-self",
        allowed_parameters: &["to", "message", "reply_to"],
        // The operator's own MTProto session, again `mode: implicit`. This one
        // speaks AS THE OPERATOR rather than as a bot, which raises what a
        // mistake costs and changes nothing about bindability: there is still
        // no sender field to bind.
        sender_parameters: &[],
        recipient_parameters: &["to"],
    },
    RestrictedAction {
        capability: "presto-gmail",
        // `extra_args` is deliberately NOT here. It is the skill's declared
        // passthrough — *"bounded additional argv, including repeated attachment
        // flags"* — and a restricted form that allowed it would allow a second
        // `--to` after the bound ones, which is the whole shape §4A.1 exists to
        // strip. Dropping it costs attachments on an autonomous send and keeps
        // the act bindable; an owner-approved send still reaches the full skill.
        allowed_parameters: &["to", "cc", "bcc", "subject", "body", "reply_to_message_id"],
        // Nothing to bind, and this is the strongest case of it in the table:
        // the skill is *"hard-pinned to the gws-presto profile; NO account
        // parameter"*, so the sender is fixed before an argument is composed.
        // The earlier `gmail` entry needed `account` precisely because that
        // skill does let the caller choose a mailbox.
        sender_parameters: &[],
        recipient_parameters: &["to", "cc", "bcc"],
    },
];

/// Capabilities whose transport fixes the sender, so there is nothing for
/// §4A.3 to bind.
///
/// Named as a list rather than checked case by case because the exemption is
/// the dangerous one: a spec that simply forgot its binding looks exactly like
/// a spec that has no sender to bind, and the difference is whether an agent
/// can choose whose name a message goes out under. Each entry here is a
/// transport with ONE configured identity and no caller-settable field —
/// Presto's WhatsApp number, one logged-in WhatsApp Web session, one bot token,
/// one MTProto session. Adding a capability here must mean reading its skill
/// and finding no sender parameter, not finding the test inconvenient.
///
/// Test-only, because it is an assertion about the table rather than an input
/// to `restrict` — nothing at runtime consults it, and a runtime that did could
/// only use it to skip a binding.
#[cfg(test)]
const SENDER_IS_THE_TRANSPORT: &[&str] = &[
    "kapso-whatsapp-send",
    "whatsapp",
    "telegram",
    "telegram-self",
    // Hard-pinned to one profile by the skill itself, with no account
    // parameter — the sender is fixed before an argument exists. Contrast
    // `gmail`, which is NOT here because it does let the caller pick a mailbox
    // and must therefore bind one.
    "presto-gmail",
];

// `calendar` and `presto-calendar` deliberately have NO restricted form.
//
// Their `events_insert` action takes a single `args` passthrough and nothing
// else, so an invitation is composed entirely from raw argv. There is no
// parameter to allow, no attendee field to canonicalise and no organiser to
// bind — a "restricted" form would be a fiction that refused every real call.
// Until the skill models its parameters, an autonomous agent cannot send a
// calendar invitation, which `outward_dispatch_class` still classifies and
// captures.
//
// `telegram`, `telegram-self`, `whatsapp` and `presto-gmail` were all absent
// too, and are not any more. What changed for the three chat channels is not
// this table — it is the skills: each grew an explicit `send` action with a
// typed recipient, so there is finally a closed surface to allow and an address
// to canonicalise. `presto-gmail` is the different case: its `send` was ALWAYS
// typed, and it was absent only because nobody had read the skill and written
// the form down. Both orderings matter. A form written ahead of the skill is a
// form composed from what a messaging API plausibly looks like, and every
// original entry here was wrong that way; a form never written for a skill that
// already models its parameters is a capability the gate refuses while
// describing the refusal as a policy.
//
// What their entries do NOT do is close the hole. A send composed inside `run`
// is not classified outward, so `restrict` is never asked about it, and no form
// here refuses what the gate above never sees.

/// Canonicalise a recipient field in place, so the value stored on the dispatch
/// is the value an envelope will be checked against.
///
/// §4A.5 is *"evaluate the envelope AFTER canonicalization, never before"*. The
/// cheapest way to make that hold is to leave no uncanonicalised form around to
/// evaluate.
fn canonicalise_recipients(value: &Value) -> Value {
    match value {
        Value::String(one) => Value::String(canonical_recipient(one)),
        Value::Array(many) => Value::Array(
            many.iter()
                .map(|entry| match entry {
                    Value::String(one) => Value::String(canonical_recipient(one)),
                    other => other.clone(),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The same rule `effective_action` applies, so the two cannot disagree about
/// whether two spellings are one recipient.
fn canonical_recipient(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"').trim();
    if trimmed.contains('@') {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// A key that is stable for identical content and different for anything else.
///
/// Folds in the canonical recipients rather than the raw parameters, so two
/// spellings of one send do not produce two keys — which would defeat the whole
/// point by letting a retry through as a new act.
fn derive_idempotency_key(
    capability: &str,
    action: &str,
    sender: &SenderIdentity,
    effective: &EffectiveAction,
    params: &HashMap<String, Value>,
) -> String {
    // BTreeMap for a deterministic ordering: a HashMap's iteration order would
    // make the key depend on where the process happened to hash its strings.
    let ordered: BTreeMap<&String, &Value> = params.iter().collect();
    let body = serde_json::to_string(&ordered).unwrap_or_default();
    let material = format!(
        "{capability}\u{1f}{action}\u{1f}{}\u{1f}{}\u{1f}{body}",
        sender.identity(),
        effective.recipients.join(","),
    );
    format!("obj-{}", &blake3::hash(material.as_bytes()).to_hex()[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::effective_action::is_passthrough_parameter;
    use serde_json::json;

    fn params(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn sender() -> SenderIdentity {
        SenderIdentity::FirstContact {
            program_id: "prog-1".to_string(),
            identity: "reach.magican@gmail.com".to_string(),
        }
    }

    /// The whole point: the restricted form is bindable by construction, so an
    /// authority decision taken on it is a decision on the act.
    #[test]
    fn a_restricted_dispatch_is_bindable() {
        let bound = restrict(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("Investor@Example.COM")),
                ("subject", json!("hello")),
            ]),
            &sender(),
        )
        .expect("restriction");

        assert!(bound.effective.is_bindable());
        assert!(bound.effective.escape_hatches.is_empty());
        assert_eq!(bound.effective.recipients, vec!["investor@example.com"]);
    }

    /// §4A.2. The hatch is REFUSED, not stripped: an argument that vanished
    /// silently gives the agent a send it did not compose.
    #[test]
    fn an_escape_hatch_is_refused_rather_than_dropped() {
        let error = restrict(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("a@example.com")),
                ("extra_args", json!(["--to", "b@example.com"])),
            ]),
            &sender(),
        )
        .expect_err("must refuse");

        assert_eq!(
            error,
            RestrictionError::EscapeHatch {
                parameter: "extra_args".to_string()
            }
        );
        assert!(error
            .guidance(restricted_form_for("agentmail-send"))
            .contains("Supply only"));
    }

    /// §4A.3. A caller-supplied sender is overwritten, so a compromised agent
    /// cannot send as somebody else.
    #[test]
    fn the_sender_is_bound_and_never_caller_chosen() {
        let bound = restrict(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("a@example.com")),
                ("inbox_id", json!("attacker-inbox@evil.example")),
            ]),
            &sender(),
        )
        .expect("restriction");

        assert_eq!(
            bound.params.get("inbox_id"),
            Some(&json!("reach.magican@gmail.com")),
            "`inbox_id` is the from-address, so the caller's choice must not survive"
        );

        // And the same for gmail, whose sender is the account profile. This one
        // matters: the earlier spec bound a `from` parameter the skill does not
        // have, so the binding was inert and the agent kept its own choice.
        let bound = restrict(
            "gmail",
            "send",
            &params(&[
                ("to", json!("a@example.com")),
                ("subject", json!("hi")),
                ("body", json!("hello")),
                ("account", json!("personal")),
            ]),
            &SenderIdentity::Engagement {
                engagement_id: "eng-1".to_string(),
                identity: "business".to_string(),
            },
        )
        .expect("restriction");
        assert_eq!(
            bound.params.get("account"),
            Some(&json!("business")),
            "an agent must not choose which mailbox it sends from"
        );
    }

    /// A parameter outside the closed set is refused rather than passed through.
    #[test]
    fn an_unknown_parameter_is_refused() {
        let error = restrict(
            "agentmail-send",
            "send",
            &params(&[("to", json!("a@example.com")), ("headers", json!("x: y"))]),
            &sender(),
        )
        .expect_err("must refuse");
        assert_eq!(
            error,
            RestrictionError::UnknownParameter {
                parameter: "headers".to_string()
            }
        );
    }

    /// The regression that made this gate refuse everything.
    ///
    /// `pack_action_for_approval` reads `action` out of the very same parameter
    /// map to decide WHICH action is running, so it is present on essentially
    /// every real dispatch. An allowlist that refused it would have blocked
    /// every live outward act with `UnknownParameter { "action" }` — and the
    /// refusal would have looked exactly like the gate working.
    #[test]
    fn a_realistic_dispatch_carrying_its_routing_key_is_not_refused() {
        for key in ["action", "action_type", "operation", "tool_name"] {
            let bound = restrict(
                "gmail",
                "send",
                &params(&[
                    (key, json!("send")),
                    ("to", json!("a@example.com")),
                    ("subject", json!("hi")),
                ]),
                &sender(),
            )
            .unwrap_or_else(|error| panic!("`{key}` is routing metadata, not payload: {error:?}"));

            assert_eq!(
                bound.params.get(key),
                Some(&json!("send")),
                "`{key}` must be preserved so the bound dispatch stays runnable"
            );
            assert!(bound.effective.is_bindable());
        }
    }

    /// The routing keys the runtime actually reads must be exactly the ones
    /// exempted. If `pack_action_for_approval` learns a fifth, this fails rather
    /// than the gate starting to refuse real sends.
    #[test]
    fn the_exempt_routing_keys_match_what_the_runtime_reads() {
        assert_eq!(
            DISPATCH_KEYS,
            &["action", "action_type", "operation", "tool_name"],
            "keep in step with agents::approval::pack_action_for_approval"
        );
    }

    /// One parameter twice under two spellings is refused rather than resolved:
    /// which value won would otherwise depend on hash order.
    #[test]
    fn a_parameter_supplied_twice_is_refused() {
        let error = restrict(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("a@example.com")),
                ("TO", json!("b@example.com")),
            ]),
            &sender(),
        )
        .expect_err("two spellings of one parameter must not silently pick one");
        assert_eq!(
            error,
            RestrictionError::DuplicateParameter {
                parameter: "to".to_string()
            }
        );
    }

    /// A capability with no restricted form cannot be dispatched autonomously.
    /// Adding a new way to reach the world must not inherit authority by
    /// default.
    #[test]
    fn a_capability_with_no_restricted_form_fails_closed() {
        // `calendar` is in the outward table and deliberately absent here — it
        // composes an invitation entirely from raw argv, so there is nothing to
        // allow. (`telegram` and then `presto-gmail` each stood here until their
        // skills grew a typed, bindable send; the property is about the table,
        // not about any one capability.)
        assert!(
            crate::magician_v2::agents::outward_action_class("calendar", "create_event").is_some()
        );
        assert!(!has_restricted_form("calendar"));

        let error = restrict("calendar", "create_event", &params(&[]), &sender())
            .expect_err("no restricted form means no autonomous send");
        assert_eq!(
            error,
            RestrictionError::NoRestrictedForm {
                capability: "calendar".to_string()
            }
        );
        assert!(error
            .guidance(restricted_form_for("calendar"))
            .contains("may not send through it"));
    }

    /// The three chat channels are bindable, and the values are pinned against
    /// the shipped skills rather than described.
    ///
    /// Each of these was un-bindable until its skill grew a typed `send`, and
    /// the effect of the gap was total: `restrict` answered `NoRestrictedForm`,
    /// so the dispatch gate refused every send, and `restricted_toolset`
    /// withheld the leaf from the catalog entirely. An owner would have met
    /// that as a refusal rather than as a decision.
    #[test]
    fn a_chat_send_reduces_to_a_bindable_act_naming_its_recipient() {
        let cases: &[(&str, &str, Value, &str)] = &[
            (
                "whatsapp",
                "jid",
                json!("919876543210@s.whatsapp.net"),
                "919876543210@s.whatsapp.net",
            ),
            (
                "telegram",
                "chat_id",
                json!("-1001234567890"),
                "-1001234567890",
            ),
            ("telegram-self", "to", json!(" @Ana "), "@ana"),
        ];

        for (capability, recipient_parameter, supplied, canonical) in cases {
            let bound = restrict(
                capability,
                "send",
                &params(&[
                    ("action", json!("send")),
                    (*recipient_parameter, supplied.clone()),
                ]),
                &sender(),
            )
            .unwrap_or_else(|error| panic!("{capability} must be bindable: {error:?}"));

            assert!(bound.effective.is_bindable());
            assert!(bound.effective.escape_hatches.is_empty());
            assert_eq!(
                bound.effective.recipients,
                vec![(*canonical).to_string()],
                "{capability} must name WHO it reaches, or the outward gate refuses it with \
                 `No recipient could be extracted` before a message ever leaves"
            );
        }
    }

    /// A compiled leaf arrives as `pack__action`, and the gate refused every
    /// one of them.
    ///
    /// `ExecutableAction::Pack` carries the tool name the model called, and a
    /// flat leaf is `whatsapp__send`. `restricted_form_for` matched that string
    /// against the table, found nothing, and `restrict` answered
    /// `NoRestrictedForm` — so the outward gate refused every leaf-shaped send,
    /// while `restricted_toolset` (which splits the name) had already offered
    /// the same leaf as *restricted*. The catalog invited the act and the gate
    /// answered "no capability by that name", and the refusal read exactly like
    /// the gate working correctly.
    #[test]
    fn a_compiled_leaf_resolves_to_the_capability_it_names() {
        for (leaf, capability) in [
            ("whatsapp__send", "whatsapp"),
            ("telegram__send", "telegram"),
            // The hyphenated name must not lose its tail to the split.
            ("telegram-self__send", "telegram-self"),
            ("gmail__send", "gmail"),
            ("agentmail-send", "agentmail-send"),
        ] {
            assert_eq!(
                restricted_form_for(leaf).map(|spec| spec.capability),
                Some(capability),
                "`{leaf}` must resolve to `{capability}`, or the gate refuses what the catalog \
                 offered"
            );
        }

        let bound = restrict(
            "whatsapp__send",
            "send",
            &params(&[
                ("jid", json!("919876543210@s.whatsapp.net")),
                ("text", json!("on my way")),
            ]),
            &sender(),
        )
        .expect("a leaf-shaped send must reduce, not refuse");
        assert!(bound.effective.is_bindable());
        assert_eq!(
            bound.effective.recipients,
            vec!["919876543210@s.whatsapp.net".to_string()]
        );

        // Splitting must not invent a form: a capability with none still has
        // none, whichever shape its name arrives in.
        assert!(!has_restricted_form("calendar__create_event"));
        // And splitting must still FIND one that exists — the bug this test was
        // written for was a leaf name matching nothing.
        assert!(has_restricted_form("presto-gmail__send"));
    }

    /// A parameter the shipped `send` action does not model is refused, so the
    /// closed surface is closed in the direction that matters.
    ///
    /// `parse_mode` is the concrete one: the Telegram adapter's send route
    /// refuses it too (`unexpected_send_parameter`), and the two refusals must
    /// agree or the runtime would compose acts the adapter rejects.
    #[test]
    fn a_chat_send_refuses_what_its_skill_does_not_model() {
        let error = restrict(
            "telegram",
            "send",
            &params(&[
                ("chat_id", json!("123")),
                ("text", json!("hi")),
                ("parse_mode", json!("HTML")),
            ]),
            &sender(),
        )
        .expect_err("the send route models chat_id and text, and nothing else");
        assert_eq!(
            error,
            RestrictionError::UnknownParameter {
                parameter: "parse_mode".to_string()
            }
        );

        // And the escape hatch, which is the shape the whole primitive exists
        // for: an agent must not be able to reach the `run` vocabulary by
        // hanging argv off a `send`.
        let hatch = restrict(
            "whatsapp",
            "send",
            &params(&[
                ("jid", json!("919876543210@s.whatsapp.net")),
                ("text", json!("hi")),
                ("extra_args", json!(["--reply-to", "x"])),
            ]),
            &sender(),
        )
        .expect_err("a hatch on a send is refused, never stripped");
        assert_eq!(
            hatch,
            RestrictionError::EscapeHatch {
                parameter: "extra_args".to_string()
            }
        );
    }

    /// A chat channel's transport fixes the sender, so a caller cannot choose
    /// one by supplying a field the skill never declared.
    ///
    /// The failure this rules out is the quiet one: an allowlist that let an
    /// unmodelled `from` through would hand the model a parameter that looks
    /// like an identity choice, and the skill would then reject the call — or
    /// worse, honour it.
    #[test]
    fn a_chat_send_has_no_sender_the_caller_can_choose() {
        for capability in ["whatsapp", "telegram", "telegram-self"] {
            let spec = restricted_form_for(capability).expect("a form");
            assert!(
                spec.sender_parameters.is_empty(),
                "{capability} claims a sender parameter its skill does not declare"
            );
            assert!(
                SENDER_IS_THE_TRANSPORT.contains(&capability),
                "{capability} must be recorded as transport-bound, not silently exempt"
            );
            for invented in ["from", "account", "inbox_id", "sender"] {
                assert!(
                    !spec.allowed_parameters.contains(&invented),
                    "{capability} allows `{invented}`, which reads as choosing an identity"
                );
            }
        }
    }

    /// §4A.6. Identical content yields an identical key, so a retry resumes the
    /// same outward record rather than sending twice.
    #[test]
    fn identical_content_yields_one_idempotency_key() {
        let first = restrict(
            "agentmail-send",
            "send",
            &params(&[("to", json!("a@example.com")), ("subject", json!("hi"))]),
            &sender(),
        )
        .expect("first");
        // The same send, spelled differently. Canonicalisation must collapse it.
        let retry = restrict(
            "agentmail-send",
            "send",
            &params(&[("to", json!("  A@Example.com ")), ("subject", json!("hi"))]),
            &sender(),
        )
        .expect("retry");

        assert_eq!(
            first.idempotency_key, retry.idempotency_key,
            "two spellings of one send must not read as two sends"
        );

        let different = restrict(
            "agentmail-send",
            "send",
            &params(&[("to", json!("a@example.com")), ("subject", json!("other"))]),
            &sender(),
        )
        .expect("different");
        assert_ne!(first.idempotency_key, different.idempotency_key);
    }

    /// A different sender is a different act, even to the same recipient with
    /// the same words.
    #[test]
    fn a_different_sender_is_a_different_act() {
        let request = params(&[("to", json!("a@example.com")), ("subject", json!("hi"))]);
        let first = restrict("agentmail-send", "send", &request, &sender()).expect("first");
        let other = restrict(
            "agentmail-send",
            "send",
            &request,
            &SenderIdentity::Engagement {
                engagement_id: "eng-1".to_string(),
                identity: "programme@magican.ai".to_string(),
            },
        )
        .expect("other sender");
        assert_ne!(first.idempotency_key, other.idempotency_key);
    }

    /// Recipients are canonicalised on the dispatch itself, so there is no
    /// uncanonicalised form left for an envelope to be evaluated against — §4A.5
    /// by construction rather than by ordering discipline.
    #[test]
    fn recipients_are_canonical_on_the_bound_dispatch() {
        let bound = restrict(
            "agentmail-send",
            "send",
            &params(&[("to", json!([" A@Example.COM ", "b@example.com"]))]),
            &sender(),
        )
        .expect("restriction");

        assert_eq!(
            bound.params.get("to"),
            Some(&json!(["a@example.com", "b@example.com"]))
        );
    }

    /// Every restricted form must name a sender parameter, or it cannot honour
    /// §4A.3 — except where the transport has no caller-settable sender at all.
    /// Pinned so a new spec cannot quietly omit the binding.
    #[test]
    fn every_restricted_form_binds_a_sender_or_has_none_to_bind() {
        for spec in RESTRICTED_ACTIONS {
            if SENDER_IS_THE_TRANSPORT.contains(&spec.capability) {
                // The transport fixes the identity — Presto's configured
                // number, one logged-in WhatsApp Web session, one bot token,
                // one MTProto session. The skill exposes no sender field, so
                // there is nothing to bind and inventing one would add a
                // parameter the skill rejects.
                assert!(
                    spec.sender_parameters.is_empty(),
                    "{} names a sender parameter while claiming its transport fixes the sender",
                    spec.capability
                );
                continue;
            }
            assert!(
                !spec.sender_parameters.is_empty(),
                "{} must bind a sender",
                spec.capability
            );
            for parameter in spec.sender_parameters {
                assert!(
                    spec.allowed_parameters.contains(parameter),
                    "{}: sender parameter `{parameter}` is not in the allowed set, so it would \
                     be refused before it could be bound",
                    spec.capability
                );
            }
        }
    }

    /// The systemic fix for the drift that made every original spec wrong.
    ///
    /// A hand-written allowlist mirroring a schema that lives in a file will
    /// drift, and the drift is invisible: an invented parameter refuses a real
    /// send, and a missing one refuses it too — both looking like the gate doing
    /// its job. So the shipped skill is the source of truth and this asserts
    /// against it.
    ///
    /// It would have caught all of: `thread_id`, `message_id` and `from` on
    /// gmail (invented), `body` on agentmail (it wants `text`), `template` and
    /// `template_variables` on kapso (invented), and the entire `calendar` spec
    /// (its `events_insert` takes one `args` passthrough and nothing else).
    #[test]
    fn every_allowed_parameter_exists_in_the_shipped_skill() {
        let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("magician crate lives under the workspace root")
            .join("skillshub");

        for spec in RESTRICTED_ACTIONS {
            let path = skills.join(spec.capability).join("SKILL.md");
            let shipped = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));

            for parameter in spec.allowed_parameters {
                assert!(
                    declares_parameter(&shipped, parameter),
                    "{}: `{parameter}` is in the allowlist but the shipped skill never declares \
                     it, so allowing it is meaningless and anything relying on it is fiction",
                    spec.capability
                );
            }

            // A hatch must never be allowed through the closed set — that would
            // undo the entire point of restriction.
            for parameter in spec.allowed_parameters {
                assert!(
                    !is_passthrough_parameter(parameter),
                    "{}: `{parameter}` forwards raw argv and must never be allowed",
                    spec.capability
                );
            }

            // A sender we cannot actually set is a binding that does nothing.
            for parameter in spec.sender_parameters {
                assert!(
                    declares_parameter(&shipped, parameter),
                    "{}: sender `{parameter}` is not a parameter of the shipped skill, so \
                     binding it would be inert and the agent would keep its own identity",
                    spec.capability
                );
            }
        }
    }

    /// Whether a skill file actually DECLARES a parameter, as opposed to merely
    /// containing the word somewhere.
    ///
    /// A plain substring search is not enough, and this is not hypothetical:
    /// `gmail`'s SKILL.md contains the sentence *"To set up a new account:"*, so
    /// searching for `account:` matched English prose. The drift test passed for
    /// the wrong reason and would have kept passing if the parameter were
    /// deleted — the exact vacuity it exists to prevent.
    ///
    /// Two real declaration forms, both matched on the TRIMMED line so nesting
    /// depth does not matter:
    ///
    /// - an action parameter, a YAML key: `to:`
    /// - a skill-level profile parameter: `name: account`
    fn declares_parameter(shipped: &str, parameter: &str) -> bool {
        let as_key = format!("{parameter}:");
        let as_profile = format!("name: {parameter}");
        shipped.lines().map(str::trim).any(|line| {
            line == as_key || line == as_profile || line.starts_with(&format!("{as_key} "))
        })
    }

    /// The matcher must reject prose and accept both declaration forms, or the
    /// drift test it powers is decorative.
    #[test]
    fn the_declaration_matcher_rejects_prose() {
        let shipped = "\
            To set up a new account:\n\
            an account: is discussed here\n\
                to:\n\
                reply_to_message_id:\n\
                    name: account\n\
                max_length: 4096\n";

        assert!(declares_parameter(shipped, "to"));
        assert!(declares_parameter(shipped, "reply_to_message_id"));
        assert!(
            declares_parameter(shipped, "account"),
            "a profile parameter is a real declaration"
        );
        assert!(
            declares_parameter(shipped, "max_length"),
            "a key with an inline value still declares it"
        );
        assert!(
            !declares_parameter(shipped, "subject"),
            "a parameter nobody declared must not match"
        );
        assert!(
            !declares_parameter(shipped, "new"),
            "`To set up a new account:` must not read as declaring `new`"
        );
    }

    /// A capability whose only interface is a raw argv passthrough cannot have a
    /// bindable form, so it must have none rather than a fictional one.
    #[test]
    fn a_purely_passthrough_capability_has_no_restricted_form() {
        for capability in ["calendar", "presto-calendar"] {
            assert!(
                !has_restricted_form(capability),
                "{capability} composes an invitation entirely from raw argv, so a restricted \
                 form would refuse every real call while claiming to bind an organiser"
            );
        }
    }

    /// Every recipient parameter must be one the effective-action resolver also
    /// recognises. If they disagree, a recipient canonicalised here would not be
    /// the one an envelope is checked against.
    #[test]
    fn recipient_parameters_agree_with_the_effective_action_resolver() {
        for spec in RESTRICTED_ACTIONS {
            for parameter in spec.recipient_parameters {
                assert!(
                    spec.allowed_parameters.contains(parameter),
                    "{}: recipient parameter `{parameter}` is not allowed",
                    spec.capability
                );
                let resolved = resolve_effective_action(
                    spec.capability,
                    "send",
                    &params(&[(parameter, json!("someone@example.com"))]),
                );
                assert_eq!(
                    resolved.recipients,
                    vec!["someone@example.com".to_string()],
                    "{}: `{parameter}` names a recipient here but not in effective_action, so a \
                     boundary predicate would be checked against the wrong set",
                    spec.capability
                );
            }
        }
    }
}
