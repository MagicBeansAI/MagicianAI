//! Magician-owned integration coverage for the built-in governed skill inventory.
//! Keep this suite in the product workspace: MagicRun must not depend on Magician's
//! source tree, inventory snapshots, or installed skill adapters.

use std::{collections::BTreeSet, fs, path::Path};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
}

#[test]
fn migrated_inventory_is_one_validated_drop_in_package_per_skill() {
    let root = workspace_root();
    let inventory: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("data/tool-runtime-inventory/phase0-classification-v1.json"))
            .expect("phase 0 classification"),
    )
    .expect("valid phase 0 classification JSON");
    let skills = inventory["skills"].as_array().expect("classified skills");
    // Ratchet: bump deliberately when a skill genuinely joins or leaves the
    // governed set, never to quiet a failure. Last moved 2026-09-02 for
    // web-via-tinyfish + work-modules (added by 02e00e14da).
    assert_eq!(
        skills.len(),
        62,
        "the governed built-in set must remain exact"
    );

    let mut unique = BTreeSet::new();
    for skill in skills {
        let skill_id = skill["id"].as_str().expect("classified skill id");
        assert!(
            unique.insert(skill_id),
            "duplicate inventory skill: {skill_id}"
        );
        let active = root.join("skillshub").join(skill_id);
        assert!(active.join("SKILL.md").is_file(), "{skill_id}");
        assert!(
            !active.join("tool_schema.yaml").exists(),
            "{skill_id} retains a second active model schema"
        );
        let source = fs::read_to_string(active.join("SKILL.md")).expect("active SKILL.md");
        let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .unwrap_or_else(|error| panic!("parse {skill_id}: {error}"))
            .unwrap_or_else(|| panic!("{skill_id} has no governed runtime package"));
        tool_runtime_core::manifest_validation::validate_skill_runtime_contract(&package.contract)
            .unwrap_or_else(|error| panic!("validate {skill_id}: {error}"));

        if let Ok(entries) = fs::read_dir(active.join("bin")) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                assert!(
                    package.contract.requires.bins.contains(name),
                    "{skill_id}/bin/{name} is not declared by its package"
                );
            }
        }
    }
}

#[test]
fn remote_mcp_packages_use_only_the_generic_sdk_executor() {
    use tool_runtime_core::manifest::{AuthKind, McpTransport, RuntimeProtocol};

    let root = workspace_root();
    for skill_id in ["swiggy-mcp", "zepto-mcp"] {
        let active = root.join("skillshub").join(skill_id);
        let source = fs::read_to_string(active.join("SKILL.md")).expect("MCP SKILL.md");
        let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .expect("parse MCP package")
            .expect("MCP package");
        assert_eq!(package.contract.auth.kind, AuthKind::OAuthSession);
        assert!(package.actions.is_none(), "MCP discovery owns live actions");
        assert!(matches!(
            &package.contract.runtime,
            RuntimeProtocol::Mcp {
                transport: McpTransport::StreamableHttp { .. },
                ..
            }
        ));
        assert!(!active.join("scripts").exists());
        assert!(!active.join("tool_schema.yaml").exists());
    }

    let executor = fs::read_to_string(
        root.join("magician/src/magician_v2/execution/primitive_dispatch/governed_mcp.rs"),
    )
    .expect("generic governed MCP executor")
    .to_ascii_lowercase();
    for provider_name in ["swiggy", "zepto"] {
        assert!(
            !executor.contains(provider_name),
            "generic MCP production code names provider {provider_name}"
        );
    }
}

#[test]
fn active_adapter_inventory_names_only_existing_or_materializable_owned_sources() {
    use tool_runtime_core::classification::ClassificationManifest;

    let root = workspace_root();
    let source =
        fs::read_to_string(root.join("data/tool-runtime-inventory/phase0-classification-v1.yaml"))
            .expect("classification manifest");
    let manifest = ClassificationManifest::from_yaml(&source).expect("valid classification");
    let files = manifest
        .adapter_groups
        .iter()
        .flat_map(|group| group.files.iter())
        .collect::<Vec<_>>();
    // Ratchet — see the note on the governed built-in count above. Last moved
    // 2026-09-02 for work-modules/bin/work-modules.
    assert_eq!(
        files.len(),
        40,
        "active adapter inventory must remain exact"
    );
    // Adapters that a governed `make setup-*` target materializes rather than
    // git carrying them. Each one's presence is verified by its own target
    // (`setup-metabase-cli`, and `verify-agent-browser`, which `make test-rust`
    // runs), so asserting it again here would only make this test fail on a
    // machine that has not run setup yet. Everything else must be committed
    // source: an adapter that is neither is unreachable on any other machine.
    let generated_sources = BTreeSet::from([
        ("metabase", "bin/metabase-pp-cli"),
        ("browser", "bin/agent-browser"),
    ]);
    for file in files {
        let active = root.join("skillshub").join(&file.skill).join(&file.path);
        if active.is_file() {
            continue;
        }
        assert!(
            generated_sources.contains(&(file.skill.as_str(), file.path.as_str())),
            "classified source is neither active nor a governed generated adapter: {}/{}",
            file.skill,
            file.path
        );
    }
    assert!(
        root.join("skillshub/metabase/scripts/spec.json").is_file(),
        "the generated Metabase adapter retains its committed source spec"
    );
    let skillshub_makefile =
        fs::read_to_string(root.join("skillshub/Makefile")).expect("skillshub Makefile");
    assert!(
        skillshub_makefile.contains("setup-metabase-cli: $(METABASE_PP_BIN)"),
        "the generated Metabase adapter retains its governed materialization target"
    );
}

#[test]
fn retired_skill_tree_and_one_off_migration_generators_are_absent() {
    let root = workspace_root();
    assert!(
        !root.join("deprecated/tool-runtime-phase7").exists(),
        "retired Phase 7 skill implementations must not return to the repository"
    );
    for script in [
        "scripts/migrate_phase7_adapter_schema.py",
        "scripts/migrate_phase7_browser_schema.py",
        "scripts/migrate_phase7_cli_schema.py",
        "skillshub/scripts/generate_pack_wrappers.py",
        "skillshub/scripts/migrate_pack_to_tool_schema.py",
    ] {
        assert!(
            !root.join(script).exists(),
            "temporary migration code: {script}"
        );
    }
}

/// One credentialed static-secret adapter and its reviewed governed budget.
struct CredentialedAdapter {
    skill_id: &'static str,
    executable: &'static str,
    /// Exact secret references the package may resolve, and nothing else.
    secret_refs: &'static [&'static str],
    /// Sum of the adapter's own provider deadlines on its longest single
    /// action. Mirrored in the adapter as `INNER_WORST_CASE_SECS`.
    inner_worst_case_secs: u32,
    /// The runtime's wall-clock kill, declared as `runtime.limits.timeout_secs`.
    /// It must be the outer bound of the two.
    governed_kill_secs: u32,
}

/// The reviewed timeout budget for all seven credentialed adapters, as a table.
///
/// margin = governed_kill_secs - inner_worst_case_secs, and must stay positive:
///
///   tavily   30 -> 35   exa      60 -> 65   openai  60 -> 65
///   claude  300 -> 305  klipy    15 -> 20   imgflip 25 -> 30
///   reddit   54 -> 60
const CREDENTIALED_ADAPTERS: &[CredentialedAdapter] = &[
    CredentialedAdapter {
        skill_id: "news-search-via-tavily",
        executable: "tavily-search",
        secret_refs: &["TAVILY_API_KEY"],
        inner_worst_case_secs: 30,
        governed_kill_secs: 35,
    },
    CredentialedAdapter {
        skill_id: "semantic-websearch-via-exa",
        executable: "exa-search",
        secret_refs: &["EXA_API_KEY"],
        inner_worst_case_secs: 60,
        governed_kill_secs: 65,
    },
    CredentialedAdapter {
        skill_id: "websearch-via-openai",
        executable: "openai-websearch",
        secret_refs: &["OPENAI_API_KEY"],
        inner_worst_case_secs: 60,
        governed_kill_secs: 65,
    },
    CredentialedAdapter {
        skill_id: "websearch-via-claude",
        executable: "claude-websearch",
        secret_refs: &["ANTHROPIC_API_KEY"],
        // One Messages call plus up to four pause-turn continuations, each with
        // a 60s provider deadline.
        inner_worst_case_secs: 300,
        governed_kill_secs: 305,
    },
    CredentialedAdapter {
        skill_id: "gif-search-via-klipy",
        executable: "klipy-gif-search",
        secret_refs: &["KLIPY_API_KEY"],
        inner_worst_case_secs: 15,
        governed_kill_secs: 20,
    },
    CredentialedAdapter {
        skill_id: "meme-generation-via-imgflip",
        executable: "imgflip-meme",
        secret_refs: &["IMGFLIP_PASSWORD", "IMGFLIP_USERNAME"],
        // A caption resolved by template name runs the catalog request (10s)
        // and the caption request (15s) in sequence.
        inner_worst_case_secs: 25,
        governed_kill_secs: 30,
    },
    CredentialedAdapter {
        skill_id: "reddit-search",
        executable: "reddit-search",
        secret_refs: &["REDDIT_CLIENT_ID", "REDDIT_CLIENT_SECRET"],
        // Two application-only OAuth attempts followed by one non-retrying
        // keyless Atom fallback: 2 * (10s + 10s) + 4s + 10s.
        inner_worst_case_secs: 54,
        governed_kill_secs: 60,
    },
];

fn credentialed_package(
    root: &Path,
    skill_id: &str,
) -> tool_runtime_core::manifest_parser::SkillRuntimePackage {
    let source = fs::read_to_string(root.join("skillshub").join(skill_id).join("SKILL.md"))
        .unwrap_or_else(|error| panic!("read {skill_id} SKILL.md: {error}"));
    tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
        .unwrap_or_else(|error| panic!("parse {skill_id}: {error}"))
        .unwrap_or_else(|| panic!("{skill_id} has no governed runtime package"))
}

/// Read one `NAME = <integer>` module constant out of a Python adapter.
///
/// The adapters must not read their own YAML manifest at execution time, so the
/// governed ceiling is mirrored as a plain literal on the Python side. This is
/// how the Rust guard reaches that mirror.
fn declared_adapter_secs(source: &str, name: &str) -> u32 {
    let needle = format!("\n{name} = ");
    assert_eq!(
        source.matches(needle.as_str()).count(),
        1,
        "{name} must be declared exactly once as a plain module constant"
    );
    let start = source.find(needle.as_str()).expect("declared constant") + needle.len();
    let digits = source[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a plain integer literal"))
}

/// Whether the retired `scripts/` directory still holds anything.
///
/// The invariant is that no legacy script *file* survives the migration, which
/// is not the same as the directory inode being gone. Git cannot represent an
/// empty directory, so an empty one can never arrive from a clone — it is left
/// behind in a working tree where the migration deleted the files in place, and
/// it is invisible to `git status` for the same reason. Failing on it reports a
/// migration that is in fact complete, and because this runs late in a
/// ~16-minute suite, the cost of that false report is the whole run.
fn legacy_scripts_are_gone(skill_dir: &Path) -> bool {
    let scripts = skill_dir.join("scripts");
    match fs::read_dir(&scripts) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        // Anything else — a permission error, a file where the directory should
        // be — is a broken checkout, not a passing migration.
        Err(error) => panic!("read {}: {error}", scripts.display()),
        Ok(mut entries) => entries.next().is_none(),
    }
}

#[test]
fn credentialed_adapters_keep_their_exact_secret_bindings_and_single_run_action() {
    use tool_runtime_core::manifest::AuthKind;

    let root = workspace_root();
    for entry in CREDENTIALED_ADAPTERS {
        let skill_id = entry.skill_id;
        let secret_refs = entry.secret_refs;
        let active = root.join("skillshub").join(skill_id);
        // Each message names the specific breakage. Three asserts sharing the
        // bare skill id means a failure prints one word and you cannot tell
        // which of them fired.
        assert!(
            active.join("bin").join(entry.executable).is_file(),
            "{skill_id}: missing migrated entrypoint bin/{}",
            entry.executable
        );
        assert!(
            !active.join("tool_schema.yaml").exists(),
            "{skill_id}: retired tool_schema.yaml is still present"
        );
        assert!(
            legacy_scripts_are_gone(&active),
            "{skill_id}: legacy scripts/ still holds files"
        );

        let package = credentialed_package(root, skill_id);
        assert_eq!(package.contract.auth.kind, AuthKind::Secrets, "{skill_id}");
        assert_eq!(
            package
                .contract
                .auth
                .secret_bindings
                .iter()
                .map(|binding| binding.secret_ref.as_str())
                .collect::<BTreeSet<_>>(),
            secret_refs.iter().copied().collect::<BTreeSet<_>>(),
            "{skill_id} secret references drifted"
        );
        assert_eq!(
            package.contract.auth.secret_bindings.len(),
            secret_refs.len(),
            "{skill_id} gained or lost a secret binding"
        );
        assert_eq!(
            package
                .actions
                .as_ref()
                .unwrap_or_else(|| panic!("{skill_id} typed actions"))
                .actions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["run"],
            "{skill_id} public action surface drifted"
        );
        assert!(
            package
                .contract
                .auth
                .injections
                .iter()
                .all(|injection| matches!(
                    injection.target,
                    tool_runtime_core::manifest::InjectionTarget::Environment { .. }
                )),
            "{skill_id} must deliver every credential through the child environment"
        );
        assert_eq!(
            package.contract.auth.injections.len(),
            secret_refs.len(),
            "{skill_id} injection count must match its secret bindings"
        );
    }
}

/// The governed kill is a backstop for a runtime that has stopped responding. A
/// backstop that fires before the work it backs stops being a backstop and
/// becomes the primary timeout under the wrong name — and every timeout and
/// retry path the adapter wrote for a slow provider turns into dead code,
/// because the blunt kill always gets there first.
///
/// The two numbers live in files that cannot reference each other: the ceiling
/// is YAML in `SKILL.md`, and the provider deadlines are Python literals in the
/// adapter, which must not read its own manifest at execution time. So the
/// adapter mirrors the ceiling as `GOVERNED_KILL_CEILING_SECS`, both sides
/// carry the arithmetic in a comment, and this test is what actually holds them
/// together.
#[test]
fn credentialed_adapter_governed_kill_outlasts_its_inner_provider_budget() {
    use tool_runtime_core::manifest::RuntimeProtocol;

    let root = workspace_root();
    for entry in CREDENTIALED_ADAPTERS {
        let skill_id = entry.skill_id;
        let executable = entry.executable;
        let package = credentialed_package(root, skill_id);
        let RuntimeProtocol::Cli { limits, .. } = &package.contract.runtime else {
            panic!("{skill_id} must remain a governed CLI package");
        };
        let declared_kill = limits
            .timeout_secs
            .unwrap_or_else(|| panic!("{skill_id} must declare an explicit governed kill"));
        assert_eq!(
            declared_kill, entry.governed_kill_secs,
            "{skill_id} governed kill drifted from the reviewed budget"
        );

        let source = fs::read_to_string(
            root.join("skillshub")
                .join(skill_id)
                .join("bin")
                .join(executable),
        )
        .unwrap_or_else(|error| panic!("read {skill_id} adapter: {error}"));
        let inner = declared_adapter_secs(&source, "INNER_WORST_CASE_SECS");
        let margin = declared_adapter_secs(&source, "GOVERNED_KILL_MARGIN_SECS");
        let mirrored_kill = declared_adapter_secs(&source, "GOVERNED_KILL_CEILING_SECS");

        assert_eq!(
            inner, entry.inner_worst_case_secs,
            "{skill_id} inner provider budget changed; re-derive the governed kill"
        );
        assert_eq!(
            mirrored_kill, declared_kill,
            "{skill_id}/bin/{executable} no longer mirrors runtime.limits.timeout_secs"
        );
        assert!(
            margin > 0,
            "{skill_id} must state a positive margin, not a tie"
        );
        assert!(
            declared_kill >= inner + margin,
            "{skill_id} governed kill {declared_kill}s does not outlast its \
             {inner}s inner provider budget plus a {margin}s margin"
        );
    }
}

#[test]
fn migrated_network_adapters_keep_explicit_response_budgets() {
    let root = workspace_root();
    for relative in [
        "skillshub/hackernews-search/bin/hackernews-search",
        "skillshub/polymarket-search/bin/polymarket-search",
        "skillshub/producthunt-search/bin/producthunt-search",
        "skillshub/reddit-search/bin/reddit-search",
        "skillshub/macos-ui-automation/bin/macos-ui-controller",
        // The credentialed adapters were absent from this guard: every skill it
        // covered was anonymous, so the six that carry a provider secret were
        // the only network adapters whose response budget nothing pinned.
        "skillshub/news-search-via-tavily/bin/tavily-search",
        "skillshub/semantic-websearch-via-exa/bin/exa-search",
        "skillshub/websearch-via-openai/bin/openai-websearch",
        "skillshub/websearch-via-claude/bin/claude-websearch",
        "skillshub/gif-search-via-klipy/bin/klipy-gif-search",
        "skillshub/meme-generation-via-imgflip/bin/imgflip-meme",
    ] {
        let source = fs::read_to_string(root.join(relative)).expect("network adapter source");
        assert!(
            source.contains("MAX_RESPONSE_BYTES")
                || source.contains("MAX_HOST_GATEWAY_RESPONSE_BYTES"),
            "{relative} lost its explicit response budget"
        );
        assert!(
            !source.contains("response.read()") && !source.contains("resp.read()"),
            "{relative} contains an unbounded network body read"
        );
    }
}

#[test]
fn generated_mcp_evidence_uses_the_shared_official_sdk_projection() {
    let root = workspace_root();
    let inventory: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            root.join("data/tool-runtime-inventory/phase0-source-inventory-v1.json"),
        )
        .expect("source inventory"),
    )
    .expect("valid source inventory JSON");
    let classification: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("data/tool-runtime-inventory/phase0-classification-v1.json"))
            .expect("source classification"),
    )
    .expect("valid source classification JSON");
    let replay: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            root.join("data/tool-runtime-inventory/phase0-replay-fixtures-v1.json"),
        )
        .expect("replay fixtures"),
    )
    .expect("valid replay fixture JSON");
    let expected_actions = BTreeSet::from([
        "auth_start",
        "call_tool",
        "clear_auth",
        "list_tools",
        "status",
    ]);

    for skill_id in ["swiggy-mcp", "zepto-mcp"] {
        let inventoried = inventory["skills"]
            .as_array()
            .expect("inventoried skills")
            .iter()
            .find(|skill| skill["id"] == skill_id)
            .expect("inventoried MCP skill");
        assert_eq!(inventoried["implementation"]["kind"], "mcp");
        assert_eq!(inventoried["implementation"]["program"], "official_mcp_sdk");
        assert_eq!(
            inventoried["action_names"]
                .as_array()
                .expect("MCP action names")
                .iter()
                .map(|name| name.as_str().expect("MCP action name"))
                .collect::<BTreeSet<_>>(),
            expected_actions
        );

        let classified = classification["skills"]
            .as_array()
            .expect("classified skills")
            .iter()
            .find(|skill| skill["id"] == skill_id)
            .expect("classified MCP skill");
        assert_eq!(classified["runtime_owner"], "official_mcp_sdk");

        let replay_actions = replay["fixtures"]
            .as_array()
            .expect("replay fixtures")
            .iter()
            .filter(|fixture| fixture["skill_id"] == skill_id)
            .map(|fixture| {
                assert_eq!(fixture["dispatch"]["kind"], "official_mcp_sdk");
                fixture["action_name"]
                    .as_str()
                    .expect("replay MCP action name")
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(replay_actions, expected_actions);
    }
    assert_eq!(replay["summary"]["official_mcp_sdk_fixtures"], 10);
}
