//! The restorable archive for user memory that leaves `knowledge.json`.
//!
//! `memory/users/archive/<YYYY-MM>.jsonl`: one line per record, holding the
//! whole record plus why and when it left. Files are only ever appended to.
//! Months older than `archive.compress_after_months` are rewritten once into
//! `<YYYY-MM>.jsonl.zip` (verified byte-for-byte before the plain file is
//! removed); readers handle both. Nothing here deletes a record.
//!
//! A batch costs one flush, not one per line: the caller hands over every
//! record a write moves out, and they land with a single `sync_all`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::agents::storage::AgentStorage;

pub const ARCHIVE_DIR: &str = "archive";
pub const LEARNING_PROMOTIONS_FILE: &str = "learning_promotions.jsonl";
const PLAIN_SUFFIX: &str = ".jsonl";
const ZIP_SUFFIX: &str = ".jsonl.zip";

/// `from_section` prefix for rows that left an activity-log lane file.
pub const LANE_SECTION_PREFIX: &str = "lane:";

/// One archived record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub archived_at: String,
    /// Why it left: `restated_by_producer`, `merged_duplicate`, `section_cap`,
    /// `review_queue_cap`, `retention`, `lane_window`, `lane_row_cap`, `lane_row_replaced`,
    /// `learning_promotion_replaced`, `migration_lane_slice`, ...
    pub reason: String,
    /// The top-level section it came from, or `lane:<name>`.
    pub from_section: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    /// `memory_record_id` when the record has one, else a content digest.
    pub record_id: String,
    pub record: Value,
}

impl ArchiveEntry {
    pub fn new(
        from_section: &str,
        reason: &str,
        record: Value,
        replaced_by: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            archived_at: now.to_rfc3339(),
            reason: reason.to_string(),
            from_section: from_section.to_string(),
            replaced_by,
            record_id: archive_record_id(from_section, &record),
            record,
        }
    }
}

/// The id a record is archived (and restored) under.
pub fn archive_record_id(from_section: &str, record: &Value) -> String {
    if let Some(id) = record
        .get("memory_record_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
    crate::magician_v2::agents::memory_lifecycle::digest(&json!([from_section, record]))
}

pub fn archive_dir(user_root: &Path) -> PathBuf {
    user_root.join(ARCHIVE_DIR)
}

fn month_path(user_root: &Path, at: DateTime<Utc>) -> PathBuf {
    archive_dir(user_root).join(format!("{:04}-{:02}{PLAIN_SUFFIX}", at.year(), at.month()))
}

/// Append `entries` to this month's archive file with one flush. Returns how
/// many lines were written. An empty batch touches nothing.
pub async fn append_batch(
    user_root: &Path,
    entries: &[ArchiveEntry],
    now: DateTime<Utc>,
    compress_after_months: u32,
) -> std::io::Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    let mut lines = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut lines, entry).map_err(std::io::Error::other)?;
        lines.push(b'\n');
    }
    append_bytes(&month_path(user_root, now), lines).await?;
    // Compression runs on its own task: callers append while holding the
    // knowledge or lane lock, and must not hold it through a rewrite.
    let root = user_root.to_path_buf();
    tokio::spawn(async move {
        maybe_compress_old_months(&root, now, compress_after_months).await;
    });
    Ok(entries.len())
}

/// Append raw JSON values as lines to `archive/<file_name>` (the learning
/// promotions audit). One flush per call.
pub async fn append_values(
    user_root: &Path,
    file_name: &str,
    values: &[Value],
) -> std::io::Result<usize> {
    if values.is_empty() {
        return Ok(0);
    }
    let mut lines = Vec::new();
    for value in values {
        serde_json::to_writer(&mut lines, value).map_err(std::io::Error::other)?;
        lines.push(b'\n');
    }
    append_bytes(&archive_dir(user_root).join(file_name), lines).await?;
    Ok(values.len())
}

async fn append_bytes(path: &Path, bytes: Vec<u8>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Serialize appenders: two writers must never interleave a line.
    let flock = AgentStorage::acquire_file_lock_exclusive(path)
        .await
        .map_err(std::io::Error::other)?;
    let path = path.to_path_buf();
    let permit = magician_core::blocking_admission::acquire_blocking_admission()
        .await
        .map_err(std::io::Error::other)?;
    magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
        let _flock = flock;
        append_bytes_blocking(&path, &bytes)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn append_bytes_blocking(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    // A crash mid-append can leave a torn last line. Start on a fresh line so
    // the new records are not fused onto it (the torn line is skipped on read).
    let len = file.metadata()?.len();
    let mut prefix = Vec::new();
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            prefix.push(b'\n');
        }
    }
    prefix.extend_from_slice(bytes);
    file.write_all(&prefix)?;
    file.sync_all()?;
    magician_core::durable_io::sync_parent_dir_blocking(path)
}

/// Every archive month file, oldest first (plain and compressed).
pub fn month_files(user_root: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(archive_dir(user_root)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| month_of(path).is_some())
        .collect();
    files.sort();
    files
}

/// `(year, month)` of a `YYYY-MM.jsonl[.zip]` path.
fn month_of(path: &Path) -> Option<(i32, u32)> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(ZIP_SUFFIX)
        .or_else(|| name.strip_suffix(PLAIN_SUFFIX))?;
    let (year, month) = stem.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    let month: u32 = month.parse().ok()?;
    (1..=12)
        .contains(&month)
        .then_some(())
        .and(year.parse().ok().map(|year| (year, month)))
}

/// Read one archive file. Lines that do not parse (a torn last line after a
/// crash) are skipped and counted, never fatal: earlier lines still read.
pub fn read_file(path: &Path) -> std::io::Result<(Vec<ArchiveEntry>, usize)> {
    let bytes = if path.to_string_lossy().ends_with(ZIP_SUFFIX) {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
        let mut inner = zip.by_index(0).map_err(std::io::Error::other)?;
        let mut bytes = Vec::new();
        inner.read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(path)?
    };
    let mut entries = Vec::new();
    let mut skipped = 0;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<ArchiveEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        warn!(
            path = %path.display(),
            skipped,
            "user memory archive has unreadable lines; they were skipped"
        );
    }
    Ok((entries, skipped))
}

/// The most recent archive entry for `record_id`, newest month first.
pub fn find_latest(user_root: &Path, record_id: &str) -> std::io::Result<Option<ArchiveEntry>> {
    for path in month_files(user_root).into_iter().rev() {
        let (entries, _) = read_file(&path)?;
        if let Some(entry) = entries
            .into_iter()
            .rev()
            .find(|entry| entry.record_id == record_id)
        {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

/// Whether a record was archived out of an activity-log lane: a lane file
/// (`lane:<name>`) or, before the clean-up, the nested `user.<name>` list.
pub fn is_lane_section(from_section: &str) -> bool {
    from_section.starts_with(LANE_SECTION_PREFIX)
        || from_section
            .strip_prefix("user.")
            .and_then(crate::magician_v2::agents::user_memory_lanes::Lane::from_name)
            .is_some()
}

/// The record as it goes back into service: archive bookkeeping removed,
/// pending review again so it takes the normal path, and marked as restored
/// (restored records are protected from folding and caps).
pub fn restorable_record(entry: &ArchiveEntry, now: DateTime<Utc>) -> Value {
    let mut record = entry.record.clone();
    if let Some(object) = record.as_object_mut() {
        if !is_lane_section(&entry.from_section) {
            for key in [
                "superseded_id",
                "superseded_by",
                "superseded_at",
                "supersession_reason",
                "supersession_source",
                "retired_at",
                "retirement_reason",
                "memory_review_deferred_at",
                "memory_review_deferred_reason",
            ] {
                object.remove(key);
            }
            object.insert("memory_lifecycle".into(), json!("pending_review"));
            object.insert("memory_review_needed".into(), json!(true));
        }
        if is_lane_section(&entry.from_section) {
            // Lane rows carry no `memory_*` fields; this marker exempts the
            // row from the lane's window and row cap.
            object.insert(
                crate::magician_v2::agents::user_memory_lanes::RESTORED_MARKER.into(),
                json!(now.to_rfc3339()),
            );
        } else {
            object.insert("memory_restored_at".into(), json!(now.to_rfc3339()));
            object.insert("memory_restored_from".into(), json!(entry.archived_at));
        }
    }
    record
}

fn last_compress_check() -> &'static Mutex<HashMap<PathBuf, chrono::NaiveDate>> {
    static CHECKS: OnceLock<Mutex<HashMap<PathBuf, chrono::NaiveDate>>> = OnceLock::new();
    CHECKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// At most once a day per archive directory: compress month files older
/// than `after_months`. Failures are logged and retried the next day.
async fn maybe_compress_old_months(user_root: &Path, now: DateTime<Utc>, after_months: u32) {
    let dir = archive_dir(user_root);
    {
        let mut checks = last_compress_check()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if checks.get(&dir) == Some(&now.date_naive()) {
            return;
        }
        checks.insert(dir, now.date_naive());
    }
    if let Err(error) = compress_old_months(user_root, now, after_months).await {
        warn!(error = %error, "user memory archive compression failed; will retry");
    }
}

/// Compress every plain month file older than `after_months` months. Returns
/// how many files were compressed.
pub async fn compress_old_months(
    user_root: &Path,
    now: DateTime<Utc>,
    after_months: u32,
) -> std::io::Result<usize> {
    let cutoff = now.year() * 12 + now.month0() as i32 - after_months as i32;
    let mut compressed = 0;
    for path in month_files(user_root) {
        if !path.to_string_lossy().ends_with(PLAIN_SUFFIX) {
            continue;
        }
        let Some((year, month)) = month_of(&path) else {
            continue;
        };
        if year * 12 + (month as i32 - 1) >= cutoff {
            continue;
        }
        let flock = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(std::io::Error::other)?;
        let target = path.clone();
        let permit = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(std::io::Error::other)?;
        magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
            let _flock = flock;
            compress_one_blocking(&target)
        })
        .await
        .map_err(std::io::Error::other)??;
        compressed += 1;
    }
    Ok(compressed)
}

fn compress_one_blocking(plain: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(plain)?;
    let name = plain
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive.jsonl")
        .to_string();
    let zipped = PathBuf::from(format!("{}.zip", plain.display()));
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let zip_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file(name, zip_options)
        .map_err(std::io::Error::other)?;
    writer.write_all(&bytes)?;
    let compressed = writer.finish().map_err(std::io::Error::other)?.into_inner();
    // Verify the compressed copy holds exactly the same bytes before it is
    // published and the plain file goes away.
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(compressed.as_slice()))
        .map_err(std::io::Error::other)?;
    let mut roundtrip = Vec::new();
    zip.by_index(0)
        .map_err(std::io::Error::other)?
        .read_to_end(&mut roundtrip)?;
    if roundtrip != bytes {
        return Err(std::io::Error::other(
            "compressed archive did not round-trip; plain file kept",
        ));
    }
    // Unique staging name, file and directory sync, owner-only mode.
    magician_core::durable_io::write_bytes_durably_with_mode_sync(
        &zipped,
        &compressed,
        Some(0o600),
    )?;
    std::fs::remove_file(plain)?;
    magician_core::durable_io::sync_parent_dir_blocking(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn record(index: usize) -> Value {
        json!({"key":format!("workflow-{index}"),"value":format!("Do step {index}"),
            "memory_record_id":format!("rec-{index}"),"source_type":"inferred"})
    }

    #[tokio::test]
    async fn archive_then_restore_gives_back_the_same_record() {
        let root = tempfile::tempdir().unwrap();
        let now = at("2026-10-18T10:00:00Z");
        let entry = ArchiveEntry::new(
            "workflows",
            "restated_by_producer",
            record(1),
            Some("rec-2".into()),
            now,
        );
        append_batch(root.path(), &[entry.clone()], now, 3)
            .await
            .unwrap();
        let found = find_latest(root.path(), "rec-1").unwrap().unwrap();
        assert_eq!(found, entry);
        assert_eq!(found.record, record(1));
        let restored = restorable_record(&found, now);
        assert_eq!(restored["memory_lifecycle"], "pending_review");
        assert_eq!(restored["memory_review_needed"], true);
        assert_eq!(restored["value"], record(1)["value"]);
        assert!(restored.get("memory_restored_at").is_some());
    }

    #[tokio::test]
    async fn a_batch_of_a_thousand_is_one_append() {
        let root = tempfile::tempdir().unwrap();
        let now = at("2026-10-18T10:00:00Z");
        let entries: Vec<_> = (0..1000)
            .map(|index| ArchiveEntry::new("workflows", "section_cap", record(index), None, now))
            .collect();
        assert_eq!(
            append_batch(root.path(), &entries, now, 3).await.unwrap(),
            1000
        );
        let files = month_files(root.path());
        assert_eq!(files.len(), 1);
        let (read, skipped) = read_file(&files[0]).unwrap();
        assert_eq!(read.len(), 1000);
        assert_eq!(skipped, 0);
    }

    #[tokio::test]
    async fn a_torn_last_line_is_skipped_and_earlier_lines_still_read() {
        let root = tempfile::tempdir().unwrap();
        let now = at("2026-10-18T10:00:00Z");
        let first = ArchiveEntry::new("workflows", "section_cap", record(1), None, now);
        append_batch(root.path(), &[first.clone()], now, 3)
            .await
            .unwrap();
        let path = month_files(root.path()).remove(0);
        // Simulate a crash mid-append.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(br#"{"archived_at":"2026-10-18T"#).unwrap();
        drop(file);
        let (read, skipped) = read_file(&path).unwrap();
        assert_eq!(read, vec![first.clone()]);
        assert_eq!(skipped, 1);
        // The next append starts on a fresh line, so it is readable too.
        let second = ArchiveEntry::new("workflows", "section_cap", record(2), None, now);
        append_batch(root.path(), &[second.clone()], now, 3)
            .await
            .unwrap();
        let (read, skipped) = read_file(&path).unwrap();
        assert_eq!(read, vec![first, second]);
        assert_eq!(skipped, 1);
    }

    #[tokio::test]
    async fn old_months_are_compressed_and_still_restorable() {
        let root = tempfile::tempdir().unwrap();
        let june = at("2026-06-03T10:00:00Z");
        let october = at("2026-10-18T10:00:00Z");
        let old = ArchiveEntry::new("workflows", "section_cap", record(1), None, june);
        append_batch(root.path(), &[old.clone()], june, 3)
            .await
            .unwrap();
        let recent = ArchiveEntry::new("workflows", "section_cap", record(2), None, october);
        // Through the public path the compression check runs on append.
        assert_eq!(
            compress_old_months(root.path(), october, 3).await.unwrap(),
            1
        );
        append_batch(root.path(), &[recent.clone()], october, 3)
            .await
            .unwrap();
        let files = month_files(root.path());
        assert_eq!(files.len(), 2);
        assert!(files[0].to_string_lossy().ends_with(".jsonl.zip"));
        assert!(files[1].to_string_lossy().ends_with("2026-10.jsonl"));
        assert_eq!(find_latest(root.path(), "rec-1").unwrap().unwrap(), old);
        assert_eq!(find_latest(root.path(), "rec-2").unwrap().unwrap(), recent);
        // A month inside the window stays plain.
        assert_eq!(
            compress_old_months(root.path(), october, 3).await.unwrap(),
            0
        );
    }
}
