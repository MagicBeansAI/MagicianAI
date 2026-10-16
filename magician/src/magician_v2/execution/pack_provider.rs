//! YAML-driven pack capability provider.
//!
//! `PackCapabilityProvider` implements `CapabilityProvider` for capabilities
//! defined in YAML capability pack files. It supports one implementation
//! strategy today:
//!
//! - **Composite**: sequences calls to other registered providers
//!
//! **Compiled** packs are skipped during directory scanning — they exist purely
//! for declarative discovery and schema documentation.
//!
//! The legacy `JavaScript` impl type (which routed JS snippets through
//! Magicutor for in-browser execution) was retired alongside the direct
//! browser-action runtime; nothing in the active tree used it.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use super::actions::{ActionResult, ExecutableAction};
use super::agentic::shell_tool::wrap_output;
use super::capability::{
    coerce_bool_flag_default, coerce_bool_flag_value, CapabilityPackDefinition, CapabilityProvider,
    CapabilityRegistry, CommandArgMapping, CompositeStep, ContentType, ImplementationType,
    ParameterDef,
};
use super::error::ExecutionError;
use super::magicutor_client::MagicutorClient;
use crate::magician_v2::api_mining::body_template::render_body_template_with_values;
use crate::magician_v2::artifact_v2::CapabilityScopePaths;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::spend_gate::SpendGate;
use crate::magician_v2::strategy::plan::PlanStep;

/// Maximum composite nesting depth to prevent infinite recursion from circular packs.
const MAX_COMPOSITE_DEPTH: u32 = 8;

// Task-local depth counter — scoped to the Tokio task, so it survives .await
// migrations across OS threads (unlike thread_local! which is unsound in async).
tokio::task_local! {
    static COMPOSITE_DEPTH: Cell<u32>;
}

/// Wrap a lowered `ExecutableAction` in `MaybeGatedAction::Gated` if the
/// pack declares `execution.spend:`, otherwise return `MaybeGatedAction::Bare`.
///
/// Extracted as a free function so every `CapabilityProvider::lower()` impl
/// (`PackCapabilityProvider`, `AgentBackendProviderShared`, plus the
/// compiled-provider `lower()` impls in `compiled_providers.rs`)
/// honours the same `spend:` semantics. Without this, providers that
/// build their own `lower()` end up silently dropping the spend
/// declaration — which is exactly what happened for `create_task.yaml`
/// and `create_agent.yaml` until this helper was introduced: their
/// providers delegated to `AgentBackendProviderShared::lower()` (which
/// hard-coded `MaybeGatedAction::Bare`), so the gate never fired
/// regardless of `resource_authority.enabled`.
///
/// Mirrors the `SpendDeclaration → SpendGate` conversion previously
/// inlined in `PackCapabilityProvider::lower()` (now delegated here).
///
/// `pack_def` is `Option<&...>` so providers whose `pack_def` field
/// hasn't been populated (test harnesses, orphan registry entries)
/// can pass `None` and fall back to `Bare` without ceremony at the
/// call site.
pub fn maybe_wrap_with_spend_gate(
    action: ExecutableAction,
    pack_def: Option<&CapabilityPackDefinition>,
    resolved_params: &HashMap<String, serde_json::Value>,
) -> MaybeGatedAction {
    use crate::magician_v2::resource_authority::gated_action::SpendGatedAction;
    use crate::magician_v2::resource_authority::spend_gate::SpendGate;

    let Some(pack_def) = pack_def else {
        return MaybeGatedAction::Bare(action);
    };
    let Some(spend) = pack_def.execution.as_ref().and_then(|e| e.spend.as_ref()) else {
        return MaybeGatedAction::Bare(action);
    };

    MaybeGatedAction::Gated(SpendGatedAction::new(
        action,
        SpendGate::from_declaration(spend, &pack_def.name, resolved_params),
    ))
}

/// Read `runtime_catalog.spend` from a skill `SKILL.md` and lower it the same
/// way pack YAML `execution.spend` is lowered.
pub fn spend_gate_from_skill_source(
    source: &str,
    capability_name: &str,
    resolved_params: &HashMap<String, serde_json::Value>,
) -> Option<SpendGate> {
    use super::compiled_providers::project_runtime_spend;
    use crate::magician_v2::resource_authority::spend_gate::SpendGate;
    use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

    let package = parse_skill_runtime_package(source).ok().flatten()?;
    let spend = package.catalog.spend.as_ref()?;
    let declaration = project_runtime_spend(spend).ok()?;
    Some(SpendGate::from_declaration(
        &declaration,
        capability_name,
        resolved_params,
    ))
}

// ============================================================================
// PackCapabilityProvider
// ============================================================================

/// A capability provider driven by a YAML-defined `CapabilityPackDefinition`.
///
/// Lowering produces `ExecutableAction::Pack`, and execution dispatches
/// based on the implementation type. Today this means `Composite` and
/// `Command`; the legacy `JavaScript` impl (Magicutor `Evaluate` round-
/// trip) was retired alongside Magicutor.
pub struct PackCapabilityProvider {
    definition: CapabilityPackDefinition,
    /// Registry reference for Composite execution (look up sub-tools).
    registry: Arc<CapabilityRegistry>,
    /// MagicutorClient retained on the constructor for backward compat
    /// with callers that still pass `Some(client)`. Not used by any
    /// active impl path; kept so introducing a new browser-bridged
    /// impl type later doesn't require re-threading the dependency.
    #[allow(dead_code)]
    magicutor_client: Option<Arc<MagicutorClient>>,
    /// Optional V3 capability scope roots for command/auth path interpolation.
    scope_paths: Option<CapabilityScopePaths>,
}

impl std::fmt::Debug for PackCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackCapabilityProvider")
            .field("definition", &self.definition)
            .finish()
    }
}

impl PackCapabilityProvider {
    /// Create a new pack provider.
    pub fn new(
        definition: CapabilityPackDefinition,
        registry: Arc<CapabilityRegistry>,
        magicutor_client: Option<Arc<MagicutorClient>>,
    ) -> Self {
        Self {
            definition,
            registry,
            magicutor_client,
            scope_paths: None,
        }
    }

    pub fn with_scope_paths(mut self, scope_paths: CapabilityScopePaths) -> Self {
        self.scope_paths = Some(scope_paths);
        self
    }
}

#[async_trait]
impl CapabilityProvider for PackCapabilityProvider {
    fn tool_name(&self) -> &str {
        &self.definition.name
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let normalized_params = self
            .definition
            .normalize_embedded_body_parameters(&step.parameters);
        let resolved_params = self.definition.resolve_params(&normalized_params)?;
        let action = ExecutableAction::Pack {
            capability_name: self.definition.name.clone(),
            implementation: self.definition.implementation.clone(),
            resolved_params: resolved_params.clone(),
        };
        Ok(maybe_wrap_with_spend_gate(
            action,
            Some(&self.definition),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let (capability_name, action_params) = match action {
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } => (capability_name, resolved_params),
            _ => {
                return Err(ExecutionError::Step(
                    "PackCapabilityProvider received non-pack action".to_string(),
                ));
            },
        };

        // Use the provider's own definition as the authoritative implementation type.
        // The action's `implementation` field may be a placeholder (Compiled) injected
        // by parse_action in decision.rs; the pack YAML definition is the source of truth.
        let implementation = &self.definition.implementation;
        let normalized_params = self
            .definition
            .normalize_embedded_body_parameters(action_params);
        let resolved_params = self.definition.resolve_params(&normalized_params)?;

        // The caller is responsible for resolving the timeout chain
        // (step override → provider YAML default → executor global).
        // We use the value as-is.
        debug!(
            "[PACK] Executing capability '{}' with {} params (timeout={}s)",
            capability_name,
            resolved_params.len(),
            timeout_secs,
        );

        match implementation {
            ImplementationType::Composite { steps } => {
                execute_composite(
                    steps,
                    &resolved_params,
                    &self.registry,
                    session_id,
                    timeout_secs,
                )
                .await
            }
            ImplementationType::Compiled { provider_name } => {
                Err(ExecutionError::Step(format!(
                    "Compiled capability '{}' (provider: '{}') should not be executed via PackCapabilityProvider — \
                     it should be handled by its compiled provider",
                    capability_name, provider_name
                )))
            }
            ImplementationType::Command {
                program,
                fixed_args,
                arg_mappings,
                suffix_args,
                content_type,
                env,
            } => {
                execute_command(
                    capability_name,
                    program,
                    fixed_args,
                    arg_mappings,
                    suffix_args,
                    content_type,
                    env,
                    &self.definition.parameters,
                    &resolved_params,
                    self.scope_paths.as_ref(),
                    session_id.as_deref(),
                    timeout_secs,
                )
                .await
            }
            ImplementationType::Primitive { .. } => {
                tracing::warn!(
                    "primitive pack '{}' not yet wired through PackCapabilityProvider",
                    capability_name
                );
                Err(ExecutionError::Step(format!(
                    "primitive capability '{}' must be dispatched through the primitive-dispatch runtime, \
                     not PackCapabilityProvider",
                    capability_name
                )))
            }
        }
    }

    fn requires_browser_session(&self) -> bool {
        // YAML metadata is the source of truth post-JS-removal: if the
        // pack declares `execution.requires_browser_session`, honour it.
        // Otherwise default to false — there's no implementation type
        // left in this provider that intrinsically requires a browser.
        self.definition
            .execution
            .as_ref()
            .and_then(|exec| exec.requires_browser_session)
            .unwrap_or(false)
    }

    fn default_timeout_secs(&self) -> u64 {
        self.definition
            .execution
            .as_ref()
            .and_then(|e| e.default_timeout_secs)
            .unwrap_or(30)
    }
}

// ============================================================================
// Composite Execution
// ============================================================================

/// Execute a composite capability by iterating sub-steps and delegating to the registry.
///
/// Returns the result of the **last** sub-step only (last-result-wins). Earlier step
/// results are not chained — each sub-step receives the original `resolved_params`,
/// not outputs from prior steps. This is a known Phase 1 limitation.
async fn execute_composite(
    steps: &[CompositeStep],
    resolved_params: &HashMap<String, serde_json::Value>,
    registry: &CapabilityRegistry,
    session_id: Option<String>,
    timeout_secs: u64,
) -> Result<ActionResult, ExecutionError> {
    // Cycle guard: prevent infinite recursion from circular composite packs.
    //
    // Uses tokio::task_local so the counter stays with the async task across
    // .await thread migrations. On the first entry (no scope set yet), we
    // initialise a new scope; recursive entries increment the existing counter.
    let already_scoped = COMPOSITE_DEPTH.try_with(|_| ()).is_ok();
    if !already_scoped {
        // First entry — set up the task-local scope and delegate.
        return COMPOSITE_DEPTH
            .scope(
                Cell::new(0),
                execute_composite_inner(steps, resolved_params, registry, session_id, timeout_secs),
            )
            .await;
    }
    // Already inside a scope — run directly.
    execute_composite_inner(steps, resolved_params, registry, session_id, timeout_secs).await
}

/// Inner implementation of composite execution, called within a COMPOSITE_DEPTH scope.
async fn execute_composite_inner(
    steps: &[CompositeStep],
    resolved_params: &HashMap<String, serde_json::Value>,
    registry: &CapabilityRegistry,
    session_id: Option<String>,
    timeout_secs: u64,
) -> Result<ActionResult, ExecutionError> {
    let depth = COMPOSITE_DEPTH.with(|d| {
        let current = d.get();
        d.set(current + 1);
        current + 1
    });
    // Ensure we decrement on all exit paths (normal return, early error, panic).
    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            COMPOSITE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    let _guard = DepthGuard;

    if depth >= MAX_COMPOSITE_DEPTH {
        return Err(ExecutionError::Step(format!(
            "Composite execution exceeded maximum nesting depth ({}) — \
             check for circular capability references",
            MAX_COMPOSITE_DEPTH
        )));
    }

    if steps.is_empty() {
        warn!("[COMPOSITE] Capability has an empty steps list — returning bare Success");
    }

    let mut last_result = ActionResult::success();

    for (i, composite_step) in steps.iter().enumerate() {
        let provider = registry.get(&composite_step.tool).ok_or_else(|| {
            ExecutionError::Step(format!(
                "Composite step {} references tool '{}' which is not registered",
                i, composite_step.tool
            ))
        })?;

        // Interpolate parameters from resolved_params into step params
        let mut interpolated = interpolate_params(&composite_step.parameters, resolved_params)
            .map_err(ExecutionError::Step)?;

        // Inject timeout into parameters so lowering functions (which read
        // params["timeout"], not step.timeout_override_secs) honour the
        // composite executor's timeout.
        interpolated
            .entry("timeout".to_string())
            .or_insert_with(|| serde_json::Value::String(timeout_secs.to_string()));

        // Build a synthetic PlanStep for the sub-tool
        let synthetic_step = PlanStep {
            id: format!("composite_step_{}", i),
            task: format!("Sub-step {} of composite capability", i),
            tool: Some(composite_step.tool.clone()),
            parameters: interpolated,
            expected_outputs: vec![],
            confidence: 1.0,
            metadata: HashMap::new(),
            timeout_override_secs: Some(timeout_secs),
            ..Default::default()
        };

        // Lower the synthetic step
        let sub_action = provider.lower(&synthetic_step)?;

        info!(
            "[COMPOSITE] Executing sub-step {}/{}: tool='{}' action_type='{}' (depth={})",
            i + 1,
            steps.len(),
            composite_step.tool,
            sub_action.inner_action().action_type_name(),
            depth
        );

        // Execute — composite sub-steps use the inner action directly;
        // spend gating is handled at the top-level dispatch, not within composites.
        last_result = provider
            .execute(sub_action.inner_action(), session_id.clone(), timeout_secs)
            .await?;
    }

    Ok(last_result)
}

/// Replace `{param_name}` placeholders in step parameter values with resolved values.
///
/// Params are sorted by key length descending so longer names match first,
/// preventing prefix collisions (e.g., `{url_encoded}` before `{url}`).
fn interpolate_params(
    step_params: &HashMap<String, String>,
    resolved: &HashMap<String, serde_json::Value>,
) -> Result<HashMap<String, serde_json::Value>, String> {
    // Sort once, reuse for all templates.
    let mut sorted_params: Vec<_> = resolved.iter().collect();
    sorted_params.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));

    step_params
        .iter()
        .map(|(key, template)| {
            let mut value = if key == "body" && template.contains("{{") {
                render_body_template_with_values(template, resolved)?
            } else {
                template.clone()
            };
            for (param_name, param_value) in &sorted_params {
                let placeholder = format!("{{{}}}", param_name);
                let replacement = match param_value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                value = value.replace(&placeholder, &replacement);
            }
            Ok((key.clone(), serde_json::Value::String(value)))
        })
        .collect()
}

// ============================================================================
// Command Execution (safe, no shell)
// ============================================================================

/// Load a skill's per-skill `.env`. Resolves which layer the skill
/// is actually installed at (via SKILL.md presence — same test the
/// cli_template dispatcher uses for `MAGICIAN_SKILL_DIR`), then loads
/// `config/.env` from that resolved layer. Returns an empty map if
/// the layer's `config/.env` is absent — a skill that doesn't declare
/// any secrets needs no env file at all.
///
/// Layer resolution mirrors `dispatcher::resolve_skill_dir`:
///   1) <scope>/skills/<skill>/SKILL.md → load <scope>/.../config/.env
///   2) else each `<extra_path>/skills/<skill>/config/.env` in
///      declared order (from `tool-runtime-config.yaml :: registry.paths`)
/// Using SKILL.md (not .env) for layer presence keeps both loaders
/// agreed on which layer is authoritative even when an operator
/// hasn't yet run `make -C skillshub setup-env`.
fn load_skill_command_env(
    scope_paths: &CapabilityScopePaths,
    skill_name: &str,
) -> Result<HashMap<String, String>, ExecutionError> {
    let mut scoped_env = HashMap::new();
    let scope_skill_dir = scope_paths
        .capabilities_root // <scope>/
        .join("skills")
        .join(skill_name);
    let resolved_skill_dir = if scope_skill_dir.join("SKILL.md").is_file() {
        Some(scope_skill_dir)
    } else {
        crate::magician_v2::config_extras::extra_skills_dirs()
            .into_iter()
            .map(|extra_skills_root| extra_skills_root.join(skill_name))
            .find(|candidate| candidate.join("SKILL.md").is_file())
    };
    let Some(skill_dir) = resolved_skill_dir else {
        return Ok(scoped_env);
    };
    let env_path = skill_dir.join("config").join(".env");
    if !env_path.exists() {
        return Ok(scoped_env);
    }
    let iter = dotenvy::from_path_iter(&env_path).map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to load skill env '{}': {}",
            env_path.display(),
            e
        ))
    })?;
    for entry in iter {
        let (key, value) = entry.map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to parse skill env '{}': {}",
                env_path.display(),
                e
            ))
        })?;
        scoped_env.insert(key, value);
    }
    Ok(scoped_env)
}

/// Execute a tool via `Command::new(program).arg()` — no shell involved.
///
/// Parameters are resolved from the capability's `resolved_params` map and passed
/// as OS-level arguments. Metacharacters are literal strings, preventing injection.
async fn execute_command(
    capability_name: &str,
    program: &str,
    fixed_args: &[String],
    arg_mappings: &[CommandArgMapping],
    suffix_args: &[String],
    content_type: &ContentType,
    env: &HashMap<String, String>,
    parameter_defs: &[ParameterDef],
    resolved_params: &HashMap<String, serde_json::Value>,
    scope_paths: Option<&CapabilityScopePaths>,
    session_id: Option<&str>,
    timeout_secs: u64,
) -> Result<ActionResult, ExecutionError> {
    let mut resolved_params = resolved_params.clone();
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        resolved_params.insert(
            "session_id".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
    }

    // Build argument vector: fixed args → mapped params → suffix args.
    let program = interpolate_command_template(
        program,
        &resolved_params,
        parameter_defs,
        capability_name,
        scope_paths,
    )?;
    let mut args: Vec<String> = fixed_args
        .iter()
        .map(|value| {
            interpolate_command_template(
                value,
                &resolved_params,
                parameter_defs,
                capability_name,
                scope_paths,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    for mapping in arg_mappings {
        match mapping {
            CommandArgMapping::Positional { param } => {
                if let Some(val) =
                    get_resolved_param(&resolved_params, parameter_defs, param, capability_name)?
                {
                    args.push(val);
                }
            },
            CommandArgMapping::Flag { flag, param } => {
                if let Some(val) =
                    get_resolved_param(&resolved_params, parameter_defs, param, capability_name)?
                {
                    args.push(flag.clone());
                    args.push(val);
                }
            },
            CommandArgMapping::BoolFlag { flag, param } => {
                if resolved_param_is_truthy(&resolved_params, parameter_defs, param) {
                    args.push(flag.clone());
                }
            },
            CommandArgMapping::SplitPositional { param } => {
                if let Some(val) =
                    get_resolved_param(&resolved_params, parameter_defs, param, capability_name)?
                {
                    if !val.trim().is_empty() {
                        let split = split_shell_words(&val).map_err(|e| {
                            ExecutionError::Step(format!(
                                "Failed to split parameter '{}' for '{}': {}",
                                param, capability_name, e
                            ))
                        })?;
                        args.extend(split);
                    }
                }
            },
            CommandArgMapping::EnvFlag { flag, env_var } => {
                let val = std::env::var(env_var).map_err(|_| {
                    ExecutionError::Step(format!(
                        "Environment variable '{}' not set (required for '{}' flag in '{}')",
                        env_var, flag, capability_name
                    ))
                })?;
                args.push(flag.clone());
                args.push(val);
            },
            CommandArgMapping::Passthrough { param } => {
                if let Some(value) = resolved_params.get(param) {
                    match value {
                        serde_json::Value::Null => {},
                        serde_json::Value::Array(items) => {
                            for item in items {
                                match item {
                                    serde_json::Value::String(s) => args.push(s.clone()),
                                    serde_json::Value::Null => {},
                                    other => args.push(other.to_string()),
                                }
                            }
                        },
                        serde_json::Value::String(s) => args.push(s.clone()),
                        other => args.push(other.to_string()),
                    }
                }
            },
            CommandArgMapping::FixedArgs { args: fixed } => {
                for value in fixed {
                    args.push(interpolate_command_template(
                        value,
                        &resolved_params,
                        parameter_defs,
                        capability_name,
                        scope_paths,
                    )?);
                }
            },
        }
    }
    for value in suffix_args {
        args.push(interpolate_command_template(
            value,
            &resolved_params,
            parameter_defs,
            capability_name,
            scope_paths,
        )?);
    }

    info!(
        "[COMMAND] Executing: {} {:?} (timeout={}s)",
        program,
        &args[..args.len().min(8)],
        timeout_secs,
    );

    // Prepend the scope tool-bin dirs so a bare npm/venv/node program (e.g. an
    // npm-vendored CLI) resolves — the same augmentation the CLI dispatcher /
    // preflight probe use. Fail-safe: a system binary still resolves via the
    // appended inherited PATH; set BEFORE the `env` overrides below so an
    // explicit per-command PATH still wins. Computed first so the program is
    // resolved against the PATH the child will see and the spawn stays on
    // `posix_spawn` (see `runtime_core::process`).
    let child_path = scope_paths.and_then(|paths| {
        let parent = std::env::var("PATH").unwrap_or_default();
        paths.subprocess_bin_path(None, &parent)
    });
    // The later env layers — the skill's `config/.env`, then the pack's own
    // `env` — win over that augmentation; when one carries `PATH`, it is the
    // PATH the child receives, so the program is resolved against it. Loaded
    // here, applied below in the same order as before.
    let skill_env = match scope_paths {
        Some(paths) => load_skill_command_env(paths, capability_name)?,
        None => HashMap::new(),
    };
    let pack_path = match env.get("PATH") {
        Some(value) => Some(interpolate_command_template(
            value,
            &resolved_params,
            parameter_defs,
            capability_name,
            scope_paths,
        )?),
        None => None,
    };
    let final_path = pack_path
        .or_else(|| skill_env.get("PATH").cloned())
        .or_else(|| child_path.clone());
    let mut cmd = tokio::process::Command::new(runtime_core::process::resolve_program_str(
        &program,
        final_path.as_deref(),
    ));
    cmd.args(&args);
    cmd.kill_on_drop(true);
    if let Some(lease) = crate::magician_v2::subprocess_owners::active_skill_working_lease() {
        cmd.current_dir(lease);
    }

    if let Some(paths) = scope_paths {
        cmd.env("HOME", &paths.home_root);
        if let Some(path) = child_path.as_deref() {
            cmd.env("PATH", path);
        }

        for (key, value) in skill_env {
            cmd.env(key, value);
        }
    }

    // Apply environment variables, interpolating {param} placeholders from resolved params.
    for (k, v) in env {
        let interpolated = interpolate_command_template(
            v,
            &resolved_params,
            parameter_defs,
            capability_name,
            scope_paths,
        )?;
        cmd.env(k, interpolated);
    }
    for name in crate::magician_v2::subprocess_owners::forbidden_child_env_names() {
        cmd.env_remove(name);
    }

    let output = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "Command '{}' timed out after {}s",
                program, timeout_secs
            ))
        })?
        .map_err(|e| ExecutionError::Step(format!("Failed to execute '{}': {}", program, e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let wrapped = wrap_output(&stdout, capability_name, content_type);

    if output.status.success() {
        Ok(ActionResult::text(wrapped))
    } else {
        // Include stderr for debugging but still return a text result.
        let error_output = if stderr.is_empty() {
            wrapped
        } else {
            format!("{}\n\n[stderr]\n{}", wrapped, stderr)
        };
        Ok(ActionResult::text(error_output))
    }
}

fn find_parameter_def<'a>(
    parameter_defs: &'a [ParameterDef],
    param: &str,
) -> Option<&'a ParameterDef> {
    parameter_defs
        .iter()
        .find(|def| def.name == param || def.aliases.iter().any(|alias| alias == param))
}

fn get_resolved_param_value<'a>(
    resolved_params: &'a HashMap<String, serde_json::Value>,
    parameter_defs: &'a [ParameterDef],
    param: &str,
) -> Option<&'a serde_json::Value> {
    resolved_params.get(param).or_else(|| {
        find_parameter_def(parameter_defs, param).and_then(|def| resolved_params.get(&def.name))
    })
}

/// True when a `BoolFlag` mapping should emit its flag.
///
/// Looks up `param` in the resolved-params map (with alias resolution
/// via `find_parameter_def`); falls back to the registered
/// `ParameterDef::default` when the param is absent. Truthiness rules
/// live in `coerce_bool_flag_value` / `coerce_bool_flag_default` (see
/// `execution::capability`).
fn resolved_param_is_truthy(
    resolved_params: &HashMap<String, serde_json::Value>,
    parameter_defs: &[ParameterDef],
    param: &str,
) -> bool {
    if let Some(value) = get_resolved_param_value(resolved_params, parameter_defs, param) {
        return coerce_bool_flag_value(value);
    }
    if let Some(def) = find_parameter_def(parameter_defs, param) {
        if let Some(default) = def.default.as_deref() {
            return coerce_bool_flag_default(default);
        }
    }
    false
}

/// Extract a parameter value as a string from the resolved params map.
///
/// Returns `Ok(None)` only when the parameter is declared optional with no
/// default and was omitted.
fn get_resolved_param(
    resolved_params: &HashMap<String, serde_json::Value>,
    parameter_defs: &[ParameterDef],
    param: &str,
    capability_name: &str,
) -> Result<Option<String>, ExecutionError> {
    if let Some(val) = get_resolved_param_value(resolved_params, parameter_defs, param) {
        return match val {
            serde_json::Value::String(s) => Ok(Some(s.clone())),
            serde_json::Value::Null => {
                if let Some(def) = find_parameter_def(parameter_defs, param) {
                    if !def.required && def.default.is_none() {
                        Ok(None)
                    } else {
                        Err(ExecutionError::Step(format!(
                            "Parameter '{}' is null for command capability '{}'",
                            param, capability_name
                        )))
                    }
                } else {
                    Err(ExecutionError::Step(format!(
                        "Parameter '{}' is null for command capability '{}'",
                        param, capability_name
                    )))
                }
            },
            other => Ok(Some(other.to_string())),
        };
    }

    if let Some(def) = find_parameter_def(parameter_defs, param) {
        if !def.required && def.default.is_none() {
            return Ok(None);
        }
        return Err(ExecutionError::Step(format!(
            "Missing required parameter '{}' for command capability '{}'",
            param, capability_name
        )));
    }

    Err(ExecutionError::Step(format!(
        "Command capability '{}' references undeclared parameter '{}'",
        capability_name, param
    )))
}

/// Interpolate `{param}` placeholders in an environment variable value
/// using resolved parameters. Safe — env values are not shell commands.
///
/// Optional omitted params normalize to the empty string. Required or
/// undeclared placeholders fail before launch.
fn interpolate_env_value(
    template: &str,
    resolved_params: &HashMap<String, serde_json::Value>,
    parameter_defs: &[ParameterDef],
    capability_name: &str,
) -> Result<String, ExecutionError> {
    let mut result = String::with_capacity(template.len());
    let mut cursor = 0usize;

    while cursor < template.len() {
        let Some(relative_start) = template[cursor..].find('{') else {
            result.push_str(&template[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        result.push_str(&template[cursor..start]);

        let Some(relative_end) = template[start..].find('}') else {
            result.push_str(&template[start..]);
            break;
        };
        let end = start + relative_end;
        let candidate = &template[start + 1..end];
        let is_placeholder = !candidate.is_empty()
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');

        if !is_placeholder {
            result.push_str(&template[start..=end]);
            cursor = end + 1;
            continue;
        }

        let replacement = if let Some(value) =
            get_resolved_param_value(resolved_params, parameter_defs, candidate)
        {
            match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            }
        } else if let Some(def) = find_parameter_def(parameter_defs, candidate) {
            if !def.required && def.default.is_none() {
                debug!(
                    "[COMMAND] omitted optional env placeholder '{{{}}}' for capability '{}'",
                    candidate, capability_name
                );
                String::new()
            } else {
                return Err(ExecutionError::Step(format!(
                    "Command capability '{}' is missing required env parameter '{}' for template '{}'",
                    capability_name, candidate, template
                )));
            }
        } else {
            return Err(ExecutionError::Step(format!(
                "Command capability '{}' env template '{}' references undeclared parameter '{}'",
                capability_name, template, candidate
            )));
        };

        result.push_str(&replacement);
        cursor = end + 1;
    }

    Ok(result)
}

fn interpolate_command_template(
    template: &str,
    resolved_params: &HashMap<String, serde_json::Value>,
    parameter_defs: &[ParameterDef],
    capability_name: &str,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Result<String, ExecutionError> {
    let template = match scope_paths {
        Some(paths) => paths.apply_vars(template),
        None => template.to_string(),
    };
    interpolate_env_value(&template, resolved_params, parameter_defs, capability_name)
}

/// Split a string into shell-word tokens without invoking a shell.
///
/// Handles:
/// - Whitespace splitting
/// - Single-quoted strings (no escape processing inside)
/// - Double-quoted strings (backslash escapes `"` and `\`)
/// - Unquoted tokens
///
/// This is safe: it's purely lexical, no command interpretation.
fn split_shell_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    let mut in_word = false;

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
                chars.next();
            },
            '\'' => {
                in_word = true;
                chars.next(); // consume opening quote
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("Unterminated single quote".to_string()),
                    }
                }
            },
            '"' => {
                in_word = true;
                chars.next(); // consume opening quote
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped) => current.push(escaped),
                            None => return Err("Unterminated escape in double quote".to_string()),
                        },
                        Some(c) => current.push(c),
                        None => return Err("Unterminated double quote".to_string()),
                    }
                }
            },
            _ => {
                in_word = true;
                current.push(c);
                chars.next();
            },
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

// ============================================================================
// YAML Loading
// ============================================================================

/// Load a `CapabilityPackDefinition` from a YAML file.
pub fn load_from_yaml(path: &Path) -> Result<CapabilityPackDefinition, ExecutionError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to read capability YAML '{}': {}",
            path.display(),
            e
        ))
    })?;

    serde_yaml::from_str(&content).map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to parse capability YAML '{}': {}",
            path.display(),
            e
        ))
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::capability::{ExecutionMetadata, ParameterDef, ParameterType};
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn build_test_scope_paths() -> CapabilityScopePaths {
        // Mirror production layout: <data_root>/scopes/<principal>/<workspace>/.
        // load_skill_command_env now resolves scope-first → extras-fallback
        // (no system layer) — the ancestor walk was retired alongside the
        // system tier.
        let data_root =
            std::env::temp_dir().join(format!("pack-provider-scope-{}", Uuid::new_v4()));
        let capabilities_root = data_root
            .join("scopes")
            .join("test-principal")
            .join("test-workspace");
        CapabilityScopePaths {
            principal: "test-principal".to_string(),
            workspace: "test-workspace".to_string(),
            bots_root: capabilities_root.join("bots"),
            auth_root: capabilities_root.join("auth"),
            workdirs_root: capabilities_root.join("workdirs"),
            // Match production: `capability_home_root` returns
            // `<workdirs>/home`, not a sibling `<scope>/home`.
            home_root: capabilities_root.join("workdirs").join("home"),
            // Test-only fixture: pack_provider's helper here is never the
            // production `scope_paths` builder. Use a stub path.
            node_modules_bin: PathBuf::from("/dev/null/skillshub_bin"),
            node_bin: PathBuf::from("/dev/null/skillshub_node_bin"),
            venv_bin: PathBuf::from("/dev/null/skillshub_venv_bin"),
            capabilities_root,
        }
    }

    /// Write a per-skill `.env` at the deployed scope-layer location,
    /// mirroring what `make -C skillshub install-scope` + `setup-env`
    /// produce in production for skills that need secrets. Also writes
    /// a stub `SKILL.md` so `load_skill_command_env` recognizes the
    /// skill is scope-installed (it uses SKILL.md presence as the
    /// layer-resolution test, matching the dispatcher).
    fn write_skill_env(
        scope_paths: &CapabilityScopePaths,
        skill_name: &str,
        contents: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let skill_dir = scope_paths
            .capabilities_root // <scope>/
            .join("skills")
            .join(skill_name);
        let config_dir = skill_dir.join("config");
        std::fs::create_dir_all(&config_dir)?;
        std::fs::write(skill_dir.join("SKILL.md"), "stub for tests\n")?;
        std::fs::write(config_dir.join(".env"), contents)?;
        Ok(())
    }

    #[test]
    fn test_interpolate_params_basic() {
        let mut step_params = HashMap::new();
        step_params.insert("url".to_string(), "https://example.com/{query}".to_string());
        step_params.insert("timeout".to_string(), "30".to_string());

        let mut resolved = HashMap::new();
        resolved.insert(
            "query".to_string(),
            serde_json::Value::String("test-search".to_string()),
        );

        let result = interpolate_params(&step_params, &resolved).unwrap();
        assert_eq!(
            result.get("url"),
            Some(&serde_json::Value::String(
                "https://example.com/test-search".to_string()
            ))
        );
        assert_eq!(
            result.get("timeout"),
            Some(&serde_json::Value::String("30".to_string()))
        );
    }

    #[test]
    fn test_interpolate_params_body_template() {
        let mut step_params = HashMap::new();
        step_params.insert(
            "body".to_string(),
            r#"{"variables":{"id":{{string:body_variables_id}},"limit":{{number:body_limit}}}}"#
                .to_string(),
        );

        let mut resolved = HashMap::new();
        resolved.insert(
            "body_variables_id".to_string(),
            serde_json::Value::String("user-42".to_string()),
        );
        resolved.insert(
            "body_limit".to_string(),
            serde_json::Value::Number(serde_json::Number::from(25)),
        );

        let result = interpolate_params(&step_params, &resolved).unwrap();
        assert_eq!(
            result.get("body"),
            Some(&serde_json::Value::String(
                r#"{"variables":{"id":"user-42","limit":25}}"#.to_string()
            ))
        );
    }

    #[test]
    fn test_load_from_yaml_roundtrip() {
        let yaml = r#"
name: test_capability
description: A test capability
version: "1.0"
parameters:
  - name: search_query
    required: true
    description: The search term
  - name: max_results
    required: false
    default: "10"
implementation:
  type: composite
  steps: []
"#;

        let tmpdir = std::env::temp_dir();
        let tmpfile = tmpdir.join("test_capability_pack.yaml");
        std::fs::write(&tmpfile, yaml).unwrap();

        let def = load_from_yaml(&tmpfile).unwrap();
        assert_eq!(def.name, "test_capability");
        assert_eq!(def.description, Some("A test capability".to_string()));
        assert_eq!(def.version, Some("1.0".to_string()));
        assert_eq!(def.parameters.len(), 2);
        assert_eq!(def.parameters[0].name, "search_query");
        assert!(def.parameters[0].required);
        assert_eq!(def.parameters[1].name, "max_results");
        assert!(!def.parameters[1].required);
        assert_eq!(def.parameters[1].default, Some("10".to_string()));

        match &def.implementation {
            ImplementationType::Composite { steps } => {
                assert!(steps.is_empty());
            },
            _ => panic!("Expected Composite implementation"),
        }

        // Clean up
        let _ = std::fs::remove_file(&tmpfile);
    }

    #[test]
    fn test_lower_produces_pack_variant() {
        let def = CapabilityPackDefinition {
            name: "my_tool".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![ParameterDef {
                name: "url".to_string(),
                required: true,
                default: None,
                description: None,
                param_type: None,
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            implementation: ImplementationType::Composite { steps: Vec::new() },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let registry = Arc::new(CapabilityRegistry::new());
        let provider = PackCapabilityProvider::new(def, registry, None);

        let step = PlanStep {
            id: "step_1".to_string(),
            task: "test".to_string(),
            tool: Some("my_tool".to_string()),
            parameters: {
                let mut p = HashMap::new();
                p.insert(
                    "url".to_string(),
                    serde_json::Value::String("https://example.com".to_string()),
                );
                p
            },
            expected_outputs: vec![],
            confidence: 1.0,
            metadata: HashMap::new(),
            timeout_override_secs: None,
            ..Default::default()
        };

        let action = provider.lower(&step).unwrap();
        assert!(action.inner_action().is_pack());
        assert_eq!(action.inner_action().action_type_name(), "pack");
    }

    #[tokio::test]
    async fn test_composite_execution_with_mock() {
        // Create a mock registry with a simple shell provider
        let registry = CapabilityRegistry::new();

        #[derive(Debug)]
        struct EchoProvider;

        #[async_trait]
        impl CapabilityProvider for EchoProvider {
            fn tool_name(&self) -> &str {
                "echo"
            }
            fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
                let msg = step
                    .parameters
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Ok(MaybeGatedAction::Bare(ExecutableAction::Bash(
                    super::super::actions::BashAction::new(format!("echo {}", msg)),
                )))
            }
            async fn execute(
                &self,
                _action: &ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<ActionResult, ExecutionError> {
                // Mock: just return success
                Ok(ActionResult::text("mock output"))
            }
        }

        registry.register(Arc::new(EchoProvider));
        let registry = Arc::new(registry);

        // Define a composite with 2 sub-steps
        let steps = vec![
            CompositeStep {
                tool: "echo".to_string(),
                parameters: {
                    let mut p = HashMap::new();
                    p.insert("message".to_string(), "step 1: {query}".to_string());
                    p
                },
            },
            CompositeStep {
                tool: "echo".to_string(),
                parameters: {
                    let mut p = HashMap::new();
                    p.insert("message".to_string(), "step 2: done".to_string());
                    p
                },
            },
        ];

        let mut resolved = HashMap::new();
        resolved.insert(
            "query".to_string(),
            serde_json::Value::String("hello world".to_string()),
        );

        let result = execute_composite(&steps, &resolved, &registry, None, 30).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.as_text(), Some("mock output"));
    }

    #[test]
    fn test_serde_roundtrip_pack_action() {
        let action = ExecutableAction::Pack {
            capability_name: "my_tool".to_string(),
            implementation: ImplementationType::Composite { steps: Vec::new() },
            resolved_params: {
                let mut p = HashMap::new();
                p.insert(
                    "key".to_string(),
                    serde_json::Value::String("value".to_string()),
                );
                p
            },
        };

        let json = serde_json::to_string(&action).unwrap();
        let deserialized: ExecutableAction = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_pack());
        assert_eq!(deserialized.action_type_name(), "pack");
    }

    #[test]
    fn requires_browser_session_from_metadata() {
        // YAML with execution.requires_browser_session: true should override heuristic
        let def = CapabilityPackDefinition {
            name: "custom_tool".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![],
            implementation: ImplementationType::Composite { steps: vec![] },
            execution: Some(ExecutionMetadata {
                requires_browser_session: Some(true),
                default_timeout_secs: None,
                chat_inline_adapter: None,
                categories: vec![],
                sandbox: None,
                composition_category: None,
                spend: None,
            }),
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let registry = Arc::new(CapabilityRegistry::new());
        let provider = PackCapabilityProvider::new(def, registry, None);

        // Composite would normally return false, but metadata overrides
        assert!(provider.requires_browser_session());

        // Verify that false also works
        let def2 = CapabilityPackDefinition {
            name: "js_tool".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![],
            implementation: ImplementationType::Composite { steps: Vec::new() },
            execution: Some(ExecutionMetadata {
                requires_browser_session: Some(false),
                default_timeout_secs: None,
                chat_inline_adapter: None,
                categories: vec![],
                sandbox: None,
                composition_category: None,
                spend: None,
            }),
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let registry2 = Arc::new(CapabilityRegistry::new());
        let provider2 = PackCapabilityProvider::new(def2, registry2, None);

        // JavaScript would normally return true, but metadata overrides to false
        assert!(!provider2.requires_browser_session());
    }

    #[test]
    fn load_from_yaml_with_enriched_fields() {
        let yaml = r#"
name: enriched_tool
description: Tool with types and aliases
version: "2.0"
parameters:
  - name: selector
    required: true
    description: CSS selector
    param_type: string
    aliases:
      - ref
    enum_values: null
  - name: timeout_ms
    required: false
    default: "10000"
    description: Timeout in ms
    param_type: integer
  - name: full_page
    required: false
    default: "false"
    description: Full page screenshot
    param_type: boolean
implementation:
  type: composite
  steps: []
execution:
  requires_browser_session: true
  default_timeout_secs: 30
  categories:
    - browser
    - web_automation
  sandbox: none
"#;

        let tmpdir = std::env::temp_dir();
        let tmpfile = tmpdir.join("test_enriched_pack.yaml");
        std::fs::write(&tmpfile, yaml).unwrap();

        let def = load_from_yaml(&tmpfile).unwrap();
        assert_eq!(def.name, "enriched_tool");
        assert_eq!(def.parameters.len(), 3);

        // Check selector param
        let selector = &def.parameters[0];
        assert_eq!(selector.param_type, Some(ParameterType::String));
        assert_eq!(selector.aliases, vec!["ref".to_string()]);

        // Check timeout_ms param
        let timeout = &def.parameters[1];
        assert_eq!(timeout.param_type, Some(ParameterType::Integer));
        assert_eq!(timeout.default, Some("10000".to_string()));

        // Check full_page param
        let full_page = &def.parameters[2];
        assert_eq!(full_page.param_type, Some(ParameterType::Boolean));

        // Check execution metadata
        let exec = def.execution.as_ref().unwrap();
        assert_eq!(exec.requires_browser_session, Some(true));
        assert_eq!(exec.default_timeout_secs, Some(30));
        assert_eq!(
            exec.categories,
            vec!["browser".to_string(), "web_automation".to_string()]
        );
        assert_eq!(exec.sandbox, Some("none".to_string()));

        // Clean up
        let _ = std::fs::remove_file(&tmpfile);
    }

    // ========================================================================
    // split_shell_words tests
    // ========================================================================

    #[test]
    fn split_shell_words_basic() {
        assert_eq!(
            split_shell_words("hello world").unwrap(),
            vec!["hello", "world"]
        );
    }

    #[test]
    fn split_shell_words_single_quotes() {
        assert_eq!(
            split_shell_words("+triage --max 5 --query 'is:unread from:team'").unwrap(),
            vec!["+triage", "--max", "5", "--query", "is:unread from:team"]
        );
    }

    #[test]
    fn split_shell_words_double_quotes_with_escape() {
        assert_eq!(
            split_shell_words(r#"echo "hello \"world\"""#).unwrap(),
            vec!["echo", r#"hello "world""#]
        );
    }

    #[test]
    fn split_shell_words_empty() {
        assert_eq!(split_shell_words("").unwrap(), Vec::<String>::new());
        assert_eq!(split_shell_words("   ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn split_shell_words_unterminated_quote() {
        assert!(split_shell_words("hello 'world").is_err());
        assert!(split_shell_words(r#"hello "world"#).is_err());
    }

    #[test]
    fn split_shell_words_injection_safe() {
        // Semicolons, pipes, etc. are NOT interpreted — they're literal characters
        let result = split_shell_words("users messages trash; rm -rf /").unwrap();
        assert_eq!(
            result,
            vec!["users", "messages", "trash;", "rm", "-rf", "/"]
        );
    }

    // ========================================================================
    // interpolate_env_value tests
    // ========================================================================

    #[test]
    fn interpolate_env_value_basic() {
        let mut params = HashMap::new();
        params.insert(
            "account".to_string(),
            serde_json::Value::String("work".to_string()),
        );
        let defs = vec![ParameterDef {
            name: "account".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: None,
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result =
            interpolate_env_value("auth/gws-{account}", &params, &defs, "gws_tool").unwrap();
        assert_eq!(result, "auth/gws-work");
    }

    #[test]
    fn interpolate_env_value_multiple_params() {
        let mut params = HashMap::new();
        params.insert("a".to_string(), serde_json::Value::String("X".to_string()));
        params.insert("b".to_string(), serde_json::Value::String("Y".to_string()));
        let defs = vec![
            ParameterDef {
                name: "a".to_string(),
                required: false,
                default: None,
                description: None,
                param_type: None,
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            },
            ParameterDef {
                name: "b".to_string(),
                required: false,
                default: None,
                description: None,
                param_type: None,
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            },
        ];
        let result = interpolate_env_value("{a}/{b}/path", &params, &defs, "test_tool").unwrap();
        assert_eq!(result, "X/Y/path");
    }

    #[test]
    fn interpolate_env_value_missing_optional_param_becomes_empty() {
        let params = HashMap::new();
        let defs = vec![ParameterDef {
            name: "missing".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: None,
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result =
            interpolate_env_value("auth/gws-{missing}", &params, &defs, "gws_tool").unwrap();
        assert_eq!(result, "auth/gws-");
    }

    #[test]
    fn interpolate_env_value_missing_required_param_errors() {
        let params = HashMap::new();
        let defs = vec![ParameterDef {
            name: "account".to_string(),
            required: true,
            default: None,
            description: None,
            param_type: None,
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let err =
            interpolate_env_value("auth/gws-{account}", &params, &defs, "gws_tool").unwrap_err();
        assert!(err.to_string().contains("missing required env parameter"));
    }

    #[test]
    fn interpolate_env_value_undeclared_param_errors() {
        let params = HashMap::new();
        let err =
            interpolate_env_value("auth/gws-{missing}", &params, &[], "gws_tool").unwrap_err();
        assert!(err.to_string().contains("references undeclared parameter"));
    }

    #[test]
    fn interpolate_env_value_no_placeholders() {
        let params = HashMap::new();
        let result = interpolate_env_value("/usr/bin/gws", &params, &[], "gws_tool").unwrap();
        assert_eq!(result, "/usr/bin/gws");
    }

    #[test]
    fn interpolate_env_value_alias_placeholder_uses_canonical_value() {
        let params = HashMap::from([("query".to_string(), serde_json::json!("Dhurandhar 2"))]);
        let defs = vec![ParameterDef {
            name: "query".to_string(),
            required: true,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec!["q".to_string()],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result = interpolate_env_value("search={q}", &params, &defs, "websearch").unwrap();
        assert_eq!(result, "search=Dhurandhar 2");
    }

    // ========================================================================
    // execute_command arg building tests (via the public execute_command fn)
    // ========================================================================

    #[tokio::test]
    async fn execute_command_echo_with_suffix_args() {
        // Test that suffix_args are appended after mapped args
        let params = HashMap::from([("msg".to_string(), serde_json::json!("hello"))]);
        let result = execute_command(
            "test_tool",
            "echo",
            &[],
            &[CommandArgMapping::Positional {
                param: "msg".to_string(),
            }],
            &["--suffix".to_string()],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &[ParameterDef {
                name: "msg".to_string(),
                required: true,
                default: None,
                description: None,
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            &params,
            None,
            None,
            10,
        )
        .await;
        assert!(result.is_ok());
        let action_result = result.unwrap();
        let output = match &action_result {
            ActionResult::Text { content, .. } => content.clone(),
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("hello --suffix"));
    }

    #[tokio::test]
    async fn execute_command_split_positional() {
        // Test that SplitPositional splits the value into multiple args
        let params = HashMap::from([("cmd".to_string(), serde_json::json!("+triage --max 5"))]);
        let result = execute_command(
            "test_tool",
            "echo",
            &["gmail".to_string()],
            &[CommandArgMapping::SplitPositional {
                param: "cmd".to_string(),
            }],
            &["--format".to_string(), "json".to_string()],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &[ParameterDef {
                name: "cmd".to_string(),
                required: true,
                default: None,
                description: None,
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            &params,
            None,
            None,
            10,
        )
        .await;
        assert!(result.is_ok());
        let output = match result.unwrap() {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        // echo should output: gmail +triage --max 5 --format json
        assert!(output.contains("gmail +triage --max 5 --format json"));
    }

    #[tokio::test]
    async fn execute_command_missing_param_errors() {
        let params = HashMap::new();
        let result = execute_command(
            "test_tool",
            "echo",
            &[],
            &[CommandArgMapping::Positional {
                param: "missing".to_string(),
            }],
            &[],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &[ParameterDef {
                name: "missing".to_string(),
                required: true,
                default: None,
                description: None,
                param_type: Some(ParameterType::String),
                aliases: vec![],
                enum_values: None,
                schema: serde_json::Value::Null,
            }],
            &params,
            None,
            None,
            10,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_command_env_interpolation() {
        // Test that env values get {param} interpolated
        let params = HashMap::from([("account".to_string(), serde_json::json!("work"))]);
        let env = HashMap::from([(
            "TEST_VAR".to_string(),
            "/runtime/auth/gws-{account}".to_string(),
        )]);
        let defs = vec![ParameterDef {
            name: "account".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result = execute_command(
            "test_tool",
            "printenv",
            &["TEST_VAR".to_string()],
            &[],
            &[],
            &ContentType::ToolOutput,
            &env,
            &defs,
            &params,
            None,
            None,
            10,
        )
        .await;
        assert!(result.is_ok());
        let output = match result.unwrap() {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("/runtime/auth/gws-work"));
    }

    #[tokio::test]
    async fn execute_command_loads_per_skill_env() {
        let scope_paths = build_test_scope_paths();
        write_skill_env(
            &scope_paths,
            "test_tool",
            "MAG_TEST_SCOPED_ENV_KEY=scoped-value\n",
        )
        .unwrap();

        let result = execute_command(
            "test_tool",
            "printenv",
            &["MAG_TEST_SCOPED_ENV_KEY".to_string()],
            &[],
            &[],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &[],
            &HashMap::new(),
            Some(&scope_paths),
            None,
            10,
        )
        .await
        .unwrap();

        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("scoped-value"));

        let _ = std::fs::remove_dir_all(
            scope_paths
                .capabilities_root
                .parent()
                .expect("scope root parent must exist"),
        );
    }

    #[tokio::test]
    async fn execute_command_pack_env_overrides_scoped_env() {
        let scope_paths = build_test_scope_paths();
        write_skill_env(
            &scope_paths,
            "test_tool",
            "MAG_TEST_SCOPED_ENV_KEY=scoped-value\n",
        )
        .unwrap();

        let env = HashMap::from([(
            "MAG_TEST_SCOPED_ENV_KEY".to_string(),
            "explicit-value".to_string(),
        )]);
        let result = execute_command(
            "test_tool",
            "printenv",
            &["MAG_TEST_SCOPED_ENV_KEY".to_string()],
            &[],
            &[],
            &ContentType::ToolOutput,
            &env,
            &[],
            &HashMap::new(),
            Some(&scope_paths),
            None,
            10,
        )
        .await
        .unwrap();

        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("explicit-value"));
        assert!(!output.contains("scoped-value"));

        let _ = std::fs::remove_dir_all(
            scope_paths
                .capabilities_root
                .parent()
                .expect("scope root parent must exist"),
        );
    }

    #[tokio::test]
    async fn execute_command_optional_flag_param_omitted_is_skipped() {
        let params = HashMap::new();
        let defs = vec![ParameterDef {
            name: "include_domains".to_string(),
            required: false,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec![],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result = execute_command(
            "test_tool",
            "echo",
            &["start".to_string()],
            &[CommandArgMapping::Flag {
                flag: "--include-domains".to_string(),
                param: "include_domains".to_string(),
            }],
            &["end".to_string()],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &defs,
            &params,
            None,
            None,
            10,
        )
        .await
        .unwrap();
        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("start end"));
        assert!(!output.contains("--include-domains"));
    }

    #[tokio::test]
    async fn execute_command_alias_mapping_uses_canonical_resolved_param() {
        let params = HashMap::from([("query".to_string(), serde_json::json!("Dhurandhar 2"))]);
        let defs = vec![ParameterDef {
            name: "query".to_string(),
            required: true,
            default: None,
            description: None,
            param_type: Some(ParameterType::String),
            aliases: vec!["q".to_string()],
            enum_values: None,
            schema: serde_json::Value::Null,
        }];
        let result = execute_command(
            "websearch",
            "echo",
            &[],
            &[CommandArgMapping::Positional {
                param: "q".to_string(),
            }],
            &[],
            &ContentType::ToolOutput,
            &HashMap::new(),
            &defs,
            &params,
            None,
            None,
            10,
        )
        .await
        .unwrap();
        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };
        assert!(output.contains("Dhurandhar 2"));
    }

    #[tokio::test]
    async fn provider_execute_uses_pack_implementation_and_resolves_defaults() {
        let def = CapabilityPackDefinition {
            name: "websearch".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![
                ParameterDef {
                    name: "query".to_string(),
                    required: true,
                    default: None,
                    description: None,
                    param_type: Some(ParameterType::String),
                    aliases: vec![],
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
                ParameterDef {
                    name: "num_results".to_string(),
                    required: false,
                    default: Some("10".to_string()),
                    description: None,
                    param_type: Some(ParameterType::Integer),
                    aliases: vec![],
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
            ],
            implementation: ImplementationType::Command {
                program: "echo".to_string(),
                fixed_args: vec![],
                arg_mappings: vec![
                    CommandArgMapping::Positional {
                        param: "query".to_string(),
                    },
                    CommandArgMapping::Positional {
                        param: "num_results".to_string(),
                    },
                ],
                suffix_args: vec![],
                content_type: ContentType::ToolOutput,
                env: HashMap::new(),
            },
            execution: Some(ExecutionMetadata {
                requires_browser_session: Some(false),
                default_timeout_secs: Some(15),
                chat_inline_adapter: None,
                categories: vec![],
                sandbox: None,
                composition_category: None,
                spend: None,
            }),
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let provider = PackCapabilityProvider::new(def, Arc::new(CapabilityRegistry::new()), None);
        let action = ExecutableAction::Pack {
            capability_name: "websearch".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "websearch".to_string(),
            },
            resolved_params: HashMap::from([(
                "query".to_string(),
                serde_json::json!("Dhurandhar 2"),
            )]),
        };

        let result = provider.execute(&action, None, 10).await.unwrap();
        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };

        assert!(output.contains("Dhurandhar 2"));
        assert!(output.contains("10"));
    }

    #[tokio::test]
    async fn provider_execute_flattens_nested_body_json_for_command_packs() {
        let def = CapabilityPackDefinition {
            name: "news_search_via_tavily".to_string(),
            description: None,
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: vec![
                ParameterDef {
                    name: "query".to_string(),
                    required: true,
                    default: None,
                    description: None,
                    param_type: Some(ParameterType::String),
                    aliases: vec![],
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
                ParameterDef {
                    name: "search_depth".to_string(),
                    required: false,
                    default: Some("basic".to_string()),
                    description: None,
                    param_type: Some(ParameterType::String),
                    aliases: vec![],
                    enum_values: None,
                    schema: serde_json::Value::Null,
                },
            ],
            implementation: ImplementationType::Command {
                program: "echo".to_string(),
                fixed_args: vec![],
                arg_mappings: vec![
                    CommandArgMapping::Positional {
                        param: "query".to_string(),
                    },
                    CommandArgMapping::Positional {
                        param: "search_depth".to_string(),
                    },
                ],
                suffix_args: vec![],
                content_type: ContentType::ToolOutput,
                env: HashMap::new(),
            },
            execution: Some(ExecutionMetadata {
                requires_browser_session: Some(false),
                default_timeout_secs: Some(15),
                chat_inline_adapter: None,
                categories: vec![],
                sandbox: None,
                composition_category: None,
                spend: None,
            }),
            auth: None,
            reliability: None,
            result_projection: None,
        };

        let provider = PackCapabilityProvider::new(def, Arc::new(CapabilityRegistry::new()), None);
        let action = ExecutableAction::Pack {
            capability_name: "news_search_via_tavily".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "news_search_via_tavily".to_string(),
            },
            resolved_params: HashMap::from([(
                "body".to_string(),
                serde_json::json!(
                    r#"{"query":"Iran latest developments","search_depth":"advanced"}"#
                ),
            )]),
        };

        let result = provider.execute(&action, None, 10).await.unwrap();
        let output = match result {
            ActionResult::Text { content, .. } => content,
            _ => panic!("Expected Text result"),
        };

        assert!(output.contains("Iran latest developments"));
        assert!(output.contains("advanced"));
    }
}
