//! Shared bind + contain + receipt kernel for app tool dispatch.
//!
//! Lock and grant only name a tool. App execution must still close the *call*:
//! classify the IO, require a wired binder for that class, require a wired
//! contain profile for the implementation, then mint a one-shot receipt.
//! New tools of an existing class reuse this kernel. New Rust is for a new
//! IO class or contain profile, not for another youtube-search-shaped skill.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tool_runtime_core::manifest::RuntimeProtocol;
use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

use super::models::{AppDigest, AppReference};
use super::tool_disclosure::AttestedAppToolTarget;
use crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref;
use crate::magician_v2::execution::CapabilityPackDefinition;

/// How this call would touch the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppToolIoKind {
    PureTransform,
    TrustedLocalClock,
    BoundHttp,
    BoundFile,
    BoundWrite,
    BoundTable,
    BoundHostRead,
    BoundSideEffect,
    Device,
    Unbound,
}

/// Where the implementation is allowed to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppToolContainProfile {
    /// Compiled provider in this process. Wired today.
    InProcessCompiled,
    /// Skillshub / CLI / USR. Needs an OS jail before it may run in apps.
    OsJail,
    /// Official-SDK remote MCP. Checkout, OAuth, and INR settlement stay in
    /// `dispatch_governed_mcp`; apps do not wrap this in OS-jail.
    GovernedMcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AppToolBindPlan {
    pub tool_name: String,
    pub operation: Option<String>,
    pub io_kind: AppToolIoKind,
    pub contain: AppToolContainProfile,
    pub runnable: bool,
    pub attested_operations: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AppToolDispatchNote {
    pub dispatchable: bool,
    pub attested_operations: Vec<String>,
    pub io_kind: AppToolIoKind,
    pub contain: AppToolContainProfile,
    pub reason: String,
}

/// Reviewed transient input envelope for the scoped learning-substrate read
/// (plan 2.5): one action selector plus a bounded state filter and page size,
/// or one exact candidate id.
pub(crate) const APP_BOUND_LEARNING_READ_INPUT_CEILING: u64 = 2 * 1024;
/// The two admitted learning read actions return one bounded page of learning
/// candidates (or one candidate with its decision log). A page whose
/// serialized form exceeds this ceiling fails closed at result settlement
/// instead of truncating mid-record; the reviewed projection bounds `limit`
/// well below the agent-facing pack ceiling so ordinary pages settle.
pub(crate) const APP_BOUND_LEARNING_READ_RESULT_CEILING: u64 = 512 * 1024;

/// Reviewed transient input envelope for the scoped thinking-map reads (the
/// Phase 4 Brainstorm verdict's re-open condition — plan 2.5's learning-read
/// pattern generalized): one action selector plus an optional lifecycle
/// filter and page size, or one exact map id.
pub(crate) const APP_BOUND_THINKING_MAP_READ_INPUT_CEILING: u64 = 2 * 1024;
/// The two admitted thinking-map read actions return one bounded page of map
/// summaries or one full map snapshot. A result whose serialized form
/// exceeds this ceiling fails closed at result settlement instead of
/// truncating mid-record; the reviewed projection bounds `limit` well below
/// the agent-facing REST page shape so ordinary pages settle.
pub(crate) const APP_BOUND_THINKING_MAP_READ_RESULT_CEILING: u64 = 512 * 1024;

/// Reviewed transport envelopes for the five-action claims/evidence/entity/
/// commitment host-read binder. The provider enforces the result ceiling too,
/// so ordinary agent dispatch and Apps settlement fail the same way.
pub(crate) const APP_BOUND_EVIDENCE_DATA_INPUT_CEILING: u64 =
    crate::magician_v2::execution::evidence_data_provider::APP_BOUND_EVIDENCE_DATA_INPUT_CEILING;
pub(crate) const APP_BOUND_EVIDENCE_DATA_RESULT_CEILING: u64 =
    crate::magician_v2::execution::evidence_data_provider::APP_BOUND_EVIDENCE_DATA_RESULT_CEILING;

/// Reviewed transport envelopes for the six-action meetings host-read binder
/// (live capture state, thread index, transcript pages, takeaways, calendar
/// context, keyword retrieval). The provider enforces the result ceiling too,
/// so ordinary agent dispatch and Apps settlement fail the same way.
pub(crate) const APP_BOUND_MEETINGS_DATA_INPUT_CEILING: u64 =
    crate::magician_v2::execution::meetings_data_provider::APP_BOUND_MEETINGS_DATA_INPUT_CEILING;
pub(crate) const APP_BOUND_MEETINGS_DATA_RESULT_CEILING: u64 =
    crate::magician_v2::execution::meetings_data_provider::APP_BOUND_MEETINGS_DATA_RESULT_CEILING;

/// Reviewed transport envelopes for the two-action agent-roster host-read
/// binder. Narrower than its siblings because the projection is narrower: a
/// bounded roster page of identity plus three persona fields.
pub(crate) const APP_BOUND_AGENT_ROSTER_INPUT_CEILING: u64 =
    crate::magician_v2::execution::agent_roster_data_provider::APP_BOUND_AGENT_ROSTER_INPUT_CEILING;
pub(crate) const APP_BOUND_AGENT_ROSTER_RESULT_CEILING: u64 =
    crate::magician_v2::execution::agent_roster_data_provider::APP_BOUND_AGENT_ROSTER_RESULT_CEILING;

/// Reviewed transport envelopes for the two-action task-list host-read
/// binder: one bounded page of narrow task rows, or one task's bounded face.
pub(crate) const APP_BOUND_TASKS_DATA_INPUT_CEILING: u64 =
    crate::magician_v2::execution::tasks_data_provider::APP_BOUND_TASKS_DATA_INPUT_CEILING;
pub(crate) const APP_BOUND_TASKS_DATA_RESULT_CEILING: u64 =
    crate::magician_v2::execution::tasks_data_provider::APP_BOUND_TASKS_DATA_RESULT_CEILING;

/// Reviewed transport envelopes for the two-action notes host-read binder:
/// bounded search hits, or one note's markdown (store-capped at 256 KiB).
pub(crate) const APP_BOUND_NOTES_DATA_INPUT_CEILING: u64 =
    crate::magician_v2::execution::notes_data_provider::APP_BOUND_NOTES_DATA_INPUT_CEILING;
pub(crate) const APP_BOUND_NOTES_DATA_RESULT_CEILING: u64 =
    crate::magician_v2::execution::notes_data_provider::APP_BOUND_NOTES_DATA_RESULT_CEILING;

/// Reviewed transport envelopes for the two-action owner-granted memory
/// host-read binder (`app_memory_read_v1`).
pub(crate) const APP_BOUND_MEMORY_DATA_INPUT_CEILING: u64 =
    crate::magician_v2::execution::memory_data_provider::APP_BOUND_MEMORY_DATA_INPUT_CEILING;
pub(crate) const APP_BOUND_MEMORY_DATA_RESULT_CEILING: u64 =
    crate::magician_v2::execution::memory_data_provider::APP_BOUND_MEMORY_DATA_RESULT_CEILING;

/// The compiled app effect owner is an explicit allow-list, not a consequence
/// of a YAML category. A provider becomes app-runnable only after it opts into
/// exact argument proof and a centrally versioned implementation identity.
/// Provider/lowering behavior changes must rotate that identity; unrelated
/// binary relinks do not invalidate every installed package lock.
pub(crate) fn compiled_app_provider_implementation_identity(
    tool_name: &str,
) -> Option<&'static str> {
    match normalized_tool_name(tool_name) {
        "time_math" => Some("magician.compiled-provider.time-math.v2"),
        "http" => Some("magician.compiled-provider.bound-http-get.v1"),
        "files" => Some("magician.compiled-provider.bound-file-read-write.v1"),
        "duckdb" => Some("magician.compiled-provider.bound-table-preview-describe.v1"),
        "internal_data" => Some("magician.compiled-provider.internal-data-learning-read.v1"),
        // The second host-read owner (Phase 4 Brainstorm re-open): the 2.5
        // learning-read shape over the thinking-map substrate. A separate
        // identity from `internal_data`'s so widening the thinking-map reads
        // can never rotate an installed learning-read lock, and vice versa.
        "thinking_maps_data" => Some("magician.compiled-provider.thinking-maps-read.v1"),
        "evidence_data" => Some("magician.compiled-provider.evidence-data-read.v1"),
        // The fourth host-read owner: the meeting rails. A separate identity
        // again, so widening a meetings read can never rotate an installed
        // claims-review or learning-read lock.
        "meetings_data" => Some("magician.compiled-provider.meetings-data-read.v1"),
        // The fifth host-read owner: the agent roster's participation face.
        // Its own identity again, so widening a roster read cannot rotate an
        // installed lock for any other binder.
        "agent_roster_data" => Some("magician.compiled-provider.agent-roster-read.v1"),
        // The sixth host-read owner: the owner's task list, narrow face. Its
        // own identity, so widening it cannot rotate another binder's lock.
        "tasks_data" => Some("magician.compiled-provider.tasks-read.v1"),
        // The seventh host-read owner: notes search and read, no host paths.
        "notes_data" => Some("magician.compiled-provider.notes-read.v1"),
        // The eighth host-read owner: owner-granted memory reads for apps.
        "memory_data" => Some("magician.compiled-provider.memory-read.v1"),
        // catchup_merge currently emits runtime duration bytes, so it is not a
        // pure transform and deliberately has no app implementation identity.
        _ => None,
    }
}

/// Host-reviewed transient input envelope. This is intentionally independent
/// of persistent app-store storage and becomes part of the implementation-plan
/// digest below. New operations remain inert until they have a finite bound.
pub(crate) fn reviewed_app_transport_input_ceiling(plan: &AppToolBindPlan) -> Option<u64> {
    match (
        normalized_tool_name(&plan.tool_name),
        plan.operation.as_deref(),
    ) {
        ("time_math", Some("now")) | ("time_math", Some("date_range")) => Some(8 * 1024),
        ("http", None | Some("get")) => {
            Some(crate::magician_v2::apps::bound_http::APP_BOUND_HTTP_INPUT_CEILING)
        },
        ("files", None | Some("read" | "write")) => {
            Some(super::bound_path::APP_BOUND_FILE_INPUT_CEILING)
        },
        ("duckdb", Some("preview" | "describe")) => {
            Some(super::bound_path::APP_BOUND_TABLE_INPUT_CEILING)
        },
        ("internal_data", None | Some("list_learning_candidates" | "read_learning_candidate")) => {
            Some(APP_BOUND_LEARNING_READ_INPUT_CEILING)
        },
        ("thinking_maps_data", None | Some("list_maps" | "read_map")) => {
            Some(APP_BOUND_THINKING_MAP_READ_INPUT_CEILING)
        },
        (
            "evidence_data",
            None
            | Some(
                "list_pending_claims"
                | "read_claim"
                | "list_evidence_records"
                | "list_entities"
                | "list_commitments",
            ),
        ) => Some(APP_BOUND_EVIDENCE_DATA_INPUT_CEILING),
        (
            "meetings_data",
            None
            | Some(
                "active_session"
                | "list_threads"
                | "read_thread"
                | "read_takeaways"
                | "upcoming_meetings"
                | "search_meeting_memory",
            ),
        ) => Some(APP_BOUND_MEETINGS_DATA_INPUT_CEILING),
        ("agent_roster_data", None | Some("list_members" | "read_member")) => {
            Some(APP_BOUND_AGENT_ROSTER_INPUT_CEILING)
        },
        ("tasks_data", None | Some("list_tasks" | "read_task")) => {
            Some(APP_BOUND_TASKS_DATA_INPUT_CEILING)
        },
        ("notes_data", None | Some("search_notes" | "read_note")) => {
            Some(APP_BOUND_NOTES_DATA_INPUT_CEILING)
        },
        ("memory_data", None | Some("search_memory" | "read_entry")) => {
            Some(APP_BOUND_MEMORY_DATA_INPUT_CEILING)
        },
        _ => None,
    }
}

/// Reviewed transport/result envelope for each app-qualified compiled action.
/// This is deliberately distinct from persistent app-store payload quotas.
/// A new operation is inert until its physical owner supplies a finite bound
/// and that bound is incorporated into the descriptor/action lock.
pub(crate) fn reviewed_app_transport_result_ceiling(plan: &AppToolBindPlan) -> Option<u64> {
    match (
        normalized_tool_name(&plan.tool_name),
        plan.operation.as_deref(),
    ) {
        ("time_math", Some("now")) => Some(4 * 1024),
        ("time_math", Some("date_range")) => Some(8 * 1024),
        ("http", None | Some("get")) => {
            Some(crate::magician_v2::apps::bound_http::APP_BOUND_HTTP_RESULT_CEILING)
        },
        ("files", None | Some("read")) => Some(super::bound_path::APP_BOUND_FILE_RESULT_CEILING),
        ("files", Some("write")) => Some(4 * 1024),
        ("duckdb", Some("preview" | "describe")) => {
            Some(super::bound_path::APP_BOUND_TABLE_RESULT_CEILING)
        },
        ("internal_data", None | Some("list_learning_candidates" | "read_learning_candidate")) => {
            Some(APP_BOUND_LEARNING_READ_RESULT_CEILING)
        },
        ("thinking_maps_data", None | Some("list_maps" | "read_map")) => {
            Some(APP_BOUND_THINKING_MAP_READ_RESULT_CEILING)
        },
        (
            "evidence_data",
            None
            | Some(
                "list_pending_claims"
                | "read_claim"
                | "list_evidence_records"
                | "list_entities"
                | "list_commitments",
            ),
        ) => Some(APP_BOUND_EVIDENCE_DATA_RESULT_CEILING),
        (
            "meetings_data",
            None
            | Some(
                "active_session"
                | "list_threads"
                | "read_thread"
                | "read_takeaways"
                | "upcoming_meetings"
                | "search_meeting_memory",
            ),
        ) => Some(APP_BOUND_MEETINGS_DATA_RESULT_CEILING),
        ("agent_roster_data", None | Some("list_members" | "read_member")) => {
            Some(APP_BOUND_AGENT_ROSTER_RESULT_CEILING)
        },
        ("tasks_data", None | Some("list_tasks" | "read_task")) => {
            Some(APP_BOUND_TASKS_DATA_RESULT_CEILING)
        },
        ("notes_data", None | Some("search_notes" | "read_note")) => {
            Some(APP_BOUND_NOTES_DATA_RESULT_CEILING)
        },
        ("memory_data", None | Some("search_memory" | "read_entry")) => {
            Some(APP_BOUND_MEMORY_DATA_RESULT_CEILING)
        },
        _ => None,
    }
}

/// Single truth for whether the current common app effect owner can safely
/// reach a physical implementation. HTTP and path classes need pinned owners;
/// OS-jail skills need the prepared-action vertical; MCP skills need the
/// governed-MCP owner.
pub(crate) fn app_effect_owner_supported(plan: &AppToolBindPlan) -> bool {
    plan.runnable
        && match plan.contain {
            AppToolContainProfile::InProcessCompiled => {
                compiled_binder_ready(&plan.tool_name, plan.operation.as_deref(), plan.io_kind)
                    && compiled_app_provider_implementation_identity(&plan.tool_name).is_some()
                    && reviewed_app_transport_input_ceiling(plan).is_some()
                    && reviewed_app_transport_result_ceiling(plan).is_some()
            },
            // The OS-jail workflow owns its finite input/result ceilings through
            // exact source/descriptor/lock validation, and admits only the first
            // authority-free pure-transform slice. HTTP, filesystem, table,
            // device and otherwise unbound skill shapes remain inert.
            // Bound HTTP is runnable in the jail only when the reviewed source
            // declares its one egress destination (see `os_jail_ready`).
            AppToolContainProfile::OsJail => matches!(
                plan.io_kind,
                AppToolIoKind::PureTransform | AppToolIoKind::BoundHttp
            ),
            AppToolContainProfile::GovernedMcp => governed_mcp_binder_ready(plan.io_kind),
        }
}

/// ToolSkill contain profile is a function of the reviewed runtime protocol.
/// MCP packages are not OS-jail children.
pub fn tool_skill_contain_profile(shape: &AppToolDeclaredShape) -> AppToolContainProfile {
    if shape.protocol.as_deref() == Some("mcp") {
        AppToolContainProfile::GovernedMcp
    } else {
        AppToolContainProfile::OsJail
    }
}

/// Call-time evidence the kernel needs to mint a receipt. Classification does
/// not need this; Bound HTTP/file/table mint does.
#[derive(Debug, Clone, Copy)]
pub struct AppToolBindEvidence<'a> {
    pub parameters: &'a HashMap<String, Value>,
    pub workdir: Option<&'a Path>,
    pub now: DateTime<Utc>,
}

/// Declared pack/skill shape. Classification uses this, not a name family.
/// A new youtube-search-shaped skill is Bound HTTP because it declares
/// `search`/`research` or `url`, not because the name ends in `-search`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppToolDeclaredShape {
    pub compiled: bool,
    pub sandbox: Option<String>,
    pub categories: Vec<String>,
    pub composition_category: Option<String>,
    pub parameter_names: Vec<String>,
    pub read_only: Option<bool>,
    pub protocol: Option<String>,
    /// The HTTPS destinations a reviewed OS-jail skill declares
    /// (`metadata.magician.app_egress`).
    pub app_egress: Option<super::os_jail_egress::AppOsJailEgressDeclaration>,
    /// The skill runs in place (`app_in_place_skill_v1`: it names companions
    /// it spawns, or its secret contract needs it), so it reaches the network
    /// through the hosts the owner grants the app even without declaring any.
    pub in_place_network: bool,
}

impl AppToolDeclaredShape {
    pub fn from_compiled_pack(pack: &CapabilityPackDefinition) -> Self {
        let execution = pack.execution.as_ref();
        Self {
            compiled: true,
            sandbox: execution.and_then(|meta| meta.sandbox.clone()),
            categories: execution
                .map(|meta| meta.categories.clone())
                .unwrap_or_default(),
            composition_category: execution.and_then(|meta| meta.composition_category.clone()),
            parameter_names: pack
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
            read_only: pack.reliability.as_ref().map(|meta| meta.read_only),
            protocol: None,
            app_egress: None,
            in_place_network: false,
        }
    }

    pub fn from_skill_source(source: &str) -> Option<Self> {
        let package = parse_skill_runtime_package(source).ok().flatten()?;
        let protocol = match package.contract.runtime {
            RuntimeProtocol::Cli { .. } => "cli",
            RuntimeProtocol::Mcp { .. } => "mcp",
        };
        let app_egress = super::os_jail_egress::parse_app_egress_declaration(source)
            .ok()
            .flatten();
        let in_place_network = matches!(package.contract.runtime, RuntimeProtocol::Cli { .. })
            && super::os_jail::source_runs_in_place(&package.contract, app_egress.as_ref());
        Some(Self {
            compiled: false,
            sandbox: None,
            categories: package.catalog.categories,
            composition_category: package.catalog.composition_category,
            parameter_names: Vec::new(),
            read_only: None,
            protocol: Some(protocol.to_owned()),
            // A malformed declaration yields no egress here; the OS-jail
            // adapter refuses the same source outright.
            app_egress,
            in_place_network,
        })
    }
}

pub fn plan_app_tool_call(
    tool_name: &str,
    operation: Option<&str>,
    contain: AppToolContainProfile,
) -> AppToolBindPlan {
    plan_app_tool_call_with_shape(tool_name, operation, contain, None)
}

pub fn plan_app_tool_call_with_shape(
    tool_name: &str,
    operation: Option<&str>,
    contain: AppToolContainProfile,
    shape: Option<AppToolDeclaredShape>,
) -> AppToolBindPlan {
    let tool_name = normalized_tool_name(tool_name).to_owned();
    let operation = operation.and_then(|value| normalize_app_action_selector(&tool_name, value));
    let shape = shape.or_else(|| compiled_pack_shape(&tool_name));
    let io_kind = classify_io(&tool_name, operation.as_deref(), shape.as_ref());
    let attested_operations =
        runnable_operations(&tool_name, contain, operation.as_deref(), shape.as_ref());
    let binder_ready = match contain {
        AppToolContainProfile::InProcessCompiled => {
            compiled_binder_ready(&tool_name, operation.as_deref(), io_kind)
        },
        AppToolContainProfile::OsJail => io_binder_ready(io_kind),
        AppToolContainProfile::GovernedMcp => governed_mcp_binder_ready(io_kind),
    };
    let contain_ready = contain_profile_ready(contain)
        && (contain != AppToolContainProfile::OsJail || os_jail_ready(io_kind, shape.as_ref()))
        && (contain != AppToolContainProfile::GovernedMcp || governed_mcp_binder_ready(io_kind));
    let runnable = binder_ready && contain_ready && io_kind != AppToolIoKind::Unbound;
    let reason = if runnable && operation.is_none() && attested_operations.len() > 1 {
        format!(
            "wired operations: {} (new tools of these classes reuse the binders)",
            attested_operations.join(", ")
        )
    } else {
        refusal_reason(io_kind, contain, binder_ready, contain_ready, runnable)
    };
    AppToolBindPlan {
        tool_name,
        operation,
        io_kind,
        contain,
        runnable,
        attested_operations,
        reason,
    }
}

pub fn normalize_app_action_name(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_ascii_lowercase().replace('-', "_"))
}

pub(crate) fn normalize_app_action_selector(tool_name: &str, value: &str) -> Option<String> {
    let selected = normalize_app_action_name(value)?;
    let normalized_tool = normalize_app_action_name(tool_name)?;
    let qualified = format!("{normalized_tool}__");
    let prefixed = format!("{normalized_tool}_");
    Some(
        selected
            .strip_prefix(&qualified)
            .or_else(|| selected.strip_prefix(&prefixed))
            .unwrap_or(&selected)
            .to_owned(),
    )
}

/// Stable reviewed implementation identity for the in-process compiled owner.
/// It binds the immutable pack bytes to the exact bind/contain classification
/// and a named lowering profile. Call-specific target/input identity remains a
/// separate final-fence concern.
pub(crate) fn compiled_implementation_plan_digest(
    source_digest: &AppDigest,
    plan: &AppToolBindPlan,
) -> Option<AppDigest> {
    if !app_effect_owner_supported(plan) {
        return None;
    }
    let provider_implementation_identity =
        compiled_app_provider_implementation_identity(&plan.tool_name)?;
    let runtime_implementation_digest = compiled_runtime_implementation_digest()?;
    let reviewed_transport_input_byte_ceiling = reviewed_app_transport_input_ceiling(plan)?;
    let reviewed_transport_result_byte_ceiling = reviewed_app_transport_result_ceiling(plan)?;
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": "magician.app-compiled-physical-owner.v1",
        // The centrally owned semantic contract is independent of build
        // source evidence. Exact source/action schemas and plans still bind
        // the package; compatible host fixes do not widen that authority.
        "runtime_implementation_digest": runtime_implementation_digest,
        "provider_implementation_identity": provider_implementation_identity,
        "source_digest": source_digest,
        "plan": plan,
        "reviewed_transport_input_byte_ceiling": reviewed_transport_input_byte_ceiling,
        "reviewed_transport_result_byte_ceiling": reviewed_transport_result_byte_ceiling,
    }))
    .ok()
}

fn compiled_runtime_implementation_digest() -> Option<&'static AppDigest> {
    static CONTRACT: OnceLock<Option<AppDigest>> = OnceLock::new();
    CONTRACT
        .get_or_init(|| AppDigest::parse(super::runtime_contract::COMPILED_OWNER_CONTRACT_V1).ok())
        .as_ref()
}

pub(crate) fn compiled_runtime_source_digest() -> Option<&'static AppDigest> {
    static DIGEST: OnceLock<Option<AppDigest>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"magician.app-compiled-runtime-source.v1\0");
            hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
            hasher.update(b"\0compiled-providers\0");
            hasher.update(include_bytes!("../execution/compiled_providers.rs"));
            hasher.update(b"\0compiled-dispatch\0");
            hasher.update(include_bytes!("../execution/compiled_dispatch.rs"));
            hasher.update(b"\0app-bound-http-owner\0");
            hasher.update(include_bytes!("bound_http.rs"));
            hasher.update(b"\0app-bound-capability-directory-owner\0");
            hasher.update(include_bytes!("bound_path.rs"));
            hasher.update(b"\0app-tool-bind-policy\0");
            hasher.update(include_bytes!("app_tool_bind.rs"));
            hasher.update(b"\0app-runtime-contract\0");
            hasher.update(include_bytes!("runtime_contract.rs"));
            hasher.update(b"\0evidence-data-provider\0");
            hasher.update(include_bytes!("../execution/evidence_data_provider.rs"));
            hasher.update(b"\0meetings-data-provider\0");
            hasher.update(include_bytes!("../execution/meetings_data_provider.rs"));
            hasher.update(b"\0agent-roster-data-provider\0");
            hasher.update(include_bytes!("../execution/agent_roster_data_provider.rs"));
            hasher.update(b"\0tasks-data-provider\0");
            hasher.update(include_bytes!("../execution/tasks_data_provider.rs"));
            hasher.update(b"\0notes-data-provider\0");
            hasher.update(include_bytes!("../execution/notes_data_provider.rs"));
            hasher.update(b"\0memory-data-provider\0");
            hasher.update(include_bytes!("../execution/memory_data_provider.rs"));
            Some(AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex())).ok()?)
        })
        .as_ref()
}

pub fn app_tool_dispatch_note(name: &str, contain: AppToolContainProfile) -> AppToolDispatchNote {
    app_tool_dispatch_note_with_shape(name, contain, None)
}

pub fn app_tool_dispatch_note_with_shape(
    name: &str,
    contain: AppToolContainProfile,
    shape: Option<AppToolDeclaredShape>,
) -> AppToolDispatchNote {
    let plan = plan_app_tool_call_with_shape(name, None, contain, shape);
    let owner_supported = app_effect_owner_supported(&plan);
    AppToolDispatchNote {
        dispatchable: plan.runnable && owner_supported,
        attested_operations: plan.attested_operations,
        io_kind: plan.io_kind,
        contain: plan.contain,
        reason: if plan.runnable && !owner_supported {
            "no common app physical-effect owner is wired for this action class".to_owned()
        } else {
            plan.reason
        },
    }
}

pub fn app_tool_is_dispatchable(name: &str, contain: AppToolContainProfile) -> bool {
    app_tool_dispatch_note(name, contain).dispatchable
}

impl AppToolBindPlan {
    /// Mint the one-shot receipt for a call that already had its arguments
    /// proved by the implementation (dates parse, timezone exists, …).
    pub fn mint(&self, tool_ref: AppReference) -> Option<AttestedAppToolTarget> {
        self.mint_with_evidence(tool_ref, None)
    }

    pub fn mint_with_evidence(
        &self,
        tool_ref: AppReference,
        _evidence: Option<AppToolBindEvidence<'_>>,
    ) -> Option<AttestedAppToolTarget> {
        if !self.runnable {
            return None;
        }
        let runtime_ref = AppReference::parse(self.runtime_ref()).ok()?;
        match self.io_kind {
            AppToolIoKind::PureTransform => Some(
                AttestedAppToolTarget::from_trusted_pure_transform_dispatcher(
                    tool_ref,
                    runtime_ref,
                ),
            ),
            AppToolIoKind::TrustedLocalClock => Some(
                AttestedAppToolTarget::from_trusted_local_clock_dispatcher(tool_ref, runtime_ref),
            ),
            // Bound HTTP can be minted only by the move-only physical owner
            // after DNS resolution has been retained through connect. A URL-
            // only evidence envelope is deliberately insufficient.
            AppToolIoKind::BoundHttp => None,
            // Files/tables can be attested only by the move-only capability-
            // directory owner after no-follow descriptors and current metadata
            // survive through the final fence. Path strings are never enough.
            AppToolIoKind::BoundFile | AppToolIoKind::BoundWrite | AppToolIoKind::BoundTable => {
                None
            },
            // The one wired host-read dispatcher (plan 2.5): the scoped
            // learning-substrate reads over `internal_data`. The result is new
            // local content read from the runtime-owned scope, so the receipt
            // uses the reviewed `ReviewedScopedHostRead` contract. Any other
            // host-read tool still has no runnable plan and mints nothing.
            AppToolIoKind::BoundHostRead if self.tool_name == "internal_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            // The second host-read dispatcher (Phase 4 Brainstorm re-open):
            // the same 2.5 shape — scoped reads over the thinking-map
            // substrate through `thinking_maps_data`. The same conservative
            // `ReviewedScopedHostRead` contract applies for the same reason.
            AppToolIoKind::BoundHostRead if self.tool_name == "thinking_maps_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead if self.tool_name == "evidence_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            // The meetings console's read side. Same conservative
            // `ReviewedScopedHostRead` contract: the result is new scoped content
            // read from the runtime-owned scope.
            AppToolIoKind::BoundHostRead if self.tool_name == "meetings_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead if self.tool_name == "agent_roster_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead if self.tool_name == "tasks_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead if self.tool_name == "notes_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead if self.tool_name == "memory_data" => Some(
                AttestedAppToolTarget::from_reviewed_scoped_host_read(tool_ref, runtime_ref),
            ),
            AppToolIoKind::BoundHostRead
            | AppToolIoKind::BoundSideEffect
            | AppToolIoKind::Device
            | AppToolIoKind::Unbound => None,
        }
    }

    fn runtime_ref(&self) -> &'static str {
        match (self.tool_name.as_str(), self.io_kind) {
            ("time_math", AppToolIoKind::PureTransform) => "runtime:compiled:time_math:v1",
            ("time_math", AppToolIoKind::TrustedLocalClock) => "runtime:compiled:time_math:now:v1",
            (_, AppToolIoKind::BoundHttp) => "runtime:compiled:app-bound-http:v1",
            (_, AppToolIoKind::BoundFile | AppToolIoKind::BoundWrite) => {
                "runtime:compiled:app-bound-file:v1"
            },
            (_, AppToolIoKind::BoundTable) => "runtime:compiled:app-bound-table:v1",
            ("internal_data", AppToolIoKind::BoundHostRead) => {
                "runtime:compiled:internal-data-learning-read:v1"
            },
            ("thinking_maps_data", AppToolIoKind::BoundHostRead) => {
                "runtime:compiled:thinking-maps-read:v1"
            },
            ("evidence_data", AppToolIoKind::BoundHostRead) => {
                "runtime:compiled:evidence-data-read:v1"
            },
            ("meetings_data", AppToolIoKind::BoundHostRead) => {
                "runtime:compiled:meetings-data-read:v1"
            },
            ("agent_roster_data", AppToolIoKind::BoundHostRead) => {
                "runtime:compiled:agent-roster-read:v1"
            },
            ("tasks_data", AppToolIoKind::BoundHostRead) => "runtime:compiled:tasks-read:v1",
            ("notes_data", AppToolIoKind::BoundHostRead) => "runtime:compiled:notes-read:v1",
            ("memory_data", AppToolIoKind::BoundHostRead) => "runtime:compiled:memory-read:v1",
            _ => "runtime:compiled:app-tool-bind:v1",
        }
    }
}

fn compiled_pack_shape(tool_name: &str) -> Option<AppToolDeclaredShape> {
    compiled_pack(tool_name).map(AppToolDeclaredShape::from_compiled_pack)
}

fn classify_io(
    tool_name: &str,
    operation: Option<&str>,
    shape: Option<&AppToolDeclaredShape>,
) -> AppToolIoKind {
    match (tool_name, operation) {
        ("time_math", Some("date_range") | None) => return AppToolIoKind::PureTransform,
        ("time_math", Some("now")) => return AppToolIoKind::TrustedLocalClock,
        ("time_math", _) => return AppToolIoKind::Unbound,
        ("shell", _) => return AppToolIoKind::Unbound,
        // The first host-read binder (plan 2.5): exactly the two learning
        // review reads are classifiable, and only as scoped substrate reads.
        // An operation-less call defaults to the narrower list read — the
        // least-authority default `http` gets from GET — and never to the
        // provider's broad `catalog`. Every other `internal_data` action
        // stays unclassified, and so unrunnable, rather than inheriting the
        // pack's compiled shape.
        ("internal_data", None | Some("list_learning_candidates" | "read_learning_candidate")) => {
            return AppToolIoKind::BoundHostRead
        },
        ("internal_data", _) => return AppToolIoKind::Unbound,
        // The second host-read binder (Phase 4 Brainstorm re-open — the 2.5
        // pattern generalized): exactly the two thinking-map reads are
        // classifiable, and only as scoped substrate reads. An operation-less
        // call defaults to the bounded list read — the least-authority
        // default `http` gets from GET — and never to a mutation or a raw
        // passthrough. Every other `thinking_maps_data` action stays
        // unclassified, and so unrunnable, rather than inheriting the pack's
        // compiled shape.
        ("thinking_maps_data", None | Some("list_maps" | "read_map")) => {
            return AppToolIoKind::BoundHostRead;
        },
        ("thinking_maps_data", _) => return AppToolIoKind::Unbound,
        // The claims-review host binder has exactly five read actions. The
        // operation-less call defaults to the pending queue; no mutation verb
        // or pack-wide fallback can inherit this class.
        (
            "evidence_data",
            None
            | Some(
                "list_pending_claims"
                | "read_claim"
                | "list_evidence_records"
                | "list_entities"
                | "list_commitments",
            ),
        ) => return AppToolIoKind::BoundHostRead,
        ("evidence_data", _) => return AppToolIoKind::Unbound,
        // The meetings host binder has exactly six read actions. The
        // operation-less call defaults to the live-capture read — the narrowest
        // of the six and the only one that needs no target — and never to a
        // transcript sweep or a pack-wide fallback. No control verb can inherit
        // this class: capture control is its own reviewed action family with a
        // different destination.
        (
            "meetings_data",
            None
            | Some(
                "active_session"
                | "list_threads"
                | "read_thread"
                | "read_takeaways"
                | "upcoming_meetings"
                | "search_meeting_memory",
            ),
        ) => return AppToolIoKind::BoundHostRead,
        ("meetings_data", _) => return AppToolIoKind::Unbound,
        // The agent-roster binder has exactly two read actions. The
        // operation-less call defaults to the bounded list; no definition-edit
        // verb can inherit this class, because editing a definition is the
        // agent owner's job and reaches a different substrate entirely.
        ("agent_roster_data", None | Some("list_members" | "read_member")) => {
            return AppToolIoKind::BoundHostRead;
        },
        ("agent_roster_data", _) => return AppToolIoKind::Unbound,
        // The task-list binder has exactly two reads. The operation-less call
        // defaults to the bounded list; no task-changing verb can inherit this
        // class — creating, running and editing tasks is the task owner's job.
        ("tasks_data", None | Some("list_tasks" | "read_task")) => {
            return AppToolIoKind::BoundHostRead;
        },
        ("tasks_data", _) => return AppToolIoKind::Unbound,
        // The notes binder has exactly two reads; an unnamed call classifies as
        // the search so the argument proof (which requires a query) decides it
        // is incomplete. No note-writing verb can inherit this class.
        ("notes_data", None | Some("search_notes" | "read_note")) => {
            return AppToolIoKind::BoundHostRead;
        },
        ("notes_data", _) => return AppToolIoKind::Unbound,
        // Owner-granted memory reads: exactly two reads, no memory write verb
        // can inherit this class.
        ("memory_data", None | Some("search_memory" | "read_entry")) => {
            return AppToolIoKind::BoundHostRead;
        },
        ("memory_data", _) => return AppToolIoKind::Unbound,
        _ => {},
    }
    if let Some(shape) = shape {
        return classify_declared_shape(operation, shape);
    }
    AppToolIoKind::Unbound
}

fn classify_declared_shape(operation: Option<&str>, shape: &AppToolDeclaredShape) -> AppToolIoKind {
    if shape.sandbox.as_deref() == Some("shell") {
        return AppToolIoKind::Unbound;
    }
    if is_device_shape(shape) {
        return AppToolIoKind::Device;
    }
    if is_table_shape(shape) {
        return classify_duckdb_operation(operation);
    }
    if is_commerce_shape(shape) {
        return AppToolIoKind::BoundSideEffect;
    }
    if is_file_shape(shape) {
        return classify_file_operation(shape, operation);
    }
    if is_http_shape(shape) {
        return classify_http_operation(operation);
    }
    if is_pure_transform_shape(shape) {
        return AppToolIoKind::PureTransform;
    }
    if is_host_read_shape(shape) {
        return AppToolIoKind::BoundHostRead;
    }
    if shape.compiled || is_mutating_skill_shape(shape) {
        return AppToolIoKind::BoundSideEffect;
    }
    AppToolIoKind::Unbound
}

fn classify_http_operation(operation: Option<&str>) -> AppToolIoKind {
    match operation.unwrap_or("get") {
        "post" | "put" | "patch" | "delete" | "http_post" | "http_put" | "http_patch"
        | "http_delete" => AppToolIoKind::BoundSideEffect,
        _ => AppToolIoKind::BoundHttp,
    }
}

/// Parameter names that can each name the operation a call performs.
///
/// Different layers historically read different ones — the classifier walks
/// this list in order while a provider's `lower()` reads whichever single name
/// it was written against. That gap is the whole defect
/// [`divergent_operation_alias`] closes.
const OPERATION_ALIAS_KEYS: &[&str] = &["operation", "__action_name", "action", "method"];

/// The first pair of operation aliases that disagree, as `(key, key)`.
///
/// An app call may name its operation once. Supplying two names with different
/// values lets the value the classifier happens to read differ from the one the
/// executor acts on — `{operation: "get", method: "DELETE"}` classifies as a
/// read and performs a delete. Which alias wins is an implementation detail of
/// the fallback order, never something a caller may choose.
///
/// Refused, never normalised: silently picking one would hand the app a call it
/// did not write, the same reasoning as
/// `execution::restricted_action`'s duplicate-parameter refusal.
pub fn divergent_operation_alias(
    parameters: &HashMap<String, Value>,
) -> Option<(&'static str, &'static str)> {
    let mut seen: Option<(&'static str, String)> = None;
    for key in OPERATION_ALIAS_KEYS {
        let Some(value) = parameters.get(*key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() {
            continue;
        }
        match &seen {
            // `http_get`-style tool-qualified spellings mean the same operation
            // as the bare verb, so compare on the suffix as well as the whole.
            Some((first_key, first_value)) if !operation_values_agree(first_value, &value) => {
                return Some((first_key, key));
            },
            Some(_) => {},
            None => seen = Some((key, value)),
        }
    }
    None
}

fn operation_values_agree(left: &str, right: &str) -> bool {
    let strip = |value: &str| {
        value
            .strip_prefix("http_")
            .or_else(|| value.strip_prefix("files_"))
            .unwrap_or(value)
            .to_owned()
    };
    left == right || strip(left) == strip(right)
}

/// The IO class the LOWERED action actually performs, when the lowered form
/// determines it.
///
/// The canonical effective act, not the caller's description of it: the
/// parameter map is what the app wrote, while this is what the runtime will
/// really do. Comparing the two is what stops a call from being classified as
/// one thing and executed as another.
///
/// `None` means the lowered form does NOT determine the class and the caller
/// must not infer agreement from it:
/// - [`ExecutableAction::DuckDb`] carries only an opaque SQL string, so a
///   `read_parquet` and a raw `query` are indistinguishable here. duckdb is
///   contained by removing its free-form SQL fragments instead (see
///   `bind_parameters_for_call`).
/// - [`ExecutableAction::Pack`] keeps its operation in `resolved_params`, which
///   the parameter-level classifier already reads.
pub fn classify_lowered_action(
    action: &crate::magician_v2::execution::actions::ExecutableAction,
) -> Option<AppToolIoKind> {
    use crate::magician_v2::execution::actions::{ExecutableAction, FileAction, HttpMethod};
    match action {
        ExecutableAction::Http(http) => Some(match http.method {
            HttpMethod::Get | HttpMethod::Head | HttpMethod::Options => AppToolIoKind::BoundHttp,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete => {
                AppToolIoKind::BoundSideEffect
            },
        }),
        ExecutableAction::File(file) => Some(match file {
            FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => {
                AppToolIoKind::BoundFile
            },
            FileAction::Write { .. }
            | FileAction::Append { .. }
            | FileAction::Delete { .. }
            | FileAction::Copy { .. }
            | FileAction::Move { .. }
            | FileAction::CreateDir { .. } => AppToolIoKind::BoundWrite,
        }),
        _ => None,
    }
}

fn classify_file_operation(shape: &AppToolDeclaredShape, operation: Option<&str>) -> AppToolIoKind {
    if has_category(shape, "write") {
        return AppToolIoKind::BoundWrite;
    }
    // Every alias the executor accepts for a mutating action must appear here.
    // `lowering::canonical_file_tool` maps `remove`→delete, `rename`→move and
    // `create_dir`/`create_directory`→mkdir, but this list once named only
    // `delete`/`move`/`mkdir` — so `{action: "remove"}` classified BoundFile, a
    // runnable READ, and then deleted the file. Same normalisation as the
    // executor (trim, lowercase, `-`→`_`) so a spelling cannot slip between the
    // two.
    let normalized = operation
        .unwrap_or("read")
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    match normalized.as_str() {
        "write" | "append" | "delete" | "remove" | "copy" | "move" | "rename" | "mkdir"
        | "create_dir" | "create_directory" => AppToolIoKind::BoundWrite,
        _ => AppToolIoKind::BoundFile,
    }
}

fn classify_duckdb_operation(operation: Option<&str>) -> AppToolIoKind {
    match operation.unwrap_or("preview") {
        "preview" | "describe" | "list_tables" | "read_parquet" => AppToolIoKind::BoundTable,
        _ => AppToolIoKind::Unbound,
    }
}

fn is_http_shape(shape: &AppToolDeclaredShape) -> bool {
    has_param(shape, "url")
        || has_any_category(shape, &["http", "api"])
        || shape.composition_category.as_deref() == Some("http_operations")
        || shape.protocol.as_deref() == Some("mcp")
        || (!shape.compiled
            && has_any_category(shape, &["search", "osint", "research", "web"])
            && !has_any_category(shape, &["filesystem", "merge"]))
}

fn is_file_shape(shape: &AppToolDeclaredShape) -> bool {
    // Only the closed files pack (`sandbox: file` or category `files`).
    // Bare `filesystem` (grep/glob/read_file) spawns rg or uses the host
    // sandbox and must not become BoundFile.
    shape.sandbox.as_deref() == Some("file") || has_category(shape, "files")
}

fn is_table_shape(shape: &AppToolDeclaredShape) -> bool {
    has_param(shape, "sql")
        && (has_param(shape, "source")
            || has_param(shape, "attach_path")
            || has_param(shape, "path"))
}

fn is_device_shape(shape: &AppToolDeclaredShape) -> bool {
    has_any_category(
        shape,
        &[
            "macos",
            "android",
            "screenshot",
            "meeting",
            "ui_automation",
            "desktop_operations",
            "device",
        ],
    )
}

fn is_pure_transform_shape(shape: &AppToolDeclaredShape) -> bool {
    has_category(shape, "merge")
        && !has_param(shape, "url")
        && shape.sandbox.as_deref() != Some("file")
}

pub fn tool_needs_workdir(kind: AppToolIoKind) -> bool {
    matches!(
        kind,
        AppToolIoKind::BoundFile | AppToolIoKind::BoundWrite | AppToolIoKind::BoundTable
    )
}

/// Normalize bound path params to exact portable relative paths and infer
/// DuckDB structured actions. The physical owner opens them descriptor-
/// relative to the separately supplied capability directory; absolute host
/// paths never enter a lowered app action.
pub fn bind_parameters_for_call(
    plan: &AppToolBindPlan,
    parameters: &HashMap<String, Value>,
    workdir: Option<&Path>,
) -> Option<HashMap<String, Value>> {
    let mut parameters = parameters.clone();
    if plan.tool_name == "files"
        && !parameters
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    {
        // `lower_file_action` routes on `action`, while the class was decided
        // from `plan.operation`. Seed `action` FROM the plan so the two always
        // name the same operation; defaulting to `read` regardless would make a
        // call classified BoundWrite lower to a read (and now be refused by the
        // lowered-form agreement check). Only the no-`action` case is seeded —
        // an explicit `action` that disagrees with another alias was already
        // refused by `divergent_operation_alias`.
        let seeded = plan.operation.clone().unwrap_or_else(|| "read".to_owned());
        parameters.insert("action".to_owned(), Value::String(seeded));
    }
    if plan.tool_name == "duckdb" {
        // A SQL fragment in interpreter position can never be bound. `select`
        // and `where_clause` are spliced into the generated statement with only
        // `path` escaped, so an app could comment out the generated FROM and
        // read any host file (`read_text`) or reach the network (`read_csv`),
        // escaping both the workdir sandbox and its network policy. There is no
        // canonicalisation that makes an arbitrary SQL string safe — the only
        // closed answer is to refuse it. Structured `columns` / `predicates`
        // are the intended replacement surface.
        //
        // App path only: agents keep raw `duckdb.query` for interactive and
        // administrative use, exactly as `restricted_action` keeps the raw
        // outward actions.
        for fragment in ["select", "where_clause"] {
            if parameters
                .get(fragment)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            {
                return None;
            }
        }
        // The classifier decided the class from `plan.operation`; the provider
        // routes on `__action_name`. Bind the second to the first so the two
        // cannot name different operations — otherwise a call classified as a
        // `preview` (BoundTable, runnable) could route to a raw `query`.
        match plan.operation.as_deref() {
            Some(operation) => {
                parameters.insert(
                    "__action_name".to_owned(),
                    Value::String(operation.to_owned()),
                );
            },
            None => {
                if !parameters
                    .get("__action_name")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                {
                    if let Some(action) = infer_duckdb_action(&parameters) {
                        parameters.insert("__action_name".to_owned(), Value::String(action));
                    } else {
                        return None;
                    }
                }
            },
        }
        // The two sides default OPPOSITE ways: this classifier assumes
        // `preview` (BoundTable, runnable) while `lower_duckdb_action` assumes
        // `query` (raw SQL) when `__action_name` is absent. That default is
        // correct for the agents' direct-dispatch callers and must stay, so the
        // app path closes the gap by never leaving it absent. Enforced here
        // rather than assumed, because the cost of the assumption breaking is a
        // BoundTable-classified call executing arbitrary SQL.
        if !parameters
            .get("__action_name")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return None;
        }
    }
    if plan.tool_name == "internal_data" {
        // Same alias gap as duckdb, with a catalog-shaped miss: the provider
        // routes on `__action_name`/`action` and silently defaults to the
        // broad `catalog` action when neither is present, while the class was
        // decided from `plan.operation` (with the bounded list read as the
        // operation-less default). Bind the provider's routing key to the
        // planned operation so a call can never classify as one learning read
        // and then execute another action.
        let operation = plan
            .operation
            .as_deref()
            .unwrap_or("list_learning_candidates");
        let selected = normalize_app_action_selector("internal_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "thinking_maps_data" {
        // Same alias gap, same fix as internal_data (the 2.5 pattern): the
        // provider routes on `__action_name`/`action` and defaults to the
        // bounded list read when neither is present, while the class was
        // decided from `plan.operation`. Bind the provider's routing key to
        // the planned operation so a call can never classify as one
        // thinking-map read and then execute another action.
        let operation = plan.operation.as_deref().unwrap_or("list_maps");
        let selected = normalize_app_action_selector("thinking_maps_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "evidence_data" {
        // Bind the provider's routing key to the already-classified operation.
        // This closes the same alias/default gap as internal_data and
        // thinking_maps_data; operation-less calls always become the bounded
        // pending-claims list.
        let operation = plan.operation.as_deref().unwrap_or("list_pending_claims");
        let selected = normalize_app_action_selector("evidence_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "agent_roster_data" {
        // Same alias/default gap, same fix: bind the routing key to the
        // operation the class was decided from.
        let operation = plan.operation.as_deref().unwrap_or("list_members");
        let selected = normalize_app_action_selector("agent_roster_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "tasks_data" {
        // Bind the provider's routing key to the operation the class was
        // decided from, so a call can never classify as one read and run
        // another.
        let operation = plan.operation.as_deref().unwrap_or("list_tasks");
        let selected = normalize_app_action_selector("tasks_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "notes_data" {
        // Bind the routing key to the classified operation.
        let operation = plan.operation.as_deref().unwrap_or("search_notes");
        let selected = normalize_app_action_selector("notes_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "memory_data" {
        let operation = plan.operation.as_deref().unwrap_or("search_memory");
        let selected = normalize_app_action_selector("memory_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if plan.tool_name == "meetings_data" {
        // Same alias/default gap, same fix: bind the routing key to the
        // operation the class was decided from, so a call can never classify as
        // one meetings read and then execute another.
        let operation = plan.operation.as_deref().unwrap_or("active_session");
        let selected = normalize_app_action_selector("meetings_data", operation)
            .unwrap_or_else(|| operation.to_owned());
        parameters.insert("__action_name".to_owned(), Value::String(selected));
    }
    if !tool_needs_workdir(plan.io_kind) {
        return Some(parameters);
    }
    let _workdir = workdir?;
    let skip_source = parameters.get("is_table").and_then(Value::as_bool) == Some(true);
    for key in PATH_PARAM_KEYS {
        if skip_source && *key == "source" {
            continue;
        }
        let Some(raw) = parameters.get(*key).and_then(Value::as_str) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let bound = super::bound_path::normalize_app_relative_path(Path::new(raw)).ok()?;
        parameters.insert((*key).to_owned(), Value::String(bound));
    }
    Some(parameters)
}

const PATH_PARAM_KEYS: &[&str] = &[
    "path",
    "source",
    "destination",
    "attach_path",
    "database",
    "output_path",
    "file_path",
];

fn infer_duckdb_action(parameters: &HashMap<String, Value>) -> Option<String> {
    if let Some(action) = parameters
        .get("operation")
        .or_else(|| parameters.get("action"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(action.to_ascii_lowercase());
    }
    if parameters
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Some("read_parquet".to_owned());
    }
    if parameters
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Some("preview".to_owned());
    }
    None
}

fn is_host_read_shape(shape: &AppToolDeclaredShape) -> bool {
    matches!(
        shape.composition_category.as_deref(),
        Some("read" | "observation")
    ) || has_any_category(shape, &["introspection", "evidence"])
}

fn is_commerce_shape(shape: &AppToolDeclaredShape) -> bool {
    shape
        .composition_category
        .as_deref()
        .is_some_and(|value| value.contains("commerce"))
        || has_any_category(shape, &["commerce", "shopping"])
}

fn is_mutating_skill_shape(shape: &AppToolDeclaredShape) -> bool {
    shape.composition_category.as_deref().is_some_and(|value| {
        value.ends_with("_operations") && !matches!(value, "http_operations" | "utility_operations")
    })
}

fn has_param(shape: &AppToolDeclaredShape, name: &str) -> bool {
    shape
        .parameter_names
        .iter()
        .any(|parameter| parameter == name)
}

fn has_category(shape: &AppToolDeclaredShape, name: &str) -> bool {
    shape.categories.iter().any(|category| category == name)
}

fn has_any_category(shape: &AppToolDeclaredShape, names: &[&str]) -> bool {
    names.iter().any(|name| has_category(shape, name))
}

fn runnable_operations(
    tool_name: &str,
    contain: AppToolContainProfile,
    selected: Option<&str>,
    shape: Option<&AppToolDeclaredShape>,
) -> Vec<String> {
    if !contain_profile_ready(contain) {
        return Vec::new();
    }
    let mut ops: BTreeSet<String> = BTreeSet::new();
    if let Some(selected) = selected {
        ops.insert(selected.to_owned());
    }
    if let Some(pack) = compiled_pack(tool_name) {
        for name in pack.native_action_schemas.keys() {
            ops.insert(name.to_ascii_lowercase());
        }
        for parameter in &pack.parameters {
            if matches!(parameter.name.as_str(), "operation" | "action" | "method") {
                if let Some(values) = &parameter.enum_values {
                    for value in values {
                        ops.insert(value.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    ops.into_iter()
        .filter(|op| {
            let io_kind = classify_io(tool_name, Some(op.as_str()), shape);
            let ready = match contain {
                AppToolContainProfile::InProcessCompiled => {
                    compiled_binder_ready(tool_name, Some(op.as_str()), io_kind)
                },
                AppToolContainProfile::OsJail => io_binder_ready(io_kind),
                AppToolContainProfile::GovernedMcp => governed_mcp_binder_ready(io_kind),
            };
            ready && io_kind != AppToolIoKind::Unbound
        })
        .collect()
}

fn compiled_pack(tool_name: &str) -> Option<&'static CapabilityPackDefinition> {
    embedded_compiled_pack_defs_ref()
        .iter()
        .find(|pack| pack.name == tool_name)
}

fn io_binder_ready(kind: AppToolIoKind) -> bool {
    matches!(
        kind,
        AppToolIoKind::PureTransform
            | AppToolIoKind::TrustedLocalClock
            | AppToolIoKind::BoundHttp
            | AppToolIoKind::BoundFile
            | AppToolIoKind::BoundWrite
            | AppToolIoKind::BoundTable
    )
}

fn governed_mcp_binder_ready(kind: AppToolIoKind) -> bool {
    matches!(
        kind,
        AppToolIoKind::BoundHttp | AppToolIoKind::BoundSideEffect
    )
}

fn compiled_binder_ready(tool_name: &str, operation: Option<&str>, kind: AppToolIoKind) -> bool {
    match (normalized_tool_name(tool_name), operation, kind) {
        ("files", None | Some("read"), AppToolIoKind::BoundFile)
        | ("files", Some("write"), AppToolIoKind::BoundWrite)
        | ("duckdb", Some("preview" | "describe"), AppToolIoKind::BoundTable) => {
            super::bound_path::APP_BOUND_PATH_OWNER_SUPPORTED
        },
        ("time_math", None | Some("date_range"), AppToolIoKind::PureTransform)
        | ("time_math", Some("now"), AppToolIoKind::TrustedLocalClock)
        | ("http", None | Some("get"), AppToolIoKind::BoundHttp) => true,
        // Scoped learning-substrate reads over `internal_data` (plan 2.5).
        // The action set is exactly the two review reads with the bounded
        // list as the operation-less default; the runtime scope stays
        // executor-owned (`__principal`/`__workspace` are stripped from model
        // args and re-injected), and the parameter surface is closed by
        // `InternalDataProvider::prove_app_tool_args`.
        (
            "internal_data",
            None | Some("list_learning_candidates" | "read_learning_candidate"),
            AppToolIoKind::BoundHostRead,
        ) => true,
        // Scoped thinking-map reads over `thinking_maps_data` (Phase 4
        // Brainstorm re-open — plan 2.5 generalized). The action set is
        // exactly the two bounded reads with the list as the operation-less
        // default; the runtime scope stays executor-owned
        // (`__principal`/`__workspace` are stripped from model args and
        // re-injected), and the parameter surface is closed by
        // `ThinkingMapsDataProvider::prove_app_tool_args`.
        (
            "thinking_maps_data",
            None | Some("list_maps" | "read_map"),
            AppToolIoKind::BoundHostRead,
        ) => true,
        (
            "evidence_data",
            None
            | Some(
                "list_pending_claims"
                | "read_claim"
                | "list_evidence_records"
                | "list_entities"
                | "list_commitments",
            ),
            AppToolIoKind::BoundHostRead,
        ) => true,
        // Scoped meeting reads over `meetings_data`. The action set is exactly
        // the six bounded reads with the live-capture read as the
        // operation-less default; the runtime scope stays executor-owned, and
        // the parameter surface is closed by
        // `MeetingsDataProvider::prove_app_tool_args`.
        (
            "meetings_data",
            None
            | Some(
                "active_session"
                | "list_threads"
                | "read_thread"
                | "read_takeaways"
                | "upcoming_meetings"
                | "search_meeting_memory",
            ),
            AppToolIoKind::BoundHostRead,
        ) => true,
        // Scoped agent-roster reads over `agent_roster_data`. Exactly the two
        // bounded reads with the list as the operation-less default; the
        // runtime scope stays executor-owned and the parameter surface is
        // closed by `AgentRosterDataProvider::prove_app_tool_args`.
        (
            "agent_roster_data",
            None | Some("list_members" | "read_member"),
            AppToolIoKind::BoundHostRead,
        ) => true,
        // Scoped task-list reads over `tasks_data`: exactly the two bounded
        // reads, the list as the operation-less default, the parameter surface
        // closed by `TasksDataProvider::prove_app_tool_args`.
        ("tasks_data", None | Some("list_tasks" | "read_task"), AppToolIoKind::BoundHostRead) => {
            true
        },
        // Scoped notes search/read over `notes_data`; the parameter surface is
        // closed by `NotesDataProvider::prove_app_tool_args`.
        ("notes_data", None | Some("search_notes" | "read_note"), AppToolIoKind::BoundHostRead) => {
            true
        },
        // Owner-granted memory reads over `memory_data`; the grant itself is
        // re-read from the app registry on every call by the provider.
        (
            "memory_data",
            None | Some("search_memory" | "read_entry"),
            AppToolIoKind::BoundHostRead,
        ) => true,
        _ => false,
    }
}

pub fn extract_bound_http_urls(parameters: &HashMap<String, Value>) -> Vec<String> {
    call_urls(parameters)
}

fn call_urls(parameters: &HashMap<String, Value>) -> Vec<String> {
    let mut urls = Vec::new();
    push_url(&mut urls, parameters.get("url"));
    push_url(
        &mut urls,
        parameters
            .get("candidate")
            .and_then(|value| value.get("url")),
    );
    push_url(
        &mut urls,
        parameters
            .get("candidate")
            .and_then(|value| value.get("canonical_url")),
    );
    if let Some(requests) = parameters.get("requests").and_then(Value::as_array) {
        for request in requests {
            push_url(&mut urls, request.get("url"));
            push_url(
                &mut urls,
                request.get("candidate").and_then(|value| value.get("url")),
            );
            push_url(
                &mut urls,
                request
                    .get("candidate")
                    .and_then(|value| value.get("canonical_url")),
            );
        }
    }
    urls
}

fn push_url(urls: &mut Vec<String>, value: Option<&Value>) {
    if let Some(url) = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        urls.push(url.to_owned());
    }
}

/// What the OS-jail owner can run: a closed pure transform, or a bound-HTTP
/// skill that declares its egress destinations or runs in place (reaching
/// the hosts the owner grants the app), in the brokered-egress jail.
fn os_jail_ready(io_kind: AppToolIoKind, shape: Option<&AppToolDeclaredShape>) -> bool {
    match io_kind {
        AppToolIoKind::PureTransform => true,
        AppToolIoKind::BoundHttp => {
            shape.is_some_and(|shape| shape.app_egress.is_some() || shape.in_place_network)
        },
        _ => false,
    }
}

fn contain_profile_ready(contain: AppToolContainProfile) -> bool {
    matches!(
        contain,
        AppToolContainProfile::InProcessCompiled
            | AppToolContainProfile::OsJail
            | AppToolContainProfile::GovernedMcp
    )
}

fn refusal_reason(
    io_kind: AppToolIoKind,
    contain: AppToolContainProfile,
    binder_ready: bool,
    contain_ready: bool,
    runnable: bool,
) -> String {
    if runnable {
        return match io_kind {
            AppToolIoKind::PureTransform => {
                "closed pure transform; new tools of this class reuse this binder".to_owned()
            },
            AppToolIoKind::TrustedLocalClock => {
                "trusted local clock; new tools of this class reuse this binder".to_owned()
            },
            AppToolIoKind::BoundHttp if contain == AppToolContainProfile::OsJail => {
                "runs in the brokered-egress jail and reaches only the hosts the app is granted \
                 (its declared destinations, or any public host under that explicit grant)"
                    .to_owned()
            },
            _ => "runnable".to_owned(),
        };
    }
    if contain == AppToolContainProfile::OsJail
        && !contain_ready
        && io_kind == AppToolIoKind::BoundHttp
    {
        return "web skill declares no egress destination (metadata.magician.app_egress), \
                so it cannot reach the network from an app"
            .to_owned();
    }
    if contain == AppToolContainProfile::OsJail && !contain_ready {
        return "skillshub/CLI tools need OS-jail contain before they can run in apps".to_owned();
    }
    if contain == AppToolContainProfile::GovernedMcp && !contain_ready {
        return "skillshub MCP tools need the governed MCP owner before they can run in apps"
            .to_owned();
    }
    if !binder_ready {
        return match io_kind {
            AppToolIoKind::BoundHttp => {
                "bound-HTTP binder is not wired; grant destinations are not enough yet".to_owned()
            },
            AppToolIoKind::BoundFile => {
                "bound-file binder is not wired; host paths stay refused".to_owned()
            },
            AppToolIoKind::BoundWrite => {
                "bound-write binder is not wired; file/HTTP mutations stay refused".to_owned()
            },
            AppToolIoKind::BoundTable => {
                "bound-table binder is not wired; open SQL stays refused".to_owned()
            },
            AppToolIoKind::BoundHostRead => {
                "bound host-read binder is not wired; Magician store reads stay refused".to_owned()
            },
            AppToolIoKind::BoundSideEffect => {
                "bound side-effect binder is not wired; mutations stay refused".to_owned()
            },
            AppToolIoKind::Device => {
                "device jail is not wired; screen/phone/desktop control stays refused".to_owned()
            },
            AppToolIoKind::Unbound => "this operation has no closed bind class for apps".to_owned(),
            AppToolIoKind::PureTransform | AppToolIoKind::TrustedLocalClock => {
                "binder is not available for this contain profile".to_owned()
            },
        };
    }
    "this operation is not runnable for apps".to_owned()
}

fn normalized_tool_name(name: &str) -> &str {
    name.trim()
        .strip_prefix("capability:")
        .unwrap_or(name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_math_in_process_is_runnable_per_operation() {
        let range = plan_app_tool_call(
            "time_math",
            Some("date_range"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(range.runnable);
        assert_eq!(range.io_kind, AppToolIoKind::PureTransform);
        let now = plan_app_tool_call(
            "capability:time_math",
            Some("now"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(now.runnable);
        assert_eq!(now.io_kind, AppToolIoKind::TrustedLocalClock);
        let explode = plan_app_tool_call(
            "time_math",
            Some("explode"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(!explode.runnable);
        assert_eq!(explode.io_kind, AppToolIoKind::Unbound);
    }

    #[test]
    fn skillshub_contain_is_not_wired() {
        // The legacy test name is retained for qualification filtering. The
        // reviewed OS-jail owner now admits only authority-free pure
        // transforms; every physical I/O class remains inert.
        let pure = app_tool_dispatch_note("time_math", AppToolContainProfile::OsJail);
        assert!(pure.dispatchable);
        assert_eq!(pure.io_kind, AppToolIoKind::PureTransform);

        let physical = app_tool_dispatch_note("http", AppToolContainProfile::OsJail);
        assert!(!physical.dispatchable);
        assert_eq!(physical.io_kind, AppToolIoKind::BoundHttp);
        assert!(physical.reason.contains("egress destination"));
    }

    #[test]
    fn only_exact_bound_http_file_write_and_table_operations_are_runnable() {
        let http = plan_app_tool_call("http", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(http.io_kind, AppToolIoKind::BoundHttp);
        assert!(http.runnable);
        let files = plan_app_tool_call("files", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(files.io_kind, AppToolIoKind::BoundFile);
        assert!(files.runnable);
        let duckdb = plan_app_tool_call(
            "duckdb",
            Some("preview"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(duckdb.io_kind, AppToolIoKind::BoundTable);
        assert!(duckdb.runnable);
        for (tool, operation) in [
            ("files", "append"),
            ("files", "delete"),
            ("files", "list"),
            ("duckdb", "query"),
            ("duckdb", "read_parquet"),
            ("duckdb", "list_tables"),
        ] {
            assert!(
                !plan_app_tool_call(
                    tool,
                    Some(operation),
                    AppToolContainProfile::InProcessCompiled,
                )
                .runnable
            );
        }
        let post = plan_app_tool_call(
            "http",
            Some("post"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(post.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!post.runnable);
    }

    #[test]
    fn mint_refuses_bound_http_without_a_public_url() {
        let plan = plan_app_tool_call("http", None, AppToolContainProfile::InProcessCompiled);
        let tool = AppReference::parse("capability:http").unwrap();
        assert!(plan.mint(tool).is_none());
    }

    #[test]
    fn internal_data_admits_only_the_learning_review_reads_as_host_reads() {
        // The first wired host-read binder (plan 2.5): exactly the two scoped
        // learning review reads are runnable, and they mint a conservative
        // local-content receipt. Everything else `internal_data` exposes —
        // including the pack-level default `catalog` and the broad SQL,
        // telemetry and procedure reads — stays unclassified and unrunnable
        // for apps.
        for operation in ["list_learning_candidates", "read_learning_candidate"] {
            let plan = plan_app_tool_call(
                "internal_data",
                Some(operation),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{operation}");
            assert!(plan.runnable, "{operation}");
            assert_eq!(
                plan.attested_operations,
                vec![
                    "list_learning_candidates".to_owned(),
                    "read_learning_candidate".to_owned()
                ],
                "the attested set is exactly the two learning review reads"
            );
            assert_eq!(
                reviewed_app_transport_input_ceiling(&plan),
                Some(APP_BOUND_LEARNING_READ_INPUT_CEILING)
            );
            assert_eq!(
                reviewed_app_transport_result_ceiling(&plan),
                Some(APP_BOUND_LEARNING_READ_RESULT_CEILING)
            );
            let target = plan
                .mint(AppReference::parse("capability:internal_data").unwrap())
                .expect("the learning review read mints a scoped substrate-read receipt");
            assert_eq!(
                target.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:internal-data-learning-read:v1")
            );
            assert_eq!(
                target.result_policy(),
                super::super::tool_disclosure::AppToolResultPolicy::ReviewedScopedHostRead
            );
        }
        // The operation-less call defaults to the bounded list read — the
        // least-authority default `http` gets from GET — so the descriptor's
        // aggregate dispatch is Ready rather than conditional.
        let default_read = plan_app_tool_call(
            "internal_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(default_read.io_kind, AppToolIoKind::BoundHostRead);
        assert!(default_read.runnable);
        for operation in [
            Some("catalog"),
            Some("schema"),
            Some("query_events"),
            Some("list_llm_traces"),
            Some("read_audio_note"),
            Some("list_learning_procedures"),
        ] {
            let plan = plan_app_tool_call(
                "internal_data",
                operation,
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{operation:?}");
            assert!(!plan.runnable, "{operation:?}");
        }
    }

    #[test]
    fn other_host_read_tools_stay_refused_after_the_learning_read_binder() {
        // `list_tasks` / `search_memory` classify as host reads from their
        // declared shapes, but no binder admits them: the refusal the catalog
        // documents ("Magician store reads stay refused") survives everywhere
        // outside the two `internal_data` learning review reads.
        for tool in ["list_tasks", "search_memory", "working_set_search"] {
            let plan = plan_app_tool_call(tool, None, AppToolContainProfile::InProcessCompiled);
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{tool}");
            assert!(!plan.runnable, "{tool}");
            assert!(
                plan.mint(AppReference::parse(format!("capability:{tool}")).unwrap())
                    .is_none(),
                "{tool} never mints a receipt"
            );
        }
    }

    #[test]
    fn internal_data_app_calls_bind_the_routing_key_to_the_planned_operation() {
        // The provider routes on `__action_name` and would silently default to
        // the broad `catalog` action; the app path must never leave that key
        // absent or divergent from the classified operation.
        let plan = plan_app_tool_call(
            "internal_data",
            Some("list_learning_candidates"),
            AppToolContainProfile::InProcessCompiled,
        );
        let mut parameters = HashMap::new();
        parameters.insert(
            "operation".to_owned(),
            Value::String("list_learning_candidates".to_owned()),
        );
        let bound = bind_parameters_for_call(&plan, &parameters, None)
            .expect("the routing key is seeded from the planned operation");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_learning_candidates")
        );
        // An operation spelled with the tool-qualified prefix still binds to
        // the bare action the provider routes on.
        let mut qualified = HashMap::new();
        qualified.insert(
            "operation".to_owned(),
            Value::String("internal_data__list_learning_candidates".to_owned()),
        );
        let bound = bind_parameters_for_call(&plan, &qualified, None)
            .expect("the qualified spelling normalizes to the same routing key");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_learning_candidates")
        );
        // A plan without an operation binds to the bounded list default, so
        // the provider's broad `catalog` default is unreachable on the app
        // path.
        let unclassified = plan_app_tool_call(
            "internal_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&unclassified, &parameters, None)
            .expect("the operation-less call binds to the list default");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_learning_candidates")
        );
    }

    #[test]
    fn thinking_maps_data_admits_only_the_two_scoped_reads_as_host_reads() {
        // The second host-read binder (Phase 4 Brainstorm re-open — the 2.5
        // pattern generalized): exactly the two bounded thinking-map reads
        // are runnable, and they mint a conservative local-content receipt
        // under their own runtime ref. Every other `thinking_maps_data`
        // action — every mutation verb the first-party surface serves —
        // stays unclassified and unrunnable for apps.
        for operation in ["list_maps", "read_map"] {
            let plan = plan_app_tool_call(
                "thinking_maps_data",
                Some(operation),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{operation}");
            assert!(plan.runnable, "{operation}");
            assert_eq!(
                plan.attested_operations,
                vec!["list_maps".to_owned(), "read_map".to_owned()],
                "the attested set is exactly the two thinking-map reads"
            );
            assert_eq!(
                reviewed_app_transport_input_ceiling(&plan),
                Some(APP_BOUND_THINKING_MAP_READ_INPUT_CEILING)
            );
            assert_eq!(
                reviewed_app_transport_result_ceiling(&plan),
                Some(APP_BOUND_THINKING_MAP_READ_RESULT_CEILING)
            );
            assert!(app_effect_owner_supported(&plan));
            assert_eq!(
                compiled_app_provider_implementation_identity("thinking_maps_data"),
                Some("magician.compiled-provider.thinking-maps-read.v1")
            );
            let target = plan
                .mint(AppReference::parse("capability:thinking_maps_data").unwrap())
                .expect("the thinking-map read mints a scoped substrate-read receipt");
            assert_eq!(
                target.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:thinking-maps-read:v1")
            );
            assert_eq!(
                target.result_policy(),
                super::super::tool_disclosure::AppToolResultPolicy::ReviewedScopedHostRead
            );
        }
        // The operation-less call defaults to the bounded list read — the
        // least-authority default `http` gets from GET — so the descriptor's
        // aggregate dispatch is Ready rather than conditional.
        let default_read = plan_app_tool_call(
            "thinking_maps_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(default_read.io_kind, AppToolIoKind::BoundHostRead);
        assert!(default_read.runnable);
        let refused_tool = AppReference::parse("capability:thinking_maps_data").unwrap();
        for operation in [
            Some("create_map"),
            Some("apply_operations"),
            Some("patch_map"),
            Some("delete_map"),
            Some("export_markdown"),
            Some("interpret"),
            Some("consolidate"),
        ] {
            let plan = plan_app_tool_call(
                "thinking_maps_data",
                operation,
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{operation:?}");
            assert!(!plan.runnable, "{operation:?}");
            assert!(
                plan.mint(refused_tool.clone()).is_none(),
                "{operation:?} never mints a receipt"
            );
        }
    }

    #[test]
    fn thinking_maps_data_app_calls_bind_the_routing_key_to_the_planned_operation() {
        // The provider routes on `__action_name`; the app path must never
        // leave that key absent or divergent from the classified operation
        // (the same closing as internal_data's catalog-shaped miss).
        let plan = plan_app_tool_call(
            "thinking_maps_data",
            Some("read_map"),
            AppToolContainProfile::InProcessCompiled,
        );
        let mut parameters = HashMap::new();
        parameters.insert("map_id".to_owned(), Value::String("map-1".to_owned()));
        let bound = bind_parameters_for_call(&plan, &parameters, None)
            .expect("the routing key is seeded from the planned operation");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("read_map")
        );
        // An operation spelled with the tool-qualified prefix still binds to
        // the bare action the provider routes on.
        let mut qualified = HashMap::new();
        qualified.insert(
            "operation".to_owned(),
            Value::String("thinking_maps_data__list_maps".to_owned()),
        );
        let plan = plan_app_tool_call(
            "thinking_maps_data",
            Some("list_maps"),
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&plan, &qualified, None)
            .expect("the qualified spelling normalizes to the same routing key");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_maps")
        );
        // A plan without an operation binds to the bounded list default.
        let unclassified = plan_app_tool_call(
            "thinking_maps_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&unclassified, &parameters, None)
            .expect("the operation-less call binds to the list default");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_maps")
        );
    }

    #[test]
    fn evidence_data_admits_exactly_five_scoped_host_reads() {
        let admitted = [
            "list_pending_claims",
            "read_claim",
            "list_evidence_records",
            "list_entities",
            "list_commitments",
        ];
        let attested = [
            "list_commitments".to_owned(),
            "list_entities".to_owned(),
            "list_evidence_records".to_owned(),
            "list_pending_claims".to_owned(),
            "read_claim".to_owned(),
        ];
        for operation in admitted {
            let plan = plan_app_tool_call(
                "evidence_data",
                Some(operation),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{operation}");
            assert!(plan.runnable, "{operation}");
            assert_eq!(
                reviewed_app_transport_input_ceiling(&plan),
                Some(APP_BOUND_EVIDENCE_DATA_INPUT_CEILING)
            );
            assert_eq!(
                reviewed_app_transport_result_ceiling(&plan),
                Some(APP_BOUND_EVIDENCE_DATA_RESULT_CEILING)
            );
            assert_eq!(plan.attested_operations, attested);
            assert!(app_effect_owner_supported(&plan));
            let target = plan
                .mint(AppReference::parse("capability:evidence_data").unwrap())
                .expect("evidence_data host reads mint local-content receipts");
            assert_eq!(
                target.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:evidence-data-read:v1")
            );
            assert_eq!(
                target.result_policy(),
                super::super::tool_disclosure::AppToolResultPolicy::ReviewedScopedHostRead
            );
        }
        let default_read = plan_app_tool_call(
            "evidence_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(default_read.runnable);
        assert_eq!(default_read.io_kind, AppToolIoKind::BoundHostRead);

        for refused in [
            "confirm_claim",
            "reject_claim",
            "record_commitment",
            "confirm_commitment",
            "ingest_transcript",
            "correct_evidence",
            "merge_entities",
        ] {
            let plan = plan_app_tool_call(
                "evidence_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
            assert!(
                plan.mint(AppReference::parse("capability:evidence_data").unwrap())
                    .is_none(),
                "{refused} never mints an attestation"
            );
        }
    }

    #[test]
    fn evidence_data_app_calls_bind_exact_routing_action() {
        let plan = plan_app_tool_call(
            "evidence_data",
            Some("list_entities"),
            AppToolContainProfile::InProcessCompiled,
        );
        let parameters = HashMap::from([(
            "agent_id".to_owned(),
            Value::String("review-agent".to_owned()),
        )]);
        let bound = bind_parameters_for_call(&plan, &parameters, None)
            .expect("the classified operation seeds the provider routing key");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_entities")
        );

        let default_plan = plan_app_tool_call(
            "evidence_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&default_plan, &HashMap::new(), None)
            .expect("operation-less evidence_data defaults to the pending queue");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_pending_claims")
        );
    }

    #[test]
    fn meetings_data_app_plan_admits_only_the_six_reads() {
        for read in [
            "active_session",
            "list_threads",
            "read_thread",
            "read_takeaways",
            "upcoming_meetings",
            "search_meeting_memory",
        ] {
            let plan = plan_app_tool_call(
                "meetings_data",
                Some(read),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{read}");
            assert!(plan.runnable, "{read}");
            assert!(app_effect_owner_supported(&plan), "{read}");
            let minted = plan
                .mint(AppReference::parse("capability:meetings_data").unwrap())
                .unwrap_or_else(|| panic!("{read} mints a trusted-local receipt"));
            assert_eq!(
                minted.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:meetings-data-read:v1")
            );
        }

        let default_read = plan_app_tool_call(
            "meetings_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(default_read.runnable);
        assert_eq!(default_read.io_kind, AppToolIoKind::BoundHostRead);

        // Capture control is a separate signed destination. No control verb —
        // and no invented read — may inherit the binder's class.
        for refused in [
            "listen",
            "join",
            "pause",
            "resume",
            "stop",
            "start_capture",
            "delete_thread",
            "write_transcript",
        ] {
            let plan = plan_app_tool_call(
                "meetings_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
            assert!(
                plan.mint(AppReference::parse("capability:meetings_data").unwrap())
                    .is_none(),
                "{refused} never mints an attestation"
            );
        }
    }

    #[test]
    fn meetings_data_app_calls_bind_exact_routing_action() {
        let plan = plan_app_tool_call(
            "meetings_data",
            Some("read_takeaways"),
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&plan, &HashMap::new(), None)
            .expect("the classified operation seeds the provider routing key");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("read_takeaways")
        );

        let default_plan = plan_app_tool_call(
            "meetings_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        let bound = bind_parameters_for_call(&default_plan, &HashMap::new(), None)
            .expect("operation-less meetings_data defaults to the live-capture read");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("active_session")
        );
    }

    #[test]
    fn agent_roster_data_admits_only_the_two_reads_and_no_definition_edit() {
        for read in ["list_members", "read_member"] {
            let plan = plan_app_tool_call(
                "agent_roster_data",
                Some(read),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{read}");
            assert!(app_effect_owner_supported(&plan), "{read}");
            let minted = plan
                .mint(AppReference::parse("capability:agent_roster_data").unwrap())
                .unwrap_or_else(|| panic!("{read} mints a trusted-local receipt"));
            assert_eq!(
                minted.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:agent-roster-read:v1")
            );
        }

        let default_read = plan_app_tool_call(
            "agent_roster_data",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(default_read.runnable);
        assert_eq!(default_read.io_kind, AppToolIoKind::BoundHostRead);
        let bound = bind_parameters_for_call(&default_read, &HashMap::new(), None)
            .expect("operation-less agent_roster_data defaults to the bounded list");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_members")
        );

        // Editing a definition is the agent owner's job and reaches a different
        // substrate; no such verb may inherit the roster read's class.
        for refused in [
            "create_agent",
            "update_agent",
            "retire_agent",
            "set_opted_out",
            "get_agent_details",
            "catalog",
        ] {
            let plan = plan_app_tool_call(
                "agent_roster_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
            assert!(
                plan.mint(AppReference::parse("capability:agent_roster_data").unwrap())
                    .is_none(),
                "{refused} never mints an attestation"
            );
        }
    }

    #[test]
    fn tasks_data_admits_only_the_two_reads_and_no_task_change() {
        for read in ["list_tasks", "read_task"] {
            let plan = plan_app_tool_call(
                "tasks_data",
                Some(read),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{read}");
            assert!(app_effect_owner_supported(&plan), "{read}");
            assert!(
                reviewed_app_transport_input_ceiling(&plan).is_some(),
                "{read}"
            );
            assert!(
                reviewed_app_transport_result_ceiling(&plan).is_some(),
                "{read}"
            );
            let minted = plan
                .mint(AppReference::parse("capability:tasks_data").unwrap())
                .unwrap_or_else(|| panic!("{read} mints a scoped host-read receipt"));
            assert_eq!(
                minted.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:tasks-read:v1")
            );
        }

        let default_read =
            plan_app_tool_call("tasks_data", None, AppToolContainProfile::InProcessCompiled);
        assert!(default_read.runnable);
        let bound = bind_parameters_for_call(&default_read, &HashMap::new(), None)
            .expect("operation-less tasks_data defaults to the bounded list");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("list_tasks")
        );

        for refused in [
            "create_task",
            "update_task",
            "delete_task",
            "run_task",
            "stop_task",
            "get_task_details",
            "catalog",
        ] {
            let plan = plan_app_tool_call(
                "tasks_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
            assert!(
                plan.mint(AppReference::parse("capability:tasks_data").unwrap())
                    .is_none(),
                "{refused} never mints an attestation"
            );
        }
    }

    #[test]
    fn notes_data_admits_only_search_and_read_and_no_note_write() {
        for read in ["search_notes", "read_note"] {
            let plan = plan_app_tool_call(
                "notes_data",
                Some(read),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{read}");
            assert!(app_effect_owner_supported(&plan), "{read}");
            let minted = plan
                .mint(AppReference::parse("capability:notes_data").unwrap())
                .unwrap_or_else(|| panic!("{read} mints a scoped host-read receipt"));
            assert_eq!(
                minted.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:notes-read:v1")
            );
        }
        for refused in [
            "create_note",
            "append_note",
            "open_note",
            "save_selection_to_note",
            "catalog",
        ] {
            let plan = plan_app_tool_call(
                "notes_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
        }
    }

    #[test]
    fn memory_data_admits_only_its_two_reads_and_no_memory_write() {
        for read in ["search_memory", "read_entry"] {
            let plan = plan_app_tool_call(
                "memory_data",
                Some(read),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::BoundHostRead, "{read}");
            assert!(app_effect_owner_supported(&plan), "{read}");
            let minted = plan
                .mint(AppReference::parse("capability:memory_data").unwrap())
                .unwrap_or_else(|| panic!("{read} mints a scoped host-read receipt"));
            assert_eq!(
                minted.runtime_ref().map(ToString::to_string).as_deref(),
                Some("runtime:compiled:memory-read:v1")
            );
        }
        for refused in [
            "save_preference",
            "forget_memory",
            "update_memory_tier",
            "catalog",
        ] {
            let plan = plan_app_tool_call(
                "memory_data",
                Some(refused),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(plan.io_kind, AppToolIoKind::Unbound, "{refused}");
            assert!(!plan.runnable, "{refused}");
        }
    }

    #[test]
    fn each_host_read_binder_keeps_its_own_implementation_identity() {
        // Widening one binder must never rotate another installed package's
        // lock, so the four identities are distinct by construction.
        let identities = [
            "internal_data",
            "thinking_maps_data",
            "evidence_data",
            "meetings_data",
            "agent_roster_data",
            "tasks_data",
            "notes_data",
            "memory_data",
        ]
        .map(|tool| {
            compiled_app_provider_implementation_identity(tool)
                .unwrap_or_else(|| panic!("{tool} has an app implementation identity"))
        });
        let mut unique = identities.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), identities.len());
    }

    #[test]
    fn mint_emits_distinct_receipts_for_time_math_ops() {
        let tool = AppReference::parse("capability:time_math").unwrap();
        let range = plan_app_tool_call(
            "time_math",
            Some("date_range"),
            AppToolContainProfile::InProcessCompiled,
        )
        .mint(tool.clone())
        .expect("date_range mints");
        assert_eq!(
            range.runtime_ref().map(ToString::to_string).as_deref(),
            Some("runtime:compiled:time_math:v1")
        );
        assert_eq!(
            range.result_policy(),
            super::super::tool_disclosure::AppToolResultPolicy::PureTransformInheritsInput
        );
        let now = plan_app_tool_call(
            "time_math",
            Some("now"),
            AppToolContainProfile::InProcessCompiled,
        )
        .mint(tool)
        .expect("now mints");
        assert_eq!(
            now.runtime_ref().map(ToString::to_string).as_deref(),
            Some("runtime:compiled:time_math:now:v1")
        );
        assert_eq!(
            now.result_policy(),
            super::super::tool_disclosure::AppToolResultPolicy::TrustedLocalClock
        );
    }

    #[test]
    fn compiled_packs_classify_from_declared_schema() {
        let web_fetch =
            plan_app_tool_call("web_fetch", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(web_fetch.io_kind, AppToolIoKind::BoundHttp);
        assert!(!web_fetch.runnable);

        let content_read = plan_app_tool_call(
            "content_read",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(content_read.io_kind, AppToolIoKind::BoundHttp);

        let web_search =
            plan_app_tool_call("web_search", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(web_search.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!web_search.runnable);
        let content_search = plan_app_tool_call(
            "content_search",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(content_search.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!content_search.runnable);

        let create_task = plan_app_tool_call(
            "create_task",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(create_task.io_kind, AppToolIoKind::BoundSideEffect);

        let list_tasks =
            plan_app_tool_call("list_tasks", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(list_tasks.io_kind, AppToolIoKind::BoundHostRead);

        let search_memory = plan_app_tool_call(
            "search_memory",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(search_memory.io_kind, AppToolIoKind::BoundHostRead);

        let working_set = plan_app_tool_call(
            "working_set_read",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(working_set.io_kind, AppToolIoKind::BoundHostRead);

        let read_file =
            plan_app_tool_call("read_file", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(read_file.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!read_file.runnable);

        let write_file =
            plan_app_tool_call("write_file", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(write_file.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!write_file.runnable);

        let grep = plan_app_tool_call("grep", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(grep.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!grep.runnable);
        let glob = plan_app_tool_call("glob", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(glob.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!glob.runnable);

        let catchup = plan_app_tool_call(
            "catchup_merge",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(catchup.io_kind, AppToolIoKind::PureTransform);
        assert!(!catchup.runnable);
        assert!(!app_effect_owner_supported(&catchup));

        let macos = plan_app_tool_call(
            "macos_automation",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(macos.io_kind, AppToolIoKind::Device);
        let android = plan_app_tool_call(
            "android_act",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(android.io_kind, AppToolIoKind::Device);
        let preview = plan_app_tool_call(
            "screenshot_preview",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(preview.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!preview.runnable);
    }

    #[test]
    fn files_http_duckdb_split_by_operation() {
        let files_write = plan_app_tool_call(
            "files",
            Some("write"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(files_write.io_kind, AppToolIoKind::BoundWrite);
        let files_read = plan_app_tool_call(
            "files",
            Some("read"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(files_read.io_kind, AppToolIoKind::BoundFile);

        let http_post = plan_app_tool_call(
            "http",
            Some("post"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(http_post.io_kind, AppToolIoKind::BoundSideEffect);
        let http_get = plan_app_tool_call(
            "http",
            Some("get"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(http_get.io_kind, AppToolIoKind::BoundHttp);

        let preview = plan_app_tool_call(
            "duckdb",
            Some("preview"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(preview.io_kind, AppToolIoKind::BoundTable);
        let parquet = plan_app_tool_call(
            "duckdb",
            Some("read_parquet"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(parquet.io_kind, AppToolIoKind::BoundTable);
        let query = plan_app_tool_call(
            "duckdb",
            Some("query"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(query.io_kind, AppToolIoKind::Unbound);
    }

    #[test]
    fn skill_families_classify_from_shape_not_a_name_table() {
        let future_search = plan_app_tool_call_with_shape(
            "brand-new-paper-search",
            None,
            AppToolContainProfile::OsJail,
            Some(AppToolDeclaredShape {
                categories: vec!["research".into(), "search".into()],
                composition_category: Some("research".into()),
                protocol: Some("cli".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(future_search.io_kind, AppToolIoKind::BoundHttp);
        assert!(!future_search.runnable);
        assert!(future_search.reason.contains("egress destination"));

        // The same shape with one declared egress host runs in the
        // brokered-egress jail.
        let declared = plan_app_tool_call_with_shape(
            "brand-new-paper-search",
            None,
            AppToolContainProfile::OsJail,
            Some(AppToolDeclaredShape {
                categories: vec!["research".into(), "search".into()],
                composition_category: Some("research".into()),
                protocol: Some("cli".into()),
                app_egress: super::super::os_jail_egress::parse_app_egress_declaration(
                    "---\nname: s\ndescription: d\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 1\n      destination: api.example.com\n---\n",
                )
                .unwrap(),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(declared.io_kind, AppToolIoKind::BoundHttp);
        assert!(declared.runnable);
        assert!(app_effect_owner_supported(&declared));
        assert!(declared.reason.contains("brokered-egress"));

        let merge = plan_app_tool_call_with_shape(
            "whatsgoingon2",
            None,
            AppToolContainProfile::OsJail,
            Some(AppToolDeclaredShape {
                categories: vec!["research".into(), "merge".into()],
                composition_category: Some("research".into()),
                protocol: Some("cli".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(merge.io_kind, AppToolIoKind::PureTransform);
        assert!(merge.runnable);
        assert!(app_effect_owner_supported(&merge));

        let commerce_shape = AppToolDeclaredShape {
            categories: vec!["commerce".into(), "shopping".into()],
            composition_category: Some("commerce_operations".into()),
            protocol: Some("mcp".into()),
            ..AppToolDeclaredShape::default()
        };
        let commerce = plan_app_tool_call_with_shape(
            "swiggy-mcp",
            None,
            AppToolContainProfile::OsJail,
            Some(commerce_shape.clone()),
        );
        assert_eq!(commerce.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!app_effect_owner_supported(&commerce));
        assert_eq!(
            tool_skill_contain_profile(&commerce_shape),
            AppToolContainProfile::GovernedMcp
        );
        let governed = plan_app_tool_call_with_shape(
            "swiggy-mcp",
            Some("call_tool"),
            AppToolContainProfile::GovernedMcp,
            Some(commerce_shape),
        );
        assert_eq!(governed.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(governed.runnable);
        assert!(app_effect_owner_supported(&governed));

        let youtube = plan_app_tool_call_with_shape(
            "youtube-search",
            None,
            AppToolContainProfile::OsJail,
            Some(AppToolDeclaredShape {
                categories: vec!["research".into(), "search".into()],
                composition_category: Some("research".into()),
                protocol: Some("cli".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(youtube.io_kind, AppToolIoKind::BoundHttp);
    }

    #[test]
    fn host_read_comes_from_schema_not_the_tool_name() {
        let named_search = plan_app_tool_call_with_shape(
            "invoice_search",
            None,
            AppToolContainProfile::InProcessCompiled,
            Some(AppToolDeclaredShape {
                compiled: true,
                categories: vec!["search".into()],
                composition_category: Some("action".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(named_search.io_kind, AppToolIoKind::BoundSideEffect);
        assert!(!named_search.runnable);

        let named_list = plan_app_tool_call_with_shape(
            "list_invoices",
            None,
            AppToolContainProfile::InProcessCompiled,
            Some(AppToolDeclaredShape {
                compiled: true,
                categories: vec!["billing".into()],
                composition_category: Some("action".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(named_list.io_kind, AppToolIoKind::BoundSideEffect);

        let schema_read = plan_app_tool_call_with_shape(
            "totally_new_store_lookup",
            None,
            AppToolContainProfile::InProcessCompiled,
            Some(AppToolDeclaredShape {
                compiled: true,
                categories: vec!["introspection".into()],
                composition_category: Some("action".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(schema_read.io_kind, AppToolIoKind::BoundHostRead);
        assert!(!schema_read.runnable);

        let no_shape = plan_app_tool_call_with_shape(
            "brand-new-paper-search",
            None,
            AppToolContainProfile::OsJail,
            None,
        );
        assert_eq!(no_shape.io_kind, AppToolIoKind::Unbound);

        let named_android = plan_app_tool_call_with_shape(
            "android_invoice",
            None,
            AppToolContainProfile::InProcessCompiled,
            Some(AppToolDeclaredShape {
                compiled: true,
                categories: vec!["billing".into()],
                composition_category: Some("action".into()),
                ..AppToolDeclaredShape::default()
            }),
        );
        assert_eq!(named_android.io_kind, AppToolIoKind::BoundSideEffect);
    }

    #[test]
    fn skill_frontmatter_drives_classification() {
        let source = "---\nname: future-arxiv-search\nmetadata:\n  magician:\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires:\n        bins: [future-arxiv-search]\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_catalog:\n      categories: [research, search]\n      composition_category: research\n---\n";
        let shape = AppToolDeclaredShape::from_skill_source(source).expect("shape");
        let plan = plan_app_tool_call_with_shape(
            "future-arxiv-search",
            None,
            AppToolContainProfile::OsJail,
            Some(shape),
        );
        assert_eq!(plan.io_kind, AppToolIoKind::BoundHttp);
    }

    #[test]
    fn bound_http_url_evidence_cannot_mint_without_a_retained_dns_pin() {
        let plan = plan_app_tool_call(
            "http",
            Some("get"),
            AppToolContainProfile::InProcessCompiled,
        );
        let tool = AppReference::parse("capability:http").unwrap();
        let now = Utc::now();
        let mut params = HashMap::new();
        params.insert("url".into(), Value::String("https://example.com/a".into()));
        assert!(plan
            .mint_with_evidence(
                tool.clone(),
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: None,
                    now,
                }),
            )
            .is_none());
        params.insert(
            "url".into(),
            Value::String("http://localhost/secret".into()),
        );
        assert!(plan
            .mint_with_evidence(
                tool.clone(),
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: None,
                    now,
                }),
            )
            .is_none());
        params.insert("url".into(), Value::String("https://a.example/x".into()));
        params.insert(
            "requests".into(),
            Value::Array(vec![serde_json::json!({"url":"https://b.example/y"})]),
        );
        assert!(plan
            .mint_with_evidence(
                tool,
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: None,
                    now,
                }),
            )
            .is_none());
    }

    #[test]
    fn bound_file_path_evidence_cannot_mint_without_retained_descriptors() {
        let dir = std::env::temp_dir().join(format!("magician-app-bind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "files",
            Some("read"),
            AppToolContainProfile::InProcessCompiled,
        );
        let tool = AppReference::parse("capability:files").unwrap();
        let now = Utc::now();
        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("note.txt".into()));
        assert!(plan
            .mint_with_evidence(
                tool.clone(),
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: Some(&dir),
                    now,
                }),
            )
            .is_none());
        params.insert("path".into(), Value::String("../escape.txt".into()));
        assert!(plan
            .mint_with_evidence(
                tool.clone(),
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: Some(&dir),
                    now,
                }),
            )
            .is_none());
        assert!(plan.mint(tool).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catchup_merge_stays_blocked_without_a_reviewed_physical_owner() {
        let tool = AppReference::parse("capability:catchup_merge").unwrap();
        let plan = plan_app_tool_call(
            "catchup_merge",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(!plan.runnable);
        assert!(plan.mint(tool).is_none());
    }

    #[test]
    fn bind_parameters_preserves_normalized_relative_paths_and_rejects_escapes() {
        let dir =
            std::env::temp_dir().join(format!("magician-app-bind-rewrite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "files",
            Some("read"),
            AppToolContainProfile::InProcessCompiled,
        );
        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("note.txt".into()));
        let bound = bind_parameters_for_call(&plan, &params, Some(&dir)).expect("rewrite");
        assert_eq!(bound.get("action").and_then(Value::as_str), Some("read"));
        let rewritten = bound.get("path").and_then(Value::as_str).unwrap();
        assert_eq!(rewritten, "note.txt");
        params.insert("path".into(), Value::String("../escape.txt".into()));
        assert!(bind_parameters_for_call(&plan, &params, Some(&dir)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_without_action_infers_read() {
        let dir = std::env::temp_dir().join(format!(
            "magician-app-bind-files-default-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call("files", None, AppToolContainProfile::InProcessCompiled);
        assert_eq!(plan.io_kind, AppToolIoKind::BoundFile);
        assert!(plan.runnable);
        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("note.txt".into()));
        let bound = bind_parameters_for_call(&plan, &params, Some(&dir)).expect("infer");
        assert_eq!(bound.get("action").and_then(Value::as_str), Some("read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duckdb_bind_normalizes_paths_but_physical_proof_refuses_database_authority() {
        let dir =
            std::env::temp_dir().join(format!("magician-app-bind-duckdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "duckdb",
            Some("preview"),
            AppToolContainProfile::InProcessCompiled,
        );
        let mut params = HashMap::new();
        params.insert("source".into(), Value::String("sales.csv".into()));
        params.insert("database".into(), Value::String("store.duckdb".into()));
        let bound = bind_parameters_for_call(&plan, &params, Some(&dir)).expect("rewrite");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("preview")
        );
        let database = bound.get("database").and_then(Value::as_str).unwrap();
        assert_eq!(database, "store.duckdb");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Closed canonical surface (app-authority remediation move 3) ─────────
    //
    // The defect class these pin: the layer that CLASSIFIES a call and the
    // layer that EXECUTES it read different fields, so a call could be
    // classified as a read and performed as a mutation.

    #[test]
    fn divergent_operation_aliases_are_refused() {
        // C1 exactly: classification reads `operation`, the http provider reads
        // `method`. Naming the operation twice, differently, is refused.
        let mut params = HashMap::new();
        params.insert(
            "url".into(),
            Value::String("https://api.example.com/x".into()),
        );
        params.insert("operation".into(), Value::String("get".into()));
        params.insert("method".into(), Value::String("DELETE".into()));
        let divergence = divergent_operation_alias(&params);
        assert!(
            divergence.is_some(),
            "a get/DELETE alias split must be refused, not silently resolved by the \
             classifier's fallback order"
        );

        // The files sibling: `operation` vs `action`.
        let mut params = HashMap::new();
        params.insert("operation".into(), Value::String("read".into()));
        params.insert("action".into(), Value::String("delete".into()));
        assert!(divergent_operation_alias(&params).is_some());
    }

    #[test]
    fn agreeing_operation_aliases_are_allowed() {
        // Honest calls must keep working, including the tool-qualified spelling
        // (`http_get`) the flat catalog injects alongside a bare `method`.
        let mut params = HashMap::new();
        params.insert(
            "url".into(),
            Value::String("https://api.example.com/x".into()),
        );
        params.insert("method".into(), Value::String("GET".into()));
        assert!(divergent_operation_alias(&params).is_none());

        let mut params = HashMap::new();
        params.insert("__action_name".into(), Value::String("http_get".into()));
        params.insert("method".into(), Value::String("get".into()));
        assert!(
            divergent_operation_alias(&params).is_none(),
            "`http_get` and `get` name the same operation"
        );

        // One alias only, and no aliases at all, are both fine.
        let mut params = HashMap::new();
        params.insert("operation".into(), Value::String("preview".into()));
        assert!(divergent_operation_alias(&params).is_none());
        assert!(divergent_operation_alias(&HashMap::new()).is_none());
    }

    #[test]
    fn lowered_action_decides_the_class_for_http_and_files() {
        use crate::magician_v2::execution::actions::{
            ExecutableAction, FileAction, HttpAction, HttpMethod,
        };
        let http = |method| {
            ExecutableAction::Http(HttpAction {
                method,
                ..HttpAction::get("https://api.example.com/x")
            })
        };
        assert_eq!(
            classify_lowered_action(&http(HttpMethod::Get)),
            Some(AppToolIoKind::BoundHttp)
        );
        assert_eq!(
            classify_lowered_action(&http(HttpMethod::Delete)),
            Some(AppToolIoKind::BoundSideEffect),
            "a lowered DELETE is a side effect however the call described itself"
        );

        assert_eq!(
            classify_lowered_action(&ExecutableAction::File(FileAction::Read {
                path: "/tmp/x".into(),
                encoding: None,
            })),
            Some(AppToolIoKind::BoundFile)
        );
        assert_eq!(
            classify_lowered_action(&ExecutableAction::File(FileAction::Delete {
                path: "/tmp/x".into(),
                recursive: false,
            })),
            Some(AppToolIoKind::BoundWrite),
            "a lowered delete is a write however the call described itself"
        );
    }

    #[test]
    fn duckdb_free_form_sql_fragments_are_refused() {
        // C2: `select` / `where_clause` are spliced into the generated SQL with
        // only `path` escaped, so they can comment out the generated FROM and
        // read arbitrary host files. A SQL string in interpreter position is
        // never bindable — it is refused rather than escaped.
        let dir = std::env::temp_dir().join(format!(
            "magician-app-bind-duckdb-sql-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "duckdb",
            Some("read_parquet"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(plan.io_kind, AppToolIoKind::BoundTable);

        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("data.parquet".into()));
        params.insert(
            "select".into(),
            Value::String("* FROM read_text('/etc/passwd') AS t(line) --".into()),
        );
        assert!(
            bind_parameters_for_call(&plan, &params, Some(&dir)).is_none(),
            "an injected `select` must be refused"
        );

        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("data.parquet".into()));
        params.insert("where_clause".into(), Value::String("1=1 --".into()));
        assert!(
            bind_parameters_for_call(&plan, &params, Some(&dir)).is_none(),
            "an injected `where_clause` must be refused"
        );

        // The same call without the fragments still works.
        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("data.parquet".into()));
        assert!(bind_parameters_for_call(&plan, &params, Some(&dir)).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duckdb_action_name_is_forced_to_the_planned_operation() {
        // Sibling B: a caller-supplied `__action_name` used to survive binding,
        // so `{operation: "preview", __action_name: "query", sql: …}` classified
        // BoundTable and then lowered raw SQL.
        let dir = std::env::temp_dir().join(format!(
            "magician-app-bind-duckdb-force-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "duckdb",
            Some("preview"),
            AppToolContainProfile::InProcessCompiled,
        );
        let mut params = HashMap::new();
        params.insert("source".into(), Value::String("sales.csv".into()));
        params.insert("__action_name".into(), Value::String("query".into()));
        let bound = bind_parameters_for_call(&plan, &params, Some(&dir)).expect("bind");
        assert_eq!(
            bound.get("__action_name").and_then(Value::as_str),
            Some("preview"),
            "the provider must route on the operation the classifier judged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_mutating_file_alias_the_executor_accepts_classifies_as_a_write() {
        // `lowering::canonical_file_tool` accepts several spellings per action.
        // This list once named only delete/move/mkdir, so `{action: "remove"}`
        // classified BoundFile — a runnable READ — and then deleted the file.
        // If the executor learns a new alias it must be added here too; that is
        // what this test is for.
        for alias in [
            "write",
            "append",
            "delete",
            "remove",
            "copy",
            "move",
            "rename",
            "mkdir",
            "create_dir",
            "create_directory",
            // The executor normalises `-` to `_` and lowercases; so must this.
            "create-dir",
            "REMOVE",
        ] {
            let plan = plan_app_tool_call(
                "files",
                Some(alias),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(
                plan.io_kind,
                AppToolIoKind::BoundWrite,
                "`{alias}` mutates the filesystem and must not classify as a read"
            );
        }
        for alias in ["read", "exists", "list"] {
            let plan = plan_app_tool_call(
                "files",
                Some(alias),
                AppToolContainProfile::InProcessCompiled,
            );
            assert_eq!(
                plan.io_kind,
                AppToolIoKind::BoundFile,
                "`{alias}` only reads"
            );
        }
    }

    #[test]
    fn files_action_is_seeded_from_the_planned_operation() {
        // The classifier judged BoundWrite from `operation`; `lower_file_action`
        // routes on `action`. Seeding `action` from the plan keeps them naming
        // one operation instead of classifying a write and lowering a read.
        let dir = std::env::temp_dir().join(format!(
            "magician-app-bind-files-seed-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = plan_app_tool_call(
            "files",
            Some("write"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert_eq!(plan.io_kind, AppToolIoKind::BoundWrite);
        let mut params = HashMap::new();
        params.insert("path".into(), Value::String("note.txt".into()));
        params.insert("content".into(), Value::String("hello".into()));
        let bound = bind_parameters_for_call(&plan, &params, Some(&dir)).expect("bind");
        assert_eq!(bound.get("action").and_then(Value::as_str), Some("write"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_read_candidate_url_cannot_bypass_the_physical_http_owner() {
        let plan = plan_app_tool_call(
            "content_read",
            None,
            AppToolContainProfile::InProcessCompiled,
        );
        let tool = AppReference::parse("capability:content_read").unwrap();
        let mut params = HashMap::new();
        params.insert(
            "candidate".into(),
            serde_json::json!({"url":"https://example.com/doc"}),
        );
        assert!(plan
            .mint_with_evidence(
                tool,
                Some(AppToolBindEvidence {
                    parameters: &params,
                    workdir: None,
                    now: Utc::now(),
                }),
            )
            .is_none());
        assert_eq!(
            extract_bound_http_urls(&params),
            vec!["https://example.com/doc".to_owned()]
        );
    }

    #[test]
    fn common_app_owner_readiness_is_explicit_and_fail_closed() {
        let time = plan_app_tool_call(
            "time_math",
            Some("now"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(app_effect_owner_supported(&time));
        assert_eq!(
            compiled_app_provider_implementation_identity("time_math"),
            Some("magician.compiled-provider.time-math.v2")
        );
        assert_eq!(reviewed_app_transport_input_ceiling(&time), Some(8 * 1024));
        assert_eq!(reviewed_app_transport_result_ceiling(&time), Some(4 * 1024));

        let http = plan_app_tool_call(
            "http",
            Some("get"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(app_effect_owner_supported(&http));
        assert_eq!(
            compiled_app_provider_implementation_identity("http"),
            Some("magician.compiled-provider.bound-http-get.v1")
        );
        assert_eq!(
            reviewed_app_transport_input_ceiling(&http),
            Some(crate::magician_v2::apps::bound_http::APP_BOUND_HTTP_INPUT_CEILING)
        );
        assert_eq!(
            reviewed_app_transport_result_ceiling(&http),
            Some(crate::magician_v2::apps::bound_http::APP_BOUND_HTTP_RESULT_CEILING)
        );

        let file_read = plan_app_tool_call(
            "files",
            Some("read"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(app_effect_owner_supported(&file_read));
        assert_eq!(
            compiled_app_provider_implementation_identity("files"),
            Some("magician.compiled-provider.bound-file-read-write.v1")
        );
        let file_write = plan_app_tool_call(
            "files",
            Some("write"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(app_effect_owner_supported(&file_write));
        let table = plan_app_tool_call(
            "duckdb",
            Some("preview"),
            AppToolContainProfile::InProcessCompiled,
        );
        assert!(app_effect_owner_supported(&table));
        assert_eq!(
            compiled_app_provider_implementation_identity("duckdb"),
            Some("magician.compiled-provider.bound-table-preview-describe.v1")
        );

        for name in ["catchup_merge", "http_request"] {
            let plan = plan_app_tool_call(name, None, AppToolContainProfile::InProcessCompiled);
            assert!(!app_effect_owner_supported(&plan));
            assert_eq!(compiled_app_provider_implementation_identity(name), None);
        }
        for (tool, action) in [
            ("files", "append"),
            ("files", "delete"),
            ("duckdb", "query"),
            ("duckdb", "read_parquet"),
        ] {
            let plan =
                plan_app_tool_call(tool, Some(action), AppToolContainProfile::InProcessCompiled);
            assert!(!app_effect_owner_supported(&plan));
        }
        let jail = plan_app_tool_call("time_math", Some("now"), AppToolContainProfile::OsJail);
        assert!(!app_effect_owner_supported(&jail));
    }
}
