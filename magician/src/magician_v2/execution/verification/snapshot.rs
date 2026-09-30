//! Immutable input snapshot, ephemeral writable workspace.
//!
//! "Immutable, and evidence is rejected if it changes" is self-contradictory
//! as written: checks must write `target/`, coverage data and generated files.
//! The resolution is two objects rather than one:
//!
//! ```text
//! content-addressed IMMUTABLE source snapshot   ← what the attestation keys on
//!         ↓ materialise
//! ephemeral WRITABLE verification workspace
//!         ↓
//! external cache mounts where policy permits
//! ```
//!
//! Evidence references the immutable **input** digest. **Mutation of tracked
//! source by a check invalidates the run; declared build outputs do not.**
//!
//! The distinction matters for speed as much as correctness: this repository
//! needs `CARGO_TARGET_DIR` on the SSD and a warm `target/`, and treating
//! those as tracked source would either invalidate every run or force a cold
//! build — turning a 7-minute check into a 40-minute one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// Directories never captured as source. These are build output or dependency
/// caches: enormous, regenerable, and not what "did the code change?" means.
const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "target",
    ".build", // SwiftPM scratch; sibling of `target` and just as large
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "dist",
    "build",
    ".svelte-kit",
    ".next",
    ".turbo",
    ".gradle",
    "DerivedData",
    ".cargo",
    ".sccache",
];

/// Runaway guard on a single captured file.
///
/// This was 32 MiB when hashing read whole files into memory. [`hash_file`]
/// streams, so the memory reason is gone and the limit only needs to catch
/// something pathological — a repository legitimately containing a large
/// fixture or checked-in binary must not fail verification outright, which is
/// what a tight cap here would do.
const MAX_SNAPSHOT_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Cap on total entries. A snapshot is taken per verification pass, so a
/// runaway tree must fail with a message rather than consuming memory until
/// the process dies partway through a run.
const MAX_SNAPSHOT_ENTRIES: usize = 200_000;

/// Streaming hash buffer. One of these exists at a time, regardless of how
/// many files the walk visits.
const HASH_BUFFER_BYTES: usize = 128 * 1024;

/// How a path was materialised, recorded so the policy question "how are
/// symlinks and submodules handled?" has one answer rather than per-call
/// improvisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    /// Symlinks are captured by their *target text*, not by following them. A
    /// followed symlink could pull content from outside the snapshot root into
    /// the digest, which would make the key depend on things the snapshot does
    /// not contain.
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub kind: EntryKind,
    /// blake3 of the file contents, or of the link target text.
    pub digest: String,
    pub size: u64,
    /// Unix mode bits that affect execution. Captured because a check that
    /// depends on a script being executable would otherwise pass or fail based
    /// on something outside the digest.
    pub executable: bool,
}

/// A content-addressed view of the source that was verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Relative path → entry. `BTreeMap` so iteration and the digest are
    /// deterministic regardless of directory-read order.
    pub entries: BTreeMap<String, SnapshotEntry>,
    /// Directories excluded when capturing.
    pub excluded: Vec<String>,
}

impl SourceSnapshot {
    /// Capture the tracked source under `root`.
    pub fn capture(root: &Path, extra_excludes: &[String]) -> Result<Self> {
        let mut excluded: BTreeSet<String> = DEFAULT_EXCLUDED_DIRS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        excluded.extend(extra_excludes.iter().cloned());

        let mut entries = BTreeMap::new();
        capture_dir(root, root, &excluded, &mut entries)?;

        Ok(Self {
            entries,
            excluded: excluded.into_iter().collect(),
        })
    }

    /// The content address the attestation keys on.
    pub fn digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for (path, entry) in &self.entries {
            hasher.update(path.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(entry.digest.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(if entry.executable { b"x" } else { b"-" });
            hasher.update(b"\x1f");
            hasher.update(match entry.kind {
                EntryKind::File => b"f",
                EntryKind::Symlink => b"l",
            });
            hasher.update(b"\x1e");
        }
        hasher.finalize().to_hex().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Walk the tree with an explicit worklist rather than recursion.
///
/// Recursing per directory level puts the traversal depth on the call stack,
/// and a snapshot root is arbitrary user content — a deeply nested generated
/// tree would abort the process rather than return an error. Depth here costs
/// heap instead.
///
/// [`MAX_SNAPSHOT_ENTRIES`] bounds **both** things this walk accumulates: the
/// captured entries *and* the pending worklist. Checking only `out.len()` left
/// directories entirely uncounted — a tree of nothing but empty directories
/// contributes no entries at all, so the walk grew the worklist without limit
/// and the bound this comment credits did not exist for the shape that needed
/// it most.
fn capture_dir(
    root: &Path,
    dir: &Path,
    excluded: &BTreeSet<String>,
    out: &mut BTreeMap<String, SnapshotEntry>,
) -> Result<()> {
    let mut worklist: Vec<PathBuf> = vec![dir.to_path_buf()];

    while let Some(dir) = worklist.pop() {
        let read = std::fs::read_dir(&dir)
            .with_context(|| format!("read snapshot dir {}", dir.display()))?;
        for entry in read {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();

            let meta = std::fs::symlink_metadata(&path)
                .with_context(|| format!("stat {}", path.display()))?;

            if meta.is_dir() {
                if excluded.contains(&name) {
                    continue;
                }
                worklist.push(path);
            } else {
                capture_file(root, &path, &meta, out)?;
            }

            // A runaway tree should fail with a message, not by exhausting
            // memory partway through a verification run.
            //
            // Counted over entries **and** queued directories. Counting only
            // entries meant a directory cost nothing, so a tree of empty
            // directories — a generated cache, a corrupt checkout — grew the
            // worklist unbounded and the walk died of memory exhaustion
            // instead of returning this message.
            if out.len() + worklist.len() > MAX_SNAPSHOT_ENTRIES {
                return Err(anyhow!(
                    "snapshot exceeds {MAX_SNAPSHOT_ENTRIES} entries under {}; \
                     narrow the snapshot root or add exclusions",
                    root.display()
                ));
            }
        }
    }
    Ok(())
}

/// Capture one non-directory entry.
fn capture_file(
    root: &Path,
    path: &Path,
    meta: &std::fs::Metadata,
    out: &mut BTreeMap<String, SnapshotEntry>,
) -> Result<()> {
    let rel = relative_path(root, path)?;

    if meta.file_type().is_symlink() {
        // Capture the link *text*. Following it would let content outside the
        // root influence the digest.
        let target =
            std::fs::read_link(path).with_context(|| format!("read link {}", path.display()))?;
        let text = target.to_string_lossy();
        out.insert(
            rel,
            SnapshotEntry {
                kind: EntryKind::Symlink,
                digest: blake3::hash(text.as_bytes()).to_hex().to_string(),
                size: text.len() as u64,
                executable: false,
            },
        );
        return Ok(());
    }

    if meta.len() > MAX_SNAPSHOT_FILE_BYTES {
        return Err(anyhow!(
            "{} is {} bytes; exceeds the {MAX_SNAPSHOT_FILE_BYTES}-byte snapshot limit",
            path.display(),
            meta.len()
        ));
    }

    out.insert(
        rel,
        SnapshotEntry {
            kind: EntryKind::File,
            digest: hash_file(path)?,
            size: meta.len(),
            executable: is_executable(meta),
        },
    );
    Ok(())
}

/// Hash a file without holding it in memory.
///
/// `fs::read` would allocate the whole file per entry, and a snapshot walks
/// thousands of files. Streaming keeps peak memory at one
/// [`HASH_BUFFER_BYTES`] buffer regardless of file or tree size.
fn hash_file(path: &Path) -> Result<String> {
    use std::io::Read;

    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = std::io::BufReader::with_capacity(HASH_BUFFER_BYTES, file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; HASH_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
    // Normalise to forward slashes so a snapshot digest is stable across
    // platforms, and refuse anything that could escape the root.
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {},
            other => {
                return Err(anyhow!(
                    "unexpected path component {other:?} in {}",
                    path.display()
                ))
            },
        }
    }
    Ok(parts.join("/"))
}

/// Result of checking whether a run stayed within its contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationReport {
    /// Tracked source files a check added, changed or deleted. Any entry here
    /// invalidates the run: the evidence would describe an input that no
    /// longer matches what was keyed.
    pub tracked_mutations: Vec<String>,
    /// Paths that changed but are declared build outputs. Expected, ignored.
    pub declared_output_changes: Vec<String>,
}

impl MutationReport {
    pub fn invalidates_run(&self) -> bool {
        !self.tracked_mutations.is_empty()
    }
}

/// Suffix of the marker file that says a workspace directory is owned.
const LEASE_SUFFIX: &str = ".lease";

/// The marker path for a workspace. A **sibling**, never a child.
///
/// Inside the workspace the marker would be captured by
/// [`EphemeralWorkspace::detect_mutations`] as a file that appeared during the
/// run, and every single pass would come back `indeterminate`.
fn lease_path(workspace: &Path) -> Result<PathBuf> {
    let name = workspace
        .file_name()
        .ok_or_else(|| {
            anyhow!(
                "workspace path {} has no final component",
                workspace.display()
            )
        })?
        .to_string_lossy()
        .to_string();
    let parent = workspace
        .parent()
        .ok_or_else(|| anyhow!("workspace path {} has no parent", workspace.display()))?;
    Ok(parent.join(format!("{name}{LEASE_SUFFIX}")))
}

/// Proof that a live pass owns a workspace directory.
///
/// An exclusive advisory lock (`flock`) on the workspace's marker file, held
/// for the pass's whole lifetime.
///
/// `Drop` is not enough on its own, which is the entire reason this exists.
/// SIGKILL, an OOM kill and a power cut all run no destructor, so a stranded
/// full copy of a repository has to be distinguishable from a live one by
/// something that survives the owning process — and an advisory lock is
/// released by the kernel however the holder dies. That is what makes
/// [`reap_stale_workspaces`] safe to run while other passes are working.
pub struct WorkspaceLease {
    path: PathBuf,
    file: std::fs::File,
}

impl WorkspaceLease {
    /// Take the marker for `workspace`, or fail because a live pass holds it.
    ///
    /// Deliberately taken **before** the directory is created, so a reaper
    /// that can see the directory at all can already see the lock. There is no
    /// window in which a half-built workspace looks abandoned.
    pub fn acquire(workspace: &Path) -> Result<Self> {
        let path = lease_path(workspace)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create workspace root {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open workspace lease {}", path.display()))?;
        file.try_lock_exclusive().map_err(|error| {
            anyhow!(
                "verification workspace {} is owned by a live pass: {error}",
                workspace.display()
            )
        })?;
        Ok(Self { path, file })
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Remove every workspace under `workspace_root` that no live pass owns.
///
/// Returns what was removed. This is the half of cleanup `Drop` cannot do:
/// `Drop` covers an orderly exit, and the failures that strand a full repository
/// copy on disk — SIGKILL, OOM, power loss — are precisely the ones that run no
/// destructor.
///
/// **A live pass cannot be reaped out from under itself.** A directory is only
/// removed when this can take its lease *exclusively*, and a live pass holds
/// that lease from before its directory exists until after its last read of it.
/// The lock is kernel state, so this stays true across threads, across
/// processes, and across a reaper that starts mid-pass.
///
/// Workspace names are unique per pass and never reused, so a reaper holding
/// the marker of a name it is deleting cannot be racing a *different* pass that
/// wants the same name.
pub fn reap_stale_workspaces(workspace_root: &Path) -> Vec<PathBuf> {
    let mut reaped = Vec::new();
    let Ok(entries) = std::fs::read_dir(workspace_root) else {
        // No workspace root yet is the normal case on a first pass.
        return reaped;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Marker files are siblings of the directories they describe; only
        // directories are workspaces.
        //
        // A marker whose directory is already gone is deliberately left alone.
        // See `WorkspaceLease` — unlinking a marker is what breaks the identity
        // this reaper depends on, and the debris is a zero-byte file.
        if !path.is_dir() {
            continue;
        }
        // Taking the lease *is* the liveness test.
        let Ok(lease) = WorkspaceLease::acquire(&path) else {
            continue;
        };
        if std::fs::remove_dir_all(&path).is_ok() {
            reaped.push(path);
        }
        // Releases the lock and unlinks the marker.
        drop(lease);
    }
    reaped
}

/// A writable materialisation of a [`SourceSnapshot`].
///
/// Dropping it removes the directory, so a crashed or cancelled run does not
/// leave checkouts behind. A process that dies without running destructors
/// leaves one behind anyway; [`reap_stale_workspaces`] is what collects those.
pub struct EphemeralWorkspace {
    path: PathBuf,
    snapshot: SourceSnapshot,
    declared_outputs: Vec<String>,
    cleanup: bool,
    /// Held for the workspace's lifetime. Declared last so it is released
    /// after `Drop for EphemeralWorkspace` has removed the directory.
    _lease: WorkspaceLease,
}

impl EphemeralWorkspace {
    /// Materialise `snapshot` from `source_root` into `dest`.
    ///
    /// `declared_outputs` are the policy's writable roots — paths a check is
    /// permitted to create or modify without invalidating the run.
    ///
    /// An existing directory at `dest` is **replaced, not refused**. Refusing
    /// was terminal for a gate rather than merely inconvenient: with the old
    /// deterministic path, one checkout stranded by a hard kill meant every
    /// later pass hit the same directory, settled `unavailable` — which keeps
    /// its outbox entry — and never advanced, because the candidate revision
    /// only moves on red and red requires materialising. Holding the lease
    /// proves no live pass owns the path, so anything there is debris; wiping
    /// it and copying fresh preserves the real invariant, which is that checks
    /// never see state from a previous run.
    pub fn materialise(
        snapshot: SourceSnapshot,
        source_root: &Path,
        dest: PathBuf,
        declared_outputs: Vec<String>,
    ) -> Result<Self> {
        // Before the directory exists, so a reaper never sees an unowned
        // half-built workspace.
        let lease = WorkspaceLease::acquire(&dest)?;

        if dest.exists() {
            std::fs::remove_dir_all(&dest).with_context(|| {
                format!(
                    "replace the stranded verification workspace {}",
                    dest.display()
                )
            })?;
        }
        std::fs::create_dir_all(&dest)
            .with_context(|| format!("create verification workspace {}", dest.display()))?;

        for (rel, entry) in &snapshot.entries {
            let target = dest.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            let origin = source_root.join(rel);
            match entry.kind {
                EntryKind::File => {
                    std::fs::copy(&origin, &target).with_context(|| {
                        format!("materialise {} -> {}", origin.display(), target.display())
                    })?;
                },
                EntryKind::Symlink => {
                    let link_target = std::fs::read_link(&origin)
                        .with_context(|| format!("read link {}", origin.display()))?;
                    materialise_symlink(&link_target, &target)?;
                },
            }
        }

        Ok(Self {
            path: dest,
            snapshot,
            declared_outputs,
            cleanup: true,
            _lease: lease,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }

    /// Keep the directory on drop. For debugging a failed run.
    ///
    /// It outlives the pass but not the next [`reap_stale_workspaces`]: the
    /// lease goes with the pass, and an unleased directory is exactly what the
    /// reaper exists to remove. Copy it elsewhere if it has to survive.
    pub fn persist(&mut self) {
        self.cleanup = false;
    }

    fn is_declared_output(&self, rel: &str) -> bool {
        self.declared_outputs.iter().any(|root| {
            let root = root.trim_end_matches('/');
            rel == root || rel.starts_with(&format!("{root}/"))
        })
    }

    /// Compare the workspace against the snapshot it was materialised from.
    ///
    /// This is what enforces §4.5's rule. It is deliberately run *after* the
    /// checks, not during: a check that rewrites a source file has already
    /// invalidated its own evidence, and the point is to refuse the evidence,
    /// not to police the filesystem in real time.
    pub fn detect_mutations(&self) -> Result<MutationReport> {
        let after = SourceSnapshot::capture(&self.path, &self.snapshot.excluded)?;

        let mut tracked_mutations = Vec::new();
        let mut declared_output_changes = Vec::new();

        let before_keys: BTreeSet<&String> = self.snapshot.entries.keys().collect();
        let after_keys: BTreeSet<&String> = after.entries.keys().collect();

        for rel in before_keys.union(&after_keys) {
            let before = self.snapshot.entries.get(*rel);
            let now = after.entries.get(*rel);
            if before == now {
                continue;
            }
            if self.is_declared_output(rel) {
                declared_output_changes.push((*rel).clone());
            } else {
                tracked_mutations.push((*rel).clone());
            }
        }

        tracked_mutations.sort();
        declared_output_changes.sort();
        Ok(MutationReport {
            tracked_mutations,
            declared_output_changes,
        })
    }
}

impl Drop for EphemeralWorkspace {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(unix)]
fn materialise_symlink(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at)
        .with_context(|| format!("symlink {} -> {}", at.display(), target.display()))
}

#[cfg(not(unix))]
fn materialise_symlink(_target: &Path, at: &Path) -> Result<()> {
    // Windows symlink creation needs elevation; record the link as a regular
    // file so materialisation does not fail outright.
    std::fs::write(at, b"").with_context(|| format!("placeholder for symlink {}", at.display()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_deeply_nested_tree_does_not_exhaust_the_stack() {
        // The walk was recursive per directory level, so traversal depth sat
        // on the call stack and a deep tree aborted the process instead of
        // returning.
        //
        // Depth is bounded by the platform, not by us: an absolute path over
        // PATH_MAX cannot be created *or* opened, so no deeper tree can exist
        // for the walk to trip on. Build to just under that ceiling — the
        // deepest input the walk will ever see.
        const PATH_BUDGET: usize = 900;
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().to_path_buf();
        let mut depth = 0;
        while deep.as_os_str().len() + "/leaf.txt".len() < PATH_BUDGET {
            deep = deep.join("d");
            depth += 1;
        }
        assert!(depth > 300, "expected a genuinely deep tree, got {depth}");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf.txt"), "bottom").unwrap();

        let snap = SourceSnapshot::capture(root.path(), &[]).unwrap();
        assert_eq!(snap.entries.len(), 1);
        assert!(snap.entries.keys().next().unwrap().ends_with("leaf.txt"));
    }

    #[test]
    fn a_file_larger_than_the_old_in_memory_cap_is_hashed_not_rejected() {
        // Hashing streams now, so a large checked-in fixture must not fail
        // verification outright the way the 32 MiB cap would have.
        let root = tempfile::tempdir().unwrap();
        let big = vec![7u8; 40 * 1024 * 1024];
        std::fs::write(root.path().join("fixture.bin"), &big).unwrap();

        let snap = SourceSnapshot::capture(root.path(), &[]).unwrap();
        let entry = snap.entries.get("fixture.bin").unwrap();
        assert_eq!(entry.size, big.len() as u64);
        // And the streamed digest matches a one-shot hash of the same bytes.
        assert_eq!(entry.digest, blake3::hash(&big).to_hex().to_string());
    }

    #[test]
    fn digest_is_deterministic_and_content_sensitive() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "src/main.rs", "fn main() {}");
        write(a.path(), "Cargo.toml", "[package]");

        let s1 = SourceSnapshot::capture(a.path(), &[]).unwrap();
        let s2 = SourceSnapshot::capture(a.path(), &[]).unwrap();
        assert_eq!(s1.digest(), s2.digest());

        write(a.path(), "src/main.rs", "fn main() { }");
        let s3 = SourceSnapshot::capture(a.path(), &[]).unwrap();
        assert_ne!(s1.digest(), s3.digest());
    }

    #[test]
    fn build_output_directories_are_not_part_of_the_input_digest() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "src/main.rs", "fn main() {}");
        let before = SourceSnapshot::capture(a.path(), &[]).unwrap();

        // A warm target/ and node_modules must not change what was verified —
        // otherwise no two runs on a real repo would ever share a key.
        write(a.path(), "target/debug/thing", "binary");
        write(a.path(), "node_modules/pkg/index.js", "x");
        write(a.path(), ".git/HEAD", "ref: refs/heads/main");

        let after = SourceSnapshot::capture(a.path(), &[]).unwrap();
        assert_eq!(before.digest(), after.digest());
    }

    #[test]
    fn symlinks_are_captured_by_target_text_not_followed() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "real.txt", "content");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("real.txt", a.path().join("link.txt")).unwrap();
            let snap = SourceSnapshot::capture(a.path(), &[]).unwrap();
            let link = snap.entries.get("link.txt").unwrap();
            assert_eq!(link.kind, EntryKind::Symlink);
            // The link's digest is of "real.txt", not of "content".
            assert_ne!(link.digest, snap.entries.get("real.txt").unwrap().digest);
        }
    }

    #[test]
    fn executable_bit_participates_in_the_digest() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let a = tempfile::tempdir().unwrap();
            write(a.path(), "run.sh", "#!/bin/sh\n");
            let before = SourceSnapshot::capture(a.path(), &[]).unwrap();

            let path = a.path().join("run.sh");
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();

            let after = SourceSnapshot::capture(a.path(), &[]).unwrap();
            assert_ne!(before.digest(), after.digest());
        }
    }

    #[test]
    fn materialise_reproduces_the_snapshot_into_a_writable_workspace() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        write(src.path(), "Cargo.toml", "[package]");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("checkout");
        let ws = EphemeralWorkspace::materialise(
            snap.clone(),
            src.path(),
            dest.clone(),
            vec!["target".into()],
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(ws.path().join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        // And it is writable — checks must be able to build.
        std::fs::write(ws.path().join("scratch.txt"), "x").unwrap();

        let re = SourceSnapshot::capture(ws.path(), &[]).unwrap();
        assert!(re.entries.contains_key("Cargo.toml"));
    }

    #[test]
    fn writing_a_declared_build_output_does_not_invalidate_the_run() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let ws = EphemeralWorkspace::materialise(
            snap,
            src.path(),
            dest_root.path().join("checkout"),
            vec!["coverage".into()],
        )
        .unwrap();

        // A check writes coverage data — declared, therefore fine.
        write(ws.path(), "coverage/lcov.info", "TN:");
        let report = ws.detect_mutations().unwrap();
        assert!(!report.invalidates_run());
        assert_eq!(report.declared_output_changes, vec!["coverage/lcov.info"]);
    }

    #[test]
    fn a_check_that_rewrites_tracked_source_invalidates_the_run() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let ws = EphemeralWorkspace::materialise(
            snap,
            src.path(),
            dest_root.path().join("checkout"),
            vec!["target".into()],
        )
        .unwrap();

        // A "check" that formats the code it is checking. The evidence would
        // otherwise describe an input that no longer exists.
        write(ws.path(), "src/main.rs", "fn main() {\n}\n");
        let report = ws.detect_mutations().unwrap();
        assert!(report.invalidates_run());
        assert_eq!(report.tracked_mutations, vec!["src/main.rs"]);
    }

    #[test]
    fn deleting_tracked_source_also_invalidates_the_run() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        write(src.path(), "src/lib.rs", "pub fn a() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let ws = EphemeralWorkspace::materialise(
            snap,
            src.path(),
            dest_root.path().join("checkout"),
            vec![],
        )
        .unwrap();

        std::fs::remove_file(ws.path().join("src/lib.rs")).unwrap();
        let report = ws.detect_mutations().unwrap();
        assert!(report.invalidates_run());
        assert_eq!(report.tracked_mutations, vec!["src/lib.rs"]);
    }

    #[test]
    fn adding_an_undeclared_source_file_invalidates_the_run() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let ws = EphemeralWorkspace::materialise(
            snap,
            src.path(),
            dest_root.path().join("checkout"),
            vec!["target".into()],
        )
        .unwrap();

        write(ws.path(), "src/sneaky.rs", "// added by a check");
        let report = ws.detect_mutations().unwrap();
        assert!(report.invalidates_run());
        assert_eq!(report.tracked_mutations, vec!["src/sneaky.rs"]);
    }

    #[test]
    fn a_clean_run_reports_no_mutation() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let ws = EphemeralWorkspace::materialise(
            snap,
            src.path(),
            dest_root.path().join("checkout"),
            vec!["target".into()],
        )
        .unwrap();

        // Build output inside an excluded dir is invisible to the comparison
        // entirely, which is what keeps a warm target/ cheap.
        write(ws.path(), "target/debug/app", "bin");
        let report = ws.detect_mutations().unwrap();
        assert!(!report.invalidates_run());
        assert!(report.declared_output_changes.is_empty());
    }

    #[test]
    fn a_stranded_checkout_is_replaced_rather_than_refusing_forever() {
        // The failure this replaces: a hard kill left a directory behind, and
        // refusing it was terminal for the gate — every later pass hit the
        // same path, settled `unavailable`, and the revision only advances on
        // red, which requires materialising. Nothing could ever clear it.
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "a.txt", "x");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("checkout");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("debris.txt"), "from a killed run").unwrap();

        let ws = EphemeralWorkspace::materialise(snap, src.path(), dest.clone(), vec![])
            .expect("a stranded checkout must not wedge the workspace path");

        // Replaced, not reused: the real invariant is that checks never see
        // state from a previous run.
        assert!(!dest.join("debris.txt").exists());
        assert_eq!(
            std::fs::read_to_string(ws.path().join("a.txt")).unwrap(),
            "x"
        );
    }

    #[test]
    fn a_workspace_a_live_pass_holds_is_refused() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "a.txt", "x");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("checkout");
        let _live = EphemeralWorkspace::materialise(snap.clone(), src.path(), dest.clone(), vec![])
            .unwrap();

        // Replacement is only ever safe because the lease proves nobody is
        // using the directory. A second materialisation onto a live one must
        // fail rather than delete a running pass's tree.
        assert!(
            EphemeralWorkspace::materialise(snap, src.path(), dest, vec![]).is_err(),
            "a live pass's checkout must never be replaced under it"
        );
    }

    #[test]
    fn reaping_removes_stranded_workspaces_and_spares_live_ones() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "a.txt", "x");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let root = tempfile::tempdir().unwrap();

        // What a SIGKILL leaves: a full directory and no lease.
        let stranded = root.path().join("gate-1-r1-deadbeef");
        std::fs::create_dir_all(stranded.join("src")).unwrap();
        std::fs::write(stranded.join("src/main.rs"), "fn main() {}").unwrap();

        // And a pass that is genuinely running.
        let live_path = root.path().join("gate-1-r2-cafebabe");
        let live =
            EphemeralWorkspace::materialise(snap, src.path(), live_path.clone(), vec![]).unwrap();

        let reaped = reap_stale_workspaces(root.path());
        assert_eq!(reaped, vec![stranded.clone()]);
        assert!(!stranded.exists());
        assert!(
            live_path.exists(),
            "a live pass must not be reaped out from under itself"
        );
        assert!(live.path().join("a.txt").exists());
    }

    #[test]
    fn a_marker_with_no_workspace_is_left_alone() {
        // Collecting these looks obviously right and is the bug. Unlinking a
        // marker frees its *name* while a starting pass may hold an unlocked
        // descriptor on its *inode*; the next `acquire` of that name creates a
        // fresh inode, takes an uncontended lock on it, and deletes a live
        // checkout. The debris this leaves instead is a zero-byte file.
        let root = tempfile::tempdir().unwrap();
        let orphan = root.path().join("gate-1-r1-deadbeef.lease");
        std::fs::write(&orphan, "").unwrap();

        assert!(reap_stale_workspaces(root.path()).is_empty());
        assert!(
            orphan.exists(),
            "a reaper must never unlink a marker it does not own the directory for"
        );
    }

    #[test]
    fn reaping_spares_a_marker_taken_before_its_directory_exists() {
        // The window `WorkspaceLease::acquire` opens on purpose: the lease is
        // held and the directory does not exist yet.
        let root = tempfile::tempdir().unwrap();
        let claimed = root.path().join("gate-1-r1-cafebabe");
        let lease = WorkspaceLease::acquire(&claimed).unwrap();

        let marker = root.path().join("gate-1-r1-cafebabe.lease");
        assert!(marker.exists());
        assert!(
            !claimed.exists(),
            "the directory is deliberately not made yet"
        );

        assert!(reap_stale_workspaces(root.path()).is_empty());
        assert!(
            marker.exists(),
            "a pass that holds its lease must keep it through a reap"
        );
        drop(lease);
    }

    #[test]
    fn the_lease_marker_is_a_sibling_so_it_never_reads_as_a_mutation() {
        // A marker inside the workspace would be captured by
        // `detect_mutations` as a file that appeared during the run, and every
        // pass would come back indeterminate.
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "src/main.rs", "fn main() {}");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let root = tempfile::tempdir().unwrap();
        let ws =
            EphemeralWorkspace::materialise(snap, src.path(), root.path().join("checkout"), vec![])
                .unwrap();

        assert!(root.path().join("checkout.lease").exists());
        let report = ws.detect_mutations().unwrap();
        assert!(!report.invalidates_run(), "{report:?}");
    }

    #[test]
    fn workspace_is_removed_on_drop() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "a.txt", "x");
        let snap = SourceSnapshot::capture(src.path(), &[]).unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("checkout");
        {
            let _ws =
                EphemeralWorkspace::materialise(snap, src.path(), dest.clone(), vec![]).unwrap();
            assert!(dest.exists());
        }
        assert!(!dest.exists(), "a cancelled run must not leave checkouts");
        assert!(
            !dest_root.path().join("checkout.lease").exists(),
            "the marker goes with the workspace it describes"
        );
    }
}
