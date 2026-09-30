//! Site `catalog` — read flows: search by query param (XHR JSON list),
//! list→detail dependency chain (the author lives only in the detail
//! response), and a server-rendered `/about` whose answer exists only in the
//! HTML Document.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::json;

use super::{finish, html, record, shell, SiteState};

pub const FOUNDED_YEAR: u32 = 1987;

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: u32,
    pub title: &'static str,
    pub author: &'static str,
    pub points: u32,
    pub comments: u32,
    pub url: &'static str,
    #[serde(skip)]
    pub tags: &'static [&'static str],
}

/// Fixed table. The three anchor rows carry distinctive values so answer
/// containment is unambiguous; filler rows never outrank them for the anchor
/// queries.
pub const ITEMS: &[Item] = &[
    Item {
        id: 7,
        title: "Rust 2031 roadmap",
        author: "Ingrid Solvang",
        points: 3119,
        comments: 412,
        url: "https://example.test/rust-2031",
        tags: &["rust", "roadmap"],
    },
    Item {
        id: 12,
        title: "Python 4 is not happening",
        author: "Tomasz Weir",
        points: 2711,
        comments: 388,
        url: "https://example.test/python-4",
        tags: &["python"],
    },
    Item {
        id: 19,
        title: "Go generics, three years on",
        author: "Ama Okafor",
        points: 1553,
        comments: 97,
        url: "https://example.test/go-generics",
        tags: &["go", "generics"],
    },
    Item {
        id: 3,
        title: "Why we moved off Kubernetes",
        author: "Dana Feld",
        points: 1207,
        comments: 301,
        url: "https://example.test/off-k8s",
        tags: &["infra"],
    },
    Item {
        id: 5,
        title: "A tour of WASI 0.3",
        author: "Ravi Menon",
        points: 991,
        comments: 74,
        url: "https://example.test/wasi",
        tags: &["wasm"],
    },
    Item {
        id: 8,
        title: "Rust async book, second edition",
        author: "Lena Ortiz",
        points: 877,
        comments: 120,
        url: "https://example.test/async-book",
        tags: &["rust", "async"],
    },
    Item {
        id: 9,
        title: "SQLite as the only database",
        author: "Kofi Mensah",
        points: 843,
        comments: 210,
        url: "https://example.test/sqlite",
        tags: &["db"],
    },
    Item {
        id: 11,
        title: "The end of the monorepo",
        author: "Pia Lund",
        points: 799,
        comments: 156,
        url: "https://example.test/monorepo",
        tags: &["infra"],
    },
    Item {
        id: 14,
        title: "Python packaging finally converges",
        author: "Ola Nwosu",
        points: 731,
        comments: 92,
        url: "https://example.test/packaging",
        tags: &["python"],
    },
    Item {
        id: 16,
        title: "A CRDT in 200 lines",
        author: "Mira Sato",
        points: 688,
        comments: 61,
        url: "https://example.test/crdt",
        tags: &["distributed"],
    },
    Item {
        id: 21,
        title: "Go 1.30 release notes",
        author: "Niamh Byrne",
        points: 640,
        comments: 45,
        url: "https://example.test/go-130",
        tags: &["go"],
    },
    Item {
        id: 22,
        title: "Postgres 20 parallel vacuum",
        author: "Jonas Ek",
        points: 612,
        comments: 38,
        url: "https://example.test/pg20",
        tags: &["db"],
    },
    Item {
        id: 24,
        title: "What I learned writing a Rust linker",
        author: "Sol Adeyemi",
        points: 577,
        comments: 83,
        url: "https://example.test/linker",
        tags: &["rust"],
    },
    Item {
        id: 26,
        title: "Observability without vendors",
        author: "Hana Kim",
        points: 540,
        comments: 77,
        url: "https://example.test/o11y",
        tags: &["infra"],
    },
    Item {
        id: 28,
        title: "The Python GIL is gone",
        author: "Bo Larsen",
        points: 498,
        comments: 190,
        url: "https://example.test/nogil",
        tags: &["python"],
    },
    Item {
        id: 30,
        title: "Zig 1.0",
        author: "Elif Demir",
        points: 471,
        comments: 133,
        url: "https://example.test/zig",
        tags: &["zig"],
    },
    Item {
        id: 31,
        title: "Go modules, ten years later",
        author: "Tariq Haddad",
        points: 433,
        comments: 29,
        url: "https://example.test/go-mod",
        tags: &["go"],
    },
    Item {
        id: 33,
        title: "Terminal UIs are back",
        author: "Ines Rocha",
        points: 402,
        comments: 66,
        url: "https://example.test/tui",
        tags: &["ui"],
    },
    Item {
        id: 35,
        title: "Rust in the Linux scheduler",
        author: "Yuki Tanaka",
        points: 388,
        comments: 240,
        url: "https://example.test/sched",
        tags: &["rust", "linux"],
    },
    Item {
        id: 37,
        title: "An honest look at serverless bills",
        author: "Femi Ade",
        points: 351,
        comments: 88,
        url: "https://example.test/serverless",
        tags: &["infra"],
    },
    Item {
        id: 38,
        title: "Python type checkers compared",
        author: "Greta Holm",
        points: 322,
        comments: 54,
        url: "https://example.test/typecheck",
        tags: &["python"],
    },
    Item {
        id: 40,
        title: "Writing a JIT in a weekend",
        author: "Omar Said",
        points: 298,
        comments: 41,
        url: "https://example.test/jit",
        tags: &["compilers"],
    },
    Item {
        id: 41,
        title: "Go's new iterator design",
        author: "Wen Zhao",
        points: 276,
        comments: 33,
        url: "https://example.test/go-iter",
        tags: &["go"],
    },
    Item {
        id: 43,
        title: "Cheap object storage tricks",
        author: "Lior Ben",
        points: 251,
        comments: 19,
        url: "https://example.test/objstore",
        tags: &["infra"],
    },
    Item {
        id: 44,
        title: "Rust embedded on RISC-V",
        author: "Noor Aziz",
        points: 233,
        comments: 27,
        url: "https://example.test/riscv",
        tags: &["rust", "embedded"],
    },
    Item {
        id: 46,
        title: "Deterministic builds, for real",
        author: "Sven Berg",
        points: 219,
        comments: 22,
        url: "https://example.test/repro",
        tags: &["build"],
    },
    Item {
        id: 47,
        title: "Python notebooks for ops",
        author: "Aya Nakamura",
        points: 197,
        comments: 14,
        url: "https://example.test/nb-ops",
        tags: &["python"],
    },
    Item {
        id: 49,
        title: "The last word on tabs vs spaces",
        author: "Rae Quinn",
        points: 181,
        comments: 402,
        url: "https://example.test/tabs",
        tags: &["fun"],
    },
    Item {
        id: 50,
        title: "Go on tiny devices",
        author: "Ibrahim Diallo",
        points: 164,
        comments: 12,
        url: "https://example.test/tinygo",
        tags: &["go"],
    },
    Item {
        id: 52,
        title: "Rust for game jams",
        author: "Vera Novak",
        points: 149,
        comments: 31,
        url: "https://example.test/gamejam",
        tags: &["rust", "games"],
    },
];

pub fn search(query: &str) -> Vec<&'static Item> {
    let needle = query.trim().to_ascii_lowercase();
    let mut hits: Vec<&Item> = ITEMS
        .iter()
        .filter(|item| {
            !needle.is_empty()
                && (item.title.to_ascii_lowercase().contains(&needle)
                    || item.tags.iter().any(|tag| *tag == needle))
        })
        .collect();
    hits.sort_by(|a, b| b.points.cmp(&a.points));
    hits
}

pub fn item(id: u32) -> Option<&'static Item> {
    ITEMS.iter().find(|item| item.id == id)
}

/// Ground truth for the driver: the top hit for a query.
pub fn top_hit(query: &str) -> Option<&'static Item> {
    search(query).into_iter().next()
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
  var path = location.pathname;
  var out = document.getElementById('out');
  if (path.indexOf('/item/') === 0) {
    var id = path.slice('/item/'.length);
    var res = await fetchJson('/api/items/' + encodeURIComponent(id));
    if (res.status !== 200) { replace(out, [el('p', 'Item not found.', {class: 'muted'})]); return; }
    var it = res.data;
    var meta = el('p');
    meta.appendChild(document.createTextNode('By '));
    meta.appendChild(el('span', it.author, {id: 'author'}));
    meta.appendChild(document.createTextNode(' · '));
    meta.appendChild(el('span', it.points, {id: 'points'}));
    meta.appendChild(document.createTextNode(' points · ' + it.comments + ' comments'));
    var link = el('p'); link.appendChild(el('a', it.url, {href: it.url}));
    var back = el('p'); back.appendChild(el('a', 'Back to search', {href: '/'}));
    replace(out, [el('h1', it.title), meta, link, back]);
    return;
  }
  var params = new URLSearchParams(location.search);
  var q = params.get('q') || '';
  document.getElementById('q').value = q;
  if (!q) { replace(out, [el('p', 'Type a query and press Search.', {class: 'muted'})]); return; }
  var res = await fetchJson('/api/search?q=' + encodeURIComponent(q));
  var hits = (res.data && res.data.hits) || [];
  if (!hits.length) { replace(out, [el('p', 'No results for ' + q + '.', {class: 'muted'})]); return; }
  var list = el('ol', null, {id: 'hits'});
  hits.forEach(function (h) {
    var li = el('li');
    li.appendChild(el('a', h.title, {href: '/item/' + h.id}));
    li.appendChild(document.createTextNode(' '));
    li.appendChild(el('span', h.points + ' points · ' + h.comments + ' comments', {class: 'muted'}));
    list.appendChild(li);
  });
  replace(out, [list]);
})();
"#;

fn shell_page(title: &str) -> String {
    shell(
        title,
        None,
        r#"<form method="get" action="/"><input id="q" name="q" placeholder="Search the catalog"> <button type="submit">Search</button></form>
<div id="out"></div>"#,
        SCRIPT,
    )
}

async fn page(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    finish(&state, seq, html(shell_page("Catalog")))
}

async fn about(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let body = format!(
        r#"<h1>About the catalog</h1>
<p>The catalog was founded in <strong id="founded">{FOUNDED_YEAR}</strong> as a reading list for systems programmers.</p>
<p>It has no editorial staff; ranking is by points.</p>
<p><a href="/">Back to search</a></p>"#
    );
    finish(
        &state,
        seq,
        html(shell("About the catalog", None, &body, "")),
    )
}

async fn api_search(state: web::Data<Arc<SiteState>>, req: HttpRequest) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let query =
        web::Query::<std::collections::HashMap<String, String>>::from_query(req.query_string())
            .map(|q| q.get("q").cloned().unwrap_or_default())
            .unwrap_or_default();
    let hits: Vec<_> = search(&query)
        .into_iter()
        .map(|item| json!({"id": item.id, "title": item.title, "points": item.points, "comments": item.comments}))
        .collect();
    finish(
        &state,
        seq,
        HttpResponse::Ok().json(json!({"query": query, "count": hits.len(), "hits": hits})),
    )
}

async fn api_item(
    state: web::Data<Arc<SiteState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let seq = record(&state, &req, Vec::new());
    let response = match path.parse::<u32>().ok().and_then(item) {
        Some(item) => HttpResponse::Ok().json(item),
        None => HttpResponse::NotFound().json(json!({"error": "not_found"})),
    };
    finish(&state, seq, response)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(page))
        .route("/item/{id}", web::get().to(page))
        .route("/about", web::get().to(about))
        .route("/api/search", web::get().to(api_search))
        .route("/api/items/{id}", web::get().to(api_item));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::{mount_common, RequestKind};
    use actix_web::{test, App};

    fn app_state() -> Arc<SiteState> {
        SiteState::new("catalog", json!({}))
    }

    #[actix_web::test]
    async fn search_returns_ranked_hits_without_author() {
        let state = app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&state)))
                .configure(configure),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/api/search?q=rust")
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits[0]["id"], 7);
        assert_eq!(hits[0]["points"], 3119);
        assert!(
            hits[0].get("author").is_none(),
            "author must only come from the detail call"
        );
        assert!(hits.len() >= 5);
        assert_eq!(top_hit("python").unwrap().id, 12);
        assert_eq!(top_hit("go").unwrap().id, 19);
        assert!(top_hit("zzzz").is_none());
    }

    #[actix_web::test]
    async fn item_detail_carries_author() {
        let state = app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&state)))
                .configure(configure),
        )
        .await;
        let req = test::TestRequest::get().uri("/api/items/7").to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(body["author"], "Ingrid Solvang");
        let missing = test::TestRequest::get().uri("/api/items/999").to_request();
        assert_eq!(test::call_service(&app, missing).await.status(), 404);
    }

    #[actix_web::test]
    async fn about_is_server_rendered_with_founding_year() {
        let state = app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&state)))
                .configure(configure),
        )
        .await;
        let req = test::TestRequest::get().uri("/about").to_request();
        let response = test::call_service(&app, req).await;
        assert!(response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();
        assert!(body.contains("1987"));
    }

    #[actix_web::test]
    async fn beacons_and_pages_are_logged_with_kinds() {
        let state = app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&state)))
                .configure(mount_common)
                .configure(configure),
        )
        .await;
        for (method, uri, nonce) in [
            ("GET", "/?q=rust", None),
            ("GET", "/px.gif?e=view", None),
            ("GET", "/api/search?q=rust", Some("n1")),
            ("POST", "/t/collect", None),
        ] {
            let mut req = match method {
                "GET" => test::TestRequest::get(),
                _ => test::TestRequest::post(),
            }
            .uri(uri);
            if let Some(nonce) = nonce {
                req = req.insert_header(("x-page-nonce", nonce));
            }
            let response = test::call_service(&app, req.to_request()).await;
            assert!(
                response.status().is_success(),
                "{method} {uri} -> {}",
                response.status()
            );
        }
        let log = state.log.all();
        let kinds: Vec<_> = log.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RequestKind::Document,
                RequestKind::Beacon,
                RequestKind::Api,
                RequestKind::Beacon
            ]
        );
        assert_eq!(log[2].page_nonce.as_deref(), Some("n1"));
        assert_eq!(log[2].query, "q=rust");
        assert_eq!(log[2].status, 200);
    }
}
