//! ThinkingMapsDataProvider — the scoped thinking-map host-read binder
//! (the 2.5 learning-read pattern generalized).
//!
//! The Phase 4 Brainstorm verdict found that a custom-surface package cannot
//! read first-party thinking-map data: the eight contract operations serve
//! only the package's own entity store, and the one host-read binder in
//! existence was 2.5's two narrowly-scoped `internal_data` learning reads.
//! This provider is that verdict's recorded re-open condition landing: the
//! same shape — exactly two bounded read actions (`list_maps`/`read_map`),
//! executor-owned runtime scope, and a fail-closed argument proof — over the
//! thinking-map substrate.
//!
//! Reaching the store from this crate: `magician-surfaces` (where
//! `ThinkingMapStore` lives) depends on `magician`, so the dependency
//! direction forbids constructing that store type here. The store is,
//! however, a durable on-disk contract over [`ArtifactV2Workspace`] —
//! `scopes/<principal>/<workspace>/thinking_maps/<map_id>/{manifest.json,
//! snapshot.json}` — and its snapshot type IS this crate's [`ThinkingMap`]
//! (`magician-surfaces` re-exports `magician_v2::thinking_map_models` as its
//! own models). This provider therefore performs the same bounded reads the
//! first-party REST handlers (`thinking_maps_api.rs` list/get) perform,
//! directly over the same durable artifacts with the same wire types: the
//! substrate stays the single source and this is a projection of it, never
//! a second authority. Nothing here writes: map mutation stays on the
//! first-party API.
//!
//! Fail-closed posture (mirrors `internal_data_provider.rs` plan 2.5): the
//! app bind kernel admits exactly the two actions; the argument proof below
//! closes their parameter surface; the runtime scope never passes through
//! the model — `__principal`/`__workspace` are executor-owned and
//! re-verified here before any store read; and every path component is
//! scope-id validated before it is joined, so a hostile `map_id` cannot
//! traverse outside the scope's `thinking_maps/` root.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::ArtifactV2Error;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;
use crate::magician_v2::strategy::plan::PlanStep;
use crate::magician_v2::thinking_map_models::{MapLifecycle, ThinkingMap};

pub const THINKING_MAPS_DATA_TOOL_NAME: &str = "thinking_maps_data";

/// Default page size for the app-bound `list_maps` read.
const DEFAULT_MAP_LIST_LIMIT: usize = 10;

/// Maximum number of map directories one `list_maps` call will scan
/// (manifest-head reads included). The returned page is already bounded by
/// the caller's `limit`; this bounds the *scan work* instead, so a scope
/// with a huge number of directories cannot turn one bounded read into an
/// unbounded enumeration. Directories are considered in deterministic
/// map-id order, and a scope over the budget flags its result with the
/// additive `scan_truncated` key — the page stays correct over the scanned
/// prefix, and callers can tell a full listing from a capped one.
const LIST_MAPS_SCAN_BUDGET: usize = 2048;

/// The scan budget above is a *window*, not a wall: an `after_map_id`
/// cursor starts the scan strictly past the cursor in the same
/// deterministic map-id order and the budget applies to the remaining
/// entries, so a caller pages through any scope in budget-sized windows
/// (`next_cursor` in the result names the resume point whenever the window
/// was capped). Without the cursor the behavior is the historical one.

/// The closed lifecycle vocabulary the app-bound list filter accepts. It
/// mirrors `MapLifecycle`'s serde snake_case spellings; an unknown lifecycle
/// fails the proof rather than being passed to the store as a filter that
/// silently matches nothing.
const APP_THINKING_MAP_READ_LIFECYCLES: &[&str] = &["active", "paused", "archived", "deleted"];

const APP_THINKING_MAP_READ_MAX_LIMIT: u64 = 25;
const APP_THINKING_MAP_READ_MAX_MAP_ID_BYTES: usize = 128;

#[derive(Clone)]
pub struct ThinkingMapsDataProvider {
    workspace_layout: ArtifactV2Workspace,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for ThinkingMapsDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThinkingMapsDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl ThinkingMapsDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for ThinkingMapsDataProvider {
    fn tool_name(&self) -> &str {
        THINKING_MAPS_DATA_TOOL_NAME
    }

    /// App-path argument proof (the 2.5 pattern over the thinking-map
    /// substrate). The app bind kernel admits exactly the two bounded read
    /// actions; this closes their parameter surface: one action selector,
    /// an optional lifecycle filter from the closed lifecycle set and an
    /// optional bounded page limit for the list, or one exact map id for
    /// the read. Anything else — other actions, unexpected keys, wrong
    /// types, oversized values — fails closed. The runtime scope never
    /// passes through here: `__principal`/`__workspace` are executor-owned
    /// and re-verified by `authorize_runtime_scope` before any store read.
    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_thinking_map_read_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "thinking_maps_data: unexpected action type".to_string(),
                ))
            },
        };

        // Same alias gap as internal_data/duckdb: the class was decided from
        // the planned operation (with the bounded list read as the
        // operation-less default), so route on the same key and never on a
        // silently broader default.
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "list_maps".to_string());
        let params = authorize_runtime_scope(params)?;
        let effective_timeout = timeout_secs.max(1);

        let value = timeout(
            Duration::from_secs(effective_timeout),
            execute_thinking_maps_data_action(&self.workspace_layout, &action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "thinking_maps_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;

        Ok(ActionResult::text(pretty_json(&value)))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

/// Argument proof for the app-bound thinking-map reads. Exactly one action,
/// a closed parameter surface, bounded values; every other key, type or
/// action is refused. Runtime-owned `__*` correlation fields pass through
/// uninterpreted — model-origin hidden keys were already stripped before
/// this proof runs, and `authorize_runtime_scope` re-derives the store scope
/// from the executor-owned values alone.
fn prove_app_thinking_map_read_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    let listed = operation == "list_maps";
    if !listed && operation != "read_map" {
        return false;
    }
    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                // An alias must agree with the routing key, tool-qualified
                // spellings included, or the call names two operations.
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        "thinking_maps_data",
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "lifecycle" if listed => {
                if value
                    .as_str()
                    .is_none_or(|lifecycle| !APP_THINKING_MAP_READ_LIFECYCLES.contains(&lifecycle))
                {
                    return false;
                }
            },
            "limit" if listed => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=APP_THINKING_MAP_READ_MAX_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            // Keyset cursor for the list page — same shape rules as
            // `map_id` (non-empty, bounded), validated as a scope id at
            // execution time like `read_map` validates its target.
            "after_map_id" if listed => {
                if value.as_str().is_none_or(|id| {
                    id.is_empty() || id.len() > APP_THINKING_MAP_READ_MAX_MAP_ID_BYTES
                }) {
                    return false;
                }
            },
            "map_id" if !listed => {
                if value.as_str().is_none_or(|id| {
                    id.is_empty() || id.len() > APP_THINKING_MAP_READ_MAX_MAP_ID_BYTES
                }) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    if !listed && parameters.get("map_id").is_none() {
        return false;
    }
    true
}

/// The manifest head fields the bounded list read projects. A strict subset
/// of the store's `MapManifest` wire shape (unknown fields are ignored by
/// serde), so manifest evolution cannot break the read while the list never
/// carries more than the summary the first-party list serves.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScopedMapManifestHead {
    map_id: String,
    title: String,
    lifecycle: MapLifecycle,
    latest_revision: u64,
    updated_at: String,
}

/// One bounded map summary: the manifest-head core of the store's
/// `MapSummary` (the optional library-card `node_preview` thumbnail is
/// deliberately omitted — it is a client rendering nicety, not substrate
/// data, and reproducing it here would fork the store's preview projection).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScopedMapSummary {
    map_id: String,
    title: String,
    lifecycle: MapLifecycle,
    latest_revision: u64,
    updated_at: String,
}

impl From<ScopedMapManifestHead> for ScopedMapSummary {
    fn from(manifest: ScopedMapManifestHead) -> Self {
        Self {
            map_id: manifest.map_id,
            title: manifest.title,
            lifecycle: manifest.lifecycle,
            latest_revision: manifest.latest_revision,
            updated_at: manifest.updated_at,
        }
    }
}

async fn execute_thinking_maps_data_action(
    workspace_layout: &ArtifactV2Workspace,
    action_name: &str,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    match action_name {
        "list_maps" => list_maps(workspace_layout, params, LIST_MAPS_SCAN_BUDGET).await,
        "read_map" => read_map(workspace_layout, params).await,
        other => Err(ExecutionError::Step(format!(
            "thinking_maps_data: unknown action `{other}`"
        ))),
    }
}

/// `scopes/<principal>/<workspace>/thinking_maps/` — the enumeration root,
/// identical to the store's own layout.
fn maps_root(workspace_layout: &ArtifactV2Workspace, principal: &str, workspace: &str) -> PathBuf {
    workspace_layout
        .scope_root(principal, workspace)
        .join("thinking_maps")
}

async fn read_manifest_head(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    map_id: &str,
) -> Result<Option<ScopedMapManifestHead>, ExecutionError> {
    let path = maps_root(workspace_layout, principal, workspace)
        .join(map_id)
        .join("manifest.json");
    let body = match workspace_layout.read_to_string_path(&path).await {
        Ok(body) => body,
        Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(err) => {
            return Err(ExecutionError::Step(format!(
                "thinking_maps_data list_maps: manifest read failed: {err}"
            )))
        },
    };
    // The store quarantines corrupt manifests (store.rs:
    // "Ok(None) | Err(Corrupt) => continue"); match it so one bad
    // manifest cannot fail the whole list page.
    match serde_json::from_str(&body) {
        Ok(head) => Ok(Some(head)),
        Err(_) => Ok(None),
    }
}

/// One bounded page of scoped map summaries — the same visible-map
/// semantics, ordering (updated_at desc, map_id asc tie-break) and
/// corruption handling the store's `list_maps`/`list_maps_by_lifecycle`
/// serve the first-party library view: deleted maps are durable tombstones,
/// hidden from the default page and selected only by an exact
/// `lifecycle=deleted` filter; unreadable/corrupt manifests are skipped
/// (quarantined) rather than failing the whole page.
///
/// The scan itself is budgeted to `scan_budget` directories (production
/// passes `LIST_MAPS_SCAN_BUDGET`): unsafe directory names are dropped
/// first (they cannot spend scan budget), the survivors are collected and
/// sorted deterministically, and only that prefix has its manifest head
/// read. An optional `after_map_id` cursor moves the prefix start strictly
/// past the cursor (keyset pagination: the budget then applies to the
/// remaining entries, so paging walks the whole scope and nothing is
/// unreachable behind the cap). An over-budget window returns a correct
/// page over what it scanned, adds the additive `"scan_truncated": true`
/// key, and — the resume point — `"next_cursor"`, the last map id the
/// window scanned; the under-budget, no-cursor result shape is unchanged,
/// and the app-path result ceiling still applies to the returned bytes
/// downstream either way.
async fn list_maps(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
    scan_budget: usize,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let lifecycle = match string_param(params, "lifecycle") {
        None => None,
        Some(value) => match parse_lifecycle_if_known(value.as_str()) {
            Some(known) => Some(known),
            // An unknown lifecycle silently widening to the default
            // (non-deleted) page would return different data than
            // requested — refuse instead (the app path already does).
            None => {
                return Err(ExecutionError::Step(format!(
                    "thinking_maps_data list_maps: unknown lifecycle filter `{}`",
                    value.as_str()
                )))
            },
        },
    };
    let limit = bounded_usize(params, "limit", DEFAULT_MAP_LIST_LIMIT);
    let root = maps_root(workspace_layout, &scope.principal, &scope.workspace);
    let entries = workspace_layout
        .read_dir_path_or_empty(&root)
        .await
        .map_err(|err| ExecutionError::Step(format!("thinking_maps_data list_maps: {err}")))?;

    // Bound the scan, not just the page: collect the directory names, drop
    // the unsafe ones before they can spend any budget, sort the rest
    // deterministically, and only read manifest heads for the scan-budget
    // prefix. Which maps a capped page can see is therefore decided by
    // map-id order, not by recency — the flag below makes that visible
    // instead of silent.
    let mut map_dirs: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.is_dir)
        .map(|entry| entry.file_name)
        // Defense-in-depth, same as the store: a stray directory with an
        // unsafe name is skipped, never joined into a path. Filtering here —
        // before sort and truncate — also keeps it from consuming scan
        // budget a safe map could otherwise have used.
        .filter(|map_id| is_safe_scope_id(map_id))
        .collect();
    map_dirs.sort();
    // A zero budget could never make progress (the window would be empty
    // and the resume point ambiguous), so floor it at one entry.
    let scan_budget = scan_budget.max(1);
    // Keyset cursor: drop everything at or before `after_map_id` in the
    // sorted order. `partition_point` over the sorted names needs no
    // membership check, so a cursor naming a deleted (or never-existing)
    // map still lands on the right resume position instead of erroring.
    let start = match string_param(params, "after_map_id") {
        None => 0,
        Some(after_map_id) => {
            if !is_safe_scope_id(&after_map_id) {
                return Err(ExecutionError::Step(
                    "thinking_maps_data list_maps: after_map_id failed the scope-id safety check"
                        .to_string(),
                ));
            }
            map_dirs.partition_point(|map_id| map_id.as_str() <= after_map_id.as_str())
        },
    };
    let scan_truncated = map_dirs.len() - start > scan_budget;
    let next_cursor = if scan_truncated {
        map_dirs.get(start + scan_budget - 1).cloned()
    } else {
        None
    };
    map_dirs.drain(..start);
    map_dirs.truncate(scan_budget);

    let mut summaries = Vec::new();
    for map_id in map_dirs {
        let Some(manifest) = read_manifest_head(
            workspace_layout,
            &scope.principal,
            &scope.workspace,
            &map_id,
        )
        .await?
        else {
            continue;
        };
        let matches = match lifecycle {
            Some(expected) => manifest.lifecycle == expected,
            None => manifest.lifecycle != MapLifecycle::Deleted,
        };
        if !matches {
            continue;
        }
        summaries.push(ScopedMapSummary::from(manifest));
    }
    summaries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.map_id.cmp(&b.map_id))
    });
    summaries.truncate(limit);
    let mut result = json!({
        "scope": scope.as_json(),
        "count": summaries.len(),
        "maps": summaries
    });
    // Additive and only present when the scan budget was hit, so the
    // under-budget result shape stays exactly what it always was. The
    // cursor names the last id the window scanned — the caller's resume
    // point for the next window.
    if scan_truncated {
        result["scan_truncated"] = json!(true);
        if let Some(cursor) = next_cursor {
            result["next_cursor"] = json!(cursor);
        }
    }
    Ok(result)
}

/// One exact scoped map snapshot — the full typed map at its current
/// revision, deserialized as this crate's own `ThinkingMap` (the exact type
/// the store persists, since `magician-surfaces` re-exports it as its model
/// core). The result is wire-identical to the first-party GET response.
async fn read_map(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let map_id = required_string(params, "map_id")?;
    if !is_safe_scope_id(&map_id) {
        return Err(ExecutionError::Step(
            "thinking_maps_data read_map: map_id failed the scope-id safety check".to_string(),
        ));
    }
    let path = maps_root(workspace_layout, &scope.principal, &scope.workspace)
        .join(&map_id)
        .join("snapshot.json");
    let body = match workspace_layout.read_to_string_path(&path).await {
        Ok(body) => body,
        Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ExecutionError::Step(format!(
                "thinking_maps_data read_map: thinking map not found: {map_id}"
            )));
        },
        Err(err) => {
            return Err(ExecutionError::Step(format!(
                "thinking_maps_data read_map: snapshot read failed: {err}"
            )))
        },
    };
    let map: ThinkingMap = serde_json::from_str(&body).map_err(|err| {
        ExecutionError::Step(format!(
            "thinking_maps_data read_map: corrupt snapshot for `{map_id}`: {err}"
        ))
    })?;
    serde_json::to_value(map).map_err(|err| {
        ExecutionError::Step(format!(
            "thinking_maps_data read_map: snapshot serialization failed: {err}"
        ))
    })
}

fn parse_lifecycle_if_known(value: &str) -> Option<MapLifecycle> {
    APP_THINKING_MAP_READ_LIFECYCLES
        .iter()
        .find(|candidate| **candidate == value)
        .map(|candidate| match *candidate {
            "paused" => MapLifecycle::Paused,
            "archived" => MapLifecycle::Archived,
            "deleted" => MapLifecycle::Deleted,
            _ => MapLifecycle::Active,
        })
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({
            "principal": self.principal,
            "workspace": self.workspace
        })
    }
}

/// The store scope for one read: the executor-owned `__principal`/
/// `__workspace` values `authorize_runtime_scope` has already verified and
/// forced into the public compatibility fields. There is deliberately NO
/// default-scope fallback: missing scope is an error, so a future caller
/// that skips authorization fails closed here instead of silently reading
/// the default scope. (The 2.5 original in `internal_data_provider.rs`
/// still falls back to the default scope; the divergence is intentional
/// hardening in this provider, not drift.)
fn scope_from_params(params: &HashMap<String, Value>) -> Result<Scope, ExecutionError> {
    let missing = |key: &str| {
        ExecutionError::Step(format!(
            "thinking_maps_data requires the runtime-authorized scope `{key}`; \
             unscoped execution is denied"
        ))
    };
    let principal = string_param(params, "__principal")
        .or_else(|| string_param(params, "principal"))
        .ok_or_else(|| missing("__principal"))?;
    let workspace = string_param(params, "__workspace")
        .or_else(|| string_param(params, "workspace"))
        .ok_or_else(|| missing("__workspace"))?;
    Ok(Scope {
        principal,
        workspace,
    })
}

/// Runtime-scope authorization, mirroring `internal_data_provider`'s: the
/// executor-owned `__principal`/`__workspace` values are the only authority,
/// an unscoped call is denied, and any public `principal`/`workspace`
/// assertion must agree with them (it can never switch scope).
fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "thinking_maps_data runtime scope contains an unsafe principal or workspace component"
                .to_string(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        let Some(value) = params.get(public_key) else {
            continue;
        };
        let Value::String(value) = value else {
            return Err(ExecutionError::Step(format!(
                "thinking_maps_data: `{public_key}` is an optional scope assertion and must be a string"
            )));
        };
        if !value.is_empty() && value != trusted_value {
            return Err(ExecutionError::Step(format!(
                "thinking_maps_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
            )));
        }
    }
    // Preserve the public compatibility fields, but force them to the trusted
    // values so every helper sees one authority.
    params.insert("principal".to_string(), Value::String(principal));
    params.insert("workspace".to_string(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "thinking_maps_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "thinking_maps_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_string())
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn required_string(params: &HashMap<String, Value>, key: &str) -> Result<String, ExecutionError> {
    string_param(params, key).ok_or_else(|| {
        ExecutionError::Step(format!(
            "thinking_maps_data: `{key}` must be a non-empty string when supplied"
        ))
    })
}

fn bounded_usize(params: &HashMap<String, Value>, key: &str, default: usize) -> usize {
    params
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(default as u64)
        .min(APP_THINKING_MAP_READ_MAX_LIMIT) as usize
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The proof matrix mirrors 2.5's suite: the two actions admit their
    // closed surfaces; every other action, key, type or bound fails closed.
    #[test]
    fn app_thinking_map_read_proof_admits_only_the_closed_read_pair() {
        let list = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("lifecycle".to_string(), json!("active")),
            ("limit".to_string(), json!(25)),
        ]);
        assert!(prove_app_thinking_map_read_args(&list));
        // Tool-qualified alias spelling agrees with the routing key.
        let mut qualified = list.clone();
        qualified.insert(
            "operation".to_string(),
            json!("thinking_maps_data__list_maps"),
        );
        assert!(prove_app_thinking_map_read_args(&qualified));
        // A list without the optional filters is still closed.
        let bare_list = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
        ]);
        assert!(prove_app_thinking_map_read_args(&bare_list));
        let read = HashMap::from([
            ("__action_name".to_string(), json!("read_map")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("map_id".to_string(), json!("map-1")),
        ]);
        assert!(prove_app_thinking_map_read_args(&read));

        // Any other action stays refused even with a perfect scope envelope.
        for action in ["catalog", "create_map", "apply_operations", "delete_map"] {
            let mut other = list.clone();
            other.insert("__action_name".to_string(), json!(action));
            assert!(!prove_app_thinking_map_read_args(&other), "{action}");
        }
        // Unknown lifecycle, oversized limit, wrong types, unexpected keys,
        // an alias naming a second operation, a missing map id, and a
        // missing action selector all fail closed.
        let mut bad_lifecycle = list.clone();
        bad_lifecycle.insert("lifecycle".to_string(), json!("guessed"));
        assert!(!prove_app_thinking_map_read_args(&bad_lifecycle));
        let mut bad_limit = list.clone();
        bad_limit.insert("limit".to_string(), json!(26));
        assert!(!prove_app_thinking_map_read_args(&bad_limit));
        let mut string_limit = list.clone();
        string_limit.insert("limit".to_string(), json!("10"));
        assert!(!prove_app_thinking_map_read_args(&string_limit));
        let mut extra_key = list.clone();
        extra_key.insert("node_preview".to_string(), json!(true));
        assert!(!prove_app_thinking_map_read_args(&extra_key));
        let mut divergent = list.clone();
        divergent.insert("operation".to_string(), json!("read_map"));
        assert!(!prove_app_thinking_map_read_args(&divergent));
        let mut no_id = read.clone();
        no_id.remove("map_id");
        assert!(!prove_app_thinking_map_read_args(&no_id));
        assert!(!prove_app_thinking_map_read_args(&HashMap::new()));
        // A read cannot carry the list filters, and a list cannot carry the
        // read selector.
        let mut mixed = read.clone();
        mixed.insert("lifecycle".to_string(), json!("active"));
        assert!(!prove_app_thinking_map_read_args(&mixed));
        let mut mixed_list = list.clone();
        mixed_list.insert("map_id".to_string(), json!("map-1"));
        assert!(!prove_app_thinking_map_read_args(&mixed_list));
        // A hostile map id is refused by length before any path is built.
        let mut oversized_id = read.clone();
        oversized_id.insert("map_id".to_string(), json!("x".repeat(129)));
        assert!(!prove_app_thinking_map_read_args(&oversized_id));
        // The keyset cursor is a list-only parameter with map_id's shape
        // rules: valid on a list, refused on a read, refused empty,
        // non-string, or oversized.
        let mut cursored = list.clone();
        cursored.insert("after_map_id".to_string(), json!("map-bbb"));
        assert!(prove_app_thinking_map_read_args(&cursored));
        let mut cursor_on_read = read.clone();
        cursor_on_read.insert("after_map_id".to_string(), json!("map-bbb"));
        assert!(!prove_app_thinking_map_read_args(&cursor_on_read));
        for bad_cursor in [json!(""), json!("x".repeat(129)), json!(7)] {
            let mut refused = list.clone();
            refused.insert("after_map_id".to_string(), bad_cursor);
            assert!(!prove_app_thinking_map_read_args(&refused));
        }
    }

    // The population leg (the 2.5 execution test generalized): the two
    // admitted actions read the REAL thinking-map substrate — created here
    // through the first-party `ThinkingMapStore` over the same workspace —
    // through the same provider execution the app attested dispatch settles
    // into, with the executor-owned scope envelope and nothing else.
    #[tokio::test]
    async fn app_bound_thinking_map_reads_return_real_store_maps_in_scope() {
        // Seed through the same durable file layout the provider reads
        // (manifest + snapshot under thinking_maps/<id>/). Using the
        // dev-dep ThinkingMapStore here would cross crate configurations
        // (magician compiled twice); writing the files directly exercises
        // the same read path without that hazard.

        fn seed_map(
            workspace: &ArtifactV2Workspace,
            map_id: &str,
            title: &str,
            lifecycle: MapLifecycle,
            revision: u64,
            updated_at: &str,
        ) {
            let base = workspace
                .scope_root("owner", "default")
                .join("thinking_maps")
                .join(map_id);
            std::fs::create_dir_all(&base).expect("create map dir");
            let manifest = serde_json::json!({
                "map_id": map_id,
                "title": title,
                "lifecycle": lifecycle,
                "latest_revision": revision,
                "updated_at": updated_at,
            });
            std::fs::write(
                base.join("manifest.json"),
                serde_json::to_string(&manifest).expect("serialize manifest"),
            )
            .expect("write manifest");
            let mut snapshot = ThinkingMap::new(
                map_id.to_string(),
                "owner",
                "default",
                title,
                crate::magician_v2::thinking_map_models::ThinkingMapSource::Solo,
                "2026-08-26T00:00:00Z",
            );
            snapshot.revision = revision;
            snapshot.lifecycle = lifecycle;
            snapshot.updated_at = updated_at.to_string();
            std::fs::write(
                base.join("snapshot.json"),
                serde_json::to_string(&snapshot).expect("serialize snapshot"),
            )
            .expect("write snapshot");
        }

        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());

        seed_map(
            &workspace,
            "alpha",
            "Design review",
            MapLifecycle::Active,
            3,
            "2026-08-26T01:00:00Z",
        );
        seed_map(
            &workspace,
            "beta",
            "Pricing brainstorm",
            MapLifecycle::Active,
            1,
            "2026-08-26T00:30:00Z",
        );
        // A soft-deleted tombstone: durable, directly loadable, but hidden
        // from the default visible page.
        seed_map(
            &workspace,
            "gone",
            "Abandoned map",
            MapLifecycle::Deleted,
            2,
            "2026-08-26T02:00:00Z",
        );

        let provider = ThinkingMapsDataProvider::new(workspace.clone());
        let list = ExecutableAction::Pack {
            capability_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: HashMap::from([
                ("__action_name".to_string(), json!("list_maps")),
                ("__principal".to_string(), json!("owner")),
                ("__workspace".to_string(), json!("default")),
                ("limit".to_string(), json!(10)),
            ]),
        };
        assert!(prove_app_thinking_map_read_args(&match &list {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => unreachable!("fixture is a pack action"),
        }));
        let ActionResult::Text { content } =
            provider.execute(&list, None, 5).await.expect("list action")
        else {
            panic!("list action returns text");
        };
        let payload: Value = serde_json::from_str(&content).expect("list result json");
        // The visible page hides the tombstone; alpha is most recently
        // updated, then beta.
        assert_eq!(payload["count"], json!(2));
        assert_eq!(payload["maps"][0]["map_id"], json!("alpha"));
        assert_eq!(payload["maps"][1]["map_id"], json!("beta"));
        assert_eq!(payload["maps"][0]["latest_revision"], json!(3));
        assert_eq!(payload["maps"][0]["lifecycle"], json!("active"));

        // The tombstone is selectable only through the exact lifecycle filter.
        let tombstones = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("lifecycle".to_string(), json!("deleted")),
        ]);
        let value = execute_thinking_maps_data_action(&workspace, "list_maps", &tombstones)
            .await
            .expect("tombstone page");
        assert_eq!(value["count"], json!(1));
        assert_eq!(value["maps"][0]["map_id"], json!("gone"));

        let read = ExecutableAction::Pack {
            capability_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: THINKING_MAPS_DATA_TOOL_NAME.to_string(),
            },
            resolved_params: HashMap::from([
                ("__action_name".to_string(), json!("read_map")),
                ("__principal".to_string(), json!("owner")),
                ("__workspace".to_string(), json!("default")),
                ("map_id".to_string(), json!("alpha")),
            ]),
        };
        let ActionResult::Text { content } =
            provider.execute(&read, None, 5).await.expect("read action")
        else {
            panic!("read action returns text");
        };
        let payload: Value = serde_json::from_str(&content).expect("read result json");
        // Wire-identical to the first-party GET: the store's own snapshot
        // round-trips through the same typed projection.
        assert_eq!(payload["map_id"], json!("alpha"));
        assert_eq!(payload["revision"], json!(3));
        assert_eq!(payload["title"], json!("Design review"));
        // The exact ThinkingMap round-trip is already pinned by the
        // phase0_wire_oracles' thinking-map operation tests; here the
        // field-level checks prove the provider reads the right file and
        // returns the right map.

        // A read outside the substrate is an error, not an empty success —
        // and a scope the store never wrote stays empty for the list.
        let ghost = HashMap::from([
            ("__action_name".to_string(), json!("read_map")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
            ("map_id".to_string(), json!("ghost")),
        ]);
        assert!(
            execute_thinking_maps_data_action(&workspace, "read_map", &ghost)
                .await
                .is_err()
        );
        let other_scope = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("someone-else")),
            ("__workspace".to_string(), json!("default")),
        ]);
        let value = execute_thinking_maps_data_action(&workspace, "list_maps", &other_scope)
            .await
            .expect("other scope lists empty");
        assert_eq!(value["count"], json!(0));

        // An unscoped call is denied before any read — and the scope
        // extractor itself carries no default-scope fallback: a caller
        // that skips authorization fails closed here instead of silently
        // reading the default scope.
        let unscoped = HashMap::from([("__action_name".to_string(), json!("list_maps"))]);
        assert!(authorize_runtime_scope(unscoped).is_err());
        assert!(scope_from_params(&HashMap::from([(
            "__action_name".to_string(),
            json!("list_maps")
        )]))
        .is_err());
    }

    /// The scan budget bounds the work, not the page: directory names are
    /// sorted deterministically and only the budget-sized prefix is
    /// scanned, so an over-budget scope flags its result with the additive
    /// `scan_truncated` key — while an under-budget listing is unchanged
    /// (same entries, same updated_at-desc/map_id-asc order, no flag key)
    /// and identical to what the production-budget execution returns.
    /// Unsafe directory names are dropped before the budget, so they can
    /// never crowd a safe map out of the scanned prefix.
    #[tokio::test]
    async fn list_maps_scan_budget_truncates_the_scan_and_flags_the_result() {
        // The production budget stays named, pinned, and large; the test
        // drives small budgets through `list_maps`'s explicit parameter.
        assert_eq!(LIST_MAPS_SCAN_BUDGET, 2048);

        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        // Manifest-only map dirs: the list read touches the manifest head
        // and nothing else. `map-zzz` is the most recently updated map but
        // sorts last by name, so a capped scan misses it — proving the cap
        // is on scan work in deterministic name order, not a recency page.
        for (map_id, updated_at) in [
            ("map-aaa", "2026-08-25T00:00:00Z"),
            ("map-bbb", "2026-08-26T05:00:00Z"),
            ("map-ccc", "2026-08-26T01:00:00Z"),
            ("map-zzz", "2026-08-26T06:00:00Z"),
        ] {
            let base = workspace
                .scope_root("owner", "default")
                .join("thinking_maps")
                .join(map_id);
            std::fs::create_dir_all(&base).expect("create map dir");
            let manifest = serde_json::json!({
                "map_id": map_id,
                "title": map_id,
                "lifecycle": "active",
                "latest_revision": 1,
                "updated_at": updated_at,
            });
            std::fs::write(
                base.join("manifest.json"),
                serde_json::to_string(&manifest).expect("serialize manifest"),
            )
            .expect("write manifest");
        }
        // A stray directory with an unsafe name (the tab makes
        // `is_safe_scope_id` refuse it) that sorts ahead of every safe map:
        // under a budget it must be dropped before truncation, not skipped
        // after it, or it would displace `map-aaa` from the scanned prefix.
        // Control characters in file names are only creatable through
        // `std::fs` on Unix, so the seeding is Unix-gated; on Windows the
        // test below then pins the all-safe byte-identical behavior.
        #[cfg(unix)]
        {
            let stray = workspace
                .scope_root("owner", "default")
                .join("thinking_maps")
                .join("\tstray-unsafe-map");
            std::fs::create_dir_all(&stray).expect("create unsafe-named map dir");
        }

        let params = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
        ]);

        // Over budget (2 < 4 safe directories): only the first two safe
        // names are scanned — the unsafe stray ahead of them does not
        // consume budget — the page stays correctly ordered, and the flag
        // says so. (With the stray counted, the prefix would hold it plus
        // `map-aaa` and the page would list one map.)
        let capped = list_maps(&workspace, &params, 2)
            .await
            .expect("capped page");
        assert_eq!(capped["count"], json!(2));
        assert_eq!(capped["maps"][0]["map_id"], json!("map-bbb"));
        assert_eq!(capped["maps"][1]["map_id"], json!("map-aaa"));
        assert_eq!(capped["scan_truncated"], json!(true));

        // Under budget: every map is found, in updated_at-desc order, and
        // the result carries no truncation flag — the historical shape.
        let full = list_maps(&workspace, &params, LIST_MAPS_SCAN_BUDGET)
            .await
            .expect("full page");
        assert_eq!(full["count"], json!(4));
        let ids: Vec<&str> = full["maps"]
            .as_array()
            .expect("maps array")
            .iter()
            .map(|map| map["map_id"].as_str().expect("map id"))
            .collect();
        assert_eq!(ids, vec!["map-zzz", "map-bbb", "map-ccc", "map-aaa"]);
        assert!(full.get("scan_truncated").is_none());

        // The production execution path passes the named budget, so its
        // under-budget result is exactly the full page above.
        let executed = execute_thinking_maps_data_action(&workspace, "list_maps", &params)
            .await
            .expect("executed page");
        assert_eq!(executed, full);
    }

    /// Keyset pagination walks a scope past the scan budget: the cursor
    /// moves the window start, the budget applies to the REMAINING entries,
    /// `next_cursor` names the resume point whenever the window was capped,
    /// and a cursor naming a map that no longer exists still lands on the
    /// right position (the sorted order, not membership, decides it).
    #[tokio::test]
    async fn list_maps_cursor_pages_past_the_scan_budget() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        for (map_id, updated_at) in [
            ("map-aaa", "2026-08-25T00:00:00Z"),
            ("map-bbb", "2026-08-26T05:00:00Z"),
            ("map-ccc", "2026-08-26T01:00:00Z"),
            ("map-zzz", "2026-08-26T06:00:00Z"),
        ] {
            let base = workspace
                .scope_root("owner", "default")
                .join("thinking_maps")
                .join(map_id);
            std::fs::create_dir_all(&base).expect("create map dir");
            std::fs::write(
                base.join("manifest.json"),
                serde_json::to_string(&serde_json::json!({
                    "map_id": map_id,
                    "title": map_id,
                    "lifecycle": "active",
                    "latest_revision": 1,
                    "updated_at": updated_at,
                }))
                .expect("serialize manifest"),
            )
            .expect("write manifest");
        }
        let base_params = HashMap::from([
            ("__action_name".to_string(), json!("list_maps")),
            ("__principal".to_string(), json!("owner")),
            ("__workspace".to_string(), json!("default")),
        ]);

        // Window one (no cursor): the first two names, correctly ordered
        // within the window, flagged, with the resume cursor.
        let page_one = list_maps(&workspace, &base_params, 2)
            .await
            .expect("first window");
        assert_eq!(page_one["count"], json!(2));
        assert_eq!(page_one["maps"][0]["map_id"], json!("map-bbb"));
        assert_eq!(page_one["maps"][1]["map_id"], json!("map-aaa"));
        assert_eq!(page_one["scan_truncated"], json!(true));
        assert_eq!(page_one["next_cursor"], json!("map-bbb"));

        // Window two (cursor = window one's resume point): the remaining
        // two names, unflagged — this page reached the end of the scope —
        // and no cursor key.
        let mut params = base_params.clone();
        params.insert("after_map_id".to_string(), json!("map-bbb"));
        let page_two = list_maps(&workspace, &params, 2)
            .await
            .expect("second window");
        assert_eq!(page_two["count"], json!(2));
        assert_eq!(page_two["maps"][0]["map_id"], json!("map-zzz"));
        assert_eq!(page_two["maps"][1]["map_id"], json!("map-ccc"));
        assert!(page_two.get("scan_truncated").is_none());
        assert!(page_two.get("next_cursor").is_none());

        // Past the end: an empty page, unflagged.
        let mut params = base_params.clone();
        params.insert("after_map_id".to_string(), json!("map-zzz"));
        let page_three = list_maps(&workspace, &params, 2)
            .await
            .expect("past the end");
        assert_eq!(page_three["count"], json!(0));
        assert!(page_three.get("scan_truncated").is_none());

        // A cursor naming a map that does not exist still positions by the
        // sorted order (no membership check, no error).
        let mut params = base_params.clone();
        params.insert("after_map_id".to_string(), json!("map-bbx"));
        let ghost = list_maps(&workspace, &params, 2)
            .await
            .expect("ghost cursor window");
        assert_eq!(ghost["count"], json!(2));
        assert_eq!(ghost["maps"][0]["map_id"], json!("map-zzz"));
        assert_eq!(ghost["maps"][1]["map_id"], json!("map-ccc"));

        // An unsafe cursor is refused with the same posture as an unsafe
        // map_id on read_map — it is never used to build anything, but the
        // input surface stays closed.
        let mut params = base_params.clone();
        params.insert("after_map_id".to_string(), json!("../evil"));
        let refused = list_maps(&workspace, &params, 2).await;
        assert!(refused.is_err());

        // The production execution path carries the cursor end to end: its
        // under-budget cursor page is exactly the direct call's.
        let mut params = base_params.clone();
        params.insert("after_map_id".to_string(), json!("map-bbb"));
        let executed = execute_thinking_maps_data_action(&workspace, "list_maps", &params)
            .await
            .expect("executed cursor page");
        let direct = list_maps(&workspace, &params, LIST_MAPS_SCAN_BUDGET)
            .await
            .expect("direct cursor page");
        assert_eq!(executed, direct);
        assert_eq!(executed["count"], json!(2));
    }
}
