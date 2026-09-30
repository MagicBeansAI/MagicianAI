//! Architecture boundary tests for the platform-layering plan (workstream
//! 0.2, docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Two dependency-direction rules, enforced source-level so a regression
//! fails the standard `make check-all` gate (which compiles all targets)
//! and `make test`:
//!
//! 1. The `magician` lib must have no **production** dependency on the
//!    satellite crates (`magician-comms`, `magician-apps`,
//!    `magician-surfaces`, `magician-media`). Dev-dependencies (test
//!    fixtures) are allowed. This is the arrow that lane extraction and
//!    the 2.1 thinking-map decision depend on.
//!
//! 2. The substrate crates (`magician-core`, `runtime-core`, `magicllm`)
//!    must not reference product lanes (tutor / copilot / brainstorm /
//!    vibedev / channel_assist) outside a checked-in allowlist. Every
//!    allowlist entry states why it exists and which plan phase (or
//!    cleanup) retires it, and must go stale the moment its file stops
//!    producing hits — the staleness assertion enforces that. The scan
//!    covers each substrate crate's `src/` **and** `tests/` trees.
//!
//! A third rule keeps the first two honest: every `[workspace]` member
//! must be classified as satellite, substrate, or explicitly exempt, so
//! adding a crate is a deliberate act instead of a silent blind spot in
//! the scans above.
//!
//! This test deliberately imports nothing from the magician lib: it reads
//! files only, so its own correctness is easy to audit by reading.

use std::fs;
use std::path::{Path, PathBuf};

const MAGICIAN_MANIFEST: &str = include_str!("../Cargo.toml");

const SATELLITE_CRATES: [&str; 4] = [
    "magician-comms",
    "magician-apps",
    "magician-surfaces",
    "magician-media",
];

const SUBSTRATE_CRATES: [&str; 3] = ["magician-core", "runtime-core", "magicllm"];

/// Workspace members that are neither satellite lanes nor substrate. Every
/// entry states why the crate sits outside both rule sets today. A new
/// workspace member must land in `SATELLITE_CRATES`, `SUBSTRATE_CRATES`, or
/// here (with a reason) before `make check-all` goes green again — that is
/// the point of the classification tripwire below.
const EXEMPT_WORKSPACE_MEMBERS: &[(&str, &str)] = &[
    (
        "magician",
        "the monolith lib under test — these rules pin its edges",
    ),
    (
        "magician-bin",
        "the production binary; composes every plane by design",
    ),
    (
        "magician-api",
        "HTTP/WS surface extracted from the monolith; depends on the \
         magician lib, not the other way around",
    ),
    (
        "magician-learning",
        "learning plane (outcome learning, data room, bots) extracted from \
         the monolith; not a rule-1 lane",
    ),
    (
        "magician-chunking",
        "logical-chunking domain adapters extracted from the monolith; \
         generic mechanics live in magicllm",
    ),
    (
        "magician-app-contract",
        "pure transport-independent wire contracts; no lane or substrate \
         code",
    ),
    (
        "magician-mcp-client",
        "governed client boundary around the official Rust MCP SDK",
    ),
    (
        "magician-event-taxonomy",
        "minimal event-taxonomy types; exists to make event codegen fast",
    ),
    (
        "magician-setup",
        "the guided installer; a leaf binary over magician-components with no \
         edge to any lane or substrate crate",
    ),
    (
        "magician-components",
        "declared component graph plus a pure resolver; no lane or substrate \
         edges, and deliberately thin so the API, the setup wizard and the \
         desktop onboarding can all read it",
    ),
    (
        "magician-pty",
        "PTY session registry extracted so the portable-pty dep tree does \
         not recompile on every magician edit",
    ),
    (
        "magician-vector-index",
        "LanceDB/Arrow memory index isolated from the magician lib",
    ),
    (
        "magic-supervisor",
        "supervisor process crate; not a lane or substrate member",
    ),
    (
        "magicutor",
        "browser execution engine; production dep of the magician lib",
    ),
    (
        "document-to-markdown-cli",
        "standalone document-to-Markdown CLI adapter",
    ),
    (
        "magician-storage",
        "neutral typed capabilities; magician lib consumes it, not a product lane",
    ),
    (
        "magician-storage-migration",
        "owner-migration coordinator; magician lib depends, magician-bin must not",
    ),
    (
        "magician-storage-s3",
        "remote object/dataset adapters; not a magician-bin startup dependency",
    ),
    (
        "magician-storage-state",
        "remote state/lease adapters; not a magician-bin startup dependency",
    ),
    (
        "magician-storage-gate1",
        "disposable Gate 1 spike; not a magician-bin dependency",
    ),
    (
        "magician-decision",
        "structured-decision plane (Choice/Score IR and model adapters); the \
         magician lib depends on it, and it is neither a product lane nor \
         substrate",
    ),
    (
        "decision-engine-contract",
        "wire types and the Unix-socket client between the decision engine \
         and its hosts; no lane or substrate code",
    ),
    (
        "decision-engine",
        "the decision-engine process; a leaf binary hosts talk to over the \
         contract, not a lane or substrate member",
    ),
];

const LANE_TOKENS: [&str; 5] = [
    "tutor",
    "copilot",
    "brainstorm",
    "vibedev",
    "channel_assist",
];

/// A sanctioned lane reference inside a substrate crate. `removal` names
/// the plan workstream (or cleanup note) that retires the entry; the test
/// fails if the entry stops matching so the allowlist cannot rot.
struct AllowedLaneReference {
    crate_dir: &'static str,
    file: &'static str,
    reason: &'static str,
    removal: &'static str,
}

const ALLOWED_LANE_REFERENCES: &[AllowedLaneReference] = &[
    AllowedLaneReference {
        crate_dir: "magician-core",
        file: "src/prompts/constants.rs",
        reason: "prompt-store key constants for the tutor/copilot/vibedev lanes — \
                 the only sanctioned lane coupling in magician-core",
        removal: "plan workstreams 1.2/1.4 move prompt keys into lane-owned \
                  modules; delete this entry then",
    },
    AllowedLaneReference {
        crate_dir: "magicllm",
        file: "src/providers/openai_responses.rs",
        reason: "prose comments and #[cfg(test)] fixture ids describing Tutor \
                 completion-guard behavior (no code dependency)",
        removal: "reword comments / rename fixtures at any time; delete this \
                  entry with them",
    },
    AllowedLaneReference {
        crate_dir: "magician-core",
        file: "src/prompts/test_prompt_storage.rs",
        reason: "#[cfg(test)] fixtures exercising channel-assist prompt \
                 constants (test-only, no code dependency)",
        removal: "plan workstream 3.1 (comms extraction) or any fixture \
                  rename; delete this entry with it",
    },
];

#[test]
fn magician_lib_has_no_production_dependency_on_satellite_crates() {
    let mut section = String::new();
    let mut violations: Vec<String> = Vec::new();

    for line in MAGICIAN_MANIFEST.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = name.trim().to_string();
            // A `[dependencies.<crate>]` header names its dependency in the
            // header itself — scan the header too, not just body lines.
            if is_production_section(&section) {
                for satellite in SATELLITE_CRATES {
                    if section.contains(satellite) {
                        violations.push(format!("[{section}] (header)"));
                    }
                }
            }
            continue;
        }
        if !is_production_section(&section) {
            continue;
        }
        for satellite in SATELLITE_CRATES {
            if trimmed.contains(satellite) {
                violations.push(format!("[{section}] {trimmed}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "magician/Cargo.toml gained a production dependency on a satellite crate:\n{}\n\
         The magician lib must depend on satellites only as dev-dependencies; \
         a production edge here inverts the extraction arrows the \
         platform-layering plan (workstream 0.2) is built on.",
        violations.join("\n")
    );
}

#[test]
fn substrate_crates_do_not_reference_product_lanes_beyond_allowlist() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut violations: Vec<String> = Vec::new();
    let mut allowlist_hits = vec![false; ALLOWED_LANE_REFERENCES.len()];

    for crate_dir in SUBSTRATE_CRATES {
        let crate_root = workspace_root.join(crate_dir);
        let mut files: Vec<PathBuf> = Vec::new();
        // Scan both trees: `src/` for the crate itself and `tests/` for its
        // integration tests (magicllm/tests exists and references provider
        // prose — lane tokens must not appear there either).
        for scan_dir in ["src", "tests"] {
            collect_rust_files(&crate_root.join(scan_dir), &mut files);
        }

        for file in files {
            let relative = file
                .strip_prefix(workspace_root.join(crate_dir))
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let contents = match fs::read_to_string(&file) {
                Ok(contents) => contents,
                Err(error) => {
                    violations.push(format!(
                        "{crate_dir}/{relative}: unreadable ({error}); boundary \
                         scan cannot be trusted"
                    ));
                    continue;
                },
            };
            for (index, line) in contents.lines().enumerate() {
                let lowercase = line.to_lowercase();
                if !LANE_TOKENS.iter().any(|token| lowercase.contains(token)) {
                    continue;
                }
                let allowed = ALLOWED_LANE_REFERENCES
                    .iter()
                    .position(|entry| entry.crate_dir == crate_dir && entry.file == relative);
                match allowed {
                    Some(entry_index) => allowlist_hits[entry_index] = true,
                    None => violations.push(format!(
                        "{crate_dir}/{relative}:{}: lane reference: {line}",
                        index + 1
                    )),
                }
            }
        }
    }

    let stale: Vec<String> = ALLOWED_LANE_REFERENCES
        .iter()
        .zip(allowlist_hits)
        .filter(|(_, hit)| !hit)
        .map(|(entry, _)| {
            format!(
                "{}/{} — {} (removal: {})",
                entry.crate_dir, entry.file, entry.reason, entry.removal
            )
        })
        .collect();

    assert!(
        violations.is_empty() && stale.is_empty(),
        "substrate-crate lane-boundary violations:\n{}\n\nstale allowlist entries \
         (no longer produce hits — delete them):\n{}\n\nProduct lanes must not \
         leak into magician-core / runtime-core / magicllm; see the \
         platform-layering plan, workstream 0.2.",
        violations.join("\n"),
        stale.join("\n")
    );
}

/// Every `[workspace]` member must be deliberately classified. Without this
/// tripwire the hardcoded arrays above silently stop covering new crates:
/// a satellite-shaped crate added tomorrow would grow production edges into
/// the magician lib without rule 1 ever firing. Classifying is the point —
/// add the crate to `SATELLITE_CRATES`, `SUBSTRATE_CRATES`, or
/// `EXEMPT_WORKSPACE_MEMBERS` (with a reason) as part of the change.
#[test]
fn workspace_members_are_all_deliberately_classified() {
    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("Cargo.toml");
    let manifest = fs::read_to_string(&workspace_manifest)
        .unwrap_or_else(|error| panic!("read {}: {error}", workspace_manifest.display()));
    let members = parse_workspace_members(&manifest);

    assert!(
        !members.is_empty(),
        "parsed zero `[workspace] members` from {} — the text parse no longer \
         matches the manifest layout; fix parse_workspace_members",
        workspace_manifest.display()
    );

    let mut unclassified: Vec<&str> = Vec::new();
    for member in &members {
        let member = member.as_str();
        let classified = SATELLITE_CRATES.contains(&member)
            || SUBSTRATE_CRATES.contains(&member)
            || EXEMPT_WORKSPACE_MEMBERS
                .iter()
                .any(|(name, _)| *name == member);
        if !classified {
            unclassified.push(member);
        }
    }

    // The reverse direction: an array entry naming a crate the workspace no
    // longer contains is a stale classification that would quietly approve
    // nothing forever.
    let mut phantom: Vec<&str> = SATELLITE_CRATES
        .iter()
        .chain(SUBSTRATE_CRATES.iter())
        .copied()
        .chain(EXEMPT_WORKSPACE_MEMBERS.iter().map(|(name, _)| *name))
        .filter(|name| !members.contains(&name.to_string()))
        .collect();

    unclassified.sort();
    phantom.sort();
    phantom.dedup();

    assert!(
        unclassified.is_empty() && phantom.is_empty(),
        "workspace classification is stale in tests/architecture_boundaries.rs:\n\
         unclassified members (classify them!): {}\n\
         classified names missing from [workspace] members (delete them): {}\n\n\
         Every `[workspace]` member must be listed in SATELLITE_CRATES, \
         SUBSTRATE_CRATES, or EXEMPT_WORKSPACE_MEMBERS with a reason, so adding \
         a crate is a deliberate act and the lane scans above stay complete.",
        unclassified.join(", "),
        phantom.join(", ")
    );
}

/// Pull the `[workspace] members` entries out of the root manifest with a
/// text scan — the same read-files-only approach the other rules use, so
/// this test stays auditable by reading. Handles the multi-line array the
/// workspace uses today and a single-line `members = ["a", "b"]`; comments
/// inside the array contribute nothing because only quoted segments count.
fn parse_workspace_members(manifest: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if !inside {
            if !(trimmed.starts_with("members") && trimmed.contains('[')) {
                continue;
            }
            inside = true;
        }
        // Odd indices of a `"`-split are the quoted segments.
        members.extend(
            trimmed
                .split('"')
                .skip(1)
                .step_by(2)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string),
        );
        if trimmed.contains(']') {
            break;
        }
    }
    members
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Production dependency sections: `[dependencies]`,
/// `[dependencies.x]` (per-dependency dotted form), `[build-dependencies]`
/// and `[build-dependencies.x]`, and target-scoped variants such as
/// `[target.'cfg(unix)'.dependencies]`. `dev-dependencies` in any form is
/// excluded — test fixtures may depend on the satellites.
fn is_production_section(section: &str) -> bool {
    section == "dependencies"
        || section.starts_with("dependencies.")
        || section.contains(".dependencies")
        || section.starts_with("build-dependencies")
        || section.ends_with(".build-dependencies")
}
