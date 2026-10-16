//! Validates checked-in V3 capability YAML assets.
//!
//! This intentionally covers repository fixtures rather than synthetic YAML:
//! these files are what agent scopes load at runtime.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use magician::magician_v2::execution::flat_loop::build_tool_index;
use magician::{
    config::BotProcessConfig,
    magician_v2::agents::AgentDefinition,
    magician_v2::execution::{
        compiled_providers::load_pack_defs_from_skills_dir, CapabilityPackDefinition,
        CapabilityPackStore, ImplementationType,
    },
};
use serde_yaml::Value;

const CAPABILITY_ROOTS: &[&str] = &[
    "magician_data_v3/system/capability_templates",
    "magician_data_v3/scopes/anonymous/default/capabilities",
];

const PACK_KEYS: &[&str] = &[
    "name",
    "description",
    "version",
    "guide",
    "parameters",
    "native_action_schemas",
    "implementation",
    "execution",
    "auth",
    "status",
    "replaced_by",
    "retire_after",
];

#[test]
fn capability_pack_yamls_parse_and_match_runtime_schema() {
    let mut errors = Vec::new();

    for root in CAPABILITY_ROOTS {
        let packs_dir = repo_path(root).join("packs");
        let mut seen_names = BTreeSet::new();

        for path in yaml_files(&packs_dir) {
            match parse_capability_pack(&path) {
                Ok(pack) => {
                    if !seen_names.insert(pack.name.clone()) {
                        errors.push(format!(
                            "{}: duplicate pack name '{}' within {}",
                            display(&path),
                            pack.name,
                            root
                        ));
                    }

                    if let Err(err) = CapabilityPackStore::validate_definition(&pack) {
                        errors.push(format!("{}: {}", display(&path), err));
                    }
                },
                Err(err) => errors.push(err),
            }
        }
    }

    assert_no_errors("capability pack YAML validation failed", errors);
}

#[test]
fn officecli_skill_schemas_keep_the_shared_runtime_and_restricted_actions() {
    const COMMON_ACTIONS: &[&str] = &[
        "add",
        "batch",
        "create",
        "get",
        "help",
        "load_guidance",
        "merge",
        "move",
        "query",
        "raw",
        "remove",
        "set",
        "validate",
        "view",
    ];
    const FORBIDDEN_ACTIONS: &[&str] = &[
        "add-part", "close", "config", "install", "mcp", "open", "plugins", "raw-set", "watch",
    ];
    let governed_packs = load_pack_defs_from_skills_dir(&repo_path("skillshub"));

    for (skill_name, format_action) in [
        ("office-word", Some("refresh")),
        ("office-excel", None),
        ("office-powerpoint", Some("swap")),
    ] {
        let relative_path = format!("skillshub/{skill_name}/SKILL.md");
        let pack = governed_packs
            .iter()
            .find(|pack| pack.name == skill_name)
            .unwrap_or_else(|| panic!("{relative_path}: governed package was not loaded"));
        CapabilityPackStore::validate_definition(pack)
            .unwrap_or_else(|err| panic!("{relative_path}: {err}"));

        assert_eq!(pack.name, skill_name);
        let guide = pack
            .guide
            .as_deref()
            .unwrap_or_else(|| panic!("{relative_path}: missing flat-loop tool guide"));
        let normalized_guide = guide.to_ascii_lowercase();
        for required_instruction in ["preserve", "validate", "render"] {
            assert!(
                normalized_guide.contains(required_instruction),
                "{relative_path}: runtime guide is missing `{required_instruction}`"
            );
        }
        for action in COMMON_ACTIONS {
            assert!(
                pack.native_action_schemas.contains_key(*action),
                "{relative_path}: missing required action {action}"
            );
        }
        if let Some(action) = format_action {
            assert!(
                pack.native_action_schemas.contains_key(action),
                "{relative_path}: missing format-specific action {action}"
            );
        }
        for action in FORBIDDEN_ACTIONS {
            assert!(
                !pack.native_action_schemas.contains_key(*action),
                "{relative_path}: forbidden OfficeCLI action {action} must not be exposed"
            );
        }
        assert_eq!(
            pack.native_action_schemas
                .get("load_guidance")
                .map(|action| action.argv.as_slice()),
            Some(["load_skill".to_string()].as_slice()),
            "{relative_path}: guidance must use OfficeCLI's read-only load_skill command"
        );

        let ImplementationType::Primitive {
            command,
            runtime_package: Some(runtime),
            timeout_secs,
            ..
        } = &pack.implementation
        else {
            panic!("{relative_path}: OfficeCLI tools must use the primitive dispatcher");
        };
        let expected_command = vec!["officecli".to_owned()];
        assert_eq!(command.as_ref(), Some(&expected_command));
        assert_eq!(*timeout_secs, Some(240));
        for (key, value) in [
            ("OFFICECLI_SKIP_UPDATE", "1"),
            ("OFFICECLI_NO_AUTO_RESIDENT", "1"),
        ] {
            assert_eq!(
                runtime
                    .package
                    .contract
                    .requires
                    .environment
                    .get(key)
                    .map(String::as_str),
                Some(value),
                "{relative_path}: missing subprocess environment guard {key}"
            );
        }
    }
}

#[test]
fn officecli_agent_templates_keep_domain_ownership_and_delegation() {
    for (agent_id, expected_office_tools) in [
        (
            "executive-assistant",
            &["office-excel", "office-powerpoint", "office-word"][..],
        ),
        ("writing-assistant", &["office-word"][..]),
        ("simple-data-analyst", &["office-excel"][..]),
        ("wealth-manager", &["office-excel"][..]),
        ("creative-mind", &["office-powerpoint"][..]),
    ] {
        let relative_path = format!(
            "magician_data_v3/system/agent_templates/agents/{agent_id}/definition.agent.yaml"
        );
        let definition = AgentDefinition::from_yaml_file(repo_path(&relative_path))
            .unwrap_or_else(|err| panic!("{relative_path}: {err}"));
        let actual = definition
            .tools
            .iter()
            .filter(|tool| tool.starts_with("office-"))
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = expected_office_tools
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual, expected,
            "{relative_path}: Office tool ownership drifted"
        );

        if agent_id == "executive-assistant" {
            let delegates = definition
                .delegation_targets
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            for delegate in ["writing-assistant", "simple-data-analyst", "creative-mind"] {
                assert!(
                    delegates.contains(delegate),
                    "{relative_path}: missing Office domain delegate {delegate}"
                );
            }
        }
    }
}

#[test]
fn tabular_tool_selector_ranks_artifact_specific_packs() {
    let selected = [
        "csvkit",
        "sheets",
        "presto-sheets",
        "marimo",
        "metabase",
        "office-excel",
    ];
    let governed_packs = load_pack_defs_from_skills_dir(&repo_path("skillshub"));
    let packs = selected
        .into_iter()
        .map(|skill_name| {
            governed_packs
                .iter()
                .find(|pack| pack.name == skill_name)
                .unwrap_or_else(|| panic!("skillshub/{skill_name}/SKILL.md did not load"))
                .clone()
        })
        .collect::<Vec<_>>();
    let index = build_tool_index(&packs);

    for (query, expected_pack) in [
        (
            "create editable local xlsx workbook formulas charts",
            "office-excel",
        ),
        ("convert existing xlsx sheet to csv", "csvkit"),
        ("publish collaborative cloud google sheet", "sheets"),
        (
            "create cloud google sheet presto own workspace identity",
            "presto-sheets",
        ),
        ("build reproducible python analysis notebook", "marimo"),
        ("query existing metabase dashboard", "metabase"),
    ] {
        let matches = index.search(query, 5);
        let ranked = matches
            .iter()
            .map(|entry| format!("{} ({})", entry.name, entry.pack_name))
            .collect::<Vec<_>>();
        assert_eq!(
            matches.first().map(|entry| entry.pack_name.as_str()),
            Some(expected_pack),
            "query `{query}` ranked the wrong tool pack; candidates: {ranked:?}"
        );
    }
}

#[test]
fn bot_and_personality_yamls_parse() {
    let mut errors = Vec::new();

    for root in CAPABILITY_ROOTS {
        validate_bot_config(&repo_path(root).join("bot_configs.yaml"), &mut errors);
        validate_personality_templates(&repo_path(root).join("personality"), &mut errors);
    }

    assert_no_errors("supporting capability YAML validation failed", errors);
}

#[test]
fn harness_sre_agent_definitions_parse_and_keep_harness_contract() {
    let required_delegates = ["internal-system-analyst", "cto"];

    for relative_path in [
        "magician_data_v3/system/agent_templates/agents/harness-sre/definition.agent.yaml",
        "magician_data_v3/scopes/anonymous/default/agent_runtime/agents/harness-sre/definition.agent.yaml",
    ] {
        let definition = AgentDefinition::from_yaml_file(repo_path(relative_path))
            .unwrap_or_else(|err| panic!("{relative_path}: {err}"));

        assert_eq!(definition.agent_id, "harness-sre");
        assert!(definition.harness.is_some(), "{relative_path}: missing harness marker");
        assert!(
            definition
                .autonomous_config
                .as_ref()
                .and_then(|config| config.focus_areas.first())
                .and_then(|focus| focus.program.as_deref())
                == Some("harness_reliability.md"),
            "{relative_path}: missing harness reliability program focus"
        );
        let delegation_targets = definition
            .delegation_targets
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for delegate in required_delegates {
            assert!(
                delegation_targets.contains(delegate),
                "{relative_path}: missing required delegation target {delegate}"
            );
        }
    }

    for agent_id in required_delegates {
        let relative_path = format!(
            "magician_data_v3/scopes/anonymous/default/agent_runtime/agents/{agent_id}/definition.agent.yaml"
        );
        let definition = AgentDefinition::from_yaml_file(repo_path(&relative_path))
            .unwrap_or_else(|err| panic!("{relative_path}: {err}"));
        assert_eq!(definition.agent_id, agent_id);
        assert_eq!(
            definition.principal.as_deref(),
            Some("anonymous"),
            "{relative_path}: missing scoped principal"
        );
        assert_eq!(
            definition.workspace.as_deref(),
            Some("default"),
            "{relative_path}: missing scoped workspace"
        );
    }

    let program_path =
        repo_path("magician_data_v3/scopes/anonymous/default/programs/harness_reliability.md");
    let program = fs::read_to_string(&program_path)
        .unwrap_or_else(|err| panic!("{}: {err}", display(&program_path)));
    assert!(
        program.contains("# Harness Reliability"),
        "{}: missing harness reliability heading",
        display(&program_path)
    );
}

fn parse_capability_pack(path: &Path) -> Result<CapabilityPackDefinition, String> {
    let content = fs::read_to_string(path)
        .map_err(|err| format!("{}: failed to read: {}", display(path), err))?;

    let value: Value = serde_yaml::from_str(&content)
        .map_err(|err| format!("{}: invalid YAML: {}", display(path), err))?;
    validate_pack_top_level_keys(path, &value)?;

    serde_yaml::from_str::<CapabilityPackDefinition>(&content).map_err(|err| {
        format!(
            "{}: failed to parse capability pack: {}",
            display(path),
            err
        )
    })
}

fn validate_pack_top_level_keys(path: &Path, value: &Value) -> Result<(), String> {
    let Some(mapping) = value.as_mapping() else {
        return Err(format!(
            "{}: pack root must be a YAML mapping",
            display(path)
        ));
    };

    let allowed: BTreeSet<&str> = PACK_KEYS.iter().copied().collect();
    for key in mapping.keys() {
        let Some(key) = key.as_str() else {
            return Err(format!(
                "{}: top-level pack keys must be strings",
                display(path)
            ));
        };
        if !allowed.contains(key) {
            return Err(format!(
                "{}: unknown top-level pack key '{}'; allowed keys are {}",
                display(path),
                key,
                PACK_KEYS.join(", ")
            ));
        }
    }

    Ok(())
}

fn validate_bot_config(path: &Path, errors: &mut Vec<String>) {
    if !path.exists() {
        return;
    }

    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            errors.push(format!("{}: failed to read: {}", display(path), err));
            return;
        },
    };

    let root: Value = match serde_yaml::from_str(&content) {
        Ok(root) => root,
        Err(err) => {
            errors.push(format!("{}: invalid YAML: {}", display(path), err));
            return;
        },
    };

    let Some(bots) = root
        .as_mapping()
        .and_then(|mapping| mapping.get(Value::String("bots".to_string())))
        .and_then(Value::as_mapping)
    else {
        errors.push(format!(
            "{}: expected a top-level 'bots' mapping",
            display(path)
        ));
        return;
    };

    for (name, config) in bots {
        let Some(name) = name.as_str() else {
            errors.push(format!("{}: bot names must be strings", display(path)));
            continue;
        };
        match serde_yaml::from_value::<BotProcessConfig>(config.clone()) {
            Ok(config) if config.command.trim().is_empty() => errors.push(format!(
                "{}: bot '{}' has an empty command",
                display(path),
                name
            )),
            Ok(_) => {},
            Err(err) => errors.push(format!(
                "{}: bot '{}' failed to parse: {}",
                display(path),
                name,
                err
            )),
        }
    }
}

fn validate_personality_templates(dir: &Path, errors: &mut Vec<String>) {
    if !dir.exists() {
        return;
    }

    for path in yaml_files(dir) {
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                errors.push(format!("{}: failed to read: {}", display(&path), err));
                continue;
            },
        };

        match serde_yaml::from_str::<HashMap<String, String>>(&content) {
            Ok(template) if template.is_empty() => {
                errors.push(format!("{}: personality template is empty", display(&path)));
            },
            Ok(_) => {},
            Err(err) => errors.push(format!(
                "{}: failed to parse personality template: {}",
                display(&path),
                err
            )),
        }
    }
}

fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yaml" | "yml")
            )
        {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

fn display(path: &Path) -> String {
    path.strip_prefix(repo_path(""))
        .unwrap_or(path)
        .display()
        .to_string()
}

fn assert_no_errors(context: &str, errors: Vec<String>) {
    if !errors.is_empty() {
        panic!("{context}:\n{}", errors.join("\n"));
    }
}
