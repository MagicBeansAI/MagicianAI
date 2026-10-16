//! Deterministic Gmail body parsing for the local distillation pipeline
//! (Channel Assist Phase 1b, N2).
//!
//! Design: `docs/plans/2026-07-05-channel-assist-phase1b-design.md`
//! ("Local-only distillation" — Gmail content path). Everything here is a
//! PURE function over the in-memory MIME tree that
//! [`super::gws_client::GwsGmailClient::get_message_full`] returns:
//!
//! 1. [`extract_text`] — MIME walk preferring the plain-text rendition
//!    (`multipart/alternative` picks `text/plain`; HTML-only messages fall
//!    back to a minimal hand-rolled tag/entity stripper), base64url decode,
//!    UTF-8-lossy. Attachment PRESENCE is counted from part filenames;
//!    attachment content is never decoded (its `attachmentId` is not even
//!    deserialized upstream).
//! 2. [`strip_quoted_history`] — trailing quoted reply history
//!    ("On … wrote:" + `>`-runs), Outlook/forward separators, `-- `
//!    signatures and device footers. CONSERVATIVE by construction: only
//!    trailing regions and exact separator lines are cut — quoted-looking
//!    text mid-message (inline replies) is kept.
//! 3. [`prepare_for_distill`] — whitespace normalization + greedy
//!    paragraph-boundary chunking with a chunk-count cap, so the local
//!    model sees bounded, coherent pieces.
//! 4. [`build_header_document`] — the metadata header block N3 prepends to
//!    the distiller input. Produced from already-persisted metadata only.
//!
//! # Privacy contract
//!
//! Inputs and outputs of this module carry BODY CONTENT and exist only in
//! process memory: nothing here persists, logs, or serializes text, and no
//! type in this module derives `Serialize`. The N3 distill queue consumes
//! the chunks, derives `{summary, intent}` on the local model, and discards
//! the text. This module never calls the distiller.

use std::sync::LazyLock;

use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine as _;
use regex::Regex;

use super::gws_client::{GmailFullMessage, GmailMimePart};
use super::types::MailMessageMeta;

/// Default per-chunk budget handed to the local model. The distiller is
/// first-chunk-only (see `render_distill_prompts`), so this is the effective
/// coalesce-content lever: chunk[0] is the merged same-thread text that
/// actually reaches the model. ~10000 chars stays well inside the local model's
/// context alongside the prompt while carrying more of a coalesced thread.
pub const DEFAULT_DISTILL_CHUNK_CHARS: usize = 10000;

/// Default cap on chunks per message — beyond this the tail is dropped and
/// the diagnostics flag `truncated`. Because distillation is first-chunk-only,
/// this bounds the `truncated` note, not the content sent (raise
/// `DEFAULT_DISTILL_CHUNK_CHARS` to grow the effective coalesce window).
pub const DEFAULT_DISTILL_MAX_CHUNKS: usize = 8;

/// Floor for the per-chunk budget so pathological env values cannot shred
/// messages into confetti.
const MIN_CHUNK_CHARS: usize = 200;

/// Env-tunable per-chunk char budget (`CHANNEL_DISTILL_CHUNK_CHARS`,
/// default [`DEFAULT_DISTILL_CHUNK_CHARS`]). The N3 queue reads this once
/// and passes it into the pure [`prepare_for_distill`].
pub fn distill_chunk_chars() -> usize {
    std::env::var("CHANNEL_DISTILL_CHUNK_CHARS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&v| v >= MIN_CHUNK_CHARS)
        .unwrap_or(DEFAULT_DISTILL_CHUNK_CHARS)
}

/// Env-tunable chunk-count cap (`CHANNEL_DISTILL_MAX_CHUNKS`, default
/// [`DEFAULT_DISTILL_MAX_CHUNKS`]).
pub fn distill_max_chunks() -> usize {
    std::env::var("CHANNEL_DISTILL_MAX_CHUNKS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_DISTILL_MAX_CHUNKS)
}

// ---------------------------------------------------------------------------
// 1. MIME walk → text
// ---------------------------------------------------------------------------

/// Result of [`extract_text`]. In-memory only — deliberately not
/// `Serialize` (see the module privacy contract).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedBody {
    /// Concatenated text segments (blank-line separated), raw — quote
    /// stripping and normalization happen in the later stages.
    pub text: String,
    /// True when any rendered segment came from `text/html` (i.e. the tag
    /// stripper ran) — a quality diagnostic for the distiller.
    pub had_html: bool,
    /// Number of attachment parts (non-empty `filename`) seen anywhere in
    /// the rendered tree. PRESENCE only; content is never decoded.
    pub attachment_count: usize,
}

/// Walk the `format=full` MIME tree and extract readable text.
///
/// Rules (deterministic, no heuristics):
/// - parts with a non-empty `filename` are attachments: counted, never
///   decoded — even when their MIME type is `text/*`;
/// - `multipart/alternative` picks ONE rendition: the first child whose
///   subtree yields text purely from `text/plain`, else the first child
///   yielding any text (the HTML fallback);
/// - other containers (`multipart/mixed`/`related`/…) concatenate their
///   children in order;
/// - `text/plain` leaves are base64url-decoded (UTF-8 lossy); `text/html`
///   leaves are decoded then tag/entity-stripped; all other leaf types are
///   skipped.
pub fn extract_text(payload: &GmailMimePart) -> ExtractedBody {
    let mut acc = Collected::default();
    collect(payload, &mut acc);
    ExtractedBody {
        text: acc.segments.join("\n\n"),
        had_html: acc.had_html,
        attachment_count: acc.attachment_count,
    }
}

#[derive(Debug, Default)]
struct Collected {
    segments: Vec<String>,
    had_html: bool,
    attachment_count: usize,
}

impl Collected {
    fn merge(&mut self, other: Collected) {
        self.segments.extend(other.segments);
        self.had_html |= other.had_html;
        self.attachment_count += other.attachment_count;
    }
}

fn is_attachment(part: &GmailMimePart) -> bool {
    part.filename
        .as_deref()
        .is_some_and(|f| !f.trim().is_empty())
}

fn collect(part: &GmailMimePart, acc: &mut Collected) {
    if is_attachment(part) {
        acc.attachment_count += 1;
        return;
    }
    let mime = part.mime_type.as_deref().unwrap_or("").to_ascii_lowercase();

    if mime.starts_with("multipart/alternative") {
        // Render every child, then keep exactly one rendition: prefer the
        // first whose text came purely from text/plain (the "deepest
        // text/plain" preference — nesting recurses), else the first with
        // any text at all (HTML fallback).
        let rendered: Vec<Collected> = part
            .parts
            .iter()
            .map(|child| {
                let mut sub = Collected::default();
                collect(child, &mut sub);
                sub
            })
            .collect();
        let chosen = rendered
            .iter()
            .position(|c| !c.segments.is_empty() && !c.had_html)
            .or_else(|| rendered.iter().position(|c| !c.segments.is_empty()));
        match chosen {
            Some(idx) => {
                for (i, sub) in rendered.into_iter().enumerate() {
                    if i == idx {
                        acc.merge(sub);
                    } else {
                        // Unchosen renditions contribute attachment
                        // PRESENCE only (never their text twice).
                        acc.attachment_count += sub.attachment_count;
                    }
                }
            },
            None => {
                for sub in rendered {
                    acc.attachment_count += sub.attachment_count;
                }
            },
        }
        return;
    }

    if !part.parts.is_empty() {
        for child in &part.parts {
            collect(child, acc);
        }
        return;
    }

    // Leaf part.
    let Some(data) = part.body.as_ref().and_then(|b| b.data.as_deref()) else {
        return;
    };
    if mime.is_empty() || mime.starts_with("text/plain") {
        if let Some(text) = decode_body_data(data) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                acc.segments.push(trimmed.to_string());
            }
        }
    } else if mime.starts_with("text/html") {
        acc.had_html = true;
        if let Some(raw) = decode_body_data(data) {
            let stripped = strip_html(&raw);
            let trimmed = stripped.trim();
            if !trimmed.is_empty() {
                acc.segments.push(trimmed.to_string());
            }
        }
    }
    // Any other leaf type (calendar invites, inline images without
    // filenames, …) is skipped: not text, not an attachment by contract.
}

/// Lenient base64url: Gmail emits URL-safe base64, usually unpadded but
/// padded variants occur; some producers embed line breaks. Decode
/// padding-indifferent, URL-safe alphabet first with a standard-alphabet
/// fallback, then UTF-8 lossy. Undecodable data yields `None` (part
/// skipped) rather than an error — one mangled part must not sink a
/// message.
fn decode_body_data(data: &str) -> Option<String> {
    const LENIENT: GeneralPurposeConfig =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    const B64_URL: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);
    const B64_STD: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);

    let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    let bytes = B64_URL
        .decode(compact.as_bytes())
        .or_else(|_| B64_STD.decode(compact.as_bytes()))
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// HTML → text (minimal hand-rolled stripper; the workspace has no direct
// html/entity crate dependency and N2 adds no new deps — checked
// Cargo.toml/Cargo.lock: `htmlescape` is transitive-only via tantivy)
// ---------------------------------------------------------------------------

/// Tags that imply a line/paragraph boundary when stripped.
const BLOCK_TAGS: [&str; 22] = [
    "br",
    "hr",
    "p",
    "div",
    "tr",
    "li",
    "ul",
    "ol",
    "table",
    "blockquote",
    "pre",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "title",
];

/// Strip HTML down to text: drops `<script>`/`<style>` (with contents) and
/// comments, maps block-level tags to newlines and other tags to spaces,
/// and decodes the common named + numeric character entities. Malformed
/// markup degrades conservatively (a bare `<` stays literal text).
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    loop {
        let Some(lt) = rest.find('<') else {
            push_entity_decoded(&mut out, rest);
            break;
        };
        push_entity_decoded(&mut out, &rest[..lt]);
        let after = &rest[lt + 1..];

        // Comment: skip through `-->` (unterminated → drop remainder;
        // comment innards must not surface as text).
        if let Some(after_bang) = after.strip_prefix("!--") {
            match after_bang.find("-->") {
                Some(end) => {
                    rest = &after_bang[end + 3..];
                    continue;
                },
                None => break,
            }
        }

        let (is_closing, name_src) = match after.strip_prefix('/') {
            Some(stripped) => (true, stripped),
            None => (false, after),
        };
        let name: String = name_src
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();

        if name.is_empty() && !after.starts_with('!') {
            // Not markup (e.g. "a < b") — keep the `<` literally.
            out.push('<');
            rest = after;
            continue;
        }

        let Some(gt) = after.find('>') else {
            // Unterminated tag at EOF: keep it as literal text.
            out.push('<');
            push_entity_decoded(&mut out, after);
            break;
        };
        let tail = &after[gt + 1..];

        if !is_closing && (name == "script" || name == "style") {
            // Skip contents through the matching close tag,
            // case-insensitively (ASCII lowering preserves byte offsets).
            let close = format!("</{name}");
            let lower = tail.to_ascii_lowercase();
            match lower.find(&close) {
                Some(k) => {
                    let after_close = &tail[k..];
                    match after_close.find('>') {
                        Some(g) => {
                            rest = &after_close[g + 1..];
                            continue;
                        },
                        None => break,
                    }
                },
                None => break,
            }
        }

        if BLOCK_TAGS.contains(&name.as_str()) {
            out.push('\n');
        } else if !name.is_empty() {
            out.push(' ');
        }
        rest = tail;
    }
    out
}

/// Append `text` to `out`, decoding `&amp;`-style entities. Unknown
/// entities are kept literally.
fn push_entity_decoded(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp + 1..];
        match tail.find(';') {
            // Entity names are short; anything longer is prose punctuation.
            Some(semi) if semi <= 10 => match decode_entity(&tail[..semi]) {
                Some(c) => {
                    out.push(c);
                    rest = &tail[semi + 1..];
                },
                None => {
                    out.push('&');
                    rest = tail;
                },
            },
            _ => {
                out.push('&');
                rest = tail;
            },
        }
    }
    out.push_str(rest);
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let num = entity.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            char::from_u32(code)
        },
    }
}

// ---------------------------------------------------------------------------
// 2. Quoted-history stripping (conservative)
// ---------------------------------------------------------------------------

static ORIGINAL_MESSAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*-{2,}\s*original message\s*-{2,}\s*$").expect("valid regex")
});
static FORWARD_SEP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*-{2,}\s*forwarded message\s*-{2,}\s*$").expect("valid regex")
});
static BEGIN_FORWARD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*begin forwarded message:?\s*$").expect("valid regex"));
/// Reply attribution ("On <date> <sender> wrote:"), matched against up to
/// three JOINED lines because clients hard-wrap it.
static ATTRIBUTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^On\b.*\bwrote:$").expect("valid regex"));
static DEVICE_FOOTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(sent from my .{1,60}|get outlook for (ios|android))$").expect("valid regex")
});

/// Remove trailing quoted reply history so the distiller sees the NEW
/// content of a message, not the whole thread ×N.
///
/// Cuts applied (all deterministic, earliest cut wins):
/// - the first exact separator line: `-----Original Message-----`,
///   `---------- Forwarded message ----------`, `Begin forwarded message:`;
/// - a TRAILING `>`-quoted run, together with the "On … wrote:"
///   attribution directly above it (attribution may wrap across up to
///   three lines);
/// - the last RFC 3676 `-- ` signature delimiter line (trailing space
///   REQUIRED — a bare `--` is prose/divider punctuation, not a
///   signature, and no longer cuts);
/// - trailing device footers ("Sent from my …", "Get Outlook for …").
///
/// CONSERVATIVE: quoted-looking text mid-message (inline replies below a
/// quote, prose after an attribution) is never touched — only regions that
/// extend to the end of the message and exact separator lines are cut. A
/// message that is nothing but quoted history strips to empty (it has no
/// new content).
pub fn strip_quoted_history(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut cut = lines.len();

    // (a) First hard separator line — everything below is history.
    for (idx, line) in lines.iter().enumerate() {
        if ORIGINAL_MESSAGE_RE.is_match(line)
            || FORWARD_SEP_RE.is_match(line)
            || BEGIN_FORWARD_RE.is_match(line)
        {
            cut = idx;
            break;
        }
    }

    // (b) Trailing quoted region (+ optional attribution directly above).
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut qstart = end;
    while qstart > 0 {
        let line = lines[qstart - 1];
        if line.trim().is_empty() || line.trim_start().starts_with('>') {
            qstart -= 1;
        } else {
            break;
        }
    }
    let has_trailing_quote = lines[qstart..end]
        .iter()
        .any(|l| l.trim_start().starts_with('>'));
    if has_trailing_quote {
        let mut cut_b = qstart;
        for take in 1..=3usize {
            if qstart < take {
                break;
            }
            let joined = lines[qstart - take..qstart].join(" ");
            if ATTRIBUTION_RE.is_match(joined.trim()) {
                cut_b = qstart - take;
                break;
            }
            if lines[qstart - take].trim().is_empty() {
                break;
            }
        }
        cut = cut.min(cut_b);
    }

    // (c) Last `-- ` signature delimiter before the cut — the RFC 3676
    // form exactly (dash dash SPACE). Bare `--` deliberately does not
    // match: it appears as a prose divider/em-dash stand-in far more
    // often than as a malformed signature delimiter.
    for idx in (0..cut).rev() {
        if lines[idx] == "-- " {
            cut = idx;
            break;
        }
    }

    // (d) Drop trailing blanks and device footers from what's kept.
    let mut kept: Vec<&str> = lines[..cut].to_vec();
    while let Some(last) = kept.last() {
        if last.trim().is_empty() || DEVICE_FOOTER_RE.is_match(last.trim()) {
            kept.pop();
        } else {
            break;
        }
    }
    kept.join("\n")
}

// ---------------------------------------------------------------------------
// 3. Normalization + chunking
// ---------------------------------------------------------------------------

/// Result of [`prepare_for_distill`]. In-memory only — not `Serialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedText {
    /// Paragraph-boundary chunks, each ≤ the char budget.
    pub chunks: Vec<String>,
    /// True when the chunk-count cap dropped tail content.
    pub truncated: bool,
}

/// Normalize whitespace and split into paragraph-boundary chunks of at
/// most `max_chars` characters (floored at an internal minimum), capped at
/// `max_chunks` chunks. Callers pass [`distill_chunk_chars`] /
/// [`distill_max_chunks`] for the env-tunable defaults; the function
/// itself stays pure.
pub fn prepare_for_distill(text: &str, max_chars: usize, max_chunks: usize) -> PreparedText {
    let max_chars = max_chars.max(MIN_CHUNK_CHARS);
    let max_chunks = max_chunks.max(1);
    let normalized = normalize_whitespace(text);
    if normalized.is_empty() {
        return PreparedText::default();
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    for paragraph in normalized.split("\n\n") {
        for piece in split_to_size(paragraph, max_chars) {
            let piece_chars = piece.chars().count();
            if current.is_empty() {
                current = piece;
                current_chars = piece_chars;
            } else if current_chars + 2 + piece_chars <= max_chars {
                current.push_str("\n\n");
                current.push_str(&piece);
                current_chars += 2 + piece_chars;
            } else {
                chunks.push(std::mem::take(&mut current));
                current = piece;
                current_chars = piece_chars;
            }
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }

    let truncated = chunks.len() > max_chunks;
    chunks.truncate(max_chunks);
    PreparedText { chunks, truncated }
}

/// CRLF→LF, per-line inline-whitespace collapse (tabs/NBSP → single
/// spaces, edges trimmed), blank-line runs collapsed to one paragraph
/// break, outer blanks trimmed.
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut started = false;
    let mut pending_blank = false;
    for raw_line in text.lines() {
        let line = collapse_inline_whitespace(raw_line);
        if line.is_empty() {
            if started {
                pending_blank = true;
            }
            continue;
        }
        if started {
            out.push('\n');
            if pending_blank {
                out.push('\n');
            }
        }
        out.push_str(&line);
        started = true;
        pending_blank = false;
    }
    out
}

fn collapse_inline_whitespace(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_ws = false;
    for c in line.chars() {
        if c == ' ' || c == '\t' || c == '\u{a0}' {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(c);
        }
    }
    out
}

/// Split one paragraph into pieces of ≤ `max_chars` characters, preferring
/// line boundaries, then word boundaries, then a hard character cut.
fn split_to_size(paragraph: &str, max_chars: usize) -> Vec<String> {
    let trimmed = paragraph.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.chars().count() <= max_chars {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut rest = trimmed;
    while rest.chars().count() > max_chars {
        // Byte offset of the (max_chars+1)-th char = window end.
        let hard = rest
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let window = &rest[..hard];
        let split_at = window
            .rfind('\n')
            .or_else(|| window.rfind(|c: char| c.is_whitespace()))
            .filter(|&i| i > 0)
            .unwrap_or(hard);
        out.push(rest[..split_at].trim_end().to_string());
        rest = rest[split_at..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

// ---------------------------------------------------------------------------
// Composed pipeline + header document (pure producers for N3)
// ---------------------------------------------------------------------------

/// Everything the N3 distill queue needs for one message: bounded chunks of
/// its NEW content plus parse diagnostics. In-memory only — not
/// `Serialize`; the queue discards it after distilling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistillContent {
    pub chunks: Vec<String>,
    pub truncated: bool,
    pub had_html: bool,
    pub attachment_count: usize,
}

/// Full deterministic pipeline for one `format=full` message:
/// [`extract_text`] → [`strip_quoted_history`] → [`prepare_for_distill`].
/// A missing payload yields empty content (metadata-only row).
pub fn prepare_message_content(
    full: &GmailFullMessage,
    max_chars: usize,
    max_chunks: usize,
) -> DistillContent {
    let Some(payload) = full.payload.as_ref() else {
        return DistillContent::default();
    };
    let extracted = extract_text(payload);
    let fresh = strip_quoted_history(&extracted.text);
    let prepared = prepare_for_distill(&fresh, max_chars, max_chunks);
    DistillContent {
        chunks: prepared.chunks,
        truncated: prepared.truncated,
        had_html: extracted.had_html,
        attachment_count: extracted.attachment_count,
    }
}

/// Header block the N3 queue prepends to the distiller input so the local
/// model sees who/what/when alongside the body chunk. Built exclusively
/// from ALREADY-PERSISTED metadata (subject, sender, recipient domains,
/// date, direction) plus the attachment PRESENCE count — never body
/// content, never persisted itself, and this function never calls the
/// distiller.
pub fn build_header_document(meta: &MailMessageMeta, attachment_count: usize) -> String {
    let nonempty = |s: &&str| !s.trim().is_empty();
    let mut lines: Vec<String> = Vec::new();
    if let Some(subject) = meta.subject.as_deref().filter(nonempty) {
        lines.push(format!("Subject: {}", subject.trim()));
    }
    match (
        meta.from_name.as_deref().filter(nonempty),
        meta.from_address.as_deref().filter(nonempty),
    ) {
        (Some(name), Some(addr)) => lines.push(format!("From: {name} <{addr}>")),
        (Some(name), None) => lines.push(format!("From: {name}")),
        (None, Some(addr)) => lines.push(format!("From: {addr}")),
        (None, None) => {},
    }
    if !meta.to_domains.is_empty() {
        lines.push(format!("To domains: {}", meta.to_domains.join(", ")));
    }
    if !meta.cc_domains.is_empty() {
        lines.push(format!("Cc domains: {}", meta.cc_domains.join(", ")));
    }
    if let Some(dt) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(meta.internal_date) {
        lines.push(format!("Date: {}", dt.to_rfc3339()));
    }
    if let Some(direction) = meta.direction {
        lines.push(format!("Direction: {}", direction.as_db_str()));
    }
    if attachment_count > 0 {
        lines.push(format!("Attachments: {attachment_count}"));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests — synthetic example.com prose only, per the standing rule.
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::{
        DistillState, MailRecordOrigin, MessageDirection, MAIL_ASSIST_SCHEMA_VERSION,
    };
    use super::*;
    use crate::channel_assist::gws_client::GmailMimeBody;

    fn leaf(mime: &str, data: &str) -> GmailMimePart {
        GmailMimePart {
            mime_type: Some(mime.to_string()),
            filename: None,
            body: Some(GmailMimeBody {
                data: Some(data.to_string()),
                size: None,
            }),
            parts: Vec::new(),
        }
    }

    fn container(mime: &str, parts: Vec<GmailMimePart>) -> GmailMimePart {
        GmailMimePart {
            mime_type: Some(mime.to_string()),
            filename: None,
            body: None,
            parts,
        }
    }

    fn attachment(mime: &str, filename: &str, data: Option<&str>) -> GmailMimePart {
        GmailMimePart {
            mime_type: Some(mime.to_string()),
            filename: Some(filename.to_string()),
            body: Some(GmailMimeBody {
                data: data.map(str::to_string),
                size: Some(1234),
            }),
            parts: Vec::new(),
        }
    }

    // base64url("Plain rendition body.\nSecond line of prose.")
    const PLAIN_B64: &str = "UGxhaW4gcmVuZGl0aW9uIGJvZHkuClNlY29uZCBsaW5lIG9mIHByb3NlLg";
    // base64url("<p>HTML rendition <b>body</b> &amp; extra markup.</p>")
    const HTML_ALT_B64: &str =
        "PHA-SFRNTCByZW5kaXRpb24gPGI-Ym9keTwvYj4gJmFtcDsgZXh0cmEgbWFya3VwLjwvcD4";
    // base64url("attachment prose that must never surface")
    const ATTACH_TEXT_B64: &str = "YXR0YWNobWVudCBwcm9zZSB0aGF0IG11c3QgbmV2ZXIgc3VyZmFjZQ";
    // base64url("Deeply nested plain rendition.")
    const NESTED_PLAIN_B64: &str = "RGVlcGx5IG5lc3RlZCBwbGFpbiByZW5kaXRpb24u";

    // -- extraction --------------------------------------------------------

    #[test]
    fn prefers_plain_rendition_in_multipart_alternative() {
        let payload = container(
            "multipart/alternative",
            vec![
                leaf("text/plain", PLAIN_B64),
                leaf("text/html", HTML_ALT_B64),
            ],
        );
        let extracted = extract_text(&payload);
        assert_eq!(
            extracted.text,
            "Plain rendition body.\nSecond line of prose."
        );
        assert!(!extracted.had_html, "chosen rendition is pure text/plain");
        assert_eq!(extracted.attachment_count, 0);
    }

    #[test]
    fn falls_back_to_html_when_no_plain_rendition() {
        let payload = container(
            "multipart/alternative",
            vec![leaf("text/html", HTML_ALT_B64)],
        );
        let extracted = extract_text(&payload);
        assert!(extracted.had_html);
        let prepared = prepare_for_distill(&extracted.text, 6000, 8);
        assert_eq!(
            prepared.chunks,
            vec!["HTML rendition body & extra markup.".to_string()]
        );
    }

    #[test]
    fn html_only_message_strips_tags_scripts_and_entities() {
        // base64url of an html document with <style>, <script>, a comment,
        // named + numeric entities, <br>, and headings.
        let html_b64 = "PGh0bWw-PGhlYWQ-PHN0eWxlPnB7Y29sb3I6cmVkfTwvc3R5bGU-PHNjcmlwdD52YXIgeD0xOzwvc2NyaXB0PjwvaGVhZD48Ym9keT48aDE-V2Vla2x5IGRpZ2VzdDwvaDE-PHA-Rmlyc3QgcGFyYWdyYXBoICZhbXA7IGZyaWVuZHMuPC9wPjxwPlNlY29uZCZuYnNwO3BhcmFncmFwaCAmbHQ7a2VwdCBsaXRlcmFsbHkmZ3Q7Ljxicj5MaW5lIGFmdGVyIGJyZWFrICYjODIxMjsgZGFzaCAmI3gyMDE0OyBhZ2Fpbi48L3A-PCEtLSBoaWRkZW4gY29tbWVudCAtLT48L2JvZHk-PC9odG1sPg";
        let extracted = extract_text(&leaf("text/html", html_b64));
        assert!(extracted.had_html);
        let text = prepare_for_distill(&extracted.text, 6000, 8)
            .chunks
            .join("\n\n");
        assert_eq!(
            text,
            "Weekly digest\n\nFirst paragraph & friends.\n\nSecond paragraph <kept literally>.\nLine after break \u{2014} dash \u{2014} again."
        );
        assert!(!text.contains("color:red"), "style contents dropped");
        assert!(!text.contains("var x"), "script contents dropped");
        assert!(!text.contains("hidden comment"), "comments dropped");
    }

    #[test]
    fn nested_mixed_extracts_text_and_counts_attachment_presence_only() {
        let payload = container(
            "multipart/mixed",
            vec![
                container(
                    "multipart/alternative",
                    vec![
                        leaf("text/plain", NESTED_PLAIN_B64),
                        leaf("text/html", HTML_ALT_B64),
                    ],
                ),
                attachment("application/pdf", "agenda.pdf", None),
                // A text/* attachment WITH inline data: presence counted,
                // content still never extracted.
                attachment("text/plain", "notes.txt", Some(ATTACH_TEXT_B64)),
            ],
        );
        let extracted = extract_text(&payload);
        assert_eq!(extracted.text, "Deeply nested plain rendition.");
        assert!(!extracted.text.contains("attachment prose"));
        assert_eq!(extracted.attachment_count, 2);
        assert!(!extracted.had_html);
    }

    #[test]
    fn unknown_leaf_types_are_skipped_without_attachment_count() {
        let payload = container(
            "multipart/mixed",
            vec![
                leaf("image/png", "aWJt"),        // inline image, no filename
                leaf("text/calendar", PLAIN_B64), // non-plain text type
            ],
        );
        let extracted = extract_text(&payload);
        assert!(extracted.text.is_empty());
        assert_eq!(extracted.attachment_count, 0);
        assert!(!extracted.had_html);
    }

    // -- base64url decode edges --------------------------------------------

    #[test]
    fn base64url_padding_and_alphabet_edges() {
        // Padded and unpadded forms both decode (padding-indifferent).
        assert_eq!(decode_body_data("YQ==").as_deref(), Some("a"));
        assert_eq!(decode_body_data("YQ").as_deref(), Some("a"));
        assert_eq!(decode_body_data("YWI=").as_deref(), Some("ab"));
        assert_eq!(decode_body_data("YWI").as_deref(), Some("ab"));
        // URL-safe alphabet ('-'/'_' present in the html fixture).
        assert!(decode_body_data(HTML_ALT_B64).is_some());
        // Standard-alphabet fallback ('/' in the data).
        assert_eq!(
            decode_body_data("c3RhbmRhcmQgYWxwaGFiZXQgYm9keSA+Pj4gb2s/").as_deref(),
            Some("standard alphabet body >>> ok?")
        );
        // Garbage → None, never an error/panic.
        assert!(decode_body_data("!!!not-base64!!!").is_none());
        assert!(decode_body_data("").is_none());
        assert!(decode_body_data("   \n  ").is_none());
    }

    #[test]
    fn whitespace_inside_body_data_is_tolerated() {
        let wrapped = "UGxhaW4gcmVuZGl0aW9u\nIGJvZHkuClNlY29uZCBs\naW5lIG9mIHByb3NlLg==";
        assert_eq!(
            decode_body_data(wrapped).as_deref(),
            Some("Plain rendition body.\nSecond line of prose.")
        );
    }

    #[test]
    fn invalid_utf8_decodes_lossily() {
        // base64 of b"caf\xe9 latte" (latin-1 e-acute — invalid UTF-8).
        let extracted = extract_text(&leaf("text/plain", "Y2Fm6SBsYXR0ZQ=="));
        assert_eq!(extracted.text, "caf\u{fffd} latte");
    }

    // -- html stripper edges -------------------------------------------------

    #[test]
    fn literal_less_than_and_unterminated_tag_kept() {
        assert_eq!(strip_html("a < b stays"), "a < b stays");
        assert_eq!(strip_html("tail <unclosedtag"), "tail <unclosedtag");
        assert_eq!(strip_html("x &unknownent; y"), "x &unknownent; y");
        assert_eq!(strip_html("&#65;&#x42;"), "AB");
    }

    // -- quoted-history stripping --------------------------------------------

    #[test]
    fn strips_trailing_gmail_attribution_and_quotes() {
        let text = "Thanks, that works for me.\n\nOn Tue, Jul 1, 2026 at 9:00 AM Alice Example <alice@example.com> wrote:\n> Earlier proposal text.\n> Second quoted line.";
        assert_eq!(strip_quoted_history(text), "Thanks, that works for me.");
    }

    #[test]
    fn strips_wrapped_attribution_across_lines() {
        let text = "Sounds good.\n\nOn Tue, Jul 1, 2026 at 9:00 AM Alice Example\n<alice@example.com> wrote:\n> Shall we move the sync?";
        assert_eq!(strip_quoted_history(text), "Sounds good.");
    }

    #[test]
    fn strips_outlook_original_message_block() {
        let text = "New note on top.\n\n-----Original Message-----\nFrom: Bob Example\nSent: Tuesday\nTo: team@example.com\nSubject: Old subject\n\nOld body text.";
        assert_eq!(strip_quoted_history(text), "New note on top.");
    }

    #[test]
    fn strips_forwarded_message_separators() {
        let gmail = "FYI, see below.\n\n---------- Forwarded message ---------\nFrom: Alice <alice@example.com>\n\nForwarded body text.";
        assert_eq!(strip_quoted_history(gmail), "FYI, see below.");

        let apple =
            "FYI.\n\nBegin forwarded message:\n\nFrom: Bob <bob@example.com>\nForwarded body.";
        assert_eq!(strip_quoted_history(apple), "FYI.");
    }

    #[test]
    fn strips_signature_delimiter_and_device_footer() {
        let sig = "Main point here.\n\n-- \nCarol Example\nexample.com | +1 555 0100";
        assert_eq!(strip_quoted_history(sig), "Main point here.");

        // Only the RFC 3676 form (`-- ` with trailing space) is a
        // signature delimiter; a bare `--` is a prose divider and keeps
        // everything below it.
        let bare = "Main point here.\n\n--\nSecond section of the same message.";
        assert_eq!(strip_quoted_history(bare), bare);

        let device = "Short reply.\n\nSent from my iPhone";
        assert_eq!(strip_quoted_history(device), "Short reply.");

        let outlook = "Another reply.\n\nGet Outlook for iOS";
        assert_eq!(strip_quoted_history(outlook), "Another reply.");
    }

    #[test]
    fn keeps_mid_message_quote_followed_by_reply() {
        // Inline-reply style: the quote is NOT trailing, so nothing is cut.
        let text = "Two points on the draft:\n\n> The schedule slips a week.\n\nThat part is fine to keep.\nSecond, the budget table needs a refresh.";
        assert_eq!(strip_quoted_history(text), text);
    }

    #[test]
    fn keeps_attribution_when_new_text_continues_below() {
        // Bottom-posted reply: attribution + quote mid-message, fresh prose
        // after — conservative keep of everything.
        let text = "On Tue, Jul 1, 2026 at 9:00 AM Bob <bob@example.com> wrote:\n> Can you send the agenda?\n\nSure, sending it over later today.\nBest, Carol";
        assert_eq!(strip_quoted_history(text), text);
    }

    #[test]
    fn strips_trailing_bare_quote_run_without_attribution() {
        let text = "Quick heads up below.\n\n> old thread line one\n> old thread line two";
        assert_eq!(strip_quoted_history(text), "Quick heads up below.");
    }

    #[test]
    fn whole_message_quote_strips_to_empty() {
        let text = "> only quoted line one\n> only quoted line two";
        assert_eq!(strip_quoted_history(text), "");
    }

    // -- normalization + chunking ---------------------------------------------

    #[test]
    fn normalizes_whitespace_and_packs_paragraphs() {
        let text = "  First   line\twith\ttabs  \r\n\r\n\r\n\r\nSecond paragraph\u{a0}here  \n\n\nThird para.";
        let prepared = prepare_for_distill(text, 6000, 8);
        assert_eq!(
            prepared.chunks,
            vec!["First line with tabs\n\nSecond paragraph here\n\nThird para.".to_string()]
        );
        assert!(!prepared.truncated);

        // Greedy packing: 90+2+90 fits in 200, third paragraph overflows.
        let paragraphs = format!(
            "{}\n\n{}\n\n{}",
            "a".repeat(90),
            "b".repeat(90),
            "c".repeat(90)
        );
        let packed = prepare_for_distill(&paragraphs, 200, 8);
        assert_eq!(packed.chunks.len(), 2);
        assert_eq!(
            packed.chunks[0],
            format!("{}\n\n{}", "a".repeat(90), "b".repeat(90))
        );
        assert_eq!(packed.chunks[1], "c".repeat(90));
        assert!(!packed.truncated);
    }

    #[test]
    fn splits_oversized_paragraph_at_word_boundaries() {
        let paragraph = "word ".repeat(100); // 100 words, ~500 chars, no \n\n
        let prepared = prepare_for_distill(paragraph.trim(), 200, 8);
        assert!(prepared.chunks.len() >= 3);
        for chunk in &prepared.chunks {
            assert!(chunk.chars().count() <= 200);
            // Word-boundary splits: no chunk starts or ends mid-word.
            assert!(chunk.split_whitespace().all(|w| w == "word"));
        }
        let total_words: usize = prepared
            .chunks
            .iter()
            .map(|c| c.split_whitespace().count())
            .sum();
        assert_eq!(total_words, 100);
        assert!(!prepared.truncated);
    }

    #[test]
    fn chunk_cap_marks_truncated_and_floor_clamps_budget() {
        // 1000 chars with a min-clamped budget of 200 → 5 pieces, cap 2.
        let prepared = prepare_for_distill(&"y".repeat(1000), 1, 2);
        assert_eq!(prepared.chunks.len(), 2);
        assert!(prepared.truncated);
        // max_chars=1 clamps to the 200 floor.
        assert_eq!(prepared.chunks[0].chars().count(), 200);
    }

    #[test]
    fn empty_and_blank_input_yield_no_chunks() {
        assert_eq!(prepare_for_distill("", 6000, 8), PreparedText::default());
        assert_eq!(
            prepare_for_distill("  \n\t\n  ", 6000, 8),
            PreparedText::default()
        );
    }

    // -- composed pipeline + header document -----------------------------------

    #[test]
    fn prepare_message_content_runs_full_pipeline() {
        // base64url of a reply whose tail is a Gmail attribution + quote.
        let quoted_reply_b64 = "VGhhbmtzLCB0aGF0IHdvcmtzIGZvciBtZS4KCk9uIFR1ZSwgSnVsIDEsIDIwMjYgYXQgOTowMCBBTSBBbGljZSBFeGFtcGxlIDxhbGljZUBleGFtcGxlLmNvbT4gd3JvdGU6Cj4gRWFybGllciBwcm9wb3NhbCB0ZXh0Lgo-IFNlY29uZCBxdW90ZWQgbGluZS4";
        let full = GmailFullMessage {
            message_id: "m-1".to_string(),
            thread_id: "t-1".to_string(),
            payload: Some(container(
                "multipart/mixed",
                vec![
                    container(
                        "multipart/alternative",
                        vec![leaf("text/plain", quoted_reply_b64)],
                    ),
                    attachment("application/pdf", "agenda.pdf", None),
                ],
            )),
        };
        let content = prepare_message_content(&full, 6000, 8);
        assert_eq!(
            content.chunks,
            vec!["Thanks, that works for me.".to_string()]
        );
        assert!(!content.truncated);
        assert!(!content.had_html);
        assert_eq!(content.attachment_count, 1);

        // Missing payload → empty content, no panic.
        let empty = GmailFullMessage {
            message_id: "m-2".to_string(),
            thread_id: "t-2".to_string(),
            payload: None,
        };
        assert_eq!(
            prepare_message_content(&empty, 6000, 8),
            DistillContent::default()
        );
    }

    #[test]
    fn header_document_carries_metadata_only() {
        let meta = MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "t-1".to_string(),
            message_id: "m-1".to_string(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some("Quarterly planning".to_string()),
            from_name: Some("Alice Example".to_string()),
            from_address: Some("alice@example.com".to_string()),
            to_domains: vec!["example.com".to_string()],
            cc_domains: Vec::new(),
            internal_date: 1_719_990_000_000, // 2024-07-03 UTC
            observed_at: 1,
            direction: Some(MessageDirection::Inbound),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        };
        let doc = build_header_document(&meta, 2);
        let lines: Vec<&str> = doc.lines().collect();
        assert_eq!(lines[0], "Subject: Quarterly planning");
        assert_eq!(lines[1], "From: Alice Example <alice@example.com>");
        assert_eq!(lines[2], "To domains: example.com");
        assert!(lines[3].starts_with("Date: 2024-07-03T"));
        assert_eq!(lines[4], "Direction: inbound");
        assert_eq!(lines[5], "Attachments: 2");
        assert_eq!(lines.len(), 6);

        // Sparse metadata → only the lines that exist; no empty labels.
        let sparse = MailMessageMeta {
            subject: None,
            from_name: None,
            from_address: None,
            to_domains: Vec::new(),
            direction: None,
            ..meta
        };
        let doc = build_header_document(&sparse, 0);
        assert!(!doc.contains("Subject:"));
        assert!(!doc.contains("From:"));
        assert!(!doc.contains("Attachments:"));
        assert!(doc.contains("Date: 2024-07-03T"));
    }
}
