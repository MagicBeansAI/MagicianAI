//! Shared authoring discovery catalog.
//!
//! CLI (`magician app tools|agents|personalities|procedures`) and the
//! `/apps/authoring/*` HTTP routes emit the same JSON. The catalog is
//! read-only: it never mints a grant, lock, or installation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tool_runtime_core::manifest_parser::{
    parse_skill_frontmatter, parse_skill_magician_extension, parse_skill_runtime_package,
};

use super::{
    app_tool_bind::AppToolIoKind,
    models::{AppDigest, AppReference},
    primitive_catalog::{
        AppPrimitiveActionDescriptor, AppPrimitiveCatalogBuilder, AppPrimitiveCatalogSnapshot,
        AppPrimitiveCollision, AppPrimitiveContainment, AppPrimitiveDispatchStatus,
        AppPrimitiveEffect, AppPrimitiveEligibilityStatus, AppPrimitiveExecutionClass,
        AppPrimitiveInvocationMode, AppPrimitiveKind, AppPrimitiveResolveError,
        AppPrimitiveSchemaState, AppPrimitiveSourceKind, MAX_APP_PRIMITIVES,
        MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::compiled_providers::{embedded_compiled_pack_defs_ref, embedded_compiled_pack_yaml},
    skills::{loader::parse_manifest as parse_skill_manifest, manifest::InferredKind},
};

pub const DEFAULT_APP_WORKFLOW_AGENT: &str = "personal-assistant";
const MAX_PRIMITIVE_RESOLVER_ROOTS: usize = 64;
const MAX_PRIMITIVE_RESOLVER_ROOT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthoringToolKind {
    Compiled,
    Skill,
    Agent,
    Interactive,
}

impl AuthoringToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compiled => "compiled",
            Self::Skill => "skill",
            Self::Agent => "agent",
            Self::Interactive => "interactive",
        }
    }

    pub fn parse_filter(raw: &str) -> Result<Self, AuthoringCatalogError> {
        match raw.trim() {
            "compiled" => Ok(Self::Compiled),
            "skill" => Ok(Self::Skill),
            "agent" => Ok(Self::Agent),
            "interactive" => Ok(Self::Interactive),
            other => Err(AuthoringCatalogError::InvalidFilter(format!(
                "kind must be compiled, skill, agent or interactive, not `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuthoringDiscoveryRoots {
    pub skill_dirs: Vec<PathBuf>,
    pub agent_template_dirs: Vec<PathBuf>,
}

impl AuthoringDiscoveryRoots {
    pub fn from_explicit(
        skill_dirs: impl IntoIterator<Item = PathBuf>,
        agent_template_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            // Preserve caller intent. The strict scanner must see and reject a
            // missing, unreadable or non-directory explicit root instead of
            // silently returning a platform-only "complete" catalog.
            skill_dirs: unique_dirs(skill_dirs),
            agent_template_dirs: unique_dirs(agent_template_dirs),
        }
    }

    fn from_runtime(
        optional_skill_dirs: impl IntoIterator<Item = PathBuf>,
        optional_agent_template_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let optional_agent_template_dirs =
            optional_agent_template_dirs.into_iter().collect::<Vec<_>>();
        let mut skill_dirs = unique_existing_dirs(optional_skill_dirs);
        append_unique_dirs(
            &mut skill_dirs,
            crate::magician_v2::config_extras::extra_skills_dirs(),
        );
        let mut agent_template_dirs =
            unique_existing_dirs(optional_agent_template_dirs.iter().take(1).cloned());
        append_unique_dirs(
            &mut agent_template_dirs,
            crate::magician_v2::config_extras::extra_agent_template_dirs(),
        );
        let fallbacks = optional_agent_template_dirs.iter().skip(1).cloned();
        for fallback in unique_existing_dirs(fallbacks) {
            if !agent_template_dirs.contains(&fallback) {
                agent_template_dirs.push(fallback);
            }
        }
        Self {
            skill_dirs,
            agent_template_dirs,
        }
    }

    /// Live catalog: only `scopes/<principal>/<workspace>/skills`.
    /// Repo `skillshub/` is the install source, not a search root.
    pub fn for_scope(storage_root: impl AsRef<Path>, principal: &str, workspace: &str) -> Self {
        let scope = storage_root
            .as_ref()
            .join("scopes")
            .join(principal)
            .join(workspace);
        Self::from_runtime(
            [scope.join("skills")],
            [
                scope.join("agent_runtime"),
                storage_root.as_ref().join("system/agent_templates"),
            ],
        )
    }

    pub fn for_live_scope(principal: &str, workspace: &str) -> Self {
        Self::for_scope(
            crate::magician_v2::process_storage::runtime_root(),
            principal,
            workspace,
        )
    }

    pub fn for_workspace_scope(
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Self {
        Self::from_runtime(
            [workspace_layout.scope_skills_root(principal, workspace)],
            [
                workspace_layout.scoped_agent_runtime_root(principal, workspace),
                workspace_layout.system_agent_template_root(),
            ],
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringCatalogList<T> {
    pub status: &'static str,
    pub count: usize,
    pub items: Vec<T>,
}

impl<T> AuthoringCatalogList<T> {
    fn ok(items: Vec<T>) -> Self {
        Self {
            status: "ok",
            count: items.len(),
            items,
        }
    }

    fn from_snapshot(snapshot: &AppPrimitiveCatalogSnapshot, items: Vec<T>) -> Self {
        Self {
            status: if !snapshot.complete() {
                "unavailable"
            } else if snapshot.issues().is_empty() {
                "ok"
            } else {
                "degraded"
            },
            count: items.len(),
            items,
        }
    }

    fn unavailable(items: Vec<T>) -> Self {
        Self {
            status: "unavailable",
            count: items.len(),
            items,
        }
    }

    fn degraded(items: Vec<T>) -> Self {
        Self {
            status: "degraded",
            count: items.len(),
            items,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringToolEntry {
    /// Exact immutable descriptor identity. Names and aliases never grant
    /// authority and may be absent when a private source collides.
    pub primitive_id: AppReference,
    pub descriptor_digest: AppDigest,
    pub name: String,
    pub kind: &'static str,
    pub version: Option<String>,
    pub description: String,
    /// Discovery proves only a bounded typed shape. Mutable skill-directory
    /// entries still require immutable reviewed-catalog evidence during pack;
    /// this signal prevents authoring UI from presenting shape eligibility as
    /// publication approval.
    pub lock_review_required: bool,
    pub app_eligible: bool,
    pub dispatchable: bool,
    pub io_kind: AppToolIoKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ineligible_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose_apps: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose_agents: Option<bool>,
    pub yaml_declaration: String,
    pub layer: String,
    pub alias_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<AuthoringPrimitiveCollision>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringPrimitiveCollision {
    pub alias: String,
    pub identities: Vec<AppReference>,
    pub reserved_platform_name: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringToolActionEntry {
    pub action_id: AppReference,
    pub action_digest: AppDigest,
    pub name: String,
    pub effects: Vec<AppPrimitiveEffect>,
    pub dispatch_status: AppPrimitiveDispatchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema_digest: Option<AppDigest>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringToolShow {
    pub status: &'static str,
    #[serde(flatten)]
    pub tool: AuthoringToolEntry,
    pub actions: Vec<String>,
    pub action_descriptors: Vec<AuthoringToolActionEntry>,
    pub yaml_snippet: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringAgentEntry {
    pub primitive_id: AppReference,
    pub descriptor_digest: AppDigest,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub description: String,
    pub default_runner: bool,
    pub yaml_declaration: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringPersonalityEntry {
    pub name: String,
    pub description: String,
    pub yaml_declaration: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthoringProcedureEntry {
    pub primitive_id: AppReference,
    pub descriptor_digest: AppDigest,
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    pub yaml_declaration: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthoringCatalogError {
    #[error("{0}")]
    InvalidFilter(String),
    #[error("authoring catalog has no tool named `{0}`")]
    ToolNotFound(String),
    #[error("authoring primitive alias `{0}` is ambiguous; use its exact primitive identity")]
    AmbiguousPrimitive(String),
    #[error("authoring primitive catalog is unavailable")]
    CatalogUnavailable,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AuthoringToolListFilter {
    pub app_eligible_only: bool,
    pub kind: Option<AuthoringToolKind>,
}

#[derive(Deserialize)]
struct CatalogFrontmatter {
    name: String,
    #[serde(default)]
    #[serde(rename = "version")]
    _version: Option<String>,
    #[serde(default)]
    description: String,
}

pub fn list_authoring_tools(
    roots: &AuthoringDiscoveryRoots,
    filter: AuthoringToolListFilter,
) -> AuthoringCatalogList<AuthoringToolEntry> {
    let snapshot = resolve_authoring_primitive_catalog(roots);
    let mut items: Vec<AuthoringToolEntry> = snapshot
        .descriptors()
        .iter()
        .filter(|descriptor| is_public_authoring_tool(&snapshot, descriptor))
        .map(|descriptor| project_tool_entry(&snapshot, descriptor))
        .filter(|entry| {
            filter
                .kind
                .map(|kind| entry.kind == kind.as_str())
                .unwrap_or(true)
                && (!filter.app_eligible_only || entry.app_eligible)
        })
        .collect();
    items.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.primitive_id.cmp(&right.primitive_id))
    });
    AuthoringCatalogList::from_snapshot(&snapshot, items)
}

pub fn show_authoring_tool(
    roots: &AuthoringDiscoveryRoots,
    name: &str,
) -> Result<AuthoringToolShow, AuthoringCatalogError> {
    let snapshot = resolve_authoring_primitive_catalog(roots);
    let descriptor = snapshot.resolve(name).map_err(|error| match error {
        AppPrimitiveResolveError::AmbiguousAlias => {
            AuthoringCatalogError::AmbiguousPrimitive(name.trim().to_owned())
        },
        AppPrimitiveResolveError::CatalogUnavailable => AuthoringCatalogError::CatalogUnavailable,
        AppPrimitiveResolveError::NotFound => {
            AuthoringCatalogError::ToolNotFound(name.trim().to_owned())
        },
    })?;
    if !is_public_authoring_tool(&snapshot, &descriptor) {
        return Err(AuthoringCatalogError::ToolNotFound(name.trim().to_owned()));
    }
    let entry = project_tool_entry(&snapshot, &descriptor);
    let actions = descriptor
        .actions()
        .iter()
        .map(|action| action.name().to_owned())
        .collect();
    let action_descriptors = descriptor
        .actions()
        .iter()
        .map(project_action_entry)
        .collect();
    let yaml_snippet = tool_yaml_snippet(&entry);
    Ok(AuthoringToolShow {
        status: if snapshot.issues().is_empty() {
            "ok"
        } else {
            "degraded"
        },
        tool: entry,
        actions,
        action_descriptors,
        yaml_snippet,
    })
}

pub fn list_authoring_agents(
    roots: &AuthoringDiscoveryRoots,
) -> AuthoringCatalogList<AuthoringAgentEntry> {
    let snapshot = resolve_authoring_primitive_catalog(roots);
    let mut items = snapshot
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.kind() == AppPrimitiveKind::Agent)
        .map(|descriptor| AuthoringAgentEntry {
            primitive_id: descriptor.identity().clone(),
            descriptor_digest: descriptor.descriptor_digest().clone(),
            name: descriptor.name().to_owned(),
            display_name: descriptor.display_name().map(str::to_owned),
            description: descriptor.description().to_owned(),
            default_runner: descriptor.name() == DEFAULT_APP_WORKFLOW_AGENT,
            yaml_declaration: format!("agent: {}", descriptor.name()),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.primitive_id.cmp(&right.primitive_id))
    });
    AuthoringCatalogList::from_snapshot(&snapshot, items)
}

pub fn list_authoring_personalities(
    roots: &AuthoringDiscoveryRoots,
) -> AuthoringCatalogList<AuthoringPersonalityEntry> {
    let scan = match scan_classified_skill_dirs(&roots.skill_dirs) {
        Ok(scan) => scan,
        Err(_) => return AuthoringCatalogList::unavailable(Vec::new()),
    };
    let mut items = Vec::new();
    for classified in scan.items {
        if classified.class != ClassifiedSkillKind::Personality {
            continue;
        }
        items.push(AuthoringPersonalityEntry {
            yaml_declaration: format!("personality: {}", classified.frontmatter.name),
            name: classified.frontmatter.name,
            description: classified.frontmatter.description,
        });
    }
    items.sort_by(|left, right| left.name.cmp(&right.name));
    if scan.invalid_sources.is_empty() {
        AuthoringCatalogList::ok(items)
    } else {
        AuthoringCatalogList::degraded(items)
    }
}

pub fn list_authoring_procedures(
    roots: &AuthoringDiscoveryRoots,
) -> AuthoringCatalogList<AuthoringProcedureEntry> {
    let snapshot = resolve_authoring_primitive_catalog(roots);
    let mut items = snapshot
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.kind() == AppPrimitiveKind::ProcedureSkill)
        .map(|descriptor| AuthoringProcedureEntry {
            primitive_id: descriptor.identity().clone(),
            descriptor_digest: descriptor.descriptor_digest().clone(),
            yaml_declaration: procedure_yaml_declaration(
                descriptor.name(),
                descriptor.semantic_version(),
            ),
            name: descriptor.name().to_owned(),
            version: descriptor.semantic_version().map(str::to_owned),
            description: descriptor.description().to_owned(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.primitive_id.cmp(&right.primitive_id))
    });
    AuthoringCatalogList::from_snapshot(&snapshot, items)
}

/// Single internal resolver seam shared by CLI and HTTP authoring. Later lock,
/// review and supported-public projections consume the immutable snapshot or
/// its binding; they must not re-infer primitive identity from these DTOs.
pub fn resolve_authoring_primitive_catalog(
    roots: &AuthoringDiscoveryRoots,
) -> Arc<AppPrimitiveCatalogSnapshot> {
    let owner_ref = match catalog_owner_ref(roots) {
        Ok(owner_ref) => owner_ref,
        Err(error) => {
            let mut builder = AppPrimitiveCatalogBuilder::new();
            builder.reject("resolver_root_bound_exceeded", error.as_bytes());
            return builder.finish();
        },
    };
    let mut builder = AppPrimitiveCatalogBuilder::for_owner(owner_ref);
    for pack in embedded_compiled_pack_defs_ref() {
        if let Some(yaml) = embedded_compiled_pack_yaml(pack.name.as_str()) {
            builder.add_compiled_pack(pack, yaml.as_bytes());
        }
    }
    // Plan-1.3 experience admission primitives ride every resolver snapshot:
    // discovery/lock evidence only, dispatch stays fail-closed until the
    // reviewed physical-owner adapter lands.
    builder.add_experience_primitives();
    match scan_classified_skill_dirs(&roots.skill_dirs) {
        Ok(scan) => {
            for invalid_source in scan.invalid_sources {
                builder.skip("invalid_scoped_skill", &invalid_source);
            }
            for classified in scan.items {
                match classified.class {
                    ClassifiedSkillKind::Tool => builder.add_tool_skill_from_directory(
                        &classified.source,
                        Some(classified.source_directory),
                    ),
                    ClassifiedSkillKind::Procedure => {
                        builder.add_procedure_skill(&classified.source)
                    },
                    ClassifiedSkillKind::Personality => {},
                }
            }
        },
        Err(error) => builder.reject("skill_catalog_scan_failed", error.as_bytes()),
    }
    let mut default_agent_digest = None;
    let mut agent_scan_failed = false;
    match scan_agent_template_sources(&roots.agent_template_dirs) {
        Ok(sources) => {
            for scanned in sources {
                if !scanned.identity_valid {
                    builder.skip("invalid_agent_definition", &scanned.source);
                    continue;
                }
                let source = scanned.source;
                let is_default = is_default_agent(&source);
                if is_default {
                    let digest = AppDigest::blake3(&source);
                    match default_agent_digest.as_ref() {
                        Some(existing) if existing == &digest => continue,
                        Some(_) => {
                            builder.reject("default_agent_definition_conflict", &source);
                            agent_scan_failed = true;
                            break;
                        },
                        None => {},
                    }
                    if builder.add_agent_definition(&source, true) {
                        default_agent_digest = Some(digest);
                    } else {
                        agent_scan_failed = true;
                        break;
                    }
                } else {
                    builder.add_agent_definition(&source, false);
                }
            }
        },
        Err(error) => {
            // The catalog issue carries only a digest of this string, which
            // tells an operator nothing. A failed agent scan makes EVERY
            // primitive unresolvable, so the reason has to be legible.
            tracing::warn!(
                error = %error,
                "agent catalog scan failed; every app primitive is now unresolvable"
            );
            builder.reject("agent_catalog_scan_failed", error.as_bytes());
            agent_scan_failed = true;
        },
    }
    if default_agent_digest.is_none() && !agent_scan_failed {
        builder.add_default_agent(
            DEFAULT_APP_WORKFLOW_AGENT,
            "Personal assistant",
            "Default app workflow runner. Needs no extra grant.",
        );
    }
    builder.finish()
}

fn project_tool_entry(
    snapshot: &AppPrimitiveCatalogSnapshot,
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> AuthoringToolEntry {
    let kind = authoring_tool_kind(descriptor)
        .expect("tool projection filters to supported authoring leaves");
    let collision = snapshot.collision_for(descriptor).map(project_collision);
    let alias_available = snapshot.alias_available(descriptor);
    AuthoringToolEntry {
        primitive_id: descriptor.identity().clone(),
        descriptor_digest: descriptor.descriptor_digest().clone(),
        yaml_declaration: tool_yaml_declaration(
            descriptor.name(),
            descriptor.semantic_version(),
            (!alias_available && descriptor.kind() != AppPrimitiveKind::CompiledTool)
                .then_some(descriptor.identity()),
            authoring_leaf_action(descriptor),
        ),
        name: descriptor.name().to_owned(),
        kind: kind.as_str(),
        version: descriptor.semantic_version().map(str::to_owned),
        description: descriptor.description().to_owned(),
        lock_review_required: descriptor.kind() != AppPrimitiveKind::CompiledTool,
        app_eligible: descriptor.exposure().apps()
            && descriptor.eligibility().status() == AppPrimitiveEligibilityStatus::Lockable,
        dispatchable: descriptor.dispatch().status() == AppPrimitiveDispatchStatus::Ready,
        io_kind: descriptor.default_io_kind(),
        dispatch_note: Some(authoring_dispatch_note(descriptor)),
        ineligible_reason: descriptor.eligibility().reason().map(str::to_owned),
        expose_apps: Some(descriptor.exposure().apps()),
        expose_agents: Some(descriptor.exposure().agents()),
        layer: match descriptor.kind() {
            AppPrimitiveKind::CompiledTool => "built-in",
            AppPrimitiveKind::ToolSkill => "skills-dir",
            AppPrimitiveKind::Agent => "agent-dir",
            AppPrimitiveKind::Interactive => "skills-dir",
            AppPrimitiveKind::ProcedureSkill => {
                unreachable!("procedure descriptors are not tool leaves")
            },
        }
        .to_owned(),
        alias_available,
        collision,
    }
}

/// The note the authoring surface shows for a tool that cannot dispatch.
///
/// A primitive whose every action is blocked carries the aggregate reason
/// "no action has an admitted typed input contract", and its actions mostly
/// carry "the exact physical implementation plan is not reviewable" — the
/// plan digest is checked before the binder and contain readiness that
/// actually withheld it. Neither names a cause. The bind planner does (e.g.
/// the OS-jail contain a CLI skill still needs), so a blocked compiled tool
/// or tool skill shows the planner's reason; otherwise a reason every blocked
/// action shares is used. Display only: the descriptor's own dispatch, and
/// so its digest, is unchanged.
fn authoring_dispatch_note(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> String {
    const UNINFORMATIVE: [&str; 2] = [
        "no action has an admitted typed input contract",
        "the exact physical implementation plan is not reviewable",
    ];
    let aggregate = descriptor.dispatch().reason();
    if descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Blocked {
        return aggregate.to_owned();
    }
    let actions = descriptor.actions();
    let shared = actions
        .first()
        .map(|action| action.dispatch().reason())
        .filter(|first| {
            !first.is_empty()
                && actions.iter().all(|action| {
                    action.dispatch().status() == AppPrimitiveDispatchStatus::Blocked
                        && action.dispatch().reason() == *first
                })
        });
    let note = shared.unwrap_or(aggregate);
    if !UNINFORMATIVE.contains(&note) {
        return note.to_owned();
    }
    let contain = match (descriptor.kind(), descriptor.containment()) {
        (AppPrimitiveKind::CompiledTool, AppPrimitiveContainment::InProcess) => {
            Some(super::app_tool_bind::AppToolContainProfile::InProcessCompiled)
        },
        (AppPrimitiveKind::ToolSkill, AppPrimitiveContainment::OsJail) => {
            Some(super::app_tool_bind::AppToolContainProfile::OsJail)
        },
        (AppPrimitiveKind::ToolSkill, AppPrimitiveContainment::GovernedMcp) => {
            Some(super::app_tool_bind::AppToolContainProfile::GovernedMcp)
        },
        _ => None,
    };
    contain
        .map(|contain| super::app_tool_bind::app_tool_dispatch_note(descriptor.name(), contain))
        .filter(|planned| !planned.dispatchable && !planned.reason.is_empty())
        .map(|planned| planned.reason)
        .unwrap_or_else(|| note.to_owned())
}

fn authoring_tool_kind(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> Option<AuthoringToolKind> {
    match descriptor.kind() {
        AppPrimitiveKind::CompiledTool => Some(AuthoringToolKind::Compiled),
        AppPrimitiveKind::ToolSkill => Some(AuthoringToolKind::Skill),
        AppPrimitiveKind::Agent if is_exact_authorable_agent_leaf(descriptor) => {
            Some(AuthoringToolKind::Agent)
        },
        AppPrimitiveKind::Interactive if is_exact_authorable_interactive_leaf(descriptor) => {
            Some(AuthoringToolKind::Interactive)
        },
        _ => None,
    }
}

fn is_public_authoring_tool(
    snapshot: &AppPrimitiveCatalogSnapshot,
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> bool {
    match authoring_tool_kind(descriptor) {
        Some(AuthoringToolKind::Interactive) => snapshot.alias_available(descriptor),
        Some(AuthoringToolKind::Agent) => {
            super::manifest::normalized_collision_key(descriptor.name()) != "browser"
                && snapshot.alias_available(descriptor)
        },
        Some(AuthoringToolKind::Compiled | AuthoringToolKind::Skill) => {
            super::manifest::normalized_collision_key(descriptor.name()) != "browser"
        },
        None => false,
    }
}

fn authoring_leaf_action(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> Option<&'static str> {
    match descriptor.kind() {
        AppPrimitiveKind::Agent if is_exact_authorable_agent_leaf(descriptor) => {
            Some("agent_as_tool")
        },
        AppPrimitiveKind::Interactive if is_exact_authorable_interactive_leaf(descriptor) => {
            match descriptor.name() {
                "browser" | "macos-ui-automation" | "android_snapshot" => Some("snapshot"),
                "android_screenshot" => Some("screenshot"),
                "android_act" => Some("tap"),
                "android_app" => Some("launch"),
                _ => None,
            }
        },
        _ => None,
    }
}

fn is_exact_authorable_agent_leaf(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Agent
        || descriptor.source().kind() != AppPrimitiveSourceKind::ScopedAgent
        || descriptor.execution_class() != AppPrimitiveExecutionClass::AgentTask
        || descriptor.containment() != AppPrimitiveContainment::TaskOwner
        || !descriptor.exposure().apps()
        || !descriptor.exposure().authoring()
        || descriptor.eligibility().status() != AppPrimitiveEligibilityStatus::Lockable
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.allowed_modes() != &BTreeSet::from([AppPrimitiveInvocationMode::AgentAsTool])
        || descriptor.actions().len() != 1
    {
        return false;
    }
    let action = &descriptor.actions()[0];
    action.name() == "agent_as_tool"
        && action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
        && action.input_schema().state() == AppPrimitiveSchemaState::Inline
        && action.result_schema().state() == AppPrimitiveSchemaState::Inline
        && action.effects() == &BTreeSet::from([AppPrimitiveEffect::Undeclared])
        && action.implementation_plan_digest().is_some()
        && action.physical_artifact_revision_ref().is_none()
        && action.physical_artifact_digest().is_none()
        && action
            .transport_result_byte_ceiling()
            .is_some_and(|ceiling| ceiling > 0 && ceiling <= 256 * 1024)
        && action.resources().required_authorities().is_empty()
        && action.resources().resource_scopes().is_empty()
}

fn is_exact_authorable_interactive_leaf(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Interactive
        || !descriptor.exposure().apps()
        || !descriptor.exposure().authoring()
        || descriptor.eligibility().status() != AppPrimitiveEligibilityStatus::Lockable
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.allowed_modes()
            != &BTreeSet::from([
                AppPrimitiveInvocationMode::CallTool,
                AppPrimitiveInvocationMode::InteractiveAction,
            ])
    {
        return false;
    }
    let expected_names = match descriptor.name() {
        "browser"
            if descriptor.source().kind() == AppPrimitiveSourceKind::ScopedSkill
                && descriptor.execution_class() == AppPrimitiveExecutionClass::BrowserOwner
                && descriptor.containment() == AppPrimitiveContainment::BrowserSession =>
        {
            BTreeSet::from(["snapshot", "navigate", "scroll", "click"])
        },
        "macos-ui-automation"
            if descriptor.source().kind() == AppPrimitiveSourceKind::ScopedSkill
                && descriptor.execution_class() == AppPrimitiveExecutionClass::MacosHostOwner
                && descriptor.containment() == AppPrimitiveContainment::MacosHost =>
        {
            BTreeSet::from([
                "launch", "focus", "snapshot", "click", "type", "key", "scroll", "drag",
            ])
        },
        "android_snapshot"
            if descriptor.execution_class() == AppPrimitiveExecutionClass::AndroidDeviceOwner
                && descriptor.containment() == AppPrimitiveContainment::AndroidDevice =>
        {
            BTreeSet::from(["snapshot"])
        },
        "android_screenshot"
            if descriptor.execution_class() == AppPrimitiveExecutionClass::AndroidDeviceOwner
                && descriptor.containment() == AppPrimitiveContainment::AndroidDevice =>
        {
            BTreeSet::from(["screenshot"])
        },
        "android_act"
            if descriptor.execution_class() == AppPrimitiveExecutionClass::AndroidDeviceOwner
                && descriptor.containment() == AppPrimitiveContainment::AndroidDevice =>
        {
            BTreeSet::from(["tap", "type", "key", "scroll"])
        },
        "android_app"
            if descriptor.execution_class() == AppPrimitiveExecutionClass::AndroidDeviceOwner
                && descriptor.containment() == AppPrimitiveContainment::AndroidDevice =>
        {
            BTreeSet::from(["launch", "close"])
        },
        _ => return false,
    };
    descriptor
        .actions()
        .iter()
        .map(|action| action.name())
        .collect::<BTreeSet<_>>()
        == expected_names
        && descriptor.actions().iter().all(|action| {
            let (input_digest, result_digest, implementation, ceiling, resource_scope) =
                match descriptor.execution_class() {
                    AppPrimitiveExecutionClass::BrowserOwner => {
                        let input =
                            super::browser_capability::browser_action_input_schema(action.name())
                                .and_then(|schema| AppDigest::blake3_canonical_json(&schema).ok());
                        let result =
                            AppDigest::blake3_canonical_json(&if action.name() == "snapshot" {
                                super::browser_capability::browser_observe_result_schema()
                            } else {
                                super::browser_capability::browser_action_result_schema()
                            })
                            .ok();
                        let ceiling = if action.name() == "snapshot" {
                            super::browser_capability::APP_BROWSER_OBSERVE_RESULT_CEILING
                        } else {
                            super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING
                        };
                        (
                        input,
                        result,
                        Some(
                            super::browser_capability::app_browser_runtime_implementation_digest(),
                        ),
                        ceiling,
                        "browser",
                    )
                    },
                    AppPrimitiveExecutionClass::MacosHostOwner => {
                        let input = super::macos_host::macos_action_input_schema(action.name())
                            .and_then(|schema| AppDigest::blake3_canonical_json(&schema).ok());
                        let result =
                            AppDigest::blake3_canonical_json(&if action.name() == "snapshot" {
                                super::macos_host::macos_observe_result_schema()
                            } else {
                                super::macos_host::macos_action_result_schema()
                            })
                            .ok();
                        let implementation = super::macos_host::macos_operation(action.name())
                            .and_then(|operation| {
                                super::macos_host::macos_operation_implementation_plan_digest(
                                    descriptor.source().content_digest(),
                                    operation,
                                )
                                .ok()
                            });
                        let ceiling = if action.name() == "snapshot" {
                            super::macos_host::APP_MACOS_OBSERVE_RESULT_CEILING
                        } else {
                            super::macos_host::APP_MACOS_ACTION_RESULT_CEILING
                        };
                        (input, result, implementation, ceiling, "macos_host")
                    },
                    AppPrimitiveExecutionClass::AndroidDeviceOwner => {
                        let input =
                            super::android_device::android_action_input_schema(action.name())
                                .and_then(|schema| AppDigest::blake3_canonical_json(&schema).ok());
                        let result =
                            super::android_device::android_action_result_schema(action.name())
                                .and_then(|schema| AppDigest::blake3_canonical_json(&schema).ok());
                        let implementation =
                            super::android_device::app_android_action_implementation_plan_digest(
                                descriptor.source().content_digest(),
                                action.name(),
                            )
                            .ok();
                        let ceiling = match action.name() {
                            "snapshot" => {
                                super::android_device::APP_ANDROID_SNAPSHOT_RESULT_CEILING
                            },
                            "screenshot" => {
                                super::android_device::APP_ANDROID_SCREENSHOT_RESULT_CEILING
                            },
                            _ => super::android_device::APP_ANDROID_ACTION_RESULT_CEILING,
                        };
                        (input, result, implementation, ceiling, "android_device")
                    },
                    _ => return false,
                };
            action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
                && action.input_schema().digest() == input_digest.as_ref()
                && action.result_schema().digest() == result_digest.as_ref()
                && action.implementation_plan_digest() == implementation.as_ref()
                && action.physical_artifact_revision_ref().is_none()
                && action.physical_artifact_digest().is_none()
                && action.transport_result_byte_ceiling() == Some(ceiling)
                && action.resources().timeout_ceiling_seconds() == Some(300)
                && action.resources().required_authorities().is_empty()
                && action.resources().resource_scopes()
                    == &BTreeSet::from([resource_scope.to_owned()])
                && action.resources().ceiling_digest().is_some()
        })
}

fn project_action_entry(action: &AppPrimitiveActionDescriptor) -> AuthoringToolActionEntry {
    AuthoringToolActionEntry {
        action_id: action.identity().clone(),
        action_digest: action.action_digest().clone(),
        name: action.name().to_owned(),
        effects: action.effects().iter().copied().collect(),
        dispatch_status: action.dispatch().status(),
        input_schema_digest: action.input_schema().digest().cloned(),
        result_schema_digest: action.result_schema().digest().cloned(),
    }
}

fn project_collision(collision: &AppPrimitiveCollision) -> AuthoringPrimitiveCollision {
    AuthoringPrimitiveCollision {
        alias: collision.alias().to_owned(),
        identities: collision.identities().to_vec(),
        reserved_platform_name: collision.reserved_platform_name(),
    }
}

fn catalog_owner_ref(roots: &AuthoringDiscoveryRoots) -> Result<AppReference, String> {
    let root_count = roots
        .skill_dirs
        .len()
        .checked_add(roots.agent_template_dirs.len())
        .filter(|count| *count <= MAX_PRIMITIVE_RESOLVER_ROOTS)
        .ok_or_else(|| {
            format!("primitive resolver exceeds {MAX_PRIMITIVE_RESOLVER_ROOTS} roots")
        })?;
    let _ = root_count;
    let mut roots_material = roots
        .skill_dirs
        .iter()
        .chain(roots.agent_template_dirs.iter())
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let root_bytes = roots_material.iter().try_fold(0usize, |total, path| {
        total.checked_add(path.len()).ok_or(())
    });
    if !matches!(root_bytes, Ok(total) if total <= MAX_PRIMITIVE_RESOLVER_ROOT_BYTES) {
        return Err(format!(
            "primitive resolver roots exceed {MAX_PRIMITIVE_RESOLVER_ROOT_BYTES} bytes"
        ));
    }
    roots_material.sort();
    let digest = AppDigest::blake3(roots_material.join("\0").as_bytes());
    AppReference::parse(format!(
        "primitive-owner:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedSkillKind {
    Tool,
    Procedure,
    Personality,
}

struct ClassifiedSkill {
    class: ClassifiedSkillKind,
    frontmatter: CatalogFrontmatter,
    source: Vec<u8>,
    source_directory: PathBuf,
}

struct ClassifiedSkillScan {
    items: Vec<ClassifiedSkill>,
    invalid_sources: Vec<Vec<u8>>,
}

#[derive(Deserialize)]
struct CatalogAgentIdentity {
    agent_id: String,
}

fn is_default_agent(source: &[u8]) -> bool {
    if super::manifest::preflight_agent_definition_yaml(
        source,
        &super::manifest::AppPackageLimits::for_bounded_agent_definition(256 * 1024),
    )
    .is_err()
    {
        return false;
    }
    matches!(
        serde_yaml::from_slice::<CatalogAgentIdentity>(source),
        Ok(identity)
            if crate::magician_v2::agents::storage::validate_agent_identifier(&identity.agent_id)
                .is_ok()
                && identity.agent_id == DEFAULT_APP_WORKFLOW_AGENT
    )
}

fn scan_classified_skill_dirs(skill_dirs: &[PathBuf]) -> Result<ClassifiedSkillScan, String> {
    let paths = scan_skill_document_paths(skill_dirs)?;
    let mut items = Vec::new();
    let mut invalid_sources = Vec::new();
    let mut source_bytes = 0usize;
    for path in paths {
        let source = read_skill_document(&path).map_err(|error| error.to_string())?;
        let resolved_marker = crate::magician_v2::skills::path_rewrite::resolve_skill_path(&path);
        let canonical_marker = fs::canonicalize(&resolved_marker)
            .map_err(|error| format!("resolving skill marker {}: {error}", path.display()))?;
        let marker_metadata = fs::symlink_metadata(&canonical_marker).map_err(|error| {
            format!(
                "reading resolved skill marker {}: {error}",
                canonical_marker.display()
            )
        })?;
        if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
            return Err(format!(
                "resolved skill marker is not a regular file: {}",
                canonical_marker.display()
            ));
        }
        let source_directory = canonical_marker
            .parent()
            .ok_or_else(|| {
                format!(
                    "resolved skill marker has no parent: {}",
                    canonical_marker.display()
                )
            })?
            .to_path_buf();
        source_bytes = source_bytes
            .checked_add(source.len())
            .filter(|total| *total <= MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES)
            .ok_or_else(|| {
                format!(
                    "skill source scan exceeds {} bytes",
                    MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES
                )
            })?;
        let Ok(text) = std::str::from_utf8(&source) else {
            invalid_sources.push(source);
            continue;
        };
        let skill_type = match parse_skill_magician_extension::<String>(text, "skill_type") {
            Ok(skill_type) => skill_type,
            Err(_) => {
                invalid_sources.push(source);
                continue;
            },
        };
        // App package manifests are owned by the package registry, not the
        // primitive skill scanner.
        if skill_type.as_deref() == Some("app") {
            continue;
        }
        let Some(parent) = path.parent() else {
            invalid_sources.push(source);
            continue;
        };
        let manifest = match parse_skill_manifest(text, parent) {
            Ok(manifest) => manifest,
            Err(_) => {
                invalid_sources.push(source);
                continue;
            },
        };
        let frontmatter = match parse_skill_frontmatter::<CatalogFrontmatter>(text) {
            Ok(frontmatter) => frontmatter,
            Err(_) => {
                invalid_sources.push(source);
                continue;
            },
        };
        if let Some(class) = classify_skill(text, manifest.inferred_kind()) {
            items.push(ClassifiedSkill {
                class,
                frontmatter,
                source,
                source_directory,
            });
        }
    }
    Ok(ClassifiedSkillScan {
        items,
        invalid_sources,
    })
}

fn skill_discovery_key(path: &Path) -> Option<String> {
    path.parent()?
        .file_name()?
        .to_str()
        .map(super::manifest::normalized_collision_key)
}

/// Resolver-owned enumeration mirrors SkillLoader's ordered, whole-folder
/// shadowing while making every scan failure explicit and bounded. A malformed
/// higher-precedence source still claims its name, so a lower fallback cannot
/// silently reactivate.
fn scan_skill_document_paths(skill_dirs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let mut seen_paths = BTreeSet::new();
    let mut claimed_names = BTreeSet::new();
    let mut entries_seen = 0usize;
    let mut declared_regular_bytes = 0usize;
    for root in skill_dirs {
        let root_metadata = fs::symlink_metadata(root)
            .map_err(|error| format!("reading skill root {}: {error}", root.display()))?;
        if !root_metadata.file_type().is_dir() {
            return Err(format!("skill root is not a directory: {}", root.display()));
        }
        let direct_marker = root.join("SKILL.md");
        match fs::symlink_metadata(&direct_marker) {
            Ok(_) => {
                register_skill_marker(
                    &mut paths,
                    &mut seen_paths,
                    &mut claimed_names,
                    &mut declared_regular_bytes,
                    direct_marker,
                )?;
                continue;
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(format!(
                    "reading skill marker {}: {error}",
                    direct_marker.display()
                ));
            },
        }
        let entries = fs::read_dir(root)
            .map_err(|error| format!("reading skill root {}: {error}", root.display()))?;
        let mut directories = Vec::new();
        for entry in entries {
            entries_seen = entries_seen
                .checked_add(1)
                .filter(|count| *count <= MAX_APP_PRIMITIVES)
                .ok_or_else(|| {
                    format!("skill discovery exceeds {MAX_APP_PRIMITIVES} directory entries")
                })?;
            let entry = entry.map_err(|error| {
                format!(
                    "reading skill directory entry in {}: {error}",
                    root.display()
                )
            })?;
            if !entry
                .file_type()
                .map_err(|error| format!("reading entry type {}: {error}", entry.path().display()))?
                .is_dir()
            {
                continue;
            }
            directories.push(entry.path());
        }
        directories.sort();
        let mut root_claims = BTreeMap::new();
        for directory in directories {
            let marker = directory.join("SKILL.md");
            match fs::symlink_metadata(&marker) {
                Ok(_) => {
                    let claim = skill_discovery_key(&marker).ok_or_else(|| {
                        format!("skill marker has no named parent: {}", marker.display())
                    })?;
                    if let Some(existing) = root_claims.insert(claim.clone(), marker.clone()) {
                        if existing != marker {
                            return Err(format!(
                                "same-root skill alias `{claim}` is claimed by {} and {}",
                                existing.display(),
                                marker.display()
                            ));
                        }
                    }
                    register_skill_marker(
                        &mut paths,
                        &mut seen_paths,
                        &mut claimed_names,
                        &mut declared_regular_bytes,
                        marker,
                    )?
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => {
                    return Err(format!(
                        "reading skill marker {}: {error}",
                        marker.display()
                    ));
                },
            }
        }
    }
    if paths.len() > MAX_APP_PRIMITIVES {
        return Err(format!(
            "skill discovery exceeds {MAX_APP_PRIMITIVES} skill documents"
        ));
    }
    Ok(paths)
}

fn register_skill_marker(
    paths: &mut Vec<PathBuf>,
    seen_paths: &mut BTreeSet<PathBuf>,
    claimed_names: &mut BTreeSet<String>,
    declared_regular_bytes: &mut usize,
    marker: PathBuf,
) -> Result<(), String> {
    let marker_metadata = fs::symlink_metadata(&marker)
        .map_err(|error| format!("reading skill marker {}: {error}", marker.display()))?;
    // Governed live installations intentionally use SKILL.md symlinks into
    // skillshub. The existing bounded reader owns safe path rewriting and the
    // per-document byte ceiling; other filesystem object kinds are rejected.
    if !marker_metadata.file_type().is_file() && !marker_metadata.file_type().is_symlink() {
        return Err(format!(
            "skill marker is not a regular file: {}",
            marker.display()
        ));
    }
    let claim = skill_discovery_key(&marker)
        .ok_or_else(|| format!("skill marker has no named parent: {}", marker.display()))?;
    if seen_paths.insert(marker.clone()) && claimed_names.insert(claim) {
        if marker_metadata.file_type().is_file() {
            *declared_regular_bytes = declared_regular_bytes
                .checked_add(marker_metadata.len() as usize)
                .filter(|total| *total <= MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES)
                .ok_or_else(|| {
                    format!(
                        "skill source scan exceeds {} bytes",
                        MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES
                    )
                })?;
        }
        paths.push(marker);
    }
    Ok(())
}

fn classify_skill(source: &str, inferred: InferredKind) -> Option<ClassifiedSkillKind> {
    if inferred == InferredKind::PersonalityMode
        || parse_skill_magician_extension::<serde_json::Value>(source, "personality")
            .ok()
            .flatten()
            .is_some()
    {
        return Some(ClassifiedSkillKind::Personality);
    }
    let skill_type = parse_skill_magician_extension::<String>(source, "skill_type")
        .ok()
        .flatten();
    match skill_type.as_deref() {
        Some("app") => None,
        Some("procedure") => Some(ClassifiedSkillKind::Procedure),
        Some("tool") => Some(ClassifiedSkillKind::Tool),
        _ if parse_skill_runtime_package(source).ok().flatten().is_some() => {
            Some(ClassifiedSkillKind::Tool)
        },
        _ => Some(ClassifiedSkillKind::Procedure),
    }
}

fn tool_yaml_declaration(
    name: &str,
    version: Option<&str>,
    primitive_ref: Option<&AppReference>,
    action: Option<&str>,
) -> String {
    let exact = primitive_ref
        .map(|reference| format!("\n  primitive_ref: {reference}"))
        .unwrap_or_default();
    let action = action
        .map(|action| format!("\n  actions: [{action}]"))
        .unwrap_or_default();
    format!(
        "- name: {name}{exact}{action}\n  version_requirement: \"{}\"",
        caret_major(version.unwrap_or("1"))
    )
}

fn tool_yaml_snippet(entry: &AuthoringToolEntry) -> String {
    format!(
        "dependencies:\n  tools:\n    {}",
        entry.yaml_declaration.replace('\n', "\n    ")
    )
}

fn procedure_yaml_declaration(name: &str, version: Option<&str>) -> String {
    format!(
        "- skill: skill:{name}\n  version_requirement: \"{}\"",
        caret_major(version.unwrap_or("1"))
    )
}

fn caret_major(version: &str) -> String {
    let major = version
        .split('.')
        .next()
        .unwrap_or("1")
        .trim_start_matches('v');
    if !major.is_empty() && major.chars().all(|byte| byte.is_ascii_digit()) {
        format!("^{major}")
    } else {
        "^1".to_string()
    }
}

struct ScannedAgentSource {
    source: Vec<u8>,
    identity_valid: bool,
}

fn scan_agent_template_sources(roots: &[PathBuf]) -> Result<Vec<ScannedAgentSource>, String> {
    let mut definition_paths = Vec::new();
    let mut claimed_agent_ids = BTreeSet::new();
    let mut entries_seen = 0usize;
    let mut declared_source_bytes = 0usize;
    for root in roots {
        let metadata = fs::symlink_metadata(root)
            .map_err(|error| format!("reading agent template root {}: {error}", root.display()))?;
        if !metadata.file_type().is_dir() {
            return Err(format!(
                "agent template root is not a directory: {}",
                root.display()
            ));
        }
        let mut root_claims = BTreeMap::<String, PathBuf>::new();
        let mut root_seen_paths = BTreeSet::new();
        for agents_root in [root.join("agents"), root.to_path_buf()] {
            let children = match fs::read_dir(&agents_root) {
                Ok(children) => children,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            let mut directories = Vec::new();
            for child in children {
                entries_seen = entries_seen
                    .checked_add(1)
                    .filter(|count| *count <= MAX_APP_PRIMITIVES)
                    .ok_or_else(|| {
                        format!("agent discovery exceeds {MAX_APP_PRIMITIVES} directory entries")
                    })?;
                let child = child.map_err(|error| error.to_string())?;
                if child
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    directories.push(child.path());
                }
            }
            directories.sort();
            for path in directories {
                let definition = path.join("definition.agent.yaml");
                let metadata = match fs::symlink_metadata(&definition) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                let claim = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(super::manifest::normalized_collision_key)
                    .ok_or_else(|| format!("invalid agent directory {}", path.display()))?;
                if !root_seen_paths.insert(definition.clone()) {
                    continue;
                }
                if let Some(existing) = root_claims.insert(claim.clone(), definition.clone()) {
                    if existing != definition {
                        return Err(format!(
                            "same-root agent alias `{claim}` is claimed by {} and {}",
                            existing.display(),
                            definition.display()
                        ));
                    }
                }
                if !metadata.file_type().is_file() || metadata.len() > 256 * 1024 {
                    return Err(format!(
                        "invalid bounded agent definition {}",
                        definition.display()
                    ));
                }
            }
        }
        for (claim, definition) in root_claims {
            if !claimed_agent_ids.insert(claim.clone()) {
                continue;
            }
            let metadata = fs::symlink_metadata(&definition).map_err(|error| error.to_string())?;
            declared_source_bytes = declared_source_bytes
                .checked_add(metadata.len() as usize)
                .filter(|total| *total <= MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES)
                .ok_or_else(|| {
                    format!(
                        "agent source scan exceeds {} bytes",
                        MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES
                    )
                })?;
            definition_paths.push((claim, definition));
        }
    }
    if definition_paths.len() > MAX_APP_PRIMITIVES {
        return Err(format!(
            "agent discovery exceeds {MAX_APP_PRIMITIVES} definitions"
        ));
    }
    let mut sources = Vec::with_capacity(definition_paths.len());
    let mut source_bytes = 0usize;
    for (claim, definition) in definition_paths {
        let bytes = fs::read(&definition).map_err(|error| error.to_string())?;
        if bytes.is_empty() || bytes.len() > 256 * 1024 {
            return Err(format!(
                "invalid bounded agent definition {}",
                definition.display()
            ));
        }
        source_bytes = source_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES)
            .ok_or_else(|| {
                format!(
                    "agent source scan exceeds {} bytes",
                    MAX_PRIMITIVE_SOURCE_MATERIAL_BYTES
                )
            })?;
        let identity = super::manifest::preflight_agent_definition_yaml(
            &bytes,
            &super::manifest::AppPackageLimits::for_bounded_agent_definition(256 * 1024),
        )
        .ok()
        .and_then(|()| serde_yaml::from_slice::<CatalogAgentIdentity>(&bytes).ok())
        .filter(|identity| {
            crate::magician_v2::agents::storage::validate_agent_identifier(&identity.agent_id)
                .is_ok()
                && super::manifest::normalized_collision_key(&identity.agent_id) == claim
        });
        if identity.is_none()
            && claim == super::manifest::normalized_collision_key(DEFAULT_APP_WORKFLOW_AGENT)
        {
            return Err("the reserved default agent definition is invalid".to_owned());
        }
        sources.push(ScannedAgentSource {
            source: bytes,
            identity_valid: identity.is_some(),
        });
    }
    Ok(sources)
}

fn read_skill_document(path: &Path) -> Result<Vec<u8>, AuthoringCatalogError> {
    crate::magician_v2::skills::embedded_extensions::read_bounded_skill_markdown(path)
        .map(|text| text.into_bytes())
        .map_err(|error| AuthoringCatalogError::InvalidFilter(error.to_string()))
}

fn unique_existing_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        push_unique_dir(&mut out, dir);
    }
    out
}

fn unique_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        if !out.iter().any(|existing| existing == &dir) {
            out.push(dir);
        }
    }
    out
}

fn append_unique_dirs(dirs: &mut Vec<PathBuf>, additional: impl IntoIterator<Item = PathBuf>) {
    for dir in additional {
        if !dirs.contains(&dir) {
            // Configured runtime extras are required inputs. Preserve missing
            // or unreadable paths so strict discovery reports unavailable.
            dirs.push(dir);
        }
    }
}

fn push_unique_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if matches!(
        fs::symlink_metadata(&dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        // Optional live-scope directories commonly do not exist yet.
        return;
    }
    if dirs.iter().any(|existing| existing == &dir) {
        return;
    }
    // Preserve every other state (including unreadable/non-directory/symlink)
    // so the strict scanner, rather than this constructor, decides validity.
    dirs.push(dir);
}

#[cfg(test)]
mod tests {

    /// Every agent definition this repo ships must pass the app platform's
    /// preflight.
    ///
    /// This is the pin for the defect that made the whole platform inoperable:
    /// the `personal-assistant` persona is a little over 6 KiB, the app-manifest
    /// string limit is 4 KiB, and applying the manifest limit to an agent
    /// definition made the reserved default agent invalid. That failed the agent
    /// catalog scan, which marked the primitive catalog unavailable, which made
    /// every app capability unresolvable — so no package could be admitted,
    /// reviewed or approved on any deployment that had the default agent
    /// seeded, which is all of them.
    ///
    /// A too-tight limit here does not fail loudly at the limit; it fails as
    /// "capability X no longer matches its locked primitive descriptor" several
    /// layers away. That distance is why this needs a pin rather than care.
    #[test]
    fn every_shipped_agent_definition_passes_preflight() {
        let templates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician_data_v3/system/agent_templates/agents");
        let entries = std::fs::read_dir(&templates).expect("shipped agent templates");
        let mut checked = 0usize;
        for entry in entries.filter_map(Result::ok) {
            let definition = entry.path().join("definition.agent.yaml");
            if !definition.is_file() {
                continue;
            }
            let bytes = std::fs::read(&definition).expect("agent definition is readable");
            super::super::manifest::preflight_agent_definition_yaml(
                &bytes,
                &super::super::manifest::AppPackageLimits::for_bounded_agent_definition(256 * 1024),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "shipped agent definition {} fails preflight: {error}",
                    definition.display()
                )
            });
            checked += 1;
        }
        assert!(
            checked > 0,
            "expected shipped agent definitions under {}",
            templates.display()
        );
    }

    use super::*;
    use crate::magician_v2::apps::tool_eligibility::typed_app_tool_document;

    fn write_skill(parent: &Path, name: &str, body: &str) {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn write_agent(parent: &Path, agent_id: &str, name: &str, description: &str) {
        let dir = parent.join("agents").join(agent_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("definition.agent.yaml"),
            format!("agent_id: {agent_id}\nname: {name}\ndescription: \"{description}\"\n"),
        )
        .unwrap();
    }

    fn write_agent_source(parent: &Path, agent_id: &str, source: &str) {
        let dir = parent.join("agents").join(agent_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("definition.agent.yaml"), source).unwrap();
    }

    fn write_callable_agent(parent: &Path, agent_id: &str) {
        let dir = parent.join("agents").join(agent_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("definition.agent.yaml"),
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
            ),
        )
        .unwrap();
    }

    #[test]
    fn compiled_catalog_marks_memory_and_host_tools_eligible() {
        let list = list_authoring_tools(
            &AuthoringDiscoveryRoots::default(),
            AuthoringToolListFilter {
                app_eligible_only: false,
                kind: Some(AuthoringToolKind::Compiled),
            },
        );
        let read = list
            .items
            .iter()
            .find(|entry| entry.name == "content_read")
            .expect("content_read");
        assert!(read.app_eligible);
        assert_eq!(read.kind, "compiled");
        assert!(read.yaml_declaration.contains("name: content_read"));

        let files = list
            .items
            .iter()
            .find(|entry| entry.name == "files")
            .expect("files");
        assert!(files.app_eligible);
        assert!(files.dispatchable);

        let time_math = list
            .items
            .iter()
            .find(|entry| entry.name == "time_math")
            .expect("time_math");
        assert!(time_math.dispatchable);
        assert!(time_math
            .dispatch_note
            .as_deref()
            .is_some_and(|note| note.contains("date_range") && note.contains("now")));

        let eligible_only = list_authoring_tools(
            &AuthoringDiscoveryRoots::default(),
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: Some(AuthoringToolKind::Compiled),
            },
        );
        assert!(eligible_only.items.iter().all(|entry| entry.app_eligible));
        assert!(eligible_only
            .items
            .iter()
            .any(|entry| entry.name == "search_memory"));
        assert!(eligible_only.items.iter().any(|entry| entry.name == "http"));
        assert!(eligible_only
            .items
            .iter()
            .any(|entry| entry.name == "macos_automation"));
    }

    #[test]
    fn skill_dir_classifies_tools_procedures_and_personalities() {
        let temp = tempfile::tempdir().unwrap();
        write_skill(
            temp.path(),
            "next-step",
            std::str::from_utf8(&typed_app_tool_document(
                "next-step",
                "1.4.0",
                "    expose:\n      apps: true\n",
            ))
            .unwrap(),
        );
        write_skill(
            temp.path(),
            "hidden-step",
            std::str::from_utf8(&typed_app_tool_document(
                "hidden-step",
                "1.0.0",
                "    expose:\n      apps: false\n",
            ))
            .unwrap(),
        );
        write_skill(
            temp.path(),
            "external-summary",
            "---\nname: external-summary\nversion: 1.2.0\ndescription: Summarize reviewed \
             input.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nProduce one concise \
             summary.\n",
        );
        write_skill(
            temp.path(),
            "brutal",
            "---\nname: brutal\ndescription: Direct voice.\nmetadata:\n  magician:\n    \
             personality:\n      active_mode: brutal\n      voice: blunt\n---\nBe direct.\n",
        );

        let roots = AuthoringDiscoveryRoots {
            skill_dirs: vec![temp.path().to_path_buf()],
            agent_template_dirs: Vec::new(),
        };
        let tools = list_authoring_tools(&roots, AuthoringToolListFilter::default());
        let next = tools
            .items
            .iter()
            .find(|entry| entry.name == "next-step")
            .expect("next-step");
        assert!(next.app_eligible);
        assert_eq!(next.kind, "skill");
        assert_eq!(next.version.as_deref(), Some("1.4.0"));
        // A CLI skill cannot dispatch in apps until OS-jail contain lands.
        // Its note names that blocker, not the cause-free aggregate.
        assert!(!next.dispatchable);
        let note = next.dispatch_note.as_deref().unwrap_or("");
        assert!(note.contains("OS-jail"), "{note}");
        assert!(!note.contains("typed input contract"), "{note}");
        let shown = show_authoring_tool(&roots, next.primitive_id.as_str())
            .expect("exact skill descriptor");
        assert_eq!(shown.action_descriptors.len(), 1);
        assert!(shown.action_descriptors[0]
            .action_id
            .as_str()
            .starts_with("primitive-action:tool-skill:"));
        assert!(shown.action_descriptors[0].input_schema_digest.is_some());
        let hidden = tools
            .items
            .iter()
            .find(|entry| entry.name == "hidden-step")
            .expect("hidden-step");
        assert!(!hidden.app_eligible);
        assert!(hidden
            .ineligible_reason
            .as_deref()
            .unwrap_or("")
            .contains("expose.apps"));

        let eligible = list_authoring_tools(
            &roots,
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: Some(AuthoringToolKind::Skill),
            },
        );
        assert_eq!(eligible.count, 1);
        assert_eq!(eligible.items[0].name, "next-step");

        let procedures = list_authoring_procedures(&roots);
        assert_eq!(procedures.count, 1);
        assert_eq!(procedures.items[0].name, "external-summary");
        assert!(procedures.items[0]
            .yaml_declaration
            .contains("skill: skill:external-summary"));

        let personalities = list_authoring_personalities(&roots);
        assert_eq!(personalities.count, 1);
        assert_eq!(personalities.items[0].name, "brutal");
        assert_eq!(
            personalities.items[0].yaml_declaration,
            "personality: brutal"
        );
    }

    #[test]
    fn show_reports_yaml_snippet_and_unknown_names_fail() {
        let shown = show_authoring_tool(&AuthoringDiscoveryRoots::default(), "content_read")
            .expect("compiled show");
        assert_eq!(shown.tool.name, "content_read");
        assert!(shown.tool.app_eligible);
        assert!(shown.yaml_snippet.contains("dependencies:"));
        assert!(shown.yaml_snippet.contains("name: content_read"));
        assert!(show_authoring_tool(&AuthoringDiscoveryRoots::default(), "not-a-tool").is_err());
    }

    #[test]
    fn public_authoring_projects_only_the_sealed_agent_and_browser_roster() {
        let temp = tempfile::tempdir().unwrap();
        let skills = temp.path().join("skills");
        let agents = temp.path().join("agents-root");
        let browser = String::from_utf8(typed_app_tool_document("browser", "1.0.0", ""))
            .expect("typed fixture is UTF-8")
            .replace(
                "---\nReturn one ranked next step.",
                "    runtime_catalog:\n      categories: [browser]\n      composition_category: \
                 web_operations\n---\nReturn one ranked next step.",
            );
        write_skill(&skills, "browser", &browser);
        write_callable_agent(&agents, "reviewed-child");
        write_agent(&agents, "untyped-child", "Untyped", "No callable contract.");
        let roots = AuthoringDiscoveryRoots {
            skill_dirs: vec![skills],
            agent_template_dirs: vec![agents],
        };

        let tools = list_authoring_tools(
            &roots,
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: None,
            },
        );
        let agent = tools
            .items
            .iter()
            .find(|entry| entry.name == "reviewed-child")
            .expect("sealed agent leaf is authorable");
        assert_eq!(agent.kind, "agent");
        assert!(agent.dispatchable);
        assert!(agent.yaml_declaration.contains("actions: [agent_as_tool]"));
        assert!(tools
            .items
            .iter()
            .all(|entry| entry.name != "untyped-child"));
        assert!(tools
            .items
            .iter()
            .all(|entry| entry.name != DEFAULT_APP_WORKFLOW_AGENT));

        let browser = tools
            .items
            .iter()
            .find(|entry| entry.name == "browser")
            .expect("sealed Browser roster is authorable");
        assert_eq!(browser.kind, "interactive");
        assert!(browser.dispatchable);
        assert!(browser.yaml_declaration.contains("actions: [snapshot]"));
        let shown = show_authoring_tool(&roots, "browser").expect("browser show");
        assert_eq!(
            shown.actions,
            vec!["snapshot", "navigate", "scroll", "click"]
        );
        assert!(shown
            .actions
            .iter()
            .all(|action| !matches!(action.as_str(), "eval" | "raw_call" | "download")));
    }

    #[test]
    fn browser_friendly_name_never_projects_an_ordinary_skill_as_browser_authority() {
        let temp = tempfile::tempdir().unwrap();
        write_skill(
            temp.path(),
            "browser",
            std::str::from_utf8(&typed_app_tool_document("browser", "1.0.0", "")).unwrap(),
        );
        let roots = AuthoringDiscoveryRoots {
            skill_dirs: vec![temp.path().to_path_buf()],
            agent_template_dirs: Vec::new(),
        };
        let tools = list_authoring_tools(&roots, AuthoringToolListFilter::default());
        assert!(tools.items.iter().all(|entry| entry.name != "browser"));
        assert!(show_authoring_tool(&roots, "browser").is_err());
    }

    #[test]
    fn agents_always_include_default_runner_and_scan_templates() {
        let temp = tempfile::tempdir().unwrap();
        write_agent(temp.path(), "research-agent", "Research", "Hires research.");
        let roots = AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![temp.path().to_path_buf()],
        };
        let agents = list_authoring_agents(&roots);
        assert!(agents
            .items
            .iter()
            .any(|entry| entry.name == DEFAULT_APP_WORKFLOW_AGENT && entry.default_runner));
        let hired = agents
            .items
            .iter()
            .find(|entry| entry.name == "research-agent")
            .expect("research-agent");
        assert_eq!(hired.yaml_declaration, "agent: research-agent");
        assert_eq!(hired.display_name.as_deref(), Some("Research"));
    }

    #[test]
    fn real_default_agent_replaces_synthetic_and_higher_root_shadows_fallback() {
        let first = tempfile::tempdir().unwrap();
        write_agent(
            first.path(),
            DEFAULT_APP_WORKFLOW_AGENT,
            "Reviewed Personal Assistant",
            "Reviewed default definition.",
        );
        let roots = AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![first.path().to_path_buf()],
        };
        let agents = list_authoring_agents(&roots);
        let defaults = agents
            .items
            .iter()
            .filter(|agent| agent.name == DEFAULT_APP_WORKFLOW_AGENT)
            .collect::<Vec<_>>();
        assert_eq!(defaults.len(), 1);
        assert!(defaults[0].default_runner);
        assert_eq!(defaults[0].description, "Reviewed default definition.");

        let second = tempfile::tempdir().unwrap();
        write_agent(
            second.path(),
            DEFAULT_APP_WORKFLOW_AGENT,
            "Conflicting Personal Assistant",
            "Different default bytes.",
        );
        let precedence = list_authoring_agents(&AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![first.path().to_path_buf(), second.path().to_path_buf()],
        });
        assert_eq!(precedence.status, "ok");
        let default = precedence
            .items
            .iter()
            .find(|agent| agent.name == DEFAULT_APP_WORKFLOW_AGENT)
            .expect("precedence-winning default");
        assert_eq!(default.description, "Reviewed default definition.");
    }

    #[test]
    fn invalid_claimed_default_agent_makes_catalog_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("agents").join(DEFAULT_APP_WORKFLOW_AGENT);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("definition.agent.yaml"),
            format!(
                "agent_id: {DEFAULT_APP_WORKFLOW_AGENT}\nname: Invalid default\ntools:\n{}",
                (0..257)
                    .map(|index| format!("  - tool-{index}\n"))
                    .collect::<String>()
            ),
        )
        .unwrap();
        let agents = list_authoring_agents(&AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![temp.path().to_path_buf()],
        });
        assert_eq!(agents.status, "unavailable");
        assert!(agents.items.is_empty());
    }

    #[test]
    fn established_agent_tags_are_admitted_without_allowing_arbitrary_yaml_tags_or_aliases() {
        let valid = tempfile::tempdir().unwrap();
        write_agent_source(
            valid.path(),
            DEFAULT_APP_WORKFLOW_AGENT,
            &format!(
                "agent_id: {DEFAULT_APP_WORKFLOW_AGENT}\nname: Atlas\ndescription: Atlas's tagged live default with cron 0 0 31 2 *\npersona: >-\n  Tags such as !shell and aliases such as *shared are inert inside this text.\nretention: !days 30\nstrategy: !fixed atomic\n"
            ),
        );
        let agents = list_authoring_agents(&AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![valid.path().to_path_buf()],
        });
        assert_eq!(agents.status, "ok");
        assert!(agents
            .items
            .iter()
            .any(|agent| agent.name == DEFAULT_APP_WORKFLOW_AGENT
                && agent.description == "Atlas's tagged live default with cron 0 0 31 2 *"));

        for hostile in [
            format!("agent_id: {DEFAULT_APP_WORKFLOW_AGENT}\nname: Presto\nretention: !shell 30\n"),
            format!(
                "agent_id: {DEFAULT_APP_WORKFLOW_AGENT}\nname: &shared Presto\nalias: *shared\n"
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            write_agent_source(root.path(), DEFAULT_APP_WORKFLOW_AGENT, &hostile);
            let agents = list_authoring_agents(&AuthoringDiscoveryRoots {
                skill_dirs: Vec::new(),
                agent_template_dirs: vec![root.path().to_path_buf()],
            });
            assert_eq!(agents.status, "unavailable");
            assert!(agents.items.is_empty());
        }
    }

    #[test]
    fn same_root_normalized_agent_alias_collision_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        write_agent(temp.path(), "Foo", "First", "first");
        write_agent(temp.path(), "Ｆoo", "Second", "second");
        let agents = list_authoring_agents(&AuthoringDiscoveryRoots {
            skill_dirs: Vec::new(),
            agent_template_dirs: vec![temp.path().to_path_buf()],
        });
        assert_eq!(agents.status, "unavailable");
    }

    #[test]
    fn platform_name_is_reserved_and_colliding_skill_keeps_exact_identity() {
        let temp = tempfile::tempdir().unwrap();
        write_skill(
            temp.path(),
            "content_read",
            std::str::from_utf8(&typed_app_tool_document(
                "content_read",
                "9.0.0",
                "    expose:\n      apps: true\n",
            ))
            .unwrap(),
        );
        let roots = AuthoringDiscoveryRoots {
            skill_dirs: vec![temp.path().to_path_buf()],
            agent_template_dirs: Vec::new(),
        };
        let shown = show_authoring_tool(&roots, "content_read").expect("reserved platform tool");
        assert_eq!(shown.tool.kind, "compiled");
        assert!(shown
            .tool
            .collision
            .as_ref()
            .is_some_and(|collision| collision.reserved_platform_name));
        let list = list_authoring_tools(&roots, AuthoringToolListFilter::default());
        let private = list
            .items
            .iter()
            .find(|entry| entry.name == "content_read" && entry.kind == "skill")
            .expect("colliding skill remains catalogued");
        assert!(!private.alias_available);
        assert!(private
            .yaml_declaration
            .contains("primitive_ref: primitive:tool-skill:"));
        let exact = show_authoring_tool(&roots, private.primitive_id.as_str())
            .expect("qualified private identity");
        assert_eq!(exact.tool.kind, "skill");
        assert_eq!(exact.tool.version.as_deref(), Some("9.0.0"));
    }

    #[test]
    fn malformed_private_skill_is_visible_as_degraded_without_erasing_platform_tools() {
        let temp = tempfile::tempdir().unwrap();
        write_skill(temp.path(), "broken", "not valid frontmatter");
        let tools = list_authoring_tools(
            &AuthoringDiscoveryRoots {
                skill_dirs: vec![temp.path().to_path_buf()],
                agent_template_dirs: Vec::new(),
            },
            AuthoringToolListFilter::default(),
        );
        assert_eq!(tools.status, "degraded");
        assert!(tools.items.iter().any(|tool| tool.name == "content_read"));
    }

    #[test]
    fn same_root_normalized_skill_alias_collision_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        write_skill(
            temp.path(),
            "Foo",
            "---\nname: Foo\ndescription: first\n---\nrun\n",
        );
        write_skill(
            temp.path(),
            "Ｆoo",
            "---\nname: Ｆoo\ndescription: second\n---\nrun\n",
        );
        let tools = list_authoring_tools(
            &AuthoringDiscoveryRoots {
                skill_dirs: vec![temp.path().to_path_buf()],
                agent_template_dirs: Vec::new(),
            },
            AuthoringToolListFilter::default(),
        );
        assert_eq!(tools.status, "unavailable");
    }

    #[test]
    fn malformed_higher_skill_claim_does_not_reactivate_lower_fallback() {
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        write_skill(higher.path(), "shadowed", "not valid frontmatter");
        write_skill(
            lower.path(),
            "shadowed",
            std::str::from_utf8(&typed_app_tool_document("shadowed", "1.0.0", "")).unwrap(),
        );
        let tools = list_authoring_tools(
            &AuthoringDiscoveryRoots {
                skill_dirs: vec![higher.path().to_path_buf(), lower.path().to_path_buf()],
                agent_template_dirs: Vec::new(),
            },
            AuthoringToolListFilter::default(),
        );
        assert_eq!(tools.status, "degraded");
        assert!(tools.items.iter().all(|tool| tool.name != "shadowed"));
    }

    #[test]
    fn invalid_explicit_root_fails_tool_and_personality_catalogs_closed() {
        let temp = tempfile::tempdir().unwrap();
        let not_a_root = temp.path().join("not-a-root");
        fs::write(&not_a_root, "file").unwrap();
        let roots = AuthoringDiscoveryRoots::from_explicit([not_a_root], Vec::<PathBuf>::new());
        assert_eq!(
            list_authoring_tools(&roots, AuthoringToolListFilter::default()).status,
            "unavailable"
        );
        assert_eq!(list_authoring_personalities(&roots).status, "unavailable");
    }

    #[test]
    fn live_scope_searches_scoped_skills_not_repo_skillshub() {
        let root = tempfile::tempdir().unwrap();
        write_skill(
            &root.path().join("scopes/anonymous/default/skills"),
            "youtube-search",
            std::str::from_utf8(&typed_app_tool_document("youtube-search", "0.3.0", "")).unwrap(),
        );
        write_skill(
            &root.path().join("skillshub"),
            "should-not-appear",
            std::str::from_utf8(&typed_app_tool_document("should-not-appear", "1.0.0", ""))
                .unwrap(),
        );
        let roots = AuthoringDiscoveryRoots::for_scope(root.path(), "anonymous", "default");
        let tools = list_authoring_tools(
            &roots,
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: Some(AuthoringToolKind::Skill),
            },
        );
        assert!(tools
            .items
            .iter()
            .any(|entry| entry.name == "youtube-search"));
        assert!(tools
            .items
            .iter()
            .all(|entry| entry.name != "should-not-appear"));
    }
}
