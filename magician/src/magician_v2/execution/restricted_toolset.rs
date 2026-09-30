//! The shape an outward capability may take in the toolset an autonomous run
//! sees — §4A, applied one step earlier than the dispatch gate.
//!
//! [`super::restricted_action`] already decides what a bindable act looks like,
//! and `execute_action_inner` refuses anything that cannot be reduced to one.
//! That refusal is correct and stays. It is also the *second-best* place to say
//! it: by then the model has already composed the act, spent an iteration on it,
//! and learned only that something it was offered does not work. The tool
//! catalog is the first place, and the only one where the answer is structural —
//! an argument the schema never mentions is an argument the model cannot send.
//!
//! So this projects each capability tool into the form it is actually allowed to
//! take:
//!
//! - **as declared** — nothing about this tool reaches the world;
//! - **restricted** — it reaches the world and has a bindable form, so the
//!   closed argument surface is substituted for the declared one;
//! - **withheld** — it reaches the world, has no bindable form, and every
//!   dispatch of it would be refused. Offering it is the wasted iteration.
//!
//! # This is a narrowing, never a widening
//!
//! Every tool this module withholds is one the dispatch gate already refuses,
//! and every parameter it removes is one `restrict` already refuses. Nothing
//! here can make an act reachable that was not reachable before, which is the
//! property that lets it run ahead of the gate without becoming a second,
//! disagreeing authority. The gate remains the decision; this is the offer.
//!
//! # Why the passthrough is judged from the SHAPE, not from one call
//!
//! `outward_dispatch_class` asks whether a *dispatch* carries a passthrough,
//! because at dispatch there are arguments to read. Here there are none — only
//! the declared surface. So the test is the one
//! [`super::effective_action::EffectiveAction::is_bindable`] uses: a tool that
//! **accepts** a passthrough is a tool whose next call can fill it. Judging by
//! arguments would be judging a call that has not happened yet.

use serde_json::{Map, Value};

use super::restricted_action::{is_dispatch_routing_key, restricted_form_for, RestrictedAction};
use crate::magician_v2::agents::outward_actions::{
    capability_can_reach_the_world, capability_only_sends, outward_action_class,
    outward_passthrough_class,
};

/// The separator a flat tool leaf joins its capability and action with
/// (`gmail__send`).
const LEAF_SEP: &str = "__";

/// How one capability tool must appear in an autonomous toolset.
///
/// No `Eq`: a JSON Schema is a `serde_json::Value`, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolProjection {
    /// Project it verbatim.
    AsDeclared,
    /// Project it with this argument surface instead, and this note appended to
    /// its description so the model is told what changed rather than left to
    /// infer it from a missing field.
    Restricted { parameters: Value, note: String },
    /// Do not project it at all — not into the callable tier, and not into the
    /// deferred names either. A withheld tool that `tool_search` can still find
    /// is a withheld tool the model will still reach for.
    Withheld { reason: String },
}

/// Split a tool name into the capability it belongs to and the action it names.
///
/// `gmail__send` is the flat leaf shape; a bare `gmail` is the umbrella shape,
/// where every action hides behind an `action` parameter and the name alone says
/// nothing about which one will run.
///
/// Splits on the FIRST separator, so a capability whose own name contains one
/// keeps its action rather than losing it to the capability half.
pub fn split_capability_leaf(tool_name: &str) -> (&str, Option<&str>) {
    match tool_name.split_once(LEAF_SEP) {
        Some((capability, action)) => (capability, Some(action)),
        None => (tool_name, None),
    }
}

/// Decide how one capability tool may appear.
///
/// `declared_parameters` is the JSON Schema the pack declares. Anything that is
/// not an object with `properties` carries no argument surface to narrow, so it
/// is projected as declared — narrowing a schema this module cannot read would
/// be inventing one.
pub fn project_capability_tool(tool_name: &str, declared_parameters: &Value) -> ToolProjection {
    let (capability, action) = split_capability_leaf(tool_name);
    if !capability_can_reach_the_world(capability) {
        return ToolProjection::AsDeclared;
    }

    let Some(properties) = declared_properties(declared_parameters) else {
        return ToolProjection::AsDeclared;
    };
    let required = declared_required(declared_parameters);
    let passthrough_is_outward = outward_passthrough_class(capability).is_some();
    let passthroughs: Vec<String> = properties
        .keys()
        // Match dispatch's distinction between an opaque outward command and
        // an ordinary argument on an action-classified capability.
        .filter(|_| passthrough_is_outward)
        .filter(|name| super::effective_action::is_passthrough_parameter(name.as_str()))
        .cloned()
        .collect();

    // Does THIS leaf send, in the sense that the send allowlist is the right
    // surface for it?
    //
    // A leaf that merely ACCEPTS a passthrough is not answered here. It can send
    // — `gmail action=raw args=["+send", …]` is a send whose token says nothing
    // — but the allowlist names what may be SENT, and imposing it on
    // `gmail__triage` would leave a read with no way to say what to read. Those
    // leaves lose the passthrough and keep the rest; see `close_passthrough`.
    //
    // The umbrella shape (no action in the name) is deliberately not a send for
    // the same reason: one tool stands for every action the capability has,
    // reads included.
    let sends = match action {
        Some(action) => outward_action_class(capability, action).is_some(),
        // The umbrella of a mixed-mode capability is not a send: it stands for
        // the reads too. The umbrella of a send-only one is, because the bare
        // invocation IS the send and its name never carries a verb to check.
        None => capability_only_sends(capability),
    };

    let Some(spec) = restricted_form_for(capability) else {
        // The capability can reach the world and nobody has decided what its
        // bindable form is. `restrict` answers `NoRestrictedForm` for every
        // dispatch of it, so a sending leaf — and any leaf that accepts a
        // passthrough — is refused whatever the model composes.
        if sends {
            return ToolProjection::Withheld {
                reason: format!(
                    "`{capability}` reaches the world and has no bindable form, so an autonomous \
                     run may not send through it"
                ),
            };
        }
        if passthroughs.is_empty() {
            return ToolProjection::AsDeclared;
        }
        return close_passthrough(
            tool_name,
            capability,
            declared_parameters,
            properties,
            &required,
            &passthroughs,
        );
    };

    if sends {
        return close_to_allowlist(capability, spec, declared_parameters, properties, &required);
    }
    if passthroughs.is_empty() {
        return ToolProjection::AsDeclared;
    }
    close_passthrough(
        tool_name,
        capability,
        declared_parameters,
        properties,
        &required,
        &passthroughs,
    )
}

/// A leaf that does not send but accepts a passthrough: remove the passthrough
/// and leave everything else alone.
///
/// The send allowlist is not applied here, and that is the whole point of the
/// split. `gmail__triage` accepts `extra_args` alongside `query`, `max` and
/// `label`; those three are not in the send allowlist because they are not
/// things you send, and intersecting with it would leave a read tool with no way
/// to say what to read.
///
/// A leaf whose passthrough is **required** cannot be composed without it, so
/// every call of it is refused and the leaf is withheld instead. That is every
/// action of the argv-only skills: their one parameter is the escape hatch.
fn close_passthrough(
    tool_name: &str,
    capability: &str,
    declared: &Value,
    properties: &Map<String, Value>,
    required: &[String],
    passthroughs: &[String],
) -> ToolProjection {
    if let Some(needed) = passthroughs
        .iter()
        .find(|name| required.iter().any(|req| req == *name))
    {
        return ToolProjection::Withheld {
            reason: format!(
                "`{tool_name}` requires `{needed}`, which forwards raw arguments — so what it does \
                 cannot be checked before it runs, and `{capability}` can reach the world"
            ),
        };
    }

    let kept: Map<String, Value> = properties
        .iter()
        .filter(|(name, _)| !passthroughs.iter().any(|hatch| hatch == *name))
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect();
    let kept_required: Vec<Value> = required
        .iter()
        .filter(|name| kept.contains_key(*name))
        .map(|name| Value::String(name.clone()))
        .collect();

    ToolProjection::Restricted {
        parameters: narrowed_schema(declared, kept, kept_required),
        note: format!(
            "Autonomous runs cannot forward raw arguments through a capability that reaches \
             people, so {} is not available here.",
            passthroughs
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
    }
}

/// A sending leaf: substitute the capability's closed argument surface.
///
/// Three removals, each mirroring one `restrict` already performs, so the schema
/// describes exactly the acts that will be accepted:
///
/// - anything outside `allowed_parameters` — `restrict` answers
///   `UnknownParameter`;
/// - every passthrough — `restrict` answers `EscapeHatch`;
/// - the sender parameters — `restrict` drops whatever the caller supplied and
///   rebinds the runtime's identity, so offering the field teaches the model to
///   compose a choice that is silently overwritten.
///
/// Routing keys (`action` and its spellings) survive: they select which
/// operation runs rather than parameterising it, and a leaf that lost its own
/// action selector could not be dispatched at all.
///
/// A leaf that **requires** a parameter the closed surface has no room for
/// cannot be composed in an acceptable form at all, so it is withheld rather
/// than offered in a shape that is refused on every call.
fn close_to_allowlist(
    capability: &str,
    spec: &RestrictedAction,
    declared: &Value,
    properties: &Map<String, Value>,
    required: &[String],
) -> ToolProjection {
    let admissible = |name: &str| {
        let lower = name.trim().to_ascii_lowercase();
        if is_dispatch_routing_key(&lower) {
            return true;
        }
        if super::effective_action::is_passthrough_parameter(&lower) {
            return false;
        }
        if spec.sender_parameters.contains(&lower.as_str()) {
            return false;
        }
        spec.allowed_parameters.contains(&lower.as_str())
    };

    if let Some(needed) = required.iter().find(|name| !admissible(name)) {
        return ToolProjection::Withheld {
            reason: format!(
                "`{capability}` requires `{needed}` to send, and its bindable form has no room \
                 for it — so every act composed here would be refused"
            ),
        };
    }

    let kept: Map<String, Value> = properties
        .iter()
        .filter(|(name, _)| admissible(name))
        .map(|(name, schema)| (name.clone(), schema.clone()))
        .collect();
    let kept_required: Vec<Value> = required
        .iter()
        .filter(|name| kept.contains_key(*name))
        .map(|name| Value::String(name.clone()))
        .collect();

    ToolProjection::Restricted {
        parameters: narrowed_schema(declared, kept, kept_required),
        note: format!(
            "This act reaches people, so it takes only its bindable form: {}. The sender is bound \
             by the runtime and cannot be chosen here.",
            spec.allowed_parameters
                .iter()
                .filter(|name| !spec.sender_parameters.contains(name))
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn declared_properties(parameters: &Value) -> Option<&Map<String, Value>> {
    parameters.as_object()?.get("properties")?.as_object()
}

fn declared_required(parameters: &Value) -> Vec<String> {
    parameters
        .as_object()
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The declared schema with a narrower `properties` and `required`, and every
/// other key left exactly as the pack wrote it.
///
/// Rebuilding a schema from scratch would silently drop whatever else it carried
/// — `additionalProperties: false` above all, which is the pack's own refusal of
/// unnamed arguments. Dropping THAT while claiming to close the surface would
/// widen the very thing this module exists to narrow.
fn narrowed_schema(
    declared: &Value,
    properties: Map<String, Value>,
    required: Vec<Value>,
) -> Value {
    let mut narrowed = declared.clone();
    if let Some(object) = narrowed.as_object_mut() {
        object.insert("properties".to_string(), Value::Object(properties));
        object.insert("required".to_string(), Value::Array(required));
        return narrowed;
    }
    // Unreachable: `declared_properties` already proved this is an object with
    // a `properties` map, and a caller that reaches here has changed that
    // without changing this.
    Value::Object(Map::from_iter([
        ("type".to_string(), Value::String("object".to_string())),
        ("properties".to_string(), Value::Object(properties)),
        ("required".to_string(), Value::Array(required)),
    ]))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decision_rail_catalog_matches_dispatch_for_action_classified_arguments() {
        let declared = json!({
            "type":"object", "additionalProperties":false,
            "properties":{"args":{"type":"array","items":{"type":"string"}}},
            "required":["args"]
        });
        let args = std::collections::HashMap::from([("args".into(), json!(["target"]))]);
        for action in ["open", "snapshot", "fill", "click", "get"] {
            assert!(
                crate::magician_v2::agents::outward_actions::outward_dispatch_class(
                    "browser", action, &args,
                )
                .is_none()
            );
            assert_eq!(
                project_capability_tool(&format!("browser__{action}"), &declared),
                ToolProjection::AsDeclared
            );
        }
        // Explicit submissions and opaque mail commands retain their gate.
        for tool in ["browser__submit", "browser__submit_form", "gmail__raw"] {
            assert!(matches!(
                project_capability_tool(tool, &declared),
                ToolProjection::Withheld { .. }
            ));
        }
    }

    fn properties_of(projection: &ToolProjection) -> Vec<String> {
        match projection {
            ToolProjection::Restricted { parameters, .. } => declared_properties(parameters)
                .map(|props| props.keys().cloned().collect())
                .unwrap_or_default(),
            other => panic!("expected a restricted projection, got {other:?}"),
        }
    }

    fn required_of(projection: &ToolProjection) -> Vec<String> {
        match projection {
            ToolProjection::Restricted { parameters, .. } => declared_required(parameters),
            other => panic!("expected a restricted projection, got {other:?}"),
        }
    }

    /// A tool that cannot reach anybody is offered exactly as its pack declared
    /// it.
    ///
    /// This module is a narrowing of outward capabilities, not a general schema
    /// rewriter. A regression here would quietly reshape the whole catalog — the
    /// shell, the filesystem, the memory tools — on the strength of a name.
    #[test]
    fn a_tool_that_reaches_nobody_is_untouched() {
        let declared = json!({
            "type": "object",
            "properties": {"command": {"type": "string"}, "args": {"type": "array"}},
            "required": ["command"],
        });
        assert_eq!(
            project_capability_tool("shell", &declared),
            ToolProjection::AsDeclared
        );
        assert_eq!(
            project_capability_tool("duckdb__query", &declared),
            ToolProjection::AsDeclared
        );
    }

    /// A sending leaf is offered only in its bindable form.
    ///
    /// `extra_args` is the hole: the shipped `gmail` skill declares it on `send`,
    /// so today the model can compose `gmail__send` with raw argv and learns it
    /// was refused only after spending the iteration. The projected schema never
    /// mentions it. `account` goes too — `restrict` overwrites it with the
    /// runtime's identity, so offering it teaches the model to choose a mailbox
    /// it does not get.
    #[test]
    fn a_sending_leaf_loses_its_escape_hatch_and_its_sender_field() {
        let declared = json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "cc": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"},
                "reply_to_message_id": {"type": "string"},
                "account": {"type": "string"},
                "extra_args": {"type": "array"},
            },
            "required": ["to", "subject", "body"],
        });
        let projected = project_capability_tool("gmail__send", &declared);

        let mut kept = properties_of(&projected);
        kept.sort();
        assert_eq!(
            kept,
            vec![
                "body".to_string(),
                "cc".to_string(),
                "reply_to_message_id".to_string(),
                "subject".to_string(),
                "to".to_string(),
            ]
        );
        let mut required = required_of(&projected);
        required.sort();
        assert_eq!(
            required,
            vec!["body".to_string(), "subject".to_string(), "to".to_string()]
        );
    }

    /// A read on a sending capability keeps the parameters it reads with.
    ///
    /// The send allowlist names what may be SENT; applying it to `gmail__triage`
    /// would strip `query`, `max` and `label` and leave a search tool with
    /// nothing to search on. Only the passthrough goes.
    #[test]
    fn a_read_on_a_sending_capability_keeps_its_own_parameters() {
        let declared = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "max": {"type": "integer"},
                "label": {"type": "string"},
                "extra_args": {"type": "array"},
            },
            "required": [],
        });
        let projected = project_capability_tool("gmail__triage", &declared);

        let mut kept = properties_of(&projected);
        kept.sort();
        assert_eq!(
            kept,
            vec!["label".to_string(), "max".to_string(), "query".to_string()]
        );
        assert_eq!(required_of(&projected), Vec::<String>::new());
    }

    /// A leaf whose only parameter IS the escape hatch is withheld, not
    /// emptied.
    ///
    /// `gmail__raw` requires `args`. Closing it leaves a tool that cannot be
    /// called and, worse, one the model will call anyway — which is the wasted
    /// iteration this module exists to remove.
    #[test]
    fn a_leaf_that_requires_its_passthrough_is_withheld() {
        let declared = json!({
            "type": "object",
            "properties": {"args": {"type": "array"}},
            "required": ["args"],
        });
        match project_capability_tool("gmail__raw", &declared) {
            ToolProjection::Withheld { reason } => {
                assert!(
                    reason.contains("`gmail__raw` requires `args`"),
                    "the refusal must name the tool and the parameter: {reason}"
                );
            },
            other => panic!("`gmail__raw` must be withheld, got {other:?}"),
        }
    }

    /// A capability that can reach the world with no bindable form is withheld
    /// wholesale.
    ///
    /// Every action of the shipped `calendar` skill takes a single argv
    /// passthrough, so `restrict` answers `NoRestrictedForm` for all of them and
    /// the dispatch gate refuses every one. The catalog must say so first.
    #[test]
    fn a_capability_with_no_bindable_form_is_withheld() {
        let declared = json!({
            "type": "object",
            "properties": {"args": {"type": "array"}},
            "required": ["args"],
        });
        assert!(matches!(
            project_capability_tool("calendar__events_insert", &declared),
            ToolProjection::Withheld { .. }
        ));
        assert!(matches!(
            project_capability_tool("calendar__events_list", &declared),
            ToolProjection::Withheld { .. }
        ));
    }

    /// A send-only capability sends whatever token it was invoked with, so its
    /// single leaf is closed to the send allowlist.
    ///
    /// `agentmail-send` has no reading actions to protect: the bare invocation
    /// IS the send. A projection that waited for a sending verb would leave this
    /// one wide open, because its name never carries one.
    #[test]
    fn a_send_only_capability_is_closed_by_its_name_alone() {
        let declared = json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "text": {"type": "string"},
                "inbox_id": {"type": "string"},
                "args": {"type": "array"},
            },
            "required": ["to"],
        });
        let projected = project_capability_tool("agentmail-send", &declared);

        let mut kept = properties_of(&projected);
        kept.sort();
        assert_eq!(
            kept,
            vec!["subject".to_string(), "text".to_string(), "to".to_string()]
        );
    }

    /// The umbrella shape keeps its read parameters and loses only its
    /// passthrough.
    ///
    /// One tool stands for every action the capability has. Closing it to the
    /// send allowlist would delete the fields its reads need, so the narrowing
    /// that applies to all of them is the one that closes the hole:
    /// `gmail action=raw args=["+send", …]` sends an email while its token says
    /// nothing.
    #[test]
    fn the_umbrella_shape_loses_only_its_passthrough() {
        let declared = json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "query": {"type": "string"},
                "to": {"type": "string"},
                "args": {"type": "array"},
            },
            "required": ["action"],
        });
        let projected = project_capability_tool("gmail", &declared);

        let mut kept = properties_of(&projected);
        kept.sort();
        assert_eq!(
            kept,
            vec!["action".to_string(), "query".to_string(), "to".to_string()]
        );
        assert_eq!(required_of(&projected), vec!["action".to_string()]);
    }

    /// A schema this module cannot read is left alone rather than replaced.
    ///
    /// Narrowing something unreadable means inventing a surface, and an invented
    /// one would refuse arguments the pack actually accepts — a gate that looks
    /// exactly like it is working while breaking every call.
    #[test]
    fn an_unreadable_schema_is_left_as_declared() {
        assert_eq!(
            project_capability_tool("gmail__send", &json!("not a schema")),
            ToolProjection::AsDeclared
        );
        assert_eq!(
            project_capability_tool("gmail__send", &json!({"type": "object"})),
            ToolProjection::AsDeclared
        );
    }

    /// The capability half of a leaf name is everything before the FIRST
    /// separator.
    ///
    /// Splitting on the last one would give `kapso-whatsapp-send__send_message`
    /// a capability of `kapso-whatsapp-send__send` — a name no table contains,
    /// so the tool would read as reaching nobody and skip every narrowing here.
    #[test]
    fn a_leaf_name_splits_on_its_first_separator() {
        assert_eq!(
            split_capability_leaf("gmail__send"),
            ("gmail", Some("send"))
        );
        assert_eq!(
            split_capability_leaf("gmail__messages__list"),
            ("gmail", Some("messages__list"))
        );
        assert_eq!(split_capability_leaf("gmail"), ("gmail", None));
    }
}
