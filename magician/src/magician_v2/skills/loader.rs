//! Discovers SKILL.md folders across precedence-ordered search paths.
//!
//! Per-name shadowing: workspace's `<name>/` wins entirely over system's. No
//! merging within a name.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::manifest::SkillManifest;

/// Loader configured with precedence-ordered search paths (highest first).
///
/// At runtime: `[<scope>/skills, <extras...>/skills]` where extras come
/// from `tool-runtime-config.yaml :: registry.paths`. Higher precedence
/// wins on name collision; whole-folder shadowing.
pub struct SkillLoader {
    search_paths: Vec<PathBuf>,
}

impl SkillLoader {
    pub fn new(search_paths_high_to_low: Vec<PathBuf>) -> Self {
        Self {
            search_paths: search_paths_high_to_low,
        }
    }

    /// Walk every search path, discover skill folders, return manifests sorted
    /// by name. Per-name shadowing applies.
    pub fn discover(&self) -> Result<Vec<SkillManifest>> {
        let mut by_name: HashMap<String, SkillManifest> = HashMap::new();
        // Claim names by their directory as soon as a higher-precedence layer
        // presents a SKILL.md. A malformed or oversized higher-layer skill must
        // fail closed for that name instead of silently reactivating a lower
        // package that the operator intended to shadow.
        let mut claimed_names: HashSet<String> = HashSet::new();
        for path in &self.search_paths {
            if !path.exists() {
                continue;
            }
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(
                        root = %path.display(),
                        %error,
                        "skipping unreadable skill search root"
                    );
                    continue;
                },
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        tracing::warn!(
                            root = %path.display(),
                            %error,
                            "skipping unreadable skill directory entry"
                        );
                        continue;
                    },
                };
                let dir = entry.path();
                match entry.file_type() {
                    Ok(file_type) if file_type.is_dir() => {},
                    Ok(_) => continue,
                    Err(error) => {
                        tracing::warn!(
                            skill = %dir.display(),
                            %error,
                            "skipping skill entry with unreadable type"
                        );
                        continue;
                    },
                }
                let skill_md = dir.join("SKILL.md");
                // Skill files are host-absolute symlinks into skillshub; rewrite
                // the target for this environment (identity on a native host).
                match std::fs::symlink_metadata(&skill_md) {
                    Ok(_) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        tracing::warn!(
                            skill = %dir.display(),
                            %error,
                            "skipping unreadable skill marker"
                        );
                        continue;
                    },
                }
                let Some(directory_name) = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                else {
                    tracing::warn!(skill = %dir.display(), "skipping non-UTF-8 skill directory");
                    continue;
                };
                if !claimed_names.insert(directory_name) {
                    continue;
                }
                let Some(manifest) = cached_skill_manifest(&skill_md, &dir) else {
                    continue;
                };
                // Workspace (first path) wins on collision.
                by_name.entry(manifest.name.clone()).or_insert(manifest);
            }
        }

        let mut out: Vec<SkillManifest> = by_name.into_values().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

/// What one `SKILL.md` parses to. Apps and unreadable or malformed files
/// resolve to `None`: discovery skips them (having already claimed the name).
fn parse_skill_file(skill_md: &Path, dir: &Path) -> Option<SkillManifest> {
    let raw = match super::embedded_extensions::read_bounded_skill_markdown(skill_md) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::warn!(
                skill = %dir.display(),
                %error,
                "skipping unreadable or oversized skill"
            );
            return None;
        },
    };
    match app_manifest_declared(&raw) {
        Ok(true) => {
            if let Err(error) = validate_declared_app_manifest(&raw) {
                tracing::warn!(
                    skill = %dir.display(),
                    %error,
                    "skipping malformed app package manifest"
                );
            }
            // Apps are owned exclusively by the app package
            // registry and never enter procedure discovery.
            return None;
        },
        Ok(false) => {},
        Err(error) => {
            tracing::warn!(
                skill = %dir.display(),
                %error,
                "skipping manifest with invalid routing metadata"
            );
            return None;
        },
    }
    let manifest = match parse_manifest(&raw, dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                skill = %dir.display(),
                %error,
                "skipping malformed skill manifest"
            );
            return None;
        },
    };
    Some(manifest)
}

/// A file's identity for the parse cache: size and modification time of
/// the `SKILL.md` a symlink resolves to.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SkillFileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

type SkillParseCache = std::sync::Mutex<HashMap<PathBuf, (SkillFileStamp, Option<SkillManifest>)>>;

fn skill_parse_cache() -> &'static SkillParseCache {
    static CACHE: OnceLock<SkillParseCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `parse_skill_file`, cached by path and file stamp. Discovery re-ran the
/// YAML front-matter parse of every installed skill — twice per file — on
/// each call, and the chat composer's reference catalog calls it four times
/// per request: ~5 s of CPU on an actix worker for 89 skills, stalling the
/// requests queued behind it (a chat switch waited on it). A stat is cheap;
/// an edited file changes its stamp and is parsed again.
fn cached_skill_manifest(skill_md: &Path, dir: &Path) -> Option<SkillManifest> {
    let stamp = std::fs::metadata(skill_md).ok().map(|meta| SkillFileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    });
    if let Some(stamp) = stamp {
        let cache = skill_parse_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached, outcome)) = cache.get(skill_md) {
            if *cached == stamp {
                return outcome.clone();
            }
        }
    }
    let outcome = parse_skill_file(skill_md, dir);
    if let Some(stamp) = stamp {
        skill_parse_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(skill_md.to_path_buf(), (stamp, outcome.clone()));
    }
    outcome
}

/// Parse SKILL.md frontmatter into a SkillManifest. Returns errors for missing
/// frontmatter, parse failures, or schema-mutex violations
/// (both `personality:` and `agent:` blocks present).
pub fn parse_manifest(raw: &str, dir: &Path) -> Result<SkillManifest> {
    if app_manifest_declared(raw)? {
        validate_declared_app_manifest(raw)?;
        anyhow::bail!(
            "skill_type app is owned by the app package registry and cannot enter the procedure loader"
        );
    }

    let mut m: SkillManifest = tool_runtime_core::manifest_parser::parse_skill_frontmatter(raw)
        .map_err(|error| anyhow::anyhow!("parse bounded frontmatter YAML: {error}"))?;
    m.source_dir = dir.to_path_buf();
    m.body = OnceLock::new();

    // Validate name matches parent directory.
    let parent_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    if !parent_name.is_empty() && parent_name != m.name {
        anyhow::bail!(
            "name '{}' does not match parent dir '{}'",
            m.name,
            parent_name
        );
    }

    Ok(m)
}

fn app_manifest_declared(raw: &str) -> Result<bool> {
    let declared_skill_type = tool_runtime_core::manifest_parser::parse_skill_magician_extension::<
        String,
    >(raw, "skill_type")
    .map_err(|error| anyhow::anyhow!("parse bounded skill routing metadata: {error}"))?;
    Ok(declared_skill_type.as_deref() == Some("app"))
}

fn validate_declared_app_manifest(raw: &str) -> Result<()> {
    crate::magician_v2::apps::manifest::parse_app_manifest_frontmatter(
        raw.as_bytes(),
        &crate::magician_v2::apps::manifest::AppPackageLimits::default(),
    )
    .map(|_| ())
    .map_err(|error| anyhow::anyhow!("strict app manifest validation failed: {error}"))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::skills::manifest::InferredKind;
    use std::fs;

    fn write_skill(parent: &Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn a_rescan_reuses_parsed_skills_and_an_edited_file_is_parsed_again() {
        let root = tempfile::tempdir().unwrap();
        write_skill(
            root.path(),
            "notes",
            "---\nname: notes\ndescription: first\n---\nbody",
        );
        let loader = SkillLoader::new(vec![root.path().to_path_buf()]);
        let first = loader.discover().unwrap();
        assert_eq!(first[0].description, "first");
        // Unchanged file: the cached parse.
        assert_eq!(loader.discover().unwrap()[0].description, "first");
        // A different size (and mtime) is a new stamp: parsed again.
        fs::write(
            root.path().join("notes").join("SKILL.md"),
            "---\nname: notes\ndescription: second, longer\n---\nbody",
        )
        .unwrap();
        assert_eq!(loader.discover().unwrap()[0].description, "second, longer");
    }

    #[test]
    fn discovers_procedure_and_personality_skills() {
        let dir = tempfile::tempdir().unwrap();

        write_skill(
            dir.path(),
            "review",
            "---\nname: review\ndescription: Review code\n---\n# body\n",
        );
        write_skill(
            dir.path(),
            "witty",
            "---\nname: witty\ndescription: Witty persona\n\
             metadata:\n  magician:\n    personality:\n      active_mode: witty\n      voice: \"sharp\"\n---\n# body\n",
        );

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let by_name: HashMap<&str, &SkillManifest> =
            manifests.iter().map(|m| (m.name.as_str(), m)).collect();

        assert_eq!(by_name["review"].inferred_kind(), InferredKind::Procedure);
        assert_eq!(
            by_name["witty"].inferred_kind(),
            InferredKind::PersonalityMode
        );
    }

    #[test]
    fn rejects_name_directory_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "actual-dir",
            "---\nname: different-name\ndescription: d\n---\n",
        );
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        assert!(manifests.is_empty());
    }

    #[test]
    fn precedence_higher_layer_wins_on_collision() {
        let low = tempfile::tempdir().unwrap();
        let high = tempfile::tempdir().unwrap();
        write_skill(
            low.path(),
            "review",
            "---\nname: review\ndescription: from-low\n---\n",
        );
        write_skill(
            high.path(),
            "review",
            "---\nname: review\ndescription: from-high\n---\n",
        );

        let manifests = SkillLoader::new(vec![high.path().to_path_buf(), low.path().to_path_buf()])
            .discover()
            .unwrap();

        let r = manifests.iter().find(|m| m.name == "review").unwrap();
        assert_eq!(r.description, "from-high");
    }

    #[test]
    fn body_lazy_loaded_only_on_request() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "verbose",
            "---\nname: verbose\ndescription: Long body\n---\n# body\nlots of content\n",
        );
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let m = &manifests[0];
        assert!(!m.body_loaded());
        let body = m.body().unwrap();
        assert!(body.contains("lots of content"));
        assert!(m.body_loaded());
    }

    #[test]
    fn malformed_skill_isolated_from_valid_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nofrontmatter");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), "just body, no frontmatter").unwrap();
        write_skill(
            dir.path(),
            "valid",
            "---\nname: valid\ndescription: valid sibling\n---\n",
        );
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "valid");
    }

    #[test]
    fn malformed_high_layer_still_shadows_valid_lower_skill() {
        let low = tempfile::tempdir().unwrap();
        let high = tempfile::tempdir().unwrap();
        write_skill(
            low.path(),
            "review",
            "---\nname: review\ndescription: lower layer\n---\n",
        );
        write_skill(high.path(), "review", "not frontmatter");

        let manifests = SkillLoader::new(vec![high.path().to_path_buf(), low.path().to_path_buf()])
            .discover()
            .unwrap();

        assert!(manifests.is_empty());
    }

    #[test]
    fn valid_app_manifest_is_strictly_validated_but_never_discovered_as_a_skill() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "learning-plan",
            &crate::magician_v2::apps::manifest::tests::valid_skill_document(),
        );

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();

        assert!(manifests.is_empty());
        let error = parse_manifest(
            &crate::magician_v2::apps::manifest::tests::valid_skill_document(),
            &dir.path().join("learning-plan"),
        )
        .expect_err("an app must not become an ordinary skill");
        assert!(error.to_string().contains("app package registry"));
    }

    #[test]
    fn one_hundred_app_manifests_do_not_expand_the_global_procedure_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let template = crate::magician_v2::apps::manifest::tests::valid_skill_document();
        for index in 0..100 {
            let name = format!("private-app-{index}");
            write_skill(
                dir.path(),
                &name,
                &template.replacen("name: learning-plan", &format!("name: {name}"), 1),
            );
        }

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();

        assert!(manifests.is_empty());
    }

    #[test]
    fn malformed_app_manifest_fails_closed_without_falling_back_to_procedure() {
        let dir = tempfile::tempdir().unwrap();
        let malformed = r#"---
name: learning-plan
version: 0.1.0
description: malformed app
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
app:
  unexpected: true
---
"#;
        write_skill(dir.path(), "learning-plan", malformed);

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        assert!(manifests.is_empty());
        let error = parse_manifest(malformed, &dir.path().join("learning-plan"))
            .expect_err("malformed app must fail at the strict app parser");
        assert!(error
            .to_string()
            .contains("strict app manifest validation failed"));
    }

    #[cfg(unix)]
    #[test]
    fn broken_high_layer_marker_still_shadows_valid_lower_skill() {
        use std::os::unix::fs::symlink;

        let low = tempfile::tempdir().unwrap();
        let high = tempfile::tempdir().unwrap();
        write_skill(
            low.path(),
            "review",
            "---\nname: review\ndescription: lower layer\n---\n",
        );
        let high_skill = high.path().join("review");
        fs::create_dir(&high_skill).unwrap();
        symlink(high_skill.join("missing"), high_skill.join("SKILL.md")).unwrap();

        let manifests = SkillLoader::new(vec![high.path().to_path_buf(), low.path().to_path_buf()])
            .discover()
            .unwrap();

        assert!(manifests.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_directories_are_not_traversed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        write_skill(
            external.path(),
            "linked",
            "---\nname: linked\ndescription: outside root\n---\n",
        );
        symlink(external.path().join("linked"), root.path().join("linked")).unwrap();

        let manifests = SkillLoader::new(vec![root.path().to_path_buf()])
            .discover()
            .unwrap();

        assert!(manifests.is_empty());
    }

    #[test]
    fn oversized_skill_isolated_from_valid_sibling() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "oversized",
            &"x".repeat(tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES + 1),
        );
        write_skill(
            dir.path(),
            "valid",
            "---\nname: valid\ndescription: valid sibling\n---\n",
        );

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "valid");
    }

    #[test]
    fn skips_dirs_without_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("not-a-skill");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("README.md"), "not a skill").unwrap();

        // Add one valid sibling so the loader has something to return.
        write_skill(
            dir.path(),
            "real-skill",
            "---\nname: real-skill\ndescription: d\n---\n",
        );

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "real-skill");
    }

    #[test]
    fn nonexistent_search_path_is_silently_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "real", "---\nname: real\ndescription: d\n---\n");
        let manifests = SkillLoader::new(vec![
            PathBuf::from("/definitely/does/not/exist"),
            dir.path().to_path_buf(),
        ])
        .discover()
        .unwrap();
        assert_eq!(manifests.len(), 1);
    }
}
