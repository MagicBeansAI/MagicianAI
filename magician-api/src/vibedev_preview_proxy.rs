use actix::{Actor, ActorContext, AsyncContext, Handler, StreamHandler};
use actix_web::http::header;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TMsg;

use magician_media::dev_server::dev_server_manager;

const HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

pub async fn vibedev_preview_proxy_handler(
    req: HttpRequest,
    body: web::Payload,
    path: web::Path<(String, String)>,
) -> Result<HttpResponse, ActixError> {
    let (project_id, tail) = path.into_inner();
    let manager = dev_server_manager();
    let port = match manager.port_for(&project_id).await {
        Some(port) => port,
        None => {
            let diagnostic = match manager.status(&project_id).await {
                Some(status) => format!(
                    "dev server not ready: {:?}\n{}",
                    status.status, status.recent_log_tail
                ),
                None => "dev server not running".to_string(),
            };
            return Ok(HttpResponse::ServiceUnavailable()
                .content_type("text/plain; charset=utf-8")
                .body(diagnostic));
        },
    };
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if is_ws {
        ws_relay(req, body, port, tail).await
    } else {
        http_forward(req, body, port, tail).await
    }
}

async fn http_forward(
    req: HttpRequest,
    body: web::Payload,
    port: u16,
    tail: String,
) -> Result<HttpResponse, ActixError> {
    let qs = req.query_string();
    let url = format!(
        "http://127.0.0.1:{port}/{tail}{}{}",
        if qs.is_empty() { "" } else { "?" },
        qs
    );
    let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
        Ok(method) => method,
        Err(error) => {
            return Ok(
                HttpResponse::BadRequest().body(format!("unsupported proxy method: {error}"))
            );
        },
    };
    let client = reqwest::Client::new();
    let mut rb = client
        .request(method, &url)
        .header(reqwest::header::HOST, format!("localhost:{port}"))
        .body(payload_body(body));
    for (name, value) in req.headers() {
        if is_hop_header(name.as_str()) {
            continue;
        }
        let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
        else {
            continue;
        };
        let Ok(header_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        rb = rb.header(header_name, header_value);
    }
    let upstream = match rb.send().await {
        Ok(response) => response,
        Err(error) => {
            return Ok(HttpResponse::BadGateway().body(format!("dev server proxy error: {error}")));
        },
    };
    let status = actix_web::http::StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);
    // HTML documents get the click-to-edit overlay injected (U7). Everything
    // else (JS/CSS/assets/HMR) streams untouched.
    let is_html = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false);
    let mut out = HttpResponse::build(status);
    for (name, value) in upstream.headers() {
        if is_hop_header(name.as_str()) {
            continue;
        }
        let Ok(header_name) = actix_web::http::header::HeaderName::try_from(name.as_str()) else {
            continue;
        };
        let Ok(header_value) = actix_web::http::header::HeaderValue::from_bytes(value.as_bytes())
        else {
            continue;
        };
        out.insert_header((header_name, header_value));
    }
    if is_html {
        return match upstream.bytes().await {
            Ok(bytes) => {
                let html = String::from_utf8_lossy(&bytes);
                Ok(out.body(inject_edit_overlay(&html)))
            },
            Err(error) => {
                Ok(HttpResponse::BadGateway().body(format!("dev server proxy error: {error}")))
            },
        };
    }
    Ok(out.streaming(upstream.bytes_stream()))
}

/// The click-to-edit overlay (U7): a dormant in-page script the cockpit toggles
/// via `postMessage`. In edit mode it outlines the hovered element, posts the
/// selected element (tag / classes / text / `data-vibe-loc` / CSS selector) to
/// the cockpit on click, and supports inline text editing on double-click. It
/// is same-origin with the cockpit (both served under the API origin via this
/// proxy), so `postMessage(origin)` round-trips safely.
const EDIT_OVERLAY: &str = r#"<script data-vibe-edit-overlay>
(function () {
  if (window.__vibeEditInstalled) return;
  window.__vibeEditInstalled = true;
  var enabled = false, hovered = null, editingEl = null;
  var origin = window.location.origin;
  function post(msg) { try { msg.source = 'vibe-edit'; window.parent.postMessage(msg, origin); } catch (e) {} }
  function cssPath(el) {
    var parts = [];
    while (el && el.nodeType === 1 && parts.length < 6) {
      var sel = el.nodeName.toLowerCase();
      if (el.id) { parts.unshift(sel + '#' + el.id); break; }
      if (el.className && typeof el.className === 'string') {
        sel += el.className.trim().split(/\s+/).slice(0, 2).map(function (c) { return '.' + c; }).join('');
      }
      var p = el.parentNode;
      if (p) {
        var sibs = Array.prototype.filter.call(p.children, function (c) { return c.nodeName === el.nodeName; });
        if (sibs.length > 1) sel += ':nth-of-type(' + (Array.prototype.indexOf.call(sibs, el) + 1) + ')';
      }
      parts.unshift(sel);
      el = el.parentNode;
    }
    return parts.join(' > ');
  }
  function describe(el) {
    return {
      tag: el.nodeName.toLowerCase(),
      id: el.id || null,
      classes: (el.className && typeof el.className === 'string') ? el.className.trim() : '',
      text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 240),
      loc: el.getAttribute('data-vibe-loc') || null,
      selector: cssPath(el)
    };
  }
  function clearHover() { if (hovered) { hovered.style.outline = ''; hovered = null; } }
  function onMove(e) {
    if (!enabled) return;
    var el = e.target;
    if (el === hovered || el === editingEl) return;
    clearHover();
    if (el && el.nodeType === 1 && el !== document.body && el !== document.documentElement) {
      hovered = el; el.style.outline = '2px solid #c2502a';
    }
  }
  function onClick(e) {
    if (!enabled || editingEl) return;
    var el = e.target;
    if (!el || el.nodeType !== 1) return;
    e.preventDefault(); e.stopPropagation();
    post({ type: 'select', element: describe(el) });
  }
  function onDblClick(e) {
    if (!enabled) return;
    var el = e.target;
    if (!el || el.nodeType !== 1) return;
    e.preventDefault(); e.stopPropagation();
    clearHover();
    editingEl = el;
    var before = el.textContent || '';
    el.setAttribute('contenteditable', 'true');
    el.style.outline = '2px dashed #2a8fc2';
    el.focus();
    function finish(commit) {
      el.removeAttribute('contenteditable');
      el.style.outline = '';
      el.removeEventListener('blur', onBlur);
      el.removeEventListener('keydown', onKey);
      var after = el.textContent || '';
      editingEl = null;
      if (commit && after !== before) {
        post({ type: 'text-edit', element: describe(el),
               oldText: before.replace(/\s+/g, ' ').trim(),
               newText: after.replace(/\s+/g, ' ').trim() });
      } else if (!commit) { el.textContent = before; }
    }
    function onBlur() { finish(true); }
    function onKey(ev) {
      if (ev.key === 'Enter' && !ev.shiftKey) { ev.preventDefault(); el.blur(); }
      else if (ev.key === 'Escape') { ev.preventDefault(); finish(false); }
    }
    el.addEventListener('blur', onBlur);
    el.addEventListener('keydown', onKey);
  }
  window.addEventListener('message', function (e) {
    if (e.origin !== origin) return;
    var d = e.data || {};
    if (d.source !== 'vibe-host') return;
    if (d.type === 'edit-mode') {
      enabled = !!d.on;
      if (!enabled) clearHover();
      if (document.body) document.body.style.cursor = enabled ? 'crosshair' : '';
    }
  });
  document.addEventListener('mousemove', onMove, true);
  document.addEventListener('click', onClick, true);
  document.addEventListener('dblclick', onDblClick, true);
  post({ type: 'ready' });
})();
</script>"#;

fn inject_edit_overlay(html: &str) -> String {
    // Inject before </body> (fallback </html>, fallback append) so the overlay
    // installs after the app's own scripts.
    if let Some(idx) = html.rfind("</body>") {
        let mut out = String::with_capacity(html.len() + EDIT_OVERLAY.len());
        out.push_str(&html[..idx]);
        out.push_str(EDIT_OVERLAY);
        out.push_str(&html[idx..]);
        return out;
    }
    if let Some(idx) = html.rfind("</html>") {
        let mut out = String::with_capacity(html.len() + EDIT_OVERLAY.len());
        out.push_str(&html[..idx]);
        out.push_str(EDIT_OVERLAY);
        out.push_str(&html[idx..]);
        return out;
    }
    format!("{html}{EDIT_OVERLAY}")
}

fn payload_body(mut body: web::Payload) -> reqwest::Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
    actix::spawn(async move {
        while let Some(chunk) = body.next().await {
            let chunk = chunk
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()));
            if tx.send(chunk).await.is_err() {
                break;
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    reqwest::Body::wrap_stream(stream)
}

async fn ws_relay(
    req: HttpRequest,
    stream: web::Payload,
    port: u16,
    tail: String,
) -> Result<HttpResponse, ActixError> {
    if crate::websocket_handler::validate_origin(&req).is_err() {
        return Ok(HttpResponse::Forbidden().finish());
    }
    let qs = req.query_string().to_string();
    ws::WsResponseBuilder::new(PreviewWsRelay::new(port, tail, qs), &req, stream)
        .frame_size(16 * 1024 * 1024)
        .start()
}

fn is_hop_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HOP_HEADERS.contains(&lower.as_str())
}

struct PreviewWsRelay {
    port: u16,
    tail: String,
    qs: String,
    up_tx: Option<tokio::sync::mpsc::Sender<TMsg>>,
}

impl PreviewWsRelay {
    fn new(port: u16, tail: String, qs: String) -> Self {
        Self {
            port,
            tail,
            qs,
            up_tx: None,
        }
    }
}

struct FromUpstream(TMsg);

impl actix::Message for FromUpstream {
    type Result = ();
}

impl Handler<FromUpstream> for PreviewWsRelay {
    type Result = ();

    fn handle(&mut self, message: FromUpstream, ctx: &mut Self::Context) {
        match message.0 {
            TMsg::Text(text) => ctx.text(text),
            TMsg::Binary(bytes) => ctx.binary(bytes),
            TMsg::Ping(bytes) => ctx.ping(&bytes),
            TMsg::Pong(bytes) => ctx.pong(&bytes),
            TMsg::Close(_) => {
                ctx.close(None);
                ctx.stop();
            },
            _ => {},
        }
    }
}

impl Actor for PreviewWsRelay {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        let url = format!(
            "ws://127.0.0.1:{}/{}{}{}",
            self.port,
            self.tail,
            if self.qs.is_empty() { "" } else { "?" },
            self.qs
        );
        let port = self.port;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TMsg>(256);
        self.up_tx = Some(tx);
        let addr = ctx.address();
        actix::spawn(async move {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;

            let mut request = match url.into_client_request() {
                Ok(request) => request,
                Err(_) => {
                    addr.do_send(FromUpstream(TMsg::Close(None)));
                    return;
                },
            };
            let Ok(host) = format!("localhost:{port}").parse() else {
                addr.do_send(FromUpstream(TMsg::Close(None)));
                return;
            };
            request.headers_mut().insert("host", host);
            let (wsio, _) = match tokio_tungstenite::connect_async(request).await {
                Ok(value) => value,
                Err(_) => {
                    addr.do_send(FromUpstream(TMsg::Close(None)));
                    return;
                },
            };
            let (mut sink, mut up_stream) = wsio.split();
            loop {
                tokio::select! {
                    outbound = rx.recv() => match outbound {
                        Some(message) => {
                            if sink.send(message).await.is_err() {
                                break;
                            }
                        },
                        None => break,
                    },
                    inbound = up_stream.next() => match inbound {
                        Some(Ok(message)) => addr.do_send(FromUpstream(message)),
                        _ => {
                            addr.do_send(FromUpstream(TMsg::Close(None)));
                            break;
                        },
                    },
                }
            }
        });
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for PreviewWsRelay {
    fn handle(&mut self, item: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        let Some(tx) = self.up_tx.clone() else {
            return;
        };
        let frame = match item {
            Ok(ws::Message::Text(text)) => TMsg::Text(text.to_string()),
            Ok(ws::Message::Binary(bytes)) => TMsg::Binary(bytes.to_vec()),
            Ok(ws::Message::Ping(bytes)) => TMsg::Ping(bytes.to_vec()),
            Ok(ws::Message::Pong(bytes)) => TMsg::Pong(bytes.to_vec()),
            Ok(ws::Message::Close(_)) => {
                let _ = tx.try_send(TMsg::Close(None));
                ctx.stop();
                return;
            },
            Ok(ws::Message::Continuation(_)) => return,
            Ok(ws::Message::Nop) => return,
            Err(_) => {
                ctx.stop();
                return;
            },
        };
        let _ = tx.try_send(frame);
    }
}
