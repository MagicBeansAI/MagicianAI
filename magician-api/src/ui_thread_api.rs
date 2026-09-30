use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};

use crate::chat_api::ChatApi;
use crate::scope::resolve_required_scope;
use magician::magician_v2::chat::{models::ChatSession, storage::ChatSessionSearchCandidate};
use magician::magician_v2::history::{infer_legacy_history_lane, HistoryLane};
use magician::magician_v2::ui_threads::{
    normalize_thread_id, UiThreadDetail, UiThreadRecord, UiThreadSearchCandidate, UiThreadService,
};

const MAX_HISTORY_SEARCH_OFFSET: usize = 100_000;

#[derive(Debug, Deserialize)]
pub struct UiThreadScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub history_lane: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUiThreadRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUiThreadRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub memory_text: Option<Option<String>>,
    /// Developer Mode toggle — accepted values are `"chat"` and `"dev"`.
    /// Any other string is rejected at the service layer with 400.
    /// Omitting the field leaves the existing mode untouched.
    #[serde(default)]
    pub display_mode: Option<String>,
    /// Plan-mode toggle for Developer Mode — when true, the autonomous
    /// executor gates each non-read-only action behind an approval
    /// escalation.
    #[serde(default)]
    pub plan_mode: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderUiThreadsRequest {
    pub ordered_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UiThreadListResponse {
    pub threads: Vec<UiThreadRecord>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Deserialize)]
pub struct HistorySearchQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistorySearchItem {
    Session {
        history_lane: HistoryLane,
        session: ChatSession,
    },
    Thread {
        history_lane: HistoryLane,
        thread: UiThreadRecord,
    },
}

#[derive(Debug, Serialize)]
pub struct HistorySearchResponse {
    pub items: Vec<HistorySearchItem>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Serialize)]
pub struct UiThreadDetailResponse {
    pub thread: UiThreadDetail,
}

#[derive(Clone)]
pub struct UiThreadApi {
    service: UiThreadService,
}

impl UiThreadApi {
    pub fn new(service: UiThreadService) -> Self {
        Self { service }
    }

    async fn search_thread_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> anyhow::Result<Vec<UiThreadSearchCandidate>> {
        self.service
            .search_thread_candidates(principal, workspace, search)
            .await
    }

    async fn get_materialized_thread(
        &self,
        principal: &str,
        workspace: &str,
        thread_id: &str,
    ) -> anyhow::Result<Option<UiThreadRecord>> {
        self.service
            .get_materialized_thread(principal, workspace, thread_id)
            .await
    }

    pub async fn list_threads(
        &self,
        req: &HttpRequest,
        query: UiThreadScopeQuery,
    ) -> Result<HttpResponse> {
        let lane = match query.history_lane.as_deref() {
            Some(value) => match HistoryLane::parse_filter(value) {
                Some(lane) => Some(lane),
                None => {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "history_lane must be 'personal' or 'automated'"
                    })));
                },
            },
            None => None,
        };
        let search = query.q.as_deref().unwrap_or("").trim();
        if search.chars().count() > 120 {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "q must be at most 120 characters"
            })));
        }
        let paged =
            query.limit.is_some() || query.offset.is_some() || lane.is_some() || !search.is_empty();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        if !paged {
            return match self.service.list_threads(&principal, &workspace).await {
                Ok(threads) => {
                    let total = threads.len();
                    Ok(HttpResponse::Ok().json(UiThreadListResponse {
                        threads,
                        total,
                        limit: total,
                        offset: 0,
                    }))
                },
                Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                }))),
            };
        }
        let limit = query.limit.unwrap_or(20).clamp(1, 100);
        let offset = query.offset.unwrap_or(0);
        match self
            .service
            .list_threads_page(&principal, &workspace, lane, search, limit, offset)
            .await
        {
            Ok(page) => Ok(HttpResponse::Ok().json(UiThreadListResponse {
                threads: page.threads,
                total: page.total,
                limit: page.limit,
                offset: page.offset,
            })),
            Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }

    pub async fn get_thread(
        &self,
        req: &HttpRequest,
        id: &str,
        query: UiThreadScopeQuery,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self.service.get_thread(&principal, &workspace, id).await {
            Ok(Some(thread)) => Ok(HttpResponse::Ok().json(UiThreadDetailResponse { thread })),
            Ok(None) => Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "thread not found"
            }))),
            Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }

    pub async fn create_thread(
        &self,
        req: &HttpRequest,
        query: UiThreadScopeQuery,
        body: CreateUiThreadRequest,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let thread_id = match body.id {
            Some(id) => normalize_thread_id(&id),
            None => normalize_thread_id(&slugify_thread_name(&body.name)),
        };
        let thread_id = match thread_id {
            Ok(id) => id,
            Err(error) => {
                return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        match self
            .service
            .create_thread(&principal, &workspace, &thread_id, Some(&body.name))
            .await
        {
            Ok(thread) => Ok(HttpResponse::Ok().json(thread)),
            Err(error) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }

    pub async fn update_thread(
        &self,
        req: &HttpRequest,
        id: &str,
        query: UiThreadScopeQuery,
        body: UpdateUiThreadRequest,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self
            .service
            .update_thread(
                &principal,
                &workspace,
                id,
                body.name,
                body.archived,
                body.memory_text,
                body.display_mode,
                body.plan_mode,
            )
            .await
        {
            Ok(Some(thread)) => Ok(HttpResponse::Ok().json(UiThreadDetailResponse { thread })),
            Ok(None) => Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "thread not found"
            }))),
            Err(error) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }

    pub async fn delete_thread(
        &self,
        req: &HttpRequest,
        id: &str,
        query: UiThreadScopeQuery,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self.service.delete_thread(&principal, &workspace, id).await {
            Ok(true) => Ok(HttpResponse::Ok().json(serde_json::json!({
                "deleted": true,
                "id": id,
            }))),
            Ok(false) => Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "thread not found"
            }))),
            Err(error) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }

    pub async fn reorder_threads(
        &self,
        req: &HttpRequest,
        query: UiThreadScopeQuery,
        body: ReorderUiThreadsRequest,
    ) -> Result<HttpResponse> {
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self
            .service
            .reorder_threads(&principal, &workspace, body.ordered_ids)
            .await
        {
            Ok(threads) => {
                let total = threads.len();
                Ok(HttpResponse::Ok().json(UiThreadListResponse {
                    threads,
                    total,
                    limit: total,
                    offset: 0,
                }))
            },
            Err(error) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("{error}")
            }))),
        }
    }
}

fn slugify_thread_name(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .replace(|ch: char| !ch.is_ascii_alphanumeric(), "-")
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HistorySearchCandidate {
    Session(ChatSessionSearchCandidate),
    Thread(UiThreadSearchCandidate),
}

impl HistorySearchCandidate {
    fn updated_at(&self) -> i64 {
        match self {
            Self::Session(candidate) => candidate.updated_at,
            Self::Thread(candidate) => candidate.updated_at,
        }
    }

    fn stable_key(&self) -> (&'static str, &str) {
        match self {
            Self::Session(candidate) => ("session", &candidate.id),
            Self::Thread(candidate) => ("thread", &candidate.id),
        }
    }
}

fn merge_history_search_candidates(
    thread_candidates: Vec<UiThreadSearchCandidate>,
    session_candidates: Vec<ChatSessionSearchCandidate>,
) -> Vec<HistorySearchCandidate> {
    let mut candidates = Vec::with_capacity(
        thread_candidates
            .len()
            .saturating_add(session_candidates.len()),
    );
    candidates.extend(
        session_candidates
            .into_iter()
            .map(HistorySearchCandidate::Session),
    );
    candidates.extend(
        thread_candidates
            .into_iter()
            .map(HistorySearchCandidate::Thread),
    );
    candidates.sort_by(|left, right| {
        right
            .updated_at()
            .cmp(&left.updated_at())
            .then_with(|| left.stable_key().cmp(&right.stable_key()))
    });
    candidates
}

fn session_search_item(mut session: ChatSession) -> HistorySearchItem {
    session.normalize_history_lane();
    HistorySearchItem::Session {
        history_lane: session.history_lane,
        session,
    }
}

fn thread_search_item(mut thread: UiThreadRecord) -> HistorySearchItem {
    let history_lane = match thread.history_lane {
        HistoryLane::Legacy => infer_legacy_history_lane(&thread.id),
        lane => lane,
    };
    thread.history_lane = history_lane;
    HistorySearchItem::Thread {
        history_lane,
        thread,
    }
}

async fn materialize_history_search_page(
    ui_api: &UiThreadApi,
    chat_api: &ChatApi,
    principal: &str,
    workspace: &str,
    candidates: Vec<HistorySearchCandidate>,
    limit: usize,
    offset: usize,
) -> anyhow::Result<HistorySearchResponse> {
    let total = candidates.len();
    let mut items = Vec::with_capacity(limit);
    for candidate in candidates.into_iter().skip(offset) {
        if items.len() >= limit {
            break;
        }
        match candidate {
            HistorySearchCandidate::Session(candidate) => {
                let Some(session) = chat_api.chat_service.get_session(&candidate.id).await? else {
                    continue;
                };
                if session.principal != principal || session.workspace != workspace {
                    continue;
                }
                items.push(session_search_item(session));
            },
            HistorySearchCandidate::Thread(candidate) => {
                let Some(thread) = ui_api
                    .get_materialized_thread(principal, workspace, &candidate.id)
                    .await?
                else {
                    continue;
                };
                items.push(thread_search_item(thread));
            },
        }
    }

    Ok(HistorySearchResponse {
        items,
        total,
        limit,
        offset,
    })
}

/// GET /api/magician/v2/history/search
///
/// Search user-created and product-generated threads and sessions as one
/// server-paginated result set. Browsing filters intentionally do not apply.
pub async fn search_history_handler(
    ui_api: web::Data<Arc<UiThreadApi>>,
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<HistorySearchQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let search = query.q.as_deref().unwrap_or_default().trim();
    if search.is_empty() {
        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "q is required"
        })));
    }
    if search.chars().count() > 120 {
        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "q must be at most 120 characters"
        })));
    }

    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    if offset > MAX_HISTORY_SEARCH_OFFSET {
        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!("offset must be at most {MAX_HISTORY_SEARCH_OFFSET}")
        })));
    }

    let candidates = tokio::try_join!(
        ui_api.search_thread_candidates(&principal, &workspace, search),
        chat_api
            .chat_service
            .search_session_candidates(&principal, &workspace, search),
    );
    match candidates {
        Ok((thread_candidates, session_candidates)) => {
            let candidates = merge_history_search_candidates(thread_candidates, session_candidates);
            match materialize_history_search_page(
                ui_api.get_ref().as_ref(),
                chat_api.get_ref(),
                &principal,
                &workspace,
                candidates,
                limit,
                offset,
            )
            .await
            {
                Ok(page) => Ok(HttpResponse::Ok().json(page)),
                Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "history search failed",
                    "details": error.to_string(),
                }))),
            }
        },
        Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "history search failed",
            "details": error.to_string(),
        }))),
    }
}

pub async fn list_ui_threads_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    query: web::Query<UiThreadScopeQuery>,
) -> Result<HttpResponse> {
    api.list_threads(&req, query.into_inner()).await
}

pub async fn get_ui_thread_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<UiThreadScopeQuery>,
) -> Result<HttpResponse> {
    api.get_thread(&req, &path.into_inner(), query.into_inner())
        .await
}

pub async fn create_ui_thread_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    query: web::Query<UiThreadScopeQuery>,
    body: web::Json<CreateUiThreadRequest>,
) -> Result<HttpResponse> {
    api.create_thread(&req, query.into_inner(), body.into_inner())
        .await
}

pub async fn update_ui_thread_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<UiThreadScopeQuery>,
    body: web::Json<UpdateUiThreadRequest>,
) -> Result<HttpResponse> {
    api.update_thread(
        &req,
        &path.into_inner(),
        query.into_inner(),
        body.into_inner(),
    )
    .await
}

pub async fn delete_ui_thread_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<UiThreadScopeQuery>,
) -> Result<HttpResponse> {
    api.delete_thread(&req, &path.into_inner(), query.into_inner())
        .await
}

pub async fn reorder_ui_threads_handler(
    api: web::Data<Arc<UiThreadApi>>,
    req: HttpRequest,
    query: web::Query<UiThreadScopeQuery>,
    body: web::Json<ReorderUiThreadsRequest>,
) -> Result<HttpResponse> {
    api.reorder_threads(&req, query.into_inner(), body.into_inner())
        .await
}

#[cfg(test)]
mod history_search_tests {
    use super::{
        merge_history_search_candidates, session_search_item, thread_search_item,
        HistorySearchCandidate, HistorySearchItem,
    };

    use magician::magician_v2::chat::{
        models::{ChatChannel, ChatSession, ChatSessionStatus},
        storage::ChatSessionSearchCandidate,
    };
    use magician::magician_v2::history::HistoryLane;
    use magician::magician_v2::ui_threads::{UiThreadRecord, UiThreadSearchCandidate};

    fn session(id: &str, title: &str, lane: HistoryLane, updated_at: i64) -> ChatSession {
        ChatSession {
            internal_voice: None,
            id: id.to_string(),
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            agent_id: "personal-assistant".to_string(),
            ui_thread_id: "general".to_string(),
            title: Some(title.to_string()),
            origin_channel: ChatChannel::web(),
            status: ChatSessionStatus::Active,
            history_lane: lane,
            is_default_session: false,
            created_at: 1,
            updated_at,
        }
    }

    fn thread(id: &str, name: &str, lane: HistoryLane, updated_at: i64) -> UiThreadRecord {
        UiThreadRecord {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            id: id.to_string(),
            name: name.to_string(),
            archived: false,
            sort_order: 0,
            memory_summary: None,
            memory_updated_at: None,
            created_at: 1,
            updated_at,
            history_lane: lane,
            display_mode: "chat".to_string(),
            plan_mode: false,
        }
    }

    #[test]
    fn mixed_history_search_orders_candidates_before_page_materialization() {
        let candidates = merge_history_search_candidates(
            vec![UiThreadSearchCandidate {
                id: "screens".to_string(),
                updated_at: 30,
            }],
            vec![
                ChatSessionSearchCandidate {
                    id: "personal".to_string(),
                    updated_at: 40,
                },
                ChatSessionSearchCandidate {
                    id: "automated".to_string(),
                    updated_at: 20,
                },
            ],
        );

        assert_eq!(candidates.len(), 3);
        assert!(matches!(
            &candidates[0],
            HistorySearchCandidate::Session(candidate) if candidate.id == "personal"
        ));
        assert!(matches!(
            &candidates[1],
            HistorySearchCandidate::Thread(candidate) if candidate.id == "screens"
        ));
        assert!(matches!(
            &candidates[2],
            HistorySearchCandidate::Session(candidate) if candidate.id == "automated"
        ));
    }

    #[test]
    fn mixed_history_search_normalizes_legacy_lane_tags() {
        let thread_item = thread_search_item(thread("screens", "Screens", HistoryLane::Legacy, 2));
        let session_item = session_search_item(session("legacy", "Wtf", HistoryLane::Legacy, 1));

        assert!(matches!(
            thread_item,
            HistorySearchItem::Thread {
                history_lane: HistoryLane::Automated,
                ..
            }
        ));
        assert!(matches!(
            session_item,
            HistorySearchItem::Session {
                history_lane: HistoryLane::Personal,
                ..
            }
        ));
    }
}
