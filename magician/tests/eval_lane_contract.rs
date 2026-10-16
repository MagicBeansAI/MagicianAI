//! Contract: every `## eval:` annotation in the repo Makefile resolves to a real
//! target and parses cleanly, **and** every target that writes an eval report
//! carries an annotation. The first direction catches a target rename in CI
//! instead of at the moment someone needs that eval; the second catches a whole
//! eval lane that was never declared and is therefore invisible to `/evals`.

use std::collections::{BTreeMap, BTreeSet};

use magician_surfaces::evals::registry::parse_eval_registry;

/// Reads the repo Makefile, or fails the test.
///
/// Deliberately `.expect(...)` rather than `if let Ok(..)`: a test that silently
/// skips when the file is unreadable can never fail, which is exactly the
/// vanishing-lane failure the whole registry module exists to prevent. A moved
/// Makefile or a changed manifest layout must break loudly here.
fn makefile() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../Makefile");
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("repo Makefile at `{path}` is readable: {error}"))
}

#[test]
fn every_annotated_lane_parses_without_error() {
    let src = makefile();
    let registry = parse_eval_registry(&src);
    let bad: Vec<String> = registry
        .lanes
        .iter()
        .filter_map(|lane| {
            lane.parse_error
                .as_deref()
                .map(|error| format!("Makefile:{} {}: {error}", lane.line, lane.target))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "malformed eval annotations:\n{}",
        bad.join("\n")
    );
}

/// THE most important assertion in this file. `parses_without_error` only
/// inspects lanes that EXIST, so it cannot see a lane that vanished — and an
/// annotation placed above a target's prose comment block, or above a grouped
/// `.PHONY`, is exactly how one vanishes. Without this, annotating 31 targets
/// and silently losing 20 still passes the whole suite.
#[test]
fn no_annotation_is_orphaned() {
    let orphans = parse_eval_registry(&makefile()).orphaned;
    assert!(
        orphans.is_empty(),
        "annotations that matched no target — check placement; an annotation must sit \
         directly above its own target or that target's `.PHONY` line, with no blank \
         line in between:\n{orphans:#?}"
    );
}

/// A lane whose target was renamed still parses perfectly — the annotation is
/// well-formed, it just describes a rule that no longer exists under that name.
/// Only re-reading the file for the target catches that.
#[test]
fn every_annotated_lane_names_a_real_target() {
    let src = makefile();
    for lane in parse_eval_registry(&src).lanes {
        assert!(
            src.contains(&format!("\n{}:", lane.target)),
            "lane `{}` (Makefile:{}) does not resolve to a Makefile target",
            lane.target,
            lane.line
        );
    }
}

/// Guards the annotation rollout: if this drops, someone deleted annotations.
/// Deliberately a floor rather than an exact count, so adding an eval target is
/// never blocked by a test that has to be edited in the same commit.
#[test]
fn the_repo_declares_a_meaningful_number_of_lanes() {
    let lanes = parse_eval_registry(&makefile()).lanes;
    assert!(
        lanes.len() >= 35,
        "expected at least 35 annotated eval lanes, found {}",
        lanes.len()
    );
}

// ---------------------------------------------------------------------------
// The inverse contract: target -> annotation.
// ---------------------------------------------------------------------------

/// Shared by the `benchmark-media-*` twins, which are all the same shape.
///
/// They are named separately rather than matched by prefix so that deleting or
/// renaming one trips [`the_unannotated_allowlist_has_no_stale_entries`]. A
/// `starts_with("benchmark-media-")` rule would also silently absolve a *new*
/// gating lane that happened to be named that way.
const UNGATED_MEDIA_BENCHMARK: &str =
    "ungated measurement run over the offline audio suite: it has no gating \
     thresholds and always exits 0, so a lane for it would read `Passed` forever \
     regardless of what the code did. It also writes to \
     `data/magician_v2/media_evals/results/` rather than under `evals/`, so the \
     detection below does not surface it today — this entry records the decision \
     for whoever widens detection to literal report paths.";

/// Targets that write into an `evals/` report directory and are deliberately
/// *not* lanes.
///
/// The reasons are the point of this list. Whoever trips
/// [`every_target_writing_an_eval_report_is_annotated`] has to decide which side
/// of the line their target is on — annotate it, or add it here — and that is
/// not a decision anyone can make from a bare list of names.
const ALLOWED_UNANNOTATED: &[(&str, &str)] = &[
    (
        "test",
        "aggregate suite, not a lane: it only exports the report variables into \
         `run-all-tests-with-report.sh`, and the children that shell script spawns \
         are the things that write reports. It runs lanes; it is not one.",
    ),
    (
        "test-verbose",
        "the same aggregate suite as `test` with output unsuppressed — a \
         pass-through for the report variables, not a lane.",
    ),
    (
        "benchmark-chat-context-retrieval",
        "measurement run, not a gate: it runs the same example as the annotated \
         `test-chat-context-retrieval-live-eval` but without any of the gating \
         flags (`--require-hybrid`, `--require-coalescing`, \
         `--max-concurrent-wall-p*-ms`), so it reports timings and always exits 0. \
         A lane for it would read `Passed` forever — a green mark nothing earned. \
         The lane that actually gates this code is \
         `test-chat-context-retrieval-live-eval`, and that one is annotated.",
    ),
    ("benchmark-media-recording-stt", UNGATED_MEDIA_BENCHMARK),
    ("benchmark-media-offline-audio", UNGATED_MEDIA_BENCHMARK),
    ("benchmark-media-vad", UNGATED_MEDIA_BENCHMARK),
    ("benchmark-media-stt", UNGATED_MEDIA_BENCHMARK),
    ("benchmark-media-tts", UNGATED_MEDIA_BENCHMARK),
    ("benchmark-media-diarization", UNGATED_MEDIA_BENCHMARK),
];

/// THE inverse of [`no_annotation_is_orphaned`]. That one proves every
/// annotation found a target; this one proves every target found an annotation.
///
/// Without it, adding an eval lane and forgetting the annotation is completely
/// silent: the lane simply never appears on `/evals`, and no test anywhere in the
/// repo has an opinion about it. That is how seven real lanes
/// (`llm-trace-phase{0,1,2f,3,4}-*`, `ollama-chunking-readiness`,
/// `test-realtime-local-transcript-live`) sat unannotated.
///
/// # Why detection is by report variable rather than by name
///
/// The scoping pass that missed those seven matched `*eval*` in the *target
/// name*. Name matching is wrong in both directions here: it returns `audit`
/// (cargo audit) and `graph-audit` (codegraph) as evals, and it misses every lane
/// whose name says `trace`, `chunking` or `transcript`. Writing a report under
/// `evals/` is the honest test, because that is the thing the `/evals` page
/// deep-links — so the rule is: find the variables whose value points into
/// `evals/`, then find the recipes that expand them.
#[test]
fn every_target_writing_an_eval_report_is_annotated() {
    let src = makefile();
    let annotated: BTreeSet<String> = parse_eval_registry(&src)
        .lanes
        .into_iter()
        .map(|lane| lane.target)
        .collect();
    let allowed: BTreeSet<&str> = ALLOWED_UNANNOTATED.iter().map(|(name, _)| *name).collect();

    let variables = eval_report_variables(&src);
    assert!(
        !variables.is_empty(),
        "found no Makefile variable pointing into an `evals/` directory — the \
         detection rule itself has stopped working, so this test would pass \
         vacuously forever"
    );

    let missing: Vec<String> = targets_writing_eval_reports(&src, &variables)
        .into_iter()
        .filter(|(target, _)| !annotated.contains(target) && !allowed.contains(target.as_str()))
        .map(|(target, vars)| {
            format!(
                "  {target}  (writes {})",
                vars.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect();

    assert!(
        missing.is_empty(),
        "these Makefile targets write an eval report but carry no `## eval:` \
         annotation, so `/evals` cannot see them at all:\n{}\n\n\
         Pick one:\n\
         (a) annotate the target — put `## eval: kind=… [requires=…] [report=…]` \
         directly above the rule, or above that rule's own `.PHONY:` line, with NO \
         blank line in between (a blank line orphans it silently; see \
         docs/components/magician/eval-lanes.md); or\n\
         (b) if it is deliberately not a lane — an aggregate suite, or an ungated \
         measurement run that always exits 0 — add it to `ALLOWED_UNANNOTATED` in \
         this file together with the reason, so the next person does not have to \
         re-derive your decision.",
        missing.join("\n")
    );
}

/// An allowlist that is never re-checked rots: a target gets renamed, its entry
/// stops matching anything, and the list quietly grows a line that excuses
/// nothing while looking like it excuses something.
#[test]
fn the_unannotated_allowlist_has_no_stale_entries() {
    let src = makefile();
    let targets = makefile_target_names(&src);
    let stale: Vec<&str> = ALLOWED_UNANNOTATED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !targets.contains(*name))
        .collect();
    assert!(
        stale.is_empty(),
        "`ALLOWED_UNANNOTATED` names targets that no longer exist in the Makefile: \
         {stale:?}. They were renamed or deleted — remove the entries, or point \
         them at the new names."
    );
}

/// Makefile variables whose value names a path under `evals/`.
fn eval_report_variables(src: &str) -> BTreeSet<String> {
    src.lines()
        .filter_map(assignment)
        .filter(|(_, value)| value.contains("evals/"))
        .map(|(name, _)| name.to_string())
        .collect()
}

/// Every target whose recipe expands one of `variables`, with the ones it used.
///
/// Recipes are TAB-indented and rules start at column 0, which is the whole
/// parse. Blank lines and comments deliberately do *not* end a recipe here —
/// unlike in the annotation parser, where a blank line ending the search is what
/// stops an orphan drifting onto an unrelated rule. Attribution has the opposite
/// risk profile: losing the rest of a recipe would drop a target from this list,
/// and a target missing from this list is a lane this test stops guarding.
fn targets_writing_eval_reports(
    src: &str,
    variables: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let expansions: Vec<(String, &String)> = variables
        .iter()
        .map(|name| (format!("$({name})"), name))
        .collect();
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current: Vec<String> = Vec::new();

    for line in src.lines() {
        if line.starts_with('\t') {
            for (expansion, name) in &expansions {
                if line.contains(expansion.as_str()) {
                    for target in &current {
                        found
                            .entry(target.clone())
                            .or_default()
                            .insert((*name).clone());
                    }
                }
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Anything that is not a rule ends the recipe; a dot-target (`.PHONY:`)
        // is a rule that owns no recipe, so it also leaves nothing current.
        current = rule_targets(line)
            .unwrap_or_default()
            .into_iter()
            .filter(|name| !name.starts_with('.'))
            .collect();
    }
    found
}

/// Every ordinary target the Makefile defines, for the staleness check.
fn makefile_target_names(src: &str) -> BTreeSet<String> {
    src.lines()
        .filter_map(rule_targets)
        .flatten()
        .filter(|name| !name.starts_with('.'))
        .collect()
}

/// The targets a rule line introduces, if it introduces any.
///
/// Multi-target rules are kept whole here — unlike in the registry parser, which
/// rejects them because an annotation above one could not say which target it
/// described. This side has no such ambiguity: a shared recipe writes a report
/// under every name it was reached by, so every one of those names needs its own
/// annotation.
fn rule_targets(line: &str) -> Option<Vec<String>> {
    if line.starts_with([' ', '\t', '#']) {
        return None;
    }
    let (head, tail) = line.split_once(':')?;
    // `VAR := x`, `VAR ?= a:b`, `export FOO ::= x` — a colon without a rule.
    if tail.starts_with('=') || tail.starts_with(":=") || head.contains('=') {
        return None;
    }
    let names: Vec<String> = head.split_whitespace().map(str::to_string).collect();
    (!names.is_empty()).then_some(names)
}

/// `NAME = v`, `NAME ?= v`, `NAME := v`, `export NAME ::= v` at column 0.
fn assignment(line: &str) -> Option<(&str, &str)> {
    if line.starts_with([' ', '\t', '#']) {
        return None;
    }
    let rest = line.strip_prefix("export ").unwrap_or(line);
    let (head, value) = rest.split_once('=')?;
    let name = head.trim_end_matches([':', '?', '+']).trim();
    let named = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    named.then_some((name, value.trim()))
}
