//! Phase 3 trust policy enforcer scaffolding.

use std::{collections::HashSet, fs, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::types::{ActionPattern, ToolActionPattern, TrustLevel, TrustPolicy};

pub const TRUST_POLICIES_TEMPLATE_YAML: &str = include_str!("builtin/trust_policies.template.yaml");
pub const TRUST_POLICIES_DEFAULT_YAML: &str = include_str!("builtin/trust_policies.default.yaml");

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicyFile {
    #[serde(default)]
    pub trust_policies: Vec<TrustPolicy>,
}

#[derive(Debug, Error)]
pub enum TrustPolicyError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("trust policy validation error: {0}")]
    Validation(String),
}

#[derive(Debug, Clone)]
pub struct TrustPolicyEnforcer {
    policies: Vec<TrustPolicy>,
}

impl TrustPolicyEnforcer {
    pub fn new(policies: Vec<TrustPolicy>) -> Result<Self, TrustPolicyError> {
        validate_policies(&policies)?;
        Ok(Self { policies })
    }

    pub fn from_yaml_str(yaml: &str) -> Result<Self, TrustPolicyError> {
        let file: TrustPolicyFile = serde_yaml::from_str(yaml)?;
        Self::new(file.trust_policies)
    }

    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self, TrustPolicyError> {
        let content = fs::read_to_string(path)?;
        Self::from_yaml_str(&content)
    }

    pub fn policies(&self) -> &[TrustPolicy] {
        &self.policies
    }

    pub fn is_allowed(&self, level: &TrustLevel, tool: &str, action: &str) -> bool {
        level.is_action_allowed(tool, action, &self.policies)
    }

    /// Check whether a trust level is defined in the loaded policies.
    ///
    /// Returns `false` for typo'd or unrecognized levels — callers should
    /// surface this at agent registration time rather than silently defaulting
    /// to deny-all at runtime.
    pub fn has_level(&self, level: &str) -> bool {
        let requested_level = TrustLevel::canonicalized_value(level);
        self.policies
            .iter()
            .any(|policy| policy.level.trim().eq_ignore_ascii_case(&requested_level))
    }

    /// Exact/case-sensitive level existence check for strict API admission.
    ///
    /// This preserves existing create/update/manual-trigger boundary behavior
    /// where trust-level spelling must exactly match the policy file.
    pub fn has_level_exact(&self, level: &str) -> bool {
        self.policies.iter().any(|p| p.level == level)
    }
}

/// Bound on cached trust policy files (one per scope). Past it the cache
/// starts over rather than tracking recency.
const TRUST_POLICY_CACHE_MAX_ENTRIES: usize = 256;

#[derive(Default)]
struct TrustPolicyCacheState {
    /// Path -> (content digest, enforcer parsed from that content).
    entries: std::collections::HashMap<std::path::PathBuf, (String, Arc<TrustPolicyEnforcer>)>,
    parses: std::collections::HashMap<std::path::PathBuf, u64>,
}

fn trust_policy_cache() -> std::sync::MutexGuard<'static, TrustPolicyCacheState> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<TrustPolicyCacheState>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read a trust policy file and return its enforcer with the hex blake3
/// digest of its content, parsing the YAML only when that content changed.
///
/// Chat reads the scope's policy on every turn. The file is still read each
/// time — a few KB, and the digest is part of the turn's policy identity —
/// but the parse and validation are served from a cache keyed by path and
/// content digest, so an edit (any byte) is seen on the next call and an
/// unchanged file shares one `Arc`. A file that fails to read or parse fails
/// exactly as an uncached load would.
pub fn load_trust_policy_file_cached(
    path: impl AsRef<Path>,
) -> Result<(Arc<TrustPolicyEnforcer>, String), TrustPolicyError> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    let digest = blake3::hash(content.as_bytes()).to_hex().to_string();
    let cached = trust_policy_cache()
        .entries
        .get(path)
        .filter(|(cached_digest, _)| *cached_digest == digest)
        .map(|(_, enforcer)| Arc::clone(enforcer));
    if let Some(enforcer) = cached {
        return Ok((enforcer, digest));
    }
    let parsed = TrustPolicyEnforcer::from_yaml_str(&content);
    let mut cache = trust_policy_cache();
    *cache.parses.entry(path.to_path_buf()).or_default() += 1;
    let enforcer = Arc::new(parsed?);
    if cache.entries.len() >= TRUST_POLICY_CACHE_MAX_ENTRIES && !cache.entries.contains_key(path) {
        cache.entries.clear();
        cache.parses.clear();
    }
    cache
        .entries
        .insert(path.to_path_buf(), (digest.clone(), Arc::clone(&enforcer)));
    Ok((enforcer, digest))
}

/// Drop one trust policy file's parsed enforcer. Returns whether one was
/// cached. Not needed for correctness (every load compares the content
/// digest); for callers that want the next load to parse.
pub fn invalidate_trust_policy_cache(path: impl AsRef<Path>) -> bool {
    trust_policy_cache().entries.remove(path.as_ref()).is_some()
}

/// How many times this process parsed the trust policy at `path`.
pub fn trust_policy_parse_count(path: impl AsRef<Path>) -> u64 {
    trust_policy_cache()
        .parses
        .get(path.as_ref())
        .copied()
        .unwrap_or(0)
}

impl TrustPolicyFile {
    /// Recommended trust policy defaults for standard deployment tiers.
    ///
    /// Mirrors TRUE_AGENTS spec defaults:
    /// - `builtin`/`local`: allow all
    /// - `reviewed`: allow all but deny outbound report channels
    /// - `untrusted`: allow read-only browser/files/search and deny mutation/exfiltration paths
    pub fn recommended_defaults() -> Self {
        Self {
            trust_policies: vec![
                TrustPolicy {
                    level: TrustLevel::BUILTIN.to_string(),
                    allow: vec![ToolActionPattern {
                        tool: "*".to_string(),
                        action: ActionPattern::Single("*".to_string()),
                    }],
                    deny: Vec::new(),
                },
                TrustPolicy {
                    level: TrustLevel::LOCAL.to_string(),
                    allow: vec![ToolActionPattern {
                        tool: "*".to_string(),
                        action: ActionPattern::Single("*".to_string()),
                    }],
                    deny: Vec::new(),
                },
                TrustPolicy {
                    level: TrustLevel::REVIEWED.to_string(),
                    allow: vec![ToolActionPattern {
                        tool: "*".to_string(),
                        action: ActionPattern::Single("*".to_string()),
                    }],
                    deny: vec![
                        ToolActionPattern {
                            tool: "report".to_string(),
                            action: ActionPattern::Multiple(vec![
                                "email".to_string(),
                                "slack".to_string(),
                            ]),
                        },
                        ToolActionPattern {
                            tool: "treasurer".to_string(),
                            action: ActionPattern::Single("execute".to_string()),
                        },
                    ],
                },
                TrustPolicy {
                    level: TrustLevel::UNTRUSTED.to_string(),
                    allow: vec![
                        ToolActionPattern {
                            tool: "files".to_string(),
                            action: ActionPattern::Single("read".to_string()),
                        },
                        ToolActionPattern {
                            tool: "search".to_string(),
                            action: ActionPattern::Single("*".to_string()),
                        },
                    ],
                    deny: vec![
                        ToolActionPattern {
                            tool: "shell".to_string(),
                            action: ActionPattern::Single("*".to_string()),
                        },
                        ToolActionPattern {
                            tool: "files".to_string(),
                            action: ActionPattern::Single("write".to_string()),
                        },
                        ToolActionPattern {
                            tool: "browser".to_string(),
                            action: ActionPattern::Single("execute".to_string()),
                        },
                        ToolActionPattern {
                            tool: "report".to_string(),
                            action: ActionPattern::Single("*".to_string()),
                        },
                        ToolActionPattern {
                            tool: "treasurer".to_string(),
                            action: ActionPattern::Single("execute".to_string()),
                        },
                    ],
                },
            ],
        }
    }

    /// Commented template with supported trust-policy features and active defaults.
    pub fn recommended_template_yaml() -> &'static str {
        TRUST_POLICIES_TEMPLATE_YAML
    }

    /// Concrete default trust policy YAML for direct runtime use.
    pub fn recommended_defaults_yaml() -> &'static str {
        TRUST_POLICIES_DEFAULT_YAML
    }
}

fn validate_policies(policies: &[TrustPolicy]) -> Result<(), TrustPolicyError> {
    let mut seen_levels = HashSet::new();
    for policy in policies {
        let level = policy.level.trim();
        if level.is_empty() {
            return Err(TrustPolicyError::Validation(
                "trust policy level must not be empty".to_string(),
            ));
        }
        if level != policy.level {
            return Err(TrustPolicyError::Validation(format!(
                "trust policy level `{}` must not include surrounding whitespace",
                policy.level
            )));
        }
        let level_key = level.to_ascii_lowercase();
        if !seen_levels.insert(level_key) {
            return Err(TrustPolicyError::Validation(format!(
                "duplicate trust policy level `{level}`"
            )));
        }

        validate_tool_action_patterns(level, "allow", &policy.allow)?;
        validate_tool_action_patterns(level, "deny", &policy.deny)?;
    }
    Ok(())
}

fn validate_tool_action_patterns(
    level: &str,
    list_name: &str,
    patterns: &[ToolActionPattern],
) -> Result<(), TrustPolicyError> {
    for (index, pattern) in patterns.iter().enumerate() {
        let tool = pattern.tool.trim();
        if tool.is_empty() {
            return Err(TrustPolicyError::Validation(format!(
                "trust policy level `{level}` {list_name}[{index}] tool must not be empty"
            )));
        }
        if tool != pattern.tool {
            return Err(TrustPolicyError::Validation(format!(
                "trust policy level `{level}` {list_name}[{index}] tool must not include surrounding whitespace"
            )));
        }

        match &pattern.action {
            ActionPattern::Single(action) => {
                let trimmed_action = action.trim();
                if trimmed_action.is_empty() {
                    return Err(TrustPolicyError::Validation(format!(
                        "trust policy level `{level}` {list_name}[{index}] action must not be empty"
                    )));
                }
                if trimmed_action != action {
                    return Err(TrustPolicyError::Validation(format!(
                        "trust policy level `{level}` {list_name}[{index}] action must not include surrounding whitespace"
                    )));
                }
            },
            ActionPattern::Multiple(actions) => {
                if actions.is_empty() {
                    return Err(TrustPolicyError::Validation(format!(
                        "trust policy level `{level}` {list_name}[{index}] action list must not be empty"
                    )));
                }
                for (action_index, action) in actions.iter().enumerate() {
                    let trimmed_action = action.trim();
                    if trimmed_action.is_empty() {
                        return Err(TrustPolicyError::Validation(format!(
                            "trust policy level `{level}` {list_name}[{index}] action[{action_index}] must not be empty"
                        )));
                    }
                    if trimmed_action != action {
                        return Err(TrustPolicyError::Validation(format!(
                            "trust policy level `{level}` {list_name}[{index}] action[{action_index}] must not include surrounding whitespace"
                        )));
                    }
                }
            },
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    const ALLOW_ALL: &str = r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
"#;

    const DENY_REPORT_EMAIL: &str = r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
    deny:
      - tool: report
        action: email
"#;

    const BROKEN: &str = "trust_policies: [unclosed\n";

    #[test]
    fn cached_trust_policy_parses_once_and_again_after_an_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trust_policies.yaml");
        fs::write(&path, ALLOW_ALL).expect("write policy");
        let reviewed = TrustLevel("reviewed".to_string());

        let (first, first_digest) = load_trust_policy_file_cached(&path).expect("load");
        let (second, second_digest) = load_trust_policy_file_cached(&path).expect("load");
        assert_eq!(trust_policy_parse_count(&path), 1);
        assert!(Arc::ptr_eq(&first, &second), "one shared Arc");
        assert_eq!(first_digest, second_digest);
        assert_eq!(
            first_digest,
            blake3::hash(ALLOW_ALL.as_bytes()).to_hex().to_string()
        );
        assert!(first.is_allowed(&reviewed, "report", "email"));

        fs::write(&path, DENY_REPORT_EMAIL).expect("edit policy");
        let (edited, edited_digest) = load_trust_policy_file_cached(&path).expect("load");
        assert_eq!(trust_policy_parse_count(&path), 2);
        assert_ne!(edited_digest, first_digest);
        assert!(!edited.is_allowed(&reviewed, "report", "email"));

        assert!(invalidate_trust_policy_cache(&path));
        load_trust_policy_file_cached(&path).expect("load");
        assert_eq!(trust_policy_parse_count(&path), 3);
    }

    #[test]
    fn cached_trust_policy_fails_like_an_uncached_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trust_policies.yaml");
        fs::write(&path, ALLOW_ALL).expect("write policy");
        load_trust_policy_file_cached(&path).expect("load");

        fs::write(&path, BROKEN).expect("break policy");
        assert!(matches!(
            load_trust_policy_file_cached(&path),
            Err(TrustPolicyError::Yaml(_))
        ));
        fs::remove_file(&path).expect("remove policy");
        assert!(matches!(
            load_trust_policy_file_cached(&path),
            Err(TrustPolicyError::Io(_))
        ));
    }

    #[test]
    fn deny_rule_takes_precedence_over_allow() {
        let enforcer = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
    deny:
      - tool: report
        action: email
"#,
        )
        .unwrap();

        let reviewed = TrustLevel("reviewed".to_string());
        assert!(!enforcer.is_allowed(&reviewed, "report", "email"));
        assert!(enforcer.is_allowed(&reviewed, "browser", "navigate"));
    }

    #[test]
    fn unknown_level_is_denied() {
        let enforcer = TrustPolicyEnforcer::new(vec![]).unwrap();
        let unknown = TrustLevel("unknown".to_string());
        assert!(!enforcer.is_allowed(&unknown, "browser", "navigate"));
    }

    #[test]
    fn duplicate_levels_are_rejected() {
        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
  - level: reviewed
    deny:
      - tool: browser
        action: submit
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));
    }

    #[test]
    fn empty_level_is_rejected() {
        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: " "
    allow:
      - tool: "*"
        action: "*"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));
    }

    #[test]
    fn has_level_detects_defined_and_missing_levels() {
        let enforcer = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
"#,
        )
        .unwrap();
        assert!(enforcer.has_level("reviewed"));
        assert!(enforcer.has_level("Reviewed"));
        assert!(enforcer.has_level(" reviewed "));
        assert!(enforcer.has_level_exact("reviewed"));
        assert!(!enforcer.has_level_exact("Reviewed"));
        assert!(!enforcer.has_level("reviwed")); // typo — not defined
        assert!(!enforcer.has_level("unknown"));
    }

    #[test]
    fn runtime_is_allowed_matches_level_case_insensitively() {
        let enforcer = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: browser
        action: navigate
"#,
        )
        .unwrap();

        let reviewed_mixed_case = TrustLevel("Reviewed".to_string());
        assert!(enforcer.is_allowed(&reviewed_mixed_case, "browser", "navigate"));
    }

    #[test]
    fn legacy_standard_alias_maps_to_local_policy() {
        let enforcer = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: local
    allow:
      - tool: browser
        action: navigate
"#,
        )
        .unwrap();

        let legacy_standard = TrustLevel("standard".to_string());
        assert!(enforcer.has_level("standard"));
        assert!(enforcer.is_allowed(&legacy_standard, "browser", "navigate"));
    }

    #[test]
    fn runtime_is_allowed_matches_tool_case_insensitively() {
        let enforcer = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
    deny:
      - tool: browser
        action: submit
"#,
        )
        .unwrap();

        let reviewed = TrustLevel("reviewed".to_string());
        assert!(enforcer.is_allowed(&reviewed, "Browser", "navigate"));
        assert!(!enforcer.is_allowed(&reviewed, "BROWSER", "submit"));
    }

    #[test]
    fn duplicate_levels_are_rejected_case_insensitively() {
        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: "*"
        action: "*"
  - level: Reviewed
    deny:
      - tool: browser
        action: submit
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));
    }

    #[test]
    fn recommended_defaults_cover_builtin_levels_and_untrusted_denials() {
        let defaults = TrustPolicyFile::recommended_defaults();
        let enforcer = TrustPolicyEnforcer::new(defaults.trust_policies).unwrap();
        assert!(enforcer.has_level(TrustLevel::BUILTIN));
        assert!(enforcer.has_level(TrustLevel::LOCAL));
        assert!(enforcer.has_level(TrustLevel::REVIEWED));
        assert!(enforcer.has_level(TrustLevel::UNTRUSTED));

        let untrusted = TrustLevel(TrustLevel::UNTRUSTED.to_string());
        assert!(!enforcer.is_allowed(&untrusted, "shell", "execute"));
        assert!(!enforcer.is_allowed(&untrusted, "report", "email"));
        assert!(!enforcer.is_allowed(&untrusted, "treasurer", "execute"));
        assert!(!enforcer.is_allowed(&untrusted, "browser", "execute"));

        let reviewed = TrustLevel(TrustLevel::REVIEWED.to_string());
        assert!(!enforcer.is_allowed(&reviewed, "treasurer", "execute"));
        assert!(enforcer.is_allowed(&reviewed, "browser", "execute"));
    }

    #[test]
    fn recommended_template_parses_and_covers_builtin_levels() {
        let enforcer =
            TrustPolicyEnforcer::from_yaml_str(TrustPolicyFile::recommended_template_yaml())
                .expect("template should parse as TrustPolicyFile");
        assert!(enforcer.has_level(TrustLevel::BUILTIN));
        assert!(enforcer.has_level(TrustLevel::LOCAL));
        assert!(enforcer.has_level(TrustLevel::REVIEWED));
        assert!(enforcer.has_level(TrustLevel::UNTRUSTED));
    }

    #[test]
    fn recommended_default_yaml_parses_and_matches_programmatic_defaults() {
        let from_yaml: TrustPolicyFile =
            serde_yaml::from_str(TrustPolicyFile::recommended_defaults_yaml())
                .expect("default yaml should parse");
        let programmatic = TrustPolicyFile::recommended_defaults();
        assert_eq!(
            serde_yaml::to_value(from_yaml).expect("yaml conversion should succeed"),
            serde_yaml::to_value(programmatic)
                .expect("programmatic defaults conversion should succeed")
        );
    }

    #[test]
    fn malformed_pattern_entries_are_rejected() {
        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: " "
        action: "*"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));

        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: browser
        action: []
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));

        let err = TrustPolicyEnforcer::from_yaml_str(
            r#"
trust_policies:
  - level: reviewed
    allow:
      - tool: browser
        action:
          - "navigate"
          - "  "
"#,
        )
        .unwrap_err();
        assert!(matches!(err, TrustPolicyError::Validation(_)));
    }
}
