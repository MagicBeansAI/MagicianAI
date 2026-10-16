//! Per-scope skill discovery + adapter from `SkillManifest` to
//! `runtime_core::ToolInfo`.
//!
//! At scope-bind time the orchestrator constructs a `SkillLoader` over
//! the precedence-ordered runtime layers:
//!
//! ```text
//! $MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/ → highest precedence
//! <each entry of tool-runtime-config.yaml :: registry.paths>/skills/ → fallback (declared order)
//! ```
//!
//! The system-shared skills tier was retired (v0.6.572); skills are
//! workspace-scoped only, with `registry.paths` providing
//! per-deployment overlays.
//!
//! Each procedure skill is converted to a `ToolInfo` with empty
//! parameters (the skill's body becomes the steering message at
//! activation time; parameters live on `scripts/` ephemeral tools, not
//! on the outer-loop selector).
//!
//! ## Allowlist name resolution
//!
//! Agent definitions should use canonical AgentSkill slugs in their
//! `tools:` field (`image-generation`, `gif-search-via-klipy`, …).
//!
//! [`resolve_skill_name`] intentionally performs exact matching only:
//! `tools[]` must equal the catalog name. When a skill is renamed, update
//! agent definitions and workspace materializations to the new canonical
//! slug instead of adding an alias table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use runtime_core::ToolInfo;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::loader::SkillLoader;
use super::manifest::{InferredKind, SkillManifest};

/// Resolve an agent allowlist entry to the matching skill catalog name
/// from `manifests`. Returns the matched manifest's `name` (a kebab-case
/// AgentSkills slug equal to its source directory name) so the caller
/// can look up the manifest by exact name in subsequent steps.
///
/// Resolution is intentionally literal. Renames must be reflected in the
/// definitions that list the skill.
pub fn resolve_skill_name<'a>(
    allowlist_name: &str,
    manifests: &'a [SkillManifest],
) -> Option<&'a str> {
    manifests
        .iter()
        .find(|m| m.name == allowlist_name)
        .map(|m| m.name.as_str())
}

/// Extract the value of a top-level `name:` key from a YAML document.
/// Top-level means column 0 (no leading whitespace) so we don't pick
/// up a nested `name` field inside `parameters` or
/// `native_action_schemas`. Returns None when no top-level `name:`
/// appears in the first 50 lines (covers every `tool_schema.yaml`
/// shipped today). Lightweight string parsing so callers don't pull
/// `serde_yaml` into hot paths just to read one field.
pub fn extract_top_level_name(yaml: &str) -> Option<String> {
    for line in yaml.lines().take(50) {
        if !line.starts_with("name:") {
            continue;
        }
        let value = line["name:".len()..].trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(value);
        if unquoted.is_empty() {
            return None;
        }
        return Some(unquoted.to_string());
    }
    None
}

/// Read a single skill directory's `tool_schema.yaml::name` if present.
/// Convenience wrapper over [`extract_top_level_name`] that handles the
/// path build + missing-file case. Returns None when no schema file
/// exists or its `name:` is unset.
pub fn read_tool_schema_name(skill_dir: &Path) -> Option<String> {
    // `tool_schema.yaml` is a host-absolute symlink into skillshub on a
    // materialized scope; rewrite the target for this environment (identity on a
    // native host) so the cli-template dispatch index resolves in a container.
    let schema = super::path_rewrite::resolve_skill_path(&skill_dir.join("tool_schema.yaml"));
    if !schema.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&schema).ok()?;
    extract_top_level_name(&content)
}

/// Convert a procedure-kind [`SkillManifest`] into a `ToolInfo` suitable
/// for inclusion in `merged_agent_tools`. Body activation, ephemeral
/// tool registration, and PATH wiring still happen at dispatch time —
/// this is just the outer-loop selector descriptor.
///
/// Returns `None` for non-procedure skills (personality-mode skills are
/// never selectable from the catalog; they're driven by
/// `switch_personality`).
pub fn skill_manifest_to_tool_info(manifest: &SkillManifest) -> Option<ToolInfo> {
    if manifest.inferred_kind() != InferredKind::Procedure {
        return None;
    }
    Some(ToolInfo {
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        category: "skill".to_string(),
        categories: vec!["skill".to_string()],
        parameters: Vec::new(),
        enhanced_description: None,
        keywords: Vec::new(),
        use_cases: Vec::new(),
        composition_category: None,
        providing_agent_id: None,
    })
}

/// Discover every procedure skill installed for a scope, ordered with
/// workspace-layer skills shadowing extras-layer skills on name
/// collision (whole-folder shadowing per the SkillLoader contract).
/// Extras come from `tool-runtime-config.yaml :: registry.paths`.
///
/// Personality-mode skills are filtered out — they belong to
/// `switch_personality`, not the agent's tool catalog.
/// CUA and Mac automation are distinct providers. Enabling a Windows/Linux CUA
/// desktop must never satisfy a skill's Mac host-gateway requirement.
fn skill_providers_available(manifest: &SkillManifest) -> bool {
    manifest
        .metadata
        .magician
        .as_ref()
        .map(|mag| {
            mag.requires.providers_available(
                crate::magician_v2::config_extras::host_gateway_available(),
                crate::magician_v2::config_extras::cua_available(),
            )
        })
        .unwrap_or(true)
}

pub fn discover_procedure_skills_for_scope(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Vec<SkillManifest> {
    let workspace_dir = layout.scope_skills_root(principal, workspace);
    let mut search_paths: Vec<PathBuf> = vec![workspace_dir];
    search_paths.extend(crate::magician_v2::config_extras::extra_skills_dirs());
    let manifests = match SkillLoader::new(search_paths).discover() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                error = %e,
                "scope_loader: skill discovery failed"
            );
            return Vec::new();
        },
    };
    manifests
        .into_iter()
        .filter(|m| m.inferred_kind() == InferredKind::Procedure)
        .filter(skill_providers_available)
        .collect()
}

/// Return the (name, description) of every procedure skill exactly
/// named in the agent's `tools:` allowlist. Uses the same literal
/// [`resolve_skill_name`] rule as `activate_skill`'s name resolution so
/// the catalog and the activation gate see the same set.
/// Personality-mode skills are filtered out.
///
/// Used by the chat-runtime `activate_skill` tool to (a) gate the
/// allowlist enforcement check and (b) render the dynamic catalog
/// block in `activate_skill`'s description.
///
/// Extras from `tool-runtime-config.yaml :: registry.paths` are appended
/// after `workspace_skills_dir` so workspace skills shadow extras on
/// collision.
pub fn agent_procedure_skill_catalog(
    workspace_skills_dir: &Path,
    agent_allowlist: &[String],
) -> Vec<(String, String)> {
    let mut search_paths: Vec<PathBuf> = vec![workspace_skills_dir.to_path_buf()];
    search_paths.extend(crate::magician_v2::config_extras::extra_skills_dirs());
    let manifests = match SkillLoader::new(search_paths).discover() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "scope_loader: agent_procedure_skill_catalog discovery failed"
            );
            return Vec::new();
        },
    };
    if manifests.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in agent_allowlist {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(resolved) = resolve_skill_name(trimmed, &manifests) else {
            continue;
        };
        let Some(manifest) = manifests.iter().find(|m| m.name == resolved) else {
            continue;
        };
        if manifest.inferred_kind() != InferredKind::Procedure {
            continue;
        }
        if !skill_providers_available(manifest) {
            continue;
        }
        if !seen.insert(manifest.name.clone()) {
            continue;
        }
        out.push((manifest.name.clone(), manifest.description.clone()));
    }
    out
}

/// Return the procedure-skill `ToolInfo` entries the agent has
/// allowlisted, in the order the allowlist entries appear in
/// `agent_allowlist`. Names not resolving to any installed skill are
/// silently skipped (caller's responsibility to log if needed).
///
/// Matching is literal: allowlist entries must equal the installed skill
/// name. When renaming a skill, update the allowlists rather than
/// accepting older spellings here.
pub fn procedure_tool_infos_for_agent(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    agent_allowlist: &[String],
) -> Vec<ToolInfo> {
    let manifests = discover_procedure_skills_for_scope(layout, principal, workspace);
    if manifests.is_empty() {
        return Vec::new();
    }
    let by_name: HashMap<&str, &SkillManifest> =
        manifests.iter().map(|m| (m.name.as_str(), m)).collect();

    let mut out: Vec<ToolInfo> = Vec::new();
    for entry in agent_allowlist {
        if let Some(manifest) = by_name.get(entry.as_str()) {
            if let Some(info) = skill_manifest_to_tool_info(manifest) {
                out.push(info);
            }
        }
    }
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_skill(parent: &Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), body).unwrap();
    }

    fn write_procedure(parent: &Path, name: &str) {
        write_skill(
            parent,
            name,
            &format!("---\nname: {name}\ndescription: a {name} procedure\n---\n# body\n"),
        );
    }

    fn write_personality(parent: &Path, name: &str) {
        write_skill(
            parent,
            name,
            &format!(
                "---\nname: {name}\ndescription: {name} persona\n\
                 metadata:\n  magician:\n    personality:\n      \
                 active_mode: {name}\n      voice: \"x\"\n---\n# body\n"
            ),
        );
    }

    /// Build a `Vec<SkillManifest>` from a list of catalog names by
    /// writing minimal SKILL.md files into a tempdir, then discovering
    /// them via `SkillLoader`.
    fn manifests_for(parent: &Path, names: &[&str]) -> Vec<SkillManifest> {
        for name in names {
            write_procedure(parent, name);
        }
        SkillLoader::new(vec![parent.to_path_buf()])
            .discover()
            .unwrap()
    }

    #[test]
    fn resolve_skill_name_matches_literal_first() {
        let dir = tempfile::tempdir().unwrap();
        let manifests = manifests_for(dir.path(), &["awk", "browser", "true-friend"]);
        assert_eq!(resolve_skill_name("awk", &manifests), Some("awk"));
    }

    #[test]
    fn resolve_skill_name_does_not_translate_underscore_names() {
        let dir = tempfile::tempdir().unwrap();
        let manifests = manifests_for(
            dir.path(),
            &["sample-hyphenated-skill", "video-generation-via-veo"],
        );
        assert!(resolve_skill_name("sample_hyphenated_skill", &manifests).is_none());
        assert!(resolve_skill_name("video_generation_via_veo", &manifests).is_none());
    }

    #[test]
    fn resolve_skill_name_does_not_use_tool_schema_pack_name() {
        let dir = tempfile::tempdir().unwrap();
        write_procedure(dir.path(), "custom-exporter");
        fs::write(
            dir.path().join("custom-exporter").join("tool_schema.yaml"),
            "name: legacy_custom_exporter\nparameters: []\n",
        )
        .unwrap();
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        assert!(resolve_skill_name("legacy_custom_exporter", &manifests).is_none());
    }

    #[test]
    fn resolve_skill_name_returns_none_when_no_form_matches() {
        let dir = tempfile::tempdir().unwrap();
        let manifests = manifests_for(dir.path(), &["awk"]);
        assert!(resolve_skill_name("treasurer", &manifests).is_none());
    }

    #[test]
    fn skill_manifest_to_tool_info_returns_none_for_personality() {
        let dir = tempfile::tempdir().unwrap();
        write_personality(dir.path(), "witty");
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let witty = manifests.iter().find(|m| m.name == "witty").unwrap();
        assert!(skill_manifest_to_tool_info(witty).is_none());
    }

    #[test]
    fn skill_manifest_to_tool_info_populates_required_fields_for_procedure() {
        let dir = tempfile::tempdir().unwrap();
        write_procedure(dir.path(), "awk");
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let awk = manifests.iter().find(|m| m.name == "awk").unwrap();
        let info = skill_manifest_to_tool_info(awk).unwrap();
        assert_eq!(info.name, "awk");
        assert!(!info.description.is_empty());
        assert_eq!(info.category, "skill");
        assert!(info.parameters.is_empty());
        assert!(info.providing_agent_id.is_none());
    }

    fn workspace_with_scope_skills(tempdir: &tempfile::TempDir) -> ArtifactV2Workspace {
        let workspace = ArtifactV2Workspace::new(tempdir.path());
        let scope_skills = workspace.scope_skills_root("anonymous", "default");
        std::fs::create_dir_all(&scope_skills).unwrap();
        workspace
    }

    #[test]
    fn procedure_tool_infos_returns_only_skills_with_byte_identical_names() {
        let tempdir = tempfile::tempdir().unwrap();
        let workspace = workspace_with_scope_skills(&tempdir);
        let scope_skills = workspace.scope_skills_root("anonymous", "default");
        write_procedure(&scope_skills, "awk");
        write_procedure(&scope_skills, "sample-hyphenated-skill");
        write_procedure(&scope_skills, "image-generation");

        // Allowlist mixes literal names (`awk`, `image-generation`), a
        // non-canonical underscore spelling (`sample_hyphenated_skill`), and a
        // non-skill capability (`treasurer`). Only byte-identical skill
        // names are surfaced here.
        let allowlist = vec![
            "awk".to_string(),
            "sample_hyphenated_skill".to_string(),
            "image-generation".to_string(),
            "treasurer".to_string(), // not a skill at all
        ];

        let infos = procedure_tool_infos_for_agent(&workspace, "anonymous", "default", &allowlist);
        let names: Vec<&str> = infos.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["awk", "image-generation"]);
    }

    #[test]
    fn procedure_tool_infos_filters_out_personality_modes() {
        let tempdir = tempfile::tempdir().unwrap();
        let workspace = workspace_with_scope_skills(&tempdir);
        let scope_skills = workspace.scope_skills_root("anonymous", "default");
        write_procedure(&scope_skills, "awk");
        write_personality(&scope_skills, "witty");

        let allowlist = vec!["awk".to_string(), "witty".to_string()];
        let infos = procedure_tool_infos_for_agent(&workspace, "anonymous", "default", &allowlist);
        let names: Vec<&str> = infos.iter().map(|t| t.name.as_str()).collect();
        // `witty` is allowlisted but it's a personality-mode skill, not
        // a procedure — `switch_personality` is its activation path.
        assert_eq!(names, vec!["awk"]);
    }

    #[test]
    fn procedure_tool_infos_returns_empty_when_no_skills_installed() {
        let tempdir = tempfile::tempdir().unwrap();
        let workspace = workspace_with_scope_skills(&tempdir);
        let allowlist = vec!["awk".to_string()];
        let infos = procedure_tool_infos_for_agent(&workspace, "anonymous", "default", &allowlist);
        assert!(infos.is_empty());
    }

    #[test]
    fn agent_procedure_skill_catalog_returns_allowlisted_skills() {
        let workspace_dir = tempfile::tempdir().unwrap();
        write_procedure(workspace_dir.path(), "email-etiquette");
        write_procedure(workspace_dir.path(), "meeting-prep-brief-format");
        write_personality(workspace_dir.path(), "witty");

        let allowlist = vec![
            "email-etiquette".to_string(),
            "meeting-prep-brief-format".to_string(),
            "witty".to_string(),        // personality — filtered out
            "non-existent".to_string(), // missing — filtered out
        ];
        let catalog = agent_procedure_skill_catalog(workspace_dir.path(), &allowlist);
        let names: Vec<&str> = catalog.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["email-etiquette", "meeting-prep-brief-format"]);
    }

    #[test]
    fn agent_procedure_skill_catalog_dedupes_literal_duplicates() {
        let workspace_dir = tempfile::tempdir().unwrap();
        write_procedure(workspace_dir.path(), "sample-hyphenated-skill");

        let allowlist = vec![
            "sample-hyphenated-skill".to_string(),
            "sample-hyphenated-skill".to_string(),
        ];
        let catalog = agent_procedure_skill_catalog(workspace_dir.path(), &allowlist);
        let names: Vec<&str> = catalog.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["sample-hyphenated-skill"]);
    }

    #[test]
    fn agent_procedure_skill_catalog_returns_empty_when_no_match() {
        let workspace_dir = tempfile::tempdir().unwrap();
        write_procedure(workspace_dir.path(), "alpha");
        let allowlist = vec!["unrelated".to_string()];
        let catalog = agent_procedure_skill_catalog(workspace_dir.path(), &allowlist);
        assert!(catalog.is_empty());
    }
}
