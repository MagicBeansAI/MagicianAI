//! Chat pack direct dispatch — the eligibility and argument check.
//!
//! When chat calls a governed skill ("pack") with the call already fully
//! specified, a model-driven run adds nothing: the run engine can execute
//! the one action with a pre-decided decision and hand back the raw result.
//! "Fully specified" means all of:
//!
//! - the call names exactly one typed action leaf — an explicit
//!   `<pack>__<action>`, or a bare pack name whose pack declares exactly one
//!   native action;
//! - that leaf has a known flat route (`classify_flat_route` is not
//!   `Unknown`);
//! - the arguments are a JSON object that validates against the leaf's
//!   parameter schema (`additionalProperties: false`).
//!
//! Anything else falls back to today's model-driven run, and the fallback
//! carries a stable reason code. Design:
//! `docs/archive/plans/2026-10-08-chat-pack-direct-dispatch-design.md`. Gated by
//! `chat.chat_pack_direct_dispatch` (the caller checks the switch and reports
//! [`DirectFallback::Disabled`]).

use std::sync::Arc;

use decision_engine_contract::action::ActionTool;
use serde_json::Value;

use crate::magician_v2::execution::agentic::native_lowering::{
    is_pack_argument_pure_annotation, is_pack_argument_reserved_by_lowering,
};
use crate::magician_v2::execution::agentic::PreDecidedPackCall;
use crate::magician_v2::execution::capability::CapabilityRegistry;
use crate::magician_v2::execution::flat_loop::dispatch::{classify_flat_route, FlatRoute};
use crate::magician_v2::execution::flat_loop::tool_index::promote_pack_to_leaves;

/// Schema errors quoted in a [`DirectFallback::SchemaInvalid`] reason.
const MAX_SCHEMA_ERRORS: usize = 3;

/// A chat pack call the run engine can execute without a model: the one
/// action leaf and the schema-valid arguments for it.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectPlan {
    /// The flat leaf name, always `<pack>__<action>`.
    pub leaf: String,
    pub args: Value,
}

/// Why a chat pack call runs model-driven instead of direct.
#[derive(Debug, Clone, PartialEq)]
pub enum DirectFallback {
    /// `chat.chat_pack_direct_dispatch` is off.
    Disabled,
    /// The chat turn has no capability registry to plan against.
    NoRegistry,
    /// The call names a pack without exactly one typed action.
    NotSingleAction,
    /// The pack, its leaf, or the leaf's flat route is unknown.
    NoFlatRoute,
    /// The arguments do not satisfy the leaf's parameter schema. Names the
    /// failing field paths; never echoes argument values.
    SchemaInvalid(String),
    /// The skill's own leaf schema cannot be compiled into a validator.
    UnusableSchema(String),
    /// The leaf declares a parameter whose name the run loop's lowering
    /// strips, flattens or lifts off the arguments, or the arguments carry
    /// one lowering acts on or lifts (decision metadata such as
    /// `task_state_action`, the `parameters` wrapper, credential references),
    /// so a direct call would silently run with different arguments than were
    /// validated. A pure annotation in the arguments (`thinking`) is not
    /// refused: lowering drops it without acting on it. Names the parameter.
    ReservedParameter(String),
}

impl DirectFallback {
    /// Stable reason code for telemetry and run metadata.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NoRegistry => "no_registry",
            Self::NotSingleAction => "not_single_action",
            Self::NoFlatRoute => "no_flat_route",
            Self::SchemaInvalid(_) => "schema_invalid",
            Self::UnusableSchema(_) => "unusable_schema",
            Self::ReservedParameter(_) => "reserved_parameter",
        }
    }
}

/// Decide whether a chat pack call is fully specified, and if so return the
/// leaf and arguments to execute. Does not consult the kill switch.
pub fn plan_direct_dispatch(
    registry: &Arc<CapabilityRegistry>,
    tool_name: &str,
    args: &Value,
) -> Result<DirectPlan, DirectFallback> {
    let resolved = registry
        .resolve_tool_definition(tool_name)
        .ok_or(DirectFallback::NoFlatRoute)?;
    let pack = &resolved.definition;
    // A bare pack name resolves with no action even when the pack has only
    // one; normalize it to that action, because a governed single-action
    // skill has a flat route only under its `<pack>__<action>` name.
    let action = match resolved.action {
        Some(action) => action,
        None => {
            let mut actions = pack.native_action_schemas.keys();
            match (actions.next(), actions.next()) {
                (Some(only), None) => only.clone(),
                _ => return Err(DirectFallback::NotSingleAction),
            }
        },
    };
    let leaf = format!("{}__{}", pack.name, action);
    if matches!(
        classify_flat_route(registry, &leaf),
        FlatRoute::Unknown { .. }
    ) {
        return Err(DirectFallback::NoFlatRoute);
    }
    let entry = promote_pack_to_leaves(pack)
        .into_iter()
        .find(|entry| entry.name == leaf)
        .ok_or(DirectFallback::NoFlatRoute)?;
    // Lowering's own rule, not a copy of its list: a parameter it would not
    // forward unchanged cannot be pre-decided.
    if let Some(reserved) = entry
        .parameters_schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| {
            properties
                .keys()
                .find(|name| is_pack_argument_reserved_by_lowering(name))
        })
    {
        return Err(DirectFallback::ReservedParameter(reserved.clone()));
    }
    let Some(arg_map) = args.as_object() else {
        return Err(DirectFallback::SchemaInvalid(
            "arguments are not an object".to_string(),
        ));
    };
    // A pure annotation the model adds to its tool calls (`thinking`) is set
    // aside before validation: lowering drops it without acting on it, so the
    // original arguments still execute exactly the validated call. Refusing
    // it sent ordinary chat calls back to the model-driven run.
    let mut validated = arg_map.clone();
    validated.retain(|name, _| !is_pack_argument_pure_annotation(name));
    // Every other key lowering acts on or lifts (a `parameters` wrapper, a
    // credential reference, `task_state_action`, plan-step and vision
    // signals) would make the executed call differ from the validated one,
    // even when the leaf's schema never declared it.
    if let Some(reserved) = validated
        .keys()
        .find(|name| is_pack_argument_reserved_by_lowering(name))
    {
        return Err(DirectFallback::ReservedParameter(reserved.clone()));
    }
    let validated = Value::Object(validated);
    let tool = ActionTool {
        name: entry.name,
        description: entry.description,
        parameters: entry.parameters_schema,
    };
    let validator = magician_decision::action::validation::validator(&tool)
        .map_err(DirectFallback::UnusableSchema)?;
    // The masked form never prints the offending value (it may be a secret
    // or arbitrarily large); the instance path names the field instead.
    let errors: Vec<String> = validator
        .iter_errors(&validated)
        .take(MAX_SCHEMA_ERRORS)
        .map(|error| {
            let path = error.instance_path.as_str();
            let path = if path.is_empty() { "/" } else { path };
            format!("{path}: {}", error.masked())
        })
        .collect();
    if !errors.is_empty() {
        return Err(DirectFallback::SchemaInvalid(errors.join("; ")));
    }
    // The original arguments, annotations included: the run loop lowers them
    // through the same `lower_native_tool_call`, which strips them again.
    Ok(DirectPlan {
        leaf,
        args: args.clone(),
    })
}

/// The chat dispatch entry: honor the kill switch, require a registry, plan
/// the call, and on success return the pre-decided call the run engine
/// executes. The tool-call id is namespaced (`predecided:<id>`) so the
/// synthetic action never collides with the chat model's own call id.
pub fn predecided_chat_pack_call(
    enabled: bool,
    registry: Option<&Arc<CapabilityRegistry>>,
    tool_name: &str,
    tool_call_id: &str,
    args: &Value,
) -> Result<PreDecidedPackCall, DirectFallback> {
    if !enabled {
        return Err(DirectFallback::Disabled);
    }
    let registry = registry.ok_or(DirectFallback::NoRegistry)?;
    let plan = plan_direct_dispatch(registry, tool_name, args)?;
    Ok(PreDecidedPackCall {
        leaf: plan.leaf,
        args: plan.args,
        tool_call_id: format!("predecided:{tool_call_id}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use serde_json::json;

    /// `lookup`: one typed action `run` on a CLI-template primitive (a known
    /// flat route). `multi`: two typed actions.
    fn registry() -> Arc<CapabilityRegistry> {
        let registry = Arc::new(CapabilityRegistry::new());
        for yaml in [
            r#"
name: lookup
parameters: []
native_action_schemas:
  run:
    parameters: [order_id]
    required: [order_id]
    parameter_overrides:
      order_id: {type: string}
implementation:
  type: primitive
  command: [lookup]
"#,
            r#"
name: multi
parameters: []
native_action_schemas:
  first:
    parameters: [query]
  second:
    parameters: [query]
implementation:
  type: primitive
  command: [multi]
"#,
            r#"
name: reserved
parameters: []
native_action_schemas:
  run:
    parameters: [query, thinking]
    required: [query]
implementation:
  type: primitive
  command: [reserved]
"#,
            r#"
name: identity
parameters: []
native_action_schemas:
  run:
    parameters: [first_name, last_name, zip]
    required: [first_name, last_name, zip]
    parameter_overrides:
      first_name: {type: string}
      last_name: {type: string}
      zip: {type: string}
implementation:
  type: primitive
  command: [identity]
"#,
        ] {
            let definition: CapabilityPackDefinition =
                serde_yaml::from_str(yaml).expect("fixture pack parses");
            let name = definition.name.clone();
            registry.set_pack_definition(&name, definition);
        }
        registry
    }

    #[test]
    fn bare_single_action_pack_normalizes_to_its_leaf() {
        let plan = plan_direct_dispatch(&registry(), "lookup", &json!({"order_id": "A-1"}))
            .expect("fully specified");
        assert_eq!(plan.leaf, "lookup__run");
        assert_eq!(plan.args, json!({"order_id": "A-1"}));
    }

    #[test]
    fn explicit_leaf_is_direct() {
        let plan = plan_direct_dispatch(&registry(), "lookup__run", &json!({"order_id": "A-1"}))
            .expect("fully specified");
        assert_eq!(plan.leaf, "lookup__run");
    }

    #[test]
    fn multi_action_pack_is_not_single_action() {
        assert_eq!(
            plan_direct_dispatch(&registry(), "multi", &json!({})),
            Err(DirectFallback::NotSingleAction)
        );
    }

    #[test]
    fn explicit_leaf_of_multi_action_pack_is_direct() {
        let plan = plan_direct_dispatch(&registry(), "multi__first", &json!({"query": "q"}))
            .expect("an explicit leaf is fully specified");
        assert_eq!(plan.leaf, "multi__first");
    }

    #[test]
    fn invented_action_suffix_has_no_flat_route() {
        assert_eq!(
            plan_direct_dispatch(&registry(), "lookup__nope", &json!({"order_id": "A-1"})),
            Err(DirectFallback::NoFlatRoute)
        );
    }

    #[test]
    fn wrong_argument_type_is_schema_invalid_without_echoing_the_value() {
        let fallback = plan_direct_dispatch(&registry(), "lookup", &json!({"order_id": 987654321}))
            .expect_err("order_id must be a string");
        let DirectFallback::SchemaInvalid(message) = &fallback else {
            panic!("expected SchemaInvalid, got {fallback:?}");
        };
        assert!(
            message.contains("/order_id"),
            "names the field path: {message}"
        );
        assert!(
            !message.contains("987654321"),
            "must not echo the argument value: {message}"
        );
        assert_eq!(fallback.code(), "schema_invalid");
    }

    #[test]
    fn missing_required_argument_is_schema_invalid() {
        let fallback = plan_direct_dispatch(&registry(), "lookup", &json!({}))
            .expect_err("order_id is required");
        let DirectFallback::SchemaInvalid(message) = &fallback else {
            panic!("expected SchemaInvalid, got {fallback:?}");
        };
        assert!(message.contains("order_id"), "names the field: {message}");
    }

    #[test]
    fn extra_property_is_schema_invalid() {
        let fallback = plan_direct_dispatch(
            &registry(),
            "lookup",
            &json!({"order_id": "A-1", "verbose": true}),
        )
        .expect_err("the leaf schema forbids extra properties");
        assert!(matches!(fallback, DirectFallback::SchemaInvalid(_)));
    }

    #[test]
    fn absent_pack_has_no_flat_route() {
        let fallback =
            plan_direct_dispatch(&registry(), "nope", &json!({})).expect_err("unknown pack");
        assert_eq!(fallback, DirectFallback::NoFlatRoute);
        assert_eq!(fallback.code(), "no_flat_route");
    }

    #[test]
    fn a_parameter_lowering_strips_is_reserved() {
        // `thinking` is decision metadata to the run loop's lowering, so a
        // direct call would reach the leaf without it.
        let fallback = plan_direct_dispatch(
            &registry(),
            "reserved",
            &json!({"query": "q", "thinking": "x"}),
        )
        .expect_err("the leaf declares a reserved parameter");
        assert_eq!(
            fallback,
            DirectFallback::ReservedParameter("thinking".to_string())
        );
        assert_eq!(fallback.code(), "reserved_parameter");
        assert!(is_pack_argument_reserved_by_lowering("credential_id"));
        assert!(is_pack_argument_reserved_by_lowering("parameters"));
        assert!(!is_pack_argument_reserved_by_lowering("query"));
    }

    #[test]
    fn a_reserved_argument_key_is_reserved_even_when_the_schema_never_declares_it() {
        // `lookup__run` declares only `order_id`; lowering would flatten a
        // `parameters` wrapper, lift a credential reference, or act on a
        // task-state action / plan-step signal, so the executed call would
        // differ from the validated one. Driven from lowering's own list, so a
        // metadata name added there is covered here without editing this test.
        let acted_on_metadata =
            crate::magician_v2::execution::agentic::native_lowering::METADATA_FIELD_NAMES
                .iter()
                .copied()
                .filter(|name| !is_pack_argument_pure_annotation(name));
        let lifted = ["parameters", "credential_id", "credential_token"];
        let keys: Vec<&str> = acted_on_metadata.chain(lifted).collect();
        assert!(keys.contains(&"task_state_action"), "{keys:?}");
        assert!(!keys.contains(&"thinking"), "{keys:?}");
        for key in keys {
            let mut args = json!({"order_id": "A-1"});
            args[key] = json!({"order_id": "B-2"});
            assert_eq!(
                plan_direct_dispatch(&registry(), "lookup", &args),
                Err(DirectFallback::ReservedParameter(key.to_string())),
                "argument key {key}"
            );
        }
    }

    /// The chat model routinely adds `thinking` to a tool call's arguments
    /// (τ² retail task 4). Lowering drops it without acting on it, so it is
    /// set aside for validation and the call is still pre-decided — with the
    /// original arguments, which the run loop lowers identically.
    #[test]
    fn a_thinking_annotation_in_the_arguments_is_still_predecided() {
        let args = json!({
            "first_name": "Yusuf",
            "last_name": "Rossi",
            "zip": "19122",
            "thinking": "verify identity"
        });
        let plan = plan_direct_dispatch(&registry(), "identity", &args)
            .expect("an annotated, otherwise valid call is fully specified");
        assert_eq!(plan.leaf, "identity__run");
        assert_eq!(plan.args, args, "the original arguments travel unchanged");
        let call = predecided_chat_pack_call(true, Some(&registry()), "identity", "call_4", &args)
            .expect("pre-decided");
        assert_eq!(call.args["thinking"], "verify identity");

        // Set aside means not validated as a parameter, not ignored: the rest
        // must still satisfy the schema.
        let missing_zip = json!({
            "first_name": "Yusuf",
            "last_name": "Rossi",
            "thinking": "verify identity"
        });
        assert!(matches!(
            plan_direct_dispatch(&registry(), "identity", &missing_zip),
            Err(DirectFallback::SchemaInvalid(_))
        ));

        // What lowering lifts is still refused, annotation or not.
        let mut credentialed = args.clone();
        credentialed["credential_id"] = json!("cred-1");
        assert_eq!(
            plan_direct_dispatch(&registry(), "identity", &credentialed),
            Err(DirectFallback::ReservedParameter(
                "credential_id".to_string()
            ))
        );

        // Lowering itself drops the annotation from the pack's parameters.
        assert!(is_pack_argument_pure_annotation("thinking"));
        assert!(is_pack_argument_reserved_by_lowering("thinking"));
        for acted_on in [
            "task_state_action",
            "credential_id",
            "parameters",
            "step_note",
        ] {
            assert!(!is_pack_argument_pure_annotation(acted_on), "{acted_on}");
        }
        let lowered =
            crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call(
                &crate::magician_v2::execution::agentic::native_types::ExecutionToolCall {
                    id: "predecided:call_4".to_string(),
                    name: "identity__run".to_string(),
                    arguments: args,
                },
            );
        let crate::magician_v2::execution::agentic::native_types::NativeDecisionOutcome::Valid(
            envelope,
        ) = lowered
        else {
            panic!("the annotated call lowers");
        };
        let crate::magician_v2::execution::agentic::Decision::Execute { candidates, .. } =
            envelope.decision
        else {
            panic!("an annotated pack call lowers to Execute");
        };
        let crate::magician_v2::execution::ExecutableAction::Pack {
            resolved_params, ..
        } = &candidates.candidates[0].action
        else {
            panic!("a pack action");
        };
        assert!(!resolved_params.contains_key("thinking"));
        assert_eq!(resolved_params["zip"], "19122");
    }

    /// A governed single-action skill whose action receives its input as
    /// canonical JSON on stdin and echoes it (`cat`). Its executable is a
    /// test-only shell wrapper in the skill's `bin/` (as the flat awk test
    /// uses), so the governed single-file snapshot never copies a signed
    /// system binary.
    const PDD_ECHO_SKILL: &str = "---
name: pdd-echo
version: 0.1.0
description: Echo the canonical JSON input of a call (pre-decided dispatch test fixture).
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [pdd-echo]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: denied}
        limits: {timeout_secs: 30, stdin_bytes: 65536}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Echo the order lookup input.
          parameters:
            order_id:
              type: string
              description: Order id.
              required: true
              max_length: 64
---

# pdd-echo
";

    /// The governed route end to end for a pre-decided call: the chat
    /// model's call (annotation included) is planned, lowered exactly as the
    /// run loop lowers it, and dispatched through the real flat dispatcher to
    /// the governed runtime, which writes the canonical JSON input to the
    /// subprocess's stdin. Its stdout comes back verbatim.
    ///
    /// The run engine is proven for the same skill shape by the `pdd_*`
    /// integration tests in `chat::service`, which install it in a scope on
    /// the process runtime root (the root the run's primitive context takes).
    /// This test isolates the lowering + flat dispatch step on its own root.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_predecided_governed_stdin_call_runs_its_subprocess_and_returns_stdout_verbatim() {
        use crate::magician_v2::execution::compiled_providers::project_runtime_package_to_pack;
        use std::os::unix::fs::PermissionsExt;
        use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

        let tmp = tempfile::tempdir().expect("tempdir");
        // macOS tempdirs live under the `/var` alias; scope authority rejects
        // aliases, so the fixture root is canonical (as in the awk test).
        let temp_root = std::fs::canonicalize(tmp.path()).expect("canonical temp root");
        let (principal, workspace) = ("anon", "ws");
        let scope_root = temp_root.join("scopes").join(principal).join(workspace);
        let skill_dir = scope_root.join("skills/pdd-echo");
        let bin_dir = skill_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("skill bin dir");
        std::fs::write(skill_dir.join("SKILL.md"), PDD_ECHO_SKILL).expect("write skill");
        let echo = bin_dir.join("pdd-echo");
        std::fs::write(&echo, "#!/bin/sh\nexec /bin/cat\n").expect("write the echo wrapper");
        std::fs::set_permissions(&echo, std::fs::Permissions::from_mode(0o700))
            .expect("make the echo wrapper executable");

        let manifest =
            crate::magician_v2::skills::loader::parse_manifest(PDD_ECHO_SKILL, &skill_dir)
                .expect("echo skill manifest");
        let package = parse_skill_runtime_package(PDD_ECHO_SKILL)
            .expect("echo runtime package parses")
            .expect("echo runtime package exists");
        let pack = project_runtime_package_to_pack(
            &manifest.name,
            &manifest.description,
            None,
            &skill_dir,
            &skill_dir,
            package,
        )
        .expect("echo governed package projects");
        assert_eq!(
            pack.native_action_schemas.len(),
            1,
            "fixture is single-action"
        );
        let registry = Arc::new(CapabilityRegistry::new());
        registry.set_pack_definition(&pack.name, pack.clone());
        assert!(matches!(
            classify_flat_route(&registry, &format!("{}__run", pack.name)),
            FlatRoute::CliTemplatePrimitive { .. }
        ));

        // The chat model's call by bare pack name, `thinking` included.
        let args = json!({"order_id": "W-1", "thinking": "look the order up"});
        let call = predecided_chat_pack_call(true, Some(&registry), &pack.name, "call_echo", &args)
            .expect("a fully specified governed call is pre-decided");
        assert_eq!(call.leaf, format!("{}__run", pack.name));

        // Lowered as the run loop lowers it, then handed to the flat
        // dispatcher the way the executor's flat route hands it.
        let lowered =
            crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call(
                &crate::magician_v2::execution::agentic::native_types::ExecutionToolCall {
                    id: call.tool_call_id.clone(),
                    name: call.leaf.clone(),
                    arguments: call.args.clone(),
                },
            );
        let crate::magician_v2::execution::agentic::native_types::NativeDecisionOutcome::Valid(
            envelope,
        ) = lowered
        else {
            panic!("the pre-decided call lowers");
        };
        let crate::magician_v2::execution::agentic::Decision::Execute { candidates, .. } =
            envelope.decision
        else {
            panic!("a pack call lowers to Execute");
        };
        let crate::magician_v2::execution::ExecutableAction::Pack {
            resolved_params, ..
        } = &candidates.candidates[0].action
        else {
            panic!("a pack action");
        };
        let dispatch_args = Value::Object(
            resolved_params
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );

        let mut ctx = crate::magician_v2::execution::primitive_dispatch::PrimitiveExecCtx::default_for_runtime();
        ctx.storage_base_path = temp_root.clone();
        ctx.principal = Some(principal.to_string());
        ctx.workspace = Some(workspace.to_string());
        ctx.governed_executable_directory = Some(bin_dir);
        ctx.secret_store = Some(Arc::new(
            crate::magician_v2::secrets::SecretStore::new_empty(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                temp_root.join("secrets"),
            ),
        ));

        let result = crate::magician_v2::execution::flat_loop::dispatch::dispatch_flat_action(
            &call.leaf,
            &dispatch_args,
            &registry,
            &ctx,
            None,
        )
        .await
        .expect("governed stdin dispatch");
        let out = result.as_text().expect("the echo result carries stdout");
        // Canonical JSON of the declared parameters only: the annotation never
        // reaches the tool, and stdout comes back byte for byte.
        assert_eq!(out.trim_end(), r#"{"order_id":"W-1"}"#, "stdout: {out:?}");
    }

    #[test]
    fn chat_entry_honors_the_switch_and_namespaces_the_call_id() {
        let registry = registry();
        let args = json!({"order_id": "A-1"});
        assert_eq!(
            predecided_chat_pack_call(false, Some(&registry), "lookup", "call_1", &args),
            Err(DirectFallback::Disabled)
        );
        let no_registry = predecided_chat_pack_call(true, None, "lookup", "call_1", &args)
            .expect_err("nothing to plan against");
        assert_eq!(no_registry, DirectFallback::NoRegistry);
        assert_eq!(no_registry.code(), "no_registry");
        let call = predecided_chat_pack_call(true, Some(&registry), "lookup", "call_1", &args)
            .expect("fully specified");
        assert_eq!(
            call,
            PreDecidedPackCall {
                leaf: "lookup__run".to_string(),
                args,
                tool_call_id: "predecided:call_1".to_string(),
            }
        );
    }

    /// A real single-action governed skill. Its typed action is re-projected
    /// as the pack-level parameters, which is what chat advertises (the chat
    /// catalog and the surface tool index both build from `pack.parameters`).
    const AWK_SKILL: &str = include_str!("../../../../skillshub/awk/SKILL.md");

    fn property_names(schema: &Value) -> std::collections::BTreeSet<String> {
        schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn required_names(schema: &Value) -> std::collections::BTreeSet<String> {
        schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the model is shown and what direct dispatch validates against
    /// must agree, or every call silently falls back. For a single-action
    /// governed pack the advertised (surface) entry and the leaf carry the
    /// same properties, the same required set and the same per-property
    /// schemas; the leaf only adds `additionalProperties: false`.
    #[test]
    fn single_action_governed_pack_advertises_exactly_the_leaf_contract() {
        use crate::magician_v2::execution::compiled_providers::project_runtime_package_to_pack;
        use crate::magician_v2::execution::flat_loop::tool_index::build_surface_tool_index;
        use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

        let skill_dir = std::path::Path::new("skillshub/awk");
        let manifest = crate::magician_v2::skills::loader::parse_manifest(AWK_SKILL, skill_dir)
            .expect("awk skill manifest");
        let package = parse_skill_runtime_package(AWK_SKILL)
            .expect("awk runtime package parses")
            .expect("awk runtime package exists");
        let pack = project_runtime_package_to_pack(
            &manifest.name,
            &manifest.description,
            None,
            skill_dir,
            skill_dir,
            package,
        )
        .expect("awk governed package projects");
        assert_eq!(
            pack.native_action_schemas.len(),
            1,
            "fixture is single-action"
        );

        let surface_index = build_surface_tool_index(std::slice::from_ref(&pack));
        let surface = surface_index.get(&pack.name).expect("surface entry");
        let leaf = promote_pack_to_leaves(&pack)
            .into_iter()
            .find(|entry| entry.name == format!("{}__run", pack.name))
            .expect("leaf entry");

        assert_eq!(
            property_names(&surface.parameters_schema),
            property_names(&leaf.parameters_schema)
        );
        assert_eq!(
            required_names(&surface.parameters_schema),
            required_names(&leaf.parameters_schema)
        );
        for name in property_names(&leaf.parameters_schema) {
            assert_eq!(
                surface.parameters_schema["properties"][&name],
                leaf.parameters_schema["properties"][&name],
                "property {name}"
            );
        }
        // The chat catalog (`pack_parameters_to_schema`) builds from the same
        // pack-level parameters.
        let pack_level: std::collections::BTreeSet<String> = pack
            .parameters
            .iter()
            .map(|param| param.name.clone())
            .collect();
        assert_eq!(pack_level, property_names(&leaf.parameters_schema));
        assert_eq!(leaf.parameters_schema["additionalProperties"], false);

        // An argument object written against the advertised schema plans.
        let registry = Arc::new(CapabilityRegistry::new());
        registry.set_pack_definition(&pack.name, pack.clone());
        let plan = plan_direct_dispatch(
            &registry,
            &pack.name,
            &json!({"program": "{print $2}", "input_file": "data.txt"}),
        )
        .expect("an advertised-schema call is fully specified");
        assert_eq!(plan.leaf, format!("{}__run", pack.name));
    }

    #[test]
    fn non_object_arguments_are_schema_invalid() {
        assert_eq!(
            plan_direct_dispatch(&registry(), "lookup", &json!("A-1")),
            Err(DirectFallback::SchemaInvalid(
                "arguments are not an object".to_string()
            ))
        );
    }
}
