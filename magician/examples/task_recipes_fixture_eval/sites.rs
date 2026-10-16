//! In-process fixture sites for the Task Recipes fixture eval.
//!
//! Every site records every request it receives. The page JS mints a fresh
//! `X-Page-Nonce` per load and fires a beacon on every load, so the driver can
//! tell "a browser executed this page" from "a replay resent captured
//! requests" without trusting `User-Agent` or `Sec-Fetch-*` (both replay
//! verbatim from header templates). `/__eval/*` is the driver's control plane;
//! the page JS never calls it, so it cannot enter a recipe.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use actix_web::{
    dev::ServerHandle,
    http::header::{HeaderMap, CONTENT_TYPE},
    web, HttpRequest, HttpResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod board;
pub mod catalog;
pub mod notes;
pub mod portal;
pub mod session;

pub const NONCE_HEADER: &str = "x-page-nonce";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const BEACON_PATHS: &[&str] = &["/t/collect", "/px.gif"];
pub const EVAL_PREFIX: &str = "/__eval/";
const MAX_LOG_RECORDS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Api,
    Document,
    Beacon,
    Static,
    EvalInternal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub seq: u64,
    pub at_ms: u64,
    pub method: String,
    pub path: String,
    pub query: String,
    pub kind: RequestKind,
    pub page_nonce: Option<String>,
    pub has_cookie: bool,
    /// First bytes of the `sid` the request carried. The fixture's session ids
    /// are fixture-only secrets, but a prefix is enough to tell "replayed a
    /// session this site never issued" from "sent none at all".
    pub cookie_sid_prefix: Option<String>,
    pub has_csrf: bool,
    pub user_agent: Option<String>,
    pub sec_fetch_mode: Option<String>,
    pub sec_fetch_dest: Option<String>,
    /// JSON top-level keys only, never values.
    pub body_keys: Vec<String>,
    /// Filled by the handler after it decides the status.
    pub status: u16,
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

impl RequestRecord {
    pub fn classify(method: &str, path: &str, headers: &HeaderMap) -> Self {
        let kind = if path.starts_with(EVAL_PREFIX) {
            RequestKind::EvalInternal
        } else if BEACON_PATHS.contains(&path) {
            RequestKind::Beacon
        } else if path.starts_with("/api/") || path == "/graphql" {
            RequestKind::Api
        } else if path.ends_with(".js") || path.ends_with(".css") || path.ends_with(".ico") {
            RequestKind::Static
        } else {
            RequestKind::Document
        };
        Self {
            seq: 0,
            at_ms: now_ms(),
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
            query: String::new(),
            kind,
            page_nonce: header_string(headers, NONCE_HEADER),
            has_cookie: header_string(headers, "cookie").is_some(),
            cookie_sid_prefix: header_string(headers, "cookie").and_then(|cookie| {
                cookie
                    .split(';')
                    .filter_map(|pair| pair.trim().split_once('='))
                    .find(|(name, _)| name.trim().ends_with("sid"))
                    .map(|(_, value)| value.trim().chars().take(8).collect())
            }),
            has_csrf: header_string(headers, CSRF_HEADER).is_some(),
            user_agent: header_string(headers, "user-agent"),
            sec_fetch_mode: header_string(headers, "sec-fetch-mode"),
            sec_fetch_dest: header_string(headers, "sec-fetch-dest"),
            body_keys: Vec::new(),
            status: 0,
        }
    }
}

#[derive(Default)]
pub struct RequestLog {
    inner: Mutex<(u64, Vec<RequestRecord>)>,
}

impl RequestLog {
    pub fn push(&self, mut record: RequestRecord) -> u64 {
        let mut guard = self.inner.lock().expect("request log poisoned");
        guard.0 += 1;
        record.seq = guard.0;
        let seq = record.seq;
        guard.1.push(record);
        if guard.1.len() > MAX_LOG_RECORDS {
            let overflow = guard.1.len() - MAX_LOG_RECORDS;
            guard.1.drain(..overflow);
        }
        seq
    }

    /// The sequence number the next pushed record will get.
    pub fn mark(&self) -> u64 {
        self.inner.lock().expect("request log poisoned").0 + 1
    }

    pub fn since(&self, mark: u64) -> Vec<RequestRecord> {
        self.inner
            .lock()
            .expect("request log poisoned")
            .1
            .iter()
            .filter(|record| record.seq >= mark)
            .cloned()
            .collect()
    }

    pub fn all(&self) -> Vec<RequestRecord> {
        self.inner.lock().expect("request log poisoned").1.clone()
    }

    pub fn set_status(&self, seq: u64, status: u16) {
        let mut guard = self.inner.lock().expect("request log poisoned");
        if let Some(record) = guard.1.iter_mut().find(|record| record.seq == seq) {
            record.status = status;
        }
    }

    /// Clears records; the sequence counter stays monotonic so marks taken
    /// before a reset never alias records logged after it.
    pub fn reset(&self) {
        self.inner.lock().expect("request log poisoned").1.clear();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Knobs {
    /// `None` = sessions never expire. `Some(0)` = every session is expired.
    pub session_ttl_secs: Option<u64>,
    pub schema_version: u32,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            session_ttl_secs: None,
            schema_version: 1,
        }
    }
}

impl Knobs {
    pub fn merged(&self, patch: &Value) -> Self {
        let mut next = self.clone();
        if let Some(object) = patch.as_object() {
            if let Some(ttl) = object.get("session_ttl_secs") {
                next.session_ttl_secs = ttl.as_u64();
            }
            if let Some(version) = object.get("schema_version").and_then(Value::as_u64) {
                next.schema_version = version as u32;
            }
        }
        next
    }
}

/// Shared per-site state handed to actix handlers.
pub struct SiteState {
    pub name: &'static str,
    pub log: RequestLog,
    pub knobs: Mutex<Knobs>,
    pub sessions: session::SessionTable,
    /// Mutable site data (notes list, etc.). Sites decide the shape.
    pub data: Mutex<Value>,
}

impl SiteState {
    pub fn new(name: &'static str, data: Value) -> Arc<Self> {
        Arc::new(Self {
            name,
            log: RequestLog::default(),
            knobs: Mutex::new(Knobs::default()),
            sessions: session::SessionTable::for_site(name),
            data: Mutex::new(data),
        })
    }

    pub fn knobs(&self) -> Knobs {
        self.knobs.lock().expect("knobs poisoned").clone()
    }
}

/// Every handler calls this first; it returns the record's sequence number so
/// the handler can stamp the final status with `finish`.
pub fn record(state: &SiteState, req: &HttpRequest, body_keys: Vec<String>) -> u64 {
    let mut record = RequestRecord::classify(req.method().as_str(), req.path(), req.headers());
    record.query = req.query_string().to_owned();
    record.body_keys = body_keys;
    state.log.push(record)
}

pub fn finish(state: &SiteState, seq: u64, response: HttpResponse) -> HttpResponse {
    state.log.set_status(seq, response.status().as_u16());
    response
}

pub fn json_keys(body: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .map(|object| object.keys().cloned().collect())
        })
        .unwrap_or_default()
}

pub fn html(body: String) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, "text/html; charset=utf-8"))
        .insert_header(("Cache-Control", "no-store"))
        .body(body)
}

/// The shell HTML shared by every site: the nonce + beacon prologue and a
/// `fetchJson` helper the site-specific script calls.
pub fn shell(title: &str, csrf_token: Option<&str>, body: &str, script: &str) -> String {
    let csrf_meta = csrf_token
        .map(|token| format!(r#"<meta name="csrf-token" content="{token}">"#))
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
{csrf_meta}
<style>body{{font:15px system-ui;max-width:720px;margin:32px auto;padding:0 16px}}li{{margin:6px 0}}.muted{{color:#666}}table{{border-collapse:collapse}}td,th{{padding:6px 10px;border-bottom:1px solid #ddd;text-align:left}}</style>
</head>
<body>
{body}
<img id="px" alt="" width="1" height="1" src="/px.gif?e=view">
<script>
window.__nonce = (crypto && crypto.randomUUID) ? crypto.randomUUID() : String(Math.random()).slice(2);
try {{ navigator.sendBeacon('/t/collect', JSON.stringify({{e: 'view', p: location.pathname, n: window.__nonce}})); }} catch (_) {{}}
function csrfToken() {{ var m = document.querySelector('meta[name="csrf-token"]'); return m ? m.content : ''; }}
async function fetchJson(url, init) {{
  init = init || {{}};
  var headers = Object.assign({{'X-Page-Nonce': window.__nonce}}, init.headers || {{}});
  if (init.method && init.method !== 'GET') {{
    headers['Content-Type'] = 'application/json';
    if (csrfToken()) headers['X-CSRF-Token'] = csrfToken();
  }}
  var res = await fetch(url, Object.assign({{}}, init, {{headers: headers, credentials: 'same-origin'}}));
  var text = await res.text();
  var data = null; try {{ data = JSON.parse(text); }} catch (_) {{ data = {{raw: text}}; }}
  return {{status: res.status, data: data}};
}}
function esc(s) {{ return String(s).replace(/[&<>"]/g, function(c) {{ return {{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}}[c]; }}); }}
{script}
</script>
</body>
</html>
"#
    )
}

async fn eval_requests(
    state: web::Data<Arc<SiteState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let since = query
        .get("since")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let records = if since == 0 {
        state.log.all()
    } else {
        state.log.since(since)
    };
    HttpResponse::Ok()
        .json(json!({ "site": state.name, "next_mark": state.log.mark(), "requests": records }))
}

async fn eval_reset(state: web::Data<Arc<SiteState>>) -> HttpResponse {
    state.log.reset();
    HttpResponse::NoContent().finish()
}

async fn eval_knobs(state: web::Data<Arc<SiteState>>, body: web::Json<Value>) -> HttpResponse {
    let next = state.knobs().merged(&body);
    *state.knobs.lock().expect("knobs poisoned") = next.clone();
    HttpResponse::Ok().json(next)
}

async fn eval_sessions(state: web::Data<Arc<SiteState>>) -> HttpResponse {
    HttpResponse::Ok().json(state.sessions.snapshot())
}

async fn beacon_collect(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let seq = record(&state, &req, json_keys(&body));
    finish(&state, seq, HttpResponse::NoContent().finish())
}

const GIF_1X1: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";

async fn beacon_pixel(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    finish(
        &state,
        seq,
        HttpResponse::Ok()
            .insert_header((CONTENT_TYPE, "image/gif"))
            .insert_header(("Cache-Control", "no-store"))
            .body(GIF_1X1),
    )
}

/// Wire the `/__eval/*` control routes and the beacon routes onto any site.
pub fn mount_common(cfg: &mut web::ServiceConfig) {
    cfg.route("/__eval/requests", web::get().to(eval_requests))
        .route("/__eval/reset", web::post().to(eval_reset))
        .route("/__eval/knobs", web::post().to(eval_knobs))
        .route("/__eval/sessions", web::get().to(eval_sessions))
        .route("/t/collect", web::post().to(beacon_collect))
        .route("/px.gif", web::get().to(beacon_pixel));
}

pub type Configure = fn(&mut web::ServiceConfig);

pub struct RunningSite {
    pub name: &'static str,
    pub origin: String,
    pub state: Arc<SiteState>,
    handle: Option<ServerHandle>,
}

impl RunningSite {
    pub async fn stop(&self) {
        if let Some(handle) = &self.handle {
            handle.stop(true).await;
        }
    }

    #[cfg(test)]
    pub fn for_tests(name: &'static str, origin: &str, data: Value) -> Self {
        Self {
            name,
            origin: origin.to_owned(),
            state: SiteState::new(name, data),
            handle: None,
        }
    }
}

/// Start every site on its own port on a dedicated actix thread. Returns in
/// site order once each server has bound.
pub fn serve_all(specs: Vec<(&'static str, Value, Configure)>) -> anyhow::Result<Vec<RunningSite>> {
    let (tx, rx) = std::sync::mpsc::channel::<
        anyhow::Result<(&'static str, String, Arc<SiteState>, ServerHandle)>,
    >();
    let count = specs.len();
    std::thread::Builder::new()
        .name("fixture-sites".into())
        .spawn(move || {
            let system = actix_web::rt::System::new();
            system.block_on(async move {
                let mut servers = Vec::with_capacity(count);
                for (name, data, configure) in specs {
                    let state = SiteState::new(name, data);
                    let app_state = Arc::clone(&state);
                    let bound = actix_web::HttpServer::new(move || {
                        actix_web::App::new()
                            .app_data(web::Data::new(Arc::clone(&app_state)))
                            .configure(mount_common)
                            .configure(configure)
                    })
                    .workers(2)
                    .disable_signals()
                    .bind(("127.0.0.1", 0));
                    let server = match bound {
                        Ok(server) => server,
                        Err(error) => {
                            let _ = tx.send(Err(anyhow::anyhow!("bind {name}: {error}")));
                            return;
                        },
                    };
                    let port = server
                        .addrs()
                        .first()
                        .map(|address| address.port())
                        .unwrap_or_default();
                    let running = server.run();
                    let handle = running.handle();
                    // Every server future must be polled concurrently; awaiting
                    // them one by one would leave the later sites unserved.
                    servers.push(actix_web::rt::spawn(running));
                    let _ = tx.send(Ok((
                        name,
                        format!("http://127.0.0.1:{port}"),
                        state,
                        handle,
                    )));
                }
                for server in servers {
                    let _ = server.await;
                }
            });
        })?;
    let mut sites = Vec::with_capacity(count);
    for _ in 0..count {
        let (name, origin, state, handle) = rx.recv()??;
        sites.push(RunningSite {
            name,
            origin,
            state,
            handle: Some(handle),
        });
    }
    Ok(sites)
}

#[cfg(test)]
pub(crate) fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(name.parse().unwrap(), value.parse().unwrap());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_record_classifies_beacon_document_and_api() {
        let beacon = RequestRecord::classify(
            "POST",
            "/t/collect",
            &headers(&[("sec-fetch-mode", "no-cors")]),
        );
        assert_eq!(beacon.kind, RequestKind::Beacon);
        let pixel = RequestRecord::classify("GET", "/px.gif", &headers(&[]));
        assert_eq!(pixel.kind, RequestKind::Beacon);
        let doc =
            RequestRecord::classify("GET", "/about", &headers(&[("accept", "text/html,*/*")]));
        assert_eq!(doc.kind, RequestKind::Document);
        let api =
            RequestRecord::classify("GET", "/api/search", &headers(&[("x-page-nonce", "abc")]));
        assert_eq!(api.kind, RequestKind::Api);
        assert_eq!(api.page_nonce.as_deref(), Some("abc"));
        let graphql = RequestRecord::classify("post", "/graphql", &headers(&[]));
        assert_eq!(graphql.kind, RequestKind::Api);
        assert_eq!(graphql.method, "POST");
        let internal = RequestRecord::classify("GET", "/__eval/requests", &headers(&[]));
        assert_eq!(internal.kind, RequestKind::EvalInternal);
    }

    #[test]
    fn request_log_window_is_by_sequence_not_time() {
        let log = RequestLog::default();
        log.push(RequestRecord::classify("GET", "/api/a", &headers(&[])));
        let mark = log.mark();
        log.push(RequestRecord::classify("GET", "/api/b", &headers(&[])));
        let window = log.since(mark);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].path, "/api/b");
        log.reset();
        let after_reset = log.mark();
        log.push(RequestRecord::classify("GET", "/api/c", &headers(&[])));
        assert_eq!(
            log.since(mark).len(),
            1,
            "reset keeps the counter monotonic"
        );
        assert_eq!(log.since(after_reset)[0].path, "/api/c");
    }

    #[test]
    fn knobs_default_and_update() {
        let knobs = Knobs::default();
        assert_eq!(knobs.session_ttl_secs, None);
        assert_eq!(knobs.schema_version, 1);
        let updated = knobs.merged(&json!({"session_ttl_secs": 5, "schema_version": 2}));
        assert_eq!(updated.session_ttl_secs, Some(5));
        assert_eq!(updated.schema_version, 2);
        let cleared = updated.merged(&json!({"session_ttl_secs": null}));
        assert_eq!(cleared.session_ttl_secs, None);
        assert_eq!(cleared.schema_version, 2);
    }

    #[test]
    fn json_keys_never_carry_values() {
        let keys = json_keys(br#"{"text":"buy milk","secret":"s3"}"#);
        assert_eq!(keys, vec!["secret".to_string(), "text".to_string()]);
        assert!(json_keys(b"not json").is_empty());
    }

    #[test]
    fn shell_embeds_nonce_beacon_and_optional_csrf() {
        let page = shell("T", Some("tok"), "<p>x</p>", "");
        assert!(page.contains(r#"name="csrf-token" content="tok""#));
        assert!(page.contains("sendBeacon('/t/collect'"));
        assert!(page.contains("'X-Page-Nonce': window.__nonce"));
        assert!(!shell("T", None, "", "").contains(r#"<meta name="csrf-token""#));
    }
}
