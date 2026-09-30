//! Resolve request tokens backwards to earlier responses, task inputs, or auth.

use crate::magician_v2::api_mining::recipe::{
    extract_unique_regex, request_body_format, Extractor, NowUnit, RequestBodyFormat,
    TaskInputSchema,
};
use crate::magician_v2::api_mining::trace_storage::is_sensitive_header_name;
use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use std::collections::{HashMap, HashSet, VecDeque};

const MIN_TOKEN_LEN: usize = 4;
const MAX_DEPTH: usize = 8;
const MAX_REQUEST_TOKENS: usize = 128;
const MAX_SOURCE_BODY: usize = 1024 * 1024;
const MAX_SOURCE_JSON_DEPTH: usize = 64;
const MAX_SOURCE_JSON_NODES: usize = 65_536;
const STANDARD_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "cache-control",
    "connection",
    "content-length",
    "content-type",
    "host",
    "origin",
    "pragma",
    "referer",
    "user-agent",
    "upgrade-insecure-requests",
    "priority",
    "te",
    "dnt",
    "if-none-match",
    "if-modified-since",
    "x-requested-with",
];
const SIGNATURE_QUERY_KEYS: &[&str] = &["sig", "signature", "hmac", "digest"];
const SENSITIVE_BODY_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passcode",
    "secret",
    "client_secret",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenLocation {
    PathSegment(usize),
    Query { key: String, occurrence: usize },
    JsonBodyPath { pointer: String, key: String },
    BodyKey { key: String, occurrence: usize },
    OpaqueBody,
    Header(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub location: TokenLocation,
    pub name: String,
    pub value: String,
    pub value_type: TaskInputSchema,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenResolution {
    FromResponse {
        source_trace: usize,
        extractor: Extractor,
    },
    FromTypedInput {
        name: String,
    },
    FromTaskText {
        name: String,
    },
    FromSession {
        scheme: String,
    },
    Now {
        unit: NowUnit,
    },
    Literal {
        volatile: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ResolvedRequest {
    pub trace_index: usize,
    pub url_template: String,
    pub body_template: Option<String>,
    pub headers_template: HashMap<String, String>,
    pub params: Vec<(Token, TokenResolution)>,
}

pub struct ResolveContext<'a> {
    pub traces: &'a [NetworkTraceEvent],
    pub typed_inputs: &'a [String],
    pub task_text: &'a str,
}

pub fn tokenize_request(trace: &NetworkTraceEvent) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut token_names = HashSet::new();
    let Ok(url) = url::Url::parse(&trace.url) else {
        return tokens;
    };
    for (index, segment) in url.path_segments().into_iter().flatten().enumerate() {
        if looks_dynamic_segment(segment) {
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return unreplayable_request_tokens();
            }
            tokens.push(Token {
                location: TokenLocation::PathSegment(index),
                name: unique_token_name(format!("p{index}"), &mut token_names),
                value: urlencoding::decode(segment)
                    .map(|value| value.into_owned())
                    .unwrap_or_else(|_| segment.to_owned()),
                value_type: TaskInputSchema::String,
            });
        }
    }
    let mut query_occurrences = HashMap::<String, usize>::new();
    for (key, value) in url.query_pairs() {
        if tokens.len() >= MAX_REQUEST_TOKENS {
            return unreplayable_request_tokens();
        }
        let key = key.to_string();
        let occurrence = query_occurrences.entry(key.clone()).or_default();
        tokens.push(Token {
            location: TokenLocation::Query {
                key: key.clone(),
                occurrence: *occurrence,
            },
            name: unique_token_name(sanitize_name(&key), &mut token_names),
            value: value.to_string(),
            value_type: TaskInputSchema::String,
        });
        *occurrence += 1;
    }
    if let Some(body) = trace.request_body.as_deref() {
        let format = effective_body_format(trace);
        const MAX_TEMPLATE_BODY_BYTES: usize = 256 * 1024;
        if body.len() > MAX_TEMPLATE_BODY_BYTES {
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return unreplayable_request_tokens();
            }
            push_opaque_body_token(&mut tokens, &mut token_names);
        } else if let Some(json) =
            matches!(format, RequestBodyFormat::Json | RequestBodyFormat::Infer)
                .then(|| serde_json::from_str::<serde_json::Value>(body).ok())
                .flatten()
                .filter(|json| json.is_object() || json.is_array())
        {
            let mut visited = 0_usize;
            let token_start = tokens.len();
            if !tokenize_json_body(
                &json,
                "",
                None,
                0,
                &mut visited,
                &mut tokens,
                &mut token_names,
            ) {
                if token_start >= MAX_REQUEST_TOKENS {
                    return unreplayable_request_tokens();
                }
                tokens.truncate(token_start);
                push_opaque_body_token(&mut tokens, &mut token_names);
            }
        } else if format == RequestBodyFormat::Form
            || (format == RequestBodyFormat::Infer && body.contains('='))
        {
            let mut body_occurrences = HashMap::<String, usize>::new();
            for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
                if tokens.len() >= MAX_REQUEST_TOKENS {
                    return unreplayable_request_tokens();
                }
                let key = key.into_owned();
                let occurrence = body_occurrences.entry(key.clone()).or_default();
                tokens.push(Token {
                    location: TokenLocation::BodyKey {
                        key: key.clone(),
                        occurrence: *occurrence,
                    },
                    name: unique_token_name(sanitize_name(&key), &mut token_names),
                    value: value.into_owned(),
                    value_type: TaskInputSchema::String,
                });
                *occurrence += 1;
            }
        } else if !body.is_empty() {
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return unreplayable_request_tokens();
            }
            push_opaque_body_token(&mut tokens, &mut token_names);
        }
    }
    for (name, value) in &trace.request_headers {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("sec-")
            || STANDARD_REQUEST_HEADERS.contains(&lower.as_str())
            || lower == "cookie"
        {
            continue;
        }
        if tokens.len() >= MAX_REQUEST_TOKENS {
            return unreplayable_request_tokens();
        }
        tokens.push(Token {
            location: TokenLocation::Header(lower.clone()),
            name: unique_token_name(sanitize_name(&lower), &mut token_names),
            value: value.clone(),
            value_type: TaskInputSchema::String,
        });
    }
    tokens
}

fn unreplayable_request_tokens() -> Vec<Token> {
    vec![Token {
        location: TokenLocation::OpaqueBody,
        name: "opaque_body".into(),
        value: String::new(),
        value_type: TaskInputSchema::String,
    }]
}

fn push_opaque_body_token(tokens: &mut Vec<Token>, token_names: &mut HashSet<String>) {
    tokens.push(Token {
        location: TokenLocation::OpaqueBody,
        name: unique_token_name("opaque_body".into(), token_names),
        value: String::new(),
        value_type: TaskInputSchema::String,
    });
}

fn tokenize_json_body(
    value: &serde_json::Value,
    pointer: &str,
    key_hint: Option<&str>,
    depth: usize,
    visited: &mut usize,
    tokens: &mut Vec<Token>,
    token_names: &mut HashSet<String>,
) -> bool {
    const MAX_JSON_BODY_DEPTH: usize = 16;
    const MAX_JSON_BODY_NODES: usize = 512;
    const MAX_JSON_SCALAR_BYTES: usize = 16 * 1024;
    if depth > MAX_JSON_BODY_DEPTH || *visited >= MAX_JSON_BODY_NODES {
        return false;
    }
    *visited += 1;
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                if !tokenize_json_body(
                    child,
                    &format!("{pointer}/{escaped}"),
                    Some(key),
                    depth + 1,
                    visited,
                    tokens,
                    token_names,
                ) {
                    return false;
                }
            }
            true
        },
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                if !tokenize_json_body(
                    child,
                    &format!("{pointer}/{index}"),
                    key_hint,
                    depth + 1,
                    visited,
                    tokens,
                    token_names,
                ) {
                    return false;
                }
            }
            true
        },
        serde_json::Value::String(value) if value.len() <= MAX_JSON_SCALAR_BYTES => {
            // GraphQL operation documents are stable request structure; only
            // their variables should be candidates for templating.
            let trimmed = value.trim_start();
            if key_hint.is_some_and(|key| key.eq_ignore_ascii_case("query"))
                && (trimmed.starts_with("query ")
                    || trimmed.starts_with("query{")
                    || trimmed.starts_with("mutation ")
                    || trimmed.starts_with("mutation{")
                    || trimmed.starts_with("subscription ")
                    || trimmed.starts_with("subscription{"))
            {
                return true;
            }
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return false;
            }
            let key = key_hint.unwrap_or("value");
            tokens.push(Token {
                location: TokenLocation::JsonBodyPath {
                    pointer: pointer.to_owned(),
                    key: key.to_owned(),
                },
                name: unique_token_name(sanitize_name(key), token_names),
                value: value.clone(),
                value_type: TaskInputSchema::String,
            });
            true
        },
        serde_json::Value::String(_) => false,
        serde_json::Value::Number(value) => {
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return false;
            }
            tokens.push(Token {
                location: TokenLocation::JsonBodyPath {
                    pointer: pointer.to_owned(),
                    key: key_hint.unwrap_or("value").to_owned(),
                },
                name: unique_token_name(sanitize_name(key_hint.unwrap_or("value")), token_names),
                value: value.to_string(),
                value_type: TaskInputSchema::Number,
            });
            true
        },
        serde_json::Value::Bool(value) => {
            if tokens.len() >= MAX_REQUEST_TOKENS {
                return false;
            }
            tokens.push(Token {
                location: TokenLocation::JsonBodyPath {
                    pointer: pointer.to_owned(),
                    key: key_hint.unwrap_or("value").to_owned(),
                },
                name: unique_token_name(sanitize_name(key_hint.unwrap_or("value")), token_names),
                value: value.to_string(),
                value_type: TaskInputSchema::Boolean,
            });
            true
        },
        serde_json::Value::Null => true,
    }
}

fn looks_dynamic_segment(segment: &str) -> bool {
    segment.chars().any(|character| character.is_ascii_digit()) || segment.len() >= 16
}

fn sanitize_name(raw: &str) -> String {
    let value: String = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let value = value.trim_matches('_');
    if value.is_empty() {
        "value".to_owned()
    } else {
        value.to_owned()
    }
}

fn unique_token_name(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("the token suffix space is unbounded")
}

pub fn looks_like_now(value: &str, target_timestamp_ms: i64) -> Option<NowUnit> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let numeric: i64 = value.parse().ok()?;
    const WINDOW_MS: i64 = 2 * 60 * 60 * 1000;
    match value.len() {
        13 if (numeric - target_timestamp_ms).abs() <= WINDOW_MS => Some(NowUnit::Millis),
        10 if (numeric * 1000 - target_timestamp_ms).abs() <= WINDOW_MS => Some(NowUnit::Seconds),
        _ => None,
    }
}

pub fn looks_secret_like(value: &str) -> bool {
    value.len() >= 16
        && value.chars().any(|character| character.is_ascii_digit())
        && value
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && !value.contains(' ')
}

pub fn looks_volatile(value: &str) -> bool {
    let all_digits = value.chars().all(|character| character.is_ascii_digit());
    let hexish = value.len() >= 16
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-');
    (all_digits && value.len() >= 10)
        || hexish
        || (value.len() >= 24
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '+' | '/' | '=' | '-' | '_')
            }))
}

pub fn resolve_closure(context: &ResolveContext<'_>, targets: &[usize]) -> Vec<ResolvedRequest> {
    let mut queue: VecDeque<_> = targets.iter().map(|index| (*index, 0)).collect();
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    while let Some((index, depth)) = queue.pop_front() {
        if index >= context.traces.len() || !seen.insert(index) || depth > MAX_DEPTH {
            continue;
        }
        let request = resolve_request(context, index);
        for (_, resolution) in &request.params {
            if let TokenResolution::FromResponse { source_trace, .. } = resolution {
                if !seen.contains(source_trace) {
                    queue.push_back((*source_trace, depth + 1));
                }
            }
        }
        resolved.push(request);
    }
    resolved.sort_by_key(|request| context.traces[request.trace_index].timestamp);
    resolved
}

pub fn resolve_request(context: &ResolveContext<'_>, index: usize) -> ResolvedRequest {
    let trace = &context.traces[index];
    let params: Vec<_> = tokenize_request(trace)
        .into_iter()
        .map(|token| {
            let resolution = resolve_token(context, index, &token);
            (token, resolution)
        })
        .collect();
    let (url_template, body_template, headers_template) = templatize(trace, &params);
    ResolvedRequest {
        trace_index: index,
        url_template,
        body_template,
        headers_template,
        params,
    }
}

fn resolve_token(context: &ResolveContext<'_>, index: usize, token: &Token) -> TokenResolution {
    let trace = &context.traces[index];
    if matches!(&token.location, TokenLocation::OpaqueBody) {
        return TokenResolution::Literal { volatile: true };
    }
    if let TokenLocation::Header(header) = &token.location {
        if is_sensitive_header_name(header) {
            // CSRF/authorization material is sometimes minted by an earlier
            // document or bootstrap response. Prefer a bounded forward data
            // flow when the exact value is observable there; only fall back
            // to captured session state when the run contains no producer.
            if token.value.len() >= MIN_TOKEN_LEN && !is_redacted_value(&token.value) {
                for source_index in (0..index).rev() {
                    if let Some(extractor) =
                        find_in_response(&context.traces[source_index], &token.value)
                    {
                        return TokenResolution::FromResponse {
                            source_trace: source_index,
                            extractor,
                        };
                    }
                }
            }
            return TokenResolution::FromSession {
                scheme: header.clone(),
            };
        }
    }
    if let TokenLocation::Query { key, .. } = &token.location {
        let key = key.to_ascii_lowercase();
        // The capture boundary decides what auth is; asking for anything it
        // would not have stored yields a recipe that cannot replay. A public
        // identifier that merely travels beside a key (an application id) is
        // not auth, and falls through to a literal below.
        if crate::magician_v2::api_mining::auth_capture::is_auth_query_key(&key) {
            return TokenResolution::FromSession {
                scheme: format!("query:{key}"),
            };
        }
        if SIGNATURE_QUERY_KEYS.contains(&key.as_str()) {
            return TokenResolution::Literal { volatile: true };
        }
    }
    if let TokenLocation::BodyKey { key, .. } | TokenLocation::JsonBodyPath { key, .. } =
        &token.location
    {
        let key = key.to_ascii_lowercase();
        if SENSITIVE_BODY_KEYS.contains(&key.as_str()) {
            return TokenResolution::FromSession {
                scheme: format!("body:{key}"),
            };
        }
    }
    // A redaction marker is not a value or proof of a dependency. In
    // particular, unrelated auth headers and JSON fields all share it.
    if is_redacted_value(&token.value) {
        return TokenResolution::Literal { volatile: true };
    }
    // Traces are timestamp-sorted by the compiler. Restricting sources to an
    // earlier index prevents equal-timestamp dependency cycles.
    if token.value.len() >= MIN_TOKEN_LEN {
        for source_index in (0..index).rev() {
            if let Some(extractor) = find_in_response(&context.traces[source_index], &token.value) {
                return TokenResolution::FromResponse {
                    source_trace: source_index,
                    extractor,
                };
            }
        }
    } else if matches!(token.location, TokenLocation::PathSegment(_))
        && token
            .value
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        // A short numeric path segment (`/items/7`) is the list→detail
        // dependency most sites are built on. The length floor guards
        // against accidental matches in free text; an exact scalar under an
        // id-like key of an earlier JSON response is not accidental.
        for source_index in (0..index).rev() {
            if let Some(extractor) =
                find_id_in_response(&context.traces[source_index], &token.value)
            {
                return TokenResolution::FromResponse {
                    source_trace: source_index,
                    extractor,
                };
            }
        }
    }
    if let Some(unit) = looks_like_now(&token.value, trace.timestamp) {
        return TokenResolution::Now { unit };
    }
    // A header that no auth capture names and whose value no response or
    // cookie produced has no server-side source: it is client-minted (a page
    // nonce, a public key embedded in the site's JS). Requiring it from the
    // session store could never be satisfied and failed every replay closed
    // before the first request; the browser sent this exact value, so does
    // the replay.
    if matches!(&token.location, TokenLocation::Header(_)) && looks_secret_like(&token.value) {
        return TokenResolution::Literal { volatile: false };
    }
    if looks_secret_like(&token.value) || looks_volatile(&token.value) {
        return TokenResolution::Literal { volatile: true };
    }
    if context
        .typed_inputs
        .iter()
        .any(|value| value == &token.value)
    {
        return TokenResolution::FromTypedInput {
            name: token.name.clone(),
        };
    }
    // A value inside an array is one of a set of client-side options — a list of
    // searchable fields, a tag filter — not the task's own parameter. Binding it
    // just because the task happens to use the same word replaces a
    // distinguishing literal in the task shape with a slot, and the matcher can
    // then no longer tell that shape from its neighbours: the recipe is never
    // selected, so every run re-reads the page and recompiles the same recipe
    // again. An explicitly typed input is still honoured above; only incidental
    // agreement with the task's wording is refused here.
    if matches!(&token.location, TokenLocation::JsonBodyPath { pointer, .. }
        if pointer
            .rsplit('/')
            .next()
            .is_some_and(|segment| !segment.is_empty()
                && segment.chars().all(|character| character.is_ascii_digit())))
    {
        return TokenResolution::Literal { volatile: false };
    }
    if text_contains_token(context.task_text, &token.value) {
        return TokenResolution::FromTaskText {
            name: token.name.clone(),
        };
    }
    TokenResolution::Literal { volatile: false }
}

fn text_contains_token(text: &str, token: &str) -> bool {
    if text.is_empty() || token.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let token = token.to_lowercase();
    let starts_word = token.chars().next().is_some_and(char::is_alphanumeric);
    let ends_word = token.chars().next_back().is_some_and(char::is_alphanumeric);
    text.match_indices(&token).any(|(start, value)| {
        let end = start + value.len();
        let left_ok = !starts_word
            || text[..start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric());
        let right_ok = !ends_word
            || text[end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_alphanumeric());
        left_ok && right_ok
    })
}

pub(super) fn supported_child_json_path(path: &str, key: &str) -> Option<String> {
    if key.contains(['.', '[', ']']) {
        return None;
    }
    let candidate = format!("{path}.{key}");
    crate::magician_v2::api_mining::workflow_replay::jsonpath::is_supported_jsonpath(&candidate)
        .then_some(candidate)
}

pub(super) fn supported_index_json_path(path: &str, index: usize) -> Option<String> {
    let candidate = format!("{path}[{index}]");
    crate::magician_v2::api_mining::workflow_replay::jsonpath::is_supported_jsonpath(&candidate)
        .then_some(candidate)
}

/// Locate `value` as an exact scalar under an id-like key (`id`, `*_id`,
/// `*Id`, `key`, `slug`, `uuid`) of a 2xx JSON response.
fn find_id_in_response(trace: &NetworkTraceEvent, value: &str) -> Option<Extractor> {
    let body = trace.response_body.as_deref()?;
    if body.is_empty() || body.len() > MAX_SOURCE_BODY || !(200..300).contains(&trace.status) {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut visited = 0;
    find_id_path_bounded(&json, "$", None, value, 0, &mut visited)
        .map(|path| Extractor::JsonPath { path })
}

fn is_id_like_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "id"
        || lower == "key"
        || lower == "slug"
        || lower == "uuid"
        || lower.ends_with("_id")
        || lower.ends_with("id") && key.ends_with("Id")
}

fn find_id_path_bounded(
    value: &serde_json::Value,
    path: &str,
    key: Option<&str>,
    target: &str,
    depth: usize,
    visited: &mut usize,
) -> Option<String> {
    if depth > MAX_SOURCE_JSON_DEPTH || *visited >= MAX_SOURCE_JSON_NODES {
        return None;
    }
    *visited += 1;
    match value {
        serde_json::Value::String(text) if key.is_some_and(is_id_like_key) && text == target => {
            Some(path.to_owned())
        },
        serde_json::Value::Number(number)
            if key.is_some_and(is_id_like_key) && number.to_string() == target =>
        {
            Some(path.to_owned())
        },
        serde_json::Value::Object(map) => map.iter().find_map(|(child_key, child)| {
            let child_path = supported_child_json_path(path, child_key)?;
            find_id_path_bounded(
                child,
                &child_path,
                Some(child_key),
                target,
                depth + 1,
                visited,
            )
        }),
        serde_json::Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            let child_path = supported_index_json_path(path, index)?;
            find_id_path_bounded(child, &child_path, key, target, depth + 1, visited)
        }),
        _ => None,
    }
}

fn find_in_response(trace: &NetworkTraceEvent, value: &str) -> Option<Extractor> {
    if is_redacted_value(value) {
        return None;
    }
    for (name, header_value) in &trace.response_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "set-cookie" {
            for cookie in header_value.split(',') {
                if let Some((cookie_name, cookie_value)) = cookie.trim().split_once('=') {
                    if cookie_value.split(';').next().map(str::trim) == Some(value) {
                        return Some(Extractor::Cookie {
                            name: cookie_name.trim().to_owned(),
                        });
                    }
                }
            }
        } else if header_value == value {
            return Some(Extractor::Header { name: lower });
        }
    }
    let body = trace.response_body.as_deref()?;
    if body.is_empty() || body.len() > MAX_SOURCE_BODY || !(200..300).contains(&trace.status) {
        return None;
    }
    if let Ok(json) = serde_json::from_str(body) {
        return find_json_path(&json, "$", value).map(|path| Extractor::JsonPath { path });
    }
    let pattern = if body.trim() == value {
        r"(?s)\A\s*(.{1,200}?)\s*\z".to_owned()
    } else {
        let prefix = &body[..body.find(value)?];
        let anchor = match prefix.chars().next_back()? {
            // Use delimiter shape only, never nearby captured page content.
            // Generic delimiters are accepted only if the exact round-trip
            // below proves a single match in the entire response.
            delimiter @ ('>' | '"' | '\'') => regex::escape(&delimiter.to_string()),
            _ => {
                // Also support a small standalone `token=value` / `token: value`
                // response. Arbitrary surrounding text is not a durable anchor:
                // it can include another user's value or a captured credential.
                let key = prefix.trim_end().strip_suffix(['=', ':'])?.trim_end();
                if key.is_empty()
                    || key.len() > 64
                    || prefix.len() > 128
                    || !key.starts_with(|character: char| {
                        character.is_ascii_alphabetic() || character == '_'
                    })
                    || !key.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
                {
                    return None;
                }
                format!(r"\A{}", regex::escape(prefix))
            },
        };
        format!("{}([^<\"'\\n&]{{1,200}})", anchor)
    };
    // Finding the literal somewhere in the page does not prove that this
    // extractor returns it. In particular, `>` or `"` anchors often select an
    // earlier, unrelated element/attribute. Use the same unique-match semantics
    // as replay and reject truncation, over-capture, and ambiguous matches.
    if extract_unique_regex(body, &pattern, 1) != Some(value) {
        return None;
    }
    Some(Extractor::Regex { pattern, group: 1 })
}

fn is_redacted_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("[redacted") || lower.contains("<redacted")
}

pub fn find_json_path(value: &serde_json::Value, path: &str, target: &str) -> Option<String> {
    let mut visited = 0;
    find_json_path_bounded(value, path, target, 0, &mut visited)
}

fn find_json_path_bounded(
    value: &serde_json::Value,
    path: &str,
    target: &str,
    depth: usize,
    visited: &mut usize,
) -> Option<String> {
    if depth > MAX_SOURCE_JSON_DEPTH || *visited >= MAX_SOURCE_JSON_NODES {
        return None;
    }
    *visited += 1;
    match value {
        serde_json::Value::String(value) if value == target => Some(path.to_owned()),
        serde_json::Value::Number(value) if value.to_string() == target => Some(path.to_owned()),
        serde_json::Value::Bool(value) if value.to_string() == target => Some(path.to_owned()),
        serde_json::Value::Object(map) => map.iter().find_map(|(key, child)| {
            let child_path = supported_child_json_path(path, key)?;
            find_json_path_bounded(child, &child_path, target, depth + 1, visited)
        }),
        serde_json::Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            let child_path = supported_index_json_path(path, index)?;
            find_json_path_bounded(child, &child_path, target, depth + 1, visited)
        }),
        _ => None,
    }
}

/// What a body IS, which is not always what its header claims. Posting JSON
/// under a form content-type is common, and reading the header alone made the
/// three readers of a body disagree: the tokenizer left its fields frozen, the
/// templatizer wrote no placeholder for them, and the runner re-encoded it. A
/// single answer keeps them consistent — a parameter no template carries is
/// refused at replay preflight, so a disagreement here costs the whole recipe.
fn effective_body_format(trace: &NetworkTraceEvent) -> RequestBodyFormat {
    let declared = request_body_format(request_content_type(trace));
    if matches!(declared, RequestBodyFormat::Json | RequestBodyFormat::Infer) {
        return declared;
    }
    match trace.request_body.as_deref() {
        Some(body)
            if !crate::magician_v2::api_mining::recipe::is_urlencoded_form_body(body)
                && serde_json::from_str::<serde_json::Value>(body)
                    .is_ok_and(|json| json.is_object() || json.is_array()) =>
        {
            RequestBodyFormat::Json
        },
        _ => declared,
    }
}

fn request_content_type(trace: &NetworkTraceEvent) -> Option<&str> {
    trace
        .request_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
}

fn templatize(
    trace: &NetworkTraceEvent,
    params: &[(Token, TokenResolution)],
) -> (String, Option<String>, HashMap<String, String>) {
    let Ok(mut url) = url::Url::parse(&trace.url) else {
        return (
            trace.url.clone(),
            trace.request_body.clone(),
            HashMap::new(),
        );
    };
    let mut segments: Vec<String> = url
        .path_segments()
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect();
    let mut query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    // These are end-to-end representation metadata, not browser/session
    // headers. Dropping them changes JSON/form parsing and content negotiation.
    // Keep transport-owned headers (Host/Content-Length/etc.) excluded.
    let mut headers: HashMap<String, String> = trace
        .request_headers
        .iter()
        .filter(|(name, _)| {
            name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("accept")
        })
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    let mut body_template = trace.request_body.clone();

    for (token, resolution) in params {
        let stable_literal = matches!(resolution, TokenResolution::Literal { volatile: false });
        match &token.location {
            TokenLocation::PathSegment(index) if !stable_literal => {
                if let Some(segment) = segments.get_mut(*index) {
                    *segment = format!("{{{}}}", token.name);
                }
            },
            TokenLocation::Query { key, occurrence } if !stable_literal => {
                if let Some((_, value)) = query
                    .iter_mut()
                    .filter(|(candidate, _)| candidate == key)
                    .nth(*occurrence)
                {
                    *value = format!("{{{}}}", token.name);
                }
            },
            TokenLocation::Header(header) => {
                if !matches!(resolution, TokenResolution::FromSession { .. }) {
                    headers.insert(
                        header.clone(),
                        if stable_literal {
                            token.value.clone()
                        } else {
                            format!("{{{}}}", token.name)
                        },
                    );
                }
            },
            TokenLocation::BodyKey { .. } | TokenLocation::JsonBodyPath { .. }
                if !stable_literal => {},
            TokenLocation::OpaqueBody => {},
            _ => {},
        }
    }

    body_template = templatize_body(body_template, params, effective_body_format(trace));

    url.set_path(&segments.join("/"));
    if query.is_empty() {
        url.set_query(None);
    } else {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (key, value) in &query {
            pairs.append_pair(key, value);
        }
    }
    let mut template = url.to_string();
    // Restore only placeholders introduced above. Globally decoding braces
    // would corrupt stable query/path literals such as JSON filters.
    for (token, resolution) in params {
        if matches!(resolution, TokenResolution::Literal { volatile: false })
            || !matches!(
                &token.location,
                TokenLocation::PathSegment(_) | TokenLocation::Query { .. }
            )
        {
            continue;
        }
        for encoded in [
            format!("%7B{}%7D", token.name),
            format!("%7b{}%7d", token.name),
        ] {
            template = template.replace(&encoded, &format!("{{{}}}", token.name));
        }
    }
    (template, body_template, headers)
}

fn templatize_body(
    body: Option<String>,
    params: &[(Token, TokenResolution)],
    format: RequestBodyFormat,
) -> Option<String> {
    let body = body?;
    if let Some((token, _)) = params
        .iter()
        .find(|(token, _)| matches!(&token.location, TokenLocation::OpaqueBody))
    {
        return Some(format!("{{{}}}", token.name));
    }
    if let Some(mut json) = matches!(format, RequestBodyFormat::Json | RequestBodyFormat::Infer)
        .then(|| serde_json::from_str::<serde_json::Value>(&body).ok())
        .flatten()
        .filter(|json| json.is_object() || json.is_array())
    {
        for (token, resolution) in params {
            if matches!(resolution, TokenResolution::Literal { volatile: false }) {
                continue;
            }
            match &token.location {
                TokenLocation::JsonBodyPath { pointer, .. } => {
                    if let Some(slot) = json.pointer_mut(pointer) {
                        *slot = serde_json::Value::String(format!("{{{}}}", token.name));
                    }
                },
                // Preserve the original top-level representation for callers
                // that construct Token values directly in tests/extensions.
                TokenLocation::BodyKey { key, .. } => {
                    if let serde_json::Value::Object(map) = &mut json {
                        if map.contains_key(key) {
                            map.insert(
                                key.clone(),
                                serde_json::Value::String(format!("{{{}}}", token.name)),
                            );
                        }
                    }
                },
                _ => {},
            }
        }
        return serde_json::to_string(&json).ok().or(Some(body));
    }

    // A declared form Content-Type does not make the body a form; re-encoding
    // a raw-JSON body through the form serializer destroys it.
    if !crate::magician_v2::api_mining::recipe::is_urlencoded_form_body(&body) {
        return Some(body);
    }
    if format != RequestBodyFormat::Form
        && !(format == RequestBodyFormat::Infer && body.contains('='))
    {
        return Some(body);
    }
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if pairs.is_empty() {
        return Some(body);
    }
    for (token, resolution) in params {
        if matches!(resolution, TokenResolution::Literal { volatile: false }) {
            continue;
        }
        if let TokenLocation::BodyKey { key, occurrence } = &token.location {
            if let Some((_, value)) = pairs
                .iter_mut()
                .filter(|(candidate, _)| candidate == key)
                .nth(*occurrence)
            {
                *value = format!("{{{}}}", token.name);
            }
        }
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(pairs);
    Some(serializer.finish().replace("%7B", "{").replace("%7D", "}"))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn trace(url: &str, body: Option<&str>) -> NetworkTraceEvent {
        use crate::magician_v2::api_mining::types::{RequestInitiator, RequestTiming};

        NetworkTraceEvent {
            request_id: "request-fixture".into(),
            timestamp: 1_700_000_000_000,
            method: "POST".into(),
            url: url.into(),
            resource_type: Some("Fetch".into()),
            frame_id: None,
            request_headers: HashMap::new(),
            request_body: body.map(str::to_owned),
            tab_id: None,
            thread_id: None,
            status: 200,
            response_headers: HashMap::new(),
            response_body: Some("{}".into()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 1.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".into(),
                stack: None,
                url: None,
            },
            request_size: body.map_or(0, |value| value.len() as u64),
            response_size: 2,
            capture_source: Some("fixture".into()),
        }
    }

    #[test]
    fn representation_headers_survive_without_transport_owned_browser_headers() {
        let mut trace = trace(
            "https://example.test/api/items",
            Some(r#"{"name":"Alice"}"#),
        );
        trace.request_headers = HashMap::from([
            (
                "Content-Type".into(),
                "application/vnd.example+json; charset=utf-8".into(),
            ),
            (
                "ACCEPT".into(),
                "application/vnd.example+json; version=2".into(),
            ),
            ("Content-Length".into(), "999".into()),
            ("Host".into(), "example.test".into()),
            ("User-Agent".into(), "captured-browser".into()),
            ("If-None-Match".into(), "old-etag".into()),
        ]);
        let traces = [trace];
        let request = resolve_request(
            &ResolveContext {
                traces: &traces,
                typed_inputs: &[],
                task_text: "Set name to Alice",
            },
            0,
        );
        assert_eq!(
            request.headers_template,
            HashMap::from([
                (
                    "content-type".into(),
                    "application/vnd.example+json; charset=utf-8".into()
                ),
                (
                    "accept".into(),
                    "application/vnd.example+json; version=2".into()
                ),
            ])
        );
        assert!(request.body_template.unwrap().contains("{name}"));
    }

    #[test]
    fn explicit_non_form_bodies_with_equals_do_not_compile_as_form_fields() {
        for (content_type, body) in [
            ("application/graphql", "query Item($id: ID = 42) { item(id: $id) { name } }"),
            ("application/xml", "<item name=\"Alice\" />"),
            ("text/plain", "name=Alice&operation=replace"),
            ("multipart/form-data; boundary=upload", "--upload\r\nContent-Disposition: form-data; name=\"label\"\r\n\r\nAlice\r\n--upload--"),
            ("application/json", "name=Alice"),
        ] {
            let mut trace = trace("https://example.test/api/items", Some(body));
            trace.request_headers.insert("content-type".into(), content_type.into());
            let traces = [trace];
            let request = resolve_request(&ResolveContext {
                traces: &traces, typed_inputs: &[], task_text: "Set name to Alice",
            }, 0);
            assert!(request.params.iter().any(|(token, resolution)|
                token.location == TokenLocation::OpaqueBody
                    && matches!(resolution, TokenResolution::Literal { volatile: true })
            ), "unsupported representation must remain unresolved: {content_type}");
            assert!(!request.params.iter().any(|(token, _)| matches!(token.location, TokenLocation::BodyKey { .. })));
        }
    }

    #[test]
    fn encoded_path_inputs_bind_decoded_task_values() {
        let traces = [trace("https://example.test/items/New%20York%20123", None)];
        let request = resolve_request(
            &ResolveContext {
                traces: &traces,
                typed_inputs: &[],
                task_text: "Find New York 123",
            },
            0,
        );
        assert!(request
            .params
            .iter()
            .any(|(token, source)| token.value == "New York 123"
                && matches!(source, TokenResolution::FromTaskText { .. })));
        assert!(request.url_template.contains("{p1}"));
    }

    #[test]
    fn redacted_traces_cannot_create_auth_data_flows() {
        let mut first = trace("https://example.test/bootstrap", None);
        first
            .response_headers
            .insert("set-cookie".into(), "[REDACTED]".into());
        first.response_body = Some(r#"{"token":"[REDACTED]"}"#.into());
        let mut second = trace(
            "https://example.test/items",
            Some(r#"{"private_field":"[REDACTED]"}"#),
        );
        second
            .request_headers
            .insert("authorization".into(), "[REDACTED]".into());
        let traces = [first, second];
        let resolved = resolve_request(
            &ResolveContext {
                traces: &traces,
                typed_inputs: &[],
                task_text: "items",
            },
            1,
        );
        assert!(resolved.params.iter().any(|(_, source)| matches!(source, TokenResolution::FromSession { scheme } if scheme == "authorization")));
        assert!(!resolved
            .params
            .iter()
            .any(|(_, source)| matches!(source, TokenResolution::FromResponse { .. })));
        assert!(resolved
            .params
            .iter()
            .any(|(token, source)| token.name == "private_field"
                && matches!(source, TokenResolution::Literal { volatile: true })));
    }

    #[test]
    fn a_client_minted_header_replays_as_a_literal_not_a_session_requirement() {
        let mut document = trace("http://127.0.0.1:5000/?q=rust", None);
        document.method = "GET".into();
        document.timestamp = 1_000;
        document.response_body = Some("<html><body>shell</body></html>".into());
        let mut api = trace("http://127.0.0.1:5000/api/search?q=rust", None);
        api.method = "GET".into();
        api.timestamp = 2_000;
        api.request_headers.insert(
            "x-page-nonce".into(),
            "9f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f".into(),
        );
        api.response_body = Some(r#"{"hits":[{"points":3119}]}"#.into());
        let traces = vec![document, api];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "points of the top result for rust",
        };
        let resolved = resolve_closure(&context, &[1]);
        let request = resolved
            .iter()
            .find(|request| request.trace_index == 1)
            .expect("api step");
        let nonce = request
            .params
            .iter()
            .find(|(token, _)| token.location == TokenLocation::Header("x-page-nonce".into()))
            .expect("nonce token");
        assert!(
            matches!(nonce.1, TokenResolution::Literal { volatile: false }),
            "{:?}",
            nonce.1
        );
    }

    #[test]
    fn a_word_inside_a_body_array_is_not_the_tasks_own_input() {
        // Measured live 2026-09-14. A search client sends its list of
        // searchable fields in the body. The task asked for a title, the list
        // happened to contain "title", and binding it turned the task shape's
        // one distinguishing word into a slot — after which the matcher could
        // not pick the recipe out from its neighbours, replay was never
        // attempted, and every run re-read the page and recompiled.
        let json = r#"{"query":"wasm","restrictSearchableAttributes":["title","url"]}"#;
        let mut api = trace("https://dsn.example/1/indexes/Item/query", Some(json));
        api.method = "POST".into();
        api.timestamp = 1_000;
        api.response_body = Some(r#"{"hits":[{"title":"WASM 3.0"}]}"#.into());
        let traces = vec![api];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "title of the top story about wasm",
        };
        let resolved = resolve_closure(&context, &[0]);
        let request = resolved.first().expect("api step");
        let bound_from_task = |key: &str| {
            request.params.iter().any(|(token, resolution)| {
                matches!(&token.location, TokenLocation::JsonBodyPath { key: k, .. } if k == key)
                    && matches!(resolution, TokenResolution::FromTaskText { .. })
            })
        };
        // The real parameter still binds.
        assert!(
            bound_from_task("query"),
            "the task's search term must stay an input"
        );
        // The list member does not, however much the task's wording agrees.
        assert!(
            !bound_from_task("restrictSearchableAttributes"),
            "a list member must not become the task's input"
        );
        let template = request.body_template.as_deref().expect("a body template");
        assert!(
            template.contains("\"title\""),
            "the list must stay literal in the template: {template}"
        );
    }

    #[test]
    fn a_json_body_under_a_form_content_type_still_yields_its_fields_as_inputs() {
        // Measured live 2026-09-14. Reading the header instead of the body left
        // every field of such a body untokenized, so the task's own search term
        // froze into the template and later tasks replayed this one's question,
        // answering them confidently and wrongly.
        let json = r#"{"query":"rust","analyticsTags":["web"],"page":0}"#;
        let mut api = trace("https://dsn.example/1/indexes/Item/query", Some(json));
        api.method = "POST".into();
        api.timestamp = 1_000;
        api.request_headers.insert(
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        api.response_body = Some(r#"{"hits":[{"points":3119}]}"#.into());
        let traces = vec![api];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "points of the top result for rust",
        };
        let resolved = resolve_closure(&context, &[0]);
        let request = resolved.first().expect("api step");
        assert!(
            request.params.iter().any(|(token, resolution)| {
                matches!(&token.location, TokenLocation::JsonBodyPath { key, .. } if key == "query")
                    && matches!(resolution, TokenResolution::FromTaskText { .. })
            }),
            "the task's own term must become a slot, not a frozen literal"
        );
        assert!(
            !request
                .params
                .iter()
                .any(|(token, _)| matches!(token.location, TokenLocation::OpaqueBody)),
            "a parseable JSON body must not fall back to opaque"
        );
    }

    #[test]
    fn a_json_body_sent_under_a_form_content_type_survives_verbatim() {
        // Measured against a live search API: it posts raw JSON with
        // `application/x-www-form-urlencoded` to dodge a CORS preflight. Round
        // -tripping that through the form serializer percent-encodes the whole
        // document into a key, and the server answers 400.
        let json = r#"{"query":"rust","analyticsTags":["web"],"page":0}"#;
        let mut api = trace("https://dsn.example/1/indexes/Item/query", Some(json));
        api.method = "POST".into();
        api.timestamp = 1_000;
        api.request_headers.insert(
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        api.response_body = Some(r#"{"hits":[{"points":3119}]}"#.into());
        let traces = vec![api];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "points of the top result for rust",
        };
        let resolved = resolve_closure(&context, &[0]);
        let request = resolved.first().expect("api step");
        let template = request.body_template.as_deref().expect("a body template");
        // The point of this test is the encoding, not the spelling: the body
        // must stay JSON. Round-tripping it through the form serializer
        // percent-encodes the whole document into a single key and the server
        // answers 400. Field order is not preserved and does not matter.
        let parsed: serde_json::Value =
            serde_json::from_str(template).expect("the template must remain JSON");
        assert_eq!(parsed["page"], serde_json::json!(0));
        assert_eq!(parsed["analyticsTags"], serde_json::json!(["web"]));
        assert!(
            !template.contains("%22") && !template.contains("%7B"),
            "a non-form body must not be percent-encoded: {template}"
        );
        // And the task's own term is a slot, not the first task's question
        // frozen into every later replay.
        assert_eq!(parsed["query"], serde_json::json!("{query}"));
        // A real form body still gets its pairs templated.
        assert!(crate::magician_v2::api_mining::recipe::is_urlencoded_form_body("q=rust&page=0"));
        assert!(!crate::magician_v2::api_mining::recipe::is_urlencoded_form_body(json));
    }

    #[test]
    fn a_query_parameter_is_session_auth_only_if_the_capture_would_store_it() {
        // Measured against a live search UI: the compiler demanded
        // `…-application-id` from the session store, the capture never stored
        // it (its name carries no auth word), and every replay died
        // `class: auth` before sending. The two sides now share one rule.
        let mut document = trace("https://search.example/?q=rust", None);
        document.method = "GET".into();
        document.timestamp = 1_000;
        document.response_body = Some("<html><body>shell</body></html>".into());
        let mut api = trace(
            "https://dsn.example/1/indexes/Item/query?x-api-key=publicsearchkey&x-application-id=APPID123",
            None,
        );
        api.method = "GET".into();
        api.timestamp = 2_000;
        api.response_body = Some(r#"{"hits":[{"points":3119}]}"#.into());
        let traces = vec![document, api];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "points of the top result for rust",
        };
        let resolved = resolve_closure(&context, &[1]);
        let request = resolved
            .iter()
            .find(|request| request.trace_index == 1)
            .expect("api step");
        let resolution = |key: &str| {
            request
                .params
                .iter()
                .find(|(token, _)| {
                    matches!(&token.location, TokenLocation::Query { key: k, .. } if k.eq_ignore_ascii_case(key))
                })
                .map(|(_, source)| source.clone())
        };
        // A key the capture stores stays session auth.
        assert!(
            matches!(
                resolution("x-api-key"),
                Some(TokenResolution::FromSession { .. })
            ),
            "{:?}",
            resolution("x-api-key")
        );
        // A public identifier the capture ignores must NOT be session auth,
        // or the recipe asks for something nothing ever saved.
        assert!(
            !matches!(
                resolution("x-application-id"),
                Some(TokenResolution::FromSession { .. })
            ),
            "{:?}",
            resolution("x-application-id")
        );
        assert!(crate::magician_v2::api_mining::auth_capture::is_auth_query_key("x-api-key"));
        assert!(
            !crate::magician_v2::api_mining::auth_capture::is_auth_query_key("x-application-id")
        );
    }

    #[test]
    fn a_short_numeric_path_id_flows_from_the_listing_response() {
        let mut listing = trace("http://127.0.0.1:5000/api/search?q=rust", None);
        listing.method = "GET".into();
        listing.timestamp = 1_000;
        listing.response_body =
            Some(r#"{"count":6,"hits":[{"comments":412,"id":7,"points":3119}]}"#.into());
        let mut detail = trace("http://127.0.0.1:5000/api/items/7", None);
        detail.method = "GET".into();
        detail.timestamp = 2_000;
        detail.response_body = Some(r#"{"id":7,"author":"Ingrid Solvang"}"#.into());
        let traces = vec![listing, detail];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "author of the top result for rust",
        };
        let resolved = resolve_closure(&context, &[1]);
        assert_eq!(
            resolved.len(),
            2,
            "the listing is pulled in as the id's source"
        );
        let detail_step = resolved
            .iter()
            .find(|request| request.trace_index == 1)
            .unwrap();
        let id = detail_step
            .params
            .iter()
            .find(|(token, _)| token.location == TokenLocation::PathSegment(2))
            .expect("the id segment is a token");
        assert!(
            matches!(&id.1, TokenResolution::FromResponse { source_trace: 0, extractor: Extractor::JsonPath { path } } if path == "$.hits[0].id"),
            "{:?}",
            id.1
        );
        // A short number under a non-id key stays a literal.
        assert!(is_id_like_key("id") && is_id_like_key("item_id") && is_id_like_key("itemId"));
        assert!(!is_id_like_key("points") && !is_id_like_key("count"));
    }

    #[test]
    fn body_templating_targets_keys_instead_of_equal_values() {
        let trace = trace(
            "https://example.test/api",
            Some(r#"{"stable":"same-value","dynamic":"same-value"}"#),
        );
        let params = vec![
            (
                Token {
                    location: TokenLocation::BodyKey {
                        key: "stable".into(),
                        occurrence: 0,
                    },
                    name: "stable".into(),
                    value: "same-value".into(),
                    value_type: TaskInputSchema::String,
                },
                TokenResolution::Literal { volatile: false },
            ),
            (
                Token {
                    location: TokenLocation::BodyKey {
                        key: "dynamic".into(),
                        occurrence: 0,
                    },
                    name: "dynamic".into(),
                    value: "same-value".into(),
                    value_type: TaskInputSchema::String,
                },
                TokenResolution::FromTypedInput {
                    name: "dynamic".into(),
                },
            ),
        ];

        let (_, body, _) = templatize(&trace, &params);
        let body: serde_json::Value = serde_json::from_str(body.as_deref().unwrap()).unwrap();
        assert_eq!(body["stable"], "same-value");
        assert_eq!(body["dynamic"], "{dynamic}");
    }

    #[test]
    fn nested_graphql_variables_are_templated_without_touching_the_operation() {
        let traces = vec![trace(
            "https://example.test/graphql",
            Some(
                r#"{"query":"query Search($query: String!) { search(query: $query) { id } }","variables":{"query":"rust","limit":10}}"#,
            ),
        )];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "search rust",
        };
        let resolved = resolve_request(&context, 0);
        let body: serde_json::Value =
            serde_json::from_str(resolved.body_template.as_deref().unwrap()).unwrap();

        assert_eq!(body["variables"]["query"], "{query}");
        assert_eq!(body["variables"]["limit"], 10);
        assert!(body["query"].as_str().unwrap().starts_with("query Search"));
        assert!(resolved.params.iter().any(|(token, resolution)| {
            token.name == "query" && matches!(resolution, TokenResolution::FromTaskText { .. })
        }));
    }

    #[test]
    fn stable_encoded_query_braces_are_not_promoted_to_placeholders() {
        let trace = trace(
            "https://example.test/api?filter=%7B%22status%22%3A%22open%22%7D&q=rust",
            None,
        );
        let tokens = tokenize_request(&trace);
        let params: Vec<_> = tokens
            .into_iter()
            .map(|token| {
                let resolution = if token.name == "q" {
                    TokenResolution::FromTypedInput {
                        name: token.name.clone(),
                    }
                } else {
                    TokenResolution::Literal { volatile: false }
                };
                (token, resolution)
            })
            .collect();

        let (template, _, _) = templatize(&trace, &params);
        assert!(template.contains("q={q}"));
        assert!(!template.contains("filter={"));
        let concrete = template.replace("{q}", "coffee");
        let parsed = url::Url::parse(&concrete).unwrap();
        assert_eq!(
            parsed
                .query_pairs()
                .find(|(key, _)| key == "filter")
                .map(|(_, value)| value.into_owned()),
            Some(r#"{"status":"open"}"#.into())
        );
    }

    #[test]
    fn unsupported_raw_body_becomes_content_free_and_unreplayable() {
        let traces = vec![trace(
            "https://example.test/api",
            Some("private raw payload that must not persist"),
        )];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &[],
            task_text: "submit payload",
        };
        let resolved = resolve_request(&context, 0);

        assert_eq!(resolved.body_template.as_deref(), Some("{opaque_body}"));
        assert!(!resolved
            .body_template
            .as_deref()
            .unwrap()
            .contains("private raw payload"));
        assert!(resolved.params.iter().any(|(token, resolution)| {
            matches!(&token.location, TokenLocation::OpaqueBody)
                && matches!(resolution, TokenResolution::Literal { volatile: true })
        }));
    }

    #[test]
    fn duplicate_query_and_form_keys_receive_distinct_placeholders() {
        let trace = trace(
            "https://example.test/api?tag=alpha&tag=beta",
            Some("tag=first&tag=second"),
        );
        let tokens = tokenize_request(&trace);
        let names: HashSet<_> = tokens.iter().map(|token| token.name.as_str()).collect();
        assert_eq!(names.len(), tokens.len());
        let params: Vec<_> = tokens
            .into_iter()
            .map(|token| {
                let name = token.name.clone();
                (token, TokenResolution::FromTypedInput { name })
            })
            .collect();

        let (url, body, _) = templatize(&trace, &params);
        assert!(url.contains("tag={tag}"));
        assert!(url.contains("tag={tag_2}"));
        assert_eq!(body.as_deref(), Some("tag={tag_3}&tag={tag_4}"));
    }

    #[test]
    fn short_exact_task_values_are_inputs_but_substrings_are_not() {
        let traces = vec![trace("https://example.test/api?q=x&verb=go", None)];
        let typed = vec!["x".to_string()];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &typed,
            task_text: "search x in google",
        };
        let resolved = resolve_request(&context, 0);
        let by_name: HashMap<_, _> = resolved
            .params
            .into_iter()
            .map(|(token, resolution)| (token.name, resolution))
            .collect();
        assert!(matches!(
            by_name.get("q"),
            Some(TokenResolution::FromTypedInput { .. })
        ));
        assert!(matches!(
            by_name.get("verb"),
            Some(TokenResolution::Literal { volatile: false })
        ));
    }

    #[test]
    fn secret_like_typed_values_never_become_durable_task_inputs() {
        let secret = "privateTokenValue123456".to_string();
        let traces = vec![trace(&format!("https://example.test/api?q={secret}"), None)];
        let typed = vec![secret];
        let context = ResolveContext {
            traces: &traces,
            typed_inputs: &typed,
            task_text: "use the supplied value",
        };
        let resolved = resolve_request(&context, 0);

        assert!(resolved.params.iter().any(|(_, resolution)| matches!(
            resolution,
            TokenResolution::Literal { volatile: true }
        )));
    }

    #[test]
    fn unsupported_json_keys_do_not_produce_broken_extractors() {
        let json = serde_json::json!({"safe": {"dotted.key": "target"}});
        assert!(find_json_path(&json, "$", "target").is_none());
        assert_eq!(
            find_json_path(&serde_json::json!([{"id": "target"}]), "$", "target").as_deref(),
            Some("$[0].id")
        );
    }

    #[test]
    fn text_dependencies_must_reproduce_one_exact_captured_value() {
        let value = "coldNonce12345678";
        for body in [
            format!("<div>unrelated</div><span>{value}</span>"),
            format!("<span>{value}</span><div>unrelated</div>"),
            format!(r#"<input name="nonce" value="{value}">"#),
            format!("<span>{value} with suffix</span>"),
            format!("private account details: {value}"),
        ] {
            let mut source = trace("https://example.test/bootstrap", None);
            source.response_body = Some(body.clone());
            assert!(
                find_in_response(&source, value).is_none(),
                "unproven text dependency: {body}"
            );
        }
    }

    #[test]
    fn proven_text_dependencies_extract_fresh_values_without_persisting_capture_content() {
        let cold = "coldNonce12345678";
        let warm = "freshNonce87654321";
        for body in [
            cold.to_owned(),
            format!("nonce={cold}"),
            format!("<span>{cold}</span>"),
        ] {
            let mut source = trace("https://example.test/bootstrap", None);
            source.response_body = Some(body.clone());
            let Extractor::Regex { pattern, group } = find_in_response(&source, cold).unwrap()
            else {
                panic!("text dependency must be regex-backed");
            };
            assert!(!pattern.contains(cold));
            assert_eq!(extract_unique_regex(&body, &pattern, group), Some(cold));
            assert_eq!(
                extract_unique_regex(&body.replace(cold, warm), &pattern, group),
                Some(warm)
            );
        }
    }

    #[test]
    fn ambiguous_html_does_not_override_captured_session_auth() {
        let token = "coldNonce12345678";
        let mut bootstrap = trace("https://example.test/bootstrap", None);
        bootstrap.response_body = Some(format!("<div>unrelated</div><span>{token}</span>"));
        let mut target = trace("https://example.test/api/items", None);
        target
            .request_headers
            .insert("x-csrf-token".into(), token.into());
        let traces = [bootstrap, target];
        let resolved = resolve_request(
            &ResolveContext {
                traces: &traces,
                typed_inputs: &[],
                task_text: "Fetch items",
            },
            1,
        );
        assert!(resolved.params.iter().any(|(token, resolution)| {
            token.location == TokenLocation::Header("x-csrf-token".into())
                && matches!(resolution, TokenResolution::FromSession { scheme } if scheme == "x-csrf-token")
        }));
        assert!(!resolved
            .params
            .iter()
            .any(|(_, resolution)| matches!(resolution, TokenResolution::FromResponse { .. })));
    }
}
