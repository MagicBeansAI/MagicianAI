//! `search_chat_history`: read-only recall over the current chat session's
//! durable transcript.
//!
//! Context compaction (`context_compaction.rs`) replaces the older part of a
//! long conversation with a summary in the model's view; the canonical
//! transcript (`ChatStore::get_llm_history`) stays complete. This tool lets the
//! model recover exact details the summary dropped — a number, a name, a quote,
//! a path, a tool output — by searching that transcript and reading one entry
//! back page by page.
//!
//! Scope is the session the turn belongs to: there is no session argument.
//! Everything returned passes the provider-bound credential sanitizer, and a
//! governed app result is never re-disclosed through this path. The tool's own
//! earlier calls and results are left out of search: their snippets copy the
//! entries they found and would otherwise outrank them. What it returns is the
//! conversation's own record, the assistant's earlier answers among it, so the
//! answer check counts it as agent-written text, never as proof
//! (`result_evidence_origin`).
//!
//! Answer sizes follow the chat surface's result projection
//! (`agent_surface_runtime.result_projection.chat`): a string longer than
//! `max_scalar_bytes` (8 KiB) is dropped from the model's view, and the whole
//! answer must fit `max_model_tokens` (4096) at the conservative estimate of
//! two bytes per token, so about 8 KiB. The tool cannot see that budget, so it
//! stays well under it: a read returns at most `MAX_ENTRY_READ_BYTES` of
//! JSON-encoded text and pages with `offset` / `next_offset`, and a search
//! keeps its matches within `MAX_SEARCH_RESPONSE_BYTES`.
//!
//! Sanitizing is the costly step, so it runs only on text that can leave. An
//! entry up to `WHOLE_SANITIZE_MAX_BYTES`, or one that looks like JSON (whose
//! secret fields the sanitizer recognises only by parsing the whole document),
//! is sanitized whole and matched on the sanitized text. A longer prose entry
//! is matched on its raw text, and every term must survive sanitizing a window
//! of `SANITIZE_MARGIN_BYTES` around it, so a match never confirms a redacted
//! secret. A read of such an entry sanitizes from its start to the end of the
//! page plus a margin.
//!
//! The pure helpers (`render_entry`, `search_history`, `read_entry`) take the
//! sanitizer as an argument so tests can run them over a plain
//! `Vec<ChatLlmTranscriptEntry>`.

use std::collections::HashSet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use super::models::{ChatLlmTranscriptEntry, TranscriptBlock};
use super::tools_runtime::{ChatRuntimeTool, ChatRuntimeToolContext, PlanePosture};
use crate::magician_v2::artifact_v2::synthesis::EvidenceOrigin;

pub const SEARCH_CHAT_HISTORY_TOOL: &str = "search_chat_history";

pub const DEFAULT_SEARCH_LIMIT: usize = 8;
pub const MAX_SEARCH_LIMIT: usize = 20;
/// JSON-encoded bytes of entry text one read returns: under the chat
/// surface's 8 KiB scalar and token budgets with room for the envelope.
pub const DEFAULT_ENTRY_READ_BYTES: usize = 6 * 1024;
pub const MAX_ENTRY_READ_BYTES: usize = 6 * 1024;
const MIN_ENTRY_READ_BYTES: usize = 256;
/// JSON-encoded bytes the match records of one search answer may take.
const MAX_SEARCH_RESPONSE_BYTES: usize = 6 * 1024;
/// Roughly what one match record takes besides its snippet.
const HIT_RECORD_OVERHEAD_BYTES: usize = 128;
/// Snippets share the search budget: fewer matches, longer snippets.
const MAX_SNIPPET_BYTES: usize = 1024;
const MIN_SNIPPET_BYTES: usize = 160;
const ELLIPSIS: char = '…';
/// One rendered entry never exceeds this; a larger entry is searched and read
/// through its first `MAX_RENDERED_ENTRY_BYTES` only.
const MAX_RENDERED_ENTRY_BYTES: usize = 2 * 1024 * 1024;
/// Entries up to this size are sanitized whole before matching.
const WHOLE_SANITIZE_MAX_BYTES: usize = 64 * 1024;
/// Raw bytes sanitized on each side of a window cut from a longer prose
/// entry, so a credential near the window reaches the sanitizer whole. A
/// credential longer than this that crosses a window edge can be missed.
const SANITIZE_MARGIN_BYTES: usize = 4 * 1024;
/// Hit counts are divided by the square root of an entry's length in these
/// units, so one huge snapshot does not outrank every short exact answer.
const RANK_LENGTH_UNIT_BYTES: usize = 1024;
const MAX_QUERY_BYTES: usize = 1024;
const MAX_QUERY_TERMS: usize = 16;
const MAX_JSON_RENDER_NODES: usize = 200_000;
const MAX_JSON_RENDER_DEPTH: usize = 64;

const ENTRY_NOTE: &str =
    "Use entry=<n> to read an entry; pass offset=<next_offset> for the rest of a long one.";

/// Coarse role of one transcript entry, as reported to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEntryKind {
    User,
    Assistant,
    ToolResult,
}

impl HistoryEntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolResult => "tool_result",
        }
    }
}

/// Searchable text rendering of one transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedEntry {
    pub kind: HistoryEntryKind,
    pub tool_name: Option<String>,
    pub text: String,
    /// Part of the entry is not in `text`: it was larger than
    /// `MAX_RENDERED_ENTRY_BYTES`, or a value exceeded the rendering limits.
    pub truncated: bool,
}

/// One search match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryHit {
    pub entry: usize,
    pub kind: HistoryEntryKind,
    pub tool_name: Option<String>,
    pub hits: usize,
    pub snippet: String,
}

/// A search's matches, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySearch {
    pub hits: Vec<HistoryHit>,
    /// `limit` ended the search with candidate entries left unexamined.
    pub more_matches: bool,
}

/// One page of an entry's sanitized text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRead {
    pub kind: HistoryEntryKind,
    pub tool_name: Option<String>,
    pub text: String,
    /// Byte offset of `text` in the entry's sanitized text.
    pub offset: usize,
    /// Where the next page starts; `None` once `text` reaches the end.
    pub next_offset: Option<usize>,
    /// `text` does not reach the end of the entry: a next page follows, or
    /// the entry was cut when rendered.
    pub truncated: bool,
}

/// Parsed tool arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRequest {
    Search {
        query: String,
        limit: usize,
    },
    Entry {
        index: usize,
        offset: usize,
        max_bytes: usize,
    },
}

/// What context compaction folded out of the model's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionBoundary {
    /// Entries `0..covered_entries` are summarized.
    pub covered_entries: usize,
    /// The covered user entry the compacted view still carries verbatim.
    pub preserved_user_entry: Option<usize>,
}

impl CompactionBoundary {
    /// Whether the model sees `index` only through the summary.
    pub fn compacted(&self, index: usize) -> bool {
        index < self.covered_entries && self.preserved_user_entry != Some(index)
    }
}

/// How the answer check may use a chat tool's result. Recall returns the
/// conversation's own record — the assistant's earlier answers among it — so
/// a figure it repeats is agent-written, not independent proof; every other
/// tool result is evidence.
pub fn result_evidence_origin(tool_name: &str) -> EvidenceOrigin {
    if tool_name == SEARCH_CHAT_HISTORY_TOOL {
        EvidenceOrigin::AgentWritten
    } else {
        EvidenceOrigin::ToolResult
    }
}

/// Appends `piece` to `out` without letting `out` exceed `cap` bytes.
/// Returns `false` once the cap cut something.
fn push_bounded(out: &mut String, piece: &str, cap: usize) -> bool {
    let room = cap.saturating_sub(out.len());
    if piece.len() <= room {
        out.push_str(piece);
        return true;
    }
    out.push_str(&piece[..floor_char_boundary(piece, room)]);
    false
}

/// Largest char boundary of `text` that is `<= index`.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Bytes `ch` takes inside a JSON string, as serde_json escapes it.
fn json_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
        ch if u32::from(ch) < 0x20 => 6,
        ch => ch.len_utf8(),
    }
}

/// Byte length of the longest prefix of `text` whose JSON-encoded size is at
/// most `budget`.
fn encoded_prefix_len(text: &str, budget: usize) -> usize {
    let mut used = 0usize;
    for (index, ch) in text.char_indices() {
        used += json_char_len(ch);
        if used > budget {
            return index;
        }
    }
    text.len()
}

fn render_blocks(out: &mut String, blocks: &[TranscriptBlock], cap: usize) -> bool {
    for (position, block) in blocks.iter().enumerate() {
        if position > 0 && !push_bounded(out, "\n", cap) {
            return false;
        }
        let complete = match block {
            TranscriptBlock::Text { text } => push_bounded(out, text, cap),
            TranscriptBlock::ImageFile { image } => {
                let label = image.label.as_deref().unwrap_or(image.stored_name.as_str());
                push_bounded(out, &format!("[image: {label}]"), cap)
            },
        };
        if !complete {
            return false;
        }
    }
    true
}

/// Compact JSON text for a value, bounded in bytes, nodes and depth and
/// rendered without recursive serializer frames.
fn render_json(out: &mut String, value: &Value, cap: usize) -> bool {
    let room = cap.saturating_sub(out.len());
    match crate::magician_v2::json_traversal::compact_json_prefix_bounded(
        value,
        room,
        MAX_JSON_RENDER_NODES,
        MAX_JSON_RENDER_DEPTH,
    ) {
        Ok(Some((text, truncated))) => {
            let complete = push_bounded(out, &text, cap);
            complete && !truncated
        },
        Ok(None) | Err(_) => {
            // The value itself is not shown, so the entry is incomplete.
            push_bounded(out, "[value exceeds rendering limits]", cap);
            false
        },
    }
}

/// Render one transcript entry to searchable text, capped at `cap` bytes.
/// `omit_own_calls` leaves this tool's own calls out of an assistant turn.
fn render_entry_with_cap(
    entry: &ChatLlmTranscriptEntry,
    cap: usize,
    omit_own_calls: bool,
) -> RenderedEntry {
    let mut text = String::new();
    let (kind, tool_name, complete) = match entry {
        ChatLlmTranscriptEntry::UserText { text: body } => (
            HistoryEntryKind::User,
            None,
            push_bounded(&mut text, body, cap),
        ),
        ChatLlmTranscriptEntry::UserTurn { content } => (
            HistoryEntryKind::User,
            None,
            render_blocks(&mut text, content, cap),
        ),
        ChatLlmTranscriptEntry::AssistantTurn {
            text: body,
            tool_calls,
            ..
        } => {
            let mut complete = body
                .as_deref()
                .is_none_or(|body| push_bounded(&mut text, body, cap));
            for call in tool_calls
                .iter()
                .filter(|call| !(omit_own_calls && call.name == SEARCH_CHAT_HISTORY_TOOL))
            {
                if !complete {
                    break;
                }
                let header = if text.is_empty() {
                    format!("[tool call {}] ", call.name)
                } else {
                    format!("\n[tool call {}] ", call.name)
                };
                complete = push_bounded(&mut text, &header, cap)
                    && render_json(&mut text, &call.arguments, cap);
            }
            (HistoryEntryKind::Assistant, None, complete)
        },
        ChatLlmTranscriptEntry::ToolResult {
            tool_name, content, ..
        } => (
            HistoryEntryKind::ToolResult,
            tool_name.clone(),
            push_bounded(&mut text, content, cap),
        ),
        ChatLlmTranscriptEntry::ToolResultRich {
            tool_name, content, ..
        } => (
            HistoryEntryKind::ToolResult,
            tool_name.clone(),
            render_blocks(&mut text, content, cap),
        ),
        ChatLlmTranscriptEntry::ToolResultProjected {
            tool_name,
            projection,
            ..
        } => {
            let complete = if projection.app_result_checkpoint.is_some() {
                // A governed app result is disclosed only through its own
                // checkpointed replay; recall never re-discloses it.
                push_bounded(
                    &mut text,
                    "[governed app result withheld from history search; run the app tool again to read it]",
                    cap,
                )
            } else if projection.validate_schema_version().is_err() {
                push_bounded(&mut text, "[unreadable tool result projection]", cap)
            } else {
                let value = crate::magician_v2::tool_result_projection::provider_safe_model_value(
                    projection,
                );
                let complete = render_json(&mut text, &value, cap);
                crate::magician_v2::json_traversal::discard_json_iteratively(value);
                complete
            };
            (HistoryEntryKind::ToolResult, tool_name.clone(), complete)
        },
    };
    RenderedEntry {
        kind,
        tool_name,
        text,
        truncated: !complete,
    }
}

/// Render one transcript entry to text, capped at `MAX_RENDERED_ENTRY_BYTES`.
pub fn render_entry(entry: &ChatLlmTranscriptEntry) -> RenderedEntry {
    render_entry_with_cap(entry, MAX_RENDERED_ENTRY_BYTES, false)
}

/// Call ids of this tool's own earlier calls.
fn own_call_ids(entries: &[ChatLlmTranscriptEntry]) -> HashSet<&str> {
    let mut ids = HashSet::new();
    for entry in entries {
        if let ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } = entry {
            ids.extend(
                tool_calls
                    .iter()
                    .filter(|call| call.name == SEARCH_CHAT_HISTORY_TOOL)
                    .map(|call| call.id.as_str()),
            );
        }
    }
    ids
}

/// A result of this tool's own earlier call: its snippets copy the entries
/// it found, so searching it would rank the copy above the original.
fn is_own_result(entry: &ChatLlmTranscriptEntry, own_calls: &HashSet<&str>) -> bool {
    let (tool_call_id, tool_name) = match entry {
        ChatLlmTranscriptEntry::ToolResult {
            tool_call_id,
            tool_name,
            ..
        }
        | ChatLlmTranscriptEntry::ToolResultRich {
            tool_call_id,
            tool_name,
            ..
        }
        | ChatLlmTranscriptEntry::ToolResultProjected {
            tool_call_id,
            tool_name,
            ..
        } => (tool_call_id, tool_name),
        _ => return false,
    };
    tool_name.as_deref() == Some(SEARCH_CHAT_HISTORY_TOOL)
        || own_calls.contains(tool_call_id.as_str())
}

/// Split a query into lower-cased terms. Double-quoted spans stay one term.
pub fn query_terms(query: &str) -> Vec<String> {
    let query = &query[..floor_char_boundary(query, MAX_QUERY_BYTES)];
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let flush = |current: &mut String, terms: &mut Vec<String>| {
        let term = current.trim().to_lowercase();
        current.clear();
        if !term.is_empty() && !terms.contains(&term) && terms.len() < MAX_QUERY_TERMS {
            terms.push(term);
        }
    };
    for ch in query.chars() {
        if ch == '"' {
            flush(&mut current, &mut terms);
            quoted = !quoted;
        } else if ch.is_whitespace() && !quoted {
            flush(&mut current, &mut terms);
        } else {
            current.push(ch);
        }
    }
    flush(&mut current, &mut terms);
    terms
}

/// Total occurrences of every term in `lower` and the byte offset (in
/// `lower`) of the earliest hit, or `None` when any term is missing.
fn score(lower: &str, terms: &[String]) -> Option<(usize, usize, usize)> {
    let mut hits = 0usize;
    let mut first: Option<(usize, usize)> = None;
    for term in terms {
        let position = lower.find(term.as_str())?;
        hits = hits.saturating_add(lower.matches(term.as_str()).count());
        if first.is_none_or(|(offset, _)| position < offset) {
            first = Some((position, term.len()));
        }
    }
    first.map(|(offset, len)| (hits, offset, len))
}

/// Ranking weight: hits divided by the square root of the entry's length in
/// `RANK_LENGTH_UNIT_BYTES`; entries up to one unit keep their raw count.
fn rank(hits: usize, len: usize) -> f64 {
    let units = len.max(RANK_LENGTH_UNIT_BYTES) as f64 / RANK_LENGTH_UNIT_BYTES as f64;
    hits as f64 / units.sqrt()
}

/// Map a byte offset in `text.to_lowercase()` back onto `text`. Lower-casing
/// keeps byte lengths for almost all text; when it does not, the offset is
/// scaled, which keeps the hit inside the snippet window.
fn map_lower_offset(text: &str, lower: &str, offset: usize) -> usize {
    let mapped = if lower.len() == text.len() || lower.is_empty() {
        offset
    } else {
        ((offset as u128 * text.len() as u128) / lower.len() as u128) as usize
    };
    floor_char_boundary(text, mapped)
}

/// A sanitized cut of at most `MAX_SNIPPET_BYTES` on each side of one hit,
/// from which a snippet of any smaller budget is taken.
#[derive(Debug, Clone)]
struct Excerpt {
    text: String,
    hit_start: usize,
    hit_len: usize,
    /// The source continues before / after `text`.
    more_before: bool,
    more_after: bool,
}

impl Excerpt {
    /// Cut from `source` around `[start, start + len)`.
    fn around(source: &str, start: usize, len: usize) -> Self {
        let start = floor_char_boundary(source, start);
        let end = floor_char_boundary(source, start.saturating_add(len)).max(start);
        let from = floor_char_boundary(source, start.saturating_sub(MAX_SNIPPET_BYTES));
        let to = floor_char_boundary(source, end.saturating_add(MAX_SNIPPET_BYTES));
        Self {
            text: source[from..to].to_string(),
            hit_start: start - from,
            hit_len: end - start,
            more_before: from > 0,
            more_after: to < source.len(),
        }
    }

    /// `…`-marked window around the hit whose JSON-encoded size, marks
    /// included, stays within `budget`. Grown from the hit outwards one
    /// character per side at a time.
    fn snippet(&self, budget: usize) -> String {
        let text = self.text.as_str();
        let budget = budget.saturating_sub(2 * ELLIPSIS.len_utf8());
        let hit_end = self.hit_start + self.hit_len;
        let mut used = 0usize;
        let mut low = self.hit_start;
        let mut high = self.hit_start;
        for ch in text[self.hit_start..hit_end].chars() {
            let cost = json_char_len(ch);
            if used + cost > budget {
                break;
            }
            used += cost;
            high += ch.len_utf8();
        }
        let mut left = text[..low].chars().rev();
        let mut right = text[high..].chars();
        let (mut grow_left, mut grow_right) = (true, true);
        while grow_left || grow_right {
            if grow_left {
                match left.next() {
                    Some(ch) if used + json_char_len(ch) <= budget => {
                        used += json_char_len(ch);
                        low -= ch.len_utf8();
                    },
                    _ => grow_left = false,
                }
            }
            if grow_right {
                match right.next() {
                    Some(ch) if used + json_char_len(ch) <= budget => {
                        used += json_char_len(ch);
                        high += ch.len_utf8();
                    },
                    _ => grow_right = false,
                }
            }
        }
        let mut snippet = String::with_capacity(high - low + 2 * ELLIPSIS.len_utf8());
        if low > 0 || self.more_before {
            snippet.push(ELLIPSIS);
        }
        snippet.push_str(&text[low..high]);
        if high < text.len() || self.more_after {
            snippet.push(ELLIPSIS);
        }
        snippet
    }
}

/// Whether `text` is sanitized whole: it is short, or it looks like JSON,
/// whose secret fields the sanitizer recognises only by parsing the whole
/// document.
fn sanitize_whole(text: &str) -> bool {
    text.len() <= WHOLE_SANITIZE_MAX_BYTES || text.trim_start().starts_with(['{', '['])
}

/// Hit count and a sanitized excerpt around the first hit of a raw match, or
/// `None` when the match does not survive sanitizing (a term was only inside
/// a redacted secret).
fn verify_match(
    text: &str,
    terms: &[String],
    sanitize: &dyn Fn(&str) -> String,
) -> Option<(usize, Excerpt)> {
    if sanitize_whole(text) {
        let sanitized = sanitize(text);
        let lower = sanitized.to_lowercase();
        let (hits, offset, len) = score(&lower, terms)?;
        let start = map_lower_offset(&sanitized, &lower, offset);
        return Some((hits, Excerpt::around(&sanitized, start, len)));
    }
    // A long prose entry: each term's first raw hit must survive sanitizing
    // a window around it. The count stays the raw one.
    let lower = text.to_lowercase();
    let (hits, _, _) = score(&lower, terms)?;
    let mut first: Option<(usize, Excerpt)> = None;
    for term in terms {
        let at = map_lower_offset(text, &lower, lower.find(term.as_str())?);
        let from = floor_char_boundary(text, at.saturating_sub(SANITIZE_MARGIN_BYTES));
        let to = floor_char_boundary(
            text,
            at.saturating_add(term.len())
                .saturating_add(SANITIZE_MARGIN_BYTES),
        );
        let window = sanitize(&text[from..to]);
        let window_lower = window.to_lowercase();
        let found = window_lower.find(term.as_str())?;
        if first.as_ref().is_none_or(|(earliest, _)| at < *earliest) {
            let start = map_lower_offset(&window, &window_lower, found);
            let mut excerpt = Excerpt::around(&window, start, term.len());
            excerpt.more_before |= from > 0;
            excerpt.more_after |= to < text.len();
            first = Some((at, excerpt));
        }
    }
    first.map(|(_, excerpt)| (hits, excerpt))
}

/// Snippet budget when `hits` matches share `MAX_SEARCH_RESPONSE_BYTES`.
fn snippet_budget(hits: usize) -> usize {
    (MAX_SEARCH_RESPONSE_BYTES / hits.max(1))
        .saturating_sub(HIT_RECORD_OVERHEAD_BYTES)
        .clamp(MIN_SNIPPET_BYTES, MAX_SNIPPET_BYTES)
}

/// All-terms, case-insensitive search, skipping this tool's own calls and
/// results. Ranked by term hits weighted down for long entries (`rank`), then
/// by recency (higher entry index first). Entries are matched on their raw
/// rendering first; only those candidates are sanitized, best first, until
/// `limit` survive (`verify_match`). Snippets are cut from sanitized text.
pub fn search_history_page(
    entries: &[ChatLlmTranscriptEntry],
    query: &str,
    limit: usize,
    sanitize: &dyn Fn(&str) -> String,
) -> HistorySearch {
    let terms = query_terms(query);
    if terms.is_empty() {
        return HistorySearch {
            hits: Vec::new(),
            more_matches: false,
        };
    }
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    let own_calls = own_call_ids(entries);
    // The rendering is kept for each candidate so it is not rebuilt.
    let mut candidates: Vec<(f64, usize, RenderedEntry)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if is_own_result(entry, &own_calls) {
            continue;
        }
        let rendered = render_entry_with_cap(entry, MAX_RENDERED_ENTRY_BYTES, true);
        if let Some((hits, _, _)) = score(&rendered.text.to_lowercase(), &terms) {
            candidates.push((rank(hits, rendered.text.len()), index, rendered));
        }
    }
    candidates
        .sort_unstable_by(|left, right| right.0.total_cmp(&left.0).then(right.1.cmp(&left.1)));
    let mut verified: Vec<(HistoryHit, Excerpt)> = Vec::new();
    let mut remaining = candidates.into_iter();
    for (_, index, rendered) in remaining.by_ref() {
        if let Some((hits, excerpt)) = verify_match(&rendered.text, &terms, sanitize) {
            verified.push((
                HistoryHit {
                    entry: index,
                    kind: rendered.kind,
                    tool_name: rendered.tool_name,
                    hits,
                    snippet: String::new(),
                },
                excerpt,
            ));
            if verified.len() == limit {
                break;
            }
        }
    }
    let more_matches = remaining.next().is_some();
    let budget = snippet_budget(verified.len());
    HistorySearch {
        hits: verified
            .into_iter()
            .map(|(mut hit, excerpt)| {
                hit.snippet = excerpt.snippet(budget);
                hit
            })
            .collect(),
        more_matches,
    }
}

/// `search_history_page` without the continuation flag.
pub fn search_history(
    entries: &[ChatLlmTranscriptEntry],
    query: &str,
    limit: usize,
    sanitize: &dyn Fn(&str) -> String,
) -> Vec<HistoryHit> {
    search_history_page(entries, query, limit, sanitize).hits
}

/// Sanitized text of `text`, exact through at least its first `need` bytes
/// or whole. Returns the text, how many of its bytes are exact, and whether
/// it covers the whole entry. A prose prefix sanitizes like the whole entry
/// except near its cut, where a credential may be split, so its last
/// `SANITIZE_MARGIN_BYTES` are not used.
fn sanitized_prefix(
    text: &str,
    need: usize,
    sanitize: &dyn Fn(&str) -> String,
) -> (String, usize, bool) {
    let whole = |text: &str| {
        let sanitized = sanitize(text);
        let len = sanitized.len();
        (sanitized, len, true)
    };
    if sanitize_whole(text) {
        return whole(text);
    }
    let mut cut = need.saturating_add(2 * SANITIZE_MARGIN_BYTES);
    loop {
        let end = floor_char_boundary(text, cut);
        if end >= text.len() {
            return whole(text);
        }
        let sanitized = sanitize(&text[..end]);
        let exact = floor_char_boundary(
            &sanitized,
            sanitized.len().saturating_sub(SANITIZE_MARGIN_BYTES),
        );
        if exact >= need {
            return (sanitized, exact, false);
        }
        cut = cut.saturating_mul(2);
    }
}

/// One page of an entry's sanitized text: from `offset` (snapped to a char
/// boundary) up to `max_bytes` of JSON-encoded text. Returns `None` when
/// `index` is out of range; an offset past the end gives an empty last page.
pub fn read_entry(
    entries: &[ChatLlmTranscriptEntry],
    index: usize,
    offset: usize,
    max_bytes: usize,
    sanitize: &dyn Fn(&str) -> String,
) -> Option<EntryRead> {
    let entry = entries.get(index)?;
    let rendered = render_entry(entry);
    let max_bytes = max_bytes.clamp(MIN_ENTRY_READ_BYTES, MAX_ENTRY_READ_BYTES);
    let (sanitized, exact, whole) =
        sanitized_prefix(&rendered.text, offset.saturating_add(max_bytes), sanitize);
    let usable = &sanitized[..exact];
    let start = floor_char_boundary(usable, offset);
    let end = start + encoded_prefix_len(&usable[start..], max_bytes);
    let more = end < exact || !whole;
    Some(EntryRead {
        kind: rendered.kind,
        tool_name: rendered.tool_name,
        text: usable[start..end].to_string(),
        offset: start,
        next_offset: more.then_some(end),
        truncated: more || rendered.truncated,
    })
}

fn usize_arg(args: &Value, key: &str) -> std::result::Result<Option<usize>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|value| value.fract() == 0.0 && *value >= 0.0)
                    .map(|value| value as u64)
            })
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a non-negative integer")),
        Some(Value::String(text)) => text
            .trim()
            .parse::<usize>()
            .map(Some)
            .map_err(|_| format!("`{key}` must be a non-negative integer")),
        Some(_) => Err(format!("`{key}` must be a non-negative integer")),
    }
}

/// Parse and clamp the tool arguments. `query` and `entry` are exclusive, and
/// `offset` / `max_bytes` belong to `entry`: an argument that would be
/// ignored is rejected instead.
pub fn parse_request(args: &Value) -> std::result::Result<HistoryRequest, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty());
    if let Some(index) = usize_arg(args, "entry")? {
        if query.is_some() {
            return Err(
                "pass either `query` (search) or `entry` (read one entry), not both".to_string(),
            );
        }
        let offset = usize_arg(args, "offset")?.unwrap_or(0);
        let max_bytes = usize_arg(args, "max_bytes")?
            .unwrap_or(DEFAULT_ENTRY_READ_BYTES)
            .clamp(MIN_ENTRY_READ_BYTES, MAX_ENTRY_READ_BYTES);
        return Ok(HistoryRequest::Entry {
            index,
            offset,
            max_bytes,
        });
    }
    let query =
        query.ok_or_else(|| "search_chat_history requires `query` (or `entry`)".to_string())?;
    if usize_arg(args, "offset")?.is_some() || usize_arg(args, "max_bytes")?.is_some() {
        return Err(
            "`offset` and `max_bytes` page through one entry; pass them with `entry`".to_string(),
        );
    }
    if query_terms(query).is_empty() {
        return Err("`query` has no searchable terms".to_string());
    }
    let limit = usize_arg(args, "limit")?
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    Ok(HistoryRequest::Search {
        query: query.to_string(),
        limit,
    })
}

/// The tool's JSON answer for one request over `entries`. `compaction` is
/// the session's checkpoint boundary, only when it matches `entries`.
pub fn history_search_response(
    entries: &[ChatLlmTranscriptEntry],
    request: &HistoryRequest,
    compaction: Option<CompactionBoundary>,
    sanitize: &dyn Fn(&str) -> String,
) -> Value {
    let total_entries = entries.len();
    let compacted = |index: usize| compaction.is_some_and(|boundary| boundary.compacted(index));
    match request {
        HistoryRequest::Search { query, limit } => {
            let search = search_history_page(entries, query, *limit, sanitize);
            // Snippets are already sized to share the budget; this is the
            // backstop for long tool names and escaping.
            let mut records = Vec::with_capacity(search.hits.len());
            let mut used = 0usize;
            let mut omitted_for_size = 0usize;
            for hit in search.hits {
                let record = json!({
                    "entry": hit.entry,
                    "kind": hit.kind.as_str(),
                    "tool_name": hit.tool_name,
                    "hits": hit.hits,
                    "compacted": compacted(hit.entry),
                    "snippet": hit.snippet,
                });
                let size = crate::magician_v2::json_traversal::json_encoded_len(&record)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1);
                if omitted_for_size > 0 || used.saturating_add(size) > MAX_SEARCH_RESPONSE_BYTES {
                    omitted_for_size += 1;
                    continue;
                }
                used += size;
                records.push(record);
            }
            json!({
                "status": "ok",
                "entries": records,
                "total_entries": total_entries,
                "compacted_through": compaction.map(|boundary| boundary.covered_entries),
                "more_matches": search.more_matches,
                "omitted_for_size": omitted_for_size,
                "note": ENTRY_NOTE,
            })
        },
        HistoryRequest::Entry {
            index,
            offset,
            max_bytes,
        } => match read_entry(entries, *index, *offset, *max_bytes, sanitize) {
            Some(read) => json!({
                "status": "ok",
                "entry": index,
                "kind": read.kind.as_str(),
                "tool_name": read.tool_name,
                "compacted": compacted(*index),
                "offset": read.offset,
                "text": read.text,
                "next_offset": read.next_offset,
                "truncated": read.truncated,
                "total_entries": total_entries,
            }),
            None => json!({
                "status": "error",
                "error_code": "history_entry_out_of_range",
                "reason": format!(
                    "entry {index} does not exist; this conversation has {total_entries} transcript entries (0-based)"
                ),
                "total_entries": total_entries,
            }),
        },
    }
}

fn provider_sanitize(text: &str) -> String {
    crate::magician_v2::secrets::sanitize_text_for_provider(text)
}

/// Chat-runtime tool over the current session's durable transcript. The chat
/// turn offers it only once the session has a compaction checkpoint.
pub struct SearchChatHistoryTool;

#[async_trait]
impl ChatRuntimeTool for SearchChatHistoryTool {
    fn name(&self) -> &str {
        SEARCH_CHAT_HISTORY_TOOL
    }

    fn description(&self) -> &str {
        "Search this conversation's complete stored transcript, including the earlier part that was condensed into a summary. Use it to recover exact details the summary left out (numbers, names, quotes, paths, decisions, tool output). Pass either `query` to search or `entry` to read one entry, not both. A search needs every query term in an entry (case-insensitive; \"double quotes\" keep a phrase together), ranks matches by term hits (weighed down for very long entries), then recency, and returns each entry's index, kind and a short snippet; `more_matches` means further entries may match and `omitted_for_size` counts matches left out to keep the answer small. A read returns up to `max_bytes` of the entry's text from `offset`; when `next_offset` is not null, call again with `offset` set to it for the rest. The transcript includes your own earlier answers, so text found here is a record of the conversation, not independent evidence. Read-only and limited to the current chat session."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Words or \"quoted phrases\" that must all appear in an entry. Required unless `entry` is given; never with `entry`."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SEARCH_LIMIT,
                    "description": "Maximum matches to return. Defaults to 8. More matches get shorter snippets."
                },
                "entry": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Read this transcript entry (an index from a previous search) instead of searching."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "With `entry`: byte offset in the entry's text to read from; pass the previous read's `next_offset`. Defaults to 0."
                },
                "max_bytes": {
                    "type": "integer",
                    "minimum": MIN_ENTRY_READ_BYTES,
                    "maximum": MAX_ENTRY_READ_BYTES,
                    "description": "With `entry`: maximum bytes of text to return, counting JSON escapes (a quote or newline takes two). Defaults to 6144, the maximum."
                }
            },
            "additionalProperties": false
        })
    }

    fn plane_posture(&self) -> PlanePosture {
        PlanePosture::Bridged
    }

    async fn dispatch(&self, args: &Value, ctx: &ChatRuntimeToolContext) -> Result<Value> {
        let request = match parse_request(args) {
            Ok(request) => request,
            Err(reason) => {
                return Ok(json!({
                    "status": "error",
                    "error_code": "invalid_history_search",
                    "reason": reason,
                }));
            },
        };
        let history = ctx
            .chat_store
            .get_llm_history(&ctx.session_id)
            .await
            .context("search_chat_history transcript load failed")?;
        let checkpoint = ctx
            .chat_store
            .get_context_checkpoint(&ctx.session_id)
            .await
            .ok()
            .flatten();
        // Rendering, matching and sanitizing a long transcript is CPU work;
        // keep it off the chat worker and its stack.
        tokio::task::spawn_blocking(move || {
            // A checkpoint for another version of the transcript says nothing
            // about which of these entries the model sees.
            let compaction = checkpoint
                .filter(|checkpoint| checkpoint.matches(&history))
                .map(|checkpoint| CompactionBoundary {
                    covered_entries: checkpoint.covered_entries,
                    preserved_user_entry: checkpoint.preserved_user_entry,
                });
            history_search_response(&history, &request, compaction, &provider_sanitize)
        })
        .await
        .context("search_chat_history worker failed")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::magician_v2::chat::models::StoredToolCall;

    fn identity(text: &str) -> String {
        text.to_string()
    }

    fn user(text: &str) -> ChatLlmTranscriptEntry {
        ChatLlmTranscriptEntry::UserText {
            text: text.to_string(),
        }
    }

    fn assistant(text: &str) -> ChatLlmTranscriptEntry {
        ChatLlmTranscriptEntry::AssistantTurn {
            text: Some(text.to_string()),
            tool_calls: Vec::new(),
            provider_state: None,
        }
    }

    /// JSON-encoded size of `text` without its quotes.
    fn encoded_len(text: &str) -> usize {
        serde_json::to_string(text).expect("string encodes").len() - 2
    }

    fn transcript() -> Vec<ChatLlmTranscriptEntry> {
        vec![
            user("My flight is QF12 on the 14th, seat 32A."),
            assistant("Noted: flight QF12, seat 32A."),
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                tool_calls: vec![StoredToolCall {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: json!({"path": "/tmp/Budget.xlsx"}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("read_file".to_string()),
                content: "budget total 4,200 EUR; flight QF12 refundable".to_string(),
            },
            ChatLlmTranscriptEntry::UserTurn {
                content: vec![TranscriptBlock::Text {
                    text: "What about the hotel? flight flight".to_string(),
                }],
            },
        ]
    }

    #[test]
    fn renders_every_entry_kind_with_tool_names_and_calls() {
        let entries = transcript();
        let call = render_entry(&entries[2]);
        assert_eq!(call.kind, HistoryEntryKind::Assistant);
        assert!(call.text.contains("[tool call read_file]"), "{}", call.text);
        assert!(call.text.contains("/tmp/Budget.xlsx"), "{}", call.text);
        let result = render_entry(&entries[3]);
        assert_eq!(result.kind, HistoryEntryKind::ToolResult);
        assert_eq!(result.tool_name.as_deref(), Some("read_file"));
        assert_eq!(render_entry(&entries[4]).kind, HistoryEntryKind::User);
        assert!(!result.truncated);
    }

    #[test]
    fn a_value_past_the_rendering_limits_marks_the_entry_incomplete() {
        let mut deep = json!(1);
        for _ in 0..(MAX_JSON_RENDER_DEPTH + 4) {
            deep = json!([deep]);
        }
        let entry = ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("calling".to_string()),
            tool_calls: vec![StoredToolCall {
                id: "call-deep".to_string(),
                name: "write_file".to_string(),
                arguments: deep,
            }],
            provider_state: None,
        };
        let rendered = render_entry(&entry);
        assert!(rendered.text.contains("[value exceeds rendering limits]"));
        assert!(rendered.truncated);
        let read = read_entry(&[entry], 0, 0, DEFAULT_ENTRY_READ_BYTES, &identity).expect("entry");
        assert_eq!(read.next_offset, None);
        assert!(read.truncated, "the placeholder is not the whole entry");
    }

    #[test]
    fn search_requires_every_term_and_ignores_case() {
        let entries = transcript();
        let hits = search_history(&entries, "qf12 SEAT", 8, &identity);
        let indexes: Vec<usize> = hits.iter().map(|hit| hit.entry).collect();
        assert_eq!(indexes, vec![1, 0]);
        assert!(search_history(&entries, "qf12 lounge", 8, &identity).is_empty());
        let budget = search_history(&entries, "budget.xlsx", 8, &identity);
        assert_eq!(budget.len(), 1);
        assert_eq!(budget[0].entry, 2);
    }

    #[test]
    fn ranks_by_hit_count_then_recency_and_honours_limit() {
        let entries = transcript();
        let hits = search_history(&entries, "flight", 8, &identity);
        let order: Vec<(usize, usize)> = hits.iter().map(|hit| (hit.entry, hit.hits)).collect();
        // Entry 4 says "flight" twice; the single-hit entries follow newest first.
        assert_eq!(order, vec![(4, 2), (3, 1), (1, 1), (0, 1)]);
        let limited = search_history_page(&entries, "flight", 2, &identity);
        assert_eq!(limited.hits.len(), 2);
        assert_eq!(limited.hits[0].entry, 4);
        assert!(limited.more_matches);
        assert!(!search_history_page(&entries, "flight", 8, &identity).more_matches);
    }

    #[test]
    fn long_entries_do_not_outrank_short_answers_by_volume() {
        let filler = "lorem ipsum dolor sit amet ".repeat(20_000);
        let entries = vec![
            user("The invoice number is INV-7."),
            user(&format!("invoice {filler} invoice {filler} invoice")),
        ];
        let hits = search_history(&entries, "invoice", 8, &identity);
        let order: Vec<(usize, usize)> = hits.iter().map(|hit| (hit.entry, hit.hits)).collect();
        assert_eq!(order, vec![(0, 1), (1, 3)]);
    }

    #[test]
    fn search_skips_its_own_calls_and_results() {
        let entries = vec![
            user("The deposit is 4200 EUR."),
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("Let me look that up.".to_string()),
                tool_calls: vec![StoredToolCall {
                    id: "call-search".to_string(),
                    name: SEARCH_CHAT_HISTORY_TOOL.to_string(),
                    arguments: json!({"query": "deposit 4200"}),
                }],
                provider_state: None,
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-search".to_string(),
                tool_name: Some(SEARCH_CHAT_HISTORY_TOOL.to_string()),
                content: "deposit 4200 deposit 4200 deposit 4200".to_string(),
            },
            // A legacy result without a tool name is known by its call id.
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "call-search".to_string(),
                tool_name: None,
                content: "deposit 4200 deposit 4200".to_string(),
            },
        ];
        let hits = search_history(&entries, "deposit 4200", 8, &identity);
        assert_eq!(
            hits.iter().map(|hit| hit.entry).collect::<Vec<_>>(),
            vec![0]
        );
        // The turn's own text is still searchable; only the call is left out.
        let turn = search_history(&entries, "look", 8, &identity);
        assert_eq!(turn.len(), 1);
        assert_eq!(turn[0].entry, 1);
        assert!(!turn[0].snippet.contains(SEARCH_CHAT_HISTORY_TOOL));
        // Reading by number still shows them.
        let read = read_entry(&entries, 2, 0, DEFAULT_ENTRY_READ_BYTES, &identity).expect("entry");
        assert_eq!(read.tool_name.as_deref(), Some(SEARCH_CHAT_HISTORY_TOOL));
        assert!(read.text.contains("deposit 4200"));
    }

    #[test]
    fn quoted_phrases_stay_one_term() {
        assert_eq!(
            query_terms("\"Seat 32A\" qf12 QF12"),
            vec!["seat 32a".to_string(), "qf12".to_string()]
        );
        let entries = transcript();
        let hits = search_history(&entries, "\"seat 32a\"", 8, &identity);
        assert_eq!(
            hits.iter().map(|hit| hit.entry).collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert!(search_history(&entries, "\"32A seat\"", 8, &identity).is_empty());
    }

    #[test]
    fn snippet_is_a_bounded_window_around_the_first_hit() {
        let filler = "x".repeat(2_000);
        let entries = vec![user(&format!("{filler} the code is ZEBRA-77 {filler}"))];
        let hits = search_history(&entries, "zebra-77", 8, &identity);
        let snippet = &hits[0].snippet;
        assert!(
            snippet.contains("ZEBRA-77"),
            "original case kept: {snippet}"
        );
        assert!(snippet.starts_with(ELLIPSIS) && snippet.ends_with(ELLIPSIS));
        assert!(
            encoded_len(snippet) <= MAX_SNIPPET_BYTES,
            "{}",
            snippet.len()
        );
    }

    #[test]
    fn snippet_is_char_boundary_safe_with_multibyte_text() {
        let text = format!("{}İstanbul trip ✈ {}", "é".repeat(400), "日本".repeat(400));
        let entries = vec![user(&text)];
        let hits = search_history(&entries, "trip", 8, &identity);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("trip"));
    }

    #[test]
    fn snippets_come_from_sanitized_text() {
        let entries = vec![user("token sk-live-SECRET for the deploy")];
        let redact = |text: &str| text.replace("sk-live-SECRET", "[REDACTED]");
        let hits = search_history(&entries, "deploy", 8, &redact);
        assert!(!hits[0].snippet.contains("SECRET"), "{}", hits[0].snippet);
        let read = read_entry(&entries, 0, 0, DEFAULT_ENTRY_READ_BYTES, &redact).expect("entry");
        assert!(!read.text.contains("SECRET"));
    }

    #[test]
    fn a_match_inside_a_redacted_secret_is_not_confirmed() {
        let redact = |text: &str| text.replace("sk-live-SECRET42", "[REDACTED]");
        let short = vec![user("deploy token sk-live-SECRET42")];
        assert!(search_history(&short, "secret42", 8, &redact).is_empty());
        assert!(search_history(&short, "deploy sk-live", 8, &redact).is_empty());
        assert_eq!(search_history(&short, "deploy", 8, &redact).len(), 1);
        // A long prose entry is matched raw but verified on sanitized windows.
        let filler = "plain words here ".repeat(8_000);
        let long = vec![user(&format!(
            "{filler} deploy token sk-live-SECRET42 {filler}"
        ))];
        assert!(search_history(&long, "secret42", 8, &redact).is_empty());
        let deploy = search_history(&long, "deploy", 8, &redact);
        assert_eq!(deploy.len(), 1);
        assert!(
            !deploy[0].snippet.contains("SECRET42"),
            "{}",
            deploy[0].snippet
        );
    }

    #[test]
    fn long_prose_matches_sanitize_windows_not_the_whole_entry() {
        let filler = "plain words here ".repeat(20_000);
        let entries = vec![user(&format!("{filler} the needle is here {filler}"))];
        let largest = Cell::new(0usize);
        let sanitize = |text: &str| {
            largest.set(largest.get().max(text.len()));
            text.to_string()
        };
        let hits = search_history(&entries, "needle", 8, &sanitize);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("the needle is here"));
        assert!(
            largest.get() <= 2 * SANITIZE_MARGIN_BYTES + "needle".len(),
            "sanitized {} bytes of a {} byte entry",
            largest.get(),
            render_entry(&entries[0]).text.len()
        );
    }

    #[test]
    fn entry_read_is_capped_and_reports_truncation() {
        let long = "é".repeat(1_000);
        let entries = vec![user(&long), assistant("short")];
        let read = read_entry(&entries, 0, 0, 301, &identity).expect("entry");
        assert!(read.truncated);
        assert!(read.text.len() <= 301);
        assert!(long.starts_with(&read.text));
        assert_eq!(read.next_offset, Some(read.text.len()));
        let short = read_entry(&entries, 1, 0, DEFAULT_ENTRY_READ_BYTES, &identity).expect("entry");
        assert_eq!(short.text, "short");
        assert!(!short.truncated);
        assert_eq!(short.next_offset, None);
        assert!(read_entry(&entries, 2, 0, DEFAULT_ENTRY_READ_BYTES, &identity).is_none());
    }

    #[test]
    fn entry_reads_page_through_offset_and_next_offset() {
        let text: String = (0..3_000)
            .map(|line| format!("line \"{line}\"\n"))
            .collect();
        let entries = vec![user(&text)];
        let mut offset = 0usize;
        let mut pages = Vec::new();
        loop {
            let read =
                read_entry(&entries, 0, offset, MAX_ENTRY_READ_BYTES, &identity).expect("entry");
            assert_eq!(read.offset, offset);
            assert!(encoded_len(&read.text) <= MAX_ENTRY_READ_BYTES);
            assert!(!read.text.is_empty());
            pages.push(read.text);
            match read.next_offset {
                Some(next) => {
                    assert!(read.truncated);
                    offset = next;
                },
                None => {
                    assert!(!read.truncated);
                    break;
                },
            }
        }
        assert!(pages.len() > 1);
        assert_eq!(pages.concat(), text);
        // An offset inside a character snaps back to its start.
        let multibyte = vec![user(&"é".repeat(10))];
        let snapped = read_entry(&multibyte, 0, 3, MAX_ENTRY_READ_BYTES, &identity).expect("entry");
        assert_eq!(snapped.offset, 2);
        assert_eq!(snapped.text, "é".repeat(9));
        // Past the end is an empty last page.
        let past = read_entry(&multibyte, 0, 999, MAX_ENTRY_READ_BYTES, &identity).expect("entry");
        assert_eq!(past.text, "");
        assert_eq!(past.next_offset, None);
    }

    #[test]
    fn long_prose_reads_sanitize_a_prefix_and_still_page_exactly() {
        let text: String = (0..20_000).map(|line| format!("row {line} ")).collect();
        assert!(text.len() > WHOLE_SANITIZE_MAX_BYTES);
        let entries = vec![user(&text)];
        let largest = Cell::new(0usize);
        let sanitize = |text: &str| {
            largest.set(largest.get().max(text.len()));
            text.to_string()
        };
        let first = read_entry(&entries, 0, 0, MAX_ENTRY_READ_BYTES, &sanitize).expect("entry");
        assert!(largest.get() <= MAX_ENTRY_READ_BYTES + 2 * SANITIZE_MARGIN_BYTES);
        assert_eq!(first.next_offset, Some(first.text.len()));
        let mut pages = vec![first.text];
        let mut next = first.next_offset;
        while let Some(offset) = next {
            let read =
                read_entry(&entries, 0, offset, MAX_ENTRY_READ_BYTES, &sanitize).expect("entry");
            pages.push(read.text);
            next = read.next_offset;
        }
        assert_eq!(pages.concat(), text);
    }

    #[test]
    fn search_answers_stay_inside_the_projection_budget() {
        let filler = "words \"quoted\" and more\n".repeat(150);
        let entries: Vec<ChatLlmTranscriptEntry> = (0..25)
            .map(|index| user(&format!("{filler} needle {index} {filler}")))
            .collect();
        let request = HistoryRequest::Search {
            query: "needle".to_string(),
            limit: MAX_SEARCH_LIMIT,
        };
        let value = history_search_response(&entries, &request, None, &identity);
        let records = value["entries"].as_array().expect("entries");
        let omitted = value["omitted_for_size"].as_u64().expect("count") as usize;
        assert_eq!(records.len() + omitted, MAX_SEARCH_LIMIT);
        assert!(records.len() > MAX_SEARCH_LIMIT / 2, "{}", records.len());
        assert_eq!(value["more_matches"], true);
        for record in records {
            assert!(record["snippet"].as_str().unwrap().contains("needle"));
        }
        let entries_bytes = serde_json::to_string(&value["entries"]).unwrap().len();
        assert!(
            entries_bytes <= MAX_SEARCH_RESPONSE_BYTES + 2,
            "{entries_bytes}"
        );
        // The whole answer fits the chat surface's ~8 KiB model budget.
        assert!(serde_json::to_string(&value).unwrap().len() < 7 * 1024);

        let read = history_search_response(
            &[user(&"x\"".repeat(10_000))],
            &HistoryRequest::Entry {
                index: 0,
                offset: 0,
                max_bytes: MAX_ENTRY_READ_BYTES,
            },
            None,
            &identity,
        );
        assert!(read["text"].as_str().unwrap().len() <= MAX_ENTRY_READ_BYTES);
        assert!(serde_json::to_string(&read).unwrap().len() < 7 * 1024);
    }

    #[test]
    fn oversized_entries_render_within_the_cap() {
        let entry = user(&"a".repeat(64));
        let rendered = render_entry_with_cap(&entry, 10, false);
        assert_eq!(rendered.text.len(), 10);
        assert!(rendered.truncated);
    }

    #[test]
    fn parse_request_validates_and_clamps() {
        assert_eq!(
            parse_request(&json!({"query": " hotel ", "limit": 99})),
            Ok(HistoryRequest::Search {
                query: "hotel".to_string(),
                limit: MAX_SEARCH_LIMIT,
            })
        );
        assert_eq!(
            parse_request(&json!({"query": "hotel"})),
            Ok(HistoryRequest::Search {
                query: "hotel".to_string(),
                limit: DEFAULT_SEARCH_LIMIT,
            })
        );
        assert_eq!(
            parse_request(&json!({"entry": 3, "max_bytes": 1_000_000})),
            Ok(HistoryRequest::Entry {
                index: 3,
                offset: 0,
                max_bytes: MAX_ENTRY_READ_BYTES,
            })
        );
        assert_eq!(
            parse_request(&json!({"entry": "4", "offset": 12_288})),
            Ok(HistoryRequest::Entry {
                index: 4,
                offset: 12_288,
                max_bytes: DEFAULT_ENTRY_READ_BYTES,
            })
        );
        assert_eq!(
            parse_request(&json!({"entry": 5, "query": "  "})),
            Ok(HistoryRequest::Entry {
                index: 5,
                offset: 0,
                max_bytes: DEFAULT_ENTRY_READ_BYTES,
            })
        );
        assert!(parse_request(&json!({})).is_err());
        assert!(parse_request(&json!({"query": "  \"\"  "})).is_err());
        assert!(parse_request(&json!({"entry": -1})).is_err());
        assert!(parse_request(&json!({"entry": 1, "query": "hotel"})).is_err());
        assert!(parse_request(&json!({"query": "hotel", "offset": 10})).is_err());
        assert!(parse_request(&json!({"entry": 1, "offset": -5})).is_err());
    }

    #[test]
    fn response_shape_carries_compaction_boundary() {
        let entries = transcript();
        let search = HistoryRequest::Search {
            query: "qf12".to_string(),
            limit: 8,
        };
        let boundary = CompactionBoundary {
            covered_entries: 2,
            preserved_user_entry: None,
        };
        let value = history_search_response(&entries, &search, Some(boundary), &identity);
        assert_eq!(value["total_entries"], 5);
        assert_eq!(value["compacted_through"], 2);
        assert_eq!(value["note"], ENTRY_NOTE);
        assert_eq!(value["more_matches"], false);
        assert_eq!(value["omitted_for_size"], 0);
        let first = &value["entries"][0];
        assert_eq!(first["entry"], 3);
        assert_eq!(first["kind"], "tool_result");
        assert_eq!(first["tool_name"], "read_file");
        assert_eq!(first["compacted"], false);
        let compacted_of = |value: &Value, entry: usize| {
            value["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hit| hit["entry"] == entry)
                .expect("entry matches")["compacted"]
                .clone()
        };
        assert_eq!(compacted_of(&value, 0), true);
        assert_eq!(compacted_of(&value, 1), true);

        // The preserved user entry is still in the model's view.
        let preserved = CompactionBoundary {
            covered_entries: 2,
            preserved_user_entry: Some(0),
        };
        let value = history_search_response(&entries, &search, Some(preserved), &identity);
        assert_eq!(compacted_of(&value, 0), false);
        assert_eq!(compacted_of(&value, 1), true);

        let uncompacted = history_search_response(&entries, &search, None, &identity);
        assert!(uncompacted["compacted_through"].is_null());

        let read = history_search_response(
            &entries,
            &HistoryRequest::Entry {
                index: 0,
                offset: 0,
                max_bytes: DEFAULT_ENTRY_READ_BYTES,
            },
            Some(boundary),
            &identity,
        );
        assert_eq!(read["kind"], "user");
        assert_eq!(read["text"], "My flight is QF12 on the 14th, seat 32A.");
        assert_eq!(read["offset"], 0);
        assert!(read["next_offset"].is_null());
        assert_eq!(read["truncated"], false);
        assert_eq!(read["compacted"], true);

        let missing = history_search_response(
            &entries,
            &HistoryRequest::Entry {
                index: 9,
                offset: 0,
                max_bytes: DEFAULT_ENTRY_READ_BYTES,
            },
            None,
            &identity,
        );
        assert_eq!(missing["error_code"], "history_entry_out_of_range");
    }

    #[test]
    fn recalled_text_is_agent_written_evidence() {
        assert_eq!(
            result_evidence_origin(SEARCH_CHAT_HISTORY_TOOL),
            EvidenceOrigin::AgentWritten
        );
        assert_eq!(
            result_evidence_origin("read_file"),
            EvidenceOrigin::ToolResult
        );
    }

    #[test]
    fn tool_schema_is_offered_under_its_name() {
        let spec = SearchChatHistoryTool.to_tool_spec();
        assert_eq!(spec.name, SEARCH_CHAT_HISTORY_TOOL);
        assert_eq!(spec.parameters["properties"]["limit"]["maximum"], 20);
        assert_eq!(
            spec.parameters["properties"]["max_bytes"]["maximum"],
            MAX_ENTRY_READ_BYTES
        );
        assert!(spec.parameters["properties"]["offset"].is_object());
        assert_eq!(SearchChatHistoryTool.plane_posture(), PlanePosture::Bridged);
    }
}
