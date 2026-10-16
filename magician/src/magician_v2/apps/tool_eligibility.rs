//! App-eligibility for one shared skill document.
//!
//! Apps and agents use the same `SKILL.md`. This module decides whether an
//! app may lock that document. It does not execute the tool and does not
//! mint a grant.

use serde::Deserialize;
use thiserror::Error;
use tool_runtime_core::{
    action_overrides::{TypedActionOverride, TypedActionParameter, TypedArgumentMapping},
    manifest::RuntimeProtocol,
    manifest_parser::{parse_skill_frontmatter, parse_skill_runtime_package, SkillExposeAudience},
    mcp_catalog_projection::project_mcp_catalog,
};

use super::{
    manifest::AppPackageLimits,
    models::{AppDigest, AppName},
};

/// How the document entered the app lock path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppToolAdmissionSource {
    /// Existing standalone app-tool publication. Omitted `expose` means
    /// `expose.apps: true` because the author is publishing an app tool.
    PublishedAppTool,
    /// Existing reviewed catalog skill. Omitted `expose` is eligible.
    ReviewedCatalog,
    /// Embedded compiled pack YAML. The same packs agents use are
    /// lockable. Grant is the control; attestation is internal.
    CompiledPack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEligibleTool {
    pub name: AppName,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub expose: SkillExposeAudience,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppToolEligibilityError {
    #[error("app tool `{label}` is invalid: {reason}")]
    Invalid { label: String, reason: String },
}

#[derive(Deserialize)]
struct ToolFrontmatter {
    name: String,
    version: String,
    description: String,
    #[serde(default)]
    metadata: ToolMetadata,
}

#[derive(Default, Deserialize)]
struct ToolMetadata {
    #[serde(default)]
    magician: Option<ToolMagicianMetadata>,
}

#[derive(Default, Deserialize)]
struct ToolMagicianMetadata {
    #[serde(default)]
    skill_type: Option<String>,
    #[serde(default)]
    personality: Option<serde::de::IgnoredAny>,
}

const SHELL_BINS: &[&str] = &["sh", "bash", "zsh", "fish", "dash"];
const DEFAULT_COMPILED_PACK_VERSION: &str = "1.0.0";

/// Admit one skill document as an app-lockable typed tool.
pub fn assess_app_tool_eligibility(
    skill_document_bytes: &[u8],
    source: AppToolAdmissionSource,
) -> Result<AppEligibleTool, AppToolEligibilityError> {
    if skill_document_bytes.is_empty() {
        return Err(invalid("app tool", "SKILL.md is empty"));
    }
    if skill_document_bytes.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES {
        return Err(invalid(
            "app tool",
            "SKILL.md exceeds the immutable capability byte ceiling",
        ));
    }
    let source_text = std::str::from_utf8(skill_document_bytes)
        .map_err(|error| invalid("app tool", format!("SKILL.md is not UTF-8: {error}")))?;
    let frontmatter: ToolFrontmatter = parse_skill_frontmatter(source_text)
        .map_err(|error| invalid("app tool", error.to_string()))?;
    let description = frontmatter.description.trim();
    if description.is_empty() || description.len() > AppPackageLimits::default().max_string_bytes()
    {
        return Err(invalid(
            "app tool",
            "description is empty or exceeds the app string ceiling",
        ));
    }
    let metadata = frontmatter.metadata.magician.as_ref();
    if metadata.is_some_and(|value| value.personality.is_some()) {
        return Err(invalid(
            "app tool",
            "personality skills cannot be locked by an app",
        ));
    }
    let skill_type = metadata.and_then(|value| value.skill_type.as_deref());
    match skill_type {
        Some("tool") => {},
        // The reviewed catalog already classifies a document with a valid USR
        // package as a tool. Requiring authors to duplicate that fact in
        // `skill_type` stranded ordinary agent tools from apps even though
        // they had the same typed contract and had passed catalog review.
        None if source == AppToolAdmissionSource::ReviewedCatalog => {},
        Some("procedure") => {
            return Err(invalid(
                "app tool",
                "procedure skills cannot be locked as executable tools",
            ));
        },
        _ => {
            return Err(invalid(
                "app tool",
                "dependency must declare metadata.magician.skill_type: tool",
            ));
        },
    }

    let package = match parse_skill_runtime_package(source_text) {
        Ok(Some(package)) => package,
        Ok(None) => {
            return Err(invalid(
                "app tool",
                "app-eligible tools must declare a Universal Skill Runtime contract",
            ));
        },
        Err(error) => return Err(invalid("app tool", error.to_string())),
    };
    if matches!(package.contract.runtime, RuntimeProtocol::Mcp { .. }) {
        if package.actions.is_some() {
            return Err(invalid(
                "app tool",
                "MCP runtime packages must use SDK discovery, not authored CLI actions",
            ));
        }
        project_mcp_catalog(&package).map_err(|error| invalid("app tool", error))?;
    } else {
        let actions = package.actions.as_ref().ok_or_else(|| {
            invalid(
                "app tool",
                "app-eligible tools must declare typed runtime_actions",
            )
        })?;
        if actions.actions.is_empty() {
            return Err(invalid(
                "app tool",
                "app-eligible tools must declare at least one typed action",
            ));
        }
        if actions
            .actions
            .values()
            .any(action_is_unbounded_raw_argv_passthrough)
        {
            return Err(invalid(
                "app tool",
                "raw argv passthrough actions cannot be locked by an app",
            ));
        }
        if package
            .contract
            .requires
            .bins
            .iter()
            .any(|bin| SHELL_BINS.contains(&bin.as_str()))
        {
            return Err(invalid(
                "app tool",
                "shell binaries cannot be locked by an app",
            ));
        }
    }

    let explicit_expose = tool_runtime_core::manifest_parser::parse_skill_magician_extension::<
        SkillExposeAudience,
    >(source_text, "expose")
    .map_err(|error| invalid("app tool", error.to_string()))?;
    // Deliberately NOT `SkillExposeAudience::default()` (apps: false when
    // omitted — see tool-runtime-core's doc on that type). Every admission
    // source reaching this arm is already a curated/reviewed/compiled set —
    // never an arbitrary agent-only skill picked up unreviewed — so an
    // omitted `expose` block here reads as "no opinion", not "opt out", and
    // resolves to eligible. Confirmed intentional in 79ab6e17b ("Omitted
    // expose.apps is eligible; only an explicit false denies").
    let expose = match (source, explicit_expose) {
        (_, Some(value)) => value,
        (
            AppToolAdmissionSource::PublishedAppTool
            | AppToolAdmissionSource::ReviewedCatalog
            | AppToolAdmissionSource::CompiledPack,
            None,
        ) => SkillExposeAudience {
            agents: true,
            apps: true,
        },
    };
    if !expose.apps {
        return Err(invalid(
            "app tool",
            "app-eligible tools must declare metadata.magician.expose.apps: true",
        ));
    }

    let name = AppName::parse(frontmatter.name).map_err(|error| invalid("app tool", error))?;
    let semantic_version = semver::Version::parse(&frontmatter.version)
        .map_err(|error| invalid("app tool", error.to_string()))?
        .to_string();
    Ok(AppEligibleTool {
        name,
        semantic_version,
        content_digest: AppDigest::blake3(skill_document_bytes),
        expose,
    })
}

/// Admit one embedded compiled pack YAML as an app-lockable typed tool.
pub fn assess_compiled_pack_eligibility(
    pack_yaml: &[u8],
) -> Result<AppEligibleTool, AppToolEligibilityError> {
    if pack_yaml.is_empty() {
        return Err(invalid("compiled pack", "pack YAML is empty"));
    }
    if pack_yaml.len() > tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES {
        return Err(invalid(
            "compiled pack",
            "pack YAML exceeds the immutable capability byte ceiling",
        ));
    }
    let source = std::str::from_utf8(pack_yaml)
        .map_err(|error| invalid("compiled pack", format!("pack YAML is not UTF-8: {error}")))?;
    let pack: crate::magician_v2::execution::CapabilityPackDefinition =
        serde_yaml::from_str(source)
            .map_err(|error| invalid("compiled pack", error.to_string()))?;
    if pack.name.trim().is_empty() {
        return Err(invalid("compiled pack", "pack name is empty"));
    }
    let description = pack.description.as_deref().unwrap_or("").trim();
    if description.is_empty() {
        return Err(invalid("compiled pack", "pack description is empty"));
    }
    let semantic_version = pack
        .version
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_COMPILED_PACK_VERSION);
    let semantic_version = semver::Version::parse(semantic_version)
        .map_err(|error| invalid("compiled pack", error.to_string()))?
        .to_string();
    let name = AppName::parse(pack.name).map_err(|error| invalid("compiled pack", error))?;
    Ok(AppEligibleTool {
        name,
        semantic_version,
        content_digest: AppDigest::blake3(pack_yaml),
        expose: SkillExposeAudience {
            agents: true,
            apps: true,
        },
    })
}

fn action_is_unbounded_raw_argv_passthrough(action: &TypedActionOverride) -> bool {
    // A non-empty `fixed_args` only prepends a fixed prefix; the rest of the
    // argv is still model-controlled once a `StringArray` parameter is
    // mapped through as passthrough, so it must be refused regardless of
    // whether `fixed_args` is empty.
    action.mappings.iter().any(|mapping| {
        matches!(
            mapping,
            TypedArgumentMapping::Passthrough { parameter }
                if action.parameters.get(parameter).is_some_and(|parameter| {
                    matches!(parameter, TypedActionParameter::StringArray { .. })
                })
        )
    })
}

fn invalid(label: impl Into<String>, reason: impl ToString) -> AppToolEligibilityError {
    AppToolEligibilityError::Invalid {
        label: label.into(),
        reason: reason.to_string(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn mcp_app_tool_document(name: &str) -> Vec<u8> {
    format!(
        "---\nname: {name}\nversion: 0.1.0\ndescription: Fixture remote MCP skill for app lock tests.\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      runtime:\n        protocol: mcp\n        transport:\n          kind: streamable_http\n          endpoint: https://mcp.example.test/mcp\n        discovery:\n          namespace: fixture\n          oauth:\n            authorization_issuer: https://auth.example.test\n            scopes: [tools:read]\n          commerce:\n            commodity: INR\n            resource_scope: commerce\n            required_resource_authority: resource_authority\n            amount_parameter: order_amount_inr\n            final_tools: [checkout]\n        limits:\n          timeout_secs: 90\n          stdout_bytes: 65536\n          stderr_bytes: 1024\n      auth:\n        kind: oauth_session\n        requirement: required\n        provider: {name}\n        profile_selection:\n          mode: selectable\n          default: personal\n      policy_floor:\n        approval: ordinary\n        resource_scopes: [external_mcp]\n    runtime_catalog:\n      categories: [commerce, shopping]\n      composition_category: commerce_operations\n---\nCall reviewed remote MCP tools.\n"
    )
    .into_bytes()
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn typed_app_tool_document(name: &str, version: &str, extra_magician: &str) -> Vec<u8> {
    format!(
        "---\nname: {name}\nversion: {version}\ndescription: Rank the next learning step.\nmetadata:\n  magician:\n    skill_type: tool\n{extra_magician}    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {{bins: [next-step]}}\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        rank:\n          description: Rank the next step.\n          fixed_args: [rank]\n---\nReturn one ranked next step.\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_mcp_skill_is_app_eligible_without_cli_actions() {
        let admitted = assess_app_tool_eligibility(
            &mcp_app_tool_document("zepto-mcp"),
            AppToolAdmissionSource::ReviewedCatalog,
        )
        .expect("reviewed MCP skills are app-lockable");
        assert_eq!(admitted.name.as_str(), "zepto-mcp");
        assert!(admitted.expose.apps);
    }

    #[test]
    fn catalog_skill_omitted_expose_is_app_eligible() {
        let admitted = assess_app_tool_eligibility(
            &typed_app_tool_document("next-step", "1.0.0", ""),
            AppToolAdmissionSource::ReviewedCatalog,
        )
        .expect("agent tools are app-lockable without a second expose flag");
        assert_eq!(admitted.name.as_str(), "next-step");
        assert!(admitted.expose.apps);
    }

    #[test]
    fn reviewed_usr_tool_does_not_need_duplicate_skill_type_marker() {
        let bytes = String::from_utf8(typed_app_tool_document("next-step", "1.0.0", ""))
            .unwrap()
            .replace("    skill_type: tool\n", "")
            .into_bytes();
        let admitted = assess_app_tool_eligibility(&bytes, AppToolAdmissionSource::ReviewedCatalog)
            .expect("reviewed typed USR tools are app-lockable");
        assert_eq!(admitted.name.as_str(), "next-step");
    }

    #[test]
    fn standalone_publication_still_requires_explicit_tool_kind() {
        let bytes = String::from_utf8(typed_app_tool_document("next-step", "1.0.0", ""))
            .unwrap()
            .replace("    skill_type: tool\n", "")
            .into_bytes();
        let error = assess_app_tool_eligibility(&bytes, AppToolAdmissionSource::PublishedAppTool)
            .expect_err("unreviewed publication must declare its kind");
        assert!(error.to_string().contains("skill_type: tool"));
    }

    #[test]
    fn published_app_tool_treats_omitted_expose_as_apps_true() {
        let admitted = assess_app_tool_eligibility(
            &typed_app_tool_document("next-step", "1.0.0", ""),
            AppToolAdmissionSource::PublishedAppTool,
        )
        .expect("legacy published app tool");
        assert!(admitted.expose.apps);
    }

    #[test]
    fn published_app_tool_honors_explicit_expose_apps_false() {
        let denied =
            typed_app_tool_document("next-step", "1.0.0", "    expose:\n      apps: false\n");
        let error = assess_app_tool_eligibility(&denied, AppToolAdmissionSource::PublishedAppTool)
            .expect_err("explicit deny wins");
        assert!(error.to_string().contains("expose.apps"));
    }

    #[test]
    fn procedure_personality_and_retired_facade_are_not_app_tools() {
        for (skill_type, extra) in [
            ("procedure", ""),
            ("facade", ""),
            ("tool", "    personality: {}\n"),
        ] {
            let bytes = format!(
                "---\nname: next-step\nversion: 1.0.0\ndescription: Not an app \
                     tool.\nmetadata:\n  magician:\n    skill_type: {skill_type}\n{extra}    \
                     expose:\n      apps: true\n---\nNo.\n"
            )
            .into_bytes();
            assert!(
                assess_app_tool_eligibility(&bytes, AppToolAdmissionSource::ReviewedCatalog)
                    .is_err(),
                "{skill_type} must fail"
            );
        }
    }

    #[test]
    fn missing_usr_or_typed_actions_fail_closed() {
        let no_usr = b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank.\nmetadata:\n  magician:\n    skill_type: tool\n    expose:\n      apps: true\n---\nRank.\n";
        assert!(
            assess_app_tool_eligibility(no_usr, AppToolAdmissionSource::ReviewedCatalog)
                .unwrap_err()
                .to_string()
                .contains("Universal Skill Runtime")
        );

        let no_actions = b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank.\nmetadata:\n  magician:\n    skill_type: tool\n    expose:\n      apps: true\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [next-step]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n---\nRank.\n";
        assert!(
            assess_app_tool_eligibility(no_actions, AppToolAdmissionSource::ReviewedCatalog)
                .unwrap_err()
                .to_string()
                .contains("typed runtime_actions")
        );
    }

    #[test]
    fn raw_argv_passthrough_and_shell_bins_are_refused() {
        let raw_argv = b"---\nname: dugite\nversion: 0.2.0\ndescription: Raw git argv.\nmetadata:\n  magician:\n    skill_type: tool\n    expose:\n      apps: true\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [git]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        run:\n          description: Run any git argv.\n          parameters:\n            args:\n              type: string_array\n              description: Argv after git.\n              required: true\n              min_items: 1\n              max_items: 16\n              max_item_bytes: 64\n          mappings:\n            - type: passthrough\n              parameter: args\n---\nRun git.\n";
        assert!(
            assess_app_tool_eligibility(raw_argv, AppToolAdmissionSource::ReviewedCatalog)
                .unwrap_err()
                .to_string()
                .contains("raw argv")
        );

        let shell =
            typed_app_tool_document("host-shell", "1.0.0", "    expose:\n      apps: true\n")
                .into_iter()
                .collect::<Vec<_>>();
        let shell = String::from_utf8(shell)
            .expect("utf8")
            .replace("bins: [next-step]", "bins: [bash]");
        assert!(assess_app_tool_eligibility(
            shell.as_bytes(),
            AppToolAdmissionSource::ReviewedCatalog
        )
        .unwrap_err()
        .to_string()
        .contains("shell"));
    }

    #[test]
    fn compiled_content_read_and_search_memory_are_app_eligible() {
        for name in [
            "content_read",
            "search_memory",
            "create_task",
            "content_search",
        ] {
            let yaml = crate::magician_v2::execution::embedded_compiled_pack_yaml(name)
                .unwrap_or_else(|| panic!("{name} yaml"));
            let admitted = assess_compiled_pack_eligibility(yaml.as_bytes())
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(admitted.name.as_str(), name);
            assert!(admitted.expose.apps);
        }
    }

    #[test]
    fn compiled_host_primitives_agents_use_are_app_eligible() {
        for name in ["files", "http", "macos_automation"] {
            let yaml = crate::magician_v2::execution::embedded_compiled_pack_yaml(name)
                .unwrap_or_else(|| panic!("{name} yaml"));
            let admitted = assess_compiled_pack_eligibility(yaml.as_bytes())
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(admitted.name.as_str(), name);
            assert!(admitted.expose.apps);
        }
    }
}
