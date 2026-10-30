//! Whole-turn usage without repeatedly decoding large activity/debug payloads.
//! The canonical JSONL is append-only. A bounded, process-local checkpoint keeps
//! deduplication state and a complete-row watermark; new reads scan only additions.

use std::{
    collections::{HashMap, HashSet},
    fs::{File, Metadata},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Instant, SystemTime},
};

use serde::Deserialize;

/// The turn's OWN model usage, over its whole event file (the `events` page is
/// only the newest ~1 MiB, which undercounts a long turn).
///
/// Same contract as the web activity card (`turnUsageSummary.ts`):
/// - LLM-call rows are `LLMResponseReceived` (flat loop / harness aggregate)
///   and the AgentEvent-wrapped `llm.succeeded` (chat-side).
/// - Own = the row names no execution id, or the turn's own id. Rows of a
///   spawned / delegated task's execution are left out: the card adds those
///   from the task usage store.
/// - One call per distinct `llm_call_id` (payload, else
///   `correlation.llm_call_id`): the chat call lands as both an
///   `llm.succeeded` and an `LLMResponseReceived` row, counted once for tokens
///   AND cost. Rows without a call id count individually.
/// - `usage_availability` false fields are unreported (`null`), not zero.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub(super) struct ChatTurnUsage {
    pub(super) calls: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    /// `null` until a call reports a price.
    pub(super) cost_usd: Option<f64>,
    /// `null` when no call reported cache figures.
    pub(super) cache_read_tokens: Option<u64>,
    pub(super) cache_creation_tokens: Option<u64>,
}

/// One LLM-call row, reduced to what the usage needs.
pub(super) struct ChatTurnCallRecord {
    call_id: Option<String>,
    own: bool,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    cost_usd: Option<f64>,
}

/// The call record of one JSONL row, or `None` when the row is not an LLM-call
/// row. Callers prefilter on the raw bytes so only candidate rows are parsed.
pub(super) fn chat_turn_call_record(
    row: &serde_json::Value,
    chat_turn_id: &str,
) -> Option<ChatTurnCallRecord> {
    use serde_json::Value;
    let payload = match row.get("event_type").and_then(Value::as_str) {
        Some("LLMResponseReceived") => row.get("data")?,
        Some("AgentEvent") => {
            let data = row.get("data")?;
            let envelope = data.get("event").unwrap_or(data);
            if envelope.get("event_type").and_then(Value::as_str) != Some("llm.succeeded") {
                return None;
            }
            envelope.get("payload")?
        },
        _ => return None,
    };
    let correlation = payload.get("correlation");
    let field = |key: &str| {
        payload
            .get(key)
            .filter(|value| !value.is_null())
            .or_else(|| correlation.and_then(|c| c.get(key)))
    };
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let call_id = text(field("llm_call_id"));
    let execution_id = text(field("execution_id"));
    let availability = field("usage_availability");
    let available = |flag: &str| {
        availability
            .and_then(|a| a.get(flag))
            .and_then(Value::as_bool)
            != Some(false)
    };
    let count = |key: &str, flag: &str| {
        if !available(flag) {
            return None;
        }
        payload
            .get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .map(|n| n.max(0.0) as u64)
    };
    let price = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
    };
    Some(ChatTurnCallRecord {
        own: execution_id.as_deref().is_none_or(|id| id == chat_turn_id),
        call_id,
        input: count("input_tokens", "tokens"),
        output: count("output_tokens", "tokens"),
        cache_read: count("cache_read_tokens", "cache_read"),
        cache_creation: count("cache_creation_tokens", "cache_write"),
        cost_usd: price("cost_usd").or_else(|| price("cost")),
    })
}

const MAX_CACHED_TURNS: usize = 64;
const MAX_CACHED_CALL_IDS: usize = 8192;
const ANCHOR_BYTES: u64 = 64;

#[derive(Clone, Default)]
struct UsageAccumulator {
    seen_tokens: HashSet<blake3::Hash>,
    seen_cost: HashSet<blake3::Hash>,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    token_calls: u64,
    own_rows: u64,
    cost: Option<f64>,
}

impl UsageAccumulator {
    fn record(&mut self, record: ChatTurnCallRecord) {
        fn add(total: Option<u64>, value: Option<u64>) -> Option<u64> {
            match (total, value) {
                (total, None) => total,
                (None, Some(value)) => Some(value),
                (Some(total), Some(value)) => Some(total.saturating_add(value)),
            }
        }
        if !record.own {
            return;
        }
        self.own_rows = self.own_rows.saturating_add(1);
        let call_id = record
            .call_id
            .as_deref()
            .map(|id| blake3::hash(id.as_bytes()));
        if let Some(price) = record.cost_usd {
            if call_id.is_none_or(|id| self.seen_cost.insert(id)) {
                self.cost = Some(self.cost.unwrap_or(0.0) + price);
            }
        }
        if call_id.is_some_and(|id| self.seen_tokens.contains(&id)) {
            return;
        }
        if record.input.is_none()
            && record.output.is_none()
            && record.cache_read.is_none()
            && record.cache_creation.is_none()
        {
            return;
        }
        if let Some(id) = call_id {
            self.seen_tokens.insert(id);
        }
        self.input = add(self.input, record.input);
        self.output = add(self.output, record.output);
        self.cache_read = add(self.cache_read, record.cache_read);
        self.cache_creation = add(self.cache_creation, record.cache_creation);
        self.token_calls = self.token_calls.saturating_add(1);
    }

    fn usage(&self) -> ChatTurnUsage {
        if self.token_calls == 0 && self.cost.is_none() {
            return ChatTurnUsage::default();
        }
        ChatTurnUsage {
            calls: if self.token_calls > 0 {
                self.token_calls
            } else {
                self.own_rows
            },
            input_tokens: self.input.unwrap_or(0),
            output_tokens: self.output.unwrap_or(0),
            cost_usd: self.cost,
            cache_read_tokens: self.cache_read,
            cache_creation_tokens: self.cache_creation,
        }
    }

    fn cacheable(&self) -> bool {
        self.seen_tokens.len().saturating_add(self.seen_cost.len()) <= MAX_CACHED_CALL_IDS
    }
}

#[cfg(test)]
pub(super) fn summarize_chat_turn_calls(
    records: impl IntoIterator<Item = ChatTurnCallRecord>,
) -> ChatTurnUsage {
    let mut accumulator = UsageAccumulator::default();
    for record in records {
        accumulator.record(record);
    }
    accumulator.usage()
}

/// Read only the first tag on canonical transport rows. The fallback supports
/// reordered/whitespace-formatted legacy JSON without allocating its payload.
/// Searching the whole line for a marker also matched huge ExecutionPanelDelta
/// payloads containing nested copies of usage events.
fn is_usage_candidate(line: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct EventKind {
        event_type: String,
    }
    let line = line.trim_ascii();
    let kind = if let Some(rest) = line.strip_prefix(br#"{"event_type":"#) {
        String::deserialize(&mut serde_json::Deserializer::from_slice(rest)).ok()
    } else if let Some(kind) = trailing_canonical_tag(line) {
        Some(kind)
    } else {
        serde_json::from_slice::<EventKind>(line)
            .ok()
            .map(|row| row.event_type)
    };
    matches!(kind.as_deref(), Some("LLMResponseReceived" | "AgentEvent"))
}

/// Fan-out rows serialized through Value have sorted keys: data first, tag last.
/// Inspect only the short final field, rather than traversing a megabyte payload.
/// Any extra closing delimiters or noncanonical fields reject this fast path;
/// the general deserializer then finds the actual top-level tag.
fn trailing_canonical_tag(line: &[u8]) -> Option<String> {
    if !line.starts_with(br#"{"data":"#) {
        return None;
    }
    let body = line.strip_suffix(b"}")?;
    let suffix = &body[body.len().saturating_sub(128)..];
    let comma = suffix.iter().rposition(|byte| *byte == b',')?;
    let rest = suffix[comma + 1..]
        .trim_ascii_start()
        .strip_prefix(br#""event_type":"#)?;
    let mut decoder = serde_json::Deserializer::from_slice(rest);
    let kind = String::deserialize(&mut decoder).ok()?;
    decoder.end().ok()?;
    Some(kind)
}

#[derive(Clone, PartialEq, Eq)]
struct FileIdentity {
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileIdentity {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }

    fn reliable(&self) -> bool {
        cfg!(unix) || self.created.is_some()
    }
}

#[derive(Clone)]
struct Checkpoint {
    identity: FileIdentity,
    modified: Option<SystemTime>,
    observed_len: u64,
    offset: u64,
    anchor: Vec<u8>,
    accumulator: UsageAccumulator,
}

type CacheKey = (PathBuf, String);
type TurnCheckpoint = Arc<Mutex<Option<Checkpoint>>>;

#[derive(Default)]
struct UsageCache {
    // Only lookup/eviction holds this mutex. Different turns scan independently.
    turns: Mutex<HashMap<CacheKey, (Instant, TurnCheckpoint)>>,
}

impl UsageCache {
    fn checkpoint(&self, path: &Path, turn_id: &str) -> io::Result<TurnCheckpoint> {
        let mut turns = self
            .turns
            .lock()
            .map_err(|_| io::Error::other("usage cache poisoned"))?;
        let key = (path.to_path_buf(), turn_id.to_owned());
        if let Some((accessed, checkpoint)) = turns.get_mut(&key) {
            *accessed = Instant::now();
            return Ok(checkpoint.clone());
        }
        if turns.len() >= MAX_CACHED_TURNS {
            let oldest = turns
                .iter()
                .filter(|(_, (_, value))| Arc::strong_count(value) == 1)
                .min_by_key(|(_, (accessed, _))| *accessed)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                turns.remove(&oldest);
            } else {
                // All slots are in use. Serve this read uncached instead of
                // growing an unbounded registry during a burst of requests.
                return Ok(Arc::new(Mutex::new(None)));
            }
        }
        let checkpoint = Arc::new(Mutex::new(None));
        turns.insert(key, (Instant::now(), checkpoint.clone()));
        Ok(checkpoint)
    }

    fn read(
        &self,
        path: &Path,
        end_offset: u64,
        turn_id: &str,
    ) -> io::Result<(ChatTurnUsage, u64)> {
        let checkpoint = self.checkpoint(path, turn_id)?;
        let mut checkpoint = checkpoint
            .lock()
            .map_err(|_| io::Error::other("turn usage checkpoint poisoned"))?;
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                *checkpoint = None;
                return Ok((ChatTurnUsage::default(), 0));
            },
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if metadata.len() < end_offset {
            *checkpoint = None;
            return Err(io::Error::other("turn event file shrank during read"));
        }
        let identity = FileIdentity::of(&metadata);
        let mut valid = false;
        if let Some(old) = checkpoint.as_ref() {
            valid = identity.reliable()
                && old.identity == identity
                && metadata.len() >= old.observed_len
                && (metadata.len() > old.observed_len || metadata.modified().ok() == old.modified)
                && read_anchor(&mut file, old.offset)? == old.anchor;
        }
        let older_snapshot = valid
            && checkpoint
                .as_ref()
                .is_some_and(|old| end_offset < old.offset);
        let mut next = if valid && !older_snapshot {
            checkpoint.as_ref().expect("validated checkpoint").clone()
        } else {
            Checkpoint {
                identity,
                modified: None,
                observed_len: 0,
                offset: 0,
                anchor: Vec::new(),
                accumulator: UsageAccumulator::default(),
            }
        };
        file.seek(SeekFrom::Start(next.offset))?;
        let mut scanned = 0;
        let mut unterminated_record = None;
        {
            let mut reader =
                BufReader::with_capacity(64 * 1024, (&mut file).take(end_offset - next.offset));
            let mut line = Vec::new();
            loop {
                line.clear();
                let bytes = reader.read_until(b'\n', &mut line)? as u64;
                scanned += bytes;
                if bytes == 0 {
                    break;
                }
                if !line.ends_with(b"\n") {
                    // Preserve the legacy result for valid JSON without a newline,
                    // but never checkpoint it: the row may still be growing.
                    if is_usage_candidate(&line) {
                        unterminated_record = serde_json::from_slice::<serde_json::Value>(&line)
                            .ok()
                            .and_then(|row| chat_turn_call_record(&row, turn_id));
                    }
                    break;
                }
                next.offset += bytes;
                if !is_usage_candidate(&line) {
                    continue;
                }
                if let Ok(row) = serde_json::from_slice::<serde_json::Value>(&line) {
                    if let Some(record) = chat_turn_call_record(&row, turn_id) {
                        next.accumulator.record(record);
                    }
                }
            }
        }
        let usage = if let Some(record) = unterminated_record {
            let mut snapshot = next.accumulator.clone();
            snapshot.record(record);
            snapshot.usage()
        } else {
            next.accumulator.usage()
        };
        if !older_snapshot {
            next.modified = metadata.modified().ok();
            next.observed_len = metadata.len();
            next.anchor = read_anchor(&mut file, next.offset)?;
            *checkpoint =
                (next.identity.reliable() && next.accumulator.cacheable()).then_some(next);
        }
        Ok((usage, scanned))
    }
}

/// Detect replacements/truncation and ordinary truncate-and-regrow rewrites.
/// Same-size modifications force a rescan via metadata. Canonical writers only
/// append; these small guards avoid hashing the entire prefix on every append.
fn read_anchor(file: &mut File, offset: u64) -> io::Result<Vec<u8>> {
    let head_len = offset.min(ANCHOR_BYTES);
    let tail_start = head_len.max(offset.saturating_sub(ANCHOR_BYTES));
    let mut anchor = vec![0; (head_len + offset - tail_start) as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut anchor[..head_len as usize])?;
    file.seek(SeekFrom::Start(tail_start))?;
    file.read_exact(&mut anchor[head_len as usize..])?;
    Ok(anchor)
}

/// Blocking; the HTTP handler runs this on spawn_blocking, after scope checks.
/// The key contains the full scoped path and turn id. No response/API changes.
pub(super) fn chat_turn_usage_from_file(
    path: &Path,
    end_offset: u64,
    chat_turn_id: &str,
) -> io::Result<ChatTurnUsage> {
    static CACHE: OnceLock<UsageCache> = OnceLock::new();
    CACHE
        .get_or_init(UsageCache::default)
        .read(path, end_offset, chat_turn_id)
        .map(|(usage, _)| usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn row(id: &str, input: u64) -> String {
        format!(
            "{}\n",
            serde_json::json!({"event_type":"LLMResponseReceived", "data":{
                "correlation":{"llm_call_id":id}, "input_tokens":input, "output_tokens":2, "cost":0.25
            }})
        )
    }

    fn append(path: &Path, text: &str) -> u64 {
        File::options()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        path.metadata().unwrap().len()
    }

    #[test]
    fn ignores_nested_markers_but_supports_reordered_and_escaped_top_level_tags() {
        let nested = br#"{"event_type":"ExecutionPanelDelta","data":{"event_type":"LLMResponseReceived","input_tokens":999}}"#;
        assert!(!is_usage_candidate(nested));
        assert!(!is_usage_candidate(
            br#"{"data":{"event_type":"LLMResponseReceived"},"event_type":"ExecutionPanelDelta"}"#
        ));
        assert!(is_usage_candidate(
            br#"{"data":{},"event_type":"LLMResponseReceived"}"#
        ));
        assert!(is_usage_candidate(
            br#"{ "event_type" : "LLMResponseReceived", "data":{} }"#
        ));
        assert!(is_usage_candidate(
            br#"{"event_type":"LLMResponse\u0052eceived","data":{}}"#
        ));
        assert!(!is_usage_candidate(br#"{"event_type":null,"data":{}}"#));
        assert!(is_usage_candidate(br#"{"data":{},"event_type":"LLMResponseReceived","extra":{"event_type":"ExecutionPanelDelta"}}"#));
        assert!(!is_usage_candidate(
            br#"{"data":{"event_type":"LLMResponseReceived"}}"#
        ));
        assert!(is_usage_candidate(
            br#"{"data":{},"event_type":"AgentEvent","extra":"event_type"}"#
        ));
    }

    #[test]
    fn scans_once_then_only_appended_bytes_and_deduplicates_across_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = row("call-1", 10);
        std::fs::write(&path, &first).unwrap();
        let cache = UsageCache::default();
        let (usage, bytes) = cache.read(&path, first.len() as u64, "turn").unwrap();
        assert_eq!(
            (usage.calls, usage.input_tokens, bytes),
            (1, 10, first.len() as u64)
        );
        assert_eq!(
            cache.read(&path, first.len() as u64, "turn").unwrap(),
            (usage, 0)
        );

        let more = format!("{}{}", row("call-1", 10), row("call-2", 20));
        let end = append(&path, &more);
        let (usage, bytes) = cache.read(&path, end, "turn").unwrap();
        assert_eq!(
            (
                usage.calls,
                usage.input_tokens,
                usage.output_tokens,
                usage.cost_usd
            ),
            (2, 30, 4, Some(0.5))
        );
        assert_eq!(bytes, more.len() as u64);
    }

    #[test]
    fn cost_and_tokens_can_arrive_in_different_rows_and_delegate_calls_stay_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = "{\"event_type\":\"AgentEvent\",\"data\":{\"event\":{\"event_type\":\"llm.succeeded\",\"payload\":{\"llm_call_id\":\"call-1\",\"input_tokens\":10,\"output_tokens\":2}}}}\n";
        std::fs::write(&path, first).unwrap();
        let cache = UsageCache::default();
        let (usage, _) = cache.read(&path, first.len() as u64, "turn").unwrap();
        assert_eq!((usage.calls, usage.cost_usd), (1, None));
        let end = append(
            &path,
            &format!(
                "{}{}\n",
                row("call-1", 10),
                serde_json::json!({
                    "event_type":"LLMResponseReceived", "data":{"execution_id":"delegated-exec", "input_tokens":999,"cost":100}
                })
            ),
        );
        let (usage, _) = cache.read(&path, end, "turn").unwrap();
        assert_eq!(
            (usage.calls, usage.input_tokens, usage.cost_usd),
            (1, 10, Some(0.25))
        );
    }

    #[test]
    fn incomplete_rows_are_retried_without_losing_or_double_counting_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = row("call-1", 10);
        let second = row("call-2", 20);
        let cut = second.len() / 2;
        std::fs::write(&path, format!("{}{}", first, &second[..cut])).unwrap();
        let cache = UsageCache::default();
        let (usage, _) = cache
            .read(&path, path.metadata().unwrap().len(), "turn")
            .unwrap();
        assert_eq!(usage.input_tokens, 10);
        let end = append(&path, &second[cut..]);
        let (usage, bytes) = cache.read(&path, end, "turn").unwrap();
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(bytes, second.len() as u64);
        assert_eq!(cache.read(&path, end, "turn").unwrap().1, 0);
    }

    #[test]
    fn valid_unterminated_json_is_counted_without_advancing_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = row("call-1", 10);
        std::fs::write(&path, first.trim_end()).unwrap();
        let cache = UsageCache::default();
        assert_eq!(
            cache
                .read(&path, path.metadata().unwrap().len(), "turn")
                .unwrap()
                .0
                .input_tokens,
            10
        );
        let end = append(&path, "\n");
        let (usage, bytes) = cache.read(&path, end, "turn").unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(bytes, end);
        assert_eq!(cache.read(&path, end, "turn").unwrap().1, 0);
    }

    #[test]
    fn late_old_snapshot_does_not_rewind_newer_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = row("call-1", 10);
        std::fs::write(&path, &first).unwrap();
        let end = append(&path, &row("call-2", 20));
        let cache = UsageCache::default();
        let (latest, _) = cache.read(&path, end, "turn").unwrap();
        let (old, _) = cache.read(&path, first.len() as u64, "turn").unwrap();
        assert_eq!(old.input_tokens, 10);
        assert_eq!(cache.read(&path, end, "turn").unwrap(), (latest, 0));
    }

    #[test]
    fn replacement_truncation_and_same_size_rewrite_invalidate_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let original = row("call-1", 10);
        std::fs::write(&path, &original).unwrap();
        let cache = UsageCache::default();
        cache.read(&path, original.len() as u64, "turn").unwrap();
        let replacement = dir.path().join("replacement");
        let changed = row("call-1", 20);
        std::fs::write(&replacement, &changed).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(
            cache
                .read(&path, changed.len() as u64, "turn")
                .unwrap()
                .0
                .input_tokens,
            20
        );

        let changed = row("call-1", 30);
        std::fs::write(&path, &changed).unwrap();
        assert_eq!(
            cache
                .read(&path, changed.len() as u64, "turn")
                .unwrap()
                .0
                .input_tokens,
            30
        );

        std::fs::write(&path, b"").unwrap();
        assert!(cache.read(&path, changed.len() as u64, "turn").is_err());
        assert_eq!(
            cache.read(&path, 0, "turn").unwrap().0,
            ChatTurnUsage::default()
        );
        let end = append(&path, &original);
        assert_eq!(cache.read(&path, end, "turn").unwrap().0.input_tokens, 10);
    }

    #[test]
    fn full_scoped_paths_isolate_identical_turn_ids() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("workspace-a.jsonl");
        let b = dir.path().join("workspace-b.jsonl");
        std::fs::write(&a, row("call-1", 10)).unwrap();
        std::fs::write(&b, row("call-1", 99)).unwrap();
        let cache = UsageCache::default();
        assert_eq!(
            cache
                .read(&a, a.metadata().unwrap().len(), "same-turn")
                .unwrap()
                .0
                .input_tokens,
            10
        );
        assert_eq!(
            cache
                .read(&b, b.metadata().unwrap().len(), "same-turn")
                .unwrap()
                .0
                .input_tokens,
            99
        );
    }

    #[test]
    fn concurrent_refreshes_share_one_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        std::fs::write(&path, row("call-1", 10)).unwrap();
        let end = path.metadata().unwrap().len();
        let cache = UsageCache::default();
        let scanned: u64 = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let path = &path;
                    let cache = &cache;
                    scope.spawn(move || {
                        let (usage, bytes) = cache.read(path, end, "turn").unwrap();
                        assert_eq!(usage.input_tokens, 10);
                        bytes
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum()
        });
        assert_eq!(scanned, end);
    }

    #[test]
    fn cache_capacity_is_bounded_even_when_all_entries_are_in_use() {
        let cache = UsageCache::default();
        let mut held = Vec::new();
        for index in 0..MAX_CACHED_TURNS * 2 {
            held.push(
                cache
                    .checkpoint(Path::new(&format!("scope-{index}")), "turn")
                    .unwrap(),
            );
        }
        assert_eq!(cache.turns.lock().unwrap().len(), MAX_CACHED_TURNS);
        drop(held);
        cache.checkpoint(Path::new("new-scope"), "turn").unwrap();
        assert_eq!(cache.turns.lock().unwrap().len(), MAX_CACHED_TURNS);
    }

    #[test]
    fn unavailable_fields_idless_calls_and_malformed_rows_preserve_existing_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.jsonl");
        let first = "{\"event_type\":\"LLMResponseReceived\",\"data\":{\"input_tokens\":10,\"output_tokens\":2,\"cost\":0.25,\"usage_availability\":{\"cache_read\":false}}}\n";
        std::fs::write(&path, format!("{first}not-json\n{first}")).unwrap();
        let (usage, _) = UsageCache::default()
            .read(&path, path.metadata().unwrap().len(), "turn")
            .unwrap();
        assert_eq!(
            (
                usage.calls,
                usage.input_tokens,
                usage.cost_usd,
                usage.cache_read_tokens
            ),
            (2, 20, Some(0.5), None)
        );
    }
}
