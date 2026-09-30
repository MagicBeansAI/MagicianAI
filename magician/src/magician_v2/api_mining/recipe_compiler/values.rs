//! Collect and normalize values reported by a completed execution.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_VALUES: usize = 200;
const MAX_TAIL_VALUE_WORDS: usize = 6;
const MAX_TAIL_VALUE_BYTES: usize = 80;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;
const MIN_STRING_LEN: usize = 3;
const MAX_STRING_LEN: usize = 200;
const MAX_RAW_VALUE_BYTES: usize = 4 * 1024;
const MAX_TEXT_SCAN_BYTES: usize = 256 * 1024;
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "is", "at", "on", "in",
    "to", "of", "true", "false", "null", "none", "ok", "yes", "no", "n/a", "unknown",
];
type ValueIdentity = (Option<String>, String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportedValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub raw: String,
    pub normalized: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportedValues {
    pub values: Vec<ReportedValue>,
}

impl ReportedValues {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn push(&mut self, seen: &mut HashSet<ValueIdentity>, field: Option<String>, raw: &str) {
        if self.values.len() >= MAX_VALUES || raw.len() > MAX_RAW_VALUE_BYTES {
            return;
        }
        let normalized = normalize_value(raw);
        if !is_useful(&normalized)
            && !(field.is_some()
                && normalized.len() <= MAX_STRING_LEN
                && is_named_scalar(&normalized))
        {
            return;
        }
        // Unlabelled summary echoes add no evidence, but two distinct named
        // fields remain distinct even when their captured values happen to be
        // equal (e.g. buy=42 and sell=42).
        if field.is_none()
            && self
                .values
                .iter()
                .any(|value| value.normalized == normalized)
        {
            return;
        }
        let identity = (
            field.as_deref().map(str::to_ascii_lowercase),
            normalized.clone(),
        );
        if !seen.insert(identity) {
            return;
        }
        if field.is_some() {
            if let Some(existing) = self
                .values
                .iter_mut()
                .find(|value| value.field.is_none() && value.normalized == normalized)
            {
                existing.field = field;
                return;
            }
        }
        self.values.push(ReportedValue {
            field,
            raw: raw.trim().to_owned(),
            normalized,
        });
    }
}

/// A currency code in front of a number is presentation, not the value: the
/// response carries `1284.5` while the summary says `USD 1284.50`. Only a
/// leading code directly followed by a digit is dropped, so a bare `USD` or a
/// sentence starting with a word survives untouched.
fn strip_leading_currency_code(value: &str) -> &str {
    const CODES: &[&str] = &[
        "usd", "eur", "gbp", "inr", "jpy", "cad", "aud", "chf", "cny", "sek", "nzd", "sgd", "aed",
        "rs", "rs.",
    ];
    let trimmed = value.trim();
    let Some((head, rest)) = trimmed.split_once(char::is_whitespace) else {
        return trimmed;
    };
    let rest = rest.trim_start();
    let head_is_code = CODES.contains(&head.to_ascii_lowercase().as_str());
    let rest_starts_numeric = rest
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit());
    if head_is_code && rest_starts_numeric {
        rest
    } else {
        trimmed
    }
}

pub fn normalize_value(raw: &str) -> String {
    let mut value = strip_leading_currency_code(raw)
        .trim()
        .trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”'))
        .trim()
        // Prose punctuation that rides along inside a quotation or at the end
        // of a sentence is not part of the value.
        .trim_end_matches(|character| matches!(character, ',' | ';' | ':' | '.' | '!' | '?'))
        .trim()
        .to_owned();
    let numeric: String = value
        .chars()
        .filter(|character| !matches!(character, '₹' | '$' | '€' | '£' | '¥' | ',' | ' '))
        .collect();
    if !numeric.is_empty()
        && numeric
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
        && numeric.chars().any(|character| character.is_ascii_digit())
    {
        value = canonical_decimal(numeric);
    }
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A decimal reported with trailing zeros (`1284.50`, `40.00`) is the same
/// number a JSON body serializes without them (`1284.5`, `40`). Dotted
/// versions and other multi-dot strings are not decimals and stay as written.
fn canonical_decimal(numeric: String) -> String {
    if numeric.matches('.').count() != 1 || numeric.starts_with('.') {
        return numeric;
    }
    let trimmed = numeric.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The first reported value that no response carries and that still counts
/// as part of the answer. Exempt are values the task text already contains
/// (restated inputs) and values contained in a covered value (a number inside
/// a title the recipe extracts whole). `None` means coverage is complete.
/// `page_text` holds the captured response bodies. An unlabelled value that
/// appears verbatim in one of them but that no extractor could locate is the
/// page's own chrome quoted back ("the orders page states \u{201c}Most recent
/// first\u{201d}") — the agent read it off a response, so it invented nothing, and
/// refusing the recipe over it would throw away a recipe that reproduces the
/// asked value exactly. A value that appears in NO response is a claim
/// nothing supports and still fails closed, which is what keeps a partial
/// answer (a second price only the DOM showed) from compiling.
pub fn first_uncovered(
    reported: &ReportedValues,
    hits: &[super::locate::AnswerHit],
    task_title: &str,
    task_text: &str,
    page_text: &[String],
) -> Option<usize> {
    let task = normalize_value(&format!("{task_title}\n{task_text}"));
    let covered: HashSet<usize> = hits.iter().map(|hit| hit.reported_index).collect();
    let covered_text: Vec<&str> = covered
        .iter()
        .filter_map(|index| reported.values.get(*index))
        .map(|value| value.normalized.as_str())
        .collect();
    reported
        .values
        .iter()
        .enumerate()
        .position(|(index, value)| {
            if covered.contains(&index) || value.normalized.is_empty() {
                return false;
            }
            // A bare boolean is status, not an answer a replay must reproduce.
            if matches!(value.normalized.as_str(), "true" | "false") {
                return false;
            }
            if task.contains(&value.normalized) {
                return false;
            }
            if value.field.is_none()
                && value.normalized.len() >= MIN_STRING_LEN
                && page_text
                    .iter()
                    .any(|body| body.contains(&value.normalized))
            {
                return false;
            }
            // The same value covered under another field is still reproducible:
            // prose labels one number two ways ("points value: 3119" names
            // `value`, the response names `points`), and equal values under
            // distinct fields each demand an exact field match, so the label
            // the prose invented would refuse a recipe that answers correctly.
            // A value NO response carries, under any field, still fails closed.
            !covered_text.iter().any(|text| {
                text == &value.normalized.as_str()
                    || (text.len() > value.normalized.len() && text.contains(&value.normalized))
            })
        })
}

pub(super) fn is_useful(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_STRING_LEN || STOP_WORDS.contains(&value) {
        return false;
    }
    let numeric = value
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.');
    if numeric {
        return value
            .chars()
            .filter(|character| character.is_ascii_digit())
            .count()
            >= 2;
    }
    value.len() >= MIN_STRING_LEN
}

fn is_named_scalar(value: &str) -> bool {
    matches!(value, "true" | "false") || value.parse::<serde_json::Number>().is_ok()
}

pub fn collect_reported_values(
    summary_text: &str,
    artifact_previews: &[serde_json::Value],
    output_previews: &[&str],
) -> ReportedValues {
    collect_reported_values_for_task(summary_text, artifact_previews, output_previews, "")
}

/// `collect_reported_values` plus task-directed extraction: the task names
/// the field it wants (`report its author`), and a summary that mentions that
/// word next to a connector (`author as Ingrid Solvang`, `author is …`,
/// `author: …`) is reporting that field's value, quotes or not.
pub fn collect_reported_values_for_task(
    summary_text: &str,
    artifact_previews: &[serde_json::Value],
    output_previews: &[&str],
    task_text: &str,
) -> ReportedValues {
    let mut output = ReportedValues::default();
    let mut seen = HashSet::new();
    if !task_text.trim().is_empty() {
        for text in std::iter::once(summary_text).chain(output_previews.iter().copied()) {
            collect_task_directed(text, task_text, &mut output, &mut seen);
        }
    }
    for preview in artifact_previews {
        let mut visited = 0;
        walk_json(preview, None, 0, &mut visited, &mut output, &mut seen);
        if output.values.len() >= MAX_VALUES {
            break;
        }
    }
    for text in std::iter::once(summary_text).chain(output_previews.iter().copied()) {
        if output.values.len() >= MAX_VALUES {
            break;
        }
        collect_headline(text, &mut output, &mut seen);
        collect_from_text(text, &mut output, &mut seen);
    }
    output
}

/// Words of the task that can name a field: alphabetic, three letters or
/// more, not a stop word.
/// The field the task is ABOUT: the first field word in document order. The
/// hook passes "<title> <description>", and a title names what is wanted
/// ("Score of board alpha" → `score`), so the head noun is the ask. The
/// sorted set from [`task_field_words`] cannot answer this — it would call
/// that task `alpha`.
pub fn task_head_field_word(task_text: &str) -> Option<String> {
    // A URL is an address, not a field name: scanning it word-by-word offers
    // `http` as the head noun of every task that states where to go.
    let without_urls: String = task_text
        .split_whitespace()
        .filter(|token| !token.contains("://") && !token.starts_with("http"))
        .collect::<Vec<_>>()
        .join(" ");
    without_urls
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| word.len() >= 3 && word.chars().all(|c| c.is_ascii_alphabetic()))
        .find(|word| {
            !STOP_WORDS.contains(&word.as_str()) && !TASK_VERB_WORDS.contains(&word.as_str())
        })
}

pub fn task_field_words(task_text: &str) -> Vec<String> {
    let mut words: Vec<String> = task_text
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| word.len() >= 3 && word.chars().all(|c| c.is_ascii_alphabetic()))
        .filter(|word| {
            !STOP_WORDS.contains(&word.as_str()) && !TASK_VERB_WORDS.contains(&word.as_str())
        })
        .collect();
    words.sort();
    words.dedup();
    words
}

/// Task words that are instructions, never field names.
const TASK_VERB_WORDS: &[&str] = &[
    "open",
    "report",
    "read",
    "use",
    "browser",
    "tool",
    "page",
    "first",
    "result",
    "then",
    "its",
    "not",
    "log",
    "unless",
    "task",
    "says",
    "answer",
    "plain",
    "value",
    "your",
    "final",
    "summary",
    "data",
    "changes",
    "between",
    "runs",
    "now",
    "never",
    "memory",
    "earlier",
    "run",
    "web",
    "search",
    "fetch",
    "evaluation",
    "fixture",
    "mining",
    "api",
    "http",
    "https",
    "www",
    "current",
    "most",
    "recent",
    "confirm",
    "appears",
    "list",
    "add",
    "note",
    "that",
];

/// For each field word the task names, take the short value the text places
/// right after it behind a connector.
/// Where a task-directed value ends. Sentence punctuation closes it, but a
/// `.` or `,` BETWEEN DIGITS is part of the number — `total as USD 1284.50.`
/// reports `USD 1284.50`, not `USD 1284`.
fn value_terminator(text: &str, from: usize) -> usize {
    let mut previous_is_digit = false;
    for (offset, character) in text[from..].char_indices() {
        let index = from + offset;
        let stops = match character {
            '\n' | ';' | '(' | ')' => true,
            '.' | ',' => {
                let next_is_digit = text[index + character.len_utf8()..]
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_ascii_digit());
                !(previous_is_digit && next_is_digit)
            },
            _ => false,
        };
        if stops {
            return index;
        }
        previous_is_digit = character.is_ascii_digit();
    }
    text.len()
}

fn collect_task_directed(
    text: &str,
    task_text: &str,
    output: &mut ReportedValues,
    seen: &mut HashSet<ValueIdentity>,
) {
    const CONNECTORS: &[&str] = &[" as ", " is ", ": ", " was ", " = ", " of ", " — ", " - "];
    let lowered = text.to_ascii_lowercase();
    collect_task_directed_colon_tails(text, &lowered, task_text, output, seen);
    for word in task_field_words(task_text) {
        let mut search_from = 0;
        while let Some(offset) = lowered[search_from..].find(&word) {
            let start = search_from + offset;
            let end = start + word.len();
            search_from = end;
            let word_bounded = (start == 0
                || !lowered.as_bytes()[start - 1].is_ascii_alphanumeric())
                && (end >= lowered.len() || !lowered.as_bytes()[end].is_ascii_alphanumeric());
            if !word_bounded {
                continue;
            }
            let rest = &lowered[end..];
            let Some(connector) = CONNECTORS
                .iter()
                .find(|connector| rest.starts_with(**connector))
            else {
                continue;
            };
            let value_start = end + connector.len();
            let value_end = value_terminator(text, value_start);
            let value = text[value_start..value_end].trim();
            if is_short_value(value)
                && !STOP_WORDS.contains(&value.to_ascii_lowercase().as_str())
                && !looks_like_predicate(value)
            {
                let value = scalar_with_unit(value).unwrap_or(value);
                output.push(seen, Some(word.clone()), value);
            }
        }
    }
}

/// "the catalog was founded in 1987" hands back a predicate, not a value: the
/// answer is the year, and the clause around it is prose. A value does not
/// open with a participle or a preposition, so such a capture is dropped and
/// the plain scalar extractors report the number on their own.
/// A prose tail like a count followed by its unit reports the number; the
/// trailing word is the unit the sentence supplies, and no response carries
/// the two glued together. Only a bare scalar followed by exactly one
/// alphabetic word qualifies, so a two-word name is left whole.
fn scalar_with_unit(value: &str) -> Option<&str> {
    let mut tokens = value.split_whitespace();
    let (scalar, unit) = (tokens.next()?, tokens.next()?);
    if tokens.next().is_some() || !unit.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let numeric = scalar.chars().any(|c| c.is_ascii_digit())
        && scalar
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+' | '%'));
    numeric.then_some(scalar)
}

fn looks_like_predicate(value: &str) -> bool {
    const LEADING_PREPOSITIONS: &[&str] = &[
        "in", "on", "at", "by", "from", "to", "with", "for", "into", "under", "over", "after",
        "before", "about", "that", "which",
    ];
    let Some(first) = value.split_whitespace().next() else {
        return false;
    };
    let first = first
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_ascii_lowercase();
    if LEADING_PREPOSITIONS.contains(&first.as_str()) {
        return true;
    }
    // Long enough that a real name ("Ed", "Ming") is not mistaken for one.
    first.len() >= 4 && (first.ends_with("ed") || first.ends_with("ing"))
}

/// A clause that names the asked field and then delivers a short value after a
/// colon reports THAT field, wherever the field word sits in the clause. The
/// generic tail rule in `collect_from_text` takes the last word before the
/// colon, which reads "the author displayed on the opened page: Ingrid
/// Solvang" as a value for `page` and hides the answer the task asked for.
fn collect_task_directed_colon_tails(
    text: &str,
    lowered: &str,
    task_text: &str,
    output: &mut ReportedValues,
    seen: &mut HashSet<ValueIdentity>,
) {
    // Only the head noun takes the value. Labelling every task word in the
    // clause ("Opened the Alpha board and read its displayed score: 7331")
    // mints a second entry with the same value under a field no response
    // carries, and equal values under distinct fields each demand an exact
    // field match — so the spurious one refuses the recipe.
    let Some(word) = task_head_field_word(task_text) else {
        return;
    };
    for (line, lowered_line) in text.lines().zip(lowered.lines()) {
        let Some(colon) = line.rfind(':') else {
            continue;
        };
        let value = line[colon + 1..].trim().trim_end_matches('.').trim();
        if !is_short_value(value)
            || STOP_WORDS.contains(&value.to_ascii_lowercase().as_str())
            || looks_like_predicate(value)
        {
            continue;
        }
        let clause = &lowered_line[..colon.min(lowered_line.len())];
        let names_field = clause.match_indices(word.as_str()).any(|(start, _)| {
            let end = start + word.len();
            (start == 0 || !clause.as_bytes()[start - 1].is_ascii_alphanumeric())
                && (end >= clause.len() || !clause.as_bytes()[end].is_ascii_alphanumeric())
        });
        if names_field {
            let value = scalar_with_unit(value).unwrap_or(value);
            output.push(seen, Some(word.clone()), value);
        }
    }
}

/// A deliverable that opens with a short bare line — the yield text itself,
/// `Ingrid Solvang` — is reporting that line as its answer. Headings,
/// bullets and prose lines are not headlines.
fn collect_headline(text: &str, output: &mut ReportedValues, seen: &mut HashSet<ValueIdentity>) {
    let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return;
    };
    if line.starts_with(['#', '-', '*', '>', '|', '`']) || line.contains(':') {
        return;
    }
    // A sentence ends in a period; a value does not. A line of nothing but
    // numbers and booleans is not an answer either.
    let bare_scalars = line.split_whitespace().all(|token| {
        token.chars().all(|c| c.is_ascii_digit() || c == '.') || matches!(token, "true" | "false")
    });
    if line.ends_with('.') || bare_scalars {
        return;
    }
    if is_short_value(line) {
        output.push(seen, None, line);
    }
}

/// Short enough to be a value rather than a sentence, and free of the
/// brackets and separators a torn sentence fragment carries.
fn is_short_value(value: &str) -> bool {
    !value.is_empty()
        && value.split_whitespace().count() <= MAX_TAIL_VALUE_WORDS
        && value.len() <= MAX_TAIL_VALUE_BYTES
        && value.chars().any(char::is_alphanumeric)
        && !value.contains("://")
        && !value.contains(['(', ')', '[', ']', ';', '|', '{', '}'])
}

fn walk_json(
    value: &serde_json::Value,
    field: Option<&str>,
    depth: usize,
    visited: &mut usize,
    output: &mut ReportedValues,
    seen: &mut HashSet<ValueIdentity>,
) {
    if depth > MAX_JSON_DEPTH || *visited >= MAX_JSON_NODES || output.values.len() >= MAX_VALUES {
        return;
    }
    *visited += 1;
    match value {
        serde_json::Value::String(value) => output.push(seen, field.map(str::to_owned), value),
        serde_json::Value::Number(value) => {
            output.push(seen, field.map(str::to_owned), &value.to_string())
        },
        serde_json::Value::Bool(value) => output.push(
            seen,
            field.map(str::to_owned),
            if *value { "true" } else { "false" },
        ),
        serde_json::Value::Array(items) => {
            for item in items {
                walk_json(item, field, depth + 1, visited, output, seen);
                if *visited >= MAX_JSON_NODES || output.values.len() >= MAX_VALUES {
                    break;
                }
            }
        },
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                walk_json(value, Some(key), depth + 1, visited, output, seen);
                if *visited >= MAX_JSON_NODES || output.values.len() >= MAX_VALUES {
                    break;
                }
            }
        },
        _ => {},
    }
}

fn collect_from_text(text: &str, output: &mut ReportedValues, seen: &mut HashSet<ValueIdentity>) {
    // Models echo JSON-escaped prose (`shows \"buy milk\".`). The backslash
    // belongs to the quoting, so scanning the raw text would report
    // `buy milk\` — a value no response can carry.
    let text = unescape_quotes(utf8_prefix(text, MAX_TEXT_SCAN_BYTES));
    for line in text.lines() {
        if output.values.len() >= MAX_VALUES {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            // A scheme's colon is not a field separator. Prose that opens with
            // where the agent went splits into a key of the trailing verb and
            // scheme, and a value that is the rest of the sentence — a pair no
            // response can ever carry, which then refuses the whole recipe.
            let scheme_separator = value.starts_with("//");
            // Every sibling collector requires the tail to look like a value.
            // This one accepted any non-empty tail, so a sentence that merely
            // contained a colon was reported as an answer.
            if !scheme_separator
                && key.split_whitespace().count() <= 3
                && is_short_value(value.trim())
            {
                let value = value.trim();
                let value = scalar_with_unit(value).unwrap_or(value);
                output.push(seen, Some(key.trim().to_lowercase()), value);
            }
        }
        // A sentence that ends in `…its byline: Ingrid Solvang.` reports one
        // short value after its final colon; the key is the sentence's tail.
        if let Some((key, value)) = line.rsplit_once(':') {
            let value = value.trim().trim_end_matches('.');
            let key_words: Vec<&str> = key.split_whitespace().collect();
            if key_words.len() > 3 && is_short_value(value) {
                let field = key_words[key_words.len() - 1]
                    .trim_matches(|character: char| !character.is_alphanumeric())
                    .to_lowercase();
                let value = scalar_with_unit(value).unwrap_or(value);
                output.push(seen, (!field.is_empty()).then_some(field), value);
            }
        }

        // Models quote with typographic marks as often as straight ones.
        for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}')] {
            let mut rest = line;
            while let Some(start) = rest.find(open) {
                let after = &rest[start + open.len_utf8()..];
                let Some(end) = after.find(close) else {
                    break;
                };
                // A span holding another quote delimiter is narration quoting a
                // value (`"Added "buy milk"."`), not a value itself; the inner
                // span is picked up on its own.
                let candidate = &after[..end];
                if !candidate.contains(['"', '\u{201c}', '\u{201d}']) {
                    output.push(seen, None, candidate);
                }
                rest = &after[end + close.len_utf8()..];
            }
        }

        // Hyphenated identifiers and dates (`ORD-8841`, `2026-09-10`) are one
        // value each; a response carries them whole, never as bare digit runs.
        collect_hyphenated_tokens(line, output, seen);

        let mut token = String::new();
        for character in line.chars().chain(std::iter::once(' ')) {
            if character.is_ascii_digit()
                || ((character == '.' || character == ',') && !token.is_empty())
            {
                token.push(character);
            } else {
                if token
                    .chars()
                    .filter(|character| character.is_ascii_digit())
                    .count()
                    >= 2
                {
                    output.push(seen, None, &token);
                }
                token.clear();
            }
        }
    }
}

fn collect_hyphenated_tokens(
    line: &str,
    output: &mut ReportedValues,
    seen: &mut HashSet<ValueIdentity>,
) {
    for token in
        line.split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
    {
        let token = token.trim_matches('-');
        let digits = token
            .chars()
            .filter(|character| character.is_ascii_digit())
            .count();
        let joins_two_parts = token.split('-').filter(|part| !part.is_empty()).count() >= 2;
        if digits >= 2 && joins_two_parts {
            output.push(seen, None, token);
        }
    }
}

/// Undo JSON-style escaping a model applied to prose it quotes back.
fn unescape_quotes(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_owned();
    }
    text.replace("\\\"", "\"").replace("\\\\", "\\")
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn a_short_value_after_a_sentence_final_colon_is_reported() {
        let reported = collect_reported_values(
            "Opened the current first Rust search result and verified its byline: Ingrid Solvang.",
            &[],
            &[],
        );
        let byline = reported
            .values
            .iter()
            .find(|value| value.normalized == "ingrid solvang")
            .expect("the byline is the reported value");
        assert_eq!(byline.field.as_deref(), Some("byline"));
        // A long tail is prose, not a value.
        let prose = collect_reported_values(
            "Summary of what happened: the agent opened the page, waited for the list to render, and then read the first entry carefully.",
            &[],
            &[],
        );
        assert!(prose
            .values
            .iter()
            .all(|value| value.normalized.len() <= MAX_TAIL_VALUE_BYTES));
        assert!(!prose
            .values
            .iter()
            .any(|value| value.normalized.starts_with("the agent opened")));
    }

    #[test]
    fn a_partial_success_report_yields_only_its_answer_values() {
        let summary = "Goal partially achieved: API-mining fixture evaluation. Open http://127.0.0.1:62784/?q=rust, open the first result, and report its author.\n\n## PARTIAL SUCCESS — completed work and remaining gaps\n\nIngrid Solvang\n\n### COMPLETED\n- Opened the current Rust search page, followed its first result (Rust 2031 roadmap), and verified the rendered author as Ingrid Solvang.\n\n### BLOCKED / REMAINING\n- Unverified against opened-page evidence: The source does not identify the author of \u{201c}Rust 2031 roadmap\u{201d} as Ingrid Solvang. (1 retrieval(s) produced content the verifier could not see; coverage: browser__read: 1 admissible page(s) in the source; browser__click: 2 failed; browser: 3 failed; browser__eval: 1 retrieval(s) with no evidence envelope (content NOT in the source))";
        let outputs = ["Ingrid Solvang\n\nTop result: \u{201c}Rust 2031 roadmap.\u{201d} The author was rendered as Ingrid Solvang, but the author detail lacked verifier-visible opened-page evidence."];
        let reported = collect_reported_values(summary, &[], &outputs);
        let collected: Vec<(Option<&str>, &str)> = reported
            .values
            .iter()
            .map(|value| (value.field.as_deref(), value.normalized.as_str()))
            .collect();
        assert!(
            !collected
                .iter()
                .any(|(_, value)| *value == "true" || *value == "false"),
            "{collected:?}"
        );
    }

    #[test]
    fn the_field_the_task_asks_for_is_read_off_the_summary() {
        let task =
            "Open http://127.0.0.1:49244/?q=rust, open the first result, and report its author.";
        let summary = "Opened the current Rust search results, selected the first result (\u{201c}Rust 2031 roadmap\u{201d}), and read its author as Ingrid Solvang.";
        let reported = collect_reported_values_for_task(summary, &[], &[], task);
        let author = reported
            .values
            .iter()
            .find(|value| value.field.as_deref() == Some("author"))
            .expect("author is the reported field");
        assert_eq!(author.normalized, "ingrid solvang");
        // Other phrasings the same rule reads.
        for text in [
            "The author is Ingrid Solvang.",
            "Author: Ingrid Solvang",
            "author — Ingrid Solvang",
        ] {
            let reported = collect_reported_values_for_task(text, &[], &[], task);
            assert!(
                reported
                    .values
                    .iter()
                    .any(|value| value.normalized == "ingrid solvang"
                        && value.field.as_deref() == Some("author")),
                "{text}: {:?}",
                reported.values
            );
        }
        // A task word without a connector reports nothing.
        let reported = collect_reported_values_for_task(
            "The author field was blank on the page.",
            &[],
            &[],
            task,
        );
        assert!(!reported
            .values
            .iter()
            .any(|value| value.field.as_deref() == Some("author")));
        assert_eq!(
            task_field_words("report the points of the first result"),
            vec!["points"]
        );
    }

    #[test]
    fn a_deliverable_headline_is_the_reported_answer() {
        let reported = collect_reported_values(
            "Goal partially achieved: API-mining fixture evaluation. Open the first result and report its author.\n\n## PARTIAL SUCCESS\n\nIngrid Solvang\n\n### BLOCKED / REMAINING\n- Unverified against opened-page evidence: coverage: browser__click: 1 failed; browser: 1 failed)",
            &[],
            &["Ingrid Solvang"],
        );
        let normalized: Vec<&str> = reported
            .values
            .iter()
            .map(|value| value.normalized.as_str())
            .collect();
        assert!(normalized.contains(&"ingrid solvang"), "{normalized:?}");
        assert!(
            !normalized.iter().any(|value| value.contains("failed")),
            "{normalized:?}"
        );
        // A heading, a bullet, a sentence, or bare scalars are not headline values.
        for text in [
            "## Result",
            "- Ingrid Solvang",
            "Ingrid Solvang wrote it.",
            "0 true false 1",
        ] {
            let reported = collect_reported_values("", &[], &[text]);
            assert!(
                !reported
                    .values
                    .iter()
                    .any(|value| value.normalized == "ingrid solvang"),
                "{text}"
            );
        }
    }

    #[test]
    fn curly_quoted_strings_are_reported_values() {
        let reported = collect_reported_values(
            "verified that the first result, \u{201c}Rust 2031 roadmap,\u{201d} has 3119 points.",
            &[],
            &[],
        );
        let normalized: Vec<&str> = reported
            .values
            .iter()
            .map(|value| value.normalized.as_str())
            .collect();
        assert!(normalized.contains(&"rust 2031 roadmap"), "{normalized:?}");
        assert!(normalized.contains(&"3119"), "{normalized:?}");
    }

    #[test]
    fn coverage_exempts_task_echoes_and_contained_values() {
        use super::super::locate::AnswerHit;
        use crate::magician_v2::api_mining::recipe::Extractor;
        let reported = collect_reported_values(
            "Goal: Open http://127.0.0.1:51430/?q=rust and report the points.\n3119 points for \"Rust 2031 roadmap\"",
            &[],
            &[],
        );
        let index_of = |needle: &str| {
            reported
                .values
                .iter()
                .position(|value| value.normalized == needle)
                .unwrap_or_else(|| panic!("{needle} not collected: {:?}", reported.values))
        };
        let hit = |index: usize| AnswerHit {
            reported_index: index,
            trace_index: 0,
            field: None,
            value: reported.values[index].normalized.clone(),
            extractor: Extractor::JsonPath { path: "$.x".into() },
        };
        let title = "Points of the top result for rust";
        let text = "Open http://127.0.0.1:51430/?q=rust and report the points.";
        // Only the answer and the title are response-backed: the URL, the
        // port and the goal sentence are echoes; 2031 sits inside the title.
        let hits = vec![hit(index_of("3119")), hit(index_of("rust 2031 roadmap"))];
        assert_eq!(first_uncovered(&reported, &hits, title, text, &[]), None);
        // A boolean from a structured payload is status, not answer.
        let flagged = collect_reported_values(
            "",
            &[serde_json::json!({"points": 3119, "verified": true})],
            &[],
        );
        let hits = vec![AnswerHit {
            reported_index: flagged
                .values
                .iter()
                .position(|v| v.normalized == "3119")
                .unwrap(),
            trace_index: 0,
            field: Some("points".into()),
            value: "3119".into(),
            extractor: Extractor::JsonPath {
                path: "$.points".into(),
            },
        }];
        assert_eq!(first_uncovered(&flagged, &hits, title, text, &[]), None);
        // A genuinely new value still fails coverage.
        let extra = collect_reported_values("3119 and 4242 comments.", &[], &[]);
        let hits = vec![AnswerHit {
            reported_index: extra
                .values
                .iter()
                .position(|v| v.normalized == "3119")
                .unwrap(),
            trace_index: 0,
            field: None,
            value: "3119".into(),
            extractor: Extractor::JsonPath { path: "$.x".into() },
        }];
        let uncovered =
            first_uncovered(&extra, &hits, title, text, &[]).expect("4242 is uncovered");
        // Page chrome the agent read off a response is narration, not a claim.
        let chrome = vec!["the page says 4242 comments".to_owned()];
        assert_eq!(first_uncovered(&extra, &hits, title, text, &chrome), None);
        assert_eq!(extra.values[uncovered].normalized, "4242");
    }

    #[test]
    fn normalizes_currency_thousands_and_case() {
        assert_eq!(normalize_value(" ₹1,582 "), "1582");
        assert_eq!(normalize_value("$40.00"), "40");
        assert_eq!(normalize_value("USD 1284.50"), "1284.5");
        assert_eq!(normalize_value("usd 1284"), "1284");
        assert_eq!(normalize_value("USD"), "usd");
        assert_eq!(normalize_value("Rust 2031 roadmap"), "rust 2031 roadmap");
        assert_eq!(normalize_value("1284.50"), "1284.5");
        assert_eq!(normalize_value("1284.5"), "1284.5");
        assert_eq!(normalize_value("312.0"), "312");
        assert_eq!(normalize_value("0.7.20"), "0.7.20");
        assert_eq!(normalize_value("0.50"), "0.5");
        assert_eq!(normalize_value("100"), "100");
        assert_eq!(
            normalize_value("\u{201c}Rust 2031 roadmap,\u{201d}"),
            "rust 2031 roadmap"
        );
        assert_eq!(normalize_value("3119."), "3119");
        assert_eq!(
            normalize_value("Why Discord is switching from Go to Rust"),
            "why discord is switching from go to rust"
        );
        assert_eq!(normalize_value("\"quoted\""), "quoted");
    }

    #[test]
    fn collects_from_json_previews_with_field_names() {
        let preview = serde_json::json!({
            "title": "Why Discord is switching from Go to Rust",
            "points": 1582,
            "nested": {"objectID": "23143689"}
        });
        let values = collect_reported_values("", &[preview], &[]);
        let by_field = |field: &str| {
            values
                .values
                .iter()
                .find(|value| value.field.as_deref() == Some(field))
                .unwrap()
        };
        assert_eq!(by_field("points").normalized, "1582");
        assert_eq!(by_field("objectID").normalized, "23143689");
        assert_eq!(
            by_field("title").raw,
            "Why Discord is switching from Go to Rust"
        );
    }

    #[test]
    fn collects_from_text_and_dedupes() {
        let values = collect_reported_values(
            "Coke Zero 300ml is available.\nPrice: ₹40\nPoints: 1582\nThe price is ₹40 today.",
            &[],
            &[],
        );
        let normalized: Vec<&str> = values
            .values
            .iter()
            .map(|value| value.normalized.as_str())
            .collect();
        assert!(normalized.contains(&"40"));
        assert!(normalized.contains(&"1582"));
        assert_eq!(normalized.iter().filter(|value| **value == "40").count(), 1);
        assert!(!normalized.contains(&"is"));
    }

    #[test]
    fn escaped_quotes_in_narration_report_only_the_inner_value() {
        let values = collect_reported_values(
            "Logged in as eval and reached the notes page. Added the note \"buy milk\". \
             Confirmed the page shows \u{201c}Added \\\"buy milk\\\".\u{201d} and lists \
             \u{201c}buy milk\u{201d}.",
            &[],
            &[],
        );
        let normalized: Vec<&str> = values
            .values
            .iter()
            .map(|value| value.normalized.as_str())
            .collect();
        assert!(normalized.contains(&"buy milk"), "{normalized:?}");
        // The escape artifact and the quoting narration are not answers.
        assert!(
            !normalized.iter().any(|value| value.contains('\\')),
            "{normalized:?}"
        );
        assert!(
            !normalized.iter().any(|value| value.starts_with("added ")),
            "{normalized:?}"
        );
    }

    #[test]
    fn a_predicate_clause_is_not_reported_as_the_fields_value() {
        // "<field> was founded in 1987" is prose; the answer is the year, and
        // the page carries it with markup between the words.
        let values = collect_reported_values_for_task(
            "Opened the current About page and verified that the catalog was founded in 1987.",
            &[],
            &[],
            "Open http://127.0.0.1:5000/about and report the founding year of the catalog.",
        );
        assert!(
            !values
                .values
                .iter()
                .any(|value| value.normalized.starts_with("founded")),
            "{:?}",
            values.values
        );
        assert!(
            values.values.iter().any(|value| value.normalized == "1987"),
            "the year is still reported: {:?}",
            values.values
        );
        // A real name that merely ends in those letters is still a value.
        assert!(!looks_like_predicate("Ed Smith"));
        assert!(looks_like_predicate("displayed on the opened page"));
        assert!(looks_like_predicate("in 1987"));
    }

    #[test]
    fn a_second_label_for_a_covered_value_does_not_gate_coverage() {
        // "the first Rust search result's points value: 3119" — the tail rule
        // names `value`, the response names `points`. One number, two labels.
        let reported = ReportedValues {
            values: vec![
                ReportedValue {
                    field: Some("points".into()),
                    raw: "3119".into(),
                    normalized: "3119".into(),
                },
                ReportedValue {
                    field: Some("value".into()),
                    raw: "3119".into(),
                    normalized: "3119".into(),
                },
            ],
        };
        let hits = vec![super::super::locate::AnswerHit {
            reported_index: 0,
            trace_index: 0,
            field: Some("points".into()),
            value: "3119".into(),
            extractor: crate::magician_v2::api_mining::recipe::Extractor::JsonPath {
                path: "$.hits[0].points".into(),
            },
        }];
        assert_eq!(first_uncovered(&reported, &hits, "", "", &[]), None);
        // A value no response carries at all still fails closed.
        let mut invented = reported.clone();
        invented.values.push(ReportedValue {
            field: Some("comments".into()),
            raw: "4242".into(),
            normalized: "4242".into(),
        });
        assert_eq!(first_uncovered(&invented, &hits, "", "", &[]), Some(2));
    }

    /// Both of these summaries were produced by the live agent on 2026-09-14
    /// against real public sites, and each refused an otherwise compilable
    /// recipe: the extractor reported a value no response carried, and
    /// coverage fails closed on exactly that.
    #[test]
    fn a_url_scheme_colon_is_not_a_field_separator() {
        let task = "Points of the top Hacker News story about rust Open \
                    https://hn.algolia.com/?q=rust and report the points of the first story \
                    result.";
        let values = collect_reported_values_for_task(
            "Opened https://hn.algolia.com/?q=rust with the browser tool and read the first \
             story result; it shows 1582 points.",
            &[],
            &[],
            task,
        );
        // The scheme's colon used to split the sentence into a field of the
        // trailing verb and a value that was the rest of the line.
        for value in &values.values {
            assert!(
                !value.normalized.starts_with("//"),
                "a URL tail was reported as a value: {:?}",
                value
            );
            assert_ne!(value.field.as_deref(), Some("opened https"), "{value:?}");
        }
    }

    #[test]
    fn a_count_reports_its_number_not_the_unit_word_glued_to_it() {
        let task = "Points of the top Hacker News story about wasm Open \
                    https://hn.algolia.com/?q=wasm and report the points of the first story \
                    result.";
        let values = collect_reported_values_for_task(
            "Opened the live Hacker News Algolia wasm search page with the browser and read \
             the first story result's score: 1086 points.",
            &[],
            &[],
            task,
        );
        // The response carries the number; nothing carries number-plus-unit.
        assert!(
            values.values.iter().any(|value| value.normalized == "1086"),
            "{:?}",
            values.values
        );
        assert!(
            !values
                .values
                .iter()
                .any(|value| value.normalized.contains("1086 points")),
            "{:?}",
            values.values
        );
    }

    #[test]
    fn a_two_word_name_is_not_mistaken_for_a_count_and_its_unit() {
        assert_eq!(scalar_with_unit("1086 points"), Some("1086"));
        assert_eq!(scalar_with_unit("38.0 m"), Some("38.0"));
        assert_eq!(scalar_with_unit("Ingrid Solvang"), None);
        assert_eq!(scalar_with_unit("3119 and 42"), None);
        assert_eq!(scalar_with_unit("1284.50"), None);
    }

    #[test]
    fn only_the_tasks_head_noun_claims_the_colon_tail() {
        // "Score of board alpha" asks for `score`; `alpha` is the subject.
        // Labelling both mints two entries with one value under two fields,
        // and equal values under distinct fields each demand an exact field
        // match, so the spurious one would refuse the recipe.
        let task = "Score of board alpha Open http://127.0.0.1:5000/board/alpha and report the \
                    board's score.";
        assert_eq!(task_head_field_word(task).as_deref(), Some("score"));
        let values = collect_reported_values_for_task(
            "Opened the Alpha board and read its displayed score: 7331.",
            &[],
            &[],
            task,
        );
        let labelled: Vec<_> = values
            .values
            .iter()
            .filter(|value| value.normalized == "7331")
            .filter_map(|value| value.field.clone())
            .collect();
        assert_eq!(labelled, vec!["score".to_owned()], "{:?}", values.values);
    }

    #[test]
    fn a_clause_naming_the_asked_field_labels_its_colon_tail() {
        // The generic tail rule would call this a value for `page`.
        let values = collect_reported_values_for_task(
            "Opened the first result, \u{201c}Rust 2031 roadmap.\u{201d} Verified the author \
             displayed on the opened page: Ingrid Solvang.",
            &[],
            &[],
            "Author of the top result for rust Open http://127.0.0.1:5000/?q=rust, open the \
             first result, and report its author.",
        );
        assert!(
            values
                .values
                .iter()
                .any(|value| value.field.as_deref() == Some("author")
                    && value.normalized == "ingrid solvang"),
            "{:?}",
            values.values
        );
    }

    #[test]
    fn a_task_directed_decimal_keeps_its_fraction_and_drops_the_currency() {
        let task = "Open http://127.0.0.1:51586/login, log in with username eval and password \
                    eval-pass, then open the orders page and report the total of the most recent \
                    order.";
        let values = collect_reported_values_for_task(
            "Logged in and opened the orders page. Read the latest order (2026-09-10) total as \
             USD 1284.50.",
            &[],
            &[],
            task,
        );
        let total = values
            .values
            .iter()
            .find(|value| value.field.as_deref() == Some("total"))
            .expect("the task's field is reported");
        // `1284`, or `usd 1284.50`, never matches the JSON number 1284.5.
        assert_eq!(total.normalized, "1284.5");
    }

    #[test]
    fn a_sentence_final_period_still_closes_a_task_directed_value() {
        let values = collect_reported_values_for_task(
            "Confirmed the author as Ingrid Solvang. Nothing else changed.",
            &[],
            &[],
            "Open the first result and report its author.",
        );
        let author = values
            .values
            .iter()
            .find(|value| value.field.as_deref() == Some("author"))
            .expect("author is reported");
        assert_eq!(author.normalized, "ingrid solvang");
    }

    #[test]
    fn hyphenated_ids_and_dates_are_whole_values() {
        let values = collect_reported_values(
            "Verified the most recent order, ORD-8841 (2026-09-10), has total USD 1284.50.",
            &[],
            &[],
        );
        let normalized: Vec<&str> = values
            .values
            .iter()
            .map(|value| value.normalized.as_str())
            .collect();
        assert!(normalized.contains(&"ord-8841"), "{normalized:?}");
        assert!(normalized.contains(&"2026-09-10"), "{normalized:?}");
        assert!(normalized.contains(&"1284.5"), "{normalized:?}");
        assert!(!normalized.contains(&"eval-pass"));
        assert!(!normalized.contains(&"-8841"));
    }

    #[test]
    fn reported_value_collection_bounds_raw_and_utf8_text() {
        let mut output = ReportedValues::default();
        let mut seen = HashSet::new();
        output.push(
            &mut seen,
            Some("oversized".into()),
            &"x".repeat(MAX_RAW_VALUE_BYTES + 1),
        );
        assert!(output.values.is_empty());

        let text = format!("{}\nPrice: 42", "é".repeat(MAX_TEXT_SCAN_BYTES));
        collect_from_text(&text, &mut output, &mut seen);
        assert!(output.values.is_empty());
        assert!(utf8_prefix(&text, MAX_TEXT_SCAN_BYTES)
            .is_char_boundary(utf8_prefix(&text, MAX_TEXT_SCAN_BYTES).len()));
    }

    #[test]
    fn named_equal_values_and_zero_boolean_answers_are_not_discarded() {
        let values = collect_reported_values(
            "Count: 0\nEnabled: false\n42",
            &[serde_json::json!({
                "buy": 42, "sell": 42, "count": 0, "enabled": false
            })],
            &[],
        );
        assert_eq!(values.values.len(), 4);
        for field in ["buy", "sell", "count", "enabled"] {
            assert!(values
                .values
                .iter()
                .any(|value| value.field.as_deref() == Some(field)));
        }
        assert!(collect_reported_values("0 true false 1", &[], &[]).is_empty());
    }
}
