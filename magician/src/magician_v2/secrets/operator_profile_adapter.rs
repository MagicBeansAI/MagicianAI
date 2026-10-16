//! Product-owned projection of provider-neutral operator profile metadata.
//!
//! The governed runtime supplies the validated auth contract. This adapter
//! selects matching non-secret profile metadata from `tool_runtime_profiles`
//! and derives the existing credential directory exclusively from the
//! contract's scoped-storage declaration. It contains no provider, executable,
//! environment-variable, or tool-specific naming policy.

use serde::Deserialize;
use tool_runtime_core::{
    credential_profile_store::{LegacyCredentialProfileReference, LegacyCredentialProfileSet},
    credential_profiles::{
        CredentialProfileAlias, CredentialProfileAvailability, CredentialProfileBinding,
        CredentialProfileKey, CredentialProfileMetadata, CredentialProfileRevision,
        CredentialScope, ExpectedCredentialIdentity, MAX_PROFILES_PER_SCOPE,
    },
    manifest::{AuthStorage, ProfileSelection, SkillRuntimeContract},
    scoped_paths::{ScopedPathAuthority, ScopedPathComponent, ScopedPathErrorCode},
};

const MAX_OPERATOR_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_OPERATOR_DOCUMENT_DEPTH: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorProfileAdapterError {
    InvalidConfiguration,
    UnsupportedContract,
    TooManyProfiles,
    InvalidProfile,
    DuplicateProfile,
    ProfilePathUnavailable,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorProfileInput {
    provider: String,
    storage_namespace: String,
    alias: String,
    #[serde(default)]
    expected_identity: Option<String>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
struct OperatorConfigProjection {
    #[serde(default)]
    tool_runtime_profiles: Vec<OperatorProfileInput>,
}

#[derive(Debug, Clone)]
pub struct OperatorProfileAdapter {
    provider: String,
    storage_namespace: String,
    partition_by_profile: bool,
    default_alias: Option<CredentialProfileAlias>,
}

impl OperatorProfileAdapter {
    pub fn from_contract(
        contract: &SkillRuntimeContract,
    ) -> Result<Self, OperatorProfileAdapterError> {
        let provider = contract
            .auth
            .provider
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(OperatorProfileAdapterError::UnsupportedContract)?;
        let AuthStorage::ScopedDirectory {
            namespace,
            partition_by_profile,
        } = &contract.auth.storage
        else {
            return Err(OperatorProfileAdapterError::UnsupportedContract);
        };
        let default_alias = match &contract.auth.profile_selection {
            ProfileSelection::Selectable { default } => default
                .as_deref()
                .map(CredentialProfileAlias::new)
                .transpose()
                .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?,
            ProfileSelection::Fixed { .. } => None,
            _ => return Err(OperatorProfileAdapterError::UnsupportedContract),
        };
        Ok(Self {
            provider: provider.to_owned(),
            storage_namespace: namespace.clone(),
            partition_by_profile: *partition_by_profile,
            default_alias,
        })
    }

    pub fn normalize(
        &self,
        source: &str,
        scope: CredentialScope,
        authority: &ScopedPathAuthority,
    ) -> Result<LegacyCredentialProfileSet, OperatorProfileAdapterError> {
        validate_source(source)?;
        let projection: OperatorConfigProjection = serde_yaml::from_str(source)
            .map_err(|_| OperatorProfileAdapterError::InvalidConfiguration)?;
        if projection.tool_runtime_profiles.len() > MAX_PROFILES_PER_SCOPE {
            return Err(OperatorProfileAdapterError::TooManyProfiles);
        }

        let matching = projection
            .tool_runtime_profiles
            .into_iter()
            .filter(|profile| {
                profile.provider == self.provider
                    && profile.storage_namespace == self.storage_namespace
            });
        let mut references = Vec::new();
        for profile in matching {
            if references.len() == MAX_PROFILES_PER_SCOPE {
                return Err(OperatorProfileAdapterError::TooManyProfiles);
            }
            let alias = CredentialProfileAlias::new(profile.alias)
                .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?;
            let key = CredentialProfileKey::new(
                scope.clone(),
                self.provider.clone(),
                alias.as_str(),
                CredentialProfileBinding::Provider,
            )
            .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?;
            let metadata = CredentialProfileMetadata::new(
                key.clone(),
                profile
                    .expected_identity
                    .map(ExpectedCredentialIdentity::new)
                    .transpose()
                    .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?,
                self.default_alias.as_ref() == Some(&alias),
                if profile.disabled {
                    CredentialProfileAvailability::Disabled
                } else {
                    CredentialProfileAvailability::Enabled
                },
                CredentialProfileRevision::new(1)
                    .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?,
            )
            .map_err(|_| OperatorProfileAdapterError::InvalidProfile)?;
            let directory =
                profile_directory(&self.storage_namespace, self.partition_by_profile, &alias)?;
            let profile_root = match authority.resolve_profile_root(&key, directory) {
                Ok(path) => Some(path),
                Err(error) if error.code == ScopedPathErrorCode::Missing => None,
                Err(_) => return Err(OperatorProfileAdapterError::ProfilePathUnavailable),
            };
            references.push(
                LegacyCredentialProfileReference::new(metadata, profile_root).map_err(|error| {
                    match error.code {
                        tool_runtime_core::credential_profiles::CredentialProfileErrorCode::DuplicateProfile => {
                            OperatorProfileAdapterError::DuplicateProfile
                        },
                        _ => OperatorProfileAdapterError::InvalidProfile,
                    }
                })?,
            );
        }
        LegacyCredentialProfileSet::new(scope, references).map_err(|error| match error.code {
            tool_runtime_core::credential_profiles::CredentialProfileErrorCode::DuplicateProfile => {
                OperatorProfileAdapterError::DuplicateProfile
            },
            tool_runtime_core::credential_profiles::CredentialProfileErrorCode::CollectionTooLarge => {
                OperatorProfileAdapterError::TooManyProfiles
            },
            _ => OperatorProfileAdapterError::InvalidProfile,
        })
    }
}

fn profile_directory(
    namespace: &str,
    partition_by_profile: bool,
    alias: &CredentialProfileAlias,
) -> Result<ScopedPathComponent, OperatorProfileAdapterError> {
    let directory = if partition_by_profile {
        format!("{namespace}-{}", alias.as_str())
    } else {
        namespace.to_owned()
    };
    ScopedPathComponent::new(directory).map_err(|_| OperatorProfileAdapterError::InvalidProfile)
}

fn validate_source(source: &str) -> Result<(), OperatorProfileAdapterError> {
    if source.len() > MAX_OPERATOR_CONFIG_BYTES {
        return Err(OperatorProfileAdapterError::TooManyProfiles);
    }
    let mut indentation = Vec::<usize>::new();
    let mut block_scalar_indent = None;
    let mut flow_depth = 0usize;
    for line in source.lines() {
        let leading = line.bytes().take_while(|byte| *byte == b' ').count();
        if let Some(indent) = block_scalar_indent {
            if leading > indent {
                continue;
            }
            block_scalar_indent = None;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        while indentation.last().is_some_and(|value| *value >= leading) {
            indentation.pop();
        }
        indentation.push(leading);
        let mut compact = 0usize;
        let mut remainder = trimmed;
        while let Some(rest) = remainder
            .strip_prefix("- ")
            .or_else(|| remainder.strip_prefix("? "))
        {
            compact = compact.saturating_add(1);
            remainder = rest;
        }
        if indentation.len().saturating_add(compact) > MAX_OPERATOR_DOCUMENT_DEPTH {
            return Err(OperatorProfileAdapterError::InvalidConfiguration);
        }

        let mut single_quoted = false;
        let mut double_quoted = false;
        let mut escaped = false;
        let mut token_start = true;
        for character in line.chars() {
            if escaped {
                escaped = false;
                token_start = false;
                continue;
            }
            if double_quoted && character == '\\' {
                escaped = true;
                continue;
            }
            match character {
                '\'' if !double_quoted => {
                    single_quoted = !single_quoted;
                    token_start = false;
                },
                '"' if !single_quoted => {
                    double_quoted = !double_quoted;
                    token_start = false;
                },
                '#' if !single_quoted && !double_quoted => break,
                '&' | '*' | '!' if !single_quoted && !double_quoted && token_start => {
                    return Err(OperatorProfileAdapterError::InvalidConfiguration);
                },
                '[' | '{' if !single_quoted && !double_quoted => {
                    flow_depth = flow_depth.saturating_add(1);
                    if flow_depth > MAX_OPERATOR_DOCUMENT_DEPTH {
                        return Err(OperatorProfileAdapterError::InvalidConfiguration);
                    }
                    token_start = true;
                },
                ']' | '}' if !single_quoted && !double_quoted => {
                    flow_depth = flow_depth.saturating_sub(1);
                    token_start = false;
                },
                value if !single_quoted && !double_quoted => {
                    token_start =
                        value.is_whitespace() || matches!(value, '[' | '{' | ',' | ':' | '?' | '-');
                },
                _ => token_start = false,
            }
        }
        if has_block_scalar_indicator(line) {
            block_scalar_indent = Some(leading);
        }
    }
    Ok(())
}

fn has_block_scalar_indicator(line: &str) -> bool {
    let without_comment = line.split('#').next().unwrap_or_default().trim_end();
    let Some((_, indicator)) = without_comment.rsplit_once(':') else {
        return false;
    };
    let mut characters = indicator.trim().chars();
    matches!(characters.next(), Some('|' | '>'))
        && characters.all(|character| character.is_ascii_digit() || matches!(character, '+' | '-'))
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;
    use tool_runtime_core::{
        credential_profiles::{CredentialProfileRegistry, CredentialScope},
        manifest::{
            AuthContract, AuthKind, AuthRequirement, CliInteraction, RuntimeLimits,
            RuntimeProtocol, RuntimeRequirements, SkillRuntimeContract,
            SkillRuntimeContractVersion, StdinContract, WorkingDirectoryContract,
        },
        scoped_paths::ScopedPathAuthority,
    };

    use super::*;

    struct Fixture {
        _root: TempDir,
        scopes_root: std::path::PathBuf,
        scope: CredentialScope,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary root");
            let canonical_root = fs::canonicalize(root.path()).expect("canonical temp root");
            let scopes_root = canonical_root.join("scopes");
            let scope = CredentialScope::new("owner", "default").unwrap();
            let principal = scopes_root.join("owner");
            let workspace = principal.join("default");
            let auth = workspace.join("auth");
            for path in [&canonical_root, &scopes_root, &principal, &workspace, &auth] {
                fs::create_dir_all(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self {
                _root: root,
                scopes_root,
                scope,
            }
        }

        fn authority(&self) -> ScopedPathAuthority {
            ScopedPathAuthority::open(&self.scopes_root).unwrap()
        }

        fn add_profile(&self, namespace: &str, alias: &str) {
            let path = self
                .scopes_root
                .join(format!("owner/default/auth/{namespace}-{alias}"));
            fs::create_dir(path.as_path()).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn contract(provider: &str, namespace: &str) -> SkillRuntimeContract {
        SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements::default(),
            runtime: RuntimeProtocol::Cli {
                command_prefix: Vec::new(),
                interaction: CliInteraction::Batch,
                stdin: StdinContract::default(),
                working_directory: WorkingDirectoryContract::default(),
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract {
                kind: AuthKind::CliProfile,
                requirement: AuthRequirement::Required,
                provider: Some(provider.to_owned()),
                profile_selection: ProfileSelection::Selectable {
                    default: Some("work".to_owned()),
                },
                storage: AuthStorage::ScopedDirectory {
                    namespace: namespace.to_owned(),
                    partition_by_profile: true,
                },
                ..AuthContract::default()
            },
            policy_floor: Default::default(),
        }
    }

    #[test]
    fn selects_only_matching_provider_and_namespace_without_exposing_unrelated_config() {
        let fixture = Fixture::new();
        fixture.add_profile("mail", "work");
        let source = r#"
secrets: {token: NEVER_PROJECT_THIS}
tool_runtime_profiles:
  - {provider: mail-provider, storage_namespace: mail, alias: work, expected_identity: owner@example.com}
  - {provider: other-provider, storage_namespace: other, alias: personal}
"#;
        let set = OperatorProfileAdapter::from_contract(&contract("mail-provider", "mail"))
            .unwrap()
            .normalize(source, fixture.scope.clone(), &fixture.authority())
            .unwrap();
        assert_eq!(set.len(), 1);
        let rendered = format!("{set:?}");
        assert!(!rendered.contains("NEVER_PROJECT_THIS"));
        assert!(!rendered.contains(fixture._root.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn one_generic_adapter_supports_a_second_cli_profile_provider() {
        let fixture = Fixture::new();
        fixture.add_profile("chat", "work");
        let source = r#"
tool_runtime_profiles:
  - {provider: chat-provider, storage_namespace: chat, alias: work, expected_identity: chat-owner}
"#;
        let set = OperatorProfileAdapter::from_contract(&contract("chat-provider", "chat"))
            .unwrap()
            .normalize(source, fixture.scope.clone(), &fixture.authority())
            .unwrap();
        let registry =
            tool_runtime_core::credential_profile_store::LocalCredentialProfileRegistry::open(
                fixture.scope.clone(),
                fixture
                    .authority()
                    .resolve_auth_root(&fixture.scope)
                    .unwrap(),
                set,
            )
            .unwrap();
        let snapshot = registry.snapshot(&fixture.scope).unwrap();
        assert_eq!(snapshot.profiles().len(), 1);
        assert_eq!(
            snapshot.profiles()[0].key().provider.as_str(),
            "chat-provider"
        );
    }

    #[test]
    fn malformed_duplicate_deep_and_unsafe_profiles_fail_closed() {
        let fixture = Fixture::new();
        let adapter =
            OperatorProfileAdapter::from_contract(&contract("mail-provider", "mail")).unwrap();
        for source in [
            "tool_runtime_profiles: [{provider: mail-provider, storage_namespace: mail, alias: work, extra: forbidden}]\n",
            "tool_runtime_profiles:\n  - {provider: mail-provider, storage_namespace: mail, alias: work}\n  - {provider: mail-provider, storage_namespace: mail, alias: work}\n",
            "tool_runtime_profiles: &profiles\n  - {provider: mail-provider, storage_namespace: mail, alias: work}\ncopy: *profiles\n",
            "tool_runtime_profiles: [{provider: mail-provider, storage_namespace: mail, alias: ../escape}]\n",
        ] {
            assert!(adapter
                .normalize(source, fixture.scope.clone(), &fixture.authority())
                .is_err());
        }
        let deep = format!(
            "unrelated: {}0{}\ntool_runtime_profiles: []\n",
            "[".repeat(MAX_OPERATOR_DOCUMENT_DEPTH + 1),
            "]".repeat(MAX_OPERATOR_DOCUMENT_DEPTH + 1)
        );
        assert_eq!(
            adapter
                .normalize(&deep, fixture.scope.clone(), &fixture.authority())
                .unwrap_err(),
            OperatorProfileAdapterError::InvalidConfiguration
        );

        fixture.add_profile("mail", "unsafe");
        let unsafe_path = fixture.scopes_root.join("owner/default/auth/mail-unsafe");
        fs::set_permissions(unsafe_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            adapter
                .normalize(
                    "tool_runtime_profiles: [{provider: mail-provider, storage_namespace: mail, alias: unsafe}]\n",
                    fixture.scope.clone(),
                    &fixture.authority(),
                )
                .unwrap_err(),
            OperatorProfileAdapterError::ProfilePathUnavailable
        );
    }

    #[test]
    fn maximum_generic_projection_and_hostile_depth_are_safe_on_a_small_stack() {
        std::thread::Builder::new()
            .name("operator-profile-adapter-small-stack".to_owned())
            .stack_size(128 * 1024)
            .spawn(|| {
                let fixture = Fixture::new();
                let source = format!(
                    "tool_runtime_profiles:\n{}",
                    (0..MAX_PROFILES_PER_SCOPE)
                        .map(|index| format!(
                            "  - provider: mail-provider\n    storage_namespace: mail\n    alias: p{index:03}\n    expected_identity: p{index:03}@example.com\n"
                        ))
                        .collect::<String>()
                );
                let adapter =
                    OperatorProfileAdapter::from_contract(&contract("mail-provider", "mail"))
                        .unwrap();
                let profiles = adapter
                    .normalize(&source, fixture.scope.clone(), &fixture.authority())
                    .expect("maximum projection");
                assert_eq!(profiles.len(), MAX_PROFILES_PER_SCOPE);

                let deep = format!(
                    "unrelated: {}0{}\ntool_runtime_profiles: []\n",
                    "[".repeat(MAX_OPERATOR_DOCUMENT_DEPTH + 1),
                    "]".repeat(MAX_OPERATOR_DOCUMENT_DEPTH + 1)
                );
                assert_eq!(
                    adapter
                        .normalize(&deep, fixture.scope.clone(), &fixture.authority())
                        .unwrap_err(),
                    OperatorProfileAdapterError::InvalidConfiguration
                );
            })
            .expect("spawn small-stack adapter")
            .join()
            .expect("small-stack adapter");
    }
}
