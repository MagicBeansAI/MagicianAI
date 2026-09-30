//! Tutor drawing primitives API.
//!
//! Serves the merged set of data-driven Tutor primitive *recipes* (global
//! built-ins overlaid with a scope's custom files) so clients can render any
//! primitive with no app rebuild. Read-only and cache-friendly.
//!
//! Route:
//! ```text
//! GET /api/magician/v2/tutor/primitives
//!     -> { "primitives": [ …recipe… ], "etag": "<sha256-hex>" }
//! ```
//!
//! Principal and workspace come from the verified bearer context engraved by
//! the outer authentication middleware.

use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use magician::magician_v2::tutor::primitives;

use crate::scope::resolve_required_scope;

/// Shared state for the tutor primitives API: the runtime root under which the
/// global + per-scope recipe folders live (`MAGICIAN_ROOT_DIR` /
/// `$HOME/MagicianNotes`).
#[derive(Clone)]
pub struct TutorApi {
    runtime_root: PathBuf,
}

impl TutorApi {
    pub fn new<P: AsRef<std::path::Path>>(runtime_root: P) -> Self {
        Self {
            runtime_root: runtime_root.as_ref().to_path_buf(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct TutorPrimitivesQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// GET /api/magician/v2/tutor/primitives
///
/// Returns the merged (global + scope) recipe set plus a stable `etag` over the
/// serialized set for cache validation.
pub async fn get_tutor_primitives_handler(
    api: web::Data<Arc<TutorApi>>,
    req: HttpRequest,
    query: web::Query<TutorPrimitivesQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };

    let runtime_root = api.runtime_root.clone();
    // Disk reads are blocking; hop to a blocking thread to keep the reactor free.
    let recipes = web::block(move || primitives::load(&runtime_root, &principal, &workspace))
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let body = json!({ "primitives": recipes });
    let etag = etag_for(&body);

    Ok(HttpResponse::Ok().json(json!({
        "primitives": body["primitives"],
        "etag": etag,
    })))
}

/// A stable content hash over the serialized primitive set (sha256 hex of the
/// `{ "primitives": [...] }` JSON), used as the cache `etag`.
///
/// Determinism depends on serde_json serializing object keys in a canonical
/// (sorted) order — true because this crate uses serde_json WITHOUT the
/// `preserve_order` feature (its `Map` is a `BTreeMap`). If `preserve_order` is
/// ever enabled workspace-wide, re-serialization would follow source key order
/// and the etag could differ for identical content — canonicalize keys here then.
fn etag_for(body: &serde_json::Value) -> String {
    let serialized = serde_json::to_vec(body).unwrap_or_default();
    let digest = Sha256::digest(&serialized);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn configure_tutor_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/tutor").route("/primitives", web::get().to(get_tutor_primitives_handler)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::{test, App};
    use serde_json::json;

    fn write_recipe(dir: &std::path::Path, file_name: &str, body: &serde_json::Value) {
        std::fs::create_dir_all(dir).expect("create recipe dir");
        std::fs::write(
            dir.join(file_name),
            serde_json::to_vec_pretty(body).expect("serialize recipe"),
        )
        .expect("write recipe file");
    }

    #[actix_web::test]
    async fn serves_merged_set_with_stable_etag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path();

        write_recipe(
            &primitives::global_primitives_dir(base),
            "circle.json",
            &json!({
                "type": "circle",
                "draw": [{ "op": "circle", "cx": "cx", "cy": "cy", "r": "r" }]
            }),
        );
        write_recipe(
            &primitives::scope_primitives_dir(base, "anonymous", "default"),
            "star.json",
            &json!({
                "type": "star",
                "draw": [{ "op": "polyline", "points": [["x", "y"]] }]
            }),
        );

        let api = web::Data::new(Arc::new(TutorApi::new(base)));
        let app = test::init_service(
            App::new()
                .app_data(api.clone())
                .configure(configure_tutor_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/tutor/primitives?workspace=default")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;

        let primitives_arr = body["primitives"].as_array().expect("primitives array");
        assert_eq!(
            primitives_arr.len(),
            2,
            "expected global + scope merged set"
        );
        let types: Vec<&str> = primitives_arr
            .iter()
            .filter_map(|r| r["type"].as_str())
            .collect();
        assert!(types.contains(&"circle"));
        assert!(types.contains(&"star"));
        assert!(
            body["etag"].as_str().is_some_and(|e| e.len() == 64),
            "etag should be a 64-char sha256 hex string"
        );
    }

    #[actix_web::test]
    async fn unknown_scope_returns_built_ins_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path();
        write_recipe(
            &primitives::global_primitives_dir(base),
            "circle.json",
            &json!({
                "type": "circle",
                "draw": [{ "op": "circle", "cx": "cx", "cy": "cy", "r": "r" }]
            }),
        );

        let api = web::Data::new(Arc::new(TutorApi::new(base)));
        let app = test::init_service(
            App::new()
                .app_data(api.clone())
                .configure(configure_tutor_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/tutor/primitives?workspace=nowhere")
            .insert_header(("X-Principal", "nobody"))
            .insert_header(("X-Workspace", "nowhere"))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        let primitives_arr = body["primitives"].as_array().expect("primitives array");
        assert_eq!(primitives_arr.len(), 1);
        assert_eq!(primitives_arr[0]["type"].as_str(), Some("circle"));
    }
}
