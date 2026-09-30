//! Real-HTTP end-to-end integration test for the Live Thinking Map REST API.
//!
//! Unlike the in-process `actix_web::test::init_service` unit tests inside
//! `thinking_maps_api.rs`, this suite boots the ACTUAL route stack on a real
//! bound TCP port via [`actix_test::start`] and drives the full lifecycle over
//! the wire with a real HTTP client (`awc`, through the returned `TestServer`).
//! A real [`ThinkingMapStore`] persists to a per-test tempdir.
//!
//! This is the durable, CI-able proof of the "S3-REST" gate in
//! `docs/runbooks/2026-07-21-e0-canonical-canary-and-removal.md`.
//!
//! Run with:
//! `cargo test -p magician --test thinking_map_http_integration_test`

use actix_web::{web, App};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician_api::thinking_maps_api::{configure, ThinkingMapsApi};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::TempDir;

/// The scope prefix the real server mounts the thinking-maps routes under.
/// Paths driven over the wire are therefore `/api/magician/v2/thinking-maps...`.
const BASE: &str = "/api/magician/v2/thinking-maps";

/// Owner-authored `add_node` operation body (a spoken/asserted node) matching
/// the wire shape the reducer expects.
fn add_node_op(node_id: &str) -> Value {
    json!({
        "op": "add_node",
        "node": {
            "node_id": node_id,
            "kind": "idea",
            "label": format!("label-{node_id}"),
            "epistemic_state": "provisional",
            "assertion_origin": "owner_spoken",
            "confidence": 0.5,
            "created_at": "2026-07-21T00:00:00Z",
            "updated_at": "2026-07-21T00:00:00Z"
        }
    })
}

/// Boot a real bound-port server backed by a fresh tempdir-backed workspace.
/// Returns the `TempDir` guard (kept alive for the test) plus the `TestServer`.
fn start_server() -> (TempDir, actix_test::TestServer) {
    let tmp = TempDir::new().expect("tempdir");
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let srv = actix_test::start(move || {
        App::new()
            .app_data(web::Data::new(Arc::new(ThinkingMapsApi::new(
                workspace.clone(),
            ))))
            .service(web::scope("/api/magician/v2").configure(configure))
    });
    (tmp, srv)
}

/// Full non-LLM lifecycle over real HTTP.
#[actix_web::test]
async fn thinking_map_full_lifecycle_over_http() {
    let (_tmp, srv) = start_server();

    // ── CREATE ──────────────────────────────────────────────────────────────
    // POST /thinking-maps {"title":"canary"} → 201, capture map_id.
    let mut resp = srv
        .post(BASE)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({ "title": "canary" }))
        .await
        .expect("create request send");
    assert_eq!(resp.status(), 201, "create should be 201");
    let created: Value = resp.json().await.expect("create body json");
    assert_eq!(created["title"], "canary");
    assert_eq!(created["revision"], 0);
    let map_id = created["map_id"]
        .as_str()
        .expect("map_id present")
        .to_string();
    let map_path = format!("{BASE}/{map_id}");

    // ── APPLY add_node (owner) ─────────────────────────────────────────────
    // POST /{id}/operations → 200 applied, resulting_revision 1, node present.
    let ops_body = json!({
        "operations": [add_node_op("n1")],
        "idempotency_key": "i1",
        "base_revision": 0
    });
    let mut resp = srv
        .post(format!("{map_path}/operations"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&ops_body)
        .await
        .expect("apply-ops request send");
    assert_eq!(resp.status(), 200, "apply ops should be 200");
    let applied: Value = resp.json().await.expect("apply body json");
    assert_eq!(applied["outcome"], "applied");
    assert_eq!(applied["resulting_revision"], 1);
    assert!(
        applied["map"]["nodes"].get("n1").is_some(),
        "returned map should contain node n1"
    );

    // ── GET → revision 1, node present ─────────────────────────────────────
    let mut resp = srv
        .get(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("get request send");
    assert_eq!(resp.status(), 200, "get should be 200");
    let fetched: Value = resp.json().await.expect("get body json");
    assert_eq!(fetched["revision"], 1);
    assert!(fetched["nodes"].get("n1").is_some());

    // ── IDEMPOTENT REPLAY (same idempotency_key) ───────────────────────────
    // Repeat the SAME operations POST → 200 idempotent_replay, revision still 1.
    let mut resp = srv
        .post(format!("{map_path}/operations"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&ops_body)
        .await
        .expect("idempotent replay request send");
    assert_eq!(resp.status(), 200, "idempotent replay should be 200");
    let replay: Value = resp.json().await.expect("replay body json");
    assert_eq!(replay["outcome"], "idempotent_replay");
    assert_eq!(replay["resulting_revision"], 1);

    // ── REVISION CONFLICT (stale base_revision, new idempotency_key) ────────
    // → 409 revision_conflict, expected 1, actual 0.
    let mut resp = srv
        .post(format!("{map_path}/operations"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({
            "operations": [add_node_op("n2")],
            "idempotency_key": "conflict",
            "base_revision": 0
        }))
        .await
        .expect("conflict request send");
    assert_eq!(resp.status(), 409, "stale base_revision should be 409");
    let conflict: Value = resp.json().await.expect("conflict body json");
    assert_eq!(conflict["error"], "revision_conflict");
    assert_eq!(conflict["expected"], 1);
    assert_eq!(conflict["actual"], 0);

    // ── APPLY a SECOND valid op at base_revision 1 → 200, revision 2 ────────
    // (so the events log now holds ≥2 events.)
    let mut resp = srv
        .post(format!("{map_path}/operations"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({
            "operations": [add_node_op("n2")],
            "idempotency_key": "i2",
            "base_revision": 1
        }))
        .await
        .expect("second apply request send");
    assert_eq!(resp.status(), 200);
    let applied2: Value = resp.json().await.expect("second apply body json");
    assert_eq!(applied2["outcome"], "applied");
    assert_eq!(applied2["resulting_revision"], 2);

    // ── EVENTS ─────────────────────────────────────────────────────────────
    // GET /{id}/events?after_seq=0 → 200, array with the applied events in
    // sequence order (2 events).
    let mut resp = srv
        .get(format!("{map_path}/events?after_seq=0"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("events request send");
    assert_eq!(resp.status(), 200, "events should be 200");
    let events: Value = resp.json().await.expect("events body json");
    let events_arr = events.as_array().expect("events is an array");
    assert_eq!(events_arr.len(), 2, "two applied ops ⇒ two events");
    assert_eq!(events_arr[0]["sequence"], 1);
    assert_eq!(events_arr[1]["sequence"], 2);

    // GET /{id}/events?after_seq=1 → only the later event.
    let mut resp = srv
        .get(format!("{map_path}/events?after_seq=1"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("events after_seq=1 request send");
    assert_eq!(resp.status(), 200);
    let events_after: Value = resp.json().await.expect("events after body json");
    let events_after_arr = events_after.as_array().expect("array");
    assert_eq!(events_after_arr.len(), 1, "after_seq=1 ⇒ only seq 2");
    assert_eq!(events_after_arr[0]["sequence"], 2);

    // ── REPLAY at_seq=1 → the map as of revision 1 (second node absent) ────
    let mut resp = srv
        .get(format!("{map_path}/replay?at_seq=1"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("replay request send");
    assert_eq!(resp.status(), 200, "replay should be 200");
    let replayed: Value = resp.json().await.expect("replay body json");
    assert_eq!(replayed["revision"], 1);
    assert!(replayed["nodes"].get("n1").is_some());
    assert!(replayed["nodes"].get("n2").is_none(), "n2 absent at seq 1");

    // ── PATCH title → 200; GET reflects the rename ─────────────────────────
    let mut resp = srv
        .patch(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({ "title": "canary renamed" }))
        .await
        .expect("patch title request send");
    assert_eq!(resp.status(), 200, "patch title should be 200");
    let patched: Value = resp.json().await.expect("patch body json");
    assert_eq!(patched["outcome"], "applied");
    assert_eq!(patched["map"]["title"], "canary renamed");

    let mut resp = srv
        .get(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("get after patch send");
    let after_patch: Value = resp.json().await.expect("get after patch json");
    assert_eq!(after_patch["title"], "canary renamed");

    // ── PATCH lifecycle=archived → 200; LIST reflects it ───────────────────
    let mut resp = srv
        .patch(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({ "lifecycle": "archived" }))
        .await
        .expect("patch lifecycle request send");
    assert_eq!(resp.status(), 200, "patch lifecycle should be 200");
    let archived: Value = resp.json().await.expect("archive body json");
    assert_eq!(archived["map"]["lifecycle"], "archived");

    let mut resp = srv
        .get(BASE)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("list request send");
    assert_eq!(resp.status(), 200, "list should be 200");
    let list: Value = resp.json().await.expect("list body json");
    let summaries = list.as_array().expect("list is an array");
    let this = summaries
        .iter()
        .find(|s| s["map_id"] == map_id)
        .expect("map present in list");
    assert_eq!(this["lifecycle"], "archived", "list reflects archive");

    // ── LIST carries a bounded node_preview (mini-graph thumbnail) ─────────
    // This map has live nodes (n1, n2), so the summary must include a preview
    // with those nodes so the client library card can draw a mini-graph.
    let preview = this
        .get("node_preview")
        .and_then(Value::as_object)
        .expect("list summary carries node_preview when the map has live nodes");
    let preview_nodes = preview["nodes"]
        .as_array()
        .expect("node_preview.nodes is an array");
    assert!(
        preview_nodes.iter().any(|n| n["node_id"] == "n1"),
        "node_preview includes node n1"
    );
    // Each previewed node exposes the mini-graph fields.
    let first = &preview_nodes[0];
    assert!(first.get("node_id").is_some());
    assert!(first.get("kind").is_some());
    assert!(first.get("suggested").is_some());
    assert!(first.get("title").is_some());
    assert!(preview.get("edges").and_then(Value::as_array).is_some());

    // ── PATCH with neither title nor lifecycle → 400 nothing_to_patch ──────
    let mut resp = srv
        .patch(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({}))
        .await
        .expect("empty patch request send");
    assert_eq!(resp.status(), 400, "empty patch should be 400");
    let empty_patch: Value = resp.json().await.expect("empty patch body json");
    assert_eq!(empty_patch["error"], "nothing_to_patch");

    // ── RESTORE at seq 1 as a branch → 201; source unchanged ───────────────
    // Capture the source head just before the restore for a byte-identical
    // comparison afterwards (the strongest "restore leaves source untouched"
    // assertion).
    let mut resp = srv
        .get(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("source before restore send");
    let source_before: Value = resp.json().await.expect("source before restore json");

    let branch_id = uuid::Uuid::new_v4().to_string();
    let mut resp = srv
        .post(format!("{map_path}/restore"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({
            "at_sequence": 1,
            "new_map_id": branch_id,
            "new_title": "branch"
        }))
        .await
        .expect("restore request send");
    assert_eq!(resp.status(), 201, "restore should be 201");
    let branch: Value = resp.json().await.expect("restore body json");
    assert_eq!(branch["map_id"], branch_id);
    assert_eq!(branch["title"], "branch");
    // Branch is the seq-1 state: n1 present, n2 absent.
    assert!(branch["nodes"].get("n1").is_some());
    assert!(branch["nodes"].get("n2").is_none());

    // Source GET is untouched by the restore. Its revision is 4 (2 add_node ops
    // + 2 PATCH envelopes — title, then lifecycle — since PATCH is applied
    // event-sourced through the reducer and bumps the revision like any op).
    // The restore forks a NEW map and must leave the source's head as-is: n2
    // still present, still archived.
    let mut resp = srv
        .get(&map_path)
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send()
        .await
        .expect("source after restore send");
    let source_after: Value = resp.json().await.expect("source after restore json");
    assert_eq!(
        source_after["revision"], 4,
        "source head unchanged by restore (2 ops + 2 patches)"
    );
    assert!(source_after["nodes"].get("n2").is_some());
    assert_eq!(source_after["lifecycle"], "archived");
    assert_eq!(
        source_after, source_before,
        "restore must leave the source map byte-identical"
    );

    // ── MISSING SCOPE (no principal/workspace headers, no body scope) ──────
    // → 400 missing_scope.
    let mut resp = srv
        .post(BASE)
        .send_json(&json!({ "title": "no scope" }))
        .await
        .expect("no-scope request send");
    assert_eq!(resp.status(), 400, "missing scope should be 400");
    let no_scope: Value = resp.json().await.expect("no-scope body json");
    assert_eq!(no_scope["error"], "missing_scope");

    // ── INTERPRET without an LLM router → 503 llm_unavailable ──────────────
    // The global operation router is unset in this test process, so the
    // handler degrades gracefully. (The map must exist so the 503 — not a 404
    // — is the assertion path.)
    let mut resp = srv
        .post(format!("{map_path}/interpret"))
        .insert_header(("X-Principal", "anonymous"))
        .insert_header(("X-Workspace", "default"))
        .send_json(&json!({ "text": "let's reconsider", "intent": "break_open" }))
        .await
        .expect("interpret request send");
    assert_eq!(resp.status(), 503, "interpret without router should be 503");
    let interpret_err: Value = resp.json().await.expect("interpret body json");
    assert_eq!(interpret_err["error"], "llm_unavailable");
}
