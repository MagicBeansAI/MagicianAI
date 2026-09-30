//! Site `portal` — session-gated read + auth heal: login form → HttpOnly
//! cookie session + CSRF meta → authenticated XHR. The `session_ttl_secs`
//! knob turns every session into a 401 so the warm run must heal auth.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    finish, html, json_keys, record,
    session::{self, SessionError},
    shell, SiteState,
};

/// Ground truth: orders for the fixture user, most recent first.
pub fn orders() -> Value {
    json!([
        {"id": "ORD-8841", "status": "shipped", "total": 1284.50, "currency": "USD", "placed_at": "2026-09-10", "items": 3},
        {"id": "ORD-8790", "status": "delivered", "total": 312.00, "currency": "USD", "placed_at": "2026-09-02", "items": 1},
        {"id": "ORD-8611", "status": "delivered", "total": 79.99, "currency": "USD", "placed_at": "2026-08-21", "items": 2}
    ])
}

pub const MOST_RECENT_TOTAL: &str = "1284.50";

// DOM is built with createElement/textContent only; no HTML strings.
const SCRIPT: &str = r#"
function el(tag, text, attrs) {
  var node = document.createElement(tag);
  if (text != null) node.textContent = String(text);
  if (attrs) Object.keys(attrs).forEach(function (k) { node.setAttribute(k, attrs[k]); });
  return node;
}
function replace(container, nodes) { container.textContent = ''; nodes.forEach(function (n) { container.appendChild(n); }); }
(async function () {
  var path = location.pathname;
  var out = document.getElementById('out');
  if (path === '/login') {
    var form = document.getElementById('login');
    form.addEventListener('submit', async function (ev) {
      ev.preventDefault();
      var res = await fetchJson('/api/session', {method: 'POST', body: JSON.stringify({
        username: document.getElementById('username').value,
        password: document.getElementById('password').value
      })});
      if (res.status === 200) { location.href = '/orders'; }
      else { replace(out, [el('p', 'Login failed.', {class: 'muted', id: 'login-error'})]); }
    });
    return;
  }
  if (path === '/orders') {
    var res = await fetchJson('/api/me/orders');
    if (res.status === 401) {
      var p = el('p', 'Session expired — ', {class: 'muted', id: 'expired'});
      p.appendChild(el('a', 'log in again', {href: '/login'}));
      replace(out, [p]);
      return;
    }
    var table = el('table', null, {id: 'orders'});
    var head = el('tr');
    ['Order', 'Placed', 'Status', 'Total'].forEach(function (h) { head.appendChild(el('th', h)); });
    table.appendChild(head);
    (res.data.orders || []).forEach(function (o) {
      var tr = el('tr', null, {'data-order': o.id});
      tr.appendChild(el('td', o.id));
      tr.appendChild(el('td', o.placed_at));
      tr.appendChild(el('td', o.status));
      tr.appendChild(el('td', o.currency + ' ' + o.total.toFixed(2), {class: 'total'}));
      table.appendChild(tr);
    });
    replace(out, [el('h1', 'Your orders'), el('p', 'Most recent first.', {class: 'muted'}), table]);
  }
})();
"#;

fn login_body() -> &'static str {
    r#"<h1>Sign in</h1>
<form id="login">
  <p><label>Username <input id="username" name="username" autocomplete="username"></label></p>
  <p><label>Password <input id="password" name="password" type="password" autocomplete="current-password"></label></p>
  <p><button type="submit">Sign in</button></p>
</form>
<div id="out"></div>"#
}

fn orders_body() -> &'static str {
    r#"<div id="out"><p class="muted">Loading orders…</p></div>"#
}

async fn login_page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let csrf = session::csrf_for_shell(&state.sessions, &req, &state.knobs(), state.name);
    finish(
        &state,
        seq,
        html(shell("Portal — sign in", Some(&csrf), login_body(), SCRIPT)),
    )
}

async fn orders_page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let csrf = session::csrf_for_shell(&state.sessions, &req, &state.knobs(), state.name);
    finish(
        &state,
        seq,
        html(shell("Portal — orders", Some(&csrf), orders_body(), SCRIPT)),
    )
}

async fn root(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Ok(_) => HttpResponse::SeeOther()
            .insert_header(("Location", "/orders"))
            .finish(),
        Err(_) => HttpResponse::SeeOther()
            .insert_header(("Location", "/login"))
            .finish(),
    };
    finish(&state, seq, response)
}

#[derive(Debug, Deserialize)]
struct LoginBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn api_session(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let seq = record(&state, &req, json_keys(&body));
    let parsed: LoginBody = serde_json::from_slice(&body).unwrap_or(LoginBody {
        username: String::new(),
        password: String::new(),
    });
    let response = if session::credentials_ok(&parsed.username, &parsed.password) {
        let created = state.sessions.create(&parsed.username);
        HttpResponse::Ok()
            .insert_header(("Set-Cookie", state.sessions.set_cookie_value(&created)))
            .json(json!({"ok": true, "username": created.username, "csrf_token": created.csrf}))
    } else {
        HttpResponse::Unauthorized().json(json!({"error": "invalid_credentials"}))
    };
    finish(&state, seq, response)
}

pub fn unauthorized(error: SessionError) -> HttpResponse {
    let reason = match error {
        SessionError::Missing => "session_missing",
        SessionError::Expired => "session_expired",
    };
    HttpResponse::Unauthorized()
        .insert_header(("WWW-Authenticate", "Cookie realm=\"portal\""))
        .json(json!({"error": reason}))
}

async fn api_orders(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Ok(session) => {
            HttpResponse::Ok().json(json!({"username": session.username, "orders": orders()}))
        },
        Err(error) => unauthorized(error),
    };
    finish(&state, seq, response)
}

async fn api_logout(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    finish(
        &state,
        seq,
        HttpResponse::NoContent()
            .insert_header(("Set-Cookie", state.sessions.clear_cookie_value()))
            .finish(),
    )
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(root))
        .route("/login", web::get().to(login_page))
        .route("/orders", web::get().to(orders_page))
        .route("/api/session", web::post().to(api_session))
        .route("/api/session", web::delete().to(api_logout))
        .route("/api/me/orders", web::get().to(api_orders));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::Knobs;
    use actix_web::{cookie::Cookie, test, App};

    macro_rules! app {
        ($state:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new(Arc::clone(&$state)))
                    .configure(configure),
            )
            .await
        };
    }

    fn sid_from(response: &actix_web::dev::ServiceResponse) -> String {
        let header = response
            .headers()
            .get("set-cookie")
            .expect("set-cookie")
            .to_str()
            .unwrap();
        assert!(header.contains("HttpOnly"), "{header}");
        header
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .expect("name=value")
            .1
            .to_owned()
    }

    #[actix_web::test]
    async fn login_sets_httponly_cookie_and_rejects_bad_credentials() {
        let state = SiteState::new("portal", json!({}));
        let app = app!(state);
        let ok = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/session")
                .set_json(json!({"username": "eval", "password": "eval-pass"}))
                .to_request(),
        )
        .await;
        assert_eq!(ok.status(), 200);
        let sid = sid_from(&ok);
        assert!(state.sessions.get(&sid).is_some());
        let bad = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/session")
                .set_json(json!({"username": "eval", "password": "nope"}))
                .to_request(),
        )
        .await;
        assert_eq!(bad.status(), 401);
        let logged = state.log.all();
        assert_eq!(
            logged[0].body_keys,
            vec!["password", "username"],
            "keys only, never values"
        );
    }

    #[actix_web::test]
    async fn orders_require_cookie_and_expire_by_knob() {
        let state = SiteState::new("portal", json!({}));
        let app = app!(state);
        let anonymous = test::call_service(
            &app,
            test::TestRequest::get().uri("/api/me/orders").to_request(),
        )
        .await;
        assert_eq!(anonymous.status(), 401);

        let created = state.sessions.create("eval");
        let authed = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/me/orders")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .to_request(),
        )
        .await;
        assert_eq!(authed.status(), 200);
        let body: Value = test::read_body_json(authed).await;
        assert_eq!(body["orders"][0]["id"], "ORD-8841");
        assert_eq!(body["orders"][0]["total"], 1284.50);

        *state.knobs.lock().unwrap() = Knobs {
            session_ttl_secs: Some(0),
            ..Knobs::default()
        };
        let expired = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/me/orders")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .to_request(),
        )
        .await;
        assert_eq!(expired.status(), 401);
        let body: Value = test::read_body_json(expired).await;
        assert_eq!(body["error"], "session_expired");
    }

    #[actix_web::test]
    async fn shell_carries_session_csrf_when_logged_in() {
        let state = SiteState::new("portal", json!({}));
        let app = app!(state);
        let anon =
            test::call_service(&app, test::TestRequest::get().uri("/login").to_request()).await;
        let anon_html = String::from_utf8(test::read_body(anon).await.to_vec()).unwrap();
        assert!(anon_html.contains(r#"name="csrf-token" content="anon-portal""#));

        let created = state.sessions.create("eval");
        let page = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/orders")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .to_request(),
        )
        .await;
        let html = String::from_utf8(test::read_body(page).await.to_vec()).unwrap();
        assert!(html.contains(&format!(r#"content="{}""#, created.csrf)));
    }
}
