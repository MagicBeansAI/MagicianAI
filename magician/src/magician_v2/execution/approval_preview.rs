//! Structured, deterministic previews for approval-gated tool calls.
//!
//! A `requires_approval` pause used to show the owner the raw call — for a
//! tool whose argument is a whole document, the whole document on one line.
//! A compiled tool may instead register an **approval-preview builder**: a
//! pure function of the call's arguments (plus read-only scope context) that
//! returns an [`ApprovalPreview`] — a title, the agent's stated reason
//! (labelled as the agent's words), an optional line diff, evidence and
//! warnings. No LLM is involved; the diff is the ground truth and the
//! rationale is presented as plain text.
//!
//! The preview is computed once, when the pause is created
//! (`build_action_confirmation_outcome_with`), and travels as data:
//! `AgenticPauseState::approval_preview` (the pause record),
//! `PendingPauseInfo::confirmation_preview` (the pending-pause listing) and
//! `hitl.requested` `input_schema.approval_preview` (every attention/feed
//! surface). Every field is optional on the wire, so a client that does not
//! know it keeps rendering the raw call it always did; the raw call stays in
//! `input_schema.action_summary` / `reason` for auditing.
//!
//! Registering a builder is one row in [`APPROVAL_PREVIEW_BUILDERS`]; the
//! pause path never names a tool.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::actions::ExecutableAction;

/// Lines of unchanged context shown around each change.
pub const APPROVAL_PREVIEW_CONTEXT_LINES: usize = 2;
/// Most diff lines (context included) a preview carries.
pub const APPROVAL_PREVIEW_MAX_DIFF_LINES: usize = 200;
/// Most diff text (sum of shown line bytes) a preview carries.
pub const APPROVAL_PREVIEW_MAX_DIFF_BYTES: usize = 16 * 1024;
/// One very long line (a pasted blob without newlines) is cut to this.
pub const APPROVAL_PREVIEW_MAX_LINE_BYTES: usize = 1024;
/// A diff that cannot be computed in this long degrades to a coarser one
/// (`similar`'s deadline) rather than stalling the pause.
const DIFF_DEADLINE: Duration = Duration::from_millis(500);

/// What the owner sees on an approval card in place of the raw call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPreview {
    /// One line naming who wants to do what, e.g. "Mira wants to change
    /// `positioning.md`". Plain text.
    pub title: String,
    /// Label for `why`, e.g. "Why (agent's words)". Present whenever `why` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_label: Option<String>,
    /// The agent's own stated reason. Agent-authored plain text: render it as
    /// text, never as markup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The change itself, when the act edits text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<ApprovalPreviewDiff>,
    /// Links or notes the agent offered in support. Agent-authored text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Facts the owner must see before deciding (a stale base, a missing
    /// target). Rendered prominently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ApprovalPreviewWarning>,
    /// Neutral facts about the preview (e.g. "no changes").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPreviewWarning {
    /// Stable machine code (`stale_base`, `missing_target`, `invalid_target`,
    /// `invalid_base`).
    pub code: String,
    pub message: String,
}

impl ApprovalPreviewWarning {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// A capped line diff of one text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPreviewDiff {
    pub path: String,
    /// Total added lines in the whole change (not only the shown hunks).
    pub added: usize,
    /// Total removed lines in the whole change.
    pub removed: usize,
    /// The shown hunks, with up to [`APPROVAL_PREVIEW_CONTEXT_LINES`] of
    /// context around each change.
    #[serde(default)]
    pub hunks: Vec<ApprovalPreviewHunk>,
    /// The shown hunks were cut at the line/byte cap.
    #[serde(default)]
    pub truncated: bool,
    /// Changed (added + removed) lines past the cap, for "… N more changed
    /// lines".
    #[serde(default)]
    pub omitted_changed_lines: usize,
    /// The shown hunks as a standard unified diff (`--- a/…`, `+++ b/…`,
    /// `@@ … @@`), for renderers that take one (the web UI's `DiffStrip`).
    /// Empty when nothing changed.
    #[serde(default)]
    pub unified_diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPreviewHunk {
    /// 1-based first old line (0 for a hunk that only inserts at the top).
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<ApprovalPreviewLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPreviewLine {
    pub kind: ApprovalPreviewLineKind,
    /// The line without its line ending.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPreviewLineKind {
    Context,
    Add,
    Remove,
}

/// Read-only scope a builder may consult.
pub struct ApprovalPreviewContext<'a> {
    pub artifact_workspace: &'a ArtifactV2Workspace,
    pub principal: Option<&'a str>,
    pub workspace: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    /// The agent definition's display `name`, when known.
    pub agent_display_name: Option<&'a str>,
}

impl ApprovalPreviewContext<'_> {
    /// Display name, else agent id, else a neutral noun.
    pub fn agent_label(&self) -> String {
        [self.agent_display_name, self.agent_id]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|value| !value.is_empty())
            .unwrap_or("The agent")
            .to_string()
    }
}

/// A tool's preview builder: the call's arguments as a JSON object.
/// `None` means "no preview" and the card shows the raw call as before.
pub type ApprovalPreviewBuilder =
    fn(&ApprovalPreviewContext<'_>, &Value) -> Option<ApprovalPreview>;

/// Tools that present a structured preview on their approval card. A tool opts
/// in with one row; the pause path is generic over this table.
static APPROVAL_PREVIEW_BUILDERS: &[(&str, ApprovalPreviewBuilder)] = &[(
    "propose_program_revision",
    crate::magician_v2::execution::compiled_handlers::propose_program_revision::approval_preview,
)];

/// The registered builder for `tool`, if any.
pub fn approval_preview_builder(tool: &str) -> Option<ApprovalPreviewBuilder> {
    APPROVAL_PREVIEW_BUILDERS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, builder)| *builder)
}

/// The preview for a gated action, when its tool registered a builder.
///
/// Never fails the pause: a builder that panics is logged and treated as "no
/// preview".
pub fn approval_preview_for_action(
    ctx: &ApprovalPreviewContext<'_>,
    action: &ExecutableAction,
) -> Option<ApprovalPreview> {
    let ExecutableAction::Pack {
        capability_name,
        resolved_params,
        ..
    } = action
    else {
        return None;
    };
    let builder = approval_preview_builder(capability_name)?;
    let args = Value::Object(
        resolved_params
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    match catch_unwind(AssertUnwindSafe(|| builder(ctx, &args))) {
        Ok(preview) => preview,
        Err(_) => {
            tracing::warn!(
                tool = %capability_name,
                "[APPROVAL-PREVIEW] preview builder panicked; the card shows the raw call"
            );
            None
        },
    }
}

/// A short human line for summaries: the title plus the change size.
pub fn approval_preview_summary_line(preview: &ApprovalPreview) -> String {
    match preview.diff.as_ref() {
        Some(diff) => format!(
            "{} (+{} lines, −{} lines)",
            preview.title, diff.added, diff.removed
        ),
        None => preview.title.clone(),
    }
}

/// `text` cut to at most `max_bytes` on a char boundary, with "…" when cut.
pub fn truncate_preview_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn strip_line_ending(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .unwrap_or(line)
}

/// A capped, hunked line diff of `old` → `new`, with the default caps.
pub fn line_diff_preview(path: &str, old: &str, new: &str) -> ApprovalPreviewDiff {
    line_diff_preview_with_limits(
        path,
        old,
        new,
        APPROVAL_PREVIEW_MAX_DIFF_LINES,
        APPROVAL_PREVIEW_MAX_DIFF_BYTES,
    )
}

/// [`line_diff_preview`] with explicit caps (tests use small ones).
pub fn line_diff_preview_with_limits(
    path: &str,
    old: &str,
    new: &str,
    max_lines: usize,
    max_bytes: usize,
) -> ApprovalPreviewDiff {
    let diff = TextDiff::configure()
        .timeout(DIFF_DEADLINE)
        .diff_lines(old, new);

    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {},
        }
    }

    let mut hunks = Vec::new();
    let mut shown_lines = 0usize;
    let mut shown_bytes = 0usize;
    let mut truncated = false;
    let mut omitted_changed_lines = 0usize;

    for group in diff.grouped_ops(APPROVAL_PREVIEW_CONTEXT_LINES) {
        let Some(first) = group.first() else {
            continue;
        };
        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => ApprovalPreviewLineKind::Context,
                    ChangeTag::Insert => ApprovalPreviewLineKind::Add,
                    ChangeTag::Delete => ApprovalPreviewLineKind::Remove,
                };
                if truncated {
                    if kind != ApprovalPreviewLineKind::Context {
                        omitted_changed_lines += 1;
                    }
                    continue;
                }
                let text = truncate_preview_text(
                    strip_line_ending(change.value()),
                    APPROVAL_PREVIEW_MAX_LINE_BYTES,
                );
                if shown_lines + 1 > max_lines || shown_bytes + text.len() > max_bytes {
                    truncated = true;
                    if kind != ApprovalPreviewLineKind::Context {
                        omitted_changed_lines += 1;
                    }
                    continue;
                }
                shown_lines += 1;
                shown_bytes += text.len();
                lines.push(ApprovalPreviewLine { kind, text });
            }
        }
        // A hunk cut down to context only says nothing; drop it.
        if lines
            .iter()
            .all(|line| line.kind == ApprovalPreviewLineKind::Context)
        {
            continue;
        }
        let old_lines = lines
            .iter()
            .filter(|line| line.kind != ApprovalPreviewLineKind::Add)
            .count();
        let new_lines = lines
            .iter()
            .filter(|line| line.kind != ApprovalPreviewLineKind::Remove)
            .count();
        hunks.push(ApprovalPreviewHunk {
            old_start: if old_lines == 0 {
                old_start
            } else {
                old_start + 1
            },
            old_lines,
            new_start: if new_lines == 0 {
                new_start
            } else {
                new_start + 1
            },
            new_lines,
            lines,
        });
    }

    let unified_diff = render_unified_diff(path, &hunks);
    ApprovalPreviewDiff {
        path: path.to_string(),
        added,
        removed,
        hunks,
        truncated,
        omitted_changed_lines,
        unified_diff,
    }
}

fn render_unified_diff(path: &str, hunks: &[ApprovalPreviewHunk]) -> String {
    if hunks.is_empty() {
        return String::new();
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for hunk in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ));
        for line in &hunk.lines {
            let prefix = match line.kind {
                ApprovalPreviewLineKind::Context => ' ',
                ApprovalPreviewLineKind::Add => '+',
                ApprovalPreviewLineKind::Remove => '-',
            };
            out.push(prefix);
            out.push_str(&line.text);
            out.push('\n');
        }
    }
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn kinds(hunk: &ApprovalPreviewHunk) -> String {
        hunk.lines
            .iter()
            .map(|line| match line.kind {
                ApprovalPreviewLineKind::Context => ' ',
                ApprovalPreviewLineKind::Add => '+',
                ApprovalPreviewLineKind::Remove => '-',
            })
            .collect()
    }

    #[test]
    fn counts_added_and_removed_lines_and_keeps_two_lines_of_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\n";
        let diff = line_diff_preview("doc.md", old, new);
        assert_eq!((diff.added, diff.removed), (2, 1));
        assert!(!diff.truncated);
        assert_eq!(diff.hunks.len(), 1, "{diff:#?}");
        let hunk = &diff.hunks[0];
        // c d [-e +E] f g h [+i] — two context lines either side, merged.
        assert_eq!(kinds(hunk), "  -+   +");
        assert_eq!(hunk.lines[0].text, "c");
        assert_eq!((hunk.old_start, hunk.old_lines), (3, 6));
        assert_eq!((hunk.new_start, hunk.new_lines), (3, 7));
        assert!(diff
            .unified_diff
            .starts_with("--- a/doc.md\n+++ b/doc.md\n@@ -3,6 +3,7 @@\n"));
        assert!(diff.unified_diff.contains("\n-e\n+E\n"));
    }

    #[test]
    fn distant_changes_become_separate_hunks() {
        let old: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 2\n", "line two\n")
            .replace("line 28\n", "line twenty-eight\n");
        let diff = line_diff_preview("doc.md", &old, &new);
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!((diff.added, diff.removed), (2, 2));
        // Unchanged lines far from both changes are not shown.
        assert!(!diff.unified_diff.contains("line 15"));
    }

    #[test]
    fn identical_text_has_no_hunks() {
        let diff = line_diff_preview("doc.md", "same\n", "same\n");
        assert_eq!((diff.added, diff.removed), (0, 0));
        assert!(diff.hunks.is_empty());
        assert!(diff.unified_diff.is_empty());
    }

    #[test]
    fn truncates_at_the_line_cap_and_counts_what_was_left_out() {
        let old = "";
        let new: String = (1..=50).map(|i| format!("added {i}\n")).collect();
        let diff = line_diff_preview_with_limits("doc.md", old, &new, 10, 1_000_000);
        assert!(diff.truncated);
        assert_eq!(diff.added, 50);
        let shown: usize = diff.hunks.iter().map(|hunk| hunk.lines.len()).sum();
        assert_eq!(shown, 10);
        assert_eq!(diff.omitted_changed_lines, 40);
        assert_eq!(diff.hunks[0].new_lines, 10);
    }

    #[test]
    fn truncates_at_the_byte_cap() {
        let new: String = (1..=20).map(|i| format!("{i:0>50}\n")).collect();
        let diff = line_diff_preview_with_limits("doc.md", "", &new, 1_000, 120);
        assert!(diff.truncated);
        let bytes: usize = diff
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .map(|line| line.text.len())
            .sum();
        assert!(bytes <= 120, "{bytes}");
        assert_eq!(diff.omitted_changed_lines, 20 - 2);
    }

    #[test]
    fn multi_byte_text_is_kept_whole_and_long_lines_cut_on_a_char_boundary() {
        let old = "# Positionierung\n\nRuhig, genau.\n";
        let new = "# Positionierung\n\nRuhig, genau, ehrlich — für Eigentümer 🧭.\n";
        let diff = line_diff_preview("doc.md", old, new);
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert!(diff
            .unified_diff
            .contains("+Ruhig, genau, ehrlich — für Eigentümer 🧭."));

        let long = "é".repeat(APPROVAL_PREVIEW_MAX_LINE_BYTES); // 2 bytes each
        let diff = line_diff_preview("doc.md", "", &format!("{long}\n"));
        let text = &diff.hunks[0].lines[0].text;
        assert!(text.ends_with('…'));
        assert!(text.len() <= APPROVAL_PREVIEW_MAX_LINE_BYTES + '…'.len_utf8());
    }

    #[test]
    fn crlf_line_endings_are_stripped() {
        let diff = line_diff_preview("doc.md", "a\r\nb\r\n", "a\r\nc\r\n");
        let texts: Vec<&str> = diff.hunks[0]
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    #[test]
    fn preview_round_trips_and_omits_empty_fields() {
        let preview = ApprovalPreview {
            title: "Mira wants to change `facts.md`".to_string(),
            why_label: None,
            why: None,
            diff: None,
            evidence: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
        };
        let json = serde_json::to_value(&preview).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"title": "Mira wants to change `facts.md`"})
        );
        let back: ApprovalPreview = serde_json::from_value(json).unwrap();
        assert_eq!(back, preview);

        let full = ApprovalPreview {
            diff: Some(line_diff_preview("facts.md", "a\n", "b\n")),
            warnings: vec![ApprovalPreviewWarning::new("stale_base", "changed")],
            evidence: vec!["task t-1".to_string()],
            ..preview
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["diff"]["hunks"][0]["lines"][0]["kind"], "remove");
        let back: ApprovalPreview = serde_json::from_value(json).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn only_registered_pack_tools_get_a_preview_builder() {
        assert!(approval_preview_builder("propose_program_revision").is_some());
        assert!(approval_preview_builder("write_file").is_none());
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().to_path_buf());
        let ctx = ApprovalPreviewContext {
            artifact_workspace: &workspace,
            principal: Some("alpha"),
            workspace: Some("default"),
            agent_id: Some("mira"),
            agent_display_name: None,
        };
        let action = ExecutableAction::SpawnSubGoal {
            goal: "x".to_string(),
            budget: 1,
        };
        assert!(approval_preview_for_action(&ctx, &action).is_none());
        assert_eq!(ctx.agent_label(), "mira");
    }
}
