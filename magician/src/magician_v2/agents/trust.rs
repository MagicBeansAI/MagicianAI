//! Phase 3 trust policy enforcer scaffolding.

use std::{collections::HashSet, fs, path::Path};

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
