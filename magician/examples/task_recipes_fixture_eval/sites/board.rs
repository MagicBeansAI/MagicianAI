//! Site `board` — GraphQL read (a POST with the query in the body) and the
//! drift knob: `schema_version = 2` renames the response field `score` →
//! `points`, which breaks a compiled JSON-path extractor while the page keeps
//! working (its JS reads whichever field exists).

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};

use super::{finish, html, json_keys, record, shell, SiteState};

pub struct Board {
    pub id: &'static str,
    pub title: &'static str,
    pub score: u32,
    pub members: u32,
}

pub const BOARDS: &[Board] = &[
    Board {
        id: "alpha",
        title: "Alpha launch",
        score: 7331,
        members: 14,
    },
    Board {
        id: "beta",
        title: "Beta retro",
        score: 2048,
        members: 6,
    },
    Board {
        id: "gamma",
        title: "Gamma backlog",
        score: 517,
        members: 3,
    },
];

pub fn board(id: &str) -> Option<&'static Board> {
    BOARDS.iter().find(|board| board.id == id)
}

/// Pull `board(id: "...")` out of a GraphQL query string. A tiny parser is
/// enough: the page always sends the same shape.
pub fn board_id_in_query(query: &str) -> Option<String> {
    let start = query.find("board(")?;
    let rest = &query[start + "board(".len()..];
    let id_start = rest.find("id:")?;
    let after = rest[id_start + 3..].trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_owned())
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
(async function () {
  var out = document.getElementById('out');
  var id = location.pathname.indexOf('/board/') === 0 ? location.pathname.slice('/board/'.length) : '';
  if (!id) {
    var list = el('ul');
    ['alpha', 'beta', 'gamma'].forEach(function (b) { var li = el('li'); li.appendChild(el('a', b, {href: '/board/' + b})); list.appendChild(li); });
    replace(out, [el('h1', 'Boards'), list]);
    return;
  }
  var query = '{ board(id: "' + id + '") { id title score members } }';
  var res = await fetchJson('/graphql', {method: 'POST', body: JSON.stringify({query: query, variables: {}})});
  var b = res.data && res.data.data && res.data.data.board;
  if (!b) { replace(out, [el('p', 'Board not found.', {class: 'muted'})]); return; }
  var score = (b.score != null) ? b.score : b.points;
  var meta = el('p');
  meta.appendChild(document.createTextNode('Score '));
  meta.appendChild(el('strong', score, {id: 'score'}));
  meta.appendChild(document.createTextNode(' · ' + b.members + ' members'));
  var back = el('p'); back.appendChild(el('a', 'All boards', {href: '/'}));
  replace(out, [el('h1', b.title, {id: 'title'}), meta, back]);
})();
"#;

async fn page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    finish(
        &state,
        seq,
        html(shell(
            "Boards",
            None,
            r#"<div id="out"><p class="muted">Loading…</p></div>"#,
            SCRIPT,
        )),
    )
}

async fn graphql(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let seq = record(&state, &req, json_keys(&body));
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let query = parsed["query"].as_str().unwrap_or_default();
    let version = state.knobs().schema_version;
    let response = match board_id_in_query(query).as_deref().and_then(board) {
        Some(found) => {
            let mut node = json!({"id": found.id, "title": found.title, "members": found.members});
            let score_key = if version >= 2 { "points" } else { "score" };
            node[score_key] = json!(found.score);
            HttpResponse::Ok().json(json!({"data": {"board": node}}))
        },
        None if query.contains("board(") => {
            HttpResponse::Ok().json(json!({"data": {"board": null}}))
        },
        None => {
            HttpResponse::BadRequest().json(json!({"errors": [{"message": "unsupported query"}]}))
        },
    };
    finish(&state, seq, response)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(page))
        .route("/board/{id}", web::get().to(page))
        .route("/graphql", web::post().to(graphql));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::Knobs;
    use actix_web::{test, App};

    fn query_for(id: &str) -> Value {
        json!({"query": format!("{{ board(id: \"{id}\") {{ id title score members }} }}"), "variables": {}})
    }

    #[actix_web::test]
    async fn board_id_parser_handles_the_page_shape() {
        assert_eq!(
            board_id_in_query(r#"{ board(id: "alpha") { title score } }"#).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            board_id_in_query(r#"query Q { board(id:'beta'){id} }"#).as_deref(),
            Some("beta")
        );
        assert_eq!(board_id_in_query("{ boards { id } }"), None);
    }

    #[actix_web::test]
    async fn graphql_returns_score_v1_and_points_v2() {
        let state = SiteState::new("board", json!({}));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&state)))
                .configure(configure),
        )
        .await;
        let v1: Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/graphql")
                .set_json(query_for("alpha"))
                .to_request(),
        )
        .await;
        assert_eq!(v1["data"]["board"]["score"], 7331);
        assert!(v1["data"]["board"].get("points").is_none());

        *state.knobs.lock().unwrap() = Knobs {
            schema_version: 2,
            ..Knobs::default()
        };
        let v2: Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/graphql")
                .set_json(query_for("alpha"))
                .to_request(),
        )
        .await;
        assert_eq!(v2["data"]["board"]["points"], 7331);
        assert!(v2["data"]["board"].get("score").is_none());

        let unknown: Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/graphql")
                .set_json(query_for("nope"))
                .to_request(),
        )
        .await;
        assert!(unknown["data"]["board"].is_null());
        assert_eq!(state.log.all()[0].body_keys, vec!["query", "variables"]);
    }
}
