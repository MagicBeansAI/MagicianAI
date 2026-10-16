//! REST API for the agent-update operator feed.
//!
//! Single endpoint: `GET /api/magician/v2/updates`. Returns a time-ordered
//! slice of `AgentUpdate` events from the per-workspace JSONL log written by
//! [`magician::magician_v2::agents::agent_update_journal`].
//!
//! Scope: the verified bearer context engraved by authentication middleware.
//!
//! Query parameters:
//! - `workspace` (ignored compatibility field; bearer scope is authoritative)
//! - `since` (optional, unix millis) — return events with `ts > since`
//! - `limit` (optional, default 200, max 1000) — slice size
//! - `agent` (optional) — filter by `agent_id`
//! - `kind` (optional) — filter by variant tag
//! - `thread` (optional) — filter by `thread_id`

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use magician::magician_v2::agents::agent_update_journal::{
    tail_matching, AgentUpdateJournalConfig,
};

use crate::scope::resolve_required_scope;

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct ListAgentUpdatesQuery {
    pub workspace: Option<String>,
    pub since: Option<i64>,
    pub limit: Option<usize>,
    pub agent: Option<String>,
    pub kind: Option<String>,
    pub thread: Option<String>,
}

/// Reject scope identifiers containing path separators, relative components
/// (`.` / `..`), control characters, or a leading `.` (dotfiles). The journal
/// path is built via `Path::join(principal).join(workspace)`; without this
/// guard, `principal=../..` would escape the data_root and read any
/// `updates.jsonl` on the filesystem.
fn is_unsafe_scope_id(id: &str) -> bool {
    id.is_empty()
        || id == "."
        || id == ".."
        || id.starts_with('.')
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.chars().any(|c| c.is_control())
}

/// `GET /api/magician/v2/updates`
pub async fn list_agent_updates_handler(
    config: web::Data<AgentUpdateJournalConfig>,
    req: HttpRequest,
    query: web::Query<ListAgentUpdatesQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    if is_unsafe_scope_id(&principal) {
        return Ok(invalid_scope_response("principal"));
    }
    if is_unsafe_scope_id(&workspace) {
        return Ok(invalid_scope_response("workspace"));
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);

    let agent_filter = query.agent.clone().filter(|s| !s.is_empty());
    let kind_filter = query.kind.clone().filter(|s| !s.is_empty());
    let thread_filter = query.thread.clone().filter(|s| !s.is_empty());

    // `tail_matching` is fully synchronous: it reads the whole journal with a
    // blocking `read_to_string`, then parses, filters and sorts every line before
    // truncating to `limit`. Rotation bounds the file, but "bounded" is still
    // hundreds of KB and thousands of `serde_json` parses — not something to do on
    // an Actix worker while it could be serving other requests.
    let journal_config = config.into_inner();
    let scoped_principal = principal.clone();
    let scoped_workspace = workspace.clone();
    let since = query.since;
    let read = tokio::task::spawn_blocking(move || {
        tail_matching(
            &journal_config,
            &scoped_principal,
            &scoped_workspace,
            since,
            limit,
            |event| {
                event_matches_filters(
                    event,
                    agent_filter.as_deref(),
                    kind_filter.as_deref(),
                    thread_filter.as_deref(),
                )
            },
        )
    })
    .await;

    let events = match read {
        Ok(Ok(events)) => events,
        Ok(Err(err)) => {
            tracing::error!(
                error = %err,
                principal = %principal,
                workspace = %workspace,
                "failed to read agent_updates journal"
            );
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "journal_read_failed",
                "message": err.to_string()
            })));
        },
        Err(err) => {
            tracing::error!(
                error = %err,
                principal = %principal,
                workspace = %workspace,
                "agent_updates journal read task failed"
            );
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "journal_read_failed",
                "message": err.to_string()
            })));
        },
    };

    Ok(HttpResponse::Ok().json(json!({
        "count": events.len(),
        "events": events,
    })))
}

fn event_matches_filters(
    event: &serde_json::Value,
    agent: Option<&str>,
    kind: Option<&str>,
    thread: Option<&str>,
) -> bool {
    if let Some(agent) = agent {
        if event.get("agent_id").and_then(|v| v.as_str()) != Some(agent) {
            return false;
        }
    }
    if let Some(kind) = kind {
        if event.get("kind").and_then(|v| v.as_str()) != Some(kind) {
            return false;
        }
    }
    if let Some(thread) = thread {
        if event.get("thread_id").and_then(|v| v.as_str()) != Some(thread) {
            return false;
        }
    }
    true
}

fn invalid_scope_response(param: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(json!({
        "error": "invalid_scope_identifier",
        "param": param,
        "message": "scope identifiers must not contain path separators, relative components, or control characters"
    }))
}

#[cfg(test)]
mod tests {

    use super::list_agent_updates_handler;
    use actix_web::{http::StatusCode, test, web, App};
    use magician::magician_v2::agents::agent_update_journal::AgentUpdateJournalConfig;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    fn make_response(events: Vec<Value>) -> Value {
        json!({ "count": events.len(), "events": events })
    }

    fn seed_events(cfg: &AgentUpdateJournalConfig, principal: &str, workspace: &str) {
        use magician::magician_v2::agents::agent_update_journal::write_event;
        use magician::magician_v2::realtime_events::AgentEventEnvelope;
        for (idx, ts) in [10_i64, 20, 30, 40, 50].iter().enumerate() {
            let thread_id = if idx % 2 == 0 { "th_a" } else { "th_b" };
            let payload = json!({
                "id": format!("e{}", ts),
                "ts": ts,
                "agent_id": if idx % 2 == 0 { "cfo" } else { "analyst" },
                "kind": if idx == 0 { "cycle_started" } else { "cycle_completed" },
                "workspace_id": workspace,
                "thread_id": thread_id,
            });
            let env = AgentEventEnvelope::new_scoped(
                "agent.update",
                "any",
                principal,
                workspace,
                payload,
            );
            write_event(cfg, &env).unwrap();
        }
    }

    #[actix_web::test]
    async fn returns_400_when_principal_missing() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn returns_events_for_scope_in_reverse_chronological_order() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        seed_events(&cfg, "alpha", "ws_a");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "ws_a"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(body["count"], 5);
        let events = body["events"].as_array().unwrap();
        let timestamps: Vec<i64> = events.iter().map(|v| v["ts"].as_i64().unwrap()).collect();
        assert_eq!(timestamps, vec![50, 40, 30, 20, 10]);
    }

    #[actix_web::test]
    async fn since_and_limit_filter_results() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        seed_events(&cfg, "alpha", "ws_a");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a&since=15&limit=2")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "ws_a"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        let timestamps: Vec<i64> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["ts"].as_i64().unwrap())
            .collect();
        assert_eq!(timestamps, vec![50, 40]);
    }

    #[actix_web::test]
    async fn agent_filter_keeps_only_matching_agent() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        seed_events(&cfg, "alpha", "ws_a");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a&agent=cfo&limit=2")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "ws_a"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        let agents: Vec<&str> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["agent_id"].as_str().unwrap())
            .collect();
        for a in &agents {
            assert_eq!(*a, "cfo");
        }
        assert_eq!(agents.len(), 2);
        let timestamps: Vec<i64> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["ts"].as_i64().unwrap())
            .collect();
        assert_eq!(timestamps, vec![50, 30]);
    }

    #[actix_web::test]
    async fn kind_filter_keeps_only_matching_kind() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        seed_events(&cfg, "alpha", "ws_a");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a&kind=cycle_completed")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "ws_a"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        let kinds: Vec<&str> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["kind"].as_str().unwrap())
            .collect();
        for k in &kinds {
            assert_eq!(*k, "cycle_completed");
        }
        assert!(!kinds.is_empty());
    }

    #[actix_web::test]
    async fn thread_filter_keeps_only_matching_thread() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        seed_events(&cfg, "alpha", "ws_a");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=ws_a&thread=th_a")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "ws_a"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        let threads: Vec<&str> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["thread_id"].as_str().unwrap())
            .collect();
        assert!(!threads.is_empty());
        for t in &threads {
            assert_eq!(*t, "th_a");
        }
    }

    #[actix_web::test]
    async fn rejects_path_traversal_in_principal() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        for bad in ["..", "../../etc", "foo/bar", "a\\b", ".hidden"] {
            let req = test::TestRequest::get()
                .uri("/updates?workspace=ws_a")
                .insert_header(("X-Principal", bad))
                .insert_header(("X-Workspace", "ws_a"))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "principal={bad} should be rejected"
            );
        }
    }

    #[actix_web::test]
    async fn rejects_path_traversal_in_workspace() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        for bad in ["..", "../evil", "etc/passwd", "ws\\a"] {
            let uri = format!("/updates?workspace={bad}");
            let req = test::TestRequest::get().uri(&uri).to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "workspace={bad} should be rejected"
            );
        }
    }

    #[actix_web::test]
    async fn returns_empty_list_for_unknown_workspace() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(cfg))
                .route("/updates", web::get().to(list_agent_updates_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/updates?workspace=missing")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "missing"))
            .to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(body, make_response(vec![]));
    }
}
