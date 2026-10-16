//! `apply_patch` — stage a unified diff as one approval-gated transaction.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::staged_file_edit;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::file_edit::transaction::ProposedEdit;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(patch) = args
        .get("patch")
        .or_else(|| args.get("diff"))
        .and_then(Value::as_str)
    else {
        return Ok(json!({
            "status": "error",
            "reason": "apply_patch requires `patch` (unified diff string).",
        }));
    };

    let parsed = match parse_unified_patch(patch) {
        Ok(files) => files,
        Err(err) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Could not parse patch: {err:#}"),
            }));
        },
    };
    if parsed.is_empty() {
        return Ok(json!({
            "status": "error",
            "reason": "Patch did not contain any file hunks.",
        }));
    }

    let mut edits = Vec::new();
    let mut scope_anchor = None;
    let mut changed_paths = Vec::new();

    for file_patch in parsed {
        let raw_target = file_patch
            .new_path
            .as_ref()
            .or(file_patch.old_path.as_ref())
            .ok_or_else(|| anyhow!("file patch has neither old nor new path"))
            .map_err(|err| {
                ExecutionError::Step(format!("apply_patch target resolution failed: {err:#}"))
            })?;
        let scoped = match staged_file_edit::resolve_path(
            &resources,
            &args,
            raw_target.to_string_lossy().as_ref(),
        ) {
            Ok(path) => path,
            Err(err) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("Invalid patch path `{}`: {err:#}", raw_target.display()),
                }));
            },
        };
        if scope_anchor.is_none() {
            scope_anchor = Some(scoped.clone());
        }

        let edit = match (&file_patch.old_path, &file_patch.new_path) {
            (None, Some(_)) => {
                let new_content = match apply_hunks("", &file_patch.hunks) {
                    Ok(content) => content,
                    Err(err) => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("Could not apply create hunk for `{}`: {err:#}", scoped.relative_path.display()),
                        }));
                    },
                };
                ProposedEdit::Create {
                    path: scoped.relative_path.clone(),
                    content: new_content,
                }
            },
            (Some(_), None) => {
                let old_content = match staged_file_edit::read_text_bounded(&scoped.absolute_path) {
                    Ok(content) => content,
                    Err(err) => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("Could not read `{}` for delete patch baseline: {err:#}", scoped.relative_path.display()),
                        }));
                    },
                };
                let new_content = match apply_hunks(&old_content, &file_patch.hunks) {
                    Ok(content) => content,
                    Err(err) => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("Delete patch did not apply to `{}`: {err:#}", scoped.relative_path.display()),
                        }));
                    },
                };
                if !new_content.is_empty() {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!("Delete patch for `{}` left {} bytes; full-file delete patches must remove all content.", scoped.relative_path.display(), new_content.len()),
                    }));
                }
                ProposedEdit::Delete {
                    path: scoped.relative_path.clone(),
                }
            },
            (Some(old_path), Some(new_path)) => {
                if old_path != new_path {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!(
                            "Rename patches are not supported yet (`{}` -> `{}`). Use separate delete/create staging.",
                            old_path.display(),
                            new_path.display()
                        ),
                    }));
                }
                let old_content = match staged_file_edit::read_text_bounded(&scoped.absolute_path) {
                    Ok(content) => content,
                    Err(err) => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("Could not read `{}` for patch baseline: {err:#}", scoped.relative_path.display()),
                        }));
                    },
                };
                let new_content = match apply_hunks(&old_content, &file_patch.hunks) {
                    Ok(content) => content,
                    Err(err) => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("Patch did not apply to `{}`: {err:#}", scoped.relative_path.display()),
                        }));
                    },
                };
                if new_content == old_content {
                    continue;
                }
                ProposedEdit::Modify {
                    path: scoped.relative_path.clone(),
                    new_content,
                }
            },
            (None, None) => {
                return Ok(json!({
                    "status": "error",
                    "reason": "Patch file entry has neither old nor new path.",
                }));
            },
        };

        changed_paths.push(scoped.relative_path.display().to_string());
        edits.push(edit);
    }

    if edits.is_empty() {
        return Ok(json!({
            "status": "ok",
            "no_change": true,
            "reason": "Patch applied cleanly but produced no content changes.",
        }));
    }

    let Some(scoped) = scope_anchor else {
        return Ok(json!({
            "status": "error",
            "reason": "Patch did not resolve to a scoped workspace.",
        }));
    };

    match staged_file_edit::stage_edits(
        &resources,
        &scoped,
        format!(
            "Apply patch to {} file{}: {}",
            changed_paths.len(),
            if changed_paths.len() == 1 { "" } else { "s" },
            changed_paths.join(", ")
        ),
        edits,
    ) {
        Ok(transaction) => Ok(staged_file_edit::pending_approval_response(&transaction)),
        Err(err) => Ok(json!({
            "status": "error",
            "reason": format!("Could not stage patch for approval: {err:#}"),
        })),
    }
}

#[derive(Debug, Clone)]
struct ParsedFilePatch {
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
    hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_patch(input: &str) -> Result<Vec<ParsedFilePatch>> {
    let mut files = Vec::new();
    let mut current: Option<ParsedFilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in input.lines() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut current, &mut current_hunk)?;
            flush_file(&mut files, &mut current);
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            flush_hunk(&mut current, &mut current_hunk)?;
            flush_file(&mut files, &mut current);
            current = Some(ParsedFilePatch {
                old_path: parse_patch_path(rest)?,
                new_path: None,
                hunks: Vec::new(),
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            let file = current
                .as_mut()
                .ok_or_else(|| anyhow!("encountered `+++` before `---`"))?;
            file.new_path = parse_patch_path(rest)?;
            continue;
        }

        if line.starts_with("@@") {
            flush_hunk(&mut current, &mut current_hunk)?;
            let old_start = parse_hunk_old_start(line)
                .with_context(|| format!("parse hunk header `{line}`"))?;
            current_hunk = Some(Hunk {
                old_start,
                lines: Vec::new(),
            });
            continue;
        }

        if line.starts_with("\\ No newline at end of file") {
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if line.is_empty() {
                continue;
            }
            let (marker, text) = line.split_at(1);
            match marker {
                " " => hunk.lines.push(HunkLine::Context(text.to_string())),
                "+" => hunk.lines.push(HunkLine::Add(text.to_string())),
                "-" => hunk.lines.push(HunkLine::Remove(text.to_string())),
                _ => {},
            }
        }
    }

    flush_hunk(&mut current, &mut current_hunk)?;
    flush_file(&mut files, &mut current);
    Ok(files)
}

fn flush_hunk(current: &mut Option<ParsedFilePatch>, hunk: &mut Option<Hunk>) -> Result<()> {
    let Some(hunk) = hunk.take() else {
        return Ok(());
    };
    let file = current
        .as_mut()
        .ok_or_else(|| anyhow!("encountered hunk before file header"))?;
    file.hunks.push(hunk);
    Ok(())
}

fn flush_file(files: &mut Vec<ParsedFilePatch>, current: &mut Option<ParsedFilePatch>) {
    if let Some(file) = current.take() {
        if !file.hunks.is_empty() {
            files.push(file);
        }
    }
}

fn parse_patch_path(raw: &str) -> Result<Option<PathBuf>> {
    let token = raw
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("missing patch path"))?;
    if token == "/dev/null" {
        return Ok(None);
    }
    let token = token
        .strip_prefix("a/")
        .or_else(|| token.strip_prefix("b/"))
        .unwrap_or(token);
    Ok(Some(staged_file_edit::normalize_relative_path(Path::new(
        token,
    ))?))
}

fn parse_hunk_old_start(line: &str) -> Result<usize> {
    let start = line
        .find('-')
        .ok_or_else(|| anyhow!("hunk header missing old range"))?
        + 1;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c == ',' || c.is_whitespace())
        .ok_or_else(|| anyhow!("hunk header old range is malformed"))?;
    let parsed = rest[..end].parse::<usize>()?;
    Ok(parsed.max(1))
}

fn apply_hunks(old_content: &str, hunks: &[Hunk]) -> Result<String> {
    let old_lines = split_lines_preserve_endings(old_content);
    let mut output = Vec::new();
    let mut old_index = 0usize;

    for hunk in hunks {
        let target = hunk.old_start.saturating_sub(1);
        if target > old_lines.len() {
            return Err(anyhow!(
                "hunk starts at old line {}, but file has {} lines",
                hunk.old_start,
                old_lines.len()
            ));
        }
        while old_index < target {
            output.push(old_lines[old_index].clone());
            old_index += 1;
        }

        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    assert_old_line(&old_lines, old_index, expected, "context")?;
                    output.push(old_lines[old_index].clone());
                    old_index += 1;
                },
                HunkLine::Remove(expected) => {
                    assert_old_line(&old_lines, old_index, expected, "remove")?;
                    old_index += 1;
                },
                HunkLine::Add(text) => output.push(format!("{text}\n")),
            }
        }
    }

    while old_index < old_lines.len() {
        output.push(old_lines[old_index].clone());
        old_index += 1;
    }

    Ok(output.concat())
}

fn split_lines_preserve_endings(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    content.split_inclusive('\n').map(str::to_string).collect()
}

fn assert_old_line(lines: &[String], index: usize, expected: &str, kind: &str) -> Result<()> {
    let Some(actual) = lines.get(index) else {
        return Err(anyhow!(
            "{kind} line expected `{expected}`, but file ended at line {}",
            index + 1
        ));
    };
    let actual_body = actual.trim_end_matches(['\r', '\n']);
    if actual_body != expected {
        return Err(anyhow!(
            "{kind} line mismatch at old line {}: expected `{}`, found `{}`",
            index + 1,
            expected,
            actual_body
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn parse_and_apply_modify_patch() {
        let patch = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
 }
";
        let files = parse_unified_patch(patch).unwrap();
        assert_eq!(files.len(), 1);
        let out = apply_hunks("fn main() {\n    println!(\"old\");\n}\n", &files[0].hunks).unwrap();
        assert_eq!(out, "fn main() {\n    println!(\"new\");\n}\n");
    }

    #[test]
    fn parse_create_patch() {
        let patch = "\
--- /dev/null
+++ b/hello.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let files = parse_unified_patch(patch).unwrap();
        assert!(files[0].old_path.is_none());
        assert_eq!(files[0].new_path.as_deref(), Some(Path::new("hello.txt")));
        assert_eq!(apply_hunks("", &files[0].hunks).unwrap(), "hello\nworld\n");
    }

    #[test]
    fn delete_patch_must_remove_all_content() {
        let patch = "\
--- a/hello.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-hello
-world
";
        let files = parse_unified_patch(patch).unwrap();
        assert!(files[0].new_path.is_none());
        assert_eq!(apply_hunks("hello\nworld\n", &files[0].hunks).unwrap(), "");
    }
}
