//! End-to-end smoke test for the skills loader against the real
//! `skillshub/` source folder at the monorepo root.

use std::path::PathBuf;

use magician::magician_v2::execution::{
    compiled_providers::load_pack_defs_from_skills_dir, ImplementationType,
};
use magician::magician_v2::skills::{
    activate_procedure_skill, build_skill_descriptors, list_personality_mode_names,
    lookup_personality_mode, InferredKind, SkillLoader,
};
use std::env;

fn skillshub_dir() -> PathBuf {
    // tests/ runs with CARGO_MANIFEST_DIR = magician/ ; skillshub/ lives at the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician/ has a parent")
        .join("skillshub")
}

#[test]
fn skillshub_dir_exists_at_repo_root() {
    let p = skillshub_dir();
    assert!(p.is_dir(), "skillshub/ must exist at the repo root: {p:?}");
}

#[test]
fn loader_discovers_at_least_one_bundled_skill() {
    let manifests = SkillLoader::new(vec![skillshub_dir()])
        .discover()
        .expect("discover bundled skills");
    assert!(
        !manifests.is_empty(),
        "skillshub/ should contain at least one SKILL.md folder"
    );
}

#[test]
fn first_bundled_skill_activates_as_procedure_with_body() {
    let manifests = SkillLoader::new(vec![skillshub_dir()])
        .discover()
        .expect("discover");

    // Phase 1 ships exactly one bundled skill (commit-message-style).
    // As more skills land in skillshub/, this test only asserts that AT
    // LEAST ONE procedure-kind skill activates correctly.
    let procedure = manifests
        .iter()
        .find(|m| m.inferred_kind() == InferredKind::Procedure)
        .expect("at least one procedure skill must be bundled");

    let activation = activate_procedure_skill(procedure).expect("activate procedure");
    assert!(
        !activation.steering_message.is_empty(),
        "procedure body must be non-empty after activation"
    );
}

#[test]
fn bundled_personality_modes_lookupable_by_name_and_underscore_alias() {
    // skillshub/ ships eight personality-mode skills (witty + 7 mass-converted).
    // The lookup helper is what `switch_personality` and the seed code path
    // rely on; if it misses, the runtime silently falls back to legacy YAML
    // and the migration leaks through.
    let names = list_personality_mode_names(&[skillshub_dir().as_path()]);
    assert!(
        names.iter().any(|n| n == "witty"),
        "witty personality must be discoverable: got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "true-friend"),
        "true-friend personality must be discoverable: got {names:?}"
    );

    // Direct name lookup.
    let witty = lookup_personality_mode(&[skillshub_dir().as_path()], "witty")
        .expect("witty personality skill must be installed in skillshub/");
    assert_eq!(witty.active_mode, "witty");
    assert!(!witty.voice.is_empty());

    // Underscore alias must resolve to the kebab-named skill so legacy
    // agent definitions (`default_personality: true_friend`) keep working.
    let true_friend = lookup_personality_mode(&[skillshub_dir().as_path()], "true_friend")
        .expect("underscore alias `true_friend` must resolve to `true-friend` skill");
    assert!(!true_friend.voice.is_empty());
}

#[test]
fn activate_procedure_skill_succeeds_on_installed_system_skill() {
    // The activate_skill chat-runtime tool walks workspace + system
    // skill roots, finds the matching manifest, and activates it. This
    // smoke test exercises the same activate_procedure_skill primitive
    // against the bundled `commit-message-style` skill so a regression in
    // the loader-to-activation chain trips here, not in the chat path.
    //
    // Falls back gracefully if `make skills-install` hasn't run by
    // pointing the loader directly at the source folder under
    // skillshub/.
    let manifests = SkillLoader::new(vec![skillshub_dir()])
        .discover()
        .expect("discover bundled skills");
    let cms = manifests
        .iter()
        .find(|m| m.name == "commit-message-style" && m.inferred_kind() == InferredKind::Procedure)
        .expect("commit-message-style skill must ship as a procedure");
    let activation = activate_procedure_skill(cms).expect("activation succeeds");
    assert_eq!(activation.skill_name, "commit-message-style");
    assert!(
        !activation.steering_message.is_empty(),
        "steering body must be non-empty"
    );
    // The bundled skill has no scripts/ or bin/ — verify the empty cases
    // don't crash the activation path.
    assert!(activation.ephemeral_tools.is_empty());
    assert!(activation.path_additions.is_empty());

    // Burn-in for env injection: the runner appends each path_addition
    // to PATH at script-launch time. With no path additions, the
    // baseline PATH stays intact. Sanity-check that the helper at least
    // reads PATH without panicking under our test process env.
    let _baseline_path = env::var("PATH").unwrap_or_default();
}

#[test]
fn end_to_end_install_then_dispatch_via_skill_loader() {
    // E2E proof: install a freshly-authored skill into a temp runtime
    // tree, discover it via SkillLoader, activate it, and run its
    // scripts/run.sh through run_ephemeral. Pins the install + activate
    // + run shell contract for skills that ship a wrapper script.
    use magician::magician_v2::skills::{run_ephemeral, RunContext};
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let skill_root = runtime_root.path().join("system").join("skills");
    let skill_dir = skill_root.join("e2e-tester");
    fs::create_dir_all(skill_dir.join("scripts")).expect("mkdirs");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: e2e-tester\ndescription: e2e dispatch smoke skill — echoes _TOOL_MESSAGE\n\
         metadata:\n  magician:\n    requires:\n      bins: [\"sh\"]\n---\n# E2E\n\
         When invoked, the wrapper script echoes back $_TOOL_MESSAGE so the\n\
         caller can verify env-var passthrough end-to-end.\n",
    )
    .expect("write SKILL.md");
    let run_sh = skill_dir.join("scripts").join("run.sh");
    fs::write(
        &run_sh,
        "#!/bin/sh\nset -eu\nmessage=\"${_TOOL_MESSAGE:?_TOOL_MESSAGE required}\"\necho \"e2e:$message\"\n",
    )
    .expect("write run.sh");
    fs::set_permissions(&run_sh, fs::Permissions::from_mode(0o755)).expect("chmod");

    let manifests = SkillLoader::new(vec![skill_root])
        .discover()
        .expect("loader discovery");
    let manifest = manifests
        .iter()
        .find(|m| m.name == "e2e-tester")
        .expect("freshly installed skill must be discoverable");
    assert_eq!(manifest.inferred_kind(), InferredKind::Procedure);

    let activation = activate_procedure_skill(manifest).expect("activation");
    assert!(activation
        .ephemeral_tools
        .iter()
        .any(|t| t.name == "e2e-tester__run"));
    let run_tool = activation
        .ephemeral_tools
        .iter()
        .find(|t| t.name == "e2e-tester__run")
        .unwrap();

    let mut env = HashMap::new();
    env.insert("_TOOL_MESSAGE".to_string(), "hello-from-test".to_string());
    let ctx = RunContext {
        scope_id: "e2e/test".to_string(),
        workspace: "test".to_string(),
        vault_token: String::new(),
        ledger_token: String::new(),
        active_skill_bins: activation.path_additions.clone(),
        extra_env: env,
        scope_paths: None,
    };
    let out = run_ephemeral(run_tool, &[], &ctx).expect("script runs");
    assert_eq!(out.exit, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "e2e:hello-from-test");
}

fn assert_drop_in_governed_cli_skill(skill_name: &str, executable: &str) {
    let skill_dir = skillshub_dir().join(skill_name);
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(
        !skill_dir.join("tool_schema.yaml").exists(),
        "{skill_name} must not retain a second catalog source"
    );
    assert!(
        !skill_dir.join("scripts/run.sh").exists(),
        "{skill_name} must not require a handwritten wrapper"
    );

    let pack = load_pack_defs_from_skills_dir(&skillshub_dir())
        .into_iter()
        .find(|pack| pack.name == skill_name)
        .unwrap_or_else(|| panic!("{skill_name} governed pack must load from SKILL.md"));
    let ImplementationType::Primitive {
        runtime_package: Some(runtime),
        command,
        ..
    } = &pack.implementation
    else {
        panic!("{skill_name} must project to the universal primitive runtime");
    };
    let expected_command = vec![executable.to_owned()];
    assert_eq!(command.as_ref(), Some(&expected_command));
    assert!(runtime.package.contract.requires.bins.contains(executable));
    let catalog = runtime.cli_actions().expect("governed CLI action catalog");
    assert_eq!(catalog.execution.executable, executable);
    assert!(catalog.actions.contains_key("run"));
}

#[test]
fn awk_skill_is_drop_in_governed_runtime_without_wrapper() {
    assert_drop_in_governed_cli_skill("awk", "awk");
}

#[test]
fn jq_skill_is_drop_in_governed_runtime_without_wrapper() {
    assert_drop_in_governed_cli_skill("jq", "jq");
}

#[test]
fn sed_skill_is_drop_in_governed_runtime_without_wrapper() {
    assert_drop_in_governed_cli_skill("sed", "sed");
}

#[test]
fn browser_skill_activates_as_documentation_only() {
    // The browser skill ships with NO scripts/run.sh — it's a
    // documentation-only procedure skill. Magician's hand-authored
    // browser action provider (Rust, in the inner-loop dispatcher) is
    // the actual execution path; the SKILL.md exists so that a) the
    // catalog surfaces a description for the LLM and b) future
    // dispatch-swap work has a place to land scripts/ when the
    // browser CLI relocates from the npm install to skillshub/browser/bin/
    // ([P2-1]).
    //
    // This smoke pins the documentation-only contract: discoverable
    // as a procedure skill with a non-empty body, zero ephemeral
    // tools, and `requires.bins: ["agent-browser"]` declared on the
    // frontmatter so deployments without the binary fail loudly at
    // activation rather than silently on first use.
    let manifests = SkillLoader::new(vec![skillshub_dir()])
        .discover()
        .expect("discover");
    let browser = manifests
        .iter()
        .find(|m| m.name == "browser")
        .expect("browser skill must be in skillshub/");
    assert_eq!(browser.inferred_kind(), InferredKind::Procedure);
    let requires = browser
        .metadata
        .magician
        .as_ref()
        .map(|m| m.requires.bins.as_slice())
        .unwrap_or(&[]);
    assert!(
        requires.iter().any(|b| b == "agent-browser"),
        "browser skill must declare requires.bins: [\"agent-browser\"]; got {requires:?}"
    );

    // Activation skipped: agent-browser may not be on the test host's
    // PATH (CI doesn't run `make setup-agent-browser`), and
    // `activate_procedure_skill` would fail check_required_bins. The
    // declarative shape is what we're pinning — the dispatch path
    // proper is exercised by the inner-loop dispatcher, not the
    // skill activation runner.
    //
    // Confirm body is non-empty so the LLM gets steering content
    // when this skill is allowlisted.
    let body = browser.body().expect("browser body readable");
    assert!(
        body.contains("agent-browser"),
        "browser skill body must reference the agent-browser CLI"
    );
}

#[test]
fn descriptors_only_list_procedure_skills() {
    let manifests = SkillLoader::new(vec![skillshub_dir()])
        .discover()
        .expect("discover");
    let descriptors = build_skill_descriptors(&manifests);

    // Every descriptor's name corresponds to a procedure-kind manifest.
    for d in &descriptors {
        let m = manifests
            .iter()
            .find(|m| m.name == d.name)
            .expect("descriptor's manifest must exist in the loaded set");
        assert_eq!(
            m.inferred_kind(),
            InferredKind::Procedure,
            "descriptor for '{}' but its manifest is kind {:?}",
            d.name,
            m.inferred_kind()
        );
    }
}
