//! Immutable, bounded primitive descriptors for the app platform.
//!
//! This projection is deliberately internal. It gives authoring, review, lock
//! and dispatch owners one exact vocabulary without making the descriptor DTO
//! itself a supported public Apps API. Names are discovery conveniences only:
//! exact primitive and action identities, descriptor digests and source
//! digests are the material later authority boundaries must bind.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tool_runtime_core::action_overrides::compile_typed_action_overrides;
use tool_runtime_core::manifest::RuntimeProtocol;
use tool_runtime_core::manifest_parser::{
    parse_skill_expose, parse_skill_frontmatter, parse_skill_magician_extension,
    parse_skill_runtime_package,
};
use tool_runtime_core::manifest_validation::validate_skill_runtime_contract;
use tool_runtime_core::mcp_catalog_projection::project_mcp_catalog;

use super::app_tool_bind::{
    compiled_implementation_plan_digest, plan_app_tool_call_with_shape, tool_skill_contain_profile,
    AppToolContainProfile, AppToolDeclaredShape, AppToolIoKind,
};
use super::manifest::{
    normalized_collision_key, preflight_agent_definition_yaml, AppPackageLimits,
};
use super::models::{AppDigest, AppReference};
use super::tool_eligibility::{
    assess_app_tool_eligibility, assess_compiled_pack_eligibility, AppToolAdmissionSource,
};
use crate::magician_v2::agents::AgentAppToolContract;
use crate::magician_v2::execution::capability::{
    derive_param_schema_for_emission, NativeActionSchemaDef,
};
use crate::magician_v2::execution::CapabilityPackDefinition;

pub const APP_PRIMITIVE_DESCRIPTOR_VERSION: &str = "magician.app-primitive-descriptor.v2";
pub const MAX_APP_PRIMITIVES: usize = 512;
pub const MAX_ACTIONS_PER_PRIMITIVE: usize = 128;
pub const MAX_PRIMITIVE_SEARCH_RESULTS: usize = 32;
pub const MAX_PRIMITIVE_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_SEARCH_TEXT_BYTES: usize = 320;
const MAX_AGENT_DEFINITION_BYTES: usize = 256 * 1024;
const MAX_SEMANTIC_VERSION_BYTES: usize = 128;
const MAX_TOOL_SELECTOR_BYTES: usize = 192;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveKind {
    CompiledTool,
    ToolSkill,
    ProcedureSkill,
    Agent,
    Interactive,
}

impl AppPrimitiveKind {
    fn identity_segment(self) -> &'static str {
        match self {
            Self::CompiledTool => "platform",
            Self::ToolSkill => "tool-skill",
            Self::ProcedureSkill => "procedure",
            Self::Agent => "agent",
            Self::Interactive => "interactive",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveSourceKind {
    EmbeddedPlatform,
    ScopedSkill,
    ScopedAgent,
    PhysicalOwner,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveExecutionClass {
    InProcessCompiled,
    UniversalSkillRuntime,
    ProcedureInstructions,
    AgentTask,
    BrowserOwner,
    MacosHostOwner,
    AndroidDeviceOwner,
    /// Plan 1.3 experience class: draw reviewed storyboard payloads on the
    /// host overlay surface. Admission exists; no dispatch consumer is wired.
    OverlayDrawOwner,
    /// Plan 1.3 experience class: narrate bounded text through the reviewed
    /// TTS media rail. Admission exists; no dispatch consumer is wired.
    NarrationOwner,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveContainment {
    InProcess,
    OsJail,
    GovernedMcp,
    TaskOwner,
    BrowserSession,
    MacosHost,
    AndroidDevice,
    /// The host overlay drawing surface (first-party `/host/overlay/draw`
    /// owner). Receipt-only: no observation flows back.
    OverlaySurface,
    /// The host media rail (reviewed TTS provider chain). Receipt-only.
    MediaRail,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveEffect {
    Pure,
    ClockRead,
    NetworkRead,
    WorkspaceRead,
    WorkspaceWrite,
    StructuredDataRead,
    HostRead,
    ExternalMutation,
    DeviceInteraction,
    Undeclared,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveDispatchStatus {
    Ready,
    Conditional,
    Blocked,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveEligibilityStatus {
    Lockable,
    DiscoverableOnly,
    Blocked,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveInvocationMode {
    CallTool,
    RunProcedure,
    AgentAsTool,
    InteractiveAction,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPrimitiveSchemaState {
    Inline,
    Undeclared,
    RejectedByBound,
}

/// A full descriptor owns schema bytes; shallow search returns only its digest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSchema {
    state: AppPrimitiveSchemaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
}

impl AppPrimitiveSchema {
    fn bounded_inline(value: Value) -> Self {
        // Do not first allocate an unbounded serialized blob merely to learn
        // that it exceeds the descriptor ceiling. The upstream parser owns
        // the in-memory Value; this counter bounds the projection bytes before
        // canonical hashing or retention.
        let within_bound = serialized_size_within(&value, MAX_PRIMITIVE_SCHEMA_BYTES);
        if !within_bound {
            return Self {
                state: AppPrimitiveSchemaState::RejectedByBound,
                digest: None,
                value: None,
            };
        }
        match AppDigest::blake3_canonical_json(&value) {
            Ok(digest) => Self {
                state: AppPrimitiveSchemaState::Inline,
                digest: Some(digest),
                value: Some(value),
            },
            Err(_) => Self {
                state: AppPrimitiveSchemaState::RejectedByBound,
                digest: None,
                value: None,
            },
        }
    }

    fn undeclared() -> Self {
        Self {
            state: AppPrimitiveSchemaState::Undeclared,
            digest: None,
            value: None,
        }
    }

    pub fn state(&self) -> AppPrimitiveSchemaState {
        self.state
    }

    pub fn digest(&self) -> Option<&AppDigest> {
        self.digest.as_ref()
    }

    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveExposure {
    apps: bool,
    agents: bool,
    authoring: bool,
    deferred_search: bool,
}

impl AppPrimitiveExposure {
    pub fn apps(&self) -> bool {
        self.apps
    }

    pub fn agents(&self) -> bool {
        self.agents
    }

    pub fn authoring(&self) -> bool {
        self.authoring
    }

    pub fn deferred_search(&self) -> bool {
        self.deferred_search
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveResourceContract {
    timeout_ceiling_seconds: Option<u64>,
    required_authorities: BTreeSet<String>,
    resource_scopes: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ceiling_digest: Option<AppDigest>,
}

impl AppPrimitiveResourceContract {
    pub fn timeout_ceiling_seconds(&self) -> Option<u64> {
        self.timeout_ceiling_seconds
    }

    pub fn required_authorities(&self) -> &BTreeSet<String> {
        &self.required_authorities
    }

    pub fn resource_scopes(&self) -> &BTreeSet<String> {
        &self.resource_scopes
    }

    pub fn ceiling_digest(&self) -> Option<&AppDigest> {
        self.ceiling_digest.as_ref()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveDispatch {
    status: AppPrimitiveDispatchStatus,
    reason: String,
}

impl AppPrimitiveDispatch {
    pub fn status(&self) -> AppPrimitiveDispatchStatus {
        self.status
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveEligibility {
    status: AppPrimitiveEligibilityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl AppPrimitiveEligibility {
    pub fn status(&self) -> AppPrimitiveEligibilityStatus {
        self.status
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSourceRef {
    kind: AppPrimitiveSourceKind,
    reference: AppReference,
    content_digest: AppDigest,
}

impl AppPrimitiveSourceRef {
    pub fn kind(&self) -> AppPrimitiveSourceKind {
        self.kind
    }

    pub fn reference(&self) -> &AppReference {
        &self.reference
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveActionDescriptor {
    identity: AppReference,
    name: String,
    description: String,
    input_schema: AppPrimitiveSchema,
    result_schema: AppPrimitiveSchema,
    effects: BTreeSet<AppPrimitiveEffect>,
    dispatch: AppPrimitiveDispatch,
    resources: AppPrimitiveResourceContract,
    /// Digest of the reviewed physical lowering/profile when the primitive
    /// has one. Dispatchable compiled, OS-jail, and governed-MCP actions
    /// require it; blocked instruction/task primitives intentionally leave it
    /// absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    implementation_plan_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    physical_artifact_revision_ref: Option<AppReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    physical_artifact_digest: Option<AppDigest>,
    /// Exact reviewed provider-response transport ceiling. This does not
    /// consume or enlarge persistent app-store payload authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_result_byte_ceiling: Option<u64>,
    action_digest: AppDigest,
}

impl AppPrimitiveActionDescriptor {
    pub fn identity(&self) -> &AppReference {
        &self.identity
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &AppPrimitiveSchema {
        &self.input_schema
    }

    pub fn result_schema(&self) -> &AppPrimitiveSchema {
        &self.result_schema
    }

    pub fn effects(&self) -> &BTreeSet<AppPrimitiveEffect> {
        &self.effects
    }

    pub fn dispatch(&self) -> &AppPrimitiveDispatch {
        &self.dispatch
    }

    pub fn resources(&self) -> &AppPrimitiveResourceContract {
        &self.resources
    }

    pub fn implementation_plan_digest(&self) -> Option<&AppDigest> {
        self.implementation_plan_digest.as_ref()
    }

    pub fn physical_artifact_revision_ref(&self) -> Option<&AppReference> {
        self.physical_artifact_revision_ref.as_ref()
    }

    pub fn physical_artifact_digest(&self) -> Option<&AppDigest> {
        self.physical_artifact_digest.as_ref()
    }

    pub fn transport_result_byte_ceiling(&self) -> Option<u64> {
        self.transport_result_byte_ceiling
    }

    pub fn action_digest(&self) -> &AppDigest {
        &self.action_digest
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveDescriptor {
    schema_version: &'static str,
    identity: AppReference,
    descriptor_digest: AppDigest,
    name: String,
    semantic_version: Option<String>,
    description: String,
    display_name: Option<String>,
    kind: AppPrimitiveKind,
    source: AppPrimitiveSourceRef,
    execution_class: AppPrimitiveExecutionClass,
    containment: AppPrimitiveContainment,
    exposure: AppPrimitiveExposure,
    eligibility: AppPrimitiveEligibility,
    dispatch: AppPrimitiveDispatch,
    default_io_kind: AppToolIoKind,
    allowed_modes: BTreeSet<AppPrimitiveInvocationMode>,
    declared_tool_selectors: BTreeSet<String>,
    actions: Vec<AppPrimitiveActionDescriptor>,
}

impl AppPrimitiveDescriptor {
    pub fn schema_version(&self) -> &'static str {
        self.schema_version
    }

    pub fn identity(&self) -> &AppReference {
        &self.identity
    }

    pub fn descriptor_digest(&self) -> &AppDigest {
        &self.descriptor_digest
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn semantic_version(&self) -> Option<&str> {
        self.semantic_version.as_deref()
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    pub fn kind(&self) -> AppPrimitiveKind {
        self.kind
    }

    pub fn source(&self) -> &AppPrimitiveSourceRef {
        &self.source
    }

    pub fn execution_class(&self) -> AppPrimitiveExecutionClass {
        self.execution_class
    }

    pub fn containment(&self) -> AppPrimitiveContainment {
        self.containment
    }

    pub fn exposure(&self) -> &AppPrimitiveExposure {
        &self.exposure
    }

    pub fn eligibility(&self) -> &AppPrimitiveEligibility {
        &self.eligibility
    }

    pub fn dispatch(&self) -> &AppPrimitiveDispatch {
        &self.dispatch
    }

    pub fn default_io_kind(&self) -> AppToolIoKind {
        self.default_io_kind
    }

    pub fn allowed_modes(&self) -> &BTreeSet<AppPrimitiveInvocationMode> {
        &self.allowed_modes
    }

    pub fn declared_tool_selectors(&self) -> &BTreeSet<String> {
        &self.declared_tool_selectors
    }

    pub fn actions(&self) -> &[AppPrimitiveActionDescriptor] {
        &self.actions
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveCollision {
    alias: String,
    identities: Vec<AppReference>,
    reserved_platform_name: bool,
}

impl AppPrimitiveCollision {
    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn identities(&self) -> &[AppReference] {
        &self.identities
    }

    pub fn reserved_platform_name(&self) -> bool {
        self.reserved_platform_name
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveCatalogIssue {
    code: &'static str,
    source_digest: AppDigest,
}

impl AppPrimitiveCatalogIssue {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn source_digest(&self) -> &AppDigest {
        &self.source_digest
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppPrimitiveResolveError {
    #[error("primitive catalog is unavailable")]
    CatalogUnavailable,
    #[error("primitive is not in the immutable catalog")]
    NotFound,
    #[error("primitive alias is ambiguous; use an exact primitive identity")]
    AmbiguousAlias,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSearchQuery {
    pub text: String,
    pub kinds: BTreeSet<AppPrimitiveKind>,
    pub apps_only: bool,
    pub dispatchable_only: bool,
    pub limit: usize,
}

impl Default for AppPrimitiveSearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            kinds: BTreeSet::new(),
            apps_only: true,
            dispatchable_only: false,
            limit: 12,
        }
    }
}

/// Schema-free deferred-search hit. Exact schemas are loaded by identity from
/// the same immutable snapshot after selection.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSearchHit {
    identity: AppReference,
    descriptor_digest: AppDigest,
    name: String,
    description: String,
    kind: AppPrimitiveKind,
    dispatch_status: AppPrimitiveDispatchStatus,
    actions: Vec<AppPrimitiveSearchAction>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSearchAction {
    action_identity: AppReference,
    input_schema_digest: Option<AppDigest>,
    result_schema_digest: Option<AppDigest>,
    dispatch_status: AppPrimitiveDispatchStatus,
}

/// Binds an immutable resolver snapshot to its scoped catalog owner. It is
/// identity, not authority: dispatch still re-resolves current lifecycle and
/// grant state. A snapshot without an owner is suitable only for process-wide
/// embedded primitives and provider-free fixtures.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPrimitiveSnapshotBinding {
    owner_ref: Option<AppReference>,
    snapshot_digest: AppDigest,
}

impl AppPrimitiveSnapshotBinding {
    pub fn owner_ref(&self) -> Option<&AppReference> {
        self.owner_ref.as_ref()
    }

    pub fn snapshot_digest(&self) -> &AppDigest {
        &self.snapshot_digest
    }
}

impl AppPrimitiveSearchHit {
    pub fn identity(&self) -> &AppReference {
        &self.identity
    }

    pub fn descriptor_digest(&self) -> &AppDigest {
        &self.descriptor_digest
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn kind(&self) -> AppPrimitiveKind {
        self.kind
    }

    pub fn dispatch_status(&self) -> AppPrimitiveDispatchStatus {
        self.dispatch_status
    }

    pub fn actions(&self) -> &[AppPrimitiveSearchAction] {
        &self.actions
    }
}

impl AppPrimitiveSearchAction {
    pub fn action_identity(&self) -> &AppReference {
        &self.action_identity
    }

    pub fn input_schema_digest(&self) -> Option<&AppDigest> {
        self.input_schema_digest.as_ref()
    }

    pub fn result_schema_digest(&self) -> Option<&AppDigest> {
        self.result_schema_digest.as_ref()
    }

    pub fn dispatch_status(&self) -> AppPrimitiveDispatchStatus {
        self.dispatch_status
    }
}

#[derive(Debug, Clone)]
pub struct AppPrimitiveCatalogSnapshot {
    complete: bool,
    owner_ref: Option<AppReference>,
    snapshot_digest: AppDigest,
    descriptors: Vec<Arc<AppPrimitiveDescriptor>>,
    by_identity: BTreeMap<AppReference, Arc<AppPrimitiveDescriptor>>,
    source_material: BTreeMap<AppReference, Arc<[u8]>>,
    /// Canonical resolver-owned directory for a retained scoped skill source.
    /// This is host-local review context, never descriptor identity or
    /// transport data. It is used only to admit a private `bin/<name>` into the
    /// content-addressed app artifact store.
    source_directories: BTreeMap<AppReference, PathBuf>,
    platform_names: BTreeMap<String, AppReference>,
    aliases: BTreeMap<String, AppReference>,
    collisions: Vec<AppPrimitiveCollision>,
    collision_aliases: BTreeSet<String>,
    issues: Vec<AppPrimitiveCatalogIssue>,
}

impl AppPrimitiveCatalogSnapshot {
    pub fn complete(&self) -> bool {
        self.complete
    }

    pub fn snapshot_digest(&self) -> &AppDigest {
        &self.snapshot_digest
    }

    pub fn binding(&self) -> AppPrimitiveSnapshotBinding {
        AppPrimitiveSnapshotBinding {
            owner_ref: self.owner_ref.clone(),
            snapshot_digest: self.snapshot_digest.clone(),
        }
    }

    pub fn descriptors(&self) -> &[Arc<AppPrimitiveDescriptor>] {
        &self.descriptors
    }

    /// Exact bounded source bytes retained for lock evidence. Only primitive
    /// kinds whose current lock codec consumes source documents are retained;
    /// callers must never infer bytes from a descriptor DTO.
    pub fn source_material(&self, identity: &AppReference) -> Option<&[u8]> {
        self.source_material
            .get(identity)
            .map(|bytes| bytes.as_ref())
    }

    pub fn source_directory(&self, identity: &AppReference) -> Option<&Path> {
        self.source_directories.get(identity).map(PathBuf::as_path)
    }

    pub fn collisions(&self) -> &[AppPrimitiveCollision] {
        &self.collisions
    }

    pub fn issues(&self) -> &[AppPrimitiveCatalogIssue] {
        &self.issues
    }

    pub fn alias_available(&self, descriptor: &AppPrimitiveDescriptor) -> bool {
        let alias = normalized_collision_key(descriptor.name());
        self.platform_names
            .get(&alias)
            .or_else(|| self.aliases.get(&alias))
            == Some(descriptor.identity())
    }

    pub fn collision_for(
        &self,
        descriptor: &AppPrimitiveDescriptor,
    ) -> Option<&AppPrimitiveCollision> {
        self.collisions
            .iter()
            .find(|collision| collision.identities.contains(descriptor.identity()))
    }

    pub fn resolve(
        &self,
        requested: &str,
    ) -> Result<Arc<AppPrimitiveDescriptor>, AppPrimitiveResolveError> {
        if !self.complete {
            return Err(AppPrimitiveResolveError::CatalogUnavailable);
        }
        if let Ok(identity) = AppReference::parse(requested.trim().to_owned()) {
            if let Some(descriptor) = self.by_identity.get(&identity) {
                return Ok(Arc::clone(descriptor));
            }
        }
        let alias = normalized_collision_key(requested);
        if let Some(identity) = self.platform_names.get(&alias) {
            return self
                .by_identity
                .get(identity)
                .cloned()
                .ok_or(AppPrimitiveResolveError::CatalogUnavailable);
        }
        if self.collision_aliases.contains(&alias) {
            return Err(AppPrimitiveResolveError::AmbiguousAlias);
        }
        let identity = self
            .aliases
            .get(&alias)
            .ok_or(AppPrimitiveResolveError::NotFound)?;
        self.by_identity
            .get(identity)
            .cloned()
            .ok_or(AppPrimitiveResolveError::CatalogUnavailable)
    }

    pub fn load_action(
        &self,
        primitive_identity: &AppReference,
        action_identity: &AppReference,
    ) -> Result<&AppPrimitiveActionDescriptor, AppPrimitiveResolveError> {
        if !self.complete {
            return Err(AppPrimitiveResolveError::CatalogUnavailable);
        }
        self.by_identity
            .get(primitive_identity)
            .and_then(|descriptor| {
                descriptor
                    .actions()
                    .iter()
                    .find(|action| action.identity() == action_identity)
            })
            .ok_or(AppPrimitiveResolveError::NotFound)
    }

    pub fn search(
        &self,
        query: &AppPrimitiveSearchQuery,
    ) -> Result<Vec<AppPrimitiveSearchHit>, AppPrimitiveResolveError> {
        if !self.complete {
            return Err(AppPrimitiveResolveError::CatalogUnavailable);
        }
        let limit = query.limit.clamp(1, MAX_PRIMITIVE_SEARCH_RESULTS);
        let needle = normalized_search_text(&query.text);
        let tokens = needle.split_whitespace().collect::<Vec<_>>();
        let mut matches = self
            .descriptors
            .iter()
            .filter(|descriptor| {
                (query.kinds.is_empty() || query.kinds.contains(&descriptor.kind()))
                    && (!query.apps_only || descriptor.exposure().apps())
                    && (!query.dispatchable_only
                        || descriptor.dispatch().status() == AppPrimitiveDispatchStatus::Ready)
            })
            .filter_map(|descriptor| {
                let haystack = normalized_search_text(&format!(
                    "{} {} {}",
                    descriptor.name(),
                    descriptor.description(),
                    descriptor
                        .actions()
                        .iter()
                        .map(AppPrimitiveActionDescriptor::name)
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                let score = search_score(descriptor.name(), &haystack, &needle, &tokens)?;
                Some((score, Arc::clone(descriptor)))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name().cmp(right.name()))
                .then_with(|| left.identity().cmp(right.identity()))
        });
        Ok(matches
            .into_iter()
            .take(limit)
            .map(|(_, descriptor)| search_hit(&descriptor))
            .collect())
    }
}

#[derive(Default)]
pub struct AppPrimitiveCatalogBuilder {
    owner_ref: Option<AppReference>,
    descriptors: Vec<AppPrimitiveDescriptor>,
    source_material: BTreeMap<AppReference, Arc<[u8]>>,
    source_directories: BTreeMap<AppReference, PathBuf>,
    source_material_bytes: usize,
    issues: Vec<AppPrimitiveCatalogIssue>,
    overflowed: bool,
}

impl AppPrimitiveCatalogBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn for_owner(owner_ref: AppReference) -> Self {
        Self {
            owner_ref: Some(owner_ref),
            ..Self::default()
        }
    }

    pub fn add_compiled_pack(&mut self, pack: &CapabilityPackDefinition, source_bytes: &[u8]) {
        match compiled_descriptor(pack, source_bytes) {
            Some(descriptor) => self.push(descriptor),
            None => self.reject("invalid_compiled_pack", source_bytes),
        }
    }

    pub fn add_tool_skill(&mut self, source_bytes: &[u8]) {
        self.add_tool_skill_from_directory(source_bytes, None);
    }

    pub(crate) fn add_tool_skill_from_directory(
        &mut self,
        source_bytes: &[u8],
        source_directory: Option<PathBuf>,
    ) {
        match tool_skill_descriptor(source_bytes) {
            Some(descriptor)
                if descriptor.eligibility().status() == AppPrimitiveEligibilityStatus::Lockable =>
            {
                let identity = descriptor.identity().clone();
                self.push_with_source(descriptor, source_bytes);
                if let Some(source_directory) = source_directory {
                    self.source_directories
                        .entry(identity)
                        .or_insert(source_directory);
                }
            },
            Some(descriptor) => self.push(descriptor),
            // A malformed private installation is quarantined. It must not
            // erase the independently valid embedded platform catalog.
            None => self.skip("invalid_tool_skill", source_bytes),
        }
    }

    pub fn add_procedure_skill(&mut self, source_bytes: &[u8]) {
        match procedure_descriptor(source_bytes) {
            // Procedure instructions must be reconstructable from the exact
            // content-addressed source selected during review. Retaining only
            // the descriptor would force a mutable name lookup on resume.
            Some(descriptor) => self.push_with_source(descriptor, source_bytes),
            None => self.skip("invalid_procedure_skill", source_bytes),
        }
    }

    pub fn add_agent_definition(&mut self, source_bytes: &[u8], default_runner: bool) -> bool {
        match agent_descriptor(source_bytes, default_runner) {
            Some(descriptor) => {
                // The definition digest is useful only when the exact bounded
                // definition bytes remain available to the lock/run owner.
                self.push_with_source(descriptor, source_bytes);
                true
            },
            None if default_runner => {
                // A private source claiming the reserved default identity is
                // platform-significant corruption, not an optional bad agent.
                self.reject("invalid_default_agent_definition", source_bytes);
                false
            },
            None => {
                self.skip("invalid_agent_definition", source_bytes);
                false
            },
        }
    }

    pub fn add_default_agent(&mut self, name: &str, display_name: &str, description: &str) {
        let source = format!(
            "agent_id: {name}\nversion: 1\nname: {display_name}\ndescription: {description}\n"
        );
        if let Some(descriptor) = agent_descriptor(source.as_bytes(), true) {
            self.push_with_source(descriptor, source.as_bytes());
        } else {
            self.reject("invalid_default_agent", source.as_bytes());
        }
    }

    /// Embed the plan-1.3 experience admission primitives (`overlay-draw` and
    /// `narration`). Like the compiled packs these are EmbeddedPlatform
    /// descriptors, but their single actions are deliberately **not**
    /// dispatch-Ready: the admission surface (bounded schemas, ceilings,
    /// caps and implementation-plan digests in `experience_capability.rs`)
    /// is the deliverable, and no dispatch consumer is wired. Installation
    /// review fails closed on both classes until a reviewed physical-owner
    /// adapter lands. A malformed static projection is platform-significant
    /// corruption and rejects like an invalid default agent.
    pub fn add_experience_primitives(&mut self) {
        for (spec, source) in [
            (
                &OVERLAY_DRAW_EXPERIENCE_SPEC,
                OVERLAY_DRAW_PRIMITIVE_SOURCE.as_bytes(),
            ),
            (
                &NARRATION_EXPERIENCE_SPEC,
                NARRATION_PRIMITIVE_SOURCE.as_bytes(),
            ),
        ] {
            match experience_primitive_descriptor(spec, source) {
                Some(descriptor) => self.push_with_source(descriptor, source),
                None => self.reject("invalid_experience_primitive", spec.name.as_bytes()),
            }
        }
    }

    pub(crate) fn reject(&mut self, code: &'static str, evidence: &[u8]) {
        self.overflowed = true;
        self.issue(code, evidence);
    }

    /// Record and quarantine one invalid non-platform source without making
    /// unrelated embedded primitives unavailable. Aggregate bounds, embedded
    /// platform corruption and identity conflicts continue to use `reject` or
    /// return an unavailable snapshot directly.
    pub(crate) fn skip(&mut self, code: &'static str, evidence: &[u8]) {
        self.issue(code, evidence);
    }

    pub fn finish(mut self) -> Arc<AppPrimitiveCatalogSnapshot> {
        let owner_ref = self.owner_ref.clone();
        if self.overflowed {
            return Arc::new(unavailable_snapshot(owner_ref, self.issues));
        }
        self.issues.sort_by(|left, right| {
            left.code
                .cmp(right.code)
                .then_with(|| left.source_digest.cmp(&right.source_digest))
        });
        self.descriptors
            .sort_by(|left, right| left.identity().cmp(right.identity()));
        let mut by_identity: BTreeMap<AppReference, Arc<AppPrimitiveDescriptor>> = BTreeMap::new();
        for descriptor in self.descriptors {
            let identity = descriptor.identity().clone();
            let descriptor = Arc::new(descriptor);
            if let Some(existing) = by_identity.get(&identity) {
                if existing.descriptor_digest() != descriptor.descriptor_digest() {
                    self.issues.push(AppPrimitiveCatalogIssue {
                        code: "identity_projection_conflict",
                        source_digest: descriptor.source().content_digest().clone(),
                    });
                    return Arc::new(unavailable_snapshot(owner_ref, self.issues));
                }
                continue;
            }
            by_identity.insert(identity, descriptor);
        }
        self.source_material
            .retain(|identity, _| by_identity.contains_key(identity));
        self.source_directories
            .retain(|identity, _| by_identity.contains_key(identity));
        let descriptors = by_identity.values().cloned().collect::<Vec<_>>();
        let mut groups: BTreeMap<String, Vec<AppReference>> = BTreeMap::new();
        for descriptor in &descriptors {
            groups
                .entry(normalized_collision_key(descriptor.name()))
                .or_default()
                .push(descriptor.identity().clone());
        }
        let mut platform_names = BTreeMap::new();
        let mut aliases = BTreeMap::new();
        let mut collisions = Vec::new();
        let mut collision_aliases = BTreeSet::new();
        for (alias, mut identities) in groups {
            identities.sort();
            let platforms = identities
                .iter()
                .filter(|identity| {
                    by_identity.get(*identity).is_some_and(|descriptor| {
                        descriptor.source().kind() == AppPrimitiveSourceKind::EmbeddedPlatform
                    })
                })
                .collect::<Vec<_>>();
            if platforms.len() > 1 {
                self.issues.push(AppPrimitiveCatalogIssue {
                    code: "platform_alias_conflict",
                    source_digest: AppDigest::blake3(alias.as_bytes()),
                });
                return Arc::new(unavailable_snapshot(owner_ref, self.issues));
            }
            if let Some(platform) = platforms.first() {
                platform_names.insert(alias.clone(), (**platform).clone());
                if identities.len() > 1 {
                    collision_aliases.insert(alias.clone());
                    collisions.push(AppPrimitiveCollision {
                        alias,
                        identities,
                        reserved_platform_name: true,
                    });
                }
            } else if identities.len() == 1 {
                aliases.insert(alias, identities[0].clone());
            } else {
                collision_aliases.insert(alias.clone());
                collisions.push(AppPrimitiveCollision {
                    alias,
                    identities,
                    reserved_platform_name: false,
                });
            }
        }
        let snapshot_digest =
            snapshot_digest(owner_ref.as_ref(), &descriptors, &collisions, &self.issues);
        Arc::new(AppPrimitiveCatalogSnapshot {
            complete: true,
            owner_ref,
            snapshot_digest,
            descriptors,
            by_identity,
            source_material: self.source_material,
            source_directories: self.source_directories,
            platform_names,
            aliases,
            collisions,
            collision_aliases,
            issues: self.issues,
        })
    }

    fn push(&mut self, descriptor: AppPrimitiveDescriptor) {
        if self.descriptors.len() >= MAX_APP_PRIMITIVES {
            self.overflowed = true;
            self.issues.push(AppPrimitiveCatalogIssue {
                code: "primitive_bound_exceeded",
                source_digest: descriptor.source().content_digest().clone(),
            });
            return;
        }
        self.descriptors.push(descriptor);
    }

    fn push_with_source(&mut self, descriptor: AppPrimitiveDescriptor, source_bytes: &[u8]) {
        if self.source_material.contains_key(descriptor.identity()) {
            self.push(descriptor);
            return;
        }
        let Some(next_bytes) = self.source_material_bytes.checked_add(source_bytes.len()) else {
            self.overflowed = true;
            self.issue("source_material_bound_exceeded", source_bytes);
            return;
        };
        if next_bytes > MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES {
            self.overflowed = true;
            self.issue("source_material_bound_exceeded", source_bytes);
            return;
        }
        if self.descriptors.len() >= MAX_APP_PRIMITIVES {
            self.overflowed = true;
            self.issue("primitive_bound_exceeded", source_bytes);
            return;
        }
        self.source_material_bytes = next_bytes;
        self.source_material.insert(
            descriptor.identity().clone(),
            Arc::<[u8]>::from(source_bytes),
        );
        self.descriptors.push(descriptor);
    }

    fn issue(&mut self, code: &'static str, source_bytes: &[u8]) {
        self.issues.push(AppPrimitiveCatalogIssue {
            code,
            source_digest: AppDigest::blake3(source_bytes),
        });
    }
}

#[derive(serde::Deserialize)]
struct PrimitiveSkillFrontmatter {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: Option<Value>,
}

/// Skill-publication metadata (`metadata.magician.app_publication`, plan 2.4).
///
/// Presence marks a skill pack as platform-publishable content. The block owns
/// only the presentation field review cannot reuse from the standard
/// frontmatter: name, description and version stay where every pack already
/// declares them. An absent block is exactly the pre-publication behavior; a
/// present-but-invalid block fails closed — the tool skill is quarantined from
/// this catalog instead of being published with unvalidated review metadata.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillAppPublicationMetadata {
    display_name: String,
}

#[derive(serde::Deserialize)]
struct PrimitiveAgentDefinition {
    agent_id: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    constraints: Value,
    #[serde(default)]
    app_tool: Option<AgentAppToolContract>,
}

fn compiled_descriptor(
    pack: &CapabilityPackDefinition,
    source_bytes: &[u8],
) -> Option<AppPrimitiveDescriptor> {
    if source_bytes.is_empty()
        || source_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
    {
        return None;
    }
    bounded_source_name(&pack.name)?;
    let source = source_ref(AppPrimitiveSourceKind::EmbeddedPlatform, source_bytes)?;
    let eligible = assess_compiled_pack_eligibility(source_bytes);
    let eligibility = match &eligible {
        Ok(_) => AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::Lockable,
            reason: None,
        },
        Err(error) => AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::Blocked,
            reason: Some(bounded_search_text(&error.to_string())),
        },
    };
    let shape = AppToolDeclaredShape::from_compiled_pack(pack);
    let default_plan = plan_app_tool_call_with_shape(
        &pack.name,
        None,
        AppToolContainProfile::InProcessCompiled,
        Some(shape.clone()),
    );
    let (execution_class, containment) = compiled_execution_class(pack);
    // Android's legacy compiled handlers accept raw device IDs, coordinates,
    // package names and MCP action vocabulary. Classify every Android pack as
    // Interactive so generic compiled dispatch can never make that vocabulary
    // app authority. Only the exact finite sealed pack projection below can
    // become Ready; runtime availability still depends on owner review.
    let kind = if execution_class == AppPrimitiveExecutionClass::AndroidDeviceOwner {
        AppPrimitiveKind::Interactive
    } else {
        AppPrimitiveKind::CompiledTool
    };
    let identity = primitive_identity(kind, source.content_digest())?;
    let mut allowed_modes = BTreeSet::from([AppPrimitiveInvocationMode::CallTool]);
    if matches!(
        execution_class,
        AppPrimitiveExecutionClass::BrowserOwner
            | AppPrimitiveExecutionClass::MacosHostOwner
            | AppPrimitiveExecutionClass::AndroidDeviceOwner
    ) {
        allowed_modes.insert(AppPrimitiveInvocationMode::InteractiveAction);
    }
    let actions = if execution_class == AppPrimitiveExecutionClass::AndroidDeviceOwner {
        android_compiled_actions(pack, &identity, source.content_digest())?
    } else {
        compiled_actions(pack, &identity, source.content_digest(), &shape)?
    };
    let exposure = AppPrimitiveExposure {
        apps: eligible.is_ok(),
        agents: true,
        authoring: true,
        deferred_search: true,
    };
    let semantic_version = bounded_optional_text(
        eligible
            .ok()
            .map(|value| value.semantic_version)
            .or_else(|| pack.version.clone()),
        MAX_SEMANTIC_VERSION_BYTES,
    )?;
    let dispatch = sealed_interactive_snapshot_dispatch(execution_class, &actions)
        .unwrap_or_else(|| aggregate_action_dispatch(&actions, dispatch_from_plan(&default_plan)));
    finalize_descriptor(DescriptorDraft {
        identity,
        name: pack.name.clone(),
        semantic_version,
        description: bounded_search_text(pack.description.as_deref().unwrap_or("")),
        display_name: None,
        kind,
        source,
        execution_class,
        containment,
        exposure,
        eligibility,
        dispatch,
        default_io_kind: default_plan.io_kind,
        allowed_modes,
        declared_tool_selectors: BTreeSet::new(),
        actions,
    })
}

pub(crate) fn tool_skill_descriptor(source_bytes: &[u8]) -> Option<AppPrimitiveDescriptor> {
    if source_bytes.is_empty()
        || source_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
    {
        return None;
    }
    let source_text = std::str::from_utf8(source_bytes).ok()?;
    let frontmatter: PrimitiveSkillFrontmatter = parse_skill_frontmatter(source_text).ok()?;
    bounded_source_name(&frontmatter.name)?;
    let app_publication = match parse_skill_magician_extension::<SkillAppPublicationMetadata>(
        source_text,
        "app_publication",
    ) {
        Ok(publication) => publication,
        // A present-but-malformed publication claim must not degrade into an
        // unmarked, privately reviewed tool skill.
        Err(_) => return None,
    };
    let display_name = match app_publication {
        Some(publication) => {
            let display_name = publication.display_name.trim();
            if display_name.is_empty() {
                return None;
            }
            Some(display_name.to_owned())
        },
        None => None,
    };
    let source = source_ref(AppPrimitiveSourceKind::ScopedSkill, source_bytes)?;
    let eligible =
        assess_app_tool_eligibility(source_bytes, AppToolAdmissionSource::ReviewedCatalog);
    let expose = parse_skill_expose(source_text).ok();
    let exposure = match &eligible {
        Ok(value) => AppPrimitiveExposure {
            apps: value.expose.apps,
            agents: value.expose.agents,
            authoring: true,
            deferred_search: true,
        },
        Err(_) => AppPrimitiveExposure {
            apps: false,
            agents: expose.as_ref().is_some_and(|value| value.agents),
            authoring: true,
            deferred_search: false,
        },
    };
    let eligibility = match &eligible {
        Ok(_) => AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::Lockable,
            reason: None,
        },
        Err(error) => AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::Blocked,
            reason: Some(bounded_search_text(&error.to_string())),
        },
    };
    let package = parse_skill_runtime_package(source_text).ok().flatten();
    let shape = AppToolDeclaredShape::from_skill_source(source_text);
    let contain = shape
        .as_ref()
        .map(tool_skill_contain_profile)
        .unwrap_or(AppToolContainProfile::OsJail);
    let default_plan =
        plan_app_tool_call_with_shape(&frontmatter.name, None, contain, shape.clone());
    let (execution_class, containment) = skill_execution_class(shape.as_ref());
    let kind = if matches!(
        execution_class,
        AppPrimitiveExecutionClass::BrowserOwner | AppPrimitiveExecutionClass::MacosHostOwner
    ) {
        AppPrimitiveKind::Interactive
    } else {
        AppPrimitiveKind::ToolSkill
    };
    let identity = primitive_identity(kind, source.content_digest())?;
    let mut allowed_modes = BTreeSet::from([AppPrimitiveInvocationMode::CallTool]);
    if execution_class != AppPrimitiveExecutionClass::UniversalSkillRuntime {
        allowed_modes.insert(AppPrimitiveInvocationMode::InteractiveAction);
    }
    let mut actions = match package.as_ref() {
        Some(_) if execution_class == AppPrimitiveExecutionClass::BrowserOwner => {
            browser_skill_actions(&identity, source.content_digest())?
        },
        Some(_) if execution_class == AppPrimitiveExecutionClass::MacosHostOwner => {
            macos_skill_actions(&identity, source.content_digest())?
        },
        Some(package) => skill_actions(
            &frontmatter.name,
            package,
            &identity,
            source.content_digest(),
            shape.as_ref(),
        )?,
        None => Vec::new(),
    };
    if actions.is_empty() {
        actions = {
            non_tool_action(
                kind,
                &identity,
                &frontmatter.name,
                "Invoke this tool after its typed runtime action contract is admitted.",
                AppPrimitiveInvocationMode::CallTool,
            )
            .into_iter()
            .collect()
        };
    }
    if actions.len() > MAX_ACTIONS_PER_PRIMITIVE {
        return None;
    }
    let semantic_version = bounded_optional_text(
        eligible
            .ok()
            .map(|value| value.semantic_version)
            .or(frontmatter.version),
        MAX_SEMANTIC_VERSION_BYTES,
    )?;
    let dispatch = sealed_interactive_snapshot_dispatch(execution_class, &actions)
        .unwrap_or_else(|| aggregate_action_dispatch(&actions, dispatch_from_plan(&default_plan)));
    finalize_descriptor(DescriptorDraft {
        identity,
        name: frontmatter.name,
        semantic_version,
        description: bounded_search_text(&frontmatter.description),
        display_name: bounded_optional_text(display_name, MAX_SEARCH_TEXT_BYTES)?,
        kind,
        source,
        execution_class,
        containment,
        exposure,
        eligibility,
        dispatch,
        default_io_kind: default_plan.io_kind,
        allowed_modes,
        declared_tool_selectors: BTreeSet::new(),
        actions,
    })
}

fn procedure_descriptor(source_bytes: &[u8]) -> Option<AppPrimitiveDescriptor> {
    if source_bytes.is_empty()
        || source_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
    {
        return None;
    }
    let source_text = std::str::from_utf8(source_bytes).ok()?;
    let frontmatter: PrimitiveSkillFrontmatter = parse_skill_frontmatter(source_text).ok()?;
    bounded_source_name(&frontmatter.name)?;
    let source = source_ref(AppPrimitiveSourceKind::ScopedSkill, source_bytes)?;
    let identity = primitive_identity(AppPrimitiveKind::ProcedureSkill, source.content_digest())?;
    let declared_tool_selectors = bounded_tool_selectors(frontmatter.allowed_tools.as_ref())?;
    let action = non_tool_action(
        AppPrimitiveKind::ProcedureSkill,
        &identity,
        "run",
        "Run this immutable procedure inside an admitted parent workflow.",
        AppPrimitiveInvocationMode::RunProcedure,
    )?;
    let semantic_version = bounded_optional_text(frontmatter.version, MAX_SEMANTIC_VERSION_BYTES)?;
    finalize_descriptor(DescriptorDraft {
        identity,
        name: frontmatter.name,
        semantic_version,
        description: bounded_search_text(&frontmatter.description),
        display_name: None,
        kind: AppPrimitiveKind::ProcedureSkill,
        source,
        execution_class: AppPrimitiveExecutionClass::ProcedureInstructions,
        containment: AppPrimitiveContainment::TaskOwner,
        exposure: AppPrimitiveExposure {
            apps: true,
            agents: true,
            authoring: true,
            deferred_search: true,
        },
        eligibility: AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::DiscoverableOnly,
            reason: Some(
                "procedure input/result and transitive tool ceilings bind at package review"
                    .to_owned(),
            ),
        },
        dispatch: AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Conditional,
            reason: "requires an immutable package lock and parent workflow authority".to_owned(),
        },
        default_io_kind: AppToolIoKind::Unbound,
        allowed_modes: BTreeSet::from([AppPrimitiveInvocationMode::RunProcedure]),
        declared_tool_selectors,
        actions: vec![action],
    })
}

fn agent_descriptor(source_bytes: &[u8], default_runner: bool) -> Option<AppPrimitiveDescriptor> {
    if source_bytes.is_empty() || source_bytes.len() > MAX_AGENT_DEFINITION_BYTES {
        return None;
    }
    // Apply the same lexical YAML safety boundary as app manifests before
    // serde_yaml can expand aliases/tags or recurse through hostile input.
    preflight_agent_definition_yaml(
        source_bytes,
        &AppPackageLimits::for_bounded_agent_definition(MAX_AGENT_DEFINITION_BYTES),
    )
    .ok()?;
    let definition: PrimitiveAgentDefinition = serde_yaml::from_slice(source_bytes).ok()?;
    let complete_definition = std::str::from_utf8(source_bytes)
        .ok()
        .and_then(|source| crate::magician_v2::agents::AgentDefinition::from_yaml_str(source).ok());
    crate::magician_v2::agents::storage::validate_agent_identifier(&definition.agent_id).ok()?;
    bounded_source_name(&definition.agent_id)?;
    if definition.tools.len() > 256 {
        return None;
    }
    let source = source_ref(AppPrimitiveSourceKind::ScopedAgent, source_bytes)?;
    let identity = primitive_identity(AppPrimitiveKind::Agent, source.content_digest())?;
    let ceiling_digest = if definition.constraints.is_null() {
        None
    } else {
        if !serialized_size_within(&definition.constraints, MAX_PRIMITIVE_SCHEMA_BYTES) {
            return None;
        }
        Some(AppDigest::blake3_canonical_json(&definition.constraints).ok()?)
    };
    let declared_tool_selectors = bounded_tool_selector_values(definition.tools)?;
    let callable_contract = definition.app_tool.filter(|contract| {
        contract.max_input_bytes > 0
            && contract.max_result_bytes > 0
            && contract.max_input_bytes <= 256 * 1024
            && contract.max_result_bytes <= 256 * 1024
            && contract.input.fields.len() <= 256
            && contract.result.fields.len() <= 256
            && serde_json::to_value(&contract.input)
                .ok()
                .is_some_and(|schema| serialized_size_within(&schema, MAX_PRIMITIVE_SCHEMA_BYTES))
            && serde_json::to_value(&contract.result)
                .ok()
                .is_some_and(|schema| serialized_size_within(&schema, MAX_PRIMITIVE_SCHEMA_BYTES))
    });
    // Discovery may retain a structurally bounded partial projection, but a
    // Ready callable child must be the exact complete definition that the
    // task owner will later seal. This closes disabled/non-Task definitions
    // and definitions that fail the ordinary agent-store validation rather
    // than advertising an action which can never pass the launch fence.
    let sealed_callable_definition = complete_definition.as_ref().is_some_and(|complete| {
        crate::magician_v2::apps::agent_capability::agent_definition_permits_app_task(complete)
            && complete.agent_id == definition.agent_id
            && complete.app_tool.as_ref() == callable_contract.as_ref()
    });
    let implementation_plan_digest = callable_contract.as_ref().and_then(|contract| {
        super::agent_capability::agent_tool_implementation_plan_digest(
            source.content_digest(),
            contract,
        )
        .ok()
    });
    let input_schema = callable_contract
        .as_ref()
        .map(|contract| {
            AppPrimitiveSchema::bounded_inline(contract.input.to_primitive_json_schema())
        })
        .unwrap_or_else(AppPrimitiveSchema::undeclared);
    let result_schema = callable_contract
        .as_ref()
        .map(|contract| {
            AppPrimitiveSchema::bounded_inline(contract.result.to_primitive_json_schema())
        })
        .unwrap_or_else(AppPrimitiveSchema::undeclared);
    let action_dispatch = if !default_runner
        && sealed_callable_definition
        && callable_contract.is_some()
        && implementation_plan_digest.is_some()
        && input_schema.state() == AppPrimitiveSchemaState::Inline
        && result_schema.state() == AppPrimitiveSchemaState::Inline
    {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Ready,
            reason:
                "one exact typed agent action has the reviewed canonical Artifact V3 child owner"
                    .to_owned(),
        }
    } else {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "typed invocation contract is absent or invalid".to_owned(),
        }
    };
    let action = action_descriptor(
        AppPrimitiveKind::Agent,
        &identity,
        "agent_as_tool",
        "Invoke this exact agent definition as a bounded child task.",
        input_schema,
        result_schema,
        BTreeSet::from([AppPrimitiveEffect::Undeclared]),
        action_dispatch,
        implementation_plan_digest.clone(),
        None,
        None,
        callable_contract
            .as_ref()
            .map(|contract| contract.max_result_bytes),
        AppPrimitiveResourceContract {
            timeout_ceiling_seconds: None,
            required_authorities: BTreeSet::new(),
            resource_scopes: BTreeSet::new(),
            ceiling_digest,
        },
    )?;
    let callable_contract_missing = callable_contract.is_none();
    let sealed_action_ready = action.dispatch().status() == AppPrimitiveDispatchStatus::Ready;
    let descriptor_dispatch = if sealed_action_ready
        && action.name() == "agent_as_tool"
        && action.input_schema().state() == AppPrimitiveSchemaState::Inline
        && action.result_schema().state() == AppPrimitiveSchemaState::Inline
        && action.effects() == &BTreeSet::from([AppPrimitiveEffect::Undeclared])
        && action.implementation_plan_digest().is_some()
        && action.physical_artifact_revision_ref().is_none()
        && action.physical_artifact_digest().is_none()
        && action
            .transport_result_byte_ceiling()
            .is_some_and(|ceiling| ceiling > 0 && ceiling <= 256 * 1024)
    {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Ready,
            reason: "one sealed agent_as_tool action has the canonical Artifact V3 leaf owner"
                .to_owned(),
        }
    } else {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: if default_runner {
                "the implicit app workflow runner is not callable as a child tool".to_owned()
            } else if callable_contract_missing {
                "agent_as_tool requires reviewed typed input/result and delegation contracts"
                    .to_owned()
            } else {
                "the agent definition is not an exact reviewed callable Task leaf".to_owned()
            },
        }
    };
    finalize_descriptor(DescriptorDraft {
        identity,
        name: definition.agent_id,
        semantic_version: Some(definition.version.max(1).to_string()),
        description: bounded_search_text(&definition.description),
        display_name: bounded_optional_text(
            Some(definition.name).filter(|value| !value.trim().is_empty()),
            MAX_SEARCH_TEXT_BYTES,
        )?,
        kind: AppPrimitiveKind::Agent,
        source,
        execution_class: AppPrimitiveExecutionClass::AgentTask,
        containment: AppPrimitiveContainment::TaskOwner,
        exposure: AppPrimitiveExposure {
            apps: true,
            agents: true,
            authoring: true,
            deferred_search: true,
        },
        eligibility: AppPrimitiveEligibility {
            status: if default_runner {
                AppPrimitiveEligibilityStatus::DiscoverableOnly
            } else if callable_contract_missing || !sealed_callable_definition {
                AppPrimitiveEligibilityStatus::Blocked
            } else {
                AppPrimitiveEligibilityStatus::Lockable
            },
            reason: (callable_contract_missing || !sealed_callable_definition).then(|| {
                "agent_as_tool requires reviewed typed input/result and delegation contracts"
                    .to_owned()
            }),
        },
        dispatch: descriptor_dispatch,
        default_io_kind: AppToolIoKind::Unbound,
        allowed_modes: BTreeSet::from([AppPrimitiveInvocationMode::AgentAsTool]),
        declared_tool_selectors,
        actions: vec![action],
    })
}

struct DescriptorDraft {
    identity: AppReference,
    name: String,
    semantic_version: Option<String>,
    description: String,
    display_name: Option<String>,
    kind: AppPrimitiveKind,
    source: AppPrimitiveSourceRef,
    execution_class: AppPrimitiveExecutionClass,
    containment: AppPrimitiveContainment,
    exposure: AppPrimitiveExposure,
    eligibility: AppPrimitiveEligibility,
    dispatch: AppPrimitiveDispatch,
    default_io_kind: AppToolIoKind,
    allowed_modes: BTreeSet<AppPrimitiveInvocationMode>,
    declared_tool_selectors: BTreeSet<String>,
    actions: Vec<AppPrimitiveActionDescriptor>,
}

fn finalize_descriptor(draft: DescriptorDraft) -> Option<AppPrimitiveDescriptor> {
    bounded_source_name(&draft.name)?;
    if draft.description.len() > MAX_SEARCH_TEXT_BYTES
        || draft.dispatch.reason.len() > MAX_SEARCH_TEXT_BYTES
        || draft
            .eligibility
            .reason
            .as_ref()
            .is_some_and(|value| value.len() > MAX_SEARCH_TEXT_BYTES)
        || draft
            .display_name
            .as_ref()
            .is_some_and(|value| value.len() > MAX_SEARCH_TEXT_BYTES)
        || draft
            .semantic_version
            .as_ref()
            .is_some_and(|value| value.len() > MAX_SEMANTIC_VERSION_BYTES)
        || draft.actions.len() > MAX_ACTIONS_PER_PRIMITIVE
        || draft.declared_tool_selectors.len()
            > super::manifest::AppPackageLimits::default().max_dependencies()
        || draft
            .declared_tool_selectors
            .iter()
            .any(|selector| selector.len() > MAX_TOOL_SELECTOR_BYTES)
    {
        return None;
    }
    let material = json!({
        "schema_version": APP_PRIMITIVE_DESCRIPTOR_VERSION,
        "identity": &draft.identity,
        "name": &draft.name,
        "semantic_version": &draft.semantic_version,
        "description": &draft.description,
        "display_name": &draft.display_name,
        "kind": draft.kind,
        "source": &draft.source,
        "execution_class": draft.execution_class,
        "containment": draft.containment,
        "exposure": &draft.exposure,
        "eligibility": &draft.eligibility,
        "dispatch": &draft.dispatch,
        "default_io_kind": draft.default_io_kind,
        "allowed_modes": &draft.allowed_modes,
        "declared_tool_selectors": &draft.declared_tool_selectors,
        "actions": &draft.actions,
    });
    let descriptor_digest = AppDigest::blake3_canonical_json(&material).ok()?;
    Some(AppPrimitiveDescriptor {
        schema_version: APP_PRIMITIVE_DESCRIPTOR_VERSION,
        identity: draft.identity,
        descriptor_digest,
        name: draft.name,
        semantic_version: draft.semantic_version,
        description: draft.description,
        display_name: draft.display_name,
        kind: draft.kind,
        source: draft.source,
        execution_class: draft.execution_class,
        containment: draft.containment,
        exposure: draft.exposure,
        eligibility: draft.eligibility,
        dispatch: draft.dispatch,
        default_io_kind: draft.default_io_kind,
        allowed_modes: draft.allowed_modes,
        declared_tool_selectors: draft.declared_tool_selectors,
        actions: draft.actions,
    })
}

fn compiled_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    if pack.name == "http" {
        return app_bound_http_actions(pack, primitive_identity, source_digest, shape);
    }
    if pack.name == "files" {
        return app_bound_file_actions(pack, primitive_identity, source_digest, shape);
    }
    if pack.name == "duckdb" {
        return app_bound_table_actions(pack, primitive_identity, source_digest, shape);
    }
    if pack.name == "internal_data" {
        return app_bound_learning_read_actions(pack, primitive_identity, source_digest, shape);
    }
    if pack.name == "thinking_maps_data" {
        return app_bound_thinking_map_read_actions(pack, primitive_identity, source_digest, shape);
    }
    if pack.native_action_schemas.is_empty() {
        let input = AppPrimitiveSchema::bounded_inline(pack_input_schema(pack));
        let description = pack.description.as_deref().unwrap_or("");
        return build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            &pack.name,
            description,
            input,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            compiled_resources(pack, None),
        )
        .map(|action| vec![action]);
    }
    if pack.native_action_schemas.len() > MAX_ACTIONS_PER_PRIMITIVE {
        return None;
    }
    let mut actions = pack.native_action_schemas.iter().collect::<Vec<_>>();
    actions.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut projected = Vec::with_capacity(actions.len());
    for (name, schema) in actions {
        projected.push(build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            name,
            schema
                .description
                .as_deref()
                .or(pack.description.as_deref())
                .unwrap_or(""),
            AppPrimitiveSchema::bounded_inline(native_action_input_schema(schema)),
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            compiled_resources(pack, schema.timeout_secs),
        )?);
    }
    Some(projected)
}

/// Project only the HTTP operation whose physical owner is complete. The
/// ordinary compiled pack remains broader for agents, but Apps must not lock a
/// generic `http` action whose input schema can select POST/PUT/PATCH/DELETE.
fn app_bound_http_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let input_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["url"],
        "properties": {
            "url": {
                "type": "string",
                "minLength": 1,
                "maxLength": 8192,
                "pattern": "^https?://"
            },
            "headers": {
                "type": "string",
                "maxLength": 32768,
                "description": "JSON object of request headers; request-identity headers are refused"
            },
            "timeout_secs": {
                "type": "integer",
                "minimum": 1,
                "maximum": 120,
                "default": 30
            },
            "follow_redirects": {
                "type": "boolean",
                "default": true,
                "description": "Redirects remain on the admitted origin and are resolved and pinned per hop"
            }
        }
    }));
    let action = build_tool_action(
        AppPrimitiveKind::CompiledTool,
        primitive_identity,
        "get",
        "Perform one bounded public HTTP GET with DNS/connect-IP/Host/SNI held through I/O.",
        input_schema,
        &pack.name,
        AppToolContainProfile::InProcessCompiled,
        shape,
        source_digest,
        None,
        None,
        None,
        Some(super::bound_http::APP_BOUND_HTTP_RESULT_CEILING),
        compiled_resources(pack, None),
    )?;
    Some(vec![action])
}

/// Project two path-exact filesystem actions. The wider built-in `files` pack
/// remains available to ordinary agents, while Apps never lock append/delete/
/// copy/move/list/mkdir behind a generic action selector.
fn app_bound_file_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let read_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path"],
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 1024,
                "pattern": "^[A-Za-z0-9._ /-]+$",
                "description": "Exact normalized path relative to the reviewed capability directory"
            },
            "encoding": {
                "type": "string",
                "enum": ["utf-8"],
                "default": "utf-8"
            }
        }
    }));
    let write_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "content"],
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 1024,
                "pattern": "^[A-Za-z0-9._ /-]+$",
                "description": "Exact normalized path relative to the reviewed capability directory"
            },
            "content": {
                "type": "string",
                "maxLength": super::bound_path::APP_BOUND_FILE_CONTENT_CEILING
            },
            "create_dirs": {
                "type": "boolean",
                "default": false,
                "description": "Parents must already exist inside the reviewed capability directory"
            }
        }
    }));
    let resources = compiled_resources(pack, None);
    Some(vec![
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "read",
            "Read one bounded regular UTF-8 file through a retained no-follow descriptor.",
            read_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources.clone(),
        )?,
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "write",
            "Atomically create or compare-and-replace one bounded regular file through a retained capability directory.",
            write_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources,
        )?,
    ])
}

/// Project only exact-file preview/describe. Raw SQL, persistent databases,
/// session tables, globs and caller-provided SELECT/WHERE fragments remain
/// absent from the Apps descriptor even though the ordinary agent pack keeps
/// those operations.
fn app_bound_table_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let source_property = json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 1024,
        "pattern": "^[A-Za-z0-9._ /-]+\\.(csv|tsv|json|jsonl|ndjson|parquet)$",
        "description": "One exact reviewed table file relative to the capability directory"
    });
    let output_property = json!({
        "type": "string",
        "enum": ["json"],
        "default": "json"
    });
    let preview_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["source"],
        "properties": {
            "source": source_property.clone(),
            "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 10},
            "output_format": output_property.clone(),
            "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 120, "default": 120}
        }
    }));
    let describe_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["source"],
        "properties": {
            "source": source_property,
            "output_format": output_property,
            "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 120, "default": 120}
        }
    }));
    let resources = compiled_resources(pack, None);
    Some(vec![
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "preview",
            "Preview one bounded CSV/JSON/Parquet descriptor without ambient SQL or paths.",
            preview_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources.clone(),
        )?,
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "describe",
            "Describe one bounded CSV/JSON/Parquet descriptor without session-table authority.",
            describe_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources,
        )?,
    ])
}

/// The first host-read Apps projection (plan 2.5): `internal_data` keeps its
/// broad diagnostics vocabulary for agents, while Apps see exactly the two
/// scoped learning review reads — the population path of the memory review
/// console package. The runtime scope never enters the schema: the executor
/// owns `__principal`/`__workspace`, and the provider's argument proof closes
/// these exact parameter shapes fail-closed. Everything else the pack exposes
/// (catalog, SQL, telemetry, logs, audio notes, procedures) is absent from
/// the Apps descriptor rather than merely blocked.
fn app_bound_learning_read_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let state_property = json!({
        "type": "string",
        "enum": [
            "observed",
            "proposed",
            "triaged",
            "approved",
            "implemented",
            "evaluated",
            "promoted",
            "rejected",
            "superseded",
            "archived"
        ],
        "description": "Optional state filter from the closed candidate-state vocabulary"
    });
    let list_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "state": state_property,
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 25,
                "default": 10,
                "description": "Bounded page size; oversized pages fail at the result ceiling"
            }
        }
    }));
    let read_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["candidate_id"],
        "properties": {
            "candidate_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9_.:-]+$",
                "description": "Exact candidate id; returns the candidate with its decision log"
            }
        }
    }));
    let resources = compiled_resources(pack, None);
    Some(vec![
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "list_learning_candidates",
            "List one bounded page of scoped learning candidates from the review substrate.",
            list_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources.clone(),
        )?,
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "read_learning_candidate",
            "Read one scoped learning candidate with its decision log.",
            read_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources,
        )?,
    ])
}

/// The second host-read Apps projection (Phase 4 Brainstorm re-open — plan
/// 2.5's learning-read projection generalized): `thinking_maps_data` exists
/// for exactly its two scoped bounded reads of the first-party Live Thinking
/// Map substrate — the population path a custom-surface Brainstorm-class
/// package needs. The runtime scope never enters the schema: the executor
/// owns `__principal`/`__workspace`, and the provider's argument proof
/// closes these exact parameter shapes fail-closed. Map mutation (create,
/// apply, patch, delete) is absent from the Apps descriptor rather than
/// merely blocked, and stays on the first-party API.
fn app_bound_thinking_map_read_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: &AppToolDeclaredShape,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let lifecycle_property = json!({
        "type": "string",
        "enum": ["active", "paused", "archived", "deleted"],
        "description": "Optional exact lifecycle filter; omit for the visible (non-deleted) default"
    });
    let list_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "lifecycle": lifecycle_property,
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 25,
                "default": 10,
                "description": "Bounded page size; oversized pages fail at the result ceiling"
            }
        }
    }));
    let read_schema = AppPrimitiveSchema::bounded_inline(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["map_id"],
        "properties": {
            "map_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9_.:-]+$",
                "description": "Exact map id; returns the full typed map snapshot"
            }
        }
    }));
    let resources = compiled_resources(pack, None);
    Some(vec![
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "list_maps",
            "List one bounded page of scoped thinking-map summaries from the first-party substrate.",
            list_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources.clone(),
        )?,
        build_tool_action(
            AppPrimitiveKind::CompiledTool,
            primitive_identity,
            "read_map",
            "Read one exact scoped thinking-map snapshot at its current revision.",
            read_schema,
            &pack.name,
            AppToolContainProfile::InProcessCompiled,
            shape,
            source_digest,
            None,
            None,
            None,
            None,
            resources,
        )?,
    ])
}

/// Apps never inherit the browser skill's raw argv/profile/CDP vocabulary.
/// Each admitted leaf is a closed owner operation; callers must select one
/// exact action in the manifest and installation review can narrow only that
/// immutable leaf.
fn browser_skill_actions(
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let implementation = super::browser_capability::app_browser_runtime_implementation_digest();
    let mut actions = Vec::new();
    for (name, description, effects, ceiling) in [
        (
            "snapshot",
            "Observe the isolated app-owned browser through bounded structured evidence.",
            BTreeSet::from([AppPrimitiveEffect::HostRead]),
            super::browser_capability::APP_BROWSER_OBSERVE_RESULT_CEILING,
        ),
        (
            "navigate",
            "Navigate the isolated browser to one exact reviewed HTTPS origin.",
            BTreeSet::from([
                AppPrimitiveEffect::NetworkRead,
                AppPrimitiveEffect::ExternalMutation,
            ]),
            super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING,
        ),
        (
            "scroll",
            "Scroll a fresh observed browser surface by one bounded typed amount.",
            BTreeSet::from([AppPrimitiveEffect::DeviceInteraction]),
            super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING,
        ),
        (
            "click",
            "Click one opaque element from the current browser observation.",
            BTreeSet::from([
                AppPrimitiveEffect::DeviceInteraction,
                AppPrimitiveEffect::ExternalMutation,
            ]),
            super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING,
        ),
    ] {
        let input_schema = AppPrimitiveSchema::bounded_inline(
            super::browser_capability::browser_action_input_schema(name)?,
        );
        let result_schema = AppPrimitiveSchema::bounded_inline(if name == "snapshot" {
            super::browser_capability::browser_observe_result_schema()
        } else {
            super::browser_capability::browser_action_result_schema()
        });
        if input_schema.state() != AppPrimitiveSchemaState::Inline
            || result_schema.state() != AppPrimitiveSchemaState::Inline
        {
            return None;
        }
        let ceiling_digest = AppDigest::blake3_canonical_json(&json!({
            "schema": super::browser_capability::APP_BROWSER_PROFILE_V1,
            "action": name,
            "source_digest": source_digest,
            "implementation_digest": &implementation,
            "max_sessions": 1,
            "max_steps": 1,
        }))
        .ok()?;
        actions.push(action_descriptor(
            AppPrimitiveKind::Interactive,
            primitive_identity,
            name,
            description,
            input_schema,
            result_schema,
            effects,
            AppPrimitiveDispatch {
                status: AppPrimitiveDispatchStatus::Ready,
                reason: format!("sealed app-owned browser {name} action"),
            },
            Some(implementation.clone()),
            None,
            None,
            Some(ceiling),
            AppPrimitiveResourceContract {
                timeout_ceiling_seconds: Some(300),
                required_authorities: BTreeSet::new(),
                resource_scopes: BTreeSet::from(["browser".to_owned()]),
                ceiling_digest: Some(ceiling_digest),
            },
        )?);
    }
    Some(actions)
}

/// Project the four existing public Android packs into their finite Apps-safe
/// leaves. Each leaf has an independent action reference and review binding;
/// the ambient agent pack vocabulary (device ids, shell/ADB, arbitrary
/// coordinates and raw selectors) never enters this descriptor.
const ANDROID_SNAPSHOT_LEAVES: [(&str, &str, &[AppPrimitiveEffect]); 1] = [(
    "snapshot",
    "Observe one exact reviewed Android package as a bounded structured tree.",
    &[AppPrimitiveEffect::HostRead],
)];
const ANDROID_SCREENSHOT_LEAVES: [(&str, &str, &[AppPrimitiveEffect]); 1] = [(
    "screenshot",
    "Capture bounded pixels from one exact reviewed foreground Android package.",
    &[AppPrimitiveEffect::HostRead],
)];

fn android_compiled_actions(
    pack: &CapabilityPackDefinition,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let leaves: &[(&str, &str, &[AppPrimitiveEffect])] = match pack.name.as_str() {
        "android_snapshot" => &ANDROID_SNAPSHOT_LEAVES,
        "android_screenshot" => &ANDROID_SCREENSHOT_LEAVES,
        "android_act" => &[
            (
                "tap",
                "Tap one opaque element from the exact fresh Android observation.",
                &[
                    AppPrimitiveEffect::DeviceInteraction,
                    AppPrimitiveEffect::ExternalMutation,
                ],
            ),
            (
                "type",
                "Type bounded text into the field selected by the fresh Android observation.",
                &[
                    AppPrimitiveEffect::DeviceInteraction,
                    AppPrimitiveEffect::ExternalMutation,
                ],
            ),
            (
                "key",
                "Send one finite reviewed key through the paired Android owner.",
                &[
                    AppPrimitiveEffect::DeviceInteraction,
                    AppPrimitiveEffect::ExternalMutation,
                ],
            ),
            (
                "scroll",
                "Scroll the exact fresh Android surface by one bounded gesture.",
                &[AppPrimitiveEffect::DeviceInteraction],
            ),
        ],
        "android_app" => &[
            (
                "launch",
                "Launch one exact owner-reviewed Android package.",
                &[AppPrimitiveEffect::ExternalMutation],
            ),
            (
                "close",
                "Move one exact owner-reviewed Android package to the background.",
                &[AppPrimitiveEffect::ExternalMutation],
            ),
        ],
        _ => return None,
    };
    let mut actions = Vec::with_capacity(leaves.len());
    for (name, description, effects) in leaves {
        let input_schema = AppPrimitiveSchema::bounded_inline(
            super::android_device::android_action_input_schema(name)?,
        );
        let result_schema = AppPrimitiveSchema::bounded_inline(
            super::android_device::android_action_result_schema(name)?,
        );
        if input_schema.state() != AppPrimitiveSchemaState::Inline
            || result_schema.state() != AppPrimitiveSchemaState::Inline
        {
            return None;
        }
        let implementation = super::android_device::app_android_action_implementation_plan_digest(
            source_digest,
            name,
        )
        .ok()?;
        let ceiling = match *name {
            "snapshot" => super::android_device::APP_ANDROID_SNAPSHOT_RESULT_CEILING,
            "screenshot" => super::android_device::APP_ANDROID_SCREENSHOT_RESULT_CEILING,
            _ => super::android_device::APP_ANDROID_ACTION_RESULT_CEILING,
        };
        let ceiling_digest = AppDigest::blake3_canonical_json(&json!({
            "schema": super::android_device::APP_ANDROID_DEVICE_PROFILE_V1,
            "action": name,
            "source_digest": source_digest,
            "implementation_plan_digest": &implementation,
            "max_sessions": 1,
            "max_steps": 1,
            "exact_paired_target": true,
            "raw_device_ids": false,
            "shell": false,
            "adb": false,
        }))
        .ok()?;
        actions.push(action_descriptor(
            AppPrimitiveKind::Interactive,
            primitive_identity,
            name,
            description,
            input_schema,
            result_schema,
            effects.iter().copied().collect(),
            AppPrimitiveDispatch {
                status: AppPrimitiveDispatchStatus::Ready,
                reason: format!("sealed paired Android {name} action"),
            },
            Some(implementation),
            None,
            None,
            Some(ceiling),
            AppPrimitiveResourceContract {
                timeout_ceiling_seconds: Some(300),
                required_authorities: BTreeSet::new(),
                resource_scopes: BTreeSet::from(["android_device".to_owned()]),
                ceiling_digest: Some(ceiling_digest),
            },
        )?);
    }
    Some(actions)
}

/// Apps never inherit the legacy macOS skill's raw action name, JSON argument,
/// executable, AX selector, PID/window, or output-file vocabulary. The
/// reviewed vertical exposes only the finite typed roster.
/// Ready admission still requires the exact owner-code/Keychain identity,
/// one-shot attestation/proposal binding, fixed-loopback transport, finalized
/// pairing, interactive session and common effect lifecycle. A keyed response
/// alone is not a physical desktop-owner proof.
fn macos_skill_actions(
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    let mut actions = Vec::new();
    for (name, effects) in [
        (
            "launch",
            BTreeSet::from([AppPrimitiveEffect::ExternalMutation]),
        ),
        (
            "focus",
            BTreeSet::from([AppPrimitiveEffect::ExternalMutation]),
        ),
        ("snapshot", BTreeSet::from([AppPrimitiveEffect::HostRead])),
        (
            "click",
            BTreeSet::from([
                AppPrimitiveEffect::DeviceInteraction,
                AppPrimitiveEffect::ExternalMutation,
            ]),
        ),
        (
            "type",
            BTreeSet::from([
                AppPrimitiveEffect::DeviceInteraction,
                AppPrimitiveEffect::ExternalMutation,
            ]),
        ),
        (
            "key",
            BTreeSet::from([
                AppPrimitiveEffect::DeviceInteraction,
                AppPrimitiveEffect::ExternalMutation,
            ]),
        ),
        (
            "scroll",
            BTreeSet::from([AppPrimitiveEffect::DeviceInteraction]),
        ),
        (
            "drag",
            BTreeSet::from([
                AppPrimitiveEffect::DeviceInteraction,
                AppPrimitiveEffect::ExternalMutation,
            ]),
        ),
    ] {
        let operation = super::macos_host::macos_operation(name)?;
        let input_schema =
            AppPrimitiveSchema::bounded_inline(super::macos_host::macos_action_input_schema(name)?);
        let result_schema = AppPrimitiveSchema::bounded_inline(if name == "snapshot" {
            super::macos_host::macos_observe_result_schema()
        } else {
            super::macos_host::macos_action_result_schema()
        });
        if input_schema.state() != AppPrimitiveSchemaState::Inline
            || result_schema.state() != AppPrimitiveSchemaState::Inline
        {
            return None;
        }
        let implementation =
            super::macos_host::macos_operation_implementation_plan_digest(source_digest, operation)
                .ok()?;
        let ceiling = if name == "snapshot" {
            super::macos_host::APP_MACOS_OBSERVE_RESULT_CEILING
        } else {
            super::macos_host::APP_MACOS_ACTION_RESULT_CEILING
        };
        let ceiling_digest = AppDigest::blake3_canonical_json(&json!({
            "schema": super::macos_host::APP_MACOS_HOST_PROFILE_V1,
            "action": name,
            "source_digest": source_digest,
            "implementation_plan_digest": &implementation,
            "max_sessions": 1,
            "max_steps": 1,
            "max_evidence_bytes": 16 * 1024 * 1024_u64,
            "max_evidence_nodes": 64 * 1024_u64,
        }))
        .ok()?;
        actions.push(action_descriptor(
            AppPrimitiveKind::Interactive,
            primitive_identity,
            name,
            &format!("Run the sealed paired macOS {name} action."),
            input_schema,
            result_schema,
            effects,
            AppPrimitiveDispatch {
                status: AppPrimitiveDispatchStatus::Ready,
                reason: format!(
                    "sealed paired macOS {name} action with exact desktop-owner identity"
                ),
            },
            Some(implementation),
            None,
            None,
            Some(ceiling),
            AppPrimitiveResourceContract {
                timeout_ceiling_seconds: Some(300),
                required_authorities: BTreeSet::new(),
                resource_scopes: BTreeSet::from(["macos_host".to_owned()]),
                ceiling_digest: Some(ceiling_digest),
            },
        )?);
    }
    Some(actions)
}

/// Static embedded source bytes for the overlay-draw experience primitive.
/// The identity is content-addressed from these bytes exactly like every
/// other EmbeddedPlatform primitive.
const OVERLAY_DRAW_PRIMITIVE_SOURCE: &str =
    "primitive: overlay-draw\nversion: 1\nclass: overlay_draw_owner\ncontainment: \
     overlay_surface\nhost_surface: /host/overlay/draw\naction: draw\npayload: \
     tutor_storyboard_recipe\nreturns: receipt\n";

/// Static embedded source bytes for the narration experience primitive.
const NARRATION_PRIMITIVE_SOURCE: &str =
    "primitive: narration\nversion: 1\nclass: narration_owner\ncontainment: \
     media_rail\nhost_surface: media_rail_tts\naction: speak\npayload: \
     bounded_text_with_delivery_hints\nvoice_selection: host_owned\nreturns: receipt\n";

/// Shared dispatch reason for both experience classes: admitted vocabulary,
/// no dispatch consumer. Flipping this to Ready is a consumer commit that
/// must ship with the reviewed physical-owner adapter and its review arm.
const EXPERIENCE_CONSUMER_PENDING_REASON: &str =
    "admission only: the reviewed physical-owner adapter is not wired (plan 1.3)";

/// One plan-1.3 experience primitive specification. The finite roster is the
/// two entries below; an unlisted name can never construct a descriptor.
struct ExperiencePrimitiveSpec {
    name: &'static str,
    description: &'static str,
    execution_class: AppPrimitiveExecutionClass,
    containment: AppPrimitiveContainment,
    action_name: &'static str,
    action_description: &'static str,
    effects: &'static [AppPrimitiveEffect],
    resource_scope: &'static str,
    timeout_ceiling_seconds: u64,
}

const OVERLAY_DRAW_EXPERIENCE_SPEC: ExperiencePrimitiveSpec = ExperiencePrimitiveSpec {
    name: "overlay-draw",
    description: "Draw one reviewed tutor-recipe storyboard payload on the host overlay; \
                  receipt-only.",
    execution_class: AppPrimitiveExecutionClass::OverlayDrawOwner,
    containment: AppPrimitiveContainment::OverlaySurface,
    action_name: "draw",
    action_description: "Validate and draw one bounded storyboard payload on the host overlay.",
    effects: &[
        AppPrimitiveEffect::DeviceInteraction,
        AppPrimitiveEffect::ExternalMutation,
    ],
    resource_scope: "overlay_draw",
    timeout_ceiling_seconds: 60,
};

const NARRATION_EXPERIENCE_SPEC: ExperiencePrimitiveSpec = ExperiencePrimitiveSpec {
    name: "narration",
    description: "Narrate one bounded text utterance through the reviewed TTS media rail; \
                  host-owned voice selection, receipt-only.",
    execution_class: AppPrimitiveExecutionClass::NarrationOwner,
    containment: AppPrimitiveContainment::MediaRail,
    action_name: "speak",
    action_description: "Validate and speak one bounded utterance through the TTS media rail.",
    effects: &[AppPrimitiveEffect::DeviceInteraction],
    resource_scope: "media_narration",
    timeout_ceiling_seconds: 60,
};

/// Project one experience primitive from its static specification and source
/// bytes. The action is Conditional by construction: the class admission
/// (`experience_capability.rs`) owns the exact schemas, ceilings and
/// implementation-plan digest, and no dispatch consumer exists. Review-time
/// identity is revalidated independently in `installation_review.rs`.
fn experience_primitive_descriptor(
    spec: &ExperiencePrimitiveSpec,
    source_bytes: &[u8],
) -> Option<AppPrimitiveDescriptor> {
    let source = source_ref(AppPrimitiveSourceKind::EmbeddedPlatform, source_bytes)?;
    let identity = primitive_identity(AppPrimitiveKind::Interactive, source.content_digest())?;
    let input_schema = AppPrimitiveSchema::bounded_inline(
        super::experience_capability::experience_action_input_schema(spec.action_name)?,
    );
    let result_schema = AppPrimitiveSchema::bounded_inline(
        super::experience_capability::experience_action_result_schema(spec.action_name)?,
    );
    if input_schema.state() != AppPrimitiveSchemaState::Inline
        || result_schema.state() != AppPrimitiveSchemaState::Inline
    {
        return None;
    }
    let Some(implementation) =
        super::experience_capability::experience_action_implementation_plan_digest(
            spec.action_name,
            source.content_digest(),
        )
        .ok()
        .flatten()
    else {
        // An experience action without a bound implementation-plan digest is
        // not admissible; skip the descriptor rather than admit it unbound.
        return None;
    };
    let ceiling = super::experience_capability::experience_action_result_ceiling(spec.action_name)?;
    let ceiling_digest = AppDigest::blake3_canonical_json(&json!({
        "schema": super::experience_capability::APP_EXPERIENCE_PROFILE_V1,
        "primitive": spec.name,
        "action": spec.action_name,
        "source_digest": source.content_digest(),
        "implementation_plan_digest": &implementation,
        "consumer_wired": false,
        "returns": "receipt",
    }))
    .ok()?;
    let dispatch = AppPrimitiveDispatch {
        status: AppPrimitiveDispatchStatus::Conditional,
        reason: EXPERIENCE_CONSUMER_PENDING_REASON.to_owned(),
    };
    let action = action_descriptor(
        AppPrimitiveKind::Interactive,
        &identity,
        spec.action_name,
        spec.action_description,
        input_schema,
        result_schema,
        spec.effects.iter().copied().collect(),
        AppPrimitiveDispatch {
            status: dispatch.status,
            reason: dispatch.reason.clone(),
        },
        Some(implementation),
        None,
        None,
        Some(ceiling),
        AppPrimitiveResourceContract {
            timeout_ceiling_seconds: Some(spec.timeout_ceiling_seconds),
            required_authorities: BTreeSet::new(),
            resource_scopes: BTreeSet::from([spec.resource_scope.to_owned()]),
            ceiling_digest: Some(ceiling_digest),
        },
    )?;
    finalize_descriptor(DescriptorDraft {
        identity,
        name: spec.name.to_owned(),
        semantic_version: Some("1".to_owned()),
        description: bounded_search_text(spec.description),
        display_name: None,
        kind: AppPrimitiveKind::Interactive,
        source,
        execution_class: spec.execution_class,
        containment: spec.containment,
        exposure: AppPrimitiveExposure {
            apps: true,
            agents: false,
            authoring: true,
            deferred_search: true,
        },
        eligibility: AppPrimitiveEligibility {
            status: AppPrimitiveEligibilityStatus::Lockable,
            reason: None,
        },
        dispatch,
        default_io_kind: AppToolIoKind::Device,
        allowed_modes: BTreeSet::from([
            AppPrimitiveInvocationMode::CallTool,
            AppPrimitiveInvocationMode::InteractiveAction,
        ]),
        declared_tool_selectors: BTreeSet::new(),
        actions: vec![action],
    })
}

fn mcp_skill_actions(
    skill_name: &str,
    package: &tool_runtime_core::manifest_parser::SkillRuntimePackage,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: Option<&AppToolDeclaredShape>,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    validate_skill_runtime_contract(&package.contract).ok()?;
    let projected = project_mcp_catalog(package).ok()?;
    if projected.actions.len() > MAX_ACTIONS_PER_PRIMITIVE {
        return None;
    }
    let default_shape = AppToolDeclaredShape::default();
    let shape = shape.unwrap_or(&default_shape);
    let transport_result_byte_ceiling =
        super::governed_mcp::reviewed_transport_result_byte_ceiling();
    let mut projected_actions = Vec::with_capacity(projected.actions.len());
    for (name, authored) in &projected.actions {
        let input_schema = AppPrimitiveSchema::bounded_inline(
            super::governed_mcp::mcp_action_input_schema(authored),
        );
        if input_schema.state() != AppPrimitiveSchemaState::Inline {
            return None;
        }
        let plan_digest = super::governed_mcp::implementation_plan_digest(
            source_digest,
            package,
            name,
            input_schema.value()?,
        );
        projected_actions.push(build_tool_action(
            AppPrimitiveKind::ToolSkill,
            primitive_identity,
            name,
            &authored.description,
            input_schema,
            skill_name,
            AppToolContainProfile::GovernedMcp,
            shape,
            source_digest,
            plan_digest,
            None,
            None,
            Some(transport_result_byte_ceiling),
            AppPrimitiveResourceContract {
                timeout_ceiling_seconds: Some(projected.timeout_secs),
                required_authorities: BTreeSet::new(),
                resource_scopes: BTreeSet::new(),
                ceiling_digest: None,
            },
        )?);
    }
    Some(projected_actions)
}

fn skill_actions(
    skill_name: &str,
    package: &tool_runtime_core::manifest_parser::SkillRuntimePackage,
    primitive_identity: &AppReference,
    source_digest: &AppDigest,
    shape: Option<&AppToolDeclaredShape>,
) -> Option<Vec<AppPrimitiveActionDescriptor>> {
    if matches!(package.contract.runtime, RuntimeProtocol::Mcp { .. }) {
        return mcp_skill_actions(
            skill_name,
            package,
            primitive_identity,
            source_digest,
            shape,
        );
    }
    let Some(actions) = package.actions.as_ref() else {
        return Some(Vec::new());
    };
    if actions.actions.len() > MAX_ACTIONS_PER_PRIMITIVE {
        return None;
    }
    // The OS-jail owner compiles and locks this same jail contract, so the
    // projected schema and plan digest match what runs.
    let jail_contract = super::os_jail::app_jail_contract(&package.contract);
    let compiled = validate_skill_runtime_contract(&jail_contract)
        .ok()
        .and_then(|validated| compile_typed_action_overrides(skill_name, validated, actions).ok());
    let default_shape = AppToolDeclaredShape::default();
    let shape = shape.unwrap_or(&default_shape);
    let egress = shape.app_egress.as_ref();
    let transport_result_byte_ceiling =
        super::os_jail::reviewed_transport_result_byte_ceiling(&jail_contract, egress);
    let mut projected = Vec::with_capacity(actions.actions.len());
    for (name, authored) in &actions.actions {
        let compiled_action = compiled
            .as_ref()
            .and_then(|catalog| catalog.actions.get(name));
        let input_schema = compiled_action
            .map(|action| {
                // The lock and the jail both use this app-facing form: a
                // read-only file input takes the file's content, not a path.
                AppPrimitiveSchema::bounded_inline(super::os_jail::app_facing_input_schema(action))
            })
            .unwrap_or_else(AppPrimitiveSchema::undeclared);
        let resources = compiled_action
            .map(|action| AppPrimitiveResourceContract {
                timeout_ceiling_seconds: Some(u64::from(action.invocation.timeout_ceiling_secs)),
                required_authorities: action
                    .effective_policy
                    .required_resource_authorities
                    .iter()
                    .cloned()
                    .collect(),
                resource_scopes: action
                    .effective_policy
                    .resource_scopes
                    .iter()
                    .cloned()
                    .collect(),
                ceiling_digest: None,
            })
            .unwrap_or_else(empty_resources);
        projected.push(build_tool_action(
            AppPrimitiveKind::ToolSkill,
            primitive_identity,
            name,
            &authored.description,
            input_schema,
            skill_name,
            AppToolContainProfile::OsJail,
            shape,
            source_digest,
            compiled_action.and_then(|action| {
                super::os_jail::implementation_plan_digest(
                    source_digest,
                    &jail_contract,
                    name,
                    action,
                    egress,
                )
            }),
            None,
            None,
            transport_result_byte_ceiling,
            resources,
        )?);
    }
    Some(projected)
}

#[allow(clippy::too_many_arguments)]
fn build_tool_action(
    kind: AppPrimitiveKind,
    primitive_identity: &AppReference,
    action_name: &str,
    description: &str,
    input_schema: AppPrimitiveSchema,
    tool_name: &str,
    contain: AppToolContainProfile,
    shape: &AppToolDeclaredShape,
    source_digest: &AppDigest,
    implementation_plan_digest: Option<AppDigest>,
    physical_artifact_revision_ref: Option<AppReference>,
    physical_artifact_digest: Option<AppDigest>,
    reviewed_transport_result_byte_ceiling: Option<u64>,
    resources: AppPrimitiveResourceContract,
) -> Option<AppPrimitiveActionDescriptor> {
    let plan =
        plan_app_tool_call_with_shape(tool_name, Some(action_name), contain, Some(shape.clone()));
    let effects = BTreeSet::from([effect_from_io(plan.io_kind)]);
    let implementation_plan_digest = implementation_plan_digest.or_else(|| {
        (contain == AppToolContainProfile::InProcessCompiled)
            .then(|| compiled_implementation_plan_digest(source_digest, &plan))
            .flatten()
    });
    let transport_result_byte_ceiling = reviewed_transport_result_byte_ceiling
        .or_else(|| super::app_tool_bind::reviewed_app_transport_result_ceiling(&plan));
    let dispatch = if input_schema.state() != AppPrimitiveSchemaState::Inline {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "action input schema is unavailable or exceeds the descriptor bound".to_owned(),
        }
    } else if implementation_plan_digest.is_none() {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "the exact physical implementation plan is not reviewable".to_owned(),
        }
    } else if transport_result_byte_ceiling.is_none_or(|ceiling| ceiling == 0) {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "the serialized result has no reviewed durable byte ceiling".to_owned(),
        }
    } else if super::app_tool_bind::app_effect_owner_supported(&plan) {
        dispatch_from_plan(&plan)
    } else {
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "no common app physical-effect owner is wired for this action class".to_owned(),
        }
    };
    action_descriptor(
        kind,
        primitive_identity,
        action_name,
        description,
        input_schema,
        AppPrimitiveSchema::undeclared(),
        effects,
        dispatch,
        implementation_plan_digest,
        physical_artifact_revision_ref,
        physical_artifact_digest,
        transport_result_byte_ceiling,
        resources,
    )
}

fn non_tool_action(
    kind: AppPrimitiveKind,
    primitive_identity: &AppReference,
    action_name: &str,
    description: &str,
    _mode: AppPrimitiveInvocationMode,
) -> Option<AppPrimitiveActionDescriptor> {
    action_descriptor(
        kind,
        primitive_identity,
        action_name,
        description,
        AppPrimitiveSchema::undeclared(),
        AppPrimitiveSchema::undeclared(),
        BTreeSet::from([AppPrimitiveEffect::Undeclared]),
        AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Blocked,
            reason: "typed invocation contract is not yet admitted".to_owned(),
        },
        None,
        None,
        None,
        None,
        empty_resources(),
    )
}

#[allow(clippy::too_many_arguments)]
fn action_descriptor(
    kind: AppPrimitiveKind,
    primitive_identity: &AppReference,
    name: &str,
    description: &str,
    input_schema: AppPrimitiveSchema,
    result_schema: AppPrimitiveSchema,
    effects: BTreeSet<AppPrimitiveEffect>,
    dispatch: AppPrimitiveDispatch,
    implementation_plan_digest: Option<AppDigest>,
    physical_artifact_revision_ref: Option<AppReference>,
    physical_artifact_digest: Option<AppDigest>,
    transport_result_byte_ceiling: Option<u64>,
    resources: AppPrimitiveResourceContract,
) -> Option<AppPrimitiveActionDescriptor> {
    bounded_source_name(name)?;
    let bounded_description = bounded_search_text(description);
    if dispatch.reason.len() > MAX_SEARCH_TEXT_BYTES
        || resources.required_authorities.len()
            > super::manifest::AppPackageLimits::default().max_dependencies()
        || resources.resource_scopes.len()
            > super::manifest::AppPackageLimits::default().max_dependencies()
        || resources
            .required_authorities
            .iter()
            .chain(resources.resource_scopes.iter())
            .any(|value| value.len() > MAX_TOOL_SELECTOR_BYTES)
    {
        return None;
    }
    let material = json!({
        "schema_version": APP_PRIMITIVE_DESCRIPTOR_VERSION,
        "primitive_identity": primitive_identity,
        "name": name,
        "description": &bounded_description,
        "input_schema": &input_schema,
        "result_schema": &result_schema,
        "effects": &effects,
        "dispatch": &dispatch,
        "implementation_plan_digest": &implementation_plan_digest,
        "physical_artifact_revision_ref": &physical_artifact_revision_ref,
        "physical_artifact_digest": &physical_artifact_digest,
        "transport_result_byte_ceiling": transport_result_byte_ceiling,
        "resources": &resources,
    });
    let action_digest = AppDigest::blake3_canonical_json(&material).ok()?;
    let identity = action_identity(kind, &action_digest)?;
    Some(AppPrimitiveActionDescriptor {
        identity,
        name: name.to_owned(),
        description: bounded_description,
        input_schema,
        result_schema,
        effects,
        dispatch,
        resources,
        implementation_plan_digest,
        physical_artifact_revision_ref,
        physical_artifact_digest,
        transport_result_byte_ceiling,
        action_digest,
    })
}

fn source_ref(kind: AppPrimitiveSourceKind, bytes: &[u8]) -> Option<AppPrimitiveSourceRef> {
    let content_digest = AppDigest::blake3(bytes);
    let digest = digest_hex(&content_digest)?;
    let segment = match kind {
        AppPrimitiveSourceKind::EmbeddedPlatform => "platform",
        AppPrimitiveSourceKind::ScopedSkill => "skill",
        AppPrimitiveSourceKind::ScopedAgent => "agent",
        AppPrimitiveSourceKind::PhysicalOwner => "owner",
    };
    Some(AppPrimitiveSourceRef {
        kind,
        reference: AppReference::parse(format!("primitive-source:{segment}:{digest}")).ok()?,
        content_digest,
    })
}

fn primitive_identity(kind: AppPrimitiveKind, digest: &AppDigest) -> Option<AppReference> {
    AppReference::parse(format!(
        "primitive:{}:{}",
        kind.identity_segment(),
        digest_hex(digest)?
    ))
    .ok()
}

fn action_identity(kind: AppPrimitiveKind, digest: &AppDigest) -> Option<AppReference> {
    AppReference::parse(format!(
        "primitive-action:{}:{}",
        kind.identity_segment(),
        digest_hex(digest)?
    ))
    .ok()
}

fn digest_hex(digest: &AppDigest) -> Option<&str> {
    digest.as_str().strip_prefix("blake3:")
}

fn pack_input_schema(pack: &CapabilityPackDefinition) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for parameter in &pack.parameters {
        properties.insert(
            parameter.name.clone(),
            derive_param_schema_for_emission(parameter),
        );
        if parameter.required {
            required.push(parameter.name.clone());
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn native_action_input_schema(schema: &NativeActionSchemaDef) -> Value {
    let mut properties = serde_json::Map::new();
    for parameter in &schema.parameters {
        properties.insert(
            parameter.clone(),
            schema
                .parameter_overrides
                .get(parameter)
                .cloned()
                .unwrap_or_else(|| json!({"type": "string"})),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": &schema.required,
        "additionalProperties": false,
    })
}

fn compiled_resources(
    pack: &CapabilityPackDefinition,
    action_timeout: Option<u64>,
) -> AppPrimitiveResourceContract {
    AppPrimitiveResourceContract {
        timeout_ceiling_seconds: action_timeout.or_else(|| {
            pack.execution
                .as_ref()
                .and_then(|execution| execution.default_timeout_secs)
        }),
        required_authorities: BTreeSet::new(),
        resource_scopes: BTreeSet::new(),
        ceiling_digest: None,
    }
}

fn empty_resources() -> AppPrimitiveResourceContract {
    AppPrimitiveResourceContract {
        timeout_ceiling_seconds: None,
        required_authorities: BTreeSet::new(),
        resource_scopes: BTreeSet::new(),
        ceiling_digest: None,
    }
}

fn compiled_execution_class(
    pack: &CapabilityPackDefinition,
) -> (AppPrimitiveExecutionClass, AppPrimitiveContainment) {
    let categories = pack
        .execution
        .as_ref()
        .map(|execution| execution.categories.as_slice())
        .unwrap_or_default();
    let composition = pack
        .execution
        .as_ref()
        .and_then(|execution| execution.composition_category.as_deref());
    if pack
        .execution
        .as_ref()
        .and_then(|execution| execution.requires_browser_session)
        .unwrap_or(false)
        || composition.is_some_and(|value| value.contains("browser"))
        || categories.iter().any(|value| value == "browser")
    {
        return (
            AppPrimitiveExecutionClass::BrowserOwner,
            AppPrimitiveContainment::BrowserSession,
        );
    }
    if categories
        .iter()
        .any(|value| matches!(value.as_str(), "macos" | "desktop" | "ui_automation"))
    {
        return (
            AppPrimitiveExecutionClass::MacosHostOwner,
            AppPrimitiveContainment::MacosHost,
        );
    }
    if categories.iter().any(|value| value == "android") {
        return (
            AppPrimitiveExecutionClass::AndroidDeviceOwner,
            AppPrimitiveContainment::AndroidDevice,
        );
    }
    (
        AppPrimitiveExecutionClass::InProcessCompiled,
        AppPrimitiveContainment::InProcess,
    )
}

fn skill_execution_class(
    shape: Option<&AppToolDeclaredShape>,
) -> (AppPrimitiveExecutionClass, AppPrimitiveContainment) {
    let Some(shape) = shape else {
        return (
            AppPrimitiveExecutionClass::UniversalSkillRuntime,
            AppPrimitiveContainment::OsJail,
        );
    };
    if shape.categories.iter().any(|value| value == "android") {
        return (
            AppPrimitiveExecutionClass::AndroidDeviceOwner,
            AppPrimitiveContainment::AndroidDevice,
        );
    }
    if shape
        .composition_category
        .as_deref()
        .is_some_and(|value| value.contains("browser") || value.contains("web_operations"))
        || shape.categories.iter().any(|value| value == "browser")
    {
        return (
            AppPrimitiveExecutionClass::BrowserOwner,
            AppPrimitiveContainment::BrowserSession,
        );
    }
    if shape.categories.iter().any(|value| {
        matches!(
            value.as_str(),
            "macos" | "macos_automation" | "desktop" | "desktop_operations" | "ui_automation"
        )
    }) {
        return (
            AppPrimitiveExecutionClass::MacosHostOwner,
            AppPrimitiveContainment::MacosHost,
        );
    }
    if shape.protocol.as_deref() == Some("mcp") {
        return (
            AppPrimitiveExecutionClass::UniversalSkillRuntime,
            AppPrimitiveContainment::GovernedMcp,
        );
    }
    (
        AppPrimitiveExecutionClass::UniversalSkillRuntime,
        AppPrimitiveContainment::OsJail,
    )
}

fn dispatch_from_plan(plan: &super::app_tool_bind::AppToolBindPlan) -> AppPrimitiveDispatch {
    let status = if plan.runnable {
        AppPrimitiveDispatchStatus::Ready
    } else if plan.io_kind != AppToolIoKind::Unbound {
        AppPrimitiveDispatchStatus::Conditional
    } else {
        AppPrimitiveDispatchStatus::Blocked
    };
    AppPrimitiveDispatch {
        status,
        reason: bounded_search_text(&plan.reason),
    }
}

fn aggregate_action_dispatch(
    actions: &[AppPrimitiveActionDescriptor],
    default: AppPrimitiveDispatch,
) -> AppPrimitiveDispatch {
    if actions
        .iter()
        .any(|action| action.dispatch().status() == AppPrimitiveDispatchStatus::Ready)
    {
        return default;
    }
    if actions
        .iter()
        .any(|action| action.dispatch().status() == AppPrimitiveDispatchStatus::Conditional)
    {
        return AppPrimitiveDispatch {
            status: AppPrimitiveDispatchStatus::Conditional,
            reason: "one or more actions require runtime owner admission".to_owned(),
        };
    }
    AppPrimitiveDispatch {
        status: AppPrimitiveDispatchStatus::Blocked,
        reason: "no action has an admitted typed input contract".to_owned(),
    }
}

fn sealed_interactive_snapshot_dispatch(
    execution_class: AppPrimitiveExecutionClass,
    actions: &[AppPrimitiveActionDescriptor],
) -> Option<AppPrimitiveDispatch> {
    if !matches!(
        execution_class,
        AppPrimitiveExecutionClass::BrowserOwner
            | AppPrimitiveExecutionClass::MacosHostOwner
            | AppPrimitiveExecutionClass::AndroidDeviceOwner
    ) || actions.is_empty()
        || actions.iter().any(|action| {
            action.dispatch().status() != AppPrimitiveDispatchStatus::Ready
                || action.implementation_plan_digest().is_none()
        })
    {
        return None;
    }
    Some(AppPrimitiveDispatch {
        status: AppPrimitiveDispatchStatus::Ready,
        reason: "sealed interactive actions have admitted physical owners".to_owned(),
    })
}

fn effect_from_io(io: AppToolIoKind) -> AppPrimitiveEffect {
    match io {
        AppToolIoKind::PureTransform => AppPrimitiveEffect::Pure,
        AppToolIoKind::TrustedLocalClock => AppPrimitiveEffect::ClockRead,
        AppToolIoKind::BoundHttp => AppPrimitiveEffect::NetworkRead,
        AppToolIoKind::BoundFile => AppPrimitiveEffect::WorkspaceRead,
        AppToolIoKind::BoundWrite => AppPrimitiveEffect::WorkspaceWrite,
        AppToolIoKind::BoundTable => AppPrimitiveEffect::StructuredDataRead,
        AppToolIoKind::BoundHostRead => AppPrimitiveEffect::HostRead,
        AppToolIoKind::BoundSideEffect => AppPrimitiveEffect::ExternalMutation,
        AppToolIoKind::Device => AppPrimitiveEffect::DeviceInteraction,
        AppToolIoKind::Unbound => AppPrimitiveEffect::Undeclared,
    }
}

fn snapshot_digest(
    owner_ref: Option<&AppReference>,
    descriptors: &[Arc<AppPrimitiveDescriptor>],
    collisions: &[AppPrimitiveCollision],
    issues: &[AppPrimitiveCatalogIssue],
) -> AppDigest {
    let material = json!({
        "schema_version": APP_PRIMITIVE_DESCRIPTOR_VERSION,
        "owner_ref": owner_ref,
        "descriptors": descriptors
            .iter()
            .map(|descriptor| descriptor.descriptor_digest())
            .collect::<Vec<_>>(),
        "collisions": collisions,
        "issues": issues,
    });
    AppDigest::blake3_canonical_json(&material)
        .unwrap_or_else(|_| AppDigest::blake3(b"invalid-app-primitive-catalog"))
}

fn unavailable_snapshot(
    owner_ref: Option<AppReference>,
    mut issues: Vec<AppPrimitiveCatalogIssue>,
) -> AppPrimitiveCatalogSnapshot {
    issues.sort_by(|left, right| {
        left.code
            .cmp(right.code)
            .then_with(|| left.source_digest.cmp(&right.source_digest))
    });
    AppPrimitiveCatalogSnapshot {
        complete: false,
        snapshot_digest: snapshot_digest(owner_ref.as_ref(), &[], &[], &issues),
        owner_ref,
        descriptors: Vec::new(),
        by_identity: BTreeMap::new(),
        source_material: BTreeMap::new(),
        source_directories: BTreeMap::new(),
        platform_names: BTreeMap::new(),
        aliases: BTreeMap::new(),
        collisions: Vec::new(),
        collision_aliases: BTreeSet::new(),
        issues,
    }
}

fn search_hit(descriptor: &AppPrimitiveDescriptor) -> AppPrimitiveSearchHit {
    AppPrimitiveSearchHit {
        identity: descriptor.identity().clone(),
        descriptor_digest: descriptor.descriptor_digest().clone(),
        name: descriptor.name().to_owned(),
        description: bounded_search_text(descriptor.description()),
        kind: descriptor.kind(),
        dispatch_status: descriptor.dispatch().status(),
        actions: descriptor
            .actions()
            .iter()
            .map(|action| AppPrimitiveSearchAction {
                action_identity: action.identity().clone(),
                input_schema_digest: action.input_schema().digest().cloned(),
                result_schema_digest: action.result_schema().digest().cloned(),
                dispatch_status: action.dispatch().status(),
            })
            .collect(),
    }
}

fn search_score(name: &str, haystack: &str, needle: &str, tokens: &[&str]) -> Option<u32> {
    if needle.is_empty() {
        return Some(1);
    }
    if normalized_collision_key(name) == normalized_collision_key(needle) {
        return Some(1_000);
    }
    if !tokens.iter().all(|token| haystack.contains(token)) {
        return None;
    }
    Some(
        100 + tokens
            .iter()
            .filter(|token| normalized_search_text(name).starts_with(**token))
            .count() as u32
            * 10,
    )
}

fn normalized_search_text(value: &str) -> String {
    value
        .chars()
        .take(MAX_SEARCH_TEXT_BYTES)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bounded_search_text(value: &str) -> String {
    value
        .chars()
        .scan(0usize, |used, character| {
            let next = used.checked_add(character.len_utf8())?;
            if next > MAX_SEARCH_TEXT_BYTES {
                return None;
            }
            *used = next;
            Some(character)
        })
        .collect()
}

fn bounded_optional_text(value: Option<String>, limit: usize) -> Option<Option<String>> {
    match value {
        Some(value) if value.len() <= limit => Some(Some(value)),
        Some(_) => None,
        None => Some(None),
    }
}

fn bounded_source_name(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then_some(value)
}

fn bounded_tool_selectors(raw: Option<&Value>) -> Option<BTreeSet<String>> {
    let Some(raw) = raw else {
        return Some(BTreeSet::new());
    };
    let values = match raw {
        Value::String(value) => value
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        Value::Array(values) => values
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .map(str::to_owned)
            .collect(),
        _ => return None,
    };
    let mut selectors = BTreeSet::new();
    for selector in values {
        if selectors.len() >= super::manifest::AppPackageLimits::default().max_dependencies()
            || selector.trim().is_empty()
            || selector.len() > MAX_TOOL_SELECTOR_BYTES
        {
            return None;
        }
        selectors.insert(selector);
    }
    Some(selectors)
}

fn bounded_tool_selector_values(values: Vec<String>) -> Option<BTreeSet<String>> {
    if values.len() > super::manifest::AppPackageLimits::default().max_dependencies()
        || values
            .iter()
            .any(|selector| selector.trim().is_empty() || selector.len() > MAX_TOOL_SELECTOR_BYTES)
    {
        return None;
    }
    Some(values.into_iter().collect())
}

struct BoundedSizeWriter {
    written: usize,
    limit: usize,
}

impl Write for BoundedSizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "schema size overflow"))?;
        if next > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "schema exceeds descriptor ceiling",
            ));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size_within(value: &Value, limit: usize) -> bool {
    let mut writer = BoundedSizeWriter { written: 0, limit };
    serde_json::to_writer(&mut writer, value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::tool_eligibility::typed_app_tool_document;
    use crate::magician_v2::execution::compiled_providers::{
        embedded_compiled_pack_defs_ref, embedded_compiled_pack_yaml,
    };

    fn compiled(name: &str) -> (&'static CapabilityPackDefinition, &'static [u8]) {
        let pack = embedded_compiled_pack_defs_ref()
            .iter()
            .find(|pack| pack.name == name)
            .expect("embedded pack");
        let bytes = embedded_compiled_pack_yaml(name)
            .expect("embedded YAML")
            .as_bytes();
        (pack, bytes)
    }

    fn callable_agent_source(agent_id: &str) -> String {
        format!(
            r#"agent_id: {agent_id}
version: 1
name: Reviewed child
description: One exact callable child.
persona: Return only the reviewed typed result.
tools: [content_read]
app_tool:
  input:
    type: object
    fields:
      request:
        type: text
        required: true
  result:
    type: object
    fields:
      answer:
        type: markdown
        required: true
  max_input_bytes: 16384
  max_result_bytes: 32768
"#
        )
    }

    #[test]
    fn reserved_platform_name_survives_a_skill_collision() {
        let (pack, bytes) = compiled("content_read");
        let skill = typed_app_tool_document("content_read", "9.0.0", "");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&skill);
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();

        let resolved = snapshot
            .resolve("content_read")
            .expect("reserved platform name");
        assert_eq!(resolved.kind(), AppPrimitiveKind::CompiledTool);
        assert_eq!(snapshot.collisions().len(), 1);
        assert!(snapshot.collisions()[0].reserved_platform_name());
        let private = snapshot
            .descriptors()
            .iter()
            .find(|descriptor| descriptor.kind() == AppPrimitiveKind::ToolSkill)
            .expect("colliding skill");
        assert!(!snapshot.alias_available(private));
        assert_eq!(
            snapshot
                .resolve(private.identity().as_str())
                .expect("exact identity")
                .kind(),
            AppPrimitiveKind::ToolSkill
        );
    }

    #[test]
    fn malformed_private_source_does_not_erase_platform_primitives() {
        let (pack, bytes) = compiled("content_read");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        builder.add_tool_skill(b"not a valid skill document");
        let snapshot = builder.finish();
        assert!(snapshot.complete());
        assert_eq!(
            snapshot
                .resolve("content_read")
                .expect("platform primitive")
                .kind(),
            AppPrimitiveKind::CompiledTool
        );
        assert!(snapshot
            .issues()
            .iter()
            .any(|issue| issue.code() == "invalid_tool_skill"));
    }

    #[test]
    fn internal_data_projects_exactly_the_two_learning_review_reads() {
        // The first host-read Apps projection (plan 2.5): the descriptor keeps
        // the embedded pack identity but its Apps action surface is exactly
        // the two scoped learning review reads, both dispatch-Ready with
        // implementation-plan digests. The pack's broader diagnostics
        // vocabulary is absent from the descriptor, not merely blocked.
        let (pack, bytes) = compiled("internal_data");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();
        assert!(snapshot.complete());
        let descriptor = snapshot
            .resolve("internal_data")
            .expect("embedded internal_data descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::CompiledTool);
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
        let action_names = descriptor
            .actions()
            .iter()
            .map(|action| action.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            action_names,
            vec![
                "list_learning_candidates".to_owned(),
                "read_learning_candidate".to_owned()
            ],
            "no other internal_data action is projected for Apps"
        );
        for action in descriptor.actions() {
            assert_eq!(
                action.dispatch().status(),
                AppPrimitiveDispatchStatus::Ready,
                "{} must be dispatch-Ready",
                action.name()
            );
            assert!(
                action.implementation_plan_digest().is_some(),
                "{} seals a compiled implementation plan",
                action.name()
            );
            assert_eq!(
                action
                    .input_schema()
                    .value()
                    .and_then(|schema| schema.get("additionalProperties"))
                    .and_then(|closed| closed.as_bool()),
                Some(false),
                "{} closes its parameter surface",
                action.name()
            );
        }
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready,
            "the aggregate dispatch is Ready because the operation-less default \
             plan resolves to the bounded list read"
        );
    }

    #[test]
    fn thinking_maps_data_projects_exactly_the_two_scoped_reads() {
        // The second host-read Apps projection (the Phase 4 Brainstorm
        // verdict's re-open condition landing): the descriptor keeps the
        // embedded pack identity but its Apps action surface is exactly the
        // two scoped thinking-map reads, both dispatch-Ready with
        // implementation-plan digests. Map mutation stays absent from the
        // descriptor, not merely blocked.
        let (pack, bytes) = compiled("thinking_maps_data");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();
        assert!(snapshot.complete());
        let descriptor = snapshot
            .resolve("thinking_maps_data")
            .expect("embedded thinking_maps_data descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::CompiledTool);
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
        let action_names = descriptor
            .actions()
            .iter()
            .map(|action| action.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            action_names,
            vec!["list_maps".to_owned(), "read_map".to_owned()],
            "no other thinking_maps_data action is projected for Apps"
        );
        for action in descriptor.actions() {
            assert_eq!(
                action.dispatch().status(),
                AppPrimitiveDispatchStatus::Ready,
                "{} must be dispatch-Ready",
                action.name()
            );
            assert!(
                action.implementation_plan_digest().is_some(),
                "{} seals a compiled implementation plan",
                action.name()
            );
            assert_eq!(
                action
                    .input_schema()
                    .value()
                    .and_then(|schema| schema.get("additionalProperties"))
                    .and_then(|closed| closed.as_bool()),
                Some(false),
                "{} closes its parameter surface",
                action.name()
            );
        }
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready,
            "the aggregate dispatch is Ready because the operation-less default \
             plan resolves to the bounded list read"
        );
    }

    #[test]
    fn evidence_data_projects_exactly_the_five_bounded_reads() {
        let (pack, bytes) = compiled("evidence_data");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();
        assert!(snapshot.complete());
        let descriptor = snapshot
            .resolve("evidence_data")
            .expect("embedded evidence_data descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::CompiledTool);
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            vec![
                "list_commitments",
                "list_entities",
                "list_evidence_records",
                "list_pending_claims",
                "read_claim",
            ],
            "no decision, correction, or ingestion verb is projected"
        );
        for action in descriptor.actions() {
            assert_eq!(
                action.dispatch().status(),
                AppPrimitiveDispatchStatus::Ready,
                "{} must be dispatch-Ready",
                action.name()
            );
            assert!(action.implementation_plan_digest().is_some());
            assert_eq!(
                action.transport_result_byte_ceiling(),
                Some(
                    crate::magician_v2::apps::app_tool_bind::APP_BOUND_EVIDENCE_DATA_RESULT_CEILING
                )
            );
            assert_eq!(
                action
                    .input_schema()
                    .value()
                    .and_then(|schema| schema.get("additionalProperties"))
                    .and_then(Value::as_bool),
                Some(false)
            );
        }
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
    }

    #[test]
    fn reviewed_mcp_commerce_skill_is_ready_through_governed_mcp() {
        let source = include_str!("../../../../skillshub/zepto-mcp/SKILL.md").as_bytes();
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source);
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("zepto-mcp")
            .expect("reviewed MCP skill is lockable");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::ToolSkill);
        assert_eq!(
            descriptor.containment(),
            AppPrimitiveContainment::GovernedMcp
        );
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        let call_tool = descriptor
            .actions()
            .iter()
            .find(|action| action.name() == "call_tool")
            .expect("call_tool");
        assert_eq!(
            call_tool.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert!(call_tool.implementation_plan_digest().is_some());
        assert_eq!(
            call_tool.transport_result_byte_ceiling(),
            Some(crate::magician_v2::apps::governed_mcp::MAX_APP_GOVERNED_MCP_RESULT_BYTES)
        );
    }

    #[test]
    fn publication_metadata_populates_the_reviewed_display_name() {
        let source = typed_app_tool_document(
            "next-step",
            "1.0.0",
            "    app_publication:\n      display_name: Next Step Ranker\n",
        );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("next-step")
            .expect("published tool skill stays lockable");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::ToolSkill);
        assert_eq!(descriptor.display_name(), Some("Next Step Ranker"));
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
    }

    #[test]
    fn absent_publication_metadata_keeps_the_unmarked_descriptor() {
        let source = typed_app_tool_document("next-step", "1.0.0", "");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("next-step")
            .expect("unmarked tool skill admits exactly as before");
        assert_eq!(descriptor.display_name(), None);
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
    }

    #[test]
    fn invalid_publication_metadata_quarantines_the_tool_skill() {
        for extra_magician in [
            "    app_publication:\n      display_name: \"  \"\n",
            "    app_publication:\n      publisher: unknown field\n",
        ] {
            let source = typed_app_tool_document("next-step", "1.0.0", extra_magician);
            let mut builder = AppPrimitiveCatalogBuilder::new();
            builder.add_tool_skill(&source);
            let snapshot = builder.finish();
            assert!(
                snapshot.resolve("next-step").is_err(),
                "an invalid publication block must fail closed"
            );
            assert!(snapshot
                .issues()
                .iter()
                .any(|issue| issue.code() == "invalid_tool_skill"));
        }
    }

    #[test]
    fn duplicate_platform_alias_makes_the_snapshot_unavailable() {
        let (pack, bytes) = compiled("content_read");
        let mut conflicting = pack.clone();
        conflicting.name = "CONTENT_READ".to_owned();
        let conflicting_source = std::str::from_utf8(bytes)
            .expect("embedded YAML is UTF-8")
            .replace("name: content_read", "name: CONTENT_READ");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        builder.add_compiled_pack(&conflicting, conflicting_source.as_bytes());
        let snapshot = builder.finish();
        assert!(!snapshot.complete());
        assert!(snapshot
            .issues()
            .iter()
            .any(|issue| issue.code() == "platform_alias_conflict"));
    }

    #[test]
    fn tool_without_declared_actions_keeps_one_blocked_root_action() {
        let source = b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank.\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [next-step]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n---\nRank.\n";
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source);
        let snapshot = builder.finish();
        let descriptor = snapshot.resolve("next-step").expect("discoverable root");
        assert_eq!(descriptor.actions().len(), 1);
        assert_eq!(
            descriptor.actions()[0].dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked
        );
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked
        );
    }

    #[test]
    fn authority_free_jailed_action_is_ready_with_its_explicit_result_ceiling() {
        let source = String::from_utf8(typed_app_tool_document(
            "bounded-merge",
            "1.0.0",
            "    runtime_catalog:\n      categories: [merge]\n",
        ))
        .unwrap()
        .replace(
            "        command_prefix: []\n",
            "        command_prefix: []\n        limits:\n          stdout_bytes: 1024\n          stderr_bytes: 512\n",
        );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot.resolve("bounded-merge").expect("bounded skill");
        let action = descriptor.actions().first().expect("typed action");
        assert_eq!(
            action.transport_result_byte_ceiling(),
            Some((1_024 + 512) * 6 + 4 * 1024),
        );
        // An authority-free action (base approval `ordinary`) has a reviewed
        // plan digest and is dispatchable through the OS-jail owner; install
        // review still requires the exact native private artifact.
        assert!(action.implementation_plan_digest().is_some());
        assert_eq!(
            action.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready,
        );
    }

    #[test]
    fn jailed_action_with_unpersistable_result_contract_stays_visible_but_blocked() {
        // Declared stream limits are clamped to the app budget; only a skill
        // that declares no stdout ceiling has no persistable result.
        let source = String::from_utf8(typed_app_tool_document(
            "oversized-merge",
            "1.0.0",
            "    runtime_catalog:\n      categories: [merge]\n",
        ))
        .unwrap()
        .replace(
            "        command_prefix: []\n",
            "        command_prefix: []\n        limits:\n          stderr_bytes: 10000\n",
        );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("oversized-merge")
            .expect("unsupported result size must not erase discovery evidence");
        let action = descriptor.actions().first().expect("typed action");
        assert!(action.transport_result_byte_ceiling().is_none());
        assert!(action.implementation_plan_digest().is_none());
        assert_eq!(
            action.dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked,
        );
        assert!(action
            .dispatch()
            .reason()
            .contains("physical implementation plan"));
    }

    #[test]
    fn jailed_action_with_additive_authority_never_receives_a_physical_plan() {
        let source = String::from_utf8(typed_app_tool_document(
            "authority-merge",
            "1.0.0",
            "    runtime_catalog:\n      categories: [merge]\n",
        ))
        .unwrap()
        .replace(
            "        command_prefix: []\n",
            "        command_prefix: []\n        limits:\n          stdout_bytes: 1024\n          stderr_bytes: 512\n",
        )
        .replace(
            "    runtime_actions:\n",
            "      policy_floor:\n        required_grants: [workspace-read]\n    runtime_actions:\n",
        );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot.resolve("authority-merge").expect("reviewed skill");
        let action = descriptor.actions().first().expect("typed action");
        assert!(action.implementation_plan_digest().is_none());
        assert_eq!(
            action.dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked,
        );
    }

    #[test]
    fn browser_skill_is_projected_as_an_interactive_physical_owner() {
        let source = String::from_utf8(typed_app_tool_document("browser-tool", "1.0.0", ""))
            .expect("typed fixture is UTF-8")
            .replace(
                "---\nReturn one ranked next step.",
                "    runtime_catalog:\n      categories: [browser]\n      composition_category: web_operations\n---\nReturn one ranked next step.",
            );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("browser-tool")
            .expect("browser descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::Interactive);
        assert_eq!(
            descriptor.execution_class(),
            AppPrimitiveExecutionClass::BrowserOwner
        );
        assert_eq!(
            descriptor.containment(),
            AppPrimitiveContainment::BrowserSession
        );
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert!(descriptor
            .allowed_modes()
            .contains(&AppPrimitiveInvocationMode::InteractiveAction));
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["snapshot", "navigate", "scroll", "click"]),
        );
        assert!(descriptor
            .actions()
            .iter()
            .all(|action| action.dispatch().status() == AppPrimitiveDispatchStatus::Ready));
    }

    #[test]
    fn macos_skill_projects_only_the_eight_sealed_owner_actions() {
        let source = String::from_utf8(typed_app_tool_document(
            "macos-ui-automation",
            "1.0.0",
            "",
        ))
        .expect("typed fixture is UTF-8")
        .replace(
            "---\nReturn one ranked next step.",
            "    runtime_catalog:\n      categories: [macos_automation, ui_automation]\n      composition_category: macos_operations\n---\nReturn one ranked next step.",
        );
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("macos-ui-automation")
            .expect("macOS descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::Interactive);
        assert_eq!(
            descriptor.execution_class(),
            AppPrimitiveExecutionClass::MacosHostOwner
        );
        assert_eq!(descriptor.containment(), AppPrimitiveContainment::MacosHost);
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "launch", "focus", "snapshot", "click", "type", "key", "scroll", "drag",
            ]),
        );
        assert!(descriptor.actions().iter().all(|action| {
            action.implementation_plan_digest().is_some()
                && action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
        }));
    }

    #[test]
    fn android_apps_project_only_the_four_sealed_pack_rosters() {
        let (pack, bytes) = compiled("android_snapshot");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("android_snapshot")
            .expect("Android snapshot descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::Interactive);
        assert_eq!(
            descriptor.execution_class(),
            AppPrimitiveExecutionClass::AndroidDeviceOwner
        );
        assert_eq!(
            descriptor.containment(),
            AppPrimitiveContainment::AndroidDevice
        );
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert_eq!(descriptor.actions().len(), 1);
        assert_eq!(descriptor.actions()[0].name(), "snapshot");
        assert!(descriptor.actions()[0]
            .implementation_plan_digest()
            .is_some());
        assert_eq!(
            descriptor.actions()[0].dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );

        for (pack_name, expected) in [
            ("android_screenshot", BTreeSet::from(["screenshot"])),
            (
                "android_act",
                BTreeSet::from(["tap", "type", "key", "scroll"]),
            ),
            ("android_app", BTreeSet::from(["launch", "close"])),
        ] {
            let (pack, bytes) = compiled(pack_name);
            let mut builder = AppPrimitiveCatalogBuilder::new();
            builder.add_compiled_pack(pack, bytes);
            let pack_snapshot = builder.finish();
            let pack_descriptor = pack_snapshot
                .resolve(pack_name)
                .expect("sealed Android descriptor");
            assert_eq!(pack_descriptor.kind(), AppPrimitiveKind::Interactive);
            assert_eq!(
                pack_descriptor.dispatch().status(),
                AppPrimitiveDispatchStatus::Ready
            );
            assert_eq!(
                pack_descriptor
                    .actions()
                    .iter()
                    .map(|action| action.name())
                    .collect::<BTreeSet<_>>(),
                expected,
            );
            assert!(pack_descriptor
                .actions()
                .iter()
                .all(|action| action.dispatch().status() == AppPrimitiveDispatchStatus::Ready));
        }
    }

    #[test]
    fn callable_agent_projects_only_one_ready_typed_v3_leaf() {
        let source = callable_agent_source("reviewed-child");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        assert!(builder.add_agent_definition(source.as_bytes(), false));
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("reviewed-child")
            .expect("callable agent descriptor");
        assert_eq!(descriptor.kind(), AppPrimitiveKind::Agent);
        assert_eq!(
            descriptor.execution_class(),
            AppPrimitiveExecutionClass::AgentTask
        );
        assert_eq!(descriptor.containment(), AppPrimitiveContainment::TaskOwner);
        assert_eq!(descriptor.default_io_kind(), AppToolIoKind::Unbound);
        assert_eq!(
            descriptor.allowed_modes(),
            &BTreeSet::from([AppPrimitiveInvocationMode::AgentAsTool]),
        );
        assert_eq!(
            descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Lockable
        );
        assert_eq!(
            descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert_eq!(descriptor.actions().len(), 1);
        let action = &descriptor.actions()[0];
        assert_eq!(action.name(), "agent_as_tool");
        assert_eq!(
            action.dispatch().status(),
            AppPrimitiveDispatchStatus::Ready
        );
        assert_eq!(
            action.input_schema().state(),
            AppPrimitiveSchemaState::Inline
        );
        assert_eq!(
            action.result_schema().state(),
            AppPrimitiveSchemaState::Inline
        );
        assert_eq!(
            action.effects(),
            &BTreeSet::from([AppPrimitiveEffect::Undeclared])
        );
        assert_eq!(action.transport_result_byte_ceiling(), Some(32_768));
        assert!(action.implementation_plan_digest().is_some());
        assert!(action.physical_artifact_revision_ref().is_none());
        assert!(action.physical_artifact_digest().is_none());
    }

    #[test]
    fn default_or_untyped_agent_never_projects_callable_authority() {
        let source = callable_agent_source("reserved-runner");
        let mut default_builder = AppPrimitiveCatalogBuilder::new();
        assert!(default_builder.add_agent_definition(source.as_bytes(), true));
        let default_snapshot = default_builder.finish();
        let default_descriptor = default_snapshot
            .resolve("reserved-runner")
            .expect("default runner remains discoverable");
        assert_eq!(
            default_descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked
        );
        assert_eq!(default_descriptor.actions().len(), 1);
        assert_eq!(
            default_descriptor.actions()[0].dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked,
        );

        let untyped =
            "agent_id: untyped-child\nversion: 1\nname: Untyped\npersona: No app contract.\n";
        let mut untyped_builder = AppPrimitiveCatalogBuilder::new();
        assert!(untyped_builder.add_agent_definition(untyped.as_bytes(), false));
        let untyped_snapshot = untyped_builder.finish();
        let untyped_descriptor = untyped_snapshot
            .resolve("untyped-child")
            .expect("untyped agent remains visible");
        assert_eq!(
            untyped_descriptor.eligibility().status(),
            AppPrimitiveEligibilityStatus::Blocked
        );
        assert_eq!(
            untyped_descriptor.dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked
        );
        assert_eq!(
            untyped_descriptor.actions()[0].dispatch().status(),
            AppPrimitiveDispatchStatus::Blocked,
        );
    }

    #[test]
    fn insertion_order_does_not_change_snapshot_or_descriptor_digest() {
        let (read, read_bytes) = compiled("content_read");
        let (time, time_bytes) = compiled("time_math");
        let mut left = AppPrimitiveCatalogBuilder::new();
        left.add_compiled_pack(read, read_bytes);
        left.add_compiled_pack(time, time_bytes);
        let mut right = AppPrimitiveCatalogBuilder::new();
        right.add_compiled_pack(time, time_bytes);
        right.add_compiled_pack(read, read_bytes);
        let left = left.finish();
        let right = right.finish();
        assert_eq!(left.snapshot_digest(), right.snapshot_digest());
        assert_eq!(
            left.resolve("time_math").unwrap().descriptor_digest(),
            right.resolve("time_math").unwrap().descriptor_digest()
        );
    }

    #[test]
    fn action_identity_and_schema_digest_are_exact_and_search_is_shallow() {
        let (pack, bytes) = compiled("time_math");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(pack, bytes);
        let snapshot = builder.finish();
        let descriptor = snapshot.resolve("time_math").expect("time math");
        assert!(!descriptor.actions().is_empty());
        let action = &descriptor.actions()[0];
        assert!(action.input_schema().digest().is_some());
        assert_eq!(
            snapshot
                .load_action(descriptor.identity(), action.identity())
                .expect("exact action")
                .action_digest(),
            action.action_digest()
        );
        let hits = snapshot
            .search(&AppPrimitiveSearchQuery {
                text: "time".to_owned(),
                ..AppPrimitiveSearchQuery::default()
            })
            .expect("complete snapshot search");
        assert_eq!(hits.len(), 1);
        assert!(hits[0]
            .actions()
            .iter()
            .any(|action| action.input_schema_digest().is_some()));
    }

    #[test]
    fn descriptor_bound_fails_the_complete_snapshot_closed() {
        let (pack, bytes) = compiled("time_math");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        for _ in 0..=MAX_APP_PRIMITIVES {
            builder.add_compiled_pack(pack, bytes);
        }
        let snapshot = builder.finish();
        assert!(!snapshot.complete());
        assert!(snapshot.descriptors().is_empty());
        assert!(matches!(
            snapshot.resolve("time_math"),
            Err(AppPrimitiveResolveError::CatalogUnavailable)
        ));
    }

    fn experience_snapshot() -> Arc<AppPrimitiveCatalogSnapshot> {
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_experience_primitives();
        builder.finish()
    }

    #[test]
    fn experience_primitives_project_the_two_admission_classes() {
        let snapshot = experience_snapshot();
        assert!(snapshot.complete());
        for (name, class, containment, action_name) in [
            (
                "overlay-draw",
                AppPrimitiveExecutionClass::OverlayDrawOwner,
                AppPrimitiveContainment::OverlaySurface,
                "draw",
            ),
            (
                "narration",
                AppPrimitiveExecutionClass::NarrationOwner,
                AppPrimitiveContainment::MediaRail,
                "speak",
            ),
        ] {
            let descriptor = snapshot.resolve(name).unwrap_or_else(|_| {
                panic!("experience primitive `{name}` must resolve by platform name")
            });
            assert_eq!(descriptor.kind(), AppPrimitiveKind::Interactive, "{name}");
            assert_eq!(descriptor.execution_class(), class, "{name}");
            assert_eq!(descriptor.containment(), containment, "{name}");
            assert_eq!(
                descriptor.source().kind(),
                AppPrimitiveSourceKind::EmbeddedPlatform,
                "{name}"
            );
            assert!(snapshot.source_material(descriptor.identity()).is_some());
            assert_eq!(
                descriptor.eligibility().status(),
                AppPrimitiveEligibilityStatus::Lockable,
                "{name}"
            );
            assert!(descriptor.exposure().apps(), "{name}");
            assert!(!descriptor.exposure().agents(), "{name}");
            assert_eq!(
                descriptor.allowed_modes(),
                &BTreeSet::from([
                    AppPrimitiveInvocationMode::CallTool,
                    AppPrimitiveInvocationMode::InteractiveAction
                ]),
                "{name}"
            );
            assert_eq!(descriptor.actions().len(), 1, "{name} has one leaf");
            let action = &descriptor.actions()[0];
            assert_eq!(action.name(), action_name, "{name}");
            assert!(action.input_schema().digest().is_some(), "{name}");
            assert!(action.result_schema().digest().is_some(), "{name}");
            assert!(action.implementation_plan_digest().is_some(), "{name}");
            assert!(action.transport_result_byte_ceiling().is_some(), "{name}");
            assert!(action.resources().ceiling_digest().is_some(), "{name}");
        }
    }

    #[test]
    fn experience_primitives_stay_conditional_until_a_consumer_lands() {
        // The plan-1.3 deliverable is the admitted, reviewed vocabulary —
        // not execution. Both the descriptor and its single action must stay
        // Conditional, and the locked action binding must therefore never
        // become dispatchable.
        let snapshot = experience_snapshot();
        for name in ["overlay-draw", "narration"] {
            let descriptor = snapshot.resolve(name).expect("experience primitive");
            assert_eq!(
                descriptor.dispatch().status(),
                AppPrimitiveDispatchStatus::Conditional,
                "{name} descriptor must not be dispatch-Ready"
            );
            assert!(
                descriptor.dispatch().reason().contains("plan 1.3"),
                "{name} dispatch reason must name the pending consumer"
            );
            let action = &descriptor.actions()[0];
            assert_eq!(
                action.dispatch().status(),
                AppPrimitiveDispatchStatus::Conditional,
                "{name} action must not be dispatch-Ready"
            );
            let binding =
                crate::magician_v2::apps::package_lock::AppLockedPrimitiveBinding::from_descriptor(
                    &descriptor,
                )
                .expect("lockable admission descriptor");
            assert!(
                binding
                    .actions()
                    .iter()
                    .all(|locked| !locked.dispatchable()),
                "{name} locked actions must stay non-dispatchable"
            );
        }
    }
}
