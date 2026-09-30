//! Rebuildable, content-free index of the canonical journal. Journal bytes
//! remain authoritative. SQLite transactions lag fsynced journal appends, so
//! an interrupted transaction is recovered by streaming only the unindexed
//! suffix. No historical envelopes or lifecycle maps live in process memory.

use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension};

use super::*;

const INDEX_NAME: &str = "recovery-index.sqlite3";
const INDEX_VERSION: i64 = 1;

pub(super) fn indexed_sequence(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
) -> Result<Option<u64>, LlmTraceJournalError> {
    if !scope.is_valid() {
        return Err(corrupt("journal scope must be valid"));
    }
    let root = workspace.analytics_llm_trace_journal_root(&scope.principal, &scope.workspace);
    ensure_real_scoped_directory_chain(workspace.base_root(), &root)
        .map_err(|e| corrupt(e.to_string()))?;
    let path = root.join(INDEX_NAME);
    if !ensure_regular_file_or_missing(&path).map_err(|e| corrupt(e.to_string()))? {
        return Ok(None);
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        ensure_regular_file_or_missing(&root.join(format!("{INDEX_NAME}{suffix}")))
            .map_err(|e| corrupt(e.to_string()))?;
    }
    let db = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_millis(100))?;
    db.execute_batch("PRAGMA cache_size=-64; PRAGMA mmap_size=0; PRAGMA query_only=ON;")?;
    Ok(Some(next_sequence(&db)? - 1))
}

#[derive(Debug)]
pub(super) struct JournalIndex {
    db: Connection,
    root: PathBuf,
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) recovered_records: usize,
}

fn storage(error: std::io::Error) -> LlmTraceJournalError {
    ArtifactV2Error::from(error).into()
}

fn corrupt(message: impl Into<String>) -> LlmTraceJournalError {
    LlmTraceJournalError::Sequence(message.into())
}

fn stamp(path: &Path) -> Result<String, LlmTraceJournalError> {
    ensure_regular_file_or_missing(path).map_err(|e| corrupt(e.to_string()))?;
    let metadata = std::fs::metadata(path).map_err(storage)?;
    let modified = metadata
        .modified()
        .map_err(storage)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| corrupt(e.to_string()))?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{}:{modified}",
            metadata.dev(),
            metadata.ino(),
            metadata.len()
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(format!("{}:{modified}", metadata.len()))
    }
}

fn open_segment(path: &Path) -> Result<File, LlmTraceJournalError> {
    ensure_regular_file_or_missing(path).map_err(|e| corrupt(e.to_string()))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path).map_err(storage)
}

impl JournalIndex {
    pub(super) fn open(
        workspace: &ArtifactV2Workspace,
        root: &Path,
        scope: &LlmScope,
        max_segment_bytes: u64,
    ) -> Result<Self, LlmTraceJournalError> {
        ensure_real_scoped_directory_chain(workspace.base_root(), root)
            .map_err(|e| corrupt(e.to_string()))?;
        workspace.create_dir_all_path_sync(root.join("segments"))?;
        for suffix in ["", "-journal", "-wal", "-shm"] {
            ensure_regular_file_or_missing(&root.join(format!("{INDEX_NAME}{suffix}")))
                .map_err(|e| corrupt(e.to_string()))?;
        }
        let path = root.join(INDEX_NAME);
        // Create privately before SQLite opens the file; the journal lease is
        // held by the caller throughout recovery and normal operation.
        if !path.exists() {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            options
                .open(&path)
                .map_err(storage)?
                .sync_all()
                .map_err(storage)?;
        }
        let db = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            PRAGMA cache_size=-1024; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;
            CREATE TABLE IF NOT EXISTS identity(version INTEGER NOT NULL, principal TEXT NOT NULL, workspace TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS records(sequence INTEGER PRIMARY KEY, record_key TEXT NOT NULL,
                checksum TEXT NOT NULL, segment TEXT NOT NULL, byte_offset INTEGER NOT NULL, byte_length INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS record_key_lookup ON records(record_key);
            CREATE TABLE IF NOT EXISTS segments(name TEXT PRIMARY KEY, indexed_bytes INTEGER NOT NULL,
                file_bytes INTEGER NOT NULL, stamp TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS calls(id TEXT PRIMARY KEY, state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS attempts(id TEXT PRIMARY KEY, call_id TEXT NOT NULL, state TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS attempts_by_call ON attempts(call_id);
            CREATE TABLE IF NOT EXISTS parents(child TEXT PRIMARY KEY, parent TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS children_by_parent ON parents(parent);")?;
        let identity: Option<(i64, String, String)> = db
            .query_row(
                "SELECT version, principal, workspace FROM identity",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match identity {
            None => {
                db.execute(
                    "INSERT INTO identity VALUES (?1,?2,?3)",
                    params![INDEX_VERSION, scope.principal, scope.workspace],
                )?;
            },
            Some((version, principal, tenant))
                if version == INDEX_VERSION
                    && principal == scope.principal
                    && tenant == scope.workspace => {},
            _ => {
                return Err(corrupt(
                    "journal index version or scope mismatch; rebuild the derived index",
                ))
            },
        }
        sync_directory(root)?;
        let mut index = Self {
            db,
            root: root.to_path_buf(),
            #[cfg(any(test, feature = "test-fixtures"))]
            recovered_records: 0,
        };
        index.recover(workspace, scope, max_segment_bytes)?;
        Ok(index)
    }

    fn recover(
        &mut self,
        workspace: &ArtifactV2Workspace,
        scope: &LlmScope,
        max_segment_bytes: u64,
    ) -> Result<(), LlmTraceJournalError> {
        let started = Instant::now();
        let segments_root = self.root.join("segments");
        let mut names = Vec::new();
        for entry in workspace.read_dir_path_sync(&segments_root)? {
            if !entry.file_name.starts_with("segment-") || !entry.file_name.ends_with(".jsonl") {
                continue;
            }
            if !entry.is_file || segment_first_sequence(&entry.file_name).is_none() {
                return Err(corrupt(format!("journal segment has an invalid sequence-bearing filename or is not a regular file: {}", entry.file_name)));
            }
            names.push(entry.file_name);
        }
        names.sort();
        // Missing indexed segments are corruption, never a reason to silently
        // forget deduplication or history. Only restricted journals prune.
        let mut statement = self.db.prepare("SELECT name FROM segments")?;
        for row in statement.query_map([], |r| r.get::<_, String>(0))? {
            let name = row?;
            if names.binary_search(&name).is_err() {
                return Err(corrupt(format!(
                    "indexed journal segment is absent: {name}"
                )));
            }
        }
        drop(statement);
        let last_indexed_segment: Option<String> = self
            .db
            .query_row(
                "SELECT name FROM segments ORDER BY name DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut recovered = 0usize;
        for (position, name) in names.iter().enumerate() {
            let path = segments_root.join(name);
            let file_stamp = stamp(&path)?;
            let file_bytes = std::fs::metadata(&path).map_err(storage)?.len();
            let limit = max_segment_bytes.max(MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES as u64);
            if file_bytes > limit {
                return Err(corrupt(format!(
                    "journal segment exceeds {limit}-byte recovery limit"
                )));
            }
            let known: Option<(u64, u64, String)> = self
                .db
                .query_row(
                    "SELECT indexed_bytes,file_bytes,stamp FROM segments WHERE name=?1",
                    [name],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let mut offset = 0;
            if let Some((indexed, old_size, old_stamp)) = known {
                offset = indexed;
                if indexed > file_bytes {
                    return Err(corrupt("indexed journal segment was truncated"));
                }
                if old_stamp != file_stamp
                    && (file_bytes == old_size || Some(name) != last_indexed_segment.as_ref())
                {
                    return Err(corrupt(
                        "indexed journal segment changed; refusing to trust its checkpoint",
                    ));
                }
                if indexed == file_bytes && old_stamp == file_stamp {
                    continue;
                }
            }
            // Bind a persisted checkpoint to its actual immutable envelope.
            if offset > 0 {
                let sequence: u64 = self.db.query_row(
                    "SELECT max(sequence) FROM records WHERE segment=?1",
                    [name],
                    |r| r.get(0),
                )?;
                self.envelope(sequence)?;
            }
            let mut reader = BufReader::new(open_segment(&path)?);
            reader.seek(SeekFrom::Start(offset)).map_err(storage)?;
            loop {
                let mut batch = Vec::new();
                let mut bytes = 0usize;
                let mut partial = false;
                while batch.len() < REPLAY_BATCH_ROWS && bytes < REPLAY_BATCH_BYTES {
                    let mut line = Vec::new();
                    (&mut reader)
                        .take((MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES + 1) as u64)
                        .read_until(b'\n', &mut line)
                        .map_err(storage)?;
                    if line.is_empty() {
                        break;
                    }
                    if line.len() > MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES {
                        return Err(corrupt("journal line exceeds recovery limit"));
                    }
                    if !line.ends_with(b"\n") {
                        if position + 1 != names.len() {
                            return Err(corrupt(format!(
                                "non-final segment {name} has a partial trailing record"
                            )));
                        }
                        partial = true;
                        break;
                    }
                    if !batch.is_empty() && bytes + line.len() > REPLAY_BATCH_BYTES {
                        reader
                            .seek(SeekFrom::Current(-(line.len() as i64)))
                            .map_err(storage)?;
                        break;
                    }
                    let envelope = LlmTraceJournalEnvelope::from_json_line_verified(&line)?;
                    if envelope.record.scope() != scope {
                        return Err(corrupt("journal envelope belongs to another scope"));
                    }
                    if matches!(
                        envelope.record,
                        LlmTraceRecord::CallIo(_)
                            | LlmTraceRecord::ContextBlock(_)
                            | LlmTraceRecord::ContentTombstone(_)
                            | LlmTraceRecord::ContentAccessAudit(_)
                    ) {
                        return Err(corrupt("restricted record in canonical journal"));
                    }
                    let length = line.len();
                    batch.push((envelope, offset, length));
                    offset += length as u64;
                    bytes += length;
                }
                if !batch.is_empty() {
                    let tx = self.db.unchecked_transaction()?;
                    let mut expected = next_sequence(&tx)?;
                    let mut touched = HashSet::new();
                    for (envelope, offset, length) in &batch {
                        if envelope.sequence != expected {
                            return Err(corrupt(format!(
                                "expected sequence {expected}, found {}",
                                envelope.sequence
                            )));
                        }
                        if *offset == 0 && segment_first_sequence(name) != Some(expected) {
                            return Err(corrupt(
                                "segment filename does not match its first sequence",
                            ));
                        }
                        check_key(&tx, &envelope.key, &envelope.payload_checksum)?;
                        apply_lifecycle(&tx, &envelope.record, &mut touched)?;
                        insert_record(&tx, envelope, name, *offset, *length)?;
                        expected = expected
                            .checked_add(1)
                            .ok_or_else(|| corrupt("journal sequence overflow"))?;
                    }
                    validate_calls(&tx, &touched)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO segments VALUES (?1,?2,?3,?4)",
                        params![name, offset, file_bytes, file_stamp],
                    )?;
                    tx.commit()?;
                    recovered += batch.len();
                }
                if partial {
                    // Trim only the unfinished final write, preserving the
                    // indexed prefix and every complete immutable envelope.
                    let mut options = OpenOptions::new();
                    options.write(true);
                    #[cfg(unix)]
                    options.custom_flags(libc::O_NOFOLLOW);
                    let file = options.open(&path).map_err(storage)?;
                    file.set_len(offset).map_err(storage)?;
                    file.sync_all().map_err(storage)?;
                    self.db.execute(
                        "INSERT OR REPLACE INTO segments VALUES (?1,?2,?2,?3)",
                        params![name, offset, stamp(&path)?],
                    )?;
                    break;
                }
                if batch.is_empty() {
                    break;
                }
            }
        }
        let next = next_sequence(&self.db)?;
        if next > 1 {
            self.envelope(next - 1)?;
        }
        #[cfg(any(test, feature = "test-fixtures"))]
        {
            self.recovered_records = recovered;
        }
        tracing::info!(target: "analytics", records_recovered = recovered,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "canonical journal index ready; historical envelopes remain on disk");
        Ok(())
    }

    pub(super) fn append(
        &mut self,
        workspace: &ArtifactV2Workspace,
        max_segment_bytes: u64,
        scope: &LlmScope,
        records: &[LlmTraceRecord],
    ) -> Result<LlmTraceAppendReceipt, LlmTraceJournalError> {
        let mut receipt = LlmTraceAppendReceipt {
            appended: 0,
            duplicates: 0,
            first_sequence: None,
            last_sequence: None,
        };
        let mut start = 0;
        while start < records.len() {
            let mut end = start;
            let mut bytes = 0;
            while end < records.len() && end - start < REPLAY_BATCH_ROWS {
                let size = serde_json::to_vec(&records[end])?.len() + 1024;
                if end > start && bytes + size > REPLAY_BATCH_BYTES {
                    break;
                }
                bytes += size;
                end += 1;
            }
            let batch = self.append_bounded(workspace, max_segment_bytes, &records[start..end])?;
            receipt.appended += batch.appended;
            receipt.duplicates += batch.duplicates;
            receipt.first_sequence = receipt.first_sequence.or(batch.first_sequence);
            receipt.last_sequence = batch.last_sequence.or(receipt.last_sequence);
            start = end;
        }
        let _ = scope;
        Ok(receipt)
    }

    fn append_bounded(
        &mut self,
        workspace: &ArtifactV2Workspace,
        max_segment_bytes: u64,
        records: &[LlmTraceRecord],
    ) -> Result<LlmTraceAppendReceipt, LlmTraceJournalError> {
        let tx = self.db.unchecked_transaction()?;
        let mut next = next_sequence(&tx)?;
        let mut active: Option<(String, u64)> = tx
            .query_row(
                "SELECT name,indexed_bytes FROM segments ORDER BY name DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut candidates = Vec::new();
        let mut duplicates = 0;
        let mut touched = HashSet::new();
        // Validate all candidates transactionally before changing the journal.
        for record in records {
            let checksum = record_checksum(record)?;
            if check_key(&tx, &record.key(), &checksum)? {
                duplicates += 1;
                continue;
            }
            let envelope = LlmTraceJournalEnvelope::new(next, record.clone())?;
            let mut bytes = serde_json::to_vec(&envelope)?;
            bytes.push(b'\n');
            if bytes.len() > MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES {
                return Err(corrupt("serialized journal line exceeds limit"));
            }
            if active
                .as_ref()
                .is_none_or(|(_, size)| *size > 0 && *size + bytes.len() as u64 > max_segment_bytes)
            {
                active = Some((segment_name(next), 0));
            }
            let (name, offset) = active.as_mut().expect("assigned segment");
            apply_lifecycle(&tx, record, &mut touched)?;
            insert_record(&tx, &envelope, name, *offset, bytes.len())?;
            *offset += bytes.len() as u64;
            candidates.push((name.clone(), bytes));
            next = next
                .checked_add(1)
                .ok_or_else(|| corrupt("journal sequence overflow"))?;
        }
        validate_calls(&tx, &touched)?;
        ensure_real_scoped_directory_chain(workspace.base_root(), &self.root.join("segments"))
            .map_err(|e| corrupt(e.to_string()))?;
        let mut position = 0;
        while position < candidates.len() {
            let name = &candidates[position].0;
            let mut body = Vec::new();
            while position < candidates.len() && &candidates[position].0 == name {
                body.extend_from_slice(&candidates[position].1);
                position += 1;
            }
            let path = self.root.join("segments").join(name);
            ensure_regular_file_or_missing(&path).map_err(|e| corrupt(e.to_string()))?;
            workspace.append_path_sync(&path, &body)?;
            let size = std::fs::metadata(&path).map_err(storage)?.len();
            tx.execute(
                "INSERT OR REPLACE INTO segments VALUES (?1,?2,?2,?3)",
                params![name, size, stamp(&path)?],
            )?;
        }
        if !candidates.is_empty() {
            sync_directory(&self.root.join("segments"))?;
        }
        // Index state cannot become visible ahead of fsynced journal bytes.
        tx.commit()?;
        let appended = candidates.len();
        Ok(LlmTraceAppendReceipt {
            appended,
            duplicates,
            first_sequence: (appended > 0).then_some(next - appended as u64),
            last_sequence: (appended > 0).then_some(next - 1),
        })
    }

    pub(super) fn envelope(
        &self,
        sequence: u64,
    ) -> Result<LlmTraceJournalEnvelope, LlmTraceJournalError> {
        let location: Option<(String, u64, usize, String)> = self
            .db
            .query_row(
                "SELECT segment,byte_offset,byte_length,checksum FROM records WHERE sequence=?1",
                [sequence],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (segment, offset, length, checksum) = location
            .ok_or_else(|| corrupt(format!("cannot commit unknown journal sequence {sequence}")))?;
        if segment_first_sequence(&segment).is_none()
            || segment.contains('/')
            || segment.contains('\\')
            || length > MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES
        {
            return Err(corrupt("invalid journal index location"));
        }
        let mut file = open_segment(&self.root.join("segments").join(segment))?;
        file.seek(SeekFrom::Start(offset)).map_err(storage)?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes).map_err(storage)?;
        let envelope = LlmTraceJournalEnvelope::from_json_line_verified(&bytes)?;
        if envelope.sequence != sequence || envelope.payload_checksum != checksum {
            return Err(corrupt("index checkpoint does not match journal envelope"));
        }
        Ok(envelope)
    }

    pub(super) fn replay_batch(
        &self,
        committed: u64,
    ) -> Result<Vec<LlmTraceJournalEnvelope>, LlmTraceJournalError> {
        let mut statement = self.db.prepare(
            "SELECT sequence,byte_length FROM records WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = statement.query_map(params![committed, REPLAY_BATCH_ROWS], |r| {
            Ok((r.get::<_, u64>(0)?, r.get::<_, usize>(1)?))
        })?;
        let mut batch = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let (sequence, length) = row?;
            if !batch.is_empty() && bytes + length > REPLAY_BATCH_BYTES {
                break;
            }
            batch.push(self.envelope(sequence)?);
            bytes += length;
        }
        Ok(batch)
    }
}

fn next_sequence(db: &Connection) -> Result<u64, LlmTraceJournalError> {
    let last: u64 = db.query_row("SELECT COALESCE(max(sequence),0) FROM records", [], |r| {
        r.get(0)
    })?;
    last.checked_add(1)
        .ok_or_else(|| corrupt("journal sequence overflow"))
}

fn check_key(
    db: &Connection,
    key: &LlmTraceRecordKey,
    checksum: &str,
) -> Result<bool, LlmTraceJournalError> {
    let existing: Option<String> = db
        .query_row(
            "SELECT checksum FROM records WHERE record_key=?1 LIMIT 1",
            [serde_json::to_string(key)?],
            |r| r.get(0),
        )
        .optional()?;
    match existing {
        Some(existing) if existing == checksum => Ok(true),
        Some(_) => Err(LlmTraceJournalError::IdempotencyConflict {
            key: key.idempotency_key(),
            detail: "the same stable revision was delivered with a different payload".into(),
        }),
        None => Ok(false),
    }
}

fn insert_record(
    db: &Connection,
    envelope: &LlmTraceJournalEnvelope,
    segment: &str,
    offset: u64,
    length: usize,
) -> Result<(), LlmTraceJournalError> {
    db.execute(
        "INSERT INTO records VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            envelope.sequence,
            serde_json::to_string(&envelope.key)?,
            envelope.payload_checksum,
            segment,
            offset,
            length
        ],
    )?;
    Ok(())
}

fn call(db: &Connection, id: &str) -> Result<CallLifecycleAudit, LlmTraceJournalError> {
    let state: Option<String> = db
        .query_row("SELECT state FROM calls WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    Ok(state
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}

/// Apply identity/revision checks using the existing lifecycle rules, with
/// only one call and one attempt resident. Cross-record checks below stream
/// related attempts/children from their indexes, including late revisions.
fn apply_lifecycle(
    db: &Connection,
    record: &LlmTraceRecord,
    touched: &mut HashSet<String>,
) -> Result<(), LlmTraceJournalError> {
    let context = match record {
        LlmTraceRecord::CallStarted(v) => &v.context,
        LlmTraceRecord::CallCompleted(v) => &v.context,
        LlmTraceRecord::ProviderAttempt(v) => &v.context,
        _ => return Ok(()),
    };
    let id = &context.llm_call_id;
    let mut audit = LifecycleAudit::default();
    audit.calls.insert(id.clone(), call(db, id)?);
    if let LlmTraceRecord::ProviderAttempt(value) = record {
        let state: Option<String> = db
            .query_row(
                "SELECT state FROM attempts WHERE id=?1",
                [&value.provider_attempt_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(state) = state {
            audit.attempts.insert(
                value.provider_attempt_id.clone(),
                serde_json::from_str(&state)?,
            );
        }
    }
    validate_lifecycle_consistency(&mut audit, std::slice::from_ref(record))?;
    for (id, mut state) in audit.calls {
        // These sets are represented by attempts_by_call on disk.
        state.attempt_ids.clear();
        state.observed_attempt_indexes.clear();
        db.execute(
            "INSERT OR REPLACE INTO calls VALUES (?1,?2)",
            params![id, serde_json::to_string(&state)?],
        )?;
    }
    for (attempt_id, state) in audit.attempts {
        db.execute(
            "INSERT OR REPLACE INTO attempts VALUES (?1,?2,?3)",
            params![attempt_id, id, serde_json::to_string(&state)?],
        )?;
    }
    if let Some(parent) = context.parent_call_id.as_ref() {
        db.execute(
            "INSERT OR REPLACE INTO parents VALUES (?1,?2)",
            params![id, parent],
        )?;
    }
    touched.insert(id.clone());
    Ok(())
}

fn parent(db: &Connection, id: &str) -> Result<Option<String>, LlmTraceJournalError> {
    Ok(db
        .query_row("SELECT parent FROM parents WHERE child=?1", [id], |r| {
            r.get(0)
        })
        .optional()?)
}

fn validate_calls(db: &Connection, touched: &HashSet<String>) -> Result<(), LlmTraceJournalError> {
    for id in touched {
        let call = call(db, id)?;
        let mut statement = db.prepare("SELECT id,state FROM attempts WHERE call_id=?1")?;
        let mut rows = statement.query([id])?;
        while let Some(row) = rows.next()? {
            let attempt_id: String = row.get(0)?;
            let state: String = row.get(1)?;
            let attempt: AttemptLifecycleAudit = serde_json::from_str(&state)?;
            if let (Some(index), Some(count)) =
                (attempt.provider_attempt_index, call.provider_attempt_count)
            {
                if index > count {
                    return lifecycle_error(
                        id,
                        "observed provider attempt exceeds the completed call's attempt count",
                    );
                }
            }
            if let (Some(start), Some(attempt_start)) = (call.started_at_ms, attempt.started_at_ms)
            {
                if attempt_start < start {
                    return lifecycle_error(
                        &attempt_id,
                        "provider attempt starts before its logical call",
                    );
                }
            }
            if let (Some(end), Some(attempt_end)) = (call.completed_at_ms, attempt.completed_at_ms)
            {
                if attempt_end > end {
                    return lifecycle_error(
                        &attempt_id,
                        "provider attempt completes after its logical call",
                    );
                }
            }
            if attempt.provider_attempt_index == call.provider_attempt_count {
                if let (Some(call_state), Some(attempt_state)) =
                    (call.terminal_state, attempt.terminal_state)
                {
                    if (call_state == LlmCallTerminalState::Succeeded)
                        != (attempt_state == LlmAttemptTerminalState::Succeeded)
                    {
                        return lifecycle_error(&attempt_id, "final provider attempt transport outcome disagrees with its logical call");
                    }
                }
            }
        }
        // Floyd's cycle detection retains two IDs even for a deep lineage.
        let mut slow = Some(id.clone());
        let mut fast = Some(id.clone());
        loop {
            slow = match slow {
                Some(v) => parent(db, &v)?,
                None => None,
            };
            fast = match fast {
                Some(v) => parent(db, &v)?,
                None => None,
            };
            fast = match fast {
                Some(v) => parent(db, &v)?,
                None => None,
            };
            if slow.is_none() || fast.is_none() {
                break;
            }
            if slow == fast {
                return lifecycle_error(id, "logical call parent graph contains a cycle");
            }
        }
        // Check both the touched child and previously unresolved children of
        // a newly arrived parent. Stream fan-out instead of retaining it.
        let mut statement =
            db.prepare("SELECT child,parent FROM parents WHERE child=?1 OR parent=?1")?;
        let mut rows = statement.query([id])?;
        while let Some(row) = rows.next()? {
            let child_id: String = row.get(0)?;
            let parent_id: String = row.get(1)?;
            let child = self::call(db, &child_id)?;
            let parent = self::call(db, &parent_id)?;
            if let (Some(child), Some(parent)) = (child.context, parent.context) {
                if child.scope != parent.scope || child.trace_id != parent.trace_id {
                    return lifecycle_error(
                        &child_id,
                        "known parent must share the child call's trace and scope",
                    );
                }
            }
        }
    }
    Ok(())
}
