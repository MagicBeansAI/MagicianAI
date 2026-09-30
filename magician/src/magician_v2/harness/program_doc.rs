//! Program DOCUMENT reads + the managed-section editor.
//!
//! Two consumers (see `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`):
//!
//! - The Fleet Civilization game reads program docs (guild "mission chips"):
//!   [`list_program_docs`] / [`read_program_doc`] — read-only, scoped to the
//!   scope's `programs/` root, never scanning outside it.
//! - The CEO decomposition tool APPLIES approved missions via
//!   [`ProgramDocEditor`]: it edits ONLY the managed `## Missions (CEO)`
//!   section (replace-or-append), stamps provenance, snapshots the prior file
//!   to `programs/.history/` before every write, and refuses to create new
//!   programs. This is deliberately NOT the learning program-STATE lane —
//!   document edits get their own narrow, section-scoped editor.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Serialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::magician_v2::artifact_v2::io::{sync_parent_dir, write_bytes_durably};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// The one section the editor manages. Everything else in a program doc is
/// human territory and is never rewritten.
pub const MISSIONS_SECTION_HEADING: &str = "Missions (CEO)";

/// Managed-section body cap — a runaway decomposition cannot bloat a program.
pub const MANAGED_SECTION_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct ProgramDocSummary {
    /// File name within `programs/` (e.g. `engineering_strategy.md`).
    pub name: String,
    /// First `# ` heading, falling back to a prettified file name.
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramDoc {
    pub name: String,
    pub title: String,
    pub content: String,
    /// The managed `## Missions (CEO)` section body, when present.
    pub missions_section: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppliedProgramEdit {
    pub program: String,
    /// History snapshot file name (under `programs/.history/`).
    pub history_snapshot: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistorySnapshot {
    /// Snapshot file name (under `programs/.history/`).
    pub snapshot: String,
    /// Filesystem mtime, RFC3339.
    pub modified: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevertedProgramEdit {
    pub program: String,
    /// The history snapshot the program was restored FROM.
    pub restored_from: String,
    /// Snapshot of the pre-revert content — a revert is itself revertible.
    pub pre_revert_snapshot: String,
}

/// Program names are bare `*.md` file names inside the scope's programs root —
/// no separators, no dotfiles, no traversal.
fn validate_name(name: &str) -> Result<()> {
    let valid = name.ends_with(".md")
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name.len() > 3;
    if !valid {
        bail!("invalid program name: {name:?}");
    }
    Ok(())
}

fn prettify(name: &str) -> String {
    name.trim_end_matches(".md")
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_from(content: &str, name: &str) -> String {
    content
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| prettify(name))
}

/// Extract the body of the managed section (without its heading line), up to
/// the next `#`/`##` heading or EOF.
fn extract_section(content: &str, heading: &str) -> Option<String> {
    let needle = format!("## {heading}");
    let mut lines = content.lines();
    let mut body: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in &mut lines {
        if inside {
            let trimmed = line.trim_start();
            if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
                break;
            }
            body.push(line);
        } else if line.trim() == needle {
            inside = true;
        }
    }
    if !inside {
        return None;
    }
    let text = body.join("\n").trim().to_string();
    Some(text)
}

/// Replace the managed section in place, or append it at the end.
fn replace_or_append_section(content: &str, heading: &str, new_section: &str) -> String {
    let needle = format!("## {heading}");
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.trim() == needle);
    match start {
        Some(start_idx) => {
            let mut end_idx = lines.len();
            for (offset, line) in lines[start_idx + 1..].iter().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
                    end_idx = start_idx + 1 + offset;
                    break;
                }
            }
            let mut out: Vec<&str> = Vec::new();
            out.extend_from_slice(&lines[..start_idx]);
            let section_lines: Vec<&str> = new_section.lines().collect();
            out.extend_from_slice(&section_lines);
            out.extend_from_slice(&lines[end_idx..]);
            let mut joined = out.join("\n");
            if content.ends_with('\n') && !joined.ends_with('\n') {
                joined.push('\n');
            }
            joined
        },
        None => {
            let mut out = content.trim_end().to_string();
            out.push_str("\n\n");
            out.push_str(new_section);
            out.push('\n');
            out
        },
    }
}

/// List the scope's program documents (non-recursive; skips dotfiles and the
/// `state`/`.history` subtrees by construction).
pub async fn list_program_docs(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
) -> Result<Vec<ProgramDocSummary>> {
    let root = workspace.programs_root(principal, workspace_name);
    let mut out = Vec::new();
    let mut entries = match fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(_) => return Ok(out), // no programs dir yet — empty, not an error
    };
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if validate_name(&name).is_err() {
            continue;
        }
        let content = fs::read_to_string(entry.path()).await.unwrap_or_default();
        out.push(ProgramDocSummary {
            title: title_from(&content, &name),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// True when `file_name` is a history snapshot OF `program` — the exact
/// `{stem}-{STAMP}-{uuid}.md` shape written by the editor. Strict on purpose:
/// a bare `starts_with(stem)` would cross-match programs whose names prefix
/// one another (e.g. `company.md` vs `company-strategy.md`).
fn is_snapshot_of(program: &str, file_name: &str) -> bool {
    let stem = format!("{}-", program.trim_end_matches(".md"));
    let Some(rest) = file_name.strip_prefix(&stem) else {
        return false;
    };
    // rest = "{%Y%m%dT...}-{uuid}.md" — begins with 8 digits + 'T'
    rest.ends_with(".md")
        && rest.len() > 9
        && rest.as_bytes()[..8].iter().all(u8::is_ascii_digit)
        && rest.as_bytes()[8] == b'T'
}

/// List the history snapshots for one program, newest first (the timestamp is
/// embedded in the file name, so a name sort IS a time sort).
pub async fn list_history_snapshots(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    program: &str,
) -> Result<Vec<HistorySnapshot>> {
    validate_name(program)?;
    let dir = workspace
        .programs_root(principal, workspace_name)
        .join(".history");
    let mut out = Vec::new();
    let mut entries = match fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(_) => return Ok(out), // no history yet
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_snapshot_of(program, &name) {
            continue;
        }
        let modified: chrono::DateTime<Utc> = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(Into::into)
            .unwrap_or_else(Utc::now);
        out.push(HistorySnapshot {
            snapshot: name,
            modified: modified.to_rfc3339(),
        });
    }
    out.sort_by(|a, b| b.snapshot.cmp(&a.snapshot));
    Ok(out)
}

/// Read one program document (None when it doesn't exist).
pub async fn read_program_doc(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    name: &str,
) -> Result<Option<ProgramDoc>> {
    validate_name(name)?;
    let path = workspace
        .programs_root(principal, workspace_name)
        .join(name);
    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(_) => return Ok(None),
    };
    Ok(Some(ProgramDoc {
        title: title_from(&content, name),
        missions_section: extract_section(&content, MISSIONS_SECTION_HEADING),
        name: name.to_string(),
        content,
    }))
}

/// The managed-section editor. Narrow by design: one section, existing
/// programs only, snapshot-before-write, size-capped.
pub struct ProgramDocEditor {
    workspace: ArtifactV2Workspace,
}

impl ProgramDocEditor {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    /// Write `content` as a new `.history/` snapshot for `program`; returns
    /// the snapshot file name. Shared by apply (pre-write) and revert
    /// (pre-revert) so every mutation leaves a restore point.
    async fn snapshot_content(
        &self,
        root: &std::path::Path,
        program: &str,
        content: &str,
    ) -> Result<String> {
        let history_dir = root.join(".history");
        fs::create_dir_all(&history_dir)
            .await
            .context("create programs/.history")?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let snapshot = format!(
            "{}-{stamp}-{}.md",
            program.trim_end_matches(".md"),
            uuid::Uuid::new_v4().simple()
        );
        let snapshot_path = history_dir.join(&snapshot);
        let mut snapshot_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&snapshot_path)
            .await
            .context("create unique history snapshot")?;
        snapshot_file
            .write_all(content.as_bytes())
            .await
            .context("write history snapshot")?;
        snapshot_file
            .flush()
            .await
            .context("flush history snapshot")?;
        // The snapshot is the ONLY restore point for the program overwrite that
        // follows, and that overwrite is now durable — so this has to be on disk
        // first, contents and directory entry both. `create_new` is kept rather
        // than routed through the temp-and-rename helper because the uniqueness
        // check is the point of this path: a rename would silently publish over
        // a name collision instead of refusing it.
        snapshot_file
            .sync_all()
            .await
            .context("fsync history snapshot")?;
        drop(snapshot_file);
        sync_parent_dir(&snapshot_path)
            .await
            .context("fsync programs/.history")?;
        Ok(snapshot)
    }

    /// Restore a program from a history snapshot (default: the newest one).
    /// The current content is snapshotted FIRST, so a revert can be reverted.
    /// A requested snapshot name is resolved strictly against this program's
    /// own listed snapshots — never used as a path.
    pub async fn revert_from_history(
        &self,
        principal: &str,
        workspace_name: &str,
        program: &str,
        snapshot: Option<&str>,
    ) -> Result<RevertedProgramEdit> {
        validate_name(program)?;
        let root = self.workspace.programs_root(principal, workspace_name);
        let path = root.join(program);
        let current = fs::read_to_string(&path)
            .await
            .with_context(|| format!("program {program:?} does not exist"))?;

        let snapshots =
            list_history_snapshots(&self.workspace, principal, workspace_name, program).await?;
        let chosen = match snapshot {
            Some(requested) => snapshots
                .iter()
                .find(|s| s.snapshot == requested)
                .map(|s| s.snapshot.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!("snapshot {requested:?} not found for program {program:?}")
                })?,
            None => snapshots
                .first()
                .map(|s| s.snapshot.clone())
                .ok_or_else(|| anyhow::anyhow!("no history snapshots for program {program:?}"))?,
        };
        let restored_content = fs::read_to_string(root.join(".history").join(&chosen))
            .await
            .with_context(|| format!("read history snapshot {chosen:?}"))?;

        let pre_revert_snapshot = self.snapshot_content(&root, program, &current).await?;
        write_bytes_durably(&path, restored_content.as_bytes())
            .await
            .with_context(|| format!("restore program {program:?}"))?;

        Ok(RevertedProgramEdit {
            program: program.to_string(),
            restored_from: chosen,
            pre_revert_snapshot,
        })
    }

    /// Apply an approved missions body to the program's managed section.
    /// `provenance` is stamped into the section (e.g. `task <id>`).
    pub async fn apply_managed_section(
        &self,
        principal: &str,
        workspace_name: &str,
        program: &str,
        body_md: &str,
        provenance: &str,
    ) -> Result<AppliedProgramEdit> {
        validate_name(program)?;
        if body_md.len() > MANAGED_SECTION_MAX_BYTES {
            bail!(
                "managed section body exceeds {MANAGED_SECTION_MAX_BYTES} bytes ({})",
                body_md.len()
            );
        }
        let root = self.workspace.programs_root(principal, workspace_name);
        let path = root.join(program);
        let existing = fs::read_to_string(&path)
            .await
            .with_context(|| format!("program {program:?} must already exist (no new files)"))?;

        // snapshot BEFORE the write — the owner's revert path
        let snapshot = self.snapshot_content(&root, program, &existing).await?;

        let section = format!(
            "## {MISSIONS_SECTION_HEADING}\n\n<!-- managed by ceo-decomposition; {} ; applied {} -->\n\n{}",
            provenance.trim(),
            Utc::now().to_rfc3339(),
            body_md.trim()
        );
        let updated = replace_or_append_section(&existing, MISSIONS_SECTION_HEADING, &section);
        // Durable publish: a torn write here loses a human-authored program
        // document, and the snapshot taken above is only a restore point if the
        // file it replaces was never left half-written.
        write_bytes_durably(&path, updated.as_bytes())
            .await
            .with_context(|| format!("write program {program:?}"))?;

        Ok(AppliedProgramEdit {
            program: program.to_string(),
            history_snapshot: snapshot,
        })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn fixture() -> (TempDir, ArtifactV2Workspace) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(dir.path());
        let programs = workspace.programs_root("owner", "default");
        fs::create_dir_all(&programs).await.expect("programs dir");
        fs::write(
            programs.join("engineering_strategy.md"),
            "# Engineering Strategy\n\nShip well.\n\n## Cadence\n\nWeekly.\n",
        )
        .await
        .expect("seed program");
        (dir, workspace)
    }

    #[tokio::test]
    async fn lists_and_reads_program_docs() {
        let (_dir, ws) = fixture().await;
        let listed = list_program_docs(&ws, "owner", "default")
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "engineering_strategy.md");
        assert_eq!(listed[0].title, "Engineering Strategy");

        let doc = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        assert!(doc.content.contains("Ship well."));
        assert!(doc.missions_section.is_none());

        assert!(read_program_doc(&ws, "owner", "default", "nope.md")
            .await
            .expect("read missing")
            .is_none());
        assert!(read_program_doc(&ws, "owner", "default", "../evil.md")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn editor_appends_then_replaces_only_the_managed_section() {
        let (_dir, ws) = fixture().await;
        let editor = ProgramDocEditor::new(ws.clone());

        let first = editor
            .apply_managed_section(
                "owner",
                "default",
                "engineering_strategy.md",
                "- **M1** — harden the pipeline\n- **M2** — cut flaky tests",
                "task t-1",
            )
            .await
            .expect("apply");
        assert!(first.history_snapshot.starts_with("engineering_strategy-"));

        let doc = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        let missions = doc.missions_section.expect("managed section present");
        assert!(missions.contains("M1"));
        // human sections untouched
        assert!(doc.content.contains("## Cadence"));
        assert!(doc.content.contains("Weekly."));

        // replace: new body, single managed section, history grows
        let second = editor
            .apply_managed_section(
                "owner",
                "default",
                "engineering_strategy.md",
                "- **M3** — the only mission now",
                "task t-2",
            )
            .await
            .expect("re-apply");
        assert_ne!(first.history_snapshot, second.history_snapshot);
        let doc2 = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        let missions2 = doc2.missions_section.expect("section");
        assert!(missions2.contains("M3"));
        assert!(!missions2.contains("M1"));
        assert_eq!(doc2.content.matches(MISSIONS_SECTION_HEADING).count(), 1);

        let history = ws.programs_root("owner", "default").join(".history");
        let first_snapshot = fs::read_to_string(history.join(&first.history_snapshot))
            .await
            .expect("first history snapshot");
        let second_snapshot = fs::read_to_string(history.join(&second.history_snapshot))
            .await
            .expect("second history snapshot");
        assert!(!first_snapshot.contains("M1"));
        assert!(second_snapshot.contains("M1"));
        assert!(!second_snapshot.contains("M3"));
        let mut count = 0;
        let mut rd = fs::read_dir(&history).await.expect("history dir");
        while rd.next_entry().await.expect("entry").is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn snapshot_matching_is_strict_about_program_stems() {
        let stamped = "20260710T041530.123Z-0123456789abcdef0123456789abcdef.md";
        assert!(is_snapshot_of("company.md", &format!("company-{stamped}")));
        // a hyphenated sibling's snapshots must NOT match the shorter program
        assert!(!is_snapshot_of(
            "company.md",
            &format!("company-strategy-{stamped}")
        ));
        assert!(is_snapshot_of(
            "company-strategy.md",
            &format!("company-strategy-{stamped}")
        ));
        assert!(!is_snapshot_of("company.md", "company-notes.md"));
    }

    #[tokio::test]
    async fn revert_restores_latest_snapshot_and_is_itself_revertible() {
        let (_dir, ws) = fixture().await;
        let editor = ProgramDocEditor::new(ws.clone());

        editor
            .apply_managed_section(
                "owner",
                "default",
                "engineering_strategy.md",
                "- **M1**",
                "t-1",
            )
            .await
            .expect("apply");
        let doc = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        assert!(doc.missions_section.is_some());

        // history now lists exactly one snapshot for this program
        let history = list_history_snapshots(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("list history");
        assert_eq!(history.len(), 1);

        let reverted = editor
            .revert_from_history("owner", "default", "engineering_strategy.md", None)
            .await
            .expect("revert");
        assert_eq!(reverted.restored_from, history[0].snapshot);
        assert_ne!(reverted.pre_revert_snapshot, reverted.restored_from);

        // the managed section is gone; human content intact
        let doc2 = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        assert!(doc2.missions_section.is_none());
        assert!(doc2.content.contains("## Cadence"));

        // the pre-revert snapshot preserves the missions — revert is revertible
        let pre = fs::read_to_string(
            ws.programs_root("owner", "default")
                .join(".history")
                .join(&reverted.pre_revert_snapshot),
        )
        .await
        .expect("pre-revert snapshot");
        assert!(pre.contains("M1"));

        // strictness: unknown snapshot name refused; no cross-program matches
        assert!(editor
            .revert_from_history(
                "owner",
                "default",
                "engineering_strategy.md",
                Some("nope.md")
            )
            .await
            .is_err());
        assert!(
            list_history_snapshots(&ws, "owner", "default", "engineering.md")
                .await
                .expect("prefix-neighbour listing")
                .is_empty()
        );
    }

    /// Every name directly under `dir` (non-recursive), sorted.
    async fn entry_names(dir: &std::path::Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut entries = fs::read_dir(dir).await.expect("read dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();
        names
    }

    #[tokio::test]
    async fn apply_and_revert_publish_atomically_and_leave_no_staging_files() {
        let (_dir, ws) = fixture().await;
        let editor = ProgramDocEditor::new(ws.clone());
        let programs = ws.programs_root("owner", "default");

        editor
            .apply_managed_section(
                "owner",
                "default",
                "engineering_strategy.md",
                "- **M1**",
                "t-1",
            )
            .await
            .expect("apply");
        editor
            .revert_from_history("owner", "default", "engineering_strategy.md", None)
            .await
            .expect("revert");

        // A reader of the programs root sees the document and the history dir —
        // never a staging sibling from the temp-and-rename publish.
        assert_eq!(
            entry_names(&programs).await,
            vec![
                ".history".to_string(),
                "engineering_strategy.md".to_string()
            ],
            "durable writes must not leave staging files in the programs root"
        );
        assert!(
            entry_names(&programs.join(".history"))
                .await
                .iter()
                .all(|name| name.ends_with(".md")),
            "history snapshots must be the only files in .history"
        );

        // And the published document still parses as a program doc.
        let doc = read_program_doc(&ws, "owner", "default", "engineering_strategy.md")
            .await
            .expect("read")
            .expect("present");
        assert!(doc.content.contains("## Cadence"));
        assert!(doc.missions_section.is_none());
    }

    #[tokio::test]
    async fn editor_refuses_new_files_and_oversized_bodies() {
        let (_dir, ws) = fixture().await;
        let editor = ProgramDocEditor::new(ws.clone());
        assert!(editor
            .apply_managed_section("owner", "default", "brand_new.md", "- M1", "t")
            .await
            .is_err());
        let huge = "x".repeat(MANAGED_SECTION_MAX_BYTES + 1);
        assert!(editor
            .apply_managed_section("owner", "default", "engineering_strategy.md", &huge, "t")
            .await
            .is_err());
    }
}
