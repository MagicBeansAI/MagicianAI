use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::Deserialize;

use magician::magician_v2::{
    artifact_v2::{ArtifactV2Service, ScopeRef},
    ask_loop::AskLoopApi,
    execution::agentic::FullPauseStore,
    execution::ScreenshotStorage,
    progress_channel_seam::event_log::EventLog,
};

use crate::scope::resolve_required_scope;

#[derive(Debug, Deserialize)]
pub struct TaskExecutionPanelQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub execution_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExecutionPanelQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Clone)]
pub struct ExecutionPanelApi {
    v3_adapter: V3ExecutionPanelAdapter,
}

impl ExecutionPanelApi {
    pub fn new(
        service: Arc<ArtifactV2Service>,
        screenshot_storage: Arc<ScreenshotStorage>,
        ask_loop_api: Arc<AskLoopApi>,
        runtime_store: ExecutionPanelRuntimeStore,
        pause_store: Arc<FullPauseStore>,
        event_log: EventLog,
    ) -> Self {
        Self {
            v3_adapter: V3ExecutionPanelAdapter::new(service)
                .with_runtime_support(screenshot_storage, ask_loop_api, runtime_store)
                .with_pause_store(pause_store)
                .with_progress_event_log(event_log),
        }
    }

    pub async fn get_task_panel_state(
        &self,
        req: &HttpRequest,
        task_id: &str,
        query: TaskExecutionPanelQuery,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let scope =
            ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
        match self
            .v3_adapter
            .get_task_panel_state(&scope, task_id, query.execution_id.as_deref())
            .await
        {
            Ok(Some(state)) => return respond(Ok(Some(state))),
            Ok(None) => {},
            Err(error) => return respond(Err(error)),
        }
        respond(Ok(None))
    }

    pub async fn get_execution_panel_state(
        &self,
        req: &HttpRequest,
        execution_id: &str,
        query: ExecutionPanelQuery,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let scope =
            ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
        match self
            .v3_adapter
            .get_execution_panel_state(&scope, execution_id)
            .await
        {
            Ok(Some(state)) => return respond(Ok(Some(state))),
            Ok(None) => {},
            Err(error) => return respond(Err(error)),
        }
        respond(Ok(None))
    }
}

fn respond(result: anyhow::Result<Option<ExecutionPanelState>>) -> Result<HttpResponse> {
    match result {
        Ok(Some(state)) => Ok(HttpResponse::Ok().json(state)),
        Ok(None) => Ok(HttpResponse::NotFound().json(serde_json::json!({
            "error": "execution panel state not found"
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("{error}")
        }))),
    }
}

pub async fn get_task_execution_panel_handler(
    api: web::Data<Arc<ExecutionPanelApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TaskExecutionPanelQuery>,
) -> Result<HttpResponse> {
    api.get_task_panel_state(&req, &path.into_inner(), query.into_inner())
        .await
}

pub async fn get_execution_panel_handler(
    api: web::Data<Arc<ExecutionPanelApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExecutionPanelQuery>,
) -> Result<HttpResponse> {
    api.get_execution_panel_state(&req, &path.into_inner(), query.into_inner())
        .await
}

use magician_learning::execution_panel::{
    ExecutionPanelRuntimeStore, ExecutionPanelState, V3ExecutionPanelAdapter,
};
