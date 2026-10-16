//! Site `notes` — guarded write: same session model as `portal`, a
//! CSRF-checked `POST /api/notes`, verify-by-read through `GET /api/notes`,
//! `DELETE` for cleanup, and a denylisted path that always refuses and is never
//! called by the driver.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    finish, html, json_keys, record,
    session::{self},
    shell, SiteState, CSRF_HEADER,
};

/// Always 403s: a write no recipe may ever replay.
pub const DENYLISTED_PATH: &str = "/api/account/delete";

pub fn initial_data() -> Value {
    json!({"next_id": 2, "notes": [{"id": 1, "text": "water plants"}]})
}

// DOM is built with createElement/textContent only; no HTML strings.
const SCRIPT: &str = r#"
function el(tag, text, attrs) {
  var node = document.createElement(tag);
  if (text != null) node.textContent = String(text);
  if (attrs) Object.keys(attrs).forEach(function (k) { node.setAttribute(k, attrs[k]); });
  return node;
}
function replace(container, nodes) { container.textContent = ''; nodes.forEach(function (n) { container.appendChild(n); }); }
async function loadNotes() {
  var out = document.getElementById('out');
  var res = await fetchJson('/api/notes');
  if (res.status === 401) {
    var p = el('p', 'Session expired — ', {class: 'muted', id: 'expired'});
    p.appendChild(el('a', 'log in again', {href: '/login'}));
    replace(out, [p]);
    return;
  }
  var list = el('ul', null, {id: 'notes'});
  (res.data.notes || []).forEach(function (n) {
    list.appendChild(el('li', n.text, {'data-id': n.id}));
  });
  replace(out, [list]);
}
(async function () {
  var path = location.pathname;
  var out = document.getElementById('out');
  if (path === '/login') {
    document.getElementById('login').addEventListener('submit', async function (ev) {
      ev.preventDefault();
      var res = await fetchJson('/api/session', {method: 'POST', body: JSON.stringify({
        username: document.getElementById('username').value,
        password: document.getElementById('password').value
      })});
      if (res.status === 200) { location.href = '/notes'; }
      else { replace(out, [el('p', 'Login failed.', {class: 'muted', id: 'login-error'})]); }
    });
    return;
  }
  if (path === '/notes') {
    document.getElementById('add').addEventListener('submit', async function (ev) {
      ev.preventDefault();
      var text = document.getElementById('text').value;
      var res = await fetchJson('/api/notes', {method: 'POST', body: JSON.stringify({text: text})});
      var status = document.getElementById('status');
      status.textContent = res.status === 201 ? 'Added "' + text + '".' : 'Could not add the note (' + res.status + ').';
      document.getElementById('text').value = '';
      await loadNotes();
    });
    await loadNotes();
  }
})();
"#;

fn login_body() -> &'static str {
    r#"<h1>Notes — sign in</h1>
<form id="login">
  <p><label>Username <input id="username" name="username" autocomplete="username"></label></p>
  <p><label>Password <input id="password" name="password" type="password" autocomplete="current-password"></label></p>
  <p><button type="submit">Sign in</button></p>
</form>
<div id="out"></div>"#
}

fn notes_body() -> &'static str {
    r#"<h1>Your notes</h1>
<form id="add"><input id="text" name="text" placeholder="New note"> <button type="submit">Add</button></form>
<p id="status" class="muted"></p>
<div id="out"><p class="muted">Loading notes…</p></div>"#
}

async fn login_page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let csrf = session::csrf_for_shell(&state.sessions, &req, &state.knobs(), state.name);
    finish(
        &state,
        seq,
        html(shell("Notes — sign in", Some(&csrf), login_body(), SCRIPT)),
    )
}

async fn notes_page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let csrf = session::csrf_for_shell(&state.sessions, &req, &state.knobs(), state.name);
    finish(
        &state,
        seq,
        html(shell("Notes", Some(&csrf), notes_body(), SCRIPT)),
    )
}

async fn root(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Ok(_) => HttpResponse::SeeOther()
            .insert_header(("Location", "/notes"))
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

fn notes_snapshot(state: &SiteState) -> Value {
    state.data.lock().expect("notes data poisoned")["notes"].clone()
}

async fn api_list(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Ok(_) => HttpResponse::Ok().json(json!({"notes": notes_snapshot(&state)})),
        Err(error) => super::portal::unauthorized(error),
    };
    finish(&state, seq, response)
}

#[derive(Debug, Deserialize)]
struct NoteBody {
    #[serde(default)]
    text: String,
}

fn csrf_ok(req: &HttpRequest, expected: &str) -> bool {
    req.headers()
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        == Some(expected)
}

async fn api_create(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let seq = record(&state, &req, json_keys(&body));
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Err(error) => super::portal::unauthorized(error),
        Ok(session) if !csrf_ok(&req, &session.csrf) => {
            HttpResponse::Forbidden().json(json!({"error": "csrf_token_invalid"}))
        },
        Ok(_) => {
            let parsed: NoteBody = serde_json::from_slice(&body).unwrap_or(NoteBody {
                text: String::new(),
            });
            let text = parsed.text.trim().to_owned();
            if text.is_empty() || text.len() > 200 {
                HttpResponse::BadRequest().json(json!({"error": "text_required"}))
            } else {
                let mut data = state.data.lock().expect("notes data poisoned");
                let id = data["next_id"].as_u64().unwrap_or(1);
                data["next_id"] = json!(id + 1);
                let note = json!({"id": id, "text": text});
                data["notes"]
                    .as_array_mut()
                    .expect("notes array")
                    .push(note.clone());
                HttpResponse::Created().json(note)
            }
        },
    };
    finish(&state, seq, response)
}

async fn api_delete(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    path: web::Path<u64>,
) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match state.sessions.resolve(&req, &state.knobs()) {
        Err(error) => super::portal::unauthorized(error),
        Ok(session) if !csrf_ok(&req, &session.csrf) => {
            HttpResponse::Forbidden().json(json!({"error": "csrf_token_invalid"}))
        },
        Ok(_) => {
            let mut data = state.data.lock().expect("notes data poisoned");
            let notes = data["notes"].as_array_mut().expect("notes array");
            let before = notes.len();
            notes.retain(|note| note["id"].as_u64() != Some(*path));
            if notes.len() < before {
                HttpResponse::NoContent().finish()
            } else {
                HttpResponse::NotFound().json(json!({"error": "not_found"}))
            }
        },
    };
    finish(&state, seq, response)
}

/// The denylisted path. Exists so the denylist contract has a real URL
/// template on a real origin; it refuses unconditionally.
async fn api_account_delete(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let seq = record(&state, &req, json_keys(&body));
    finish(
        &state,
        seq,
        HttpResponse::Forbidden().json(json!({"error": "denylisted_fixture_path"})),
    )
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(root))
        .route("/login", web::get().to(login_page))
        .route("/notes", web::get().to(notes_page))
        .route("/api/session", web::post().to(api_session))
        .route("/api/notes", web::get().to(api_list))
        .route("/api/notes", web::post().to(api_create))
        .route("/api/notes/{id}", web::delete().to(api_delete))
        .route(DENYLISTED_PATH, web::post().to(api_account_delete));
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[actix_web::test]
    async fn post_requires_session_and_csrf() {
        let state = SiteState::new("notes", initial_data());
        let app = app!(state);
        let anonymous = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/notes")
                .set_json(json!({"text": "x"}))
                .to_request(),
        )
        .await;
        assert_eq!(anonymous.status(), 401);
        let created = state.sessions.create("eval");
        let no_csrf = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/notes")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .set_json(json!({"text": "x"}))
                .to_request(),
        )
        .await;
        assert_eq!(no_csrf.status(), 403);
        let wrong_csrf = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/notes")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .insert_header(("x-csrf-token", "nope"))
                .set_json(json!({"text": "x"}))
                .to_request(),
        )
        .await;
        assert_eq!(wrong_csrf.status(), 403);
    }

    #[actix_web::test]
    async fn post_appends_list_reflects_and_delete_removes() {
        let state = SiteState::new("notes", initial_data());
        let app = app!(state);
        let created = state.sessions.create("eval");
        let add = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/notes")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .insert_header(("x-csrf-token", created.csrf.clone()))
                .set_json(json!({"text": "buy milk"}))
                .to_request(),
        )
        .await;
        assert_eq!(add.status(), 201);
        let note: Value = test::read_body_json(add).await;
        assert_eq!(note["id"], 2);
        let list: Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/notes")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .to_request(),
        )
        .await;
        assert_eq!(list["notes"].as_array().unwrap().len(), 2);
        assert_eq!(list["notes"][1]["text"], "buy milk");
        assert_eq!(
            state.log.all().last().unwrap().body_keys,
            Vec::<String>::new()
        );
        assert_eq!(state.log.all()[0].body_keys, vec!["text"]);

        let delete = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/api/notes/2")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .insert_header(("x-csrf-token", created.csrf.clone()))
                .to_request(),
        )
        .await;
        assert_eq!(delete.status(), 204);
        assert_eq!(notes_snapshot(&state).as_array().unwrap().len(), 1);
    }

    #[actix_web::test]
    async fn account_delete_is_always_refused() {
        let state = SiteState::new("notes", initial_data());
        let app = app!(state);
        let created = state.sessions.create("eval");
        let refused = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/account/delete")
                .cookie(Cookie::new(
                    state.sessions.cookie_name(),
                    created.id.clone(),
                ))
                .insert_header(("x-csrf-token", created.csrf.clone()))
                .set_json(json!({"confirm": true}))
                .to_request(),
        )
        .await;
        assert_eq!(refused.status(), 403);
    }
}
