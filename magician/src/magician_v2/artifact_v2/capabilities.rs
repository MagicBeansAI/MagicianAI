use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::magician_v2::execution::capability::CapabilityPackDefinition;

use super::service::ArtifactV2Error;
use super::workspace::{ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE};

fn absolutize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityScopePaths {
    pub principal: String,
    pub workspace: String,
    /// `<scope>/` — the scope's runtime tree root. Pre-refactor this
    /// was `<scope>/capabilities/`; bots/auth/workdirs are now siblings
    /// of skills/ directly at scope root. Field name kept for callsite
    /// stability; the `{scope_capabilities_root}` template var
    /// substitutes to this path.
    pub capabilities_root: PathBuf,
    pub bots_root: PathBuf,
    pub auth_root: PathBuf,
    pub workdirs_root: PathBuf,
    pub home_root: PathBuf,
    /// Repo-relative path to `skillshub/node_modules/.bin`. The root
    /// npm workspace at `skillshub/` hoists every npm-vendored skill
    /// binary here (gws, telegraf, tgcli, wu, …). The dispatcher prepends
    /// this to skill subprocess PATH so `tool_schema.yaml` can call them
    /// bare (`gws`, `telegraf`, …); in-process spawns (e.g. bot manager)
    /// still use this field to construct an absolute path.
    pub node_modules_bin: PathBuf,
    /// Repo-relative path to `skillshub/.node/bin`. Populated by
    /// `make -C skillshub setup-node`, which unpacks the official Node
    /// 22 LTS tarball. Prepended to skill subprocess PATH so `node`,
    /// `npm`, `npx`, plus the corepack shims (`pnpm`, `yarn`) all
    /// resolve from the project-local install instead of host brew /
    /// nvm / fnm.
    pub node_bin: PathBuf,
    /// Repo-relative path to `skillshub/.venv/bin`. Skill subprocesses
    /// get this prepended to PATH so plain `python3` resolves to the
    /// venv interpreter (Pillow, fpdf2, google-genai, …) without the
    /// skill needing to know the absolute path.
    pub venv_bin: PathBuf,
}

impl CapabilityScopePaths {
    /// Substitute the live skill-template variables. The set is
    /// intentionally minimal — every entry below is referenced by at
    /// least one shipped skill (see `skillshub/`). Removed-as-dead
    /// alongside the capabilities/ umbrella refactor:
    ///   `{scope_node_runtime_root}`, `{scope_capability_config_root}`,
    ///   `{shared_node_tools_root}`, `{shared_node_bots_root}`,
    ///   `{shared_python_tools_root}`, `{system_shared_tools_root}`,
    ///   `{skillshub_node_modules_bin}` (the cli_template dispatcher now
    ///   prepends skillshub/node_modules/.bin to subprocess PATH, so skills
    ///   spell npm-vendored binaries bare: `gws`, `telegraf`, `tgcli`, …).
    pub fn apply_vars(&self, value: &str) -> String {
        let replacements = [
            (
                "{scope_capabilities_root}",
                self.capabilities_root.to_string_lossy().to_string(),
            ),
            (
                "{scope_capability_auth_root}",
                self.auth_root.to_string_lossy().to_string(),
            ),
            (
                "{scope_capability_workdir_root}",
                self.workdirs_root.to_string_lossy().to_string(),
            ),
        ];

        replacements
            .into_iter()
            .fold(value.to_string(), |acc, (needle, replacement)| {
                acc.replace(needle, &replacement)
            })
    }

    /// Build a PATH that prepends the scope's tool-bin dirs to `parent_path`,
    /// so `gws`/`node`/`python3` resolve — the same augmentation skill
    /// subprocesses get. `extra_bin` is an optional leading dir (a per-skill
    /// `bin/`). Only dirs that exist are prepended. Returns None if none exist
    /// (caller then leaves PATH as the parent).
    ///
    /// Order MUST match the cli_template dispatcher's inline construction:
    /// `[extra_bin?, venv_bin, node_modules_bin, node_bin]`, each included
    /// only when the directory exists, with `parent_path` appended last.
    pub fn subprocess_bin_path(
        &self,
        extra_bin: Option<&std::path::Path>,
        parent_path: &str,
    ) -> Option<String> {
        let prefix_dirs: Vec<&Path> = extra_bin
            .into_iter()
            .chain([
                self.venv_bin.as_path(),
                self.node_modules_bin.as_path(),
                self.node_bin.as_path(),
            ])
            .filter(|p| p.exists())
            .collect();
        if prefix_dirs.is_empty() {
            return None;
        }
        let mut segments: Vec<String> = prefix_dirs
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        if !parent_path.is_empty() {
            segments.push(parent_path.to_string());
        }
        Some(segments.join(":"))
    }
}

/// Resolve a capability template string (probe / check_command /
/// setup_command) the same way the cli_template dispatcher and the
/// provider auth path do, so callers that read these strings outside the
/// dispatcher — notably the per-task preflight gate — don't run or render
/// broken literals like `{scope_capability_auth_root}` / `{account}`.
///
/// Two substitution passes, mirroring `CliTemplateDispatcher::interpolate`:
///   1. `scope_paths.apply_vars` for the `{scope_*}` skill-template vars
///      (no-op when `scope_paths` is `None`, e.g. non-scoped / test
///      callers, or when the string has no such placeholders).
///   2. `{param}` placeholders resolved from the pack's parameter
///      **defaults** (there are no live call args at preflight time), e.g.
///      `{account}` → its declared default (`work` for the gws packs).
///
/// Fail-open by construction: any placeholder without a scope var or a
/// parameter default is left verbatim rather than blanked or panicking,
/// so a partial config never turns into a silently mangled command.
pub fn resolve_probe_string(
    pack: &CapabilityPackDefinition,
    scope_paths: Option<&CapabilityScopePaths>,
    value: &str,
) -> String {
    let mut resolved = match scope_paths {
        Some(paths) => paths.apply_vars(value),
        None => value.to_string(),
    };

    for def in &pack.parameters {
        let Some(default) = def.default.as_deref().filter(|d| !d.is_empty()) else {
            continue;
        };
        for name in std::iter::once(def.name.as_str()).chain(def.aliases.iter().map(String::as_str))
        {
            let placeholder = format!("{{{name}}}");
            if resolved.contains(&placeholder) {
                resolved = resolved.replace(&placeholder, default);
            }
        }
    }

    resolved
}

#[derive(Debug, Error)]
pub enum CapabilityWorkspaceError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("artifact workspace error: {0}")]
    ArtifactWorkspace(#[from] ArtifactV2Error),
}

#[derive(Debug, Clone)]
pub struct CapabilityWorkspaceManager {
    workspace_layout: ArtifactV2Workspace,
    repo_root: PathBuf,
    /// Parsed scope catalogs, one per scope, served until their sources are
    /// digested again. Shared by every clone of this manager so a run's
    /// bootstrap — which asks for the catalog once per delegate target —
    /// parses the skills on disk once, not once per ask.
    scope_pack_cache: Arc<RwLock<HashMap<(String, String), Arc<ScopePackCacheEntry>>>>,
    scope_pack_cache_metrics: Arc<ScopePackCacheMetrics>,
}

/// How long a cached scope catalog is served before its sources are digested
/// again. The same cadence as the scoped capability cache's background
/// revision check, so an out-of-band skill edit reaches both within one
/// interval; canonical mutation paths invalidate eagerly instead of waiting.
const SCOPE_PACK_SOURCE_RECHECK_SECONDS: u64 = 30;

const SCOPE_PACK_SOURCE_REVISION_VERSION: &str = "scope-pack-source-revision.v1";

#[derive(Debug)]
struct ScopePackCacheEntry {
    source_revision: String,
    set: Arc<ScopedPackDefinitionSet>,
    checked_at_epoch_ms: AtomicU64,
}

#[derive(Debug, Default)]
struct ScopePackCacheMetrics {
    hits: AtomicU64,
    builds: AtomicU64,
    revalidations: AtomicU64,
    invalidations: AtomicU64,
}

/// Counters for the per-scope pack catalog cache. `hits` are asks served
/// from memory without digesting the sources; `revalidations` digested the
/// sources and found the served catalog current; `builds` parsed the skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePackCacheStatus {
    pub entry_count: usize,
    pub hits: u64,
    pub builds: u64,
    pub revalidations: u64,
    pub invalidations: u64,
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Digest a skills root the way the scoped capability cache does: every
/// skill's `SKILL.md` and `tool_schema.yaml` — the governed contract boundary
/// and the legacy contract the loader still reads. A change to either rolls
/// the catalog; anything else under a skill (bins, deps, env) does not.
pub(crate) fn hash_skill_catalog(
    hasher: &mut blake3::Hasher,
    label: &str,
    root: &Path,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    hasher.update(label.as_bytes());
    hasher.update(&[0]);
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update(&[0]);

    let mut skill_dirs = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<PathBuf>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            hasher.update(b"<missing>");
            return Ok(());
        },
        Err(error) => {
            return Err(error).with_context(|| format!("reading skills root {}", root.display()));
        },
    };
    skill_dirs.sort();
    for skill_dir in skill_dirs {
        let skill_name = skill_dir
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        hasher.update(skill_name.as_bytes());
        hasher.update(&[0]);
        for file_name in ["SKILL.md", "tool_schema.yaml"] {
            let source = skill_dir.join(file_name);
            let resolved = crate::magician_v2::skills::path_rewrite::resolve_skill_path(&source);
            if !resolved.is_file() {
                continue;
            }
            let bytes = fs::read(&resolved).with_context(|| {
                format!("reading capability revision source {}", resolved.display())
            })?;
            hasher.update(file_name.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
    }
    Ok(())
}

/// Scope pack definitions together with the names whose selected definition
/// came from the binary's embedded fallback. Keeping this provenance beside
/// the selected definitions prevents a byte-identical skill override from
/// inheriting built-in Apps authority merely because it parses the same way.
#[derive(Debug, Clone)]
pub(crate) struct ScopedPackDefinitionSet {
    pub(crate) definitions: Vec<CapabilityPackDefinition>,
    pub(crate) embedded_fallback_names: HashSet<String>,
    /// Skills (by directory name) that produced at least one pack, from the
    /// scope root and the extra roots.
    pub(crate) loaded_skills: Vec<String>,
    /// Skills the loader could not turn into a capability. The scoped
    /// capability cache refuses to replace a catalog with one that newly
    /// fails a skill the current catalog serves.
    pub(crate) load_failures: Vec<crate::magician_v2::execution::SkillLoadFailure>,
}

impl CapabilityWorkspaceManager {
    pub fn new(workspace_layout: ArtifactV2Workspace, repo_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_layout: ArtifactV2Workspace::new(absolutize_path(
                workspace_layout.base_root(),
            )),
            repo_root: absolutize_path(repo_root),
            scope_pack_cache: Arc::new(RwLock::new(HashMap::new())),
            scope_pack_cache_metrics: Arc::new(ScopePackCacheMetrics::default()),
        }
    }

    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Source-of-truth location for the bot daemons + `bot_configs.yaml`
    /// seed. The bot code lives under `skillshub/bots/` post-relocation;
    /// this used to be `magician_data_v3/system/capability_templates/bots/`
    /// but bots aren't runtime data — they're committed source under
    /// skillshub/.
    pub fn skillshub_bots_root(&self) -> PathBuf {
        self.repo_root.join("skillshub").join("bots")
    }

    pub fn skillshub_bot_configs_path(&self) -> PathBuf {
        self.skillshub_bots_root().join("bot_configs.yaml")
    }

    pub fn scope_paths(&self, principal: &str, workspace: &str) -> CapabilityScopePaths {
        CapabilityScopePaths {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            capabilities_root: self
                .workspace_layout
                .capabilities_root(principal, workspace),
            bots_root: self
                .workspace_layout
                .capability_bots_root(principal, workspace),
            auth_root: self
                .workspace_layout
                .capability_auth_root(principal, workspace),
            workdirs_root: crate::magician_v2::subprocess_owners::workdirs_root(
                &self.workspace_layout,
                principal,
                workspace,
            ),
            home_root: crate::magician_v2::subprocess_owners::workdirs_root(
                &self.workspace_layout,
                principal,
                workspace,
            )
            .join("home"),
            node_modules_bin: self
                .repo_root
                .join("skillshub")
                .join("node_modules")
                .join(".bin"),
            node_bin: self.repo_root.join("skillshub").join(".node").join("bin"),
            venv_bin: self.repo_root.join("skillshub").join(".venv").join("bin"),
        }
    }

    pub fn default_scope_paths(&self) -> CapabilityScopePaths {
        self.scope_paths(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
    }

    pub fn scoped_bot_configs_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace_layout
            .capability_bot_configs_path(principal, workspace)
    }

    pub fn load_pack_defs_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<CapabilityPackDefinition> {
        self.load_pack_defs_for_scope_with_provenance(principal, workspace)
            .definitions
    }

    /// The scope catalog, served from memory until its sources are digested
    /// again (every [`SCOPE_PACK_SOURCE_RECHECK_SECONDS`]) or the scope is
    /// invalidated. A caller that already knows the sources changed — the
    /// scoped capability cache building a candidate to validate — must read
    /// the disk through [`Self::load_pack_defs_for_scope_fresh`] instead.
    pub(crate) fn load_pack_defs_for_scope_with_provenance(
        &self,
        principal: &str,
        workspace: &str,
    ) -> ScopedPackDefinitionSet {
        let key = (principal.to_string(), workspace.to_string());
        let now = now_epoch_ms();
        let cached = self
            .scope_pack_cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned();
        if let Some(entry) = cached.as_ref() {
            let checked_at = entry.checked_at_epoch_ms.load(Ordering::Acquire);
            if now.saturating_sub(checked_at) < SCOPE_PACK_SOURCE_RECHECK_SECONDS * 1_000 {
                self.scope_pack_cache_metrics
                    .hits
                    .fetch_add(1, Ordering::Relaxed);
                return (*entry.set).clone();
            }
        }
        let source_revision = match self.scope_pack_source_revision(principal, workspace) {
            Ok(revision) => revision,
            Err(error) => {
                // A source that cannot be digested cannot be trusted from
                // memory either; read the disk and cache nothing.
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[CAPABILITY] scope catalog sources could not be digested; loading uncached"
                );
                return self.load_pack_defs_for_scope_uncached(principal, workspace);
            },
        };
        if let Some(entry) = cached {
            if entry.source_revision == source_revision {
                entry.checked_at_epoch_ms.store(now, Ordering::Release);
                self.scope_pack_cache_metrics
                    .revalidations
                    .fetch_add(1, Ordering::Relaxed);
                return (*entry.set).clone();
            }
        }
        let set = self.load_pack_defs_for_scope_uncached(principal, workspace);
        self.store_scope_pack_cache_entry(key, source_revision, set.clone(), now);
        set
    }

    /// Read the scope catalog from disk regardless of the trust window, and
    /// make that read the served catalog.
    pub(crate) fn load_pack_defs_for_scope_fresh(
        &self,
        principal: &str,
        workspace: &str,
    ) -> ScopedPackDefinitionSet {
        let set = self.load_pack_defs_for_scope_uncached(principal, workspace);
        if let Ok(source_revision) = self.scope_pack_source_revision(principal, workspace) {
            self.store_scope_pack_cache_entry(
                (principal.to_string(), workspace.to_string()),
                source_revision,
                set.clone(),
                now_epoch_ms(),
            );
        }
        set
    }

    fn store_scope_pack_cache_entry(
        &self,
        key: (String, String),
        source_revision: String,
        set: ScopedPackDefinitionSet,
        checked_at: u64,
    ) {
        self.scope_pack_cache_metrics
            .builds
            .fetch_add(1, Ordering::Relaxed);
        self.scope_pack_cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                Arc::new(ScopePackCacheEntry {
                    source_revision,
                    set: Arc::new(set),
                    checked_at_epoch_ms: AtomicU64::new(checked_at),
                }),
            );
    }

    /// Drop one scope's cached catalog so the next ask reads the disk.
    /// Returns whether anything was cached.
    pub fn invalidate_scope_pack_defs(&self, principal: &str, workspace: &str) -> bool {
        let removed = self
            .scope_pack_cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(principal.to_string(), workspace.to_string()))
            .is_some();
        if removed {
            self.scope_pack_cache_metrics
                .invalidations
                .fetch_add(1, Ordering::Relaxed);
        }
        removed
    }

    /// Drop every scope's cached catalog. Returns how many were cached.
    pub fn clear_scope_pack_cache(&self) -> usize {
        let mut cache = self
            .scope_pack_cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed = cache.len();
        cache.clear();
        drop(cache);
        if removed > 0 {
            self.scope_pack_cache_metrics
                .invalidations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn scope_pack_cache_status(&self) -> ScopePackCacheStatus {
        let entry_count = self
            .scope_pack_cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        let metrics = &self.scope_pack_cache_metrics;
        ScopePackCacheStatus {
            entry_count,
            hits: metrics.hits.load(Ordering::Relaxed),
            builds: metrics.builds.load(Ordering::Relaxed),
            revalidations: metrics.revalidations.load(Ordering::Relaxed),
            invalidations: metrics.invalidations.load(Ordering::Relaxed),
        }
    }

    /// Age one scope's cached catalog past its trust window so the next ask
    /// digests the sources again, without waiting the interval out.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn expire_scope_pack_cache_for_test(&self, principal: &str, workspace: &str) {
        if let Some(entry) = self
            .scope_pack_cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(principal.to_string(), workspace.to_string()))
        {
            entry.checked_at_epoch_ms.store(0, Ordering::Release);
        }
    }

    /// Digest of everything the scope catalog is parsed from: the scope's
    /// skills, the configured extra skills roots, and the scope's Task Recipe
    /// catalog. Embedded packs are compiled into the binary and need no digest.
    fn scope_pack_source_revision(
        &self,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<String> {
        use anyhow::Context as _;

        let mut hasher = blake3::Hasher::new();
        hasher.update(SCOPE_PACK_SOURCE_REVISION_VERSION.as_bytes());
        hasher.update(principal.as_bytes());
        hasher.update(&[0]);
        hasher.update(workspace.as_bytes());
        hash_skill_catalog(
            &mut hasher,
            "scope",
            &self
                .workspace_layout
                .scope_skills_root(principal, workspace),
        )?;
        for (index, extra) in crate::magician_v2::config_extras::extra_skills_dirs()
            .iter()
            .enumerate()
        {
            hash_skill_catalog(&mut hasher, &format!("extra:{index}"), extra)?;
        }
        let recipe_catalog =
            crate::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
                &self.workspace_layout,
                principal,
                workspace,
            )
            .catalog_path();
        hasher.update(b"recipes");
        match fs::read(&recipe_catalog) {
            Ok(bytes) => {
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                hasher.update(b"<missing>");
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading recipe catalog {}", recipe_catalog.display())
                });
            },
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    fn load_pack_defs_for_scope_uncached(
        &self,
        principal: &str,
        workspace: &str,
    ) -> ScopedPackDefinitionSet {
        // Scope-level loader. Mirrors `bin/magician.rs` startup priority:
        //   1. `<scope>/skills/<skill>/tool_schema.yaml` — scope-layer
        //      skills (populated by `make -C skillshub install-scope`).
        //   2. `<extra_path>/skills/<skill>/tool_schema.yaml` — each
        //      entry from `tool-runtime-config.yaml :: registry.paths`,
        //      in declared order. Lets a user shadow embedded packs
        //      from an external directory without copying into every
        //      scope.
        //   3. Embedded compiled pack defs (built into the binary).
        // Earlier wins on name collision. The legacy disk fallback at
        // `<scope>/capabilities/packs/<name>.yaml` was removed: every
        // pack is either skill-authored or compiled into the binary.
        let scope_skills_dir = self
            .workspace_layout
            .scope_skills_root(principal, workspace);
        let scope_load = crate::magician_v2::execution::load_skills_dir(&scope_skills_dir);
        let mut out = scope_load.packs;
        let mut loaded_skills = scope_load.loaded_skills;
        let mut load_failures = scope_load.failures;
        let mut already: std::collections::HashSet<String> =
            out.iter().map(|p| p.name.clone()).collect();
        // Candidate+ Task Recipes are scoped compiled-provider aliases. Load
        // them into the same per-scope catalog build as skill-authored packs;
        // loading them into the process-global registry would leak one
        // workspace's learned API shapes into another workspace.
        let recipe_pack_store =
            crate::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
                &self.workspace_layout,
                principal,
                workspace,
            );
        if let Ok(catalog) = recipe_pack_store.load_catalog() {
            for record in catalog.packs.into_iter().filter(|record| {
                record.metadata.source
                    == crate::magician_v2::execution::CapabilityPackSource::TaskRecipe
                    && !matches!(
                        record.metadata.status,
                        crate::magician_v2::execution::CapabilityLifecycleStatus::Deprecated
                    )
            }) {
                if already.insert(record.definition.name.clone()) {
                    out.push(record.definition);
                }
            }
        }
        let mut embedded_fallback_names = HashSet::new();
        for extra_skills_dir in crate::magician_v2::config_extras::extra_skills_dirs() {
            let extra_load = crate::magician_v2::execution::load_skills_dir(&extra_skills_dir);
            loaded_skills.extend(extra_load.loaded_skills);
            load_failures.extend(extra_load.failures);
            for pack in extra_load.packs {
                if already.contains(&pack.name) {
                    continue;
                }
                already.insert(pack.name.clone());
                out.push(pack);
            }
        }
        for pack in crate::magician_v2::execution::embedded_compiled_pack_defs() {
            if already.contains(&pack.name) {
                continue;
            }
            already.insert(pack.name.clone());
            embedded_fallback_names.insert(pack.name.clone());
            out.push(pack);
        }
        ScopedPackDefinitionSet {
            definitions: out,
            embedded_fallback_names,
            loaded_skills,
            load_failures,
        }
    }

    /// Walk every existing scope under `<data_root>/scopes/<principal>/<workspace>/`
    /// and ensure each has the canonical runtime-tree skeleton. Always
    /// includes the `anonymous/default` scope. Idempotent.
    ///
    /// Setup-time concerns (skill installs, bot env templates, OAuth
    /// account dirs, npm/pip deps) are all owned by the
    /// `make -C skillshub setup-all` chain — they're operator-driven,
    /// not runtime auto-magic. This function is the *minimum* runtime
    /// bootstrap: just `mkdir`s + the bot-bundle / bot_configs.yaml
    /// dance via `materialize_scope`.
    pub fn ensure_seeded_for_existing_scopes(&self) -> Result<(), CapabilityWorkspaceError> {
        self.workspace_layout.ensure_root_sync()?;

        // Tenants only. This is the minimum runtime bootstrap — it mkdirs a
        // scope's capability workdirs and materializes its bot bundle — so
        // running it over a reserved sink scaffolds a workspace for a bucket
        // that can never run a bot.
        let mut scopes = self.workspace_layout.list_tenant_scope_segments_sync()?;
        if !scopes.iter().any(|(principal, workspace)| {
            principal == DEFAULT_SCOPE_PRINCIPAL && workspace == DEFAULT_SCOPE_WORKSPACE
        }) {
            scopes.push((
                DEFAULT_SCOPE_PRINCIPAL.to_string(),
                DEFAULT_SCOPE_WORKSPACE.to_string(),
            ));
        }

        scopes.sort();
        scopes.dedup();

        for (principal, workspace) in &scopes {
            self.materialize_scope(principal, workspace)?;
        }

        Ok(())
    }

    /// Just-in-time scope bootstrap. Invoked when a new principal/
    /// workspace is touched (via API or boot) and on first magician
    /// start. Does the minimum the runtime can't outsource to scripts:
    ///   - mkdir the canonical scope subdirs
    ///   - copy `bot_configs.yaml` from skillshub if scope is missing one
    ///   - copy each bot's bundled `dist/index.js` to scope
    ///
    /// Setup-time concerns (skill installs, per-skill `.env`, bot env
    /// files, OAuth client / accounts.txt, npm/pip deps) live in
    /// `skillshub/scripts/*.py` driven by `make -C skillshub setup-all`.
    pub fn materialize_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(), CapabilityWorkspaceError> {
        // `system/system` is the unscoped/diagnostic transport-log
        // fall-through bucket (see `transport_log.rs`). It is NOT a user
        // scope, so it must never receive user-shaped runtime config —
        // `bots/`, `auth/`, `workdirs/`, `home/`, `bot_configs.yaml`.
        // Multiple call-sites (local_tool_services, bots/scoped_runtime,
        // ui_threads/store, feed/store, execution/scoped_capability_resolver)
        // route here whenever they see *any* scope, including the system
        // one. Without this guard a diagnostic-only bucket ends up with a
        // full bot-config seed that the bot installer can then balloon
        // into hundreds of MB of node_modules.
        if principal == "system" && workspace == "system" {
            return Ok(());
        }

        let scope_paths = self.scope_paths(principal, workspace);
        fs::create_dir_all(&scope_paths.bots_root)?;
        fs::create_dir_all(&scope_paths.auth_root)?;
        fs::create_dir_all(&scope_paths.workdirs_root)?;
        fs::create_dir_all(&scope_paths.home_root)?;

        copy_file_if_present(
            &self.skillshub_bot_configs_path(),
            &self.scoped_bot_configs_path(principal, workspace),
            false,
        )?;

        // Bot bundles live as symlinks at <scope>/bots/<bot>/dist/index.js
        // back to skillshub/bots/<bot>/dist/index.js. They're materialized
        // by `skillshub/scripts/install_bot_bundles.py` (run via
        // `make install-bot-bundles` or the full `make setup-all`).
        // We used to re-sync them here on every workspace open, but with
        // symlinks the source path is always live — there's nothing to
        // re-do. Removed.
        Ok(())
    }
}

fn copy_file_if_present(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), CapabilityWorkspaceError> {
    if !source.exists() {
        return Ok(());
    }
    copy_file(source, destination, overwrite)
}

fn copy_file(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), CapabilityWorkspaceError> {
    if destination.exists() && !overwrite {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        if destination.is_dir() {
            fs::remove_dir_all(destination)?;
        } else {
            fs::remove_file(destination)?;
        }
    }
    fs::copy(source, destination)?;
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    // Note: bot bundle materialization moved out of materialize_scope
    // entirely — `<scope>/bots/<bot>/dist/index.js` is now a symlink
    // created by `skillshub/scripts/install_bot_bundles.py` at
    // setup time, not by the runtime on workspace open. The earlier
    // `materialize_scope_copies_only_bundled_dist_per_bot` test was
    // deleted alongside the removed Rust copy path.

    fn write_widget_skill(skill_root: &Path, name: &str, description: &str) {
        fs::create_dir_all(skill_root).expect("skill root");
        fs::write(
            skill_root.join("tool_schema.yaml"),
            format!(
                "name: {name}\nparameters:\n- name: input\n  required: true\n  param_type: string\n  \
                 description: Input value\nimplementation:\n  type: primitive\n  command: [\"{name}\"]\n"
            ),
        )
        .expect("tool_schema");
        fs::write(
            skill_root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n"),
        )
        .expect("SKILL.md");
    }

    fn widget_description(set: &ScopedPackDefinitionSet) -> Option<String> {
        set.definitions
            .iter()
            .find(|pack| pack.name == "widget")
            .and_then(|pack| pack.description.clone())
    }

    /// A run's bootstrap asks for the scope catalog once per delegate target —
    /// dozens of times — and every ask used to re-parse every skill on disk
    /// (14–25 s of a 70 s spawn). The catalog is parsed once per source
    /// revision; the asks in between are memory hits.
    #[test]
    fn scope_pack_defs_are_parsed_once_per_source_revision() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        let skill_root = layout.scope_skills_root("owner", "default").join("widget");
        write_widget_skill(&skill_root, "widget", "first revision");

        let first = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&first).as_deref(),
            Some("first revision")
        );
        for _ in 0..40 {
            let again = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
            assert_eq!(
                widget_description(&again).as_deref(),
                Some("first revision")
            );
        }
        let status = manager.scope_pack_cache_status();
        assert_eq!(status.builds, 1, "{status:?}");
        assert_eq!(status.hits, 40, "{status:?}");
    }

    /// An out-of-band edit reaches the catalog once its sources are digested
    /// again — the same cadence the scoped capability cache's revision check
    /// keeps — and a canonical mutation path reaches it at once by
    /// invalidating.
    #[test]
    fn a_skill_edit_reaches_the_catalog_on_revalidation_or_invalidation() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        let skill_root = layout.scope_skills_root("owner", "default").join("widget");
        write_widget_skill(&skill_root, "widget", "first revision");
        let first = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&first).as_deref(),
            Some("first revision")
        );

        write_widget_skill(&skill_root, "widget", "second revision");
        // Inside the trust window the cached catalog is still served.
        let cached = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&cached).as_deref(),
            Some("first revision")
        );

        // Past the window the sources are digested again and the edit lands.
        manager.expire_scope_pack_cache_for_test("owner", "default");
        let revalidated = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&revalidated).as_deref(),
            Some("second revision")
        );
        assert_eq!(manager.scope_pack_cache_status().builds, 2);

        // An unchanged catalog past the window is revalidated, not rebuilt.
        manager.expire_scope_pack_cache_for_test("owner", "default");
        let unchanged = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&unchanged).as_deref(),
            Some("second revision")
        );
        let status = manager.scope_pack_cache_status();
        assert_eq!(status.builds, 2, "{status:?}");
        assert_eq!(status.revalidations, 1, "{status:?}");

        // Invalidation is immediate.
        write_widget_skill(&skill_root, "widget", "third revision");
        assert!(manager.invalidate_scope_pack_defs("owner", "default"));
        let invalidated = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&invalidated).as_deref(),
            Some("third revision")
        );
        assert_eq!(manager.scope_pack_cache_status().builds, 3);
    }

    /// The scoped capability cache decides for itself when a catalog changed
    /// and builds a candidate to validate before swapping; that build must
    /// read the disk, never the trust window, or the candidate it validates
    /// is the catalog it already serves.
    #[test]
    fn a_fresh_load_reads_the_disk_and_refreshes_the_cache() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        let skill_root = layout.scope_skills_root("owner", "default").join("widget");
        write_widget_skill(&skill_root, "widget", "first revision");
        let _ = manager.load_pack_defs_for_scope_with_provenance("owner", "default");

        write_widget_skill(&skill_root, "widget", "second revision");
        let fresh = manager.load_pack_defs_for_scope_fresh("owner", "default");
        assert_eq!(
            widget_description(&fresh).as_deref(),
            Some("second revision")
        );
        let cached = manager.load_pack_defs_for_scope_with_provenance("owner", "default");
        assert_eq!(
            widget_description(&cached).as_deref(),
            Some("second revision")
        );
        assert_eq!(manager.scope_pack_cache_status().hits, 1);
    }

    #[test]
    fn materialize_scope_copies_template_bot_config_when_scope_is_missing_one() {
        let temp = tempdir().expect("tempdir");
        let workspace_root = temp.path().join("magician_data_v3");
        let layout = ArtifactV2Workspace::new(&workspace_root);
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        let template_config = manager.skillshub_bot_configs_path();
        let scoped_config =
            layout.capability_bot_configs_path(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE);

        fs::create_dir_all(manager.skillshub_bots_root()).expect("skillshub bots root");
        fs::write(
            &template_config,
            "bots:\n  telegram:\n    enabled: true\n    command: node\n",
        )
        .expect("template bot config");

        manager
            .materialize_scope(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .expect("materialize scope");

        assert_eq!(
            fs::read_to_string(&scoped_config).expect("scoped bot config"),
            "bots:\n  telegram:\n    enabled: true\n    command: node\n"
        );
    }

    #[test]
    fn materialize_scope_does_not_create_shared_env_file() {
        // Per-skill `.env` files now live at
        // `<scope>/skills/<skill>/config/.env`, populated by
        // `make -C skillshub install-scope` + `setup-env`. The legacy
        // shared `<scope>/capabilities/config/.env.development` is gone,
        // as is the retired system-shared `system/skills/` tier.
        let temp = tempdir().expect("tempdir");
        let workspace_root = temp.path().join("magician_data_v3");
        let layout = ArtifactV2Workspace::new(&workspace_root);
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        // Hardcode the path the legacy umbrella *would* have created —
        // the test exists to assert it never gets created.
        let config_root = layout
            .scope_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .join("config");

        manager
            .materialize_scope(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .expect("materialize scope");

        assert!(
            !config_root.join(".env.development").exists(),
            "scope-level .env.development should no longer be auto-seeded"
        );
        assert!(
            !config_root.join(".env").exists(),
            "scope-level .env should no longer be auto-seeded"
        );
    }

    #[test]
    fn materialize_scope_does_not_create_capability_config_dir() {
        // The scope's `capabilities/config/` directory is no longer
        // auto-created. Per-skill secrets live next to each skill at
        // `<system>/skills/<skill>/config/.env`; the OAuth client
        // is copied directly into each `<scope>/capabilities/auth/
        // gws-<account>/`. Nothing scope-level needs to live here.
        let temp = tempdir().expect("tempdir");
        let workspace_root = temp.path().join("magician_data_v3");
        let layout = ArtifactV2Workspace::new(&workspace_root);
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        // Hardcode the path the legacy umbrella *would* have created —
        // the test exists to assert it never gets created.
        let config_root = layout
            .scope_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .join("config");

        manager
            .materialize_scope(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .expect("materialize scope");

        assert!(
            !config_root.exists(),
            "scope-level capabilities/config/ should not be auto-created"
        );
    }

    #[test]
    fn subprocess_bin_path_prepends_only_existing_dirs_in_order() {
        let temp = tempdir().expect("tempdir");
        // Two existing bin dirs (venv_bin + node_modules_bin) and one that
        // does not exist on disk (node_bin) — the missing one must be dropped.
        let venv_bin = temp.path().join("venv/bin");
        let node_modules_bin = temp.path().join("node_modules/.bin");
        let node_bin = temp.path().join("does/not/exist/bin");
        fs::create_dir_all(&venv_bin).expect("venv bin");
        fs::create_dir_all(&node_modules_bin).expect("node_modules bin");

        let layout = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let manager = CapabilityWorkspaceManager::new(layout, temp.path());
        let mut scope_paths = manager.default_scope_paths();
        scope_paths.venv_bin = venv_bin.clone();
        scope_paths.node_modules_bin = node_modules_bin.clone();
        scope_paths.node_bin = node_bin.clone();

        // An existing per-skill `bin/` leads the list.
        let skill_bin = temp.path().join("skill/bin");
        fs::create_dir_all(&skill_bin).expect("skill bin");

        let parent = "/usr/bin:/bin";
        let path = scope_paths
            .subprocess_bin_path(Some(skill_bin.as_path()), parent)
            .expect("at least one bin dir exists");

        // Order: extra_bin, venv_bin, node_modules_bin (node_bin dropped —
        // does not exist), then the parent PATH appended last.
        let expected = format!(
            "{}:{}:{}:{}",
            skill_bin.display(),
            venv_bin.display(),
            node_modules_bin.display(),
            parent,
        );
        assert_eq!(path, expected);
        assert!(
            !path.contains(&node_bin.display().to_string()),
            "non-existent node_bin must not appear: {path}"
        );

        // Empty parent → no trailing segment.
        let path_no_parent = scope_paths
            .subprocess_bin_path(Some(skill_bin.as_path()), "")
            .expect("bin dirs exist");
        assert_eq!(
            path_no_parent,
            format!(
                "{}:{}:{}",
                skill_bin.display(),
                venv_bin.display(),
                node_modules_bin.display(),
            )
        );

        // No extra_bin still prepends the existing scope bins.
        let path_no_extra = scope_paths
            .subprocess_bin_path(None, parent)
            .expect("scope bins exist");
        assert_eq!(
            path_no_extra,
            format!(
                "{}:{}:{}",
                venv_bin.display(),
                node_modules_bin.display(),
                parent,
            )
        );

        // No existing bin dirs → None (caller leaves the parent PATH).
        let empty = temp.path().join("empty");
        let mut none_paths = scope_paths.clone();
        none_paths.venv_bin = empty.join("a");
        none_paths.node_modules_bin = empty.join("b");
        none_paths.node_bin = empty.join("c");
        assert!(
            none_paths.subprocess_bin_path(None, parent).is_none(),
            "no existing dirs must yield None"
        );
    }

    #[test]
    fn scope_paths_are_absolute_when_workspace_root_is_relative() {
        let layout = ArtifactV2Workspace::new("magician_data_v3");
        let manager = CapabilityWorkspaceManager::new(layout, ".");
        let scope_paths = manager.default_scope_paths();

        assert!(scope_paths.capabilities_root.is_absolute());
        assert!(scope_paths.bots_root.is_absolute());
        assert!(scope_paths.auth_root.is_absolute());
        assert!(scope_paths.workdirs_root.is_absolute());
        assert_eq!(
            scope_paths.apply_vars("{scope_capabilities_root}/bots/gmail/.env.business"),
            scope_paths
                .capabilities_root
                .join("bots/gmail/.env.business")
                .to_string_lossy()
                .to_string()
        );
    }

    #[test]
    fn byte_identical_scope_pack_does_not_gain_embedded_provenance() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let manager = CapabilityWorkspaceManager::new(layout.clone(), temp.path());
        let skill_dir = layout
            .scope_skills_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
            .join("evidence_data_override");
        fs::create_dir_all(&skill_dir).expect("scope skill directory");
        fs::write(
            skill_dir.join("tool_schema.yaml"),
            crate::magician_v2::execution::embedded_compiled_pack_yaml("evidence_data")
                .expect("embedded evidence_data pack"),
        )
        .expect("scope evidence_data schema");

        let selected = manager.load_pack_defs_for_scope_with_provenance(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
        );
        assert!(selected
            .definitions
            .iter()
            .any(|pack| pack.name == "evidence_data"));
        assert!(!selected.embedded_fallback_names.contains("evidence_data"));
    }
}
