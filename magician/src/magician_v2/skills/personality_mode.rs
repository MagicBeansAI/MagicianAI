//! Personality-mode skill lookup helpers used by `switch_personality` and
//! the personality-seeding code path on session start.
//!
//! A personality-mode skill is a SKILL.md folder whose frontmatter carries
//! a `metadata.magician.personality:` block. The body is descriptive prose
//! (operator-facing); the structured `voice / expression_bias /
//! suppression_rules / expression_triggers / active_mode` fields are what
//! get written into the agent's `personality_profile` memory tier.
//!
//! Today these helpers are the *primary* source for personality mode
//! lookups; legacy `<system_capability_template>/personality/*.yaml`
//! remains as a fallback while skillshub catches up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::loader::SkillLoader;
use super::manifest::{InferredKind, PersonalitySpec};

/// Returns the [`PersonalitySpec`] for `mode_name` if a personality-mode
/// skill is installed at any of the search paths (highest-precedence
/// first). Returns `None` when no installed skill matches — caller falls
/// back to legacy YAML loading.
///
/// Legacy callers may pass an underscore-form mode name (`true_friend`)
/// even though AgentSkills v1 requires hyphenated `name` fields
/// (`true-friend`). This bridges both forms during the migration so
/// existing agent definitions don't break.
pub fn lookup_personality_mode(
    search_paths_high_to_low: &[&Path],
    mode_name: &str,
) -> Option<PersonalitySpec> {
    let owned: Vec<PathBuf> = search_paths_high_to_low
        .iter()
        .map(|p| p.to_path_buf())
        .collect();
    let manifests = match SkillLoader::new(owned).discover() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "personality_mode lookup: skill discovery failed at {:?}: {e}",
                search_paths_high_to_low
            );
            return None;
        },
    };
    let kebab = mode_name.replace('_', "-");
    manifests
        .into_iter()
        .find(|m| {
            m.inferred_kind() == InferredKind::PersonalityMode
                && (m.name == mode_name || m.name == kebab)
        })
        .and_then(|m| {
            m.metadata
                .magician
                .as_ref()
                .and_then(|mag| mag.personality.clone())
        })
}

/// Returns the names of every personality-mode skill installed across
/// `search_paths`. Order is loader-determined (alphabetical by name with
/// higher-precedence layers shadowing on collision).
pub fn list_personality_mode_names(search_paths_high_to_low: &[&Path]) -> Vec<String> {
    let owned: Vec<PathBuf> = search_paths_high_to_low
        .iter()
        .map(|p| p.to_path_buf())
        .collect();
    let manifests = match SkillLoader::new(owned).discover() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "personality_mode listing: skill discovery failed at {:?}: {e}",
                search_paths_high_to_low
            );
            return Vec::new();
        },
    };
    manifests
        .into_iter()
        .filter(|m| m.inferred_kind() == InferredKind::PersonalityMode)
        .map(|m| m.name)
        .collect()
}

/// Flatten a [`PersonalitySpec`] into the field-bag the
/// `personality_profile` memory tier expects. Mirrors the legacy
/// HashMap<String, String> from `serde_yaml::from_str(&template_yaml)`.
pub fn personality_spec_to_fields(spec: &PersonalitySpec) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert("voice".to_string(), spec.voice.clone());
    out.insert("expression_bias".to_string(), spec.expression_bias.clone());
    out.insert(
        "suppression_rules".to_string(),
        spec.suppression_rules.clone(),
    );
    out.insert(
        "expression_triggers".to_string(),
        spec.expression_triggers.clone(),
    );
    out.insert("active_mode".to_string(), spec.active_mode.clone());
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;

    fn write_personality_skill(parent: &Path, name: &str, voice: &str, mode: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {name} persona\n\
                 metadata:\n  magician:\n    personality:\n      \
                 active_mode: {mode}\n      voice: \"{voice}\"\n---\n# body\n"
            ),
        )
        .unwrap();
    }

    fn write_procedure_skill(parent: &Path, name: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: a procedure\n---\n# body\n"),
        )
        .unwrap();
    }

    #[test]
    fn lookup_returns_personality_when_skill_present() {
        let dir = tempfile::tempdir().unwrap();
        write_personality_skill(dir.path(), "witty", "Sharp + concise", "witty");
        let p = lookup_personality_mode(&[dir.path()], "witty").unwrap();
        assert_eq!(p.active_mode, "witty");
        assert!(p.voice.contains("Sharp"));
    }

    #[test]
    fn lookup_returns_none_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        write_personality_skill(dir.path(), "witty", "x", "witty");
        assert!(lookup_personality_mode(&[dir.path()], "brutal").is_none());
    }

    #[test]
    fn lookup_does_not_match_a_procedure_skill_with_the_same_name() {
        // Defence-in-depth: even if someone installed a procedure skill named
        // `witty`, we must NOT treat it as a personality mode.
        let dir = tempfile::tempdir().unwrap();
        write_procedure_skill(dir.path(), "witty");
        assert!(lookup_personality_mode(&[dir.path()], "witty").is_none());
    }

    #[test]
    fn list_returns_only_personality_mode_skills() {
        let dir = tempfile::tempdir().unwrap();
        write_personality_skill(dir.path(), "witty", "x", "witty");
        write_personality_skill(dir.path(), "brutal", "y", "brutal");
        write_procedure_skill(dir.path(), "review");
        let mut names = list_personality_mode_names(&[dir.path()]);
        names.sort();
        assert_eq!(names, vec!["brutal".to_string(), "witty".to_string()]);
    }

    #[test]
    fn lookup_bridges_underscore_to_kebab_for_legacy_callers() {
        // Legacy agent definitions use `default_personality: true_friend`
        // but AgentSkills v1 requires hyphenated names (`true-friend`).
        let dir = tempfile::tempdir().unwrap();
        write_personality_skill(dir.path(), "true-friend", "warm + honest", "true-friend");
        let p = lookup_personality_mode(&[dir.path()], "true_friend").unwrap();
        assert_eq!(p.active_mode, "true-friend");
    }

    #[test]
    fn higher_precedence_path_shadows_lower_on_collision() {
        let high = tempfile::tempdir().unwrap();
        let low = tempfile::tempdir().unwrap();
        write_personality_skill(high.path(), "witty", "high-voice", "witty");
        write_personality_skill(low.path(), "witty", "low-voice", "witty");
        let p = lookup_personality_mode(&[high.path(), low.path()], "witty").unwrap();
        assert_eq!(p.voice, "high-voice");
    }

    #[test]
    fn personality_spec_to_fields_round_trips_all_keys() {
        let spec = PersonalitySpec {
            active_mode: "witty".to_string(),
            voice: "v".to_string(),
            expression_bias: "eb".to_string(),
            suppression_rules: "sr".to_string(),
            expression_triggers: "et".to_string(),
        };
        let fields = personality_spec_to_fields(&spec);
        assert_eq!(fields["active_mode"], "witty");
        assert_eq!(fields["voice"], "v");
        assert_eq!(fields["expression_bias"], "eb");
        assert_eq!(fields["suppression_rules"], "sr");
        assert_eq!(fields["expression_triggers"], "et");
    }
}
