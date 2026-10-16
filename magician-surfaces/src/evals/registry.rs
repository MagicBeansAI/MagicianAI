//! Eval lanes parsed out of `## eval:` annotations in the repo Makefile.
//!
//! # Why the Makefile is the source of truth
//!
//! The repo already has 31 eval targets whose reports are mutually
//! incompatible. A second, hand-kept manifest of them would drift the first
//! time someone added or renamed a target, and the drift would be invisible
//! until an eval quietly stopped showing up. Keeping the declaration on the
//! line above the recipe means a lane can only go wrong if the person editing
//! the recipe walks past the comment describing it.
//!
//! # Why `kind` and `requires` are declared rather than inferred
//!
//! The recipes do imply both: live lanes end in `-live-eval` and shell out to
//! `run-ollama`, harness lanes do not. Inference would be correct today and
//! would break silently on the first rename — and it would break in the
//! expensive direction, reporting a lane as ready, starting it, and letting the
//! run die halfway through after burning minutes of model time. A declaration
//! can be wrong too, but it is wrong *visibly*: it sits in the diff next to the
//! recipe it describes.
//!
//! # Why nothing is ever dropped
//!
//! Every failure mode here degrades to something the page can render: a bad
//! field becomes an [`EvalLane::parse_error`], a `kind` nobody declared becomes
//! [`EvalKind::Unknown`] (which has no runnable representation at all), and an
//! annotation that never reached a rule — including one misspelled as
//! `##eval:` or parked in trailing `##` help text — becomes an
//! [`OrphanedAnnotation`]. A lane missing from the grid must never be
//! indistinguishable from a lane that does not exist, because that is the one
//! failure nobody can see.
//!
//! # Why this module is pure
//!
//! Text in, lanes out, no file I/O. This is the piece most likely to rot as the
//! Makefile changes, so it has to be cheap to table-test against pathological
//! input rather than requiring a fixture tree on disk.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Declares a lane: `## eval: kind=live requires=ollama,magician report=evals/x`.
const EVAL_ANNOTATION_PREFIX: &str = "## eval:";
/// Optional human label, on a line between the annotation and the rule.
const DESC_ANNOTATION_PREFIX: &str = "## desc:";

/// How a lane runs, which decides what the page may promise about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    /// Self-contained: no external service, safe to launch unattended.
    Harness,
    /// Talks to a real model or a real running service; slow and costly.
    Live,
    /// The annotation did not successfully declare a `kind`.
    ///
    /// Every way of failing lands here — a misspelled value (`kind=liv`), a
    /// misspelled key (`KIND=live`, `kinds=live`), stray spaces
    /// (`kind = live`), or no `kind` at all. Defaulting any of those to
    /// `Harness` would put the mistake in the *dangerous* direction, since
    /// `Harness` is the kind the page may launch unattended, and it would only
    /// be visible to a consumer that remembered to read
    /// [`EvalLane::parse_error`] first. An annotation is already opt-in, so
    /// requiring one field of it costs nothing.
    Unknown,
}

impl EvalKind {
    /// The kind as something that may actually be started, or `None` when
    /// nobody successfully declared what this lane is.
    ///
    /// This returns a type rather than a `bool` so the guarantee survives
    /// contact with a future run entrypoint: `match kind { Live => gate(), _ =>
    /// launch() }` re-creates the bug and compiles fine, whereas a function
    /// taking [`RunnableKind`] cannot be handed an `Unknown` at all.
    pub const fn runnable(self) -> Option<RunnableKind> {
        match self {
            Self::Harness => Some(RunnableKind::Harness),
            Self::Live => Some(RunnableKind::Live),
            Self::Unknown => None,
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "harness" => Some(Self::Harness),
            "live" => Some(Self::Live),
            _ => None,
        }
    }
}

/// The kinds a lane can actually be started as.
///
/// There is deliberately no `Unknown` here. Whatever eventually launches a lane
/// should take this type, so that "a lane nobody understood can never start"
/// is a fact about the signature rather than a rule someone has to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnableKind {
    Harness,
    Live,
}

/// A precondition a lane needs before it is worth starting.
///
/// # Why there are three magician-shaped variants
///
/// A single `magician` token was the first thing annotating the real Makefile
/// disproved. Three of the lanes it covered need three unrelated things: a
/// server answering HTTP, an executable sitting on disk, and a *different*
/// service entirely. One token cannot be probed — whichever check it ran would
/// report at least one of those lanes wrongly, and reporting a blocked lane as
/// runnable is how an expensive run dies halfway through. Splitting them means
/// each variant maps to exactly one probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalRequirement {
    /// A reachable Ollama daemon — the lane drives a resident local model.
    /// Satisfied by an HTTP endpoint, not by having ollama installed.
    Ollama,
    /// A *running* magician backend answering on its HTTP API (the lanes that
    /// need this drive `$MAGICIAN_BASE_URL`, conventionally
    /// `http://127.0.0.1:3002`). Nothing about a build on disk implies it.
    Magician,
    /// The built `./$(MAGICIAN_BIN)` binary present and executable on disk. The
    /// lane shells out to it (`memory-index-prewarm`, `Makefile:996-1002`) and
    /// does not care whether a server is up — so a machine with no server
    /// running can still run these.
    MagicianBinary,
    /// A reachable Magicutor — the separate tool/browser runtime magician talks
    /// to over `execution.magicutor_base_url`. It has its own process, its own
    /// port and its own `/health`; a healthy magician says nothing about it.
    Magicutor,
    /// Real cloud provider credentials in the environment. This is the token
    /// that means the lane *spends money*, so it is also the one where a false
    /// "ready" is most expensive.
    ProviderKeys,
}

impl EvalRequirement {
    /// The tokens accepted in `requires=`, quoted for diagnostics. Kept beside
    /// the match so a new variant cannot be added without updating the message.
    const VALID_TOKENS: &'static str =
        "`ollama`, `magician`, `magician_binary`, `magicutor`, `provider_keys`";

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "ollama" => Some(Self::Ollama),
            "magician" => Some(Self::Magician),
            "magician_binary" => Some(Self::MagicianBinary),
            "magicutor" => Some(Self::Magicutor),
            "provider_keys" => Some(Self::ProviderKeys),
            _ => None,
        }
    }
}

/// One eval lane: a Makefile target the `/evals` page can describe and run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalLane {
    /// Stable identity for run records and URLs. The Makefile target name is
    /// already unique and slug-shaped, so it *is* the id; a second naming
    /// scheme would only be one more thing to keep in sync.
    pub id: String,
    /// The `make` target to invoke.
    pub target: String,
    pub kind: EvalKind,
    /// Preconditions, as a canonical set: sorted and de-duplicated, so that
    /// `ollama,magician` and `magician,ollama` describe the same lane.
    pub requires: Vec<EvalRequirement>,
    /// Repo-relative directory this lane writes its own (non-uniform) report
    /// into, for deep-linking out of the uniform run record.
    pub report_dir: Option<String>,
    pub desc: Option<String>,
    /// 1-based line of the `## eval:` annotation that declared this lane, so a
    /// diagnostic can say *where* to go and fix it.
    pub line: usize,
    /// Set when the annotation was malformed. The lane is returned anyway, so
    /// the defect shows up on the page instead of thinning the grid by one row.
    pub parse_error: Option<String>,
}

impl EvalLane {
    fn note_error(&mut self, message: String) {
        self.parse_error = Some(match self.parse_error.take() {
            Some(existing) => format!("{existing}; {message}"),
            None => message,
        });
    }
}

/// An annotation that never became a lane.
///
/// Without this the annotation would simply not exist in the output, which is
/// the one shape of failure the `/evals` page cannot draw. Task 2's contract
/// test asserts this list is empty against the real Makefile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanedAnnotation {
    /// 1-based line of the annotation that was left dangling.
    pub line: usize,
    /// The line as written, so the diagnostic can quote it back.
    pub text: String,
    /// What got in the way.
    pub reason: String,
}

/// Everything a Makefile declared: the lanes, and the annotations that failed
/// to become lanes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalRegistry {
    pub lanes: Vec<EvalLane>,
    pub orphaned: Vec<OrphanedAnnotation>,
}

/// Parses every `## eval:` annotation in a Makefile.
///
/// An annotation describes the next rule below it. Comments and Make's special
/// targets are skipped on the way there, because 6 of this Makefile's 31 eval
/// rules carry a prose block and/or a `.PHONY:` line directly above them
/// (`Makefile:2313-2331`), and the top of that block is where a contributor
/// naturally writes the annotation.
///
/// A *blank* line ends the search. The other 25 rules sit bare, and deleting a
/// rule together with its recipe always leaves the blank line behind, so this
/// is what stops an orphaned annotation drifting down onto an unrelated rule.
/// Anything else that is not a rule — `ifeq`, `include`, a bare `export`, an
/// assignment — ends the search too. Every ending that did not reach a rule is
/// reported as an [`OrphanedAnnotation`] rather than dropped.
pub fn parse_eval_registry(makefile: &str) -> EvalRegistry {
    let mut registry = EvalRegistry::default();
    // id -> index into `registry.lanes`, so a duplicate can annotate both ends.
    let mut first_declaration: HashMap<String, usize> = HashMap::new();
    let mut pending: Option<PendingAnnotation> = None;

    for (index, raw_line) in makefile.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim_end();
        let trimmed = line.trim_start();

        if let Some(fields) = trimmed.strip_prefix(EVAL_ANNOTATION_PREFIX) {
            let annotation = PendingAnnotation::parse(fields, line_number, trimmed);
            if let Some(superseded) = pending.replace(annotation) {
                registry.orphaned.push(superseded.into_orphan(format!(
                    "a second `## eval:` annotation on line {line_number} replaced it \
                     before any rule was reached"
                )));
            }
            continue;
        }
        // Deliberately does not `continue`: the line may still be a rule, as in
        // the trailing-help form `eval-x: ## eval: kind=harness`.
        if looks_like_eval_annotation(trimmed) {
            registry.orphaned.push(OrphanedAnnotation {
                line: line_number,
                text: trimmed.to_string(),
                reason: format!(
                    "looks like an eval annotation but does not begin with \
                     `{EVAL_ANNOTATION_PREFIX}` at the start of its own line"
                ),
            });
        }
        if let Some(desc) = trimmed.strip_prefix(DESC_ANNOTATION_PREFIX) {
            if let Some(annotation) = pending.as_mut() {
                let desc = desc.trim();
                if !desc.is_empty() {
                    annotation.desc = Some(desc.to_string());
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            if let Some(annotation) = pending.take() {
                registry.orphaned.push(annotation.into_orphan(format!(
                    "the blank line on line {line_number} ended the search before a \
                     rule was reached"
                )));
            }
            continue;
        }
        // A prose comment must not orphan the annotation: it is what this
        // Makefile puts between a lane's description and its rule.
        if trimmed.starts_with('#') {
            continue;
        }

        let Some(annotation) = pending.take() else {
            continue;
        };
        let Some(name) = rule_name(line) else {
            registry.orphaned.push(annotation.into_orphan(format!(
                "line {line_number} is not a single-target rule, so the annotation \
                 describes nothing"
            )));
            continue;
        };
        if name.starts_with('.') {
            // Make's special targets (`.PHONY`, `.SUFFIXES`, ...) are never eval
            // lanes, and this Makefile writes them both ways: directly above the
            // rule they declare, and in grouped blocks naming dozens of targets
            // at once (`Makefile:7-12`, `Makefile:2344`). The only reading that
            // holds for both is that a dot-target is neither the lane nor the
            // thing that ends the search for one.
            pending = Some(annotation);
            continue;
        }

        let mut lane = annotation.into_lane(name);
        if let Some(bad) = unusable_target_char(&lane.target) {
            lane.note_error(format!(
                "target `{}` contains `{bad}`, which cannot appear in a run id or a \
                 URL path (expected only letters, digits, `.`, `_` and `-`)",
                lane.target
            ));
        }
        match first_declaration.get(&lane.id) {
            Some(&first_index) => {
                let first_line = registry.lanes[first_index].line;
                let duplicate_line = lane.line;
                lane.note_error(format!(
                    "duplicate lane id `{}`, first declared at line {first_line}",
                    lane.id
                ));
                let first = &mut registry.lanes[first_index];
                first.note_error(format!(
                    "duplicate lane id `{}`, also declared at line {duplicate_line}",
                    first.id
                ));
            },
            None => {
                first_declaration.insert(lane.id.clone(), registry.lanes.len());
            },
        }
        registry.lanes.push(lane);
    }

    if let Some(annotation) = pending.take() {
        registry
            .orphaned
            .push(annotation.into_orphan("end of file reached before any rule".to_string()));
    }

    registry
}

/// An annotation that has been read but not yet matched to a rule.
struct PendingAnnotation {
    kind: Option<EvalKind>,
    /// Whether a `kind` key was present at all, so a misspelled *value* is not
    /// also reported as a missing field.
    kind_declared: bool,
    requires: Vec<EvalRequirement>,
    report_dir: Option<String>,
    desc: Option<String>,
    line: usize,
    text: String,
    errors: Vec<String>,
}

impl PendingAnnotation {
    /// Parses the `k=v k=v` tail of a `## eval:` line.
    ///
    /// `requires` and `report` are optional and default to the conservative
    /// reading — no preconditions, no report — because absence there promises
    /// nothing. `kind` is required: absence there would promise something.
    fn parse(fields: &str, line: usize, text: &str) -> Self {
        let mut annotation = Self {
            kind: None,
            kind_declared: false,
            requires: Vec::new(),
            report_dir: None,
            desc: None,
            line,
            text: text.to_string(),
            errors: Vec::new(),
        };

        for field in fields.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                // Almost always `requires=a, b` or `kind = live`: the space split
                // one field into two, and the lane quietly ended up declaring
                // less than its author wrote.
                annotation.errors.push(format!(
                    "`{field}` is not a `key=value` field (fields and list values \
                     such as `requires=a,b` must not contain spaces)"
                ));
                continue;
            };
            match key {
                "kind" => {
                    annotation.kind_declared = true;
                    match EvalKind::from_token(value) {
                        Some(kind) => annotation.kind = Some(kind),
                        None => {
                            annotation.kind = None;
                            annotation.errors.push(format!(
                                "unknown kind `{value}` (expected `harness` or `live`)"
                            ));
                        },
                    }
                },
                "requires" => {
                    for token in value.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                        match EvalRequirement::from_token(token) {
                            Some(requirement) => annotation.requires.push(requirement),
                            None => annotation.errors.push(format!(
                                "unknown requirement `{token}` (expected {})",
                                EvalRequirement::VALID_TOKENS
                            )),
                        }
                    }
                },
                "report" => match validate_report_dir(value) {
                    Ok(()) => annotation.report_dir = Some(value.to_string()),
                    Err(problem) => annotation.errors.push(problem),
                },
                _ => annotation.errors.push(format!(
                    "unknown field `{key}` (expected `kind`, `requires` or `report`)"
                )),
            }
        }

        if annotation.kind.is_none() && !annotation.kind_declared {
            annotation
                .errors
                .push("`kind` is required (expected `kind=harness` or `kind=live`)".to_string());
        }

        // Preconditions are a set, not a sequence: the declaration order carries
        // no meaning, so two spellings of the same set must compare equal.
        annotation.requires.sort_unstable();
        annotation.requires.dedup();
        annotation
    }

    fn into_lane(self, target: &str) -> EvalLane {
        EvalLane {
            id: target.to_string(),
            target: target.to_string(),
            kind: self.kind.unwrap_or(EvalKind::Unknown),
            requires: self.requires,
            report_dir: self.report_dir,
            desc: self.desc,
            line: self.line,
            parse_error: (!self.errors.is_empty()).then(|| self.errors.join("; ")),
        }
    }

    fn into_orphan(self, reason: String) -> OrphanedAnnotation {
        OrphanedAnnotation {
            line: self.line,
            text: self.text,
            reason,
        }
    }
}

/// Whether a line was *trying* to be an eval annotation without being one:
/// `##eval:`, `## Eval:`, `## eval :`, and the trailing form
/// `eval-x: ## eval: kind=harness`.
///
/// This Makefile already uses `##` for trailing help text (`Makefile:782`,
/// `:2579`, `:2593`), so that last spelling is one an author arrives at by
/// following the file's own convention — and without this check it would be
/// indistinguishable from never having written an annotation at all.
fn looks_like_eval_annotation(text: &str) -> bool {
    let Some(hashes) = text.find("##") else {
        return false;
    };
    let rest = text[hashes + 2..].trim_start();
    let Some(word) = rest.get(..4) else {
        return false;
    };
    if !word.eq_ignore_ascii_case("eval") {
        return false;
    }
    // `## evals live under coverage/` is prose, not a near miss.
    !rest[4..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A later task deep-links this path, so it must stay inside the repo — and
/// this parser is not `make`, so it cannot expand a variable reference.
fn validate_report_dir(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("empty `report` path".to_string());
    }
    if value.contains('$') {
        return Err(format!(
            "`report` path `{value}` contains `$`; this parser cannot expand make \
             variables, so write the literal repo-relative path"
        ));
    }
    if value.starts_with('/') || value.split('/').any(|segment| segment == "..") {
        return Err(format!(
            "`report` path `{value}` must be relative to the repo root and must not \
             escape it via `..`"
        ));
    }
    Ok(())
}

/// The first character of `name` that could not survive being a run-store key
/// or a URL path segment.
fn unusable_target_char(name: &str) -> Option<char> {
    name.chars()
        .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
}

/// Returns the single target a Makefile line introduces, if it introduces one.
///
/// Rules start at column 0 and recipes are TAB-indented, so indentation alone
/// rules most lines out. `VAR = a:b`, `VAR := x` and `export FOO ::= x` contain
/// a colon without being rules. A multi-target rule (`a b: dep`) is rejected
/// because there would be no way to say which target the annotation described.
///
/// Whether the name is *usable* as an id is a separate question — see
/// [`unusable_target_char`]. A rule with an awkward name still becomes a lane
/// carrying a `parse_error`; only a line that is not a rule at all returns
/// `None`.
fn rule_name(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t']) || line.starts_with('#') {
        return None;
    }
    let (head, tail) = line.split_once(':')?;
    if tail.starts_with('=') || tail.starts_with(":=") || head.contains('=') {
        return None;
    }
    let name = head.trim();
    if name.is_empty() || name.split_whitespace().count() > 1 {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
## eval: kind=live requires=ollama,magician_binary report=evals/memory-temperature
## desc: Gate real/synthetic memory recall
test-memory-temperature-live-eval:
\t@echo hi

## eval: kind=harness
eval-monitor-golden:
\t@echo hi

not-an-eval:
\t@echo hi
";

    /// Every lane-focused test goes through here. A regression that started
    /// spuriously orphaning annotations must not be able to hide behind a test
    /// that only ever looks at `lanes` — that is the exact failure `orphaned`
    /// exists to make visible.
    fn lanes_of(makefile: &str) -> Vec<EvalLane> {
        let registry = parse_eval_registry(makefile);
        assert!(
            registry.orphaned.is_empty(),
            "unexpected orphans: {:?}",
            registry.orphaned
        );
        registry.lanes
    }

    #[test]
    fn parses_annotated_lanes_only() {
        let lanes = lanes_of(SAMPLE);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].target, "test-memory-temperature-live-eval");
        assert_eq!(lanes[1].target, "eval-monitor-golden");
    }

    #[test]
    fn parses_kind_requires_report_and_desc() {
        let lanes = lanes_of(SAMPLE);
        let lane = &lanes[0];
        assert_eq!(lane.kind, EvalKind::Live);
        assert_eq!(
            lane.requires,
            vec![EvalRequirement::Ollama, EvalRequirement::MagicianBinary]
        );
        assert_eq!(lane.report_dir.as_deref(), Some("evals/memory-temperature"));
        assert_eq!(
            lane.desc.as_deref(),
            Some("Gate real/synthetic memory recall")
        );
        assert_eq!(lane.parse_error, None);
    }

    #[test]
    fn optional_fields_default_conservatively() {
        let lanes = lanes_of(SAMPLE);
        let lane = &lanes[1];
        assert_eq!(lane.kind, EvalKind::Harness);
        assert!(lane.requires.is_empty());
        assert!(lane.report_dir.is_none());
        assert_eq!(lane.parse_error, None);
    }

    #[test]
    fn malformed_annotation_yields_an_unparseable_lane_not_a_missing_one() {
        let lanes = lanes_of("## eval: kind=nonsense\nsome-eval-target:\n\t@echo hi\n");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].target, "some-eval-target");
        assert!(
            lanes[0].parse_error.is_some(),
            "bad kind must be reported, not dropped"
        );
    }

    /// A typo in the *value* must not leave the lane claiming the safer kind.
    #[test]
    fn an_unparseable_kind_never_reads_as_harness() {
        let lanes = lanes_of("## eval: kind=liv\nsome-eval:\n\t@echo hi\n");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].kind, EvalKind::Unknown);
        assert!(lanes[0].kind.runnable().is_none());
        let error = lanes[0].parse_error.as_deref().unwrap();
        assert!(
            error.contains("live"),
            "error should name the valid kinds: {error}"
        );
    }

    /// Every way of failing to declare a kind, not just a bad value. A lane
    /// whose author forgot `kind=` must not publish as "safe to launch
    /// unattended" with a clean `parse_error`.
    #[test]
    fn kind_is_required_not_defaulted() {
        for annotation in [
            "## eval: requires=ollama report=evals/x",
            "## eval: KIND=live requires=ollama",
            "## eval: kinds=live",
            "## eval: kind = live",
        ] {
            let src = format!("{annotation}\nsome-eval:\n\t@echo hi\n");
            let lanes = lanes_of(&src);
            assert_eq!(lanes.len(), 1, "{annotation}");
            assert_eq!(lanes[0].kind, EvalKind::Unknown, "{annotation}");
            assert!(lanes[0].kind.runnable().is_none(), "{annotation}");
            let error = lanes[0].parse_error.as_deref().unwrap_or("");
            assert!(error.contains("`kind`"), "{annotation}: {error}");
        }
    }

    /// The guarantee is structural, not advisory: a run entrypoint taking
    /// `RunnableKind` cannot be handed a lane nobody understood.
    #[test]
    fn unknown_kind_has_no_runnable_representation() {
        assert_eq!(EvalKind::Harness.runnable(), Some(RunnableKind::Harness));
        assert_eq!(EvalKind::Live.runnable(), Some(RunnableKind::Live));
        assert_eq!(EvalKind::Unknown.runnable(), None);
    }

    /// Six of the 31 eval rules have a prose block and/or a `.PHONY:` line
    /// directly above them (`Makefile:2313-2331`). If a comment orphaned the
    /// annotation, the natural place to write `## eval:` would yield no lane and
    /// no diagnostic — the exact silent vanish this module exists to prevent.
    #[test]
    fn a_prose_comment_block_between_the_annotation_and_the_rule_keeps_the_lane() {
        let src = "\
## eval: kind=harness report=evals/monitor
## desc: Golden monitor traces
# Provider-free golden eval for the Recurring Monitors change ledger.
# No LLM, no server.
.PHONY: eval-monitor-golden
eval-monitor-golden:
\t@echo hi
";
        let lanes = lanes_of(src);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].target, "eval-monitor-golden");
        assert_eq!(lanes[0].parse_error, None);
        assert_eq!(lanes[0].desc.as_deref(), Some("Golden monitor traces"));
    }

    #[test]
    fn annotation_not_followed_by_a_target_is_reported_as_orphaned() {
        let registry = parse_eval_registry("## eval: kind=live\n\n# just a comment\n");
        assert!(registry.lanes.is_empty());
        assert_eq!(registry.orphaned.len(), 1);
        assert_eq!(registry.orphaned[0].line, 1);
        assert_eq!(registry.orphaned[0].text, "## eval: kind=live");
        assert!(!registry.orphaned[0].reason.is_empty());
    }

    /// Deleting a rule and its recipe always leaves the blank line behind, so a
    /// blank line is what stops an orphaned annotation drifting onto whatever
    /// rule happens to come next.
    #[test]
    fn a_blank_line_ends_the_search_for_a_rule() {
        let registry =
            parse_eval_registry("## eval: kind=harness\n\nsome-other-eval:\n\t@echo hi\n");
        assert!(registry.lanes.is_empty());
        assert_eq!(registry.orphaned.len(), 1);
        assert_eq!(registry.orphaned[0].line, 1);
        assert!(
            registry.orphaned[0].reason.contains("blank"),
            "{}",
            registry.orphaned[0].reason
        );
    }

    /// `ifeq`, `include` and bare `export FOO` (`Makefile:358`) carry no colon,
    /// so they end the search for a rule. The annotation must still be visible.
    #[test]
    fn an_annotation_above_a_conditional_is_orphaned_not_dropped() {
        let src = "## eval: kind=live\nifeq ($(OS),Darwin)\ntest-x-live-eval:\n\t@echo hi\nendif\n";
        let registry = parse_eval_registry(src);
        assert!(registry.lanes.is_empty());
        assert_eq!(registry.orphaned.len(), 1);
        assert_eq!(registry.orphaned[0].line, 1);
    }

    #[test]
    fn a_second_annotation_orphans_the_first() {
        let src = "## eval: kind=live\n## eval: kind=harness\nsome-eval:\n\t@echo hi\n";
        let registry = parse_eval_registry(src);
        assert_eq!(registry.lanes.len(), 1);
        assert_eq!(registry.lanes[0].kind, EvalKind::Harness);
        assert_eq!(registry.orphaned.len(), 1);
        assert_eq!(registry.orphaned[0].line, 1);
    }

    /// This Makefile already uses `##` for trailing help text (`Makefile:782`,
    /// `:2579`, `:2593`), so an author following the file's own convention
    /// writes the annotation where it would otherwise be invisible.
    #[test]
    fn a_trailing_annotation_on_a_rule_line_is_reported() {
        let registry =
            parse_eval_registry("eval-monitor-golden: ## eval: kind=harness\n\t@echo hi\n");
        assert!(registry.lanes.is_empty());
        assert_eq!(registry.orphaned.len(), 1);
        assert_eq!(registry.orphaned[0].line, 1);
    }

    #[test]
    fn a_near_miss_annotation_is_reported_instead_of_ignored() {
        for text in [
            "##eval: kind=harness",
            "## Eval: kind=harness",
            "## eval : kind=harness",
        ] {
            let src = format!("{text}\nsome-eval:\n\t@echo hi\n");
            let registry = parse_eval_registry(&src);
            assert!(registry.lanes.is_empty(), "{text}");
            assert_eq!(registry.orphaned.len(), 1, "{text}");
            assert_eq!(registry.orphaned[0].line, 1, "{text}");
            assert_eq!(registry.orphaned[0].text, text, "{text}");
        }
    }

    /// A comment that merely mentions evals is not a near miss.
    #[test]
    fn ordinary_comments_are_not_near_misses() {
        for text in [
            "## evals live under coverage/",
            "## eval-page notes",
            "test-container: build-container  ## Run container integration tests",
        ] {
            let src = format!("{text}\nsome-target:\n\t@echo hi\n");
            let registry = parse_eval_registry(&src);
            assert!(
                registry.orphaned.is_empty(),
                "{text}: {:?}",
                registry.orphaned
            );
        }
    }

    #[test]
    fn lanes_carry_the_line_of_their_annotation() {
        let lanes = lanes_of(SAMPLE);
        assert_eq!(lanes[0].line, 1);
        assert_eq!(lanes[1].line, 6);
    }

    /// The id becomes a URL path segment and a run-store key.
    #[test]
    fn a_target_name_that_cannot_be_a_run_id_is_reported() {
        let lanes = lanes_of("## eval: kind=harness\n$(EVAL_DIR)/report:\n\t@echo hi\n");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].target, "$(EVAL_DIR)/report");
        let error = lanes[0].parse_error.as_deref().unwrap();
        assert!(
            error.contains('$'),
            "error should name the bad character: {error}"
        );
    }

    /// `requires=ollama, magician` silently declared *fewer* preconditions,
    /// because the space split it into two fields.
    #[test]
    fn a_space_inside_a_list_value_is_diagnosed_as_such() {
        let lanes =
            lanes_of("## eval: kind=harness requires=ollama, magician\nsome-eval:\n\t@echo hi\n");
        assert_eq!(lanes[0].requires, vec![EvalRequirement::Ollama]);
        let error = lanes[0].parse_error.as_deref().unwrap();
        assert!(error.contains("space"), "{error}");
    }

    /// The three magician-shaped requirements are separate tokens, because they
    /// are separate checks. Before this existed, `test-memory-temperature-live-eval`
    /// (needs the binary, `Makefile:996-1002`) and
    /// `test-content-retrieval-runtime-live-eval` (needs Magicutor) both said
    /// `magician`, so any probe of "is the server up" reported at least one of
    /// them wrongly.
    #[test]
    fn the_three_magician_shaped_requirements_are_distinct_tokens() {
        let lanes = lanes_of(
            "## eval: kind=live requires=magician,magician_binary,magicutor\nsome-eval:\n\t@echo hi\n",
        );
        assert_eq!(
            lanes[0].requires,
            vec![
                EvalRequirement::Magician,
                EvalRequirement::MagicianBinary,
                EvalRequirement::Magicutor,
            ]
        );
        assert_eq!(lanes[0].parse_error, None);
    }

    /// A near-miss spelling must not silently declare a *different*, weaker
    /// precondition: `magician-binary` is not `magician`.
    #[test]
    fn a_misspelled_requirement_is_reported_and_names_every_valid_token() {
        let lanes =
            lanes_of("## eval: kind=live requires=magician-binary\nsome-eval:\n\t@echo hi\n");
        assert!(lanes[0].requires.is_empty());
        let error = lanes[0].parse_error.as_deref().unwrap();
        for token in ["magician_binary", "magicutor", "provider_keys"] {
            assert!(error.contains(token), "{token} missing from: {error}");
        }
    }

    /// Preconditions are a set: two spellings of the same set must compare equal.
    #[test]
    fn requires_is_a_canonical_set() {
        let lanes = lanes_of(
            "## eval: kind=live requires=magician,ollama,ollama\nsome-eval:\n\t@echo hi\n",
        );
        assert_eq!(
            lanes[0].requires,
            vec![EvalRequirement::Ollama, EvalRequirement::Magician]
        );
        assert_eq!(lanes[0].parse_error, None);
    }

    #[test]
    fn report_paths_must_stay_inside_the_repo() {
        for bad in ["report=/etc/passwd", "report=../../secrets"] {
            let src = format!("## eval: kind=harness {bad}\nsome-eval:\n\t@echo hi\n");
            let lanes = lanes_of(&src);
            assert_eq!(lanes[0].report_dir, None, "{bad}");
            let error = lanes[0].parse_error.as_deref().unwrap();
            assert!(error.contains("relative"), "{bad}: {error}");
        }
    }

    /// `$(COVERAGE_BASE_DIR)/evals/...` is how every report dir in this Makefile
    /// is written (`Makefile:414-447`), so it is the likeliest thing an author
    /// copies in — and this parser cannot expand it.
    #[test]
    fn report_paths_must_not_contain_make_variables() {
        let src =
            "## eval: kind=harness report=$(COVERAGE_BASE_DIR)/evals/x\nsome-eval:\n\t@echo hi\n";
        let lanes = lanes_of(src);
        assert_eq!(lanes[0].report_dir, None);
        let error = lanes[0].parse_error.as_deref().unwrap();
        assert!(error.contains('$'), "{error}");
    }

    /// The repo Makefile writes `.PHONY: eval-monitor-golden` on the line
    /// directly above `eval-monitor-golden:`. A naive "next line with a colon"
    /// rule would register a lane called `.PHONY`.
    #[test]
    fn a_phony_declaration_does_not_steal_the_annotation() {
        let lanes = lanes_of(
            "## eval: kind=harness\n.PHONY: eval-monitor-golden\neval-monitor-golden:\n\t@echo hi\n",
        );
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].target, "eval-monitor-golden");
    }

    /// Both lanes share an id, so both will collide in run records and deep
    /// links; blaming only the second hides half the problem.
    #[test]
    fn duplicate_lane_ids_are_reported_on_both_lanes() {
        let lanes = lanes_of(
            "## eval: kind=harness\ndup-eval:\n\t@echo a\n## eval: kind=harness\ndup-eval:\n\t@echo b\n",
        );
        assert_eq!(lanes.len(), 2);
        let first = lanes[0].parse_error.as_deref().unwrap();
        let second = lanes[1].parse_error.as_deref().unwrap();
        assert!(
            first.contains("duplicate") && first.contains("line 4"),
            "first should point at the later declaration: {first}"
        );
        assert!(
            second.contains("duplicate") && second.contains("line 1"),
            "second should point at the first declaration: {second}"
        );
    }

    #[test]
    fn rule_name_recognises_only_single_target_rules() {
        let cases: [(&str, Option<&str>); 8] = [
            ("VAR := x", None),
            ("VAR = a:b", None),
            ("export FOO := b", None),
            ("export CARGO_TARGET_DIR", None),
            ("a b: dep", None),
            ("\ttab-indented: x", None),
            ("target:: dep", Some("target")),
            ("my-eval: VAR = x", Some("my-eval")),
        ];
        for (line, expected) in cases {
            assert_eq!(rule_name(line), expected, "{line}");
        }
    }
}
