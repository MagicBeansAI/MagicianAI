//! Tier 2 live canaries: the smallest real call that proves each tool skill
//! still works end to end against its actual provider.
//!
//! # Why this file lives in `magician` and not in `tool-runtime-core`
//!
//! Do not "tidy" this into the lower crate. The property that makes this
//! harness worth having depends on where it sits, and moving it silently
//! destroys that property while leaving every test green.
//!
//! `tool-runtime-core` owns `GovernedExecutionCoordinator`, but the coordinator
//! is only a settlement boundary: it accepts an already-assembled invocation,
//! including `baseline_values: ChildEnvironmentValues` — the child environment
//! — as an input it never builds. Everything that *assembles* a governed call
//! lives here in `magician`, in
//! `src/magician_v2/execution/primitive_dispatch/governed_runtime.rs`:
//! `baseline_values()`, `resolve_ca_bundle()`, `governed_executable_directories()`,
//! the authorizer, and the audit sink. None of it is reachable from
//! `tool-runtime-core`, which is the lower layer and does not depend on
//! `magician`.
//!
//! So a runner placed in `tool-runtime-core` could only *re-implement* that
//! assembly. That is precisely the defect this harness exists to prevent. The
//! rewrite that broke twenty-one of sixty-four skills went unnoticed because
//! the eval that should have caught it built its own invocation with
//! `os.environ.copy()`: it tested an execution model production no longer had
//! and would have passed while production failed. A re-implementing runner
//! would repeat that exactly — and worse, it would be structurally blind to the
//! specific bug, because `SSL_CERT_FILE` reaches adapters only through
//! `baseline_values()`. A canary that cannot see the CA-bundle binding cannot
//! report on the failure that motivated it.
//!
//! Therefore this file drives the real production entry point and builds
//! nothing itself: `ScopedDeterministicCapabilityInvoker::invoke()`, the same
//! call a live agent makes, which routes through `admit_governed_route()` and
//! `dispatch_governed_runtime()` into `baseline_values()` and the coordinator.
//! There is no `Command` and no environment assembled anywhere below.
//!
//! # What it asserts
//!
//! Per skill, from that skill's own manifest — never a shape written here:
//!
//! * the governed call succeeded;
//! * the result satisfies the declared `min_items` against the manifest's own
//!   items pointer;
//! * the adapter's error envelope is absent;
//! * latency and reported cost are inside the declared bounds.
//!
//! The error-envelope check is the one that would have caught the original
//! failure. Under it, an adapter that returns zero items while reporting a
//! provider-unavailable or provider-misconfigured condition FAILS rather than
//! degrading to a partial success nobody reads.
//!
//! # Spend and safety
//!
//! Cost tiers gate real money: the default run covers `free` and `cheap`;
//! `expensive` requires `TOOL_CANARY_TIER=expensive`. Skills whose every action
//! sends a message, places an order, or mutates remote state carry a declared
//! exemption instead of a call, and are reported as exempt rather than passed.
//!
//! A skill whose credential is absent is reported SKIPPED, never PASSED. That
//! distinction is the whole point: a silent pass on a missing key is the same
//! false green the harness exists to eliminate.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use magician::magician_v2::{
    artifact_v2::capabilities::CapabilityScopePaths,
    artifact_v2::workspace::{runtime_root_env, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE},
    execution::{
        load_pack_defs_from_skills_dir,
        primitive_dispatch::{
            DeterministicCapabilityInvocation, DeterministicCapabilityInvocationSource,
            DeterministicCapabilityInvoker, PrimitiveExecCtx, ScopedDeterministicCapabilityInvoker,
        },
        CapabilityRegistry,
    },
    secrets::{in_memory_secret_runtime_bootstrap, SecretStore, SecretStoreResolver},
};

/// The installed scope this lane probes. A canary must run where the operator's
/// skills are actually installed, or it reports on the package source instead
/// of the thing production uses.
const CANARY_PRINCIPAL: &str = DEFAULT_SCOPE_PRINCIPAL;
const CANARY_WORKSPACE: &str = DEFAULT_SCOPE_WORKSPACE;
use tool_runtime_core::{
    canary::{CanaryCostTier, CanaryRun, SkillRuntimeCanary},
    governed_batch_process::process_memory_limits_are_enforceable,
    manifest::{
        AuthKind, AuthRequirement, AuthStorage, ProfileSelection, RuntimeProtocol,
        WorkingDirectoryMode,
    },
    manifest_parser::{parse_skill_runtime_package, SkillRuntimePackage},
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician/ has a parent")
        .to_path_buf()
}

/// One discovered governed tool skill, with its canary declaration.
struct CanarySkill {
    name: String,
    package: SkillRuntimePackage,
    canary: SkillRuntimeCanary,
    /// `content_source.output` pointers, when the package declares them. The
    /// runner prefers these over anything restated in the canary so the probe
    /// asserts against the shape the product itself consumes.
    items_pointer: Option<String>,
    error_pointer: Option<String>,
    cost_pointer: Option<String>,
    /// The package's own declared cost encoding. Read rather than assumed:
    /// packages differ (`decimal_major_units` vs `microunits`), and hardcoding
    /// one conversion would inflate the other by a factor of a million and
    /// invent a spend failure that never happened.
    cost_encoding: Option<String>,
    /// The package's own declared cost commodity. Reading the encoding fixed
    /// half of this: a number can be converted only once it is known what it
    /// counts. exa prices in `usd`, tavily in `tavily_credit`, and the
    /// product's retrieval budgets are keyed by commodity precisely because
    /// the two never add up. The canary's ceiling must name the same commodity
    /// or the comparison is between unlike things.
    cost_commodity: Option<String>,
}

fn frontmatter_source(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(&rest[..end])
}

fn pointer_at(value: &serde_yaml::Value, path: &[&str]) -> Option<String> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key)?;
    }
    cursor.as_str().map(str::to_owned)
}

/// Discover every skill declaring `runtime_contract`, exactly as the Tier 1
/// contract suite does, and pair it with its canary.
fn canary_skills() -> Vec<CanarySkill> {
    let root = workspace_root().join("skillshub");
    let mut names = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    names.sort();

    let mut skills = Vec::new();
    for name in names {
        let manifest = root.join(&name).join("SKILL.md");
        let Ok(source) = fs::read_to_string(&manifest) else {
            continue;
        };
        let Some(raw) = frontmatter_source(&source) else {
            continue;
        };
        let Ok(frontmatter) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
            continue;
        };
        // Tier 1 already asserts every one of these parses and declares a
        // canary, so anything unreadable here is that suite's failure to
        // report, not this one's to paper over.
        let Ok(Some(package)) = parse_skill_runtime_package(&source) else {
            continue;
        };
        let Ok(Some(canary)) = SkillRuntimeCanary::parse(&frontmatter) else {
            continue;
        };
        let output = ["metadata", "magician", "content_source", "output"];
        let at = |leaf: &str| {
            let mut path = output.to_vec();
            path.push(leaf);
            pointer_at(&frontmatter, &path)
        };
        skills.push(CanarySkill {
            name,
            package,
            canary,
            items_pointer: at("items_pointer"),
            error_pointer: at("error_pointer"),
            cost_pointer: pointer_at(
                &frontmatter,
                &[
                    "metadata",
                    "magician",
                    "content_source",
                    "output",
                    "cost",
                    "pointer",
                ],
            ),
            cost_encoding: pointer_at(
                &frontmatter,
                &[
                    "metadata",
                    "magician",
                    "content_source",
                    "output",
                    "cost",
                    "encoding",
                ],
            ),
            cost_commodity: pointer_at(
                &frontmatter,
                &[
                    "metadata",
                    "magician",
                    "content_source",
                    "output",
                    "cost",
                    "commodity",
                ],
            ),
        });
    }
    assert!(!skills.is_empty(), "skillshub/ must contain tool skills");
    skills
}

/// Which credential names this skill needs, and whether the host has them.
///
/// Name presence only — no value is ever read, compared, or logged. A skill
/// whose credential is absent is SKIPPED; reporting it as a pass would be the
/// same false green that let a third of the tool surface go dark.
fn missing_credentials(skill: &CanarySkill, workspace: &Path) -> Vec<String> {
    let auth = &skill.package.contract.auth;
    // `Optional` means the skill works unauthenticated — a missing key there is
    // a lower rate limit, not an untested skill, so it must still run.
    //
    // `Conditional` is the contract saying authentication depends on the CALL,
    // not on the skill. Nothing in the schema says which calls need it: auth is
    // declared once per contract and `tool-runtime.typed-action-overrides.v1`
    // carries no per-action auth field, so neither the contract nor the canary
    // can be read to mean "this action needs the profile". A harness that
    // synthesizes a mandatory credential from `conditional` therefore invents a
    // requirement the package never declared, and invents it uniformly — over
    // exactly the credential-free paths a canary author picks on purpose so the
    // probe is free, offline, and deterministic. That is what kept `ocr`, whose
    // canary pins the tesseract engine, permanently SKIPPED.
    //
    // So a conditional requirement runs, and the real call answers. The failure
    // mode this admits is a false RED — a genuinely credential-needing action
    // fails loudly with the provider's own message — never a false green, which
    // is the one outcome this lane exists to eliminate.
    //
    // Closing the gap properly needs the schema to gain the statement, not this
    // file to guess it: a per-action `requires_auth` in the typed-action
    // overrides, or a canary-level acknowledgement that the declared action
    // needs no credential, either of which this check could then read.
    if matches!(
        auth.requirement,
        AuthRequirement::None | AuthRequirement::Optional | AuthRequirement::Conditional
    ) {
        return Vec::new();
    }
    match auth.kind {
        AuthKind::Secrets => {
            let names = auth
                .secret_bindings
                .iter()
                .map(|binding| binding.secret_ref.clone())
                .collect::<Vec<_>>();
            let available = available_secret_names(&skill.name, workspace);
            let missing = names
                .iter()
                .filter(|name| !available.contains(*name))
                .cloned()
                .collect::<Vec<_>>();
            // `at_least_one` is satisfied by any single declared key, so it
            // must not be reported missing while one of them is present.
            if auth.requirement == AuthRequirement::AtLeastOne && missing.len() < names.len() {
                Vec::new()
            } else {
                missing
            }
        },
        // A CLI-profile skill authenticates through an operator-provisioned
        // profile this harness must not create or inspect.
        AuthKind::CliProfile
        | AuthKind::OAuthSession
        | AuthKind::BrowserProfile
        | AuthKind::NativePermission
        | AuthKind::DelegatedCredential => {
            vec![format!("{:?} profile", auth.kind)]
        },
        AuthKind::None => Vec::new(),
    }
}

/// The runtime root that holds installed scopes.
///
/// `PrimitiveExecCtx::default_storage_base_path()` deliberately redirects to a
/// throwaway directory under a cargo test harness (`workspace.rs:211`), so it
/// cannot be used to find the operator's real installed skills — a live canary
/// asking "does this skill work" must look where the skill is actually
/// installed. `runtime_root_env()` is the runtime's own public resolver for the
/// two environment variables; only the `$HOME/MagicianNotes` default is
/// mirrored here, matching the authority in `skillshub/Makefile:27`
/// (`MAGICIAN_ROOT_DIR`, then `MAGICIAN_STORAGE_PATH`, then `$HOME/MagicianNotes`).
/// The result is canonicalized. `~/MagicianNotes` is commonly a symlink onto an
/// external volume, and the governed runtime correctly refuses to grant scoped
/// path authority to an aliased directory. Resolving the real spelling here is
/// the fix; weakening that authority check to accept an alias would trade a
/// genuine safety property for the convenience of this lane.
fn runtime_root() -> PathBuf {
    let root = runtime_root_env().unwrap_or_else(|| match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("MagicianNotes"),
        _ => PathBuf::from("MagicianNotes"),
    });
    fs::canonicalize(&root).unwrap_or(root)
}

/// The installed scope root: `<runtime root>/scopes/<principal>/<workspace>`.
fn scope_root() -> PathBuf {
    runtime_root()
        .join("scopes")
        .join(CANARY_PRINCIPAL)
        .join(CANARY_WORKSPACE)
}

/// This process's own directory, canonicalized exactly the way the implicit-CLI
/// lane canonicalizes it, so the runner and the lane name the same directory.
fn process_directory() -> PathBuf {
    let current = std::env::current_dir().expect("the canary process has a working directory");
    fs::canonicalize(&current).unwrap_or(current)
}

/// The directory this skill's governed child will actually be launched in.
///
/// The scope root is NOT that directory for every skill, and the disagreement is
/// deliberate rather than incidental. `dispatch_governed_runtime_blocking`
/// (`magician/src/magician_v2/execution/primitive_dispatch/governed_runtime.rs`)
/// routes a contract whose auth is `cli_profile` with
/// `profile_selection.mode: implicit` and `storage.kind: cli_owned` into
/// `dispatch_implicit_cli_runtime`, whose `implicit_cli_working_directory` binds
/// `std::env::current_dir()` — the CALLING process's directory. That is the whole
/// point of the lane: a CLI that owns its own completed login (`agy`, `claude`,
/// `codex`, `opencode`, and `ocr`, which borrows their identity) operates on the
/// directory its caller is standing in. Every other lane binds the scope root.
///
/// A contract declaring `working_directory.mode: denied` gets no directory bound
/// in either lane, so its child simply inherits this process's own.
///
/// The three auth fields read here are the same three the router reads, on the
/// same contract: the router's `effective_contract` differs from the package
/// contract only in `requires.bins`/`requires.entrypoint` and `policy_floor`, and
/// touches no auth field. So this is reading the declaration, not re-deriving the
/// decision — and unlike a hardcoded skill name it is already correct for the
/// tenth CLI-owned skill on the day it lands.
fn governed_working_directory(skill: &CanarySkill) -> PathBuf {
    let contract = &skill.package.contract;
    let mode = match &contract.runtime {
        RuntimeProtocol::Cli {
            working_directory, ..
        } => working_directory.mode,
        RuntimeProtocol::Mcp { .. } => WorkingDirectoryMode::Denied,
    };
    let implicit_cli_owned = contract.auth.kind == AuthKind::CliProfile
        && matches!(contract.auth.profile_selection, ProfileSelection::Implicit)
        && matches!(contract.auth.storage, AuthStorage::CliOwned);
    match mode {
        WorkingDirectoryMode::Workspace if !implicit_cli_owned => scope_root(),
        // `output_root` is refused by both lanes today, so there is no base to
        // name and the call fails loudly on its own. The runner must not invent
        // one to keep it quiet.
        _ => process_directory(),
    }
}

/// Secret NAMES the host offers this skill, from the same declarative sources
/// the runtime itself reads. Values are never touched.
///
/// Order matters and mirrors the documented rule that exa's own SKILL.md
/// states: a vault record always wins, and an existing private scoped `.env`
/// remains a migration bridge when no vault record exists. The **installed
/// scope** is searched, not the package source: the repository carries only
/// `config/.env.example`, while the real credential lives beside the installed
/// skill. Searching only the source is what made this harness report exa — the
/// skill whose breakage started all of this — as a benign skip, which is the
/// same false negative it exists to eliminate.
fn available_secret_names(skill: &str, workspace: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for candidate in [
        // Vault-backed and scoped-bridge credentials for the installed skill.
        scope_root().join("skills").join(skill).join("config/.env"),
        runtime_root().join(".env"),
        // Package source and repository roots, as fallbacks only.
        workspace.join("skillshub").join(skill).join("config/.env"),
        workspace.join("magician_data_v3/.env"),
        workspace.join(".env"),
    ] {
        let Ok(source) = fs::read_to_string(&candidate) else {
            continue;
        };
        for line in source.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            // `value` is bound only to test emptiness and is dropped here.
            // Nothing in this file reads, prints, or stores a secret value.
            if value.trim().is_empty() {
                continue;
            }
            let name = name.trim().trim_start_matches("export ").trim();
            if !name.is_empty() {
                names.push(name.to_owned());
            }
        }
    }
    for (name, value) in std::env::vars_os() {
        if value.is_empty() {
            continue;
        }
        if let Some(name) = name.to_str() {
            names.push(name.to_owned());
        }
    }
    names
}

/// A declared process memory ceiling this host has no way to hold.
///
/// `document-to-markdown` declares `runtime.limits.memory_bytes: 4 GiB`. Where
/// `RLIMIT_AS` is a real address-space resource that becomes
/// `setrlimit(RLIMIT_AS, …)` before the child execs. Darwin aliases `RLIMIT_AS`
/// onto `RLIMIT_RSS` and answers every finite value with `EINVAL`, so it holds
/// the ceiling from the parent instead: the executor samples the owned process
/// group's physical footprint and terminates it on breach
/// (`MemoryLimitEnforcement::ParentFootprintWatchdog`). Both are enforcement, so
/// both run the skill.
///
/// What remains skippable is a host with neither mechanism, where
/// `GovernedBatchProcess::from_authorized_parts` refuses to build an executable
/// capability carrying a bound nothing can honour. Running the child there would
/// leave the manifest advertising a ceiling that exists nowhere, which is the
/// same class of false green this lane exists to eliminate.
///
/// That refusal is a fact about the HOST and not about the package, so it belongs
/// with the credential and fixture preconditions: SKIPPED, with the reason named.
/// PASSED would restate the lie about the ceiling; FAILED would blame a package
/// that is correct.
///
/// Keyed on the declared limit and on `tool-runtime-core`'s own published
/// capability predicate, never on a skill name, so any skill that later declares
/// a ceiling is covered without touching this file — and so a platform that gains
/// a mechanism starts running these skills with no edit here.
fn unenforceable_memory_limit(skill: &CanarySkill) -> Option<u64> {
    if process_memory_limits_are_enforceable() {
        return None;
    }
    match &skill.package.contract.runtime {
        RuntimeProtocol::Cli { limits, .. } => limits.memory_bytes,
        RuntimeProtocol::Mcp { limits, .. } => limits.memory_bytes,
    }
}

/// Packaged canary fixtures the installed scope does not yet carry.
///
/// A fixture that cannot be authored as a manifest string — `CanaryRun`'s
/// `fixtures` is `BTreeMap<String, String>` and this runner writes it
/// verbatim, so a PNG, a workbook or a deck has no representation there —
/// ships inside the package at `canary-fixtures/`, and its canary spells it
/// relative to the governed working directory as
/// `skills/<name>/canary-fixtures/<file>`. `make -C skillshub install-scope`
/// links the package's files into the scope, at which point the two spellings
/// name the same bytes.
///
/// Before that install has run the file is simply absent and the call fails
/// on a missing input. That is a report about the installation, not about the
/// skill, so it is SKIPPED — the same honesty rule the credential check
/// already follows, and the same reason it exists: a red that means "you have
/// not installed this" teaches nothing and trains the reader to ignore the
/// lane.
///
/// Keyed on the package layout convention, never on a skill name: any skill
/// that ships a `canary-fixtures/` directory is covered the day it is added.
///
/// This precondition belongs only to a skill whose lane stands in the scope,
/// because installation is what makes the declared path resolve there. A skill
/// whose lane stands somewhere else never reads the scope at all, and gets the
/// same fixture staged from the package by `stage_packaged_fixtures` — so
/// demanding an install of it would be a skip with no cause behind it.
fn uninstalled_fixtures(skill: &CanarySkill, workspace: &Path) -> Vec<String> {
    let packaged = workspace
        .join("skillshub")
        .join(&skill.name)
        .join("canary-fixtures");
    let Ok(entries) = fs::read_dir(&packaged) else {
        return Vec::new();
    };
    let installed = scope_root()
        .join("skills")
        .join(&skill.name)
        .join("canary-fixtures");
    let mut missing = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.file_name())
        // `is_file` follows the link, which is exactly the question: the
        // installed entry is a symlink onto the package file, and what matters
        // is whether the tool can read bytes through it.
        .filter(|name| !installed.join(name).is_file())
        .map(|name| {
            format!(
                "skills/{}/canary-fixtures/{}",
                skill.name,
                name.to_string_lossy()
            )
        })
        .collect::<Vec<_>>();
    missing.sort();
    missing
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Passed,
    Failed,
    Skipped,
    Exempt,
    NotInTier,
}

#[derive(Debug, serde::Serialize)]
struct CanaryOutcome {
    name: String,
    verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<String>,
}

/// Resolve the tier ceiling for this run. Media generation and delegating
/// coding agents bill real money per call, so reaching them takes an explicit
/// opt-in rather than simply running the suite.
fn requested_tier() -> CanaryCostTier {
    match std::env::var("TOOL_CANARY_TIER").ok().as_deref() {
        Some("free") => CanaryCostTier::Free,
        Some("expensive") => CanaryCostTier::Expensive,
        _ => CanaryCostTier::Cheap,
    }
}

/// Optional comma-separated package selector for focused diagnosis. The
/// default remains the complete tier sweep; unknown names fail loudly so a
/// typo cannot produce an empty green run.
fn requested_skill_names() -> Option<BTreeSet<String>> {
    let raw = std::env::var("TOOL_CANARY_SKILLS").ok()?;
    let names = raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert!(
        !names.is_empty(),
        "TOOL_CANARY_SKILLS must name at least one skill"
    );
    Some(names)
}

/// Exactly what this lane created, so it can remove exactly that and nothing
/// else.
///
/// Directories are tracked as well as files. The lane now materializes fixtures
/// outside the operator's scope for skills whose dispatch lane launches
/// elsewhere — under a repository directory, in a cargo run — and leaving
/// `skills/<name>/canary-fixtures/` behind there would be litter in a checkout.
#[derive(Default)]
struct StagedFixtures {
    files: Vec<PathBuf>,
    /// Shallowest first, in creation order.
    directories: Vec<PathBuf>,
}

impl StagedFixtures {
    fn remove(&self) {
        for path in &self.files {
            let _ = fs::remove_file(path);
        }
        // Deepest first on the way out. `remove_dir` refuses a non-empty
        // directory, which is the wanted safety property: anything this lane did
        // not put there keeps its parent alive.
        for path in self.directories.iter().rev() {
            let _ = fs::remove_dir(path);
        }
    }
}

/// `create_dir_all` that records which directories it actually created.
fn create_directory_recording(path: &Path, created: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_directory_recording(parent, created)?;
    }
    match fs::create_dir(path) {
        Ok(()) => {
            created.push(path.to_path_buf());
            Ok(())
        },
        // Lost a race, or the path appeared between the check and the call.
        // Either way this lane did not create it and must not remove it.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Materialize a canary's declared `fixtures:` inside the governed working
/// directory of the lane this skill actually routes to.
///
/// A fixture path is spelled relative to that directory, so the base is the
/// whole question — see `governed_working_directory`. Writing every skill's
/// fixtures at the scope root is what produced the doubled `magician/magician/`
/// path for the one skill whose lane stands elsewhere.
fn write_fixtures(base: &Path, run: &CanaryRun, staged: &mut StagedFixtures) -> Result<(), String> {
    for (name, content) in &run.fixtures {
        let path = base.join(name);
        let parent = path
            .parent()
            .ok_or_else(|| format!("fixture {name} has no parent"))?;
        create_directory_recording(parent, &mut staged.directories)
            .map_err(|error| format!("fixture dir {name}: {error}"))?;
        fs::write(&path, content).map_err(|error| format!("fixture {name}: {error}"))?;
        staged.files.push(path);
    }
    Ok(())
}

/// Reproduce a package's shipped `canary-fixtures/` under a base that is not the
/// scope root.
///
/// A packaged fixture — a PNG, a workbook, a deck — has no representation in the
/// manifest's `fixtures:` map, so its canary names it as
/// `skills/<name>/canary-fixtures/<file>`: the path `install-scope` creates under
/// the scope. For a lane that does not stand in the scope that spelling names
/// nothing, and no other single spelling could name both bases, because they are
/// different directories. So the runner reproduces the declared relative path
/// under the base the child will really use.
///
/// The bytes are copied from the package rather than from the installed link.
/// The package is always present, and sourcing from it keeps a lane that never
/// reads the scope free of an install precondition it does not have.
fn stage_packaged_fixtures(
    skill: &CanarySkill,
    workspace: &Path,
    base: &Path,
    staged: &mut StagedFixtures,
) -> Result<(), String> {
    let packaged = workspace
        .join("skillshub")
        .join(&skill.name)
        .join("canary-fixtures");
    let Ok(entries) = fs::read_dir(&packaged) else {
        return Ok(());
    };
    let target = base
        .join("skills")
        .join(&skill.name)
        .join("canary-fixtures");
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let destination = target.join(entry.file_name());
        // Never overwrite, and never adopt for deletion, a file this lane did
        // not place there.
        if destination.exists() {
            continue;
        }
        create_directory_recording(&target, &mut staged.directories)
            .map_err(|error| format!("fixture dir {}: {error}", target.display()))?;
        fs::copy(entry.path(), &destination)
            .map_err(|error| format!("fixture {}: {error}", destination.display()))?;
        staged.files.push(destination);
    }
    Ok(())
}

/// Count items at an RFC 6901 pointer, where the empty pointer is the whole
/// document — the correct shape for an adapter returning a bare array.
fn items_at(value: &serde_json::Value, pointer: &str) -> Option<usize> {
    let target = if pointer.is_empty() {
        Some(value)
    } else {
        value.pointer(pointer)
    }?;
    match target {
        serde_json::Value::Array(items) => Some(items.len()),
        serde_json::Value::Null => Some(0),
        _ => None,
    }
}

/// Judge one completed call against the manifest's declared expectations.
fn judge(skill: &CanarySkill, run: &CanaryRun, text: &str, elapsed_ms: u64) -> Vec<String> {
    let mut failures = Vec::new();
    let expect = &run.expect;

    if elapsed_ms > expect.max_latency_ms {
        failures.push(format!(
            "took {elapsed_ms}ms, above the declared {}ms ceiling",
            expect.max_latency_ms
        ));
    }

    if let Some(substring) = expect.stdout_contains.as_deref() {
        if !text.contains(substring) {
            failures.push(format!("output does not contain {substring:?}"));
        }
    }

    let needs_json = expect.min_items.is_some()
        || !expect.require_pointers.is_empty()
        || expect.max_cost_microunits.is_some();
    if !needs_json {
        return failures;
    }

    let parsed = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(parsed) => parsed,
        Err(error) => {
            failures.push(format!("result is not JSON: {error}"));
            return failures;
        },
    };

    // The assertion that would have caught the original failure. An adapter
    // that answers with an error envelope has NOT succeeded, whatever its exit
    // status: this is the zero-items provider-unavailable / misconfigured case
    // that previously degraded to a partial success and went unread.
    let error_pointer = expect
        .error_pointer
        .as_deref()
        .or(skill.error_pointer.as_deref());
    if let Some(pointer) = error_pointer {
        if let Some(found) = parsed.pointer(pointer) {
            if !found.is_null() {
                failures.push(format!(
                    "adapter reported a provider error at {pointer}: {}",
                    serde_json::to_string(found).unwrap_or_default()
                ));
            }
        }
    }

    if let Some(minimum) = expect.min_items {
        let pointer = expect
            .items_pointer
            .as_deref()
            .or(skill.items_pointer.as_deref());
        match pointer {
            None => failures.push(
                "declares min_items but neither the canary nor content_source names an \
                 items pointer"
                    .to_owned(),
            ),
            Some(pointer) => match items_at(&parsed, pointer) {
                None => failures.push(format!("{pointer} does not resolve to an array")),
                Some(count) if (count as u32) < minimum => failures.push(format!(
                    "returned {count} items at {pointer}, below the declared minimum of \
                     {minimum}"
                )),
                Some(_) => {},
            },
        }
    }

    // A required pointer is a POSITIVE assertion, so the empty forms of a value
    // count as absent: null, a blank string, and numeric zero. Zero matters —
    // an adapter that fetched nothing reports `chars_extracted: 0`, and
    // accepting that as "present" would let the exact no-content failure this
    // lane hunts for register as a pass.
    for pointer in &expect.require_pointers {
        match parsed.pointer(pointer) {
            None | Some(serde_json::Value::Null) => {
                failures.push(format!("required pointer {pointer} is absent"));
            },
            Some(serde_json::Value::String(text)) if text.trim().is_empty() => {
                failures.push(format!("required pointer {pointer} is empty"));
            },
            Some(serde_json::Value::Number(number)) if number.as_f64() == Some(0.0) => {
                failures.push(format!("required pointer {pointer} is zero"));
            },
            Some(serde_json::Value::Array(items)) if items.is_empty() => {
                failures.push(format!("required pointer {pointer} is an empty array"));
            },
            Some(_) => {},
        }
    }

    if let (Some(ceiling), Some(pointer)) =
        (expect.max_cost_microunits, skill.cost_pointer.as_deref())
    {
        if let Some(found) = parsed.pointer(pointer).and_then(serde_json::Value::as_f64) {
            // Two declarations, both read from the package and neither assumed.
            //
            // The ENCODING says how to scale the reported number. Packages
            // differ — one reports decimal major units, another reports
            // microunits directly — so assuming a single conversion would
            // misread the other by a factor of a million.
            //
            // The COMMODITY says what the number counts, and it is the half
            // that made this comparison unsound. exa prices in `usd`; tavily
            // prices in `tavily_credit`, one credit per basic search, because
            // the provider returns no money figure at all and the runtime has
            // no plan-specific rate to convert one. A ceiling of 20000 copied
            // from the first package onto the second read one credit as a
            // dollar of spend and failed a call that really cost about $0.008.
            // The product's own retrieval budgets are keyed by commodity for
            // this reason (`content_sources/retrieval.rs`); the lane matches
            // that rather than flattening it.
            //
            // Either declaration missing or disagreeing is reported, never
            // guessed: inventing a scale or a currency here is the same class
            // of mistake as inventing an output shape.
            let declared = expect.max_cost_commodity.as_deref();
            let reported = skill.cost_commodity.as_deref();
            if declared != reported {
                failures.push(format!(
                    "cost ceiling is declared in {} but the package reports {} at {pointer}; \
                     a ceiling can only bound the commodity it names",
                    declared.unwrap_or("<undeclared>"),
                    reported.unwrap_or("<undeclared>")
                ));
            } else {
                let microunits = match skill.cost_encoding.as_deref() {
                    Some("microunits") => Some(found.round() as u64),
                    Some("decimal_major_units") => Some((found * 1_000_000.0).round() as u64),
                    other => {
                        failures.push(format!(
                            "cost is reported at {pointer} under encoding {:?}, which this \
                             lane cannot convert; teach it the encoding rather than guessing \
                             a scale",
                            other.unwrap_or("<undeclared>")
                        ));
                        None
                    },
                };
                if let Some(microunits) = microunits {
                    if microunits > ceiling {
                        let commodity = declared.unwrap_or("<undeclared>");
                        failures.push(format!(
                            "reported cost {microunits} microunits of {commodity} exceeds the \
                             declared ceiling of {ceiling}"
                        ));
                    }
                }
            }
        }
    }

    failures
}

/// Opt-in: `make test-tool-skills-live`, which passes `--ignored`.
///
/// This lane makes real network calls and spends real money — the `cheap` tier
/// alone bills exa, tavily, klipy, imgflip, and both LLM websearch skills on
/// every run. It also inherits every provider's uptime as a test dependency, so
/// an ordinary `cargo test -p magician` would go red because Reddit rate-limited,
/// not because anything here regressed.
///
/// Tier 1 (`magician/tests/tool_skill_contract.rs`) is the lane that
/// runs unconditionally. It is hermetic, and `every_tool_skill_declares_a_canary`
/// fails if a skill ships without a declaration — so the canaries cannot rot
/// unnoticed while this test is ignored, which is the failure mode that let a
/// previous eval drive a deleted path for months.
#[ignore = "live lane: spends money and needs network; run via make test-tool-skills-live"]
#[tokio::test(flavor = "multi_thread")]
async fn tool_skill_canaries() {
    let workspace = workspace_root();
    let requested = requested_tier();
    let mut skills = canary_skills();
    if let Some(requested_names) = requested_skill_names() {
        let available = skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<BTreeSet<_>>();
        let unknown = requested_names
            .difference(&available)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            unknown.is_empty(),
            "TOOL_CANARY_SKILLS names unknown skills: {unknown:?}"
        );
        skills.retain(|skill| requested_names.contains(&skill.name));
    }

    // Build the production capability registry from the real packages, then
    // hand it to the real invoker. Nothing below this line constructs a
    // command, an argument vector, or an environment.
    // Mirror the boot order in `bin/magician.rs`: the installed scope's skills
    // win, and the repository's packages fill any gap.
    //
    // This is not a detail. A pack loaded from the repository carries
    // `legacy_secret_environment = <repo>/skillshub/<skill>/config/.env`, which
    // holds only `.env.example` — so the governed runtime would find no
    // credential and the skill would fail for a reason that has nothing to do
    // with whether it works. The installed pack points at the scoped `.env`
    // bridge where the real credential lives, which is how production reaches
    // it.
    //
    // Executable resolution needs no help here and must not be given any.
    // `project_runtime_package_to_pack` does take both a `skill_dir` and a
    // `resolved_skill_dir` — credentials from the former, `bin/` from the
    // latter — but the split is decided by `resolve_skill_path`, which
    // rewrites only when `MAGICIAN_SKILLSHUB_ROOT` names a container's
    // skillshub and is the identity otherwise. On a native host the two
    // arguments are therefore the same directory, and every pack's executable
    // directory is the scope's own `bin/`, whose entries are symlinks onto the
    // package sources. That is the directory production resolves too, so a
    // runner that aimed executables at the repository instead would be probing
    // a path production never takes — the re-implementation this file exists
    // to refuse. Whether a linked entry point may be launched is the governed
    // runtime's judgement, in `reviewed_executable_directory`, and it belongs
    // there.
    let registry = std::sync::Arc::new(CapabilityRegistry::new());
    let scope_skills = scope_root().join("skills");
    let mut installed = std::collections::BTreeSet::new();
    for pack in load_pack_defs_from_skills_dir(&scope_skills) {
        installed.insert(pack.name.clone());
        registry.set_pack_definition(&pack.name.clone(), pack);
    }
    let mut from_repository = 0usize;
    for pack in load_pack_defs_from_skills_dir(&workspace.join("skillshub")) {
        if installed.contains(&pack.name) {
            continue;
        }
        from_repository += 1;
        registry.set_pack_definition(&pack.name.clone(), pack);
    }
    println!(
        "packs: {} from the installed scope ({}), {from_repository} from the repository",
        installed.len(),
        scope_skills.display()
    );

    // The governed runtime requires a secret authority to settle its audit
    // receipts, exactly as it does at boot. `in_memory_secret_runtime_bootstrap`
    // is the runtime's own harness constructor for that, so the lane supplies a
    // real one rather than reaching around the requirement.
    //
    // The audit and vault roots stay on a throwaway directory — they are this
    // lane's own bookkeeping, not the operator's. The SCOPE, by contrast, must
    // be the real installed one, because that is where the skills and their
    // credentials actually live.
    let bookkeeping = tempfile::tempdir().expect("canary bookkeeping root");
    let storage_base = fs::canonicalize(bookkeeping.path()).expect("canonical bookkeeping root");
    let (audit_key, _, _) = in_memory_secret_runtime_bootstrap().into_parts();
    let (vault_key, vault_capabilities, _) = in_memory_secret_runtime_bootstrap().into_parts();
    let secret_store = std::sync::Arc::new(SecretStore::new_empty(
        audit_key,
        storage_base.join("secrets"),
    ));
    let secret_resolver = std::sync::Arc::new(SecretStoreResolver::new_with_capabilities(
        vault_key,
        storage_base.join("credential-vault"),
        vault_capabilities,
    ));

    // `default_for_runtime()` resolves storage through the test-harness branch,
    // so the runtime root is set explicitly to the operator's real one.
    let mut exec_ctx = PrimitiveExecCtx::default_for_runtime()
        .with_scope(
            Some(CANARY_PRINCIPAL.to_owned()),
            Some(CANARY_WORKSPACE.to_owned()),
            None,
        )
        .with_secret_context(Some(secret_store), None)
        .with_secret_store_resolver(Some(secret_resolver));
    exec_ctx.storage_base_path = runtime_root();
    let _ = storage_base;

    let scope_root = scope_root();
    let scope_paths = CapabilityScopePaths {
        principal: CANARY_PRINCIPAL.to_owned(),
        workspace: CANARY_WORKSPACE.to_owned(),
        capabilities_root: scope_root.clone(),
        bots_root: scope_root.join("bots"),
        auth_root: scope_root.join("auth"),
        workdirs_root: scope_root.join("workdirs"),
        home_root: scope_root.join("home"),
        node_modules_bin: workspace.join("skillshub/node_modules/.bin"),
        node_bin: workspace.join("skillshub/.node/bin"),
        venv_bin: workspace.join("skillshub/.venv/bin"),
    };
    // A host with no installed scope is a legitimate state — a fresh clone, or
    // CI. It must degrade to "no credentials found", which surfaces as SKIPPED
    // per skill, rather than taking the whole lane down. The distinction is
    // still visible: the pack census above reports how many packs came from the
    // scope versus the repository.
    if !scope_root.is_dir() {
        println!(
            "note: no installed scope at {} — credentialed skills will report SKIPPED",
            scope_root.display()
        );
    }

    let invoker =
        ScopedDeterministicCapabilityInvoker::new(registry, exec_ctx).with_scope_paths(scope_paths);

    let mut outcomes = Vec::new();
    // Fixtures are written into whichever directory each skill's dispatch lane
    // will actually launch in, so the lane removes exactly the files and the
    // directories it created — nothing it did not write is touched.
    let mut staged = StagedFixtures::default();
    for skill in &skills {
        let Some(run) = skill.canary.run() else {
            let reason = skill
                .canary
                .exemption()
                .map(|exempt| exempt.reason.clone())
                .unwrap_or_default();
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Exempt,
                cost_tier: None,
                action: None,
                duration_ms: 0,
                detail: Some(reason),
                failures: Vec::new(),
            });
            continue;
        };

        if !run.cost_tier.runs_at(requested) {
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::NotInTier,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: 0,
                detail: Some(format!(
                    "tier {} is above the requested ceiling {requested}",
                    run.cost_tier
                )),
                failures: Vec::new(),
            });
            continue;
        }

        let missing = missing_credentials(skill, &workspace);
        if !missing.is_empty() {
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Skipped,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: 0,
                detail: Some(format!("no credential: {}", missing.join(", "))),
                failures: Vec::new(),
            });
            continue;
        }

        if let Some(bytes) = unenforceable_memory_limit(skill) {
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Skipped,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: 0,
                detail: Some(format!(
                    "this host has neither a kernel address-space bound nor a parent \
                     footprint watchdog for the declared {bytes}-byte process memory \
                     ceiling, and the governed runtime refuses to launch a child under a \
                     bound it cannot hold rather than drop it silently"
                )),
                failures: Vec::new(),
            });
            continue;
        }

        // Where this skill's child will really stand. The scope root is the
        // answer for most skills and the wrong answer for a CLI-owned one.
        let base = governed_working_directory(skill);
        let stands_in_scope = base == scope_root;

        if stands_in_scope {
            let uninstalled = uninstalled_fixtures(skill, &workspace);
            if !uninstalled.is_empty() {
                outcomes.push(CanaryOutcome {
                    name: skill.name.clone(),
                    verdict: Verdict::Skipped,
                    cost_tier: Some(run.cost_tier.to_string()),
                    action: Some(run.action.clone()),
                    duration_ms: 0,
                    detail: Some(format!(
                        "packaged canary fixtures are not installed in the scope \
                         ({}); run `make -C skillshub install-scope`",
                        uninstalled.join(", ")
                    )),
                    failures: Vec::new(),
                });
                continue;
            }
        } else if let Err(error) = stage_packaged_fixtures(skill, &workspace, &base, &mut staged) {
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Failed,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: 0,
                detail: Some("packaged fixtures could not be staged".to_owned()),
                failures: vec![error],
            });
            continue;
        }

        if let Err(error) = write_fixtures(&base, run, &mut staged) {
            outcomes.push(CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Failed,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: 0,
                detail: Some("fixtures could not be materialized".to_owned()),
                failures: vec![error],
            });
            continue;
        }

        let mut arguments = serde_json::Map::new();
        for (name, value) in &run.input {
            arguments.insert(name.clone(), value.clone());
        }
        let invocation = DeterministicCapabilityInvocation::new(
            skill.name.clone(),
            run.action.clone(),
            serde_json::Value::Object(arguments),
            DeterministicCapabilityInvocationSource::InternalSystem,
        )
        .with_scope(CANARY_PRINCIPAL, CANARY_WORKSPACE);

        let started = Instant::now();
        let result = invoker.invoke(invocation).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let outcome = match result {
            Err(error) => CanaryOutcome {
                name: skill.name.clone(),
                verdict: Verdict::Failed,
                cost_tier: Some(run.cost_tier.to_string()),
                action: Some(run.action.clone()),
                duration_ms: elapsed_ms,
                detail: Some("the governed call did not succeed".to_owned()),
                failures: vec![error.to_string()],
            },
            Ok(result) => {
                let text = result.output_text().unwrap_or_default().to_owned();
                let failures = judge(skill, run, &text, elapsed_ms);
                CanaryOutcome {
                    name: skill.name.clone(),
                    verdict: if failures.is_empty() {
                        Verdict::Passed
                    } else {
                        Verdict::Failed
                    },
                    cost_tier: Some(run.cost_tier.to_string()),
                    action: Some(run.action.clone()),
                    duration_ms: elapsed_ms,
                    detail: None,
                    failures,
                }
            },
        };
        outcomes.push(outcome);
    }

    staged.remove();

    report(&outcomes, requested);

    let failed = outcomes
        .iter()
        .filter(|outcome| outcome.verdict == Verdict::Failed)
        .map(|outcome| format!("{}: {}", outcome.name, outcome.failures.join("; ")))
        .collect::<Vec<_>>();
    assert!(failed.is_empty(), "{failed:#?}");
}

fn count(outcomes: &[CanaryOutcome], verdict: Verdict) -> usize {
    outcomes
        .iter()
        .filter(|outcome| outcome.verdict == verdict)
        .count()
}

/// Write the lane report and print the breakdown.
///
/// PASSED and SKIPPED are reported as separate counts and never summed. A run
/// where everything skipped must be visibly distinct from a run where
/// everything passed, or the lane becomes a green light for an untested
/// surface. SKIPPED itself means the HOST is missing a precondition — a
/// credential, a packaged fixture that `install-scope` has not linked yet, or a
/// declared process memory ceiling this kernel cannot apply — never that the
/// skill was found wanting.
fn report(outcomes: &[CanaryOutcome], requested: CanaryCostTier) {
    let passed = count(outcomes, Verdict::Passed);
    let failed = count(outcomes, Verdict::Failed);
    let skipped = count(outcomes, Verdict::Skipped);
    let exempt = count(outcomes, Verdict::Exempt);
    let not_in_tier = count(outcomes, Verdict::NotInTier);

    println!("\n=== tool skill canaries (tier ceiling: {requested}) ===");
    for outcome in outcomes {
        let mark = match outcome.verdict {
            Verdict::Passed => "PASS",
            Verdict::Failed => "FAIL",
            // SKIPPED covers several preconditions the host owns rather than the
            // skill — an absent credential, an uninstalled packaged fixture, a
            // declared memory ceiling this kernel cannot apply — so the reason
            // travels in the detail column instead of the mark, which must not
            // name only one of them.
            Verdict::Skipped => "SKIPPED",
            Verdict::Exempt => "EXEMPT",
            Verdict::NotInTier => "not-in-tier",
        };
        println!(
            "{mark:24} {:38} {:>7}ms {}",
            outcome.name,
            outcome.duration_ms,
            outcome.detail.as_deref().unwrap_or("")
        );
        for failure in &outcome.failures {
            println!("{:24} {:38}   - {failure}", "", "");
        }
    }
    println!(
        "\nPASSED {passed}  FAILED {failed}  SKIPPED {skipped}  EXEMPT {exempt}  \
         NOT-IN-TIER {not_in_tier}  (total {})",
        outcomes.len()
    );

    let base = std::env::var("COVERAGE_BASE_DIR")
        .unwrap_or_else(|_| "/Volumes/build/magician/coverage".to_owned());
    let directory = PathBuf::from(base).join("evals/tool-skills");
    if fs::create_dir_all(&directory).is_err() {
        eprintln!("tool-skills eval report directory is unavailable");
        return;
    }
    let document = serde_json::json!({
        "gate": if failed == 0 { "PASS" } else { "FAIL" },
        "generated_by": "magician::tool_skill_canary",
        "tier_ceiling": requested.to_string(),
        "scenario_count": outcomes.len(),
        "passed_count": passed,
        "failed_count": failed,
        "skipped_count": skipped,
        "exempt_count": exempt,
        "not_in_tier_count": not_in_tier,
        "scenarios": outcomes,
    });
    let json = serde_json::to_string_pretty(&document).unwrap_or_default();
    let _ = fs::write(directory.join("latest.json"), &json);
    let _ = fs::write(
        directory.join("latest.html"),
        html(outcomes, requested, &document),
    );
    println!("report: {}", directory.join("latest.html").display());
}

fn html(
    outcomes: &[CanaryOutcome],
    requested: CanaryCostTier,
    document: &serde_json::Value,
) -> String {
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut rows = String::new();
    for outcome in outcomes {
        let (mark, colour) = match outcome.verdict {
            Verdict::Passed => ("PASS", "#137333"),
            Verdict::Failed => ("FAIL", "#b3261e"),
            Verdict::Skipped => ("SKIPPED", "#a06000"),
            Verdict::Exempt => ("EXEMPT", "#5f6368"),
            Verdict::NotInTier => ("not-in-tier", "#5f6368"),
        };
        let mut detail = outcome.detail.clone().unwrap_or_default();
        if !outcome.failures.is_empty() {
            detail = outcome.failures.join("; ");
        }
        rows.push_str(&format!(
            "<tr><td style=\"color:{colour};font-weight:600\">{mark}</td><td>{}</td>\
             <td>{}</td><td style=\"text-align:right\">{}</td><td>{}</td></tr>",
            escape(&outcome.name),
            escape(outcome.cost_tier.as_deref().unwrap_or("")),
            outcome.duration_ms,
            escape(&detail)
        ));
    }
    let counts = |key: &str| document.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Tool skill canaries</title>\
         <style>body{{font:14px system-ui;margin:2rem;max-width:70rem}}\
         table{{border-collapse:collapse;width:100%}}\
         th,td{{border-bottom:1px solid #ddd;padding:.4rem .6rem;text-align:left}}\
         .k{{display:inline-block;margin-right:1.5rem}}</style>\
         <h1>Tool skill canaries</h1>\
         <p>Live Tier 2 lane driven through the production \
         <code>GovernedExecutionCoordinator</code>. Tier ceiling: <b>{requested}</b>.</p>\
         <p><span class=\"k\">PASSED <b>{}</b></span>\
         <span class=\"k\">FAILED <b>{}</b></span>\
         <span class=\"k\">SKIPPED <b>{}</b></span>\
         <span class=\"k\">EXEMPT <b>{}</b></span>\
         <span class=\"k\">NOT-IN-TIER <b>{}</b></span></p>\
         <table><thead><tr><th>Verdict</th><th>Skill</th><th>Tier</th><th>ms</th>\
         <th>Detail</th></tr></thead><tbody>{rows}</tbody></table>",
        counts("passed_count"),
        counts("failed_count"),
        counts("skipped_count"),
        counts("exempt_count"),
        counts("not_in_tier_count"),
    )
}

/// Fixtures must be materialized under the directory each skill's dispatch lane
/// will actually launch in, and the two lanes do not agree on that directory.
///
/// This is the property whose absence produced the doubled `magician/magician/`
/// path: every fixture was written at the scope root while `ocr` — `cli_profile`
/// with `profile_selection: implicit` and `storage: cli_owned` — was launched in
/// the calling process's directory by `dispatch_implicit_cli_runtime`. No
/// relative spelling can name both bases, because they are different
/// directories.
///
/// The census at the end is the part that regresses silently: collapse the lane
/// distinction and every base becomes the scope root again while this file still
/// compiles and every other assertion here still holds.
#[test]
fn fixture_bases_follow_each_skills_dispatch_lane() {
    let scope = scope_root();
    let process = process_directory();
    let mut off_scope = Vec::new();
    for skill in canary_skills() {
        let base = governed_working_directory(&skill);
        assert!(
            base == scope || base == process,
            "{} resolves to {}, which is neither dispatch lane's working directory",
            skill.name,
            base.display()
        );
        if base != scope {
            off_scope.push(skill.name.clone());
        }
    }
    assert!(
        !off_scope.is_empty(),
        "no skill routes off the scope root, so either the lane distinction was \
         collapsed or every CLI-owned skill was removed"
    );
    println!(
        "skills launched outside the scope root: {}",
        off_scope.join(", ")
    );
}

/// A packaged fixture lands at exactly the path its canary spells, resolved
/// against the base the child will stand in.
///
/// Staged into a throwaway base rather than the real one: this assertion is
/// about the path arithmetic, and reproducing it over the shared directory would
/// race the live lane running in the same test binary.
#[test]
fn a_packaged_fixture_is_staged_at_the_path_its_canary_spells() {
    let workspace = workspace_root();
    let staging = tempfile::tempdir().expect("staging base");
    let base = staging.path();
    let mut proved = 0usize;
    for skill in canary_skills() {
        let packaged = workspace
            .join("skillshub")
            .join(&skill.name)
            .join("canary-fixtures");
        let Ok(entries) = fs::read_dir(&packaged) else {
            continue;
        };
        let names = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if names.is_empty() {
            continue;
        }
        let mut staged = StagedFixtures::default();
        stage_packaged_fixtures(&skill, &workspace, base, &mut staged)
            .unwrap_or_else(|error| panic!("stage {}: {error}", skill.name));
        for name in &names {
            // The spelling every packaged canary uses, and the one
            // `install-scope` creates under the scope.
            let declared = format!("skills/{}/canary-fixtures/{name}", skill.name);
            assert!(
                base.join(&declared).is_file(),
                "{} is not readable at {declared} under the staged base",
                skill.name
            );
            proved += 1;
        }
        // The lane removes exactly what it created, directories included.
        staged.remove();
        assert!(
            !base.join("skills").join(&skill.name).exists(),
            "staging left {} behind under the base",
            skill.name
        );
    }
    assert!(
        proved > 0,
        "no package ships a canary fixture, so this proof asserted nothing"
    );
}

/// Guard the guard: the discovery this lane runs on must not quietly shrink.
///
/// If a refactor breaks canary parsing, every skill would silently become
/// invisible here and the lane would report a serene zero failures over zero
/// calls.
#[test]
fn every_tool_skill_is_visible_to_the_lane() {
    let skills = canary_skills();
    let declared = fs::read_dir(workspace_root().join("skillshub"))
        .expect("skillshub is readable")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            fs::read_to_string(entry.path().join("SKILL.md"))
                .is_ok_and(|source| source.contains("runtime_contract:"))
        })
        .count();
    assert_eq!(
        skills.len(),
        declared,
        "the canary lane sees {} of {declared} tool skills",
        skills.len()
    );
}
