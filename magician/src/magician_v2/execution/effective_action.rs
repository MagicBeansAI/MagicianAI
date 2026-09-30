//! What a dispatch will ACTUALLY do, as opposed to what its typed arguments say.
//!
//! A generic primitive, not a feature of any one flow. **Any** gate that decides
//! from a dispatch's arguments — approval, spend ceilings, capture, audit,
//! authority envelopes — is unsound if the arguments it inspected can be widened
//! by arguments it did not.
//!
//! # The concrete hole this closes
//!
//! Skill command templates support a `passthrough` mapping: a parameter whose
//! string array is appended to `argv` verbatim. The shipped `agentmail-send`
//! skill documents the consequence outright —
//!
//! > `to` — "Primary recipient. **For multiple recipients pass extra `--to`
//! > pairs via `extra_args`.**"
//!
//! So a gate reading the typed `to` field sees one recipient while five receive
//! the mail. `kapso-whatsapp-send` exposes the same hatch. Nothing about that is
//! specific to email or to outward work: it is a general property of any
//! template that forwards raw tokens.
//!
//! # Two questions, deliberately separate
//!
//! - **What does this act actually touch?** [`EffectiveAction::recipients`]
//!   merges typed fields with values expanded out of passthroughs, so a record
//!   or a log describes the real act.
//! - **May a decision be BOUND to this?** [`EffectiveAction::is_bindable`] is
//!   false whenever an escape hatch is present at all. Expanding what we
//!   recognise makes a record more truthful; it does not make an open-ended
//!   token list safe to authorise against, because the next token we do not
//!   model changes the act again.
//!
//! A gate that only needs to describe (audit, capture telemetry) uses the first.
//! A gate that grants authority uses the second and refuses when it is false.

use std::collections::HashMap;

use serde_json::Value;

/// Parameter names that forward raw tokens to the underlying command.
///
/// `args` is the CLI template's own passthrough key; the others are the
/// conventional spellings a skill author reaches for. Matching by name rather
/// than by template introspection keeps this usable anywhere a parameter map is
/// available, including before a template has been resolved.
const PASSTHROUGH_PARAMETERS: &[&str] =
    &["args", "extra_args", "extra_arguments", "raw_args", "argv"];

/// Typed parameters that name someone the act reaches.
const RECIPIENT_PARAMETERS: &[&str] = &[
    "to",
    "cc",
    "bcc",
    "recipient",
    "recipients",
    "email",
    "phone",
    "number",
    "chat_id",
    "audience",
    // A calendar invitation reaches its attendees. Omitting this made the
    // restricted `calendar` form canonicalise a field the resolver ignored, so
    // a boundary predicate would have been checked against an empty set.
    "attendees",
    // WhatsApp addresses a person by JID, and `skillshub/whatsapp/SKILL.md`
    // declares the `send` action's `jid` as *"Recipient chat JID — WHO this act
    // reaches"*. Without it here the resolver returns NO recipients for a
    // whatsapp send, and an empty list on a `Message` act is not a quiet
    // degradation: `outward_gate::Addressing::for_class` reads `Message` as
    // `MustNameRecipients`, so the send is refused outright with *"No recipient
    // could be extracted"* — the gated path failing while the ungated `run`
    // path still works, which is the one pressure that would push a model back
    // onto `run`. The `attendees` lesson, arriving through a different door.
    "jid",
];

/// Command flags that name someone the act reaches.
const RECIPIENT_FLAGS: &[&str] = &[
    "--to",
    "--cc",
    "--bcc",
    "--recipient",
    "--email",
    "--phone",
    "--number",
    "--to-number",
    "--chat-id",
];

/// Whether a parameter name forwards raw tokens to the underlying command.
///
/// Exposed so [`super::restricted_action`] refuses exactly what this module
/// counts as a hatch. Two copies of this list would eventually disagree, and the
/// disagreement would look like a restricted action that is not bindable.
pub fn is_passthrough_parameter(name: &str) -> bool {
    PASSTHROUGH_PARAMETERS.contains(&name.trim().to_ascii_lowercase().as_str())
}

/// The canonical effective form of one dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAction {
    pub capability: String,
    pub action: String,
    /// Everyone this act reaches, canonicalised and deduplicated — typed fields
    /// merged with values recovered from passthrough tokens.
    pub recipients: Vec<String>,
    /// Passthrough parameters present on this dispatch, by name. Non-empty means
    /// the argument list is open-ended.
    pub escape_hatches: Vec<String>,
    /// Passthrough tokens not recognised as a recipient. Kept for diagnostics:
    /// they are why the act is not bindable, and naming them makes the refusal
    /// explainable rather than mysterious.
    pub unmodelled_tokens: Vec<String>,
}

impl EffectiveAction {
    /// Whether a decision may be **bound** to this act.
    ///
    /// False whenever any escape hatch is present. Expanding the tokens we
    /// recognise makes a *description* truthful; it cannot make an open-ended
    /// list safe to authorise against, because the property that matters is that
    /// nothing outside what was inspected can still change the act.
    ///
    /// The intended fix for a false result is not better parsing. It is a
    /// restricted form of the action that has no escape hatch — then this is
    /// true by construction.
    pub fn is_bindable(&self) -> bool {
        self.escape_hatches.is_empty()
    }
}

/// Resolve the effective action for a capability dispatch.
///
/// Pure and allocation-light: it reads a parameter map and nothing else, so it
/// can run inside a dispatch gate without I/O or ordering concerns.
pub fn resolve_effective_action(
    capability: &str,
    action: &str,
    params: &HashMap<String, Value>,
) -> EffectiveAction {
    let mut recipients: Vec<String> = Vec::new();
    let mut escape_hatches: Vec<String> = Vec::new();
    let mut unmodelled_tokens: Vec<String> = Vec::new();

    for (key, value) in params {
        let key_lower = key.trim().to_ascii_lowercase();

        if is_passthrough_parameter(&key_lower) {
            let tokens = string_tokens(value);
            // An empty passthrough still counts: the parameter is accepted, so
            // the NEXT dispatch can fill it. Bindability is a property of the
            // action's shape, not of one call's arguments.
            escape_hatches.push(key_lower.clone());
            expand_tokens(&tokens, &mut recipients, &mut unmodelled_tokens);
            continue;
        }

        if RECIPIENT_PARAMETERS.contains(&key_lower.as_str()) {
            for entry in string_tokens(value) {
                push_recipient(&mut recipients, &entry);
            }
        }
    }

    escape_hatches.sort();
    escape_hatches.dedup();
    recipients.sort();
    recipients.dedup();
    // `params` is a HashMap, so with TWO passthrough parameters the tokens would
    // otherwise arrive in whatever order the process happened to hash their
    // names — making `EffectiveAction` inequal to itself across runs, and any
    // assertion on it flaky rather than wrong.
    unmodelled_tokens.sort();
    unmodelled_tokens.dedup();

    EffectiveAction {
        capability: capability.trim().to_string(),
        action: action.trim().to_string(),
        recipients,
        escape_hatches,
        unmodelled_tokens,
    }
}

/// Pull recipients out of raw command tokens, in both `--flag value` and
/// `--flag=value` spellings.
fn expand_tokens(tokens: &[String], recipients: &mut Vec<String>, unmodelled: &mut Vec<String>) {
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].trim();
        let (flag, inline_value) = match token.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (token, None),
        };
        if RECIPIENT_FLAGS.contains(&flag.to_ascii_lowercase().as_str()) {
            match inline_value {
                Some(value) => {
                    push_recipient(recipients, value);
                    index += 1;
                },
                None => {
                    // A flag's value must not itself be another flag. `--to --cc`
                    // is a malformed pair, and consuming `--cc` as a recipient
                    // would record a literal `--cc` as somebody we wrote to.
                    match tokens.get(index + 1) {
                        Some(value) if !value.trim_start().starts_with('-') => {
                            push_recipient(recipients, value);
                            index += 2;
                        },
                        _ => {
                            unmodelled.push(token.to_string());
                            index += 1;
                        },
                    }
                },
            }
            continue;
        }
        if !token.is_empty() {
            unmodelled.push(token.to_string());
        }
        index += 1;
    }
}

/// Canonicalise before recording. Two spellings of one recipient would defeat a
/// per-recipient cap and would split a reverse lookup in two.
fn push_recipient(recipients: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return;
    }
    // Addresses are case-insensitive in the part that matters; phone numbers and
    // ids are left alone beyond trimming, because lowercasing an opaque id could
    // change it.
    let canonical = if trimmed.contains('@') {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    };
    recipients.push(canonical);
}

/// A JSON value as a list of strings, whether it was one or many.
fn string_tokens(value: &Value) -> Vec<String> {
    match value {
        Value::String(one) => vec![one.clone()],
        Value::Array(many) => many
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    /// The documented hole, as behaviour: a gate reading the typed `to` sees one
    /// recipient while four more arrive through the escape hatch.
    #[test]
    fn recipients_hidden_in_a_passthrough_are_recovered() {
        let effective = resolve_effective_action(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("first@example.com")),
                (
                    "extra_args",
                    json!([
                        "--to",
                        "second@example.com",
                        "--to=third@example.com",
                        "--cc",
                        "fourth@example.com"
                    ]),
                ),
            ]),
        );

        assert_eq!(
            effective.recipients,
            vec![
                "first@example.com".to_string(),
                "fourth@example.com".to_string(),
                "second@example.com".to_string(),
                "third@example.com".to_string(),
            ],
            "a record built from the typed field alone would name one of four"
        );
    }

    /// Expanding what we recognise does NOT make the act bindable. The property
    /// a gate needs is that nothing outside what it inspected can change the
    /// act, and an open-ended token list cannot offer that.
    #[test]
    fn an_escape_hatch_makes_the_act_unbindable_even_when_fully_expanded() {
        let effective = resolve_effective_action(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("first@example.com")),
                ("extra_args", json!(["--to", "second@example.com"])),
            ]),
        );

        assert_eq!(effective.recipients.len(), 2, "both are recovered");
        assert!(
            !effective.is_bindable(),
            "every token was recognised, and the act is still not bindable: the \
             next token we do not model would change it again"
        );
        assert_eq!(effective.escape_hatches, vec!["extra_args".to_string()]);
    }

    /// An empty or absent-valued hatch still counts. Bindability is a property
    /// of the action's SHAPE — the parameter is accepted, so the next call can
    /// fill it — not of one call's arguments happening to be tame.
    #[test]
    fn an_empty_escape_hatch_still_blocks_binding() {
        for value in [json!([]), json!(null), json!("")] {
            let effective = resolve_effective_action(
                "agentmail-send",
                "send",
                &params(&[
                    ("to", json!("a@example.com")),
                    ("extra_args", value.clone()),
                ]),
            );
            assert!(
                !effective.is_bindable(),
                "an accepted-but-empty hatch ({value}) must not read as bounded"
            );
        }
    }

    /// With no hatch, the act is bindable — which is what a restricted form of
    /// an action buys, and the reason the fix is to remove the hatch rather than
    /// to parse it better.
    #[test]
    fn an_action_with_no_escape_hatch_is_bindable() {
        let effective = resolve_effective_action(
            "restricted-mail-send",
            "send",
            &params(&[
                ("to", json!(["a@example.com", "b@example.com"])),
                ("subject", json!("hello")),
            ]),
        );

        assert!(effective.is_bindable());
        assert!(effective.unmodelled_tokens.is_empty());
        assert_eq!(effective.recipients.len(), 2);
    }

    /// Tokens we cannot classify are reported rather than ignored, so a refusal
    /// can say what caused it. A gate that fails closed without being able to
    /// explain why gets switched off.
    #[test]
    fn unmodelled_tokens_are_named_not_swallowed() {
        let effective = resolve_effective_action(
            "kapso-whatsapp-send",
            "send",
            &params(&[(
                "extra_args",
                json!(["--template", "promo_v2", "--force", "--to", "+911234567890"]),
            )]),
        );

        assert_eq!(effective.recipients, vec!["+911234567890".to_string()]);
        assert!(effective
            .unmodelled_tokens
            .contains(&"--template".to_string()));
        assert!(effective.unmodelled_tokens.contains(&"--force".to_string()));
        assert!(!effective.is_bindable());
    }

    /// Canonicalisation, because two spellings of one recipient would defeat a
    /// per-recipient cap and split a reverse lookup in two.
    #[test]
    fn one_recipient_written_two_ways_is_one_recipient() {
        let effective = resolve_effective_action(
            "agentmail-send",
            "send",
            &params(&[
                ("to", json!("  Investor@Example.COM ")),
                ("extra_args", json!(["--to", "investor@example.com"])),
            ]),
        );

        assert_eq!(
            effective.recipients,
            vec!["investor@example.com".to_string()],
            "case and whitespace must not create a second recipient"
        );
    }

    /// An opaque id is not an address and must not be lowercased — doing so
    /// could change it.
    #[test]
    fn an_opaque_recipient_id_is_left_alone() {
        let effective = resolve_effective_action(
            "telegram",
            "send",
            &params(&[("chat_id", json!("AbC123XyZ"))]),
        );
        assert_eq!(effective.recipients, vec!["AbC123XyZ".to_string()]);
    }

    /// A flag whose value is another flag is malformed, and consuming it would
    /// record a literal `--cc` as somebody we wrote to.
    #[test]
    fn a_flag_is_never_consumed_as_the_previous_flags_value() {
        let effective = resolve_effective_action(
            "agentmail-send",
            "send",
            &params(&[("extra_args", json!(["--to", "--cc", "real@example.com"]))]),
        );
        assert_eq!(
            effective.recipients,
            vec!["real@example.com".to_string()],
            "`--cc` is a flag, not a person"
        );
        assert!(effective.unmodelled_tokens.contains(&"--to".to_string()));
    }

    /// A trailing flag with nothing after it must not silently vanish.
    #[test]
    fn a_trailing_flag_with_no_value_is_reported() {
        let effective = resolve_effective_action(
            "agentmail-send",
            "send",
            &params(&[("extra_args", json!(["--to"]))]),
        );
        assert!(effective.recipients.is_empty());
        assert_eq!(effective.unmodelled_tokens, vec!["--to".to_string()]);
    }

    /// `params` is a HashMap, so two passthrough parameters would otherwise
    /// yield tokens in whatever order the process hashed their names — making
    /// the struct unequal to itself across runs.
    #[test]
    fn the_resolved_action_is_identical_across_runs() {
        let build = || {
            resolve_effective_action(
                "agentmail-send",
                "send",
                &params(&[
                    ("extra_args", json!(["--template", "a"])),
                    ("raw_args", json!(["--force", "--verbose"])),
                    ("to", json!("a@example.com")),
                ]),
            )
        };
        let first = build();
        for _ in 0..16 {
            assert_eq!(build(), first, "resolution must not depend on hash order");
        }
    }

    /// A WhatsApp send names its recipient in `jid`, and the resolver must see
    /// it.
    ///
    /// The shipped `whatsapp` `send` action declares `jid` as *"Recipient chat
    /// JID — WHO this act reaches"*. Before `jid` was listed the resolver
    /// answered an empty recipient list for every one of those sends, and an
    /// empty list on a `Message` act is a refusal rather than a shrug —
    /// `Addressing::for_class(Message)` is `MustNameRecipients`. The gated
    /// `send` would have been refused while the ungated `run` still worked.
    #[test]
    fn a_whatsapp_send_names_the_person_it_reaches() {
        let effective = resolve_effective_action(
            "whatsapp",
            "send",
            &params(&[
                ("jid", json!("919876543210@s.whatsapp.net")),
                ("text", json!("on my way")),
            ]),
        );
        assert_eq!(
            effective.recipients,
            vec!["919876543210@s.whatsapp.net".to_string()],
            "a send whose recipient cannot be named is refused by the outward gate"
        );
        assert!(effective.is_bindable());

        // A group JID is a recipient too — the act reaches everyone in it, and
        // the screen must see the chat it was addressed to.
        let group = resolve_effective_action(
            "whatsapp",
            "send",
            &params(&[
                ("jid", json!("120363021234567890@g.us")),
                ("text", json!("hi")),
            ]),
        );
        assert_eq!(
            group.recipients,
            vec!["120363021234567890@g.us".to_string()]
        );
    }

    /// A dispatch that names nobody resolves to no recipients rather than
    /// failing — this primitive describes an act, it does not judge it.
    #[test]
    fn an_act_with_no_recipients_is_not_an_error() {
        let effective = resolve_effective_action(
            "websearch",
            "search",
            &params(&[("query", json!("who funds seed rounds"))]),
        );
        assert!(effective.recipients.is_empty());
        assert!(effective.is_bindable());
    }
}
