//! HTTP install endpoint for AgentSkills v1 skills.
//!
//! Operators can POST a skill source + target to
//! `/api/magician/v2/skills/install` to land it in the runtime tree.
//! After install, magician's `SkillLoader` picks the skill up at the
//! next session.
//!
//! # Source schemes
//!
//! - `path:<absolute-path>` — local folder containing a `SKILL.md`.
//! - `skillshub:<name>` — install a skill that's already authored under
//!   this monorepo's `skillshub/<name>/`. Equivalent to a single-skill
//!   scoped installation.
//!
//! Git URL support (`git:<url>`) is intentionally NOT in 5b-2; it
//! requires extra hardening (clone timeout, ref pinning, signature
//! verification) and will land alongside the Forge UI in 5b-3.
//!
//! # Targets
//!
//! - `target.workspaces: ["principal/workspace", …]` →
//!   `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/`
//!
//! The legacy `target.system: true` install tier has been retired —
//! skills are workspace-scoped only. At least one workspace entry is
//! required.
//!
//! # Atomicity
//!
//! Each install is "all or nothing" *per target* — the source is copied
//! to a temp name (`<name>.installing-<ts>`) inside the target's parent
//! and then `rename`d into place. If the rename fails, the temp dir is
//! removed.
//!
//! # Validation
//!
//! Before copying, the source is loaded by the same `SkillLoader` the
//! runtime uses. Anything the loader rejects (missing frontmatter,
//! name/dir mismatch, …) causes a 422. This guarantees no broken skills
//! land in the runtime tree.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use magician::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::artifact_v2::{CapabilityScopePaths, CapabilityWorkspaceManager};
use magician::magician_v2::content_sources::ContentAcquisitionResolver;
use magician::magician_v2::execution::{embedded_compiled_pack_defs, ScopedCapabilityResolver};
use magician::magician_v2::secrets::mcp_oauth_vault::SecretStoreMcpOAuthVault;
use magician::magician_v2::skills::{InferredKind, SkillLoader};
use magician_components::setup::{self, ResolvedSetup, SetupBinding, SetupDriver};
use magician_mcp_client::{McpOAuthClientIdentity, McpOAuthCoordinator};
use tool_runtime_core::credential_profiles::{
    CanonicalCredentialUrl, CredentialProfileBinding, CredentialProfileKey, CredentialScope,
};
use tool_runtime_core::manifest::{
    AuthKind, AuthRequirement, AuthState, CliInteraction, McpTransport, ProfileSelection,
    RuntimeProtocol,
};

use crate::secret_vault_api::SecretVaultApi;

/// Shared state for the skills install endpoint.
#[derive(Clone)]
pub struct SkillsApi {
    workspace_layout: ArtifactV2Workspace,
    repo_root: PathBuf,
    scoped_capability_resolver: Arc<StdRwLock<Option<Arc<ScopedCapabilityResolver>>>>,
    content_acquisition_resolver: Arc<StdRwLock<Option<Arc<ContentAcquisitionResolver>>>>,
}

impl SkillsApi {
    pub fn new(base_root: PathBuf, repo_root: PathBuf) -> Self {
        Self::with_workspace_layout(
            ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(&base_root)),
            repo_root,
        )
    }

    pub fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
        repo_root: PathBuf,
    ) -> Self {
        Self {
            workspace_layout,
            repo_root,
            scoped_capability_resolver: Arc::new(StdRwLock::new(None)),
            content_acquisition_resolver: Arc::new(StdRwLock::new(None)),
        }
    }

    /// Bind the process-shared scoped resolver after startup has installed all
    /// compiled handlers and runtime services. Successful skill mutations can
    /// then evict the affected immutable catalog immediately instead of
    /// waiting for the content-revision guard on the next lookup.
    pub fn set_scoped_capability_resolver(&self, resolver: Arc<ScopedCapabilityResolver>) {
        *self
            .scoped_capability_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resolver);
    }

    pub fn set_content_acquisition_resolver(&self, resolver: Arc<ContentAcquisitionResolver>) {
        *self
            .content_acquisition_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resolver);
    }

    fn invalidate_capability_scope(&self, principal: &str, workspace: &str) {
        let content_resolver = self
            .content_acquisition_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(resolver) = content_resolver {
            resolver.invalidate_scope(principal, workspace);
            return;
        }
        let resolver = self
            .scoped_capability_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(resolver) = resolver {
            resolver.invalidate_scope(principal, workspace);
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    /// Source URI: `path:<abs>` or `skillshub:<name>`.
    pub source: String,
    /// At least one workspace target is required. The system-shared tier
    /// has been retired — install into specific scopes only.
    #[serde(default)]
    pub target: InstallTarget,
}

#[derive(Debug, Default, Deserialize)]
pub struct InstallTarget {
    /// Each entry is `principal/workspace`. Backwards-compat note: the
    /// pre-retirement schema also accepted `system: true`; that field
    /// is now silently ignored and rejected at handler entry.
    #[serde(default)]
    pub workspaces: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub installed: Vec<String>,
    pub kind: String,
    pub targets: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillListEntry {
    pub name: String,
    pub description: String,
    /// `procedure`, `personality-mode`, or `compiled`.
    ///
    /// `compiled` entries are the in-process Rust capability providers
    /// (`create_agent`, `treasurer`, `list_agents`, `notify_owner`, …)
    /// whose pack definitions ship bundled in the magician binary via
    /// `include_str!` (see `execution::embedded_compiled_pack_defs`).
    /// They appear in the catalog so operators can see them and use
    /// allow-for-agent on them, but they cannot be promoted, demoted,
    /// or installed — they're built into the binary.
    pub kind: String,
    /// `system`, `workspace`, or `built-in`. `built-in` is reserved
    /// for `kind: "compiled"` entries.
    pub layer: String,
    /// Required binaries declared via `metadata.magician.requires.bins`.
    pub requires_bins: Vec<String>,
    /// Required env vars declared via `metadata.magician.requires.env`.
    pub requires_env: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillListResponse {
    pub skills: Vec<SkillListEntry>,
}

/// `GET /api/magician/v2/skills`
///
/// Returns every skill installed in the system layer plus the bearer-bound workspace
/// layer. Workspace-layer entries shadow same-named system entries
/// (workspace-first, whole-folder shadowing per the SkillLoader
/// contract).
///
/// Listing is read-only, but workspace selection still comes only from the
/// request's verified bearer (or the local anonymous/default open-mode scope).
pub async fn list_skills_handler(req: HttpRequest, api: web::Data<SkillsApi>) -> HttpResponse {
    let workspace_dir = req
        .headers()
        .get("X-Principal")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|principal| {
            req.headers()
                .get("X-Workspace")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|workspace| api.workspace_layout.scope_skills_root(principal, workspace))
        });
    let extras_dirs = magician::magician_v2::config_extras::extra_skills_dirs();

    let mut search_paths: Vec<PathBuf> = Vec::new();
    if let Some(ref ws) = workspace_dir {
        search_paths.push(ws.clone());
    }
    for dir in &extras_dirs {
        search_paths.push(dir.clone());
    }

    let manifests = match SkillLoader::new(search_paths).discover() {
        Ok(m) => m,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("skill discovery failed: {e:#}"),
            )
        },
    };

    let mut entries: Vec<SkillListEntry> = manifests
        .into_iter()
        .map(|m| {
            let kind = match m.inferred_kind() {
                InferredKind::Procedure => "procedure",
                InferredKind::PersonalityMode => "personality-mode",
            }
            .to_string();
            // Layer is determined by which root the source_dir starts with.
            let layer = if let Some(ref ws) = workspace_dir {
                if m.source_dir.starts_with(ws) {
                    "workspace".to_string()
                } else {
                    "extras".to_string()
                }
            } else {
                "extras".to_string()
            };
            let (requires_bins, requires_env) = match &m.metadata.magician {
                Some(mag) => (mag.requires.bins.clone(), mag.requires.env.clone()),
                None => (Vec::new(), Vec::new()),
            };
            SkillListEntry {
                name: m.name,
                description: m.description,
                kind,
                layer,
                requires_bins,
                requires_env,
            }
        })
        .collect();

    // Append built-in compiled capabilities. These ship bundled in the
    // binary and have no SKILL.md — but they ARE callable tools the LLM
    // can pick, so they belong in the catalog. Skill folders win on
    // name collision.
    let already: std::collections::HashSet<&str> =
        entries.iter().map(|e| e.name.as_str()).collect();
    let mut compiled_entries: Vec<SkillListEntry> = embedded_compiled_pack_defs()
        .into_iter()
        .filter(|p| !already.contains(p.name.as_str()))
        .map(|p| SkillListEntry {
            name: p.name.clone(),
            description: p.description.clone().unwrap_or_default(),
            kind: "compiled".to_string(),
            layer: "built-in".to_string(),
            requires_bins: Vec::new(),
            requires_env: Vec::new(),
        })
        .collect();
    entries.append(&mut compiled_entries);

    HttpResponse::Ok().json(SkillListResponse { skills: entries })
}

/// `POST /api/magician/v2/skills/install`
///
/// Admin-privileged. Requires `X-Magician-Setup-Token` matching the
/// runtime vault's setup token; same admin pattern as
/// `/api/magician/v2/secrets/*`.
pub async fn install_skill_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    body: web::Json<InstallRequest>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    if body.target.workspaces.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "target.workspaces must specify at least one `principal/workspace` entry — the system-shared install tier has been retired",
        );
    }

    // Resolve source to a concrete folder on disk.
    let source_dir = match resolve_source(&body.source, &api.repo_root) {
        Ok(p) => p,
        Err(SourceError::Unsupported(scheme)) => {
            return error(
                actix_web::http::StatusCode::BAD_REQUEST,
                &format!(
                    "unsupported source scheme '{scheme}' — expected `path:<abs>` or `skillshub:<name>`"
                ),
            )
        },
        Err(SourceError::NotFound(p)) => {
            return error(
                actix_web::http::StatusCode::NOT_FOUND,
                &format!("source folder not found: {}", p.display()),
            )
        },
        Err(SourceError::NotADirectory(p)) => {
            return error(
                actix_web::http::StatusCode::BAD_REQUEST,
                &format!("source is not a directory: {}", p.display()),
            )
        },
        Err(SourceError::PathNotAbsolute) => {
            return error(
                actix_web::http::StatusCode::BAD_REQUEST,
                "path: scheme requires an absolute path",
            )
        },
    };

    // Validate via the same loader the runtime uses. The loader checks
    // frontmatter shape, name/dir match, etc. We ALSO require an
    // explicit SKILL.md at the root so a misconfigured `path:` of the
    // skillshub root (which contains many skill folders) fails fast.
    if !source_dir.join("SKILL.md").is_file() {
        return error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            &format!(
                "source has no SKILL.md at its root: {}",
                source_dir.display()
            ),
        );
    }
    let manifest = match validate_source(&source_dir) {
        Ok(m) => m,
        Err(reason) => return error(actix_web::http::StatusCode::UNPROCESSABLE_ENTITY, &reason),
    };
    let skill_name = manifest.name.clone();
    let kind = match manifest.inferred_kind() {
        InferredKind::Procedure => "procedure",
        InferredKind::PersonalityMode => "personality-mode",
    }
    .to_string();

    // Compute target paths and install one by one.
    let mut targets: Vec<String> = Vec::new();
    for entry in &body.target.workspaces {
        let (principal, workspace) = match parse_scope(entry) {
            Ok(pair) => pair,
            Err(reason) => return error(actix_web::http::StatusCode::BAD_REQUEST, &reason),
        };
        let dest = api
            .workspace_layout
            .scope_skills_root(&principal, &workspace)
            .join(&skill_name);
        if let Err(e) = install_atomic(&source_dir, &dest) {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("install to scope {principal}/{workspace} failed: {e}"),
            );
        }
        api.invalidate_capability_scope(&principal, &workspace);
        targets.push(format!(
            "workspace {principal}/{workspace}: {}",
            dest.display()
        ));
    }

    tracing::info!(
        skill = %skill_name,
        kind = %kind,
        targets = ?targets,
        source = %body.source,
        "skills_api: install succeeded"
    );

    HttpResponse::Ok().json(InstallResponse {
        installed: vec![skill_name],
        kind,
        targets,
    })
}

#[derive(Debug)]
enum SourceError {
    Unsupported(String),
    NotFound(PathBuf),
    NotADirectory(PathBuf),
    PathNotAbsolute,
}

fn resolve_source(source: &str, repo_root: &Path) -> Result<PathBuf, SourceError> {
    if let Some(rest) = source.strip_prefix("path:") {
        let p = PathBuf::from(rest);
        if !p.is_absolute() {
            return Err(SourceError::PathNotAbsolute);
        }
        if !p.exists() {
            return Err(SourceError::NotFound(p));
        }
        if !p.is_dir() {
            return Err(SourceError::NotADirectory(p));
        }
        Ok(p)
    } else if let Some(name) = source.strip_prefix("skillshub:") {
        let p = repo_root.join("skillshub").join(name);
        if !p.exists() {
            return Err(SourceError::NotFound(p));
        }
        if !p.is_dir() {
            return Err(SourceError::NotADirectory(p));
        }
        Ok(p)
    } else {
        let scheme = source.split(':').next().unwrap_or("").to_string();
        Err(SourceError::Unsupported(scheme))
    }
}

fn validate_source(dir: &Path) -> Result<magician::magician_v2::skills::SkillManifest, String> {
    let parent = dir.parent().ok_or_else(|| {
        format!(
            "source folder has no parent: {} — cannot run loader on it",
            dir.display()
        )
    })?;
    let name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("source folder has no usable file name: {}", dir.display()))?;
    let manifests = SkillLoader::new(vec![parent.to_path_buf()])
        .discover()
        .map_err(|e| format!("loader rejected source: {e:#}"))?;
    manifests
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| {
            format!(
                "loader found no skill named '{name}' under {} — frontmatter `name` must match the folder name",
                parent.display()
            )
        })
}

/// Path params are FULLY percent-decoded by actix's router (including
/// `%2F` and `%2E`), so an unvalidated `{name}` can traverse out of the
/// scope skills root — and the uninstall path is destructive. One guard,
/// every `{name}` handler.
fn skill_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn skill_name_guard(name: &str) -> Option<HttpResponse> {
    if skill_name_is_safe(name) {
        return None;
    }
    Some(error(
        actix_web::http::StatusCode::BAD_REQUEST,
        &format!("invalid skill name {name:?}"),
    ))
}

fn install_atomic(source: &Path, dest: &Path) -> std::io::Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let temp_name = format!(
        "{}.installing-{}",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("skill"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let staging = parent.join(&temp_name);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    if let Err(e) = copy_dir_recursive(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if dest.exists() {
        // The uninstall contract keeps `config/`, `auth/` and `.skill-state/`
        // in the destination skeleton; a reinstall must CARRY them over
        // (like `uninstall_skill_layer.py`) instead of wiping credentials.
        for preserved in UNINSTALL_PRESERVED {
            let from = dest.join(preserved);
            if from.exists() {
                std::fs::rename(&from, staging.join(preserved))?;
            }
        }
        std::fs::remove_dir_all(dest)?;
    }
    if let Err(e) = std::fs::rename(&staging, dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if kind.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if kind.is_symlink() {
            // Skills shouldn't ship symlinks; refuse rather than blindly follow.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("symlinks not allowed in skill source: {}", from.display()),
            ));
        } else {
            std::fs::copy(&from, &to)?;
            // Preserve executable bit on scripts/bin entries.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::metadata(&from)?.permissions();
                std::fs::set_permissions(&to, std::fs::Permissions::from_mode(perm.mode()))?;
            }
        }
    }
    Ok(())
}

fn parse_scope(entry: &str) -> Result<(String, String), String> {
    let mut parts = entry.split('/');
    let principal = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("workspace target '{entry}' must be `principal/workspace`"))?;
    let workspace = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("workspace target '{entry}' must be `principal/workspace`"))?;
    if parts.next().is_some() {
        return Err(format!(
            "workspace target '{entry}' has too many slashes — use `principal/workspace`"
        ));
    }
    Ok((principal.to_string(), workspace.to_string()))
}

fn error(status: actix_web::http::StatusCode, reason: &str) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({
        "status": "error",
        "reason": reason,
    }))
}

// Allow Arc-wrapping if a downstream caller wants shared ownership.
impl SkillsApi {
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[derive(Debug, Deserialize)]
pub struct AllowForAgentRequest {
    pub agent_id: String,
    /// "add" → append the skill to the agent's `tools:` list.
    /// "remove" → drop a same-named entry from the list.
    pub action: String,
    /// Optional scope (`principal/workspace`). When set, edits the
    /// per-scope agent runtime YAML at
    /// `magician_data_v3/<scope>/agent_runtime/agents/<agent>/definition.agent.yaml`.
    /// When unset, edits the system template at
    /// `magician_data_v3/system/agent_templates/agents/<agent>/definition.agent.yaml`.
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AllowForAgentResponse {
    pub status: String,
    pub agent_id: String,
    pub skill: String,
    pub action: String,
    pub agent_definition_path: String,
    pub tools: Vec<String>,
}

/// `POST /api/magician/v2/skills/{name}/allow-for-agent`
///
/// Modifies an agent's `tools:` list in its definition YAML to add or
/// remove a skill name. Uses line-based editing rather than a YAML
/// round-trip to preserve the file's existing comments and formatting
/// (serde_yaml strips comments and re-flows literals).
///
/// Auth: `X-Magician-Setup-Token` (admin-only — agent definitions are
/// privileged config).
pub async fn allow_for_agent_handler(
    req: HttpRequest,
    path: web::Path<String>,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    body: web::Json<AllowForAgentRequest>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let skill_name = path.into_inner();
    if let Some(response) = skill_name_guard(&skill_name) {
        return response;
    }
    if body.agent_id.trim().is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "agent_id is required",
        );
    }
    let action = body.action.trim().to_lowercase();
    if action != "add" && action != "remove" {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "action must be 'add' or 'remove'",
        );
    }

    // Locate the agent definition YAML. Per-scope wins over system
    // when a scope is requested.
    let agent_yaml = if let Some(scope) = body.scope.as_deref() {
        let (principal, workspace) = match parse_scope(scope) {
            Ok(pair) => pair,
            Err(reason) => return error(actix_web::http::StatusCode::BAD_REQUEST, &reason),
        };
        api.workspace_layout
            .scoped_agent_runtime_root(&principal, &workspace)
            .join("agents")
            .join(&body.agent_id)
            .join("definition.agent.yaml")
    } else {
        api.workspace_layout
            .system_agent_template_root()
            .join("agents")
            .join(&body.agent_id)
            .join("definition.agent.yaml")
    };
    if !agent_yaml.is_file() {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("agent definition not found at {}", agent_yaml.display()),
        );
    }

    let original = match std::fs::read_to_string(&agent_yaml) {
        Ok(s) => s,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to read {}: {e}", agent_yaml.display()),
            )
        },
    };

    let (updated, tools) = match edit_agent_tools_list(&original, &skill_name, &action) {
        Ok(pair) => pair,
        Err(reason) => return error(actix_web::http::StatusCode::UNPROCESSABLE_ENTITY, &reason),
    };

    if updated == original {
        return HttpResponse::Ok().json(AllowForAgentResponse {
            status: "noop".to_string(),
            agent_id: body.agent_id.clone(),
            skill: skill_name,
            action,
            agent_definition_path: agent_yaml.display().to_string(),
            tools,
        });
    }

    if let Err(e) = std::fs::write(&agent_yaml, &updated) {
        return error(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to write {}: {e}", agent_yaml.display()),
        );
    }

    tracing::info!(
        agent = %body.agent_id,
        skill = %skill_name,
        action = %action,
        path = %agent_yaml.display(),
        "skills_api: agent tools list updated"
    );

    HttpResponse::Ok().json(AllowForAgentResponse {
        status: "ok".to_string(),
        agent_id: body.agent_id.clone(),
        skill: skill_name,
        action,
        agent_definition_path: agent_yaml.display().to_string(),
        tools,
    })
}

/// Modify the `tools:` list in an agent definition YAML, preserving the
/// rest of the file byte-for-byte. Line-based edit so comments and
/// formatting survive.
///
/// Returns `(new_text, updated_tools_list)`.
///
/// Idempotent: adding an already-present tool or removing an absent
/// tool returns the original text + the existing tools list.
fn edit_agent_tools_list(
    yaml: &str,
    tool_name: &str,
    action: &str,
) -> Result<(String, Vec<String>), String> {
    let lines: Vec<&str> = yaml.split_inclusive('\n').collect();
    // Find the `tools:` line at column 0 (top-level key). The agent
    // YAMLs we ship have it at column 0; if a future schema nests
    // tools under another key, this fails fast rather than silently
    // editing the wrong block.
    let tools_idx = lines
        .iter()
        .position(|line| line.trim_end_matches(['\r', '\n']) == "tools:")
        .ok_or_else(|| "no top-level `tools:` block in agent definition".to_string())?;

    // Walk forward collecting `- <name>` entries at any indent until
    // we hit a non-list line.
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut last_entry_idx: usize = tools_idx;
    for (offset, line) in lines.iter().enumerate().skip(tools_idx + 1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") {
            let name = trimmed[2..].trim_end_matches(['\r', '\n', ' ']).to_string();
            entries.push((offset, name));
            last_entry_idx = offset;
        } else if trimmed.is_empty() {
            // Blank line within the tools block — tolerate.
            continue;
        } else {
            // Top-level (non-indented) sibling key — end of tools block.
            if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
            // Indented continuation (multi-line value, comment, etc.) —
            // tolerate but don't extend `last_entry_idx`.
        }
    }

    let existing_names: Vec<String> = entries.iter().map(|(_, n)| n.clone()).collect();

    let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

    let mut result_tools = existing_names.clone();
    let new_text = match action {
        "add" => {
            if existing_names.iter().any(|n| n == tool_name) {
                yaml.to_string()
            } else {
                // Insert after the last `- <name>` line, preserving the
                // entries' indentation.
                let insert_after = last_entry_idx;
                let indent = entries
                    .last()
                    .map(|(idx, _)| {
                        let line = lines[*idx];
                        line[..line.len() - line.trim_start().len()].to_string()
                    })
                    .unwrap_or_else(|| String::from(""));
                let new_line = format!("{indent}- {tool_name}\n");
                new_lines.insert(insert_after + 1, new_line);
                result_tools.push(tool_name.to_string());
                new_lines.concat()
            }
        },
        "remove" => {
            if let Some((idx, _)) = entries.iter().find(|(_, n)| n == tool_name) {
                new_lines.remove(*idx);
                result_tools.retain(|n| n != tool_name);
                new_lines.concat()
            } else {
                yaml.to_string()
            }
        },
        _ => return Err(format!("unsupported action '{action}'")),
    };

    Ok((new_text, result_tools))
}

/// Resolve a skill folder by name, preferring workspace layer over
/// `paths`-declared extras. Returns None if the skill isn't installed
/// in any layer.
fn resolve_skill_folder(api: &SkillsApi, req: &HttpRequest, name: &str) -> Option<PathBuf> {
    // Guarded here so every {name} consumer (schema, run) shares one
    // traversal-proof gate.
    if !skill_name_is_safe(name) {
        return None;
    }
    let workspace_dir = req
        .headers()
        .get("X-Principal")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|principal| {
            req.headers()
                .get("X-Workspace")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|workspace| api.workspace_layout.scope_skills_root(principal, workspace))
        });

    // `SKILL.md` is a host-absolute symlink into skillshub on a materialized
    // scope; rewrite the target for THIS environment (identity on a native host)
    // for the existence probe so it doesn't dangle in a container. The returned
    // `candidate` is the REAL (un-rewritten) scope dir — per-skill `config/.env`
    // is read from there; only `MAGICIAN_SKILL_DIR` + `bin/` get rewritten at
    // spawn time (mirror dispatcher.rs:330-331).
    if let Some(ws) = workspace_dir {
        let candidate = ws.join(name);
        if magician::magician_v2::skills::path_rewrite::resolve_skill_path(
            &candidate.join("SKILL.md"),
        )
        .is_file()
        {
            return Some(candidate);
        }
    }
    for extra_dir in magician::magician_v2::config_extras::extra_skills_dirs() {
        let candidate = extra_dir.join(name);
        if magician::magician_v2::skills::path_rewrite::resolve_skill_path(
            &candidate.join("SKILL.md"),
        )
        .is_file()
        {
            return Some(candidate);
        }
    }
    None
}

/// Build the scope's `CapabilityScopePaths` for a `/run` request so skill
/// subprocesses get the same tool-bin PATH augmentation the inner-loop CLI
/// dispatcher applies (venv/node_modules/.bin/node). The headers read here are
/// internal values engraved by the bearer middleware; open mode engraves the
/// local anonymous/default scope. Mirrors `run_task_preflight_soft`.
fn request_scope_paths(api: &SkillsApi, req: &HttpRequest) -> CapabilityScopePaths {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let principal = header("X-Principal").unwrap_or_else(|| DEFAULT_SCOPE_PRINCIPAL.to_string());
    let workspace = header("X-Workspace").unwrap_or_else(|| DEFAULT_SCOPE_WORKSPACE.to_string());
    CapabilityWorkspaceManager::new(api.workspace_layout.clone(), api.repo_root.clone())
        .scope_paths(&principal, &workspace)
}

/// `GET /api/magician/v2/skills/{name}/schema`
///
/// Returns the parsed `tool_schema.yaml` for the named skill as JSON,
/// preferring workspace-layer over system-layer (workspace wins).
/// Used by the debug "Direct Actions" panel to populate per-skill
/// action picker + parameter form. Read-only, unauthenticated.
pub async fn get_skill_schema_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let name = path.into_inner();
    let Some(skill_dir) = resolve_skill_folder(&api, &req, &name) else {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("skill `{name}` not found"),
        );
    };
    // `tool_schema.yaml` is a host-absolute symlink into skillshub on a
    // materialized scope; rewrite the target for THIS environment (identity on a
    // native host) so the existence check + read resolve in a container.
    let schema_path = magician::magician_v2::skills::path_rewrite::resolve_skill_path(
        &skill_dir.join("tool_schema.yaml"),
    );
    if !schema_path.is_file() {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("skill `{name}` has no tool_schema.yaml"),
        );
    }
    let raw = match std::fs::read_to_string(&schema_path) {
        Ok(s) => s,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to read tool_schema.yaml: {e}"),
            )
        },
    };
    let parsed: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to parse tool_schema.yaml: {e}"),
            )
        },
    };
    HttpResponse::Ok().json(parsed)
}

/// Body for `POST /api/magician/v2/skills/{name}/run`.
#[derive(Debug, Deserialize)]
pub struct RunSkillActionRequest {
    /// Optional action key in the schema's `native_action_schemas`. When
    /// set, the action's `argv:` prefix (if any) is appended to the
    /// resolved binary before `args`.
    #[serde(default)]
    pub action: Option<String>,
    /// Argv tokens passed after `<binary> <action_argv?>`. Caller is
    /// responsible for any per-parameter formatting — the runtime just
    /// forwards these tokens.
    #[serde(default)]
    pub args: Vec<String>,
    /// Top-level skill `parameters` values applied as a per-skill
    /// prelude before the main action runs. Browser maps these to
    /// session-start argv (`open about:blank --headed`, `connect <cdp_url>`,
    /// `navigate <url>`); other skills currently ignore them.
    #[serde(default)]
    pub session_params: std::collections::HashMap<String, String>,
    /// Wall-clock timeout. Defaults to 60s if omitted.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct RunSkillStep {
    pub label: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub parsed_json: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct RunSkillActionResponse {
    /// Per-skill setup steps run before the main action (browser
    /// session start / starting-URL navigate). Empty when no
    /// `session_params` were sent or the skill has no prelude mapping.
    pub prelude: Vec<RunSkillStep>,
    /// The primary action step. Mirrored into the flat top-level
    /// fields below so simple callers don't have to drill in.
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub parsed_json: Option<serde_json::Value>,
}

const DEFAULT_RUN_TIMEOUT_SECS: u64 = 60;
const BROWSER_DEBUG_SESSION_ID: &str = "magician-debug";

/// Spawn `argv[0]` with the rest as arguments, set the standard skill
/// env (MAGICIAN_SKILL_DIR + PATH prepend + per-skill `config/.env`),
/// pipe stdout/stderr, and capture the result with a wall-clock
/// timeout. Used by both prelude and primary steps.
///
/// `runtime_dir` is the environment-resolved skill dir (rewritten to the real
/// skillshub dir in a container, identity natively) and feeds
/// `MAGICIAN_SKILL_DIR` + the `bin/` PATH prepend so vendored binaries resolve.
/// `real_dir` is the un-rewritten scope dir; per-skill `config/.env` lives there
/// and is NOT materialized into skillshub (mirror dispatcher.rs:330-331).
async fn spawn_skill_step(
    label: &str,
    argv: &[String],
    runtime_dir: &Path,
    real_dir: &Path,
    timeout_secs: u64,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Result<RunSkillStep, String> {
    if argv.is_empty() {
        return Err(format!("step `{label}` has empty argv"));
    }
    let started = Instant::now();
    // Prepend the per-skill `bin/` AND the scope's shared tool-bin dirs
    // (venv/node_modules/.bin/node) so a bare npm/venv/node tool (`gws`, and
    // node-shebang absolutes needing `node`) resolves — the hand-rolled block
    // here only prepended `bin/`, so those exited 127. Fail-safe: no
    // scope_paths / no existing dirs → fall back to `bin/` only, else inherited.
    // Computed first so the program is resolved against the PATH the child
    // will see and the spawn stays on `posix_spawn` (see
    // `runtime_core::process`).
    let skill_bin = runtime_dir.join("bin");
    let parent_path = std::env::var("PATH").unwrap_or_default();
    let augmented =
        scope_paths.and_then(|sp| sp.subprocess_bin_path(Some(skill_bin.as_path()), &parent_path));
    let child_path = match augmented {
        Some(path) => Some(path),
        None if skill_bin.is_dir() => Some(if parent_path.is_empty() {
            skill_bin.to_string_lossy().to_string()
        } else {
            format!("{}:{}", skill_bin.display(), parent_path)
        }),
        None => None,
    };
    // The per-skill `config/.env` is applied after that prepend and wins;
    // when it carries `PATH`, that is the PATH the child receives, so the
    // program is resolved against it. Read here, applied below in the same
    // order as before.
    let env_file = real_dir.join("config").join(".env");
    let skill_env: Vec<(String, String)> = if env_file.is_file() {
        dotenvy::from_path_iter(&env_file)
            .map(|iter| iter.flatten().collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let final_path = skill_env
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| child_path.clone());
    let mut cmd = Command::new(runtime_core::process::resolve_program_str(
        &argv[0],
        final_path.as_deref(),
    ));
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    cmd.env("MAGICIAN_SKILL_DIR", runtime_dir);
    if let Some(path) = child_path {
        cmd.env("PATH", path);
    }
    for (key, value) in skill_env {
        cmd.env(key, value);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs.max(1)),
        cmd.output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("failed to spawn `{}`: {e}", argv[0])),
        Err(_) => return Err(format!("step `{label}` exceeded {timeout_secs}s timeout")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let parsed_json = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();

    Ok(RunSkillStep {
        label: label.to_string(),
        argv: argv.to_vec(),
        exit_code: output.status.code(),
        success: output.status.success(),
        stdout,
        stderr,
        duration_ms: started.elapsed().as_millis(),
        parsed_json,
    })
}

/// Translate browser session_params into a list of `agent-browser`
/// argv steps that prepare the named session before the primary
/// primitive runs. Mirrors `BrowserDispatcher`'s session-start logic
/// (`open about:blank --headed`, `connect <cdp_url>`, plus an optional
/// `navigate <url>` hint).
fn browser_session_prelude(
    session_id: &str,
    session_params: &std::collections::HashMap<String, String>,
) -> Vec<(String, Vec<String>)> {
    let mut steps: Vec<(String, Vec<String>)> = Vec::new();
    let mode = session_params
        .get("connection_mode")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let cdp_url = session_params
        .get("cdp_url")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let url = session_params
        .get("url")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    if let Some(mode) = mode {
        let mut argv = vec![
            "agent-browser".to_string(),
            "--session".to_string(),
            session_id.to_string(),
        ];
        match mode {
            "cdp" => {
                argv.push("connect".to_string());
                argv.push(cdp_url.unwrap_or("http://localhost:9222").to_string());
            },
            "headed" => {
                argv.extend([
                    "open".to_string(),
                    "about:blank".to_string(),
                    "--headed".to_string(),
                ]);
            },
            "headless" | _ => {
                argv.extend(["open".to_string(), "about:blank".to_string()]);
            },
        }
        steps.push((format!("session-start ({mode})"), argv));
    }

    if let Some(url) = url {
        let argv = vec![
            "agent-browser".to_string(),
            "--session".to_string(),
            session_id.to_string(),
            "navigate".to_string(),
            url.to_string(),
        ];
        steps.push(("navigate".to_string(), argv));
    }

    steps
}

/// `POST /api/magician/v2/skills/{name}/run`
///
/// Runs one CLI primitive against the named skill and returns
/// stdout/stderr/exit-code. The "Direct Actions" debug panel uses this
/// to exercise individual primitives without going through the LLM
/// inner loop.
///
/// Resolution rules:
/// - `browser` skill → binary is `agent-browser` (custom session-based
///   dispatcher in production; debug panel just shells out one-shot).
/// - other skills → binary is `implementation.command[0]` from the
///   skill's `tool_schema.yaml`. Subsequent tokens are appended.
///
/// When `action` is set and the schema declares `native_action_schemas
/// .<action>.argv`, those tokens are appended after the binary command
/// and before `args`. Mirrors the agent-browser-style contract used by
/// gmail / jq / awk / csvkit / sheets / etc.
///
/// `MAGICIAN_SKILL_DIR` and `<skill_dir>/bin` PATH prepend mirror the
/// inner-loop CLI dispatcher so vendored binaries (pdftotext/bin,
/// ocr/bin, marimo/bin) resolve. Per-skill `config/.env` is also loaded
/// the same way.
pub async fn run_skill_action_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    path: web::Path<String>,
    body: web::Json<RunSkillActionRequest>,
) -> HttpResponse {
    let name = path.into_inner();
    let Some(skill_dir) = resolve_skill_folder(&api, &req, &name) else {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("skill `{name}` not found"),
        );
    };

    // `tool_schema.yaml` is a host-absolute symlink into skillshub on a
    // materialized scope; rewrite the target for THIS environment (identity on a
    // native host) so the read resolves in a container.
    let schema_path = magician::magician_v2::skills::path_rewrite::resolve_skill_path(
        &skill_dir.join("tool_schema.yaml"),
    );
    let schema_raw = match std::fs::read_to_string(&schema_path) {
        Ok(s) => s,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to read tool_schema.yaml: {e}"),
            )
        },
    };

    // Scripts + `bin/` must resolve for THIS environment (rewritten to the real
    // skillshub dir in a container, identity natively); per-skill `config/.env`
    // stays on the REAL scope dir — it is NOT materialized into skillshub
    // (mirror dispatcher.rs:330-331). `runtime_dir` feeds MAGICIAN_SKILL_DIR +
    // the `bin/` PATH prepend; `skill_dir` keeps the secrets read.
    let runtime_dir = magician::magician_v2::skills::path_rewrite::rewrite_skill_dir(&skill_dir);
    let schema: serde_yaml::Value = match serde_yaml::from_str(&schema_raw) {
        Ok(v) => v,
        Err(e) => {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to parse tool_schema.yaml: {e}"),
            )
        },
    };

    let timeout_secs = body.timeout_secs.unwrap_or(DEFAULT_RUN_TIMEOUT_SECS);
    let is_browser = name == "browser";
    // Scope's tool-bin PATH augmentation for every skill subprocess below.
    let scope_paths = request_scope_paths(&api, &req);

    // Browser-only: derive the session id from session_params (or the
    // stable debug default) and run any session-start / navigate
    // prelude steps before the primary primitive. Each step shares the
    // same `--session <id>`, so subsequent /run calls reuse the live
    // browser without needing the operator to re-fill connection_mode.
    let mut prelude_steps: Vec<RunSkillStep> = Vec::new();
    let session_id = body
        .session_params
        .get("session_id")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| BROWSER_DEBUG_SESSION_ID.to_string());

    if is_browser {
        for (label, argv) in browser_session_prelude(&session_id, &body.session_params) {
            match spawn_skill_step(
                &label,
                &argv,
                &runtime_dir,
                &skill_dir,
                timeout_secs,
                Some(&scope_paths),
            )
            .await
            {
                Ok(step) => {
                    let failed = !step.success;
                    prelude_steps.push(step);
                    if failed {
                        return HttpResponse::Ok().json(serde_json::json!({
                            "prelude": prelude_steps,
                            "argv": Vec::<String>::new(),
                            "exit_code": serde_json::Value::Null,
                            "success": false,
                            "stdout": "",
                            "stderr": format!("aborted: prelude step `{}` failed", label),
                            "duration_ms": 0,
                            "parsed_json": serde_json::Value::Null,
                        }));
                    }
                },
                Err(e) => return error(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
    }

    // Resolve primary-step binary. Browser uses `agent-browser` via PATH
    // and gets `--session <id>` injected so all primitives share state.
    // Other inner-loop CLI skills use `implementation.command` from YAML.
    let mut argv: Vec<String> = if is_browser {
        vec![
            "agent-browser".to_string(),
            "--session".to_string(),
            session_id.clone(),
        ]
    } else {
        match schema
            .get("implementation")
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_sequence())
        {
            Some(seq) => seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => {
                return error(
                    actix_web::http::StatusCode::BAD_REQUEST,
                    &format!(
                        "skill `{name}` has no `implementation.command`; one-shot runner only supports CLI-template skills"
                    ),
                )
            },
        }
    };
    if argv.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            &format!("skill `{name}` declares an empty implementation.command"),
        );
    }

    // Per-action argv prefix (e.g. gmail's [`gmail`, `+triage`]).
    if let Some(action_name) = body.action.as_deref().filter(|s| !s.is_empty()) {
        if let Some(prefix) = schema
            .get("native_action_schemas")
            .and_then(|v| v.get(action_name))
            .and_then(|v| v.get("argv"))
            .and_then(|v| v.as_sequence())
        {
            for token in prefix {
                if let Some(s) = token.as_str() {
                    argv.push(s.to_string());
                }
            }
        }
    }

    argv.extend(body.args.iter().cloned());

    let primary = match spawn_skill_step(
        "primary",
        &argv,
        &runtime_dir,
        &skill_dir,
        timeout_secs,
        Some(&scope_paths),
    )
    .await
    {
        Ok(step) => step,
        Err(e) => return error(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, &e),
    };

    HttpResponse::Ok().json(RunSkillActionResponse {
        prelude: prelude_steps,
        argv: primary.argv,
        exit_code: primary.exit_code,
        success: primary.success,
        stdout: primary.stdout,
        stderr: primary.stderr,
        duration_ms: primary.duration_ms,
        parsed_json: primary.parsed_json,
    })
}

// ---------------------------------------------------------------------------
// Per-skill env — the skill's own `config/.env` inside a scope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SkillEnvKey {
    pub key: String,
    /// `required` (manifest requires.env), `template` (config/.env.example),
    /// or `extra` (set but declared nowhere).
    pub source: String,
    pub set: bool,
}

#[derive(Debug, Serialize)]
pub struct SkillEnvStatus {
    pub skill: String,
    pub scope: String,
    pub keys: Vec<SkillEnvKey>,
}

#[derive(Debug, Deserialize)]
pub struct SkillEnvUpdateRequest {
    pub workspaces: Vec<String>,
    pub updates: std::collections::HashMap<String, Option<String>>,
}

fn skill_env_example_keys(dir: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(dir.join("config/.env.example")).unwrap_or_default();
    let mut keys: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // Template keys appear as `KEY=...` or commented `# KEY=...`.
        let candidate = trimmed.strip_prefix('#').map(str::trim).unwrap_or(trimmed);
        if let Some((key, _)) = candidate.split_once('=') {
            let key = key.trim();
            let valid = !key.is_empty()
                && key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if valid {
                keys.push(key.to_string());
            }
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn scope_skill_dir(api: &SkillsApi, entry: &str, name: &str) -> Result<PathBuf, HttpResponse> {
    let (principal, workspace) = match parse_scope(entry) {
        Ok(pair) => pair,
        Err(reason) => {
            return Err(error(actix_web::http::StatusCode::BAD_REQUEST, &reason));
        },
    };
    let dir = api
        .workspace_layout
        .scope_skills_root(&principal, &workspace)
        .join(name);
    if !dir.is_dir() {
        return Err(error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("skill '{name}' is not installed in scope {principal}/{workspace}"),
        ));
    }
    Ok(dir)
}

/// `GET /api/magician/v2/skills/catalog/{name}/env?scope=principal/workspace`
///
/// Status-only: which env keys the skill wants (manifest + template) and
/// which are set in its `config/.env` — names, never values.
pub async fn get_skill_env_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    path: web::Path<String>,
) -> HttpResponse {
    // Setup token: per-scope key status is cross-scope management data, not
    // bearer-visible state.
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let name = path.into_inner();
    if let Some(response) = skill_name_guard(&name) {
        return response;
    }
    let scope = req
        .query_string()
        .split('&')
        .find_map(|pair| pair.strip_prefix("scope="))
        .map(|s| s.replace("%2F", "/").replace("%2f", "/"))
        .unwrap_or_default();
    if scope.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "query parameter `scope=principal/workspace` is required",
        );
    }
    let dir = match scope_skill_dir(&api, &scope, &name) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let required: std::collections::HashSet<String> = validate_source(&dir)
        .ok()
        .and_then(|manifest| manifest.metadata.magician.map(|mag| mag.requires.env))
        .map(|env| env.into_iter().collect())
        .unwrap_or_default();
    let template: std::collections::HashSet<String> =
        skill_env_example_keys(&dir).into_iter().collect();
    let set: std::collections::HashMap<String, String> =
        magician::magician_v2::runtime_settings::read_env_file_values(&dir.join("config/.env"));

    let mut keys: Vec<SkillEnvKey> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for key in required.iter().chain(template.iter()) {
        if seen.insert(key.clone()) {
            let source = if required.contains(key) {
                "required"
            } else {
                "template"
            };
            keys.push(SkillEnvKey {
                key: key.clone(),
                source: source.to_string(),
                set: set.contains_key(key),
            });
        }
    }
    for key in set.keys() {
        if seen.insert(key.clone()) {
            keys.push(SkillEnvKey {
                key: key.clone(),
                source: "extra".to_string(),
                set: true,
            });
        }
    }
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    HttpResponse::Ok().json(SkillEnvStatus {
        skill: name,
        scope,
        keys,
    })
}

/// `POST /api/magician/v2/skills/catalog/{name}/env`
///
/// Admin (setup token). Writes `<scope>/skills/<name>/config/.env` through
/// the shared atomic 0600 writer — the same file skill dispatch reads.
pub async fn put_skill_env_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    path: web::Path<String>,
    body: web::Json<SkillEnvUpdateRequest>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let name = path.into_inner();
    if let Some(response) = skill_name_guard(&name) {
        return response;
    }
    if body.workspaces.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "workspaces must specify at least one `principal/workspace` entry",
        );
    }
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    for (key, value) in &body.updates {
        let valid = !key.is_empty()
            && key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return error(
                actix_web::http::StatusCode::BAD_REQUEST,
                &format!("invalid env key {key:?}"),
            );
        }
        if let Some(v) = value {
            if v.trim().is_empty() {
                return error(
                    actix_web::http::StatusCode::BAD_REQUEST,
                    &format!("{key}: empty values are not allowed — send null to delete"),
                );
            }
        }
        pairs.push((key.clone(), value.clone()));
    }
    if pairs.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "updates must contain at least one key",
        );
    }

    let mut targets: Vec<String> = Vec::new();
    for entry in &body.workspaces {
        let dir = match scope_skill_dir(&api, entry, &name) {
            Ok(dir) => dir,
            Err(response) => return response,
        };
        if let Err(e) = std::fs::create_dir_all(dir.join("config")) {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("config dir create failed: {e}"),
            );
        }
        let updates: Vec<(&str, Option<String>)> =
            pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        if let Err(e) = magician::magician_v2::runtime_settings::update_env_file(
            &dir.join("config/.env"),
            &updates,
        ) {
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("env write failed: {e}"),
            );
        }
        targets.push(entry.clone());
    }

    tracing::info!(
        skill = %name,
        targets = ?targets,
        "skills_api: per-skill env updated"
    );
    HttpResponse::Ok().json(serde_json::json!({ "updated": targets }))
}

#[derive(Serialize)]
struct SkillOAuthStartResponse {
    authorization_url: String,
    status: &'static str,
}

#[derive(Serialize)]
struct SkillOAuthStatusResponse {
    ready: bool,
    status: AuthState,
    detail: String,
}

fn skill_oauth_coordinator(
    req: &HttpRequest,
    api: &SkillsApi,
    vault: &SecretVaultApi,
    oauth_api: &crate::mcp_oauth_api::McpOAuthApi,
    name: &str,
) -> Result<(Arc<McpOAuthCoordinator>, CredentialScope), HttpResponse> {
    let Some(skill_dir) = resolve_skill_folder(api, req, name) else {
        return Err(error(
            actix_web::http::StatusCode::NOT_FOUND,
            &format!("skill `{name}` is not installed in this workspace"),
        ));
    };
    let skill_md = magician::magician_v2::skills::path_rewrite::resolve_skill_path(
        &skill_dir.join("SKILL.md"),
    );
    let source = std::fs::read_to_string(&skill_md).map_err(|_| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "the installed skill manifest could not be read",
        )
    })?;
    let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
        .map_err(|_| {
            error(
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                "the installed skill runtime contract is invalid",
            )
        })?
        .ok_or_else(|| {
            error(
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                "the installed skill has no governed runtime contract",
            )
        })?;
    if package.contract.auth.kind != AuthKind::OAuthSession {
        return Err(error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "this skill does not use governed OAuth",
        ));
    }
    let frontmatter = tool_runtime_core::manifest_parser::parse_skill_frontmatter::<
        SkillCatalogFrontmatter,
    >(&source)
    .map_err(|_| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "the installed skill setup metadata is invalid",
        )
    })?;
    let mut setup_binding = frontmatter.metadata.magician.setup.ok_or_else(|| {
        error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "this skill does not declare a Desktop-managed setup flow",
        )
    })?;
    if setup_binding.profile.is_none() {
        setup_binding.profile = setup_profile(&package.contract.auth.profile_selection);
    }
    let resolved_setup = setup::resolve(&setup_binding).map_err(|message| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            &format!("the installed skill setup metadata is invalid: {message}"),
        )
    })?;
    if !matches!(resolved_setup.driver, SetupDriver::GovernedOauth) {
        return Err(error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "this skill does not declare governed OAuth setup",
        ));
    }
    let provider = package.contract.auth.provider.as_deref().ok_or_else(|| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "the skill OAuth provider is not declared",
        )
    })?;
    let profile_alias = match &package.contract.auth.profile_selection {
        ProfileSelection::Fixed { alias } => alias.clone(),
        ProfileSelection::Selectable {
            default: Some(alias),
        } => alias.clone(),
        ProfileSelection::None => "default".to_string(),
        ProfileSelection::Selectable { default: None } | ProfileSelection::Implicit => {
            return Err(error(
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                "the skill requires an explicit OAuth profile before setup",
            ));
        },
    };
    let RuntimeProtocol::Mcp {
        transport,
        discovery,
        ..
    } = &package.contract.runtime
    else {
        return Err(error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "governed OAuth setup is available only for remote MCP skills",
        ));
    };
    let McpTransport::StreamableHttp { endpoint } = transport else {
        return Err(error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "governed OAuth setup requires a Streamable HTTP MCP endpoint",
        ));
    };
    let resource_url = if discovery.endpoint_aliases.is_empty() {
        endpoint.as_str()
    } else {
        let alias = discovery.default_endpoint_alias.as_deref().ok_or_else(|| {
            error(
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                "the skill requires an MCP endpoint choice before setup",
            )
        })?;
        discovery
            .endpoint_aliases
            .get(alias)
            .map(String::as_str)
            .ok_or_else(|| {
                error(
                    actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "the skill's default MCP endpoint is not declared",
                )
            })?
    };
    let oauth = discovery.oauth.as_ref().ok_or_else(|| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "the skill's MCP OAuth policy is not declared",
        )
    })?;
    let (principal, workspace) = vault.resolve_required_scope(req)?;
    let scope = CredentialScope::new(principal, workspace).map_err(|_| {
        error(
            actix_web::http::StatusCode::FORBIDDEN,
            "the authenticated workspace scope is invalid",
        )
    })?;
    let profile = CredentialProfileKey::new(
        scope.clone(),
        provider,
        profile_alias,
        CredentialProfileBinding::McpOauth {
            resource_url: CanonicalCredentialUrl::new(resource_url).map_err(|_| {
                error(
                    actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "the skill's MCP resource binding is invalid",
                )
            })?,
            authorization_issuer: CanonicalCredentialUrl::new(&oauth.authorization_issuer)
                .map_err(|_| {
                    error(
                        actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
                        "the skill's OAuth issuer binding is invalid",
                    )
                })?,
        },
    )
    .map_err(|_| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "the skill's OAuth profile binding is invalid",
        )
    })?;
    let scoped_store = vault.scoped_store(req)?;
    let oauth_vault = Arc::new(SecretStoreMcpOAuthVault::new(scoped_store));
    let coordinator = McpOAuthCoordinator::new(
        profile,
        oauth_vault,
        oauth_api.callback_base_url(),
        McpOAuthClientIdentity::DynamicPublic,
    )
    .and_then(|coordinator| coordinator.with_client_name("Magican MCP Client"))
    .and_then(|coordinator| coordinator.with_scopes(oauth.scopes.iter().cloned()))
    .map_err(|oauth_error| {
        error(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            oauth_error.message,
        )
    })?;
    Ok((Arc::new(coordinator), scope))
}

fn skill_oauth_product_error(
    error_value: crate::mcp_oauth_api::McpOAuthProductError,
) -> HttpResponse {
    use crate::mcp_oauth_api::McpOAuthProductErrorCode;
    let status = match error_value.code {
        McpOAuthProductErrorCode::PendingAuthorizationConflict => {
            actix_web::http::StatusCode::CONFLICT
        },
        McpOAuthProductErrorCode::CapacityExceeded
        | McpOAuthProductErrorCode::BrowserUnavailable
        | McpOAuthProductErrorCode::ProviderUnavailable => {
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        },
        McpOAuthProductErrorCode::ScopeMismatch => actix_web::http::StatusCode::FORBIDDEN,
        _ => actix_web::http::StatusCode::BAD_REQUEST,
    };
    error(status, error_value.message)
}

/// Start one exact governed MCP OAuth binding for native Desktop setup.
/// The narrow setup-token route returns the provider URL once; tokens and
/// callback state remain in the engine's scoped vault.
pub async fn start_skill_oauth_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    oauth_api: web::Data<crate::mcp_oauth_api::McpOAuthApi>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let name = path.into_inner();
    if let Some(response) = skill_name_guard(&name) {
        return response;
    }
    let (coordinator, _scope) = match skill_oauth_coordinator(&req, &api, &vault, &oauth_api, &name)
    {
        Ok(result) => result,
        Err(response) => return response,
    };
    let launch = match oauth_api
        .register_and_begin_for_native_setup(coordinator)
        .await
    {
        Ok(launch) => launch,
        Err(oauth_error) => return skill_oauth_product_error(oauth_error),
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(SkillOAuthStartResponse {
            authorization_url: launch.authorization_url().to_owned(),
            status: "authorization_pending",
        })
}

/// Return secret-free readiness for one exact governed MCP OAuth binding.
pub async fn get_skill_oauth_status_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    oauth_api: web::Data<crate::mcp_oauth_api::McpOAuthApi>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let name = path.into_inner();
    if let Some(response) = skill_name_guard(&name) {
        return response;
    }
    let (coordinator, scope) = match skill_oauth_coordinator(&req, &api, &vault, &oauth_api, &name)
    {
        Ok(result) => result,
        Err(response) => return response,
    };
    let binding_id = coordinator.callback_binding_id();
    let status = match oauth_api.credential_status(&binding_id, &scope).await {
        Ok(status) => status,
        Err(oauth_error)
            if oauth_error.code
                == crate::mcp_oauth_api::McpOAuthProductErrorCode::AuthorizationNotFound =>
        {
            if let Err(register_error) = oauth_api.register_coordinator(Arc::clone(&coordinator)) {
                if register_error.code
                    != crate::mcp_oauth_api::McpOAuthProductErrorCode::PendingAuthorizationConflict
                {
                    return skill_oauth_product_error(register_error);
                }
            }
            match oauth_api.credential_status(&binding_id, &scope).await {
                Ok(status) => status,
                Err(oauth_error) => return skill_oauth_product_error(oauth_error),
            }
        },
        Err(oauth_error) => return skill_oauth_product_error(oauth_error),
    };
    let ready = status.state == AuthState::Ready;
    let detail = match status.state {
        AuthState::Ready => "Account login is active",
        AuthState::Authenticating => "Waiting for account authorization",
        AuthState::Missing | AuthState::InteractionRequired => "Account login is required",
        AuthState::Expired => "Account login has expired",
        AuthState::Revoked => "Account access was revoked",
        AuthState::IdentityMismatch => "The connected account does not match this profile",
        AuthState::Denied => "Account authorization was denied",
        AuthState::Error => "Account login could not be verified",
        AuthState::Unknown => "Account login status is not available",
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(SkillOAuthStatusResponse {
            ready,
            status: status.state,
            detail: detail.to_string(),
        })
}

/// Register the skills routes under the `/api/magician/v2` scope.
pub fn configure_skills_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/skills", web::get().to(list_skills_handler))
        .route(
            "/skills/catalog",
            web::get().to(list_skills_catalog_handler),
        )
        .route("/skills/install", web::post().to(install_skill_handler))
        .route(
            "/skills/{name}/uninstall",
            web::post().to(uninstall_skill_handler),
        )
        .route(
            "/skills/{name}/schema",
            web::get().to(get_skill_schema_handler),
        )
        .route(
            "/skills/{name}/run",
            web::post().to(run_skill_action_handler),
        )
        .route(
            "/skills/{name}/allow-for-agent",
            web::post().to(allow_for_agent_handler),
        )
        .route(
            "/skills/catalog/{name}/env",
            web::get().to(get_skill_env_handler),
        )
        .route(
            "/skills/catalog/{name}/env",
            web::post().to(put_skill_env_handler),
        )
        .route(
            "/skills/catalog/{name}/oauth/start",
            web::post().to(start_skill_oauth_handler),
        )
        .route(
            "/skills/catalog/{name}/oauth/status",
            web::get().to(get_skill_oauth_status_handler),
        );
}

// ---------------------------------------------------------------------------
// Catalog — every skill in the system, installed or not
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SkillCatalogEntry {
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    /// `tool`, `procedure`, `personality`, or `compiled` — the authoring
    /// taxonomy, so procedures and personality modes are first-class catalog
    /// rows rather than an afterthought of the tool list.
    pub kind: String,
    /// Where the folder lives: `skillshub` (authored source), `extras`
    /// (tool-runtime-config registry paths), `scope` (installed from a
    /// `path:` source, no upstream folder), or `built-in` (compiled packs).
    pub origin: String,
    /// Authored Skillshub and extra skills can be installed into a scope.
    /// Compiled built-ins are always present and therefore have no install
    /// action.
    pub installable: bool,
    pub requires_bins: Vec<String>,
    /// Required executables not resolvable in the current request scope's
    /// actual skill subprocess PATH. Empty for skills not installed there.
    pub missing_bins: Vec<String>,
    pub requires_env: Vec<String>,
    /// Human-authored prerequisite guidance from
    /// `metadata.magician.install_hint.docs`.
    pub install_hint: Option<String>,
    /// Typed authentication contract for governed tool skills. Values are
    /// names and setup modes only; secret values never enter this response.
    pub auth: Option<SkillCatalogAuth>,
    /// `principal/workspace` labels of every scope whose skills dir contains
    /// this name — scope install is pure filesystem presence.
    pub installed_scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillCatalogAuth {
    pub kind: AuthKind,
    pub requirement: AuthRequirement,
    pub provider: Option<String>,
    pub profile_selection: ProfileSelection,
    pub secret_refs: Vec<String>,
    pub has_status: bool,
    pub has_login: bool,
    pub login_interaction: Option<CliInteraction>,
    pub has_logout: bool,
    /// Declarative Desktop/setup flow resolved from Skillshub metadata.
    pub setup: Option<ResolvedSetup>,
}

#[derive(Debug, Deserialize)]
struct SkillCatalogFrontmatter {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    metadata: SkillCatalogMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SkillCatalogMetadata {
    #[serde(default)]
    magician: SkillCatalogMagicianMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SkillCatalogMagicianMetadata {
    #[serde(default)]
    setup: Option<SetupBinding>,
}

#[derive(Debug, Serialize)]
pub struct SkillCatalogResponse {
    pub skills: Vec<SkillCatalogEntry>,
    /// Scope engraved by the authenticated request middleware. Desktop uses
    /// this exact label for scoped install/remove actions.
    pub current_scope: String,
}

/// `GET /api/magician/v2/skills/catalog`
///
/// The full system catalog: skillshub authoring source + extras roots +
/// skills that exist only inside a scope + embedded compiled packs, each with
/// its install footprint across scopes. Read-only like `GET /skills`.
pub async fn list_skills_catalog_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
) -> HttpResponse {
    let scope_paths = request_scope_paths(&api, &req);
    let current_scope = format!("{}/{}", scope_paths.principal, scope_paths.workspace);
    let scope_sets = collect_scope_skill_sets(&api.workspace_layout);

    let mut entries: Vec<SkillCatalogEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut origin_dirs: Vec<(PathBuf, &str)> =
        vec![(api.repo_root.join("skillshub"), "skillshub")];
    for dir in magician::magician_v2::config_extras::extra_skills_dirs() {
        origin_dirs.push((dir, "extras"));
    }
    for (root, origin) in origin_dirs {
        let children = match std::fs::read_dir(&root) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for child in children.flatten() {
            let dir = child.path();
            if !dir.is_dir() || !dir.join("SKILL.md").is_file() {
                continue;
            }
            let name = match dir.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !seen.insert(name.clone()) {
                continue;
            }
            match catalog_entry_from_dir(&dir, &name, origin, &scope_sets) {
                Some(entry) => entries.push(entry),
                None => {
                    seen.remove(&name);
                    tracing::warn!(
                        skill = %name,
                        origin = origin,
                        "skills_api: catalog skipped a skill the loader rejects"
                    );
                },
            }
        }
    }

    // Skills installed into a scope from a `path:` source have no upstream
    // folder — they are still part of the system and must be manageable.
    for (scope_label, names) in &scope_sets {
        let (principal, workspace) = match parse_scope(scope_label) {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        for name in names {
            if !seen.insert(name.clone()) {
                continue;
            }
            let dir = api
                .workspace_layout
                .scope_skills_root(&principal, &workspace)
                .join(&name);
            match catalog_entry_from_dir(&dir, &name, "scope", &scope_sets) {
                Some(entry) => entries.push(entry),
                None => {
                    seen.remove(name);
                },
            }
        }
    }

    // Embedded compiled packs: callable built-ins, never installable.
    for pack in embedded_compiled_pack_defs() {
        if seen.insert(pack.name.clone()) {
            entries.push(SkillCatalogEntry {
                name: pack.name.clone(),
                version: None,
                description: pack.description.clone().unwrap_or_default(),
                kind: "compiled".to_string(),
                origin: "built-in".to_string(),
                installable: false,
                requires_bins: Vec::new(),
                missing_bins: Vec::new(),
                requires_env: Vec::new(),
                install_hint: None,
                auth: None,
                installed_scopes: Vec::new(),
            });
        }
    }

    annotate_missing_catalog_bins(&mut entries, &scope_paths, &current_scope);
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    HttpResponse::Ok().json(SkillCatalogResponse {
        skills: entries,
        current_scope,
    })
}

/// One catalog entry from a skill folder, or `None` when the loader rejects
/// it (the catalog lists skills the runtime could actually load).
fn catalog_entry_from_dir(
    dir: &Path,
    name: &str,
    origin: &str,
    scope_sets: &[(String, std::collections::HashSet<String>)],
) -> Option<SkillCatalogEntry> {
    let manifest = validate_source(dir).ok()?;
    let source = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let (requires_bins, requires_env) = match &manifest.metadata.magician {
        Some(mag) => (mag.requires.bins.clone(), mag.requires.env.clone()),
        None => (Vec::new(), Vec::new()),
    };
    let install_hint = manifest
        .metadata
        .magician
        .as_ref()
        .and_then(|magician| magician.install_hint.get("docs"))
        .cloned();
    let catalog_frontmatter = tool_runtime_core::manifest_parser::parse_skill_frontmatter::<
        SkillCatalogFrontmatter,
    >(&source)
    .ok();
    let version = catalog_frontmatter
        .as_ref()
        .and_then(|frontmatter| frontmatter.version.clone());
    let setup_binding =
        catalog_frontmatter.and_then(|frontmatter| frontmatter.metadata.magician.setup);
    let auth = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
        .ok()
        .flatten()
        .map(|package| {
            let auth = package.contract.auth;
            let has_status = auth.lifecycle.status.is_some();
            let has_login = auth.lifecycle.login.is_some();
            let login_interaction = auth.lifecycle.login.as_ref().map(|hook| hook.interaction);
            let has_logout = auth.lifecycle.logout.is_some();
            let setup = setup_binding.as_ref().and_then(|binding| {
                let mut binding = binding.clone();
                if binding.profile.is_none() {
                    binding.profile = setup_profile(&auth.profile_selection);
                }
                match setup::resolve(&binding) {
                    Ok(setup) => Some(setup),
                    Err(error) => {
                        tracing::warn!(skill = %name, %error, "skills_api: invalid setup binding");
                        None
                    },
                }
            });
            SkillCatalogAuth {
                kind: auth.kind,
                requirement: auth.requirement,
                provider: auth.provider,
                profile_selection: auth.profile_selection,
                secret_refs: auth
                    .secret_bindings
                    .into_iter()
                    .map(|binding| binding.secret_ref)
                    .collect(),
                has_status,
                has_login,
                login_interaction,
                has_logout,
                setup,
            }
        });
    let installed_scopes = scope_sets
        .iter()
        .filter(|(_, names)| names.contains(name))
        .map(|(label, _)| label.clone())
        .collect();
    Some(SkillCatalogEntry {
        name: name.to_string(),
        version,
        description: manifest.description.clone(),
        kind: catalog_kind(dir, &manifest),
        origin: origin.to_string(),
        installable: origin == "skillshub",
        requires_bins,
        missing_bins: Vec::new(),
        requires_env,
        install_hint,
        auth,
        installed_scopes,
    })
}

fn setup_profile(selection: &ProfileSelection) -> Option<String> {
    match selection {
        ProfileSelection::Fixed { alias } => Some(alias.clone()),
        ProfileSelection::Selectable { default } => default.clone(),
        ProfileSelection::None | ProfileSelection::Implicit => None,
    }
}

fn annotate_missing_catalog_bins(
    entries: &mut [SkillCatalogEntry],
    scope_paths: &CapabilityScopePaths,
    current_scope: &str,
) {
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    for entry in entries {
        if !entry
            .installed_scopes
            .iter()
            .any(|scope| scope == current_scope)
        {
            continue;
        }
        let skill_bin = scope_paths
            .capabilities_root
            .join("skills")
            .join(&entry.name)
            .join("bin");
        let effective_path = scope_paths
            .subprocess_bin_path(Some(&skill_bin), &inherited_path)
            .unwrap_or_else(|| inherited_path.clone());
        entry.missing_bins = entry
            .requires_bins
            .iter()
            .filter(|binary| {
                !is_runnable_program(&runtime_core::process::resolve_program(
                    std::ffi::OsStr::new(binary.as_str()),
                    Some(std::ffi::OsStr::new(&effective_path)),
                ))
            })
            .cloned()
            .collect();
    }
}

fn is_runnable_program(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The authoring taxonomy for one skill folder: a `metadata.magician.personality`
/// block makes it a personality; otherwise the declared `skill_type` decides
/// tool vs procedure (same rules as the apps authoring classifier, minus the
/// app/package branches the loader already excluded).
fn catalog_kind(dir: &Path, manifest: &magician::magician_v2::skills::SkillManifest) -> String {
    if manifest.inferred_kind() == InferredKind::PersonalityMode {
        return "personality".to_string();
    }
    let raw = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let skill_type = tool_runtime_core::manifest_parser::parse_skill_magician_extension::<String>(
        &raw,
        "skill_type",
    )
    .ok()
    .flatten();
    match skill_type.as_deref() {
        Some("tool") => "tool".to_string(),
        _ if tool_runtime_core::manifest_parser::parse_skill_runtime_package(&raw)
            .ok()
            .flatten()
            .is_some() =>
        {
            "tool".to_string()
        },
        _ => "procedure".to_string(),
    }
}

/// Per-scope sets of installed skill names, sorted by scope label. Scope
/// enumeration misses nothing on disk because scope install IS filesystem
/// presence.
fn collect_scope_skill_sets(
    layout: &ArtifactV2Workspace,
) -> Vec<(String, std::collections::HashSet<String>)> {
    let scopes = layout.list_scope_segments_sync().unwrap_or_default();
    let mut sets: Vec<(String, std::collections::HashSet<String>)> = scopes
        .into_iter()
        .map(|(principal, workspace)| {
            let label = format!("{principal}/{workspace}");
            let names = std::fs::read_dir(layout.scope_skills_root(&principal, &workspace))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        // A preserved uninstall skeleton (config/auth kept,
                        // no SKILL.md) is NOT installed — the loader skips it.
                        .filter(|e| e.path().join("SKILL.md").exists())
                        .filter_map(|e| e.file_name().to_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (label, names)
        })
        .collect();
    sets.sort_by(|a, b| a.0.cmp(&b.0));
    sets
}

// ---------------------------------------------------------------------------
// Uninstall — remove a skill from scopes
// ---------------------------------------------------------------------------

/// Subfolders the skillshub uninstall contract keeps across remove/reinstall
/// so credentials and per-skill state survive (see
/// `skillshub/scripts/uninstall_skill_layer.py`).
const UNINSTALL_PRESERVED: [&str; 3] = ["config", "auth", ".skill-state"];

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    /// `principal/workspace` entries, same grammar as install.
    pub workspaces: Vec<String>,
    /// Full delete (no retention). Requires the preview round-trip: the
    /// first call returns a confirmation token instead of deleting.
    #[serde(default)]
    pub purge: bool,
    #[serde(default)]
    pub confirm_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UninstallResponse {
    pub removed: Vec<String>,
    pub targets: Vec<String>,
    /// Present when a purge needs its confirmation round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_token: Option<String>,
}

/// `POST /api/magician/v2/skills/{name}/uninstall`
///
/// Admin-privileged (setup token, same as install). Default removal keeps the
/// preserved subfolders in place as an inert skeleton (the loader skips a
/// folder without `SKILL.md`), so a reinstall restores config and state.
/// `purge: true` deletes everything after the preview round-trip.
pub async fn uninstall_skill_handler(
    req: HttpRequest,
    api: web::Data<SkillsApi>,
    vault: web::Data<SecretVaultApi>,
    path: web::Path<String>,
    body: web::Json<UninstallRequest>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let name = path.into_inner();
    if let Some(response) = skill_name_guard(&name) {
        return response;
    }
    if body.workspaces.is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "workspaces must specify at least one `principal/workspace` entry",
        );
    }

    let mut targets: Vec<(String, String, String, PathBuf)> = Vec::new();
    for entry in &body.workspaces {
        let (principal, workspace) = match parse_scope(entry) {
            Ok(pair) => pair,
            Err(reason) => return error(actix_web::http::StatusCode::BAD_REQUEST, &reason),
        };
        let dir = api
            .workspace_layout
            .scope_skills_root(&principal, &workspace)
            .join(&name);
        if !dir.is_dir() {
            return error(
                actix_web::http::StatusCode::NOT_FOUND,
                &format!("skill '{name}' is not installed in scope {principal}/{workspace}"),
            );
        }
        targets.push((principal, workspace, entry.clone(), dir));
    }

    if body.purge {
        let token = purge_confirm_token(&name, &targets);
        if body.confirm_token.as_deref() != Some(token.as_str()) {
            return HttpResponse::Ok().json(UninstallResponse {
                removed: Vec::new(),
                targets: targets
                    .iter()
                    .map(|(_, _, label, dir)| format!("would delete {label}: {}", dir.display()))
                    .collect(),
                confirm_token: Some(token),
            });
        }
    }

    let mut messages: Vec<String> = Vec::new();
    for (principal, workspace, label, dir) in &targets {
        let outcome = if body.purge {
            std::fs::remove_dir_all(dir).map(|_| Vec::new())
        } else {
            remove_skill_preserving(dir)
        };
        match outcome {
            Ok(preserved) => {
                api.invalidate_capability_scope(principal, workspace);
                messages.push(if body.purge {
                    format!("workspace {label}: purged {}", dir.display())
                } else {
                    format!(
                        "workspace {label}: removed {} (preserved: {})",
                        dir.display(),
                        if preserved.is_empty() {
                            "none".to_string()
                        } else {
                            preserved.join(", ")
                        }
                    )
                });
            },
            Err(e) => {
                // Destructive multi-scope op: report what already happened
                // so the operator is not left guessing.
                tracing::error!(
                    skill = %name,
                    completed = ?messages,
                    failed_scope = %label,
                    error = %e,
                    "skills_api: uninstall failed partway"
                );
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "reason": format!("uninstall from scope {label} failed: {e}"),
                    "completed": messages
                }));
            },
        }
    }

    tracing::info!(
        skill = %name,
        purge = body.purge,
        targets = ?messages,
        "skills_api: uninstall succeeded"
    );

    HttpResponse::Ok().json(UninstallResponse {
        removed: vec![name],
        targets: messages,
        confirm_token: None,
    })
}

/// Delete the skill folder while moving the contract-preserved subfolders
/// aside and restoring them afterwards, leaving an inert skeleton the loader
/// skips (no `SKILL.md`). Symlink-installed entries are safe: renames move
/// the links and `remove_dir_all` unlinks rather than recursing through them.
fn remove_skill_preserving(dir: &Path) -> std::io::Result<Vec<String>> {
    let parent = dir.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "skill dir has no parent")
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let staging = parent.join(format!(
        ".{}-uninstalling-{stamp}",
        dir.file_name().and_then(|s| s.to_str()).unwrap_or("skill")
    ));
    std::fs::create_dir_all(&staging)?;

    let mut preserved: Vec<String> = Vec::new();
    for sub in UNINSTALL_PRESERVED {
        let from = dir.join(sub);
        if from.exists() {
            std::fs::rename(&from, staging.join(sub))?;
            preserved.push(sub.to_string());
        }
    }

    match std::fs::remove_dir_all(dir) {
        Ok(()) => {},
        Err(e) => {
            // Best effort rollback so a failed remove leaves the skill loaded.
            for sub in &preserved {
                let _ = std::fs::rename(staging.join(sub), dir.join(sub));
            }
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        },
    }

    if !preserved.is_empty() {
        std::fs::create_dir_all(dir)?;
        for sub in &preserved {
            std::fs::rename(staging.join(sub), dir.join(sub))?;
        }
    }
    std::fs::remove_dir_all(&staging)?;
    Ok(preserved)
}

/// Deterministic-within-process confirmation token binding the purge to its
/// exact deletion set. The round-trip exists to prevent accidental full
/// deletes, not to add a second auth layer — the setup token already gates
/// the route.
fn purge_confirm_token(name: &str, targets: &[(String, String, String, PathBuf)]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    for (_, _, _, dir) in targets {
        dir.hash(&mut hasher);
    }
    format!("purge-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    use std::fs;

    fn write_skill(parent: &Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), body).unwrap();
    }

    fn procedure_body(name: &str) -> String {
        format!("---\nname: {name}\ndescription: a {name} procedure\n---\n# body\n")
    }

    fn personality_body(name: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: {name} persona\n\
             metadata:\n  magician:\n    personality:\n      \
             active_mode: {name}\n      voice: \"x\"\n---\n# body\n"
        )
    }

    #[test]
    fn parse_scope_accepts_principal_workspace() {
        assert_eq!(
            parse_scope("anonymous/default").unwrap(),
            ("anonymous".to_string(), "default".to_string())
        );
    }

    #[test]
    fn parse_scope_rejects_missing_workspace() {
        assert!(parse_scope("anonymous").is_err());
        assert!(parse_scope("anonymous/").is_err());
        assert!(parse_scope("/default").is_err());
        assert!(parse_scope("a/b/c").is_err());
    }

    #[test]
    fn resolve_source_accepts_path_scheme() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "review", &procedure_body("review"));
        let p = dir.path().join("review");
        let resolved = resolve_source(&format!("path:{}", p.display()), Path::new("/")).unwrap();
        assert_eq!(resolved, p);
    }

    #[test]
    fn resolve_source_rejects_relative_path() {
        let err = resolve_source("path:relative/dir", Path::new("/")).unwrap_err();
        assert!(matches!(err, SourceError::PathNotAbsolute));
    }

    #[test]
    fn resolve_source_rejects_missing_path() {
        let err = resolve_source("path:/nonexistent/skill", Path::new("/")).unwrap_err();
        assert!(matches!(err, SourceError::NotFound(_)));
    }

    #[test]
    fn resolve_source_rejects_unsupported_scheme() {
        let err = resolve_source("git:https://example.com/x.git", Path::new("/")).unwrap_err();
        assert!(matches!(err, SourceError::Unsupported(s) if s == "git"));
    }

    fn tool_body(name: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: a {name} tool\n\
             metadata:\n  magician:\n    skill_type: tool\n---\n# body\n"
        )
    }

    #[test]
    fn catalog_kind_classifies_personality_tool_and_procedure() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "persona-x", &personality_body("persona-x"));
        write_skill(dir.path(), "tool-x", &tool_body("tool-x"));
        write_skill(dir.path(), "proc-x", &procedure_body("proc-x"));
        for (name, expected) in [
            ("persona-x", "personality"),
            ("tool-x", "tool"),
            ("proc-x", "procedure"),
        ] {
            let skill_dir = dir.path().join(name);
            let manifest = validate_source(&skill_dir).unwrap();
            assert_eq!(catalog_kind(&skill_dir, &manifest), expected);
        }
    }

    #[test]
    fn collect_scope_skill_sets_lists_installed_names_per_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        let skills = layout.scope_skills_root("anonymous", "default");
        write_skill(&skills, "awk", &procedure_body("awk"));
        let sets = collect_scope_skill_sets(&layout);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "anonymous/default");
        assert!(sets[0].1.contains("awk"));
    }

    #[test]
    fn remove_skill_preserving_keeps_config_env_and_drops_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("keepme");
        write_skill(dir.path(), "keepme", &procedure_body("keepme"));
        std::fs::create_dir_all(skill.join("config")).unwrap();
        std::fs::write(skill.join("config/.env"), "SECRET=1\n").unwrap();
        std::fs::create_dir_all(skill.join(".skill-state")).unwrap();
        std::fs::write(skill.join(".skill-state/run.json"), "{}").unwrap();
        std::fs::write(skill.join("script.sh"), "echo x\n").unwrap();

        let preserved = remove_skill_preserving(&skill).unwrap();
        assert!(preserved.contains(&"config".to_string()));
        assert!(preserved.contains(&".skill-state".to_string()));
        // The folder is now an inert skeleton: no SKILL.md → loader skips it.
        assert!(!skill.join("SKILL.md").exists());
        assert!(!skill.join("script.sh").exists());
        assert_eq!(
            std::fs::read_to_string(skill.join("config/.env")).unwrap(),
            "SECRET=1\n"
        );
        assert!(skill.join(".skill-state/run.json").exists());
        // No staging leftovers beside the skill.
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .filter(|e| e
                    .file_name()
                    .to_str()
                    .map(|n| n.starts_with(".keepme-uninstalling"))
                    .unwrap_or(false))
                .count(),
            0
        );
    }

    #[test]
    fn skill_name_is_safe_rejects_traversal_and_odd_chars() {
        assert!(skill_name_is_safe("awk"));
        assert!(skill_name_is_safe("my-skill.v2_beta"));
        // actix fully decodes path params, so these arrive as literal
        // names after %2F/%2E decoding — all must be refused.
        assert!(!skill_name_is_safe(".."));
        assert!(!skill_name_is_safe("."));
        assert!(!skill_name_is_safe(""));
        assert!(!skill_name_is_safe("a/b"));
        assert!(!skill_name_is_safe("a\\b"));
        assert!(!skill_name_is_safe("../../skills"));
        assert!(!skill_name_is_safe("space name"));
        assert!(!skill_name_is_safe("sym;link"));
    }

    #[test]
    fn install_atomic_carries_preserved_state_across_reinstall() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fresh");
        write_skill(&source, "again", &procedure_body("again"));
        let dest = dir.path().join("dest");
        write_skill(&dest, "again", &procedure_body("again"));
        // Simulate a preserved uninstall skeleton: config kept, SKILL.md still
        // present from the old install (uninstall removes it; reinstall must
        // not destroy what remains either way).
        std::fs::create_dir_all(dest.join("config")).unwrap();
        std::fs::write(dest.join("config/.env"), "SECRET=keep\n").unwrap();
        std::fs::create_dir_all(dest.join(".skill-state")).unwrap();

        install_atomic(&source, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("config/.env")).unwrap(),
            "SECRET=keep\n"
        );
        assert!(dest.join(".skill-state").is_dir());
    }

    #[test]
    fn collect_scope_skill_sets_skips_uninstall_skeletons() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        let skills = layout.scope_skills_root("anonymous", "default");
        write_skill(&skills, "real", &procedure_body("real"));
        // Skeleton: config kept, no SKILL.md — NOT installed.
        let skeleton = skills.join("ghost");
        std::fs::create_dir_all(skeleton.join("config")).unwrap();
        std::fs::write(skeleton.join("config/.env"), "X=1\n").unwrap();
        let sets = collect_scope_skill_sets(&layout);
        assert!(sets[0].1.contains("real"));
        assert!(!sets[0].1.contains("ghost"));
    }

    #[test]
    fn skill_env_example_keys_extracts_commented_and_plain_keys() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join(".env.example"),
            "# Kapso credentials\nKAPSO_API_KEY=your-key\n# KAPSO_PHONE_NUMBER_ID=123\n\n# prose note\n",
        )
        .unwrap();
        assert_eq!(
            skill_env_example_keys(dir.path()),
            vec![
                "KAPSO_API_KEY".to_string(),
                "KAPSO_PHONE_NUMBER_ID".to_string()
            ]
        );
    }

    #[test]
    fn purge_confirm_token_binds_to_the_deletion_set() {
        let a = purge_confirm_token(
            "awk",
            &[(
                "anonymous".into(),
                "default".into(),
                "anonymous/default".into(),
                PathBuf::from("/r/scopes/anonymous/default/skills/awk"),
            )],
        );
        let same = purge_confirm_token(
            "awk",
            &[(
                "anonymous".into(),
                "default".into(),
                "anonymous/default".into(),
                PathBuf::from("/r/scopes/anonymous/default/skills/awk"),
            )],
        );
        let other = purge_confirm_token(
            "awk",
            &[(
                "anonymous".into(),
                "other".into(),
                "anonymous/other".into(),
                PathBuf::from("/r/scopes/anonymous/other/skills/awk"),
            )],
        );
        assert_eq!(a, same);
        assert_ne!(a, other);
    }

    #[test]
    fn resolve_source_finds_skillshub_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let skillshub = dir.path().join("skillshub");
        fs::create_dir_all(&skillshub).unwrap();
        write_skill(&skillshub, "awk", &procedure_body("awk"));
        let resolved = resolve_source("skillshub:awk", dir.path()).unwrap();
        assert_eq!(resolved, skillshub.join("awk"));
    }

    #[test]
    fn validate_source_accepts_well_formed_procedure() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "review", &procedure_body("review"));
        let manifest = validate_source(&dir.path().join("review")).unwrap();
        assert_eq!(manifest.name, "review");
        assert_eq!(manifest.inferred_kind(), InferredKind::Procedure);
    }

    #[test]
    fn validate_source_accepts_personality_mode() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "witty", &personality_body("witty"));
        let manifest = validate_source(&dir.path().join("witty")).unwrap();
        assert_eq!(manifest.inferred_kind(), InferredKind::PersonalityMode);
    }

    #[test]
    fn validate_source_rejects_name_dir_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "actual-dir", &procedure_body("different-name"));
        let err = validate_source(&dir.path().join("actual-dir")).unwrap_err();
        assert!(
            err.contains("does not match parent dir") || err.contains("no skill named"),
            "expected name/dir-mismatch error, got: {err}"
        );
    }

    #[test]
    fn install_atomic_overwrites_existing_dest() {
        let src_root = tempfile::tempdir().unwrap();
        write_skill(src_root.path(), "awk", &procedure_body("awk"));
        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("awk");
        // Pre-existing entry that should be overwritten.
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("STALE"), "old").unwrap();
        install_atomic(&src_root.path().join("awk"), &dest).unwrap();
        assert!(dest.join("SKILL.md").is_file());
        assert!(!dest.join("STALE").exists());
    }

    #[test]
    fn install_atomic_preserves_executable_bit() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let src_root = tempfile::tempdir().unwrap();
            let src = src_root.path().join("awk");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("SKILL.md"), procedure_body("awk")).unwrap();
            let scripts = src.join("scripts");
            fs::create_dir_all(&scripts).unwrap();
            let script = scripts.join("run.sh");
            fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

            let dest_root = tempfile::tempdir().unwrap();
            let dest = dest_root.path().join("awk");
            install_atomic(&src, &dest).unwrap();
            let installed_script = dest.join("scripts").join("run.sh");
            let mode = fs::metadata(&installed_script)
                .unwrap()
                .permissions()
                .mode();
            // mode includes file type bits in higher bits; mask to permission bits.
            assert_eq!(mode & 0o777, 0o755, "executable bit not preserved");
        }
    }

    #[test]
    fn edit_agent_tools_list_adds_to_existing_block() {
        let yaml = "agent_id: pa\nname: PA\ntools:\n- awk\n- jq\nother: value\n";
        let (out, tools) = edit_agent_tools_list(yaml, "browser", "add").expect("add succeeds");
        assert!(out.contains("- awk\n- jq\n- browser\n"));
        assert!(out.contains("other: value\n"));
        assert_eq!(
            tools,
            vec!["awk".to_string(), "jq".to_string(), "browser".to_string()]
        );
    }

    #[test]
    fn edit_agent_tools_list_removes_existing_entry() {
        let yaml = "tools:\n- awk\n- jq\n- browser\nother: value\n";
        let (out, tools) = edit_agent_tools_list(yaml, "jq", "remove").expect("remove succeeds");
        assert!(!out.contains("- jq\n"));
        assert!(out.contains("- awk\n"));
        assert!(out.contains("- browser\n"));
        assert_eq!(tools, vec!["awk".to_string(), "browser".to_string()]);
    }

    #[test]
    fn edit_agent_tools_list_idempotent_on_add_when_present() {
        let yaml = "tools:\n- awk\n- jq\n";
        let (out, tools) = edit_agent_tools_list(yaml, "awk", "add").expect("add idempotent");
        assert_eq!(out, yaml);
        assert_eq!(tools, vec!["awk".to_string(), "jq".to_string()]);
    }

    #[test]
    fn edit_agent_tools_list_idempotent_on_remove_when_absent() {
        let yaml = "tools:\n- awk\n";
        let (out, tools) =
            edit_agent_tools_list(yaml, "browser", "remove").expect("remove idempotent");
        assert_eq!(out, yaml);
        assert_eq!(tools, vec!["awk".to_string()]);
    }

    #[test]
    fn edit_agent_tools_list_preserves_tail_block() {
        let yaml = "tools:\n- awk\nexcluded_tools: []\nconstraints:\n  max: 40\n";
        let (out, _) = edit_agent_tools_list(yaml, "jq", "add").expect("add preserves tail");
        // The change must NOT touch `excluded_tools:` or `constraints:`.
        assert!(out.contains("excluded_tools: []\n"));
        assert!(out.contains("constraints:\n  max: 40\n"));
        assert!(out.contains("- awk\n- jq\n"));
    }

    #[test]
    fn edit_agent_tools_list_rejects_yaml_without_tools_block() {
        let yaml = "agent_id: pa\nname: PA\n";
        let err = edit_agent_tools_list(yaml, "awk", "add").unwrap_err();
        assert!(err.contains("no top-level `tools:` block"));
    }

    #[test]
    fn copy_dir_recursive_refuses_symlinks() {
        #[cfg(unix)]
        {
            let src_root = tempfile::tempdir().unwrap();
            let src = src_root.path().join("evil");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("SKILL.md"), procedure_body("evil")).unwrap();
            std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
            let dest_root = tempfile::tempdir().unwrap();
            let dest = dest_root.path().join("evil");
            let err = copy_dir_recursive(&src, &dest).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        }
    }

    // ----------------------------------------------------------------
    // HTTP integration tests (route wiring + auth integration)
    //
    // These tests prove that the skills_api routes are correctly wired
    // through actix-web and that the X-Magician-Setup-Token guard is
    // enforced at the HTTP layer, not just inside individual handlers.
    // Happy-path filesystem behaviour is covered by the unit tests
    // above (install_atomic, edit_agent_tools_list, parse_scope, etc.) —
    // these tests close the gap between "the helpers work" and "the
    // helpers are reachable through the live HTTP surface."
    // ----------------------------------------------------------------

    use actix_web::{http::StatusCode, test as actix_test, web::Data, App};

    use magician::magician_v2::secrets::{InMemoryKeyProvider, SecretStore};

    fn http_test_v3_root(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("magician_data_v3")
    }

    fn http_test_skills_api(dir: &tempfile::TempDir) -> SkillsApi {
        // Repo root is one level up from `magician_data_v3/`. For the
        // tests we don't need real skillshub content; the install
        // endpoint validation runs on the source folder we point at.
        let storage_root = http_test_v3_root(dir);
        let repo_root = dir.path().to_path_buf();
        SkillsApi::new(storage_root, repo_root)
    }

    fn http_test_vault_api(dir: &tempfile::TempDir) -> SecretVaultApi {
        let scoped_secrets = http_test_v3_root(dir).join("scopes/test/test/secrets");
        let store = std::sync::Arc::new(SecretStore::open(
            Box::new(InMemoryKeyProvider::new()),
            scoped_secrets,
        ));
        SecretVaultApi::new_for_tests(http_test_v3_root(dir), store)
    }

    #[test]
    fn runtime_package_without_explicit_skill_type_is_catalogued_as_a_tool() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("skillshub")
            .join("arxiv-search");
        let manifest = validate_source(&directory).expect("repository skill is valid");

        assert_eq!(catalog_kind(&directory, &manifest), "tool");
    }

    #[test]
    fn skill_catalog_resolves_manifest_declared_setup_drivers() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("skillshub");
        let scopes = Vec::new();

        let gmail = catalog_entry_from_dir(&root.join("gmail"), "gmail", "skillshub", &scopes)
            .expect("gmail catalog entry");
        let gmail_setup = gmail
            .auth
            .and_then(|auth| auth.setup)
            .expect("gmail setup descriptor");
        assert_eq!(gmail_setup.profile.as_deref(), Some("work"));
        assert!(matches!(gmail_setup.driver, SetupDriver::ManagedBot { .. }));

        let swiggy =
            catalog_entry_from_dir(&root.join("swiggy-mcp"), "swiggy-mcp", "skillshub", &scopes)
                .expect("Swiggy catalog entry");
        assert!(matches!(
            swiggy
                .auth
                .and_then(|auth| auth.setup)
                .map(|setup| setup.driver),
            Some(SetupDriver::GovernedOauth)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn binary_readiness_requires_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("tempdir");
        let binary = directory.path().join("tool");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("binary fixture");
        assert!(!is_runnable_program(&binary));

        let mut permissions = std::fs::metadata(&binary)
            .expect("binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("mark executable");
        assert!(is_runnable_program(&binary));
    }

    #[test]
    fn governed_oauth_setup_uses_the_reviewed_default_endpoint_and_request_scope() {
        let directory = tempfile::tempdir().expect("tempdir");
        let skill_dir = http_test_v3_root(&directory).join("scopes/test/test/skills/swiggy-mcp");
        std::fs::create_dir_all(&skill_dir).expect("skill directory");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("skillshub")
            .join("swiggy-mcp")
            .join("SKILL.md");
        std::fs::copy(source, skill_dir.join("SKILL.md")).expect("install skill fixture");

        let api = http_test_skills_api(&directory);
        let vault = http_test_vault_api(&directory);
        let broadcaster =
            Arc::new(magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(4));
        let oauth_api =
            crate::mcp_oauth_api::McpOAuthApi::new_with_callback_port(broadcaster, 4317);
        let request = actix_test::TestRequest::default()
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .to_http_request();

        let (coordinator, scope) =
            skill_oauth_coordinator(&request, &api, &vault, &oauth_api, "swiggy-mcp")
                .expect("build setup coordinator");
        assert_eq!(scope.principal.as_str(), "test");
        assert_eq!(scope.workspace.as_str(), "test");
        assert_eq!(coordinator.profile_key().provider.as_str(), "swiggy-mcp");
        assert_eq!(coordinator.profile_key().alias.as_str(), "personal");
        let CredentialProfileBinding::McpOauth {
            resource_url,
            authorization_issuer,
        } = &coordinator.profile_key().binding
        else {
            panic!("expected MCP OAuth binding");
        };
        assert_eq!(resource_url.as_str(), "https://mcp.swiggy.com/food");
        assert_eq!(authorization_issuer.as_str(), "https://mcp.swiggy.com/auth");
    }

    #[actix_rt::test]
    async fn http_install_without_setup_token_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(Data::new(http_test_skills_api(&dir)))
                .app_data(Data::new(http_test_vault_api(&dir)))
                .configure(configure_skills_routes),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/skills/install")
            .set_json(serde_json::json!({
                "source": "path:/tmp/some-skill",
                "target": {"workspaces": ["anonymous/default"]}
            }))
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_rt::test]
    async fn http_allow_for_agent_without_setup_token_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(Data::new(http_test_skills_api(&dir)))
                .app_data(Data::new(http_test_vault_api(&dir)))
                .configure(configure_skills_routes),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/skills/awk/allow-for-agent")
            .set_json(serde_json::json!({
                "agent_id": "personal-assistant",
                "action": "add"
            }))
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_rt::test]
    async fn http_list_is_unauthenticated_and_returns_built_ins_when_no_skills_installed() {
        // GET /skills is intentionally not gated by setup-token (per
        // skills-spec.md: listing what's installed isn't privileged).
        // Even when no SKILL.md skills are installed, the compiled
        // built-in capabilities ship bundled in the binary and surface
        // here. Currently 31 (22 harness state + 5 Rust providers + 3
        // specialized inner-loop + 1 composite); see
        // `embedded_compiled_pack_defs` for the canonical list.
        let dir = tempfile::tempdir().unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(Data::new(http_test_skills_api(&dir)))
                .app_data(Data::new(http_test_vault_api(&dir)))
                .configure(configure_skills_routes),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/skills").to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        let skills = body
            .get("skills")
            .and_then(|v| v.as_array())
            .expect("skills array");
        let kinds: Vec<&str> = skills
            .iter()
            .filter_map(|s| s.get("kind").and_then(|k| k.as_str()))
            .collect();
        let compiled_count = kinds.iter().filter(|k| **k == "compiled").count();
        assert!(
            compiled_count >= 20,
            "expected at least 20 compiled built-ins; got {compiled_count} in kinds {kinds:?}"
        );
        assert!(
            kinds.iter().all(|k| *k == "compiled"),
            "with no SKILL.md skills installed, every entry should be a compiled built-in; got {kinds:?}"
        );
    }

    #[actix_rt::test]
    async fn http_list_built_ins_carry_compiled_kind_and_built_in_layer() {
        let dir = tempfile::tempdir().unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(Data::new(http_test_skills_api(&dir)))
                .app_data(Data::new(http_test_vault_api(&dir)))
                .configure(configure_skills_routes),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/skills").to_request();
        let body: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let skills = body
            .get("skills")
            .and_then(|v| v.as_array())
            .expect("skills array");
        let create_agent = skills
            .iter()
            .find(|s| s.get("name").and_then(|n| n.as_str()) == Some("create_agent"))
            .expect("create_agent built-in must be in the list");
        assert_eq!(
            create_agent.get("kind").and_then(|k| k.as_str()),
            Some("compiled")
        );
        assert_eq!(
            create_agent.get("layer").and_then(|l| l.as_str()),
            Some("built-in")
        );
        assert!(
            create_agent
                .get("description")
                .and_then(|d| d.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "built-in entry must carry a non-empty description"
        );
    }

    #[actix_rt::test]
    async fn http_list_surfaces_a_freshly_installed_workspace_skill() {
        let dir = tempfile::tempdir().unwrap();
        // Author a fake skill directly into the workspace skills layer
        // to bypass the setup-token-gated install endpoint. The list
        // endpoint doesn't care how the skill arrived; it just walks
        // the filesystem.
        let workspace_skills = http_test_v3_root(&dir).join("scopes/anonymous/default/skills");
        std::fs::create_dir_all(&workspace_skills).unwrap();
        write_skill(&workspace_skills, "fixture", &procedure_body("fixture"));

        let app = actix_test::init_service(
            App::new()
                .app_data(Data::new(http_test_skills_api(&dir)))
                .app_data(Data::new(http_test_vault_api(&dir)))
                .configure(configure_skills_routes),
        )
        .await;

        // Pass the workspace context via headers so the list handler
        // walks `<scopes>/anonymous/default/skills/`.
        let req = actix_test::TestRequest::get()
            .uri("/skills")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let body: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let skills = body
            .get("skills")
            .and_then(|v| v.as_array())
            .expect("skills array");
        let names: Vec<&str> = skills
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(
            names.contains(&"fixture"),
            "expected fixture skill in list; got {names:?}"
        );
    }
}
