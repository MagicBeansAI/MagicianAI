//! HAR conversion for headed/headless browser API-mining capture.

use crate::magician_v2::api_mining::types::{NetworkTraceEvent, RequestInitiator, RequestTiming};
use base64::Engine as _;
use std::collections::HashMap;

const MAX_CAPTURED_BODY_BYTES: usize = 1024 * 1024;

/// `agent-browser network har start` takes no path: the archive is exported
/// by `har stop <path>`. Passing the path at start was silently ignored and
/// left every headed/headless session without a HAR, so API mining captured
/// nothing outside the CDP-proxy mode.
pub fn har_start_args() -> [&'static str; 3] {
    ["network", "har", "start"]
}

/// `agent-browser network har stop <path>` writes the archive to `path`.
pub fn har_stop_args(path: &str) -> [&str; 4] {
    ["network", "har", "stop", path]
}

pub fn har_to_traces(har: &serde_json::Value, thread_id: Option<&str>) -> Vec<NetworkTraceEvent> {
    let Some(entries) = har
        .pointer("/log/entries")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry_to_trace(index, entry, thread_id))
        .collect()
}

fn entry_to_trace(
    index: usize,
    entry: &serde_json::Value,
    thread_id: Option<&str>,
) -> Option<NetworkTraceEvent> {
    let request = entry.get("request")?;
    let response = entry.get("response")?;
    let method = request.get("method")?.as_str()?.to_owned();
    let url = request.get("url")?.as_str()?.to_owned();
    if !matches!(url::Url::parse(&url).ok()?.scheme(), "http" | "https") {
        return None;
    }
    let timestamp = entry
        .get("startedDateTime")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis())
        .unwrap_or_default();
    let response_body = response
        .pointer("/content/text")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| decode_body(text, response.pointer("/content/encoding")))
        .map(|body| truncate_utf8(body, MAX_CAPTURED_BODY_BYTES));
    let request_body = request
        .pointer("/postData/text")
        .and_then(serde_json::Value::as_str)
        .map(|body| truncate_utf8(body.to_owned(), MAX_CAPTURED_BODY_BYTES));
    let request_size = request_body.as_ref().map_or(0, |body| body.len() as u64);
    let response_size = response_body.as_ref().map_or_else(
        || {
            response
                .pointer("/content/size")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                .max(0) as u64
        },
        |body| body.len() as u64,
    );
    Some(NetworkTraceEvent {
        request_id: format!("har-{timestamp}-{index}"),
        method,
        url,
        resource_type: entry
            .get("_resourceType")
            .and_then(serde_json::Value::as_str)
            .map(normalize_resource_type),
        frame_id: None,
        request_headers: request_headers_with_cookies(request),
        request_body,
        tab_id: None,
        thread_id: thread_id.map(str::to_owned),
        response_headers: headers(response.get("headers")),
        response_body,
        body_unavailable_reason: None,
        failure_error_text: None,
        failure_blocked_reason: None,
        failure_canceled: None,
        status: response
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
            .min(u16::MAX as u64) as u16,
        timing: RequestTiming {
            request_time: timestamp as f64,
            dns_duration: non_negative(entry.pointer("/timings/dns")),
            connect_duration: non_negative(entry.pointer("/timings/connect")),
            ssl_duration: non_negative(entry.pointer("/timings/ssl")),
            ttfb: non_negative(entry.pointer("/timings/wait")),
            total_duration: entry
                .get("time")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default()
                .max(0.0),
        },
        initiator: RequestInitiator {
            initiator_type: entry
                .pointer("/_initiator/type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("har")
                .to_owned(),
            stack: None,
            url: entry
                .pointer("/_initiator/url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        },
        timestamp,
        request_size,
        response_size,
        capture_source: Some("har".into()),
    })
}

/// HAR keeps request cookies in their own `cookies` array, and exporters
/// routinely omit the `Cookie` header from `headers` because the browser
/// treats it as forbidden. Reading only `headers` therefore loses the session
/// entirely: the auth drain finds no cookie to capture, and the recipe
/// compiler cannot see that the request was authenticated, so a cookie-gated
/// endpoint compiles as anonymous and every replay 401s. Fold the array back
/// into the header the rest of the pipeline reads.
fn request_headers_with_cookies(request: &serde_json::Value) -> HashMap<String, String> {
    let mut output = headers(request.get("headers"));
    if output.contains_key("cookie") {
        return output;
    }
    let pairs: Vec<String> = request
        .get("cookies")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|cookie| {
            let name = cookie.get("name").and_then(serde_json::Value::as_str)?;
            let value = cookie.get("value").and_then(serde_json::Value::as_str)?;
            (!name.is_empty()).then(|| format!("{name}={value}"))
        })
        .collect();
    if !pairs.is_empty() {
        output.insert("cookie".to_owned(), pairs.join("; "));
    }
    output
}

fn headers(value: Option<&serde_json::Value>) -> HashMap<String, String> {
    let mut output = HashMap::new();
    for header in value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = header.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(value) = header.get("value").and_then(serde_json::Value::as_str) else {
            continue;
        };
        output
            .entry(name.to_ascii_lowercase())
            .and_modify(|current: &mut String| {
                current.push('\n');
                current.push_str(value);
            })
            .or_insert_with(|| value.to_owned());
    }
    output
}

fn decode_body(text: &str, encoding: Option<&serde_json::Value>) -> Option<String> {
    if encoding.and_then(serde_json::Value::as_str) == Some("base64") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(text)
            .ok()?;
        String::from_utf8(decoded).ok()
    } else {
        Some(text.to_owned())
    }
}

fn truncate_utf8(mut value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    let mut boundary = max;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn non_negative(value: Option<&serde_json::Value>) -> Option<f64> {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|value| *value >= 0.0)
}

fn normalize_resource_type(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "xhr" => "XHR".into(),
        "fetch" => "Fetch".into(),
        "document" => "Document".into(),
        other => {
            let mut characters = other.chars();
            characters
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                .unwrap_or_else(|| "Other".into())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn har_path_travels_with_stop_not_start() {
        assert_eq!(har_start_args(), ["network", "har", "start"]);
        assert_eq!(
            har_stop_args("/tmp/x.har"),
            ["network", "har", "stop", "/tmp/x.har"]
        );
    }

    fn entry_with_cookies(
        cookies: serde_json::Value,
        headers: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({"log": {"entries": [{
            "startedDateTime": "2026-09-13T21:12:34.000Z",
            "time": 4.0,
            "request": {
                "method": "GET",
                "url": "http://127.0.0.1:57385/api/me/orders",
                "headers": headers,
                "cookies": cookies,
            },
            "response": {"status": 200, "headers": [], "content": {"text": "{}", "size": 2}},
        }]}})
    }

    #[test]
    fn har_request_cookies_become_the_cookie_header() {
        // The exporter split the session out of `headers` into `cookies`.
        let har = entry_with_cookies(
            serde_json::json!([
                {"name": "sid", "value": "abc123"},
                {"name": "theme", "value": "dark"},
            ]),
            serde_json::json!([{"name": "User-Agent", "value": "probe"}]),
        );
        let traces = har_to_traces(&har, None);
        assert_eq!(traces.len(), 1);
        assert_eq!(
            traces[0].request_headers.get("cookie").map(String::as_str),
            Some("sid=abc123; theme=dark")
        );
    }

    #[test]
    fn an_existing_cookie_header_is_left_alone_and_no_cookies_add_none() {
        let har = entry_with_cookies(
            serde_json::json!([{"name": "sid", "value": "from-array"}]),
            serde_json::json!([{"name": "Cookie", "value": "sid=from-header"}]),
        );
        let traces = har_to_traces(&har, None);
        assert_eq!(
            traces[0].request_headers.get("cookie").map(String::as_str),
            Some("sid=from-header")
        );

        let bare = entry_with_cookies(serde_json::json!([]), serde_json::json!([]));
        let traces = har_to_traces(&bare, None);
        assert!(!traces[0].request_headers.contains_key("cookie"));
    }
}
