//! Major checkpoints — a known-good, rewindable state of a VibeDev coding run.
//!
//! A checkpoint is minted **once per applied code change that subsequently passes
//! `run_project_checks`** (so every node is a verified-good state — see
//! `docs/archive/plans/2026-06-18-major-checkpoints.md`). It is a metadata record that
//! REFERENCES the rewind anchors rather than capturing content itself:
//!   - `git_sha`: a Magician-made commit of the REAL working tree at the checks-pass
//!     boundary — the correct whole-tree rewind anchor (`git checkout <sha>`).
//!   - `snapshot_id`: the apply-time per-proposal file snapshot (transaction undo).
//!   - `native_session_id`: engine-bound session/thread to resume from.
//!   - `pi_session_id`: legacy Pi session id. Readers fall back to this.
//!
//! One JSON file per checkpoint under `<scope_root>/checkpoints/<id>.json`, mirroring
//! the snapshots/proposals layout. The id is the **originating proposal id**, so "is
//! this applied change already checkpointed?" is a trivial `exists()` (the dedup gate);
//! the implicit run-start baseline uses `baseline-<execution_id>`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// Implicit "before this run" anchor, synthesized at run start.
    Baseline,
    /// A checks-passing applied code change.
    Major,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Stable id. For a `Major` checkpoint this is the originating proposal id (so the
    /// dedup gate is a trivial `exists`); for a `Baseline` it is `baseline-<execution_id>`.
    pub id: String,
    /// Human/AI label — the Pi-authored proposal summary (no extra LLM cost).
    pub name: String,
    pub kind: CheckpointKind,
    /// The applied proposal this checkpoint attests (None for a baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    /// Magician-made commit of the REAL working tree at the checks-pass boundary — the
    /// whole-tree rewind anchor. None if the scope is not a git repo / the commit failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    /// The apply-time per-proposal file snapshot (transaction-undo fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Engine that owns `native_session_id` (`pi`, `codex_app_server`, or `grok_acp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Engine-bound native session or thread. Preferred over `pi_session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    /// Legacy Pi coding-session. Readers fall back here when `native_session_id`
    /// is absent so old checkpoint files still rewind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_files: Vec<PathBuf>,
    /// The REAL repo working-tree path this checkpoint was captured in — where `git restore` runs
    /// on rewind. None for legacy / non-git checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<PathBuf>,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Checkpoint {
    /// The id used for a run-start baseline checkpoint.
    pub fn baseline_id(execution_id: &str) -> String {
        format!("baseline-{execution_id}")
    }

    /// Native session to resume. Prefers the generic field; old records
    /// that only stored `pi_session_id` still rewind.
    pub fn resume_session_id(&self) -> Option<&str> {
        self.native_session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.pi_session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
    }
}

/// One JSON file per checkpoint under `<scope_root>/checkpoints/<id>.json`. Mirrors
/// `SnapshotStore` (tmp-then-rename persist) but adds the listing the snapshot store
/// lacks — the rail needs to enumerate a run's checkpoints.
pub struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    pub fn new(scope_root: impl Into<PathBuf>) -> Self {
        Self {
            root: scope_root.into().join("checkpoints"),
        }
    }

    /// Persist a checkpoint (create-or-overwrite). Idempotent by id.
    pub fn create(&self, checkpoint: &Checkpoint) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create checkpoints root {}", self.root.display()))?;
        let bytes = serde_json::to_vec_pretty(checkpoint).context("serialize checkpoint")?;
        let path = self.path_for(&checkpoint.id);
        // The shared durable writer, not a fixed `<id>.json.tmp` sibling: the
        // create is overwrite-by-id, so two runs checkpointing the same id
        // share that one staging name and can rename a half-written record
        // over the checkpoint. It also `sync_all`s the record and the
        // checkpoints directory, so a checkpoint that survives a crash is a
        // rewind anchor that still resolves.
        write_bytes_durably_sync(&path, &bytes)
            .with_context(|| format!("write checkpoint {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Checkpoint>> {
        let path = self.path_for(id);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let cp: Checkpoint = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse checkpoint {}", path.display()))?;
                Ok(Some(cp))
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read checkpoint {}", path.display())),
        }
    }

    /// True if a checkpoint already exists for this id — the dedup gate, so a re-run of
    /// `run_project_checks` with no new applied change mints nothing.
    pub fn exists(&self, id: &str) -> bool {
        self.path_for(id).exists()
    }

    /// All checkpoints for a task, newest first. Best-effort: skips unparseable files.
    pub fn list_for_task(&self, task_id: &str) -> Vec<Checkpoint> {
        let mut out = self.list_all();
        out.retain(|cp| cp.task_id == task_id);
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    fn list_all(&self) -> Vec<Checkpoint> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(cp) = serde_json::from_slice::<Checkpoint>(&bytes) {
                    out.push(cp);
                }
            }
        }
        out
    }

    fn path_for(&self, id: &str) -> PathBuf {
        // ids come from proposal ids / "baseline-<exec>" — sanitize to a safe filename.
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join(format!("{safe}.json"))
    }
}

/// Capture the REAL working tree (tracked + untracked) into a git commit reachable ONLY via a
/// side ref (`refs/vibedev/checkpoints/<id>`) — never the user's branch, index, or working files
/// — so history stays clean but the commit object exists for a true rewind. A throwaway
/// `GIT_INDEX_FILE` keeps the real index untouched. Returns the commit sha, or None when the path
/// isn't a git work tree / git is unavailable / nothing to capture. Non-mutating by construction.
pub fn git_side_ref_snapshot(
    real_path: &std::path::Path,
    checkpoint_id: &str,
    message: &str,
) -> Option<String> {
    use std::process::Command;

    let safe_id: String = checkpoint_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let git = |index: Option<&std::path::Path>, a: &[&str]| -> Option<std::process::Output> {
        let mut command = Command::new("git");
        command.arg("-C").arg(real_path);
        if let Some(idx) = index {
            command.env("GIT_INDEX_FILE", idx);
        }
        command.args(a).output().ok()
    };

    if !git(None, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return None;
    }
    let tmp_index = std::env::temp_dir().join(format!(
        "vibedev-ckpt-index-{}-{}",
        std::process::id(),
        safe_id
    ));
    let _ = std::fs::remove_file(&tmp_index);
    let _ = git(Some(&tmp_index), &["read-tree", "HEAD"]);
    let added = git(Some(&tmp_index), &["add", "-A"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    let tree = if added {
        git(Some(&tmp_index), &["write-tree"])
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let _ = std::fs::remove_file(&tmp_index);
    let tree = tree?;

    let head = git(None, &["rev-parse", "HEAD"])
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let message = format!("vibedev checkpoint: {message}");
    let mut commit_args: Vec<&str> = vec!["commit-tree", &tree, "-m", &message];
    if let Some(head) = head.as_deref() {
        commit_args.push("-p");
        commit_args.push(head);
    }
    let commit = git(None, &commit_args)
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let refname = format!("refs/vibedev/checkpoints/{safe_id}");
    let _ = git(None, &["update-ref", &refname, &commit]);
    Some(commit)
}

/// Restore the REAL working tree's tracked files to the state captured in `git_sha` (a checkpoint
/// side-ref commit) via `git restore --source=<sha> --worktree -- .`. CONSERVATIVE + safe: it
/// reverts files present in the checkpoint but does NOT delete files added after it, and does NOT
/// touch the index/branch. Callers MUST `git_side_ref_snapshot` the CURRENT state first, so the
/// rewind is itself undoable. Returns true on success.
pub fn git_restore_to(real_path: &std::path::Path, git_sha: &str) -> bool {
    use std::process::Command;
    if git_sha.trim().is_empty() {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(real_path)
        .args(["restore", "--source", git_sha, "--worktree", "--", "."])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pending_pi_resume_path(scope_root: &std::path::Path, task_id: &str) -> PathBuf {
    let safe: String = task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    scope_root
        .join("checkpoints")
        .join(".pending_pi_resume")
        .join(safe)
}

/// Engine-tagged one-shot resume queued by a checkpoint revert. Cross-engine
/// consumers leave the marker on disk instead of binding a Pi id onto
/// Grok/Codex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEngineResume {
    pub engine: String,
    pub native_session_id: String,
}

/// One-shot marker: "the next coding turn for this run should resume session X"
/// on the named engine — written by a checkpoint revert, consumed by
/// `run_coding_task`, so the agent's context rewinds with the code.
pub fn set_pending_engine_resume(
    scope_root: &std::path::Path,
    task_id: &str,
    engine: &str,
    native_session_id: &str,
) -> std::io::Result<()> {
    let path = pending_pi_resume_path(scope_root, task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let engine = engine.trim();
    let native_session_id = native_session_id.trim();
    if native_session_id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pending resume requires a native session id",
        ));
    }
    let record = PendingEngineResume {
        engine: if engine.is_empty() {
            "pi".to_string()
        } else {
            engine.to_string()
        },
        native_session_id: native_session_id.to_string(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    // Durable rather than a plain `fs::write`: the consumer reads this marker
    // once and deletes it, so a torn write hands `run_coding_task` a truncated
    // session id and the agent's context rewinds to the wrong place. The
    // marker is written by a revert, which is exactly when a crash is likely.
    write_bytes_durably_sync(&path, &bytes)
}

/// Legacy Pi-only writer. New callers should use [`set_pending_engine_resume`].
pub fn set_pending_pi_resume(
    scope_root: &std::path::Path,
    task_id: &str,
    session_id: &str,
) -> std::io::Result<()> {
    set_pending_engine_resume(scope_root, task_id, "pi", session_id)
}

/// Take (read + delete) the pending engine-resume marker for this run, if any.
/// One-shot. A legacy plain session-id file hydrates as Pi.
pub fn take_pending_engine_resume(
    scope_root: &std::path::Path,
    task_id: &str,
) -> Option<PendingEngineResume> {
    consume_pending_engine_resume(scope_root, task_id, None)
}

/// Take the pending resume only when it is tagged for `engine`. A foreign
/// marker stays on disk so a later same-engine turn can still rewind.
pub fn take_pending_engine_resume_for(
    scope_root: &std::path::Path,
    task_id: &str,
    engine: &str,
) -> Option<PendingEngineResume> {
    consume_pending_engine_resume(scope_root, task_id, Some(engine))
}

fn consume_pending_engine_resume(
    scope_root: &std::path::Path,
    task_id: &str,
    wanted_engine: Option<&str>,
) -> Option<PendingEngineResume> {
    let path = pending_pi_resume_path(scope_root, task_id);
    let contents = std::fs::read_to_string(&path).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let parsed = if let Ok(parsed) = serde_json::from_str::<PendingEngineResume>(trimmed) {
        let native = parsed.native_session_id.trim().to_string();
        if native.is_empty() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let engine = parsed.engine.trim();
        PendingEngineResume {
            engine: if engine.is_empty() {
                "pi".to_string()
            } else {
                engine.to_string()
            },
            native_session_id: native,
        }
    } else {
        PendingEngineResume {
            engine: "pi".to_string(),
            native_session_id: trimmed.to_string(),
        }
    };
    if let Some(wanted) = wanted_engine {
        if !pending_engine_name_matches(&parsed.engine, wanted) {
            return None;
        }
    }
    let _ = std::fs::remove_file(&path);
    Some(parsed)
}

fn pending_engine_name_matches(got: &str, wanted: &str) -> bool {
    let got = got.trim();
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return true;
    }
    if got.is_empty() {
        return wanted.eq_ignore_ascii_case("pi");
    }
    got.eq_ignore_ascii_case(wanted)
}

/// Take (read + delete) the pending Pi-resume session id for this run, if any. One-shot.
pub fn take_pending_pi_resume(scope_root: &std::path::Path, task_id: &str) -> Option<String> {
    take_pending_engine_resume_for(scope_root, task_id, "pi").map(|item| item.native_session_id)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn cp(id: &str, task: &str, kind: CheckpointKind) -> Checkpoint {
        Checkpoint {
            id: id.to_string(),
            name: format!("change {id}"),
            kind,
            proposal_id: (kind == CheckpointKind::Major).then(|| id.to_string()),
            git_sha: Some("deadbeef".to_string()),
            snapshot_id: Some("snap-1".to_string()),
            engine: Some("pi".to_string()),
            native_session_id: Some("sess-1".to_string()),
            pi_session_id: Some("sess-1".to_string()),
            applied_files: vec![PathBuf::from("a.rs")],
            repo_path: Some(PathBuf::from("/repo")),
            task_id: task.to_string(),
            execution_id: Some("exec-1".to_string()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn create_get_exists_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ckpt-test-{}", std::process::id()));
        let store = CheckpointStore::new(&dir);
        assert!(!store.exists("ccp-1"));
        store
            .create(&cp("ccp-1", "task-a", CheckpointKind::Major))
            .unwrap();
        assert!(store.exists("ccp-1")); // dedup gate
        let got = store.get("ccp-1").unwrap().expect("present");
        assert_eq!(got.proposal_id.as_deref(), Some("ccp-1"));
        assert_eq!(got.git_sha.as_deref(), Some("deadbeef"));
        assert!(store.get("missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_for_task_filters_and_sorts_newest_first() {
        let dir = std::env::temp_dir().join(format!("ckpt-list-{}", std::process::id()));
        let store = CheckpointStore::new(&dir);
        let mut a = cp("ccp-old", "task-a", CheckpointKind::Major);
        a.created_at = Utc::now() - chrono::Duration::seconds(60);
        store.create(&a).unwrap();
        store
            .create(&cp("ccp-new", "task-a", CheckpointKind::Major))
            .unwrap();
        store
            .create(&cp("ccp-other", "task-b", CheckpointKind::Major))
            .unwrap();
        let list = store.list_for_task("task-a");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ccp-new"); // newest first
        assert_eq!(list[1].id, "ccp-old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `list_all` walks the checkpoints directory, so a staging sibling left
    /// behind is a file the rail has to step over on every listing. Create is
    /// overwrite-by-id, so a re-create must not accumulate them either.
    #[test]
    fn create_publishes_without_leaving_a_staging_sibling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = CheckpointStore::new(temp.path());

        store
            .create(&cp("ccp-1", "task-a", CheckpointKind::Major))
            .expect("first create");
        let mut renamed = cp("ccp-1", "task-a", CheckpointKind::Major);
        renamed.name = "renamed".to_string();
        store.create(&renamed).expect("overwrite by id");

        let published = store.get("ccp-1").expect("read back").expect("present");
        assert_eq!(published.name, "renamed");

        let entries = std::fs::read_dir(temp.path().join("checkpoints"))
            .expect("checkpoints listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec!["ccp-1.json".to_string()],
            "the checkpoint record must be the only file in the directory"
        );
    }

    #[test]
    fn resume_session_prefers_native_and_hydrates_legacy_pi() {
        let mut current = cp("ccp-1", "task-a", CheckpointKind::Major);
        assert_eq!(current.resume_session_id(), Some("sess-1"));

        current.native_session_id = Some("native-9".to_string());
        current.pi_session_id = Some("sess-1".to_string());
        assert_eq!(current.resume_session_id(), Some("native-9"));

        current.native_session_id = Some(String::new());
        assert_eq!(current.resume_session_id(), Some("sess-1"));

        let legacy: Checkpoint = serde_json::from_str(
            r#"{
                "id": "ccp-old",
                "name": "old",
                "kind": "major",
                "pi_session_id": "legacy-pi",
                "task_id": "task-a",
                "created_at": "2026-08-01T00:00:00Z"
            }"#,
        )
        .expect("legacy checkpoint");
        assert_eq!(legacy.engine, None);
        assert_eq!(legacy.native_session_id, None);
        assert_eq!(legacy.resume_session_id(), Some("legacy-pi"));
    }

    /// The Pi-resume marker is read once and deleted; a torn write hands the
    /// next coding turn a truncated session id.
    #[test]
    fn pending_pi_resume_marker_publishes_without_a_staging_sibling() {
        let temp = tempfile::tempdir().expect("tempdir");
        set_pending_pi_resume(temp.path(), "task-a", "sess-1").expect("marker write");
        set_pending_pi_resume(temp.path(), "task-a", "sess-2").expect("marker overwrite");

        let marker_dir = temp.path().join("checkpoints").join(".pending_pi_resume");
        let entries = std::fs::read_dir(&marker_dir)
            .expect("marker listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["task-a".to_string()]);
        assert_eq!(
            take_pending_pi_resume(temp.path(), "task-a").as_deref(),
            Some("sess-2")
        );
    }

    #[test]
    fn pending_resume_is_engine_tagged_and_legacy_plain_text_is_pi() {
        let temp = tempfile::tempdir().expect("tempdir");
        set_pending_engine_resume(temp.path(), "task-a", "grok_acp", "sess-acp")
            .expect("grok marker");
        assert_eq!(
            take_pending_engine_resume(temp.path(), "task-a"),
            Some(PendingEngineResume {
                engine: "grok_acp".to_string(),
                native_session_id: "sess-acp".to_string(),
            })
        );

        let path = pending_pi_resume_path(temp.path(), "task-b");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "legacy-pi").expect("legacy write");
        assert_eq!(
            take_pending_engine_resume(temp.path(), "task-b"),
            Some(PendingEngineResume {
                engine: "pi".to_string(),
                native_session_id: "legacy-pi".to_string(),
            })
        );
    }

    #[test]
    fn take_pending_engine_resume_for_leaves_a_foreign_engine_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        set_pending_engine_resume(temp.path(), "task-a", "grok_acp", "sess-acp")
            .expect("grok marker");
        assert!(
            take_pending_engine_resume_for(temp.path(), "task-a", "pi").is_none(),
            "Pi must not consume a Grok pending resume"
        );
        assert_eq!(
            take_pending_engine_resume_for(temp.path(), "task-a", "grok_acp"),
            Some(PendingEngineResume {
                engine: "grok_acp".to_string(),
                native_session_id: "sess-acp".to_string(),
            })
        );
        assert!(take_pending_engine_resume(temp.path(), "task-a").is_none());
    }
}
