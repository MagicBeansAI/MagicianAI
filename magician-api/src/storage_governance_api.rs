use actix_web::{web, HttpRequest, HttpResponse};
use magician_comms::channel_assist::governance::StorageGovernanceService;
use serde::Deserialize;

use magician::magician_v2::storage_governance::DuckDbTarget;

use crate::scope::resolve_required_scope;
use magician::magician_v2::apps::registry::AppStoreMaintenanceAction;

const CONFIRM_DATABASE: &str = "COMPACT DATABASE";
const CONFIRM_PARQUET: &str = "COMPACT PARQUET";
const CONFIRM_RETENTION: &str = "APPLY RETENTION";
const CONFIRM_CLEAR_METRICS: &str = "CLEAR COMPACTION METRICS";
const CONFIRM_ATTENTION_OPTIMIZE: &str = "OPTIMIZE ATTENTION";
const CONFIRM_ATTENTION_RETENTION: &str = "CLEAN ATTENTION HISTORY";
const CONFIRM_ATTENTION_RECLAIM: &str = "RECLAIM ATTENTION DATABASE";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageScopeQuery {
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactDatabasesRequest {
    targets: Vec<DuckDbTarget>,
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactParquetRequest {
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRetentionRequest {
    retention_days: u32,
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearCompactionMetricsRequest {
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionOptimizeRequest {
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionRetentionRequest {
    retention_days: u32,
    #[serde(default)]
    confirmation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionReclaimRequest {
    confirmation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppStoreMaintenanceRequest {
    operation: AppStoreMaintenanceAction,
    confirmation: String,
}

pub fn configure(config: &mut web::ServiceConfig) {
    config
        .route("/storage", web::get().to(snapshot_handler))
        .route(
            "/storage/maintenance",
            web::get().to(maintenance_status_handler),
        )
        .route(
            "/storage/actions/app-store",
            web::post().to(maintain_app_store_handler),
        )
        .route(
            "/storage/actions/compact-databases",
            web::post().to(compact_databases_handler),
        )
        .route(
            "/storage/actions/compact-parquet",
            web::post().to(compact_parquet_handler),
        )
        .route(
            "/storage/actions/apply-retention",
            web::post().to(apply_retention_handler),
        )
        .route(
            "/storage/actions/clear-compaction-metrics",
            web::post().to(clear_compaction_metrics_handler),
        )
        .route(
            "/storage/actions/attention-learning/optimize",
            web::post().to(optimize_attention_learning_handler),
        )
        .route(
            "/storage/actions/attention-learning/retention-preview",
            web::post().to(preview_attention_learning_retention_handler),
        )
        .route(
            "/storage/actions/attention-learning/retention-apply",
            web::post().to(apply_attention_learning_retention_handler),
        )
        .route(
            "/storage/actions/attention-learning/reclaim",
            web::post().to(reclaim_attention_learning_handler),
        );
}

pub async fn maintain_app_store_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<AppStoreMaintenanceRequest>,
    apps: web::Data<crate::apps_api::AppPlatformApi>,
) -> HttpResponse {
    let now = chrono::Utc::now();
    let authenticated = match crate::apps_api::authenticated_app_scope(&request, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if query
        .workspace
        .as_deref()
        .is_some_and(|value| value != authenticated.scope().workspace.as_str())
    {
        return HttpResponse::Forbidden()
            .json(serde_json::json!({"error":"app_storage_scope_mismatch"}));
    }
    if body.confirmation != body.operation.confirmation() {
        return confirmation_error(body.operation.confirmation());
    }
    match apps
        .registry()
        .maintain_store(&authenticated, body.operation, now)
        .await
    {
        Ok(Some(report)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(report),
        Ok(None) => {
            HttpResponse::NotFound().json(serde_json::json!({"error":"app_store_not_initialized"}))
        },
        Err(error) => {
            tracing::warn!(error = %error, "App store maintenance failed");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error":"app_store_maintenance_failed",
                "message":"App database maintenance did not finish. Check the runtime log for details."
            }))
        },
    }
}

pub async fn snapshot_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match service.snapshot(&principal, &workspace).await {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(error) => internal_error(error),
    }
}

/// Small scoped status endpoint; safe to poll while the database is gated.
pub async fn maintenance_status_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match service
        .database_maintenance_status(&principal, &workspace)
        .await
    {
        Ok(status) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => internal_error(error),
    }
}

pub async fn compact_databases_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<CompactDatabasesRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_DATABASE {
        return confirmation_error(CONFIRM_DATABASE);
    }
    if body.targets.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "at least one database target is required"
        }));
    }
    let mut targets = body.targets.clone();
    targets.sort_by_key(|target| match target {
        DuckDbTarget::Analytics => 0,
        DuckDbTarget::ChannelAssist => 1,
        DuckDbTarget::UiThreads => 2,
        DuckDbTarget::Social => 3,
    });
    targets.dedup();
    match service
        .compact_databases(&principal, &workspace, &targets)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => internal_error(error),
    }
}

pub async fn compact_parquet_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<CompactParquetRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_PARQUET {
        return confirmation_error(CONFIRM_PARQUET);
    }
    match service.compact_parquet(&principal, &workspace).await {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => internal_error(error),
    }
}

pub async fn apply_retention_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<ApplyRetentionRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_RETENTION {
        return confirmation_error(CONFIRM_RETENTION);
    }
    match service
        .apply_retention(&principal, &workspace, body.retention_days)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) if error.to_string().contains("retention_days") => {
            HttpResponse::BadRequest().json(serde_json::json!({ "error": error.to_string() }))
        },
        Err(error) => internal_error(error),
    }
}

pub async fn clear_compaction_metrics_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<ClearCompactionMetricsRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_CLEAR_METRICS {
        return confirmation_error(CONFIRM_CLEAR_METRICS);
    }
    match service
        .clear_compaction_metrics(&principal, &workspace)
        .await
    {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(error) => internal_error(error),
    }
}

pub async fn optimize_attention_learning_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<AttentionOptimizeRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_ATTENTION_OPTIMIZE {
        return confirmation_error(CONFIRM_ATTENTION_OPTIMIZE);
    }
    match service
        .optimize_attention_learning(&principal, &workspace)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => attention_maintenance_error(error),
    }
}

pub async fn preview_attention_learning_retention_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<AttentionRetentionRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation.is_some() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "retention preview must not carry an apply confirmation"
        }));
    }
    match service
        .retain_attention_learning(&principal, &workspace, body.retention_days, false)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => attention_maintenance_error(error),
    }
}

pub async fn apply_attention_learning_retention_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<AttentionRetentionRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation.as_deref() != Some(CONFIRM_ATTENTION_RETENTION) {
        return confirmation_error(CONFIRM_ATTENTION_RETENTION);
    }
    match service
        .retain_attention_learning(&principal, &workspace, body.retention_days, true)
        .await
    {
        Ok(report) => {
            crate::canonical_attention_api::invalidate_canonical_attention_projection_cache(
                &principal, &workspace,
            );
            HttpResponse::Ok().json(report)
        },
        Err(error) => attention_maintenance_error(error),
    }
}

pub async fn reclaim_attention_learning_handler(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<AttentionReclaimRequest>,
    service: web::Data<StorageGovernanceService>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CONFIRM_ATTENTION_RECLAIM {
        return confirmation_error(CONFIRM_ATTENTION_RECLAIM);
    }
    match service
        .reclaim_attention_learning(&principal, &workspace)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => attention_maintenance_error(error),
    }
}

fn confirmation_error(expected: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "error": "confirmation_mismatch",
        "expected": expected
    }))
}

fn internal_error(error: anyhow::Error) -> HttpResponse {
    tracing::error!(
        target: "storage_governance",
        error = %error,
        "storage governance operation failed"
    );
    HttpResponse::InternalServerError().json(serde_json::json!({
        "error": "storage_maintenance_failed",
        "message": error.to_string()
    }))
}

fn attention_maintenance_error(error: anyhow::Error) -> HttpResponse {
    let message = error.to_string();
    if message.contains("retention_days") {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": message }));
    }
    if message.contains("maintenance is already running")
        || message.contains("maintenance is in progress")
        || message.contains("physical reclaim is already running")
        || message.contains("timed out draining other attention-learning processes")
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "attention_maintenance_in_progress",
            "message": message,
        }));
    }
    if message.contains("maintenance is unavailable") {
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "attention_maintenance_unavailable",
            "message": message,
        }));
    }
    internal_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_comms::channel_assist::governance::StorageGovernanceService;

    use actix_web::{http::StatusCode, test, App};

    use magician::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace, ui_threads::UiThreadStore,
    };
    use magician_comms::channel_assist::store::MailAssistStore;

    fn service(root: &std::path::Path) -> StorageGovernanceService {
        let layout = ArtifactV2Workspace::new(root);
        StorageGovernanceService::new(
            layout.clone(),
            MailAssistStore::open_workspace(layout.clone()).expect("mail"),
            UiThreadStore::open_workspace(layout.clone()).expect("threads"),
            std::sync::Arc::new(
                magician::magician_v2::social::store::SocialStoreRegistry::new(layout),
            ),
        )
        .with_attention_learning(
            magician::magician_v2::attention::learning::AttentionLearningService::open(
                root,
                magician::config::AttentionLearningConfig::default(),
            )
            .expect("attention learning"),
        )
    }

    #[actix_web::test]
    async fn app_store_maintenance_requires_verified_scope_and_keeps_dormant_stores_lazy() {
        use actix_web::HttpMessage;
        use magician::magician_v2::cloudflare_access::{
            VerifiedRequestAuthentication, VerifiedRequestIdentity,
        };
        let temporary = tempfile::tempdir().expect("temporary");
        let root = temporary.path().canonicalize().unwrap();
        let layout = ArtifactV2Workspace::new(&root);
        let database = layout.app_store_db_path("owner", "default");
        let apps = web::Data::new(crate::apps_api::AppPlatformApi::new(layout));
        for (verified, query_workspace, confirmation, expected) in [
            (false, None, "CHECK APP DATABASE", StatusCode::UNAUTHORIZED),
            (
                true,
                Some("other"),
                "CHECK APP DATABASE",
                StatusCode::FORBIDDEN,
            ),
            (true, None, "yes", StatusCode::BAD_REQUEST),
            (true, None, "CHECK APP DATABASE", StatusCode::NOT_FOUND),
        ] {
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/app-store")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request();
            if verified {
                request
                    .extensions_mut()
                    .insert(VerifiedRequestIdentity::for_test_at(
                        "owner",
                        Some("default"),
                        VerifiedRequestAuthentication::CloudflareAccess,
                        chrono::Utc::now(),
                    ));
            }
            let response = maintain_app_store_handler(
                request,
                web::Query(StorageScopeQuery {
                    workspace: query_workspace.map(str::to_owned),
                }),
                web::Json(AppStoreMaintenanceRequest {
                    operation: AppStoreMaintenanceAction::Verify,
                    confirmation: confirmation.to_owned(),
                }),
                apps.clone(),
            )
            .await;
            assert_eq!(response.status(), expected);
            assert!(
                !database.exists(),
                "inspection or rejected maintenance must not initialize an App database"
            );
        }
    }

    #[actix_web::test]
    async fn app_store_http_maintenance_preserves_encrypted_records_during_scoped_activity() {
        use actix_web::{dev::Service, HttpMessage};
        use magician::magician_v2::{
            apps::{models::AppReference, records::AppScope, registry::AppWorkflowControlKind},
            cloudflare_access::{VerifiedRequestAuthentication, VerifiedRequestIdentity},
        };
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let layout = ArtifactV2Workspace::new(&root);
        let database = layout.app_store_db_path("storage-owner", "default");
        let apps = web::Data::new(crate::apps_api::AppPlatformApi::new(layout));
        let scope = AppScope {
            principal: AppReference::parse("storage-owner").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        };
        let other = AppScope {
            workspace: AppReference::parse("other").unwrap(),
            ..scope.clone()
        };
        let kind = AppWorkflowControlKind::RunState;
        for candidate in [&scope, &other] {
            apps.registry()
                .publish_workflow_control_generation(
                    candidate.clone(),
                    "task-fixture".to_owned(),
                    "exec-fixture".to_owned(),
                    kind,
                    b"retained encrypted workflow record".to_vec(),
                    chrono::Utc::now(),
                )
                .await
                .unwrap();
        }
        let other_before = apps
            .registry()
            .current_workflow_control(
                other.clone(),
                "task-fixture".to_owned(),
                "exec-fixture".to_owned(),
                kind,
            )
            .await
            .unwrap()
            .unwrap();
        let app = test::init_service(
            App::new()
                .app_data(apps.clone())
                .wrap_fn(|request, service| {
                    request
                        .extensions_mut()
                        .insert(VerifiedRequestIdentity::for_test_at(
                            "storage-owner",
                            Some("default"),
                            VerifiedRequestAuthentication::CloudflareAccess,
                            chrono::Utc::now(),
                        ));
                    service.call(request)
                })
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;
        // Exercise actual routing/extraction, not only the Rust handler's arguments.
        for (query, payload, expected) in [
            (
                "?workspace=other",
                serde_json::json!({"operation":"verify","confirmation":"CHECK APP DATABASE"}),
                StatusCode::FORBIDDEN,
            ),
            (
                "",
                serde_json::json!({"operation":"verify","confirmation":"yes"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                "",
                serde_json::json!({"operation":"erase","confirmation":"CHECK APP DATABASE"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                "",
                serde_json::json!({"operation":"verify","confirmation":"CHECK APP DATABASE","path":"other.sqlite3"}),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let request = test::TestRequest::post()
                .uri(&format!(
                    "/api/magician/v2/storage/actions/app-store{query}"
                ))
                .insert_header(("X-Workspace", "default"))
                .set_json(payload)
                .to_request();
            assert_eq!(test::call_service(&app, request).await.status(), expected);
        }
        let mut expected_generation = 1;
        for operation in [
            AppStoreMaintenanceAction::Verify,
            AppStoreMaintenanceAction::Optimize,
            AppStoreMaintenanceAction::Reclaim,
        ] {
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/app-store")
                .insert_header(("X-Workspace","default"))
                .set_json(serde_json::json!({"operation":operation,"confirmation":operation.confirmation()}))
                .to_request();
            let maintenance = test::call_service(&app, request);
            let writes = async {
                for index in 0..4 {
                    let payload =
                        format!("retained encrypted workflow record {expected_generation}:{index}")
                            .into_bytes();
                    apps.registry()
                        .publish_workflow_control_generation(
                            scope.clone(),
                            "task-fixture".to_owned(),
                            "exec-fixture".to_owned(),
                            kind,
                            payload.clone(),
                            chrono::Utc::now(),
                        )
                        .await
                        .unwrap();
                    let current = apps
                        .registry()
                        .current_workflow_control(
                            scope.clone(),
                            "task-fixture".to_owned(),
                            "exec-fixture".to_owned(),
                            kind,
                        )
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(current.sealed_blob(), payload.as_slice());
                }
            };
            let other_reads = async {
                for _ in 0..4 {
                    let current = apps
                        .registry()
                        .current_workflow_control(
                            other.clone(),
                            "task-fixture".to_owned(),
                            "exec-fixture".to_owned(),
                            kind,
                        )
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(current.content_digest(), other_before.content_digest());
                    assert_eq!(current.generation(), other_before.generation());
                }
            };
            let (response, (), ()) = tokio::join!(maintenance, writes, other_reads);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get("cache-control").unwrap(),
                "private, no-store"
            );
            let report: serde_json::Value = test::read_body_json(response).await;
            assert_eq!(report["principal"], "storage-owner");
            assert_eq!(report["workspace"], "default");
            assert_eq!(report["relative_path"], "apps/app_store.sqlite3");
            assert_eq!(report["encrypted"], true);
            assert_eq!(report["integrity_ok"], true);
            assert_eq!(
                report["operation"],
                serde_json::to_value(operation).unwrap()
            );
            assert!(report["database_bytes_after"].as_u64().unwrap() > 0);
            assert!(report["wal_bytes_after"].as_u64().is_some());
            assert!(report["shm_bytes_after"].as_u64().is_some());
            assert_eq!(report["checkpoint_busy"], false);
            expected_generation += 4;
            let current = apps
                .registry()
                .current_workflow_control(
                    scope.clone(),
                    "task-fixture".to_owned(),
                    "exec-fixture".to_owned(),
                    kind,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.generation(), expected_generation);
            let bytes = std::fs::read(&database).unwrap();
            assert!(!bytes.starts_with(b"SQLite format 3\0"));
            assert!(!bytes
                .windows(b"retained encrypted workflow record".len())
                .any(|window| window == b"retained encrypted workflow record"));
        }
    }

    #[actix_web::test]
    async fn attention_cross_process_drain_conflicts_are_not_reported_as_server_failures() {
        let response = attention_maintenance_error(anyhow::anyhow!(
            "timed out draining other attention-learning processes after 120000 ms"
        ));
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn routes_require_scope_and_exact_confirmation() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;

        let missing_scope = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/magician/v2/storage")
                .to_request(),
        )
        .await;
        assert_eq!(missing_scope.status(), StatusCode::BAD_REQUEST);

        let bad_confirmation = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/compact-parquet")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({ "confirmation": "yes" }))
                .to_request(),
        )
        .await;
        assert_eq!(bad_confirmation.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn maintenance_status_is_scoped_no_store_and_does_not_open_databases() {
        let temporary = tempfile::tempdir().unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;
        let denied = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/magician/v2/storage/maintenance")
                .to_request(),
        )
        .await;
        assert!(!denied.status().is_success());
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/storage/maintenance")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "private, no-store"
        );
        let rows: Vec<
            magician_comms::channel_assist::governance::automatic::DatabaseMaintenanceStatus,
        > = test::read_body_json(response).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.state == "idle"));
    }

    #[actix_web::test]
    async fn snapshot_is_typed_and_scope_bound() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/storage")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let snapshot: magician::magician_v2::storage_governance::StorageSnapshot =
            test::call_and_read_body_json(&app, request).await;
        assert_eq!(snapshot.principal, "owner");
        assert_eq!(snapshot.workspace, "default");
        for id in [
            "analytics_duckdb",
            "channel_assist_duckdb",
            "ui_threads_duckdb",
            "feed_duckdb",
            "attention_learning_sqlite",
            "attention_funnel_sqlite",
            "resurfacing_sqlite",
            "events",
            "memory_events",
            "activity_rows",
            "activity_rollups",
            "transport_log_jsonl",
            "llm_calls",
            "llm_embeddings",
            "llm_provider_attempts",
            "llm_tool_calls",
            "llm_capture_gaps",
            "llm_dispatch",
            "llm_call_io",
            "llm_context_blocks",
            "llm_content_tombstones",
            "llm_content_access_audit",
            "llm_trace_journal",
            "llm_restricted_journal",
            "compaction_metrics",
        ] {
            assert!(
                snapshot.entries.iter().any(|entry| entry.id == id),
                "inventory is missing {id}"
            );
        }
        assert!(snapshot
            .entries
            .iter()
            .any(|entry| entry.id == "llm_tool_calls" && entry.retention_days == Some(90)));
        // The spine is the one dataset with two tiers, and the whole point of
        // the tiering is that they expire on different clocks. If these two
        // ever report the same window, one of them is not doing its job.
        assert!(snapshot
            .entries
            .iter()
            .any(|entry| entry.id == "activity_rows" && entry.retention_days == Some(7)));
        assert!(snapshot
            .entries
            .iter()
            .any(|entry| entry.id == "activity_rollups" && entry.retention_days == Some(396)));
        assert_eq!(snapshot.compaction_metrics.event_count, 0);
    }

    #[actix_web::test]
    async fn compaction_metrics_clear_requires_exact_confirmation_and_only_clears_history() {
        use magician::magician_v2::storage_governance::compaction_metrics::{
            self, CompactionMetricEvent, CompactionMetricKind, CompactionMetricTrigger,
        };

        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let protected = layout.scope_root("owner", "default").join("protected.txt");
        layout
            .write_path_sync(&protected, b"keep")
            .expect("protected fixture");
        let mut event = CompactionMetricEvent::new(
            CompactionMetricTrigger::Manual,
            CompactionMetricKind::Parquet,
            "events",
            100,
        );
        event.files_compacted = 3;
        compaction_metrics::append_events(&layout, "owner", "default", [event])
            .expect("seed metrics");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;

        let bad = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/clear-compaction-metrics?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({ "confirmation": "CLEAR EVERYTHING" }))
                .to_request(),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        let request = test::TestRequest::post()
            .uri("/api/magician/v2/storage/actions/clear-compaction-metrics?workspace=default")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .set_json(serde_json::json!({ "confirmation": "CLEAR COMPACTION METRICS" }))
            .to_request();
        let cleared: compaction_metrics::CompactionMetricsSnapshot =
            test::call_and_read_body_json(&app, request).await;
        assert_eq!(cleared.event_count, 0);
        assert_eq!(
            std::fs::read(protected).expect("protected remains"),
            b"keep"
        );
    }

    #[actix_web::test]
    async fn database_action_compacts_each_store_owner_and_deduplicates_targets() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/storage/actions/compact-databases?workspace=default")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .set_json(serde_json::json!({
                "targets": ["ui_threads", "analytics", "channel_assist", "analytics"],
                "confirmation": "COMPACT DATABASE"
            }))
            .to_request();
        let report: magician::magician_v2::storage_governance::StorageMaintenanceReport =
            test::call_and_read_body_json(&app, request).await;
        assert_eq!(report.duckdb.len(), 3);
        assert!(report.completed_at_ms >= report.started_at_ms);
        assert!(report
            .duckdb
            .iter()
            .all(|database| database.bytes_after > 0));
        let snapshot_request = test::TestRequest::get()
            .uri("/api/magician/v2/storage?workspace=default")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let snapshot: magician::magician_v2::storage_governance::StorageSnapshot =
            test::call_and_read_body_json(&app, snapshot_request).await;
        assert_eq!(snapshot.compaction_metrics.event_count, 3);
        assert_eq!(snapshot.compaction_metrics.areas.len(), 3);
        assert!(snapshot
            .compaction_metrics
            .recent_events
            .iter()
            .all(|event| event.trigger
                == magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricTrigger::Manual));
    }

    #[actix_web::test]
    async fn retention_rejects_widened_payloads_and_unsafe_windows() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;
        let unknown_field = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/apply-retention?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "retention_days": 90,
                    "confirmation": "APPLY RETENTION",
                    "delete_mail": true
                }))
                .to_request(),
        )
        .await;
        assert_eq!(unknown_field.status(), StatusCode::BAD_REQUEST);

        let short_window = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/apply-retention?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "retention_days": 1,
                    "confirmation": "APPLY RETENTION"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(short_window.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn attention_actions_keep_online_optimization_cleanup_and_reclaim_separate() {
        use magician_comms::channel_assist::governance::{
            AttentionLearningMaintenanceOperation, AttentionLearningMaintenanceReport,
        };

        let temporary = tempfile::tempdir().expect("tempdir");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service(temporary.path())))
                .service(web::scope("/api/magician/v2").configure(configure)),
        )
        .await;

        let preview: AttentionLearningMaintenanceReport = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/attention-learning/retention-preview?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({ "retention_days": 90 }))
                .to_request(),
        )
        .await;
        assert_eq!(
            preview.operation,
            AttentionLearningMaintenanceOperation::RetentionPreview
        );
        assert_eq!(
            preview.retention.as_ref().map(|value| value.apply),
            Some(false)
        );

        let missing_confirmation = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/attention-learning/retention-apply?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({ "retention_days": 90 }))
                .to_request(),
        )
        .await;
        assert_eq!(missing_confirmation.status(), StatusCode::BAD_REQUEST);

        let cleaned: AttentionLearningMaintenanceReport = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/attention-learning/retention-apply?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "retention_days": 90,
                    "confirmation": "CLEAN ATTENTION HISTORY"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(
            cleaned.operation,
            AttentionLearningMaintenanceOperation::RetentionApply
        );
        assert_eq!(
            cleaned.retention.as_ref().map(|value| value.apply),
            Some(true)
        );

        let optimized: AttentionLearningMaintenanceReport = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/magician/v2/storage/actions/attention-learning/optimize?workspace=default")
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({ "confirmation": "OPTIMIZE ATTENTION" }))
                .to_request(),
        )
        .await;
        assert_eq!(
            optimized.operation,
            AttentionLearningMaintenanceOperation::Optimize
        );
        assert!(optimized.optimize.is_some());

        let reclaimed: AttentionLearningMaintenanceReport = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri(
                    "/api/magician/v2/storage/actions/attention-learning/reclaim?workspace=default",
                )
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "confirmation": "RECLAIM ATTENTION DATABASE"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(
            reclaimed.operation,
            AttentionLearningMaintenanceOperation::Reclaim
        );
        assert_eq!(
            reclaimed
                .reclaim
                .as_ref()
                .map(|value| value.integrity_check.as_str()),
            Some("ok")
        );
    }
}
