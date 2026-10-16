//! GAUI Layout API — REST endpoints for reading and writing agent MUIJ layouts.
//!
//! Provides `GET /agents/{id}/layout` and `PUT /agents/{id}/layout` following
//! the `TaskApi` pattern of a dedicated API struct in its own module.

use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use tracing::error;

use crate::web_api::{api_error_response, resolve_required_scope_from_request};
use magician::magician_v2::agents::AgentDefinitionStore;
use magician::magician_v2::gaui::{
    agent_snapshot_cache_key, load_materialized_snapshot_document, DefaultComponentRegistry,
    MuijDocument, MuijDocumentCache, MuijQueryEngine, MuijStorage, MuijValidationError,
    SnapshotLoadError,
};

// ---------------------------------------------------------------------------
// GauiApi
// ---------------------------------------------------------------------------

pub struct GauiApi {
    definition_store: Arc<AgentDefinitionStore>,
    doc_cache: Option<MuijDocumentCache>,
}

impl GauiApi {
    pub fn new(_muij_storage: MuijStorage, definition_store: Arc<AgentDefinitionStore>) -> Self {
        Self {
            definition_store,
            doc_cache: None,
        }
    }

    pub fn with_doc_cache(mut self, doc_cache: MuijDocumentCache) -> Self {
        self.doc_cache = Some(doc_cache);
        self
    }

    fn scoped_storage(&self, principal: &str, workspace: &str) -> MuijStorage {
        let scoped_store = self.definition_store.for_scope(principal, workspace);
        MuijStorage::new(scoped_store.storage().root().to_path_buf())
    }

    /// GET /agents/{id}/layout
    pub async fn get_layout(&self, request: &HttpRequest, agent_id: &str) -> HttpResponse {
        let (principal, workspace) = match resolve_required_scope_from_request(request) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
        let definition_store = self.definition_store.for_scope(&principal, &workspace);
        let scoped_muij_storage = self.scoped_storage(&principal, &workspace);
        // Check agent exists
        match definition_store.get_definition(agent_id).await {
            Ok(None) => {
                return api_error_response(
                    StatusCode::NOT_FOUND,
                    "agent_not_found",
                    format!("Agent '{}' not found", agent_id),
                    None,
                );
            },
            Err(e) => {
                error!(
                    "Failed to check agent existence for '{}': {:?}",
                    agent_id, e
                );
                return api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    // R338: Generic message — full error (with fs paths) logged server-side only
                    "Internal storage error".to_string(),
                    None,
                );
            },
            Ok(Some(_)) => {}, // agent exists, continue
        }

        let mut query_engine = MuijQueryEngine::new();
        let cache_key = agent_snapshot_cache_key(agent_id, Some(&principal), Some(&workspace));
        match load_materialized_snapshot_document(
            &scoped_muij_storage,
            agent_id,
            &mut query_engine,
            self.doc_cache.as_ref(),
            Some(cache_key.as_str()),
        )
        .await
        {
            Ok(doc) => HttpResponse::Ok().json(doc),
            Err(SnapshotLoadError::InvalidLayout) => api_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "layout_validation_failed",
                "Stored layout failed validation".to_string(),
                None,
            ),
            Err(SnapshotLoadError::StorageRead) => {
                error!("Failed to read layout for '{}'", agent_id);
                api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    "Internal storage error".to_string(),
                    None,
                )
            },
        }
    }

    /// PUT /agents/{id}/layout
    pub async fn put_layout(
        &self,
        request: &HttpRequest,
        agent_id: &str,
        doc: MuijDocument,
    ) -> HttpResponse {
        let (principal, workspace) = match resolve_required_scope_from_request(request) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
        let definition_store = self.definition_store.for_scope(&principal, &workspace);
        let scoped_muij_storage = self.scoped_storage(&principal, &workspace);
        // Check agent exists
        match definition_store.get_definition(agent_id).await {
            Ok(None) => {
                return api_error_response(
                    StatusCode::NOT_FOUND,
                    "agent_not_found",
                    format!("Agent '{}' not found", agent_id),
                    None,
                );
            },
            Err(e) => {
                error!(
                    "Failed to check agent existence for '{}': {:?}",
                    agent_id, e
                );
                return api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    // R338: Generic message — full error (with fs paths) logged server-side only
                    "Internal storage error".to_string(),
                    None,
                );
            },
            Ok(Some(_)) => {},
        }

        // Validate agent_id in body matches path (validate() doesn't cover this)
        if doc.agent_id != agent_id {
            return api_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                format!(
                    "Body agent_id '{}' does not match path '{}'",
                    doc.agent_id, agent_id
                ),
                Some(serde_json::json!({ "field": "agent_id" })),
            );
        }

        // Full document validation (version, components, duplicates, etc.)
        // R215: Include structured error details for component-level errors.
        let registry = DefaultComponentRegistry;
        if let Err(e) = doc.validate(&registry) {
            let details = match &e {
                MuijValidationError::DuplicateComponentId(id) => {
                    Some(serde_json::json!({ "component_id": id, "reason": "duplicate_id" }))
                },
                MuijValidationError::UnknownComponentType(ct) => {
                    Some(serde_json::json!({ "component_type": ct, "reason": "unknown_type" }))
                },
                MuijValidationError::EmptyComponentId => {
                    Some(serde_json::json!({ "reason": "empty_component_id" }))
                },
                MuijValidationError::InvalidPropsShape(id) => {
                    Some(serde_json::json!({ "component_id": id, "reason": "invalid_props_shape" }))
                },
                MuijValidationError::UnsupportedVersion(v) => Some(
                    serde_json::json!({ "field": "muij_version", "value": v, "reason": "unsupported_version" }),
                ),
                MuijValidationError::InvalidAgentId(id) => Some(
                    serde_json::json!({ "field": "agent_id", "value": id, "reason": "invalid_agent_id" }),
                ),
                MuijValidationError::MaxDepthExceeded(depth) => {
                    Some(serde_json::json!({ "reason": "max_depth_exceeded", "max_depth": depth }))
                },
                MuijValidationError::MaxComponentCount(count) => Some(
                    serde_json::json!({ "reason": "max_component_count", "max_components": count }),
                ),
                MuijValidationError::MaxGraphNodes(count) => {
                    Some(serde_json::json!({ "reason": "max_graph_nodes", "max_nodes": count }))
                },
                MuijValidationError::MaxGraphEdges(count) => {
                    Some(serde_json::json!({ "reason": "max_graph_edges", "max_edges": count }))
                },
                MuijValidationError::InvalidGraphProps {
                    component_id,
                    reason,
                } => Some(serde_json::json!({ "component_id": component_id, "reason": reason })),
            };
            return api_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                format!("{}", e),
                details,
            );
        }

        // Write
        match scoped_muij_storage.write_layout(agent_id, &doc).await {
            Ok(()) => {
                if let Some(cache) = self.doc_cache.as_ref() {
                    let cache_key =
                        agent_snapshot_cache_key(agent_id, Some(&principal), Some(&workspace));
                    cache.write().await.insert(cache_key, doc.clone());
                }
                HttpResponse::Ok().json(doc)
            },
            Err(e) => {
                error!("Failed to write layout for '{}': {:?}", agent_id, e);
                api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    // R338: Generic message — full error (with fs paths) logged server-side only
                    "Internal storage error".to_string(),
                    None,
                )
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Handler functions (thin wrappers for route registration)
// ---------------------------------------------------------------------------

pub async fn get_layout_handler(
    gaui: web::Data<GauiApi>,
    request: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    gaui.get_layout(&request, &path.into_inner()).await
}

pub async fn put_layout_handler(
    gaui: web::Data<GauiApi>,
    request: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MuijDocument>,
) -> HttpResponse {
    gaui.put_layout(&request, &path.into_inner(), body.into_inner())
        .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::collections::HashMap;

    use crate::web_api::ApiErrorEnvelope;
    use actix_web::body::MessageBody;
    use actix_web::test::TestRequest;
    use actix_web::HttpRequest;
    use chrono::Utc;
    use magician::magician_v2::agents::{AgentDefinition, AgentDefinitionStore};
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::gaui::{MuijComponent, MuijStorage};

    fn sample_agent_definition(agent_id: &str) -> AgentDefinition {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "Test Agent"
persona: "Test persona"
tools: []
"#
        );
        AgentDefinition::from_yaml_str(&yaml).unwrap()
    }

    fn sample_component(id: &str, component_type: &str) -> MuijComponent {
        MuijComponent {
            id: id.to_string(),
            component_type: component_type.to_string(),
            label: format!("{id} label"),
            source: None,
            query: None,
            props: json!({}),
            static_snapshot: None,
            children: vec![],
        }
    }

    fn sample_document(agent_id: &str) -> MuijDocument {
        MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: agent_id.to_string(),
            layout: vec![
                sample_component("gauge_1", "Gauge"),
                sample_component("log_1", "TerminalTransient"),
            ],
            generated_at: Utc::now(),
        }
    }

    async fn setup(agent_id: &str) -> (GauiApi, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(AgentDefinitionStore::with_workspace_root(tmp.path()));
        store
            .for_scope("test-user", "test-workspace")
            .create_definition(sample_agent_definition(agent_id))
            .await
            .unwrap();
        let muij_storage = MuijStorage::new(tmp.path().join("unused_muij"));
        let api = GauiApi::new(muij_storage, store);
        (api, tmp)
    }

    /// Extract response body as bytes and deserialize.
    fn body_json<T: serde::de::DeserializeOwned>(resp: HttpResponse) -> T {
        let body = resp.into_body().try_into_bytes().unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn scoped_request() -> HttpRequest {
        scoped_request_for("test-user", "test-workspace")
    }

    fn scoped_request_for(principal: &str, workspace: &str) -> HttpRequest {
        TestRequest::default()
            .insert_header(("X-Principal", principal))
            .insert_header(("X-Workspace", workspace))
            .to_http_request()
    }

    fn scoped_muij_storage(api: &GauiApi, principal: &str, workspace: &str) -> MuijStorage {
        MuijStorage::new(
            api.definition_store
                .for_scope(principal, workspace)
                .storage()
                .root()
                .to_path_buf(),
        )
    }

    // ── GET tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_layout_returns_200_with_valid_json() {
        let (api, _tmp) = setup("test-agent").await;
        let doc = sample_document("test-agent");
        scoped_muij_storage(&api, "test-user", "test-workspace")
            .write_layout("test-agent", &doc)
            .await
            .unwrap();

        let request = scoped_request();
        let resp = api.get_layout(&request, "test-agent").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let returned: MuijDocument = body_json(resp);
        assert_eq!(returned.agent_id, "test-agent");
        assert_eq!(returned.layout.len(), 2);
        assert_eq!(returned.layout[0].id, "gauge_1");
    }

    #[tokio::test]
    async fn get_layout_returns_empty_doc_when_no_layout() {
        // R707: REST returns synthetic empty doc (parity with WS snapshot path)
        let (api, _tmp) = setup("test-agent").await;

        let request = scoped_request();
        let resp = api.get_layout(&request, "test-agent").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let doc: MuijDocument = body_json(resp);
        assert_eq!(doc.agent_id, "test-agent");
        assert!(doc.layout.is_empty(), "empty layout expected");
        assert_eq!(doc.muij_version, "1.0");
    }

    #[tokio::test]
    async fn get_layout_returns_404_nonexistent_agent() {
        let (api, _tmp) = setup("test-agent").await;

        let request = scoped_request();
        let resp = api.get_layout(&request, "no-such-agent").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "agent_not_found");
    }

    // ── PUT tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn put_layout_returns_200_valid() {
        let (api, _tmp) = setup("test-agent").await;
        let doc = sample_document("test-agent");

        let request = scoped_request();
        let resp = api.put_layout(&request, "test-agent", doc.clone()).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let returned: MuijDocument = body_json(resp);
        assert_eq!(returned.agent_id, "test-agent");
        assert_eq!(returned.layout.len(), 2);

        // Verify GET roundtrip
        let get_request = scoped_request();
        let get_resp = api.get_layout(&get_request, "test-agent").await;
        assert_eq!(get_resp.status(), StatusCode::OK);
        let get_doc: MuijDocument = body_json(get_resp);
        assert_eq!(get_doc.agent_id, "test-agent");
        assert_eq!(get_doc.layout.len(), 2);
    }

    #[tokio::test]
    async fn put_layout_refreshes_scoped_cache_entry() {
        let (api, _tmp) = setup("test-agent").await;
        let cache: MuijDocumentCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let api = api.with_doc_cache(cache.clone());
        let mut stale = sample_document("test-agent");
        stale.layout[0].id = "stale_component".to_string();
        cache.write().await.insert(
            agent_snapshot_cache_key("test-agent", Some("test-user"), Some("test-workspace")),
            stale,
        );

        let mut fresh = sample_document("test-agent");
        fresh.layout[0].id = "fresh_component".to_string();

        let request = scoped_request();
        let resp = api.put_layout(&request, "test-agent", fresh.clone()).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let cache_key =
            agent_snapshot_cache_key("test-agent", Some("test-user"), Some("test-workspace"));
        let guard = cache.read().await;
        let cached = guard.get(&cache_key).expect("cache entry should exist");
        assert_eq!(cached.layout[0].id, "fresh_component");
    }

    #[tokio::test]
    async fn put_layout_returns_422_bad_version() {
        let (api, _tmp) = setup("test-agent").await;
        let mut doc = sample_document("test-agent");
        doc.muij_version = "2.0".to_string();

        let request = scoped_request();
        let resp = api.put_layout(&request, "test-agent", doc).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "validation_error");
    }

    #[tokio::test]
    async fn put_layout_returns_422_unknown_component() {
        let (api, _tmp) = setup("test-agent").await;
        let mut doc = sample_document("test-agent");
        doc.layout.push(sample_component("bad_1", "UnknownWidget"));

        let request = scoped_request();
        let resp = api.put_layout(&request, "test-agent", doc).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "validation_error");
        // R215: Structured details should include component type and reason
        let details = err.details.expect("should have structured details");
        assert_eq!(details["component_type"], "UnknownWidget");
        assert_eq!(details["reason"], "unknown_type");
    }

    #[tokio::test]
    async fn put_layout_returns_404_nonexistent_agent() {
        let (api, _tmp) = setup("test-agent").await;
        let doc = sample_document("no-such-agent");

        let request = scoped_request();
        let resp = api.put_layout(&request, "no-such-agent", doc).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "agent_not_found");
    }

    #[tokio::test]
    async fn put_layout_returns_422_agent_id_mismatch() {
        let (api, _tmp) = setup("test-agent").await;
        let doc = sample_document("different-agent");

        let request = scoped_request();
        let resp = api.put_layout(&request, "test-agent", doc).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "validation_error");
        assert!(err.details.is_some());
    }

    // ── R194: GET with corrupt stored doc → 422 ────────────────────────

    #[tokio::test]
    async fn get_layout_corrupt_stored_doc_returns_422() {
        let (api, _tmp) = setup("test-agent").await;

        // Persist a document with an unknown component type.
        // write_layout doesn't validate, so the corrupt doc is stored.
        let corrupt = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "test-agent".to_string(),
            layout: vec![MuijComponent {
                id: "c1".to_string(),
                component_type: "NonExistentWidget".to_string(),
                label: "Bad".to_string(),
                source: None,
                query: None,
                props: json!({}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: Utc::now(),
        };
        scoped_muij_storage(&api, "test-user", "test-workspace")
            .write_layout("test-agent", &corrupt)
            .await
            .unwrap();

        // R157: GET should validate and return 422.
        let request = scoped_request();
        let resp = api.get_layout(&request, "test-agent").await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let err: ApiErrorEnvelope = body_json(resp);
        assert_eq!(err.code, "layout_validation_failed");
        // R588: Error message is now generic — check it mentions validation
        assert!(
            err.error.contains("validation"),
            "error should mention validation: {}",
            err.error
        );
    }

    #[tokio::test]
    async fn put_layout_writes_into_scoped_agent_runtime_not_system_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(AgentDefinitionStore::with_workspace_root(tmp.path()));
        store
            .for_scope("scope-a", "workspace-a")
            .create_definition(sample_agent_definition("test-agent"))
            .await
            .unwrap();
        store
            .for_scope("scope-b", "workspace-b")
            .create_definition(sample_agent_definition("test-agent"))
            .await
            .unwrap();

        let api = GauiApi::new(MuijStorage::new(tmp.path().join("unused_muij")), store);
        let workspace = ArtifactV2Workspace::new(tmp.path());

        let mut doc_a = sample_document("test-agent");
        doc_a.layout[0].id = "scope_a_component".to_string();
        let resp_a = api
            .put_layout(
                &scoped_request_for("scope-a", "workspace-a"),
                "test-agent",
                doc_a,
            )
            .await;
        assert_eq!(resp_a.status(), StatusCode::OK);

        let mut doc_b = sample_document("test-agent");
        doc_b.layout[0].id = "scope_b_component".to_string();
        let resp_b = api
            .put_layout(
                &scoped_request_for("scope-b", "workspace-b"),
                "test-agent",
                doc_b,
            )
            .await;
        assert_eq!(resp_b.status(), StatusCode::OK);

        let path_a = workspace
            .scoped_agent_runtime_root("scope-a", "workspace-a")
            .join("agents/test-agent/ui_layout.muij.json");
        let path_b = workspace
            .scoped_agent_runtime_root("scope-b", "workspace-b")
            .join("agents/test-agent/ui_layout.muij.json");
        let template_path = workspace
            .system_agent_template_root()
            .join("agents/test-agent/ui_layout.muij.json");

        assert!(path_a.exists(), "scope-a layout should be persisted");
        assert!(path_b.exists(), "scope-b layout should be persisted");
        assert!(
            !template_path.exists(),
            "runtime agent layout must not be written into system agent templates"
        );

        let stored_a: MuijDocument =
            serde_json::from_str(&std::fs::read_to_string(path_a).unwrap()).unwrap();
        let stored_b: MuijDocument =
            serde_json::from_str(&std::fs::read_to_string(path_b).unwrap()).unwrap();
        assert_eq!(stored_a.layout[0].id, "scope_a_component");
        assert_eq!(stored_b.layout[0].id, "scope_b_component");
    }
}
