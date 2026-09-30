//! Integration tests for MagicianV2 API endpoints
//!
//! Tests the complete V2 API including:
//! - Execution CRUD operations
//! - Turn management with pagination
//! - Direction filtering
//! - Error handling
//!
//! Run with: cargo test magician_v2_api_integration --test-threads=1

#[path = "support/v2_api_harness.rs"]
mod harness;
#[allow(unused_imports)]
use harness::*;

use std::time::{Duration, Instant};

use actix_web::{test, web, App, HttpRequest};
use magician::magician_v2::{agents::AgentStorage, storage::TurnDirection};
use magician_api::{
    web_api::{get_approval_handler, list_approvals_handler, respond_hitl_handler},
    {
        create_agent_definition_handler, delete_agent_definition_handler,
        get_agent_definition_handler, list_agent_definitions_handler, manual_trigger_agent_handler,
        pause_agent_handler, resume_agent_handler, update_agent_definition_handler, MagicianV2Api,
    },
};
use serde_json::{json, Value};

// ============================================================================
// Execution CRUD Tests
// ============================================================================

#[tokio::test]
async fn test_create_execution_success() {
    let api = create_test_v2_api().await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions",
        web::post().to(
            |api: web::Data<MagicianV2Api>,
             http_req: HttpRequest,
             req: web::Json<magician_api::CreateExecutionRequest>| async move {
                api.create_execution(&http_req, req).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::post()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .set_json(json!({
            "principal": "test-user",
            "workspace": "test-workspace"
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert!(body.get("execution_id").is_some());
    assert_eq!(body["execution"]["principal"], "test-user");
    assert_eq!(body["execution"]["workspace"], "test-workspace");
}

#[tokio::test]
async fn test_create_execution_with_title() {
    let api = create_test_v2_api().await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions",
        web::post().to(
            |api: web::Data<MagicianV2Api>,
             http_req: HttpRequest,
             req: web::Json<magician_api::CreateExecutionRequest>| async move {
                api.create_execution(&http_req, req).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::post()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .set_json(json!({
            "title": "Test Conversation"
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["execution"]["title"], "Test Conversation");
    assert_eq!(body["execution"]["principal"], "test-user");
    assert_eq!(body["execution"]["workspace"], "test-workspace");
}

#[tokio::test]
async fn test_post_message_skip_planning_returns_accepted() {
    let api = create_test_v2_api().await;

    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/executions",
                web::post().to(
                    |api: web::Data<MagicianV2Api>,
                     http_req: HttpRequest,
                     req: web::Json<magician_api::CreateExecutionRequest>| async move {
                        api.create_execution(&http_req, req).await
                    },
                ),
            )
            .route(
                "/api/magician/v2/executions/{id}/message",
                web::post().to(
                    |api: web::Data<MagicianV2Api>,
                     path: web::Path<String>,
                     req: web::Json<magician_api::MagicianV2PostMessageRequest>| async move {
                        api.post_message_v2(path, req, None).await
                    },
                ),
            ),
    )
    .await;

    let create_req = test::TestRequest::post()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .set_json(serde_json::json!({}))
        .to_request();
    let create_resp = test::call_service(&app, create_req).await;
    let create_status = create_resp.status();
    let create_body: Value = test::read_body_json(create_resp).await;
    assert!(
        create_status.is_success(),
        "create execution should succeed: status={} body={}",
        create_status,
        create_body
    );
    let execution_id = create_body["execution_id"]
        .as_str()
        .expect("missing execution id");

    let msg_req = test::TestRequest::post()
        .uri(&format!(
            "/api/magician/v2/executions/{}/message",
            execution_id
        ))
        .set_json(serde_json::json!({
            "text": "Open https://example.com and report the title",
            "skip_planning": true
        }))
        .to_request();

    let msg_resp = test::call_service(&app, msg_req).await;
    assert_eq!(msg_resp.status(), actix_web::http::StatusCode::ACCEPTED);

    let body: Value = test::read_body_json(msg_resp).await;
    assert_eq!(body["execution_id"], execution_id);
    assert_eq!(body["skip_planning"], true);
}

#[tokio::test]
async fn test_start_execution_returns_accepted() {
    let api = create_test_v2_api().await;

    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/executions",
                web::post().to(
                    |api: web::Data<MagicianV2Api>,
                     http_req: HttpRequest,
                     req: web::Json<magician_api::CreateExecutionRequest>| async move {
                        api.create_execution(&http_req, req).await
                    },
                ),
            )
            .route(
                "/api/magician/v2/executions/{id}/start",
                web::post().to(
                    |api: web::Data<MagicianV2Api>,
                     path: web::Path<String>,
                     req: web::Json<magician_api::StartExecutionRequest>| async move {
                        api.start_execution(path, req).await
                    },
                ),
            ),
    )
    .await;

    let create_req = test::TestRequest::post()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .set_json(serde_json::json!({}))
        .to_request();
    let create_resp = test::call_service(&app, create_req).await;
    let create_status = create_resp.status();
    let create_body: Value = test::read_body_json(create_resp).await;
    assert!(
        create_status.is_success(),
        "create execution should succeed: status={} body={}",
        create_status,
        create_body
    );
    let execution_id = create_body["execution_id"]
        .as_str()
        .expect("missing execution id");

    let start_req = test::TestRequest::post()
        .uri(&format!(
            "/api/magician/v2/executions/{}/start",
            execution_id
        ))
        .set_json(serde_json::json!({
            "goal": "Open https://example.com and report the title"
        }))
        .to_request();

    let start_resp = test::call_service(&app, start_req).await;
    assert_eq!(start_resp.status(), actix_web::http::StatusCode::ACCEPTED);

    let body: Value = test::read_body_json(start_resp).await;
    assert_eq!(body["execution_id"], execution_id);
    assert_eq!(body["skip_planning"], true);
}

#[tokio::test]
async fn test_list_executions_empty() {
    let api = create_test_v2_api().await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             http_req: HttpRequest,
             query: web::Query<magician_api::ListExecutionsQuery>| async move {
                api.list_executions(&http_req, query).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["total"], 0);
    assert_eq!(body["pagination"]["has_more"], false);
}

// Continue with remaining tests...
// (Due to length, showing structure - full implementation continues similarly)

#[tokio::test]
async fn test_list_executions_with_data() {
    let api = create_test_v2_api().await;

    // Create multiple executions under an explicit test scope.
    let store = api.orchestrator().get_conversation_store();

    let _execution1 = create_task_backed_execution(
        &store,
        "test-user",
        "test-workspace",
        Some("Execution 1".to_string()),
    )
    .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    let _execution2 = create_task_backed_execution(
        &store,
        "test-user",
        "test-workspace",
        Some("Execution 2".to_string()),
    )
    .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    // Execution 3 in a different workspace should not be visible via the scoped API call.
    let _execution3 = create_task_backed_execution(
        &store,
        "test-user",
        "other-workspace",
        Some("Execution 3".to_string()),
    )
    .await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             http_req: HttpRequest,
             query: web::Query<magician_api::ListExecutionsQuery>| async move {
                api.list_executions(&http_req, query).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);

    // Verify sorting (newest first)
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["title"], "Execution 2");
    assert_eq!(items[1]["title"], "Execution 1");
}

#[tokio::test]
async fn test_list_executions_pagination() {
    let api = create_test_v2_api().await;

    // Create 5 executions under an explicit test scope.
    let store = api.orchestrator().get_conversation_store();

    for i in 1..=5 {
        create_task_backed_execution(
            &store,
            "test-user",
            "test-workspace",
            Some(format!("Execution {}", i)),
        )
        .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             http_req: HttpRequest,
             query: web::Query<magician_api::ListExecutionsQuery>| async move {
                api.list_executions(&http_req, query).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions?limit=2&offset=0")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 5);
    assert_eq!(body["pagination"]["limit"], 2);
    assert_eq!(body["pagination"]["offset"], 0);
    assert_eq!(body["pagination"]["has_more"], true);

    // Second page (limit=2, offset=2)
    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions?limit=2&offset=2")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["has_more"], true);

    // Last page (limit=2, offset=4)
    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions?limit=2&offset=4")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "test-workspace"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["pagination"]["has_more"], false);
}

#[tokio::test]
async fn test_get_execution_success() {
    let api = create_test_v2_api().await;

    // Create an execution
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(
        &store,
        "test-user",
        "workspace-1",
        Some("Test Execution".to_string()),
    )
    .await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}",
        web::get().to(
            |api: web::Data<MagicianV2Api>, path: web::Path<String>| async move {
                api.get_execution(path).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri(&format!("/api/magician/v2/executions/{}", execution.id))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], execution.id);
    assert_eq!(body["title"], "Test Execution");
    assert_eq!(body["principal"], "test-user");
    assert_eq!(body["workspace"], "workspace-1");
}

#[tokio::test]
async fn test_get_execution_not_found() {
    let api = create_test_v2_api().await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}",
        web::get().to(
            |api: web::Data<MagicianV2Api>, path: web::Path<String>| async move {
                api.get_execution(path).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri("/api/magician/v2/executions/nonexistent-execution-id")
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_delete_execution_success() {
    let api = create_test_v2_api().await;

    // Create an execution
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(
        &store,
        "test-user",
        "workspace-1",
        Some("Test Execution".to_string()),
    )
    .await;

    let app =
        test::init_service(
            App::new().app_data(api.clone()).route(
                "/api/magician/v2/executions/{id}",
                web::delete().to(
                    |api: web::Data<MagicianV2Api>,
                     http_req: HttpRequest,
                     path: web::Path<String>| async move {
                        api.delete_execution(&http_req, path).await
                    },
                ),
            ),
        )
        .await;

    let req = test::TestRequest::delete()
        .uri(&format!("/api/magician/v2/executions/{}", execution.id))
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "workspace-1"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);

    // Verify execution is deleted
    let result = store.get_execution(&execution.id).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_execution_not_found() {
    let api = create_test_v2_api().await;

    let app =
        test::init_service(
            App::new().app_data(api.clone()).route(
                "/api/magician/v2/executions/{id}",
                web::delete().to(
                    |api: web::Data<MagicianV2Api>,
                     http_req: HttpRequest,
                     path: web::Path<String>| async move {
                        api.delete_execution(&http_req, path).await
                    },
                ),
            ),
        )
        .await;

    let req = test::TestRequest::delete()
        .uri("/api/magician/v2/executions/nonexistent-execution-id")
        .insert_header(("X-Principal", "test-user"))
        .insert_header(("X-Workspace", "workspace-1"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ============================================================================
// Turn Management Tests
// ============================================================================

#[tokio::test]
async fn test_list_turns_empty() {
    let api = create_test_v2_api().await;

    // Create an empty execution
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(&store, "test-user", "workspace-1", None).await;

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}/turns",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             path: web::Path<String>,
             query: web::Query<magician_api::ListTurnsQuery>| async move {
                api.list_turns(path, query).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["total"], 0);
    assert_eq!(body["pagination"]["has_more"], false);
}

#[tokio::test]
async fn test_list_turns_with_data() {
    let api = create_test_v2_api().await;

    // Create execution with multiple turns
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(&store, "test-user", "workspace-1", None).await;

    // Add inbound and outbound turns
    let _turn1 = store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "User message 1".to_string(),
            None,
        )
        .await
        .unwrap();
    let _turn2 = store
        .add_turn(
            &execution.id,
            TurnDirection::Outbound,
            "Assistant response 1".to_string(),
            None,
        )
        .await
        .unwrap();
    let _turn3 = store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "User message 2".to_string(),
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}/turns",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             path: web::Path<String>,
             query: web::Query<magician_api::ListTurnsQuery>| async move {
                api.list_turns(path, query).await
            },
        ),
    ))
    .await;

    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["pagination"]["total"], 3);

    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["text"], "User message 1");
    assert_eq!(items[1]["text"], "Assistant response 1");
    assert_eq!(items[2]["text"], "User message 2");
}

#[tokio::test]
async fn test_list_turns_pagination() {
    let api = create_test_v2_api().await;

    // Create execution with 7 turns
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(&store, "test-user", "workspace-1", None).await;

    for i in 1..=7 {
        let direction = if i % 2 == 1 {
            TurnDirection::Inbound
        } else {
            TurnDirection::Outbound
        };
        store
            .add_turn(&execution.id, direction, format!("Turn {}", i), None)
            .await
            .unwrap();
    }

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}/turns",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             path: web::Path<String>,
             query: web::Query<magician_api::ListTurnsQuery>| async move {
                api.list_turns(path, query).await
            },
        ),
    ))
    .await;

    // First page (limit=3, offset=0)
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns?limit=3&offset=0",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["pagination"]["total"], 7);
    assert_eq!(body["pagination"]["limit"], 3);
    assert_eq!(body["pagination"]["offset"], 0);
    assert_eq!(body["pagination"]["has_more"], true);

    // Second page (limit=3, offset=3)
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns?limit=3&offset=3",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["pagination"]["has_more"], true);

    // Last page (limit=3, offset=6)
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns?limit=3&offset=6",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["pagination"]["has_more"], false);
}

#[tokio::test]
async fn test_list_turns_direction_filter() {
    let api = create_test_v2_api().await;

    // Create execution with mixed turns
    let store = api.orchestrator().get_conversation_store();

    let execution = create_task_backed_execution(&store, "test-user", "workspace-1", None).await;

    // Add 3 inbound and 2 outbound turns
    store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "User 1".to_string(),
            None,
        )
        .await
        .unwrap();
    store
        .add_turn(
            &execution.id,
            TurnDirection::Outbound,
            "Assistant 1".to_string(),
            None,
        )
        .await
        .unwrap();
    store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "User 2".to_string(),
            None,
        )
        .await
        .unwrap();
    store
        .add_turn(
            &execution.id,
            TurnDirection::Outbound,
            "Assistant 2".to_string(),
            None,
        )
        .await
        .unwrap();
    store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "User 3".to_string(),
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/executions/{id}/turns",
        web::get().to(
            |api: web::Data<MagicianV2Api>,
             path: web::Path<String>,
             query: web::Query<magician_api::ListTurnsQuery>| async move {
                api.list_turns(path, query).await
            },
        ),
    ))
    .await;

    // Filter Inbound only
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns?direction=Inbound",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["pagination"]["total"], 3);

    let items = body["items"].as_array().unwrap();
    for item in items {
        assert_eq!(item["direction"], "Inbound");
    }

    // Filter Outbound only
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/magician/v2/executions/{}/turns?direction=Outbound",
            execution.id
        ))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);

    let items = body["items"].as_array().unwrap();
    for item in items {
        assert_eq!(item["direction"], "Outbound");
    }
}

fn sample_agent_payload(agent_id: &str, name: &str) -> Value {
    json!({
        "agent_id": agent_id,
        "name": name,
        "persona": "Test persona",
        "principal": INTEGRATION_PRINCIPAL,
        "workspace": INTEGRATION_WORKSPACE,
        "tools": []
    })
}

#[tokio::test]
async fn test_agent_routes_crud_and_pagination() {
    let api = create_test_v2_api().await;
    let agent_a = format!("agent-{}", uuid::Uuid::new_v4().simple());
    let agent_b = format!("agent-{}", uuid::Uuid::new_v4().simple());
    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents",
                web::get().to(list_agent_definitions_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}",
                web::get().to(get_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}",
                web::put().to(update_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}",
                web::delete().to(delete_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/pause",
                web::post().to(pause_agent_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/resume",
                web::post().to(resume_agent_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/trigger",
                web::post().to(manual_trigger_agent_handler),
            ),
    )
    .await;

    let create_a = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(sample_agent_payload(&agent_a, "Alpha"))
        .to_request();
    let create_a_resp = test::call_service(&app, create_a).await;
    assert_eq!(create_a_resp.status(), actix_web::http::StatusCode::CREATED);

    let create_b = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(sample_agent_payload(&agent_b, "Bravo"))
        .to_request();
    let create_b_resp = test::call_service(&app, create_b).await;
    assert_eq!(create_b_resp.status(), actix_web::http::StatusCode::CREATED);

    let list_req = with_integration_scope(test::TestRequest::get())
        .uri("/api/magician/v2/agents?offset=1&limit=1")
        .to_request();
    let list_resp = test::call_service(&app, list_req).await;
    assert_eq!(list_resp.status(), actix_web::http::StatusCode::OK);
    let list_body: Value = test::read_body_json(list_resp).await;
    assert_eq!(list_body["agents"].as_array().unwrap().len(), 1);

    let get_req = with_integration_scope(test::TestRequest::get())
        .uri(&format!("/api/magician/v2/agents/{agent_a}"))
        .to_request();
    let get_resp = test::call_service(&app, get_req).await;
    assert_eq!(get_resp.status(), actix_web::http::StatusCode::OK);
    let etag = get_resp
        .headers()
        .get(actix_web::http::header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let update_req = with_integration_scope(test::TestRequest::put())
        .uri(&format!("/api/magician/v2/agents/{agent_a}"))
        .insert_header((actix_web::http::header::IF_MATCH, etag))
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
        .set_payload(serde_json::to_vec(&sample_agent_payload(&agent_a, "Alpha v2")).unwrap())
        .to_request();
    let update_resp = test::call_service(&app, update_req).await;
    assert_eq!(update_resp.status(), actix_web::http::StatusCode::OK);

    let delete_req = with_integration_scope(test::TestRequest::delete())
        .uri(&format!("/api/magician/v2/agents/{agent_a}"))
        .to_request();
    let delete_resp = test::call_service(&app, delete_req).await;
    assert_eq!(
        delete_resp.status(),
        actix_web::http::StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn test_agent_update_route_requires_if_match() {
    let api = create_test_v2_api().await;
    let agent_id = format!("agent-{}", uuid::Uuid::new_v4().simple());
    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}",
                web::put().to(update_agent_definition_handler),
            ),
    )
    .await;

    let create = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(sample_agent_payload(&agent_id, "Alpha"))
        .to_request();
    let create_resp = test::call_service(&app, create).await;
    assert_eq!(create_resp.status(), actix_web::http::StatusCode::CREATED);

    let update_without_if_match = with_integration_scope(test::TestRequest::put())
        .uri(&format!("/api/magician/v2/agents/{agent_id}"))
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
        .set_payload(serde_json::to_vec(&sample_agent_payload(&agent_id, "Alpha v2")).unwrap())
        .to_request();
    let resp = test::call_service(&app, update_without_if_match).await;
    assert_eq!(
        resp.status(),
        actix_web::http::StatusCode::PRECONDITION_REQUIRED
    );
}

#[tokio::test]
async fn test_agent_create_route_rejects_malformed_json_via_extractor() {
    let api = create_test_v2_api().await;
    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/agents",
        web::post().to(create_agent_definition_handler),
    ))
    .await;

    let req = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
        .set_payload(br#"{"agent_id":"agent-a""#.as_slice())
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_agent_list_route_rejects_non_numeric_limit_query() {
    let api = create_test_v2_api().await;
    let app = test::init_service(App::new().app_data(api.clone()).route(
        "/api/magician/v2/agents",
        web::get().to(list_agent_definitions_handler),
    ))
    .await;

    let req = with_integration_scope(test::TestRequest::get())
        .uri("/api/magician/v2/agents?limit=oops")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_manual_trigger_route_rejects_unknown_fields_via_extractor() {
    let api = create_test_v2_api().await;
    let agent_id = format!("agent-{}", uuid::Uuid::new_v4().simple());
    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/trigger",
                web::post().to(manual_trigger_agent_handler),
            ),
    )
    .await;

    let create = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(sample_agent_payload(&agent_id, "Alpha"))
        .to_request();
    let create_resp = test::call_service(&app, create).await;
    assert_eq!(create_resp.status(), actix_web::http::StatusCode::CREATED);

    let trigger = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/agents/{agent_id}/trigger"))
        .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
        .set_payload(br#"{"goalId":"g1","trigger":"manual"}"#.as_slice())
        .to_request();
    let trigger_resp = test::call_service(&app, trigger).await;
    assert_eq!(
        trigger_resp.status(),
        actix_web::http::StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn test_p3_14_create_trigger_approval_resolve_episode_flow() {
    let api = create_test_v2_api().await;
    let agent_id = format!("agent-p314-{}", uuid::Uuid::new_v4().simple());
    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/trigger",
                web::post().to(manual_trigger_agent_handler),
            )
            .route(
                "/api/magician/v2/approvals",
                web::get().to(list_approvals_handler),
            )
            .route(
                "/api/magician/v2/approvals/{approval_id}",
                web::get().to(get_approval_handler),
            )
            .route(
                "/api/magician/v2/hitl/{correlation_id}/respond",
                web::post().to(respond_hitl_handler),
            ),
    )
    .await;

    let create_payload = json!({
        "agent_id": agent_id,
        "name": "P3-14 Agent",
        "persona": "Test persona",
        "principal": INTEGRATION_PRINCIPAL,
        "workspace": INTEGRATION_WORKSPACE,
        "tools": [],
        "trust_level": "reviewed",
        "constraints": {
            "approval_ttl_secs": 300,
            "requires_approval": [{
                "tool": "*",
                "action": "*",
                "ttl_secs": 300
            }]
        }
    });
    let create = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(create_payload)
        .to_request();
    let create_resp = test::call_service(&app, create).await;
    assert_eq!(create_resp.status(), actix_web::http::StatusCode::CREATED);

    let trigger = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/agents/{agent_id}/trigger"))
        .set_json(json!({
            "goal_id": "g1",
            "trigger": "manual"
        }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger).await;
    assert_eq!(trigger_resp.status(), actix_web::http::StatusCode::ACCEPTED);
    let trigger_body: Value = test::read_body_json(trigger_resp).await;
    let trigger_seq = trigger_body["trigger_seq"]
        .as_u64()
        .expect("trigger_seq should be returned");
    let cycle_id = trigger_body["cycle_id"]
        .as_str()
        .expect("cycle_id should be returned")
        .to_string();

    let pause_states_path = api.orchestrator().pause_states_storage_path();
    let memory_service = resolve_integration_memory_service(&pause_states_path);
    let approval_id = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let list = with_integration_scope(test::TestRequest::get())
                .uri(&format!(
                    "/api/magician/v2/approvals?status=pending&agent_id={agent_id}"
                ))
                .to_request();
            let list_resp = test::call_service(&app, list).await;
            assert_eq!(list_resp.status(), actix_web::http::StatusCode::OK);
            let list_body: Value = test::read_body_json(list_resp).await;
            let approvals = list_body["approvals"]
                .as_array()
                .expect("approvals should be returned");
            if let Some(approval) = approvals
                .iter()
                .find(|item| item["cycle_id"].as_str() == Some(cycle_id.as_str()))
            {
                break approval["approval_id"]
                    .as_str()
                    .expect("approval_id should be a string")
                    .to_string();
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for pending approval (agent_id={agent_id}, cycle_id={cycle_id})"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    let list = with_integration_scope(test::TestRequest::get())
        .uri(&format!(
            "/api/magician/v2/approvals?status=pending&agent_id={agent_id}"
        ))
        .to_request();
    let list_resp = test::call_service(&app, list).await;
    assert_eq!(list_resp.status(), actix_web::http::StatusCode::OK);
    let list_body: Value = test::read_body_json(list_resp).await;
    let approvals = list_body["approvals"]
        .as_array()
        .expect("approvals should be returned");
    let listed = approvals
        .iter()
        .find(|item| item["approval_id"].as_str() == Some(approval_id.as_str()))
        .expect("created approval should be listed");
    assert_eq!(listed["status"], "pending");

    let get_pending = with_integration_scope(test::TestRequest::get())
        .uri(&format!("/api/magician/v2/approvals/{}", approval_id))
        .to_request();
    let get_pending_resp = test::call_service(&app, get_pending).await;
    assert_eq!(get_pending_resp.status(), actix_web::http::StatusCode::OK);
    let get_pending_body: Value = test::read_body_json(get_pending_resp).await;
    assert_eq!(get_pending_body["request"]["approval_id"], approval_id);
    assert_eq!(get_pending_body["request"]["status"], "pending");

    let resolve = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/hitl/{}/respond", approval_id))
        .set_json(json!({
            "source": "approval",
            "value": {
                "type": "confirmation",
                "confirmed": false
            },
            "channel": "in_app"
        }))
        .to_request();
    let resolve_resp = test::call_service(&app, resolve).await;
    assert_eq!(resolve_resp.status(), actix_web::http::StatusCode::OK);
    let resolve_body: Value = test::read_body_json(resolve_resp).await;
    assert_eq!(resolve_body["resolved"], true);

    let get_rejected = with_integration_scope(test::TestRequest::get())
        .uri(&format!("/api/magician/v2/approvals/{}", approval_id))
        .to_request();
    let get_rejected_resp = test::call_service(&app, get_rejected).await;
    assert_eq!(get_rejected_resp.status(), actix_web::http::StatusCode::OK);
    let get_rejected_body: Value = test::read_body_json(get_rejected_resp).await;
    assert_eq!(get_rejected_body["request"]["status"], "rejected");
    assert_eq!(get_rejected_body["request"]["resolved_by"], "in_app");
    let episode = wait_for_episode_for_trigger(
        &memory_service,
        &agent_id,
        "g1",
        trigger_seq,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(episode.episode_id, cycle_id);
    assert_eq!(episode.outcome_kind, "paused");
    assert!(
        episode
            .outcome_pending_actions()
            .iter()
            .any(|action| action.contains("awaiting_confirmation")),
        "paused episode should capture confirmation wait state"
    );
}

#[tokio::test]
async fn test_p3_14_restart_resume_scheduler_behavior_via_api() {
    let agent_id = format!("agent-p314-restart-{}", uuid::Uuid::new_v4().simple());
    let shared_storage_path = unique_integration_storage_path();

    // Each simulated process lifecycle gets a distinct Actix system, and the
    // first system is fully dropped before the second is constructed. This
    // models a restart without retaining two non-Send service graphs inside one
    // generated async-test future and without a custom test-thread stack.
    let first_storage_path = shared_storage_path.clone();
    let first_agent_id = agent_id.clone();
    let trigger_seq_one = tokio::task::spawn_blocking(move || {
        actix_web::rt::System::new().block_on(create_agent_and_trigger_for_restart_test(
            &first_storage_path,
            &first_agent_id,
        ))
    })
    .await
    .expect("first API lifecycle should join");
    let second_storage_path = shared_storage_path.clone();
    let second_agent_id = agent_id.clone();
    let trigger_seq_two = tokio::task::spawn_blocking(move || {
        actix_web::rt::System::new().block_on(trigger_after_restart_for_restart_test(
            &second_storage_path,
            &second_agent_id,
        ))
    })
    .await
    .expect("restarted API lifecycle should join");

    assert_eq!(
        trigger_seq_two,
        trigger_seq_one + 1,
        "trigger sequence should continue across API restart"
    );
}

async fn create_agent_and_trigger_for_restart_test(storage_path: &str, agent_id: &str) -> u64 {
    let api_first = create_test_v2_api_with_storage_path(storage_path.to_string()).await;
    let app_first = test::init_service(
        App::new()
            .app_data(api_first.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/trigger",
                web::post().to(manual_trigger_agent_handler),
            ),
    )
    .await;

    let create = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(sample_agent_payload(agent_id, "P3-14 Restart Agent"))
        .to_request();
    let create_resp = test::call_service(&app_first, create).await;
    assert_eq!(create_resp.status(), actix_web::http::StatusCode::CREATED);

    let trigger_one = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/agents/{agent_id}/trigger"))
        .set_json(json!({
            "goal_id": "g1",
            "trigger": "manual"
        }))
        .to_request();
    let trigger_one_resp = test::call_service(&app_first, trigger_one).await;
    assert_eq!(
        trigger_one_resp.status(),
        actix_web::http::StatusCode::ACCEPTED
    );
    let trigger_one_body: Value = test::read_body_json(trigger_one_resp).await;
    trigger_one_body["trigger_seq"]
        .as_u64()
        .expect("first trigger_seq should be returned")
}

async fn trigger_after_restart_for_restart_test(storage_path: &str, agent_id: &str) -> u64 {
    let api_restarted = create_test_v2_api_with_storage_path(storage_path.to_string()).await;
    let app_restarted = test::init_service(App::new().app_data(api_restarted.clone()).route(
        "/api/magician/v2/agents/{id}/trigger",
        web::post().to(manual_trigger_agent_handler),
    ))
    .await;

    let trigger_two = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/agents/{agent_id}/trigger"))
        .set_json(json!({
            "goal_id": "g1",
            "trigger": "manual"
        }))
        .to_request();
    let trigger_two_resp = test::call_service(&app_restarted, trigger_two).await;
    assert_eq!(
        trigger_two_resp.status(),
        actix_web::http::StatusCode::ACCEPTED
    );
    let trigger_two_body: Value = test::read_body_json(trigger_two_resp).await;
    trigger_two_body["trigger_seq"]
        .as_u64()
        .expect("second trigger_seq should be returned")
}

#[tokio::test]
async fn test_p3_14_trust_deny_reason_persists_in_episode_storage() {
    let api = create_test_v2_api().await;
    let pause_states_path = api.orchestrator().pause_states_storage_path();
    let agent_storage =
        AgentStorage::new(resolve_integration_agent_runtime_root(&pause_states_path));
    deny_integration_bash_for_local_agents(&agent_storage);
    let agent_id = format!("agent-p314-trust-{}", uuid::Uuid::new_v4().simple());
    let app = test::init_service(
        App::new()
            .app_data(api.clone())
            .route(
                "/api/magician/v2/agents",
                web::post().to(create_agent_definition_handler),
            )
            .route(
                "/api/magician/v2/agents/{id}/trigger",
                web::post().to(manual_trigger_agent_handler),
            ),
    )
    .await;

    let create_payload = json!({
        "agent_id": agent_id,
        "name": "P3-14 Trust Deny Agent",
        "persona": "Test persona",
        "principal": INTEGRATION_PRINCIPAL,
        "workspace": INTEGRATION_WORKSPACE,
        // The definition validly declares the shell pack. This test's scoped
        // policy denies its canonical runtime action name (`bash`), allowing
        // admission to succeed while exercising the execution-time denial.
        "tools": ["shell"],
        "trust_level": "local"
    });
    let create = with_integration_scope(test::TestRequest::post())
        .uri("/api/magician/v2/agents")
        .set_json(create_payload)
        .to_request();
    let create_resp = test::call_service(&app, create).await;
    let create_status = create_resp.status();
    let create_body = test::read_body(create_resp).await;
    assert_eq!(
        create_status,
        actix_web::http::StatusCode::CREATED,
        "agent creation failed: {}",
        String::from_utf8_lossy(&create_body)
    );

    let trigger = with_integration_scope(test::TestRequest::post())
        .uri(&format!("/api/magician/v2/agents/{agent_id}/trigger"))
        .set_json(json!({
            "goal_id": "g1",
            "trigger": "manual"
        }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger).await;
    assert_eq!(trigger_resp.status(), actix_web::http::StatusCode::ACCEPTED);
    let trigger_body: Value = test::read_body_json(trigger_resp).await;
    let trigger_seq = trigger_body["trigger_seq"]
        .as_u64()
        .expect("trigger_seq should be returned");
    let cycle_id = trigger_body["cycle_id"]
        .as_str()
        .expect("cycle_id should be returned")
        .to_string();

    let memory_service = resolve_integration_memory_service(&pause_states_path);
    let episode = wait_for_episode_for_trigger(
        &memory_service,
        &agent_id,
        "g1",
        trigger_seq,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(episode.episode_id, cycle_id);
    assert_eq!(episode.outcome_kind, "failed");
    let error = episode.outcome_error_summary();
    assert!(
        error.contains("trust policy denied action"),
        "unexpected trust-deny failure reason: {error}"
    );
    // 0.8c canonicalizes the shell/bash lane to the `bash` action type
    // (builtin_action_types::canonical_decision_builtin_action_type +
    // autonomous_goal trust resolution), so the deny reason reports the
    // canonical `tool=bash`, not the pre-0.8c surface label `shell`.
    assert!(
        error.contains("tool=bash"),
        "unexpected trust tool: {error}"
    );
    assert!(
        error.contains("action=execute"),
        "unexpected trust action: {error}"
    );
}
