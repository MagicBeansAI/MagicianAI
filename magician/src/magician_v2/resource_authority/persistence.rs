use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rust_decimal::Decimal;

use super::ledger::{Account, JournalEntry, ResourceLedger};

/// Process-local monotonic counter for tmpfile naming. Guarantees
/// uniqueness even when two `atomic_write` calls land in the same
/// nanosecond (`SystemTime::now` resolution is ~10–100ns on common
/// platforms, so concurrent persists on multi-core can collide).
/// Combined with pid + nanos in `tmp_sibling`, this gives belt-and-
/// suspenders uniqueness within the process AND disambiguation across
/// restarts that reuse the same pid.
static TMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic + durable file replacement: write to a sibling tmpfile,
/// `fsync` the tmpfile to push contents to disk, `rename` atomically
/// into place, then best-effort `fsync` the parent directory so the
/// rename itself is durable.
///
/// Crash-safe at two layers:
/// - **Concurrency / mid-write reads**: rename is atomic on POSIX so a
///   reader observes either the pre-write content (target file
///   unchanged) or the post-write content (rename done); never a
///   half-written file.
/// - **Power loss**: `sync_all` on the tmpfile flushes its contents
///   from the kernel page cache to the underlying storage before the
///   rename point. Without this, the rename could complete (a
///   metadata-only operation) while the tmpfile's data sits in
///   write-back cache — a power loss between those moments leaves
///   the target name pointing at a zero/garbage inode. The
///   parent-directory fsync after rename pushes the rename itself
///   to disk for the same reason (the rename modifies the parent
///   dir's entries, which also live in cache until flushed). Both
///   syncs are best-effort — surfacing them as warnings rather than
///   errors keeps the in-memory state authoritative on transient I/O
///   failures.
///
/// Required because `save_journal` and `TokenStore::save` are now
/// called from multiple paths (REST API + dispatch gate via
/// `ScopedAuthorityBundle::persist_state`); the previous "open +
/// truncate + write lines" pattern left both a partial-write window
/// AND a power-loss window where on-disk state could regress.
///
/// Caller is responsible for serialising concurrent writers — atomic
/// rename only guarantees consumers see a complete file, not that the
/// last writer's content wins. (For resource-authority state both
/// writers observe the same in-memory Arcs and produce
/// content-identical files, so last-writer-wins is degenerate.)
fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent();
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = tmp_sibling(path);
    // Best-effort cleanup if a previous run crashed mid-rename.
    let _ = fs::remove_file(&tmp_path);
    {
        let mut tmp_file = fs::File::create(&tmp_path)?;
        tmp_file.write_all(contents)?;
        // Push tmpfile contents to disk before the rename. If the
        // rename completes but the data hasn't been flushed, a power
        // loss could leave the target name pointing at a zero/garbage
        // inode. `sync_all` (fsync) closes that window.
        if let Err(error) = tmp_file.sync_all() {
            // Don't abort the write — sync failures on transient I/O
            // hiccups would otherwise prevent any persistence. The
            // rename below still produces atomic visibility; we only
            // lose durability against power loss between the rename
            // and the next implicit flush. Log so operators can see
            // the degraded durability if it ever fires.
            tracing::warn!(
                error = %error,
                path = %tmp_path.display(),
                "[RESOURCE-AUTHORITY] tmpfile fsync failed; rename will still produce \
                 atomic visibility but power loss between rename and next flush could \
                 lose this write"
            );
        }
    }
    fs::rename(&tmp_path, path)?;
    // Best-effort parent-directory fsync — pushes the rename itself
    // (a parent-dir metadata change) to disk. Skipped silently if the
    // parent can't be opened (e.g. root-level temp dir on some
    // platforms) since the rename atomicity is already guaranteed.
    if let Some(parent) = parent {
        if let Ok(dir) = fs::File::open(parent) {
            if let Err(error) = dir.sync_all() {
                tracing::warn!(
                    error = %error,
                    parent = %parent.display(),
                    "[RESOURCE-AUTHORITY] parent-dir fsync failed; rename is visible to \
                     readers but power loss before next implicit flush could revert it"
                );
            }
        }
    }
    Ok(())
}

/// Build a sibling path `{stem}.tmp.{pid}.{nanos}.{counter}` for the
/// tmpfile. Three layers of uniqueness:
///
/// - **pid** disambiguates between processes (different boot
///   instances), including the case where the OS recycles a pid
///   after our prior process exited.
/// - **nanos** disambiguates across reboots that reuse the same pid
///   with a fresh process-local counter (we don't persist the
///   counter — nanos covers cross-restart skew).
/// - **counter** disambiguates within the process even when two
///   `atomic_write` calls land in the same `SystemTime` tick. Critical
///   on multi-core machines where two concurrent persists can both
///   observe identical `SystemTime::now()` values.
///
/// Without the counter, two persists in the same nanosecond would
/// pick the same tmp path; one would race to `remove_file` the
/// other's mid-write tmpfile (or `File::create` would truncate it),
/// leaving an inconsistent rename result.
fn tmp_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|os| os.to_str())
        .unwrap_or("file");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{file_name}.tmp.{pid}.{nanos}.{counter}"))
}

/// Public wrapper around `atomic_write` for callers outside this
/// module that want the same crash-safe replacement semantics for
/// their own JSON state files (e.g. `TokenStore::save`).
pub fn atomic_write_bytes(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write(path, contents)
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Corrupt journal file at line {line}: {reason}")]
    CorruptFile { line: usize, reason: String },
}

/// What a journal replay produced, and what it had to do to produce it.
///
/// The reader returns this instead of a bare [`ResourceLedger`] on purpose: a
/// caller must not be able to confuse "replayed clean" with "replayed after
/// dropping bytes off the end". For a spend ledger the difference is money.
#[derive(Debug)]
pub struct JournalReplay {
    /// The ledger reconstructed from every COMMITTED journal record.
    pub ledger: ResourceLedger,
    /// Bytes discarded from an unterminated trailing fragment. Non-zero means
    /// the process died mid-append; the fragment was never newline-committed,
    /// so no spend was lost by dropping it — but the caller should say so out
    /// loud rather than pretend the file was pristine.
    pub torn_tail_bytes: usize,
}

impl JournalReplay {
    fn clean(ledger: ResourceLedger) -> Self {
        Self {
            ledger,
            torn_tail_bytes: 0,
        }
    }
}

/// Length of the newline-terminated prefix — the only part of an append-only
/// JSONL file that was ever committed.
///
/// [`append_journal`] writes `record` + `\n` and then fsyncs, so a record is
/// durable only once its terminating newline is on disk. Bytes after the last
/// newline are a torn append: begun, never acknowledged, never observed by any
/// reader as a record. Same rule the LLM trace journal uses
/// (`analytics/llm_trace_journal.rs::complete_jsonl_prefix_len`); this is that
/// scheme, not a new one.
fn committed_jsonl_prefix_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return bytes.len();
    }
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// Rebuild account balances by folding every ledger entry in the journal.
fn accounts_from_journal(journal: &[JournalEntry]) -> HashMap<String, Account> {
    let mut accounts: HashMap<String, Account> = HashMap::new();
    for je in journal {
        for le in &je.entries {
            let account = accounts
                .entry(le.account.clone())
                .or_insert_with(|| Account {
                    id: le.account.clone(),
                    commodity: le.amount.commodity.clone(),
                    cached_balance: Decimal::ZERO,
                });
            account.cached_balance += le.amount.value;
        }
    }
    accounts
}

/// Save all journal entries to a JSONL file (one JSON object per line).
/// This is a full write — used for initial save or when starting fresh.
///
/// Atomic on disk: serialises the full journal into a buffer in memory,
/// then writes-then-renames via `atomic_write`. Readers and crash
/// recovery never observe a partial file. Callers must still serialise
/// concurrent writers if they need last-writer-wins ordering; rename
/// only protects against partial writes, not write/write races.
pub fn save_journal(ledger: &ResourceLedger, path: &Path) -> Result<(), PersistenceError> {
    let buffer = journal_to_jsonl_bytes(ledger)?;
    atomic_write(path, &buffer)?;
    Ok(())
}

pub fn journal_to_jsonl_bytes(ledger: &ResourceLedger) -> Result<Vec<u8>, PersistenceError> {
    let mut buffer = Vec::new();
    for entry in &ledger.journal {
        let line = serde_json::to_string(entry)?;
        buffer.extend_from_slice(line.as_bytes());
        buffer.push(b'\n');
    }
    Ok(buffer)
}

/// Append new journal entries to an existing JSONL file.
/// `start_index` is the index of the first new entry to append.
///
/// **Durability.** A spend record that returns `Ok` here has been fsynced. The
/// previous implementation `writeln!`'d each entry and called `flush()`, which
/// only pushes the buffer into the kernel — the bytes then sit in the page
/// cache and a power loss loses them, or worse, tears the last record in half.
/// A torn record is what the reader has to defend against, so the writer owes
/// the reader the fsync. Same durability contract as this module's
/// `atomic_write` and `artifact_v2::io`'s writers (`sync_all` + parent-dir sync);
/// unlike them, the fsync is a hard error rather than a warning, because the
/// whole point of the call is "this spend is now on disk".
///
/// The batch is serialized into one buffer and written with a single
/// `write_all`, so a serialization failure part-way through the batch cannot
/// leave a half-batch on disk.
pub fn append_journal(
    ledger: &ResourceLedger,
    path: &Path,
    start_index: usize,
) -> Result<(), PersistenceError> {
    let mut buffer = Vec::new();
    for entry in ledger.journal.iter().skip(start_index) {
        let line = serde_json::to_string(entry)?;
        buffer.extend_from_slice(line.as_bytes());
        buffer.push(b'\n');
    }
    if buffer.is_empty() {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&buffer)?;
    file.flush()?;
    file.sync_all()?;

    // Best-effort parent-directory fsync — only matters when this call CREATED
    // the file (the directory entry itself lives in cache until flushed). The
    // record bytes are already durable at this point, so a failure here is a
    // warning, not an error.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            if let Err(error) = dir.sync_all() {
                tracing::warn!(
                    error = %error,
                    parent = %parent.display(),
                    "[RESOURCE-AUTHORITY] parent-dir fsync failed after journal append; the \
                     appended records are durable but a newly created journal file's directory \
                     entry may not survive power loss"
                );
            }
        }
    }
    Ok(())
}

/// Replay a ledger from raw journal bytes.
///
/// ## Trailing vs interior damage
/// These are NOT the same failure and are not treated the same way.
///
/// - An unterminated **trailing** fragment was never committed: the writer
///   fsyncs only after the record's newline, so a reader has never been
///   entitled to see those bytes as a record and no acknowledged spend is in
///   them. It is dropped, and the count of dropped bytes is reported.
/// - Anything inside the committed (newline-terminated) prefix that will not
///   parse is a **hard error**. Skipping it would silently understate spend —
///   the ledger would replay with a reserve but no commit, or with a whole
///   agent's expense missing, and the gate would then hand out budget that has
///   already been consumed. Understating spend is the expensive direction, so
///   the reader refuses to guess and makes the caller deal with it.
pub fn load_journal_from_bytes(bytes: &[u8]) -> Result<JournalReplay, PersistenceError> {
    let committed_len = committed_jsonl_prefix_len(bytes);
    let torn_tail_bytes = bytes.len() - committed_len;

    let mut journal = Vec::new();
    for (line_index, line) in bytes[..committed_len]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        let trimmed = trim_ascii_whitespace(line);
        if trimmed.is_empty() {
            continue;
        }
        let entry: JournalEntry =
            serde_json::from_slice(trimmed).map_err(|e| PersistenceError::CorruptFile {
                line: line_index + 1,
                reason: e.to_string(),
            })?;
        journal.push(entry);
    }

    Ok(JournalReplay {
        ledger: ResourceLedger {
            accounts: accounts_from_journal(&journal),
            journal,
            active_reservations: HashMap::new(),
            period_closes: Vec::new(),
        },
        torn_tail_bytes,
    })
}

/// Load a ledger from a JSONL file. Reconstructs accounts and cached_balance
/// from journal entries. A missing file is a fresh, empty ledger; see
/// [`load_journal_from_bytes`] for how damage is classified.
pub fn load_journal(path: &Path) -> Result<JournalReplay, PersistenceError> {
    match fs::read(path) {
        Ok(bytes) => load_journal_from_bytes(&bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(JournalReplay::clean(ResourceLedger::new()))
        },
        Err(error) => Err(error.into()),
    }
}

// NOTE: there is deliberately no `load_journal_from_str`. Reading the journal as
// a `String` first means a torn append that splits a multi-byte character fails
// UTF-8 decoding before this module ever sees it, turning a recoverable torn tail
// into an unrecoverable read error. Callers pass bytes.

/// `[u8]::trim_ascii` equivalent that does not depend on the stabilised
/// inherent method being available on this toolchain.
fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end]
}
