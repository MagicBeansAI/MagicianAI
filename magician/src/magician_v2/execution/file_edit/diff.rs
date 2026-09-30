//! Unified-diff computation for `FileEditTransaction`.
//!
//! Given `(old_text, new_text, path)`, produces a standard
//! unified-diff-format string that matches what `git diff` emits and
//! what the frontend `DiffStrip` (`ui/unified-ui/src/lib/shell/DiffStrip.svelte`)
//! expects. Uses the `similar` crate's Myers diff implementation;
//! no shell-out to `git`, no working-tree assumption, no repo
//! required — works on any pair of text strings.
//!
//! Output shape matches the contract documented in the frontend's
//! `DiffFile` interface:
//!
//! ```text
//! --- a/<path>
//! +++ b/<path>
//! @@ -<old_start>,<old_count> +<new_start>,<new_count> @@
//!  context line
//! -removed line
//! +added line
//!  context line
//! ```
//!
//! Pure functions; no I/O. Safe to call from any task / thread.

use similar::TextDiff;

/// Number of context lines included around each hunk. Matches `git
/// diff`'s default (`-U3`) so the output is visually consistent with
/// what operators see in their terminal.
const UNIFIED_CONTEXT_LINES: usize = 3;

/// Compute a unified-diff string between two text contents.
///
/// `path` is used to populate the `--- a/<path>` / `+++ b/<path>`
/// header lines. Pass the destination path (the post-edit location)
/// even for moves — the caller is responsible for emitting two
/// separate diffs for rename operations if the on-disk shape requires
/// it.
///
/// Returns an empty string when the texts are identical (no header,
/// no body) so callers can branch on `diff.is_empty()` for a clean
/// "nothing to apply" check. This matches `git diff`'s behavior.
pub fn compute_unified_diff(old_text: &str, new_text: &str, path: &str) -> String {
    if old_text == new_text {
        return String::new();
    }
    let diff = TextDiff::from_lines(old_text, new_text);
    let mut out = diff
        .unified_diff()
        .context_radius(UNIFIED_CONTEXT_LINES)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    // `similar`'s unified-diff emitter doesn't append a trailing
    // newline; `git diff` does. Normalize to match for downstream
    // parsing consistency.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Count `+`/`-` lines in a unified-diff string, ignoring the
/// `+++`/`---` file-meta header lines.
///
/// Returns `(additions, deletions)`. Used to populate the
/// `additions`/`deletions` fields on `DiffFile` payloads without
/// re-walking the diff in the frontend.
pub fn diff_stats(unified_diff: &str) -> DiffStats {
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for line in unified_diff.lines() {
        // Skip file-meta headers (`+++ b/...` / `--- a/...`).
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        // Skip hunk headers (`@@ ... @@`) — they start with `@`, not
        // `+`/`-`, so they wouldn't match anyway, but explicit skip
        // makes the intent obvious.
        if line.starts_with("@@") {
            continue;
        }
        match line.chars().next() {
            Some('+') => additions += 1,
            Some('-') => deletions += 1,
            _ => {},
        }
    }
    DiffStats {
        additions,
        deletions,
    }
}

/// Add/delete line counts for a single file's diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn identical_content_returns_empty_string() {
        let s = "hello\nworld\n";
        let diff = compute_unified_diff(s, s, "test.txt");
        assert!(diff.is_empty(), "got: {diff:?}");
    }

    #[test]
    fn pure_addition_emits_only_plus_lines() {
        let old = "";
        let new = "alpha\nbeta\n";
        let diff = compute_unified_diff(old, new, "new.txt");
        assert!(diff.contains("--- a/new.txt"), "header missing: {diff}");
        assert!(diff.contains("+++ b/new.txt"), "header missing: {diff}");
        assert!(diff.contains("+alpha"), "addition missing: {diff}");
        assert!(diff.contains("+beta"), "addition missing: {diff}");
        let stats = diff_stats(&diff);
        assert_eq!(
            stats,
            DiffStats {
                additions: 2,
                deletions: 0,
            },
            "stats wrong: {diff}"
        );
    }

    #[test]
    fn pure_deletion_emits_only_minus_lines() {
        let old = "alpha\nbeta\n";
        let new = "";
        let diff = compute_unified_diff(old, new, "gone.txt");
        assert!(diff.contains("-alpha"), "deletion missing: {diff}");
        assert!(diff.contains("-beta"), "deletion missing: {diff}");
        let stats = diff_stats(&diff);
        assert_eq!(
            stats,
            DiffStats {
                additions: 0,
                deletions: 2,
            }
        );
    }

    #[test]
    fn modification_emits_mixed_lines_with_context() {
        let old = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let new = "line 1\nline 2 modified\nline 3\nline 4\nline 5\n";
        let diff = compute_unified_diff(old, new, "src/lib.rs");
        assert!(diff.contains("-line 2\n"), "old not removed: {diff}");
        assert!(diff.contains("+line 2 modified\n"), "new not added: {diff}");
        // Context surrounds the change (radius 3).
        assert!(diff.contains(" line 1\n"), "context missing: {diff}");
        assert!(diff.contains(" line 3\n"), "context missing: {diff}");
        let stats = diff_stats(&diff);
        assert_eq!(
            stats,
            DiffStats {
                additions: 1,
                deletions: 1,
            },
            "stats wrong: {diff}"
        );
    }

    #[test]
    fn stats_skip_file_meta_headers() {
        // Even if the diff body contains lines that visually look like
        // additions (`+++ x`), the leading `+++` / `---` markers identify
        // them as headers and stats must ignore them.
        let diff = "--- a/foo\n+++ b/foo\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let stats = diff_stats(diff);
        assert_eq!(
            stats,
            DiffStats {
                additions: 1,
                deletions: 1,
            }
        );
    }

    #[test]
    fn trailing_newline_normalized() {
        // similar's emitter doesn't always trail with `\n`; we
        // normalize so consumers can split on `\n` without an empty
        // trailing element.
        let diff = compute_unified_diff("a\n", "b\n", "x.txt");
        assert!(
            diff.ends_with('\n'),
            "diff missing trailing newline: {diff:?}"
        );
    }

    #[test]
    fn content_with_null_bytes_is_handled() {
        // Defensive: `similar` operates on `str`, so callers that pass
        // arbitrary bytes through `String::from_utf8_lossy` get a clean
        // diff with the replacement char. We're not testing the lossy
        // conversion itself, just that the diff doesn't panic on
        // unusual content.
        let old = "header\n\u{FFFD}\nfooter\n";
        let new = "header\n\u{FFFD}\u{FFFD}\nfooter\n";
        let diff = compute_unified_diff(old, new, "weird.bin");
        // Just check the function returns; specific output isn't
        // contractually guaranteed for replacement chars.
        assert!(!diff.is_empty());
    }

    #[test]
    fn header_path_appears_verbatim() {
        let diff = compute_unified_diff("a", "b", "src/nested/with spaces.rs");
        assert!(
            diff.contains("--- a/src/nested/with spaces.rs"),
            "header path mangled: {diff}"
        );
        assert!(
            diff.contains("+++ b/src/nested/with spaces.rs"),
            "header path mangled: {diff}"
        );
    }

    /// `similar` does not produce a `unified_diff()` for two identical
    /// strings (which we short-circuit), but pure whitespace changes
    /// should still produce a non-empty diff.
    #[test]
    fn whitespace_only_change_is_detected() {
        let old = "func() {\n  body;\n}\n";
        let new = "func() {\n    body;\n}\n"; // 2-space → 4-space indent
        let diff = compute_unified_diff(old, new, "indent.txt");
        assert!(!diff.is_empty(), "whitespace change not detected: {diff}");
        let stats = diff_stats(&diff);
        assert!(
            stats.additions >= 1 && stats.deletions >= 1,
            "stats: {stats:?}"
        );
    }

    /// Tracked by `ChangeTag` in `similar`; we don't depend on it
    /// directly, but ensure the enum is reachable so a future use of
    /// per-line walks compiles.
    #[test]
    fn changetag_reachable() {
        let _: similar::ChangeTag = similar::ChangeTag::Equal;
    }
}
