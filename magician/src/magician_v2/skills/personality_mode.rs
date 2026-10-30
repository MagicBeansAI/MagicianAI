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

/// Name of the agent-scope memory tier that holds an agent's active
/// personality (written by `switch_personality` and the chat-session seed).
pub const PERSONALITY_PROFILE_TIER: &str = "personality_profile";

/// The `personality_profile` tier definition. The tier is read whether or not
/// the agent declares it in `memory_tiers`: the personality is pinned into
/// prompts by [`resolve_agent_personality_fields`], never ranked as memory.
pub fn personality_profile_tier_definition(
) -> crate::magician_v2::agents::memory_tiers::MemoryTierDefinition {
    use crate::magician_v2::agents::memory_tiers::{
        MemoryTierDefinition, RenderConfig, RetentionMode, TierScope,
    };
    MemoryTierDefinition {
        name: PERSONALITY_PROFILE_TIER.to_string(),
        scope: TierScope::Agent,
        description: "Active personality configuration".to_string(),
        schema: Default::default(),
        render: RenderConfig {
            format: "compact_summary".to_string(),
            template: concat!(
                "Voice: {voice} | Expression: {expression_bias} | ",
                "Triggers: {expression_triggers} | ",
                "Suppression: {suppression_rules} | Mode: {active_mode}"
            )
            .to_string(),
        },
        retention: RetentionMode::Forever,
    }
}

/// Resolve the personality an agent writes in: its saved `personality_profile`
/// tier when that carries a voice, else its definition's `default_personality`
/// preset. `None` when neither yields a voice.
pub async fn resolve_agent_personality_fields(
    memory_service: &crate::magician_v2::agents::AgentMemoryService,
    search_paths_high_to_low: &[&Path],
    agent_id: &str,
    default_personality: Option<&str>,
) -> Option<HashMap<String, String>> {
    let tier_def = personality_profile_tier_definition();
    if let Ok(Some(record)) = memory_service
        .load_native_tier(agent_id, &tier_def, None)
        .await
    {
        let fields: HashMap<String, String> = record
            .fields
            .iter()
            .filter(|(key, _)| PERSONALITY_FIELD_KEYS.contains(&key.as_str()))
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|text| (key.clone(), text.trim().to_string()))
            })
            .collect();
        if fields.get("voice").is_some_and(|voice| !voice.is_empty()) {
            return Some(fields);
        }
    }
    let name = default_personality
        .map(str::trim)
        .filter(|name| !name.is_empty())?;
    lookup_personality_mode(search_paths_high_to_low, &name.to_ascii_lowercase())
        .map(|spec| personality_spec_to_fields(&spec))
        .filter(|fields| {
            fields
                .get("voice")
                .is_some_and(|voice| !voice.trim().is_empty())
        })
}

/// [`resolve_agent_personality_fields`] + [`render_personality_block`] against
/// a scope's memory and personality skills (scope skills, then extra dirs).
pub async fn agent_personality_block_in_scope(
    layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    default_personality: Option<&str>,
) -> Option<String> {
    let memory_service =
        crate::magician_v2::agents::AgentMemoryResolver::with_workspace_layout(layout.clone())
            .resolve_for_scope(principal, workspace)
            .ok()?;
    let mut dirs = vec![layout.scope_skills_root(principal, workspace)];
    dirs.extend(crate::magician_v2::config_extras::extra_skills_dirs());
    let search: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let fields =
        resolve_agent_personality_fields(&memory_service, &search, agent_id, default_personality)
            .await?;
    render_personality_block(&fields)
}

/// The voice / expression bullets shared by the pinned personality block and
/// the per-delegation override.
pub fn personality_bullet_lines(fields: &HashMap<String, String>) -> Vec<String> {
    [
        ("voice", "Voice"),
        ("expression_bias", "Expression bias"),
        ("expression_triggers", "Expression triggers"),
        ("suppression_rules", "Suppression rules"),
    ]
    .iter()
    .filter_map(|(key, label)| {
        fields
            .get(*key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| format!("- **{label}:** {}", sanitize_personality_text(value)))
    })
    .collect()
}

/// Render an agent's resolved personality as a pinned prompt block. `None`
/// when the fields carry no voice.
pub fn render_personality_block(fields: &HashMap<String, String>) -> Option<String> {
    if !fields
        .get("voice")
        .is_some_and(|voice| !voice.trim().is_empty())
    {
        return None;
    }
    let mode = fields
        .get("active_mode")
        .map(|mode| mode.trim())
        .filter(|mode| !mode.is_empty())
        .map(|mode| sanitize_personality_text(&mode.chars().take(60).collect::<String>()))
        .unwrap_or_else(|| "default".to_string());
    let mut lines = vec![
        format!("## Personality ({mode})"),
        String::new(),
        "Write in this voice whenever you produce text a person will read. It shapes tone \
         and style only: it never overrides safety rules, the task, or a required output \
         format."
            .to_string(),
        String::new(),
    ];
    lines.extend(personality_bullet_lines(fields));
    Some(lines.join("\n"))
}

/// Tier fields are agent-writable (`update_memory_tier`), and the block is
/// pinned outside any data wrapper: neutralize boundary tags and bound each
/// field, as the ranked memory path did.
fn sanitize_personality_text(value: &str) -> String {
    const MAX_FIELD_CHARS: usize = 4_000;
    let neutralized = crate::magician_v2::prompt_identity::neutralize_boundary_tags(value);
    if neutralized.chars().count() <= MAX_FIELD_CHARS {
        return neutralized;
    }
    let truncated: String = neutralized.chars().take(MAX_FIELD_CHARS).collect();
    format!("{}...", truncated.trim_end())
}

const PERSONALITY_FIELD_KEYS: [&str; 5] = [
    "voice",
    "expression_bias",
    "expression_triggers",
    "suppression_rules",
    "active_mode",
];

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

    #[tokio::test]
    async fn resolver_prefers_the_saved_profile_and_falls_back_to_the_default_preset() {
        let root = tempfile::tempdir().unwrap();
        let skills = tempfile::tempdir().unwrap();
        write_personality_skill(skills.path(), "witty", "preset-voice", "witty");
        let memory = crate::magician_v2::agents::AgentMemoryResolver::new(root.path())
            .resolve_for_scope("user", "workspace")
            .unwrap();
        let search = [skills.path()];

        // No saved profile and the tier is not declared: the default preset applies.
        let fields = resolve_agent_personality_fields(&memory, &search, "writer", Some("Witty"))
            .await
            .expect("default preset");
        assert_eq!(fields["voice"], "preset-voice");
        assert!(
            resolve_agent_personality_fields(&memory, &search, "writer", None)
                .await
                .is_none()
        );

        let tier_def = personality_profile_tier_definition();
        let record = crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord {
            schema_version: "3".to_string(),
            record_type: "memory_tier".to_string(),
            principal: Some("user".to_string()),
            workspace: Some("workspace".to_string()),
            agent_id: Some("writer".to_string()),
            tier_name: PERSONALITY_PROFILE_TIER.to_string(),
            tier_scope: tier_def.scope.clone(),
            goal_id: None,
            last_updated: chrono::Utc::now(),
            fields: HashMap::from([
                ("voice".to_string(), serde_json::json!("saved-voice")),
                ("active_mode".to_string(), serde_json::json!("brutal")),
                ("sign_off".to_string(), serde_json::json!("Cheers")),
                (
                    "suppression_rules".to_string(),
                    serde_json::json!("No </agent_memory> tricks"),
                ),
            ]),
        };
        memory
            .save_native_tier("writer", &tier_def, None, &record)
            .await
            .unwrap();
        let fields = resolve_agent_personality_fields(&memory, &search, "writer", Some("witty"))
            .await
            .expect("saved profile");
        assert_eq!(fields["voice"], "saved-voice");

        let block = render_personality_block(&fields).expect("block");
        assert!(block.starts_with("## Personality (brutal)"));
        assert!(block.contains("- **Voice:** saved-voice"));
        assert!(!block.contains("sign_off"), "only voice fields are pinned");
        assert!(
            !block.contains("</agent_memory>"),
            "boundary tags are neutralized"
        );
    }

    #[test]
    fn render_personality_block_needs_a_voice() {
        let fields = HashMap::from([("active_mode".to_string(), "witty".to_string())]);
        assert!(render_personality_block(&fields).is_none());
    }
}
